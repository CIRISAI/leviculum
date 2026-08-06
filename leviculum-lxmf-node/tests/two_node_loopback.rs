//! Two `lxmf-node` helpers, in process, over TCP loopback — the whole protocol
//! against itself.
//!
//! This is the in-tree half of the evidence for Codeberg #196's first
//! criterion. The other half is the periculum scenario
//! (`conformance/lxmf_two_node_leviculum_python.toml`), which puts our stack
//! against Python's inside containers; that one needs docker and the reviewer's
//! rig. This one needs neither, so it is the part that can be run on every
//! commit, and it is what proves the helper works at all before a scenario is
//! ever scheduled.
//!
//! What it covers, and why each is a *plural* rather than one case in hand:
//!
//! * **Both directions.** A→B and B→A are separate deliveries with separate
//!   links; a helper that only handled the initiator side would pass a
//!   one-directional test.
//! * **Every command the driver sends.** `announce`, `wait_for_peer`, `send`
//!   and `quit` all run here; `lxmf_start` is the process coming up, which is
//!   `lxmf_ready` below.
//! * **Both outcomes of `wait_for_peer`.** `..._ok` in the connected topology,
//!   `..._timeout` in the partitioned one. A helper that emitted `ok`
//!   unconditionally would pass every positive test in the suite.
//! * **The negative control.** `nothing_is_delivered_across_a_partition` is the
//!   same script with the TCP link removed. If it ever goes green alongside the
//!   positive test, the positive test is measuring the harness rather than the
//!   stack.
//!
//! Not covered here, and covered by the periculum scenario instead: Python on
//! the other end (this is our stack on both ends), and a real daemon in the
//! middle (these two nodes are directly connected, where the scenario's
//! helpers are shared-instance clients of `lnsd`/`rnsd`).

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use leviculum_lxmf_node::processor::{Emitter, HelperConfig, Input, LxmfHelperProcessor, Out};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};

/// Everything a single helper needs to be driven and observed.
struct Helper {
    node: ReticulumNode,
    inputs: Sender<Input>,
    lines: Receiver<Out>,
    /// Every `EVENT` line seen so far, parsed. Never drained: several
    /// assertions are "did this ever happen", which a consuming read cannot
    /// answer.
    events: Vec<Event>,
    /// `lxmf_ready`'s hash, once it has arrived.
    delivery_hash: Option<String>,
    _storage: tempfile::TempDir,
}

/// One parsed `EVENT` line, the same shape the real driver parses into
/// (`periculum/src/lxmf.rs`, `EventLine`).
#[derive(Debug, Clone)]
struct Event {
    name: String,
    fields: BTreeMap<String, String>,
}

impl Event {
    fn parse(line: &str) -> Option<Event> {
        let mut tokens = line.split_whitespace();
        if tokens.next()? != "EVENT" {
            return None;
        }
        let name = tokens.next()?.to_string();
        let fields = tokens
            .filter_map(|token| token.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Some(Event { name, fields })
    }

    fn field(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }
}

/// A free loopback port. The listener is dropped before the node binds, which
/// is the same small race every TCP test in the tree runs.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

impl Helper {
    /// Build and start one helper. `wire` decides whether this node listens,
    /// dials, or does neither (the partitioned case).
    async fn start(display_name: &str, wire: Wire) -> Helper {
        let storage = tempfile::tempdir().expect("tempdir");
        let (lines_tx, lines_rx) = mpsc::channel::<Out>();
        let (inputs_tx, inputs_rx) = mpsc::channel::<Input>();
        // The stamp and shutdown queues are unbounded senders whose receivers
        // are dropped immediately: no peer here advertises a stamp cost, and
        // the test decides when the node stops. A send on either fails rather
        // than blocks, which is exactly the behaviour a hook needs.
        let (stamps_tx, _) = tokio::sync::mpsc::unbounded_channel();
        let (shutdown_tx, _) = tokio::sync::mpsc::unbounded_channel();

        let processor = LxmfHelperProcessor::new(
            HelperConfig {
                display_name: display_name.as_bytes().to_vec(),
            },
            Emitter::new(lines_tx, Instant::now()),
            inputs_rx,
            stamps_tx,
            shutdown_tx,
        );

        let mut builder = ReticulumNodeBuilder::new()
            .enable_transport(false)
            .storage_path(storage.path().to_path_buf())
            .core_processor(processor);
        builder = match wire {
            Wire::Listen(addr) => builder.add_tcp_server(addr),
            Wire::Dial(addr) => builder.add_tcp_client(addr),
            Wire::Alone => builder,
        };
        let mut node = builder.build().await.expect("build node");
        node.start().await.expect("start node");

        Helper {
            node,
            inputs: inputs_tx,
            lines: lines_rx,
            events: Vec::new(),
            delivery_hash: None,
            _storage: storage,
        }
    }

    fn command(&self, line: &str) {
        self.inputs
            .send(Input::Line(line.to_string()))
            .expect("helper input channel is open");
    }

    /// Absorb everything the helper has said since the last call.
    fn drain(&mut self) {
        while let Ok(out) = self.lines.try_recv() {
            match out {
                Out::Event(line) => {
                    if let Some(event) = Event::parse(&line) {
                        if event.name == "lxmf_ready" {
                            self.delivery_hash = event.field("hash").map(str::to_string);
                        }
                        self.events.push(event);
                    }
                }
                // Kept visible: a failing run's diagnosis is usually here.
                Out::Log(line) => eprintln!("{line}"),
            }
        }
    }

    fn seen(&self, name: &str) -> bool {
        self.events.iter().any(|event| event.name == name)
    }

    fn find(&self, name: &str) -> Option<&Event> {
        self.events.iter().find(|event| event.name == name)
    }

    /// A received message matching the driver's own predicate: right source,
    /// right body (`periculum/src/executor.rs`, `lxmf_received_verdict`).
    fn received(&self, src: &str, body_b64: &str) -> Option<&Event> {
        self.events.iter().find(|event| {
            event.name == "lxmf_msg_received"
                && event.field("src") == Some(src)
                && event.field("body_b64") == Some(body_b64)
        })
    }
}

enum Wire {
    Listen(SocketAddr),
    Dial(SocketAddr),
    Alone,
}

/// Poll both helpers until `done` holds or the deadline passes.
async fn pump_until<F>(a: &mut Helper, b: &mut Helper, budget: Duration, mut done: F) -> bool
where
    F: FnMut(&Helper, &Helper) -> bool,
{
    let deadline = Instant::now() + budget;
    loop {
        a.drain();
        b.drain();
        if done(a, b) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The base64 the driver would build for this body
/// (`periculum/src/executor.rs`: `B64.encode(body.as_bytes())`).
fn body_b64(body: &str) -> String {
    leviculum_lxmf_node::protocol::b64_encode(body.as_bytes())
}

/// The `wait_for_peer` window, the same on both runs.
///
/// It has to be long enough for a loopback announce to install a path and
/// short enough that the partitioned run does not spend it waiting. Path
/// install over one TCP hop is sub-second here — `core_processor_seam.rs`
/// budgets 5 s for the same step — so 10 s is slack, not a cadence.
const WAIT_SECS: u64 = 10;

/// Run the full six-verb script over whatever topology `connected` implies.
///
/// Returns the two helpers with every event they emitted, so each test can ask
/// its own questions of the same run.
async fn run_script(connected: bool) -> (Helper, Helper) {
    let addr: SocketAddr = format!("127.0.0.1:{}", free_port()).parse().unwrap();
    let (a_wire, b_wire) = if connected {
        (Wire::Listen(addr), Wire::Dial(addr))
    } else {
        (Wire::Alone, Wire::Alone)
    };
    let mut alice = Helper::start("Alice", a_wire).await;
    let mut bob = Helper::start("Bob", b_wire).await;

    // lxmf_start: the helper is up once it has named its delivery destination.
    let ready = pump_until(&mut alice, &mut bob, Duration::from_secs(20), |a, b| {
        a.delivery_hash.is_some() && b.delivery_hash.is_some()
    })
    .await;
    assert!(ready, "both helpers must emit lxmf_ready");
    let alice_hash = alice.delivery_hash.clone().unwrap();
    let bob_hash = bob.delivery_hash.clone().unwrap();
    assert_eq!(alice_hash.len(), 32, "a delivery hash is 16 bytes of hex");
    assert_ne!(alice_hash, bob_hash, "two helpers, two destinations");

    // lxmf_wait_for_peer, with lxmf_announce re-driven underneath it. A single
    // announce can lose the race with the TCP connect, which is why the
    // reference harness re-announces too (`announce_until_daemon_has_path` in
    // `leviculum-std/tests/rnsd_interop/lxmf_interop_tests.rs`).
    alice.command(&format!("wait_for_peer {bob_hash} {WAIT_SECS}"));
    bob.command(&format!("wait_for_peer {alice_hash} {WAIT_SECS}"));
    let answered =
        |h: &Helper| h.seen("lxmf_wait_for_peer_ok") || h.seen("lxmf_wait_for_peer_timeout");
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
        if answered(&alice) && answered(&bob) {
            settled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        settled,
        "both helpers must answer wait_for_peer within its own window plus grace"
    );

    if connected {
        // lxmf_send, both directions, then lxmf_assert_received on each.
        alice.command(&format!(
            "send {bob_hash} {}",
            body_b64("hello bob from alice")
        ));
        bob.command(&format!(
            "send {alice_hash} {}",
            body_b64("hello alice from bob")
        ));
        let delivered = pump_until(&mut alice, &mut bob, Duration::from_secs(60), |a, b| {
            b.received(&alice_hash, &body_b64("hello bob from alice"))
                .is_some()
                && a.received(&bob_hash, &body_b64("hello alice from bob"))
                    .is_some()
        })
        .await;
        assert!(
            delivered,
            "both messages must arrive: {:?}",
            (&alice.events, &bob.events)
        );
    }

    // lxmf_stop.
    alice.command("quit");
    bob.command("quit");
    pump_until(&mut alice, &mut bob, Duration::from_secs(5), |a, b| {
        a.seen("lxmf_shutdown") && b.seen("lxmf_shutdown")
    })
    .await;

    let _ = alice.node.stop().await;
    let _ = bob.node.stop().await;
    alice.drain();
    bob.drain();
    (alice, bob)
}

/// The positive run: two helpers on one TCP link deliver in both directions,
/// and every field the driver reads is the field it expects.
#[tokio::test]
async fn two_helpers_exchange_messages_in_both_directions() {
    let (alice, bob) = run_script(true).await;
    let alice_hash = alice.delivery_hash.clone().unwrap();
    let bob_hash = bob.delivery_hash.clone().unwrap();

    // wait_for_peer resolved positively on both sides — the plural the
    // scenario's "kennenlernen" step asserts.
    for (who, helper) in [("alice", &alice), ("bob", &bob)] {
        assert!(
            helper.seen("lxmf_wait_for_peer_ok"),
            "{who} must report wait_for_peer_ok, got {:?}",
            helper.events
        );
        assert!(
            !helper.seen("lxmf_wait_for_peer_timeout"),
            "{who} must not also report a timeout"
        );
        assert!(
            helper.seen("lxmf_announce_sent"),
            "{who} must ack its announce"
        );
        assert!(helper.seen("lxmf_msg_sent"), "{who} must ack its send");
        assert!(helper.seen("lxmf_shutdown"), "{who} must ack quit");
        assert!(
            !helper.seen("lxmf_error"),
            "{who} reported an error: {:?}",
            helper.find("lxmf_error")
        );
    }

    // The received events, checked the way the driver checks them plus the two
    // fields it only prints. `sig_valid=true` is load-bearing: it is the
    // difference between "some bytes arrived" and "the peer we announced to
    // signed them".
    for (who, helper, src, body) in [
        ("bob", &bob, &alice_hash, "hello bob from alice"),
        ("alice", &alice, &bob_hash, "hello alice from bob"),
    ] {
        let event = helper
            .received(src, &body_b64(body))
            .unwrap_or_else(|| panic!("{who} must have received '{body}': {:?}", helper.events));
        assert_eq!(
            event.field("sig_valid"),
            Some("true"),
            "{who}: the sender's announce was seen, so the signature must verify"
        );
        assert_eq!(
            event.field("transport_encryption"),
            Some("Curve25519"),
            "{who}: what Python reports for a SINGLE/LINK arrival"
        );
    }
}

/// The negative control: the identical script with no link between the nodes.
///
/// Everything local still happens — both helpers come up, both announce, both
/// accept the commands — and nothing crosses. If this test ever passes its
/// positive twin's assertions, the positive test is measuring the harness.
#[tokio::test]
async fn nothing_is_delivered_across_a_partition() {
    let (alice, bob) = run_script(false).await;

    for (who, helper) in [("alice", &alice), ("bob", &bob)] {
        assert!(
            helper.delivery_hash.is_some(),
            "{who} must still come up: a partition is not a startup failure"
        );
        assert!(
            helper.seen("lxmf_wait_for_peer_timeout"),
            "{who} must report wait_for_peer_timeout, got {:?}",
            helper.events
        );
        assert!(
            !helper.seen("lxmf_wait_for_peer_ok"),
            "{who} must not claim to have found an unreachable peer"
        );
        assert!(
            !helper.seen("lxmf_msg_received"),
            "{who} received a message with no path to anyone: {:?}",
            helper.find("lxmf_msg_received")
        );
    }
}
