//! One settings structure (1450, walk 1 finding 21): the same nine
//! sections in the same order on every target, from this one list.
//!
//! Before this the phone had its settings in the ⋮ menu with nine
//! sections and the window a gear with a flat page (read receipts, photo
//! size, maps, the profile, help) and a light/dark button in its bar; the
//! terminal and the M9 had none. Now:
//!
//! - [`SECTIONS`] is the list, in the phone's order: Profile, Mailbox,
//!   Appearance, Chats, Notifications, Privacy, Maps, Backup, Advanced.
//!   Its words are the catalogue's ([`Section::title`],
//!   [`Section::summary`]), which the phone reads as `R.string` too.
//! - The entry point is the target's own: the ⋮ menu on the phone, the
//!   gear on the window, `S` and the palette's `settings` on the terminal
//!   ([`super::intent::Intent::Settings`]), and on the M9 the house key
//!   (1561, `loquax-m9/src/house.rs`), whose page lists the sections the
//!   M9 draws ([`PLACES`] says why the others are not).
//! - The theme switch lives in Appearance: the window's bar has no
//!   light/dark button any more.
//! - [`PLACES`] says per section and target whether it is drawn, with the
//!   file and token that draw it, or not applicable with the reason, as
//!   the intent inventory ([`super::inventory`]) does per intent; its
//!   tests are the guard that every target draws every section it does
//!   not mark n/a. The inventory's `Settings` row points here.
//!
//! What a section holds is the target's: the phone's pages are Android's
//! (the system's notification channels, the share sheet, the backup
//! paper), the window draws its own switches for Appearance, Chats,
//! Privacy and Maps; [`SettingsScreen::rows`] are the words and verbs the
//! core knows, which the terminal draws whole and the window for the rest.

use alloc::vec::Vec;

use super::intent::{Intent, Switch};
use super::inventory::{Cell, Via};
use super::photos::PhotoSize;
use super::scene::Action;
use super::strings::{StrId, Text};
use super::UiModel;
use leviculum_ui::theme::Theme;

/// A section of the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    Profile,
    Mailbox,
    Appearance,
    Chats,
    Notifications,
    Privacy,
    Maps,
    Backup,
    Advanced,
}

/// The sections in the order every target lists them: the phone's.
pub const SECTIONS: [Section; 9] = [
    Section::Profile,
    Section::Mailbox,
    Section::Appearance,
    Section::Chats,
    Section::Notifications,
    Section::Privacy,
    Section::Maps,
    Section::Backup,
    Section::Advanced,
];

impl Section {
    /// Its stable word: the phone's page extra, the facade's key.
    pub const fn key(self) -> &'static str {
        match self {
            Section::Profile => "profile",
            Section::Mailbox => "mailbox",
            Section::Appearance => "appearance",
            Section::Chats => "chats",
            Section::Notifications => "notifications",
            Section::Privacy => "privacy",
            Section::Maps => "maps",
            Section::Backup => "backup",
            Section::Advanced => "advanced",
        }
    }

    /// The section a key names.
    pub fn of_key(key: &str) -> Option<Section> {
        SECTIONS.into_iter().find(|s| s.key() == key)
    }

    /// Its title in the list and over its page.
    pub const fn title(self) -> StrId {
        match self {
            Section::Profile => StrId::SettingsProfile,
            Section::Mailbox => StrId::Mailbox,
            Section::Appearance => StrId::SettingsAppearance,
            Section::Chats => StrId::SettingsChats,
            Section::Notifications => StrId::SettingsNotifications,
            Section::Privacy => StrId::SettingsPrivacy,
            Section::Maps => StrId::SettingsMaps,
            Section::Backup => StrId::SettingsBackup,
            Section::Advanced => StrId::Advanced,
        }
    }

    /// The line under its title in the list: what it holds.
    pub const fn summary(self) -> StrId {
        match self {
            Section::Profile => StrId::SettingsProfileSummary,
            Section::Mailbox => StrId::SettingsMailboxSummary,
            Section::Appearance => StrId::SettingsAppearanceSummary,
            Section::Chats => StrId::SettingsChatsSummary,
            Section::Notifications => StrId::SettingsNotificationsSummary,
            Section::Privacy => StrId::SettingsPrivacySummary,
            Section::Maps => StrId::SettingsMapsSummary,
            Section::Backup => StrId::SettingsBackupSummary,
            Section::Advanced => StrId::SettingsAdvancedSummary,
        }
    }
}

/// The four targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Phone,
    Window,
    Terminal,
    M9,
}

/// One section on each target: drawn (with the file and the token that
/// draw it), missing, or not applicable, as [`super::inventory::Cell`].
#[derive(Clone, Copy, Debug)]
pub struct Place {
    pub section: Section,
    pub phone: Cell,
    pub window: Cell,
    pub terminal: Cell,
    pub m9: Cell,
}

impl Place {
    /// The cell of `target`.
    pub const fn on(&self, target: Target) -> Cell {
        match target {
            Target::Phone => self.phone,
            Target::Window => self.window,
            Target::Terminal => self.terminal,
            Target::M9 => self.m9,
        }
    }
}

const JAVA: &str = "loquax-android/android/src/network/leviculum/loquax/SettingsActivity.java";
const GUI_CHAT: &str = "loquax/src/gui/chat.rs";
const UI: &str = "loquax/src/ui.rs";

const fn at(file: &'static str, token: &'static str) -> Cell {
    Cell::Built(Via::At(file, token))
}

/// The M9's settings page (1561): the house key's, which draws
/// [`screen`] for [`Target::M9`].
const M9_HOUSE: &str = "loquax-m9/src/house.rs";

/// Where each section is drawn. The window's and the terminal's cells are
/// the page's handler for the section; the phone's the arm of
/// `SettingsActivity.refresh` (or the entry's, for the profile).
pub const PLACES: [Place; 9] = [
    Place {
        section: Section::Profile,
        phone: at(JAVA, "\"profile\".equals(target)"),
        window: at(GUI_CHAT, "Section::Profile =>"),
        terminal: at(UI, "Section::Profile =>"),
        m9: Cell::Na("the M9 is a device of its home (#232): its name and address are the home's, shown where the home runs"),
    },
    Place {
        section: Section::Mailbox,
        phone: at(JAVA, "\"mailbox\".equals(page)"),
        window: at(GUI_CHAT, "Section::Mailbox"),
        terminal: at(UI, "Section::Mailbox"),
        m9: Cell::Missing("the M9 joins its home on the pairing screen; a page that names the home and its devices is owed (1642's mailbox page drawn on the board)"),
    },
    Place {
        section: Section::Appearance,
        phone: at(JAVA, "\"appearance\".equals(page)"),
        window: at(GUI_CHAT, "Section::Appearance =>"),
        terminal: at(UI, "Section::Appearance"),
        m9: at(M9_HOUSE, "Section::Appearance =>"),
    },
    Place {
        section: Section::Chats,
        phone: at(JAVA, "\"chats\".equals(page)"),
        window: at(GUI_CHAT, "Section::Chats =>"),
        terminal: at(UI, "Section::Chats"),
        m9: Cell::Na("photo size: the M9 has no camera and sends no photos, and its Enter always sends"),
    },
    Place {
        section: Section::Notifications,
        phone: at(JAVA, "\"notifications\".equals(page)"),
        window: at(GUI_CHAT, "Section::Notifications"),
        terminal: at(UI, "Section::Notifications"),
        m9: Cell::Na("the M9 has no notification service: an arrival shows on its list and in its top bar"),
    },
    Place {
        section: Section::Privacy,
        phone: at(JAVA, "\"privacy\".equals(page)"),
        window: at(GUI_CHAT, "Section::Privacy =>"),
        terminal: at(UI, "Section::Privacy"),
        m9: Cell::Missing("read receipts: the home signs them, so a switch on the board must reach the home and the client record must keep it; neither path exists yet"),
    },
    Place {
        section: Section::Maps,
        phone: at(JAVA, "\"maps\".equals(page)"),
        window: at(GUI_CHAT, "Section::Maps =>"),
        terminal: at(UI, "Section::Maps"),
        m9: Cell::Na("the M9 draws the regions lnflash wrote and the world overview; it downloads none"),
    },
    Place {
        section: Section::Backup,
        phone: at(JAVA, "\"backup\".equals(page)"),
        window: at(GUI_CHAT, "Section::Backup"),
        terminal: at(UI, "Section::Backup"),
        m9: Cell::Na("the M9 keeps no history of its own to save: the home holds it, and a new board pulls it"),
    },
    Place {
        section: Section::Advanced,
        phone: at(JAVA, "\"advanced\".equals(page)"),
        window: at(GUI_CHAT, "Section::Advanced"),
        terminal: at(UI, "Section::Advanced"),
        m9: at(M9_HOUSE, "Section::Advanced =>"),
    },
];

/// The sections `target` draws, in the list's order: every one it does
/// not mark missing or n/a.
pub fn sections(target: Target) -> impl Iterator<Item = Section> {
    PLACES
        .into_iter()
        .filter(move |p| matches!(p.on(target), Cell::Built(_)))
        .map(|p| p.section)
}

/// One section in the list.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub section: Section,
    pub title: Text,
    pub summary: Text,
    /// The key that opens it where keys do: `1` to `9`.
    pub key: char,
    pub open: Action,
}

/// A line of an open section: words, the value in force, and the verb a
/// press or its key runs.
#[derive(Clone, Debug, PartialEq)]
pub struct PageRow {
    pub label: Text,
    pub value: Option<Text>,
    pub key: Option<char>,
    pub action: Option<Action>,
}

/// The settings as every target draws them: the list, and the open
/// section's rows.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsScreen {
    /// "Settings" over the list, the section's title over its page.
    pub title: Text,
    pub entries: Vec<Entry>,
    pub open: Option<Section>,
    /// The open section's rows as the core knows them.
    pub rows: Vec<PageRow>,
}

/// The list of `target`: what the phone's settings screen, the window's
/// gear and the terminal's `S` list.
pub fn entries(target: Target) -> Vec<Entry> {
    sections(target)
        .enumerate()
        .map(|(i, section)| Entry {
            section,
            title: Text::key(section.title()),
            summary: Text::key(section.summary()),
            key: char::from_digit(u32::try_from(i + 1).unwrap_or(0), 10).unwrap_or('?'),
            open: Action::of(Intent::Settings(Some(section))),
        })
        .collect()
}

/// The list of `target`, with `open`'s rows.
pub fn screen<B>(model: &UiModel<B>, target: Target, open: Option<Section>) -> SettingsScreen {
    let entries = entries(target);
    let title = Text::key(open.map_or(StrId::Settings, Section::title));
    SettingsScreen {
        title,
        entries,
        open,
        rows: open.map(|s| rows(model, target, s)).unwrap_or_default(),
    }
}

fn on_off(on: bool) -> Text {
    Text::key(match on {
        true => StrId::SettingsOn,
        false => StrId::SettingsOff,
    })
}

fn note(id: StrId) -> PageRow {
    PageRow {
        label: Text::key(id),
        value: None,
        key: None,
        action: None,
    }
}

fn verb(label: StrId, value: Option<Text>, key: char, intent: Intent) -> PageRow {
    PageRow {
        label: Text::key(label),
        value,
        key: Some(key),
        action: Some(Action::of(intent)),
    }
}

/// What `section` holds on `target`, as far as the core knows it.
pub fn rows<B>(model: &UiModel<B>, target: Target, section: Section) -> Vec<PageRow> {
    let terminal = target == Target::Terminal;
    match section {
        Section::Profile => Vec::from([verb(StrId::ProfileTitle, None, 'p', Intent::Profile)]),
        // 1642: which mailbox, how it is reached, which devices use it.
        Section::Mailbox => super::mailbox_page::page(model).rows(),
        // The M9 draws the dark tokens only (`Look::m9`), so no theme
        // switch; its type size row is the board's own (`house.rs`).
        Section::Appearance if target == Target::M9 => Vec::new(),
        Section::Appearance => {
            let value = Text::key(match model.theme {
                Theme::Dark => StrId::SettingsDark,
                Theme::Light => StrId::SettingsLight,
            });
            Vec::from([verb(StrId::Theme, Some(value), 't', Intent::Theme)])
        }
        Section::Chats => {
            let size = Text::key(match model.photo_size {
                PhotoSize::Small => StrId::PhotoSmall,
                PhotoSize::Phone => StrId::PhotoPhone,
                PhotoSize::Original => StrId::PhotoOriginal,
            });
            let next = model.photo_size.next();
            let mut rows = Vec::from([verb(
                StrId::PhotoSize,
                Some(size),
                'f',
                Intent::PhotoSize(next),
            )]);
            if terminal {
                rows.push(verb(
                    StrId::SendOnEnter,
                    Some(on_off(model.send_on_enter)),
                    'e',
                    Intent::SendOnEnter,
                ));
            }
            rows
        }
        Section::Notifications => Vec::from([note(StrId::SettingsNotificationsDesktop)]),
        Section::Privacy => {
            let mut rows = Vec::from([verb(
                StrId::ReadReceipts,
                Some(on_off(model.reading.on)),
                'r',
                Intent::ReadReceipts { on: Switch::Toggle },
            )]);
            if terminal {
                rows.push(verb(StrId::Blocked, None, 'b', Intent::ShowBlocked));
            }
            rows
        }
        Section::Maps => match terminal {
            true => Vec::from([verb(StrId::MapDrawing, None, 'm', Intent::MapDrawing)]),
            false => Vec::new(),
        },
        Section::Backup => Vec::from([note(StrId::SettingsBackupDesktop)]),
        // The M9 has no rooms and no help screen; its about rows (the
        // version, the map's data) are the board's own (`house.rs`).
        Section::Advanced if target == Target::M9 => Vec::new(),
        Section::Advanced => {
            let mut rows = Vec::from([
                note(StrId::PropagationNode),
                note(StrId::PropagationNodeText),
            ]);
            rows.extend(hub_rows(model, target));
            rows.push(verb(StrId::Help, None, '?', Intent::Help));
            rows
        }
    }
}

/// How many hubs a page of the hub rows holds (1599): the closest ten,
/// then "More…".
pub const HUB_PAGE: usize = 10;

/// A hub the settings offer (1599): heard, shipped, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offered {
    pub hash: [u8; 16],
    /// Its name; `None` for a nameless announce ("Unnamed hub").
    pub name: Option<alloc::string::String>,
    pub hops: Option<u8>,
    /// On the shipped list.
    pub shipped: bool,
    /// Heard announcing.
    pub heard: bool,
    /// `Some(false)`: it gave no answer when last opened.
    pub answered: Option<bool>,
    /// When it was last heard, unix seconds; `None` for one never heard.
    pub last: Option<i64>,
}

/// The hubs in the order the settings offer them (1599): the heard ones
/// by last seen, the closer first on a tie, a shipped one among them
/// marked as such; then the shipped ones not heard, closest first, as the
/// shipped file is ordered. The hub in force stands first (1659, walk 10,
/// 55: it was tenth in the settings while Explore, which draws this same
/// order, showed it elsewhere), so the settings and Explore list the
/// servers alike.
pub fn offered_hubs(rooms: &super::rooms::Rooms) -> Vec<Offered> {
    let mut out = offered_by_seen(rooms);
    if let Some(at) = rooms
        .hub
        .and_then(|hub| out.iter().position(|o| o.hash == hub))
    {
        let chosen = out.remove(at);
        out.insert(0, chosen);
    }
    out
}

/// [`offered_hubs`] before the hub in force is put first.
fn offered_by_seen(rooms: &super::rooms::Rooms) -> Vec<Offered> {
    let shipped = super::hubs::shipped();
    let mut out: Vec<Offered> = rooms
        .heard
        .hubs()
        .iter()
        .map(|h| {
            let listed = shipped.iter().find(|s| s.hash == h.hash);
            // A name that is empty is none (1618: the walk saw a row with
            // no name at all where a shipped entry's name was "").
            let named = |name: &Option<alloc::string::String>| {
                name.clone().filter(|n| !n.trim().is_empty())
            };
            Offered {
                hash: h.hash,
                name: named(&listed.map(|s| s.name.clone())).or_else(|| named(&h.name)),
                hops: h.hops,
                shipped: listed.is_some(),
                heard: true,
                answered: h.answered,
                last: Some(h.last),
            }
        })
        .collect();
    for hub in shipped {
        if out.iter().any(|o| o.hash == hub.hash) {
            continue;
        }
        out.push(Offered {
            name: Some(hub.name).filter(|n| !n.trim().is_empty()),
            hash: hub.hash,
            // The shipped file's hops are the distance from where the list
            // was measured, not from here (1629, walk 3 new 5: "not seen
            // yet · reachable over 3 stations" said both). The list keeps
            // the file's order, closest first; a hub says how far once
            // this device heard it.
            hops: None,
            shipped: true,
            heard: false,
            answered: None,
            last: None,
        });
    }
    out
}

/// The one hub "suggested" marks (1621, walk 2 saw it on four): the
/// closest shipped hub with a name heard on the mesh that did not fail to
/// answer, the first in the offered order on a tie; none while a hub is in
/// force, whose mark says enough, and none when no such hub was heard.
///
/// Once made, a suggestion holds (1629, walk 3 new 6: it wandered from a
/// nameless server over ShadowLink to 3moelf in ten minutes): `held`
/// stays the one while it still qualifies, however close another one is
/// heard; only a failure to answer or its leaving the list moves it.
pub fn suggested(
    offered: &[Offered],
    chosen: Option<[u8; 16]>,
    held: Option<[u8; 16]>,
) -> Option<[u8; 16]> {
    if chosen.is_some() {
        return None;
    }
    let fits = |o: &&Offered| o.shipped && o.heard && o.answered != Some(false) && o.name.is_some();
    if let Some(held) = held {
        if offered.iter().filter(fits).any(|o| o.hash == held) {
            return Some(held);
        }
    }
    offered
        .iter()
        .filter(fits)
        .min_by_key(|o| o.hops.unwrap_or(u8::MAX))
        .map(|o| o.hash)
}

/// The suggestion after the hubs heard changed: held while it fits,
/// else the closest that does ([`suggested`]). A hub in force holds none.
pub fn settle_suggestion(rooms: &mut super::rooms::Rooms) {
    let before = rooms.heard.held();
    let now = settled(rooms);
    if now != before {
        say_suggestion(rooms);
    }
}

/// The suggestion settled ([`settle_suggestion`]) and said in the event
/// log whether it moved or not: a start says where it stands.
pub fn settle_suggestion_said(rooms: &mut super::rooms::Rooms) {
    settled(rooms);
    say_suggestion(rooms);
}

fn settled(rooms: &mut super::rooms::Rooms) -> Option<[u8; 16]> {
    let now = suggested(&offered_hubs(rooms), rooms.hub, rooms.heard.held());
    rooms.heard.hold(now);
    now
}

/// `ROOMS_SUGGEST` (1653, walk 8, 41: no server said "empfohlen", and
/// nothing on the device told why): the hub held, or `-`, and why: a hub
/// in force (`in_use`), no named shipped hub heard that answers
/// (`none_fits`), or the one marked (`marked`); `fits` counts the hubs
/// that could be.
fn say_suggestion(rooms: &super::rooms::Rooms) {
    let offered = offered_hubs(rooms);
    let fits = offered
        .iter()
        .filter(|o| o.shipped && o.heard && o.answered != Some(false) && o.name.is_some())
        .count();
    let held = rooms.heard.held();
    let why = match (rooms.hub, held) {
        (Some(_), _) => "in_use",
        (None, None) => "none_fits",
        (None, Some(_)) => "marked",
    };
    crate::events::rooms_suggest(held.as_ref(), why, fits);
}

/// What a hub's row says beside its name: how far, "suggested" for the
/// one [`suggested`] names, "no answer", and on the one in force how its
/// link stands (`link`, 1655) and the mark.
fn offered_value(hub: &Offered, chosen: bool, suggested: bool, link: Option<Text>) -> Option<Text> {
    let mut parts = Vec::new();
    match hub.hops {
        Some(1) => parts.push(Text::key(StrId::RoomsHubHop)),
        Some(hops) => parts.push(Text::Key(
            StrId::RoomsHubHops,
            Vec::from([leviculum_ui::strings::Arg::Int(i64::from(hops))]),
        )),
        None => {}
    }
    if suggested {
        parts.push(Text::key(StrId::RoomsHubSuggested));
    }
    if chosen {
        // The rooms screen's words (1655, walk 9, 49: the row said no
        // state), which say a silence themselves.
        match link {
            Some(link) => parts.push(link),
            None if hub.answered == Some(false) => parts.push(Text::key(StrId::RoomsHubNoAnswer)),
            None => {}
        }
        parts.push(Text::Raw(alloc::string::String::from("\u{2713}")));
    } else if hub.answered == Some(false) {
        parts.push(Text::key(StrId::RoomsHubNoAnswer));
    }
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Text::Join(parts)),
    }
}

/// The public rooms' hub (1577, 1599), in Advanced on every target: the
/// line that says which, the hubs [`offered_hubs`] lists by name with a
/// mark on the one in force, ten a page (keys `1` to `9` and `0`), "More…"
/// (`m`) while there are more, none (`n`), another to paste (`a`), and what
/// a hub learns. Never a hash. The terminal turns the pages with "More…";
/// the window and the phone show every hub in their scroll once it was
/// chosen.
pub fn hub_rows<B>(model: &UiModel<B>, target: Target) -> Vec<PageRow> {
    use super::rooms::HubPick;
    let chosen = model.rooms.hub;
    let mark = |on: bool| on.then(|| Text::Raw(alloc::string::String::from("\u{2713}")));
    let mut rows = Vec::from([PageRow {
        label: Text::key(StrId::RoomsHub),
        value: Some(super::rooms::hub_label(&model.rooms)),
        key: None,
        action: None,
    }]);
    let offered = offered_hubs(&model.rooms);
    let suggest = suggested(&offered, chosen, model.rooms.heard.held());
    let known = |hub: [u8; 16]| offered.iter().any(|o| o.hash == hub);
    let own = chosen.is_some_and(|hub| !known(hub));
    let pages = offered.len().div_ceil(HUB_PAGE).max(1);
    let page = model.rooms.hub_page % pages;
    let shown: &[Offered] = match (target, page) {
        (Target::Terminal, _) => {
            let start = page * HUB_PAGE;
            &offered[start..offered.len().min(start + HUB_PAGE)]
        }
        (_, 0) => &offered[..offered.len().min(HUB_PAGE)],
        _ => &offered[..],
    };
    for (i, hub) in shown.iter().enumerate() {
        let key = match i {
            0..=8 => char::from_digit(u32::try_from(i + 1).unwrap_or(0), 10),
            9 => Some('0'),
            _ => None,
        };
        rows.push(PageRow {
            label: match &hub.name {
                Some(name) => Text::Raw(name.clone()),
                None => Text::key(StrId::RoomsHubUnnamed),
            },
            value: offered_value(
                hub,
                chosen == Some(hub.hash),
                suggest == Some(hub.hash),
                super::rooms::link_words(model).filter(|_| chosen == Some(hub.hash)),
            ),
            key,
            action: Some(Action::of(Intent::RoomsHub(HubPick::Hub(hub.hash)))),
        });
    }
    let more = match target {
        Target::Terminal => pages > 1,
        _ => page == 0 && pages > 1,
    };
    if more {
        rows.push(PageRow {
            label: Text::key(StrId::RoomsHubMore),
            value: None,
            key: Some('m'),
            action: Some(Action::of(Intent::RoomsHub(HubPick::More))),
        });
    }
    rows.push(PageRow {
        label: Text::key(StrId::RoomsHubNone),
        value: mark(chosen.is_none()),
        key: Some('n'),
        action: Some(Action::of(Intent::RoomsHub(HubPick::None))),
    });
    rows.push(PageRow {
        label: Text::key(StrId::RoomsHubPaste),
        value: own.then(|| super::rooms::hub_label(&model.rooms)),
        key: Some('a'),
        action: Some(Action::of(Intent::RoomsHub(HubPick::Paste))),
    });
    rows.push(PageRow {
        label: super::rooms::name_notice(model),
        value: None,
        key: None,
        action: None,
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::strings::Lang;

    /// 1599: the hub rows offer the heard hubs by last seen, a shipped one
    /// among them marked "suggested", a nameless one as "Unnamed hub", one
    /// that did not answer marked so and kept; then the shipped ones not
    /// heard. Ten a page with "More…": the terminal turns the pages, the
    /// window shows all from the second. No hash on screen.
    #[test]
    fn the_hub_rows_offer_heard_hubs_first_ten_a_page() {
        use crate::ui::hubs::HeardHub;
        use crate::ui::rooms::{Down, RoomsEvent};
        let mut model = UiModel::<crate::ui::tests::Field>::new(&[]);
        let labels = |rows: &[PageRow]| -> Vec<String> {
            rows.iter().map(|r| r.label.resolve(Lang::En)).collect()
        };
        // A fresh install: the shipped list, closest first, ten and more.
        let rows = hub_rows(&model, Target::Terminal);
        let shipped = crate::ui::hubs::shipped();
        assert_eq!(labels(&rows)[1], shipped[0].name);
        let keys: Vec<Option<char>> = rows.iter().map(|r| r.key).collect();
        assert_eq!(
            keys[1..=10],
            [
                Some('1'),
                Some('2'),
                Some('3'),
                Some('4'),
                Some('5'),
                Some('6'),
                Some('7'),
                Some('8'),
                Some('9'),
                Some('0')
            ]
        );
        assert_eq!(labels(&rows)[11], "More…");
        assert_eq!(labels(&rows)[12], "None");
        let hadarc = shipped.iter().find(|h| h.name == "HADARC").map(|h| h.hash);
        let hadarc = hadarc.expect("shipped");
        let heard = |hash: [u8; 16], name: Option<&str>, hops: u8, at: i64| HeardHub {
            hash,
            name: name.map(Into::into),
            hops: Some(hops),
            first: at,
            last: at,
            answered: None,
        };
        model
            .rooms
            .heard
            .hear(heard([0xa1; 16], Some("Villa"), 4, 300));
        model.rooms.heard.hear(heard([0xa2; 16], None, 2, 300));
        model
            .rooms
            .heard
            .hear(heard(hadarc, Some("HADARC rrc"), 4, 200));
        model.rooms.hub = Some([0xa1; 16]);
        crate::ui::rooms::fold(&mut model, RoomsEvent::Down(Down::NoAnswer));
        let rows = hub_rows(&model, Target::Terminal);
        let said: Vec<String> = rows
            .iter()
            .map(|r| match &r.value {
                Some(value) => {
                    alloc::format!("{}: {}", r.label.resolve(Lang::En), value.resolve(Lang::En))
                }
                None => r.label.resolve(Lang::En),
            })
            .collect();
        assert_eq!(said[0], "Server for public rooms: Villa");
        // The one in use first (1659, walk 10, 55: it stood tenth), saying
        // its link in the rooms screen's words (1655).
        assert_eq!(
            said[1],
            "Villa: reachable over 4 stations · The server does not answer. · \u{2713}"
        );
        assert_eq!(
            said[2], "Server without a name: reachable over 2 stations",
            "the rest by last seen, the closer first on a tie"
        );
        // A server is in use: none is suggested (1621).
        assert_eq!(said[3], "HADARC: reachable over 4 stations");
        // A shipped server never heard says no distance (1629, walk 3 new
        // 5): the shipped file's hops were measured elsewhere.
        assert_eq!(said[4], shipped[0].name.clone());
        assert!(
            said.iter()
                .all(|s| !s.contains("a1a1") && !s.contains("rrc")),
            "{said:?}"
        );
        // The pages: 70 hubs make seven; More turns them, then wraps.
        let all = offered_hubs(&model.rooms);
        assert_eq!(all.len(), shipped.len() + 2, "HADARC once");
        crate::ui::rooms::more(&mut model);
        let second = hub_rows(&model, Target::Terminal);
        assert_eq!(
            labels(&second)[1],
            all[HUB_PAGE].name.clone().unwrap_or_default()
        );
        assert_eq!(second.iter().filter(|r| matches!(&r.action, Some(a) if matches!(a.intent, Intent::RoomsHub(crate::ui::rooms::HubPick::Hub(_))))).count(), HUB_PAGE);
        // The window shows every hub from the second page on, and no More.
        let window = hub_rows(&model, Target::Window);
        let hubs_shown = window
            .iter()
            .filter(|r| matches!(&r.action, Some(a) if matches!(a.intent, Intent::RoomsHub(crate::ui::rooms::HubPick::Hub(_)))))
            .count();
        assert_eq!(hubs_shown, all.len());
        assert!(!labels(&window).contains(&"More…".to_string()));
        for _ in 1..all.len().div_ceil(HUB_PAGE) {
            crate::ui::rooms::more(&mut model);
        }
        assert_eq!(model.rooms.hub_page, 0, "after the last page, the first");
    }

    /// The order is the phone's, every section has its row in the table,
    /// and its key reads back.
    /// The row of the server in use says how its link stands, in the
    /// rooms screen's words (1655, walk 9, 49: name and tick only); the
    /// other rows and what a tap does stay as they were.
    #[test]
    fn the_server_in_use_says_how_its_link_stands() {
        use crate::ui::rooms::{Down, HubState};
        let mut model = UiModel::<crate::ui::tests::Field>::new(&[]);
        let shipped = crate::ui::hubs::shipped();
        let before = hub_rows(&model, Target::Phone);
        model.rooms.hub = Some(shipped[0].hash);
        model.rooms.enabled = true;
        let value = |model: &UiModel<crate::ui::tests::Field>| {
            hub_rows(model, Target::Phone)[1]
                .value
                .as_ref()
                .map(|v| v.resolve(Lang::De))
        };
        assert_eq!(
            value(&model).as_deref(),
            Some("Noch nicht verbunden. · \u{2713}")
        );
        model.rooms.state = HubState::Linking;
        assert_eq!(value(&model).as_deref(), Some("Verbinde… · \u{2713}"));
        model.rooms.state = HubState::Ready;
        assert_eq!(value(&model).as_deref(), Some("Verbunden · \u{2713}"));
        model.rooms.state = HubState::Down(Down::NoAnswer);
        assert_eq!(
            value(&model).as_deref(),
            Some("Server antwortet nicht. · \u{2713}")
        );
        // The others: as before, and every row's tap is the same verb.
        let after = hub_rows(&model, Target::Phone);
        assert_eq!(before.len(), after.len());
        for (b, a) in before.iter().zip(&after).skip(2) {
            assert_eq!((&b.label, &b.action), (&a.label, &a.action));
        }
        assert_eq!(before[1].action, after[1].action);
    }

    #[test]
    fn nine_sections_in_the_phones_order_each_placed_once() {
        let titles: Vec<&str> = SECTIONS.iter().map(|s| s.title().text(Lang::En)).collect();
        assert_eq!(
            titles,
            [
                "Profile",
                "Mailbox",
                "Appearance",
                "Chats",
                "Notifications",
                "Privacy",
                "Maps",
                "Backup",
                "Advanced"
            ]
        );
        assert_eq!(
            PLACES.iter().map(|p| p.section).collect::<Vec<_>>(),
            SECTIONS,
            "the table is the list"
        );
        for section in SECTIONS {
            assert_eq!(Section::of_key(section.key()), Some(section));
        }
        assert_eq!(Section::of_key("nothing"), None);
    }

    /// The guard (1450): every cell of [`PLACES`] is drawn with its token
    /// in its file, or says why not. A window, a terminal and a phone draw
    /// all nine.
    #[test]
    fn every_target_draws_every_section_it_does_not_mark_na() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut failures = Vec::new();
        for place in PLACES {
            for target in [Target::Phone, Target::Window, Target::Terminal, Target::M9] {
                match place.on(target) {
                    Cell::Built(Via::At(file, token)) => {
                        let text = std::fs::read_to_string(root.join(file)).unwrap_or_default();
                        if !text.contains(token) {
                            failures.push(alloc::format!(
                                "{:?} on {target:?}: {file} has no {token:?}",
                                place.section
                            ));
                        }
                    }
                    Cell::Built(other) => failures.push(alloc::format!(
                        "{:?} on {target:?}: {other:?} is no file and token",
                        place.section
                    )),
                    Cell::Missing(why) | Cell::Na(why) if why.trim().is_empty() => failures.push(
                        alloc::format!("{:?} on {target:?}: no reason", place.section),
                    ),
                    Cell::Missing(_) | Cell::Na(_) => {}
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
        for target in [Target::Phone, Target::Window, Target::Terminal] {
            assert_eq!(
                sections(target).collect::<Vec<_>>(),
                SECTIONS,
                "{target:?} draws all nine"
            );
        }
        // The M9 (1561): the type size and the about rows behind its
        // house key; every other cell says why not.
        assert_eq!(
            sections(Target::M9).collect::<Vec<_>>(),
            [Section::Appearance, Section::Advanced]
        );
        let entries = entries(Target::M9);
        assert_eq!((entries[0].key, entries[1].key), ('1', '2'));
    }

    /// The control of the guard's ingredient: a token a file lacks is
    /// caught.
    #[test]
    fn a_false_place_is_caught() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let text = std::fs::read_to_string(root.join(UI)).expect("ui.rs");
        assert!(text.contains("Section::Privacy"), "the real token");
        assert!(!text.contains("Section::PrivacyGone"), "a false one");
    }

    /// The screen: the list numbered `1` to `9`, each entry opening its
    /// section; an open section's rows hold the model's switches as they
    /// stand.
    #[test]
    fn the_screen_lists_the_sections_and_a_page_holds_the_switches() {
        let mut model = UiModel::<crate::ui::tests::Field>::new(&[]);
        let list = screen(&model, Target::Terminal, None);
        assert_eq!(list.title, Text::key(StrId::Settings));
        assert_eq!(list.entries.len(), 9);
        assert_eq!(list.entries[0].key, '1');
        assert_eq!(list.entries[8].key, '9');
        assert_eq!(
            list.entries[5].open.intent,
            Intent::Settings(Some(Section::Privacy))
        );
        assert!(list.rows.is_empty());
        model.reading.on = false;
        let privacy = screen(&model, Target::Terminal, Some(Section::Privacy));
        assert_eq!(privacy.title, Text::key(StrId::SettingsPrivacy));
        assert_eq!(privacy.rows[0].label, Text::key(StrId::ReadReceipts));
        assert_eq!(privacy.rows[0].value, Some(Text::key(StrId::SettingsOff)));
        let appearance = screen(&model, Target::Window, Some(Section::Appearance));
        assert_eq!(
            appearance.rows[0].action.as_ref().map(|a| a.intent.clone()),
            Some(Intent::Theme),
            "the theme switch is Appearance's"
        );
    }
}
