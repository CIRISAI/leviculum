//! A reading crosses the wire between two `lxmf-node` helpers.
//!
//! #344 proved the receive path at the `RouterEvent::MessageReceived` seam —
//! a message composed in-process, handed straight to `report`. That is the
//! decoder and both sinks, and it is not the wire: it cannot tell whether a
//! `FIELD_TELEMETRY` survives being packed, signed, encrypted, sent over a
//! link and unpacked at the far end, because no message in that test was ever
//! sent. `send_telemetry` (`protocol.rs`) is the verb that closes it, and this
//! is the test it exists for.
//!
//! The comparison is deliberately byte-level in one place: the hex the driver
//! types into `send_telemetry` must come back out of the receiver as
//! `fields_hex`, unchanged. Everything else here — the decoded columns, the
//! `telemetry.jsonl` row — is the same rendering #344 already covers, checked
//! once more on bytes that made the trip.
//!
//! **The positive control is a plain `send`**, delivered in the same run,
//! after the reading. It proves two things at once that "B saw one telemetry
//! line" cannot prove alone: that an ordinary message contributes no
//! telemetry line (the receive path does not fire on every arrival), and that
//! the absence is not simply a dead link — the control's own
//! `lxmf_msg_received` is asserted before the count is.
//!
//! Topology and bring-up are `two_node_loopback.rs`'s, minus the partition
//! arm; the harness is shared in `common/`.

mod common;

use std::time::{Duration, Instant};

use common::{body_b64, pump_until, Helper, Setup, Wire};
use leviculum_lxmf::msgpack::Number;
use leviculum_lxmf::telemetry::{Battery, Location, Telemetry};
use leviculum_lxmf_node::protocol::hex_encode;

/// The `wait_for_peer` window, as in `two_node_loopback.rs`: slack over a
/// sub-second loopback path install, not a cadence.
const WAIT_SECS: u64 = 10;

/// The reading a walk sends: where, how full the battery is, and when.
///
/// The same values `telemetry.rs`'s unit tests use, so a difference between
/// this run and those is the wire and nothing else.
fn walk_reading() -> Telemetry {
    Telemetry {
        time: Some(1_790_000_000),
        location: Some(Location {
            latitude_e6: 52_520_008,
            longitude_e6: 13_404_954,
            altitude_e2: 3_412,
            speed_e2: 137,
            bearing_e2: 9_150,
            accuracy_e2: 480,
            last_update: 1_789_999_995,
        }),
        battery: Some(Battery {
            charge_percent: Number::Int(87),
            charging: Some(false),
            temperature: None,
        }),
        ..Telemetry::default()
    }
}

/// Two helpers on one TCP link that have each learned the other's identity
/// and path, returned with their delivery hashes.
async fn connected_pair() -> (Helper, String, Helper, String) {
    let mut alice = Helper::start(Setup::new("Alice", Wire::listen_any())).await;
    let mut bob = Helper::start(Setup::new("Bob", Wire::Dial(alice.listen_addr()))).await;

    let ready = pump_until(&mut alice, &mut bob, Duration::from_secs(20), |a, b| {
        a.delivery_hash.is_some() && b.delivery_hash.is_some()
    })
    .await;
    assert!(ready, "both helpers must emit lxmf_ready");
    let alice_hash = alice.delivery_hash.clone().expect("alice is ready");
    let bob_hash = bob.delivery_hash.clone().expect("bob is ready");

    // Both directions: Alice needs Bob's identity to encrypt to him, Bob
    // needs Alice's to verify what he gets. A re-driven announce underneath,
    // because one can lose the race with the TCP connect.
    alice.command(&format!("wait_for_peer {bob_hash} {WAIT_SECS}"));
    bob.command(&format!("wait_for_peer {alice_hash} {WAIT_SECS}"));
    let deadline = Instant::now() + Duration::from_secs(WAIT_SECS + 10);
    let mut next_announce = Instant::now();
    let mut settled = false;
    while Instant::now() < deadline {
        if Instant::now() >= next_announce {
            alice.command("announce");
            bob.command("announce");
            next_announce = Instant::now() + Duration::from_secs(3);
        }
        alice.drain();
        bob.drain();
        if alice.seen("lxmf_wait_for_peer_ok") && bob.seen("lxmf_wait_for_peer_ok") {
            settled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        settled,
        "both helpers must find each other: {:?}",
        (&alice.events, &bob.events)
    );
    (alice, alice_hash, bob, bob_hash)
}

fn count(helper: &Helper, name: &str) -> usize {
    helper.events.iter().filter(|e| e.name == name).count()
}

#[tokio::test]
async fn a_reading_sent_over_the_wire_reaches_the_receiver_line_and_row() {
    let (mut alice, alice_hash, mut bob, bob_hash) = connected_pair().await;

    let reading = walk_reading();
    let blob_hex = hex_encode(&reading.encode());
    alice.command(&format!("send_telemetry {bob_hash} {blob_hex}"));

    let arrived = pump_until(&mut alice, &mut bob, Duration::from_secs(60), |_, b| {
        b.seen("lxmf_telemetry_received")
    })
    .await;
    assert!(
        arrived,
        "the reading must reach Bob: {:?}",
        (&alice.events, &bob.events)
    );

    // The sender's ack names what it carried. The rest of the line is the
    // one every scenario already parses, so an old driver reading it sees
    // nothing new in the fields it knows.
    let sent = alice
        .find("lxmf_msg_sent")
        .expect("alice must ack the send");
    assert_eq!(sent.field("dst"), Some(bob_hash.as_str()));
    assert_eq!(
        sent.field("fields"),
        Some("telemetry"),
        "the ack must say the message carried a reading: {sent:?}"
    );
    assert_eq!(
        sent.field("body_b64"),
        Some(""),
        "a report has an empty body: {sent:?}"
    );
    assert!(
        !alice.seen("lxmf_error"),
        "alice reported an error: {:?}",
        alice.find("lxmf_error")
    );

    // The receiver's line. `src` and `via` are both Alice: this reading was
    // measured by the sender, not relayed out of a collector's stream.
    let event = bob
        .find("lxmf_telemetry_received")
        .expect("bob must report the reading");
    assert_eq!(event.field("src"), Some(alice_hash.as_str()));
    assert_eq!(event.field("via"), Some(alice_hash.as_str()));
    assert_eq!(event.field("time"), Some("1790000000"));
    assert_eq!(event.field("lat"), Some("52.520008"));
    assert_eq!(event.field("lon"), Some("13.404954"));
    assert_eq!(event.field("alt"), Some("34.12"));
    assert_eq!(event.field("speed"), Some("1.37"));
    assert_eq!(event.field("bearing"), Some("91.50"));
    assert_eq!(event.field("accuracy"), Some("4.80"));
    assert_eq!(event.field("fix_time"), Some("1789999995"));
    assert_eq!(event.field("battery_pct"), Some("87"));
    assert_eq!(event.field("battery_charging"), Some("false"));
    assert_eq!(event.field("battery_temp_c"), Some("none"));
    // The byte-level claim: what the driver typed is what the far end holds.
    assert_eq!(
        event.field("fields_hex"),
        Some(blob_hex.as_str()),
        "the packed blob must cross the wire unchanged"
    );
    assert!(
        !bob.seen("lxmf_telemetry_undecodable"),
        "a reading that decodes in-process must decode after a round trip: {:?}",
        bob.find("lxmf_telemetry_undecodable")
    );

    // The durable half: the row is in Bob's `telemetry.jsonl`, carrying the
    // same reading plus the two clocks.
    let rows = bob.telemetry_rows();
    assert_eq!(rows.len(), 1, "one reading, one row: {rows:?}");
    let row = &rows[0];
    for token in [
        &format!("\"src\":\"{alice_hash}\""),
        &format!("\"via\":\"{alice_hash}\""),
        "\"status\":\"ok\"",
        "\"time\":1790000000",
        "\"lat\":52.520008",
        "\"lon\":13.404954",
        "\"alt\":34.12",
        "\"speed\":1.37",
        "\"bearing\":91.50",
        "\"accuracy\":4.80",
        "\"fix_time\":1789999995",
        "\"battery_pct\":87",
        "\"battery_charging\":false",
        "\"battery_temp_c\":null",
        "\"producers\":null",
        &format!("\"raw\":\"{blob_hex}\""),
    ] {
        assert!(row.contains(token), "row lacks {token}: {row}");
    }

    // The positive control, delivered after the reading: a plain `send`
    // arrives as an ordinary message and adds neither a line nor a row.
    let control = "no reading in here";
    alice.command(&format!("send {bob_hash} {}", body_b64(control)));
    let control_arrived = pump_until(&mut alice, &mut bob, Duration::from_secs(60), |_, b| {
        b.received(&alice_hash, &body_b64(control)).is_some()
    })
    .await;
    assert!(
        control_arrived,
        "the control message must arrive, or its silence proves nothing: {:?}",
        bob.events
    );
    assert_eq!(
        count(&bob, "lxmf_msg_received"),
        2,
        "the reading and the control both arrived as messages: {:?}",
        bob.events
    );
    assert_eq!(
        count(&bob, "lxmf_telemetry_received"),
        1,
        "only the reading is a reading: {:?}",
        bob.events
    );
    assert_eq!(
        bob.telemetry_rows().len(),
        1,
        "a message without fields files no row"
    );

    alice.command("quit");
    bob.command("quit");
    pump_until(&mut alice, &mut bob, Duration::from_secs(5), |a, b| {
        a.seen("lxmf_shutdown") && b.seen("lxmf_shutdown")
    })
    .await;
    let _ = alice.node.stop().await;
    let _ = bob.node.stop().await;
}

/// A blob the helper cannot read never reaches the wire: the verb refuses it
/// at parse time and says so, so a scenario cannot mistake garbage for a
/// delivery failure.
#[tokio::test]
async fn a_blob_that_is_not_a_telemeter_map_is_refused_before_it_is_sent() {
    let (mut alice, _alice_hash, mut bob, bob_hash) = connected_pair().await;

    // `a26e6f` is a msgpack string, not a map.
    alice.command(&format!("send_telemetry {bob_hash} a26e6f"));
    let refused = pump_until(&mut alice, &mut bob, Duration::from_secs(10), |a, _| {
        a.seen("lxmf_error")
    })
    .await;
    assert!(refused, "the blob must be refused: {:?}", alice.events);
    let error = alice.find("lxmf_error").expect("the refusal was seen");
    let detail = error.field("detail").unwrap_or_default();
    assert!(
        detail.starts_with("usage:_send_telemetry"),
        "the refusal must carry the usage line: {detail}"
    );
    assert!(
        !alice.seen("lxmf_msg_sent"),
        "a refused verb must not have sent anything"
    );
    assert!(
        !bob.seen("lxmf_telemetry_received") && !bob.seen("lxmf_telemetry_undecodable"),
        "nothing reached Bob: {:?}",
        bob.events
    );

    alice.command("quit");
    bob.command("quit");
    pump_until(&mut alice, &mut bob, Duration::from_secs(5), |a, b| {
        a.seen("lxmf_shutdown") && b.seen("lxmf_shutdown")
    })
    .await;
    let _ = alice.node.stop().await;
    let _ = bob.node.stop().await;
}
