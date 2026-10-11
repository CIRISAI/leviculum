//! The log rings: the persistent tail a boot replays from the boot before,
//! and the runtime ring the debug port drains (leviculum#413, the stores
//! section of `docs/src/firmware/host-test-seams.md`).
//!
//! The persistent tail lives in `.retained` RAM beside the post-mortems
//! ([`super::postmortem`]): a magic, a write position and a ring that every
//! log line is mirrored into. A soft reset keeps it; power-on RAM, a torn
//! record or an older image's layout does not carry this magic. The
//! volatile reads and writes stay in `leviculum-nrf/src/log.rs`; what they
//! decide is here:
//!
//! - [`must_rearm`]: a writer that finds any word but [`TAIL_MAGIC`] zeroes
//!   the ring and starts the current boot's tail at position zero.
//! - [`tail_window`]: what a boot replays: nothing without the magic, the
//!   whole ring once the writer has gone round it, the bytes written so far
//!   before that.
//! - [`replay_lines`]: the lines a boot logs from it. The ring wraps inside
//!   a line, so the first line is dropped unless it is the whole snapshot.
//! - [`lapped_reader`]: a writer that laps the runtime ring's reader moves
//!   the reader to the oldest byte still there.

use crate::postmortem::ring_window;

/// The persistent tail's valid marker. Bump it if the record's layout
/// changes, so an older image's tail reads as absent rather than misparsed.
pub const TAIL_MAGIC: u32 = 0x10A9_11A1;

/// Whether the writer has to zero the ring and restart at position zero
/// before its first write: any word but [`TAIL_MAGIC`]. After a boot replayed
/// and cleared the tail, this is what starts the current boot's.
pub const fn must_rearm(magic: u32) -> bool {
    magic != TAIL_MAGIC
}

/// The previous boot's tail in a ring of `cap` bytes: `(start, len)` with
/// the oldest byte at `start`, or `None` without [`TAIL_MAGIC`].
///
/// The write position is the count of bytes the boot reserved, so a value
/// at or past `cap` means the writer went round the ring and the last `cap`
/// bytes survive. A write position no boot could have written (a torn
/// record that kept its magic) still reads inside the ring.
pub fn tail_window(magic: u32, write_pos: usize, cap: usize) -> Option<(usize, usize)> {
    if must_rearm(magic) {
        return None;
    }
    Some(ring_window(write_pos, cap))
}

/// The lines a boot logs from a replayed `snapshot`, without their line
/// ends, empty lines skipped, bytes that are not UTF-8 named as such.
///
/// The first line is dropped unless it reaches the end of the snapshot: the
/// ring wraps inside lines, so the oldest bytes are usually the tail of a
/// line whose head was overwritten. A snapshot without a line end is one
/// line and kept.
pub fn replay_lines(snapshot: &[u8]) -> impl Iterator<Item = &str> {
    let mut start = 0usize;
    core::iter::from_fn(move || loop {
        if start >= snapshot.len() {
            return None;
        }
        let end = snapshot[start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| start + p + 1)
            .unwrap_or(snapshot.len());
        let first = start == 0;
        let raw = &snapshot[start..end];
        start = end;
        if first && end != snapshot.len() {
            continue;
        }
        let line = core::str::from_utf8(raw)
            .unwrap_or("<non-utf8>")
            .trim_end_matches(['\r', '\n']);
        if !line.is_empty() {
            return Some(line);
        }
    })
}

/// Where the runtime ring's reader stands after a write that ended at
/// `write_pos`: `Some(oldest)` when the writer lapped it and the bytes it had
/// not read are gone, `None` when it is still behind the writer by at most
/// `cap`. Positions are free-running counters, compared with wrapping
/// arithmetic.
pub fn lapped_reader(write_pos: usize, read_pos: usize, cap: usize) -> Option<usize> {
    (write_pos.wrapping_sub(read_pos) > cap).then(|| write_pos.wrapping_sub(cap))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 2048;

    #[test]
    fn power_on_ram_and_a_foreign_magic_rearm_the_tail() {
        assert!(must_rearm(0));
        assert!(must_rearm(0xFFFF_FFFF));
        // The post-mortems' magics share `.retained` and are not this one.
        assert!(must_rearm(crate::postmortem::PANIC_RECORD.fresh));
        assert!(!must_rearm(TAIL_MAGIC));
    }

    #[test]
    fn without_the_magic_nothing_is_replayed() {
        assert_eq!(tail_window(0, 100, CAP), None);
        assert_eq!(tail_window(TAIL_MAGIC ^ 1, 100, CAP), None);
    }

    #[test]
    fn a_tail_that_did_not_go_round_is_replayed_from_the_start() {
        assert_eq!(tail_window(TAIL_MAGIC, 0, CAP), Some((0, 0)));
        assert_eq!(tail_window(TAIL_MAGIC, 700, CAP), Some((0, 700)));
        assert_eq!(tail_window(TAIL_MAGIC, CAP, CAP), Some((0, CAP)));
    }

    #[test]
    fn a_tail_that_went_round_keeps_its_last_ring_of_bytes() {
        assert_eq!(tail_window(TAIL_MAGIC, CAP + 5, CAP), Some((5, CAP)));
        assert_eq!(tail_window(TAIL_MAGIC, 3 * CAP + 17, CAP), Some((17, CAP)));
    }

    #[test]
    fn a_torn_write_position_still_reads_inside_the_ring() {
        let (start, len) = tail_window(TAIL_MAGIC, usize::MAX, CAP).unwrap();
        assert!(start < CAP);
        assert_eq!(len, CAP);
        let (start, len) = tail_window(TAIL_MAGIC, 0xFFFF_FFF0, CAP).unwrap();
        assert!(start < CAP);
        assert_eq!(len, CAP);
    }

    fn replayed(snapshot: &[u8]) -> Vec<&str> {
        replay_lines(snapshot).collect()
    }

    #[test]
    fn the_leading_partial_line_is_dropped() {
        assert_eq!(
            replayed(b"ss=12 t=40\n[LORA] tx\n[PN] round\n"),
            ["[LORA] tx", "[PN] round"]
        );
    }

    #[test]
    fn a_last_line_without_its_end_is_kept() {
        assert_eq!(
            replayed(b"tail\n[LORA] tx\n[PANIC] at"),
            ["[LORA] tx", "[PANIC] at"]
        );
    }

    #[test]
    fn a_snapshot_that_is_one_line_is_kept() {
        assert_eq!(replayed(b"[BOOT] only line"), ["[BOOT] only line"]);
        assert_eq!(replayed(b"[BOOT] only line\r\n"), ["[BOOT] only line"]);
    }

    #[test]
    fn empty_lines_and_line_ends_are_not_replayed() {
        assert_eq!(replayed(b"x\n\r\n\n[A] a\r\n"), ["[A] a"]);
        assert!(replayed(b"").is_empty());
    }

    #[test]
    fn bytes_that_are_not_utf8_are_named() {
        assert_eq!(replayed(b"x\n\xff\xfe\n"), ["<non-utf8>"]);
    }

    #[test]
    fn a_writer_that_laps_the_reader_moves_it_to_the_oldest_byte() {
        let cap = 8192;
        assert_eq!(lapped_reader(100, 0, cap), None);
        assert_eq!(lapped_reader(cap, 0, cap), None);
        assert_eq!(lapped_reader(cap + 1, 0, cap), Some(1));
        assert_eq!(lapped_reader(5 * cap, cap, cap), Some(4 * cap));
    }

    #[test]
    fn the_lap_is_found_across_the_counters_wrap() {
        let cap = 8192;
        let read = usize::MAX - 10;
        assert_eq!(lapped_reader(read.wrapping_add(cap), read, cap), None);
        assert_eq!(
            lapped_reader(read.wrapping_add(cap + 3), read, cap),
            Some(read.wrapping_add(3))
        );
    }
}
