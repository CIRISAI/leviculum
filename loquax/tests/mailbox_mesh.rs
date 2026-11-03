//! A mailbox on a computer without lnsd (1354): `loquax mailbox run`
//! decides by `loquax::home::upstream` and, with no daemon answering on its
//! shared instance, runs on a node of its own that dials the entry servers,
//! as the phone's mailbox does (`DaemonUpstream::Mesh`). Here against two
//! entry nodes, the second taking over when the first is gone, across a
//! restart that decides the same way again. The phone's own decision is
//! loquax-android's and untouched: its `home_reach` runs the same harness.

use std::time::Duration;

use loquax::home::upstream::{self, Way};

#[path = "support/reach.rs"]
mod reach;

use reach::{active, entry, four_steps, four_steps_again, logs, wait_until, Mailbox, Phone, Relay};

/// The rule as `mailbox run` asks it, against an instance name no daemon
/// on this host listens on.
fn decided(home: &std::path::Path) -> (Way, &'static str) {
    let config = loquax::config::load(home).expect("the config");
    let instance = format!("mailbox-mesh-test-{}", std::process::id());
    upstream::decide(&config, || {
        leviculum_std::interfaces::SharedInstanceTransport::default().answers(&instance)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn without_lnsd_the_mailbox_runs_mesh_on_two_entries_and_a_restart_keeps_it() {
    logs();
    let one = Relay::start(&[]).await;
    let two = Relay::start(&[]).await;
    let dir = tempfile::tempdir().expect("mailbox dir");
    // Only the entry list: no `mailbox_upstream`, so no daemon decides it.
    std::fs::write(
        dir.path().join(loquax::config::CONFIG_FILE),
        format!(
            "mailbox_servers = {}, {}\n",
            entry(one.peer),
            entry(two.peer)
        ),
    )
    .expect("the config");
    let servers = vec![entry(one.peer), entry(two.peer)];
    let mesh = Way::Mesh {
        servers: servers.clone(),
        from: "config",
    };
    assert_eq!(decided(dir.path()), (mesh.clone(), "no-lnsd"));

    let mailbox = Mailbox::start_in(dir, servers.clone(), true).await;
    let home = mailbox.home();
    let phone = Phone::start(vec![entry(one.ap), entry(two.ap)], &mailbox, true).await;
    let node = phone.attached.node();
    wait_until("the mailbox on entry one", 15, || one.node.has_path(&home)).await;
    wait_until("the phone's path through entry one", 10, || {
        active(&phone.attached) == Some(entry(one.ap)) && node.has_path(&home)
    })
    .await;
    four_steps(&phone, &mailbox, Duration::from_secs(30)).await;

    // A restart under the same config, with entry one gone meanwhile: the
    // same way again, and the node comes up on entry two.
    let dir = mailbox.stop_keeping().await;
    assert_eq!(decided(dir.path()), (mesh, "no-lnsd"));
    one.stop().await;
    let mailbox = Mailbox::start_in(dir, servers, false).await;
    wait_until("the phone on entry two", 15, || {
        active(&phone.attached) == Some(entry(two.ap))
    })
    .await;
    wait_until("the restarted mailbox on entry two", 20, || {
        two.node.has_path(&home) && node.has_path(&home)
    })
    .await;
    std::fs::remove_file(phone.dir.path().join(loquax::home::client::CURSOR_FILE)).ok();
    four_steps_again(&phone, &mailbox).await;

    drop(node);
    phone.attached.stop().await;
    mailbox.stop().await;
    two.stop().await;
}
