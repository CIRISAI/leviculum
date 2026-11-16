//! The Rooms on the phone end to end (1384, #232): the phone's session
//! with its mailbox on the phone, which dials a Rust transport node, and
//! the fake hub of 1379 (`loquax/tests/support/rrc_hub.rs`) behind it.
//! The tab links to the hub over the mailbox's node, lent to the Rooms,
//! identified with the mailbox's identity; a room is joined from the
//! input field, our line reaches the hub and comes back on the phone's
//! screen as "you", in Java's bytes, and a line over the hub's limit
//! stays in the field with nothing sent.
//!
//! ```text
//! session ── mailbox node (lent) ── TCP ── transport ── TCP ── fake hub
//! ```

use std::time::{Duration, Instant};

use leviculum_rrc::Kind;
use leviculum_std::driver::ReticulumNodeBuilder;
use loquax_android::schema::{self, Query, Request, RoomsScreen, Start};
use loquax_android::session::Session;

#[path = "../../loquax/tests/support/rrc_hub.rs"]
mod rrc_hub;
use rrc_hub::Hub;

fn rooms(session: &Session, verb: &str, text: &str, index: i64) {
    session
        .request(Request::Rooms {
            verb: verb.to_string(),
            text: text.to_string(),
            index,
        })
        .unwrap_or_else(|error| panic!("{verb}: {error}"));
}

fn screen(session: &Session) -> RoomsScreen {
    schema::decode(&session.query(Query::Rooms).expect("the screen")).expect("decoded")
}

/// Ask the screen until `want` holds, up to `secs`; the last screen.
fn until(
    session: &Session,
    secs: u64,
    what: &str,
    want: impl Fn(&RoomsScreen) -> bool,
) -> RoomsScreen {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let shown = screen(session);
        if want(&shown) {
            return shown;
        }
        assert!(
            Instant::now() < deadline,
            "no {what} in {secs} s; last: {shown:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn hex(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_phone_joins_a_room_and_its_line_comes_back_as_you() {
    let rns = tempfile::tempdir().expect("transport storage");
    let mut transport = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .add_tcp_server("127.0.0.1:0".parse().expect("an address"))
        .storage_path(rns.path().to_path_buf())
        .build_sync()
        .expect("the transport node");
    transport.start().await.expect("start it");
    let entry = *transport
        .tcp_listen_addrs()
        .first()
        .expect("its TCP server");
    let hub = Hub::start(entry).await;

    // The phone: its mailbox here, dialling the transport node, and the
    // hub named in its config, which is what turns the tab on.
    let phone = tempfile::tempdir().expect("app dir");
    let mailbox = phone.path().join(loquax_android::mailbox::DIR);
    std::fs::create_dir_all(&mailbox).expect("mailbox dir");
    std::fs::write(
        mailbox.join(loquax_android::mailbox::SERVERS_FILE),
        format!("{entry}\n"),
    )
    .expect("servers file");
    std::fs::write(
        phone.path().join("config"),
        format!("rooms_hub = {}\n", hex(&hub.dest)),
    )
    .expect("config");
    let start = Start {
        dir: phone.path().display().to_string(),
        here: true,
        lang: Some("en".to_string()),
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        Session::start(&start).unwrap_or_else(|error| panic!("{error:?}"))
    })
    .await
    .expect("the session");
    assert!(session.is_here());
    let me = *loquax_android::mailbox::identity(phone.path())
        .expect("the mailbox's identity")
        .hash();
    let session = std::sync::Arc::new(session);

    let s = std::sync::Arc::clone(&session);
    let (welcomed, joined, echoed, long, parted) = tokio::task::spawn_blocking(move || {
        let closed = screen(&s);
        assert!(closed.enabled && !closed.open, "the tab is there, closed");
        // Opening the rooms of the hub the config names links (1652) and
        // asks for its rooms; nothing is typed: a tap on one joins it.
        rooms(&s, "tab", "", 0);
        let welcomed = until(&s, 60, "the hub's rooms", |shown| {
            shown.open && shown.status.is_empty() && !shown.listed.is_empty()
        });
        assert!(welcomed.open, "{welcomed:?}");
        rooms(&s, "join", "lab", 0);
        let joined = until(&s, 30, "the room", |shown| shown.room == "lab");
        rooms(&s, "say", "rrc line from the phone", 0);
        let echoed = until(&s, 30, "the echo", |shown| {
            shown
                .lines
                .iter()
                .any(|line| line.kind == "msg" && line.text == "rrc line from the phone")
        });
        // Past the hub's 350 bytes: refused before it goes, kept in the
        // field.
        let too_long = "y".repeat(351);
        rooms(&s, "say", &too_long, 0);
        let long = screen(&s);
        assert_eq!(long.input, too_long, "the line stays in the field");
        rooms(&s, "say", "/part", 0);
        let parted = until(&s, 30, "the part", |shown| shown.rows.is_empty());
        rooms(&s, "chats", "", 0);
        assert!(!screen(&s).open, "the tab gave way");
        (welcomed, joined, echoed, long, parted)
    })
    .await
    .expect("the walk");

    println!("ROOMS_SCREEN joined {joined:#?}");
    println!("ROOMS_SCREEN echoed {echoed:#?}");
    // The hub's list, as it spells it, under its title, the field below
    // as the way to another room (1652).
    assert_eq!(welcomed.listed, ["general chat", "lab"], "{welcomed:?}");
    assert_eq!(welcomed.listed_title, "Rooms on periculum");
    assert_eq!(welcomed.other, "Join another room");
    assert!(welcomed.empty.is_empty(), "the field moved under the list");
    assert!(welcomed.retry.is_empty());
    assert_eq!(joined.rows.len(), 1);
    assert!(joined.rows[0].selected);
    assert!(joined.live.starts_with("A room is live"), "{}", joined.live);
    assert!(
        joined.lines.iter().any(|line| line.kind == "joined"),
        "{:?}",
        joined.lines
    );
    let ours = echoed
        .lines
        .iter()
        .find(|line| line.text == "rrc line from the phone")
        .expect("our line");
    assert_eq!((ours.who.as_str(), ours.ours), ("you", true));
    assert!(echoed.input.is_empty(), "a line that went leaves the field");
    assert!(long.lines.len() == echoed.lines.len());
    // After the part nothing is joined, and the hub names the room again.
    assert!(parted.room.is_empty() && parted.rows.is_empty());
    assert!(welcomed.status.is_empty(), "the link was up: {welcomed:?}");

    {
        let seen = hub.seen.lock().expect("seen");
        assert_eq!(
            seen.identified,
            [me],
            "the link was identified with the mailbox's identity"
        );
        // The PONG to the hub's PING goes whenever the link task reads
        // the PING, which may be after the JOIN the screen asked: the
        // order of the reader's own is what counts. The long line never
        // reached the hub.
        let kinds: Vec<Kind> = seen
            .envelopes
            .iter()
            .map(|e| e.kind)
            .filter(|kind| *kind != Kind::Pong)
            .collect();
        // `/list` after the WELCOME (1652), then the tap's JOIN.
        assert_eq!(
            kinds,
            [Kind::Hello, Kind::Msg, Kind::Join, Kind::Msg, Kind::Part],
            "{kinds:?}"
        );
        assert_eq!(seen.envelopes[1].text(), Some("/list"));
    }

    let s = std::sync::Arc::try_unwrap(session).unwrap_or_else(|_| panic!("one session"));
    tokio::task::spawn_blocking(move || s.stop())
        .await
        .expect("stopped");
    hub.task.abort();
    transport.stop().await.expect("stop the transport node");
}
