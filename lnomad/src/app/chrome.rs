//! The browser chrome laid out in character cells: the top bar, the footer,
//! the help text. Shared by the frontend that draws it and by the click and
//! hover hit-tests in [`update`](super::event::update), which is why it lives in the core.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use unicode_width::UnicodeWidthStr;

use super::geometry::Rect;
use super::intent::Intent;
use super::model::*;
use super::scene::{
    address as scene_address, state as scene_state, top_bar as scene_top_bar, Chip, ChipId, NodeId,
    TopBar as SceneTopBar,
};
use super::strings::{Arg, Lang, StrId, Text};

/// The ` · ` separator drawn between the title and the address on the top bar.
pub(crate) const TOPBAR_SEP: &str = " · ";

/// The resolved geometry of the one-row top bar: the title and address (each
/// already ellipsized to fit) and the right-aligned status cluster (bookmark
/// star, cache bolt, hop count). The renderer and the click / hint hit-testing
/// both derive from this one function, so they can never disagree on where a
/// span sits.
pub struct TopBar {
    /// The title text (label only), ellipsized to fit; empty when none.
    pub title: String,
    /// The column the title starts at.
    pub title_x: u16,
    /// The `·` separator's column, present only when both title and address show.
    pub sep_x: Option<u16>,
    /// The address text, ellipsized to fit; empty when it did not fit at all.
    pub address: String,
    /// The address slot's rect; a click here opens the address editor.
    pub address_rect: Rect,
    /// The bookmark star: its glyph (★ / ☆) and rect, absent when no page is
    /// loaded. A click toggles the bookmark.
    pub star: Option<(&'static str, Rect)>,
    /// The identity marker (key glyph + fingerprint prefix) and its column,
    /// present only while identifying to the current node. Anonymous — the
    /// default — shows nothing: the visible state is the one with consequences.
    pub identity: Option<(String, u16)>,
    /// The cache-bolt glyph's column, present only for a cached view.
    pub cached_x: Option<u16>,
    /// The hop-count text (`N hops` / `unknown`) and its column.
    pub hops: Option<(String, u16)>,
}

/// The bookmark-star glyph of the scene's top bar: filled when the page is
/// bookmarked (its star's action takes the bookmark off), hollow when not, or
/// `None` when the bar has no star (no page loaded, nothing to bookmark).
fn star_of(bar: &SceneTopBar) -> Option<&'static str> {
    bar.actions.iter().find_map(|action| match action.intent {
        Intent::Bookmark { on: true } => Some(STAR_OFF),
        Intent::Bookmark { on: false } => Some(STAR_ON),
        _ => None,
    })
}

/// The scene chip `id`, if the top bar's state has it.
fn chip_of(chips: &[Chip], id: ChipId) -> Option<&Chip> {
    chips.iter().find(|chip| chip.id == NodeId::Chip(id))
}

/// The identity chip as the one-row bar draws it: the key glyph in place of
/// the chip's words, then the fingerprint prefix the chip names.
fn identity_of(chips: &[Chip]) -> Option<String> {
    match &chip_of(chips, ChipId::Identity)?.text {
        Text::Key(_, args) => match args.first() {
            Some(Arg::Str(fp8)) => Some(format!("{IDENTIFY_GLYPH} {fp8}")),
            _ => None,
        },
        _ => None,
    }
}

/// The bookmark-star glyph for the current page, from the scene's top bar.
#[cfg(test)]
pub(crate) fn star_glyph(model: &Model) -> Option<&'static str> {
    star_of(&scene_top_bar(model))
}

/// The hop-count cluster text for the current destination, the scene's hops
/// chip in the model's language: `N hops` when the destination is a known
/// announced node with a hop count, `unknown` when it is not in the registry,
/// or `None` when no page is loaded.
#[cfg(test)]
pub(crate) fn hops_text(model: &Model) -> Option<String> {
    let chips = scene_state(model);
    Some(chip_of(&chips, ChipId::Hops)?.text.resolve(model.lang))
}

/// The top bar laid out in cells over the scene's words: its title, its
/// address field and its state chips are what the cluster and the left side
/// show, so the terminal draws the scene and the hit tests read the same
/// geometry.
pub(crate) fn topbar_layout(model: &Model, area: Rect) -> TopBar {
    let y = area.y;
    let right = area.right();

    let bar = scene_top_bar(model);
    let field = scene_address(model);
    let chips = scene_state(model);

    // Right-aligned status cluster: star, identity marker, cache bolt, hop
    // count, each present only when it applies, separated by a single column.
    let star = star_of(&bar);
    let identity = identity_of(&chips);
    let mut cached = chip_of(&chips, ChipId::Cached).is_some();
    let mut hops = chip_of(&chips, ChipId::Hops).map(|chip| chip.text.resolve(model.lang));
    let seg_w = |g: &str| UnicodeWidthStr::width(g) as u16;
    let cluster_width = |cached: bool, hops: &Option<String>| -> u16 {
        let mut w = 0u16;
        let mut segs = 0u16;
        if let Some(g) = star {
            w += seg_w(g);
            segs += 1;
        }
        if let Some(t) = &identity {
            w += seg_w(t);
            segs += 1;
        }
        if cached {
            w += seg_w(CACHED_GLYPH);
            segs += 1;
        }
        if let Some(h) = hops {
            w += seg_w(h);
            segs += 1;
        }
        w + segs.saturating_sub(1) // one blank column between segments
    };
    // When even the bare cluster cannot fit the bar, drop the least important
    // segments: the hop count first, then the cache bolt. The star and the
    // identity marker — the security-relevant state — always survive.
    if cluster_width(cached, &hops) > area.width && hops.is_some() {
        hops = None;
    }
    if cluster_width(cached, &hops) > area.width {
        cached = false;
    }
    let cluster_w = cluster_width(cached, &hops);
    let cluster_x = right.saturating_sub(cluster_w).max(area.x);

    // Place each cluster segment left to right from cluster_x.
    let mut cx = cluster_x;
    let star = star.map(|g| {
        let w = seg_w(g);
        let rect = Rect {
            x: cx,
            y,
            width: w,
            height: 1,
        };
        cx = cx.saturating_add(w).saturating_add(1);
        (g, rect)
    });
    let identity = identity.map(|t| {
        let x = cx;
        cx = cx.saturating_add(seg_w(&t)).saturating_add(1);
        (t, x)
    });
    let cached_x = if cached {
        let x = cx;
        cx = cx.saturating_add(seg_w(CACHED_GLYPH)).saturating_add(1);
        Some(x)
    } else {
        None
    };
    let hops = hops.map(|h| {
        let x = cx;
        (h, x)
    });

    // Left identity: title, separator, address. The cluster keeps its width; the
    // title comes next; the address ellipsizes; and, only when even the title
    // plus separator will not fit, the title ellipsizes too.
    let left_end = if cluster_w > 0 {
        cluster_x.saturating_sub(CTRL_GAP)
    } else {
        right
    };
    let left_w = left_end.saturating_sub(area.x);
    let title_full = bar.title.resolve(model.lang);
    // An empty field shows its hint, as a platform's text field does.
    let addr_full = match field.value.is_empty() {
        true => field.hint.resolve(model.lang),
        false => field.value,
    };
    // A host's name for the page is the title and the address alike
    // (1621): said once, in the address slot.
    let title_full = match !field.editing && title_full == addr_full {
        true => String::new(),
        false => title_full,
    };
    let sep_w = UnicodeWidthStr::width(TOPBAR_SEP) as u16;
    let tw = UnicodeWidthStr::width(title_full.as_str()) as u16;

    // Decide the title and how much width the address slot gets. The cluster
    // already has its width; the title comes next; the address slot takes the
    // rest and ellipsizes; and only when even the title plus separator plus one
    // address column will not fit does the title ellipsize and the address drop.
    let (title, has_sep, addr_avail) = if title_full.is_empty() {
        (String::new(), false, left_w)
    } else if tw + sep_w + 1 > left_w {
        (truncate_to_cols(&title_full, left_w as usize), false, 0)
    } else {
        (title_full, true, left_w - tw - sep_w)
    };

    let title_x = area.x;
    let title_w = UnicodeWidthStr::width(title.as_str()) as u16;
    let (sep_x, addr_x) = if has_sep {
        (Some(area.x + title_w), area.x + title_w + sep_w)
    } else if title.is_empty() {
        (None, area.x)
    } else {
        (None, area.x + title_w)
    };
    // The address text ellipsizes to the slot; the slot itself is the full
    // remaining width so a click anywhere in it, and the live editor, both land.
    let address = truncate_to_cols(&addr_full, addr_avail as usize);
    let address_rect = Rect {
        x: addr_x,
        y,
        width: addr_avail,
        height: 1,
    };

    TopBar {
        title,
        title_x,
        sep_x,
        address,
        address_rect,
        star,
        identity,
        cached_x,
        hops,
    }
}

/// The clickable top-bar controls (the address slot and, when a page is loaded,
/// the bookmark star), derived from [`topbar_layout`] so clicks and hint labels
/// hit-test against exactly what the renderer draws.
pub(crate) fn top_bar_controls(model: &Model, area: Rect) -> Vec<ControlRect> {
    let tb = topbar_layout(model, area);
    let mut out = vec![ControlRect {
        control: Control::Address,
        rect: tb.address_rect,
    }];
    if let Some((_, rect)) = tb.star {
        out.push(ControlRect {
            control: Control::Bookmark,
            rect,
        });
    }
    out
}

/// The address-slot breadcrumb for the current page, or a hint when nothing is
/// loaded yet.
pub(crate) fn breadcrumb(model: &Model) -> String {
    match model.history.current() {
        Some(target) => format!("{}:{}", short_hex(&target.dest_hash), target.path),
        None => StrId::AddressHint.text(model.lang).to_string(),
    }
}

/// A short, glanceable form of a destination hash: the first 8 hex characters
/// followed by an ellipsis.
pub(crate) fn short_hex(dest: &[u8; 16]) -> String {
    let mut s = String::with_capacity(9);
    for byte in &dest[..4] {
        s.push_str(&format!("{byte:02x}"));
    }
    s.push('…');
    s
}

/// What a footer button runs when pressed or clicked: the key table's
/// action for the chord it shows ([`super::keys`]), so a click and the key
/// cannot do two different things. [`Action::Sentence`](super::keys::Action::Sentence)
/// is a sentence, not a button.
pub type FooterAction = super::keys::Action;

/// One footer button of lnomad's strip: [`leviculum_ui::footer::FooterItem`]
/// over lnomad's [`FooterAction`].
pub type FooterItem = leviculum_ui::footer::FooterItem<FooterAction>;

/// A laid-out button of lnomad's strip.
pub type FooterSlot = leviculum_ui::footer::FooterSlot<FooterAction>;

impl leviculum_ui::footer::FooterRole for FooterAction {
    /// The navigation trio, rendered before the stronger divider.
    fn is_nav(&self) -> bool {
        matches!(
            self,
            FooterAction::Back | FooterAction::Forward | FooterAction::Reload
        )
    }
}

/// The footer button list for the current mode, derived from the key table
/// ([`super::keys::footer_items`]): the nav trio, then the mode's buttons.
pub(crate) fn footer_items(model: &Model) -> Vec<FooterItem> {
    super::keys::footer_items(model)
}

/// Lay lnomad's footer buttons out across `area`
/// ([`leviculum_ui::footer::layout`] over [`footer_items`]).
pub(crate) fn footer_layout(model: &Model, area: Rect) -> Vec<FooterSlot> {
    leviculum_ui::footer::layout(footer_items(model), area)
}

/// Whether the footer currently shows its button strip. It now always does,
/// except in search mode where the status bar becomes the `/<query>` editor: the
/// loading spinner and a focused/hovered link's target float in the bottom-left
/// [`render_status_chip`] over the content instead of covering the buttons.
pub(crate) fn footer_shows_buttons(model: &Model) -> bool {
    model.mode != Mode::Search
}

/// The status-bar text for the focused-or-hovered link's target, if any: the
/// current node's short hash and the link's path (`node · :/page/x.mu`).
pub fn focused_link_target(model: &Model) -> Option<String> {
    let idx = model.focus.or(model.hover)?;
    let link = model.links.iter().find(|l| l.index == idx)?;
    // A same-destination link's target begins with ':'; prefix the current node
    // so the reader sees which node it resolves against.
    match model.current_dest {
        Some(dest) if link.target.starts_with(':') => {
            Some(format!("{} · {}", short_hex(&dest), link.target))
        }
        _ => Some(link.target.clone()),
    }
}

pub(crate) use leviculum_ui::help::HelpGroup;

/// The catalogue's words for a shared scroll row's description: the rows live
/// in `leviculum-ui` with English words, the browser says them in its own
/// language. A row this does not know keeps its English words
/// (`every_scroll_row_has_its_words` holds that none is left so).
pub(crate) fn scroll_desc(desc: &'static str, lang: Lang) -> &'static str {
    let id = match desc {
        "scroll a line" => StrId::HelpScrollLine,
        "page down / up" => StrId::HelpScrollPage,
        "half a page" => StrId::HelpScrollHalf,
        "top / bottom" => StrId::HelpScrollEnds,
        _ => return desc,
    };
    id.text(lang)
}

/// The keybinding groups shown in the help overlay, in `lang`, derived from
/// the key table ([`super::keys::help_groups_in`]): the overlay can never
/// list a binding the key handler does not honour, nor omit one it does.
pub(crate) fn help_groups_in(lang: Lang) -> Vec<HelpGroup> {
    super::keys::help_groups_in(lang)
}

/// The help groups in English, as the tests of `tui.rs` read them.
#[cfg(test)]
pub(crate) fn help_groups() -> Vec<HelpGroup> {
    help_groups_in(Lang::En)
}

/// The plain-text lines of lnomad's help overlay ([`leviculum_ui::help::lines`]
/// over [`help_groups`]), for the tests: the renderer and the scroll bounds
/// draw and count their wrapped rows ([`leviculum_ui::help::rows`]).
#[cfg(test)]
pub(crate) fn help_lines() -> Vec<String> {
    leviculum_ui::help::lines(&help_groups())
}

/// The help overlay's scroll geometry for the current terminal size: the number
/// of help lines that fit at once (the viewport) and the largest valid
/// [`Model::help_scroll`] (`max`). Shared by the key/wheel handler (to clamp the
/// offset via [`scrolled`]) and the renderer (to slice the visible lines), so
/// the two can never disagree on the bounds: both are
/// [`leviculum_ui::help::scroll_bounds`] over the whole terminal.
pub(crate) fn help_scroll_geometry(model: &Model) -> (usize, usize) {
    let (cols, rows) = model.size;
    leviculum_ui::help::scroll_bounds(
        &help_groups_in(model.lang),
        leviculum_ui::Rect::new(0, 0, cols, rows),
    )
}
