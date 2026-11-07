//! The profile behind the reader's avatar (1443): one scene on every
//! target. Lew, 2026-11-06: the phone opened "Your profile" on a tap on
//! the avatar and the window did nothing, though the programs share their
//! code and the UI is a thin layer over it. So the screen is built here,
//! from the model and the facts the shell knows, and the phone's profile
//! activity, the window's layer, the terminal's view and the M9's lines
//! draw this node and nothing of their own words.
//!
//! What it shows, top to bottom: the avatar, the name the announce
//! carries, the address with its groups of four, the contact code's QR
//! (the core's bitmap, [`QrNode`], which the shell encodes with
//! `loquax::home::qr` as it encodes every other code), where the mailbox
//! is and how it is reached ([`Rung`], or how far a pull came), the
//! devices of this identity, and the one action "Share my address".
//!
//! A tap on the avatar raises [`Intent::Profile`]: the bar's
//! [`super::scene::Lead::Me`] carries it, so every renderer that draws the
//! avatar knows what a tap on it does without a word of its own.

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_ui::scene::ToJson;
use serde_json::{json, Value};

use super::intent::Intent;
use super::scene::{A11y, Action, Avatar, NodeId, Role};
use super::strings::{Arg, StrId, Text};
use super::UiModel;
use crate::address::to_hex;
use crate::home::board::Word;

/// How a device reaches its mailbox: the "header word" column of the
/// ladder in `docs/src/concepts/loquax-reachability.md`. The shell says
/// the one its link runs over when it knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rung {
    /// Rung 0: the same machine, its shared instance or in-process.
    Local,
    /// Rung 1: Bluetooth.
    Bluetooth,
    /// Rungs 2a to 2c: the same WiFi.
    HomeWifi,
    /// Rungs 3, 4, 5 and D: an address of the mailbox's own.
    Direct,
    /// Rungs B and 6: the rendezvous, a public transport node.
    Relayed,
    /// Rung A: the general mesh, any path over any media.
    Mesh,
}

impl Rung {
    /// Its words, what "Connected over …" names.
    pub fn word(self) -> StrId {
        match self {
            Rung::Local => StrId::RungLocal,
            Rung::Bluetooth => StrId::RungBluetooth,
            Rung::HomeWifi => StrId::RungHomeWifi,
            Rung::Direct => StrId::RungDirect,
            Rung::Relayed => StrId::RungRelayed,
            Rung::Mesh => StrId::RungMesh,
        }
    }
}

/// Where the mailbox is, as the shell knows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Mailbox {
    /// No mailbox is set up, or the shell has not said yet.
    #[default]
    Unknown,
    /// On this device: it is the home.
    Here,
    /// On another device, `name` the name the mailbox announces where the
    /// shell has heard it (never a hash, `feedback_no_hashes_on_screen`,
    /// and never the server it is reached through: that is the
    /// rendezvous, not where the mailbox is, 1447); `rung` how the link
    /// runs, `word` how the last ask went (the board's word,
    /// `loquax::home::board::Word`: connected, fetched N of M,
    /// unreachable).
    Away {
        name: Option<String>,
        rung: Option<Rung>,
        word: Option<Word>,
    },
}

/// A device of this identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    /// The name it joined under; empty where the shell knows none.
    pub name: String,
    /// Whether it is the one the screen is on.
    pub this: bool,
}

/// What the shell knows of the reader that the model does not: the name
/// the announce carries, the mailbox, the devices. The shell sets it
/// ([`super::UiEvent::ProfileFacts`]) whenever one of them moves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileFacts {
    /// The name recipients see; empty when none was given here.
    pub name: String,
    pub mailbox: Mailbox,
    /// This identity's devices, this one among them; empty where the
    /// shell does not know them.
    pub devices: Vec<Device>,
}

/// A QR code as the core's bitmap: `side` modules a side, row by row,
/// true for dark, without the quiet zone. A pixel screen draws it as the
/// map is drawn, a cell screen in half blocks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrNode {
    pub side: u32,
    pub modules: Vec<bool>,
}

impl QrNode {
    /// Whether the module at `x`, `y` is dark; outside is light.
    pub fn dark(&self, x: u32, y: u32) -> bool {
        if x >= self.side || y >= self.side {
            return false;
        }
        let at = (y as usize) * (self.side as usize) + x as usize;
        self.modules.get(at).copied().unwrap_or(false)
    }

    /// The code in half blocks, two module rows a text row, with a quiet
    /// zone of `quiet` modules: light modules drawn, dark ones blank, as
    /// `loquax::home::qr::draw` does for a terminal's light text on dark.
    pub fn half_blocks(&self, quiet: u32) -> Vec<String> {
        let side = self.side + 2 * quiet;
        let light = |x: u32, y: u32| {
            let (x, y) = (x.wrapping_sub(quiet), y.wrapping_sub(quiet));
            !self.dark(x, y)
        };
        (0..side.div_ceil(2))
            .map(|row| {
                (0..side)
                    .map(|x| {
                        let top = light(x, 2 * row);
                        let bottom = 2 * row + 1 < side && light(x, 2 * row + 1);
                        match (top, bottom) {
                            (true, true) => '█',
                            (true, false) => '▀',
                            (false, true) => '▄',
                            (false, false) => ' ',
                        }
                    })
                    .collect()
            })
            .collect()
    }
}

/// The profile as every target draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileScreen {
    pub id: NodeId,
    /// Back to where the avatar was tapped.
    pub back: Action,
    pub title: Text,
    pub avatar: Avatar,
    /// The name, or where it is set when it is not set here.
    pub name: Text,
    pub address_label: Text,
    /// The address as lowercase hex, and in groups of four to read out.
    pub address: String,
    pub fours: String,
    /// The contact code a friend pastes, `lxma://…`, and its QR.
    pub code: Option<String>,
    pub qr: Option<QrNode>,
    pub caption: Text,
    /// Where the mailbox is.
    pub mailbox: Text,
    /// How it is reached, or how the last ask went.
    pub state: Option<Text>,
    pub devices_label: Text,
    pub devices: Vec<Text>,
    /// "Share my address": the phone's share sheet, the window's and the
    /// terminal's clipboard. Disabled without a code.
    pub share: Action,
    pub a11y: A11y,
}

/// An address in groups of four hex digits.
pub fn fours(address: &[u8; 16]) -> String {
    let hex = to_hex(address);
    let mut out = String::with_capacity(hex.len() + hex.len() / 4);
    for (i, c) in hex.chars().enumerate() {
        if i > 0 && i % 4 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// The mailbox's line and its state line: "Postfach: Lews Rechner
/// (verbunden)" by the name it announces, the rung it is reached over only
/// in the state line under it (1447, walk 1 finding 13).
pub fn mailbox_words(mailbox: &Mailbox) -> (Text, Option<Text>) {
    match mailbox {
        Mailbox::Unknown => (Text::key(StrId::ProfileMailboxUnknown), None),
        Mailbox::Here => (Text::key(StrId::ProfileMailboxHere), None),
        Mailbox::Away { name, rung, word } => {
            let line = match name.as_deref().map(str::trim) {
                Some(name) if !name.is_empty() => Text::Key(
                    StrId::ProfileMailboxAway,
                    Vec::from([Arg::Str(String::from(name))]),
                ),
                _ => Text::key(StrId::ProfileMailboxElsewhere),
            };
            let line = match word {
                Some(Word::Connected) => Text::Key(
                    StrId::ProfileMailboxConnected,
                    Vec::from([Arg::Text(alloc::boxed::Box::new(line))]),
                ),
                _ => line,
            };
            let state = match (word, rung) {
                // A pull under way says how far it came, whatever the rung.
                (Some(word @ Word::Fetched { .. }), _) => Some(word.text()),
                (Some(Word::Connected) | None, Some(rung)) => Some(Text::Key(
                    StrId::ProfileVia,
                    Vec::from([Arg::Text(alloc::boxed::Box::new(Text::key(rung.word())))]),
                )),
                (Some(word), _) => Some(word.text()),
                (None, None) => None,
            };
            (line, state)
        }
    }
}

impl<B> UiModel<B> {
    /// The profile, while it is open: what [`Intent::Profile`] opened.
    pub fn profile_screen(&self) -> Option<ProfileScreen> {
        self.profile_open.then(|| profile(self))
    }

    /// Open the profile, or close it: from its first row, over whatever
    /// detail view was up.
    pub fn show_profile(&mut self, open: bool) -> Vec<super::UiEffect> {
        if open {
            self.detail = None;
        }
        self.profile_open = open;
        self.detail_scroll = 0;
        Vec::new()
    }

    /// "Share my address" where the share is the clipboard: the contact
    /// code, and a toast that says it is there. Nothing without a code.
    pub fn share_code(&mut self) -> Vec<super::UiEffect> {
        let Some(code) = self.own_code.as_ref().map(|c| c.code.clone()) else {
            return Vec::new();
        };
        let said = StrId::ProfileCopied.text(self.lang);
        self.toast(leviculum_ui::toast::ToastKind::Info, String::from(said));
        Vec::from([super::UiEffect::Copy(code)])
    }
}

/// The profile of `model`, open or not.
pub fn profile<B>(model: &UiModel<B>) -> ProfileScreen {
    let own = model.address.unwrap_or([0; 16]);
    let avatar = Avatar::of(
        super::chat::initials(model.names.shown(&own), &own),
        &own,
        None,
        None,
    );
    let name = match model.profile.name.trim() {
        "" => match model.names.shown(&own) {
            Some(known) if !known.is_empty() => Text::Raw(String::from(known)),
            _ => Text::key(StrId::ProfileNameElsewhere),
        },
        name => Text::Raw(String::from(name)),
    };
    // A shell that has not heard the mailbox's name takes the one this
    // identity's announce carries: the mailbox announces it (1447).
    let mut facts = model.profile.mailbox.clone();
    if let Mailbox::Away {
        name: name @ None, ..
    } = &mut facts
    {
        *name = model.names.announced(&own).map(String::from);
    }
    let (mailbox, state) = mailbox_words(&facts);
    let devices = model
        .profile
        .devices
        .iter()
        .map(|device| match (device.this, device.name.is_empty()) {
            (true, true) => Text::key(StrId::ProfileThisDeviceUnnamed),
            (true, false) => Text::Key(
                StrId::ProfileThisDevice,
                Vec::from([Arg::Str(device.name.clone())]),
            ),
            (false, true) => Text::key(StrId::ProfileDeviceUnnamed),
            (false, false) => Text::Raw(device.name.clone()),
        })
        .collect();
    let code = model.own_code.as_ref().map(|c| c.code.clone());
    let qr = model
        .own_code
        .as_ref()
        .filter(|c| c.side > 0)
        .map(|c| QrNode {
            side: c.side,
            modules: c.modules.clone(),
        });
    let share = Action {
        enabled: code.is_some(),
        ..Action::of(Intent::ShareCode)
    };
    ProfileScreen {
        id: NodeId::Profile,
        back: Action::of(Intent::Back),
        title: Text::key(StrId::ProfileTitle),
        avatar,
        name,
        address_label: Text::key(StrId::ProfileAddress),
        address: to_hex(&own),
        fours: fours(&own),
        code,
        qr,
        caption: Text::key(StrId::ProfileQrCaption),
        mailbox,
        state,
        devices_label: Text::key(StrId::ProfileDevices),
        devices,
        share,
        a11y: A11y::of(NodeId::Profile, Role::Sheet, Text::key(StrId::ProfileTitle)),
    }
}

impl ProfileScreen {
    /// The node for a golden file: the QR as its side and its rows of
    /// `#` and `.`, so a moved module shows as a moved character.
    pub fn to_json(&self) -> Value {
        let qr = self.qr.as_ref().map(|qr| {
            let rows: Vec<String> = (0..qr.side)
                .map(|y| {
                    (0..qr.side)
                        .map(|x| if qr.dark(x, y) { '#' } else { '.' })
                        .collect()
                })
                .collect();
            json!({ "side": qr.side, "rows": rows })
        });
        json!({
            "id": self.id.resource(),
            "back": self.back.to_json(),
            "title": self.title.to_json(),
            "avatar": self.avatar.to_json(),
            "name": self.name.to_json(),
            "address_label": self.address_label.to_json(),
            "address": self.address,
            "fours": self.fours,
            "code": self.code,
            "qr": qr,
            "caption": self.caption.to_json(),
            "mailbox": self.mailbox.to_json(),
            "state": self.state.as_ref().map(Text::to_json),
            "devices_label": self.devices_label.to_json(),
            "devices": self.devices.iter().map(Text::to_json).collect::<Vec<_>>(),
            "share": self.share.to_json(),
            "a11y": self.a11y.to_json(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::strings::Lang;

    #[test]
    fn an_address_reads_in_groups_of_four() {
        let address: [u8; 16] = core::array::from_fn(|i| i as u8 * 17);
        assert_eq!(fours(&address), "0011 2233 4455 6677 8899 aabb ccdd eeff");
    }

    /// The mailbox's state: the rung while connected, how far a pull came
    /// while one runs, unreachable when the last ask failed; nothing when
    /// the shell knows neither.
    #[test]
    fn the_mailbox_says_its_rung_its_pull_or_that_it_is_unreachable() {
        let away = |rung, word| Mailbox::Away {
            name: Some(String::from("Lews Rechner")),
            rung,
            word,
        };
        let said = |m: &Mailbox, lang| {
            let (line, state) = mailbox_words(m);
            (line.resolve(lang), state.map(|s| s.resolve(lang)))
        };
        assert_eq!(
            said(&away(Some(Rung::Relayed), Some(Word::Connected)), Lang::De),
            (
                String::from("Postfach: Lews Rechner (verbunden)"),
                Some(String::from("Verbunden über einen Vermittlungsserver"))
            )
        );
        let fetching = away(
            Some(Rung::HomeWifi),
            Some(Word::Fetched {
                taken: 3,
                of: Some(10),
            }),
        );
        assert_eq!(
            said(&fetching, Lang::De).1.as_deref(),
            Some("3 von 10 abgeholt")
        );
        assert_eq!(
            said(&away(Some(Rung::Mesh), Some(Word::Unreachable)), Lang::En)
                .1
                .as_deref(),
            Some("Mailbox unreachable")
        );
        assert_eq!(said(&away(None, None), Lang::En).1, None);
        assert_eq!(said(&Mailbox::Here, Lang::En).0, "Mailbox: on this device");
        // Without a name heard, another device; never the rendezvous.
        let nameless = Mailbox::Away {
            name: None,
            rung: Some(Rung::Relayed),
            word: Some(Word::Connected),
        };
        assert_eq!(
            said(&nameless, Lang::De).0,
            "Postfach: auf einem anderen Gerät (verbunden)"
        );
        assert_eq!(
            said(&away(Some(Rung::Mesh), Some(Word::Unreachable)), Lang::En).0,
            "Mailbox: Lews Rechner"
        );
    }

    /// The half blocks of a 1x1 code with a quiet zone of one: light
    /// around, the dark module the blank lower half of the middle cell.
    #[test]
    fn a_qr_draws_light_modules_and_leaves_dark_ones_blank() {
        let qr = QrNode {
            side: 1,
            modules: Vec::from([true]),
        };
        assert_eq!(
            qr.half_blocks(1),
            Vec::from([String::from("█▀█"), String::from("▀▀▀")])
        );
    }
}
