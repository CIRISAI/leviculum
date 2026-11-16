//! The Explore tab on the phone (1578, #232): the pages and the public
//! rooms under one button, as the window and the terminal carry it
//! (`loquax::ui::explore`).
//!
//! Two halves, both pure, as [`crate::rooms`]'s: [`screen`] flattens the
//! model's Explore node into the [`schema::ExploreScreen`] Java draws, its
//! words resolved in the phone's language; [`events`] reads a
//! [`schema::Request::Explore`] back into the model's events. Java adds
//! no logic of the tab: the order, the headings and what a row opens are
//! the library's.

use loquax::ui::explore::{self as model_explore, Explore, ExploreEvent};
use loquax::ui::scene::TabId;
use loquax::ui::strings::{Arg, Lang, StrId, Text};
use loquax::ui::{UiEvent, UiModel};

use crate::schema::{self, ExploreHub, ExplorePage, ExploreScreen, RoomsRow};

/// What is in front, in the schema's word.
fn showing<B>(model: &UiModel<B>) -> &'static str {
    match () {
        _ if !model.explore.open => "",
        _ if model.browse.showing() => "browse",
        _ if model.rooms.showing() => "rooms",
        _ if Explore::showing(model) => "list",
        _ => "",
    }
}

fn hub(row: &model_explore::HubRow, lang: Lang) -> ExploreHub {
    ExploreHub {
        dest: hex(&row.hash),
        name: row.name.resolve(lang),
        value: said(row.seen.iter().chain(row.value.iter()), lang),
        chosen: row.chosen,
    }
}

/// Texts said one after the other, ` · ` between.
fn said<'a>(parts: impl Iterator<Item = &'a Text>, lang: Lang) -> String {
    parts
        .map(|t| t.resolve(lang))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn text_or_empty(text: &Option<Text>, lang: Lang) -> String {
    text.as_ref().map(|t| t.resolve(lang)).unwrap_or_default()
}

fn hex(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The Explore tab as Java draws it, in `lang`.
pub fn screen<B>(model: &UiModel<B>, lang: Lang, v: u32) -> ExploreScreen {
    let node = model_explore::node(model);
    let colour = |c: u32| format!("{c:06x}");
    let pages = &node.pages;
    let rooms = &node.rooms;
    ExploreScreen {
        v,
        showing: showing(model).to_string(),
        title: Text::key(StrId::TabExplore).resolve(lang),
        pages_title: pages.title.resolve(lang),
        query: pages.query.clone(),
        placeholder: pages.placeholder.resolve(lang),
        sort: pages.sort.resolve(lang),
        az: pages.az,
        address: pages
            .address
            .as_ref()
            .map(|(url, _)| {
                Text::Key(
                    StrId::ExploreOpenAddress,
                    Vec::from([Arg::Str(url.clone())]),
                )
                .resolve(lang)
            })
            .unwrap_or_default(),
        pages: pages
            .groups
            .iter()
            .flat_map(|group| {
                group
                    .rows
                    .iter()
                    .enumerate()
                    .map(move |(i, row)| ExplorePage {
                        heading: match i {
                            0 => group.heading.resolve(lang),
                            _ => String::new(),
                        },
                        dest: hex(&row.dest),
                        label: row.label.resolve(lang),
                        said: said(row.seen.iter().chain(row.hops.iter()), lang),
                        bookmarked: row.bookmarked,
                        url: row.url.clone(),
                    })
            })
            .collect(),
        pages_empty: pages
            .empty
            .as_ref()
            .map(|t| t.resolve(lang))
            .unwrap_or_default(),
        pages_off: pages
            .off
            .as_ref()
            .map(|t| t.resolve(lang))
            .unwrap_or_default(),
        as_of: text_or_empty(&pages.as_of, lang),
        rooms_title: rooms.title.resolve(lang),
        joined_empty: text_or_empty(&rooms.joined_empty, lang),
        open_rooms: text_or_empty(&rooms.open_rooms, lang),
        servers_title: rooms.servers_title.resolve(lang),
        servers_empty: text_or_empty(&rooms.servers_empty, lang),
        joined: rooms
            .joined
            .iter()
            .map(|row| RoomsRow {
                name: row.name.clone(),
                unread: row.unread,
                selected: false,
                initials: row.avatar.initials.clone(),
                icon_fg: colour(row.avatar.fg),
                icon_bg: colour(row.avatar.bg),
                public: row.public.resolve(lang),
            })
            .collect(),
        hubs: rooms.hubs.iter().map(|h| hub(h, lang)).collect(),
        sheet_dest: node
            .sheet
            .as_ref()
            .map(|s| hex(&s.hash))
            .unwrap_or_default(),
        sheet_name: node
            .sheet
            .as_ref()
            .map(|s| s.name.resolve(lang))
            .unwrap_or_default(),
        sheet_said: node
            .sheet
            .as_ref()
            .map(|s| said(s.seen.iter().chain(s.far.iter()), lang))
            .unwrap_or_default(),
        sheet_chosen: node.sheet.as_ref().is_some_and(|s| s.chosen),
        sheet_button: node
            .sheet
            .as_ref()
            .map(|s| s.button.resolve(lang))
            .unwrap_or_default(),
        sheet_notice: node
            .sheet
            .as_ref()
            .map(|s| s.notice.resolve(lang))
            .unwrap_or_default(),
        sheet_rooms_title: node
            .sheet
            .as_ref()
            .map(|s| s.rooms_title.resolve(lang))
            .unwrap_or_default(),
        sheet_rooms: node
            .sheet
            .as_ref()
            .map(|s| s.rooms.clone())
            .unwrap_or_default(),
        sheet_rooms_note: node
            .sheet
            .as_ref()
            .map(|s| text_or_empty(&s.rooms_note, lang))
            .unwrap_or_default(),
        sheet_retry: node
            .sheet
            .as_ref()
            .map(|s| text_or_empty(&s.retry, lang))
            .unwrap_or_default(),
        sheet_asking: node.sheet.as_ref().is_some_and(|s| s.asking),
        sheet_open_rooms: node
            .sheet
            .as_ref()
            .map(|s| text_or_empty(&s.open_rooms, lang))
            .unwrap_or_default(),
        sheet_close: node
            .sheet
            .as_ref()
            .map(|s| s.close.resolve(lang))
            .unwrap_or_default(),
    }
}

fn dest(text: &str) -> Result<[u8; 16], String> {
    loquax::address::parse(text).map_err(|_| format!("no destination {text:?}"))
}

/// The model's events for a [`schema::Request::Explore`].
pub fn events(verb: &str, text: &str, to: &str, index: i64) -> Result<Vec<UiEvent>, String> {
    let explore = |event| Vec::from([UiEvent::Explore(event)]);
    Ok(match verb {
        "tab" => Vec::from([UiEvent::Tab(TabId::Explore)]),
        "chats" => Vec::from([UiEvent::Tab(TabId::Chats)]),
        "typed" => explore(ExploreEvent::Typed(text.to_string())),
        "az" => explore(ExploreEvent::Az(index != 0)),
        "open" => explore(ExploreEvent::OpenPage(dest(to)?)),
        "address" => explore(ExploreEvent::OpenAddress),
        "bookmark" => explore(ExploreEvent::Bookmark(dest(to)?)),
        "room" => explore(ExploreEvent::OpenRoom(
            usize::try_from(index).map_err(|_| format!("no room {index}"))?,
        )),
        "hub" => explore(ExploreEvent::Hub(dest(to)?)),
        "use" => explore(ExploreEvent::UseHub(dest(to)?)),
        "rooms" => explore(ExploreEvent::OpenRooms),
        "join" => explore(ExploreEvent::JoinRoom(dest(to)?, text.to_string())),
        "ask" => explore(ExploreEvent::AskAgain(dest(to)?)),
        "close" => explore(ExploreEvent::CloseSheet),
        "touch" => explore(ExploreEvent::Touch(index != 0)),
        other => return Err(format!("no explore verb {other:?}")),
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
        let node = "5a".repeat(16);
        assert_eq!(
            events("tab", "", "", 0),
            Ok(vec![UiEvent::Tab(TabId::Explore)])
        );
        assert_eq!(
            events("open", "", &node, 0),
            Ok(vec![UiEvent::Explore(ExploreEvent::OpenPage([0x5a; 16]))])
        );
        assert_eq!(
            events("rooms", "", "", 0),
            Ok(vec![UiEvent::Explore(ExploreEvent::OpenRooms)])
        );
        assert_eq!(
            events("az", "", "", 1),
            Ok(vec![UiEvent::Explore(ExploreEvent::Az(true))])
        );
        assert_eq!(
            events("typed", "marcel", "", 0),
            Ok(vec![UiEvent::Explore(ExploreEvent::Typed("marcel".into()))])
        );
        // 1618: the sheet's verbs and the finger on the list.
        assert_eq!(
            events("hub", "", &node, 0),
            Ok(vec![UiEvent::Explore(ExploreEvent::Hub([0x5a; 16]))])
        );
        assert_eq!(
            events("use", "", &node, 0),
            Ok(vec![UiEvent::Explore(ExploreEvent::UseHub([0x5a; 16]))])
        );
        assert_eq!(
            events("join", "#lobby", &node, 0),
            Ok(vec![UiEvent::Explore(ExploreEvent::JoinRoom(
                [0x5a; 16],
                "#lobby".into()
            ))])
        );
        assert_eq!(
            events("close", "", "", 0),
            Ok(vec![UiEvent::Explore(ExploreEvent::CloseSheet)])
        );
        assert_eq!(
            events("touch", "", "", 1),
            Ok(vec![UiEvent::Explore(ExploreEvent::Touch(true))])
        );
        assert_eq!(
            events("touch", "", "", 0),
            Ok(vec![UiEvent::Explore(ExploreEvent::Touch(false))])
        );
        // The controls: a bad destination and an unknown verb are refused.
        assert!(events("open", "", "zz", 0).is_err());
        assert!(events("room", "", "", -1).is_err());
        assert!(events("fly", "", "", 0).is_err());
    }
}
