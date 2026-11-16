//! lnomad's keymap: its rows beside the shared scroll rows, and the readers
//! of them (slice 1b of `docs/src/concepts/lnomad-everywhere.md`).
//!
//! The key handlers were one hand-written `match` per mode. They are now
//! this table over [`leviculum_ui::keymap`], read the way Loquax reads its
//! own (`loquax/src/keys.rs`): [`resolve`] is the key handler's lookup, an
//! [`Action`] becomes a [`super::intent::Intent`] through
//! [`super::intent::Intent::of_key`], [`help_groups_in`] is the help overlay
//! and [`footer_items`] the footer strip. The help and the footer show the
//! keys derived from the chords, so neither can name a key the handler does
//! not honour.
//!
//! A mode is a pane: [`BROWSE`], the three editors ([`ADDRESS`],
//! [`SEARCH`], [`FIELD`]), [`HINT`], and the three overlays that own the
//! keys while open ([`HELP`], [`PLACES`], [`NAMES`], and [`ASK`], the name
//! list while it asks whether to pin a second key). Browsing and the
//! overlays that move a cursor take the shared scroll rows too
//! ([`pane`]). Two contexts: [`LOADING`] (a fetch is in flight, so `Esc`
//! cancels it) and [`TOGGLES`] (the focused field is a checkbox or a radio,
//! so `Space` toggles it rather than typing a space).
//!
//! What a table cannot hold stays with the handler: the text an editor
//! takes, the labels typed in hint mode, and the answer "no" that any key
//! but `y` gives the pin question. The mouse is not a chord either: a
//! row's words name the click that does the same, and the wheel and the
//! hover keep lines of their own.
//!
//! The rows' words are catalogue keys (`browser_help_*`), so the help is
//! drawn in the reader's language; `every_row_speaks_the_catalogue` holds
//! that each one resolves.

use alloc::vec::Vec;

use leviculum_micron::FieldKind;
use leviculum_ui::help::{HelpEntry, HelpGroup};
use leviculum_ui::input::{KeyCode, KeyEvent};
use leviculum_ui::keymap::{self, Chord, Ctx, Mods, Panes, Row, ScrollCmd, SCROLL_ROWS};

use super::chrome::{scroll_desc, FooterItem};
use super::model::{Mode, Model};
use super::strings::{Lang, StrId, CATALOGUE};

/// Browsing the page.
pub const BROWSE: Panes = Panes(1 << 1);
/// The address editor.
pub const ADDRESS: Panes = Panes(1 << 2);
/// The in-page search editor.
pub const SEARCH: Panes = Panes(1 << 3);
/// A focused form field.
pub const FIELD: Panes = Panes(1 << 4);
/// Hint mode: the labels over the links.
pub const HINT: Panes = Panes(1 << 5);
/// The help overlay.
pub const HELP: Panes = Panes(1 << 6);
/// The places panel.
pub const PLACES: Panes = Panes(1 << 7);
/// The name search's result list.
pub const NAMES: Panes = Panes(1 << 8);
/// The name list asking whether to open and pin a second key.
pub const ASK: Panes = Panes(1 << 9);
/// The three editors and hint mode: they take typing.
pub const TYPING: Panes = Panes(ADDRESS.0 | SEARCH.0 | FIELD.0 | HINT.0);
/// Every pane.
pub const EVERY: Panes = Panes(BROWSE.0 | TYPING.0 | HELP.0 | PLACES.0 | NAMES.0 | ASK.0);
/// Where `Esc` closes, cancels or leaves: every pane but browsing, the help
/// (which has its own row) and the question.
const CLOSES: Panes = Panes(TYPING.0 | PLACES.0 | NAMES.0);

/// A fetch or a download is in flight.
pub const LOADING: Ctx = Ctx(1 << 1);
/// The focused field is a checkbox or a radio.
pub const TOGGLES: Ctx = Ctx(1 << 2);

/// What a key runs. [`super::intent::Intent::of_key`] turns it into the
/// intent, reading from the model what the key acts on (the focused link,
/// the selected place, whether the page is bookmarked).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Stop the fetch or the download in flight.
    Cancel,
    /// Lay the page out again and redraw.
    Redraw,
    /// Edit the address.
    Address,
    Hint,
    /// Open the in-page search field.
    Find,
    /// The next search match (`true`) or the previous one.
    Match(bool),
    /// Light or dark.
    Theme,
    /// Open or close the places panel.
    Places,
    /// Bookmark the page, or take the bookmark away.
    Mark,
    /// The focused picture in the system viewer.
    OpenImage,
    Copy,
    /// Identify to the page's node, or go anonymous.
    Identify,
    FocusNext,
    FocusPrev,
    /// Follow the focused link.
    Follow,
    Help,
    Reload,
    Back,
    Forward,
    Scroll(ScrollCmd),
    /// Close the panel, cancel the entry, leave the field.
    Dismiss,
    /// Go where the address field says.
    Go,
    /// Search the page for what the search field holds.
    Search,
    /// Toggle the focused checkbox or radio.
    Toggle,
    /// Open the selected place or name.
    Open,
    /// Remove the selected bookmark.
    Forget,
    /// The removed bookmark comes back.
    Undo,
    /// The answer to the pin question.
    Answer(bool),
    /// No action: a footer sentence that is not a button (hint mode's).
    Sentence,
}

const fn key(code: KeyCode, mods: Mods, action: Action, panes: Panes) -> Chord<Action> {
    Chord::new(code, mods, action).panes(panes)
}

/// The help group titles, in the overlay's order, as catalogue keys. The
/// first is the shared scroll rows' group.
pub const GROUPS: [&str; 7] = [MOVE, FOLLOW, PAGE, IMAGES, FIELDS, PLACES_GROUP, APP];
const MOVE: &str = "browser_help_move";
const FOLLOW: &str = "browser_help_follow";
const PAGE: &str = "browser_help_page";
const IMAGES: &str = "browser_help_images";
const FIELDS: &str = "browser_help_fields";
const PLACES_GROUP: &str = "browser_help_places_group";
const APP: &str = "browser_help_app";

/// lnomad's own rows, beside [`SCROLL_ROWS`]. Every chord the old
/// per-mode `match`es honoured is here with the modifiers it took: a key
/// that took any modifiers is [`Mods::Any`], one that refused Ctrl and Alt
/// is [`Mods::Plain`], one that asked for Ctrl or Alt is that class.
pub const ROWS: &[Row<Action>] = {
    use Action::*;
    use KeyCode::{BackTab, Char, Delete, Enter, Esc, Left, Right, Tab, F};
    use Mods::{Alt, Any, Ctrl, Plain};
    &[
        // Follow.
        Row::new(
            "browser_help_focus",
            FOLLOW,
            &[&[
                key(Tab, Any, FocusNext, Panes(BROWSE.0 | FIELD.0)),
                key(BackTab, Any, FocusPrev, Panes(BROWSE.0 | FIELD.0)),
            ]],
        ),
        Row::new(
            "browser_help_follow_link",
            FOLLOW,
            &[&[key(Enter, Any, Follow, BROWSE)]],
        ),
        Row::new(
            "browser_help_hint",
            FOLLOW,
            &[&[key(Char('f'), Plain, Hint, BROWSE)]],
        ),
        Row::new(
            "browser_help_history",
            FOLLOW,
            &[&[
                key(Left, Alt, Back, BROWSE),
                key(Right, Alt, Forward, BROWSE),
            ]],
        ),
        // Page.
        Row::new(
            "browser_help_url",
            PAGE,
            &[&[key(Char(':'), Any, Address, BROWSE)]],
        ),
        Row::new("browser_help_go", PAGE, &[&[key(Enter, Any, Go, ADDRESS)]]),
        // Reload is `R`, `Ctrl-r` and `F5`, browser muscle memory; bare
        // lowercase `r` stays free, as a mesh reload is expensive and single
        // letters are reserved for cheap local actions.
        Row::new(
            "browser_help_reload",
            PAGE,
            &[&[
                key(Char('R'), Any, Reload, BROWSE),
                key(Char('r'), Ctrl, Reload, BROWSE),
                key(F(5), Any, Reload, BROWSE),
            ]],
        ),
        Row::new(
            "browser_help_cancel",
            PAGE,
            &[&[
                key(Esc, Any, Cancel, BROWSE).when(LOADING),
                key(Char('g'), Ctrl, Cancel, BROWSE).when(LOADING),
            ]],
        ),
        Row::new(
            "browser_help_mark",
            PAGE,
            &[&[key(Char('m'), Plain, Mark, BROWSE)]],
        ),
        Row::new(
            "browser_help_copy",
            PAGE,
            &[&[key(Char('y'), Plain, Copy, BROWSE)]],
        ),
        Row::new(
            "browser_help_identify",
            PAGE,
            &[&[key(Char('i'), Plain, Identify, BROWSE)]],
        )
        .note("browser_help_identify_note"),
        Row::new(
            "browser_help_search",
            PAGE,
            &[
                &[key(Char('/'), Plain, Find, BROWSE)],
                &[
                    key(Char('n'), Plain, Match(true), BROWSE),
                    key(Char('N'), Plain, Match(false), BROWSE),
                ],
            ],
        ),
        Row::new(
            "browser_help_search_go",
            PAGE,
            &[&[key(Enter, Any, Search, SEARCH)]],
        ),
        Row::new(
            "browser_help_redraw",
            PAGE,
            &[&[key(Char('l'), Ctrl, Redraw, BROWSE)]],
        ),
        // Images.
        Row::new(
            "browser_help_open_image",
            IMAGES,
            &[&[key(Char('o'), Plain, OpenImage, BROWSE)]],
        )
        .note("browser_help_save_image"),
        Row::new("browser_help_images_off", IMAGES, &[]),
        // Fields.
        Row::new(
            "browser_help_field",
            FIELDS,
            &[&[key(Char(' '), Any, Toggle, FIELD).when(TOGGLES)]],
        ),
        // Places and names.
        Row::new(
            "browser_help_places",
            PLACES_GROUP,
            // `d` closes the panel it opened; any modifier did, but `Ctrl-d`
            // is the shared half page, which is more specific.
            &[&[
                key(Char('d'), Plain, Places, BROWSE),
                key(Char('d'), Any, Places, PLACES).quiet(),
            ]],
        ),
        Row::new(
            "browser_help_open_place",
            PLACES_GROUP,
            &[&[key(Enter, Any, Open, Panes(PLACES.0 | NAMES.0))]],
        ),
        Row::new(
            "browser_help_forget",
            PLACES_GROUP,
            &[&[
                key(Char('x'), Any, Forget, PLACES),
                key(Delete, Any, Forget, PLACES),
            ]],
        ),
        Row::new(
            "browser_help_undo",
            PLACES_GROUP,
            &[&[key(Char('u'), Any, Undo, PLACES)]],
        ),
        Row::new(
            "browser_help_answer",
            PLACES_GROUP,
            &[&[
                key(Char('y'), Any, Answer(true), ASK),
                key(Char('Y'), Any, Answer(true), ASK).quiet(),
            ]],
        ),
        // App.
        Row::new(
            "browser_help_theme",
            APP,
            &[&[key(Char('t'), Plain, Theme, BROWSE)]],
        ),
        Row::new(
            "browser_help",
            APP,
            &[
                &[key(Char('?'), Any, Help, BROWSE)],
                &[key(F(1), Any, Help, Panes(BROWSE.0 | TYPING.0))],
            ],
        ),
        Row::new(
            "browser_help_close",
            APP,
            &[&[
                key(Char('?'), Any, Dismiss, HELP),
                key(Esc, Any, Dismiss, HELP),
                key(Char('q'), Any, Dismiss, HELP),
            ]],
        ),
        Row::new(
            "browser_help_dismiss",
            APP,
            &[&[key(Esc, Any, Dismiss, CLOSES)]],
        ),
        Row::new(
            "browser_quit",
            APP,
            &[&[
                key(Char('q'), Any, Quit, BROWSE),
                key(Char('c'), Ctrl, Quit, EVERY),
            ]],
        ),
    ]
};

/// The pane the model's keys go to: the help, then the places panel, then
/// the name list own them while open, else the mode's. Browsing and the
/// overlays with a cursor take the shared scroll rows too.
pub fn pane(model: &Model) -> Panes {
    if model.show_help {
        return HELP.with(Panes::SCROLL);
    }
    if model.show_places {
        return PLACES.with(Panes::SCROLL);
    }
    if let Some(results) = &model.names {
        return match results.confirm {
            true => ASK,
            false => NAMES.with(Panes::SCROLL),
        };
    }
    mode_pane(model.mode)
}

/// The pane of a mode, the footer's and the keys' when nothing is open
/// over the page.
pub fn mode_pane(mode: Mode) -> Panes {
    match mode {
        Mode::Browse => BROWSE.with(Panes::SCROLL),
        Mode::Address => ADDRESS,
        Mode::Search => SEARCH,
        Mode::Field => FIELD,
        Mode::Hint => HINT,
    }
}

/// What holds besides the pane: a fetch in flight, a toggle focused.
pub fn ctx(model: &Model) -> Ctx {
    let mut ctx = Ctx::NONE;
    if model.is_loading() {
        ctx = ctx.with(LOADING);
    }
    let toggles = model
        .field_focus
        .and_then(|fi| model.field_state.get(fi.wrapping_sub(1)))
        .is_some_and(|fe| matches!(fe.kind, FieldKind::Checkbox | FieldKind::Radio));
    if toggles {
        ctx = ctx.with(TOGGLES);
    }
    ctx
}

/// What `key` runs in `pane` under `ctx`: the most specific chord across
/// lnomad's rows and the shared scroll rows, lnomad's first among equals,
/// as Loquax resolves (`loquax::keys::resolve`).
pub fn resolve(key: &KeyEvent, pane: Panes, ctx: Ctx) -> Option<Action> {
    let own = keymap::best(ROWS, key, pane, ctx);
    let shared = keymap::best(SCROLL_ROWS, key, pane, ctx).map(|(r, c)| (r, Action::Scroll(c)));
    match (own, shared) {
        (Some(own), Some(shared)) if shared.0 > own.0 => Some(shared.1),
        (Some(own), _) => Some(own.1),
        (None, shared) => shared.map(|(_, action)| action),
    }
}

/// The catalogue entry a row's word names.
fn id_of(key: &str) -> Option<StrId> {
    CATALOGUE
        .iter()
        .find(|(_, k, _, _)| *k == key)
        .map(|(id, _, _, _)| *id)
}

/// A row's word in `lang`. The test `every_row_speaks_the_catalogue` holds
/// that none falls back to the key itself.
fn word(key: &'static str, lang: Lang) -> &'static str {
    id_of(key).map_or(key, |id| id.text(lang))
}

/// The help overlay, in `lang`: every row of both tables in the groups of
/// [`GROUPS`], its keys derived from its chords, its note the line under
/// it. The wheel and the hover are the two mouse lines of their own.
pub fn help_groups_in(lang: Lang) -> Vec<HelpGroup> {
    let entry = |keys, desc| HelpEntry { keys, desc };
    GROUPS
        .iter()
        .map(|&title| {
            let mut entries = Vec::new();
            if title == MOVE {
                entries.extend(
                    SCROLL_ROWS
                        .iter()
                        .map(|r| entry(r.keys(), scroll_desc(r.desc, lang))),
                );
                entries.push(entry(StrId::HelpWheel.text(lang), StrId::Scroll.text(lang)));
            }
            for row in ROWS.iter().filter(|r| r.group == title) {
                entries.push(entry(row.keys(), word(row.desc, lang)));
                if let Some(note) = row.note {
                    entries.push(entry("", word(note, lang)));
                }
            }
            if title == FOLLOW {
                entries.push(entry(
                    StrId::HelpHoverKeys.text(lang),
                    StrId::HelpHover.text(lang),
                ));
            }
            HelpGroup {
                title: word(title, lang),
                entries,
            }
        })
        .collect()
}

/// A footer button: the action, its word, and when it drops on a narrow
/// row (`None` never). Its key is the chord the table has for the action
/// in the footer's pane.
struct Button(Action, StrId, Option<u8>);

/// The navigation trio, first on every footer. Their keys are browsing's,
/// whatever the mode: a click runs them from anywhere.
const NAV: [Button; 3] = [
    Button(Action::Back, StrId::Back, None),
    Button(Action::Forward, StrId::Forward, None),
    Button(Action::Reload, StrId::Reload, None),
];

/// Each mode's buttons after the trio, in the strip's order.
fn buttons(mode: Mode) -> &'static [Button] {
    use Action::*;
    match mode {
        Mode::Browse => &[
            Button(Hint, StrId::HintMode, Some(5)),
            Button(Find, StrId::Search, Some(4)),
            Button(Places, StrId::Places, Some(3)),
            Button(Mark, StrId::Mark, Some(2)),
            Button(Copy, StrId::Copy, Some(1)),
            Button(Help, StrId::Help, None),
            Button(Quit, StrId::Quit, None),
        ],
        Mode::Field => &[
            Button(Dismiss, StrId::Done, None),
            Button(FocusNext, StrId::NextField, Some(1)),
            Button(Toggle, StrId::Toggle, Some(2)),
            Button(Help, StrId::Help, None),
        ],
        Mode::Search => &[
            Button(Search, StrId::Go, None),
            Button(Dismiss, StrId::Cancel, None),
            Button(Help, StrId::Help, Some(1)),
        ],
        Mode::Address => &[
            Button(Go, StrId::Go, None),
            Button(Dismiss, StrId::Cancel, None),
            Button(Help, StrId::Help, Some(1)),
        ],
        Mode::Hint => &[Button(Dismiss, StrId::Cancel, None)],
    }
}

/// The chord that runs `action` in `pane` under `ctx`, as the reader reads
/// it: the first live one in table order.
pub fn chord_for(action: Action, pane: Panes, ctx: Ctx) -> Option<&'static str> {
    ROWS.iter()
        .flat_map(|r| r.chords())
        .find(|c| c.action == action && c.live(pane, ctx))
        .map(|c| c.label())
}

/// The footer for the model's mode: the navigation trio, then the mode's
/// buttons whose action has a chord there now, each showing that chord.
/// Browsing shows identify when there is a page to identify to, worded by
/// the state it flips; hint mode leads with its instruction.
pub fn footer_items(model: &Model) -> Vec<FooterItem> {
    let w = |id: StrId| id.text(model.lang);
    let ctx = ctx(model);
    let button = |b: &Button, pane: Panes, label: &'static str| {
        let key = chord_for(b.0, pane, ctx)?;
        Some(match b.2 {
            None => FooterItem::action(key, label, b.0),
            Some(rank) => FooterItem::droppable(key, label, b.0, rank),
        })
    };
    let available = [
        model.history.can_back(),
        model.history.can_forward(),
        model.history.current().is_some(),
    ];
    let mut items: Vec<FooterItem> = NAV
        .iter()
        .zip(available)
        .filter_map(|(b, available)| {
            let item = button(b, BROWSE, w(b.1))?;
            Some(FooterItem { available, ..item })
        })
        .collect();
    let pane = mode_pane(model.mode);
    match model.mode {
        Mode::Browse if model.current_dest.is_some() => {
            let label = match model.identifying {
                true => w(StrId::Anonymous),
                false => w(StrId::Identify),
            };
            items.extend(button(
                &Button(Action::Identify, StrId::Identify, Some(6)),
                pane,
                label,
            ));
        }
        Mode::Hint => items.push(FooterItem {
            key: "",
            label: w(StrId::HintSentence),
            key_first: true,
            action: Action::Sentence,
            drop_rank: None,
            available: true,
        }),
        _ => {}
    }
    items.extend(
        buttons(model.mode)
            .iter()
            .filter_map(|b| button(b, pane, w(b.1))),
    );
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_ui::input::KeyModifiers;
    use leviculum_ui::keymap::{all_best, canonical};

    /// Every pane a key can meet, with the scroll rows where they apply.
    const PANES: [Panes; 9] = [
        Panes(BROWSE.0 | Panes::SCROLL.0),
        ADDRESS,
        SEARCH,
        FIELD,
        HINT,
        Panes(HELP.0 | Panes::SCROLL.0),
        Panes(PLACES.0 | Panes::SCROLL.0),
        Panes(NAMES.0 | Panes::SCROLL.0),
        ASK,
    ];
    /// Every setting: each of the two context bits on and off.
    const CTXS: [Ctx; 4] = [Ctx::NONE, LOADING, TOGGLES, Ctx(LOADING.0 | TOGGLES.0)];

    /// Every action `key` reaches at the winning rank across both tables.
    fn hits(key: &KeyEvent, pane: Panes, ctx: Ctx) -> Vec<Action> {
        let own = all_best(ROWS, key, pane, ctx);
        let shared: Vec<(u8, Action)> = all_best(SCROLL_ROWS, key, pane, ctx)
            .into_iter()
            .map(|(r, c)| (r, Action::Scroll(c)))
            .collect();
        let top = own.iter().chain(&shared).map(|(r, _)| *r).max();
        let mut out = Vec::new();
        for (rank, action) in own.into_iter().chain(shared) {
            if Some(rank) == top && !out.contains(&action) {
                out.push(action);
            }
        }
        out
    }

    /// No chord resolves to two intents in one mode: each chord's own key,
    /// in every pane and setting it is in force, resolves to its own row's
    /// action and to nothing else.
    #[test]
    fn no_chord_resolves_to_two_actions_in_one_pane() {
        let own = ROWS
            .iter()
            .flat_map(|r| r.chords())
            .map(|c| (c.action, canonical(c), c.label(), c.panes, c.when, c.unless));
        let shared = SCROLL_ROWS.iter().flat_map(|r| r.chords()).map(|c| {
            (
                Action::Scroll(c.action),
                canonical(c),
                c.label(),
                c.panes,
                c.when,
                c.unless,
            )
        });
        let mut checked = 0;
        for (action, event, label, panes, when, unless) in own.chain(shared) {
            for pane in PANES {
                for ctx in CTXS {
                    if !(panes.meets(pane) && ctx.contains(when) && !ctx.meets(unless)) {
                        continue;
                    }
                    assert_eq!(
                        hits(&event, pane, ctx),
                        [action],
                        "{label} in {pane:?} under {ctx:?}"
                    );
                    assert_eq!(resolve(&event, pane, ctx), Some(action), "{label}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "only {checked} chord placements checked");
    }

    /// The old handlers, transcribed from the per-mode `match`es this table
    /// replaced (`update_key`, `update_browse_key`, `update_address_key`,
    /// `update_search_key`, `update_hint_key`, `update_field_key`,
    /// `update_places_key`, `update_names_key` before slice 1b), as the
    /// action each key reached. `None` is a key the mode handed to its editor
    /// or ignored. `scroll` is the shared scroll lookup they all called.
    fn old(key: &KeyEvent, pane: Panes, ctx: Ctx) -> Option<Action> {
        use Action::*;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let plain = !ctrl && !alt;
        let code = key.code;
        let ch = |c: char| code == KeyCode::Char(c);
        let scroll =
            || keymap::best(SCROLL_ROWS, key, Panes::SCROLL, Ctx::NONE).map(|(_, c)| Scroll(c));
        if ctrl && ch('c') {
            return Some(Quit);
        }
        if pane.meets(HELP) {
            if matches!(code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')) {
                return Some(Dismiss);
            }
            return scroll();
        }
        if pane.meets(PLACES) {
            if let Some(s) = scroll() {
                return Some(s);
            }
            return match code {
                KeyCode::Esc => Some(Dismiss),
                KeyCode::Char('d') => Some(Places),
                KeyCode::Enter => Some(Open),
                KeyCode::Char('x') | KeyCode::Delete => Some(Forget),
                KeyCode::Char('u') => Some(Undo),
                _ => None,
            };
        }
        if pane.meets(ASK) {
            return matches!(code, KeyCode::Char('y') | KeyCode::Char('Y')).then_some(Answer(true));
        }
        if pane.meets(NAMES) {
            if let Some(s) = scroll() {
                return Some(s);
            }
            return match code {
                KeyCode::Esc => Some(Dismiss),
                KeyCode::Enter => Some(Open),
                _ => None,
            };
        }
        if pane.meets(ADDRESS) {
            return match code {
                KeyCode::Esc => Some(Dismiss),
                KeyCode::Enter => Some(Go),
                _ => None,
            };
        }
        if pane.meets(SEARCH) {
            return match code {
                KeyCode::Esc => Some(Dismiss),
                KeyCode::Enter => Some(Search),
                _ => None,
            };
        }
        if pane.meets(HINT) {
            return (code == KeyCode::Esc).then_some(Dismiss);
        }
        if pane.meets(FIELD) {
            return match code {
                KeyCode::Tab => Some(FocusNext),
                KeyCode::BackTab => Some(FocusPrev),
                KeyCode::Esc => Some(Dismiss),
                KeyCode::Char(' ') if ctx.contains(TOGGLES) => Some(Toggle),
                _ => None,
            };
        }
        // Browsing, in the old handler's order.
        if ch('q') {
            return Some(Quit);
        }
        if ctx.contains(LOADING) && (code == KeyCode::Esc || (ctrl && ch('g'))) {
            return Some(Cancel);
        }
        if ctrl && ch('l') {
            return Some(Redraw);
        }
        if ch(':') {
            return Some(Address);
        }
        let plain_keys = [
            ('f', Hint),
            ('/', Find),
            ('n', Match(true)),
            ('N', Match(false)),
            ('t', Theme),
            ('d', Places),
            ('m', Mark),
            ('o', OpenImage),
            ('y', Copy),
            ('i', Identify),
        ];
        for (c, action) in plain_keys {
            if ch(c) && plain {
                return Some(action);
            }
        }
        match code {
            KeyCode::Tab => return Some(FocusNext),
            KeyCode::BackTab => return Some(FocusPrev),
            KeyCode::Enter => return Some(Follow),
            KeyCode::Char('?') => return Some(Help),
            _ => {}
        }
        if ch('R') || (ctrl && ch('r')) || code == KeyCode::F(5) {
            return Some(Reload);
        }
        if alt && code == KeyCode::Left {
            return Some(Back);
        }
        if alt && code == KeyCode::Right {
            return Some(Forward);
        }
        scroll()
    }

    /// Every key the tests press: the printable ASCII, a letter outside it,
    /// and every special key, each under every mix of Shift, Ctrl and Alt.
    fn every_key() -> Vec<KeyEvent> {
        let mut codes: Vec<KeyCode> = (0x20u8..0x7f).map(|b| KeyCode::Char(b as char)).collect();
        codes.push(KeyCode::Char('ä'));
        codes.extend([
            KeyCode::Backspace,
            KeyCode::Enter,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::Esc,
            KeyCode::Null,
        ]);
        codes.extend((1..=12).map(KeyCode::F));
        let mut out = Vec::new();
        for code in codes {
            for bits in 0..8u8 {
                let mut mods = KeyModifiers::NONE;
                if bits & 1 != 0 {
                    mods |= KeyModifiers::SHIFT;
                }
                if bits & 2 != 0 {
                    mods |= KeyModifiers::CONTROL;
                }
                if bits & 4 != 0 {
                    mods |= KeyModifiers::ALT;
                }
                out.push(KeyEvent::new(code, mods));
            }
        }
        out
    }

    /// The keys the table adds to the old handlers: `F1` opens the help
    /// from browsing and from every editor, as it does in Loquax, so the
    /// editors' footers name a help key they honour.
    fn added(key: &KeyEvent, pane: Panes) -> Option<Action> {
        (key.code == KeyCode::F(1) && (pane.meets(BROWSE) || pane.meets(TYPING)))
            .then_some(Action::Help)
    }

    /// Each chord the old `match`es honoured resolves to the same action in
    /// the table, in every mode and setting, and the table honours nothing
    /// the old handlers did not, but for [`added`]. 9 panes × 4 settings ×
    /// every key under 8 modifier mixes.
    #[test]
    fn every_old_chord_resolves_to_the_same_action() {
        let keys = every_key();
        let mut honoured = 0;
        for pane in PANES {
            for ctx in CTXS {
                for key in &keys {
                    let want = old(key, pane, ctx).or_else(|| added(key, pane));
                    assert_eq!(
                        resolve(key, pane, ctx),
                        want,
                        "{:?} {:?} in {pane:?} under {ctx:?}",
                        key.code,
                        key.modifiers
                    );
                    honoured += usize::from(want.is_some());
                }
            }
        }
        assert!(honoured > 1000, "only {honoured} chords honoured");
    }

    /// Positive control for the equivalence test: a table that loses a
    /// chord, or gives one another action, is caught by the same comparison.
    #[test]
    fn the_equivalence_test_sees_a_changed_chord() {
        const LOST: &[Row<Action>] = &[];
        let m = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE);
        let browse = PANES[0];
        assert_eq!(old(&m, browse, Ctx::NONE), Some(Action::Mark));
        assert_eq!(keymap::best(LOST, &m, browse, Ctx::NONE), None);
        const SWAPPED: &[Row<Action>] = &[Row::new(
            "x",
            "x",
            &[&[key(KeyCode::Char('m'), Mods::Plain, Action::Copy, BROWSE)]],
        )];
        assert_ne!(
            keymap::best(SWAPPED, &m, browse, Ctx::NONE).map(|(_, a)| a),
            old(&m, browse, Ctx::NONE)
        );
    }

    /// Every row's words, notes and group are catalogue keys, in English
    /// and German, so the help never shows a key name.
    #[test]
    fn every_row_speaks_the_catalogue() {
        for row in ROWS {
            for key in [Some(row.desc), Some(row.group), row.note]
                .into_iter()
                .flatten()
            {
                assert!(id_of(key).is_some(), "{key:?} is not in the catalogue");
            }
            assert!(GROUPS.contains(&row.group), "{:?}", row.group);
        }
        for title in GROUPS {
            assert!(id_of(title).is_some(), "{title:?}");
        }
    }

    /// The help lists exactly the chords the handler honours: every row of
    /// both tables with its derived keys, and no keys besides them but the
    /// two mouse lines.
    #[test]
    fn the_help_lists_exactly_the_tables_chords() {
        let groups = help_groups_in(Lang::En);
        let shown: Vec<&str> = groups
            .iter()
            .flat_map(|g| g.entries.iter().map(|e| e.keys))
            .filter(|k| !k.is_empty())
            .collect();
        let mut want: Vec<&str> = SCROLL_ROWS.iter().map(|r| r.keys()).collect();
        want.push(StrId::HelpWheel.text(Lang::En));
        for group in GROUPS {
            for row in ROWS.iter().filter(|r| r.group == group) {
                if !row.keys().is_empty() {
                    want.push(row.keys());
                }
            }
            if group == FOLLOW {
                want.push(StrId::HelpHoverKeys.text(Lang::En));
            }
        }
        let mut shown_sorted = shown.clone();
        shown_sorted.sort_unstable();
        want.sort_unstable();
        assert_eq!(shown_sorted, want);
    }

    /// The table's size per mode: how many chords are in force in each
    /// pane, counted over both tables, printed for the record and held so a
    /// lost row shows.
    #[test]
    fn the_tables_size_per_mode() {
        let count = |pane: Panes| {
            ROWS.iter()
                .flat_map(|r| r.chords())
                .filter(|c| c.panes.meets(pane))
                .count()
                + SCROLL_ROWS
                    .iter()
                    .flat_map(|r| r.chords())
                    .filter(|c| c.panes.meets(pane))
                    .count()
        };
        let sizes: Vec<(&str, usize)> = [
            ("browse", PANES[0]),
            ("address", ADDRESS),
            ("search", SEARCH),
            ("field", FIELD),
            ("hint", HINT),
            ("help", PANES[5]),
            ("places", PANES[6]),
            ("names", PANES[7]),
            ("ask", ASK),
        ]
        .into_iter()
        .map(|(name, pane)| (name, count(pane)))
        .collect();
        std::println!("chords per mode: {sizes:?}");
        assert_eq!(
            sizes,
            [
                ("browse", 47),
                ("address", 4),
                ("search", 4),
                ("field", 6),
                ("hint", 3),
                ("help", 25),
                ("places", 28),
                ("names", 24),
                ("ask", 3),
            ]
        );
    }
}
