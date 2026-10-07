//! leviculum#77: the link telemetry a host reads, end to end on real nodes.
//! The link list, the lifecycle counters, the mirror check, and the metrics
//! `publish_metrics` records onto an installed recorder.

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leviculum_core::{Destination, DestinationType, Direction, Identity};
use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
// Named through leviculum_std alone, as a consumer without a direct
// leviculum-core dependency would (Codex review on PR #76).
use leviculum_std::{
    EventReceiver, LinkCensus, LinkCloseReason, LinkCountCheck, LinkInfo, LinkLifecycle, LinkRole,
    NodeEvent, NodeHeapCensus,
};
use metrics::{
    Counter, CounterFn, Gauge, GaugeFn, Histogram, Key, KeyName, Metadata, Recorder, SharedString,
    Unit,
};

static PORT_COUNTER: AtomicU16 = AtomicU16::new(60910);

fn next_port() -> u16 {
    loop {
        let candidate = PORT_COUNTER.fetch_add(1, Ordering::Relaxed);
        if candidate >= 60990 {
            PORT_COUNTER.store(60910, Ordering::Relaxed);
            continue;
        }
        if StdTcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return candidate;
        }
    }
}

/// A recorder that keeps the last value written to every series, keyed
/// `name{label=value,...}`.
#[derive(Default, Clone)]
struct Capture(Arc<Mutex<BTreeMap<String, f64>>>, Arc<Mutex<Vec<String>>>);

struct Series(String, Arc<Mutex<BTreeMap<String, f64>>>);

impl CounterFn for Series {
    fn increment(&self, value: u64) {
        *self.1.lock().unwrap().entry(self.0.clone()).or_default() += value as f64;
    }
    fn absolute(&self, value: u64) {
        self.1.lock().unwrap().insert(self.0.clone(), value as f64);
    }
}

impl GaugeFn for Series {
    fn increment(&self, value: f64) {
        *self.1.lock().unwrap().entry(self.0.clone()).or_default() += value;
    }
    fn decrement(&self, value: f64) {
        *self.1.lock().unwrap().entry(self.0.clone()).or_default() -= value;
    }
    fn set(&self, value: f64) {
        self.1.lock().unwrap().insert(self.0.clone(), value);
    }
}

fn series_name(key: &Key) -> String {
    let labels: Vec<String> = key
        .labels()
        .map(|l| format!("{}={}", l.key(), l.value()))
        .collect();
    if labels.is_empty() {
        key.name().to_string()
    } else {
        format!("{}{{{}}}", key.name(), labels.join(","))
    }
}

impl Recorder for Capture {
    fn describe_counter(&self, name: KeyName, _: Option<Unit>, _: SharedString) {
        self.1.lock().unwrap().push(name.as_str().to_string());
    }
    fn describe_gauge(&self, name: KeyName, _: Option<Unit>, _: SharedString) {
        self.1.lock().unwrap().push(name.as_str().to_string());
    }
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        Counter::from_arc(Arc::new(Series(series_name(key), self.0.clone())))
    }
    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::from_arc(Arc::new(Series(series_name(key), self.0.clone())))
    }
    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

impl Capture {
    fn publish(&self, node: &ReticulumNode) -> BTreeMap<String, f64> {
        metrics::with_local_recorder(self, || node.publish_metrics());
        self.0.lock().unwrap().clone()
    }
}

struct TestNode {
    node: ReticulumNode,
    rx: EventReceiver,
}

async fn start(builder: ReticulumNodeBuilder) -> TestNode {
    let storage = tempfile::tempdir().expect("tempdir");
    let mut node = builder
        .storage_path(storage.path().to_path_buf())
        .build()
        .await
        .expect("build node");
    std::mem::forget(storage);
    node.start().await.expect("start node");
    let rx = node.take_event_receiver().expect("event rx");
    TestNode { node, rx }
}

async fn saw(rx: &mut EventReceiver, mut pred: impl FnMut(&NodeEvent) -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(ev)) if pred(&ev) => return true,
            Ok(Some(_)) => {}
            _ => return false,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_link_telemetry_follows_a_link_through_its_life() {
    let port = next_port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut a = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_server(addr),
    )
    .await;
    let mut b = start(
        ReticulumNodeBuilder::new()
            .enable_transport(false)
            .add_tcp_client(addr),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let identity = Identity::generate(&mut rand_core::OsRng);
    let signing_key: [u8; 32] = identity.public_key_bytes()[32..64].try_into().unwrap();
    let mut dest = Destination::new(
        Some(identity),
        Direction::In,
        DestinationType::Single,
        "telemetry",
        &["e2e"],
    )
    .unwrap();
    dest.set_accepts_links(true);
    let hash = *dest.hash();
    a.node.register_destination(dest);
    a.node.announce_destination(&hash, None).await.unwrap();
    assert!(
        saw(&mut b.rx, |ev| matches!(
            ev,
            NodeEvent::AnnounceReceived { .. }
        ))
        .await,
        "B hears A"
    );

    let handle = b.node.connect(&hash, &signing_key).await.expect("connect");
    let link_id = *handle.link_id();
    b.node
        .await_link_established(&link_id)
        .await
        .expect("established");
    assert!(
        saw(&mut a.rx, |ev| matches!(
            ev,
            NodeEvent::LinkEstablished { .. }
        ))
        .await,
        "A sees the link"
    );

    // The list, from both ends. Every telemetry result is nameable from
    // leviculum_std alone.
    let _: LinkCensus = b.node.link_census();
    let _: LinkLifecycle = b.node.link_lifecycle();
    let _: NodeHeapCensus = b.node.heap_census();
    let _: LinkCountCheck = b.node.link_count_check();
    let list: Vec<LinkInfo> = b.node.link_list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].link_id, link_id, "the id the application was given");
    assert_eq!(list[0].role, LinkRole::Initiator);
    assert_eq!(list[0].destination_hash, hash);
    assert!(list[0].age_secs.is_some() && list[0].idle_secs.is_some());
    assert!(
        list[0].rtt_ms.is_some(),
        "an established link has a measured RTT"
    );
    assert_eq!(a.node.link_list()[0].role, LinkRole::Responder);

    // The mirror agrees with the core.
    let check = b.node.link_count_check();
    assert_eq!((check.mirror, check.core, check.alarms), (1, 1, 0));

    // The metrics.
    let capture = Capture::default();
    let m = capture.publish(&b.node);
    assert_eq!(m["leviculum.link.established{role=initiator}"], 1.0);
    assert_eq!(m["leviculum.link.live{role=initiator}"], 1.0);
    assert_eq!(m["leviculum.link.live{role=responder}"], 0.0);
    assert_eq!(m["leviculum.link.mirror"], 1.0);
    assert_eq!(m["leviculum.link.mirror_skew"], 0.0);
    assert_eq!(m["leviculum.link.max_per_destination"], 1.0);
    assert!(m["leviculum.transport.packets{direction=sent}"] > 0.0);
    assert!(m["leviculum.memory.bytes{component=links}"] > 0.0);
    assert!(m.contains_key("leviculum.transport.dropped{reason=no-path}"));
    let a_capture = Capture::default();
    let a_metrics = a_capture.publish(&a.node);
    assert_eq!(a_metrics["leviculum.link.established{role=responder}"], 1.0);
    // A recorder that arrives after earlier publishes still learns every
    // unit and description.
    assert_eq!(
        a_capture.1.lock().unwrap().len(),
        leviculum_std::telemetry::METRIC_CATALOG.len()
    );

    // Two nodes publishing into one recorder sum their counters, rather than
    // the series holding only the larger total.
    let both = Capture::default();
    both.publish(&a.node);
    let m = both.publish(&b.node);
    let sent = a.node.transport_stats().packets_sent() + b.node.transport_stats().packets_sent();
    assert!(
        m["leviculum.transport.packets{direction=sent}"] >= sent as f64,
        "summed: {} vs {sent}",
        m["leviculum.transport.packets{direction=sent}"]
    );
    assert_eq!(m["leviculum.link.established{role=initiator}"], 1.0);
    assert_eq!(m["leviculum.link.established{role=responder}"], 1.0);

    // Close it: the counters move, the gauges fall, the ends agree on why.
    b.node.close_link(&link_id).await.unwrap();
    assert!(
        saw(&mut a.rx, |ev| matches!(ev, NodeEvent::LinkClosed { .. })).await,
        "A sees the close"
    );
    assert_eq!(b.node.link_lifecycle().closed(LinkCloseReason::Normal), 1);
    assert_eq!(
        a.node.link_lifecycle().closed(LinkCloseReason::PeerClosed),
        1
    );
    let m = capture.publish(&b.node);
    assert_eq!(m["leviculum.link.closed{reason=normal}"], 1.0);
    assert_eq!(m["leviculum.link.live{role=initiator}"], 0.0);
    assert!(b.node.link_list().is_empty());
    let check = b.node.link_count_check();
    assert_eq!((check.mirror, check.core), (0, 0));
}
