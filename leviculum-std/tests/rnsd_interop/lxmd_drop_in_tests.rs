//! Drop-ins against a stock `lxmd` (leviculum order 671).
//!
//! Every propagation decision of #442 — peering off the announce, the
//! load-time re-pricing of D6 (668), rotation (0a1d7e38) — is pinned by unit
//! tests and periculum cells against our own reading of the reference. These
//! run lnpnd's production engine beside the reference daemon itself, inside
//! `cargo test`, so what is checked is that the stock lxmd agrees. The
//! fixture is `lxmd_peer.rs`; the periculum cells running the helper's
//! `LXMRouter` beside lnpnd (`lxmf_pn_peers_both_stacks`,
//! `lxmf_pn_two_python_peers`, `lnpnd_receives_a_stock_peers_store_at_defaults`)
//! stay what they are.
//!
//! Each test prints `LXMD[<name>] <line>` for the lxmd lines it rests on,
//! `LNPND <event>` for lnpnd's, and `WALL` at the end.

use std::time::{Duration, Instant};

use leviculum_lxmf::{MemoryPropagationStore, PeeringConfig, PropagationStore};
use leviculum_std::FilePropagationStore;
use lnpnd::engine::EngineEvent;

use crate::lxmd_peer::{
    is_peer_add, is_peer_drop, is_sync_out_to, stamped_message, Lnpnd, LnpndOptions, LxmdConfig,
    LxmdPeer, PreloadedMessage, SharedInstance,
};

/// lxmd's own node announce leaves 20 s after its start
/// (`NODE_ANNOUNCE_DELAY`, `LXMRouter.py:41`, sent from `enable_propagation`,
/// `:685`); this is that plus the margin a loaded gate host needs.
const LXMD_ANNOUNCE_WAIT: Duration = Duration::from_secs(35);

fn show(lxmd: &LxmdPeer, needles: &[&str]) {
    for line in lxmd.log().lines() {
        if needles.iter().any(|needle| line.contains(needle)) {
            eprintln!("LXMD[{}] {line}", lxmd.name);
        }
    }
}

/// lnpnd announces first. The stock lxmd autopeers it from that announce
/// (`Peered with`, `LXMRouter.py:2046`) before lnpnd knows lxmd at all, and
/// once lxmd's own node announce arrives lnpnd holds lxmd as its one peer,
/// as `lnpnd --status` reports it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stock_lxmd_peers_lnpnd_from_its_announce() {
    let wall = Instant::now();
    let instance = SharedInstance::start("lxmd_drop_in_peer").await;
    let lxmd = LxmdPeer::start(
        &instance,
        &LxmdConfig {
            display_name: "stock-a".into(),
            ..LxmdConfig::default()
        },
        &[],
    )
    .await;
    let mut pnd = Lnpnd::start(
        &instance,
        MemoryPropagationStore::new(1 << 20),
        LnpndOptions::default(),
    )
    .await;
    let pnd_hex = hex::encode(pnd.propagation_hash);

    let peered = lxmd
        .wait_for_log(&format!("Peered with <{pnd_hex}>"), Duration::from_secs(20))
        .await;
    assert!(
        peered.is_some(),
        "lxmd never peered lnpnd from its announce; lxmd log:\n{}",
        lxmd.log()
    );
    // The order is the subject: lxmd peered off lnpnd's announce, not off
    // anything lnpnd did as its peer.
    assert!(
        pnd.events_matching(is_peer_add(&lxmd.propagation_hash))
            .is_empty(),
        "lnpnd had peered lxmd before lxmd peered it"
    );
    let (total, line) = pnd.peers_line().await;
    eprintln!("LNPND before lxmd's announce: {line}");
    assert_eq!(total, 0, "{line}");

    let added = pnd
        .wait_event(LXMD_ANNOUNCE_WAIT, is_peer_add(&lxmd.propagation_hash))
        .await;
    assert!(
        added.is_some(),
        "lnpnd never peered lxmd; events {:?}; lxmd log:\n{}",
        pnd.seen,
        lxmd.log()
    );
    eprintln!("LNPND {:?} at {:?}", added, wall.elapsed());
    let (total, line) = pnd.peers_line().await;
    eprintln!("LNPND {line}");
    assert_eq!(total, 1, "{line}");

    show(
        &lxmd,
        &["Peered with", "Started lxmd", "Propagation Node started"],
    );
    eprintln!("WALL {:?}", wall.elapsed());
    lxmd.stop().await;
    pnd.stop().await;
    instance.stop().await;
}

/// The mirror: lxmd announces first (its 20 s node announce) while lnpnd
/// stays silent; lnpnd peers it from that announce, and only when lnpnd's own
/// announce goes out does lxmd peer back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lnpnd_peers_a_stock_lxmd_from_its_announce() {
    let wall = Instant::now();
    let instance = SharedInstance::start("lxmd_drop_in_mirror").await;
    let lxmd = LxmdPeer::start(
        &instance,
        &LxmdConfig {
            display_name: "stock-b".into(),
            ..LxmdConfig::default()
        },
        &[],
    )
    .await;
    // lnpnd's first announce at 30 s: after lxmd's at 20 s.
    let mut pnd = Lnpnd::start(
        &instance,
        MemoryPropagationStore::new(1 << 20),
        LnpndOptions {
            announce_delay_secs: Some(30),
            ..LnpndOptions::default()
        },
    )
    .await;
    let pnd_hex = hex::encode(pnd.propagation_hash);

    let added = pnd
        .wait_event(LXMD_ANNOUNCE_WAIT, is_peer_add(&lxmd.propagation_hash))
        .await;
    assert!(
        added.is_some(),
        "lnpnd never peered lxmd from lxmd's announce; events {:?}; lxmd log:\n{}",
        pnd.seen,
        lxmd.log()
    );
    eprintln!("LNPND {:?} at {:?}", added, wall.elapsed());
    assert!(
        pnd.events_matching(|e| matches!(e, EngineEvent::Announced))
            .is_empty(),
        "lnpnd announced before it peered lxmd: the order under test did not hold"
    );
    assert!(
        lxmd.lines(&format!("Peered with <{pnd_hex}>")).is_empty(),
        "lxmd peered lnpnd before lnpnd announced"
    );
    let (total, line) = pnd.peers_line().await;
    eprintln!("LNPND {line}");
    assert_eq!(total, 1, "{line}");

    let peered = lxmd
        .wait_for_log(&format!("Peered with <{pnd_hex}>"), LXMD_ANNOUNCE_WAIT)
        .await;
    assert!(
        peered.is_some(),
        "lxmd never peered lnpnd after lnpnd announced; lxmd log:\n{}",
        lxmd.log()
    );

    show(&lxmd, &["Peered with", "Propagation Node started"]);
    eprintln!("WALL {:?}", wall.elapsed());
    lxmd.stop().await;
    pnd.stop().await;
    instance.stop().await;
}

/// The stock peer's minimum in the D6 and rotation tests: lxmd's floor of 13
/// (`PROPAGATION_COST_MIN`) with flexibility 9 accepts and is offered stamps
/// worth 4 (`LXMRouter.py:2242`, `LXMPeer.py:331`), which mine in
/// milliseconds where 10, the stock minimum, takes seconds in a debug build.
fn stock_peer_at_four(name: &str) -> LxmdConfig {
    LxmdConfig {
        display_name: name.into(),
        stamp_cost_target: 13,
        stamp_cost_flexibility: 9,
        ..LxmdConfig::default()
    }
}

const PEER_MINIMUM: u8 = 4;

/// Receive time for a stored message: now, or the store's expiry sweep
/// takes it before any round runs.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One D6 arm: a store holding SUBJECT at stamp value 0 (what a cost-0
/// lnpnd stored before 8f470aa7, and what lxmd's own files without a value
/// component read as) and WITNESS at its true value; `reprice` runs the
/// load-time pass `main.rs` runs (`lnpnd::reprice_stored_values`, 668) or
/// not. Returns once lnpnd's first round toward lxmd concluded.
async fn d6_arm(tag: &str, reprice: bool) -> D6Arm {
    let (subject, subject_body, subject_value) = stamped_message(PEER_MINIMUM);
    let (witness, witness_body, witness_value) = stamped_message(PEER_MINIMUM);
    assert!(subject_value >= PEER_MINIMUM && witness_value >= PEER_MINIMUM);

    let store_dir = tempfile::tempdir().expect("store dir");
    let mut store = FilePropagationStore::open(store_dir.path(), 1 << 20).expect("open store");
    let now = unix_now();
    store
        .append(&subject, now, 0, &subject_body)
        .expect("store the subject at value 0");
    store
        .append(&witness, now, witness_value, &witness_body)
        .expect("store the witness at its value");
    if reprice {
        assert_eq!(
            lnpnd::reprice_stored_values(&mut store).expect("re-price"),
            1,
            "exactly the subject is re-priced"
        );
    }
    let stored_value = |id: &[u8; 32]| store.meta(id).unwrap().expect("stored").stamp_value;
    eprintln!(
        "LNPND store: subject value {} (true {subject_value}), witness value {}",
        stored_value(&subject),
        stored_value(&witness)
    );

    let instance = SharedInstance::start(tag).await;
    let lxmd = LxmdPeer::start(&instance, &stock_peer_at_four(tag), &[]).await;
    let mut pnd = Lnpnd::start(&instance, store, LnpndOptions::default()).await;
    let round = pnd
        .wait_event(LXMD_ANNOUNCE_WAIT, is_sync_out_to(&lxmd.propagation_hash))
        .await;
    assert!(
        round.is_some(),
        "lnpnd never concluded a round toward lxmd; events {:?}; lxmd log:\n{}",
        pnd.seen,
        lxmd.log()
    );
    eprintln!("LNPND {:?}", round);
    for offer in pnd.events_matching(|e| matches!(e, EngineEvent::Offer { dir: "out", .. })) {
        eprintln!("LNPND {offer:?}");
    }
    D6Arm {
        lxmd,
        pnd,
        instance,
        subject,
        witness,
        _store_dir: store_dir,
    }
}

struct D6Arm {
    lxmd: LxmdPeer,
    pnd: Lnpnd,
    instance: SharedInstance,
    subject: [u8; 32],
    witness: [u8; 32],
    /// The file store's directory, alive as long as the engine owns it.
    _store_dir: tempfile::TempDir,
}

impl D6Arm {
    async fn stop(self) {
        self.lxmd.stop().await;
        self.pnd.stop().await;
        self.instance.stop().await;
    }
}

/// D6 (668): a message stored at value 0 is re-priced at load and the stock
/// lxmd receives it in lnpnd's first round toward it. The negative control
/// is the same store without the pass: the round runs, the witness arrives,
/// the subject is skipped as below the peer's minimum and never reaches lxmd.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_stored_at_value_zero_reaches_a_stock_lxmd_once_repriced() {
    let wall = Instant::now();
    let arm = d6_arm("lxmd_drop_in_d6", true).await;
    let lxmd = &arm.lxmd;
    for (what, id) in [("subject", &arm.subject), ("witness", &arm.witness)] {
        assert!(
            lxmd.wait_for_stored(id, Duration::from_secs(10)).await,
            "lxmd's store never took the {what}; store {:?}; lxmd log:\n{}",
            lxmd.stored(),
            lxmd.log()
        );
    }
    assert_eq!(
        lxmd.lines("Accepted all 2 offered messages from").len(),
        1,
        "lxmd was not offered both in one round:\n{}",
        lxmd.log()
    );
    show(lxmd, &["Peered with", "offered message", "No wanted"]);
    eprintln!("LXMD[{}] store {:?}", lxmd.name, lxmd.stored());
    eprintln!("WALL {:?}", wall.elapsed());
    arm.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_repricing_pass_the_value_zero_message_is_not_offered() {
    let wall = Instant::now();
    let arm = d6_arm("lxmd_drop_in_d6_neg", false).await;
    let lxmd = &arm.lxmd;
    assert!(
        lxmd.wait_for_stored(&arm.witness, Duration::from_secs(10))
            .await,
        "the witness never reached lxmd, so the round says nothing about the \
         subject; store {:?}; lxmd log:\n{}",
        lxmd.stored(),
        lxmd.log()
    );
    // The round that carried the witness offered one message, not two:
    // the subject was skipped on lnpnd's side (`build_offer`'s
    // `skipped_low_value`), it was not refused on lxmd's.
    assert_eq!(
        lxmd.lines("Accepted all 1 offered message from").len(),
        1,
        "lxmd's offers:\n{}",
        lxmd.log()
    );
    assert!(
        !lxmd.stored().contains(&hex::encode(arm.subject)),
        "the value-0 subject reached lxmd without the re-pricing pass"
    );
    show(lxmd, &["Peered with", "offered message", "No wanted"]);
    eprintln!("LXMD[{}] store {:?}", lxmd.name, lxmd.stored());
    eprintln!("WALL {:?}", wall.elapsed());
    arm.stop().await;
}

/// Rotation (0a1d7e38) against three stock peers. lnpnd runs at
/// `max_peers = 3` — at 2 the reference's rule never drops anything, since a
/// pass only drops while more than one peer would remain
/// (`LXMRouter.py:2062-2064`: headroom 1, one drop required, one left) — and
/// holds one message M. KEEP-1 and KEEP-2 take M when offered (acceptance
/// 1/1). VICTIM already holds M in its store and answers the offer with "No
/// wanted messages" (`LXMRouter.py:2320-2321`): acceptance 0/1, the lowest,
/// below 50 %, so the reference's rule drops it, and it is the only peer
/// below 50 %, so the drop does not depend on when the pass runs. A fourth
/// stock node, LATE, started once the table is full, announces after the
/// pass and takes the freed slot.
///
/// The three table peers run with `autopeer = no`. Stock nodes on one
/// instance otherwise peer each other, and KEEP-2 could pass M to KEEP-1
/// before lnpnd's offer reaches it; KEEP-1 would then decline lnpnd too,
/// tie VICTIM at 0 %, and the drop would follow hash order. With autopeer
/// off lnpnd's offers are the only ones, an offer from a non-peer is still
/// answered (`offer_request` checks the key, not the peer table), and
/// VICTIM never syncs M into lnpnd: its own view of lnpnd is what it logs
/// about lnpnd's offers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lnpnd_rotates_out_the_peer_a_stock_lxmd_would_and_a_newcomer_peers() {
    let wall = Instant::now();
    let (transient_id, body, value) = stamped_message(PEER_MINIMUM);
    let mut store = MemoryPropagationStore::new(1 << 20);
    store
        .append(&transient_id, unix_now(), value, &body)
        .expect("store M");

    let instance = SharedInstance::start("lxmd_drop_in_rot").await;
    let table_peer = |name: &str| LxmdConfig {
        autopeer: false,
        ..stock_peer_at_four(name)
    };
    let keep_1 = LxmdPeer::start(&instance, &table_peer("keep-1"), &[]).await;
    let keep_2 = LxmdPeer::start(&instance, &table_peer("keep-2"), &[]).await;
    let victim = LxmdPeer::start(
        &instance,
        &table_peer("victim"),
        &[PreloadedMessage {
            transient_id,
            body: body.clone(),
            stamp_value: value,
        }],
    )
    .await;
    assert!(
        victim.stored().contains(&hex::encode(transient_id)),
        "VICTIM does not hold M"
    );
    let mut pnd = Lnpnd::start(
        &instance,
        store,
        LnpndOptions {
            peering: PeeringConfig {
                max_peers: 3,
                ..PeeringConfig::default()
            },
            sync_interval_secs: Some(1),
            rotation_interval_secs: Some(5),
            ..LnpndOptions::default()
        },
    )
    .await;
    let pnd_hex = hex::encode(pnd.propagation_hash);

    // All three peered and each offered M once.
    for peer in [&keep_1, &keep_2, &victim] {
        let round = pnd
            .wait_event(LXMD_ANNOUNCE_WAIT, is_sync_out_to(&peer.propagation_hash))
            .await;
        assert!(
            round.is_some(),
            "lnpnd concluded no round toward {}; events {:?}",
            peer.name,
            pnd.seen
        );
        eprintln!("LNPND toward {}: {:?}", peer.name, round);
    }
    let (total, line) = pnd.peers_line().await;
    eprintln!("LNPND full table: {line}");

    // LATE starts now: its node announce arrives 20 s on, after the next
    // rotation pass (every 5 s here).
    let late = LxmdPeer::start(&instance, &stock_peer_at_four("late"), &[]).await;

    let dropped = pnd
        .wait_event(
            Duration::from_secs(15),
            is_peer_drop(&victim.propagation_hash),
        )
        .await;
    assert!(
        dropped.is_some(),
        "lnpnd never rotated VICTIM out (table {total}); events {:?}",
        pnd.seen
    );
    eprintln!("LNPND {:?} at {:?}", dropped, wall.elapsed());
    for keeper in [&keep_1, &keep_2] {
        assert!(
            pnd.events_matching(is_peer_drop(&keeper.propagation_hash))
                .is_empty(),
            "lnpnd dropped {}, which accepted everything it was offered",
            keeper.name
        );
    }
    let (total, line) = pnd.peers_line().await;
    eprintln!("LNPND after the pass: {line}");
    assert_eq!(total, 2, "{line}");

    let joined = pnd
        .wait_event(LXMD_ANNOUNCE_WAIT, is_peer_add(&late.propagation_hash))
        .await;
    assert!(
        joined.is_some(),
        "LATE did not take the freed slot; events {:?}",
        pnd.seen
    );
    eprintln!("LNPND {:?} at {:?}", joined, wall.elapsed());
    let (total, line) = pnd.peers_line().await;
    eprintln!("LNPND with LATE: {line}");
    assert_eq!(total, 3, "{line}");

    // VICTIM's own view: it never peered lnpnd (autopeer off) and declined
    // the one offer; nothing lnpnd does on its side reaches it. The keepers
    // each took M from lnpnd in one offer.
    assert_eq!(
        victim
            .lines(&format!("No wanted messages in offer from <{pnd_hex}>"))
            .len(),
        1,
        "VICTIM's view of lnpnd's offers:\n{}",
        victim.log()
    );
    for keeper in [&keep_1, &keep_2] {
        assert_eq!(
            keeper
                .lines(&format!("Accepted all 1 offered message from <{pnd_hex}>"))
                .len(),
            1,
            "{}'s view of lnpnd's offers:\n{}",
            keeper.name,
            keeper.log()
        );
    }
    assert!(victim.lines("Peered with").is_empty());
    for peer in [&keep_1, &keep_2, &victim, &late] {
        show(peer, &["Peered with", "offered message", "No wanted"]);
    }
    eprintln!("WALL {:?}", wall.elapsed());
    for peer in [keep_1, keep_2, victim, late] {
        peer.stop().await;
    }
    pnd.stop().await;
    instance.stop().await;
}
