//! Operation counters (1489, review 2026-11-07 finding 14): what one event
//! cost the core in work, counted rather than timed, so a test can say
//! "a key in the list builds the rows on screen" and mean it on any host.
//!
//! The model counts what its presentation builds ([`UiModel::counters`]),
//! the store what it reads and commits ([`crate::store::Store::counters`]);
//! a test adds the two. [`crate::ui::update`] starts the model's count
//! afresh, so after an event and the scene the shell builds for it the
//! count is that event's; a test that measures a frame without an event
//! calls [`UiModel::reset_counters`] first.
//!
//! Compiled in where `debug_assertions` are on (every test and debug
//! build, of this crate and of every crate over it: the window, the
//! terminal, the phone's facade, the M9's host tests) or with the
//! `counters` feature; a release build without the feature carries a
//! zero-sized [`Ops`] whose bumps are empty, and [`UiModel::counters`]
//! answers `None` there, so a test that would read them in such a build
//! fails loudly instead of reading zeros as "built nothing".
//!
//! [`UiModel::counters`]: super::UiModel::counters
//! [`UiModel::reset_counters`]: super::UiModel::reset_counters

use core::fmt;
use core::ops::Add;

/// One event's work, as the core and the store counted it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Scenes built ([`super::UiModel::scene`], `list_scene`).
    pub scenes: u32,
    /// Nodes put into those scenes before a class threw any away: the
    /// reader's bar, each tab, each list row, the chat's bar, each bubble.
    pub scene_nodes: u32,
    /// List rows derived from the conversations ([`super::chat::list_row`]):
    /// the scene's, the terminal's list, the phone's lists.
    pub rows_built: u32,
    /// Bubbles derived from a chat's messages ([`super::chat::bubbles`]).
    pub bubbles_built: u32,
    /// Rows laid out in cells: the terminal's conversation
    /// ([`super::layout::conversation`]) and list ([`super::layout::list`]).
    pub rows_laid: u32,
    /// Times the list was put in order ([`super::UiModel::order`]).
    pub sorts: u32,
    /// Conversations those sorts ordered, summed.
    pub sorted: u32,
    /// Rows the store's reads handed up.
    pub store_rows_read: u32,
    /// Transactions the store committed, each one a sync to the disk.
    pub commits: u32,
    /// Messages walked to find an id because a conversation's summary
    /// was behind them (1521): a batch's folds before its note did, every
    /// message of the conversation per entry.
    pub walked: u32,
    /// Messages copied into store writes ([`super::history::StoreWrite::Message`]),
    /// each a whole message cloned for a store that keeps it (1521b). A
    /// board's store keeps its entries, not the model's copies: there it
    /// is waste, and under the board's first-fit heap the copies' drop
    /// cost the M9 a minute of its restore.
    pub copied: u32,
    /// Conversations a read mark looked into for an id (1600): the ones
    /// the id index named, where before every conversation was asked.
    pub probed: u32,
}

impl Add for Counters {
    type Output = Counters;

    fn add(self, other: Counters) -> Counters {
        Counters {
            scenes: self.scenes + other.scenes,
            scene_nodes: self.scene_nodes + other.scene_nodes,
            rows_built: self.rows_built + other.rows_built,
            bubbles_built: self.bubbles_built + other.bubbles_built,
            rows_laid: self.rows_laid + other.rows_laid,
            sorts: self.sorts + other.sorts,
            sorted: self.sorted + other.sorted,
            store_rows_read: self.store_rows_read + other.store_rows_read,
            commits: self.commits + other.commits,
            walked: self.walked + other.walked,
            copied: self.copied + other.copied,
            probed: self.probed + other.probed,
        }
    }
}

impl fmt::Display for Counters {
    /// One line, every counter by its name: what a test prints and the
    /// report's table is made of.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "scenes={} scene_nodes={} rows_built={} bubbles_built={} rows_laid={} \
             sorts={} sorted={} store_rows_read={} commits={} walked={} copied={} probed={}",
            self.scenes,
            self.scene_nodes,
            self.rows_built,
            self.bubbles_built,
            self.rows_laid,
            self.sorts,
            self.sorted,
            self.store_rows_read,
            self.commits,
            self.walked,
            self.copied,
            self.probed
        )
    }
}

/// What is counted, one slot each in [`Ops`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Scenes,
    SceneNodes,
    RowsBuilt,
    BubblesBuilt,
    RowsLaid,
    Sorts,
    Sorted,
    StoreRowsRead,
    Commits,
    Walked,
    Copied,
    Probed,
}

#[cfg(any(debug_assertions, feature = "counters"))]
const SLOTS: usize = 12;

/// The counters' cells, one set in the model and one in the store.
/// Atomics, so the store's commit hook (a `'static` closure) and a
/// model shared behind a lock count alike; only ever relaxed.
#[cfg(any(debug_assertions, feature = "counters"))]
#[derive(Debug, Default)]
pub struct Ops {
    cells: [core::sync::atomic::AtomicU32; SLOTS],
    /// Everything counted since the cells were made, never reset (1514):
    /// what a batch of many events cost, where [`Ops::reset`] would have
    /// kept only the last event's.
    total: [core::sync::atomic::AtomicU32; SLOTS],
}

/// Off: nothing, and nothing done.
#[cfg(not(any(debug_assertions, feature = "counters")))]
#[derive(Debug, Default)]
pub struct Ops;

/// Whether this build counts.
pub const ON: bool = cfg!(any(debug_assertions, feature = "counters"));

#[cfg(any(debug_assertions, feature = "counters"))]
impl Ops {
    /// Every count at zero.
    pub const fn new() -> Self {
        Self {
            cells: [const { core::sync::atomic::AtomicU32::new(0) }; SLOTS],
            total: [const { core::sync::atomic::AtomicU32::new(0) }; SLOTS],
        }
    }

    /// Count `n` more of `op`.
    #[inline]
    pub fn add(&self, op: Op, n: usize) {
        let n = u32::try_from(n).unwrap_or(u32::MAX);
        self.cells[op as usize].fetch_add(n, core::sync::atomic::Ordering::Relaxed);
        self.total[op as usize].fetch_add(n, core::sync::atomic::Ordering::Relaxed);
    }

    /// Start every count afresh.
    pub fn reset(&self) {
        for cell in &self.cells {
            cell.store(0, core::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The counts so far.
    pub fn read(&self) -> Option<Counters> {
        Some(Self::counts(&self.cells))
    }

    /// The counts since the cells were made, across every reset.
    pub fn read_total(&self) -> Option<Counters> {
        Some(Self::counts(&self.total))
    }

    fn counts(cells: &[core::sync::atomic::AtomicU32; SLOTS]) -> Counters {
        let at = |op: Op| cells[op as usize].load(core::sync::atomic::Ordering::Relaxed);
        Counters {
            scenes: at(Op::Scenes),
            scene_nodes: at(Op::SceneNodes),
            rows_built: at(Op::RowsBuilt),
            bubbles_built: at(Op::BubblesBuilt),
            rows_laid: at(Op::RowsLaid),
            sorts: at(Op::Sorts),
            sorted: at(Op::Sorted),
            store_rows_read: at(Op::StoreRowsRead),
            commits: at(Op::Commits),
            walked: at(Op::Walked),
            copied: at(Op::Copied),
            probed: at(Op::Probed),
        }
    }
}

#[cfg(not(any(debug_assertions, feature = "counters")))]
impl Ops {
    #[inline(always)]
    pub const fn new() -> Self {
        Self
    }

    #[inline(always)]
    pub fn add(&self, _op: Op, _n: usize) {}

    #[inline(always)]
    pub fn reset(&self) {}

    #[inline(always)]
    pub fn read(&self) -> Option<Counters> {
        None
    }

    #[inline(always)]
    pub fn read_total(&self) -> Option<Counters> {
        None
    }
}

impl Clone for Ops {
    /// A copy of the model counts on from where the original stood.
    fn clone(&self) -> Self {
        let ops = Ops::new();
        #[cfg(any(debug_assertions, feature = "counters"))]
        for (to, from) in [(&ops.cells, &self.cells), (&ops.total, &self.total)] {
            for (to, from) in to.iter().zip(from) {
                to.store(
                    from.load(core::sync::atomic::Ordering::Relaxed),
                    core::sync::atomic::Ordering::Relaxed,
                );
            }
        }
        ops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cells count what they are told, a reset empties them, a clone
    /// carries them, and the sum adds slot by slot.
    #[test]
    fn the_cells_count_reset_and_add() {
        // A release test build without the feature counts nothing.
        if !ON {
            return;
        }
        let ops = Ops::new();
        ops.add(Op::RowsBuilt, 3);
        ops.add(Op::RowsBuilt, 2);
        ops.add(Op::Commits, 1);
        let read = ops.read().expect("a test build counts");
        assert_eq!((read.rows_built, read.commits, read.scenes), (5, 1, 0));
        assert_eq!(ops.clone().read(), Some(read));
        assert_eq!((read + read).rows_built, 10);
        ops.reset();
        assert_eq!(ops.read(), Some(Counters::default()));
        // The total outlives the reset (1514).
        ops.add(Op::RowsBuilt, 1);
        assert_eq!(ops.read_total().map(|t| t.rows_built), Some(6));
        assert_eq!(ops.read().map(|t| t.rows_built), Some(1));
    }
}
