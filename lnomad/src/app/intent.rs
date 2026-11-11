//! The browser's verbs as one enum (1255, slice 1 of
//! `docs/src/concepts/lnomad-everywhere.md`, its section 2.1): what a key,
//! a click, a tap or a host's facade asks the browser to do.
//!
//! The browser's intents are lnomad's, not arms of Loquax's `Intent`: a
//! host that carries the browser as a tab wraps this enum
//! (`Browse(lnomad::Intent)`, section 4 of the page). A scene node's
//! [`Action`](super::scene::Action) carries the intent a tap on it raises,
//! and [`apply`] folds one into the model through the same functions the
//! keys reach, so a tap and a key cannot do two different things.
//!
//! | Today's spelling | Intent |
//! |---|---|
//! | Enter in the address editor | `Go { url: None }` |
//! | `:`, a click on the address | `Activate(NodeId::Address)` |
//! | `Esc`, `Ctrl-g` while a page loads | `Cancel` |
//! | `Alt-←`, the footer's back | `Back` |
//! | `Alt-→`, the footer's forward | `Forward` |
//! | `R`, `Ctrl-R`, `F5`, the footer's reload | `Reload` |
//! | `m`, the star | `Bookmark { on }` |
//! | `i`, the footer's identify | `Identify { on }` |
//! | Enter on a focused link, a click, a hint | `Activate(NodeId::Link(n))` |
//! | a click on a field, a hint | `Activate(NodeId::Field(n))` |
//! | `Tab`, `Shift-Tab` | `FocusNext`, `FocusPrev` |
//! | `f` | `Hint` |
//! | Space on a checkbox or radio | `Toggle(n)` |
//! | `/` | `Find` |
//! | Enter in the search editor | `Search { query: None }` |
//! | `n`, `N` | `Match { next }` |
//! | a name typed into the address (1227) | `FindName { query }` |
//! | Enter in the name results | `Activate(NodeId::Name(row))` |
//! | `y` to the pin question, any other key | `Answer { yes }` |
//! | the scroll rows, the wheel | `Scroll(cmd)` |
//! | `d` | `Places` |
//! | Enter in the places panel | `Activate(NodeId::Place(row))` |
//! | `x`, `Delete`, the row's `×` | `Forget(row)` |
//! | `u` in the places panel | `Undo` |
//! | `y` | `Copy` |
//! | `o` | `OpenImage` |
//! | `t` | `Theme` |
//! | `Ctrl-l` | `Redraw` |
//! | `?`, `F1` | `Help` |
//! | `Esc` over a panel, in an editor or a field | `Dismiss` |
//! | `q`, `Ctrl-c` | `Quit` |
//!
//! Since slice 1b the keys are a table ([`super::keys`]) whose
//! [`super::keys::Action`]s become these through [`Intent::of_key`],
//! and the key handler folds them with [`apply`] as a tap does.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use leviculum_ui::keymap::ScrollCmd;
use leviculum_ui::scene::Labelled;

use crate::names::PinMark;

use super::chrome::help_scroll_geometry;
use super::event::*;
use super::input::InputBuffer;
use super::keys::Action;
use super::model::*;
use super::scene::NodeId;
use super::strings::{StrId, Text};

/// What the reader asks of the browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    // Navigation.
    /// Go where `url` says, or what the address field holds: a URL, or a
    /// name to search (1227).
    Go {
        url: Option<String>,
    },
    Back,
    Forward,
    Reload,
    /// A tap or a click on a node: the link is followed, the field takes
    /// the focus, the place or the name opens, the address is edited.
    Activate(NodeId),
    /// The topmost of what is open closes: the help, the name list, the
    /// places panel, the hints, the search, the address editor, the field.
    Dismiss,
    /// The fetch or the download in flight stops.
    Cancel,

    // The page.
    FocusNext,
    FocusPrev,
    /// Labels over the visible links, fields and controls.
    Hint,
    /// A checkbox or a radio, by its 1-based field index.
    Toggle(usize),
    /// A text field, by its 1-based index, holds `text`: what a platform's
    /// own text field (a phone's) hands in, where a terminal types key by
    /// key (1337).
    Fill {
        field: usize,
        text: String,
    },
    Scroll(ScrollCmd),
    /// The in-page search field opens, empty.
    Find,
    /// Search the page for `query`, or what the search field holds.
    Search {
        query: Option<String>,
    },
    /// The next or the previous match.
    Match {
        next: bool,
    },

    // What the reader keeps.
    /// The current page bookmarked or not.
    Bookmark {
        on: bool,
    },
    /// Identify to the current page's node or be anonymous to it.
    Identify {
        on: bool,
    },
    /// The sites a name names, from the announces and the pins (1227).
    FindName {
        query: String,
    },
    /// The bookmarks and the discovered nodes.
    Places,
    /// The bookmark of the places panel's row.
    Forget(usize),
    /// The last bookmark removed comes back.
    Undo,
    /// The answer to the name list's question whether to open, and pin, a
    /// name pinned to another key.
    Answer {
        yes: bool,
    },
    /// The focused link's URL or the page's to the clipboard.
    Copy,
    /// The focused picture in the system viewer.
    OpenImage,
    /// Light or dark.
    Theme,
    /// The page laid out again.
    Redraw,
    Help,
    Quit,
}

impl Intent {
    /// The intent a key's [`Action`] raises now: what the focused link, the
    /// selected place or the state of the page make of it. `None` where it
    /// acts on nothing (`Enter` with no link focused, `Space` with no field)
    /// and for a footer sentence.
    pub fn of_key(action: Action, model: &Model) -> Option<Intent> {
        Some(match action {
            Action::Quit => Intent::Quit,
            Action::Cancel => Intent::Cancel,
            Action::Redraw => Intent::Redraw,
            Action::Address => Intent::Activate(NodeId::Address),
            Action::Hint => Intent::Hint,
            Action::Find => Intent::Find,
            Action::Match(next) => Intent::Match { next },
            Action::Theme => Intent::Theme,
            Action::Places => Intent::Places,
            Action::Mark => Intent::Bookmark {
                on: !current_url(model).is_some_and(|url| model.bookmarks.contains(&url)),
            },
            Action::OpenImage => Intent::OpenImage,
            Action::Copy => Intent::Copy,
            Action::Identify => Intent::Identify {
                on: !model.identifying,
            },
            Action::FocusNext => Intent::FocusNext,
            Action::FocusPrev => Intent::FocusPrev,
            Action::Follow => Intent::Activate(NodeId::Link(model.focus?)),
            Action::Help => Intent::Help,
            Action::Reload => Intent::Reload,
            Action::Back => Intent::Back,
            Action::Forward => Intent::Forward,
            Action::Scroll(cmd) => Intent::Scroll(cmd),
            Action::Dismiss => Intent::Dismiss,
            Action::Go => Intent::Go { url: None },
            Action::Search => Intent::Search { query: None },
            Action::Toggle => Intent::Toggle(model.field_focus?),
            Action::Open => match &model.names {
                Some(results) => Intent::Activate(NodeId::Name(results.sel)),
                None => Intent::Activate(NodeId::Place(model.places_sel)),
            },
            Action::Forget => Intent::Forget(model.places_sel),
            Action::Undo => Intent::Undo,
            Action::Answer(yes) => Intent::Answer { yes },
            Action::Sentence => return None,
        })
    }
}

impl Labelled for Intent {
    type Key = StrId;

    fn label(&self) -> StrId {
        match self {
            Intent::Go { .. } => StrId::Go,
            Intent::Back => StrId::Back,
            Intent::Forward => StrId::Forward,
            Intent::Reload => StrId::Reload,
            Intent::Activate(NodeId::Address) => StrId::Edit,
            Intent::Activate(NodeId::Field(_)) => StrId::Edit,
            Intent::Activate(_) => StrId::Open,
            Intent::Dismiss => StrId::Dismiss,
            Intent::Cancel => StrId::Cancel,
            Intent::Find => StrId::Search,
            Intent::FocusNext => StrId::NextField,
            Intent::FocusPrev => StrId::PrevField,
            Intent::Hint => StrId::HintMode,
            Intent::Toggle(_) => StrId::Toggle,
            Intent::Fill { .. } => StrId::Edit,
            Intent::Scroll(_) => StrId::Scroll,
            Intent::Search { .. } => StrId::Search,
            Intent::Match { next: true } => StrId::Next,
            Intent::Match { next: false } => StrId::Prev,
            Intent::Bookmark { on: true } => StrId::Bookmark,
            Intent::Bookmark { on: false } => StrId::Unbookmark,
            Intent::Identify { on: true } => StrId::Identify,
            Intent::Identify { on: false } => StrId::Anonymous,
            Intent::FindName { .. } => StrId::FindName,
            Intent::Places => StrId::Places,
            Intent::Forget(_) => StrId::Remove,
            Intent::Undo => StrId::Undo,
            Intent::Answer { yes: true } => StrId::Open,
            Intent::Answer { yes: false } => StrId::Cancel,
            Intent::Copy => StrId::Copy,
            Intent::OpenImage => StrId::Open,
            Intent::Theme => StrId::HelpTheme,
            Intent::Redraw => StrId::HelpRedraw,
            Intent::Help => StrId::Help,
            Intent::Quit => StrId::Quit,
        }
    }
}

/// Fold `intent` into the model through the functions the keys reach,
/// returning the effects for the shell. An intent that does not apply now
/// (back with no past, a bookmark that is already set) changes nothing.
pub fn apply(model: &mut Model, intent: Intent) -> Vec<Effect> {
    match intent {
        Intent::Go { url } => {
            let raw = url.unwrap_or_else(|| InputBuffer::value(&model.input).to_string());
            go_to(model, raw.trim())
        }
        Intent::Back if model.hosted && loading_past_the_page(model) => stop_loading(model),
        Intent::Back if model.hosted && !model.history.can_back() => leave_host(model),
        Intent::Back => go_back(model),
        Intent::Forward => go_forward(model),
        Intent::Reload => reload_current(model),
        Intent::Activate(node) => activate(model, node),
        Intent::Dismiss => {
            dismiss(model);
            Vec::new()
        }
        Intent::Cancel => {
            if !model.is_loading() {
                return Vec::new();
            }
            model.pending = None;
            model.download = None;
            model.say(ToastKind::Info, Text::key(StrId::Cancelled));
            vec![Effect::Cancel]
        }
        Intent::Find => {
            enter_search(model);
            Vec::new()
        }
        Intent::FocusNext => {
            focus_next(model);
            Vec::new()
        }
        Intent::FocusPrev => {
            focus_prev(model);
            Vec::new()
        }
        Intent::Hint => {
            enter_hint(model);
            Vec::new()
        }
        Intent::Toggle(field) => {
            if (1..=model.field_state.len()).contains(&field) {
                toggle_field(model, field);
            }
            Vec::new()
        }
        Intent::Fill { field, text } => {
            fill_field(model, field, &text);
            Vec::new()
        }
        Intent::Scroll(cmd) => {
            scroll(model, cmd);
            Vec::new()
        }
        Intent::Search { query } => {
            let query =
                query.unwrap_or_else(|| InputBuffer::value(&model.search_input).to_string());
            model.mode = Mode::Browse;
            commit_search(model, &query);
            Vec::new()
        }
        Intent::Match { next } => {
            match next {
                true => next_match(model),
                false => prev_match(model),
            }
            Vec::new()
        }
        Intent::Bookmark { on } => match current_url(model) {
            Some(url) if model.bookmarks.contains(&url) == on => Vec::new(),
            _ => toggle_bookmark_current(model),
        },
        Intent::Identify { on } if model.identifying == on => Vec::new(),
        Intent::Identify { .. } => toggle_identify(model),
        Intent::FindName { query } => search_names(model, query.trim()),
        Intent::Places => {
            toggle_places(model);
            Vec::new()
        }
        Intent::Forget(row) => match places(model).get(row) {
            Some(Place::Bookmark { .. }) => delete_bookmark_at(model, row),
            _ => Vec::new(),
        },
        Intent::Undo => undo_delete_bookmark(model),
        Intent::Answer { yes: true } => match model.names.as_ref().is_some_and(|r| r.confirm) {
            true => open_name_hit(model, true),
            false => Vec::new(),
        },
        Intent::Answer { yes: false } => {
            if let Some(results) = model.names.as_mut() {
                results.confirm = false;
            }
            Vec::new()
        }
        Intent::Copy => yank_url(model),
        Intent::OpenImage => match model
            .focus
            .and_then(|index| saved_image_effect(model, index, true))
        {
            Some(effect) => {
                model.dismiss_toast();
                vec![effect]
            }
            None => Vec::new(),
        },
        Intent::Theme => {
            model.toggle_theme();
            Vec::new()
        }
        Intent::Redraw => {
            let (w, vp) = (model.width, model.viewport());
            model.relayout(w);
            model.clamp_scroll(vp);
            Vec::new()
        }
        Intent::Help => {
            model.show_help = true;
            model.help_scroll = 0;
            Vec::new()
        }
        Intent::Quit => {
            model.quit = true;
            vec![Effect::Quit]
        }
    }
}

/// A tap on `node`.
fn activate(model: &mut Model, node: NodeId) -> Vec<Effect> {
    match node {
        NodeId::Address => {
            enter_address(model);
            Vec::new()
        }
        NodeId::Link(n) if model.links.iter().any(|l| l.index == n) => follow_link(model, n),
        NodeId::Field(n) if (1..=model.field_state.len()).contains(&n) => {
            activate_field(model, n);
            Vec::new()
        }
        NodeId::Place(row) if row < places(model).len() => activate_place(model, row),
        NodeId::Name(row) => {
            let Some(results) = model.names.as_mut() else {
                return Vec::new();
            };
            match results.hits.get(row) {
                // A name pinned to another key asks before it opens.
                Some(hit) if hit.mark == PinMark::Conflict && !results.confirm => {
                    results.sel = row;
                    results.confirm = true;
                    return Vec::new();
                }
                Some(_) => results.sel = row,
                // An empty list closes, as Enter on it always did.
                None if results.hits.is_empty() => {}
                None => return Vec::new(),
            }
            open_name_hit(model, false)
        }
        _ => Vec::new(),
    }
}

/// Scroll what is in front: the help, the places panel's selection, the
/// name list's, else the page. The keys reach the same one.
fn scroll(model: &mut Model, cmd: ScrollCmd) {
    if model.show_help {
        let (viewport, max) = help_scroll_geometry(model);
        model.help_scroll = leviculum_ui::keymap::scrolled(model.help_scroll, cmd, viewport, max);
    } else if model.show_places {
        apply_places_scroll(model, cmd);
    } else if model.names.is_some() {
        move_name_sel(model, cmd);
    } else {
        let vp = model.viewport();
        model.apply_scroll(cmd, vp);
    }
}

/// Close what is open over the page, the topmost first.
fn dismiss(model: &mut Model) {
    if model.show_help {
        model.show_help = false;
    } else if model.names.is_some() {
        model.names = None;
    } else if model.show_places {
        close_places(model);
    } else if model.mode == Mode::Hint {
        exit_hint(model);
    } else if model.mode == Mode::Search {
        exit_search(model);
    } else if model.mode == Mode::Address {
        model.mode = Mode::Browse;
        model.input.set_value("");
        model.dismiss_toast();
    } else if model.mode == Mode::Field {
        model.field_focus = None;
        model.mode = Mode::Browse;
    }
}

/// A hosted browser fetches a page other than the one on screen (1669, walk
/// 12, finding 73): a Back then means "not that one", as in any phone's
/// browser. A first page from the host (nothing on screen yet) and the
/// fresh copy of a cached page on screen are not this.
fn loading_past_the_page(model: &Model) -> bool {
    match (&model.pending, model.history.current()) {
        (Some(pending), Some(shown)) => pending.target != *shown,
        _ => false,
    }
}

/// Stop the fetch in flight and stay on the page on screen
/// ([`Effect::Cancel`], which the host's fetch says as `stopped`).
fn stop_loading(model: &mut Model) -> Vec<Effect> {
    model.pending = None;
    model.pending_anchor = None;
    model.download = None;
    vec![Effect::Cancel]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::input::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use crate::url::Target;
    use leviculum_micron::parse;

    const SAMPLE: &str = include_str!("../../tests/fixtures/sample.mu");

    fn tgt(n: u8) -> Target {
        Target {
            dest_hash: [n; 16],
            path: format!("/page/{n}.mu"),
            fields: Vec::new(),
            is_file: false,
        }
    }

    /// The sample page, the second of two in history, its node current.
    fn model() -> Model {
        let mut model = Model::from_document(&parse(SAMPLE), 79, "Sample", (80, 24));
        model.history.visit(tgt(1));
        model.history.visit(tgt(2));
        model.current_dest = Some([2; 16]);
        model.fingerprint = "0123456789abcdef".to_string();
        model.clock = Some(1_000);
        model
            .node_registry
            .upsert([7; 16], Some("Marcel".to_string()), Some(2), 900);
        model
    }

    /// A URL the address field takes.
    const URL: &str = "07070707070707070707070707070707:/page/x.mu";

    fn keys(model: &mut Model, keys: &[(KeyCode, KeyModifiers)]) -> Vec<Effect> {
        let mut effects = Vec::new();
        for (code, modifiers) in keys {
            effects = update(
                model,
                AppEvent::Key(KeyEvent {
                    code: *code,
                    modifiers: *modifiers,
                    kind: KeyEventKind::Press,
                }),
            );
        }
        effects
    }

    /// A key and the modifiers held.
    type Chord = (KeyCode, KeyModifiers);

    fn plain(c: char) -> (KeyCode, KeyModifiers) {
        (KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Each intent does what the key it names does: the same effects, the
    /// same scene after. A tap and a key cannot drift apart.
    #[test]
    fn an_intent_does_what_its_key_does() {
        let none = KeyModifiers::NONE;
        let mut go: Vec<_> = vec![plain(':')];
        go.extend(URL.chars().map(plain));
        go.push((KeyCode::Enter, none));
        let mut name: Vec<_> = vec![plain(':')];
        name.extend("Marcel".chars().map(plain));
        name.push((KeyCode::Enter, none));
        let cases: Vec<(Intent, Vec<(KeyCode, KeyModifiers)>)> = vec![
            (Intent::Back, vec![(KeyCode::Left, KeyModifiers::ALT)]),
            (Intent::Reload, vec![plain('R')]),
            (Intent::Bookmark { on: true }, vec![plain('m')]),
            (Intent::Identify { on: true }, vec![plain('i')]),
            (Intent::FocusNext, vec![(KeyCode::Tab, none)]),
            (Intent::FocusPrev, vec![(KeyCode::BackTab, none)]),
            (Intent::Hint, vec![plain('f')]),
            (Intent::Scroll(ScrollCmd::LineDown), vec![plain('j')]),
            (Intent::Places, vec![plain('d')]),
            (Intent::Copy, vec![plain('y')]),
            (Intent::Help, vec![plain('?')]),
            (Intent::Help, vec![(KeyCode::F(1), none)]),
            (Intent::Quit, vec![plain('q')]),
            (
                Intent::Quit,
                vec![(KeyCode::Char('c'), KeyModifiers::CONTROL)],
            ),
            (Intent::Find, vec![plain('/')]),
            (Intent::Match { next: true }, vec![plain('n')]),
            (Intent::Theme, vec![plain('t')]),
            (
                Intent::Redraw,
                vec![(KeyCode::Char('l'), KeyModifiers::CONTROL)],
            ),
            (Intent::Forward, vec![(KeyCode::Right, KeyModifiers::ALT)]),
            (Intent::Scroll(ScrollCmd::Bottom), vec![plain('G')]),
            (
                Intent::Go {
                    url: Some(URL.to_string()),
                },
                go,
            ),
            (
                Intent::FindName {
                    query: "Marcel".to_string(),
                },
                name,
            ),
        ];
        for (intent, chord) in cases {
            let mut by_intent = model();
            let mut by_key = model();
            let a = apply(&mut by_intent, intent.clone());
            let b = keys(&mut by_key, &chord);
            let navigates = matches!(
                intent,
                Intent::Go { .. }
                    | Intent::FindName { .. }
                    | Intent::Back
                    | Intent::Reload
                    | Intent::Identify { .. }
            );
            assert_eq!(
                navigates,
                a.iter().any(|e| matches!(e, Effect::Navigate(_))),
                "{intent:?} navigates"
            );
            assert_eq!(a, b, "{intent:?}: effects");
            assert_eq!(by_intent.scene(), by_key.scene(), "{intent:?}: scene");
        }
    }

    /// In a panel or an editor a key's intent is the panel's: after the
    /// same opening keys, the key and its intent leave the same effects and
    /// the same scene. `Dismiss` leaves the address editor, `Scroll` moves
    /// the places panel's selection, `Places` closes it.
    #[test]
    fn a_key_in_a_panel_is_the_panels_intent() {
        let none = KeyModifiers::NONE;
        let cases: Vec<(Vec<Chord>, Chord, Intent)> = vec![
            (
                vec![plain(':'), plain('x')],
                (KeyCode::Esc, none),
                Intent::Dismiss,
            ),
            (
                vec![plain('/'), plain('S')],
                (KeyCode::Esc, none),
                Intent::Dismiss,
            ),
            (vec![plain('f')], (KeyCode::Esc, none), Intent::Dismiss),
            (vec![plain('?')], (KeyCode::Esc, none), Intent::Dismiss),
            (
                vec![plain('?')],
                plain('j'),
                Intent::Scroll(ScrollCmd::LineDown),
            ),
            (
                vec![plain('m'), plain('d')],
                plain('j'),
                Intent::Scroll(ScrollCmd::LineDown),
            ),
            (vec![plain('d')], plain('d'), Intent::Places),
            (vec![plain('m'), plain('d')], plain('x'), Intent::Forget(0)),
            (
                vec![plain('/'), plain('S')],
                (KeyCode::Enter, none),
                Intent::Search { query: None },
            ),
        ];
        for (opening, key, intent) in cases {
            let mut by_key = model();
            keys(&mut by_key, &opening);
            let mut by_intent = by_key.clone();
            let b = keys(&mut by_key, &[key]);
            by_intent.dismiss_toast();
            let a = apply(&mut by_intent, intent.clone());
            assert_eq!(a, b, "{intent:?} after {opening:?}: effects");
            assert_eq!(
                by_intent.scene(),
                by_key.scene(),
                "{intent:?} after {opening:?}: scene"
            );
            assert_eq!(by_intent.mode, by_key.mode, "{intent:?}: mode");
            assert_eq!(by_intent.show_places, by_key.show_places, "{intent:?}");
            assert_eq!(by_intent.show_help, by_key.show_help, "{intent:?}");
            assert_eq!(by_intent.help_scroll, by_key.help_scroll, "{intent:?}");
        }
    }

    /// A tap on a link follows it as Enter on the focused link does.
    #[test]
    fn a_fill_sets_a_text_field_as_typing_it_would() {
        // The phone's editor hands the whole text in; the terminal types it
        // key by key. Both leave the same field and the same page.
        let mut typed = model();
        apply(&mut typed, Intent::Activate(NodeId::Field(1)));
        for c in "Lew".chars() {
            update(
                &mut typed,
                AppEvent::Key(KeyEvent {
                    code: KeyCode::Char(c),
                    modifiers: KeyModifiers::NONE,
                    kind: KeyEventKind::Press,
                }),
            );
        }
        let mut filled = model();
        let fill = |text: &str| Intent::Fill {
            field: 1,
            text: text.to_string(),
        };
        assert_eq!(apply(&mut filled, fill("Lew")), Vec::new());
        assert_eq!(filled.field_state[0].input.value(), "Lew");
        assert_eq!(filled.field_values(), typed.field_values());
        assert_eq!(filled.scene().page.lines, typed.scene().page.lines);
        // Off the page, nothing moves (the control).
        let before = filled.field_values();
        apply(
            &mut filled,
            Intent::Fill {
                field: 9,
                text: "x".to_string(),
            },
        );
        assert_eq!(filled.field_values(), before);
    }

    #[test]
    fn activating_a_link_follows_it() {
        let mut by_key = model();
        keys(&mut by_key, &[(KeyCode::Tab, KeyModifiers::NONE)]);
        let link = by_key.focus.expect("a focused link");
        let mut by_intent = by_key.clone();
        let b = keys(&mut by_key, &[(KeyCode::Enter, KeyModifiers::NONE)]);
        let a = apply(&mut by_intent, Intent::Activate(NodeId::Link(link)));
        assert!(!a.is_empty(), "following a link has an effect");
        assert_eq!(a, b);
        assert_eq!(by_intent.scene(), by_key.scene());
    }

    /// An intent that is already true changes nothing: a second "bookmark
    /// it" does not take the bookmark away, back with no past stays.
    #[test]
    fn an_intent_already_true_changes_nothing() {
        let mut model = model();
        apply(&mut model, Intent::Bookmark { on: true });
        model.dismiss_toast();
        let before = model.scene();
        assert!(apply(&mut model, Intent::Bookmark { on: true }).is_empty());
        assert!(apply(&mut model, Intent::Identify { on: false }).is_empty());
        assert!(apply(&mut model, Intent::Activate(NodeId::Link(999))).is_empty());
        assert_eq!(model.scene(), before);
        let mut first = Model::from_document(&parse(SAMPLE), 79, "Sample", (80, 24));
        assert!(apply(&mut first, Intent::Back).is_empty());
    }

    /// The scene's actions raise intents [`apply`] knows: the star's and
    /// identify's flip their state.
    #[test]
    fn the_top_bar_raises_what_flips_its_state() {
        let mut model = model();
        let star = |m: &Model| {
            m.scene()
                .top_bar
                .actions
                .into_iter()
                .find(|a| matches!(a.intent, Intent::Bookmark { .. }))
                .expect("the star")
                .intent
        };
        assert_eq!(star(&model), Intent::Bookmark { on: true });
        for on in [false, true] {
            let tap = star(&model);
            apply(&mut model, tap);
            assert_eq!(star(&model), Intent::Bookmark { on });
        }
    }
}
