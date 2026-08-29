//! The Columba BLE protocol (v2.2, advertisement records v0.3.0).
//!
//! Everything on this side of the seam is what a Columba peer expects to
//! see on the air and nothing else:
//!
//! - the GATT service and its three characteristics, under the
//!   `37145b00-…` vendor UUID space,
//! - the 16-byte identity handshake that opens a session,
//! - the 1-byte keepalive that holds it open,
//! - the advertisement: the service UUID a scanner filters on, the
//!   `LN-<hex8>` name in the scan response, and the v0.3.0 capability
//!   record,
//! - and, in phase B of #255, the MAC-sorting rule that decides which of
//!   two nodes initiates.
//!
//! What it is built on — SoftDevice bring-up, the packet channels, the
//! per-connection drain table, the fragment pump — lives in
//! [`super`] and [`super::notify`] and carries no Columba knowledge. See
//! the [`super`] module docs for the seam and what still crosses it.

use core::cell::Cell;

use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use embassy_time::{Duration, Instant, Timer};
use leviculum_ble_tx::{
    device_name, manufacturer_data, ADV_BYTES_USED, CAP_PERIPHERAL_ONLY, LEGACY_AD_CAPACITY,
    MANUFACTURER_DATA_LEN,
};
use leviculum_core::framing::ble::{
    self as ble_framing, BleDefragmenter, DefragResult, FRAGMENT_HEADER_SIZE, KEEPALIVE_BYTE,
    KEEPALIVE_INTERVAL_MS,
};
use nrf_softdevice::ble::advertisement_builder::{
    AdvertisementDataType, Flag, LegacyAdvertisementBuilder, LegacyAdvertisementPayload,
    ServiceList,
};
use nrf_softdevice::ble::gatt_server::{Server, WriteOp};
use nrf_softdevice::ble::{gatt_server, peripheral, Connection};
use nrf_softdevice::Softdevice;
use static_cell::StaticCell;

use super::notify::{notify_fragments, BLE_TX_DRAIN_UNROUTED};
use super::{BLE_INCOMING, BLE_OUTGOING, HVN_DRAIN};

/// The two data characteristics are 251 bytes wide, which is also the
/// largest GATTS write event the SoftDevice can hand us (251 bytes of data
/// behind an 18-byte event header). The buffer that event is read into is
/// sized by the `nrf-softdevice/evt-max-size-*` feature in `Cargo.toml`, and
/// nrf-softdevice panics rather than truncating when the event does not fit
/// (Codeberg #354). Widening 251 means rechecking that feature.
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

/// [`ReticulumServer`] plus the one `Server` callback the
/// `#[gatt_server]` macro does not generate.
///
/// The macro emits `on_write` only; every other callback keeps the
/// trait's default, and the default for `on_notify_tx_complete` throws
/// the event away. That event is exactly what tells us a connection's
/// HVN queue has room again, so the impl is written by hand here and the
/// write path is delegated unchanged.
///
/// This is seam leak (2) from the [`super`] docs: the routing it
/// performs is protocol-neutral, but `Server` is implemented on a
/// concrete GATT server, so each protocol owns the obligation to call
/// it.
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

    /// One or more notifications left the SoftDevice's queue for `conn`.
    ///
    /// `count` says how many; the fragment pump waits on the edge rather
    /// than on a credit count, so it is not forwarded — one edge is
    /// enough to make it re-offer, and a full queue simply refuses again.
    fn on_notify_tx_complete(&self, conn: &Connection, _count: u8) -> Option<Self::Event> {
        let routed = match conn.handle() {
            Some(handle) => HVN_DRAIN.drained(handle),
            // The connection is already being torn down. There is no
            // waiter this edge can honestly answer; waking one anyway is
            // what the pre-phase-A global signal did.
            None => false,
        };
        if !routed {
            BLE_TX_DRAIN_UNROUTED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        None
    }
}

// Static advertising payload. 16-byte service UUID in little-endian.
//
// Same UUID as the GATT service: 37145b00-442d-4a94-917f-8f42c5da28e3,
// reversed to LE: e3 28 da c5 42 8f 7f 91 94 4a 2d 44 00 5b 14 37
const RETICULUM_SVC_UUID_LE: [u8; 16] = [
    0xe3, 0x28, 0xda, 0xc5, 0x42, 0x8f, 0x7f, 0x91, 0x94, 0x4a, 0x2d, 0x44, 0x00, 0x5b, 0x14, 0x37,
];

/// The v0.3.0 capability record this node advertises.
///
/// `PERIPHERAL_ONLY` is set because [`super`]'s `gap_role_count` has
/// `central_role_count: 0`: we cannot initiate, so a peer must not defer
/// to the v2.2 address sort and wait for us. Phase B clears the bit in
/// the same commit that raises the role count — the two are one fact
/// stated twice, and they must not drift. See [`leviculum_ble_tx::adv`]
/// for the layout and the byte budget.
const CAPABILITY_AD: [u8; MANUFACTURER_DATA_LEN] = manufacturer_data(CAP_PERIPHERAL_ONLY);

// The advertisement builder panics on overflow, on a board, at boot. The
// budget is arithmetic over constants, so it is decided here instead.
const _: () = assert!(ADV_BYTES_USED <= LEGACY_AD_CAPACITY);

/// Register the GATT service and spawn the Columba peripheral task.
/// Called by [`super::init`] once the SoftDevice is enabled.
///
/// Takes the SoftDevice by unique reference and hands back a shared one:
/// registering a service is the only step that needs exclusive access
/// (`ReticulumServer::new` adds attributes to the SoftDevice's table),
/// and everything after it — the SoC-event task, the flash writes in
/// [`crate::radio_store`] — shares the handle.
pub fn spawn(
    spawner: &Spawner,
    sd: &'static mut Softdevice,
    identity_hash: [u8; 16],
) -> &'static Softdevice {
    static SERVER: StaticCell<NotifyAwareServer> = StaticCell::new();
    let server = SERVER.init(NotifyAwareServer {
        inner: ReticulumServer::new(sd).expect("GATT server"),
    });
    let sd: &'static Softdevice = sd;
    spawner.must_spawn(ble_task(sd, server, identity_hash));
    sd
}

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
            .services_128(ServiceList::Complete, &[RETICULUM_SVC_UUID_LE])
            .raw(
                AdvertisementDataType::MANUFACTURER_SPECIFIC_DATA,
                &CAPABILITY_AD,
            )
            .build(),
    );
    // Individual per-node name, `LN-<hex8>` from the identity hash
    // (#255) — same hex as the LXMF display name Columba shows, see
    // `leviculum_ble_tx::device_name`. Hex output is ASCII, so the
    // `from_utf8` fallback arm is unreachable. It rides in the SCAN
    // RESPONSE, a second 31-byte PDU, so it does not compete with the
    // advertisement's budget.
    let name = device_name(&identity_hash);
    let name = core::str::from_utf8(&name).unwrap_or("LN-invalid");
    let scan = SCAN_DATA.init(LegacyAdvertisementBuilder::new().full_name(name).build());

    // The measured bytes, not the computed ones: if the builder ever
    // disagrees with `ADV_BYTES_USED`, the capture says so.
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "ADV adv_bytes={} scan_bytes={} cap={} peripheral_only={}",
            adv.as_ref().len(),
            scan.as_ref().len(),
            LEGACY_AD_CAPACITY,
            u8::from(CAPABILITY_AD[3] & CAP_PERIPHERAL_ONLY != 0),
        ),
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
                // The ATT MTU the peer and we settled on, reported here
                // rather than at connect because the Exchange MTU Request
                // arrives after the connection event. `on_disconnected` does
                // not clear it and we still hold the `Connection`, so the
                // negotiated value is still readable. This is the only place
                // `conn_gatt.att_mtu` becomes observable from outside the
                // board — without it, "is our conn_cfg in force?" can only be
                // answered by reading crate source.
                crate::info!("BLE: disconnected att_mtu={}", conn.att_mtu());
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
    incoming_tx: &Sender<'static, CriticalSectionRawMutex, alloc::vec::Vec<u8>, 4>,
    outgoing_rx: &Receiver<'static, CriticalSectionRawMutex, alloc::vec::Vec<u8>, 4>,
) {
    // This connection's drain edge. Claimed for the lifetime of the
    // connection and released below, so a reconnect (or, from phase B,
    // a second link) never inherits another link's pending edge.
    let Some(conn_handle) = conn.handle() else {
        crate::warn!("BLE: connection without a handle, dropping it");
        return;
    };
    let Some(drain) = HVN_DRAIN.claim(conn_handle) else {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_DRAIN_TABLE_FULL conn={} slots={}",
                conn_handle,
                HVN_DRAIN.capacity()
            ),
        );
        return;
    };

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
                        drain,
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
                    notify_fragments(conn, drain, tx_handle, 1, |_| &kv, "keepalive", kv.len())
                        .await;
                    last_keepalive.set(Instant::now());
                }
            }
        }
    };

    let _ = select(inbound, outbound).await;

    HVN_DRAIN.release(conn_handle);
}
