//! `lnstest selftest`: real-network integration self-test
//!
//! Two ephemeral nodes in one process, both connected to a public relay,
//! establishing a link through it and exchanging messages bidirectionally.
//! After the link phase, a single-packet (fire-and-forget) exchange is run
//! to exercise the destination-addressed code path.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use leviculum_std::driver::{EventReceiver, LinkHandle, PacketSender, ReticulumNodeBuilder};
use leviculum_std::interfaces::{disable_fault_injection, enable_fault_injection};
use leviculum_std::{
    Destination, DestinationHash, DestinationType, Direction, Identity, LinkId, NodeEvent,
};

use leviculum_core::transport::AcquisitionCeiling;

// Message Format

/// Which exchange a message belongs to.
///
/// Carried in the message rather than read off the tool's current phase
/// when the message arrives. A link message still in flight when the
/// single-packet phase opens is a link message; counting it as a
/// single-packet receipt inflates exactly the delivery rate this tool
/// exists to measure, and can put the single-packet tally above the
/// number of single packets sent (Codeberg #337).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Sent over the established link.
    Link,
    /// Sent as a destination-addressed single packet.
    SinglePacket,
}

impl Phase {
    fn tag(self) -> &'static str {
        match self {
            Phase::Link => "l",
            Phase::SinglePacket => "p",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "l" => Some(Phase::Link),
            "p" => Some(Phase::SinglePacket),
            _ => None,
        }
    }
}

fn build_message(phase: Phase, dir: &str, seq: u64, now_ms: u64) -> Vec<u8> {
    let payload = format!("{}:{dir}:{seq}:{now_ms}", phase.tag());
    let mut hasher = Sha256::new();
    hasher.update(payload.as_bytes());
    let hash = hasher.finalize();
    let checksum = &crate::hex_encode(&hash[..4]);
    format!("{payload}:{checksum}").into_bytes()
}

struct ParsedMessage {
    phase: Phase,
    dir: String,
    seq: u64,
    timestamp_ms: u64,
}

fn parse_message(data: &[u8]) -> Option<ParsedMessage> {
    let s = std::str::from_utf8(data).ok()?;
    let parts: Vec<&str> = s.splitn(5, ':').collect();
    if parts.len() != 5 {
        return None;
    }
    let phase = Phase::from_tag(parts[0])?;
    let dir = parts[1].to_string();
    let seq: u64 = parts[2].parse().ok()?;
    let timestamp_ms: u64 = parts[3].parse().ok()?;
    let checksum = parts[4];

    // Verify checksum
    let payload = format!("{}:{dir}:{seq}:{timestamp_ms}", phase.tag());
    let mut hasher = Sha256::new();
    hasher.update(payload.as_bytes());
    let hash = hasher.finalize();
    let expected = crate::hex_encode(&hash[..4]);
    if checksum != expected {
        return None;
    }

    Some(ParsedMessage {
        phase,
        dir,
        seq,
        timestamp_ms,
    })
}

// Stats
struct SelftestStats {
    // Link phase
    sent_a: u64,
    sent_b: u64,
    recv_a: u64,
    recv_b: u64,
    confirmed_a: u64,
    confirmed_b: u64,
    send_fails_a: u64,
    send_fails_b: u64,
    corrupt: u64,
    last_seq_recv_a: u64,
    last_seq_recv_b: u64,
    out_of_order: u64,
    duplicates: u64,
    seen_seqs_a: BTreeSet<u64>,
    seen_seqs_b: BTreeSet<u64>,
    rtt_samples: Vec<u64>,
    retransmits_a: u64,
    retransmits_b: u64,
    stale_count: u64,
    recovered_count: u64,

    // Single-packet phase
    sp_sent_a: u64,
    sp_sent_b: u64,
    sp_recv_a: u64,
    sp_recv_b: u64,
    sp_send_fails_a: u64,
    sp_send_fails_b: u64,
    sp_corrupt: u64,
    sp_last_seq_recv_a: u64,
    sp_last_seq_recv_b: u64,
    sp_out_of_order: u64,
    sp_duplicates: u64,
    sp_seen_seqs_a: BTreeSet<u64>,
    sp_seen_seqs_b: BTreeSet<u64>,
    sp_rtt_samples: Vec<u64>,
    /// Single packets that landed after their phase's drain budget expired,
    /// inside the grace window the tool keeps its clients up for. Counted in
    /// `sp_recv_*` like any other receipt and subtracted back out of the
    /// delivery bar, so a late frame neither flatters the bar nor reads as
    /// lost.
    sp_late: u64,
}

impl SelftestStats {
    /// The single packets the drain budget covered — what the delivery bar
    /// is computed from.
    fn sp_in_budget_recv(&self) -> u64 {
        (self.sp_recv_a + self.sp_recv_b).saturating_sub(self.sp_late)
    }

    fn new() -> Self {
        Self {
            sent_a: 0,
            sent_b: 0,
            recv_a: 0,
            recv_b: 0,
            confirmed_a: 0,
            confirmed_b: 0,
            send_fails_a: 0,
            send_fails_b: 0,
            corrupt: 0,
            last_seq_recv_a: 0,
            last_seq_recv_b: 0,
            out_of_order: 0,
            duplicates: 0,
            seen_seqs_a: BTreeSet::new(),
            seen_seqs_b: BTreeSet::new(),
            rtt_samples: Vec::new(),
            retransmits_a: 0,
            retransmits_b: 0,
            stale_count: 0,
            recovered_count: 0,

            sp_sent_a: 0,
            sp_sent_b: 0,
            sp_recv_a: 0,
            sp_recv_b: 0,
            sp_send_fails_a: 0,
            sp_send_fails_b: 0,
            sp_corrupt: 0,
            sp_last_seq_recv_a: 0,
            sp_last_seq_recv_b: 0,
            sp_out_of_order: 0,
            sp_duplicates: 0,
            sp_seen_seqs_a: BTreeSet::new(),
            sp_seen_seqs_b: BTreeSet::new(),
            sp_rtt_samples: Vec::new(),
            sp_late: 0,
        }
    }
}

// Shared State
struct SharedState {
    stats: Mutex<SelftestStats>,
    // Discovery
    b_signing_key: Mutex<Option<[u8; 32]>>,
    a_signing_key: Mutex<Option<[u8; 32]>>,
    a_discovered_b: Notify,
    b_discovered_a: Notify,
    // Link
    link_established_b: Notify,
    // Link id of the link B actually established (the re-keyed retry that won,
    // when the first proof was lost). Used to mint the responder handle.
    established_link_b: Mutex<Option<LinkId>>,
    // Phase flag: true during single-packet phase
    single_packet_phase: AtomicBool,
    // Link death detection
    link_dead: AtomicBool,
    link_dead_elapsed_secs: Mutex<Option<u64>>,
}

impl SharedState {
    fn new() -> Self {
        Self {
            stats: Mutex::new(SelftestStats::new()),
            b_signing_key: Mutex::new(None),
            a_signing_key: Mutex::new(None),
            a_discovered_b: Notify::new(),
            b_discovered_a: Notify::new(),
            link_established_b: Notify::new(),
            established_link_b: Mutex::new(None),
            single_packet_phase: AtomicBool::new(false),
            link_dead: AtomicBool::new(false),
            link_dead_elapsed_secs: Mutex::new(None),
        }
    }
}

// Received message recording
/// Record a received message into the stats, handling dedup, ordering, and RTT.
/// `is_a` = true means node A received (expects dir "ba"), false means node B (expects "ab").
///
/// The message says which exchange it belongs to, so a link message that
/// arrives after the single-packet phase has opened is still counted as a
/// link message (Codeberg #337). `sp_phase_open` is the tool's current
/// phase and is used for one thing only: a frame that did not parse carries
/// no phase, so the corrupt count has to be attributed to the phase that is
/// running.
fn record_received_message(
    st: &mut SelftestStats,
    data: &[u8],
    now_ms: u64,
    is_a: bool,
    sp_phase_open: bool,
) {
    let expected_dir = if is_a { "ba" } else { "ab" };
    match parse_message(data) {
        Some(msg) if msg.dir == expected_dir => {
            if msg.phase == Phase::SinglePacket {
                // Single-packet phase counters
                if st.sp_seen_seqs_a.contains(&msg.seq) && is_a
                    || st.sp_seen_seqs_b.contains(&msg.seq) && !is_a
                {
                    st.sp_duplicates += 1;
                } else {
                    let (seen, last_seq, recv, rtt) = if is_a {
                        (
                            &mut st.sp_seen_seqs_a,
                            &mut st.sp_last_seq_recv_a,
                            &mut st.sp_recv_a,
                            &mut st.sp_rtt_samples,
                        )
                    } else {
                        (
                            &mut st.sp_seen_seqs_b,
                            &mut st.sp_last_seq_recv_b,
                            &mut st.sp_recv_b,
                            &mut st.sp_rtt_samples,
                        )
                    };
                    seen.insert(msg.seq);
                    if msg.seq < *last_seq && *recv > 0 {
                        st.sp_out_of_order += 1;
                    }
                    *last_seq = msg.seq;
                    *recv += 1;
                    rtt.push(now_ms.saturating_sub(msg.timestamp_ms));
                }
            } else {
                // Link phase counters
                let (seen, last_seq, recv, rtt) = if is_a {
                    (
                        &mut st.seen_seqs_a,
                        &mut st.last_seq_recv_a,
                        &mut st.recv_a,
                        &mut st.rtt_samples,
                    )
                } else {
                    (
                        &mut st.seen_seqs_b,
                        &mut st.last_seq_recv_b,
                        &mut st.recv_b,
                        &mut st.rtt_samples,
                    )
                };
                if seen.contains(&msg.seq) {
                    st.duplicates += 1;
                } else {
                    seen.insert(msg.seq);
                    if msg.seq < *last_seq && *recv > 0 {
                        st.out_of_order += 1;
                    }
                    *last_seq = msg.seq;
                    *recv += 1;
                    rtt.push(now_ms.saturating_sub(msg.timestamp_ms));
                }
            }
        }
        Some(_) => {} // Message from wrong direction
        None => {
            if sp_phase_open {
                st.sp_corrupt += 1;
            } else {
                st.corrupt += 1;
            }
        }
    }
}

// Verdict
/// The worst thing a phase saw, and what the tool's exit code is derived
/// from. Public because the caller decides what a `Fail` costs: the binary
/// exits non-zero on it, an in-process test asserts on it. A library
/// function that exits the process instead cannot be called from a test at
/// all — the test binary goes with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    Pass,
    Warn,
    Fail,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Verdict::Pass => write!(f, "PASS"),
            Verdict::Warn => write!(f, "WARN"),
            Verdict::Fail => write!(f, "FAIL"),
        }
    }
}

fn compute_link_verdict(stats: &SelftestStats, warnings: &[String]) -> Verdict {
    let total_sent = stats.sent_a + stats.sent_b;
    let total_recv = stats.recv_a + stats.recv_b;

    if total_sent > 0 && total_recv == 0 {
        return Verdict::Fail;
    }
    if total_sent > 0 {
        let recv_pct = (total_recv as f64 / total_sent as f64) * 100.0;
        if recv_pct < 90.0 {
            return Verdict::Fail;
        }
        if recv_pct < 99.0 {
            return Verdict::Warn;
        }
    }
    if !warnings.is_empty() {
        return Verdict::Warn;
    }

    Verdict::Pass
}

/// The single-packet verdict, on the receipts the drain budget covered.
///
/// A frame that landed in the grace window after expiry is counted as late
/// (`sp_late`) and stays out of the bar: the bar says what the link delivered
/// inside the window the link's own figures priced, and a late frame that
/// lifted a cell over its threshold would hide exactly the pacing problem the
/// budget exists to expose.
fn compute_sp_verdict(stats: &SelftestStats, warnings: &[String]) -> Verdict {
    let total_sent = stats.sp_sent_a + stats.sp_sent_b;
    let total_recv = stats.sp_in_budget_recv();

    // FAIL conditions, relaxed for unreliable single packets
    if total_sent > 0 && total_recv == 0 {
        return Verdict::Fail;
    }
    if total_sent > 0 {
        let recv_pct = (total_recv as f64 / total_sent as f64) * 100.0;
        if recv_pct < 50.0 {
            return Verdict::Fail;
        }
    }

    if !warnings.is_empty() {
        return Verdict::Warn;
    }

    Verdict::Pass
}

// Event Tasks
async fn event_task_a(
    mut event_rx: EventReceiver,
    state: Arc<SharedState>,
    dest_hash_b: DestinationHash,
    link_id_a: Arc<Mutex<Option<LinkId>>>,
    start_time: Instant,
) {
    while let Some(event) = event_rx.recv().await {
        match event {
            NodeEvent::AnnounceReceived { announce, .. }
                if *announce.destination_hash() == dest_hash_b =>
            {
                let pk = announce.public_key();
                let mut key = [0u8; 32];
                key.copy_from_slice(&pk[32..64]);
                *state.b_signing_key.lock().unwrap() = Some(key);
                state.a_discovered_b.notify_one();
            }
            NodeEvent::LinkEstablished { link_id, .. } => {
                *link_id_a.lock().unwrap() = Some(link_id);
            }
            NodeEvent::MessageReceived { data, .. } => {
                let now_ms = start_time.elapsed().as_millis() as u64;
                let is_sp = state.single_packet_phase.load(Ordering::Relaxed);
                let mut st = state.stats.lock().unwrap();
                record_received_message(&mut st, &data, now_ms, true, is_sp);
            }
            NodeEvent::PacketReceived { data, .. } => {
                let now_ms = start_time.elapsed().as_millis() as u64;
                let is_sp = state.single_packet_phase.load(Ordering::Relaxed);
                let mut st = state.stats.lock().unwrap();
                record_received_message(&mut st, &data, now_ms, true, is_sp);
            }
            NodeEvent::LinkDeliveryConfirmed { .. } => {
                state.stats.lock().unwrap().confirmed_a += 1;
            }
            NodeEvent::ChannelRetransmit { .. } => {
                state.stats.lock().unwrap().retransmits_a += 1;
            }
            NodeEvent::LinkStale { .. } => {
                state.stats.lock().unwrap().stale_count += 1;
            }
            NodeEvent::LinkRecovered { .. } => {
                state.stats.lock().unwrap().recovered_count += 1;
            }
            NodeEvent::LinkClosed { reason, .. } => {
                let elapsed = start_time.elapsed().as_secs();
                println!("[selftest]   +{elapsed}s:  Link died ({reason:?})");
                state.link_dead.store(true, Ordering::Relaxed);
                *state.link_dead_elapsed_secs.lock().unwrap() = Some(elapsed);
            }
            _ => {}
        }
    }
}

async fn event_task_b(
    mut event_rx: EventReceiver,
    state: Arc<SharedState>,
    dest_hash_a: DestinationHash,
    start_time: Instant,
) {
    while let Some(event) = event_rx.recv().await {
        match event {
            NodeEvent::AnnounceReceived { announce, .. }
                if *announce.destination_hash() == dest_hash_a =>
            {
                let pk = announce.public_key();
                let mut key = [0u8; 32];
                key.copy_from_slice(&pk[32..64]);
                *state.a_signing_key.lock().unwrap() = Some(key);
                state.b_discovered_a.notify_one();
            }
            NodeEvent::LinkEstablished { link_id, .. } => {
                *state.established_link_b.lock().unwrap() = Some(link_id);
                state.link_established_b.notify_one();
            }
            NodeEvent::MessageReceived { data, .. } => {
                let now_ms = start_time.elapsed().as_millis() as u64;
                let is_sp = state.single_packet_phase.load(Ordering::Relaxed);
                let mut st = state.stats.lock().unwrap();
                record_received_message(&mut st, &data, now_ms, false, is_sp);
            }
            NodeEvent::PacketReceived { data, .. } => {
                let now_ms = start_time.elapsed().as_millis() as u64;
                let is_sp = state.single_packet_phase.load(Ordering::Relaxed);
                let mut st = state.stats.lock().unwrap();
                record_received_message(&mut st, &data, now_ms, false, is_sp);
            }
            NodeEvent::LinkDeliveryConfirmed { .. } => {
                state.stats.lock().unwrap().confirmed_b += 1;
            }
            NodeEvent::ChannelRetransmit { .. } => {
                state.stats.lock().unwrap().retransmits_b += 1;
            }
            NodeEvent::LinkStale { .. } => {
                state.stats.lock().unwrap().stale_count += 1;
            }
            NodeEvent::LinkRecovered { .. } => {
                state.stats.lock().unwrap().recovered_count += 1;
            }
            NodeEvent::LinkClosed { reason, .. } => {
                let elapsed = start_time.elapsed().as_secs();
                println!("[selftest]   +{elapsed}s:  Link died ({reason:?})");
                state.link_dead.store(true, Ordering::Relaxed);
                *state.link_dead_elapsed_secs.lock().unwrap() = Some(elapsed);
            }
            _ => {}
        }
    }
}

// Send helpers
async fn send_msg(
    stream: &LinkHandle,
    dir: &str,
    seq: u64,
    start_time: Instant,
    state: &SharedState,
    is_a: bool,
) {
    let now_ms = start_time.elapsed().as_millis() as u64;
    let msg = build_message(Phase::Link, dir, seq, now_ms);
    match stream.send(&msg).await {
        Ok(()) => {
            let mut st = state.stats.lock().unwrap();
            if is_a {
                st.sent_a += 1;
            } else {
                st.sent_b += 1;
            }
        }
        Err(_) => {
            let mut st = state.stats.lock().unwrap();
            if is_a {
                st.send_fails_a += 1;
            } else {
                st.send_fails_b += 1;
            }
        }
    }
}

async fn send_single_msg(
    endpoint: &PacketSender,
    dir: &str,
    seq: u64,
    start_time: Instant,
    state: &SharedState,
    is_a: bool,
) -> Option<usize> {
    let now_ms = start_time.elapsed().as_millis() as u64;
    let msg = build_message(Phase::SinglePacket, dir, seq, now_ms);
    match endpoint.send_measured(&msg).await {
        Ok((_hash, wire_len)) => {
            let mut st = state.stats.lock().unwrap();
            if is_a {
                st.sp_sent_a += 1;
            } else {
                st.sp_sent_b += 1;
            }
            return Some(wire_len);
        }
        Err(_) => {
            let mut st = state.stats.lock().unwrap();
            if is_a {
                st.sp_send_fails_a += 1;
            } else {
                st.sp_send_fails_b += 1;
            }
        }
    }
    None
}

// Drain Budget
//
// How long a burst of single packets needs before the far side can be
// counted. Derived from what the link reports about itself: a fixed sleep
// sized for one set of radio settings is wrong at every other one, and reads
// as packet loss when it expires early (Codeberg #190).

/// The share of a frame's on-air cost that the interface's reported bitrate
/// does not price, in permille.
///
/// The bitrate an interface derives from its radio settings counts payload
/// symbols. Preamble, explicit header and the medium-access delay the carrier
/// imposes before each frame are on top of it. Measured on the T-Beam pair of
/// Codeberg #190 at 2734 bps: a 147-byte frame occupies 502 ms against 430 ms
/// of payload symbols (1.17x), a 225-byte announce 750 ms against 658 ms
/// (1.14x). 1200 permille covers the measured spread with a small reserve.
///
/// Deliberately not larger. A budget that is merely generous passes every
/// test and hides the next window bug; this one is meant to expire when the
/// link genuinely fails to keep up.
const FRAME_OVERHEAD_PERMILLE: u64 = 1_200;

/// The size the frame reaches on the air, given the size the tool's own node
/// packed and the hop count to the destination.
///
/// The tool measures its packet where it leaves its own node, which is a hop
/// short of the radio. A packet a transport node forwards is rewritten from
/// the minimum header form to the maximum one — the forwarder inserts its own
/// address field ahead of the destination's, `HEADER_MAXSIZE - HEADER_MINSIZE`
/// = one truncated hash — and that happens once, at the first forwarder,
/// regardless of how many hops follow.
///
/// Measured, not assumed: on the T-Beam pair of Codeberg #190 the tool packs
/// 131 bytes and the daemon's `LORA_TX` carries 147 for the same frame, over
/// a 3-hop path. Pricing the 131 under-sized the airtime term by 12 %, which
/// is a second of window on a 20-frame burst — enough to cut the tail of the
/// slower direction off and read it as loss, which is the whole failure this
/// derivation exists to remove.
///
/// A directly attached destination is not rewritten, so nothing is added.
fn on_air_bytes(packed: usize, hops: Option<u8>) -> usize {
    use leviculum_core::constants::{HEADER_MAXSIZE, HEADER_MINSIZE};
    match hops {
        Some(h) if h >= 2 => packed + (HEADER_MAXSIZE - HEADER_MINSIZE),
        _ => packed,
    }
}

/// A delivery budget with the arithmetic that produced it, so the run's log
/// carries the derivation and not just the number.
struct DrainBudget {
    total: std::time::Duration,
    /// How long the clients stay up past expiry, still counting — one hold
    /// plus one airtime, the time a frame already handed to the modem needs
    /// to reach the far side. Zero when nothing on-air was priced.
    grace: std::time::Duration,
    detail: String,
}

/// The link profile a phase sizes its drain budget from, and where it came
/// from — printed with every budget, because a number whose provenance is not
/// in the log cannot be checked afterwards.
#[derive(Clone)]
struct LinkSizing {
    profile: Option<leviculum_core::transport::LinkProfile>,
    origin: String,
}

impl LinkSizing {
    /// Nothing to size from, and the reason why.
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            profile: None,
            origin: reason.into(),
        }
    }
}

/// Pick the profile that describes the air a phase's frames have to cross.
///
/// The tool's own next hop wins when it has one — that is a node sitting
/// directly on the radio, and nothing describes its link better. In the
/// normal deployment the tool is a TCP client two hops from the radio and its
/// next hop has no airtime at all, so the daemon's answer is what is left.
fn link_sizing(
    local_next_hop: Option<leviculum_core::transport::LinkProfile>,
    from_daemon: &LinkSizing,
) -> LinkSizing {
    match local_next_hop {
        Some(profile) if profile.bitrate_bps > 0 => LinkSizing {
            profile: Some(profile),
            origin: "this node's own next-hop interface".to_string(),
        },
        _ => from_daemon.clone(),
    }
}

/// Read a radio row's on-air `bitrate`, in either shape a daemon serves it.
///
/// `lnsd` serves the key as an integer (`rpc/handlers.rs`, `pickle_int`).
/// `rnsd` serves whatever Python computed, and `RNodeInterface.updateBitrate`
/// is float arithmetic (RNodeInterface.py:694) reported unrounded
/// (Reticulum.py:1423): 3125.0 on the default SF8/CR5/125 kHz PHY, 2734.375 on
/// SF7/CR5 at 62.5 kHz. Reading only the integer shape made every `rnsd` row
/// bitrate-less, so the same phase ran a derived budget against our daemon and
/// the fixed fallback against Python's — a budget that differs by daemon is a
/// config difference smuggled into a comparison that is supposed to differ
/// only in the stack under test.
///
/// A fractional rate is floored, never rounded up: an over-stated bitrate
/// prices the air short, and cutting a train off is the failure this whole
/// derivation exists to remove, while waiting a fraction of a frame too long
/// costs nothing. Rates below 1 bps, non-finite ones and anything past
/// `u32::MAX` are not a link: the caller skips the row and says so.
///
/// Returns the rate and the shape it arrived in, so the sizing line names the
/// daemon shape that fed it.
fn radio_bitrate_bps(iface: &serde_json::Value) -> Option<(u32, String)> {
    let raw = iface.get("bitrate")?;
    if let Some(bps) = raw.as_u64() {
        if bps == 0 || bps > u32::MAX as u64 {
            return None;
        }
        return Some((bps as u32, "reported as an integer".to_string()));
    }
    let bps = raw.as_f64()?;
    if !bps.is_finite() || bps < 1.0 || bps > u32::MAX as f64 {
        return None;
    }
    let shape = if bps.fract() == 0.0 {
        format!("reported as the float {bps:?}")
    } else {
        format!("floored from the float {bps:?}")
    };
    Some((bps as u32, shape))
}

/// Read a link profile out of an `interface_stats` payload.
///
/// The payload is the shared-instance dict `rnsd` serves too, so this reads
/// what both stacks report and tolerates what only one of them does:
///
/// - `bitrate` on a radio row is the interface's own on-air rate on both
///   stacks (Python `RNodeInterface.updateBitrate`, RNodeInterface.py:693-696,
///   reported through Reticulum.py:1421-1423). The two stacks serve it in
///   different shapes, and [`radio_bitrate_bps`] reads both.
/// - `tx_jitter_max` is ours alone (Codeberg #190). A payload without it —
///   from `rnsd`, or from an `lnsd` older than this change — yields a profile
///   with no handover term rather than an error: the frames' own airtime is
///   still derived, which is a better bound than the fixed sleep it replaces,
///   and the caller says in its log that the handover went unaccounted.
///
/// Radio rows are the ones carrying `airtime_short`, which both stacks emit
/// only for an RNode interface (Reticulum.py:1371-1372 gates it on
/// `hasattr(interface, "r_airtime_short")`). Of those, the most constraining —
/// lowest bitrate — is chosen: a burst is bounded by the slowest air it has to
/// cross, and picking the fastest would under-size the window, which is the
/// failure this whole derivation exists to remove.
struct RadioRow {
    bitrate_bps: u32,
    tx_jitter_max_ms: Option<u64>,
    acquisition: Option<AcquisitionCeiling>,
    /// The interface's own name, for the line the caller logs.
    name: String,
    /// Which shape the bitrate was read out of, likewise for the log.
    shape: String,
}

fn link_profile_from_interface_stats(
    stats: &serde_json::Value,
) -> Result<(leviculum_core::transport::LinkProfile, String), String> {
    let interfaces = stats
        .get("interfaces")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "payload carries no `interfaces` array".to_string())?;

    let mut best: Option<RadioRow> = None;
    for iface in interfaces {
        // A radio row, by the key both stacks gate on the radio itself.
        if iface.get("airtime_short").is_none() {
            continue;
        }
        let Some((bitrate, shape)) = radio_bitrate_bps(iface) else {
            continue;
        };
        // Seconds on the wire, milliseconds in the profile. Absent on any
        // daemon that does not report a pre-TX contention bound.
        let jitter_ms = iface
            .get("tx_jitter_max")
            .and_then(|v| v.as_f64())
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(|s| (s * 1000.0) as u64);
        // What TAKING the channel costs, which is a different quantity from
        // the per-frame ceiling above on an interface that prices its
        // contention slots in whole frames (#347 arm 3). Seconds on the wire
        // like `tx_jitter_max`; the slot count rides beside it and is absent
        // on every interface whose acquisition does not scale with the frame.
        let acquisition = iface
            .get("tx_acquisition_max")
            .and_then(|v| v.as_f64())
            .filter(|s| s.is_finite() && *s >= 0.0)
            .map(|s| AcquisitionCeiling {
                max_ms: (s * 1000.0) as u64,
                frame_slots: iface
                    .get("tx_acquisition_frame_slots")
                    .and_then(|v| v.as_u64())
                    .filter(|n| *n > 0),
                // Only the daemon knows what a full-size frame costs on its
                // own carrier, and it does not report that; this reader
                // prices the frames it is itself about to send, so the
                // full-frame figure is the slot-priced one.
                full_frame_ms: (s * 1000.0) as u64,
            });
        let name = iface
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("<unnamed>")
            .to_string();
        if best.as_ref().is_none_or(|b| bitrate < b.bitrate_bps) {
            best = Some(RadioRow {
                bitrate_bps: bitrate,
                tx_jitter_max_ms: jitter_ms,
                acquisition,
                name,
                shape,
            });
        }
    }

    let Some(RadioRow {
        bitrate_bps,
        tx_jitter_max_ms,
        acquisition,
        name,
        shape,
    }) = best
    else {
        return Err("the daemon reports no radio interface with a usable bitrate".to_string());
    };
    let acquisition_note = match acquisition {
        Some(AcquisitionCeiling {
            max_ms,
            frame_slots: Some(slots),
            ..
        }) => format!(", acquisition {slots} slots of one frame's airtime, at least {max_ms} ms"),
        Some(AcquisitionCeiling { max_ms, .. }) => format!(", acquisition {max_ms} ms"),
        None => String::new(),
    };
    let origin = match tx_jitter_max_ms {
        Some(ms) => {
            format!(
                "the daemon's `{name}` ({bitrate_bps} bps {shape}, jitter ceiling {ms} \
                 ms{acquisition_note})"
            )
        }
        None => format!(
            "the daemon's `{name}` ({bitrate_bps} bps {shape}; it reports no pre-TX jitter \
             ceiling, so the handover goes unaccounted)"
        ),
    };
    Ok((
        leviculum_core::transport::LinkProfile {
            bitrate_bps,
            tx_jitter_max_ms,
            acquisition,
        },
        origin,
    ))
}

/// Ask the daemon that owns the radio what its link looks like.
///
/// Every failure is a reason, never an abort: the tool still runs, on the
/// fixed fallback wait, and says in its log which state it is in.
async fn daemon_link_sizing(config_dir: Option<&std::path::Path>) -> LinkSizing {
    let Some(dir) = config_dir else {
        return LinkSizing::unavailable(
            "no -c/--config given, so the daemon that owns the radio was not asked",
        );
    };
    let access = match crate::daemon_rpc::DaemonAccess::resolve(Some(dir), None) {
        Ok(a) => a,
        Err(e) => {
            return LinkSizing::unavailable(format!(
                "cannot reach the daemon's shared instance under {}: {e}",
                dir.display()
            ));
        }
    };
    let stats = match access.query("interface_stats").await {
        Ok(v) => v,
        Err(e) => {
            return LinkSizing::unavailable(format!(
                "interface_stats query to instance `{}` failed: {e}",
                access.instance_name
            ));
        }
    };
    match link_profile_from_interface_stats(&stats) {
        Ok((profile, origin)) => LinkSizing {
            profile: Some(profile),
            origin,
        },
        Err(e) => LinkSizing::unavailable(format!(
            "instance `{}` answered, but {e}",
            access.instance_name
        )),
    }
}

// Announce cap slot
//
// What the reference stack does to an announce it cannot air yet, and what
// this tool does about it. Both matter to Phase 2, where a single announce
// per destination is the only thing that makes the far side discoverable.

/// The app data each client's announce carries. Named rather than inlined
/// because [`ANNOUNCE_WIRE_BYTES`] is computed from its length: an announce
/// whose size is guessed prices the cap slot below against the wrong packet.
const APP_DATA_A: &[u8] = b"selftest-a";
/// Client B's, the same length as [`APP_DATA_A`].
const APP_DATA_B: &[u8] = b"selftest-b";

/// The share of an interface an announce may occupy in the reference stack,
/// in percent (`ANNOUNCE_CAP`, `reference/Reticulum/RNS/Reticulum.py:114`).
///
/// A neighbour running it holds the interface closed to further announces for
/// `tx_time / announce_cap` after airing one — 50 x the announce's own air
/// at the default 2 % (`process_announce_queue`,
/// `reference/Reticulum/RNS/Interfaces/Interface.py:346`, and the same
/// arithmetic on the transmit path at
/// `reference/Reticulum/RNS/Transport.py:1259`). An announce that arrives
/// inside that window is queued, not aired, and what happens to the queued
/// entry is the neighbour's business, not ours.
const REFERENCE_ANNOUNCE_CAP_PERCENT: u64 = 2;

/// One selftest announce on the wire, in bytes.
///
/// Header (flags, hops, destination hash, context) plus the announce payload
/// the reference prices `len(packet.raw)*8 / bitrate` against: public key,
/// name hash, random hash, ratchet, signature, app data. The ratchet is
/// counted whether or not this run enables ratchets — an over-stated slot
/// waits a little longer before re-announcing, an under-stated one
/// re-announces while the neighbour is still capped, which is the whole
/// failure this exists to avoid.
const ANNOUNCE_WIRE_BYTES: u64 = {
    use leviculum_core::constants::{
        ED25519_SIGNATURE_SIZE, HEADER_MINSIZE, IDENTITY_KEY_SIZE, NAME_HASHBYTES,
        RANDOM_HASHBYTES, RATCHET_SIZE,
    };
    (HEADER_MINSIZE
        + IDENTITY_KEY_SIZE
        + NAME_HASHBYTES
        + RANDOM_HASHBYTES
        + RATCHET_SIZE
        + ED25519_SIGNATURE_SIZE
        + APP_DATA_A.len()) as u64
};

/// The slot to re-announce on when the link the announces cross is not
/// priced — no daemon asked, or none that owns a radio.
///
/// A fixed number, and it has to be: with no bitrate there is nothing to
/// derive from. 30 s is one cap slot for a 209-byte announce at 2789 bps,
/// which is the order of the LoRa links this tool is pointed at
/// (`lora_ratchet_*` runs at 2734 bps).
const ANNOUNCE_CAP_SLOT_FALLBACK: std::time::Duration = std::time::Duration::from_secs(30);

/// The shortest slot this tool will re-announce on, whatever the arithmetic
/// says.
///
/// A fast link prices the slot in milliseconds — 8 ms at 10 Mbit/s — and a
/// re-announce every 8 ms is an announce storm, not a repair. A neighbour
/// whose cap slot is that short has also long since drained its queue, so
/// there is nothing there for a re-announce to repair.
const ANNOUNCE_CAP_SLOT_FLOOR: std::time::Duration = std::time::Duration::from_secs(1);

/// How often Phase 2 re-announces while it waits, and the arithmetic that
/// produced it.
struct AnnounceCapSlot {
    slot: std::time::Duration,
    detail: String,
}

/// Derive the re-announce slot from what the daemon says about its radio.
fn announce_cap_slot(sizing: &LinkSizing) -> AnnounceCapSlot {
    let Some(profile) = sizing.profile.filter(|p| p.bitrate_bps > 0) else {
        return AnnounceCapSlot {
            slot: ANNOUNCE_CAP_SLOT_FALLBACK,
            detail: format!(
                "{:.1}s fixed — no on-air bitrate to derive the reference's \
                 {REFERENCE_ANNOUNCE_CAP_PERCENT}% announce cap from ({})",
                ANNOUNCE_CAP_SLOT_FALLBACK.as_secs_f64(),
                sizing.origin,
            ),
        };
    };

    let tx_ms = ANNOUNCE_WIRE_BYTES * 8 * 1000 / profile.bitrate_bps as u64;
    let slot_ms = tx_ms * 100 / REFERENCE_ANNOUNCE_CAP_PERCENT;
    let derived = std::time::Duration::from_millis(slot_ms);
    let slot = derived.max(ANNOUNCE_CAP_SLOT_FLOOR);
    let floored = if slot > derived {
        format!(
            ", floored at {:.1}s so a fast link is not re-announced at its own \
             millisecond slot",
            ANNOUNCE_CAP_SLOT_FLOOR.as_secs_f64()
        )
    } else {
        String::new()
    };
    AnnounceCapSlot {
        slot,
        detail: format!(
            "{:.1}s — a {ANNOUNCE_WIRE_BYTES}B announce is {:.2}s of air at {} bps, \
             and the reference lets announces have {REFERENCE_ANNOUNCE_CAP_PERCENT}% \
             of an interface{floored}; sized from {}",
            slot.as_secs_f64(),
            tx_ms as f64 / 1000.0,
            profile.bitrate_bps,
            sizing.origin,
        ),
    }
}

/// What one frame costs the medium and its own sender, at the link
/// `profile` describes.
///
/// Both terms are per frame, and both are the interface's own arithmetic seen
/// from the outside: `tx_hold` in `leviculum-std/src/interfaces/rnode.rs`
/// prices exactly this, one layer down, when it decides how long to hold the
/// next frame back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FramePacing {
    /// What the frame occupies on the air, preamble and header included.
    air_ms: u64,
    /// What the sending interface holds the NEXT frame back after handing
    /// this one to the modem: the frame's own airtime plus the contention the
    /// firmware runs before the frame behind it.
    hold_ms: u64,
}

/// How many radios fill the medium while a phase drains.
///
/// Both ends of this tool send, and the hold is per side: while one side sits
/// out its hold the other side's frame is on the air, so the medium carries a
/// frame roughly every half hold. Measured on the rig 2026-09-23 (run 179):
/// `LORA_TX_HOLD` paced each side at ~1.232 s per frame in all three ratchet
/// cells, and the interleaved medium carried one frame every ~0.616 s.
const INTERLEAVED_SENDERS: u64 = 2;

/// The contention window a burst meets, as a multiple of the band-1 ceiling
/// the interface reports as `tx_jitter_max`.
///
/// That ceiling is DIFS plus the widest of the 14 equally likely band-1 draws
/// — 15 slots in all. The modem does not stay in band 1 under a burst:
/// `update_csma_parameters` leaves it as soon as its own airtime share passes
/// `CSMA_BAND_1_MAX_AIRTIME` = 7 % (`RNode_Firmware.ino:1603-1607`), and the
/// next band's window is `cw_min = 15, cw_max = 29`
/// (`RNode_Firmware.ino:1616-1617` with `CSMA_CW_PER_BAND_WINDOWS` = 15,
/// `Config.h:110`), whose widest draw is 28 slots — 30 slots with DIFS,
/// exactly twice the band-1 ceiling.
///
/// Measured, not assumed: run 179 reported `LORA_TX_HOLD held_ms=863` in band
/// 1 and `held_ms=1223` in band 2 for the same 147-byte frame on a link whose
/// band-1 ceiling is 360 ms, and 1223 = 863 + 360.
///
/// A phase that drives the medium harder still (band 3 or 4) is priced short
/// by this, which is deliberate: the budget is meant to expire when the link
/// genuinely fails to keep up, and what lands after it is counted and printed
/// as late rather than thrown away ([`drain_single_packets`]).
const BURST_CONTENTION_BANDS: u64 = 2;

/// Price one frame at the link `profile` describes.
fn frame_pacing(wire_bytes: usize, profile: leviculum_core::transport::LinkProfile) -> FramePacing {
    let payload_ms = (wire_bytes as u64) * 8 * 1000 / profile.bitrate_bps as u64;
    let air_ms = payload_ms * FRAME_OVERHEAD_PERMILLE / 1000;
    let contention_ms = profile.tx_jitter_max_ms.unwrap_or(0) * BURST_CONTENTION_BANDS;
    FramePacing {
        air_ms,
        hold_ms: air_ms + contention_ms,
    }
}

/// How the interface priced the acquisition the budget above charges, in
/// words: a run document has to show what was priced, not only its total.
fn acquisition_shape(profile: leviculum_core::transport::LinkProfile, frame_air_ms: u64) -> String {
    match profile.acquisition {
        Some(AcquisitionCeiling {
            frame_slots: Some(slots),
            max_ms,
            ..
        }) => format!(
            "{slots} contention slots, each floored at the {:.2}s the frame is on \
             the air, at least {:.2}s",
            frame_air_ms as f64 / 1000.0,
            max_ms as f64 / 1000.0,
        ),
        Some(AcquisitionCeiling { max_ms, .. }) => format!(
            "{:.2}s of fixed contention slots, whatever the frame",
            max_ms as f64 / 1000.0
        ),
        None => "the daemon reports none, so taking the channel goes unaccounted".to_string(),
    }
}

/// Size the drain window for `frames` frames of `wire_bytes` each over the
/// link `profile` describes.
///
/// ```text
/// window = frames x max(air, hold / senders) + (hold + air) + senders x acquisition
/// ```
///
/// The first term is the medium's pace. A frame cannot cross faster than its
/// own airtime, and it cannot follow the one before it faster than the
/// sending interface's hold allows — the hold is what keeps the modem's queue
/// at one frame, so that a won contest flushes one frame rather than a whole
/// burst on a deaf radio. With both ends sending, one side's hold is served
/// while the other side transmits, hence `senders`.
///
/// The second term is the tail deferral: the last frame of a burst still has
/// to wait out the peer's hold before it is handed over, and then fly. Run
/// 179 measured 1.7 to 2.6 s of it in the three ratchet cells.
///
/// The third term is what taking the channel costs, once per sender: the
/// first frame each side enqueues serves an acquisition before it may be
/// handed over at all, and the rest of that side's burst rides the wait it
/// served. On an interface whose contention slots are priced in whole frames
/// it is the largest term here — trace 223 (2026-09-24) measured single
/// acquisitions of 5.03 s and 5.53 s on a link whose per-frame ceiling is
/// 360 ms, and the two arrivals that fell outside the unpriced window were
/// counted as losses by arithmetic rather than by the radio.
///
/// Falls back to `fallback` when the next hop reports no link profile: TCP,
/// UDP and Local have no airtime to account for, and nothing measured here
/// applies to them.
fn drain_budget(
    frames: u64,
    wire_bytes: usize,
    profile: Option<leviculum_core::transport::LinkProfile>,
    fallback: std::time::Duration,
) -> DrainBudget {
    let profile = match profile {
        Some(p) if p.bitrate_bps > 0 => p,
        _ => {
            return DrainBudget {
                total: fallback,
                grace: std::time::Duration::ZERO,
                detail: format!(
                    "no on-air bitrate to price airtime against; fixed {:.1}s, \
                     and nothing priced to wait out past it",
                    fallback.as_secs_f64()
                ),
            };
        }
    };

    let pacing = frame_pacing(wire_bytes, profile);
    let payload_ms = (wire_bytes as u64) * 8 * 1000 / profile.bitrate_bps as u64;
    let medium_ms = pacing.air_ms.max(pacing.hold_ms / INTERLEAVED_SENDERS);
    let tail_ms = pacing.hold_ms + pacing.air_ms;
    // One acquisition per sender: each side takes the channel once for the
    // burst it enqueues, and both sides' first frames are held by their own.
    let acquisition_each_ms = profile.acquisition_ceiling_ms(pacing.air_ms).unwrap_or(0);
    let acquisition_ms = INTERLEAVED_SENDERS * acquisition_each_ms;
    let total_ms = frames * medium_ms + tail_ms + acquisition_ms;

    DrainBudget {
        total: std::time::Duration::from_millis(total_ms),
        // One more hold plus airtime: what a frame already handed over needs
        // before it can be on the far side's radio, counted from expiry.
        grace: std::time::Duration::from_millis(tail_ms),
        detail: format!(
            "{frames} frames x {wire_bytes}B at {} bps: air {:.2}s/frame \
             (payload {:.2}s +{}% preamble/header), hold {:.2}s/frame \
             (air + {}x the {:.2}s pre-TX jitter ceiling, the band a burst \
             runs in), acquisition {:.2}s/sender ({}); window = frames x \
             max(air, hold/{}) + tail deferral (hold + air) + {} x \
             acquisition = {frames} x {:.2}s + {:.2}s + {:.2}s = {:.1}s",
            profile.bitrate_bps,
            pacing.air_ms as f64 / 1000.0,
            payload_ms as f64 / 1000.0,
            FRAME_OVERHEAD_PERMILLE / 10 - 100,
            pacing.hold_ms as f64 / 1000.0,
            BURST_CONTENTION_BANDS,
            profile.tx_jitter_max_ms.unwrap_or(0) as f64 / 1000.0,
            acquisition_each_ms as f64 / 1000.0,
            acquisition_shape(profile, pacing.air_ms),
            INTERLEAVED_SENDERS,
            INTERLEAVED_SENDERS,
            medium_ms as f64 / 1000.0,
            tail_ms as f64 / 1000.0,
            acquisition_ms as f64 / 1000.0,
            total_ms as f64 / 1000.0,
        ),
    }
}

/// Hold a single-packet phase open until its frames have drained, then return
/// how many are still outstanding.
///
/// The budget is sized for the frames that are actually still in flight when
/// the send loop ends — `expected` minus what the far sides have already
/// accounted for — and runs from that moment. A burst leaves nearly all of
/// them outstanding and gets the full window; a phase that sent below the
/// link's capacity leaves one or two and gets a short one. The same rule
/// therefore fits both without either being told which it is.
///
/// The count can only over-estimate what is left (receptions are counted as
/// they land, never retracted), so the budget errs toward waiting rather than
/// toward cutting a train off.
///
/// Expiry stops the counting, it does not end the run: the clients stay up
/// for one more hold plus airtime, and whatever lands in that window is
/// recorded as [`SelftestStats::sp_late`] — outside the delivery bar, because
/// the budget is what the bar is measured against, but printed, because a
/// frame that arrived 0.5 s late is not a frame that was lost. Tearing the
/// clients down at expiry instead made the far daemon decode a frame that was
/// already on the air and drop it as "no path known", while the verdict
/// printed `lost=0` (run 179, 2026-09-23).
///
/// Prints the derivation, and on expiry says what the expiry means. Returns
/// how many frames were still unaccounted for when the grace window closed,
/// 0 when everything drained.
async fn drain_single_packets(
    state: &SharedState,
    label: &str,
    expected: u64,
    wire_bytes: usize,
    sizing: &LinkSizing,
    fallback: std::time::Duration,
) -> u64 {
    let received_at_end = {
        let st = state.stats.lock().unwrap();
        st.sp_recv_a + st.sp_recv_b
    };
    let outstanding = expected.saturating_sub(received_at_end);
    let budget = drain_budget(outstanding, wire_bytes, sizing.profile, fallback);
    println!(
        "[selftest] {label}: drain budget from {}: {}",
        sizing.origin, budget.detail
    );

    let deadline = Instant::now() + budget.total;
    loop {
        let received = {
            let st = state.stats.lock().unwrap();
            st.sp_recv_a + st.sp_recv_b
        };
        if received >= expected {
            return 0;
        }
        let now = Instant::now();
        if now >= deadline {
            let left = expected - received;
            println!(
                "[selftest] {label}: budget expired with {left} of {expected} outstanding — \
                 the link did not deliver within its own computed budget ({:.1}s); this \
                 bounds delivery from below, it is not a loss count",
                budget.total.as_secs_f64(),
            );
            return count_late_arrivals(state, label, expected, received, budget.grace).await;
        }
        let step = std::time::Duration::from_millis(100).min(deadline - now);
        tokio::time::sleep(step).await;
    }
}

/// Keep the clients up past the budget and count what still arrives:
/// everything that lands from here on is late, not delivered.
///
/// `in_budget` is what the far sides had accounted for when the budget
/// expired — the count the verdict's delivery bar is computed from. The
/// arrivals of the grace window go to [`SelftestStats::sp_late`], which the
/// bar subtracts back out and the results line prints.
///
/// Returns what is still unaccounted for when the window closes.
async fn count_late_arrivals(
    state: &SharedState,
    label: &str,
    expected: u64,
    in_budget: u64,
    grace: std::time::Duration,
) -> u64 {
    if grace.is_zero() {
        println!(
            "[selftest] {label}: no on-air pacing priced, so nothing to wait out past the \
             budget — outstanding={} late=0",
            expected.saturating_sub(in_budget)
        );
        return expected.saturating_sub(in_budget);
    }
    println!(
        "[selftest] {label}: clients stay up {:.1}s past the budget (one hold + one airtime) — \
         what lands now is counted as late, not as delivered",
        grace.as_secs_f64()
    );

    let deadline = Instant::now() + grace;
    let received = loop {
        let received = {
            let st = state.stats.lock().unwrap();
            st.sp_recv_a + st.sp_recv_b
        };
        let now = Instant::now();
        if received >= expected || now >= deadline {
            break received;
        }
        let step = std::time::Duration::from_millis(100).min(deadline - now);
        tokio::time::sleep(step).await;
    };

    let late = received.saturating_sub(in_budget);
    state.stats.lock().unwrap().sp_late += late;
    let outstanding = expected.saturating_sub(received);
    println!(
        "[selftest] {label}: grace window closed — outstanding={outstanding} late={late} \
         (late frames arrived after the budget and are not in the delivery bar)"
    );
    outstanding
}

// Address Resolution
/// Resolve an address string to a SocketAddr, supporting both IP:port and hostname:port.
async fn resolve_address(addr: &str) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    // Try direct parse first (fast path for IP:port)
    if let Ok(sa) = addr.parse::<SocketAddr>() {
        return Ok(sa);
    }
    // DNS resolution for hostname:port
    let resolved = tokio::net::lookup_host(addr)
        .await
        .map_err(|e| format!("cannot resolve '{addr}': {e}"))?
        .next()
        .ok_or_else(|| format!("no addresses found for '{addr}'"))?;
    Ok(resolved)
}

// Main Entry Point
pub async fn run_selftest(
    targets: Vec<String>,
    duration: u64,
    rate: f64,
    mode: &str,
    corrupt_every: Option<u64>,
    discovery_timeout_secs: u64,
    config_dir: Option<std::path::PathBuf>,
) -> Result<Verdict, Box<dyn std::error::Error>> {
    let run_link = mode == "all" || mode == "link";
    let run_packet = mode == "all" || mode == "packet";
    let run_ratchet_basic = mode == "ratchet-basic";
    let run_ratchet_enforced = mode == "ratchet-enforced";
    let run_bulk_transfer = mode == "bulk-transfer";
    let run_ratchet_rotation = mode == "ratchet-rotation";
    let run_ratchet_any =
        run_ratchet_basic || run_ratchet_enforced || run_bulk_transfer || run_ratchet_rotation;

    if !run_link && !run_packet && !run_ratchet_any {
        return Err(format!(
            "invalid --mode '{mode}': expected all, link, packet, \
             ratchet-basic, ratchet-enforced, bulk-transfer, or ratchet-rotation"
        )
        .into());
    }

    if targets.is_empty() {
        return Err("at least one target address required".into());
    }

    let addr_a = resolve_address(&targets[0]).await?;
    let addr_b = if targets.len() > 1 {
        resolve_address(&targets[1]).await?
    } else {
        addr_a
    };

    let dual = addr_a != addr_b;

    // Phase 1: Setup
    if dual {
        println!(
            "[selftest] Client A -> {} / Client B -> {} (mode: {mode})",
            addr_a, addr_b
        );
    } else {
        println!("[selftest] Both clients -> {addr_a} (mode: {mode})");
    }
    if let Some(n) = corrupt_every {
        println!("[selftest] Fault injection: --corrupt-every {n} (deferred until after Phase 2)");
        disable_fault_injection();
    }

    // The numbers a single-packet phase sizes its drain window from live on the
    // daemon, not here: this tool is a TCP client two hops from the radio and
    // its own next hop has no airtime at all (Codeberg #190). Ask once, up
    // front, and say which state the run is in either way.
    let daemon_sizing = daemon_link_sizing(config_dir.as_deref()).await;
    println!("[selftest] Link sizing: {}", daemon_sizing.origin);

    // TCP pre-check
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect(addr_a),
    )
    .await
    .map_err(|_| format!("TCP connect timeout (10s) to {addr_a}"))?
    .map_err(|e| format!("cannot connect to {addr_a}: {e}"))?;

    if dual {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::net::TcpStream::connect(addr_b),
        )
        .await
        .map_err(|_| format!("TCP connect timeout (10s) to {addr_b}"))?
        .map_err(|e| format!("cannot connect to {addr_b}: {e}"))?;
    }

    // Two ephemeral identities, need two instances each (Identity is not Clone)
    use rand_core::OsRng;
    let id_a = Identity::generate(&mut OsRng);
    let pk_a = id_a.private_key_bytes().map_err(|e| e.to_string())?;
    let id_a2 = Identity::from_private_key_bytes(&pk_a).map_err(|e| e.to_string())?;

    let id_b = Identity::generate(&mut OsRng);
    let pk_b = id_b.private_key_bytes().map_err(|e| e.to_string())?;
    let id_b2 = Identity::from_private_key_bytes(&pk_b).map_err(|e| e.to_string())?;

    // Each selftest node uses a unique temp storage directory so it never
    // overwrites the daemon's ~/.reticulum/storage/transport_identity.
    // The TempDir handles must live until after node shutdown.
    let tmp_storage_a = tempfile::tempdir().map_err(|e| format!("tempdir A: {e}"))?;
    let tmp_storage_b = tempfile::tempdir().map_err(|e| format!("tempdir B: {e}"))?;

    // Build and start nodes
    let mut node_a = ReticulumNodeBuilder::new()
        .storage_path(tmp_storage_a.path().to_path_buf())
        .identity(id_a)
        .enable_transport(false)
        .add_tcp_client(addr_a)
        .corrupt_every(corrupt_every)
        .build()
        .await?;
    node_a.start().await?;

    let mut node_b = ReticulumNodeBuilder::new()
        .storage_path(tmp_storage_b.path().to_path_buf())
        .identity(id_b)
        .enable_transport(false)
        .add_tcp_client(addr_b)
        .corrupt_every(corrupt_every)
        .build()
        .await?;
    node_b.start().await?;

    // Register destinations with different app paths
    let mut dest_a = Destination::new(
        Some(id_a2),
        Direction::In,
        DestinationType::Single,
        "selftest",
        &["a"],
    )
    .map_err(|e| format!("destination error: {e}"))?;
    dest_a.set_accepts_links(true);
    if run_ratchet_any {
        dest_a
            .enable_ratchets(&mut OsRng, 0)
            .map_err(|e| format!("ratchet A: {e}"))?;
        if run_ratchet_enforced {
            dest_a.set_enforce_ratchets(true);
        }
        if run_ratchet_rotation {
            dest_a.set_ratchet_interval(5000);
        }
    }
    let dest_hash_a = *dest_a.hash();
    node_a.register_destination(dest_a);

    let mut dest_b = Destination::new(
        Some(id_b2),
        Direction::In,
        DestinationType::Single,
        "selftest",
        &["b"],
    )
    .map_err(|e| format!("destination error: {e}"))?;
    dest_b.set_accepts_links(true);
    if run_ratchet_any {
        dest_b
            .enable_ratchets(&mut OsRng, 0)
            .map_err(|e| format!("ratchet B: {e}"))?;
        if run_ratchet_enforced {
            dest_b.set_enforce_ratchets(true);
        }
        if run_ratchet_rotation {
            dest_b.set_ratchet_interval(5000);
        }
    }
    let dest_hash_b = *dest_b.hash();
    node_b.register_destination(dest_b);

    println!(
        "[selftest] Phase 1: OK — A={} B={}",
        crate::hex_encode(&dest_hash_a.as_bytes()[..8]),
        crate::hex_encode(&dest_hash_b.as_bytes()[..8]),
    );

    let start_time = Instant::now();
    let state = Arc::new(SharedState::new());
    let link_id_a: Arc<Mutex<Option<LinkId>>> = Arc::new(Mutex::new(None));

    // Phase 2: Discovery
    let event_rx_a = node_a.take_event_receiver().ok_or("event rx A")?;
    let event_rx_b = node_b.take_event_receiver().ok_or("event rx B")?;

    // Announce both
    node_a
        .announce_destination(&dest_hash_a, Some(APP_DATA_A))
        .await
        .map_err(|e| format!("announce A: {e}"))?;
    node_b
        .announce_destination(&dest_hash_b, Some(APP_DATA_B))
        .await
        .map_err(|e| format!("announce B: {e}"))?;

    // Spawn event tasks (run for entire test duration)
    let ev_state_a = Arc::clone(&state);
    let ev_link_a = Arc::clone(&link_id_a);
    let ev_task_a = tokio::spawn(event_task_a(
        event_rx_a,
        ev_state_a,
        dest_hash_b,
        ev_link_a,
        start_time,
    ));

    let ev_state_b = Arc::clone(&state);
    let ev_task_b = tokio::spawn(event_task_b(
        event_rx_b,
        ev_state_b,
        dest_hash_a,
        start_time,
    ));

    let discovery_start = Instant::now();

    // Wait for mutual discovery, announcing again once per cap slot until it
    // completes or the window ends.
    //
    // One announce per destination is one chance. A neighbour running the
    // reference stack that cannot air an announce right now puts it in its
    // per-interface announce queue (`reference/Reticulum/RNS/Transport.py:1283`,
    // the `should_queue` branch), and what becomes of the queued entry is its
    // business: the rig series of 2026-09-23 (runs 151 and 179) has rnsd
    // logging "in 17.77s" for the client's entry and never airing it, with no
    // error line. Nothing this tool ships can reach into that queue. What it
    // can do is send another announce once the cap has lapsed — an ordinary
    // announce the neighbour accepts, and its queue holds one entry per
    // destination, so a second one refreshes rather than stacks.
    let cap = announce_cap_slot(&daemon_sizing);
    println!("[selftest] Phase 2: re-announce slot {}", cap.detail);

    let discovery = async {
        tokio::join!(
            state.a_discovered_b.notified(),
            state.b_discovered_a.notified()
        );
    };
    tokio::pin!(discovery);

    let window = std::time::Duration::from_secs(discovery_timeout_secs);
    let deadline = discovery_start + window;
    let mut re_announces: u32 = 0;
    let discovered = loop {
        let now = Instant::now();
        if now >= deadline {
            break false;
        }
        // The next slot boundary, never past the window: the window is the
        // cap, so this loop cannot outlive it however short the slot is.
        let next_slot = discovery_start + cap.slot * (re_announces + 1);
        let wait = next_slot.min(deadline).saturating_duration_since(now);
        let done = tokio::select! {
            _ = &mut discovery => true,
            _ = tokio::time::sleep(wait) => false,
        };
        if done {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        re_announces += 1;
        node_a
            .announce_destination(&dest_hash_a, Some(APP_DATA_A))
            .await
            .map_err(|e| format!("re-announce A: {e}"))?;
        node_b
            .announce_destination(&dest_hash_b, Some(APP_DATA_B))
            .await
            .map_err(|e| format!("re-announce B: {e}"))?;
        println!(
            "[selftest] Phase 2: no mutual discovery after {:.1}s — announced A and B \
             again (re-announce {re_announces}, one per {:.1}s cap slot)",
            discovery_start.elapsed().as_secs_f64(),
            cap.slot.as_secs_f64(),
        );
    };
    if !discovered {
        return Err(format!(
            "Phase 2 timeout: discovery took >{discovery_timeout_secs}s \
             ({re_announces} re-announce(s) sent, one per {:.1}s cap slot)",
            cap.slot.as_secs_f64(),
        )
        .into());
    }

    let discovery_time = discovery_start.elapsed();
    let hops = node_a.hops_to(&dest_hash_b).unwrap_or(0);
    println!(
        "[selftest] Phase 2: OK — path found in {:.1}s ({} hops)",
        discovery_time.as_secs_f64(),
        hops,
    );

    if corrupt_every.is_some() {
        enable_fault_injection();
        println!("[selftest] Fault injection: activated post-Phase-2");
    }

    let interval_ms = if rate > 0.0 {
        (1000.0 / rate) as u64
    } else {
        1000
    };

    // Variables used by the report, assigned by the link phases or defaulted
    let mut link_warnings: Vec<String> = Vec::new();
    let mut final_win = 0usize;
    let mut final_win_max = 0usize;

    if run_link {
        // Phase 3: Link
        let link_start = Instant::now();

        let signing_key_b = state
            .b_signing_key
            .lock()
            .unwrap()
            .ok_or("no signing key for B")?;

        let stream_a = node_a.connect(&dest_hash_b, &signing_key_b).await?;

        // Link-request retry budget: (1 + max(LINK_REQUEST_MAX_RETRIES, hops))
        // attempts, each up to ESTABLISHMENT_TIMEOUT_PER_HOP_MS × (hops + 1) ms.
        // Add 25% margin for scheduling jitter.
        let effective_hops = core::cmp::max(1, hops as u64);
        let effective_retries = core::cmp::max(
            leviculum_core::constants::LINK_REQUEST_MAX_RETRIES as u64,
            effective_hops,
        );
        let per_attempt_ms =
            leviculum_core::constants::ESTABLISHMENT_TIMEOUT_PER_HOP_MS * (effective_hops + 1);
        let total_attempts = 1 + effective_retries;
        let retry_budget_secs = (per_attempt_ms * total_attempts * 5 / 4) / 1000;
        // Floor at 60s for fast paths (direct TCP, 1 hop).
        let phase3_timeout_secs = core::cmp::max(60, retry_budget_secs);

        // Auto-accept (Python parity): B's stack proves every incoming link
        // request inline, including the re-keyed retries the initiator sends when
        // a proof is lost on a cold path (Codeberg #66). The responder no longer
        // accepts anything; it just waits for its own LinkEstablished event and
        // then mints a writable handle for that link.
        tokio::time::timeout(
            std::time::Duration::from_secs(phase3_timeout_secs),
            state.link_established_b.notified(),
        )
        .await
        .map_err(|_| {
            format!("Phase 3 timeout: link not established on B >{phase3_timeout_secs}s")
        })?;

        let established_b_id = state
            .established_link_b
            .lock()
            .unwrap()
            .ok_or("no established link on B")?;
        let stream_b = node_b.link_handle(&established_b_id);

        println!(
            "[selftest] Phase 3: OK — link established in {:.1}s",
            link_start.elapsed().as_secs_f64(),
        );

        // Phase 4: Warmup
        let warmup_start = Instant::now();
        let warmup_msgs = 10u64;

        for seq in 0..warmup_msgs {
            send_msg(&stream_a, "ab", seq, start_time, &state, true).await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        // Wait for at least 5 confirmations (up to 30s)
        let warmup_deadline = Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let confirmed = state.stats.lock().unwrap().confirmed_a;
            if confirmed >= 5 {
                break;
            }
            if Instant::now() > warmup_deadline {
                if confirmed == 0 {
                    println!(
                        "[selftest] Phase 4: FAIL — 0 confirmations after 30s (proofs not arriving)"
                    );
                    cleanup(&mut node_a, &mut node_b, &ev_task_a, &ev_task_b).await;
                    std::process::exit(1);
                }
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        // Check window
        let link_id = link_id_a.lock().unwrap().ok_or("no link_id on A")?;
        let window = node_a.link_stats(&link_id).map(|s| s.window()).unwrap_or(0);

        if window <= 2 && Instant::now() > warmup_deadline {
            println!(
                "[selftest] Phase 4: FAIL — window still {} after warmup",
                window
            );
            cleanup(&mut node_a, &mut node_b, &ev_task_a, &ev_task_b).await;
            std::process::exit(1);
        }

        println!(
            "[selftest] Phase 4: OK — warmup {}/{}, window={} ({:.1}s)",
            state.stats.lock().unwrap().confirmed_a,
            warmup_msgs,
            window,
            warmup_start.elapsed().as_secs_f64(),
        );

        // Phase 5: Sustained exchange
        println!("[selftest] Phase 5: Sustained link exchange ({duration}s)");

        let mut seq_a = warmup_msgs;
        let mut seq_b = 0u64;
        let mut last_recv_a = 0u64;
        let mut last_recv_b = 0u64;
        let mut zero_recv_streak = 0u32;

        let phase5_start = Instant::now();
        let phase5_end = phase5_start + std::time::Duration::from_secs(duration);
        let mut next_send = tokio::time::Instant::now();
        let mut next_health = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut elapsed_checks = 0u64;

        loop {
            let now = Instant::now();
            if now >= phase5_end {
                break;
            }
            if state.link_dead.load(Ordering::Relaxed) {
                break;
            }

            let send_due = tokio::time::Instant::now() >= next_send;
            let health_due = tokio::time::Instant::now() >= next_health;

            if send_due {
                send_msg(&stream_a, "ab", seq_a, start_time, &state, true).await;
                seq_a += 1;
                send_msg(&stream_b, "ba", seq_b, start_time, &state, false).await;
                seq_b += 1;
                next_send =
                    tokio::time::Instant::now() + std::time::Duration::from_millis(interval_ms);
            }

            if health_due {
                elapsed_checks += 15;
                next_health = tokio::time::Instant::now() + std::time::Duration::from_secs(15);

                let st = state.stats.lock().unwrap();
                let recv_a = st.recv_a;
                let recv_b = st.recv_b;
                let conf_a = st.confirmed_a;
                let conf_b = st.confirmed_b;
                let fails = st.send_fails_a + st.send_fails_b;
                let corrupt = st.corrupt;
                let oo = st.out_of_order;
                let dupes = st.duplicates;
                let total_sent = st.sent_a + st.sent_b;
                let retx = st.retransmits_a + st.retransmits_b;
                drop(st);

                let win = node_a.link_stats(&link_id).map(|s| s.window()).unwrap_or(0);

                let recv_progress = recv_a > last_recv_a || recv_b > last_recv_b;

                let mut status = "OK";
                if !recv_progress && total_sent > 0 {
                    zero_recv_streak += 1;
                    if zero_recv_streak >= 2 {
                        status = "WARN";
                        link_warnings.push(format!("+{}s: no recv in 30s", elapsed_checks));
                    }
                } else {
                    zero_recv_streak = 0;
                }

                if corrupt > 0 || oo > 0 || dupes > 0 {
                    status = "WARN";
                }

                println!(
                    "[selftest]   +{elapsed_checks}s:  sent={total_sent}  recv={}  ack={}  fails={fails}  retx={retx}  win={win} — {status}",
                    recv_a + recv_b,
                    conf_a + conf_b,
                );

                last_recv_a = recv_a;
                last_recv_b = recv_b;
            }

            if !send_due && !health_due {
                let sleep_until = next_send.min(next_health);
                tokio::time::sleep_until(sleep_until).await;
            }
        }

        {
            let st = state.stats.lock().unwrap();
            let total_sent = st.sent_a + st.sent_b;
            println!("[selftest] Phase 5 complete: sent {total_sent} messages, entering drain");
        }

        final_win = node_a.link_stats(&link_id).map(|s| s.window()).unwrap_or(0);
        final_win_max = node_a
            .link_stats(&link_id)
            .map(|s| s.window_max())
            .unwrap_or(0);

        if state.link_dead.load(Ordering::Relaxed) {
            println!("[selftest] Phase 6: Skipped (link dead)");
            println!("[selftest] Phase 7: Skipped (link dead)");
        } else {
            // Phase 6: Burst
            // Send 10 messages as fast as possible. send() absorbs
            // pacing delays and busy conditions automatically.
            let mut burst_ok = 0u64;
            for seq in 0..10u64 {
                let msg = build_message(
                    Phase::Link,
                    "ab",
                    10000 + seq,
                    start_time.elapsed().as_millis() as u64,
                );
                match tokio::time::timeout(std::time::Duration::from_secs(10), stream_a.send(&msg))
                    .await
                {
                    Ok(Ok(())) => {
                        state.stats.lock().unwrap().sent_a += 1;
                        burst_ok += 1;
                    }
                    _ => {
                        state.stats.lock().unwrap().send_fails_a += 1;
                    }
                }
            }

            if burst_ok < 10 {
                println!(
                    "[selftest] Phase 6: Burst {burst_ok}/10 — WARN ({} not sent)",
                    10 - burst_ok
                );
                link_warnings.push(format!("burst: {}/10 not sent", 10 - burst_ok));
            } else {
                println!("[selftest] Phase 6: Burst 10/10 — OK");
            }

            // Phase 7: Drain + Close
            let drain_start = Instant::now();
            let drain_max = std::time::Duration::from_secs(120);
            let mut drain_last_print = Instant::now();
            let mut drain_last_recv: u64;
            let mut drain_last_ack: u64;
            let mut drain_stagnant_since = Instant::now();
            let stagnation_limit = std::time::Duration::from_secs(30);

            // Initialize with current values
            {
                let st = state.stats.lock().unwrap();
                drain_last_recv = st.recv_a + st.recv_b;
                drain_last_ack = st.confirmed_a + st.confirmed_b;
            }

            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                let (total_sent, total_recv, total_ack) = {
                    let st = state.stats.lock().unwrap();
                    (
                        st.sent_a + st.sent_b,
                        st.recv_a + st.recv_b,
                        st.confirmed_a + st.confirmed_b,
                    )
                };

                // Exit: all messages received
                if total_recv >= total_sent {
                    println!("[selftest] Phase 7: All messages received — closing");
                    break;
                }

                // Exit: max timeout
                if drain_start.elapsed() > drain_max {
                    println!(
                        "[selftest] Phase 7: Drain timeout (120s) — recv={total_recv}/{total_sent} ack={total_ack}/{total_sent}"
                    );
                    break;
                }

                // Stagnation detection
                if total_recv != drain_last_recv || total_ack != drain_last_ack {
                    drain_last_recv = total_recv;
                    drain_last_ack = total_ack;
                    drain_stagnant_since = Instant::now();
                } else if drain_stagnant_since.elapsed() > stagnation_limit {
                    println!(
                        "[selftest] Phase 7: Stagnant 30s — recv={total_recv}/{total_sent} ack={total_ack}/{total_sent}"
                    );
                    break;
                }

                // Progress print every 15s
                if drain_last_print.elapsed() >= std::time::Duration::from_secs(15) {
                    let elapsed = drain_start.elapsed().as_secs();
                    println!(
                        "[selftest]   drain +{elapsed}s: recv={total_recv}/{total_sent} ack={total_ack}/{total_sent}"
                    );
                    drain_last_print = Instant::now();
                }
            }

            final_win = node_a.link_stats(&link_id).map(|s| s.window()).unwrap_or(0);
            final_win_max = node_a
                .link_stats(&link_id)
                .map(|s| s.window_max())
                .unwrap_or(0);

            // Close from B side
            if let Err(e) = node_b.close_link(stream_b.link_id()).await {
                eprintln!("[selftest] close B: {e}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            println!("[selftest] Phase 7: Close link — OK");
        }
    }

    let mut sp_warnings: Vec<String> = Vec::new();

    if run_packet {
        // Phase 8: Single-packet sustained exchange
        println!("[selftest] Phase 8: Single-packet exchange ({duration}s)");
        state.single_packet_phase.store(true, Ordering::Relaxed);

        let ep_a = node_a.packet_sender(&dest_hash_b);
        let ep_b = node_b.packet_sender(&dest_hash_a);

        let mut sp_seq_a = 0u64;
        let mut sp_seq_b = 0u64;
        let mut sp_last_recv_a = 0u64;
        let mut sp_last_recv_b = 0u64;
        let mut sp_zero_recv_streak = 0u32;
        let mut sp_wire_bytes = 0usize;

        let phase8_start = Instant::now();
        let phase8_end = phase8_start + std::time::Duration::from_secs(duration);
        let mut next_send = tokio::time::Instant::now();
        let mut next_health = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut sp_elapsed_checks = 0u64;

        loop {
            let now = Instant::now();
            if now >= phase8_end {
                break;
            }

            let send_due = tokio::time::Instant::now() >= next_send;
            let health_due = tokio::time::Instant::now() >= next_health;

            if send_due {
                if let Some(n) =
                    send_single_msg(&ep_a, "ab", sp_seq_a, start_time, &state, true).await
                {
                    sp_wire_bytes = sp_wire_bytes.max(n);
                }
                sp_seq_a += 1;
                if let Some(n) =
                    send_single_msg(&ep_b, "ba", sp_seq_b, start_time, &state, false).await
                {
                    sp_wire_bytes = sp_wire_bytes.max(n);
                }
                sp_seq_b += 1;
                next_send =
                    tokio::time::Instant::now() + std::time::Duration::from_millis(interval_ms);
            }

            if health_due {
                sp_elapsed_checks += 15;
                next_health = tokio::time::Instant::now() + std::time::Duration::from_secs(15);

                let st = state.stats.lock().unwrap();
                let recv_a = st.sp_recv_a;
                let recv_b = st.sp_recv_b;
                let fails = st.sp_send_fails_a + st.sp_send_fails_b;
                let total_sent = st.sp_sent_a + st.sp_sent_b;
                drop(st);

                let recv_progress = recv_a > sp_last_recv_a || recv_b > sp_last_recv_b;

                let mut status = "OK";
                if !recv_progress && total_sent > 0 {
                    sp_zero_recv_streak += 1;
                    if sp_zero_recv_streak >= 2 {
                        status = "WARN";
                        sp_warnings.push(format!("+{}s: no recv in 30s", sp_elapsed_checks));
                    }
                } else {
                    sp_zero_recv_streak = 0;
                }

                println!(
                    "[selftest]   +{sp_elapsed_checks}s:  sent={total_sent}  recv={}  fails={fails} — {status}",
                    recv_a + recv_b,
                );

                sp_last_recv_a = recv_a;
                sp_last_recv_b = recv_b;
            }

            if !send_due && !health_due {
                let sleep_until = next_send.min(next_health);
                tokio::time::sleep_until(sleep_until).await;
            }
        }

        // Let what is still in flight land before the counter is read. At a
        // send rate above the link's capacity this phase ends with most of
        // its frames still queued, and a fixed wait counts them as lost.
        let sp_expected = {
            let st = state.stats.lock().unwrap();
            st.sp_sent_a + st.sp_sent_b
        };
        drain_single_packets(
            &state,
            "Phase 8",
            sp_expected,
            on_air_bytes(sp_wire_bytes, node_a.hops_to(&dest_hash_b)),
            &link_sizing(node_a.next_hop_link_profile(&dest_hash_b), &daemon_sizing),
            std::time::Duration::from_secs(5),
        )
        .await;
    }

    // Ratchet Phases
    let mut ratchet_verdict: Option<Verdict> = None;

    if run_ratchet_any {
        let ratchet_start = Instant::now();

        // Precondition: a ratchet-enforced receiver REJECTS non-ratchet packets, so a
        // sender that never learned the peer's KNOWN REMOTE ratchet (from a ratcheted
        // announce) drops to 0%. The old blind 2s wait raced this. Poll the actual
        // send-path lookup until BOTH directions have the peer's ratchet, re-announcing
        // the owning peer whenever a side lags.
        let ratchet_ready_timeout = std::time::Duration::from_secs(30);
        let poll_start = Instant::now();
        let mut poll_iter = 0u32;
        loop {
            let a_has_b = node_a.known_remote_ratchet(&dest_hash_b).is_some();
            let b_has_a = node_b.known_remote_ratchet(&dest_hash_a).is_some();
            if a_has_b && b_has_a {
                break;
            }
            if poll_start.elapsed() >= ratchet_ready_timeout {
                return Err(format!(
                    "Ratchet precondition timeout: known-remote-ratchet not learned after {}s \
                     (A_has_B={a_has_b} B_has_A={b_has_a})",
                    ratchet_ready_timeout.as_secs()
                )
                .into());
            }
            // Every ~4s (8 × 500ms), re-announce the peer whose ratchet is still missing.
            // A missing B's ratchet -> B re-announces its own destination, and vice versa.
            if poll_iter > 0 && poll_iter.is_multiple_of(8) {
                if !a_has_b {
                    node_b
                        .announce_destination(&dest_hash_b, Some(b"selftest-b"))
                        .await
                        .map_err(|e| format!("ratchet re-announce B: {e}"))?;
                }
                if !b_has_a {
                    node_a
                        .announce_destination(&dest_hash_a, Some(b"selftest-a"))
                        .await
                        .map_err(|e| format!("ratchet re-announce A: {e}"))?;
                }
            }
            poll_iter += 1;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        println!(
            "[selftest] RATCHET-READY: A_has_B={} B_has_A={}",
            node_a.known_remote_ratchet(&dest_hash_b).is_some(),
            node_b.known_remote_ratchet(&dest_hash_a).is_some(),
        );

        let ep_a = node_a.packet_sender(&dest_hash_b);
        let ep_b = node_b.packet_sender(&dest_hash_a);

        let pass_threshold = if run_bulk_transfer {
            80.0
        } else if run_ratchet_enforced && corrupt_every.is_some() {
            50.0
        } else {
            90.0
        };

        if run_ratchet_basic || run_ratchet_enforced {
            let msg_count = 10u64;
            println!(
                "[selftest] Ratchet: sending {msg_count} messages each direction (threshold: {pass_threshold:.0}%)"
            );

            state.single_packet_phase.store(true, Ordering::Relaxed);
            // Reset single-packet stats
            {
                let mut st = state.stats.lock().unwrap();
                st.sp_sent_a = 0;
                st.sp_sent_b = 0;
                st.sp_recv_a = 0;
                st.sp_recv_b = 0;
                st.sp_send_fails_a = 0;
                st.sp_send_fails_b = 0;
                st.sp_corrupt = 0;
                st.sp_late = 0;
            }

            let mut wire_bytes = 0usize;
            for seq in 0..msg_count {
                if let Some(n) = send_single_msg(&ep_a, "ab", seq, start_time, &state, true).await {
                    wire_bytes = wire_bytes.max(n);
                }
                if let Some(n) = send_single_msg(&ep_b, "ba", seq, start_time, &state, false).await
                {
                    wire_bytes = wire_bytes.max(n);
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }

            drain_single_packets(
                &state,
                &format!("Ratchet {mode}"),
                msg_count * 2,
                on_air_bytes(wire_bytes, node_a.hops_to(&dest_hash_b)),
                &link_sizing(node_a.next_hop_link_profile(&dest_hash_b), &daemon_sizing),
                std::time::Duration::from_secs(10),
            )
            .await;

            let (total_sent, total_recv, late, corrupt) = {
                let st = state.stats.lock().unwrap();
                (
                    st.sp_sent_a + st.sp_sent_b,
                    st.sp_in_budget_recv(),
                    st.sp_late,
                    st.sp_corrupt,
                )
            };

            let recv_pct = if total_sent > 0 {
                (total_recv as f64 / total_sent as f64) * 100.0
            } else {
                0.0
            };

            let verdict = if recv_pct >= pass_threshold && corrupt == 0 {
                Verdict::Pass
            } else if total_recv > 0 {
                Verdict::Warn
            } else {
                Verdict::Fail
            };

            println!(
                "[selftest] Ratchet {mode}: sent={total_sent} recv={total_recv} ({recv_pct:.1}%) late={late} corrupt={corrupt} — {verdict}"
            );
            ratchet_verdict = Some(verdict);
        } else if run_bulk_transfer {
            let msg_count = 100u64;
            println!(
                "[selftest] Ratchet: bulk transfer {msg_count} messages each direction (threshold: {pass_threshold:.0}%)"
            );

            state.single_packet_phase.store(true, Ordering::Relaxed);
            {
                let mut st = state.stats.lock().unwrap();
                st.sp_sent_a = 0;
                st.sp_sent_b = 0;
                st.sp_recv_a = 0;
                st.sp_recv_b = 0;
                st.sp_send_fails_a = 0;
                st.sp_send_fails_b = 0;
                st.sp_corrupt = 0;
                st.sp_late = 0;
            }

            for seq in 0..msg_count {
                send_single_msg(&ep_a, "ab", seq, start_time, &state, true).await;
                send_single_msg(&ep_b, "ba", seq, start_time, &state, false).await;
                // Slight pacing to avoid overwhelming
                if seq % 10 == 9 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }

            // Wait for delivery
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;

            let (total_sent, total_recv, late, corrupt) = {
                let st = state.stats.lock().unwrap();
                (
                    st.sp_sent_a + st.sp_sent_b,
                    st.sp_in_budget_recv(),
                    st.sp_late,
                    st.sp_corrupt,
                )
            };

            let recv_pct = if total_sent > 0 {
                (total_recv as f64 / total_sent as f64) * 100.0
            } else {
                0.0
            };

            let verdict = if recv_pct >= pass_threshold && corrupt == 0 {
                Verdict::Pass
            } else if total_recv > 0 {
                Verdict::Warn
            } else {
                Verdict::Fail
            };

            println!(
                "[selftest] Ratchet bulk-transfer: sent={total_sent} recv={total_recv} ({recv_pct:.1}%) late={late} corrupt={corrupt} — {verdict}"
            );
            ratchet_verdict = Some(verdict);
        } else if run_ratchet_rotation {
            println!("[selftest] Ratchet rotation: pre-rotation exchange...");

            // Capture ratchet keys before rotation
            let ratchet_before_a = node_a.destination_ratchet_public(&dest_hash_a);
            let ratchet_before_b = node_b.destination_ratchet_public(&dest_hash_b);

            state.single_packet_phase.store(true, Ordering::Relaxed);
            {
                let mut st = state.stats.lock().unwrap();
                st.sp_sent_a = 0;
                st.sp_sent_b = 0;
                st.sp_recv_a = 0;
                st.sp_recv_b = 0;
                st.sp_send_fails_a = 0;
                st.sp_send_fails_b = 0;
                st.sp_corrupt = 0;
                st.sp_late = 0;
            }

            // Pre-rotation exchange
            let mut wire_bytes = 0usize;
            for seq in 0..5u64 {
                if let Some(n) = send_single_msg(&ep_a, "ab", seq, start_time, &state, true).await {
                    wire_bytes = wire_bytes.max(n);
                }
                if let Some(n) = send_single_msg(&ep_b, "ba", seq, start_time, &state, false).await
                {
                    wire_bytes = wire_bytes.max(n);
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            drain_single_packets(
                &state,
                "Ratchet rotation pre-rotation",
                10,
                on_air_bytes(wire_bytes, node_a.hops_to(&dest_hash_b)),
                &link_sizing(node_a.next_hop_link_profile(&dest_hash_b), &daemon_sizing),
                std::time::Duration::from_secs(5),
            )
            .await;

            let (pre_recv, pre_late, pre_sent) = {
                let st = state.stats.lock().unwrap();
                (
                    st.sp_in_budget_recv(),
                    st.sp_late,
                    st.sp_sent_a + st.sp_sent_b,
                )
            };

            let pre_pct = if pre_sent > 0 {
                (pre_recv as f64 / pre_sent as f64) * 100.0
            } else {
                0.0
            };
            println!(
                "[selftest] Ratchet rotation: pre-rotation sent={pre_sent} recv={pre_recv} ({pre_pct:.1}%) late={pre_late}"
            );

            // Sleep to let ratchet interval expire (interval = 5s)
            println!("[selftest] Ratchet rotation: sleeping 6s for interval expiry...");
            tokio::time::sleep(std::time::Duration::from_secs(6)).await;

            // Re-announce to trigger rotation
            println!("[selftest] Ratchet rotation: re-announcing to trigger rotation...");
            node_a
                .announce_destination(&dest_hash_a, Some(b"selftest-a"))
                .await
                .map_err(|e| format!("re-announce A: {e}"))?;
            node_b
                .announce_destination(&dest_hash_b, Some(b"selftest-b"))
                .await
                .map_err(|e| format!("re-announce B: {e}"))?;

            // Wait for announce propagation
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            // Verify rotation happened
            let ratchet_after_a = node_a.destination_ratchet_public(&dest_hash_a);
            let ratchet_after_b = node_b.destination_ratchet_public(&dest_hash_b);

            let rotation_a = ratchet_before_a != ratchet_after_a;
            let rotation_b = ratchet_before_b != ratchet_after_b;

            if !rotation_a || !rotation_b {
                println!(
                    "[selftest] Ratchet rotation: FAIL — rotation did not happen (A={rotation_a} B={rotation_b})"
                );
                ratchet_verdict = Some(Verdict::Fail);
            } else {
                println!("[selftest] Ratchet rotation: keys rotated — exchanging post-rotation...");

                // Reset stats for post-rotation exchange
                {
                    let mut st = state.stats.lock().unwrap();
                    st.sp_sent_a = 0;
                    st.sp_sent_b = 0;
                    st.sp_recv_a = 0;
                    st.sp_recv_b = 0;
                    st.sp_send_fails_a = 0;
                    st.sp_send_fails_b = 0;
                    st.sp_corrupt = 0;
                    st.sp_late = 0;
                }

                let mut wire_bytes = 0usize;
                for seq in 100..105u64 {
                    if let Some(n) =
                        send_single_msg(&ep_a, "ab", seq, start_time, &state, true).await
                    {
                        wire_bytes = wire_bytes.max(n);
                    }
                    if let Some(n) =
                        send_single_msg(&ep_b, "ba", seq, start_time, &state, false).await
                    {
                        wire_bytes = wire_bytes.max(n);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                drain_single_packets(
                    &state,
                    "Ratchet rotation post-rotation",
                    10,
                    on_air_bytes(wire_bytes, node_a.hops_to(&dest_hash_b)),
                    &link_sizing(node_a.next_hop_link_profile(&dest_hash_b), &daemon_sizing),
                    std::time::Duration::from_secs(5),
                )
                .await;

                let (post_sent, post_recv, post_late, post_corrupt) = {
                    let st = state.stats.lock().unwrap();
                    (
                        st.sp_sent_a + st.sp_sent_b,
                        st.sp_in_budget_recv(),
                        st.sp_late,
                        st.sp_corrupt,
                    )
                };

                let post_pct = if post_sent > 0 {
                    (post_recv as f64 / post_sent as f64) * 100.0
                } else {
                    0.0
                };

                let verdict =
                    if pre_pct >= pass_threshold && post_pct >= pass_threshold && post_corrupt == 0
                    {
                        Verdict::Pass
                    } else if post_recv > 0 {
                        Verdict::Warn
                    } else {
                        Verdict::Fail
                    };

                println!(
                    "[selftest] Ratchet rotation: post-rotation sent={post_sent} recv={post_recv} ({post_pct:.1}%) late={post_late} corrupt={post_corrupt} — {verdict}"
                );
                ratchet_verdict = Some(verdict);
            }
        }

        println!(
            "[selftest] Ratchet phase completed in {:.1}s",
            ratchet_start.elapsed().as_secs_f64()
        );
    }

    // Report
    let total_time = start_time.elapsed();
    let mut verdicts: Vec<Verdict> = Vec::new();

    println!("[selftest] ══════════════════════════════════════════════════");
    println!(
        "[selftest]  Duration:      {:.1}s",
        total_time.as_secs_f64()
    );
    if let Some(n) = corrupt_every {
        println!("[selftest]  Fault inject:  ~1 byte per {n} bytes");
    }

    if run_link {
        let st = state.stats.lock().unwrap();
        let total_sent = st.sent_a + st.sent_b;
        let total_recv = st.recv_a + st.recv_b;
        let total_confirmed = st.confirmed_a + st.confirmed_b;
        let total_fails = st.send_fails_a + st.send_fails_b;

        let recv_pct = if total_sent > 0 {
            (total_recv as f64 / total_sent as f64) * 100.0
        } else {
            0.0
        };
        let conf_pct = if total_sent > 0 {
            (total_confirmed as f64 / total_sent as f64) * 100.0
        } else {
            0.0
        };

        let stale_count = st.stale_count;
        let recovered_count = st.recovered_count;
        let corrupt = st.corrupt;
        let oo = st.out_of_order;
        let dupes = st.duplicates;

        let fail_rate = if total_sent > 0 {
            total_fails as f64 / total_sent as f64
        } else {
            0.0
        };
        drop(st);

        if fail_rate > 0.05 {
            link_warnings.push(format!("send fail rate {:.1}%", fail_rate * 100.0));
        }
        if stale_count > 0 && recovered_count < stale_count {
            link_warnings.push(format!("stale={stale_count} recovered={recovered_count}"));
        }
        if corrupt > 0 {
            link_warnings.push(format!("corrupt={corrupt}"));
        }
        if dupes > 0 {
            link_warnings.push(format!("duplicates={dupes}"));
        }
        if oo > 0 {
            link_warnings.push(format!("out_of_order={oo}"));
        }

        let link_verdict = {
            let st = state.stats.lock().unwrap();
            compute_link_verdict(&st, &link_warnings)
        };
        verdicts.push(link_verdict);

        println!("[selftest] ──────────────────────────────────────────────────");
        println!("[selftest]  RESULTS — Link Phase");
        println!(
            "[selftest]  Messages:      sent={total_sent} recv={total_recv} ({recv_pct:.1}%) ack={total_confirmed} ({conf_pct:.1}%)"
        );
        println!(
            "[selftest]  Integrity:     corrupt={corrupt} out_of_order={oo} duplicates={dupes}"
        );
        println!("[selftest]  Send fails:    {total_fails} Busy");
        println!("[selftest]  Window:        final={final_win} (max={final_win_max})");
        let retransmits = {
            let st = state.stats.lock().unwrap();
            (st.retransmits_a, st.retransmits_b)
        };
        println!(
            "[selftest]  Retransmits:   a={} b={} total={}",
            retransmits.0,
            retransmits.1,
            retransmits.0 + retransmits.1,
        );
        println!("[selftest]  Link events:   stale={stale_count} recovered={recovered_count}");
        if let Some(dead_at) = *state.link_dead_elapsed_secs.lock().unwrap() {
            println!("[selftest]  Link death:    +{dead_at}s");
        }
        if !link_warnings.is_empty() {
            for w in &link_warnings {
                println!("[selftest]  Warning:       {w}");
            }
        }
        println!("[selftest]  Verdict:       {link_verdict}");
    }

    if run_packet {
        let st = state.stats.lock().unwrap();
        let total_sent = st.sp_sent_a + st.sp_sent_b;
        let total_recv = st.sp_in_budget_recv();
        let late = st.sp_late;
        let total_fails = st.sp_send_fails_a + st.sp_send_fails_b;

        let recv_pct = if total_sent > 0 {
            (total_recv as f64 / total_sent as f64) * 100.0
        } else {
            0.0
        };

        let corrupt = st.sp_corrupt;
        let oo = st.sp_out_of_order;
        let dupes = st.sp_duplicates;
        drop(st);

        if corrupt > 0 {
            sp_warnings.push(format!("corrupt={corrupt}"));
        }
        if dupes > 0 {
            sp_warnings.push(format!("duplicates={dupes}"));
        }
        if oo > 0 {
            sp_warnings.push(format!("out_of_order={oo}"));
        }

        let sp_verdict = {
            let st = state.stats.lock().unwrap();
            compute_sp_verdict(&st, &sp_warnings)
        };
        verdicts.push(sp_verdict);

        println!("[selftest] ──────────────────────────────────────────────────");
        println!("[selftest]  RESULTS — Single-Packet Phase");
        println!(
            "[selftest]  Messages:      sent={total_sent} recv={total_recv} ({recv_pct:.1}%) late={late}"
        );
        println!(
            "[selftest]  Integrity:     corrupt={corrupt} out_of_order={oo} duplicates={dupes}"
        );
        println!("[selftest]  Send fails:    {total_fails} NoPath");
        if !sp_warnings.is_empty() {
            for w in &sp_warnings {
                println!("[selftest]  Warning:       {w}");
            }
        }
        println!("[selftest]  Verdict:       {sp_verdict}");
    }

    if let Some(rv) = ratchet_verdict {
        verdicts.push(rv);
        println!("[selftest] ──────────────────────────────────────────────────");
        println!("[selftest]  RESULTS — Ratchet Phase ({mode})");
        println!("[selftest]  Verdict:       {rv}");
    }

    println!("[selftest] ══════════════════════════════════════════════════");

    // Cleanup
    ev_task_a.abort();
    ev_task_b.abort();
    node_a.stop().await?;
    node_b.stop().await?;

    // The worst verdict is the run's verdict; the caller decides what it
    // costs (the binary turns a `Fail` into exit code 1).
    Ok(verdicts.into_iter().max().unwrap_or(Verdict::Pass))
}

async fn cleanup(
    node_a: &mut leviculum_std::driver::ReticulumNode,
    node_b: &mut leviculum_std::driver::ReticulumNode,
    ev_task_a: &tokio::task::JoinHandle<()>,
    ev_task_b: &tokio::task::JoinHandle<()>,
) {
    ev_task_a.abort();
    ev_task_b.abort();
    let _ = node_a.stop().await;
    let _ = node_b.stop().await;
}

// Unit Tests
#[cfg(test)]
mod tests {
    use super::*;

    use leviculum_core::transport::LinkProfile;

    /// The link Codeberg #190 was measured on: a T-Beam pair at 2734 bps
    /// with a 2926 ms pre-TX jitter ceiling, carrying 147-byte frames.
    const MEASURED_LINK: LinkProfile = LinkProfile {
        bitrate_bps: 2734,
        tx_jitter_max_ms: Some(2926),
        acquisition: None,
    };
    const MEASURED_FRAME_BYTES: usize = 147;

    /// The link the rig ran on the night of 2026-09-23 (run 179), the first
    /// full corpus with the interface's post-TX hold (d6a158be): 2380 bps,
    /// 147-byte frames, and a band-1 jitter ceiling of 360 ms — the
    /// difference between the two `LORA_TX_HOLD` figures that run reported
    /// for the same frame, 863 ms in band 1 and 1223 ms in band 2.
    const HELD_LINK: LinkProfile = LinkProfile {
        bitrate_bps: 2380,
        tx_jitter_max_ms: Some(360),
        acquisition: None,
    };
    /// What that run's `LORA_TX_HOLD` reported per frame once the burst had
    /// pushed the modem out of band 1.
    const HELD_LINK_BAND_2_HOLD_MS: u64 = 1_223;

    /// The link `lora_ratchet_rotation_listened` ran on for trace 223
    /// (2026-09-24), as the daemon reports it under the arm that lost
    /// nothing on the air: 2734 bps, a 360 ms per-frame jitter ceiling, and
    /// an acquisition of 15 contention slots each floored at the airtime of
    /// the frame about to go out (#347 arm 3).
    const ROTATION_ARM_3: LinkProfile = LinkProfile {
        bitrate_bps: 2734,
        tx_jitter_max_ms: Some(360),
        acquisition: Some(AcquisitionCeiling {
            max_ms: 360,
            frame_slots: Some(15),
            full_frame_ms: 360,
        }),
    };
    /// The same link under the arm the daemon runs by default, where the
    /// acquisition is the slot-priced 360 ms whatever the frame.
    const ROTATION_ARM_1: LinkProfile = LinkProfile {
        bitrate_bps: 2734,
        tx_jitter_max_ms: Some(360),
        acquisition: Some(AcquisitionCeiling {
            max_ms: 360,
            frame_slots: None,
            full_frame_ms: 360,
        }),
    };

    /// The drain window pays one acquisition per sender, priced for the
    /// frame it is waiting on.
    ///
    /// Trace 223 measured arm 3 delivering 40 of 40 data frames with zero
    /// mutual key-ups and still scored two runs red: one frame each arriving
    /// 0.46 s and 0.68 s after a window that priced the handover at the
    /// 360 ms per-frame ceiling. Under that arm a single acquisition cost
    /// 5.03 s and 5.53 s on the same link. Both sides of this tool send, and
    /// each takes the channel once for the burst it enqueues, so the budget
    /// owes two of them.
    #[test]
    fn the_budget_pays_one_acquisition_per_sender() {
        const FRAMES: u64 = 18;
        let fallback = std::time::Duration::from_secs(10);
        let air_ms = frame_pacing(MEASURED_FRAME_BYTES, ROTATION_ARM_3).air_ms;

        let arm_3 = drain_budget(FRAMES, MEASURED_FRAME_BYTES, Some(ROTATION_ARM_3), fallback);
        let arm_1 = drain_budget(FRAMES, MEASURED_FRAME_BYTES, Some(ROTATION_ARM_1), fallback);

        let per_sender_3 = ROTATION_ARM_3
            .acquisition_ceiling_ms(air_ms)
            .expect("the arm-3 profile carries an acquisition");
        let per_sender_1 = ROTATION_ARM_1
            .acquisition_ceiling_ms(air_ms)
            .expect("the arm-1 profile carries an acquisition");
        assert_eq!(
            (per_sender_3, per_sender_1),
            (15 * air_ms, 360),
            "arm 3 owes 15 frames of {air_ms}ms where arm 1 owes 15 slots"
        );

        assert_eq!(
            arm_3.total.as_millis() as u64 - arm_1.total.as_millis() as u64,
            INTERLEAVED_SENDERS * (per_sender_3 - per_sender_1),
            "the two windows differ by exactly one acquisition per sender \
             and nothing else:\n  arm 3: {}\n  arm 1: {}",
            arm_3.detail,
            arm_1.detail
        );

        // The same figure, against the window that has no acquisition term
        // at all — what the tool priced before 223.
        let unpriced = drain_budget(
            FRAMES,
            MEASURED_FRAME_BYTES,
            Some(LinkProfile {
                acquisition: None,
                ..ROTATION_ARM_3
            }),
            fallback,
        );
        assert_eq!(
            arm_3.total.as_millis() as u64 - unpriced.total.as_millis() as u64,
            INTERLEAVED_SENDERS * per_sender_3,
        );
        assert!(
            arm_3.total.as_secs_f64() - unpriced.total.as_secs_f64() >= 0.68,
            "the window has to grow by more than the 0.68s that fell outside \
             it on the rig, or the same run is red for the same reason"
        );

        // The term is printed, not just charged: a run document has to show
        // what was priced.
        assert!(
            arm_3.detail.contains("acquisition"),
            "the budget line names no acquisition: {}",
            arm_3.detail
        );
        assert!(
            arm_3
                .detail
                .contains(&format!("{:.2}s/sender", per_sender_3 as f64 / 1000.0)),
            "the budget line does not print the acquisition it charged: {}",
            arm_3.detail
        );
    }

    /// The basic ratchet cell of run 179, as it was measured: every one of
    /// its 18 outstanding frames was handed over, held, keyed and decoded by
    /// the far modem, and the cell was still red — the burst completed in
    /// 11.8 s against a budget of 11.0 s, which priced the frames' airtime
    /// and a single handover but not the hold that paces every frame behind
    /// the one on the air.
    ///
    /// The budget has to cover what the hold costs, or the tool reports its
    /// own arithmetic as packet loss.
    #[test]
    fn the_budget_covers_a_burst_the_interface_holds() {
        const MEASURED_COMPLETION: f64 = 11.8;
        let budget = drain_budget(
            18,
            MEASURED_FRAME_BYTES,
            Some(HELD_LINK),
            std::time::Duration::from_secs(10),
        );
        assert!(
            budget.total.as_secs_f64() >= MEASURED_COMPLETION,
            "budget {:.1}s is under the {MEASURED_COMPLETION}s the same burst \
             took on the rig (run 179, 2026-09-23): {}",
            budget.total.as_secs_f64(),
            budget.detail
        );
        assert!(
            budget.total.as_secs_f64() <= MEASURED_COMPLETION * 1.5,
            "budget {:.1}s pads the measured {MEASURED_COMPLETION}s by more \
             than half: {}",
            budget.total.as_secs_f64(),
            budget.detail
        );
    }

    /// The hold the tool prices has to be the hold the interface applies.
    ///
    /// Run 179 reported `LORA_TX_HOLD held_ms=863` in band 1 and
    /// `held_ms=1223` in band 2 for the same 147-byte frame on this link,
    /// whose reported band-1 ceiling is 360 ms. A budget priced at the band-1
    /// figure is 30 % short of the pace the burst actually ran at, which is
    /// the whole of the arithmetic the three ratchet cells died on.
    #[test]
    fn the_priced_hold_covers_the_band_a_burst_runs_in() {
        let pacing = frame_pacing(MEASURED_FRAME_BYTES, HELD_LINK);
        assert!(
            pacing.hold_ms >= HELD_LINK_BAND_2_HOLD_MS,
            "priced hold {}ms is under the {}ms the interface reported for \
             the same frame in band 2",
            pacing.hold_ms,
            HELD_LINK_BAND_2_HOLD_MS
        );
        // Band 3 is the next one up; pricing that far ahead would pad every
        // budget by another 12 slots per frame.
        assert!(
            pacing.hold_ms < HELD_LINK_BAND_2_HOLD_MS * 3 / 2,
            "priced hold {}ms reaches past the band the measurement found",
            pacing.hold_ms
        );
    }

    /// The grace window past expiry is the same quantity as the tail
    /// deferral: one hold plus one airtime, the time a frame already handed
    /// to the modem needs to reach the far side. A link with nothing on the
    /// air gets none, because there is nothing to wait out.
    #[test]
    fn expiry_is_followed_by_a_priced_grace_window() {
        let budget = drain_budget(
            18,
            MEASURED_FRAME_BYTES,
            Some(HELD_LINK),
            std::time::Duration::from_secs(10),
        );
        let pacing = frame_pacing(MEASURED_FRAME_BYTES, HELD_LINK);
        assert_eq!(
            budget.grace,
            std::time::Duration::from_millis(pacing.hold_ms + pacing.air_ms)
        );
        assert_eq!(
            drain_budget(18, 147, None, std::time::Duration::from_secs(10)).grace,
            std::time::Duration::ZERO,
            "no on-air pricing, nothing priced to wait out"
        );
    }

    /// A frame that lands after the budget expired is late, not delivered.
    ///
    /// The verdict's bar is what the link managed inside the window its own
    /// figures priced. Counting the grace window's arrivals into it would let
    /// a cell pass on frames that arrived after the tool stopped waiting —
    /// which is the same blindness as the teardown that dropped them, one
    /// sign flipped.
    #[test]
    fn late_arrivals_stay_out_of_the_delivery_bar() {
        let mut stats = SelftestStats::new();
        stats.sp_sent_a = 10;
        stats.sp_sent_b = 10;
        stats.sp_recv_a = 10;
        stats.sp_recv_b = 10;
        assert_eq!(stats.sp_in_budget_recv(), 20);
        assert_eq!(compute_sp_verdict(&stats, &[]), Verdict::Pass);

        stats.sp_late = 12;
        assert_eq!(
            stats.sp_in_budget_recv(),
            8,
            "the bar is the receipts the budget covered"
        );
        assert_eq!(
            compute_sp_verdict(&stats, &[]),
            Verdict::Fail,
            "8 of 20 inside the budget is 40 %, whatever arrived after it"
        );
    }

    /// The sizing line has to carry the formula, not just the number: a
    /// budget whose derivation is not in the log cannot be checked against
    /// the run it sized.
    #[test]
    fn the_sizing_line_states_what_it_priced() {
        let budget = drain_budget(
            18,
            MEASURED_FRAME_BYTES,
            Some(HELD_LINK),
            std::time::Duration::from_secs(10),
        );
        for term in [
            "air ",
            "hold ",
            "jitter ceiling",
            "tail deferral",
            "2380 bps",
        ] {
            assert!(
                budget.detail.contains(term),
                "sizing line is missing `{term}`: {}",
                budget.detail
            );
        }
    }

    /// Regression for the ratchet-rotation window: the fixed 5 s sleep gave
    /// the pre-rotation burst a 6.0 s window where 7.5-7.7 s was needed, so
    /// exactly the five frames of whichever radio transmitted second were
    /// counted as lost, in run after run.
    ///
    /// Re-based on run 179 (2026-09-23), the first full corpus with the
    /// interface's post-TX hold: on that link the same 10-frame exchange
    /// completed in 6.16 s against a 5.7 s budget, and the cell went red with
    /// every frame accounted for on the air. The pre-hold figures this test
    /// used to carry (7.5-7.7 s on the Codeberg #190 link) are not comparable
    /// — nothing held a frame back on that link, so a burst crossed it at the
    /// modem's own pace.
    #[test]
    fn rotation_window_clears_the_measured_requirement_without_padding_it() {
        const MEASURED_COMPLETION: f64 = 6.16;
        for outstanding in [8, 9, 10] {
            let budget = drain_budget(
                outstanding,
                MEASURED_FRAME_BYTES,
                Some(HELD_LINK),
                std::time::Duration::from_secs(5),
            );
            let window = budget.total.as_secs_f64();
            assert!(
                window >= MEASURED_COMPLETION,
                "window {window:.2}s ({outstanding} outstanding) is under the \
                 {MEASURED_COMPLETION}s this exchange took on the rig: {}",
                budget.detail
            );
            assert!(
                window <= MEASURED_COMPLETION * 1.6,
                "window {window:.2}s ({outstanding} outstanding) pads the \
                 {MEASURED_COMPLETION}s requirement by more than the stated \
                 margin: {}",
                budget.detail
            );
        }
    }

    /// The point of deriving it: a window sized for a fast radio must not be
    /// what a slow radio gets. At SF12 both terms grow, and the budget grows
    /// with them.
    #[test]
    fn budget_scales_with_the_radio_settings() {
        let fast = drain_budget(
            10,
            MEASURED_FRAME_BYTES,
            Some(LinkProfile {
                bitrate_bps: 5468,
                tx_jitter_max_ms: Some(1463),
                acquisition: None,
            }),
            std::time::Duration::from_secs(5),
        );
        let slow = drain_budget(
            10,
            MEASURED_FRAME_BYTES,
            Some(LinkProfile {
                bitrate_bps: 366,
                tx_jitter_max_ms: Some(21_857),
                acquisition: None,
            }),
            std::time::Duration::from_secs(5),
        );
        assert!(
            slow.total > fast.total * 4,
            "SF12 budget {:?} must dwarf the SF7 budget {:?}",
            slow.total,
            fast.total
        );
    }

    /// A medium with no airtime to account for keeps the legacy fixed wait:
    /// none of the arithmetic above describes TCP.
    #[test]
    fn budget_falls_back_when_the_next_hop_reports_no_bitrate() {
        let fallback = std::time::Duration::from_secs(10);
        assert_eq!(
            drain_budget(20, 147, None, fallback).total,
            fallback,
            "no profile must keep the fixed wait"
        );
        assert_eq!(
            drain_budget(
                20,
                147,
                Some(LinkProfile {
                    bitrate_bps: 0,
                    tx_jitter_max_ms: None,
                    acquisition: None
                }),
                fallback
            )
            .total,
            fallback,
            "a zero bitrate must not divide the budget to nothing"
        );
    }

    /// An interface that does not jitter its transmissions contributes no
    /// contention term rather than a made-up one: its hold is the frame's own
    /// airtime and nothing else, which leaves the frames' airtime pacing the
    /// medium (10 x 516 ms) and one tail deferral of hold + air behind it.
    #[test]
    fn budget_without_a_jitter_ceiling_is_airtime_only() {
        let budget = drain_budget(
            10,
            MEASURED_FRAME_BYTES,
            Some(LinkProfile {
                bitrate_bps: 2734,
                tx_jitter_max_ms: None,
                acquisition: None,
            }),
            std::time::Duration::from_secs(5),
        );
        assert_eq!(
            budget.total,
            std::time::Duration::from_millis(5_160 + 2 * 516),
            "{}",
            budget.detail
        );
    }

    /// The frame the budget prices must be the frame that crosses the air.
    ///
    /// Measured on the T-Beam pair of Codeberg #190, 2026-08-05: the tool's
    /// own node packs 131 bytes and the daemon's `LORA_TX` carries 147 for the
    /// same frame over a 3-hop path — the forwarder's own address field. The
    /// first re-measurement priced the 131 and expired 0.75 s before the
    /// slower direction's last two frames landed, with both radios' logs
    /// showing 10 sent and 10 received. Same shape as the fixed sleep it
    /// replaced, one layer down.
    #[test]
    fn a_relayed_frame_is_priced_as_it_crosses_the_air() {
        assert_eq!(
            on_air_bytes(131, Some(3)),
            147,
            "the measured on-air size of the measured packed size"
        );
        assert_eq!(
            on_air_bytes(131, Some(1)),
            131,
            "a directly attached destination is not rewritten"
        );
        assert_eq!(on_air_bytes(131, None), 131, "no path, nothing to add");

        // And the difference is the one that mattered: the 18-frame burst's
        // budget has to grow with it. Half a second rather than the full one
        // it was before the hold was priced — the medium term now moves at
        // half a frame's airtime, because the hold that paces it is shared
        // between the two senders.
        let priced_short = drain_budget(
            18,
            131,
            Some(MEASURED_LINK),
            std::time::Duration::from_secs(10),
        );
        let priced_right = drain_budget(
            18,
            on_air_bytes(131, Some(3)),
            Some(MEASURED_LINK),
            std::time::Duration::from_secs(10),
        );
        let gained = priced_right.total - priced_short.total;
        assert!(
            gained >= std::time::Duration::from_millis(600),
            "pricing the on-air frame must recover the window the truncated \
             measurement lost, gained {gained:?}"
        );
    }

    // Reading the daemon's answer (Codeberg #190)
    //
    // The payload is the shared-instance dict `rnsd` serves too, so the read
    // side is exercised against all three shapes it can arrive in: ours with
    // the key, a daemon without it, and one with no radio at all.

    /// One `interface_stats` interface row, as JSON.
    fn iface_row(name: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut row = serde_json::json!({
            "name": name,
            "type": "TCPClientInterface",
            "bitrate": 10_000_000u64,
            "status": true,
        });
        for (k, v) in extra.as_object().expect("object").iter() {
            row[k] = v.clone();
        }
        row
    }

    fn stats_with(rows: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({ "interfaces": rows })
    }

    /// The engaged state: a daemon of ours, reporting the radio's own on-air
    /// bitrate and the jitter ceiling it draws against.
    #[test]
    fn a_radio_row_with_the_jitter_key_yields_the_full_profile() {
        let stats = stats_with(vec![
            iface_row(
                "TCPServerInterface[lo/127.0.0.1:4242]",
                serde_json::json!({}),
            ),
            iface_row(
                "RNodeInterface[radio]",
                serde_json::json!({
                    "type": "RNodeInterface",
                    "bitrate": 2734u64,
                    "airtime_short": 0.0,
                    "tx_jitter_max": 2.926,
                }),
            ),
        ]);
        let (profile, origin) =
            link_profile_from_interface_stats(&stats).expect("a radio row is present");
        assert_eq!(profile.bitrate_bps, 2734);
        assert_eq!(profile.tx_jitter_max_ms, Some(2926));
        assert!(origin.contains("RNodeInterface[radio]"), "{origin}");
    }

    /// The tolerance Codeberg #183 was about, on this payload: a daemon that
    /// does not carry the key — `rnsd`, or an `lnsd` older than this change —
    /// must still yield the airtime term, not an error and not a made-up
    /// handover.
    #[test]
    fn a_radio_row_without_the_jitter_key_still_yields_the_airtime_term() {
        let stats = stats_with(vec![iface_row(
            "RNodeInterface[radio]",
            serde_json::json!({
                "type": "RNodeInterface",
                "bitrate": 2734u64,
                "airtime_short": 0.0,
            }),
        )]);
        let (profile, origin) =
            link_profile_from_interface_stats(&stats).expect("a radio row is present");
        assert_eq!(profile.bitrate_bps, 2734);
        assert_eq!(profile.tx_jitter_max_ms, None);
        assert!(
            origin.contains("no pre-TX jitter"),
            "the log must say the handover went unaccounted: {origin}"
        );
        // And the budget that comes out of it is the airtime alone — derived,
        // not the fixed fallback: the frames' own airtime pacing the medium
        // plus one tail deferral, with no contention term anywhere in it.
        let budget = drain_budget(
            10,
            MEASURED_FRAME_BYTES,
            Some(profile),
            std::time::Duration::from_secs(5),
        );
        assert_eq!(
            budget.total,
            std::time::Duration::from_millis(5_160 + 2 * 516),
            "{}",
            budget.detail
        );
    }

    /// No radio on the far side of the RPC at all: a reason, and the caller
    /// keeps its fixed wait. Not a panic, and not a budget computed off a TCP
    /// interface's 10 Mbps guess, which would size the window to nothing.
    #[test]
    fn a_daemon_with_no_radio_is_a_reason_not_a_bogus_profile() {
        let stats = stats_with(vec![
            iface_row(
                "TCPServerInterface[lo/127.0.0.1:4242]",
                serde_json::json!({}),
            ),
            iface_row(
                "Shared Instance[rns/default]",
                serde_json::json!({"type": "LocalServerInterface", "bitrate": 1_000_000_000u64}),
            ),
        ]);
        let err = link_profile_from_interface_stats(&stats).expect_err("no radio row");
        assert!(err.contains("no radio interface"), "{err}");
    }

    /// Malformed payloads are reasons too. A tool that panics on a daemon's
    /// answer cannot report on the daemon.
    #[test]
    fn a_payload_without_the_interfaces_array_is_a_reason() {
        for shape in [
            serde_json::json!({}),
            serde_json::json!({"interfaces": 7}),
            serde_json::json!("not a dict"),
        ] {
            let err = link_profile_from_interface_stats(&shape).expect_err("{shape}");
            assert!(err.contains("`interfaces`"), "{err}");
        }
    }

    /// A zero or absent bitrate on a radio row is not a link profile: dividing
    /// by it is the nonsense the fallback exists to avoid.
    #[test]
    fn a_radio_row_with_no_usable_bitrate_is_skipped() {
        let stats = stats_with(vec![iface_row(
            "RNodeInterface[radio]",
            serde_json::json!({
                "type": "RNodeInterface",
                "bitrate": 0u64,
                "airtime_short": 0.0,
                "tx_jitter_max": 2.926,
            }),
        )]);
        assert!(link_profile_from_interface_stats(&stats).is_err());
    }

    /// The shape `rnsd` actually serves. Python computes the RNode's on-air
    /// rate in float arithmetic (`RNodeInterface.updateBitrate`,
    /// RNodeInterface.py:694 — 2734.375 bps on SF7/CR5 at 62.5 kHz, 3125.0 on
    /// the SF8/CR5/125 kHz default) and serialises it unrounded
    /// (Reticulum.py:1423), while `lnsd` serves the same key as an integer
    /// (`rpc/handlers.rs`, `pickle_int`). Reading only the integer shape left
    /// every `rnsd` row bitrate-less, so the same phase ran a derived budget
    /// against our daemon and the fixed fallback against Python's: a budget
    /// that differs by daemon is a config difference smuggled into a stack
    /// comparison the A/B contract says must differ only in the stack.
    #[test]
    fn a_float_bitrate_sizes_the_window_the_integer_of_that_value_does() {
        for (float_bitrate, integer_bitrate) in [
            (serde_json::json!(2734.375), serde_json::json!(2734u64)),
            (serde_json::json!(3125.0), serde_json::json!(3125u64)),
        ] {
            let row = |bitrate: serde_json::Value| {
                stats_with(vec![iface_row(
                    "RNodeInterface[radio]",
                    serde_json::json!({
                        "type": "RNodeInterface",
                        "bitrate": bitrate,
                        "airtime_short": 0.0,
                        "tx_jitter_max": 2.926,
                    }),
                )])
            };
            let (from_float, origin) = link_profile_from_interface_stats(&row(
                float_bitrate.clone()
            ))
            .unwrap_or_else(|e| panic!("rnsd serves {float_bitrate} and it must read: {e}"));
            let (from_integer, _) =
                link_profile_from_interface_stats(&row(integer_bitrate.clone()))
                    .expect("the integer shape reads");
            assert_eq!(
                from_float, from_integer,
                "{float_bitrate} and {integer_bitrate} describe the same air"
            );
            let budget = |profile| {
                drain_budget(
                    24,
                    MEASURED_FRAME_BYTES,
                    Some(profile),
                    std::time::Duration::from_secs(5),
                )
                .total
            };
            assert_eq!(
                budget(from_float),
                budget(from_integer),
                "the drain window must not depend on which daemon served the rate"
            );
            // And the line says which shape it read, so a log can be checked
            // afterwards against the daemon that produced it.
            assert!(
                origin.contains(&float_bitrate.to_string()),
                "the sizing line must name the float it came from: {origin}"
            );
        }
    }

    /// A fractional rate is floored, never rounded up: an over-stated bitrate
    /// prices the air short, and cutting a train off is the failure this
    /// derivation exists to remove. Waiting a few milliseconds too long is not.
    #[test]
    fn a_fractional_bitrate_is_floored_not_rounded_up() {
        let stats = stats_with(vec![iface_row(
            "RNodeInterface[radio]",
            serde_json::json!({
                "type": "RNodeInterface",
                "bitrate": 5468.75,
                "airtime_short": 0.0,
            }),
        )]);
        let (profile, _) =
            link_profile_from_interface_stats(&stats).expect("a radio row is present");
        assert_eq!(profile.bitrate_bps, 5468);
    }

    /// Float shapes that are not a rate are skipped like their integer twins:
    /// zero and negative divide into nonsense, and the fallback exists for it.
    #[test]
    fn a_radio_row_with_an_unusable_float_bitrate_is_skipped() {
        for bitrate in [
            serde_json::json!(0.0),
            serde_json::json!(-3125.0),
            serde_json::json!(0.4),
            serde_json::json!(serde_json::Value::Null),
            serde_json::json!("3125"),
        ] {
            let stats = stats_with(vec![iface_row(
                "RNodeInterface[radio]",
                serde_json::json!({
                    "type": "RNodeInterface",
                    "bitrate": bitrate,
                    "airtime_short": 0.0,
                }),
            )]);
            assert!(
                link_profile_from_interface_stats(&stats).is_err(),
                "{bitrate} is not an on-air rate"
            );
        }
    }

    /// Two radios: the burst is bounded by the slowest air it has to cross, so
    /// that is the one the window is sized from.
    #[test]
    fn the_most_constraining_radio_is_the_one_chosen() {
        let stats = stats_with(vec![
            iface_row(
                "RNodeInterface[fast]",
                serde_json::json!({
                    "type": "RNodeInterface",
                    "bitrate": 5468u64,
                    "airtime_short": 0.0,
                    "tx_jitter_max": 1.463,
                }),
            ),
            iface_row(
                "RNodeInterface[slow]",
                serde_json::json!({
                    "type": "RNodeInterface",
                    "bitrate": 366u64,
                    "airtime_short": 0.0,
                    "tx_jitter_max": 21.857,
                }),
            ),
        ]);
        let (profile, origin) = link_profile_from_interface_stats(&stats).expect("radio rows");
        assert_eq!(profile.bitrate_bps, 366);
        assert!(origin.contains("RNodeInterface[slow]"), "{origin}");
    }

    /// A node sitting directly on the radio keeps its own answer; everything
    /// else takes the daemon's.
    #[test]
    fn the_local_next_hop_wins_when_it_has_airtime_of_its_own() {
        let from_daemon = LinkSizing {
            profile: Some(MEASURED_LINK),
            origin: "the daemon".to_string(),
        };
        let local = LinkProfile {
            bitrate_bps: 366,
            tx_jitter_max_ms: Some(21_857),
            acquisition: None,
        };
        assert_eq!(
            link_sizing(Some(local), &from_daemon).profile,
            Some(local),
            "a next hop with airtime of its own describes the link best"
        );
        assert_eq!(
            link_sizing(None, &from_daemon).profile,
            Some(MEASURED_LINK),
            "a TCP next hop must defer to the daemon that owns the radio"
        );
        assert_eq!(
            link_sizing(
                Some(LinkProfile {
                    bitrate_bps: 0,
                    tx_jitter_max_ms: None,
                    acquisition: None
                }),
                &from_daemon
            )
            .profile,
            Some(MEASURED_LINK),
            "a next hop reporting no bitrate is not an answer"
        );
    }

    /// Both engagement states of the tool's own entry point, without a daemon:
    /// no config dir, and a config dir no daemon owns. Each is a stated reason
    /// and a fixed wait.
    #[tokio::test]
    async fn the_sizing_query_degrades_to_a_stated_reason() {
        let none = daemon_link_sizing(None).await;
        assert!(none.profile.is_none());
        assert!(none.origin.contains("-c/--config"), "{}", none.origin);

        let tmp = tempfile::tempdir().expect("tempdir");
        let absent = daemon_link_sizing(Some(tmp.path())).await;
        assert!(absent.profile.is_none());
        assert!(
            absent.origin.contains("cannot reach the daemon"),
            "{}",
            absent.origin
        );
    }

    #[test]
    fn test_build_parse_roundtrip() {
        let msg = build_message(Phase::SinglePacket, "ab", 42, 1000);
        let parsed = parse_message(&msg).expect("should parse");
        assert_eq!(parsed.phase, Phase::SinglePacket);
        assert_eq!(parsed.dir, "ab");
        assert_eq!(parsed.seq, 42);
        assert_eq!(parsed.timestamp_ms, 1000);
    }

    #[test]
    fn test_parse_bad_checksum() {
        let mut msg = build_message(Phase::Link, "ab", 1, 1000);
        // Corrupt the last byte
        let len = msg.len();
        msg[len - 1] = b'X';
        assert!(parse_message(&msg).is_none());
    }

    #[test]
    fn test_parse_bad_format() {
        assert!(parse_message(b"not:a:valid").is_none());
        assert!(parse_message(b"").is_none());
        assert!(parse_message(b"l:ab:notnum:1000:deadbeef").is_none());
        // A message with no phase tag is not a message this tool wrote.
        assert!(parse_message(b"ab:0:1000:deadbeef").is_none());
    }

    #[test]
    fn test_link_verdict_pass() {
        let mut stats = SelftestStats::new();
        stats.sent_a = 100;
        stats.sent_b = 100;
        stats.recv_a = 100;
        stats.recv_b = 100;
        let warnings = vec![];
        assert_eq!(compute_link_verdict(&stats, &warnings), Verdict::Pass);
    }

    #[test]
    fn test_link_verdict_pass_boundary() {
        // recv = 99% exactly → PASS
        let mut stats = SelftestStats::new();
        stats.sent_a = 100;
        stats.sent_b = 100;
        stats.recv_a = 99;
        stats.recv_b = 99;
        let warnings = vec![];
        assert_eq!(compute_link_verdict(&stats, &warnings), Verdict::Pass);
    }

    #[test]
    fn test_link_verdict_warn() {
        let mut stats = SelftestStats::new();
        stats.sent_a = 100;
        stats.sent_b = 100;
        stats.recv_a = 95;
        stats.recv_b = 95;
        let warnings = vec!["something".to_string()];
        assert_eq!(compute_link_verdict(&stats, &warnings), Verdict::Warn);
    }

    #[test]
    fn test_link_verdict_warn_low_recv() {
        // recv = 91% (between 90-99%), no warnings → WARN
        let mut stats = SelftestStats::new();
        stats.sent_a = 100;
        stats.sent_b = 100;
        stats.recv_a = 91;
        stats.recv_b = 91;
        let warnings = vec![];
        assert_eq!(compute_link_verdict(&stats, &warnings), Verdict::Warn);
    }

    #[test]
    fn test_link_verdict_warn_boundary() {
        // recv = 90% exactly → WARN (not FAIL, threshold is <90%)
        let mut stats = SelftestStats::new();
        stats.sent_a = 100;
        stats.sent_b = 100;
        stats.recv_a = 90;
        stats.recv_b = 90;
        let warnings = vec![];
        assert_eq!(compute_link_verdict(&stats, &warnings), Verdict::Warn);
    }

    #[test]
    fn test_link_verdict_fail_low_recv() {
        let mut stats = SelftestStats::new();
        stats.sent_a = 100;
        stats.sent_b = 100;
        stats.recv_a = 50;
        stats.recv_b = 50;
        let warnings = vec![];
        assert_eq!(compute_link_verdict(&stats, &warnings), Verdict::Fail);
    }

    #[test]
    fn test_link_verdict_fail_zero_recv() {
        let mut stats = SelftestStats::new();
        stats.sent_a = 100;
        stats.sent_b = 100;
        let warnings = vec![];
        assert_eq!(compute_link_verdict(&stats, &warnings), Verdict::Fail);
    }

    #[test]
    fn test_sp_verdict_pass() {
        let mut stats = SelftestStats::new();
        stats.sp_sent_a = 100;
        stats.sp_sent_b = 100;
        stats.sp_recv_a = 90;
        stats.sp_recv_b = 90;
        let warnings = vec![];
        assert_eq!(compute_sp_verdict(&stats, &warnings), Verdict::Pass);
    }

    #[test]
    fn test_sp_verdict_fail_low_recv() {
        let mut stats = SelftestStats::new();
        stats.sp_sent_a = 100;
        stats.sp_sent_b = 100;
        stats.sp_recv_a = 20;
        stats.sp_recv_b = 20;
        let warnings = vec![];
        assert_eq!(compute_sp_verdict(&stats, &warnings), Verdict::Fail);
    }

    #[test]
    fn test_sp_verdict_fail_zero_recv() {
        let mut stats = SelftestStats::new();
        stats.sp_sent_a = 100;
        stats.sp_sent_b = 100;
        let warnings = vec![];
        assert_eq!(compute_sp_verdict(&stats, &warnings), Verdict::Fail);
    }

    #[test]
    fn test_record_received_link_phase() {
        let mut stats = SelftestStats::new();
        let msg = build_message(Phase::Link, "ba", 0, 100);
        record_received_message(&mut stats, &msg, 200, true, false);
        assert_eq!(stats.recv_a, 1);
        assert_eq!(stats.sp_recv_a, 0);
    }

    #[test]
    fn test_record_received_sp_phase() {
        let mut stats = SelftestStats::new();
        let msg = build_message(Phase::SinglePacket, "ba", 0, 100);
        record_received_message(&mut stats, &msg, 200, true, true);
        assert_eq!(stats.recv_a, 0);
        assert_eq!(stats.sp_recv_a, 1);
    }

    /// Codeberg #337: a link message is still in flight when the
    /// single-packet phase opens. It used to be counted as a single-packet
    /// receipt because the tally read the tool's clock instead of the
    /// message, which inflates the single-packet delivery rate — the number
    /// every #221-family PDR reading is taken from — and can put it above
    /// 100%. The message says which exchange it belongs to; that is what
    /// counts it.
    #[test]
    fn a_late_link_message_stays_in_the_link_tally() {
        let mut stats = SelftestStats::new();
        stats.sent_a = 1;
        stats.sp_sent_a = 1;

        let late = build_message(Phase::Link, "ba", 0, 100);
        record_received_message(&mut stats, &late, 5_000, true, true);

        assert_eq!(stats.recv_a, 1, "the link sent it, the link counts it");
        assert_eq!(
            stats.sp_recv_a, 0,
            "no single packet was received, so none may be reported"
        );

        // And the converse still holds: a single packet that arrives after
        // the phase flag was reset is not credited to the link.
        let mut stats = SelftestStats::new();
        let single = build_message(Phase::SinglePacket, "ba", 0, 100);
        record_received_message(&mut stats, &single, 5_000, true, false);
        assert_eq!(stats.sp_recv_a, 1);
        assert_eq!(stats.recv_a, 0);
    }

    #[test]
    fn test_record_received_wrong_dir() {
        let mut stats = SelftestStats::new();
        let msg = build_message(Phase::Link, "ab", 0, 100);
        // Node A expects "ba", so "ab" should be ignored
        record_received_message(&mut stats, &msg, 200, true, false);
        assert_eq!(stats.recv_a, 0);
        assert_eq!(stats.corrupt, 0);
    }

    #[test]
    fn test_record_received_corrupt() {
        let mut stats = SelftestStats::new();
        record_received_message(&mut stats, b"garbage", 200, true, false);
        assert_eq!(stats.corrupt, 1);
    }

    #[test]
    fn test_verdict_ordering() {
        assert!(Verdict::Pass < Verdict::Warn);
        assert!(Verdict::Warn < Verdict::Fail);
        assert_eq!(Verdict::Pass.max(Verdict::Fail), Verdict::Fail);
        assert_eq!(Verdict::Warn.max(Verdict::Pass), Verdict::Warn);
    }

    // Announce cap slot

    /// The slot is the reference's own arithmetic: one announce's air time
    /// divided by the 2 % share an interface gives announces.
    ///
    /// At 2734 bps a 209-byte announce is 611 ms of payload, so a neighbour
    /// running the reference holds its interface closed to announces for
    /// 30.5 s after airing one. Re-announcing sooner than that lands in the
    /// same closed window the first announce did.
    #[test]
    fn the_slot_is_one_announce_divided_by_the_reference_cap() {
        let sizing = LinkSizing {
            profile: Some(MEASURED_LINK),
            origin: "the daemon's `RNodeInterface`".to_string(),
        };
        let cap = announce_cap_slot(&sizing);
        let expected_ms = ANNOUNCE_WIRE_BYTES * 8 * 1000 / 2734 * 50;
        assert_eq!(cap.slot, std::time::Duration::from_millis(expected_ms));
        assert!(
            cap.slot.as_secs_f64() > 30.0 && cap.slot.as_secs_f64() < 31.0,
            "one cap slot on the #190 link is half a minute, got {:?}",
            cap.slot
        );
        for term in ["2734 bps", "2%", "the daemon's `RNodeInterface`"] {
            assert!(
                cap.detail.contains(term),
                "the slot line must carry its derivation, missing {term:?}: {}",
                cap.detail
            );
        }
    }

    /// No bitrate, no derivation: the fixed fallback, and the line says which
    /// state the run is in rather than printing a bare number.
    #[test]
    fn an_unpriced_link_falls_back_to_a_fixed_slot() {
        let sizing = LinkSizing::unavailable("no -c/--config given");
        let cap = announce_cap_slot(&sizing);
        assert_eq!(cap.slot, ANNOUNCE_CAP_SLOT_FALLBACK);
        assert!(
            cap.detail.contains("no -c/--config given"),
            "the fallback has to name why nothing was derived: {}",
            cap.detail
        );
    }

    /// A zero bitrate is not a link either — dividing by it would panic, and
    /// the row it came from describes nothing.
    #[test]
    fn a_zero_bitrate_does_not_price_a_slot() {
        let sizing = LinkSizing {
            profile: Some(LinkProfile {
                bitrate_bps: 0,
                tx_jitter_max_ms: None,
                acquisition: None,
            }),
            origin: "a row with no usable bitrate".to_string(),
        };
        assert_eq!(announce_cap_slot(&sizing).slot, ANNOUNCE_CAP_SLOT_FALLBACK);
    }

    /// On a fast link the derived slot is milliseconds, and re-announcing at
    /// that rate is an announce storm. The floor is what stops it, and the
    /// line says the number was floored rather than derived.
    #[test]
    fn a_fast_link_is_floored_rather_than_re_announced_every_few_ms() {
        let sizing = LinkSizing {
            profile: Some(LinkProfile {
                bitrate_bps: 10_000_000,
                tx_jitter_max_ms: None,
                acquisition: None,
            }),
            origin: "a 10 Mbit/s row".to_string(),
        };
        let cap = announce_cap_slot(&sizing);
        assert_eq!(cap.slot, ANNOUNCE_CAP_SLOT_FLOOR);
        assert!(
            cap.detail.contains("floored"),
            "a floored slot must say so: {}",
            cap.detail
        );
    }

    /// The size the slot is priced against is the announce this tool sends,
    /// not a round number: header, identity, hashes, ratchet, signature and
    /// the app data Phase 2 actually carries.
    #[test]
    fn the_slot_is_priced_against_the_announce_this_tool_sends() {
        assert_eq!(APP_DATA_A.len(), APP_DATA_B.len());
        assert_eq!(ANNOUNCE_WIRE_BYTES, 19 + 64 + 10 + 10 + 32 + 64 + 10);
    }
}
