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
//!
//! # Why the fallback class is NOT ordered by address (#412)
//!
//! A resolvable private address has `01` in its top two bits and a
//! SoftDevice static random address has `11`
//! (`peer::any_rpa_sorts_below_any_static_random_address`), so a phone
//! is numerically below every board, always. It is therefore never a
//! strict candidate, and under a fallback class ordered by address it
//! is ALWAYS the first one: every fallback dial of every board in the
//! room lands on it, and 48 s later it has a new address and wins
//! again. `graph_formation.rs` measures the cost of that at 42 % of the
//! room's dials with one Android Columba in it, and the rig measured
//! the steady-state version — `feld-t114` spent all 7 outgoing links it
//! made over two days on the phone and none on a board.
//!
//! The strict class has no such problem: its members are boards with
//! static addresses, and ordering them is what closed the
//! saturated-cycle lock above. So the fix has to be narrow, and the
//! harness measured two candidates for it (per 1000 arrival orders,
//! split board graphs at n=10 / n=20, then the share of the room's
//! dials aimed at the churning peer):
//!
//! | fallback order | empty room | one phone   |
//! |----------------|------------|-------------|
//! | `Address`      | 0 / 0      | 44 / 180, 42 % / 28 % |
//! | `FirstHeard`   | 2 / 74     |  6 /  58, 13 % /  3 % |
//! | `RotatingLast` | 0 / 0      |  0 /   0,  0 % /  0 % |
//!
//! [`FallbackOrder::FirstHeard`] drops the address term entirely and
//! breaks the tie by which advertiser the window heard first. It works
//! against the phone, and it gives back most of the saturated-cycle
//! lock in the empty room, because the agreement between searchers
//! that the address order produced is exactly what closed that lock.
//! Both halves are measured, so neither is a guess.
//!
//! [`FallbackOrder::RotatingLast`], what ships, keeps the address term
//! for the candidates whose address is a key and puts the ones that
//! redraw it behind them, first-heard among themselves
//! ([`rotating_address`]). Every board in a room of boards has a
//! static random address, so the empty room is bit-identical to the
//! order before it — the pinned cells prove that rather than assume it
//! — and a rotating peer is dialled only when it is the only candidate
//! the rule permitted.
//!
//! [`FallbackOrder::Address`] is the pre-#412 order, kept because
//! `graph_formation.rs` replays the #375 and #412 findings with it —
//! the same reason `FallbackSpec::Quiet` survives there.

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

/// What breaks a tie inside the FALLBACK class (#412). The strict
/// class is unaffected by either value: it is ordered by free slots
/// and then by address, as it has been since #375 item 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FallbackOrder {
    /// The lowest address wins, as the strict sort itself would have
    /// ordered them. What #375 item 2 shipped and what #412 measured
    /// the cost of; kept so `graph_formation.rs` can replay both
    /// findings, never shipped.
    Address,
    /// Whichever of the tied candidates the window heard first, for
    /// every candidate. It carries no correlation with the address, so
    /// a peer whose address class is structurally the lowest in the
    /// room cannot win the class by construction — but it also gives
    /// up the agreement between searchers that closed the
    /// saturated-cycle lock, and `graph_formation.rs` measures that
    /// back as 2 and 74 split graphs per 1000 orders in an empty room.
    /// Kept as the record of that trade; not shipped.
    FirstHeard,
    /// The lowest address wins among candidates whose address is a
    /// KEY, and those candidates all outrank the ones whose address is
    /// not, which fall back to first-heard among themselves. The
    /// shipped order since #412; see [`rotating_address`].
    #[default]
    RotatingLast,
}

/// Whether this address is one its owner re-draws, by the class its
/// top two bits name (Core Spec Vol 6 Part B §1.3.2.2): `01` is a
/// resolvable private address, which Android redraws every few minutes
/// for privacy, and `00` a non-resolvable one, redrawn the same way.
/// `11` is a static random address, which the Core Spec binds to the
/// power cycle, and which is what both our stacks and every board in
/// the room use.
///
/// Why the window asks this at all: the address is an ordering KEY,
/// and a key its holder redraws at will is not one. A peer with a
/// resolvable private address gets a fresh draw of the whole 46-bit
/// space every rotation, so ordering it against fixed addresses hands
/// it a fresh chance to be first, forever — that is #412's mechanism
/// exactly, and at the bottom of the RPA class it is not a chance but
/// a certainty.
///
/// What this is NOT: a test for "is this a phone", and it never
/// decides whether a link is permitted. It reorders candidates the
/// rule has ALREADY permitted, inside the fallback class alone, and a
/// peer that is the only candidate is dialled whatever its address
/// class.
///
/// The residual, stated because the window cannot see past it: a
/// PUBLIC address (an IEEE assignment, which a BlueZ host commonly
/// uses) has no constraint on its top bits, so one whose OUI begins in
/// `0x00..=0x7F` reads as rotating here and is ordered behind the
/// boards inside the fallback class. It costs such a peer position in
/// one class, never a dial. The address TYPE the scanner receives with
/// every advertising report would settle it exactly; plumbing that
/// through both stacks is the honest fix and it is not this one.
#[must_use]
pub fn rotating_address(addr: u64) -> bool {
    addr >> 46 != 0b11
}

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
    /// Which sighting of a new address this was, counted from the
    /// window's open — the [`FallbackOrder::FirstHeard`] tie-break
    /// (#412). It is assigned once, when the address enters the table,
    /// and a re-advertisement never refreshes it: "heard first" is
    /// about the first time, not the last.
    seq: u32,
    payload: T,
}

/// Preference key, lowest wins: strict-class verdicts (the sort or the
/// capability override permitted the dial) outrank fallback-class
/// verdicts; within a class the peer with the most free incoming slots
/// wins; and equal counts fall back to the third term.
///
/// The middle term is a DEFICIT (`PERIPH_SLOTS` minus the count) so
/// that "lowest key wins" stays the one comparison rule, and a peer
/// that advertised no count deficits by zero: it ties with the most
/// generous advertisers and the last term decides, as before item 3.
///
/// The last two terms are the lower address in the strict class — the
/// order the strict sort itself would have produced — and, since #412,
/// the [`FallbackOrder`] in the fallback class: a group term, then
/// either the address or the sighting order within it. `seq` and
/// `addr` are never compared against each other: class is the first
/// term and the group the third, so a key that carries a `seq` is only
/// ever ordered against other `seq` keys.
fn rank<T>(candidate: &Candidate<T>, order: FallbackOrder) -> (u8, u8, u8, u64) {
    let class = match candidate.decision {
        ConnectDecision::InitiateFallback => 1,
        _ => 0,
    };
    let deficit = PERIPH_SLOTS.saturating_sub(candidate.free_slots.unwrap_or(PERIPH_SLOTS));
    let (group, tie) = match (class, order) {
        (0, _) | (_, FallbackOrder::Address) => (0, candidate.addr),
        (_, FallbackOrder::FirstHeard) => (0, u64::from(candidate.seq)),
        (_, FallbackOrder::RotatingLast) if rotating_address(candidate.addr) => {
            (1, u64::from(candidate.seq))
        }
        (_, FallbackOrder::RotatingLast) => (0, candidate.addr),
    };
    (class, deficit, group, tie)
}

/// One scan window's eligible candidates, bounded and allocation-free.
///
/// [`offer`](Self::offer) every eligible sighting during the window,
/// then [`into_best`](Self::into_best) once to get the dial target:
/// strict verdicts before fallback verdicts, the emptiest advertiser
/// first within a class, ties broken by the lowest address in the
/// strict class and by the earliest sighting in the fallback class
/// (#412). Duplicate addresses collapse into one entry (a waiting peer
/// advertises several times per window); a full table keeps the best
/// candidates, so overflow can drop a `seen=` increment but never
/// change the choice.
#[derive(Debug)]
pub struct CandidateTable<T, const N: usize> {
    entries: [Option<Candidate<T>>; N],
    order: FallbackOrder,
    /// Distinct addresses offered so far, the source of each entry's
    /// `seq`. It counts sightings that ENTERED the table, which is the
    /// only order the choice can be made from.
    next_seq: u32,
}

impl<T, const N: usize> Default for CandidateTable<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> CandidateTable<T, N> {
    /// An empty window under the shipped order
    /// ([`FallbackOrder::RotatingLast`]).
    pub fn new() -> Self {
        Self::with_fallback_order(FallbackOrder::RotatingLast)
    }

    /// An empty window under a stated fallback order. Only
    /// `graph_formation.rs` passes anything but the default: replaying
    /// the #375 and #412 findings needs the order they were measured
    /// under.
    pub fn with_fallback_order(order: FallbackOrder) -> Self {
        Self {
            entries: core::array::from_fn(|_| None),
            order,
            next_seq: 0,
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
    /// is how a hint turns into a lie. Its `seq` is the opposite: it
    /// keeps the FIRST sighting's, because that is what first-heard
    /// means (#412).
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
        let order = self.order;
        let mut candidate = Candidate {
            addr,
            decision,
            free_slots,
            seq: self.next_seq,
            payload,
        };
        if let Some(existing) = self
            .entries
            .iter_mut()
            .flatten()
            .find(|entry| entry.addr == addr)
        {
            candidate.seq = existing.seq;
            if rank(&candidate, order) < rank(existing, order) {
                existing.decision = candidate.decision;
            }
            existing.free_slots = candidate.free_slots;
            existing.payload = candidate.payload;
            return true;
        }
        self.next_seq = self.next_seq.saturating_add(1);
        if let Some(slot) = self.entries.iter_mut().find(|entry| entry.is_none()) {
            *slot = Some(candidate);
            return true;
        }
        let worst = self
            .entries
            .iter_mut()
            .flatten()
            .max_by_key(|entry| rank(entry, order));
        match worst {
            Some(worst) if rank(&candidate, order) < rank(worst, order) => {
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
        let order = self.order;
        self.entries
            .into_iter()
            .flatten()
            .min_by_key(|entry| rank(entry, order))
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

    /// The two address classes as the air presents them (Core Spec Vol
    /// 6 Part B §1.3.2.2): a resolvable private address has `01` on
    /// top, a static random one `11`. `peer.rs` pins the consequence —
    /// every RPA is numerically below every static random address.
    const PHONE: u64 = 0x4A1B_2C3D_4E5F;
    const BOARD: u64 = 0xC01D_BEEF_0001;

    /// #412's change, and the exact failure it removes: the phone's
    /// address is structurally the lowest in the room, so under the
    /// pre-#412 order it won the fallback class from either position.
    /// The shipped order puts every address its owner redraws behind
    /// every address that stays put.
    #[test]
    fn the_fallback_class_puts_a_rotating_address_behind_a_fixed_one() {
        for offers in [[BOARD, PHONE], [PHONE, BOARD]] {
            let mut t = Table::new();
            for addr in offers {
                assert!(t.offer(addr, InitiateFallback, None, "peer"));
            }
            assert_eq!(
                t.into_best().map(|(addr, _, _)| addr),
                Some(BOARD),
                "heard {offers:x?}: the lower address took the class again"
            );
            // The control that the fixture is a real difference: the
            // pre-#412 order, same offers, elects the phone both ways.
            let mut legacy: CandidateTable<&'static str, 4> =
                CandidateTable::with_fallback_order(FallbackOrder::Address);
            for addr in offers {
                assert!(legacy.offer(addr, InitiateFallback, None, "peer"));
            }
            assert_eq!(
                legacy.into_best().map(|(addr, _, _)| addr),
                Some(PHONE),
                "the address order must still be replayable"
            );
        }
    }

    /// It is a preference inside a class the rule already permitted,
    /// never a refusal: a rotating peer with nobody to lose to is
    /// still the dial.
    #[test]
    fn a_rotating_address_is_still_dialled_when_it_is_the_only_candidate() {
        let mut t = Table::new();
        assert!(t.offer(PHONE, InitiateFallback, None, "phone"));
        assert_eq!(t.into_best(), Some((PHONE, InitiateFallback, "phone")));
    }

    #[test]
    fn rotating_addresses_among_themselves_go_by_first_heard() {
        let higher = PHONE | 0x0000_0F00_0000;
        assert!(rotating_address(PHONE) && rotating_address(higher));
        let mut t = Table::new();
        assert!(t.offer(higher, InitiateFallback, None, "heard first"));
        assert!(t.offer(PHONE, InitiateFallback, None, "lower, heard later"));
        assert_eq!(
            t.into_best(),
            Some((higher, InitiateFallback, "heard first"))
        );
    }

    #[test]
    fn the_address_class_test_reads_the_core_spec_classes() {
        // Static random (`11`) is the only fixed one; the two private
        // classes (`01`, `00`) and the reserved `10` are not keys.
        assert!(!rotating_address(0xC000_0000_0000));
        assert!(!rotating_address(0xFFFF_FFFF_FFFF));
        assert!(rotating_address(0xBFFF_FFFF_FFFF));
        assert!(rotating_address(0x4000_0000_0000));
        assert!(rotating_address(0x3FFF_FFFF_FFFF));
        assert!(rotating_address(0));
    }

    /// Under `FirstHeard` — the order measured and not shipped — the
    /// address plays no part at all, phone or board.
    #[test]
    fn the_first_heard_order_ignores_the_address_entirely() {
        for offers in [[BOARD, PHONE], [PHONE, BOARD]] {
            let mut t: CandidateTable<&'static str, 4> =
                CandidateTable::with_fallback_order(FallbackOrder::FirstHeard);
            for addr in offers {
                assert!(t.offer(addr, InitiateFallback, None, "peer"));
            }
            assert_eq!(
                t.into_best().map(|(addr, _, _)| addr),
                Some(offers[0]),
                "first heard wins whichever it was"
            );
        }
    }

    #[test]
    fn the_strict_class_is_still_ordered_by_address_under_every_order() {
        for order in [
            FallbackOrder::FirstHeard,
            FallbackOrder::Address,
            FallbackOrder::RotatingLast,
        ] {
            let mut t: CandidateTable<&'static str, 4> = CandidateTable::with_fallback_order(order);
            assert!(t.offer(PHONE, InitiateLowerAddress, None, "heard first"));
            assert!(t.offer(0x10, InitiateLowerAddress, None, "lowest"));
            assert_eq!(
                t.into_best(),
                Some((0x10, InitiateLowerAddress, "lowest")),
                "{order:?}: the strict class is ordered by address, rotating or not"
            );
        }
    }

    #[test]
    fn free_slots_still_outrank_both_fallback_tie_breaks() {
        // The tie-break is the LAST term: a fallback candidate that
        // says it has room beats one ahead of it on the tie that says
        // it is nearly full — including across the rotating/fixed
        // groups, which is what keeps this a preference and not a
        // second eligibility rule.
        let mut t = Table::new();
        assert!(t.offer(BOARD, InitiateFallback, Some(1), "fixed, nearly full"));
        assert!(t.offer(PHONE, InitiateFallback, Some(3), "rotating, empty"));
        assert_eq!(
            t.into_best(),
            Some((PHONE, InitiateFallback, "rotating, empty"))
        );
    }

    #[test]
    fn a_re_advertisement_does_not_move_a_candidate_to_the_back() {
        // "Heard first" is about the first sighting: a peer that keeps
        // advertising through the window must not lose its place to one
        // that advertised once.
        let mut t = Table::new();
        assert!(t.offer(0x90, InitiateFallback, None, "first"));
        assert!(t.offer(0x10, InitiateFallback, None, "second"));
        assert!(t.offer(0x90, InitiateFallback, None, "first again"));
        assert_eq!(t.seen(), 2);
        assert_eq!(
            t.into_best(),
            Some((0x90, InitiateFallback, "first again")),
            "the payload refreshes, the place does not"
        );
    }

    #[test]
    fn overflow_keeps_the_earliest_rotating_candidates() {
        let mut t = Table::new();
        for addr in [0x40u64, 0x30, 0x20, 0x10] {
            assert!(t.offer(addr, InitiateFallback, None, "held"));
        }
        // Lower-addressed than every holder and rotating like them, so
        // under the pre-#412 order it would have displaced one; now it
        // ranks worse than all four and is dropped.
        assert!(!t.offer(0x05, InitiateFallback, None, "latecomer"));
        assert_eq!(t.seen(), 4);
        // A fixed address displaces one of them whenever it arrives.
        let mut fixed = Table::new();
        for addr in [0x40u64, 0x30, 0x20, 0x10] {
            assert!(fixed.offer(addr, InitiateFallback, None, "held"));
        }
        assert!(fixed.offer(BOARD, InitiateFallback, None, "fixed"));
        assert_eq!(t.into_best(), Some((0x40, InitiateFallback, "held")));
        assert_eq!(fixed.into_best(), Some((BOARD, InitiateFallback, "fixed")));
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
