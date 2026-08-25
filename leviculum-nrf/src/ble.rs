//! BLE Peripheral interface — Day-3 nrf-softdevice + Columba v2.2.
//!
//! GATT service rewritten on top of `nrf-softdevice::gatt_service` /
//! `gatt_server` macros. The Columba v2.2 protocol layer (defrag,
//! keepalive, identity handshake) carries over from the prior
//! trouble-host implementation as type renames over the same byte-
//! level operations — same wire format, same characteristic UUIDs,
//! same fragment header / keepalive byte semantics.
//!
//! Architecture differences from trouble-host:
//! - Single-task model: `peripheral::advertise_connectable` produces a
//!   Connection, then `gatt_server::run(&conn, &server, |evt| { ... })`
//!   drives a callback closure for incoming writes. Outgoing
//!   notifications use `gatt_server::notify_value(conn, handle, &data)`
//!   sync, one fragment at a time, flow-controlled against the
//!   SoftDevice's per-connection HVN queue (see `notify_fragments`).
//!   Concurrent inbound + outbound is via embassy_futures::select
//!   inside the connection lifetime.
//! - SoftDevice owns RADIO/TIMER0/RTC0/etc.; we don't bind those.
//!   USB VBUS detect goes via `SoftwareVbusDetect` fed by SoC events.

extern crate alloc;

use alloc::vec::Vec;
use core::cell::Cell;
use core::mem;
use core::sync::atomic::{AtomicU32, Ordering};
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_nrf::peripherals;
use embassy_nrf::usb::vbus_detect::SoftwareVbusDetect;
use embassy_nrf::{bind_interrupts, Peri};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use leviculum_ble_tx::{Action, Event, NotifyOutcome, PacketTx, DRAIN_WAIT_MS};
use leviculum_core::framing::ble::{
    self as ble_framing, BleDefragmenter, DefragResult, FRAGMENT_HEADER_SIZE, KEEPALIVE_BYTE,
    KEEPALIVE_INTERVAL_MS,
};
use leviculum_core::traits::{Interface, InterfaceError};
use leviculum_core::InterfaceId;
use nrf_softdevice::ble::advertisement_builder::{
    Flag, LegacyAdvertisementBuilder, LegacyAdvertisementPayload,
};
use nrf_softdevice::ble::gatt_server::{NotifyValueError, Server, WriteOp};
use nrf_softdevice::ble::{gatt_server, peripheral, Connection};
use nrf_softdevice::{raw, RawError, SocEvent, Softdevice};
use static_cell::StaticCell;

// USBD only. SoftDevice owns the rest of the IRQs we used to bind.
bind_interrupts!(pub struct Irqs {
    USBD => embassy_nrf::usb::InterruptHandler<peripherals::USBD>;
});

#[nrf_softdevice::gatt_service(uuid = "37145b00-442d-4a94-917f-8f42c5da28e3")]
pub struct ReticulumService {
    #[characteristic(
        uuid = "37145b00-442d-4a94-917f-8f42c5da28e5",
        write,
        write_without_response
    )]
    rx: heapless_v8::Vec<u8, 251>,

    #[characteristic(uuid = "37145b00-442d-4a94-917f-8f42c5da28e4", read, notify)]
    tx: heapless_v8::Vec<u8, 251>,

    #[characteristic(uuid = "37145b00-442d-4a94-917f-8f42c5da28e6", read)]
    identity: [u8; 16],
}

#[nrf_softdevice::gatt_server]
pub struct ReticulumServer {
    pub reticulum_service: ReticulumService,
}

/// Signalled on every `BLE_GATTS_EVT_HVN_TX_COMPLETE`: at least one slot
/// of the per-connection HVN queue is free again. The outbound fragment
/// loop waits on this instead of guessing an interval (Codeberg #264).
static HVN_TX_DRAINED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Packets whose every fragment reached the SoftDevice's notification
/// queue.
pub static BLE_TX_PACKETS: AtomicU32 = AtomicU32::new(0);
/// Packets abandoned part-way through their fragments. Before #264 this
/// was the common case and it incremented nothing at all: the loop could
/// not tell a dropped fragment from a sent one.
pub static BLE_TX_DROPPED: AtomicU32 = AtomicU32::new(0);
/// Times the outbound loop waited for the HVN queue to drain. On a
/// one-deep queue (the S140 default) this tracks "fragments beyond the
/// first", so it is the direct measure of multi-fragment traffic.
pub static BLE_TX_DRAIN_WAITS: AtomicU32 = AtomicU32::new(0);

/// [`ReticulumServer`] plus the one `Server` callback the
/// `#[gatt_server]` macro does not generate.
///
/// The macro emits `on_write` only; every other callback keeps the
/// trait's default, and the default for `on_notify_tx_complete` throws
/// the event away. That event is exactly what tells us the HVN queue has
/// room again, so the impl is written by hand here and the write path is
/// delegated unchanged.
pub struct NotifyAwareServer {
    pub inner: ReticulumServer,
}

impl Server for NotifyAwareServer {
    type Event = ReticulumServerEvent;

    fn on_write(
        &self,
        conn: &Connection,
        handle: u16,
        op: WriteOp,
        offset: usize,
        data: &[u8],
    ) -> Option<Self::Event> {
        self.inner.on_write(conn, handle, op, offset, data)
    }

    fn on_notify_tx_complete(&self, _conn: &Connection, _count: u8) -> Option<Self::Event> {
        HVN_TX_DRAINED.signal(());
        None
    }
}

// Channels between BLE task and the binaries' main loop.
static BLE_INCOMING: Channel<CriticalSectionRawMutex, Vec<u8>, 4> = Channel::new();
static BLE_OUTGOING: Channel<CriticalSectionRawMutex, Vec<u8>, 4> = Channel::new();

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

// Static advertising payload. 16-byte service UUID in little-endian.
//
// Same UUID as the GATT service: 37145b00-442d-4a94-917f-8f42c5da28e3,
// reversed to LE: e3 28 da c5 42 8f 7f 91 94 4a 2d 44 00 5b 14 37
const RETICULUM_SVC_UUID_LE: [u8; 16] = [
    0xe3, 0x28, 0xda, 0xc5, 0x42, 0x8f, 0x7f, 0x91, 0x94, 0x4a, 0x2d, 0x44, 0x00, 0x5b, 0x14, 0x37,
];

#[embassy_executor::task]
async fn ble_task(
    sd: &'static Softdevice,
    server: &'static NotifyAwareServer,
    identity_hash: [u8; 16],
) {
    // Publish the identity characteristic value so a connecting peer can
    // read it before exchanging frames over rx/tx.
    let _ = server.inner.reticulum_service.identity_set(&identity_hash);

    // Static-lifetime advertising / scan payloads — nrf-softdevice's
    // peripheral::advertise_connectable wants &'static slices.
    static ADV_DATA: StaticCell<LegacyAdvertisementPayload> = StaticCell::new();
    static SCAN_DATA: StaticCell<LegacyAdvertisementPayload> = StaticCell::new();
    let adv = ADV_DATA.init(
        LegacyAdvertisementBuilder::new()
            .flags(&[Flag::GeneralDiscovery, Flag::LE_Only])
            .services_128(
                nrf_softdevice::ble::advertisement_builder::ServiceList::Complete,
                &[RETICULUM_SVC_UUID_LE],
            )
            .build(),
    );
    let scan = SCAN_DATA.init(
        LegacyAdvertisementBuilder::new()
            .full_name("leviculum")
            .build(),
    );

    let outgoing_rx = BLE_OUTGOING.receiver();
    let incoming_tx = BLE_INCOMING.sender();

    loop {
        let config = peripheral::Config::default();
        let advertisement = peripheral::ConnectableAdvertisement::ScannableUndirected {
            adv_data: adv.as_ref(),
            scan_data: scan.as_ref(),
        };
        match peripheral::advertise_connectable(sd, advertisement, &config).await {
            Ok(conn) => {
                crate::info!("BLE: connected");
                gatt_events(&conn, server, &incoming_tx, &outgoing_rx).await;
                crate::info!("BLE: disconnected");
            }
            Err(_) => {
                Timer::after_millis(1000).await;
            }
        }
    }
}

/// Per-connection event-loop. Inbound writes drive `gatt_server::run`'s
/// closure (Columba defrag + handshake state); outbound BLE_OUTGOING and
/// keepalive timer feed `gatt_server::notify_value`. The two halves run
/// concurrently via `embassy_futures::select`.
async fn gatt_events(
    conn: &Connection,
    server: &NotifyAwareServer,
    incoming_tx: &Sender<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
    outgoing_rx: &Receiver<'static, CriticalSectionRawMutex, Vec<u8>, 4>,
) {
    // Per-connection state. `Cell` lets the closure inside
    // `gatt_server::run` mutate handshake_done while the outbound branch
    // reads it. Single-task context (the executor polls one future at a
    // time), so no Mutex is needed.
    let defrag: Cell<BleDefragmenter> = Cell::new(BleDefragmenter::new());
    let handshake_done: Cell<bool> = Cell::new(false);
    let last_keepalive: Cell<Instant> = Cell::new(Instant::now());

    // Drain stale outgoing packets from before this connection.
    while outgoing_rx.try_receive().is_ok() {}

    let tx_handle = server.inner.reticulum_service.tx_value_handle;

    let inbound = gatt_server::run(conn, server, |evt| {
        let ReticulumServerEvent::ReticulumService(service_evt) = evt;
        // tx is notify-only; CCCD writes from the peer would also land in
        // this event stream, but the macro variant naming depends on
        // whether `notify` was declared. Any variant other than RxWrite
        // is a no-op for us, hence `if let`.
        if let ReticulumServiceEvent::RxWrite(data) = service_evt {
            if !handshake_done.get() && data.len() == 16 {
                // Identity handshake — peer's first write is its 16-byte identity.
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "peer id: {:02x}{:02x}{:02x}{:02x}",
                        data[0], data[1], data[2], data[3]
                    ),
                );
                handshake_done.set(true);
                last_keepalive.set(Instant::now());
            } else if data.len() < FRAGMENT_HEADER_SIZE {
                // Single-byte keepalive (0x00); nothing to defragment.
            } else {
                let now = Instant::now().as_millis();
                let mut d = defrag.replace(BleDefragmenter::new());
                let result = d.process(&data, now);
                defrag.set(d);
                match result {
                    DefragResult::Complete(packet) => {
                        crate::info!("BLE: RX {}B", packet.len());
                        // try_send: if the consumer is slow and the 4-deep
                        // channel is full, drop the packet rather than block
                        // here (we're in a sync closure, can't await).
                        let _ = incoming_tx.try_send(packet);
                    }
                    DefragResult::NeedMore => {}
                    DefragResult::Error => {
                        let mut d = defrag.replace(BleDefragmenter::new());
                        d.reset();
                        defrag.set(d);
                    }
                }
            }
        }
    });

    let outbound = async {
        loop {
            let keepalive_deadline = if handshake_done.get() {
                Timer::at(last_keepalive.get() + Duration::from_millis(KEEPALIVE_INTERVAL_MS))
            } else {
                Timer::at(Instant::MAX)
            };

            match select(outgoing_rx.receive(), keepalive_deadline).await {
                Either::First(packet) => {
                    let fragments = ble_framing::fragment_packet(&packet, ble_framing::DEFAULT_MTU);
                    notify_fragments(
                        conn,
                        tx_handle,
                        fragments.len(),
                        |index| &fragments[index],
                        "packet",
                        packet.len(),
                    )
                    .await;
                }
                Either::Second(()) => {
                    let kv = [KEEPALIVE_BYTE];
                    notify_fragments(conn, tx_handle, 1, |_| &kv, "keepalive", kv.len()).await;
                    last_keepalive.set(Instant::now());
                }
            }
        }
    };

    let _ = select(inbound, outbound).await;
}

/// Push one packet's fragments through the SoftDevice notification queue,
/// in order and exactly once, and report it when that fails.
///
/// The thin driver half of Codeberg #264. Every decision — retry the
/// same fragment, abort, give up on the budget — belongs to
/// [`leviculum_ble_tx::PacketTx`] and is unit-tested on the host; this
/// function only performs the actions and reports the outcome.
///
/// The wait is on `BLE_GATTS_EVT_HVN_TX_COMPLETE` (surfaced by
/// `nrf-softdevice` as `Server::on_notify_tx_complete`, forwarded to
/// [`HVN_TX_DRAINED`] by [`NotifyAwareServer`]), never on a guessed
/// interval, and it is bounded by [`DRAIN_WAIT_MS`] so a peer that
/// stopped listening cannot wedge the outbound task.
///
/// `fragment` yields fragment `index`; the caller keeps the buffers, so
/// nothing is copied or allocated here.
async fn notify_fragments<'a, F>(
    conn: &Connection,
    handle: u16,
    fragment_count: usize,
    fragment: F,
    kind: &str,
    packet_len: usize,
) where
    F: Fn(usize) -> &'a [u8],
{
    // A drain edge still pending here belongs to a fragment of an
    // earlier packet and has already been paid for. Clearing it keeps
    // the first wait of this packet an honest measurement.
    HVN_TX_DRAINED.reset();

    let (mut tx, mut action) = PacketTx::start(fragment_count);
    loop {
        match action {
            Action::Send { index } => {
                let outcome = match gatt_server::notify_value(conn, handle, fragment(index)) {
                    Ok(()) => NotifyOutcome::Sent,
                    // The queue is full; the fragment was NOT taken.
                    Err(NotifyValueError::Raw(RawError::Resources)) => NotifyOutcome::QueueFull,
                    Err(NotifyValueError::Disconnected) => NotifyOutcome::Disconnected,
                    Err(NotifyValueError::Raw(err)) => NotifyOutcome::Failed(u32::from(err)),
                };
                action = tx.step(Event::Notify(outcome));
            }
            Action::AwaitDrain { .. } => {
                let event =
                    match select(HVN_TX_DRAINED.wait(), Timer::after_millis(DRAIN_WAIT_MS)).await {
                        Either::First(()) => Event::Drained,
                        Either::Second(()) => Event::WaitTimedOut,
                    };
                action = tx.step(event);
            }
            Action::Done => {
                BLE_TX_PACKETS.fetch_add(1, Ordering::Relaxed);
                BLE_TX_DRAIN_WAITS.fetch_add(tx.drain_waits(), Ordering::Relaxed);
                return;
            }
            Action::Abort { index, reason } => {
                report_tx_drop(
                    kind,
                    packet_len,
                    index,
                    fragment_count,
                    &tx,
                    reason.as_str(),
                    reason.code(),
                );
                return;
            }
            // Unreachable: every event fed above answers the action just
            // performed. Reported rather than swallowed — a silently
            // dropped packet is the exact bug this function removes.
            Action::Nothing => {
                report_tx_drop(
                    kind,
                    packet_len,
                    tx.fragments_sent(),
                    fragment_count,
                    &tx,
                    "internal",
                    0,
                );
                return;
            }
        }
    }
}

/// Emit the structured drop event and bump the counters.
///
/// Format per `docs/src/structured-event-logs.md`: `NAME key=value …
/// t=<ms>`, one line, scalar values, no whitespace inside a value — so
/// `grep BLE_TX_DROP` over a captured debug-port log is a usable
/// measurement of how much BLE traffic never left the node.
///
/// The trailing `t=` is not written here: `log_fmt` appends it to every
/// line, from the same `Instant::now()`. This call site used to write
/// its own, which after that change rendered the field twice.
fn report_tx_drop(
    kind: &str,
    packet_len: usize,
    index: usize,
    fragment_count: usize,
    tx: &PacketTx,
    reason: &str,
    code: u32,
) {
    let dropped = BLE_TX_DROPPED.fetch_add(1, Ordering::Relaxed) + 1;
    BLE_TX_DRAIN_WAITS.fetch_add(tx.drain_waits(), Ordering::Relaxed);
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_TX_DROP kind={} len={} frag={} of={} sent={} reason={} code={} waits={} dropped={}",
            kind,
            packet_len,
            index,
            fragment_count,
            tx.fragments_sent(),
            reason,
            code,
            tx.drain_waits(),
            dropped,
        ),
    );
}

/// Bring up S140 + start the BLE peripheral task. Peripherals previously
/// owned by MPSL/SDC (RTC0/TIMER0/PPI/RNG/etc.) are kept in the signature
/// for ABI compatibility with the binaries; the SoftDevice claims them
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
    let config = nrf_softdevice::Config {
        clock: Some(raw::nrf_clock_lf_cfg_t {
            // Synthesized LF from HF crystal; matches Heltec/RAK/Adafruit
            // factory bootloaders' expectation. RAK4631 + T114 have no
            // dedicated 32.768 kHz LF crystal.
            source: raw::NRF_CLOCK_LF_SRC_RC as u8,
            rc_ctiv: 16,
            rc_temp_ctiv: 2,
            accuracy: raw::NRF_CLOCK_LF_ACCURACY_500_PPM as u8,
        }),
        conn_gap: Some(raw::ble_gap_conn_cfg_t {
            conn_count: 1,
            event_length: 24,
        }),
        conn_gatt: Some(raw::ble_gatt_conn_cfg_t { att_mtu: 256 }),
        gatts_attr_tab_size: Some(raw::ble_gatts_cfg_attr_tab_size_t {
            attr_tab_size: raw::BLE_GATTS_ATTR_TAB_SIZE_DEFAULT,
        }),
        gap_role_count: Some(raw::ble_gap_cfg_role_count_t {
            adv_set_count: 1,
            periph_role_count: 1,
            central_role_count: 0,
            central_sec_count: 0,
            _bitfield_1: raw::ble_gap_cfg_role_count_t::new_bitfield_1(0),
        }),
        gap_device_name: Some(raw::ble_gap_cfg_device_name_t {
            p_value: b"leviculum" as *const u8 as _,
            current_len: 9,
            max_len: 9,
            write_perm: unsafe { mem::zeroed() },
            _bitfield_1: raw::ble_gap_cfg_device_name_t::new_bitfield_1(
                raw::BLE_GATTS_VLOC_STACK as u8,
            ),
        }),
        ..Default::default()
    };

    let sd = Softdevice::enable(&config);

    static SERVER: StaticCell<NotifyAwareServer> = StaticCell::new();
    let server = SERVER.init(NotifyAwareServer {
        inner: ReticulumServer::new(sd).expect("GATT server"),
    });

    spawner.must_spawn(softdevice_task(sd, vbus));
    spawner.must_spawn(ble_task(sd, server, identity_hash));

    sd
}
