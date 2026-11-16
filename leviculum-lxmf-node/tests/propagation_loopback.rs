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

/// Three helpers dialed in, the hub's role enabled with `pn_enable`, both
/// clients on the hub's node, and one propagated message from the sender in
/// the hub's store. Returns the sender's delivery hash and the body sent.
async fn one_message_stored(
    hub: &mut Helper,
    sender: &mut Helper,
    recipient: &mut Helper,
    pn_enable: &str,
) -> (String, String) {
    one_message_of(
        hub,
        sender,
        recipient,
        pn_enable,
        "stored and drained through the helper pn",
    )
    .await
}

/// [`one_message_stored`] with the message's text named.
async fn one_message_of(
    hub: &mut Helper,
    sender: &mut Helper,
    recipient: &mut Helper,
    pn_enable: &str,
    text: &str,
) -> (String, String) {
    let ready = pump3(
        hub,
        sender,
        recipient,
        Duration::from_secs(20),
        |a, b, c| {
            a.delivery_hash.is_some() && b.delivery_hash.is_some() && c.delivery_hash.is_some()
        },
    )
    .await;
    assert!(ready, "all three helpers must report lxmf_ready");
    let recipient_hash = recipient.delivery_hash.clone().expect("recipient hash");

    // The hub takes the role; the first announce is due after one second.
    hub.command(pn_enable);
    let pn_ready = pump3(
        hub,
        sender,
        recipient,
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
        hub,
        sender,
        recipient,
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
        hub,
        sender,
        recipient,
        Duration::from_secs(25),
        |_, b, _| b.seen("lxmf_wait_for_peer_ok"),
    )
    .await;
    assert!(peer_known, "the sender must learn the recipient's announce");

    // Upload: one propagated message for the recipient's mailbox. The
    // sender's packet is proven only after the hub's store append returned,
    // and the store probe must count exactly one message.
    let body = body_b64(text);
    sender.command(&format!("send_propagated {recipient_hash} {body}"));
    let stored = pump3(
        hub,
        sender,
        recipient,
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

    let sender_hash = sender.delivery_hash.clone().expect("sender hash");
    (sender_hash, body)
}

async fn three_helpers() -> (Helper, Helper, Helper) {
    let hub = Helper::start(Setup {
        transport: true,
        ..Setup::new("pn-hub", Wire::listen_any())
    })
    .await;
    let hub_addr = hub.listen_addr();
    let sender = Helper::start(Setup::new("pn-sender", Wire::Dial(hub_addr))).await;
    let recipient = Helper::start(Setup::new("pn-recipient", Wire::Dial(hub_addr))).await;
    (hub, sender, recipient)
}

#[tokio::test]
async fn a_sender_uploads_and_the_recipient_drains_through_the_pn_helper() {
    let (mut hub, mut sender, mut recipient) = three_helpers().await;
    let (sender_hash, body) =
        one_message_stored(&mut hub, &mut sender, &mut recipient, "pn_enable 1").await;

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

/// `serve_limit_bytes` reaches the node: a ceiling no message fits serves
/// nothing, and the unconfirmed message stays in the store for a later
/// round, as a board's serve limit leaves it
/// (`PropagationNodeConfig::serve_limit_bytes`). 40 B still lists one id
/// (`PropagationNode::list_capacity`), so the round is a list and a fetch
/// that comes back empty, not a throttled list; 24 B of response overhead
/// plus 16 B per message leave no room for any body.
#[tokio::test]
async fn a_serve_limit_the_message_does_not_fit_serves_nothing_and_keeps_it() {
    let (mut hub, mut sender, mut recipient) = three_helpers().await;
    one_message_stored(
        &mut hub,
        &mut sender,
        &mut recipient,
        "pn_enable 1 serve_limit_bytes=40",
    )
    .await;

    recipient.command("sync");
    let synced = pump3(
        &mut hub,
        &mut sender,
        &mut recipient,
        Duration::from_secs(30),
        |_, _, c| c.seen("lxmf_sync_done"),
    )
    .await;
    assert!(
        synced,
        "the sync must conclude; recipient events: {:?}; recipient logs: {:?}",
        recipient.events, recipient.logs
    );
    assert_eq!(
        recipient
            .find_last("lxmf_sync_done")
            .and_then(|event| event.field("count")),
        Some("0"),
        "a 40-byte serve limit must serve nothing: {:?}",
        recipient.events
    );
    hub.command("pn_store_size");
    let kept = pump3(
        &mut hub,
        &mut sender,
        &mut recipient,
        Duration::from_secs(10),
        |a, _, _| {
            a.find_last("lxmf_pn_store")
                .and_then(|event| event.field("size"))
                == Some("1")
        },
    )
    .await;
    assert!(
        kept,
        "the unserved message must stay in the store: {:?}",
        hub.find_last("lxmf_pn_store")
    );
}

/// A bare `pn_enable` announces and enforces the reference's limits, lxmd's
/// 256 KB per message and 40 times that per sync
/// (`reference/LXMF/LXMF/LXMRouter.py:55`, `:59`), as the Python helper's
/// stock router does (order 1664). Until then the helper ran the board's
/// 4 KB / 32 KB, so a cell comparing the two stacks without naming a limit
/// compared two node classes. An upload is held to the sync limit
/// (`propagation_resource_advertised`, `LXMRouter.py:2220-2224`): 40 000
/// bytes is above the old 32 KB and far below 10 240 KB, so the hub storing
/// it is the default under test. Negative control, run when the test was
/// written: the same script with `pn_enable 1 transfer_limit_kb=4
/// sync_limit_kb=32` never stores it.
#[tokio::test]
async fn a_bare_pn_enable_takes_an_upload_above_the_board_limits() {
    let (mut hub, mut sender, mut recipient) = three_helpers().await;
    let text = "x".repeat(40_000);
    let (sender_hash, body) =
        one_message_of(&mut hub, &mut sender, &mut recipient, "pn_enable 1", &text).await;

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
        "the sync must fetch the 40 kB message; recipient events: {:?}; recipient logs: {:?}",
        recipient.events, recipient.logs
    );
    assert!(
        recipient.received(&sender_hash, &body).is_some(),
        "the 40 kB message must arrive intact"
    );
}
