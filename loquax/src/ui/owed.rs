//! What this device set for a peer and its home node has not taken yet
//! (1635 for petnames, 1640 for peer states): one [`Owed`] per kind of
//! word, the rules the same for both.
//!
//! A word the reader sets on a client of a home node is owed until the
//! home answers ok: the newest of each peer is kept in the device's store,
//! handed again after a restart, and goes one hand at a time so the home
//! takes a peer's words in the order they were set. The home stamps an
//! entry when it takes it and keeps only a peer's newest, which gives the
//! two rules for an entry a pull brings ([`Owed::pulled`]):
//!
//! - While owed, a pulled entry for the peer changes nothing, since the
//!   owed word will be the home's newest; an equal entry is the home's echo
//!   and settles the debt.
//! - Once taken, until the next pull ends ([`Owed::fetch_done`]), a
//!   different entry counts only if the home stamped it after the answer
//!   came: a pull under way when the home took the word may still bring an
//!   older one.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// The words of one kind this device owes its home node, and those the
/// home took until the next pull ends.
#[derive(Clone, Debug)]
pub struct Owed<V> {
    /// The newest word of each peer the home has not taken.
    owed: BTreeMap<[u8; 16], V>,
    /// A hand of [`Owed::take`] is under way: the next waits for its
    /// [`Owed::handed`].
    in_flight: bool,
    /// [`Self::owed`] changed since the store last kept it.
    changed: bool,
    /// The words the home took until the next pull ends, each with this
    /// device's clock when the home's answer came.
    taken: BTreeMap<[u8; 16], (V, i64)>,
}

impl<V> Default for Owed<V> {
    fn default() -> Self {
        Self {
            owed: BTreeMap::new(),
            in_flight: false,
            changed: false,
            taken: BTreeMap::new(),
        }
    }
}

impl<V: Clone + PartialEq> Owed<V> {
    /// The reader set `value` for `peer` here: owed until the home takes
    /// it, in place of what was owed for the peer.
    pub fn set(&mut self, peer: [u8; 16], value: V) {
        self.owed.insert(peer, value);
        self.changed = true;
    }

    /// The word owed for `peer`, if any.
    pub fn get(&self, peer: &[u8; 16]) -> Option<&V> {
        self.owed.get(peer)
    }

    /// Whether a pulled entry of `value` for `peer`, stamped by the home at
    /// `at`, is the reader's word here; the rules of the module's head.
    pub fn pulled(&mut self, peer: &[u8; 16], value: &V, at: i64) -> bool {
        if let Some(owed) = self.owed.get(peer) {
            if owed == value {
                self.owed.remove(peer);
                self.changed = true;
            }
            return false;
        }
        if let Some((taken, taken_at)) = self.taken.get(peer) {
            if taken == value || at <= *taken_at {
                return false;
            }
            self.taken.remove(peer);
        }
        true
    }

    /// What is owed, the newest of each peer, for a hand; nothing while a
    /// hand is under way, since two at once could reach the home the other
    /// way round.
    pub fn take(&mut self) -> Vec<([u8; 16], V)> {
        if self.in_flight || self.owed.is_empty() {
            return Vec::new();
        }
        self.in_flight = true;
        self.owed
            .iter()
            .map(|(peer, value)| (*peer, value.clone()))
            .collect()
    }

    /// A hand ended, the home's answer at `now` on this device's clock:
    /// what it took is owed no more, unless the reader set another word
    /// for the peer meanwhile, and stands over older entries until the next
    /// pull ends. What it did not take goes with the next hand.
    pub fn handed(&mut self, taken: Vec<([u8; 16], V)>, now: i64) {
        self.in_flight = false;
        for (peer, value) in taken {
            if self.owed.get(&peer) == Some(&value) {
                self.owed.remove(&peer);
                self.changed = true;
                self.taken.insert(peer, (value, now));
            }
        }
    }

    /// A pull ended: the next began after every word the home took so far
    /// and brings nothing older than them.
    pub fn fetch_done(&mut self) {
        self.taken.clear();
    }

    /// What the store kept as owed, at the start: handed again with the
    /// first hand, and the reader's word over what the first pull brings.
    pub fn restore(&mut self, owed: BTreeMap<[u8; 16], V>) {
        *self = Self {
            owed,
            ..Self::default()
        };
    }

    /// The owed words whole, when they changed since the last call, for
    /// the store to keep in place of what it kept.
    pub fn take_changed(&mut self) -> Option<BTreeMap<[u8; 16], V>> {
        core::mem::take(&mut self.changed).then(|| self.owed.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const PEER: [u8; 16] = [7; 16];

    /// The rules on a bare value: owed stands over every pull and its echo
    /// settles it; taken stands over an older stamp until a pull ends; one
    /// hand at a time; a word set during a hand stays owed.
    #[test]
    fn owed_and_taken_follow_the_home_stamp() {
        let mut owed = Owed::<u8>::default();
        owed.set(PEER, 1);
        assert_eq!(owed.take(), [(PEER, 1)]);
        owed.set(PEER, 2);
        assert!(owed.take().is_empty(), "a hand is under way");
        owed.handed(vec![(PEER, 1)], 10);
        assert_eq!(owed.get(&PEER), Some(&2), "set during the hand");
        assert!(!owed.pulled(&PEER, &1, 99));
        assert!(!owed.pulled(&PEER, &2, 0), "the echo");
        assert_eq!(owed.get(&PEER), None);
        assert_eq!(owed.take_changed(), Some(BTreeMap::new()));
        assert_eq!(owed.take_changed(), None);

        owed.set(PEER, 3);
        assert_eq!(owed.take(), [(PEER, 3)]);
        owed.handed(vec![(PEER, 3)], 10);
        assert!(!owed.pulled(&PEER, &1, 10), "stamped before the answer");
        assert!(owed.pulled(&PEER, &4, 11), "stamped after it");
        owed.set(PEER, 5);
        owed.take();
        owed.handed(vec![(PEER, 5)], 20);
        owed.fetch_done();
        assert!(
            owed.pulled(&PEER, &1, 0),
            "a later pull brings nothing older"
        );

        owed.restore(BTreeMap::from([(PEER, 6)]));
        assert_eq!(owed.take(), [(PEER, 6)]);
        assert_eq!(owed.take_changed(), None);
    }
}
