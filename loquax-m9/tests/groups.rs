//! A group's screens and keys on the M9, on the host (slice six, 1366,
//! `handheld-messenger.md`, "Slice six, host half"). The goldens of the
//! seven group screens are pinned with the board's in `firmware.rs`; this
//! holds what they rest on: every row of the group's key table runs the
//! action its scene node offers, the last key of every walk is in its
//! frame, each verb ends in the effect the board hands its home and in the
//! ask it makes of it, and one renderer walking all of it draws what a
//! fresh one draws.

use leviculum_lxmf::lev::{Lev, MEMBER_LEFT};
use loquax::home::ask::GroupVerb;
use loquax::ui::groups::Verb;
use loquax::ui::intent::Intent;
use loquax::ui::scene::Scene;
use loquax::ui::{UiEffect, UiModel};
use loquax_m9::field::Field;
use loquax_m9::fixture::{self, group_model, Screen, SCREENS};
use loquax_m9::frame::Frame;
use loquax_m9::group::{self, Hit, Place, ROWS};
use loquax_m9::keys::{event, M9Key};
use loquax_m9::render::Renderer;
use loquax_m9::screen::{self, Look, COLS};

fn look(model: &UiModel<Field>, row: usize) -> Look {
    Look {
        row,
        ..Look::of(model)
    }
}

fn drawn(model: &UiModel<Field>, row: usize) -> Frame {
    let mut frame = Frame::new();
    Renderer::new(&mut frame)
        .expect("renderer")
        .draw(&screen::scene(model), &look(model, row))
        .expect("draw");
    frame
}

/// The walk's model and cursor before its last key, and the last key.
fn before_last(shown: Screen) -> (UiModel<Field>, usize, M9Key) {
    let keys = shown.keys();
    let (last, walk) = keys.split_last().expect("a walk");
    let mut model = match shown {
        Screen::Chats | Screen::Chat => fixture::model(),
        _ => group_model(),
    };
    let mut row = 0;
    for key in walk {
        group::press(&mut model, &mut row, &event(*key));
    }
    (model, row, *last)
}

/// Where a key table row is proven: the screen whose walk leaves the keys
/// at its place.
fn screen_for(place: Place) -> Screen {
    match place {
        Place::List => Screen::Groups,
        Place::Chat => Screen::Invite,
        Place::Sheet => Screen::Admin,
        Place::Ask => Screen::Ask,
        Place::Pick => Screen::Pick,
        Place::Name => Screen::Name,
        Place::Other => unreachable!("no row is the compose line's"),
    }
}

/// What each row must do on its screen, read off the scene's nodes: the
/// action a tap on the node runs, or the cursor's move.
fn wanted(verb: &str, place: Place, scene: &Scene, row: usize) -> Hit {
    let run = |intent: &Intent| Hit::Run(intent.clone());
    let sheet = || scene.members.as_ref().expect("the sheet");
    let new = || scene.new_group.as_ref().expect("New group");
    match (place, verb) {
        (Place::List, "new group") => Hit::Run(Intent::Group(Verb::New)),
        (Place::Chat, "members") => run(&scene.chat.as_ref().expect("chat").bar.actions[0].intent),
        (Place::Chat, "join") => {
            let card = scene.chat.as_ref().and_then(|c| c.invite.as_ref());
            run(&card.and_then(|i| i.join.as_ref()).expect("Join").intent)
        }
        (Place::Sheet | Place::Pick, "up") => Hit::Move(row.saturating_sub(1)),
        (Place::Sheet | Place::Pick, "down") => Hit::Move(row + 1),
        (Place::Sheet, "leave") => run(&sheet().leave.as_ref().expect("leave").intent),
        (Place::Sheet, "add members") => run(&sheet().add.as_ref().expect("add").intent),
        (Place::Sheet, "remove") => run(&sheet().rows[row].remove.as_ref().expect("rm").intent),
        (Place::Sheet, "delete group") => run(&sheet().delete.as_ref().expect("delete").intent),
        // An admin's sheet of a group still on offers no Start again.
        (Place::Sheet, "start again") => Hit::Stay,
        (Place::Sheet, "close") => run(&sheet().close.intent),
        (Place::Ask, "yes") => run(&sheet().ask.as_ref().expect("ask").yes.intent),
        (Place::Ask, "no") => run(&sheet().ask.as_ref().expect("ask").no.intent),
        (Place::Pick, "tick") => run(&new().rows[row].toggle.intent),
        (Place::Pick, "next") => run(&new().next.intent),
        (Place::Pick | Place::Name, "close") => run(&new().close.intent),
        (Place::Name, "type the name") => {
            Hit::Run(Intent::Group(Verb::Name(format!("{}a", new().name))))
        }
        (Place::Name, "create") => run(&new().create.intent),
        other => panic!("no want for {other:?}"),
    }
}

/// Every row of the group's key table, on the screen of its place, runs
/// the action its node offers: the M9 binds the scene's verbs, no new one.
#[test]
fn every_group_key_runs_the_action_its_node_offers() {
    for key_row in ROWS {
        let shown = screen_for(key_row.place);
        let (model, row) = shown.walk();
        assert_eq!(group::Place::of(&model), key_row.place, "{shown:?}");
        let scene = screen::scene(&model);
        let got = group::hit(
            &scene,
            key_row.place,
            &look(&model, row),
            &event(key_row.key),
        );
        let want = wanted(key_row.verb, key_row.place, &scene, row);
        assert_eq!(got, Some(want), "{} ({:?})", key_row.verb, key_row.key);
    }
    // The control: a key the group's screens leave alone goes to the
    // model, and a key on a plain chat is the chat's.
    let (model, row) = Screen::Chat.walk();
    let scene = screen::scene(&model);
    assert_eq!(
        group::hit(
            &scene,
            Place::Chat,
            &look(&model, row),
            &event(M9Key::Char('m'))
        ),
        None,
        "a peer's chat has no member sheet"
    );
    assert_eq!(
        group::hit(
            &scene,
            Place::Chat,
            &look(&model, row),
            &event(M9Key::Enter)
        ),
        None,
        "a peer's chat has no Join"
    );
}

/// The last key of every group screen's walk changes its frame, so each
/// golden pins the screen it names; Groups has no keys, its control is
/// that it differs from the board's first list.
#[test]
fn the_last_key_of_every_group_walk_is_in_its_frame() {
    for shown in SCREENS {
        // The map's walks are `tests/map.rs`'s.
        if matches!(
            shown,
            Screen::Chats | Screen::Chat | Screen::Groups | Screen::Map | Screen::Pan
        ) {
            continue;
        }
        let (model, row) = shown.walk();
        let (before, before_row, last) = before_last(shown);
        assert_ne!(
            drawn(&model, row).pixels(),
            drawn(&before, before_row).pixels(),
            "{shown:?}: {last:?} changed nothing"
        );
    }
    assert_ne!(
        drawn(&Screen::Groups.model(), 0).pixels(),
        drawn(&Screen::Chats.model(), 0).pixels()
    );
}

/// What the group's screens put in cells, word for word, and the cursor's
/// bar on the row it stands on.
#[test]
fn the_group_screens_say_what_the_scene_carries() {
    let text = |shown: Screen| -> Vec<String> {
        // The words held here are the catalogue's English; the board
        // speaks German by default (1447), the goldens show it.
        let (mut model, row) = shown.walk();
        model.lang = loquax::ui::strings::Lang::En;
        screen::lines(
            &screen::scene(&model),
            &look(&model, row),
            COLS,
            screen::ROWS,
        )
        .iter()
        .map(|l| l.cells.iter().map(|c| c.ch).collect())
        .collect()
    };
    // A sentence may wrap over rows at 32 cells: the rows read as one
    // text, a space between two, the padding folded.
    let has = |rows: &[String], words: &str| {
        let text: Vec<&str> = rows.iter().map(|r| r.trim()).collect();
        rows.iter().any(|r| r.contains(words)) || text.join(" ").contains(words)
    };

    let list = text(Screen::Groups);
    assert!(
        list.iter().any(|r| r.starts_with("  4   Kletterhalle")),
        "{list:#?}"
    );
    assert!(
        list.iter().any(|r| r.starts_with("  3   Familie")),
        "{list:#?}"
    );

    let invite = text(Screen::Invite);
    for words in [
        "Jürgen invited you to Chor",
        "Jürgen could read",
        "[Enter] Join",
    ] {
        assert!(has(&invite, words), "{words:?}: {invite:#?}");
    }

    let member = text(Screen::Members);
    assert!(member[0].contains("Familie") && member[0].contains("3 members"));
    assert!(has(&member, "Ünal") && has(&member, "left"), "{member:#?}");
    assert!(has(&member, "[l] Leave group"), "{member:#?}");
    for admin_only in ["[a]", "[r]", "[d]"] {
        assert!(!has(&member, admin_only), "a member has no {admin_only}");
    }

    let admin = text(Screen::Admin);
    assert!(
        admin[2].starts_with("▌ Zoë Ærø"),
        "the cursor on Zoë: {admin:#?}"
    );
    for words in [
        "[a] Add members",
        "[r] Remove Zoë Ærø",
        "[d] Delete group",
        "invited",
    ] {
        assert!(has(&admin, words), "{words:?}: {admin:#?}");
    }

    let ask = text(Screen::Ask);
    assert!(has(&ask, "Delete Kletterhalle for everyone?"), "{ask:#?}");
    assert!(has(&ask, "[y] Yes   [n] No"), "{ask:#?}");
    assert!(!has(&ask, "[d]"), "the question replaces the verbs");
    // Control for the joined reading: words that are on no row do not
    // turn up between two.
    assert!(!has(&ask, "everyone? everyone?"), "{ask:#?}");

    let pick = text(Screen::Pick);
    assert!(pick[0].contains("2 selected"), "{pick:#?}");
    assert!(pick[1].starts_with(" [x]") && pick[2].starts_with(" [x]"));
    assert!(
        pick[3].starts_with("▌[ ]"),
        "the cursor on the third: {pick:#?}"
    );

    let name = text(Screen::Name);
    assert!(name[3].starts_with(" Hütte "), "{name:#?}");
    assert!(has(&name, "Hütte is kept on your mailbox"), "{name:#?}");
}

/// Press `keys` on `model` as the board does, and keep what it hands out.
fn press_all(model: &mut UiModel<Field>, row: &mut usize, keys: &[M9Key]) -> Vec<UiEffect> {
    keys.iter()
        .flat_map(|key| group::press(model, row, &event(*key)))
        .collect()
}

fn state_of(effect: &UiEffect) -> Option<Lev> {
    match effect {
        UiEffect::Send { body, fields, .. } if body.is_empty() => Lev::from_fields(fields),
        _ => None,
    }
}

/// Every verb on the board's keys ends in the effect the desktop's shell
/// hands its home, and [`group::ask_of`] makes that the ask the board's
/// executor runs: create, join, leave, add, remove, delete.
#[test]
fn each_group_verb_ends_in_the_ask_the_board_makes_of_its_home() {
    use M9Key::{Back, Char, Enter};
    let id = [7; 16];

    // Create: the Name walk and Enter.
    let (mut model, mut row) = Screen::Name.walk();
    let made = press_all(&mut model, &mut row, &[Enter]);
    let [UiEffect::CreateGroup { name, members }] = made.as_slice() else {
        panic!("Enter on the name creates: {made:?}");
    };
    assert_eq!(name, "Hütte");
    assert_eq!(members.len(), 2);
    let Some(GroupVerb::Create(request)) = group::ask_of(&made[0], id) else {
        panic!("a /group");
    };
    assert_eq!((request.client_id, request.name.as_str()), (id, "Hütte"));

    // Join: Enter on Chor's card is the Join's mark alone, sent to Chor.
    let (mut model, mut row) = Screen::Invite.walk();
    let joined = press_all(&mut model, &mut row, &[Enter]);
    let [join @ UiEffect::Send { to, .. }] = joined.as_slice() else {
        panic!("Enter on the card joins: {joined:?}");
    };
    assert_eq!(*to, fixture::CHOR);
    assert!(state_of(join).is_some_and(|lev| lev.members.is_none()));
    let Some(GroupVerb::State(submission)) = group::ask_of(join, id) else {
        panic!("a state message");
    };
    assert_eq!(submission.destination, fixture::CHOR);

    // Leave: l and y on Familie's sheet, the membership naming the reader
    // as left; then the sheet of a left group offers Start again.
    let (mut model, mut row) = Screen::Members.walk();
    let left = press_all(&mut model, &mut row, &[Char('l'), Char('y')]);
    let lev = left.iter().find_map(state_of).expect("a state message");
    let gone = &lev.members.expect("a membership").members[0];
    assert_eq!((gone.address, gone.flags), (fixture::OWN, MEMBER_LEFT));
    assert!(matches!(
        group::ask_of(&left[0], id),
        Some(GroupVerb::State(_))
    ));
    press_all(&mut model, &mut row, &[Char('m')]);
    let again = press_all(&mut model, &mut row, &[Char('s')]);
    assert!(again.is_empty());
    assert_eq!(
        group::Place::of(&model),
        Place::Name,
        "Start again names it"
    );

    // Add: a on Kletterhalle's sheet, a friend ticked, Enter.
    let (mut model, mut row) = Screen::Admin.walk();
    let added = press_all(&mut model, &mut row, &[Char('a'), Char(' '), Enter]);
    let [UiEffect::AddMembers { group: g, members }] = added.as_slice() else {
        panic!("Enter on the ticked friend adds: {added:?}");
    };
    assert_eq!((*g, members.len()), (fixture::KLETTERHALLE, 1));
    assert!(matches!(
        group::ask_of(&added[0], id),
        Some(GroupVerb::Add(_))
    ));

    // Remove: r on Zoë, n keeps her, r and y remove her.
    let (mut model, mut row) = Screen::Admin.walk();
    let kept = press_all(&mut model, &mut row, &[Char('r'), Char('n')]);
    assert!(kept.is_empty(), "No asks nothing of the home");
    let removed = press_all(&mut model, &mut row, &[Char('r'), Char('y')]);
    let lev = removed.iter().find_map(state_of).expect("a state message");
    let gone = &lev.members.expect("a membership").members[0];
    assert_eq!(
        (gone.address, gone.flags),
        (fixture::peer(0x43), MEMBER_LEFT)
    );

    // Delete: the Ask screen and y.
    let (mut model, mut row) = Screen::Ask.walk();
    let deleted = press_all(&mut model, &mut row, &[Char('y')]);
    assert_eq!(
        deleted,
        vec![UiEffect::DeleteGroup {
            group: fixture::KLETTERHALLE
        }]
    );
    let Some(GroupVerb::Delete(request)) = group::ask_of(&deleted[0], id) else {
        panic!("a /groupdelete");
    };
    assert_eq!(request.group, fixture::KLETTERHALLE);

    // The control: a plain message is no group's ask, and Back on every
    // group screen closes it and asks nothing.
    let (mut model, mut row) = Screen::Chat.walk();
    let sent = press_all(&mut model, &mut row, &[Char('i'), Char('x'), Enter]);
    assert!(sent.iter().any(|e| matches!(e, UiEffect::Send { .. })));
    assert!(sent.iter().all(|e| group::ask_of(e, id).is_none()));
    for shown in [Screen::Members, Screen::Pick, Screen::Name] {
        let (mut model, mut row) = shown.walk();
        assert!(press_all(&mut model, &mut row, &[Back]).is_empty());
        assert_ne!(group::Place::of(&model), group::Place::of(&shown.model()));
    }
}

/// The redraw rule of slice one over the group's screens: one renderer
/// walking from the list into New group, out, into an admin's sheet, its
/// question and back, draws what a fresh renderer draws at every step.
#[test]
fn a_group_redraw_is_the_frame_a_fresh_draw_gives() {
    use M9Key::{Back, Char, Down, Enter, Up};
    let mut model = group_model();
    let mut row = 0;
    let mut frame = Frame::new();
    let mut renderer = Renderer::new(&mut frame).expect("renderer");
    let walk = [
        Char('n'),
        Down,
        Char(' '),
        Down,
        Char(' '),
        Up,
        Enter,
        Char('Ö'),
        Char('l'),
        Back,
        Back,
        Down,
        Enter,
        Char('m'),
        Down,
        Down,
        Char('d'),
        Char('n'),
        Up,
        Back,
    ];
    for key in walk {
        group::press(&mut model, &mut row, &event(key));
        renderer
            .draw(&screen::scene(&model), &look(&model, row))
            .expect("draw");
        assert!(
            renderer.panel().pixels() == drawn(&model, row).pixels(),
            "after {key:?} the redraw differs"
        );
    }
}
