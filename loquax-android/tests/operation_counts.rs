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

/// The list rows a phone's screen shows.
const SCREEN_ROWS: u32 = 14;

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
/// rows than the screen shows and one (1595: the row it changed; the
/// others are the last list's), makes the lists once, and the pump's
/// turn after it makes them no second time.
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
    // The start through first (1600): the engine's `Ready` and the pull
    // it starts land on whichever pump turn comes next, and when that was
    // the probe's turn below, the turn made the lists for an engine event
    // of its own (`redraws(Engine(_))`), not for the key: red in 2 of 6
    // runs on 68761e32, 10 of 12 loaded runs here. Waited for, the start's
    // events are folded before the key, every time.
    wait_started(&session);
    // The pump held (1587): `update` starts the count afresh for every
    // event, and the request wakes the pump, whose turn folds the clock
    // (`Clock`, `Tick`, `Elapsed`) and builds nothing; read after such a
    // turn the bar saw the pump's last event, not the key, and went green
    // in three runs of four. Held, it reads the key's own work: its
    // `Draft` and the lists the request makes after it.
    let (counts, builds, echo) = session.with_pump_held(|session| {
        session.reset_counters();
        let before = session.scene_builds();
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
        let builds = session.scene_builds() - before;
        // The pump's turn after the key (1595): it makes the lists again
        // only for a change of its own, never for the key's, which the
        // request's lists show. A minute turning inside this turn moves
        // the lists' revision, and a build for that is no echo.
        let revision = session.lists_revision();
        let shown = session.scene_builds();
        session.pump_once();
        let echo = session.scene_builds() - shown;
        let moved = session.lists_revision() != revision;
        // A turn that folded a change of its own builds for that change;
        // the rule promises only that the request's own change is not
        // built twice. After the start none is left offline: this says so
        // if one comes.
        let own = session.last_turn_was_its_own();
        if own {
            println!("ECHO the probe's turn folded a change of its own");
        }
        (counts, builds, if moved || own { 0 } else { echo })
    });
    session.stop();
    println!("COUNTS phone.compose_key {counts} builds={builds} echo={echo}");
    bar(
        "phone.a_key_in_compose_builds_no_list",
        counts.rows_built <= SCREEN_ROWS + 1 && builds == 1 && echo == 0,
        &format!(
            "rows_built {} (bar {}), lists made {builds} (bar 1), by the pump's echo {echo} (bar 0)",
            counts.rows_built,
            SCREEN_ROWS + 1
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

/// Wait until the session's start is through (1600, `Session::started`):
/// the engine said it is ready and its first pull ended.
fn wait_started(session: &Session) {
    let begin = Instant::now();
    while !session.started() {
        assert!(
            begin.elapsed() < Duration::from_secs(60),
            "the session's start never finished"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait until `window` from now does not turn the wall clock's minute
/// (1600), and have the pump fold the minute it is in: the model's look
/// counts the minute (`look_fingerprint`, the rows' ago-words move by it),
/// so the lists are made again once a minute, as they should be. The
/// fixture's own idle keeps inside a minute for the same reason
/// (`real_store::idle`).
///
/// An idle pump sleeps seconds between turns (`idle_wait`), so a minute
/// that turned just before the window is folded by the pump's next turn,
/// inside it (1649: the pump's last turn at …:57, its next five seconds
/// later at …:02, the window from …:00.5; 1679: a window from …:00.10 got
/// the minute's lists at …:01.51, red in the land gate's forge stage). The
/// end of the window is kept out of the next minute by waiting; its start
/// by one pump turn here, held, which folds the wall clock as the pump's
/// thread does and makes the minute's lists before the bar counts.
fn inside_a_minute(session: &Session, window: Duration) {
    let second = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() % 60.0)
            .unwrap_or(0.0)
    };
    let room = 60.0 - window.as_secs_f64() - 1.0;
    let at = second();
    if at > room {
        std::thread::sleep(Duration::from_secs_f64(60.0 - at + 0.5));
    }
    session.with_pump_held(|session| session.pump_once());
    assert!(second() <= room, "the window would turn a minute");
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
    // A minute turning in the window made the lists once, by the rule
    // (1600): 5 reds in 24 loaded runs, every one with its window across
    // :00 and the pump's build at :00.09 with nothing dirty and the lists
    // revision moved; no green window crossed a minute.
    inside_a_minute(&session, Duration::from_millis(real_store::IDLE_MS + 500));
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

/// The open chat's rows as `Core.messages` asks for them.
fn rows(session: &Session, peer: &str) -> Vec<loquax_android::schema::MessageRow> {
    let answer = session
        .query(Query::Messages {
            peer: peer.to_string(),
        })
        .expect("the chat");
    let messages: loquax_android::schema::Messages =
        loquax_android::schema::decode(&answer).expect("decode");
    messages.messages
}

/// The phone's chat goes past its first page (1496, structure review
/// 2026-11-07 finding 10): the long chat of the fixture (1200 messages)
/// opens with its newest page, and a viewport that shows the first loaded
/// bubble, as `ChatActivity` says it when its list reaches the top, loads
/// the page above it, until the store has no more. Before, the phone
/// dropped `UiEffect::Store` in its catch-all: a restart's deferred chat
/// opened empty and no viewport ever loaded more. Negative: a viewport
/// below the first bubble loads nothing.
#[test]
fn the_phones_chat_loads_older_pages_as_its_viewport_reaches_the_top() {
    let dir = tempfile::tempdir().expect("app dir");
    let session = fixture_session(dir.path());
    let anna = loquax::address::to_hex(&real_store::peer(real_store::ANNA));
    session
        .request(Request::Open { peer: anna.clone() })
        .expect("open");
    let first = rows(&session, &anna);
    println!("COUNTS phone.earlier opened={}", first.len());
    assert!(!first.is_empty(), "the chat opened empty");
    assert!(
        first.len() < real_store::ANNA_MESSAGES,
        "the whole chat at once: {}",
        first.len()
    );
    // A viewport of the newest bubbles only: nothing more.
    let newest: Vec<i64> = first.iter().rev().take(5).map(|row| row.key).collect();
    session
        .request(Request::Viewport {
            peer: anna.clone(),
            keys: newest,
        })
        .expect("viewport");
    assert_eq!(
        rows(&session, &anna).len(),
        first.len(),
        "loaded below the top"
    );
    // Up to the top, page after page, as the list scrolls.
    let mut shown = first;
    let mut pages = 0;
    loop {
        let top: Vec<i64> = shown.iter().take(3).map(|row| row.key).collect();
        session
            .request(Request::Viewport {
                peer: anna.clone(),
                keys: top,
            })
            .expect("viewport");
        let now = rows(&session, &anna);
        if now.len() == shown.len() {
            break;
        }
        assert!(
            now.len() > shown.len(),
            "{} after {}",
            now.len(),
            shown.len()
        );
        // The bubbles shown before are still there, below the new page.
        assert_eq!(
            now[now.len() - shown.len()].key,
            shown[0].key,
            "the page went somewhere else than above"
        );
        shown = now;
        pages += 1;
        assert!(pages < 100, "never at the end");
    }
    session.stop();
    println!("COUNTS phone.earlier pages={pages} rows={}", shown.len());
    assert!(pages >= 1, "no older page loaded");
    let bodies = shown.iter().filter(|row| !row.body.is_empty()).count();
    assert!(
        shown.len() >= real_store::ANNA_MESSAGES * 9 / 10,
        "the top came at {} rows of {} messages ({bodies} with words)",
        shown.len(),
        real_store::ANNA_MESSAGES
    );
}

/// A list made again asks the file system nothing (1496, structure review
/// 2026-11-07 K4): a linked peer's thumbnail is looked for once until its
/// link changes. Before, every list made stat'ed every linked row, 300
/// per list for a book that links the fixture's conversations. The
/// control: a link that changes asks again, for that peer only.
#[test]
fn a_list_made_again_stats_no_thumbnail() {
    use loquax_android::schema::{Blob, ContactCopy};
    let dir = tempfile::tempdir().expect("app dir");
    let session = fixture_session(dir.path());
    let linked: Vec<String> = real_store::FRIENDS
        .map(|i| loquax::address::to_hex(&real_store::peer(i)))
        .collect();
    let link = |peer: &str, photo: &[u8]| {
        session
            .request(Request::Link {
                peer: peer.to_string(),
                contact: ContactCopy {
                    lookup: format!("lookup-{peer}"),
                    contact_id: 7,
                    name: format!("Kontakt {}", &peer[..4]),
                    photo: Some(Blob(photo.to_vec())),
                },
            })
            .expect("link");
    };
    for peer in &linked {
        link(peer, &[0xff, 0xd8, 0xff]);
    }
    let after_links = session.photo_stats();
    let builds = session.scene_builds();
    let anna = loquax::address::to_hex(&real_store::peer(real_store::ANNA));
    for on in [true, false, true, false] {
        session
            .request(Request::Pin {
                peer: anna.clone(),
                on,
            })
            .expect("pin");
    }
    let made = session.scene_builds() - builds;
    let again = session.photo_stats() - after_links;
    link(&linked[0], &[0xff, 0xd8, 0xfe]);
    session
        .request(Request::Pin {
            peer: anna.clone(),
            on: true,
        })
        .expect("pin");
    let changed = session.photo_stats() - after_links - again;
    session.stop();
    println!(
        "COUNTS phone.photo_stats linked={} first={after_links} lists={made} again={again} changed={changed}",
        linked.len()
    );
    assert!(made >= 4, "the control made no lists: {made}");
    assert!(after_links >= 1, "no thumbnail was ever looked for");
    assert_eq!(again, 0, "{made} lists stat'ed {again} thumbnails");
    assert_eq!(changed, 1, "a changed link is looked for again, once");
}

/// The pump turns on an OS thread of its own (1496, structure review
/// 2026-11-07 B4), never on a worker of the runtime the node and the
/// runners need: each turn's commits block only it.
#[test]
fn the_pump_turns_on_its_own_thread() {
    let dir = tempfile::tempdir().expect("app dir");
    let session = fixture_session(dir.path());
    let begin = Instant::now();
    let mut seen = session.pump_thread();
    while seen.is_none() && begin.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(20));
        seen = session.pump_thread();
    }
    session.stop();
    println!("COUNTS phone.pump_thread {seen:?}");
    assert_eq!(
        seen.as_deref(),
        Some(loquax_android::session::PUMP_THREAD),
        "the pump turned elsewhere"
    );
}
