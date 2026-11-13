//! The Browse tab (1337, #475): lnomad's browser hosted inside Loquax on
//! the phone and the M9 (`docs/src/concepts/lnomad-everywhere.md`,
//! section 4, Lew's decision of 2026-11-02).
//!
//! The tab is lnomad's core as it is, `no_std` plus `alloc`: its model,
//! `update`, layout, scene, intents and catalogue. Loquax adds no browser
//! of its own (section 6: "no browser intents in Loquax's `Intent`"): a
//! tap in the tab is an [`lnomad::Intent`] that Loquax carries as
//! [`super::intent::Intent::Browse`] and folds here as
//! [`super::UiEvent::Browse`]; the page the tab shows is lnomad's scene
//! ([`lnomad::app::scene::Scene`]), a window of runs from the one layout,
//! carried in Loquax's scene beside its tab bar.
//!
//! What the browser asks of the platform (a fetch, a download, the
//! clipboard) goes to the shell as [`super::UiEffect::Browse`]. Two of its
//! effects are Loquax's own and never leave the core: a page's link to its
//! operator ([`lnomad::Effect::Write`]) opens that chat, and the browser's
//! quit leaves the tab for the chats.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_ui::input::InputBuffer;
use leviculum_ui::strings::Lang;
use lnomad::app::show_start_screen;
use lnomad::app::strings::Text;
use lnomad::{AppEvent, DiscoveredNode, Effect, Intent as BrowseIntent, Model};

use super::scene::TabId;
use super::{Pane, UiEffect, UiModel};

/// The tab's state: whether it is the one shown, and the browser in it.
///
/// The browser lives as long as the model, shown or not, so a page, its
/// history and the nodes the announces named are where the reader left
/// them when the tab comes back.
#[derive(Clone, Debug)]
pub struct Browse {
    /// The host shows a Browse tab: the config's `browse_tab` on the
    /// phone, the terminal and the window (1369), the M9 by its own word
    /// (Lew, 2026-11-02). Off, the tab bar has no Browse tab and a page
    /// link in a chat is copied for lnomad as before.
    pub enabled: bool,
    /// The Browse tab is the one in front.
    pub open: bool,
    /// lnomad's model, hosted ([`Model::hosted`]).
    pub model: Model,
    /// On a device of a home (1579): the stamp of the page rows it holds
    /// and the bookmarks it shares with the home
    /// ([`crate::home::pages::Shared`]).
    pub shared: crate::home::pages::Shared,
    /// Bookmark changes the reader made, for the shell to hand the home
    /// (`/bookmark`); empty on a device without a home, whose shell never
    /// takes them.
    pub outgoing: Vec<crate::home::pages::BookmarkRow>,
    /// When the page on its way was asked for, on the browser's wall
    /// clock (1632): the loading state counts the seconds after three.
    pub loading_since: Option<u64>,
}

/// How long a page loads before its loading state says the seconds
/// (1632, walk 4 new 12: six to nine black seconds read as a hang).
pub const LOADING_SECS_AFTER: u64 = 3;

/// How many NomadNet nodes the hosted browser keeps (1578): as many as a
/// home remembers on a desktop and a phone, which have the memory, so the
/// Explore list a home hands over is whole; lnomad's own
/// [`lnomad::discovery::NODE_CAPACITY`] on the M9, whose heap is counted.
pub const REGISTRY: usize = if cfg!(feature = "std") {
    4096
} else {
    lnomad::discovery::NODE_CAPACITY
};

/// The browser's cells before the platform says its size: the M9's 53 by
/// 24 (section 2.2 of the concept page).
pub const CELLS: (u16, u16) = (53, 24);

impl Default for Browse {
    fn default() -> Self {
        let mut model = Model {
            hosted: true,
            size: CELLS,
            ..Model::default()
        };
        model.relayout(usize::from(CELLS.0).saturating_sub(1).max(1));
        model.node_registry.set_capacity(REGISTRY);
        Browse {
            enabled: false,
            open: false,
            model,
            shared: crate::home::pages::Shared::new(),
            outgoing: Vec::new(),
            loading_since: None,
        }
    }
}

impl Browse {
    /// The tab comes to the front. The first time, the browser has no page
    /// yet: it shows its start screen with the places panel open over it,
    /// the bookmarks and the NomadNet nodes the announces named, which is
    /// the list the terminal's browser opens on.
    ///
    /// A page asked for is no "first time" (1629, walk 3 new 1): Explore's
    /// open clears the title and starts the fetch, and the phone's pane
    /// says this verb right after. The start screen goes through the load
    /// path, which would take that fetch as its own: the loading line
    /// gone, the places list under the page's name, and the start screen
    /// cached under the page's address.
    pub fn show(&mut self, lang: Lang) {
        self.open = true;
        self.model.lang = lang;
        if self.model.title.is_empty() && !self.model.is_loading() {
            show_start_screen(&mut self.model);
        }
    }

    /// Start or stop the loading clock as the browser says a page is on
    /// its way or not.
    pub fn note_loading(&mut self) {
        match (self.model.is_loading(), self.loading_since) {
            (true, None) => self.loading_since = Some(self.model.wall_now()),
            (false, Some(_)) => self.loading_since = None,
            _ => {}
        }
    }

    /// The loading state's sentence (1632, walk 4 new 12): `Lade <name>
    /// …`, with the seconds after [`LOADING_SECS_AFTER`]; `None` when no
    /// page is on its way or a cached copy stands while it comes, which
    /// the status line says.
    pub fn loading_words(&self, lang: Lang) -> Option<String> {
        if !self.model.is_loading() || self.model.cached_view {
            return None;
        }
        let name = self.shown_name(lang)?;
        let secs = self
            .loading_since
            .map_or(0, |since| self.model.wall_now().saturating_sub(since));
        let text = match secs >= LOADING_SECS_AFTER {
            true => super::strings::Text::Key(
                super::strings::StrId::BrowseLoadingSecs,
                Vec::from([
                    super::strings::Arg::Str(name),
                    super::strings::Arg::Int(i64::try_from(secs).unwrap_or(i64::MAX)),
                ]),
            ),
            false => super::strings::Text::Key(
                super::strings::StrId::BrowseLoading,
                Vec::from([super::strings::Arg::Str(name)]),
            ),
        };
        Some(text.resolve(lang))
    }

    /// Fold a page that arrived. The fresh copy of the cached page on
    /// screen keeps the reader's line when its content is the cached one's
    /// (1650: the phone showed the copy, the reader scrolled, the fresh one
    /// came and threw them to the top); a page that changed starts at its
    /// top, as every other arriving page does.
    pub fn fold_loaded(&mut self, event: AppEvent) -> Vec<Effect> {
        let model = &self.model;
        let fresh_copy = model.cached_view
            && model
                .pending
                .as_ref()
                .is_some_and(|p| model.history.current() == Some(&p.target));
        let before = fresh_copy.then(|| (model.doc.clone(), model.title.clone(), model.scroll));
        let effects = self.fold(event);
        if let Some((doc, title, scroll)) = before {
            if self.model.doc == doc && self.model.title == title {
                let viewport = self.model.viewport();
                self.model.scroll = scroll.min(self.model.max_scroll(viewport));
            }
        }
        effects
    }

    /// The platform scrolled the page itself (1650, the phone's native
    /// scroll): `line` is the top line the reader sees now. The browser's
    /// [`lnomad::app::Model::scroll`] follows, so a fresh copy, a Back to
    /// this page and the paging arrows start from where the reader is.
    pub fn read_at(&mut self, line: usize) {
        let viewport = self.model.viewport();
        self.model.scroll = line.min(self.model.max_scroll(viewport));
    }

    /// The tab is on this host and in front: the browser has the keys.
    pub fn showing(&self) -> bool {
        self.enabled && self.open
    }

    /// Fold one of the browser's events, returning what the host does.
    pub fn fold(&mut self, event: AppEvent) -> Vec<Effect> {
        lnomad::update(&mut self.model, event)
    }

    /// Open the tab at `url`: a page Explore lists, or one a chat linked
    /// to. The page before and its trail go at once (1621, walk 2: the old
    /// page stood 45 s under "loading"), so the first Back from this one
    /// returns to where the reader came from; the history is the in-page
    /// Back's, from here on.
    pub fn go(&mut self, url: String, lang: Lang) -> Vec<Effect> {
        self.open = true;
        self.model.lang = lang;
        let mut effects = lnomad::app::start_trail(&mut self.model);
        effects.extend(self.fold(AppEvent::Intent(BrowseIntent::Go { url: Some(url) })));
        // A page from the cache shows at once and is asked for again
        // (1629, walk 3 new 2): a cached "Request Not Allowed" stood as if
        // it were the page. The line says it is the cache's while the
        // fresh copy comes.
        if self.model.cached_view && !self.model.is_loading() {
            effects.extend(self.fold(AppEvent::Intent(BrowseIntent::Reload)));
        }
        effects
    }

    /// The node of the page shown or loading: the fetch's while one is
    /// out, else the history's.
    pub fn shown_dest(&self) -> Option<[u8; 16]> {
        let model = &self.model;
        model
            .pending
            .as_ref()
            .map(|p| p.target.dest_hash)
            .or_else(|| model.history.current().map(|t| t.dest_hash))
    }

    /// The name the page goes by (1621, walk 2: the bar said "Pages" and
    /// the field a hash): the name its node announced, else a bookmark's
    /// title on it, else "Unnamed page", as Explore lists it; `None` on
    /// the start screen.
    pub fn shown_name(&self, lang: Lang) -> Option<String> {
        let dest = self.shown_dest()?;
        let model = &self.model;
        let announced = model
            .node_registry
            .get_by_hash(&dest)
            .and_then(|n| n.name.clone())
            .filter(|n| !n.trim().is_empty());
        let marked = || {
            model
                .bookmarks
                .list()
                .iter()
                .find(|b| {
                    lnomad::parse_url(&b.url, None).is_ok_and(|t| t.dest_hash == dest)
                        && !b.title.trim().is_empty()
                })
                .map(|b| b.title.clone())
        };
        Some(announced.or_else(marked).unwrap_or_else(|| {
            super::strings::StrId::ExploreUnnamedPage
                .text(lang)
                .to_string()
        }))
    }

    /// The browser told the name its page goes by ([`Browse::shown_name`]),
    /// for its bar, its address field and its loading line.
    pub fn relabel(&mut self, lang: Lang) {
        self.model.label = self.shown_name(lang);
    }
}

/// What the hosted browser is told, in Loquax's events: a verb, or what
/// the platform brings it. The page comes as its micron, which the core
/// parses and lays out, so no platform parses micron (section 2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowseEvent {
    /// A tap on a node of the tab, or a key's verb.
    Intent(BrowseIntent),
    /// The fetch of [`Effect::Navigate`] brought this page.
    Loaded { micron: String, title: String },
    /// The fetch failed, in the platform's words.
    Failed(String),
    /// The fetch failed, in the browser's catalogue (the fetch's own
    /// errors, `lnomad::FetchError::text`), said in the reader's language.
    FailedText(Text),
    /// The fetch's word on identifying to `dest`, read with the page it
    /// brought: it corrects the bar's optimistic toggle, and covers a node
    /// an earlier run identified to.
    Identifying { dest: [u8; 16], on: bool },
    /// The file of [`Effect::Download`] arrived and was kept as `name`, of
    /// `bytes`, where the platform keeps downloads (`dir`, for the toast).
    Downloaded {
        name: String,
        bytes: usize,
        dir: String,
    },
    /// The file of [`Effect::Download`] did not arrive or was not kept.
    DownloadFailed(Text),
    /// An announce named a NomadNet node (`nomadnetwork.node`).
    Node(DiscoveredNode),
    /// The home's `/pages` answer (1579): the nodes it heard, listed as
    /// nodes this device heard itself, and the bookmarks of the identity,
    /// applied by stamp ([`crate::home::pages::Shared::apply`]).
    Home(crate::home::pages::Delta),
    /// The page's area in cells, columns by rows.
    Resize { cols: u16, rows: u16 },
    /// The platform scrolled the page itself to this top line (1650).
    Reading(usize),
    /// The platform draws in this theme now (the window's light or dark,
    /// 1369): the page is laid out again in its accents, as lnomad's own
    /// toggle does.
    Theme(leviculum_ui::theme::Theme),
    /// The platform's own address field holds this now (the window's,
    /// 1369): what lnomad's window says on every edit
    /// ([`lnomad::app::type_address`]).
    Address(String),
    /// The shell's tick: the spinner, the toast's age.
    Tick,
}

impl BrowseEvent {
    /// The browser's event for this, but the identify word, which is no
    /// event of the browser's but a note in its model, and the address
    /// field's text, which the browser takes as the window's does.
    fn into_app(self) -> Result<AppEvent, Aside> {
        Ok(match self {
            BrowseEvent::Identifying { dest, on } => return Err(Aside::Identify(dest, on)),
            BrowseEvent::FailedText(text) => AppEvent::LoadFailedText(text),
            BrowseEvent::Downloaded { name, bytes, dir } => {
                AppEvent::DownloadDone { name, bytes, dir }
            }
            BrowseEvent::DownloadFailed(text) => AppEvent::DownloadFailed(text),
            BrowseEvent::Intent(intent) => AppEvent::Intent(intent),
            BrowseEvent::Loaded { micron, title } => AppEvent::PageLoaded {
                doc: leviculum_micron::parse(&micron),
                title,
            },
            BrowseEvent::Failed(why) => AppEvent::LoadFailed(why),
            BrowseEvent::Node(node) => AppEvent::NodeDiscovered(node),
            BrowseEvent::Home(delta) => return Err(Aside::Home(delta)),
            BrowseEvent::Resize { cols, rows } => AppEvent::Resize(cols, rows),
            BrowseEvent::Reading(line) => return Err(Aside::Reading(line)),
            BrowseEvent::Address(value) => return Err(Aside::Typed(value)),
            BrowseEvent::Theme(theme) => return Err(Aside::Theme(theme)),
            BrowseEvent::Tick => AppEvent::Tick,
        })
    }
}

/// What a [`BrowseEvent`] is when it is no event of the browser's.
enum Aside {
    Identify([u8; 16], bool),
    Reading(usize),
    Home(crate::home::pages::Delta),
    Typed(String),
    Theme(leviculum_ui::theme::Theme),
}

/// A tab chosen: Browse comes to the front, any other sends it back.
pub(super) fn tab<B: InputBuffer + Default>(model: &mut UiModel<B>, tab: TabId) -> Vec<UiEffect> {
    match tab {
        TabId::Browse if model.browse.enabled => model.browse.show(model.lang),
        TabId::Browse => {}
        TabId::Chats | TabId::Map | TabId::Rooms | TabId::Explore => model.browse.open = false,
    }
    Vec::new()
}

/// Fold an event into the hosted browser. An event while the host has no
/// Browse tab is dropped, but a node's announce, which the list keeps for
/// when the tab opens.
pub(super) fn fold<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    event: BrowseEvent,
) -> Vec<UiEffect> {
    if !model.browse.enabled && !matches!(event, BrowseEvent::Node(_) | BrowseEvent::Home(_)) {
        return Vec::new();
    }
    model.browse.model.lang = model.lang;
    if let BrowseEvent::Node(node) = &event {
        super::explore::owe_page(model, node);
    }
    let effects = match event.into_app() {
        Ok(event @ AppEvent::PageLoaded { .. }) => model.browse.fold_loaded(event),
        Ok(event) => model.browse.fold(event),
        Err(Aside::Reading(line)) => {
            model.browse.read_at(line);
            Vec::new()
        }
        Err(Aside::Identify(dest, on)) => {
            model.browse.model.note_identify(dest, on);
            Vec::new()
        }
        Err(Aside::Home(delta)) => home_pages(model, delta),
        Err(Aside::Typed(value)) => lnomad::app::type_address(&mut model.browse.model, &value),
        Err(Aside::Theme(theme)) => {
            if model.browse.model.theme != theme {
                model.browse.model.toggle_theme();
            }
            Vec::new()
        }
    };
    model.browse.relabel(model.lang);
    effects_of(model, effects)
}

/// The home's rows into the browser (1579): every node into its list, the
/// bookmarks by stamp, saved when they changed. The stamp moves to the
/// answer's. Its name rows (1643) name the peers the mailbox heard, as an
/// announce this device heard would ([`super::name_announced`]): a device
/// of an external mailbox hears no mesh of its own. A mailbox from before
/// 1643 sends none, and the names this device heard itself stay.
fn home_pages<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    delta: crate::home::pages::Delta,
) -> Vec<Effect> {
    for row in delta.names() {
        super::name_announced(model, row.address, Some(&row.name));
    }
    let browse = &mut model.browse;
    let mut effects = Vec::new();
    for node in delta.nodes() {
        effects.extend(browse.fold(AppEvent::NodeDiscovered(node.clone())));
    }
    // What the home heard stays on this device too (1632): a restart
    // out of the home's reach still lists it.
    for node in delta.nodes() {
        super::explore::owe_page(model, node);
    }
    let browse = &mut model.browse;
    if browse
        .shared
        .apply(&mut browse.model.bookmarks, delta.bookmarks())
    {
        effects.push(Effect::SaveBookmarks);
    }
    browse.shared.stamp = delta.stamp;
    super::explore::pages_answered(model);
    effects
}

/// Fold one of lnomad's own events into the hosted browser: a key or the
/// mouse while the tab is in front (1369), or what a desktop's picture
/// code says of a picture. Dropped while the host has no Browse tab.
pub fn app<B: InputBuffer + Default>(model: &mut UiModel<B>, event: AppEvent) -> Vec<UiEffect> {
    if !model.browse.enabled {
        return Vec::new();
    }
    model.browse.model.lang = model.lang;
    let effects = model.browse.fold(event);
    model.browse.relabel(model.lang);
    effects_of(model, effects)
}

/// The palette's `browse` and the terminal's `B` (1369): the tab comes to
/// the front where the host has one, and where it has none the reader is
/// told how to switch it on.
pub(super) fn open<B: InputBuffer + Default>(model: &mut UiModel<B>) -> Vec<UiEffect> {
    if model.browse.enabled {
        return tab(model, TabId::Browse);
    }
    model.toast(
        leviculum_ui::toast::ToastKind::Info,
        "the Browse tab is off; browse_tab = on in the config turns it on".into(),
    );
    Vec::new()
}

/// Open the Browse tab at the NomadNet page `url` a chat linked to.
pub(super) fn open_page<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    url: String,
) -> Vec<UiEffect> {
    let effects = model.browse.go(url, model.lang);
    model.browse.relabel(model.lang);
    effects_of(model, effects)
}

/// The browser's effects for the shell, but the two that are Loquax's
/// own: a page's `lxmf@` link opens that chat for writing, and the
/// browser's quit leaves the tab for the chats.
pub fn effects_of<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    effects: Vec<Effect>,
) -> Vec<UiEffect> {
    model.browse.note_loading();
    let mut out = Vec::new();
    for effect in effects {
        match effect {
            Effect::Write(address) => {
                if let Ok(peer) = crate::address::parse(&address) {
                    let index = model.conversation(peer);
                    model.select(index);
                    model.focus = Pane::Compose;
                    model.browse.open = false;
                    model.explore.open = false;
                }
            }
            Effect::Quit => {
                model.browse.model.quit = false;
                model.browse.open = false;
            }
            Effect::SaveBookmarks => {
                // A change the reader made goes to the home (1579); one the
                // home brought is no change here.
                let at = i64::try_from(model.browse.model.wall_now())
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1000);
                let browse = &mut model.browse;
                let rows = browse.shared.local(&browse.model.bookmarks, at);
                browse.outgoing.extend(rows);
                out.push(UiEffect::Browse(Effect::SaveBookmarks));
            }
            other => out.push(UiEffect::Browse(other)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::scene::{LayoutClass, NodeId as LoquaxNode, ScreenId};
    use crate::ui::tests::Field as Buffer;
    use crate::ui::{update, UiEvent};
    use alloc::string::ToString;
    use alloc::vec;
    use lnomad::app::scene::NodeId;
    use lnomad::Target;

    /// A NomadNet node and the reader's friend, its operator.
    const NODE: [u8; 16] = [0x5a; 16];
    const OPERATOR: [u8; 16] = [0x0e; 16];

    fn node(dest_hash: [u8; 16], name: &str) -> DiscoveredNode {
        DiscoveredNode {
            dest_hash,
            name: Some(name.to_string()),
            first_seen: 1_800_000_000,
            last_seen: 1_800_000_000,
            hops: Some(2),
        }
    }

    /// 1643: the mailbox's name rows name strangers this device never
    /// heard, by the label every list draws a peer with (chats, contacts,
    /// a new conversation), and a newer name renames the
    /// peer as an announce would. Control, a mailbox from before 1643: its
    /// answer carries no name rows, and the name this device heard itself
    /// stays.
    #[test]
    fn the_mailboxs_name_rows_name_strangers_and_an_old_mailbox_leaves_the_own() {
        use crate::home::pages::{Delta, NameRow, PageRow, Row};
        const STRANGER: [u8; 16] = [0x3c; 16];
        const HEARD: [u8; 16] = [0x4d; 16];
        let row = |address: [u8; 16], name: &str| {
            Row::Name(NameRow {
                address,
                name: name.into(),
                first_seen: 1_800_000_000,
                last_seen: 1_800_000_000,
                hops: Some(3),
            })
        };
        let mut model = phone();
        let hash_only = model.names.label(&STRANGER, Lang::En);
        assert!(!hash_only.contains("Kaja"), "{hash_only}");
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Home(Delta {
                stamp: 4,
                more: false,
                rows: vec![row(STRANGER, "Kaja")],
            })),
        );
        assert_eq!(model.names.announced(&STRANGER), Some("Kaja"));
        assert_eq!(model.names.label(&STRANGER, Lang::En), "Kaja");
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Home(Delta {
                stamp: 5,
                more: false,
                rows: vec![row(STRANGER, "Kaja W.")],
            })),
        );
        assert_eq!(model.names.label(&STRANGER, Lang::En), "Kaja W.");
        // The old mailbox: pages only. The device's own hearing stays.
        let mut old = phone();
        update(
            &mut old,
            UiEvent::Engine(crate::outbox::OutboxEvent::Announced {
                destination: HEARD,
                name: Some("Heard here".into()),
                path_response: false,
            }),
        );
        update(
            &mut old,
            UiEvent::Browse(BrowseEvent::Home(Delta {
                stamp: 2,
                more: false,
                rows: vec![Row::Page(PageRow {
                    node: node([0x7c; 16], "Relay"),
                    count: 1,
                })],
            })),
        );
        assert_eq!(old.names.label(&HEARD, Lang::En), "Heard here");
        assert_eq!(old.names.announced(&STRANGER), None);
        assert_eq!(old.names.heard_count(), 1);
    }

    /// A phone's model: the host has the tab, a node was announced.
    fn phone() -> UiModel<Buffer> {
        let mut model = UiModel::<Buffer>::new(&[]);
        model.browse.enabled = true;
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Node(node(NODE, "Marcel's node"))),
        );
        model
    }

    fn browse(model: &mut UiModel<Buffer>, intent: BrowseIntent) -> Vec<UiEffect> {
        update(model, UiEvent::Browse(BrowseEvent::Intent(intent)))
    }

    fn page(model: &mut UiModel<Buffer>, micron: &str) -> Vec<UiEffect> {
        update(
            model,
            UiEvent::Browse(BrowseEvent::Loaded {
                micron: micron.to_string(),
                title: "Marcel's node".to_string(),
            }),
        )
    }

    fn navigate(dest_hash: [u8; 16], path: &str) -> UiEffect {
        UiEffect::Browse(Effect::Navigate(Target {
            dest_hash,
            path: path.to_string(),
            fields: Vec::new(),
            is_file: false,
        }))
    }

    /// 1579: the home's answer lists its nodes beside the one this device
    /// heard, applies the identity's bookmarks and saves them, and moves
    /// the stamp; what the home brought is no change to hand back. A
    /// bookmark the reader adds is. The standalone arm: a device that never
    /// hears its home lists the nodes its own announces named, and its
    /// stamp stays 0.
    #[test]
    fn the_homes_pages_and_bookmarks_join_the_list_and_only_a_local_change_goes_back() {
        use crate::home::pages::{BookmarkRow, Delta, PageRow, Row};
        let alone = phone();
        assert_eq!(alone.browse.model.node_registry.len(), 1);
        assert_eq!(alone.browse.shared.stamp, 0);
        let mut model = phone();
        model.browse.model.clock = Some(1_800_000_100);
        let set = BookmarkRow {
            url: "7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c:/page/index.mu".into(),
            title: "Relay Süd".into(),
            at: 1_800_000_000_000,
            deleted: false,
        };
        let effects = update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Home(Delta {
                stamp: 9,
                more: false,
                rows: vec![
                    Row::Page(PageRow {
                        node: node([0x7c; 16], "Relay Süd"),
                        count: 4,
                    }),
                    Row::Bookmark(set.clone()),
                ],
            })),
        );
        assert_eq!(effects, [UiEffect::Browse(Effect::SaveBookmarks)]);
        assert_eq!(model.browse.model.node_registry.len(), 2);
        assert!(model.browse.model.bookmarks.contains(&set.url));
        assert_eq!(model.browse.shared.stamp, 9);
        assert!(model.browse.outgoing.is_empty(), "the home's own row");
        model.browse.model.bookmarks.add(lnomad::Bookmark {
            url: "5a5a:/page/index.mu".into(),
            title: "Marcel's node".into(),
        });
        let effects = effects_of(&mut model, vec![Effect::SaveBookmarks]);
        assert_eq!(effects, [UiEffect::Browse(Effect::SaveBookmarks)]);
        assert_eq!(
            model.browse.outgoing,
            [BookmarkRow {
                url: "5a5a:/page/index.mu".into(),
                title: "Marcel's node".into(),
                at: 1_800_000_100_000,
                deleted: false,
            }]
        );
        assert_eq!(model.browse.shared.pending().len(), 1);
        // A host without the tab still keeps the home's list for later.
        let mut off = UiModel::<Buffer>::new(&[]);
        update(
            &mut off,
            UiEvent::Browse(BrowseEvent::Home(Delta {
                stamp: 3,
                more: false,
                rows: vec![Row::Page(PageRow {
                    node: node(NODE, "Marcel's node"),
                    count: 1,
                })],
            })),
        );
        assert_eq!(off.browse.model.node_registry.len(), 1);
    }

    #[test]
    fn the_tab_opens_on_the_nodes_the_announces_named() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Browse));
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(scene.screen, ScreenId::Browse);
        let tabs = scene.tabs.expect("the tab bar");
        assert_eq!(
            tabs.active,
            TabId::Explore,
            "the browser is Explore's (1578)"
        );
        let browser = scene.browse.expect("the browser's scene");
        let places = browser.places.expect("the places panel");
        assert!(
            places
                .rows
                .iter()
                .any(|row| format!("{row:?}").contains("Marcel's node")),
            "the node is listed: {places:?}"
        );
        // Chats sends the tab back, and the chats' scene has no browser.
        update(&mut model, UiEvent::Tab(TabId::Chats));
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(scene.screen, ScreenId::Chats);
        assert!(scene.browse.is_none());
    }

    #[test]
    fn a_host_without_the_tab_has_none_and_drops_its_verbs() {
        // The control of every test here: a host whose config turns
        // `browse_tab` off (on is the default since 1618).
        let mut model = UiModel::<Buffer>::new(&[]);
        update(&mut model, UiEvent::Tab(TabId::Browse));
        let scene = model.scene(LayoutClass::Compact);
        // No browser: Explore's list, which says pages cannot be opened.
        assert_eq!(scene.screen, ScreenId::Explore);
        let explore = scene.explore.expect("Explore's list");
        assert!(explore.pages.off.is_some());
        let tabs = scene.tabs.expect("the tab bar");
        assert!(tabs.items.iter().all(|tab| tab.id != TabId::Browse));
        assert!(browse(&mut model, BrowseIntent::Places).is_empty());
        // An announce is kept for when a host enables the tab.
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Node(node(NODE, "Marcel's node"))),
        );
        assert_eq!(model.browse.model.node_registry.nodes().len(), 1);
    }

    #[test]
    fn the_tab_is_in_the_bar_where_the_host_has_it() {
        let model = phone();
        let tabs = model.scene(LayoutClass::Compact).tabs.expect("tabs");
        let ids: Vec<_> = tabs.items.iter().map(|tab| tab.id).collect();
        // Three buttons and no fourth (1578): the pages are Explore's.
        assert_eq!(ids, [TabId::Chats, TabId::Map, TabId::Explore]);
        assert_eq!(
            LoquaxNode::Tab(TabId::Explore).resource(),
            "tab_explore",
            "the device walk finds it"
        );
        let off = UiModel::<Buffer>::new(&[]);
        let ids: Vec<_> = off
            .scene(LayoutClass::Compact)
            .tabs
            .expect("tabs")
            .items
            .iter()
            .map(|tab| tab.id)
            .collect();
        assert_eq!(
            ids,
            [TabId::Chats, TabId::Map, TabId::Explore],
            "never gone"
        );
    }

    #[test]
    fn a_place_opens_its_node_and_the_page_comes_through_the_scene() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Browse));
        let row = lnomad::app::places(&model.browse.model)
            .iter()
            .position(
                |p| matches!(p, lnomad::app::Place::Node { dest_hash, .. } if *dest_hash == NODE),
            )
            .expect("the node's row");
        let effects = browse(&mut model, BrowseIntent::Activate(NodeId::Place(row)));
        assert_eq!(effects, vec![navigate(NODE, "/page/index.mu")]);
        page(&mut model, ">Hello\n\n`[Alpha`:/page/alpha.mu]");
        let browser = model.scene(LayoutClass::Compact).browse.expect("browser");
        let text: String = browser
            .page
            .lines
            .iter()
            .flat_map(|line| line.runs.iter().map(|run| run.text.clone()))
            .collect();
        assert!(text.contains("Hello") && text.contains("Alpha"), "{text:?}");
        assert!(browser.places.is_none(), "the page replaced the list");
        // Its link is a run that a tap follows on the same node.
        assert_eq!(
            browse(&mut model, BrowseIntent::Activate(NodeId::Link(1))),
            vec![navigate(NODE, "/page/alpha.mu")]
        );
    }

    #[test]
    fn back_goes_to_the_page_before() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Browse));
        browse(
            &mut model,
            BrowseIntent::Go {
                url: Some(format!("{}:/page/index.mu", hex(&NODE))),
            },
        );
        page(&mut model, "`[Alpha`:/page/alpha.mu]");
        browse(&mut model, BrowseIntent::Activate(NodeId::Link(1)));
        page(&mut model, "alpha");
        assert!(!page_text(&model).contains("Alpha"));
        // The page before comes from the browser's cache, no second fetch.
        assert_eq!(browse(&mut model, BrowseIntent::Back), vec![]);
        assert!(page_text(&model).contains("Alpha"), "{}", page_text(&model));
        // Forward again; with no history a back does nothing (the control).
        browse(&mut model, BrowseIntent::Forward);
        assert!(page_text(&model).contains("alpha"));
        let mut fresh = phone();
        update(&mut fresh, UiEvent::Tab(TabId::Browse));
        assert_eq!(browse(&mut fresh, BrowseIntent::Back), vec![]);
    }

    fn page_text(model: &UiModel<Buffer>) -> String {
        model
            .scene(LayoutClass::Compact)
            .browse
            .expect("browser")
            .page
            .lines
            .iter()
            .flat_map(|line| line.runs.iter().map(|run| run.text.clone()))
            .collect()
    }

    #[test]
    fn a_page_link_to_its_operator_opens_the_chat() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Browse));
        browse(
            &mut model,
            BrowseIntent::Go {
                url: Some(format!("{}:/page/index.mu", hex(&NODE))),
            },
        );
        page(
            &mut model,
            &format!("`[Write to me`lxmf@{}]", hex(&OPERATOR)),
        );
        let effects = browse(&mut model, BrowseIntent::Activate(NodeId::Link(1)));
        assert_eq!(effects, vec![], "no copy, no fetch");
        assert!(!model.browse.open, "the tab gave way to the chat");
        assert_eq!(model.conversations[model.selected].peer, OPERATOR);
        assert_eq!(model.focus, Pane::Compose);
        assert_eq!(
            model.scene(LayoutClass::Compact).screen,
            ScreenId::Chat { peer: OPERATOR }
        );
    }

    #[test]
    fn the_browsers_quit_leaves_the_tab_not_the_app() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Browse));
        assert_eq!(browse(&mut model, BrowseIntent::Quit), vec![]);
        assert!(!model.browse.open);
        assert!(!model.quit && !model.browse.model.quit);
    }

    #[test]
    fn the_page_is_laid_out_on_the_cells_the_platform_names() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Browse));
        let long = "word ".repeat(40);
        page(&mut model, &long);
        let at = |model: &UiModel<Buffer>| {
            model
                .scene(LayoutClass::Compact)
                .browse
                .expect("browser")
                .page
                .total
        };
        let narrow = at(&model);
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Resize {
                cols: 120,
                rows: 40,
            }),
        );
        assert!(at(&model) < narrow, "wider cells, fewer lines");
    }

    #[test]
    fn the_tab_speaks_the_readers_language() {
        let mut model = phone();
        model.lang = Lang::De;
        update(&mut model, UiEvent::Tab(TabId::Browse));
        assert_eq!(model.browse.model.lang, Lang::De);
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(
            scene.top.title.resolve(Lang::De),
            "Seiten",
            "Loquax's bar names the tab"
        );
    }

    /// 1337 pass two: what the phone's fetch says back. The identify
    /// word follows the page and sets the bar's flag for its node, and for
    /// another node only what is known (the control); a failure ends the
    /// loading and is said on the page in the reader's language (1669); a
    /// download's end is a toast.
    #[test]
    fn the_fetchs_words_reach_the_browser() {
        let mut model = phone();
        model.lang = Lang::De;
        update(&mut model, UiEvent::Tab(TabId::Browse));
        let effects = update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Intent(BrowseIntent::Go {
                url: Some(format!("{}:/page/index.mu", hex(&NODE))),
            })),
        );
        assert_eq!(effects, vec![navigate(NODE, "/page/index.mu")]);
        assert!(model.browse.model.pending.is_some(), "loading");
        page(&mut model, ">Hallo");
        assert!(!model.browse.model.identifying);
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Identifying {
                dest: OPERATOR,
                on: true,
            }),
        );
        assert!(!model.browse.model.identifying, "another node's word");
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Identifying {
                dest: NODE,
                on: true,
            }),
        );
        assert!(model.browse.model.identifying, "this node's word");

        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Intent(BrowseIntent::Go {
                url: Some(format!("{}:/page/next.mu", hex(&NODE))),
            })),
        );
        let no_path = lnomad::app::strings::Text::key(lnomad::app::strings::StrId::FetchNoPath);
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::FailedText(no_path.clone())),
        );
        assert!(model.browse.model.pending.is_none(), "no longer loading");
        // Said on the page, in German, not in a toast (1669, walk 12,
        // finding 69), and the page before is one Back away.
        assert!(model.browse.model.toast.is_none(), "no toast");
        assert!(
            page_text(&model).contains("Keine Verbindung zu Marcel's node."),
            "{}",
            page_text(&model)
        );
        assert_eq!(
            model
                .browse
                .model
                .history
                .current()
                .map(|t| t.path.as_str()),
            Some("/page/next.mu")
        );
        browse(&mut model, BrowseIntent::Back);
        assert!(page_text(&model).contains("Hallo"), "{}", page_text(&model));
        assert_eq!(
            model
                .browse
                .model
                .history
                .current()
                .map(|t| t.path.as_str()),
            Some("/page/index.mu")
        );

        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Downloaded {
                name: "karte.pdf".to_string(),
                bytes: 1234,
                dir: "Loquax".to_string(),
            }),
        );
        let said = model.browse.model.toast.clone().expect("a toast").text;
        assert!(
            said.contains("karte.pdf") && said.contains("Loquax"),
            "{said}"
        );
    }

    /// The tab's start page is the phone's, one sentence over the places
    /// list, and names no key; the terminal's names them (the control).
    #[test]
    fn the_tab_opens_on_the_phones_start_page() {
        let words = |model: &UiModel<Buffer>| -> String {
            model
                .scene(LayoutClass::Compact)
                .browse
                .expect("browser")
                .page
                .lines
                .iter()
                .flat_map(|line| line.runs.iter().map(|run| run.text.clone()))
                .collect()
        };
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Browse));
        let text = words(&model);
        assert!(text.contains("Tap a node"), "{text:?}");
        assert!(!text.contains("Terminal"), "{text:?}");
        let mut terminal = phone();
        terminal.browse.model.hosted = false;
        update(&mut terminal, UiEvent::Tab(TabId::Browse));
        let lines = words(&terminal);
        assert!(lines.contains("Terminal"), "the control: {lines:?}");
    }

    fn key(c: char) -> UiEvent {
        UiEvent::Key(leviculum_ui::input::KeyEvent::new(
            leviculum_ui::input::KeyCode::Char(c),
            leviculum_ui::input::KeyModifiers::NONE,
        ))
    }

    /// 1369: on the desktop the tab in front has the keys and the mouse,
    /// as lnomad's own screen does: `?` is the browser's help, its `q`
    /// leaves the tab, and with the tab behind the same keys are the
    /// messenger's again (the control).
    #[test]
    fn the_tab_in_front_has_the_keys_and_the_mouse() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Browse));
        browse(
            &mut model,
            BrowseIntent::Go {
                url: Some(format!("{}:/page/index.mu", hex(&NODE))),
            },
        );
        page(&mut model, &"line\n".repeat(80));
        // Esc closes the places list, which owns the keys while open.
        update(
            &mut model,
            UiEvent::Key(leviculum_ui::input::KeyEvent::new(
                leviculum_ui::input::KeyCode::Esc,
                leviculum_ui::input::KeyModifiers::NONE,
            )),
        );
        assert!(!model.browse.model.show_places);
        update(&mut model, key('?'));
        assert!(model.browse.model.show_help, "the browser's help");
        assert!(!model.help, "not the messenger's");
        update(&mut model, key('?'));
        assert!(!model.browse.model.show_help);
        // `/` is the page's own search.
        update(&mut model, key('/'));
        assert_eq!(model.browse.model.mode, lnomad::app::Mode::Search);
        assert!(model.search.is_none(), "not the messenger's search");
        update(
            &mut model,
            UiEvent::Key(leviculum_ui::input::KeyEvent::new(
                leviculum_ui::input::KeyCode::Esc,
                leviculum_ui::input::KeyModifiers::NONE,
            )),
        );
        let scrolled = model.browse.model.scroll;
        let wheel = leviculum_ui::input::MouseEvent {
            kind: leviculum_ui::input::MouseEventKind::ScrollDown,
            column: 1,
            row: 3,
            modifiers: leviculum_ui::input::KeyModifiers::NONE,
        };
        update(&mut model, UiEvent::Mouse(wheel));
        assert!(
            model.browse.model.scroll > scrolled,
            "the wheel scrolls the page"
        );
        update(&mut model, key('q'));
        assert!(!model.browse.open, "its quit leaves the tab");
        assert!(!model.quit);
        // Explore's list, which opened it, is in front again (1578); its
        // Esc leaves for the chats.
        assert!(crate::ui::explore::Explore::showing(&model));
        update(
            &mut model,
            UiEvent::Key(leviculum_ui::input::KeyEvent::new(
                leviculum_ui::input::KeyCode::Esc,
                leviculum_ui::input::KeyModifiers::NONE,
            )),
        );
        // Behind, `?` is the messenger's help.
        update(&mut model, key('?'));
        assert!(model.help);
        assert!(!model.browse.model.show_help);
    }

    /// The terminal's screen is the tab's page: its size reaches the
    /// browser where the host has the tab, and not where it has none.
    #[test]
    fn the_terminals_size_is_the_pages() {
        let mut model = phone();
        update(
            &mut model,
            UiEvent::Resize {
                width: 100,
                height: 30,
            },
        );
        assert_eq!(model.browse.model.size, (100, 30));
        let mut off = UiModel::<Buffer>::new(&[]);
        update(
            &mut off,
            UiEvent::Resize {
                width: 100,
                height: 30,
            },
        );
        assert_eq!(off.browse.model.size, CELLS, "the control");
    }

    /// The palette's `browse` (the terminal's `B`) brings the tab to the
    /// front where the host has it, and says how to switch it on where
    /// it has none.
    #[test]
    fn the_palettes_browse_opens_the_tab_or_says_how() {
        let mut model = phone();
        update(&mut model, UiEvent::Run(crate::ui::PaletteCommand::Browse));
        assert!(model.browse.showing());
        let mut off = UiModel::<Buffer>::new(&[]);
        update(&mut off, UiEvent::Run(crate::ui::PaletteCommand::Browse));
        assert!(!off.browse.open);
        let said = off.toasts.last().expect("a toast").text.clone();
        assert!(said.contains("browse_tab = on"), "{said}");
    }

    /// The window's address field and its light or dark (1369): the text
    /// enters the address bar as lnomad's window enters it, and a theme
    /// lays the page out again only when it differs.
    #[test]
    fn the_windows_field_and_theme_reach_the_browser() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Browse));
        update(
            &mut model,
            UiEvent::Browse(BrowseEvent::Address("5a5a:/page/x.mu".to_string())),
        );
        assert_eq!(model.browse.model.mode, lnomad::app::Mode::Address);
        assert_eq!(
            leviculum_ui::input::InputBuffer::value(&model.browse.model.input),
            "5a5a:/page/x.mu"
        );
        let other = model.browse.model.theme.toggle();
        update(&mut model, UiEvent::Browse(BrowseEvent::Theme(other)));
        assert_eq!(model.browse.model.theme, other);
        update(&mut model, UiEvent::Browse(BrowseEvent::Theme(other)));
        assert_eq!(model.browse.model.theme, other, "the same theme stays");
    }

    fn hex(bytes: &[u8; 16]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
