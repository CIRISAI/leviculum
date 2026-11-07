//! The phone's bar over the real fixture (1489, structure review
//! 2026-11-07 finding 14): a key typed in a chat builds no list the screen
//! does not show, counted with the core's operation counters through the
//! session, never timed. Red at the commit that wrote it (every request
//! ends with both lists built and encoded whole, `Session::request` ->
//! `show_scene`), judged through the fixture's register
//! (`loquax/tests/support/real_store.rs`).

#[path = "../../loquax/tests/support/real_store.rs"]
mod real_store;

use loquax_android::schema::{Query, Request, Start};
use loquax_android::session::Session;
use real_store::bar;
use std::time::{Duration, Instant};

/// The list rows a phone's screen shows, and as many again.
const LIST_ON_SCREEN: u32 = 14;

/// A session on the phone with its mailbox here, offline, over `dir`.
fn start(dir: &std::path::Path) -> Session {
    let start = Start {
        dir: dir.display().to_string(),
        here: true,
        lang: Some("en".to_string()),
        ..Start::default()
    };
    Session::start(&start).unwrap_or_else(|error| panic!("{error:?}"))
}

/// A key typed in the long chat's compose field, as Java sends it
/// (`ChatActivity`: `Core.draft` on every change), builds no more list
/// rows than the screen shows and as many again; the list is not even on
/// screen while the chat is.
#[test]
fn a_key_in_compose_builds_no_list() {
    if !real_store::counting() {
        return;
    }
    let dir = tempfile::tempdir().expect("app dir");
    // No dialling out: the session runs offline.
    std::fs::write(dir.path().join("config"), "rendezvous_dial = off\n").expect("config");
    // A first start makes the mailbox and its address; the fixture goes
    // into the client store under that address before the second.
    let first = start(dir.path());
    let me = first.address();
    first.stop();
    drop(real_store::fill_store_for(dir.path(), me));
    let session = start(dir.path());
    let anna = loquax::address::to_hex(&real_store::peer(real_store::ANNA));
    session
        .request(Request::Open { peer: anna.clone() })
        .expect("open");
    session.reset_counters();
    session
        .request(Request::Draft {
            peer: anna,
            body: "B".to_string(),
            reply: String::new(),
        })
        .expect("a key");
    let counts = session
        .counters()
        .expect("counting() said this build counts");
    session.stop();
    println!("COUNTS phone.compose_key {counts}");
    bar(
        "phone.a_key_in_compose_builds_no_list",
        counts.rows_built <= 2 * LIST_ON_SCREEN,
        &format!(
            "rows_built {} (bar {})",
            counts.rows_built,
            2 * LIST_ON_SCREEN
        ),
    );
}

/// The fixture's session, offline, the long chat open: the first start
/// makes the mailbox and its address, the fixture goes into the client
/// store under it before the second.
fn fixture_session(dir: &std::path::Path) -> Session {
    std::fs::write(dir.join("config"), "rendezvous_dial = off\n").expect("config");
    let first = start(dir);
    let me = first.address();
    first.stop();
    drop(real_store::fill_store_for(dir, me));
    start(dir)
}

/// Wait until the session made no list for `quiet`, at most `most`:
/// what its start owed (the first pull's word, a toast aging out) is
/// done. Whether it settled.
fn settle(session: &Session, quiet: Duration, most: Duration) -> bool {
    let begin = Instant::now();
    let mut last = (session.scene_builds(), Instant::now());
    while begin.elapsed() < most {
        std::thread::sleep(Duration::from_millis(50));
        let builds = session.scene_builds();
        if builds != last.0 {
            last = (builds, Instant::now());
        } else if last.1.elapsed() >= quiet {
            return true;
        }
    }
    false
}

/// Five seconds of a phone left alone make no list (1491), however
/// often the UI thread asks for one: the pump folds the clock every turn
/// and makes the lists only when the model's revision moved, and
/// `listed_now` never makes them (before 1491 every pump turn and every
/// lock made them stale, and the UI thread's next ask computed both on
/// its own thread). The control: a request that changes the list makes
/// them once.
#[test]
fn an_idle_phone_builds_no_list() {
    let dir = tempfile::tempdir().expect("app dir");
    let session = fixture_session(dir.path());
    let settled = settle(&session, Duration::from_secs(2), Duration::from_secs(30));
    let before = session.scene_builds();
    let begin = Instant::now();
    let mut asked = 0;
    while begin.elapsed() < Duration::from_millis(real_store::IDLE_MS) {
        let listed = session
            .query(Query::Conversations { search: None })
            .expect("the list");
        assert!(!listed.is_empty(), "no list kept");
        asked += 1;
        std::thread::sleep(Duration::from_millis(50));
    }
    let idle = session.scene_builds() - before;
    let anna = loquax::address::to_hex(&real_store::peer(real_store::ANNA));
    session.request(Request::Open { peer: anna }).expect("open");
    let control = session.scene_builds() - before - idle;
    session.stop();
    println!(
        "COUNTS phone.idle settled={settled} asked={asked} builds={idle} control_builds={control}"
    );
    assert!(settled, "the session never settled");
    assert!(control >= 1, "the control made no list: {control}");
    bar(
        "phone.an_idle_phone_builds_no_list",
        idle == 0,
        &format!(
            "lists made {idle} over {} ms, {asked} asks (bar 0)",
            real_store::IDLE_MS
        ),
    );
}
