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
//!   `mallinfo2()`, looked up at run time with `mallinfo()` as the fallback
//!   on a glibc older than 2.33), plus the thread count. musl has no such
//!   call, and the
//!   shipped musl-static lnsd runs mimalloc rather than libc's allocator
//!   anyway (#437), so there the section says so instead of printing zeros.
//! * `=== Mappings ===` buckets the anonymous writable regions of
//!   `/proc/self/maps` by size. Allocator arenas, large mmapped allocations
//!   and thread stacks each land in a recognisable size class, and the ten
//!   largest regions are named by address.
//! * The `gap:` line does the subtraction a reader would otherwise do by
//!   hand: resident plus swapped, minus the resident file pages, minus the
//!   tracked total (or, in a `heap-accounting` build, the measured live
//!   bytes instead), minus the allocator's free bytes, minus its mmapped
//!   bytes.
//!
//! The file pages come off because they are not the process's memory to
//! account: they are the binary's text and the libraries, mapped from disk,
//! and under memory pressure the kernel drops them from the resident set
//! without swapping them and faults them back in as the code runs. Left in,
//! they moved the line by megabytes on a host short of memory while nothing
//! the daemon owns changed (#478, order 1253: lnpnd 10.0 to 1.9 MB resident
//! with 0.56 MB swapped, then back up through the canary's window).
//!
//! The shared pages (`RssShmem`) come off for the same reason. A file on
//! tmpfs is booked there, not under `RssFile`: a binary started from a
//! tmpfs tree (a `/tmp` deploy, likely the forge's workspace) or a store on tmpfs
//! that is mapped. When those pages leave the resident set, the swap they
//! may go to is the tmpfs's, which `VmSwap` does not count, so they leave
//! RSS plus swap exactly as file pages do (order 1372, pipelines 602/603).
//! The daemons map no anonymous shared memory of their own.
//!
//! Two more figures ride on the line without being subtracted:
//! `lazyfree=` is the `LazyFree` of `/proc/self/smaps_rollup`, anonymous
//! pages the allocator released with `MADV_FREE` that the kernel may drop
//! from the resident set under pressure without swapping them, and
//! `majfault=` is the major-fault count of `/proc/self/stat`. The nightly
//! canary of 2026-11-10 read a host-reclaim dip as growth: anonymous pages
//! left the resident set without reaching swap and came back (#467, order
//! 1637). Whether those were lazy-free pages is what the two figures are
//! there to show; the arithmetic waits for a census that says so. lnpnd
//! runs on mimalloc, whose `purge_decommits` option defaults to on
//! (the options table of libmimalloc-sys 0.1.49), so its purges are
//! `MADV_DONTNEED` and leave the resident set at once: there the figure is
//! expected to stay near 0, and a dip that is not lazy-free must be read
//! from `majfault=` and swap instead.
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
    /// `RssFile:` in bytes: resident pages mapped from files.
    pub rss_file_bytes: Option<u64>,
    /// `RssShmem:` in bytes: resident shared pages, tmpfs files included.
    pub rss_shmem_bytes: Option<u64>,
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
            "RssFile" => out.rss_file_bytes = value.map(|kb: u64| kb * 1024),
            "RssShmem" => out.rss_shmem_bytes = value.map(|kb: u64| kb * 1024),
            _ => {}
        }
    }
    out
}

/// The reclaim witnesses the `gap:` line prints but does not subtract.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReclaimFields {
    /// `LazyFree:` of `/proc/self/smaps_rollup` in bytes; 0 where the
    /// kernel does not report it.
    pub lazyfree_bytes: u64,
    /// Major page faults since start (`/proc/self/stat` field 12).
    pub majfault: Option<u64>,
}

/// `LazyFree:` of `/proc/self/smaps_rollup` text, in bytes; 0 when the
/// kernel prints no such line (older kernels, or no rollup at all).
pub fn parse_lazyfree(smaps_rollup: &str) -> u64 {
    smaps_rollup
        .lines()
        .find_map(|line| {
            let rest = line.strip_prefix("LazyFree:")?;
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            kb.checked_mul(1024)
        })
        .unwrap_or(0)
}

/// Field 12 (`majflt`) of `/proc/self/stat` text.
///
/// The second field is the command name in parentheses and may itself hold
/// spaces and parentheses, so counting starts after the LAST `)`: the field
/// that follows it is field 3, which puts field 12 nine further on.
pub fn parse_majfault(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(12 - 3)?.parse().ok()
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

/// The glibc allocator's balance sheet (`mallinfo2()`, or `mallinfo()` on a
/// glibc older than 2.33), in bytes except the two counts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AllocatorView {
    /// Which glibc call the numbers came from.
    pub source: AllocatorSource,
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

/// The glibc call an [`AllocatorView`] was read with.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AllocatorSource {
    /// `mallinfo2()`, glibc 2.33 and later: `size_t` fields.
    #[default]
    Mallinfo2,
    /// `mallinfo()`, the fallback on an older glibc: `int` fields, read
    /// here as unsigned, so a total wraps once it passes 4 GiB.
    Mallinfo,
}

/// The allocator's view, where the libc has one to give.
///
/// Both calls are looked up at run time, never linked by name: the packages
/// link against glibc 2.31 (`GLIBC_FLOOR` in scripts/build-deb.sh) and
/// `mallinfo2` arrived in 2.33, so a direct call would not link there
/// (order 1674: the desktop loquax pulled this census in through the mailbox
/// and every nightly package build failed on `undefined symbol: mallinfo2`).
/// Whether the running glibc has it is a fact about the host the binary runs
/// on, not about the one it was built on.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn allocator_view() -> Option<AllocatorView> {
    allocator_view_via(|name| {
        // SAFETY: dlsym with RTLD_DEFAULT and a NUL-terminated name only
        // reads the loaded objects' symbol tables.
        unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }
    })
}

/// [`allocator_view`] over a symbol lookup, so a test can play a glibc
/// without `mallinfo2`. `lookup` must answer null or the address of the
/// glibc function of that name.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn allocator_view_via(
    lookup: impl Fn(&std::ffi::CStr) -> *mut libc::c_void,
) -> Option<AllocatorView> {
    type Mallinfo2Fn = unsafe extern "C" fn() -> libc::mallinfo2;
    type MallinfoFn = unsafe extern "C" fn() -> libc::mallinfo;

    let sym = lookup(c"mallinfo2");
    if !sym.is_null() {
        // SAFETY: glibc's mallinfo2 has exactly this signature (malloc.h:
        // `struct mallinfo2 mallinfo2 (void)`), and libc::mallinfo2 mirrors
        // that struct. It takes no arguments and walks the arenas under
        // their own locks.
        let m = unsafe { std::mem::transmute::<*mut libc::c_void, Mallinfo2Fn>(sym)() };
        return Some(AllocatorView {
            source: AllocatorSource::Mallinfo2,
            arena: m.arena as u64,
            ordblks: m.ordblks as u64,
            hblks: m.hblks as u64,
            hblkhd: m.hblkhd as u64,
            uordblks: m.uordblks as u64,
            fordblks: m.fordblks as u64,
            keepcost: m.keepcost as u64,
        });
    }
    let sym = lookup(c"mallinfo");
    if sym.is_null() {
        return None;
    }
    // SAFETY: glibc's mallinfo is `struct mallinfo mallinfo (void)` and
    // libc::mallinfo mirrors that struct.
    let m = unsafe { std::mem::transmute::<*mut libc::c_void, MallinfoFn>(sym)() };
    // The fields are C ints that wrap past 2 GiB; read them as the unsigned
    // counters they are, which is right up to 4 GiB.
    let u = |v: libc::c_int| u64::from(v as libc::c_uint);
    Some(AllocatorView {
        source: AllocatorSource::Mallinfo,
        arena: u(m.arena),
        ordblks: u(m.ordblks),
        hblks: u(m.hblks),
        hblkhd: u(m.hblkhd),
        uordblks: u(m.uordblks),
        fordblks: u(m.fordblks),
        keepcost: u(m.keepcost),
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
            let header = match v.source {
                AllocatorSource::Mallinfo2 => "glibc mallinfo2",
                AllocatorSource::Mallinfo => {
                    "glibc mallinfo (no mallinfo2 on this glibc; 32-bit counters)"
                }
            };
            let _ = writeln!(out, "=== Allocator: {header} ===");
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
                "allocator: unavailable (no libc totals to read; read the Mappings section)"
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
///
/// `status` gives swap (`VmSwap`) and the two terms taken off first.
/// The resident file pages (`RssFile`) go because reclaim drops them
/// without swapping (module docs). The resident shared pages (`RssShmem`)
/// go next: a file on tmpfs is booked there and leaves the line the same
/// way (module docs).
///
/// `reclaim` is printed as `lazyfree=<bytes> majfault=<n>` just before
/// `unaccounted`, which stays the line's last term because periculum reads
/// it from the end. Neither is subtracted (module docs).
///
/// `live` is the counting shim's balance ([`crate::heap_accounting`]), known
/// only in a `heap-accounting` build. When it is known it is subtracted
/// INSTEAD of `tracked`, which is then printed but not taken off: `tracked`
/// is a model of the same live owners, so taking both would count them
/// twice. What is left after `live` is allocator retention plus stacks, the
/// split #437 could not make on musl without it.
pub fn render_gap_line(
    rss: u64,
    status: &StatusFields,
    reclaim: ReclaimFields,
    tracked: Option<u64>,
    live: Option<u64>,
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
    let swap = status.swap_bytes;
    let swap_bytes = swap.unwrap_or(0);
    let mut left = rss + swap_bytes;
    let mut out = format!(
        "gap: rss {rss} + swap {} = {left}",
        swap.map_or_else(|| "n/a".to_string(), |s| s.to_string())
    );
    step(&mut out, &mut left, "file", status.rss_file_bytes);
    step(&mut out, &mut left, "shmem", status.rss_shmem_bytes);
    match (live, tracked) {
        (Some(_), Some(t)) => {
            let _ = write!(out, "; tracked {t} (model, not subtracted)");
        }
        (Some(_), None) => out.push_str("; tracked n/a"),
        (None, _) => step(&mut out, &mut left, "tracked", tracked),
    }
    step(&mut out, &mut left, "live", live);
    step(
        &mut out,
        &mut left,
        "allocator free",
        view.map(|v| v.fordblks + v.keepcost),
    );
    step(&mut out, &mut left, "mmapped", view.map(|v| v.hblkhd));
    let _ = write!(
        out,
        "; lazyfree={} majfault={}",
        reclaim.lazyfree_bytes,
        reclaim
            .majfault
            .map_or_else(|| "n/a".to_string(), |m| m.to_string())
    );
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
    let reclaim = ReclaimFields {
        lazyfree_bytes: std::fs::read_to_string("/proc/self/smaps_rollup")
            .map(|t| parse_lazyfree(&t))
            .unwrap_or(0),
        majfault: std::fs::read_to_string("/proc/self/stat")
            .ok()
            .as_deref()
            .and_then(parse_majfault),
    };
    let view = allocator_view();

    let mut out = render_allocator(view, status.threads);
    out.push_str(&render_mappings(&regions));
    out.push_str(&render_gap_line(
        rss,
        &status,
        reclaim,
        tracked,
        measured_live(),
        view,
    ));
    out
}

/// The shim's live balance, in a build whose binary installs it. The
/// feature is the only reliable witness: the counters are statics that
/// any caller of `CountingAllocator` moves, the unit tests included, so a
/// nonzero count does not prove the shim is the global allocator.
#[cfg(feature = "heap-accounting")]
fn measured_live() -> Option<u64> {
    Some(crate::heap_accounting::live_bytes() as u64)
}

#[cfg(not(feature = "heap-accounting"))]
fn measured_live() -> Option<u64> {
    None
}

/// `MemTotal` of a `/proc/meminfo` text, in bytes.
///
/// The host's RAM bounds lnpnd's store index (`lnpnd/src/store_limit.rs`)
/// and every transport table ([`crate::table_budget`]); both read it here.
pub fn parse_mem_total(meminfo: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?;
        let mut fields = rest.split_whitespace();
        let value: u64 = fields.next()?.parse().ok()?;
        // The kernel writes kB and means KiB.
        match fields.next() {
            Some("kB") => value.checked_mul(1024),
            None => Some(value),
            Some(_) => None,
        }
    })
}

/// The host's total RAM in bytes, `None` where `/proc/meminfo` cannot say.
pub fn host_ram_bytes() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .as_deref()
        .and_then(parse_mem_total)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "Name:\tlnsd\n\
        VmPeak:\t 1900000 kB\n\
        VmRSS:\t  907776 kB\n\
        VmData:\t 1841124 kB\n\
        RssFile:\t    9216 kB\n\
        RssShmem:\t     512 kB\n\
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
    fn status_parser_reads_threads_swap_and_file_pages() {
        let s = parse_status(STATUS);
        assert_eq!(s.threads, Some(13));
        assert_eq!(s.swap_bytes, Some(893_516 * 1024));
        assert_eq!(s.rss_file_bytes, Some(9_216 * 1024));
        assert_eq!(s.rss_shmem_bytes, Some(512 * 1024));
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

    /// A `smaps_rollup` as a 6.12 kernel writes it, with lazy-free pages.
    const SMAPS_ROLLUP: &str = "\
55ebce6cd000-7ffe26bb3000 ---p 00000000 00:00 0                          [rollup]
Rss:                9888 kB
Anonymous:          5120 kB
KSM:                   0 kB
LazyFree:           1536 kB
AnonHugePages:         0 kB
Swap:                356 kB
";

    /// A `/proc/self/stat` whose command name holds a space and a `)`.
    const STAT: &str = "4242 (ln d) x) S 1 4242 4242 0 -1 4194560 1830 0 17 0 \
        12 3 0 0 20 0 13 0 777 1900000000 2424 18446744073709551615\n";

    #[test]
    fn lazyfree_parser_reads_bytes_and_defaults_to_zero() {
        assert_eq!(parse_lazyfree(SMAPS_ROLLUP), 1536 * 1024);
        // A kernel without the line, or no rollup file at all.
        assert_eq!(parse_lazyfree("Rss:  100 kB\nSwap: 0 kB\n"), 0);
        assert_eq!(parse_lazyfree(""), 0);
    }

    #[test]
    fn majfault_parser_counts_fields_after_the_last_paren() {
        // minflt (10) is 1830, cminflt (11) 0, majflt (12) 17.
        assert_eq!(parse_majfault(STAT), Some(17));
        assert_eq!(parse_majfault("1 (x) S 1 2"), None);
        assert_eq!(parse_majfault("no parens at all"), None);
    }

    /// The fields of the old positional arguments, as a status block.
    fn status(swap: Option<u64>, file: Option<u64>, shmem: Option<u64>) -> StatusFields {
        StatusFields {
            threads: None,
            swap_bytes: swap,
            rss_file_bytes: file,
            rss_shmem_bytes: shmem,
        }
    }

    /// Order 1637: the reclaim witnesses ride on the line before
    /// `unaccounted`, and move nothing in the arithmetic.
    #[test]
    fn gap_line_prints_lazyfree_and_majfault_without_subtracting() {
        let reclaim = ReclaimFields {
            lazyfree_bytes: 300,
            majfault: Some(17),
        };
        let line = render_gap_line(
            1000,
            &status(Some(500), Some(100), Some(50)),
            reclaim,
            Some(400),
            None,
            None,
        );
        assert_eq!(
            line,
            "gap: rss 1000 + swap 500 = 1500; - file 100 = 1400; - shmem 50 = 1350; \
             - tracked 400 = 950; - live n/a; - allocator free n/a; - mmapped n/a; \
             lazyfree=300 majfault=17; unaccounted 950\n"
        );
        assert_eq!(unaccounted_of(&line), 950);
        let unknown = render_gap_line(
            1000,
            &status(None, None, None),
            ReclaimFields::default(),
            None,
            None,
            None,
        );
        assert!(
            unknown.ends_with("; lazyfree=0 majfault=n/a; unaccounted 1000\n"),
            "{unknown}"
        );
    }

    /// The live process: the line carries both figures, and on Linux the
    /// stat file always gives a fault count.
    #[cfg(target_os = "linux")]
    #[test]
    fn census_gap_line_carries_the_reclaim_witnesses() {
        let out = census_sections(1 << 20, Some(1024));
        let gap = out
            .lines()
            .find(|l| l.starts_with("gap: "))
            .unwrap_or_default();
        assert!(gap.contains("; lazyfree="), "{gap}");
        assert!(!gap.contains("majfault=n/a"), "{gap}");
        assert!(gap.contains("; unaccounted "), "{gap}");
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
            render_gap_line(
                1000,
                &status(Some(500), Some(100), Some(50)),
                ReclaimFields::default(),
                Some(400),
                None,
                Some(view)
            ),
            "gap: rss 1000 + swap 500 = 1500; - file 100 = 1400; - shmem 50 = 1350; \
             - tracked 400 = 950; - live n/a; - allocator free 120 = 830; - mmapped 300 = 530; \
             lazyfree=0 majfault=n/a; unaccounted 530\n"
        );
        assert_eq!(
            render_gap_line(
                1000,
                &status(None, None, None),
                ReclaimFields::default(),
                Some(400),
                None,
                None
            ),
            "gap: rss 1000 + swap n/a = 1000; - file n/a; - shmem n/a; - tracked 400 = 600; \
             - live n/a; - allocator free n/a; - mmapped n/a; lazyfree=0 majfault=n/a; \
             unaccounted 600\n"
        );
    }

    /// A heap-accounting build: the measured balance replaces the model in
    /// the subtraction, and the model stays on the line to be read against
    /// it. The musl shape, no allocator books, is the one #437 needs.
    #[test]
    fn gap_line_subtracts_live_instead_of_tracked_when_measured() {
        assert_eq!(
            render_gap_line(
                1000,
                &status(Some(500), Some(100), Some(0)),
                ReclaimFields::default(),
                Some(400),
                Some(250),
                None
            ),
            "gap: rss 1000 + swap 500 = 1500; - file 100 = 1400; - shmem 0 = 1400; \
             tracked 400 (model, not subtracted); - live 250 = 1150; \
             - allocator free n/a; - mmapped n/a; lazyfree=0 majfault=n/a; unaccounted 1150\n"
        );
        assert_eq!(
            render_gap_line(
                1000,
                &status(None, None, None),
                ReclaimFields::default(),
                None,
                Some(250),
                None
            ),
            "gap: rss 1000 + swap n/a = 1000; - file n/a; - shmem n/a; tracked n/a; \
             - live 250 = 750; - allocator free n/a; - mmapped n/a; lazyfree=0 majfault=n/a; \
             unaccounted 750\n"
        );
    }

    /// The feature decides which shape `census_sections` prints.
    #[test]
    fn census_gap_line_shape_follows_the_feature() {
        let out = census_sections(1 << 20, Some(1024));
        let gap = out
            .lines()
            .find(|l| l.starts_with("gap: "))
            .unwrap_or_default();
        if cfg!(feature = "heap-accounting") {
            assert!(
                gap.contains("; tracked 1024 (model, not subtracted); - live "),
                "{gap}"
            );
        } else {
            assert!(gap.contains("; - tracked 1024 = "), "{gap}");
            assert!(gap.contains("; - live n/a;"), "{gap}");
        }
    }

    /// The `unaccounted` figure of a census line, as periculum reads it.
    fn unaccounted_of(sections: &str) -> u64 {
        sections
            .lines()
            .find(|l| l.starts_with("gap: "))
            .and_then(|l| l.rsplit_once("; unaccounted "))
            .and_then(|(_, v)| v.trim().parse().ok())
            .unwrap_or_else(|| panic!("no unaccounted figure: {sections}"))
    }

    /// The resident set from `/proc/self/statm`, the source the dump uses.
    fn statm_rss() -> u64 {
        let statm = std::fs::read_to_string("/proc/self/statm").expect("statm");
        let pages: u64 = statm
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse().ok())
            .expect("statm rss field");
        pages * 4096
    }

    /// Codeberg #478, order 1253: the canary went red at 15.6 MB/h on
    /// lnpnd's unaccounted bytes while its live heap and VmData held. The
    /// host was short of memory; reclaim took lnpnd's resident set from
    /// 10.0 to 1.9 MB and its swap rose 0.56 MB, so ~7.5 MB of it were
    /// file pages (the binary's text) that leave without going to swap and
    /// fault back in as the daemon runs: 3.8 to 6.6 MB in the window.
    ///
    /// `MADV_DONTNEED` on a private file mapping does to this process's
    /// resident set what reclaim does to file pages, so the test maps a
    /// file on disk (this test binary), faults it in, drops it, and holds
    /// the unaccounted figure to anonymous memory: it must not follow.
    ///
    /// Order 1372: on the forge (pipelines 602/603) the unaccounted figure
    /// followed the drop. The same binary copied to `/dev/shm` failed here
    /// the same way, its pages booked as `RssShmem`, which the census did
    /// not subtract then. The test now proves first that the kernel booked
    /// the drop under a term the census reads, and prints every figure when
    /// red.
    #[cfg(target_os = "linux")]
    #[test]
    fn unaccounted_does_not_follow_file_pages_out_of_the_resident_set() {
        use std::os::fd::AsRawFd;
        const CAP: usize = 64 << 20;
        let exe = std::env::current_exe().expect("test binary path");
        let file = std::fs::File::open(&exe).expect("open test binary");
        let len = (file.metadata().expect("metadata").len() as usize).min(CAP);
        let page = 4096;
        let len = len / page * page;
        assert!(len >= 8 << 20, "test binary too small to measure: {len}");
        // SAFETY: a fresh read-only private mapping of a file we hold open;
        // it is only read below, and unmapped before the file is dropped.
        let base = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        assert_ne!(base, libc::MAP_FAILED, "mmap");
        let mut sum = 0u64;
        for off in (0..len).step_by(page) {
            // SAFETY: off < len, inside the mapping made above.
            sum = sum.wrapping_add(unsafe { *(base as *const u8).add(off) } as u64);
        }
        std::hint::black_box(sum);

        let status_in = std::fs::read_to_string("/proc/self/status").expect("status");
        let rss_in = statm_rss();
        let sections_in = census_sections(rss_in, None);
        // SAFETY: the range is exactly the mapping made above.
        let rc = unsafe { libc::madvise(base, len, libc::MADV_DONTNEED) };
        assert_eq!(rc, 0, "madvise");
        let status_out = std::fs::read_to_string("/proc/self/status").expect("status");
        let rss_out = statm_rss();
        let sections_out = census_sections(rss_out, None);
        // SAFETY: unmapping the mapping made above, once.
        unsafe { libc::munmap(base, len) };

        let len = len as u64;
        // Everything a reader of a red run needs: both gap lines and every
        // resident-set field the kernel gave, before and after the drop.
        let rss_lines = |status: &str| {
            status
                .lines()
                .filter(|l| {
                    l.starts_with("Rss") || l.starts_with("VmRSS") || l.starts_with("VmSwap")
                })
                .collect::<Vec<_>>()
                .join(" | ")
        };
        let gap_line = |sections: &str| {
            sections
                .lines()
                .find(|l| l.starts_with("gap: "))
                .unwrap_or_default()
                .to_string()
        };
        let diag = format!(
            "mapped {len} bytes of {}\n  in:  statm {rss_in}; {}\n       {}\n  out: statm {rss_out}; {}\n       {}",
            exe.display(),
            rss_lines(&status_in),
            gap_line(&sections_in),
            rss_lines(&status_out),
            gap_line(&sections_out),
        );
        // The positive control, part one: the resident set did lose the pages.
        assert!(
            rss_in.saturating_sub(rss_out) >= len * 3 / 4,
            "the drop did not leave the resident set\n{diag}"
        );
        // Part two: the kernel booked them where the census looks, under
        // RssFile for a file on disk or RssShmem for one on tmpfs. A kernel
        // that books them under neither (or prints neither line) gives the
        // census nothing to subtract, which is a statement about the kernel,
        // not about the subtraction this test holds.
        let (st_in, st_out) = (parse_status(&status_in), parse_status(&status_out));
        let moved = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(a), Some(b)) => a.saturating_sub(b),
            _ => 0,
        };
        let booked = moved(st_in.rss_file_bytes, st_out.rss_file_bytes)
            + moved(st_in.rss_shmem_bytes, st_out.rss_shmem_bytes);
        if booked < len * 3 / 4 {
            eprintln!(
                "skip: the kernel booked the dropped file pages under neither RssFile nor \
                 RssShmem ({booked} of {len} bytes moved), so the census has no term to \
                 subtract them by\n{diag}"
            );
            return;
        }
        let in_set = unaccounted_of(&sections_in);
        let out_of_set = unaccounted_of(&sections_out);
        // Other tests allocate beside this one; a quarter of the mapping is
        // far above that and far below the pages that left.
        assert!(
            in_set.abs_diff(out_of_set) < len / 4,
            "unaccounted followed {len} bytes of file pages: {in_set} -> {out_of_set}\n{diag}"
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
        assert!(some.starts_with("=== Allocator: glibc mallinfo2 ===\n"));
        let old = render_allocator(
            Some(AllocatorView {
                source: AllocatorSource::Mallinfo,
                ..AllocatorView::default()
            }),
            None,
        );
        assert!(old.starts_with(
            "=== Allocator: glibc mallinfo (no mallinfo2 on this glibc; 32-bit counters) ===\n"
        ));
    }

    /// A gnu build resolves mallinfo2 at run time (every host the suite
    /// runs on has glibc 2.33 or later) and reads a real arena: a lookup
    /// that silently fell through to the fallback, or to None, would hide
    /// the allocator's numbers on every current host.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn allocator_view_resolves_mallinfo2_at_run_time() {
        let keep = vec![0u8; 1 << 16];
        let view = allocator_view();
        drop(keep);
        let Some(view) = view else {
            panic!("no allocator view on a glibc host");
        };
        assert_eq!(view.source, AllocatorSource::Mallinfo2);
        assert!(view.arena > 0 || view.hblkhd > 0, "{view:?}");
    }

    /// A glibc older than 2.33 (the package floor is 2.31) has no
    /// mallinfo2: the view falls back to mallinfo and says so, and a libc
    /// with neither gives None rather than zeros.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn allocator_view_falls_back_to_mallinfo_without_mallinfo2() {
        let real = |name: &std::ffi::CStr| {
            // SAFETY: as in allocator_view.
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }
        };
        let keep = vec![0u8; 1 << 16];
        let old = allocator_view_via(|name| {
            if name == c"mallinfo2" {
                std::ptr::null_mut()
            } else {
                real(name)
            }
        });
        drop(keep);
        let Some(old) = old else {
            panic!("mallinfo is in every glibc");
        };
        assert_eq!(old.source, AllocatorSource::Mallinfo);
        assert!(old.arena > 0 || old.hblkhd > 0, "{old:?}");
        assert_eq!(allocator_view_via(|_| std::ptr::null_mut()), None);
    }
}
