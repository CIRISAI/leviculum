//! The Rooms tab on the phone (1384, #232): Reticulum Relay Chat on the
//! hub the config's `rooms_hub` names, as the terminal and the window
//! carry it since 1379 (`loquax::ui::rooms`).
//!
//! Two halves, both pure, as [`crate::browser`]'s: [`screen`] flattens the
//! model's Rooms surface ([`loquax::ui::rooms::node`]) into the
//! [`schema::RoomsScreen`] Java draws, its words resolved in the phone's
//! language; [`events`] reads a [`schema::Request::Rooms`] back into the
//! model's events. Java adds no logic of the Rooms: its field hands the
//! input line in whole, and the model reads it as the terminal's. Since
//! 1450 a room is drawn as a chat, its header's people and its menu's
//! "Leave room" verbs of their own.

use leviculum_rrc::LineKind;
use loquax::ui::rooms::{self as model_rooms, RoomLineKind, RoomsEvent};
use loquax::ui::scene::TabId;
use loquax::ui::strings::Lang;
use loquax::ui::{UiEvent, UiModel};

use crate::schema::{self, RoomsLine, RoomsRow, RoomsScreen, RoomsVerb};

/// The Rooms tab as Java draws it, in `lang`. Without a hub it says only
/// that there is no tab.
pub fn screen<B>(model: &UiModel<B>, lang: Lang, v: u32) -> RoomsScreen {
    let rooms = &model.rooms;
    if !rooms.enabled {
        return RoomsScreen {
            v,
            ..RoomsScreen::default()
        };
    }
    let node = model_rooms::node(model);
    let hex = |c: u32| format!("{c:06x}");
    let mut shown = RoomsScreen {
        v,
        enabled: true,
        open: rooms.showing(),
        // "Räume auf EuskalReticulum" with no room open (1659, walk 10, 52).
        title: node.title.resolve(lang),
        status: node
            .status
            .as_ref()
            .map(|s| s.resolve(lang))
            .unwrap_or_default(),
        rows: node
            .rows
            .iter()
            .map(|row| RoomsRow {
                name: row.name.clone(),
                unread: row.unread,
                selected: row.selected,
                initials: row.avatar.initials.clone(),
                icon_fg: hex(row.avatar.fg),
                icon_bg: hex(row.avatar.bg),
                public: row.public.resolve(lang),
            })
            .collect(),
        input: node.input.clone(),
        placeholder: node.placeholder.resolve(lang),
        empty: node
            .empty
            .as_ref()
            .map(|e| e.resolve(lang))
            .unwrap_or_default(),
        retry: node
            .retry
            .as_ref()
            .map(|r| r.resolve(lang))
            .unwrap_or_default(),
        ..RoomsScreen::default()
    };
    if let Some(listed) = &node.listed {
        shown.listed_title = listed.title.resolve(lang);
        shown.listed.clone_from(&listed.rooms);
        shown.listed_note = listed
            .note
            .as_ref()
            .map(|n| n.resolve(lang))
            .unwrap_or_default();
        shown.other = listed
            .other
            .as_ref()
            .map(|o| o.resolve(lang))
            .unwrap_or_default();
    }
    if let Some(open) = &node.open {
        shown.room = open.name.clone();
        shown.initials = open.avatar.initials.clone();
        shown.icon_fg = hex(open.avatar.fg);
        shown.icon_bg = hex(open.avatar.bg);
        shown.subtitle = open.subtitle.resolve(lang);
        shown.people_shown = open.people.is_some();
        shown.people = open
            .people
            .iter()
            .flatten()
            .map(|p| p.resolve(lang))
            .collect();
        shown.people_note = open.people_note.resolve(lang);
        shown.menu = open
            .menu
            .iter()
            .map(|verb| {
                let (name, index) = match verb.event {
                    RoomsEvent::People(show) => ("people", i64::from(show)),
                    _ => ("leave", 0),
                };
                RoomsVerb {
                    label: verb.label.resolve(lang),
                    verb: name.to_string(),
                    index,
                }
            })
            .collect();
        shown.live = open.live.resolve(lang);
        shown.lines = open
            .lines
            .iter()
            .map(|line| RoomsLine {
                who: line.who.resolve(lang),
                text: line.text.resolve(lang),
                kind: kind(line.kind).to_string(),
                ours: line.ours,
                bubble: line.bubble,
                named: line.named,
                initials: line
                    .avatar
                    .as_ref()
                    .map(|a| a.initials.clone())
                    .unwrap_or_default(),
                icon_fg: line.avatar.as_ref().map(|a| hex(a.fg)).unwrap_or_default(),
                icon_bg: line.avatar.as_ref().map(|a| hex(a.bg)).unwrap_or_default(),
                time: line.time.clone().unwrap_or_default(),
            })
            .collect();
    }
    shown
}

/// A line's kind in the schema's word.
fn kind(kind: RoomLineKind) -> &'static str {
    match kind {
        RoomLineKind::Said(LineKind::Msg) => "msg",
        RoomLineKind::Said(LineKind::Notice) => "notice",
        RoomLineKind::Said(LineKind::Action) => "action",
        RoomLineKind::Hub => "hub",
        RoomLineKind::Joined => "joined",
        RoomLineKind::Parted => "parted",
        RoomLineKind::Error => "error",
    }
}

/// The model's events for a [`schema::Request::Rooms`]: `say` is the
/// field's text as the input line, then sent, as the window's field does.
pub fn events(verb: &str, text: &str, index: i64) -> Result<Vec<UiEvent>, String> {
    let rooms = |event| UiEvent::Rooms(event);
    Ok(match verb {
        "tab" => Vec::from([UiEvent::Tab(TabId::Rooms)]),
        "chats" => Vec::from([UiEvent::Tab(TabId::Chats)]),
        "show" => Vec::from([rooms(RoomsEvent::Show(usize::try_from(index).ok()))]),
        "say" => Vec::from([
            rooms(RoomsEvent::Typed(text.to_string())),
            rooms(RoomsEvent::Submit),
        ]),
        // The open room's header and menu (1450).
        "people" => Vec::from([rooms(RoomsEvent::People(index != 0))]),
        "leave" => Vec::from([rooms(RoomsEvent::Leave)]),
        // A room the hub names, tapped (1652): joined by its name as the
        // hub spells it, and opened once the hub says we are in.
        "join" => Vec::from([rooms(RoomsEvent::Join(text.to_string()))]),
        // "Connect" or "Try again" under the link's state (1652).
        "retry" => Vec::from([rooms(RoomsEvent::Retry)]),
        other => return Err(format!("no rooms verb {other:?}")),
    })
}

/// The request's encoded answer, for the facade's tests and Java alike.
pub fn encode<B>(model: &UiModel<B>, lang: Lang, v: u32) -> Vec<u8> {
    schema::encode(&screen(model, lang, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verbs_read_as_the_models_events() {
        assert_eq!(events("tab", "", 0), Ok(vec![UiEvent::Tab(TabId::Rooms)]));
        assert_eq!(
            events("show", "", 2),
            Ok(vec![UiEvent::Rooms(RoomsEvent::Show(Some(2)))])
        );
        assert_eq!(
            events("show", "", -1),
            Ok(vec![UiEvent::Rooms(RoomsEvent::Show(None))]),
            "a negative index is the list"
        );
        assert_eq!(
            events("say", "#lab", 0),
            Ok(vec![
                UiEvent::Rooms(RoomsEvent::Typed("#lab".to_string())),
                UiEvent::Rooms(RoomsEvent::Submit),
            ])
        );
    }

    #[test]
    fn a_listed_room_joins_by_its_name_and_retry_links() {
        assert_eq!(
            events("join", "general chat", 0),
            Ok(vec![UiEvent::Rooms(RoomsEvent::Join(
                "general chat".to_string()
            ))])
        );
        assert_eq!(
            events("retry", "", 0),
            Ok(vec![UiEvent::Rooms(RoomsEvent::Retry)])
        );
    }

    #[test]
    fn a_verb_the_tab_does_not_know_is_refused() {
        assert!(events("list", "", 0).is_err());
        assert!(events("part", "", 0).is_err(), "the menu's verb is leave");
        assert!(events("", "", 0).is_err());
    }
}
