//! BLE for the nRF52840 + S140, split along the protocol-reuse seam.
//!
//! # The seam (#255 phase A)
//!
//! Everything the board does over Bluetooth today speaks **one** wire
//! protocol — Columba v2.2 — but almost none of the machinery under it
//! is specific to that protocol. The roadmap's next BLE carrier
//! (`ble_leviculum`: BT5 extended-advertising broadcast on Coded PHY,
//! `docs/ble5-broadcast-protocol3-spike.md`) reuses the machinery and
//! replaces the protocol, so the two are separated here rather than
//! after the fact:
//!
//! - [`columba`] — **the protocol.** The GATT service layout and its
//!   UUIDs (`37145b00-…`), the 16-byte identity handshake, the 1-byte
//!   keepalive, the advertisement contents including the v0.3.0
//!   capability record, and (since phase B) the scanner, the MAC-sorting
//!   connection rule with its v0.3.0 override, and the GATT-client
//!   central path that mirrors the peripheral one.
//! - this module and [`notify`] — **protocol-neutral.** SoftDevice
//!   bring-up and its RAM-floor guard, the `Irqs` binding, the SoC-event
//!   task, the packet channels and the [`Interface`] implementation, the
//!   per-connection HVN drain table (whose claim index doubles as the
//!   link identity), the per-link outbound queues and the fan-out that
//!   copies each outbound packet to every live link, and the fragment
//!   pump that walks [`leviculum_ble_tx::PacketTx`] over a GATT notify
//!   handle.
//!
//! The acceptance test for the split is that a sibling `ble_leviculum`
//! could be added without touching [`columba`]. What such a sibling
//! would still have to reach across the seam for is recorded honestly:
//!
//! 1. [`init`] spawns the Columba tasks by name (via `columba::spawn`,
//!    which since phase B spawns both the peripheral and the central
//!    half behind the one entry point). A second carrier means a second
//!    spawn here — a one-line edit in the neutral module, not a change
//!    to the protocol one.
//! 2. `on_notify_tx_complete` is a method on the `Server` trait, so it
//!    is implemented on whatever concrete GATT server the protocol
//!    defines. The *routing* it performs is neutral ([`HVN_DRAIN`]); the
//!    obligation to call it belongs to each protocol's server.
//! 3. `leviculum_core::framing::ble` is filed as neutral shared framing,
//!    but its fragment header and `KEEPALIVE_BYTE` are Columba wire
//!    specifics. A broadcast carrier with its own framing would not
//!    reuse it, and would not need to: nothing in this module or in
//!    [`notify`] refers to it.
//! 4. The advertisement is built with `LegacyAdvertisementBuilder`,
//!    which is legacy-PDU-only. Extended advertising is a different
//!    nrf-softdevice API and a different `adv_set_count`; the config
//!    below is where that lands.
//!
//! # Architecture notes (carried over from the trouble-host migration)
//!
//! - One task per role: the peripheral task's
//!   `peripheral::advertise_connectable` produces a Connection, then
//!   `gatt_server::run(&conn, &server, |evt| { ... })` drives a callback
//!   closure for incoming writes. Outgoing notifications use
//!   `gatt_server::notify_value(conn, handle, &data)` sync, one fragment
//!   at a time, flow-controlled against the SoftDevice's per-connection
//!   HVN queue (see [`notify`]). The central task (phase B) holds the
//!   same shape with the GATT roles mirrored. Concurrent inbound +
//!   outbound is via embassy_futures::select inside the connection
//!   lifetime; each task carries at most one connection, which is what
//!   holds `conn_count = 2` structurally.
//! - SoftDevice owns RADIO/TIMER0/RTC0/etc.; we don't bind those.
//!   USB VBUS detect goes via `SoftwareVbusDetect` fed by SoC events.
//!
//! [`Interface`]: leviculum_core::traits::Interface

extern crate alloc;

pub mod columba;
pub mod notify;

use alloc::vec::Vec;
use core::mem;
use core::ptr;
use embassy_executor::Spawner;
use embassy_nrf::peripherals;
use embassy_nrf::usb::vbus_detect::SoftwareVbusDetect;
use embassy_nrf::{bind_interrupts, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use leviculum_ble_tx::{device_name, DrainRouter, DEVICE_NAME_LEN};
use leviculum_core::traits::{Interface, InterfaceError};
use leviculum_core::InterfaceId;
use nrf_softdevice::{raw, SocEvent, Softdevice};

pub use notify::{BLE_TX_DRAIN_UNROUTED, BLE_TX_DRAIN_WAITS, BLE_TX_DROPPED, BLE_TX_PACKETS};

// USBD only. SoftDevice owns the rest of the IRQs we used to bind.
bind_interrupts!(pub struct Irqs {
    USBD => embassy_nrf::usb::InterruptHandler<peripherals::USBD>;
});

/// Concurrent BLE connections the SoftDevice is configured for.
///
/// Two (#255 phase B, Ausbaustufe 1): the phone on the peripheral slot
/// plus ONE neighbour LNode we initiate to on the central slot. The
/// design headroom recorded on the issue is 4. The RAM consequence was
/// paid in phase A — see the `memory.x` header, and the A3
/// `SD_RAM_FLOOR` boot check judges this configuration against the
/// linked floor on every boot.
const CONN_COUNT: u8 = 2;

/// Slots in the per-connection HVN drain table ([`HVN_DRAIN`]).
///
/// Sized to the design headroom rather than to today's [`CONN_COUNT`]:
/// the table is a handful of bytes per slot, and sizing it once means
/// raising `conn_count` is a SoftDevice-config change and nothing else.
pub const MAX_LINKS: usize = 4;

// A table smaller than the SoftDevice's connection count would refuse a
// legitimate connection's claim — quietly, on a board.
const _: () = assert!(MAX_LINKS >= CONN_COUNT as usize);

/// Where `BLE_GATTS_EVT_HVN_TX_COMPLETE` goes.
///
/// The HVN queue is per connection, so its drain edge is too. Routing it
/// by connection handle is what keeps link A's drain out of link B's
/// fragment pump; see [`leviculum_ble_tx::drain`] for the failure mode
/// and its host tests.
pub static HVN_DRAIN: DrainRouter<MAX_LINKS> = DrainRouter::new();

/// The channel depth every BLE packet queue uses.
const QUEUE_DEPTH: usize = 4;

/// One packet queue, as both the node-facing channels and the per-link
/// queues use it.
type PacketQueue = Channel<CriticalSectionRawMutex, Vec<u8>, QUEUE_DEPTH>;

// Channels between the BLE tasks and the binaries' main loop.
static BLE_INCOMING: PacketQueue = Channel::new();
static BLE_OUTGOING: PacketQueue = Channel::new();

/// Per-link outbound queues, indexed by the link's [`HVN_DRAIN`] slot.
///
/// A Reticulum interface is a broadcast domain: one `try_send` from the
/// node core must reach **every** peer on the medium, exactly as one
/// LoRa transmission reaches every listener. With two live links, two
/// connection tasks receiving from the single [`BLE_OUTGOING`] channel
/// would round-robin it instead — each announce reaching one peer and
/// not the other — so [`tx_fanout_task`] is the only consumer of
/// [`BLE_OUTGOING`], and it copies each packet into the queue of every
/// live link. Which links are live is read from [`HVN_DRAIN`]'s claims:
/// a connection claims its slot for its whole lifetime, so the drain
/// table is also the link table, and a second registry that could
/// disagree with it is never built.
static LINK_OUT: [PacketQueue; MAX_LINKS] = [const { Channel::new() }; MAX_LINKS];

/// This link's private outbound queue (see [`LINK_OUT`]).
///
/// Handed to a connection task along with its drain-slot index. Stale
/// packets from a previous tenancy of the slot are the caller's to
/// drain at claim time, exactly as [`BLE_OUTGOING`] was drained per
/// connection before phase B.
pub(crate) fn link_out(slot_index: usize) -> &'static PacketQueue {
    &LINK_OUT[slot_index]
}

/// Fan one outbound packet out to every live link (see [`LINK_OUT`]).
///
/// `try_send`, never `send`: a link whose queue is full — a peer that
/// stopped draining — costs that link the packet and is told so in the
/// log, but must not stall delivery to the healthy links or wedge the
/// fan-out. With no live link at all the packet is dropped silently;
/// that is today's behaviour for an unconnected board, just moved from
/// the connect-time drain to the moment of sending.
#[embassy_executor::task]
async fn tx_fanout_task() -> ! {
    loop {
        let packet = BLE_OUTGOING.receive().await;
        for (index, queue) in LINK_OUT.iter().enumerate() {
            if HVN_DRAIN.handle_at(index).is_none() {
                continue;
            }
            if queue.try_send(packet.clone()).is_err() {
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_TX_FANOUT_DROP slot={} len={} depth={}",
                        index,
                        packet.len(),
                        QUEUE_DEPTH
                    ),
                );
            }
        }
    }
}

pub struct BleChannels {
    pub incoming_rx: Receiver<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
    pub outgoing_tx: Sender<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
}

pub fn channels() -> BleChannels {
    BleChannels {
        incoming_rx: BLE_INCOMING.receiver(),
        outgoing_tx: BLE_OUTGOING.sender(),
    }
}

pub struct BleInterface {
    sender: Sender<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
}

impl BleInterface {
    pub fn new(sender: Sender<'static, CriticalSectionRawMutex, Vec<u8>, 4>) -> Self {
        Self { sender }
    }
}

impl Interface for BleInterface {
    fn id(&self) -> InterfaceId {
        InterfaceId(2)
    }
    fn name(&self) -> &str {
        "ble"
    }
    fn mtu(&self) -> usize {
        564
    }
    fn is_online(&self) -> bool {
        true
    }
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        self.sender.try_send(data.to_vec()).map_err(|_| {
            // Codeberg #344: same silence as the other two. A phone that
            // stops draining the notify path fills this queue, and the board
            // could not tell that from a mesh with nothing to say.
            crate::log::log_fmt(
                "[IFACE_FULL] ",
                format_args!(
                    "iface={} depth={} len={}",
                    self.name(),
                    self.sender.capacity(),
                    data.len()
                ),
            );
            InterfaceError::BufferFull
        })
    }
}

#[embassy_executor::task]
async fn softdevice_task(sd: &'static Softdevice, vbus: &'static SoftwareVbusDetect) -> ! {
    sd.run_with_callback(|evt| match evt {
        SocEvent::PowerUsbDetected => vbus.detected(true),
        SocEvent::PowerUsbRemoved => vbus.detected(false),
        SocEvent::PowerUsbPowerReady => vbus.ready(),
        _ => {}
    })
    .await
}

/// The GAP connection configuration, as one value so [`sd_config`] and
/// [`required_app_ram_base`] cannot drift apart.
const CONN_GAP: raw::ble_gap_conn_cfg_t = raw::ble_gap_conn_cfg_t {
    conn_count: CONN_COUNT,
    event_length: 24,
};

/// A ceiling, not an answer. `ble_gatt_conn_cfg_t::att_mtu` is the
/// "maximum size of ATT packet the SoftDevice can send or receive" for
/// connections opened on this conn_cfg tag; the S140 does not answer an
/// Exchange MTU Request on its own, it raises
/// BLE_GATTS_EVT_EXCHANGE_MTU_REQUEST and waits for
/// `sd_ble_gatts_exchange_mtu_reply`, whose server_rx_mtu "maximum
/// value is ble_gatt_conn_cfg_t::att_mtu in the connection
/// configuration used for this connection". nrf-softdevice makes that
/// reply for us with min(peer's request, this value)
/// (nrf-softdevice/src/ble/gatt_server.rs:447-463), and the connection
/// is opened on the tag this value was set under: the crate sets every
/// conn_cfg under APP_CONN_CFG_TAG = 1 (softdevice.rs:68) and starts
/// advertising with the same tag (peripheral.rs:277).
///
/// What this does NOT do is keep events inside the
/// `nrf-softdevice/evt-max-size-512` buffer selected in Cargo.toml.
/// That bound is the 251-byte width of rx/tx (see
/// [`columba::ReticulumService`]): both values live in the SoftDevice's
/// attribute table with max_len 251 and no write authorization, so a
/// longer write is rejected by the SoftDevice with an ATT error and
/// never becomes an event. Widening 251 is what forces a recheck of
/// `evt-max-size-*`; moving this number does not.
const CONN_GATT: raw::ble_gatt_conn_cfg_t = raw::ble_gatt_conn_cfg_t { att_mtu: 256 };

const ATTR_TAB_SIZE: raw::ble_gatts_cfg_attr_tab_size_t = raw::ble_gatts_cfg_attr_tab_size_t {
    attr_tab_size: raw::BLE_GATTS_ATTR_TAB_SIZE_DEFAULT,
};

/// Role counts. `central_role_count: 1` is the phase-B role flip,
/// raised together with [`CONN_COUNT`] and with clearing the
/// `PERIPHERAL_ONLY` advertisement bit in [`columba`] — three spellings
/// of the one fact "this node can initiate one connection", changed in
/// the same commit so they cannot drift.
///
/// Not a `const`: bindgen's `new_bitfield_1` is a plain `fn`.
fn role_count_cfg() -> raw::ble_gap_cfg_role_count_t {
    raw::ble_gap_cfg_role_count_t {
        adv_set_count: 1,
        periph_role_count: 1,
        central_role_count: 1,
        central_sec_count: 0,
        _bitfield_1: raw::ble_gap_cfg_role_count_t::new_bitfield_1(0),
    }
}

/// The GAP device name a connected peer reads. Our name is
/// runtime-derived (`LN-<hex8>` of this node's identity hash), and the
/// SoftDevice's contract for this struct is explicit
/// (`nrf-softdevice-s140` bindings, `ble_gap_cfg_device_name_t`):
///
///   "If vloc is BLE_GATTS_VLOC_STACK:
///     - p_value must point to non-volatile memory (flash) or be NULL.
///     - If p_value is NULL, the device name will initially be empty."
///
/// So a pointer into RAM is not an option here, no matter how 'static
/// that RAM is — handing `sd_ble_cfg_set` one earns
/// NRF_ERROR_INVALID_ADDR, which nrf-softdevice turns into an outright
/// panic (`softdevice.rs`, `cfg_set`). That panic sits inside
/// `Softdevice::enable` below, i.e. inside `main` before its first
/// await, so the USB task never gets polled: the board dies
/// pre-enumeration and boot-loops (fixed in `e4d7ef7`; regression
/// `e52dba1`).
///
/// NULL + `max_len` is the reservation: the name lives in the
/// SoftDevice's own attribute table, empty at enable, and
/// [`set_gap_device_name`] writes it. 11 <= BLE_GAP_DEVNAME_DEFAULT_LEN
/// (31), so [`ATTR_TAB_SIZE`] needs no bump.
fn gap_device_name_cfg() -> raw::ble_gap_cfg_device_name_t {
    raw::ble_gap_cfg_device_name_t {
        p_value: ptr::null_mut(),
        current_len: 0,
        max_len: DEVICE_NAME_LEN as u16,
        write_perm: unsafe { mem::zeroed() },
        _bitfield_1: raw::ble_gap_cfg_device_name_t::new_bitfield_1(
            raw::BLE_GATTS_VLOC_STACK as u8,
        ),
    }
}

/// The LF clock configuration. Synthesized LF from the HF crystal;
/// matches Heltec/RAK/Adafruit factory bootloaders' expectation. RAK4631
/// and T114 have no dedicated 32.768 kHz LF crystal.
const CLOCK_CFG: raw::nrf_clock_lf_cfg_t = raw::nrf_clock_lf_cfg_t {
    source: raw::NRF_CLOCK_LF_SRC_RC as u8,
    rc_ctiv: 16,
    rc_temp_ctiv: 2,
    accuracy: raw::NRF_CLOCK_LF_ACCURACY_500_PPM as u8,
};

/// The one SoftDevice configuration this firmware enables with.
fn sd_config() -> nrf_softdevice::Config {
    nrf_softdevice::Config {
        clock: Some(CLOCK_CFG),
        conn_gap: Some(CONN_GAP),
        conn_gatt: Some(CONN_GATT),
        gatts_attr_tab_size: Some(ATTR_TAB_SIZE),
        gap_role_count: Some(role_count_cfg()),
        gap_device_name: Some(gap_device_name_cfg()),
        ..Default::default()
    }
}

/// The flip-link stack floor: `ORIGIN(RAM)` from `memory.x`, the lowest
/// address our stack can reach before it is in the SoftDevice's RAM.
fn stack_floor() -> u32 {
    extern "C" {
        static _stack_end: u32;
    }
    ptr::addr_of!(_stack_end) as u32
}

/// The SoftDevice's own assertion callback, used only for the RAM probe
/// below. The probe never starts BLE activity, so reaching this means
/// the enable/disable cycle itself faulted — worth a loud panic, which
/// the lib panic handler persists as a post-mortem.
unsafe extern "C" fn probe_fault_handler(id: u32, pc: u32, info: u32) {
    panic!("SD fault during RAM probe id={id} pc={pc:#x} info={info:#x}");
}

/// Ask the S140 what application RAM base [`sd_config`] requires,
/// without letting it initialise the BLE stack.
///
/// `sd_ble_enable` against a deliberately undersized base answers
/// `NRF_ERROR_NO_MEM` and writes the exact required base into the in-out
/// parameter, and on that path it never begins initialisation — the
/// measurement is free of side effects. The SoftDevice is disabled again
/// before returning, so `Softdevice::enable` afterwards sees a clean
/// slate. `src/bin/sd-ram-probe.rs` runs this same cycle seven times in
/// one boot; this is one instance of it, on the real config.
///
/// Returns 0 if any call failed for a reason other than the expected
/// `NO_MEM` — an inconclusive probe must not be read as "it fits".
fn required_app_ram_base() -> u32 {
    /// Below the S140's own 8 KiB MBR reservation floor, so the answer is
    /// always NO_MEM and never a success write-back.
    const UNDERSIZED_BASE: u32 = 0x2000_2000;

    // SAFETY: nothing has enabled the SoftDevice yet (this runs at the
    // top of `init`, which each binary calls once), and every pointer
    // handed over lives for the duration of its call.
    let ret = unsafe { raw::sd_softdevice_enable(&CLOCK_CFG, Some(probe_fault_handler)) };
    if ret != raw::NRF_SUCCESS {
        crate::warn!("BLE: RAM probe could not enable the SD, err={}", ret);
        return 0;
    }

    let conn_gap = raw::ble_cfg_t {
        conn_cfg: raw::ble_conn_cfg_t {
            conn_cfg_tag: APP_CONN_CFG_TAG,
            params: raw::ble_conn_cfg_t__bindgen_ty_1 {
                gap_conn_cfg: CONN_GAP,
            },
        },
    };
    let conn_gatt = raw::ble_cfg_t {
        conn_cfg: raw::ble_conn_cfg_t {
            conn_cfg_tag: APP_CONN_CFG_TAG,
            params: raw::ble_conn_cfg_t__bindgen_ty_1 {
                gatt_conn_cfg: CONN_GATT,
            },
        },
    };
    let role_count = raw::ble_cfg_t {
        gap_cfg: raw::ble_gap_cfg_t {
            role_count_cfg: role_count_cfg(),
        },
    };
    let device_name = raw::ble_cfg_t {
        gap_cfg: raw::ble_gap_cfg_t {
            device_name_cfg: gap_device_name_cfg(),
        },
    };
    let attr_tab = raw::ble_cfg_t {
        gatts_cfg: raw::ble_gatts_cfg_t {
            attr_tab_size: ATTR_TAB_SIZE,
        },
    };

    // Same five configs `Softdevice::enable` sets for `sd_config()`, in
    // the same order and under the same tag. `sd_ble_cfg_set` may itself
    // answer NO_MEM against the undersized base; like nrf-softdevice we
    // let `sd_ble_enable` deliver the verdict and only bail on errors
    // that mean the config never registered at all.
    for (id, cfg) in [
        (raw::BLE_CONN_CFGS_BLE_CONN_CFG_GAP, &conn_gap),
        (raw::BLE_CONN_CFGS_BLE_CONN_CFG_GATT, &conn_gatt),
        (raw::BLE_GAP_CFGS_BLE_GAP_CFG_ROLE_COUNT, &role_count),
        (raw::BLE_GAP_CFGS_BLE_GAP_CFG_DEVICE_NAME, &device_name),
        (raw::BLE_GATTS_CFGS_BLE_GATTS_CFG_ATTR_TAB_SIZE, &attr_tab),
    ] {
        // SAFETY: the SoftDevice is enabled and `cfg` outlives the call.
        let ret = unsafe { raw::sd_ble_cfg_set(id, cfg, UNDERSIZED_BASE) };
        if ret != raw::NRF_SUCCESS && ret != raw::NRF_ERROR_NO_MEM {
            crate::warn!("BLE: RAM probe cfg_set id={} err={}", id, ret);
            // SAFETY: enabled just above.
            let _ = unsafe { raw::sd_softdevice_disable() };
            return 0;
        }
    }

    let mut wanted: u32 = UNDERSIZED_BASE;
    // SAFETY: enabled, `wanted` is a live in-out parameter.
    let ret = unsafe { raw::sd_ble_enable(&mut wanted) };
    // SAFETY: enabled; the BLE stack was never initialised on the NO_MEM
    // path, so this is a plain teardown of the SoC-level enable.
    let _ = unsafe { raw::sd_softdevice_disable() };

    if ret != raw::NRF_ERROR_NO_MEM {
        crate::warn!("BLE: RAM probe expected NO_MEM, got err={}", ret);
        return 0;
    }
    wanted
}

/// `APP_CONN_CFG_TAG` — the tag nrf-softdevice opens every connection on
/// (`softdevice.rs:68`) and advertises with (`peripheral.rs:277`).
const APP_CONN_CFG_TAG: u8 = 1;

/// Cycles of blocking delay after the probe's `sd_softdevice_disable`,
/// before the real enable. 64 MHz core, so ~100 ms — the same settling
/// time `src/bin/sd-ram-probe.rs` leaves between its seven cycles, which
/// is the only enable/disable/enable sequence measured on a board.
const PROBE_SETTLE_CYCLES: u32 = 6_400_000;

/// Refuse to enable the SoftDevice if it would take RAM out from under
/// our stack.
///
/// nrf-softdevice performs its own version of this check inside
/// `Softdevice::enable`, and under flip-link that check is a whole stack
/// region too lenient: it compares the SoftDevice's requirement against
/// `get_app_ram_base()`, which is `__sdata` — the *top* of the stack
/// region, not the bottom. The layout is
///
/// ```text
///   _stack_end = ORIGIN(RAM)   <- the real floor; below it is the SD's
///        |  stack, grows DOWN
///   __sdata = _stack_start     <- what nrf-softdevice compares against
///        .data / .bss / .uninit
/// ```
///
/// so a configuration whose requirement lands anywhere inside the stack
/// region passes the crate's check, the SoftDevice takes the bottom of
/// our stack, and the symptom is not a refusal at boot but an SD
/// internal assertion under load, once the stack happens to get that
/// deep. Raising `conn_count` is exactly the change that moves the
/// requirement, which is why this guard lands in phase A, before the
/// change that needs it.
///
/// A violation panics: the post-mortem survives the reset and
/// `scripts/lnode-panic-query.sh` reads it back, so the board says which
/// two numbers disagreed instead of dying silently later.
fn assert_sd_fits_below_the_stack(wanted: u32) {
    let floor = stack_floor();
    if wanted == 0 {
        // Inconclusive, not "fits". Loud, but not fatal: refusing to
        // boot over a probe that could not run would trade a possible
        // problem for a certain one.
        crate::warn!("BLE: SD RAM probe inconclusive, floor={} unchecked", floor);
        return;
    }
    crate::log::log_fmt_critical(
        "[BLE ] ",
        format_args!(
            "SD_RAM_FLOOR wanted=0x{:08x} floor=0x{:08x} margin={} fits={}",
            wanted,
            floor,
            i64::from(floor) - i64::from(wanted),
            u8::from(wanted <= floor),
        ),
    );
    assert!(
        wanted <= floor,
        "SoftDevice wants app RAM base {wanted:#010x}, stack floor (ORIGIN(RAM)) is {floor:#010x}: \
         the SD would own the bottom of our stack. Raise ORIGIN(RAM) in memory.x to {wanted:#010x} \
         or above and shrink LENGTH by the same amount."
    );
}

/// Write this node's individual GAP device name, `LN-<hex8>` of the
/// identity hash (#255), into the SoftDevice's attribute table.
///
/// This is the runtime half of [`gap_device_name_cfg`]: the config
/// reserves `DEVICE_NAME_LEN` bytes with a NULL `p_value` — the only
/// pointer `BLE_GATTS_VLOC_STACK` accepts for a name that is not a flash
/// literal — leaving the name empty, and `sd_ble_gap_device_name_set`
/// fills it in. The SoftDevice copies the bytes out of `name`, so a
/// stack local is a valid source; nothing has to stay alive afterwards.
///
/// Deliberately non-fatal. The device name is cosmetic — it decides what
/// a scanner lists, nothing about whether packets move — while this call
/// sits on the boot path ahead of USB enumeration, where a panic costs
/// the whole node and takes the log with it. `e52dba1` is exactly that
/// failure, and the lesson is not only "hand it the right pointer" but
/// "never let the name be able to stop the boot".
fn set_gap_device_name(identity_hash: &[u8; 16]) {
    let name = device_name(identity_hash);
    // No write access: the name is derived from the identity, a peer has
    // no business changing it. Same permission the config carries.
    let write_perm: raw::ble_gap_conn_sec_mode_t = unsafe { mem::zeroed() };
    // SAFETY: the SoftDevice is enabled (the caller just returned from
    // `Softdevice::enable`), both pointers are valid for the duration of
    // the call, and `len` is the true length of `name`.
    let ret = unsafe {
        raw::sd_ble_gap_device_name_set(&write_perm, name.as_ptr(), DEVICE_NAME_LEN as u16)
    };
    if ret == raw::NRF_SUCCESS {
        crate::info!(
            "BLE: gap device name set to {}",
            core::str::from_utf8(&name).unwrap_or("<non-utf8>")
        );
    } else {
        crate::warn!("BLE: gap device name set failed err={}", ret);
    }
}

/// How often [`counters_task`] emits its line: the `[TRANSPORT]`
/// cadence (`transport_stats::PERIOD`), so the periodic counter lines
/// interleave predictably in a capture.
const COUNTERS_PERIOD_SECS: u64 = 30;

/// Periodic `BLE_COUNTERS` line on the debug CDC — the
/// [`crate::transport_stats`]-style surface for the #264/#255 counters,
/// which until phase B existed only as atomics nothing printed. A
/// `BLE_TX_DROP` line names each abandoned packet as it happens, but
/// the *absence* of misrouting is a claim about a counter staying 0,
/// and an absence needs a heartbeat to be quotable from a log:
///
/// ```text
/// BLE_COUNTERS packets=<n> dropped=<n> waits=<n> unrouted=<n> links=<n>
/// ```
///
/// `links=` is the number of claimed drain slots — live BLE links.
/// The two-link acceptance for #255 phase B reads `links=2 unrouted=0`
/// off this line: both slots claimed, and every HVN drain edge still
/// found the link that produced it.
#[embassy_executor::task]
async fn counters_task() -> ! {
    use core::sync::atomic::Ordering;
    loop {
        embassy_time::Timer::after_secs(COUNTERS_PERIOD_SECS).await;
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_COUNTERS packets={} dropped={} waits={} unrouted={} links={}",
                BLE_TX_PACKETS.load(Ordering::Relaxed),
                BLE_TX_DROPPED.load(Ordering::Relaxed),
                BLE_TX_DRAIN_WAITS.load(Ordering::Relaxed),
                BLE_TX_DRAIN_UNROUTED.load(Ordering::Relaxed),
                HVN_DRAIN.claimed(),
            ),
        );
    }
}

/// Bring up S140 + start the BLE task. Peripherals previously owned by
/// MPSL/SDC (RTC0/TIMER0/PPI/RNG/etc.) are kept in the signature for ABI
/// compatibility with the binaries; the SoftDevice claims them
/// internally.
///
/// Returns the enabled SoftDevice, which anything needing a SoftDevice
/// syscall after this point has to hold — the flash writes in
/// [`crate::radio_store`] are the current caller.
#[allow(clippy::too_many_arguments)]
pub fn init(
    spawner: &Spawner,
    identity_hash: [u8; 16],
    vbus: &'static SoftwareVbusDetect,
    _rtc0: Peri<'static, peripherals::RTC0>,
    _timer0: Peri<'static, peripherals::TIMER0>,
    _temp: Peri<'static, peripherals::TEMP>,
    _ppi_ch19: Peri<'static, peripherals::PPI_CH19>,
    _ppi_ch30: Peri<'static, peripherals::PPI_CH30>,
    _ppi_ch31: Peri<'static, peripherals::PPI_CH31>,
    _ppi_ch17: Peri<'static, peripherals::PPI_CH17>,
    _ppi_ch18: Peri<'static, peripherals::PPI_CH18>,
    _ppi_ch20: Peri<'static, peripherals::PPI_CH20>,
    _ppi_ch21: Peri<'static, peripherals::PPI_CH21>,
    _ppi_ch22: Peri<'static, peripherals::PPI_CH22>,
    _ppi_ch23: Peri<'static, peripherals::PPI_CH23>,
    _ppi_ch24: Peri<'static, peripherals::PPI_CH24>,
    _ppi_ch25: Peri<'static, peripherals::PPI_CH25>,
    _ppi_ch26: Peri<'static, peripherals::PPI_CH26>,
    _ppi_ch27: Peri<'static, peripherals::PPI_CH27>,
    _ppi_ch28: Peri<'static, peripherals::PPI_CH28>,
    _ppi_ch29: Peri<'static, peripherals::PPI_CH29>,
    _rng_periph: Peri<'static, peripherals::RNG>,
) -> &'static Softdevice {
    // Measure first, enable second: the probe leaves the BLE stack
    // uninitialised, so a configuration that would eat our stack is
    // refused before it can.
    let wanted = required_app_ram_base();
    cortex_m::asm::delay(PROBE_SETTLE_CYCLES);
    assert_sd_fits_below_the_stack(wanted);

    let sd = Softdevice::enable(&sd_config());
    set_gap_device_name(&identity_hash);

    let sd = columba::spawn(spawner, sd, identity_hash);
    spawner.must_spawn(softdevice_task(sd, vbus));
    // The outbound fan-out is protocol-neutral machinery, like the
    // drain table it reads: packets in, one copy per live link out.
    spawner.must_spawn(tx_fanout_task());
    spawner.must_spawn(counters_task());

    sd
}
