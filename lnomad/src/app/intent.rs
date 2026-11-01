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
//! | Enter in the search editor | `Search { query: None }` |
//! | `n`, `N` | `Match { next }` |
//! | a name typed into the address (1227) | `FindName { query }` |
//! | Enter in the name results | `Activate(NodeId::Name(row))` |
//! | the scroll rows, the wheel | `Scroll(cmd)` |
//! | `d` | `Places` |
//! | Enter in the places panel | `Activate(NodeId::Place(row))` |
//! | `x`, `Delete`, the row's `×` | `Forget(row)` |
//! | `u` in the places panel | `Undo` |
//! | `y` | `Copy` |
//! | `?` | `Help` |
//! | `Esc` over a panel | `Dismiss` |
//! | `q` | `Quit` |
//!
//! Slice 1b turns the keys into a table that resolves to these; until
//! then the keys keep their `match`es and these are what the scene's
//! actions and a host raise.

use leviculum_ui::keymap::ScrollCmd;
use leviculum_ui::scene::Labelled;

use super::event::*;
use super::model::*;
use super::scene::NodeId;
use super::strings::StrId;

/// What the reader asks of the browser.
#[derive(Clone, Debug, PartialEq)]
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
    /// The open panel, the hints or the help closes.
    Dismiss,

    // The page.
    FocusNext,
    FocusPrev,
    /// Labels over the visible links, fields and controls.
    Hint,
    /// A checkbox or a radio, by its 1-based field index.
    Toggle(usize),
    Scroll(ScrollCmd),
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
    /// The focused link's URL or the page's to the clipboard.
    Copy,
    Help,
    Quit,
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
            Intent::FocusNext => StrId::NextField,
            Intent::FocusPrev => StrId::PrevField,
            Intent::Hint => StrId::HintMode,
            Intent::Toggle(_) => StrId::Toggle,
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
            Intent::Copy => StrId::Copy,
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
            let raw = url.unwrap_or_else(|| model.input.value().to_string());
            go_to(model, raw.trim())
        }
        Intent::Back => go_back(model),
        Intent::Forward => go_forward(model),
        Intent::Reload => reload_current(model),
        Intent::Activate(node) => activate(model, node),
        Intent::Dismiss => {
            dismiss(model);
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
                activate_field(model, field);
            }
            Vec::new()
        }
        Intent::Scroll(cmd) => {
            let vp = model.viewport();
            model.apply_scroll(cmd, vp);
            Vec::new()
        }
        Intent::Search { query } => {
            let query = query.unwrap_or_else(|| model.search_input.value().to_string());
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
        Intent::Copy => yank_url(model),
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
            match model.names.as_mut() {
                Some(results) if row < results.hits.len() => results.sel = row,
                _ => return Vec::new(),
            }
            open_name_hit(model, false)
        }
        _ => Vec::new(),
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
    }
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
            (Intent::Quit, vec![plain('q')]),
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

    /// A tap on a link follows it as Enter on the focused link does.
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
