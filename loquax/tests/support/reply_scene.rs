// 1451, the walk's blind spots: one chat for the goldens of every target
// (the window, the terminal, the M9) that shows a reply, a reaction and a
// photo, and a second reply being written. Anna wrote "Kommst du morgen
// mit zum Markt?" at 09:12 and sent a photo of the stall at 09:13; we
// replied to her question at 09:20 ("Ja, um zehn am Brunnen"), which went
// as a reply and reached her device; she put a thumbs up on our reply at
// 09:21 and wrote "Super, bis morgen" at 09:22. The clock stands at 09:30.
// `reply_composing` then starts a reply to her last line, "Ich bring
// Kaffee mit", typed and not sent, as a frontend's field tells the model.
//
// Included with `include!`, not as a module, so the crate's own tests can
// use it too: an includer inside loquax says `use crate as loquax;` first.
// No inner doc comments here for that reason.

use leviculum_lxmf::router::MessageState;
use leviculum_ui::input::InputBuffer;
use loquax::outbox::OutboxEvent;
use loquax::ui::{update, UiEvent, UiModel};
use loquax::when::WallTime;

/// Anna, who asks, sends the photo and reacts.
pub const REPLY_ANNA: [u8; 16] = [0x7a; 16];
/// Her question, 09:12.
pub const REPLY_ASKED: [u8; 32] = [0xc1; 32];
/// Her photo, 09:13.
pub const REPLY_PHOTO: [u8; 32] = [0xc2; 32];
/// Our reply to her question, 09:20.
pub const REPLY_OURS: [u8; 32] = [0xc3; 32];
/// Her thumbs up on our reply, 09:21.
pub const REPLY_REACTION: [u8; 32] = [0xc4; 32];
/// Her last line, 09:22, which `reply_composing` answers.
pub const REPLY_LAST: [u8; 32] = [0xc5; 32];
/// The reply being written.
pub const REPLY_DRAFT: &str = "Ich bring Kaffee mit";

/// What Anna sent at `sent_at`, as the mailbox hands it on.
fn reply_received(
    id: [u8; 32],
    sent_at: i64,
    body: &str,
    fields: Vec<(i64, Vec<u8>)>,
) -> UiEvent {
    UiEvent::Engine(OutboxEvent::Received {
        sent_at,
        message_id: id,
        source: REPLY_ANNA,
        title: Vec::new(),
        body: body.as_bytes().to_vec(),
        fields,
        verified: true,
    })
}

/// Put Anna's chat into `model`, today's midnight at `midnight` (unix
/// seconds, `offset_secs` from UTC, local), the clock at 09:30, her chat
/// open. Anna's conversation, by its place in the list.
pub fn reply_scene<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    midnight: i64,
    offset_secs: i32,
) -> usize {
    use leviculum_lxmf::constants::FIELD_REACTION;
    use leviculum_lxmf::reaction::Reaction;
    let at = |hours: i64, minutes: i64| {
        midnight - i64::from(offset_secs) + hours * 3_600 + minutes * 60
    };
    let clock = |unix_secs: i64| {
        UiEvent::Clock(WallTime {
            unix_secs,
            offset_secs,
        })
    };
    model.names.remember(REPLY_ANNA, "Anna");
    update(model, clock(at(9, 12)));
    update(
        model,
        reply_received(
            REPLY_ASKED,
            at(9, 12),
            "Kommst du morgen mit zum Markt?",
            Vec::new(),
        ),
    );
    update(model, UiEvent::Open { peer: REPLY_ANNA });
    update(model, clock(at(9, 13)));
    update(
        model,
        reply_received(
            REPLY_PHOTO,
            at(9, 13),
            "",
            loquax::ui::photos::photo_fields("jpg", vec![7; 184_320]),
        ),
    );
    update(model, clock(at(9, 20)));
    update(
        model,
        UiEvent::Reply {
            peer: REPLY_ANNA,
            target: REPLY_ASKED,
            body: "Ja, um zehn am Brunnen".into(),
        },
    );
    update(
        model,
        UiEvent::Engine(OutboxEvent::Queued {
            message_id: REPLY_OURS,
        }),
    );
    update(model, UiEvent::SendDone(Ok(())));
    update(
        model,
        UiEvent::Engine(OutboxEvent::State {
            message_id: REPLY_OURS,
            state: MessageState::Delivered,
        }),
    );
    update(model, clock(at(9, 21)));
    let thumbs = Reaction {
        to: REPLY_OURS,
        content: "\u{1F44D}".into(),
    }
    .encode_field_value();
    update(
        model,
        reply_received(
            REPLY_REACTION,
            at(9, 21),
            "",
            Vec::from([(FIELD_REACTION, thumbs)]),
        ),
    );
    update(model, clock(at(9, 22)));
    update(
        model,
        reply_received(REPLY_LAST, at(9, 22), "Super, bis morgen", Vec::new()),
    );
    update(model, clock(at(9, 30)));
    model
        .conversations
        .iter()
        .position(|c| c.peer == REPLY_ANNA)
        .unwrap_or(0)
}

/// A reply to Anna's last line being written: [`REPLY_DRAFT`] typed and
/// not sent, as a frontend's own field tells the model.
pub fn reply_composing<B: InputBuffer + Default>(model: &mut UiModel<B>) {
    update(
        model,
        UiEvent::Draft {
            peer: REPLY_ANNA,
            text: REPLY_DRAFT.into(),
            reply: Some(REPLY_LAST),
        },
    );
}
