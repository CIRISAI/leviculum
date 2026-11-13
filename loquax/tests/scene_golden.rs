//! The scene of the list and the chat (1217), built from the ugly store
//! (1173) for both layout classes and pinned as JSON under
//! `tests/golden/scene-*.json`: keys sorted, texts as catalogue keys with
//! their arguments, so one file pins both languages' structure. A scene
//! that moved fails with the first differing line and leaves the new one
//! in the temp dir as `loquax-golden/<name>`; `LOQUAX_WRITE_GOLDEN=1`
//! writes the goldens instead, for a change that means to move them.

#[path = "support/ugly_store.rs"]
mod ugly_store;

use std::path::{Path, PathBuf};

use leviculum_ui::input::{InputBuffer, KeyEvent};
use loquax::store::Store;
use loquax::ui::scene::LayoutClass;
use loquax::ui::{update, UiEvent, UiModel, NONE_OPEN};
use loquax::when::WallTime;

/// A compose field nobody types into.
#[derive(Debug, Default, Clone)]
struct Line(String);

impl InputBuffer for Line {
    fn value(&self) -> &str {
        &self.0
    }
    fn cursor(&self) -> usize {
        self.0.chars().count()
    }
    fn edit(&mut self, _key: &KeyEvent) -> bool {
        false
    }
    fn take(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
    fn newline(&mut self) {
        self.0.push('\n');
    }
    fn set_cursor(&mut self, _at: usize) {}
    fn set_value(&mut self, text: &str) {
        self.0 = text.to_string();
    }
}

/// The reader: no conversation's peer.
const OWN: usize = 0x30;

/// A day after the store's last row: every age and day line is fixed.
fn now() -> WallTime {
    let last = ugly_store::BASE
        + (ugly_store::PER_CONVERSATION as i64) * 600
        + (ugly_store::CONVERSATIONS as i64) * 7;
    WallTime {
        unix_secs: last + 86_400,
        offset_secs: 3600,
    }
}

/// The model a terminal loquax restores from the ugly store, as the shell
/// builds it: contacts, names, the history, the positions.
fn ugly_model(home: &Path) -> UiModel<Line> {
    let own = ugly_store::identity(OWN);
    ugly_store::fill_store_of(home, &own);
    let me = loquax::engine::delivery_address(&own).expect("address");
    let store = Store::open_in(home, me).expect("the store");
    let contacts = loquax::names::load_contacts(home).expect("contacts");
    let mut model = UiModel::<Line>::new(&[]);
    model.address = Some(me);
    model.names = loquax::names::Names::new(contacts);
    for (peer, name) in store.names().expect("names") {
        model.names.remember(peer, &name);
    }
    update(&mut model, UiEvent::Clock(now()));
    model.restore(store.restore().expect("restore"));
    model.restore_positions(store.positions().expect("positions"));
    model
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn check(name: &str, scene: &loquax::ui::scene::Scene) {
    let mut got = serde_json::to_string_pretty(&scene.to_json()).expect("json");
    got.push('\n');
    let path = golden(name);
    if std::env::var_os("LOQUAX_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write the golden");
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    if got != want {
        let dir = std::env::temp_dir().join("loquax-golden");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(name), &got);
        let line = got
            .lines()
            .zip(want.lines())
            .position(|(g, w)| g != w)
            .unwrap_or_else(|| got.lines().count().min(want.lines().count()));
        panic!(
            "{name} moved at line {}: got {:?}, want {:?}",
            line + 1,
            got.lines().nth(line),
            want.lines().nth(line)
        );
    }
}

#[test]
fn the_ugly_store_is_the_golden_scene_in_both_classes() {
    let home = tempfile::tempdir().expect("home");
    let mut model = ugly_model(home.path());
    let open = model
        .conversations
        .iter()
        .position(|c| c.peer == ugly_store::peer(2))
        .expect("conversation 2");
    model.selected = NONE_OPEN;
    check(
        "scene-compact-list.json",
        &model.scene(LayoutClass::Compact),
    );
    model.selected = open;
    check(
        "scene-compact-chat.json",
        &model.scene(LayoutClass::Compact),
    );
    check("scene-wide.json", &model.scene(LayoutClass::Wide));
}

/// The scene is a function of the store: two homes filled alike give the
/// same scene, so the goldens pin no accident of a run.
#[test]
fn the_scene_is_the_same_from_two_homes() {
    let a = tempfile::tempdir().expect("home a");
    let b = tempfile::tempdir().expect("home b");
    let a = ugly_model(a.path()).scene(LayoutClass::Wide).to_json();
    let b = ugly_model(b.path()).scene(LayoutClass::Wide).to_json();
    assert_eq!(a, b);
}

/// 1307 pass two: the four group screens as every target gets them. A
/// reader who was invited by Lew to "Familie" and joined, a second
/// invitation still open, the group's chat with Lew's and Anna's copies
/// under their names, the member sheet, and "New group" at its two steps.
/// Lew is the reader's contact; the group, its members and the copies come
/// as the home sends them.
#[test]
fn the_group_screens_are_the_golden_scenes() {
    use leviculum_lxmf::lev::{GroupCopy, Invite, Lev, Member, Members};
    use loquax::outbox::OutboxEvent;
    use loquax::ui::groups::Verb;
    use loquax::ui::intent::Target;

    const ME: [u8; 16] = [0x30; 16];
    const LEW: [u8; 16] = [0x41; 16];
    const ANNA: [u8; 16] = [0x42; 16];
    const BOB: [u8; 16] = [0x43; 16];
    const FAMILIE: [u8; 16] = [0x51; 16];
    const VEREIN: [u8; 16] = [0x52; 16];
    let at = |minutes: i64| WallTime {
        unix_secs: 1_800_000_000 + minutes * 60,
        offset_secs: 3600,
    };
    let fields = |lev: Lev| {
        let mut fields = Vec::new();
        lev.put_into(&mut fields);
        fields
    };
    let invite = |group: [u8; 16], name: &str| Lev {
        capabilities: Some(3),
        invite: Some(Invite {
            group,
            public_key: [7; 64],
            name: name.into(),
            admin_name: "Lew".into(),
            epoch: 1,
        }),
        ..Lev::default()
    };
    let copy = |writer: [u8; 16]| Lev {
        capabilities: Some(2),
        group_copy: Some(GroupCopy {
            sender: writer,
            sent_ms: 1,
            epoch: 2,
        }),
        ..Lev::default()
    };
    let mut model = UiModel::<Line>::new(&[BOB]);
    model.address = Some(ME);
    model.names.remember(LEW, "Lew");
    model.names.remember(BOB, "Bob");
    let arrive = |model: &mut UiModel<Line>, minute, from, id: u8, body: &str, lev: Lev| {
        update(model, UiEvent::Clock(at(minute)));
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: [id; 32],
                source: from,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                fields: fields(lev),
                verified: true,
            }),
        );
    };
    arrive(
        &mut model,
        0,
        LEW,
        1,
        "Lew invited you to Familie: lxma://…",
        invite(FAMILIE, "Familie"),
    );
    update(
        &mut model,
        UiEvent::Group(Verb::Join {
            group: Target::Peer(FAMILIE),
        }),
    );
    let list = |flags: [u64; 3]| Lev {
        capabilities: Some(3),
        members: Some(Members {
            epoch: 2,
            members: vec![
                Member {
                    address: LEW,
                    name: "Lew".into(),
                    flags: flags[0],
                },
                Member {
                    address: ME,
                    name: "Kim".into(),
                    flags: flags[1],
                },
                Member {
                    address: ANNA,
                    name: "Anna".into(),
                    flags: flags[2],
                },
            ],
            ended: false,
        }),
        ..Lev::default()
    };
    arrive(&mut model, 1, FAMILIE, 2, "", list([2 | 4, 4, 0]));
    arrive(&mut model, 2, FAMILIE, 3, "Lew: Brot ist da", copy(LEW));
    arrive(&mut model, 3, FAMILIE, 4, "Anna: Danke!", copy(ANNA));
    arrive(
        &mut model,
        4,
        LEW,
        5,
        "Lew invited you to Verein: lxma://…",
        invite(VEREIN, "Verein"),
    );
    update(&mut model, UiEvent::Clock(at(10)));
    let position = |model: &UiModel<Line>, peer: [u8; 16]| {
        model
            .conversations
            .iter()
            .position(|c| c.peer == peer)
            .expect("listed")
    };

    model.selected = NONE_OPEN;
    check("scene-group-list.json", &model.scene(LayoutClass::Compact));
    model.selected = position(&model, FAMILIE);
    check("scene-group-chat.json", &model.scene(LayoutClass::Compact));
    model.selected = position(&model, LEW);
    check(
        "scene-group-invitation.json",
        &model.scene(LayoutClass::Compact),
    );
    model.selected = position(&model, VEREIN);
    check(
        "scene-group-invited-chat.json",
        &model.scene(LayoutClass::Compact),
    );
    model.selected = position(&model, FAMILIE);
    update(
        &mut model,
        UiEvent::Group(Verb::Members {
            group: Target::Open,
        }),
    );
    check("scene-group-members.json", &model.scene(LayoutClass::Wide));
    update(
        &mut model,
        UiEvent::Group(Verb::Leave {
            group: Target::Open,
            confirmed: false,
        }),
    );
    check(
        "scene-group-members-leave.json",
        &model.scene(LayoutClass::Compact),
    );
    update(&mut model, UiEvent::Group(Verb::Close));
    update(&mut model, UiEvent::Group(Verb::Close));
    update(&mut model, UiEvent::Group(Verb::New));
    update(&mut model, UiEvent::Group(Verb::Pick(BOB)));
    check(
        "scene-group-new-pick.json",
        &model.scene(LayoutClass::Compact),
    );
    update(&mut model, UiEvent::Group(Verb::Next));
    update(&mut model, UiEvent::Group(Verb::Name("Wanderung".into())));
    check(
        "scene-group-new-name.json",
        &model.scene(LayoutClass::Compact),
    );
}

/// 1578: the Explore tab, the pages and the public rooms under one button,
/// in both classes: a bookmark on top, the pages a home and the device
/// heard by last seen under "Today", "This week" and "Older", the shipped
/// hubs, and a hub heard on the mesh; then the same list A-Z and filtered
/// by a short hash.
#[test]
fn the_explore_tab_is_the_golden_scene() {
    use lnomad::DiscoveredNode;
    use loquax::ui::browse::BrowseEvent;
    use loquax::ui::explore::ExploreEvent;
    use loquax::ui::scene::TabId;

    const NOW: i64 = 1_800_000_000;
    let node = |byte: u8, name: Option<&str>, ago: i64, hops| DiscoveredNode {
        dest_hash: [byte; 16],
        name: name.map(str::to_string),
        first_seen: (NOW - ago) as u64,
        last_seen: (NOW - ago) as u64,
        hops,
    };
    let mut model = UiModel::<Line>::new(&[]);
    model.address = Some([0x30; 16]);
    model.browse.enabled = true;
    model.now = Some(loquax::when::WallTime {
        unix_secs: NOW,
        offset_secs: 3_600,
    });
    for found in [
        node(0x5a, Some("Marcel's node"), 600, Some(2)),
        node(0x7c, Some("Relay Süd"), 2 * 86_400, Some(1)),
        // Five days: a page silent for a week is not listed (1644).
        node(0x3d, None, 5 * 86_400, None),
        node(0x41, Some("Bergwacht"), 5 * 3_600, Some(4)),
    ] {
        update(&mut model, UiEvent::Browse(BrowseEvent::Node(found)));
    }
    model.browse.model.bookmarks.add(lnomad::Bookmark {
        url: format!("{}:/page/index.mu", "7c".repeat(16)),
        title: "Relay Süd".to_string(),
    });
    update(
        &mut model,
        UiEvent::Rooms(loquax::ui::rooms::RoomsEvent::Heard(
            loquax::ui::hubs::HeardHub {
                hash: [0x99; 16],
                name: Some("Werkstatt".to_string()),
                hops: Some(3),
                first: NOW - 60,
                last: NOW - 60,
                answered: None,
            },
        )),
    );
    update(&mut model, UiEvent::Tab(TabId::Explore));
    check("scene-explore.json", &model.scene(LayoutClass::Compact));
    check("scene-explore-wide.json", &model.scene(LayoutClass::Wide));
    update(&mut model, UiEvent::Explore(ExploreEvent::Az(true)));
    // A search finds a name, never a hash's letters (1618).
    update(
        &mut model,
        UiEvent::Explore(ExploreEvent::Typed("marcel".to_string())),
    );
    check(
        "scene-explore-search.json",
        &model.scene(LayoutClass::Compact),
    );
    // A server's row opens its sheet and changes nothing (1618).
    update(
        &mut model,
        UiEvent::Explore(ExploreEvent::Typed(String::new())),
    );
    update(&mut model, UiEvent::Explore(ExploreEvent::Hub([0x99; 16])));
    assert_eq!(model.rooms.hub, None);
    check(
        "scene-explore-sheet.json",
        &model.scene(LayoutClass::Compact),
    );
}

/// 1337: the Browse tab as the phone and the M9 get it, lnomad's scene
/// inside Loquax's: the list it opens on (two NomadNet nodes the announces
/// named), a page from the first, and the same page with its form field
/// taking text. The page is lnomad's sample fixture, laid out on the M9's
/// 53 by 24 cells.
#[test]
fn the_browse_tab_is_the_golden_scene() {
    use lnomad::app::scene::NodeId;
    use lnomad::{DiscoveredNode, Intent};
    use loquax::ui::browse::BrowseEvent;
    use loquax::ui::scene::TabId;

    const MARCEL: [u8; 16] = [0x5a; 16];
    const RELAY: [u8; 16] = [0x7c; 16];
    let node = |dest_hash, name: &str, hops| DiscoveredNode {
        dest_hash,
        name: Some(name.to_string()),
        first_seen: 1_800_000_000,
        last_seen: 1_800_000_000,
        hops: Some(hops),
    };
    let mut model = UiModel::<Line>::new(&[]);
    model.address = Some([0x30; 16]);
    model.browse.enabled = true;
    for found in [
        node(MARCEL, "Marcel's node", 2),
        node(RELAY, "Relay Süd", 1),
    ] {
        update(&mut model, UiEvent::Browse(BrowseEvent::Node(found)));
    }
    update(&mut model, UiEvent::Tab(TabId::Browse));
    check("scene-browse-list.json", &model.scene(LayoutClass::Compact));

    let row = lnomad::app::places(&model.browse.model)
        .iter()
        .position(
            |p| matches!(p, lnomad::app::Place::Node { dest_hash, .. } if *dest_hash == MARCEL),
        )
        .expect("Marcel's row");
    let browse = |model: &mut UiModel<Line>, intent| {
        update(model, UiEvent::Browse(BrowseEvent::Intent(intent)))
    };
    browse(&mut model, Intent::Activate(NodeId::Place(row)));
    let micron = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../lnomad/tests/fixtures/sample.mu"),
    )
    .expect("lnomad's sample page");
    update(
        &mut model,
        UiEvent::Browse(BrowseEvent::Loaded {
            micron,
            title: "Marcel's node".to_string(),
        }),
    );
    check("scene-browse-page.json", &model.scene(LayoutClass::Compact));

    // The phone's editor over the field hands its text in.
    browse(&mut model, Intent::Activate(NodeId::Field(1)));
    browse(
        &mut model,
        Intent::Fill {
            field: 1,
            text: "Lew".to_string(),
        },
    );
    check("scene-browse-form.json", &model.scene(LayoutClass::Compact));
}

/// 1443: the profile as every target gets it, the phone's facade too:
/// the reader of the ugly store with a contact code, a mailbox on a
/// server reached over the relay and pulling, and two devices. The
/// avatar of the list's bar raises the profile, and the profile's
/// scene is the golden; its QR is the code's modules row by row. The QR
/// is `home::qr`, behind the `qr` feature this target does not require.
#[cfg(feature = "qr")]
#[test]
fn the_profile_is_the_golden_scene() {
    use loquax::ui::intent::Intent;
    use loquax::ui::profile::{Device, Mailbox, ProfileFacts, Rung};
    use loquax::ui::scene::NodeId;

    let home = tempfile::tempdir().expect("home");
    let mut model = ugly_model(home.path());
    model.selected = NONE_OPEN;
    let list = model.scene(LayoutClass::Compact);
    assert!(list.profile.is_none(), "control: closed");
    let open = list.action(NodeId::Me).expect("the avatar opens");
    assert_eq!(open.intent, Intent::Profile);

    let own = ugly_store::identity(OWN);
    let code = loquax::lxma::encode(&own.public_key_bytes()).expect("a code");
    let qr = loquax::home::qr::verbatim_modules(&code).expect("a QR");
    model.own_code = Some(loquax::ui::OwnCode::of(code, Some(qr)));
    update(
        &mut model,
        UiEvent::ProfileFacts(Box::new(ProfileFacts {
            name: "Mia Sander".to_string(),
            mailbox: Mailbox::Away {
                name: Some("Mia's computer".to_string()),
                rung: Some(Rung::Relayed),
                word: Some(loquax::home::board::Word::Fetched {
                    taken: 3,
                    of: Some(10),
                }),
            },
            devices: vec![
                Device {
                    name: "Pixel 8".to_string(),
                    this: true,
                },
                Device {
                    name: "Laptop".to_string(),
                    this: false,
                },
            ],
            ..Default::default()
        })),
    );
    update(&mut model, UiEvent::Profile { open: true });
    let scene = model.scene(LayoutClass::Compact);
    assert!(scene.profile.is_some());
    check("scene-profile.json", &scene);
    update(&mut model, UiEvent::Profile { open: false });
    assert!(model.scene(LayoutClass::Compact).profile.is_none());
}

/// The ladder of `docs/src/concepts/loquax-read-receipts.md`, section 3,
/// in one chat (1462): waiting, at the mailbox, arrived (the proof, one
/// tick), on the device (the device's receipt `0x07`, two ticks), read and
/// not sent, each a bubble of ours with its mark and its words. Every
/// target draws what this scene says, so one golden pins the rule for all
/// four.
#[test]
fn the_ladder_is_the_golden_scene() {
    use leviculum_lxmf::lev::{Lev, PropagationReceipt, Receipt};
    use leviculum_lxmf::router::MessageState;
    use loquax::outbox::OutboxEvent;

    let peer = ugly_store::peer(1);
    let mut model = UiModel::<Line>::new(&[peer]);
    update(&mut model, UiEvent::Clock(now()));
    let event = |model: &mut UiModel<Line>, event: OutboxEvent| {
        update(model, UiEvent::Engine(event));
    };
    let received = |model: &mut UiModel<Line>, n: u8, lev: Lev| {
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: [0xe0 + n; 32],
                source: peer,
                title: Vec::new(),
                body: Vec::new(),
                fields: lev.fields().to_vec(),
                verified: true,
            }),
        );
    };
    let rungs: [(&str, &[MessageState]); 6] = [
        ("waiting", &[]),
        ("at the mailbox", &[MessageState::AwaitingCollection]),
        ("arrived, the proof", &[MessageState::Delivered]),
        ("on the device", &[MessageState::Delivered]),
        ("read", &[MessageState::Delivered]),
        ("not sent", &[MessageState::Failed]),
    ];
    for (n, (body, states)) in rungs.iter().enumerate() {
        let id = [0x10 + n as u8; 32];
        update(
            &mut model,
            UiEvent::Write {
                peer,
                body: body.to_string(),
            },
        );
        event(&mut model, OutboxEvent::Queued { message_id: id });
        update(&mut model, UiEvent::SendDone(Ok(())));
        for state in *states {
            event(
                &mut model,
                OutboxEvent::State {
                    message_id: id,
                    state: *state,
                },
            );
        }
    }
    let at = (now().unix_secs - 60) as u32;
    received(
        &mut model,
        1,
        Lev {
            propagation_receipt: Some(PropagationReceipt {
                received_at: at,
                ids: vec![[0x13; 32], [0x14; 32]],
            }),
            ..Lev::default()
        },
    );
    received(
        &mut model,
        2,
        Lev::mark_with(Receipt {
            read_at: at,
            ids: vec![[0x14; 32]],
        }),
    );
    model.selected = model
        .conversations
        .iter()
        .position(|c| c.peer == peer)
        .expect("the chat");
    let scene = model.scene(LayoutClass::Compact);
    let marks: Vec<String> = scene
        .chat
        .as_ref()
        .expect("the chat")
        .groups
        .iter()
        .flat_map(|g| &g.bubbles)
        .filter_map(|b| b.mark.as_ref().map(|m| m.mark.word().to_string()))
        .collect();
    assert_eq!(
        marks,
        [
            "sending",
            "mailbox",
            "arrived",
            "delivered",
            "read",
            "failed"
        ]
    );
    check("scene-ladder.json", &scene);
}

mod group_ticks {
    include!("support/group_ticks.rs");
}

/// 1466, the group's ticks tell the truth too: a group's chat with a line
/// of ours every member's device has (two ticks, `delivered`, its words
/// "On the devices") and one Ben's device has not said yet (one tick,
/// `arrived`, "Arrived"). Every target draws what this scene says.
#[test]
fn a_groups_one_and_two_ticks_are_the_golden_scene() {
    use loquax::ui::strings::{Lang, StrId, Text};

    let mut model = UiModel::<Line>::new(&[]);
    model.address = Some([0x30; 16]);
    group_ticks::group_ticks(&mut model, now().unix_secs, now().offset_secs);
    let scene = model.scene(LayoutClass::Compact);
    let marks: Vec<(String, String)> = scene
        .chat
        .as_ref()
        .expect("the chat")
        .groups
        .iter()
        .flat_map(|g| &g.bubbles)
        .filter_map(|b| {
            b.mark
                .as_ref()
                .map(|m| (m.mark.word().to_string(), m.words.resolve(Lang::De)))
        })
        .collect();
    assert_eq!(
        marks,
        [
            ("delivered".to_string(), "Auf den Geräten".to_string()),
            ("arrived".to_string(), "Angekommen".to_string()),
        ]
    );
    let words = Text::key(StrId::StateGroupDelivered);
    assert_eq!(words.resolve(Lang::En), "On the devices");
    check("scene-group-ticks.json", &scene);
}

mod relay_group {
    include!("support/relay_group.rs");
}

/// 1482, a relayed post shows its writer: a relay bot's chat, each post
/// under its writer and without the bot's prefix, the bar saying it is a
/// group via a relay, a reaction to a message this device does not hold
/// said in words, and a reply quoting a post by its writer. Every target
/// draws what this scene says.
#[test]
fn a_relayed_group_is_the_golden_scene() {
    use loquax::ui::strings::Lang;

    let mut model = UiModel::<Line>::new(&[]);
    model.address = Some([0x30; 16]);
    relay_group::relay_group(&mut model, now().unix_secs, now().offset_secs);
    let scene = model.scene(LayoutClass::Compact);
    let chat = scene.chat.as_ref().expect("the chat");
    let title = chat.bar.title.resolve(Lang::De);
    assert!(
        title.starts_with("ReticulumNet NL") && title.ends_with(" · Gruppe über Relais"),
        "{title}"
    );
    let groups: Vec<(String, Vec<String>)> = chat
        .groups
        .iter()
        .map(|g| {
            (
                g.sender.resolve(Lang::En),
                g.bubbles.iter().map(|b| b.bubble.body.clone()).collect(),
            )
        })
        .collect();
    assert_eq!(
        groups,
        [
            ("Anna".to_string(), vec!["Hallo zusammen".to_string()]),
            (
                "Ben".to_string(),
                vec![
                    "Moin Anna".to_string(),
                    "reacted 👍 to a message that is not here".to_string()
                ]
            ),
            ("You".to_string(), vec!["Ich bin auch da".to_string()]),
            (
                "Anna".to_string(),
                vec!["Schön, dass du da bist".to_string()]
            ),
        ]
    );
    let quote = chat
        .groups
        .last()
        .and_then(|g| g.bubbles[0].bubble.quote.clone())
        .map(|q| q.text);
    assert_eq!(quote.as_deref(), Some("Ben: Moin Anna"));
    check("scene-relay-group.json", &scene);
}

mod reply_scene {
    include!("support/reply_scene.rs");
}

/// 1451, the walk's blind spots: Anna's chat of the shared fixture, our
/// reply under the quote of her question, her thumbs up on it, her photo
/// as its stand-in; then a reply to her last line being written, which the
/// compose bar carries with the quote of the line it answers. Every
/// target's golden of this chat is drawn from this scene.
#[test]
fn a_reply_a_reaction_and_a_photo_are_the_golden_scene() {
    let mut model = UiModel::<Line>::new(&[]);
    model.address = Some([0x30; 16]);
    reply_scene::reply_scene(&mut model, now().unix_secs, now().offset_secs);
    let scene = model.scene(LayoutClass::Compact);
    let chat = scene.chat.as_ref().expect("the chat");
    let bubbles: Vec<_> = chat.groups.iter().flat_map(|g| g.bubbles.iter()).collect();
    let ours = bubbles
        .iter()
        .find(|b| b.bubble.ours)
        .expect("our reply is in the chat");
    assert_eq!(ours.bubble.body, "Ja, um zehn am Brunnen");
    assert_eq!(
        ours.bubble.quote.as_ref().map(|q| q.text.as_str()),
        Some("Kommst du morgen mit zum Markt?"),
        "our reply quotes her question"
    );
    assert_eq!(ours.bubble.chips.len(), 1, "her thumbs up on our reply");
    assert!(
        bubbles.iter().any(|b| b.photo.is_some()),
        "her photo is a bubble of its own"
    );
    check("scene-reply.json", &scene);

    reply_scene::reply_composing(&mut model);
    let scene = model.scene(LayoutClass::Compact);
    let compose = &scene.chat.as_ref().expect("the chat").compose;
    assert_eq!(
        compose.reply,
        Some(reply_scene::REPLY_LAST),
        "the compose bar carries the line it answers"
    );
    assert_eq!(compose.text, reply_scene::REPLY_DRAFT);
    check("scene-reply-compose.json", &scene);
}
