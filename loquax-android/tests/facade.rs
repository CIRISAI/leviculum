//! The facade the app's Java calls, on the host and without JNI: the bytes
//! in and out of `start`, `command`, `query` and `nextEvent`, and a session
//! against a real home node (`loquax::home::daemon`, in process) on a Rust
//! transport node with a TCP server, the leaf dialling in as the phone does.

use std::path::Path;
use std::time::Duration;

use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use loquax_android::schema::{self, Event, Request, Start, Started};
use loquax_android::session::Session;

#[path = "../../loquax/tests/support/python_peer.rs"]
mod python_peer;

#[path = "../../loquax/tests/support/nomadnet_tree.rs"]
mod nomadnet_tree;

#[path = "../../loquax/tests/support/ugly_store.rs"]
mod ugly_store;

/// The exported facade keeps one session per process, and the tests of a
/// binary share the process: those that go through it take turns. Tokio's
/// lock, which the async test holds across its awaits.
static FACADE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn started(bytes: &[u8]) -> Started {
    rmp_serde::from_slice(bytes).expect("a Started")
}

fn start_bytes(start: &Start) -> Vec<u8> {
    schema::encode(start)
}

/// The ways a first start goes wrong, each with what the join screen asks
/// for and words for it. Through the exported facade, which keeps no
/// session after a failed start.
#[test]
fn a_first_start_says_what_it_needs() {
    let _facade = FACADE.blocking_lock();
    let dir = tempfile::tempdir().expect("app dir");
    let base = Start {
        dir: dir.path().display().to_string(),
        ..Start::default()
    };

    let answer = started(&loquax_android::start(&start_bytes(&base)));
    assert!(!answer.ok);
    assert_eq!(answer.need.as_deref(), Some("code"));
    assert!(answer.error.unwrap().contains("reads no mailbox"));

    let answer = started(&loquax_android::start(&start_bytes(&Start {
        code: Some("lqxh1nonsense".to_string()),
        ..base.clone()
    })));
    assert_eq!(answer.need.as_deref(), Some("code"));
    assert!(answer
        .error
        .unwrap()
        .starts_with("That is not a mailbox code"));

    let code = code_for(&leviculum_core::Identity::generate(&mut rand_core::OsRng));
    let answer = started(&loquax_android::start(&start_bytes(&Start {
        code: Some(code.clone()),
        ..base.clone()
    })));
    assert_eq!(answer.need.as_deref(), Some("server"));
    assert!(answer.error.unwrap().contains("host:port"));

    let answer = started(&loquax_android::start(&start_bytes(&Start {
        code: Some(code),
        server: Some("homenode.lan".to_string()),
        ..base
    })));
    assert_eq!(answer.need.as_deref(), Some("server"));
    assert!(answer.error.unwrap().contains("has no port"));

    let answer = started(&loquax_android::start(b"\xc1"));
    assert!(!answer.ok);
    assert!(answer.error.unwrap().contains("did not decode"));

    // Nothing runs, and the calls say so instead of failing.
    let done: schema::Done =
        rmp_serde::from_slice(&loquax_android::command(&schema::encode(&Request::Pull)))
            .expect("a Done");
    assert_eq!(done.error.as_deref(), Some("not started"));
    let done: schema::Done = rmp_serde::from_slice(&loquax_android::query(&schema::encode(
        &schema::Query::Conversations,
    )))
    .expect("a Done");
    assert_eq!(done.error.as_deref(), Some("not started"));
    let event: Event =
        rmp_serde::from_slice(&loquax_android::next_event(Duration::ZERO).expect("an event"))
            .expect("an Event");
    assert!(matches!(event, Event::Stopped { .. }), "{event:?}");
    // No join file, no server file: nothing was written for a failed start.
    assert!(!dir.path().join(loquax::home::client::CLIENT_FILE).exists());
    assert!(!dir
        .path()
        .join(loquax_android::session::SERVER_FILE)
        .exists());
}

/// An admission code for a fresh token of `identity`'s home, not issued
/// anywhere: enough for the checks before any network.
fn code_for(identity: &leviculum_core::Identity) -> String {
    loquax::home::code::HomeCode {
        public_key: identity.public_key_bytes(),
        token: [7; 16],
    }
    .encode()
}

/// A token issued for "phone" in the store of the home node of `home`, and
/// the code that carries it with the home identity's public key.
fn admit(home: &Path) -> String {
    let identity = loquax::identity::load_or_create(&home.join("identity")).expect("identity");
    let address = loquax::engine::delivery_address(&identity).expect("address");
    let mut store = loquax::store::Store::open_in(home, address).expect("the home store");
    let token = [0x42; 16];
    store
        .issue_token(&token, "phone", leviculum_std::local_time::unix_now_secs())
        .expect("a token");
    loquax::home::code::HomeCode {
        public_key: identity.public_key_bytes(),
        token,
    }
    .encode()
}

/// Fill the home node's archive in `home` with the ugly store (1173): a
/// Ratspeak and a NomadNet import, 800 messages over 20 conversations,
/// names, petnames, a block, an archived conversation. Every test that
/// opens a session against a home node does so on it; the phone's pull on
/// open brings all of it.
fn ugly(home: &Path) {
    let filled = ugly_store::fill_archive(home);
    assert_eq!(
        filled.messages,
        ugly_store::CONVERSATIONS * ugly_store::PER_CONVERSATION
    );
}

/// Wait until `session` shows the ugly store whole: every conversation
/// listed under its name where it has one, the archived one in the
/// archive, the blocked one among the blocked, and each conversation's
/// rows decoded.
fn ugly_in(session: &Session) {
    use ugly_store::{ARCHIVED, BLOCKED, CONVERSATIONS, PER_CONVERSATION, REACTIONS};
    let shown = |c: &schema::Conversations| {
        (0..CONVERSATIONS)
            .filter(|i| ![ARCHIVED, BLOCKED].contains(i))
            .all(|i| {
                c.conversations.iter().any(|row| {
                    row.peer == ugly_store::peer_hex(i)
                        && ugly_store::shown_name(i).is_none_or(|name| row.name.starts_with(&name))
                })
            })
    };
    wait_for(session, schema::Query::Conversations, 60, shown);
    wait_for(
        session,
        schema::Query::Archived,
        20,
        |c: &schema::Conversations| {
            c.conversations
                .iter()
                .any(|row| row.peer == ugly_store::peer_hex(ARCHIVED))
        },
    );
    wait_for(
        session,
        schema::Query::Blocked,
        20,
        |c: &schema::Conversations| {
            c.conversations
                .iter()
                .any(|row| row.peer == ugly_store::peer_hex(BLOCKED))
        },
    );
    for i in (0..CONVERSATIONS).filter(|i| *i != BLOCKED) {
        let messages: schema::Messages = wait_for(
            session,
            schema::Query::Messages {
                peer: ugly_store::peer_hex(i),
            },
            20,
            |m: &schema::Messages| m.messages.len() == PER_CONVERSATION - REACTIONS,
        );
        assert!(
            messages
                .messages
                .iter()
                .any(|m| m.body == ugly_store::long_body()),
            "conversation {i}: the long body"
        );
    }
}

/// Wait for a query's answer to satisfy `ok`, reading events meanwhile.
fn wait_for<T: serde::de::DeserializeOwned + std::fmt::Debug>(
    session: &Session,
    query: schema::Query,
    secs: u64,
    ok: impl Fn(&T) -> bool,
) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let answer: T =
            rmp_serde::from_slice(&session.query(query.clone()).expect("a query")).expect("decode");
        if ok(&answer) {
            return answer;
        }
        assert!(std::time::Instant::now() < deadline, "never: {answer:?}");
        let _ = session.next_event(Duration::from_millis(200));
    }
}

/// The phone end to end: the first start joins with the code over TCP and
/// keeps the client and the server; a send goes through the home node,
/// which names the bubble; a pull runs; the conversation list shows it. The
/// second start needs neither code nor server and shows the conversation
/// from the store. A second code on a joined phone is refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_phone_joins_sends_pulls_and_comes_back() {
    let instance = instance();
    // The transport's Reticulum config directory, its storage under
    // `storage/`, as lnsd keeps it: the home node takes its RPC key there.
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    ugly(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let home_dir = home.path().to_path_buf();

    let phone = tempfile::tempdir().expect("app dir");
    let dir = phone.path().display().to_string();
    let first = Start {
        dir: dir.clone(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code.clone()),
        name: Some("Pixel".to_string()),
        offset: 3600,
        ..Start::default()
    };
    let peer = loquax::address::to_hex(&[0x5a; 16]);
    let (address, peer) = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        ugly_in(&session);
        let address = loquax::address::to_hex(&session.address());
        session
            .request(Request::Send {
                peer: peer.clone(),
                body: "hallo vom Telefon".to_string(),
            })
            .expect("send");
        // The home node took it: the bubble carries its id.
        let messages: schema::Messages = wait_for(
            &session,
            schema::Query::Messages { peer: peer.clone() },
            60,
            |m: &schema::Messages| m.messages.last().is_some_and(|b| !b.id.is_empty()),
        );
        let bubble = messages.messages.last().expect("the bubble");
        assert!(bubble.ours);
        assert_eq!(bubble.body, "hallo vom Telefon");
        assert_ne!(bubble.state, "failed", "{bubble:?}");
        // A reply to it (1137): sent with the target's id, which the row
        // reads back out of its own fields, and the target's text quoted.
        let target = bubble.id.clone();
        session
            .request(Request::Reply {
                peer: peer.clone(),
                target: target.clone(),
                body: "und die Antwort".to_string(),
            })
            .expect("reply");
        let messages: schema::Messages = wait_for(
            &session,
            schema::Query::Messages { peer: peer.clone() },
            60,
            |m: &schema::Messages| m.messages.len() == 2 && !m.messages[1].id.is_empty(),
        );
        let reply = &messages.messages[1];
        assert_eq!(reply.body, "und die Antwort");
        assert_eq!(reply.quote_to.as_deref(), Some(target.as_str()));
        assert_eq!(reply.quote.as_deref(), Some("hallo vom Telefon"));
        assert_eq!(reply.quote_ours, Some(true));
        assert!(messages.home_client);
        let info: schema::Info = rmp_serde::from_slice(
            &session
                .query(schema::Query::Info {
                    peer: peer.clone(),
                    key: reply.key,
                })
                .expect("info"),
        )
        .expect("decode");
        assert!(info.rows.iter().any(|r| r.contains(&reply.id)), "{info:?}");
        // The reaction sent and the emoji picked are the recents, newest
        // first, kept in the app's directory.
        session
            .request(Request::React {
                peer: peer.clone(),
                target: target.clone(),
                content: "\u{1F44D}".to_string(),
            })
            .expect("react");
        session
            .request(Request::Used {
                emoji: "\u{1F600}".to_string(),
            })
            .expect("used");
        let recents: schema::Recents =
            rmp_serde::from_slice(&session.query(schema::Query::Recents).expect("recents"))
                .expect("decode");
        assert_eq!(recents.emoji, ["\u{1F600}", "\u{1F44D}"]);
        let messages: schema::Messages = wait_for(
            &session,
            schema::Query::Messages { peer: peer.clone() },
            60,
            |m: &schema::Messages| m.messages[0].chips.len() == 1,
        );
        let chip = &messages.messages[0].chips[0];
        assert_eq!((chip.content.as_str(), chip.ours), ("\u{1F44D}", true));
        // A peer without a name: the avatar shows its address's first byte,
        // in the app's colour since it sent no icon appearance (1129).
        assert_eq!(messages.initials, peer[..2].to_uppercase());
        assert_eq!(
            (messages.icon_fg.as_deref(), messages.icon_bg.as_deref()),
            (None, None)
        );

        session.request(Request::Pull).expect("pull");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            match session.next_event(Duration::from_millis(500)) {
                Some(Event::Sync { state, .. }) if state == "done" => break,
                Some(Event::Sync { state, detail, .. }) if state == "failed" => {
                    panic!("the pull failed: {detail}")
                }
                _ => assert!(std::time::Instant::now() < deadline, "no pull ended"),
            }
        }
        let list: schema::Conversations = wait_for(
            &session,
            schema::Query::Conversations,
            5,
            |c: &schema::Conversations| c.conversations.iter().any(|row| row.peer == peer),
        );
        // The newest conversation, below the ugly store's pinned one.
        assert_eq!(
            list.conversations[0].peer,
            ugly_store::peer_hex(ugly_store::PINNED)
        );
        let row = &list.conversations[1];
        assert_eq!(row.peer, peer);
        assert_eq!(row.last, "you reacted \u{1F44D}");
        assert!(row.ours);
        assert_eq!(row.initials, peer[..2].to_uppercase());
        // The row's mark (1130): ours is last, so a delivery word, a text.
        assert_eq!(row.kind, "reaction");
        assert!(!row.state.is_empty());
        assert_eq!(list.initials.chars().count(), 2);
        assert_eq!(list.address.len(), 32);

        // The address book (1139): linked with what the picker gave, the
        // contact's name shows and its photo, under the user's own name,
        // which the home node keeps for the home's other devices.
        let lookup = "0r1-ALICE".to_string();
        let thumbnail = vec![0x89, b'P', b'N', b'G', 1, 2, 3];
        assert!(
            !messages.linked && !messages.ask_link,
            "only ours: no banner"
        );
        session
            .request(Request::Rename {
                peer: peer.clone(),
                name: "Mum".to_string(),
            })
            .expect("rename");
        session
            .request(Request::Link {
                peer: peer.clone(),
                contact: schema::ContactCopy {
                    lookup: lookup.clone(),
                    contact_id: 17,
                    name: "Alice Schmidt".to_string(),
                    photo: Some(schema::Blob(thumbnail.clone())),
                },
            })
            .expect("link");
        let short = &peer[..4];
        let shown: schema::Messages = rmp_serde::from_slice(
            &session
                .query(schema::Query::Messages { peer: peer.clone() })
                .expect("query"),
        )
        .expect("decode");
        // The petname first, and the ugly store's other "Mum" marks both.
        assert_eq!(
            shown.name,
            format!("Mum ·{short} {}", loquax::names::COLLISION),
            "the petname first"
        );
        assert!(shown.linked);
        assert_eq!(
            (shown.lookup.as_deref(), shown.contact_id),
            (Some(lookup.as_str()), Some(17))
        );
        assert_eq!(shown.petname.as_deref(), Some("Mum"));
        let photo = shown.photo.clone().expect("the photo");
        assert_eq!(std::fs::read(&photo).expect("the copy"), thumbnail);
        // The petname at the home node: an archive entry of its own.
        let home_store = loquax::store::Store::open_read_only(
            &home_dir.join(loquax::store::STORE_FILE),
            session.address(),
        )
        .expect("the home store")
        .expect("there");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let page = home_store.archive_since(0, usize::MAX).expect("page");
            let petnames: Vec<_> = page
                .entries
                .iter()
                .filter(|e| e.kind == loquax::home::KIND_PETNAME)
                .map(|e| loquax::home::wire::read_petname_entry(&e.packed).expect("petname"))
                .filter(|(peer, _)| *peer == [0x5a; 16])
                .collect();
            if !petnames.is_empty() {
                assert_eq!(petnames, [([0x5a; 16], Some("Mum".to_string()))]);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no petname reached home"
            );
            let _ = session.next_event(Duration::from_millis(200));
        }
        // The petname cleared, the contact's name; the list says it too.
        session
            .request(Request::Rename {
                peer: peer.clone(),
                name: String::new(),
            })
            .expect("clear");
        let list: schema::Conversations =
            rmp_serde::from_slice(&session.query(schema::Query::Conversations).expect("list"))
                .expect("decode");
        let row = list
            .conversations
            .iter()
            .find(|row| row.peer == peer)
            .expect("listed");
        assert_eq!(row.name, format!("Alice Schmidt ·{short}"));
        assert_eq!(row.initials, "AS");
        assert_eq!(
            (row.linked, row.photo.as_deref()),
            (true, Some(photo.as_str()))
        );
        let detail: schema::Addresses = rmp_serde::from_slice(
            &session
                .query(schema::Query::Contact {
                    lookup: lookup.clone(),
                })
                .expect("detail"),
        )
        .expect("decode");
        assert_eq!(detail.addresses.len(), 1);
        assert_eq!(detail.addresses[0].peer, peer);
        // A peer that announced nothing gets no suggestion.
        let none: schema::Suggestions = rmp_serde::from_slice(
            &session
                .query(schema::Query::Suggest {
                    peer: peer.clone(),
                    candidates: vec![schema::CandidateRow {
                        key: lookup.clone(),
                        display: "Alice Schmidt".to_string(),
                        ..schema::CandidateRow::default()
                    }],
                })
                .expect("suggest"),
        )
        .expect("decode");
        assert!(none.suggestions.is_empty());

        // The contact screen (1146): every field there, then the reader's
        // word on the peer, each change in the detail and the list.
        let detail = |session: &Session| -> schema::Detail {
            rmp_serde::from_slice(
                &session
                    .query(schema::Query::Detail { peer: peer.clone() })
                    .expect("detail"),
            )
            .expect("decode")
        };
        let list = |session: &Session, query: schema::Query| -> schema::Conversations {
            rmp_serde::from_slice(&session.query(query).expect("list")).expect("decode")
        };
        let shown = detail(&session);
        assert_eq!(shown.peer, peer);
        assert_eq!(shown.fingerprint.split(' ').count(), 8);
        assert_eq!(shown.fingerprint.replace(' ', ""), peer);
        assert_eq!((shown.qr_size, shown.qr.0.len()), (25, 625));
        assert!(shown.home_client && shown.last_sent > 0, "{shown:?}");
        assert!(!shown.last_sent_how.is_empty());
        assert!(!shown.verified && !shown.muted && !shown.pinned && !shown.blocked);
        session
            .request(Request::Mute {
                peer: peer.clone(),
                secs: 3600,
            })
            .expect("mute");
        session
            .request(Request::Pin {
                peer: peer.clone(),
                on: true,
            })
            .expect("pin");
        session
            .request(Request::Verify {
                peer: peer.clone(),
                on: true,
            })
            .expect("verify");
        let shown = detail(&session);
        assert!(shown.muted && shown.pinned && shown.verified, "{shown:?}");
        assert!(shown.muted_until > 0);
        let row = list(&session, schema::Query::Conversations);
        assert!(row
            .conversations
            .iter()
            .any(|r| r.peer == peer && r.muted && r.pinned));
        session
            .request(Request::Mute {
                peer: peer.clone(),
                secs: -1,
            })
            .expect("mute always");
        assert_eq!(detail(&session).muted_until, -1);
        session
            .request(Request::Mute {
                peer: peer.clone(),
                secs: 0,
            })
            .expect("unmute");
        assert!(!detail(&session).muted);
        session
            .request(Request::Archive {
                peer: peer.clone(),
                on: true,
            })
            .expect("archive");
        let main = list(&session, schema::Query::Conversations);
        assert!(main.conversations.iter().all(|r| r.peer != peer));
        // Beside the ugly store's archived one.
        assert_eq!(main.archived, 2);
        let archived = list(&session, schema::Query::Archived);
        assert_eq!(archived.conversations.len(), 2);
        assert!(archived.conversations.iter().any(|r| r.peer == peer));
        session
            .request(Request::Archive {
                peer: peer.clone(),
                on: false,
            })
            .expect("unarchive");
        session
            .request(Request::Block {
                peer: peer.clone(),
                on: true,
            })
            .expect("block");
        // Beside the ugly store's blocked one.
        let ugly_blocked = ugly_store::peer_hex(ugly_store::BLOCKED);
        let blocked = list(&session, schema::Query::Blocked);
        assert_eq!(blocked.conversations.len(), 2);
        assert!(blocked.conversations.iter().any(|r| r.peer == peer));
        let main = list(&session, schema::Query::Conversations);
        assert!(main.conversations.iter().all(|r| r.peer != peer));
        session
            .request(Request::Block {
                peer: peer.clone(),
                on: false,
            })
            .expect("unblock");
        let blocked = list(&session, schema::Query::Blocked);
        assert_eq!(blocked.conversations.len(), 1);
        assert_eq!(blocked.conversations[0].peer, ugly_blocked);
        let main = list(&session, schema::Query::Conversations);
        assert!(main.conversations.iter().any(|r| r.peer == peer));
        assert!(session
            .query(schema::Query::Detail {
                peer: "zz".to_string()
            })
            .unwrap_err()
            .contains("not an address"));

        // Negative: an address that is none, and a message id that is none.
        assert!(session
            .request(Request::Open {
                peer: "zz".to_string()
            })
            .unwrap_err()
            .contains("not an address"));
        assert!(session
            .request(Request::React {
                peer: peer.clone(),
                target: "00".to_string(),
                content: "ok".to_string()
            })
            .unwrap_err()
            .contains("not a message id"));
        session.stop();
        (address, peer)
    })
    .await
    .expect("the first session");

    // The second start: no code, no server, the conversation from the store.
    let again = Start {
        dir: dir.clone(),
        ..Start::default()
    };
    let other = code_for(&leviculum_core::Identity::generate(&mut rand_core::OsRng));
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&again).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(loquax::address::to_hex(&session.address()), address);
        assert_eq!(session.server(), format!("127.0.0.1:{port}"));
        let messages: schema::Messages = rmp_serde::from_slice(
            &session
                .query(schema::Query::Messages { peer: peer.clone() })
                .expect("query"),
        )
        .expect("decode");
        assert_eq!(messages.messages.len(), 2, "{messages:?}");
        assert_eq!(messages.messages[0].body, "hallo vom Telefon");
        // The link outlived the session; unlinked, the address again and
        // no photo kept.
        let short = &peer[..4];
        assert_eq!(messages.name, format!("Alice Schmidt ·{short}"));
        let photo = messages.photo.clone().expect("the photo kept");
        session
            .request(Request::Unlink { peer: peer.clone() })
            .expect("unlink");
        let unlinked: schema::Messages = rmp_serde::from_slice(
            &session
                .query(schema::Query::Messages { peer: peer.clone() })
                .expect("query"),
        )
        .expect("decode");
        assert_eq!(unlinked.name, format!("<{}…>", &peer[..8]));
        assert!(!unlinked.linked && unlinked.photo.is_none() && unlinked.lookup.is_none());
        assert!(!Path::new(&photo).exists());
        // 1146: the pin and the verified mark outlived the session.
        let shown: schema::Detail = rmp_serde::from_slice(
            &session
                .query(schema::Query::Detail { peer: peer.clone() })
                .expect("detail"),
        )
        .expect("decode");
        assert!(shown.pinned && shown.verified && !shown.muted, "{shown:?}");
        // The reply's field came back from the store with the message.
        assert_eq!(
            messages.messages[1].quote_to.as_deref(),
            Some(messages.messages[0].id.as_str())
        );
        session.stop();
        drop(session);

        let refused = Session::start(&Start {
            dir,
            code: Some(other),
            ..Start::default()
        })
        .err()
        .expect("another home's code");
        assert!(refused.text.contains("another mailbox"), "{refused:?}");
    })
    .await
    .expect("the second session");

    node.kill().await;
}

/// The wake on the phone (1123): a joined session, its open pull done,
/// then a loquax messenger on the home node's instance sends to the home
/// address. The session pulls on the home node's wake, with no `pull`
/// asked, says `changed`, and the conversation list shows the message.
/// Then the sender renames itself (1136): the status line hears nothing,
/// neither a wake's pull nor a count, and opening the conversation pulls
/// quietly and brings the new name.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_message_shows_on_the_phone_without_a_pull() {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    ugly(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        offset: 0,
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        // The pull on open is quiet; a Refresh says its end, and the
        // watching link's first word settles.
        session.request(Request::Pull).expect("pull");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            match session.next_event(Duration::from_millis(500)) {
                Some(Event::Sync { state, .. }) if state == "done" => break,
                Some(Event::Sync { state, detail, .. }) if state == "failed" => {
                    panic!("the pull on open failed: {detail}")
                }
                _ => assert!(std::time::Instant::now() < deadline, "no pull ended"),
            }
        }
        ugly_in(&session);
        std::thread::sleep(Duration::from_secs(3));
        while session.next_event(Duration::ZERO).is_some() {}
        session
    })
    .await
    .expect("the session");
    let address = session.address();

    let sender = tempfile::tempdir().expect("sender dir");
    let storage = sender.path().join("storage");
    std::fs::create_dir_all(&storage).expect("sender storage");
    let sender_identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let sender_address = loquax::address::to_hex(
        &loquax::engine::delivery_address(&sender_identity).expect("address"),
    );
    let mut attached = loquax::engine::attach(loquax::engine::AttachConfig {
        instance: instance.clone(),
        config_dir: Some(rns.path().to_path_buf()),
        storage_dir: storage.clone(),
        identity: sender_identity.clone(),
        display_name: b"sender".to_vec(),
        notices: loquax::effects::Terminal::new(sender.path().to_path_buf()),
    })
    .await
    .expect("the sender attaches");
    let body = "ohne Ziehen";
    loquax::send::run_send(
        &mut attached.outbox,
        loquax::outbox::SendRequest {
            destination: address,
            title: Vec::new(),
            body: body.as_bytes().to_vec(),
            fields: Vec::new(),
            via: loquax::outbox::Via::Direct,
        },
        &loquax::send::SendOptions::new(instance.clone(), Duration::from_secs(40)),
        &loquax::clock::Tokio::new(),
    )
    .await
    .expect("the sender hands it on");
    let sent_at = std::time::Instant::now();

    let session = tokio::task::spawn_blocking(move || {
        let mut changed = false;
        // The wake's pull is quiet but for its result: one new message.
        let mut said_one = false;
        let mut look = |event: Option<Event>, changed: &mut bool| match event {
            Some(Event::Changed { .. }) => *changed = true,
            Some(Event::Sync { state, detail, .. }) => {
                assert_eq!((state.as_str(), detail.as_str()), ("done", "1"));
                said_one = true;
            }
            _ => {}
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let list: schema::Conversations = rmp_serde::from_slice(
                &session
                    .query(schema::Query::Conversations)
                    .expect("a query"),
            )
            .expect("decode");
            if changed && list.conversations.iter().any(|c| c.last == body) {
                eprintln!(
                    "WAKE phone shows it {} ms after the send returned",
                    sent_at.elapsed().as_millis()
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the message never showed without a pull: {list:?}"
            );
            look(session.next_event(Duration::from_millis(100)), &mut changed);
        }
        // The pull's end comes in the pump that showed the message, or the
        // next; nothing of this pull is left for the rename below.
        let settled = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < settled {
            look(session.next_event(Duration::from_millis(100)), &mut changed);
        }
        assert!(said_one, "the wake's pull said its one new message");
        session
    })
    .await
    .expect("the wait for the wake");
    attached.stop().await;

    // The sender comes back under another name and announces it.
    let renamed = loquax::engine::attach(loquax::engine::AttachConfig {
        instance: instance.clone(),
        config_dir: Some(rns.path().to_path_buf()),
        storage_dir: storage,
        identity: sender_identity,
        display_name: b"renamed".to_vec(),
        notices: loquax::effects::Terminal::new(sender.path().to_path_buf()),
    })
    .await
    .expect("the sender attaches again");
    loquax::outbox::Outbox::submit(&renamed.outbox, loquax::outbox::Command::Announce)
        .expect("announce");
    tokio::task::spawn_blocking(move || {
        // Whatever pulls run meanwhile: a name says nothing on the line.
        let silent = |event: Option<Event>| {
            assert!(
                !matches!(event, Some(Event::Sync { .. } | Event::Notice { .. })),
                "a name made the status line speak: {event:?}"
            );
        };
        let quiet_until = std::time::Instant::now() + Duration::from_secs(4);
        while std::time::Instant::now() < quiet_until {
            silent(session.next_event(Duration::from_millis(100)));
        }
        // Opening the conversation pulls, quietly, and the name comes. It
        // is opened again every two seconds, in case its pull found one
        // running that began before the name was archived.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut opened = std::time::Instant::now() - Duration::from_secs(2);
        loop {
            if opened.elapsed() >= Duration::from_secs(2) {
                session
                    .request(Request::Open {
                        peer: sender_address.clone(),
                    })
                    .expect("open");
                opened = std::time::Instant::now();
            }
            let messages: schema::Messages = rmp_serde::from_slice(
                &session
                    .query(schema::Query::Messages {
                        peer: sender_address.clone(),
                    })
                    .expect("a query"),
            )
            .expect("decode");
            // The label is the name with the screen's short-hash mark.
            if messages.name.starts_with("renamed") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the new name never came: {:?}",
                messages.name
            );
            silent(session.next_event(Duration::from_millis(100)));
        }
        session.stop();
    })
    .await
    .expect("the rename");
    renamed.stop().await;
    node.kill().await;
}

/// Lew's migration (1159): the mailbox imported a Ratspeak history and its
/// contacts' names, the phone had joined and pulled before. The next pull
/// brings the imported rows ([`loquax::home::KIND_IMPORTED`]) and the name
/// as a petname entry, and the same live session lists the conversation
/// under that name and shows its messages: closing the app restarts
/// nothing, so a restart is no answer.
#[tokio::test(flavor = "multi_thread")]
async fn imported_history_shows_in_the_live_session_under_its_name() {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    ugly(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        offset: 0,
        ..Start::default()
    };
    let pulled = |session: &Session, what: &str| {
        session.request(Request::Pull).expect("pull");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            match session.next_event(Duration::from_millis(500)) {
                Some(Event::Sync { state, .. }) if state == "done" => break,
                Some(Event::Sync { state, detail, .. }) if state == "failed" => {
                    panic!("{what} failed: {detail}")
                }
                _ => assert!(std::time::Instant::now() < deadline, "{what} never ended"),
            }
        }
    };
    let session = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        pulled(&session, "the pull on open");
        ugly_in(&session);
        let list: schema::Conversations =
            rmp_serde::from_slice(&session.query(schema::Query::Conversations).expect("list"))
                .expect("decode");
        let imported = loquax::address::to_hex(&[0x09; 16]);
        assert!(
            list.conversations.iter().all(|row| row.peer != imported),
            "{list:?}"
        );
        session
    })
    .await
    .expect("the session");

    // The import on the mailbox: two rows of one conversation, and the
    // contact's name.
    let peer = [0x09; 16];
    let peer_hex = loquax::address::to_hex(&peer);
    import_ratspeak(home.path(), &peer);

    tokio::task::spawn_blocking(move || {
        // The rows and the name are two commits: a pull between them lists
        // the conversation unnamed, and the name comes with the wake the
        // daemon sends for the second (1164).
        pulled(&session, "the pull after the import");
        let list: schema::Conversations = wait_for(
            &session,
            schema::Query::Conversations,
            20,
            |c: &schema::Conversations| {
                c.conversations
                    .iter()
                    .any(|row| row.peer == peer_hex && row.name.starts_with("Ratte"))
            },
        );
        let row = list
            .conversations
            .iter()
            .find(|row| row.peer == peer_hex)
            .expect("listed");
        assert!(row.name.starts_with("Ratte"), "{row:?}");
        assert_eq!(row.last, "zurueck");
        let messages: schema::Messages = rmp_serde::from_slice(
            &session
                .query(schema::Query::Messages {
                    peer: peer_hex.clone(),
                })
                .expect("a query"),
        )
        .expect("decode");
        let bodies: Vec<&str> = messages.messages.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(bodies, ["von Ratspeak", "zurueck"]);
        session.stop();
    })
    .await
    .expect("the imported history");
    node.kill().await;
}

/// 1162: the mailbox runs the real import of a NomadNet tree after the
/// phone joined. Its history goes into the archive as a Ratspeak one does,
/// its name as a petname and its block as a peer state, so the phone's
/// next pull lists the conversation under the name in the same live
/// session, shows both messages, and lists the blocked peer.
#[tokio::test(flavor = "multi_thread")]
async fn a_nomadnet_import_on_the_mailbox_shows_in_the_live_session_with_its_block() {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    ugly(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        offset: 0,
        ..Start::default()
    };
    let pulled = |session: &Session, what: &str| {
        session.request(Request::Pull).expect("pull");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            match session.next_event(Duration::from_millis(500)) {
                Some(Event::Sync { state, .. }) if state == "done" => break,
                Some(Event::Sync { state, detail, .. }) if state == "failed" => {
                    panic!("{what} failed: {detail}")
                }
                _ => assert!(std::time::Instant::now() < deadline, "{what} never ended"),
            }
        }
    };
    let session = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        pulled(&session, "the pull on open");
        ugly_in(&session);
        session
    })
    .await
    .expect("the session");

    // The import on the mailbox, as `loquax import <tree>` runs it there.
    let own = loquax::identity::load_or_create(&home.path().join("identity")).expect("identity");
    let peer = leviculum_std::generate_identity();
    let peer_hex = hex(&loquax::engine::delivery_address(&peer).expect("address"));
    let source = tempfile::tempdir().expect("source dir");
    let root = nomadnet_tree::write(&source.path().join(".nomadnetwork"), &own, &peer);
    let found = loquax::import::sniff(&root).expect("a NomadNet tree");
    let choices = loquax::import::Choices {
        contacts: true,
        history: true,
        identity: false,
    };
    let report = loquax::import::import(&found, home.path(), choices, false).expect("import");
    assert_eq!(report.archived, Some(2), "{:?}", report.lines(false));
    assert_eq!(report.blocks_archived, Some(1));

    tokio::task::spawn_blocking(move || {
        // A pull of the start may still run while the import writes and
        // take only part of it; the daemon sees the rest written beside it
        // and wakes the session for it (1164). One pull, then wait.
        pulled(&session, "the pull after the import");
        let list: schema::Conversations = wait_for(
            &session,
            schema::Query::Conversations,
            20,
            |c: &schema::Conversations| {
                c.conversations
                    .iter()
                    .any(|row| row.peer == peer_hex && row.name.starts_with(nomadnet_tree::NAME))
            },
        );
        let row = list
            .conversations
            .iter()
            .find(|row| row.peer == peer_hex)
            .expect("listed");
        assert!(row.name.starts_with(nomadnet_tree::NAME), "{row:?}");
        assert_eq!(row.last, nomadnet_tree::RECEIVED);
        let messages: schema::Messages = rmp_serde::from_slice(
            &session
                .query(schema::Query::Messages {
                    peer: peer_hex.clone(),
                })
                .expect("a query"),
        )
        .expect("decode");
        let bodies: Vec<&str> = messages.messages.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(bodies, [nomadnet_tree::SENT, nomadnet_tree::RECEIVED]);
        let blocked_hex = hex(&nomadnet_tree::BLOCKED);
        let blocked: schema::Conversations = wait_for(
            &session,
            schema::Query::Blocked,
            20,
            |c: &schema::Conversations| c.conversations.iter().any(|row| row.peer == blocked_hex),
        );
        // Beside the ugly store's blocked one.
        assert_eq!(blocked.conversations.len(), 2, "{blocked:?}");
        session.stop();
    })
    .await
    .expect("the imported history");
    node.kill().await;
}

/// What `loquax import` of a Ratspeak history writes on the mailbox, on a
/// store connection of its own as the import's process has: two rows of
/// one conversation with `peer`, and the contact's name "Ratte".
fn import_ratspeak(home: &Path, peer: &[u8; 16]) {
    let identity = loquax::identity::load_or_create(&home.join("identity")).expect("identity");
    let address = loquax::engine::delivery_address(&identity).expect("address");
    let mut store = loquax::store::Store::open_in(home, address).expect("home store");
    let row = |id: u8, outbound: bool, at: i64, content: &str| loquax::store::ImportRow {
        id: [id; 32],
        outbound,
        timestamp: Some(at as f64),
        received_at: Some(at),
        title: Vec::new(),
        content: content.to_string(),
        fields: Vec::new(),
        delivery: None,
        signature: loquax::ui::Signature::Unverified,
        origin_verdict: None,
        held: None,
    };
    let entries: Vec<([u8; 32], i64, Vec<u8>)> = [
        row(0xa1, false, 1_000, "von Ratspeak"),
        row(0xa2, true, 2_000, "zurueck"),
    ]
    .iter()
    .map(|r| {
        let at = r.received_at.unwrap_or(0);
        (
            r.id,
            at,
            loquax::import::archive::entry("ratspeak", peer, r),
        )
    })
    .collect();
    assert_eq!(
        store.archive_imported(&entries, 1 << 20).expect("archive"),
        2
    );
    let names = [(peer, "Ratte")];
    assert_eq!(
        loquax::home::server::archive_petnames(&mut store, names, 3_000, 1 << 20)
            .expect("the name"),
        1
    );
}

/// 1166, Lew's long press on a message of the imported conversation, then
/// "Info": the store's keys lie on both sides of seq 0 since the import
/// (1159), and a message key crosses JNI as Java's `long`, which its codec
/// writes as a signed int64 (`d3`) and reads back from a uint64 bit for
/// bit. Every request that carries a key, sent as `MsgPack.java` sends it
/// with the key as `Core.num` read it from the list, must reach its
/// message: Info answers with the message's detail, forward copies it,
/// delete takes it away; reply and react, which name the id, ride along.
#[tokio::test(flavor = "multi_thread")]
async fn every_keyed_request_reaches_an_imported_message_through_javas_codec() {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    ugly(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        offset: 0,
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        session.request(Request::Pull).expect("pull");
        session
    })
    .await
    .expect("the session");

    let peer = [0x0c; 16];
    let peer_hex = loquax::address::to_hex(&peer);
    let other_hex = loquax::address::to_hex(&[0x0d; 16]);
    import_ratspeak(home.path(), &peer);

    tokio::task::spawn_blocking(move || {
        let messages = |peer: &str| -> JavaMessages {
            let bytes = session
                .query(schema::Query::Messages {
                    peer: peer.to_string(),
                })
                .expect("a query");
            rmp_serde::from_slice(&bytes).expect("decode as Java reads it")
        };
        let command = |entries: &[(&str, Java)]| -> Result<(), String> {
            session.request(schema::decode(&java_map(entries))?)
        };
        session.request(Request::Pull).expect("pull");
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let rows = loop {
            let rows = messages(&peer_hex).messages;
            if rows.len() == 2 {
                break rows;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "never imported: {rows:?}"
            );
            let _ = session.next_event(Duration::from_millis(200));
        };
        let (older, newer) = (&rows[0], &rows[1]);
        assert_eq!(older.body, "von Ratspeak");

        // Info first, on every row: the query Lew's long press sent. Each
        // import lays its block out below the store's lowest key, so these
        // rows lie below the ugly store's imports, whose newest sits at seq
        // 0 (checked below); a row of this device's own lies above them.
        for row in &rows {
            let info = java_map(&[
                ("t", Java::Str("info")),
                ("peer", Java::Str(&peer_hex)),
                ("key", Java::Long(row.key.0)),
            ]);
            let query = schema::decode::<schema::Query>(&info)
                .unwrap_or_else(|error| panic!("info on {:?}: {error}", row.body));
            let detail: schema::Info =
                rmp_serde::from_slice(&session.query(query).expect("the detail")).expect("an Info");
            let id_row = format!("id     {}", row.id);
            assert!(detail.rows.contains(&id_row), "{:?}", detail.rows);
        }

        command(&[
            ("t", Java::Str("reply")),
            ("peer", Java::Str(&peer_hex)),
            ("target", Java::Str(&older.id)),
            ("body", Java::Str("antwort")),
        ])
        .expect("reply");
        command(&[
            ("t", Java::Str("react")),
            ("peer", Java::Str(&peer_hex)),
            ("target", Java::Str(&older.id)),
            ("content", Java::Str("\u{1f44d}")),
        ])
        .expect("react");
        command(&[
            ("t", Java::Str("forward")),
            ("peer", Java::Str(&peer_hex)),
            ("key", Java::Long(older.key.0)),
            ("to", Java::Str(&other_hex)),
        ])
        .expect("forward");
        let forwarded = messages(&other_hex).messages;
        assert!(
            forwarded.iter().any(|row| row.body == "von Ratspeak"),
            "{forwarded:?}"
        );
        command(&[
            ("t", Java::Str("delete")),
            ("peer", Java::Str(&peer_hex)),
            ("keys", Java::Longs(&[newer.key.0])),
        ])
        .expect("delete");
        let left = messages(&peer_hex).messages;
        assert!(left.iter().all(|row| row.key != newer.key), "{left:?}");
        assert!(left.iter().any(|row| row.key == older.key), "{left:?}");

        // 1173: the ugly store's two imports, a Ratspeak one (entry
        // version 1) and a NomadNet one (version 2, with provenance), get
        // every request the facade knows, keyed ones in Java's bytes.
        ugly_in(&session);
        let mut seen = Vec::new();
        let ask = |query: schema::Query| -> Vec<u8> {
            session
                .query(query.clone())
                .unwrap_or_else(|error| panic!("{query:?}: {error}"))
        };
        for i in [ugly_store::RATSPEAK, ugly_store::NOMADNET] {
            let peer = ugly_store::peer_hex(i);
            let name = ugly_store::petname(i).expect("a petname");
            command(&[("t", Java::Str("open")), ("peer", Java::Str(&peer))])
                .unwrap_or_else(|error| panic!("open {i}: {error}"));
            let rows = messages(&peer).messages;
            let keys: Vec<i64> = rows.iter().map(|r| r.key.0).collect();
            seen.extend_from_slice(&keys);
            assert_eq!(
                rows.len(),
                ugly_store::PER_CONVERSATION - ugly_store::REACTIONS,
                "{i}: {keys:?}"
            );
            assert!(rows.iter().any(|r| r.body == ugly_store::long_body()));
            // The imports lay their blocks out below seq 1 (1159): store
            // keys of negative seqs, Java's sign the other way round.
            assert!(
                keys.iter()
                    .all(|key| loquax::store::seq_of(*key as u64) < 1),
                "{i}: {keys:?}"
            );
            let typed: schema::Messages =
                rmp_serde::from_slice(&ask(schema::Query::Messages { peer: peer.clone() }))
                    .expect("Messages");
            assert!(typed.name.starts_with(name), "{i}: {:?}", typed.name);
            assert!(
                typed.messages.iter().any(|m| m.quote_to.is_some()),
                "{i}: a reply"
            );
            assert!(
                typed.messages.iter().any(|m| !m.chips.is_empty()),
                "{i}: a reaction"
            );
            assert!(
                typed.messages.iter().any(|m| m.lat_e6.is_some()),
                "{i}: a position"
            );
            for row in &rows {
                let info = java_map(&[
                    ("t", Java::Str("info")),
                    ("peer", Java::Str(&peer)),
                    ("key", Java::Long(row.key.0)),
                ]);
                let query = schema::decode::<schema::Query>(&info).expect("an info query");
                let detail: schema::Info = rmp_serde::from_slice(&ask(query)).expect("an Info");
                assert!(
                    detail.rows.contains(&format!("id     {}", row.id)),
                    "{i}: {:?}",
                    detail.rows
                );
            }
            let detail: schema::Detail =
                rmp_serde::from_slice(&ask(schema::Query::Detail { peer: peer.clone() }))
                    .expect("a Detail");
            assert_eq!(detail.peer, peer);
            let (first, last) = (&rows[0], &rows[rows.len() - 1]);
            command(&[
                ("t", Java::Str("reply")),
                ("peer", Java::Str(&peer)),
                ("target", Java::Str(&first.id)),
                ("body", Java::Str("Antwort auf den Ältesten")),
            ])
            .unwrap_or_else(|error| panic!("reply {i}: {error}"));
            command(&[
                ("t", Java::Str("react")),
                ("peer", Java::Str(&peer)),
                ("target", Java::Str(&first.id)),
                ("content", Java::Str("\u{2764}\u{FE0F}")),
            ])
            .unwrap_or_else(|error| panic!("react {i}: {error}"));
            command(&[
                ("t", Java::Str("forward")),
                ("peer", Java::Str(&peer)),
                ("key", Java::Long(first.key.0)),
                ("to", Java::Str(&other_hex)),
            ])
            .unwrap_or_else(|error| panic!("forward {i}: {error}"));
            assert!(
                messages(&other_hex)
                    .messages
                    .iter()
                    .any(|r| r.body == first.body),
                "{i}: forwarded"
            );
            command(&[
                ("t", Java::Str("delete")),
                ("peer", Java::Str(&peer)),
                ("keys", Java::Longs(&[last.key.0])),
            ])
            .unwrap_or_else(|error| panic!("delete {i}: {error}"));
            assert!(messages(&peer).messages.iter().all(|r| r.key != last.key));
            // Mark unread and read again, the petname cleared and given
            // back, the peer blocked and unblocked.
            for (t, on) in [
                ("unread", true),
                ("unread", false),
                ("block", true),
                ("block", false),
            ] {
                let request = schema::decode::<Request>(&java_bool_map(t, &peer, on))
                    .unwrap_or_else(|error| panic!("{t} {i}: {error}"));
                session
                    .request(request)
                    .unwrap_or_else(|error| panic!("{t} {i}: {error}"));
            }
            let list: schema::Conversations =
                rmp_serde::from_slice(&ask(schema::Query::Conversations)).expect("a list");
            let row = list
                .conversations
                .iter()
                .find(|row| row.peer == peer)
                .expect("listed again after the unblock");
            assert_eq!(row.unread, 0, "{row:?}");
            for name in ["", name] {
                command(&[
                    ("t", Java::Str("rename")),
                    ("peer", Java::Str(&peer)),
                    ("name", Java::Str(name)),
                ])
                .unwrap_or_else(|error| panic!("rename {i}: {error}"));
            }
            let typed: schema::Messages =
                rmp_serde::from_slice(&ask(schema::Query::Messages { peer: peer.clone() }))
                    .expect("Messages");
            assert!(typed.name.starts_with(name), "{i}: {:?}", typed.name);
            assert!(
                typed
                    .messages
                    .iter()
                    .any(|m| m.body == "Antwort auf den Ältesten"),
                "{i}: the reply"
            );
        }
        // The first import's newest row at seq 0: Java holds its key as
        // `i64::MIN`, the key Lew's Info failed on before 1166.
        assert!(seen.contains(&i64::MIN), "{seen:?}");
        session.stop();
    })
    .await
    .expect("the keyed requests");
    node.kill().await;
}

/// A [`schema::Messages`] as `MsgPack.java` and `Core.num` read it.
#[derive(Debug, serde::Deserialize)]
struct JavaMessages {
    messages: Vec<JavaRow>,
}

#[derive(Debug, serde::Deserialize)]
struct JavaRow {
    body: String,
    id: String,
    key: JavaLong,
}

/// An integer as Java's codec reads it into a `long`: a uint64 bit for bit
/// (`case 0xcf: return be(8)`).
#[derive(Debug, PartialEq, Eq)]
struct JavaLong(i64);

impl<'de> serde::Deserialize<'de> for JavaLong {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = JavaLong;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an integer")
            }
            fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<JavaLong, E> {
                Ok(JavaLong(n))
            }
            fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<JavaLong, E> {
                Ok(JavaLong(n as i64))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// A value of a request map as the app builds it.
enum Java<'a> {
    Str(&'a str),
    Long(i64),
    Longs(&'a [i64]),
}

/// A request map in the bytes `MsgPack.pack` writes: a long below -32 or
/// above 127 as `d3` and eight bytes, whatever its size.
fn java_map(entries: &[(&str, Java)]) -> Vec<u8> {
    fn long(out: &mut Vec<u8>, n: i64) {
        if (0..128).contains(&n) || (-32..0).contains(&n) {
            out.push(n as u8);
        } else {
            out.push(0xd3);
            out.extend_from_slice(&n.to_be_bytes());
        }
    }
    fn string(out: &mut Vec<u8>, s: &str) {
        let n = s.len();
        if n < 32 {
            out.push(0xa0 | n as u8);
        } else if n < 0x100 {
            out.extend_from_slice(&[0xd9, n as u8]);
        } else {
            out.push(0xda);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        out.extend_from_slice(s.as_bytes());
    }
    let mut out = vec![0x80 | entries.len() as u8];
    for (name, value) in entries {
        string(&mut out, name);
        match value {
            Java::Str(s) => string(&mut out, s),
            Java::Long(n) => long(&mut out, *n),
            Java::Longs(ns) => {
                out.push(0x90 | ns.len() as u8);
                for n in *ns {
                    long(&mut out, *n);
                }
            }
        }
    }
    out
}

/// A request map of `t` with a peer and a boolean `on`, as `MsgPack.pack`
/// writes a Java `boolean` (`c3` and `c2`).
fn java_bool_map(t: &str, peer: &str, on: bool) -> Vec<u8> {
    let mut out = java_map(&[("t", Java::Str(t)), ("peer", Java::Str(peer))]);
    out[0] += 1;
    out.extend_from_slice(&[0xa2, b'o', b'n', if on { 0xc3 } else { 0xc2 }]);
    out
}

/// 1164: an import on the running mailbox runs outside its daemon, on a
/// store connection of its own. A phone that only watches, its one pull on
/// open done and its watching link up, asks for nothing more: the daemon
/// sees the archive move under it within a second, wakes the phone, and
/// the phone's wake pulls list the imported conversation under its name,
/// the name being a commit of its own.
#[tokio::test(flavor = "multi_thread")]
async fn an_import_beside_the_running_mailbox_wakes_a_phone_that_only_watches() {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    ugly(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        offset: 0,
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        session.request(Request::Pull).expect("pull");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            match session.next_event(Duration::from_millis(500)) {
                Some(Event::Sync { state, .. }) if state == "done" => break,
                Some(Event::Sync { state, detail, .. }) if state == "failed" => {
                    panic!("the pull on open failed: {detail}")
                }
                _ => assert!(std::time::Instant::now() < deadline, "no pull ended"),
            }
        }
        ugly_in(&session);
        // The start's other pulls end and the watching link comes up.
        let settled = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < settled {
            let _ = session.next_event(Duration::from_millis(100));
        }
        session
    })
    .await
    .expect("the session");

    let peer = [0x0b; 16];
    let peer_hex = loquax::address::to_hex(&peer);
    import_ratspeak(home.path(), &peer);
    let imported_at = std::time::Instant::now();

    tokio::task::spawn_blocking(move || {
        let list: schema::Conversations = wait_for(
            &session,
            schema::Query::Conversations,
            WAKE_BOUND_SECS,
            |c: &schema::Conversations| {
                c.conversations
                    .iter()
                    .any(|row| row.peer == peer_hex && row.name.starts_with("Ratte"))
            },
        );
        eprintln!(
            "WAKE phone lists the import {} ms after it committed",
            imported_at.elapsed().as_millis()
        );
        let row = list
            .conversations
            .iter()
            .find(|row| row.peer == peer_hex)
            .expect("listed");
        assert!(row.name.starts_with("Ratte"), "{row:?}");
        assert_eq!(row.last, "zurueck");
        session.stop();
    })
    .await
    .expect("the woken pull");
    node.kill().await;
}

/// How long a phone that only watches may wait for what another process
/// wrote on the mailbox: the daemon's look at the store (a second), the
/// wake's spacing (half a second), and the pull over the loopback, with
/// room for a loaded test host.
const WAKE_BOUND_SECS: u64 = 5;

/// A shared-instance name of this test process's own.
fn instance() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "loquax-android-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// A home node: a Rust transport node with a TCP server on `listener`, its
/// config directory `rns`, and `loquax::home::daemon` of `home` on its
/// shared instance, both in this process.
struct HomeNode {
    transport: ReticulumNode,
    daemon: tokio::task::JoinHandle<Result<(), String>>,
}

impl HomeNode {
    async fn start(
        listener: std::net::TcpListener,
        instance: &str,
        rns: &Path,
        home: &Path,
    ) -> Self {
        let mut transport = ReticulumNodeBuilder::new()
            .enable_transport(true)
            .share_instance(true)
            .instance_name(instance.to_string())
            .add_tcp_server_listener(listener)
            .storage_path(rns.join("storage"))
            .build_sync()
            .expect("the transport node");
        transport.start().await.expect("start it");
        tokio::time::sleep(Duration::from_millis(500)).await;

        let identity = loquax::identity::load_or_create(&home.join("identity")).expect("identity");
        let daemon_storage = home.join("storage");
        std::fs::create_dir_all(&daemon_storage).expect("daemon storage");
        let daemon = tokio::spawn(loquax::home::daemon::run(
            loquax::home::daemon::DaemonOptions {
                home: home.to_path_buf(),
                upstream: loquax::home::daemon::DaemonUpstream::Shared {
                    instance: instance.to_string(),
                    config_dir: rns.to_path_buf(),
                    mailbox: Box::new(|| {
                        Box::pin(async { loquax::mailbox_state::MailboxRun { input: None } })
                    }),
                },
                storage_dir: daemon_storage,
                identity,
                display_name: b"home".to_vec(),
                archive_cap_bytes: 1 << 20,
                token_ttl_secs: 3600,
                policy: loquax::home::policy(&loquax::config::Config::default()),
                preferred_pn: None,
                pn_source: "none",
                sync_bound_ms: None,
                fetch_budget: Duration::from_secs(10),
                wake: true,
                stop: None,
            },
        ));
        Self { transport, daemon }
    }

    /// Kill both, as a power cut does to the box they run on: the daemon
    /// gone with its lock, the TCP server closed.
    async fn kill(mut self) {
        self.daemon.abort();
        let _ = self.daemon.await;
        self.transport
            .stop()
            .await
            .expect("stop the transport node");
    }
}

/// The next event through the facade, up to `secs`, that `want` picks;
/// the others pass. A `stopped` that `want` does not pick fails the test.
fn facade_event(secs: u64, what: &str, want: impl Fn(&Event) -> bool) -> Event {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no {what} in {secs} s"
        );
        let Some(bytes) = loquax_android::next_event(Duration::from_millis(200)) else {
            continue;
        };
        let event: Event = rmp_serde::from_slice(&bytes).expect("an Event");
        if want(&event) {
            return event;
        }
        assert!(
            !matches!(event, Event::Stopped { .. }),
            "stopped while waiting for {what}: {event:?}"
        );
    }
}

/// Wait up to `secs` for the exported facade to list both imported
/// conversations of the ugly store under their names, and for Info to
/// answer on every row of each.
fn facade_shows_ugly(secs: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    for i in [ugly_store::RATSPEAK, ugly_store::NOMADNET] {
        let peer = ugly_store::peer_hex(i);
        let rows = loop {
            let messages: schema::Messages = rmp_serde::from_slice(&loquax_android::query(
                &schema::encode(&schema::Query::Messages { peer: peer.clone() }),
            ))
            .expect("Messages");
            if messages.messages.len() == ugly_store::PER_CONVERSATION - ugly_store::REACTIONS {
                break messages.messages;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the facade never showed conversation {i}: {} rows",
                messages.messages.len()
            );
            let _ = loquax_android::next_event(Duration::from_millis(200));
        };
        for row in rows {
            let info: schema::Info = rmp_serde::from_slice(&loquax_android::query(
                &schema::encode(&schema::Query::Info {
                    peer: peer.clone(),
                    key: row.key,
                }),
            ))
            .unwrap_or_else(|error| panic!("info on {:?}: {error}", row.body));
            assert!(
                info.rows.iter().any(|r| r.contains(&row.id)),
                "{:?}",
                info.rows
            );
        }
    }
}

/// Wait until the list's `home_link` (1199), the status dot, says `up`:
/// the watching link came up or went down.
fn facade_home_link(up: bool, secs: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let list: schema::Conversations = rmp_serde::from_slice(&loquax_android::query(
            &schema::encode(&schema::Query::Conversations),
        ))
        .expect("Conversations");
        if list.home_link == up {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the list's home_link never said {up} in {secs} s"
        );
        // The pump reads the watching link's word: let events flow.
        let _ = loquax_android::next_event(Duration::from_millis(200));
    }
}

fn link_event(up: bool, secs: u64) {
    facade_event(
        secs,
        &format!("link up={up}"),
        |event| matches!(event, Event::Link { up: now, .. } if *now == up),
    );
}

/// Send `body` to `peer` through the facade and wait until the home node
/// took it: its bubble carries an id.
fn send_through_facade(peer: &str, body: &str, secs: u64) {
    let done: schema::Done =
        rmp_serde::from_slice(&loquax_android::command(&schema::encode(&Request::Send {
            peer: peer.to_string(),
            body: body.to_string(),
        })))
        .expect("a Done");
    assert!(done.ok, "{done:?}");
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    let mut notices = Vec::new();
    loop {
        let messages: schema::Messages = rmp_serde::from_slice(&loquax_android::query(
            &schema::encode(&schema::Query::Messages {
                peer: peer.to_string(),
            }),
        ))
        .expect("Messages");
        let bubble = messages.messages.iter().find(|m| m.body == body);
        if let Some(bubble) = bubble.filter(|b| !b.id.is_empty()) {
            assert_ne!(bubble.state, "failed", "{bubble:?}");
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the home node never took {body:?}: {messages:?}, notices {notices:?}"
        );
        if let Some(bytes) = loquax_android::next_event(Duration::from_millis(200)) {
            if let Ok(Event::Notice { text, .. }) = rmp_serde::from_slice(&bytes) {
                notices.push(text);
            }
        }
    }
}

/// The phone's session over a home node that goes away and comes back,
/// and over an engine that dies, through the exported facade as Java
/// calls it. The first start takes the code with the address behind it
/// and no server. The link event says when the TCP interface is up; the
/// home node killed, it says down, and the leaf's TCP client redials the
/// home node brought back on the same port, so a send goes through again
/// in the same session. The list's `home_link`, the status dot (1199),
/// follows the watching link up, down and up again. The engine dead, the library says `stopped` with
/// why and drops the session; the service's next start, with neither code
/// nor server, builds a fresh one, and a send goes through that.
#[tokio::test(flavor = "multi_thread")]
async fn a_phone_session_rides_out_a_dead_home_node_and_a_dead_engine() {
    let _facade = FACADE.lock().await;
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    ugly(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let dir = phone.path().display().to_string();
    let server = format!("127.0.0.1:{port}");
    let first = Start {
        dir: dir.clone(),
        code: Some(format!("{code}@{server}")),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    let peer = loquax::address::to_hex(&[0x6b; 16]);
    let (to, expected) = (peer.clone(), server.clone());
    tokio::task::spawn_blocking(move || {
        let answer = started(&loquax_android::start(&schema::encode(&first)));
        assert!(answer.ok, "{answer:?}");
        assert_eq!(answer.server.as_deref(), Some(expected.as_str()));
        link_event(true, 30);
        facade_home_link(true, 30);
        facade_shows_ugly(60);
        send_through_facade(&to, "vor dem Ausfall", 60);
    })
    .await
    .expect("the first session");

    node.kill().await;
    tokio::task::spawn_blocking(|| {
        link_event(false, 30);
        facade_home_link(false, 30);
    })
    .await
    .expect("the link goes down");

    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("the same port again");
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let to = peer.clone();
    tokio::task::spawn_blocking(move || {
        // The TCP client redials every 5 s, doubling after the third try.
        link_event(true, 90);
        facade_home_link(true, 60);
        send_through_facade(&to, "nach dem Ausfall", 60);
    })
    .await
    .expect("the session after the home node came back");

    let to = peer.clone();
    tokio::task::spawn_blocking(move || {
        assert!(loquax_android::simulate_engine_death());
        let stopped = facade_event(10, "stopped", |e| matches!(e, Event::Stopped { .. }));
        match stopped {
            Event::Stopped { detail, .. } => {
                assert!(detail.contains("stopped answering"), "{detail}")
            }
            other => panic!("{other:?}"),
        }
        // The dead session is gone from the library, not handed out again.
        let done: schema::Done =
            rmp_serde::from_slice(&loquax_android::command(&schema::encode(&Request::Pull)))
                .expect("a Done");
        assert_eq!(done.error.as_deref(), Some("not started"));

        // The service's retry: neither code nor server, a fresh session.
        let again = Start {
            dir,
            ..Start::default()
        };
        let answer = started(&loquax_android::start(&schema::encode(&again)));
        assert!(answer.ok, "{answer:?}");
        assert_eq!(answer.server.as_deref(), Some(server.as_str()));
        link_event(true, 30);
        send_through_facade(&to, "nach dem Neustart", 60);
        let messages: schema::Messages = rmp_serde::from_slice(&loquax_android::query(
            &schema::encode(&schema::Query::Messages { peer: to.clone() }),
        ))
        .expect("Messages");
        let bodies: Vec<&str> = messages.messages.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(
            bodies,
            ["vor dem Ausfall", "nach dem Ausfall", "nach dem Neustart"]
        );
        loquax_android::stop();
    })
    .await
    .expect("the session after the engine died");

    node.kill().await;
}

/// The mailbox on the phone (1152): the first start that chooses it needs no
/// code and no server, the phone's node dials its entry server (here a stock
/// Python RNS instance), a stock Python LXMF sender delivers DIRECT to the
/// phone's address and gets DELIVERED, and the message shows on the
/// screens, which reach the mailbox as its client. A second start finds the
/// choice and the same address. Negative: a phone that reads a mailbox on a
/// computer does not become one.
#[tokio::test(flavor = "multi_thread")]
async fn a_python_sender_reaches_the_mailbox_on_the_phone_and_its_screens() {
    let peer = python_peer::PythonPeer::start();
    peer.lxmf_init();
    let phone = tempfile::tempdir().expect("app dir");
    let mailbox = phone.path().join(loquax_android::mailbox::DIR);
    std::fs::create_dir_all(&mailbox).expect("mailbox dir");
    std::fs::write(
        mailbox.join(loquax_android::mailbox::SERVERS_FILE),
        format!("127.0.0.1:{}\n", peer.rns_port),
    )
    .expect("servers file");
    // The phone's own mailbox holds the ugly store; its screens are its
    // client and take it on their pull.
    let filled = ugly_store::fill_archive(&mailbox);
    assert_eq!(filled.imported, 2 * ugly_store::PER_CONVERSATION);
    let first = Start {
        dir: phone.path().display().to_string(),
        here: true,
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        ugly_in(&session);
        session
    })
    .await
    .expect("the session");
    assert!(session.is_here());
    assert!(
        session.server().starts_with("127.0.0.1:"),
        "{}",
        session.server()
    );
    let address = loquax::address::to_hex(&session.address());

    let body = "an das Postfach im Telefon";
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let message_hash = loop {
        match peer.try_rpc(
            "lxmf_send",
            serde_json::json!({
                "dest_hash": address,
                "method": "direct",
                "content": hex(body.as_bytes()),
            }),
        ) {
            Ok(result) => {
                break result
                    .get("message_hash")
                    .and_then(|v| v.as_str())
                    .expect("a message hash")
                    .to_string()
            }
            Err(error) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "Python never learned the phone's mailbox: {error}"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let state = peer.rpc(
            "lxmf_get_outbound_status",
            serde_json::json!({ "message_hash": message_hash }),
        );
        match state.get("state").and_then(|v| v.as_str()) {
            Some("DELIVERED") => break,
            Some("FAILED") | Some("REJECTED") => panic!("the sender gave up: {state}"),
            _ => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the sender never got DELIVERED: {state}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let session = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let list: schema::Conversations = rmp_serde::from_slice(
                &session
                    .query(schema::Query::Conversations)
                    .expect("a query"),
            )
            .expect("decode");
            if list.conversations.iter().any(|c| c.last == body) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the screens never showed it: {list:?}"
            );
            session.next_event(Duration::from_millis(200));
        }
        session
    })
    .await
    .expect("the screens");
    let address_before = session.address();

    // The last screen's address, and the backup the settings make: its
    // line opens to the identity the mailbox runs on.
    let (session, line) = tokio::task::spawn_blocking(move || {
        let me: schema::Me =
            rmp_serde::from_slice(&session.query(schema::Query::Me).expect("me")).expect("decode");
        assert!(me.here);
        assert_eq!(me.address, loquax::address::to_hex(&session.address()));
        assert_eq!(me.qr.0.len(), (me.qr_size * me.qr_size) as usize);
        let backup: schema::Backup = rmp_serde::from_slice(
            &session
                .query(schema::Query::Backup {
                    passphrase: "Kastanienallee 7".to_string(),
                })
                .expect("backup"),
        )
        .expect("decode");
        assert_eq!(
            backup.qr.0.len(),
            (backup.qr_size * backup.qr_size) as usize
        );
        (session, backup.line)
    })
    .await
    .expect("the backup");
    let private = loquax::home::backup::open(&line, "Kastanienallee 7").expect("it opens");
    let restored = leviculum_core::Identity::from_private_key_bytes(&private).expect("a key");
    assert_eq!(
        loquax::engine::delivery_address(&restored).expect("address"),
        address_before
    );
    tokio::task::spawn_blocking(move || session.stop())
        .await
        .expect("stopped");

    // A new phone that restores with the wrong passphrase is asked again
    // and holds no address.
    let new_phone = tempfile::tempdir().expect("new phone");
    let dir = new_phone.path().display().to_string();
    let wrong = tokio::task::spawn_blocking(move || {
        Session::start(&Start {
            dir,
            here: true,
            restore: Some(line),
            passphrase: Some("Kastanienallee 8".to_string()),
            ..Start::default()
        })
        .err()
    })
    .await
    .expect("the wrong restore")
    .expect("refused");
    assert_eq!(wrong.need, Some("passphrase"), "{wrong:?}");
    assert!(!loquax_android::mailbox::is_here(new_phone.path()));

    // Again, as the sticky service starts it: no choice said, none needed.
    let again = Start {
        dir: phone.path().display().to_string(),
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        Session::start(&again).unwrap_or_else(|error| panic!("{error:?}"))
    })
    .await
    .expect("the second session");
    assert!(session.is_here());
    assert_eq!(session.address(), address_before);
    tokio::task::spawn_blocking(move || session.stop())
        .await
        .expect("stopped");

    // Negative: a client of a computer's mailbox is no mailbox.
    let reader = tempfile::tempdir().expect("reader dir");
    let elsewhere = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    loquax::home::client::save_client(reader.path(), &elsewhere.public_key_bytes())
        .expect("joined elsewhere");
    let dir = reader.path().display().to_string();
    let refused = tokio::task::spawn_blocking(move || {
        Session::start(&Start {
            dir,
            here: true,
            ..Start::default()
        })
        .err()
    })
    .await
    .expect("the refused start");
    let error = refused.expect("refused");
    assert!(error.text.contains("on a computer"), "{error:?}");
    assert!(!loquax_android::mailbox::is_here(reader.path()));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lew's phone, 2026-10-31 (1184): Columba's report a minute was a bubble,
/// an unread count and a notification each. Over the ugly store with a run
/// of 14 reports between two texts, the facade answers three rows, the
/// run's with its count and when it began, and counts it unread once; the
/// expand lists all 14, newest first. A report after the text starts a new
/// run, a row of its own counted once, which the next report updates in
/// place. The store keeps every report.
#[test]
fn a_run_of_reports_is_one_row_counted_once_and_expands() {
    use loquax::outbox::OutboxEvent;
    use loquax::ui::{update, UiEvent};
    use loquax_android::session;
    use ugly_store::{RUN, RUN_REPORTS};
    let home = tempfile::tempdir().expect("app dir");
    let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    ugly_store::fill_store_of(home.path(), &identity);
    ugly_store::fill_run_of(home.path(), &identity);
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let store = loquax::store::Store::open_in(home.path(), me).expect("the store");
    // As the session starts: the store's conversations, none open.
    let mut model = session::known(home.path(), &store).expect("a model");
    model.home_client = true;
    model.list_apart = true;
    update(
        &mut model,
        UiEvent::Clock(loquax::when::WallTime {
            unix_secs: ugly_store::BASE + 2 * 86_400,
            offset_secs: 0,
        }),
    );
    model.restore(store.restore().expect("restore"));
    update(&mut model, UiEvent::Close);
    let peer = ugly_store::peer(RUN);
    let c = model
        .conversations
        .iter()
        .position(|conversation| conversation.peer == peer)
        .expect("the run's conversation");

    let rows = session::rows(&model, c);
    let shape: Vec<(&str, u32)> = rows.iter().map(|r| (r.body.as_str(), r.updates)).collect();
    assert_eq!(
        shape,
        [
            ("losgefahren", 0),
            ("", RUN_REPORTS as u32),
            ("angekommen", 0)
        ]
    );
    let run = &rows[1];
    assert_eq!(
        run.lat_e6,
        Some(52_524_360 + RUN_REPORTS as i32),
        "the newest"
    );
    assert!(run.place.is_some() && run.accuracy.is_some(), "{run:?}");
    assert_eq!(
        run.since,
        ugly_store::BASE + 86_400 + 60,
        "the first report"
    );
    assert_eq!(loquax::ui::chat::list_row(&model, &peer).unread, 3);

    let expanded = session::run(&model, peer, loquax_android::schema::model_key(run.key));
    assert_eq!(expanded.positions.len(), RUN_REPORTS);
    assert_eq!(expanded.positions[0].key, run.key, "newest first");
    let lats: Vec<i32> = expanded.positions.iter().filter_map(|p| p.lat_e6).collect();
    let want: Vec<i32> = (1..=RUN_REPORTS as i32)
        .rev()
        .map(|j| 52_524_360 + j)
        .collect();
    assert_eq!(lats, want);
    // A text's row expands to itself alone.
    let alone = session::run(&model, peer, loquax_android::schema::model_key(rows[0].key));
    assert_eq!(alone.positions.len(), 1);

    // After the text a report starts a new run: a row of its own, unread
    // once; the next report updates that row, and counts nothing more.
    let report = |id: u8, lat: i32| {
        UiEvent::Engine(OutboxEvent::Received {
            message_id: [id; 32],
            source: peer,
            title: Vec::new(),
            body: Vec::new(),
            fields: ugly_store::position(ugly_store::BASE + 2 * 86_400, lat),
            verified: true,
        })
    };
    update(&mut model, report(0xe1, 52_600_000));
    let rows = session::rows(&model, c);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[3].updates, 0, "one report is no run yet");
    assert_eq!(loquax::ui::chat::list_row(&model, &peer).unread, 4);
    update(&mut model, report(0xe2, 52_600_001));
    let rows = session::rows(&model, c);
    assert_eq!(rows.len(), 4, "updated in place");
    assert_eq!((rows[3].updates, rows[3].lat_e6), (2, Some(52_600_001)));
    let row = loquax::ui::chat::list_row(&model, &peer);
    assert_eq!((row.unread, row.updates), (4, 2));
    // Every report is still there, and every one unread for the read marks.
    assert_eq!(model.conversations[c].messages.len(), RUN_REPORTS + 4);
    assert_eq!(model.conversations[c].unread, RUN_REPORTS + 4);
}
