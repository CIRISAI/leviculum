//! Whether the list says "no connection" (1448, walk finding 14). Without
//! a network the list showed only a grey dot on the own avatar and "looking
//! for messages…", and nobody saw that nothing went out. Now a bar says so
//! while the engine has no path to the mailbox: its watching link is down.
//!
//! The link is down for a moment at every start and after every network
//! change, before the rendezvous carries it again; a bar for that would
//! flash on each. So it shows only once the link stayed down
//! [`OFFLINE_AFTER_MS`], and goes the moment the link is up. Pure: the
//! session feeds it the link and the clock.

/// How long the link stays down before the list says so.
pub const OFFLINE_AFTER_MS: u64 = 5_000;

/// The watching link's downtime, as the bar sees it.
#[derive(Debug, Default)]
pub struct Offline {
    /// Since when the link is down; `None` while it is up or not looked at.
    down_since: Option<u64>,
    /// What the bar shows.
    shown: bool,
}

impl Offline {
    /// The link is `up` at `now_ms`. True when the bar changed, so the
    /// list is drawn again.
    pub fn look(&mut self, up: bool, now_ms: u64) -> bool {
        let shown = if up {
            self.down_since = None;
            false
        } else {
            let since = *self.down_since.get_or_insert(now_ms);
            now_ms.saturating_sub(since) >= OFFLINE_AFTER_MS
        };
        std::mem::replace(&mut self.shown, shown) != shown
    }

    /// Whether the bar shows: the link stayed down long enough.
    pub fn offline(&self) -> bool {
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start's moment without a link says nothing; a link that stays
    /// down says so once, and an up link takes it back at once.
    #[test]
    fn the_bar_shows_only_for_a_link_that_stays_down() {
        let mut bar = Offline::default();
        assert!(!bar.look(false, 1_000), "the start's moment");
        assert!(!bar.look(false, 1_000 + OFFLINE_AFTER_MS - 1));
        assert!(!bar.offline());
        assert!(bar.look(false, 1_000 + OFFLINE_AFTER_MS), "stayed down");
        assert!(bar.offline());
        assert!(!bar.look(false, 60_000), "said once");
        assert!(bar.look(true, 60_001), "up: gone at once");
        assert!(!bar.offline());
        // Down again: the wait starts again.
        assert!(!bar.look(false, 70_000));
        assert!(!bar.look(false, 70_000 + OFFLINE_AFTER_MS - 1));
        assert!(bar.look(false, 70_000 + OFFLINE_AFTER_MS));
    }
}
