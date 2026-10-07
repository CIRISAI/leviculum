//! What a board remembers about the peers that keep LEAVING (#417):
//! per PEER IDENTITY, how many times a session with it ended by the
//! peer's own departure inside one window, and whether the board may
//! spend a fallback dial on a rotating-addressed candidate right now.
//!
//! # The dial the dial ledger cannot refuse
//!
//! [`crate::dial_ledger`] remembers a run of WASTED dials — refused
//! post-connect, or a session that never outlived one keepalive — and a
//! session that lasted clears the run. A phone that stays linked until
//! its next address rotation buys the board a session of a minute or
//! more, so every one of those dials counts as useful there, and the
//! run never reaches its bound. What #412's capture shows is the next
//! round of the same waste: `feld-t114` spent 7 outgoing links in two
//! days on one identity, each one a full dial, each one a session the
//! dial ledger rightly calls useful, each one gone at the peer's next
//! rotation — and the board's ONE central slot (`CENTRAL_LINKS = 1`,
//! `columba.rs`) paid for all of them. The waste is not in any single
//! dial; it is in the repetition, and only a memory of DEPARTURES can
//! see repetition.
//!
//! # What counts as a leave
//!
//! A session with a ROTATING-addressed peer that ended by the peer's
//! own doing: it went silent until the registry's expiry swept the
//! link (`BLE_LINK_EXPIRE`, the rotation's signature), or a newer
//! connection of the same identity displaced it (`BLE_LINK_REPLACED` —
//! the peer left its old address even though it came straight back).
//! The role does not matter: an incoming session the peer paid for
//! says exactly as much about its habits as one we dialled, and on a
//! board beside a phone the incoming ones are most of the evidence —
//! the board learns the peer churns without spending a dial on it.
//!
//! What does NOT count, and why: a session ended by OUR side (carrier
//! switch, displacement we initiated), a supervision timeout or range
//! loss (a board that walks away wears a static address, and its
//! address is [`crate::dial_ledger`]'s and the dead-end table's
//! business), and any session with a fixed-addressed peer, static or
//! public. The filter is [`ChurnLedger::note_departure`]'s, or the
//! caller's before [`ChurnLedger::note_leave`]: this table is fed leaves
//! of rotating-addressed sessions only, so that a flaky board can never
//! arm a pause that is paid by every phone in the room.
//!
//! # Why the key is an identity and the effect is a CLASS
//!
//! The same fact as the dial ledger's: the identity behind an
//! advertisement is unknowable before connecting, so a table keyed by
//! identity cannot be consulted per candidate. What it can decide is
//! whether the board spends a fallback dial on the class the leaver
//! belongs to — the candidates whose address their owner redraws
//! ([`crate::window::rotating_address`], the same class test the
//! fallback order already uses). While the cooldown stands the board
//! still dials every strict verdict and every FIXED-addressed fallback
//! candidate; only the rotating ones wait. That scope is measured, not
//! assumed: holding the whole fallback class instead costs the
//! ten-board room board-to-board links (`graph_formation.rs`, the #417
//! design table), because a board whose only strict candidates are
//! below it re-links to boards through fallback dials, and those
//! dials' targets are fixed-addressed.
//!
//! The counter is per identity all the same, because the trigger must
//! be: three different phones that each left once are three first
//! offences, not one condemned class.
//!
//! # The window, and the retry the restart grants
//!
//! A leave run is `leave_run` departures of ONE identity inside
//! `window_ms` of the run's first. The window is what keeps an
//! occasional leaver innocent: a phone that drops once an hour never
//! accumulates a run, while the measured phone — five addresses in
//! four minutes (#412) — reaches three leaves inside any window longer
//! than two of its rotations. A run whose window has passed starts
//! over at one, which under continuous churn re-opens the class for
//! one dial roughly once per window: the deliberate retry, same
//! philosophy as the dial ledger's once-per-pause — a peer that has
//! stopped churning is re-learned, never remembered forever.
//!
//! Reference: Python-RNS has no BLE interface; the reference for this
//! rule is our own measured field behaviour (#412's captures), and the
//! wire/semantics deviation rule does not apply — no wire format and
//! no Python peer is involved.

use crate::admission::PeerAddr;
use crate::dial_ledger::LEDGER_SLOTS;

/// When a run of leaves closes the rotating fallback class, and for
/// how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChurnPolicy {
    /// Departures by ONE identity inside [`Self::window_ms`] before
    /// the board stops spending fallback dials on rotating-addressed
    /// candidates. Zero keeps no ledger at all.
    pub leave_run: u32,
    /// How long a run has to fit: counted from the run's FIRST leave,
    /// and a run that outlives it starts over at one.
    pub window_ms: u64,
    /// How long the rotating fallback class then waits, counted from
    /// the leave that armed it. Every further leave inside the window
    /// re-arms it.
    pub cooldown_ms: u64,
}

impl ChurnPolicy {
    /// No ledger: nothing is recorded and no dial is ever held.
    pub const OFF: Self = Self {
        leave_run: 0,
        window_ms: 0,
        cooldown_ms: 0,
    };

    /// The run the tables are measured at — three, for the dial
    /// ledger's own reasons (two fires on a coincidence, five leaves
    /// most of the waste unspent), and the `graph_formation` sweep
    /// confirms the choice the same way: the board-to-board links and
    /// the split-graph count do not move at any of the three.
    pub const LEAVE_RUN: u32 = 3;

    /// The window, 300 s. It has to hold [`Self::LEAVE_RUN`] departures
    /// of the measured phone — a rotation every 48 s (#412's five
    /// addresses in four minutes), so three leaves span about two
    /// rotations, under 150 s — with room for the slower "new address
    /// every few minutes" cadence an operator's phone may run at. And
    /// it has to be short enough that departures which CANNOT be
    /// rotation churn (minutes of session between them) never
    /// accumulate: at 300 s a peer must leave better than once per
    /// 150 s to be condemned.
    pub const WINDOW_MS: u64 = 300_000;

    /// The cooldown, 120 s: the same period the firmware's address
    /// table waits after a dial that bought nothing (`DEAD_END_TTL`),
    /// and the dial ledger's own pause — one bound, three tables.
    pub const COOLDOWN_MS: u64 = 120_000;

    /// The defaults every table in `graph_formation` is measured at.
    pub const MEASURED: Self = Self {
        leave_run: Self::LEAVE_RUN,
        window_ms: Self::WINDOW_MS,
        cooldown_ms: Self::COOLDOWN_MS,
    };

    /// Whether this policy keeps a ledger at all. When false nothing
    /// is recorded and nothing is held, which is what makes
    /// [`Self::OFF`] bit-identical to no ledger rather than merely
    /// equal in aggregate.
    #[must_use]
    pub const fn keeps(self) -> bool {
        self.leave_run > 0
    }
}

/// One identity's run of leaves: when it started, how long it is, and
/// when it was last written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    identity: [u8; 16],
    /// The run's first leave — the window is measured from here.
    first_ms: u64,
    /// Leaves since [`Self::first_ms`]. Never zero: an entry exists
    /// because a leave was recorded.
    run: u32,
    written_ms: u64,
}

/// What one recorded leave did — the inputs of the `BLE_CHURN_LEDGER`
/// line a board logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaveNoted {
    /// The identity's run after this leave (one when the window had
    /// passed and the run started over).
    pub run: u32,
    /// Whether it armed (or re-armed) the cooldown.
    pub armed: bool,
}

/// The per-board table: [`LEDGER_SLOTS`] identities and one cooldown.
#[derive(Debug, Clone, Copy)]
pub struct ChurnLedger {
    entries: [Option<Entry>; LEDGER_SLOTS],
    /// When the board may spend a rotating fallback dial again. `None`
    /// is a board that has never had a run reach the bound.
    held_until_ms: Option<u64>,
}

impl Default for ChurnLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl ChurnLedger {
    /// An empty ledger: nothing remembered, nothing held.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [None; LEDGER_SLOTS],
            held_until_ms: None,
        }
    }

    /// Record one departure of a ROTATING-addressed peer, against the
    /// identity behind whatever address it left on. The caller filters
    /// (see the module docs): a static-addressed session's end must
    /// not reach this table.
    pub fn note_leave(
        &mut self,
        policy: ChurnPolicy,
        identity: &[u8; 16],
        now_ms: u64,
    ) -> LeaveNoted {
        if !policy.keeps() {
            return LeaveNoted {
                run: 0,
                armed: false,
            };
        }
        let run = self.write(policy, identity, now_ms);
        let armed = run >= policy.leave_run;
        if armed {
            self.held_until_ms = Some(now_ms + policy.cooldown_ms);
        }
        LeaveNoted { run, armed }
    }

    /// Record one departure of `peer`'s identity iff `peer` rotates —
    /// the class filter the module docs give the caller, in one place
    /// for the firmware (#506). `None` for a fixed address, public or
    /// static random, which records nothing.
    pub fn note_departure(
        &mut self,
        policy: ChurnPolicy,
        peer: PeerAddr,
        identity: &[u8; 16],
        now_ms: u64,
    ) -> Option<LeaveNoted> {
        peer.rotating()
            .then(|| self.note_leave(policy, identity, now_ms))
    }

    /// Whether a fallback dial to a ROTATING-addressed candidate is
    /// held right now. A strict verdict is never held, and neither is
    /// a fallback candidate whose address is a key — the class is the
    /// scope the design table chose, and the module docs say why.
    #[must_use]
    pub fn rotating_fallback_held(&self, now_ms: u64) -> bool {
        self.held_until_ms.is_some_and(|until| now_ms < until)
    }

    /// The deadline [`Self::rotating_fallback_held`] compares against,
    /// for the log line. `None` is a board that has never had a run
    /// reach the bound; an expired deadline is reported rather than
    /// cleared, for the same capture-reading reason the dial ledger's
    /// is.
    #[must_use]
    pub const fn held_until_ms(&self) -> Option<u64> {
        self.held_until_ms
    }

    /// The run this identity is on, for a log line or a test. Zero for
    /// an identity the table holds nothing about.
    #[must_use]
    pub fn run_of(&self, identity: &[u8; 16]) -> u32 {
        self.entries
            .iter()
            .flatten()
            .find(|e| &e.identity == identity)
            .map_or(0, |e| e.run)
    }

    /// Extend this identity's run — or start it over when the window
    /// has passed — and return the run it is on.
    fn write(&mut self, policy: ChurnPolicy, identity: &[u8; 16], now_ms: u64) -> u32 {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .flatten()
            .find(|e| &e.identity == identity)
        {
            if now_ms.saturating_sub(entry.first_ms) > policy.window_ms {
                entry.first_ms = now_ms;
                entry.run = 1;
            } else {
                entry.run += 1;
            }
            entry.written_ms = now_ms;
            return entry.run;
        }
        let fresh = Entry {
            identity: *identity,
            first_ms: now_ms,
            run: 1,
            written_ms: now_ms,
        };
        // A free slot first, then the entry written longest ago: the
        // stalest information, and an early restart rather than a
        // loss — exactly the dial ledger's overflow.
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

    const POLICY: ChurnPolicy = ChurnPolicy::MEASURED;

    /// The measured phone's cadence: a new address — and with it one
    /// departure per linked board — every 48 s (#412).
    const ROTATE_MS: u64 = 48_000;

    fn identity(tag: u8) -> [u8; 16] {
        [tag; 16]
    }

    /// The rule itself: a run of `leave_run` departures inside the
    /// window closes the rotating fallback class, and one short of it
    /// does not.
    #[test]
    fn a_run_of_leaves_inside_the_window_arms_and_one_short_does_not() {
        let mut ledger = ChurnLedger::new();
        let phone = identity(1);
        for leave in 1..POLICY.leave_run {
            let noted = ledger.note_leave(POLICY, &phone, u64::from(leave) * ROTATE_MS);
            assert!(!noted.armed, "leave {leave} armed the cooldown early");
            assert!(!ledger.rotating_fallback_held(u64::from(leave) * ROTATE_MS));
        }
        let at = u64::from(POLICY.leave_run) * ROTATE_MS;
        assert!(
            at <= ROTATE_MS + POLICY.window_ms,
            "the fixture fits the window"
        );
        let noted = ledger.note_leave(POLICY, &phone, at);
        assert_eq!((noted.run, noted.armed), (POLICY.leave_run, true));
        assert!(ledger.rotating_fallback_held(at));
        assert!(ledger.rotating_fallback_held(at + POLICY.cooldown_ms - 1));
        assert!(
            !ledger.rotating_fallback_held(at + POLICY.cooldown_ms),
            "the cooldown is a deadline, not a state"
        );
    }

    /// #506: the address #504 caught (`00:1A:7D:DA:71:13`, Public)
    /// leaves as often as the measured phone and arms nothing, because
    /// its type says it is a key; the same value read as random would.
    #[test]
    fn a_public_address_leaving_arms_no_churn_cooldown() {
        use crate::admission::AddrType;
        const PUBLIC_504: u64 = 0x001A_7DDA_7113;
        let host = identity(5);
        let mut ledger = ChurnLedger::new();
        for leave in 1..=POLICY.leave_run {
            let at = u64::from(leave) * ROTATE_MS;
            let public = PeerAddr {
                value: PUBLIC_504,
                addr_type: AddrType::Public,
            };
            assert_eq!(ledger.note_departure(POLICY, public, &host, at), None);
            assert!(!ledger.rotating_fallback_held(at));
        }
        assert_eq!(ledger.run_of(&host), 0);
        // The control: a resolvable private address with the same
        // cadence does arm, so the departures above were enough to.
        let mut control = ChurnLedger::new();
        let phone = PeerAddr {
            value: 0x4A1B_2C3D_4E5F,
            addr_type: AddrType::RandomPrivateResolvable,
        };
        let mut armed = false;
        for leave in 1..=POLICY.leave_run {
            let at = u64::from(leave) * ROTATE_MS;
            armed = control
                .note_departure(POLICY, phone, &host, at)
                .is_some_and(|noted| noted.armed);
        }
        assert!(armed);
    }

    /// The window is what keeps an occasional leaver innocent: the
    /// same number of departures an hour apart never accumulates a
    /// run, however many of them there are.
    #[test]
    fn departures_slower_than_the_window_never_arm() {
        let mut ledger = ChurnLedger::new();
        let phone = identity(2);
        for leave in 0..20u64 {
            let noted = ledger.note_leave(POLICY, &phone, leave * 3_600_000);
            assert_eq!(
                (noted.run, noted.armed),
                (1, false),
                "leave {leave}: an hour apart, every run starts over"
            );
        }
        assert!(!ledger.rotating_fallback_held(20 * 3_600_000));
    }

    /// Why the key is the identity: five phones that each left once
    /// are five first offences, not one condemned class — and the one
    /// phone that left five times is.
    #[test]
    fn one_identity_leaving_is_a_run_and_five_identities_are_five() {
        let mut churning = ChurnLedger::new();
        let phone = identity(7);
        for rotation in 0..5 {
            churning.note_leave(POLICY, &phone, rotation * ROTATE_MS);
        }
        assert_eq!(churning.run_of(&phone), 5);
        assert!(churning.rotating_fallback_held(4 * ROTATE_MS));

        let mut strangers = ChurnLedger::new();
        for peer in 0..5u8 {
            let noted = strangers.note_leave(POLICY, &identity(peer), u64::from(peer) * ROTATE_MS);
            assert_eq!(noted.run, 1, "peer {peer} inherited another peer's run");
            assert!(!noted.armed);
        }
        assert!(!strangers.rotating_fallback_held(4 * ROTATE_MS));
    }

    /// A run whose window has passed starts over at one — which under
    /// continuous churn re-opens the class for one dial roughly once
    /// per window, the deliberate retry the module docs state.
    #[test]
    fn the_window_restart_grants_the_retry_and_churn_re_arms_it() {
        let mut ledger = ChurnLedger::new();
        let phone = identity(3);
        let mut now = 0u64;
        // Continuous rotation: the run grows past the bound inside the
        // window…
        while now <= POLICY.window_ms {
            ledger.note_leave(POLICY, &phone, now);
            now += ROTATE_MS;
        }
        let run_before = ledger.run_of(&phone);
        assert!(run_before > POLICY.leave_run);
        // …and the first leave past the window starts a fresh run.
        let noted = ledger.note_leave(POLICY, &phone, now);
        assert_eq!((noted.run, noted.armed), (1, false));
        // The cooldown armed by the LAST in-window leave still stands:
        // the retry opens when it lapses, not at the restart.
        assert!(ledger.rotating_fallback_held(now));
        // Two more rotations and the fresh run reaches the bound again.
        ledger.note_leave(POLICY, &phone, now + ROTATE_MS);
        let noted = ledger.note_leave(POLICY, &phone, now + 2 * ROTATE_MS);
        assert!(noted.armed);
        assert!(ledger.rotating_fallback_held(now + 2 * ROTATE_MS + POLICY.cooldown_ms - 1));
    }

    /// Every further leave inside the window re-arms the cooldown, so
    /// the hold outlasts any single deadline while the churn lasts.
    #[test]
    fn further_leaves_re_arm_the_cooldown() {
        let mut ledger = ChurnLedger::new();
        let phone = identity(4);
        for leave in 0..POLICY.leave_run {
            ledger.note_leave(POLICY, &phone, u64::from(leave) * ROTATE_MS);
        }
        let armed_at = u64::from(POLICY.leave_run - 1) * ROTATE_MS;
        assert_eq!(ledger.held_until_ms(), Some(armed_at + POLICY.cooldown_ms));
        let next = armed_at + ROTATE_MS;
        let noted = ledger.note_leave(POLICY, &phone, next);
        assert!(noted.armed && noted.run == POLICY.leave_run + 1);
        assert_eq!(
            ledger.held_until_ms(),
            Some(next + POLICY.cooldown_ms),
            "the deadline moved with the leave"
        );
    }

    /// The table is [`LEDGER_SLOTS`] deep and the overflow drops the
    /// entry written longest ago; the identity that falls out starts
    /// again from one, an early restart and not a loss.
    #[test]
    fn the_overflow_drops_the_entry_written_longest_ago() {
        let mut ledger = ChurnLedger::new();
        for peer in 0..LEDGER_SLOTS {
            let tag = u8::try_from(peer).expect("the table is smaller than a byte");
            ledger.note_leave(POLICY, &identity(tag), u64::from(tag) * 1000);
        }
        let stalest = identity(0);
        assert_eq!(ledger.run_of(&stalest), 1);
        let newcomer = identity(200);
        ledger.note_leave(POLICY, &newcomer, 100_000);
        assert_eq!(ledger.run_of(&newcomer), 1);
        assert_eq!(ledger.run_of(&stalest), 0, "a non-stalest entry was taken");
        for peer in 1..LEDGER_SLOTS {
            let tag = u8::try_from(peer).expect("the table is smaller than a byte");
            assert_eq!(ledger.run_of(&identity(tag)), 1);
        }
    }

    /// The deadline the `BLE_CHURN_LEDGER held` line prints is the one
    /// the rule compares against, and it outlives its own expiry.
    #[test]
    fn the_held_deadline_is_the_one_the_rule_compares_against() {
        let mut ledger = ChurnLedger::new();
        assert_eq!(ledger.held_until_ms(), None, "nothing armed it yet");
        let phone = identity(8);
        for leave in 0..POLICY.leave_run {
            ledger.note_leave(POLICY, &phone, u64::from(leave) * 1000);
        }
        let until = u64::from(POLICY.leave_run - 1) * 1000 + POLICY.cooldown_ms;
        assert_eq!(ledger.held_until_ms(), Some(until));
        assert!(ledger.rotating_fallback_held(until - 1));
        assert!(!ledger.rotating_fallback_held(until));
        assert_eq!(
            ledger.held_until_ms(),
            Some(until),
            "an expired cooldown still says when it ended"
        );
    }

    /// The zero: a policy that keeps no ledger records nothing and
    /// holds nothing, however much churn it is shown.
    #[test]
    fn the_policy_that_keeps_no_ledger_holds_no_dial() {
        let mut ledger = ChurnLedger::new();
        let phone = identity(6);
        for leave in 0..20 {
            let noted = ledger.note_leave(ChurnPolicy::OFF, &phone, leave * 1000);
            assert_eq!((noted.run, noted.armed), (0, false));
        }
        assert_eq!(ledger.run_of(&phone), 0);
        assert!(!ledger.rotating_fallback_held(0));
        assert!(!ledger.rotating_fallback_held(20_000));
    }
}
