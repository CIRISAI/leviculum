//! Events in: [`AppEvent`], and [`update`], which folds one into the
//! [`Model`] and returns the effects it asks the shell to perform.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use leviculum_micron::{FieldKind, MicronDocument};

use crate::bookmarks::Bookmark;
use crate::discovery::DiscoveredNode;
use crate::image::{ImageSlot, Probe, SlotState, MAX_INLINE_IMAGES};
use crate::names::{direct_hit, search, VisitOutcome};
use crate::page_cache::CacheEntry;
use crate::render::{standalone_links, RenderedLink};
use crate::url::{
    classify_link, is_nururl, parse_nururl, parse_url, resolve_link, LinkKind, Target, UrlError,
    DEFAULT_PATH,
};

use super::chrome::*;
use super::geometry::{rect_contains, regions, Rect};
use super::input::{InputBuffer, KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use super::intent::{apply, Intent};
use super::keys;
use super::model::*;
use super::strings::{with, with_args, Arg, Lang, StrId, Text};
#[cfg(test)]
pub(crate) use leviculum_ui::keymap::{self, Ctx, Panes, SCROLL_ROWS};

/// A UI input event, decoupled from crossterm so [`update`] is trivially
/// testable and the event source can be swapped later.
#[derive(Clone, Debug)]
pub enum AppEvent {
    /// A key was pressed.
    Key(KeyEvent),
    /// A mouse event occurred (wheel scrolling, left-click to follow a link or
    /// activate a top-bar control, and move-to-hover).
    Mouse(MouseEvent),
    /// The terminal was resized to `(cols, rows)`.
    Resize(u16, u16),
    /// An explicit request to quit.
    Quit,
    /// A navigation completed: the parsed document and its resolved title.
    PageLoaded {
        /// The freshly parsed page.
        doc: MicronDocument,
        /// The resolved top-bar title for it.
        title: String,
    },
    /// A navigation failed with a human-readable message.
    LoadFailed(String),
    /// A navigation failed, in the catalogue's words (the shell's fetch).
    LoadFailedText(Text),
    /// A file download completed and was saved to disk.
    DownloadDone {
        /// The filename it was saved as.
        name: String,
        /// The number of bytes saved.
        bytes: usize,
        /// The directory it was saved into, for the toast.
        dir: String,
    },
    /// A file download failed, with what the reader is told.
    DownloadFailed(Text),
    /// An inline image's transfer started.
    ImageLoading {
        /// The link index it belongs to.
        link: usize,
    },
    /// An inline image arrived, decoded, and is ready to be drawn at `cells`.
    /// The encoded protocol itself lives in the IO shell's `ImageStore`; the
    /// model only needs the size, so it stays pure data.
    ImageReady {
        /// The link index it belongs to.
        link: usize,
        /// The cell size it will be drawn at.
        cells: (u16, u16),
        /// What the payload turned out to be.
        probe: Probe,
    },
    /// An inline image arrived but will not be drawn: the terminal cannot show
    /// pictures, or the payload did not decode. The reader gets a status line
    /// instead, which is the whole point of keeping this distinct from a
    /// failure.
    ImageText {
        /// The link index it belongs to.
        link: usize,
        /// What the payload turned out to be, if it could be read at all.
        probe: Option<Probe>,
        /// Why there is no picture.
        reason: String,
    },
    /// An inline image's transfer failed.
    ImageFailed {
        /// The link index it belongs to.
        link: usize,
        /// What went wrong.
        reason: String,
    },
    /// A NomadNet node announce was seen on the session's event stream; fold it
    /// into the model's discovered-node registry.
    NodeDiscovered(DiscoveredNode),
    /// The spinner animation tick, delivered while a fetch is in flight.
    Tick,
    /// What a tap on a scene node, or a host, asks ([`super::intent`]).
    Intent(super::intent::Intent),
}

/// Fold an event into the model, returning any [`Effect`]s for the IO shell.
/// Pure and IO-free.
pub fn update(model: &mut Model, event: AppEvent) -> Vec<Effect> {
    match event {
        AppEvent::Quit => {
            model.quit = true;
            vec![Effect::Quit]
        }
        AppEvent::Tick => {
            model.spin = model.spin.wrapping_add(1);
            model.now_tick = model.now_tick.wrapping_add(1);
            model.expire_toast();
            Vec::new()
        }
        AppEvent::PageLoaded { doc, title } => apply_loaded(model, doc, title),
        AppEvent::Intent(intent) => super::intent::apply(model, intent),
        AppEvent::LoadFailed(msg) => {
            if show_failed(model) {
                return Vec::new();
            }
            model.pending = None;
            model.set_toast(ToastKind::Error, msg);
            Vec::new()
        }
        AppEvent::LoadFailedText(text) => {
            if show_failed(model) {
                return Vec::new();
            }
            model.pending = None;
            model.say(ToastKind::Error, text);
            Vec::new()
        }
        AppEvent::DownloadDone { name, bytes, dir } => {
            model.download = None;
            model.say(
                ToastKind::Info,
                with_args(
                    StrId::Downloaded,
                    vec![Arg::Str(name), Arg::Int(bytes as i64), Arg::Str(dir)],
                ),
            );
            Vec::new()
        }
        AppEvent::DownloadFailed(msg) => {
            model.download = None;
            model.say(ToastKind::Error, msg);
            Vec::new()
        }
        AppEvent::ImageLoading { link } => {
            model.set_image_state(link, SlotState::Loading);
            Vec::new()
        }
        AppEvent::ImageReady { link, cells, probe } => {
            model.set_image_state(link, SlotState::Ready { cells, probe });
            Vec::new()
        }
        AppEvent::ImageText {
            link,
            probe,
            reason,
        } => {
            model.set_image_state(link, SlotState::Text { probe, reason });
            Vec::new()
        }
        AppEvent::ImageFailed { link, reason } => {
            model.set_image_state(link, SlotState::Failed(reason));
            Vec::new()
        }
        AppEvent::NodeDiscovered(node) => {
            model.node_registry.upsert_node(&node);
            Vec::new()
        }
        AppEvent::Resize(cols, rows) => {
            model.size = (cols, rows);
            model.relayout(content_width(cols));
            let vp = model.viewport();
            model.clamp_scroll(vp);
            Vec::new()
        }
        AppEvent::Mouse(mouse) => {
            // The name-search list is keyboard driven: the wheel moves its
            // selection, and no click or hover reaches the page underneath.
            if model.names.is_some() {
                match mouse.kind {
                    MouseEventKind::ScrollDown => move_name_sel(model, ScrollCmd::LineDown),
                    MouseEventKind::ScrollUp => move_name_sel(model, ScrollCmd::LineUp),
                    _ => {}
                }
                return Vec::new();
            }
            let vp = model.viewport();
            match mouse.kind {
                // The wheel scrolls whichever window owns the screen: an open
                // overlay takes it over the page underneath.
                MouseEventKind::ScrollDown => {
                    if model.show_help {
                        let (_, max) = help_scroll_geometry(model);
                        model.help_scroll = model.help_scroll.saturating_add(WHEEL_STEP).min(max);
                    } else if model.show_places {
                        // The wheel is a VIEW control, not a cursor: it moves the
                        // scroll offset and leaves the selection alone, so no
                        // entry is ever skipped past.
                        wheel_places_view(model, ScrollCmd::LineDown);
                    } else {
                        model.scroll_lines_down(WHEEL_STEP, vp);
                    }
                    Vec::new()
                }
                MouseEventKind::ScrollUp => {
                    if model.show_help {
                        model.help_scroll = model.help_scroll.saturating_sub(WHEEL_STEP);
                    } else if model.show_places {
                        wheel_places_view(model, ScrollCmd::LineUp);
                    } else {
                        model.scroll_lines_up(WHEEL_STEP);
                    }
                    Vec::new()
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    handle_click(model, mouse.column, mouse.row)
                }
                // Mouse side buttons: Back (button 8) and Forward (button 9)
                // drive the same history navigation as Alt-Left / Alt-Right,
                // and are a no-op when there is no history in that direction.
                MouseEventKind::Down(MouseButton::Back) => go_back(model),
                MouseEventKind::Down(MouseButton::Forward) => go_forward(model),
                MouseEventKind::Moved => {
                    update_hover(model, mouse.column, mouse.row);
                    Vec::new()
                }
                _ => Vec::new(),
            }
        }
        AppEvent::Key(key) => update_key(model, key),
    }
}

/// Fold a completed page load: replace the document, relayout, reset scroll, set
/// the title, and record the navigation in history per its pending action.
pub(crate) fn apply_loaded(model: &mut Model, doc: MicronDocument, title: String) -> Vec<Effect> {
    let pending = model.pending.take();
    let anchor = model.pending_anchor.take();
    show_doc(model, doc, title);
    let loaded_target = pending.map(|pending| {
        enter_history(model, &pending);
        pending.target
    });
    model.failed = None;
    // The loaded page's node may differ from the previous one: refresh the
    // identify display flag from what the model has learned so far. The IO
    // shell follows up with the session's authoritative answer for this load.
    if let Some(dest) = model.current_dest {
        model.identifying = model.identify_known.contains(&dest);
    }
    // A followed `#anchor` scrolls its block's first line to the top; an unknown
    // anchor falls back to the top of the page with a toast note.
    if let Some(name) = anchor {
        match anchor_line(model, &name) {
            Some(line) => {
                let vp = model.viewport();
                model.scroll = line.min(model.max_scroll(vp));
            }
            None => model.say(ToastKind::Error, with(StrId::AnchorNotFound, name)),
        }
    }
    // Cache the freshly loaded page (overwriting any prior entry for this target,
    // as a reload does) so a later revisit renders instantly. A non-idempotent
    // form submit is never cached.
    let mut effects = Vec::new();
    if let Some(target) = &loaded_target {
        if pin_visit(model, target.dest_hash) {
            effects.push(Effect::SavePins);
        }
    }
    if let Some(target) = loaded_target {
        if !is_form_submit(&target) {
            model.page_cache.insert(
                target,
                CacheEntry {
                    doc: model.doc.clone(),
                    title: model.title.clone(),
                    scroll: model.scroll,
                },
            );
        }
    }
    effects.extend(plan_images(model));
    effects
}

/// Work out which of the page's links are pictures, install a slot for each,
/// and ask the IO shell to fetch them.
///
/// Called after every load, cached or fresh: the page cache holds documents,
/// not payloads, so a revisit re-fetches its pictures. The list is capped at
/// [`MAX_INLINE_IMAGES`]; the rest keep their slots (so the reader is told they
/// exist) but reserve no rows and fetch nothing until followed.
pub(crate) fn plan_images(model: &mut Model) -> Vec<Effect> {
    model.images.clear();
    if model.no_images {
        return Vec::new();
    }
    let standalone = standalone_links(&model.page, &model.links);
    let mut requests = Vec::new();
    let mut slots = Vec::new();
    for index in standalone {
        let Some(link) = model.links.iter().find(|l| l.index == index) else {
            continue;
        };
        let path = crate::image::link_path(&link.target);
        if !crate::image::is_image_path(path) {
            continue;
        }
        let Some(name) = crate::image::file_name(path).map(str::to_string) else {
            continue;
        };
        if requests.len() >= MAX_INLINE_IMAGES {
            slots.push(ImageSlot {
                link: index,
                name,
                state: SlotState::TooMany,
            });
            continue;
        }
        let Ok((target, _)) = resolve_link(link, model.current_dest) else {
            continue;
        };
        requests.push(ImageRequest {
            link: index,
            target,
        });
        slots.push(ImageSlot {
            link: index,
            name,
            state: SlotState::Queued,
        });
    }
    model.images = slots;
    if model.images.is_empty() {
        return Vec::new();
    }
    // The slots reserve a status row each, so re-lay the page out before the
    // first frame: the reader sees where the pictures will be while they load.
    let (w, vp) = (model.width, model.viewport());
    model.relayout(w);
    model.clamp_scroll(vp);
    match requests.is_empty() {
        true => Vec::new(),
        false => vec![Effect::FetchImages(requests)],
    }
}

/// The start screen's micron source in `lang`: what a reader sees when
/// `lnomad` is started without a URL. A real micron document rather than
/// hand-drawn chrome, so it goes through the same parse/layout/render path as a
/// fetched page and re-wraps on a resize or a theme change for free.
pub(crate) fn start_page(lang: Lang, hosted: bool) -> String {
    let w = |id: StrId| id.text(lang);
    if hosted {
        // A messenger's tab on a touch screen (1337): no keys to name, one
        // sentence over the places list the tab opens with.
        return format!(
            ">{}\n\n{}",
            w(StrId::StartHostedTitle),
            w(StrId::StartHosted)
        );
    }
    format!(
        ">lnomad\n\n{}\n\n{}\n\n`!d`! {}   `!:`! {}   `!?`! {}   `!q`! {}",
        w(StrId::StartTagline),
        w(StrId::StartNoPage),
        w(StrId::Places),
        w(StrId::Url),
        w(StrId::Help),
        w(StrId::Quit),
    )
}

/// The title shown for the start screen, in place of a page's node name.
pub(crate) const START_TITLE: &str = "lnomad";

/// Put `doc` on screen under `title`, at its top, every cursor into the
/// page before gone: a fresh fetch's page, or a hosted browser's failure
/// page ([`show_failed`]).
fn show_doc(model: &mut Model, doc: MicronDocument, title: String) {
    // A fresh fetch, not a cache hit: clear the "cached" marker.
    model.cached_view = false;
    model.doc = doc;
    // A fresh document replaces the old fields: clear the store so the relayout
    // lays them out from the new prefill, then rebuild the editable store from it.
    model.field_state.clear();
    model.relayout(content_width(model.size.0));
    model.rebuild_field_state();
    model.scroll = 0;
    model.title = title;
    model.dismiss_toast();
    // A fresh page invalidates any focus/hover cursor into the old link set, and
    // any search match highlights against the old page.
    model.focus = None;
    model.field_focus = None;
    model.mode = Mode::Browse;
    model.hover = None;
    model.matches.clear();
    model.current_match = None;
}

/// The navigation `pending` arrived: its destination is the current one and
/// the history moves to it. The retry of a failure page replaces that page's
/// entry rather than pushing the same target a second time (1669).
fn enter_history(model: &mut Model, pending: &Pending) {
    model.current_dest = Some(pending.target.dest_hash);
    match pending.action {
        HistoryAction::Push
            if model.failed.as_ref() == Some(&pending.target)
                && model.history.current() == Some(&pending.target) => {}
        HistoryAction::Push => model.history.visit(pending.target.clone()),
        HistoryAction::Goto(idx) => model.history.goto(idx),
    }
}

/// A hosted browser's page that did not come (1669, walk 12, finding 69:
/// the phone showed an empty page under "Pages", the address field back to
/// its placeholder and two toasts in jargon, with no way to try again). The
/// failure is the page now: its address stays the target's, it says that
/// there was no connection to the page's node by the name the host gave it
/// ([`Model::label`]) and offers to try again as a link to the same target,
/// and it is an entry of the history, so Back leaves it as it leaves a
/// page. Nothing of it is cached or pinned. `false`, and nothing changed,
/// where it does not apply: the standalone browser, a download, no fetch
/// out, and the refresh of a cached copy on screen, which stays.
fn show_failed(model: &mut Model) -> bool {
    if !model.hosted {
        return false;
    }
    let Some(pending) = model.pending.clone() else {
        return false;
    };
    if pending.target.is_file
        || (model.cached_view && model.history.current() == Some(&pending.target))
    {
        return false;
    }
    model.pending = None;
    model.pending_anchor = None;
    let name = model
        .label
        .clone()
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| short_hex(&pending.target.dest_hash));
    let said = with(StrId::FetchNoConnection, escape_micron(&name)).resolve(model.lang);
    let micron = format!(
        "\n{said}\n\n`[{}`{}]\n",
        escape_micron(StrId::FetchTryAgain.text(model.lang)),
        target_url(&pending.target)
    );
    show_doc(model, leviculum_micron::parse(&micron), name);
    enter_history(model, &pending);
    model.failed = Some(pending.target);
    if let Some(dest) = model.current_dest {
        model.identifying = model.identify_known.contains(&dest);
    }
    true
}

/// `text` as micron shows it verbatim: the escape before each character
/// micron reads as markup inside a line.
fn escape_micron(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '`' | '\\' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Show the start screen and open the places panel over it.
///
/// The page is applied through the ordinary load path with no pending
/// navigation, so nothing is fetched, no history entry is pushed and no current
/// destination is set: the top bar carries no address, `back` / `forward` /
/// `reload` stay unavailable, and the first real navigation starts the history
/// from scratch.
pub fn show_start_screen(model: &mut Model) {
    apply_loaded(
        model,
        leviculum_micron::parse(&start_page(model.lang, model.hosted)),
        START_TITLE.to_string(),
    );
    model.show_places = true;
}

/// A host opens a page anew (1621, Loquax's Explore and a chat's link):
/// the page shown, its title and its trail go at once, so nothing of the
/// page before shows while the new one loads, and the first Back from it
/// gives the browser back to the host ([`Effect::Quit`]). A fetch in flight
/// is cancelled. The cache, the bookmarks and the nodes stay.
pub fn start_trail(model: &mut Model) -> Vec<Effect> {
    let was_loading = model.is_loading();
    model.pending = None;
    model.pending_anchor = None;
    model.download = None;
    let mut effects = apply_loaded(model, MicronDocument::default(), String::new());
    model.history = super::model::History::default();
    model.current_dest = None;
    model.show_places = false;
    model.names = None;
    if was_loading {
        effects.insert(0, Effect::Cancel);
    }
    effects
}

/// Back at the start of a hosted browser's trail (1621): a fetch in
/// flight is cancelled and the browser gives way to its host
/// ([`Effect::Quit`], which a host reads as "back to where the reader came
/// from").
pub(crate) fn leave_host(model: &mut Model) -> Vec<Effect> {
    let mut effects = Vec::new();
    if model.is_loading() {
        model.pending = None;
        model.pending_anchor = None;
        model.download = None;
        effects.push(Effect::Cancel);
    }
    effects.push(Effect::Quit);
    effects
}

/// The platform's own address field now holds `value`: the address bar
/// is entered first, as a press on it enters it, and then holds the text,
/// its cursor at the end. What a window's text field says on every edit,
/// the standalone window's and the Browse tab's in Loquax's (1369).
pub fn type_address(model: &mut Model, value: &str) -> Vec<Effect> {
    let mut effects = Vec::new();
    if model.mode != Mode::Address {
        effects = apply(model, Intent::Activate(super::scene::NodeId::Address));
    }
    model.input = super::input::field_with(value);
    effects
}

/// Split a trailing `#anchor` off a target's path, returning the anchor-free
/// target and the anchor name (when non-empty). The initial URL keeps its
/// `#anchor` inside `path` after [`crate::url::parse_url`]; stripping
/// it here keeps the fetched path clean and lets the load handler scroll to it.
pub fn split_path_anchor(mut target: Target) -> (Target, Option<String>) {
    let split = target
        .path
        .split_once('#')
        .map(|(base, anchor)| (base.to_string(), anchor.to_string()));
    match split {
        Some((base, anchor)) if !anchor.is_empty() => {
            target.path = base;
            (target, Some(anchor))
        }
        _ => (target, None),
    }
}

/// Fold a key press. The key table ([`super::keys`]) names the action the
/// key runs in the pane the model is in (the help, the places panel and the
/// name list own the keys while open, else the mode); the action becomes an
/// [`Intent`] and folds through [`apply`] as a tap does. A key the table
/// does not hold goes to what the pane types into, or nowhere.
pub(crate) fn update_key(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    // Any key dismisses the current toast; the intent below may raise a fresh
    // one for this very key (e.g. `m` -> "bookmarked"), which then wins.
    model.dismiss_toast();

    let pane = keys::pane(model);
    if let Some(action) = keys::resolve(&key, pane, keys::ctx(model)) {
        return match Intent::of_key(action, model) {
            Some(intent) => apply(model, intent),
            None => Vec::new(),
        };
    }
    // While the name list asks, any key but `y` is the answer no.
    if pane == keys::ASK {
        return apply(model, Intent::Answer { yes: false });
    }
    if model.show_help || model.show_places || model.names.is_some() {
        return Vec::new();
    }
    match model.mode {
        Mode::Address => {
            model.input.edit(&key);
            Vec::new()
        }
        Mode::Search => {
            model.search_input.edit(&key);
            Vec::new()
        }
        Mode::Hint => type_hint(model, key),
        Mode::Field => type_field(model, key),
        Mode::Browse => Vec::new(),
    }
}

/// Type a key into the focused form field, the keys the table left over. A
/// text field takes the usual editing keys (typing, Backspace/Delete,
/// Left/Right, Home/End) and re-lays the page out so its widget reflects the
/// new value; a checkbox or radio takes nothing but its `Space`, which the
/// table holds. With no field focused the mode falls back to browsing.
pub(crate) fn type_field(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    let Some(fi) = model.field_focus else {
        model.mode = Mode::Browse;
        return Vec::new();
    };
    let Some(fe) = model.field_state.get(fi - 1) else {
        return Vec::new();
    };
    let editing = matches!(
        key.code,
        KeyCode::Char(_)
            | KeyCode::Backspace
            | KeyCode::Delete
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
    );
    if fe.kind == FieldKind::Text && editing {
        if let Some(fe) = model.field_state.get_mut(fi - 1) {
            fe.input.edit(&key);
        }
        // The widget text changed width: re-lay the page out so the box
        // and every downstream position stay correct, keeping the scroll.
        let (w, vp) = (model.width, model.viewport());
        model.relayout(w);
        model.clamp_scroll(vp);
    }
    Vec::new()
}

/// Set text field `fi` (1-based) to `text`, as a platform's own editor
/// hands it in, and lay the page out again around the new width, keeping
/// the scroll. A checkbox, a radio or an index off the page is left alone.
pub(crate) fn fill_field(model: &mut Model, fi: usize, text: &str) {
    let Some(fe) = fi.checked_sub(1).and_then(|i| model.field_state.get_mut(i)) else {
        return;
    };
    if fe.kind != FieldKind::Text {
        return;
    }
    fe.input.set_value(text);
    let (w, vp) = (model.width, model.viewport());
    model.relayout(w);
    model.clamp_scroll(vp);
}

/// Activate field `fi` (1-based): a checkbox toggles its checked state, a radio
/// selects itself within its group, a text field does nothing. Re-lays the page
/// out so the rendered widget shows the new state, keeping the scroll. The one
/// activation body shared by the Space key, a mouse click and a field hint.
pub(crate) fn toggle_field(model: &mut Model, fi: usize) {
    let Some(kind) = model.field_state.get(fi - 1).map(|fe| fe.kind) else {
        return;
    };
    match kind {
        FieldKind::Text => return,
        FieldKind::Checkbox => {
            if let Some(fe) = model.field_state.get_mut(fi - 1) {
                fe.checked = !fe.checked;
            }
        }
        FieldKind::Radio => select_radio(model, fi),
    }
    let (w, vp) = (model.width, model.viewport());
    model.relayout(w);
    model.clamp_scroll(vp);
}

/// Select radio field `fi` (1-based), deselecting every other radio sharing its
/// name (one selection per group), matching the reference radio-group behaviour.
pub(crate) fn select_radio(model: &mut Model, fi: usize) {
    let Some(name) = model.field_state.get(fi - 1).map(|fe| fe.name.clone()) else {
        return;
    };
    for fe in model.field_state.iter_mut() {
        if fe.kind == FieldKind::Radio && fe.name == name {
            fe.checked = false;
        }
    }
    if let Some(fe) = model.field_state.get_mut(fi - 1) {
        fe.checked = true;
    }
}

/// Type a key in hint mode, the keys the table left over. A character
/// narrows the visible labels (by label prefix or link-text substring); a
/// unique match follows the link, activates the form field, or drives the
/// control; `Backspace` takes the last character back. A non-matching key
/// is ignored.
pub(crate) fn type_hint(model: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Backspace => {
            model.hint_input.pop();
            Vec::new()
        }
        KeyCode::Char(c) => {
            let mut candidate = model.hint_input.clone();
            candidate.push(c.to_ascii_lowercase());
            let all = hints(model);
            // An exact label match activates immediately (handles single-char
            // labels, where the label is also its own prefix).
            if let Some(hint) = all.iter().find(|h| h.label == candidate) {
                let target = hint.target;
                exit_hint(model);
                return activate_hint_target(model, target);
            }
            let matches: Vec<&Hint> = all
                .iter()
                .filter(|h| hint_matches(model, h, &candidate))
                .collect();
            match matches.as_slice() {
                // A key that matches nothing is ignored (not accepted).
                [] => Vec::new(),
                [only] => {
                    let target = only.target;
                    exit_hint(model);
                    activate_hint_target(model, target)
                }
                _ => {
                    model.hint_input = candidate;
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    }
}

/// Go where the address field's text `raw` says: a URL, a `nomadnetwork://`
/// one, or a name to search (1227). What Enter in the address editor and
/// [`Intent::Go`](super::intent::Intent::Go) do.
pub(crate) fn go_to(model: &mut Model, raw: &str) -> Vec<Effect> {
    let raw = raw.to_string();
    if is_nururl(&raw) {
        return match parse_nururl(&raw) {
            Ok(nur) => {
                model.mode = Mode::Browse;
                model.input.set_value("");
                let effects = begin_navigation(model, nur.target, HistoryAction::Push, None);
                // After begin_navigation, whose toast dismissal must not
                // clear this one.
                if !nur.ignored.is_empty() {
                    model.say(
                        ToastKind::Info,
                        with(StrId::NururlIgnored, nur.ignored.join(", ")),
                    );
                }
                effects
            }
            Err(err) => {
                model.say(ToastKind::Error, bad_url_note(StrId::BadUrl, &raw, err));
                Vec::new()
            }
        };
    }
    match parse_url(&raw, model.current_dest) {
        Ok(target) => {
            model.mode = Mode::Browse;
            model.input.set_value("");
            begin_navigation(model, target, HistoryAction::Push, None)
        }
        Err(_) if is_name_query(&raw) => search_names(model, &raw),
        Err(err) => {
            model.say(ToastKind::Error, bad_url_note(StrId::BadUrl, &raw, err));
            Vec::new()
        }
    }
}

/// Enter address-entry mode with a cleared editor.
pub(crate) fn enter_address(model: &mut Model) {
    model.mode = Mode::Address;
    model.input.set_value("");
    model.dismiss_toast();
}

/// Enter hint mode with a cleared filter buffer.
pub(crate) fn enter_hint(model: &mut Model) {
    model.mode = Mode::Hint;
    model.hint_input.clear();
    model.dismiss_toast();
}

/// Leave hint mode, discarding the filter buffer.
pub(crate) fn exit_hint(model: &mut Model) {
    model.mode = Mode::Browse;
    model.hint_input.clear();
}

/// Enter in-page search mode with a cleared query editor, dropping any prior
/// match highlights so the reader starts from a clean slate.
pub(crate) fn enter_search(model: &mut Model) {
    model.mode = Mode::Search;
    model.search_input.set_value("");
    model.matches.clear();
    model.current_match = None;
    model.dismiss_toast();
}

/// Leave search mode (Esc): back to Browse, clearing the query and all match
/// highlights plus the current-match marker.
pub(crate) fn exit_search(model: &mut Model) {
    model.mode = Mode::Browse;
    model.search_input.set_value("");
    model.matches.clear();
    model.current_match = None;
    model.dismiss_toast();
}

/// Commit a search query: recompute matches over the current page, mark the
/// first as current and scroll it into view. An empty result clears the current
/// match and notes it in a toast.
pub(crate) fn commit_search(model: &mut Model, query: &str) {
    model.matches = find_matches(&model.page, query);
    if model.matches.is_empty() {
        model.current_match = None;
        model.say(ToastKind::Info, with(StrId::NoMatches, query));
    } else {
        model.current_match = Some(0);
        model.dismiss_toast();
        scroll_to_current_match(model);
    }
}

/// Advance the current match to the next one (wrapping) and scroll it into view.
/// A no-op when there are no matches.
pub(crate) fn next_match(model: &mut Model) {
    let n = model.matches.len();
    if n == 0 {
        return;
    }
    let cur = model.current_match.unwrap_or(0);
    model.current_match = Some((cur + 1) % n);
    scroll_to_current_match(model);
}

/// Move the current match to the previous one (wrapping) and scroll it into
/// view. A no-op when there are no matches.
pub(crate) fn prev_match(model: &mut Model) {
    let n = model.matches.len();
    if n == 0 {
        return;
    }
    let cur = model.current_match.unwrap_or(0);
    model.current_match = Some((cur + n - 1) % n);
    scroll_to_current_match(model);
}

/// Scroll so the current match's line is inside the viewport (minimal motion,
/// like Tab focus), clamped to the page. A no-op when there is no current match.
pub(crate) fn scroll_to_current_match(model: &mut Model) {
    let Some(ci) = model.current_match else {
        return;
    };
    let Some(m) = model.matches.get(ci) else {
        return;
    };
    let line = m.line_idx;
    let vp = model.viewport();
    if line < model.scroll {
        model.scroll = line;
    } else if vp > 0 && line >= model.scroll + vp {
        model.scroll = line + 1 - vp;
    }
    model.clamp_scroll(vp);
}

/// One row in the places panel: a saved bookmark or a discovered node. Built by
/// [`places`] as a flat, ordered list (bookmarks first, then discovered nodes)
/// that [`Model::places_sel`] indexes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A saved bookmark: the URL to reopen and its captured title.
    Bookmark {
        /// The full page URL to reopen.
        url: String,
        /// The page title captured when it was bookmarked.
        title: String,
    },
    /// A discovered NomadNet node: its default page is opened on activation.
    Node {
        /// The node's destination hash.
        dest_hash: [u8; 16],
        /// The node's display name, if its announce carried one.
        name: Option<String>,
        /// The hop count from the most recent announce, if known.
        hops: Option<u32>,
        /// Unix seconds of the most recent announce.
        last_seen: u64,
    },
}

/// A bookmark deleted from the places panel, stashed for the one-slot `u` undo:
/// enough to rebuild it and put it back where it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletedBookmark {
    /// The deleted bookmark's URL.
    pub url: String,
    /// The title it was saved under.
    pub title: String,
    /// Its position within the bookmarks list, so the undo restores it in
    /// place (clamped to the list length at restore time).
    pub index: usize,
}

/// The flat, ordered list of places: every bookmark (in insertion order), then
/// every discovered node, the one heard last first, as Loquax's Explore lists
/// them (1639, walk 5 new 24: the sheet listed the oldest first); discovery
/// order breaks a tie. The single source of truth for both the panel's
/// rendering and its selection/activation, so a row index means the same thing
/// to each.
pub fn places(model: &Model) -> Vec<Place> {
    let mut out = Vec::new();
    for b in model.bookmarks.list() {
        out.push(Place::Bookmark {
            url: b.url.clone(),
            title: b.title.clone(),
        });
    }
    let mut nodes = model.node_registry.nodes();
    nodes.sort_by_key(|n| core::cmp::Reverse(n.last_seen));
    for n in nodes {
        out.push(Place::Node {
            dest_hash: n.dest_hash,
            name: n.name.clone(),
            hops: n.hops,
            last_seen: n.last_seen,
        });
    }
    out
}

/// Toggle the places panel. Opening it resets the selection to the first row and
/// clears any transient status.
pub(crate) fn toggle_places(model: &mut Model) {
    model.show_places = !model.show_places;
    if model.show_places {
        model.places_sel = 0;
        model.places_scroll = 0;
        model.places_hover = None;
        model.dismiss_toast();
    }
}

/// Close the places panel.
pub(crate) fn close_places(model: &mut Model) {
    model.show_places = false;
    model.places_hover = None;
}

/// The places panel's line layout: the rendered LINE each entry sits on (indexed
/// by its flat [`places`] index) and the total number of panel lines. The panel
/// interleaves section titles and a blank spacer with the entry rows, so an
/// entry's selection index is not its line index; this is the single mapping the
/// renderer's sizing and the scroll math both read, so the selected row's on-
/// screen position is always exact. Mirrors the line order `render_places` draws.
pub(crate) struct PlacesLayout {
    /// `entry_line[i]` is the rendered line of the i-th entry in [`places`].
    pub(crate) entry_line: Vec<usize>,
    /// Total rendered panel lines: titles, the spacer, placeholders and rows.
    pub(crate) total: usize,
}

/// Compute the [`PlacesLayout`] for the current model, walking the same line
/// order [`render_places`] builds so the two never disagree.
pub(crate) fn places_layout(model: &Model) -> PlacesLayout {
    let count = places(model).len();
    let bm_count = model.bookmarks.len();
    let mut entry_line = vec![0usize; count];
    let mut line = 0usize;
    line += 1; // "Bookmarks" title
    if bm_count == 0 {
        line += 1; // "  (none)"
    } else {
        for slot in entry_line.iter_mut().take(bm_count) {
            *slot = line;
            line += 1;
        }
    }
    line += 1; // blank spacer
    line += 1; // "Discovered nodes" title
    if count == bm_count {
        line += 1; // "  (none)"
    } else {
        for slot in entry_line.iter_mut().skip(bm_count) {
            *slot = line;
            line += 1;
        }
    }
    PlacesLayout {
        entry_line,
        total: line,
    }
}

/// The places overlay's inner viewport in rows for a panel of `total` lines: the
/// overlay is at most the full terminal height, two rows go to the border.
/// Mirrors `render_places`'s sizing so the scroll math and the draw agree.
pub(crate) fn places_viewport(model: &Model, total: usize) -> usize {
    let rows = model.size.1 as usize;
    let height = (total + 2).min(rows);
    height.saturating_sub(2)
}

/// The places overlay's screen rectangle: the same centred box `render_places`
/// draws, sized from the panel's line count and capped to the terminal. Shared by
/// the renderer and the click / hover hit-test so the two never disagree on where
/// the panel and its rows sit.
pub(crate) fn places_overlay_rect(model: &Model, area: Rect) -> Rect {
    let total = places_layout(model).total;
    let width = 72u16.min(area.width);
    let height = (total as u16 + 2).min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

/// The places-panel entry under screen point `(col, row)`, honouring the panel's
/// scroll offset, or `None` when the point is not on an entry row — the border, a
/// section title, the blank spacer, a `(none)` placeholder, the scrollbar, or
/// anywhere outside the panel. The screen row maps to a panel line through the
/// clamped scroll offset, and that line maps back to an entry through the same
/// `entry_line` table the scroll math tracks, so a click can never land on the
/// wrong row.
pub(crate) fn place_at(model: &Model, col: u16, row: u16) -> Option<usize> {
    let overlay = places_overlay_rect(model, model.frame_area());
    let inner_top = overlay.y + 1;
    // Strictly inside the border: the top/bottom border rows and the left/right
    // border columns (the right one carries the scrollbar) are not entry rows.
    if row < inner_top
        || row + 1 >= overlay.bottom()
        || col <= overlay.x
        || col + 1 >= overlay.right()
    {
        return None;
    }
    let layout = places_layout(model);
    let vp = places_viewport(model, layout.total);
    let max = layout.total.saturating_sub(vp);
    let offset = model.places_scroll.min(max);
    let line = offset + (row - inner_top) as usize;
    layout.entry_line.iter().position(|&l| l == line)
}

/// Whether a places row shows the `×` delete marker: a bookmark row that is the
/// selection or under the mouse (the browser-tab convention). A node row never
/// shows one — discovered nodes are not deletable. The single predicate behind
/// both the renderer and the click hit-test.
pub(crate) fn marks_x(place: &Place, idx: usize, sel: usize, hover: Option<usize>) -> bool {
    matches!(place, Place::Bookmark { .. }) && (idx == sel || hover == Some(idx))
}

/// The `×` delete marker's column within the panel's inner width: the LAST
/// inner cell, or `None` when the panel is too narrow to reserve one. The one
/// place the marker's cell is computed, so [`place_line`]'s rendering and the
/// click hit-test can never disagree on where it sits.
pub(crate) fn place_x_col(inner: usize) -> Option<usize> {
    (inner >= 2).then(|| inner - 1)
}

/// The `×` marker's SCREEN column for the current overlay geometry, or `None`
/// when the panel has no room for one. Derived from the same overlay rect and
/// inner width `render_places` draws with.
pub(crate) fn place_x_screen_col(model: &Model) -> Option<u16> {
    let overlay = places_overlay_rect(model, model.frame_area());
    let inner = overlay.width.saturating_sub(2) as usize;
    place_x_col(inner).map(|c| overlay.x + 1 + c as u16)
}

/// Whether places row `idx` currently shows the `×` delete marker, through the
/// same [`marks_x`] predicate the renderer uses.
pub(crate) fn place_shows_x(model: &Model, idx: usize) -> bool {
    places(model)
        .get(idx)
        .is_some_and(|p| marks_x(p, idx, model.places_sel, model.places_hover))
}

/// Scroll the places view the MINIMUM needed to keep the selected entry's row
/// inside the viewport, then clamp it to the list. The single piece of view
/// state, so the selected row can never end up off-screen (mirrors the content's
/// keep-line-visible logic).
pub(crate) fn clamp_places_scroll(model: &mut Model) {
    let layout = places_layout(model);
    let vp = places_viewport(model, layout.total);
    let max = layout.total.saturating_sub(vp);
    let sel_line = layout
        .entry_line
        .get(model.places_sel)
        .copied()
        .unwrap_or(0);
    if sel_line < model.places_scroll {
        model.places_scroll = sel_line;
    } else if vp > 0 && sel_line >= model.places_scroll + vp {
        model.places_scroll = sel_line + 1 - vp;
    }
    model.places_scroll = model.places_scroll.min(max);
}

/// Scroll the places VIEW (not the selection) by `WHEEL_STEP` lines in `cmd`'s
/// direction, clamped to `0..=max` through the shared [`scrolled`] rule. This is
/// the mouse wheel: unlike the keys it never touches [`Model::places_sel`], so a
/// notch cannot jump over entries and leave them unreachable. A later key motion
/// moves the selection by one and `clamp_places_scroll` snaps the view back to
/// it — the familiar file-manager behaviour.
pub(crate) fn wheel_places_view(model: &mut Model, cmd: ScrollCmd) {
    let layout = places_layout(model);
    let vp = places_viewport(model, layout.total);
    let max = layout.total.saturating_sub(vp);
    for _ in 0..WHEEL_STEP {
        model.places_scroll = scrolled(model.places_scroll, cmd, vp, max);
    }
}

/// Apply a [`ScrollCmd`] to the places-panel selection through the shared
/// [`scrolled`] rule, so the panel and the page move by the same semantics. Line
/// motions step one entry; page/half-page motions move by the number of entries
/// that fit in the overlay's inner height; top/bottom select the first/last
/// entry. The view then follows via [`clamp_places_scroll`]. A no-op when empty.
pub(crate) fn apply_places_scroll(model: &mut Model, cmd: ScrollCmd) {
    let len = places(model).len();
    if len == 0 {
        model.places_sel = 0;
        model.places_scroll = 0;
        return;
    }
    let vp = places_viewport(model, places_layout(model).total);
    model.places_sel = scrolled(model.places_sel, cmd, vp, len - 1);
    clamp_places_scroll(model);
}

/// Open the selected place: a bookmark's URL, or a discovered node's default
/// page. Closes the panel and starts a fresh navigation. A malformed bookmark
/// URL surfaces an error toast instead.
pub(crate) fn activate_place(model: &mut Model, idx: usize) -> Vec<Effect> {
    let Some(place) = places(model).into_iter().nth(idx) else {
        return Vec::new();
    };
    let target = match place {
        Place::Bookmark { url, .. } => match parse_url(&url, model.current_dest) {
            Ok(target) => target,
            Err(err) => {
                model.say(
                    ToastKind::Error,
                    bad_url_note(StrId::BadBookmark, &url, err),
                );
                return Vec::new();
            }
        },
        Place::Node { dest_hash, .. } => node_target(dest_hash),
    };
    close_places(model);
    begin_navigation(model, target, HistoryAction::Push, None)
}

/// A node's default page: what opening a discovered node or a name-search hit
/// fetches.
pub(crate) fn node_target(dest_hash: [u8; 16]) -> Target {
    Target {
        dest_hash,
        path: DEFAULT_PATH.to_string(),
        fields: Vec::new(),
        is_file: false,
    }
}

/// Whether a typed address that did not parse as a URL is a name to search
/// for ([`crate::names`]). Everything is, except what is plainly a mistyped
/// address: a local URL or a bare path (`:`, `/`), a backtick field, any
/// `scheme://` form (`nomadnet://`, `lxmf://`), and an `lxmf@` address.
pub(crate) fn is_name_query(raw: &str) -> bool {
    !raw.is_empty()
        && !raw.starts_with([':', '/'])
        && !raw.contains('`')
        && !raw.contains("://")
        && !matches!(classify_link(raw), LinkKind::Lxmf(_))
}

/// Search the announces seen and the pinned sites for a typed name. No hit
/// says so; the one exact, unambiguous name opens directly; anything else
/// opens the result list.
pub(crate) fn search_names(model: &mut Model, raw: &str) -> Vec<Effect> {
    let hits = search(&model.node_registry, &model.pins, raw);
    if hits.is_empty() {
        model.say(ToastKind::Info, with(StrId::NoSiteNamed, raw));
        return Vec::new();
    }
    model.mode = Mode::Browse;
    model.input.set_value("");
    if let Some(hit) = direct_hit(&hits, raw) {
        let dest = hit.dest;
        return begin_navigation(model, node_target(dest), HistoryAction::Push, None);
    }
    model.names = Some(NameResults {
        query: raw.to_string(),
        hits,
        sel: 0,
        confirm: false,
    });
    Vec::new()
}

/// The name-search list's viewport in rows: the overlay is at most the full
/// terminal height, minus its border and its one hint line.
pub(crate) fn names_viewport(model: &Model) -> usize {
    (model.size.1 as usize).saturating_sub(3).max(1)
}

/// Move the name-search selection by a scroll motion, leaving any question.
pub(crate) fn move_name_sel(model: &mut Model, cmd: ScrollCmd) {
    let vp = names_viewport(model);
    if let Some(results) = model.names.as_mut() {
        if results.hits.is_empty() {
            return;
        }
        results.confirm = false;
        results.sel = scrolled(results.sel, cmd, vp, results.hits.len() - 1);
    }
}

/// Close the list and open its selected site. `confirmed` is the reader's
/// yes to a site whose name is pinned to another hash: the site's hash is then
/// pinned under that name too, beside the first, before the fetch starts.
pub(crate) fn open_name_hit(model: &mut Model, confirmed: bool) -> Vec<Effect> {
    let Some(results) = model.names.take() else {
        return Vec::new();
    };
    let Some(hit) = results.hits.get(results.sel).cloned() else {
        return Vec::new();
    };
    let mut effects = Vec::new();
    if confirmed {
        if let Some(name) = &hit.name {
            let now = model.wall_now();
            if model.pins.pin(name, &hit.dest, now) {
                effects.push(Effect::SavePins);
            }
        }
    }
    effects.extend(begin_navigation(
        model,
        node_target(hit.dest),
        HistoryAction::Push,
        None,
    ));
    effects
}

/// Record a fresh load of `dest` against the pins: the first visit pins the
/// name it announced, a later one moves its `last_seen`. Returns whether the
/// pins changed and so need saving.
pub(crate) fn pin_visit(model: &mut Model, dest: [u8; 16]) -> bool {
    let name = model
        .node_registry
        .get_by_hash(&dest)
        .and_then(|n| n.name.clone());
    let now = model.wall_now();
    model.pins.note_visit(name.as_deref(), &dest, now) != VisitOutcome::Unchanged
}

/// Delete the bookmark on places row `idx` and persist: stash it in the
/// one-slot undo ([`Model::last_deleted_bookmark`]), keep the selection in
/// range, and toast the title with the undo hint. The single delete path,
/// shared by `x`/`Delete` on the selection and a click on a row's `×` marker.
/// A no-op (no save, no toast) when that row is a discovered node.
pub(crate) fn delete_bookmark_at(model: &mut Model, idx: usize) -> Vec<Effect> {
    let Some(Place::Bookmark { url, title }) = places(model).into_iter().nth(idx) else {
        return Vec::new();
    };
    // Bookmarks lead the flat places list, so the place index IS the bookmark's
    // position within the bookmarks list — the slot the undo restores it to.
    model.last_deleted_bookmark = Some(DeletedBookmark {
        url: url.clone(),
        title: title.clone(),
        index: idx,
    });
    model.bookmarks.remove(&url);
    // Keep the selection on the entry it was on (rows above the selection shift
    // up by one), then clamp it into the shrunk list.
    if model.places_sel > idx {
        model.places_sel -= 1;
    }
    let len = places(model).len();
    model.places_sel = model.places_sel.min(len.saturating_sub(1));
    clamp_places_scroll(model);
    let label = if title.is_empty() { url } else { title };
    model.say(ToastKind::Info, with(StrId::RemovedUndo, label));
    vec![Effect::SaveBookmarks]
}

/// Undo the last places-panel bookmark deletion (`u`): re-insert the stashed
/// bookmark at its remembered position (clamped to the current length, so an
/// out-of-range slot appends), clear the one-slot stash, select the restored
/// row and persist. A muted no-op toast when there is nothing to undo, and no
/// save when the URL was re-bookmarked in the meantime.
pub(crate) fn undo_delete_bookmark(model: &mut Model) -> Vec<Effect> {
    let Some(del) = model.last_deleted_bookmark.take() else {
        model.say(ToastKind::Info, Text::key(StrId::NothingToUndo));
        return Vec::new();
    };
    let at = del.index.min(model.bookmarks.len());
    let restored = model.bookmarks.insert(
        at,
        Bookmark {
            url: del.url.clone(),
            title: del.title.clone(),
        },
    );
    if !restored {
        model.say(ToastKind::Info, with(StrId::AlreadyBookmarked, del.url));
        return Vec::new();
    }
    model.places_sel = at;
    clamp_places_scroll(model);
    let label = if del.title.is_empty() {
        del.url
    } else {
        del.title
    };
    model.say(ToastKind::Info, with(StrId::Restored, label));
    vec![Effect::SaveBookmarks]
}

/// Toggle a bookmark for the current page: remove it when the page is already
/// bookmarked, else add it under the current title. Persists on change. A no-op
/// with a toast note when nothing is loaded.
pub(crate) fn toggle_bookmark_current(model: &mut Model) -> Vec<Effect> {
    let Some(url) = current_url(model) else {
        model.say(ToastKind::Info, Text::key(StrId::NothingToBookmark));
        return Vec::new();
    };
    if model.bookmarks.contains(&url) {
        model.bookmarks.remove(&url);
        model.say(ToastKind::Info, with(StrId::RemovedBookmark, url));
    } else {
        let title = model.title.trim().to_string();
        model.bookmarks.add(Bookmark {
            url: url.clone(),
            title,
        });
        model.say(ToastKind::Info, with(StrId::Bookmarked, url));
    }
    vec![Effect::SaveBookmarks]
}

/// Yank the focused link's target URL, or (with nothing focused) the current
/// page URL, to the clipboard. A no-op with a toast note when there is nothing
/// to copy.
pub(crate) fn yank_url(model: &mut Model) -> Vec<Effect> {
    let url = match focused_link_url(model).or_else(|| current_url(model)) {
        Some(url) => url,
        None => {
            model.say(ToastKind::Info, Text::key(StrId::NothingToCopy));
            return Vec::new();
        }
    };
    model.say(ToastKind::Info, with(StrId::Copied, url.clone()));
    vec![Effect::Copy(url)]
}

/// The full URL of the focused link's resolved target, or `None` when no link is
/// focused (or it fails to resolve).
pub(crate) fn focused_link_url(model: &Model) -> Option<String> {
    let idx = model.focus?;
    let link = model.links.iter().find(|l| l.index == idx)?;
    let (target, _anchor) = resolve_link(link, model.current_dest).ok()?;
    Some(target_url(&target))
}

/// The full URL of the current page (`<dest_hex>:<path>`), or `None` when no
/// page is loaded.
pub(crate) fn current_url(model: &Model) -> Option<String> {
    model.history.current().map(target_url)
}

/// A full, reopenable URL for a target: `<dest_hex>:<path>`, with any query
/// fields reattached as the backtick blob [`parse_url`] understands (the stored
/// `var_` key prefix stripped back off).
pub(crate) fn target_url(target: &Target) -> String {
    let base = format!("{}:{}", full_hex(&target.dest_hash), target.path);
    if target.fields.is_empty() {
        return base;
    }
    let blob = target
        .fields
        .iter()
        .map(|(k, v)| format!("{}={}", k.strip_prefix("var_").unwrap_or(k), v))
        .collect::<Vec<_>>()
        .join("|");
    format!("{base}`{blob}")
}

/// The full 32-character lowercase hex of a destination hash.
pub(crate) fn full_hex(dest: &[u8; 16]) -> String {
    let mut s = String::with_capacity(dest.len() * 2);
    for byte in dest {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// The OSC 52 clipboard-set sequence the IO shell writes for a copy: shared
/// with loquax's drag selection, in `leviculum-ui`.
pub use leviculum_ui::clipboard::osc52;

/// Whether `target` is a non-idempotent form submit: it carries at least one
/// `field_<name>` value collected from an on-page form. Such requests are never
/// served from (or stored in) the page cache. Preset-only (`var_`) targets are
/// idempotent and cacheable.
pub(crate) fn is_form_submit(target: &Target) -> bool {
    target.fields.iter().any(|(k, _)| k.starts_with("field_"))
}

/// Start a navigation to `target`, recording it in history per `action` and
/// scrolling to `anchor` (if any) once shown.
///
/// A cache hit for an idempotent target renders instantly from
/// [`Model::page_cache`] with its stored scroll restored and emits NO fetch and
/// NO spinner. A miss, or a non-idempotent form submit (never cached), sets the
/// pending fetch and emits [`Effect::Navigate`]. The single entry point every
/// navigation (address bar, link, bookmark, back/forward) routes through so the
/// cache is checked uniformly; a reload deliberately bypasses it.
pub(crate) fn begin_navigation(
    model: &mut Model,
    target: Target,
    action: HistoryAction,
    anchor: Option<String>,
) -> Vec<Effect> {
    // A /file/ target is a download, not a navigation: the shown page and the
    // history stay put while the IO shell fetches and saves the bytes.
    if target.is_file {
        return begin_download(model, target);
    }

    // Remember where the reader was on the page being left, so a later revisit
    // of it restores the scroll position.
    model.stash_scroll();
    model.dismiss_toast();

    if !is_form_submit(&target) {
        if let Some(entry) = model.page_cache.get(&target).cloned() {
            return load_from_cache(model, target, action, anchor, entry);
        }
    }

    model.pending = Some(Pending {
        target: target.clone(),
        action,
    });
    model.pending_anchor = anchor;
    // The fetch supersedes any download in flight (they share the IO slot).
    model.download = None;
    vec![Effect::Navigate(target)]
}

/// Start a file download for `target`: mark it in flight (which shows the
/// loading indicator) and hand the fetch+save to the IO shell. Supersedes any
/// fetch or download already in flight, exactly as starting a fetch does.
pub(crate) fn begin_download(model: &mut Model, target: Target) -> Vec<Effect> {
    model.dismiss_toast();
    model.pending = None;
    model.pending_anchor = None;
    model.download = Some(crate::download::basename(&target.path));
    vec![Effect::Download(target)]
}

/// Show `target` from its cached parsed document: relayout at the current width/
/// theme, restore the stored scroll (or scroll to `anchor`), fold the navigation
/// into history exactly as a fresh load would, and flag the view as cached.
///
/// Emits no fetch. When a fetch was in flight it emits [`Effect::Cancel`] so a
/// late result cannot clobber the page we just restored.
pub(crate) fn load_from_cache(
    model: &mut Model,
    target: Target,
    action: HistoryAction,
    anchor: Option<String>,
    entry: CacheEntry,
) -> Vec<Effect> {
    let was_loading = model.is_loading();
    model.pending = None;
    model.pending_anchor = None;
    model.download = None;

    model.doc = entry.doc;
    model.title = entry.title;
    // The cached document brings its own fields: rebuild the editable store from
    // the freshly laid-out prefill, exactly as a fresh load does.
    model.field_state.clear();
    model.relayout(content_width(model.size.0));
    model.rebuild_field_state();

    // A different page invalidates any focus/hover cursor and search matches
    // held against the old one.
    model.focus = None;
    model.field_focus = None;
    model.mode = Mode::Browse;
    model.hover = None;
    model.matches.clear();
    model.current_match = None;

    model.current_dest = Some(target.dest_hash);
    // A cache hit asks the session nothing, so the identify display flag comes
    // from what the model learned when this run fetched (or toggled) the node.
    model.identifying = model.identify_known.contains(&target.dest_hash);
    match action {
        HistoryAction::Push => model.history.visit(target),
        HistoryAction::Goto(idx) => model.history.goto(idx),
    }

    // Restore the reader's last scroll, or scroll to a followed `#anchor`.
    let vp = model.viewport();
    match anchor {
        Some(name) => match anchor_line(model, &name) {
            Some(line) => model.scroll = line.min(model.max_scroll(vp)),
            None => {
                model.scroll = 0;
                model.say(ToastKind::Error, with(StrId::AnchorNotFound, name));
            }
        },
        None => model.scroll = entry.scroll.min(model.max_scroll(vp)),
    }

    model.cached_view = true;

    // The page cache holds documents, not payloads: a revisit re-fetches its
    // pictures. Cheap to change later (the store could keep the bytes), but
    // caching an unbounded pile of photographs is not obviously the kindness
    // it looks like.
    let mut effects = plan_images(model);
    if was_loading {
        effects.insert(0, Effect::Cancel);
    }
    effects
}

/// Reload the page under the history cursor without changing history.
pub(crate) fn reload_current(model: &mut Model) -> Vec<Effect> {
    let Some(target) = model.history.current().cloned() else {
        return Vec::new();
    };
    let idx = model.history.idx;
    model.pending = Some(Pending {
        target: target.clone(),
        action: HistoryAction::Goto(idx),
    });
    model.pending_anchor = None;
    model.download = None;
    model.dismiss_toast();
    vec![Effect::Navigate(target)]
}

/// Toggle identifying to the current page's node: flip the model's flag
/// optimistically (the shell's post-reload refresh is authoritative), ask the
/// shell to persist the decision, and reload the page — identify binds to the
/// link, so the new decision only takes effect on a fresh one. The single
/// source of truth for both the `i` key and the footer's identify button.
pub(crate) fn toggle_identify(model: &mut Model) -> Vec<Effect> {
    let Some(dest) = model.current_dest else {
        return Vec::new();
    };
    let on = !model.identifying;
    model.note_identify(dest, on);
    let mut effects = vec![Effect::SetIdentify { dest, on }];
    effects.extend(reload_current(model));
    // After reload_current, whose toast dismissal must not clear this one.
    let dest8: String = crate::identity::fingerprint_hex(&dest)
        .chars()
        .take(8)
        .collect();
    if on && model.hosted && !model.identify_explained {
        // A host's reader has no help screen and no `i` key (1650): the
        // first switch-on says what it means.
        model.identify_explained = true;
        model.say(ToastKind::Info, Text::key(StrId::IdentifyExplained));
    } else if on {
        let fp8: String = model.fingerprint.chars().take(8).collect();
        model.say(
            ToastKind::Info,
            with_args(StrId::IdentifyingTo, vec![Arg::Str(dest8), Arg::Str(fp8)]),
        );
    } else {
        model.say(ToastKind::Info, with(StrId::AnonymousTo, dest8));
    }
    effects
}

/// Navigate back one history entry (re-fetching it), if possible.
pub(crate) fn go_back(model: &mut Model) -> Vec<Effect> {
    if !model.history.can_back() {
        return Vec::new();
    }
    let idx = model.history.idx - 1;
    let target = model.history.stack[idx].clone();
    begin_navigation(model, target, HistoryAction::Goto(idx), None)
}

/// Navigate forward one history entry (re-fetching it), if possible.
pub(crate) fn go_forward(model: &mut Model) -> Vec<Effect> {
    if !model.history.can_forward() {
        return Vec::new();
    }
    let idx = model.history.idx + 1;
    let target = model.history.stack[idx].clone();
    begin_navigation(model, target, HistoryAction::Goto(idx), None)
}

/// Follow the link with 1-based `index`: resolve its target against the current
/// destination and start a fresh navigation, or raise an error toast.
pub(crate) fn follow_link(model: &mut Model, index: usize) -> Vec<Effect> {
    let Some(link) = model.links.iter().find(|l| l.index == index).cloned() else {
        return Vec::new();
    };
    // A picture already on the page has been transferred once; following its
    // link saves those bytes rather than fetching them again. Over a mesh link
    // that difference is minutes, not milliseconds.
    if let Some(effect) = saved_image_effect(model, index, false) {
        return vec![effect];
    }
    // An external URL is opened in the user's default handler; an unsafe scheme
    // is refused outright. Only an RNS target is fetched in-mesh.
    match classify_link(&link.target) {
        LinkKind::External(url) => {
            model.dismiss_toast();
            vec![Effect::OpenExternal(url)]
        }
        LinkKind::Unsafe(scheme) => {
            model.say(ToastKind::Error, with(StrId::WontOpen, scheme));
            Vec::new()
        }
        // NomadNet opens a conversation here. We are a page browser with no
        // composer, so the useful thing is to hand the address over: show it
        // and put it on the clipboard, ready to paste into a client that can
        // send. Anything is better than the "malformed URL" this used to
        // produce, which read as a broken page rather than an unsupported
        // action.
        // Hosted in a messenger (a tab of Loquax), there is a composer: the
        // address goes to it, as NomadNet's does.
        LinkKind::Lxmf(hash) if model.hosted => {
            model.dismiss_toast();
            vec![Effect::Write(hash)]
        }
        LinkKind::Lxmf(hash) => {
            model.say(ToastKind::Info, with(StrId::LxmfCopied, hash.clone()));
            vec![Effect::Copy(hash)]
        }
        LinkKind::Rns => match resolve_link(&link, model.current_dest) {
            Ok((mut target, anchor)) => {
                // A submit link (one that references form fields, or `*` for all)
                // carries the CURRENT field values, packaged as NomadNet expects:
                // `field_<name>` entries alongside the `var_<key>` presets that
                // resolve_link already put on the target. Preset `var_` and
                // collected `field_` keys never collide, so we just append.
                target.fields.extend(collect_submit_fields(model, &link));
                // A submit target is routed through the same entry point but is
                // never served from cache (see [`is_form_submit`]); any `#anchor`
                // is resolved against the shown page.
                begin_navigation(model, target, HistoryAction::Push, anchor)
            }
            Err(err) => {
                model.say(
                    ToastKind::Error,
                    bad_url_note(StrId::BadLink, &link.target, err),
                );
                Vec::new()
            }
        },
    }
}

/// The toast for a URL that would not parse: what was wrong and, when the fix
/// is mechanical, the URL that would have worked.
///
/// A page that links to its own node with `/page/about.mu` has dropped the `:`
/// that a local URL keeps. That is a dead end in the reference browser too, so
/// the useful thing is not to restate the grammar but to show the reader (and,
/// through them, the node's author) the exact string that works.
///
/// The suggestion leads and the explanation trails, because the toast is one
/// truncated line: an over-long path eats the parenthetical rather than the fix.
/// The rejected URL is not repeated — the suggestion already contains it.
/// The save-or-open effect for a link that is an inline picture already held
/// in memory, or `None` when it is not one (or has not arrived).
///
/// `open` picks between saving and saving-then-opening. A slot that is still
/// queued, loading or failed has no bytes, so it falls through to the ordinary
/// download path and fetches them.
pub(crate) fn saved_image_effect(model: &Model, index: usize, open: bool) -> Option<Effect> {
    let slot = model.image_slot(index)?;
    match slot.state {
        SlotState::Ready { .. } | SlotState::Text { .. } => Some(match open {
            true => Effect::OpenImage { link: index },
            false => Effect::SaveImage { link: index },
        }),
        _ => None,
    }
}

pub(crate) fn bad_url_note(what: StrId, raw: &str, err: UrlError) -> Text {
    let what = Arg::Text(Box::new(Text::key(what)));
    match err {
        UrlError::NoDestination if raw.starts_with('/') => {
            with_args(StrId::TryLocal, vec![what, Arg::Str(raw.to_string())])
        }
        other => with_args(
            StrId::BadUrlWhy,
            vec![what, Arg::Str(raw.to_string()), Arg::Str(other.to_string())],
        ),
    }
}

/// Collect the current values of the form fields a link references, packaged as
/// the NomadNet request map expects: each becomes a `field_<name>` entry.
///
/// Mirrors the reference `Browser.handle_link` (NomadNet `Browser.py`): a link's
/// field components that carry no `=` are field-NAME references (a `*` component
/// means "all fields"); their referenced fields' current widget values are added
/// under a `field_` prefix. A text field is always included; a checkbox/radio is
/// included only when checked (its submit value), and several checked checkboxes
/// sharing a name are comma-joined. The `k=v` preset components are handled by
/// [`resolve_link`] as `var_<k>`, so they are ignored here.
pub(crate) fn collect_submit_fields(model: &Model, link: &RenderedLink) -> Vec<(String, String)> {
    let mut all = false;
    let mut referenced: Vec<&str> = Vec::new();
    for (k, v) in &link.fields {
        if !v.is_empty() {
            continue; // a `k=v` preset -> handled as var_ by resolve_link.
        }
        if k == "*" {
            all = true;
        } else {
            referenced.push(k.as_str());
        }
    }
    if !all && referenced.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<(String, String)> = Vec::new();
    for fe in &model.field_state {
        if !all && !referenced.iter().any(|n| *n == fe.name) {
            continue;
        }
        let key = format!("field_{}", fe.name);
        match fe.kind {
            FieldKind::Text => out.push((key, InputBuffer::value(&fe.input).to_string())),
            FieldKind::Checkbox => {
                if fe.checked {
                    if let Some(existing) = out.iter_mut().find(|(k, _)| *k == key) {
                        existing.1 = format!("{},{}", existing.1, fe.value);
                    } else {
                        out.push((key, fe.value.clone()));
                    }
                }
            }
            FieldKind::Radio => {
                if fe.checked {
                    if let Some(existing) = out.iter_mut().find(|(k, _)| *k == key) {
                        existing.1 = fe.value.clone();
                    } else {
                        out.push((key, fe.value.clone()));
                    }
                }
            }
        }
    }
    out
}

/// Activate a hint target: follow the link, activate the form field, or drive
/// the top-bar control.
pub(crate) fn activate_hint_target(model: &mut Model, target: HintTarget) -> Vec<Effect> {
    match target {
        HintTarget::Link(idx) => follow_link(model, idx),
        HintTarget::Field(fi) => {
            activate_field(model, fi);
            Vec::new()
        }
        HintTarget::Control(Control::Address) => {
            enter_address(model);
            Vec::new()
        }
        HintTarget::Control(Control::Bookmark) => toggle_bookmark_current(model),
    }
}

/// Focus and activate form field `fi` (1-based): a checkbox toggles, a radio
/// selects, a text field only takes focus (entering [`Mode::Field`]). The one
/// body shared by a mouse click and a hint activation, so they never diverge.
pub(crate) fn activate_field(model: &mut Model, fi: usize) {
    apply_focus(model, Focus::Field(fi));
    toggle_field(model, fi);
}

/// The interactive elements (links and form fields) in document order: sorted by
/// laid-out `(line, col_start)`, so the Tab cycle walks them exactly as they read
/// down the page. The single source of truth for focus traversal.
pub(crate) fn interactive_order(model: &Model) -> Vec<Focus> {
    let mut items: Vec<(usize, usize, Focus)> = Vec::new();
    for l in &model.links {
        items.push((l.line, l.col_start, Focus::Link(l.index)));
    }
    for f in &model.field_defs {
        items.push((f.line, f.col_start, Focus::Field(f.index)));
    }
    items.sort_by_key(|(line, col, _)| (*line, *col));
    items.into_iter().map(|(_, _, focus)| focus).collect()
}

/// The currently focused interactive element, as a [`Focus`], or `None`.
pub(crate) fn current_focus(model: &Model) -> Option<Focus> {
    if let Some(i) = model.field_focus {
        Some(Focus::Field(i))
    } else {
        model.focus.map(Focus::Link)
    }
}

/// Move the focus cursor to the next interactive element (link or field) in
/// reading order (wrapping), scrolling it into view. A no-op when the page has no
/// interactive elements.
pub(crate) fn focus_next(model: &mut Model) {
    let order = interactive_order(model);
    if order.is_empty() {
        return;
    }
    let next = match current_focus(model).and_then(|c| order.iter().position(|x| *x == c)) {
        Some(i) => order[(i + 1) % order.len()],
        None => order[0],
    };
    apply_focus(model, next);
}

/// Move the focus cursor to the previous interactive element (wrapping),
/// scrolling it into view. A no-op when the page has no interactive elements.
pub(crate) fn focus_prev(model: &mut Model) {
    let order = interactive_order(model);
    if order.is_empty() {
        return;
    }
    let last = order.len() - 1;
    let prev = match current_focus(model).and_then(|c| order.iter().position(|x| *x == c)) {
        Some(0) | None => order[last],
        Some(i) => order[i - 1],
    };
    apply_focus(model, prev);
}

/// Focus an interactive element: set the link/field focus (they are mutually
/// exclusive), switch mode (a field enters [`Mode::Field`] editing; a link
/// returns to [`Mode::Browse`]), and auto-scroll it into view.
pub(crate) fn apply_focus(model: &mut Model, focus: Focus) {
    match focus {
        Focus::Link(index) => {
            model.focus = Some(index);
            model.field_focus = None;
            model.mode = Mode::Browse;
            if let Some(line) = model
                .links
                .iter()
                .find(|l| l.index == index)
                .map(|l| l.line)
            {
                scroll_line_into_view(model, line);
            }
        }
        Focus::Field(index) => {
            model.field_focus = Some(index);
            model.focus = None;
            model.mode = Mode::Field;
            if let Some(line) = model
                .field_defs
                .iter()
                .find(|f| f.index == index)
                .map(|f| f.line)
            {
                scroll_line_into_view(model, line);
            }
        }
    }
}

/// Scroll the minimal amount so page `line` is inside the viewport.
pub(crate) fn scroll_line_into_view(model: &mut Model, line: usize) {
    let vp = model.viewport();
    if line < model.scroll {
        model.scroll = line;
    } else if vp > 0 && line >= model.scroll + vp {
        model.scroll = line + 1 - vp;
    }
    model.clamp_scroll(vp);
}

/// Hit-test a mouse click at `(col, row)`: follow the link under it, else
/// activate the top-bar control under it, else do nothing.
pub(crate) fn handle_click(model: &mut Model, col: u16, row: u16) -> Vec<Effect> {
    // The help overlay is modal: a left click anywhere while it is open closes
    // it, before any other hit-testing, so the click cannot fall through to a
    // link, control or footer button underneath.
    if model.show_help {
        model.show_help = false;
        return Vec::new();
    }
    // The places panel is modal too: a single left click on an entry selects and
    // activates it (the same language as links and footer buttons); a click
    // elsewhere inside the panel does nothing; a click outside closes it. Every
    // case is swallowed here, so a click can never fall through to a link,
    // control or footer button underneath.
    if model.show_places {
        if let Some(idx) = place_at(model, col, row) {
            // The `×` marker's cell is special-cased before the normal row
            // action: on a row showing the marker it deletes that bookmark; on
            // any row without one (a node row never has one) it is inert, so a
            // click there can never delete or activate. The cell comes from the
            // same layout `place_line` renders, so click and draw agree.
            if place_x_screen_col(model) == Some(col) {
                if place_shows_x(model, idx) {
                    return delete_bookmark_at(model, idx);
                }
                return Vec::new();
            }
            model.places_sel = idx;
            return activate_place(model, idx);
        }
        if !rect_contains(places_overlay_rect(model, model.frame_area()), col, row) {
            close_places(model);
        }
        return Vec::new();
    }
    for vl in visible_links(model) {
        if row == vl.row && col >= vl.col_start && col < vl.col_end {
            return follow_link(model, vl.index);
        }
    }
    // A click on a form field focuses it (entering field-edit mode) and, for a
    // checkbox or radio, also activates it — the same as pressing Space, and
    // the same as the reference browser's mouse behaviour.
    for vf in visible_fields(model) {
        if row == vf.row && col >= vf.col_start && col < vf.col_end {
            activate_field(model, vf.index);
            return Vec::new();
        }
    }
    let [topbar, _content, footer] = regions(model.frame_area());
    for cr in top_bar_controls(model, topbar) {
        if rect_contains(cr.rect, col, row) {
            return activate_hint_target(model, HintTarget::Control(cr.control));
        }
    }
    // A click on a footer button runs its action (same as pressing its key).
    if footer_shows_buttons(model) {
        for slot in footer_layout(model, footer) {
            if slot.item.action != FooterAction::Sentence && rect_contains(slot.rect, col, row) {
                return run_footer_action(model, slot.item.action);
            }
        }
    }
    Vec::new()
}

/// Set the hovered link to whichever visible link is under `(col, row)`, or
/// `None` when the cursor is over no link.
pub(crate) fn update_hover(model: &mut Model, col: u16, row: u16) {
    // The places panel owns the pointer while it is open: a move over an entry
    // highlights that row, a move off any entry clears it. The link and footer
    // hovers stay cleared beneath the modal panel.
    if model.show_places {
        model.places_hover = place_at(model, col, row);
        model.hover = None;
        model.footer_hover = None;
        return;
    }
    // A move over a footer button highlights it; a footer hover and a content
    // link hover are mutually exclusive.
    if footer_shows_buttons(model) {
        let [_topbar, _content, footer] = regions(model.frame_area());
        if let Some(slot) = footer_layout(model, footer)
            .into_iter()
            .find(|s| s.item.action != FooterAction::Sentence && rect_contains(s.rect, col, row))
        {
            model.footer_hover = Some(slot.index);
            model.hover = None;
            return;
        }
    }
    model.footer_hover = None;
    model.hover = visible_links(model)
        .into_iter()
        .find(|vl| row == vl.row && col >= vl.col_start && col < vl.col_end)
        .map(|vl| vl.index);
}

/// Map a key press to a [`ScrollCmd`], honouring both vi and emacs idioms, from
/// the shared scroll rows ([`SCROLL_ROWS`], in `leviculum-ui` so loquax reads the
/// same ones). Returns `None` for keys that are not scroll motions.
#[cfg(test)]
pub(crate) fn key_to_scroll(key: &KeyEvent) -> Option<ScrollCmd> {
    keymap::best(SCROLL_ROWS, key, Panes::SCROLL, Ctx::NONE).map(|(_, cmd)| cmd)
}

/// Run a footer button's action: the intent the key it shows raises.
pub(crate) fn run_footer_action(model: &mut Model, action: FooterAction) -> Vec<Effect> {
    match Intent::of_key(action, model) {
        Some(intent) => apply(model, intent),
        None => Vec::new(),
    }
}
