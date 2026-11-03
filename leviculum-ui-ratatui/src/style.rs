//! Styles over the shared palette: the chrome styles every bar and overlay is
//! drawn in, the colour-depth post-pass, and the bridge from the terminal-free
//! styled text model ([`leviculum_ui::style`]) to ratatui text.

use alloc::string::String;
use alloc::vec::Vec;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RtLine, Span as RtSpan, Text};
use ratatui::Frame;

use leviculum_ui::color::{rgb_to_ansi16, rgb_to_ansi256, ColorDepth, Ink};
use leviculum_ui::style::{RLine, RStyle};
use leviculum_ui::theme::Theme;
use leviculum_ui::tokens::Paint;

/// A ratatui [`Color`] from a theme RGB triple.
pub fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// A ratatui [`Color`] from an [`Ink`]. The sixteen ANSI colours become
/// ratatui's named ones, which crossterm sends as `30`-`37` and `90`-`97`,
/// the forms a 16-colour terminal reads; the rest stay indexed.
pub fn ink(ink: Ink) -> Color {
    match ink {
        Ink::Rgb(r, g, b) => Color::Rgb(r, g, b),
        Ink::Indexed(n) => ansi(n),
    }
}

/// An index as ratatui names it.
fn ansi(n: u8) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        15 => Color::White,
        n => Color::Indexed(n),
    }
}

/// The style a colour token ([`leviculum_ui::tokens`](mod@leviculum_ui::tokens)) draws in.
pub fn paint(paint: Paint) -> Style {
    let mut style = Style::default();
    if let Some(fg) = paint.fg {
        style = style.fg(ink(fg));
    }
    if let Some(bg) = paint.bg {
        style = style.bg(ink(bg));
    }
    if paint.reverse {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

/// The style the fixed top-bar and status bar are filled with, full width, so
/// they stand out from the content. With colour, the theme's chrome background
/// and foreground (a muted slate on dark, a light bar on light); under
/// `no_color`, the REVERSE modifier so the bars still delineate without any
/// colour. Shared by both bars for a consistent look.
pub fn chrome_style(no_color: bool, theme: Theme) -> Style {
    if no_color {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
            .fg(rgb(theme.chrome_fg()))
            .bg(rgb(theme.chrome_bg()))
    }
}

/// The foreground for the bright chrome text drawn over the bars: the status-bar
/// key-hints and the AVAILABLE top-bar controls. Uses the theme's bright chrome
/// foreground (~4.5:1 on the chrome background) instead of `Modifier::DIM`,
/// which halved the foreground and read as too low contrast. Under `no_color`
/// it returns the plain style so the reverse-video chrome fill shows through.
pub fn chrome_text_style(no_color: bool, theme: Theme) -> Style {
    if no_color {
        Style::default()
    } else {
        Style::default().fg(rgb(theme.chrome_fg()))
    }
}

/// The foreground for UNAVAILABLE top-bar controls: the theme's muted chrome
/// foreground, a distinct-but-readable grey (~3:1 on the chrome background) that
/// reads as "disabled" without the illegibility of `Modifier::DIM`. Under
/// `no_color` it returns the plain style so the reverse-video fill shows
/// through (no DIM under reverse).
pub fn chrome_muted_style(no_color: bool, theme: Theme) -> Style {
    if no_color {
        Style::default()
    } else {
        Style::default().fg(rgb(theme.chrome_muted_fg()))
    }
}

/// Downgrade the rendered buffer's 24-bit colours to the xterm-256 palette or
/// the sixteen ANSI colours when the resolved [`ColorDepth`] is one of them.
/// Indexed colours, which the token table names itself, pass. A no-op under
/// [`ColorDepth::Truecolor`] and under `no_color` (where `NO_COLOR` already wins
/// and the buffer carries the reverse-video chrome instead of colour), so both
/// leave the buffer byte-identical to before this feature.
pub fn downgrade_buffer(frame: &mut Frame, depth: ColorDepth, no_color: bool) {
    if no_color || depth == ColorDepth::Truecolor {
        return;
    }
    let area = frame.area();
    let buf = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.fg = downgrade_to(cell.fg, depth);
                cell.bg = downgrade_to(cell.bg, depth);
            }
        }
    }
}

/// Map a single 24-bit [`Color::Rgb`] to its nearest xterm-256 [`Color::Indexed`];
/// every other colour (already-indexed, named, reset) passes through unchanged.
pub fn downgrade_color(color: Color) -> Color {
    downgrade_to(color, ColorDepth::Ansi256)
}

/// [`downgrade_color`] at a depth: the nearest xterm-256 index, or the
/// nearest of the sixteen as its named colour.
pub fn downgrade_to(color: Color, depth: ColorDepth) -> Color {
    match (color, depth) {
        (Color::Rgb(r, g, b), ColorDepth::Ansi256) => Color::Indexed(rgb_to_ansi256(r, g, b)),
        (Color::Rgb(r, g, b), ColorDepth::Ansi16) => ansi(rgb_to_ansi16(r, g, b)),
        (other, _) => other,
    }
}

/// Map laid-out styled lines to a ratatui [`Text`], grouping runs of equal
/// style into one span each (the same run-grouping the ANSI sink uses).
pub fn to_ratatui_text(lines: &[RLine]) -> Text<'static> {
    let mut out_lines: Vec<RtLine<'static>> = Vec::with_capacity(lines.len());
    for line in lines {
        let mut spans: Vec<RtSpan<'static>> = Vec::new();
        let mut i = 0;
        while i < line.cells.len() {
            let st = line.cells[i].st;
            let mut j = i;
            let mut text = String::new();
            while j < line.cells.len() && line.cells[j].st == st {
                text.push(line.cells[j].ch);
                j += 1;
            }
            spans.push(RtSpan::styled(text, rstyle_to_ratatui(st)));
            i = j;
        }
        out_lines.push(RtLine::from(spans));
    }
    Text::from(out_lines)
}

/// Translate a resolved [`RStyle`] into a ratatui [`Style`]: colours map by
/// [`ink`], and bold/underline/italic/reverse map to the matching modifiers.
pub fn rstyle_to_ratatui(st: RStyle) -> Style {
    let mut style = Style::default();
    if let Some(fg) = st.fg {
        style = style.fg(ink(fg));
    }
    if let Some(bg) = st.bg {
        style = style.bg(ink(bg));
    }
    if st.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if st.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if st.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if st.reverse {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}
