// 1505, a late message sits at its sent time: one fixture for the goldens
// of every target (the terminal, the window, the phone's facade, the M9).
// Anna wrote "Gute Nacht" last night at 21:30 and "Bis gleich am Bahnhof"
// today at 13:55, we answered at 13:58; her "Kommst du morgen mit?",
// written last night at 22:10, reached the mailbox late and came at
// 14:05, while Bob's chat was open. Her chat runs in her order: last
// night's two under last night's day line, the late one second, with the
// `new` rule above it (it is new to the reader, though older than what
// was read), then today's line and today's two.
//
// Included with `include!`, not as a module, so the crate's own tests can
// use it too: an includer inside loquax says `use crate as loquax;` first.
// No inner doc comments here for that reason.

use leviculum_lxmf::router::MessageState;
use leviculum_ui::input::InputBuffer;
use loquax::outbox::OutboxEvent;
use loquax::ui::{update, UiEvent, UiModel};
use loquax::when::WallTime;

/// Anna, who wrote the late one.
pub const LATE_ANNA: [u8; 16] = [0x6a; 16];
/// Bob, whose chat was open when it came.
pub const LATE_BOB: [u8; 16] = [0x6b; 16];
/// Last night's first: 21:30.
pub const LATE_NIGHT: [u8; 32] = [0xb1; 32];
/// Today's: 13:55.
pub const LATE_TODAY: [u8; 32] = [0xb2; 32];
/// The late one: written last night at 22:10, taken today at 14:05.
pub const LATE_LATE: [u8; 32] = [0xb3; 32];
/// Ours, today at 13:58.
pub const LATE_OURS: [u8; 32] = [0xb4; 32];

/// What `from` wrote at `sent_at`, as the mailbox hands it on.
fn late_received(from: [u8; 16], id: [u8; 32], sent_at: i64, body: &str) -> UiEvent {
    UiEvent::Engine(OutboxEvent::Received {
        sent_at,
        message_id: id,
        source: from,
        title: Vec::new(),
        body: body.as_bytes().to_vec(),
        fields: Vec::new(),
        verified: true,
    })
}

/// Put Anna's and Bob's chats into `model`, today's midnight at `midnight`
/// (unix seconds, `offset_secs` from UTC, local), the clock at 14:06, and
/// open Anna's chat. Anna's conversation, by its place in the list.
pub fn late_message<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    midnight: i64,
    offset_secs: i32,
) -> usize {
    let at = |hours: i64, minutes: i64| midnight - i64::from(offset_secs) + hours * 3_600 + minutes * 60;
    let clock = |unix_secs: i64| {
        UiEvent::Clock(WallTime {
            unix_secs,
            offset_secs,
        })
    };
    model.names.remember(LATE_ANNA, "Anna");
    model.names.remember(LATE_BOB, "Bob");
    update(model, clock(at(-24 + 21, 30)));
    update(model, late_received(LATE_ANNA, LATE_NIGHT, at(-24 + 21, 30), "Gute Nacht"));
    update(model, UiEvent::Open { peer: LATE_ANNA });
    update(model, clock(at(13, 55)));
    update(
        model,
        late_received(LATE_ANNA, LATE_TODAY, at(13, 55), "Bis gleich am Bahnhof"),
    );
    update(model, clock(at(13, 58)));
    update(
        model,
        UiEvent::Write {
            peer: LATE_ANNA,
            body: "Bin unterwegs".into(),
        },
    );
    update(
        model,
        UiEvent::Engine(OutboxEvent::Queued {
            message_id: LATE_OURS,
        }),
    );
    update(model, UiEvent::SendDone(Ok(())));
    update(
        model,
        UiEvent::Engine(OutboxEvent::State {
            message_id: LATE_OURS,
            state: MessageState::Delivered,
        }),
    );
    update(model, clock(at(14, 0)));
    update(
        model,
        late_received(LATE_BOB, [0xb5; 32], at(14, 0), "Ich bin schon da"),
    );
    update(model, UiEvent::Open { peer: LATE_BOB });
    update(model, clock(at(14, 5)));
    update(
        model,
        late_received(LATE_ANNA, LATE_LATE, at(-24 + 22, 10), "Kommst du morgen mit?"),
    );
    update(model, clock(at(14, 6)));
    update(model, UiEvent::Open { peer: LATE_ANNA });
    model
        .conversations
        .iter()
        .position(|c| c.peer == LATE_ANNA)
        .unwrap_or(0)
}
