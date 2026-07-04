//! Network interfaces
//!
//! Interfaces handle the physical layer communication for Reticulum.
//! Each interface type runs as a spawned tokio task communicating through
//! channels. `InterfaceHandle` represents the event loop's end of the
//! channel pair, and `InterfaceRegistry` manages all active handles.
//!
//! `InterfaceHandle` implements [`leviculum_core::traits::Interface`] so that
//! core's [`dispatch_actions()`](leviculum_core::transport::dispatch_actions)
//! can route packets to interfaces directly.

pub(crate) mod airtime;
pub mod auto_interface;
pub mod hdlc;
pub(crate) mod i2p;
pub(crate) mod kiss;
pub(crate) mod local;
pub(crate) mod netdevice;
pub(crate) mod pipe;
pub(crate) mod rnode;
pub use rnode::{
    RNodeChannelConfig, RNodeChannelFactory, RNodeChannelHalves, RNodeChannelHandle,
    RNodeChannelOpenFuture,
};
pub(crate) mod serial;
pub(crate) mod tcp;
pub use tcp::{disable_fault_injection, enable_fault_injection};
pub(crate) mod udp;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use leviculum_core::traits::{InterfaceError, InterfaceMode};
use leviculum_core::transport::InterfaceId;
use tokio::sync::{mpsc, Notify};

use self::airtime::AirtimeCredit;

/// Monotonic wall-clock in milliseconds since the process-local anchor.
///
/// CRITICAL: this anchor MUST match Transport's SystemClock anchor so
/// that `last_update_ms` values stored by the credit bucket
/// (via try_send_prioritized) and `now_ms` values read by the retry
/// scheduler (via interface_next_slot_ms) are in the same frame.
/// A drift of even a few seconds flips `earliest_fit_time` into
/// returning "ready now" when the bucket is actually in deficit,
/// silently defeating retry deferral.
///
/// `init_clock_anchor` must be called from the driver with the
/// Transport SystemClock's start instant BEFORE any `now_ms()` call.
/// If `init_clock_anchor` was never called, we fall back to anchoring
/// at first `now_ms()` invocation, this is safe for unit tests that
/// never touch Transport's clock.
static CLOCK_ANCHOR: OnceLock<Instant> = OnceLock::new();

pub(crate) fn init_clock_anchor(anchor: Instant) {
    // First writer wins. If the driver calls this before any TX,
    // Transport and the bucket share a frame. If the bucket saw a
    // try_send BEFORE the driver called this, the OnceLock already
    // holds the bucket's fallback anchor, the driver's call is a
    // no-op. That's a programming error (driver must init first) but
    // does no harm: both frames are still self-consistent, just
    // offset by <1s.
    let _ = CLOCK_ANCHOR.set(anchor);
}

fn now_ms() -> u64 {
    let boot = CLOCK_ANCHOR.get_or_init(Instant::now);
    boot.elapsed().as_millis() as u64
}

/// Speed sampling state, updated every second by the traffic counter task.
struct SpeedState {
    prev_rx: u64,
    prev_tx: u64,
    prev_time: Instant,
    cached_rxs: f64,
    cached_txs: f64,
}

/// Latest radio statistics reported by an RNode over KISS `CMD_STAT_*`
/// (Codeberg #25).
///
/// Field names and units mirror Python `RNodeInterface`'s `r_*` attributes as
/// surfaced through `Reticulum.get_interface_stats()` (Reticulum.py:1371-1420),
/// so rnstatus/lnstatus render the radio rows without special-casing:
///
/// - `airtime_short`/`airtime_long`, `channel_load_short`/`channel_load_long`:
///   percent (raw device u16 / 100.0), default `0.0` before the first report.
/// - `noise_floor`: dBm (`raw - 157`), `None` until the device reports it.
/// - `cpu_temp`: temperature in Celsius (`raw - 120`), clamped to `[-30, 90]`
///   and `None` outside that range (Python `r_temperature`/`cpu_temp`).
/// - `battery_state`/`battery_percent`: only surfaced once the state leaves
///   `Unknown` (Python only emits the keys when `r_battery_state != 0x00`).
/// - `last_rssi`/`last_snr`: most recent `CMD_STAT_RSSI` (dBm) and
///   `CMD_STAT_SNR` (dB, `raw * 0.25`). Stored for completeness; Python does
///   not place these in the `interface_stats` dict (they feed per-packet RSSI/
///   SNR reporting), so they are not emitted there either.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RadioStats {
    pub airtime_short: f64,
    pub airtime_long: f64,
    pub channel_load_short: f64,
    pub channel_load_long: f64,
    pub noise_floor: Option<i16>,
    pub cpu_temp: Option<i16>,
    pub battery_state: leviculum_core::rnode::BatteryState,
    pub battery_percent: u8,
    pub last_rssi: Option<i16>,
    pub last_snr: Option<f64>,
}

/// Shared I/O counters for an interface, readable from the RPC handler.
///
/// Created by each interface spawn function, cloned into the I/O task.
/// The RPC handler reads these via `InterfaceStatsMap`.
///
/// `rx_bytes`/`tx_bytes` are written by I/O tasks (lock-free atomics).
/// `speed` is updated every second by a background task (see
/// `spawn_traffic_counter`) and read by the RPC handler.
///
/// `radio` holds the latest RNode `CMD_STAT_*` values (Codeberg #25). It is
/// `None` for non-radio interfaces (TCP/UDP/Auto/Local); RNode interfaces set
/// it to `Some(RadioStats::default())` at spawn so the stats keys are always
/// present (mirroring Python's `hasattr(interface, "r_airtime_short")` gate).
pub(crate) struct InterfaceCounters {
    pub rx_bytes: AtomicU64,
    pub tx_bytes: AtomicU64,
    speed: std::sync::Mutex<SpeedState>,
    radio: std::sync::Mutex<Option<RadioStats>>,
}

impl InterfaceCounters {
    pub(crate) fn new() -> Self {
        Self {
            rx_bytes: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            speed: std::sync::Mutex::new(SpeedState {
                prev_rx: 0,
                prev_tx: 0,
                prev_time: Instant::now(),
                cached_rxs: 0.0,
                cached_txs: 0.0,
            }),
            radio: std::sync::Mutex::new(None),
        }
    }

    /// Mark this interface as radio-capable so `interface_stats` always emits
    /// the RNode radio keys (with their `0.0`/`None` defaults) even before the
    /// first `CMD_STAT_*` frame arrives. Called once at RNode spawn.
    pub(crate) fn enable_radio_stats(&self) {
        let mut guard = self.radio.lock().unwrap();
        if guard.is_none() {
            *guard = Some(RadioStats::default());
        }
    }

    /// Apply an update to the stored radio stats, creating the record if the
    /// interface was not pre-marked. Called by the RNode I/O task on each
    /// parsed `CMD_STAT_*` frame.
    pub(crate) fn update_radio(&self, f: impl FnOnce(&mut RadioStats)) {
        let mut guard = self.radio.lock().unwrap();
        f(guard.get_or_insert_with(RadioStats::default));
    }

    /// Snapshot the latest radio stats, or `None` for non-radio interfaces.
    pub(crate) fn radio_stats(&self) -> Option<RadioStats> {
        *self.radio.lock().unwrap()
    }

    /// Sample current byte counters and recompute cached speeds.
    ///
    /// Called every second by the traffic counter task. Formula matches
    /// Python's `count_traffic_loop`: `(byte_diff * 8) / time_diff`.
    pub(crate) fn update_speed(&self) {
        let mut state = self.speed.lock().unwrap();
        let now = Instant::now();
        let elapsed = now.duration_since(state.prev_time).as_secs_f64();
        if elapsed > 0.0 {
            let rx = self.rx_bytes.load(Ordering::Relaxed);
            let tx = self.tx_bytes.load(Ordering::Relaxed);
            state.cached_rxs = (rx.saturating_sub(state.prev_rx) as f64 * 8.0) / elapsed;
            state.cached_txs = (tx.saturating_sub(state.prev_tx) as f64 * 8.0) / elapsed;
            state.prev_rx = rx;
            state.prev_tx = tx;
            state.prev_time = now;
        }
    }

    /// Return the cached rx/tx speeds in bits per second.
    ///
    /// Returns the values last computed by `update_speed()`.
    pub(crate) fn speeds(&self) -> (f64, f64) {
        let state = self.speed.lock().unwrap();
        (state.cached_rxs, state.cached_txs)
    }
}

/// Spawn a background worker that samples interface byte counters every second
/// and updates cached speeds. Mirrors Python's `Transport.count_traffic_loop()`.
///
/// Runs on a dedicated OS thread rather than a `tokio::spawn`d task: the work
/// is purely synchronous (atomic loads + a `std::sync::Mutex`), and using
/// `std::thread::sleep` instead of `tokio::time::interval` removes any
/// dependency on the host runtime's time driver. That dependency was a bug —
/// an embedder runtime built without `enable_time()` made `interval()` panic
/// on first poll, killing the counter. A library housekeeping task must not
/// assume how the host wired its runtime.
///
/// The thread holds only a `Weak` reference to the stats map, so it
/// self-terminates within ~1 s of the owning node dropping its last strong
/// reference (the driver retains `self.iface_stats_map`) — no leaked thread
/// per node.
pub(crate) fn spawn_traffic_counter(iface_stats_map: InterfaceStatsMap) {
    let stats = Arc::downgrade(&iface_stats_map);
    drop(iface_stats_map); // don't keep the map alive ourselves
    let spawned = std::thread::Builder::new()
        .name("reticulum-traffic-counter".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let Some(map) = stats.upgrade() else {
                break; // owning node dropped — nothing left to sample
            };
            for counters in map.lock().unwrap().values() {
                counters.update_speed();
            }
        });
    if let Err(e) = spawned {
        // Speed reporting is observability-only; degrade rather than abort.
        tracing::warn!("traffic-counter thread not spawned: {e}");
    }
}

/// Shared map of interface counters, keyed by interface ID index.
///
/// Populated by the event loop when handles are registered.
/// Read by the RPC handler for byte counter reporting.
pub(crate) type InterfaceStatsMap =
    Arc<std::sync::Mutex<std::collections::BTreeMap<usize, Arc<InterfaceCounters>>>>;

/// Shared map of per-interface online status, keyed by interface ID index.
///
/// Populated by the driver when an interface registers (`true`) and updated
/// on disconnect (entry removed alongside the stats entry — once the core
/// also drops the interface name, the RPC layer's `interface_stats`
/// enumeration won't visit the interface anyway). The RPC handler reads
/// this to thread real `is_online()` into the `status` field of the
/// `interface_stats` response (Codeberg #56). A missing entry falls back
/// to `true` — preserves the pre-fix behavior for any caller-side mismatch.
pub(crate) type InterfaceOnlineMap = Arc<std::sync::Mutex<std::collections::BTreeMap<usize, bool>>>;

/// Per-interface readiness signal.
///
/// Created by each interface spawn function and shared with the
/// driver via the `InterfaceReadyMap`.  Owners of an `Arc<ReadySignal>`
/// can:
///
/// - Call [`signal_ready`](Self::signal_ready) once the interface has
///   reached its readiness condition (TCP client: connect Ok; UDP /
///   server / immediate-ready interfaces: at construction).
/// - Call [`wait`](Self::wait) to await the readiness condition with
///   a timeout.
/// - Call [`is_ready`](Self::is_ready) for a non-blocking check.
///
/// The readiness contract per interface type is documented on
/// [`crate::driver::ReticulumNode::wait_for_interface_ready`].
pub struct ReadySignal {
    flag: AtomicBool,
    notify: Notify,
}

impl ReadySignal {
    /// Create a new `not yet ready` signal.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            flag: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    /// Create a signal that is already in the ready state.  Used by
    /// interface types whose readiness is established synchronously
    /// at construction (TCP server listener, UDP socket, local IPC
    /// shared-instance client) — the spawn function can return a
    /// pre-signaled handle.
    pub fn ready_immediate() -> Arc<Self> {
        let s = Self {
            flag: AtomicBool::new(true),
            notify: Notify::new(),
        };
        Arc::new(s)
    }

    /// Mark the interface ready and wake all waiters.
    ///
    /// Idempotent — repeat calls are no-ops once the flag is set.
    pub fn signal_ready(&self) {
        if !self.flag.swap(true, Ordering::Release) {
            self.notify.notify_waiters();
        }
    }

    /// Non-blocking readiness check.
    pub fn is_ready(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Wait for the interface to become ready, or return `Err` after
    /// `timeout` elapses.  Returns immediately if already ready.
    pub async fn wait(&self, timeout: Duration) -> Result<(), tokio::time::error::Elapsed> {
        if self.is_ready() {
            return Ok(());
        }
        // Register the waiter BEFORE the second flag check to avoid a
        // race where signal_ready runs between the first check and
        // notified.await — Notify::notified() is permit-aware so a
        // wakeup that arrives between registration and await is still
        // delivered, but the second is_ready re-check covers the
        // permit-already-consumed case for callers waking after the
        // signal has been observable.
        let notified = self.notify.notified();
        if self.is_ready() {
            return Ok(());
        }
        tokio::time::timeout(timeout, notified).await
    }
}

/// Shared map of interface readiness signals, keyed by interface ID
/// index.  Populated by the driver when interfaces are spawned;
/// consumed by [`crate::driver::ReticulumNode::wait_for_interface_ready`]
/// and friends.
pub(crate) type InterfaceReadyMap =
    Arc<std::sync::Mutex<std::collections::BTreeMap<usize, Arc<ReadySignal>>>>;

/// Packet received from an interface, ready for the event loop
pub(crate) struct IncomingPacket {
    pub data: Vec<u8>,
}

/// Packet to send out through an interface
pub(crate) struct OutgoingPacket {
    pub data: Vec<u8>,
    /// High-priority packets (link requests, proofs, channel data) are sent
    /// before normal-priority packets (announce rebroadcasts) on constrained
    /// interfaces like LoRa. Read by RNode send queue (behind `serial` feature).
    pub high_priority: bool,
}

/// Metadata describing a registered interface
pub(crate) struct InterfaceInfo {
    pub id: InterfaceId,
    pub name: String,
    /// Hardware MTU for link MTU negotiation (e.g., TCP=262144, UDP=1064).
    /// `None` means the interface uses the base protocol MTU (500).
    pub hw_mtu: Option<u32>,
    /// Whether this interface is a local IPC client (shared instance).
    /// Local clients receive announce forwarding and path request routing.
    pub is_local_client: bool,
    /// On-air bitrate in bits/sec (e.g., LoRa ~5468 bps for SF7/CR5/BW125kHz).
    /// `None` for interfaces without a fixed bitrate (TCP, UDP).
    pub bitrate: Option<u32>,
    /// IFAC config inherited from the parent interface (e.g., TCP server listener).
    /// When a TCP server accepts a connection, the child interface inherits the
    /// parent's IFAC config so that IFAC verification/application works on the
    /// dynamically-created interface.
    pub ifac: Option<leviculum_core::ifac::IfacConfig>,
    /// Reticulum propagation mode for this interface (Codeberg #91/#104).
    /// Almost always `Full`; a TCP server listener passes its configured mode
    /// down to each accepted (spawned) child so the inbound-side mode rules
    /// apply to peers connecting to an AP/roaming/etc. server, mirroring Python
    /// `spawned_interface.mode = self.mode` (TCPInterface.py:625). The driver
    /// hands this value to `Transport::set_interface_mode` when the interface is
    /// registered.
    pub mode: InterfaceMode,
}

/// Event loop's handle to a spawned interface task
pub(crate) struct InterfaceHandle {
    pub info: InterfaceInfo,
    pub incoming: mpsc::Receiver<IncomingPacket>,
    pub outgoing: mpsc::Sender<OutgoingPacket>,
    pub counters: Arc<InterfaceCounters>,
    /// Airtime budget for interfaces whose capacity is constrained by the
    /// radio physics (currently only LoRa-Serial). `None` for TCP, UDP,
    /// Local, RNode, AutoInterface, those are "always ready" from the
    /// backpressure-layer's perspective. Consumed by
    /// `try_send_prioritized` (credit-charge) and by Phase B4's
    /// `next_slot_ms` override. See `airtime.rs` for the bucket model.
    pub credit: Option<Arc<Mutex<AirtimeCredit>>>,
    /// Readiness signal — fires when the interface has completed any
    /// connection / bind / handshake step it needs before it can route
    /// packets.  TCP-client interfaces fire on connect Ok; immediate-
    /// ready interfaces (UDP, server listeners, local IPC) ship as
    /// pre-signaled.  Read by `ReticulumNode::wait_for_interface_ready`.
    pub ready: Arc<ReadySignal>,
}

impl leviculum_core::traits::Interface for InterfaceHandle {
    fn id(&self) -> InterfaceId {
        self.info.id
    }
    fn name(&self) -> &str {
        &self.info.name
    }
    fn mtu(&self) -> usize {
        leviculum_core::constants::MTU
    }
    fn mode(&self) -> InterfaceMode {
        self.info.mode
    }
    fn is_online(&self) -> bool {
        !self.outgoing.is_closed()
    }
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        self.try_send_prioritized(data, false)
    }
    fn try_send_prioritized(
        &mut self,
        data: &[u8],
        high_priority: bool,
    ) -> Result<(), InterfaceError> {
        // First: airtime-credit check for constrained interfaces. LoRa-Serial
        // populates `credit`; TCP/UDP/Local leave it `None` and skip the
        // charge entirely. See `airtime.rs` for the bucket semantics.
        if let Some(credit) = &self.credit {
            let mut c = credit.lock().expect("airtime credit mutex poisoned");
            if c.try_charge(data.len() as u32, now_ms()).is_err() {
                return Err(InterfaceError::BufferFull);
            }
        }
        match self.outgoing.try_send(OutgoingPacket {
            data: data.to_vec(),
            high_priority,
        }) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Err(InterfaceError::BufferFull),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(InterfaceError::Disconnected)
            }
        }
    }

    fn next_slot_ms(&self, size: usize, now_ms: u64) -> u64 {
        // LoRa-Serial: ask the credit bucket when it will next fit a
        // packet of this size. TCP/UDP/Local have credit == None and
        // fall through to the trait default's always-ready semantics.
        match &self.credit {
            Some(credit) => credit
                .lock()
                .expect("airtime credit mutex poisoned")
                .earliest_fit_time(size as u32, now_ms),
            None => now_ms,
        }
    }
}

/// Registry of active interface handles with round-robin polling
pub(crate) struct InterfaceRegistry {
    handles: Vec<InterfaceHandle>,
    /// Round-robin start index to prevent busy interfaces from starving others
    poll_start: usize,
}

impl InterfaceRegistry {
    /// Create an empty registry
    pub(crate) fn new() -> Self {
        Self {
            handles: Vec::new(),
            poll_start: 0,
        }
    }

    /// Register a new interface handle
    pub(crate) fn register(&mut self, handle: InterfaceHandle) {
        self.handles.push(handle);
    }

    /// Remove an interface by ID, returns true if found
    pub(crate) fn remove(&mut self, id: InterfaceId) -> bool {
        let before = self.handles.len();
        self.handles.retain(|h| h.info.id != id);
        let removed = self.handles.len() < before;
        if removed && !self.handles.is_empty() {
            self.poll_start %= self.handles.len();
        } else if self.handles.is_empty() {
            self.poll_start = 0;
        }
        removed
    }

    /// Whether the registry has no interfaces
    pub(crate) fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    /// Get the name of an interface by ID
    pub(crate) fn name_of(&self, id: InterfaceId) -> &str {
        self.handles
            .iter()
            .find(|h| h.info.id == id)
            .map(|h| h.info.name.as_str())
            .unwrap_or("unknown")
    }

    /// Immutable slice of all handles
    pub(crate) fn handles(&self) -> &[InterfaceHandle] {
        &self.handles
    }

    /// Mutable access to handles and poll_start for recv_any
    pub(crate) fn handles_mut(&mut self) -> (&mut Vec<InterfaceHandle>, &mut usize) {
        (&mut self.handles, &mut self.poll_start)
    }

    /// Mutable slice of all handles for dispatch_actions()
    pub(crate) fn handles_mut_slice(&mut self) -> &mut [InterfaceHandle] {
        &mut self.handles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a bare-bones InterfaceHandle plus the kept-alive receiver
    /// for the outgoing channel. The receiver must stay in scope for
    /// the duration of any `try_send_prioritized` test, otherwise the
    /// channel closes and `try_send` returns `Disconnected`.
    ///
    /// Invariant: `credit` defaults to `None`; non-LoRa interfaces leave
    /// the bucket empty, which makes the `next_slot_ms` override return
    /// `now_ms` (always ready) for them.
    fn make_handle(id: usize) -> (InterfaceHandle, mpsc::Receiver<OutgoingPacket>) {
        let (_inc_tx, inc_rx) = mpsc::channel(4);
        let (out_tx, out_rx) = mpsc::channel(4);
        let handle = InterfaceHandle {
            info: InterfaceInfo {
                id: InterfaceId(id),
                name: format!("test-{id}"),
                hw_mtu: None,
                is_local_client: false,
                bitrate: None,
                ifac: None,
                mode: leviculum_core::traits::InterfaceMode::default(),
            },
            incoming: inc_rx,
            outgoing: out_tx,
            counters: Arc::new(InterfaceCounters::new()),
            credit: None,
            ready: ReadySignal::ready_immediate(),
        };
        (handle, out_rx)
    }

    #[test]
    fn interface_handle_defaults_to_no_credit() {
        let (h, _rx) = make_handle(7);
        assert!(h.credit.is_none());
    }

    /// Regression: `spawn_traffic_counter` must not panic when the host
    /// runtime lacks a time driver.
    ///
    /// The old implementation built `tokio::time::interval` inside a
    /// `tokio::spawn`; under an embedder runtime built without
    /// `enable_time()` (e.g. a `new_current_thread().build()` with no time
    /// feature), the first poll panicked with "timers are disabled". The
    /// panic fired the embedder's panic hook and killed the counter task.
    /// A library housekeeping task must tolerate any host runtime, so this
    /// builds exactly that runtime and asserts zero panics escape.
    #[test]
    fn spawn_traffic_counter_survives_runtime_without_time_driver() {
        use std::collections::BTreeMap;
        use std::sync::atomic::AtomicUsize;

        // The panic hook is process-global and the std lib suite runs
        // multi-threaded, so the swapped hook must neither count an
        // UNRELATED test's panic (false RED here) nor swallow its
        // report. Both panic sources this regression guards run on
        // known-named threads — the pre-fix interval panic fires on
        // THIS test's thread (current-thread runtime inside block_on;
        // libtest names it after the test), the post-fix counter thread
        // is named "reticulum-traffic-counter" — so count only those
        // and delegate everything else to the previous hook. The
        // previous hook is restored exactly afterwards (Arc round-trip:
        // our installed closure holds the only other clone). No other
        // test in this workspace swaps the hook.
        let panics = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&panics);
        type PrevHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send>;
        let prev_hook: Arc<PrevHook> = Arc::new(std::panic::take_hook());
        let prev_for_hook = Arc::clone(&prev_hook);
        std::panic::set_hook(Box::new(move |info| {
            let thread = std::thread::current();
            let name = thread.name().unwrap_or("");
            if name.contains("spawn_traffic_counter_survives")
                || name.starts_with("reticulum-traffic-counter")
            {
                counter.fetch_add(1, Ordering::SeqCst);
            } else {
                prev_for_hook(info);
            }
        }));

        // Deliberately NO `.enable_time()` — mirrors the embedder runtime
        // that surfaced the bug. (`#[tokio::test]` would enable_all and hide
        // it.)
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build current-thread runtime");
        let map: InterfaceStatsMap = Arc::new(Mutex::new(BTreeMap::new()));
        rt.block_on(async move {
            spawn_traffic_counter(Arc::clone(&map));
            // Pump the scheduler so any spawned task is polled at least once;
            // the pre-fix `interval` panics on that first poll.
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }
        });

        // Uninstall our hook (drops its Arc clone), then put the
        // original hook back exactly.
        drop(std::panic::take_hook());
        if let Ok(prev) = Arc::try_unwrap(prev_hook) {
            std::panic::set_hook(prev);
        }
        assert_eq!(
            panics.load(Ordering::SeqCst),
            0,
            "spawn_traffic_counter panicked under a runtime without the time driver"
        );
    }

    #[test]
    fn interface_handle_with_credit_attached_is_some() {
        let (mut h, _rx) = make_handle(8);
        let credit = AirtimeCredit::new(125_000, 10, 8, 500);
        h.credit = Some(Arc::new(Mutex::new(credit)));
        assert!(h.credit.is_some());
    }

    /// try_send_prioritized on a handle without a credit bucket behaves
    /// identically to the pre-B3 code path, pure mpsc dispatch.
    #[test]
    fn try_send_without_credit_goes_straight_to_mpsc() {
        use leviculum_core::traits::Interface;
        let (mut h, _rx) = make_handle(9);
        assert!(h.credit.is_none());
        h.try_send_prioritized(&[1, 2, 3, 4], false)
            .expect("no credit → should succeed via mpsc");
    }

    /// try_send_prioritized on a LoRa handle with fresh credit succeeds
    /// and charges the bucket.
    ///
    /// Determinism note: the original version of this test read the bucket
    /// back via `current(now_ms())` and asserted `< 0`, relying on the global
    /// `CLOCK_ANCHOR` still being near zero. That is not portable. `now_ms()`
    /// is "ms since the process first read the anchor", and the bucket starts
    /// at `last_update_ms = 0`, so by the time the charge runs the bucket has
    /// silently regenerated `now_ms()` worth of idle credit — clamped at
    /// `max_credit_ms`. Once `now_ms()` ≳ a 50-byte SF10 packet cost, the
    /// accrued idle credit covers the charge and the post-charge balance is no
    /// longer negative; once `now_ms()` ≥ `max_credit_ms` the clamp pins it
    /// positive outright. On Windows MSVC (slower/coarser anchor init, plus a
    /// different test-binary startup cost) `now_ms()` was large enough at this
    /// point that the assertion flipped. The bug was the test's hidden
    /// dependence on a global, process-age-dependent clock — not the airtime
    /// logic, which is correct.
    ///
    /// Fix: charge at an explicit, fixed timestamp so no idle credit accrues,
    /// then observe at that same timestamp. This still drives the real
    /// `try_send_prioritized` path (the bucket is seeded so its baseline
    /// equals the clock instant), giving a deterministic deficit on every
    /// platform.
    #[test]
    fn try_send_with_fresh_credit_charges_bucket() {
        use leviculum_core::traits::Interface;
        let (mut h, _rx) = make_handle(10);
        let mut credit = AirtimeCredit::new(125_000, 10, 8, 500);
        // Anchor the bucket's baseline to "now" so it carries zero idle credit
        // when `try_send_prioritized` charges it via the global clock. Without
        // this, `current()` would add `now_ms()` ms of regenerated credit
        // (clamped at max_credit_ms) and the charge could land non-negative.
        credit.seed_last_update_ms(now_ms());
        h.credit = Some(Arc::new(Mutex::new(credit)));
        h.try_send_prioritized(&[0u8; 50], true)
            .expect("fresh credit + small packet → Ok");
        // Observe at the charge instant (`last_update_ms`): `current()` returns
        // `credit_ms` with no regen term, reflecting exactly what was deducted.
        let bucket = h.credit.as_ref().unwrap().lock().unwrap();
        let charged_at = bucket.last_update_ms();
        let current_ms = bucket.current(charged_at);
        assert!(
            current_ms < 0,
            "expected credit to be in deficit after charge, got {current_ms}"
        );
    }

    /// Empirical proof that the pre-fix observation was clock-fragile and that
    /// the seed fixes it. Reproduces both the old and the new observation at an
    /// explicit large clock value instead of relying on process age, so the
    /// flip is demonstrated deterministically.
    ///
    /// A 50-byte SF10 packet costs 887 ms of airtime. A fresh bucket left at
    /// `last_update_ms = 0` banks `now_ms()` worth of idle credit, so once the
    /// clock passes 887 ms the post-charge balance is no longer negative and
    /// the old `< 0` assertion would have failed.
    #[test]
    fn pre_fix_observation_is_clock_fragile_seed_fixes_it() {
        use leviculum_core::rnode::airtime_ms;

        let cost = airtime_ms(50, 125_000, 10, 8) as i64;
        // A clock past the 50-byte packet cost: the regime the old test
        // silently entered the later it ran in the suite.
        let large_now = cost as u64 + 1;

        // Old path: fresh bucket (last_update_ms = 0), charge via the clock,
        // then re-read current() the way the original test did.
        let mut old_bucket = AirtimeCredit::new(125_000, 10, 8, 500);
        old_bucket
            .try_charge(50, large_now)
            .expect("charge succeeds");
        let old_observed = old_bucket.current(large_now);
        assert!(
            old_observed >= 0,
            "pre-fix observation should go non-negative under a large clock \
             (the latent flake), got {old_observed}"
        );

        // New path: seed the baseline to the clock so no idle credit accrues.
        // Same large clock now yields a deterministic deficit.
        let mut new_bucket = AirtimeCredit::new(125_000, 10, 8, 500);
        new_bucket.seed_last_update_ms(large_now);
        new_bucket
            .try_charge(50, large_now)
            .expect("charge succeeds");
        let charged_at = new_bucket.last_update_ms();
        let new_observed = new_bucket.current(charged_at);
        assert!(
            new_observed < 0,
            "seeded observation should be a deterministic deficit at any clock \
             value, got {new_observed}"
        );
        // The deficit is exactly the packet cost: no regen, no clock leakage.
        assert_eq!(new_observed, -cost);
    }

    /// LoRa handle with a fresh credit bucket at `now_ms` > 0 reports
    /// the interface as ready (the bucket has already regenerated past
    /// the small packet's cost thanks to the wall-clock baseline).
    #[test]
    fn next_slot_ms_lora_fresh_is_ready() {
        use leviculum_core::traits::Interface;
        let (mut h, _rx) = make_handle(20);
        let credit = AirtimeCredit::new(125_000, 10, 8, 500);
        h.credit = Some(Arc::new(Mutex::new(credit)));
        // A fresh bucket with no charge history: enough credit has
        // regenerated by `now_ms = 20_000` to fit a 50-byte packet.
        assert_eq!(h.next_slot_ms(50, 20_000), 20_000);
    }

    /// LoRa handle after a full-MTU charge reports a non-now slot.
    #[test]
    fn next_slot_ms_lora_saturated_returns_future() {
        use leviculum_core::traits::Interface;
        let (mut h, _rx) = make_handle(21);
        let mut credit = AirtimeCredit::new(125_000, 10, 8, 500);
        credit.try_charge(500, 0).expect("initial charge fits");
        let expected = credit.earliest_fit_time(50, 0);
        h.credit = Some(Arc::new(Mutex::new(credit)));
        // Bucket is at threshold; next_slot_ms at now=0 must match the
        // bucket's own earliest_fit_time for the same payload.
        let slot = h.next_slot_ms(50, 0);
        assert_eq!(slot, expected);
        assert!(
            slot > 0,
            "saturated bucket should yield future slot, got {slot}"
        );
    }

    /// Non-LoRa handle (credit == None) returns now_ms verbatim.    /// the Interface trait's default semantic.
    #[test]
    fn next_slot_ms_non_lora_returns_now() {
        use leviculum_core::traits::Interface;
        let (h, _rx) = make_handle(22);
        assert!(h.credit.is_none());
        assert_eq!(h.next_slot_ms(500, 9_999), 9_999);
    }

    /// A bucket manually exhausted to exactly threshold causes the next
    /// try_send_prioritized to return BufferFull without touching the
    /// mpsc. Case 4 (regen recovery) is covered by
    /// `airtime::tests::try_charge_succeeds_after_regen_wait`.
    #[test]
    fn try_send_with_exhausted_credit_returns_buffer_full() {
        use leviculum_core::traits::Interface;
        let (mut h, _rx) = make_handle(11);
        let mut credit = AirtimeCredit::new(125_000, 10, 8, 500);
        // Charge a full MTU at t=0 so `current() == threshold_ms`; any
        // subsequent charge at t≈0 has to fail.
        credit.try_charge(500, 0).expect("first charge should fit");
        h.credit = Some(Arc::new(Mutex::new(credit)));

        // Immediate follow-up: now_ms() is only slightly > 0, far below the
        // regeneration needed to accept another charge. Expect BufferFull.
        let err = h
            .try_send_prioritized(&[0u8; 500], true)
            .expect_err("exhausted credit → BufferFull");
        assert!(matches!(err, InterfaceError::BufferFull));
    }
}
