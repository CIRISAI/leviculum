//! The per-node egress scheduler (+ciris; `docs/egress-scheduler.md`).
//!
//! One queue per node between `dispatch_actions` and the per-interface
//! channels, shared by every [`InterfaceHandle`](crate::interfaces) of a
//! network kind (TCP, UDP, punched direct UDP, I2P, Auto, byte channels).
//! It does three things the interface channels cannot, because each of them
//! is a plain FIFO the kernel then drains in arrival order:
//!
//! * **priority**: strict, by [`EgressClass`] (Control and Proof, then
//!   LinkSetup, Audio, Video, Announce, Bulk), with a floor that guarantees
//!   Announce and Bulk together a minimum share so they never starve;
//! * **pacing** at a rate the owner sets ([`EgressScheduler::set_rate`]);
//!   with none set nothing is paced and packets leave as fast as the
//!   interfaces take them;
//! * **head-drop by age**: a media packet older than its class's bound when
//!   it reaches the head is dropped and counted, never sent.
//!
//! It is always on. What replaces an off switch is the idle property, held
//! by unit witnesses below: with nothing queued a packet goes straight to
//! its interface's channel on the same call, in order, and with no rate set
//! nothing is ever queued (an interface whose channel is full refuses the
//! packet with `BufferFull` exactly as before).
//!
//! Radio and local kinds (serial, RNode, KISS, BLE, pipe, local IPC) bypass
//! it: they carry their own airtime credit and priority queue, and pacing a
//! loopback client at the uplink's rate would be wrong.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use leviculum_core::egress::{classify_frame, FrameClass, FrameHint};
use leviculum_core::traits::{InterfaceError, InterfaceKind};
use leviculum_core::transport::InterfaceId;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::Notify;

use crate::interfaces::OutgoingPacket;
use crate::sync_ext::MutexRecover;

/// How many classes there are; the length of every per-class array here.
pub const EGRESS_CLASSES: usize = 7;

/// The scheduling class of an outbound frame, in priority order (highest
/// first). Control and Proof share the top tier and are served oldest
/// first between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EgressClass {
    /// Channel messages, requests, responses, plain data.
    Control,
    /// Every PROOF packet.
    Proof,
    /// Link requests, RTT, identify, close, keepalive.
    LinkSetup,
    /// A link datagram the owner tagged audio.
    Audio,
    /// A link datagram tagged video, or not tagged at all.
    Video,
    /// Announces and path requests.
    Announce,
    /// Resource transfer.
    Bulk,
}

impl EgressClass {
    /// Every class, in priority order; index `i` is the class whose
    /// [`Self::index`] is `i`.
    pub const ALL: [EgressClass; EGRESS_CLASSES] = [
        EgressClass::Control,
        EgressClass::Proof,
        EgressClass::LinkSetup,
        EgressClass::Audio,
        EgressClass::Video,
        EgressClass::Announce,
        EgressClass::Bulk,
    ];

    /// The class's slot in [`EgressDepth::classes`] and
    /// [`EgressStats::classes`].
    pub const fn index(self) -> usize {
        match self {
            EgressClass::Control => 0,
            EgressClass::Proof => 1,
            EgressClass::LinkSetup => 2,
            EgressClass::Audio => 3,
            EgressClass::Video => 4,
            EgressClass::Announce => 5,
            EgressClass::Bulk => 6,
        }
    }

    /// Stable lowercase name, for metrics labels.
    pub const fn name(self) -> &'static str {
        match self {
            EgressClass::Control => "control",
            EgressClass::Proof => "proof",
            EgressClass::LinkSetup => "link_setup",
            EgressClass::Audio => "audio",
            EgressClass::Video => "video",
            EgressClass::Announce => "announce",
            EgressClass::Bulk => "bulk",
        }
    }

    /// Priority tier: lower is served first. Control and Proof share 0.
    const fn tier(self) -> usize {
        match self {
            EgressClass::Control | EgressClass::Proof => 0,
            EgressClass::LinkSetup => 1,
            EgressClass::Audio => 2,
            EgressClass::Video => 3,
            EgressClass::Announce => 4,
            EgressClass::Bulk => 5,
        }
    }

    /// The two lowest tiers, which the floor protects.
    const fn is_low(self) -> bool {
        self.tier() >= LOW_TIER
    }
}

/// Live media kind of a link datagram, an owner's per-send or per-link hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// Scheduled as [`EgressClass::Audio`].
    Audio,
    /// Scheduled as [`EgressClass::Video`].
    Video,
}

impl MediaKind {
    const fn class(self) -> EgressClass {
        match self {
            MediaKind::Audio => EgressClass::Audio,
            MediaKind::Video => EgressClass::Video,
        }
    }
}

/// What one class holds right now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EgressClassDepth {
    /// Packets queued.
    pub queued_packets: u64,
    /// Bytes queued (as handed to the interface: IFAC included, interface
    /// framing not).
    pub queued_bytes: u64,
    /// Age of the oldest queued packet, in milliseconds; 0 when none.
    pub oldest_age_ms: u64,
}

/// The node's egress queue depth, per class. A copy: reading it allocates
/// nothing and holds the scheduler's lock for one pass over the interfaces
/// that have something queued.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EgressDepth {
    /// The pacing rate in force, bits per second; `None` = unpaced.
    pub rate_bps: Option<u64>,
    /// Per class, indexed by [`EgressClass::index`].
    pub classes: [EgressClassDepth; EGRESS_CLASSES],
}

impl EgressDepth {
    /// One class's depth.
    pub fn class(&self, class: EgressClass) -> EgressClassDepth {
        self.classes[class.index()]
    }

    /// Bytes queued over every class.
    pub fn total_bytes(&self) -> u64 {
        self.classes.iter().map(|c| c.queued_bytes).sum()
    }

    /// Age of the oldest queued packet of any class, in milliseconds.
    pub fn oldest_age_ms(&self) -> u64 {
        self.classes
            .iter()
            .map(|c| c.oldest_age_ms)
            .max()
            .unwrap_or(0)
    }
}

/// Cumulative counters of one class since the node was built.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EgressClassCounters {
    /// Packets handed to an interface.
    pub sent_packets: u64,
    /// Bytes handed to an interface.
    pub sent_bytes: u64,
    /// Packets dropped by age at the head of the queue (media only), or
    /// because their interface went away while they were queued.
    pub dropped_packets: u64,
    /// Bytes of [`Self::dropped_packets`].
    pub dropped_bytes: u64,
    /// Packets refused with `BufferFull`: the interface channel was full
    /// with no rate set, or the class's per-interface queue bound was hit.
    pub refused_packets: u64,
}

/// Cumulative scheduler counters. A copy; reading it allocates nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EgressStats {
    /// Per class, indexed by [`EgressClass::index`].
    pub classes: [EgressClassCounters; EGRESS_CLASSES],
    /// Packets that went straight to their interface (nothing ahead of them,
    /// rate allowing), the idle path.
    pub direct_packets: u64,
    /// Packets that waited in the queue.
    pub queued_packets: u64,
}

impl EgressStats {
    /// One class's counters.
    pub fn class(&self, class: EgressClass) -> EgressClassCounters {
        self.classes[class.index()]
    }
}

/// The share the two lowest classes (Announce, Bulk) are guaranteed
/// together, so a full load of anything above them never starves them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EgressFloor {
    /// Percent of the bytes sent (1..=50). Default 10.
    pub share_percent: u8,
    /// An absolute minimum, bits per second, that accrues with time whatever
    /// else is sent (capped at the pacing rate). Default 4000.
    pub min_bps: u64,
}

impl Default for EgressFloor {
    fn default() -> Self {
        Self {
            share_percent: DEFAULT_FLOOR_SHARE_PERCENT,
            min_bps: DEFAULT_FLOOR_MIN_BPS,
        }
    }
}

/// Default floor share for Announce + Bulk.
pub const DEFAULT_FLOOR_SHARE_PERCENT: u8 = 10;
/// Default absolute floor for Announce + Bulk, bits per second.
pub const DEFAULT_FLOOR_MIN_BPS: u64 = 4_000;
/// Default age bound of both media classes: the live-media staleness budget.
pub const DEFAULT_MEDIA_AGE_BOUND: Duration = Duration::from_millis(800);
/// The lowest pacing rate taken; a smaller one is raised to it.
pub const MIN_RATE_BPS: u64 = 1_000;

/// First tier the floor protects (Announce).
const LOW_TIER: usize = 4;
/// Tiers, Control/Proof being one.
const TIERS: usize = 6;
/// The smallest token bucket, bytes: room for a full-MTU frame over UDP
/// and its IFAC.
const BURST_MIN_BYTES: f64 = 1_500.0;
/// The token bucket holds this much of the rate, beyond the minimum.
const BURST_WINDOW: Duration = Duration::from_millis(10);
/// Most the floor credit may bank while the low classes have nothing big
/// waiting, bytes.
const FLOOR_BURST_BYTES: f64 = 1_500.0;
/// How soon the pump looks again at an interface whose channel was full.
const BLOCKED_POLL: Duration = Duration::from_millis(2);
/// Per-send media hints kept for frames not yet dispatched.
const MEDIA_HINTS_MAX: usize = 1_024;
/// Per-link class hints kept at most.
const LINK_HINTS_MAX: usize = 4_096;

/// Bytes one interface may hold queued in one class before the next frame
/// of that class is refused with `BufferFull`, so a peer that stops draining
/// is refused (and its circuit breaker opens) as it was before the
/// scheduler, instead of growing the node's heap.
const fn class_bound_bytes(class: EgressClass) -> usize {
    match class {
        EgressClass::Control | EgressClass::Proof | EgressClass::LinkSetup => 256 * 1024,
        EgressClass::Audio | EgressClass::Video => 128 * 1024,
        EgressClass::Announce => 64 * 1024,
        EgressClass::Bulk => 256 * 1024,
    }
}

/// Whether frames for an interface of `kind` bypass the scheduler.
pub(crate) const fn exempt(kind: InterfaceKind) -> bool {
    matches!(
        kind,
        InterfaceKind::Serial
            | InterfaceKind::Rnode
            | InterfaceKind::Kiss
            | InterfaceKind::Ble
            | InterfaceKind::Pipe
            | InterfaceKind::Local
    )
}

struct Entry {
    pkt: OutgoingPacket,
    len: usize,
    at: Instant,
    /// Arrival order across the node, which settles who is older between
    /// two frames of one instant.
    seq: u64,
    /// Only a frame the header said is a datagram may be dropped by age; a
    /// Channel frame a link hint moved into a media class is not.
    droppable: bool,
}

struct IfaceQueue {
    id: InterfaceId,
    tx: mpsc::Sender<OutgoingPacket>,
    q: [VecDeque<Entry>; EGRESS_CLASSES],
    bytes: [usize; EGRESS_CLASSES],
    blocked_until: Option<Instant>,
}

impl IfaceQueue {
    fn new(id: InterfaceId, tx: mpsc::Sender<OutgoingPacket>) -> Self {
        Self {
            id,
            tx,
            q: Default::default(),
            bytes: [0; EGRESS_CLASSES],
            blocked_until: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.q.iter().all(VecDeque::is_empty)
    }

    /// Anything queued at `tier` or above.
    fn queued_at_or_above(&self, tier: usize) -> bool {
        EgressClass::ALL
            .iter()
            .any(|c| c.tier() <= tier && !self.q[c.index()].is_empty())
    }

    fn blocked(&self, now: Instant) -> bool {
        self.blocked_until.is_some_and(|t| now < t)
    }
}

struct State {
    rate_bps: Option<u64>,
    /// Token bucket, bytes; negative is debt (a frame larger than the
    /// balance went out and the bucket pays it back before the next).
    tokens: f64,
    last_refill: Option<Instant>,
    floor: EgressFloor,
    /// Bytes the low classes may send ahead of higher ones.
    low_credit: f64,
    media_bound: [Duration; 2],
    ifaces: Vec<IfaceQueue>,
    depth_pkts: [u64; EGRESS_CLASSES],
    depth_bytes: [u64; EGRESS_CLASSES],
    stats: EgressStats,
    link_class: HashMap<[u8; 16], EgressClass>,
    media_hint: HashMap<u64, MediaKind>,
    media_hint_order: VecDeque<u64>,
    pump_running: bool,
    next_seq: u64,
}

/// The scheduler. One per node, shared by its interface handles; the public
/// face is the `egress_*` methods of
/// [`ReticulumNode`](crate::driver::ReticulumNode).
pub(crate) struct EgressScheduler {
    state: Mutex<State>,
    notify: Notify,
    me: Weak<EgressScheduler>,
}

/// What [`EgressScheduler::run`] did, for the witnesses.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunOutcome {
    pub sent: usize,
    pub dropped: usize,
    /// When to run again; `None` = nothing queued.
    pub next: Option<Instant>,
}

impl EgressScheduler {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            state: Mutex::new(State {
                rate_bps: None,
                tokens: BURST_MIN_BYTES,
                last_refill: None,
                floor: EgressFloor::default(),
                low_credit: 0.0,
                media_bound: [DEFAULT_MEDIA_AGE_BOUND; 2],
                ifaces: Vec::new(),
                depth_pkts: [0; EGRESS_CLASSES],
                depth_bytes: [0; EGRESS_CLASSES],
                stats: EgressStats::default(),
                link_class: HashMap::new(),
                media_hint: HashMap::new(),
                media_hint_order: VecDeque::new(),
                pump_running: false,
                next_seq: 0,
            }),
            notify: Notify::new(),
            me: me.clone(),
        })
    }

    /// Set the pacing rate, bits per second; `None` stops pacing. A rate
    /// below [`MIN_RATE_BPS`] is raised to it.
    pub(crate) fn set_rate(&self, rate_bps: Option<u64>) {
        let mut st = self.state.lock_recover();
        let now = Instant::now();
        st.refill(now);
        st.rate_bps = rate_bps.map(|r| r.max(MIN_RATE_BPS));
        let cap = st.bucket_cap();
        st.tokens = st.tokens.min(cap);
        drop(st);
        // Queued frames may be free to go now (a raised or removed rate).
        self.notify.notify_one();
    }

    pub(crate) fn rate(&self) -> Option<u64> {
        self.state.lock_recover().rate_bps
    }

    pub(crate) fn set_floor(&self, floor: EgressFloor) {
        let share = floor.share_percent.clamp(1, 50);
        self.state.lock_recover().floor = EgressFloor {
            share_percent: share,
            min_bps: floor.min_bps,
        };
    }

    pub(crate) fn set_media_age_bound(&self, kind: MediaKind, bound: Duration) {
        let i = match kind {
            MediaKind::Audio => 0,
            MediaKind::Video => 1,
        };
        self.state.lock_recover().media_bound[i] = bound;
    }

    /// Set (or with `None` clear) the class a link's Control frames are
    /// scheduled as; for `Audio`/`Video` also the default kind of its
    /// datagrams. Keyed by the link's wire id.
    pub(crate) fn set_link_class(&self, link: [u8; 16], class: Option<EgressClass>) {
        let mut st = self.state.lock_recover();
        match class {
            Some(c) => {
                if st.link_class.len() >= LINK_HINTS_MAX && !st.link_class.contains_key(&link) {
                    // Full of hints nobody cleared: a link closing clears its
                    // own, so this is a leak bound, not a policy.
                    return;
                }
                st.link_class.insert(link, c);
            }
            None => {
                st.link_class.remove(&link);
            }
        }
    }

    /// Forget a closed link's hint.
    pub(crate) fn forget_link(&self, link: &[u8; 16]) {
        let mut st = self.state.lock_recover();
        if !st.link_class.is_empty() {
            st.link_class.remove(link);
        }
    }

    /// Note the media kind of a frame not yet dispatched, by its
    /// [`leviculum_core::egress::frame_fingerprint`].
    pub(crate) fn hint_media(&self, fingerprint: u64, kind: MediaKind) {
        let mut st = self.state.lock_recover();
        if st.media_hint_order.len() >= MEDIA_HINTS_MAX {
            if let Some(old) = st.media_hint_order.pop_front() {
                st.media_hint.remove(&old);
            }
        }
        st.media_hint.insert(fingerprint, kind);
        st.media_hint_order.push_back(fingerprint);
    }

    /// The queue depth now.
    pub(crate) fn depth(&self) -> EgressDepth {
        self.depth_at(Instant::now())
    }

    pub(crate) fn depth_at(&self, now: Instant) -> EgressDepth {
        self.state.lock_recover().depth(now)
    }

    pub(crate) fn stats(&self) -> EgressStats {
        self.state.lock_recover().stats
    }

    /// An interface went away: drop what it had queued, and the sender
    /// clone that kept its channel open.
    pub(crate) fn forget(&self, id: InterfaceId) {
        let mut st = self.state.lock_recover();
        if let Some(i) = st.ifaces.iter().position(|q| q.id == id) {
            let q = st.ifaces.swap_remove(i);
            st.discard(q);
        }
    }

    /// Hand one frame on its way to interface `id` (whose channel is `tx`).
    ///
    /// Straight to the channel when nothing of its class or above is queued
    /// for that interface and the rate allows; queued otherwise. With no
    /// rate set and nothing queued, a full channel refuses it as before.
    pub(crate) fn submit(
        &self,
        id: InterfaceId,
        tx: &mpsc::Sender<OutgoingPacket>,
        hint: Option<FrameHint>,
        pkt: OutgoingPacket,
    ) -> Result<(), InterfaceError> {
        self.submit_at(Instant::now(), id, tx, hint, pkt)
    }

    pub(crate) fn submit_at(
        &self,
        now: Instant,
        id: InterfaceId,
        tx: &mpsc::Sender<OutgoingPacket>,
        hint: Option<FrameHint>,
        pkt: OutgoingPacket,
    ) -> Result<(), InterfaceError> {
        let mut st = self.state.lock_recover();
        let (class, droppable) = st.resolve(hint, &pkt.data);
        let len = pkt.data.len();
        st.refill(now);
        let idx = st.ifaces.iter().position(|q| q.id == id);
        let ahead = idx.is_some_and(|i| st.ifaces[i].queued_at_or_above(class.tier()));
        let paced = st.rate_bps.is_some();
        let rate_ok =
            !paced || (!st.queued_at_or_above(class.tier()) && st.tokens >= 0.0 && !st.floor_due());
        let mut pkt = pkt;
        let mut was_full = false;
        if !ahead && rate_ok {
            match tx.try_send(pkt) {
                Ok(()) => {
                    st.on_sent(class, len);
                    st.stats.direct_packets += 1;
                    return Ok(());
                }
                Err(TrySendError::Closed(_)) => return Err(InterfaceError::Disconnected),
                Err(TrySendError::Full(back)) => {
                    if !paced && idx.is_none() {
                        // Unpaced and nothing queued: the interface's own
                        // backpressure, answered as before the scheduler.
                        st.stats.classes[class.index()].refused_packets += 1;
                        return Err(InterfaceError::BufferFull);
                    }
                    pkt = back;
                    was_full = true;
                }
            }
        }
        let i = match idx {
            Some(i) => i,
            None => {
                st.ifaces.push(IfaceQueue::new(id, tx.clone()));
                st.ifaces.len() - 1
            }
        };
        let c = class.index();
        if st.ifaces[i].bytes[c] + len > class_bound_bytes(class) {
            st.stats.classes[c].refused_packets += 1;
            if st.ifaces[i].is_empty() {
                st.ifaces.swap_remove(i);
            }
            return Err(InterfaceError::BufferFull);
        }
        let seq = st.next_seq;
        st.next_seq += 1;
        let q = &mut st.ifaces[i];
        if was_full {
            q.blocked_until = Some(now + BLOCKED_POLL);
        }
        q.q[c].push_back(Entry {
            pkt,
            len,
            at: now,
            seq,
            droppable,
        });
        q.bytes[c] += len;
        st.depth_pkts[c] += 1;
        st.depth_bytes[c] += len as u64;
        st.stats.queued_packets += 1;
        // The pump sleeps only on tokens or on blocked interfaces; a frame
        // that could go now has to wake it.
        let wake = (!paced || st.tokens >= 0.0) && !st.ifaces[i].blocked(now);
        self.ensure_pump(&mut st);
        drop(st);
        if wake {
            self.notify.notify_one();
        }
        Ok(())
    }

    fn ensure_pump(&self, st: &mut State) {
        if st.pump_running {
            return;
        }
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            st.pump_running = true;
            rt.spawn(pump(self.me.clone()));
        }
    }

    /// Send what may go now; say when to look again.
    pub(crate) fn run(&self, now: Instant) -> RunOutcome {
        let mut st = self.state.lock_recover();
        let out = st.run(now);
        if out.next.is_none() {
            // Nothing queued: the pump ends, and the next frame that has to
            // wait starts another (both under this lock, so none is missed).
            st.pump_running = false;
        }
        out
    }
}

/// The pump: one task per node, alive only while something is queued.
async fn pump(me: Weak<EgressScheduler>) {
    loop {
        let Some(s) = me.upgrade() else {
            return;
        };
        let out = s.run(Instant::now());
        let Some(next) = out.next else {
            return;
        };
        let notified = s.notify.notified();
        tokio::select! {
            _ = notified => {}
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(next)) => {}
        }
    }
}

impl State {
    fn bucket_cap(&self) -> f64 {
        match self.rate_bps {
            Some(r) => (r as f64 / 8.0 * BURST_WINDOW.as_secs_f64()).max(BURST_MIN_BYTES),
            None => BURST_MIN_BYTES,
        }
    }

    fn refill(&mut self, now: Instant) {
        let dt = match self.last_refill {
            Some(t) => now.saturating_duration_since(t).as_secs_f64(),
            None => 0.0,
        };
        // An Instant earlier than the last one adds nothing and does not move
        // the clock back.
        if self.last_refill.is_none_or(|t| now > t) {
            self.last_refill = Some(now);
        }
        if let Some(r) = self.rate_bps {
            self.tokens = (self.tokens + r as f64 / 8.0 * dt).min(self.bucket_cap());
        }
        if self.low_queued() {
            let min_bps = match self.rate_bps {
                Some(r) => self.floor.min_bps.min(r),
                None => self.floor.min_bps,
            };
            self.low_credit += min_bps as f64 / 8.0 * dt;
        }
    }

    fn low_queued(&self) -> bool {
        EgressClass::ALL
            .iter()
            .any(|c| c.is_low() && self.depth_pkts[c.index()] > 0)
    }

    /// Anything queued anywhere at `tier` or above.
    fn queued_at_or_above(&self, tier: usize) -> bool {
        EgressClass::ALL
            .iter()
            .any(|c| c.tier() <= tier && self.depth_pkts[c.index()] > 0)
    }

    /// The low classes have earned a turn ahead of everything else.
    fn floor_due(&self) -> bool {
        self.low_queued()
            && self
                .best_head(LOW_TIER..TIERS, None)
                .is_some_and(|(i, c)| self.low_credit >= self.ifaces[i].q[c][0].len as f64)
    }

    fn resolve(&mut self, hint: Option<FrameHint>, data: &[u8]) -> (EgressClass, bool) {
        let hint = match hint {
            Some(h) => h,
            // A frame no dispatch hinted (a driver retry): read it, unless
            // IFAC already masked the header, in which case it is Control,
            // which is never dropped.
            None if data.first().is_some_and(|f| f & 0x80 == 0) => classify_frame(data),
            None => return (EgressClass::Control, false),
        };
        match hint.class {
            FrameClass::Control => (
                self.link_class
                    .get(&hint.dst)
                    .copied()
                    .unwrap_or(EgressClass::Control),
                false,
            ),
            FrameClass::Proof => (EgressClass::Proof, false),
            FrameClass::LinkSetup => (EgressClass::LinkSetup, false),
            FrameClass::Announce => (EgressClass::Announce, false),
            FrameClass::Bulk => (EgressClass::Bulk, false),
            FrameClass::Media => {
                let kind = if self.media_hint.is_empty() {
                    None
                } else {
                    self.media_hint.remove(&hint.fingerprint)
                };
                let class = match kind {
                    Some(k) => k.class(),
                    None => match self.link_class.get(&hint.dst) {
                        Some(c @ (EgressClass::Audio | EgressClass::Video)) => *c,
                        _ => EgressClass::Video,
                    },
                };
                (class, true)
            }
        }
    }

    fn on_sent(&mut self, class: EgressClass, len: usize) {
        let c = &mut self.stats.classes[class.index()];
        c.sent_packets += 1;
        c.sent_bytes += len as u64;
        if self.rate_bps.is_some() {
            self.tokens -= len as f64;
        }
        if !class.is_low() && self.low_queued() {
            let share = f64::from(self.floor.share_percent);
            self.low_credit += len as f64 * share / (100.0 - share);
        }
    }

    fn age_bound(&self, class: EgressClass) -> Option<Duration> {
        match class {
            EgressClass::Audio => Some(self.media_bound[0]),
            EgressClass::Video => Some(self.media_bound[1]),
            _ => None,
        }
    }

    /// The (interface, class) whose head goes next among `tiers`, or `None`.
    /// Oldest (first handed) head first within a tier; a blocked interface is skipped when
    /// `now` is given.
    fn best_head(
        &self,
        tiers: core::ops::Range<usize>,
        now: Option<Instant>,
    ) -> Option<(usize, usize)> {
        for tier in tiers {
            let mut best: Option<(usize, usize, u64)> = None;
            for (i, q) in self.ifaces.iter().enumerate() {
                if now.is_some_and(|n| q.blocked(n)) {
                    continue;
                }
                for class in EgressClass::ALL.iter().filter(|c| c.tier() == tier) {
                    let c = class.index();
                    if let Some(head) = q.q[c].front() {
                        if best.is_none_or(|(_, _, seq)| head.seq < seq) {
                            best = Some((i, c, head.seq));
                        }
                    }
                }
            }
            if let Some((i, c, _)) = best {
                return Some((i, c));
            }
        }
        None
    }

    fn pop(&mut self, i: usize, c: usize) -> Entry {
        let e = self.ifaces[i].q[c].pop_front().expect("head exists");
        self.ifaces[i].bytes[c] -= e.len;
        self.depth_pkts[c] -= 1;
        self.depth_bytes[c] -= e.len as u64;
        e
    }

    /// Head-drop by age: every media queue's stale head is dropped.
    fn expire(&mut self, now: Instant) -> usize {
        let mut dropped = 0;
        for class in [EgressClass::Audio, EgressClass::Video] {
            let Some(bound) = self.age_bound(class) else {
                continue;
            };
            let c = class.index();
            for i in 0..self.ifaces.len() {
                while let Some(head) = self.ifaces[i].q[c].front() {
                    if !head.droppable || now.saturating_duration_since(head.at) <= bound {
                        break;
                    }
                    let e = self.pop(i, c);
                    let s = &mut self.stats.classes[c];
                    s.dropped_packets += 1;
                    s.dropped_bytes += e.len as u64;
                    dropped += 1;
                }
            }
        }
        dropped
    }

    fn discard(&mut self, q: IfaceQueue) {
        for (c, queue) in q.q.into_iter().enumerate() {
            for e in queue {
                self.depth_pkts[c] -= 1;
                self.depth_bytes[c] -= e.len as u64;
                let s = &mut self.stats.classes[c];
                s.dropped_packets += 1;
                s.dropped_bytes += e.len as u64;
            }
        }
    }

    fn run(&mut self, now: Instant) -> RunOutcome {
        let mut out = RunOutcome::default();
        loop {
            self.refill(now);
            out.dropped += self.expire(now);
            if self.rate_bps.is_some() && self.tokens < 0.0 {
                break;
            }
            let high = self.best_head(0..LOW_TIER, Some(now));
            let low = self.best_head(LOW_TIER..TIERS, Some(now));
            let (pick, floor_turn) = match (high, low) {
                (Some(_), Some((i, c))) if self.low_credit >= self.ifaces[i].q[c][0].len as f64 => {
                    ((i, c), true)
                }
                (Some(h), _) => (h, false),
                (None, Some(l)) => (l, false),
                (None, None) => break,
            };
            let (i, c) = pick;
            let Entry {
                pkt,
                len,
                at,
                seq,
                droppable,
            } = self.pop(i, c);
            match self.ifaces[i].tx.try_send(pkt) {
                Ok(()) => {
                    self.on_sent(EgressClass::ALL[c], len);
                    if floor_turn {
                        self.low_credit = (self.low_credit - len as f64).max(0.0);
                    }
                    out.sent += 1;
                }
                Err(TrySendError::Full(pkt)) => {
                    // Back at the head, in order; the interface is skipped
                    // until it has had time to drain.
                    let q = &mut self.ifaces[i];
                    q.q[c].push_front(Entry {
                        pkt,
                        len,
                        at,
                        seq,
                        droppable,
                    });
                    q.bytes[c] += len;
                    q.blocked_until = Some(now + BLOCKED_POLL);
                    self.depth_pkts[c] += 1;
                    self.depth_bytes[c] += len as u64;
                }
                Err(TrySendError::Closed(_)) => {
                    let q = self.ifaces.swap_remove(i);
                    self.discard(q);
                }
            }
        }
        // Bank no more floor credit than the next low frame needs.
        let cap = match self.best_head(LOW_TIER..TIERS, None) {
            Some((i, c)) => (self.ifaces[i].q[c][0].len as f64).max(FLOOR_BURST_BYTES),
            None => FLOOR_BURST_BYTES,
        };
        if !self.low_queued() {
            self.low_credit = self.low_credit.min(FLOOR_BURST_BYTES);
        } else {
            self.low_credit = self.low_credit.min(cap);
        }
        self.ifaces.retain(|q| !q.is_empty());
        out.next = self.next_wake(now);
        out
    }

    fn next_wake(&self, now: Instant) -> Option<Instant> {
        if self.ifaces.is_empty() {
            return None;
        }
        let mut next: Option<Instant> = None;
        let mut fold = |t: Instant| {
            next = Some(next.map_or(t, |n| n.min(t)));
        };
        for q in &self.ifaces {
            if let Some(t) = q.blocked_until {
                if t > now {
                    fold(t);
                }
            }
        }
        if let Some(r) = self.rate_bps {
            if self.tokens < 0.0 {
                let secs = -self.tokens / (r as f64 / 8.0);
                fold(now + Duration::from_secs_f64(secs));
            }
        }
        // The oldest media head's expiry, so the depth reading does not
        // carry frames that can no longer go.
        for class in [EgressClass::Audio, EgressClass::Video] {
            let bound = self.age_bound(class).unwrap_or(Duration::MAX);
            for q in &self.ifaces {
                if let Some(head) = q.q[class.index()].front() {
                    if head.droppable {
                        if let Some(t) = head.at.checked_add(bound) {
                            fold(t.max(now) + Duration::from_millis(1));
                        }
                    }
                }
            }
        }
        // Something queued but nothing to wait for (an interface unblocked
        // this instant): look again at once.
        Some(next.unwrap_or(now))
    }

    fn depth(&self, now: Instant) -> EgressDepth {
        let mut d = EgressDepth {
            rate_bps: self.rate_bps,
            ..Default::default()
        };
        for class in EgressClass::ALL {
            let c = class.index();
            let oldest = self
                .ifaces
                .iter()
                .filter_map(|q| q.q[c].front().map(|e| e.at))
                .min();
            d.classes[c] = EgressClassDepth {
                queued_packets: self.depth_pkts[c],
                queued_bytes: self.depth_bytes[c],
                oldest_age_ms: oldest
                    .map(|t| now.saturating_duration_since(t).as_millis() as u64)
                    .unwrap_or(0),
            };
        }
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const DST: [u8; 16] = [0x42; 16];

    /// Raw frame bytes as the core builds them: flags (packet type in the
    /// low two bits), hops, destination, context, payload. `salt` fills the
    /// payload head so fingerprints differ.
    fn raw(ptype: u8, context: u8, len: usize, salt: u8) -> Vec<u8> {
        let mut v = vec![ptype & 0x03, 0];
        v.extend_from_slice(&DST);
        v.push(context);
        v.resize(len.max(20), salt);
        v
    }

    fn frame_of(class: EgressClass, len: usize, salt: u8) -> Vec<u8> {
        match class {
            EgressClass::Control => raw(0, 0x0E, len, salt),
            EgressClass::Proof => raw(3, 0xFF, len, salt),
            EgressClass::LinkSetup => raw(2, 0, len, salt),
            EgressClass::Audio | EgressClass::Video => raw(0, 0xF0, len, salt),
            EgressClass::Announce => raw(1, 0, len, salt),
            EgressClass::Bulk => raw(0, 0x01, len, salt),
        }
    }

    fn pkt(data: Vec<u8>) -> OutgoingPacket {
        OutgoingPacket {
            data: data.into(),
            ..Default::default()
        }
    }

    /// What dispatch would hint for `data`.
    fn hint(data: &[u8]) -> Option<FrameHint> {
        Some(classify_frame(data))
    }

    struct Rig {
        s: Arc<EgressScheduler>,
        tx: mpsc::Sender<OutgoingPacket>,
        rx: mpsc::Receiver<OutgoingPacket>,
        t0: Instant,
    }

    fn rig(cap: usize) -> Rig {
        let (tx, rx) = mpsc::channel(cap);
        Rig {
            s: EgressScheduler::new(),
            tx,
            rx,
            t0: Instant::now(),
        }
    }

    impl Rig {
        fn at(&self, ms: u64) -> Instant {
            self.t0 + Duration::from_millis(ms)
        }
        fn submit(&self, ms: u64, data: Vec<u8>) -> Result<(), InterfaceError> {
            let h = hint(&data);
            self.s
                .submit_at(self.at(ms), InterfaceId(0), &self.tx, h, pkt(data))
        }
        /// Submit a `class` frame, tagged audio as an owner would when the
        /// class is Audio; returns its bytes.
        fn submit_c(&self, ms: u64, class: EgressClass, len: usize, salt: u8) -> Vec<u8> {
            let f = frame_of(class, len, salt);
            if class == EgressClass::Audio {
                self.s
                    .hint_media(classify_frame(&f).fingerprint, MediaKind::Audio);
            }
            self.submit(ms, f.clone()).unwrap();
            f
        }
        fn drain(&mut self) -> Vec<Arc<[u8]>> {
            let mut out = Vec::new();
            while let Ok(p) = self.rx.try_recv() {
                out.push(p.data);
            }
            out
        }
        /// Use up the token bucket with one oversized Control frame, so the
        /// next frames wait for the rate.
        fn exhaust_tokens(&mut self, ms: u64) {
            self.submit(ms, raw(0, 0x0E, 20_000, 0xFF)).unwrap();
            assert_eq!(self.drain().len(), 1, "the filler went straight out");
        }
    }

    fn class_of(s: &EgressScheduler, data: &[u8]) -> EgressClass {
        s.state.lock_recover().resolve(hint(data), data).0
    }

    /// Idle: every frame reaches its interface's channel on the same call,
    /// in the order handed, whatever its class, with or without a rate (the
    /// bucket has room), and nothing is ever queued.
    #[test]
    fn idle_adds_no_delay_and_no_reordering() {
        for rate in [None, Some(10_000_000)] {
            let mut r = rig(64);
            r.s.set_rate(rate);
            let mut sent = Vec::new();
            for (n, class) in EgressClass::ALL.iter().rev().enumerate() {
                let f = frame_of(*class, 100, n as u8);
                r.submit(0, f.clone()).unwrap();
                // On the same call: already in the channel.
                let got = r.drain();
                assert_eq!(got.len(), 1, "{class:?} rate={rate:?}");
                assert_eq!(&got[0][..], &f[..]);
                sent.push(f);
            }
            let st = r.s.stats();
            assert_eq!(st.direct_packets, 7);
            assert_eq!(st.queued_packets, 0);
            assert_eq!(r.s.depth_at(r.at(0)).total_bytes(), 0);
            // Same frames, same order, in a burst too.
            for f in &sent {
                r.submit(1, f.clone()).unwrap();
            }
            let got: Vec<Vec<u8>> = r.drain().iter().map(|d| d.to_vec()).collect();
            assert_eq!(got, sent, "rate={rate:?}");
        }
    }

    /// No rate: the scheduler is the channel. Fed past the channel's
    /// capacity it accepts and refuses exactly what a bare `try_send` would,
    /// queues nothing, and so leaves throughput where it was.
    #[test]
    fn no_rate_leaves_throughput_unchanged() {
        let mut r = rig(4);
        let (bare_tx, mut bare_rx) = mpsc::channel::<OutgoingPacket>(4);
        for round in 0..3u8 {
            let mut via = Vec::new();
            let mut bare = Vec::new();
            for n in 0..10u8 {
                let f = frame_of(EgressClass::Video, 300, n.wrapping_add(round * 10));
                via.push(r.submit(0, f.clone()).is_ok());
                bare.push(bare_tx.try_send(pkt(f)).is_ok());
            }
            assert_eq!(via, bare, "round {round}");
            assert_eq!(r.drain().len(), 4);
            while bare_rx.try_recv().is_ok() {}
        }
        assert_eq!(r.s.stats().queued_packets, 0);
        assert_eq!(r.s.stats().class(EgressClass::Video).refused_packets, 18);
        assert_eq!(r.s.run(r.at(0)).next, None, "nothing queued, no pump");
    }

    /// Strict priority: frames queued behind the rate leave Control/Proof
    /// (oldest first), LinkSetup, Audio, Video, Announce, Bulk, whatever
    /// order they were handed in; within a class, FIFO.
    #[test]
    fn priority_order() {
        let mut r = rig(64);
        r.s.set_rate(Some(800_000));
        // The floor is its own witness; here it is never due.
        r.s.set_floor(EgressFloor {
            share_percent: 1,
            min_bps: 0,
        });
        r.exhaust_tokens(0);
        // Lowest first, two of each, so a FIFO would invert the order.
        let mut handed = Vec::new();
        for class in EgressClass::ALL.iter().rev() {
            for k in 0..2u8 {
                let f = r.submit_c(0, *class, 200, class.index() as u8 * 10 + k);
                handed.push((*class, f));
            }
        }
        assert!(r.drain().is_empty(), "all wait for the rate");
        let mut out = Vec::new();
        // 20 kB of filler debt at 100 B/ms, then the 14 frames.
        for ms in 0..30_000 {
            r.s.run(r.at(ms));
            out.extend(r.drain());
        }
        assert_eq!(out.len(), 14);
        let order: Vec<EgressClass> = out
            .iter()
            .map(|d| handed.iter().find(|(_, f)| f[..] == d[..]).unwrap().0)
            .collect();
        use EgressClass::*;
        assert_eq!(
            order,
            vec![
                Proof, Proof, Control, Control, LinkSetup, LinkSetup, Audio, Audio, Video, Video,
                Announce, Announce, Bulk, Bulk
            ]
        );
        // FIFO within a class: salt k=0 before k=1.
        for pair in out.chunks(2) {
            assert!(pair[0][19] < pair[1][19]);
        }
    }

    /// The floor: under a media load twice the rate, Bulk still gets its
    /// share (10 % of what is sent), and media the rest.
    #[test]
    fn floor_bulk_never_starves_under_full_media_load() {
        let mut r = rig(1_024);
        let rate = 300_000u64;
        r.s.set_rate(Some(rate));
        let mut salt = 0u8;
        let mut next = || {
            salt = salt.wrapping_add(1);
            salt
        };
        // A bulk backlog that never runs dry.
        for _ in 0..400 {
            r.submit_c(0, EgressClass::Bulk, 500, next());
        }
        // Counted by context byte: media (0xF0) and bulk (0x01).
        let (mut media, mut bulk) = (0u64, 0u64);
        let ms_total = 10_000u64;
        for ms in 0..ms_total {
            // 2x the rate in media: 600 kbit/s = 75 B/ms = one 500 B frame
            // every ~6.7 ms, alternating audio and video.
            if ms % 20 == 0 {
                r.submit_c(ms, EgressClass::Audio, 500, next());
            }
            if ms % 20 == 7 || ms % 20 == 14 {
                r.submit_c(ms, EgressClass::Video, 500, next());
            }
            r.s.run(r.at(ms));
            for d in r.drain() {
                match d[18] {
                    0xF0 => media += d.len() as u64,
                    0x01 => bulk += d.len() as u64,
                    other => panic!("unexpected context {other:#x}"),
                }
            }
        }
        let total = media + bulk;
        // The rate holds (bucket slack aside)...
        let expected = rate / 8 * ms_total / 1_000;
        assert!(
            total <= expected + 4_000,
            "total {total} expected {expected}"
        );
        assert!(
            total >= expected * 95 / 100,
            "total {total} expected {expected}"
        );
        // ...and Bulk gets its tenth while media is offered at twice it.
        assert!(
            bulk * 100 >= total * 9,
            "bulk {bulk} of {total} under full media load"
        );
        assert!(
            bulk * 100 <= total * 15,
            "bulk {bulk} of {total}: no more than its floor"
        );
        assert!(media * 100 >= total * 80, "media {media} of {total}");
        let st = r.s.stats();
        assert!(st.class(EgressClass::Audio).sent_packets > 0);
        assert!(st.class(EgressClass::Video).sent_packets > 0);
    }

    /// Head-drop by age: a datagram older than its bound at the head is
    /// dropped and counted, never sent; Control, Proof, LinkSetup, Announce,
    /// Bulk and a Channel frame a link hint moved into Video are never
    /// dropped by age.
    #[test]
    fn head_drop_by_age_is_media_only() {
        let mut r = rig(64);
        r.s.set_rate(Some(1_000));
        r.exhaust_tokens(0);
        r.s.set_media_age_bound(MediaKind::Audio, Duration::from_millis(300));
        for class in EgressClass::ALL {
            r.submit_c(0, class, 100, class.index() as u8);
        }
        // A Channel frame on a link marked video: queued in Video.
        r.s.set_link_class(DST, Some(EgressClass::Video));
        let moved = raw(0, 0x0E, 100, 0x77);
        assert_eq!(class_of(&r.s, &moved), EgressClass::Video);
        r.submit(0, moved).unwrap();
        r.s.set_link_class(DST, None);
        assert_eq!(
            r.s.depth_at(r.at(0))
                .class(EgressClass::Video)
                .queued_packets,
            2
        );
        // 1 kbit/s and 4000 B of debt: nothing can go for 30 s.
        let out = r.s.run(r.at(301));
        assert_eq!(out.dropped, 1, "audio is past its 300 ms");
        let out = r.s.run(r.at(801));
        assert_eq!(out.dropped, 1, "video past the default 800 ms");
        let out = r.s.run(r.at(20_000));
        assert_eq!(out.dropped, 0, "nothing else is ever dropped by age");
        assert!(r.drain().is_empty(), "a dropped frame is never sent");
        let st = r.s.stats();
        for class in EgressClass::ALL {
            let want = matches!(class, EgressClass::Audio | EgressClass::Video) as u64;
            assert_eq!(st.class(class).dropped_packets, want, "{class:?}");
        }
        let d = r.s.depth_at(r.at(20_000));
        assert_eq!(
            d.class(EgressClass::Video).queued_packets,
            1,
            "the moved frame stays"
        );
        assert_eq!(d.class(EgressClass::Audio).queued_packets, 0);
        assert_eq!(d.class(EgressClass::Bulk).queued_packets, 1);
    }

    /// The depth reading: per class, the bytes and packets queued and the
    /// age of the oldest, exactly.
    #[test]
    fn depth_reading_is_exact() {
        let mut r = rig(64);
        r.s.set_rate(Some(1_000));
        r.exhaust_tokens(0);
        r.submit(10, frame_of(EgressClass::Video, 300, 1)).unwrap();
        r.submit(20, frame_of(EgressClass::Video, 200, 2)).unwrap();
        r.submit(30, frame_of(EgressClass::Bulk, 1_000, 3)).unwrap();
        r.submit(40, frame_of(EgressClass::Control, 50, 4)).unwrap();
        let d = r.s.depth_at(r.at(110));
        assert_eq!(d.rate_bps, Some(1_000));
        let v = d.class(EgressClass::Video);
        assert_eq!(
            (v.queued_packets, v.queued_bytes, v.oldest_age_ms),
            (2, 500, 100)
        );
        let b = d.class(EgressClass::Bulk);
        assert_eq!(
            (b.queued_packets, b.queued_bytes, b.oldest_age_ms),
            (1, 1_000, 80)
        );
        let c = d.class(EgressClass::Control);
        assert_eq!(
            (c.queued_packets, c.queued_bytes, c.oldest_age_ms),
            (1, 50, 70)
        );
        assert_eq!(d.class(EgressClass::Audio), EgressClassDepth::default());
        assert_eq!(d.total_bytes(), 1_550);
        assert_eq!(d.oldest_age_ms(), 100);
        // After the video bound passes, the reading no longer carries them.
        r.s.run(r.at(900));
        assert_eq!(
            r.s.depth_at(r.at(900))
                .class(EgressClass::Video)
                .queued_packets,
            0
        );
    }

    /// The rate holds: a backlog leaves at rate/8 bytes a second, give or
    /// take the bucket.
    #[test]
    fn pacing_holds_the_rate() {
        let mut r = rig(4_096);
        r.s.set_rate(Some(400_000));
        for n in 0..400u32 {
            r.submit(0, frame_of(EgressClass::Control, 500, n as u8))
                .unwrap();
        }
        let mut bytes = 0u64;
        for ms in 0..=1_000 {
            r.s.run(r.at(ms));
            bytes += r.drain().iter().map(|d| d.len() as u64).sum::<u64>();
        }
        assert!(
            (50_000..=52_000).contains(&bytes),
            "sent {bytes} in 1 s at 400 kbit/s"
        );
    }

    /// A full channel under a rate: the frame stays queued at its head, the
    /// interface is looked at again after it drains, and order holds.
    #[test]
    fn a_full_channel_holds_frames_in_order() {
        let mut r = rig(2);
        r.s.set_rate(Some(100_000_000));
        let frames: Vec<Vec<u8>> = (0..6u8)
            .map(|n| frame_of(EgressClass::Control, 100, n))
            .collect();
        for f in &frames {
            r.submit(0, f.clone()).unwrap();
        }
        assert_eq!(
            r.s.depth_at(r.at(0))
                .class(EgressClass::Control)
                .queued_packets,
            4
        );
        let mut got = r.drain();
        for ms in 1..20 {
            r.s.run(r.at(ms));
            got.extend(r.drain());
        }
        let got: Vec<Vec<u8>> = got.iter().map(|d| d.to_vec()).collect();
        assert_eq!(got, frames);
    }

    /// Per-send and per-link hints: an audio-tagged datagram is Audio, an
    /// untagged one takes its link's kind, or Video; a hint is used once.
    /// A frame no dispatch hinted is read from its bytes, or, IFAC-masked,
    /// is Control.
    #[test]
    fn hints_resolve_the_class() {
        let s = EgressScheduler::new();
        let d1 = frame_of(EgressClass::Audio, 100, 1);
        let d2 = frame_of(EgressClass::Audio, 100, 2);
        assert_eq!(class_of(&s, &d1), EgressClass::Video, "untagged datagram");
        s.hint_media(classify_frame(&d1).fingerprint, MediaKind::Audio);
        assert_eq!(
            class_of(&s, &d2),
            EgressClass::Video,
            "another frame's hint"
        );
        assert_eq!(class_of(&s, &d1), EgressClass::Audio);
        assert_eq!(class_of(&s, &d1), EgressClass::Video, "a hint is used once");
        s.set_link_class(DST, Some(EgressClass::Audio));
        assert_eq!(class_of(&s, &d1), EgressClass::Audio, "the link's kind");
        assert_eq!(
            class_of(&s, &raw(0, 0x0E, 50, 0)),
            EgressClass::Audio,
            "the link's Channel frames"
        );
        assert_eq!(
            class_of(&s, &frame_of(EgressClass::Proof, 50, 0)),
            EgressClass::Proof
        );
        s.forget_link(&DST);
        assert_eq!(class_of(&s, &raw(0, 0x0E, 50, 0)), EgressClass::Control);
        let mut st = s.state.lock_recover();
        let bulk = frame_of(EgressClass::Bulk, 50, 0);
        assert_eq!(
            st.resolve(None, &bulk).0,
            EgressClass::Bulk,
            "read from the bytes"
        );
        let mut masked = bulk.clone();
        masked[0] |= 0x80;
        assert_eq!(st.resolve(None, &masked), (EgressClass::Control, false));
    }

    /// An interface that goes away takes its queued frames (counted as
    /// dropped) and the sender clone with it, so its channel closes.
    #[test]
    fn forget_drops_the_queue_and_the_sender() {
        let mut r = rig(8);
        r.s.set_rate(Some(1_000));
        r.exhaust_tokens(0);
        r.submit(0, frame_of(EgressClass::Bulk, 100, 1)).unwrap();
        r.s.forget(InterfaceId(0));
        assert_eq!(r.s.stats().class(EgressClass::Bulk).dropped_packets, 1);
        assert_eq!(r.s.depth_at(r.at(0)).total_bytes(), 0);
        drop(std::mem::replace(&mut r.tx, mpsc::channel(1).0));
        assert!(matches!(
            r.rx.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
    }

    /// The pump, on a real runtime: a paced backlog arrives whole and in
    /// order, no sooner than the rate allows.
    #[tokio::test(flavor = "current_thread")]
    async fn the_pump_drains_a_paced_backlog() {
        let (tx, mut rx) = mpsc::channel(256);
        let s = EgressScheduler::new();
        s.set_rate(Some(400_000));
        let frames: Vec<Vec<u8>> = (0..12u8)
            .map(|n| frame_of(EgressClass::Control, 500, n))
            .collect();
        let t0 = Instant::now();
        for f in &frames {
            s.submit(InterfaceId(0), &tx, hint(f), pkt(f.clone()))
                .unwrap();
        }
        let mut got = Vec::new();
        while got.len() < frames.len() {
            let p = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("the pump delivers")
                .unwrap();
            got.push(p.data.to_vec());
        }
        assert_eq!(got, frames);
        // 6000 B at 50 kB/s is 120 ms; the bucket lets 1500 B go at once.
        assert!(
            t0.elapsed() >= Duration::from_millis(80),
            "{:?}",
            t0.elapsed()
        );
        // Idle again: the pump has ended.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!s.state.lock_recover().pump_running);
    }
}
