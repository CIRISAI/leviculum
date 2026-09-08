//! Columba BLE interface (`ble-reticulum` protocol v2.2 + v0.3.0) over
//! BlueZ — the PC side of the mesh the LNode firmware and Columba phones
//! already speak.
//!
//! One configured `BLEInterface` section is **one Reticulum interface and
//! one broadcast domain**: every outbound packet fans out to all live BLE
//! links, every live link's inbound packets feed the same interface. The
//! interface runs both GATT roles at once — it advertises + serves the
//! Columba GATT layout (peripheral) and scans + dials peers the shared
//! connection-direction rule tells it to (central).
//!
//! # Layering
//!
//! - [`links`] is the pure half: admission, per-link framing state,
//!   keepalive/expiry policy, fan-out planning. Host-tested, no BlueZ.
//! - [`bluez`] is the carrier binding: bluer (the official BlueZ Rust
//!   binding, over D-Bus) performs advertising, the GATT server, scanning
//!   and the central-role connections, and reports everything as
//!   [`Ev`] events into the orchestrator task below.
//! - The orchestrator owns the [`links::LinkTable`] and the two mpsc
//!   channels of the [`InterfaceHandle`] contract; it is the only writer
//!   of interface state, so there is no lock.
//!
//! Wire logic is reused, not restated: fragmentation from
//! `leviculum_core::framing::ble`, advertisement parsing and the
//! connection decision from `leviculum-ble-tx` (the firmware's own
//! host-tested crate), both driven from [`links`].

pub(crate) mod bluez;
pub(crate) mod links;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use bluer::gatt::local::CharacteristicNotifier;
use bluer::Address;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use super::{
    IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket, PeerEvent,
    ReadySignal,
};
use leviculum_core::traits::{InterfaceKind, InterfaceMode};
use leviculum_core::transport::InterfaceId;
use links::{Addr, Admission, IdentityHash, Inbound, LinkTable, Role};

/// Outbound channel depth, matching the other interfaces' default.
const BLE_BUFFER_SIZE: usize = 256;

/// Per-central-link fragment channel depth. A full channel drops the
/// packet on that link (counted + logged), it never blocks the domain.
const LINK_QUEUE_DEPTH: usize = 32;

/// Reconnect/backoff after a failed or ended central session, the
/// firmware's `SESSION_BACKOFF`.
const SESSION_BACKOFF_MS: u64 = 5_000;

/// How long an address that presented a duplicate or our own identity is
/// not redialled, the firmware's `DUPLICATE_ADDR_TTL`.
const DUPLICATE_ADDR_TTL_MS: u64 = 120_000;

/// Delay before retrying BlueZ session/adapter setup after a failure.
const ADAPTER_RETRY: Duration = Duration::from_secs(10);

/// The reference's `BITRATE_GUESS` for this medium (`ble-reticulum@07d94130` `BLEInterface.py`, `BITRATE_GUESS`).
const BLE_BITRATE_GUESS: u32 = 700_000;

/// Everything the builder resolves from the config section.
#[derive(Debug, Clone)]
pub(crate) struct BleOptions {
    /// BlueZ adapter name (`hci0`); `None` = the default adapter.
    pub adapter: Option<String>,
    /// Simultaneous link cap, both roles counted together.
    pub max_connections: usize,
    /// Sightings weaker than this are not dialled (reference `min_rssi`).
    pub min_rssi: i16,
    /// Pause between scan windows (reference `discovery_interval`).
    pub discovery_interval: Duration,
    /// Run the scanning + dialling half.
    pub enable_central: bool,
    /// Run the advertising + GATT-server half.
    pub enable_peripheral: bool,
}

impl Default for BleOptions {
    fn default() -> Self {
        Self {
            adapter: None,
            max_connections: links::DEFAULT_MAX_LINKS,
            min_rssi: -85,
            discovery_interval: Duration::from_secs(5),
            enable_central: true,
            enable_peripheral: true,
        }
    }
}

/// Everything the carrier tasks report back to the orchestrator.
pub(crate) enum Ev {
    /// A central wrote to our RX characteristic.
    PeriphWrite {
        addr: Address,
        mtu: usize,
        data: Vec<u8>,
    },
    /// A central subscribed to our TX characteristic.
    PeriphNotify(CharacteristicNotifier),
    /// One scanner sighting of a device offering (or not) the service.
    Scan {
        addr: Address,
        rssi: Option<i16>,
        offers_service: bool,
        /// Payload of the CID 0xFFFF manufacturer record, if present.
        record: Option<Vec<u8>>,
    },
    /// A central-role connection read the peer's identity and asks to be
    /// admitted before it handshakes. `true` on `ack` = proceed.
    CentralIdentity {
        addr: Address,
        identity: IdentityHash,
        mtu: usize,
        frames: mpsc::Sender<Vec<u8>>,
        ack: oneshot::Sender<bool>,
    },
    /// A notification arrived on a central-role link.
    CentralFrame { addr: Address, data: Vec<u8> },
    /// A central-role task ended (connect failure, session end, or the
    /// admission said no). Always the task's last event.
    CentralGone { addr: Address },
}

/// Spawn the Columba BLE interface. Never fails at build time: BlueZ
/// availability is a runtime concern (the adapter can appear, power up,
/// or restart while lnsd runs), handled by the task's retry loop.
pub(crate) fn spawn_ble_interface(
    id: InterfaceId,
    name: String,
    opts: BleOptions,
    identity_hash: IdentityHash,
    peer_event_tx: mpsc::Sender<(InterfaceId, PeerEvent)>,
) -> InterfaceHandle {
    let (incoming_tx, incoming_rx) = mpsc::channel(BLE_BUFFER_SIZE);
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<OutgoingPacket>(BLE_BUFFER_SIZE);
    let counters = Arc::new(InterfaceCounters::new());
    let ready = ReadySignal::new();

    let info = InterfaceInfo {
        id,
        name: name.clone(),
        // Reassembled packets carry the base protocol MTU; no link-MTU
        // upgrade is signalled (the reference's HW_MTU is the same 500).
        hw_mtu: None,
        is_local_client: false,
        bitrate: Some(BLE_BITRATE_GUESS),
        tx_jitter_max_ms: None,
        ifac: None,
        mode: InterfaceMode::default(),
        kind: InterfaceKind::Ble,
        ingress_control: None,
    };

    let task = BleTask {
        id,
        name,
        opts,
        identity_hash,
        incoming_tx,
        peer_event_tx,
        counters: Arc::clone(&counters),
        ready: Arc::clone(&ready),
    };
    tokio::spawn(task.run(outgoing_rx));

    InterfaceHandle {
        info,
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
        ready,
    }
}

struct BleTask {
    id: InterfaceId,
    name: String,
    opts: BleOptions,
    identity_hash: IdentityHash,
    incoming_tx: mpsc::Sender<IncomingPacket>,
    /// Peer transitions toward the driver loop (Codeberg #365): `Lost`
    /// when an identity's LAST link on this broadcast domain died (the
    /// loop culls the paths via that peer), `Up` when an identity
    /// gained its FIRST link (the loop counts it into the peer mirror
    /// and runs the opt-in peer-up pull).
    /// One ordered channel for both — see [`PeerEvent`].
    peer_event_tx: mpsc::Sender<(InterfaceId, PeerEvent)>,
    counters: Arc<InterfaceCounters>,
    ready: Arc<ReadySignal>,
}

/// Orchestrator-side state for one central-role link's TX pipe.
struct CentralPipe {
    frames: mpsc::Sender<Vec<u8>>,
}

impl BleTask {
    async fn run(self, mut outgoing_rx: mpsc::Receiver<OutgoingPacket>) {
        loop {
            match self.session(&mut outgoing_rx).await {
                Ok(()) => return, // interface detached
                Err(e) => {
                    tracing::warn!(
                        "BLE {}: BlueZ session failed ({e}); retrying in {:?}",
                        self.name,
                        ADAPTER_RETRY
                    );
                    tokio::time::sleep(ADAPTER_RETRY).await;
                }
            }
        }
    }

    /// One BlueZ session: set up both roles, then run the event loop
    /// until the daemon drops the interface (`Ok`) or the adapter goes
    /// away (`Err` → retried by [`run`]).
    async fn session(&self, outgoing_rx: &mut mpsc::Receiver<OutgoingPacket>) -> bluer::Result<()> {
        let session = bluer::Session::new().await?;
        let adapter = match &self.opts.adapter {
            Some(name) => session.adapter(name)?,
            None => session.default_adapter().await?,
        };
        adapter.set_powered(true).await?;
        let local_addr: Addr = adapter.address().await?.0;

        let (ev_tx, mut ev_rx) = mpsc::channel::<Ev>(64);

        // Peripheral half: advertisement + GATT application. The handles
        // deregister on drop, so they live for the session.
        let _periph = if self.opts.enable_peripheral {
            Some(
                bluez::start_peripheral(&adapter, self.identity_hash, ev_tx.clone(), &self.name)
                    .await?,
            )
        } else {
            None
        };

        // Central half: windowed scanning.
        let scan_guard = if self.opts.enable_central {
            let scan = bluez::ScanTask {
                adapter: adapter.clone(),
                ev_tx: ev_tx.clone(),
                interval: self.opts.discovery_interval,
                iface: self.name.clone(),
            };
            Some(tokio::spawn(scan.run()))
        } else {
            None
        };
        // Abort the scanner with the session, not at some later drop.
        let _scan_abort = scan_guard.map(AbortOnDrop);

        tracing::info!(
            "BLE {}: up on {} addr {} name LN-… (peripheral={}, central={}, max_links={})",
            self.name,
            adapter.name(),
            Address(local_addr),
            self.opts.enable_peripheral,
            self.opts.enable_central,
            self.opts.max_connections,
        );
        self.ready.signal_ready();

        let start = Instant::now();
        let now_ms = |i: Instant| i.duration_since(start).as_millis() as u64;

        let mut table = LinkTable::new(self.identity_hash, self.opts.max_connections);
        let mut notifiers: Vec<CharacteristicNotifier> = Vec::new();
        let mut central_pipes: HashMap<Addr, CentralPipe> = HashMap::new();
        let mut dialling: HashSet<Addr> = HashSet::new();
        let mut backoff_until: HashMap<Addr, u64> = HashMap::new();
        // BLE_SCAN_DECISION is logged once per (address, decision) change,
        // not per sighting — a waiting peer advertises several times a
        // second (same rule as the firmware).
        let mut last_decision: HashMap<Addr, links::ScanDecision> = HashMap::new();

        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                outgoing = outgoing_rx.recv() => {
                    let Some(packet) = outgoing else {
                        // Daemon dropped the handle: interface detached.
                        return Ok(());
                    };
                    self.send_packet(&table, &central_pipes, &mut notifiers, &packet.data).await;
                }
                ev = ev_rx.recv() => {
                    let Some(ev) = ev else { return Ok(()) };
                    self.handle_event(
                        ev,
                        &adapter,
                        &local_addr,
                        &mut table,
                        &mut notifiers,
                        &mut central_pipes,
                        &mut dialling,
                        &mut backoff_until,
                        &mut last_decision,
                        &ev_tx,
                        now_ms(Instant::now()),
                    )
                    .await;
                }
                _ = tick.tick() => {
                    let now = now_ms(Instant::now());
                    notifiers.retain(|n| !n.is_stopped());
                    let ka = table.keepalives_due(now);
                    if ka.notify {
                        send_via_notifiers(
                            &mut notifiers,
                            vec![leviculum_core::framing::ble::KEEPALIVE_BYTE],
                        )
                        .await;
                    }
                    for addr in ka.central {
                        if let Some(pipe) = central_pipes.get(&addr) {
                            let _ = pipe
                                .frames
                                .try_send(vec![leviculum_core::framing::ble::KEEPALIVE_BYTE]);
                        }
                    }
                    let expired = table.expire(now);
                    for (identity, addr, role) in expired.links {
                        self.log_link_down(&identity, role, "timeout");
                        central_pipes.remove(&addr);
                        disconnect_quietly(&adapter, addr).await;
                        backoff_until.insert(addr, now + SESSION_BACKOFF_MS);
                        self.report_peer_lost(&table, identity).await;
                    }
                    for addr in expired.pending {
                        tracing::debug!(
                            "BLE {}: disconnecting {} — no identity handshake within timeout",
                            self.name, Address(addr)
                        );
                        disconnect_quietly(&adapter, addr).await;
                    }
                    backoff_until.retain(|_, until| *until > now);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_event(
        &self,
        ev: Ev,
        adapter: &bluer::Adapter,
        local_addr: &Addr,
        table: &mut LinkTable,
        notifiers: &mut Vec<CharacteristicNotifier>,
        central_pipes: &mut HashMap<Addr, CentralPipe>,
        dialling: &mut HashSet<Addr>,
        backoff_until: &mut HashMap<Addr, u64>,
        last_decision: &mut HashMap<Addr, links::ScanDecision>,
        ev_tx: &mpsc::Sender<Ev>,
        now: u64,
    ) {
        match ev {
            Ev::PeriphNotify(notifier) => {
                notifiers.push(notifier);
            }
            Ev::PeriphWrite { addr, mtu, data } => {
                self.counters
                    .rx_bytes
                    .fetch_add(data.len() as u64, Ordering::Relaxed);
                match table.peripheral_frame(addr.0, mtu, &data, now) {
                    Inbound::Packet(packet) => self.deliver(packet).await,
                    Inbound::HandshakeComplete {
                        identity,
                        displaced,
                    } => {
                        let first_link = displaced.is_none();
                        if let Some((identity, old_addr, role)) = displaced {
                            self.log_link_down(&identity, role, "displaced");
                            central_pipes.remove(&old_addr);
                            disconnect_quietly(adapter, old_addr).await;
                        }
                        self.log_link_up(&identity, &addr.0, Role::Peripheral, mtu);
                        if first_link {
                            self.report_peer_up(identity).await;
                        }
                    }
                    Inbound::HandshakeRejected(admission) => {
                        self.log_rejection(admission, &data, &addr.0);
                        backoff_until.insert(addr.0, now + DUPLICATE_ADDR_TTL_MS);
                        disconnect_quietly(adapter, addr.0).await;
                    }
                    Inbound::Error => {
                        tracing::debug!("BLE {}: bad fragment from {} dropped", self.name, addr);
                    }
                    Inbound::Keepalive | Inbound::NeedMore | Inbound::NotHandshaked => {}
                }
                self.log_abandon_reports(table);
            }
            Ev::Scan {
                addr,
                rssi,
                offers_service,
                record,
            } => {
                // A sighting with no RSSI is a BlueZ cache entry, not a
                // device on the air right now.
                let Some(rssi) = rssi else { return };
                let Some(decision) =
                    links::decide_from_scan(local_addr, &addr.0, offers_service, record.as_deref())
                else {
                    return;
                };
                if last_decision.insert(addr.0, decision) != Some(decision) {
                    tracing::info!(
                        event = "BLE_SCAN_DECISION",
                        addr = %hex12(&addr.0),
                        caps = %format_args!("{:#04x}", decision.caps),
                        caps_record = u8::from(decision.caps_record),
                        initiate = u8::from(decision.decision.initiate()),
                        rule = decision.decision.as_str(),
                    );
                }
                if !decision.decision.initiate()
                    || rssi < self.opts.min_rssi
                    || table.is_full()
                    || table.knows_addr(&addr.0)
                    || dialling.contains(&addr.0)
                    || backoff_until.get(&addr.0).is_some_and(|until| *until > now)
                {
                    return;
                }
                dialling.insert(addr.0);
                let central = bluez::CentralTask {
                    adapter: adapter.clone(),
                    addr,
                    own_identity: self.identity_hash,
                    ev_tx: ev_tx.clone(),
                    iface: self.name.clone(),
                };
                tokio::spawn(central.run());
            }
            Ev::CentralIdentity {
                addr,
                identity,
                mtu,
                frames,
                ack,
            } => {
                let (admission, displaced) = table.admit(identity, addr.0, Role::Central, mtu, now);
                match admission {
                    Admission::Accept => {
                        let first_link = displaced.is_none();
                        if let Some((identity, old_addr, role)) = displaced {
                            self.log_link_down(&identity, role, "displaced");
                            central_pipes.remove(&old_addr);
                            disconnect_quietly(adapter, old_addr).await;
                        }
                        central_pipes.insert(addr.0, CentralPipe { frames });
                        self.log_link_up(&identity, &addr.0, Role::Central, mtu);
                        if first_link {
                            self.report_peer_up(identity).await;
                        }
                        let _ = ack.send(true);
                    }
                    other => {
                        self.log_rejection(other, &identity, &addr.0);
                        backoff_until.insert(
                            addr.0,
                            now + match other {
                                Admission::RejectFull => SESSION_BACKOFF_MS,
                                _ => DUPLICATE_ADDR_TTL_MS,
                            },
                        );
                        let _ = ack.send(false);
                    }
                }
            }
            Ev::CentralFrame { addr, data } => {
                self.counters
                    .rx_bytes
                    .fetch_add(data.len() as u64, Ordering::Relaxed);
                match table.central_frame(addr.0, &data, now) {
                    Inbound::Packet(packet) => self.deliver(packet).await,
                    Inbound::Error => {
                        tracing::debug!("BLE {}: bad fragment from {} dropped", self.name, addr);
                    }
                    _ => {}
                }
                self.log_abandon_reports(table);
            }
            Ev::CentralGone { addr } => {
                dialling.remove(&addr.0);
                central_pipes.remove(&addr.0);
                if let Some((identity, _, role)) = table.remove_by_addr(&addr.0) {
                    self.log_link_down(&identity, role, "disconnected");
                    self.report_peer_lost(table, identity).await;
                }
                let until = backoff_until.entry(addr.0).or_insert(0);
                *until = (*until).max(now + SESSION_BACKOFF_MS);
            }
        }
    }

    /// Fan one Reticulum packet out to every live link.
    async fn send_packet(
        &self,
        table: &LinkTable,
        central_pipes: &HashMap<Addr, CentralPipe>,
        notifiers: &mut Vec<CharacteristicNotifier>,
        packet: &[u8],
    ) {
        let plan = table.plan_tx(packet);
        let mut delivered = false;
        if !plan.notify_fragments.is_empty() {
            let mut ok = true;
            for frag in plan.notify_fragments {
                if !send_via_notifiers(notifiers, frag).await {
                    ok = false;
                    break;
                }
            }
            delivered |= ok;
        }
        for (addr, frags) in plan.central {
            let Some(pipe) = central_pipes.get(&addr) else {
                continue;
            };
            let depth = frags.len();
            let mut ok = true;
            for frag in frags {
                if pipe.frames.try_send(frag).is_err() {
                    ok = false;
                    break;
                }
            }
            if ok {
                delivered = true;
            } else {
                // A partially queued packet tears the peer's reassembly;
                // the periodic keepalive/expiry keeps the link honest and
                // the drop is counted, never silent.
                self.counters.tx_queue_drops.fetch_add(1, Ordering::Relaxed);
                self.counters
                    .tx_dropped_bytes
                    .fetch_add(packet.len() as u64, Ordering::Relaxed);
                let peer = table
                    .link_by_addr(&addr)
                    .map(|l| hex8(&l.identity))
                    .unwrap_or_default();
                tracing::warn!(
                    event = "BLE_TX_FANOUT_DROP",
                    iface = %self.name,
                    peer = %peer,
                    len = packet.len(),
                    depth = depth,
                );
            }
        }
        if delivered {
            self.counters
                .tx_bytes
                .fetch_add(packet.len() as u64, Ordering::Relaxed);
        }
    }

    async fn deliver(&self, packet: Vec<u8>) {
        let _ = self.incoming_tx.send(IncomingPacket { data: packet }).await;
    }

    /// Report a peer loss to the driver loop, unless the identity still
    /// owns another live link — `LinkTable::knows_identity` is the
    /// single decision point (Codeberg #365). Called at the timeout and
    /// disconnect removal sites; the displacement sites are exempt by
    /// construction (a displaced link is replaced by a live link with
    /// the SAME identity, so the peer was never lost).
    async fn report_peer_lost(&self, table: &LinkTable, identity: IdentityHash) {
        if table.knows_identity(&identity) {
            return;
        }
        tracing::info!(
            event = "BLE_PEER_LOST",
            iface = %self.name,
            peer = %hex8(&identity),
        );
        let _ = self
            .peer_event_tx
            .send((self.id, PeerEvent::Lost(identity)))
            .await;
    }

    /// Report a peer's arrival to the driver loop (Codeberg #365) — the
    /// mirror of [`report_peer_lost`](Self::report_peer_lost). Called at
    /// the two link-up sites (peripheral handshake completion, central
    /// admission) exactly when the admission carried no displacement:
    /// `admit` displaces same-identity links only, so `displaced == None`
    /// on an Accept means the identity gained its FIRST link — the moment
    /// `knows_identity` starts returning true. A displacement relink (the
    /// phone's ~60 s random-address rotation) is churn, not an arrival,
    /// and must not spray pull requests.
    async fn report_peer_up(&self, identity: IdentityHash) {
        tracing::info!(
            event = "BLE_PEER_UP",
            iface = %self.name,
            peer = %hex8(&identity),
        );
        let _ = self
            .peer_event_tx
            .send((self.id, PeerEvent::Up(identity)))
            .await;
    }

    /// One `BLE_RX_ABANDON` line per reassembly this receiver discarded
    /// before completion (#373): a torn or interleaved fragment stream
    /// from the peer cost `lost=` whole Reticulum packets; `total=` is
    /// the link's running count. Mirrors the firmware's line of the
    /// same name (slot-keyed there, identity-keyed here), so a merged
    /// bench timeline shows the receiver side of a `BLE_TX_PKT
    /// sent=<all>` that never became a delivery.
    fn log_abandon_reports(&self, table: &mut LinkTable) {
        for (identity, lost, total) in table.take_abandon_reports() {
            tracing::warn!(
                event = "BLE_RX_ABANDON",
                iface = %self.name,
                peer = %hex8(&identity),
                lost = lost,
                total = total,
            );
        }
    }

    fn log_link_up(&self, identity: &IdentityHash, addr: &Addr, role: Role, mtu: usize) {
        tracing::info!(
            event = "BLE_LINK_UP",
            iface = %self.name,
            peer = %hex8(identity),
            addr = %hex12(addr),
            role = role.as_str(),
            mtu = mtu,
        );
    }

    fn log_link_down(&self, identity: &IdentityHash, role: Role, reason: &'static str) {
        tracing::info!(
            event = "BLE_LINK_DOWN",
            iface = %self.name,
            peer = %hex8(identity),
            role = role.as_str(),
            reason = reason,
        );
    }

    fn log_rejection(&self, admission: Admission, identity: &[u8], addr: &Addr) {
        match admission {
            Admission::RejectSelf => tracing::warn!(
                event = "BLE_LINK_SELF",
                addr = %hex12(addr),
                action = "disconnect",
            ),
            Admission::RejectDuplicate => tracing::info!(
                event = "BLE_LINK_DUP",
                peer = %hex8(identity),
                addr = %hex12(addr),
                action = "disconnect",
            ),
            Admission::RejectFull => tracing::info!(
                "BLE {}: link limit reached, rejecting {}",
                self.name,
                hex12(addr)
            ),
            Admission::Accept => {}
        }
    }
}

/// Emit one frame as a GATT notification. BlueZ fans a notification out
/// to every subscribed central, so exactly one live notifier is used per
/// frame — calling more than one would duplicate the frame at every
/// subscriber.
async fn send_via_notifiers(notifiers: &mut Vec<CharacteristicNotifier>, frame: Vec<u8>) -> bool {
    notifiers.retain(|n| !n.is_stopped());
    for notifier in notifiers.iter_mut() {
        if notifier.notify(frame.clone()).await.is_ok() {
            return true;
        }
    }
    false
}

async fn disconnect_quietly(adapter: &bluer::Adapter, addr: Addr) {
    if let Ok(device) = adapter.device(Address(addr)) {
        let _ = device.disconnect().await;
    }
}

/// `peer=` value: leading 4 identity-hash bytes as hex, the same hex the
/// firmware logs and the LN-<hex8> name carries.
fn hex8(identity: &[u8]) -> String {
    identity
        .iter()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `addr=` value: the 48-bit address as 12 hex digits in display order,
/// matching the firmware's `addr={:012x}` (which prints the numeric
/// value, i.e. the displayed byte order).
fn hex12(addr: &Addr) -> String {
    addr.iter().map(|b| format!("{b:02x}")).collect()
}

/// Aborts a spawned task when dropped, tying its lifetime to the session.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
