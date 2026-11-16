// 1466, the group's ticks tell the truth too: one fixture for the goldens
// of every target (the scene, the terminal, the window, the phone's facade,
// the M9). "Familie" with Anna and Ben, and two lines of ours in it: the
// first every member's device said it has (two ticks, "On the devices"),
// the second every copy proven but Ben's device silent (one tick,
// "Arrived"). The group's status (`lev` 0x03) carries the rungs.
//
// Included with `include!`, not as a module, so the crate's own tests can
// use it too: an includer inside loquax says `use crate as loquax;` first.
// No inner doc comments here for that reason.

use leviculum_lxmf::lev::{GroupStatus, Lev, MemberRung, CAP_GROUPS, CAP_READ_RECEIPTS};
use leviculum_lxmf::router::MessageState;
use leviculum_ui::input::InputBuffer;
use loquax::outbox::OutboxEvent;
use loquax::ui::groups::{GroupInfo, GroupMember, Standing};
use loquax::ui::{update, UiEvent, UiModel};
use loquax::when::WallTime;

/// "Familie".
pub const TICKS_GROUP: [u8; 16] = [0x5a; 16];
/// Anna, a member.
pub const TICKS_ANNA: [u8; 16] = [0x5b; 16];
/// Ben, a member.
pub const TICKS_BEN: [u8; 16] = [0x5c; 16];
/// Our line on every member's device: two ticks.
pub const TICKS_TWO: [u8; 32] = [0xa2; 32];
/// Our line every copy of which is proven, Ben's device silent: one tick.
pub const TICKS_ONE: [u8; 32] = [0xa1; 32];

/// The status the group's home sends of our line `original`, at `at`:
/// both copies proven, `on_device` of them on a device (Anna's first).
fn ticks_status(original: [u8; 32], at: i64, on_device: u64) -> Vec<leviculum_lxmf::Field> {
    let at = |plus: i64| u32::try_from(at + plus).unwrap_or(0);
    let rung = |member, n: u64, device| MemberRung {
        member,
        proven_at: at(5),
        on_device_at: match on_device > n {
            true => at(device),
            false => 0,
        },
        read_at: 0,
    };
    Lev {
        capabilities: Some(CAP_READ_RECEIPTS | CAP_GROUPS),
        status: Some(GroupStatus {
            original,
            delivered: 2,
            read: 0,
            members: 2,
            readers: 2,
            on_device: Some(on_device),
            rungs: vec![rung(TICKS_ANNA, 0, 60), rung(TICKS_BEN, 1, 120)],
        }),
        ..Lev::default()
    }
    .fields()
    .to_vec()
}

/// Put "Familie" and our two lines into `model`, its clock at `at`
/// (unix seconds, `offset_secs` from UTC), and open the group's chat. The
/// group's conversation, by its place in the list.
pub fn group_ticks<B: InputBuffer + Default>(
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
    model.names.remember(TICKS_GROUP, "Familie");
    model.names.remember(TICKS_ANNA, "Anna");
    model.names.remember(TICKS_BEN, "Ben");
    let member = |address, name: &str| GroupMember {
        address,
        name: name.into(),
        admin: false,
        joined: true,
        left: false,
    };
    model.groups.insert(
        TICKS_GROUP,
        GroupInfo {
            name: "Familie".into(),
            ours: true,
            standing: Standing::Joined,
            epoch: 2,
            members: vec![member(TICKS_ANNA, "Anna"), member(TICKS_BEN, "Ben")],
            ..GroupInfo::new(TICKS_GROUP)
        },
    );
    for (minute, id, body) in [
        (-10, TICKS_TWO, "Brot ist da"),
        (-5, TICKS_ONE, "Wer kommt mit?"),
    ] {
        update(model, clock(minute));
        update(
            model,
            UiEvent::Write {
                peer: TICKS_GROUP,
                body: body.into(),
            },
        );
        update(
            model,
            UiEvent::Engine(OutboxEvent::Queued { message_id: id }),
        );
        update(model, UiEvent::SendDone(Ok(())));
        update(
            model,
            UiEvent::Engine(OutboxEvent::State {
                message_id: id,
                state: MessageState::Delivered,
            }),
        );
    }
    for (minute, id, original, on_device) in [
        (-4, [0xb1; 32], TICKS_TWO, 2),
        (-1, [0xb2; 32], TICKS_ONE, 1),
    ] {
        update(model, clock(minute));
        update(
            model,
            UiEvent::Engine(OutboxEvent::Received {
                sent_at: 0,
                message_id: id,
                source: TICKS_GROUP,
                title: Vec::new(),
                body: Vec::new(),
                fields: ticks_status(original, at - 600, on_device),
                verified: true,
            }),
        );
    }
    update(model, clock(0));
    let c = model
        .conversations
        .iter()
        .position(|c| c.peer == TICKS_GROUP)
        .expect("the group's chat");
    model.selected = c;
    c
}
