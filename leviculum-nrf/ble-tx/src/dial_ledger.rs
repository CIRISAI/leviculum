//! What a board remembers about the dials it has already paid for
//! (Codeberg #412 part 2): per PEER IDENTITY, how many dials in a row
//! bought nothing, and whether the board may spend a fallback dial at
//! all right now.
//!
//! # The window no candidate order can defend
//!
//! Everything in [`crate::window`] ORDERS the peers one scan window
//! heard, and an order needs something to prefer. The
//! `graph_formation` harness measures how often there is nothing: in the
//! room the rig has — three boards and an Android Columba — the
//! highest-addressed board's freed outgoing slot goes to the phone in
//! 100 % of the windows where links stand, and 57 % of those windows hold
//! no board at all (38 % at the capture's 600 s lifetime mean). The
//! phone is not preferred over a board there; it is the only thing the
//! rule allows. No tie-break can help, and the two candidate keys #412
//! measured cannot either.
//!
//! What can is a memory of what the last dials BOUGHT. A board that has
//! spent three dials in a row on one identity and got no usable session
//! out of any of them stops reaching for the fallback class for a while,
//! and spends its one outgoing slot only where the address sort says it
//! belongs.
//!
//! # Why the key is an identity and the effect is the whole class
//!
//! The two are one fact. The identity behind an advertisement is
//! unknowable before connecting — the board reads the Identity
//! characteristic post-connect — so a table keyed by identity cannot be
//! consulted per candidate the way the firmware's address table is. What
//! it can decide is whether this board may spend a fallback dial at all,
//! which is what [`DialLedger::fallback_held`] answers: while the pause
//! stands the board dials strict verdicts only (a peer the v2.2 address
//! sort hands it) and leaves a window holding nothing but fallback
//! verdicts alone.
//!
//! Keying by identity rather than by address is what lets the run reach
//! `k` at all. The firmware's dead-end table is address-keyed, and a
//! Columba was seen under five addresses in four minutes: every entry is
//! a first offence forever. One identity behind all five is the whole
//! difference, and it is why this table cannot be folded into that one.
//!
//! # What counts as wasted
//!
//! One dial, one outcome, recorded against the identity the dial turned
//! out to have:
//!
//! - refused post-connect because that identity already held a live link
//!   (the rotated-address duplicate): wasted, always. The connect, the
//!   discovery and the identity read were paid for and no session
//!   existed.
//! - a session that ended below [`LedgerPolicy::useful_session_ms`]:
//!   wasted. The dial was paid for, the link carried nothing.
//! - a session that reached it: not wasted, and the identity's run goes
//!   back to zero. The ledger remembers a RUN of waste, never a total —
//!   a peer that was unreachable this afternoon and is fine now is fine
//!   now.
//!
//! A link still standing has no outcome yet and is not recorded.
//!
//! # What it is measured to be worth, and where it is worth nothing
//!
//! `graph_formation`'s tables, per 1000 arrival orders, under the shipped
//! candidate order and at the defaults below. In the three-board room
//! with a phone in it and no other node, links standing: the room's dials
//! fall from 9556 to 7686 and the refused ones — a full connect spent to
//! be told the identity was already live — from 4746 to 2704, dials per
//! link that lasted from 2.04 to 1.59, and the board-to-board links
//! formed and the split-graph count do not move at all (2040 and 0, for
//! every `k` in {2, 3, 5} and at every lifetime). It refuses about one
//! solo window per order.
//!
//! At ten and twenty boards it is nearly inert, and that is the right
//! answer rather than a gap: a board in a room of ten has strict
//! candidates, so it rarely reaches a fallback dial at all, and the whole
//! thousand orders hold four refused dials. In a room of boards whose
//! links never end it cannot fire by construction — no dial ever has a
//! wasted outcome — so #375's own guarantee cell is bit-identical with
//! the ledger on.
//!
//! The one thing it does NOT do is move the dial to a better peer. In the
//! solo window there is no better peer; the slot stays free instead of
//! being spent, which is the only defence that window admits.

/// How many identities one board remembers at a time.
///
/// Sized like the firmware's address table (`DEAD_END_SLOTS`, twice the
/// link count): a board holds at most a handful of links, so eight
/// entries span more than one pause of them. Overflow drops the entry
/// whose count was written longest ago — the stalest information, and an
/// early re-dial rather than a loss, exactly as that table's overflow is.
pub const LEDGER_SLOTS: usize = 8;

/// When a run of wasted dials closes the fallback class, and for how
/// long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerPolicy {
    /// Wasted dials to ONE identity, in a row, before the board stops
    /// spending fallback dials. Zero keeps no ledger at all, which is
    /// what every measurement before #412 part 2 ran at.
    pub wasted_run: u32,
    /// The session length a dial has to have bought to count as useful.
    pub useful_session_ms: u64,
    /// How long the board then goes without a fallback dial, counted
    /// from the outcome that armed it. Every further wasted dial to a
    /// condemned identity re-arms it, so such a peer is reached for at
    /// most once per pause.
    pub pause_ms: u64,
}

impl LedgerPolicy {
    /// No ledger: nothing is remembered and no dial is ever held.
    pub const OFF: Self = Self {
        wasted_run: 0,
        useful_session_ms: 0,
        pause_ms: 0,
    };

    /// The run the tables are measured at. Two is enough to fire on a
    /// coincidence, five leaves most of the waste unspent; three is what
    /// the sweep in `graph_formation` settles on, and the column it is
    /// chosen by is that the board-to-board links and the split-graph
    /// count do not move at all at any of the three.
    pub const WASTED_RUN: u32 = 3;

    /// The session length below which a dial counts as wasted: one
    /// keepalive interval, [`crate::LINK_ABANDONED_MS`] being two of
    /// them. A link that did not outlive one keepalive never proved it
    /// was carrying anything. Deliberately the most generous bound
    /// available — a longer one counts more sessions as wasted, so this
    /// choice under-states the waste rather than manufacturing it, and
    /// the harness measures 30 s and 45 s as well: both hold more windows
    /// shut, and both start costing board-to-board links.
    pub const USEFUL_SESSION_MS: u64 = crate::LINK_ABANDONED_MS / 2;

    /// The pause, 120 s: the same period the firmware's address table
    /// already waits after a dial that bought nothing, keyed by identity
    /// instead of by address.
    ///
    /// One scan cycle (`SCAN_FALLBACK_AFTER_MS`, 30 s) cannot be the
    /// default, and the reason is structural rather than measured: a
    /// teardown resets the strict clock, so the board already spends one
    /// full scan cycle in strict mode before it can reach a fallback
    /// dial, and a pause of exactly that length expires in the round the
    /// fallback becomes available. The harness pins that inertness as a
    /// control of this constant.
    pub const PAUSE_MS: u64 = 120_000;

    /// The defaults every table in `graph_formation` is measured at.
    pub const MEASURED: Self = Self {
        wasted_run: Self::WASTED_RUN,
        useful_session_ms: Self::USEFUL_SESSION_MS,
        pause_ms: Self::PAUSE_MS,
    };

    /// Whether this policy keeps a ledger at all. When false nothing is
    /// recorded and nothing is held, which is what makes [`Self::OFF`]
    /// bit-identical to no ledger rather than merely equal in aggregate.
    #[must_use]
    pub const fn keeps(self) -> bool {
        self.wasted_run > 0
    }
}

/// One identity's run of wasted dials, and when it was last written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    identity: [u8; 16],
    /// Wasted dials in a row. Never zero: a run that ends takes the
    /// entry with it, so a slot holding an entry holds a run.
    run: u32,
    written_ms: u64,
}

/// What one recorded outcome did — the inputs of the `BLE_DIAL_LEDGER`
/// line a board logs, so a capture says why a dial was not made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Noted {
    /// Whether this outcome counted as wasted.
    pub wasted: bool,
    /// The identity's run of wasted dials after this outcome.
    pub run: u32,
    /// Whether it armed (or re-armed) the pause.
    pub armed: bool,
}

/// The per-board table: [`LEDGER_SLOTS`] identities and one pause.
#[derive(Debug, Clone, Copy)]
pub struct DialLedger {
    entries: [Option<Entry>; LEDGER_SLOTS],
    /// When the board may spend a fallback dial again. `None` is a board
    /// that has never had a run reach the bound.
    held_until_ms: Option<u64>,
}

impl Default for DialLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl DialLedger {
    /// An empty ledger: nothing remembered, nothing held.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [None; LEDGER_SLOTS],
            held_until_ms: None,
        }
    }

    /// Record one dial outcome against the identity the dial reached.
    ///
    /// `session_ms` is `None` for a dial refused post-connect — there was
    /// no session — and `Some(ms)` for one whose link has ended. A link
    /// still standing has no outcome and must not be recorded.
    pub fn note(
        &mut self,
        policy: LedgerPolicy,
        identity: &[u8; 16],
        session_ms: Option<u64>,
        now_ms: u64,
    ) -> Noted {
        if !policy.keeps() {
            return Noted {
                wasted: false,
                run: 0,
                armed: false,
            };
        }
        let wasted = session_ms.is_none_or(|ms| ms < policy.useful_session_ms);
        let run = self.write(identity, wasted, now_ms);
        let armed = run >= policy.wasted_run;
        if armed {
            self.held_until_ms = Some(now_ms + policy.pause_ms);
        }
        Noted { wasted, run, armed }
    }

    /// Whether the board may spend a fallback dial now. A strict verdict
    /// is never held: the address sort is #375's guarantee and this table
    /// has no business in it.
    #[must_use]
    pub fn fallback_held(&self, now_ms: u64) -> bool {
        self.held_until_ms.is_some_and(|until| now_ms < until)
    }

    /// The run this identity is on, for a log line or a test. Zero for an
    /// identity the table has never had to write down.
    #[must_use]
    pub fn run_of(&self, identity: &[u8; 16]) -> u32 {
        self.entry(identity).map_or(0, |e| e.run)
    }

    fn entry(&self, identity: &[u8; 16]) -> Option<&Entry> {
        self.entries
            .iter()
            .flatten()
            .find(|e| &e.identity == identity)
    }

    /// Extend or end this identity's run, and return the run it is on.
    /// A run that ends releases the slot: the table's whole content is
    /// runs in progress, so a forgotten zero and an absent entry are the
    /// same statement.
    fn write(&mut self, identity: &[u8; 16], wasted: bool, now_ms: u64) -> u32 {
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|slot| slot.is_some_and(|e| &e.identity == identity))
        {
            if !wasted {
                *slot = None;
                return 0;
            }
            let entry = slot.as_mut().expect("just matched a filled slot");
            entry.run += 1;
            entry.written_ms = now_ms;
            return entry.run;
        }
        if !wasted {
            return 0;
        }
        let fresh = Entry {
            identity: *identity,
            run: 1,
            written_ms: now_ms,
        };
        // A free slot first, then the entry written longest ago: its
        // count is the stalest information in the table.
        if let Some(slot) = self.entries.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(fresh);
        } else if let Some(stalest) = self
            .entries
            .iter_mut()
            .min_by_key(|slot| slot.map_or(0, |e| e.written_ms))
        {
            *stalest = Some(fresh);
        }
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: LedgerPolicy = LedgerPolicy::MEASURED;

    /// A distinct identity per byte, so a test reads as its own room.
    fn identity(tag: u8) -> [u8; 16] {
        [tag; 16]
    }

    /// The rule itself: a run of `wasted_run` wasted dials to one
    /// identity closes the fallback class, and one short of it does not.
    #[test]
    fn a_run_of_wasted_dials_closes_the_fallback_and_one_short_of_it_does_not() {
        let mut ledger = DialLedger::new();
        let phone = identity(1);
        for dial in 1..POLICY.wasted_run {
            let noted = ledger.note(POLICY, &phone, None, u64::from(dial) * 1000);
            assert!(noted.wasted && !noted.armed, "dial {dial} armed the pause");
            assert!(!ledger.fallback_held(u64::from(dial) * 1000));
        }
        let noted = ledger.note(POLICY, &phone, None, 10_000);
        assert_eq!(
            (noted.wasted, noted.run, noted.armed),
            (true, POLICY.wasted_run, true)
        );
        assert!(ledger.fallback_held(10_000));
        assert!(ledger.fallback_held(10_000 + POLICY.pause_ms - 1));
        assert!(
            !ledger.fallback_held(10_000 + POLICY.pause_ms),
            "the pause is a deadline, not a state"
        );
    }

    /// Why the key is the identity: a Columba was seen under five
    /// addresses in four minutes, and the firmware's address-keyed table
    /// makes every one of them a first offence. One identity behind all
    /// five is what lets the run reach the bound at all — and the control
    /// is the other direction, five DIFFERENT identities, where no run
    /// gets past one and nothing is held.
    #[test]
    fn one_identity_under_five_addresses_is_one_run_and_five_identities_are_five() {
        let mut rotating = DialLedger::new();
        let phone = identity(7);
        for rotation in 0..5 {
            rotating.note(POLICY, &phone, None, rotation * 48_000);
        }
        assert_eq!(rotating.run_of(&phone), 5);
        assert!(rotating.fallback_held(4 * 48_000));

        let mut strangers = DialLedger::new();
        for peer in 0..5u8 {
            let noted = strangers.note(POLICY, &identity(peer), None, u64::from(peer) * 48_000);
            assert_eq!(noted.run, 1, "peer {peer} inherited another peer's run");
            assert!(!noted.armed);
        }
        assert!(
            !strangers.fallback_held(4 * 48_000),
            "five peers that each wasted one dial are not one peer that wasted five"
        );
    }

    /// The ledger remembers a run, not a total: one session that lasted
    /// clears the identity, and the count starts again from there.
    #[test]
    fn a_session_that_lasted_clears_the_run() {
        let mut ledger = DialLedger::new();
        let peer = identity(2);
        ledger.note(POLICY, &peer, None, 0);
        ledger.note(POLICY, &peer, None, 1000);
        assert_eq!(ledger.run_of(&peer), 2);
        let noted = ledger.note(POLICY, &peer, Some(POLICY.useful_session_ms), 2000);
        assert_eq!((noted.wasted, noted.run, noted.armed), (false, 0, false));
        assert_eq!(ledger.run_of(&peer), 0);
        ledger.note(POLICY, &peer, None, 3000);
        ledger.note(POLICY, &peer, None, 4000);
        assert!(
            !ledger.fallback_held(4000),
            "the two wasted dials before the good session still count towards the run"
        );
    }

    /// The threshold is a session length and the boundary belongs to the
    /// useful side: exactly one keepalive interval proved the link was
    /// carrying something.
    #[test]
    fn the_threshold_belongs_to_the_useful_side() {
        let mut ledger = DialLedger::new();
        let peer = identity(3);
        assert!(
            !ledger
                .note(POLICY, &peer, Some(POLICY.useful_session_ms), 0)
                .wasted
        );
        assert!(
            ledger
                .note(POLICY, &peer, Some(POLICY.useful_session_ms - 1), 1000)
                .wasted
        );
    }

    /// A dial refused post-connect is wasted whatever the threshold says,
    /// because there was no session to measure: that is the outcome #412
    /// part 3 feeds this table with, and today it only reaches the
    /// address-keyed one.
    #[test]
    fn a_refused_dial_is_wasted_at_every_threshold() {
        let generous = LedgerPolicy {
            useful_session_ms: 0,
            ..POLICY
        };
        let mut ledger = DialLedger::new();
        let peer = identity(4);
        assert!(
            !ledger.note(generous, &peer, Some(0), 0).wasted,
            "a zero-length session is useful under a zero threshold, by arithmetic"
        );
        assert!(
            ledger.note(generous, &peer, None, 1000).wasted,
            "a refusal has no session length to compare against"
        );
    }

    /// After the pause the board tries once more, and a peer that wastes
    /// that dial too re-arms it: a condemned identity is reached for at
    /// most once per pause, never never again.
    #[test]
    fn the_pause_re_arms_on_the_next_wasted_dial() {
        let mut ledger = DialLedger::new();
        let phone = identity(5);
        for dial in 0..POLICY.wasted_run {
            ledger.note(POLICY, &phone, None, u64::from(dial));
        }
        let armed_at = u64::from(POLICY.wasted_run) - 1;
        assert!(!ledger.fallback_held(armed_at + POLICY.pause_ms));
        let noted = ledger.note(POLICY, &phone, None, armed_at + POLICY.pause_ms);
        assert!(
            noted.armed && noted.run == POLICY.wasted_run + 1,
            "the retry's outcome extends the run and arms the pause again"
        );
        assert!(ledger.fallback_held(armed_at + POLICY.pause_ms + 1));
    }

    /// The table is [`LEDGER_SLOTS`] deep and the overflow drops the
    /// entry written longest ago. The identity that falls out starts
    /// again from one, which is an early re-dial and not a loss.
    #[test]
    fn the_overflow_drops_the_entry_written_longest_ago() {
        let mut ledger = DialLedger::new();
        for peer in 0..LEDGER_SLOTS {
            let tag = u8::try_from(peer).expect("the table is smaller than a byte");
            ledger.note(POLICY, &identity(tag), None, u64::from(tag) * 1000);
        }
        let stalest = identity(0);
        assert_eq!(ledger.run_of(&stalest), 1);
        let newcomer = identity(200);
        ledger.note(POLICY, &newcomer, None, 100_000);
        assert_eq!(ledger.run_of(&newcomer), 1);
        assert_eq!(
            ledger.run_of(&stalest),
            0,
            "the overflow took an entry that was not the stalest one"
        );
        for peer in 1..LEDGER_SLOTS {
            let tag = u8::try_from(peer).expect("the table is smaller than a byte");
            assert_eq!(
                ledger.run_of(&identity(tag)),
                1,
                "the overflow took more than one entry"
            );
        }
    }

    /// The zero: a policy that keeps no ledger records nothing and holds
    /// nothing, however much waste it is shown.
    #[test]
    fn the_policy_that_keeps_no_ledger_holds_no_dial() {
        let mut ledger = DialLedger::new();
        let phone = identity(6);
        for dial in 0..20 {
            let noted = ledger.note(LedgerPolicy::OFF, &phone, None, dial * 1000);
            assert_eq!((noted.wasted, noted.run, noted.armed), (false, 0, false));
        }
        assert_eq!(ledger.run_of(&phone), 0);
        assert!(!ledger.fallback_held(0));
        assert!(!ledger.fallback_held(20_000));
    }
}
