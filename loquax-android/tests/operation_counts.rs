//! The phone's bar over the real fixture (1489, structure review
//! 2026-11-07 finding 14): a key typed in a chat builds no list the screen
//! does not show, counted with the core's operation counters through the
//! session, never timed. Red at the commit that wrote it (every request
//! ends with both lists built and encoded whole, `Session::request` ->
//! `show_scene`), judged through the fixture's register
//! (`loquax/tests/support/real_store.rs`).

#[path = "../../loquax/tests/support/real_store.rs"]
mod real_store;

use loquax_android::schema::{Request, Start};
use loquax_android::session::Session;
use real_store::bar;

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
