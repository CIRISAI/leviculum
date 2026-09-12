//! A stamped upload at a non-zero cost, end to end, with validation on
//! the engine's worker thread (leviculum#384 part 4, deliverable 5).
//!
//! Part 1 validated inline in the event hook; the engine now queues the
//! workblock grinding to `lnpnd::validation` and applies the verdict —
//! append, then proof — at the drain. This script is the proof the
//! restructure kept the contract: the node announces stamp cost 8, the
//! sending client mines a genuine propagation stamp to it, the upload is
//! accepted (validated on the worker, since `min_accepted_cost` = 8−3 =
//! 5 > 0), stored exactly once, and drained by the recipient with the
//! body intact. A validation that never completed would leave the store
//! empty and the sender's packet unproven; a wrongly rejected stamp
//! would tear the sender's link down — both fail this script inside its
//! windows.

mod common;

use std::time::Duration;

use common::{body_b64, Helper, Setup, Wire};

async fn pump3<F>(
    hub: &mut Helper,
    sender: &mut Helper,
    recipient: &mut Helper,
    budget: Duration,
    mut done: F,
) -> bool
where
    F: FnMut(&mut Helper, &mut Helper, &mut Helper) -> bool,
{
    let deadline = std::time::Instant::now() + budget;
    loop {
        hub.drain();
        sender.drain();
        recipient.drain();
        if done(hub, sender, recipient) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_cost_8_upload_validates_on_the_worker_and_serves() {
    let mut hub = Helper::start(Setup {
        transport: true,
        ..Setup::new("pn-hub", Wire::listen_any())
    })
    .await;
    let hub_addr = hub.listen_addr();
    let mut sender = Helper::start(Setup::new("pn-sender", Wire::Dial(hub_addr))).await;
    let mut recipient = Helper::start(Setup::new("pn-recipient", Wire::Dial(hub_addr))).await;

    let ready = pump3(
        &mut hub,
        &mut sender,
        &mut recipient,
        Duration::from_secs(20),
        |a, b, c| {
            a.delivery_hash.is_some() && b.delivery_hash.is_some() && c.delivery_hash.is_some()
        },
    )
    .await;
    assert!(ready, "all three helpers must report lxmf_ready");
    let recipient_hash = recipient.delivery_hash.clone().expect("recipient hash");

    // Cost 8 with the default flexibility 3: min accepted value 5, so
    // every accept runs the 1000-round workblock — on the worker.
    hub.command("pn_enable 1 stamp_cost=8");
    let pn_ready = pump3(
        &mut hub,
        &mut sender,
        &mut recipient,
        Duration::from_secs(10),
        |a, _, _| a.seen("lxmf_pn_ready"),
    )
    .await;
    assert!(
        pn_ready,
        "pn_enable must report the propagation destination"
    );
    let pn_hash = hub
        .find("lxmf_pn_ready")
        .and_then(|event| event.field("hash"))
        .expect("pn hash")
        .to_string();

    recipient.command("announce");
    let selected = pump3(
        &mut hub,
        &mut sender,
        &mut recipient,
        Duration::from_secs(30),
        |_, b, c| {
            if b.seen("lxmf_pn_selected") && c.seen("lxmf_pn_selected") {
                return true;
            }
            b.command(&format!("set_pn {pn_hash}"));
            c.command(&format!("set_pn {pn_hash}"));
            false
        },
    )
    .await;
    assert!(selected, "both clients must select the announced node");
    sender.command(&format!("wait_for_peer {recipient_hash} 20"));
    let peer_known = pump3(
        &mut hub,
        &mut sender,
        &mut recipient,
        Duration::from_secs(25),
        |_, b, _| b.seen("lxmf_wait_for_peer_ok"),
    )
    .await;
    assert!(peer_known, "the sender must learn the recipient's announce");

    // The upload: the sender mines the propagation stamp at cost 8 (a
    // subsecond mine), the node validates it off the hook and proves.
    let body = body_b64("a stamped upload validated off the core lock");
    sender.command(&format!("send_propagated {recipient_hash} {body}"));
    let stored = pump3(
        &mut hub,
        &mut sender,
        &mut recipient,
        Duration::from_secs(60),
        |a, b, _| {
            if !b.seen("lxmf_msg_sent") {
                return false;
            }
            a.command("pn_store_size");
            a.find_last("lxmf_pn_store")
                .and_then(|event| event.field("size"))
                == Some("1")
        },
    )
    .await;
    assert!(
        stored,
        "the stamped upload must land in the store; hub events: {:?}; \
         sender events: {:?}; hub logs: {:?}",
        hub.events, sender.events, hub.logs
    );

    recipient.command("sync");
    let drained = pump3(
        &mut hub,
        &mut sender,
        &mut recipient,
        Duration::from_secs(30),
        |_, _, c| {
            c.find_last("lxmf_sync_done")
                .and_then(|event| event.field("count"))
                == Some("1")
        },
    )
    .await;
    assert!(
        drained,
        "the sync must fetch exactly one message; recipient events: {:?}",
        recipient.events
    );
    let sender_hash = sender.delivery_hash.clone().expect("sender hash");
    assert!(
        recipient.received(&sender_hash, &body).is_some(),
        "the drained message must arrive with its body intact: {:?}",
        recipient.events
    );
}
