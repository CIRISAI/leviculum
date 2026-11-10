// 1482, a relayed post shows its writer: one fixture for the goldens of
// the scene, the terminal, the window and the phone's facade. A relay bot,
// "ReticulumNet NL", hands us Anna's and Ben's posts with its default
// prefix `Name\n<hash>\n` in front, their fields passed through: Anna's
// post, Ben's post, Ben's reaction to his own copy of Anna's post (an id
// this device does not hold), our own line, and Anna's reply to her copy
// of Ben's post, its quote with the relay's prefix in it.
//
// Included with `include!`, not as a module, so the crate's own tests can
// use it too: an includer inside loquax says `use crate as loquax;` first.
// No inner doc comments here for that reason.

use leviculum_lxmf::constants::{FIELD_REACTION, FIELD_REPLY_QUOTE, FIELD_REPLY_TO};
use leviculum_lxmf::lev::Lev;
use leviculum_lxmf::reaction::Reaction;
use leviculum_ui::input::InputBuffer;
use loquax::outbox::OutboxEvent;
use loquax::ui::{update, UiEvent, UiModel};
use loquax::when::WallTime;

/// The relay, "ReticulumNet NL".
pub const RELAY: [u8; 16] = [0x6a; 16];
/// Anna, a member writing through it.
pub const RELAY_ANNA: [u8; 16] = [0x6b; 16];
/// Ben, another.
pub const RELAY_BEN: [u8; 16] = [0x6c; 16];
/// Anna's post, as this device holds the relay's copy of it.
pub const RELAY_POST: [u8; 32] = [0xc1; 32];

/// Put the relay's chat into `model`, its clock at `at` (unix seconds,
/// `offset_secs` from UTC), and open it. The chat, by its place in the
/// list.
pub fn relay_group<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    at: i64,
    offset_secs: i32,
) -> usize {
    let clock = |minutes: i64| {
        UiEvent::Clock(WallTime {
            unix_secs: at + minutes * 60,
            offset_secs,
        })
    };
    model.names.remember(RELAY, "ReticulumNet NL");
    let prefix = |name: &str, address: &[u8; 16]| {
        format!("{name}\n<{}>\n", loquax::address::to_hex(address))
    };
    let mark = |writer: [u8; 16]| {
        Lev {
            capabilities: Some(7),
            writer: Some(writer),
            ..Lev::default()
        }
        .fields()
        .to_vec()
    };
    let bin = |bytes: &[u8]| {
        let mut value = Vec::new();
        leviculum_lxmf::msgpack::bin(&mut value, bytes);
        value
    };
    let elsewhere = [0x77; 32];
    let mut reaction = mark(RELAY_BEN);
    reaction.push((
        FIELD_REACTION,
        Reaction {
            to: elsewhere,
            content: "👍".into(),
        }
        .encode_field_value(),
    ));
    let mut reply = mark(RELAY_ANNA);
    reply.push((FIELD_REPLY_TO, bin(&[0x78; 32])));
    reply.push((
        FIELD_REPLY_QUOTE,
        bin(format!("{}Moin Anna", prefix("Ben", &RELAY_BEN)).as_bytes()),
    ));
    let posts = [
        (-20, RELAY_POST, prefix("Anna", &RELAY_ANNA) + "Hallo zusammen", mark(RELAY_ANNA)),
        (-15, [0xc2; 32], prefix("Ben", &RELAY_BEN) + "Moin Anna", mark(RELAY_BEN)),
        (-14, [0xc3; 32], prefix("Ben", &RELAY_BEN), reaction),
    ];
    for (minute, id, body, fields) in posts {
        update(model, clock(minute));
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: at + minute * 60,
                message_id: id,
                source: RELAY,
                title: Vec::new(),
                body: body.into_bytes(),
                fields,
                verified: true,
            }),
        );
    }
    update(model, clock(-10));
    update(
        model,
        UiEvent::Write {
            peer: RELAY,
            body: "Ich bin auch da".into(),
        },
    );
    update(
        model,
        UiEvent::Engine(OutboxEvent::Queued {
            message_id: [0xc4; 32],
        }),
    );
    update(model, UiEvent::SendDone(Ok(())));
    update(model, clock(-5));
    update(
        model,
        UiEvent::Engine(OutboxEvent::Received {
            sent_at: at - 300,
            message_id: [0xc5; 32],
            source: RELAY,
            title: Vec::new(),
            body: (prefix("Anna", &RELAY_ANNA) + "Schön, dass du da bist").into_bytes(),
            fields: reply,
            verified: true,
        }),
    );
    update(model, clock(0));
    let c = model
        .conversations
        .iter()
        .position(|c| c.peer == RELAY)
        .expect("the relay's chat");
    model.selected = c;
    c
}
