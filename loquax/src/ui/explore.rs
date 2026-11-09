//! Explore (1578, #232): one tab for the NomadNet pages and the public
//! rooms. Lew, 2026-11-08: three buttons, "Chats", "Map", "Explore", and
//! no fourth; the pages and the rooms live under Explore.
//!
//! **The pages** are the union of three lists, one row per destination:
//! the nodes the home heard and handed over (1579, the main source, in
//! the hosted browser's `node_registry`), the nodes this device heard
//! itself (the same registry), and the identity's bookmarks. The newest
//! name wins (the registry keeps the latest announce's), the row says
//! `name ·hash4`, when it was last seen, its hops when known, and a mark
//! when it is bookmarked. The order is bookmarks first, then last seen
//! first, grouped "Today", "This week", "Older" (Lew, 2026-11-09: an
//! announce list is a liveness list; a newcomer knows no names, so A-Z
//! gives no orientation). A-Z is a toggle, kept per device. The search
//! field filters by name and short hash (1227: a name is a search, never
//! a lookup) and takes a pasted address or `nomadnet://` form, the
//! secondary way in. Opening a page is lnomad's fetch and render in the
//! browser the tab hosts ([`super::browse`]).
//!
//! **The public rooms** are the rooms the reader is in, the hubs Loquax
//! ships (1577), the hubs heard on the mesh (1599, "Hubs in your mesh")
//! and the reader's own hub. A hub's row chooses it and shows its rooms,
//! where a room's name joins one; the hub link opens on the first join,
//! never on opening the tab (the privacy sentence of 1577).
//!
//! The tab never disappears: with nothing reachable each section says so
//! in one sentence.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_ui::input::InputBuffer;
use leviculum_ui::toast::ToastKind;
use serde_json::{json, Value};

use super::rooms::{HubChoice, RoomRow, RoomsEvent};
use super::scene::TabId;
use super::strings::{Arg, StrId, Text};
use super::{UiEffect, UiModel};

/// The tab's own state; the pages are the hosted browser's, the rooms the
/// Rooms'.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Explore {
    /// The tab is in front: its list, or the browser or a hub's rooms
    /// opened from it, which give it back when they close.
    pub open: bool,
    /// What the search field holds.
    pub query: String,
    /// A-Z instead of last seen (the toggle, per device: the config's
    /// `explore_az`).
    pub az: bool,
    /// The terminal's cursor over the tab's rows ([`picks`]).
    pub cursor: usize,
}

impl Explore {
    /// The tab's list is what is in front: not the browser or a hub's
    /// rooms opened from it.
    pub fn showing<B>(model: &UiModel<B>) -> bool {
        model.explore.open && !model.browse.open && !model.rooms.open
    }
}

/// What the tab is told: the reader's verbs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExploreEvent {
    /// The search field holds this now.
    Typed(String),
    /// A-Z (`true`) or last seen first.
    Az(bool),
    /// A page's row: its node's start page, or the bookmarked page.
    OpenPage([u8; 16]),
    /// The address the search field holds, opened.
    OpenAddress,
    /// A long press or the menu on a page's row: bookmarked, or not.
    Bookmark([u8; 16]),
    /// A room the reader is in, opened (an index into the Rooms' list).
    OpenRoom(usize),
    /// A hub's row: it is the public rooms' hub from now on, and its rooms
    /// show. Nothing is linked until a room is joined.
    Hub([u8; 16]),
}

/// A page's row as the tab draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRow {
    pub dest: [u8; 16],
    /// `name ·hash4`, `·hash4` for a node that gave no name.
    pub label: String,
    /// "seen 2 h ago"; none for a bookmark never heard.
    pub seen: Option<Text>,
    pub hops: Option<Text>,
    pub bookmarked: bool,
    /// What a tap opens: the bookmarked page, else the node's start page.
    pub url: String,
    /// The terminal's cursor is on it.
    pub cursor: bool,
}

/// A heading and its pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageGroup {
    pub heading: Text,
    pub rows: Vec<PageRow>,
}

/// A hub's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubRow {
    pub hash: [u8; 16],
    /// Its name; "Unnamed hub" for a nameless announce, never a hash.
    pub name: Text,
    /// How far, "suggested", "no answer".
    pub value: Option<Text>,
    /// The public rooms' hub in force.
    pub chosen: bool,
    pub cursor: bool,
}

/// The pages' section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PagesNode {
    pub title: Text,
    /// The field: what it holds and its hint.
    pub query: String,
    pub placeholder: Text,
    /// The order's toggle: its words, and whether A-Z is on.
    pub sort: Text,
    pub az: bool,
    /// "Open" and the address, while the field holds an address.
    pub address: Option<(String, bool)>,
    pub groups: Vec<PageGroup>,
    /// The one sentence where there is nothing to list.
    pub empty: Option<Text>,
    /// Opening a page is off on this device: the one sentence that says so.
    pub off: Option<Text>,
}

/// The public rooms' section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomsSection {
    pub title: Text,
    /// The rooms the reader is in.
    pub joined: Vec<RoomRow>,
    /// Where the terminal's cursor is among them.
    pub joined_cursor: Option<usize>,
    /// The hubs Loquax ships and the reader's own hub.
    pub hubs: Vec<HubRow>,
    /// "Hubs in your mesh": heard announcing (1599), not shipped.
    pub heard_title: Text,
    pub heard: Vec<HubRow>,
    pub empty: Option<Text>,
}

/// The tab as the frontends draw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExploreNode {
    pub pages: PagesNode,
    pub rooms: RoomsSection,
}

/// One of the tab's rows, in the order the terminal's cursor walks them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    Address,
    Page([u8; 16]),
    Room(usize),
    Hub([u8; 16]),
}

/// A page known to the device, before it is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub dest: [u8; 16],
    pub name: Option<String>,
    /// Unix seconds; 0 for a bookmark never heard.
    pub last_seen: u64,
    pub hops: Option<u32>,
    /// The first bookmark on it, by its url.
    pub bookmark: Option<String>,
}

/// When a page was last seen, against now: the three headings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Age {
    Today,
    ThisWeek,
    Older,
}

/// The age of `seen` at `now`, both unix seconds, in the local day of
/// `offset` seconds east of UTC: today is the same calendar day, this week
/// the seven days before.
pub fn age(now: i64, seen: i64, offset: i32) -> Age {
    let day = |t: i64| (t + i64::from(offset)).div_euclid(86_400);
    if day(seen) >= day(now) {
        Age::Today
    } else if now.saturating_sub(seen) < 7 * 86_400 {
        Age::ThisWeek
    } else {
        Age::Older
    }
}

/// The destination a bookmark's url names, if it names one.
fn bookmark_dest(url: &str) -> Option<[u8; 16]> {
    lnomad::parse_url(url, None).ok().map(|t| t.dest_hash)
}

/// The pages the device knows: the registry's nodes (the home's and its
/// own), and the bookmarks on nodes it has not heard, one per destination.
pub fn known(model: &lnomad::Model) -> Vec<Known> {
    let mut out: Vec<Known> = model
        .node_registry
        .nodes()
        .into_iter()
        .map(|n| Known {
            dest: n.dest_hash,
            name: n.name.clone(),
            last_seen: n.last_seen,
            hops: n.hops,
            bookmark: None,
        })
        .collect();
    for bookmark in model.bookmarks.list() {
        let Some(dest) = bookmark_dest(&bookmark.url) else {
            continue;
        };
        match out.iter_mut().find(|k| k.dest == dest) {
            Some(known) => {
                if known.bookmark.is_none() {
                    known.bookmark = Some(bookmark.url.clone());
                }
                if known.name.is_none() && !bookmark.title.trim().is_empty() {
                    known.name = Some(bookmark.title.clone());
                }
            }
            None => out.push(Known {
                dest,
                name: Some(bookmark.title.clone()).filter(|t| !t.trim().is_empty()),
                last_seen: 0,
                hops: None,
                bookmark: Some(bookmark.url.clone()),
            }),
        }
    }
    out
}

/// Whether `known` matches the search: its label, case folded, holds the
/// query, or its hash starts with it (a short hash with or without `·`).
pub fn matches(known: &Known, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let label = lnomad::names::site_label(known.name.as_deref(), &known.dest).to_lowercase();
    let hex = crate::address::to_hex(&known.dest);
    let bare = query.trim_start_matches('·');
    label.contains(&query) || (!bare.is_empty() && hex.starts_with(bare))
}

/// The pages in the tab's order: bookmarks first, then by last seen, the
/// newest first, or by name with `az`; a tie by hash, so the order is
/// stable.
pub fn ordered(mut pages: Vec<Known>, az: bool) -> Vec<Known> {
    let name = |k: &Known| k.name.as_deref().unwrap_or("").to_lowercase();
    pages.sort_by(|a, b| {
        b.bookmark
            .is_some()
            .cmp(&a.bookmark.is_some())
            .then_with(|| match az {
                true => name(a).cmp(&name(b)),
                false => b.last_seen.cmp(&a.last_seen),
            })
            .then_with(|| a.dest.cmp(&b.dest))
    });
    pages
}

/// The address the search field holds, as the browser opens it: a
/// destination hash with or without a path, or the `nomadnet://` form.
pub fn address(query: &str) -> Option<String> {
    let query = query.trim();
    let bare = query.strip_prefix("nomadnet://").unwrap_or(query);
    let bare = bare.trim_start_matches('/');
    lnomad::parse_url(bare, None).ok()?;
    Some(bare.to_string())
}

/// "seen just now", "seen 2 h ago".
fn seen(now: u64, then: u64) -> Option<Text> {
    if then == 0 {
        return None;
    }
    let secs = now.saturating_sub(then);
    let n = |id: StrId, unit: u64| {
        Text::Key(
            id,
            Vec::from([Arg::Int(i64::try_from(secs / unit).unwrap_or(i64::MAX))]),
        )
    };
    Some(match secs {
        0..=59 => Text::key(StrId::ExploreSeenNow),
        60..=3599 => n(StrId::ExploreSeenMinutes, 60),
        3600..=86_399 => n(StrId::ExploreSeenHours, 3600),
        86_400..=172_799 => Text::key(StrId::ExploreSeenDay),
        _ => n(StrId::ExploreSeenDays, 86_400),
    })
}

fn hops(hops: Option<u32>) -> Option<Text> {
    match hops? {
        1 => Some(Text::key(StrId::ExploreHop)),
        n => Some(Text::Key(
            StrId::ExploreHops,
            Vec::from([Arg::Int(i64::from(n))]),
        )),
    }
}

/// Now, in unix seconds and the local offset: the model's clock, else the
/// browser's.
fn now<B>(model: &UiModel<B>) -> (u64, i32) {
    match model.now {
        Some(now) => (u64::try_from(now.unix_secs).unwrap_or(0), now.offset_secs),
        None => (model.browse.model.wall_now(), 0),
    }
}

/// The tab's rows in the terminal cursor's order: the address, the
/// pages, the rooms the reader is in, the hubs and the heard hubs.
pub fn picks<B>(model: &UiModel<B>) -> Vec<Pick> {
    let mut out = Vec::new();
    if address(&model.explore.query).is_some() {
        out.push(Pick::Address);
    }
    out.extend(
        ordered(
            known(&model.browse.model)
                .into_iter()
                .filter(|k| matches(k, &model.explore.query))
                .collect(),
            model.explore.az,
        )
        .into_iter()
        .map(|k| Pick::Page(k.dest)),
    );
    out.extend((0..model.rooms.rooms.len()).map(Pick::Room));
    let (hubs, heard) = hub_lists(model);
    out.extend(hubs.iter().chain(heard.iter()).map(|h| Pick::Hub(h.hash)));
    out
}

/// The hubs the section lists: the first [`super::settings::HUB_PAGE`]
/// shipped ones in the settings' order (heard first, then the closest),
/// the one in force always among them, and the reader's own; then the
/// ones heard on the mesh and not shipped. The settings list every
/// shipped hub; Explore is a list to pick from, not the whole catalogue.
fn hub_lists<B>(
    model: &UiModel<B>,
) -> (Vec<super::settings::Offered>, Vec<super::settings::Offered>) {
    let offered = super::settings::offered_hubs(&model.rooms);
    let (shipped, heard): (Vec<_>, Vec<_>) = offered.into_iter().partition(|o| o.shipped);
    let chosen = model.rooms.hub;
    let mut hubs: Vec<_> = shipped
        .iter()
        .take(super::settings::HUB_PAGE)
        .cloned()
        .collect();
    if let Some(hub) = shipped.into_iter().find(|o| Some(o.hash) == chosen) {
        if !hubs.iter().any(|o| o.hash == hub.hash) {
            hubs.push(hub);
        }
    }
    if let Some(own) = model.rooms.hub {
        if !hubs.iter().chain(heard.iter()).any(|o| o.hash == own) {
            hubs.push(super::settings::Offered {
                hash: own,
                name: None,
                hops: None,
                shipped: false,
                heard: false,
                answered: None,
            });
        }
    }
    (hubs, heard)
}

fn hub_row<B>(model: &UiModel<B>, hub: &super::settings::Offered, cursor: Option<&Pick>) -> HubRow {
    let chosen = model.rooms.hub == Some(hub.hash);
    let name = match &hub.name {
        Some(name) => Text::Raw(name.clone()),
        None if !hub.shipped && !hub.heard => super::rooms::hub_label(&model.rooms),
        None => Text::key(StrId::RoomsHubUnnamed),
    };
    let mut parts = Vec::new();
    match hub.hops {
        Some(1) => parts.push(Text::key(StrId::RoomsHubHop)),
        Some(n) => parts.push(Text::Key(
            StrId::RoomsHubHops,
            Vec::from([Arg::Int(i64::from(n))]),
        )),
        None => {}
    }
    if hub.shipped && hub.heard {
        parts.push(Text::key(StrId::RoomsHubSuggested));
    }
    if hub.answered == Some(false) {
        parts.push(Text::key(StrId::RoomsHubNoAnswer));
    }
    let value = match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Text::Join(parts)),
    };
    HubRow {
        hash: hub.hash,
        name,
        value,
        chosen,
        cursor: cursor == Some(&Pick::Hub(hub.hash)),
    }
}

/// The tab for the frontends.
pub fn node<B>(model: &UiModel<B>) -> ExploreNode {
    let explore = &model.explore;
    let picks = picks(model);
    let cursor = picks.get(explore.cursor.min(picks.len().saturating_sub(1)));
    let (now_secs, offset) = now(model);
    let all = known(&model.browse.model);
    let any = !all.is_empty();
    let pages = ordered(
        all.into_iter()
            .filter(|k| matches(k, &explore.query))
            .collect(),
        explore.az,
    );
    let mut groups: Vec<PageGroup> = Vec::new();
    for page in pages {
        let heading = match (&page.bookmark, explore.az) {
            (Some(_), _) => StrId::ExploreBookmarks,
            (None, true) => StrId::ExploreAz,
            (None, false) => match age(
                i64::try_from(now_secs).unwrap_or(i64::MAX),
                i64::try_from(page.last_seen).unwrap_or(0),
                offset,
            ) {
                Age::Today => StrId::ExploreToday,
                Age::ThisWeek => StrId::ExploreThisWeek,
                Age::Older => StrId::ExploreOlder,
            },
        };
        let row = PageRow {
            dest: page.dest,
            label: lnomad::names::site_label(page.name.as_deref(), &page.dest),
            seen: seen(now_secs, page.last_seen),
            hops: hops(page.hops),
            bookmarked: page.bookmark.is_some(),
            url: page.bookmark.clone().unwrap_or_else(|| {
                format!("{}:/page/index.mu", crate::address::to_hex(&page.dest))
            }),
            cursor: cursor == Some(&Pick::Page(page.dest)),
        };
        match groups.last_mut() {
            Some(group) if group.heading == Text::key(heading) => group.rows.push(row),
            _ => groups.push(PageGroup {
                heading: Text::key(heading),
                rows: Vec::from([row]),
            }),
        }
    }
    let typed = address(&explore.query);
    let empty = match (groups.is_empty(), typed.is_some(), any) {
        (false, _, _) | (true, true, _) => None,
        (true, false, true) => Some(Text::Key(
            StrId::ExploreNoMatch,
            Vec::from([Arg::Str(explore.query.trim().to_string())]),
        )),
        (true, false, false) => Some(Text::key(StrId::ExploreNoPages)),
    };
    let pages = PagesNode {
        title: Text::key(StrId::ExplorePages),
        query: explore.query.clone(),
        placeholder: Text::key(StrId::ExploreSearchHint),
        sort: Text::key(match explore.az {
            true => StrId::ExploreSortAz,
            false => StrId::ExploreSortSeen,
        }),
        az: explore.az,
        address: typed.map(|url| (url, cursor == Some(&Pick::Address))),
        groups,
        empty,
        off: (!model.browse.enabled).then(|| Text::key(StrId::ExplorePagesOff)),
    };
    let rooms_node = super::rooms::node(model);
    let (hubs, heard) = hub_lists(model);
    let hubs: Vec<HubRow> = hubs.iter().map(|h| hub_row(model, h, cursor)).collect();
    let heard: Vec<HubRow> = heard.iter().map(|h| hub_row(model, h, cursor)).collect();
    let joined_cursor = match cursor {
        Some(Pick::Room(i)) => Some(*i),
        _ => None,
    };
    let empty = (rooms_node.rows.is_empty() && hubs.is_empty() && heard.is_empty())
        .then(|| Text::key(StrId::ExploreNoRooms));
    ExploreNode {
        pages,
        rooms: RoomsSection {
            title: Text::key(StrId::ExploreRooms),
            joined: rooms_node.rows,
            joined_cursor,
            hubs,
            heard_title: Text::key(StrId::ExploreHeardHubs),
            heard,
            empty,
        },
    }
}

/// A tab chosen: Explore comes to the front with its list, anything it
/// opened closes; another tab sends it back.
pub(super) fn tab<B>(model: &mut UiModel<B>, tab: TabId) -> Vec<UiEffect> {
    match tab {
        TabId::Explore => {
            model.explore.open = true;
            model.browse.open = false;
            model.rooms.open = false;
            model.explore.cursor = 0;
        }
        TabId::Browse | TabId::Rooms => model.explore.open = true,
        TabId::Chats | TabId::Map => model.explore.open = false,
    }
    Vec::new()
}

/// Fold one of the tab's events.
pub(super) fn fold<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    event: ExploreEvent,
) -> Vec<UiEffect> {
    match event {
        ExploreEvent::Typed(text) => {
            model.explore.query = text;
            model.explore.cursor = 0;
            Vec::new()
        }
        ExploreEvent::Az(on) => {
            model.explore.az = on;
            Vec::from([UiEffect::Remember {
                key: "explore_az",
                value: if on { "on" } else { "off" },
            }])
        }
        ExploreEvent::OpenPage(dest) => {
            let Some(page) = known(&model.browse.model)
                .into_iter()
                .find(|k| k.dest == dest)
            else {
                return Vec::new();
            };
            let url = page
                .bookmark
                .unwrap_or_else(|| format!("{}:/page/index.mu", crate::address::to_hex(&dest)));
            open_url(model, url)
        }
        ExploreEvent::OpenAddress => match address(&model.explore.query) {
            Some(url) => open_url(model, url),
            None => Vec::new(),
        },
        ExploreEvent::Bookmark(dest) => bookmark(model, dest),
        ExploreEvent::OpenRoom(index) => {
            if index >= model.rooms.rooms.len() {
                return Vec::new();
            }
            show_rooms(model);
            super::rooms::fold(model, RoomsEvent::Show(Some(index)))
        }
        ExploreEvent::Hub(hash) => {
            let mut effects = super::rooms::choose(model, HubChoice::Hub(hash));
            show_rooms(model);
            effects.extend(super::rooms::fold(model, RoomsEvent::Show(None)));
            effects
        }
    }
}

/// A hub's rooms in front, from the tab: no link yet.
fn show_rooms<B>(model: &mut UiModel<B>) {
    model.explore.open = true;
    model.browse.open = false;
    model.rooms.open = true;
}

/// Open `url` in the hosted browser, or say in one sentence that opening
/// pages is off here.
fn open_url<B: InputBuffer + Default>(model: &mut UiModel<B>, url: String) -> Vec<UiEffect> {
    if !model.browse.enabled {
        let text = Text::key(StrId::ExplorePagesOff).resolve(model.lang);
        model.toast(ToastKind::Info, text);
        return Vec::new();
    }
    model.explore.open = true;
    model.rooms.open = false;
    super::browse::open_page(model, url)
}

/// A page's row bookmarked, or its bookmark gone: client state the home
/// keeps for every device of the identity (1579).
fn bookmark<B: InputBuffer + Default>(model: &mut UiModel<B>, dest: [u8; 16]) -> Vec<UiEffect> {
    let Some(page) = known(&model.browse.model)
        .into_iter()
        .find(|k| k.dest == dest)
    else {
        return Vec::new();
    };
    let marks = &mut model.browse.model.bookmarks;
    let removed: Vec<String> = marks
        .list()
        .iter()
        .filter(|b| bookmark_dest(&b.url) == Some(dest))
        .map(|b| b.url.clone())
        .collect();
    if removed.is_empty() {
        marks.add(lnomad::Bookmark {
            url: format!("{}:/page/index.mu", crate::address::to_hex(&dest)),
            title: page.name.unwrap_or_default(),
        });
    } else {
        for url in removed {
            marks.remove(&url);
        }
    }
    super::browse::effects_of(model, Vec::from([lnomad::Effect::SaveBookmarks]))
}

/// A key while the tab's list is in front (the terminal; a phone and a
/// window have their own field and say [`ExploreEvent::Typed`]): letters
/// go to the search field, `Backspace` takes one back, `Up` and `Down`
/// move the cursor over the rows, `Enter` opens the row under it, `Alt-b`
/// bookmarks a page's row, `Alt-s` turns the order, `Esc` clears the
/// field and, empty, leaves the tab for the chats, as `Ctrl-g` does.
pub(super) fn key<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    key: leviculum_ui::input::KeyEvent,
) -> Vec<UiEffect> {
    use leviculum_ui::input::{KeyCode, KeyModifiers};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let picks = picks(model);
    let under = picks
        .get(model.explore.cursor.min(picks.len().saturating_sub(1)))
        .cloned();
    match key.code {
        KeyCode::Char('g') if ctrl => model.explore.open = false,
        KeyCode::Char('s') if alt => {
            let az = !model.explore.az;
            return fold(model, ExploreEvent::Az(az));
        }
        KeyCode::Char('b') if alt => {
            if let Some(Pick::Page(dest)) = under {
                return fold(model, ExploreEvent::Bookmark(dest));
            }
        }
        KeyCode::Char(c) if !ctrl && !alt => {
            model.explore.query.push(c);
            model.explore.cursor = 0;
        }
        KeyCode::Backspace => {
            model.explore.query.pop();
            model.explore.cursor = 0;
        }
        KeyCode::Up => model.explore.cursor = model.explore.cursor.saturating_sub(1),
        KeyCode::Down if !picks.is_empty() => {
            model.explore.cursor = (model.explore.cursor + 1).min(picks.len() - 1);
        }
        KeyCode::Enter => {
            return match under {
                Some(Pick::Address) => fold(model, ExploreEvent::OpenAddress),
                Some(Pick::Page(dest)) => fold(model, ExploreEvent::OpenPage(dest)),
                Some(Pick::Room(i)) => fold(model, ExploreEvent::OpenRoom(i)),
                Some(Pick::Hub(hash)) => fold(model, ExploreEvent::Hub(hash)),
                None => Vec::new(),
            };
        }
        KeyCode::Esc if !model.explore.query.is_empty() => {
            model.explore.query.clear();
            model.explore.cursor = 0;
        }
        KeyCode::Esc => model.explore.open = false,
        _ => {}
    }
    Vec::new()
}

fn page_json(row: &PageRow) -> Value {
    json!({
        "dest": crate::address::to_hex(&row.dest),
        "label": row.label,
        "seen": row.seen.as_ref().map(Text::to_json),
        "hops": row.hops.as_ref().map(Text::to_json),
        "bookmarked": row.bookmarked,
        "url": row.url,
        "cursor": row.cursor,
    })
}

fn hub_json(row: &HubRow) -> Value {
    json!({
        "hash": crate::address::to_hex(&row.hash),
        "name": row.name.to_json(),
        "value": row.value.as_ref().map(Text::to_json),
        "chosen": row.chosen,
        "cursor": row.cursor,
    })
}

impl ExploreNode {
    /// The tab as JSON for golden files, its texts unresolved.
    pub fn to_json(&self) -> Value {
        let p = &self.pages;
        let r = &self.rooms;
        json!({
            "pages": {
                "title": p.title.to_json(),
                "query": p.query,
                "placeholder": p.placeholder.to_json(),
                "sort": p.sort.to_json(),
                "az": p.az,
                "address": p.address.as_ref().map(|(url, cursor)| json!({"url": url, "cursor": cursor})),
                "groups": p.groups.iter().map(|g| json!({
                    "heading": g.heading.to_json(),
                    "rows": g.rows.iter().map(page_json).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "empty": p.empty.as_ref().map(Text::to_json),
                "off": p.off.as_ref().map(Text::to_json),
            },
            "rooms": {
                "title": r.title.to_json(),
                "joined": r.joined.iter().map(|row| json!({
                    "name": row.name,
                    "unread": row.unread,
                    "public": row.public.to_json(),
                })).collect::<Vec<_>>(),
                "joined_cursor": r.joined_cursor,
                "hubs": r.hubs.iter().map(hub_json).collect::<Vec<_>>(),
                "heard_title": r.heard_title.to_json(),
                "heard": r.heard.iter().map(hub_json).collect::<Vec<_>>(),
                "empty": r.empty.as_ref().map(Text::to_json),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::rooms::{HubState, RoomAsk};
    use crate::ui::scene::{LayoutClass, ScreenId};
    use crate::ui::tests::Field as Buffer;
    use crate::ui::{update, UiEvent};
    use crate::when::WallTime;
    use alloc::vec;
    use lnomad::DiscoveredNode;

    /// 2027-01-15 12:00 UTC, a Friday.
    const NOW: i64 = 1_800_000_000;

    fn heard(byte: u8, name: Option<&str>, seen_ago: i64, hops: Option<u32>) -> DiscoveredNode {
        let at = u64::try_from(NOW - seen_ago).expect("positive");
        DiscoveredNode {
            dest_hash: [byte; 16],
            name: name.map(ToString::to_string),
            first_seen: at,
            last_seen: at,
            hops,
        }
    }

    /// A phone with the browser, its clock at [`NOW`] in UTC, three nodes
    /// heard: one an hour ago, one three days ago, one a month ago.
    fn phone() -> UiModel<Buffer> {
        let mut model = UiModel::<Buffer>::new(&[]);
        model.browse.enabled = true;
        model.now = Some(WallTime {
            unix_secs: NOW,
            offset_secs: 0,
        });
        for n in [
            heard(0x11, Some("Zebra Hof"), 3_600, Some(2)),
            heard(0x22, Some("Alpha Relay"), 3 * 86_400, Some(1)),
            heard(0x33, None, 30 * 86_400, None),
        ] {
            update(
                &mut model,
                UiEvent::Browse(crate::ui::browse::BrowseEvent::Node(n)),
            );
        }
        model
    }

    fn explore(model: &mut UiModel<Buffer>, event: ExploreEvent) -> Vec<UiEffect> {
        update(model, UiEvent::Explore(event))
    }

    fn headings(node: &ExploreNode) -> Vec<StrId> {
        node.pages
            .groups
            .iter()
            .map(|g| match &g.heading {
                Text::Key(id, _) => *id,
                other => panic!("a heading from the catalogue: {other:?}"),
            })
            .collect()
    }

    fn labels(node: &ExploreNode) -> Vec<String> {
        node.pages
            .groups
            .iter()
            .flat_map(|g| g.rows.iter().map(|r| r.label.clone()))
            .collect()
    }

    /// The default order: bookmarks first, then last seen first, under
    /// "Today", "This week", "Older"; A-Z the toggle, which the config
    /// keeps.
    #[test]
    fn bookmarks_first_then_last_seen_first_in_three_ages_and_a_z_is_the_toggle() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let shown = node(&model);
        assert_eq!(
            labels(&shown),
            ["Zebra Hof ·1111", "Alpha Relay ·2222", "·3333"]
        );
        assert_eq!(
            headings(&shown),
            [
                StrId::ExploreToday,
                StrId::ExploreThisWeek,
                StrId::ExploreOlder
            ]
        );
        let first = &shown.pages.groups[0].rows[0];
        assert_eq!(
            first.seen,
            Some(Text::Key(StrId::ExploreSeenHours, vec![Arg::Int(1)]))
        );
        assert_eq!(
            first.seen.as_ref().map(|t| t.resolve(Lang::De)).as_deref(),
            Some("zuletzt gesehen vor 1 h")
        );
        assert_eq!(
            first.hops,
            Some(Text::Key(StrId::ExploreHops, vec![Arg::Int(2)]))
        );
        // A bookmark on the oldest puts it on top, under its own heading.
        explore(&mut model, ExploreEvent::Bookmark([0x33; 16]));
        let shown = node(&model);
        assert_eq!(labels(&shown)[0], "·3333");
        assert_eq!(headings(&shown)[0], StrId::ExploreBookmarks);
        assert!(shown.pages.groups[0].rows[0].bookmarked);
        // A-Z: the bookmark stays on top, the rest by name.
        let effects = explore(&mut model, ExploreEvent::Az(true));
        assert_eq!(
            effects,
            [UiEffect::Remember {
                key: "explore_az",
                value: "on"
            }]
        );
        let shown = node(&model);
        assert_eq!(
            labels(&shown),
            ["·3333", "Alpha Relay ·2222", "Zebra Hof ·1111"]
        );
        assert_eq!(
            headings(&shown),
            [StrId::ExploreBookmarks, StrId::ExploreAz]
        );
        assert_eq!(shown.pages.sort, Text::key(StrId::ExploreSortAz));
    }

    use crate::ui::strings::Lang;

    /// "Today" is the local calendar day, not the last 24 hours: a page
    /// seen at 23:00 is yesterday's at 01:00, in the reader's zone.
    #[test]
    fn today_is_the_local_day() {
        let midnight = 1_800_057_600; // 2027-01-16 00:00 UTC
        assert_eq!(age(midnight + 3_600, midnight - 3_600, 0), Age::ThisWeek);
        assert_eq!(age(midnight + 3_600, midnight + 60, 0), Age::Today);
        // Two hours east, the same instants are both on the 16th.
        assert_eq!(age(midnight + 3_600, midnight - 3_600, 7_200), Age::Today);
        assert_eq!(age(NOW, NOW - 8 * 86_400, 0), Age::Older);
    }

    /// One row per destination: the home's row and the device's own merge
    /// (the registry's), a bookmark on a node heard is that node's row, a
    /// bookmark on one never heard is a row of its own with no age.
    #[test]
    fn one_row_per_destination_from_the_home_the_device_and_the_bookmarks() {
        use crate::home::pages::{Delta, PageRow as HomePage, Row};
        let mut model = phone();
        update(
            &mut model,
            UiEvent::Browse(crate::ui::browse::BrowseEvent::Home(Delta {
                stamp: 4,
                more: false,
                rows: vec![
                    // The home heard Zebra Hof later, under a new name.
                    Row::Page(HomePage {
                        node: heard(0x11, Some("Zebra Hof Neu"), 60, Some(3)),
                        count: 9,
                    }),
                    Row::Page(HomePage {
                        node: heard(0x44, Some("Bergwacht"), 7_200, None),
                        count: 1,
                    }),
                ],
            })),
        );
        model.browse.model.bookmarks.add(lnomad::Bookmark {
            url: format!("{}:/page/news.mu", "22".repeat(16)),
            title: "Alpha News".into(),
        });
        model.browse.model.bookmarks.add(lnomad::Bookmark {
            url: format!("{}:/page/index.mu", "55".repeat(16)),
            title: "Never heard".into(),
        });
        let shown = node(&model);
        let all: Vec<&PageRow> = shown
            .pages
            .groups
            .iter()
            .flat_map(|g| g.rows.iter())
            .collect();
        assert_eq!(all.len(), 5, "{:?}", labels(&shown));
        let alpha = all.iter().find(|r| r.dest == [0x22; 16]).expect("alpha");
        assert!(alpha.bookmarked);
        assert_eq!(alpha.label, "Alpha Relay ·2222", "the announced name wins");
        assert!(
            alpha.url.ends_with(":/page/news.mu"),
            "the bookmarked page opens"
        );
        let zebra = all.iter().find(|r| r.dest == [0x11; 16]).expect("zebra");
        assert_eq!(zebra.label, "Zebra Hof Neu ·1111", "the newest name wins");
        let never = all.iter().find(|r| r.dest == [0x55; 16]).expect("bookmark");
        assert_eq!(never.seen, None);
        assert_eq!(never.label, "Never heard ·5555");
    }

    /// The field filters by name and short hash, and takes an address or
    /// its `nomadnet://` form, which opens through the browser.
    #[test]
    fn the_field_filters_by_name_and_hash_and_opens_an_address() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        explore(&mut model, ExploreEvent::Typed("alpha".into()));
        assert_eq!(labels(&node(&model)), ["Alpha Relay ·2222"]);
        explore(&mut model, ExploreEvent::Typed("·33".into()));
        assert_eq!(labels(&node(&model)), ["·3333"]);
        explore(&mut model, ExploreEvent::Typed("nobody".into()));
        let shown = node(&model);
        assert!(labels(&shown).is_empty());
        assert_eq!(
            shown.pages.empty,
            Some(Text::Key(
                StrId::ExploreNoMatch,
                vec![Arg::Str("nobody".into())]
            ))
        );
        let hex = "7c".repeat(16);
        explore(
            &mut model,
            ExploreEvent::Typed(format!("nomadnet://{hex}:/page/x.mu")),
        );
        let shown = node(&model);
        assert_eq!(
            shown.pages.address.as_ref().map(|(url, _)| url.clone()),
            Some(format!("{hex}:/page/x.mu"))
        );
        assert_eq!(shown.pages.empty, None, "the address is what there is");
        let effects = explore(&mut model, ExploreEvent::OpenAddress);
        assert_eq!(
            effects,
            [UiEffect::Browse(lnomad::Effect::Navigate(lnomad::Target {
                dest_hash: [0x7c; 16],
                path: "/page/x.mu".into(),
                fields: Vec::new(),
                is_file: false,
            }))]
        );
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Browse);
        // The control: a name is no address.
        assert_eq!(address("Alpha Relay"), None);
    }

    /// A row opens its node's start page in the browser, Explore stays the
    /// active tab, and the browser's quit comes back to the list.
    #[test]
    fn a_row_opens_the_page_and_the_browser_comes_back_to_the_list() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Explore);
        let effects = explore(&mut model, ExploreEvent::OpenPage([0x22; 16]));
        assert_eq!(effects.len(), 1, "{effects:?}");
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(scene.screen, ScreenId::Browse);
        assert_eq!(scene.tabs.expect("tabs").active, TabId::Explore);
        update(
            &mut model,
            UiEvent::Browse(crate::ui::browse::BrowseEvent::Intent(lnomad::Intent::Quit)),
        );
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Explore);
        update(&mut model, UiEvent::Tab(TabId::Chats));
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Chats);
    }

    /// A host that cannot open pages lists them all the same and says so
    /// in one sentence; a tap says it again and opens nothing.
    #[test]
    fn without_the_browser_the_list_is_there_and_says_pages_cannot_open() {
        let mut model = phone();
        model.browse.enabled = false;
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let shown = node(&model);
        assert_eq!(labels(&shown).len(), 3);
        assert_eq!(shown.pages.off, Some(Text::key(StrId::ExplorePagesOff)));
        assert!(explore(&mut model, ExploreEvent::OpenPage([0x22; 16])).is_empty());
        assert!(model.current_toast().is_some());
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Explore);
    }

    /// The tab never disappears: a fresh model with nothing heard and no
    /// hub has it, and each section says so in one sentence or lists what
    /// ships.
    #[test]
    fn the_tab_is_always_there_and_an_empty_section_says_so() {
        let mut model = UiModel::<Buffer>::new(&[]);
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(scene.screen, ScreenId::Explore);
        let shown = scene.explore.expect("the list");
        assert_eq!(shown.pages.empty, Some(Text::key(StrId::ExploreNoPages)));
        // The shipped hubs are always offered; with none it says so.
        assert_eq!(
            shown.rooms.empty.is_some(),
            crate::ui::hubs::shipped().is_empty()
        );
        assert!(shown.rooms.heard.is_empty());
    }

    /// A hub's row chooses it and shows its rooms, and links nothing; the
    /// first join links (the privacy sentence of 1577) and the room joins
    /// on the hub's welcome. Heard hubs are listed under "Hubs in your
    /// mesh".
    #[test]
    fn a_hub_is_picked_without_a_link_and_the_first_join_links() {
        let mut model = phone();
        let hub = [0x99; 16];
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Heard(crate::ui::hubs::HeardHub {
                hash: hub,
                name: Some("Werkstatt".into()),
                hops: Some(2),
                first: NOW - 60,
                last: NOW - 60,
                answered: None,
            })),
        );
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let shown = node(&model);
        assert_eq!(shown.rooms.heard.len(), 1);
        assert_eq!(shown.rooms.heard[0].name, Text::Raw("Werkstatt".into()));
        assert_eq!(shown.rooms.heard_title, Text::key(StrId::ExploreHeardHubs));
        let effects = explore(&mut model, ExploreEvent::Hub(hub));
        assert!(
            effects
                .iter()
                .all(|e| !matches!(e, UiEffect::Rooms(RoomAsk::Open { .. }))),
            "no link on picking: {effects:?}"
        );
        assert_eq!(model.rooms.state, HubState::Off);
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(scene.screen, ScreenId::Rooms);
        assert_eq!(scene.tabs.expect("tabs").active, TabId::Explore);
        update(&mut model, UiEvent::Rooms(RoomsEvent::Typed("#lab".into())));
        let effects = update(&mut model, UiEvent::Rooms(RoomsEvent::Submit));
        assert!(
            effects.contains(&UiEffect::Rooms(RoomAsk::Open { hub })),
            "the first join links: {effects:?}"
        );
        // Esc on the rooms' list goes back to Explore, which opened them.
        update(
            &mut model,
            UiEvent::Key(leviculum_ui::input::KeyEvent::new(
                leviculum_ui::input::KeyCode::Esc,
                leviculum_ui::input::KeyModifiers::NONE,
            )),
        );
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Explore);
        let picked = node(&model);
        assert!(picked.rooms.heard[0].chosen);
    }

    /// The terminal: letters go to the field, the arrows move the cursor,
    /// Enter opens the row under it, Alt-b bookmarks, Esc clears the field
    /// and then leaves.
    #[test]
    fn the_terminals_keys_search_move_open_and_leave() {
        use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
        let key = |code| UiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE));
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        update(&mut model, key(KeyCode::Down));
        let shown = node(&model);
        assert!(shown.pages.groups[1].rows[0].cursor, "the second page");
        update(
            &mut model,
            UiEvent::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT)),
        );
        assert!(model
            .browse
            .model
            .bookmarks
            .contains(&format!("{}:/page/index.mu", "22".repeat(16))));
        for c in "zeb".chars() {
            update(&mut model, key(KeyCode::Char(c)));
        }
        assert_eq!(model.explore.query, "zeb");
        assert_eq!(labels(&node(&model)), ["Zebra Hof ·1111"]);
        let effects = update(&mut model, key(KeyCode::Enter));
        assert_eq!(effects.len(), 1, "the page is fetched: {effects:?}");
        assert!(model.browse.showing());
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        update(&mut model, key(KeyCode::Char('x')));
        update(&mut model, key(KeyCode::Esc));
        assert!(model.explore.query.is_empty() && model.explore.open);
        update(&mut model, key(KeyCode::Esc));
        assert!(!model.explore.open);
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Chats);
    }

    /// The device keeps as many nodes as its home remembers (1578): 4096
    /// on a host with `std`, the board's 500 without.
    #[test]
    fn the_browser_keeps_as_many_nodes_as_the_home() {
        let model = UiModel::<Buffer>::new(&[]);
        assert_eq!(model.browse.model.node_registry.capacity(), 4096);
        assert_eq!(crate::ui::browse::REGISTRY, crate::home::pages::PAGES_CAP);
    }
}
