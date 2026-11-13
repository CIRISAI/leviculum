//! What the board says it shows (1533): one `[PAGE]` line whenever the
//! page or the cursor on it changes, so a bench walk asserts where it is
//! without a frame dump.
//!
//! ```text
//! [PAGE] name=<page> cursor=<n> top=<n> [zoom=<z>] t=<ms>
//! ```
//!
//! `name` is the screen the keys are in: `chats`, `chat`, `compose`,
//! `members`, `ask`, `pick`, `name` (a group's screens, [`Place`]), `map`,
//! `pair` (the pairing screen), `house` (the overview, 1532, [`house`]),
//! `search`, or `other`. `cursor` is the
//! row the selection is on: the list's selected row in the whole order,
//! the chat's message, the compose field's caret, the M9's own row on a
//! group's sheet. `top` is the first row on screen in the same count: the
//! list's first entry shown, the chat's topmost message shown. The map
//! adds its zoom; its cursor and top are 0.

use alloc::string::String;
use core::fmt;

use leviculum_ui::input::InputBuffer;
use loquax::ui::{Pane, UiModel};

use crate::group::Place;
use crate::screen::Window;

/// Where the board is (the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub name: &'static str,
    pub cursor: usize,
    pub top: usize,
    pub zoom: Option<u8>,
}

/// The settings behind the house key (1532, 1561): the cursor is the
/// row among those a reader can change (`house::Overview::at`).
pub fn house(cursor: usize) -> Page {
    Page {
        name: "house",
        cursor,
        top: 0,
        zoom: None,
    }
}

/// The page `model` shows with the M9's `row` over a group's rows, the
/// pairing screen over it when `pairing`, as the last draw's `window`
/// stood.
pub fn of<B: InputBuffer>(model: &UiModel<B>, row: usize, pairing: bool, window: &Window) -> Page {
    let page = |name, cursor, top| Page {
        name,
        cursor,
        top,
        zoom: None,
    };
    if pairing {
        return page("pair", 0, 0);
    }
    if crate::map::shown(model) {
        return Page {
            zoom: model.map.as_ref().map(|state| state.zoom),
            ..page("map", 0, 0)
        };
    }
    let chat_top = window
        .chat_top
        .and_then(|key| {
            model
                .conversations
                .get(model.selected)?
                .messages
                .iter()
                .position(|m| m.activity == key)
        })
        .unwrap_or(0);
    match Place::of(model) {
        Place::List => {
            let (cursor, top) = window.list.unwrap_or((0, 0));
            page("chats", cursor, top)
        }
        Place::Chat => page("chat", model.message_cursor, chat_top),
        Place::Sheet => page("members", row, 0),
        Place::Ask => page("ask", row, 0),
        Place::Pick => page("pick", row, 0),
        Place::Name => page("name", row, 0),
        Place::Other => match model.focus {
            Pane::Compose => page("compose", model.compose.cursor(), chat_top),
            Pane::Search => page("search", 0, 0),
            _ => page("other", 0, 0),
        },
    }
}

impl fmt::Display for Page {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "name={} cursor={} top={}",
            self.name, self.cursor, self.top
        )?;
        if let Some(zoom) = self.zoom {
            write!(f, " zoom={zoom}")?;
        }
        Ok(())
    }
}

/// What the board remembers of the last `[PAGE]` line it said.
#[derive(Clone, Copy, Debug, Default)]
pub struct Said {
    last: Option<Page>,
}

impl Said {
    /// The line's body when `page` differs from the last one said.
    pub fn say(&mut self, page: Page) -> Option<String> {
        if self.last == Some(page) {
            return None;
        }
        self.last = Some(page);
        Some(alloc::format!("{page}"))
    }
}
