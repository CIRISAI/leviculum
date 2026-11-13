//! The client session against the recorded hub: what it sends is the
//! oracle client's bytes when given the same id and time, and the hub's
//! recorded answers mean what the cell saw.

use leviculum_rrc::{Envelope, Event, LineKind, Refusal, Session};
use serde_json::Value as Json;

fn vector(name: &str) -> (Vec<u8>, Envelope) {
    let file: Json = serde_json::from_str(include_str!("vectors/envelopes.json")).unwrap();
    let v = file["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap_or_else(|| panic!("no vector {name}"))
        .clone();
    let hex = v["cbor"].as_str().unwrap();
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    let envelope = Envelope::decode(&bytes).unwrap();
    (bytes, envelope)
}

fn id_of(e: &Envelope) -> [u8; 8] {
    e.id.as_slice().try_into().unwrap()
}

fn me(e: &Envelope) -> [u8; 16] {
    e.src.as_deref().unwrap().try_into().unwrap()
}

/// Alice's session as the cell ran it, welcomed and in `#lab`.
fn alice() -> Session {
    let (_, hello) = vector("hello_up");
    let mut session = Session::new(me(&hello), Some("alice".into()), "periculum-rrc", "0.1");
    let (welcome, _) = vector("welcome_down");
    session.receive(&welcome, [0; 8], 0).unwrap();
    let (joined, _) = vector("joined_down");
    session.receive(&joined, [0; 8], 0).unwrap();
    session
}

#[test]
fn what_the_session_sends_is_the_oracle_clients_bytes() {
    let (hello_bytes, hello) = vector("hello_up");
    let session = Session::new(me(&hello), Some("alice".into()), "periculum-rrc", "0.1");
    assert_eq!(session.hello(id_of(&hello), hello.ts), hello_bytes, "HELLO");
    assert_eq!(
        session.join("#lab", None, [0; 8], 0),
        Err(Refusal::NotReady),
        "no JOIN before WELCOME"
    );

    let mut session = alice();
    let (join_bytes, join) = vector("join_up");
    assert_eq!(
        Session::new(me(&join), None, "x", "y").join("#lab", None, id_of(&join), join.ts),
        Err(Refusal::NotReady)
    );
    let mut welcomed = Session::new(me(&join), None, "x", "y");
    welcomed
        .receive(&vector("welcome_down").0, [0; 8], 0)
        .unwrap();
    assert_eq!(
        welcomed.join("#LAB ", None, id_of(&join), join.ts).unwrap(),
        join_bytes
    );

    let (msg_bytes, msg) = vector("msg_up");
    let sent = session
        .say(
            "#lab",
            LineKind::Msg,
            msg.text().unwrap(),
            id_of(&msg),
            msg.ts,
        )
        .unwrap();
    assert_eq!(sent, msg_bytes, "MSG");

    // Bob's: a keyed JOIN, a NOTICE, an ACTION, a PING, a PART.
    let (key_bytes, key) = vector("join_up_2");
    let mut bob = Session::new(me(&key), Some("bob".into()), "periculum-rrc", "0.1");
    bob.receive(&vector("welcome_down").0, [0; 8], 0).unwrap();
    assert_eq!(
        bob.join("#lab", Some("sesame"), id_of(&key), key.ts)
            .unwrap(),
        key_bytes
    );

    let (notice_bytes, notice) = vector("notice_up");
    let (action_bytes, action) = vector("action_up");
    let (ping_bytes, ping) = vector("ping_up");
    let (part_bytes, part) = vector("part_up");
    let mut bob = Session::new(me(&notice), Some("bob".into()), "periculum-rrc", "0.1");
    bob.receive(&vector("welcome_down").0, [0; 8], 0).unwrap();
    let mut joined = vector("joined_down").1;
    joined.nick = None;
    bob.receive(&joined.encode(), [0; 8], 0).unwrap();
    assert_eq!(
        bob.say(
            "#lab",
            LineKind::Notice,
            "a notice",
            id_of(&notice),
            notice.ts
        )
        .unwrap(),
        notice_bytes
    );
    assert_eq!(
        bob.say("#lab", LineKind::Action, "waves", id_of(&action), action.ts)
            .unwrap(),
        action_bytes
    );
    assert_eq!(bob.ping(ping.ts, id_of(&ping), ping.ts), ping_bytes);
    assert_eq!(bob.part("#lab", id_of(&part), part.ts).unwrap(), part_bytes);

    // The PONG to the hub's PING is the oracle's, body unchanged.
    let (pong_bytes, pong) = vector("pong_up");
    let got = session
        .receive(&vector("ping_down").0, id_of(&pong), pong.ts)
        .unwrap();
    assert_eq!(got.event, Event::Pinged);
    assert_eq!(got.reply, Some(pong_bytes));
}

#[test]
fn the_hubs_answers_mean_what_the_cell_saw() {
    let (_, hello) = vector("hello_up");
    let mut session = Session::new(me(&hello), Some("alice".into()), "periculum-rrc", "0.1");
    let got = session
        .receive(&vector("welcome_down").0, [0; 8], 0)
        .unwrap();
    let Event::Welcome {
        hub,
        version,
        limits,
    } = got.event
    else {
        panic!("a welcome");
    };
    assert_eq!(
        (hub.as_deref(), version.as_deref()),
        (Some("periculum"), Some("0.3.2"))
    );
    assert_eq!(
        (
            limits.nick,
            limits.room,
            limits.body,
            limits.rooms,
            limits.per_minute
        ),
        (32, 64, 350, 32, 240)
    );

    let got = session
        .receive(&vector("joined_down").0, [0; 8], 0)
        .unwrap();
    assert_eq!(
        got.event,
        Event::Joined {
            room: "#lab".into(),
            nick: None
        }
    );
    assert_eq!(session.rooms(), ["#lab"]);
    let got = session
        .receive(&vector("joined_down_2").0, [0; 8], 0)
        .unwrap();
    assert_eq!(
        got.event,
        Event::Joined {
            room: "#lab".into(),
            nick: Some("bob".into())
        }
    );

    let got = session
        .receive(&vector("notice_down").0, [0; 8], 0)
        .unwrap();
    assert!(
        matches!(got.event, Event::HubNotice { room: Some(_), .. }),
        "{:?}",
        got.event
    );
    let got = session
        .receive(&vector("notice_down_2").0, [0; 8], 0)
        .unwrap();
    assert!(matches!(
        got.event,
        Event::Line {
            kind: LineKind::Notice,
            ours: false,
            ..
        }
    ));
    let got = session
        .receive(&vector("action_down").0, [0; 8], 0)
        .unwrap();
    assert!(matches!(
        got.event,
        Event::Line {
            kind: LineKind::Action,
            ours: false,
            ..
        }
    ));
    let got = session.receive(&vector("pong_down").0, [0; 8], 0).unwrap();
    assert_eq!(got.event, Event::Pong);
    let got = session.receive(&vector("error_down").0, [0; 8], 0).unwrap();
    assert_eq!(
        got.event,
        Event::Error {
            room: Some("#lab".into()),
            text: "bad key (+k)".into()
        }
    );

    let got = session
        .receive(&vector("parted_down_2").0, [0; 8], 0)
        .unwrap();
    assert_eq!(
        got.event,
        Event::Parted {
            room: "#lab".into(),
            nick: Some("bob".into())
        }
    );
    assert_eq!(session.rooms(), ["#lab"], "bob left, not us");
    let got = session
        .receive(&vector("parted_down").0, [0; 8], 0)
        .unwrap();
    assert_eq!(
        got.event,
        Event::Parted {
            room: "#lab".into(),
            nick: None
        }
    );
    assert!(session.rooms().is_empty());
}

#[test]
fn the_hubs_echo_of_our_line_is_ours_and_anothers_is_not() {
    let mut session = alice();
    let (msg_bytes, msg) = vector("msg_down");
    // Before we sent it, the same bytes are someone's line (the control).
    let got = session.receive(&msg_bytes, [0; 8], 0).unwrap();
    assert!(matches!(got.event, Event::Line { ours: false, .. }));
    session
        .say(
            "#lab",
            LineKind::Msg,
            msg.text().unwrap(),
            id_of(&msg),
            msg.ts,
        )
        .unwrap();
    let got = session.receive(&msg_bytes, [0; 8], 0).unwrap();
    let Event::Line {
        ours, text, nick, ..
    } = got.event
    else {
        panic!("a line");
    };
    assert!(ours, "the echo of what we sent");
    assert_eq!(
        (text.as_str(), nick.as_deref()),
        ("rrc line, thirty-one bytes long", Some("alice"))
    );

    // Same id, another identity: not ours.
    let mut other = vector("msg_down").1;
    other.src = Some(vec![0x55; 16]);
    let got = session.receive(&other.encode(), [0; 8], 0).unwrap();
    assert!(matches!(got.event, Event::Line { ours: false, .. }));
}

#[test]
fn a_line_past_the_limit_is_refused_with_a_sentence_before_it_is_sent() {
    let mut session = alice();
    let long = "x".repeat(351);
    let refusal = session
        .say("#lab", LineKind::Msg, &long, [1; 8], 1)
        .unwrap_err();
    assert_eq!(
        refusal,
        Refusal::TooLong {
            bytes: 351,
            limit: 350
        }
    );
    assert_eq!(
        refusal.to_string(),
        "This line is 351 bytes long and a room takes at most 350. Shorten it."
    );
    // The control: at the limit it goes.
    assert!(session
        .say("#lab", LineKind::Msg, &"x".repeat(350), [1; 8], 1)
        .is_ok());
    // Bytes count, not characters: 175 two-byte letters are 350 bytes.
    assert!(session
        .say("#lab", LineKind::Msg, &"ä".repeat(175), [2; 8], 1)
        .is_ok());
    assert!(session
        .say("#lab", LineKind::Msg, &"ä".repeat(176), [3; 8], 1)
        .is_err());

    // A link narrower than the envelope refuses too.
    session.set_mdu(100);
    let refusal = session
        .say("#lab", LineKind::Msg, &"x".repeat(60), [4; 8], 1)
        .unwrap_err();
    assert!(
        matches!(refusal, Refusal::TooBig { mdu: 100, .. }),
        "{refusal:?}"
    );

    assert_eq!(
        session.say("#other", LineKind::Msg, "hi", [5; 8], 1),
        Err(Refusal::NotInRoom("#other".into()))
    );
    assert_eq!(
        session.say("#lab", LineKind::Msg, "  ", [5; 8], 1),
        Err(Refusal::Empty)
    );
}

/// 1652: `/list` goes as a MSG without a room, the command in its body,
/// which rrcd reads before it asks for a room; nothing goes before WELCOME.
#[test]
fn a_command_is_a_msg_without_a_room() {
    let (_, hello) = vector("hello_up");
    let fresh = Session::new(me(&hello), None, "x", "y");
    assert_eq!(
        fresh.command("/list", [1; 8], 1),
        Err(Refusal::NotReady),
        "no command before WELCOME"
    );
    let session = alice();
    let bytes = session.command("/list", [7; 8], 42).unwrap();
    let sent = Envelope::decode(&bytes).unwrap();
    assert_eq!(sent.kind, leviculum_rrc::Kind::Msg);
    assert_eq!(sent.room, None, "a command names no room");
    assert_eq!(sent.text(), Some("/list"));
    assert_eq!(sent.src.as_deref(), Some(&me(&hello)[..]));
    assert_eq!(
        session.command("list", [8; 8], 1),
        Err(Refusal::Empty),
        "a command starts with a slash"
    );
}

/// 1652: a room the hub lists with a space or a slash is joined as the
/// hub spells it; a line break is no name.
#[test]
fn a_room_name_with_a_space_is_the_hubs_to_allow() {
    assert_eq!(
        leviculum_rrc::room_name(" General Chat ", 64),
        Ok("general chat".to_string())
    );
    assert_eq!(
        leviculum_rrc::room_name("gmrs/ham", 64),
        Ok("gmrs/ham".to_string())
    );
    assert_eq!(leviculum_rrc::room_name("a\nb", 64), Err(Refusal::BadRoom));
    assert_eq!(leviculum_rrc::room_name("   ", 64), Err(Refusal::BadRoom));
    let session = alice();
    let bytes = session.join("general chat", None, [9; 8], 1).unwrap();
    let sent = Envelope::decode(&bytes).unwrap();
    assert_eq!(sent.room.as_deref(), Some("general chat"));
}
