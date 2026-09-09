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

use core::cell::{Cell, RefCell};

use embassy_executor::Spawner;
use embassy_futures::select::{select, select3, Either, Either3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::channel::Sender;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use leviculum_ble_tx::{
    addr_value, manufacturer_data, parse_peer_advertisement, should_initiate, CandidateTable,
    ConnectDecision, PeerRegistry, ScanMode, TxGap, ADV_BYTES_USED, CAP_PERIPHERAL_ONLY,
    LEGACY_AD_CAPACITY, MANUFACTURER_DATA_LEN, SCAN_FALLBACK_AFTER_MS, SCAN_WINDOW_COLLECT_MS,
    WINDOW_CANDIDATES,
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
use nrf_softdevice::ble::{central, gatt_client, gatt_server, peripheral, Address, Connection};
use nrf_softdevice::Softdevice;
use static_cell::StaticCell;

use super::notify::{notify_fragments, BLE_TX_DRAIN_UNROUTED, BLE_TX_DROPPED, BLE_TX_PACKETS};
use super::{CarrierWaiter, BLE_INCOMING, HVN_DRAIN, MAX_LINKS};

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

/// The capability bits this node advertises AND feeds into its own side
/// of the connection decision — one constant so the record on the air
/// and the [`should_initiate`] input cannot disagree.
///
/// Zero since phase B: [`super`]'s `gap_role_count` now grants one
/// central role, we can initiate, so `PERIPHERAL_ONLY` (bit 0) is
/// cleared in the same commit — the two are one fact stated twice, and
/// they must not drift.
const LOCAL_CAPS: u8 = 0;

/// The v0.3.0 capability record this node advertises.
///
/// The record STAYS in the advertisement with the flag cleared, rather
/// than being dropped: a peer that sees no manufacturer data at all
/// cannot tell a central-capable v0.3.0 node from a pre-v0.3.0 node
/// that never spoke the version, and would sort against the wrong rule
/// (v0.3.0 §3.2; asserted host-side by [`leviculum_ble_tx::adv`]'s
/// `clearing_the_flag_leaves_the_record_and_its_size_alone`). See
/// [`leviculum_ble_tx::adv`] for the layout and the byte budget.
const CAPABILITY_AD: [u8; MANUFACTURER_DATA_LEN] = manufacturer_data(LOCAL_CAPS);

// The advertisement builder panics on overflow, on a board, at boot. The
// budget is arithmetic over constants, so it is decided here instead.
const _: () = assert!(ADV_BYTES_USED <= LEGACY_AD_CAPACITY);

/// Register the GATT service and spawn the Columba tasks: one
/// peripheral task per incoming link slot (#372) and the central half.
/// Called by [`super::init`] once the SoftDevice is enabled.
///
/// Takes the SoftDevice by unique reference and hands back a shared one:
/// registering a service is the only step that needs exclusive access
/// (`ReticulumServer::new` adds attributes to the SoftDevice's table),
/// and everything after it — the SoC-event task, the flash writes in
/// [`crate::radio_store`] — shares the handle.
///
/// The advertising and scan-response payloads are built here, once, for
/// all peripheral tasks: nrf-softdevice's `advertise_connectable` wants
/// `&'static` slices, and the name in the scan response is the boot
/// name — the operator's (#235) if one is set, `LN-<hex8>` from the
/// identity hash otherwise (#255), the same value
/// `crate::ble::set_gap_device_name` writes into the GAP attribute so
/// the two BLE surfaces cannot disagree, truncated visibly on a
/// codepoint boundary to `DEVICE_NAME_LEN`. It rides in the SCAN
/// RESPONSE, a second 31-byte PDU, so it does not compete with the
/// advertisement's budget. Built once into `StaticCell`s the SoftDevice
/// holds `&'static`s to for the life of the advertising loops: that is
/// why a name set at runtime reaches BLE only at the next boot, and why
/// the control frame's report says so rather than implying otherwise.
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

    // Publish the identity characteristic value so a connecting peer can
    // read it before exchanging frames over rx/tx.
    let _ = server.inner.reticulum_service.identity_set(&identity_hash);

    static ADV_DATA: StaticCell<LegacyAdvertisementPayload> = StaticCell::new();
    static SCAN_DATA: StaticCell<LegacyAdvertisementPayload> = StaticCell::new();
    let adv: &'static LegacyAdvertisementPayload = ADV_DATA.init(
        LegacyAdvertisementBuilder::new()
            .flags(&[Flag::GeneralDiscovery, Flag::LE_Only])
            .services_128(ServiceList::Complete, &[RETICULUM_SVC_UUID_LE])
            .raw(
                AdvertisementDataType::MANUFACTURER_SPECIFIC_DATA,
                &CAPABILITY_AD,
            )
            .build(),
    );
    let name = crate::name::boot_gap_name();
    let scan: &'static LegacyAdvertisementPayload = SCAN_DATA.init(
        LegacyAdvertisementBuilder::new()
            .full_name(name.as_str())
            .build(),
    );

    // The measured bytes, not the computed ones: if the builder ever
    // disagrees with `ADV_BYTES_USED`, the capture says so. Critical
    // path: this fires once, at boot, before the host's DTR-assert has
    // opened the runtime drain — the gated `log_fmt` silently dropped
    // it and the #372 rig proof could not observe its own checkpoint.
    crate::log::log_fmt_critical(
        "[BLE ] ",
        format_args!(
            "ADV adv_bytes={} scan_bytes={} cap={} peripheral_only={} periph_links={}",
            adv.as_ref().len(),
            scan.as_ref().len(),
            LEGACY_AD_CAPACITY,
            u8::from(CAPABILITY_AD[3] & CAP_PERIPHERAL_ONLY != 0),
            super::PERIPH_LINKS,
        ),
    );

    for index in 0..super::PERIPH_LINKS {
        spawner.must_spawn(peripheral_task(sd, server, adv, scan, index));
    }
    // Phase B (#255): the central half — scan, decide, initiate. Both
    // halves of the protocol spawn here, behind the one entry point the
    // neutral module calls.
    spawner.must_spawn(central_task(sd, identity_hash));
    sd
}

/// Serializes the advertise phase across the peripheral tasks: the
/// SoftDevice runs ONE advertising set (`adv_set_count: 1`), so exactly
/// one free task may hold `advertise_connectable` at a time. On a
/// connect the winner releases the lock and serves its session; the
/// next free task takes over the advertisement, which is what keeps the
/// board connectable while any slot is free (#372). With every slot in
/// a session nobody holds the lock and nothing advertises — a full
/// board is honestly silent rather than accepting a connection it
/// would immediately have to refuse.
static ADV_LOCK: Mutex<CriticalSectionRawMutex, ()> = Mutex::new(());

/// One incoming-link slot: advertise (serialized on [`ADV_LOCK`]),
/// accept, serve the session, repeat. `index` is the task's identity in
/// the logs and its carrier-wake latch; the session itself is keyed by
/// its drain slot, as every session is.
#[embassy_executor::task(pool_size = super::PERIPH_LINKS)]
async fn peripheral_task(
    sd: &'static Softdevice,
    server: &'static NotifyAwareServer,
    adv: &'static LegacyAdvertisementPayload,
    scan: &'static LegacyAdvertisementPayload,
    index: usize,
) {
    let incoming_tx = BLE_INCOMING.sender();

    loop {
        // The runtime carrier gate: a profile switched to `ble=off`
        // holds the task here, so nothing is on the air — no
        // advertisement, no acceptable connection. Off→on (the carrier
        // came up at boot, it was only gated) falls through and resumes
        // advertising. Logged only when it actually gates, once per
        // gated period, not once per connection.
        if !crate::media::ble_active() {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_GATE role=peripheral adv={index} state=off"),
            );
            super::carrier_on(CarrierWaiter::Peripheral(index)).await;
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_GATE role=peripheral adv={index} state=on"),
            );
        }
        let conn = {
            let _adv_turn = ADV_LOCK.lock().await;
            // The carrier can flip off while this task waits for the
            // lock; back to the gate rather than advertising a
            // switched-off carrier onto the air.
            if !crate::media::ble_active() {
                continue;
            }
            let config = peripheral::Config::default();
            let advertisement = peripheral::ConnectableAdvertisement::ScannableUndirected {
                adv_data: adv.as_ref(),
                scan_data: scan.as_ref(),
            };
            match select(
                peripheral::advertise_connectable(sd, advertisement, &config),
                super::carrier_off(CarrierWaiter::Peripheral(index)),
            )
            .await
            {
                Either::First(Ok(conn)) => Some(conn),
                Either::First(Err(_)) => None,
                // Carrier switched off while advertising: dropping the
                // advertise future is what stops the advertisement (the
                // SoftDevice cancels it on drop). The gate above then
                // holds the loop.
                Either::Second(()) => continue,
            }
            // The lock drops here: a session must not hold the
            // advertisement hostage, and a failed advertise must not
            // spin-hold it through its backoff.
        };
        match conn {
            Some(conn) => {
                crate::info!("BLE: connected");
                gatt_events(&conn, server, &incoming_tx).await;
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
            None => {
                Timer::after_millis(1000).await;
            }
        }
    }
}

/// Run one inbound frame through a link's defragmenter, and say so when
/// that cost a partial packet (#373): a new START over an unfinished
/// head, a `total` that contradicts the reassembly in progress, or the
/// hard reset after a garbage frame each discard one whole in-progress
/// Reticulum packet — and before this line the discard was invisible on
/// every surface. `lost=` is what this frame cost, `total=` the
/// defragmenter's running count for the link, so one line states the
/// incident and the history.
fn process_logged(
    d: &mut BleDefragmenter,
    data: &[u8],
    now_ms: u64,
    slot_index: usize,
) -> DefragResult {
    let before = d.abandoned_count();
    let result = d.process(data, now_ms);
    if matches!(result, DefragResult::Error) {
        // Hard reset: a garbage frame amid a reassembly must not leave a
        // stale head for the next packet's tail to complete (#255), and
        // the head it discards is a loss this link must report.
        d.abandon();
    }
    let after = d.abandoned_count();
    if after != before {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_RX_ABANDON slot={} lost={} total={}",
                slot_index,
                after.saturating_sub(before),
                after,
            ),
        );
    }
    result
}

/// Hold the next packet back until this connection's inter-packet gap
/// (#376, `--set-ble-tx-gap`) has elapsed. Both pumps — the peripheral
/// notify path and the central write path — call this before a packet's
/// first fragment and [`TxGap::packet_done`] after its last, so the gap
/// is packet-to-packet on one connection handle, exactly as specified.
/// Keepalives bypass it on both sides: the knob is specified between
/// packets, and a keepalive that slid the window would change the
/// quantity being swept. With the gap at 0 (the default) the wait is 0
/// and nothing is logged — the fleet path is untouched.
async fn serve_tx_gap(gap: &TxGap, conn_handle: u16) {
    let wait_ms = gap.wait_ms(Instant::now().as_millis(), super::tx_gap_ms());
    if wait_ms > 0 {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!("BLE_TX_GAP conn={} waited_ms={}", conn_handle, wait_ms),
        );
        Timer::after(Duration::from_millis(wait_ms)).await;
    }
}

/// Per-connection event-loop. Inbound writes drive `gatt_server::run`'s
/// closure (Columba defrag + handshake state); outbound BLE_OUTGOING and
/// keepalive timer feed `gatt_server::notify_value`. The two halves run
/// concurrently via `embassy_futures::select`.
async fn gatt_events(
    conn: &Connection,
    server: &NotifyAwareServer,
    incoming_tx: &Sender<
        'static,
        CriticalSectionRawMutex,
        (Option<[u8; 16]>, alloc::vec::Vec<u8>),
        4,
    >,
) {
    // This connection's drain edge. Claimed for the lifetime of the
    // connection and released below, so a reconnect (or a second link,
    // since phase B) never inherits another link's pending edge. The
    // slot index doubles as the link's identity: it selects the
    // per-link outbound queue the fan-out feeds and the [`LIVE_PEERS`]
    // entry the handshake fills.
    let Some(conn_handle) = conn.handle() else {
        crate::warn!("BLE: connection without a handle, dropping it");
        return;
    };
    let Some((slot_index, drain)) = HVN_DRAIN.claim_indexed(conn_handle) else {
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
    // The connection event, before any identity: the peer's address
    // enters the registry (pre-dial exclusion) and the fallback clock
    // restarts (#375 §0 — resetting only at identity left a 3 s gap
    // the rig's fallback fired straight into).
    conn_link_up(slot_index, addr_value(&conn.peer_address().bytes()));

    // Per-connection state. `Cell` lets the closure inside
    // `gatt_server::run` mutate handshake_done while the outbound branch
    // reads it. Single-task context (the executor polls one future at a
    // time), so no Mutex is needed.
    let defrag: Cell<BleDefragmenter> = Cell::new(BleDefragmenter::new());
    let handshake_done: Cell<bool> = Cell::new(false);
    let last_keepalive: Cell<Instant> = Cell::new(Instant::now());
    // The identity this link's peer presented in the handshake, for
    // tagging inbound packets with their ingress link (Codeberg #365).
    // `None` until the handshake lands.
    let link_peer: Cell<Option<[u8; 16]>> = Cell::new(None);

    // This link's private outbound queue (fed by the fan-out; see
    // super::LINK_OUT). Drain packets left over from the slot's
    // previous tenancy before serving this connection.
    let outgoing_rx = super::link_out(slot_index).receiver();
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
                let mut peer_id = [0u8; 16];
                peer_id.copy_from_slice(&data);
                peer_link_up(slot_index, peer_id);
                link_peer.set(Some(peer_id));
                handshake_done.set(true);
                last_keepalive.set(Instant::now());
            } else if data.len() < FRAGMENT_HEADER_SIZE {
                // Single-byte keepalive (0x00); nothing to defragment.
            } else {
                let now = Instant::now().as_millis();
                let mut d = defrag.replace(BleDefragmenter::new());
                let result = process_logged(&mut d, &data, now, slot_index);
                defrag.set(d);
                match result {
                    DefragResult::Complete(packet) => {
                        crate::info!("BLE: RX {}B", packet.len());
                        // try_send: if the consumer is slow and the 4-deep
                        // channel is full, drop the packet rather than block
                        // here (we're in a sync closure, can't await).
                        let _ = incoming_tx.try_send((link_peer.get(), packet));
                    }
                    DefragResult::NeedMore | DefragResult::Error => {}
                }
            }
        }
    });

    let outbound = async {
        // This connection's inter-packet gap state (#376); dies with the
        // link, so a reconnect starts unpaced.
        let mut tx_gap = TxGap::new();
        loop {
            let keepalive_deadline = if handshake_done.get() {
                Timer::at(last_keepalive.get() + Duration::from_millis(KEEPALIVE_INTERVAL_MS))
            } else {
                Timer::at(Instant::MAX)
            };

            match select(outgoing_rx.receive(), keepalive_deadline).await {
                Either::First(packet) => {
                    serve_tx_gap(&tx_gap, conn_handle).await;
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
                    tx_gap.packet_done(Instant::now().as_millis());
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

    // The third arm is the runtime carrier switch (`--set-media
    // ble=off`): the link is disconnected exactly as if the peer had
    // walked out of range — the teardown below reports the loss through
    // the same `peer_link_down`, so the core's cull is identical.
    if let Either3::Third(()) = select3(
        inbound,
        outbound,
        super::carrier_off(CarrierWaiter::Session(slot_index)),
    )
    .await
    {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!("BLE_CARRIER_DROP role=peripheral slot={}", slot_index),
        );
        // Already-disconnected is fine; the teardown is the same.
        let _ = conn.disconnect();
    }

    // Registry entry first, then the slot: a slot that reads free while
    // the identity still reads linked would refuse a legitimate
    // reconnect in the central task's duplicate check.
    peer_link_down(slot_index);
    conn_link_down(slot_index);
    HVN_DRAIN.release(conn_handle);
}

// --- The central half (#255 phase B): scan, decide, initiate ---

/// The 16-byte identities of currently linked peers, indexed by the
/// link's drain-slot (the same index that selects its outbound queue).
///
/// v2.2 keys everything durable by identity precisely because BLE
/// addresses rotate (§"Why Not Use MAC Addresses as Keys?"). The
/// scanner cannot apply that lesson pre-connect — an advertisement
/// carries no identity — so a peer we already hold a link to can rotate
/// its address and reappear as a seemingly new device. This registry is
/// the post-connect closure of that hole: the central path reads the
/// peer's Identity characteristic first and drops the connection if
/// that identity is already live ([`BLE_LINK_DUP`] in the log).
///
/// The first/last-link rules live host-tested in
/// [`leviculum_ble_tx::registry`]; this is the one firmware instance,
/// behind the critical-section mutex the host crate cannot need.
static LIVE_PEERS: BlockingMutex<CriticalSectionRawMutex, RefCell<PeerRegistry<MAX_LINKS>>> =
    BlockingMutex::new(RefCell::new(PeerRegistry::new()));

/// Register a slot's peer and, when this is the identity's FIRST link,
/// report the arrival to the main loop so the transport learns about
/// the peer behind the new link (Codeberg #365) — the mirror
/// of [`peer_link_down`]'s last-link rule. A same-identity link on
/// another slot (the zombie-displacement window) means the peer was
/// never gone: registry churn, not an arrival, so no report.
fn peer_link_up(slot_index: usize, peer_id: [u8; 16]) {
    let first = LIVE_PEERS.lock(|peers| peers.borrow_mut().link_up(slot_index, peer_id));
    if first {
        super::report_peer_event(super::PeerEvent::Up(peer_id));
    }
    // A link up in either role restarts the fallback clock (#375): the
    // board is reachable over BLE again, so the strict rule gets its
    // full bound before the scanner may dial against the sort.
    note_strict_reset();
}

/// Clear a slot's registry entry and, when that took the peer's LAST
/// link, report the loss to the main loop so the transport culls the
/// paths via that peer (Codeberg #365). A same-identity link on another
/// slot (the zombie-displacement window) means the peer is still
/// reachable: registry churn, not a loss, so no report.
fn peer_link_down(slot_index: usize) {
    let lost = LIVE_PEERS.lock(|peers| peers.borrow_mut().link_down(slot_index));
    if let Some(identity) = lost {
        super::report_peer_event(super::PeerEvent::Lost(identity));
    }
}

fn peer_already_linked(peer_id: &[u8; 16]) -> bool {
    LIVE_PEERS.lock(|peers| peers.borrow().is_linked(peer_id))
}

/// Register a connection's address at the connection event, in either
/// role — before any identity is known — and restart the strict phase:
/// the rig showed the fallback firing in the 3 s gap between `BLE:
/// connected` and the identity handshake (#375 §0), because the clock
/// used to reset only at identity.
fn conn_link_up(slot_index: usize, addr_value: u64) {
    LIVE_PEERS.lock(|peers| peers.borrow_mut().conn_up(slot_index, addr_value));
    note_strict_reset();
}

/// Clear a connection's address at teardown. The strict phase restarts
/// here too: a board that just lost a link gets the full strict bound
/// before it may dial against the sort.
fn conn_link_down(slot_index: usize) {
    LIVE_PEERS.lock(|peers| peers.borrow_mut().conn_down(slot_index));
    note_strict_reset();
}

/// Whether a live connection was made on this address. Core Spec Vol 6
/// Part B §4.5 permits one connection per address pair — the initiator
/// shall not dial an advertiser it is connected to, and the advertiser
/// shall ignore such a CONNECT_IND — so a dial here can only time out
/// (5 s on the air, the rig's `BLE_CENTRAL_FAIL … err=Timeout` loop)
/// and the scanner excludes the address before dialling.
fn addr_already_linked(addr_value: u64) -> bool {
    LIVE_PEERS.lock(|peers| peers.borrow().addr_linked(addr_value))
}

/// The number of distinct live peer identities on the BLE interface
/// (Codeberg #365) — what the main loop mirrors into the core as the
/// interface's peer count, next to the `is_online` mirror. Counted from
/// the post-handshake registry, not the drain table: a link that has
/// not presented an identity yet is not a peer the transport can ask
/// anything of.
pub(crate) fn live_peer_count() -> usize {
    LIVE_PEERS.lock(|peers| peers.borrow().peer_count())
}

/// The Columba service from the client side — the same three
/// characteristics as [`ReticulumService`], with the directions
/// mirrored (v2.2 §GATT Service Structure: we write the peer's RX, the
/// peer notifies us on its TX, its Identity is read once at connect).
#[nrf_softdevice::gatt_client(uuid = "37145b00-442d-4a94-917f-8f42c5da28e3")]
pub struct ReticulumClient {
    #[characteristic(uuid = "37145b00-442d-4a94-917f-8f42c5da28e5", write)]
    rx: heapless_v8::Vec<u8, 251>,

    #[characteristic(uuid = "37145b00-442d-4a94-917f-8f42c5da28e4", read, notify)]
    tx: heapless_v8::Vec<u8, 251>,

    #[characteristic(uuid = "37145b00-442d-4a94-917f-8f42c5da28e6", read)]
    identity: [u8; 16],
}

/// Discovery-scan cadence, in 625 µs units: a 30 ms listen every
/// 100 ms. Passive — the decision needs only the advertising PDU
/// (service UUID + capability record), so scan requests would spend
/// airtime to learn a name we do not use (v2.2 §Discovery Phase:
/// matching is by service UUID, never by name). The radio is shared
/// with our own advertising and up to four live connections; 30 %
/// leaves the SoftDevice scheduler room for all of them.
const SCAN_INTERVAL_625US: u32 = 160;
const SCAN_WINDOW_625US: u32 = 48;

/// Bound on one connect attempt, in 10 ms units (5 s). The peer
/// advertised moments ago; if it does not answer a CONNECT_IND within
/// seconds it is gone or busy, and the scanner should look again.
const CONNECT_TIMEOUT_10MS: u16 = 500;

/// Pause after a session ended or an attempt failed, before the next
/// scan pass. Keeps a refusing/vanishing peer from being hammered.
const CENTRAL_RETRY_BACKOFF_MS: u64 = 5_000;

// The fallback bound itself, `SCAN_FALLBACK_AFTER_MS` (30 s), lives in
// `leviculum_ble_tx::window` since #375 part 2 — lnsd runs the same
// clock, and a shared constant cannot drift. The firmware-side
// arithmetic behind it still holds: one failed strict attempt costs up
// to [`CONNECT_TIMEOUT_10MS`] (5 s) plus [`CENTRAL_RETRY_BACKOFF_MS`]
// (5 s), so 30 s spans several complete search-connect-backoff cycles
// at the 30 %-duty passive scan cadence
// ([`SCAN_INTERVAL_625US`]/[`SCAN_WINDOW_625US`]).

/// The strict-phase clock behind [`SCAN_FALLBACK_AFTER_MS`]: when the
/// current strict phase began (in `embassy_time` ticks) and whether the
/// switch to fallback was already logged for this phase. Reset by
/// [`note_strict_reset`] — a connection event in either role, a
/// teardown, or the strict rule producing a target — and otherwise it
/// runs whenever the central task is scanning, live links
/// notwithstanding (the eager spec, #375 part 3). Live links no longer
/// suspend it: the doomed dial that motivated the quiet spec is
/// excluded by address before it can leave the scanner
/// ([`addr_already_linked`], Core Spec Vol 6 Part B §4.5), a failed
/// fallback dial is backed off by [`RecentDeadEnds`], and the quiet
/// suspension itself measurably cost merges —
/// `ble-tx/tests/graph_formation.rs` puts it at 28 and 78 all-linked
/// splits per 1000 arrival orders at 10 and 20 boards against eager's
/// zero.
///
/// A blocking mutex over a `Cell`, like [`LIVE_PEERS`]: written from
/// the peripheral tasks (via [`peer_link_up`]) and read from the
/// central task's scan callback, and the ticks are 64-bit, which the
/// target has no atomic for.
static FALLBACK_CLOCK: BlockingMutex<CriticalSectionRawMutex, Cell<(u64, bool)>> =
    BlockingMutex::new(Cell::new((0, false)));

/// Restart the strict phase: a link came up in either role, or the
/// strict rule found a permitted peer — either way the board is not
/// stranded, so the fallback clock starts over.
fn note_strict_reset() {
    FALLBACK_CLOCK.lock(|clock| clock.set((Instant::now().as_ticks(), false)));
}

/// The [`ScanMode`] the current strict phase has reached, logging the
/// strict-to-fallback switch once per phase (`BLE_SCAN_FALLBACK`).
///
/// Sampled per advertising report rather than per scan pass: a strict
/// pass that never finds a target never *ends* — `central::scan` runs
/// until its callback accepts a peer — so "the next pass" would never
/// come for exactly the stranded board the fallback exists for. The
/// timing stays an input to [`should_initiate`]; the rule itself never
/// measures it.
///
/// Under the eager spec a linked board reaches fallback too — on a
/// two-node rig the surviving scanner sits in fallback with its only
/// neighbour's address excluded, so no candidate ever opens a window
/// and nothing is dialled. `BLE_SCAN_FALLBACK` is still logged once
/// per phase, not once per pass: the `announced` flag only clears at
/// [`note_strict_reset`], and a board in that state has no connection
/// events to reset it.
fn scan_mode_now() -> ScanMode {
    // One lock for the read-check-mark sequence: a reset racing in
    // between two separate locks would be overwritten with the stale
    // phase. The log line itself stays outside the critical section.
    let announce = FALLBACK_CLOCK.lock(|clock| {
        let (since_ticks, announced) = clock.get();
        let elapsed_ms =
            Duration::from_ticks(Instant::now().as_ticks().saturating_sub(since_ticks)).as_millis();
        if elapsed_ms < SCAN_FALLBACK_AFTER_MS {
            return None;
        }
        if !announced {
            clock.set((since_ticks, true));
            return Some(Some(elapsed_ms));
        }
        Some(None)
    });
    match announce {
        None => ScanMode::Strict,
        Some(logged) => {
            if let Some(elapsed_ms) = logged {
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!("BLE_SCAN_FALLBACK after_ms={elapsed_ms}"),
                );
            }
            ScanMode::Fallback
        }
    }
}

/// How long a dead-end address is skipped: one that carried an
/// already-linked (or our own) identity, or that a fallback dial could
/// not connect to. Sized to the RPA rotation timescale (minutes): the
/// address dies on its own at the peer's next rotation, this just
/// stops us re-dialling it every scan pass until then — the rig's
/// pre-#375-part-2 capture shows the alternative, a 5 s timeout dial
/// at the same address every ~20 s, forever.
const DEAD_END_TTL: Duration = Duration::from_secs(120);

/// Room for the addresses [`DEAD_END_TTL`] talks about: up to
/// [`MAX_LINKS`] revealed duplicates plus a run of failed fallback
/// targets (at most one failure per ~13 s dial cycle, so eight slots
/// bridge more than one TTL of them). Overflow reuses the oldest
/// entry — an early re-dial, not a loss.
const DEAD_END_SLOTS: usize = 2 * MAX_LINKS;

/// The addresses the scanner must not dial for a while, and when each
/// was condemned: duplicates/self revealed post-connect, and failed
/// fallback dials (#375 §0 — a fallback target that does not answer a
/// CONNECT_IND within 5 s is gone or unreachable by rule, and without
/// this entry the 5 s backoff re-dialled it forever).
struct RecentDeadEnds {
    entries: [Option<(u64, Instant)>; DEAD_END_SLOTS],
}

impl RecentDeadEnds {
    const fn new() -> Self {
        Self {
            entries: [None; DEAD_END_SLOTS],
        }
    }

    fn note(&mut self, addr: u64) {
        let now = Instant::now();
        // Reuse an expired (or the oldest) entry.
        let slot = self
            .entries
            .iter()
            .position(|e| e.is_none_or(|(_, at)| now - at >= DEAD_END_TTL))
            .unwrap_or_else(|| {
                self.entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.map_or(Instant::MIN, |(_, at)| at))
                    .map_or(0, |(i, _)| i)
            });
        self.entries[slot] = Some((addr, now));
    }

    fn contains(&self, addr: u64) -> bool {
        let now = Instant::now();
        self.entries
            .iter()
            .flatten()
            .any(|&(a, at)| a == addr && now - at < DEAD_END_TTL)
    }
}

/// Scan until one advertisement wins an initiate decision, keep
/// collecting further eligible advertisers for one bounded window
/// ([`SCAN_WINDOW_COLLECT_MS`]), then return the best candidate — the
/// lowest eligible address, strict verdicts before fallback verdicts —
/// plus the rule that permitted it (the caller resets the fallback
/// clock on a strict verdict, #375) and the window's `seen` count for
/// the `BLE_SCAN_WINDOW` log line.
///
/// Dialling the FIRST eligible advertiser instead is what enabled the
/// saturated-cycle lock (`graph_formation.rs`: 48/1000 disconnected
/// orders at 20 boards; the window's choice gives 0). The choice
/// itself lives in [`CandidateTable`], the same pure structure the
/// simulation and lnsd use, so the three cannot drift.
///
/// Every connectable PDU carrying the Reticulum service UUID gets a
/// [`should_initiate`] verdict from the v2.2 sort + v0.3.0 override
/// (the rule lives host-tested in [`leviculum_ble_tx::peer`]), under
/// the [`ScanMode`] the fallback clock has reached at that report; the
/// verdict is logged once per (address, rule) change rather than per
/// PDU, because a waiting peer re-advertises several times a second.
async fn find_peer_to_initiate(
    sd: &'static Softdevice,
    own_addr_value: u64,
    skip: &RecentDeadEnds,
) -> Result<Option<(Address, ConnectDecision, usize)>, central::ScanError> {
    let config = central::ScanConfig {
        active: false,
        extended: false,
        interval: SCAN_INTERVAL_625US,
        window: SCAN_WINDOW_625US,
        ..central::ScanConfig::default()
    };
    let window: RefCell<CandidateTable<Address, WINDOW_CANDIDATES>> =
        RefCell::new(CandidateTable::new());
    let last_logged: Cell<Option<(u64, ConnectDecision)>> = Cell::new(None);
    // One advertising report's whole pipeline — parse, decide, log,
    // filter, collect — shared verbatim by both scan phases below;
    // `true` iff the report put a candidate into the window.
    let consider = |report: &nrf_softdevice::raw::ble_gap_evt_adv_report_t| -> bool {
        // Only PDUs we could act on: connectable advertising, not scan
        // responses (passive scanning yields none, but the filter makes
        // the assumption explicit rather than inherited).
        if report.type_.connectable() == 0 || report.type_.scan_response() != 0 {
            return false;
        }
        // SAFETY: the SoftDevice hands us this report synchronously;
        // p_data/len describe its advertising-data buffer, valid for
        // the duration of the callback.
        let data =
            unsafe { core::slice::from_raw_parts(report.data.p_data, report.data.len as usize) };
        let parsed = parse_peer_advertisement(data, &RETICULUM_SVC_UUID_LE);
        if !parsed.offers_service {
            return false;
        }
        let peer = Address::from_raw(report.peer_addr);
        let peer_value = addr_value(&peer.bytes());
        let decision = should_initiate(
            LOCAL_CAPS,
            own_addr_value,
            parsed.caps,
            peer_value,
            scan_mode_now(),
        );
        if last_logged.get() != Some((peer_value, decision)) {
            last_logged.set(Some((peer_value, decision)));
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!(
                    // caps only means something when caps_record=1;
                    // caps_record=0 is a v2.2 peer with no readable
                    // v0.3.0 record — distinct from an explicit
                    // dual-role record advertising caps=0x00.
                    "BLE_SCAN_DECISION addr={:012x} caps_record={} caps={:#04x} rule={} initiate={}",
                    peer_value,
                    u8::from(parsed.caps.is_some()),
                    parsed.caps.unwrap_or(0),
                    decision.as_str(),
                    u8::from(decision.initiate()),
                ),
            );
        }
        // The third filter is the §4.5 exclusion: a dial to an address
        // we already hold a connection with can only time out (see
        // [`addr_already_linked`]), so it never leaves the scanner.
        if !decision.initiate() || skip.contains(peer_value) || addr_already_linked(peer_value) {
            return false;
        }
        window.borrow_mut().offer(peer_value, decision, peer)
    };
    // Phase 1: scan until the first eligible candidate opens the
    // window. A strict pass that never finds one never ends — that is
    // the searching state the fallback clock measures.
    central::scan(sd, &config, |report| consider(report).then_some(())).await?;
    // Phase 2: keep scanning for the window bound, collecting into the
    // table; the callback accepts nothing, so this scan can only end
    // in an error — the timer closing the window is the normal path.
    let collect = central::scan(sd, &config, |report| {
        consider(report);
        None::<()>
    });
    match select(collect, Timer::after_millis(SCAN_WINDOW_COLLECT_MS)).await {
        Either::First(Err(err)) => return Err(err),
        Either::First(Ok(())) | Either::Second(()) => {}
    }
    let window = window.into_inner();
    let seen = window.seen();
    Ok(window
        .into_best()
        .map(|(_, decision, peer)| (peer, decision, seen)))
}

/// One central link, connect to teardown. Returns after logging why it
/// ended; the caller paces the next attempt. `decision` is the verdict
/// that permitted the dial: a FALLBACK target that does not even
/// connect goes into the dead-end table (#375 §0 — before that entry
/// existed, the 5 s retry backoff re-dialled the same dead address
/// every ~20 s forever), while a failed strict dial keeps only the
/// short backoff, because the sort will keep electing that peer and a
/// two-minute hole toward it would cost real links.
async fn central_link(
    sd: &'static Softdevice,
    own_identity: &[u8; 16],
    peer: Address,
    decision: ConnectDecision,
    dead_ends: &mut RecentDeadEnds,
) {
    let peer_value = addr_value(&peer.bytes());
    let whitelist = [&peer];
    let config = central::ConnectConfig {
        scan_config: central::ScanConfig {
            whitelist: Some(&whitelist),
            active: false,
            extended: false,
            interval: SCAN_INTERVAL_625US,
            window: SCAN_WINDOW_625US,
            timeout: CONNECT_TIMEOUT_10MS,
            ..central::ScanConfig::default()
        },
        ..central::ConnectConfig::default()
    };
    let conn = match central::connect(sd, &config).await {
        Ok(conn) => conn,
        Err(err) => {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CENTRAL_FAIL addr={peer_value:012x} stage=connect err={err:?}"),
            );
            if decision == ConnectDecision::InitiateFallback {
                dead_ends.note(peer_value);
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_DIAL_DEAD_END addr={peer_value:012x} reason=fallback_connect ttl_s={}",
                        DEAD_END_TTL.as_secs()
                    ),
                );
            }
            return;
        }
    };

    // v2.2 §Connection Phase, in the spec's order: service discovery
    // (3), read the Identity characteristic (4) — with the checks the
    // registry exists for — then subscribe (5) and handshake (6).
    let client: ReticulumClient = match gatt_client::discover(&conn).await {
        Ok(client) => client,
        Err(err) => {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CENTRAL_FAIL addr={peer_value:012x} stage=discover err={err:?}"),
            );
            let _ = conn.disconnect();
            return;
        }
    };
    let peer_id = match client.identity_read().await {
        Ok(id) => id,
        Err(err) => {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CENTRAL_FAIL addr={peer_value:012x} stage=identity err={err:?}"),
            );
            let _ = conn.disconnect();
            return;
        }
    };
    // Null-hypothesis check first: an unexpected identity may be us
    // from a different angle. The scanner never sees our own PDUs, but
    // a peer could mirror our identity back through a bug, and "linked
    // to myself" must be a loud log line, not a quiet link.
    if peer_id == *own_identity {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!("BLE_LINK_SELF addr={peer_value:012x} action=disconnect"),
        );
        dead_ends.note(peer_value);
        let _ = conn.disconnect();
        return;
    }
    if peer_already_linked(&peer_id) {
        // The rotation hole, closed: same identity, (usually) a fresh
        // address. See [`LIVE_PEERS`].
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_LINK_DUP peer={:02x}{:02x}{:02x}{:02x} addr={:012x} action=disconnect",
                peer_id[0], peer_id[1], peer_id[2], peer_id[3], peer_value
            ),
        );
        dead_ends.note(peer_value);
        let _ = conn.disconnect();
        return;
    }

    // From here the link is real: register it exactly as the peripheral
    // side does — a drain-table claim (whose index is the link identity
    // for the fan-out) plus the identity registry entry. The claimed
    // drain slot never signals on this link (a GATT client sends
    // write-commands, not notifications, so no HVN edges arise), but
    // holding it is what makes "claimed slot" mean "live link".
    let Some(conn_handle) = conn.handle() else {
        crate::warn!("BLE: central connection without a handle, dropping it");
        return;
    };
    let Some((slot_index, _drain)) = HVN_DRAIN.claim_indexed(conn_handle) else {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!(
                "BLE_DRAIN_TABLE_FULL conn={} slots={}",
                conn_handle,
                HVN_DRAIN.capacity()
            ),
        );
        let _ = conn.disconnect();
        return;
    };
    conn_link_up(slot_index, peer_value);
    peer_link_up(slot_index, peer_id);

    run_central_session(
        &conn,
        &client,
        own_identity,
        peer_id,
        slot_index,
        peer_value,
    )
    .await;

    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_CENTRAL_DOWN peer={:02x}{:02x}{:02x}{:02x} slot={}",
            peer_id[0], peer_id[1], peer_id[2], peer_id[3], slot_index
        ),
    );
    peer_link_down(slot_index);
    conn_link_down(slot_index);
    HVN_DRAIN.release(conn_handle);
}

/// Subscribe, handshake, then pump both directions until either side
/// ends the link. The mirror of [`gatt_events`], with the roles
/// swapped: inbound is the peer's TX notifications, outbound writes the
/// peer's RX characteristic.
async fn run_central_session(
    conn: &Connection,
    client: &ReticulumClient,
    own_identity: &[u8; 16],
    peer_id: [u8; 16],
    slot_index: usize,
    peer_value: u64,
) {
    // Subscribe to the peer's TX before announcing ourselves, so
    // nothing the peer sends in response to the handshake can fall into
    // an unsubscribed gap (v2.2 §Connection Phase steps 5 and 6).
    if let Err(err) = client.tx_cccd_write(true).await {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!("BLE_CENTRAL_FAIL addr={peer_value:012x} stage=subscribe err={err:?}"),
        );
        let _ = conn.disconnect();
        return;
    }
    // The identity handshake: our 16-byte identity hash as the first
    // write, which is how the peripheral — who never scans — learns who
    // connected (v2.2 §Identity Handshake Protocol). Written WITH
    // response: the confirmation is the one signal the session may
    // start.
    let handshake = heapless_v8::Vec::from_slice(own_identity).unwrap_or_default();
    if handshake.len() != own_identity.len() {
        // Unreachable (16 <= 251), but a truncated handshake must not
        // go on the air as a valid-looking write.
        return;
    }
    if let Err(err) = client.rx_write(&handshake).await {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!("BLE_CENTRAL_FAIL addr={peer_value:012x} stage=handshake err={err:?}"),
        );
        let _ = conn.disconnect();
        return;
    }
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!(
            "BLE_CENTRAL_UP peer={:02x}{:02x}{:02x}{:02x} slot={} att_mtu={}",
            peer_id[0],
            peer_id[1],
            peer_id[2],
            peer_id[3],
            slot_index,
            conn.att_mtu(),
        ),
    );

    let incoming_tx = BLE_INCOMING.sender();
    let defrag: Cell<BleDefragmenter> = Cell::new(BleDefragmenter::new());
    let last_keepalive: Cell<Instant> = Cell::new(Instant::now());

    // This link's outbound queue; stale packets are the previous
    // tenant's (same claim-time drain as the peripheral side).
    let outgoing_rx = super::link_out(slot_index).receiver();
    while outgoing_rx.try_receive().is_ok() {}

    // Inbound: the peer's TX notifications. We read the peer's identity
    // from its characteristic, so a 16-byte first frame is NOT a
    // handshake here — per v2.2 §Identity Handshake ("only 16-byte
    // packets without an existing identity are treated as handshakes")
    // everything that is not a keepalive is fragment traffic.
    let inbound = gatt_client::run(conn, client, |event| {
        let ReticulumClientEvent::TxNotification(data) = event;
        if data.len() < FRAGMENT_HEADER_SIZE {
            // The peer's 1-byte keepalive; nothing to defragment.
            return;
        }
        let now = Instant::now().as_millis();
        let mut d = defrag.replace(BleDefragmenter::new());
        let result = process_logged(&mut d, &data, now, slot_index);
        defrag.set(d);
        match result {
            DefragResult::Complete(packet) => {
                crate::info!("BLE: RX {}B", packet.len());
                let _ = incoming_tx.try_send((Some(peer_id), packet));
            }
            DefragResult::NeedMore | DefragResult::Error => {}
        }
    });

    // Outbound: fragments as write-without-response. Queue-full
    // flow control lives in `gatt_client::write_without_response`
    // itself — it re-offers after BLE_GATTC_EVT_WRITE_CMD_TX_COMPLETE —
    // so unlike the notify path no drain machinery is needed; every
    // remaining error is fatal for the link.
    let outbound = async {
        // This connection's inter-packet gap state (#376); dies with the
        // link, so a reconnect starts unpaced.
        let mut tx_gap = TxGap::new();
        loop {
            let keepalive_deadline =
                Timer::at(last_keepalive.get() + Duration::from_millis(KEEPALIVE_INTERVAL_MS));
            match select(outgoing_rx.receive(), keepalive_deadline).await {
                Either::First(packet) => {
                    serve_tx_gap(&tx_gap, conn.handle().unwrap_or(u16::MAX)).await;
                    let fragments = ble_framing::fragment_packet(&packet, ble_framing::DEFAULT_MTU);
                    let mut sent = 0usize;
                    let mut torn = false;
                    for (index, fragment) in fragments.iter().enumerate() {
                        let Ok(value) = heapless_v8::Vec::from_slice(fragment.as_slice()) else {
                            // Unreachable: DEFAULT_MTU fragments are
                            // narrower than the 251-byte characteristic.
                            // Reported all the same — this used to be a
                            // bare `return`, i.e. exactly the silent
                            // mid-packet loss #373 hunts.
                            let dropped = BLE_TX_DROPPED
                                .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                                + 1;
                            crate::log::log_fmt(
                                "[BLE ] ",
                                format_args!(
                                    "BLE_TX_DROP kind=packet len={} frag={} of={} sent={} reason=oversize code=0 conn={} dropped={}",
                                    packet.len(),
                                    index,
                                    fragments.len(),
                                    sent,
                                    conn.handle().unwrap_or(u16::MAX),
                                    dropped,
                                ),
                            );
                            torn = true;
                            break;
                        };
                        if let Err(err) = client.rx_write_without_response(&value).await {
                            let dropped = BLE_TX_DROPPED
                                .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
                                + 1;
                            crate::log::log_fmt(
                                "[BLE ] ",
                                format_args!(
                                    "BLE_TX_DROP kind=packet len={} frag={} of={} sent={} reason=write_cmd code=0 dropped={} err={:?}",
                                    packet.len(),
                                    index,
                                    fragments.len(),
                                    sent,
                                    dropped,
                                    err,
                                ),
                            );
                            torn = true;
                            break;
                        }
                        sent += 1;
                    }
                    tx_gap.packet_done(Instant::now().as_millis());
                    // One BLE_TX_PKT per multi-fragment packet, success
                    // and failure alike (#373) — same line, same host
                    // test as the notify path's.
                    if fragments.len() > 1 {
                        crate::log::log_fmt(
                            "[BLE ] ",
                            format_args!(
                                "{}",
                                leviculum_ble_tx::TxPktLine {
                                    conn: conn.handle().unwrap_or(u16::MAX),
                                    len: packet.len(),
                                    frags: fragments.len(),
                                    sent,
                                }
                            ),
                        );
                    }
                    if torn {
                        // A torn head poisons the peer's reassembler
                        // exactly as on the notify path (see
                        // super::notify): the only in-band reset is
                        // dropping the link. After a write error the
                        // link is dead anyway; make it official.
                        let _ = conn.disconnect();
                        return;
                    }
                    BLE_TX_PACKETS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                }
                Either::Second(()) => {
                    let kv = heapless_v8::Vec::from_slice(&[KEEPALIVE_BYTE]).unwrap_or_default();
                    if client.rx_write_without_response(&kv).await.is_err() {
                        return;
                    }
                    last_keepalive.set(Instant::now());
                }
            }
        }
    };

    // Third arm: the runtime carrier switch, as on the peripheral side —
    // disconnect, and let [`central_link`]'s teardown report the loss
    // through the same `peer_link_down` that range loss takes.
    if let Either3::Third(()) = select3(
        inbound,
        outbound,
        super::carrier_off(CarrierWaiter::Session(slot_index)),
    )
    .await
    {
        crate::log::log_fmt(
            "[BLE ] ",
            format_args!("BLE_CARRIER_DROP role=central slot={}", slot_index),
        );
        let _ = conn.disconnect();
    }
}

/// The scanner/initiator task: the peripheral task's counterpart, one
/// central link at a time (Ausbaustufe 1 — `central_role_count` is 1,
/// and the task's sequential scan→connect→session loop is what holds
/// that structurally).
#[embassy_executor::task]
async fn central_task(sd: &'static Softdevice, identity_hash: [u8; 16]) {
    // Our own current address, as the sort compares it. Static random,
    // set by the SoftDevice at enable; it does not rotate, so reading
    // it once is reading it right.
    let own = nrf_softdevice::ble::get_address(sd);
    let own_addr_value = addr_value(&own.bytes());
    crate::log::log_fmt(
        "[BLE ] ",
        format_args!("BLE_CENTRAL_ADDR addr={own_addr_value:012x} caps={LOCAL_CAPS:#04x}"),
    );

    let mut dead_ends = RecentDeadEnds::new();
    loop {
        // The runtime carrier gate, as at the top of the peripheral
        // loop: `ble=off` holds the task here, so a switched-off
        // carrier neither scans nor initiates; off→on falls through
        // and resumes scanning.
        if !crate::media::ble_active() {
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_GATE role=central state=off"),
            );
            super::carrier_on(CarrierWaiter::Central).await;
            crate::log::log_fmt(
                "[BLE ] ",
                format_args!("BLE_CARRIER_GATE role=central state=on"),
            );
        }
        match select(
            find_peer_to_initiate(sd, own_addr_value, &dead_ends),
            super::carrier_off(CarrierWaiter::Central),
        )
        .await
        {
            Either::First(Ok(Some((peer, decision, seen)))) => {
                // A strict verdict is a permitted peer: the strict rule
                // works here, so the fallback clock restarts (#375). A
                // fallback verdict must NOT restart it — if this dial
                // fails, the board is still stranded and the next pass
                // must stay in fallback rather than wait out the bound
                // again.
                if decision != ConnectDecision::InitiateFallback {
                    note_strict_reset();
                }
                // Once per window that ends in a dial: how many
                // eligible advertisers the window held and which one
                // the shared choice elected, under which rule.
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_SCAN_WINDOW seen={} chosen={:012x} rule={}",
                        seen,
                        addr_value(&peer.bytes()),
                        decision.as_str(),
                    ),
                );
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_CENTRAL_CONNECT addr={:012x}",
                        addr_value(&peer.bytes())
                    ),
                );
                central_link(sd, &identity_hash, peer, decision, &mut dead_ends).await;
            }
            // Unreachable in practice — a window only closes after its
            // first candidate — but an empty one is a plain rescan,
            // never a panic.
            Either::First(Ok(None)) => continue,
            Either::First(Err(err)) => {
                crate::warn!("BLE: scan pass failed err={:?}", err);
            }
            // Carrier switched off mid-scan: dropping the scan future
            // stops the scan. Straight back to the gate, no backoff —
            // there is nothing to pace against while off.
            Either::Second(()) => continue,
        }
        Timer::after_millis(CENTRAL_RETRY_BACKOFF_MS).await;
    }
}
