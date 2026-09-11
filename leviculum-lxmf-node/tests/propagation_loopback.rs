//! The propagation-node helper verbs, in process, over TCP loopback
//! (leviculum#384 part 1).
//!
//! Three helpers in the conformance cells' own shape: a hub whose node runs
//! as a transport and carries the propagation role (`pn_enable`, lnpnd's
//! production engine), a sender and a recipient dialing it. The script is
//! the exact command sequence the cells drive — `set_pn`,
//! `send_propagated`, `sync` — and every assertion is on the events those
//! cells assert on: `lxmf_pn_ready`, `lxmf_pn_selected`, `lxmf_msg_sent`,
//! `lxmf_sync_done`, `lxmf_msg_received`, and `lxmf_pn_store` before and
//! after the confirmed fetch.
//!
//! The cross-stack version of this script, with genuine Python clients, is
//! `leviculum-std/tests/rnsd_interop/propagation_node_interop_tests.rs` and
//! the periculum conformance cells.

mod common;

use std::time::Duration;

use common::{body_b64, Helper, Setup, Wire};

/// Poll all three helpers until `done` holds or the deadline passes.
async fn pump3<F>(
    hub: &mut Helper,
    sender: &mut Helper,
    recipient: &mut Helper,
    budget: Duration,
    mut done: F,
) -> bool
where
    F: FnMut(&Helper, &Helper, &Helper) -> bool,
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
async fn a_sender_uploads_and_the_recipient_drains_through_the_pn_helper() {
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

    // The hub takes the role; the first announce is due after one second.
    hub.command("pn_enable 1");
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

    // The recipient announces so the sender can encrypt to it (relayed by
    // the hub's transport); both clients select the announced node, polling
    // `set_pn` exactly as a scenario polls it.
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

    // Upload: one propagated message for the recipient's mailbox. The
    // sender's packet is proven only after the hub's store append returned,
    // and the store probe must count exactly one message.
    let body = body_b64("stored and drained through the helper pn");
    sender.command(&format!("send_propagated {recipient_hash} {body}"));
    let stored = pump3(
        &mut hub,
        &mut sender,
        &mut recipient,
        Duration::from_secs(30),
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
        "the upload must land in the hub's store; hub events: {:?}; sender events: {:?}; \
         hub logs: {:?}; sender logs: {:?}",
        hub.events, sender.events, hub.logs, sender.logs
    );

    // Drain: list, fetch, confirm-purge. The message reaches the recipient
    // and the confirmed fetch empties the store.
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
        "the sync must fetch exactly one message; recipient events: {:?}; recipient logs: {:?}",
        recipient.events, recipient.logs
    );
    let sender_hash = sender.delivery_hash.clone().expect("sender hash");
    assert!(
        recipient.received(&sender_hash, &body).is_some(),
        "the drained message must be delivered with its body intact: {:?}",
        recipient.events
    );

    let emptied = pump3(
        &mut hub,
        &mut sender,
        &mut recipient,
        Duration::from_secs(20),
        |a, _, _| {
            a.command("pn_store_size");
            a.find_last("lxmf_pn_store")
                .and_then(|event| event.field("size"))
                == Some("0")
        },
    )
    .await;
    assert!(
        emptied,
        "the store must be empty after the confirmed fetch: {:?}",
        hub.find_last("lxmf_pn_store")
    );
}
