//! Per-peer outbound circuit breaker (leviculum#66).
//!
//! # Why a breaker and not a deeper queue
//!
//! The retry queue is the right shape for a *burst*: it trades memory for the
//! chance to catch up. It is the wrong shape for a peer that is simply not
//! draining, because there is no catching up — the queue reaches its cap and
//! discards continuously, so the depth buys nothing and the discard is silent
//! to whatever is producing.
//!
//! That is not hypothetical. On the canonical, one peer of 130 accounted for
//! all 34,390 dropped packets in 24 h — ~24/s sustained, with no idle hour in
//! the window — while the other 129 dropped none.
//!
//! # Where this sits, and why that placement is the point
//!
//! Reticulum puts flow control at Link/Channel and leaves the interface layer
//! best-effort: `BufferFull` → drop is correct behaviour there, not a bug. So
//! this breaker does not try to make the interface reliable. It makes the
//! shedding *cheap* and *visible*, and it leaves the reliability question to
//! the layer that owns it.
//!
//! Cheap matters literally. [`dispatch_actions`] masks a packet — an Ed25519
//! signature plus an HKDF mask stream, per packet — and only then discovers
//! the interface will not take it. A pinned queue therefore pays full IFAC
//! crypto for every packet it is about to throw away. The breaker is consulted
//! *before* masking, so a shed packet costs a state check instead of a
//! signature.
//!
//! # What it will not shed
//!
//! Proofs are never shed, in any state. A proof is the delivery confirmation
//! the peer's producer is waiting on; drop it and the producer retransmits,
//! which raises the offered load exactly when it is already too high. Shedding
//! proofs is how a congested link becomes a collapsing one. New data yields;
//! confirmations of data already accepted do not.
//!
//! [`dispatch_actions`]: leviculum_core::transport::dispatch_actions

use leviculum_core::packet::PacketType;

/// Consecutive failed sends before the circuit opens.
///
/// Three, because a single `BufferFull` is normal on a constrained link and
/// two can be one slow drain; three in a row is a peer that is not keeping up.
/// The reference channel uses the same order of magnitude before it gives up
/// on a packet (`Channel.py`, `_max_tries`).
pub(crate) const BREAKER_TRIP_THRESHOLD: u32 = 3;

/// First cooldown after the circuit opens, in milliseconds.
pub(crate) const BREAKER_COOLDOWN_BASE_MS: u64 = 500;

/// Ceiling on the cooldown, in milliseconds.
///
/// Five minutes, deliberately the same cadence a backed-off producer should
/// trickle at: when both sides are at their limit, the probe and the trickle
/// meet at roughly the same rate instead of beating against each other.
pub(crate) const BREAKER_COOLDOWN_MAX_MS: u64 = 300_000;

/// What the breaker says about one outbound packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admit {
    /// Hand it to the interface.
    Send,
    /// Shed it here, before masking. The circuit is open and this packet is
    /// not one of the kinds that outrank the circuit.
    Shed,
}

/// Circuit position for one interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BreakerState {
    /// Normal service.
    Closed,
    /// Shedding. Nothing but proofs goes out until the cooldown expires.
    Open,
    /// One probe is allowed through to find out whether the peer recovered.
    HalfOpen,
}

impl BreakerState {
    /// Stable lowercase label for logs, gauges and the status snapshot.
    pub(crate) fn label(self) -> &'static str {
        match self {
            BreakerState::Closed => "closed",
            BreakerState::Open => "open",
            BreakerState::HalfOpen => "half-open",
        }
    }
}

/// Per-interface outbound breaker.
///
/// One of these per interface id, held by the driver beside that interface's
/// retry queue. It is fed by the dispatch result — every `BufferFull` is a
/// failure, every accepted send is a success — and consulted before masking.
#[derive(Debug, Clone)]
pub(crate) struct PeerBreaker {
    state: BreakerState,
    /// Consecutive failures; reset by any success.
    consecutive_failures: u32,
    /// When the current open period ends (absolute ms).
    probe_at_ms: u64,
    /// Current cooldown, doubling per failed probe up to the ceiling.
    cooldown_ms: u64,
    /// True while a half-open probe is outstanding, so exactly one packet is
    /// spent finding out whether the peer came back.
    probe_in_flight: bool,
    /// Packets shed while open — never masked, never signed, never queued.
    shed_packets: u64,
    /// Bytes those packets would have carried, pre-mask.
    shed_bytes: u64,
    /// How many times this circuit has opened.
    trips: u64,
}

impl Default for PeerBreaker {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerBreaker {
    /// A closed breaker with no history.
    pub(crate) fn new() -> Self {
        Self {
            state: BreakerState::Closed,
            consecutive_failures: 0,
            probe_at_ms: 0,
            cooldown_ms: BREAKER_COOLDOWN_BASE_MS,
            probe_in_flight: false,
            shed_packets: 0,
            shed_bytes: 0,
            trips: 0,
        }
    }

    /// Current position, after applying any cooldown that has expired.
    ///
    /// Takes `now_ms` because the transition out of [`BreakerState::Open`] is
    /// driven by the clock, not by an event: nothing arrives to tell us the
    /// peer recovered, so the only way to find out is to try.
    pub(crate) fn state(&mut self, now_ms: u64) -> BreakerState {
        if self.state == BreakerState::Open && now_ms >= self.probe_at_ms {
            self.state = BreakerState::HalfOpen;
            self.probe_in_flight = false;
        }
        self.state
    }

    /// Whether this packet may go out, and what that costs.
    ///
    /// `raw` is the packet as the core produced it — before IFAC masking,
    /// which is the whole point of asking here.
    pub(crate) fn admits(&mut self, now_ms: u64, raw: &[u8]) -> Admit {
        // A proof outranks the circuit in every state. See the module note:
        // shedding confirmations is what turns congestion into collapse.
        if is_proof(raw) {
            return Admit::Send;
        }

        match self.state(now_ms) {
            BreakerState::Closed => Admit::Send,
            BreakerState::HalfOpen => {
                if self.probe_in_flight {
                    self.record_shed(raw);
                    Admit::Shed
                } else {
                    self.probe_in_flight = true;
                    Admit::Send
                }
            }
            BreakerState::Open => {
                self.record_shed(raw);
                Admit::Shed
            }
        }
    }

    /// The interface accepted a packet.
    pub(crate) fn on_send_ok(&mut self) {
        self.consecutive_failures = 0;
        self.probe_in_flight = false;
        if self.state != BreakerState::Closed {
            // A successful probe closes the circuit and forgives the cooldown:
            // the next episode starts from the base again, so a peer that
            // recovers is not punished for having been unreachable once.
            self.state = BreakerState::Closed;
            self.cooldown_ms = BREAKER_COOLDOWN_BASE_MS;
        }
    }

    /// The interface refused a packet (`BufferFull`).
    pub(crate) fn on_send_failed(&mut self, now_ms: u64) {
        self.probe_in_flight = false;

        match self.state {
            BreakerState::HalfOpen => {
                // The probe failed: the peer is still not draining. Back off
                // further rather than probing at the same rate.
                self.cooldown_ms = (self.cooldown_ms * 2).min(BREAKER_COOLDOWN_MAX_MS);
                self.open(now_ms);
            }
            BreakerState::Open => {
                // A proof got through the open circuit and failed. That is not
                // evidence about the cooldown, so leave it alone.
            }
            BreakerState::Closed => {
                self.consecutive_failures += 1;
                if self.consecutive_failures >= BREAKER_TRIP_THRESHOLD {
                    self.open(now_ms);
                }
            }
        }
    }

    fn open(&mut self, now_ms: u64) {
        let was_closed = self.state == BreakerState::Closed;
        self.state = BreakerState::Open;
        self.probe_at_ms = now_ms.saturating_add(self.cooldown_ms);
        self.probe_in_flight = false;
        if was_closed {
            self.trips += 1;
        }
    }

    fn record_shed(&mut self, raw: &[u8]) {
        self.shed_packets += 1;
        self.shed_bytes += raw.len() as u64;
    }

    /// Packets shed by this breaker since the node started.
    pub(crate) fn shed_packets(&self) -> u64 {
        self.shed_packets
    }

    /// Bytes those shed packets would have carried.
    pub(crate) fn shed_bytes(&self) -> u64 {
        self.shed_bytes
    }

    /// How many times this circuit has opened.
    pub(crate) fn trips(&self) -> u64 {
        self.trips
    }

    /// Current cooldown in milliseconds.
    pub(crate) fn cooldown_ms(&self) -> u64 {
        self.cooldown_ms
    }
}

/// Whether these pre-mask bytes are a proof.
///
/// The packet type is the low two bits of the first header byte
/// (`[ifac:1][header_type:1][context:1][transport:1][dest_type:2][packet_type:2]`,
/// `leviculum_core::packet`). This reads the packet as the core produced it,
/// so it is valid only before IFAC masking — which is where the breaker runs.
fn is_proof(raw: &[u8]) -> bool {
    match raw.first() {
        Some(b) => PacketType::try_from(*b & 0x03) == Ok(PacketType::Proof),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data packet: packet_type bits = 0b00.
    fn data() -> Vec<u8> {
        vec![0x00, 0x00, 1, 2, 3]
    }

    /// A proof: packet_type bits = 0b11.
    fn proof() -> Vec<u8> {
        vec![0x03, 0x00, 9, 9]
    }

    #[test]
    fn a_closed_breaker_sends_everything() {
        let mut b = PeerBreaker::new();
        assert_eq!(b.admits(0, &data()), Admit::Send);
        assert_eq!(b.admits(0, &proof()), Admit::Send);
        assert_eq!(b.state(0), BreakerState::Closed);
    }

    #[test]
    fn three_consecutive_failures_open_the_circuit() {
        let mut b = PeerBreaker::new();
        b.on_send_failed(0);
        b.on_send_failed(0);
        assert_eq!(b.state(0), BreakerState::Closed, "two is not yet a pattern");
        b.on_send_failed(0);
        assert_eq!(b.state(0), BreakerState::Open);
        assert_eq!(b.trips(), 1);
    }

    #[test]
    fn a_success_forgives_the_run() {
        let mut b = PeerBreaker::new();
        b.on_send_failed(0);
        b.on_send_failed(0);
        b.on_send_ok();
        b.on_send_failed(0);
        assert_eq!(
            b.state(0),
            BreakerState::Closed,
            "failures must be consecutive to count"
        );
    }

    #[test]
    fn an_open_circuit_sheds_data_without_masking_it() {
        let mut b = PeerBreaker::new();
        for _ in 0..BREAKER_TRIP_THRESHOLD {
            b.on_send_failed(0);
        }
        assert_eq!(b.admits(0, &data()), Admit::Shed);
        assert_eq!(b.shed_packets(), 1);
        assert_eq!(b.shed_bytes(), data().len() as u64);
    }

    #[test]
    fn an_open_circuit_still_carries_proofs() {
        let mut b = PeerBreaker::new();
        for _ in 0..BREAKER_TRIP_THRESHOLD {
            b.on_send_failed(0);
        }
        assert_eq!(b.state(0), BreakerState::Open);
        assert_eq!(
            b.admits(0, &proof()),
            Admit::Send,
            "a proof is the confirmation the producer is waiting on; shedding \
             it makes the producer retransmit and raises the offered load"
        );
        assert_eq!(b.shed_packets(), 0, "a proof must not count as shed");
    }

    #[test]
    fn the_cooldown_expiring_allows_exactly_one_probe() {
        let mut b = PeerBreaker::new();
        for _ in 0..BREAKER_TRIP_THRESHOLD {
            b.on_send_failed(0);
        }
        assert_eq!(b.admits(0, &data()), Admit::Shed);

        let t = BREAKER_COOLDOWN_BASE_MS;
        assert_eq!(b.state(t), BreakerState::HalfOpen);
        assert_eq!(b.admits(t, &data()), Admit::Send, "the probe goes out");
        assert_eq!(
            b.admits(t, &data()),
            Admit::Shed,
            "only one packet is spent finding out"
        );
    }

    #[test]
    fn a_successful_probe_closes_the_circuit_and_resets_the_cooldown() {
        let mut b = PeerBreaker::new();
        for _ in 0..BREAKER_TRIP_THRESHOLD {
            b.on_send_failed(0);
        }
        // Fail one probe so the cooldown has grown.
        let t1 = BREAKER_COOLDOWN_BASE_MS;
        assert_eq!(b.state(t1), BreakerState::HalfOpen);
        b.on_send_failed(t1);
        assert!(b.cooldown_ms() > BREAKER_COOLDOWN_BASE_MS);

        let t2 = t1 + b.cooldown_ms();
        assert_eq!(b.state(t2), BreakerState::HalfOpen);
        b.on_send_ok();
        assert_eq!(b.state(t2), BreakerState::Closed);
        assert_eq!(
            b.cooldown_ms(),
            BREAKER_COOLDOWN_BASE_MS,
            "a peer that recovers is not punished for having been unreachable"
        );
    }

    #[test]
    fn a_failed_probe_doubles_the_cooldown_up_to_the_ceiling() {
        let mut b = PeerBreaker::new();
        for _ in 0..BREAKER_TRIP_THRESHOLD {
            b.on_send_failed(0);
        }
        let mut now = 0u64;
        let mut seen = Vec::new();
        for _ in 0..12 {
            now += b.cooldown_ms();
            assert_eq!(b.state(now), BreakerState::HalfOpen);
            b.on_send_failed(now);
            seen.push(b.cooldown_ms());
        }
        assert_eq!(
            *seen.last().unwrap(),
            BREAKER_COOLDOWN_MAX_MS,
            "the cooldown must stop doubling at the ceiling"
        );
        assert!(
            seen.windows(2).all(|w| w[1] >= w[0]),
            "the cooldown must never shrink while probes keep failing"
        );
    }

    #[test]
    fn a_proof_failing_while_open_does_not_move_the_cooldown() {
        let mut b = PeerBreaker::new();
        for _ in 0..BREAKER_TRIP_THRESHOLD {
            b.on_send_failed(0);
        }
        let before = b.cooldown_ms();
        // Proofs still go out while open, and can still be refused.
        assert_eq!(b.admits(0, &proof()), Admit::Send);
        b.on_send_failed(0);
        assert_eq!(
            b.cooldown_ms(),
            before,
            "a proof's failure says nothing about when to probe with data"
        );
    }

    #[test]
    fn a_trip_is_counted_once_per_episode() {
        let mut b = PeerBreaker::new();
        for _ in 0..BREAKER_TRIP_THRESHOLD {
            b.on_send_failed(0);
        }
        assert_eq!(b.trips(), 1);
        // Failing probes re-open the same episode; that is not a new trip.
        let t = BREAKER_COOLDOWN_BASE_MS;
        assert_eq!(b.state(t), BreakerState::HalfOpen);
        b.on_send_failed(t);
        assert_eq!(b.trips(), 1);
    }

    #[test]
    fn packet_type_is_read_from_the_unmasked_header() {
        assert!(is_proof(&proof()));
        assert!(!is_proof(&data()));
        assert!(!is_proof(&[]), "an empty frame is not a proof");
        // Only the low two bits decide; the rest of the flags are noise here.
        assert!(is_proof(&[0xFF, 0x00]), "0b11 in the low bits is a proof");
        assert!(!is_proof(&[0xFC, 0x00]), "0b00 in the low bits is data");
    }
}
