//! The ratatui/crossterm terminal frontend over the browser core in
//! [`crate::app`]: the renderer, the translation of terminal events into
//! [`AppEvent`]s, and the thin IO shell.
//!
//! The design is TEA (The Elm Architecture) so the interesting logic is pure
//! and driven from headless tests. The first two parts live in the core, which
//! imports no terminal crate (Codeberg #219); the last two are this module:
//!
//! - [`Model`] is the whole UI state (the laid-out page, its links, the title,
//!   the browsing history, the address editor, the terminal size, a quit flag).
//! - [`update()`] folds an [`AppEvent`] into the model and returns a list of
//!   [`Effect`]s for the IO shell to run. It does no IO, so a test feeds it
//!   events and inspects the model and the returned effects directly. Navigation
//!   is expressed purely: an address entry or a history motion yields
//!   [`Effect::Navigate`]; a cancel yields [`Effect::Cancel`]. A completed fetch
//!   arrives back as [`AppEvent::PageLoaded`] / [`AppEvent::LoadFailed`], also
//!   folded by `update`.
//! - [`view`] draws the model into a ratatui [`Frame`]: a one-row top-bar (page
//!   title, address, and a right-aligned status cluster), the scrollable
//!   content, and a one-row status bar. A test renders it into a
//!   [`ratatui::backend::TestBackend`] and asserts on the resulting buffer.
//! - [`run_tui`] is the only part that touches a real terminal and the network.
//!   It turns crossterm's key and mouse events into the core's own
//!   ([`crate::app::input`]) on the way in, and nothing else crosses the line.
//!   It owns the [`Session`], runs the initial fetch and every subsequent
//!   navigation as a background task whose result rejoins the `tokio::select!`
//!   loop, so the UI stays live (spinner animates, keys and the address input
//!   work) while a page loads. A slow or failed fetch never blocks the UI; a
//!   cancel drops the in-flight task and returns to the current page.
//!
//! Phase 3 adds the top-bar, the history stack, async navigation with a loading
//! spinner and `Esc`/`Ctrl-g` cancel, and a `tui-input` address editor.
//!
//! Phase 4 adds the navigation layer over the rendered page: [`visible_links`]
//! maps each on-screen link to a screen rectangle, reused by Tab focus (with
//! auto-scroll), `f` hint mode (vimium-style labels over links and controls),
//! left-click hit-testing, and mouse-hover. The focused or hovered link's target
//! shows in the status bar. Links no longer carry a visible `[N]` marker.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as RtLine, Span as RtSpan, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::{Frame, Terminal};
use tokio::sync::{mpsc, Mutex};
use unicode_width::UnicodeWidthStr;

use leviculum_ui_ratatui::background::detect_background;
use leviculum_ui_ratatui::event::{key_event, mouse_event, step_event, EventStep};
use leviculum_ui_ratatui::geometry::{from_ratatui, to_ratatui};
use leviculum_ui_ratatui::images::{self as pictures, Pictures, SlicedProtocol};
use leviculum_ui_ratatui::spinner::spinner_span;
use leviculum_ui_ratatui::style::{
    chrome_muted_style, chrome_style, chrome_text_style, downgrade_buffer, paint, rgb,
    rstyle_to_ratatui,
};
use leviculum_ui_ratatui::terminal::install_panic_hook;
use leviculum_ui_ratatui::{chip, footer, help, toast};
pub use leviculum_ui_ratatui::{to_ratatui_text, CrosstermTerminal, TerminalGuard, TerminalOps};

use crate::bookmarks::Bookmarks;
use crate::browser::BrowserOptions;
use crate::color::ColorDepth;
use crate::discovery::DiscoveredNode;
use crate::fetch::Session;
use crate::image::{Backend, Probe, SlotState};
use crate::image_cache::ImageCache;
use crate::theme::{resolve_theme, ThemeFlag};
use crate::transfer::{
    close_session, open_saved, save_picture, say_saved, spawn_discovery, spawn_download,
    spawn_fetch, spawn_image_queue, FetchOutcome, ImageOutcome, ImageResult, LoadResult,
};
use crate::url::Target;
use leviculum_micron::FieldKind;

use crate::app::scene::{AddressField, Chip, Page, Row as SceneRow, SceneToast, Sheet};
use crate::app::strings::{with, StrId, Text as StrText};
use crate::app::*;

/// [`crate::app::geometry::regions`] over ratatui rectangles, for the drawing
/// code.
fn regions(area: Rect) -> [Rect; 3] {
    crate::app::geometry::regions(from_ratatui(area)).map(to_ratatui)
}

/// Draw the whole UI: the top-bar, the scrollable content, and the status bar,
/// plus the help overlay when active.
pub fn view(model: &Model, images: &ImageStore, frame: &mut Frame) {
    let scene = model.scene();
    let [topbar, content, status] = regions(frame.area());
    render_topbar(model, &scene.address, frame, topbar);
    render_content(&scene.page, frame, content);
    // Pictures go over the reserved rows, before every other overlay: a toast,
    // the help or the places panel must be able to cover one.
    render_images(model, images, frame, content);
    // The bottom-left status chip floats over the content, above the footer row,
    // so the footer's clickable buttons below it are never covered.
    render_status_chip(model, scene.status.as_ref(), frame, frame.area());
    render_status(model, frame, status);
    // Overlays drawn on top of the laid-out page: the form-field input boxes, the
    // search-match highlights, the focus highlight, the mouse-hover highlight, and
    // the hint badges while hint mode is active.
    render_fields(model, scene.page.focus, frame);
    render_search_matches(model, &scene.page, frame, content);
    render_focus(model, scene.page.focus, frame);
    render_hover(model, frame);
    if model.mode == Mode::Hint {
        render_hints(model, frame);
    }
    if let Some(sheet) = &scene.places {
        render_places(model, sheet, frame, frame.area());
    }
    if let Some(sheet) = &scene.names {
        render_names(model, sheet, frame, frame.area());
    }
    if model.show_help {
        render_help(model, frame, frame.area());
    }
    // The toast floats on top of everything so it is always visible.
    render_toast(model, scene.toast.as_ref(), frame, frame.area());
    // Colour-depth post-pass: on a terminal without true colour, downgrade every
    // 24-bit RGB colour in the finished buffer to its nearest xterm-256 index in
    // one sweep. A single chokepoint over the rendered buffer keeps every colour
    // site (content, chrome, spinner, toast, highlights) media-agnostic.
    downgrade_buffer(frame, model.depth, model.no_color);
}

/// Draw the form-field input boxes over the laid-out page: every field's cells
/// get an input style (a distinct chrome-slate box, or an underline box under
/// `no_color`); the scene's focused field is reversed on top so it stands out.
/// A focused TEXT field also places the hardware cursor at its edit position.
/// The boxes are the hit-test geometry ([`visible_fields`]), so a click and the
/// box agree.
fn render_fields(model: &Model, focus: Option<Focus>, frame: &mut Frame) {
    let visible = visible_fields(model);
    if visible.is_empty() {
        return;
    }
    let area = frame.area();
    let base = if model.no_color {
        Style::default().add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default()
            .fg(rgb(model.theme.chrome_fg()))
            .bg(rgb(model.theme.chrome_bg()))
    };
    let focused = base.add_modifier(Modifier::REVERSED);
    // The hardware cursor position for a focused text field, resolved after the
    // buffer paint so it lands on the correct cell.
    let mut cursor: Option<(u16, u16)> = None;
    {
        let buf = frame.buffer_mut();
        for vf in &visible {
            let is_focused = focus == Some(Focus::Field(vf.index));
            let style = if is_focused { focused } else { base };
            for x in vf.col_start..vf.col_end {
                if x >= area.width || vf.row >= area.height {
                    continue;
                }
                if let Some(c) = buf.cell_mut((x, vf.row)) {
                    c.set_style(style);
                }
            }
            if is_focused && vf.kind == FieldKind::Text {
                if let Some(fe) = model.field_state.get(vf.index - 1) {
                    // The box carries no bracket chrome, and its text may be
                    // scrolled: the caret sits at the editor offset minus the
                    // window's scroll, clamped to the box's last cell. Same
                    // window the relayout used, so caret and text agree.
                    let w = (vf.col_end - vf.col_start) as usize;
                    let scroll = fe.input.visual_scroll(w.saturating_sub(1));
                    let cx = vf.col_start + fe.input.visual_cursor().saturating_sub(scroll) as u16;
                    let clamped = cx.min(vf.col_end.saturating_sub(1));
                    cursor = Some((clamped, vf.row));
                }
            }
        }
    }
    if let Some(pos) = cursor {
        frame.set_cursor_position(pos);
    }
}

/// Highlight the scene's focused link (reverse video) so Tab navigation is
/// visible, over the link's hit-test span ([`visible_links`]): the cells a
/// click on it lands on. A no-op when no link is focused or the focus is
/// off-screen.
fn render_focus(model: &Model, focus: Option<Focus>, frame: &mut Frame) {
    let Some(Focus::Link(idx)) = focus else {
        return;
    };
    let area = frame.area();
    let buf = frame.buffer_mut();
    let highlight = paint(model.tokens().selection);
    for vl in visible_links(model) {
        if vl.index != idx {
            continue;
        }
        for x in vl.col_start..vl.col_end {
            if x >= area.width || vl.row >= area.height {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, vl.row)) {
                cell.set_style(highlight);
            }
        }
    }
}

/// Highlight the link the mouse is hovering over, so the pointer's target is
/// visible in the content as well as the status bar. Kept distinct from the Tab
/// focus (reverse video): hover patches bold + underline onto the link's own
/// colour, so the two can coexist on different links. A no-op when nothing is
/// hovered or the hovered link is off-screen.
fn render_hover(model: &Model, frame: &mut Frame) {
    let Some(idx) = model.hover else {
        return;
    };
    let area = frame.area();
    let buf = frame.buffer_mut();
    let hover = Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    for vl in visible_links(model) {
        if vl.index != idx {
            continue;
        }
        for x in vl.col_start..vl.col_end {
            if x >= area.width || vl.row >= area.height {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, vl.row)) {
                // Patch (not replace) so the link colour and any focus reverse
                // underneath are preserved and the hover modifiers add on top.
                let style = cell.style().patch(hover);
                cell.set_style(style);
            }
        }
    }
}

/// Highlight the on-screen search matches from the scene's runs: a matched run
/// wears a theme-aware background tint, the current match a stronger reversed
/// highlight. The page window is what the screen shows, so nothing off it is
/// painted.
fn render_search_matches(model: &Model, page: &Page, frame: &mut Frame, content: Rect) {
    let area = frame.area();
    let match_style = Style::default()
        .fg(rgb(model.theme.search_match_fg()))
        .bg(rgb(model.theme.search_match_bg()));
    let current_style = Style::default().add_modifier(Modifier::REVERSED);
    let buf = frame.buffer_mut();
    for (row, line) in page.lines.iter().enumerate() {
        let y = content.y.saturating_add(row as u16);
        let mut x = content.x;
        for run in &line.runs {
            let width = UnicodeWidthStr::width(run.text.as_str()) as u16;
            let style = match run.matched {
                Some(true) => Some(current_style),
                Some(false) => Some(match_style),
                None => None,
            };
            if let Some(style) = style {
                for cx in x..x.saturating_add(width) {
                    if cx >= area.width || y >= area.height {
                        continue;
                    }
                    if let Some(cell) = buf.cell_mut((cx, y)) {
                        cell.set_style(style);
                    }
                }
            }
            x = x.saturating_add(width);
        }
    }
}

/// Draw the hint badges: each matching hint's label over the first cells of its
/// link or control, on a distinct background. Only hints still matching the
/// typed prefix are shown, so typing narrows the field.
fn render_hints(model: &Model, frame: &mut Frame) {
    let area = frame.area();
    let input = model.hint_input.clone();
    let badge = Style::default()
        .fg(rgb(model.theme.hint_badge_fg()))
        .bg(rgb(model.theme.hint_badge_bg()))
        .add_modifier(Modifier::BOLD);
    let buf = frame.buffer_mut();
    for hint in hints(model) {
        if !hint.label.starts_with(&input) {
            continue;
        }
        for (i, ch) in hint.label.chars().enumerate() {
            let x = hint.col + i as u16;
            if x >= area.width || hint.row >= area.height {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x, hint.row)) {
                cell.set_char(ch);
                cell.set_style(badge);
            }
        }
    }
}

/// Draw the scene's window of the page, run by run, and the scrollbar into
/// the content region.
fn render_content(page: &Page, frame: &mut Frame, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let viewport = area.height as usize;
    let body = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(SCROLLBAR_COLS),
        height: area.height,
    };
    let lines: Vec<RtLine<'static>> = page
        .lines
        .iter()
        .take(viewport)
        .map(|line| {
            RtLine::from(
                line.runs
                    .iter()
                    .map(|run| RtSpan::styled(run.text.clone(), rstyle_to_ratatui(run.style)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), body);

    // Map the scrollbar over the SCROLL POSITIONS, not the line count: content
    // length is `max_scroll` so `position` spans the full `[0, max_scroll]` range
    // across the track. This makes the thumb top hit the track top at `scroll==0`
    // and the thumb bottom hit the track bottom at `scroll==max_scroll`. Setting
    // `content_length` to `page.len()` with `viewport_content_length` instead left
    // the thumb short of the bottom for pages only a little taller than the
    // viewport (the position denominator over-counted by the viewport height).
    let max_scroll = page.total.saturating_sub(viewport);
    let mut state = ScrollbarState::new(max_scroll.max(1)).position(page.first);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None);
    frame.render_stateful_widget(scrollbar, area, &mut state);
}

/// Draw the current page's inline images into the rows the layout reserved
/// for them, and a status line for every slot that is not a picture.
///
/// A picture is drawn with [`pictures::render`], which crops it to the area: an
/// image half-scrolled off the top is drawn from its middle rather than
/// jumping or spilling over the top bar. The position is signed for exactly
/// that reason.
fn render_images(model: &Model, images: &ImageStore, frame: &mut Frame, area: Rect) {
    if model.images.is_empty() || area.height == 0 || area.width == 0 {
        return;
    }
    let body = Rect {
        width: area.width.saturating_sub(SCROLLBAR_COLS),
        ..area
    };
    for slot in &model.images {
        let Some(link) = model.links.iter().find(|l| l.index == slot.link) else {
            continue;
        };
        // The reserved rows sit directly under the link's line.
        let first = link.line + 1;
        let y = first as i64 - model.scroll as i64;
        let x = link.col_start.min(u16::MAX as usize) as i16;

        match (&slot.state, images.protocol(slot.link)) {
            (SlotState::Ready { cells, .. }, Some(protocol)) => {
                // Entirely above or below the viewport: nothing to draw.
                if y + cells.1 as i64 <= 0 || y >= body.height as i64 {
                    continue;
                }
                let y = y.clamp(i16::MIN as i64, i16::MAX as i64) as i16;
                pictures::render(frame, body, protocol, x, y);
            }
            // Everything else is one line of text: queued, loading, failed, or
            // a terminal that cannot draw pictures at all.
            (state, _) => {
                if y < 0 || y >= body.height as i64 {
                    continue;
                }
                let row = body.y + y as u16;
                let text = crate::image::status_text(&slot.name, state).resolve(model.lang);
                let style = match model.no_color {
                    true => Style::default().add_modifier(Modifier::DIM),
                    false => paint(model.tokens().muted),
                };
                let rect = Rect {
                    x: body.x + x as u16,
                    y: row,
                    width: body.width.saturating_sub(x as u16),
                    height: 1,
                };
                frame.render_widget(Paragraph::new(RtSpan::styled(text, style)), rect);
            }
        }
    }
}

/// Draw the one-row top bar: the page title, a `·`, and the address on the left,
/// then the right-aligned status cluster (bookmark star, cache bolt, hop count).
/// The words are the scene's top bar, address field and state chips, laid out
/// by [`topbar_layout`]. While the field is edited the address slot becomes the
/// live editor.
fn render_topbar(model: &Model, field: &AddressField, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    // Fill the bar full width with the chrome style first; the title, address and
    // cluster render on top and inherit its background.
    frame.render_widget(
        Block::default().style(chrome_style(model.no_color, model.theme)),
        area,
    );
    let tb = topbar_layout(model, from_ratatui(area));

    // Title in bold bright chrome fg.
    if !tb.title.is_empty() {
        let style = chrome_text_style(model.no_color, model.theme).add_modifier(Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(RtSpan::styled(tb.title.clone(), style)),
            Rect {
                x: tb.title_x,
                y: area.y,
                width: UnicodeWidthStr::width(tb.title.as_str()) as u16,
                height: 1,
            },
        );
    }
    // The `·` separator in the muted chrome fg.
    if let Some(sx) = tb.sep_x {
        let style = chrome_muted_style(model.no_color, model.theme);
        frame.render_widget(
            Paragraph::new(RtSpan::styled(TOPBAR_SEP, style)),
            Rect {
                x: sx,
                y: area.y,
                width: UnicodeWidthStr::width(TOPBAR_SEP) as u16,
                height: 1,
            },
        );
    }

    // The address slot: the live editor in address mode, else the address text
    // with an input-field look (underlined) so it reads as editable.
    if field.editing {
        render_address_editor(model, field, frame, to_ratatui(tb.address_rect));
    } else if !tb.address.is_empty() {
        let style =
            chrome_text_style(model.no_color, model.theme).add_modifier(Modifier::UNDERLINED);
        frame.render_widget(
            Paragraph::new(RtSpan::styled(tb.address.clone(), style)),
            to_ratatui(tb.address_rect),
        );
    }

    // Right-aligned status cluster. The star reads in the bright chrome fg (it is
    // a click target); the bolt and hop count in the muted chrome fg.
    let bright = chrome_text_style(model.no_color, model.theme);
    let muted = chrome_muted_style(model.no_color, model.theme);
    if let Some((glyph, rect)) = tb.star {
        frame.render_widget(
            Paragraph::new(RtSpan::styled(glyph, bright)),
            to_ratatui(rect),
        );
    }
    // The identity marker reads in the bright chrome fg: it is security-relevant
    // state, at least as prominent as the star, never muted.
    if let Some((text, x)) = &tb.identity {
        frame.render_widget(
            Paragraph::new(RtSpan::styled(text.clone(), bright)),
            Rect {
                x: *x,
                y: area.y,
                width: UnicodeWidthStr::width(text.as_str()) as u16,
                height: 1,
            },
        );
    }
    if let Some(x) = tb.cached_x {
        frame.render_widget(
            Paragraph::new(RtSpan::styled(CACHED_GLYPH, muted)),
            Rect {
                x,
                y: area.y,
                width: UnicodeWidthStr::width(CACHED_GLYPH) as u16,
                height: 1,
            },
        );
    }
    if let Some((text, x)) = &tb.hops {
        frame.render_widget(
            Paragraph::new(RtSpan::styled(text.clone(), muted)),
            Rect {
                x: *x,
                y: area.y,
                width: UnicodeWidthStr::width(text.as_str()) as u16,
                height: 1,
            },
        );
    }
}

/// Draw the live address editor over the address slot: a `:` prompt, then the
/// field's value with its caret. The window over a long value is the editor's
/// own scroll, so the caret stays where the keys put it.
fn render_address_editor(model: &Model, field: &AddressField, frame: &mut Frame, area: Rect) {
    if area.width == 0 {
        return;
    }
    let style = chrome_text_style(model.no_color, model.theme);
    frame.render_widget(
        Paragraph::new(RtSpan::styled(":", style)),
        Rect { width: 1, ..area },
    );
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y,
        width: area.width.saturating_sub(1),
        height: 1,
    };
    if inner.width == 0 {
        return;
    }
    let w = inner.width as usize;
    let scroll = model.input.visual_scroll(w.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(RtSpan::styled(field.value.clone(), style)).scroll((0, scroll as u16)),
        inner,
    );
    let caret = field.cursor.unwrap_or_default();
    let cx = inner.x + (caret.saturating_sub(scroll)) as u16;
    frame.set_cursor_position((cx.min(inner.right().saturating_sub(1)), inner.y));
}

// --- the footer: a unified strip of clickable button / keybinding hints -----

/// Draw the status bar: always the unified footer button strip, so the reader's
/// clickable controls are never covered. In search mode it is the live `/<query>`
/// editor instead. The loading spinner and a focused/hovered link's target float
/// in the bottom-left [`render_status_chip`] over the content; transient messages
/// float as a [`render_toast`] overlay.
fn render_status(model: &Model, frame: &mut Frame, area: Rect) {
    if area.height == 0 {
        return;
    }
    // Fill the status bar full width with the chrome style; the buttons render on
    // top and inherit its background.
    frame.render_widget(
        Block::default().style(chrome_style(model.no_color, model.theme)),
        area,
    );
    // In search mode the status bar becomes the `/<query>` editor.
    if model.mode == Mode::Search {
        render_search_bar(model, frame, area);
        return;
    }
    render_footer_strip(model, frame, area);
}

/// Draw the bottom-left floating status chip: a one-line field on the content's
/// last row showing the CURRENT pointer/page state, floating over the page just
/// above the footer so the footer's clickable buttons are never covered. It
/// draws the scene's status chip: while a fetch is in flight, the spinner and
/// the loading path; else, when a link is focused (Tab) or hovered (mouse),
/// that link's target (`→ <address>`); else nothing at all. Transient events
/// use [`render_toast`] (bottom-right) instead.
fn render_status_chip(model: &Model, shown: Option<&Chip>, frame: &mut Frame, area: Rect) {
    let [_topbar, content, _status] = regions(area);
    if content.width == 0 || content.height == 0 {
        return;
    }
    let inner_budget = chip::budget(content);
    if inner_budget == 0 {
        return;
    }
    // Idle has no chip and leaves the content row untouched.
    let Some(status) = shown else {
        return;
    };
    let text_style = chrome_muted_style(model.no_color, model.theme);
    let words = status.text.resolve(model.lang);
    // A load or a download spins; a link's target only names it.
    let spins = matches!(
        status.text,
        StrText::Key(StrId::Loading | StrId::Downloading, _)
    );
    let (spans, inner_w): (Vec<RtSpan<'static>>, u16) = if spins {
        let label = truncate_to_cols(&format!(" {words}"), inner_budget.saturating_sub(1));
        let w = 1 + UnicodeWidthStr::width(label.as_str()) as u16;
        (
            vec![spinner_span(model.spin), RtSpan::styled(label, text_style)],
            w,
        )
    } else {
        let body = truncate_to_cols(&words, inner_budget);
        let w = UnicodeWidthStr::width(body.as_str()) as u16;
        (vec![RtSpan::styled(body, text_style)], w)
    };
    chip::render(frame, content, spans, inner_w, model.no_color, model.theme);
}

/// Draw lnomad's footer button strip ([`footer::render`] over its layout).
fn render_footer_strip(model: &Model, frame: &mut Frame, area: Rect) {
    let slots = footer_layout(model, from_ratatui(area));
    footer::render(
        frame,
        area,
        &slots,
        model.footer_hover,
        model.no_color,
        model.theme,
    );
}

/// Draw the in-page search editor into the status bar: a `/` prompt, then the
/// live query with its cursor, styled to read on the chrome bar.
fn render_search_bar(model: &Model, frame: &mut Frame, area: Rect) {
    if area.width == 0 {
        return;
    }
    let text_style = chrome_text_style(model.no_color, model.theme);
    frame.render_widget(
        Paragraph::new(RtSpan::styled("/", text_style)),
        Rect { width: 1, ..area },
    );
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y,
        width: area.width.saturating_sub(1),
        height: 1,
    };
    if inner.width == 0 {
        return;
    }
    let w = inner.width as usize;
    let scroll = model.search_input.visual_scroll(w.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(RtSpan::styled(
            model.search_input.value().to_string(),
            text_style,
        ))
        .scroll((0, scroll as u16)),
        inner,
    );
    let cx = inner.x + (model.search_input.visual_cursor().saturating_sub(scroll)) as u16;
    frame.set_cursor_position((cx.min(inner.right().saturating_sub(1)), inner.y));
}

/// Draw the centered places panel from the scene's sheet: a chrome-styled
/// overlay with a "Bookmarks" section and a "Pages heard" section, the
/// selected row reverse-video highlighted. Both the fill and the border use
/// the theme's chrome colours, so the panel tracks the light/dark theme.
fn render_places(model: &Model, sheet: &Sheet, frame: &mut Frame, area: Rect) {
    let lang = model.lang;
    let text_style = chrome_text_style(model.no_color, model.theme);
    let muted = chrome_muted_style(model.no_color, model.theme);
    let header = text_style.add_modifier(Modifier::BOLD);
    let selected = paint(model.tokens().selection);

    // The centred overlay box, shared with the click / hover hit-test so both
    // agree on where the panel sits. Its width fixes the inner display width (the
    // two border columns removed) rows are truncated to: a long bookmark row that
    // reached the border let the selected row's reversed bar spill past it;
    // truncating to the inner width keeps every row strictly inside the border.
    let overlay = to_ratatui(places_overlay_rect(model, from_ratatui(area)));
    let inner = overlay.width.saturating_sub(2) as usize;

    // A heading after the first stands under a blank spacer: exactly the
    // order `places_layout` models, so the entry -> line mapping there (and
    // the overlay height derived from it) stays exact.
    let mut lines: Vec<RtLine<'static>> = Vec::new();
    for row in &sheet.rows {
        match row {
            SceneRow::Heading(title) => {
                if !lines.is_empty() {
                    lines.push(RtLine::from(""));
                }
                lines.push(RtLine::from(RtSpan::styled(title.resolve(lang), header)));
            }
            SceneRow::Placeholder(text) => {
                lines.push(RtLine::from(RtSpan::styled(
                    format!("  {}", text.resolve(lang)),
                    muted,
                )));
            }
            SceneRow::Item {
                item,
                title,
                hovered,
            } => lines.push(place_line(
                &format!("  {}", title.resolve(lang)),
                item.selected,
                *hovered,
                !item.menu.is_empty(),
                text_style,
                selected,
                inner,
            )),
            SceneRow::Note { text, .. } => {
                lines.push(RtLine::from(RtSpan::styled(text.resolve(lang), muted)));
            }
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", sheet.title.resolve(lang)))
        .style(chrome_style(model.no_color, model.theme));
    frame.render_widget(Clear, overlay);

    // The block's inner height is the viewport; when the list is taller than the
    // terminal, draw the slice starting at `places_scroll` (clamped to keep the
    // selected row visible on the update side) and paint a scrollbar on the right
    // border. Same shared treatment as the help overlay.
    let total = lines.len();
    let inner_h = overlay.height.saturating_sub(2) as usize;
    let max = total.saturating_sub(inner_h);
    let offset = model.places_scroll.min(max);
    let visible: Vec<RtLine> = lines.into_iter().skip(offset).take(inner_h).collect();
    frame.render_widget(Paragraph::new(Text::from(visible)).block(block), overlay);
    if max > 0 {
        let mut state = ScrollbarState::new(max).position(offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        frame.render_stateful_widget(scrollbar, overlay.inner(Margin::new(0, 1)), &mut state);
    }
}

/// Draw the name-search result list from the scene's sheet: one row per hit
/// (`name ·hash4`, hops, last seen, pin mark), the selected row highlighted,
/// and a last line that is either the key hint or, while asking, the "same
/// name, other key" question.
fn render_names(model: &Model, sheet: &Sheet, frame: &mut Frame, area: Rect) {
    let lang = model.lang;
    let text_style = chrome_text_style(model.no_color, model.theme);
    let muted = chrome_muted_style(model.no_color, model.theme);
    let selected = paint(model.tokens().selection);

    let items: Vec<(&StrText, bool)> = sheet
        .rows
        .iter()
        .filter_map(|row| match row {
            SceneRow::Item { item, title, .. } => Some((title, item.selected)),
            _ => None,
        })
        .collect();
    let sel = items.iter().position(|(_, on)| *on).unwrap_or(0);
    let width = 76u16.min(area.width);
    let rows = names_viewport(model).min(items.len());
    let height = (rows as u16 + 3).min(area.height);
    let overlay = ratatui::layout::Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let inner = overlay.width.saturating_sub(2) as usize;
    let offset = sel.saturating_sub(rows.saturating_sub(1));

    let mut lines: Vec<RtLine<'static>> = items
        .iter()
        .skip(offset)
        .take(rows)
        .map(|(title, on)| {
            let style = if *on { selected } else { text_style };
            let row = format!("  {}", title.resolve(lang));
            RtLine::from(RtSpan::styled(truncate_to_cols(&row, inner), style))
        })
        .collect();
    for row in &sheet.rows {
        if let SceneRow::Note { text, ask } = row {
            let style = if *ask { text_style } else { muted };
            lines.push(RtLine::from(RtSpan::styled(
                truncate_to_cols(&text.resolve(lang), inner),
                style,
            )));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", sheet.title.resolve(lang)))
        .style(chrome_style(model.no_color, model.theme));
    frame.render_widget(Clear, overlay);
    frame.render_widget(Paragraph::new(Text::from(lines)).block(block), overlay);
}

/// One places-panel row: its label, reverse-video highlighted when it is the
/// selected row. A selected or hovered row with a menu (a BOOKMARK row, whose
/// menu forgets it) additionally carries a right-aligned `×` delete marker on
/// the last inner cell ([`marks_x`]).
fn place_line(
    label: &str,
    is_selected: bool,
    hovered: bool,
    removable: bool,
    normal: Style,
    selected: Style,
    inner: usize,
) -> RtLine<'static> {
    // Selection wins over hover, and the two styles differ (reverse video vs the
    // bold + underline the links and footer buttons use on hover), so a hovered
    // row stays distinguishable from the selected row even when they coincide.
    let style = if is_selected {
        selected
    } else if hovered {
        normal.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        normal
    };
    let x_col = place_x_col(inner).filter(|_| removable && (is_selected || hovered));
    let Some(x_col) = x_col else {
        let label = truncate_to_cols(label, inner);
        return RtLine::from(RtSpan::styled(label, style));
    };
    // The marker's cell plus a one-column gap are reserved: the label truncates
    // before them, so it can never overlap the `×` or spill past the border.
    let label = truncate_to_cols(label, x_col.saturating_sub(1));
    // The pad is measured with ratatui's ADDRESSING width (see `fit_width`'s
    // position-vs-paint note), so the `×` lands exactly on the cell
    // `place_x_screen_col` hit-tests.
    let pad = x_col.saturating_sub(UnicodeWidthStr::width(label.as_str()));
    RtLine::from(vec![
        RtSpan::styled(label, style),
        RtSpan::raw(" ".repeat(pad)),
        RtSpan::styled("×", normal),
    ])
}

/// Draw the scene's toast over the content ([`toast::render`]). A no-op when
/// no toast is up.
fn render_toast(model: &Model, shown: Option<&SceneToast>, frame: &mut Frame, area: Rect) {
    let (Some(t), Some(up)) = (shown, &model.toast) else {
        return;
    };
    let [_topbar, content, _status] = regions(area);
    let toast = Toast {
        kind: t.kind,
        text: t.text.resolve(model.lang),
        shown_at: up.shown_at,
    };
    toast::render(frame, content, &toast, model.tokens());
}

/// Draw lnomad's help overlay ([`help::render`] over its keybinding groups).
fn render_help(model: &Model, frame: &mut Frame, area: Rect) {
    help::render(
        frame,
        area,
        &help_groups_in(model.lang),
        StrId::HelpTitle.text(model.lang),
        model.help_scroll,
        model.no_color,
        model.theme,
    );
}

/// One picture the shell is holding: the payload, and the encoded form the
/// terminal will actually be handed.
struct ImageEntry {
    /// The file name, for saving and for the toast.
    name: String,
    /// The payload as fetched, kept so the picture can be re-encoded on a
    /// resize and saved without a second transfer.
    bytes: Vec<u8>,
    /// The encoded picture, absent when the backend is [`Backend::Text`] or
    /// the payload did not decode.
    protocol: Option<SlicedProtocol>,
    /// Where it was written, once the reader has saved it.
    saved: Option<PathBuf>,
}

/// The IO shell's side of inline images: what this terminal can draw, the
/// pictures held for the current page, and the transfer queue.
///
/// Kept out of [`Model`] on purpose. The model is pure, cloneable data that
/// every unit test builds by hand; an encoded Kitty or Sixel payload is
/// neither, and the fetch queue is IO. The model carries only the slot states,
/// which is all the layout and the assertions need.
pub struct ImageStore {
    /// What this terminal can draw.
    pictures: Pictures,
    entries: HashMap<usize, ImageEntry>,
    task: Option<tokio::task::JoinHandle<()>>,
    generation: u64,
    /// Payloads held across page changes, so a revisit costs no airtime.
    /// Bounded by bytes; see [`ImageCache`].
    cache: ImageCache,
}

impl Default for ImageStore {
    /// A store that draws nothing: no terminal was asked, so there is no
    /// picture to draw. What the unit tests and the print sink use.
    fn default() -> Self {
        ImageStore {
            pictures: Pictures::default(),
            entries: HashMap::new(),
            task: None,
            generation: 0,
            cache: ImageCache::default(),
        }
    }
}

impl ImageStore {
    /// Ask the terminal what it can draw ([`Pictures::detect`]), with a
    /// payload cache of `cache_bytes`.
    pub fn detect(no_color: bool, depth: ColorDepth, cache_bytes: u64) -> Self {
        ImageStore {
            pictures: Pictures::detect(no_color, depth),
            entries: HashMap::new(),
            task: None,
            generation: 0,
            cache: ImageCache::new(cache_bytes),
        }
    }

    /// Split a page's image requests into the ones the cache can answer at
    /// once and the ones that have to be fetched.
    ///
    /// The cached ones come back as folded-ready events: they are decoded and
    /// encoded here exactly as a fresh payload would be, so a cache hit and a
    /// fetch are indistinguishable from the model's side apart from the
    /// waiting. Note that the ENCODED form is not cached, only the payload —
    /// the cell size an encode targets depends on the terminal width, which
    /// changes between visits.
    fn take_cached(
        &mut self,
        requests: Vec<ImageRequest>,
        model: &Model,
    ) -> (Vec<AppEvent>, Vec<ImageRequest>) {
        let mut events = Vec::new();
        let mut pending = Vec::new();
        for request in requests {
            let Some(bytes) = self.cache.get(&request.target).map(<[u8]>::to_vec) else {
                pending.push(request);
                continue;
            };
            let name = model
                .image_slot(request.link)
                .map(|slot| slot.name.clone())
                .unwrap_or_else(|| "image".to_string());
            events.push(self.accept(request.link, name, bytes, model));
        }
        (events, pending)
    }

    /// The encoded picture for a link, if there is one to draw.
    fn protocol(&self, link: usize) -> Option<&SlicedProtocol> {
        self.entries.get(&link)?.protocol.as_ref()
    }

    /// Drop the current page's pictures and stop its transfer queue. The
    /// cache is deliberately untouched: it exists precisely to outlive the
    /// page that filled it.
    ///
    /// Returns the aborted transfer task. Abort only requests cancellation:
    /// the task's future, and the session clone it holds, is dropped later
    /// by the runtime. A caller that needs the clone gone awaits the handle,
    /// as [`ImageStore::stop`] does; a navigation does not, and drops it.
    fn reset(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let task = self.task.take();
        if let Some(task) = &task {
            task.abort();
        }
        self.generation = self.generation.wrapping_add(1);
        self.entries.clear();
        task
    }

    /// [`ImageStore::reset`], then wait until the aborted transfer task is
    /// gone, so the session clone it held is dropped when this returns.
    async fn stop(&mut self) {
        if let Some(task) = self.reset() {
            let _ = task.await;
        }
    }

    /// Take a fetched payload, encode it if this terminal can draw it, and
    /// return the event the model should fold.
    ///
    /// Every failure path ends in [`AppEvent::ImageText`] rather than an error:
    /// the reader still gets a line saying what the file is and can still save
    /// or open it, which is a better answer than a picture-shaped hole.
    fn accept(&mut self, link: usize, name: String, bytes: Vec<u8>, model: &Model) -> AppEvent {
        let probe = crate::image::probe(&bytes);
        let entry = ImageEntry {
            name,
            bytes,
            protocol: None,
            saved: None,
        };
        self.entries.insert(link, entry);

        if self.pictures.backend() == Backend::Text {
            return AppEvent::ImageText {
                link,
                probe,
                reason: "this terminal shows no images".to_string(),
            };
        }
        let Some(probe) = probe else {
            return AppEvent::ImageText {
                link,
                probe: None,
                reason: "not a format this browser decodes".to_string(),
            };
        };
        match self.encode(link, probe, model) {
            Ok(cells) => AppEvent::ImageReady { link, cells, probe },
            Err(reason) => AppEvent::ImageText {
                link,
                probe: Some(probe),
                reason,
            },
        }
    }

    /// Decode and encode one held payload at the size the current layout
    /// allows, storing the result. Returns the cell size it will occupy.
    fn encode(&mut self, link: usize, probe: Probe, model: &Model) -> Result<(u16, u16), String> {
        let Some(font) = self.pictures.font_size() else {
            return Err("this terminal shows no images".to_string());
        };
        let Some(entry) = self.entries.get_mut(&link) else {
            return Err("no payload".to_string());
        };
        let decoded = crate::image::decode(&entry.bytes)?;
        // Cell geometry and the row ceiling are both backend-dependent: a
        // graphics protocol paints the cell's full pixel box, half-blocks
        // carry one pixel across and two down whatever the font says.
        let backend = self.pictures.backend();
        let cells = crate::image::fit_cells(
            (probe.width, probe.height),
            backend.cell_pixels(font),
            model.width.min(u16::MAX as usize) as u16,
            crate::image::max_rows(backend, model.viewport()),
        );
        let sliced = self.pictures.encode(decoded, cells)?;
        entry.protocol = Some(sliced);
        Ok(cells)
    }

    /// Re-encode every held picture for the current layout, after a resize.
    ///
    /// Returns the events the caller should fold, so a picture that no longer
    /// fits (or now fits better) changes the rows it reserves.
    fn reencode_all(&mut self, model: &Model) -> Vec<AppEvent> {
        let links: Vec<usize> = model
            .images
            .iter()
            .filter(|slot| matches!(slot.state, SlotState::Ready { .. }))
            .map(|slot| slot.link)
            .collect();
        let mut events = Vec::new();
        for link in links {
            let Some(probe) = self
                .entries
                .get(&link)
                .and_then(|entry| crate::image::probe(&entry.bytes))
            else {
                continue;
            };
            match self.encode(link, probe, model) {
                Ok(cells) => events.push(AppEvent::ImageReady { link, cells, probe }),
                Err(reason) => events.push(AppEvent::ImageText {
                    link,
                    probe: Some(probe),
                    reason,
                }),
            }
        }
        events
    }

    /// Write a held picture into the download directory, once. A second save
    /// returns the path it already has, so `s` then `o` does not produce two
    /// files.
    fn save(&mut self, link: usize) -> Result<(PathBuf, usize), StrText> {
        let Some(entry) = self.entries.get_mut(&link) else {
            return Err(StrText::key(StrId::ImageNotArrived));
        };
        save_picture(&entry.name, &entry.bytes, &mut entry.saved)
    }
}

/// The IO handles [`run_effects`] works with, bundled.
///
/// A struct rather than eight positional arguments: the list grew past the
/// point where a caller could get the order right by reading it, and every
/// member is borrowed from the same run loop anyway.
struct Shell<'a> {
    /// The in-flight page fetch or download.
    inflight: &'a mut Option<tokio::task::JoinHandle<()>>,
    /// The fetch generation, bumped to invalidate a superseded result.
    generation: &'a mut u64,
    /// The shared session every transfer runs over.
    session: &'a Arc<Mutex<Session>>,
    /// Where a fetch reports back.
    tx: &'a mpsc::UnboundedSender<FetchOutcome>,
    /// The inline-image store and its queue.
    images: &'a mut ImageStore,
    /// Where the image queue reports back.
    image_tx: &'a mpsc::UnboundedSender<ImageOutcome>,
    /// Per-request timeout.
    timeout: Duration,
}

/// Run the effects [`update`] returned: start navigations, cancel the in-flight
/// fetch, persist an identify decision, or record a quit.
async fn run_effects(effects: Vec<Effect>, model: &mut Model, shell: Shell<'_>) {
    let Shell {
        inflight,
        generation,
        session,
        tx,
        images,
        image_tx,
        timeout,
    } = shell;
    for effect in effects {
        match effect {
            Effect::Navigate(target) => {
                // The pictures of the page being left are of no use to anyone.
                images.reset();
                spawn_fetch(inflight, generation, session, tx, timeout, target);
            }
            Effect::FetchImages(requests) => {
                // Whatever the queue was working on belongs to the page just
                // left. Reset before serving, so a cache hit is not wiped by
                // its own reset.
                images.reset();
                let (cached, pending) = images.take_cached(requests, model);
                for event in cached {
                    update(model, event);
                }
                if !pending.is_empty() {
                    images.task = Some(spawn_image_queue(
                        session,
                        image_tx,
                        timeout,
                        pending,
                        images.generation,
                    ));
                }
            }
            Effect::SaveImage { link } => say_saved(model, images.save(link)),
            Effect::OpenImage { link } => open_saved(model, images.save(link)),
            Effect::Download(target) => {
                spawn_download(inflight, generation, session, tx, timeout, target);
            }
            Effect::SetIdentify { dest, on } => {
                // The decision must be stored (and the reused link dropped)
                // BEFORE the accompanying reload's fetch builds its fresh link.
                // An in-flight fetch may hold the session lock for seconds, but
                // it is superseded by that reload anyway — abort it now so the
                // lock frees within one discovery slice.
                if let Some(handle) = inflight.take() {
                    handle.abort();
                }
                *generation = generation.wrapping_add(1);
                if let Err(err) = session.lock().await.set_identify(&dest, on).await {
                    model.say(
                        ToastKind::Error,
                        with(StrId::IdentifyNotSaved, err.to_string()),
                    );
                }
            }
            Effect::OpenExternal(url) => {
                // Hand the URL to the platform default handler (`xdg-open` on
                // Linux). `open::that` launches the handler and returns; a
                // failure only raises a toast, never takes the UI down.
                match open::that(&url) {
                    Ok(_) => model.say(ToastKind::Info, with(StrId::OpenedExternally, url)),
                    Err(_) => model.say(ToastKind::Error, with(StrId::FailedToOpen, url)),
                }
            }
            Effect::Cancel => {
                if let Some(handle) = inflight.take() {
                    handle.abort();
                }
                // Cancelling means cancelling: the queued pictures stop too.
                images.reset();
                // Invalidate any late result from the cancelled task.
                *generation = generation.wrapping_add(1);
            }
            Effect::Copy(text) => {
                // Write the OSC 52 clipboard-set sequence straight to the
                // terminal; the update side has already raised the "copied" toast.
                let _ =
                    crossterm::execute!(std::io::stdout(), crossterm::style::Print(osc52(&text)));
            }
            Effect::SaveBookmarks => {
                // Persist the bookmarks, ignoring IO errors: a failed write must
                // not take down the browser (no config dir → nothing to do).
                if let Some(path) = &model.bookmarks_path {
                    let _ = model.bookmarks.save(path);
                }
            }
            Effect::SavePins => {
                // Same rule as the bookmarks: a failed write never takes the
                // browser down, the pins then last for the session.
                if let Some(path) = &model.pins_path {
                    let _ = model.pins.save(path);
                }
            }
            Effect::Quit => model.quit = true,
            // Never raised here: the terminal is not hosted by a messenger.
            Effect::Write(_) => {}
        }
    }
}

/// Run the interactive TUI, owning the [`Session`] and driving navigation.
///
/// Does the initial fetch of `initial` and every subsequent navigation as a
/// background task; the task's result rejoins the `tokio::select!` loop as a
/// [`AppEvent::PageLoaded`] / [`AppEvent::LoadFailed`], so the UI stays live
/// (spinner animates, keys and the address editor work) while a page loads. A
/// cancel drops the in-flight task; a slow or failed fetch never blocks the UI.
pub async fn run_tui(
    session: Session,
    initial: Option<Target>,
    opts: BrowserOptions,
    theme_flag: ThemeFlag,
) -> io::Result<()> {
    install_panic_hook();

    // Resolve the theme BEFORE entering the alt-screen/raw event loop: the OSC 11
    // background query (via `termbg`) briefly toggles raw mode and reads a reply
    // on stdin, and doing it now keeps that reply from leaking into key input.
    // `termbg` restores the terminal state and drains stdin itself.
    let theme = resolve_theme(detect_background(theme_flag), theme_flag);

    let mut guard = TerminalGuard::new(CrosstermTerminal::new())?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut events = EventStream::new();

    // Load persisted bookmarks (best-effort): resolve the store path and read it,
    // tolerating a missing or corrupt file. `None` when no config dir is
    // resolvable, in which case bookmarks stay in-memory only and
    // `Effect::SaveBookmarks` becomes a no-op.
    let bookmarks_path = crate::bookmarks::default_path();
    let bookmarks = bookmarks_path
        .as_deref()
        .map(Bookmarks::load)
        .unwrap_or_default();

    // The pins (`names.rs`), loaded the same best-effort way.
    let pins_path = crate::names::default_path();
    let pins = pins_path
        .as_deref()
        .map(crate::names::PinStore::load)
        .unwrap_or_default();

    let mut model = Model {
        no_color: opts.no_color,
        no_images: opts.no_images,
        depth: opts.depth,
        theme,
        bookmarks,
        bookmarks_path,
        pins,
        pins_path,
        // The session's own fingerprint is stable for its whole life: set once.
        fingerprint: session.fingerprint_hex(),
        // Every word of the shell is in the catalogue now, so it speaks the
        // locale's language.
        lang: crate::app::strings::locale_lang(|name| std::env::var(name).ok()),
        ..Model::default()
    };
    let size = terminal.size()?;
    model.size = (size.width, size.height);
    model.relayout(content_width(size.width));

    // Wire continuous node discovery: the session forwards every node announce it
    // sees (idle-drained by the background task below, or drained by a fetch) to
    // this channel, which the select loop folds into the model registry. Set the
    // sink before sharing the session so no announce is missed at startup.
    let (node_tx, mut node_rx) = mpsc::unbounded_channel::<DiscoveredNode>();
    let mut session = session;
    session.set_announce_sink(node_tx);

    let session = Arc::new(Mutex::new(session));
    let (tx, mut rx) = mpsc::unbounded_channel::<FetchOutcome>();
    let (image_tx, mut image_rx) = mpsc::unbounded_channel::<ImageOutcome>();
    let mut inflight: Option<tokio::task::JoinHandle<()>> = None;
    let mut generation: u64 = 0;
    // Ask the terminal what it can draw once, now that raw mode is on. Every
    // failure path inside settles on half-blocks, so this cannot fail.
    let mut images = match model.no_images {
        true => ImageStore::default(),
        false => ImageStore::detect(model.no_color, model.depth, opts.image_cache_bytes),
    };

    // Continuous background discovery: lives for the whole session, independent
    // of fetches, feeding `node_rx` (see `spawn_discovery`).
    let discovery = spawn_discovery(&session);

    match initial {
        // Kick off the initial navigation, honouring a `#anchor` on the initial
        // URL (the parser folds it into the path; split it back off so the
        // fetched path is clean and the load handler can scroll to it).
        Some(target) => {
            let (target, anchor) = split_path_anchor(target);
            model.pending = Some(Pending {
                target: target.clone(),
                action: HistoryAction::Push,
            });
            model.pending_anchor = anchor;
            spawn_fetch(
                &mut inflight,
                &mut generation,
                &session,
                &tx,
                opts.timeout,
                target,
            );
        }
        // No URL to open: show the start screen and put the places panel in
        // front of it, since that is where a reader without an address finds
        // one — their bookmarks, and the nodes background discovery turns up.
        None => {
            show_start_screen(&mut model);
        }
    }

    let mut ticker = tokio::time::interval(Duration::from_millis(SPINNER_TICK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let result = loop {
        if let Err(err) = terminal.draw(|frame| view(&model, &images, frame)) {
            break Err(err);
        }
        if model.quit {
            break Ok(());
        }
        // Tick while a fetch spinner animates or a toast is aging towards its
        // auto-dismiss, so an idle toast still expires.
        let animate = model.needs_tick();
        tokio::select! {
            maybe_event = events.next() => match step_event(maybe_event, map_event) {
                EventStep::Apply(app) => {
                    // A resize changes how many cells a picture may occupy, so
                    // every held picture is re-encoded for the new layout.
                    let resized = matches!(app, AppEvent::Resize(_, _));
                    let mut effects = update(&mut model, app);
                    if resized {
                        for event in images.reencode_all(&model) {
                            effects.extend(update(&mut model, event));
                        }
                    }
                    run_effects(
                        effects,
                        &mut model,
                        Shell {
                            inflight: &mut inflight,
                            generation: &mut generation,
                            session: &session,
                            tx: &tx,
                            images: &mut images,
                            image_tx: &image_tx,
                            timeout: opts.timeout,
                        },
                    )
                    .await;
                }
                // A recognised-but-inert event (focus/paste, which map to
                // nothing): keep looping.
                EventStep::Ignore => {}
                // A closed stream (Ok) or a genuine IO error (Err) both leave
                // the loop; the error propagates so a dead terminal cannot
                // busy-loop.
                EventStep::End(result) => break result,
            },
            Some(outcome) = rx.recv() => {
                if outcome.generation == generation {
                    inflight = None;
                    match outcome.result {
                        LoadResult::Ok { doc, title, dest, identifying } => {
                            let effects =
                                update(&mut model, AppEvent::PageLoaded { doc, title });
                            run_effects(
                                effects,
                                &mut model,
                                Shell {
                                    inflight: &mut inflight,
                                    generation: &mut generation,
                                    session: &session,
                                    tx: &tx,
                                    images: &mut images,
                                    image_tx: &image_tx,
                                    timeout: opts.timeout,
                                },
                            )
                            .await;
                            // The session's answer for this load is
                            // authoritative: it corrects the optimistic toggle
                            // and covers a first visit to a node persisted in
                            // identify.toml by an earlier run.
                            model.note_identify(dest, identifying);
                        }
                        LoadResult::Err(text) => {
                            update(&mut model, AppEvent::LoadFailedText(text));
                        }
                        LoadResult::Downloaded { name, bytes, dir } => {
                            update(&mut model, AppEvent::DownloadDone { name, bytes, dir });
                        }
                        LoadResult::DownloadErr(msg) => {
                            update(&mut model, AppEvent::DownloadFailed(msg));
                        }
                    }
                }
            },
            Some(outcome) = image_rx.recv() => {
                // A stale generation belongs to a page the reader has left.
                if outcome.generation == images.generation {
                    let event = match outcome.result {
                        ImageResult::Started => AppEvent::ImageLoading { link: outcome.link },
                        ImageResult::Failed(reason) => {
                            AppEvent::ImageFailed { link: outcome.link, reason }
                        }
                        ImageResult::Bytes(bytes) => {
                            let name = model
                                .image_slot(outcome.link)
                                .map(|slot| slot.name.clone())
                                .unwrap_or_else(|| "image".to_string());
                            // Cached before it is drawn, so it survives the
                            // page: the airtime is already spent either way.
                            images.cache.insert(outcome.target, bytes.clone());
                            images.accept(outcome.link, name, bytes, &model)
                        }
                    };
                    update(&mut model, event);
                }
            }
            Some(node) = node_rx.recv() => {
                update(&mut model, AppEvent::NodeDiscovered(node));
            }
            _ = ticker.tick(), if animate => {
                update(&mut model, AppEvent::Tick);
            }
        }
    };

    // Tear down: abort and drain the in-flight task and the background discovery
    // task (releasing their session clones), then best-effort close the session
    // once no task clone of it survives.
    if let Some(handle) = inflight.take() {
        handle.abort();
        let _ = handle.await;
    }
    images.stop().await;
    discovery.abort();
    let _ = discovery.await;
    drop(tx);
    let leaked = close_session(session).await;

    guard.restore_now()?;
    if let Some(message) = leaked {
        eprintln!("{message}");
    }
    result
}

/// Translate a crossterm event into an [`AppEvent`], dropping the ones the UI
/// does not act on (focus, paste).
fn map_event(event: Event) -> Option<AppEvent> {
    match event {
        Event::Key(key) => Some(AppEvent::Key(key_event(key))),
        Event::Mouse(mouse) => Some(AppEvent::Mouse(mouse_event(mouse))),
        Event::Resize(cols, rows) => Some(AppEvent::Resize(cols, rows)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::geometry::{regions, Rect};
    use crate::bookmarks::Bookmark;
    use crate::browser;
    use crate::color::rgb_to_ansi256;
    use crate::discovery::NomadNodeRegistry;
    use crate::image::MAX_INLINE_IMAGES;
    use crate::page_cache::CacheEntry;
    use crate::render::RenderedLink;
    use crate::render::{RLine, RStyle};
    use crate::theme::Theme;
    use crate::url::{parse_url, DEFAULT_PATH};
    use leviculum_micron::parse;
    use leviculum_ui::spinner::{hsv_to_rgb, HUE_STEP, SPIN_FRAMES};
    use leviculum_ui_ratatui::images::{Picker, ProtocolType};
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    const SAMPLE: &str = include_str!("../tests/fixtures/sample.mu");

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
        }
    }

    fn press(model: &mut Model, code: KeyCode, mods: KeyModifiers) -> Vec<Effect> {
        update(model, AppEvent::Key(key(code, mods)))
    }

    fn type_str(model: &mut Model, s: &str) {
        for ch in s.chars() {
            press(model, KeyCode::Char(ch), KeyModifiers::NONE);
        }
    }

    fn tgt(n: u8) -> Target {
        Target {
            dest_hash: [n; 16],
            path: format!("/page/{n}.mu"),
            fields: Vec::new(),
            is_file: false,
        }
    }

    fn model_from_sample(width: usize, size: (u16, u16)) -> Model {
        Model::from_document(&parse(SAMPLE), width, "Sample", size)
    }

    /// The full buffer flattened to a newline-joined string.
    fn flat(buffer: &ratatui::buffer::Buffer) -> String {
        let mut s = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                s.push_str(buffer[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    /// The text of buffer row `y` across columns `0..width`.
    fn row_text(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
        let mut s = String::new();
        for x in 0..width {
            s.push_str(buffer[(x, y)].symbol());
        }
        s
    }

    /// The plain text of a laid-out line (its cells' characters).
    fn line_text(line: &RLine) -> String {
        line.cells.iter().map(|c| c.ch).collect()
    }

    #[test]
    fn quit_key_sets_quit_flag() {
        let mut model = model_from_sample(80, (80, 24));
        assert!(!model.quit);
        let effects = press(&mut model, KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(model.quit);
        assert_eq!(effects, vec![Effect::Quit]);
    }

    #[test]
    fn ctrl_c_sets_quit_flag() {
        let mut model = model_from_sample(80, (80, 24));
        let effects = press(&mut model, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(model.quit);
        assert_eq!(effects, vec![Effect::Quit]);
    }

    #[test]
    fn plain_c_does_not_quit() {
        let mut model = model_from_sample(80, (80, 24));
        press(&mut model, KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(!model.quit);
    }

    #[test]
    fn resize_updates_size() {
        let mut model = model_from_sample(80, (80, 24));
        update(&mut model, AppEvent::Resize(40, 10));
        assert_eq!(model.size, (40, 10));
    }

    #[test]
    fn content_region_is_total_minus_chrome() {
        assert_eq!(content_height(24), 22);
        let [topbar, content, status] = regions(Rect::new(0, 0, 80, 24));
        assert_eq!(topbar.height, TOPBAR_ROWS);
        assert_eq!(
            topbar.y, 0,
            "content begins on the row after the one-row top bar"
        );
        assert_eq!(content.y, TOPBAR_ROWS);
        assert_eq!(status.height, STATUS_ROWS);
        assert_eq!(content.height, 24 - CHROME_ROWS);
        assert_eq!(model_from_sample(80, (80, 24)).viewport(), 22);
    }

    // --- History ---------------------------------------------------------

    #[test]
    fn history_visit_back_forward_and_truncation() {
        let mut h = History::default();
        assert!(!h.can_back());
        assert!(!h.can_forward());
        assert_eq!(h.current(), None);

        let (a, b, c) = (tgt(1), tgt(2), tgt(3));
        h.visit(a.clone());
        assert_eq!(h.current(), Some(&a));
        assert!(!h.can_back());

        h.visit(b.clone());
        assert!(h.can_back());
        assert!(!h.can_forward());
        assert_eq!(h.current(), Some(&b));

        assert_eq!(h.back(), Some(&a));
        assert_eq!(h.current(), Some(&a));
        assert!(h.can_forward());
        assert_eq!(h.forward(), Some(&b));

        // From the middle, a fresh visit truncates the forward branch.
        assert_eq!(h.back(), Some(&a));
        h.visit(c.clone());
        assert_eq!(h.current(), Some(&c));
        assert!(!h.can_forward());
        assert_eq!(h.stack.len(), 2);

        // back then forward lands on c, not the discarded b.
        assert_eq!(h.back(), Some(&a));
        assert_eq!(h.forward(), Some(&c));
    }

    #[test]
    fn history_edges_return_none() {
        let mut h = History::default();
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), None);
        h.visit(tgt(1));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), None);
        assert_eq!(h.idx, 0);
    }

    // --- update / Effect -------------------------------------------------

    #[test]
    fn colon_enters_address_mode() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        let effects = press(&mut m, KeyCode::Char(':'), KeyModifiers::NONE);
        assert!(effects.is_empty());
        assert_eq!(m.mode, Mode::Address);
    }

    #[test]
    fn typing_then_enter_yields_navigate() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        press(&mut m, KeyCode::Char(':'), KeyModifiers::NONE);
        let hash = "0123456789abcdef0123456789abcdef";
        type_str(&mut m, &format!("{hash}:/page/about.mu"));
        let effects = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        let expected = parse_url(&format!("{hash}:/page/about.mu"), None).unwrap();
        assert_eq!(effects, vec![Effect::Navigate(expected.clone())]);
        assert_eq!(m.mode, Mode::Browse);
        assert_eq!(m.pending.as_ref().map(|p| &p.target), Some(&expected));
        assert!(m.input.value().is_empty());
    }

    #[test]
    fn bad_url_sets_error_and_no_navigate() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        press(&mut m, KeyCode::Char(':'), KeyModifiers::NONE);
        // A scheme form is an address, never a name search, so a bad one
        // stays a bad URL.
        type_str(&mut m, "nomadnet://not-a-hash");
        let effects = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert!(effects.is_empty());
        let toast = m.toast.as_ref().expect("an error toast is raised");
        assert_eq!(toast.kind, ToastKind::Error);
        assert!(toast.text.contains("not-a-hash"), "toast: {}", toast.text);
        // Still in address mode so the user can fix the input.
        assert_eq!(m.mode, Mode::Address);
    }

    #[test]
    fn alt_left_on_history_yields_navigate_prev() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        let (a, b) = (tgt(1), tgt(2));
        m.history.visit(a.clone());
        m.history.visit(b.clone());
        m.current_dest = Some(b.dest_hash);

        let effects = press(&mut m, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(effects, vec![Effect::Navigate(a.clone())]);
        // The cursor only moves once the page loads.
        assert_eq!(m.history.idx, 1);
        assert_eq!(
            m.pending.as_ref().map(|p| p.action.clone()),
            Some(HistoryAction::Goto(0))
        );
    }

    #[test]
    fn mouse_back_button_on_history_yields_navigate_prev() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        let (a, b) = (tgt(1), tgt(2));
        m.history.visit(a.clone());
        m.history.visit(b.clone());
        m.current_dest = Some(b.dest_hash);

        let effects = update(
            &mut m,
            AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Back))),
        );
        assert_eq!(effects, vec![Effect::Navigate(a.clone())]);
        // The cursor only moves once the page loads (same path as Alt-Left).
        assert_eq!(m.history.idx, 1);
        assert_eq!(
            m.pending.as_ref().map(|p| p.action.clone()),
            Some(HistoryAction::Goto(0))
        );
    }

    #[test]
    fn mouse_forward_button_on_history_yields_navigate_next() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        let (a, b) = (tgt(1), tgt(2));
        m.history.visit(a.clone());
        m.history.visit(b);
        // Step back so a forward step exists.
        m.history.goto(0);
        m.current_dest = Some(a.dest_hash);

        let effects = update(
            &mut m,
            AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Forward))),
        );
        assert_eq!(effects, vec![Effect::Navigate(tgt(2))]);
        assert_eq!(m.history.idx, 0);
        assert_eq!(
            m.pending.as_ref().map(|p| p.action.clone()),
            Some(HistoryAction::Goto(1))
        );
    }

    #[test]
    fn mouse_side_buttons_are_noop_without_history() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        // No prior navigation: nothing behind or ahead.
        let back = update(
            &mut m,
            AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Back))),
        );
        let forward = update(
            &mut m,
            AppEvent::Mouse(mouse(MouseEventKind::Down(MouseButton::Forward))),
        );
        assert!(back.is_empty());
        assert!(forward.is_empty());
        assert!(m.pending.is_none());
    }

    #[test]
    fn esc_during_loading_yields_cancel() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.pending = Some(Pending {
            target: tgt(1),
            action: HistoryAction::Push,
        });
        let effects = press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(effects, vec![Effect::Cancel]);
        assert!(m.pending.is_none());
        let toast = m.toast.as_ref().expect("a cancel toast is raised");
        assert_eq!(toast.kind, ToastKind::Info);
        assert_eq!(toast.text, "cancelled");
    }

    #[test]
    fn ctrl_g_during_loading_yields_cancel() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.pending = Some(Pending {
            target: tgt(1),
            action: HistoryAction::Push,
        });
        let effects = press(&mut m, KeyCode::Char('g'), KeyModifiers::CONTROL);
        assert_eq!(effects, vec![Effect::Cancel]);
        assert!(m.pending.is_none());
    }

    #[test]
    fn page_loaded_updates_doc_pushes_history_resets_scroll() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.scroll = 5;
        m.pending = Some(Pending {
            target: tgt(7),
            action: HistoryAction::Push,
        });
        let effects = update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("Fresh page body"),
                title: "Fresh".to_string(),
            },
        );
        assert!(effects.is_empty());
        assert_eq!(m.scroll, 0);
        assert_eq!(m.title, "Fresh");
        assert_eq!(m.history.stack.len(), 1);
        assert_eq!(m.history.stack[0], tgt(7));
        assert!(m.pending.is_none());
        assert_eq!(m.current_dest, Some([7; 16]));
        assert!(flat_page(&m).contains("Fresh"), "page: {:?}", flat_page(&m));
    }

    /// The current page's flattened text.
    fn flat_page(m: &Model) -> String {
        m.page.iter().map(line_text).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn load_failed_keeps_page_and_sets_status() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.pending = Some(Pending {
            target: tgt(1),
            action: HistoryAction::Push,
        });
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("Kept page body"),
                title: "Kept".to_string(),
            },
        );
        let lines_before = m.page.len();

        m.pending = Some(Pending {
            target: tgt(2),
            action: HistoryAction::Push,
        });
        let effects = update(&mut m, AppEvent::LoadFailed("no path".to_string()));
        assert!(effects.is_empty());
        let toast = m.toast.as_ref().expect("a failure toast is raised");
        assert_eq!(toast.kind, ToastKind::Error);
        assert_eq!(toast.text, "no path");
        assert!(m.pending.is_none());
        // The page is unchanged: still the previously loaded document.
        assert_eq!(m.title, "Kept");
        assert_eq!(m.page.len(), lines_before);
        assert_eq!(m.history.stack.len(), 1);
    }

    #[test]
    fn reload_navigates_current_without_history_change() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.history.visit(tgt(3));
        let effects = press(&mut m, KeyCode::Char('R'), KeyModifiers::NONE);
        assert_eq!(effects, vec![Effect::Navigate(tgt(3))]);
        assert_eq!(
            m.pending.as_ref().map(|p| p.action.clone()),
            Some(HistoryAction::Goto(0))
        );
    }

    #[test]
    fn reload_bound_to_r_ctrl_r_and_f5_but_not_bare_r() {
        // R, Ctrl-R and F5 each reload the current page (browser muscle memory);
        // bare lowercase r is deliberately left free, as a mesh reload is
        // expensive and single letters are reserved for cheap local actions.
        for (code, mods) in [
            (KeyCode::Char('R'), KeyModifiers::NONE),
            (KeyCode::Char('r'), KeyModifiers::CONTROL),
            (KeyCode::F(5), KeyModifiers::NONE),
        ] {
            let mut m = model_from_sample(content_width(80), (80, 24));
            m.history.visit(tgt(3));
            let effects = press(&mut m, code, mods);
            assert_eq!(
                effects,
                vec![Effect::Navigate(tgt(3))],
                "{code:?} + {mods:?} should reload"
            );
            assert_eq!(
                m.pending.as_ref().map(|p| p.action.clone()),
                Some(HistoryAction::Goto(0)),
                "{code:?} + {mods:?} reload must not change history"
            );
        }
        // Bare lowercase r does nothing: no navigation, no pending load.
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.history.visit(tgt(3));
        let effects = press(&mut m, KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(effects.is_empty(), "bare r must be unbound: {effects:?}");
        assert!(m.pending.is_none(), "bare r must not start a load");
    }

    // --- page cache ------------------------------------------------------

    #[test]
    fn is_form_submit_flags_field_targets_only() {
        let submit = Target {
            dest_hash: [3; 16],
            path: "/page/s.mu".to_string(),
            fields: vec![("field_name".to_string(), "value".to_string())],
            is_file: false,
        };
        assert!(is_form_submit(&submit));
        // A `var_` preset is idempotent and cacheable.
        let preset = Target {
            dest_hash: [3; 16],
            path: "/page/s.mu".to_string(),
            fields: vec![("var_a".to_string(), "1".to_string())],
            is_file: false,
        };
        assert!(!is_form_submit(&preset));
        assert!(!is_form_submit(&tgt(1)));
    }

    #[test]
    fn page_loaded_inserts_into_the_cache() {
        let mut m = tall_model((80, 24));
        m.pending = Some(Pending {
            target: tgt(1),
            action: HistoryAction::Push,
        });
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_doc(),
                title: "A".to_string(),
            },
        );
        assert!(m.page_cache.contains(&tgt(1)), "fresh load is cached");
        assert!(!m.cached_view, "a fresh load is not a cached view");
    }

    #[test]
    fn fresh_navigation_to_cached_target_loads_from_cache_without_fetch() {
        let mut m = tall_model((80, 24));
        m.history.visit(tgt(1));
        m.current_dest = Some([1; 16]);
        m.page_cache.insert(
            tgt(2),
            CacheEntry {
                doc: long_doc(),
                title: "Two".to_string(),
                scroll: 7,
            },
        );
        let fx = begin_navigation(&mut m, tgt(2), HistoryAction::Push, None);
        assert!(fx.is_empty(), "a cache hit emits no fetch: {fx:?}");
        assert!(m.pending.is_none(), "no fetch is pending");
        assert_eq!(m.title, "Two");
        assert_eq!(m.scroll, 7, "the stored scroll is restored");
        assert!(m.cached_view, "the cached marker is set");
        // History is folded immediately (no async round-trip).
        assert_eq!(m.history.stack, vec![tgt(1), tgt(2)]);
        assert_eq!(m.history.idx, 1);
    }

    #[test]
    fn back_and_forward_hit_the_cache_and_restore_scroll() {
        // A tall, narrow viewport makes the sample document long enough that a
        // scroll offset of 12 is well within range (not clamped on restore).
        let mut m = tall_model((40, 13));
        // Load A through the fetch path so it caches.
        m.pending = Some(Pending {
            target: tgt(1),
            action: HistoryAction::Push,
        });
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_doc(),
                title: "A".to_string(),
            },
        );
        m.scroll = 12;

        // Navigate to B (a fresh fetch): leaving A stashes its scroll.
        let fx = begin_navigation(&mut m, tgt(2), HistoryAction::Push, None);
        assert_eq!(fx, vec![Effect::Navigate(tgt(2))]);
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_doc(),
                title: "B".to_string(),
            },
        );
        m.scroll = 3;

        // Back to A: instant from cache, scroll 12 restored, no fetch.
        let fx = press(&mut m, KeyCode::Left, KeyModifiers::ALT);
        assert!(fx.is_empty(), "back hit the cache: {fx:?}");
        assert!(m.pending.is_none());
        assert_eq!(m.history.idx, 0, "history moved immediately");
        assert_eq!(m.title, "A");
        assert_eq!(m.scroll, 12, "A's stashed scroll restored");
        assert!(m.cached_view);

        // Forward to B: instant, scroll 3 restored (stashed on leaving B).
        let fx = press(&mut m, KeyCode::Right, KeyModifiers::ALT);
        assert!(fx.is_empty(), "forward hit the cache: {fx:?}");
        assert_eq!(m.history.idx, 1);
        assert_eq!(m.title, "B");
        assert_eq!(m.scroll, 3);
        assert!(m.cached_view);
    }

    #[test]
    fn reload_bypasses_cache_and_overwrites_the_entry() {
        let mut m = tall_model((80, 24));
        m.history.visit(tgt(1));
        m.current_dest = Some([1; 16]);
        m.page_cache.insert(
            tgt(1),
            CacheEntry {
                doc: parse("old body"),
                title: "Old".to_string(),
                scroll: 3,
            },
        );
        // R refetches even though the page is cached.
        let fx = press(&mut m, KeyCode::Char('R'), KeyModifiers::NONE);
        assert_eq!(fx, vec![Effect::Navigate(tgt(1))]);
        assert_eq!(
            m.pending.as_ref().map(|p| p.action.clone()),
            Some(HistoryAction::Goto(0))
        );
        // On load the entry is overwritten with the fresh page.
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("new body"),
                title: "New".to_string(),
            },
        );
        assert!(!m.cached_view, "a fresh load clears the cached marker");
        let hit = m.page_cache.get(&tgt(1)).expect("still cached");
        assert_eq!(hit.title, "New", "the entry was overwritten");
    }

    #[test]
    fn form_submit_is_never_cached_and_always_fetches() {
        let mut m = tall_model((80, 24));
        let submit = Target {
            dest_hash: [3; 16],
            path: "/page/s.mu".to_string(),
            fields: vec![("field_name".to_string(), "value".to_string())],
            is_file: false,
        };
        let fx = begin_navigation(&mut m, submit.clone(), HistoryAction::Push, None);
        assert_eq!(fx, vec![Effect::Navigate(submit.clone())], "always fetches");
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("submitted"),
                title: "S".to_string(),
            },
        );
        assert!(
            !m.page_cache.contains(&submit),
            "a form submit result must not be cached"
        );
    }

    #[test]
    fn cache_hit_while_loading_cancels_the_inflight_fetch() {
        let mut m = tall_model((80, 24));
        m.history.visit(tgt(1));
        m.current_dest = Some([1; 16]);
        m.page_cache.insert(
            tgt(2),
            CacheEntry {
                doc: long_doc(),
                title: "Two".to_string(),
                scroll: 0,
            },
        );
        m.pending = Some(Pending {
            target: tgt(9),
            action: HistoryAction::Push,
        });
        let fx = begin_navigation(&mut m, tgt(2), HistoryAction::Push, None);
        assert_eq!(fx, vec![Effect::Cancel], "a stale fetch is cancelled");
        assert!(m.pending.is_none());
        assert_eq!(m.title, "Two");
    }

    #[test]
    fn cached_marker_shown_only_when_view_is_cached() {
        let mut m = loaded_model((80, 24));
        let fresh = flat(&render(&m, 80, 24));
        assert!(!fresh.contains(CACHED_GLYPH), "no bolt on a fresh page");
        m.cached_view = true;
        let cached = flat(&render(&m, 80, 24));
        assert!(
            cached.contains(CACHED_GLYPH),
            "cache bolt shown when cached:\n{cached}"
        );
    }

    // --- scrolling (retained from phase 2) -------------------------------

    #[test]
    fn scrolled_is_the_one_scroll_rule() {
        let vp = 10;
        let max = 90;
        // Line motions step one, clamped at both ends.
        assert_eq!(scrolled(5, ScrollCmd::LineDown, vp, max), 6);
        assert_eq!(scrolled(5, ScrollCmd::LineUp, vp, max), 4);
        assert_eq!(scrolled(0, ScrollCmd::LineUp, vp, max), 0, "clamps at 0");
        assert_eq!(
            scrolled(max, ScrollCmd::LineDown, vp, max),
            max,
            "clamps at max"
        );
        // Full page uses the viewport, half page uses viewport/2.
        assert_eq!(scrolled(0, ScrollCmd::PageDown, vp, max), 10);
        assert_eq!(scrolled(20, ScrollCmd::PageUp, vp, max), 10);
        assert_eq!(scrolled(0, ScrollCmd::HalfPageDown, vp, max), 5);
        assert_eq!(scrolled(20, ScrollCmd::HalfPageUp, vp, max), 15);
        // Page motions still clamp to the ends.
        assert_eq!(scrolled(85, ScrollCmd::PageDown, vp, max), max);
        assert_eq!(scrolled(3, ScrollCmd::PageUp, vp, max), 0);
        // Top and Bottom jump straight to the extremes.
        assert_eq!(scrolled(42, ScrollCmd::Top, vp, max), 0);
        assert_eq!(scrolled(0, ScrollCmd::Bottom, vp, max), max);
        // A zero viewport never divides by zero and still advances by one.
        assert_eq!(scrolled(0, ScrollCmd::HalfPageDown, 0, max), 1);
        // A max of 0 (content fits) pins every motion at 0.
        assert_eq!(scrolled(0, ScrollCmd::PageDown, vp, 0), 0);
        assert_eq!(scrolled(0, ScrollCmd::Bottom, vp, 0), 0);
    }

    #[test]
    fn apply_scroll_math() {
        let mut m = Model {
            page: vec![RLine::default(); 100],
            ..Model::default()
        };
        let vp = 10;
        m.apply_scroll(ScrollCmd::LineDown, vp);
        assert_eq!(m.scroll, 1, "LineDown increments");

        m.scroll = 0;
        m.apply_scroll(ScrollCmd::PageDown, vp);
        assert_eq!(m.scroll, 10, "PageDown moves by viewport");

        m.scroll = 0;
        m.apply_scroll(ScrollCmd::HalfPageDown, vp);
        assert_eq!(m.scroll, 5, "HalfPage moves by viewport/2");

        m.scroll = 42;
        m.apply_scroll(ScrollCmd::Top, vp);
        assert_eq!(m.scroll, 0, "Top is 0");

        m.apply_scroll(ScrollCmd::Bottom, vp);
        assert_eq!(m.scroll, 90, "Bottom is max = len - viewport");

        m.apply_scroll(ScrollCmd::LineDown, vp);
        assert_eq!(m.scroll, 90, "LineDown clamps at bottom");
        m.apply_scroll(ScrollCmd::PageDown, vp);
        assert_eq!(m.scroll, 90, "PageDown clamps at bottom");

        m.scroll = 0;
        m.apply_scroll(ScrollCmd::LineUp, vp);
        assert_eq!(m.scroll, 0, "LineUp clamps at top");
        m.apply_scroll(ScrollCmd::PageUp, vp);
        assert_eq!(m.scroll, 0, "PageUp clamps at top");
    }

    #[test]
    fn apply_scroll_no_overflow_when_page_shorter_than_viewport() {
        let mut m = Model {
            page: vec![RLine::default(); 3],
            ..Model::default()
        };
        let vp = 10;
        for cmd in [
            ScrollCmd::LineDown,
            ScrollCmd::PageDown,
            ScrollCmd::HalfPageDown,
            ScrollCmd::Bottom,
        ] {
            m.scroll = 0;
            m.apply_scroll(cmd, vp);
            assert_eq!(m.scroll, 0, "{cmd:?} must clamp to 0 for a short page");
        }
        let mut e = Model::default();
        e.apply_scroll(ScrollCmd::PageDown, 0);
        e.apply_scroll(ScrollCmd::Bottom, 0);
        assert_eq!(e.scroll, 0);
    }

    fn long_doc() -> leviculum_micron::MicronDocument {
        let words: Vec<String> = (0..300).map(|i| format!("word{i:03}")).collect();
        parse(&words.join(" "))
    }

    fn tall_model(size: (u16, u16)) -> Model {
        Model::from_document(&long_doc(), content_width(size.0), "Long", size)
    }

    #[test]
    fn update_scroll_keys() {
        let mut m = tall_model((40, 13)); // viewport 13 - 2 = 11
        let vp = m.viewport();
        assert_eq!(vp, 11);
        assert_eq!(m.scroll, 0);

        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(m.scroll, 1, "j scrolls down one line");

        press(&mut m, KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!(m.scroll, 1 + vp, "Ctrl-f pages down by the viewport");

        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        let bottom = m.max_scroll(vp);
        assert!(bottom > 0);
        assert_eq!(m.scroll, bottom, "G jumps to the bottom");

        press(&mut m, KeyCode::Char('v'), KeyModifiers::ALT);
        assert_eq!(m.scroll, bottom - vp, "Alt-v pages up by the viewport");

        let before = m.scroll;
        update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollDown)));
        assert_eq!(
            m.scroll,
            (before + WHEEL_STEP).min(bottom),
            "wheel down scrolls by WHEEL_STEP"
        );
    }

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn vi_and_emacs_map_same_command() {
        let cases = [
            (
                key(KeyCode::Char('j'), KeyModifiers::NONE),
                key(KeyCode::Char('n'), KeyModifiers::CONTROL),
            ),
            (
                key(KeyCode::Char('k'), KeyModifiers::NONE),
                key(KeyCode::Char('p'), KeyModifiers::CONTROL),
            ),
            (
                key(KeyCode::Char('f'), KeyModifiers::CONTROL),
                key(KeyCode::Char(' '), KeyModifiers::NONE),
            ),
            (
                key(KeyCode::Char('b'), KeyModifiers::CONTROL),
                key(KeyCode::Char('v'), KeyModifiers::ALT),
            ),
            (
                key(KeyCode::Char('g'), KeyModifiers::NONE),
                key(KeyCode::Home, KeyModifiers::NONE),
            ),
            (
                key(KeyCode::Char('G'), KeyModifiers::NONE),
                key(KeyCode::End, KeyModifiers::NONE),
            ),
        ];
        for (vi, emacs) in cases {
            let cmd = key_to_scroll(&vi);
            assert!(cmd.is_some(), "{vi:?} should map to a scroll command");
            assert_eq!(cmd, key_to_scroll(&emacs), "{vi:?} vs {emacs:?}");
        }
    }

    #[test]
    fn resize_rewraps_and_clamps() {
        let mut m = Model::from_document(&long_doc(), content_width(100), "Long", (100, 20));
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);

        update(&mut m, AppEvent::Resize(40, 20));
        let narrow = m.page.len();
        assert!(
            m.scroll <= m.max_scroll(m.viewport()),
            "scroll must stay clamped after shrinking"
        );

        update(&mut m, AppEvent::Resize(100, 20));
        let wide = m.page.len();
        assert!(
            m.scroll <= m.max_scroll(m.viewport()),
            "scroll must stay clamped after growing"
        );
        assert!(
            narrow > wide,
            "a narrower width must re-wrap into more lines: {narrow} vs {wide}"
        );
    }

    // --- view (TestBackend) ---------------------------------------------

    /// A loaded model with a seeded history entry, ready to render.
    fn loaded_model(size: (u16, u16)) -> Model {
        let mut m =
            Model::from_document(&parse(SAMPLE), content_width(size.0), " Test Node ", size);
        m.history.visit(tgt(0xab));
        m.current_dest = Some([0xab; 16]);
        m
    }

    fn render(model: &Model, w: u16, h: u16) -> ratatui::buffer::Buffer {
        render_with_images(model, &ImageStore::default(), w, h)
    }

    /// Render with a specific image store. The default one holds no pictures
    /// and draws none, which is what every test that is not about images
    /// wants; the image tests build one that does.
    fn render_with_images(
        model: &Model,
        images: &ImageStore,
        w: u16,
        h: u16,
    ) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| view(model, images, frame))
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    #[test]
    fn topbar_is_one_row_title_address_and_cluster() {
        let m = loaded_model((80, 24));
        let buffer = render(&m, 80, 24);
        // Everything lives on the single identity row (row 0).
        let top = row_text(&buffer, 0, 80);
        assert!(top.contains("Test Node"), "title missing: {top:?}");
        // The address, carrying the path exactly once (never in the title too).
        assert!(top.contains(":/page/171.mu"), "address missing: {top:?}");
        assert_eq!(
            top.matches(":/page/171.mu").count(),
            1,
            "path must appear exactly once: {top:?}"
        );
        // Exactly one · separator between title and address.
        assert_eq!(top.matches('·').count(), 1, "want one separator: {top:?}");
        // The status cluster shows a hollow star (this page is not bookmarked).
        assert!(top.contains(STAR_OFF), "bookmark star missing: {top:?}");
    }

    #[test]
    fn a_hostile_announce_name_cannot_control_the_frame() {
        // Codeberg #356: the node display name arrives in a remote node's announce
        // and reaches this frame through browser::page_title and the places panel,
        // never through the micron parser — so the filter #281 put at the parser
        // boundary does not cover it. The strip sits where the announce bytes
        // become a name (discovery::decode_node_name); this test pins the property
        // at the sink the reader actually sees.
        let hostile = b"Node\x1b[2J\x1b]0;pwned\x07\xc2\x9b31m\x7f\tTail";
        let mut reg = NomadNodeRegistry::new();
        assert!(reg.observe_parts(
            &crate::discovery::nomad_node_name_hash(),
            [0xcd; 16],
            hostile,
            Some(1),
            10,
        ));
        let name = reg.get_by_hash(&[0xcd; 16]).and_then(|n| n.name.clone());
        let title = crate::browser::page_title(name.as_deref(), &[0xab; 16], "/page/171.mu");

        let mut m =
            Model::from_document(&parse(SAMPLE), content_width(80), title.clone(), (80, 24));
        m.history.visit(tgt(0xab));
        m.current_dest = Some([0xab; 16]);
        m.node_registry = reg;
        m.show_places = true;
        let buffer = render(&m, 80, 24);

        // No cell of the frame — top bar or places row — holds anything that could
        // act on a terminal.
        for y in 0..24 {
            for x in 0..80 {
                let sym = buffer[(x, y)].symbol();
                assert!(
                    !sym.chars()
                        .any(|c| c.is_control() || ('\u{80}'..='\u{9f}').contains(&c)),
                    "a control character reached cell ({x},{y}): {sym:?}"
                );
            }
        }

        // Our column accounting agrees with what is drawn: the separator follows
        // the title immediately, so the announce can neither shift the address the
        // reader judges the page by nor push its path off the bar.
        let top = row_text(&buffer, 0, 80);
        assert!(
            top.starts_with(&format!("{title}{TOPBAR_SEP}")),
            "title and separator are not flush: {top:?}"
        );
        assert!(top.contains(":/page/171.mu"), "address missing: {top:?}");
        // The printable residue stays visible, so the reader is shown the attempt.
        assert!(top.contains("Node"), "the name's text is missing: {top:?}");
    }

    #[test]
    fn an_all_control_announce_name_leaves_the_address_readable() {
        // The extreme of the same mechanism: a name of nothing but control
        // characters drew no glyph yet claimed one column per character, blanking
        // the title and squeezing the address until its path was gone. Such an
        // announce carries no name at all, so the frame falls back to the short
        // dest hex and the address stays whole.
        let hostile = vec![0x7f_u8; 60];
        let mut reg = NomadNodeRegistry::new();
        assert!(reg.observe_parts(
            &crate::discovery::nomad_node_name_hash(),
            [0xab; 16],
            &hostile,
            Some(1),
            10,
        ));
        let name = reg.get_by_hash(&[0xab; 16]).and_then(|n| n.name.clone());
        assert_eq!(name, None, "a name of pure controls is no name");

        let title = crate::browser::page_title(name.as_deref(), &[0xab; 16], "/page/171.mu");
        let mut m = Model::from_document(&parse(SAMPLE), content_width(80), title, (80, 24));
        m.history.visit(tgt(0xab));
        m.current_dest = Some([0xab; 16]);
        let buffer = render(&m, 80, 24);
        let top = row_text(&buffer, 0, 80);
        assert!(
            top.starts_with("abababab…"),
            "want the short dest hex as the title: {top:?}"
        );
        assert!(
            top.contains(":/page/171.mu"),
            "address path missing: {top:?}"
        );
    }

    #[test]
    fn status_bar_renders_hints() {
        let m = loaded_model((80, 24));
        let buffer = render(&m, 80, 24);
        let status = row_text(&buffer, 23, 80);
        assert!(status.contains("quit"), "hints missing: {status:?}");
    }

    #[test]
    fn loading_state_renders_spinner_and_text() {
        let mut m = loaded_model((80, 24));
        m.pending = Some(Pending {
            target: tgt(9),
            action: HistoryAction::Push,
        });
        let buffer = render(&m, 80, 24);
        // The loading state now floats in the bottom-left chip on the content's
        // last row (row 22), not the footer row.
        let chip = row_text(&buffer, 22, 80);
        assert!(chip.contains("Loading"), "loading missing: {chip:?}");
    }

    #[test]
    fn loading_status_bar_has_rainbow_bold_spinner_cell() {
        let mut m = loaded_model((80, 24));
        m.pending = Some(Pending {
            target: tgt(9),
            action: HistoryAction::Push,
        });
        m.spin = 3;
        let hue = (m.spin as f32 * HUE_STEP).rem_euclid(360.0);
        let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
        let buffer = render(&m, 80, 24);
        // The spinner now lives in the bottom-left chip on the content's last row.
        let mut found = false;
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, 22)];
            if cell.modifier.contains(Modifier::BOLD) && cell.fg == Color::Rgb(r, g, b) {
                found = true;
            }
        }
        assert!(found, "no bold rainbow spinner cell in the loading chip");
    }

    #[test]
    fn error_toast_renders_over_content_and_status_keeps_hints() {
        let mut m = loaded_model((80, 24));
        m.set_toast(ToastKind::Error, "no path to destination");
        let buffer = render(&m, 80, 24);

        // The toast text and its warning glyph render somewhere in the content.
        let all = flat(&buffer);
        assert!(all.contains("no path"), "toast text missing:\n{all}");
        assert!(all.contains('⚠'), "error glyph missing:\n{all}");

        // The toast token fills the box and the error token draws its border
        // (leviculum_ui::tokens, the dark truecolor table).
        let mut found_error_style = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                if cell.fg == Color::Rgb(0xff, 0x6b, 0x6b)
                    && cell.bg == Color::Rgb(0x3a, 0x3f, 0x4a)
                {
                    found_error_style = true;
                }
            }
        }
        assert!(found_error_style, "error style not painted");

        // The status bar (row 23) still shows the key-hints, not the message.
        let status = row_text(&buffer, 23, 80);
        assert!(status.contains("quit"), "hints missing: {status:?}");
        assert!(
            !status.contains("no path"),
            "message leaked into status bar"
        );
    }

    #[test]
    fn set_toast_stores_kind_and_text() {
        let mut m = Model::default();
        m.set_toast(ToastKind::Info, "copied aa11:/page/x.mu");
        let toast = m.toast.as_ref().expect("toast stored");
        assert_eq!(toast.kind, ToastKind::Info);
        assert_eq!(toast.text, "copied aa11:/page/x.mu");
        assert_eq!(toast.shown_at, m.now_tick);
    }

    #[test]
    fn toast_expires_after_the_timeout() {
        let mut m = Model::default();
        m.set_toast(ToastKind::Info, "copied");
        // Just short of the lifetime it is still up.
        for _ in 0..TOAST_TICKS - 1 {
            update(&mut m, AppEvent::Tick);
        }
        assert!(m.toast.is_some(), "toast dismissed too early");
        // One more tick crosses the lifetime and clears it.
        update(&mut m, AppEvent::Tick);
        assert!(m.toast.is_none(), "toast should have expired");
    }

    #[test]
    fn any_key_dismisses_the_toast() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        m.set_toast(ToastKind::Info, "bookmarked");
        // A plain scroll key is a navigation event: it clears the toast.
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(m.toast.is_none(), "a key event should dismiss the toast");
    }

    #[test]
    fn needs_tick_tracks_loading_and_toast() {
        let mut m = Model::default();
        assert!(!m.needs_tick(), "idle model needs no tick");
        m.set_toast(ToastKind::Info, "up");
        assert!(m.needs_tick(), "an active toast must drive the tick");
    }

    #[test]
    fn bad_link_follow_sets_error_toast_with_url_not_status_bar() {
        // A same-destination link with no current destination fails to resolve.
        let mut m = loaded_model((80, 24));
        m.current_dest = None;
        m.links = vec![RenderedLink {
            index: 1,
            label: "Broken".to_string(),
            target: ":/page/broken.mu".to_string(),
            ..RenderedLink::default()
        }];
        let effects = follow_link(&mut m, 1);
        assert!(effects.is_empty(), "a broken link must not navigate");
        let toast = m.toast.as_ref().expect("a bad-link toast is raised");
        assert_eq!(toast.kind, ToastKind::Error);
        assert!(
            toast.text.contains(":/page/broken.mu"),
            "toast must carry the offending url: {}",
            toast.text
        );

        // The offending url shows in the toast overlay, never in the status bar.
        let buffer = render(&m, 80, 24);
        let status = row_text(&buffer, 23, 80);
        assert!(
            !status.contains(":/page/broken.mu"),
            "the url leaked into the status bar: {status:?}"
        );
        assert!(
            status.contains("quit"),
            "status bar lost its hints: {status:?}"
        );
    }

    #[test]
    fn view_renders_heading_and_underlined_link_label() {
        let model = model_from_sample(content_width(80), (80, 24));
        let buffer = render(&model, 80, 24);
        let text = flat(&buffer);
        assert!(text.contains("Sample Page"), "heading missing:\n{text}");
        assert!(text.contains("Alpha"), "link label missing:\n{text}");

        let link_fg = Color::Rgb(0, 175, 255);
        let mut found_styled_link = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "A"
                    && cell.fg == link_fg
                    && cell.modifier.contains(Modifier::UNDERLINED)
                {
                    found_styled_link = true;
                }
            }
        }
        assert!(
            found_styled_link,
            "no underlined LINK_FG 'A' cell found:\n{text}"
        );
    }

    /// Find the fg colour of the underlined link-label 'A' cell in a rendered
    /// buffer, if any.
    fn link_label_fg(buffer: &ratatui::buffer::Buffer) -> Option<Color> {
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "A" && cell.modifier.contains(Modifier::UNDERLINED) {
                    return Some(cell.fg);
                }
            }
        }
        None
    }

    #[test]
    fn ansi256_depth_downgrades_link_colour_to_indexed() {
        let mut model = model_from_sample(content_width(80), (80, 24));
        model.depth = ColorDepth::Ansi256;
        let buffer = render(&model, 80, 24);
        // The link label keeps its colour, but as the nearest palette index, not
        // a 24-bit Rgb triple.
        let expect = Color::Indexed(rgb_to_ansi256(0, 175, 255));
        assert_eq!(
            link_label_fg(&buffer),
            Some(expect),
            "Ansi256 render should downgrade the link fg to an indexed colour"
        );
    }

    #[test]
    fn truecolor_depth_keeps_link_colour_as_rgb() {
        let mut model = model_from_sample(content_width(80), (80, 24));
        model.depth = ColorDepth::Truecolor;
        let buffer = render(&model, 80, 24);
        assert_eq!(
            link_label_fg(&buffer),
            Some(Color::Rgb(0, 175, 255)),
            "Truecolor render should keep the link fg as a 24-bit Rgb triple"
        );
    }

    #[test]
    fn no_color_suppresses_the_ansi256_downgrade() {
        // NO_COLOR still wins: even with an Ansi256 depth, the downgrade post-pass
        // is skipped so the reverse-video chrome path is untouched.
        let mut model = model_from_sample(content_width(80), (80, 24));
        model.no_color = true;
        model.depth = ColorDepth::Ansi256;
        let buffer = render(&model, 80, 24);
        // No cell carries an Indexed colour (the downgrade never ran).
        let has_indexed = (0..buffer.area.height).any(|y| {
            (0..buffer.area.width).any(|x| {
                let cell = &buffer[(x, y)];
                matches!(cell.fg, Color::Indexed(_)) || matches!(cell.bg, Color::Indexed(_))
            })
        });
        assert!(!has_indexed, "no_color must suppress the Ansi256 downgrade");
    }

    #[test]
    fn light_theme_view_uses_light_link_and_heading_band() {
        let mut model = model_from_sample(content_width(80), (80, 24));
        model.toggle_theme();
        assert_eq!(model.theme, Theme::Light);
        let buffer = render(&model, 80, 24);

        // The link label 'A' now carries the light (deep-blue) link colour.
        let light_link = Color::Rgb(0, 90, 170);
        let mut found_light_link = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "A"
                    && cell.fg == light_link
                    && cell.modifier.contains(Modifier::UNDERLINED)
                {
                    found_light_link = true;
                }
            }
        }
        assert!(found_light_link, "no light-blue underlined link cell found");

        // The depth-1 "Sample Page" heading carries the light band bg #777777
        // (the dark theme would band it #bbbbbb instead).
        let light_band_bg = Color::Rgb(0x77, 0x77, 0x77);
        let has_light_band = (0..buffer.area.height)
            .any(|y| (0..buffer.area.width).any(|x| buffer[(x, y)].bg == light_band_bg));
        assert!(has_light_band, "light heading band not found");
        // The dark depth-1 band must not appear under the light theme.
        let dark_band_bg = Color::Rgb(0xbb, 0xbb, 0xbb);
        let has_dark_band = (0..buffer.area.height)
            .any(|y| (0..buffer.area.width).any(|x| buffer[(x, y)].bg == dark_band_bg));
        assert!(!has_dark_band, "dark heading band leaked into light theme");
    }

    #[test]
    fn t_toggles_theme_and_view_reflects_it() {
        let mut m = loaded_model((80, 24));
        assert_eq!(m.theme, Theme::Dark);

        // `t` flips the model to the light theme...
        let effects = press(&mut m, KeyCode::Char('t'), KeyModifiers::NONE);
        assert!(effects.is_empty());
        assert_eq!(m.theme, Theme::Light);
        let light_bg = rgb(Theme::Light.chrome_bg());
        let buffer = render(&m, 80, 24);
        assert_eq!(
            buffer[(0, 0)].bg,
            light_bg,
            "view must reflect the light chrome after toggling"
        );

        // ...and `t` again flips back to dark.
        press(&mut m, KeyCode::Char('t'), KeyModifiers::NONE);
        assert_eq!(m.theme, Theme::Dark);
        let dark_bg = rgb(Theme::Dark.chrome_bg());
        let buffer = render(&m, 80, 24);
        assert_eq!(
            buffer[(0, 0)].bg,
            dark_bg,
            "view must return to dark chrome"
        );
    }

    #[test]
    fn view_scrolls_slice_below_topbar_and_draws_scrollbar() {
        let (w, h) = (40u16, 13u16);
        let mut m = Model::from_document(&long_doc(), content_width(w), "Long", (w, h));
        assert!(
            m.page.len() > m.viewport(),
            "fixture must exceed the viewport"
        );
        let content_top = TOPBAR_ROWS; // content begins after the two-row top-bar

        let buffer = render(&m, w, h);
        let top0 = row_text(&buffer, content_top, w - SCROLLBAR_COLS);
        assert_eq!(top0.trim_end(), line_text(&m.page[0]).trim_end());

        let vp = m.viewport();
        press(&mut m, KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!(m.scroll, vp);
        let buffer = render(&m, w, h);
        let top1 = row_text(&buffer, content_top, w - SCROLLBAR_COLS);
        assert_eq!(top1.trim_end(), line_text(&m.page[vp]).trim_end());

        // The scrollbar occupies the reserved right-hand column of the content.
        let mut scrollbar_cell = false;
        for y in content_top..h {
            if buffer[(w - SCROLLBAR_COLS, y)].symbol() != " " {
                scrollbar_cell = true;
            }
        }
        assert!(scrollbar_cell, "scrollbar column should not be empty");
    }

    #[test]
    fn scrollbar_thumb_reaches_top_and_bottom() {
        // A page much taller than the viewport, so the thumb is a small handle
        // that must travel the whole track as `scroll` runs `0..=max_scroll`.
        let (w, h) = (40u16, 13u16);
        let mut m = Model::from_document(&long_doc(), content_width(w), "Long", (w, h));
        assert!(
            m.page.len() > m.viewport() * 2,
            "fixture must be much taller than the viewport"
        );

        let col = w - SCROLLBAR_COLS; // rightmost column carries the scrollbar
        let content_top = TOPBAR_ROWS; // first content row (below the two-row top-bar)
        let content_bottom = h - STATUS_ROWS - 1; // last content row (above the status bar)

        // The rows in the scrollbar column occupied by the thumb glyph (block).
        let thumb_rows = |buf: &ratatui::buffer::Buffer| -> Vec<u16> {
            (content_top..=content_bottom)
                .filter(|&y| buf[(col, y)].symbol() == "█")
                .collect()
        };

        // scroll = 0: the thumb sits at the TOP of the track, not the bottom.
        m.scroll = 0;
        let buf = render(&m, w, h);
        let top = thumb_rows(&buf);
        assert!(!top.is_empty(), "thumb should be visible at scroll 0");
        assert_eq!(
            *top.first().unwrap(),
            content_top,
            "thumb top must hit the track top at scroll 0"
        );
        assert!(
            *top.last().unwrap() < content_bottom,
            "thumb must not fill to the bottom at scroll 0"
        );

        // scroll = max_scroll: the thumb BOTTOM reaches the bottom row of the track.
        let vp = m.viewport();
        m.scroll = m.max_scroll(vp);
        assert!(m.scroll > 0, "max_scroll must be positive for a tall page");
        let buf = render(&m, w, h);
        let bottom = thumb_rows(&buf);
        assert!(!bottom.is_empty(), "thumb should be visible at max scroll");
        assert_eq!(
            *bottom.last().unwrap(),
            content_bottom,
            "thumb bottom must reach the track bottom at max scroll"
        );
        assert!(
            *bottom.first().unwrap() > content_top,
            "thumb must have left the top at max scroll"
        );

        // A mid value puts the thumb strictly between the two extremes.
        m.scroll = m.max_scroll(vp) / 2;
        let buf = render(&m, w, h);
        let mid = thumb_rows(&buf);
        assert!(!mid.is_empty(), "thumb should be visible mid-scroll");
        assert!(
            *mid.first().unwrap() > content_top && *mid.last().unwrap() < content_bottom,
            "mid-scroll thumb must sit between the track ends: {mid:?}"
        );
    }

    #[test]
    fn chrome_bars_carry_background_full_width() {
        let m = loaded_model((80, 24));
        assert!(!m.no_color, "colour is on by default");
        assert_eq!(m.theme, Theme::Dark, "dark is the default theme");
        let dark_bg = rgb(Theme::Dark.chrome_bg());
        let buffer = render(&m, 80, 24);
        // The one-row top bar (0) and the status row (23) carry the chrome
        // background across the full width, including the rightmost column.
        for &y in &[0u16, 23] {
            for &x in &[0u16, 40, 79] {
                assert_eq!(
                    buffer[(x, y)].bg,
                    dark_bg,
                    "cell ({x},{y}) is missing the chrome background"
                );
            }
        }
    }

    #[test]
    fn chrome_bars_carry_light_background_under_light_theme() {
        let mut m = loaded_model((80, 24));
        m.toggle_theme();
        assert_eq!(m.theme, Theme::Light);
        let light_bg = rgb(Theme::Light.chrome_bg());
        let buffer = render(&m, 80, 24);
        for &y in &[0u16, 23] {
            for &x in &[0u16, 40, 79] {
                assert_eq!(
                    buffer[(x, y)].bg,
                    light_bg,
                    "cell ({x},{y}) is missing the light chrome background"
                );
            }
        }
    }

    #[test]
    fn chrome_bars_use_reverse_under_no_color() {
        let dark_bg = rgb(Theme::Dark.chrome_bg());
        for theme in [Theme::Dark, Theme::Light] {
            let mut m = loaded_model((80, 24));
            m.no_color = true;
            m.theme = theme;
            m.relayout(m.width);
            let buffer = render(&m, 80, 24);
            for &y in &[0u16, 23] {
                for &x in &[0u16, 40, 79] {
                    let cell = &buffer[(x, y)];
                    assert!(
                        cell.modifier.contains(Modifier::REVERSED),
                        "cell ({x},{y}) must be reversed under no_color ({theme:?})"
                    );
                    assert_ne!(
                        cell.bg, dark_bg,
                        "no_color must not paint the colour background ({theme:?})"
                    );
                }
            }
        }
    }

    /// The laid-out footer slot whose button carries `action`.
    fn footer_slot(m: &Model, action: FooterAction) -> FooterSlot {
        let footer = regions(m.frame_area())[2];
        footer_layout(m, footer)
            .into_iter()
            .find(|s| s.item.action == action)
            .expect("footer slot")
    }

    #[test]
    fn footer_key_is_bright_bold_and_label_is_muted() {
        // The grouping is carried by contrast, not spacing: the key cell renders
        // in the bright chrome fg and BOLD (never DIM); the label cell in the
        // muted chrome fg; and the two clearly differ. Checked under both themes.
        for theme in [Theme::Dark, Theme::Light] {
            let mut m = loaded_model((80, 24));
            if theme == Theme::Light {
                m.toggle_theme();
            }
            assert_eq!(m.theme, theme);
            // The `q quit` button: its key is available, so it is the active case.
            let slot = footer_slot(&m, FooterAction::Quit);
            let buffer = render(&m, 80, 24);
            let key = &buffer[(slot.rect.x, 23u16)];
            assert_eq!(
                key.fg,
                rgb(theme.chrome_fg()),
                "footer key must use the bright chrome fg ({theme:?})"
            );
            assert!(
                key.modifier.contains(Modifier::BOLD),
                "footer key must be BOLD ({theme:?})"
            );
            assert!(
                !key.modifier.contains(Modifier::DIM),
                "footer key must not be DIM ({theme:?})"
            );
            // The label ("quit") starts two columns past the key ("q" + a space).
            let label = &buffer[(slot.rect.x + 2, 23u16)];
            assert_eq!(
                label.fg,
                rgb(theme.chrome_muted_fg()),
                "footer label must use the muted chrome fg ({theme:?})"
            );
            assert_ne!(key.fg, label.fg, "key and label must differ ({theme:?})");
        }
    }

    /// The core splits the frame into top bar, content and status bar on the
    /// same cells as the ratatui `Layout` the drawing code used before the cut
    /// (#219), down to frames too short to hold all three.
    #[test]
    fn the_core_region_split_matches_the_ratatui_layout() {
        use ratatui::layout::{Constraint, Layout};
        for height in 0..=40 {
            for (x, y, width) in [(0, 0, 80), (3, 2, 10), (0, 0, 0)] {
                let area = ratatui::layout::Rect::new(x, y, width, height);
                let parts = Layout::vertical([
                    Constraint::Length(TOPBAR_ROWS),
                    Constraint::Min(0),
                    Constraint::Length(STATUS_ROWS),
                ])
                .split(area);
                let core = crate::app::geometry::regions(from_ratatui(area)).map(to_ratatui);
                assert_eq!(core, [parts[0], parts[1], parts[2]], "{area:?}");
            }
        }
    }

    /// A terminal tall enough for the whole help: since the help lists
    /// every chord of the key table (slice 1b), it no longer fits 40 rows.
    const HELP_ROWS: u16 = 50;

    #[test]
    fn help_overlay_renders_keybindings() {
        let mut m = loaded_model((120, HELP_ROWS));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(m.show_help);
        let buffer = render(&m, 120, HELP_ROWS);
        let text = flat(&buffer);
        assert!(text.contains("help"), "help title missing:\n{text}");
        assert!(text.contains("quit"), "help body missing:\n{text}");
        // Esc dismisses it.
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.show_help);
    }

    #[test]
    fn help_overlay_widens_to_the_longest_line() {
        let mut m = loaded_model((120, 40));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let buffer = render(&m, 120, 40);
        let text = flat(&buffer);
        // A wide row renders contiguously rather than wrapping onto two lines.
        assert!(
            text.lines()
                .any(|row| row.contains("hint — type a label or the link text")),
            "widest help line wrapped:\n{text}"
        );
        // The overlay is at least the widest line plus its two borders wide.
        let inner = help_lines()
            .iter()
            .map(|l| UnicodeWidthStr::width(l.as_str()))
            .max()
            .unwrap();
        let border = text
            .lines()
            .find(|row| row.contains('┌'))
            .expect("top border row");
        let chars: Vec<char> = border.chars().collect();
        let start = chars.iter().position(|&c| c == '┌').unwrap();
        let end = chars.iter().position(|&c| c == '┐').unwrap();
        let width = end - start + 1;
        assert!(
            width >= inner + 2,
            "overlay width {width} < longest line {inner} + 2",
        );
    }

    #[test]
    fn help_lists_every_scroll_binding() {
        // Every keys-string in the scroll keymap appears verbatim in the rendered
        // overlay: the table is the single source, so the help cannot omit a
        // binding the key handler honours.
        let mut m = loaded_model((120, 40));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let text = flat(&render(&m, 120, 40));
        for row in SCROLL_ROWS {
            assert!(
                text.lines().any(|l| l.contains(row.keys())),
                "scroll binding {:?} missing from help:\n{text}",
                row.keys()
            );
        }
    }

    #[test]
    fn help_descriptions_all_align() {
        // Every description starts at the same display column: the keys column is
        // computed, never hand-padded, so alignment is structural.
        let mut m = loaded_model((120, HELP_ROWS));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let text = flat(&render(&m, 120, HELP_ROWS));
        let rows: Vec<&str> = text.lines().collect();
        let mut cols = Vec::new();
        for group in help_groups() {
            for e in &group.entries {
                let row = rows
                    .iter()
                    .find(|l| l.contains(e.keys) && l.contains(e.desc))
                    .unwrap_or_else(|| panic!("help row for {:?} missing:\n{text}", e.keys));
                // Char count, not byte offset: the border and arrows are multibyte
                // but each occupies one display column.
                let byte = row.find(e.desc).expect("desc column");
                cols.push(row[..byte].chars().count());
            }
        }
        let first = cols[0];
        assert!(
            cols.iter().all(|&c| c == first),
            "descriptions misaligned: columns {cols:?}"
        );
    }

    #[test]
    fn help_lines_never_exceed_the_overlay_width() {
        // The overlay sizes itself to its widest line, so no content line wraps:
        // every built line renders intact on a single row.
        let mut m = loaded_model((120, HELP_ROWS));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let text = flat(&render(&m, 120, HELP_ROWS));
        for line in help_lines().iter().filter(|l| !l.is_empty()) {
            assert!(
                text.lines().any(|row| row.contains(line.as_str())),
                "help line wrapped or missing: {line:?}\n{text}"
            );
        }
    }

    #[test]
    fn address_mode_renders_editor() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char(':'), KeyModifiers::NONE);
        type_str(&mut m, "abc");
        let buffer = render(&m, 80, 24);
        let text = flat(&buffer);
        assert!(text.contains("abc"), "editor content missing:\n{text}");
    }

    // --- phase 4: visible_links, hints, focus, mouse --------------------

    /// A left-button mouse-down event at `(col, row)`.
    fn click(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// A mouse-move event at `(col, row)`.
    fn moved(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// The laid-out link with 1-based `index`.
    fn link_of(m: &Model, index: usize) -> &RenderedLink {
        m.links.iter().find(|l| l.index == index).expect("link")
    }

    #[test]
    fn visible_links_map_content_lines_to_screen_rects() {
        let m = model_from_sample(content_width(80), (80, 24));
        let vls = visible_links(&m);
        assert_eq!(vls.len(), 2, "sample has two links");
        for vl in &vls {
            let link = link_of(&m, vl.index);
            // row = top-bar rows + (content line - scroll); scroll is 0 here.
            assert_eq!(vl.row, TOPBAR_ROWS + link.line as u16);
            assert_eq!(vl.col_start, link.col_start as u16);
            assert_eq!(vl.col_end, link.col_end as u16);
        }
    }

    #[test]
    fn visible_links_exclude_off_viewport() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        // Scroll past both links (they sit near the top of the page).
        m.scroll = link_of(&m, 2).line + 1;
        assert!(
            visible_links(&m).is_empty(),
            "links above the scroll must be excluded"
        );
    }

    #[test]
    fn visible_links_partial_scroll_keeps_only_lower_link() {
        let mut m = model_from_sample(content_width(80), (80, 24));
        let beta_line = link_of(&m, 2).line;
        m.scroll = beta_line;
        let vls = visible_links(&m);
        assert_eq!(vls.len(), 1, "only the lower link stays visible");
        assert_eq!(vls[0].index, 2);
        // The now-topmost visible link sits at the content top row.
        assert_eq!(vls[0].row, TOPBAR_ROWS);
    }

    #[test]
    fn hint_labels_are_single_char_when_few() {
        let labels = hint_labels(3);
        assert_eq!(labels, vec!["a", "s", "d"]);
        // All nine of the alphabet fit as single characters.
        assert!(hint_labels(9).iter().all(|l| l.len() == 1));
        let nine = hint_labels(9);
        assert_eq!(nine.len(), 9);
        assert_eq!(
            nine.iter().collect::<std::collections::HashSet<_>>().len(),
            9
        );
    }

    #[test]
    fn hint_labels_are_two_char_when_many() {
        let labels = hint_labels(10);
        assert_eq!(labels.len(), 10);
        assert!(
            labels.iter().all(|l| l.chars().count() == 2),
            "got: {labels:?}"
        );
        assert_eq!(labels[0], "aa");
        // Unique across the whole set.
        let uniq: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(uniq.len(), 10);
    }

    #[test]
    fn hints_cover_visible_links_then_fields_then_controls_with_unique_labels() {
        let m = loaded_model((80, 24));
        let hs = hints(&m);
        // Two links + one form field + two top-bar controls (the address slot
        // and the star).
        assert_eq!(hs.len(), 5);
        assert!(matches!(hs[0].target, HintTarget::Link(1)));
        assert!(matches!(hs[1].target, HintTarget::Link(2)));
        assert!(matches!(hs[2].target, HintTarget::Field(1)));
        assert!(matches!(
            hs[3].target,
            HintTarget::Control(Control::Address)
        ));
        assert!(matches!(
            hs[4].target,
            HintTarget::Control(Control::Bookmark)
        ));
        let labels: std::collections::HashSet<_> = hs.iter().map(|h| &h.label).collect();
        assert_eq!(labels.len(), 5, "labels must be unique");
    }

    #[test]
    fn f_enters_hint_mode() {
        let mut m = loaded_model((80, 24));
        let effects = press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(effects.is_empty());
        assert_eq!(m.mode, Mode::Hint);
    }

    #[test]
    fn typing_a_unique_label_follows_that_link() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        // The first link's label is 'a'.
        let effects = press(&mut m, KeyCode::Char('a'), KeyModifiers::NONE);
        let expected = browser::resolve_link(link_of(&m, 1), m.current_dest)
            .unwrap()
            .0;
        assert_eq!(effects, vec![Effect::Navigate(expected.clone())]);
        assert_eq!(m.mode, Mode::Browse, "hint mode exits after a match");
        assert_eq!(m.pending.as_ref().map(|p| &p.target), Some(&expected));
    }

    #[test]
    fn typing_link_text_follows_that_link() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        // 'b' matches no label but uniquely matches the "• Beta" link text.
        let effects = press(&mut m, KeyCode::Char('b'), KeyModifiers::NONE);
        let expected = browser::resolve_link(link_of(&m, 2), m.current_dest)
            .unwrap()
            .0;
        assert_eq!(effects, vec![Effect::Navigate(expected)]);
        assert_eq!(m.mode, Mode::Browse);
    }

    #[test]
    fn non_matching_hint_key_is_ignored() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        // 'z' is neither a label prefix nor in any link's text.
        let effects = press(&mut m, KeyCode::Char('z'), KeyModifiers::NONE);
        assert!(effects.is_empty());
        assert_eq!(m.mode, Mode::Hint, "still hinting");
        assert!(m.hint_input.is_empty(), "invalid key not accepted");
    }

    #[test]
    fn esc_exits_hint_mode() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(m.mode, Mode::Browse);
    }

    #[test]
    fn tab_cycles_focus_and_wraps() {
        // The sample page has two links then one text field, in document order.
        let mut m = loaded_model((80, 24));
        assert_eq!(m.focus, None);
        assert_eq!(m.field_focus, None);
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(1));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(2));
        // The third element is the text field: link focus clears, field focus
        // takes over, and the mode switches to field editing.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, None);
        assert_eq!(m.field_focus, Some(1));
        assert_eq!(m.mode, Mode::Field);
        // Wrap back to the first link (and back to browse mode).
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(1));
        assert_eq!(m.field_focus, None);
        assert_eq!(m.mode, Mode::Browse);
        // Shift-Tab goes backward from the first link, wrapping to the field.
        press(&mut m, KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(m.field_focus, Some(1));
        assert_eq!(m.mode, Mode::Field);
    }

    #[test]
    fn tab_sets_status_target() {
        let m = loaded_model((80, 24));
        let mut m2 = m.clone();
        press(&mut m2, KeyCode::Tab, KeyModifiers::NONE);
        let target = focused_link_target(&m2).expect("focused target");
        assert!(
            target.contains(&link_of(&m, 1).target),
            "status target missing the link path: {target:?}"
        );
    }

    #[test]
    fn enter_follows_focused_link() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        let effects = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        let expected = browser::resolve_link(link_of(&m, 1), m.current_dest)
            .unwrap()
            .0;
        assert_eq!(effects, vec![Effect::Navigate(expected)]);
    }

    #[test]
    fn following_external_https_link_opens_externally() {
        let mut m = Model {
            links: vec![RenderedLink {
                index: 1,
                label: "Site".to_string(),
                target: "https://example.org/x".to_string(),
                ..RenderedLink::default()
            }],
            ..Model::default()
        };
        let effects = follow_link(&mut m, 1);
        assert_eq!(
            effects,
            vec![Effect::OpenExternal("https://example.org/x".to_string())]
        );
        assert!(
            m.pending.is_none(),
            "an external link must not start a fetch"
        );
    }

    #[test]
    fn following_unsafe_scheme_link_refuses_with_toast() {
        let mut m = Model {
            links: vec![RenderedLink {
                index: 1,
                label: "Bad".to_string(),
                target: "file:///etc/passwd".to_string(),
                ..RenderedLink::default()
            }],
            ..Model::default()
        };
        let effects = follow_link(&mut m, 1);
        assert!(effects.is_empty(), "an unsafe link must yield no effect");
        assert!(m.pending.is_none(), "an unsafe link must not navigate");
        let toast = m.toast.as_ref().expect("a refusal toast is raised");
        assert_eq!(toast.kind, ToastKind::Error);
        assert!(
            toast.text.contains("file"),
            "toast names the refused scheme: {}",
            toast.text
        );
    }

    #[test]
    fn a_hosted_browser_hands_an_lxmf_link_to_its_messenger() {
        // In Loquax's Browse tab a page's "message the operator" link opens
        // the chat: the browser asks the host to write, copies nothing and
        // fetches nothing. Without `hosted` the same link is copied (the
        // test below), which is the control.
        let hash = "0ec84236630cea839d80a71c39fb41ce";
        let mut m = Model {
            links: vec![RenderedLink {
                index: 1,
                label: "Lew Palm".to_string(),
                target: format!("lxmf@{hash}"),
                ..RenderedLink::default()
            }],
            hosted: true,
            ..Model::default()
        };
        assert_eq!(
            follow_link(&mut m, 1),
            vec![Effect::Write(hash.to_string())]
        );
        assert!(m.pending.is_none(), "an address must not start a fetch");
        assert!(m.toast.is_none(), "the chat opening says it all");
    }

    #[test]
    fn following_lxmf_link_copies_the_address_instead_of_failing() {
        // Before this was handled, the target reached parse_url and produced
        // "bad link: ... (malformed URL)", which reads as a broken page
        // rather than an action this browser does not perform.
        let hash = "0ec84236630cea839d80a71c39fb41ce";
        let mut m = Model {
            links: vec![RenderedLink {
                index: 1,
                label: "Lew Palm".to_string(),
                target: format!("lxmf@{hash}"),
                ..RenderedLink::default()
            }],
            ..Model::default()
        };
        let effects = follow_link(&mut m, 1);
        assert_eq!(effects, vec![Effect::Copy(hash.to_string())]);
        assert!(m.pending.is_none(), "an address must not start a fetch");
        let toast = m.toast.as_ref().expect("a toast reports what happened");
        assert_eq!(toast.kind, ToastKind::Info, "this is not an error");
        assert!(
            toast.text.contains(hash),
            "the address is shown, not just copied: {}",
            toast.text
        );
    }

    /// A page that links to its own node with a bare `/page/x.mu` dropped the
    /// `:` a local URL keeps. The link is dead here exactly as it is in the
    /// reference (see `url::tests::reference_rejects_the_same_urls_we_do`), so
    /// the toast's job is to hand over the string that would have worked rather
    /// than to restate the grammar.
    #[test]
    fn link_missing_the_local_colon_is_told_which_colon() {
        let mut m = loaded_model((80, 24));
        m.links = vec![RenderedLink {
            index: 1,
            label: "About".to_string(),
            target: "/page/about.md".to_string(),
            ..RenderedLink::default()
        }];

        let effects = follow_link(&mut m, 1);
        assert!(effects.is_empty(), "a dead link must not navigate");
        assert!(m.pending.is_none());
        let toast = m.toast.as_ref().expect("an error toast is raised");
        assert_eq!(toast.kind, ToastKind::Error);
        assert!(
            toast.text.contains(":/page/about.md"),
            "the toast must show the URL that works: {}",
            toast.text
        );
        // The suggestion must survive the single truncated toast line at 80
        // columns: the whole note fits, and in any case leads with the fix.
        assert!(
            UnicodeWidthStr::width(toast.text.as_str()) <= 74,
            "toast too long to read at 80 columns: {}",
            toast.text
        );
        let suggestion = toast.text.find(":/page/about.md").expect("suggestion");
        assert!(
            suggestion < toast.text.find("keeps").expect("explanation"),
            "the fix must come before the explanation: {}",
            toast.text
        );
    }

    /// A local URL typed with nothing open — reachable from the start screen,
    /// where the URL editor is one keypress away and no page is loaded.
    #[test]
    fn local_url_with_no_open_page_says_so() {
        let mut m = Model {
            size: (80, 24),
            ..Model::default()
        };
        m.relayout(content_width(80));
        show_start_screen(&mut m);
        m.show_places = false;

        press(&mut m, KeyCode::Char(':'), KeyModifiers::NONE);
        type_str(&mut m, ":/page/x.mu");
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);

        let toast = m.toast.as_ref().expect("an error toast is raised");
        assert_eq!(toast.kind, ToastKind::Error);
        assert!(
            toast.text.contains("open page"),
            "must say what is missing: {}",
            toast.text
        );
    }

    #[test]
    fn following_rns_link_still_navigates() {
        let mut m = loaded_model((80, 24));
        let effects = follow_link(&mut m, 1);
        let expected = browser::resolve_link(link_of(&m, 1), m.current_dest)
            .unwrap()
            .0;
        assert_eq!(effects, vec![Effect::Navigate(expected)]);
    }

    #[test]
    fn following_file_link_downloads_instead_of_navigating() {
        let mut m = loaded_model((80, 24));
        m.links = vec![RenderedLink {
            index: 1,
            label: "Manual".to_string(),
            target: ":/file/docs/manual.pdf".to_string(),
            ..RenderedLink::default()
        }];
        let history_before = m.history.clone();
        let effects = follow_link(&mut m, 1);
        let expected = Target {
            dest_hash: [0xab; 16],
            path: "/file/docs/manual.pdf".to_string(),
            fields: Vec::new(),
            is_file: true,
        };
        assert_eq!(effects, vec![Effect::Download(expected)]);
        assert!(
            m.pending.is_none(),
            "a download must not start a page fetch"
        );
        assert_eq!(
            m.history, history_before,
            "a download must not change history"
        );
        assert_eq!(m.download.as_deref(), Some("manual.pdf"));
        assert!(m.is_loading(), "a download shows the loading indicator");
    }

    #[test]
    fn download_done_clears_the_flight_and_raises_info_toast() {
        let mut m = loaded_model((80, 24));
        m.download = Some("manual.pdf".to_string());
        let effects = update(
            &mut m,
            AppEvent::DownloadDone {
                name: "manual.pdf".to_string(),
                bytes: 1234,
                dir: "/home/x/Downloads".to_string(),
            },
        );
        assert!(effects.is_empty());
        assert!(m.download.is_none(), "the flight must be cleared");
        let toast = m.toast.as_ref().expect("a completion toast is raised");
        assert_eq!(toast.kind, ToastKind::Info);
        assert_eq!(
            toast.text,
            "downloaded manual.pdf (1234 bytes) to /home/x/Downloads"
        );
    }

    #[test]
    fn download_failed_raises_error_toast() {
        let mut m = loaded_model((80, 24));
        m.download = Some("manual.pdf".to_string());
        let effects = update(
            &mut m,
            AppEvent::DownloadFailed(StrText::Raw("request timed out".to_string())),
        );
        assert!(effects.is_empty());
        assert!(m.download.is_none(), "the flight must be cleared");
        let toast = m.toast.as_ref().expect("a failure toast is raised");
        assert_eq!(toast.kind, ToastKind::Error);
        assert_eq!(toast.text, "request timed out");
    }

    #[test]
    fn esc_cancels_an_inflight_download() {
        let mut m = loaded_model((80, 24));
        m.download = Some("manual.pdf".to_string());
        let effects = press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(effects, vec![Effect::Cancel]);
        assert!(m.download.is_none(), "Esc must clear the download flight");
    }

    #[test]
    fn navigation_supersedes_an_inflight_download() {
        let mut m = loaded_model((80, 24));
        m.download = Some("manual.pdf".to_string());
        let effects = begin_navigation(&mut m, tgt(2), HistoryAction::Push, None);
        assert_eq!(effects, vec![Effect::Navigate(tgt(2))]);
        assert!(
            m.download.is_none(),
            "a fresh navigation supersedes the download flight"
        );
    }

    #[test]
    fn focus_auto_scrolls_to_off_screen_link() {
        // A link on content line 50 with a 21-row viewport must scroll into view.
        let mut m = Model {
            page: vec![RLine::default(); 60],
            links: vec![RenderedLink {
                index: 1,
                label: "Deep".to_string(),
                target: ":/page/deep.mu".to_string(),
                line: 50,
                col_start: 0,
                col_end: 4,
                ..RenderedLink::default()
            }],
            size: (80, 24),
            ..Model::default()
        };
        assert_eq!(m.viewport(), 22);
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.focus, Some(1));
        // scroll = line + 1 - viewport = 50 + 1 - 22 = 29.
        assert_eq!(m.scroll, 29);
    }

    #[test]
    fn mouse_click_on_link_navigates() {
        let mut m = loaded_model((80, 24));
        let vl = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        let effects = update(&mut m, AppEvent::Mouse(click(vl.col_start, vl.row)));
        let expected = browser::resolve_link(link_of(&m, 1), m.current_dest)
            .unwrap()
            .0;
        assert_eq!(effects, vec![Effect::Navigate(expected)]);
    }

    #[test]
    fn mouse_click_off_a_link_does_nothing() {
        let mut m = loaded_model((80, 24));
        // Row well below any link, in the content body.
        let effects = update(&mut m, AppEvent::Mouse(click(0, 20)));
        assert!(effects.is_empty());
        assert!(m.pending.is_none());
    }

    #[test]
    fn mouse_click_on_address_enters_address_mode() {
        let mut m = loaded_model((80, 24));
        let cr = top_bar_controls(&m, regions(m.frame_area())[0])
            .into_iter()
            .find(|c| c.control == Control::Address)
            .expect("address control");
        let effects = update(&mut m, AppEvent::Mouse(click(cr.rect.x, cr.rect.y)));
        assert!(effects.is_empty());
        assert_eq!(m.mode, Mode::Address);
    }

    #[test]
    fn mouse_click_on_star_toggles_bookmark() {
        let mut m = loaded_model((80, 24));
        let url = current_url(&m).expect("a page is loaded");
        assert!(!m.bookmarks.contains(&url), "not bookmarked to start");
        let cr = top_bar_controls(&m, regions(m.frame_area())[0])
            .into_iter()
            .find(|c| c.control == Control::Bookmark)
            .expect("bookmark star");
        let effects = update(&mut m, AppEvent::Mouse(click(cr.rect.x, cr.rect.y)));
        assert_eq!(effects, vec![Effect::SaveBookmarks]);
        assert!(m.bookmarks.contains(&url), "star click added the bookmark");
        // A second click removes it again.
        let cr = top_bar_controls(&m, regions(m.frame_area())[0])
            .into_iter()
            .find(|c| c.control == Control::Bookmark)
            .expect("bookmark star");
        update(&mut m, AppEvent::Mouse(click(cr.rect.x, cr.rect.y)));
        assert!(
            !m.bookmarks.contains(&url),
            "star click removed the bookmark"
        );
    }

    #[test]
    fn star_glyph_reflects_bookmark_state() {
        let mut m = loaded_model((80, 24));
        assert_eq!(star_glyph(&m), Some(STAR_OFF));
        toggle_bookmark_current(&mut m);
        assert_eq!(star_glyph(&m), Some(STAR_ON));
    }

    #[test]
    fn hops_cluster_reads_registry_or_unknown() {
        let mut m = loaded_model((80, 24));
        // Nothing announced for this dest yet: unknown.
        assert_eq!(hops_text(&m).as_deref(), Some("unknown"));
        // Once the dest is a known announced node with a hop count, N hops.
        let mut node = disc_node(0xab, "Node-AB");
        node.hops = Some(3);
        m.node_registry.upsert_node(&node);
        assert_eq!(hops_text(&m).as_deref(), Some("3 hops"));
        let top = row_text(&render(&m, 80, 24), 0, 80);
        assert!(top.contains("3 hops"), "hop count not rendered: {top:?}");
    }

    #[test]
    fn narrow_topbar_ellipsizes_address_without_overflow() {
        let m = loaded_model((30, 24));
        let buffer = render(&m, 30, 24);
        let top = row_text(&buffer, 0, 30);
        // The row is at most the terminal width, never more.
        assert!(UnicodeWidthStr::width(top.trim_end()) <= 30);
        // The address had to ellipsize at this width.
        assert!(
            top.contains('…'),
            "narrow address should ellipsize: {top:?}"
        );
    }

    #[test]
    fn rendered_buffer_has_no_marker_or_legend() {
        // A link-bearing page with no "Links:" content of its own.
        let m = Model::from_document(
            &parse("Intro paragraph\n\n`[Alpha`:/page/alpha.mu]"),
            content_width(80),
            "T",
            (80, 24),
        );
        let buffer = render(&m, 80, 24);
        let text = flat(&buffer);
        assert!(text.contains("Alpha"), "link label missing:\n{text}");
        assert!(!text.contains("[1]"), "visible [N] marker leaked:\n{text}");
        assert!(!text.contains("Links:"), "legend leaked:\n{text}");
    }

    #[test]
    fn hint_badges_render_over_links() {
        let mut m = loaded_model((80, 24));
        let vl = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        press(&mut m, KeyCode::Char('f'), KeyModifiers::NONE);
        let buffer = render(&m, 80, 24);
        // The first link wears the 'a' badge over its first cell.
        let cell = &buffer[(vl.col_start, vl.row)];
        assert_eq!(cell.symbol(), "a", "hint badge not drawn over the link");
        assert_eq!(cell.bg, Color::Rgb(255, 215, 0), "badge background missing");
    }

    #[test]
    fn mouse_move_over_link_sets_and_clears_hover() {
        let mut m = loaded_model((80, 24));
        let vl = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        update(&mut m, AppEvent::Mouse(moved(vl.col_start, vl.row)));
        assert_eq!(m.hover, Some(1), "moving over a link sets the hover");
        // A move off any link clears it.
        update(&mut m, AppEvent::Mouse(moved(vl.col_start, vl.row + 3)));
        assert_eq!(m.hover, None, "moving off a link clears the hover");
    }

    #[test]
    fn hovered_link_renders_highlighted_and_others_do_not() {
        let mut m = loaded_model((80, 24));
        let hovered = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        let other = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 2)
            .expect("beta visible");
        update(
            &mut m,
            AppEvent::Mouse(moved(hovered.col_start, hovered.row)),
        );
        let buffer = render(&m, 80, 24);
        let cell = &buffer[(hovered.col_start, hovered.row)];
        assert!(
            cell.modifier.contains(Modifier::BOLD) && cell.modifier.contains(Modifier::UNDERLINED),
            "hovered link not highlighted: {:?}",
            cell.modifier
        );
        // The non-hovered link keeps its plain link styling (no hover bold).
        let plain = &buffer[(other.col_start, other.row)];
        assert!(
            !plain.modifier.contains(Modifier::BOLD),
            "non-hovered link wrongly bolded: {:?}",
            plain.modifier
        );
    }

    #[test]
    fn focused_link_renders_highlighted() {
        let mut m = loaded_model((80, 24));
        let vl = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        let buffer = render(&m, 80, 24);
        let cell = &buffer[(vl.col_start, vl.row)];
        assert!(
            cell.modifier.contains(Modifier::REVERSED),
            "focused link not highlighted"
        );
    }

    #[test]
    fn focused_link_shows_chip_and_keeps_footer_buttons() {
        let mut m = loaded_model((80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        let buffer = render(&m, 80, 24);
        // The chip renders the focused link's address on the content's last row
        // (row 22), above the footer (row 23).
        let chip = row_text(&buffer, 22, 80);
        assert!(
            chip.contains(&link_of(&m, 1).target),
            "chip missing the focused target on the content's last row: {chip:?}"
        );
        assert!(
            chip.contains('→'),
            "chip missing its arrow marker: {chip:?}"
        );
        // The regression this batch prevents: the footer still draws its buttons.
        let status = row_text(&buffer, 23, 80);
        assert!(
            status.contains("back") && status.contains("quit"),
            "footer buttons covered by the focused link: {status:?}"
        );
    }

    #[test]
    fn hovered_link_shows_chip_and_keeps_footer_buttons() {
        let mut m = loaded_model((80, 24));
        let vl = visible_links(&m)[0];
        update_hover(&mut m, vl.col_start, vl.row);
        let buffer = render(&m, 80, 24);
        let chip = row_text(&buffer, 22, 80);
        assert!(
            chip.contains(&link_of(&m, vl.index).target),
            "chip missing the hovered target: {chip:?}"
        );
        let status = row_text(&buffer, 23, 80);
        assert!(
            status.contains("back") && status.contains("quit"),
            "footer buttons covered by the hovered link: {status:?}"
        );
    }

    #[test]
    fn loading_chip_shows_spinner_and_path_and_keeps_footer_buttons() {
        let mut m = loaded_model((80, 24));
        m.pending = Some(Pending {
            target: tgt(7),
            action: HistoryAction::Push,
        });
        let buffer = render(&m, 80, 24);
        let chip = row_text(&buffer, 22, 80);
        assert!(
            chip.contains("Loading") && chip.contains("/page/7.mu"),
            "loading chip missing spinner label / path: {chip:?}"
        );
        let spins: Vec<&str> = SPIN_FRAMES.to_vec();
        assert!(
            spins.iter().any(|g| chip.contains(g)),
            "loading chip missing the spinner glyph: {chip:?}"
        );
        let status = row_text(&buffer, 23, 80);
        assert!(
            status.contains("back") && status.contains("quit"),
            "footer buttons covered while loading: {status:?}"
        );
    }

    #[test]
    fn idle_draws_no_chip_and_leaves_the_content_row_untouched() {
        let m = loaded_model((80, 24));
        // No loading, no focus, no hover.
        assert!(m.pending.is_none() && m.focus.is_none() && m.hover.is_none());
        let buffer = render(&m, 80, 24);
        let chip = row_text(&buffer, 22, 80);
        assert!(!chip.contains('→'), "idle chip drew an arrow: {chip:?}");
        assert!(
            !chip.contains("Loading"),
            "idle chip drew a loading label: {chip:?}"
        );
    }

    #[test]
    fn loading_wins_over_a_hovered_link() {
        let mut m = loaded_model((80, 24));
        let vl = visible_links(&m)[0];
        update_hover(&mut m, vl.col_start, vl.row);
        m.pending = Some(Pending {
            target: tgt(7),
            action: HistoryAction::Push,
        });
        let buffer = render(&m, 80, 24);
        let chip = row_text(&buffer, 22, 80);
        assert!(
            chip.contains("Loading") && chip.contains("/page/7.mu"),
            "loading did not win over the hovered link: {chip:?}"
        );
        assert!(
            !chip.contains('→'),
            "the hovered link's arrow leaked while loading: {chip:?}"
        );
    }

    #[test]
    fn a_long_address_is_truncated_and_the_chip_never_exceeds_content_width() {
        // A narrow terminal forces truncation of a focused link's long target.
        let cols = 20u16;
        let mut m = loaded_model((cols, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        let target = focused_link_target(&m).expect("focused target");
        assert!(
            UnicodeWidthStr::width(target.as_str()) + 2 > cols as usize,
            "test needs a target wider than the terminal"
        );
        let buffer = render(&m, cols, 24);
        let chip = row_text(&buffer, 22, cols);
        // The whole row fits the terminal, so the field never spilled over.
        assert_eq!(chip.chars().count(), cols as usize);
        // The arrow and the node prefix survive; the truncated tail is gone.
        assert!(
            chip.contains('→'),
            "chip missing its arrow marker: {chip:?}"
        );
        assert!(
            !chip.contains("alpha.mu"),
            "long address was not truncated: {chip:?}"
        );
        assert!(chip.contains('…'), "no ellipsis after truncation: {chip:?}");
    }

    // --- phase 5: in-page search + #anchor scroll-to --------------------

    /// One laid-out line from plain text, every cell in the default style.
    fn rline(text: &str) -> RLine {
        RLine {
            cells: text
                .chars()
                .map(|ch| crate::render::StyledChar {
                    ch,
                    st: RStyle::default(),
                    link: None,
                    field: None,
                })
                .collect(),
        }
    }

    #[test]
    fn find_matches_multiple_across_lines_and_case_insensitive() {
        let page = vec![rline("Foo bar foo"), rline("nothing here"), rline("FOObar")];
        let ms = find_matches(&page, "foo");
        assert_eq!(
            ms,
            vec![
                Match {
                    line_idx: 0,
                    col_start: 0,
                    col_end: 3
                },
                Match {
                    line_idx: 0,
                    col_start: 8,
                    col_end: 11
                },
                Match {
                    line_idx: 2,
                    col_start: 0,
                    col_end: 3
                },
            ]
        );
    }

    #[test]
    fn find_matches_empty_query_and_no_match_are_empty() {
        let page = vec![rline("abcdef")];
        assert!(
            find_matches(&page, "").is_empty(),
            "empty query -> no matches"
        );
        assert!(
            find_matches(&page, "xyz").is_empty(),
            "no hit -> no matches"
        );
        // A query longer than the line never matches.
        assert!(find_matches(&page, "abcdefg").is_empty());
    }

    #[test]
    fn find_matches_adjacent_hits_are_non_overlapping() {
        // "aa" over "aaaa" yields two adjacent, non-overlapping matches (not the
        // three an overlapping scan would report).
        let ms = find_matches(&[rline("aaaa")], "aa");
        assert_eq!(
            ms,
            vec![
                Match {
                    line_idx: 0,
                    col_start: 0,
                    col_end: 2
                },
                Match {
                    line_idx: 0,
                    col_start: 2,
                    col_end: 4
                },
            ]
        );
    }

    #[test]
    fn find_matches_positions_are_display_columns_after_wide_chars() {
        // Two width-2 emojis (4 display columns) precede "term", which starts at
        // char index 2 but display column 4; col_end is 4 + width("term") = 8.
        let ms = find_matches(&[rline("😀😀term")], "term");
        assert_eq!(
            ms,
            vec![Match {
                line_idx: 0,
                col_start: 4,
                col_end: 8,
            }]
        );
    }

    #[test]
    fn search_highlight_lands_on_the_term_after_wide_chars() {
        // "😀😀 " is 5 display columns; the term occupies the next six.
        let mut m = Model::from_document(&parse("😀😀 needle"), content_width(80), "S", (80, 24));
        commit_search(&mut m, "needle");
        let mch = m.matches.first().copied().expect("one match");
        assert_eq!((mch.col_start, mch.col_end), (5, 11));

        let buffer = render(&m, 80, 24);
        let row = TOPBAR_ROWS + (mch.line_idx - m.scroll) as u16;
        // The highlighted cells spell the term, not a wide-char-shifted slice.
        let mut got = String::new();
        for x in mch.col_start as u16..mch.col_end as u16 {
            got.push_str(buffer[(x, row)].symbol());
        }
        assert_eq!(got, "needle");
        assert!(
            buffer[(mch.col_start as u16, row)]
                .modifier
                .contains(Modifier::REVERSED),
            "the current match should be highlighted",
        );
    }

    #[test]
    fn link_after_wide_chars_hit_tests_at_its_display_column() {
        let doc = "😀😀 `[Alpha`:/page/alpha.mu]";
        let mut m = Model::from_document(&parse(doc), content_width(80), "S", (80, 24));
        // A current destination so the relative link resolves on activation.
        m.current_dest = Some([0x11; 16]);
        let vl = visible_links(&m)
            .into_iter()
            .find(|v| v.index == 1)
            .expect("alpha visible");
        // The label starts at display column 5 (after 😀😀 + space), not the char
        // index 3 the old cell-index positioning reported.
        assert_eq!(vl.col_start, 5);

        // A click on the label's display column follows it ...
        let effects = update(&mut m, AppEvent::Mouse(click(vl.col_start, vl.row)));
        assert!(matches!(effects.as_slice(), [Effect::Navigate(_)]));

        // ... while the old char-index column (3) sits inside the second emoji and
        // hits nothing.
        let mut m2 = Model::from_document(&parse(doc), content_width(80), "S", (80, 24));
        m2.current_dest = Some([0x11; 16]);
        let effects = update(&mut m2, AppEvent::Mouse(click(3, vl.row)));
        assert!(
            effects.is_empty(),
            "char-index column must not hit the link"
        );
    }

    /// A controlled document with a known anchor deep in a tall page: 30 intro
    /// paragraphs, then `` `:deep `` before a heading, then 30 trailing ones, so
    /// the anchor sits well below the first viewport and above the page bottom.
    fn long_anchor_doc() -> leviculum_micron::MicronDocument {
        let mut s = String::new();
        for i in 0..30 {
            s.push_str(&format!("intro{i}\n\n"));
        }
        s.push_str("`:deep\n>Deep Heading\n\n");
        for i in 0..30 {
            s.push_str(&format!("tail{i}\n\n"));
        }
        parse(&s)
    }

    #[test]
    fn anchor_line_resolves_known_and_rejects_unknown() {
        // `:target binds to its own (blank) block; the heading slug binds to the
        // heading block. Both resolve to a page line; an unknown anchor does not.
        let m = Model::from_document(
            &parse("Intro para\n`:target\n>Marked Section\nbody text"),
            content_width(80),
            "T",
            (80, 24),
        );
        let target_line = anchor_line(&m, "target").expect("known anchor resolves");
        let heading_line = anchor_line(&m, "marked-section").expect("heading slug resolves");
        assert!(
            line_text(&m.page[heading_line]).contains("Marked Section"),
            "heading anchor must land on the heading line: {:?}",
            line_text(&m.page[heading_line])
        );
        assert!(
            target_line < heading_line,
            "the anchor precedes the heading"
        );
        assert_eq!(anchor_line(&m, "nope"), None);
    }

    #[test]
    fn navigation_with_anchor_scrolls_to_anchor_line() {
        let mut m = model_from_sample(content_width(80), (80, 10));
        m.pending = Some(Pending {
            target: tgt(7),
            action: HistoryAction::Push,
        });
        m.pending_anchor = Some("deep".to_string());
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_anchor_doc(),
                title: "Deep".to_string(),
            },
        );
        let vp = m.viewport();
        let line = anchor_line(&m, "deep").expect("anchor resolves on the loaded page");
        assert!(
            line > vp,
            "anchor must sit below the first viewport to test a jump"
        );
        assert_eq!(m.scroll, line.min(m.max_scroll(vp)));
        assert!(m.scroll > 0, "scroll must have moved to the anchor");
        assert!(m.pending_anchor.is_none(), "anchor consumed on load");
    }

    #[test]
    fn navigation_with_unknown_anchor_stays_top_and_notes() {
        let mut m = model_from_sample(content_width(80), (80, 10));
        m.pending = Some(Pending {
            target: tgt(7),
            action: HistoryAction::Push,
        });
        m.pending_anchor = Some("missing".to_string());
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_anchor_doc(),
                title: "Deep".to_string(),
            },
        );
        assert_eq!(m.scroll, 0, "unknown anchor falls back to the top");
        let toast = m.toast.as_ref().expect("unknown anchor raises a toast");
        assert!(
            toast.text.contains("not found"),
            "unknown anchor should note it: {}",
            toast.text
        );
    }

    #[test]
    fn split_path_anchor_strips_the_fragment() {
        let (base, anchor) = split_path_anchor(Target {
            dest_hash: [1; 16],
            path: "/page/x.mu#sec".to_string(),
            fields: Vec::new(),
            is_file: false,
        });
        assert_eq!(base.path, "/page/x.mu");
        assert_eq!(anchor.as_deref(), Some("sec"));
        // No fragment -> no anchor, path untouched.
        let (base2, anchor2) = split_path_anchor(Target {
            dest_hash: [1; 16],
            path: "/page/y.mu".to_string(),
            fields: Vec::new(),
            is_file: false,
        });
        assert_eq!(base2.path, "/page/y.mu");
        assert!(anchor2.is_none());
    }

    /// A tall page whose only "needle" sits below the first viewport, so a search
    /// commit must scroll to reveal it.
    fn deep_match_model(size: (u16, u16)) -> Model {
        let mut s = String::new();
        for i in 0..40 {
            s.push_str(&format!("filler{i}\n\n"));
        }
        s.push_str("needle deep here\n");
        Model::from_document(&parse(&s), content_width(size.0), "S", size)
    }

    /// A short page with three "needle" occurrences on distinct lines, all inside
    /// a full-height viewport, for cycling tests.
    fn triple_match_model() -> Model {
        let s = "needle one\n\nfiller\n\nneedle two\n\nneedle three";
        Model::from_document(&parse(s), content_width(80), "S", (80, 24))
    }

    #[test]
    fn slash_enters_search_and_enter_commits_matches_and_scrolls() {
        let mut m = deep_match_model((80, 10));
        let eff = press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(eff.is_empty());
        assert_eq!(m.mode, Mode::Search);
        type_str(&mut m, "needle");
        let eff = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert!(eff.is_empty());
        assert_eq!(m.mode, Mode::Browse, "Enter commits and leaves search");
        assert!(!m.matches.is_empty(), "commit populates matches");
        assert_eq!(m.current_match, Some(0));
        // The (single, deep) match is scrolled into the viewport.
        let first = m.matches[0];
        let vp = m.viewport();
        assert!(
            first.line_idx >= vp,
            "match must start below the first viewport"
        );
        assert!(
            first.line_idx >= m.scroll && first.line_idx < m.scroll + vp,
            "current match must be visible after commit"
        );
    }

    #[test]
    fn n_and_shift_n_cycle_matches_and_wrap() {
        let mut m = triple_match_model();
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "needle");
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(m.matches.len(), 3, "three needle lines");
        assert_eq!(m.current_match, Some(0));
        press(&mut m, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(m.current_match, Some(1));
        press(&mut m, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(m.current_match, Some(2));
        // n wraps past the end back to the first.
        press(&mut m, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(m.current_match, Some(0));
        // N wraps before the start to the last.
        press(&mut m, KeyCode::Char('N'), KeyModifiers::NONE);
        assert_eq!(m.current_match, Some(2));
    }

    #[test]
    fn n_with_no_matches_is_a_noop() {
        let mut m = triple_match_model();
        assert!(m.matches.is_empty());
        press(&mut m, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(m.current_match, None);
        assert_eq!(m.scroll, 0);
    }

    #[test]
    fn reentering_search_and_esc_clears_highlights() {
        let mut m = triple_match_model();
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "needle");
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!m.matches.is_empty(), "committed matches exist");
        // Re-entering search drops the prior highlights immediately.
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(
            m.matches.is_empty(),
            "entering search clears old highlights"
        );
        // Esc returns to browse with nothing highlighted.
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(m.mode, Mode::Browse);
        assert!(m.matches.is_empty());
        assert!(m.current_match.is_none());
    }

    #[test]
    fn commit_with_no_match_notes_it() {
        let mut m = triple_match_model();
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "zzz");
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert!(m.matches.is_empty());
        assert_eq!(m.current_match, None);
        let toast = m.toast.as_ref().expect("a fruitless search raises a toast");
        assert!(
            toast.text.contains("no matches"),
            "a fruitless search should say so: {}",
            toast.text
        );
    }

    #[test]
    fn search_highlights_matches_and_current_is_stronger() {
        let mut m = triple_match_model();
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "needle");
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        let buffer = render(&m, 80, 24);
        // The current (first) match is drawn reversed.
        let cur = m.matches[0];
        let cur_row = TOPBAR_ROWS + cur.line_idx as u16;
        let cur_cell = &buffer[(cur.col_start as u16, cur_row)];
        assert!(
            cur_cell.modifier.contains(Modifier::REVERSED),
            "current match must use the stronger reversed highlight"
        );
        // A non-current match carries the theme's match tint, not the reverse.
        let other = m.matches[1];
        let other_row = TOPBAR_ROWS + other.line_idx as u16;
        let other_cell = &buffer[(other.col_start as u16, other_row)];
        assert_eq!(
            other_cell.bg,
            rgb(Theme::Dark.search_match_bg()),
            "non-current match must carry the tint background"
        );
        assert!(
            !other_cell.modifier.contains(Modifier::REVERSED),
            "non-current match must not use the stronger highlight"
        );
    }

    #[test]
    fn search_commit_scrolls_current_match_into_the_rendered_viewport() {
        let mut m = deep_match_model((80, 10));
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "needle");
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        let mline = m.matches[0].line_idx;
        let vp = m.viewport();
        assert!(
            mline >= m.scroll && mline < m.scroll + vp,
            "match in viewport"
        );
        let buffer = render(&m, 80, 10);
        let row = TOPBAR_ROWS + (mline - m.scroll) as u16;
        let cell = &buffer[(m.matches[0].col_start as u16, row)];
        assert!(
            cell.modifier.contains(Modifier::REVERSED),
            "the current match must render (reversed) inside the viewport"
        );
    }

    #[test]
    fn anchor_jump_renders_the_anchor_line_at_the_content_top() {
        let mut m = model_from_sample(content_width(80), (80, 10));
        m.pending = Some(Pending {
            target: tgt(7),
            action: HistoryAction::Push,
        });
        m.pending_anchor = Some("deep".to_string());
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: long_anchor_doc(),
                title: "Deep".to_string(),
            },
        );
        let buffer = render(&m, 80, 10);
        let top = row_text(&buffer, TOPBAR_ROWS, 80 - SCROLLBAR_COLS);
        assert_eq!(
            top.trim_end(),
            line_text(&m.page[m.scroll]).trim_end(),
            "the anchor's line must sit at the top of the content viewport"
        );
    }

    #[test]
    fn search_bar_renders_query_line() {
        let mut m = triple_match_model();
        press(&mut m, KeyCode::Char('/'), KeyModifiers::NONE);
        type_str(&mut m, "abc");
        let buffer = render(&m, 80, 24);
        let status = row_text(&buffer, 23, 80);
        assert!(status.contains('/'), "search prompt missing: {status:?}");
        assert!(status.contains("abc"), "query text missing: {status:?}");
    }

    // --- phase 6: bookmarks, copy, places panel -------------------------

    fn disc_node(hash: u8, name: &str) -> DiscoveredNode {
        DiscoveredNode {
            dest_hash: [hash; 16],
            name: Some(name.to_string()),
            first_seen: 0,
            last_seen: 0,
            hops: Some(1),
        }
    }

    #[test]
    fn osc52_wraps_base64_payload() {
        // ESC ] 52 ; c ; base64("hi") ST, with ST = ESC backslash.
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x1b\\");
    }

    /// The set of footer actions a browse footer keeps at width `w`.
    fn browse_actions(w: u16) -> Vec<FooterAction> {
        let m = loaded_model((80, 24));
        footer_layout(&m, Rect::new(0, 23, w, 1))
            .into_iter()
            .map(|s| s.item.action)
            .collect()
    }

    #[test]
    fn footer_shows_every_button_at_a_wide_width() {
        // Given enough room, the whole browse footer renders with labels and no
        // collapse: nav trio + 8 browse actions = 11 buttons, in order.
        let m = loaded_model((120, 24));
        let slots = footer_layout(&m, Rect::new(0, 23, 120, 1));
        assert_eq!(slots.len(), 11, "all browse buttons should fit wide");
        assert!(slots.iter().all(|s| !s.collapsed), "no collapse when wide");
        assert!(matches!(slots[0].item.action, FooterAction::Back));
        assert!(matches!(slots[1].item.action, FooterAction::Forward));
        assert!(matches!(slots[2].item.action, FooterAction::Reload));
        assert!(matches!(slots[3].item.action, FooterAction::Identify));
        // The stronger divider sits before the first non-nav button.
        assert!(slots[3].strong_divider, "nav/mode boundary divider missing");
        assert!(!slots[1].strong_divider, "nav items share the dim divider");
        let right = slots.last().map(|s| s.rect.right()).unwrap_or(0);
        assert!(right <= 120, "footer overflows: {right}");
    }

    #[test]
    fn footer_nav_trio_is_first_and_click_navigates_back() {
        let mut m = loaded_model((80, 24));
        // Give history a back step so back is available and does something.
        m.history.visit(tgt(0xcd));
        m.current_dest = Some([0xcd; 16]);
        assert!(m.history.can_back());
        let slot = footer_slot(&m, FooterAction::Back);
        let effects = update(&mut m, AppEvent::Mouse(click(slot.rect.x, slot.rect.y)));
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Navigate(_)));
    }

    #[test]
    fn footer_nav_trio_renders_keys_not_glyphs() {
        // The nav trio reads as ordinary key + label items like the rest of the
        // strip: `Alt-← back`, `Alt-→ forward`, `R reload`, no glyphs anywhere.
        let m = loaded_model((120, 24));
        let slots = footer_layout(&m, Rect::new(0, 23, 120, 1));
        let trio: Vec<(&str, &str)> = slots[..3]
            .iter()
            .map(|s| (s.item.key, s.item.label))
            .collect();
        assert_eq!(
            trio,
            vec![("Alt-←", "back"), ("Alt-→", "forward"), ("R", "reload")]
        );
        let buffer = render(&m, 120, 24);
        let row = row_text(&buffer, 23, 120);
        for g in ['‹', '›', '⟳'] {
            assert!(!row.contains(g), "glyph {g:?} still in footer: {row:?}");
        }
        assert!(row.contains("Alt-← back"), "back item text: {row:?}");
        assert!(row.contains("Alt-→ forward"), "forward item text: {row:?}");
        assert!(row.contains("R reload"), "reload item text: {row:?}");
    }

    #[test]
    fn footer_reload_key_is_bright_bold_and_label_is_muted() {
        // The reload item wears exactly the same chrome as every other footer
        // item: KEY in the bright chrome fg + BOLD, LABEL in the muted chrome fg.
        let m = loaded_model((80, 24));
        let slot = footer_slot(&m, FooterAction::Reload);
        let buffer = render(&m, 80, 24);
        let key = &buffer[(slot.rect.x, 23u16)];
        assert_eq!(
            key.fg,
            rgb(m.theme.chrome_fg()),
            "reload key must be bright"
        );
        assert!(
            key.modifier.contains(Modifier::BOLD),
            "reload key must be BOLD"
        );
        // The label ("reload") starts two columns past the key ("R" + a space).
        let label = &buffer[(slot.rect.x + 2, 23u16)];
        assert_eq!(
            label.fg,
            rgb(m.theme.chrome_muted_fg()),
            "reload label must be muted"
        );
    }

    #[test]
    fn footer_click_on_reload_reloads_the_page() {
        let mut m = loaded_model((80, 24));
        let slot = footer_slot(&m, FooterAction::Reload);
        let effects = update(&mut m, AppEvent::Mouse(click(slot.rect.x, slot.rect.y)));
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Navigate(_)));
    }

    #[test]
    fn help_overlay_lists_all_bindings_and_affordances() {
        let mut m = loaded_model((120, HELP_ROWS));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let text = flat(&render(&m, 120, HELP_ROWS));
        // The bindings the old hand-written list silently dropped.
        for s in [
            "Ctrl-n", "Ctrl-p", "Ctrl-v", "PageDown", "PageUp", "wheel", "Ctrl-r", "F5", "Alt-←",
            "Alt-→",
        ] {
            assert!(text.contains(s), "help missing binding {s:?}:\n{text}");
        }
        // The mouse affordances: the clickable URL, the star, the footer
        // buttons, and the side buttons.
        for s in [
            "click the URL",
            "click star",
            "footer button",
            "mouse side buttons",
        ] {
            assert!(text.contains(s), "help missing affordance {s:?}:\n{text}");
        }
    }

    #[test]
    fn help_overlay_has_no_heading_and_a_closing_border_title() {
        let mut m = loaded_model((120, 40));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let text = flat(&render(&m, 120, 40));
        assert!(
            !text.contains("lnomad — keys"),
            "stale heading line present:\n{text}"
        );
        assert!(
            text.contains("click to close"),
            "border title must mention closing:\n{text}"
        );
    }

    #[test]
    fn help_mentions_back_forward_exactly_once() {
        let mut m = loaded_model((120, 40));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let text = flat(&render(&m, 120, 40));
        let count = text
            .lines()
            .filter(|l| l.contains("back / forward"))
            .count();
        assert_eq!(count, 1, "back / forward must appear once:\n{text}");
    }

    /// The overlay scrollbar's thumb (`█`) appears in a column strictly left of
    /// the content's own scrollbar (the rightmost column), so this detects the
    /// overlay's scrollbar without picking up the always-present content one.
    fn overlay_scrollbar_shown(buffer: &ratatui::buffer::Buffer) -> bool {
        let w = buffer.area.width;
        for y in 0..buffer.area.height {
            for x in 0..w.saturating_sub(1) {
                if buffer[(x, y)].symbol() == "█" {
                    return true;
                }
            }
        }
        false
    }

    /// Whether the places panel's selected entry currently sits inside the
    /// overlay's viewport — the invariant the view-follow logic must keep.
    fn selected_visible(m: &Model) -> bool {
        let layout = places_layout(m);
        let vp = places_viewport(m, layout.total);
        let sel_line = layout.entry_line[m.places_sel];
        sel_line >= m.places_scroll && sel_line < m.places_scroll + vp
    }

    #[test]
    fn help_overlay_scrolls_when_taller_than_the_terminal() {
        // A terminal too short to show the whole help.
        let rows = 8u16;
        let mut m = loaded_model((80, rows));
        let total = help_lines().len();
        assert!(
            total + 2 > rows as usize,
            "the help must overflow an {rows}-row terminal for this test"
        );
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(m.help_scroll, 0, "the help opens at the top");
        let page_before = m.scroll;
        let (viewport, max) = help_scroll_geometry(&m);
        assert!(max > 0, "there must be room to scroll");

        // Down motions: j, Ctrl-n, PageDown, Ctrl-f.
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(m.help_scroll, 1, "j scrolls down one");
        press(&mut m, KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert_eq!(m.help_scroll, 2, "Ctrl-n scrolls down one");
        let before = m.help_scroll;
        press(&mut m, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(
            m.help_scroll,
            (before + viewport).min(max),
            "PageDown moves by a viewport"
        );
        m.help_scroll = 0;
        press(&mut m, KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!(m.help_scroll, viewport.min(max), "Ctrl-f pages down");

        // G / End reach the last line; g / Home return to the top.
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        assert_eq!(m.help_scroll, max, "G reaches the bottom");
        press(&mut m, KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(m.help_scroll, 0, "g returns to the top");
        press(&mut m, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(m.help_scroll, max, "End also reaches the bottom");
        press(&mut m, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(m.help_scroll, 0, "Home returns to the top");

        // The wheel scrolls the overlay, both ways, clamped.
        update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollDown)));
        assert_eq!(
            m.help_scroll,
            WHEEL_STEP.min(max),
            "wheel down scrolls the help"
        );
        update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollUp)));
        assert_eq!(m.help_scroll, 0, "wheel up scrolls back to the top");

        // The offset clamps at both ends.
        press(&mut m, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(m.help_scroll, 0, "clamps at the top");
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(m.help_scroll, max, "clamps at the bottom");

        // Scrolling the help never moves the page underneath.
        assert_eq!(m.scroll, page_before, "the page offset is untouched");
    }

    #[test]
    fn help_scroll_reveals_the_last_line_and_draws_a_scrollbar() {
        // Wide but short: the overlay is narrower than the terminal, so its
        // right-border scrollbar sits strictly left of the content scrollbar.
        let (w, rows) = (120u16, 8u16);
        let mut m = loaded_model((w, rows));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        // The last help entry's keys are unique to the help body.
        let marker = "q / Ctrl-c";
        let top = flat(&render(&m, w, rows));
        assert!(
            !top.contains(marker),
            "the last line must be hidden at the top:\n{top}"
        );
        assert!(
            overlay_scrollbar_shown(&render(&m, w, rows)),
            "the scrollbar must render while the help overflows"
        );
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        let bottom = flat(&render(&m, w, rows));
        assert!(
            bottom.contains(marker),
            "G must reveal the last help line:\n{bottom}"
        );
    }

    #[test]
    fn help_that_fits_shows_no_scrollbar_and_ignores_scroll_keys() {
        let mut m = loaded_model((120, HELP_ROWS));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let (_, max) = help_scroll_geometry(&m);
        assert_eq!(max, 0, "the help fits, so there is nothing to scroll");
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        assert_eq!(
            m.help_scroll, 0,
            "scroll keys are pinned when the help fits"
        );
        assert!(
            !overlay_scrollbar_shown(&render(&m, 120, HELP_ROWS)),
            "no scrollbar when the help fits the terminal"
        );
    }

    #[test]
    fn click_closes_help_without_falling_through() {
        let mut m = loaded_model((120, 40));
        // A footer button that WOULD act on a click were the help closed.
        let slot = footer_slot(&m, FooterAction::Reload);
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(m.show_help);
        let effects = update(&mut m, AppEvent::Mouse(click(slot.rect.x, slot.rect.y)));
        assert!(!m.show_help, "a click must close the help");
        assert!(
            effects.is_empty(),
            "the click must not fall through to the footer button: {effects:?}"
        );
        // Esc and ? still close it.
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(m.show_help);
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.show_help, "Esc must still close the help");
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(m.show_help);
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(!m.show_help, "? must still close the help");
    }

    #[test]
    fn help_titles_dim_keys_bold_bright_desc_muted() {
        // The first cell of `needle`'s run in the rendered buffer.
        fn find_cell(buffer: &ratatui::buffer::Buffer, needle: &str) -> (u16, u16) {
            let chars: Vec<char> = needle.chars().collect();
            let area = buffer.area();
            for y in 0..area.height {
                for x in 0..area.width {
                    if (0..chars.len() as u16).all(|i| {
                        let cx = x + i;
                        cx < area.width && buffer[(cx, y)].symbol().starts_with(chars[i as usize])
                    }) {
                        return (x, y);
                    }
                }
            }
            panic!("cell for {needle:?} not found");
        }

        for theme in [Theme::Dark, Theme::Light] {
            let mut m = loaded_model((120, 40));
            if theme == Theme::Light {
                m.toggle_theme();
            }
            press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
            let buffer = render(&m, 120, 40);

            // A group title renders dim, in the muted chrome fg.
            let title = &buffer[find_cell(&buffer, "Move")];
            assert!(
                title.modifier.contains(Modifier::DIM),
                "group title must be dim ({theme:?})"
            );
            assert_eq!(
                title.fg,
                rgb(theme.chrome_muted_fg()),
                "group title must use the muted chrome fg ({theme:?})"
            );
            // A key cell is BOLD in the bright chrome fg, never dim.
            let key = &buffer[find_cell(&buffer, "wheel")];
            assert!(
                key.modifier.contains(Modifier::BOLD),
                "key cell must be bold ({theme:?})"
            );
            assert!(
                !key.modifier.contains(Modifier::DIM),
                "key cell must not be dim ({theme:?})"
            );
            assert_eq!(
                key.fg,
                rgb(theme.chrome_fg()),
                "key cell must use the bright chrome fg ({theme:?})"
            );
            // A description cell is muted, not bold.
            let desc = &buffer[find_cell(&buffer, "scroll")];
            assert_eq!(
                desc.fg,
                rgb(theme.chrome_muted_fg()),
                "description must use the muted chrome fg ({theme:?})"
            );
            assert!(
                !desc.modifier.contains(Modifier::BOLD),
                "description must not be bold ({theme:?})"
            );
            assert_ne!(
                key.fg, desc.fg,
                "key and description must differ ({theme:?})"
            );
        }
    }

    #[test]
    fn footer_unavailable_nav_reads_muted_not_dim() {
        // loaded_model has a single history entry: back is unavailable and must
        // render in the readable muted chrome fg, never DIM.
        for theme in [Theme::Dark, Theme::Light] {
            let mut m = loaded_model((80, 24));
            if theme == Theme::Light {
                m.toggle_theme();
            }
            assert!(!m.history.can_back(), "back must be unavailable here");
            let slot = footer_slot(&m, FooterAction::Back);
            let buffer = render(&m, 80, 24);
            let cell = &buffer[(slot.rect.x, 23u16)];
            assert_eq!(
                cell.fg,
                rgb(theme.chrome_muted_fg()),
                "unavailable nav must use the muted chrome fg ({theme:?})"
            );
            assert!(
                !cell.modifier.contains(Modifier::DIM),
                "unavailable nav must not be DIM ({theme:?})"
            );
        }
    }

    #[test]
    fn footer_click_on_places_opens_the_panel() {
        // Wide enough that `d places` survives the priority truncation (the
        // spelled-out nav keys make the full strip a touch wider than before).
        let mut m = loaded_model((120, 24));
        assert!(!m.show_places);
        let slot = footer_slot(&m, FooterAction::Places);
        let effects = update(&mut m, AppEvent::Mouse(click(slot.rect.x, slot.rect.y)));
        assert!(effects.is_empty());
        assert!(m.show_places, "clicking `d places` opens the places panel");
    }

    #[test]
    fn footer_priority_truncation_drops_copy_first() {
        // At a width too tight for every label, `y copy` drops first while the nav
        // trio, `? help` and `q quit` always remain, and labels stay (no collapse).
        let acts = browse_actions(90);
        assert!(
            !acts.contains(&FooterAction::Copy),
            "y copy should drop first: {acts:?}"
        );
        for keep in [
            FooterAction::Back,
            FooterAction::Forward,
            FooterAction::Reload,
            FooterAction::Help,
            FooterAction::Quit,
        ] {
            assert!(acts.contains(&keep), "{keep:?} must survive: {acts:?}");
        }
        let m = loaded_model((80, 24));
        assert!(
            footer_layout(&m, Rect::new(0, 23, 90, 1))
                .iter()
                .all(|s| !s.collapsed),
            "labels should stay at a merely-narrow width"
        );
    }

    #[test]
    fn footer_collapses_labels_to_keys_when_very_narrow() {
        let m = loaded_model((30, 24));
        let slots = footer_layout(&m, Rect::new(0, 23, 30, 1));
        assert!(
            slots.iter().all(|s| s.collapsed),
            "a very narrow footer must collapse to keys"
        );
        // Rendered: the keys survive, the words do not.
        let buffer = render(&m, 30, 24);
        let row = row_text(&buffer, 23, 30);
        assert!(row.contains("Alt-←"), "back key missing: {row:?}");
        assert!(row.contains('q'), "quit key missing: {row:?}");
        assert!(!row.contains("quit"), "labels should be gone: {row:?}");
        assert!(!row.contains("back"), "labels should be gone: {row:?}");
    }

    #[test]
    fn footer_field_mode_shows_field_actions() {
        let mut m = loaded_model((80, 24));
        // Tab past the two links onto the sample page's text field.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.mode, Mode::Field);
        // At a comfortable width the full field-mode set is shown.
        let labels: Vec<&str> = footer_layout(&m, Rect::new(0, 23, 100, 1))
            .into_iter()
            .map(|s| s.item.label)
            .collect();
        assert!(labels.contains(&"done"), "Esc done missing: {labels:?}");
        assert!(
            labels.contains(&"next field"),
            "Tab next field missing: {labels:?}"
        );
        // A text field takes Space as a letter, so the footer offers no toggle
        // there: its keys are the key table's, live in this field (slice 1b).
        assert!(
            !labels.contains(&"toggle"),
            "Space toggle offered on a text field: {labels:?}"
        );
        // The help is F1 here: `?` is a letter while typing.
        let keys: Vec<&str> = footer_layout(&m, Rect::new(0, 23, 100, 1))
            .into_iter()
            .map(|s| s.item.key)
            .collect();
        assert!(keys.contains(&"F1"), "help key missing: {keys:?}");
        assert!(!keys.contains(&"?"), "? is typed, not help: {keys:?}");
        // On a checkbox Space toggles, and the footer says so.
        let mut m = field_model("Agree: `<?|agree`Agree>", (80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.mode, Mode::Field);
        let toggle = footer_layout(&m, Rect::new(0, 23, 100, 1))
            .into_iter()
            .find(|s| s.item.label == "toggle")
            .expect("Space toggle on a checkbox");
        assert_eq!(toggle.item.key, "Space");
    }

    #[test]
    fn footer_hover_highlights_then_clears() {
        let mut m = loaded_model((80, 24));
        let slot = footer_slot(&m, FooterAction::Quit);
        update(&mut m, AppEvent::Mouse(moved(slot.rect.x, slot.rect.y)));
        assert_eq!(m.footer_hover, Some(slot.index), "move sets footer hover");
        let buffer = render(&m, 80, 24);
        let cell = &buffer[(slot.rect.x, 23u16)];
        assert!(
            cell.modifier.contains(Modifier::UNDERLINED),
            "hovered footer button not highlighted: {:?}",
            cell.modifier
        );
        // A move off the footer clears it.
        update(&mut m, AppEvent::Mouse(moved(slot.rect.x, 10)));
        assert_eq!(m.footer_hover, None, "moving off clears footer hover");
    }

    #[test]
    fn bookmark_toggle_adds_then_removes_and_persists() {
        let mut m = loaded_model((80, 24));
        let url = current_url(&m).expect("current url");
        // First `m` bookmarks the page and asks the shell to persist.
        let fx = press(&mut m, KeyCode::Char('m'), KeyModifiers::NONE);
        assert!(m.bookmarks.contains(&url));
        assert_eq!(fx, vec![Effect::SaveBookmarks]);
        // Second `m` un-bookmarks it, again persisting.
        let fx = press(&mut m, KeyCode::Char('m'), KeyModifiers::NONE);
        assert!(!m.bookmarks.contains(&url));
        assert_eq!(fx, vec![Effect::SaveBookmarks]);
    }

    #[test]
    fn yank_copies_current_page_url() {
        let mut m = loaded_model((80, 24));
        let url = current_url(&m).expect("current url");
        let fx = press(&mut m, KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(fx, vec![Effect::Copy(url)]);
        let toast = m.toast.as_ref().expect("a copy toast is raised");
        assert_eq!(toast.kind, ToastKind::Info);
        assert!(toast.text.contains("copied"), "toast: {}", toast.text);
    }

    // --- identify: the `i` toggle, top-bar marker, footer button, help ----

    /// A loaded model with a fingerprint set, as the IO shell would build it.
    fn identify_model(size: (u16, u16)) -> Model {
        let mut m = loaded_model(size);
        m.fingerprint = "aabbccddeeff00112233445566778899".to_string();
        m
    }

    #[test]
    fn i_toggles_identify_emits_setidentify_and_reload() {
        let mut m = identify_model((80, 24));
        assert!(!m.identifying, "anonymous by default");

        let fx = press(&mut m, KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(
            fx.first(),
            Some(&Effect::SetIdentify {
                dest: [0xab; 16],
                on: true
            }),
            "the decision is persisted before the reload: {fx:?}"
        );
        assert!(
            matches!(fx.get(1), Some(Effect::Navigate(t)) if t.dest_hash == [0xab; 16]),
            "the toggle reloads the current page: {fx:?}"
        );
        assert!(m.identifying, "the flag flips optimistically");
        let toast = m.toast.as_ref().expect("a toggle toast is raised");
        assert_eq!(toast.kind, ToastKind::Info);
        assert!(
            toast.text.contains("Identifying") && toast.text.contains("aabbccdd"),
            "toast names the fingerprint: {}",
            toast.text
        );

        let fx = press(&mut m, KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(
            fx.first(),
            Some(&Effect::SetIdentify {
                dest: [0xab; 16],
                on: false
            }),
            "the second toggle turns it off: {fx:?}"
        );
        assert!(
            matches!(fx.get(1), Some(Effect::Navigate(_))),
            "the second toggle reloads too: {fx:?}"
        );
        assert!(!m.identifying, "back to anonymous");
        let toast = m.toast.as_ref().expect("a toggle toast is raised");
        assert!(
            toast.text.contains("anonymous"),
            "toast notes anonymity: {}",
            toast.text
        );
    }

    #[test]
    fn i_without_current_dest_is_noop() {
        let mut m = model_from_sample(80, (80, 24));
        assert!(m.current_dest.is_none());
        let fx = press(&mut m, KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(fx.is_empty(), "no page, no effects: {fx:?}");
        assert!(!m.identifying);
    }

    #[test]
    fn topbar_shows_key_marker_only_when_identifying() {
        let mut m = identify_model((80, 24));
        let anon = row_text(&render(&m, 80, 24), 0, 80);
        assert!(
            !anon.contains(IDENTIFY_GLYPH),
            "anonymous shows no marker: {anon:?}"
        );
        m.identifying = true;
        let ident = row_text(&render(&m, 80, 24), 0, 80);
        assert!(
            ident.contains(IDENTIFY_GLYPH),
            "key marker missing: {ident:?}"
        );
        assert!(
            ident.contains("aabbccdd"),
            "fingerprint prefix missing: {ident:?}"
        );
        assert!(
            !ident.contains("eeff0011"),
            "only the first 8 hex chars show: {ident:?}"
        );
    }

    #[test]
    fn topbar_identity_marker_survives_hops_and_bolt_dropping() {
        // At a width too narrow for the whole cluster the hop count drops
        // first, then the cache bolt; the identity marker (and the star)
        // survive. current_dest is set, so hops would read "unknown".
        let mut m = identify_model((14, 24));
        m.identifying = true;
        m.cached_view = true;
        let top = row_text(&render(&m, 14, 24), 0, 14);
        assert!(
            top.contains(IDENTIFY_GLYPH) && top.contains("aabbccdd"),
            "identity must survive truncation: {top:?}"
        );
        assert!(!top.contains("unknown"), "hops drop first: {top:?}");
        assert!(!top.contains(CACHED_GLYPH), "the bolt drops next: {top:?}");
    }

    #[test]
    fn footer_identify_label_flips() {
        let mut m = loaded_model((80, 24));
        let label_of = |m: &Model| {
            footer_items(m)
                .into_iter()
                .find(|i| i.action == FooterAction::Identify)
                .map(|i| i.label)
        };
        assert_eq!(label_of(&m), Some("identify"), "anonymous state");
        m.identifying = true;
        assert_eq!(label_of(&m), Some("anonymous"), "identifying state");
        m.current_dest = None;
        assert_eq!(label_of(&m), None, "no page, no identify button");
    }

    #[test]
    fn click_footer_identify_toggles() {
        let mut m = identify_model((80, 24));
        let slot = footer_slot(&m, FooterAction::Identify);
        let fx = update(&mut m, AppEvent::Mouse(click(slot.rect.x, slot.rect.y)));
        assert_eq!(
            fx.first(),
            Some(&Effect::SetIdentify {
                dest: [0xab; 16],
                on: true
            }),
            "the click persists like the key: {fx:?}"
        );
        assert!(
            matches!(fx.get(1), Some(Effect::Navigate(_))),
            "the click reloads like the key: {fx:?}"
        );
        assert!(m.identifying, "the click flips the flag like the key");
    }

    #[test]
    fn help_lists_identify_binding() {
        let mut m = loaded_model((120, 40));
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let text = flat(&render(&m, 120, 40));
        assert!(
            text.contains("identify / go anonymous to this node"),
            "help missing the i binding:\n{text}"
        );
        assert!(
            text.contains("default anonymous"),
            "help missing the anonymity-model line:\n{text}"
        );
    }

    #[test]
    fn cached_revisit_restores_identify_state_per_node() {
        let mut m = tall_model((80, 24));
        // Fresh-load node 1 and learn (as the IO shell does after a load) that
        // the session identifies to it.
        begin_navigation(&mut m, tgt(1), HistoryAction::Push, None);
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("one"),
                title: "One".to_string(),
            },
        );
        m.note_identify([1; 16], true);
        assert!(m.identifying);
        // Fresh-load node 2, anonymous.
        begin_navigation(&mut m, tgt(2), HistoryAction::Push, None);
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("two"),
                title: "Two".to_string(),
            },
        );
        m.note_identify([2; 16], false);
        assert!(!m.identifying, "node 2 is anonymous");
        // Back to node 1 is a cache hit: no fetch, yet the flag is restored.
        let fx = go_back(&mut m);
        assert!(fx.is_empty(), "cache hit fetches nothing: {fx:?}");
        assert!(m.identifying, "cached revisit restores the per-node state");
        // Forward to node 2, also cached: anonymous again.
        go_forward(&mut m);
        assert!(!m.identifying, "cached forward restores anonymity");
    }

    #[test]
    /// Started without a URL, the browser shows its start screen with the places
    /// panel already in front of it: nothing is fetched, nothing enters the
    /// history, and no destination is current, so the top bar carries no address
    /// and back/forward/reload stay unavailable.
    fn start_screen_opens_the_places_panel_over_a_local_page() {
        let mut m = Model {
            size: (80, 24),
            ..Model::default()
        };
        m.relayout(content_width(80));
        show_start_screen(&mut m);

        assert!(m.show_places, "the places panel must open by itself");
        assert_eq!(m.title, START_TITLE);
        assert!(!m.page.is_empty(), "the start page must be laid out");
        assert!(m.history.current().is_none(), "no history entry");
        assert!(!m.history.can_back() && !m.history.can_forward());
        assert_eq!(m.current_dest, None, "no node is current");
        assert_eq!(current_url(&m), None, "no address to show");
        assert!(!m.is_loading(), "the start screen must not fetch anything");
        assert!(m.page_cache.get(&tgt(0xab)).is_none(), "nothing cached");
    }

    #[test]
    /// The start screen names the keys that lead somewhere, and closing the
    /// panel leaves it in view rather than an empty frame.
    fn start_screen_survives_closing_the_panel() {
        let mut m = Model {
            size: (80, 24),
            ..Model::default()
        };
        m.relayout(content_width(80));
        show_start_screen(&mut m);

        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.show_places, "Esc closes the panel");

        let text = flat(&render(&m, 80, 24));
        assert!(text.contains("lnomad"), "start screen gone: {text}");
        assert!(text.contains("places"), "no hint at the panel: {text}");
        assert!(text.contains("URL"), "no hint at the URL entry: {text}");
    }

    #[test]
    /// An empty discovered-nodes section says announces are still awaited, not
    /// that there are none: on a first run the panel opens by itself with both
    /// sections empty, and "(none)" would read as a broken feature.
    fn empty_places_panel_says_it_is_still_listening() {
        let mut m = Model {
            size: (80, 24),
            ..Model::default()
        };
        m.relayout(content_width(80));
        show_start_screen(&mut m);

        let text = flat(&render(&m, 80, 24));
        assert!(
            text.contains("listening for announces"),
            "no listening note: {text}"
        );
    }

    #[test]
    fn places_lists_bookmarks_then_discovered_nodes() {
        let mut m = loaded_model((80, 24));
        m.bookmarks.add(Bookmark {
            url: "aa11:/page/index.mu".to_string(),
            title: "Home".to_string(),
        });
        m.node_registry.upsert_node(&disc_node(0xcd, "Node-C"));
        let ps = places(&m);
        assert_eq!(ps.len(), 2, "one bookmark then one node");
        assert!(matches!(ps[0], Place::Bookmark { .. }));
        assert!(matches!(ps[1], Place::Node { .. }));
    }

    #[test]
    fn places_panel_opens_navigates_and_activates() {
        let mut m = loaded_model((80, 24));
        m.bookmarks.add(Bookmark {
            url: "aa11:/page/index.mu".to_string(),
            title: "Home".to_string(),
        });
        m.node_registry.upsert_node(&disc_node(0xcd, "Node-C"));

        // `d` opens the panel; it then owns keys.
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(m.show_places);
        assert_eq!(m.places_sel, 0);

        // `j` moves onto the discovered node; Enter opens its default page.
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 1);
        let fx = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!m.show_places, "activating closes the panel");
        assert_eq!(
            fx,
            vec![Effect::Navigate(Target {
                dest_hash: [0xcd; 16],
                path: DEFAULT_PATH.to_string(),
                fields: Vec::new(),
                is_file: false,
            })]
        );
    }

    #[test]
    fn places_panel_shares_the_content_scroll_keymap() {
        let mut m = loaded_model((80, 24));
        for i in 0..5u8 {
            m.bookmarks.add(Bookmark {
                url: format!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa{i:02}:/page/index.mu"),
                title: format!("B{i}"),
            });
        }
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(m.show_places);
        assert_eq!(m.places_sel, 0);

        // Line down: j, Ctrl-n, Down all step one entry.
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 1);
        press(&mut m, KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert_eq!(m.places_sel, 2);
        press(&mut m, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(m.places_sel, 3);

        // Line up: k, Ctrl-p, Up step back one entry.
        press(&mut m, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 2);
        press(&mut m, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(m.places_sel, 1);
        press(&mut m, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(m.places_sel, 0);

        // g / G / Home / End jump to the first / last entry.
        let last = places(&m).len() - 1;
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, last);
        press(&mut m, KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 0);
        press(&mut m, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(m.places_sel, last);
        press(&mut m, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(m.places_sel, 0);

        // Ctrl-d is a half-page motion here, not the `d` that closes the panel.
        press(&mut m, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(m.show_places, "Ctrl-d must not close the panel");
        assert_eq!(
            m.places_sel, last,
            "half-page jump clamps to the last entry"
        );

        // Plain `d` still closes the panel.
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(!m.show_places);
    }

    /// Fill a model with `n` discovered nodes so the panel overflows a short
    /// terminal.
    fn with_nodes(size: (u16, u16), n: u8) -> Model {
        let mut m = loaded_model(size);
        for i in 0..n {
            update(
                &mut m,
                AppEvent::NodeDiscovered(disc_node(0x40 + i, &format!("Node-{i}"))),
            );
        }
        m
    }

    #[test]
    fn places_panel_scrolls_to_follow_the_selection() {
        let mut m = with_nodes((80, 8), 20);
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(m.show_places);
        assert_eq!(m.places_sel, 0);
        assert_eq!(m.places_scroll, 0, "the panel opens at the top");

        let layout = places_layout(&m);
        let vp = places_viewport(&m, layout.total);
        assert!(layout.total > vp, "the list must overflow this terminal");

        // Stepping down one entry at a time keeps the selected row visible, and
        // once the selection reaches the viewport bottom the view scrolls by one
        // (all entries are nodes here, so a step is exactly one line).
        let len = places(&m).len();
        let mut prev = m.places_scroll;
        let mut ever_scrolled = false;
        for _ in 0..len - 1 {
            press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
            assert!(selected_visible(&m), "the selected row must stay visible");
            if m.places_scroll != prev {
                assert_eq!(m.places_scroll, prev + 1, "the view scrolls by one line");
                ever_scrolled = true;
            }
            prev = m.places_scroll;
        }
        assert!(
            ever_scrolled,
            "stepping past the bottom must scroll the view"
        );

        // G selects the last node AND shows it; g returns to the top.
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, len - 1, "G selects the last node");
        assert!(selected_visible(&m), "the last node's row is visible");
        press(&mut m, KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 0, "g selects the first entry");
        // Minimum-needed scrolling: the first entry is brought into view without
        // scrolling any further than required to reveal it.
        assert!(selected_visible(&m), "the first entry is visible after g");
    }

    #[test]
    fn places_selection_stays_visible_across_the_section_boundary() {
        let mut m = with_nodes((80, 8), 10);
        for i in 0..4u8 {
            m.bookmarks.add(Bookmark {
                url: format!("aa{i:02}:/page/index.mu"),
                title: format!("B{i}"),
            });
        }
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        let len = places(&m).len();
        // Walk the whole list; the selected row is visible at every step, even
        // as it crosses the blank spacer and the "Pages heard" title.
        for _ in 0..len + 2 {
            press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
            assert!(
                selected_visible(&m),
                "selection off-screen at sel={}",
                m.places_sel
            );
        }
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        assert!(selected_visible(&m), "G keeps the last row visible");
    }

    #[test]
    fn places_page_motion_moves_by_the_visible_entry_count() {
        let mut m = with_nodes((80, 8), 20);
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        let vp = places_viewport(&m, places_layout(&m).total);
        let max = places(&m).len() - 1;
        press(&mut m, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(
            m.places_sel,
            vp.min(max),
            "PageDown moves the selection by the viewport entry count, not a constant"
        );
        assert!(selected_visible(&m));
    }

    #[test]
    fn places_wheel_scrolls_the_view_and_never_moves_the_selection() {
        let mut m = with_nodes((80, 8), 20);
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        let layout = places_layout(&m);
        let vp = places_viewport(&m, layout.total);
        let max = layout.total.saturating_sub(vp);
        assert!(max > WHEEL_STEP, "the list must overflow enough to wheel");

        // A notch down moves the VIEW by WHEEL_STEP lines and leaves the
        // selection where it was.
        update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollDown)));
        assert_eq!(m.places_scroll, WHEEL_STEP, "wheel down scrolls the view");
        assert_eq!(m.places_sel, 0, "wheel down leaves the selection alone");

        // A notch up scrolls the view back and again leaves the selection alone.
        update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollUp)));
        assert_eq!(m.places_scroll, 0, "wheel up scrolls the view back");
        assert_eq!(m.places_sel, 0, "wheel up leaves the selection alone");
    }

    #[test]
    fn places_wheel_clamps_at_both_ends() {
        let mut m = with_nodes((80, 8), 20);
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        let layout = places_layout(&m);
        let vp = places_viewport(&m, layout.total);
        let max = layout.total.saturating_sub(vp);

        // Wheeling up at the top never underflows.
        update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollUp)));
        assert_eq!(m.places_scroll, 0, "the view never scrolls below 0");

        // Wheeling down past the bottom clamps to max, never beyond.
        for _ in 0..layout.total {
            update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollDown)));
        }
        assert_eq!(m.places_scroll, max, "the view never scrolls past max");
        update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollDown)));
        assert_eq!(m.places_scroll, max, "an extra notch stays clamped");
        assert_eq!(m.places_sel, 0, "wheeling never moved the selection");
    }

    #[test]
    fn places_wheel_leaves_every_entry_reachable() {
        // The regression: one notch used to jump the selection by WHEEL_STEP and
        // skip entries. Now a notch does not move the selection at all, so the
        // very next `j` steps by exactly one and no entry is skipped.
        let mut m = with_nodes((80, 8), 20);
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        let sel = m.places_sel;
        update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollDown)));
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(
            m.places_sel,
            sel + 1,
            "after a wheel notch, j moves the selection by one, not by WHEEL_STEP"
        );
    }

    #[test]
    fn places_key_after_wheeling_off_screen_snaps_the_view_back() {
        let mut m = with_nodes((80, 8), 20);
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        let sel_before = m.places_sel;

        // Wheel the view down until the selected row is off-screen.
        for _ in 0..layout_max_notches(&m) {
            update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollDown)));
        }
        assert!(!selected_visible(&m), "the selection is now off-screen");
        assert_eq!(m.places_sel, sel_before, "wheeling did not move it");

        // One `j` moves the selection by exactly one and the view snaps back so
        // the selected row is inside the viewport again.
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, sel_before + 1, "j steps by exactly one");
        assert!(
            selected_visible(&m),
            "the view scrolled back to the selection"
        );
    }

    #[test]
    fn places_wheel_is_a_no_op_when_the_panel_fits() {
        // A tall terminal shows the whole (short) list, so there is nothing to
        // scroll and the wheel changes neither the view nor the selection.
        let mut m = with_nodes((80, 40), 3);
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        let layout = places_layout(&m);
        let vp = places_viewport(&m, layout.total);
        assert!(layout.total <= vp, "the whole list must fit for this test");

        update(&mut m, AppEvent::Mouse(mouse(MouseEventKind::ScrollDown)));
        assert_eq!(m.places_scroll, 0, "a fitting panel ignores the wheel");
        assert_eq!(m.places_sel, 0, "and does not move the selection");
    }

    /// How many wheel notches it takes to reach the bottom of the places view.
    fn layout_max_notches(m: &Model) -> usize {
        let layout = places_layout(m);
        let vp = places_viewport(m, layout.total);
        let max = layout.total.saturating_sub(vp);
        max.div_ceil(WHEEL_STEP)
    }

    /// The screen row the `idx`-th places entry is drawn on, through the same
    /// overlay geometry and clamped scroll offset the renderer and hit-test use.
    fn place_row(m: &Model, idx: usize) -> u16 {
        let overlay = places_overlay_rect(m, m.frame_area());
        let layout = places_layout(m);
        let vp = places_viewport(m, layout.total);
        let max = layout.total.saturating_sub(vp);
        let offset = m.places_scroll.min(max);
        overlay.y + 1 + (layout.entry_line[idx] - offset) as u16
    }

    /// A column safely inside the panel's inner region.
    fn place_col(m: &Model) -> u16 {
        places_overlay_rect(m, m.frame_area()).x + 2
    }

    /// A model with one bookmark then one discovered node, panel open. The
    /// bookmark carries a full 32-hex destination hash so activating it parses to
    /// a real navigation target.
    fn places_model() -> Model {
        let mut m = loaded_model((80, 24));
        m.bookmarks.add(Bookmark {
            url: format!("{}:/page/index.mu", "bb".repeat(16)),
            title: "Home".to_string(),
        });
        m.node_registry.upsert_node(&disc_node(0xcd, "Node-C"));
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(m.show_places);
        m
    }

    /// A model with three titled bookmarks (indices 0..=2) then one discovered
    /// node (index 3), panel open on the first row.
    fn bookmarks_model() -> Model {
        let mut m = loaded_model((80, 24));
        for (i, title) in ["Alpha", "Beta", "Gamma"].iter().enumerate() {
            m.bookmarks.add(Bookmark {
                url: format!("{}:/page/{i}.mu", "aa".repeat(16)),
                title: title.to_string(),
            });
        }
        m.node_registry.upsert_node(&disc_node(0xcd, "Node-C"));
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(m.show_places);
        assert_eq!(m.places_sel, 0);
        m
    }

    /// The titles of `m`'s bookmarks, in order.
    fn bookmark_titles(m: &Model) -> Vec<String> {
        m.bookmarks.list().iter().map(|b| b.title.clone()).collect()
    }

    #[test]
    fn places_click_on_a_bookmark_row_activates_it() {
        let mut m = places_model();
        // A single left click on the bookmark row does exactly what Enter on that
        // selection does, and selects it first.
        let mut via_key = m.clone();
        let expected = press(&mut via_key, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!expected.is_empty(), "a bookmark row navigates");

        let (col, row) = (place_col(&m), place_row(&m, 0));
        let fx = update(&mut m, AppEvent::Mouse(click(col, row)));
        assert_eq!(m.places_sel, 0, "the click selects the bookmark row");
        assert_eq!(fx, expected, "the click activates exactly like Enter");
        assert!(!m.show_places, "activating closes the panel");
    }

    #[test]
    fn places_click_on_a_discovered_node_row_activates_it() {
        let mut m = places_model();
        let node_idx = 1;
        let mut via_key = m.clone();
        via_key.places_sel = node_idx;
        let expected = press(&mut via_key, KeyCode::Enter, KeyModifiers::NONE);

        let (col, row) = (place_col(&m), place_row(&m, node_idx));
        let fx = update(&mut m, AppEvent::Mouse(click(col, row)));
        assert_eq!(m.places_sel, node_idx, "the click selects the node row");
        assert_eq!(
            fx,
            vec![Effect::Navigate(Target {
                dest_hash: [0xcd; 16],
                path: DEFAULT_PATH.to_string(),
                fields: Vec::new(),
                is_file: false,
            })],
            "the click opens the node's default page"
        );
        assert_eq!(fx, expected, "the click activates exactly like Enter");
        assert!(!m.show_places, "activating closes the panel");
    }

    #[test]
    fn places_click_honours_the_scroll_offset() {
        // A tall list in a short terminal, scrolled to the bottom so the first
        // visible row is well down the list. A click on that top row must select
        // the entry actually drawn there, not the entry the row would hold at
        // scroll 0.
        let mut m = with_nodes((80, 8), 20);
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        assert!(m.places_scroll > 0, "the list must be scrolled");

        let layout = places_layout(&m);
        let vp = places_viewport(&m, layout.total);
        let offset = m.places_scroll.min(layout.total.saturating_sub(vp));
        let top_idx = layout
            .entry_line
            .iter()
            .position(|&l| l == offset)
            .expect("an entry sits on the first visible line");
        assert!(top_idx > 0, "the scrolled-in entry is not the first entry");

        let Place::Node { dest_hash, .. } = places(&m)[top_idx].clone() else {
            panic!("the scrolled-in entry is a discovered node");
        };
        let overlay = places_overlay_rect(&m, m.frame_area());
        let fx = update(&mut m, AppEvent::Mouse(click(overlay.x + 2, overlay.y + 1)));
        assert_eq!(
            m.places_sel, top_idx,
            "the hit-test maps the top screen row through the scroll offset"
        );
        assert_eq!(
            fx,
            vec![Effect::Navigate(Target {
                dest_hash,
                path: DEFAULT_PATH.to_string(),
                fields: Vec::new(),
                is_file: false,
            })],
            "it activates the entry actually drawn on that row"
        );
    }

    #[test]
    fn places_click_on_a_title_or_blank_does_nothing() {
        let mut m = places_model();
        let overlay = places_overlay_rect(&m, m.frame_area());
        let sel_before = m.places_sel;
        // Row 0 inside the panel is the "Bookmarks" section title, never an entry.
        let fx = update(&mut m, AppEvent::Mouse(click(overlay.x + 2, overlay.y + 1)));
        assert!(fx.is_empty(), "a click on a section title does nothing");
        assert!(m.show_places, "the panel stays open");
        assert_eq!(m.places_sel, sel_before, "the selection is unchanged");
        assert!(place_at(&m, overlay.x + 2, overlay.y + 1).is_none());

        // The blank spacer line between the two sections is likewise inert.
        let blank = places_layout(&m).entry_line[0] + 1; // line after the sole bookmark
        let fx = update(
            &mut m,
            AppEvent::Mouse(click(overlay.x + 2, overlay.y + 1 + blank as u16)),
        );
        assert!(fx.is_empty(), "a click on a blank line does nothing");
        assert!(m.show_places);
    }

    #[test]
    fn places_click_outside_the_panel_closes_it() {
        let mut m = places_model();
        let overlay = places_overlay_rect(&m, m.frame_area());
        // A point above and left of the panel is outside it (and would otherwise
        // reach the top bar underneath).
        assert!(overlay.y > 0, "there is room above the panel");
        let fx = update(&mut m, AppEvent::Mouse(click(0, 0)));
        assert!(fx.is_empty(), "closing the panel follows no link");
        assert!(!m.show_places, "a click outside closes the panel");
    }

    #[test]
    fn places_hover_highlights_an_entry_and_clears_off_it() {
        let mut m = places_model();
        let (col, node_row) = (place_col(&m), place_row(&m, 1));
        update(&mut m, AppEvent::Mouse(moved(col, node_row)));
        assert_eq!(m.places_hover, Some(1), "moving over an entry hovers it");

        // The hovered (non-selected) row is bold + underlined, distinct from the
        // selected row's reverse video.
        let buffer = render(&m, 80, 24);
        let hovered = &buffer[(col, node_row)];
        assert!(
            hovered.modifier.contains(Modifier::UNDERLINED)
                && hovered.modifier.contains(Modifier::BOLD),
            "the hovered row is bold + underlined: {:?}",
            hovered.modifier
        );
        assert!(
            !hovered.modifier.contains(Modifier::REVERSED),
            "the hover highlight is distinct from the selection's reverse video"
        );
        let sel_row = place_row(&m, 0);
        let selected = &buffer[(col, sel_row)];
        assert!(
            selected.modifier.contains(Modifier::REVERSED),
            "the selected row keeps its reverse-video highlight"
        );

        // Moving onto the section title (not an entry) clears the hover.
        let overlay = places_overlay_rect(&m, m.frame_area());
        update(&mut m, AppEvent::Mouse(moved(overlay.x + 2, overlay.y + 1)));
        assert_eq!(m.places_hover, None, "moving off an entry clears the hover");
    }

    #[test]
    fn places_scrollbar_shows_only_when_the_list_overflows() {
        // Wide + short: the panel overflows, so the scrollbar renders.
        let mut over = with_nodes((120, 8), 20);
        press(&mut over, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(
            overlay_scrollbar_shown(&render(&over, 120, 8)),
            "a scrollbar renders while the list overflows"
        );
        // Tall: the whole list fits, so there is no scrollbar.
        let mut fits = with_nodes((120, 40), 3);
        press(&mut fits, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(
            !overlay_scrollbar_shown(&render(&fits, 120, 40)),
            "no scrollbar when the list fits the terminal"
        );
    }

    #[test]
    fn overlays_never_draw_outside_their_rect_on_a_tiny_terminal() {
        // A 4x3 terminal is far smaller than either overlay's natural size;
        // rendering must clamp to the rect and never panic.
        let mut m = with_nodes((4, 3), 10);
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let _ = render(&m, 4, 3);
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        let _ = render(&m, 4, 3);
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        let _ = render(&m, 4, 3);
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        let _ = render(&m, 4, 3);
    }

    #[test]
    fn places_panel_deletes_selected_bookmark_and_persists() {
        let mut m = loaded_model((80, 24));
        m.bookmarks.add(Bookmark {
            url: "aa11:/page/index.mu".to_string(),
            title: "Home".to_string(),
        });
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        // `x` removes the selected bookmark and asks to persist.
        let fx = press(&mut m, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(m.bookmarks.is_empty());
        assert_eq!(fx, vec![Effect::SaveBookmarks]);
    }

    #[test]
    fn x_deletes_selected_bookmark_with_title_toast() {
        let mut m = bookmarks_model();
        let fx = press(&mut m, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(fx, vec![Effect::SaveBookmarks]);
        assert_eq!(bookmark_titles(&m), ["Beta", "Gamma"]);
        // The toast names the TITLE (not the URL) and carries the undo hint.
        let toast = m.toast.clone().expect("a toast is shown");
        assert_eq!(toast.text, "removed Alpha — u to undo");

        // An untitled bookmark falls back to its URL in the toast.
        let mut m = loaded_model((80, 24));
        m.bookmarks.add(Bookmark {
            url: "aa11:/page/index.mu".to_string(),
            title: String::new(),
        });
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut m, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(
            m.toast.clone().expect("a toast is shown").text,
            "removed aa11:/page/index.mu — u to undo"
        );
    }

    #[test]
    fn delete_key_deletes_like_x() {
        let mut via_x = bookmarks_model();
        let mut via_del = bookmarks_model();
        press(&mut via_x, KeyCode::Char('j'), KeyModifiers::NONE);
        press(&mut via_del, KeyCode::Char('j'), KeyModifiers::NONE);
        let fx_x = press(&mut via_x, KeyCode::Char('x'), KeyModifiers::NONE);
        let fx_del = press(&mut via_del, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(fx_del, fx_x, "Delete emits the same effects as x");
        assert_eq!(fx_del, vec![Effect::SaveBookmarks]);
        assert_eq!(bookmark_titles(&via_del), bookmark_titles(&via_x));
        assert_eq!(bookmark_titles(&via_del), ["Alpha", "Gamma"]);
        assert_eq!(via_del.toast, via_x.toast, "same toast either way");
    }

    #[test]
    fn u_restores_the_last_deleted_bookmark() {
        let mut m = bookmarks_model();
        // Delete the middle bookmark ...
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        press(&mut m, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(bookmark_titles(&m), ["Alpha", "Gamma"]);
        // ... and `u` puts it back exactly where it was.
        let fx = press(&mut m, KeyCode::Char('u'), KeyModifiers::NONE);
        assert_eq!(fx, vec![Effect::SaveBookmarks]);
        assert_eq!(
            bookmark_titles(&m),
            ["Alpha", "Beta", "Gamma"],
            "restored in place"
        );
        assert_eq!(m.places_sel, 1, "the restored row is selected");
        assert_eq!(
            m.toast.clone().expect("a toast is shown").text,
            "restored Beta"
        );

        // A second `u` has nothing left to undo: no save, list unchanged.
        let fx = press(&mut m, KeyCode::Char('u'), KeyModifiers::NONE);
        assert!(fx.is_empty(), "an empty undo slot saves nothing");
        assert_eq!(bookmark_titles(&m), ["Alpha", "Beta", "Gamma"]);

        // A new deletion replaces the one slot: after deleting Alpha and then
        // Gamma, `u` restores only Gamma (the newer); Alpha stays gone.
        press(&mut m, KeyCode::Char('g'), KeyModifiers::NONE);
        press(&mut m, KeyCode::Char('x'), KeyModifiers::NONE);
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        press(&mut m, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(bookmark_titles(&m), ["Beta"]);
        press(&mut m, KeyCode::Char('u'), KeyModifiers::NONE);
        assert_eq!(
            bookmark_titles(&m),
            ["Beta", "Gamma"],
            "u restores the newer deletion only"
        );
    }

    #[test]
    fn x_marker_shows_only_on_selected_or_hovered_bookmark_row() {
        let mut m = bookmarks_model();
        let x_col = place_x_screen_col(&m).expect("the panel reserves an × cell");
        // Hover the third bookmark row while the first stays selected.
        let (col, row) = (place_col(&m), place_row(&m, 2));
        update(&mut m, AppEvent::Mouse(moved(col, row)));
        assert_eq!(m.places_hover, Some(2));
        let buffer = render(&m, 80, 24);
        let sym = |idx: usize| buffer[(x_col, place_row(&m, idx))].symbol().to_string();
        assert_eq!(sym(0), "×", "the selected bookmark row shows the marker");
        assert_eq!(sym(2), "×", "a hovered bookmark row shows the marker");
        assert_eq!(sym(1), " ", "an unmarked bookmark row shows none");
        assert_eq!(sym(3), " ", "a node row shows none");

        // Even a selected AND hovered node row shows no marker.
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        let (col, row) = (place_col(&m), place_row(&m, 3));
        update(&mut m, AppEvent::Mouse(moved(col, row)));
        assert_eq!((m.places_sel, m.places_hover), (3, Some(3)));
        let buffer = render(&m, 80, 24);
        assert_eq!(
            buffer[(x_col, place_row(&m, 3))].symbol(),
            " ",
            "a selected + hovered node row shows no ×"
        );
    }

    #[test]
    fn click_on_x_deletes_that_bookmark() {
        let mut m = bookmarks_model();
        let x_col = place_x_screen_col(&m).expect("the panel reserves an × cell");
        // Hover then click the × on the second bookmark row: THAT bookmark goes
        // (not the selected one), the panel stays open, nothing navigates.
        let row = place_row(&m, 1);
        update(&mut m, AppEvent::Mouse(moved(x_col, row)));
        assert_eq!(m.places_hover, Some(1));
        let fx = update(&mut m, AppEvent::Mouse(click(x_col, row)));
        assert_eq!(fx, vec![Effect::SaveBookmarks]);
        assert_eq!(bookmark_titles(&m), ["Alpha", "Gamma"]);
        assert!(m.show_places, "deleting keeps the panel open");

        // A click elsewhere on the row keeps the select + activate behaviour.
        let mut m = places_model();
        let (col, row) = (place_col(&m), place_row(&m, 0));
        let fx = update(&mut m, AppEvent::Mouse(click(col, row)));
        assert!(
            matches!(fx.as_slice(), [Effect::Navigate(_)]),
            "a non-× click on a bookmark row still activates it: {fx:?}"
        );
        assert!(!m.show_places, "activating closes the panel");
        assert_eq!(m.bookmarks.len(), 1, "nothing was deleted");
    }

    #[test]
    fn node_row_has_no_x_and_is_not_deletable() {
        let mut m = bookmarks_model();
        let node_idx = 3;
        // Select the node row; x and Delete are no-ops: nothing removed, no
        // save, no toast, no undo slot.
        press(&mut m, KeyCode::Char('G'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, node_idx);
        assert!(press(&mut m, KeyCode::Char('x'), KeyModifiers::NONE).is_empty());
        assert!(press(&mut m, KeyCode::Delete, KeyModifiers::NONE).is_empty());
        assert_eq!(bookmark_titles(&m), ["Alpha", "Beta", "Gamma"]);
        assert_eq!(places(&m).len(), 4, "the node is still listed");
        assert!(m.toast.is_none(), "a node-row delete shows no toast");
        assert!(m.last_deleted_bookmark.is_none(), "no undo slot is written");

        // A click on the node row's would-be × cell (selected AND hovered) is
        // inert: no delete, no activation, the panel stays open.
        let x_col = place_x_screen_col(&m).expect("the panel reserves an × cell");
        let row = place_row(&m, node_idx);
        update(&mut m, AppEvent::Mouse(moved(x_col, row)));
        assert_eq!(m.places_hover, Some(node_idx));
        let fx = update(&mut m, AppEvent::Mouse(click(x_col, row)));
        assert!(fx.is_empty(), "the × column of a node row does nothing");
        assert!(m.show_places, "the panel stays open");
        assert_eq!(places(&m).len(), 4, "nothing was deleted");
    }

    #[test]
    fn discovery_event_feeds_the_places_panel() {
        let mut m = loaded_model((80, 24));
        update(&mut m, AppEvent::NodeDiscovered(disc_node(0xcd, "Node-C")));
        let ps = places(&m);
        assert_eq!(ps.len(), 1);
        assert!(matches!(ps[0], Place::Node { .. }));
    }

    #[test]
    fn discovery_channel_node_reaches_the_model() {
        // The background discovery task forwards each node onto a channel whose
        // receiver is the loop's fourth select arm. Drive that channel directly
        // (no real RNS): a node sent on it is received and folded into the model,
        // and the places panel then lists it — exactly what the arm does.
        let mut m = loaded_model((80, 24));
        let (node_tx, mut node_rx) = mpsc::unbounded_channel::<DiscoveredNode>();
        node_tx.send(disc_node(0xab, "Node-A")).expect("send node");
        let node = node_rx.try_recv().expect("node delivered on channel");
        update(&mut m, AppEvent::NodeDiscovered(node));
        assert!(
            m.node_registry.get_by_hash(&[0xab; 16]).is_some(),
            "node folded into the model registry"
        );
        let ps = places(&m);
        assert_eq!(ps.len(), 1);
        assert!(matches!(ps[0], Place::Node { .. }));
    }

    /// Assert the places panel's right border on `row` is intact and no
    /// reversed selected-bar cell reaches or passes it (the overflow that
    /// truncation fixes). Panel width 72 centred in 80 cols → x=4, border at 75.
    fn assert_places_border_intact(buffer: &ratatui::buffer::Buffer, row: u16) {
        let width = 72u16;
        let x = (80 - width) / 2;
        let right_border = x + width - 1;
        assert_eq!(
            buffer[(right_border, row)].symbol(),
            "│",
            "right border overwritten"
        );
        for col in right_border..buffer.area.width {
            assert!(
                !buffer[(col, row)].modifier.contains(Modifier::REVERSED),
                "selected bar spilled to column {col} on row {row}"
            );
        }
    }

    /// The row carrying the truncation ellipsis, or panics.
    fn ellipsis_row(buffer: &ratatui::buffer::Buffer) -> u16 {
        let mut row = None;
        for y in 0..buffer.area.height {
            if row_text(buffer, y, 80).contains('…') {
                row = Some(y);
            }
        }
        row.expect("a truncated bookmark row with an ellipsis")
    }

    #[test]
    fn places_row_truncates_and_keeps_border_intact() {
        let mut m = loaded_model((80, 24));
        // A bookmark whose label (title + url + emoji) far exceeds the panel's
        // inner width, so the selected row must be truncated with an ellipsis.
        // The title carries a VS16 emoji-presentation sequence (☯️ = U+262F +
        // U+FE0F), which unicode-width under-counts as one column: with the old
        // plain-width truncation the row spilled past the border, so this test
        // would have caught the bug.
        m.bookmarks.add(Bookmark {
            url: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:/page/a-very-long-page-path.mu".to_string(),
            title: "☯️ an extremely long bookmark title that cannot fit inside the panel"
                .to_string(),
        });
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 0, "the long bookmark is the selected row");
        let buffer = render(&m, 80, 24);
        assert_places_border_intact(&buffer, ellipsis_row(&buffer));
    }

    #[test]
    fn places_row_real_vs16_title_stays_inside_border() {
        let mut m = loaded_model((80, 24));
        // The exact real-world bookmark that overflowed: two emoji-presentation
        // sequences (☯️ and ☸️, each base + U+FE0F). unicode-width scores each
        // as one column, the terminal paints two, so the row shifted two
        // columns right, past the panel's right border.
        m.bookmarks.add(Bookmark {
            url: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:/page/index.mu".to_string(),
            title: "☯️  Zen of Reticulum ☸️".to_string(),
        });
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(m.places_sel, 0, "the long bookmark is the selected row");
        let buffer = render(&m, 80, 24);
        assert_places_border_intact(&buffer, ellipsis_row(&buffer));
    }

    #[test]
    fn places_row_ordinary_wide_emoji_still_truncates() {
        // Regression guard: rows with ordinary wide emoji (🌐, 📰) that
        // unicode-width already scores as two columns keep truncating cleanly.
        for title in [
            "🌐 a very long bookmark title that overflows the places panel border",
            "📰 a very long bookmark title that overflows the places panel border",
        ] {
            let mut m = loaded_model((80, 24));
            m.bookmarks.add(Bookmark {
                url: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:/page/a-very-long-page-path.mu".to_string(),
                title: title.to_string(),
            });
            press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
            let buffer = render(&m, 80, 24);
            assert_places_border_intact(&buffer, ellipsis_row(&buffer));
        }
    }

    #[test]
    fn places_panel_renders_both_sections() {
        let mut m = loaded_model((80, 24));
        m.bookmarks.add(Bookmark {
            url: "aa11:/page/index.mu".to_string(),
            title: "Home".to_string(),
        });
        m.node_registry.upsert_node(&disc_node(0xcd, "Node-C"));
        press(&mut m, KeyCode::Char('d'), KeyModifiers::NONE);
        let buffer = render(&m, 80, 24);
        let text = flat(&buffer);
        assert!(text.contains("Bookmarks"), "missing header:\n{text}");
        assert!(text.contains("Pages heard"), "missing header:\n{text}");
        assert!(text.contains("Home"), "missing bookmark:\n{text}");
        assert!(text.contains("Node-C"), "missing node:\n{text}");
    }

    // --- Phase 7: editable form fields + submit -------------------------

    const HASH_HEX: &str = "0123456789abcdef0123456789abcdef";

    /// A loaded model from a micron source with a current destination set, so
    /// same-destination links and field submission resolve.
    fn field_model(src: &str, size: (u16, u16)) -> Model {
        let mut m = Model::from_document(&parse(src), content_width(size.0), "T", size);
        m.history.visit(tgt(0xab));
        m.current_dest = Some([0xab; 16]);
        m
    }

    #[test]
    fn field_store_inits_from_prefill() {
        let src = "Name: `<name`bob>\nAgree: `<?|agree`Agree>";
        let m = field_model(src, (80, 24));
        assert_eq!(m.field_state.len(), 2);
        assert_eq!(m.field_state[0].name, "name");
        assert_eq!(m.field_state[0].kind, FieldKind::Text);
        assert_eq!(m.field_state[0].input.value(), "bob");
        assert_eq!(m.field_state[1].name, "agree");
        assert_eq!(m.field_state[1].kind, FieldKind::Checkbox);
        assert!(!m.field_state[1].checked, "checkbox starts unchecked");
    }

    #[test]
    fn typing_edits_focused_text_field() {
        let src = "Name: `<name`bo>";
        let mut m = field_model(src, (80, 24));
        // Tab focuses the only interactive element, the text field.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.field_focus, Some(1));
        assert_eq!(m.mode, Mode::Field);
        // The editor starts with the cursor at the end of "bo"; typing appends.
        type_str(&mut m, "b");
        assert_eq!(m.field_state[0].input.value(), "bob");
        // The re-layout reflects the new value in the laid-out widget.
        let field_line: String = m.page[m.field_defs[0].line]
            .cells
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            field_line.contains("bob"),
            "widget not updated: {field_line:?}"
        );
        // The widget keeps its declared width while editing; typing fills it in
        // place rather than growing the box.
        let vf = visible_fields(&m)[0].clone();
        assert_eq!((vf.col_end - vf.col_start) as usize, 24);
    }

    #[test]
    fn long_text_scrolls_inside_the_field_box() {
        // An 8-wide field typed full of 12 characters: nothing may be lost, and
        // the box must slide so the caret — and the tail being typed — stay in
        // view. This is what makes long chat messages enterable.
        let src = "`<8|msg`>";
        let mut m = field_model(src, (80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.field_focus, Some(1));
        type_str(&mut m, "abcdefghijkl");
        assert_eq!(
            m.field_state[0].input.value(),
            "abcdefghijkl",
            "the editor must keep the whole text, not just what fits"
        );

        let vf = visible_fields(&m)[0].clone();
        assert_eq!(
            (vf.col_end - vf.col_start) as usize,
            8,
            "box keeps its width"
        );

        let line: String = m.page[m.field_defs[0].line]
            .cells
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            line.contains("fghijkl"),
            "box did not scroll to the caret: {line:?}"
        );
        assert!(
            !line.contains("abcde"),
            "box still shows the head, so the caret is off-screen: {line:?}"
        );
    }

    #[test]
    fn space_toggles_focused_checkbox() {
        let src = "Agree: `<?|agree`Agree>";
        let mut m = field_model(src, (80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.field_focus, Some(1));
        assert!(!m.field_state[0].checked);
        press(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(m.field_state[0].checked, "Space should check the box");
        press(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!m.field_state[0].checked, "Space should uncheck it again");
    }

    #[test]
    fn space_selects_radio_and_deselects_group_siblings() {
        // Two radios sharing the name "col": selecting one deselects the other.
        let src = "`<^|col`Red>`<^|col`Blue>";
        let mut m = field_model(src, (80, 24));
        assert_eq!(m.field_state.len(), 2);
        // Focus + select the first radio.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(m.field_state[0].checked);
        assert!(!m.field_state[1].checked);
        // Move to the second and select it: the first must clear.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!m.field_state[0].checked, "sibling radio not cleared");
        assert!(m.field_state[1].checked);
    }

    #[test]
    fn click_toggles_checkbox() {
        let src = "Agree: `<?|agree`Agree>";
        let mut m = field_model(src, (80, 24));
        let vf = visible_fields(&m)[0].clone();
        update(&mut m, AppEvent::Mouse(click(vf.col_start, vf.row)));
        assert!(m.field_state[0].checked, "click should check the box");
        let vf = visible_fields(&m)[0].clone();
        update(&mut m, AppEvent::Mouse(click(vf.col_start, vf.row)));
        assert!(!m.field_state[0].checked, "second click should uncheck it");
    }

    #[test]
    fn click_selects_radio() {
        // Two radios sharing the name "col": clicking one selects it and clears
        // its sibling; clicking the selected one keeps it selected (a radio can
        // only be unselected by selecting another in its group).
        let src = "`<^|col`Red>`<^|col`Blue>";
        let mut m = field_model(src, (80, 24));
        let vf = visible_fields(&m)[0].clone();
        update(&mut m, AppEvent::Mouse(click(vf.col_start, vf.row)));
        assert!(m.field_state[0].checked, "click should select the radio");
        assert!(!m.field_state[1].checked);
        let vf = visible_fields(&m)[1].clone();
        update(&mut m, AppEvent::Mouse(click(vf.col_start, vf.row)));
        assert!(!m.field_state[0].checked, "sibling radio not cleared");
        assert!(m.field_state[1].checked);
        let vf = visible_fields(&m)[1].clone();
        update(&mut m, AppEvent::Mouse(click(vf.col_start, vf.row)));
        assert!(
            m.field_state[1].checked,
            "clicking the selected radio must keep it selected"
        );
    }

    #[test]
    fn click_on_text_field_only_focuses() {
        let src = "Name: `<name`bob>";
        let mut m = field_model(src, (80, 24));
        let vf = visible_fields(&m)[0].clone();
        update(&mut m, AppEvent::Mouse(click(vf.col_start, vf.row)));
        assert_eq!(m.mode, Mode::Field);
        assert_eq!(m.field_focus, Some(1));
        assert_eq!(
            m.field_state[0].input.value(),
            "bob",
            "a click must not edit the text"
        );
    }

    /// Enter hint mode and type the label of field `fi`'s hint badge.
    fn activate_field_hint(m: &mut Model, fi: usize) {
        press(m, KeyCode::Char('f'), KeyModifiers::NONE);
        assert_eq!(m.mode, Mode::Hint);
        let label = hints(m)
            .into_iter()
            .find(|h| h.target == HintTarget::Field(fi))
            .expect("field hint present")
            .label;
        for c in label.chars() {
            press(m, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    #[test]
    fn hints_include_visible_fields() {
        let src = "`[Alpha`:/page/a.mu]\nAgree: `<?|agree`Agree>\nName: `<name`bob>";
        let m = field_model(src, (80, 24));
        let fields = visible_fields(&m);
        assert_eq!(fields.len(), 2);
        let hs = hints(&m);
        for vf in fields {
            let hint = hs
                .iter()
                .find(|h| h.target == HintTarget::Field(vf.index))
                .expect("every visible field gets a hint");
            assert_eq!((hint.row, hint.col), (vf.row, vf.col_start));
        }
    }

    #[test]
    fn hint_labels_cover_links_fields_and_controls() {
        let src = "`[Alpha`:/page/a.mu]\nAgree: `<?|agree`Agree>\nName: `<name`bob>";
        let m = field_model(src, (80, 24));
        assert_eq!(visible_links(&m).len(), 1);
        assert_eq!(visible_fields(&m).len(), 2);
        let [topbar, _content, _status] = regions(m.frame_area());
        let expected =
            visible_links(&m).len() + visible_fields(&m).len() + top_bar_controls(&m, topbar).len();
        let hs = hints(&m);
        assert_eq!(hs.len(), expected, "no target dropped or double-counted");
        let labels: std::collections::HashSet<_> = hs.iter().map(|h| &h.label).collect();
        assert_eq!(labels.len(), hs.len(), "labels must be unique");
    }

    #[test]
    fn hint_activate_checkbox_toggles() {
        let src = "Agree: `<?|agree`Agree>";
        let mut m = field_model(src, (80, 24));
        activate_field_hint(&mut m, 1);
        assert!(m.field_state[0].checked, "hint should check the box");
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        activate_field_hint(&mut m, 1);
        assert!(!m.field_state[0].checked, "second hint should uncheck it");
    }

    #[test]
    fn hint_activate_radio_selects() {
        // Two radios sharing the name "col": hinting one selects it and clears
        // its group sibling, same as a click.
        let src = "`<^|col`Red>`<^|col`Blue>";
        let mut m = field_model(src, (80, 24));
        activate_field_hint(&mut m, 1);
        assert!(m.field_state[0].checked, "hint should select the radio");
        assert!(!m.field_state[1].checked);
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        activate_field_hint(&mut m, 2);
        assert!(!m.field_state[0].checked, "sibling radio not cleared");
        assert!(m.field_state[1].checked);
    }

    #[test]
    fn hint_activate_text_field_focuses() {
        let src = "Name: `<name`bob>";
        let mut m = field_model(src, (80, 24));
        activate_field_hint(&mut m, 1);
        assert_eq!(m.mode, Mode::Field, "text field hint enters field editing");
        assert_eq!(m.field_focus, Some(1));
        assert_eq!(
            m.field_state[0].input.value(),
            "bob",
            "a hint must not edit the text"
        );
    }

    #[test]
    fn esc_leaves_field_editing() {
        let src = "Name: `<name`>";
        let mut m = field_model(src, (80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(m.mode, Mode::Field);
        press(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(m.mode, Mode::Browse);
        assert_eq!(m.field_focus, None);
    }

    #[test]
    fn visible_fields_positions_after_wide_char() {
        // A width-2 emoji then a space, then the field: the widget starts at
        // display column 3 (2 + 1), not character index 2.
        let src = "\u{1f389} `<name`>";
        let m = field_model(src, (80, 24));
        let vf = visible_fields(&m);
        assert_eq!(vf.len(), 1);
        assert_eq!(vf[0].row, TOPBAR_ROWS);
        assert_eq!(vf[0].col_start, 3, "field mispositioned after wide char");
        // An empty text field still spans its declared micron width (default 24),
        // like NomadNet's full-width input bar — not the width of its content.
        assert_eq!(vf[0].col_end, 3 + 24);
    }

    #[test]
    fn text_field_box_spans_declared_width_not_content() {
        // Default width (24) with a prefill, an explicit width, and an empty
        // field: the widget is always exactly `width` columns wide.
        for (src, width) in [
            ("`<name`Guest>", 24usize),
            ("`<12|nick`Guest>", 12),
            ("`<40|msg`>", 40),
        ] {
            let m = field_model(src, (120, 24));
            let vf = visible_fields(&m);
            assert_eq!(vf.len(), 1, "one field in {src:?}");
            assert_eq!(
                (vf[0].col_end - vf[0].col_start) as usize,
                width,
                "field box in {src:?} must span its declared width {width}"
            );
        }
    }

    #[test]
    fn submit_link_carries_collected_field_values() {
        // A page with a text field "name" (prefill "bob") and a submit link that
        // references it plus a `k=v` preset.
        let src = format!("`<name`bob>\n`[Go`{HASH_HEX}:/page/s.mu`name|a=1]");
        let mut m = field_model(&src, (80, 24));
        let effects = follow_link(&mut m, 1);
        let target = match effects.as_slice() {
            [Effect::Navigate(t)] => t.clone(),
            other => panic!("expected a single Navigate, got {other:?}"),
        };
        assert_eq!(target.path, "/page/s.mu");
        // The preset lands as var_a; the collected field value as field_name.
        assert!(
            target
                .fields
                .contains(&("var_a".to_string(), "1".to_string())),
            "preset missing: {:?}",
            target.fields
        );
        assert!(
            target
                .fields
                .contains(&("field_name".to_string(), "bob".to_string())),
            "collected field missing: {:?}",
            target.fields
        );
    }

    #[test]
    fn submit_link_reflects_edited_value_and_ignores_unreferenced_fields() {
        // Two text fields; the link references only "q". Editing "q" must flow
        // through, and the unreferenced "other" field must not be submitted.
        let src = format!("`<q`hi>`<other`x>\n`[Go`{HASH_HEX}:/page/s.mu`q]");
        let mut m = field_model(&src, (80, 24));
        // Focus the first field (q) and append "!".
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        type_str(&mut m, "!");
        assert_eq!(m.field_state[0].input.value(), "hi!");
        let link = m.links.iter().find(|l| l.index == 1).cloned().unwrap();
        let collected = collect_submit_fields(&m, &link);
        assert_eq!(
            collected,
            vec![("field_q".to_string(), "hi!".to_string())],
            "only the referenced, edited field should be collected"
        );
    }

    #[test]
    fn submit_link_star_collects_all_fields() {
        let src = format!("`<a`1>`<b`2>\n`[Go`{HASH_HEX}:/page/s.mu`*]");
        let m = field_model(&src, (80, 24));
        let link = m.links.iter().find(|l| l.index == 1).cloned().unwrap();
        let collected = collect_submit_fields(&m, &link);
        assert_eq!(
            collected,
            vec![
                ("field_a".to_string(), "1".to_string()),
                ("field_b".to_string(), "2".to_string()),
            ],
            "`*` should collect every field"
        );
    }

    #[test]
    fn unchecked_checkbox_is_not_submitted() {
        let src = format!("`<?|agree`Agree>\n`[Go`{HASH_HEX}:/page/s.mu`agree]");
        let mut m = field_model(&src, (80, 24));
        let link = m.links.iter().find(|l| l.index == 1).cloned().unwrap();
        // Unchecked: nothing collected.
        assert!(collect_submit_fields(&m, &link).is_empty());
        // Check it (focus the checkbox, Space), then it submits its value.
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        let collected = collect_submit_fields(&m, &link);
        assert_eq!(
            collected,
            vec![("field_agree".to_string(), "Agree".to_string())],
            "a checked box submits its value (the label when none given)"
        );
    }

    #[test]
    fn mouse_click_on_field_focuses_it() {
        let src = "Name: `<name`>";
        let mut m = field_model(src, (80, 24));
        let vf = visible_fields(&m)[0].clone();
        let effects = update(&mut m, AppEvent::Mouse(click(vf.col_start, vf.row)));
        assert!(effects.is_empty(), "focusing a field yields no effect");
        assert_eq!(m.field_focus, Some(1));
        assert_eq!(m.mode, Mode::Field);
    }

    #[test]
    fn field_state_survives_resize() {
        let src = "Name: `<name`>";
        let mut m = field_model(src, (80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        type_str(&mut m, "kept");
        assert_eq!(m.field_state[0].input.value(), "kept");
        update(&mut m, AppEvent::Resize(60, 20));
        assert_eq!(
            m.field_state[0].input.value(),
            "kept",
            "a resize must not reset the field store"
        );
    }

    // --- Phase 7: TestBackend rendering ---------------------------------

    #[test]
    fn input_field_renders_its_value_in_a_box() {
        let src = "Name: `<name`bob>";
        let m = field_model(src, (80, 24));
        let buffer = render(&m, 80, 24);
        assert!(flat(&buffer).contains("bob"), "field value not drawn");
        let vf = visible_fields(&m)[0].clone();
        // The box spans the field's DECLARED width (default 24), not "bob" (3).
        assert_eq!((vf.col_end - vf.col_start) as usize, 24);
        // Every cell of the box — including the padding past the text — carries
        // the input-box background, so the field's full extent is visible, the
        // way NomadNet draws an input bar.
        let want = Color::Rgb(
            Theme::Dark.chrome_bg().0,
            Theme::Dark.chrome_bg().1,
            Theme::Dark.chrome_bg().2,
        );
        for x in vf.col_start..vf.col_end {
            assert_eq!(
                buffer[(x, vf.row)].bg,
                want,
                "input box not filled at column {x}"
            );
        }
    }

    #[test]
    fn focused_field_is_highlighted() {
        let src = "Name: `<name`bob>";
        let mut m = field_model(src, (80, 24));
        press(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        let vf = visible_fields(&m)[0].clone();
        let buffer = render(&m, 80, 24);
        let cell = &buffer[(vf.col_start, vf.row)];
        assert!(
            cell.modifier.contains(Modifier::REVERSED),
            "a focused field should be highlighted"
        );
    }

    #[test]
    fn checkbox_renders_its_state() {
        // Prechecked checkbox renders "[x]"; an unchecked one "[ ]".
        let checked = field_model("`<?|a|Yes|*`On>", (80, 24));
        assert!(flat(&render(&checked, 80, 24)).contains("[x] On"));
        let unchecked = field_model("`<?|a`Off>", (80, 24));
        assert!(flat(&render(&unchecked, 80, 24)).contains("[ ] Off"));
    }

    // --- inline images ----------------------------------------------------

    /// The micron a blog page with one picture looks like: a standalone link
    /// to a file in the node's file area, which is the only form micron has.
    const IMAGE_PAGE: &str = "\
>>Antennenbau

Der Mast steht.

`[Antenne`:/file/antenne.png]

Text unter dem Bild.
";

    /// A model showing IMAGE_PAGE, loaded through the ordinary load path so the
    /// image slots are planned exactly as they are in production.
    fn image_model() -> (Model, Vec<Effect>) {
        let mut m = Model {
            size: (80, 24),
            current_dest: Some([0xab; 16]),
            ..Model::default()
        };
        m.relayout(content_width(80));
        let effects = update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse(IMAGE_PAGE),
                title: "Blog".to_string(),
            },
        );
        (m, effects)
    }

    /// The probe a fetched picture would have produced.
    fn probe() -> Probe {
        Probe {
            format: "PNG",
            width: 640,
            height: 320,
            len: 12_400,
        }
    }

    #[test]
    fn a_standalone_image_link_is_planned_and_fetched() {
        let (m, effects) = image_model();
        assert_eq!(m.images.len(), 1, "the picture must get a slot");
        assert_eq!(m.images[0].name, "antenne.png");
        assert_eq!(m.images[0].state, SlotState::Queued);

        let [Effect::FetchImages(requests)] = &effects[..] else {
            panic!("expected one FetchImages effect, got {effects:?}");
        };
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].link, m.images[0].link);
        assert_eq!(requests[0].target.path, "/file/antenne.png");
        assert!(requests[0].target.is_file);
        assert_eq!(
            requests[0].target.dest_hash, [0xab; 16],
            "a `:`-relative link resolves against the page's own node"
        );
    }

    #[test]
    fn a_page_without_pictures_asks_for_nothing() {
        let mut m = model_from_sample(78, (80, 24));
        let effects = update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse(SAMPLE),
                title: "Sample".to_string(),
            },
        );
        assert!(m.images.is_empty());
        assert!(!effects.iter().any(|e| matches!(e, Effect::FetchImages(_))));
    }

    #[test]
    fn images_off_plans_nothing_at_all() {
        let mut m = Model {
            size: (80, 24),
            no_images: true,
            current_dest: Some([0xab; 16]),
            ..Model::default()
        };
        let effects = update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse(IMAGE_PAGE),
                title: "Blog".to_string(),
            },
        );
        assert!(m.images.is_empty(), "--images off must plan no pictures");
        assert!(effects.is_empty(), "and must ask for no transfers");
        // The link is still there and still followable.
        assert_eq!(m.links.len(), 1);
    }

    #[test]
    fn a_page_beyond_the_ceiling_fetches_only_the_first_ones() {
        let mut page = String::new();
        for i in 0..(MAX_INLINE_IMAGES + 3) {
            page.push_str(&format!("`[Bild {i}`:/file/bild{i}.png]\n\n"));
        }
        let mut m = Model {
            size: (80, 40),
            current_dest: Some([0xab; 16]),
            ..Model::default()
        };
        let effects = update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse(&page),
                title: "Viele".to_string(),
            },
        );

        assert_eq!(
            m.images.len(),
            MAX_INLINE_IMAGES + 3,
            "every one gets a slot"
        );
        let fetched = m
            .images
            .iter()
            .filter(|s| s.state == SlotState::Queued)
            .count();
        assert_eq!(fetched, MAX_INLINE_IMAGES);
        let skipped = m
            .images
            .iter()
            .filter(|s| s.state == SlotState::TooMany)
            .count();
        assert_eq!(skipped, 3, "a hostile page cannot start fifty transfers");

        let [Effect::FetchImages(requests)] = &effects[..] else {
            panic!("expected one FetchImages effect, got {effects:?}");
        };
        assert_eq!(requests.len(), MAX_INLINE_IMAGES);
    }

    #[test]
    fn a_ready_picture_reserves_its_rows_and_pushes_the_text_down() {
        let (mut m, _) = image_model();
        let link_line = m.links[0].line;
        let below: Vec<String> = m.page[link_line + 2..]
            .iter()
            .map(|l| l.cells.iter().map(|c| c.ch).collect())
            .collect();

        let link = m.images[0].link;
        update(
            &mut m,
            AppEvent::ImageReady {
                link,
                cells: (40, 6),
                probe: probe(),
            },
        );

        assert_eq!(m.images[0].state.rows(), 6);
        let after: Vec<String> = m.page[link_line + 7..]
            .iter()
            .map(|l| l.cells.iter().map(|c| c.ch).collect())
            .collect();
        assert_eq!(after, below, "the text below must survive, only moved");
    }

    #[test]
    fn a_terminal_that_cannot_draw_gets_a_line_that_says_what_the_picture_is() {
        let (mut m, _) = image_model();
        let link = m.images[0].link;
        update(
            &mut m,
            AppEvent::ImageText {
                link,
                probe: Some(probe()),
                reason: "this terminal shows no images".to_string(),
            },
        );

        let drawn = flat(&render(&m, 80, 24));
        assert!(
            drawn.contains("antenne.png, PNG 640x320, 12.4 kB - this terminal shows no images"),
            "the fallback must say what the file is: {drawn}"
        );
    }

    #[test]
    fn a_failed_transfer_says_so_without_losing_the_link() {
        let (mut m, _) = image_model();
        let link = m.images[0].link;
        update(
            &mut m,
            AppEvent::ImageFailed {
                link,
                reason: "request timed out".to_string(),
            },
        );
        let drawn = flat(&render(&m, 80, 24));
        assert!(drawn.contains("antenne.png: request timed out"), "{drawn}");
        assert!(drawn.contains("Antenne"), "the link itself must remain");
    }

    #[test]
    fn a_queued_picture_shows_that_it_is_coming() {
        let (m, _) = image_model();
        let drawn = flat(&render(&m, 80, 24));
        assert!(drawn.contains("antenne.png: queued"), "{drawn}");
    }

    #[test]
    fn following_an_arrived_picture_saves_it_instead_of_fetching_it_again() {
        let (mut m, _) = image_model();
        let link = m.images[0].link;
        update(
            &mut m,
            AppEvent::ImageReady {
                link,
                cells: (40, 6),
                probe: probe(),
            },
        );
        m.focus = Some(m.images[0].link);

        let effects = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            effects,
            vec![Effect::SaveImage {
                link: m.images[0].link
            }],
            "the bytes are already here; a second transfer would be minutes of airtime"
        );

        let effects = press(&mut m, KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(
            effects,
            vec![Effect::OpenImage {
                link: m.images[0].link
            }]
        );
    }

    #[test]
    fn following_a_picture_that_has_not_arrived_downloads_it() {
        let (mut m, _) = image_model();
        m.focus = Some(m.images[0].link);
        let effects = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        let [Effect::Download(target)] = &effects[..] else {
            panic!("expected the ordinary download path, got {effects:?}");
        };
        assert_eq!(target.path, "/file/antenne.png");
    }

    #[test]
    fn o_does_nothing_without_a_focused_picture() {
        let (mut m, _) = image_model();
        assert!(press(&mut m, KeyCode::Char('o'), KeyModifiers::NONE).is_empty());
    }

    #[test]
    fn navigating_away_drops_the_pictures_of_the_page_left_behind() {
        let (mut m, _) = image_model();
        let link = m.images[0].link;
        update(
            &mut m,
            AppEvent::ImageReady {
                link,
                cells: (40, 6),
                probe: probe(),
            },
        );
        assert_eq!(m.images.len(), 1);

        // A fresh page with no pictures replaces the slots outright, and the
        // shell aborts the queue on the Navigate/FetchImages that come with it.
        update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("Nur Text.\n"),
                title: "Text".to_string(),
            },
        );
        assert!(m.images.is_empty());
    }

    #[test]
    fn a_picture_scrolled_out_of_view_is_not_drawn_over_the_chrome() {
        // The status line for a slot above the viewport must not be painted
        // into the top bar, which is what a naive row calculation would do.
        let (mut m, _) = image_model();
        let link = m.images[0].link;
        update(
            &mut m,
            AppEvent::ImageText {
                link,
                probe: Some(probe()),
                reason: "this terminal shows no images".to_string(),
            },
        );
        m.scroll = m.page.len();
        let buffer = render(&m, 80, 24);
        let top: String = (0..80)
            .map(|x| buffer[(x, 0)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            !top.contains("antenne.png"),
            "a scrolled-away picture must not land on the top bar: {top}"
        );
    }

    /// A tiny picture, encoded so the test data cannot drift from what the
    /// decoder is handed.
    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut img = ::image::RgbaImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = ::image::Rgba([(x * 8 % 256) as u8, (y * 8 % 256) as u8, 200, 255]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        ::image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, ::image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
    }

    /// A store that draws with half-blocks, the fallback every colour terminal
    /// can take. Pinned rather than detected: a test must not depend on which
    /// terminal happens to be running it.
    fn halfblock_store() -> ImageStore {
        ImageStore {
            // Pinned rather than queried: `halfblocks()` fixes both the
            // protocol and the font size (10x20), so the assertions below do
            // not depend on which terminal runs the test.
            pictures: Pictures::new(Some(Picker::halfblocks()), Backend::Halfblocks),
            entries: HashMap::new(),
            task: None,
            generation: 0,
            cache: ImageCache::default(),
        }
    }

    #[test]
    fn a_fetched_picture_is_encoded_and_painted_into_its_reserved_rows() {
        // The whole draw path, end to end: payload -> probe -> decode ->
        // half-block encode -> cells in the frame buffer.
        let (mut m, _) = image_model();
        let link = m.images[0].link;
        let mut store = halfblock_store();

        let event = store.accept(link, "antenne.png".to_string(), png_bytes(64, 32), &m);
        let AppEvent::ImageReady { cells, probe, .. } = &event else {
            panic!("a valid PNG on a half-block terminal must be drawable, got {event:?}");
        };
        assert_eq!((probe.width, probe.height), (64, 32));
        let (cols, rows) = *cells;
        assert!(cols > 0 && rows > 0);
        update(&mut m, event);

        let buffer = render_with_images(&m, &store, 80, 24);
        // Page line -> screen row: the content region starts below the top bar,
        // and the picture's rows start one line under its link.
        let content = regions(Rect::new(0, 0, 80, 24))[1];
        let first = content.y + (m.links[0].line + 1 - m.scroll) as u16;
        let left = content.x + m.links[0].col_start as u16;
        let painted = (0..rows)
            .flat_map(|y| (0..cols).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let cell = &buffer[(left + x, first + y)];
                cell.symbol() != " " || cell.bg != Color::Reset
            })
            .count();
        assert_eq!(
            painted,
            (cols * rows) as usize,
            "every reserved cell must carry part of the picture ({cols}x{rows} at \
             row {first}, col {left})"
        );
    }

    #[test]
    fn rubbish_that_claims_to_be_a_picture_becomes_a_line_of_text() {
        // The payload comes from somebody else's node. Undecodable bytes must
        // not panic, and must leave the reader something readable.
        let (mut m, _) = image_model();
        let link = m.images[0].link;
        let mut store = halfblock_store();

        let event = store.accept(link, "antenne.png".to_string(), b"not a png".to_vec(), &m);
        let AppEvent::ImageText { probe, reason, .. } = &event else {
            panic!("undecodable bytes must fall back to text, got {event:?}");
        };
        assert!(probe.is_none());
        assert!(reason.contains("decode"), "{reason}");
        update(&mut m, event);
        assert!(flat(&render_with_images(&m, &store, 80, 24)).contains("antenne.png"));
    }

    #[test]
    fn a_resize_re_encodes_every_held_picture_for_the_new_width() {
        let (mut m, _) = image_model();
        let link = m.images[0].link;
        let mut store = halfblock_store();
        let event = store.accept(link, "antenne.png".to_string(), png_bytes(640, 320), &m);
        let wide = match event {
            AppEvent::ImageReady { cells, .. } => cells,
            other => panic!("expected a drawable picture, got {other:?}"),
        };
        update(&mut m, event);

        update(&mut m, AppEvent::Resize(40, 24));
        let events = store.reencode_all(&m);
        let narrow = match events.first() {
            Some(AppEvent::ImageReady { cells, .. }) => *cells,
            other => panic!("expected a re-encoded picture, got {other:?}"),
        };
        assert!(
            narrow.0 < wide.0,
            "a narrower terminal must draw the picture smaller: {narrow:?} vs {wide:?}"
        );
    }

    #[test]
    fn a_terminal_without_a_picker_reports_that_it_shows_no_images() {
        let (m, _) = image_model();
        let mut store = ImageStore::default();
        let event = store.accept(1, "antenne.png".to_string(), png_bytes(8, 8), &m);
        assert!(matches!(
            event,
            AppEvent::ImageText { reason, .. } if reason.contains("no images")
        ));
    }

    #[test]
    fn every_graphics_protocol_encodes_the_same_payload() {
        // The half-block path is asserted above end to end. This pins that we
        // drive the other three correctly too: each must accept the payload and
        // report a drawable size, since a terminal that answers "Kitty" gets
        // whichever one it named and there is no second chance.
        let (m, _) = image_model();
        for protocol in [
            ProtocolType::Kitty,
            ProtocolType::Iterm2,
            ProtocolType::Sixel,
            ProtocolType::Halfblocks,
        ] {
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(protocol);
            let mut store = ImageStore {
                pictures: Pictures::new(Some(picker), Backend::Graphics(protocol)),
                entries: HashMap::new(),
                task: None,
                generation: 0,
                cache: ImageCache::default(),
            };
            let event = store.accept(1, "antenne.png".to_string(), png_bytes(64, 32), &m);
            match event {
                AppEvent::ImageReady { cells, .. } => {
                    assert!(cells.0 > 0 && cells.1 > 0, "{protocol:?} gave {cells:?}");
                    assert!(
                        store.protocol(1).is_some(),
                        "{protocol:?} must leave something to draw"
                    );
                }
                other => panic!("{protocol:?} must encode a valid PNG, got {other:?}"),
            }
        }
    }

    /// The fetch target the fixture page's picture resolves to.
    fn image_target() -> Target {
        Target {
            dest_hash: [0xab; 16],
            path: "/file/antenne.png".to_string(),
            fields: Vec::new(),
            is_file: true,
        }
    }

    #[test]
    fn a_cached_picture_is_shown_without_a_transfer() {
        // The whole point of the cache: stepping back to a page must not spend
        // the airtime again.
        let (m, effects) = image_model();
        let [Effect::FetchImages(requests)] = &effects[..] else {
            panic!("expected one FetchImages effect, got {effects:?}");
        };
        let mut store = halfblock_store();
        store.cache.insert(image_target(), png_bytes(64, 32));

        let (events, pending) = store.take_cached(requests.clone(), &m);
        assert!(
            pending.is_empty(),
            "nothing may be left to fetch: {pending:?}"
        );
        assert!(
            matches!(events[..], [AppEvent::ImageReady { .. }]),
            "the cached payload must arrive drawable, got {events:?}"
        );
        assert!(store.protocol(requests[0].link).is_some());
    }

    #[test]
    fn an_uncached_picture_still_has_to_be_fetched() {
        let (m, effects) = image_model();
        let [Effect::FetchImages(requests)] = &effects[..] else {
            panic!("expected one FetchImages effect, got {effects:?}");
        };
        let mut store = halfblock_store();
        // A picture from another node under the same name is a different
        // picture, and must not be served from this entry.
        store.cache.insert(
            Target {
                dest_hash: [0x11; 16],
                ..image_target()
            },
            png_bytes(64, 32),
        );

        let (events, pending) = store.take_cached(requests.clone(), &m);
        assert!(events.is_empty());
        assert_eq!(pending.len(), 1);
    }

    /// L-0056: teardown unwraps the session Arc, so every task holding a clone
    /// must be gone first. An image task that is only aborted still holds its
    /// clone until the runtime drops the future; awaiting the handle is what
    /// guarantees the drop.
    #[tokio::test]
    async fn stopping_the_store_releases_the_image_tasks_session_clone() {
        let shared = Arc::new(Mutex::new(()));
        let mut store = ImageStore::default();
        let clone = shared.clone();
        store.task = Some(tokio::spawn(async move {
            let _held = clone;
            std::future::pending::<()>().await;
        }));
        // Let the task start and park, as it would inside a transfer.
        tokio::task::yield_now().await;

        store.stop().await;

        assert!(
            Arc::try_unwrap(shared).is_ok(),
            "the image task still holds the session after teardown"
        );
    }

    #[test]
    fn leaving_a_page_keeps_the_cache_but_drops_the_page_holdings() {
        let (m, _) = image_model();
        let mut store = halfblock_store();
        store.accept(1, "antenne.png".to_string(), png_bytes(64, 32), &m);
        store.cache.insert(image_target(), png_bytes(64, 32));

        store.reset();

        assert!(
            store.protocol(1).is_none(),
            "the page's encoded pictures go with the page"
        );
        assert!(
            store.cache.contains(&image_target()),
            "the cache is what outlives it"
        );
    }

    #[test]
    fn the_cache_budget_comes_from_the_option() {
        let store = ImageStore {
            cache: ImageCache::new(2_000_000),
            ..halfblock_store()
        };
        assert_eq!(store.cache.max_bytes(), 2_000_000);
        // The default budget is what `--image-cache 10` asks for.
        assert_eq!(
            ImageStore::default().cache.max_bytes(),
            crate::image_cache::DEFAULT_MAX_BYTES
        );
    }

    #[test]
    fn a_photo_on_half_blocks_uses_the_page_width_not_the_font_box() {
        // The regression this guards: sizing a half-block picture by the
        // terminal's font box drew a 300x300 portrait as 38x38 half-block
        // pixels — "gigantic pixels", unrecognisable. A cell carries one pixel
        // across and two down, so the picture must instead grow to the page
        // width, and `Resize::Scale` must keep the protocol at that size.
        let (mut m, _) = image_model();
        let mut store = halfblock_store();

        let event = store.accept(1, "portrait.jpg".to_string(), png_bytes(300, 300), &m);
        let AppEvent::ImageReady { cells, .. } = &event else {
            panic!("a valid PNG must be drawable on half-blocks, got {event:?}");
        };
        let (cols, rows) = *cells;
        assert!(
            cols as usize >= m.width - 2,
            "a photo must use the page width on half-blocks, got {cols} of {}",
            m.width
        );
        assert!(
            cols > 38,
            "38 columns is the font-box fit this bug produced; got {cols}"
        );
        // Square source, square picture: the height follows the aspect ratio
        // in half-block geometry (one pixel across, two down).
        assert_eq!(
            rows * 2,
            cols,
            "a square picture must stay square: {cols}x{rows} cells"
        );

        // And the protocol really occupies that area rather than shrinking
        // back to the font box, which is what `Resize::Fit` did. The event has
        // to be folded first: until the slot is Ready, the reserved rows carry
        // the status line, not the picture.
        update(&mut m, event);
        let buffer = render_with_images(&m, &store, 80, 24);
        let content = regions(Rect::new(0, 0, 80, 24))[1];
        let first = content.y + (m.links[0].line + 1 - m.scroll) as u16;
        let left = content.x + m.links[0].col_start as u16;
        let painted = (0..cols)
            .filter(|x| {
                let c = &buffer[(left + x, first)];
                c.symbol() != " " || c.bg != Color::Reset
            })
            .count();
        assert_eq!(
            painted, cols as usize,
            "the picture's first row must be painted across its full width"
        );
    }

    // --- names: search and pins ----------------------------------------

    const MARCEL: [u8; 16] = [
        0xfa, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
        0xcd,
    ];
    const IMPOSTOR: [u8; 16] = [0x9c; 16];

    /// A model with two sites heard: Marcel (pinned) and one wearing his name
    /// under another key, plus an unrelated one.
    fn names_model() -> Model {
        let mut m = loaded_model((90, 24));
        m.clock = Some(10_000);
        m.node_registry
            .upsert(MARCEL, Some("Marcel".to_string()), Some(3), 10_000 - 7200);
        m.node_registry
            .upsert(IMPOSTOR, Some("Marcel".to_string()), Some(1), 10_000 - 30);
        m.node_registry.upsert(
            [0x17; 16],
            Some("Solar Nord".to_string()),
            Some(2),
            10_000 - 600,
        );
        m.pins.note_visit(Some("Marcel"), &MARCEL, 100);
        m
    }

    fn enter_query(m: &mut Model, query: &str) -> Vec<Effect> {
        press(m, KeyCode::Char(':'), KeyModifiers::NONE);
        type_str(m, query);
        press(m, KeyCode::Enter, KeyModifiers::NONE)
    }

    #[test]
    fn an_exact_unambiguous_name_opens_directly() {
        let mut m = names_model();
        let effects = enter_query(&mut m, "solar nord");
        assert_eq!(
            effects,
            vec![Effect::Navigate(crate::app::node_target([0x17; 16]))]
        );
        assert!(m.names.is_none());
        assert_eq!(m.mode, Mode::Browse);
    }

    #[test]
    fn a_shared_name_lists_the_pinned_site_first() {
        let mut m = names_model();
        let effects = enter_query(&mut m, "Marcel");
        assert!(effects.is_empty(), "{effects:?}");
        let results = m.names.as_ref().expect("result list open");
        let dests: Vec<[u8; 16]> = results.hits.iter().map(|h| h.dest).collect();
        assert_eq!(dests, vec![MARCEL, IMPOSTOR]);
        // Enter on the pinned one opens it with no question.
        let effects = press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            effects,
            vec![Effect::Navigate(crate::app::node_target(MARCEL))]
        );
        assert!(m.names.is_none());
    }

    #[test]
    fn the_other_key_asks_once_and_a_yes_pins_it_beside_the_first() {
        let mut m = names_model();
        enter_query(&mut m, "fa01");
        // The short hash finds exactly Marcel's pinned site. It is listed, not
        // opened: only an exact name opens directly.
        let dests: Vec<[u8; 16]> = m
            .names
            .as_ref()
            .unwrap()
            .hits
            .iter()
            .map(|h| h.dest)
            .collect();
        assert_eq!(dests, vec![MARCEL]);

        let mut m = names_model();
        enter_query(&mut m, "marcel");
        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        // Enter on the conflicting row asks instead of opening.
        assert!(press(&mut m, KeyCode::Enter, KeyModifiers::NONE).is_empty());
        assert!(m.names.as_ref().is_some_and(|r| r.confirm));
        // Any key but y returns to the list, nothing pinned.
        assert!(press(&mut m, KeyCode::Char('n'), KeyModifiers::NONE).is_empty());
        assert!(m.names.as_ref().is_some_and(|r| !r.confirm));
        assert_eq!(m.pins.dests_for("Marcel"), vec![MARCEL]);
        // Asked again, a yes pins the second key and opens it.
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        let effects = press(&mut m, KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(
            effects,
            vec![
                Effect::SavePins,
                Effect::Navigate(crate::app::node_target(IMPOSTOR))
            ]
        );
        assert_eq!(m.pins.dests_for("marcel"), vec![MARCEL, IMPOSTOR]);
        assert_eq!(m.pins.list().len(), 2);
    }

    #[test]
    fn a_name_nobody_announced_says_so_and_keeps_the_editor() {
        let mut m = names_model();
        let effects = enter_query(&mut m, "zebra");
        assert!(effects.is_empty());
        let toast = m.toast.as_ref().expect("a note is raised");
        assert_eq!(toast.kind, ToastKind::Info);
        assert!(toast.text.contains("zebra"), "{}", toast.text);
        assert_eq!(m.mode, Mode::Address);
    }

    #[test]
    fn a_fresh_load_pins_the_visited_site_once() {
        let mut m = names_model();
        enter_query(&mut m, "solar nord");
        let effects = update(
            &mut m,
            AppEvent::PageLoaded {
                doc: parse("hi"),
                title: "Solar".to_string(),
            },
        );
        assert_eq!(effects.first(), Some(&Effect::SavePins));
        assert_eq!(m.pins.dests_for("Solar Nord"), vec![[0x17; 16]]);
        let pin = m.pins.list().last().unwrap().clone();
        assert_eq!((pin.first_seen, pin.last_seen), (10_000, 10_000));
    }

    #[test]
    fn a_nururl_opens_and_names_the_ignored_hint() {
        let mut m = names_model();
        let hash = "0123456789abcdef0123456789abcdef";
        let effects = enter_query(
            &mut m,
            &format!("nururl:{hash}/page/x.mu?nn.a=1&hint=marcel"),
        );
        let Some(Effect::Navigate(target)) = effects.first() else {
            panic!("{effects:?}");
        };
        assert_eq!(target.path, "/page/x.mu");
        assert_eq!(target.fields, vec![("var_a".to_string(), "1".to_string())]);
        let toast = m.toast.as_ref().expect("the ignored hint is named");
        assert!(toast.text.contains("hint=marcel"), "{}", toast.text);
    }

    #[test]
    fn the_result_list_renders_its_rows_and_the_question() {
        let mut m = names_model();
        enter_query(&mut m, "marcel");
        let buffer = render(&m, 90, 24);
        let screen: Vec<String> = (0..24).map(|y| row_text(&buffer, y, 90)).collect();
        let joined = screen.join("\n");
        assert!(joined.contains("sites matching \"marcel\""), "{joined}");
        let pinned = screen
            .iter()
            .position(|r| r.contains("Marcel ·fa01  ·  3 hops  ·  2 h ago  pinned"))
            .expect("pinned row");
        let other = screen
            .iter()
            .position(|r| r.contains("Marcel ·9c9c  ·  1 hop  ·  just now  ! name pinned to ·fa01"))
            .expect("conflict row");
        assert_eq!(other, pinned + 1, "{joined}");
        assert!(joined.contains("Enter open"), "{joined}");

        press(&mut m, KeyCode::Char('j'), KeyModifiers::NONE);
        press(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        let buffer = render(&m, 90, 24);
        let joined: String = (0..24)
            .map(|y| row_text(&buffer, y, 90))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("\"Marcel\" is pinned to ·fa01; this is ·9c9c"),
            "{joined}"
        );
    }

    /// Italic text reaches the frame as the italic modifier, and plain text
    /// beside it does not: no other frame test held the italic flag of a run
    /// (pass one's positive control found the gap).
    #[test]
    fn an_italic_run_is_drawn_italic() {
        let m = Model::from_document(
            &parse("`*slanted`* upright"),
            content_width(40),
            "t",
            (40, 6),
        );
        let buffer = render(&m, 40, 6);
        let row = (0..buffer.area.height)
            .find(|&y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .contains("slanted upright")
            })
            .expect("the line is drawn");
        let line: String = (0..buffer.area.width)
            .map(|x| buffer[(x, row)].symbol())
            .collect();
        let at = line.find("slanted").expect("slanted") as u16;
        let up = line.find("upright").expect("upright") as u16;
        for x in at..at + 7 {
            assert!(buffer[(x, row)].modifier.contains(Modifier::ITALIC), "{x}");
        }
        for x in up..up + 7 {
            assert!(!buffer[(x, row)].modifier.contains(Modifier::ITALIC), "{x}");
        }
    }

    /// In German the help, the footer and the start page are German: the
    /// shell may follow the locale because nothing it draws is left English.
    #[test]
    fn the_terminal_speaks_german() {
        let mut m = Model {
            lang: crate::app::strings::Lang::De,
            ..Model::default()
        };
        m.size = (100, 40);
        m.relayout(content_width(100));
        show_start_screen(&mut m);
        m.show_places = false;
        let start = flat(&render(&m, 100, 40));
        assert!(start.contains("Keine Seite offen"), "{start}");
        assert!(start.contains("beenden"), "{start}");
        assert!(!start.contains("No page is open"), "{start}");
        press(&mut m, KeyCode::Char('?'), KeyModifiers::NONE);
        let help = flat(&render(&m, 100, 40));
        assert!(help.contains("Bewegen"), "{help}");
        assert!(help.contains("Hilfe"), "{help}");
        assert!(!help.contains("close this help"), "{help}");
    }
}
