//! Two propagation nodes peer and sync, in process, over TCP loopback
//! (leviculum#384 part 2).
//!
//! The chain-cell shape minus the stock Python middle: node A and node B
//! both run lnpnd's production engine, autopeer from each other's
//! announces, and A syncs a client's upload to B over the real `/offer`
//! round — key, offer, wanted-list, resource — where the recipient drains
//! it. The second round is the §5 bounded full re-offer: `pn_reoffer`
//! resets A's cursor and B answers "want none" for what it already holds
//! (`lxmf_pn_offer dir=out wanted=0`), which is also the chain cells'
//! nothing-new assertion. The cross-stack versions, with a genuine `lxmd`
//! in the middle, are the periculum conformance cells.

mod common;

use std::time::Duration;

use common::{body_b64, Event, Helper, Setup, Wire};

/// Poll all four helpers until `done` holds or the deadline passes.
async fn pump4<F>(helpers: &mut [&mut Helper; 4], budget: Duration, mut done: F) -> bool
where
    F: FnMut(&[&mut Helper; 4]) -> bool,
{
    let deadline = std::time::Instant::now() + budget;
    loop {
        for helper in helpers.iter_mut() {
            helper.drain();
        }
        if done(helpers) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn last_peer_count(helper: &Helper) -> Option<&str> {
    helper
        .find_last("lxmf_pn_peers")
        .and_then(|event| event.field("count"))
}

fn last_store_size(helper: &Helper) -> Option<&str> {
    helper
        .find_last("lxmf_pn_store")
        .and_then(|event| event.field("size"))
}

fn offer_out_with_wanted<'h>(helper: &'h Helper, wanted: &str) -> Option<&'h Event> {
    helper.events.iter().find(|event| {
        event.name == "lxmf_pn_offer"
            && event.field("dir") == Some("out")
            && event.field("wanted") == Some(wanted)
    })
}

#[tokio::test]
async fn two_nodes_autopeer_and_sync_a_stored_message() {
    let mut node_a = Helper::start(Setup {
        transport: true,
        ..Setup::new("pn-a", Wire::listen_any())
    })
    .await;
    let a_addr = node_a.listen_addr();
    let mut node_b = Helper::start(Setup {
        transport: true,
        ..Setup::new("pn-b", Wire::Dial(a_addr))
    })
    .await;
    let mut sender = Helper::start(Setup::new("peer-sender", Wire::Dial(a_addr))).await;
    let mut recipient = Helper::start(Setup::new("peer-recipient", Wire::Dial(a_addr))).await;

    let mut helpers = [&mut node_a, &mut node_b, &mut sender, &mut recipient];
    let ready = pump4(&mut helpers, Duration::from_secs(20), |h| {
        h.iter().all(|helper| helper.delivery_hash.is_some())
    })
    .await;
    assert!(ready, "all four helpers must report lxmf_ready");

    // Both nodes take the role at the defaults (stamp cost 0, peering
    // cost 0): nothing about peering itself needs a nonzero cost, and the
    // costed paths have their own cells against the reference.
    helpers[0].command("pn_enable 1");
    helpers[1].command("pn_enable 1");
    let pn_ready = pump4(&mut helpers, Duration::from_secs(10), |h| {
        h[0].seen("lxmf_pn_ready") && h[1].seen("lxmf_pn_ready")
    })
    .await;
    assert!(pn_ready, "both nodes must take the propagation role");
    let a_hash = helpers[0]
        .find("lxmf_pn_ready")
        .and_then(|event| event.field("hash"))
        .expect("node A pn hash")
        .to_string();
    let b_hash = helpers[1]
        .find("lxmf_pn_ready")
        .and_then(|event| event.field("hash"))
        .expect("node B pn hash")
        .to_string();

    // Autopeering: each node hears the other's announce (one hop, inside
    // the default autopeer_maxdepth of 4) and adds it — the Handlers.py
    // decision tree over our table.
    let peered = pump4(&mut helpers, Duration::from_secs(30), |h| {
        if last_peer_count(h[0]) == Some("1") && last_peer_count(h[1]) == Some("1") {
            return true;
        }
        h[0].command("pn_peers");
        h[1].command("pn_peers");
        false
    })
    .await;
    assert!(
        peered,
        "both nodes must autopeer from announces; A events: {:?}; B events: {:?}",
        helpers[0].events, helpers[1].events
    );
    assert!(
        helpers[0].seen("lxmf_pn_peer"),
        "the peer add must be reported"
    );

    // A client uploads one message for the recipient into node A.
    helpers[3].command("announce");
    let recipient_hash = helpers[3].delivery_hash.clone().expect("recipient hash");
    let sender_hash = helpers[2].delivery_hash.clone().expect("sender hash");
    helpers[2].command(&format!("wait_for_peer {recipient_hash} 20"));
    let peer_known = pump4(&mut helpers, Duration::from_secs(25), |h| {
        h[2].seen("lxmf_wait_for_peer_ok")
    })
    .await;
    assert!(peer_known, "the sender must learn the recipient's announce");
    let selected = pump4(&mut helpers, Duration::from_secs(30), |h| {
        if h[2].seen("lxmf_pn_selected") && h[3].seen("lxmf_pn_selected") {
            return true;
        }
        h[2].command(&format!("set_pn {a_hash}"));
        h[3].command(&format!("set_pn {b_hash}"));
        false
    })
    .await;
    assert!(selected, "sender selects node A, recipient node B");

    let body = body_b64("carried from node A to node B by peering");
    helpers[2].command(&format!("send_propagated {recipient_hash} {body}"));
    let stored_at_a = pump4(&mut helpers, Duration::from_secs(30), |h| {
        h[0].command("pn_store_size");
        last_store_size(h[0]) == Some("1")
    })
    .await;
    assert!(
        stored_at_a,
        "the upload must land in node A's store; A events: {:?}",
        helpers[0].events
    );

    // The sync: A's scheduler picks B (its only peer), mines the free key
    // once, offers, B wants the message, one resource carries it. The
    // cursor advances only on the concluded transfer.
    let synced = pump4(&mut helpers, Duration::from_secs(90), |h| {
        h[1].command("pn_store_size");
        last_store_size(h[1]) == Some("1")
    })
    .await;
    assert!(
        synced,
        "node B must hold the message after the sync; A events: {:?}; A logs: {:?}",
        helpers[0].events, helpers[0].logs
    );
    let offer = offer_out_with_wanted(helpers[0], "1")
        .expect("node A must report the outbound offer round");
    assert_eq!(offer.field("offered"), Some("1"));
    assert!(
        helpers[0].events.iter().any(|event| {
            event.name == "lxmf_pn_sync"
                && event.field("dir") == Some("out")
                && event.field("result") == Some("ok")
                && event.field("transferred") == Some("1")
        }),
        "node A must conclude the sync round: {:?}",
        helpers[0].events
    );
    // Exactly one copy per store.
    assert_eq!(last_store_size(helpers[0]), Some("1"));

    // The bounded full re-offer (§5's reboot / reclaimed-page case),
    // while B still holds its copy: reset A's cursor, and the second
    // round offers everything again — B's store already has it, so it
    // answers "want none" and nothing is transferred twice. Ordering
    // matters here: offer answering is store membership only
    // (offer_request, LXMRouter.py:2318), so a *drained* store would
    // honestly want the message again.
    helpers[0].command(&format!("pn_reoffer {b_hash}"));
    let reoffered = pump4(&mut helpers, Duration::from_secs(60), |h| {
        offer_out_with_wanted(h[0], "0").is_some()
    })
    .await;
    assert!(
        reoffered,
        "the re-offer round must conclude with wanted=0; A events: {:?}; A logs: {:?}",
        helpers[0].events, helpers[0].logs
    );
    helpers[0].command("pn_store_size");
    helpers[1].command("pn_store_size");
    let settled = pump4(&mut helpers, Duration::from_secs(10), |h| {
        last_store_size(h[0]) == Some("1") && last_store_size(h[1]) == Some("1")
    })
    .await;
    assert!(settled, "the re-offer must not duplicate anything");

    // The recipient drains its copy from node B — the message crossed the
    // node-to-node hop intact.
    helpers[3].command("sync");
    let drained = pump4(&mut helpers, Duration::from_secs(30), |h| {
        h[3].find_last("lxmf_sync_done")
            .and_then(|event| event.field("count"))
            == Some("1")
    })
    .await;
    assert!(
        drained,
        "the recipient must fetch exactly one message from node B: {:?}",
        helpers[3].events
    );
    assert!(
        helpers[3].received(&sender_hash, &body).is_some(),
        "the body must survive the peer hop: {:?}",
        helpers[3].events
    );
}
