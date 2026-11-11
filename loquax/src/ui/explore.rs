//! Explore (1578, #232): one tab for the NomadNet pages and the public
//! rooms. Lew, 2026-11-08: three buttons, "Chats", "Map", "Explore", and
//! no fourth; the pages and the rooms live under Explore.
//!
//! **The pages** are the union of three lists, one row per destination:
//! the nodes the home heard and handed over (1579, the main source, in
//! the hosted browser's `node_registry`), the nodes this device heard
//! itself (the same registry), and the identity's bookmarks. The newest
//! name wins (the registry keeps the latest announce's), the row says
//! the name alone (1618: `·hash4` only where two names look alike, as the
//! chat list since 1452; "Unnamed page" where a node gave none), when it
//! was last seen, how far when known, and a mark when it is bookmarked. The order is bookmarks first, then last seen
//! first, grouped "Today", "This week", "Older" (Lew, 2026-11-09: an
//! announce list is a liveness list; a newcomer knows no names, so A-Z
//! gives no orientation). A-Z is a toggle, kept per device. A new order
//! is applied at most once a second and never while a finger is on the
//! list (1618: the walk tapped one page and opened the next), so a row
//! never moves under a finger. The search field filters by name (1227: a
//! name is a search, never a lookup), never by a hash's letters (1618),
//! and takes a pasted address or `nomadnet://` form, the secondary way
//! in. Opening a page is lnomad's fetch and render in the
//! browser the tab hosts ([`super::browse`]).
//!
//! **The rooms** (1621: "Rooms", then "Servers") are the rooms the reader
//! is in, then in one list the hubs Loquax ships (1577), the hubs heard on
//! the mesh (1599) and the reader's own hub, each saying when it was seen,
//! "suggested" on one at most. The reader's word for a hub is "server"
//! (1618). A hub's row opens its sheet and chooses nothing: its name, how
//! far, the rooms it names (a tap joins one) and the one button that makes
//! it the public rooms' hub. The sheet asks the hub which rooms it names
//! on a question of its own ([`super::rooms::RoomAsk::Peek`]: a throwaway
//! identity, no nick, no join), kept for the run. The rooms' link opens on
//! the first join, never on opening the tab or the sheet (the privacy
//! sentence of 1577: nothing that names the reader goes out before).
//!
//! The tab never disappears: with nothing reachable each section says so
//! in one sentence.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_ui::input::InputBuffer;
use leviculum_ui::toast::ToastKind;
use serde_json::{json, Value};

use super::rooms::{HubChoice, Named, RoomRow, RoomsEvent};
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
    /// A hub's sheet is open over the list (1618).
    pub sheet: Option<[u8; 16]>,
    /// The terminal's cursor over the sheet's rows ([`sheet_picks`]).
    pub sheet_cursor: usize,
    /// The pages' order as last applied (1618): each page's destination
    /// and its heading. Empty before the first; a page heard since stands
    /// after them until the next is applied.
    pub order: Vec<([u8; 16], StrId)>,
    /// When it was applied, on the model's monotonic clock.
    pub applied_ms: Option<u64>,
    /// A finger is down on the list: no new order until it lifts.
    pub held: bool,
    /// What this device heard and its host has not kept yet (1632).
    pub owed: HeardOwed,
    /// The suggestion as the host last kept it.
    pub held_kept: Option<[u8; 16]>,
    /// The mailbox's list as this device has it (1644).
    pub mailbox: MailboxList,
}

/// The mailbox's list of pages and servers as this device last had it
/// (1644, step 4 of the mailbox-first plan): whether the link to the
/// mailbox is up, when the device last asked `/pages` and when the last
/// answer came. Opening the tab asks once more ([`UiEffect::AskPages`]),
/// never twice within [`ASK_EVERY_MS`]; the line on top says how old the
/// list is once the link is down or the answer older than
/// [`AS_OF_AFTER_SECS`] ([`as_of`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailboxList {
    /// The link to the mailbox: up, down, or `None` on a device that has
    /// told of none (no mailbox, or not told yet), which neither asks on
    /// opening nor says an age.
    pub link: Option<bool>,
    /// When the last `/pages` ask began, on the model's monotonic clock:
    /// the one after a pull ([`pages_asked`]) or the tab's.
    pub asked_ms: Option<u64>,
    /// When the last answer came, unix seconds. In memory only, as the
    /// stamp is: a start asks for the whole list anyway.
    pub answered: Option<i64>,
}

/// The least time between two `/pages` asks the tab's opening starts
/// (1644): one a minute at most.
pub const ASK_EVERY_MS: u64 = 60_000;

/// From this age of the last answer on, the line on top says it (1644):
/// "As of 3 h ago". Below it, nothing.
pub const AS_OF_AFTER_SECS: i64 = 3_600;

/// A page or a server not announced for this long is not listed (1644,
/// Lew took the plan's recommendation: seven days, a default and not a
/// setting). Its row stays in the store and the registry; the next
/// announce lists it again. A bookmark stays listed, as does the server
/// in use and the ones Loquax ships.
pub const HIDDEN_AFTER_SECS: u64 = 7 * 86_400;

/// Whether a thing last announced at `last_seen` (unix seconds, 0 for
/// never) is too old to list at `now`.
pub fn aged_out(now: u64, last_seen: u64) -> bool {
    last_seen != 0 && now.saturating_sub(last_seen) >= HIDDEN_AFTER_SECS
}

/// A host began a `/pages` ask (the one after a pull, 1579): the tab's
/// opening waits [`ASK_EVERY_MS`] from here.
pub fn pages_asked<B>(model: &mut UiModel<B>) {
    model.explore.mailbox.asked_ms = Some(model.elapsed_ms);
}

/// The mailbox's `/pages` answer came (1644): the list is as of now.
pub(crate) fn pages_answered<B>(model: &mut UiModel<B>) {
    let (now_secs, _) = now(model);
    model.explore.mailbox.answered = Some(i64::try_from(now_secs).unwrap_or(i64::MAX));
}

/// Opening the tab asks the mailbox (1644): only a device that has one,
/// and never within [`ASK_EVERY_MS`] of the last ask. The ask is marked
/// begun here; the host runs it one at a time.
///
/// Every opening logs `EXPLORE_OPEN` with whether it asked and why not
/// (1667, walk 11, finding 63).
fn ask_on_opening<B>(model: &mut UiModel<B>) -> Option<UiEffect> {
    let list = &model.explore.mailbox;
    if list.link.is_none() {
        crate::events::explore_open(false, "no_mailbox", None);
        return None;
    }
    let since = list.asked_ms.map(|at| model.elapsed_ms.saturating_sub(at));
    if list
        .asked_ms
        .is_some_and(|at| model.elapsed_ms < at.saturating_add(ASK_EVERY_MS))
    {
        crate::events::explore_open(false, "recent", since);
        return None;
    }
    crate::events::explore_open(true, "asked", since);
    pages_asked(model);
    Some(UiEffect::AskPages)
}

/// The line on top of the tab (1644): how old the list is, in the
/// catalogue's words. The link down: "Not connected", with "as of
/// yesterday 14:20" once an answer came; the link up and the answer older
/// than [`AS_OF_AFTER_SECS`]: "As of 3 h ago"; fresh, or a device without
/// a mailbox: nothing.
pub fn as_of<B>(model: &UiModel<B>) -> Option<Text> {
    let list = &model.explore.mailbox;
    let (now_secs, offset) = now(model);
    let now_secs = i64::try_from(now_secs).unwrap_or(i64::MAX);
    match (list.link?, list.answered) {
        (false, None) => Some(Text::key(StrId::MailboxDown)),
        (false, Some(at)) => Some(Text::Key(
            StrId::ExploreDownAsOf,
            Vec::from([Arg::Text(alloc::boxed::Box::new(as_of_when(
                at, now_secs, offset,
            )))]),
        )),
        (true, Some(at)) if now_secs.saturating_sub(at) > AS_OF_AFTER_SECS => Some(Text::Key(
            StrId::ExploreAsOf,
            Vec::from([Arg::Text(alloc::boxed::Box::new(super::scene::ago(
                now_secs.saturating_sub(at),
            )))]),
        )),
        (true, _) => None,
    }
}

/// "as of yesterday 14:20": the answer's day and time of day, as a
/// shared position's sentence says them ([`super::scene::position_words`]).
fn as_of_when(at: i64, now: i64, offset: i32) -> Text {
    use crate::when::WallTime;
    let at = WallTime {
        unix_secs: at,
        offset_secs: offset,
    };
    let now = WallTime {
        unix_secs: now,
        offset_secs: offset,
    };
    let clock = Arg::Str(at.hh_mm());
    match now.day() - at.day() {
        ..=0 => Text::Key(StrId::ExploreAsOfToday, Vec::from([clock])),
        1 => Text::Key(StrId::ExploreAsOfYesterday, Vec::from([clock])),
        2..=6 => Text::Key(
            StrId::ExploreAsOfWeekday,
            Vec::from([
                Arg::Text(alloc::boxed::Box::new(super::scene::weekday(&at))),
                clock,
            ]),
        ),
        _ => Text::Key(
            StrId::ExploreAsOfDate,
            Vec::from([
                Arg::Text(alloc::boxed::Box::new(super::scene::list_time(&at, &now))),
                clock,
            ]),
        ),
    }
}

/// What a device heard since its host last wrote it to the store (1632,
/// walk 4 new 15: after a restart the list held one to four pages of
/// over twenty, and "suggested" sat elsewhere). The model owes it, the
/// host takes it with [`take_heard`] and keeps it (`crate::heard`), and
/// the next start reads it back with [`restore_heard`]. Bounded, so a
/// host that never takes it (the M9 keeps no store) holds no more than
/// the lists themselves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeardOwed {
    /// Pages heard, oldest first.
    pub pages: Vec<lnomad::discovery::DiscoveredNode>,
    /// Hubs heard, oldest first.
    pub hubs: Vec<super::hubs::HeardHub>,
    /// The suggestion, when it changed since the host last kept it.
    pub held: Option<Option<[u8; 16]>>,
}

/// The most pages [`HeardOwed`] holds before the oldest goes.
const OWED_PAGES: usize = 512;

/// A page this device heard, owed to the store.
pub(crate) fn owe_page<B>(model: &mut UiModel<B>, node: &lnomad::discovery::DiscoveredNode) {
    let pages = &mut model.explore.owed.pages;
    if pages.len() >= OWED_PAGES {
        pages.remove(0);
    }
    pages.push(node.clone());
}

/// A hub this device heard, owed to the store.
pub(crate) fn owe_hub<B>(model: &mut UiModel<B>, hub: &super::hubs::HeardHub) {
    let hubs = &mut model.explore.owed.hubs;
    if hubs.len() >= super::hubs::HEARD_BOUND {
        hubs.remove(0);
    }
    hubs.push(hub.clone());
}

/// What the host owes its store now: the pages and hubs heard since the
/// last take, and the suggestion when it moved.
pub fn take_heard<B>(model: &mut UiModel<B>) -> HeardOwed {
    let mut owed = core::mem::take(&mut model.explore.owed);
    let held = model.rooms.heard.held();
    if held != model.explore.held_kept {
        owed.held = Some(held);
        model.explore.held_kept = held;
    }
    owed
}

/// What the store kept, back in the lists at a start (1632): every page
/// with its last seen, every hub heard, and the suggestion held before,
/// which stays while it still fits ([`super::settings::settle_suggestion`]).
/// Nothing of it is owed again.
pub fn restore_heard<B>(
    model: &mut UiModel<B>,
    pages: Vec<lnomad::discovery::DiscoveredNode>,
    hubs: Vec<super::hubs::HeardHub>,
    held: Option<[u8; 16]>,
) {
    for node in pages {
        model.browse.fold(lnomad::AppEvent::NodeDiscovered(node));
    }
    for hub in hubs {
        model.rooms.heard.hear(hub);
    }
    model.rooms.heard.hold(held);
    super::settings::settle_suggestion_said(&mut model.rooms);
    model.explore.held_kept = model.rooms.heard.held();
}

/// The least time between two orders applied on their own (1618): a
/// new announce moves the list at most once a second.
pub const SETTLE_MS: u64 = 1_000;

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
    /// The star or the page's menu: bookmarked, or not.
    Bookmark([u8; 16]),
    /// A room the reader is in, opened (an index into the Rooms' list).
    OpenRoom(usize),
    /// A hub's row: its sheet opens. Nothing is chosen and nothing linked
    /// (1618).
    Hub([u8; 16]),
    /// "Open rooms" (1655, walk 9, 45): the rooms screen of the server in
    /// use, its list, its state and the field for another room, joining
    /// nothing; it connects when it is not connected.
    OpenRooms,
    /// The sheet's button: this hub is the public rooms' hub from now on,
    /// linked at once (1652), and its rooms show as it lists them.
    UseHub([u8; 16]),
    /// One of the rooms the sheet lists: the hub is used, and the room
    /// joined as the hub spells it once it welcomed us.
    JoinRoom([u8; 16], String),
    /// The sheet's "try again" (1653): the server is asked again for its
    /// rooms.
    AskAgain([u8; 16]),
    /// The sheet closes.
    CloseSheet,
    /// A finger came down on the list (`true`) or lifted (1618): while it
    /// is down no new order is applied.
    Touch(bool),
    /// The host's word on the link to its mailbox (1644): up or down. A
    /// host without a mailbox never says it.
    Mailbox(bool),
}

/// A page's row as the tab draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRow {
    pub dest: [u8; 16],
    /// The name alone; `name ·hash4` only where another page's name looks
    /// the same; "Unnamed page" for a node that gave none (1618).
    pub label: Text,
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
    /// Its name; "Server without a name" for a nameless announce, never a
    /// hash.
    pub name: Text,
    /// When it was last heard, as a page's row says it (1618).
    pub seen: Option<Text>,
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
    /// How old the mailbox's list is, on top of the tab (1644, [`as_of`]).
    pub as_of: Option<Text>,
}

/// The rooms' section (1621, walk 2: "Public rooms" held servers and no
/// room): "Rooms" with the rooms the reader is in, then "Servers" with
/// every server to pick from, each part one sentence when it is empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomsSection {
    /// "Rooms".
    pub title: Text,
    /// The rooms the reader is in.
    pub joined: Vec<RoomRow>,
    /// Where the terminal's cursor is among them.
    pub joined_cursor: Option<usize>,
    /// "You are in no room. Pick a server below." while there is none,
    /// "You are in no room on `<server>`." while one is in use (1655, walk
    /// 9, 45: the first sentence stood above the server's tick).
    pub joined_empty: Option<Text>,
    /// "Open rooms" while a server is in use (1655, walk 9, 45): its
    /// rooms screen, without joining anything.
    pub open_rooms: Option<Text>,
    /// The terminal's cursor is on it.
    pub open_cursor: bool,
    /// "Servers".
    pub servers_title: Text,
    /// The servers Loquax ships, the reader's own and those heard on the
    /// mesh, in one list.
    pub hubs: Vec<HubRow>,
    /// "No server within reach.", or "No server matches …" in a search.
    pub servers_empty: Option<Text>,
}

/// A hub's sheet (1618): what a tap on its row shows, changing nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubSheet {
    pub hash: [u8; 16],
    pub name: Text,
    /// "reachable over 5 stations", when known.
    pub far: Option<Text>,
    pub seen: Option<Text>,
    /// The public rooms' hub in force: the button says so instead.
    pub chosen: bool,
    /// "Use this server for rooms", or "Your rooms are on this server."
    pub button: Text,
    /// What using it tells the hub: the reader's name (1577).
    pub notice: Text,
    pub rooms_title: Text,
    /// The rooms it named, each a join: what its notices named when the
    /// sheet asked (1621, [`super::rooms::RoomAsk::Peek`]) and what the
    /// shipped list recorded, each once.
    pub rooms: Vec<String>,
    /// The one sentence about them: asking, no answer, or that it named
    /// none (only once it answered and named none, walk 2).
    pub rooms_note: Option<Text>,
    /// "Try again" once the server gave no answer, or none within
    /// [`super::rooms::ASK_PATIENCE_SECS`] (1653).
    pub retry: Option<Text>,
    /// The question is out and the sentence may still turn: a frontend
    /// that draws on answers only draws again within a second.
    pub asking: bool,
    /// "Open rooms" on the sheet of the server in use (1655, walk 9, 45:
    /// it had no way to its rooms screen but joining a room).
    pub open_rooms: Option<Text>,
    pub close: Text,
    /// The terminal's cursor: 0 the button ("Open rooms" on the server in
    /// use), then the rooms.
    pub cursor: usize,
}

/// The tab as the frontends draw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExploreNode {
    pub pages: PagesNode,
    pub rooms: RoomsSection,
    /// A hub's sheet over the list (1618).
    pub sheet: Option<HubSheet>,
}

/// One of the tab's rows, in the order the terminal's cursor walks them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    Address,
    Page([u8; 16]),
    Room(usize),
    /// "Open rooms" (1655).
    OpenRooms,
    Hub([u8; 16]),
}

/// A page known to the device, before it is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    pub dest: [u8; 16],
    pub name: Option<String>,
    /// Unix seconds; 0 for a bookmark never heard.
    pub last_seen: u64,
    /// Unix seconds; 0 for a bookmark never heard.
    pub first_seen: u64,
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
            name: n.name.clone().filter(|name| !name.trim().is_empty()),
            last_seen: n.last_seen,
            first_seen: n.first_seen,
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
                first_seen: 0,
                hops: None,
                bookmark: Some(bookmark.url.clone()),
            }),
        }
    }
    out
}

/// Whether a row named `name` at `dest` matches the search (1618): the
/// name, case folded, holds the query, or the query is an address and
/// names `dest`. Never a hash's letters: "a" found every row whose short
/// hash held an "a".
pub fn matches_name(name: Option<&str>, dest: &[u8; 16], query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    if let Some(url) = address(query) {
        return bookmark_dest(&url) == Some(*dest);
    }
    let query = query.to_lowercase();
    name.is_some_and(|name| name.to_lowercase().contains(&query))
}

/// Whether `known` matches the search ([`matches_name`]).
pub fn matches(known: &Known, query: &str) -> bool {
    matches_name(known.name.as_deref(), &known.dest, query)
}

/// The pages in the tab's order: bookmarks first, then by last seen, the
/// newest first, or by name with `az`, where a page without a name comes
/// after the named ones, the first heard newest first (1618: they stood
/// on top of A); a tie by hash, so the order is stable.
pub fn ordered(mut pages: Vec<Known>, az: bool) -> Vec<Known> {
    let name = |k: &Known| k.name.as_deref().map(str::to_lowercase);
    pages.sort_by(|a, b| {
        b.bookmark
            .is_some()
            .cmp(&a.bookmark.is_some())
            .then_with(|| match az {
                true => match (name(a), name(b)) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => core::cmp::Ordering::Less,
                    (None, Some(_)) => core::cmp::Ordering::Greater,
                    (None, None) => b.first_seen.cmp(&a.first_seen),
                },
                false => b.last_seen.cmp(&a.last_seen),
            })
            .then_with(|| a.dest.cmp(&b.dest))
    });
    pages
}

/// The heading a page stands under: bookmarks, A-Z's one, or its age.
fn heading(page: &Known, az: bool, now_secs: u64, offset: i32) -> StrId {
    match (&page.bookmark, az) {
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
    }
}

/// The pages the tab may list (1644): every one [`known`] but those not
/// announced for [`HIDDEN_AFTER_SECS`]; a bookmark always.
fn listable<B>(model: &UiModel<B>) -> Vec<Known> {
    let (now_secs, _) = now(model);
    let mut pages = known(&model.browse.model);
    pages.retain(|k| k.bookmark.is_some() || !aged_out(now_secs, k.last_seen));
    pages
}

/// Every page the device knows in the order the model would apply now,
/// each under its heading.
fn fresh<B>(model: &UiModel<B>) -> Vec<(Known, StrId)> {
    let (now_secs, offset) = now(model);
    let az = model.explore.az;
    ordered(listable(model), az)
        .into_iter()
        .map(|k| {
            let h = heading(&k, az, now_secs, offset);
            (k, h)
        })
        .collect()
}

/// Every page in the order last applied (1618): the pages of
/// [`Explore::order`] where they stood, under the heading they had, then
/// the pages heard since in the fresh order. Before the first order, the
/// fresh one.
fn arranged<B>(model: &UiModel<B>) -> Vec<(Known, StrId)> {
    let fresh = fresh(model);
    let order = &model.explore.order;
    if order.is_empty() {
        return fresh;
    }
    let mut slot: BTreeMap<[u8; 16], usize> = BTreeMap::new();
    for (i, (dest, _)) in order.iter().enumerate() {
        slot.insert(*dest, i);
    }
    let mut placed: Vec<Option<(Known, StrId)>> = (0..order.len()).map(|_| None).collect();
    let mut later = Vec::new();
    for (known, heading) in fresh {
        match slot.get(&known.dest) {
            Some(&i) => {
                let kept = order.get(i).map_or(heading, |(_, h)| *h);
                if let Some(at) = placed.get_mut(i) {
                    *at = Some((known, kept));
                }
            }
            None => later.push((known, heading)),
        }
    }
    placed.into_iter().flatten().chain(later).collect()
}

/// Apply the fresh order (1618): `force` for the reader's own verbs (the
/// tab opened, a search, the toggle, a bookmark), else at most once in
/// [`SETTLE_MS`] and never while a finger is down.
pub(super) fn settle<B>(model: &mut UiModel<B>, force: bool) {
    if !model.explore.open {
        return;
    }
    if !force {
        if model.explore.held {
            return;
        }
        if let Some(at) = model.explore.applied_ms {
            if model.elapsed_ms < at.saturating_add(SETTLE_MS) {
                return;
            }
        }
    }
    let order: Vec<([u8; 16], StrId)> =
        fresh(model).into_iter().map(|(k, h)| (k.dest, h)).collect();
    if !force && order == model.explore.order {
        return;
    }
    model.explore.order = order;
    model.explore.applied_ms = Some(model.elapsed_ms);
}

/// The pages the tab lists: arranged, filtered by the search.
fn listed<B>(model: &UiModel<B>) -> Vec<(Known, StrId)> {
    arranged(model)
        .into_iter()
        .filter(|(k, _)| matches(k, &model.explore.query))
        .collect()
}

/// How many pages bear each look of a name ([`crate::names::skeleton`]).
fn looks(pages: &[Known]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for name in pages.iter().filter_map(|k| k.name.as_deref()) {
        *out.entry(crate::names::skeleton(name)).or_insert(0) += 1;
    }
    out
}

/// A page's label (1618): the name alone, its short hash after it only
/// where another page's name looks the same, "Unnamed page" without one.
fn label(page: &Known, looks: &BTreeMap<String, usize>) -> Text {
    match &page.name {
        Some(name) => match looks.get(&crate::names::skeleton(name)) {
            Some(n) if *n > 1 => {
                Text::Raw(format!("{name} {}", crate::names::hash_suffix(&page.dest)))
            }
            _ => Text::Raw(name.clone()),
        },
        None => Text::key(StrId::ExploreUnnamedPage),
    }
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

/// How far a page is, when known and a count a reader can use
/// ([`lnomad::discovery::shown_hops`], 1632: the mesh carries 110).
fn hops(hops: Option<u32>) -> Option<Text> {
    match lnomad::discovery::shown_hops(hops)? {
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
    out.extend(listed(model).into_iter().map(|(k, _)| Pick::Page(k.dest)));
    out.extend((0..model.rooms.rooms.len()).map(Pick::Room));
    if model.rooms.hub.is_some() {
        out.push(Pick::OpenRooms);
    }
    out.extend(hub_list(model).iter().map(|h| Pick::Hub(h.hash)));
    out
}

/// A hub's name as its row says it.
fn hub_name<B>(model: &UiModel<B>, hub: &super::settings::Offered) -> Text {
    match &hub.name {
        Some(name) => Text::Raw(name.clone()),
        None if !hub.shipped && !hub.heard => super::rooms::hub_label(&model.rooms),
        None => Text::key(StrId::RoomsHubUnnamed),
    }
}

/// How far a hub is, in stations (1618), when known.
fn far(hops: Option<u8>) -> Option<Text> {
    match lnomad::discovery::shown_hops(hops.map(u32::from))? {
        0 | 1 => Some(Text::key(StrId::RoomsHubHop)),
        n => Some(Text::Key(
            StrId::RoomsHubHops,
            Vec::from([Arg::Int(i64::from(n))]),
        )),
    }
}

/// The servers the section lists, in one list (1621): every server
/// heard, shipped or not, last seen first, so a server once heard never
/// leaves the list (1655, walk 9, 44: the list showed only the first
/// [`super::settings::HUB_PAGE`] shipped ones, and twenty heard after
/// Whatcom pushed it out); then the shipped ones never heard, in the
/// file's order, as many as make [`super::settings::HUB_PAGE`] shipped
/// rows, all of them under a search (Explore is a list to pick from, the
/// search reaches the whole catalogue); the one in force and the
/// reader's own always among them.
fn hub_list<B>(model: &UiModel<B>) -> Vec<super::settings::Offered> {
    let offered = super::settings::offered_hubs(&model.rooms);
    let searching = !model.explore.query.trim().is_empty();
    let chosen = model.rooms.hub;
    // A server not announced for a week (1644) counts as not heard: a
    // shipped one stands among the shipped, any other is not listed, the
    // one in use always is.
    let (now_secs, _) = now(model);
    let stale = |o: &super::settings::Offered| {
        o.heard
            && o.last
                .is_some_and(|last| aged_out(now_secs, u64::try_from(last).unwrap_or(0)))
    };
    let hidden = |o: &super::settings::Offered| stale(o) && !o.shipped && Some(o.hash) != chosen;
    let heard_shipped = offered
        .iter()
        .filter(|o| o.heard && !stale(o) && o.shipped)
        .count();
    let mut unheard = super::settings::HUB_PAGE.saturating_sub(heard_shipped);
    let mut hubs: Vec<super::settings::Offered> = Vec::new();
    for hub in offered {
        if hidden(&hub) {
            continue;
        }
        if (hub.heard && !stale(&hub)) || searching || Some(hub.hash) == chosen {
            hubs.push(hub);
        } else if hub.shipped && unheard > 0 {
            unheard -= 1;
            hubs.push(hub);
        }
    }
    if let Some(own) = chosen {
        if !hubs.iter().any(|o| o.hash == own) {
            hubs.push(super::settings::Offered {
                hash: own,
                name: None,
                hops: None,
                shipped: false,
                heard: false,
                answered: None,
                last: None,
            });
        }
    }
    // The search filters them too (1618), by name or a pasted address.
    let query = model.explore.query.clone();
    let keep = |o: &super::settings::Offered| {
        let name = hub_name(model, o).resolve(model.lang);
        matches_name(Some(&name), &o.hash, &query)
    };
    // The suggested one first, the rest in their order (1632, walk 4 new
    // 17: it stood fourth, and a newcomer looks for it on top).
    if let Some(first) = suggested(model) {
        match hubs.iter().position(|o| o.hash == first) {
            Some(at) => {
                let hub = hubs.remove(at);
                hubs.insert(0, hub);
            }
            None => {
                if let Some(hub) = super::settings::offered_hubs(&model.rooms)
                    .into_iter()
                    .find(|o| o.hash == first)
                {
                    hubs.insert(0, hub);
                }
            }
        }
    }
    hubs.retain(|o| keep(o) && !hidden(o));
    hubs
}

/// The one server "suggested" marks ([`super::settings::suggested`]),
/// over every server offered, the search's or not.
fn suggested<B>(model: &UiModel<B>) -> Option<[u8; 16]> {
    super::settings::suggested(
        &super::settings::offered_hubs(&model.rooms),
        model.rooms.hub,
        model.rooms.heard.held(),
    )
}

fn hub_row<B>(
    model: &UiModel<B>,
    hub: &super::settings::Offered,
    cursor: Option<&Pick>,
    suggest: Option<[u8; 16]>,
) -> HubRow {
    let chosen = model.rooms.hub == Some(hub.hash);
    let name = hub_name(model, hub);
    let mut parts = Vec::new();
    parts.extend(far(hub.hops));
    if suggest == Some(hub.hash) {
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
    let (now_secs, _) = now(model);
    HubRow {
        hash: hub.hash,
        name,
        // Every server says when it was heard (1621, walk 2: the shipped
        // ones said nothing): "not seen yet" for one never heard.
        seen: Some(
            hub.last
                .and_then(|last| seen(now_secs, u64::try_from(last).unwrap_or(0)))
                .unwrap_or_else(|| Text::key(StrId::ExploreNotSeen)),
        ),
        value,
        chosen,
        cursor: cursor == Some(&Pick::Hub(hub.hash)),
    }
}

/// The hub the sheet is about, as [`hub_lists`] offers it, or as the
/// settings know it where the search hides its row.
fn sheet_hub<B>(model: &UiModel<B>, hash: [u8; 16]) -> super::settings::Offered {
    super::settings::offered_hubs(&model.rooms)
        .into_iter()
        .find(|o| o.hash == hash)
        .unwrap_or(super::settings::Offered {
            hash,
            name: None,
            hops: None,
            shipped: false,
            heard: false,
            answered: None,
            last: None,
        })
}

/// The rooms the hub named (1621): what its notices named when its
/// sheet asked, or until that answer came what it named when the mailbox
/// asked (1643), then what the shipped list recorded, each once: a
/// `#word` and the bare word are one room, spelt as the answer spells it
/// (1655, walk 9, 46: Minuteman's "lobby" and the list's "#lobby" were two
/// rows; [`super::hubs::rooms_merged`], the rooms screen's rule since 1652).
fn sheet_rooms(rooms: &super::rooms::Rooms, hash: &[u8; 16]) -> Vec<String> {
    let answered: &[String] = match (rooms.named.get(hash), rooms.mailbox_named.get(hash)) {
        (Some(Named::Rooms(named)), _) => named,
        (_, Some(mailbox)) => mailbox,
        _ => &[],
    };
    let shipped = super::hubs::named(hash)
        .map(|h| h.rooms)
        .unwrap_or_default();
    super::hubs::rooms_merged(answered, &shipped)
}

/// The sentence under the sheet's rooms (1621): asking while the question
/// is out, with its seconds from the fifth on as the rooms screen counts
/// its connecting (1655, walk 9, 47: a slow server's sheet said the same
/// sentence for 60 to 90 s); the hub's silence; or that it named none,
/// only once it answered and none is known. Silence and an empty answer
/// read apart, in the rooms screen's words (1652).
fn rooms_note(
    rooms: &super::rooms::Rooms,
    hash: &[u8; 16],
    listed: bool,
    now: Option<i64>,
) -> Option<Text> {
    match rooms.named.get(hash) {
        Some(Named::Asking { since }) if overdue(*since, now) => {
            Some(Text::key(StrId::RoomsNoAnswer))
        }
        Some(Named::Asking { since }) => Some(match (*since, now) {
            // Slow from 20 s, as the rooms screen says it (1659, walk 10, 53).
            (Some(since), Some(now)) if now - since >= super::rooms::SLOW_SECS => {
                Text::Key(StrId::RoomsSlowFor, Vec::from([Arg::Int(now - since)]))
            }
            (Some(since), Some(now)) if now - since >= super::rooms::LINKING_QUIET_SECS => {
                Text::Key(
                    StrId::ExploreServerAskingFor,
                    Vec::from([Arg::Int(now - since)]),
                )
            }
            _ => Text::key(StrId::ExploreServerAsking),
        }),
        Some(Named::NoAnswer) if !listed => Some(Text::key(StrId::RoomsNoAnswer)),
        Some(Named::Rooms(_)) if !listed => Some(Text::key(StrId::RoomsNoneNamed)),
        _ => None,
    }
}

/// The question asked at `since` waited [`super::rooms::ASK_PATIENCE_SECS`]
/// by `now` (1653).
fn overdue(since: Option<i64>, now: Option<i64>) -> bool {
    match (since, now) {
        (Some(since), Some(now)) => now - since >= super::rooms::ASK_PATIENCE_SECS,
        _ => false,
    }
}

/// Whether the sheet offers to ask again: the server gave no answer, or
/// none in time (1653).
fn retry(rooms: &super::rooms::Rooms, hash: &[u8; 16], now: Option<i64>) -> bool {
    match rooms.named.get(hash) {
        Some(Named::NoAnswer) => true,
        Some(Named::Asking { since }) => overdue(*since, now),
        _ => false,
    }
}

/// Ask the server of the sheet for its rooms (1621), stamped with the
/// model's clock (1653).
fn ask<B>(model: &mut UiModel<B>, hash: [u8; 16]) -> Vec<UiEffect> {
    let since = model.now.map(|now| now.unix_secs);
    model.rooms.named.insert(hash, Named::Asking { since });
    Vec::from([UiEffect::Rooms(super::rooms::RoomAsk::Peek { hub: hash })])
}

/// The sheet's rows in the terminal cursor's order: the button ("Open
/// rooms" on the hub in force, 1655), then the rooms.
pub fn sheet_picks<B>(model: &UiModel<B>) -> usize {
    let Some(hash) = model.explore.sheet else {
        return 0;
    };
    1 + sheet_rooms(&model.rooms, &hash).len()
}

/// The open sheet as the frontends draw it.
fn sheet<B>(model: &UiModel<B>) -> Option<HubSheet> {
    let hash = model.explore.sheet?;
    let hub = sheet_hub(model, hash);
    let chosen = model.rooms.hub == Some(hash);
    let rooms = sheet_rooms(&model.rooms, &hash);
    let (now_secs, _) = now(model);
    let clock = model.now.map(|now| now.unix_secs);
    Some(HubSheet {
        hash,
        name: hub_name(model, &hub),
        far: far(hub.hops),
        // As its row says it (1629): "not seen yet" for one never heard.
        seen: Some(
            hub.last
                .and_then(|last| seen(now_secs, u64::try_from(last).unwrap_or(0)))
                .unwrap_or_else(|| Text::key(StrId::ExploreNotSeen)),
        ),
        chosen,
        button: Text::key(match chosen {
            true => StrId::ExploreServerInUse,
            false => StrId::ExploreServerUse,
        }),
        notice: super::rooms::name_notice(model),
        rooms_title: Text::key(StrId::ExploreServerRooms),
        rooms_note: rooms_note(&model.rooms, &hash, !rooms.is_empty(), clock),
        retry: retry(&model.rooms, &hash, clock).then(|| Text::key(StrId::RoomsRetry)),
        asking: matches!(model.rooms.named.get(&hash), Some(Named::Asking { .. })),
        open_rooms: chosen.then(|| Text::key(StrId::ExploreOpenRooms)),
        rooms,
        close: Text::key(StrId::ExploreServerClose),
        cursor: model
            .explore
            .sheet_cursor
            .min(sheet_picks(model).saturating_sub(1)),
    })
}

/// The tab for the frontends.
pub fn node<B>(model: &UiModel<B>) -> ExploreNode {
    let explore = &model.explore;
    let picks = picks(model);
    let cursor = picks.get(explore.cursor.min(picks.len().saturating_sub(1)));
    let (now_secs, _) = now(model);
    let all = listable(model);
    let any = !all.is_empty();
    let looks = looks(&all);
    let pages = listed(model);
    let mut groups: Vec<PageGroup> = Vec::new();
    for (page, heading) in pages {
        let row = PageRow {
            dest: page.dest,
            label: label(&page, &looks),
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
        as_of: as_of(model),
    };
    let rooms_node = super::rooms::node(model);
    let suggest = suggested(model);
    let listed = hub_list(model);
    // Two servers that read alike get their short address (1629, walk 3
    // new 6: two rows "Server ohne Namen"), as pages and names do.
    let mut alike: BTreeMap<String, usize> = BTreeMap::new();
    for hub in &listed {
        let shown = hub_name(model, hub).resolve(model.lang);
        *alike.entry(crate::names::skeleton(&shown)).or_default() += 1;
    }
    let hubs: Vec<HubRow> = listed
        .iter()
        .map(|h| {
            let mut row = hub_row(model, h, cursor, suggest);
            let shown = row.name.resolve(model.lang);
            if alike
                .get(&crate::names::skeleton(&shown))
                .is_some_and(|n| *n > 1)
            {
                row.name = Text::Raw(format!("{shown} {}", crate::names::hash_suffix(&h.hash)));
            }
            row
        })
        .collect();
    let joined_cursor = match cursor {
        Some(Pick::Room(i)) => Some(*i),
        _ => None,
    };
    let query = explore.query.trim();
    let servers_empty = hubs.is_empty().then(|| match query.is_empty() {
        true => Text::key(StrId::ExploreNoServers),
        false => Text::Key(
            StrId::ExploreNoServerMatch,
            Vec::from([Arg::Str(query.to_string())]),
        ),
    });
    ExploreNode {
        pages,
        rooms: RoomsSection {
            title: Text::key(StrId::ExploreRooms),
            joined_empty: rooms_node.rows.is_empty().then(|| match model.rooms.hub {
                // The server in use by name (1655, walk 9, 45).
                Some(_) => Text::Key(
                    StrId::ExploreNoJoinedOn,
                    Vec::from([Arg::Text(alloc::boxed::Box::new(super::rooms::hub_label(
                        &model.rooms,
                    )))]),
                ),
                None => Text::key(StrId::ExploreNoJoined),
            }),
            open_rooms: model.rooms.hub.map(|_| Text::key(StrId::ExploreOpenRooms)),
            open_cursor: cursor == Some(&Pick::OpenRooms),
            joined: rooms_node.rows,
            joined_cursor,
            servers_title: Text::key(StrId::ExploreServers),
            hubs,
            servers_empty,
        },
        sheet: sheet(model),
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
            model.explore.held = false;
            settle(model, true);
            return ask_on_opening(model).into_iter().collect();
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
        ExploreEvent::Mailbox(up) => {
            model.explore.mailbox.link = Some(up);
            Vec::new()
        }
        ExploreEvent::Typed(text) => {
            if model.explore.query != text {
                model.explore.query = text;
                model.explore.cursor = 0;
                settle(model, true);
            }
            Vec::new()
        }
        ExploreEvent::Az(on) => {
            model.explore.az = on;
            settle(model, true);
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
        ExploreEvent::Bookmark(dest) => {
            let effects = bookmark(model, dest);
            settle(model, true);
            effects
        }
        ExploreEvent::OpenRoom(index) => {
            if index >= model.rooms.rooms.len() {
                return Vec::new();
            }
            show_rooms(model);
            super::rooms::fold(model, RoomsEvent::Show(Some(index)))
        }
        ExploreEvent::Hub(hash) => {
            model.explore.sheet = Some(hash);
            model.explore.sheet_cursor = 0;
            // The sheet asks the server what it names (1621), once a run:
            // a known answer shows at once, a silence is asked again.
            // An overdue question is asked again too (1653).
            let clock = model.now.map(|now| now.unix_secs);
            match model.rooms.named.get(&hash) {
                Some(Named::Asking { since }) if !overdue(*since, clock) => Vec::new(),
                Some(Named::Rooms(_)) => Vec::new(),
                None | Some(Named::NoAnswer | Named::Asking { .. }) => ask(model, hash),
            }
        }
        ExploreEvent::AskAgain(hash) => {
            if model.explore.sheet != Some(hash) {
                return Vec::new();
            }
            ask(model, hash)
        }
        ExploreEvent::OpenRooms => {
            if model.rooms.hub.is_none() {
                return Vec::new();
            }
            model.explore.sheet = None;
            show_rooms(model);
            let mut effects = super::rooms::fold(model, RoomsEvent::Show(None));
            effects.extend(super::rooms::fold(model, RoomsEvent::Retry));
            effects
        }
        ExploreEvent::UseHub(hash) => {
            model.explore.sheet = None;
            let mut effects = super::rooms::choose(model, HubChoice::Hub(hash));
            show_rooms(model);
            effects.extend(super::rooms::fold(model, RoomsEvent::Show(None)));
            effects
        }
        ExploreEvent::JoinRoom(hash, room) => {
            model.explore.sheet = None;
            let mut effects = super::rooms::choose(model, HubChoice::Hub(hash));
            show_rooms(model);
            effects.extend(super::rooms::fold(model, RoomsEvent::Show(None)));
            // Joined by its name, not typed as `/join name`: a name the hub
            // spells with a space would be read as a room and a key (1652).
            effects.extend(super::rooms::fold(model, RoomsEvent::Join(room)));
            effects
        }
        ExploreEvent::CloseSheet => {
            model.explore.sheet = None;
            Vec::new()
        }
        ExploreEvent::Touch(down) => {
            model.explore.held = down;
            if !down {
                settle(model, false);
            }
            Vec::new()
        }
    }
}

/// A hub's rooms in front, from the tab; the choice linked (1652).
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
/// bookmarks a page's row, `Alt-s` turns the order, `Alt-1`, `Alt-2` and
/// `Alt-3` put the cursor on the pages, the rooms and the servers (1659,
/// walk 10, 54: the phone's jump chips), `Esc` clears the field and,
/// empty, leaves the tab for the chats, as `Ctrl-g` does.
pub(super) fn key<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    key: leviculum_ui::input::KeyEvent,
) -> Vec<UiEffect> {
    use leviculum_ui::input::{KeyCode, KeyModifiers};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    // A hub's sheet over the list (1618): the arrows walk the button and
    // the rooms, Enter takes the one under the cursor, Esc closes it.
    if let Some(hash) = model.explore.sheet {
        let rows = sheet_picks(model);
        let button = 1;
        let at = model.explore.sheet_cursor.min(rows.saturating_sub(1));
        match key.code {
            KeyCode::Up => model.explore.sheet_cursor = at.saturating_sub(1),
            KeyCode::Down if rows > 0 => model.explore.sheet_cursor = (at + 1).min(rows - 1),
            KeyCode::Enter if rows > 0 => {
                if at < button {
                    return match model.rooms.hub == Some(hash) {
                        true => fold(model, ExploreEvent::OpenRooms),
                        false => fold(model, ExploreEvent::UseHub(hash)),
                    };
                }
                if let Some(room) = sheet_rooms(&model.rooms, &hash)
                    .into_iter()
                    .nth(at - button)
                {
                    return fold(model, ExploreEvent::JoinRoom(hash, room));
                }
            }
            KeyCode::Esc => model.explore.sheet = None,
            KeyCode::Char('g') if ctrl => model.explore.sheet = None,
            _ => {}
        }
        return Vec::new();
    }
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
        KeyCode::Char(c @ '1'..='3') if alt => {
            if let Some(at) = section_start(&picks, c) {
                model.explore.cursor = at;
            }
        }
        KeyCode::Char(c) if !ctrl && !alt => {
            model.explore.query.push(c);
            model.explore.cursor = 0;
            settle(model, true);
        }
        KeyCode::Backspace => {
            model.explore.query.pop();
            model.explore.cursor = 0;
            settle(model, true);
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
                Some(Pick::OpenRooms) => fold(model, ExploreEvent::OpenRooms),
                Some(Pick::Hub(hash)) => fold(model, ExploreEvent::Hub(hash)),
                None => Vec::new(),
            };
        }
        KeyCode::Esc if !model.explore.query.is_empty() => {
            model.explore.query.clear();
            model.explore.cursor = 0;
            settle(model, true);
        }
        KeyCode::Esc => model.explore.open = false,
        _ => {}
    }
    Vec::new()
}

/// The first pick of a section (1659, walk 10, 54): `'1'` the pages (the
/// list's top), `'2'` the rooms (a room, "Open rooms", else the first
/// server under the section), `'3'` the servers; `None` when the section
/// has none.
fn section_start(picks: &[Pick], section: char) -> Option<usize> {
    match section {
        '1' => (!picks.is_empty()).then_some(0),
        '2' => picks
            .iter()
            .position(|p| matches!(p, Pick::Room(_) | Pick::OpenRooms | Pick::Hub(_))),
        _ => picks.iter().position(|p| matches!(p, Pick::Hub(_))),
    }
}

fn page_json(row: &PageRow) -> Value {
    json!({
        "dest": crate::address::to_hex(&row.dest),
        "label": row.label.to_json(),
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
        "seen": row.seen.as_ref().map(Text::to_json),
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
                "as_of": p.as_of.as_ref().map(Text::to_json),
            },
            "rooms": {
                "title": r.title.to_json(),
                "joined": r.joined.iter().map(|row| json!({
                    "name": row.name,
                    "unread": row.unread,
                    "public": row.public.to_json(),
                })).collect::<Vec<_>>(),
                "joined_cursor": r.joined_cursor,
                "joined_empty": r.joined_empty.as_ref().map(Text::to_json),
                "open_rooms": r.open_rooms.as_ref().map(Text::to_json),
                "servers_title": r.servers_title.to_json(),
                "hubs": r.hubs.iter().map(hub_json).collect::<Vec<_>>(),
                "servers_empty": r.servers_empty.as_ref().map(Text::to_json),
            },
            "sheet": self.sheet.as_ref().map(|s| json!({
                "hash": crate::address::to_hex(&s.hash),
                "name": s.name.to_json(),
                "far": s.far.as_ref().map(Text::to_json),
                "seen": s.seen.as_ref().map(Text::to_json),
                "chosen": s.chosen,
                "button": s.button.to_json(),
                "notice": s.notice.to_json(),
                "rooms_title": s.rooms_title.to_json(),
                "rooms": s.rooms,
                "rooms_note": s.rooms_note.as_ref().map(Text::to_json),
                "open_rooms": s.open_rooms.as_ref().map(Text::to_json),
                "close": s.close.to_json(),
                "cursor": s.cursor,
            })),
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
    /// heard: one an hour ago, one three days ago, one six days ago (a
    /// month ago until 1644, which lists no page silent for a week).
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
            heard(0x33, None, 6 * 86_400, None),
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
            .flat_map(|g| g.rows.iter().map(|r| r.label.resolve(Lang::En)))
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
        assert_eq!(labels(&shown), ["Zebra Hof", "Alpha Relay", "Unnamed page"]);
        // "Older" holds no page that is not bookmarked since 1644: a page
        // silent for a week is not listed ([`HIDDEN_AFTER_SECS`]).
        assert_eq!(
            headings(&shown),
            [StrId::ExploreToday, StrId::ExploreThisWeek]
        );
        let first = &shown.pages.groups[0].rows[0];
        assert_eq!(
            first.seen,
            Some(Text::Key(StrId::ExploreSeenHours, vec![Arg::Int(1)]))
        );
        assert_eq!(
            first.seen.as_ref().map(|t| t.resolve(Lang::De)).as_deref(),
            Some("gesehen vor 1 Std.")
        );
        assert_eq!(
            first.hops,
            Some(Text::Key(StrId::ExploreHops, vec![Arg::Int(2)]))
        );
        // A bookmark on the oldest puts it on top, under its own heading.
        explore(&mut model, ExploreEvent::Bookmark([0x33; 16]));
        let shown = node(&model);
        assert_eq!(labels(&shown)[0], "Unnamed page");
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
        assert_eq!(labels(&shown), ["Unnamed page", "Alpha Relay", "Zebra Hof"]);
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
        assert_eq!(
            alpha.label,
            Text::Raw("Alpha Relay".into()),
            "the announced name wins"
        );
        assert!(
            alpha.url.ends_with(":/page/news.mu"),
            "the bookmarked page opens"
        );
        let zebra = all.iter().find(|r| r.dest == [0x11; 16]).expect("zebra");
        assert_eq!(
            zebra.label,
            Text::Raw("Zebra Hof Neu".into()),
            "the newest name wins"
        );
        let never = all.iter().find(|r| r.dest == [0x55; 16]).expect("bookmark");
        assert_eq!(never.seen, None);
        assert_eq!(never.label, Text::Raw("Never heard".into()));
    }

    /// The field filters by name, never by a hash's letters (1618), and
    /// takes an address or its `nomadnet://` form, which opens through the
    /// browser.
    #[test]
    fn the_field_filters_by_name_and_opens_an_address() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        explore(&mut model, ExploreEvent::Typed("alpha".into()));
        assert_eq!(labels(&node(&model)), ["Alpha Relay"]);
        // The walk's "a" found "SPECTRO RNS ·ea54" by its hash: a short
        // hash's letters find nothing now.
        explore(&mut model, ExploreEvent::Typed("33".into()));
        assert!(labels(&node(&model)).is_empty());
        explore(&mut model, ExploreEvent::Typed("·11".into()));
        assert!(labels(&node(&model)).is_empty());
        // A pasted address finds its page.
        explore(&mut model, ExploreEvent::Typed("33".repeat(16)));
        assert_eq!(labels(&node(&model)), ["Unnamed page"]);
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
        // Each part of the rooms' section in one sentence (1621): no room
        // joined says so; the shipped servers are always offered, and with
        // none it says so.
        assert_eq!(shown.rooms.title, Text::key(StrId::ExploreRooms));
        assert_eq!(
            shown.rooms.joined_empty,
            Some(Text::key(StrId::ExploreNoJoined))
        );
        assert_eq!(
            shown
                .rooms
                .joined_empty
                .as_ref()
                .map(|t| t.resolve(Lang::De))
                .as_deref(),
            Some("Du bist in keinem Raum. Such dir unten einen Server aus.")
        );
        assert_eq!(shown.rooms.servers_title, Text::key(StrId::ExploreServers));
        assert_eq!(
            shown.rooms.servers_empty.is_some(),
            crate::ui::hubs::shipped().is_empty()
        );
        // A search that finds no server says so in the same form.
        explore(&mut model, ExploreEvent::Typed("zzzz".into()));
        let shown = node(&model);
        assert_eq!(
            shown
                .rooms
                .servers_empty
                .as_ref()
                .map(|t| t.resolve(Lang::De))
                .as_deref(),
            Some("Kein Server passt zu „zzzz“.")
        );
    }

    /// A hub's row opens its sheet, which asks the server for its rooms
    /// over a question of its own (1621) and chooses nothing; the button
    /// chooses it and, since 1652, links at once (the privacy sentence of
    /// 1577 comes with it): "Connecting…", then the rooms the hub lists,
    /// one tap from a room. Heard hubs are listed with the shipped ones
    /// under "Servers".
    #[test]
    fn a_hub_picked_links_and_its_listed_room_is_one_tap_away() {
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
        let row = |shown: &ExploreNode| {
            shown
                .rooms
                .hubs
                .iter()
                .find(|h| h.hash == hub)
                .cloned()
                .expect("the heard server among the servers")
        };
        assert_eq!(row(&shown).name, Text::Raw("Werkstatt".into()));
        assert_eq!(shown.rooms.servers_title, Text::key(StrId::ExploreServers));
        // The row opens the sheet and chooses nothing (1618); it asks the
        // server what it names (1621), and nothing else.
        let effects = explore(&mut model, ExploreEvent::Hub(hub));
        assert_eq!(effects, [UiEffect::Rooms(RoomAsk::Peek { hub })]);
        assert_eq!(model.rooms.hub, None);
        let sheet = node(&model).sheet.expect("the sheet");
        assert_eq!(sheet.name, Text::Raw("Werkstatt".into()));
        assert!(!sheet.chosen);
        assert_eq!(
            sheet.far.as_ref().map(|t| t.resolve(Lang::De)).as_deref(),
            Some("über 2 Stationen erreichbar")
        );
        assert_eq!(sheet.button, Text::key(StrId::ExploreServerUse));
        let effects = explore(&mut model, ExploreEvent::UseHub(hub));
        assert!(node(&model).sheet.is_none());
        // Picking links at once (1652): no typed room name needed.
        assert!(
            effects.contains(&UiEffect::Rooms(RoomAsk::Open { hub })),
            "picking links: {effects:?}"
        );
        assert_eq!(model.rooms.state, HubState::Linking);
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(scene.screen, ScreenId::Rooms);
        assert_eq!(scene.tabs.expect("tabs").active, TabId::Explore);
        // The words tell the state: connecting, not "not connected".
        let rooms = crate::ui::rooms::node(&model);
        assert_eq!(rooms.status, Some(Text::key(StrId::RoomsLinking)));
        assert_eq!(rooms.retry, None, "nothing to retry while it links");
        // Before the welcome the title names the server, and nothing is
        // known of its rooms yet (1659: the list stands whatever the link).
        let listed = rooms.listed.expect("the title before the welcome");
        assert!(listed.rooms.is_empty() && listed.note.is_none());
        // The welcome: the hub's rooms are asked; until they come, it says so.
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Welcomed {
                name: Some("Werkstatt".into()),
                body_limit: 350,
            }),
        );
        let rooms = crate::ui::rooms::node(&model);
        assert_eq!(rooms.status, None);
        let listed = rooms.listed.expect("the list's place");
        assert_eq!(listed.note, Some(Text::key(StrId::RoomsAsking)));
        // Its answer: rows to tap, the typed-name field below them.
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Listed {
                rooms: vec!["general chat".into(), "lobby".into()],
            }),
        );
        let rooms = crate::ui::rooms::node(&model);
        let listed = rooms.listed.expect("the hub's rooms");
        assert_eq!(listed.rooms, ["general chat", "lobby"]);
        assert_eq!(listed.title.resolve(Lang::De), "Räume auf Werkstatt");
        assert_eq!(listed.other, Some(Text::key(StrId::RoomsOther)));
        assert_eq!(rooms.empty, None, "the field moved under the list");
        // A tap joins as the hub spells it, and the room opens.
        let effects = update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Join("general chat".into())),
        );
        assert_eq!(
            effects,
            [UiEffect::Rooms(RoomAsk::Join {
                room: "general chat".into(),
                key: None
            })]
        );
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Joined {
                room: "general chat".into(),
                nick: None,
            }),
        );
        let rooms = crate::ui::rooms::node(&model);
        assert_eq!(rooms.open.expect("the room opened").name, "general chat");
        // Back to the list: the joined room is a row, no longer offered.
        update(&mut model, UiEvent::Rooms(RoomsEvent::Show(None)));
        let rooms = crate::ui::rooms::node(&model);
        assert_eq!(rooms.listed.expect("the rest").rooms, ["lobby"]);
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
        assert!(row(&picked).chosen);
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
        assert_eq!(labels(&node(&model)), ["Zebra Hof"]);
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

    /// A name alone, its short hash only where two pages' names look the
    /// same (1618, the chat list's rule since 1452); "Unnamed page" never
    /// a hash, and under A-Z after the named ones, not on top.
    #[test]
    fn a_page_is_its_name_and_a_hash_only_on_a_collision() {
        let mut model = phone();
        update(
            &mut model,
            UiEvent::Browse(crate::ui::browse::BrowseEvent::Node(heard(
                0x44,
                Some("alpha relay"),
                7_200,
                None,
            ))),
        );
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let shown = labels(&node(&model));
        assert!(shown.contains(&"Zebra Hof".to_string()), "{shown:?}");
        assert!(
            shown.contains(&"Alpha Relay ·2222".to_string()),
            "{shown:?}"
        );
        assert!(
            shown.contains(&"alpha relay ·4444".to_string()),
            "{shown:?}"
        );
        assert!(shown
            .iter()
            .all(|l| l == "Unnamed page" || !l.starts_with('·')));
        explore(&mut model, ExploreEvent::Az(true));
        let az = labels(&node(&model));
        assert_eq!(
            az.last().map(String::as_str),
            Some("Unnamed page"),
            "{az:?}"
        );
        assert_eq!(
            Text::key(StrId::ExploreUnnamedPage).resolve(Lang::De),
            "Unbenannte Seite"
        );
    }

    fn order(model: &UiModel<Buffer>) -> Vec<[u8; 16]> {
        node(model)
            .pages
            .groups
            .iter()
            .flat_map(|g| g.rows.iter().map(|r| r.dest))
            .collect()
    }

    /// Rows never move under a finger (1618): a page heard while a finger
    /// is down keeps every row where it stood, and so does the next tick;
    /// the new order comes when the finger lifts, and on its own at most
    /// once a second. The walk tapped "Radio RPi4" and opened "IranTalk".
    #[test]
    fn a_row_never_moves_under_a_finger_and_the_order_settles_once_a_second() {
        let mut model = phone();
        update(&mut model, UiEvent::Elapsed(10_000));
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let before = order(&model);
        assert_eq!(before[2], [0x33; 16]);
        explore(&mut model, ExploreEvent::Touch(true));
        // The oldest page is heard again: fresh, it would come first.
        let again = |model: &mut UiModel<Buffer>| {
            update(
                model,
                UiEvent::Browse(crate::ui::browse::BrowseEvent::Node(heard(
                    0x33, None, 0, None,
                ))),
            );
        };
        again(&mut model);
        update(&mut model, UiEvent::Elapsed(12_000));
        assert_eq!(
            order(&model),
            before,
            "nothing moves while the finger is down"
        );
        // The row under the finger is the one it came down on.
        assert_eq!(picks(&model)[2], Pick::Page([0x33; 16]));
        explore(&mut model, ExploreEvent::Touch(false));
        assert_eq!(order(&model)[0], [0x33; 16], "the order comes on the lift");
        // Without a finger, a page heard within the second waits for it.
        update(
            &mut model,
            UiEvent::Browse(crate::ui::browse::BrowseEvent::Node(heard(
                0x22,
                Some("Alpha Relay"),
                0,
                Some(1),
            ))),
        );
        assert_eq!(order(&model)[0], [0x33; 16], "within the second: as it was");
        update(&mut model, UiEvent::Elapsed(13_100));
        assert_eq!(
            order(&model)[0],
            [0x22; 16],
            "a second later: the new order"
        );
        // The control: the order without the hold is the fresh one.
        let fresh: Vec<[u8; 16]> = fresh(&model).into_iter().map(|(k, _)| k.dest).collect();
        assert_eq!(order(&model), fresh);
    }

    /// A server heard once stays in the list (1655, walk 9, 44: Whatcom
    /// stood first at 17:03 and was gone at 17:11, while servers heard
    /// five hours before stayed). Twenty other shipped servers heard over
    /// the next ten minutes pushed it past the ten shipped ones the list
    /// showed; the heard ones that are not shipped were never cut.
    #[test]
    fn a_server_heard_stays_listed_while_twenty_others_are_heard_after_it() {
        let shipped = crate::ui::hubs::shipped();
        assert!(shipped.len() > 22, "the shipped list holds 68");
        let mut model = phone();
        let hear = |model: &mut UiModel<Buffer>, hub: &crate::ui::hubs::Hub, at: i64| {
            update(
                model,
                UiEvent::Rooms(RoomsEvent::Heard(crate::ui::hubs::HeardHub {
                    hash: hub.hash,
                    name: Some(hub.name.clone()),
                    hops: Some(3),
                    first: at,
                    last: at,
                    answered: None,
                })),
            );
        };
        // Another server heard before holds "suggested", which stands
        // first whatever was heard after it; the one watched is not it.
        let t0 = NOW - 600;
        hear(&mut model, &shipped[0], t0 - 60);
        let first = &shipped[1];
        hear(&mut model, first, t0);
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let listed = |model: &UiModel<Buffer>, hash: [u8; 16]| {
            node(model).rooms.hubs.iter().any(|h| h.hash == hash)
        };
        assert!(listed(&model, first.hash), "listed when heard");
        assert_eq!(suggested(&model), Some(shipped[0].hash));
        for (i, hub) in shipped[2..22].iter().enumerate() {
            hear(
                &mut model,
                hub,
                t0 + 30 * (i64::try_from(i).unwrap_or(0) + 1),
            );
        }
        assert!(
            listed(&model, first.hash),
            "still listed ten minutes and twenty servers later"
        );
        // Every one of the 22 heard is listed.
        for hub in &shipped[..22] {
            assert!(listed(&model, hub.hash), "{}", hub.name);
        }
    }

    /// 1659, walk 10, 55: the settings and Explore list the servers in one
    /// order, the one in use first; the rest by last seen.
    #[test]
    fn the_server_in_use_stands_first_in_explore_and_in_the_settings() {
        let shipped = crate::ui::hubs::shipped();
        let mut model = phone();
        for (i, hub) in shipped[..5].iter().enumerate() {
            update(
                &mut model,
                UiEvent::Rooms(RoomsEvent::Heard(crate::ui::hubs::HeardHub {
                    hash: hub.hash,
                    name: Some(hub.name.clone()),
                    hops: Some(3),
                    first: NOW - 600,
                    last: NOW - 600 + 60 * i64::try_from(i).unwrap_or(0),
                    answered: None,
                })),
            );
        }
        let chosen = shipped[1].hash;
        model.rooms.hub = Some(chosen);
        model.rooms.enabled = true;
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let explore: Vec<[u8; 16]> = node(&model).rooms.hubs.iter().map(|h| h.hash).collect();
        let settings: Vec<[u8; 16]> = crate::ui::settings::offered_hubs(&model.rooms)
            .iter()
            .map(|o| o.hash)
            .collect();
        assert_eq!(explore[0], chosen, "Explore: the one in use first");
        assert_eq!(settings[0], chosen, "settings: the one in use first");
        assert_eq!(explore[..5], settings[..5], "one order");
        // The rest by last seen: the one heard last next.
        assert_eq!(explore[1], shipped[4].hash);
    }

    /// 1659, walk 10, 54: the terminal's jumps, as the phone's chips:
    /// `Alt-2` puts the cursor on "Open rooms" under the pages, `Alt-3` on
    /// the first server, `Alt-1` back on top.
    #[test]
    fn the_terminal_jumps_to_the_rooms_and_the_servers() {
        use leviculum_ui::input::{KeyCode, KeyEvent, KeyModifiers};
        let mut model = phone();
        let hub = crate::ui::hubs::shipped()[0].hash;
        model.rooms.hub = Some(hub);
        model.rooms.enabled = true;
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let alt = |c| UiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT));
        let picks = picks(&model);
        update(&mut model, alt('2'));
        assert_eq!(picks[model.explore.cursor], Pick::OpenRooms);
        update(&mut model, alt('3'));
        assert!(matches!(picks[model.explore.cursor], Pick::Hub(_)));
        assert_eq!(picks[model.explore.cursor - 1], Pick::OpenRooms);
        update(&mut model, alt('1'));
        assert_eq!(model.explore.cursor, 0);
    }

    /// The search reaches every server Loquax offers (1655, walk 9, 44:
    /// "Colorado" and "Salon" found nothing while both stood in the
    /// shipped list, past the ten the list shows).
    #[test]
    fn the_search_finds_a_shipped_server_never_heard() {
        let shipped = crate::ui::hubs::shipped();
        let Some(colorado) = shipped.iter().find(|h| h.name == "Colorado Mesh") else {
            panic!("Colorado Mesh is shipped");
        };
        let at = shipped.iter().position(|h| h.hash == colorado.hash);
        assert!(
            at.is_some_and(|at| at >= crate::ui::settings::HUB_PAGE),
            "past the first page: {at:?}"
        );
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        explore(&mut model, ExploreEvent::Typed("Colorado".into()));
        let shown = node(&model);
        assert!(
            shown.rooms.hubs.iter().any(|h| h.hash == colorado.hash),
            "{:?}",
            shown.rooms.servers_empty
        );
        assert_eq!(shown.rooms.servers_empty, None);
    }

    /// Searching filters the servers too (1618), by their names.
    #[test]
    fn the_search_filters_the_servers() {
        let mut model = phone();
        for (byte, name) in [(0x91, "Werkstatt"), (0x92, "Bergwacht")] {
            update(
                &mut model,
                UiEvent::Rooms(RoomsEvent::Heard(crate::ui::hubs::HeardHub {
                    hash: [byte; 16],
                    name: Some(name.into()),
                    hops: Some(2),
                    first: NOW - 60,
                    last: NOW - 60,
                    answered: None,
                })),
            );
        }
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let heard = |shown: &ExploreNode| {
            shown
                .rooms
                .hubs
                .iter()
                .filter(|h| h.hash == [0x91; 16] || h.hash == [0x92; 16])
                .count()
        };
        assert_eq!(heard(&node(&model)), 2);
        explore(&mut model, ExploreEvent::Typed("werk".into()));
        let shown = node(&model);
        assert_eq!(heard(&shown), 1);
        assert!(shown.rooms.hubs.iter().all(|h| h
            .name
            .resolve(Lang::En)
            .to_lowercase()
            .contains("werk")));
        assert_eq!(shown.rooms.servers_empty, None);
        // A server's row says when it was heard, as a page's.
        let werkstatt = shown
            .rooms
            .hubs
            .iter()
            .find(|h| h.hash == [0x91; 16])
            .expect("Werkstatt");
        assert_eq!(
            werkstatt.seen,
            Some(Text::Key(StrId::ExploreSeenMinutes, vec![Arg::Int(1)]))
        );
    }

    /// Every listed server says when it was heard (1621, walk 2: the
    /// shipped ones said nothing): "not seen yet" for one never heard.
    #[test]
    fn every_server_says_when_it_was_seen() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let shown = node(&model);
        for row in &shown.rooms.hubs {
            assert_eq!(
                row.seen,
                Some(Text::key(StrId::ExploreNotSeen)),
                "nothing heard yet: {:?}",
                row.name
            );
            // No distance beside "not seen yet" (1629, walk 3 new 5).
            assert!(
                row.value
                    .as_ref()
                    .is_none_or(|v| !v.resolve(Lang::De).contains("Station")),
                "{:?}",
                row.value
            );
        }
        assert_eq!(StrId::ExploreNotSeen.text(Lang::De), "noch nicht gesehen");
    }

    /// "suggested" marks one server, the closest shipped one heard when
    /// it is made and held after (1629), or none (1621, walk 2 saw it on
    /// four); none while a server is in use.
    #[test]
    fn suggested_marks_one_server_at_most() {
        let shipped = crate::ui::hubs::shipped();
        if shipped.len() < 3 {
            return;
        }
        let mut model = phone();
        // Three shipped servers heard, at 4, 2 and 3 hops.
        for (hub, hops) in shipped.iter().take(3).zip([4u8, 2, 3]) {
            update(
                &mut model,
                UiEvent::Rooms(RoomsEvent::Heard(crate::ui::hubs::HeardHub {
                    hash: hub.hash,
                    name: Some(hub.name.clone()),
                    hops: Some(hops),
                    first: NOW - 60,
                    last: NOW - 60,
                    answered: None,
                })),
            );
        }
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let marked = |model: &UiModel<Buffer>| -> Vec<[u8; 16]> {
            node(model)
                .rooms
                .hubs
                .iter()
                .filter(|h| {
                    h.value
                        .as_ref()
                        .is_some_and(|v| v.resolve(Lang::En).contains("suggested"))
                })
                .map(|h| h.hash)
                .collect()
        };
        // One, alone: the first heard holds the mark though a closer one
        // came after it (1629, walk 3 new 6).
        assert_eq!(marked(&model), [shipped[0].hash], "the first heard, alone");
        // The control: the rule before 1621 marked every shipped one heard.
        let offered = crate::ui::settings::offered_hubs(&model.rooms);
        assert_eq!(offered.iter().filter(|o| o.shipped && o.heard).count(), 3);
        // A server in use: nothing is suggested.
        explore(&mut model, ExploreEvent::UseHub(shipped[2].hash));
        update(&mut model, UiEvent::Tab(TabId::Explore));
        assert!(marked(&model).is_empty());
    }

    /// The suggested server stands first (1632, walk 4 new 17: it stood
    /// fourth, under three closer ones heard after it), the rest in their
    /// order.
    #[test]
    fn the_suggested_server_stands_first() {
        let shipped = crate::ui::hubs::shipped();
        if shipped.len() < 4 {
            return;
        }
        let mut model = phone();
        for (hub, hops) in [
            (&shipped[3], 5u8),
            (&shipped[0], 1),
            (&shipped[1], 1),
            (&shipped[2], 2),
        ] {
            update(
                &mut model,
                UiEvent::Rooms(RoomsEvent::Heard(crate::ui::hubs::HeardHub {
                    hash: hub.hash,
                    name: Some(hub.name.clone()),
                    hops: Some(hops),
                    first: NOW - 60,
                    last: NOW - 60,
                    answered: None,
                })),
            );
        }
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let hubs: Vec<[u8; 16]> = node(&model).rooms.hubs.iter().map(|h| h.hash).collect();
        assert_eq!(hubs[0], shipped[3].hash, "the suggested one first");
        assert!(node(&model).rooms.hubs[0]
            .value
            .as_ref()
            .is_some_and(|v| v.resolve(Lang::En).contains("suggested")));
        // The rest in the settings' order: the closer ones heard after it.
        assert_eq!(
            &hubs[1..4],
            &[shipped[0].hash, shipped[1].hash, shipped[2].hash]
        );
        // The control: the settings' order alone puts it fourth.
        let offered = crate::ui::settings::offered_hubs(&model.rooms);
        let at = offered.iter().position(|o| o.hash == shipped[3].hash);
        assert_eq!(
            at,
            Some(3),
            "{:?}",
            offered.iter().map(|o| o.hops).collect::<Vec<_>>()
        );
    }

    /// "suggested" holds once made (1629, walk 3 new 6: it wandered over
    /// three servers in ten minutes): a closer server heard later does not
    /// take it; a failure to answer does. Two servers that read alike get
    /// their short address, and a nameless one is never suggested.
    #[test]
    fn suggested_holds_and_alike_servers_get_their_address() {
        let shipped = crate::ui::hubs::shipped();
        let named: Vec<_> = shipped
            .iter()
            .filter(|h| !h.name.trim().is_empty())
            .take(2)
            .cloned()
            .collect();
        if named.len() < 2 {
            return;
        }
        let hear = |model: &mut UiModel<Buffer>, hash, name: Option<&str>, hops, answered| {
            update(
                model,
                UiEvent::Rooms(RoomsEvent::Heard(crate::ui::hubs::HeardHub {
                    hash,
                    name: name.map(Into::into),
                    hops: Some(hops),
                    first: NOW - 60,
                    last: NOW - 60,
                    answered,
                })),
            );
        };
        let marked = |model: &UiModel<Buffer>| -> Vec<[u8; 16]> {
            node(model)
                .rooms
                .hubs
                .iter()
                .filter(|h| {
                    h.value
                        .as_ref()
                        .is_some_and(|v| v.resolve(Lang::En).contains("suggested"))
                })
                .map(|h| h.hash)
                .collect()
        };
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        hear(&mut model, named[0].hash, Some(&named[0].name), 4, None);
        assert_eq!(marked(&model), [named[0].hash]);
        // A closer one heard: the mark stays.
        hear(&mut model, named[1].hash, Some(&named[1].name), 2, None);
        assert_eq!(marked(&model), [named[0].hash], "the mark holds");
        // The control: the rule without the hold names the closer one.
        let offered = crate::ui::settings::offered_hubs(&model.rooms);
        assert_eq!(
            crate::ui::settings::suggested(&offered, None, None),
            Some(named[1].hash)
        );
        // The settings say the same.
        let rows = crate::ui::settings::hub_rows(&model, crate::ui::settings::Target::Terminal);
        let marked_rows: Vec<String> = rows
            .iter()
            .filter(|r| {
                r.value
                    .as_ref()
                    .is_some_and(|v| v.resolve(Lang::En).contains("suggested"))
            })
            .map(|r| r.label.resolve(Lang::En))
            .collect();
        assert_eq!(marked_rows, [named[0].name.clone()]);
        // It failed to answer: the mark moves.
        hear(&mut model, named[0].hash, None, 4, Some(false));
        assert_eq!(marked(&model), [named[1].hash]);
        // Two nameless servers, closer than any: never suggested, and
        // told apart by their short address.
        hear(&mut model, [0xb1; 16], None, 1, None);
        hear(&mut model, [0xb2; 16], None, 1, None);
        assert_eq!(marked(&model), [named[1].hash]);
        let names: Vec<String> = node(&model)
            .rooms
            .hubs
            .iter()
            .map(|h| h.name.resolve(Lang::En))
            .collect();
        let unnamed = StrId::RoomsHubUnnamed.text(Lang::En);
        assert!(names.contains(&format!("{unnamed} ·b1b1")), "{names:?}");
        assert!(names.contains(&format!("{unnamed} ·b2b2")), "{names:?}");
        assert!(names.contains(&named[1].name), "a name alone: {names:?}");
    }

    /// The sheet says what the room shows of the reader (1629, walk 3 new
    /// 9): without a name, that they appear without one; with one, that
    /// it is visible to everyone.
    #[test]
    fn the_sheet_says_what_the_room_shows_of_the_reader() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        explore(&mut model, ExploreEvent::Hub([0x95; 16]));
        let notice = |model: &UiModel<Buffer>| node(model).sheet.expect("the sheet").notice;
        assert!(model.profile.name.is_empty());
        assert_eq!(notice(&model), Text::key(StrId::RoomsNoName));
        assert_eq!(
            StrId::RoomsNoName.text(Lang::De),
            "Auf diesem Server hast du noch keinen Namen: Im Raum erscheinst du ohne Namen."
        );
        // Walk 11, finding 62: where the mailbox names the profile, the
        // notice says that name beside "no name on this server".
        model.profile.mailbox = crate::ui::profile::Mailbox::Away {
            name: Some("lew".into()),
            rung: None,
            word: None,
        };
        assert_eq!(
            notice(&model).resolve(Lang::De),
            "Auf diesem Server hast du noch keinen Namen (dein Profil heißt lew): \
             Im Raum erscheinst du ohne Namen."
        );
        model.profile.name = "Lew".into();
        assert_eq!(notice(&model), Text::key(StrId::RoomsNameVisible));
    }

    /// The sheet asks the server for its rooms (1621): "asking" while the
    /// question is out, the rooms it named once it answered, kept for the
    /// next opening without a second question; "names no rooms" only once
    /// it answered and named none; a silence is asked again.
    #[test]
    fn a_servers_sheet_asks_it_for_its_rooms_and_keeps_the_answer() {
        let mut model = phone();
        let hub = [0x95; 16];
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let asked = explore(&mut model, ExploreEvent::Hub(hub));
        assert_eq!(asked, [UiEffect::Rooms(RoomAsk::Peek { hub })]);
        let sheet = node(&model).sheet.expect("the sheet");
        assert!(sheet.rooms.is_empty());
        assert_eq!(
            sheet.rooms_note,
            Some(Text::key(StrId::ExploreServerAsking))
        );
        // The answer: two rooms.
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Peeked {
                hub,
                rooms: Some(vec!["#lobby".into(), "#radio".into()]),
            }),
        );
        let sheet = node(&model).sheet.expect("the sheet");
        assert_eq!(sheet.rooms, ["#lobby", "#radio"]);
        assert_eq!(sheet.rooms_note, None);
        assert_eq!(sheet_picks(&model), 3, "the button and two rooms");
        // Closed and opened again: shown at once, no second question.
        explore(&mut model, ExploreEvent::CloseSheet);
        assert!(explore(&mut model, ExploreEvent::Hub(hub)).is_empty());
        assert_eq!(node(&model).sheet.expect("the sheet").rooms.len(), 2);
        // Another that answers and names none: only now "names no rooms".
        let quiet = [0x96; 16];
        explore(&mut model, ExploreEvent::Hub(quiet));
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Peeked {
                hub: quiet,
                rooms: Some(Vec::new()),
            }),
        );
        // In the rooms screen's words (1655, walk 9, 47), apart from a
        // silence.
        assert_eq!(
            node(&model).sheet.expect("the sheet").rooms_note,
            Some(Text::key(StrId::RoomsNoneNamed))
        );
        assert_eq!(
            StrId::RoomsNoneNamed.text(Lang::De),
            "Dieser Server nennt keine öffentlichen Räume."
        );
        // One that never welcomed: said so, and the next opening asks again.
        let silent = [0x97; 16];
        explore(&mut model, ExploreEvent::Hub(silent));
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Peeked {
                hub: silent,
                rooms: None,
            }),
        );
        assert_eq!(
            node(&model).sheet.expect("the sheet").rooms_note,
            Some(Text::key(StrId::RoomsNoAnswer))
        );
        explore(&mut model, ExploreEvent::CloseSheet);
        assert_eq!(
            explore(&mut model, ExploreEvent::Hub(silent)),
            [UiEffect::Rooms(RoomAsk::Peek { hub: silent })]
        );
    }

    /// The server in use has a way to its rooms screen (1655, walk 9, 45:
    /// "You are in no room. Pick a server below." stood above
    /// "EuskalReticulum ✓", and its sheet had no way there but joining).
    /// The sentence names it; "Open rooms" on the section and on its
    /// sheet opens the rooms screen and connects, joining nothing.
    #[test]
    fn the_server_in_use_opens_its_rooms_without_a_join() {
        let Some(euskal) = crate::ui::hubs::shipped()
            .into_iter()
            .find(|h| h.name == "EuskalReticulum")
        else {
            panic!("EuskalReticulum is shipped");
        };
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        // Before a server is in use: the old sentence, no button.
        let shown = node(&model);
        assert_eq!(
            shown.rooms.joined_empty,
            Some(Text::key(StrId::ExploreNoJoined))
        );
        assert_eq!(shown.rooms.open_rooms, None);
        assert!(!picks(&model).contains(&Pick::OpenRooms));
        // In use, as a start reads it from the config: nothing linked yet.
        model.rooms.hub = Some(euskal.hash);
        model.rooms.enabled = true;
        let shown = node(&model);
        assert_eq!(
            shown
                .rooms
                .joined_empty
                .as_ref()
                .map(|t| t.resolve(Lang::De))
                .as_deref(),
            Some("Du bist in keinem Raum auf EuskalReticulum.")
        );
        assert_eq!(
            shown
                .rooms
                .open_rooms
                .as_ref()
                .map(|t| t.resolve(Lang::De))
                .as_deref(),
            Some("Räume öffnen")
        );
        // Its sheet says so too, and the cursor's first row is it.
        explore(&mut model, ExploreEvent::Hub(euskal.hash));
        let sheet = node(&model).sheet.expect("the sheet");
        assert!(sheet.chosen);
        assert_eq!(sheet.open_rooms, Some(Text::key(StrId::ExploreOpenRooms)));
        // Another server's sheet has none.
        explore(&mut model, ExploreEvent::Hub([0x77; 16]));
        assert_eq!(node(&model).sheet.expect("the sheet").open_rooms, None);
        explore(&mut model, ExploreEvent::CloseSheet);
        // The tap: the rooms screen in front, the link asked, no join.
        let effects = explore(&mut model, ExploreEvent::OpenRooms);
        assert!(
            effects.contains(&UiEffect::Rooms(RoomAsk::Open { hub: euskal.hash })),
            "{effects:?}"
        );
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, UiEffect::Rooms(RoomAsk::Join { .. }))),
            "{effects:?}"
        );
        assert!(model.rooms.open && model.explore.open && !model.browse.open);
        assert_eq!(model.rooms.state, HubState::Linking);
        assert_eq!(model.explore.sheet, None);
        // The terminal's Enter on the sheet's first row is the same verb.
        let mut terminal = phone();
        terminal.rooms.hub = Some(euskal.hash);
        terminal.rooms.enabled = true;
        update(&mut terminal, UiEvent::Tab(TabId::Explore));
        explore(&mut terminal, ExploreEvent::Hub(euskal.hash));
        key(
            &mut terminal,
            leviculum_ui::input::KeyEvent::new(
                leviculum_ui::input::KeyCode::Enter,
                leviculum_ui::input::KeyModifiers::NONE,
            ),
        );
        assert!(
            terminal.rooms.open,
            "Enter on the first row opened the rooms"
        );
        // Without a server in use the verb does nothing.
        let mut none = phone();
        assert!(explore(&mut none, ExploreEvent::OpenRooms).is_empty());
        assert!(!none.rooms.open);
    }

    /// One room is one row on the sheet (1655, walk 9, 46: Minuteman
    /// listed "lobby" and "#lobby"). Its answer says "lobby", the shipped
    /// list "#lobby"; the answer's spelling wins, as on the rooms screen
    /// (1652). The mailbox's rooms merge alike.
    #[test]
    fn a_room_the_answer_and_the_shipped_list_spell_apart_is_one_row() {
        let Some(minuteman) = crate::ui::hubs::shipped()
            .into_iter()
            .find(|h| h.name == "Minuteman")
        else {
            panic!("Minuteman is shipped");
        };
        assert_eq!(minuteman.rooms, ["#lobby"], "the shipped spelling");
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        explore(&mut model, ExploreEvent::Hub(minuteman.hash));
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Peeked {
                hub: minuteman.hash,
                rooms: Some(vec!["lobby".into(), "Radio".into()]),
            }),
        );
        let sheet = node(&model).sheet.expect("the sheet");
        assert_eq!(sheet.rooms, ["lobby", "Radio"]);
        assert_eq!(sheet_picks(&model), 3, "the button and two rooms");
        // Before any answer, the mailbox's spelling against the shipped one.
        let mut fresh = phone();
        fresh
            .rooms
            .mailbox_named
            .insert(minuteman.hash, vec!["lobby".into()]);
        update(&mut fresh, UiEvent::Tab(TabId::Explore));
        explore(&mut fresh, ExploreEvent::Hub(minuteman.hash));
        assert_eq!(node(&fresh).sheet.expect("the sheet").rooms, ["lobby"]);
    }

    /// 1643: the rooms the mailbox's question named, pulled with the hub,
    /// are on the sheet the moment it opens, before any link of this
    /// device's; its own answer then replaces them as the fresher copy.
    /// Control: a hub the mailbox named nothing for opens empty.
    #[test]
    fn the_mailboxs_rooms_are_on_the_sheet_before_any_link() {
        let mut model = phone();
        let hub = [0x98; 16];
        update(
            &mut model,
            UiEvent::Engine(crate::outbox::OutboxEvent::HubRooms {
                hub,
                rooms: vec!["#lobby".into(), "#radio".into()],
            }),
        );
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let asked = explore(&mut model, ExploreEvent::Hub(hub));
        assert_eq!(asked, [UiEffect::Rooms(RoomAsk::Peek { hub })]);
        let sheet = node(&model).sheet.expect("the sheet");
        assert_eq!(sheet.rooms, ["#lobby", "#radio"], "before any answer");
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Peeked {
                hub,
                rooms: Some(vec!["#lobby".into()]),
            }),
        );
        assert_eq!(node(&model).sheet.expect("the sheet").rooms, ["#lobby"]);
        explore(&mut model, ExploreEvent::CloseSheet);
        let other = [0x99; 16];
        explore(&mut model, ExploreEvent::Hub(other));
        assert!(node(&model).sheet.expect("the sheet").rooms.is_empty());
    }

    /// Walk 8 of 1653, 42: a server's sheet said "Frage den Server nach
    /// seinen Räumen…" for 90 s, the question's whole budget. After
    /// [`super::super::rooms::ASK_PATIENCE_SECS`] it says what the Rooms
    /// pane says of a silent server (1652's words) and offers to ask
    /// again; the question is asked again on that tap, the sentence back
    /// to asking, and a late answer still fills the sheet. The control:
    /// 19 s in it still asks, and an answered sheet offers no retry.
    /// Since 1659 (walk 10, 53: Minuteman answered after 45 s, the sheet
    /// had said "does not answer" at 20 s) it says the server is slow from
    /// 20 s, with the seconds, and gives up at 45 s, as the rooms screen.
    #[test]
    fn a_silent_server_is_slow_at_twenty_seconds_and_silent_at_forty_five() {
        let mut model = phone();
        let hub = [0x98; 16];
        let at = |model: &mut UiModel<Buffer>, secs: i64| {
            update(
                model,
                UiEvent::Clock(crate::when::WallTime {
                    unix_secs: secs,
                    offset_secs: 0,
                }),
            );
        };
        at(&mut model, 1_000_000);
        update(&mut model, UiEvent::Tab(TabId::Explore));
        assert_eq!(
            explore(&mut model, ExploreEvent::Hub(hub)),
            [UiEffect::Rooms(RoomAsk::Peek { hub })]
        );
        let sheet = node(&model).sheet.expect("the sheet");
        assert!(sheet.asking);
        assert_eq!(sheet.retry, None);
        // The seconds from the fifth on (1655, walk 9, 47: the sheet said
        // the same sentence for 60 to 90 s), as the rooms screen counts.
        at(&mut model, 1_000_004);
        assert_eq!(
            node(&model).sheet.expect("the sheet").rooms_note,
            Some(Text::key(StrId::ExploreServerAsking)),
            "4 s"
        );
        at(&mut model, 1_000_005);
        let note = node(&model).sheet.expect("the sheet").rooms_note;
        assert_eq!(
            note.as_ref().map(|t| t.resolve(Lang::De)).as_deref(),
            Some("Frage den Server nach seinen Räumen… 5 s"),
            "5 s"
        );
        at(&mut model, 1_000_019);
        let sheet = node(&model).sheet.expect("the sheet");
        assert_eq!(
            sheet.rooms_note,
            Some(Text::Key(StrId::ExploreServerAskingFor, vec![Arg::Int(19)])),
            "19 s"
        );
        assert_eq!(sheet.retry, None);
        at(&mut model, 1_000_020);
        let sheet = node(&model).sheet.expect("the sheet");
        assert_eq!(
            sheet
                .rooms_note
                .as_ref()
                .map(|t| t.resolve(Lang::De))
                .as_deref(),
            Some("Server antwortet langsam… 20 s"),
            "20 s: slow, not silent"
        );
        assert_eq!(sheet.retry, None, "20 s: no retry yet");
        at(&mut model, 1_000_044);
        let sheet = node(&model).sheet.expect("the sheet");
        assert_eq!(
            sheet.rooms_note,
            Some(Text::Key(StrId::RoomsSlowFor, vec![Arg::Int(44)])),
            "44 s"
        );
        assert_eq!(sheet.retry, None);
        at(&mut model, 1_000_045);
        let sheet = node(&model).sheet.expect("the sheet");
        assert_eq!(
            sheet.rooms_note,
            Some(Text::key(StrId::RoomsNoAnswer)),
            "45 s"
        );
        assert_eq!(sheet.retry, Some(Text::key(StrId::RoomsRetry)));
        assert_eq!(
            sheet
                .rooms_note
                .as_ref()
                .map(|t| t.resolve(Lang::De))
                .as_deref(),
            Some("Server antwortet nicht.")
        );
        // The retry asks again, and the sentence asks again.
        assert_eq!(
            explore(&mut model, ExploreEvent::AskAgain(hub)),
            [UiEffect::Rooms(RoomAsk::Peek { hub })]
        );
        let sheet = node(&model).sheet.expect("the sheet");
        assert_eq!(
            sheet.rooms_note,
            Some(Text::key(StrId::ExploreServerAsking))
        );
        assert_eq!(sheet.retry, None);
        // A retry for a sheet not open asks nothing.
        assert!(explore(&mut model, ExploreEvent::AskAgain([0x99; 16])).is_empty());
        // The late answer fills it, no retry.
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Peeked {
                hub,
                rooms: Some(vec!["lobby".into()]),
            }),
        );
        let sheet = node(&model).sheet.expect("the sheet");
        assert_eq!(sheet.rooms, ["lobby"]);
        assert_eq!(
            (sheet.rooms_note, sheet.retry, sheet.asking),
            (None, None, false)
        );
        // A question that ended without an answer offers the retry too.
        let silent = [0x9a; 16];
        explore(&mut model, ExploreEvent::Hub(silent));
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Peeked {
                hub: silent,
                rooms: None,
            }),
        );
        assert_eq!(
            node(&model).sheet.expect("the sheet").retry,
            Some(Text::key(StrId::RoomsRetry))
        );
    }

    fn page_text(model: &UiModel<Buffer>) -> String {
        model
            .scene(LayoutClass::Compact)
            .browse
            .expect("the browser")
            .page
            .lines
            .iter()
            .flat_map(|line| line.runs.iter().map(|run| run.text.clone()))
            .collect()
    }

    fn loaded(model: &mut UiModel<Buffer>, micron: &str) {
        update(
            model,
            UiEvent::Browse(crate::ui::browse::BrowseEvent::Loaded {
                micron: micron.into(),
                title: "the page's own title".into(),
            }),
        );
    }

    fn back(model: &mut UiModel<Buffer>) -> Vec<UiEffect> {
        update(
            model,
            UiEvent::Browse(crate::ui::browse::BrowseEvent::Intent(lnomad::Intent::Back)),
        )
    }

    /// The page view tells the truth (1621, walk 2 new 1 to 3): opening a
    /// page clears the one before at once, and the bar, the address field
    /// and the loading line say the target's name, never its hash or the
    /// page before's; the first Back returns to Explore; the in-page Back
    /// walks the pages followed from it. The sequence open, Back, Explore.
    #[test]
    fn a_page_opens_clean_under_its_name_and_back_returns_to_explore() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        explore(&mut model, ExploreEvent::OpenPage([0x11; 16]));
        loaded(&mut model, "Zebra page\n`[More`:/page/more.mu]");
        assert!(page_text(&model).contains("Zebra page"));
        // open, Back, Explore: one Back.
        back(&mut model);
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Explore);
        // Another page: nothing of the one before while it loads.
        explore(&mut model, ExploreEvent::OpenPage([0x22; 16]));
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(scene.screen, ScreenId::Browse);
        assert_eq!(scene.top.title, Text::Raw("Alpha Relay".into()));
        let browser = scene.browse.expect("the browser");
        assert!(
            !page_text(&model).contains("Zebra"),
            "{}",
            page_text(&model)
        );
        assert_eq!(browser.address.value, "Alpha Relay");
        assert_eq!(browser.top_bar.title.resolve(Lang::De), "Alpha Relay");
        assert_eq!(
            browser.status.map(|c| c.text.resolve(Lang::De)).as_deref(),
            Some("Lade Alpha Relay")
        );
        assert!(matches!(
            browser.top_bar.lead,
            leviculum_ui::scene::Lead::Back { .. }
        ));
        loaded(&mut model, "Alpha page\n`[More`:/page/more.mu]");
        let browser = model
            .scene(LayoutClass::Compact)
            .browse
            .expect("the browser");
        assert_eq!(browser.address.value, "Alpha Relay", "no hash, no path");
        assert_eq!(browser.top_bar.title.resolve(Lang::En), "Alpha Relay");
        // In-page: a link, then Back walks back to the page, then Explore.
        update(
            &mut model,
            UiEvent::Browse(crate::ui::browse::BrowseEvent::Intent(
                lnomad::Intent::Activate(lnomad::app::scene::NodeId::Link(1)),
            )),
        );
        loaded(&mut model, "More of Alpha");
        assert!(page_text(&model).contains("More of Alpha"));
        back(&mut model);
        assert!(
            page_text(&model).contains("Alpha page"),
            "{}",
            page_text(&model)
        );
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Browse);
        back(&mut model);
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Explore);
        // A page seen before opens from the cache, at once.
        explore(&mut model, ExploreEvent::OpenPage([0x11; 16]));
        assert!(page_text(&model).contains("Zebra page"));
        back(&mut model);
        // A nameless node is "Unnamed page", never its hash; Back while it
        // loads cancels the fetch and Explore is back.
        explore(&mut model, ExploreEvent::OpenPage([0x33; 16]));
        let browser = model
            .scene(LayoutClass::Compact)
            .browse
            .expect("the browser");
        assert_eq!(browser.address.value, "Unnamed page");
        assert!(page_text(&model).trim().is_empty(), "{}", page_text(&model));
        let effects = back(&mut model);
        assert!(
            effects.contains(&UiEffect::Browse(lnomad::Effect::Cancel)),
            "{effects:?}"
        );
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Explore);
    }

    /// The control for the clean open: lnomad's own navigation keeps the
    /// page before on screen while the next one loads, which is what the
    /// walk saw for 45 s.
    #[test]
    fn the_control_a_navigation_without_a_fresh_trail_keeps_the_old_page() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        explore(&mut model, ExploreEvent::OpenPage([0x11; 16]));
        loaded(&mut model, "Zebra page");
        let _ = model
            .browse
            .fold(lnomad::AppEvent::Intent(lnomad::Intent::Go {
                url: Some(format!("{}:/page/index.mu", "22".repeat(16))),
            }));
        assert!(page_text(&model).contains("Zebra page"));
    }

    /// A page opened from Explore loads on the phone (1629, walk 3 new 1).
    /// The phone's shell does what the 1621 test above never did: Explore's
    /// next screen says "browse", the activity brings the Browse pane to
    /// the front and the pane says the `tab` verb. That verb found the
    /// browser with no title (the fresh trail had cleared it) and drew the
    /// start screen through the load path, which took the fetch in flight
    /// as if the start screen were its page: no loading line, the places
    /// list under the page's name, and the start screen cached under the
    /// page's address for the next open.
    #[test]
    fn a_page_opened_from_explore_loads_when_the_phone_brings_its_tab_to_the_front() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let effects = explore(&mut model, ExploreEvent::OpenPage([0x22; 16]));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, UiEffect::Browse(lnomad::Effect::Navigate(_)))),
            "{effects:?}"
        );
        // The pane in front, as `BrowsePane.show` says it.
        let effects = update(&mut model, UiEvent::Tab(TabId::Browse));
        assert!(effects.is_empty(), "the tab asks nothing: {effects:?}");
        assert!(
            model.browse.model.is_loading(),
            "the fetch is still in flight"
        );
        let browser = model
            .scene(LayoutClass::Compact)
            .browse
            .expect("the browser");
        assert!(browser.places.is_none(), "no start view over the page");
        assert_eq!(
            browser.status.map(|c| c.text.resolve(Lang::De)).as_deref(),
            Some("Lade Alpha Relay")
        );
        assert!(page_text(&model).trim().is_empty(), "{}", page_text(&model));
        // The page arrives and is the page, under its name.
        loaded(&mut model, "Alpha page");
        assert!(
            page_text(&model).contains("Alpha page"),
            "{}",
            page_text(&model)
        );
        let browser = model
            .scene(LayoutClass::Compact)
            .browse
            .expect("the browser");
        assert!(
            browser.status.is_none(),
            "the loading line leaves with the page"
        );
        assert_eq!(browser.address.value, "Alpha Relay");
        // A second `tab` (the pane shown again) leaves the page standing.
        update(&mut model, UiEvent::Tab(TabId::Browse));
        assert!(
            page_text(&model).contains("Alpha page"),
            "{}",
            page_text(&model)
        );
        // Back to Explore and open again: the cache holds the page, not
        // the start screen.
        back(&mut model);
        explore(&mut model, ExploreEvent::OpenPage([0x22; 16]));
        update(&mut model, UiEvent::Tab(TabId::Browse));
        assert!(
            page_text(&model).contains("Alpha page"),
            "{}",
            page_text(&model)
        );
    }

    /// A page seen before opens from the cache at once and is asked for
    /// again (1629, walk 3 new 2): the line says it is the cache's while
    /// the fresh copy comes, and leaves when it is there.
    #[test]
    fn a_cached_page_says_it_is_cached_while_the_fresh_copy_comes() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        explore(&mut model, ExploreEvent::OpenPage([0x22; 16]));
        update(&mut model, UiEvent::Tab(TabId::Browse));
        loaded(&mut model, "Request Not Allowed");
        back(&mut model);
        let effects = explore(&mut model, ExploreEvent::OpenPage([0x22; 16]));
        update(&mut model, UiEvent::Tab(TabId::Browse));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, UiEffect::Browse(lnomad::Effect::Navigate(_)))),
            "{effects:?}"
        );
        assert!(page_text(&model).contains("Request Not Allowed"));
        let status = |model: &UiModel<Buffer>| {
            model
                .scene(LayoutClass::Compact)
                .browse
                .expect("the browser")
                .status
                .map(|c| c.text.resolve(Lang::De))
        };
        assert_eq!(
            status(&model).as_deref(),
            Some("aus dem Seitenspeicher · wird aktualisiert")
        );
        loaded(&mut model, "The waterfall");
        assert!(page_text(&model).contains("The waterfall"));
        assert_eq!(status(&model), None, "no line once the page is fresh");
    }

    /// The control: the tab opened on its own, with nothing shown and
    /// nothing loading, still shows the start screen and its places list.
    #[test]
    fn the_control_the_tab_opened_bare_shows_the_start_screen() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Browse));
        let browser = model
            .scene(LayoutClass::Compact)
            .browse
            .expect("the browser");
        assert!(browser.places.is_some(), "the places list over the start");
        assert!(!model.browse.model.is_loading());
    }

    /// A server's notices name its rooms as the probe read them: the word
    /// after `/join` and every `#word`, once each.
    #[test]
    fn the_notices_name_the_rooms() {
        assert_eq!(
            crate::ui::hubs::rooms_named("Welcome! Try /join lobby or #radio, #radio.\nBye #"),
            ["lobby", "#radio"]
        );
        assert!(crate::ui::hubs::rooms_named("Welcome to the hub.").is_empty());
    }

    /// A shipped server's room on its sheet joins: the server is used, and
    /// the join links, as the first join does (1577).
    #[test]
    fn a_room_on_a_servers_sheet_joins_it() {
        let Some(hub) = crate::ui::hubs::shipped()
            .into_iter()
            .find(|h| !h.rooms.is_empty())
        else {
            return;
        };
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        explore(&mut model, ExploreEvent::Hub(hub.hash));
        let sheet = node(&model).sheet.expect("the sheet");
        assert_eq!(sheet.rooms, hub.rooms);
        assert_eq!(
            sheet.rooms_note,
            Some(Text::key(StrId::ExploreServerAsking)),
            "asking while the shipped rooms show"
        );
        let room = hub.rooms[0].clone();
        let effects = explore(&mut model, ExploreEvent::JoinRoom(hub.hash, room.clone()));
        assert_eq!(model.rooms.hub, Some(hub.hash));
        assert!(
            effects.contains(&UiEffect::Rooms(RoomAsk::Open { hub: hub.hash })),
            "{effects:?}"
        );
        assert_eq!(model.scene(LayoutClass::Compact).screen, ScreenId::Rooms);
        let _ = room;
    }

    /// A server without a name is "Server without a name", never an empty
    /// row (1618: a shipped entry's name "" stood as "  · 5 Hops").
    #[test]
    fn a_nameless_server_is_said_so() {
        let mut model = phone();
        let nameless = crate::ui::hubs::shipped()
            .into_iter()
            .find(|h| h.name.trim().is_empty());
        let hash = match nameless {
            Some(h) => h.hash,
            None => [0x93; 16],
        };
        update(
            &mut model,
            UiEvent::Rooms(RoomsEvent::Heard(crate::ui::hubs::HeardHub {
                hash,
                name: Some(String::new()),
                hops: Some(5),
                first: NOW - 60,
                last: NOW - 60,
                answered: None,
            })),
        );
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let shown = node(&model);
        let row = shown
            .rooms
            .hubs
            .iter()
            .find(|h| h.hash == hash)
            .expect("the row");
        assert_eq!(row.name.resolve(Lang::De), "Server ohne Namen");
    }

    fn asks(effects: &[UiEffect]) -> usize {
        effects
            .iter()
            .filter(|e| matches!(e, UiEffect::AskPages))
            .count()
    }

    fn open_at(model: &mut UiModel<Buffer>, ms: u64) -> usize {
        update(model, UiEvent::Elapsed(ms));
        update(model, UiEvent::Tab(TabId::Chats));
        asks(&update(model, UiEvent::Tab(TabId::Explore)))
    }

    /// 1644 item 1: opening Explore asks the mailbox once, in addition to
    /// the ask after a pull, never twice within a minute; a device without
    /// a mailbox never asks.
    #[test]
    fn opening_explore_asks_the_mailbox_and_two_opens_within_a_minute_ask_once() {
        let mut model = phone();
        assert_eq!(open_at(&mut model, 1_000), 0, "no mailbox, no ask");
        explore(&mut model, ExploreEvent::Mailbox(true));
        assert_eq!(open_at(&mut model, 10_000), 1, "opened: one ask");
        assert_eq!(
            open_at(&mut model, 10_000 + ASK_EVERY_MS - 1),
            0,
            "opened again within the minute"
        );
        assert_eq!(open_at(&mut model, 10_000 + ASK_EVERY_MS), 1, "a minute on");
        // The ask after a pull counts too: the tab waits a minute from it.
        update(&mut model, UiEvent::Elapsed(200_000));
        pages_asked(&mut model);
        assert_eq!(open_at(&mut model, 230_000), 0);
        assert_eq!(open_at(&mut model, 260_000), 1);
        // The link down asks all the same: the host's ask finds out.
        explore(&mut model, ExploreEvent::Mailbox(false));
        assert_eq!(open_at(&mut model, 400_000), 1);
        // Other tabs ask nothing.
        assert_eq!(asks(&update(&mut model, UiEvent::Tab(TabId::Browse))), 0);
    }

    fn at(model: &mut UiModel<Buffer>, unix_secs: i64) {
        model.now = Some(WallTime {
            unix_secs,
            offset_secs: 0,
        });
    }

    fn answer(model: &mut UiModel<Buffer>) {
        update(
            model,
            UiEvent::Browse(crate::ui::browse::BrowseEvent::Home(
                crate::home::pages::Delta {
                    stamp: 1,
                    more: false,
                    rows: Vec::new(),
                },
            )),
        );
    }

    fn line(model: &UiModel<Buffer>, lang: Lang) -> Option<String> {
        let shown = node(model).pages.as_of;
        assert_eq!(shown, as_of(model));
        shown.map(|t| t.resolve(lang))
    }

    /// 1644 item 2: the line on top says how old the mailbox's list is:
    /// nothing while fresh, "Stand: vor 3 Std." once the last answer is
    /// older than an hour, "Nicht verbunden · Stand von gestern 14:20"
    /// while the link is down; a device without a mailbox says nothing.
    #[test]
    fn the_line_on_top_says_how_old_the_mailboxs_list_is() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Explore));
        assert_eq!(line(&model, Lang::De), None, "no mailbox");
        explore(&mut model, ExploreEvent::Mailbox(true));
        assert_eq!(line(&model, Lang::De), None, "up, no answer yet");
        // The answer comes at 14:20 of NOW's day.
        let answered = NOW - NOW.rem_euclid(86_400) + 14 * 3_600 + 20 * 60;
        at(&mut model, answered);
        answer(&mut model);
        assert_eq!(model.explore.mailbox.answered, Some(answered));
        // State one: fresh.
        assert_eq!(line(&model, Lang::De), None, "fresh");
        at(&mut model, answered + AS_OF_AFTER_SECS);
        assert_eq!(line(&model, Lang::De), None, "an hour old is fresh");
        // State two: up, the answer older than an hour.
        at(&mut model, answered + AS_OF_AFTER_SECS + 1);
        assert_eq!(line(&model, Lang::De).as_deref(), Some("Stand: vor 1 Std."));
        at(&mut model, answered + 3 * 3_600);
        assert_eq!(line(&model, Lang::De).as_deref(), Some("Stand: vor 3 Std."));
        assert_eq!(line(&model, Lang::En).as_deref(), Some("As of 3 h ago"));
        // State three: the link down, said with the answer's day and time.
        explore(&mut model, ExploreEvent::Mailbox(false));
        assert_eq!(
            line(&model, Lang::De).as_deref(),
            Some("Nicht verbunden · Stand von heute 14:20")
        );
        at(&mut model, answered + 86_400);
        assert_eq!(
            line(&model, Lang::De).as_deref(),
            Some("Nicht verbunden · Stand von gestern 14:20")
        );
        assert_eq!(
            line(&model, Lang::En).as_deref(),
            Some("Not connected · as of yesterday 14:20")
        );
        // A fresh answer and the link up: nothing again.
        answer(&mut model);
        explore(&mut model, ExploreEvent::Mailbox(true));
        assert_eq!(line(&model, Lang::De), None);
        // Down before any answer: only that it is not connected.
        let mut fresh = phone();
        explore(&mut fresh, ExploreEvent::Mailbox(false));
        assert_eq!(line(&fresh, Lang::De).as_deref(), Some("Nicht verbunden"));
    }

    fn hear_hub(model: &mut UiModel<Buffer>, hash: [u8; 16], name: &str, last: i64) {
        update(
            model,
            UiEvent::Rooms(RoomsEvent::Heard(crate::ui::hubs::HeardHub {
                hash,
                name: Some(name.into()),
                hops: Some(2),
                first: last,
                last,
                answered: None,
            })),
        );
    }

    fn listed_hub(model: &UiModel<Buffer>, hash: [u8; 16]) -> bool {
        node(model).rooms.hubs.iter().any(|h| h.hash == hash)
    }

    /// 1644 item 3: a page or a server not announced for seven days is not
    /// listed; the control, six days and 23 hours, is. The row stays, and
    /// the next announce lists it again. A bookmark and the server in use
    /// stay listed whatever their age.
    #[test]
    fn a_page_or_server_silent_for_seven_days_is_not_listed_and_six_days_23_hours_is() {
        const WEEK: i64 = 7 * 86_400;
        let mut model = phone();
        for n in [
            heard(0x66, Some("Funkstille"), WEEK, None),
            heard(0x77, Some("Knapp"), WEEK - 3_600, None),
            heard(0x88, Some("Gemerkt"), 30 * 86_400, None),
        ] {
            update(
                &mut model,
                UiEvent::Browse(crate::ui::browse::BrowseEvent::Node(n)),
            );
        }
        model.browse.model.bookmarks.add(lnomad::Bookmark {
            url: format!("{}:/page/index.mu", "88".repeat(16)),
            title: "Gemerkt".into(),
        });
        let silent = [0xa1; 16];
        let close = [0xa2; 16];
        let in_use = [0xa3; 16];
        hear_hub(&mut model, silent, "Stiller Server", NOW - WEEK);
        hear_hub(&mut model, close, "Knapper Server", NOW - WEEK + 3_600);
        hear_hub(&mut model, in_use, "Mein Server", NOW - 30 * 86_400);
        model.rooms.hub = Some(in_use);
        update(&mut model, UiEvent::Tab(TabId::Explore));
        let shown = labels(&node(&model));
        assert!(!shown.iter().any(|l| l == "Funkstille"), "{shown:?}");
        assert!(shown.iter().any(|l| l == "Knapp"), "control: {shown:?}");
        assert!(
            shown.iter().any(|l| l == "Gemerkt"),
            "a bookmark: {shown:?}"
        );
        assert!(!listed_hub(&model, silent));
        assert!(listed_hub(&model, close), "control");
        assert!(listed_hub(&model, in_use), "the server in use");
        // A search does not reach them either.
        explore(&mut model, ExploreEvent::Typed("Stille".into()));
        assert!(labels(&node(&model)).is_empty());
        assert!(!listed_hub(&model, silent));
        explore(&mut model, ExploreEvent::Typed(String::new()));
        // The rows are kept: the registry and the servers heard hold them.
        assert!(model
            .browse
            .model
            .node_registry
            .nodes()
            .iter()
            .any(|n| n.dest_hash == [0x66; 16]));
        assert!(crate::ui::settings::offered_hubs(&model.rooms)
            .iter()
            .any(|o| o.hash == silent));
        // The next announce lists them again.
        update(
            &mut model,
            UiEvent::Browse(crate::ui::browse::BrowseEvent::Node(heard(
                0x66,
                Some("Funkstille"),
                60,
                None,
            ))),
        );
        hear_hub(&mut model, silent, "Stiller Server", NOW - 60);
        update(&mut model, UiEvent::Tab(TabId::Explore));
        assert!(labels(&node(&model)).iter().any(|l| l == "Funkstille"));
        assert!(listed_hub(&model, silent));
    }
}
