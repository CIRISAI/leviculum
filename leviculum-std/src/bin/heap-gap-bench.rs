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
//! live data (diagnostic_dump)  vs  resident set (/proc/self/statm)
//! ```
//!
//! Same seed, same counts, same answer — run it twice and compare before
//! believing any fix.
//!
//! # What it is a model of, and what it is not
//!
//! The load is **announce ingestion**, which is what fills the tables that
//! dominate a propagation node: `path_table`, `known_identities`,
//! `announce_cache`, `announce_rate_table`, `path_states` and the packet
//! dedup cache. A repeat phase re-announces the same identities with fresh
//! random hashes, so the churn of ongoing mesh traffic is present and not
//! only the one-off fill.
//!
//! It does NOT populate `reverse_table` or `link_table`: those need relayed
//! traffic and live links, which need a second node and a driver. The field
//! node had 55 047 reverse entries against 11 511 paths, so the absolute
//! live number here is smaller than a field node's. That is fine for the
//! job: this bench is a DIFFERENCE instrument — the same load before and
//! after a change — not a replica of the soak node.
//!
//! # The allocator this measures
//!
//! The workspace builds for `x86_64-unknown-linux-musl` by default
//! (`.cargo/config.toml`), so a plain `cargo build` gives the same mallocng
//! the field nodes run. The target triple is printed on the header line;
//! a run on a glibc target measures a different allocator and is not
//! comparable to a musl one.
//!
//! # Usage
//!
//! ```text
//! cargo build --release --bin heap-gap-bench
//! target/x86_64-unknown-linux-musl/release/heap-gap-bench
//! target/.../heap-gap-bench --announces 40000 --repeats 3 --no-event-log
//! ```
//!
//! Output is three greppable lines (`HEAPGAP*`); `--dump` adds the raw
//! `diagnostic_dump` text on stderr.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use clap::Parser;
use sha2::{Digest, Sha256};

use leviculum_core::constants::RANDOM_HASHBYTES;
use leviculum_core::identity::Identity;
use leviculum_core::node::NodeCoreBuilder;
use leviculum_core::packet::{
    HeaderType, Packet, PacketContext, PacketData, PacketFlags, PacketType, TransportType,
};
use leviculum_core::{Destination, DestinationType, Direction, InterfaceId};
use leviculum_std::driver::{StdClock, StdStorage};

/// Bytes currently handed out by the allocator and not yet returned, and
/// the running totals behind them.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static TOTAL: AtomicUsize = AtomicUsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

/// A counting shim in FRONT of the system allocator, not a replacement for
/// it.
///
/// This matters, because "do not change the allocator" is a standing rule
/// for this investigation and this does not break it: every call is
/// forwarded to `System` with the layout it arrived with, so the binary
/// runs the same musl mallocng, takes the same code paths and produces the
/// same group layout as one without the shim. What it adds is two relaxed
/// atomics per call.
///
/// It exists because the alternative measurement is not good enough.
/// `diagnostic_dump` prices collections with flat multipliers (3x for a
/// BTreeMap, 1.5x for a HashMap), which is a model, and a model cannot be
/// put on the other side of an equation from a measured resident set: any
/// gap it shows could be the model being wrong rather than the memory
/// being lost. `LIVE` is not a model. It is the sum of what the allocator
/// gave out minus what it took back, so `rss / live` is a fragmentation
/// number and nothing else.
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() {
            record(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // Forwarded rather than left to the default (alloc + memset): a
        // large zeroed block is a fresh mmap in mallocng and already zero,
        // and rewriting it by hand would touch pages production does not.
        let p = System.alloc_zeroed(layout);
        if !p.is_null() {
            record(layout.size());
        }
        p
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = System.realloc(ptr, layout, new_size);
        if !p.is_null() {
            LIVE.fetch_add(new_size.wrapping_sub(layout.size()), Ordering::Relaxed);
            TOTAL.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

/// `LIVE` is only ever read when the true balance is non-negative, so the
/// wrapping subtraction in `dealloc` and `realloc` costs nothing and saves
/// a signed type.
fn record(size: usize) {
    LIVE.fetch_add(size, Ordering::Relaxed);
    TOTAL.fetch_add(size, Ordering::Relaxed);
    ALLOCS.fetch_add(1, Ordering::Relaxed);
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Interfaces the load arrives on. Two, so the announce path exercises the
/// "seen on another interface" branch rather than a single-interface
/// special case.
const IFACES: [usize; 2] = [0, 1];

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
}

fn main() {
    let args = Args::parse();

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

    let started = Instant::now();
    let visits_before = leviculum_std::event_log::visit_count();

    // Phase 1: one announce per identity — the fill.
    // Phase 2..: the same identities again with a fresh random hash — the
    // churn a node carrying other people's traffic never stops seeing.
    let mut fed = 0u64;
    for round in 0..=args.repeats {
        for i in 0..args.announces {
            let identity = bench_identity(args.seed, i);
            let raw = announce_bytes(&identity, i, round);
            let iface = IFACES[(i as usize + round as usize) % IFACES.len()];
            let _ = node.handle_packet(InterfaceId(iface), &raw);
            fed += 1;
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
    let live = LIVE.load(Ordering::Relaxed);
    let rss = rss_bytes().expect("RSS from /proc/self/statm");
    let maps = map_count();

    println!(
        "HEAPGAP target={} announces={} repeats={} seed={} event_log={} packets={}",
        target_triple(),
        args.announces,
        args.repeats,
        args.seed,
        match (args.no_event_log, args.async_event_log) {
            (true, _) => "off",
            (false, false) => "sync",
            (false, true) => "async",
        },
        fed,
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
        ALLOCS.load(Ordering::Relaxed),
        TOTAL.load(Ordering::Relaxed),
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

/// A signed Single-destination announce for `identity`, as it would arrive
/// on the wire. `round` varies the random hash so a re-announce is a new
/// packet rather than a replay the dedup cache eats.
fn announce_bytes(identity: &Identity, i: u32, round: u32) -> Vec<u8> {
    let dest = Destination::new(
        Some(identity.clone()),
        Direction::In,
        DestinationType::Single,
        "bench",
        &["node"],
    )
    .expect("destination");
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
