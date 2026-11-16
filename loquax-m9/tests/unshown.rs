//! The M9 never holds a screen it does not draw (1612).
//!
//! Window 23's hang walk lost one Back frame on the line (1613). The
//! walker's next Enter then came in the open chat, where the shared key
//! table's Enter opens the message under the cursor (1076): its detail
//! view, `UiModel::detail`. The M9 draws no detail view and its `[PAGE]`
//! line says `chat` for it, so the next Back only closed what nobody saw,
//! and every Enter and Back after it did the same: five minutes of keys
//! the board read, counted and handled, and a screen that never moved.
//! The keys were never deaf; the board was in a state it did not show.
//!
//! The rule: whatever a key does, the model is left in no state the M9
//! has no screen for (the message's detail, the profile, the settings,
//! the help, the palette, the copy mode); a key that would open one is
//! said as a notice and the state closed again.

use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
use loquax::ui::{UiEffect, UiModel};
use loquax_m9::field::Field;
use loquax_m9::group::{self, Place};
use loquax_m9::keyboard::Key;
use loquax_m9::keys::{event, M9Key};

/// What the model holds that the M9 does not draw, by name.
fn unshown(model: &UiModel<Field>) -> Vec<&'static str> {
    let mut held = Vec::new();
    if model.detail.is_some() {
        held.push("detail");
    }
    if model.profile_open {
        held.push("profile");
    }
    if model.settings.is_some() {
        held.push("settings");
    }
    if model.help {
        held.push("help");
    }
    if model.palette.is_some() {
        held.push("palette");
    }
    if model.copy_mode.is_some() {
        held.push("copy_mode");
    }
    held
}

/// The fixture with its selected chat open, as the ring's centre opens it.
fn open_chat() -> (UiModel<Field>, usize) {
    let mut model = loquax_m9::fixture::model();
    let mut row = 0;
    group::press(&mut model, &mut row, &event(M9Key::Enter));
    assert_eq!(Place::of(&model), Place::Chat, "the centre opens the chat");
    assert!(
        !model.messages_of_selected().is_empty(),
        "the fixture's chat holds messages, so Enter has one under the cursor"
    );
    (model, row)
}

/// Window 23's walk after its lost Back, on the host: Enter in the open
/// chat, then Back. Back leaves the chat for the list.
#[test]
fn back_leaves_the_chat_after_an_enter_inside_it() {
    let (mut model, mut row) = open_chat();
    let effects = group::press(&mut model, &mut row, &event(M9Key::Enter));
    assert_eq!(
        unshown(&model),
        Vec::<&str>::new(),
        "Enter left a state the M9 does not draw"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, UiEffect::Notice(line) if line.starts_with("unshown=detail"))),
        "the closed detail is said: {effects:?}"
    );
    group::press(&mut model, &mut row, &event(M9Key::Back));
    assert_eq!(Place::of(&model), Place::List, "one Back leaves the chat");
}

/// Every key the controller can send, pressed once in the open chat and
/// once on the list: none leaves a state the M9 has no screen for.
#[test]
fn no_key_leaves_a_screen_the_m9_does_not_draw() {
    let codes = (0x20u8..=0x7e).chain([0x08, 0x0d, 0x86, 0xb4, 0xb5, 0xb6, 0xb7]);
    for code in codes {
        let Some(key) = Key::of(code).and_then(Key::event) else {
            continue;
        };
        let (mut model, mut row) = open_chat();
        group::press(&mut model, &mut row, &key);
        assert_eq!(
            unshown(&model),
            Vec::<&str>::new(),
            "{code:#04x} in the open chat"
        );
        let mut model = loquax_m9::fixture::model();
        let mut row = 0;
        group::press(&mut model, &mut row, &key);
        assert_eq!(
            unshown(&model),
            Vec::<&str>::new(),
            "{code:#04x} on the list"
        );
    }
    // The terminal's own chords for them, which no M9 key sends, are the
    // positive control that the check sees the states at all.
    let (mut model, _) = open_chat();
    loquax::ui::update(
        &mut model,
        loquax::ui::UiEvent::Key(KeyEvent::new(KeyCode::Char('I'), KeyModifiers::NONE)),
    );
    assert_eq!(
        unshown(&model),
        ["detail"],
        "the control: the model's `I` opens the detail"
    );
}
