//! The browser chrome laid out in character cells: the top bar, the footer,
//! the help text. Shared by the frontend that draws it and by the click and
//! hover hit-tests in [`update`], which is why it lives in the core.

use unicode_width::UnicodeWidthStr;

use super::event::*;
use super::geometry::Rect;
use super::input::KeyCode;
use super::model::*;
use super::strings::{Arg, StrId, Text};

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

/// The bookmark-star glyph for the current page: filled when bookmarked, hollow
/// when not, or `None` when no page is loaded (nothing to bookmark).
pub(crate) fn star_glyph(model: &Model) -> Option<&'static str> {
    let url = current_url(model)?;
    Some(if model.bookmarks.contains(&url) {
        STAR_ON
    } else {
        STAR_OFF
    })
}

/// The identity-marker cluster text: the key glyph plus the first 8 hex chars
/// of the session's own fingerprint, or `None` when not identifying to the
/// current node (or the fingerprint is unset, as in a bare test model).
pub(crate) fn identity_text(model: &Model) -> Option<String> {
    if !model.identifying || model.fingerprint.is_empty() {
        return None;
    }
    let fp8: String = model.fingerprint.chars().take(8).collect();
    Some(format!("{IDENTIFY_GLYPH} {fp8}"))
}

/// The hop-count cluster text for the current destination: `N hops` when the
/// destination is a known announced node with a hop count, `unknown` when it is
/// not in the registry, or `None` when no page is loaded.
pub(crate) fn hops_text(model: &Model) -> Option<String> {
    let dest = model.current_dest?;
    Some(
        match model.node_registry.get_by_hash(&dest).and_then(|n| n.hops) {
            Some(h) => Text::Key(StrId::Hops, vec![Arg::Int(h.into())]),
            None => Text::key(StrId::HopsUnknown),
        }
        .resolve(model.lang),
    )
}

pub(crate) fn topbar_layout(model: &Model, area: Rect) -> TopBar {
    let y = area.y;
    let right = area.right();

    // Right-aligned status cluster: star, identity marker, cache bolt, hop
    // count, each present only when it applies, separated by a single column.
    let star = star_glyph(model);
    let identity = identity_text(model);
    let mut cached = model.cached_view;
    let mut hops = hops_text(model);
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
    let title_full = model.title.trim().to_string();
    let addr_full = breadcrumb(model);
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

/// What a footer button activates when pressed or clicked. Mode-specific keys
/// (`Esc`, `Tab`, `Space`, `Enter`, `n`, `N`) route back through the key handler
/// so the click behaves exactly like the key press it mirrors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterAction {
    Back,
    Forward,
    Reload,
    /// Toggle identifying to the current page's node (same as the `i` key).
    Identify,
    Hint,
    Search,
    Places,
    Mark,
    Copy,
    Help,
    Quit,
    /// Synthesize a plain key press (the mode-action buttons).
    Key(KeyCode),
    /// A non-interactive sentence (hint mode's instruction).
    None,
}

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

/// The footer's navigation trio, always first: back, forward, reload. They are
/// ordinary `key + label` items like the rest of the strip, spelling their keys
/// (`Alt-←`, `Alt-→`, `R`) rather than using glyphs. Their availability tracks
/// the history so an impossible motion reads muted.
pub(crate) fn footer_nav(model: &Model) -> [FooterItem; 3] {
    let w = |id: StrId| id.text(model.lang);
    [
        FooterItem {
            available: model.history.can_back(),
            ..FooterItem::action("Alt-←", w(StrId::Back), FooterAction::Back)
        },
        FooterItem {
            available: model.history.can_forward(),
            ..FooterItem::action("Alt-→", w(StrId::Forward), FooterAction::Forward)
        },
        FooterItem {
            available: model.history.current().is_some(),
            ..FooterItem::action("R", w(StrId::Reload), FooterAction::Reload)
        },
    ]
}

/// The footer button list for the current mode: the nav trio, then the
/// mode-dependent action set. The first non-nav item carries the stronger
/// divider before it.
pub(crate) fn footer_items(model: &Model) -> Vec<FooterItem> {
    let mut items: Vec<FooterItem> = footer_nav(model).to_vec();
    let w = |id: StrId| id.text(model.lang);
    match model.mode {
        Mode::Browse => {
            // The identify toggle, only when there is a page to identify to. A
            // state action, so under truncation it outlives the hint/search/
            // places/mark/copy conveniences (highest drop rank, right below the
            // never-dropped reload).
            if model.current_dest.is_some() {
                let label = if model.identifying {
                    w(StrId::Anonymous)
                } else {
                    w(StrId::Identify)
                };
                items.push(FooterItem::droppable("i", label, FooterAction::Identify, 6));
            }
            items.extend_from_slice(&[
                FooterItem::droppable("f", w(StrId::HintMode), FooterAction::Hint, 5),
                FooterItem::droppable("/", w(StrId::Search), FooterAction::Search, 4),
                FooterItem::droppable("d", w(StrId::Places), FooterAction::Places, 3),
                FooterItem::droppable("m", w(StrId::Mark), FooterAction::Mark, 2),
                FooterItem::droppable("y", w(StrId::Copy), FooterAction::Copy, 1),
                FooterItem::action("?", w(StrId::Help), FooterAction::Help),
                FooterItem::action("q", w(StrId::Quit), FooterAction::Quit),
            ]);
        }
        Mode::Field => items.extend_from_slice(&[
            FooterItem::action("Esc", w(StrId::Done), FooterAction::Key(KeyCode::Esc)),
            FooterItem::droppable(
                "Tab",
                w(StrId::NextField),
                FooterAction::Key(KeyCode::Tab),
                1,
            ),
            FooterItem::droppable(
                "Space",
                w(StrId::Toggle),
                FooterAction::Key(KeyCode::Char(' ')),
                2,
            ),
            FooterItem::action("?", w(StrId::Help), FooterAction::Help),
        ]),
        Mode::Search => items.extend_from_slice(&[
            FooterItem::action("Enter", w(StrId::Go), FooterAction::Key(KeyCode::Enter)),
            FooterItem::droppable(
                "n",
                w(StrId::Next),
                FooterAction::Key(KeyCode::Char('n')),
                2,
            ),
            FooterItem::droppable(
                "N",
                w(StrId::Prev),
                FooterAction::Key(KeyCode::Char('N')),
                3,
            ),
            FooterItem::action("Esc", w(StrId::Cancel), FooterAction::Key(KeyCode::Esc)),
            FooterItem::droppable("?", w(StrId::Help), FooterAction::Help, 1),
        ]),
        Mode::Address => items.extend_from_slice(&[
            FooterItem::action("Enter", w(StrId::Go), FooterAction::Key(KeyCode::Enter)),
            FooterItem::action("Esc", w(StrId::Cancel), FooterAction::Key(KeyCode::Esc)),
            FooterItem::droppable("?", w(StrId::Help), FooterAction::Help, 1),
        ]),
        Mode::Hint => items.extend_from_slice(&[
            FooterItem {
                key: "",
                label: w(StrId::HintSentence),
                key_first: true,
                action: FooterAction::None,
                drop_rank: None,
                available: true,
            },
            FooterItem::action("Esc", w(StrId::Cancel), FooterAction::Key(KeyCode::Esc)),
        ]),
    }
    items
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

pub(crate) use leviculum_ui::help::{HelpEntry, HelpGroup};

/// The keybinding groups shown in the help overlay. The Move group's scroll rows
/// are pulled straight from [`SCROLL_ROWS`], so the overlay can never list a
/// scroll binding that the key handler does not honour, nor omit one it does.
pub(crate) fn help_groups() -> Vec<HelpGroup> {
    // The scroll rows come straight from the keymap table; the mouse wheel is the
    // one Move affordance that is not a key chord, so it is appended here.
    let mut move_entries: Vec<HelpEntry> = SCROLL_ROWS
        .iter()
        .map(|s| HelpEntry {
            keys: s.keys(),
            desc: s.desc,
        })
        .collect();
    move_entries.push(HelpEntry {
        keys: "wheel",
        desc: "scroll",
    });
    vec![
        HelpGroup {
            title: "Move",
            entries: move_entries,
        },
        HelpGroup {
            title: "Follow",
            entries: vec![
                HelpEntry {
                    keys: "Tab / Shift-Tab",
                    desc: "focus a link or field",
                },
                HelpEntry {
                    keys: "Enter / click",
                    desc: "follow a link or footer button",
                },
                HelpEntry {
                    keys: "f",
                    desc: "hint — type a label or the link text",
                },
                HelpEntry {
                    keys: "Alt-← / Alt-→   mouse side buttons",
                    desc: "back / forward",
                },
                HelpEntry {
                    keys: "hover a link",
                    desc: "show its URL",
                },
            ],
        },
        HelpGroup {
            title: "Page",
            entries: vec![
                HelpEntry {
                    keys: ": / click the URL",
                    desc: "enter a URL",
                },
                HelpEntry {
                    keys: "R / Ctrl-R / F5",
                    desc: "reload (always refetches)",
                },
                HelpEntry {
                    keys: "Esc / Ctrl-g",
                    desc: "cancel a load",
                },
                HelpEntry {
                    keys: "m / click star",
                    desc: "bookmark this page",
                },
                HelpEntry {
                    keys: "y",
                    desc: "copy the link's / page's URL",
                },
                HelpEntry {
                    keys: "d / click a place",
                    desc: "places panel — click an entry to open it",
                },
                HelpEntry {
                    keys: "i",
                    desc: "identify / go anonymous to this node",
                },
                HelpEntry {
                    keys: "",
                    desc: "default anonymous; identifying lets this node attribute your posts",
                },
                HelpEntry {
                    keys: "/ n N",
                    desc: "search / next / prev",
                },
            ],
        },
        HelpGroup {
            title: "Images",
            entries: vec![
                HelpEntry {
                    keys: "Enter",
                    desc: "save the focused picture (no second transfer)",
                },
                HelpEntry {
                    keys: "o",
                    desc: "open it in the system viewer",
                },
                HelpEntry {
                    keys: "",
                    desc: "start with --images off to leave them as plain links",
                },
            ],
        },
        HelpGroup {
            title: "Fields",
            entries: vec![HelpEntry {
                keys: "type / Space / Esc",
                desc: "edit / toggle / leave",
            }],
        },
        HelpGroup {
            title: "App",
            entries: vec![
                HelpEntry {
                    keys: "t",
                    desc: "light / dark theme",
                },
                HelpEntry {
                    keys: "? / Esc / click",
                    desc: "close this help",
                },
                HelpEntry {
                    keys: "q / Ctrl-c",
                    desc: "quit",
                },
            ],
        },
    ]
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
    leviculum_ui::help::scroll_bounds(&help_groups(), leviculum_ui::Rect::new(0, 0, cols, rows))
}
