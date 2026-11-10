//! The compose bar's buttons (1270; Lew 2026-11-01, after Signal): one
//! rounded field with the emoji button at its left edge, the position
//! button, the camera and the microphone inside it at its right edge, and
//! a round button outside it that is a plus while there is nothing to
//! send and Send as soon as there is:
//!
//! ```text
//! ( ☺  Message            ⌖  📷  🎤 )  (+)
//! ( ☺  on my way          ⌖  📷     )  (➤)
//! ```
//!
//! The plus opens the attachment sheet (Gallery, File, Position); the
//! position button opens the position sheet straight away, so sharing a
//! position is one tap and not buried in the plus as Signal buries it
//! (Lew 2026-11-01 13:30); the camera takes a photo and stages it for
//! sending with an optional caption; the microphone records while held
//! (1222) and hides once the button is Send.
//!
//! What a shell can do decides which buttons it gets ([`Abilities`]): a
//! window without a camera has no camera button, one that cannot pick a
//! file has no Gallery and File rows, and with no row left in the plus the
//! round button is Send all the time.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::telemetry::Location;

use super::share::Span;
use super::{photos, position_fields, UiEffect, UiModel};

/// What the shell around the model can do: set once by the shell, never
/// by the user. All off by default, which is a terminal's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Abilities {
    /// It can take a photo: the phone's camera.
    pub camera: bool,
    /// It can record a voice message while the microphone is held.
    pub microphone: bool,
    /// It has the system's picker for a picture and a file.
    pub picker: bool,
    /// It can read its own position and ask for the permission.
    pub location: bool,
    /// It takes the own position by hand, typed or the map's centre
    /// (1371): a desktop, which has no receiver.
    pub manual: bool,
}

impl Abilities {
    /// A phone: the camera where it has one, everything else.
    pub fn phone(camera: bool) -> Self {
        Abilities {
            camera,
            microphone: true,
            picker: true,
            location: true,
            manual: false,
        }
    }

    /// A desktop (1371): the system's picker and the microphone, the
    /// position by hand; no camera and no receiver.
    pub fn desktop() -> Self {
        Abilities {
            camera: false,
            microphone: true,
            picker: true,
            location: false,
            manual: true,
        }
    }

    /// Whether the position sheet has something to offer: a fix read or
    /// one set by hand.
    pub fn locates(self) -> bool {
        self.location || self.manual
    }
}

/// The round button outside the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Round {
    /// Nothing to send: the attachment sheet.
    Plus,
    /// Text or a staged photo: it goes.
    Send,
}

impl Round {
    /// Signal's rule: text in the field (blanks are none) or a staged
    /// attachment make it Send; else it is the plus, where the plus has
    /// rows to offer (`plus`). Without one it stays Send.
    pub fn of(text: &str, staged: bool, plus: bool) -> Self {
        match staged || !text.trim().is_empty() || !plus {
            true => Round::Send,
            false => Round::Plus,
        }
    }

    /// The facade's word.
    pub fn word(self) -> &'static str {
        match self {
            Round::Plus => "plus",
            Round::Send => "send",
        }
    }
}

/// A button of the bar, in the order drawn: the emoji at the field's left
/// edge, position, camera and microphone at its right edge, the round
/// one outside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Button {
    Emoji,
    Locate,
    Camera,
    Microphone,
    Plus,
    Send,
}

impl Button {
    /// Inside the field, or the round one outside it.
    pub fn inside(self) -> bool {
        !matches!(self, Button::Plus | Button::Send)
    }

    /// The facade's word, and the device walk's resource id.
    pub fn word(self) -> &'static str {
        match self {
            Button::Emoji => "emoji",
            Button::Locate => "locate",
            Button::Camera => "camera",
            Button::Microphone => "record",
            Button::Plus => "plus",
            Button::Send => "send",
        }
    }
}

/// The bar's buttons for `abilities` and the round button's state, in the
/// order drawn.
pub fn buttons(abilities: Abilities, round: Round) -> Vec<Button> {
    let mut out = Vec::from([Button::Emoji]);
    if abilities.locates() {
        out.push(Button::Locate);
    }
    if abilities.camera {
        out.push(Button::Camera);
    }
    if abilities.microphone && round == Round::Plus {
        out.push(Button::Microphone);
    }
    out.push(match round {
        Round::Plus => Button::Plus,
        Round::Send => Button::Send,
    });
    out
}

/// A row of a sheet the bar opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SheetRow {
    /// A picture from the system's picker (1220).
    Gallery,
    /// Any file from it (1221).
    File,
    /// The position sheet, the same the position button opens.
    Position,
    /// "Now, once": the own fix goes to the chat as a report.
    ShareNow,
    /// "For 15 minutes", "For 1 hour", "For 8 hours", "Until I stop": a
    /// share starts for the chat (1273).
    ShareFor(Span),
    /// No fix known: turn location on (the 1201 permission flow).
    LocationOn,
    /// Type or paste the position (1371), on a shell without a receiver.
    EnterPosition,
    /// The map's centre is the position (1371), once a map was open.
    MapCentre,
}

impl SheetRow {
    /// The resource id's tail: `sheet_gallery`.
    pub fn word(self) -> &'static str {
        match self {
            SheetRow::Gallery => "gallery",
            SheetRow::File => "file",
            SheetRow::Position => "position",
            SheetRow::ShareNow => "share_now",
            SheetRow::ShareFor(Span::Minutes15) => "share_15_min",
            SheetRow::ShareFor(Span::Hour1) => "share_1_h",
            SheetRow::ShareFor(Span::Hours8) => "share_8_h",
            SheetRow::ShareFor(Span::UntilStopped) => "share_until_stopped",
            SheetRow::LocationOn => "location_on",
            SheetRow::EnterPosition => "enter_position",
            SheetRow::MapCentre => "map_centre",
        }
    }
}

/// The plus sheet's rows for `abilities`: Gallery and File with a picker,
/// Position with location (later Contact).
pub fn attach_rows(abilities: Abilities) -> Vec<SheetRow> {
    let mut rows = Vec::new();
    if abilities.picker {
        rows.push(SheetRow::Gallery);
        rows.push(SheetRow::File);
    }
    if abilities.locates() {
        rows.push(SheetRow::Position);
    }
    rows
}

/// The position sheet's rows: with a fix, once and the four shares
/// (1273); without one, location on where the shell reads its own. A
/// shell that takes it by hand (1371) offers the entry after those, and
/// the map's centre when `centre`, a map having been open.
pub fn position_rows(fix: Option<&Location>, abilities: Abilities, centre: bool) -> Vec<SheetRow> {
    let mut rows = match fix {
        Some(_) => core::iter::once(SheetRow::ShareNow)
            .chain(Span::ALL.into_iter().map(SheetRow::ShareFor))
            .collect(),
        None if abilities.location || !abilities.manual => Vec::from([SheetRow::LocationOn]),
        None => Vec::new(),
    };
    if abilities.manual {
        rows.push(SheetRow::EnterPosition);
        if centre {
            rows.push(SheetRow::MapCentre);
        }
    }
    rows
}

/// Which sheet the bar opened, over whose chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    /// The plus's.
    Attach { peer: [u8; 16] },
    /// The position button's, and the plus's Position row.
    Position { peer: [u8; 16] },
}

impl Sheet {
    pub fn peer(self) -> [u8; 16] {
        match self {
            Sheet::Attach { peer } | Sheet::Position { peer } => peer,
        }
    }

    /// The facade's word.
    pub fn word(self) -> &'static str {
        match self {
            Sheet::Attach { .. } => "attach",
            Sheet::Position { .. } => "position",
        }
    }
}

/// A photo the camera took, scaled and stripped by the shell already
/// (`crate::photo::prepare`), waiting in the compose bar for Send and
/// its optional caption. Kept in memory only: a photo is not a draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    /// `jpg`, `png`, `gif`.
    pub format: String,
    pub data: Vec<u8>,
}

/// The bar's state beyond the field's text, which is the draft (1219).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bar {
    pub abilities: Abilities,
    /// The open sheet, if any.
    pub sheet: Option<Sheet>,
    /// The staged photo, by conversation.
    pub staged: BTreeMap<[u8; 16], Staged>,
    /// The shell's newest own fix, for the position sheet.
    pub own_fix: Option<Location>,
    /// The own fix was set by hand (1371), not read.
    pub by_hand: bool,
    /// The shell's map's centre, once it showed a map (1371).
    pub map_centre: Option<Location>,
}

impl<B> UiModel<B> {
    /// The round button of `peer`'s bar: [`Round::of`] over its draft and
    /// staged photo.
    pub fn round(&self, peer: [u8; 16]) -> Round {
        let text = self
            .conversations
            .iter()
            .find(|c| c.peer == peer)
            .map_or("", |c| c.draft.text.as_str());
        Round::of(
            text,
            self.bar.staged.contains_key(&peer),
            !attach_rows(self.bar.abilities).is_empty(),
        )
    }

    /// [`super::UiEvent::StagePhoto`]: the photo waits in `peer`'s bar,
    /// in place of one staged before.
    pub(super) fn stage_photo(&mut self, peer: [u8; 16], format: String, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        self.conversation(peer);
        self.bar.sheet = None;
        self.bar.staged.insert(peer, Staged { format, data });
    }

    /// [`super::UiEvent::Unstage`]: the staged photo is dropped.
    pub(super) fn unstage(&mut self, peer: [u8; 16]) {
        self.bar.staged.remove(&peer);
    }

    /// [`super::UiEvent::OpenSheet`]: one sheet at a time.
    pub(super) fn open_sheet(&mut self, sheet: Sheet) {
        self.bar.sheet = Some(sheet);
    }

    /// The staged photo of the selected conversation goes with `body` as
    /// its caption, one message (1268's one bubble): the effects, or none
    /// when nothing is staged there.
    pub(super) fn send_staged(&mut self, body: String) -> Option<Vec<UiEffect>> {
        let peer = self.conversations.get(self.selected)?.peer;
        let staged = self.bar.staged.remove(&peer)?;
        let body = super::outgoing(body);
        self.clear_draft(peer);
        let index = self.selected;
        let effects = self.send_fields(
            index,
            body,
            photos::photo_fields(&staged.format, staged.data),
        );
        self.message_cursor = self.conversations[index].messages.len().saturating_sub(1);
        self.scroll_top = None;
        Some(effects)
    }

    /// [`super::UiEvent::ShareFix`]: the own fix goes to `peer` as a
    /// position report (1209, a state and not a message), and the sheet
    /// closes; without a fix nothing goes and the sheet stays, saying so.
    pub(super) fn share_fix(&mut self, peer: [u8; 16]) -> Vec<UiEffect> {
        let Some(fix) = self.bar.own_fix else {
            return Vec::new();
        };
        self.bar.sheet = None;
        let index = self.conversation(peer);
        self.send_fields(index, String::new(), position_fields(fix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::Field;
    use crate::ui::{update, UiEvent, UiModel};
    use alloc::string::ToString;
    use alloc::vec;
    use leviculum_lxmf::constants::{FIELD_IMAGE, FIELD_TELEMETRY};

    const PEER: [u8; 16] = [0xab; 16];

    fn phone() -> UiModel<Field> {
        let mut model = UiModel::new(&[PEER]);
        model.bar.abilities = Abilities::phone(true);
        update(&mut model, UiEvent::Open { peer: PEER });
        model
    }

    fn draft(model: &mut UiModel<Field>, text: &str) {
        update(
            model,
            UiEvent::Draft {
                peer: PEER,
                text: text.to_string(),
                reply: None,
            },
        );
    }

    /// The plus/send rule: empty or blank is the plus, text or a staged
    /// photo is Send, and a plus with no row to offer is Send all along.
    #[test]
    fn the_round_button_is_plus_until_there_is_something_to_send() {
        assert_eq!(Round::of("", false, true), Round::Plus);
        assert_eq!(Round::of(" \n\t", false, true), Round::Plus);
        assert_eq!(Round::of("hi", false, true), Round::Send);
        assert_eq!(Round::of("", true, true), Round::Send);
        assert_eq!(Round::of(" ", true, true), Round::Send);
        assert_eq!(Round::of("", false, false), Round::Send, "no row: no plus");
    }

    /// The model applies it to the open chat's draft and staged photo,
    /// and the microphone hides with Send.
    #[test]
    fn typing_and_staging_turn_the_plus_into_send_and_hide_the_microphone() {
        let mut model = phone();
        assert_eq!(model.round(PEER), Round::Plus);
        assert_eq!(
            buttons(model.bar.abilities, model.round(PEER)),
            [
                Button::Emoji,
                Button::Locate,
                Button::Camera,
                Button::Microphone,
                Button::Plus
            ]
        );
        draft(&mut model, "on my way");
        assert_eq!(model.round(PEER), Round::Send);
        assert_eq!(
            buttons(model.bar.abilities, model.round(PEER)),
            [Button::Emoji, Button::Locate, Button::Camera, Button::Send]
        );
        draft(&mut model, "  ");
        assert_eq!(model.round(PEER), Round::Plus, "blanks are nothing");
        update(
            &mut model,
            UiEvent::StagePhoto {
                peer: PEER,
                format: "jpg".into(),
                data: vec![0xff, 0xd8],
            },
        );
        assert_eq!(model.round(PEER), Round::Send);
        update(&mut model, UiEvent::Unstage { peer: PEER });
        assert_eq!(model.round(PEER), Round::Plus);
    }

    /// A shell without camera, microphone, picker and location (the
    /// window today) has the emoji and Send alone.
    #[test]
    fn a_shell_that_can_do_little_gets_few_buttons() {
        let none = Abilities::default();
        assert!(attach_rows(none).is_empty());
        assert_eq!(
            buttons(none, Round::of("", false, false)),
            [Button::Emoji, Button::Send]
        );
        let no_camera = Abilities::phone(false);
        assert_eq!(
            buttons(no_camera, Round::Plus),
            [
                Button::Emoji,
                Button::Locate,
                Button::Microphone,
                Button::Plus
            ]
        );
        assert_eq!(
            attach_rows(no_camera),
            [SheetRow::Gallery, SheetRow::File, SheetRow::Position]
        );
        assert!(Button::Locate.inside() && Button::Microphone.inside());
        assert!(!Button::Plus.inside() && !Button::Send.inside());
    }

    /// Send with a staged photo sends one message: the image field and
    /// the caption as its text; the bar is empty after.
    #[test]
    fn send_takes_the_staged_photo_and_the_caption_as_one_message() {
        let mut model = phone();
        update(
            &mut model,
            UiEvent::StagePhoto {
                peer: PEER,
                format: "jpg".into(),
                data: vec![0xff, 0xd8],
            },
        );
        draft(&mut model, "the view");
        let effects = update(
            &mut model,
            UiEvent::Write {
                peer: PEER,
                body: "the view".into(),
            },
        );
        let sends: Vec<_> = effects
            .iter()
            .filter_map(|e| match e {
                UiEffect::Send { to, body, fields } => Some((to, body, fields)),
                _ => None,
            })
            .collect();
        assert_eq!(sends.len(), 1, "one message: {effects:?}");
        assert_eq!(sends[0].1, "the view");
        assert!(sends[0].2.iter().any(|(k, _)| *k == FIELD_IMAGE));
        assert!(model.bar.staged.is_empty());
        assert_eq!(model.round(PEER), Round::Plus);
        let c = &model.conversations[model.selected];
        assert_eq!(c.messages.len(), 1);
        assert!(c.draft.text.is_empty());
    }

    /// A staged photo without words goes with an empty text, as a photo
    /// from the gallery does.
    #[test]
    fn a_staged_photo_without_a_caption_goes_alone() {
        let mut model = phone();
        update(
            &mut model,
            UiEvent::StagePhoto {
                peer: PEER,
                format: "jpg".into(),
                data: vec![0xff, 0xd8],
            },
        );
        let effects = update(
            &mut model,
            UiEvent::Write {
                peer: PEER,
                body: "  ".into(),
            },
        );
        assert!(matches!(
            effects.as_slice(),
            [UiEffect::Send { body, .. }] if body.is_empty()
        ));
    }

    /// The position sheet: the share needs a fix; with one it goes as a
    /// report (the telemetry field alone) and the sheet closes.
    #[test]
    fn share_now_sends_the_own_fix_as_a_report_and_closes_the_sheet() {
        let mut model = phone();
        update(
            &mut model,
            UiEvent::OpenSheet(Sheet::Position { peer: PEER }),
        );
        assert_eq!(
            position_rows(model.bar.own_fix.as_ref(), model.bar.abilities, false),
            [SheetRow::LocationOn]
        );
        assert!(update(&mut model, UiEvent::ShareFix { peer: PEER }).is_empty());
        assert!(model.bar.sheet.is_some(), "no fix: the sheet stays");
        let fix = Location {
            latitude_e6: 43_773_100,
            longitude_e6: 11_256_000,
            altitude_e2: 0,
            speed_e2: 0,
            bearing_e2: 0,
            accuracy_e2: 500,
            last_update: 1_000,
        };
        update(&mut model, UiEvent::OwnFix(Some(fix)));
        assert_eq!(
            position_rows(model.bar.own_fix.as_ref(), model.bar.abilities, false),
            [
                SheetRow::ShareNow,
                SheetRow::ShareFor(Span::Minutes15),
                SheetRow::ShareFor(Span::Hour1),
                SheetRow::ShareFor(Span::Hours8),
                SheetRow::ShareFor(Span::UntilStopped),
            ]
        );
        let effects = update(&mut model, UiEvent::ShareFix { peer: PEER });
        assert!(matches!(
            effects.as_slice(),
            [UiEffect::Send { body, fields, .. }]
                if body.is_empty() && fields.iter().any(|(k, _)| *k == FIELD_TELEMETRY)
        ));
        assert_eq!(model.bar.sheet, None);
    }
}
