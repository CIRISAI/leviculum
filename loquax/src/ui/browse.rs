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

use alloc::string::String;
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
}

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
        Browse {
            enabled: false,
            open: false,
            model,
        }
    }
}

impl Browse {
    /// The tab comes to the front. The first time, the browser has no page
    /// yet: it shows its start screen with the places panel open over it,
    /// the bookmarks and the NomadNet nodes the announces named, which is
    /// the list the terminal's browser opens on.
    pub fn show(&mut self, lang: Lang) {
        self.open = true;
        self.model.lang = lang;
        if self.model.title.is_empty() {
            show_start_screen(&mut self.model);
        }
    }

    /// The tab is on this host and in front: the browser has the keys.
    pub fn showing(&self) -> bool {
        self.enabled && self.open
    }

    /// Fold one of the browser's events, returning what the host does.
    pub fn fold(&mut self, event: AppEvent) -> Vec<Effect> {
        lnomad::update(&mut self.model, event)
    }

    /// Open the tab at `url`: a NomadNet page a chat linked to.
    pub fn go(&mut self, url: String, lang: Lang) -> Vec<Effect> {
        self.show(lang);
        self.model.show_places = false;
        self.fold(AppEvent::Intent(BrowseIntent::Go { url: Some(url) }))
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
    /// The page's area in cells, columns by rows.
    Resize { cols: u16, rows: u16 },
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
            BrowseEvent::Resize { cols, rows } => AppEvent::Resize(cols, rows),
            BrowseEvent::Address(value) => return Err(Aside::Typed(value)),
            BrowseEvent::Theme(theme) => return Err(Aside::Theme(theme)),
            BrowseEvent::Tick => AppEvent::Tick,
        })
    }
}

/// What a [`BrowseEvent`] is when it is no event of the browser's.
enum Aside {
    Identify([u8; 16], bool),
    Typed(String),
    Theme(leviculum_ui::theme::Theme),
}

/// A tab chosen: Browse comes to the front, any other sends it back.
pub(super) fn tab<B: InputBuffer + Default>(model: &mut UiModel<B>, tab: TabId) -> Vec<UiEffect> {
    match tab {
        TabId::Browse if model.browse.enabled => model.browse.show(model.lang),
        TabId::Browse => {}
        TabId::Chats | TabId::Map | TabId::Rooms => model.browse.open = false,
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
    if !model.browse.enabled && !matches!(event, BrowseEvent::Node(_)) {
        return Vec::new();
    }
    model.browse.model.lang = model.lang;
    let effects = match event.into_app() {
        Ok(event) => model.browse.fold(event),
        Err(Aside::Identify(dest, on)) => {
            model.browse.model.note_identify(dest, on);
            Vec::new()
        }
        Err(Aside::Typed(value)) => lnomad::app::type_address(&mut model.browse.model, &value),
        Err(Aside::Theme(theme)) => {
            if model.browse.model.theme != theme {
                model.browse.model.toggle_theme();
            }
            Vec::new()
        }
    };
    effects_of(model, effects)
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
    effects_of(model, effects)
}

/// The browser's effects for the shell, but the two that are Loquax's
/// own: a page's `lxmf@` link opens that chat for writing, and the
/// browser's quit leaves the tab for the chats.
pub fn effects_of<B: InputBuffer + Default>(
    model: &mut UiModel<B>,
    effects: Vec<Effect>,
) -> Vec<UiEffect> {
    let mut out = Vec::new();
    for effect in effects {
        match effect {
            Effect::Write(address) => {
                if let Ok(peer) = crate::address::parse(&address) {
                    let index = model.conversation(peer);
                    model.select(index);
                    model.focus = Pane::Compose;
                    model.browse.open = false;
                }
            }
            Effect::Quit => {
                model.browse.model.quit = false;
                model.browse.open = false;
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

    #[test]
    fn the_tab_opens_on_the_nodes_the_announces_named() {
        let mut model = phone();
        update(&mut model, UiEvent::Tab(TabId::Browse));
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(scene.screen, ScreenId::Browse);
        let tabs = scene.tabs.expect("the tab bar");
        assert_eq!(tabs.active, TabId::Browse);
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
        // The control of every test here: a host whose config leaves
        // `browse_tab` off, the default on every target.
        let mut model = UiModel::<Buffer>::new(&[]);
        update(&mut model, UiEvent::Tab(TabId::Browse));
        let scene = model.scene(LayoutClass::Compact);
        assert_eq!(scene.screen, ScreenId::Chats);
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
        assert_eq!(ids, [TabId::Chats, TabId::Map, TabId::Browse]);
        assert_eq!(
            LoquaxNode::Tab(TabId::Browse).resource(),
            "tab_browse",
            "the device walk finds it"
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
    /// another node only what is known (the control); a failure in the
    /// catalogue's words is said in the reader's language and ends the
    /// loading; a download's end is a toast.
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
        let said = model.browse.model.toast_text.clone().expect("a toast");
        assert_eq!(said, no_path);
        let toast = model.browse.model.toast.clone().expect("a toast").text;
        assert_eq!(toast, no_path.resolve(Lang::De), "said in German");

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
