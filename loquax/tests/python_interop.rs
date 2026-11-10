//! `loquax send` against the real Python LXMF stack (`reference/LXMF`).
//!
//! Standing policy is that every feature gets interop tests against real
//! Python, positive and negative, before it merges. This is that test for the
//! send path, and it drives the shipped binary rather than the library, so
//! what is asserted is exactly what a cron job would get: an exit code, a
//! silent stdout, and a message in someone else's inbox.
//!
//! The interop claim rests on one identity: the id our `LNMSG_ENQUEUED` line
//! carries is the `message_hash` **Python** computed over the message it
//! received. Both sides derive it independently from the LXMF wire bytes, so
//! the match is agreement between two stacks and not a value we handed over.
//!
//! ```text
//!   loquax (subprocess)                    python3 scripts/test_daemon.py
//!         │                                    (real RNS + LXMF.LXMRouter)
//!         │ shared-instance IPC                     │
//!         v                                         │ TCP
//!   in-process Rust daemon ────────────────────────-┘
//!   (share_instance + TCP client)
//! ```
//!
//! The Rust daemon is built in-process rather than by spawning `lnsd`: it is
//! the same `ReticulumNodeBuilder` configuration `lnsd` assembles from a
//! config file, and `CARGO_BIN_EXE_` only reaches this package's own binaries.
//! What is under test is `loquax`, and it attaches over the same IPC socket
//! either way — the drop-in property is exercised more directly by the
//! `rnsd_interop` suites, which point the same clients at both daemons.

use std::time::Duration;

#[path = "support/python.rs"]
mod python;

use python::{Mesh, PythonPeer};

/// Poll the Python router until it has delivered the message with this id.
async fn wait_for_python(peer: &PythonPeer, id: &str, within: Duration) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let seen = peer.received();
        if let Some(message) = seen
            .iter()
            .find(|m| m.get("message_hash").and_then(|v| v.as_str()) == Some(id))
        {
            return message.clone();
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("Python never received {id}; it holds: {seen:?}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn hex_bytes(message: &serde_json::Value, key: &str) -> Vec<u8> {
    hex::decode(
        message
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("{key} is missing from {message}")),
    )
    .unwrap_or_else(|_| panic!("{key} is not hex in {message}"))
}

/// The positive control's knob for the renamed-sender check (leviculum
/// 1465): with `LOQUAX_TEST_HOLD_RENAME_S=<seconds>` the Python peer stores
/// an announce that renames a sender it knows that much later, which is the
/// order the land gate on b07e759b saw. Unset in every gate.
fn held_rename_s() -> Option<f64> {
    std::env::var("LOQUAX_TEST_HOLD_RENAME_S")
        .ok()
        .and_then(|s| s.parse().ok())
}

/// Positive: a message sent by the shipped binary arrives in a real Python
/// LXMF inbox with its body and title intact, non-ASCII included.
#[tokio::test(flavor = "multi_thread")]
async fn lnmsg_send_reaches_a_python_lxmf_receiver() {
    // The in-process daemon's structured events (ANN_RX, PATH_ADD, ANN_PACED,
    // ANN_TX, IFACE_DOWN), dumped to stderr if the test panics. The renamed
    // announce went missing once under a loaded host (order 1584) and the
    // run carried no line from the instance to say where.
    let _instance_events = leviculum_std::test_support::event_log::init_event_log();
    let peer = PythonPeer::start();
    let python_address = peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    peer.lxmf_announce();

    // Non-ASCII in both body and title: LXMF carries bytes, and a messenger
    // that mangles them is useless to everyone outside en_US.
    let body = "Grüße vom Dachboden — 91 % voll ☂";
    let title = "Füllstand";

    let sent = mesh
        .send(
            vec![
                python_address.clone(),
                "--title".to_string(),
                title.to_string(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            format!("{body}\n").into_bytes(),
        )
        .await;

    assert_eq!(sent.code, Some(0), "{sent}");
    // Byte-exact, not "contains no hex blob": an assertion that merely failed
    // to find an id would pass for any other thing we started printing.
    assert_eq!(
        sent.stdout, "",
        "a successful send says nothing on stdout: {sent}"
    );

    // The id from our own event log; `wait_for_python` accepts it only if it
    // equals the `message_hash` Python computed over what it received.
    let id = sent.enqueued_id();
    let message = wait_for_python(&peer, &id, Duration::from_secs(40)).await;
    assert_eq!(
        String::from_utf8(hex_bytes(&message, "content")).expect("the body round-trips as UTF-8"),
        body,
        "the body must arrive byte for byte"
    );
    assert_eq!(
        String::from_utf8(hex_bytes(&message, "title")).expect("the title round-trips as UTF-8"),
        title
    );
    assert_eq!(
        message.get("destination_hash").and_then(|v| v.as_str()),
        Some(python_address.as_str())
    );
    // loquax announces its own delivery destination before sending precisely so
    // this holds: without the announce the reference marks our message
    // SOURCE_UNKNOWN and nobody could reply to us.
    assert_eq!(
        message.get("signature_validated").and_then(|v| v.as_bool()),
        Some(true),
        "Python must be able to validate our signature: {message}"
    );

    // The name a recipient actually sees. Python resolves it out of the
    // announce app_data the way its clients do, so this is the only check that
    // covers the whole path: resolution, announce, wire, reference decoder.
    // The expected value is resolved here by the same code the child ran,
    // rather than hardcoded, because the account name differs per host.
    let expected_name = loquax::display_name::from_process(None)
        .expect("no override is set for the test process")
        .name;
    assert_eq!(
        message.get("source_display_name").and_then(|v| v.as_str()),
        Some(expected_name.as_str()),
        "Python must see the operator's name, not the tool's: {message}"
    );
    assert_ne!(
        message.get("source_display_name").and_then(|v| v.as_str()),
        Some("Loquax"),
        "'Loquax' names the program, not the person it sends for"
    );

    // And `--from` is what an operator uses when the bare account name is
    // ambiguous — three hosts, one user. Same identity, so this run's announce
    // replaces the name Python holds for us.
    if let Some(seconds) = held_rename_s() {
        peer.rpc(
            "lxmf_hold_renames",
            serde_json::json!({ "seconds": seconds }),
        );
    }
    let overridden = mesh
        .send(
            vec![
                python_address.clone(),
                "--from".to_string(),
                "lew@schneckenschreck".to_string(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            b"second line\n".to_vec(),
        )
        .await;
    assert_eq!(overridden.code, Some(0), "{overridden}");
    assert_eq!(
        overridden.stdout, "",
        "a successful send says nothing on stdout: {overridden}"
    );
    let overridden_id = overridden.enqueued_id();
    let message = wait_for_python(&peer, &overridden_id, Duration::from_secs(40)).await;
    assert_eq!(
        String::from_utf8(hex_bytes(&message, "content")).expect("the body round-trips as UTF-8"),
        "second line",
        "the renamed sender's message must arrive itself: {message}"
    );
    // The new name is asserted where Python holds it, not on the message.
    // Names come from announces, and a recipient shows the old one until the
    // new announce lands, which is the reference's model and its clients'.
    // Nothing orders that announce ahead of the message: the instance holds
    // a client's announce for the fresh-interface pacer (up to 600 ms after
    // the previous own announce on that TCP link, `ANN_PACED`, and a timer
    // releases it), while the message's link packets are forwarded as they
    // arrive. On an idle host the announce still won by about 230 ms in 20 of
    // 20 runs; the land gate on b07e759b starved the timer and the message
    // came first. A relay would add its rebroadcast wait to the announce and
    // none to the link, so a multi-hop path orders even less.
    let source = message
        .get("source_hash")
        .and_then(|v| v.as_str())
        .expect("the message carries its source")
        .to_string();
    let shown = message.get("source_display_name").and_then(|v| v.as_str());
    assert!(
        shown == Some(expected_name.as_str()) || shown == Some("lew@schneckenschreck"),
        "the message shows the old name or the new one, nothing else: {message}"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        let known = peer.rpc(
            "lxmf_known_name",
            serde_json::json!({ "destination_hash": source }),
        );
        if known.as_str() == Some("lew@schneckenschreck") {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            // Each place the renamed announce can stop says what it holds:
            // the second client's own events (did it announce?), the
            // instance's on the panic dump above, and Python's interface
            // (a held announce, an ingress burst) and path table.
            panic!(
                "--from must reach the receiver verbatim; Python holds {known} and heard {}\n\
                 Python has a path to {source}: {}\nPython's interfaces: {}\n\
                 the renamed send: {overridden}",
                peer.rpc("lxmf_names_heard", serde_json::json!({})),
                peer.rpc("has_path", serde_json::json!({ "hash": source })),
                peer.rpc("get_interfaces", serde_json::json!({})),
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// Negative: an address that exists nowhere in the mesh must not produce a
/// success exit — a script that trusted exit 0 here would report a status line
/// nobody ever received. Silence alone means nothing now that a success is
/// silent too; the exit code is the whole signal, so this asserts it exactly.
#[tokio::test(flavor = "multi_thread")]
async fn a_destination_that_does_not_exist_is_not_a_success() {
    let peer = PythonPeer::start();
    peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    peer.lxmf_announce();

    // Well-formed, and belonging to nobody: same length and shape as the real
    // one above, so what is being tested is reachability and not parsing.
    let nowhere = "0123456789abcdef0123456789abcdef";

    let sent = mesh
        .send(
            vec![
                nowhere.to_string(),
                "--timeout".to_string(),
                "8".to_string(),
            ],
            b"this goes nowhere\n".to_vec(),
        )
        .await;

    assert_ne!(
        sent.code,
        Some(0),
        "an unreachable address is not a success"
    );
    assert_eq!(sent.code, Some(1), "{sent}");
    assert_eq!(sent.stdout, "", "stdout stays empty on failure too: {sent}");
    assert!(
        sent.stderr.contains(nowhere),
        "the error must name the address it could not reach: {sent}"
    );
    assert!(
        peer.received().is_empty(),
        "nothing may have reached the Python router"
    );

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// `--via auto`, recipient online: the direct delivery link comes up, the
/// message arrives at the real Python receiver, and the exit code is 0 —
/// never 3, because no mailbox was involved. This is the leg that has to
/// work for auto to be an honest default rather than a propagation tax.
#[tokio::test(flavor = "multi_thread")]
async fn via_auto_delivers_direct_when_the_python_receiver_is_online() {
    let peer = PythonPeer::start();
    let python_address = peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    peer.lxmf_announce();

    let sent = mesh
        .send(
            vec![
                python_address.clone(),
                "--via".to_string(),
                "auto".to_string(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            b"direct while online\n".to_vec(),
        )
        .await;

    assert_eq!(sent.code, Some(0), "{sent}");
    assert_eq!(sent.stdout, "", "a successful send says nothing: {sent}");
    assert!(
        sent.log.contains("LNMSG_VIA")
            && sent.log.contains("method=direct")
            && sent.log.contains("reason=link-delivery"),
        "the decision line must record the direct leg winning: {sent}"
    );

    let id = sent.enqueued_id();
    let message = wait_for_python(&peer, &id, Duration::from_secs(40)).await;
    assert_eq!(
        String::from_utf8(hex_bytes(&message, "content")).expect("utf8"),
        "direct while online"
    );

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// The `t=` of the first event-log line that starts with `prefix` and
/// carries every one of `fields`.
fn event_ms(log: &str, prefix: &str, fields: &[&str]) -> Option<u64> {
    log.lines()
        .filter(|line| line.starts_with(prefix))
        .find(|line| {
            fields
                .iter()
                .all(|field| line.split_whitespace().any(|token| token == *field))
        })
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|token| token.strip_prefix("t="))
                .and_then(|t| t.parse().ok())
        })
}

/// The compose line's send against a stock Python LXMF receiver on one
/// daemon: the direct leg is proven well inside a second (leviculum 1002).
///
/// Lew's hand test and the 1001 cell measured 3985 to 4008 ms from Enter to
/// the receive on one host, four runs alike, with the wire exchange itself
/// about 4 ms: the router held a message queued between two processing
/// passes until the next pass of its four-second interval. The reference
/// sends from `handle_outbound` at once, so the same receiver took 3 ms from
/// `lxmf_node send`. Measured between our own `LNMSG_ENQUEUED` and the
/// proof's `LNMSG_STATE`, so attaching and resolving are not in the number;
/// 2 s leaves the link handshake a wide margin on a loaded runner and is
/// still half of what the old interval cost.
#[tokio::test(flavor = "multi_thread")]
async fn via_auto_to_a_python_receiver_is_proven_well_inside_a_second() {
    let peer = PythonPeer::start();
    let python_address = peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    peer.lxmf_announce();

    let sent = mesh
        .send(
            vec![
                python_address,
                "--via".to_string(),
                "auto".to_string(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            b"how long does this take\n".to_vec(),
        )
        .await;
    assert_eq!(sent.code, Some(0), "{sent}");

    let queued = event_ms(&sent.log, "LNMSG_ENQUEUED ", &[]).expect("an enqueue line");
    let proven = event_ms(&sent.log, "LNMSG_STATE ", &["state=Delivered"])
        .unwrap_or_else(|| panic!("the direct leg ends on the proof: {sent}"));
    let took = proven.saturating_sub(queued);
    eprintln!("enqueue to proof: {took} ms");
    assert!(
        took < 2_000,
        "{took} ms from the enqueue to the proof; the router waited for its \
         processing interval: {sent}"
    );

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// The whole mailbox round against a real Python propagation node: a sender
/// client uploads with `--via propagated` (exit 3, "a node holds it"), the
/// Python node stores it, and the recipient client drains it with the
/// genuine list/fetch/confirm round — the same flow the periculum control
/// cell runs with lxmd, here against `LXMRouter.enable_propagation` in
/// process. A second drain proves the confirm purged the store AND that the
/// seen store suppresses a reprint.
#[tokio::test(flavor = "multi_thread")]
async fn propagated_upload_and_fetch_round_trip_through_a_python_node() {
    let peer = PythonPeer::start();
    peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    let pn_hash = peer
        .rpc("lxmf_enable_propagation", serde_json::json!({}))
        .get("propagation_hash")
        .and_then(|v| v.as_str())
        .expect("the Python node reports its propagation hash")
        .to_string();
    peer.rpc("lxmf_announce_propagation_node", serde_json::json!({}));

    let sender_home = tempfile::tempdir().expect("sender home");
    let recipient_home = tempfile::tempdir().expect("recipient home");

    // The recipient announces itself once (any run announces on attach), so
    // the sender can encrypt to it; the drain coming back empty is itself
    // the negative half of the round.
    let first_drain = mesh
        .run(
            "fetch",
            recipient_home.path().to_path_buf(),
            vec![
                "--pn".to_string(),
                pn_hash.clone(),
                "--timeout".to_string(),
                "60".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(first_drain.code, Some(0), "{first_drain}");
    assert_eq!(
        first_drain.stdout, "",
        "an empty mailbox prints nothing: {first_drain}"
    );
    let recipient_address = {
        let line = first_drain
            .log
            .lines()
            .find(|line| line.starts_with("LNMSG_ATTACHED "))
            .expect("the drain attached");
        line.split_whitespace()
            .find_map(|token| token.strip_prefix("address="))
            .expect("the attach line carries our address")
            .to_string()
    };

    let sent = mesh
        .run(
            "send",
            sender_home.path().to_path_buf(),
            vec![
                recipient_address.clone(),
                "--via".to_string(),
                "propagated".to_string(),
                "--pn".to_string(),
                pn_hash.clone(),
                "--title".to_string(),
                "mailbox".to_string(),
                "--timeout".to_string(),
                "60".to_string(),
            ],
            b"held by the python node\n".to_vec(),
        )
        .await;
    assert_eq!(
        sent.code,
        Some(3),
        "a node holding the message is exit 3, not 0: {sent}"
    );
    assert_eq!(sent.stdout, "", "{sent}");

    let drained = mesh
        .run(
            "fetch",
            recipient_home.path().to_path_buf(),
            vec![
                "--pn".to_string(),
                pn_hash.clone(),
                "--timeout".to_string(),
                "60".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(drained.code, Some(0), "{drained}");
    assert!(
        drained.stdout.contains("held by the python node"),
        "the body must come back out of the Python node's store: {drained}"
    );
    assert!(
        drained.stdout.contains("title mailbox"),
        "the title travels too: {drained}"
    );

    let empty_again = mesh
        .run(
            "fetch",
            recipient_home.path().to_path_buf(),
            vec![
                "--pn".to_string(),
                pn_hash,
                "--timeout".to_string(),
                "60".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(empty_again.code, Some(0), "{empty_again}");
    assert_eq!(
        empty_again.stdout, "",
        "the confirm purged the store and the seen store suppresses reprints: {empty_again}"
    );

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// Negative: a propagation node hash that belongs to nobody must fail inside
/// the timeout with the no-node/no-path story, never hang and never
/// fabricate an empty success.
#[tokio::test(flavor = "multi_thread")]
async fn a_fetch_from_a_nonexistent_node_fails_cleanly() {
    let peer = PythonPeer::start();
    peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;

    let nowhere = "0123456789abcdef0123456789abcdef";
    let fetched = mesh
        .run(
            "fetch",
            mesh.home.path().to_path_buf(),
            vec![
                "--pn".to_string(),
                nowhere.to_string(),
                "--timeout".to_string(),
                "25".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(fetched.code, Some(1), "{fetched}");
    assert_eq!(fetched.stdout, "", "no messages may be invented: {fetched}");

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// The image field Python's router handed its delivery callback, read by
/// our codec: the format word and the picture's bytes.
#[cfg(feature = "photos")]
fn python_photo(message: &serde_json::Value) -> (String, Vec<u8>) {
    use leviculum_lxmf::msgpack;
    let packed = hex_bytes(message, "fields");
    let mut at = 0;
    let entries = msgpack::map_len(&packed, &mut at).expect("a fields map");
    for _ in 0..entries {
        let key = msgpack::read_int(&packed, &mut at).expect("an int key");
        let start = at;
        msgpack::skip(&packed, &mut at).expect("a value");
        if key == leviculum_lxmf::constants::FIELD_IMAGE {
            let fields = [(key, packed[start..at].to_vec())];
            let (format, data) = loquax::ui::photos::photo_parts(&fields).expect("[format, bytes]");
            return (format, data.to_vec());
        }
    }
    panic!("no image field in {message}");
}

/// Positive and negative (1220): `loquax send --photo` delivers the
/// picture to a real Python LXMF inbox as the image field Columba and
/// Sideband write, `[format, bytes]` under `FIELD_IMAGE`
/// (`reference/LXMF/LXMF/LXMF.py:13`), scaled to the size asked for,
/// upright and without its EXIF; over a link, since it is past one
/// packet. A file that is no picture is a usage error and sends nothing.
#[cfg(feature = "photos")]
#[tokio::test(flavor = "multi_thread")]
async fn a_photo_reaches_a_python_lxmf_receiver_as_the_image_field() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/photo-portrait.jpg"
    );
    let peer = PythonPeer::start();
    let python_address = peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    peer.lxmf_announce();

    for (size, edge) in [("small", 800u32), ("phone", 1600)] {
        let sent = mesh
            .send(
                vec![
                    python_address.clone(),
                    "--photo".to_string(),
                    fixture.to_string(),
                    "--photo-size".to_string(),
                    size.to_string(),
                    "--via".to_string(),
                    "direct".to_string(),
                    "--timeout".to_string(),
                    "40".to_string(),
                ],
                Vec::new(),
            )
            .await;
        assert_eq!(sent.code, Some(0), "{size}: {sent}");
        let id = sent.enqueued_id();
        let message = wait_for_python(&peer, &id, Duration::from_secs(40)).await;
        assert!(hex_bytes(&message, "content").is_empty(), "{size}: no text");
        let (format, data) = python_photo(&message);
        assert_eq!(format, "jpg");
        let (w, h) = loquax::photo::dimensions(&data).expect("a picture");
        assert_eq!((w.max(h), h > w), (edge, true), "{size}: {w}x{h}, upright");
        for secret in [&b"Exif"[..], b"Pixel 8", b"GPS"] {
            assert!(!data.windows(secret.len()).any(|w| w == secret), "{size}");
        }
    }

    let refused = mesh
        .send(
            vec![
                python_address.clone(),
                "--photo".to_string(),
                concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml").to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(refused.code, Some(2), "a usage error: {refused}");
    assert!(refused.stderr.contains("not a picture"), "{refused}");

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}

/// The file attachments field Python's router handed its delivery
/// callback, raw: the entries as the reference packed them.
fn python_files(message: &serde_json::Value) -> Vec<u8> {
    use leviculum_lxmf::msgpack;
    let packed = hex_bytes(message, "fields");
    let mut at = 0;
    let entries = msgpack::map_len(&packed, &mut at).expect("a fields map");
    for _ in 0..entries {
        let key = msgpack::read_int(&packed, &mut at).expect("an int key");
        let start = at;
        msgpack::skip(&packed, &mut at).expect("a value");
        if key == leviculum_lxmf::constants::FIELD_FILE_ATTACHMENTS {
            return packed[start..at].to_vec();
        }
    }
    panic!("no file attachments field in {message}");
}

/// Positive and negative (1221): `loquax send --file` delivers a file to a
/// real Python LXMF inbox as the field Columba and Sideband write,
/// `[[name, bytes]]` under `FIELD_FILE_ATTACHMENTS`
/// (`reference/LXMF/LXMF/LXMF.py:12`), the name a str as Columba sends it
/// and the bytes unchanged; over a link, since it is past one packet. A
/// file past the 990 KB a receiver takes is a usage error and sends
/// nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_reaches_a_python_lxmf_receiver_as_the_attachments_field() {
    use leviculum_lxmf::msgpack;
    // Its own directory, not a fixed name: two copies of this binary run side
    // by side under the quota survey and raced on one (1590).
    std::fs::create_dir_all(env!("CARGO_TARGET_TMPDIR")).expect("the target tmpdir");
    let scratch = tempfile::Builder::new()
        .prefix("1221-file-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("a scratch dir");
    let dir = scratch.path();
    let name = "Protokoll Grüße.pdf";
    let path = dir.join(name);
    let data: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    std::fs::write(&path, &data).expect("the file");
    let large = dir.join("large.bin");
    std::fs::write(&large, vec![0u8; loquax::ui::files::LARGEST + 1]).expect("the large file");

    let peer = PythonPeer::start();
    let python_address = peer.lxmf_init();
    let mut mesh = Mesh::start(peer.rns_port).await;
    peer.lxmf_announce();

    let sent = mesh
        .send(
            vec![
                python_address.clone(),
                "--file".to_string(),
                path.display().to_string(),
                "--via".to_string(),
                "direct".to_string(),
                "--timeout".to_string(),
                "40".to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(sent.code, Some(0), "{sent}");
    let id = sent.enqueued_id();
    let message = wait_for_python(&peer, &id, Duration::from_secs(40)).await;
    assert!(hex_bytes(&message, "content").is_empty(), "no text");
    let value = python_files(&message);
    let mut at = 0;
    assert_eq!(msgpack::array_len(&value, &mut at), Ok(1), "one file");
    assert_eq!(msgpack::array_len(&value, &mut at), Ok(2), "[name, bytes]");
    assert_eq!(
        msgpack::read_str(&value, &mut at),
        Ok(name),
        "the name, a str"
    );
    assert!(
        msgpack::read_bin(&value, &mut at) == Ok(&data[..]),
        "the bytes unchanged"
    );

    let refused = mesh
        .send(
            vec![
                python_address.clone(),
                "--file".to_string(),
                large.display().to_string(),
            ],
            Vec::new(),
        )
        .await;
    assert_eq!(refused.code, Some(2), "a usage error: {refused}");
    assert!(
        refused.stderr.contains("A message carries at most 990 KB"),
        "{refused}"
    );

    mesh.daemon.stop().await.expect("stop the Rust daemon");
}
