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
//!   ([`super::intent::Intent::Settings`]), and on the M9 none yet
//!   ([`PLACES`] says why).
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

/// The M9 has no settings yet: the reason each of its cells gives.
const M9: Cell = Cell::Missing(
    "the M9 has no menu screen to list the sections in: a screen in loquax-m9 that draws \
     SettingsScreen, and the key that opens it (the home key is the list's, 1440)",
);

/// Where each section is drawn. The window's and the terminal's cells are
/// the page's handler for the section; the phone's the arm of
/// `SettingsActivity.refresh` (or the entry's, for the profile).
pub const PLACES: [Place; 9] = [
    Place {
        section: Section::Profile,
        phone: at(JAVA, "\"profile\".equals(target)"),
        window: at(GUI_CHAT, "Section::Profile =>"),
        terminal: at(UI, "Section::Profile =>"),
        m9: M9,
    },
    Place {
        section: Section::Mailbox,
        phone: at(JAVA, "\"mailbox\".equals(page)"),
        window: at(GUI_CHAT, "Section::Mailbox"),
        terminal: at(UI, "Section::Mailbox"),
        m9: M9,
    },
    Place {
        section: Section::Appearance,
        phone: at(JAVA, "\"appearance\".equals(page)"),
        window: at(GUI_CHAT, "Section::Appearance =>"),
        terminal: at(UI, "Section::Appearance"),
        m9: M9,
    },
    Place {
        section: Section::Chats,
        phone: at(JAVA, "\"chats\".equals(page)"),
        window: at(GUI_CHAT, "Section::Chats =>"),
        terminal: at(UI, "Section::Chats"),
        m9: M9,
    },
    Place {
        section: Section::Notifications,
        phone: at(JAVA, "\"notifications\".equals(page)"),
        window: at(GUI_CHAT, "Section::Notifications"),
        terminal: at(UI, "Section::Notifications"),
        m9: M9,
    },
    Place {
        section: Section::Privacy,
        phone: at(JAVA, "\"privacy\".equals(page)"),
        window: at(GUI_CHAT, "Section::Privacy =>"),
        terminal: at(UI, "Section::Privacy"),
        m9: M9,
    },
    Place {
        section: Section::Maps,
        phone: at(JAVA, "\"maps\".equals(page)"),
        window: at(GUI_CHAT, "Section::Maps =>"),
        terminal: at(UI, "Section::Maps"),
        m9: M9,
    },
    Place {
        section: Section::Backup,
        phone: at(JAVA, "\"backup\".equals(page)"),
        window: at(GUI_CHAT, "Section::Backup"),
        terminal: at(UI, "Section::Backup"),
        m9: M9,
    },
    Place {
        section: Section::Advanced,
        phone: at(JAVA, "\"advanced\".equals(page)"),
        window: at(GUI_CHAT, "Section::Advanced"),
        terminal: at(UI, "Section::Advanced"),
        m9: M9,
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
        Section::Mailbox => {
            let (line, state) = super::profile::mailbox_words(&model.profile.mailbox);
            let mut rows = Vec::from([PageRow {
                label: line,
                value: None,
                key: None,
                action: None,
            }]);
            rows.extend(state.map(|state| PageRow {
                label: state,
                value: None,
                key: None,
                action: None,
            }));
            rows
        }
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
        Section::Advanced => {
            let mut rows = Vec::from([
                note(StrId::PropagationNode),
                note(StrId::PropagationNodeText),
            ]);
            rows.extend(hub_rows(model));
            rows.push(verb(StrId::Help, None, '?', Intent::Help));
            rows
        }
    }
}

/// The public rooms' hub (1577), in Advanced on every target: the line
/// that says which, each shipped hub by its name with a mark on the one
/// in force (keys `1` on), none (`0`), another to paste (`a`), and what
/// a hub learns. Never a hash.
pub fn hub_rows<B>(model: &UiModel<B>) -> Vec<PageRow> {
    use super::rooms::HubPick;
    let chosen = model.rooms.hub;
    let mark = |on: bool| on.then(|| Text::Raw(alloc::string::String::from("\u{2713}")));
    let mut rows = Vec::from([PageRow {
        label: Text::key(StrId::RoomsHub),
        value: Some(super::rooms::hub_label(&model.rooms)),
        key: None,
        action: None,
    }]);
    let shipped = super::hubs::shipped();
    let own = chosen.is_some_and(|hub| !shipped.iter().any(|h| h.hash == hub));
    for (i, hub) in shipped.into_iter().enumerate() {
        let key = char::from_digit(u32::try_from(i + 1).unwrap_or(0), 10);
        rows.push(PageRow {
            label: Text::Raw(hub.name),
            value: mark(chosen == Some(hub.hash)),
            key,
            action: Some(Action::of(Intent::RoomsHub(HubPick::Hub(hub.hash)))),
        });
    }
    rows.push(PageRow {
        label: Text::key(StrId::RoomsHubNone),
        value: mark(chosen.is_none()),
        key: Some('0'),
        action: Some(Action::of(Intent::RoomsHub(HubPick::None))),
    });
    rows.push(PageRow {
        label: Text::key(StrId::RoomsHubPaste),
        value: own.then(|| super::rooms::hub_label(&model.rooms)),
        key: Some('a'),
        action: Some(Action::of(Intent::RoomsHub(HubPick::Paste))),
    });
    rows.push(note(StrId::RoomsNameVisible));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::strings::Lang;

    /// The order is the phone's, every section has its row in the table,
    /// and its key reads back.
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
