//! A photo or a file under way, in its bubble (1662; `loquax-files.md`,
//! section 5): how far a sending message's resource has gone, or how much
//! of a message being fetched has come, as a bar and in words, "wird
//! gesendet · 43 %", "wird geholt · 1,2 MB von 3,0 MB".
//!
//! The model keeps one [`Transfer`] a bubble, by conversation and key, and
//! takes a new one at most every [`EVERY_MS`] of its own clock: a resource
//! reports every part it moves, and a bubble that redraws for each would
//! cost a frame a part. The last one, the transfer complete, is always
//! taken. A transfer is shown only while the bubble still sends
//! ([`super::chat::Mark::Sending`]) or, fetched, until the message is
//! whole; [`Transfers::end`] drops it then.

use alloc::collections::BTreeMap;
use alloc::vec;

use super::strings::{Arg, StrId, Text};

/// The least time between two updates of one bubble, in the model's
/// milliseconds ([`super::UiEvent::Elapsed`]).
pub const EVERY_MS: u64 = 250;

/// Which way the bytes go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    /// Ours, leaving: its words say the share sent.
    Sending,
    /// Coming to this device: its words say the bytes come of the whole.
    Fetching,
}

/// How far one bubble's bytes have gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub way: Way,
    /// The bytes moved so far, at most `total`.
    pub done: u64,
    /// The bytes in all.
    pub total: u64,
}

impl Transfer {
    /// `permille` thousandths of `total` sent: what a resource's progress,
    /// a share of its parts, says of the bubble's bytes.
    pub fn sending(permille: u16, total: u64) -> Self {
        let permille = u64::from(permille.min(1000));
        Self {
            way: Way::Sending,
            done: total.saturating_mul(permille) / 1000,
            total,
        }
    }

    /// A resource's progress, 0.0 to 1.0, in thousandths: what the events
    /// carry, so they compare equal.
    pub fn permille(fraction: f32) -> u16 {
        match fraction.is_finite() {
            true => (fraction.clamp(0.0, 1.0) * 1000.0) as u16,
            false => 0,
        }
    }

    /// The share moved in whole percent, 0 to 100; 0 of nothing is 0.
    pub fn percent(&self) -> u8 {
        if self.total == 0 {
            return 0;
        }
        let done = self.done.min(self.total);
        u8::try_from(done.saturating_mul(100) / self.total).unwrap_or(100)
    }

    /// Whether every byte moved.
    pub fn complete(&self) -> bool {
        self.done >= self.total
    }

    /// What the bubble says and a screen reader reads: "sending · 43 %",
    /// "wird geholt · 1,2 MB von 3,0 MB".
    pub fn words(&self) -> Text {
        match self.way {
            Way::Sending => Text::Key(
                StrId::TransferSending,
                vec![Arg::Int(i64::from(self.percent()))],
            ),
            Way::Fetching => Text::Key(
                StrId::TransferFetching,
                vec![
                    Arg::Text(alloc::boxed::Box::new(megabytes(self.done.min(self.total)))),
                    Arg::Text(alloc::boxed::Box::new(megabytes(self.total))),
                ],
            ),
        }
    }
}

/// `bytes` in megabytes to a tenth, rounded down, with the reader's
/// decimal mark: "1.2 MB", "1,2 MB".
fn megabytes(bytes: u64) -> Text {
    let tenths = bytes / 100_000;
    Text::Key(
        StrId::Megabytes,
        vec![
            Arg::Int(i64::try_from(tenths / 10).unwrap_or(i64::MAX)),
            Arg::Int(i64::try_from(tenths % 10).unwrap_or(0)),
        ],
    )
}

/// The bubbles under way, by conversation and key.
#[derive(Clone, Debug, Default)]
pub struct Transfers {
    running: BTreeMap<([u8; 16], u64), (Transfer, u64)>,
    /// Bumped by every taken change: the chat's built window and the
    /// terminal's laid cache are keyed by it.
    rev: u64,
}

impl Transfers {
    /// The transfer of `key` in `peer`'s conversation, if one runs.
    pub fn of(&self, peer: &[u8; 16], key: u64) -> Option<Transfer> {
        self.running.get(&(*peer, key)).map(|(t, _)| *t)
    }

    /// Take `transfer` for `key` at `now_ms`, unless the bubble took one
    /// less than [`EVERY_MS`] before and this one is not the last. Whether
    /// it was taken.
    pub fn set(&mut self, peer: [u8; 16], key: u64, transfer: Transfer, now_ms: u64) -> bool {
        if let Some((was, at)) = self.running.get(&(peer, key)) {
            if *was == transfer {
                return false;
            }
            if now_ms < at.saturating_add(EVERY_MS) && !transfer.complete() {
                return false;
            }
        }
        self.running.insert((peer, key), (transfer, now_ms));
        self.rev += 1;
        true
    }

    /// The bubble's transfer is over, however it ended.
    pub fn end(&mut self, peer: &[u8; 16], key: u64) {
        if self.running.remove(&(*peer, key)).is_some() {
            self.rev += 1;
        }
    }

    /// Every transfer of `peer`'s conversation is over: it was deleted.
    pub fn end_all(&mut self, peer: &[u8; 16]) {
        let before = self.running.len();
        self.running.retain(|(p, _), _| p != peer);
        if self.running.len() != before {
            self.rev += 1;
        }
    }

    /// How often a change was taken.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Whether none runs.
    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::strings::Lang;

    #[test]
    fn a_send_says_its_share_and_a_fetch_its_bytes_in_the_readers_words() {
        let sending = Transfer::sending(430, 300_000);
        assert_eq!(sending.percent(), 43);
        assert_eq!(sending.words().resolve(Lang::De), "wird gesendet · 43 %");
        assert_eq!(sending.words().resolve(Lang::En), "sending · 43 %");
        let fetching = Transfer {
            way: Way::Fetching,
            done: 1_234_567,
            total: 3_000_000,
        };
        assert_eq!(
            fetching.words().resolve(Lang::De),
            "wird geholt · 1,2 MB von 3,0 MB"
        );
        assert_eq!(
            fetching.words().resolve(Lang::En),
            "fetching · 1.2 MB of 3.0 MB"
        );
        assert_eq!(Transfer::permille(f32::NAN), 0);
        assert_eq!(Transfer::permille(0.4321), 432);
        assert_eq!(Transfer::sending(7000, 10).percent(), 100);
        assert_eq!(
            Transfer {
                way: Way::Sending,
                done: 0,
                total: 0
            }
            .percent(),
            0
        );
    }

    /// A bubble takes one update per 250 ms; the last always.
    #[test]
    fn a_bubble_takes_one_update_a_quarter_second_and_the_last_always() {
        let mut transfers = Transfers::default();
        let peer = [1; 16];
        assert!(transfers.set(peer, 7, Transfer::sending(100, 1000), 1_000));
        assert!(!transfers.set(peer, 7, Transfer::sending(200, 1000), 1_100));
        assert!(!transfers.set(peer, 7, Transfer::sending(300, 1000), 1_249));
        assert!(transfers.set(peer, 7, Transfer::sending(400, 1000), 1_250));
        assert_eq!(transfers.of(&peer, 7).map(|t| t.percent()), Some(40));
        assert!(transfers.set(peer, 7, Transfer::sending(1000, 1000), 1_260));
        // Another bubble has its own quarter second.
        assert!(transfers.set(peer, 8, Transfer::sending(500, 1000), 1_260));
        let rev = transfers.rev();
        transfers.end(&peer, 7);
        assert_eq!(transfers.of(&peer, 7), None);
        assert!(transfers.rev() > rev);
        transfers.end_all(&peer);
        assert!(transfers.is_empty());
    }
}
