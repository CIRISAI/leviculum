//! The facade the app's Java calls, on the host and without JNI: the bytes
//! in and out of `start`, `command`, `query` and `nextEvent`, and a session
//! against a real home node (`loquax::home::daemon`, in process) on a Rust
//! transport node with a TCP server, the leaf dialling in as the phone does.

use std::path::Path;
use std::time::Duration;

use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use loquax::ui::strings::Lang;
use loquax_android::schema::{self, Event, Request, Start, Started};
use loquax_android::session::Session;

#[path = "../../loquax/tests/support/python_peer.rs"]
mod python_peer;

#[path = "../../loquax/tests/support/nomadnet_tree.rs"]
mod nomadnet_tree;

#[path = "../../loquax/tests/support/ugly_store.rs"]
mod ugly_store;

#[path = "../../loquax/tests/support/lnpnd.rs"]
mod lnpnd_node;

mod support {
    pub mod java;
}
use support::java::{java_map, Java};

/// The exported facade keeps one session per process, and the tests of a
/// binary share the process: those that go through it take turns. Tokio's
/// lock, which the async test holds across its awaits.
static FACADE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The facade's turn, with no session left in the slot: a test that failed
/// before its `stop` leaves its session there, and the next `start` would
/// answer for that one (its server, its link) and fail a second test for
/// the first one's fault (1334).
async fn facade_turn() -> tokio::sync::MutexGuard<'static, ()> {
    let turn = FACADE.lock().await;
    tokio::task::spawn_blocking(loquax_android::stop)
        .await
        .expect("stop a session left over");
    turn
}

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
    // As `facade_turn`: no session left over from a failed test.
    loquax_android::stop();
    let dir = tempfile::tempdir().expect("app dir");
    let base = Start {
        dir: dir.path().display().to_string(),
        ..Start::default()
    };

    let answer = started(&loquax_android::start(&start_bytes(&base)));
    assert!(!answer.ok);
    assert_eq!(answer.need.as_deref(), Some("code"));
    assert_eq!(
        answer.error.as_deref(),
        Some("Scan or paste the code your mailbox shows.")
    );
    assert!(answer.details.unwrap().contains("reads no mailbox"));

    // The sentence in the phone's language, the parser's words under
    // Details (1263, finding 8).
    for (lang, said) in [
        (None, "That code is not a mailbox code."),
        (Some("de-DE"), "Das ist kein Postfach-Code."),
    ] {
        let answer = started(&loquax_android::start(&start_bytes(&Start {
            code: Some("lqxh1nonsense".to_string()),
            lang: lang.map(str::to_string),
            ..base.clone()
        })));
        assert_eq!(answer.need.as_deref(), Some("code"));
        assert_eq!(answer.error.as_deref(), Some(said));
        assert!(answer
            .details
            .unwrap()
            .starts_with("That is not a mailbox code"));
    }

    // A code without an address dials the shipped rendezvous (1196); with
    // "mesh only" in the app's config there is nothing to dial, and the
    // screen asks for the address as before.
    std::fs::write(dir.path().join("config"), "rendezvous_dial = off\n").expect("config");
    let code = code_for(&leviculum_core::Identity::generate(&mut rand_core::OsRng));
    let answer = started(&loquax_android::start(&start_bytes(&Start {
        code: Some(code.clone()),
        ..base.clone()
    })));
    assert_eq!(answer.need.as_deref(), Some("server"));
    assert_eq!(
        answer.error.as_deref(),
        Some("Loquax does not know where your mailbox is. Type where it is below.")
    );

    let answer = started(&loquax_android::start(&start_bytes(&Start {
        code: Some(code),
        server: Some("homenode.lan".to_string()),
        ..base
    })));
    assert_eq!(answer.need.as_deref(), Some("server"));
    assert!(answer.error.unwrap().contains("192.168.1.20:4242"));
    assert!(answer.details.unwrap().contains("has no port"));

    // A list typed by hand (1471) is refused, with the sentence naming one
    // address; a code's own list is not typed and goes whole.
    let answer = started(&loquax_android::start(&start_bytes(&Start {
        code: Some(code_for(&leviculum_core::Identity::generate(
            &mut rand_core::OsRng,
        ))),
        server: Some("a.example:4242,b.example:4242".to_string()),
        dir: dir.path().display().to_string(),
        ..Start::default()
    })));
    assert_eq!(answer.need.as_deref(), Some("server"));
    assert!(answer.error.unwrap().contains("192.168.1.20:4242"));
    assert!(answer.details.unwrap().contains("several servers"));

    let answer = started(&loquax_android::start(b"\xc1"));
    assert!(!answer.ok);
    assert!(answer.error.unwrap().contains("did not decode"));

    // Nothing runs, and the calls say so instead of failing.
    let done: schema::Done =
        rmp_serde::from_slice(&loquax_android::command(&schema::encode(&Request::Pull)))
            .expect("a Done");
    assert_eq!(done.error.as_deref(), Some("not started"));
    let done: schema::Done = rmp_serde::from_slice(&loquax_android::query(&schema::encode(
        &schema::Query::Conversations { search: None },
    )))
    .expect("a Done");
    assert_eq!(done.error.as_deref(), Some("not started"));
    // A code is read before anything runs (1263): the first start's
    // scanner asks, in the phone's language.
    let read = |text: &str, lang: Option<&str>| {
        loquax_android::query(&schema::encode(&schema::Query::Lxma {
            text: text.to_string(),
            lang: lang.map(str::to_string),
        }))
    };
    let done: schema::Done = rmp_serde::from_slice(&read("hello", Some("de"))).expect("a Done");
    assert_eq!(done.error.as_deref(), Some("Das ist kein Loquax-Code."));
    let mailbox: schema::Lxma = rmp_serde::from_slice(&read(
        &code_for(&leviculum_core::Identity::generate(&mut rand_core::OsRng)),
        None,
    ))
    .expect("a mailbox code reads");
    assert_eq!(mailbox.kind, "mailbox");
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

/// `code` with the rendezvous list `servers` behind it, as `mailbox admit`
/// prints it (1471).
fn with_servers(code: &str, servers: &[String]) -> String {
    loquax::home::code::HomeCode::parse(code)
        .expect("a code")
        .encode_with_rendezvous(servers)
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
    wait_for(
        session,
        schema::Query::Conversations { search: None },
        60,
        shown,
    );
    wait_for(
        session,
        schema::Query::Archived { search: None },
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
///
/// The code carries three servers, as `mailbox admit` prints the ladder
/// (1471), and the start names none, as the join screen sends it: the
/// session dials the code's list in its order and keeps all three.
#[tokio::test(flavor = "multi_thread")]
async fn a_phone_joins_sends_pulls_and_comes_back() {
    let instance = instance();
    // The transport's Reticulum config directory, its storage under
    // `storage/`, as lnsd keeps it: the home node takes its RPC key there.
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    // Two more rendezvous behind the home's own server, ports nobody
    // listens on: the ladder's later rungs, never reached.
    let silent = || {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("a loopback port")
            .local_addr()
            .expect("its address")
            .to_string()
    };
    let servers = vec![format!("127.0.0.1:{port}"), silent(), silent()];
    let code = with_servers(&admit(home.path()), &servers);
    ugly(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let home_dir = home.path().to_path_buf();

    let phone = tempfile::tempdir().expect("app dir");
    let dir = phone.path().display().to_string();
    let saved = phone.path().join(loquax_android::session::SERVER_FILE);
    let first = Start {
        dir: dir.clone(),
        server: None,
        code: Some(code.clone()),
        name: Some("Pixel".to_string()),
        offset: 3600,
        ..Start::default()
    };
    let peer = loquax::address::to_hex(&[0x5a; 16]);
    // A device that began before the ugly store's history (1448): its
    // people count as met here, and the banner asks as before. The second
    // start below is the same phone begun after it, which asks nothing.
    std::fs::write(
        phone.path().join(loquax_android::links::BOOK_FILE),
        "since 1\n",
    )
    .expect("the book's beginning");
    let (address, peer) = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        ugly_in(&session);
        // A new conversation's contact list (1448, walk finding 17): the
        // people by name, ignoring case, the archived one too and the
        // blocked one not; the search finds by name.
        let people = |search: Option<&str>| -> schema::Conversations {
            rmp_serde::from_slice(
                &session
                    .query(schema::Query::People {
                        search: search.map(str::to_string),
                    })
                    .expect("people"),
            )
            .expect("decode")
        };
        let all = people(None);
        let names: Vec<String> = all
            .conversations
            .iter()
            .map(|r| r.name.to_lowercase())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "by name");
        let has = |rows: &schema::Conversations, i: usize| {
            rows.conversations
                .iter()
                .any(|r| r.peer == ugly_store::peer_hex(i))
        };
        assert!(has(&all, ugly_store::ARCHIVED), "the archived one");
        assert!(!has(&all, ugly_store::BLOCKED), "not the blocked one");
        let first_name = all.conversations[0].name.clone();
        let found = people(Some(&first_name));
        assert!(
            !found.conversations.is_empty()
                && found.conversations.len() < all.conversations.len()
                && found.conversations.iter().any(|r| r.name == first_name),
            "{first_name}: {:?}",
            found
                .conversations
                .iter()
                .map(|r| &r.name)
                .collect::<Vec<_>>()
        );
        // A peer that wrote to us and is in no address book contact: its
        // chat asks to save it to the phone's contacts. Added by its code,
        // the same chat no longer asks while this session runs (walk
        // finding 15): the user just added it.
        let wrote = ugly_store::peer_hex(3);
        let asks = |session: &Session| -> bool {
            let messages: schema::Messages = rmp_serde::from_slice(
                &session
                    .query(schema::Query::Messages {
                        peer: wrote.clone(),
                    })
                    .expect("messages"),
            )
            .expect("decode");
            messages.ask_link
        };
        assert!(
            asks(&session),
            "a peer that wrote, not in the book: the banner"
        );
        session
            .request(Request::AddContact {
                lxma: wrote.clone(),
                name: None,
            })
            .expect("added by address");
        assert!(!asks(&session), "just added: no banner");
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
        assert!(
            info.advanced.iter().any(|r| r.contains(&reply.id)),
            "{info:?}"
        );
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
        // A peer without a name: the avatar shows `?`, never its address
        // (1452), in the app's colour since it sent no icon appearance (1129).
        assert_eq!(messages.initials, "?");
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
            schema::Query::Conversations { search: None },
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
        assert_eq!(row.last, "You reacted \u{1F44D}");
        assert!(row.ours);
        assert_eq!(row.initials, "?", "no hex on a nameless avatar (1452)");
        // The row's mark (1130): ours is last, so a delivery word, a text.
        assert_eq!(row.kind, "reaction");
        assert!(!row.state.is_empty());
        // The own avatar: the own name's initials, or `?`, never hex (1452).
        assert!(
            !list.initials.is_empty() && !list.address.to_uppercase().starts_with(&list.initials),
            "{}",
            list.initials
        );
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
        // The petname first, and the ugly store's other "Mum" gives both
        // their hash tail and the header its sentence (1452).
        assert_eq!(shown.name, format!("Mum ·{short}"), "the petname first");
        assert_eq!(shown.same_name, "Two contacts are called Mum");
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
        let list: schema::Conversations = rmp_serde::from_slice(
            &session
                .query(schema::Query::Conversations { search: None })
                .expect("list"),
        )
        .expect("decode");
        let row = list
            .conversations
            .iter()
            .find(|row| row.peer == peer)
            .expect("listed");
        // The name alone while nobody shares it (1452).
        assert_eq!(row.name, "Alice Schmidt");
        assert!(row.same_name.is_empty());
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
        let row = list(&session, schema::Query::Conversations { search: None });
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
        let main = list(&session, schema::Query::Conversations { search: None });
        assert!(main.conversations.iter().all(|r| r.peer != peer));
        // Beside the ugly store's archived one.
        assert_eq!(main.archived, 2);
        let archived = list(&session, schema::Query::Archived { search: None });
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
        let main = list(&session, schema::Query::Conversations { search: None });
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
        let main = list(&session, schema::Query::Conversations { search: None });
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

    // All three kept, in the code's order.
    let kept = std::fs::read_to_string(&saved).expect("the servers kept");
    assert_eq!(
        kept.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>(),
        servers.iter().map(String::as_str).collect::<Vec<_>>(),
        "{kept}"
    );

    // The book's beginning moved to now, as on a device that joined after
    // the history it pulled (1448, walk finding 12).
    let book = phone.path().join(loquax_android::links::BOOK_FILE);
    let kept = std::fs::read_to_string(&book).expect("the book");
    assert!(kept.contains("since 1\n"), "{kept}");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock")
        .as_secs();
    std::fs::write(&book, kept.replace("since 1\n", &format!("since {now}\n")))
        .expect("the book begun now");

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
        // The peer the first session offered for the phone's contacts
        // wrote only in the history from before the device began: no
        // banner (1448). Control: the first session, begun before it, asked.
        let history: schema::Messages = rmp_serde::from_slice(
            &session
                .query(schema::Query::Messages {
                    peer: ugly_store::peer_hex(3),
                })
                .expect("query"),
        )
        .expect("decode");
        assert!(!history.messages.is_empty(), "the history is there");
        assert!(!history.ask_link, "a pulled history asks nothing");
        // The link outlived the session; unlinked, nameless again ("Unknown",
        // never the address, 1452) and no photo kept.
        assert_eq!(messages.name, "Alice Schmidt");
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
        assert!(
            matches!(
                unlinked.name.split(" ·").next(),
                Some("Unknown" | "Unbekannt")
            ),
            "{}",
            unlinked.name
        );
        assert!(!unlinked.name.contains(&peer[..8]), "{}", unlinked.name);
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
        assert_eq!(
            refused.text, "This phone already reads another mailbox.",
            "{refused:?}"
        );
    })
    .await
    .expect("the second session");

    node.kill().await;
}

/// 1614, 1635: a rename the reader takes back just before the app stops
/// stays taken back, and reaches the home all the same. The forge gate on
/// 0a67952c saw "Mum ·5a5a ≈" on the second start of
/// `a_phone_joins_sends_pulls_and_comes_back`: the clear was handed to the
/// home on a task the stop dropped, and the second start's pull took the
/// home's "Mum" again. Here the stop follows the clear at once. The clear
/// is owed until the home takes it (1635): kept in the store over the
/// stop, the reader's word over the second start's pull, and handed by
/// the second session.
#[tokio::test(flavor = "multi_thread")]
async fn a_rename_taken_back_before_the_stop_stays_taken_back() {
    let instance = instance();
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
        code: Some(code),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    let peer = loquax::address::to_hex(&[0x5a; 16]);
    let peer_again = peer.clone();
    let home_petnames = move |address: [u8; 16]| -> Vec<([u8; 16], Option<String>)> {
        let store = loquax::store::Store::open_read_only(
            &home_dir.join(loquax::store::STORE_FILE),
            address,
        )
        .expect("the home store")
        .expect("there");
        store
            .archive_since(0, usize::MAX)
            .expect("page")
            .entries
            .iter()
            .filter(|e| e.kind == loquax::home::KIND_PETNAME)
            .map(|e| loquax::home::wire::read_petname_entry(&e.packed).expect("petname"))
            .filter(|(peer, _)| *peer == [0x5a; 16])
            .collect()
    };
    let home_petnames_again = home_petnames.clone();
    let address = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        let address = session.address();
        session
            .request(Request::Rename {
                peer: peer.clone(),
                name: "Mum".to_string(),
            })
            .expect("rename");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while home_petnames(address).is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "no petname reached home"
            );
            let _ = session.next_event(Duration::from_millis(200));
        }
        session
            .request(Request::Rename {
                peer: peer.clone(),
                name: String::new(),
            })
            .expect("clear");
        session.stop();
        address
    })
    .await
    .expect("the first session");

    let again = Start {
        dir,
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&again).unwrap_or_else(|error| panic!("{error:?}"));
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
        let messages: schema::Messages = rmp_serde::from_slice(
            &session
                .query(schema::Query::Messages {
                    peer: peer_again.clone(),
                })
                .expect("query"),
        )
        .expect("decode");
        // The peer's own name, "Unknown" while it has none (9dd62f9c),
        // never the cleared petname.
        assert!(!messages.name.contains("Mum"), "{}", messages.name);
        assert_eq!(messages.petname, None);
        // The owed clear reaches the home from the second session.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while home_petnames_again(address) != [([0x5a; 16], None)] {
            assert!(
                std::time::Instant::now() < deadline,
                "the owed clear never reached the home: {:?}",
                home_petnames_again(address)
            );
            let _ = session.next_event(Duration::from_millis(200));
        }
        session.stop();
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
                    .query(schema::Query::Conversations { search: None })
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
        let list: schema::Conversations = rmp_serde::from_slice(
            &session
                .query(schema::Query::Conversations { search: None })
                .expect("list"),
        )
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
            schema::Query::Conversations { search: None },
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
            schema::Query::Conversations { search: None },
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
            assert!(detail.advanced.contains(&id_row), "{:?}", detail.advanced);
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
                    detail.advanced.contains(&format!("id     {}", row.id)),
                    "{i}: {:?}",
                    detail.advanced
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
                rmp_serde::from_slice(&ask(schema::Query::Conversations { search: None }))
                    .expect("a list");
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

/// A request map of `t` with a peer and a boolean `on`, as `MsgPack.pack`
/// writes a Java `boolean` (`c3` and `c2`).
fn java_bool_map(t: &str, peer: &str, on: bool) -> Vec<u8> {
    java_map(&[
        ("t", Java::Str(t)),
        ("peer", Java::Str(peer)),
        ("on", Java::Bool(on)),
    ])
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
            schema::Query::Conversations { search: None },
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

/// The petname entries the home node of `home` keeps for `peer`, oldest
/// first, and their sequences.
fn home_petnames(home: &Path, address: [u8; 16], peer: [u8; 16]) -> Vec<(u64, Option<String>)> {
    let store =
        loquax::store::Store::open_read_only(&home.join(loquax::store::STORE_FILE), address)
            .expect("the home store")
            .expect("there");
    let page = store.archive_since(0, usize::MAX).expect("page");
    page.entries
        .iter()
        .filter(|e| e.kind == loquax::home::KIND_PETNAME)
        .map(|e| {
            let (about, name) = loquax::home::wire::read_petname_entry(&e.packed).expect("petname");
            (about, (e.seq, name))
        })
        .filter(|(about, _)| *about == peer)
        .map(|(_, entry)| entry)
        .collect()
}

/// Wait until the home node of `home` keeps `want` as `peer`'s petname,
/// reading `session`'s events meanwhile; the entry's sequence.
fn home_keeps_petname(session: &Session, home: &Path, peer: [u8; 16], want: Option<&str>) -> u64 {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let kept = home_petnames(home, session.address(), peer);
        if let Some((seq, name)) = kept.last() {
            if name.as_deref() == want {
                return *seq;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the home never kept {want:?}: {kept:?}"
        );
        let _ = session.next_event(Duration::from_millis(200));
    }
}

/// 1635: the reader names a peer "Mum", the home node keeps it, and the
/// reader clears the name again. `taken` says whether the home node took
/// the clearing before the session stopped; without it the home is
/// killed first, as a phone that stops before its hand of the clearing
/// ended (the forge's pipeline #620). The phone's pull cursor is set back
/// before the "Mum" entry, as on a phone whose pull had not reached it,
/// and the home node comes back. The second start must show no petname,
/// and the home must come to keep the clearing.
async fn a_cleared_petname_across_a_restart(taken: bool) {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let home_dir = home.path().to_path_buf();
    let phone = tempfile::tempdir().expect("app dir");
    let dir = phone.path().display().to_string();
    let first = Start {
        dir: dir.clone(),
        code: Some(format!("{code}@127.0.0.1:{port}")),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    let peer = [0x5a; 16];
    let hex = loquax::address::to_hex(&peer);
    let runtime = tokio::runtime::Handle::current();
    let (to, at) = (hex.clone(), home_dir.clone());
    let mut node = Some(node);
    let mum = tokio::task::spawn_blocking(move || {
        let mut kill = || {
            if let Some(node) = node.take() {
                runtime.block_on(node.kill());
            }
        };
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        session
            .request(Request::Rename {
                peer: to.clone(),
                name: "Mum".to_string(),
            })
            .expect("rename");
        let mum = home_keeps_petname(&session, &at, peer, Some("Mum"));
        if !taken {
            kill();
        }
        session
            .request(Request::Rename {
                peer: to.clone(),
                name: String::new(),
            })
            .expect("clear");
        if taken {
            home_keeps_petname(&session, &at, peer, None);
            kill();
        }
        let address = session.address();
        session.stop();
        (mum, address)
    })
    .await
    .expect("the first session");
    let (mum, address) = mum;
    // What the phone owes its home: the clearing, unless the home took it.
    let owed = loquax::store::Store::open_read_only(
        &phone.path().join(loquax::store::STORE_FILE),
        address,
    )
    .expect("the phone's store")
    .expect("there")
    .owed_petnames()
    .expect("owed");
    let want = match taken {
        true => std::collections::BTreeMap::new(),
        false => std::collections::BTreeMap::from([(peer, None)]),
    };
    assert_eq!(owed, want, "what the phone owes its home");

    // The cursor before the "Mum" entry; a phone that never ended a pull
    // has none, which is before every entry.
    let cursors = files_named(phone.path(), loquax::home::client::CURSOR_FILE);
    assert!(cursors.len() <= 1, "{cursors:?}");
    for cursor in cursors {
        std::fs::write(&cursor, format!("{}\n", mum - 1)).expect("the cursor set back");
    }

    let listener =
        std::net::TcpListener::bind(format!("127.0.0.1:{port}")).expect("the same port again");
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let again = Start {
        dir,
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&again).unwrap_or_else(|error| panic!("{error:?}"));
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
        let shown: schema::Detail = rmp_serde::from_slice(
            &session
                .query(schema::Query::Detail { peer: hex.clone() })
                .expect("detail"),
        )
        .expect("decode");
        assert_eq!(shown.petname, None, "the cleared petname came back");
        // The home node comes to keep the clearing, and the phone still
        // shows none once its echo is pulled.
        home_keeps_petname(&session, &home_dir, peer, None);
        session.request(Request::Pull).expect("pull");
        let shown: schema::Detail = wait_for(
            &session,
            schema::Query::Detail { peer: hex.clone() },
            5,
            |d: &schema::Detail| d.petname.is_none(),
        );
        assert_eq!(shown.petname, None);
        session.stop();
    })
    .await
    .expect("the second session");
    node.stop().await;
}

/// 1635: a petname cleared while the home node could not take it stays
/// cleared after a restart, and reaches the home node then.
#[tokio::test(flavor = "multi_thread")]
async fn a_cleared_petname_the_home_never_took_stays_cleared_after_a_restart() {
    a_cleared_petname_across_a_restart(false).await;
}

/// 1635's control: the home node took the clearing before the stop.
#[tokio::test(flavor = "multi_thread")]
async fn a_cleared_petname_the_home_took_stays_cleared_after_a_restart() {
    a_cleared_petname_across_a_restart(true).await;
}

/// The peer state entries the home node of `home` keeps for `peer`,
/// oldest first, and their sequences (1640).
fn home_peer_states(
    home: &Path,
    address: [u8; 16],
    peer: [u8; 16],
) -> Vec<(u64, loquax::home::wire::PeerState)> {
    let store =
        loquax::store::Store::open_read_only(&home.join(loquax::store::STORE_FILE), address)
            .expect("the home store")
            .expect("there");
    let page = store.archive_since(0, usize::MAX).expect("page");
    page.entries
        .iter()
        .filter(|e| e.kind == loquax::home::KIND_PEERSTATE)
        .map(|e| {
            let (about, state) =
                loquax::home::wire::read_peer_state_entry(&e.packed).expect("peer state");
            (about, (e.seq, state))
        })
        .filter(|(about, _)| *about == peer)
        .map(|(_, entry)| entry)
        .collect()
}

/// Wait until the home node of `home` keeps `peer` pinned or not as
/// `pinned` says, reading `session`'s events meanwhile; the entry's
/// sequence (1640).
fn home_keeps_pin(session: &Session, home: &Path, peer: [u8; 16], pinned: bool) -> u64 {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let kept = home_peer_states(home, session.address(), peer);
        if let Some((seq, state)) = kept.last() {
            if state.pinned == pinned {
                return *seq;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the home never kept pinned={pinned}: {kept:?}"
        );
        let _ = session.next_event(Duration::from_millis(200));
    }
}

/// 1640, the shape of 1635's petname test for a peer state: the reader
/// pins a peer, the home node keeps it, and the reader loosens the pin
/// again. `taken` says whether the home node took the loosening before
/// the session stopped; without it the home is killed first. The phone's
/// pull cursor is set back before the pinned entry and the home node comes
/// back. The second start must show the peer unpinned, and the home must
/// come to keep it so.
async fn an_unpinned_peer_across_a_restart(taken: bool) {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let home_dir = home.path().to_path_buf();
    let phone = tempfile::tempdir().expect("app dir");
    let dir = phone.path().display().to_string();
    let first = Start {
        dir: dir.clone(),
        code: Some(format!("{code}@127.0.0.1:{port}")),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    let peer = [0x5c; 16];
    let hex = loquax::address::to_hex(&peer);
    let runtime = tokio::runtime::Handle::current();
    let (to, at) = (hex.clone(), home_dir.clone());
    let mut node = Some(node);
    let (pinned, address) = tokio::task::spawn_blocking(move || {
        let mut kill = || {
            if let Some(node) = node.take() {
                runtime.block_on(node.kill());
            }
        };
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        session
            .request(Request::Pin {
                peer: to.clone(),
                on: true,
            })
            .expect("pin");
        let pinned = home_keeps_pin(&session, &at, peer, true);
        if !taken {
            kill();
        }
        session
            .request(Request::Pin {
                peer: to.clone(),
                on: false,
            })
            .expect("unpin");
        if taken {
            home_keeps_pin(&session, &at, peer, false);
            kill();
        }
        let address = session.address();
        session.stop();
        (pinned, address)
    })
    .await
    .expect("the first session");
    // What the phone owes its home: the loosening, unless the home took it.
    let owed = loquax::store::Store::open_read_only(
        &phone.path().join(loquax::store::STORE_FILE),
        address,
    )
    .expect("the phone's store")
    .expect("there")
    .owed_peer_states()
    .expect("owed");
    let want = match taken {
        true => std::collections::BTreeMap::new(),
        false => {
            std::collections::BTreeMap::from([(peer, loquax::home::wire::PeerState::default())])
        }
    };
    assert_eq!(owed, want, "what the phone owes its home");

    // The cursor before the pinned entry.
    let cursors = files_named(phone.path(), loquax::home::client::CURSOR_FILE);
    assert!(cursors.len() <= 1, "{cursors:?}");
    for cursor in cursors {
        std::fs::write(&cursor, format!("{}\n", pinned - 1)).expect("the cursor set back");
    }

    let listener =
        std::net::TcpListener::bind(format!("127.0.0.1:{port}")).expect("the same port again");
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let again = Start {
        dir,
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&again).unwrap_or_else(|error| panic!("{error:?}"));
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
        let shown: schema::Detail = rmp_serde::from_slice(
            &session
                .query(schema::Query::Detail { peer: hex.clone() })
                .expect("detail"),
        )
        .expect("decode");
        assert!(!shown.pinned, "the loosened pin came back");
        // The home node comes to keep the loosening, and the phone still
        // shows it once its echo is pulled.
        home_keeps_pin(&session, &home_dir, peer, false);
        session.request(Request::Pull).expect("pull");
        let shown: schema::Detail = wait_for(
            &session,
            schema::Query::Detail { peer: hex.clone() },
            5,
            |d: &schema::Detail| !d.pinned,
        );
        assert!(!shown.pinned);
        session.stop();
    })
    .await
    .expect("the second session");
    node.stop().await;
}

/// 1640: a pin loosened while the home node could not take it stays
/// loosened after a restart, and reaches the home node then.
#[tokio::test(flavor = "multi_thread")]
async fn an_unpinned_peer_the_home_never_took_stays_unpinned_after_a_restart() {
    an_unpinned_peer_across_a_restart(false).await;
}

/// 1640's control: the home node took the loosening before the stop.
#[tokio::test(flavor = "multi_thread")]
async fn an_unpinned_peer_the_home_took_stays_unpinned_after_a_restart() {
    an_unpinned_peer_across_a_restart(true).await;
}

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
    /// The daemon's stop: dropping it stops the daemon too.
    stop: tokio::sync::oneshot::Sender<()>,
    /// The home's own propagation node on the transport's instance (1409).
    pn: Option<lnpnd_node::PropagationNode>,
}

impl HomeNode {
    async fn start(
        listener: std::net::TcpListener,
        instance: &str,
        rns: &Path,
        home: &Path,
    ) -> Self {
        Self::start_linked(listener, instance, rns, home, None).await
    }

    /// [`HomeNode::start`] with a TCP link to a Python daemon on
    /// `python`, so the home reaches a stock member there.
    async fn start_linked(
        listener: std::net::TcpListener,
        instance: &str,
        rns: &Path,
        home: &Path,
        python: Option<u16>,
    ) -> Self {
        Self::start_full(listener, instance, rns, home, python, None).await
    }

    /// [`HomeNode::start_linked`] with the home's own propagation node
    /// (1409): lnpnd on the transport's instance, asking stamp cost `.0`,
    /// as the daemon's `--pn`, and a sync round every `.1` ms.
    async fn start_full(
        listener: std::net::TcpListener,
        instance: &str,
        rns: &Path,
        home: &Path,
        python: Option<u16>,
        pn: Option<(u8, u64)>,
    ) -> Self {
        Self::start_capped(listener, instance, rns, home, python, pn, ugly_store::CAP).await
    }

    /// [`HomeNode::start_full`] with an archive of `cap` bytes: the big
    /// mailbox of 1446 does not fit the facade tests' 1 MiB.
    async fn start_capped(
        listener: std::net::TcpListener,
        instance: &str,
        rns: &Path,
        home: &Path,
        python: Option<u16>,
        pn: Option<(u8, u64)>,
        cap: u64,
    ) -> Self {
        let mut builder = ReticulumNodeBuilder::new()
            .enable_transport(true)
            .share_instance(true)
            .instance_name(instance.to_string())
            .add_tcp_server_listener(listener);
        if let Some(port) = python {
            let python: std::net::SocketAddr = format!("127.0.0.1:{port}")
                .parse()
                .expect("loopback address");
            builder = builder.add_tcp_client(python);
        }
        let mut transport = builder
            .storage_path(rns.join("storage"))
            .build_sync()
            .expect("the transport node");
        transport.start().await.expect("start it");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (pn, pn_setup) = match pn {
            Some((cost, sync_ms)) => {
                let node = lnpnd_node::PropagationNode::start(instance, cost).await;
                let hash = node.hash;
                (Some(node), Some((hash, sync_ms)))
            }
            None => (None, None),
        };

        let identity = loquax::identity::load_or_create(&home.join("identity")).expect("identity");
        let daemon_storage = home.join("storage");
        std::fs::create_dir_all(&daemon_storage).expect("daemon storage");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let daemon = tokio::spawn(loquax::home::daemon::run(
            loquax::home::daemon::DaemonOptions {
                home: home.to_path_buf(),
                upstream: loquax::home::daemon::DaemonUpstream::Shared {
                    instance: instance.to_string(),
                    config_dir: rns.to_path_buf(),
                    mailbox: Box::new(|| {
                        Box::pin(async { loquax::mailbox_state::MailboxRun { input: None } })
                    }),
                    rendezvous: Vec::new(),
                    tcp: Default::default(),
                },
                storage_dir: daemon_storage,
                identity,
                display_name: b"home".to_vec(),
                archive_cap_bytes: cap,
                token_ttl_secs: 3600,
                policy: loquax::home::policy(&loquax::config::Config::default()),
                preferred_pn: pn_setup.map(|(hash, _)| hash),
                pn_source: if pn_setup.is_some() { "--pn" } else { "none" },
                sync_bound_ms: pn_setup.map(|(_, sync_ms)| sync_ms),
                fetch_budget: Duration::from_secs(10),
                wake: true,
                stop: Some(stopped),
                network: Default::default(),
                lends: Vec::new(),
            },
        ));
        Self {
            transport,
            daemon,
            stop,
            pn,
        }
    }

    /// Stop both as a shutdown does: the daemon closes its node, which
    /// writes what it keeps to disk.
    async fn stop(mut self) {
        let _ = self.stop.send(());
        self.daemon
            .await
            .expect("the daemon task")
            .expect("the daemon stopped cleanly");
        self.transport
            .stop()
            .await
            .expect("stop the transport node");
    }

    /// Kill both, as a power cut does to the box they run on: the daemon
    /// gone with its lock, the TCP server closed.
    async fn kill(mut self) {
        drop(self.stop);
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
                info.advanced.iter().any(|r| r.contains(&row.id)),
                "{:?}",
                info.advanced
            );
        }
    }
}

/// 1642, the mailbox page names the mailbox, end to end: a phone joins a
/// real mailbox with a code the mailbox opened for "phone" and asks the page with the bytes Java's
/// `Core.mailboxPage` sends. Once the link is up the page names the
/// mailbox, its full address with a copy, the build it runs, the server
/// and port it is reached over, and lists the mailbox's devices with this
/// one said as such, all from the mailbox's `/about`. The mailbox killed,
/// the same page says "Not connected" and keeps what the mailbox said.
/// Both answers are printed whole (`--nocapture`), the facade dump of the
/// page reachable and unreachable.
#[tokio::test(flavor = "multi_thread")]
async fn the_mailbox_page_names_the_mailbox_and_says_when_it_went_away() {
    let _facade = facade_turn().await;
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        code: Some(format!("{code}@127.0.0.1:{port}")),
        name: Some("Pixel".to_string()),
        lang: Some("en-GB".to_string()),
        ..Start::default()
    };
    let page = || -> schema::MailboxPage {
        let request: schema::Query =
            schema::decode(&java_map(&[("t", Java::Str("mailboxpage"))])).expect("Java's bytes");
        assert_eq!(request, schema::Query::MailboxPage);
        rmp_serde::from_slice(&loquax_android::query(&schema::encode(&request)))
            .expect("a MailboxPage")
    };
    let lines = |page: &schema::MailboxPage| -> Vec<String> {
        page.groups
            .iter()
            .flat_map(|g| g.lines.iter())
            .map(|l| match &l.value {
                Some(value) => format!("{}: {value}", l.label),
                None => l.label.clone(),
            })
            .collect()
    };
    let until = move |what: &str, done: &dyn Fn(&[String]) -> bool| -> schema::MailboxPage {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let now = page();
            if done(&lines(&now)) {
                return now;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the page never said {what}: {:#?}",
                lines(&now)
            );
            let _ = loquax_android::next_event(Duration::from_millis(200));
        }
    };
    let up = tokio::task::spawn_blocking(move || {
        let answer = started(&loquax_android::start(&schema::encode(&first)));
        assert!(answer.ok, "{answer:?}");
        link_event(true, 30);
        // The connection is the watching link to the mailbox (1647).
        let reachable = until("the mailbox's devices, connected", &|l| {
            l.iter().any(|l| l == "phone: this device")
                && l.iter().any(|l| l.starts_with("This device connected"))
        });
        let said = lines(&reachable);
        // The version's words, never a commit (1647, finding 30).
        let words = loquax::VERSION
            .split(" (")
            .next()
            .expect("the words")
            .to_string();
        let version = format!("Version: {words}");
        // The machine the mailbox says it runs on, an unusual port said.
        let host = reachable.groups[0]
            .lines
            .iter()
            .find(|l| l.label == "Runs on")
            .and_then(|l| l.value.clone())
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let way = format!("Connection to {host}, port {port}");
        for want in [
            "Which mailbox",
            version.as_str(),
            way.as_str(),
            "Last contact: now",
            "This is your address too: the mailbox and you are one identity.",
        ] {
            assert!(
                said.iter().any(|l| l == want) || reachable.groups.iter().any(|g| g.title == want),
                "{want:?} in {said:#?}"
            );
        }
        assert!(
            said.iter().any(|l| l.starts_with("This device connected")),
            "{said:#?}"
        );
        let address = reachable.groups[0]
            .lines
            .iter()
            .find_map(|l| l.copy.clone())
            .expect("the address to copy");
        assert_eq!(address.len(), 32, "the address in full");
        assert!(said.contains(&format!("Address: {address}")), "{said:#?}");
        assert_eq!(reachable.copy_label, "Copy");
        let code = reachable
            .groups
            .iter()
            .flat_map(|g| g.lines.iter())
            .find(|l| l.action.as_deref() == Some("profile"))
            .expect("Show my code");
        assert_eq!(code.label, "Show my code");
        for line in &said {
            assert!(
                !line.contains(loquax::VERSION.split(['(', ')']).nth(1).unwrap_or("-")),
                "no commit on the page: {line}"
            );
        }
        println!(
            "FACADE mailboxpage reachable:\n{}",
            serde_json::to_string_pretty(&reachable).expect("json")
        );
        // 1647, walk 6, finding 28: the page asked again five seconds
        // later has the same shape (groups, lines, their kinds), so the
        // phone writes it into the lines on screen and the scroll anchor
        // stays (`MailboxLines.java`, `fling.rs`). Only words move.
        let shape = |page: &schema::MailboxPage| -> Vec<String> {
            page.groups
                .iter()
                .flat_map(|g| {
                    std::iter::once(format!("group {}", g.title)).chain(g.lines.iter().map(|l| {
                        let kind = match (l.note, l.copy.is_some(), &l.action) {
                            (true, _, _) => "note".to_string(),
                            (false, true, _) => "copy".to_string(),
                            (false, false, Some(action)) => format!("action {action}"),
                            (false, false, None) => "line".to_string(),
                        };
                        match &l.value {
                            Some(_) => format!("{kind} value"),
                            None => kind,
                        }
                    }))
                })
                .collect()
        };
        let mut asks = Vec::new();
        for _ in 0..2 {
            let start = std::time::Instant::now();
            while start.elapsed() < Duration::from_secs(5) {
                let _ = loquax_android::next_event(Duration::from_millis(200));
            }
            asks.push(page());
        }
        for (i, again) in asks.iter().enumerate() {
            assert_eq!(
                shape(again),
                shape(&reachable),
                "ask {} five seconds on changed the page's shape:\n{:#?}\nvs\n{:#?}",
                i + 1,
                lines(again),
                said
            );
        }
        println!(
            "FACADE mailboxpage anchor: shape {:?} held over two asks five seconds apart; last ask {:?}",
            shape(&reachable),
            lines(&asks[1])
        );
    });
    up.await.expect("the page while the mailbox runs");
    node.kill().await;
    tokio::task::spawn_blocking(move || {
        link_event(false, 30);
        let unreachable = until("not connected", &|l| {
            l.iter().any(|l| {
                l.starts_with("This device is not connected")
                    || l.starts_with("This device not connected")
            })
        });
        let said = lines(&unreachable);
        assert!(
            said.iter().any(|l| l == "phone: this device"),
            "what the mailbox said stays: {said:#?}"
        );
        assert!(
            said.iter().any(|l| l.starts_with("Last contact: ")),
            "{said:#?}"
        );
        println!(
            "FACADE mailboxpage unreachable:\n{}",
            serde_json::to_string_pretty(&unreachable).expect("json")
        );
        loquax_android::stop();
    })
    .await
    .expect("the page after the mailbox went");
}

/// Wait until the list's `home_link` (1199), the status dot, says `up`:
/// the watching link came up or went down.
fn facade_home_link(up: bool, secs: u64) {
    facade_list(secs, &format!("home_link {up}"), |list| {
        list.home_link == up
    });
}

/// Wait until the list's `offline` (1448), the bar "no connection", says
/// `shown`.
fn facade_offline(shown: bool, secs: u64) {
    facade_list(secs, &format!("offline {shown}"), |list| {
        list.offline == shown
    });
}

/// Wait until the list answers `what`, the pump turning meanwhile.
fn facade_list(secs: u64, what: &str, done: impl Fn(&schema::Conversations) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let list: schema::Conversations = rmp_serde::from_slice(&loquax_android::query(
            &schema::encode(&schema::Query::Conversations { search: None }),
        ))
        .expect("Conversations");
        if done(&list) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the list never said {what} in {secs} s"
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
    let _facade = facade_turn().await;
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
        // The link up: no bar (1448).
        facade_offline(false, 1);
        facade_shows_ugly(60);
        send_through_facade(&to, "vor dem Ausfall", 60);
    })
    .await
    .expect("the first session");

    node.kill().await;
    tokio::task::spawn_blocking(|| {
        link_event(false, 30);
        facade_home_link(false, 30);
        // Down for a moment the list says nothing yet; down for good, the
        // bar "no connection" (1448, walk finding 14).
        let list: schema::Conversations = rmp_serde::from_slice(&loquax_android::query(
            &schema::encode(&schema::Query::Conversations { search: None }),
        ))
        .expect("Conversations");
        assert!(!list.offline, "the link went down just now");
        facade_offline(true, 30);
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
        // Up again: the bar goes with the dot's turn.
        facade_offline(false, 5);
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

/// A network change through the facade (1298): Java's `Core.network` with
/// another network drops the session's connection to the mailbox and dials
/// it at once; the line says "Reconnecting…" (`Event::Network`
/// `reconnecting`), then it is gone (`back`), the link is up again and a
/// send reaches the mailbox, with no restart and no user action. A burst of
/// three words is one reconnect; the same network with the link up is none.
#[tokio::test(flavor = "multi_thread")]
async fn a_network_change_reconnects_the_phone_without_a_restart() {
    let _facade = facade_turn().await;
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        code: Some(format!("{code}@127.0.0.1:{port}")),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    let peer = loquax::address::to_hex(&[0x6e; 16]);
    tokio::task::spawn_blocking(move || {
        let answer = started(&loquax_android::start(&schema::encode(&first)));
        assert!(answer.ok, "{answer:?}");
        link_event(true, 30);
        facade_home_link(true, 30);
        let network = |kind: &str, changed: bool| {
            let done: schema::Done =
                rmp_serde::from_slice(&loquax_android::command(&schema::encode(
                    &Request::Network {
                        kind: kind.to_string(),
                        changed,
                    },
                )))
                .expect("a Done");
            assert!(done.ok, "{done:?}");
        };
        let state = |want: &'static str| {
            move |event: &Event| matches!(event, Event::Network { state, .. } if state == want)
        };

        // WLAN to mobile data, as the platform says it: lost, available,
        // its capabilities.
        network("none", true);
        network("mobile", true);
        network("mobile", false);
        let began = std::time::Instant::now();
        facade_event(10, "reconnecting", state("reconnecting"));
        facade_event(20, "back", state("back"));
        let took = began.elapsed();
        // The interface is up again: its level, not a `link` event. The
        // session reads the level once a second, and a re-dial on loopback
        // is up within a millisecond, so it may never read it down and has
        // no edge to say (1334).
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !started(&loquax_android::start(&schema::encode(&first))).link {
            assert!(
                std::time::Instant::now() < deadline,
                "the interface not up again in 10 s"
            );
            let _ = loquax_android::next_event(Duration::from_millis(200));
        }
        facade_home_link(true, 60);
        send_through_facade(&peer, "nach dem Netzwechsel", 60);
        // One reconnect for the burst: no second line within the settle.
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        while std::time::Instant::now() < deadline {
            if let Some(bytes) = loquax_android::next_event(Duration::from_millis(200)) {
                let event: Event = rmp_serde::from_slice(&bytes).expect("an Event");
                assert!(
                    !matches!(event, Event::Network { .. }),
                    "a second reconnect for one burst: {event:?}"
                );
            }
        }
        // Asleep and back on the same network, the link up: kept.
        network("mobile", false);
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        while std::time::Instant::now() < deadline {
            if let Some(bytes) = loquax_android::next_event(Duration::from_millis(200)) {
                let event: Event = rmp_serde::from_slice(&bytes).expect("an Event");
                assert!(
                    !matches!(event, Event::Network { .. } | Event::Link { up: false, .. }),
                    "the same network dropped an up link: {event:?}"
                );
            }
        }
        eprintln!("NETWORK change to back: {} ms", took.as_millis());
        loquax_android::stop();
    })
    .await
    .expect("the session");
    node.kill().await;
}

/// The phone never freezes after joining (1446). UI walk 1 on the Pixel 3a
/// XL: right after the join, `ConversationsActivity.onResume` called
/// `Core.close()` on the UI thread, and that call waited on the session's
/// lock while the pump held it to fold the first pull of a full mailbox,
/// one store commit per message: Android said "Loquax reagiert nicht".
/// Here a mailbox of [`BIG_CONVERSATIONS`] conversations, thousands of
/// entries, as Lew's holds, on disk (the store's fsyncs are what the
/// pump's turn waits on; a tmpfs would hide them). The phone joins; while
/// the first pull runs, two probes measure each block. The engine's
/// thread does what `onResume` hands it (close, front) every 50 ms, as
/// `Engine.run` would; the UI thread asks the conversation list every
/// 16 ms, as `ConversationsActivity.ask` does once per frame (1464: the
/// Pixel spent 83.6 % of 44 s in that query, 79 to 100 ms a call while
/// the pull ran). The list's work must stay under [`UI_BAR`], a few
/// frames, so no tap is lost; the engine's under [`ENGINE_BAR`]; and
/// neither may wait for the lock behind more than one event a wait.
/// Measured as cost, not wall time, so a slow runner does not fail it
/// ([`Block`], 1610). The history must arrive whole.
#[tokio::test(flavor = "multi_thread")]
async fn the_first_pull_of_a_big_mailbox_never_blocks_the_screen() {
    let _facade = facade_turn().await;
    let instance = instance();
    let scratch = env!("CARGO_TARGET_TMPDIR");
    let rns = tempfile::tempdir_in(scratch).expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir_in(scratch).expect("home node dir");
    let code = admit(home.path());
    let filled = ugly_store::fill_archive_of(home.path(), BIG_CONVERSATIONS, BIG_CAP);
    assert_eq!(
        filled.messages,
        BIG_CONVERSATIONS * ugly_store::PER_CONVERSATION
    );
    let node = HomeNode::start_capped(
        listener,
        &instance,
        rns.path(),
        home.path(),
        None,
        None,
        BIG_CAP,
    )
    .await;

    let phone = tempfile::tempdir_in(scratch).expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        code: Some(format!("{code}@127.0.0.1:{port}")),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let joined = std::time::Instant::now();
        let answer = started(&loquax_android::start(&schema::encode(&first)));
        assert!(answer.ok, "{answer:?}");
        eprintln!("BIGPULL join: {} ms", joined.elapsed().as_millis());

        // The engine's thread with onResume's commands, every 50 ms, and
        // the UI thread with the list, once a frame, while the pull runs.
        // Each block is kept as what it cost, not how long it took: the
        // probe thread's own CPU, and the engine's events the pump folded
        // while it waited for the session's lock ([`Block`]).
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probing = std::sync::Arc::clone(&done);
        let probe = std::thread::spawn(move || {
            let close = schema::encode(&Request::Close);
            let front = schema::encode(&Request::Front { front: true });
            let mut blocks = Vec::new();
            while !probing.load(std::sync::atomic::Ordering::Relaxed) {
                blocks.push(Block::of(|| {
                    let _ = loquax_android::command(&close);
                    let _ = loquax_android::command(&front);
                }));
                std::thread::sleep(Duration::from_millis(50));
            }
            blocks
        });
        let looking = std::sync::Arc::clone(&done);
        let look = std::thread::spawn(move || {
            let list = schema::encode(&schema::Query::Conversations { search: None });
            let mut blocks = Vec::new();
            let mut rows = 0;
            while !looking.load(std::sync::atomic::Ordering::Relaxed) {
                let mut answer = Vec::new();
                blocks.push(Block::of(|| answer = loquax_android::query(&list)));
                let answer: schema::Conversations =
                    rmp_serde::from_slice(&answer).expect("the list");
                rows = rows.max(answer.conversations.len());
                std::thread::sleep(Duration::from_millis(16));
            }
            (blocks, rows)
        });

        // The history whole: every conversation but the archived and the
        // blocked one listed, each of them with every bubble.
        let pulled = std::time::Instant::now();
        let listed = BIG_CONVERSATIONS - 2;
        let deadline = std::time::Instant::now() + Duration::from_secs(BIG_PULL_SECS);
        let mut whole = false;
        while std::time::Instant::now() < deadline {
            // The check reads all hundred conversations under the lock: four
            // times a second, not once per event, or the harness's own
            // reads are the probes' longest waits (1464).
            std::thread::sleep(Duration::from_millis(250));
            for _ in 0..10_000 {
                if loquax_android::next_event(Duration::ZERO).is_none() {
                    break;
                }
            }
            let list: schema::Conversations = rmp_serde::from_slice(&loquax_android::query(
                &schema::encode(&schema::Query::Conversations { search: None }),
            ))
            .expect("the list");
            if list.conversations.len() < listed {
                continue;
            }
            whole = (0..BIG_CONVERSATIONS)
                .filter(|i| *i != ugly_store::BLOCKED)
                .all(|i| {
                    let messages: schema::Messages = rmp_serde::from_slice(&loquax_android::query(
                        &schema::encode(&schema::Query::Messages {
                            peer: ugly_store::peer_hex(i),
                        }),
                    ))
                    .expect("the messages");
                    messages.messages.len() == ugly_store::PER_CONVERSATION - ugly_store::REACTIONS
                });
            if whole {
                break;
            }
        }
        let took = pulled.elapsed();
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        let blocks = probe.join().expect("the probe");
        let (looks, rows) = look.join().expect("the list's probe");
        let engine = Block::worst(&blocks);
        let ui = Block::worst(&looks);
        eprintln!(
            "BIGPULL whole={whole} after {} ms; onResume probes={} longest={} ms cpu={} ms \
             waited behind {} events in {} locks; list probes={} longest={} ms cpu={} ms \
             waited behind {} events in {} locks rows={rows}",
            took.as_millis(),
            blocks.len(),
            engine.wall.as_millis(),
            engine.cpu.as_millis(),
            engine.folds,
            engine.locks,
            looks.len(),
            ui.wall.as_millis(),
            ui.cpu.as_millis(),
            ui.folds,
            ui.locks,
        );
        loquax_android::stop();
        assert!(whole, "the history not whole in {BIG_PULL_SECS} s");
        assert!(
            ui.cpu < UI_BAR,
            "the list's query worked {} ms on the UI thread during the first pull (bar {} ms)",
            ui.cpu.as_millis(),
            UI_BAR.as_millis()
        );
        assert!(
            ui.folds <= ui.locks,
            "the list's query waited for the lock while the pump folded {} events, in {} \
             waits (bar: one a wait)",
            ui.folds,
            ui.locks
        );
        assert!(
            engine.cpu < ENGINE_BAR,
            "onResume's commands worked {} ms on the engine's thread during the first pull \
             (bar {} ms)",
            engine.cpu.as_millis(),
            ENGINE_BAR.as_millis()
        );
        assert!(
            engine.folds <= engine.locks,
            "onResume's commands waited for the lock while the pump folded {} events, in {} \
             waits (bar: one a wait)",
            engine.folds,
            engine.locks
        );
    })
    .await
    .expect("the session");
    node.kill().await;
}

/// The big mailbox of 1446: 100 conversations of 40, 4000 entries.
const BIG_CONVERSATIONS: usize = 100;
/// Its archive cap: room for all of it.
const BIG_CAP: u64 = 64 << 20;
/// How long its first pull may take: a liveness bound, nothing measures
/// it. The pull took 16 to 24 s on the coder host, 65 to 76 s under a
/// quarter of a CPU and 187 to 199 s under 15 % (1610).
const BIG_PULL_SECS: u64 = 600;
/// The list query's work on the UI thread (1464): 100 ms, a few frames.
/// 1446's 1 s kept Android's 5 s input limit away, but the Pixel still
/// froze 35 s in blocks of 80 to 100 ms, one per event, and lost taps.
/// The query reads the scene the pump keeps; on the host it costs 1 to
/// 11 ms of CPU under any quota.
const UI_BAR: Duration = Duration::from_millis(100);
/// The engine thread's work for onResume's two commands (1464): each makes
/// a store commit of its own and the scene of the lists. 13 to 33 ms of
/// CPU on the host, unconstrained and under 25 % and 15 % of a CPU alike.
const ENGINE_BAR: Duration = Duration::from_millis(250);
/// What one probe's call cost (1610). The forge runner is shared and slow:
/// pipeline 617 saw onResume's commands take 438 ms there, 255 to 350 ms
/// under a quarter of a CPU here and up to 1151 ms under 15 %, while the
/// same calls cost the same 13 to 33 ms of CPU as on an idle host. The rest
/// is the host's run queue, which no change to the app shortens, so the
/// bars hold the cost: the calling thread's own CPU time, and the waits
/// for the session's lock counted in the pump's own unit, the events it
/// folded while the call waited, whatever a fold takes on the host. The
/// pump ends its turn after the event in hand once a caller waits, and
/// starts none while one does, so each wait is behind one event at most.
/// A pump that ignores the waiting caller folds its whole 50 ms slice,
/// 25 to 65 events, and one that holds the lock for the whole backlog
/// (1446) thousands. The wall time is kept for the log.
#[derive(Clone, Copy, Debug, Default)]
struct Block {
    wall: Duration,
    cpu: Duration,
    /// Waits for the session's lock.
    locks: u64,
    /// The engine's events the pump folded while they waited.
    folds: u64,
}

impl Block {
    /// Run `call` on this thread and say what it cost.
    fn of(call: impl FnOnce()) -> Self {
        let cpu = thread_cpu();
        let (locks, folds) = loquax_android::lock_waits();
        let began = std::time::Instant::now();
        call();
        let wall = began.elapsed();
        let (locks_after, folds_after) = loquax_android::lock_waits();
        Self {
            wall,
            cpu: thread_cpu().saturating_sub(cpu),
            locks: locks_after - locks,
            folds: folds_after - folds,
        }
    }

    /// The worst of `blocks`: the longest wall and CPU time, and the
    /// block that waited behind the most events beyond one a wait.
    fn worst(blocks: &[Block]) -> Self {
        let over = |b: &Block| b.folds.saturating_sub(b.locks);
        let waits = blocks
            .iter()
            .max_by_key(|b| (over(b), b.folds))
            .copied()
            .unwrap_or_default();
        blocks.iter().fold(waits, |w, b| Self {
            wall: w.wall.max(b.wall),
            cpu: w.cpu.max(b.cpu),
            ..w
        })
    }
}

/// The calling thread's CPU time so far: the first field of its
/// `schedstat`, nanoseconds on the CPU.
fn thread_cpu() -> Duration {
    let stat =
        std::fs::read_to_string("/proc/thread-self/schedstat").expect("this thread's schedstat");
    let ns = stat
        .split_whitespace()
        .next()
        .and_then(|ns| ns.parse().ok())
        .expect("its CPU time");
    Duration::from_nanos(ns)
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
                    .query(schema::Query::Conversations { search: None })
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

/// A session dropped on a runtime's own thread, as a test's async body
/// drops it when an assertion unwinds (1274): it stops its mailbox and its
/// runtime without blocking that thread. Before, `Session::stop` ran
/// `block_on` there, tokio panicked inside the unwinding, and the process
/// aborted with the assertion's message buried. Its mailbox lets go of the
/// directory: a second session starts on it.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_dropped_on_a_runtime_thread_stops_its_mailbox() {
    let phone = tempfile::tempdir().expect("app dir");
    let mailbox = phone.path().join(loquax_android::mailbox::DIR);
    std::fs::create_dir_all(&mailbox).expect("mailbox dir");
    // An entry nobody answers: the mailbox runs without one.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free port")
        .port();
    std::fs::write(
        mailbox.join(loquax_android::mailbox::SERVERS_FILE),
        format!("127.0.0.1:{port}\n"),
    )
    .expect("servers file");
    let start = Start {
        dir: phone.path().display().to_string(),
        here: true,
        ..Start::default()
    };
    let again = start.clone();
    let session = tokio::task::spawn_blocking(move || {
        Session::start(&start).unwrap_or_else(|error| panic!("{error:?}"))
    })
    .await
    .expect("the session");
    assert!(session.is_here());
    let address = session.address();
    drop(session);
    let session = tokio::task::spawn_blocking(move || {
        Session::start(&again).unwrap_or_else(|error| panic!("{error:?}"))
    })
    .await
    .expect("the second session");
    assert_eq!(session.address(), address);
    tokio::task::spawn_blocking(move || session.stop())
        .await
        .expect("stopped");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lew's phone, 2026-10-31 (1184): Columba's report a minute was a bubble,
/// an unread count and a notification each. Over the ugly store with a run
/// of 14 reports between two texts, the facade answers three rows, the
/// run's with its count and when it began, and counts it unread not at all
/// (1209, it counted once at 1184); the
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

    let rows = session::rows(&model, c, Lang::En);
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
    // The texts count, the run not (1209).
    assert_eq!(loquax::ui::chat::list_row(&model, &peer).unread, 2);

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
    // never (1209); the next report updates that row.
    let report = |id: u8, lat: i32| {
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [id; 32],
            source: peer,
            title: Vec::new(),
            body: Vec::new(),
            fields: ugly_store::position(ugly_store::BASE + 2 * 86_400, lat),
            verified: true,
        })
    };
    update(&mut model, report(0xe1, 52_600_000));
    let rows = session::rows(&model, c, Lang::En);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[3].updates, 0, "one report is no run yet");
    assert_eq!(loquax::ui::chat::list_row(&model, &peer).unread, 2);
    update(&mut model, report(0xe2, 52_600_001));
    let rows = session::rows(&model, c, Lang::En);
    assert_eq!(rows.len(), 4, "updated in place");
    assert_eq!((rows[3].updates, rows[3].lat_e6), (2, Some(52_600_001)));
    let row = loquax::ui::chat::list_row(&model, &peer);
    assert_eq!((row.unread, row.updates), (2, 2));
    // Every report is still there, and every one unread for the read marks.
    assert_eq!(model.conversations[c].messages.len(), RUN_REPORTS + 4);
    assert_eq!(model.conversations[c].unread, RUN_REPORTS + 4);
}

/// Lew, 2026-10-31 (1209): a position report is a state, not a message.
/// Over the ugly store, six reports from one peer ten minutes apart raise
/// no unread count (the phone's notification grows with it), no toast (the
/// facade's `notice`) and no move in the list; a text from the same peer
/// still does all three.
#[test]
fn a_position_report_is_a_state_not_a_message() {
    use loquax::outbox::OutboxEvent;
    use loquax::ui::chat::list_row;
    use loquax::ui::{update, UiEvent};
    use loquax::when::WallTime;
    use loquax_android::session;
    let home = tempfile::tempdir().expect("app dir");
    let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    ugly_store::fill_store_of(home.path(), &identity);
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let store = loquax::store::Store::open_in(home.path(), me).expect("the store");
    let mut model = session::known(home.path(), &store).expect("a model");
    model.home_client = true;
    model.list_apart = true;
    let clock = |at: i64| {
        UiEvent::Clock(WallTime {
            unix_secs: at,
            offset_secs: 0,
        })
    };
    let start = ugly_store::BASE + 3 * 86_400;
    update(&mut model, clock(start));
    model.restore(store.restore().expect("restore"));
    update(&mut model, UiEvent::Close);
    // A conversation in the middle of the list, read.
    let order = model.order();
    let c = order[order.len() / 2];
    let peer = model.conversations[c].peer;
    model.conversations[c].unread = 0;
    model.conversations[c].new_rule = None;
    assert_eq!(list_row(&model, &peer).unread, 0);
    let toasts = model.toasts.len();
    let received = |id: u8, body: &str, fields: Vec<_>| {
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [id; 32],
            source: peer,
            title: Vec::new(),
            body: body.as_bytes().to_vec(),
            fields,
            verified: true,
        })
    };
    let newest = start + 5 * 600;
    for i in 0..6u8 {
        let at = start + i64::from(i) * 600;
        update(&mut model, clock(at));
        update(
            &mut model,
            received(
                0xa0 + i,
                "",
                ugly_store::position(at, 52_500_000 + i32::from(i)),
            ),
        );
    }
    assert_eq!(list_row(&model, &peer).unread, 0, "a report counts nothing");
    assert_eq!(model.toasts.len(), toasts, "nor toasts: {:?}", model.toasts);
    assert_eq!(model.order(), order, "nor moves the conversation up");
    assert_eq!(
        model.conversations[c].unread, 6,
        "the read marks still name them"
    );
    assert!(
        model.conversations[c].new_rule.is_none(),
        "nor sets the new rule"
    );
    // Alive: six reports ten minutes apart are every 10 minutes, and the
    // newest is the row that says so, on the list, the bubble and the map.
    use loquax::ui::liveness::State;
    let live = list_row(&model, &peer).liveness;
    assert_eq!(
        (live.state, live.cadence_s, live.last_fix_at),
        (State::Live, Some(600), Some(newest))
    );
    assert_eq!(list_row(&model, &peer).last, "📍 live");
    let rows = session::rows(&model, c, Lang::En);
    let newest_row = rows.last().expect("the run's row");
    assert_eq!(
        (
            newest_row.liveness.as_str(),
            newest_row.cadence_s,
            newest_row.updates
        ),
        ("live", 600, 6)
    );
    assert!(
        rows[..rows.len() - 1].iter().all(|r| r.liveness.is_empty()),
        "only the newest report's row says it"
    );
    let friend = |model: &loquax::ui::UiModel<session::Line>, at: i64| {
        loquax_android::friends::friends(model, at, None, Lang::En, |_| None)
            .friends
            .into_iter()
            .find(|f| f.peer == loquax::address::to_hex(&peer))
            .map(|f| (f.liveness, f.cadence_s, f.last_fix_at))
    };
    assert_eq!(
        friend(&model, newest),
        Some(("live".to_string(), 600, newest))
    );
    // The words and the ring come said (1217): the phone computes none.
    let said = |model: &loquax::ui::UiModel<session::Line>, at: i64, lang: Lang| {
        let row = session::rows(model, c, lang).pop().expect("the run's row");
        let card = loquax_android::friends::friends(model, at, None, lang, |_| None)
            .friends
            .into_iter()
            .find(|f| f.peer == loquax::address::to_hex(&peer))
            .expect("the card");
        let list = loquax_android::session::found(model, model.order(), None)
            .into_iter()
            .find(|r| r.row.peer == peer)
            .expect("the list's row");
        // The list's ring is its map pin since 1224: the avatar has none.
        assert_eq!(session::ring(list.avatar.ring), "");
        (
            row.live,
            card.live,
            card.ring,
            list.live.map(|l| l.resolve(lang)),
            session::pin(list.position.as_ref(), lang).0,
        )
    };
    assert_eq!(
        said(&model, newest, Lang::En),
        (
            "live · every 10 min · now".to_string(),
            "live · every 10 min · now".to_string(),
            "accent".to_string(),
            Some("live".to_string()),
            "accent".to_string()
        )
    );
    assert_eq!(
        said(&model, newest, Lang::De).0,
        "live · alle 10 Min. · gerade eben"
    );
    // 25 minutes on, past twice the cadence: recent.
    update(&mut model, clock(newest + 25 * 60));
    assert_eq!(list_row(&model, &peer).liveness.state, State::Recent);
    assert_eq!(list_row(&model, &peer).last, "📍 25 min ago");
    assert_eq!(
        session::rows(&model, c, Lang::En)
            .last()
            .map(|r| r.liveness.clone()),
        Some("recent".into())
    );
    assert_eq!(
        said(&model, newest + 25 * 60, Lang::En),
        (
            "last position 25 min ago".to_string(),
            "last position 25 min ago".to_string(),
            "secondary".to_string(),
            Some("25 min ago".to_string()),
            "secondary".to_string()
        )
    );
    // 25 hours on: stale.
    update(&mut model, clock(newest + 25 * 3600));
    assert_eq!(list_row(&model, &peer).liveness.state, State::Stale);
    assert_eq!(
        friend(&model, newest + 25 * 3600).map(|f| f.0),
        Some("stale".to_string())
    );
    assert_eq!(
        model.toasts.len(),
        toasts,
        "time passing raises nothing either"
    );

    // A text from the same peer still notifies, counts and moves up.
    update(&mut model, clock(newest + 25 * 3600 + 60));
    update(&mut model, received(0xb0, "bin da", Vec::new()));
    assert_eq!(list_row(&model, &peer).unread, 1);
    let first_unpinned = model
        .order()
        .into_iter()
        .find(|&i| !model.conversations[i].pinned);
    assert_eq!(first_unpinned, Some(c), "on top, under the pinned");
    assert_eq!(model.toasts.len(), toasts + 1);
    // A text that carries a position stays a message.
    update(
        &mut model,
        received(
            0xb1,
            "hier",
            ugly_store::position(newest + 25 * 3600, 52_600_000),
        ),
    );
    assert_eq!(list_row(&model, &peer).unread, 2);
}

/// 1224: a chat whose peer has a position younger than a day carries the
/// map pin in its list row, tinted as the ring was (live in the accent,
/// recent in the secondary colour), with its content description in the
/// phone's language; at 25 hours the pin is gone, and the avatar never
/// rings in the list. Over the ugly store, whose own positions are months
/// old: no other row has a pin.
#[test]
fn a_fresh_position_puts_the_map_pin_on_the_list_row() {
    use loquax::outbox::OutboxEvent;
    use loquax::ui::{update, UiEvent};
    use loquax::when::WallTime;
    use loquax_android::session;
    let home = tempfile::tempdir().expect("app dir");
    let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    ugly_store::fill_store_of(home.path(), &identity);
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let store = loquax::store::Store::open_in(home.path(), me).expect("the store");
    let mut model = session::known(home.path(), &store).expect("a model");
    model.home_client = true;
    model.list_apart = true;
    let clock = |at: i64| {
        UiEvent::Clock(WallTime {
            unix_secs: at,
            offset_secs: 0,
        })
    };
    let start = ugly_store::BASE + 3 * 86_400;
    update(&mut model, clock(start));
    model.restore(store.restore().expect("restore"));
    update(&mut model, UiEvent::Close);
    let pins = |model: &loquax::ui::UiModel<session::Line>, lang: Lang| {
        session::found(model, model.order(), None)
            .into_iter()
            .filter_map(|row| {
                assert_eq!(session::ring(row.avatar.ring), "", "{}", row.row.name);
                row.position
                    .as_ref()
                    .map(|_| (row.row.peer, session::pin(row.position.as_ref(), lang)))
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pins(&model, Lang::En),
        Vec::new(),
        "the store's fixes are old"
    );
    let order = model.order();
    let c = order[order.len() / 2];
    let peer = model.conversations[c].peer;
    update(
        &mut model,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [0xc4; 32],
            source: peer,
            title: Vec::new(),
            body: Vec::new(),
            fields: ugly_store::position(start, 52_500_000),
            verified: true,
        }),
    );
    let name = loquax::ui::chat::list_row(&model, &peer).name;
    update(&mut model, clock(start + 120));
    assert_eq!(
        pins(&model, Lang::En),
        [(
            peer,
            (
                "accent".to_string(),
                format!("Position of {name}, 2 min ago")
            )
        )]
    );
    update(&mut model, clock(start + 23 * 3600));
    assert_eq!(
        pins(&model, Lang::En),
        [(
            peer,
            (
                "secondary".to_string(),
                format!("Position of {name}, 23 h ago")
            )
        )]
    );
    assert_eq!(
        pins(&model, Lang::De)[0].1 .1,
        format!("Position von {name}, vor 23 Std.")
    );
    update(&mut model, clock(start + 25 * 3600));
    assert_eq!(pins(&model, Lang::En), Vec::new(), "older than a day");
}

/// 1217: the phone draws the chat's grouping, its day lines and the newest
/// group from the scene and searches the list by the rule of every loquax
/// screen, so it computes none of them again. Over the ugly store each
/// bubble's placement is `chat::placements`, a day line stands above the
/// first bubble of a day in the phone's language, and only the newest
/// group is `newest`. "anna rome" finds Anna's row, whose last line says
/// Rome, as the desktop's search does; the phone's old rule, the query
/// whole as one substring, did not.
#[test]
fn the_phone_reads_grouping_days_and_search_from_the_scene() {
    use loquax::outbox::OutboxEvent;
    use loquax::ui::chat;
    use loquax::ui::{update, UiEvent};
    use loquax::when::WallTime;
    use loquax_android::session;
    let home = tempfile::tempdir().expect("app dir");
    let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    ugly_store::fill_store_of(home.path(), &identity);
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let store = loquax::store::Store::open_in(home.path(), me).expect("the store");
    let mut model = session::known(home.path(), &store).expect("a model");
    model.home_client = true;
    model.list_apart = true;
    let now = ugly_store::BASE + 3 * 86_400;
    update(
        &mut model,
        UiEvent::Clock(WallTime {
            unix_secs: now,
            offset_secs: 0,
        }),
    );
    model.restore(store.restore().expect("restore"));
    update(&mut model, UiEvent::Close);

    let mut days = 0;
    for c in 0..model.conversations.len() {
        let rows = session::rows(&model, c, Lang::De);
        let places = chat::placements(&chat::bubbles(&model, c));
        assert_eq!(rows.len(), places.len());
        let groups = loquax::ui::scene::chat(&model, c).groups;
        let newest = groups.last().map_or(0, |g| g.bubbles.len());
        for (i, (row, place)) in rows.iter().zip(&places).enumerate() {
            assert_eq!(
                (row.joins_before, row.joins_after, row.meta),
                (place.joins_before, place.joins_after, place.meta),
                "conversation {c} row {i}"
            );
            assert_eq!(!row.day.is_empty(), place.day, "conversation {c} row {i}");
            assert_eq!(
                row.newest,
                i >= rows.len() - newest,
                "conversation {c} row {i}"
            );
            if place.day {
                days += 1;
                let at = loquax::when::WallTime {
                    unix_secs: row.at,
                    offset_secs: 0,
                };
                let today = WallTime {
                    unix_secs: now,
                    offset_secs: 0,
                };
                assert_eq!(
                    row.day,
                    loquax::ui::scene::day_words(&at, Some(&today)).resolve(Lang::De)
                );
            }
        }
    }
    assert!(days > 0, "the ugly store spans days");
    let day_lines: Vec<String> = session::rows(&model, 0, Lang::De)
        .into_iter()
        .map(|r| r.day)
        .filter(|d| !d.is_empty())
        .collect();
    assert!(
        day_lines
            .iter()
            .any(|d| d == "Vorgestern" || d.contains(". ")),
        "German day lines: {day_lines:?}"
    );

    // Search: Anna, whose last line says Rome.
    let peer = ugly_store::peer(2);
    update(
        &mut model,
        UiEvent::Rename {
            peer,
            name: "Anna".to_string(),
        },
    );
    update(
        &mut model,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [0xc1; 32],
            source: peer,
            title: Vec::new(),
            body: b"Ciao from Rome".to_vec(),
            fields: Vec::new(),
            verified: true,
        }),
    );
    let finds = |query: &str| -> Vec<[u8; 16]> {
        session::found(&model, model.order(), Some(query))
            .into_iter()
            .map(|r| r.row.peer)
            .collect()
    };
    assert_eq!(finds("anna rome"), [peer]);
    assert_eq!(finds("ROME anna"), [peer], "any order, any case");
    assert!(finds("anna paris").is_empty(), "every word");
    assert_eq!(
        finds("").len(),
        model.order().len(),
        "the empty query finds all"
    );
    // The desktop's rule says the same of the same row.
    let row = chat::list_row(&model, &peer);
    assert!(chat::row_matches(&row, "anna rome"));
    // The phone's old rule did not find it: the query whole as one
    // substring of the name or the last line.
    let old = |text: &str| text.to_lowercase().contains("anna rome");
    assert!(!old(&row.name) && !old(&row.last), "{row:?}");
}

/// 1662, a file tells its progress: a scripted resource moves the
/// running send of a file to 43 % and then a fetch to 1.2 MB of 3.0 MB;
/// the facade's rows carry the bubble's words in the phone's language and
/// the bar's percent, and a sent message has neither.
#[test]
fn a_file_under_way_carries_its_words_and_bar_to_the_phone() {
    use loquax::outbox::OutboxEvent;
    use loquax::ui::transfer::{Transfer, Way};
    use loquax::ui::{update, UiEvent, UiModel};
    use loquax_android::session::{self, Line};
    let peer = ugly_store::peer(93);
    let mut model = UiModel::<Line>::new(&[peer]);
    update(
        &mut model,
        UiEvent::SendFile {
            peer,
            name: "Protokoll.pdf".into(),
            data: vec![7; 300_000],
        },
    );
    update(
        &mut model,
        UiEvent::Engine(OutboxEvent::Queued {
            message_id: [0x62; 32],
        }),
    );
    update(&mut model, UiEvent::Elapsed(1_000));
    update(
        &mut model,
        UiEvent::Engine(OutboxEvent::Progress {
            message_id: [0x62; 32],
            permille: 430,
        }),
    );
    let rows = session::rows(&model, 0, Lang::De);
    let ours = rows.iter().find(|r| r.ours).expect("our row");
    assert_eq!(ours.progress_words, "wird gesendet · 43 %");
    assert_eq!(ours.progress_percent, 43);
    assert_eq!(ours.files.len(), 1, "the file's line stays");
    // A client's upload names the running send the same way.
    update(&mut model, UiEvent::Elapsed(1_250));
    update(&mut model, UiEvent::SendProgress(870));
    let rows = session::rows(&model, 0, Lang::En);
    assert_eq!(
        rows.iter()
            .find(|r| r.ours)
            .map(|r| r.progress_words.as_str()),
        Some("sending · 87 %")
    );
    update(
        &mut model,
        UiEvent::Engine(OutboxEvent::State {
            message_id: [0x62; 32],
            state: leviculum_lxmf::router::MessageState::Delivered,
        }),
    );
    let rows = session::rows(&model, 0, Lang::De);
    let ours = rows.iter().find(|r| r.ours).expect("our row");
    assert_eq!(
        (ours.progress_words.as_str(), ours.progress_percent),
        ("", 0)
    );
    // A fetch says its bytes of the whole.
    update(
        &mut model,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [0x63; 32],
            source: peer,
            title: Vec::new(),
            body: b"das Bild".to_vec(),
            fields: Vec::new(),
            verified: true,
        }),
    );
    let key = model.conversations[0]
        .messages
        .iter()
        .find(|m| !m.outbound)
        .map(|m| m.activity)
        .expect("theirs");
    update(&mut model, UiEvent::Elapsed(2_000));
    update(
        &mut model,
        UiEvent::Transfer {
            peer,
            key,
            transfer: Transfer {
                way: Way::Fetching,
                done: 1_234_567,
                total: 3_000_000,
            },
        },
    );
    let rows = session::rows(&model, 0, Lang::De);
    let theirs = rows.iter().find(|r| !r.ours).expect("their row");
    assert_eq!(theirs.progress_words, "wird geholt · 1,2 MB von 3,0 MB");
    assert_eq!(theirs.progress_percent, 41);
}

/// 1662, a picture sent as a file shows its thumbnail: a PNG that goes
/// through the file path, ours and theirs, gives its row the photo's
/// format, its size upright and its words, as a photo's row has them, with
/// the file's line under it; the library draws its pixels. A file that
/// only calls itself `.png` is a file row alone.
#[test]
fn a_png_sent_as_a_file_shows_its_thumbnail_with_the_file_line() {
    use loquax::outbox::OutboxEvent;
    use loquax::ui::{files, photos, update, UiEvent, UiModel};
    use loquax_android::session::{self, Line};
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 40, 30);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer
            .write_image_data(&[0xc0; 40 * 30 * 4])
            .expect("png data");
    }
    let peer = ugly_store::peer(94);
    let mut model = UiModel::<Line>::new(&[peer]);
    update(
        &mut model,
        UiEvent::SendFile {
            peer,
            name: "Bild.png".into(),
            data: png.clone(),
        },
    );
    update(
        &mut model,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [0x64; 32],
            source: peer,
            title: Vec::new(),
            body: Vec::new(),
            fields: files::file_fields("scan.dat", png.clone()),
            verified: true,
        }),
    );
    update(
        &mut model,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [0x65; 32],
            source: peer,
            title: Vec::new(),
            body: Vec::new(),
            fields: files::file_fields("not-a-picture.png", b"%PDF-1.7".to_vec()),
            verified: true,
        }),
    );
    let rows = session::rows(&model, 0, Lang::En);
    assert_eq!(rows.len(), 3, "{rows:#?}");
    for (row, name) in [(&rows[0], "Bild.png"), (&rows[1], "scan.dat")] {
        assert_eq!(row.photo, "png", "{row:#?}");
        assert_eq!((row.photo_w, row.photo_h), (40, 30));
        assert_eq!(row.photo_bytes as usize, png.len());
        assert!(
            row.photo_words.starts_with("Photo, "),
            "{}",
            row.photo_words
        );
        let names: Vec<&str> = row.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, [name], "the file's line stays under it");
    }
    let lie = &rows[2];
    assert_eq!((lie.photo.as_str(), lie.photo_w), ("", 0), "{lie:#?}");
    assert_eq!(lie.files.len(), 1);
    // What the phone's thumbnail asks for: the file's bytes, drawn.
    let theirs = &model.conversations[0].messages[1];
    let bytes = photos::picture_bytes(&theirs.fields).expect("the picture's bytes");
    let pixels = loquax::photo::fit(bytes, 20).expect("drawn");
    assert_eq!((pixels.width, pixels.height), (20, 15));
}

/// The map's Friends sheet (1200), as the facade answers it from the
/// session's model: everyone with a position, newest first, the last 24 h
/// marked recent and the older after them; ourselves apart as `me`, the
/// blocked not at all; a distance only with the phone's own fix, and each
/// row's `geo:` link for Navigate.
#[test]
fn the_friends_sheet_lists_positions_newest_first_with_the_day_marked() {
    use loquax::outbox::OutboxEvent;
    use loquax::ui::{update, UiEvent, UiModel};
    use loquax_android::friends;
    use loquax_android::session::Line;
    let now = ugly_store::BASE + 10 * 86_400;
    let me = ugly_store::peer(90);
    let mut model = UiModel::<Line>::new(&[]);
    model.address = Some(me);
    update(
        &mut model,
        UiEvent::Clock(loquax::when::WallTime {
            unix_secs: now,
            offset_secs: 0,
        }),
    );
    let report = |n: u8, peer: [u8; 16], at: i64, lat: i32| {
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [n; 32],
            source: peer,
            title: Vec::new(),
            body: Vec::new(),
            fields: ugly_store::position(at, lat),
            verified: true,
        })
    };
    let (anna, bob, carla, dieter) = (
        ugly_store::peer(91),
        ugly_store::peer(92),
        ugly_store::peer(93),
        ugly_store::peer(94),
    );
    // Bob an hour ago, Carla 30 h ago, Anna five minutes ago, Dieter
    // blocked a minute ago, ourselves now: received in no order.
    update(&mut model, report(1, bob, now - 3_600, 52_529_360));
    update(&mut model, report(2, carla, now - 30 * 3_600, 52_554_360));
    update(&mut model, report(3, anna, now - 300, 52_525_360));
    update(&mut model, report(4, dieter, now - 60, 52_524_360));
    update(&mut model, report(5, me, now, 52_524_360));
    model.names.contacts.blocked.insert(dieter);
    let hex = |p: &[u8; 16]| loquax::address::to_hex(p);

    let sheet = friends::friends(&model, now, None, Lang::En, |_| None);
    let order: Vec<(String, bool, i64)> = sheet
        .friends
        .iter()
        .map(|f| (f.peer.clone(), f.recent, f.at))
        .collect();
    assert_eq!(
        order,
        vec![
            (hex(&anna), true, now - 300),
            (hex(&bob), true, now - 3_600),
            (hex(&carla), false, now - 30 * 3_600),
        ],
        "newest first, the day marked, neither us nor the blocked"
    );
    assert!(
        sheet.friends.iter().all(|f| f.distance_m.is_none()),
        "no distance without the phone's fix"
    );
    let ours = sheet.me.expect("our own position apart");
    assert_eq!((ours.peer, ours.lat_e6), (hex(&me), 52_524_360));
    let anna_row = &sheet.friends[0];
    assert_eq!(anna_row.lon_e6, 11_256_000);
    assert!(!anna_row.initials.is_empty() && !anna_row.name.is_empty());
    assert_eq!(
        anna_row.geo,
        friends::geo_uri(52_525_360, 11_256_000, &anna_row.name)
    );
    // The window's edge: exactly 24 h old is still recent.
    update(
        &mut model,
        report(6, carla, now - friends::WINDOW_SECS, 52_554_360),
    );
    let sheet = friends::friends(&model, now, None, Lang::En, |_| None);
    assert!(sheet.friends[2].recent, "{:?}", sheet.friends[2]);

    // With the phone's fix: a thousandth of a degree of latitude is 111 m.
    let sheet = friends::friends(
        &model,
        now,
        Some((52_524_360, 11_256_000)),
        Lang::En,
        |_| None,
    );
    let metres: Vec<Option<u32>> = sheet.friends.iter().map(|f| f.distance_m).collect();
    assert_eq!(metres, vec![Some(111), Some(556), Some(3_336)]);
}

/// 1201: a share is the user's position to one peer, a report of ours;
/// the privacy page lists every one sent, newest first, with whom and
/// when, from the store, and a text or a report received is none of them.
/// A fix off the globe never leaves the phone.
#[test]
fn a_shared_position_reaches_the_privacy_list_and_a_bad_fix_is_refused() {
    use loquax::store::Store;
    use loquax::ui::{update, UiEffect, UiEvent, UiModel};
    use loquax_android::session::{self, Line};
    let now = ugly_store::BASE + 10 * 86_400;
    let me = ugly_store::peer(90);
    let (anna, bob) = (ugly_store::peer(91), ugly_store::peer(92));
    let home = tempfile::tempdir().expect("home");
    let mut store = Store::open_in(home.path(), me).expect("the store");
    let mut model = UiModel::<Line>::new(&[]);
    model.address = Some(me);
    let clock = |model: &mut UiModel<Line>, at: i64| {
        update(
            model,
            UiEvent::Clock(loquax::when::WallTime {
                unix_secs: at,
                offset_secs: 0,
            }),
        );
    };
    let share = |model: &mut UiModel<Line>, peer: [u8; 16], lat: i32, at: i64| {
        let location = session::share_location(lat, 13_404_954, 3_400, 500, at).expect("a place");
        let effects = update(model, UiEvent::SharePosition { peer, location });
        assert!(
            matches!(effects.as_slice(), [UiEffect::Send { body, .. }] if body.is_empty()),
            "{effects:?}"
        );
    };
    clock(&mut model, now - 600);
    share(&mut model, anna, 52_520_008, now - 610);
    update(
        &mut model,
        UiEvent::Write {
            peer: bob,
            body: "no position".to_string(),
        },
    );
    update(
        &mut model,
        UiEvent::Engine(loquax::outbox::OutboxEvent::Received {
            sent_at: 0,
            message_id: [7; 32],
            source: bob,
            title: Vec::new(),
            body: Vec::new(),
            fields: ugly_store::position(now - 500, 48_000_000),
            verified: true,
        }),
    );
    clock(&mut model, now);
    share(&mut model, bob, 52_530_000, now - 5);
    store.write(&model.take_writes()).expect("written");

    let list = session::shared(&model, &store, |_| None).expect("the list");
    let seen: Vec<(String, i64, i32)> = list
        .shared
        .iter()
        .map(|row| (row.peer.clone(), row.at, row.lat_e6))
        .collect();
    let hex = |p: &[u8; 16]| loquax::address::to_hex(p);
    assert_eq!(
        seen,
        vec![
            (hex(&bob), now, 52_530_000),
            (hex(&anna), now - 600, 52_520_008)
        ],
        "newest first, sent time, our fixes only"
    );
    assert!(list
        .shared
        .iter()
        .all(|row| !row.name.is_empty() && !row.initials.is_empty()));
    // It crosses as the app reads it.
    let bytes = schema::encode(&list);
    assert_eq!(schema::decode::<schema::Shared>(&bytes), Ok(list));
    // Our bubble names where we were.
    let rows = session::rows(&model, 0, Lang::En);
    assert!(
        rows.iter()
            .any(|row| row.ours && row.lat_e6 == Some(52_520_008) && row.place.is_some()),
        "{rows:?}"
    );

    for (lat, lon) in [(90_000_001, 0), (0, -180_000_001), (i32::MIN, i32::MAX)] {
        let refused = session::share_location(lat, lon, 0, 0, now);
        assert!(refused.is_err(), "{lat}, {lon}");
    }
    assert!(session::share_location(-90_000_000, 180_000_000, 0, 0, now).is_ok());
    // The request crosses as Java writes it, `at` a long.
    let request: Request = schema::decode(&schema::encode(&Request::Share {
        peer: hex(&anna),
        lat_e6: 1,
        lon_e6: 2,
        alt_e2: 0,
        accuracy_e2: 0,
        at: now,
    }))
    .expect("decodes");
    assert!(matches!(request, Request::Share { at, .. } if at == now));
}

/// The texts of the QR code `size` squared modules draw, read back by the
/// scanner's decoder: what a phone pointed at the profile reads.
fn read_qr(size: u32, modules: &[u8]) -> Vec<String> {
    let (size, scale) = (size as usize, 4);
    let side = (size + 8) * scale;
    let mut luma = vec![255u8; side * side];
    for y in 0..size {
        for x in 0..size {
            if modules[y * size + x] != 0 {
                for dy in 0..scale {
                    for dx in 0..scale {
                        luma[((y + 4) * scale + dy) * side + (x + 4) * scale + dx] = 0;
                    }
                }
            }
        }
    }
    loquax_android::scan::decode(side, side, side, &luma)
}

/// Every file under `dir` named `name`, however deep.
fn files_named(dir: &Path, name: &str) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_named(&path, name));
        } else if path.file_name().is_some_and(|n| n == name) {
            found.push(path);
        }
    }
    found
}

/// The profile behind the avatar (1187) and a contact from its code: the
/// profile names our address, the contact code of the mailbox's identity
/// and a QR code that reads back as exactly that code, where the mailbox
/// is and the advanced facts. Our own code parses back to our address; a
/// contact code adds the contact under the name given, its key reaches
/// the home node, which keeps it on disk, and a malformed code is refused
/// with words for the screen.
#[tokio::test(flavor = "multi_thread")]
async fn the_profile_shows_our_contact_code_and_a_scanned_code_adds_a_contact() {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let home_identity =
        loquax::identity::load_or_create(&home.path().join("identity")).expect("identity");
    let home_key = home_identity.public_key_bytes();

    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    let bob = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let bob_key = bob.public_key_bytes();
    let bob_code = loquax::lxma::encode(&bob_key).expect("bob's code");
    let bob_address = loquax::address::to_hex(&loquax::home::lxmf_address(&bob_key).expect("a"));
    let session = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        let profile: schema::Profile =
            rmp_serde::from_slice(&session.query(schema::Query::Profile).expect("profile"))
                .expect("decode");
        let address = loquax::address::to_hex(&session.address());
        assert_eq!(profile.address, address);
        assert_eq!(profile.lxma, loquax::lxma::encode(&home_key).expect("code"));
        assert!(profile.lxma.starts_with(&format!("lxma://{address}:")));
        assert_eq!(
            profile.qr.0.len(),
            (profile.qr_size * profile.qr_size) as usize
        );
        assert_eq!(
            read_qr(profile.qr_size, &profile.qr.0),
            vec![profile.lxma.clone()]
        );
        assert!(!profile.here && !profile.name_editable);
        assert_eq!(profile.mailbox, "127.0.0.1");
        assert!(profile.since > 0, "{profile:?}");
        assert_eq!(profile.identity, hex(home_identity.hash()));
        assert_eq!(
            profile.home_node,
            hex(&loquax::home::destination_hash(&home_key).expect("home"))
        );
        assert_eq!(profile.version, loquax::VERSION);

        // The first start's QR is the same code (1263, finding 1).
        let me: schema::Me =
            rmp_serde::from_slice(&session.query(schema::Query::Me).expect("me")).expect("me");
        assert_eq!(me.lxma, profile.lxma);
        assert_eq!(read_qr(me.qr_size, &me.qr.0), vec![profile.lxma.clone()]);

        // Our own code, parsed back: our address.
        let ours: schema::Lxma = rmp_serde::from_slice(
            &session
                .query(schema::Query::Lxma {
                    text: profile.lxma.clone(),
                    lang: None,
                })
                .expect("our code parses"),
        )
        .expect("decode");
        assert_eq!(ours.address, address);

        // Bob's, pasted with a newline in capitals: whose it is, then added.
        let scanned: schema::Lxma = rmp_serde::from_slice(
            &session
                .query(schema::Query::Lxma {
                    text: format!("{}\n", bob_code.to_uppercase()),
                    lang: None,
                })
                .expect("bob's code parses"),
        )
        .expect("decode");
        assert_eq!(scanned.kind, "contact");
        assert_eq!(scanned.address, bob_address);
        assert_eq!(scanned.lxma, bob_code);
        assert_eq!(scanned.known, "");
        session
            .request(Request::AddContact {
                lxma: scanned.lxma.clone(),
                name: Some("Bob".to_string()),
            })
            .expect("added");
        wait_for(
            &session,
            schema::Query::Lxma {
                text: bob_code.clone(),
                lang: None,
            },
            20,
            |l: &schema::Lxma| l.known == "Bob",
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while session.keys_told() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the home node never took the key"
            );
            let _ = session.next_event(Duration::from_millis(200));
        }

        // A code with a wrong digit, and no code at all: refused, in words.
        let mut wrong = bob_code.clone();
        let at = "lxma://".len();
        let flipped = if wrong.as_bytes()[at] == b'0' {
            "1"
        } else {
            "0"
        };
        wrong.replace_range(at..at + 1, flipped);
        let refused = session
            .request(Request::AddContact {
                lxma: wrong.clone(),
                name: None,
            })
            .expect_err("a wrong code");
        assert_eq!(refused, "This code is damaged. Ask for it again.");
        let refused = session
            .query(schema::Query::Lxma {
                text: "hello".to_string(),
                lang: None,
            })
            .expect_err("no code");
        assert_eq!(refused, "That is not a Loquax code.");

        // An old code, the address alone in capitals as the first start's
        // QR held it before 1263: a contact without a key, its chat open.
        let carol = leviculum_core::Identity::generate(&mut rand_core::OsRng);
        let carol_address = loquax::home::lxmf_address(&carol.public_key_bytes()).expect("a");
        let carol_hex = loquax::address::to_hex(&carol_address);
        let old: schema::Lxma = rmp_serde::from_slice(
            &session
                .query(schema::Query::Lxma {
                    text: carol_hex.to_uppercase(),
                    lang: None,
                })
                .expect("an address reads"),
        )
        .expect("decode");
        assert_eq!(
            (old.kind.as_str(), old.address.as_str(), old.lxma.as_str()),
            ("address", carol_hex.as_str(), carol_hex.as_str())
        );
        session
            .request(Request::AddContact {
                lxma: old.lxma,
                name: Some("Carol".to_string()),
            })
            .expect("added by address");
        wait_for(
            &session,
            schema::Query::Lxma {
                text: carol_hex.clone(),
                lang: None,
            },
            20,
            |l: &schema::Lxma| l.known == "Carol",
        );
        // A mailbox's code reads as one, and is no contact.
        let mailbox: schema::Lxma = rmp_serde::from_slice(
            &session
                .query(schema::Query::Lxma {
                    text: code_for(&carol),
                    lang: None,
                })
                .expect("a mailbox code reads"),
        )
        .expect("decode");
        assert_eq!(
            (mailbox.kind.as_str(), mailbox.address.as_str()),
            ("mailbox", "")
        );
        assert_eq!(
            session
                .request(Request::AddContact {
                    lxma: code_for(&carol),
                    name: None,
                })
                .expect_err("no contact"),
            "That code is for a mailbox, not for a contact."
        );
        session
    })
    .await
    .expect("the profile");
    tokio::task::spawn_blocking(move || session.stop())
        .await
        .expect("stopped");
    node.stop().await;
    let kept = files_named(home.path(), "known_destinations");
    assert!(
        kept.iter().any(|file| std::fs::read(file)
            .unwrap_or_default()
            .windows(bob_key.len())
            .any(|window| window == bob_key)),
        "the home node kept no key for Bob in {kept:?}"
    );
}

/// One query in the bytes Java sends, through the session as the facade's
/// `query` hands it on, and its answer as `T`.
fn java_query<T: serde::de::DeserializeOwned>(
    session: &Session,
    entries: &[(&str, Java)],
) -> Result<T, String> {
    let query: schema::Query = schema::decode(&java_map(entries))?;
    let answer = session.query(query)?;
    rmp_serde::from_slice(&answer).map_err(|error| error.to_string())
}

/// 1207, Lew's phone on 2026-10-31: the Maps page showed the switch off,
/// "0 B" and no maps with Berlin installed, and the switch said "the
/// request did not decode: missing field `w`". The page and Appearance
/// now ask with queries of their own, here in Java's bytes with exactly
/// the fields `Core.mapRegions` and `Core.mapTheme` put in, against a
/// phone with one installed region and no `map_autoload` in its config:
/// the region with its size and date, the automatic download on (1089),
/// the switch off stored and kept across a session's restart, and a new
/// renderer reads it from the config; Delete removes the file; the map's
/// theme is asked and set; the Advanced page's map fling (1188) is on
/// until switched off, and the next frame says so; the Privacy page's
/// "Send read receipts" (1233) is on, switched off, kept off across the
/// restart, and on again.
#[tokio::test(flavor = "multi_thread")]
async fn the_settings_pages_ask_the_renderer_in_javas_bytes() {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let maps = phone.path().join("maps");
    std::fs::create_dir_all(&maps).expect("maps dir");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures/firenze.lmr");
    std::fs::copy(&fixture, maps.join("firenze.lmr")).expect("the installed region");
    let len = std::fs::metadata(maps.join("firenze.lmr"))
        .expect("its size")
        .len();
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        let config = loquax::config::load(Path::new(&first.dir)).unwrap_or_default();
        assert_eq!(config.map_autoload, None, "no choice stored yet");

        let page: schema::MapRegions =
            java_query(&session, &[("t", Java::Str("mapregions"))]).expect("the Maps page");
        assert!(page.ok && page.autoload, "on by default (1089): {page:?}");
        assert_eq!(page.regions.len(), 1, "{page:?}");
        let region = &page.regions[0];
        assert_eq!(region.name, "firenze.lmr");
        assert_eq!((region.bytes, page.region_bytes), (len, len));
        assert!(region.modified > 0, "a date: {region:?}");

        let off: schema::MapRegions = java_query(
            &session,
            &[
                ("t", Java::Str("mapregions")),
                ("set_autoload", Java::Bool(false)),
            ],
        )
        .expect("the switch off");
        assert!(off.ok && !off.autoload && off.regions.len() == 1, "{off:?}");

        let theme: schema::MapThemeChoice =
            java_query(&session, &[("t", Java::Str("maptheme"))]).expect("Appearance");
        assert!(
            theme.ok && theme.theme == "auto" && !theme.theme_chosen,
            "{theme:?}"
        );
        let dark: schema::MapThemeChoice = java_query(
            &session,
            &[("t", Java::Str("maptheme")), ("set", Java::Str("dark"))],
        )
        .expect("dark chosen");
        assert!(
            dark.ok && dark.theme == "dark" && dark.theme_chosen,
            "{dark:?}"
        );
        let refused = java_query::<schema::MapThemeChoice>(
            &session,
            &[("t", Java::Str("maptheme")), ("set", Java::Str("sepia"))],
        );
        assert!(refused.is_err(), "{refused:?}");

        let fling: schema::MapFlingChoice =
            java_query(&session, &[("t", Java::Str("mapfling"))]).expect("Advanced");
        assert!(fling.ok && fling.fling, "on by default: {fling:?}");
        let off: schema::MapFlingChoice = java_query(
            &session,
            &[("t", Java::Str("mapfling")), ("set", Java::Bool(false))],
        )
        .expect("the fling off");
        assert!(off.ok && !off.fling, "{off:?}");
        assert_eq!(
            loquax::config::load(Path::new(&first.dir))
                .expect("config")
                .map_fling,
            Some(false)
        );
        let ask = schema::MapRequest {
            dir: first.dir.clone(),
            w: 480,
            h: 320,
            density_milli: 1000,
            ..schema::MapRequest::default()
        };
        let (shown, _) = schema::unframe(&loquax_android::map::render(
            &ask,
            Some(Vec::new()),
            loquax::when::WallTime {
                unix_secs: 1_790_000_000,
                offset_secs: 0,
            },
        ))
        .expect("a frame");
        assert!(shown.ok && !shown.fling, "the frame says it: {shown:?}");
        let on: schema::MapFlingChoice = java_query(
            &session,
            &[("t", Java::Str("mapfling")), ("set", Java::Bool(true))],
        )
        .expect("the fling on");
        assert!(on.ok && on.fling, "{on:?}");

        // The Privacy page's "Send read receipts" (1233), as
        // `Core.receipts` asks: on by default, off stored in the config.
        let receipts: schema::ReceiptsChoice =
            java_query(&session, &[("t", Java::Str("receipts"))]).expect("Privacy");
        assert!(receipts.ok && receipts.on, "on by default: {receipts:?}");
        let off: schema::ReceiptsChoice = java_query(
            &session,
            &[("t", Java::Str("receipts")), ("set", Java::Bool(false))],
        )
        .expect("receipts off");
        assert!(off.ok && !off.on, "{off:?}");
        assert_eq!(
            loquax::config::load(Path::new(&first.dir))
                .expect("config")
                .read_receipts,
            Some(false)
        );
        session.stop();
    })
    .await
    .expect("the first session");

    // Restarted as the sticky service starts it: the switch is still off,
    // and a renderer made afresh reads it from the config.
    let again = Start {
        dir: phone.path().display().to_string(),
        ..Start::default()
    };
    let dir = phone.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&again).unwrap_or_else(|error| panic!("{error:?}"));
        let page: schema::MapRegions =
            java_query(&session, &[("t", Java::Str("mapregions"))]).expect("the Maps page");
        assert!(page.ok && !page.autoload, "kept: {page:?}");
        // The switch read back from the config, and on again.
        let receipts: schema::ReceiptsChoice =
            java_query(&session, &[("t", Java::Str("receipts"))]).expect("Privacy");
        assert!(receipts.ok && !receipts.on, "kept off: {receipts:?}");
        let on: schema::ReceiptsChoice = java_query(
            &session,
            &[("t", Java::Str("receipts")), ("set", Java::Bool(true))],
        )
        .expect("receipts on");
        assert!(on.ok && on.on, "{on:?}");
        assert_eq!(
            loquax::config::load(&dir).expect("config").read_receipts,
            Some(true)
        );
        let fresh = loquax_android::map::Renderer::new(&dir)
            .regions(None, None)
            .expect("a fresh renderer");
        assert!(!fresh.autoload, "stored in the config");
        assert_eq!(
            loquax::config::load(&dir).expect("config").map_autoload,
            Some(false)
        );

        let on: schema::MapRegions = java_query(
            &session,
            &[
                ("t", Java::Str("mapregions")),
                ("set_autoload", Java::Bool(true)),
            ],
        )
        .expect("the switch on");
        assert!(on.ok && on.autoload, "{on:?}");

        let refused = java_query::<schema::MapRegions>(
            &session,
            &[
                ("t", Java::Str("mapregions")),
                ("delete", Java::Str("../config.toml")),
            ],
        );
        assert!(refused.is_err(), "{refused:?}");
        let gone: schema::MapRegions = java_query(
            &session,
            &[
                ("t", Java::Str("mapregions")),
                ("delete", Java::Str("firenze.lmr")),
            ],
        )
        .expect("deleted");
        assert!(
            gone.ok && gone.regions.is_empty() && gone.region_bytes == 0,
            "{gone:?}"
        );
        assert!(!dir.join("maps/firenze.lmr").exists());
        session.stop();
    })
    .await
    .expect("the second session");
    node.stop().await;
}

/// 1290: the Maps page's "Delete all downloaded maps", in Java's bytes as
/// `Core.deleteRegions` sends it after its question, against a phone with
/// three regions, a hand placed one among them: before, the page lists
/// three and their sum; after, none, zero bytes, and no region file left,
/// the stray file beside them untouched and the automatic download as it
/// was. A second press with nothing left deletes nothing and is no error.
#[tokio::test(flavor = "multi_thread")]
async fn delete_all_maps_empties_the_maps_page() {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let maps = phone.path().join("maps");
    std::fs::create_dir_all(&maps).expect("maps dir");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures/firenze.lmr");
    let names = ["firenze.lmr", "auto-a.lmr", "auto-b.lmr"];
    for name in names {
        std::fs::copy(&fixture, maps.join(name)).expect("a region");
    }
    std::fs::write(maps.join("notes.txt"), "not a region").expect("a stray file");
    let len = std::fs::metadata(&fixture).expect("its size").len();
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        let page: schema::MapRegions =
            java_query(&session, &[("t", Java::Str("mapregions"))]).expect("the Maps page");
        assert!(page.ok && page.autoload, "{page:?}");
        assert_eq!(page.regions.len(), 3, "{page:?}");
        assert_eq!(page.region_bytes, 3 * len);

        let delete = || {
            session.request(schema::decode(&java_map(&[(
                "t",
                Java::Str("deleteregions"),
            )]))?)
        };
        delete().expect("every map deleted");
        let empty: schema::MapRegions =
            java_query(&session, &[("t", Java::Str("mapregions"))]).expect("the page again");
        assert!(
            empty.ok && empty.regions.is_empty() && empty.region_bytes == 0,
            "{empty:?}"
        );
        assert!(empty.autoload, "the automatic download stays on");
        for name in names {
            assert!(!maps.join(name).exists(), "{name} is gone");
        }
        assert!(maps.join("notes.txt").is_file(), "only regions go");
        delete().expect("nothing left is no error");
        session.stop();
    })
    .await
    .expect("the session");
    node.stop().await;
}

/// One answer of `session` to `query`.
fn asked<T: serde::de::DeserializeOwned>(session: &Session, query: schema::Query) -> T {
    rmp_serde::from_slice(&session.query(query).expect("a query")).expect("decode")
}

/// 1219, over the ugly store against a real home node: a message typed and
/// not sent stays with its conversation. Typed in one chat, it is in the
/// list as `Draft: …` once another opens or none, counts nothing unread,
/// and is in the field again when its chat opens. The store holds it once
/// typing paused, before the session ends; a new session puts it back
/// with its reply. The reply sent takes it away.
#[tokio::test(flavor = "multi_thread")]
async fn a_draft_survives_another_chat_and_a_new_session() {
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
    let first = Start {
        dir: dir.clone(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        lang: Some("de".to_string()),
        ..Start::default()
    };
    let (a, b) = (ugly_store::peer_hex(5), ugly_store::peer_hex(6));
    let phone_dir = phone.path().to_path_buf();
    let first_a = a.clone();
    let (address, target) = tokio::task::spawn_blocking(move || {
        let a = first_a;
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        ugly_in(&session);
        let row = |session: &Session, peer: &str| -> schema::ConversationRow {
            let list: schema::Conversations =
                asked(session, schema::Query::Conversations { search: None });
            list.conversations
                .into_iter()
                .find(|r| r.peer == peer)
                .expect("the row")
        };
        let before = row(&session, &a);
        session
            .request(Request::Open { peer: a.clone() })
            .expect("open");
        let messages: schema::Messages =
            asked(&session, schema::Query::Messages { peer: a.clone() });
        assert_eq!(messages.draft, "", "none yet");
        let target = messages
            .messages
            .iter()
            .rev()
            .find(|m| !m.ours && !m.id.is_empty())
            .expect("a message of theirs")
            .id
            .clone();
        // 1381, finding 19: Info on it is plain rows in German, the id
        // only under Advanced.
        let info = |session: &Session, peer: &str, id: &str| -> schema::Info {
            let messages: schema::Messages = asked(
                session,
                schema::Query::Messages {
                    peer: peer.to_string(),
                },
            );
            let key = messages
                .messages
                .iter()
                .find(|m| m.id == id)
                .expect("the message")
                .key;
            asked(
                session,
                schema::Query::Info {
                    peer: peer.to_string(),
                    key,
                },
            )
        };
        let german = info(&session, &a, &target);
        assert_eq!(german.rows[0].label, "Von", "{german:?}");
        let size = german
            .rows
            .iter()
            .find(|r| r.label == "Größe")
            .unwrap_or_else(|| panic!("no size: {german:?}"));
        assert!(
            size.value.ends_with(" KB") && size.value.contains(','),
            "{german:?}"
        );
        assert!(
            german
                .rows
                .iter()
                .all(|r| !r.value.contains(&target[..8]) && !r.value.contains(&a[..8])),
            "hex in the plain rows: {german:?}"
        );
        assert!(
            german.advanced.contains(&format!("id     {target}")),
            "{german:?}"
        );
        for typed in ["half", "half a thought"] {
            session
                .request(Request::Draft {
                    peer: a.clone(),
                    body: typed.to_string(),
                    reply: target.clone(),
                })
                .expect("draft");
        }
        let messages: schema::Messages =
            asked(&session, schema::Query::Messages { peer: a.clone() });
        assert_eq!(messages.draft, "half a thought");
        assert_eq!(messages.draft_reply, target);
        assert_eq!(
            row(&session, &a).draft,
            "",
            "the open chat's row shows none"
        );

        // Another chat: its own field, and the first one's row says it.
        session.request(Request::Close).expect("close");
        session
            .request(Request::Open { peer: b.clone() })
            .expect("open the other");
        let other: schema::Messages = asked(&session, schema::Query::Messages { peer: b.clone() });
        assert_eq!(other.draft, "");
        assert_eq!(other.draft_reply, "");
        let shown = row(&session, &a);
        assert_eq!(shown.draft, "Entwurf: half a thought");
        assert_eq!(shown.unread, before.unread, "a draft is nothing unread");
        assert_eq!(shown.last, before.last, "the last line is kept beside it");
        session.request(Request::Close).expect("close");
        let list: schema::Conversations =
            asked(&session, schema::Query::Conversations { search: None });
        let order = |l: &schema::Conversations| {
            l.conversations
                .iter()
                .map(|r| r.peer.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            list.conversations
                .iter()
                .filter(|r| !r.draft.is_empty())
                .count(),
            1
        );

        // Back: the field holds it again.
        session
            .request(Request::Open { peer: a.clone() })
            .expect("open again");
        let messages: schema::Messages =
            asked(&session, schema::Query::Messages { peer: a.clone() });
        assert_eq!(messages.draft, "half a thought");
        session.request(Request::Close).expect("close");
        let after: schema::Conversations =
            asked(&session, schema::Query::Conversations { search: None });
        assert_eq!(
            order(&after),
            order(&list),
            "a draft moves nothing in the list"
        );

        // The store has it once typing paused, before the session ends.
        let me = session.address();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let store = loquax::store::Store::open_read_only(
                &phone_dir.join(loquax::store::STORE_FILE),
                me,
            )
            .expect("the store")
            .expect("it is there");
            let kept = store
                .restore()
                .expect("restore")
                .into_iter()
                .find(|c| loquax::address::to_hex(&c.peer) == a)
                .map(|c| c.draft.text);
            if kept.as_deref() == Some("half a thought") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "never kept: {kept:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(session);
        (loquax::address::to_hex(&me), target)
    })
    .await
    .expect("the first session");

    // A new session: the field opens with it, the reply with it.
    let again = Start {
        dir,
        lang: Some("en".to_string()),
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&again).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(loquax::address::to_hex(&session.address()), address);
        let list: schema::Conversations =
            asked(&session, schema::Query::Conversations { search: None });
        let row = list
            .conversations
            .iter()
            .find(|r| r.peer == a)
            .expect("the row");
        assert_eq!(row.draft, "Draft: half a thought");
        session
            .request(Request::Open { peer: a.clone() })
            .expect("open");
        let messages: schema::Messages =
            asked(&session, schema::Query::Messages { peer: a.clone() });
        assert_eq!(messages.draft, "half a thought");
        assert_eq!(messages.draft_reply, target);
        // The same Info in an English session (1381).
        let key = messages
            .messages
            .iter()
            .find(|m| m.id == target)
            .expect("the message")
            .key;
        let english: schema::Info = asked(
            &session,
            schema::Query::Info {
                peer: a.clone(),
                key,
            },
        );
        assert_eq!(english.rows[0].label, "From", "{english:?}");
        assert!(
            english.rows.iter().any(|r| r.label == "Size"),
            "{english:?}"
        );

        // The reply sent takes the draft with it.
        session
            .request(Request::Reply {
                peer: a.clone(),
                target: target.clone(),
                body: "half a thought".to_string(),
            })
            .expect("reply");
        let messages: schema::Messages =
            asked(&session, schema::Query::Messages { peer: a.clone() });
        assert_eq!(messages.draft, "");
        assert_eq!(messages.draft_reply, "");
        // 1449 item 1 (walk finding 5): the reply chosen, Back (the
        // keyboard, then the screen: a Close and nothing else since 1449,
        // no draft without its reply), the chat again, Send: the message
        // that went out carries the quote.
        let sent = messages
            .messages
            .iter()
            .rev()
            .find(|m| m.ours && m.body == "half a thought")
            .expect("the reply sent");
        assert_eq!(sent.quote_to.as_deref(), Some(target.as_str()), "{sent:?}");
        assert!(
            sent.quote.as_deref().is_some_and(|q| !q.is_empty()),
            "{sent:?}"
        );
        session.request(Request::Close).expect("close");
        let list: schema::Conversations =
            asked(&session, schema::Query::Conversations { search: None });
        assert!(list.conversations.iter().all(|r| r.draft.is_empty()));
        session.stop();
    })
    .await
    .expect("the second session");

    node.kill().await;
}

/// 1270, against a real home node: the compose bar through the facade.
/// A phone with a camera has the five buttons and the plus; a typed
/// letter makes it Send and hides the microphone. The camera's capture
/// (the photo fixture as a camera writes it, EXIF turn, place and model
/// in it) reaches the same scaling and stripping as a picked one: the
/// staged photo is upright at the phone size, and the message Send makes
/// of it and its caption is one row carrying exactly the bytes
/// `loquax::photo::prepare` makes of a gallery pick. The position
/// button's sheet says there is no fix, then shows the one the phone gave,
/// and its row sends it as a report.
#[tokio::test(flavor = "multi_thread")]
async fn the_compose_bar_stages_a_camera_photo_through_the_gallery_path() {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        camera: true,
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        let peer = "0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c".to_string();
        session
            .request(Request::Open { peer: peer.clone() })
            .expect("open");
        let bar = |session: &Session| -> schema::Bar {
            asked(session, schema::Query::Bar { peer: peer.clone() })
        };
        let ids = |bar: &schema::Bar| bar.buttons.iter().map(|b| b.id.clone()).collect::<Vec<_>>();
        let empty = bar(&session);
        assert_eq!(empty.round, "plus");
        assert_eq!(ids(&empty), ["emoji", "locate", "camera", "record", "plus"]);
        assert_eq!(empty.buttons[2].label, "Take a photo");
        assert_eq!(empty.hint, "Message");
        session
            .request(Request::Draft {
                peer: peer.clone(),
                body: "h".to_string(),
                reply: String::new(),
            })
            .expect("draft");
        let typed = bar(&session);
        assert_eq!(typed.round, "send");
        assert_eq!(ids(&typed), ["emoji", "locate", "camera", "send"]);
        session
            .request(Request::Draft {
                peer: peer.clone(),
                body: String::new(),
                reply: String::new(),
            })
            .expect("draft");

        // The camera's capture, staged.
        session
            .request(Request::Stage {
                peer: peer.clone(),
                data: schema::Blob(PHOTO.to_vec()),
            })
            .expect("stage");
        let staged = bar(&session);
        let gallery = loquax::photo::prepare(PHOTO, loquax::ui::photos::PhotoSize::Phone)
            .expect("the gallery path");
        assert_eq!(
            staged.staged,
            format!(
                "Photo to send, {}",
                loquax::ui::photos::size_words(gallery.data.len() as u64)
            )
        );
        assert_eq!(staged.round, "send", "a staged photo is something to send");
        assert_eq!(staged.hint, "Add a caption");
        let pixels: schema::PhotoPixels = asked(
            &session,
            schema::Query::Staged {
                peer: peer.clone(),
                edge: 4096,
            },
        );
        assert_eq!(
            (pixels.full_width, pixels.full_height),
            (gallery.width, gallery.height)
        );
        assert_eq!(
            (gallery.width, gallery.height),
            (1200, 1600),
            "upright, phone size"
        );
        assert!(red_corner(&pixels.rgba.0, pixels.width), "drawn upright");
        let before: schema::Messages =
            asked(&session, schema::Query::Messages { peer: peer.clone() });
        session
            .request(Request::Send {
                peer: peer.clone(),
                body: "the view".to_string(),
            })
            .expect("send");
        let after: schema::Messages =
            asked(&session, schema::Query::Messages { peer: peer.clone() });
        assert_eq!(
            after.messages.len(),
            before.messages.len() + 1,
            "one message"
        );
        let sent = after.messages.last().expect("the photo's row");
        assert_eq!(sent.body, "the view");
        assert_eq!(sent.photo, gallery.format);
        assert_eq!(
            sent.photo_bytes as usize,
            gallery.data.len(),
            "the gallery's bytes"
        );
        assert_eq!(after.staged, "");
        assert_eq!(after.round, "plus");

        // The position button's sheet: no fix, then the phone's.
        session
            .request(Request::Sheet {
                peer: peer.clone(),
                kind: "position".to_string(),
            })
            .expect("sheet");
        session.request(Request::NoFix).expect("no fix");
        let none = bar(&session);
        assert_eq!(none.sheet, "position");
        // Whom it goes to (1448): the peer's name, nameless here.
        assert_eq!(none.sheet_title, "Send position to Unknown");
        assert!(!none.has_fix);
        assert_eq!(none.fix_words, "Loquax does not know where you are yet.");
        assert_eq!(none.sheet_rows[0].id, "sheet_location_on");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970")
            .as_secs() as i64;
        session
            .request(Request::Fix {
                lat_e6: 43_773_100,
                lon_e6: 11_256_000,
                alt_e2: 0,
                accuracy_e2: 500,
                at: now,
            })
            .expect("fix");
        let fixed = bar(&session);
        assert!(fixed.has_fix);
        assert_eq!(
            (fixed.fix_lat_e6, fixed.fix_lon_e6),
            (43_773_100, 11_256_000)
        );
        assert_eq!(fixed.sheet_rows[0].id, "sheet_share_now");
        assert_eq!(fixed.sheet_rows[0].label, "Now, once");
        session
            .request(Request::ShareFix { peer: peer.clone() })
            .expect("share");
        let shared: schema::Messages =
            asked(&session, schema::Query::Messages { peer: peer.clone() });
        assert_eq!(shared.sheet, "", "the share closes the sheet");
        let report = shared.messages.last().expect("the report's row");
        assert!(report.ours);
        assert_eq!(
            (report.lat_e6, report.lon_e6),
            (Some(43_773_100), Some(11_256_000))
        );

        // The plus's sheet: its three rows.
        session
            .request(Request::Sheet {
                peer: peer.clone(),
                kind: "attach".to_string(),
            })
            .expect("sheet");
        let attach = bar(&session);
        assert_eq!(
            attach
                .sheet_rows
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["sheet_gallery", "sheet_file", "sheet_position"]
        );
        session
            .request(Request::Sheet {
                peer: peer.clone(),
                kind: String::new(),
            })
            .expect("close");
        assert_eq!(bar(&session).sheet, "");
        session.stop();
    })
    .await
    .expect("the session");
    node.kill().await;
}

/// 1273: a share for 15 minutes, started from the position sheet's row,
/// sends the phone's fix at once, shows its chip in the chat, the ⌖ on
/// the list's row and its row in Settings, Privacy; a share until stopped
/// replaces it; Stop sends one report marked as the end, takes the chip,
/// the sign and the row away, and the phone's store keeps no share.
#[tokio::test(flavor = "multi_thread")]
async fn a_share_starts_lists_and_stops_with_its_end_report() {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let phone = tempfile::tempdir().expect("app dir");
    let phone_dir = phone.path().to_path_buf();
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        let address = session.address();
        let peer = "0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c".to_string();
        session
            .request(Request::Open { peer: peer.clone() })
            .expect("open");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970")
            .as_secs() as i64;
        session
            .request(Request::Fix {
                lat_e6: 43_773_100,
                lon_e6: 11_256_000,
                alt_e2: 0,
                accuracy_e2: 500,
                at: now,
            })
            .expect("fix");
        session
            .request(Request::Sheet {
                peer: peer.clone(),
                kind: "position".to_string(),
            })
            .expect("sheet");
        let sheet: schema::Bar = asked(&session, schema::Query::Bar { peer: peer.clone() });
        assert_eq!(
            sheet
                .sheet_rows
                .iter()
                .map(|r| (r.id.as_str(), r.label.as_str()))
                .collect::<Vec<_>>(),
            [
                ("sheet_share_now", "Now, once"),
                ("sheet_share_15_min", "For 15 minutes"),
                ("sheet_share_1_h", "For 1 hour"),
                ("sheet_share_8_h", "For 8 hours"),
                ("sheet_share_until_stopped", "Until I stop"),
            ]
        );
        let none: schema::Shares = asked(&session, schema::Query::Shares);
        assert!(none.shares.is_empty());
        assert_eq!(none.title, "Position sharing");
        // A report of ours shows its fix once the session knows its own
        // address (`own_fix`), which the engine says when it is ready: on
        // a loaded host after the share started, so the count is waited for.
        let reports = |session: &Session, want: usize| -> usize {
            let count = || {
                let chat: schema::Messages =
                    asked(session, schema::Query::Messages { peer: peer.clone() });
                chat.messages
                    .iter()
                    .filter(|m| m.ours && m.lat_e6.is_some())
                    .count()
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while count() < want && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            count()
        };
        assert!(
            session
                .request(Request::StartShare {
                    peer: peer.clone(),
                    span: "a while".to_string(),
                })
                .is_err(),
            "a span the library does not know"
        );
        session
            .request(Request::StartShare {
                peer: peer.clone(),
                span: "15_min".to_string(),
            })
            .expect("start");
        let chat: schema::Messages =
            asked(&session, schema::Query::Messages { peer: peer.clone() });
        assert_eq!(chat.sheet, "", "the row closes the sheet");
        assert!(
            chat.share.starts_with("Sharing your position, ") && chat.share.ends_with(" min left"),
            "{:?}",
            chat.share
        );
        assert_eq!(chat.share_stop, "Stop");
        assert_eq!(reports(&session, 1), 1, "the first fix at once");
        let list: schema::Conversations =
            asked(&session, schema::Query::Conversations { search: None });
        assert!(list
            .conversations
            .iter()
            .any(|row| row.peer == peer && row.sharing));
        let running: schema::Shares = asked(&session, schema::Query::Shares);
        assert_eq!(running.shares.len(), 1);
        assert_eq!(running.shares[0].peer, peer);
        assert_eq!(running.shares[0].id, format!("share_{peer}"));
        assert!((running.shares[0].until - (now + 900)).abs() <= 5);

        session
            .request(Request::StartShare {
                peer: peer.clone(),
                span: "until_stopped".to_string(),
            })
            .expect("until stopped");
        let chat: schema::Messages =
            asked(&session, schema::Query::Messages { peer: peer.clone() });
        assert_eq!(chat.share, "Sharing your position until you stop");
        let running: schema::Shares = asked(&session, schema::Query::Shares);
        assert_eq!(running.shares[0].until, 0);

        session
            .request(Request::StopShare { peer: peer.clone() })
            .expect("stop");
        let chat: schema::Messages =
            asked(&session, schema::Query::Messages { peer: peer.clone() });
        assert_eq!(chat.share, "");
        assert_eq!(reports(&session, 3), 3, "the first of each, and the end");
        let list: schema::Conversations =
            asked(&session, schema::Query::Conversations { search: None });
        assert!(list.conversations.iter().all(|row| !row.sharing));
        let none: schema::Shares = asked(&session, schema::Query::Shares);
        assert!(none.shares.is_empty());
        session.stop();
        address
    })
    .await
    .map(|address| {
        let store = loquax::store::Store::open_in(&phone_dir, address).expect("the phone's store");
        assert!(store.shares().expect("shares").is_empty(), "no share kept");
        let restored = store.restore().expect("restore");
        let marks: Vec<leviculum_lxmf::telemetry::ShareMark> = restored
            .iter()
            .flat_map(|c| &c.messages)
            .filter(|m| m.outbound)
            .filter_map(|m| {
                let (_, value) = m
                    .fields
                    .iter()
                    .find(|(k, _)| *k == leviculum_lxmf::constants::FIELD_TELEMETRY)?;
                let mut p = 0;
                let blob = leviculum_lxmf::msgpack::read_bin(value, &mut p).ok()?;
                Some(leviculum_lxmf::telemetry::ShareMark::of_blob(blob))
            })
            .collect();
        assert_eq!(marks.len(), 3, "{marks:?}");
        assert!(marks[..2].iter().all(|m| m.every == Some(900) && !m.ended));
        assert!(marks[2].ended, "the stop's report is the end: {marks:?}");
    })
    .expect("the session");
    node.kill().await;
}

/// 1230, against the ugly store: a read receipt from a peer turns the
/// message of ours it names into `read` on the phone's rows, with the
/// reader's time, adds no row, and the read and the peer's flag survive the
/// store. An id the peer was never sent is dropped with a log line.
#[test]
fn a_read_receipt_reads_ours_on_the_phone_and_survives_the_store() {
    use leviculum_lxmf::lev::{Lev, Receipt};
    use leviculum_ui::delivery::DeliveryState;
    use loquax::outbox::OutboxEvent;
    use loquax::ui::{update, UiEffect, UiEvent};
    use loquax::when::WallTime;
    use loquax_android::session;
    let home = tempfile::tempdir().expect("app dir");
    let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    ugly_store::fill_store_of(home.path(), &identity);
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let mut store = loquax::store::Store::open_in(home.path(), me).expect("the store");
    let mut model = session::known(home.path(), &store).expect("a model");
    let start = ugly_store::BASE + 3 * 86_400;
    update(
        &mut model,
        UiEvent::Clock(WallTime {
            unix_secs: start,
            offset_secs: 0,
        }),
    );
    model.restore(store.restore().expect("restore"));
    let (c, id, sent) = model
        .conversations
        .iter()
        .enumerate()
        .find_map(|(c, conversation)| {
            conversation.messages.iter().find_map(|m| {
                (m.outbound && m.id.is_some() && m.delivery == Some(DeliveryState::Delivered))
                    .then(|| (c, m.id.expect("an id"), m.at.map_or(0, |at| at.unix_secs)))
            })
        })
        .expect("a delivered message of ours");
    let peer = model.conversations[c].peer;
    assert!(!model.conversations[c].flags.receipts, "a stock store");
    let rows_before = session::rows(&model, c, Lang::En).len();
    let read_at = start - 30;
    let receipt = Lev::mark_with(Receipt {
        read_at: read_at as u32,
        ids: vec![id, [0xee; 32]],
    });
    let effects = update(
        &mut model,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [0xe1; 32],
            source: peer,
            title: Vec::new(),
            body: Vec::new(),
            fields: receipt.fields().to_vec(),
            verified: true,
        }),
    );
    assert!(
        matches!(effects.as_slice(), [UiEffect::Log(line)] if line.starts_with("RECEIPT dropped id=eeee")),
        "{effects:?}"
    );
    let want = u32::try_from(read_at.max(sent)).expect("u32");
    let rows = session::rows(&model, c, Lang::En);
    assert_eq!(rows.len(), rows_before, "a receipt is no row");
    let row = rows
        .iter()
        .find(|r| r.id == hex(&id))
        .expect("the row of the message read");
    assert_eq!((row.state.as_str(), row.read_at), ("read", i64::from(want)));
    assert!(rows.iter().filter(|r| r.state == "read").count() == 1);
    assert!(model.conversations[c].flags.receipts);
    store.write(&model.take_writes()).expect("written");
    drop(store);
    let store = loquax::store::Store::open_in(home.path(), me).expect("again");
    let restored = store.restore().expect("restore");
    let again = restored.iter().find(|r| r.peer == peer).expect("the peer");
    assert!(again.flags.receipts);
    assert_eq!(
        again
            .messages
            .iter()
            .find(|m| m.id == Some(id))
            .and_then(|m| m.delivery),
        Some(DeliveryState::Read { at: want })
    );
}

/// 1395 and 1462, against the ugly store: a device receipt (0x07) from a
/// peer turns the message of ours it names into two grey ticks on the
/// phone's rows, `delivered`, adds no row, and the Info says "on the
/// device" in the reader's language, with the time it came; the words
/// survive the store. Read receipts off do not hide it.
#[test]
fn a_device_receipt_shows_two_ticks_on_the_phone_and_survives_the_store() {
    use leviculum_lxmf::lev::{Lev, PropagationReceipt};
    use leviculum_ui::delivery::DeliveryState;
    use loquax::outbox::OutboxEvent;
    use loquax::ui::{update, UiEvent};
    use loquax::when::WallTime;
    use loquax_android::session;
    let home = tempfile::tempdir().expect("app dir");
    let identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    ugly_store::fill_store_of(home.path(), &identity);
    let me = loquax::engine::delivery_address(&identity).expect("address");
    let mut store = loquax::store::Store::open_in(home.path(), me).expect("the store");
    let mut model = session::known(home.path(), &store).expect("a model");
    let start = ugly_store::BASE + 3 * 86_400;
    update(
        &mut model,
        UiEvent::Clock(WallTime {
            unix_secs: start,
            offset_secs: 0,
        }),
    );
    model.restore(store.restore().expect("restore"));
    update(&mut model, UiEvent::ReadReceipts(false));
    let (c, id, key) = model
        .conversations
        .iter()
        .enumerate()
        .find_map(|(c, conversation)| {
            conversation.messages.iter().find_map(|m| {
                (m.outbound
                    && m.id.is_some()
                    && !matches!(
                        m.delivery,
                        Some(DeliveryState::Delivered | DeliveryState::Read { .. })
                    ))
                .then(|| (c, m.id.expect("an id"), m.activity))
            })
        })
        .expect("a message of ours not delivered yet");
    let peer = model.conversations[c].peer;
    let rows_before = session::rows(&model, c, Lang::En).len();
    let receipt = Lev {
        propagation_receipt: Some(PropagationReceipt {
            received_at: (start - 60) as u32,
            ids: vec![id],
        }),
        ..Lev::default()
    };
    update(
        &mut model,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [0xe7; 32],
            source: peer,
            title: Vec::new(),
            body: Vec::new(),
            fields: receipt.fields().to_vec(),
            verified: true,
        }),
    );
    // The words carry when this device learned it, the session's clock.
    let at = WallTime {
        unix_secs: start,
        offset_secs: 0,
    }
    .hh_mm();
    let rows = session::rows(&model, c, Lang::En);
    assert_eq!(rows.len(), rows_before, "a receipt is no row");
    let row = rows.iter().find(|r| r.id == hex(&id)).expect("the row");
    assert_eq!(row.state, "delivered");
    let state_words = |model: &loquax::ui::UiModel<_>, lang: Lang| {
        loquax::ui::info::message_info(model, peer, key)
            .expect("its info")
            .rows
            .iter()
            .map(|row| row.resolve(lang))
            .find(|(label, _)| label == "Status")
            .map(|(_, value)| value)
            .expect("a status row")
    };
    let who = model.names.label(&peer, Lang::En);
    assert_eq!(state_words(&model, Lang::En), format!("On {who}'s device"));
    assert_eq!(
        state_words(&model, Lang::De),
        format!("Auf dem Gerät von {who}")
    );
    let on_device = |model: &loquax::ui::UiModel<_>| {
        loquax::ui::info::message_info(model, peer, key)
            .expect("its info")
            .rows
            .iter()
            .map(|row| row.resolve(Lang::En))
            .find(|(label, _)| label == "On the device")
            .map(|(_, value)| value)
    };
    assert!(
        on_device(&model).is_some_and(|value| value.ends_with(&at)),
        "{:?}",
        on_device(&model)
    );
    store.write(&model.take_writes()).expect("written");
    drop(store);
    let store = loquax::store::Store::open_in(home.path(), me).expect("again");
    let mut again = session::known(home.path(), &store).expect("a model");
    update(
        &mut again,
        UiEvent::Clock(WallTime {
            unix_secs: start,
            offset_secs: 0,
        }),
    );
    again.restore(store.restore().expect("restore"));
    let key = again
        .conversations
        .iter()
        .flat_map(|c| c.messages.iter())
        .find(|m| m.id == Some(id))
        .map(|m| m.activity)
        .expect("ours, restored");
    let restored = loquax::ui::info::message_info(&again, peer, key)
        .expect("its info")
        .rows
        .iter()
        .map(|row| row.resolve(Lang::En))
        .find(|(label, _)| label == "Status")
        .map(|(_, value)| value);
    assert_eq!(restored, Some(format!("On {who}'s device")));
    assert!(
        on_device(&again).is_some_and(|value| value.ends_with(&at)),
        "{:?}",
        on_device(&again)
    );
}

/// 1409 end to end: the phone sends through its home to a stock recipient
/// that is not there; the home propagates it to its lnpnd, which announces
/// `/lev/status`. The stock Python recipient comes up and collects, and
/// sends no receipt of its own. The home's next sync round asks the node
/// over a link it never identifies, archives the node's `collected`, and
/// the phone's row moves from the mailbox's tick to the arrived rung (1462:
/// still one tick, the recipient's identity collected it, which its own
/// mailbox might have done), its Info saying "Collected from the
/// propagation network", which only the node's word gives (a proof would
/// say "Arrived"). The phone, a client of its home, asks nothing itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_message_collected_at_the_node_shows_collected_on_the_phone() {
    let recipient_peer = python_peer::PythonPeer::start();
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start_full(
        listener,
        &instance,
        rns.path(),
        home.path(),
        Some(recipient_peer.rns_port),
        Some((8, 10_000)),
    )
    .await;
    let pn = node.pn.as_ref().expect("the home's node");
    let pn_hash = pn.hex();

    // The recipient's key reaches the home by its contact code; the
    // recipient itself is not up, so the direct leg finds nobody.
    let recipient = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let recipient_code = loquax::lxma::encode(&recipient.public_key_bytes()).expect("code");
    let recipient_address = loquax::address::to_hex(
        &loquax::home::lxmf_address(&recipient.public_key_bytes()).expect("address"),
    );
    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    let body = "abgeholt am Knoten";
    let peer = recipient_address.clone();
    let session = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        session
            .request(Request::AddContact {
                lxma: recipient_code,
                name: Some("Anna".to_string()),
            })
            .expect("added");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while session.keys_told() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the home node never took the key"
            );
            let _ = session.next_event(Duration::from_millis(200));
        }
        session
            .request(Request::Send {
                peer: peer.clone(),
                body: body.to_string(),
            })
            .expect("send");
        wait_for(
            &session,
            schema::Query::Messages { peer },
            60,
            |m: &schema::Messages| m.messages.last().is_some_and(|b| !b.id.is_empty()),
        );
        session
    })
    .await
    .expect("the send");

    // The node takes it once the direct leg ended.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);
    loop {
        match pn.events.try_recv() {
            Ok(lnpnd::engine::EngineEvent::Accepted { .. }) => break,
            Ok(_) => {}
            Err(_) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the node never took the message"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
    let row_peer = recipient_address.clone();
    let row_state = move |session: &Session| -> (String, String) {
        let messages: schema::Messages = rmp_serde::from_slice(
            &session
                .query(schema::Query::Messages {
                    peer: row_peer.clone(),
                })
                .expect("messages"),
        )
        .expect("decode");
        let bubble = messages
            .messages
            .iter()
            .find(|m| m.body == body)
            .expect("the bubble");
        (bubble.state.clone(), bubble.id.clone())
    };
    let (before, _) = row_state(&session);
    assert_eq!(before, "mailbox", "nobody collected it yet");
    let row_state = std::sync::Arc::new(row_state);
    let row_state_later = std::sync::Arc::clone(&row_state);

    // The stock recipient comes up and collects from the node.
    recipient_peer.rpc(
        "lxmf_init",
        serde_json::json!({
            "identity_prv": hex(&recipient.private_key_bytes().expect("private key")),
        }),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while let Err(error) = recipient_peer.try_rpc(
        "lxmf_set_propagation_node",
        serde_json::json!({ "dest_hash": pn_hash }),
    ) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the recipient never heard the propagation node: {error}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    recipient_peer.rpc("lxmf_request_from_propagation_node", serde_json::json!({}));
    let content = hex(body.as_bytes());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !recipient_peer
        .received()
        .iter()
        .any(|m| m.get("content").and_then(|v| v.as_str()) == Some(content.as_str()))
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the recipient never collected it"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The home asks at its next round; the phone's row moves.
    let (session, id) = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        loop {
            let (state, id) = row_state_later(&session);
            if state == "arrived" {
                break (session, id);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the row never moved past {state}"
            );
            let _ = session.next_event(Duration::from_millis(200));
        }
    })
    .await
    .expect("the row");
    let session = tokio::task::spawn_blocking(move || {
        let messages: schema::Messages = rmp_serde::from_slice(
            &session
                .query(schema::Query::Messages {
                    peer: loquax::address::to_hex(
                        &loquax::home::lxmf_address(&recipient.public_key_bytes())
                            .expect("address"),
                    ),
                })
                .expect("messages"),
        )
        .expect("decode");
        let bubble = messages
            .messages
            .iter()
            .find(|m| m.id == id)
            .expect("the bubble");
        // The phone's language is the session's, English here; the German
        // words are the catalogue's, tested beside the model.
        let info: schema::Info = rmp_serde::from_slice(
            &session
                .query(schema::Query::Info {
                    peer: loquax::address::to_hex(
                        &loquax::home::lxmf_address(&recipient.public_key_bytes())
                            .expect("address"),
                    ),
                    key: bubble.key,
                })
                .expect("info"),
        )
        .expect("decode");
        assert!(
            info.rows.iter().any(|row| row
                .value
                .starts_with("Collected from the propagation network at ")),
            "{info:?}"
        );
        session
    })
    .await
    .expect("the info");
    tokio::task::spawn_blocking(move || session.stop())
        .await
        .expect("stopped");
    node.stop().await;
}

/// The photo fixture (1220, `loquax/tests/fixtures/make-photo.py`): 1800 x 2400
/// as shown, stored turned with EXIF orientation 6, a GPS position and a
/// camera model in its EXIF, a red block top left as it shows.
const PHOTO: &[u8] = include_bytes!("../../loquax/tests/fixtures/photo-portrait.jpg");

/// The image field of the fields map Python's router handed its delivery
/// callback (`umsgpack.packb(message.fields)`), as our codec reads it.
fn python_photo(message: &serde_json::Value) -> Option<(String, Vec<u8>)> {
    let packed = message.get("fields")?.as_str()?;
    let packed: Vec<u8> = (0..packed.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&packed[i..i + 2], 16).ok())
        .collect::<Option<_>>()?;
    use leviculum_lxmf::msgpack;
    let mut at = 0;
    let entries = msgpack::map_len(&packed, &mut at).ok()?;
    for _ in 0..entries {
        let key = msgpack::read_int(&packed, &mut at).ok()?;
        let start = at;
        msgpack::skip(&packed, &mut at).ok()?;
        if key == leviculum_lxmf::constants::FIELD_IMAGE {
            let fields = [(key, packed[start..at].to_vec())];
            return loquax::ui::photos::photo_parts(&fields)
                .map(|(format, data)| (format, data.to_vec()));
        }
    }
    None
}

/// Whether the top left of `rgba` (`width` wide) is the fixture's red
/// block: the picture is upright.
fn red_corner(rgba: &[u8], width: u32) -> bool {
    let at = ((8 * width + 8) * 4) as usize;
    rgba.get(at..at + 3)
        .is_some_and(|p| p[0] > 200 && p[1] < 80 && p[2] < 80)
}

/// Photos both ways between the phone and the Python reference (1220).
/// A photo Python sends as Sideband does, `fields[FIELD_IMAGE] = ["jpg",
/// bytes]`, shows on the phone: its row says photo, its size upright and
/// its words, and the library draws it upright. A photo the phone sends
/// at each of the three sizes reaches Python's router as that field with
/// the size's longest edge, upright, without the EXIF's place or camera;
/// past one packet it travels to the phone's mailbox as a request
/// resource (`ReticulumNode::send_request_awaited`). A picture that is
/// none is refused with the reason, and nothing goes.
#[tokio::test(flavor = "multi_thread")]
async fn a_photo_travels_both_ways_with_python_lxmf_at_each_size() {
    let peer = python_peer::PythonPeer::start();
    let python = peer.lxmf_init();
    let phone = tempfile::tempdir().expect("app dir");
    let mailbox = phone.path().join(loquax_android::mailbox::DIR);
    std::fs::create_dir_all(&mailbox).expect("mailbox dir");
    std::fs::write(
        mailbox.join(loquax_android::mailbox::SERVERS_FILE),
        format!("127.0.0.1:{}\n", peer.rns_port),
    )
    .expect("servers file");
    let first = Start {
        dir: phone.path().display().to_string(),
        here: true,
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"))
    })
    .await
    .expect("the session");
    let address = loquax::address::to_hex(&session.address());
    peer.lxmf_announce();

    // Python to the phone, as Sideband sends one.
    let mut field = Vec::new();
    leviculum_lxmf::msgpack::map(&mut field, 1);
    leviculum_lxmf::msgpack::int(&mut field, leviculum_lxmf::constants::FIELD_IMAGE);
    leviculum_lxmf::msgpack::array(&mut field, 2);
    leviculum_lxmf::msgpack::string(&mut field, "jpg");
    leviculum_lxmf::msgpack::bin(&mut field, PHOTO);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let message_hash = loop {
        match peer.try_rpc(
            "lxmf_send",
            serde_json::json!({
                "dest_hash": address,
                "method": "direct",
                "content": "",
                "fields": hex(&field),
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
    let shown_python = python.clone();
    let session = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let row = loop {
            let messages: schema::Messages = rmp_serde::from_slice(
                &session
                    .query(schema::Query::Messages {
                        peer: shown_python.clone(),
                    })
                    .expect("a query"),
            )
            .expect("decode");
            if let Some(row) = messages.messages.iter().find(|m| !m.photo.is_empty()) {
                break row.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the screens never showed the photo: {messages:?}"
            );
            session.next_event(Duration::from_millis(200));
        };
        assert_eq!(row.photo, "jpg");
        assert_eq!(row.photo_bytes as usize, PHOTO.len());
        assert_eq!((row.photo_w, row.photo_h), (1800, 2400), "upright");
        assert_eq!(row.photo_words, "Photo, 361 KB");
        assert_eq!(row.body, "");
        let list: schema::Conversations = rmp_serde::from_slice(
            &session
                .query(schema::Query::Conversations { search: None })
                .expect("a query"),
        )
        .expect("decode");
        let listed = list
            .conversations
            .iter()
            .find(|c| c.peer == shown_python)
            .expect("listed");
        assert_eq!(
            (listed.last.as_str(), listed.kind.as_str()),
            ("Photo", "photo")
        );
        let pixels: schema::PhotoPixels = rmp_serde::from_slice(
            &session
                .query(schema::Query::Photo {
                    peer: shown_python.clone(),
                    key: row.key,
                    edge: 480,
                })
                .expect("its pixels"),
        )
        .expect("decode");
        assert_eq!((pixels.width, pixels.height), (360, 480));
        assert_eq!(pixels.rgba.0.len(), 360 * 480 * 4);
        assert!(red_corner(&pixels.rgba.0, pixels.width), "drawn upright");
        session
    })
    .await
    .expect("the photo on the phone");

    // The phone to Python, at each size.
    let mut session = session;
    for (size, edge) in [("small", 800), ("phone", 1600), ("original", 2400)] {
        let to = python.clone();
        let (back, before) = (session, peer.received().len());
        session = tokio::task::spawn_blocking(move || {
            let chosen: schema::PhotoSizeChoice = rmp_serde::from_slice(
                &back
                    .query(schema::Query::PhotoSize {
                        set: Some(size.to_string()),
                    })
                    .expect("set"),
            )
            .expect("decode");
            assert_eq!(chosen.size, size);
            back.request(Request::Photo {
                peer: to,
                data: schema::Blob(PHOTO.to_vec()),
            })
            .unwrap_or_else(|error| panic!("{size}: {error}"));
            back
        })
        .await
        .expect("sent");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let (format, data) = loop {
            let got = peer.received();
            if let Some(photo) = got.iter().skip(before).find_map(python_photo) {
                break photo;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{size}: Python never received the photo: {:?}",
                got.len()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        assert_eq!(format, "jpg", "{size}");
        let (w, h) = loquax::photo::dimensions(&data).expect("Python got a picture");
        assert_eq!(w.max(h), edge, "{size}: {w}x{h}");
        assert!(h > w, "{size}: upright, portrait: {w}x{h}");
        let pixels = loquax::photo::fit(&data, 400).expect("decodes");
        assert!(red_corner(&pixels.rgba, pixels.width), "{size}: upright");
        for secret in [&b"Exif"[..], b"Pixel 8", b"GPS"] {
            assert!(
                !data.windows(secret.len()).any(|w| w == secret),
                "{size}: carries {:?}",
                String::from_utf8_lossy(secret)
            );
        }
    }

    // Negative: no picture, nothing sent, the reason said.
    let to = python.clone();
    let before = peer.received().len();
    let session = tokio::task::spawn_blocking(move || {
        let refused = session.request(Request::Photo {
            peer: to,
            data: schema::Blob(b"%PDF-1.7 a document".to_vec()),
        });
        assert!(
            refused.as_ref().is_err_and(|e| e.contains("not a picture")),
            "{refused:?}"
        );
        session
    })
    .await
    .expect("refused");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(peer.received().len(), before, "nothing went");
    tokio::task::spawn_blocking(move || session.stop())
        .await
        .expect("stopped");
}

/// The file attachments field of the fields map Python's router handed its
/// delivery callback, raw: the entries as the reference packed them.
fn python_files(message: &serde_json::Value) -> Option<Vec<u8>> {
    let packed = message.get("fields")?.as_str()?;
    let packed: Vec<u8> = (0..packed.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&packed[i..i + 2], 16).ok())
        .collect::<Option<_>>()?;
    use leviculum_lxmf::msgpack;
    let mut at = 0;
    let entries = msgpack::map_len(&packed, &mut at).ok()?;
    for _ in 0..entries {
        let key = msgpack::read_int(&packed, &mut at).ok()?;
        let start = at;
        msgpack::skip(&packed, &mut at).ok()?;
        if key == leviculum_lxmf::constants::FIELD_FILE_ATTACHMENTS {
            return Some(packed[start..at].to_vec());
        }
    }
    None
}

/// Files both ways between the phone and the Python reference (1221).
/// Files Python sends as Sideband does, `fields[FIELD_FILE_ATTACHMENTS] =
/// [[name, bytes], …]`, show on the phone: a row each with its name, size
/// and words, the list says the first name; saved for the open intent
/// they land under the app's files with their bytes, a hostile name made
/// safe. A file the phone sends reaches Python's router as that field,
/// the name a str as Columba writes it and the bytes unchanged, past one
/// packet through the phone's mailbox as a request resource. A file past
/// the 990 KB a receiver takes is refused with both sizes, and nothing
/// goes.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_travels_both_ways_with_python_lxmf() {
    let peer = python_peer::PythonPeer::start();
    let python = peer.lxmf_init();
    let phone = tempfile::tempdir().expect("app dir");
    let mailbox = phone.path().join(loquax_android::mailbox::DIR);
    std::fs::create_dir_all(&mailbox).expect("mailbox dir");
    std::fs::write(
        mailbox.join(loquax_android::mailbox::SERVERS_FILE),
        format!("127.0.0.1:{}\n", peer.rns_port),
    )
    .expect("servers file");
    let first = Start {
        dir: phone.path().display().to_string(),
        here: true,
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"))
    })
    .await
    .expect("the session");
    let address = loquax::address::to_hex(&session.address());
    peer.lxmf_announce();

    // Python to the phone, as Sideband sends files: two, one of them
    // named to climb out of the folder it is saved in.
    let report: Vec<u8> = (0..200_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let mut field = Vec::new();
    leviculum_lxmf::msgpack::map(&mut field, 1);
    leviculum_lxmf::msgpack::int(
        &mut field,
        leviculum_lxmf::constants::FIELD_FILE_ATTACHMENTS,
    );
    leviculum_lxmf::msgpack::array(&mut field, 2);
    leviculum_lxmf::msgpack::array(&mut field, 2);
    leviculum_lxmf::msgpack::string(&mut field, "Protokoll.pdf");
    leviculum_lxmf::msgpack::bin(&mut field, &report);
    leviculum_lxmf::msgpack::array(&mut field, 2);
    leviculum_lxmf::msgpack::string(&mut field, "../../evil.sh");
    leviculum_lxmf::msgpack::bin(&mut field, b"#!/bin/sh\n");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let message_hash = loop {
        match peer.try_rpc(
            "lxmf_send",
            serde_json::json!({
                "dest_hash": address,
                "method": "direct",
                "content": "",
                "fields": hex(&field),
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
    let shown_python = python.clone();
    let home = phone.path().to_path_buf();
    let session = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let row = loop {
            let messages: schema::Messages = rmp_serde::from_slice(
                &session
                    .query(schema::Query::Messages {
                        peer: shown_python.clone(),
                    })
                    .expect("a query"),
            )
            .expect("decode");
            if let Some(row) = messages.messages.iter().find(|m| !m.files.is_empty()) {
                break row.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the screens never showed the files: {messages:?}"
            );
            session.next_event(Duration::from_millis(200));
        };
        assert_eq!(row.body, "");
        let shown: Vec<(&str, u32, &str, &str)> = row
            .files
            .iter()
            .map(|f| (f.name.as_str(), f.bytes, f.size.as_str(), f.words.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "Protokoll.pdf",
                    200_000,
                    "200 KB",
                    "File Protokoll.pdf, 200 KB"
                ),
                ("_.._evil.sh", 10, "10 B", "File _.._evil.sh, 10 B"),
            ]
        );
        let list: schema::Conversations = rmp_serde::from_slice(
            &session
                .query(schema::Query::Conversations { search: None })
                .expect("a query"),
        )
        .expect("decode");
        let listed = list
            .conversations
            .iter()
            .find(|c| c.peer == shown_python)
            .expect("listed");
        assert_eq!(
            (listed.last.as_str(), listed.kind.as_str()),
            ("Protokoll.pdf", "file")
        );
        let mut kept = Vec::new();
        for (index, want) in [(0u32, &report[..]), (1, &b"#!/bin/sh\n"[..])] {
            let saved: schema::SavedFile = rmp_serde::from_slice(
                &session
                    .query(schema::Query::SaveFile {
                        peer: shown_python.clone(),
                        key: row.key,
                        index,
                    })
                    .expect("saved"),
            )
            .expect("decode");
            let path = std::path::PathBuf::from(&saved.path);
            assert!(
                path.starts_with(home.join(loquax::file_save::DIR)),
                "under the app's files: {}",
                saved.path
            );
            assert_eq!(std::fs::read(&path).expect("its bytes"), want);
            assert_eq!(
                path.file_name().and_then(|n| n.to_str()),
                Some(saved.name.as_str())
            );
            kept.push(path);
        }
        assert!(session
            .query(schema::Query::SaveFile {
                peer: shown_python.clone(),
                key: row.key,
                index: 2,
            })
            .is_err());
        // 1662: a saved file goes with its message. The reader exported
        // the report out of the app (the share sheet's "Save to
        // Downloads"); deleting the message takes both saved files from
        // the app's storage and leaves the exported copy.
        let downloads = tempfile::tempdir().expect("downloads");
        let exported = downloads.path().join("Protokoll.pdf");
        std::fs::copy(&kept[0], &exported).expect("exported");
        session
            .request(Request::Delete {
                peer: shown_python.clone(),
                keys: vec![row.key],
            })
            .expect("deleted");
        for path in &kept {
            assert!(!path.exists(), "still saved: {}", path.display());
            assert!(
                !path.parent().is_some_and(Path::exists),
                "its folder stays: {}",
                path.display()
            );
        }
        assert_eq!(std::fs::read(&exported).expect("exported"), report);
        session
    })
    .await
    .expect("the files on the phone");

    // The phone to Python.
    let notes: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(40_503) >> 8) as u8)
        .collect();
    let to = python.clone();
    let before = peer.received().len();
    let sent = notes.clone();
    let session = tokio::task::spawn_blocking(move || {
        session
            .request(Request::File {
                peer: to,
                name: "Grüße.txt".to_string(),
                data: schema::Blob(sent),
                size: 0,
            })
            .unwrap_or_else(|error| panic!("{error}"));
        session
    })
    .await
    .expect("sent");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let value = loop {
        let got = peer.received();
        if let Some(value) = got.iter().skip(before).find_map(python_files) {
            break value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Python never received the file: {:?}",
            got.len()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    use leviculum_lxmf::msgpack;
    let mut at = 0;
    assert_eq!(msgpack::array_len(&value, &mut at), Ok(1), "one file");
    assert_eq!(msgpack::array_len(&value, &mut at), Ok(2), "[name, bytes]");
    assert_eq!(msgpack::read_str(&value, &mut at), Ok("Grüße.txt"), "a str");
    assert!(
        msgpack::read_bin(&value, &mut at) == Ok(&notes[..]),
        "the bytes"
    );

    // Negative: past what a receiver takes, nothing sent, both sizes said.
    let to = python.clone();
    let python_to = python.clone();
    let before = peer.received().len();
    let session = tokio::task::spawn_blocking(move || {
        let refused = session.request(Request::File {
            peer: to,
            name: "film.mp4".to_string(),
            data: schema::Blob(vec![0; loquax::ui::files::LARGEST + 1]),
            size: 0,
        });
        assert_eq!(
            refused,
            Err("This file is 991 KB. A message carries at most 990 KB.".to_string())
        );
        // As Java hands one it stopped reading: the first megabyte and
        // the count.
        let refused = session.request(Request::File {
            peer: python_to.clone(),
            name: "film.mp4".to_string(),
            data: schema::Blob(vec![0; 1_000_000]),
            size: 48_300_000,
        });
        assert_eq!(
            refused,
            Err("This file is 48.3 MB. A message carries at most 990 KB.".to_string())
        );
        session
    })
    .await
    .expect("refused");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(peer.received().len(), before, "nothing went");
    tokio::task::spawn_blocking(move || session.stop())
        .await
        .expect("stopped");
}

/// The audio field of a message Python's router received: its mode and
/// bytes.
fn python_audio(message: &serde_json::Value) -> Option<(u8, Vec<u8>)> {
    let packed = message.get("fields")?.as_str()?;
    let packed: Vec<u8> = (0..packed.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&packed[i..i + 2], 16).ok())
        .collect::<Option<_>>()?;
    use leviculum_lxmf::msgpack;
    let mut at = 0;
    let entries = msgpack::map_len(&packed, &mut at).ok()?;
    for _ in 0..entries {
        let key = msgpack::read_int(&packed, &mut at).ok()?;
        let start = at;
        msgpack::skip(&packed, &mut at).ok()?;
        if key == leviculum_lxmf::constants::FIELD_AUDIO {
            let mut at = start;
            msgpack::array_len(&packed, &mut at).ok()?;
            let mode = u8::try_from(msgpack::read_uint(&packed, &mut at).ok()?).ok()?;
            return Some((mode, msgpack::read_bin(&packed, &mut at).ok()?.to_vec()));
        }
    }
    None
}

/// One of the reference codec tools (opus-tools' opusenc and opusdec,
/// codec2's c2enc and c2dec: `sudo apt install opus-tools codec2`, listed
/// in scripts/install-ci.sh), run to its end.
fn codec_tool(tool: &str, args: &[&str]) {
    let status = std::process::Command::new(tool)
        .args(args)
        .status()
        .unwrap_or_else(|e| {
            panic!("{tool}: {e} (sudo apt install opus-tools codec2, scripts/install-ci.sh)")
        });
    assert!(status.success(), "{tool} {args:?}: {status}");
}

fn le_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|s| i16::from_le_bytes([s[0], s[1]]))
        .collect()
}

fn le_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// Sends `audio` as `[mode, bytes]` from Python to `address` and waits for
/// DELIVERED.
async fn python_sends_voice(peer: &python_peer::PythonPeer, address: &str, mode: u8, audio: &[u8]) {
    let mut field = Vec::new();
    leviculum_lxmf::msgpack::map(&mut field, 1);
    leviculum_lxmf::msgpack::int(&mut field, leviculum_lxmf::constants::FIELD_AUDIO);
    leviculum_lxmf::msgpack::array(&mut field, 2);
    leviculum_lxmf::msgpack::uint(&mut field, u64::from(mode));
    leviculum_lxmf::msgpack::bin(&mut field, audio);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let message_hash = loop {
        match peer.try_rpc(
            "lxmf_send",
            serde_json::json!({
                "dest_hash": address,
                "method": "direct",
                "content": "",
                "fields": hex(&field),
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
}

/// Voice messages both ways between the phone and the Python reference
/// (1222), each side's sound made and judged by the reference codecs.
/// Python sends Codec2 2400 as Sideband does on a slow link (frames from
/// c2enc, no header) and Opus in Ogg as Columba and Sideband do (from
/// opusenc): the phone's rows say a voice message with its length, its
/// button plays and pauses, and its sound is the reference decoder's
/// (c2dec, opusdec) within what two implementations of one codec differ
/// by. A recording the phone sends reaches Python's router as
/// `[AM_OPUS_OGG, bytes]`, and opusdec plays it back as what was
/// recorded. A tap on the microphone, shorter than 0.6 s, sends nothing
/// and says to hold it.
#[tokio::test(flavor = "multi_thread")]
async fn a_voice_message_travels_both_ways_with_python_lxmf() {
    use loquax::ui::voice::{AM_CODEC2_2400, AM_OPUS_OGG};
    use loquax::voice::{likeness, test_speech};
    let peer = python_peer::PythonPeer::start();
    let python = peer.lxmf_init();
    let phone = tempfile::tempdir().expect("app dir");
    let work = tempfile::tempdir().expect("a work dir");
    let file = |name: &str| work.path().join(name).display().to_string();
    let mailbox = phone.path().join(loquax_android::mailbox::DIR);
    std::fs::create_dir_all(&mailbox).expect("mailbox dir");
    std::fs::write(
        mailbox.join(loquax_android::mailbox::SERVERS_FILE),
        format!("127.0.0.1:{}\n", peer.rns_port),
    )
    .expect("servers file");
    let first = Start {
        dir: phone.path().display().to_string(),
        here: true,
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"))
    })
    .await
    .expect("the session");
    let address = loquax::address::to_hex(&session.address());
    peer.lxmf_announce();

    // The reference codecs make Python's two messages and say what they
    // sound like.
    let speech8 = test_speech(8_000, 2_000);
    std::fs::write(file("in8.raw"), le_bytes(&speech8)).expect("raw");
    codec_tool("c2enc", &["2400", &file("in8.raw"), &file("c2.bit")]);
    codec_tool("c2dec", &["2400", &file("c2.bit"), &file("c2.raw")]);
    let codec2 = std::fs::read(file("c2.bit")).expect("c2enc's frames");
    assert_eq!(codec2.len(), 600, "six bytes a 20 ms frame, no header");
    let c2_reference = le_samples(&std::fs::read(file("c2.raw")).expect("c2dec's"));
    let speech16 = test_speech(16_000, 2_000);
    std::fs::write(file("in16.raw"), le_bytes(&speech16)).expect("raw");
    codec_tool(
        "opusenc",
        &[
            "--quiet",
            "--speech",
            "--bitrate",
            "12",
            "--raw",
            "--raw-rate",
            "16000",
            "--raw-chan",
            "1",
            &file("in16.raw"),
            &file("ref.opus"),
        ],
    );
    codec_tool(
        "opusdec",
        &[
            "--quiet",
            "--rate",
            "24000",
            &file("ref.opus"),
            &file("ref.raw"),
        ],
    );
    let opus = std::fs::read(file("ref.opus")).expect("opusenc's file");
    let opus_reference = le_samples(&std::fs::read(file("ref.raw")).expect("opusdec's"));

    python_sends_voice(&peer, &address, AM_CODEC2_2400, &codec2).await;
    python_sends_voice(&peer, &address, AM_OPUS_OGG, &opus).await;
    let shown_python = python.clone();
    let session = tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let messages = |session: &Session| -> schema::Messages {
            rmp_serde::from_slice(
                &session
                    .query(schema::Query::Messages {
                        peer: shown_python.clone(),
                    })
                    .expect("a query"),
            )
            .expect("decode")
        };
        let rows = loop {
            let rows: Vec<schema::MessageRow> = messages(&session)
                .messages
                .into_iter()
                .filter(|m| m.voice_mode != 0)
                .collect();
            if rows.len() == 2 {
                break rows;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the screens never showed both voice messages: {rows:?}"
            );
            session.next_event(Duration::from_millis(200));
        };
        let shown: Vec<(u32, u32, &str, bool, &str)> = rows
            .iter()
            .map(|r| {
                (
                    r.voice_mode,
                    r.voice_ms,
                    r.voice_duration.as_str(),
                    r.voice_playable,
                    r.voice_words.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (8, 2_000, "0:02", true, "Voice message, 0:02"),
                (0x10, 2_000, "0:02", true, "Voice message, 0:02"),
            ]
        );
        assert!(rows[0].voice_levels.0.is_empty(), "Codec2: an even bar");
        assert_eq!(rows[1].voice_levels.0.len(), loquax::ui::voice::BARS);
        let list: schema::Conversations = rmp_serde::from_slice(
            &session
                .query(schema::Query::Conversations { search: None })
                .expect("a query"),
        )
        .expect("decode");
        let listed = list
            .conversations
            .iter()
            .find(|c| c.peer == shown_python)
            .expect("listed");
        assert_eq!(
            (listed.last.as_str(), listed.kind.as_str()),
            ("Voice message, 0:02", "voice")
        );

        // The sound is the reference decoder's.
        let sound = |key: i64| -> schema::VoicePcm {
            rmp_serde::from_slice(
                &session
                    .query(schema::Query::Voice {
                        peer: shown_python.clone(),
                        key,
                    })
                    .expect("decoded"),
            )
            .expect("decode")
        };
        let c2 = sound(rows[0].key);
        assert_eq!(c2.rate, 8_000);
        let c2 = le_samples(&c2.pcm.0);
        assert_eq!(c2.len(), c2_reference.len());
        let like = likeness(&c2_reference, &c2, 8);
        assert!(like > 0.9, "Codec2 against c2dec: {like}");
        let ours = sound(rows[1].key);
        assert_eq!(ours.rate, 24_000);
        let ours = le_samples(&ours.pcm.0);
        let like = likeness(&opus_reference, &ours, 24);
        assert!(like > 0.95, "Opus against opusdec: {like}");
        assert!(
            ours.len().abs_diff(opus_reference.len()) <= 24,
            "{} against {}",
            ours.len(),
            opus_reference.len()
        );

        // The button plays and pauses.
        let key = rows[1].key;
        let play = |on: bool| {
            session
                .request(Request::PlayVoice {
                    peer: shown_python.clone(),
                    key,
                    on,
                })
                .unwrap_or_else(|error| panic!("{error}"));
            messages(&session)
                .messages
                .into_iter()
                .find(|m| m.key == key)
                .map(|m| m.voice_playing)
        };
        assert_eq!(play(true), Some(true));
        assert_eq!(play(false), Some(false));
        session
    })
    .await
    .expect("the voice messages on the phone");

    // The phone to Python: three seconds at the rate the microphone gave.
    let recorded = test_speech(16_000, 3_000);
    let to = python.clone();
    let before = peer.received().len();
    let pcm = le_bytes(&recorded);
    let session = tokio::task::spawn_blocking(move || {
        session
            .request(Request::Voice {
                peer: to,
                pcm: schema::Blob(pcm),
                rate: 16_000,
            })
            .unwrap_or_else(|error| panic!("{error}"));
        session
    })
    .await
    .expect("sent");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let (mode, ogg) = loop {
        let got = peer.received();
        if let Some(value) = got.iter().skip(before).find_map(python_audio) {
            break value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Python never received the voice message: {:?}",
            got.len()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(mode, AM_OPUS_OGG, "the mode every receiver plays");
    assert!(ogg.starts_with(b"OggS"), "Opus in Ogg");
    std::fs::write(file("ours.opus"), &ogg).expect("written");
    codec_tool(
        "opusdec",
        &[
            "--quiet",
            "--rate",
            "16000",
            &file("ours.opus"),
            &file("ours.raw"),
        ],
    );
    let heard = le_samples(&std::fs::read(file("ours.raw")).expect("opusdec's"));
    assert_eq!(
        heard.len(),
        recorded.len(),
        "as long as recorded, no padding"
    );
    let like = likeness(&recorded, &heard, 80);
    assert!(like > 0.8, "opusdec's against what was recorded: {like}");

    // Negative: a tap on the microphone sends nothing and says to hold it.
    let to = python.clone();
    let before = peer.received().len();
    let session = tokio::task::spawn_blocking(move || {
        let refused = session.request(Request::Voice {
            peer: to,
            pcm: schema::Blob(le_bytes(&test_speech(16_000, 300))),
            rate: 16_000,
        });
        assert_eq!(
            refused,
            Err("Hold the microphone to record, release to send.".to_string())
        );
        session
    })
    .await
    .expect("refused");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(peer.received().len(), before, "nothing went");
    tokio::task::spawn_blocking(move || session.stop())
        .await
        .expect("stopped");
}

/// Lew's phone, 2026-11-01 (1268): a photo without words drew the picture
/// and an empty bubble under it. The facade's rows carry the scene's
/// words: a photo, a file or a voice message alone has an empty body
/// (the app draws no text row then), a body of blanks is none, and a
/// photo with words keeps them.
#[test]
fn media_without_words_has_an_empty_body() {
    use loquax::outbox::OutboxEvent;
    use loquax::ui::{update, UiEvent, UiModel};
    use loquax_android::session::{self, Line};
    let anna = ugly_store::peer(91);
    let mut model = UiModel::<Line>::new(&[anna]);
    let carrying = |n: u8, body: &str, fields| {
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: 0,
            message_id: [n; 32],
            source: anna,
            title: Vec::new(),
            body: body.as_bytes().to_vec(),
            fields,
            verified: true,
        })
    };
    let photo = || loquax::ui::photos::photo_fields("jpg", vec![7; 1_000]);
    update(&mut model, carrying(1, "", photo()));
    update(&mut model, carrying(2, " \n", photo()));
    update(
        &mut model,
        carrying(3, "", loquax::ui::files::file_fields("a.txt", vec![7; 10])),
    );
    update(
        &mut model,
        carrying(
            4,
            "",
            loquax::ui::voice::voice_fields(loquax::ui::voice::AM_OPUS_OGG, vec![7; 90]),
        ),
    );
    update(&mut model, carrying(5, "the view", photo()));
    let rows = session::rows(&model, 0, Lang::En);
    let shape: Vec<(&str, bool, usize, bool)> = rows
        .iter()
        .map(|r| {
            (
                r.body.as_str(),
                !r.photo.is_empty(),
                r.files.len(),
                r.voice_mode != 0,
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            ("", true, 0, false),
            ("", true, 0, false),
            ("", false, 1, false),
            ("", false, 0, true),
            ("the view", true, 0, false),
        ]
    );
}

/// 1276 on the phone, against a real home node: the microphone's gestures
/// and the recorder's levels go in as `record`, the bar's buttons as
/// `recording`, and `Query::Recording` answers the scene's recording bar
/// with what the recorder is to do, once.
///
/// - A press holds the bar up with a haptic tick and the recorder's
///   start; the levels arrive in the wave, newest at the right.
/// - Slid past 96 dp the hint says to release, and the release discards:
///   nothing is sent.
/// - A swipe up locks (a second tick), the release then does nothing;
///   pause stands the wave still as the whole recording, play and its end
///   swap the button, resume goes on, Send asks for the PCM. The PCM
///   handed over is one voice message, whose bubble's levels are the
///   recording's own track folded to 32 bars by peak.
/// - A feed at the floor says, after 2 s and not before, that nothing is
///   heard, offers the permission, and the dot stops pulsing; one heard
///   window takes it back.
/// - A voice message without a track of its own (1222's path) draws the
///   decoded sound's once `Query::Voice` decoded it.
#[tokio::test(flavor = "multi_thread")]
async fn a_recording_is_held_slid_away_locked_paused_and_sent_from_its_bar() {
    use loquax::ui::recording::{level_of, Fold, LevelTrack, LIVE_WINDOWS};
    use loquax::ui::voice::BARS;
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        let peer = "0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c".to_string();
        session
            .request(Request::Open { peer: peer.clone() })
            .expect("open");
        let record = |input: &str, now: i64, dx: i32, dy: i32, rms: u32| {
            session
                .request(Request::Record {
                    peer: peer.clone(),
                    input: input.to_string(),
                    now,
                    dx,
                    dy,
                    rms,
                })
                .unwrap_or_else(|error| panic!("{input}: {error}"));
        };
        let tap = |verb: &str| {
            session
                .request(Request::Recording {
                    peer: peer.clone(),
                    verb: verb.to_string(),
                })
                .unwrap_or_else(|error| panic!("{verb}: {error}"));
        };
        let bar = || -> schema::Recording {
            asked(&session, schema::Query::Recording { peer: peer.clone() })
        };
        let ids = |bar: &schema::Recording| -> Vec<String> {
            bar.buttons.iter().map(|b| b.id.clone()).collect()
        };
        let chat = || -> schema::Messages {
            asked(&session, schema::Query::Messages { peer: peer.clone() })
        };
        let voices = || -> Vec<schema::MessageRow> {
            chat()
                .messages
                .into_iter()
                .filter(|m| m.ours && m.voice_mode != 0)
                .collect()
        };
        // Windows of `rms` from `from` on, `n` of them, 50 ms apart; their
        // levels go into `track` too.
        let feed = |track: &mut LevelTrack, from: i64, n: i64, rms: &dyn Fn(i64) -> u32| {
            for w in 1..=n {
                let r = rms(w);
                record("level", from + w * 50, 0, 0, r);
                track.push_rms(u16::try_from(r).expect("16 bits"));
            }
            from + n * 50
        };

        // Held, then slid away.
        assert_eq!(bar().phase, "idle");
        assert!(!bar().shown);
        record("press", 10_000, 0, 0, 0);
        let held = bar();
        assert_eq!(held.effects, ["haptic_start", "start"]);
        assert_eq!(bar().effects, Vec::<String>::new(), "handed out once");
        assert_eq!((held.phase.as_str(), held.shown), ("held", true));
        assert!(held.pulsing && held.moving);
        assert_eq!(held.timer, "REC 00:00");
        assert_eq!(held.held, "Release to send");
        assert_eq!(held.cancel, "Slide left to cancel");
        assert_eq!(held.lock, "Lock the recording");
        assert!(!held.lock_closed);
        assert!(held.buttons.is_empty());
        assert_eq!(
            (
                held.cancel_dp,
                held.lock_dp,
                held.window_ms,
                held.longest_ms,
                held.wave_slots
            ),
            (96, 72, 50, 600_000, 60)
        );
        assert_eq!(chat().recording, "held");
        let mut track = LevelTrack::default();
        let now = feed(&mut track, 10_000, 20, &|w| 300 + 400 * (w as u32 % 5));
        let heard = bar();
        assert_eq!(heard.timer, "REC 00:01");
        assert_eq!(heard.wave.0.len(), 20);
        assert_eq!(heard.wave.0, track.levels());
        // The newest, at the right edge: the 20th window, 20 % 5 = 0.
        assert_eq!(heard.wave.0[19], level_of(300));
        assert_eq!(heard.wave.0[18], level_of(300 + 400 * 4));
        record("slide", 0, -60, 0, 0);
        let sliding = bar();
        assert_eq!((sliding.cancel_progress, sliding.armed), (62, false));
        record("slide", 0, -100, 5, 0);
        let armed = bar();
        assert!(armed.armed);
        assert_eq!(armed.cancel, "Release to cancel");
        record("release", now + 10, 0, 0, 0);
        let gone = bar();
        assert_eq!(gone.effects, ["discard"]);
        assert_eq!((gone.phase.as_str(), gone.shown), ("cancelled", false));
        assert_eq!(chat().recording, "");
        assert!(voices().is_empty(), "a slide away sends nothing");

        // Locked, paused, heard back, resumed and sent.
        record("press", 20_000, 0, 0, 0);
        assert_eq!(bar().effects, ["haptic_start", "start"]);
        let mut track = LevelTrack::default();
        let now = feed(&mut track, 20_000, 30, &|w| 200 + 1_100 * (w as u32 % 7));
        record("slide", 0, 0, -40, 0);
        assert_eq!(bar().lock_progress, 55);
        record("slide", 0, 3, -80, 0);
        let locked = bar();
        assert_eq!(locked.effects, ["haptic_lock"]);
        assert_eq!(locked.phase, "locked");
        assert!(locked.lock_closed);
        assert_eq!(locked.lock, "Recording locked");
        assert_eq!(ids(&locked), ["rec_discard", "rec_pause", "rec_send"]);
        assert_eq!((locked.cancel.as_str(), locked.held.as_str()), ("", ""));
        record("release", now + 5, 0, 0, 0);
        let still = bar();
        assert_eq!((still.phase.as_str(), still.effects.len()), ("locked", 0));
        let now = feed(&mut track, now, 20, &|w| 50 + 700 * (w as u32 % 3));
        tap("rec_pause");
        let paused = bar();
        assert_eq!(paused.effects, ["pause"]);
        assert_eq!(paused.phase, "paused");
        assert!(!paused.moving && !paused.pulsing);
        assert_eq!(
            ids(&paused),
            ["rec_discard", "rec_play", "rec_resume", "rec_send"]
        );
        assert_eq!(paused.wave.0, track.bars(LIVE_WINDOWS, Fold::Peak));
        assert_eq!(paused.timer, "REC 00:02");
        // A level while paused is no window of the recording.
        record("level", now + 50, 0, 0, 9_000);
        assert_eq!(bar().wave.0, track.bars(LIVE_WINDOWS, Fold::Peak));
        tap("rec_play");
        let playing = bar();
        assert_eq!(playing.effects, ["play"]);
        assert_eq!(
            ids(&playing),
            ["rec_discard", "rec_stop_play", "rec_resume", "rec_send"]
        );
        record("played", 0, 0, 0, 0);
        assert_eq!(
            ids(&bar()),
            ["rec_discard", "rec_play", "rec_resume", "rec_send"]
        );
        tap("rec_resume");
        let resumed = bar();
        assert_eq!(resumed.effects, ["resume"]);
        assert_eq!(resumed.phase, "locked");
        let now = feed(&mut track, now + 50, 20, &|w| 3_000 + 50 * w as u32);
        assert_eq!(bar().timer, "REC 00:03");
        let _ = now;
        tap("rec_send");
        let sent = bar();
        assert_eq!(sent.effects, ["finish"]);
        assert_eq!((sent.phase.as_str(), sent.shown), ("sent", false));
        // The finish effect: Java hands the PCM over, as 1222 did.
        session
            .request(Request::Voice {
                peer: peer.clone(),
                pcm: schema::Blob(le_bytes(&loquax::voice::test_speech(16_000, 3_500))),
                rate: 16_000,
            })
            .unwrap_or_else(|error| panic!("{error}"));
        let ours = voices();
        assert_eq!(ours.len(), 1, "one voice message");
        assert_eq!(ours[0].voice_levels.0, track.bars(BARS, Fold::Peak));
        assert_eq!(ours[0].voice_levels.0.len(), BARS);
        // Decoding it to play keeps the recording's own track.
        let _: schema::VoicePcm = asked(
            &session,
            schema::Query::Voice {
                peer: peer.clone(),
                key: ours[0].key,
            },
        );
        assert_eq!(voices()[0].voice_levels.0, track.bars(BARS, Fold::Peak));

        // Nothing heard.
        record("press", 40_000, 0, 0, 0);
        let mut quiet = LevelTrack::default();
        let now = feed(&mut quiet, 40_000, 39, &|w| (w % 2) as u32);
        let early = bar();
        assert_eq!(early.notice, "", "not before 2 s");
        assert!(early.pulsing);
        let now = feed(&mut quiet, now, 1, &|_| 0);
        let silent = bar();
        assert_eq!(silent.timer_ms, 2_000);
        assert_eq!(silent.notice, "Nothing heard. Is the microphone allowed?");
        assert_eq!(silent.notice_allow, "Allow the microphone");
        assert!(!silent.pulsing, "the dot stops pulsing");
        assert!(silent.moving, "it still records");
        tap("rec_allow");
        assert_eq!(bar().effects, ["ask_microphone"]);
        feed(&mut quiet, now, 1, &|_| 2);
        let back = bar();
        assert_eq!((back.notice.as_str(), back.pulsing), ("", true));
        tap("rec_discard");
        assert_eq!(bar().effects, ["discard"]);
        assert_eq!(voices().len(), 1, "the discarded one went nowhere");

        // Words the library does not know.
        for (input, verb) in [("hold", "rec_send"), ("press", "rec_forward")] {
            assert!(session
                .request(Request::Record {
                    peer: peer.clone(),
                    input: input.to_string(),
                    now: 0,
                    dx: 0,
                    dy: 0,
                    rms: 0,
                })
                .map_or(true, |()| input == "press"));
            assert!(session
                .request(Request::Recording {
                    peer: peer.clone(),
                    verb: verb.to_string(),
                })
                .map_or(true, |()| verb == "rec_send"));
        }
        tap("rec_discard");

        // 1222's path, no recording: the stream's stand-in, then the
        // decoded sound's own track.
        session
            .request(Request::Voice {
                peer: peer.clone(),
                pcm: schema::Blob(le_bytes(&loquax::voice::test_speech(16_000, 2_000))),
                rate: 16_000,
            })
            .unwrap_or_else(|error| panic!("{error}"));
        let plain = voices().pop().expect("the second voice message");
        let sound: schema::VoicePcm = asked(
            &session,
            schema::Query::Voice {
                peer: peer.clone(),
                key: plain.key,
            },
        );
        let decoded = LevelTrack::from_pcm(&le_samples(&sound.pcm.0), sound.rate);
        let drawn = voices().pop().expect("still there");
        assert_eq!(drawn.voice_levels.0, decoded.bars(BARS, Fold::Peak));
        session.stop();
    })
    .await
    .expect("the session");
    node.kill().await;
}

/// Link a device (1264, finding 7): two phones, one mailbox. The first
/// holds the mailbox, whose node dials a Rust transport node standing in
/// for the rendezvous (its servers file names it, as `code_list` then puts
/// it into the code); the profile's "Link a device" answers a code of the
/// `lqxh1` form `loquax mailbox admit` prints, its QR reads back to it
/// through the scanner's decoder, good for the default day, and `linked`
/// waits. The second phone starts with what the scanner read, as its first
/// start's "Join an existing mailbox" hands it over, is admitted under the
/// name it brought, and pulls the whole ugly store from the first phone's
/// archive; the first then says the device came, by that name. Negative:
/// the code is single use, a third phone with it is refused at the code.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_phone_links_to_the_mailbox_on_the_first_and_gets_its_history() {
    let rns = tempfile::tempdir().expect("rns dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let entry = format!("127.0.0.1:{}", listener.local_addr().expect("addr").port());
    let mut transport = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .add_tcp_server_listener(listener)
        .storage_path(rns.path().join("storage"))
        .build_sync()
        .expect("the transport node");
    transport.start().await.expect("start it");

    let first = tempfile::tempdir().expect("first phone");
    let mailbox = first.path().join(loquax_android::mailbox::DIR);
    std::fs::create_dir_all(&mailbox).expect("mailbox dir");
    std::fs::write(
        mailbox.join(loquax_android::mailbox::SERVERS_FILE),
        format!("{entry}\n"),
    )
    .expect("servers file");
    ugly_store::fill_archive(&mailbox);
    let start = Start {
        dir: first.path().display().to_string(),
        here: true,
        ..Start::default()
    };
    let (a, code, scanned) = tokio::task::spawn_blocking(move || {
        let a = Session::start(&start).unwrap_or_else(|error| panic!("{error:?}"));
        let linked: schema::Linked = asked(&a, schema::Query::Linked);
        assert_eq!(linked.state, "none");
        let code: schema::LinkCode = asked(&a, schema::Query::LinkDevice);
        let scanned = read_qr(code.qr_size, &code.qr.0);
        assert_eq!(scanned.len(), 1, "{scanned:?}");
        (a, code, scanned.into_iter().next().unwrap_or_default())
    })
    .await
    .expect("the first phone");
    let (parsed, list) =
        loquax::home::code::HomeCode::parse_with_rendezvous(&code.code).expect("a join code");
    assert!(
        code.code.starts_with(loquax::home::code::PREFIX),
        "{}",
        code.code
    );
    assert_eq!(
        loquax::home::code::HomeCode::parse_with_rendezvous(&scanned).expect("the QR's"),
        (parsed.clone(), list.clone())
    );
    assert_eq!(list, std::slice::from_ref(&entry));
    let mailbox_key = loquax_android::mailbox::identity(first.path())
        .expect("the mailbox identity")
        .public_key_bytes();
    assert_eq!(parsed.public_key, mailbox_key);
    assert_eq!(u64::from(code.hours), loquax::home::DEFAULT_TOKEN_HOURS);
    let a = tokio::task::spawn_blocking(move || {
        let linked: schema::Linked = asked(&a, schema::Query::Linked);
        assert_eq!(linked.state, "waiting");
        a
    })
    .await
    .expect("waiting");

    let second = tempfile::tempdir().expect("second phone");
    let start = Start {
        dir: second.path().display().to_string(),
        code: Some(scanned.clone()),
        name: Some("Pixel 8".to_string()),
        ..Start::default()
    };
    let b = tokio::task::spawn_blocking(move || {
        let b = Session::start(&start).unwrap_or_else(|error| panic!("{error:?}"));
        ugly_in(&b);
        b
    })
    .await
    .expect("the second phone");
    assert_eq!(b.address(), a.address());
    let a = tokio::task::spawn_blocking(move || {
        let linked: schema::Linked =
            wait_for(&a, schema::Query::Linked, 10, |l: &schema::Linked| {
                l.state == "linked"
            });
        assert_eq!(linked.name, "Pixel 8");
        a
    })
    .await
    .expect("linked");

    let third = tempfile::tempdir().expect("third phone");
    let start = Start {
        dir: third.path().display().to_string(),
        code: Some(scanned),
        name: Some("Fairphone".to_string()),
        ..Start::default()
    };
    let refused = tokio::task::spawn_blocking(move || Session::start(&start).err())
        .await
        .expect("the third phone")
        .expect("a used code is refused");
    assert_eq!(refused.need, Some("code"), "{refused:?}");

    tokio::task::spawn_blocking(move || {
        b.stop();
        a.stop();
    })
    .await
    .expect("stopped");
    transport.stop().await.expect("stop the transport node");
}

/// A transport node standing in for the rendezvous, and a phone directory
/// whose mailbox dials it ([`loquax_android::mailbox::SERVERS_FILE`]).
async fn rendezvous() -> (tempfile::TempDir, String, ReticulumNode) {
    let rns = tempfile::tempdir().expect("rns dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let entry = format!("127.0.0.1:{}", listener.local_addr().expect("addr").port());
    let mut transport = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .add_tcp_server_listener(listener)
        .storage_path(rns.path().join("storage"))
        .build_sync()
        .expect("the transport node");
    transport.start().await.expect("start it");
    (rns, entry, transport)
}

fn phone_dialling(entry: &str) -> tempfile::TempDir {
    let phone = tempfile::tempdir().expect("a phone");
    let mailbox = phone.path().join(loquax_android::mailbox::DIR);
    std::fs::create_dir_all(&mailbox).expect("mailbox dir");
    std::fs::write(
        mailbox.join(loquax_android::mailbox::SERVERS_FILE),
        format!("{entry}\n"),
    )
    .expect("servers file");
    phone
}

/// Walk finding 24 (1281): the old phone's "Move to a new phone" code,
/// scanned at the new phone's first start, carries the mailbox over: the
/// identity, its name, the contacts' names, blocks and the whole ugly
/// archive travel over the rendezvous, the new phone is the mailbox, and
/// the old one says the move took, stops, and does not start again until
/// its owner takes it back. Negative: the used code moves nothing to a
/// third phone, which keeps no identity.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_phone_takes_the_mailbox_over_and_the_old_one_stops() {
    let (_rns, entry, mut transport) = rendezvous().await;
    let old = phone_dialling(&entry);
    let mailbox = old.path().join(loquax_android::mailbox::DIR);
    ugly_store::fill_archive(&mailbox);
    loquax_android::mailbox::set_name(old.path(), "Anna").expect("a name");
    let start = Start {
        dir: old.path().display().to_string(),
        here: true,
        ..Start::default()
    };
    let (a, code) = tokio::task::spawn_blocking(move || {
        let a = Session::start(&start).unwrap_or_else(|error| panic!("{error:?}"));
        let moving: schema::Linked = asked(&a, schema::Query::Moving);
        assert_eq!(moving.state, "none");
        let code: schema::LinkCode = asked(&a, schema::Query::MovePhone);
        let scanned = read_qr(code.qr_size, &code.qr.0);
        assert_eq!(scanned.len(), 1, "{scanned:?}");
        let moving: schema::Linked = asked(&a, schema::Query::Moving);
        assert_eq!(moving.state, "waiting");
        (a, scanned.into_iter().next().unwrap_or_default())
    })
    .await
    .expect("the old phone");
    let key = loquax_android::mailbox::identity(old.path())
        .expect("the old identity")
        .public_key_bytes();

    let new = phone_dialling(&entry);
    let start = Start {
        dir: new.path().display().to_string(),
        here: true,
        moving: Some(code.clone()),
        name: Some("Pixel 9".to_string()),
        ..Start::default()
    };
    let b = tokio::task::spawn_blocking(move || {
        let b = Session::start(&start).unwrap_or_else(|error| panic!("{error:?}"));
        assert!(b.is_here());
        ugly_in(&b);
        b
    })
    .await
    .expect("the new phone");
    assert_eq!(b.address(), a.address());
    assert_eq!(
        loquax_android::mailbox::identity(new.path())
            .expect("the moved identity")
            .public_key_bytes(),
        key
    );
    assert_eq!(loquax_android::mailbox::name(new.path()), "Anna");
    assert!(!new
        .path()
        .join(loquax_android::mailbox::MOVING_DIR)
        .exists());

    let a = tokio::task::spawn_blocking(move || {
        let moving: schema::Linked =
            wait_for(&a, schema::Query::Moving, 10, |l: &schema::Linked| {
                l.state == "moved"
            });
        assert_eq!(moving.name, "Pixel 9");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !a.is_over() {
            assert!(
                std::time::Instant::now() < deadline,
                "the old mailbox runs on"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        a.stop();
    })
    .await;
    a.expect("the old phone stopped");
    assert!(loquax_android::mailbox::has_moved(old.path()));

    // The old phone does not start again by itself, and starts when its
    // owner says so.
    let again = Start {
        dir: old.path().display().to_string(),
        ..Start::default()
    };
    let refused = tokio::task::spawn_blocking(move || Session::start(&again).err())
        .await
        .expect("a start")
        .expect("a moved phone does not start");
    assert_eq!(refused.need, Some("moved"), "{refused:?}");
    assert_eq!(
        refused.text,
        loquax::ui::strings::StrId::MovedAway.text(Lang::En)
    );

    let third = phone_dialling(&entry);
    let start = Start {
        dir: third.path().display().to_string(),
        here: true,
        moving: Some(code),
        name: Some("Fairphone".to_string()),
        ..Start::default()
    };
    let refused = tokio::task::spawn_blocking(move || Session::start(&start).err())
        .await
        .expect("the third phone")
        .expect("a used move code moves nothing");
    assert_eq!(refused.need, Some("code"), "{refused:?}");
    assert!(!third
        .path()
        .join(loquax_android::mailbox::DIR)
        .join("identity")
        .exists());

    let start = Start {
        dir: old.path().display().to_string(),
        take_back: true,
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let a = Session::start(&start).unwrap_or_else(|error| panic!("{error:?}"));
        assert!(a.is_here());
        a.stop();
        b.stop();
    })
    .await
    .expect("taken back");
    assert!(!loquax_android::mailbox::has_moved(old.path()));
    transport.stop().await.expect("stop the transport node");
}

/// The restore's guard (1281, walk finding 24): a backup of an address
/// whose mailbox answers on another phone right now is refused with
/// `live` and the warning, and restores when the user chooses to anyway.
/// Positive control: with that mailbox stopped the same restore goes
/// through without a word.
#[tokio::test(flavor = "multi_thread")]
async fn a_restore_of_an_address_live_elsewhere_warns_first() {
    let (_rns, entry, mut transport) = rendezvous().await;
    let live = phone_dialling(&entry);
    let start = Start {
        dir: live.path().display().to_string(),
        here: true,
        ..Start::default()
    };
    let a = tokio::task::spawn_blocking(move || {
        Session::start(&start).unwrap_or_else(|error| panic!("{error:?}"))
    })
    .await
    .expect("the live phone");
    let line = loquax_android::mailbox::backup(live.path(), "Kastanienallee 7").expect("a backup");

    let second = phone_dialling(&entry);
    let restore = |dir: &Path, anyway: bool| Start {
        dir: dir.display().to_string(),
        here: true,
        restore: Some(line.clone()),
        passphrase: Some("Kastanienallee 7".to_string()),
        anyway,
        ..Start::default()
    };
    let warned = restore(second.path(), false);
    let refused = tokio::task::spawn_blocking(move || Session::start(&warned).err())
        .await
        .expect("a start")
        .expect("a live address warns");
    assert_eq!(refused.need, Some("live"), "{refused:?}");
    assert_eq!(
        refused.text,
        loquax::ui::strings::StrId::RestoreLive.text(Lang::En)
    );
    assert!(!second
        .path()
        .join(loquax_android::mailbox::DIR)
        .join("identity")
        .exists());
    let anyway = restore(second.path(), true);
    let address = a.address();
    tokio::task::spawn_blocking(move || {
        let b = Session::start(&anyway).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(b.address(), address);
        b.stop();
        a.stop();
    })
    .await
    .expect("restored anyway");

    let third = phone_dialling(&entry);
    let quiet = restore(third.path(), false);
    tokio::task::spawn_blocking(move || {
        let c = Session::start(&quiet).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(c.address(), address);
        c.stop();
    })
    .await
    .expect("restored with nobody live");
    transport.stop().await.expect("stop the transport node");
}

/// 1307 pass two: the phone gets the group screens through the scene's
/// export and asks its verbs in Java's bytes. A row of a group carries its
/// count and words, an invited one `Invitation`; a copy's first bubble of
/// a run carries its writer and the text without the home's prefix; the
/// invitation's bubble is a card with Join; each verb Java writes decodes
/// to the core's. Control: a verb the core has no word for is refused.
#[test]
fn the_phone_reads_groups_from_the_scene_and_asks_their_verbs() {
    use leviculum_lxmf::lev::{GroupCopy, Invite, Lev, Member, Members};
    use loquax::outbox::OutboxEvent;
    use loquax::ui::groups::Verb;
    use loquax::ui::intent::{Intent, Target};
    use loquax::ui::{update, UiEvent};
    use loquax_android::session::{self, Decoded};

    const ME: [u8; 16] = [0x30; 16];
    const LEW: [u8; 16] = [0x41; 16];
    const ANNA: [u8; 16] = [0x42; 16];
    const GROUP: [u8; 16] = [0x51; 16];
    let lev = |lev: Lev| {
        let mut fields = Vec::new();
        lev.put_into(&mut fields);
        fields
    };
    let mut model = loquax::ui::UiModel::<loquax_android::session::Line>::new(&[]);
    model.address = Some(ME);
    model.names.remember(LEW, "Lew");
    // One minute for all of it: the bubbles of one writer join.
    update(
        &mut model,
        UiEvent::Clock(loquax::when::WallTime {
            unix_secs: 1_800_000_000,
            offset_secs: 0,
        }),
    );
    let arrive = |model: &mut loquax::ui::UiModel<session::Line>, from, id: u8, body: &str, l| {
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: [id; 32],
                source: from,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields: lev(l),
                verified: true,
            }),
        );
    };
    arrive(
        &mut model,
        LEW,
        1,
        "Lew invited you to Familie",
        Lev {
            invite: Some(Invite {
                group: GROUP,
                public_key: [7; 64],
                name: "Familie".into(),
                admin_name: "Lew".into(),
                epoch: 1,
            }),
            ..Lev::default()
        },
    );
    let lew = model
        .conversations
        .iter()
        .position(|c| c.peer == LEW)
        .expect("Lew");
    let card = &session::rows(&model, lew, Lang::En)[0];
    assert_eq!(card.invite_group, hex(&GROUP));
    assert_eq!(card.invite_title, "Lew invited you to Familie");
    // 1348: the same message on a German phone, the card from `lev 0x04`.
    assert_eq!(
        session::rows(&model, lew, Lang::De)[0].invite_title,
        "Lew lädt dich zu Familie ein"
    );
    assert!(card.invite_join);
    assert!(card.invite_trust.contains("Lew could read"));

    update(
        &mut model,
        UiEvent::Group(Verb::Join {
            group: Target::Peer(GROUP),
        }),
    );
    arrive(
        &mut model,
        GROUP,
        2,
        "",
        Lev {
            members: Some(Members {
                epoch: 1,
                members: vec![
                    Member {
                        address: LEW,
                        name: "Lew".into(),
                        flags: 2 | 4,
                    },
                    Member {
                        address: ME,
                        name: "Me".into(),
                        flags: 4,
                    },
                    Member {
                        address: ANNA,
                        name: "Anna".into(),
                        flags: 4,
                    },
                ],
                ended: false,
            }),
            ..Lev::default()
        },
    );
    for (id, writer, body) in [
        (3, LEW, "Lew: Brot"),
        (4, LEW, "Lew: ist da"),
        (5, ANNA, "Anna: Danke"),
    ] {
        arrive(
            &mut model,
            GROUP,
            id,
            body,
            Lev {
                group_copy: Some(GroupCopy {
                    sender: writer,
                    // Each line its writer's own time: a line is shown
                    // once by writer and time (1307 pass four).
                    sent_ms: u64::from(id),
                    epoch: 1,
                }),
                ..Lev::default()
            },
        );
    }
    let g = model
        .conversations
        .iter()
        .position(|c| c.peer == GROUP)
        .expect("group");
    let rows: Vec<_> = session::rows(&model, g, Lang::En)
        .into_iter()
        .map(|r| (r.writer, r.body))
        .collect();
    assert_eq!(
        rows,
        [
            ("Lew".to_string(), "Brot".to_string()),
            (String::new(), "ist da".to_string()),
            ("Anna".to_string(), "Danke".to_string()),
        ],
        "a writer once per run, the text without the prefix"
    );
    let row = loquax::ui::scene::row(&model, &GROUP);
    assert_eq!(row.avatar.initials, "3");

    // Lew deletes it (1307 pass four): the home's last membership is a
    // note row, drawn by Java as a centred line, and no bubble.
    arrive(
        &mut model,
        GROUP,
        6,
        "",
        Lev {
            members: Some(Members {
                epoch: 2,
                members: vec![
                    Member {
                        address: LEW,
                        name: "Lew".into(),
                        flags: 1 | 2 | 4,
                    },
                    Member {
                        address: ME,
                        name: "Me".into(),
                        flags: 1 | 4,
                    },
                ],
                ended: true,
            }),
            ..Lev::default()
        },
    );
    let last = session::rows(&model, g, Lang::En).pop().expect("the note");
    assert!(last.note, "a note row");
    assert_eq!(last.body, "Lew ended the group.");
    assert!(
        session::rows(&model, g, Lang::En)
            .iter()
            .filter(|r| !r.note)
            .all(|r| r.late.is_empty()),
        "control: no line came late"
    );

    let decoded = |bytes: Vec<u8>| -> Result<Intent, String> {
        let request: schema::Request = schema::decode(&bytes)?;
        match session::decode(request)? {
            Decoded::Intent(intent) => Ok(intent),
            Decoded::Platform(_) => Err("platform".into()),
        }
    };
    let group = hex(&GROUP);
    let anna = hex(&ANNA);
    let cases: Vec<(Vec<u8>, Verb)> = vec![
        (
            java_map(&[("t", Java::Str("group")), ("verb", Java::Str("new"))]),
            Verb::New,
        ),
        (
            java_map(&[
                ("t", Java::Str("group")),
                ("verb", Java::Str("pick")),
                ("peer", Java::Str(&anna)),
            ]),
            Verb::Pick(ANNA),
        ),
        (
            java_map(&[
                ("t", Java::Str("group")),
                ("verb", Java::Str("name")),
                ("text", Java::Str("Familie")),
            ]),
            Verb::Name("Familie".into()),
        ),
        (
            java_map(&[("t", Java::Str("group")), ("verb", Java::Str("create"))]),
            Verb::Create,
        ),
        (
            java_map(&[
                ("t", Java::Str("group")),
                ("verb", Java::Str("join")),
                ("peer", Java::Str(&group)),
            ]),
            Verb::Join {
                group: Target::Peer(GROUP),
            },
        ),
        // The admin's Add members (1307 G2).
        (
            java_map(&[
                ("t", Java::Str("group")),
                ("verb", Java::Str("add")),
                ("peer", Java::Str(&group)),
            ]),
            Verb::Add {
                group: Target::Peer(GROUP),
            },
        ),
        (
            java_map(&[
                ("t", Java::Str("group")),
                ("verb", Java::Str("remove")),
                ("peer", Java::Str(&group)),
                ("member", Java::Str(&anna)),
                ("confirmed", Java::Bool(true)),
            ]),
            Verb::Remove {
                group: Target::Peer(GROUP),
                member: Some(ANNA),
                confirmed: true,
            },
        ),
        (
            java_map(&[
                ("t", Java::Str("group")),
                ("verb", Java::Str("leave")),
                ("peer", Java::Str(&group)),
                ("confirmed", Java::Bool(false)),
            ]),
            Verb::Leave {
                group: Target::Peer(GROUP),
                confirmed: false,
            },
        ),
        // The admin's Delete group and Start again (1307 pass four).
        (
            java_map(&[
                ("t", Java::Str("group")),
                ("verb", Java::Str("delete")),
                ("peer", Java::Str(&group)),
                ("confirmed", Java::Bool(true)),
            ]),
            Verb::Delete {
                group: Target::Peer(GROUP),
                confirmed: true,
            },
        ),
        (
            java_map(&[
                ("t", Java::Str("group")),
                ("verb", Java::Str("again")),
                ("peer", Java::Str(&group)),
            ]),
            Verb::Again {
                group: Target::Peer(GROUP),
            },
        ),
    ];
    for (bytes, verb) in cases {
        assert_eq!(decoded(bytes).expect("decodes"), Intent::Group(verb));
    }
    let unknown = java_map(&[("t", Java::Str("group")), ("verb", Java::Str("invite"))]);
    assert!(decoded(unknown).is_err());
}

/// 1307 pass five, Lew's phone 2026-11-02: "I can create a group, but I do
/// not see it". On a home that hosts groups, the home's answer to Create
/// says `changed`, and the list Java reads then has the group's row, on
/// top of every unpinned chat, under its name and with its members, before
/// anyone wrote in it. A restart lists it again from the store. Control:
/// the list read right after Create, before the home answered, has no
/// such row, so the row comes from the answer and nothing pretends.
#[tokio::test(flavor = "multi_thread")]
async fn a_group_made_on_the_phone_is_in_the_list_at_once_and_after_a_restart() {
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
    let first = Start {
        dir: dir.clone(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    let anna = ugly_store::peer_hex(1);
    let group = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        ugly_in(&session);
        let verb = |verb: &str, peer: &str, text: &str| {
            session
                .request(Request::Group {
                    verb: verb.to_string(),
                    peer: peer.to_string(),
                    member: String::new(),
                    text: text.to_string(),
                    confirmed: false,
                })
                .unwrap_or_else(|error| panic!("{verb}: {error}"));
        };
        verb("new", "", "");
        verb("pick", &anna, "");
        verb("next", "", "");
        verb("name", "", "Familie");
        // Drain what came before, so the next `changed` is the answer's.
        while session.next_event(Duration::from_millis(300)).is_some() {}
        verb("create", "", "");
        let list = |session: &Session| -> schema::Conversations {
            rmp_serde::from_slice(
                &session
                    .query(schema::Query::Conversations { search: None })
                    .expect("the list"),
            )
            .expect("decode")
        };
        let before = list(&session);
        assert!(
            before
                .conversations
                .iter()
                .all(|row| !row.name.starts_with("Familie")),
            "control: no row before the home answered"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let shown = loop {
            assert!(
                std::time::Instant::now() < deadline,
                "no changed with the group"
            );
            match session.next_event(Duration::from_millis(500)) {
                Some(Event::Notice { text, .. }) => panic!("the home refused: {text}"),
                Some(Event::Changed { .. }) => {
                    let shown = list(&session);
                    if shown
                        .conversations
                        .iter()
                        .any(|row| row.name.starts_with("Familie"))
                    {
                        break shown;
                    }
                }
                _ => {}
            }
        };
        let at = shown
            .conversations
            .iter()
            .position(|row| row.name.starts_with("Familie"))
            .expect("the row");
        let row = &shown.conversations[at];
        assert!(row.group_members >= 2, "{row:?}");
        assert!(
            shown.conversations[..at].iter().all(|row| row.pinned),
            "on top of the unpinned chats: {:?}",
            shown.conversations[..at]
                .iter()
                .map(|r| &r.name)
                .collect::<Vec<_>>()
        );
        let group = row.peer.clone();
        session.stop();
        group
    })
    .await
    .expect("the first session");

    let again = Start {
        dir,
        ..Start::default()
    };
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&again).unwrap_or_else(|error| panic!("{error:?}"));
        let listed: schema::Conversations = rmp_serde::from_slice(
            &session
                .query(schema::Query::Conversations { search: None })
                .expect("the list"),
        )
        .expect("decode");
        let row = listed
            .conversations
            .iter()
            .find(|row| row.peer == group)
            .unwrap_or_else(|| panic!("the group after a restart: {listed:?}"));
        assert!(row.name.starts_with("Familie"), "{row:?}");
        assert!(row.group_members >= 2, "{row:?}");
        session.stop();
    })
    .await
    .expect("the second session");
    node.stop().await;
}

/// 1388, Lew's live test with a stock Columba member 2026-11-03: a photo
/// without words from the phone, in a group on its home, reaches a stock
/// Python member from the group's address as the photo with the English
/// sentence `"Photo from <name>"`, not the bare `"<name>:"` it said before.
#[tokio::test(flavor = "multi_thread")]
async fn a_photo_without_words_reaches_a_stock_member_as_photo_from_its_writer() {
    let peer = python_peer::PythonPeer::start();
    let python = peer.lxmf_init();
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start_linked(
        listener,
        &instance,
        rns.path(),
        home.path(),
        Some(peer.rns_port),
    )
    .await;
    // The home learns the stock member's key before it invites it.
    peer.lxmf_announce();
    tokio::time::sleep(Duration::from_secs(2)).await;

    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    let bob = python.clone();
    let (session, group) = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        let verb = |verb: &str, peer: &str, text: &str| {
            session
                .request(Request::Group {
                    verb: verb.to_string(),
                    peer: peer.to_string(),
                    member: String::new(),
                    text: text.to_string(),
                    confirmed: false,
                })
                .unwrap_or_else(|error| panic!("{verb}: {error}"));
        };
        verb("new", "", "");
        verb("pick", &bob, "");
        verb("next", "", "");
        verb("name", "", "Familie");
        while session.next_event(Duration::from_millis(300)).is_some() {}
        verb("create", "", "");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let group = loop {
            assert!(
                std::time::Instant::now() < deadline,
                "no changed with the group"
            );
            match session.next_event(Duration::from_millis(500)) {
                Some(Event::Notice { text, .. }) => panic!("the home refused: {text}"),
                Some(Event::Changed { .. }) => {
                    let shown: schema::Conversations = rmp_serde::from_slice(
                        &session
                            .query(schema::Query::Conversations { search: None })
                            .expect("the list"),
                    )
                    .expect("decode");
                    if let Some(row) = shown
                        .conversations
                        .iter()
                        .find(|row| row.name.starts_with("Familie"))
                    {
                        break row.peer.clone();
                    }
                }
                _ => {}
            }
        };
        session
            .request(Request::Photo {
                peer: group.clone(),
                data: schema::Blob(PHOTO.to_vec()),
            })
            .unwrap_or_else(|error| panic!("the photo: {error}"));
        (session, group)
    })
    .await
    .expect("the session");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let got = loop {
        let found = peer
            .received()
            .into_iter()
            .find(|m| m["source_hash"] == group.as_str() && python_photo(m).is_some());
        if let Some(found) = found {
            break found;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the stock member never got the photo: {:?}",
            peer.received()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    // The peer hands content over as hex.
    let content = got["content"].as_str().expect("content");
    let content: Vec<u8> = (0..content.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&content[i..i + 2], 16).expect("hex"))
        .collect();
    let content = String::from_utf8(content).expect("utf-8");
    let name = content
        .strip_prefix("Photo from ")
        .unwrap_or_else(|| panic!("the sentence, not {content:?}"));
    assert!(
        !name.is_empty() && !name.contains(':'),
        "a name after it: {content:?}"
    );
    tokio::task::spawn_blocking(move || session.stop())
        .await
        .expect("stopped");
    node.stop().await;
}

mod group_ticks {
    include!("../../loquax/tests/support/group_ticks.rs");
}

/// 1466, the group's ticks tell the truth too, on the phone: the group's
/// chat of `scene-group-ticks.json` as Java reads its rows (our line every
/// member's device has `delivered`, the one Ben's device has not said yet
/// `arrived`) and each line's Info in German, a row per member with its
/// rung and since when. Pinned as `golden/group-ticks.json`;
/// `LOQUAX_WRITE_GOLDEN=1` writes it again.
#[test]
fn a_groups_one_and_two_ticks_are_the_phones_golden() {
    use loquax_android::session;
    let mut model = loquax::ui::UiModel::<session::Line>::new(&[]);
    model.address = Some([0x30; 16]);
    let c = group_ticks::group_ticks(&mut model, 1_800_000_000, 3600);
    let rows = session::rows(&model, c, Lang::De);
    let states: Vec<(&str, &str)> = rows
        .iter()
        .filter(|r| r.ours)
        .map(|r| (r.body.as_str(), r.state.as_str()))
        .collect();
    assert_eq!(
        states,
        [("Brot ist da", "delivered"), ("Wer kommt mit?", "arrived")]
    );
    let info = |id: [u8; 32]| -> Vec<schema::InfoRow> {
        let key = model.conversations[c]
            .messages
            .iter()
            .find(|m| m.id == Some(id))
            .expect("our line")
            .activity;
        loquax::ui::info::message_info(&model, group_ticks::TICKS_GROUP, key)
            .expect("its info")
            .rows
            .iter()
            .map(|row| {
                let (label, value) = row.resolve(Lang::De);
                schema::InfoRow { label, value }
            })
            .collect()
    };
    let two = info(group_ticks::TICKS_TWO);
    let one = info(group_ticks::TICKS_ONE);
    let value = |rows: &[schema::InfoRow], label: &str| {
        rows.iter()
            .find(|r| r.label == label)
            .map(|r| r.value.clone())
            .unwrap_or_else(|| panic!("no {label}: {rows:?}"))
    };
    assert_eq!(value(&two, "Status"), "Auf den Geräten");
    assert_eq!(value(&one, "Status"), "Angekommen");
    // 1449 item 7: the row's word for a screen reader is the Info's, not
    // `Ui.state("delivered")`'s one device.
    let words: Vec<(&str, &str)> = rows
        .iter()
        .filter(|r| r.ours)
        .map(|r| (r.body.as_str(), r.state_words.as_str()))
        .collect();
    assert_eq!(
        words,
        [
            ("Brot ist da", "Auf den Geräten"),
            ("Wer kommt mit?", "Angekommen")
        ]
    );
    assert!(
        value(&one, "Anna").starts_with("Auf dem Gerät · "),
        "{one:?}"
    );
    assert!(value(&one, "Ben").starts_with("Angekommen · "), "{one:?}");
    let mut got = serde_json::to_string_pretty(&serde_json::json!({
        "rows": rows,
        "info_two_ticks": two,
        "info_one_tick": one,
    }))
    .expect("json");
    got.push('\n');
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/group-ticks.json");
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "group-ticks.json moved; the answer now:\n{got}\n(LOQUAX_WRITE_GOLDEN=1 writes it)"
    );
}

mod late_message {
    include!("../../loquax/tests/support/late_message.rs");
}

/// 1505, a late message sits at its sent time, on the phone: Anna's chat of
/// the shared fixture as Java reads its rows, in German, the message she
/// wrote last night at 22:10 that came today at 14:05 second, under last
/// night's day, and today's two below with their own day. Pinned as
/// `golden/late-message.json`; `LOQUAX_WRITE_GOLDEN=1` writes it again.
#[test]
fn a_late_message_is_the_phones_golden() {
    use loquax_android::session;
    let mut model = loquax::ui::UiModel::<session::Line>::new(&[]);
    model.address = Some([0x30; 16]);
    let c = late_message::late_message(&mut model, 1_799_971_200, 3600);
    let rows = session::rows(&model, c, Lang::De);
    let bodies: Vec<&str> = rows.iter().map(|r| r.body.as_str()).collect();
    assert_eq!(
        bodies,
        [
            "Gute Nacht",
            "Kommst du morgen mit?",
            "Bis gleich am Bahnhof",
            "Bin unterwegs"
        ]
    );
    let mut got = serde_json::to_string_pretty(&serde_json::json!({ "rows": rows })).expect("json");
    got.push('\n');
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/late-message.json");
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "late-message.json moved; the answer now:\n{got}\n(LOQUAX_WRITE_GOLDEN=1 writes it)"
    );
}

mod relay_group {
    include!("../../loquax/tests/support/relay_group.rs");
}

/// 1482, a relayed post shows its writer, on the phone: the relay's chat
/// of `scene-relay-group.json` as Java reads it, in German. The bar's
/// title says it is a group via a relay; each row carries its writer and
/// the words without the bot's prefix; the reaction whose target is not
/// here is a row in words; the reply's quote names its writer; the Info
/// names Anna with her address's 4-hex tail and the relay. Pinned as
/// `golden/relay-group.json`; `LOQUAX_WRITE_GOLDEN=1` writes it again.
#[test]
fn a_relayed_group_is_the_phones_golden() {
    use loquax_android::session;
    let mut model = loquax::ui::UiModel::<session::Line>::new(&[]);
    model.address = Some([0x30; 16]);
    model.lang = Lang::De;
    let c = relay_group::relay_group(&mut model, 1_800_000_000, 3600);
    let title = session::relayed_title(&model, &relay_group::RELAY, Lang::De);
    assert!(title.ends_with(" · Gruppe über Relais"), "{title}");
    let rows = session::rows(&model, c, Lang::De);
    let bodies: Vec<&str> = rows.iter().map(|r| r.body.as_str()).collect();
    assert_eq!(
        bodies,
        [
            "Hallo zusammen",
            "Moin Anna",
            "hat mit 👍 auf eine Nachricht reagiert, die hier nicht liegt",
            "Ich bin auch da",
            "Schön, dass du da bist",
        ]
    );
    let writers: Vec<&str> = rows.iter().map(|r| r.writer.as_str()).collect();
    assert_eq!(writers, ["Anna", "Ben", "", "", "Anna"]);
    assert_eq!(rows[4].quote.as_deref(), Some("Ben: Moin Anna"));
    let key = model.conversations[c]
        .messages
        .iter()
        .find(|m| m.id == Some(relay_group::RELAY_POST))
        .expect("Anna's post")
        .activity;
    let info: Vec<schema::InfoRow> =
        loquax::ui::info::message_info(&model, relay_group::RELAY, key)
            .expect("its info")
            .rows
            .iter()
            .map(|row| {
                let (label, value) = row.resolve(Lang::De);
                schema::InfoRow { label, value }
            })
            .collect();
    assert!(
        info[0]
            .value
            .starts_with("Anna ·6b6b, über ReticulumNet NL"),
        "{info:?}"
    );
    let mut got = serde_json::to_string_pretty(&serde_json::json!({
        "title": title,
        "rows": rows,
        "info_annas_post": info,
    }))
    .expect("json");
    got.push('\n');
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/relay-group.json");
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "relay-group.json moved; the answer now:\n{got}\n(LOQUAX_WRITE_GOLDEN=1 writes it)"
    );
}

/// The read receipts of the messages Python's router handed its delivery
/// callback (`umsgpack.packb(message.fields)`): every id a `lev` receipt
/// names.
fn python_receipts(received: &[serde_json::Value]) -> Vec<[u8; 32]> {
    let mut ids = Vec::new();
    for message in received {
        let Some(packed) = message.get("fields").and_then(|f| f.as_str()) else {
            continue;
        };
        let Some(packed): Option<Vec<u8>> = (0..packed.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&packed[i..i + 2], 16).ok())
            .collect()
        else {
            continue;
        };
        use leviculum_lxmf::msgpack;
        let mut at = 0;
        let Ok(entries) = msgpack::map_len(&packed, &mut at) else {
            continue;
        };
        let mut fields = Vec::new();
        for _ in 0..entries {
            let Ok(key) = msgpack::read_int(&packed, &mut at) else {
                break;
            };
            let start = at;
            if msgpack::skip(&packed, &mut at).is_err() {
                break;
            }
            fields.push((key, packed[start..at].to_vec()));
        }
        if let Some(receipt) =
            leviculum_lxmf::lev::Lev::from_fields(&fields).and_then(|lev| lev.receipt)
        {
            ids.extend(receipt.ids);
        }
    }
    ids
}

/// The phone sends a read receipt a Python peer sees (1496, structure
/// review 2026-11-07 finding 10). A Python LXMF client that marks its
/// message as one that wants receipts (`lev` `{0x00: 1}`) sends DIRECT to
/// the mailbox on the phone; the chat opens in front. Negative first: open
/// and in front, without a viewport, as the phone was before 1496 (no Java
/// said one), Python gets no receipt in twice the batch time. Then the
/// viewport `ChatActivity` says, with the bubble's key: Python receives a
/// `lev` receipt that names its message's id, signed by the mailbox.
#[tokio::test(flavor = "multi_thread")]
async fn a_python_writer_sees_the_read_once_the_phone_says_its_viewport() {
    use leviculum_lxmf::lev::{Lev, CAP_READ_RECEIPTS};
    use loquax::ui::receipts::BATCH_MS;
    let peer = python_peer::PythonPeer::start();
    let python = peer.lxmf_init();
    let phone = tempfile::tempdir().expect("app dir");
    let mailbox = phone.path().join(loquax_android::mailbox::DIR);
    std::fs::create_dir_all(&mailbox).expect("mailbox dir");
    std::fs::write(
        mailbox.join(loquax_android::mailbox::SERVERS_FILE),
        format!("127.0.0.1:{}\n", peer.rns_port),
    )
    .expect("servers file");
    let first = Start {
        dir: phone.path().display().to_string(),
        here: true,
        lang: Some("en".to_string()),
        ..Start::default()
    };
    let session = tokio::task::spawn_blocking(move || {
        Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"))
    })
    .await
    .expect("the session");
    let address = loquax::address::to_hex(&session.address());
    peer.lxmf_announce();

    let mark = Lev::mark();
    assert_eq!(mark.capabilities, Some(CAP_READ_RECEIPTS));
    let mut fields = Vec::new();
    leviculum_lxmf::msgpack::map(&mut fields, 2);
    for (key, value) in mark.fields() {
        leviculum_lxmf::msgpack::int(&mut fields, key);
        fields.extend_from_slice(&value);
    }
    let body = "lies mich";
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let message_hash = loop {
        match peer.try_rpc(
            "lxmf_send",
            serde_json::json!({
                "dest_hash": address,
                "method": "direct",
                "content": hex(body.as_bytes()),
                "fields": hex(&fields),
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
    let id: [u8; 32] = (0..32)
        .map(|i| u8::from_str_radix(&message_hash[2 * i..2 * i + 2], 16).expect("hex"))
        .collect::<Vec<u8>>()
        .try_into()
        .expect("32 bytes");
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

    // The bubble on the screens, the chat open and in front.
    let shown = python.clone();
    let (session, key) = tokio::task::spawn_blocking(move || {
        let messages: schema::Messages = wait_for(
            &session,
            schema::Query::Messages {
                peer: shown.clone(),
            },
            30,
            |m: &schema::Messages| m.messages.iter().any(|b| b.body == body),
        );
        let key = messages
            .messages
            .iter()
            .find(|b| b.body == body)
            .expect("the bubble")
            .key;
        session
            .request(Request::Open {
                peer: shown.clone(),
            })
            .expect("open");
        session
            .request(Request::Front { front: true })
            .expect("front");
        (session, key)
    })
    .await
    .expect("the open chat");

    // Negative: no viewport, no receipt.
    let quiet = tokio::time::Instant::now() + Duration::from_millis(2 * BATCH_MS + 1_000);
    while tokio::time::Instant::now() < quiet {
        assert!(
            !python_receipts(&peer.received()).contains(&id),
            "a receipt without a viewport"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The viewport, as ChatActivity says it: Python sees the read.
    let shown = python.clone();
    let session = tokio::task::spawn_blocking(move || {
        session
            .request(Request::Viewport {
                peer: shown,
                keys: vec![key],
            })
            .expect("viewport");
        session
    })
    .await
    .expect("the viewport");
    let said = tokio::time::Instant::now();
    let deadline = said + Duration::from_secs(60);
    loop {
        if python_receipts(&peer.received()).contains(&id) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Python never saw the read: {:?}",
            peer.received()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    println!(
        "COUNTS phone.receipt python_saw_read_after_ms={}",
        said.elapsed().as_millis()
    );
    tokio::task::spawn_blocking(move || session.stop())
        .await
        .expect("stopped");
}
