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
pub(crate) mod census;
pub(crate) mod links;

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use bluer::gatt::local::CharacteristicNotifier;
use bluer::Address;
use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use super::{
    IncomingPacket, InterfaceCounters, InterfaceHandle, InterfaceInfo, OutgoingPacket, PeerEvent,
    ReadySignal,
};
use crate::event_log::Scalar;
use leviculum_ble_tx::{dial_preference, AddrType, DialBackoff, DialFailure};
use leviculum_core::traits::{InterfaceKind, InterfaceMode};
use leviculum_core::transport::InterfaceId;
use links::{
    Addr, Admission, DialQueue, Displaced, IdentityHash, Inbound, LinkTable, Role, ScanScheduler,
};

/// Outbound channel depth, matching the other interfaces' default.
const BLE_BUFFER_SIZE: usize = 256;

/// Per-central-link packet-unit channel depth (one message = one
/// packet's fragments, #376). A full channel drops the packet on that
/// link (counted + logged), it never blocks the domain.
const LINK_QUEUE_DEPTH: usize = 32;

/// Reconnect/backoff after a failed or ended central session, the
/// firmware's `SESSION_BACKOFF`. A failure that names the peer as no
/// Columba peer books more, from the shared [`DialBackoff`] (579).
const SESSION_BACKOFF_MS: u64 = leviculum_ble_tx::DIAL_BACKOFF_BASE_MS;

/// Room in the host's [`DialBackoff`]: a PC sits in rooms with many
/// more advertisers than a board's eight slots cover, and an evicted
/// address only loses its run, so this is generous, not exact.
const DIAL_BACKOFF_HOST_SLOTS: usize = 64;

/// How long an address that presented a duplicate or our own identity is
/// not redialled, the firmware's `DEAD_END_TTL`.
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
    /// Peers we may DIAL (`initiate_only`). Empty — the default — is
    /// every peer, exactly as before the key existed. Consulted on the
    /// scan path only: it never reaches admission, so a peer left off
    /// it that dials us is served like any other.
    pub initiate_only: links::PeerAllowlist,
    /// Peers whose incoming link we SERVE (`accept_only`). Empty — the
    /// default — serves every peer that dials us, exactly as before the
    /// key existed. Handed to the [`LinkTable`], which asks it at the
    /// identity handshake; it never reaches the scan path, so a peer
    /// left off it is still dialled if `initiate_only` allows.
    pub accept_only: links::PeerAllowlist,
    /// Inter-packet gap override in ms (`tx_gap_ms`, #379); `None` —
    /// the default — is the firmware's compiled gap. Every pacer of
    /// the interface, the notify pipe's and each central link's, reads
    /// it.
    pub tx_gap_ms: Option<u16>,
    /// Cap on the ATT MTU this interface sizes its outbound fragments
    /// by (`fragment_mtu`, #379); `None` — the default — is the MTU the
    /// link exchanged. A measurement knob: BlueZ to BlueZ exchanges
    /// 512, at which no Reticulum packet needs a second fragment, and
    /// the gap floor is a question about packets that do.
    pub fragment_mtu: Option<usize>,
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
            initiate_only: links::PeerAllowlist::default(),
            accept_only: links::PeerAllowlist::default(),
            tx_gap_ms: None,
            fragment_mtu: None,
        }
    }
}

/// Everything the carrier tasks report back to the orchestrator.
pub(crate) enum Ev {
    /// A central wrote to our RX characteristic. One central's writes
    /// arrive in order: they come from its own acquired socket, read by
    /// one task (`bluez::periph_reader`, #436).
    PeriphWrite {
        addr: Address,
        mtu: usize,
        data: Vec<u8>,
    },
    /// A central's write socket closed: BlueZ dropped it because the
    /// central disconnected. Always that reader's last event.
    PeriphGone { addr: Address },
    /// A central subscribed to our TX characteristic.
    PeriphNotify(CharacteristicNotifier),
    /// One scanner sighting of a device offering (or not) the service.
    Scan {
        addr: Address,
        /// The type BlueZ holds for the address, `None` when it could
        /// not say (#506).
        addr_type: Option<AddrType>,
        rssi: Option<i16>,
        offers_service: bool,
        /// Payload of the CID 0xFFFF manufacturer record, if present.
        record: Option<Vec<u8>>,
    },
    /// A central-role connection read the peer's identity and asks to be
    /// admitted before it handshakes. `true` on `ack` = proceed.
    ///
    /// `frames` carries one PACKET per message — all its fragments as a
    /// unit — so the link task can pace packet-to-packet (#376) and a
    /// full queue costs whole packets, never a torn fragment stream. A
    /// keepalive rides as a single sub-header-size frame.
    CentralIdentity {
        addr: Address,
        identity: IdentityHash,
        mtu: usize,
        frames: census::PacketTx,
        ack: oneshot::Sender<bool>,
    },
    /// A notification arrived on a central-role link.
    CentralFrame { addr: Address, data: Vec<u8> },
    /// A central-role task ended (connect failure, session end, or the
    /// admission said no). Always the task's last event.
    ///
    /// `failure` is what a give-up before the session said about the
    /// peer (579), `None` when a session ran or the admission refused:
    /// the backoff the orchestrator books reads it.
    CentralGone {
        addr: Address,
        failure: Option<DialFailure>,
    },
    /// The scan task found the adapter's D-Bus object gone (600):
    /// bluetoothd restarted or the controller re-enumerated, and with
    /// the object went the GATT application, the advertisement and
    /// every connection. The scan task's last event; `error` is the
    /// discovery failure that showed it.
    AdapterLost { error: String },
}

/// How a BlueZ session ended without a setup error.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionEnd {
    /// The daemon dropped the interface or asked for carrier teardown.
    Detached,
    /// The adapter's object vanished under a running session (600).
    /// `forget` is every peer whose stored device object the session
    /// could not remove, because the adapter went first.
    AdapterLost { forget: BTreeSet<Addr> },
}

/// What a session that lost its adapter hands the sessions after it,
/// until one comes up (600).
struct LostAdapter {
    since: Instant,
    /// The peers whose `Device1` objects the link-down path owed a
    /// removal (#443) when the adapter vanished: the links released at
    /// the loss, and any departure whose removal failed against the
    /// already vanishing adapter. bluetoothd restores a connected peer's
    /// stored object, cached GATT database included, the moment the
    /// adapter returns, and a dial at that object found no Columba
    /// layout every time on btvirt (600's runs); the session that finds
    /// the adapter again removes them before it scans.
    forget: BTreeSet<Addr>,
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
        announce_cap_bitrate: None,
        tx_jitter_max_ms: None,
        tx_hold_spread_max_ms: None,
        acquisition: None,
        frame_turnaround_ms: None,
        ifac: None,
        mode: InterfaceMode::default(),
        kind: InterfaceKind::Ble,
        ingress_control: None,
    };

    // The carrier-teardown hook (#443 follow-on): BLE links are
    // peer-visible carrier state a dying process does not take with it —
    // the peer's BlueZ releases them by its own deferred disconnect,
    // seconds later — so the driver's shutdown drain asks this task to
    // close them while the runtime still polls it.
    let (teardown_tx, teardown_rx) = oneshot::channel();

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
    tokio::spawn(task.run(outgoing_rx, teardown_rx));

    InterfaceHandle {
        info,
        incoming: incoming_rx,
        outgoing: outgoing_tx,
        counters,
        credit: None,
        ready,
        teardown: Some(teardown_tx),
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

/// Orchestrator-side state for one central-role link's TX pipe. One
/// message = one packet's fragments (see [`Ev::CentralIdentity`]).
struct CentralPipe {
    frames: census::PacketTx,
}

/// The advertisement as it currently stands: BlueZ's handle (dropping it
/// deregisters the record) and the free-incoming-slot count the record
/// on the air carries (#432).
///
/// The count is kept beside the handle because BlueZ has no in-place
/// record update, so the only way to tell a stale count from a current
/// one is to remember what was published. See
/// [`BleTask::reconcile_advertising`].
///
/// The handle is held and never read: dropping it IS the
/// deregistration, so its only job is to live exactly as long as the
/// record it registered.
struct Advertised {
    _handle: bluer::adv::AdvertisementHandle,
    free_slots: u8,
}

impl BleTask {
    async fn run(
        self,
        mut outgoing_rx: mpsc::Receiver<OutgoingPacket>,
        teardown_rx: oneshot::Receiver<oneshot::Sender<()>>,
    ) {
        // Held across session retries: the hook fires once, whichever
        // session is live at that moment answers it.
        let mut teardown_rx = Some(teardown_rx);
        // Set while the adapter a running session held is gone (600):
        // re-resolution then runs at the scan cadence, quietly, and the
        // session that finds it again says so.
        let mut lost: Option<LostAdapter> = None;
        loop {
            let ended = self
                .session(&mut outgoing_rx, &mut teardown_rx, lost.as_ref())
                .await;
            // The session's tables died with it (597).
            self.counters.publish_census(None);
            match ended {
                Ok(SessionEnd::Detached) => return,
                Ok(SessionEnd::AdapterLost { forget }) => {
                    lost = Some(LostAdapter {
                        since: Instant::now(),
                        forget,
                    });
                    tokio::time::sleep(self.adapter_reresolve()).await;
                }
                Err(e) if lost.is_some() => {
                    tracing::debug!(
                        "BLE {}: adapter still gone ({e}); retrying in {:?}",
                        self.name,
                        self.adapter_reresolve()
                    );
                    tokio::time::sleep(self.adapter_reresolve()).await;
                }
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

    /// The pause between re-resolutions of a lost adapter (600): the
    /// scan cadence, so the interface is deaf for at most one scan
    /// interval longer than the adapter is gone. Floored at a second,
    /// because `discovery_interval = 0` is a valid config and must not
    /// turn into a busy loop against bluetoothd.
    fn adapter_reresolve(&self) -> Duration {
        self.opts.discovery_interval.max(Duration::from_secs(1))
    }

    /// One BlueZ session: set up both roles, then run the event loop
    /// until the daemon drops the interface or asks for carrier
    /// teardown ([`SessionEnd::Detached`]), the adapter's object
    /// vanishes under it ([`SessionEnd::AdapterLost`], 600), or a setup
    /// call fails (`Err`); [`run`] retries the last two. `lost` is set
    /// when an earlier session lost its adapter: this session then
    /// removes the device objects that loss left behind before either
    /// role starts, and logs `BLE_ADAPTER_BACK` once it is up.
    async fn session(
        &self,
        outgoing_rx: &mut mpsc::Receiver<OutgoingPacket>,
        teardown_rx: &mut Option<oneshot::Receiver<oneshot::Sender<()>>>,
        lost: Option<&LostAdapter>,
    ) -> bluer::Result<SessionEnd> {
        let session = bluer::Session::new().await?;
        let adapter = match &self.opts.adapter {
            Some(name) => session.adapter(name)?,
            None => session.default_adapter().await?,
        };
        adapter.set_powered(true).await?;
        let local_addr: Addr = adapter.address().await?.0;
        if let Some(lost) = lost {
            for addr in &lost.forget {
                forget_device(&adapter, *addr).await;
            }
        }
        // The removals an adapter loss could swallow (600), handed to
        // the next session if this one loses its adapter.
        let mut owed_removals = links::OwedRemovals::default();
        // The adapter object's removal as bluetoothd announces it (600):
        // a restart or a re-enumeration takes the GATT application, the
        // advertisement and every connection with the object. The scan
        // task's failing discovery shows the same loss, but only while
        // the object stays away; a bluetoothd restart can bring it back
        // before the next window, and then the scan task scans on while
        // nothing serves or advertises. Without the stream (subscription
        // refused) the scan task's check still stands.
        let mut adapter_events = match session.events().await {
            Ok(events) => Some(Box::pin(events)),
            Err(e) => {
                tracing::warn!(
                    "BLE {}: adapter removal watch not subscribed: {e}",
                    self.name
                );
                None
            }
        };

        let (ev_tx, mut ev_rx) = mpsc::channel::<Ev>(64);

        // The link table is built before either role, because both read
        // it at setup: the advertisement carries the table's free
        // incoming count (#432) from its very first registration, so
        // there is no window in which a fresh node advertises a count it
        // did not compute.
        let mut table = LinkTable::new(self.identity_hash, self.opts.max_connections)
            .with_accept_only(self.opts.accept_only.clone());

        // Peripheral half. The GATT application lives for the session —
        // live peripheral links keep using it — while the advertisement
        // is gated on the table's INCOMING occupancy below
        // (`should_advertise`, the firmware's ADV_LOCK policy at
        // ead0bce): deregistered when they fill, re-registered when a
        // slot frees or the published count goes stale. Both handles
        // deregister on drop.
        let _app = if self.opts.enable_peripheral {
            Some(
                bluez::serve_gatt(
                    &adapter,
                    self.identity_hash,
                    ev_tx.clone(),
                    self.name.clone(),
                )
                .await?,
            )
        } else {
            None
        };
        let mut adv = if self.opts.enable_peripheral {
            let free_slots = table.free_incoming_slots();
            Some(Advertised {
                _handle: bluez::register_advertisement(
                    &adapter,
                    self.identity_hash,
                    &self.name,
                    free_slots,
                )
                .await?,
                free_slots,
            })
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
        if let Some(lost) = lost {
            tracing::info!(
                event = "BLE_ADAPTER_BACK",
                iface = %Scalar(&self.name),
                hci = %Scalar(adapter.name()),
                down_ms = lost.since.elapsed().as_millis() as u64,
                forgot = lost.forget.len(),
            );
        }
        self.ready.signal_ready();

        let start = Instant::now();
        let now_ms = |i: Instant| i.duration_since(start).as_millis() as u64;

        // The peripheral notify pipe's inter-packet gap (#376). One
        // pacer for the pipe IS per-link pacing here: a notification
        // reaches every subscribed central at once, so all peripheral
        // links share one schedule. The central links pace themselves,
        // each in its own task (`bluez::CentralTask::session_loop`).
        let mut notify_pacer = links::LinkPacer::new(self.opts.tx_gap_ms);
        // The firmware central task's fallback clock and collection
        // window (#375 part 2, item 3), driven from the event loop:
        // sightings feed it, ticks close its windows.
        let mut scheduler = ScanScheduler::new(0);
        let mut notifiers: Vec<CharacteristicNotifier> = Vec::new();
        let mut central_pipes: HashMap<Addr, CentralPipe> = HashMap::new();
        // One connection setup in flight per adapter, jittered (#49
        // part 3). Seeded from entropy; tests seed the queue directly.
        let mut dial_queue = DialQueue::new(rand_core::RngCore::next_u64(&mut rand_core::OsRng));
        let mut backoff_until: HashMap<Addr, u64> = HashMap::new();
        // The per-address run behind a failed dial (579), the board's
        // table: it decides how long `backoff_until` holds an address
        // after a give-up, and the sightings below feed its absence
        // reset.
        let mut dial_backoff: DialBackoff<DIAL_BACKOFF_HOST_SLOTS> = DialBackoff::new();
        // BLE_SCAN_DECISION and BLE_SCAN_DROPPED are logged once per
        // (address, decision) and (address, drop) change, not per
        // sighting — a waiting peer advertises several times a second
        // (same rule as the firmware).
        let mut scan_lines = links::ScanLineGate::default();
        // The peers that keep leaving (#417, #412): fed at the two
        // departure sites below, read where a sighting would enter the
        // collection window.
        let mut churn = links::ChurnGate::new(links::CHURN_POLICY);
        // The address type of every handshaked link, read from BlueZ at
        // the handshake (#506): the departure that ends the link reads
        // it, when the device object may already be gone. Pruned to the
        // table's links on every tick.
        let mut link_addr_types: HashMap<Addr, AddrType> = HashMap::new();

        // Published once before the first tick, so a dump taken in the
        // first second already names this interface's (empty) tables.
        self.counters.publish_census(Some(
            census::BleState {
                table: &table,
                central_pipes: &central_pipes,
                notifiers: &notifiers,
                dial_queue: &dial_queue,
                backoff_until: &backoff_until,
                dial_backoff: &dial_backoff,
                scan_lines: &scan_lines,
                churn: &churn,
                link_addr_types: &link_addr_types,
                owed_removals: &owed_removals,
            }
            .census(),
        ));

        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            // The head dial's start time, re-read each pass: any event
            // may arm, consume or postpone it. `None` parks the arm.
            let dial_at = dial_queue
                .next_deadline_ms()
                .map(|ms| start + Duration::from_millis(ms));
            tokio::select! {
                outgoing = outgoing_rx.recv() => {
                    let Some(packet) = outgoing else {
                        // Daemon dropped the handle: interface detached.
                        return Ok(SessionEnd::Detached);
                    };
                    self.send_packet(
                        &table,
                        &central_pipes,
                        &mut notifiers,
                        &mut notify_pacer,
                        start,
                        &packet.data,
                        packet.peer,
                    )
                    .await;
                }
                ack = async {
                    match teardown_rx.as_mut() {
                        Some(rx) => rx.await,
                        None => std::future::pending().await,
                    }
                } => {
                    // The driver's shutdown drain (or, on the error arm,
                    // a dropped hook) asks for carrier teardown: close
                    // every connection deliberately, while the runtime
                    // still polls this task, and leave this adapter the
                    // fresh device state #443 wants the next daemon on
                    // this controller to meet. What this CANNOT buy is a
                    // faster peer notice: bluetoothd paces every
                    // teardown — a client's `Disconnect`, a
                    // `RemoveDevice` of a connected device, and its own
                    // cleanup when the client just dies — through the
                    // same ~2 s deferred-disconnect machinery, so the
                    // peers' `BLE_LINK_DOWN` stays at 2.0-3.0 s after
                    // SIGTERM whichever of the three happens (measured
                    // on btvirt, all three ways). Each call is bounded
                    // and its reply dropped: the request is on the
                    // D-Bus connection's write path the moment the call
                    // starts, bluetoothd executes it without us, and
                    // waiting out its timer would only trip the
                    // driver's teardown bound.
                    *teardown_rx = None;
                    let addrs = table.connected_addrs();
                    tracing::info!(
                        "BLE {}: teardown, closing {} connection(s)",
                        self.name,
                        addrs.len()
                    );
                    for addr in addrs {
                        let _ = tokio::time::timeout(
                            Duration::from_millis(50),
                            forget_device(&adapter, addr),
                        )
                        .await;
                    }
                    if let Ok(ack) = ack {
                        let _ = ack.send(());
                    }
                    return Ok(SessionEnd::Detached);
                }
                gone = async {
                    match adapter_events.as_mut() {
                        Some(events) => events.next().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match gone {
                        Some(bluer::SessionEvent::AdapterRemoved(name)) if name == adapter.name() => {
                            let forget = self
                                .release_lost_adapter(
                                    &adapter,
                                    &mut table,
                                    owed_removals,
                                    "InterfacesRemoved",
                                    now_ms(Instant::now()),
                                )
                                .await;
                            return Ok(SessionEnd::AdapterLost { forget });
                        }
                        Some(_) => {}
                        None => adapter_events = None,
                    }
                }
                ev = ev_rx.recv() => {
                    let Some(ev) = ev else { return Ok(SessionEnd::Detached) };
                    if let Ev::AdapterLost { error } = ev {
                        let forget = self
                            .release_lost_adapter(
                                &adapter,
                                &mut table,
                                owed_removals,
                                &error,
                                now_ms(Instant::now()),
                            )
                            .await;
                        return Ok(SessionEnd::AdapterLost { forget });
                    }
                    self.handle_event(
                        ev,
                        &adapter,
                        &local_addr,
                        &mut table,
                        &mut scheduler,
                        &mut notifiers,
                        &mut central_pipes,
                        &mut dial_queue,
                        &mut backoff_until,
                        &mut dial_backoff,
                        &mut scan_lines,
                        &mut churn,
                        &mut link_addr_types,
                        &mut owed_removals,
                        &ev_tx,
                        now_ms(Instant::now()),
                    )
                    .await;
                    self.reconcile_advertising(&adapter, &table, &mut adv).await;
                }
                _ = async {
                    match dial_at {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.pump_dials(
                        &mut dial_queue,
                        &table,
                        &adapter,
                        &backoff_until,
                        &ev_tx,
                        now_ms(Instant::now()),
                    );
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
                            pipe.frames
                                .try_send(vec![vec![leviculum_core::framing::ble::KEEPALIVE_BYTE]]);
                        }
                    }
                    let expired = table.expire(now);
                    if !(expired.links.is_empty() && expired.pending.is_empty()) {
                        // Connections ended: a fresh strict phase, as
                        // the firmware resets its clock at teardown.
                        scheduler.note_reset(now);
                    }
                    for (identity, addr, role) in expired.links {
                        // An expiry is the peer's own departure (#417):
                        // it went silent until the bound swept the link,
                        // the rotation's signature. The firmware's
                        // `BLE_LINK_EXPIRE` site feeds the same ledger.
                        let reason = match role {
                            Role::Central => "expiry_central",
                            Role::Peripheral => "expiry_peripheral",
                        };
                        let addr_type = link_addr_types.get(&addr).copied();
                        self.note_peer_leave(&mut churn, &identity, &addr, addr_type, reason, now);
                        self.log_link_down(&identity, role, "timeout");
                        central_pipes.remove(&addr);
                        disconnect_quietly(&adapter, addr).await;
                        let done = forget_device(&adapter, addr).await;
                        owed_removals.note(addr, done, now);
                        backoff_until.insert(addr, now + SESSION_BACKOFF_MS);
                        self.report_peer_lost(&table, identity).await;
                    }
                    for addr in expired.pending {
                        tracing::debug!(
                            "BLE {}: disconnecting {} — no identity handshake within timeout",
                            self.name, Address(addr)
                        );
                        disconnect_quietly(&adapter, addr).await;
                        // #548's timeout says the peer will not
                        // handshake: its address is not dialled back
                        // on the next sighting (579).
                        self.book_dial_failure(
                            &mut dial_backoff,
                            &mut backoff_until,
                            addr,
                            DialFailure::NoHandshake,
                            now,
                        );
                    }
                    backoff_until.retain(|_, until| *until > now);
                    link_addr_types.retain(|addr, _| table.knows_addr(addr));
                    // A collection window whose bound passed without a
                    // further sighting closes on the tick.
                    self.dial_window_choice(
                        &mut scheduler,
                        &table,
                        &adapter,
                        &mut dial_queue,
                        &backoff_until,
                        &ev_tx,
                        now,
                    );
                    // Expiry can free a slot, and a failed re-register
                    // gets its retry here.
                    self.reconcile_advertising(&adapter, &table, &mut adv).await;
                    // The census of this task's own tables (597), read by
                    // the SIGUSR1 dump: at most one tick old.
                    self.counters.publish_census(Some(
                        census::BleState {
                            table: &table,
                            central_pipes: &central_pipes,
                            notifiers: &notifiers,
                            dial_queue: &dial_queue,
                            backoff_until: &backoff_until,
                            dial_backoff: &dial_backoff,
                            scan_lines: &scan_lines,
                            churn: &churn,
                            link_addr_types: &link_addr_types,
                            owed_removals: &owed_removals,
                        }
                        .census(),
                    ));
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
        scheduler: &mut ScanScheduler,
        notifiers: &mut Vec<CharacteristicNotifier>,
        central_pipes: &mut HashMap<Addr, CentralPipe>,
        dial_queue: &mut DialQueue,
        backoff_until: &mut HashMap<Addr, u64>,
        dial_backoff: &mut DialBackoff<DIAL_BACKOFF_HOST_SLOTS>,
        scan_lines: &mut links::ScanLineGate,
        churn: &mut links::ChurnGate,
        link_addr_types: &mut HashMap<Addr, AddrType>,
        owed_removals: &mut links::OwedRemovals,
        ev_tx: &mpsc::Sender<Ev>,
        now: u64,
    ) {
        match ev {
            Ev::PeriphNotify(notifier) => {
                notifiers.push(notifier);
            }
            Ev::PeriphWrite { addr, mtu, data } => {
                let mtu = self.sizing_mtu(mtu);
                self.counters
                    .rx_bytes
                    .fetch_add(data.len() as u64, Ordering::Relaxed);
                // Read before the table is borrowed for the frame:
                // only the refusal arm needs it, and hoisting keeps the
                // arm free of a second borrow.
                let accept_listed = table.accept_only_len();
                match table.peripheral_frame(addr.0, mtu, &data, now) {
                    Inbound::Packet(packet) => {
                        let peer = table.link_by_addr(&addr.0).map(|l| l.identity);
                        self.deliver(packet, peer).await
                    }
                    Inbound::HandshakeComplete {
                        identity,
                        displaced,
                    } => {
                        // A connection event in either role restarts
                        // the strict phase (#375 §0).
                        scheduler.note_reset(now);
                        // A handshake in either role ends the address's
                        // backoff run (579).
                        dial_backoff.note_success(links::addr_value_display(&addr.0));
                        let addr_type = bluez::addr_type(adapter, addr).await;
                        if let Some(addr_type) = addr_type {
                            link_addr_types.insert(addr.0, addr_type);
                        }
                        let first_link = displaced.is_none();
                        if let Some(old) = displaced {
                            // A replacement is a departure too (#417):
                            // the identity left its old address even
                            // though it came straight back.
                            self.note_peer_leave(
                                churn, &identity, &addr.0, addr_type, "replaced", now,
                            );
                            self.log_link_replaced(&identity, &addr.0, Role::Peripheral, &old);
                            self.log_link_down(&old.identity, old.role, "displaced");
                            central_pipes.remove(&old.addr);
                            disconnect_quietly(adapter, old.addr).await;
                        }
                        self.log_link_up(&identity, &addr.0, Role::Peripheral, mtu);
                        if first_link {
                            self.report_peer_up(identity).await;
                        }
                    }
                    Inbound::HandshakeRejected(admission) => {
                        self.log_rejection(
                            admission,
                            Role::Peripheral,
                            &data,
                            &addr.0,
                            accept_listed,
                        );
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
            Ev::PeriphGone { addr } => {
                // The central left (#436). Before the write socket the
                // host had no word of it and the link waited out the
                // expiry sweep; now it goes when the carrier says so, the
                // way a central-role link goes at `CentralGone`.
                if let Some((identity, _, role)) = table.remove_peripheral_by_addr(&addr.0) {
                    scheduler.note_reset(now);
                    self.log_link_down(&identity, role, "disconnected");
                    let done = forget_device(adapter, addr.0).await;
                    owed_removals.note(addr.0, done, now);
                    self.report_peer_lost(table, identity).await;
                }
            }
            Ev::Scan {
                addr,
                addr_type,
                rssi,
                offers_service,
                record,
            } => {
                // A sighting with no RSSI is a BlueZ cache entry, not a
                // device on the air right now.
                let Some(rssi) = rssi else { return };
                // The fallback clock's busy input (#375 part 3, the
                // eager spec): only a dial of ours in flight or queued
                // — either way its outcome is about to reset the clock
                // — or a handshake still pending holds the clock at
                // zero. A handshaked link does not — its address is
                // kept out of the window by knows_addr below, and the
                // clock must keep running so a linked-but-losing-the-
                // sort board can still fallback-dial a third party.
                let busy = table.has_pending_handshakes() || !dial_queue.is_idle();
                let (mode, announce) = scheduler.mode(busy, now);
                if let Some(after_ms) = announce {
                    tracing::info!(
                        event = "BLE_SCAN_FALLBACK",
                        iface = %Scalar(&self.name),
                        after_ms = after_ms,
                    );
                }
                let decision = match links::decide_from_scan(
                    local_addr,
                    &addr.0,
                    offers_service,
                    record.as_deref(),
                    mode,
                ) {
                    Ok(decision) => decision,
                    Err(dropped) => {
                        // Not a peer to the decision, but not silent
                        // either (577): a Columba device whose stored
                        // `UUIDs` lost the service reads exactly like a
                        // stranger here, and that silence cost #443 its
                        // first hour.
                        if scan_lines.dropped(addr.0, dropped) {
                            tracing::debug!(
                                event = "BLE_SCAN_DROPPED",
                                iface = %Scalar(&self.name),
                                addr = %hex12(&addr.0),
                                reason = dropped.reason.as_str(),
                                hint = %links::hint_str(dropped.identity_hint),
                                rssi = rssi,
                            );
                        }
                        return;
                    }
                };
                // Who we are WILLING to dial (`initiate_only`), as
                // opposed to who the sort says dials whom. A pure
                // function of the address and the advertised hint, both
                // already part of the deduplicated decision below, so
                // the verdict changes only when that line does.
                let dial_allowed = self
                    .opts
                    .initiate_only
                    .allows(&addr.0, decision.identity_hint);
                // The sighting feeds the dial backoff's absence reset
                // and reads its hold (579), whatever the verdict.
                let held = dial_backoff.sighted(links::addr_value_display(&addr.0), now);
                if scan_lines.decided(addr.0, decision) {
                    tracing::info!(
                        event = "BLE_SCAN_DECISION",
                        addr = %hex12(&addr.0),
                        caps = %format_args!("{:#04x}", decision.caps),
                        caps_record = u8::from(decision.caps_record),
                        free_slots = %match decision.free_slots {
                            Some(free) => format!("{free}"),
                            None => "unknown".to_string(),
                        },
                        hint = %links::hint_str(decision.identity_hint),
                        initiate = u8::from(decision.decision.initiate()),
                        rule = decision.decision.as_str(),
                        // The window's first ordering term (#412 part
                        // 1), the same key the firmware prints: a pure
                        // function of this advertisement, so a merged
                        // rig timeline can compare the two stacks' tiers
                        // on one peer.
                        pref = dial_preference(decision.decision, decision.free_slots).as_str(),
                    );
                    // Only when the allow-list is what stops the dial:
                    // a peer the sort tells us to wait for is not one
                    // we declined, and a line saying so would send a
                    // scenario author after the wrong key.
                    if decision.decision.initiate() && !dial_allowed {
                        tracing::info!(
                            event = "BLE_DIAL_NOT_ALLOWED",
                            iface = %Scalar(&self.name),
                            addr = %hex12(&addr.0),
                            hint = %links::hint_str(decision.identity_hint),
                            listed = self.opts.initiate_only.len(),
                        );
                    }
                }
                // A backed-off address that the verdict would dial says
                // so once per hold (579): the line a room reads to see
                // a stranger left alone, and why until when. `until_ms`
                // is on this interface's clock, the one `now` runs on.
                if let Some(until_ms) = held.filter(|_| decision.decision.initiate()) {
                    if scan_lines.backed_off(addr.0, until_ms) {
                        let value = links::addr_value_display(&addr.0);
                        tracing::info!(
                            event = "BLE_SCAN_DECISION",
                            addr = %hex12(&addr.0),
                            hint = %links::hint_str(decision.identity_hint),
                            initiate = 0u8,
                            rule = "backoff",
                            until_ms = until_ms,
                            in_ms = until_ms.saturating_sub(now),
                            strikes = dial_backoff.strikes(value),
                        );
                    }
                }
                // `knows_identity_hint` is the address filter above it
                // keyed by IDENTITY (#412): a peer that rotated its
                // address advertises as a stranger, and dialling it
                // costs a connect, a discovery and an identity read
                // before the duplicate is found. A peer that carried no
                // hint matches nothing and is dialled exactly as before.
                //
                // The allow-list sits with the other reasons not to
                // dial, and is checked here alone: the window's
                // re-checks below re-ask what the WORLD may have
                // changed (no outgoing slot, backoff, an inbound link
                // that landed meanwhile), and a policy does not change
                // under them. Nothing enters the window that did not
                // pass this line.
                //
                // `peer_has_room` sits here for the same reason (#432):
                // it is a property of THIS advertisement — the peer said
                // its last incoming slot is gone — and a re-check later
                // in the window would have no fresher record to read. A
                // peer that frees a slot advertises again, with a count
                // that passes.
                if !decision.decision.initiate()
                    || !dial_allowed
                    || !links::peer_has_room(decision.free_slots)
                    || rssi < self.opts.min_rssi
                    || !table.may_dial()
                    || table.knows_addr(&addr.0)
                    || table.knows_identity_hint(decision.identity_hint)
                    || dial_queue.knows(&addr.0)
                    || backoff_until.get(&addr.0).is_some_and(|until| *until > now)
                    || held.is_some()
                    || self.churn_held(churn, &addr.0, addr_type, decision.decision, now)
                {
                    return;
                }
                // Eligible: into the collection window instead of an
                // immediate dial — the firmware's window, the same
                // CandidateTable, the same choice: the peer that cannot
                // dial us at all first (#412 part 1), then the emptiest
                // advertised peer (#375 item 3), then the lowest
                // address, except that a peer which redraws its address
                // does not get to win that tie with it (#412).
                // The type ranks a public address with the fixed ones
                // (#506; the guess put #504's behind every board).
                scheduler.offer(
                    addr.0,
                    addr_type,
                    decision.decision,
                    decision.free_slots,
                    now,
                );
                self.dial_window_choice(
                    scheduler,
                    table,
                    adapter,
                    dial_queue,
                    backoff_until,
                    ev_tx,
                    now,
                );
            }
            Ev::CentralIdentity {
                addr,
                identity,
                mtu,
                frames,
                ack,
            } => {
                let mtu = self.sizing_mtu(mtu);
                let (admission, displaced) = table.admit(identity, addr.0, Role::Central, mtu, now);
                match admission {
                    Admission::Accept => {
                        scheduler.note_reset(now);
                        // The dial landed: the handshake completing ends
                        // the setup, so the queue's one-in-flight slot
                        // frees here (the eager spec's clock keeps
                        // running while links are live). CentralGone
                        // still fires at the session's end; its release
                        // is then a no-op.
                        dial_queue.release(&addr.0, now);
                        dial_backoff.note_success(links::addr_value_display(&addr.0));
                        let addr_type = bluez::addr_type(adapter, addr).await;
                        if let Some(addr_type) = addr_type {
                            link_addr_types.insert(addr.0, addr_type);
                        }
                        let first_link = displaced.is_none();
                        if let Some(old) = displaced {
                            self.note_peer_leave(
                                churn, &identity, &addr.0, addr_type, "replaced", now,
                            );
                            self.log_link_replaced(&identity, &addr.0, Role::Central, &old);
                            self.log_link_down(&old.identity, old.role, "displaced");
                            central_pipes.remove(&old.addr);
                            disconnect_quietly(adapter, old.addr).await;
                        }
                        central_pipes.insert(addr.0, CentralPipe { frames });
                        self.log_link_up(&identity, &addr.0, Role::Central, mtu);
                        if first_link {
                            self.report_peer_up(identity).await;
                        }
                        let _ = ack.send(true);
                    }
                    other => {
                        self.log_rejection(
                            other,
                            Role::Central,
                            &identity,
                            &addr.0,
                            table.accept_only_len(),
                        );
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
                    Inbound::Packet(packet) => {
                        let peer = table.link_by_addr(&addr.0).map(|l| l.identity);
                        self.deliver(packet, peer).await
                    }
                    Inbound::Error => {
                        tracing::debug!("BLE {}: bad fragment from {} dropped", self.name, addr);
                    }
                    _ => {}
                }
                self.log_abandon_reports(table);
            }
            Ev::CentralGone { addr, failure } => {
                // A dial or link ended either way: fresh strict phase,
                // as the firmware resets its clock at teardown. A setup
                // that timed out or failed frees the queue's slot here
                // (a completed one already freed it at admission).
                scheduler.note_reset(now);
                dial_queue.release(&addr.0, now);
                central_pipes.remove(&addr.0);
                if let Some((identity, _, role)) = table.remove_by_addr(&addr.0) {
                    self.log_link_down(&identity, role, "disconnected");
                    let done = forget_device(adapter, addr.0).await;
                    owed_removals.note(addr.0, done, now);
                    self.report_peer_lost(table, identity).await;
                }
                match failure {
                    Some(failure) => {
                        self.book_dial_failure(dial_backoff, backoff_until, addr.0, failure, now);
                    }
                    None => {
                        let until = backoff_until.entry(addr.0).or_insert(0);
                        *until = (*until).max(now + SESSION_BACKOFF_MS);
                    }
                }
            }
            // Ends the session in the event loop, which never hands it
            // here (600).
            Ev::AdapterLost { .. } => {}
        }
    }

    /// Book a dial that gave up at `addr` in the shared backoff (579)
    /// and hold the address until the deadline it chose: the flat base
    /// for a transport failure, a doubling run for one that named the
    /// peer. Logs `BLE_DIAL_BACKOFF` with the run.
    fn book_dial_failure(
        &self,
        dial_backoff: &mut DialBackoff<DIAL_BACKOFF_HOST_SLOTS>,
        backoff_until: &mut HashMap<Addr, u64>,
        addr: Addr,
        failure: DialFailure,
        now: u64,
    ) {
        let booked = dial_backoff.note_failure(links::addr_value_display(&addr), failure, now);
        let until = backoff_until.entry(addr).or_insert(0);
        *until = (*until).max(booked.until_ms);
        tracing::info!(
            event = "BLE_DIAL_BACKOFF",
            iface = %Scalar(&self.name),
            addr = %hex12(&addr),
            failure = failure.as_str(),
            strikes = booked.strikes,
            backoff_ms = booked.until_ms.saturating_sub(now),
            until_ms = booked.until_ms,
        );
    }

    /// Close the scheduler's collection window if its bound has passed
    /// and defer its choice into the dial queue — strict verdicts
    /// first, then the emptiest peer, then the lowest address among
    /// those that keep one (#412), elected by the same
    /// [`CandidateTable`] the firmware and the #375 simulation use. Logs
    /// `BLE_SCAN_WINDOW` once per window that ends in a dial, like the
    /// firmware, and `BLE_DIAL_QUEUE` for the deferred dial (#49 part 3:
    /// every dial defers — at least its pre-connect jitter, plus any
    /// setup already in flight).
    ///
    /// The eligibility filters ran when each candidate was collected;
    /// they run again here because the window is seconds long and the
    /// world moves — the chosen peer may have connected to us in the
    /// meantime, the table may have filled, a failure may have imposed
    /// a backoff. A window whose choice fails the re-check simply ends
    /// without a dial; the peer's next advertisement opens a new one.
    #[allow(clippy::too_many_arguments)]
    fn dial_window_choice(
        &self,
        scheduler: &mut ScanScheduler,
        table: &LinkTable,
        adapter: &bluer::Adapter,
        dial_queue: &mut DialQueue,
        backoff_until: &HashMap<Addr, u64>,
        ev_tx: &mpsc::Sender<Ev>,
        now: u64,
    ) {
        if let Some((addr, decision, seen)) = scheduler.poll(now) {
            if table.may_dial()
                && !(table.knows_addr(&addr)
                    || dial_queue.knows(&addr)
                    || backoff_until.get(&addr).is_some_and(|until| *until > now))
            {
                tracing::info!(
                    event = "BLE_SCAN_WINDOW",
                    iface = %Scalar(&self.name),
                    seen = seen,
                    chosen = %hex12(&addr),
                    rule = decision.as_str(),
                );
                if let Some(queued) = dial_queue.enqueue(addr, decision, now) {
                    tracing::info!(
                        event = "BLE_DIAL_QUEUE",
                        iface = %Scalar(&self.name),
                        depth = queued.depth,
                        wait_ms = queued.wait_ms,
                    );
                }
            }
        }
        self.pump_dials(dial_queue, table, adapter, backoff_until, ev_tx, now);
    }

    /// Start the dial the queue hands out, if any: one connection setup
    /// in flight per adapter (#49 part 3). A popped dial whose
    /// eligibility re-check fails — the world moved while it waited out
    /// its jitter or a setup ahead of it — is dropped, its release
    /// arming the next queued dial.
    fn pump_dials(
        &self,
        dial_queue: &mut DialQueue,
        table: &LinkTable,
        adapter: &bluer::Adapter,
        backoff_until: &HashMap<Addr, u64>,
        ev_tx: &mpsc::Sender<Ev>,
        now: u64,
    ) {
        while let Some((addr, _decision)) = dial_queue.pop_ready(now) {
            if !table.may_dial()
                || table.knows_addr(&addr)
                || backoff_until.get(&addr).is_some_and(|until| *until > now)
            {
                tracing::debug!(
                    "BLE {}: queued dial to {} no longer eligible, dropped",
                    self.name,
                    Address(addr)
                );
                dial_queue.release(&addr, now);
                continue;
            }
            let central = bluez::CentralTask {
                adapter: adapter.clone(),
                addr: Address(addr),
                own_identity: self.identity_hash,
                ev_tx: ev_tx.clone(),
                iface: self.name.clone(),
                tx_gap_ms: self.opts.tx_gap_ms,
            };
            tokio::spawn(central.run());
        }
    }

    /// Apply the table's advertising verdict (#49 item 1, the firmware's
    /// ADV_LOCK policy): a node with no incoming slot free takes the
    /// advertisement off the air — dropping the handle deregisters it
    /// from BlueZ — and a freed slot puts it back. The GATT application
    /// is untouched either way; live peripheral sessions keep running
    /// while dark, exactly as the firmware keeps serving its sessions
    /// when nothing advertises. Called after every event and on every
    /// tick, so a failed re-registration is retried within a second.
    ///
    /// Since #432 it also keeps the PUBLISHED count honest: the record
    /// carries the free incoming slots, and BlueZ offers no in-place
    /// edit of a live record, so a changed count is a deregister and a
    /// register. That costs an advertising gap exactly when a link went
    /// up or came down — the moments the count changes and no others,
    /// because the published value is remembered in [`Advertised`] and
    /// compared, not re-registered per tick. A board pays the same price
    /// by rebuilding its record at each advertising start.
    ///
    /// Like a board's, the count can understate and never overstate: an
    /// incoming link is admitted before the record naming its slot comes
    /// off the air, so the air lags behind a node that got emptier and
    /// never behind one that got fuller.
    async fn reconcile_advertising(
        &self,
        adapter: &bluer::Adapter,
        table: &LinkTable,
        adv: &mut Option<Advertised>,
    ) {
        if !self.opts.enable_peripheral {
            return;
        }
        if !table.should_advertise() {
            if adv.take().is_some() {
                tracing::info!(
                    event = "BLE_ADV_GATE",
                    iface = %Scalar(&self.name),
                    state = "off",
                    reason = "full",
                );
            }
            return;
        }
        // Non-zero by the verdict above, which is a test of exactly this
        // number.
        let free_slots = table.free_incoming_slots();
        if adv.as_ref().is_some_and(|a| a.free_slots == free_slots) {
            return;
        }
        // Either nothing is on the air or what is on the air names the
        // wrong count. Drop it first: two registrations of one node
        // would put two records with two counts on the air.
        let refresh = u8::from(adv.take().is_some());
        match bluez::register_advertisement(adapter, self.identity_hash, &self.name, free_slots)
            .await
        {
            Ok(handle) => {
                *adv = Some(Advertised {
                    _handle: handle,
                    free_slots,
                });
                tracing::info!(
                    event = "BLE_ADV_GATE",
                    iface = %Scalar(&self.name),
                    state = "on",
                    free_slots = free_slots,
                    refresh = refresh,
                );
            }
            Err(e) => tracing::warn!(
                "BLE {}: re-registering advertisement failed ({e}); retrying",
                self.name
            ),
        }
    }

    /// Deliver one Reticulum packet: to the peer the core addressed it
    /// to, or — with no addressee — to every live link.
    ///
    /// `peer` is the core's #376 delivery hint, the identity a path entry
    /// carries as `via_peer`. With it, this interface stops copying a
    /// routed packet onto links it was never meant for; without it (an
    /// announce, a path request) the fan-out is unchanged. See
    /// [`LinkTable::plan_tx_to`] for the mapping and for the
    /// peer-with-no-live-link decision.
    ///
    /// The notify pipe is paced here (#376): the orchestrator is the
    /// pipe's only writer, so the inter-packet gap is served inline
    /// before the packet's first fragment. That parks the event loop
    /// for up to the gap — deliberate: the gap IS the pipe's throughput
    /// ceiling, and inbound events queue in their channels meanwhile.
    /// Central links get their packet as one queued unit and pace
    /// themselves in their own task.
    #[allow(clippy::too_many_arguments)]
    async fn send_packet(
        &self,
        table: &LinkTable,
        central_pipes: &HashMap<Addr, CentralPipe>,
        notifiers: &mut Vec<CharacteristicNotifier>,
        notify_pacer: &mut links::LinkPacer,
        start: Instant,
        packet: &[u8],
        peer: Option<IdentityHash>,
    ) {
        let plan = table.plan_tx_to(packet, peer.as_ref());
        match plan.route {
            // One line per outbound packet, like the sibling
            // `BLE_TX_GAP`: the bench recipe on #376 reads the routing
            // decision off the same capture as the pacing.
            links::TxRoute::Flood => tracing::info!(
                event = "BLE_TX_FLOOD",
                iface = %Scalar(&self.name),
                links = table.live_links(),
                len = packet.len(),
            ),
            links::TxRoute::Routed { peer, role } => tracing::info!(
                event = "BLE_TX_ROUTE",
                iface = %Scalar(&self.name),
                peer = %hex8(&peer),
                conn = %role.as_str(),
                len = packet.len(),
            ),
            links::TxRoute::NoLink { peer } => {
                self.counters.tx_queue_drops.fetch_add(1, Ordering::Relaxed);
                self.counters
                    .tx_dropped_bytes
                    .fetch_add(packet.len() as u64, Ordering::Relaxed);
                tracing::warn!(
                    event = "BLE_TX_ROUTE_MISS",
                    iface = %Scalar(&self.name),
                    peer = %hex8(&peer),
                    len = packet.len(),
                );
                return;
            }
        }
        let mut delivered = false;
        notifiers.retain(|n| !n.is_stopped());
        if !plan.notify_fragments.is_empty() && !notifiers.is_empty() {
            let now_ms = Instant::now().duration_since(start).as_millis() as u64;
            let wait = notify_pacer.wait_ms(now_ms);
            if wait > 0 {
                tracing::info!(
                    event = "BLE_TX_GAP",
                    iface = %Scalar(&self.name),
                    link = "notify",
                    waited_ms = wait,
                );
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
            let mut ok = true;
            for frag in plan.notify_fragments {
                if !send_via_notifiers(notifiers, frag).await {
                    ok = false;
                    break;
                }
            }
            notify_pacer.packet_done(Instant::now().duration_since(start).as_millis() as u64);
            delivered |= ok;
        }
        for (addr, frags) in plan.central {
            let Some(pipe) = central_pipes.get(&addr) else {
                continue;
            };
            let frag_count = frags.len();
            // One try_send per packet: it lands whole or not at all, so
            // a full queue can no longer tear the peer's reassembly
            // with a partial fragment stream.
            if pipe.frames.try_send(frags) {
                delivered = true;
            } else {
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
                    iface = %Scalar(&self.name),
                    peer = %peer,
                    len = packet.len(),
                    depth = frag_count,
                );
            }
        }
        if delivered {
            self.counters
                .tx_bytes
                .fetch_add(packet.len() as u64, Ordering::Relaxed);
        }
    }

    /// Hand one reassembled packet to the driver, naming the peer link it
    /// arrived through (Codeberg #573): the identity this link presented
    /// in the handshake, the same value [`Self::send_packet`] gets back as
    /// a delivery hint. Without it the core learns no `via_peer` and keeps
    /// no peer per relayed link, and every routed packet floods.
    async fn deliver(&self, packet: Vec<u8>, peer: Option<IdentityHash>) {
        let _ = self
            .incoming_tx
            .send(IncomingPacket { data: packet, peer })
            .await;
    }

    /// The adapter's object vanished (600): log `BLE_ADAPTER_LOST` and
    /// release every link through the link-down path. The connections
    /// went with the object, so there is nothing to disconnect; what
    /// matters is that the driver hears `BLE_PEER_LOST` for each peer
    /// and culls the paths through it, instead of holding them until
    /// the session that replaces this one has forgotten them. Not a
    /// departure of the peer, so the churn ledger and the dial backoff
    /// are left alone. The path's last step, removing the peer's device
    /// object (#443), cannot run without the adapter: the addresses come
    /// back, with every earlier removal the loss could have swallowed
    /// ([`links::OwedRemovals`]), for the session that finds the adapter
    /// again.
    async fn release_lost_adapter(
        &self,
        adapter: &bluer::Adapter,
        table: &mut LinkTable,
        owed_removals: links::OwedRemovals,
        error: &str,
        now: u64,
    ) -> BTreeSet<Addr> {
        let addrs = table.connected_addrs();
        tracing::warn!(
            event = "BLE_ADAPTER_LOST",
            iface = %Scalar(&self.name),
            hci = %Scalar(adapter.name()),
            links = addrs.len(),
            error = %Scalar(error),
        );
        let owed = owed_removals.owed(&addrs, now);
        for addr in addrs {
            if let Some((identity, _, role)) = table.remove_by_addr(&addr) {
                self.log_link_down(&identity, role, "adapter_lost");
                self.report_peer_lost(table, identity).await;
            }
        }
        owed
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
            iface = %Scalar(&self.name),
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
            iface = %Scalar(&self.name),
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
                iface = %Scalar(&self.name),
                peer = %hex8(&identity),
                lost = lost,
                total = total,
            );
        }
    }

    /// The MTU a link's outbound fragments are sized by: the exchanged
    /// one, capped by the interface's `fragment_mtu` when set. Applied
    /// where BlueZ reports an MTU, so the link table, its fragmenter and
    /// the `BLE_LINK_UP` line all see the same number.
    fn sizing_mtu(&self, exchanged: usize) -> usize {
        self.opts
            .fragment_mtu
            .map_or(exchanged, |cap| exchanged.min(cap))
    }

    fn log_link_up(&self, identity: &IdentityHash, addr: &Addr, role: Role, mtu: usize) {
        tracing::info!(
            event = "BLE_LINK_UP",
            iface = %Scalar(&self.name),
            peer = %hex8(identity),
            addr = %hex12(addr),
            role = role.as_str(),
            mtu = mtu,
        );
    }

    fn log_link_down(&self, identity: &IdentityHash, role: Role, reason: &'static str) {
        tracing::info!(
            event = "BLE_LINK_DOWN",
            iface = %Scalar(&self.name),
            peer = %hex8(identity),
            role = role.as_str(),
            reason = reason,
        );
    }

    /// Record one departure in the churn ledger (#417) and log the
    /// arming, in the firmware's `BLE_CHURN_LEDGER armed` shape. Only
    /// the arming is logged: the departure already has its line
    /// (`BLE_LINK_DOWN reason=timeout`, `BLE_LINK_REPLACED`), and the one
    /// thing those cannot say is that the run has now closed the class.
    /// The class filter is the gate's: a static address records nothing.
    fn note_peer_leave(
        &self,
        churn: &mut links::ChurnGate,
        identity: &IdentityHash,
        addr: &Addr,
        addr_type: Option<AddrType>,
        reason: &'static str,
        now: u64,
    ) {
        let Some(noted) = churn.note_leave(identity, addr, addr_type, now) else {
            return;
        };
        if noted.armed {
            let policy = churn.policy();
            tracing::info!(
                event = "BLE_CHURN_LEDGER",
                iface = %Scalar(&self.name),
                state = "armed",
                identity = %hex8(identity),
                run = noted.run,
                reason = reason,
                window_ms = policy.window_ms,
                cooldown_ms = policy.cooldown_ms,
            );
        }
    }

    /// Whether the churn ledger holds this sighting out of the window
    /// (#417): a fallback verdict at a rotating address while the
    /// cooldown stands. Logs the firmware's `BLE_CHURN_LEDGER held`
    /// line once per cooldown.
    fn churn_held(
        &self,
        churn: &mut links::ChurnGate,
        addr: &Addr,
        addr_type: Option<AddrType>,
        decision: leviculum_ble_tx::ConnectDecision,
        now: u64,
    ) -> bool {
        let Some(held) = churn.holds(addr, addr_type, decision, now) else {
            return false;
        };
        if held.fresh {
            tracing::info!(
                event = "BLE_CHURN_LEDGER",
                iface = %Scalar(&self.name),
                state = "held",
                run = churn.policy().leave_run,
                until_ms = held.until_ms,
                now_ms = now,
            );
        }
        true
    }

    /// A duplicate identity we resolved by replacing the old link
    /// (#376/#360). The firmware's `BLE_LINK_REPLACED`,
    /// key for key, so one grep reads a mixed capture: `origin=` names
    /// who opened the connection that won (reported, not consulted),
    /// `old_silence_ms=` how long the loser had delivered nothing at
    /// all, `old_data_silence_ms=` how long since it carried real
    /// payload — the number the decision turned on, `never` for a link
    /// that carried none.
    fn log_link_replaced(&self, identity: &IdentityHash, addr: &Addr, role: Role, old: &Displaced) {
        tracing::info!(
            event = "BLE_LINK_REPLACED",
            iface = %Scalar(&self.name),
            peer = %hex8(identity),
            addr = %hex12(addr),
            rule = old.rule.as_str(),
            origin = role.origin_as_str(),
            old_role = old.role.as_str(),
            old_mtu = old.old_usable_mtu,
            new_mtu = old.new_usable_mtu,
            old_silence_ms = old.silence_ms,
            old_data_silence_ms = %leviculum_ble_tx::DataSilence(old.data_silence_ms),
        );
    }

    /// `listed` is how many peers `accept_only` names — the number that
    /// tells a reader of `BLE_LINK_NOT_ADMITTED` that a list is in
    /// force at all, and how big it is.
    fn log_rejection(
        &self,
        admission: Admission,
        role: Role,
        identity: &[u8],
        addr: &Addr,
        listed: usize,
    ) {
        match admission {
            Admission::RejectSelf => tracing::warn!(
                event = "BLE_LINK_SELF",
                addr = %hex12(addr),
                action = "disconnect",
            ),
            Admission::RejectDuplicate {
                rule,
                old_silence_ms,
                old_data_silence_ms,
                old_usable_mtu,
                new_usable_mtu,
            } => tracing::info!(
                event = "BLE_LINK_DUP",
                iface = %Scalar(&self.name),
                peer = %hex8(identity),
                addr = %hex12(addr),
                action = "refuse",
                rule = rule.as_str(),
                origin = role.origin_as_str(),
                old_mtu = old_usable_mtu,
                new_mtu = new_usable_mtu,
                old_silence_ms = old_silence_ms,
                old_data_silence_ms = %leviculum_ble_tx::DataSilence(old_data_silence_ms),
            ),
            Admission::RejectFull => tracing::info!(
                "BLE {}: link limit reached, rejecting {}",
                self.name,
                hex12(addr)
            ),
            // The one refusal a run has to be able to SEE: without a
            // line here a measuring host that turned strangers away
            // would look exactly like a host nobody tried, and "the
            // room was empty" and "the room was full of refused
            // phones" are not the same measurement. Both spellings the
            // key accepts are printed, so an operator who decides the
            // device belongs in the cell can paste either into
            // `accept_only`.
            Admission::RejectNotAdmitted => tracing::warn!(
                event = "BLE_LINK_NOT_ADMITTED",
                iface = %Scalar(&self.name),
                peer = %hex8(identity),
                identity = %hex_all(identity),
                addr = %hex12(addr),
                role = role.as_str(),
                listed = listed,
                action = "disconnect",
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

/// Drop BlueZ's stored `Device1` object for a peer whose link just went
/// down (#443).
///
/// bluetoothd resolves a connected peer's GATT services into the
/// device's `UUIDs` property and drops them from it again when the
/// unpaired peer disconnects, while the object itself — once connected,
/// no longer temporary — is never cleaned up. From then on every scan
/// window re-offers the stored object (adv reports arrive, RSSI and
/// `ManufacturerData` update), but the probe's `UUIDs` read answers a
/// list without the Columba service, so `decide_from_scan` discards the
/// sighting as a non-peer and a daemon that returns on its old address
/// is never dialled again. Measured on btvirt (2026-10-10, three runs,
/// btmon + full D-Bus monitor): `UUIDs` was `[1800, 1801, 180a,
/// 37145b00…]` while linked and `[1800, 1801, 180a]` ever after.
///
/// Removing the object makes the peer's next advertisement recreate it
/// from the air — advertised UUIDs, fresh record, RSSI — the state a
/// never-met peer starts from. We never bond, so nothing kept is lost.
/// The removal can race the peer's own reconnect; that dial fails its
/// setup once and the peer retries after its backoff.
///
/// `false` when the removal failed because the ADAPTER's object is gone
/// (600: D-Bus `UnknownObject`; a device that is already gone answers
/// `DoesNotExist` instead): bluetoothd keeps the stored object and
/// restores it with the adapter, so the caller owes the removal to the
/// session that finds the adapter again.
async fn forget_device(adapter: &bluer::Adapter, addr: Addr) -> bool {
    !matches!(
        adapter.remove_device(Address(addr)).await,
        Err(bluer::Error {
            kind: bluer::ErrorKind::NotFound,
            ..
        })
    )
}

/// `peer=` value: leading 4 identity-hash bytes as hex, the same hex the
/// firmware logs and the LN-<hex8> name carries.
fn hex8(identity: &[u8]) -> String {
    crate::storage::hex_encode(&identity[..identity.len().min(4)])
}

/// `identity=` value: the whole identity hash as hex. `peer=`'s four
/// bytes are what every other BLE line carries and what the air
/// publishes as a hint; the full hash is printed only where an operator
/// is expected to copy it back into a config key, and `accept_only`
/// takes either spelling.
fn hex_all(identity: &[u8]) -> String {
    crate::storage::hex_encode(identity)
}

/// `addr=` value: the 48-bit address as 12 hex digits in display order,
/// matching the firmware's `addr={:012x}` (which prints the numeric
/// value, i.e. the displayed byte order).
fn hex12(addr: &Addr) -> String {
    crate::storage::hex_encode(addr)
}

/// Aborts a spawned task when dropped, tying its lifetime to the session.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
