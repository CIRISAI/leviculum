//! The page node must survive being started before the shared-instance
//! daemon is listening (Codeberg #311).
//!
//! On a fresh install and on every boot, `lblogd.service` and the daemon
//! start in the same transaction and the daemon's IPC socket is not bound
//! yet when lblogd dials it: `lnsd` is `Type=simple`, so systemd calls it
//! started the moment it is exec'd, seconds before it listens, and on a
//! package install the two `systemctl start` calls are not ordered against
//! each other at all. The reported gap was three seconds.
//!
//! Both tests run a real daemon over a real abstract Unix socket; nothing
//! here is mocked, because the condition under test is exactly "is that
//! socket accepting yet".

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use leviculum_std::driver::ReticulumNodeBuilder;

use lblogd::content::{Reloader, Sources};
use lblogd::node::{BlogNode, BlogNodeConfig};
use lblogd::render::BlogMeta;

fn fixture_meta() -> BlogMeta {
    BlogMeta {
        title: "wait test blog".to_string(),
        language: "en".to_string(),
        ..BlogMeta::default()
    }
}

/// A daemon built but not started: its shared-instance socket is not bound
/// until `start()` is awaited, which is the window the bug lives in.
fn build_daemon(instance_name: &str, storage: &std::path::Path) -> leviculum_std::ReticulumNode {
    // `:0`: the kernel picks the port, nothing dials this server; the test
    // is wired entirely over the shared instance.
    let daemon_tcp: SocketAddr = "127.0.0.1:0".parse().unwrap();
    ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance_name.to_string())
        .add_tcp_server(daemon_tcp)
        .storage_path(storage.to_path_buf())
        .build_sync()
        .expect("build daemon")
}

fn blog_config(instance_name: &str, data_dir: &std::path::Path) -> BlogNodeConfig {
    BlogNodeConfig {
        instance_name: instance_name.to_string(),
        data_dir: data_dir.to_path_buf(),
        display_name: "wait test blog".to_string(),
        announce_interval: Duration::from_secs(3600),
    }
}

fn empty_content() -> lblogd::content::SnapshotRx {
    let posts_dir = Box::leak(Box::new(tempfile::tempdir().expect("posts dir")));
    let (reloader, content) =
        Reloader::new(fixture_meta(), Sources::new(posts_dir.path())).expect("initial content");
    // The reloader owns the watch channel's sender; keep it alive for the
    // test's duration so the receiver does not see it drop.
    Box::leak(Box::new(reloader));
    content
}

/// The bug: a daemon that is merely a couple of seconds late must not cost
/// the node its start.
#[tokio::test]
async fn start_waiting_survives_a_daemon_that_is_not_up_yet() {
    let instance_name = format!("lblogd-311-late-{}", std::process::id());
    let daemon_storage = tempfile::tempdir().expect("daemon storage");
    let mut daemon = build_daemon(&instance_name, daemon_storage.path());

    // The daemon comes up two seconds after the blog node dials it — a
    // little under the three seconds the issue reported from a PineNote.
    let daemon_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        daemon.start().await.expect("start daemon");
        // Hold the daemon (and its socket) for the rest of the test.
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let data_dir = tempfile::tempdir().expect("data dir");
    let started = Instant::now();
    let blog = BlogNode::start_waiting(
        blog_config(&instance_name, data_dir.path()),
        empty_content(),
        Duration::from_secs(20),
    )
    .await
    .expect("the node must wait for the daemon instead of exiting");
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "the node cannot have connected before the daemon was listening"
    );
    assert!(
        !blog.destination_hash().as_bytes().is_empty(),
        "a node that started has a destination"
    );

    daemon_task.abort();
}

/// The wait is bounded, so an `instance_name` no daemon will ever serve
/// still fails the start instead of leaving the unit `active` and mute.
#[tokio::test]
async fn start_waiting_still_fails_when_no_daemon_ever_appears() {
    let instance_name = format!("lblogd-311-absent-{}", std::process::id());
    let data_dir = tempfile::tempdir().expect("data dir");

    let started = Instant::now();
    let result = BlogNode::start_waiting(
        blog_config(&instance_name, data_dir.path()),
        empty_content(),
        Duration::from_millis(600),
    )
    .await;

    let err = result.err().expect("no daemon means no node");
    assert!(
        err.to_string().contains("is lnsd or rnsd running?"),
        "the failure must still name the absent daemon, got: {err}"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "it must have actually waited before giving up"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the wait is bounded, not endless (took {:?})",
        started.elapsed()
    );
}

/// The plain `start` keeps its fail-fast contract: `lncp`, `lnstatus` and
/// `--print-hash` want an absent daemon reported at once, not waited on.
#[tokio::test]
async fn plain_start_still_fails_fast() {
    let instance_name = format!("lblogd-311-fast-{}", std::process::id());
    let data_dir = tempfile::tempdir().expect("data dir");

    let started = Instant::now();
    let result = BlogNode::start(
        blog_config(&instance_name, data_dir.path()),
        empty_content(),
    )
    .await;

    assert!(result.is_err(), "no daemon means no node");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "start must not have waited (took {:?})",
        started.elapsed()
    );
}
