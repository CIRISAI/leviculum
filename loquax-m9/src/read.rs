//! What the reader saw on the M9 (1541): the board says the model which
//! bubbles of the open chat are on screen, as the phone and the terminal
//! do ([`UiEvent::Viewport`]), so a message is read by the rule of 1529,
//! its last line on screen for the dwell (`loquax::ui::receipts`), not by
//! the open alone. `UiEvent::Open` stays the open's word for the first
//! page (1493, [`crate::group::press`]).
//!
//! The core does the rest, as on every target: the bubbles that count,
//! the dwell ([`loquax::ui::receipts::SEEN_MS`]), and, the board being a
//! device of its home, the seen mark in place of a receipt of its own,
//! which [`crate::home::route`] asks the home for and the store keeps
//! ([`crate::home::keep_marks`]). The home signs the receipt and the
//! sender sees the message read.
//!
//! The dwell runs on the model's clocks, which the board never said
//! before: the monotonic one, its uptime ([`UiEvent::Elapsed`]), and the
//! time of day ([`UiEvent::Clock`]), which a seen mark carries as the
//! reader's time. The board has no clock of its own for the day; the
//! home's archive stamps every entry with the home's time
//! ([`Entry::at`]), so the board's day is the newest stamp it took,
//! carried forward by its uptime ([`Reader::saw`]). That is never later
//! than the true time, and never earlier than the message read arrived
//! at the home.

use alloc::vec::Vec;

use loquax::home::wire::Entry;
use loquax::ui::history::StoreRead;
use loquax::ui::{update, UiEffect, UiEvent, UiModel};
use loquax::when::WallTime;

use crate::field::Field;

/// The board's clocks for the model, and its word on what is on screen.
#[derive(Clone, Copy, Debug, Default)]
pub struct Reader {
    /// The home's time as last taken: the newest entry's stamp, unix
    /// seconds, and the board's uptime when it was taken, milliseconds.
    home: Option<(i64, u64)>,
    /// The second of uptime the model was last told.
    told: Option<u64>,
}

impl Reader {
    /// Take the home's time from `entries` (a pulled page, a page read
    /// from the ring), at uptime `uptime_ms`: a stamp later than the day
    /// carried forward moves it there; an older one says nothing.
    pub fn saw(&mut self, entries: &[Entry], uptime_ms: u64) {
        let Some(newest) = entries.iter().map(|entry| entry.at).max() else {
            return;
        };
        let carried = self.day(uptime_ms).map(|day| day.unix_secs);
        if carried.is_none_or(|carried| newest > carried) {
            self.home = Some((newest, uptime_ms));
        }
    }

    /// The time of day at uptime `uptime_ms`, as [`Reader::saw`] keeps it;
    /// none before the first stamp.
    pub fn day(&self, uptime_ms: u64) -> Option<WallTime> {
        let (at, then) = self.home?;
        let since = i64::try_from(uptime_ms.saturating_sub(then) / 1000).unwrap_or(i64::MAX);
        Some(WallTime {
            unix_secs: at.saturating_add(since),
            offset_secs: 0,
        })
    }

    /// Tell the model the clocks once a second of uptime: the dwell and the
    /// receipt's batch run on them. What the model hands out, and whether
    /// the screen owes a draw: the day was first known, which the list's
    /// times show.
    pub fn tick(&mut self, model: &mut UiModel<Field>, uptime_ms: u64) -> (Vec<UiEffect>, bool) {
        let second = uptime_ms / 1000;
        if self.told == Some(second) {
            return (Vec::new(), false);
        }
        self.told = Some(second);
        let mut effects = update(model, UiEvent::Elapsed(uptime_ms));
        let mut first = false;
        if let Some(day) = self.day(uptime_ms) {
            first = model.now.is_none();
            effects.extend(update(model, UiEvent::Clock(day)));
        }
        (effects, first)
    }

    /// Say what the last draw showed of the open chat, `seen` the keys of
    /// its bubbles whose last line was on screen
    /// ([`crate::render::Renderer::seen`]); none when the draw showed no
    /// chat, which says an empty viewport, as the terminal's map does.
    /// Said only when it moved: one event a drawn window, none a frame
    /// that showed the same. The board's panel is its front while it
    /// draws: that is said once, as the phone says its screen on.
    ///
    /// What the model hands out, but the earlier page the viewport asks
    /// when the first loaded bubble shows: the M9 asks that page by its
    /// own lines (1529, the scroll keys' `UiEvent::Lines`), and a second
    /// ask would read the same page from the ring twice.
    pub fn say(
        &mut self,
        model: &mut UiModel<Field>,
        seen: Option<&[u64]>,
        uptime_ms: u64,
    ) -> Vec<UiEffect> {
        let Some(peer) = model.conversations.get(model.selected).map(|c| c.peer) else {
            return Vec::new();
        };
        let keys = seen.map(<[u64]>::to_vec).unwrap_or_default();
        let mut effects = Vec::new();
        if model.reading.front != Some(true) {
            effects.extend(update(model, UiEvent::Front(true)));
        }
        if model.reading.viewport() == Some(&(peer, keys.clone())) {
            return effects;
        }
        // The dwell counts from this moment, not from the last whole
        // second the model was told.
        if model.elapsed_ms != uptime_ms {
            effects.extend(update(model, UiEvent::Elapsed(uptime_ms)));
        }
        effects.extend(
            update(model, UiEvent::Viewport { peer, keys })
                .into_iter()
                .filter(|effect| !matches!(effect, UiEffect::Store(StoreRead::Earlier { .. }))),
        );
        effects
    }
}
