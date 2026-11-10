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
//!   task, the per-connection HVN drain table (whose claim index doubles
//!   as the link identity), the per-link outbound queues and the fan-out
//!   that places each outbound packet on the link of the peer the core
//!   addressed it to — or, for a broadcast, on every live link (#376) —
//!   and the fragment pump that walks [`leviculum_ble_tx::PacketTx`]
//!   over a GATT notify handle. The node-facing channels and the
//!   `Interface` implementation live above the seam since 999
//!   (`leviculum_firmware::ble_link`), the node's link table since 1003;
//!   this backend reports through
//!   [`leviculum_firmware::seams::BleLinkEvent`] and implements
//!   [`leviculum_firmware::seams::BleLink`] ([`SoftDeviceLink`]) over its
//!   slot map.
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
//! - One task per link: each of the `PERIPH_LINKS` peripheral tasks'
//!   `peripheral::advertise_connectable` produces a Connection (the
//!   tasks serialize on one advertising lock — the SoftDevice runs a
//!   single advertising set — so exactly one free task advertises at a
//!   time and a connect hands the lock to the next free one, #372),
//!   then `gatt_server::run(&conn, &server, |evt| { ... })` drives a
//!   callback closure for incoming writes. Outgoing notifications use
//!   `gatt_server::notify_value(conn, handle, &data)` sync, one fragment
//!   at a time, flow-controlled against the SoftDevice's per-connection
//!   HVN queue (see [`notify`]). The central task (phase B) holds the
//!   same shape with the GATT roles mirrored. Concurrent inbound +
//!   outbound is via embassy_futures::select inside the connection
//!   lifetime; each task carries at most one connection, which is what
//!   holds `conn_count` = `CONN_COUNT` structurally.
//! - SoftDevice owns RADIO/TIMER0/RTC0/etc.; we don't bind those.
//!   USB VBUS detect goes via `SoftwareVbusDetect` fed by SoC events.

extern crate alloc;

pub mod columba;
pub mod err_text;
pub mod notify;

use alloc::vec::Vec;
use core::mem;
use core::ptr;
use embassy_executor::Spawner;
use embassy_nrf::peripherals;
use embassy_nrf::usb::vbus_detect::SoftwareVbusDetect;
use embassy_nrf::{bind_interrupts, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use leviculum_ble_tx::{Enqueue, Fill, DEVICE_NAME_LEN};
use leviculum_core::node_name::NodeName;
use nrf_softdevice::{raw, SocEvent, Softdevice};

pub use notify::{BLE_TX_DRAIN_UNROUTED, BLE_TX_DRAIN_WAITS, BLE_TX_DROPPED, BLE_TX_PACKETS};

// USBD only. SoftDevice owns the rest of the IRQs we used to bind.
bind_interrupts!(pub struct Irqs {
    USBD => embassy_nrf::usb::InterruptHandler<peripherals::USBD>;
});

/// Incoming (peripheral-role) BLE links accepted concurrently
/// (Codeberg #372).
///
/// Three, on both bins: with one slot, two boards beside a phone pair
/// with each other first and the phone can only reach the board whose
/// single slot is still free — the 2026-09-07/08 field failure. The RAM
/// bill is paid in `memory.x` (both bins share it and the SoftDevice
/// cost is board-independent), and the measured stack floor leaves
/// ~74 KiB of headroom after it, so nothing forces a smaller Pocket
/// value. One advertising set serves all slots: the peripheral tasks
/// serialize on [`columba`]'s advertising lock, so a free slot keeps
/// the board connectable while the busy ones run their sessions.
pub(crate) const PERIPH_LINKS: usize = 3;

/// Outgoing (central-role) links this node initiates. Stays at one
/// (#372 step 4): a board dials at most one neighbour at a time, the
/// central task's sequential scan→connect→session loop holds that
/// structurally, and no field topology has yet needed more — raising it
/// is a budget question (`memory.x`) before it is a code change.
const CENTRAL_LINKS: usize = 1;

/// Concurrent BLE connections the SoftDevice is configured for:
/// every incoming slot plus the initiated one. The boot-time
/// `SD_RAM_FLOOR` check judges this configuration against the linked
/// ceiling on every boot.
const CONN_COUNT: u8 = (PERIPH_LINKS + CENTRAL_LINKS) as u8;

// Slots in the per-connection HVN drain table ([`HVN_DRAIN`]). A term of
// the boot heap budget, so it is defined with that arithmetic (#388).
pub use leviculum_heap_budget::MAX_LINKS;

// A table smaller than the SoftDevice's connection count would refuse a
// legitimate connection's claim — quietly, on a board.
const _: () = assert!(MAX_LINKS >= CONN_COUNT as usize);

// The node side of the BLE link lives in `leviculum_firmware::ble_link`
// since 998 (step 4's first slice): the carrier gate, the pacing value,
// the drain/link table and the held-bytes census this backend publishes
// into — and, since 999 (the second slice), the node-facing channels,
// the peer events and inbound packets as `seams::BleLinkEvent`
// (`deliver`) and the `Interface` implementation, and since 1003 the
// node's link table, which those peer events feed. Re-imported
// here so the backend's own modules keep their `super::` paths; the
// orchestration and the binaries name `ble_link` directly.
pub use leviculum_firmware::ble_link::HVN_DRAIN;
pub(crate) use leviculum_firmware::ble_link::{
    carrier, carrier_off, carrier_on, defrag_held_set, deliver, session_held_moved,
    session_held_reset, session_held_sub, tx_gap_ms, CarrierWaiter,
};
pub(crate) use leviculum_firmware::seams::BleLinkEvent;

// The queue depth, the MTU and the two worst cases built from them are terms
// of the boot heap budget (#388), defined with that arithmetic.
use leviculum_heap_budget::QUEUE_DEPTH;
pub use leviculum_heap_budget::{CHANNEL_BUDGET_BYTES, INTERFACE_MTU, SESSION_BUDGET_BYTES};

/// One packet queue, as the per-link queues use it (the node-facing
/// channels are `leviculum_firmware::ble_link`'s since 999).
type PacketQueue = Channel<CriticalSectionRawMutex, Vec<u8>, QUEUE_DEPTH>;

// The runtime half of the media profile's BLE switch — the carrier gate
// each protocol task holds, `leviculum_firmware::ble_link` since 998 —
// is re-imported above; the boot half is [`init`]'s `columba_enabled`,
// which decides whether the protocol tasks exist at all. The media
// profile pushes its verdict with `ble_link::set_active`, and the woken
// task re-reads `carrier()` — a live session disconnects its link
// (through the same teardown a peer walking out of range takes, so the
// `Lost` report and the cull are identical), and the advertise/scan
// futures are dropped and not re-entered until the carrier reads on
// again. The waiter layout bound lives with [`CONN_COUNT`]'s assert:
// `CarrierWaiter::Peripheral(i)` must stay below the central scanner's
// latch, which `MAX_LINKS >= CONN_COUNT` guarantees.

/// Per-link outbound queues, indexed by the link's [`HVN_DRAIN`] slot.
///
/// A Reticulum interface is a broadcast domain: one BROADCAST `try_send`
/// from the node core must reach **every** peer on the medium, exactly
/// as one LoRa transmission reaches every listener. With two live links,
/// two connection tasks receiving from the single outbound channel
/// (`leviculum_firmware::ble_link`'s, since 999) would round-robin it
/// instead — each announce reaching one peer and not the other — so
/// [`tx_fanout_task`] is its only consumer (`ble_link::run_fanout`).
/// Which links are live is read from
/// [`HVN_DRAIN`]'s claims: a connection claims its slot for its whole
/// lifetime, so the drain table is also the link table, and a second
/// registry that could disagree with it is never built.
///
/// A ROUTED packet is a different question, and since #376 the core
/// answers it: the packet carries the peer it is for, and the fan-out
/// queues it on that peer's link alone. The broadcast domain is
/// unchanged — an announce still reaches every link — but a report
/// addressed to the phone no longer also travels to the board beside
/// it, which used to forward it back to the phone.
static LINK_OUT: [PacketQueue; MAX_LINKS] = [const { Channel::new() }; MAX_LINKS];

/// This link's private outbound queue (see [`LINK_OUT`]).
///
/// Handed to a connection task along with its drain-slot index. Stale
/// packets from a previous tenancy of the slot are the caller's to
/// drain at claim time, exactly as the node-facing outbound channel was
/// drained per connection before phase B.
pub(crate) fn link_out(slot_index: usize) -> &'static PacketQueue {
    &LINK_OUT[slot_index]
}

/// The S140 backend's side of [`leviculum_firmware::seams::BleLink`]:
/// what the orchestration asks of the links below it.
pub struct SoftDeviceLink;

impl leviculum_firmware::seams::BleLink for SoftDeviceLink {
    /// Deliver one outbound packet: to the link of the peer the core
    /// addressed it to, or — with no addressee — to every live link (see
    /// `LINK_OUT`).
    ///
    /// The hint is the core's `via_peer` (Codeberg #376), the same 16
    /// bytes this interface reports on peer-up/peer-lost, and
    /// [`leviculum_ble_tx::plan_fanout`] maps it onto a drain slot over
    /// this backend's slot map. Before it, a routed packet was copied onto
    /// every link: a telemetry report addressed to the phone also went to
    /// the other board, which forwarded it back to the phone — double
    /// airtime per report, `TRANSPORT dup=` on both boards, and relayed
    /// copies racing the direct ones (the desk timeline on #376). A
    /// broadcast still reaches every claimed slot, a connection that has
    /// not presented its identity yet included, which is what a Reticulum
    /// interface owes it.
    ///
    /// `try_send`, never `send`: a link whose queue is full — a peer that
    /// stopped draining — costs that link the packet and is told so in the
    /// log, but must not stall delivery to the healthy links or wedge the
    /// fan-out. With no live link at all a flooded packet is dropped
    /// silently; that is today's behaviour for an unconnected board, just
    /// moved from the connect-time drain to the moment of sending. A ROUTED
    /// packet whose peer has no live link is dropped loudly
    /// (`BLE_TX_ROUTE_MISS`) rather than flooded — see
    /// [`leviculum_ble_tx::TxFanout::NoLink`] for why.
    fn send(&mut self, aim: leviculum_ble_tx::TxAim, packet: Vec<u8>) {
        // The registry plans, the drain table says which links are
        // still claimed; `leviculum_ble_tx::resolve_fanout` decides.
        match leviculum_ble_tx::resolve_fanout(&HVN_DRAIN, columba::plan_fanout(aim)) {
            // `Link` and `RouteMiss` are only reachable from `Peer`, so
            // the peer is known in both arms.
            leviculum_ble_tx::Delivery::Link { slot, handle } => {
                let hint = aim_peer(aim);
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_TX_ROUTE peer={:02x}{:02x}{:02x}{:02x} conn={} slot={} len={}",
                        hint[0],
                        hint[1],
                        hint[2],
                        hint[3],
                        handle,
                        slot,
                        packet.len()
                    ),
                );
                queue_on_link(slot, packet);
            }
            leviculum_ble_tx::Delivery::RouteMiss => log_route_miss(&aim_peer(aim), packet.len()),
            leviculum_ble_tx::Delivery::Flood(targets) => {
                let links = flood(&packet, targets);
                match aim {
                    // The #422 ingress link: every live link but the one
                    // these bytes came in on. The count is of links
                    // actually served, so a flood of 1 out of 2 links
                    // reads as such.
                    leviculum_ble_tx::TxAim::FloodExcept(excluded) => crate::log::log_fmt(
                        "[BLE ] ",
                        format_args!(
                            "BLE_TX_FLOOD links={} len={} excl_link={:02x}{:02x}{:02x}{:02x}",
                            links,
                            packet.len(),
                            excluded[0],
                            excluded[1],
                            excluded[2],
                            excluded[3]
                        ),
                    ),
                    _ => crate::log::log_fmt(
                        "[BLE ] ",
                        format_args!("BLE_TX_FLOOD links={} len={}", links, packet.len()),
                    ),
                }
            }
        }
    }

    /// The GAP device name (`set_gap_device_name`). The scan
    /// response's Complete Local Name is built from the same value at
    /// [`columba::spawn`], which [`init`] calls right after this, so the
    /// two surfaces cannot disagree.
    fn set_advertised_name(&mut self, name: NodeName) {
        set_gap_device_name(name);
    }
}

/// The fan-out task: [`leviculum_firmware::ble_link::run_fanout`] over
/// [`SoftDeviceLink`], the one consumer of the node-facing outbound
/// channel.
#[embassy_executor::task]
async fn tx_fanout_task() -> ! {
    leviculum_firmware::ble_link::run_fanout(&mut SoftDeviceLink).await
}

/// Copy one packet onto every link in `targets`, answering how many got
/// it.
fn flood(packet: &[u8], targets: leviculum_ble_tx::SlotMask) -> usize {
    let mut links = 0usize;
    for index in 0..LINK_OUT.len() {
        if !targets.contains(index) {
            continue;
        }
        links += 1;
        queue_on_link(index, packet.to_vec());
    }
    links
}

/// The peer an aim names, or zeroes for a plain flood (which names
/// none). Only used for logging and for the miss line.
fn aim_peer(aim: leviculum_ble_tx::TxAim) -> [u8; 16] {
    match aim {
        leviculum_ble_tx::TxAim::Flood => [0u8; 16],
        leviculum_ble_tx::TxAim::Peer(peer) | leviculum_ble_tx::TxAim::FloodExcept(peer) => peer,
    }
}

/// Put one packet in a link's outbound queue, reporting a full queue
/// ([`leviculum_ble_tx::link_queue_verdict`]).
fn queue_on_link(slot: usize, packet: Vec<u8>) {
    let len = packet.len();
    let bytes = packet.capacity();
    let queue = &LINK_OUT[slot];
    let fill = Fill::new(queue.len(), queue.capacity());
    let verdict = leviculum_ble_tx::link_queue_verdict(fill, leviculum_ble_tx::LinkItem::Fanout);
    if verdict != Enqueue::Queue || queue.try_send(packet).is_err() {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_TX_FANOUT_DROP slot={} len={} depth={}",
                slot, len, QUEUE_DEPTH
            ),
        );
    } else {
        // #388 census: the packet is now in this link's custody, until
        // its pump subtracts it after the last fragment (or its death).
        leviculum_firmware::ble_link::session_held_add(slot, bytes);
    }
}

/// One line per routed packet whose peer holds no live link here.
fn log_route_miss(peer: &[u8; 16], len: usize) {
    BLE_TX_ROUTE_MISSES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_TX_ROUTE_MISS peer={:02x}{:02x}{:02x}{:02x} len={}",
            peer[0], peer[1], peer[2], peer[3], len
        ),
    );
}

/// Routed packets dropped because the addressed peer held no live link
/// (Codeberg #376), for the `BLE_COUNTERS` line.
pub static BLE_TX_ROUTE_MISSES: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

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
/// What this does NOT do is bound what reaches the GATT value
/// conversions. The 2026-09-12 field panic (#387) disproved the
/// earlier claim here that the SoftDevice rejects every write past the
/// attribute's `max_len` before an event exists: a value longer than
/// the rx/tx width reached `from_gatt` on a live board. The width
/// itself is `GATT_VALUE_MAX` = this MTU − 3, the largest single ATT
/// write the grant permits (see [`columba::GattData`] and
/// `leviculum-ble-tx`'s `gatt_bytes`, where the arithmetic lives), the
/// conversion tolerates ANY event length, and the event buffer
/// (`nrf-softdevice/evt-max-size-512` in Cargo.toml) is the one bound
/// that still panics when exceeded (#354) — rechecking it is what
/// widening `GATT_VALUE_MAX` forces.
///
/// The value is the shared `ATT_MTU` constant so the MTU grant and the
/// characteristic width cannot drift apart.
const CONN_GATT: raw::ble_gatt_conn_cfg_t = raw::ble_gatt_conn_cfg_t {
    att_mtu: leviculum_ble_tx::ATT_MTU as u16,
};

const ATTR_TAB_SIZE: raw::ble_gatts_cfg_attr_tab_size_t = raw::ble_gatts_cfg_attr_tab_size_t {
    attr_tab_size: raw::BLE_GATTS_ATTR_TAB_SIZE_DEFAULT,
};

/// Role counts, fed from [`PERIPH_LINKS`] / [`CENTRAL_LINKS`] so the
/// SoftDevice grant and the task structure cannot drift: one peripheral
/// task per incoming slot (#372), one sequential central task
/// (`central_role_count: 1` was the phase-B role flip, tied to the
/// cleared `PERIPHERAL_ONLY` advertisement bit in [`columba`]).
///
/// Not a `const`: bindgen's `new_bitfield_1` is a plain `fn`.
fn role_count_cfg() -> raw::ble_gap_cfg_role_count_t {
    raw::ble_gap_cfg_role_count_t {
        adv_set_count: 1,
        periph_role_count: PERIPH_LINKS as u8,
        central_role_count: CENTRAL_LINKS as u8,
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

/// The SoftDevice's RAM ceiling: `__sretained` = `ORIGIN(RETAINED)` from
/// `memory.x`. Above it sit first the cross-boot records (`.retained`),
/// then the flip-link stack floor `ORIGIN(RAM)` — so an SD that stays
/// below this line touches neither the records nor the stack. Comparing
/// against `_stack_end` instead would be lenient by exactly the retained
/// region: an SD landing inside it would pass and silently eat the
/// post-mortems and boot breadcrumbs.
fn sd_ceiling() -> u32 {
    extern "C" {
        static __sretained: u32;
    }
    ptr::addr_of!(__sretained) as u32
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
/// the retained records or our stack.
///
/// nrf-softdevice performs its own version of this check inside
/// `Softdevice::enable`, and under flip-link that check is a whole stack
/// region too lenient: it compares the SoftDevice's requirement against
/// `get_app_ram_base()`, which is `__sdata` — the *top* of the stack
/// region, not the bottom. The layout is
///
/// ```text
///   __sretained = ORIGIN(RETAINED) <- the real ceiling; below it is
///        .retained (cross-boot records)      the SD's
///   _stack_end = ORIGIN(RAM)
///        |  stack, grows DOWN
///   __sdata = _stack_start     <- what nrf-softdevice compares against
///        .data / .bss
/// ```
///
/// so a configuration whose requirement lands anywhere inside the
/// retained region or the stack passes the crate's check, the SoftDevice
/// takes it, and the symptom is not a refusal at boot but an SD internal
/// assertion under load, once the stack happens to get that deep — or,
/// for the retained region, silently corrupted post-mortems. Raising
/// `conn_count` is exactly the change that moves the requirement, which
/// is why this guard lands in phase A, before the change that needs it.
///
/// A violation panics: the post-mortem survives the reset and
/// `scripts/lnode-panic-query.sh` reads it back, so the board says which
/// two numbers disagreed instead of dying silently later.
fn assert_sd_fits_below_retained(wanted: u32) {
    // `floor=` on the wire for continuity: the value is the same address
    // the pre-RETAINED layout logged (ORIGIN(RETAINED) took over the old
    // ORIGIN(RAM)), only what sits directly above it changed.
    let floor = sd_ceiling();
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
        "SoftDevice wants app RAM base {wanted:#010x}, its ceiling (ORIGIN(RETAINED)) is \
         {floor:#010x}: the SD would own the retained records and then the bottom of our stack. \
         Raise ORIGIN(RETAINED) and ORIGIN(RAM) in memory.x by the shortfall and shrink \
         LENGTH(RAM) by the same amount."
    );
}

/// Write this node's GAP device name into the SoftDevice's attribute
/// table: the operator's name (#235) if one is set, `LN-<hex8>` of the
/// identity hash otherwise (#255).
///
/// The name is [`init`]'s argument since 998: the binaries pass the boot
/// name their name module recorded just before `init`, and `init` hands
/// the same value to [`columba::spawn`] for the advertisement's Complete
/// Local Name, so the two BLE surfaces cannot show different names. A
/// name longer than [`DEVICE_NAME_LEN`] arrives here already shortened
/// on a codepoint boundary (`leviculum_ble_tx::gap_name`).
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
///
/// An operator-set name is written here and nowhere else, though this
/// call would take one at runtime — the SoftDevice copies the bytes, so
/// unlike the advertisement payload there is no aliasing problem. What
/// stops it is [`columba::spawn`]'s scan response, which cannot be
/// rebuilt under a live stack: updating only the attribute would leave
/// the board advertising one name and answering with another. Both wait
/// for the reset together, and the control frame's report says so (the
/// name module).
fn set_gap_device_name(name: NodeName) {
    // No write access: the name is ours to publish, a peer has no
    // business changing it. Same permission the config carries.
    let write_perm: raw::ble_gap_conn_sec_mode_t = unsafe { mem::zeroed() };
    // SAFETY: the SoftDevice is enabled (the caller just returned from
    // `Softdevice::enable`), both pointers are valid for the duration of
    // the call, and `len` is the true length of `name`.
    let ret = unsafe {
        raw::sd_ble_gap_device_name_set(&write_perm, name.as_bytes().as_ptr(), name.len() as u16)
    };
    if ret == raw::NRF_SUCCESS {
        crate::info!("BLE: gap device name set to {}", name.as_str());
    } else {
        crate::warn!("BLE: gap device name set failed err={}", ret);
    }
}

/// How often [`counters_task`] emits its line: the `[TRANSPORT]`
/// cadence (`transport_stats::PERIOD`), so the periodic counter lines
/// interleave predictably in a capture.
///
/// The counters and their task stay below the seam (999): every number
/// on the line is a backend static — this module's and [`columba`]'s —
/// plus the drain table's claim count, and the task reaches nothing
/// upward but the log. Whether they become `ble_link`-style census
/// publishes is part of the registry split, step 4's remaining slice.
const COUNTERS_PERIOD_SECS: u64 = 30;

/// Periodic `BLE_COUNTERS` line on the debug CDC — the
/// [`crate::transport_stats`]-style surface for the #264/#255 counters,
/// which until phase B existed only as atomics nothing printed. A
/// `BLE_TX_DROP` line names each abandoned packet as it happens, but
/// the *absence* of misrouting is a claim about a counter staying 0,
/// and an absence needs a heartbeat to be quotable from a log:
///
/// ```text
/// BLE_COUNTERS packets=<n> dropped=<n> waits=<n> unrouted=<n> links=<n> displaced=<n> refused=<n> route_miss=<n> oversize=<n> unidentified=<n> rx_full=<n>
/// ```
///
/// `links=` is the number of claimed drain slots — live BLE links.
/// `displaced=` counts links torn down because a newer connection of
/// the same identity took over — the peer dialled in, or our own dial
/// found a link that had stopped answering — and `refused=` counts
/// dials of OURS sent away because that identity's existing link was
/// still answering. Beside a Columba phone both climb: `refused=` for
/// the fallback dials that keep finding the phone's rotated address,
/// `displaced=` once per reconnect the phone itself initiates (#382).
/// `route_miss=` counts routed
/// packets dropped because the peer the core addressed held no live
/// link here (#376); a rising value on a healthy board means the path
/// table outlived a link and the #365 cull should have fired.
/// `oversize=` counts inbound GATT values dropped for exceeding
/// `GATT_VALUE_MAX` (#387), writes and notifications together; each is
/// also named as its own `BLE_GATT_*_OVERSIZE` line, and every count
/// here was a board panic before #387.
/// `unidentified=` counts payload writes refused because they came
/// before the peer's 16-byte identity (#548); each is also a
/// `BLE_PRE_HANDSHAKE_REFUSE` line, and its connection was closed.
/// `rx_full=` counts completed inbound packets dropped because the main
/// loop's queue was full, both roles together (leviculum#413); before it
/// the drop left no trace.
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
                "BLE_COUNTERS packets={} dropped={} waits={} unrouted={} links={} displaced={} refused={} route_miss={} oversize={} unidentified={} rx_full={}",
                BLE_TX_PACKETS.load(Ordering::Relaxed),
                BLE_TX_DROPPED.load(Ordering::Relaxed),
                BLE_TX_DRAIN_WAITS.load(Ordering::Relaxed),
                BLE_TX_DRAIN_UNROUTED.load(Ordering::Relaxed),
                HVN_DRAIN.claimed(),
                columba::BLE_LINKS_DISPLACED.load(Ordering::Relaxed),
                columba::BLE_LINKS_REFUSED.load(Ordering::Relaxed),
                BLE_TX_ROUTE_MISSES.load(Ordering::Relaxed),
                columba::BLE_GATT_OVERSIZE.load(Ordering::Relaxed),
                columba::BLE_PRE_HANDSHAKE_REFUSED.load(Ordering::Relaxed),
                columba::BLE_RX_QUEUE_FULL.load(Ordering::Relaxed),
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
///
/// `gap_name` is the boot name both BLE surfaces come up under (998):
/// [`SoftDeviceLink`]'s `set_advertised_name` (the seam's call, 1003)
/// writes it into the GAP attribute and
/// [`columba::spawn`] builds the scan response's Complete Local Name
/// from it, so neither surface can disagree with the other.
///
/// # `columba_enabled`
///
/// The media profile ([`crate::media`]) decides whether this node meshes
/// over BLE at all, and the answer is a **spawn decision here** — which is
/// the acceptance test the #255 phase-A seam was written for, and it
/// held: `false` skips `columba::spawn` and nothing else changes. No
/// advertisement, no scan, no GATT service, no connection; the protocol
/// module is not reached.
///
/// The SoftDevice is enabled either way, and deliberately so. It is not
/// only the BLE stack: `sd_flash_write` is the one legal way to write
/// internal flash once it is enabled, and both persistence store tasks
/// ride on it. A board with `ble=off` that could not persist its own
/// profile could not be put back on BLE, which is the one state this
/// feature must never be able to reach. Enabling the SoftDevice without
/// advertising costs idle current and nothing on the air.
#[allow(clippy::too_many_arguments)]
pub fn init(
    spawner: &Spawner,
    columba_enabled: bool,
    identity_hash: [u8; 16],
    gap_name: NodeName,
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
    //
    // Each step names itself in the boot record before it is taken
    // (#469, order 1056): two boots of the Pocket hung after
    // `lora-task` and before `SD_RAM_FLOOR`, and `lora-task` alone
    // could not say whether the probe, the settle or the enable held them.
    leviculum_firmware::boot_trace::phase(leviculum_firmware::boot_trace::Phase::SdProbe);
    let wanted = required_app_ram_base();
    leviculum_firmware::boot_trace::phase(leviculum_firmware::boot_trace::Phase::SdSettle);
    cortex_m::asm::delay(PROBE_SETTLE_CYCLES);
    assert_sd_fits_below_retained(wanted);

    leviculum_firmware::boot_trace::phase(leviculum_firmware::boot_trace::Phase::SdEnable);
    let sd = Softdevice::enable(&sd_config());
    leviculum_firmware::boot_trace::phase(leviculum_firmware::boot_trace::Phase::SdEnabled);
    leviculum_firmware::seams::BleLink::set_advertised_name(&mut SoftDeviceLink, gap_name);

    let sd = if columba_enabled {
        columba::spawn(spawner, sd, identity_hash, gap_name)
    } else {
        leviculum_firmware::media::log_carrier_held_down("ble");
        sd
    };
    spawner.must_spawn(softdevice_task(sd, vbus));
    leviculum_firmware::boot_trace::phase(leviculum_firmware::boot_trace::Phase::BleTask);
    // The outbound fan-out is protocol-neutral machinery, like the
    // drain table it reads: packets in, one copy per live link out. It
    // runs either way: with no protocol task there is no claimed drain
    // slot, so it has nothing to fan out to and idles on the channel.
    spawner.must_spawn(tx_fanout_task());
    spawner.must_spawn(counters_task());

    sd
}
