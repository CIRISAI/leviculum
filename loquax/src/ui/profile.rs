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
    /// What the mailbox said of itself (1642, `/about`).
    pub told: super::mailbox_page::Told,
    /// How this device reaches its mailbox now (1642).
    pub reach: super::mailbox_page::Reach,
    /// This device's RNS identity hash, which finds it in the mailbox's
    /// list (1642); never shown.
    pub device: Option<[u8; 16]>,
    /// This device's build (`loquax::VERSION`), which the mailbox's is
    /// held against (1642); empty where the shell did not say it.
    pub version: String,
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

/// The reader's own name, wherever this device has it (1452 rule 4): the
/// names' word for us, else the name the profile has, else the one the
/// mailbox says in `/about` (1647, walk 6, finding 31: the mailbox page
/// said "lew" from `/about` while the profile's avatar, which read the
/// names only, said "?" until a pull brought the archived name). One chain
/// for the list's bar, the profile and the phone's list.
pub fn own_name<B>(model: &UiModel<B>) -> Option<&str> {
    let own = model.address?;
    let said = |name: &str| !name.trim().is_empty();
    model
        .names
        .shown(&own)
        .filter(|n| said(n))
        .or(Some(model.profile.name.as_str()).filter(|n| said(n)))
        .or(match &model.profile.told {
            super::mailbox_page::Told::Told(about) => Some(about.name.as_str()).filter(|n| said(n)),
            _ => None,
        })
}

/// The profile of `model`, open or not.
pub fn profile<B>(model: &UiModel<B>) -> ProfileScreen {
    let own = model.address.unwrap_or([0; 16]);
    let avatar = Avatar::of(
        super::chat::initials(own_name(model), &own),
        &own,
        None,
        None,
    );
    // The name the profile keeps first, as before 1647; else the chain.
    let name = match model.profile.name.trim() {
        "" => match own_name(model) {
            Some(known) => Text::Raw(String::from(known.trim())),
            None => Text::key(StrId::ProfileNameElsewhere),
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

    /// 1647, walk 6, finding 31: the mailbox page said "lew" (its
    /// `/about`) while the avatar said "?" until a pull brought the
    /// archived name. The mailbox's own word names the avatar, the
    /// profile and the list's bar at once; a name already known wins.
    #[test]
    fn the_mailboxs_about_names_the_avatar_before_any_pull() {
        use crate::home::about::About;
        use crate::ui::mailbox_page::Told;
        use crate::ui::tests::Field;
        let own = [0x90; 16];
        let mut model = UiModel::<Field>::new(&[]);
        model.address = Some(own);
        model.profile.mailbox = Mailbox::Away {
            name: None,
            rung: None,
            word: None,
        };
        let before = profile(&model);
        assert_eq!(before.avatar.initials, "?", "the control: Lew's '?'");
        model.profile.told = Told::Told(About {
            name: String::from("lew"),
            ..About::default()
        });
        let after = profile(&model);
        assert_eq!(after.avatar.initials, "L");
        assert_eq!(after.name.resolve(Lang::De), "lew");
        // The list's bar and the phone's list read the same `own_name`.
        model.profile.name = String::from("Lew Palm");
        assert_eq!(
            own_name(&model),
            Some("Lew Palm"),
            "the profile's name first"
        );
    }

    /// 1449 item 2 (walk 1 finding 8): the mailbox's archive hands the
    /// reader's own name as an announce of the reader's own address; the
    /// avatar's letters and the profile's name are it, never the address's
    /// hex digits. An echo without a name keeps it.
    #[test]
    fn the_own_name_from_the_mailbox_names_the_avatar_and_the_profile() {
        use crate::outbox::OutboxEvent;
        use crate::ui::tests::Field;
        use crate::ui::{update, UiEvent};
        let own = [0x90; 16];
        let mut model = UiModel::<Field>::new(&[]);
        model.address = Some(own);
        model.profile.mailbox = Mailbox::Away {
            name: None,
            rung: None,
            word: None,
        };
        let before = profile(&model);
        assert_ne!(before.avatar.initials, "LP", "the control: no name before");
        assert_eq!(
            before.name.resolve(Lang::De),
            StrId::ProfileNameElsewhere.text(Lang::De)
        );
        let heard = |name: Option<&str>| {
            UiEvent::Engine(OutboxEvent::Announced {
                destination: own,
                name: name.map(String::from),
                path_response: false,
            })
        };
        update(&mut model, heard(Some("Lew Palm")));
        update(&mut model, heard(None));
        let after = profile(&model);
        assert_eq!(after.avatar.initials, "LP");
        assert_eq!(after.name.resolve(Lang::De), "Lew Palm");
        assert!(
            model.conversations.is_empty(),
            "the reader is no conversation"
        );
    }

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
