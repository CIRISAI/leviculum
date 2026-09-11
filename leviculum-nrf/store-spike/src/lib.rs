//! Spike harness for Codeberg #384: which store goes on the internal flash.
//!
//! Neither board carries a QSPI part (commit 081522b2), so the store lives in
//! the nRF52840's own flash behind the firmware image. Two candidates were
//! weighed:
//!
//! - **`sequential-storage` 8.0.1**, its queue (`tests/sequential_storage.rs`);
//! - **`leviculum-record-log`**, ported to `embedded-storage-async`
//!   (`tests/record_log.rs`).
//!
//! Both run over `leviculum_record_log::sim::SimNor` and nothing else, so a
//! difference between them is a difference in the store rather than in what
//! it was measured against. The six acceptance criteria are numbered the
//! same way in both test files, and a criterion a candidate cannot meet is
//! reported as a failing or `#[ignore]`d test with the reason on it rather
//! than quietly reshaped.
//!
//! This crate ships nothing. It exists so the comparison is reproducible;
//! `cargo test -p leviculum-store-spike` is the whole of it.

use std::vec::Vec;

pub use leviculum_record_log::sim::{block_on, SimNor, Yielding};

/// Pages in the region every acceptance test uses, unless it says otherwise.
pub const PAGES: u32 = 8;
/// Erase unit, and page size, of the nRF52840's internal flash.
pub const PAGE: u32 = 4096;
/// The region both candidates are measured on.
pub const REGION: u32 = PAGES * PAGE;

/// The median stored object the 2026-09-09 field walk measured: 272 B of
/// `lxmf_data` plus a 32-byte propagation stamp.
pub const FIELD_BODY: usize = 304;

/// A distinguishable payload of `len` bytes for entry `n`.
pub fn body(n: u32, len: usize) -> Vec<u8> {
    (0..len).map(|i| (n as u8).wrapping_add(i as u8)).collect()
}

/// A deterministic stream of bytes that is not flash-shaped: no long `0xFF`
/// or `0x00` runs, so "somebody else's data" is actually foreign.
pub fn noise(seed: u32, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect()
}

/// The wear pin: the spread between the most- and least-erased page.
pub fn wear_spread(counts: &[u32]) -> u32 {
    let max = counts.iter().copied().max().unwrap_or(0);
    let min = counts.iter().copied().min().unwrap_or(0);
    max - min
}

/// Which push produced each entry in a view, given that entries `0..=max`
/// were pushed in order.
///
/// Panics if an entry is one nobody pushed, which is the first thing a store
/// that invents data would trip over.
pub fn ids(view: &[Vec<u8>], max: u32, len: usize) -> Vec<u32> {
    view.iter()
        .map(|entry| {
            (0..=max)
                .find(|n| body(*n, len) == *entry)
                .expect("an entry nobody pushed")
        })
        .collect()
}

/// The invariant an overwriting store has to hold after a failed or
/// interrupted append: what is left is a contiguous run of what went in,
/// nothing newer than `newest` is there, and entries may have gone only from
/// the oldest end. Returns how many were lost from that end.
pub fn assert_contiguous_suffix(
    before: &[u32],
    after: &[u32],
    newest: u32,
    context: &str,
) -> usize {
    assert!(!after.is_empty(), "{context}: the store went empty");
    assert!(
        after.windows(2).all(|w| w[1] == w[0] + 1),
        "{context}: the survivors must be contiguous: {after:?}"
    );
    assert_eq!(
        *after.last().unwrap(),
        newest,
        "{context}: the newest entry is not the one the call left behind: {after:?}"
    );
    assert!(
        before.ends_with(after),
        "{context}: entries may only be dropped from the oldest end: \
         {before:?} -> {after:?}"
    );
    before.len() - after.len()
}
