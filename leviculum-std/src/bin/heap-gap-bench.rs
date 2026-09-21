//! `heap-gap-bench` — the instrument for "the RSS is four times the data".
//!
//! # Why this exists before any fix
//!
//! A field node's resident set cannot judge a memory fix. It is confounded
//! by the traffic it carries, by whoever queries it (a status RPC allocates
//! tens of megabytes to describe the tables, Codeberg #028), and by the
//! hourly flush. A sawtooth measured that way was, on 2026-09-21, the
//! measuring instrument and not the node.
//!
//! So this binary drives a node to a field-like table state from a fixed
//! seed, with no network and no wall-clock dependence in what it stores,
//! and reports the one ratio that matters:
//!
//! ```text
//! live data (the counting allocator)  vs  resident set (/proc/self/statm)
//! ```
//!
//! Same seed, same counts, same answer — run it twice and compare before
//! believing any fix.
//!
//! # What it is a model of, and what it is not
//!
//! The base load is **announce ingestion**, which is what fills the tables
//! that dominate a propagation node: `path_table`, `known_identities`,
//! `announce_cache`, `announce_rate_table`, `path_states` and the packet
//! dedup cache. A repeat phase re-announces the same identities with fresh
//! random hashes, so the churn of ongoing mesh traffic is present and not
//! only the one-off fill.
//!
//! Three further switches exist because the announce-only load reached a
//! resident-to-live ratio of 1.20 while the soak node sat at 4.4, and each
//! names one thing the bench was missing (2026-09-21):
//!
//! * `--forwards N` relays N data packets through this node, which is the
//!   only thing that fills `reverse_table` — one entry per forwarded packet,
//!   expiring after 8 minutes. The field node carried 55 047 of them against
//!   11 511 paths, and it is the one table whose size is set by traffic
//!   rather than by destinations.
//! * `--links N` relays N link requests, filling `link_table` (352 in the
//!   field) and `reverse_table` with them.
//! * `--flushes N` runs the storage flush N times across the run. The flush
//!   is not free and not transient-free: it folds every known identity into
//!   `known_dest_entries`, which a bench without it never allocates at all,
//!   and clones that map plus both dedup generations while it writes.
//!
//! `--progress N` prints the ratio every N packets, which is how a run
//! answers whether a gap accumulates or appears at a threshold.
//!
//! # The allocator this measures
//!
//! The workspace builds for `x86_64-unknown-linux-musl` by default
//! (`.cargo/config.toml`), so a plain `cargo build` gives the same mallocng
//! the field nodes run. The target triple is printed on the header line;
//! a run on a glibc target measures a different allocator and is not
//! comparable to a musl one.
//!
//! A **gnu** build is nonetheless worth having, for one reason: valgrind's
//! massif works on it and cannot work on the musl-static binary (symbol
//! interposition has nothing to interpose on, and massif records
//! `mem_heap_B=0`). massif answers a question this binary's counters cannot
//! — *which call site* the live bytes came from:
//!
//! ```text
//! RUSTFLAGS=-g cargo build --release --target x86_64-unknown-linux-gnu \
//!   --bin heap-gap-bench
//! valgrind --tool=massif --time-unit=B --depth=20 --detailed-freq=5 \
//!   --alloc-fn=__rustc::__rust_alloc --alloc-fn=__rust_alloc --alloc-fn=malloc \
//!   "--alloc-fn=<alloc::raw_vec::RawVecInner>::try_allocate_in" \
//!   "--alloc-fn=<alloc::raw_vec::RawVecInner>::finish_grow" \
//!   "--alloc-fn=<leviculum_std::heap_accounting::CountingAllocator as core::alloc::global::GlobalAlloc>::alloc" \
//!   "--alloc-fn=<leviculum_std::heap_accounting::CountingAllocator as core::alloc::global::GlobalAlloc>::alloc_zeroed" \
//!   "--alloc-fn=<leviculum_std::heap_accounting::CountingAllocator as core::alloc::global::GlobalAlloc>::realloc" \
//!   target/x86_64-unknown-linux-gnu/release/heap-gap-bench --announces 2000
//! ```
//!
//! Without that `--alloc-fn` list every tree collapses at one frame and says
//! nothing — and the three `CountingAllocator` lines are not optional: this
//! binary's allocations all pass through the shim, so without them 94.93 % of
//! the peak reads as `CountingAllocator::alloc` and the reference recipe
//! (which predates the shim moving into leviculum-std) is useless. With them
//! the tree names our own code, e.g. `handle_announce_inner`,
//! `Storage::take_flush_snapshot`, `encode_packet_hashlist`.
//!
//! And one number from such a run must never be quoted: the `rss_bytes` this
//! binary prints under valgrind includes valgrind's own footprint, which is
//! where a `factor=13.78` came from.
//!
//! # Reading mallocng's own books
//!
//! massif and the counting shim both answer "what did the program ask
//! for". Neither can answer "which size class is the resident set sitting
//! in", and that is the question a `rss / live` of 1.4 actually poses.
//! mallocng knows: `ctx.usage_by_class[]` is the slot capacity it holds per
//! class, and its meta areas carry one `struct meta` per group with the
//! avail/freed bitmasks that say how much of each group is live. A
//! musl-static binary carries both as local symbols, so gdb reads them with
//! no instrumentation in our code at all —
//! `scripts/mallocng-census.gdb` plus `scripts/mallocng-census.py` do
//! exactly that, once per `HEAPGAP_PROGRESS` line.
//!
//! The binary must be a plain `cargo build --release`: the profile strips
//! debuginfo, and it has to stay stripped, because with DWARF present gdb
//! resolves the hidden `__malloc_context` against the current frame's unit
//! and silently reads zeros.
//!
//! That is what named the 5 MB step of 2026-09-21, which no live-byte
//! series could see. Over the window `fed` 200 000 → 220 000 of
//! `--repeats 11 --tick-every 500`, RSS rose 3.71 MB against 0.37 MB of
//! live growth, and the census says where it went: the 192-byte class lost
//! 17 933 live slots while keeping all but 15 of its 2 018 groups — 3.41 MB
//! of resident, empty slots — and the 240-byte class gained 567 groups
//! (4.86 MB). A live-neutral migration of one per-destination allocation
//! across one size-class boundary, charged as 3.7 MB of resident set,
//! because mallocng returns a group only when the group is ENTIRELY free
//! (`okay_to_free`, reached from `nontrivial_free`, in musl 1.2.5's
//! mallocng `free.c` — not a path in this repo, so no line citation). The same run
//! on the gnu build has no step: RSS flat at 40 603 648 from `fed` 74 000 to
//! 204 000, with a live series matching musl's to 0.03 %.
//!
//! # Usage
//!
//! ```text
//! cargo build --release --bin heap-gap-bench
//! target/x86_64-unknown-linux-musl/release/heap-gap-bench
//! target/.../heap-gap-bench --announces 40000 --repeats 3 --no-event-log
//! target/.../heap-gap-bench --forwards 55047 --links 352 --flushes 21
//! ```
//!
//! Output is three greppable lines (`HEAPGAP*`), plus one `HEAPGAP_PROGRESS`
//! per progress step; `--dump` adds the raw `diagnostic_dump` text on stderr.

use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use sha2::{Digest, Sha256};

use leviculum_core::constants::RANDOM_HASHBYTES;
use leviculum_core::identity::Identity;
use leviculum_core::node::NodeCoreBuilder;
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::traits::Storage as _;
use leviculum_core::{Destination, DestinationType, Direction, InterfaceId};
use leviculum_std::driver::{StdClock, StdNodeCore, StdStorage};
use leviculum_std::heap_accounting::{
    allocation_count, live_bytes, total_bytes, CountingAllocator,
};

/// The measurement itself. It counts, it does not replace: every call is
/// forwarded to the system allocator with the layout it arrived with, so
/// this binary runs the same mallocng a daemon does. See
/// [`leviculum_std::heap_accounting`] for why the counted number and not the
/// dump's modelled one is what `rss / live` may be built on.
#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator;

/// Interfaces the load arrives on. Two, so the announce path exercises the
/// "seen on another interface" branch rather than a single-interface
/// special case.
const IFACES: [usize; 2] = [0, 1];

/// Payload of a relayed data packet. A propagation node relays link and
/// resource traffic whose payloads run to a few hundred bytes, but nothing
/// downstream of the relay keeps the payload — `reverse_table` stores the
/// truncated hash and the two interface indices — so the size only has to
/// be plausible, not exact.
const RELAY_PAYLOAD_BYTES: usize = 64;

#[derive(Parser)]
#[command(
    name = "heap-gap-bench",
    about = "Drive a node to a field-like table state and report live data against RSS"
)]
struct Args {
    /// Distinct announcing identities (one path + one identity each).
    #[arg(long, default_value_t = 20_000)]
    announces: u32,

    /// Re-announce rounds over the same identities, with fresh random
    /// hashes: the ongoing-traffic churn, not the one-off fill.
    #[arg(long, default_value_t = 2)]
    repeats: u32,

    /// Data packets to relay through this node after the announce phases,
    /// spread over the announced destinations. One `reverse_table` entry
    /// each; 55 047 is what the soak node carried.
    #[arg(long, default_value_t = 0)]
    forwards: u32,

    /// Link requests to relay through this node, spread over the announced
    /// destinations. One `link_table` and one `reverse_table` entry each;
    /// 352 is what the soak node carried.
    #[arg(long, default_value_t = 0)]
    links: u32,

    /// Storage flushes to run across the whole load, evenly spaced. The
    /// soak node ran 21 in 23 hours.
    #[arg(long, default_value_t = 0)]
    flushes: u32,

    /// Print a `HEAPGAP_PROGRESS` line every N packets: the curve, which is
    /// what tells accumulation from a threshold. 0 disables.
    #[arg(long, default_value_t = 0)]
    progress: u64,

    /// Run the node's periodic maintenance every N packets, the way the
    /// driver's event loop does. It is what expires a `reverse_table` entry
    /// at 8 minutes and a path at its lifetime, so a run long enough to
    /// reach either only sees the expiry — and the allocate/free churn
    /// behind it — with this on. 0 disables, which is a node whose
    /// maintenance never runs and no field node at all.
    #[arg(long, default_value_t = 0)]
    tick_every: u64,

    /// Seed for the identity derivation. Same seed, same identities,
    /// same table contents.
    #[arg(long, default_value_t = 1)]
    seed: u64,

    /// Run without the structured event log installed, i.e. without the
    /// largest churn source the soak node has and a quiet node does not.
    #[arg(long)]
    no_event_log: bool,

    /// Hand event lines to the writer thread the way a daemon does,
    /// instead of writing them on the emitting thread.
    ///
    /// Off by default, and that is the one place this bench deliberately
    /// departs from production. The hand-off is asynchronous, so how much
    /// of the queue is still resident at the end depends on the scheduler:
    /// measured on this host, three runs of the same build came out at
    /// 10 645 504 bytes RSS to the byte with the writer thread out of the
    /// picture, and moved by a full 2 MiB between runs with it in. A
    /// 2 MiB jitter cannot judge a fix. The visitor churn the filter is
    /// about happens on the emitting thread either way, so the default
    /// keeps every allocation under study and drops only the noise.
    #[arg(long)]
    async_event_log: bool,

    /// Print the full `diagnostic_dump` text on stderr.
    #[arg(long)]
    dump: bool,

    /// Keep the working directory (storage + event log) instead of
    /// deleting it, and say where it is.
    #[arg(long)]
    keep: bool,

    /// Measure what the counting shim costs per allocation — the number a
    /// field deployment of it has to be judged on — and exit without
    /// driving a node.
    #[arg(long)]
    alloc_overhead: bool,
}

fn main() {
    let args = Args::parse();

    if args.alloc_overhead {
        alloc_overhead();
        return;
    }

    // A working directory under the system temp dir, named after the pid so
    // two concurrent runs cannot share storage.
    let work: PathBuf = std::env::temp_dir().join(format!("heap-gap-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("create work dir");

    // The subscriber install reads LEVICULUM_EVENT_LOG, so the variable has
    // to be set first and the install has to happen before the first event.
    if !args.no_event_log {
        std::env::set_var("LEVICULUM_EVENT_LOG", work.join("events.log"));
        if !args.async_event_log {
            std::env::set_var("LEVICULUM_EVENT_LOG_SYNC", "1");
        }
    }
    std::env::set_var("LEVICULUM_EVENT_NODE", "bench");
    leviculum_std::event_log::install_global_subscriber("info");

    let storage = StdStorage::new(work.join("storage")).expect("storage");
    let mut node = NodeCoreBuilder::new().enable_transport(true).build(
        rand_core::OsRng,
        StdClock::new(),
        storage,
    );
    for iface in IFACES {
        node.set_interface_name(iface, format!("bench{iface}"));
        // Ingress control off, which is what the driver resolves for a
        // point-to-point interface (Codeberg #8) -- and a TCP uplink is
        // exactly where a propagation node takes its announce firehose. Left
        // on, the burst limiter holds every announce past the first few,
        // because the bench delivers in seconds what a mesh delivers in
        // hours: the run would measure the limiter, not the tables.
        node.set_interface_ingress_control(iface, false);
    }
    let transport_id = *node.identity().hash();

    let started = Instant::now();
    let visits_before = leviculum_std::event_log::visit_count();

    // Every packet this run will feed, so the flushes and the progress
    // lines can be spaced across the whole of it and not across a phase.
    let total_packets = (args.repeats as u64 + 1) * args.announces as u64
        + args.forwards as u64
        + args.links as u64;
    let mut pace = Pacer::new(&args, total_packets, started);

    // Phase 1: one announce per identity — the fill.
    // Phase 2..: the same identities again with a fresh random hash — the
    // churn a node carrying other people's traffic never stops seeing.
    for round in 0..=args.repeats {
        for i in 0..args.announces {
            let identity = bench_identity(args.seed, i);
            let raw = announce_bytes(&identity, i, round);
            let iface = IFACES[(i as usize + round as usize) % IFACES.len()];
            let _ = node.handle_packet(InterfaceId(iface), &raw);
            pace.after_packet(&mut node, "announce");
        }
    }

    // Phase 3: relayed data packets. Grouped by destination so each
    // identity is derived once, which keeps the phase's cost in the node
    // and not in the key derivation.
    if args.forwards > 0 && args.announces > 0 {
        let per_dest = args.forwards.div_ceil(args.announces);
        let mut made = 0u32;
        'forwards: for i in 0..args.announces {
            let dest_hash = bench_dest_hash(args.seed, i);
            for k in 0..per_dest {
                if made >= args.forwards {
                    break 'forwards;
                }
                let raw = relay_bytes(
                    PacketType::Data,
                    &dest_hash,
                    &transport_id,
                    i,
                    k,
                    RELAY_PAYLOAD_BYTES,
                );
                let iface = IFACES[(i as usize + k as usize) % IFACES.len()];
                let _ = node.handle_packet(InterfaceId(iface), &raw);
                made += 1;
                pace.after_packet(&mut node, "forward");
            }
        }
    }

    // Phase 4: relayed link requests. Same shape, one link_table entry each.
    if args.links > 0 && args.announces > 0 {
        for j in 0..args.links {
            let i = j % args.announces;
            let dest_hash = bench_dest_hash(args.seed, i);
            // 64 bytes: the two public keys a link request carries. Nothing
            // on the relay path parses them, it stores the signing key it
            // already has from the cached announce.
            let raw = relay_bytes(
                PacketType::LinkRequest,
                &dest_hash,
                &transport_id,
                i,
                j.wrapping_add(1),
                64,
            );
            let iface = IFACES[j as usize % IFACES.len()];
            let _ = node.handle_packet(InterfaceId(iface), &raw);
            pace.after_packet(&mut node, "link");
        }
    }

    let elapsed_ms = started.elapsed().as_millis();
    let visits = leviculum_std::event_log::visit_count() - visits_before;

    // Flush before reading RSS: the writer thread holds queued lines and
    // its buffers, and a half-drained queue is a different resident set
    // every run.
    leviculum_std::event_log::flush_event_log(std::time::Duration::from_secs(10));

    let dump = node.diagnostic_dump();
    let modelled = parse_marker(&dump, "=== Total estimated: ").expect("total in dump");
    let live = live_bytes();
    let rss = rss_bytes().expect("RSS from /proc/self/statm");
    let maps = map_count();

    println!(
        "HEAPGAP target={} announces={} repeats={} forwards={} links={} flushes={} ticks={} seed={} event_log={} packets={}",
        target_triple(),
        args.announces,
        args.repeats,
        args.forwards,
        args.links,
        pace.flushes_done,
        pace.ticks_done,
        args.seed,
        match (args.no_event_log, args.async_event_log) {
            (true, _) => "off",
            (false, false) => "sync",
            (false, true) => "async",
        },
        pace.fed,
    );
    println!("HEAPGAP_TABLES {}", table_counts(&dump));
    println!(
        "HEAPGAP_RESULT live_bytes={} rss_bytes={} gap_bytes={} factor={:.2} modelled_bytes={} maps={} allocs={} churn_bytes={} visits={} elapsed_ms={}",
        live,
        rss,
        rss.saturating_sub(live as u64),
        rss as f64 / live.max(1) as f64,
        modelled,
        maps,
        allocation_count(),
        total_bytes(),
        visits,
        elapsed_ms,
    );

    if args.dump {
        eprintln!("{dump}");
    }

    if args.keep {
        println!("HEAPGAP_WORKDIR {}", work.display());
    } else {
        // The node still owns the storage handle; drop it first so nothing
        // writes into a directory that is being removed.
        drop(node);
        let _ = std::fs::remove_dir_all(&work);
    }
}

/// Counts packets and, on the strides asked for, runs a storage flush or
/// prints a progress line.
///
/// Both are spaced over the WHOLE load rather than over a phase, because
/// both model something the field node does by the clock: 21 flushes in 23
/// hours knows nothing about which phase the node is in.
struct Pacer {
    fed: u64,
    flush_stride: u64,
    next_flush: u64,
    flushes_done: u32,
    tick_stride: u64,
    next_tick: u64,
    ticks_done: u64,
    progress_stride: u64,
    next_progress: u64,
    started: Instant,
}

impl Pacer {
    fn new(args: &Args, total_packets: u64, started: Instant) -> Self {
        let flush_stride = if args.flushes == 0 {
            u64::MAX
        } else {
            (total_packets / args.flushes as u64).max(1)
        };
        let progress_stride = if args.progress == 0 {
            u64::MAX
        } else {
            args.progress
        };
        let tick_stride = if args.tick_every == 0 {
            u64::MAX
        } else {
            args.tick_every
        };
        Self {
            fed: 0,
            flush_stride,
            next_flush: flush_stride,
            flushes_done: 0,
            tick_stride,
            next_tick: tick_stride,
            ticks_done: 0,
            progress_stride,
            next_progress: progress_stride,
            started,
        }
    }

    fn after_packet(&mut self, node: &mut StdNodeCore, phase: &str) {
        self.fed += 1;
        if self.fed >= self.next_flush {
            // The daemon's periodic flush, minus the thread: `Storage::flush`
            // is the same three phases the event loop runs, back to back
            // (leviculum-std's `begin_flush`/`settle_flush` split them only
            // so the file IO leaves the node lock). The transient — the
            // clone of `known_dest_entries` and of both dedup generations,
            // alive while the write runs — is therefore the real one.
            node.storage_mut().flush();
            self.flushes_done += 1;
            self.next_flush = self.fed + self.flush_stride;
        }
        if self.fed >= self.next_tick {
            let _ = node.handle_timeout();
            self.ticks_done += 1;
            self.next_tick = self.fed + self.tick_stride;
        }
        if self.fed >= self.next_progress {
            let live = live_bytes();
            let rss = rss_bytes().unwrap_or(0);
            println!(
                "HEAPGAP_PROGRESS fed={} phase={} live_bytes={} rss_bytes={} factor={:.2} maps={} flushes={} ticks={} t_ms={}",
                self.fed,
                phase,
                live,
                rss,
                rss as f64 / live.max(1) as f64,
                map_count(),
                self.flushes_done,
                self.ticks_done,
                self.started.elapsed().as_millis(),
            );
            self.next_progress = self.fed + self.progress_stride;
        }
    }
}

/// What the two relaxed atomics in front of `System` cost, measured rather
/// than asserted: the same allocate-and-free loop run through the counting
/// global allocator and through `System` directly.
///
/// This is the number a field deployment of the shim is judged on, so it is
/// printed per allocation and the caller can multiply it by the allocation
/// rate the node actually runs at (the bench's own `allocs=` divided by
/// `elapsed_ms=` is one such rate).
fn alloc_overhead() {
    use std::alloc::{GlobalAlloc, Layout, System};

    // Sizes a node actually asks for: a table entry, a packet buffer, an
    // event-log line.
    const SIZES: [usize; 3] = [48, 512, 4096];
    const ITERS: usize = 200_000;
    // The two atomics are single-digit nanoseconds against a malloc/free pair
    // of forty-odd, and one timed loop each cannot see a difference that
    // small: measured that way the counted loop came out FASTER than the
    // plain one, which is the cache and the scheduler talking. So both loops
    // run ROUNDS times, alternating, and each is scored by its own minimum —
    // the round least disturbed by everything that is not the work.
    const ROUNDS: usize = 9;

    println!(
        "HEAPGAP_OVERHEAD target={} iters={} rounds={}",
        target_triple(),
        ITERS,
        ROUNDS
    );
    for size in SIZES {
        let layout = Layout::from_size_align(size, 8).expect("layout");
        // Warm the allocator's groups for this size class first, so the
        // first loop does not pay for what the second one inherits.
        for _ in 0..(ITERS / 10) {
            unsafe {
                let p = System.alloc(layout);
                System.dealloc(p, layout);
            }
        }

        let mut plain = u128::MAX;
        let mut counted = u128::MAX;
        for _ in 0..ROUNDS {
            let t0 = Instant::now();
            for _ in 0..ITERS {
                unsafe {
                    let p = System.alloc(layout);
                    std::hint::black_box(p).write(1);
                    System.dealloc(p, layout);
                }
            }
            plain = plain.min(t0.elapsed().as_nanos());

            let t1 = Instant::now();
            for _ in 0..ITERS {
                unsafe {
                    let p = ALLOC.alloc(layout);
                    std::hint::black_box(p).write(1);
                    ALLOC.dealloc(p, layout);
                }
            }
            counted = counted.min(t1.elapsed().as_nanos());
        }

        let per_plain = plain as f64 / ITERS as f64;
        let per_counted = counted as f64 / ITERS as f64;
        println!(
            "HEAPGAP_OVERHEAD size={} plain_ns={:.2} counted_ns={:.2} delta_ns={:.2} pct={:.1}",
            size,
            per_plain,
            per_counted,
            per_counted - per_plain,
            (per_counted - per_plain) / per_plain * 100.0,
        );
    }
}

/// Derive identity number `i` from `seed` by hashing, not by drawing from an
/// RNG: the bench must produce the same 20 000 identity hashes — and so the
/// same BTreeMap shapes and the same table contents — on every run and on
/// every host.
fn bench_identity(seed: u64, i: u32) -> Identity {
    let mut key = [0u8; 64];
    for (half, tag) in [(0usize, "x25519"), (32usize, "ed25519")] {
        let mut h = Sha256::new();
        h.update(b"heap-gap-bench");
        h.update(seed.to_le_bytes());
        h.update(i.to_le_bytes());
        h.update(tag.as_bytes());
        key[half..half + 32].copy_from_slice(&h.finalize());
    }
    Identity::from_private_key_bytes(&key).expect("64-byte key material")
}

/// The destination hash identity number `i` announced, recomputed rather
/// than remembered: a `Vec` of 20 000 hashes would be a third of a megabyte
/// of live bytes that the node under measurement never allocates.
fn bench_dest_hash(seed: u64, i: u32) -> [u8; leviculum_core::constants::TRUNCATED_HASHBYTES] {
    bench_destination(&bench_identity(seed, i))
        .hash()
        .into_bytes()
}

/// The one destination shape this bench announces, in one place, so the
/// announce and the traffic addressed at it cannot drift apart.
fn bench_destination(identity: &Identity) -> Destination {
    Destination::new(
        Some(identity.clone()),
        Direction::In,
        DestinationType::Single,
        "bench",
        &["node"],
    )
    .expect("destination")
}

/// A signed Single-destination announce for `identity`, as it would arrive
/// on the wire. `round` varies the random hash so a re-announce is a new
/// packet rather than a replay the dedup cache eats.
fn announce_bytes(identity: &Identity, i: u32, round: u32) -> Vec<u8> {
    let dest = bench_destination(identity);
    let id = dest.identity().expect("identity on destination");

    let mut random_hash = [0u8; RANDOM_HASHBYTES];
    let mut h = Sha256::new();
    h.update(i.to_le_bytes());
    h.update(round.to_le_bytes());
    random_hash.copy_from_slice(&h.finalize()[..RANDOM_HASHBYTES]);

    let app_data = b"heap-gap-bench";

    let mut signed = Vec::new();
    signed.extend_from_slice(dest.hash().as_bytes());
    signed.extend_from_slice(&id.public_key_bytes());
    signed.extend_from_slice(dest.name_hash());
    signed.extend_from_slice(&random_hash);
    signed.extend_from_slice(app_data);
    let signature = id.sign(&signed).expect("sign announce");

    let mut payload = Vec::new();
    payload.extend_from_slice(&id.public_key_bytes());
    payload.extend_from_slice(dest.name_hash());
    payload.extend_from_slice(&random_hash);
    payload.extend_from_slice(&signature);
    payload.extend_from_slice(app_data);

    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type1,
            context_flag: false,
            transport_type: TransportType::Broadcast,
            dest_type: DestinationType::Single,
            packet_type: PacketType::Announce,
        },
        hops: 1,
        transport_id: None,
        destination_hash: dest.hash().into_bytes(),
        context: PacketContext::None,
        data: PacketData::Owned(payload),
    };
    let mut buf = [0u8; 512];
    let len = packet.pack(&mut buf).expect("pack announce");
    buf[..len].to_vec()
}

/// A packet this node is the designated next hop for: HEADER_2 carrying our
/// own transport id, which is the only form a transport node may repeat
/// (`Transport.py:1559-1560`, and the `designated_hop` check in
/// `transport.rs`). An overheard HEADER_1 copy is dropped without touching a
/// table, so a bench that sent one would measure nothing.
///
/// `i` and `nonce` only have to make the payload unique: the packet hash is
/// the `reverse_table` key and, for a link request, the link id.
fn relay_bytes(
    packet_type: PacketType,
    dest_hash: &[u8; leviculum_core::constants::TRUNCATED_HASHBYTES],
    transport_id: &[u8; leviculum_core::constants::TRUNCATED_HASHBYTES],
    i: u32,
    nonce: u32,
    payload_len: usize,
) -> Vec<u8> {
    let mut payload = vec![0u8; payload_len];
    let mut h = Sha256::new();
    h.update(b"heap-gap-bench-relay");
    h.update(i.to_le_bytes());
    h.update(nonce.to_le_bytes());
    let seed = h.finalize();
    for (chunk, byte) in payload.chunks_mut(32).zip(0u8..) {
        let n = chunk.len().min(32);
        chunk[..n].copy_from_slice(&seed[..n]);
        // Distinguish the chunks so a long payload is not a repeated block.
        chunk[0] ^= byte;
    }

    let packet = Packet {
        flags: PacketFlags {
            ifac_flag: false,
            header_type: HeaderType::Type2,
            context_flag: false,
            transport_type: TransportType::Transport,
            dest_type: DestinationType::Single,
            packet_type,
        },
        hops: 1,
        transport_id: Some(*transport_id),
        destination_hash: *dest_hash,
        context: PacketContext::None,
        data: PacketData::Owned(payload),
    };
    let mut buf = [0u8; 512];
    let len = packet.pack(&mut buf).expect("pack relayed packet");
    buf[..len].to_vec()
}

/// Pull the `u64` that follows `marker` in the dump text.
fn parse_marker(dump: &str, marker: &str) -> Option<u64> {
    let rest = dump.split(marker).nth(1)?;
    rest.split_whitespace().next()?.parse().ok()
}

/// Reduce the dump to `name=entries` pairs, so a run's table state is one
/// greppable line next to its ratio. Without it a reader cannot tell a
/// smaller gap from a smaller load.
fn table_counts(dump: &str) -> String {
    let mut out = Vec::new();
    for line in dump.lines() {
        let Some((name, rest)) = line.split_once(": ") else {
            continue;
        };
        let Some(count) = rest.split_whitespace().next() else {
            continue;
        };
        if rest.contains("entries") && count.parse::<u64>().is_ok() {
            out.push(format!("{name}={count}"));
        }
    }
    out.join(" ")
}

/// Resident set from `/proc/self/statm` field 2 (pages), the same source
/// the daemon's own dump uses, so the two numbers are commensurable.
fn rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

/// Mapping count from `/proc/self/maps`. mallocng gives a group back with
/// `munmap` only when every slot in it is free, so the mapping count next
/// to the live bytes says how thinly the live objects are spread.
fn map_count() -> usize {
    std::fs::read_to_string("/proc/self/maps")
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// The allocator is half the measurement, so the run says which one it ran
/// on. A musl number and a glibc number are not comparable and the header
/// line is where a reader finds out which they are holding.
fn target_triple() -> String {
    let libc = if cfg!(target_env = "musl") {
        "musl"
    } else {
        "gnu"
    };
    format!("{}-{}-{libc}", std::env::consts::ARCH, std::env::consts::OS)
}
