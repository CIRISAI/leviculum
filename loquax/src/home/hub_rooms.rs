//! The rooms of the hubs a mailbox hears, asked for its devices (1643,
//! item 6 of the mailbox split; 1652 built the question).
//!
//! A device of an external mailbox hears no mesh of its own, so its
//! server sheet had nothing to list until its own `/list` came back over
//! a link it first had to build. The mailbox asks each hub it hears with
//! [`crate::rooms_link`]'s question ([`crate::rooms_link::Ask::Peek`]: a
//! link of its own, a throwaway identity and no nick, so the hub learns
//! nothing of the owner, HELLO, `/list`, the notices read), at most once
//! per [`LIST_EVERY_SECS`] a hub, and keeps what the hub named with the
//! hub's archive entry ([`crate::home::wire::hub_entry_rooms`]). The
//! device's pull brings it with the hub, and the sheet lists the rooms the
//! moment it opens; the device's own `/list` after it connects stays the
//! fresh copy ([`crate::ui::rooms::Rooms::mailbox_named`]).
//!
//! A hub heard is asked when its last question is [`LIST_EVERY_SECS`]
//! old; a hub that did not answer is asked again only then, too. The
//! questions run one at a time on the link task, which queues the rest.
//! The daemon starts the lister with the first hub it hears, and not at
//! all on a phone whose mailbox lends the Rooms' slot to its own screen
//! (`daemon::Lends`): a second lend to the Rooms ends the first, and that
//! screen's sheet asks the hub over the same node anyway.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Notify};

use crate::rooms_link::{self, Ask, Linking, Told};
use crate::store::Store;

/// How often a hub is asked for its rooms: as often as its entry is
/// refreshed ([`crate::home::HUB_REFRESH_SECS`]).
pub const LIST_EVERY_SECS: i64 = crate::home::HUB_REFRESH_SECS;

/// The most hubs whose last question is remembered: the hubs the archive
/// keeps. Past it the oldest question is forgotten, and that hub is asked
/// again the next time it is heard.
const ASKED_BOUND: usize = crate::ui::hubs::HEARD_BOUND;

/// What one answer did, for the log and the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answered {
    /// The hub named these rooms; `seq` is the hub entry appended with
    /// them, `None` when they are the kept ones.
    Rooms {
        hub: [u8; 16],
        rooms: Vec<String>,
        seq: Option<u64>,
    },
    /// The hub gave no WELCOME in the question's budget.
    NoAnswer { hub: [u8; 16] },
}

/// The mailbox's questions to the hubs it hears.
pub struct Lister {
    linking: Linking,
    /// When each hub was last asked, unix seconds.
    asked: HashMap<[u8; 16], i64>,
}

impl Lister {
    /// The questions over the mailbox's own node, lent to the Rooms
    /// ([`crate::engine::Lender::Rooms`]): the mailbox opens no hub link
    /// of its own, so the Rooms' slot is free.
    pub fn start(attached: &crate::engine::Attached) -> Self {
        let (lend, lends) = mpsc::unbounded_channel();
        let _ = lend.send(attached.lend_to(crate::engine::Lender::Rooms));
        Self::over(lends)
    }

    /// The questions over whatever node `lends` lends (the tests' own).
    pub fn over(lends: mpsc::UnboundedReceiver<crate::engine::Lent>) -> Self {
        let linking = rooms_link::start(
            &tokio::runtime::Handle::current(),
            rooms_link::Options {
                lends,
                // The rooms' own link is never opened here; every question
                // makes an identity of its own.
                identity: leviculum_std::generate_identity(),
                nick: None,
            },
            // The daemon reads the answers on its own pump.
            Arc::new(Notify::new()),
        );
        Self {
            linking,
            asked: HashMap::new(),
        }
    }

    /// The questions' table in heap bytes, for the mailbox's census
    /// (`hub_rooms`, [`super::census`]).
    pub fn heap_bytes(&self) -> usize {
        leviculum_std::census::hash_table_bytes(
            self.asked.capacity(),
            core::mem::size_of::<([u8; 16], i64)>(),
        )
    }

    /// `hub` was heard at `now`: asked for its rooms when its last
    /// question is [`LIST_EVERY_SECS`] old, or it was never asked. Whether
    /// it was asked.
    pub fn heard(&mut self, hub: [u8; 16], now: i64) -> bool {
        if let Some(last) = self.asked.get(&hub) {
            if now < last.saturating_add(LIST_EVERY_SECS) {
                return false;
            }
        }
        if !self.asked.contains_key(&hub) && self.asked.len() >= ASKED_BOUND {
            if let Some(oldest) = self
                .asked
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(h, _)| *h)
            {
                self.asked.remove(&oldest);
            }
        }
        self.asked.insert(hub, now);
        let asked = self.linking.asks.send(Ask::Peek { hub }).is_ok();
        tracing::debug!(
            "HOME hub asked hub={} for=rooms",
            crate::address::to_hex(&hub)
        );
        asked
    }

    /// The answers that came since the last call, each hub's rooms kept
    /// with its archive entry. Never waits.
    pub fn take(&mut self, store: &mut Store, cap_bytes: u64) -> Vec<Answered> {
        let mut out = Vec::new();
        while let Ok(told) = self.linking.told.try_recv() {
            let Told::Peeked { hub, rooms } = told else {
                continue;
            };
            let Some(rooms) = rooms else {
                tracing::info!(
                    "HOME hub rooms hub={} answered=false",
                    crate::address::to_hex(&hub)
                );
                out.push(Answered::NoAnswer { hub });
                continue;
            };
            let seq = match store.archive_hub_rooms(&hub, &rooms, cap_bytes) {
                Ok(seq) => seq,
                Err(error) => {
                    tracing::warn!("HOME archive: {error}");
                    None
                }
            };
            tracing::info!(
                "HOME hub rooms hub={} answered=true rooms={} seq={}",
                crate::address::to_hex(&hub),
                rooms.len(),
                seq.map_or(-1, |s| i64::try_from(s).unwrap_or(i64::MAX))
            );
            out.push(Answered::Rooms { hub, rooms, seq });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hub is asked when first heard, not again within the hour, and
    /// again an hour on; another hub is asked on its own clock. The
    /// questions go to the link task, which with no node lent answers each
    /// with no answer.
    #[tokio::test]
    async fn a_hub_is_asked_once_an_hour() {
        let (_lend, lends) = mpsc::unbounded_channel();
        let mut lister = Lister::over(lends);
        assert!(lister.heard([1; 16], 1000));
        assert!(!lister.heard([1; 16], 1000 + LIST_EVERY_SECS - 1));
        assert!(lister.heard([2; 16], 1500));
        assert!(lister.heard([1; 16], 1000 + LIST_EVERY_SECS));
        let home = tempfile::tempdir().expect("home");
        let mut store = Store::open_in(home.path(), [0x11; 16]).expect("store");
        let mut answered = Vec::new();
        for _ in 0..100 {
            answered.extend(lister.take(&mut store, u64::MAX));
            if answered.len() >= 3 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            answered,
            [
                Answered::NoAnswer { hub: [1; 16] },
                Answered::NoAnswer { hub: [2; 16] },
                Answered::NoAnswer { hub: [1; 16] },
            ]
        );
    }
}
