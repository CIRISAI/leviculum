//! The process's own view of its memory, for the SIGUSR1 census.
//!
//! # Why this exists beside the collection census
//!
//! The core census prices what the node's tables hold. On the soak node of
//! Codeberg #437 it said 160 MB while the process held 1.8 GB of anonymous
//! memory (RSS plus swap), and it could not say whether the difference was
//! the allocator keeping freed memory or a live owner nobody counts. These
//! three sections are the measurements that tell the two apart:
//!
//! * `=== Allocator ===` is the allocator's own balance sheet (glibc
//!   `mallinfo2()`), plus the thread count. musl's mallocng, which the
//!   shipped musl-static builds run, has no such call, and the section says
//!   so instead of printing zeros.
//! * `=== Mappings ===` buckets the anonymous writable regions of
//!   `/proc/self/maps` by size. Allocator arenas, large mmapped allocations
//!   and thread stacks each land in a recognisable size class, and the ten
//!   largest regions are named by address.
//! * The `gap:` line does the subtraction a reader would otherwise do by
//!   hand: resident plus swapped, minus the tracked total, minus the
//!   allocator's free bytes, minus its mmapped bytes.
//!
//! Every parser here takes text, not a path, so the tests feed fixtures and
//! the daemon feeds `/proc/self/*`.

use std::fmt::Write as _;

/// The `/proc/self/status` fields the census reads.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StatusFields {
    /// `Threads:` — tokio workers, blocking-pool threads, interface readers.
    pub threads: Option<u64>,
    /// `VmSwap:` in bytes (the file says kB).
    pub swap_bytes: Option<u64>,
}

/// Parse the fields the census needs out of `/proc/self/status` text.
pub fn parse_status(text: &str) -> StatusFields {
    let mut out = StatusFields::default();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let value = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        match key {
            "Threads" => out.threads = value,
            "VmSwap" => out.swap_bytes = value.map(|kb: u64| kb * 1024),
            _ => {}
        }
    }
    out
}

/// One anonymous writable region of `/proc/self/maps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonRegion {
    /// First address of the region.
    pub start: u64,
    /// One past the last address.
    pub end: u64,
    /// The bracketed name (`[heap]`, `[stack]`, `[anon:...]`), empty for a
    /// plain anonymous mapping.
    pub name: String,
}

impl AnonRegion {
    /// Size of the region in bytes.
    pub fn size(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

/// Keep the anonymous writable regions of `/proc/self/maps` text.
///
/// Anonymous means inode 0 and either no pathname or a bracketed one; the
/// kernel's own `[vvar]`, `[vdso]` and `[vsyscall]` pages are not ours and
/// are dropped. Writable drops the `---p` guard pages under every thread
/// stack, which would otherwise double the small-region count.
pub fn parse_anon_writable_maps(text: &str) -> Vec<AnonRegion> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(range), Some(perms), Some(_offset), Some(_dev), Some(inode)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let name = fields.collect::<Vec<_>>().join(" ");
        if inode != "0" || perms.as_bytes().get(1) != Some(&b'w') {
            continue;
        }
        if !(name.is_empty() || name.starts_with('[')) {
            continue;
        }
        if matches!(name.as_str(), "[vvar]" | "[vdso]" | "[vsyscall]") {
            continue;
        }
        let Some((start, end)) = range.split_once('-') else {
            continue;
        };
        let (Ok(start), Ok(end)) = (u64::from_str_radix(start, 16), u64::from_str_radix(end, 16))
        else {
            continue;
        };
        out.push(AnonRegion { start, end, name });
    }
    out
}

/// The size classes of the mapping histogram, as (upper bound exclusive,
/// label). The last class has no bound.
const BUCKETS: [(u64, &str); 4] = [
    (64 * 1024, "<64KiB"),
    (1024 * 1024, "64KiB-1MiB"),
    (8 * 1024 * 1024, "1MiB-8MiB"),
    (64 * 1024 * 1024, "8MiB-64MiB"),
];

/// How many of the largest regions the section names.
const LARGEST: usize = 10;

/// Render the `=== Mappings ===` section from parsed regions.
pub fn render_mappings(regions: &[AnonRegion]) -> String {
    let mut counts = [(0u64, 0u64); BUCKETS.len() + 1];
    for r in regions {
        let size = r.size();
        let idx = BUCKETS
            .iter()
            .position(|(bound, _)| size < *bound)
            .unwrap_or(BUCKETS.len());
        counts[idx].0 += 1;
        counts[idx].1 += size;
    }
    let total: u64 = regions.iter().map(AnonRegion::size).sum();

    let mut out = String::new();
    let _ = writeln!(
        out,
        "=== Mappings: {} anonymous writable regions, {} bytes ===",
        regions.len(),
        total
    );
    let labels = BUCKETS
        .iter()
        .map(|(_, label)| *label)
        .chain(core::iter::once(">=64MiB"));
    for (label, (count, bytes)) in labels.zip(counts.iter()) {
        let _ = writeln!(out, "maps {label}: {count} regions, {bytes} bytes");
    }
    let mut largest: Vec<&AnonRegion> = regions.iter().collect();
    largest.sort_by_key(|r| core::cmp::Reverse(r.size()));
    for r in largest.into_iter().take(LARGEST) {
        let _ = writeln!(
            out,
            "maps largest: {:x}-{:x} {} bytes{}{}",
            r.start,
            r.end,
            r.size(),
            if r.name.is_empty() { "" } else { " " },
            r.name
        );
    }
    out
}

/// The glibc allocator's balance sheet (`mallinfo2()`), in bytes except the
/// two counts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AllocatorView {
    /// Non-mmapped space taken from the system (all arenas).
    pub arena: u64,
    /// Number of free chunks.
    pub ordblks: u64,
    /// Number of mmapped chunks.
    pub hblks: u64,
    /// Bytes in mmapped chunks (large allocations, live).
    pub hblkhd: u64,
    /// Bytes in use in the arenas.
    pub uordblks: u64,
    /// Bytes free in the arenas, held but not returned.
    pub fordblks: u64,
    /// Releasable free space at the top of the main arena.
    pub keepcost: u64,
}

/// The allocator's view, where the libc has one to give.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn allocator_view() -> Option<AllocatorView> {
    // SAFETY: mallinfo2 takes no arguments and returns a struct by value;
    // it walks the arenas under their own locks.
    let m = unsafe { libc::mallinfo2() };
    Some(AllocatorView {
        arena: m.arena as u64,
        ordblks: m.ordblks as u64,
        hblks: m.hblks as u64,
        hblkhd: m.hblkhd as u64,
        uordblks: m.uordblks as u64,
        fordblks: m.fordblks as u64,
        keepcost: m.keepcost as u64,
    })
}

/// The allocator's view, where the libc has one to give. musl's mallocng
/// keeps no such totals, so a musl build answers `None` and the dump says
/// why.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn allocator_view() -> Option<AllocatorView> {
    None
}

/// Render the `=== Allocator ===` section.
pub fn render_allocator(view: Option<AllocatorView>, threads: Option<u64>) -> String {
    let mut out = String::new();
    match view {
        Some(v) => {
            let _ = writeln!(out, "=== Allocator: glibc mallinfo2 ===");
            let _ = writeln!(
                out,
                "allocator: arena={} ordblks={} hblks={} hblkhd={} uordblks={} fordblks={} keepcost={}",
                v.arena, v.ordblks, v.hblks, v.hblkhd, v.uordblks, v.fordblks, v.keepcost
            );
        }
        None => {
            let _ = writeln!(out, "=== Allocator: no mallinfo2 on this libc ===");
            let _ = writeln!(
                out,
                "allocator: unavailable (musl mallocng keeps no totals; read the Mappings section)"
            );
        }
    }
    match threads {
        Some(t) => {
            let _ = writeln!(out, "threads: {t}");
        }
        None => {
            let _ = writeln!(out, "threads: unknown");
        }
    }
    out
}

/// Render the `gap:` line: resident plus swap, then what is left after
/// each subtraction in turn. A term the process cannot measure is printed
/// as `n/a` and subtracts nothing.
pub fn render_gap_line(
    rss: u64,
    swap: Option<u64>,
    tracked: Option<u64>,
    view: Option<AllocatorView>,
) -> String {
    fn step(out: &mut String, left: &mut u64, label: &str, term: Option<u64>) {
        match term {
            Some(t) => {
                *left = left.saturating_sub(t);
                let _ = write!(out, "; - {label} {t} = {left}");
            }
            None => {
                let _ = write!(out, "; - {label} n/a");
            }
        }
    }
    let swap_bytes = swap.unwrap_or(0);
    let mut left = rss + swap_bytes;
    let mut out = format!(
        "gap: rss {rss} + swap {} = {left}",
        swap.map_or_else(|| "n/a".to_string(), |s| s.to_string())
    );
    step(&mut out, &mut left, "tracked", tracked);
    step(
        &mut out,
        &mut left,
        "allocator free",
        view.map(|v| v.fordblks + v.keepcost),
    );
    step(&mut out, &mut left, "mmapped", view.map(|v| v.hblkhd));
    let _ = writeln!(out, "; unaccounted {left}");
    out
}

/// The three sections, read from this process's `/proc/self` files.
/// `rss` and `tracked` come from the caller, which already holds them.
pub fn census_sections(rss: u64, tracked: Option<u64>) -> String {
    let status = std::fs::read_to_string("/proc/self/status")
        .map(|t| parse_status(&t))
        .unwrap_or_default();
    let regions = std::fs::read_to_string("/proc/self/maps")
        .map(|t| parse_anon_writable_maps(&t))
        .unwrap_or_default();
    let view = allocator_view();

    let mut out = render_allocator(view, status.threads);
    out.push_str(&render_mappings(&regions));
    out.push_str(&render_gap_line(rss, status.swap_bytes, tracked, view));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "Name:\tlnsd\n\
        VmPeak:\t 1900000 kB\n\
        VmRSS:\t  907776 kB\n\
        VmData:\t 1841124 kB\n\
        VmSwap:\t  893516 kB\n\
        Threads:\t13\n\
        SigQ:\t0/7000\n";

    const MAPS: &str = "\
5581d0a00000-5581d0c00000 r-xp 00000000 fd:01 1234 /usr/bin/lnsd
5581d0c00000-5581d0c10000 rw-p 00200000 fd:01 1234 /usr/bin/lnsd
5581d1000000-5581d1100000 rw-p 00000000 00:00 0 [heap]
7f0000000000-7f0004000000 rw-p 00000000 00:00 0
7f0004000000-7f0005000000 rw-p 00000000 00:00 0
7f0005000000-7f0005001000 ---p 00000000 00:00 0
7f0005001000-7f0005201000 rw-p 00000000 00:00 0
7f0005300000-7f0005308000 rw-p 00000000 00:00 0
7f0005400000-7f0005420000 r--p 00000000 00:00 0
7f0006000000-7f0012000000 rw-p 00000000 00:00 0
7ffd00000000-7ffd00021000 rw-p 00000000 00:00 0 [stack]
7ffd00100000-7ffd00102000 r--p 00000000 00:00 0 [vvar]
7ffd00102000-7ffd00104000 r-xp 00000000 00:00 0 [vdso]
";

    #[test]
    fn status_parser_reads_threads_and_swap() {
        let s = parse_status(STATUS);
        assert_eq!(s.threads, Some(13));
        assert_eq!(s.swap_bytes, Some(893_516 * 1024));
        assert_eq!(parse_status("Name:\tx\n"), StatusFields::default());
    }

    #[test]
    fn maps_parser_keeps_anonymous_writable_regions_only() {
        let r = parse_anon_writable_maps(MAPS);
        let sizes: Vec<u64> = r.iter().map(AnonRegion::size).collect();
        // heap 1 MiB, 64 MiB, 16 MiB, 2 MiB stack-sized, 32 KiB, 192 MiB,
        // [stack] 132 KiB. Dropped: the file-backed lines, the guard page,
        // the read-only anonymous region, vvar and vdso.
        assert_eq!(
            sizes,
            vec![0x10_0000, 0x400_0000, 0x100_0000, 0x20_0000, 0x8000, 0xc00_0000, 0x21000]
        );
        assert_eq!(r[0].name, "[heap]");
        assert_eq!(r[1].name, "");
    }

    #[test]
    fn mappings_histogram_buckets_and_names_the_largest() {
        let out = render_mappings(&parse_anon_writable_maps(MAPS));
        assert!(out.starts_with("=== Mappings: 7 anonymous writable regions, "));
        assert!(
            out.contains("maps <64KiB: 1 regions, 32768 bytes\n"),
            "{out}"
        );
        assert!(
            out.contains("maps 64KiB-1MiB: 1 regions, 135168 bytes\n"),
            "{out}"
        );
        // 1 MiB is the lower edge of its class, not the upper edge of the last.
        assert!(
            out.contains("maps 1MiB-8MiB: 2 regions, 3145728 bytes\n"),
            "{out}"
        );
        assert!(
            out.contains("maps 8MiB-64MiB: 1 regions, 16777216 bytes\n"),
            "{out}"
        );
        assert!(
            out.contains("maps >=64MiB: 2 regions, 268435456 bytes\n"),
            "{out}"
        );
        let first = out
            .lines()
            .find(|l| l.starts_with("maps largest: "))
            .unwrap_or_default();
        assert_eq!(
            first,
            "maps largest: 7f0006000000-7f0012000000 201326592 bytes"
        );
        assert!(out.contains("1048576 bytes [heap]\n"), "{out}");
    }

    #[test]
    fn gap_line_subtracts_each_term_in_turn() {
        let view = AllocatorView {
            fordblks: 100,
            keepcost: 20,
            hblkhd: 300,
            ..AllocatorView::default()
        };
        assert_eq!(
            render_gap_line(1000, Some(500), Some(400), Some(view)),
            "gap: rss 1000 + swap 500 = 1500; - tracked 400 = 1100; \
             - allocator free 120 = 980; - mmapped 300 = 680; unaccounted 680\n"
        );
        assert_eq!(
            render_gap_line(1000, None, Some(400), None),
            "gap: rss 1000 + swap n/a = 1000; - tracked 400 = 600; \
             - allocator free n/a; - mmapped n/a; unaccounted 600\n"
        );
    }

    #[test]
    fn allocator_section_says_why_it_is_empty() {
        let none = render_allocator(None, Some(4));
        assert!(none.starts_with("=== Allocator: no mallinfo2 on this libc ===\n"));
        assert!(none.contains("threads: 4\n"));
        let some = render_allocator(Some(AllocatorView::default()), None);
        assert!(some.contains("allocator: arena=0 ordblks=0 hblks=0 hblkhd=0 "));
        assert!(some.contains("threads: unknown\n"));
    }
}
