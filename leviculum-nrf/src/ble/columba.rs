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
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::channel::Sender;
use embassy_time::{Duration, Instant, Timer};
use leviculum_ble_tx::{
    addr_value, device_name, manufacturer_data, parse_peer_advertisement, should_initiate,
    ConnectDecision, ADV_BYTES_USED, CAP_PERIPHERAL_ONLY, LEGACY_AD_CAPACITY,
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
use nrf_softdevice::ble::{central, gatt_client, gatt_server, peripheral, Address, Connection};
use nrf_softdevice::Softdevice;
use static_cell::StaticCell;

use super::notify::{notify_fragments, BLE_TX_DRAIN_UNROUTED, BLE_TX_DROPPED, BLE_TX_PACKETS};
use super::{BLE_INCOMING, HVN_DRAIN, MAX_LINKS};

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
    // Phase B (#255): the central half — scan, decide, initiate. Both
    // halves of the protocol spawn here, behind the one entry point the
    // neutral module calls.
    spawner.must_spawn(central_task(sd, identity_hash));
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

    // Per-connection state. `Cell` lets the closure inside
    // `gatt_server::run` mutate handshake_done while the outbound branch
    // reads it. Single-task context (the executor polls one future at a
    // time), so no Mutex is needed.
    let defrag: Cell<BleDefragmenter> = Cell::new(BleDefragmenter::new());
    let handshake_done: Cell<bool> = Cell::new(false);
    let last_keepalive: Cell<Instant> = Cell::new(Instant::now());

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

    // Registry entry first, then the slot: a slot that reads free while
    // the identity still reads linked would refuse a legitimate
    // reconnect in the central task's duplicate check.
    peer_link_down(slot_index);
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
static LIVE_PEERS: BlockingMutex<CriticalSectionRawMutex, RefCell<[Option<[u8; 16]>; MAX_LINKS]>> =
    BlockingMutex::new(RefCell::new([None; MAX_LINKS]));

fn peer_link_up(slot_index: usize, peer_id: [u8; 16]) {
    LIVE_PEERS.lock(|peers| peers.borrow_mut()[slot_index] = Some(peer_id));
}

fn peer_link_down(slot_index: usize) {
    LIVE_PEERS.lock(|peers| peers.borrow_mut()[slot_index] = None);
}

fn peer_already_linked(peer_id: &[u8; 16]) -> bool {
    LIVE_PEERS.lock(|peers| peers.borrow().iter().flatten().any(|id| id == peer_id))
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
/// with our own advertising and up to two live connections; 30 %
/// leaves the SoftDevice scheduler room for both.
const SCAN_INTERVAL_625US: u32 = 160;
const SCAN_WINDOW_625US: u32 = 48;

/// Bound on one connect attempt, in 10 ms units (5 s). The peer
/// advertised moments ago; if it does not answer a CONNECT_IND within
/// seconds it is gone or busy, and the scanner should look again.
const CONNECT_TIMEOUT_10MS: u16 = 500;

/// Pause after a session ended or an attempt failed, before the next
/// scan pass. Keeps a refusing/vanishing peer from being hammered.
const CENTRAL_RETRY_BACKOFF_MS: u64 = 5_000;

/// How long an address that turned out to carry an already-linked
/// identity is skipped. Sized to the RPA rotation timescale (minutes):
/// the address dies on its own at the peer's next rotation, this just
/// stops us re-connecting to it every scan pass until then.
const DUPLICATE_ADDR_TTL: Duration = Duration::from_secs(120);

/// The addresses [`DUPLICATE_ADDR_TTL`] talks about. Small and flat:
/// at most one phone and one neighbour are linked, so duplicates are
/// rare; the table only has to bridge one rotation interval.
struct RecentDuplicates {
    entries: [Option<(u64, Instant)>; MAX_LINKS],
}

impl RecentDuplicates {
    const fn new() -> Self {
        Self {
            entries: [None; MAX_LINKS],
        }
    }

    fn note(&mut self, addr: u64) {
        let now = Instant::now();
        // Reuse an expired (or the oldest) entry.
        let slot = self
            .entries
            .iter()
            .position(|e| e.is_none_or(|(_, at)| now - at >= DUPLICATE_ADDR_TTL))
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
            .any(|&(a, at)| a == addr && now - at < DUPLICATE_ADDR_TTL)
    }
}

/// Scan until one advertisement wins an initiate decision, and return
/// that peer's current address.
///
/// Every connectable PDU carrying the Reticulum service UUID gets a
/// [`should_initiate`] verdict from the v2.2 sort + v0.3.0 override
/// (the rule lives host-tested in [`leviculum_ble_tx::peer`]); the
/// verdict is logged once per (address, rule) change rather than per
/// PDU, because a waiting peer re-advertises several times a second.
async fn find_peer_to_initiate(
    sd: &'static Softdevice,
    own_addr_value: u64,
    skip: &RecentDuplicates,
) -> Result<Address, central::ScanError> {
    let config = central::ScanConfig {
        active: false,
        extended: false,
        interval: SCAN_INTERVAL_625US,
        window: SCAN_WINDOW_625US,
        ..central::ScanConfig::default()
    };
    let last_logged: Cell<Option<(u64, ConnectDecision)>> = Cell::new(None);
    central::scan(sd, &config, |report| {
        // Only PDUs we could act on: connectable advertising, not scan
        // responses (passive scanning yields none, but the filter makes
        // the assumption explicit rather than inherited).
        if report.type_.connectable() == 0 || report.type_.scan_response() != 0 {
            return None;
        }
        // SAFETY: the SoftDevice hands us this report synchronously;
        // p_data/len describe its advertising-data buffer, valid for
        // the duration of the callback.
        let data = unsafe { core::slice::from_raw_parts(report.data.p_data, report.data.len as usize) };
        let parsed = parse_peer_advertisement(data, &RETICULUM_SVC_UUID_LE);
        if !parsed.offers_service {
            return None;
        }
        let peer = Address::from_raw(report.peer_addr);
        let peer_value = addr_value(&peer.bytes());
        let decision = should_initiate(LOCAL_CAPS, own_addr_value, parsed.caps, peer_value);
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
        if !decision.initiate() || skip.contains(peer_value) {
            return None;
        }
        Some(peer)
    })
    .await
}

/// One central link, connect to teardown. Returns after logging why it
/// ended; the caller paces the next attempt.
async fn central_link(
    sd: &'static Softdevice,
    own_identity: &[u8; 16],
    peer: Address,
    duplicates: &mut RecentDuplicates,
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
        duplicates.note(peer_value);
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
        duplicates.note(peer_value);
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
        let result = d.process(&data, now);
        defrag.set(d);
        match result {
            DefragResult::Complete(packet) => {
                crate::info!("BLE: RX {}B", packet.len());
                let _ = incoming_tx.try_send(packet);
            }
            DefragResult::NeedMore => {}
            DefragResult::Error => {
                let mut d = defrag.replace(BleDefragmenter::new());
                d.reset();
                defrag.set(d);
            }
        }
    });

    // Outbound: fragments as write-without-response. Queue-full
    // flow control lives in `gatt_client::write_without_response`
    // itself — it re-offers after BLE_GATTC_EVT_WRITE_CMD_TX_COMPLETE —
    // so unlike the notify path no drain machinery is needed; every
    // remaining error is fatal for the link.
    let outbound = async {
        loop {
            let keepalive_deadline =
                Timer::at(last_keepalive.get() + Duration::from_millis(KEEPALIVE_INTERVAL_MS));
            match select(outgoing_rx.receive(), keepalive_deadline).await {
                Either::First(packet) => {
                    let fragments = ble_framing::fragment_packet(&packet, ble_framing::DEFAULT_MTU);
                    for (index, fragment) in fragments.iter().enumerate() {
                        let Ok(value) = heapless_v8::Vec::from_slice(fragment.as_slice()) else {
                            // Unreachable: DEFAULT_MTU fragments are
                            // narrower than the 251-byte characteristic.
                            return;
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
                                    index,
                                    dropped,
                                    err,
                                ),
                            );
                            // A torn head poisons the peer's reassembler
                            // exactly as on the notify path (see
                            // super::notify): the only in-band reset is
                            // dropping the link. After a write error the
                            // link is dead anyway; make it official.
                            let _ = conn.disconnect();
                            return;
                        }
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

    let _ = select(inbound, outbound).await;
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

    let mut duplicates = RecentDuplicates::new();
    loop {
        match find_peer_to_initiate(sd, own_addr_value, &duplicates).await {
            Ok(peer) => {
                crate::log::log_fmt(
                    "[BLE ] ",
                    format_args!(
                        "BLE_CENTRAL_CONNECT addr={:012x}",
                        addr_value(&peer.bytes())
                    ),
                );
                central_link(sd, &identity_hash, peer, &mut duplicates).await;
            }
            Err(err) => {
                crate::warn!("BLE: scan pass failed err={:?}", err);
            }
        }
        Timer::after_millis(CENTRAL_RETRY_BACKOFF_MS).await;
    }
}
