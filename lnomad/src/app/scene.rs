//! The browser's scene: what its screen shows and what each thing on it
//! does, as one value built from the model (1255, slice 1 of
//! `docs/src/concepts/lnomad-everywhere.md`, sections 2 and 3).
//!
//! [`Model::scene`] builds it. A renderer (the terminal's cells today, a
//! window's widgets, a phone's Views, the M9's pixels) turns the nodes into
//! its own drawing; a tap on a node raises the node's [`Action`]'s
//! [`Intent`], which [`apply`](super::intent::apply) folds. The scene
//! carries no geometry: no cell, pixel or dp is in it.
//!
//! The shared vocabulary is `leviculum_ui::scene` (slice 0): the browser
//! names its own catalogue, intents, node ids, avatar and sheet rows once,
//! on [`Lnomad`], and the shared nodes through aliases.
//!
//! **The page is a subtree of runs from the one layout** (section 2.2's
//! decision). [`Page`] is a window of [`PageLine`]s, each a list of
//! [`Run`]s: a stretch of cells with one style, one link, one field and one
//! search state. The window is the screen's rows, never the whole page, so
//! a board and a JNI crossing pay for the rows a reader sees. No renderer
//! parses or lays out micron.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use serde_json::{json, Value};
use unicode_width::UnicodeWidthChar;

use leviculum_ui::color::Ink;
pub use leviculum_ui::scene::Role;
use leviculum_ui::scene::{actions_json, Contract, Node, ToJson};
use leviculum_ui::style::RStyle;

use super::chrome::*;
use super::event::*;
use super::input::{visual_cursor, InputBuffer};
use super::intent::Intent;
use super::model::*;
use super::strings::{with, Arg, StrId, Text};
use crate::names::{conflict_text, result_text};

/// A node's stable identity: a renderer diffs on it, hit tests to it, and
/// moves the focus over it; [`NodeId::resource`] is its accessibility id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeId {
    TopBar,
    /// The address field.
    Address,
    Page,
    /// A page link, by its 1-based index.
    Link(usize),
    /// A form field, by its 1-based index.
    Field(usize),
    Chip(ChipId),
    /// The places panel.
    Places,
    /// A row of the places panel, by its index into [`places`].
    Place(usize),
    /// The name search's result list (1227).
    Names,
    /// A result, by its row.
    Name(usize),
    Toast,
}

/// The chips of the browser: the top bar's state and the status chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChipId {
    /// Identifying to the page's node.
    Identity,
    /// The page came from the page cache.
    Cached,
    /// How far the page's node is.
    Hops,
    /// What is loading, or where the focused link goes.
    Status,
}

impl NodeId {
    /// The id `uiautomator` and the device walk find the node by: lower
    /// case, digits and `_`, as an Android resource name is.
    pub fn resource(&self) -> String {
        match self {
            NodeId::TopBar => "top_bar".to_string(),
            NodeId::Address => "address".to_string(),
            NodeId::Page => "page".to_string(),
            NodeId::Link(n) => format!("link_{n}"),
            NodeId::Field(n) => format!("field_{n}"),
            NodeId::Chip(ChipId::Identity) => "chip_identity".to_string(),
            NodeId::Chip(ChipId::Cached) => "chip_cached".to_string(),
            NodeId::Chip(ChipId::Hops) => "chip_hops".to_string(),
            NodeId::Chip(ChipId::Status) => "chip_status".to_string(),
            NodeId::Places => "places".to_string(),
            NodeId::Place(n) => format!("place_{n}"),
            NodeId::Names => "names".to_string(),
            NodeId::Name(n) => format!("name_{n}"),
            NodeId::Toast => "toast".to_string(),
        }
    }
}

impl Node for NodeId {
    fn resource(&self) -> String {
        NodeId::resource(self)
    }
}

/// What leads the browser's top bar: the reader's own identity, or the
/// node of the page a back step leaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Badge {
    /// The reader: the first eight hex digits of lnomad's identity, empty
    /// while the shell has not named it.
    Me { fingerprint: String },
    /// The page's node, none before the first page.
    Site { dest: Option<[u8; 16]> },
}

impl ToJson for Badge {
    fn to_json(&self) -> Value {
        match self {
            Badge::Me { fingerprint } => json!({ "me": fingerprint }),
            Badge::Site { dest } => json!({ "site": dest.as_ref().map(hex) }),
        }
    }
}

/// lnomad's names in the shared scene vocabulary (`leviculum_ui::scene`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lnomad;

impl Contract for Lnomad {
    type Key = StrId;
    type Intent = Intent;
    type Node = NodeId;
    /// The standalone browser has no tab bar (section 3, "Tab bar").
    type Tab = ();
    type Avatar = Badge;
    type Row = Row;
}

/// What a screen reader and `uiautomator` need of a node.
pub type A11y = leviculum_ui::scene::A11y<StrId>;

/// Something a node does: the intent a tap raises, whether it may now.
pub type Action = leviculum_ui::scene::Action<Intent>;

/// What leads the top bar.
pub type Lead = leviculum_ui::scene::Lead<Lnomad>;

/// The bar at the top: the page's title, back, forward, reload, the star,
/// identify.
pub type TopBar = leviculum_ui::scene::TopBar<Lnomad>;

/// A row's generic part: its id, whether it is the chosen one, its tap and
/// its menu.
pub type ListItem = leviculum_ui::scene::ListItem<Lnomad>;

/// A sheet over the page: the places panel, the name results.
pub type Sheet = leviculum_ui::scene::Sheet<Lnomad>;

/// A transient state.
pub type Chip = leviculum_ui::scene::Chip<Lnomad>;

/// A row of one of the browser's sheets.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// A section's title: "Bookmarks", "Discovered nodes".
    Heading(Text),
    /// What an empty section says.
    Placeholder(Text),
    /// A place or a result: what a tap opens, and its words.
    Item {
        item: ListItem,
        title: Text,
        /// The pointer is over it.
        hovered: bool,
    },
    /// A line under the rows: the result list's key hint, or the question
    /// it asks before opening a name pinned to another key.
    Note { text: Text, ask: bool },
}

impl Row {
    pub fn to_json(&self) -> Value {
        match self {
            Row::Heading(text) => json!({ "heading": text.to_json() }),
            Row::Placeholder(text) => json!({ "placeholder": text.to_json() }),
            Row::Item {
                item,
                title,
                hovered,
            } => json!({ "item": {
                "id": item.id.resource(),
                "selected": item.selected,
                "hovered": hovered,
                "activate": item.activate.to_json(),
                "menu": actions_json(&item.menu),
                "title": title.to_json(),
                "a11y": item.a11y.to_json(),
            }}),
            Row::Note { text, ask } => json!({ "note": { "text": text.to_json(), "ask": ask } }),
        }
    }
}

/// The address field: the platform's text field, the scene names what it
/// holds, its hint and its go (section 2.1).
#[derive(Clone, Debug, PartialEq)]
pub struct AddressField {
    pub id: NodeId,
    /// The page's address (`node…:/path`) while not edited, what is typed
    /// while it is; empty before the first page.
    pub value: String,
    /// What it says while it holds nothing.
    pub hint: Text,
    /// The reader is typing into it.
    pub editing: bool,
    /// Where the caret is while editing, in characters.
    pub cursor: Option<usize>,
    /// A tap on it while not editing: edit it.
    pub activate: Action,
    /// Enter while editing.
    pub go: Action,
    pub a11y: A11y,
}

/// A stretch of a page line's cells with one style, one link, one field and
/// one search state: what a renderer draws as one span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub style: RStyle,
    /// The 1-based link it is part of.
    pub link: Option<usize>,
    /// The 1-based form field it is part of.
    pub field: Option<usize>,
    /// Inside a search match: `Some(true)` for the current one.
    pub matched: Option<bool>,
}

/// One laid-out line of the page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageLine {
    pub runs: Vec<Run>,
}

/// The page: the window of laid-out lines the screen shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub id: NodeId,
    pub lines: Vec<PageLine>,
    /// The page line the window starts at.
    pub first: usize,
    /// The page's lines in all.
    pub total: usize,
    /// The focused link or field.
    pub focus: Option<Focus>,
    pub a11y: A11y,
}

/// A toast in the catalogue's words.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneToast {
    pub kind: ToastKind,
    pub text: Text,
}

/// What the browser's screen shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub top_bar: TopBar,
    pub address: AddressField,
    /// The top bar's state, in the order it reads: identity, cache, hops.
    pub state: Vec<Chip>,
    pub page: Page,
    /// What is loading, or where the focused link goes.
    pub status: Option<Chip>,
    pub places: Option<Sheet>,
    pub names: Option<Sheet>,
    pub toast: Option<SceneToast>,
    /// The help is open: the renderer draws the key rows.
    pub help: bool,
}

impl Model {
    /// The scene of the model as it is now.
    pub fn scene(&self) -> Scene {
        Scene {
            top_bar: top_bar(self),
            address: address(self),
            state: state(self),
            page: page(self),
            status: status(self),
            places: self.show_places.then(|| places_sheet(self)),
            names: self.names.as_ref().map(|r| names_sheet(self, r)),
            toast: self.toast.as_ref().map(|toast| SceneToast {
                kind: toast.kind,
                text: self
                    .toast_text
                    .clone()
                    .unwrap_or_else(|| Text::Raw(toast.text.clone())),
            }),
            help: self.show_help,
        }
    }
}

pub(crate) fn top_bar(model: &Model) -> TopBar {
    // Hosted, Back is always there (1621): at the trail's start it gives
    // the page back to the host (`leave_host`).
    let lead = match model.history.can_back() || model.hosted {
        true => Lead::Back {
            back: Action::of(Intent::Back),
            avatar: Badge::Site {
                dest: model.current_dest,
            },
        },
        false => Lead::Me {
            avatar: Badge::Me {
                fingerprint: model.fingerprint.chars().take(8).collect(),
            },
            link: None,
            open: None,
        },
    };
    let loaded = model.history.current().is_some();
    let mut actions = vec![
        Action {
            enabled: model.history.can_forward(),
            ..Action::of(Intent::Forward)
        },
        Action {
            enabled: loaded,
            ..Action::of(Intent::Reload)
        },
    ];
    if let Some(url) = current_url(model) {
        actions.push(Action::of(Intent::Bookmark {
            on: !model.bookmarks.contains(&url),
        }));
    }
    if model.current_dest.is_some() {
        actions.push(Action::of(Intent::Identify {
            on: !model.identifying,
        }));
    }
    let title = match &model.label {
        Some(label) => label.trim().to_string(),
        None => model.title.trim().to_string(),
    };
    let label = match title.is_empty() {
        true => Text::key(StrId::Page),
        false => Text::Raw(title.clone()),
    };
    TopBar {
        id: NodeId::TopBar,
        lead,
        title: Text::Raw(title),
        actions,
        a11y: A11y::of(NodeId::TopBar, Role::Bar, label),
    }
}

pub(crate) fn address(model: &Model) -> AddressField {
    let editing = model.mode == Mode::Address;
    let shown = model.history.current().is_some() || model.pending.is_some();
    let value = match (editing, &model.label, model.history.current()) {
        (true, _, _) => InputBuffer::value(&model.input).to_string(),
        // The host's name for the page, from the moment it is asked (1621).
        (false, Some(label), _) if shown => label.clone(),
        (false, _, Some(_)) => breadcrumb(model),
        (false, _, None) => String::new(),
    };
    AddressField {
        id: NodeId::Address,
        value,
        hint: Text::key(StrId::AddressHint),
        editing,
        cursor: editing.then(|| visual_cursor(&model.input)),
        activate: Action::of(Intent::Activate(NodeId::Address)),
        go: Action::of(Intent::Go { url: None }),
        a11y: A11y {
            selected: editing,
            ..A11y::of(NodeId::Address, Role::TextField, Text::key(StrId::Address))
        },
    }
}

fn chip(id: ChipId, text: Text) -> Chip {
    Chip {
        id: NodeId::Chip(id),
        a11y: A11y::of(NodeId::Chip(id), Role::Chip, text.clone()),
        text,
    }
}

pub(crate) fn state(model: &Model) -> Vec<Chip> {
    let mut out = Vec::new();
    if model.identifying && !model.fingerprint.is_empty() {
        let fp8: String = model.fingerprint.chars().take(8).collect();
        out.push(chip(ChipId::Identity, with(StrId::Identity, fp8)));
    }
    if model.cached_view {
        out.push(chip(ChipId::Cached, Text::key(StrId::Cached)));
    }
    if let Some(dest) = model.current_dest {
        let text = match model.node_registry.get_by_hash(&dest).and_then(|n| n.hops) {
            Some(h) => Text::Key(StrId::Hops, vec![Arg::Int(h.into())]),
            None => Text::key(StrId::HopsUnknown),
        };
        out.push(chip(ChipId::Hops, text));
    }
    out
}

fn status(model: &Model) -> Option<Chip> {
    let text = if model.pending.is_some() && model.cached_view {
        // The cached page stands while its fresh copy comes (1629, walk 3
        // new 2: "loading" under a page read as if the page were not there).
        Text::key(StrId::CachedRefreshing)
    } else if let Some(pending) = &model.pending {
        let what = model
            .label
            .clone()
            .unwrap_or_else(|| pending.target.path.clone());
        with(StrId::Loading, what)
    } else if let Some(name) = &model.download {
        with(StrId::Downloading, name.clone())
    } else if let Some(target) = focused_link_target(model) {
        with(StrId::LinkTarget, target)
    } else if model.hosted && model.cached_view {
        // A host's line is the only state it draws (the phone has no chips).
        Text::key(StrId::Cached)
    } else {
        return None;
    };
    Some(chip(ChipId::Status, text))
}

/// The window of the page the screen shows, as runs: the viewport's lines
/// from the scroll, or for a host that scrolls natively
/// ([`Model::window`]) the whole page up to its cap.
fn page(model: &Model) -> Page {
    let total = model.page.len();
    let (first, end) = window(model, total);
    let lines = (first..end).map(|at| page_line(model, at)).collect();
    Page {
        id: NodeId::Page,
        lines,
        first,
        total,
        focus: current_focus(model),
        a11y: A11y::of(
            NodeId::Page,
            Role::Page,
            match model.title.trim() {
                "" => Text::key(StrId::Page),
                title => Text::Raw(title.to_string()),
            },
        ),
    }
}

/// The lines `[first, end)` of a page `total` lines long the scene carries.
fn window(model: &Model, total: usize) -> (usize, usize) {
    match model.window {
        None => {
            let end = model.scroll.saturating_add(model.viewport()).min(total);
            (model.scroll.min(end), end)
        }
        Some(cap) if total <= cap => (0, total),
        // A page longer than the cap: a window around the reader's line,
        // so the native scroll reaches both ways from it.
        Some(cap) => {
            let first = model
                .scroll
                .saturating_sub(cap / 2)
                .min(total.saturating_sub(cap));
            (first, first.saturating_add(cap).min(total))
        }
    }
}

/// Page line `at` as runs: a new run where the style, the link, the field
/// or the search state changes.
fn page_line(model: &Model, at: usize) -> PageLine {
    let matches: Vec<(usize, &Match)> = model
        .matches
        .iter()
        .enumerate()
        .filter(|(_, m)| m.line_idx == at)
        .collect();
    let mut runs: Vec<Run> = Vec::new();
    let mut col = 0usize;
    for cell in &model.page[at].cells {
        let matched = matches
            .iter()
            .find(|(_, m)| (m.col_start..m.col_end).contains(&col))
            .map(|(i, _)| model.current_match == Some(*i));
        col += UnicodeWidthChar::width(cell.ch).unwrap_or(0);
        match runs.last_mut() {
            Some(run)
                if run.style == cell.st
                    && run.link == cell.link
                    && run.field == cell.field
                    && run.matched == matched =>
            {
                run.text.push(cell.ch)
            }
            _ => runs.push(Run {
                text: cell.ch.to_string(),
                style: cell.st,
                link: cell.link,
                field: cell.field,
                matched,
            }),
        }
    }
    PageLine { runs }
}

/// The places panel: the bookmarks, then the discovered nodes.
fn places_sheet(model: &Model) -> Sheet {
    let entries = places(model);
    let marks = model.bookmarks.len();
    let now = model.wall_now();
    let item = |i: usize, place: &Place| {
        let title = place_title(place, now, place_collides(&entries, i));
        let menu = match place {
            Place::Bookmark { .. } => vec![Action::of(Intent::Forget(i))],
            Place::Node { .. } => Vec::new(),
        };
        Row::Item {
            item: ListItem {
                id: NodeId::Place(i),
                selected: i == model.places_sel,
                activate: Action::of(Intent::Activate(NodeId::Place(i))),
                menu,
                a11y: A11y {
                    selected: i == model.places_sel,
                    ..A11y::of(NodeId::Place(i), Role::Row, title.clone())
                },
            },
            title,
            hovered: model.places_hover == Some(i),
        }
    };
    let mut rows = vec![Row::Heading(Text::key(StrId::Bookmarks))];
    match marks {
        0 => rows.push(Row::Placeholder(Text::key(StrId::NoBookmarks))),
        _ => rows.extend(
            entries
                .iter()
                .enumerate()
                .take(marks)
                .map(|(i, p)| item(i, p)),
        ),
    }
    rows.push(Row::Heading(Text::key(StrId::Nodes)));
    match entries.len() == marks {
        // Discovery always runs: an empty list means nothing announced yet.
        true => rows.push(Row::Placeholder(Text::key(StrId::NoNodesYet))),
        false => rows.extend(
            entries
                .iter()
                .enumerate()
                .skip(marks)
                .map(|(i, p)| item(i, p)),
        ),
    }
    Sheet {
        id: NodeId::Places,
        title: Text::key(StrId::PlacesTitle),
        rows,
        actions: vec![Action::of(Intent::Undo), Action::of(Intent::Dismiss)],
        a11y: A11y::of(NodeId::Places, Role::Sheet, Text::key(StrId::PlacesTitle)),
    }
}

/// A place's words: a bookmark's title and URL; a heard node's name, when
/// it was heard and how far it is (1632, walk 4: no hash, and a hop count
/// no list can mean said as nothing). `tail` adds the short hash after the
/// name, for a name another row shows too.
pub(crate) fn place_title(place: &Place, now: u64, tail: bool) -> Text {
    match place {
        Place::Bookmark { url, title } if title.is_empty() => Text::Raw(url.clone()),
        Place::Bookmark { url, title } => Text::Key(
            StrId::PlaceBookmark,
            vec![Arg::Str(title.clone()), Arg::Str(url.clone())],
        ),
        Place::Node {
            dest_hash,
            hops,
            last_seen,
            ..
        } => {
            let label = node_label(place).unwrap_or_default();
            let label = match (label.is_empty(), tail) {
                (true, false) => Text::key(StrId::PlaceUnnamed),
                (true, true) => Text::Key(
                    StrId::PlaceUnnamedTail,
                    vec![Arg::Str(crate::names::hash4(dest_hash))],
                ),
                (false, false) => Text::Raw(label),
                (false, true) => Text::Raw(format!("{label} ·{}", crate::names::hash4(dest_hash))),
            };
            let seen = place_seen(now, *last_seen);
            match crate::discovery::shown_hops(*hops) {
                Some(h) => Text::Key(
                    StrId::PlaceNode,
                    vec![
                        Arg::Text(Box::new(label)),
                        Arg::Text(Box::new(seen)),
                        Arg::Text(Box::new(match h {
                            0 | 1 => Text::key(StrId::PlaceHop),
                            n => Text::Key(StrId::PlaceHops, vec![Arg::Int(n.into())]),
                        })),
                    ],
                ),
                None => Text::Key(
                    StrId::PlaceNodeSeen,
                    vec![Arg::Text(Box::new(label)), Arg::Text(Box::new(seen))],
                ),
            }
        }
    }
}

/// A node's announced name, trimmed; `None` for a bookmark.
fn node_label(place: &Place) -> Option<String> {
    match place {
        Place::Node { name, .. } => Some(name.as_deref().unwrap_or("").trim().to_string()),
        Place::Bookmark { .. } => None,
    }
}

/// Whether the node at `i` shares its shown name with another node row.
fn place_collides(entries: &[Place], i: usize) -> bool {
    let Some(mine) = entries.get(i).and_then(node_label) else {
        return false;
    };
    entries
        .iter()
        .enumerate()
        .any(|(j, p)| j != i && node_label(p).as_deref() == Some(mine.as_str()))
}

/// "seen 3 min ago", in the words Explore's page list uses.
fn place_seen(now: u64, then: u64) -> Text {
    let secs = now.saturating_sub(then);
    let n = |id: StrId, unit: u64| {
        Text::Key(
            id,
            vec![Arg::Int(i64::try_from(secs / unit).unwrap_or(i64::MAX))],
        )
    };
    match secs {
        0..=59 => Text::key(StrId::PlaceSeenNow),
        60..=3599 => n(StrId::PlaceSeenMinutes, 60),
        3600..=86_399 => n(StrId::PlaceSeenHours, 3600),
        _ => n(StrId::PlaceSeenDays, 86_400),
    }
}

/// The name search's results (1227) as a list of rows, and the line under
/// them.
fn names_sheet(model: &Model, results: &NameResults) -> Sheet {
    let now = model.wall_now();
    let mut rows: Vec<Row> = results
        .hits
        .iter()
        .enumerate()
        .map(|(i, hit)| {
            let title = result_text(hit, &model.pins, now);
            Row::Item {
                item: ListItem {
                    id: NodeId::Name(i),
                    selected: i == results.sel,
                    activate: Action::of(Intent::Activate(NodeId::Name(i))),
                    menu: Vec::new(),
                    a11y: A11y {
                        selected: i == results.sel,
                        ..A11y::of(NodeId::Name(i), Role::Row, title.clone())
                    },
                },
                title,
                hovered: false,
            }
        })
        .collect();
    rows.push(match results.hits.get(results.sel) {
        Some(hit) if results.confirm => Row::Note {
            text: conflict_text(hit, &model.pins),
            ask: true,
        },
        _ => Row::Note {
            text: Text::key(StrId::NamesHint),
            ask: false,
        },
    });
    let title = with(StrId::NamesTitle, results.query.clone());
    Sheet {
        id: NodeId::Names,
        title: title.clone(),
        rows,
        actions: vec![Action::of(Intent::Dismiss)],
        a11y: A11y::of(NodeId::Names, Role::Sheet, title),
    }
}

fn hex(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn ink_json(ink: Option<Ink>) -> Value {
    match ink {
        None => Value::Null,
        Some(Ink::Rgb(r, g, b)) => json!(format!("#{r:02x}{g:02x}{b:02x}")),
        Some(Ink::Indexed(n)) => json!(n),
    }
}

fn style_json(st: &RStyle) -> Value {
    json!({
        "fg": ink_json(st.fg),
        "bg": ink_json(st.bg),
        "bold": st.bold,
        "underline": st.underline,
        "italic": st.italic,
        "reverse": st.reverse,
    })
}

fn focus_json(focus: Option<Focus>) -> Value {
    match focus {
        None => Value::Null,
        Some(Focus::Link(n)) => json!({ "link": n }),
        Some(Focus::Field(n)) => json!({ "field": n }),
    }
}

fn chip_json(chip: &Chip) -> Value {
    json!({
        "id": chip.id.resource(),
        "text": chip.text.to_json(),
        "a11y": chip.a11y.to_json(),
    })
}

fn sheet_json(sheet: &Sheet) -> Value {
    json!({
        "id": sheet.id.resource(),
        "title": sheet.title.to_json(),
        "rows": sheet.rows.iter().map(Row::to_json).collect::<Vec<_>>(),
        "actions": actions_json(&sheet.actions),
        "a11y": sheet.a11y.to_json(),
    })
}

impl Run {
    pub fn to_json(&self) -> Value {
        json!({
            "text": self.text,
            "style": style_json(&self.style),
            "link": self.link,
            "field": self.field,
            "matched": self.matched,
        })
    }
}

impl Page {
    /// The runs in the window.
    pub fn runs(&self) -> usize {
        self.lines.iter().map(|line| line.runs.len()).sum()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id.resource(),
            "first": self.first,
            "total": self.total,
            "focus": focus_json(self.focus),
            "lines": self.lines.iter().map(|line| {
                Value::Array(line.runs.iter().map(Run::to_json).collect())
            }).collect::<Vec<_>>(),
            "a11y": self.a11y.to_json(),
        })
    }
}

impl AddressField {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id.resource(),
            "value": self.value,
            "hint": self.hint.to_json(),
            "editing": self.editing,
            "cursor": self.cursor,
            "activate": self.activate.to_json(),
            "go": self.go.to_json(),
            "a11y": self.a11y.to_json(),
        })
    }
}

impl Scene {
    /// The scene as JSON for a golden file: keys sorted, texts as catalogue
    /// keys with their arguments, so one file pins both languages.
    pub fn to_json(&self) -> Value {
        json!({
            "top_bar": self.top_bar.to_json(),
            "address": self.address.to_json(),
            "state": self.state.iter().map(chip_json).collect::<Vec<_>>(),
            "page": self.page.to_json(),
            "status": self.status.as_ref().map(chip_json),
            "places": self.places.as_ref().map(sheet_json),
            "names": self.names.as_ref().map(sheet_json),
            "toast": self.toast.as_ref().map(|t| json!({
                "kind": format!("{:?}", t.kind),
                "text": t.text.to_json(),
            })),
            "help": self.help,
        })
    }
}

#[cfg(test)]
mod place_words {
    use super::*;
    use crate::app::strings::Lang;

    fn node(byte: u8, name: Option<&str>, hops: Option<u32>, last_seen: u64) -> Place {
        Place::Node {
            dest_hash: [byte; 16],
            name: name.map(str::to_string),
            hops,
            last_seen,
        }
    }

    /// Walk 4 of 1632, new 14: the places sheet said "RNS Search ·
    /// 3e5f19fd… · 110 Sprünge". A node is its name, when it was heard and
    /// how far it is; a hop count no reader can use says nothing; a
    /// nameless node is "Unbenannte Seite", with the short hash only when
    /// another row reads the same.
    #[test]
    fn a_heard_node_is_its_name_and_never_110_stations() {
        let now = 10_000;
        let far = node(0x3e, Some("RNS Search"), Some(110), now - 180);
        let text = place_title(&far, now, false).resolve(Lang::De);
        assert_eq!(text, "RNS Search  ·  gesehen vor 3 Min.");
        let near = node(0x3e, Some("NL-WB Backbone"), Some(5), now - 30);
        assert_eq!(
            place_title(&near, now, false).resolve(Lang::De),
            "NL-WB Backbone  ·  gesehen gerade eben  ·  über 5 Stationen erreichbar"
        );
        assert!(!place_title(&near, now, false)
            .resolve(Lang::De)
            .contains("3e3e"));
        // The edge: 32 shows, 33 does not.
        let edge = |h| place_title(&node(1, Some("x"), Some(h), now), now, false).resolve(Lang::En);
        assert!(edge(32).contains("32 stations"), "{}", edge(32));
        assert!(!edge(33).contains("stations"), "{}", edge(33));
        // Nameless: the word, and the tail only on a collision.
        let entries = [
            node(0xab, None, Some(2), now),
            node(0xcd, Some(" "), None, now),
            node(0xef, Some("Solo"), None, now),
        ];
        assert!(place_collides(&entries, 0));
        assert!(!place_collides(&entries, 2));
        assert_eq!(
            place_title(&entries[0], now, place_collides(&entries, 0)).resolve(Lang::De),
            "Unbenannte Seite ·abab  ·  gesehen gerade eben  ·  über 2 Stationen erreichbar"
        );
        assert_eq!(
            place_title(&entries[2], now, false).resolve(Lang::En),
            "Solo  ·  seen just now"
        );
        let alone = [node(0xab, None, None, now)];
        assert_eq!(
            place_title(&alone[0], now, place_collides(&alone, 0)).resolve(Lang::De),
            "Unbenannte Seite  ·  gesehen gerade eben"
        );
    }

    /// Walk 5 of 1639, new 24: the sheet listed the heard pages oldest
    /// first, Explore newest first. The sheet now lists them as Explore
    /// does, the one heard last first; a page heard again moves up.
    #[test]
    fn heard_pages_stand_newest_first() {
        let doc = leviculum_micron::parse("x");
        let mut model = Model::from_document(&doc, 79, "x", (80, 24));
        for (byte, seen) in [(1u8, 100u64), (2, 300), (3, 200)] {
            model
                .node_registry
                .upsert([byte; 16], Some(format!("n{byte}")), Some(1), seen);
        }
        let order = |model: &Model| -> Vec<u8> {
            crate::app::event::places(model)
                .iter()
                .filter_map(|p| match p {
                    Place::Node { dest_hash, .. } => Some(dest_hash[0]),
                    Place::Bookmark { .. } => None,
                })
                .collect()
        };
        assert_eq!(order(&model), [2, 3, 1]);
        model
            .node_registry
            .upsert([1; 16], Some("n1".into()), Some(1), 400);
        assert_eq!(order(&model), [1, 2, 3], "heard again, it stands first");
    }
}
