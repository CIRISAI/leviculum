//! The #375 scan-window candidate collection: which peer a scanner
//! dials when more than one advertiser is eligible.
//!
//! First-advertiser-wins — dial whichever eligible PDU the radio happens
//! to hear first — is what enables the saturated-cycle lock the
//! `graph_formation` harness measures: a fallback dial can land inside
//! the dialler's own component and close a cycle there, spending the
//! component's last free central, and if every component saturates this
//! way simultaneously, disjoint components never merge. Collecting one
//! bounded window of verdicts and dialling the LOWEST-addressed eligible
//! candidate — strict verdicts before fallback verdicts — removes the
//! lock entirely (0/1000 disconnected orders at 10 and 20 boards in the
//! harness, against 2/1000 and 48/1000 under first-advertiser-wins).
//!
//! The choice lives here as one pure structure, [`CandidateTable`], so
//! the firmware's central task, lnsd's BlueZ scanner and the simulation
//! agree on the policy by construction rather than by three readings of
//! it.
//!
//! # The free-slot preference (#375 item 3)
//!
//! Lowest-address-wins still picks blind between a peer with three free
//! incoming slots and one with its last slot free, so several searchers
//! elect the same board in the same window and all but one are refused.
//! Since #375 item 3 an advertiser may say how many slots it has left
//! ([`crate::adv::free_slots`]), and among candidates of the same class
//! the one with the MOST free slots wins; equal counts fall back to the
//! lowest address, exactly as before. The change is a refinement of the
//! existing order, not a new rule: nothing becomes eligible or
//! ineligible because of it.
//!
//! A peer that said nothing — an older board, a phone, another
//! implementation — is ranked as though it had all its slots free, the
//! same "assume full capability" v0.3.0 §3.2 applies to the capability
//! flags themselves. That is what keeps it at today's behaviour: it
//! ties with the most generous advertisers and the address decides, as
//! it always did. Ranking it last would demote every phone below every
//! board, and ranking a silent peer as "zero free" is exactly the
//! misreading [`crate::adv::CAP_FREE_SLOTS_VALID`] exists to prevent.
//!
//! The count is a HINT and may be stale by the time the dial lands.
//! Nothing downstream trusts it: the duplicate and refusal paths are
//! untouched, and a peer that advertised a free slot and has none left
//! refuses the connection exactly as it does today.

use crate::adv::PERIPH_SLOTS;
use crate::peer::ConnectDecision;

/// How long the strict rule may search without one `initiate` verdict —
/// and without a live connection in either role — before the scanner
/// switches to [`crate::ScanMode::Fallback`] (Codeberg #375).
///
/// 30 s, sized between the cadences on either side of it. Below: both
/// stacks' connect attempts and retry backoffs run on ~5 s cycles and
/// both scanners hear a waiting peer — which advertises several times a
/// second — within seconds, so 30 s spans several complete
/// search-connect-backoff cycles and a permitted peer that exists gets
/// found under the strict rule rather than tripping a premature
/// fallback. Above: this is the whole BLE-less window of a stranded
/// board (one that outranks every visible neighbour, #375's
/// disconnected-graph case), so tens of seconds is the ceiling the
/// issue allows; half a minute also keeps the stranding shorter than
/// one dead-end-table TTL (120 s on both stacks).
pub const SCAN_FALLBACK_AFTER_MS: u64 = 30_000;

/// How long the scanner keeps collecting after the FIRST eligible
/// candidate, before it closes the window and dials the best one.
///
/// 3 s. Long enough to hear every waiting peer: a waiting Columba peer
/// advertises on a sub-second cadence (the SoftDevice default is
/// 250 ms), so even at the firmware's 30 % passive-scan duty a peer
/// present during the window gets ~4 audible advertising events and
/// missing all of them is vanishingly unlikely. Short against the
/// bounds around it: a tenth of [`SCAN_FALLBACK_AFTER_MS`], and smaller
/// than one connect-timeout + retry-backoff cycle (~10 s), so the
/// window delays a dial by less than one failed wrong dial would have
/// cost.
pub const SCAN_WINDOW_COLLECT_MS: u64 = 3_000;

/// Bound on distinct advertisers one window can hold.
///
/// 16: four times the link limit on either stack, and comfortably above
/// the number of *eligible* candidates a desk or room mesh produces in
/// one 3 s window (full boards do not advertise, already-linked and
/// backed-off addresses are filtered before the table). ~200 bytes of
/// state at this bound. On overflow the table keeps the best candidates
/// and drops the worst newcomer, so the CHOICE is unaffected — only the
/// `seen=` count saturates at the bound.
pub const WINDOW_CANDIDATES: usize = 16;

/// One collected candidate: the address the sort compares, the verdict
/// that made it eligible, and the caller's payload (the dialling handle
/// — a slot index in the simulation, an `Address` on the firmware, a
/// `bluer` address in lnsd).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Candidate<T> {
    addr: u64,
    decision: ConnectDecision,
    /// Free incoming slots the peer advertised, `None` when it said
    /// nothing (#375 item 3).
    free_slots: Option<u8>,
    payload: T,
}

/// Preference key, lowest wins: strict-class verdicts (the sort or the
/// capability override permitted the dial) outrank fallback-class
/// verdicts; within a class the peer with the most free incoming slots
/// wins; and equal counts fall back to the lower address — the same
/// order the strict sort itself would have produced, extended to the
/// fallback set.
///
/// The middle term is a DEFICIT (`PERIPH_SLOTS` minus the count) so
/// that "lowest key wins" stays the one comparison rule, and a peer
/// that advertised no count deficits by zero: it ties with the most
/// generous advertisers and the address decides, as before item 3.
fn rank<T>(candidate: &Candidate<T>) -> (u8, u8, u64) {
    let class = match candidate.decision {
        ConnectDecision::InitiateFallback => 1,
        _ => 0,
    };
    let deficit = PERIPH_SLOTS.saturating_sub(candidate.free_slots.unwrap_or(PERIPH_SLOTS));
    (class, deficit, candidate.addr)
}

/// One scan window's eligible candidates, bounded and allocation-free.
///
/// [`offer`](Self::offer) every eligible sighting during the window,
/// then [`into_best`](Self::into_best) once to get the dial target:
/// the lowest-addressed candidate, strict verdicts before fallback
/// verdicts. Duplicate addresses collapse into one entry (a waiting
/// peer advertises several times per window); a full table keeps the
/// best candidates, so overflow can drop a `seen=` increment but never
/// change the choice.
#[derive(Debug)]
pub struct CandidateTable<T, const N: usize> {
    entries: [Option<Candidate<T>>; N],
}

impl<T, const N: usize> Default for CandidateTable<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> CandidateTable<T, N> {
    /// An empty window.
    pub fn new() -> Self {
        Self {
            entries: core::array::from_fn(|_| None),
        }
    }

    /// Record one eligible sighting. Only initiate verdicts are
    /// candidates; anything else is refused (`false`). `free_slots` is
    /// what the peer advertised (`None` when it said nothing, #375
    /// item 3). A sighting of an address already in the table replaces
    /// that entry iff the new verdict ranks better (the payload — a
    /// possibly refreshed dialling handle — is kept current either
    /// way). When the table is full, the newcomer displaces the worst
    /// entry iff it ranks better; a newcomer worse than everything held
    /// is dropped, which cannot change what
    /// [`into_best`](Self::into_best) returns.
    ///
    /// The slot count of a repeated address is taken from the LATEST
    /// sighting, not the best one: unlike the verdict it is a live
    /// value — a link landing on that peer during our window is exactly
    /// the news worth having — and believing the rosier of two readings
    /// is how a hint turns into a lie.
    pub fn offer(
        &mut self,
        addr: u64,
        decision: ConnectDecision,
        free_slots: Option<u8>,
        payload: T,
    ) -> bool {
        if !decision.initiate() {
            return false;
        }
        let candidate = Candidate {
            addr,
            decision,
            free_slots,
            payload,
        };
        if let Some(existing) = self
            .entries
            .iter_mut()
            .flatten()
            .find(|entry| entry.addr == addr)
        {
            if rank(&candidate) < rank(existing) {
                existing.decision = candidate.decision;
            }
            existing.free_slots = candidate.free_slots;
            existing.payload = candidate.payload;
            return true;
        }
        if let Some(slot) = self.entries.iter_mut().find(|entry| entry.is_none()) {
            *slot = Some(candidate);
            return true;
        }
        let worst = self
            .entries
            .iter_mut()
            .flatten()
            .max_by_key(|entry| rank(entry));
        match worst {
            Some(worst) if rank(&candidate) < rank(worst) => {
                *worst = candidate;
                true
            }
            _ => false,
        }
    }

    /// Distinct advertisers held — the `seen=` value of the
    /// `BLE_SCAN_WINDOW` log line. Saturates at `N` on overflow.
    pub fn seen(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    /// Close the window: the best candidate, or `None` for an empty
    /// window (a window is only opened by a first candidate, so both
    /// stacks treat `None` as unreachable-but-handled).
    pub fn into_best(self) -> Option<(u64, ConnectDecision, T)> {
        self.entries
            .into_iter()
            .flatten()
            .min_by_key(rank)
            .map(|c| (c.addr, c.decision, c.payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ConnectDecision::*;

    type Table = CandidateTable<&'static str, 4>;

    #[test]
    fn the_lowest_address_wins_within_a_class() {
        let mut t = Table::new();
        assert!(t.offer(0x30, InitiateLowerAddress, None, "c"));
        assert!(t.offer(0x10, InitiateLowerAddress, None, "a"));
        assert!(t.offer(0x20, InitiatePeripheralOnlyPeer, None, "b"));
        assert_eq!(t.seen(), 3);
        assert_eq!(t.into_best(), Some((0x10, InitiateLowerAddress, "a")));
    }

    #[test]
    fn the_peer_with_the_most_free_slots_wins_within_a_class() {
        let mut t = Table::new();
        // The lowest address is also the fullest: before item 3 it won
        // and three searchers piled onto its last slot.
        assert!(t.offer(0x10, InitiateLowerAddress, Some(1), "nearly full"));
        assert!(t.offer(0x20, InitiateLowerAddress, Some(3), "empty"));
        assert!(t.offer(0x30, InitiateLowerAddress, Some(2), "middling"));
        assert_eq!(
            t.into_best(),
            Some((0x20, InitiateLowerAddress, "empty")),
            "most free slots first, address only after"
        );
    }

    #[test]
    fn equal_free_slot_counts_fall_back_to_the_lowest_address() {
        let mut t = Table::new();
        assert!(t.offer(0x30, InitiateLowerAddress, Some(2), "high"));
        assert!(t.offer(0x10, InitiateLowerAddress, Some(2), "low"));
        assert!(t.offer(0x20, InitiateLowerAddress, Some(2), "mid"));
        assert_eq!(t.into_best(), Some((0x10, InitiateLowerAddress, "low")));
    }

    #[test]
    fn a_peer_that_said_nothing_keeps_its_pre_item_3_standing() {
        // Silence ranks as "all slots free", so against a fully free
        // advertiser the address decides — exactly as it did before the
        // count existed. Ranking silence as zero would have inverted
        // this and demoted every phone below every board.
        let mut t = Table::new();
        assert!(t.offer(0x10, InitiateLowerAddress, None, "silent"));
        assert!(t.offer(0x20, InitiateLowerAddress, Some(3), "all free"));
        assert_eq!(t.into_best(), Some((0x10, InitiateLowerAddress, "silent")));
        // But a peer that says it is nearly full loses to the silent one.
        let mut t = Table::new();
        assert!(t.offer(0x90, InitiateLowerAddress, None, "silent"));
        assert!(t.offer(0x10, InitiateLowerAddress, Some(1), "nearly full"));
        assert_eq!(t.into_best(), Some((0x90, InitiateLowerAddress, "silent")));
    }

    #[test]
    fn a_strict_verdict_outranks_a_fuller_fallback_candidate() {
        // Class still dominates: eligibility is a rule, the slot count
        // is only a preference inside what the rule already permits.
        let mut t = Table::new();
        assert!(t.offer(0x10, InitiateFallback, Some(3), "fallback, empty"));
        assert!(t.offer(0x90, InitiateLowerAddress, Some(0), "strict, full"));
        assert_eq!(
            t.into_best(),
            Some((0x90, InitiateLowerAddress, "strict, full"))
        );
    }

    #[test]
    fn a_repeated_address_takes_the_latest_slot_count() {
        let mut t = Table::new();
        assert!(t.offer(0x10, InitiateLowerAddress, Some(3), "first sighting"));
        // A link landed on it during our window: believe the news.
        assert!(t.offer(0x10, InitiateLowerAddress, Some(1), "later sighting"));
        assert!(t.offer(0x20, InitiateLowerAddress, Some(2), "other"));
        assert_eq!(t.seen(), 2);
        assert_eq!(
            t.into_best(),
            Some((0x20, InitiateLowerAddress, "other")),
            "the stale count would have kept 0x10 in front"
        );
    }

    #[test]
    fn a_fuller_newcomer_does_not_evict_an_emptier_holder() {
        // Overflow evicts by the same key the choice uses, so the slot
        // count cannot be lost to a newcomer that ranks worse on it.
        let mut t = Table::new();
        for addr in [0x10u64, 0x20, 0x30, 0x40] {
            assert!(t.offer(addr, InitiateLowerAddress, Some(3), "empty"));
        }
        assert!(!t.offer(0x05, InitiateLowerAddress, Some(0), "full newcomer"));
        assert_eq!(
            t.into_best(),
            Some((0x10, InitiateLowerAddress, "empty")),
            "a full newcomer must not displace an empty holder"
        );
    }

    #[test]
    fn a_strict_verdict_outranks_a_lower_addressed_fallback_verdict() {
        let mut t = Table::new();
        assert!(t.offer(0x10, InitiateFallback, None, "fallback"));
        assert!(t.offer(0x90, InitiateLowerAddress, None, "strict"));
        assert_eq!(t.into_best(), Some((0x90, InitiateLowerAddress, "strict")));
    }

    #[test]
    fn a_repeated_address_is_one_candidate_with_the_best_verdict() {
        let mut t = Table::new();
        assert!(t.offer(0x10, InitiateFallback, None, "first sighting"));
        assert!(t.offer(0x10, InitiateFallback, None, "second sighting"));
        assert_eq!(t.seen(), 1, "re-advertisement is not a new candidate");
        // An upgrade keeps the address as one entry with the better rank.
        assert!(t.offer(0x10, InitiatePeripheralOnlyPeer, None, "upgraded"));
        assert_eq!(t.seen(), 1);
        assert_eq!(
            t.into_best(),
            Some((0x10, InitiatePeripheralOnlyPeer, "upgraded"))
        );
    }

    #[test]
    fn a_wait_verdict_is_not_a_candidate() {
        let mut t = Table::new();
        assert!(!t.offer(0x10, WaitPeerHasLowerAddress, None, "no"));
        assert!(!t.offer(0x10, NobodyEqualAddresses, None, "no"));
        assert_eq!(t.seen(), 0);
        assert_eq!(t.into_best(), None);
    }

    #[test]
    fn overflow_keeps_the_best_candidates_and_never_changes_the_choice() {
        let mut t = Table::new();
        for addr in [0x40u64, 0x30, 0x20, 0x10] {
            assert!(t.offer(addr, InitiateLowerAddress, None, "filler"));
        }
        assert_eq!(t.seen(), 4, "at the bound");
        // A better newcomer displaces the worst entry.
        assert!(t.offer(0x05, InitiateLowerAddress, None, "best"));
        assert_eq!(t.seen(), 4, "seen saturates at the bound");
        // A worse newcomer is dropped.
        let mut t2 = Table::new();
        for addr in [0x10u64, 0x20, 0x30, 0x40] {
            assert!(t2.offer(addr, InitiateLowerAddress, None, "filler"));
        }
        assert!(!t2.offer(0x50, InitiateLowerAddress, None, "worst"));
        assert_eq!(t.into_best(), Some((0x05, InitiateLowerAddress, "best")));
        assert_eq!(t2.into_best(), Some((0x10, InitiateLowerAddress, "filler")));
    }

    #[test]
    fn a_full_table_still_evicts_a_fallback_for_a_strict_newcomer() {
        let mut t = Table::new();
        for addr in [0x10u64, 0x20, 0x30, 0x40] {
            assert!(t.offer(addr, InitiateFallback, None, "fallback"));
        }
        assert!(t.offer(0xF0, InitiateLowerAddress, None, "strict"));
        assert_eq!(
            t.into_best(),
            Some((0xF0, InitiateLowerAddress, "strict")),
            "the strict newcomer outranks every held fallback"
        );
    }
}
