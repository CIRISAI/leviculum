//! Periodic `[TRANSPORT] ` counter line on the debug CDC (Codeberg #344).
//!
//! The core already counts every routing decision a transport node makes —
//! `TransportStats::packets_forwarded` and the per-reason drop counters
//! (`DropReason::NoPath`, `Duplicate`, `OverheardTransportId`,
//! `ForwardMaxHops`). None of it was reachable on hardware: the firmware
//! pulls `leviculum-core` with `default-features = false`
//! (`leviculum-nrf/Cargo.toml:17`), so the `tracing` feature is off and every
//! `debug!`/`trace!` in the core compiles to a no-op
//! (`leviculum-core/src/lib.rs:83-100`). A T114 that hears a packet it is
//! asked to relay and does not relay it could not say whether it dropped with
//! `NoPath`, deduped it, or handed it to a silent interface.
//!
//! Enabling `tracing` on the firmware is not the answer — the T114's stack
//! margin is ~13 KiB and every `.bss` byte comes out of it. This module reads
//! the counters that are already there and prints them; it adds no counter, no
//! allocation, and no hashing.
//!
//! ```text
//! [TRANSPORT] fwd=<n> link_fwd=<n> rx=<n> tx=<n> nopath=<n> dup=<n> overheard=<n> maxhops=<n> paths=<n> lora_stale=<n>
//! [TRANSPORT] iface=<name> rxb=<n> txb=<n>
//! ```
//!
//! `link_fwd=` is the link-addressed share of `fwd=`: packets repeated by a
//! link-table lookup (dest type LINK, never the node's own link). A relay's
//! capture can then say how much of what it keys is somebody else's link
//! traffic — the question the field day of 2026-09-27 could not answer,
//! where the airtime that engaged the Pocket's duty lock had to be
//! attributed frame prefix by frame prefix (order 368). Type-blind: the
//! counter sits at the transport's central forward site and carries no
//! policy.
//!
//! `lora_stale=` is the one field on the line that does not come from the
//! core, and it cannot: it counts frames the LoRa interface threw away after
//! holding them too long behind the regulatory airtime lock (Codeberg #433),
//! and the hold is invisible above the interface boundary by design
//! (CLAUDE.md, "Architecture: interface isolation"). It is on this line rather
//! than a line of its own because the question it answers is the same question
//! `nopath` and `dup` answer — where did the packets go — and a reader
//! correlating an interface-level loss against the core's drops should not have
//! to align two cadences to do it. Source:
//! [`crate::lora::LORA_STALE_DROP_COUNT`].
//!
//! `paths=` is on the same line on purpose: `nopath` against a populated path
//! table and `nopath` against an empty one are different diagnoses, and one
//! line that carries both cannot be correlated wrongly.
//!
//! The byte counters (Codeberg #235) get **one line per interface** rather
//! than six more fields on the totals line: the totals line is already at the
//! width a serial capture wraps, and a per-interface line is what
//! `grep 'iface=lora_sx1262'` turns into a single carrier's time series.
//! They come from [`crate::iface_bytes`], which is also what the `/status`
//! bundle answers `rnstatus -R` with, so the instrument and the remote answer
//! agree by construction rather than by review.

use embassy_time::{Duration, Instant};
use leviculum_core::node::NodeCore;
use leviculum_core::traits::{Clock, Storage};
use rand_core::CryptoRngCore;

/// How often the `[TRANSPORT]` line is emitted.
///
/// 30 s, the same cadence as the `[HEAP]` line (`lib.rs:254`), so the two
/// periodic counter lines interleave predictably in a capture. The events
/// being diagnosed are minutes apart (a 15-minute management-announce
/// heartbeat, a report every few minutes), so 30 s puts ~30 samples inside
/// one heartbeat interval — enough to place a counter step within 30 s of the
/// transmission that caused it, which is what separates "the relay never saw
/// it" from "the relay saw it and dropped it".
///
/// Cost: two wake-ups per minute against the 2 s `[STACK]` task and the 5 s
/// `[FW_BUILD]` banner already running, i.e. invisible; and ~80 bytes per
/// 30 s on the debug port, ~3 B/s, against an 8 KiB log ring.
pub const PERIOD: Duration = Duration::from_secs(30);

/// Emission schedule for the `[TRANSPORT]` line.
///
/// Deliberately NOT an `#[embassy_executor::task]`: the counters live inside
/// the `NodeCore`, which the main loop owns exclusively, and a spawned task
/// could only read them through a mirrored set of atomics — new state, kept in
/// sync by hand, for a value the main loop already holds. Instead the ticker
/// rides the loop it belongs to: [`Self::deadline`] clamps the loop's select
/// deadline so a quiet channel still wakes on schedule, and [`Self::poll`]
/// emits at the top of the next iteration.
pub struct Ticker {
    next: Instant,
}

impl Ticker {
    /// Arm the first emission one [`PERIOD`] from now. Called once, right
    /// before the main loop is entered.
    pub fn new() -> Self {
        Self {
            next: Instant::now() + PERIOD,
        }
    }

    /// The instant the loop must wake by for the next line to be on time.
    /// Fold it into the select deadline with `.min()`.
    pub fn deadline(&self) -> Instant {
        self.next
    }

    /// Emit the line if it is due, and re-arm. Cheap and safe to call on
    /// every loop iteration: the common case is one `Instant` comparison.
    pub fn poll<R, C, S>(&mut self, node: &NodeCore<R, C, S>)
    where
        R: CryptoRngCore,
        C: Clock,
        S: Storage,
    {
        let now = Instant::now();
        if now < self.next {
            return;
        }
        // Re-arm from `now`, not from `self.next`: a loop that was busy for
        // longer than a period must not then emit a burst of catch-up lines.
        self.next = now + PERIOD;
        // One sampler, one cadence: the speed window a `/status` answer
        // reports is the window whose bytes were just printed.
        crate::iface_bytes::sample_all(now.as_millis());
        log(node);
    }
}

impl Default for Ticker {
    fn default() -> Self {
        Self::new()
    }
}

/// Format one `[TRANSPORT]` line from the node's current counters.
///
/// Counter reads only — every value below already had a public accessor on
/// `TransportStats` (`transport.rs:1150ff`) or on `NodeCore`; this function
/// adds nothing to what the core measures.
pub fn log<R, C, S>(node: &NodeCore<R, C, S>)
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    let stats = node.transport_stats();
    crate::log::log_fmt(
        "[TRANSPORT] ",
        format_args!(
            "fwd={} link_fwd={} rx={} tx={} nopath={} dup={} overheard={} maxhops={} paths={} lora_stale={}",
            stats.packets_forwarded(),
            stats.packets_forwarded_link(),
            stats.packets_received(),
            stats.packets_sent(),
            stats.drops_no_path(),
            stats.drops_duplicate(),
            stats.drops_overheard_transport_id(),
            stats.drops_forward_max_hops(),
            node.path_count(),
            crate::lora::LORA_STALE_DROP_COUNT.load(core::sync::atomic::Ordering::Relaxed),
        ),
    );
    for (id, name) in crate::iface_bytes::NAMES.iter().enumerate() {
        let Some(counters) = crate::iface_bytes::counters(id) else {
            continue;
        };
        let (rxb, txb) = counters.totals();
        crate::log::log_fmt(
            "[TRANSPORT] ",
            format_args!("iface={name} rxb={rxb} txb={txb}"),
        );
    }
}
