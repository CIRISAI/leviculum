//! The home node's side of the wake (`docs/src/concepts/loquax-home-node.md`,
//! "The wake"): which links asked `/watch`, the archive's newest sequence,
//! and which link is owed a wake now. Pure: the daemon (`super::daemon`)
//! feeds it the links, the appends and the clock, and carries out what
//! [`Wakes::due`] names.
//!
//! A wake is sent at once for the first append after a quiet spell, and a
//! burst behind it is folded into one more wake [`COALESCE_MS`] later, so a
//! collection that brings fifty messages costs every client two packets,
//! not fifty.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// The least time between two wakes on one link.
pub const COALESCE_MS: u64 = 500;

/// One link that asked `/watch`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Watcher {
    /// The device the link was identified as: checked again before each
    /// wake, so a revoked device hears nothing more.
    device: [u8; 16],
    /// The newest sequence this link was told, by the `/watch` answer or
    /// the last wake.
    told: u64,
    /// When the last wake went, `None` before the first.
    at_ms: Option<u64>,
}

/// One wake to send: on `link`, of `device`, the newest sequence `newest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Due<K> {
    pub link: K,
    pub device: [u8; 16],
    pub newest: u64,
    /// What the link was told before this wake, for [`Wakes::unsent`].
    pub before: u64,
}

/// The watching links and what each was told. `K` is the link's id.
#[derive(Debug, Clone)]
pub struct Wakes<K> {
    watchers: BTreeMap<K, Watcher>,
    newest: u64,
}

impl<K> Default for Wakes<K> {
    fn default() -> Self {
        Self {
            watchers: BTreeMap::new(),
            newest: 0,
        }
    }
}

impl<K: Ord + Copy> Wakes<K> {
    /// Watching links held and their estimated heap bytes, for the
    /// mailbox's census (order 1585).
    pub fn census(&self) -> (usize, usize) {
        (
            self.watchers.len(),
            leviculum_core::heap_census::btree_map_bytes(&self.watchers),
        )
    }

    /// `link`, identified as `device`, asked `/watch` and was answered with
    /// `told`, the newest sequence then. The same link asking again
    /// replaces what it had.
    pub fn watch(&mut self, link: K, device: [u8; 16], told: u64) {
        self.newest = self.newest.max(told);
        self.watchers.insert(
            link,
            Watcher {
                device,
                told,
                at_ms: None,
            },
        );
    }

    /// The archive's newest sequence is now `seq`. A smaller one than
    /// known changes nothing: sequences only grow.
    pub fn appended(&mut self, seq: u64) {
        self.newest = self.newest.max(seq);
    }

    /// The link closed, or its device is no longer admitted.
    pub fn forget(&mut self, link: &K) {
        self.watchers.remove(link);
    }

    /// The wake on `link` did not go (the link was busy): it is owed again
    /// on the next [`Wakes::due`] past its spacing.
    pub fn unsent(&mut self, link: &K, told_before: u64) {
        if let Some(watcher) = self.watchers.get_mut(link) {
            watcher.told = watcher.told.min(told_before);
        }
    }

    /// How many links watch.
    pub fn len(&self) -> usize {
        self.watchers.len()
    }

    /// Whether no link watches.
    pub fn is_empty(&self) -> bool {
        self.watchers.is_empty()
    }

    /// The wakes owed at `now_ms`: every link told less than the newest
    /// sequence whose last wake is [`COALESCE_MS`] old or older. Each is
    /// counted as sent; one that did not go is handed back with
    /// [`Wakes::unsent`].
    pub fn due(&mut self, now_ms: u64) -> Vec<Due<K>> {
        let newest = self.newest;
        let mut due = Vec::new();
        for (link, watcher) in &mut self.watchers {
            if watcher.told >= newest {
                continue;
            }
            if watcher
                .at_ms
                .is_some_and(|at| now_ms.saturating_sub(at) < COALESCE_MS)
            {
                continue;
            }
            due.push(Due {
                link: *link,
                device: watcher.device,
                newest,
                before: watcher.told,
            });
            watcher.told = newest;
            watcher.at_ms = Some(now_ms);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE: [u8; 16] = [1; 16];
    const LAPTOP: [u8; 16] = [2; 16];

    fn links(due: &[Due<u8>]) -> Vec<(u8, u64)> {
        due.iter().map(|due| (due.link, due.newest)).collect()
    }

    #[test]
    fn the_first_append_wakes_at_once_and_a_burst_is_one_more_wake_later() {
        let mut wakes = Wakes::default();
        wakes.watch(7u8, PHONE, 10);
        assert!(wakes.due(0).is_empty(), "told 10, nothing newer");
        wakes.appended(11);
        assert_eq!(links(&wakes.due(1_000)), [(7, 11)]);
        // A burst right behind it: held for the spacing, then one wake with
        // the newest of the burst.
        for seq in 12..=20 {
            wakes.appended(seq);
            assert!(wakes.due(1_000 + seq).is_empty(), "seq {seq}");
        }
        assert!(wakes.due(1_000 + COALESCE_MS - 1).is_empty());
        assert_eq!(links(&wakes.due(1_000 + COALESCE_MS)), [(7, 20)]);
        // Quiet again: nothing owed, and the next append goes at once.
        assert!(wakes.due(5_000).is_empty());
        wakes.appended(21);
        assert_eq!(links(&wakes.due(5_001)), [(7, 21)]);
    }

    #[test]
    fn each_link_is_spaced_on_its_own_and_told_only_what_it_lacks() {
        let mut wakes = Wakes::default();
        wakes.watch(1u8, PHONE, 5);
        wakes.appended(6);
        assert_eq!(links(&wakes.due(100)), [(1, 6)]);
        // The laptop comes in later, already told 6 by its answer.
        wakes.watch(2u8, LAPTOP, 6);
        wakes.appended(7);
        // The phone was woken 50 ms ago; the laptop never.
        assert_eq!(links(&wakes.due(150)), [(2, 7)]);
        assert_eq!(links(&wakes.due(600)), [(1, 7)]);
        assert!(wakes.due(10_000).is_empty());
        assert_eq!(wakes.len(), 2);
    }

    #[test]
    fn a_sequence_never_goes_back_and_a_forgotten_link_hears_nothing() {
        let mut wakes = Wakes::default();
        wakes.watch(1u8, PHONE, 9);
        wakes.appended(3);
        assert!(wakes.due(0).is_empty());
        wakes.appended(10);
        wakes.forget(&1);
        assert!(wakes.due(0).is_empty());
        assert!(wakes.is_empty());
    }

    #[test]
    fn a_wake_that_did_not_go_is_owed_again_after_the_spacing() {
        let mut wakes = Wakes::default();
        wakes.watch(1u8, PHONE, 0);
        wakes.appended(4);
        let due = wakes.due(0);
        assert_eq!(links(&due), [(1, 4)]);
        wakes.unsent(&1, due[0].before);
        assert!(wakes.due(10).is_empty(), "spaced");
        assert_eq!(links(&wakes.due(COALESCE_MS)), [(1, 4)]);
    }
}
