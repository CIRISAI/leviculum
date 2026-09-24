//! Phase 2 of `lnstest selftest` against a reference daemon whose announce
//! cap swallowed the first announce.
//!
//! # What the rig saw
//!
//! Both Python arms of the ratchet family died in Phase 2 in two of the five
//! runs of 2026-09-23 (151, 179). The tool announced each client destination
//! exactly once and then waited on mutual discovery; `rnsd` put the client's
//! announce in the outbound interface's announce queue ("in 17.77s"), aired
//! the older entry, and never aired the client's — no error line, no retry.
//! That queue is the reference's, not ours, and nothing we ship can reach
//! into it. What we ship is the tool, and a second announce one cap slot
//! later is an ordinary announce the reference accepts.
//!
//! # What this test does, and why it is shaped this way
//!
//! Two daemons. `d1` carries the capped hop: a `TCPClientInterface` to `d2`
//! at [`CAPPED_BITRATE`], where the reference's 2 % announce cap
//! (`ANNOUNCE_CAP`, `reference/Reticulum/RNS/Reticulum.py:114`) turns one
//! announce into a slot of roughly twenty seconds. A filler node attached to
//! `d1` announces once before the tool starts; that announce airs, sets
//! `announce_allowed_at` on the capped interface
//! (`reference/Reticulum/RNS/Transport.py:1259`) and is confirmed by `d2`'s
//! path table. From then on the hop is shut to announces for one slot, and
//! the shutness pre-dates the tool — which is what makes this deterministic
//! rather than a race against the clients' attach.
//!
//! `d1` runs with an announce queue zero entries deep. Two facts force that:
//!
//! 1. A *held* announce cannot be the failure. The vendored 1.3.5 queue airs
//!    the held entry when the slot lapses, and a second announce for the same
//!    destination refreshes the entry it finds rather than overtaking it
//!    (`should_queue`, `reference/Reticulum/RNS/Transport.py:1283`), so a
//!    re-announce cannot beat a hold and no arrangement of a default daemon
//!    is red before this change and green after.
//! 2. A *dropped* announce is what the rig saw. At a queue depth of 0 the
//!    reference takes its own drop branch — capped, not queued, nothing
//!    logged — which is exactly what an `rnsd` whose queue never airs the
//!    entry looks like from outside. The symptom is reproduced through the
//!    reference's own code, not through a stub.
//!
//! So: A's announce reaches `d1` and dies at the capped hop, including the
//! one rebroadcast retry the reference makes ~5.5 s later
//! (`PATHFINDER_R`/`PATHFINDER_G`,
//! `reference/Reticulum/RNS/Transport.py:68`). B's announce crosses the other
//! direction untouched, so A discovers B at once and B discovers A only
//! through the tool's second announce.
//!
//! Before the re-announce this test was red with
//! `Phase 2 timeout: discovery took >90s`.

use std::time::{Duration, Instant};

use rand_core::OsRng;

use leviculum_core::identity::Identity;
use leviculum_core::{Destination, DestinationType, Direction};
use leviculum_std::driver::ReticulumNodeBuilder;

use crate::harness::TestDaemon;

/// Bitrate of the capped hop, in bits per second.
///
/// The reference prices a cap slot as `len(raw)*8 / bitrate / announce_cap`.
/// The announce `d1` rebroadcasts is 193 bytes (a 177-byte announce plus the
/// transport id a rebroadcast carries), so this bitrate buys a slot of
/// 193 x 8 x 50 / 3500 = 22.1 s. The slot has to sit between two numbers:
///
/// * above ~14 s, or the reference's own rebroadcast retry (~5.5 s after the
///   first attempt, which itself lands a few seconds into the run) would air
///   A's first announce and the test would pass without a re-announce;
/// * below 30 s, the slot the tool re-announces on when no radio bitrate is
///   known — which is this topology, since neither daemon owns a radio.
///
/// 22 s clears both with room on either side.
const CAPPED_BITRATE: u64 = 3500;

/// Phase 2's window. Long enough for the 30 s re-announce plus the crossing,
/// short enough that a red run fails in a minute and a half rather than
/// hanging.
const DISCOVERY_WINDOW_SECS: u64 = 90;

/// How long the run must take before its Phase 2 can be believed.
///
/// Everything before Phase 2 (two TCP pre-checks, two node starts, two
/// identities) is seconds, and the phases after it are one packet each way
/// against a 5 s drain fallback. A run that finishes faster than this got its
/// discovery from the first announce, which means the cap was not in force
/// and the test proved nothing.
const MIN_PLAUSIBLE_RUN_SECS: f64 = 15.0;

#[tokio::test]
async fn phase_two_completes_through_the_second_announce_when_the_cap_ate_the_first() {
    let d1 = TestDaemon::start_with_announce_queue_depth(0)
        .await
        .expect("entry daemon");
    let d2 = TestDaemon::start().await.expect("exit daemon");

    d1.add_client_interface_with_bitrate(
        "127.0.0.1",
        d2.rns_port(),
        Some("CappedLinkToD2"),
        CAPPED_BITRATE,
    )
    .await
    .expect("capped hop between the daemons");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // One announce across the capped hop, and the hop is shut for a slot.
    let filler_storage = tempfile::tempdir().expect("filler storage");
    let filler_id = Identity::generate(&mut OsRng);
    let filler_key = filler_id.private_key_bytes().expect("filler key");
    let filler_id2 = Identity::from_private_key_bytes(&filler_key).expect("filler key reload");
    let mut filler = ReticulumNodeBuilder::new()
        .storage_path(filler_storage.path().to_path_buf())
        .identity(filler_id)
        .enable_transport(false)
        .add_tcp_client(d1.rns_addr())
        .build()
        .await
        .expect("filler node");
    filler.start().await.expect("filler start");

    let filler_dest = Destination::new(
        Some(filler_id2),
        Direction::In,
        DestinationType::Single,
        "selftest",
        &["capfiller"],
    )
    .expect("filler destination");
    let filler_hash = *filler_dest.hash();
    filler.register_destination(filler_dest);
    tokio::time::sleep(Duration::from_secs(1)).await;
    filler
        .announce_destination(&filler_hash, Some(b"cap-filler"))
        .await
        .expect("filler announce");

    // The cap is only consumed if that announce actually crossed, so wait for
    // the far daemon to have the path rather than for a fixed time.
    let mut crossed = false;
    let waited_from = Instant::now();
    while waited_from.elapsed() < Duration::from_secs(20) {
        if d2.has_path(filler_hash.as_bytes()).await {
            crossed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        crossed,
        "the filler announce never reached the far daemon, so the capped hop \
         was never shut and this test would pass for the wrong reason"
    );

    let started = Instant::now();
    let outcome = leviculum_cli::selftest::run_selftest(
        vec![d1.rns_addr().to_string(), d2.rns_addr().to_string()],
        1,
        1.0,
        "packet",
        None,
        DISCOVERY_WINDOW_SECS,
        None,
    )
    .await;
    let elapsed = started.elapsed();

    let _ = filler.stop().await;

    let verdict = outcome.unwrap_or_else(|e| {
        panic!(
            "selftest gave up after {:.1}s: {e}. The capped hop swallowed the \
             first announce, so Phase 2 can only complete through a second one",
            elapsed.as_secs_f64()
        )
    });
    println!("[test] selftest verdict {verdict:?} after {elapsed:.1?}");

    assert!(
        elapsed.as_secs_f64() >= MIN_PLAUSIBLE_RUN_SECS,
        "the run took {:.1}s, less than the {MIN_PLAUSIBLE_RUN_SECS}s a \
         discovery that had to wait out a cap slot needs — the first announce \
         got through, so the cap this test rests on was not in force",
        elapsed.as_secs_f64()
    );
}
