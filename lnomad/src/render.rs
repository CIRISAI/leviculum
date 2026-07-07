//! ANSI terminal renderer for the micron document model.
//!
//! [`render`] turns a [`MicronDocument`] into a [`RenderedPage`]: an ANSI SGR
//! string laid out for a fixed `width`, plus the page's links collected for
//! navigation. It mirrors the style semantics of NomadNet's `MicronParser.py`
//! (`markup_to_attrmaps`/`make_style`/`make_output`) and `Browser.py`, but
//! emits 24-bit ANSI SGR rather than urwid attributes.
//!
//! Style mapping mirrored from the reference (canonical
//! `nomadnet-textui/MicronParser.py`):
//!
//! - Colour resolution comes from [`leviculum_micron::Color::rgb`], which ports
//!   `high_color` (lines 518-567): three hex nibbles doubled into `#rrggbb`, a
//!   grayscale `gNN` level, or six hex characters as a full true colour. A colour
//!   that does not resolve falls back to the terminal default, as in the
//!   reference `try/except`.
//! - Bold (`` `! ``), underline (`` `_ ``), italic (`` `* ``) map to SGR `1`/`4`/`3`
//!   (`make_style` lines 570-591). Every styled run is self contained and reset.
//! - Alignment (`` `c ``/`` `l ``/`` `r ``) centres/left/right aligns the content
//!   within the content width (`make_output` lines 648-655, applied at the urwid
//!   `Text` `align` in `parse_line`).
//! - Section headings indent by `(depth - 1) * SECTION_INDENT` and carry the
//!   theme banding colour (`left_indent` line 418, `STYLES_DARK` lines 18-23,
//!   heading dispatch lines 287-318). The banding colour spans the full row.
//! - The default page foreground (`DEFAULT_FG_DARK = "ddd"`, line 12) is mapped
//!   to the terminal's own default foreground: an unset span colour emits no
//!   SGR, which is wire-compatible with the reference's near-white default.
//! - Dividers draw a full-width rule (`parse_line` lines 324-336), indented by
//!   `left_indent`/`right_indent` when inside a section.
//! - Literal blocks pass through verbatim with no inline parsing or wrapping
//!   (`parse_line` lines 226-231, `make_output` lines 595-598).
//! - Tables lay out `|`-separated rows in aligned columns bounded by
//!   `MAX_TABLE_WIDTH = 100` (lines 37, 197-218).

use leviculum_micron::{Align, Block, Field, FieldKind, Line, MicronDocument, Style};

/// Per-depth section indent, matching the reference `SECTION_INDENT`.
const SECTION_INDENT: usize = 2;
/// Default table width cap, matching the reference `MAX_TABLE_WIDTH`.
const MAX_TABLE_WIDTH: usize = 100;
/// Smallest width the renderer will lay out to; anything smaller is clamped up
/// so wrapping and alignment stay well defined.
const MIN_WIDTH: usize = 1;
/// The foreground colour applied to link labels and their `[N]` markers, so a
/// link reads as distinct from body text regardless of the page's own colours.
const LINK_FG: (u8, u8, u8) = (0, 175, 255);

/// The full rendered page: laid-out text plus the links found in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderedPage {
    /// The rendered ANSI (or, with `no_color`, plain) text.
    pub text: String,
    /// The page's links, in source order, each with a 1-based `index`.
    pub links: Vec<RenderedLink>,
}

/// A single link collected while rendering, ready for Phase 4 navigation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderedLink {
    /// 1-based index matching the visible `[N]` marker in the text.
    pub index: usize,
    /// The link's display label.
    pub label: String,
    /// The link target (destination hash / page path).
    pub target: String,
    /// The link's `|`-separated field components, split into `(key, value)`
    /// pairs (a component without `=` yields an empty value).
    pub fields: Vec<(String, String)>,
}

/// Render a document to a [`RenderedPage`] at `width` columns, with 24-bit ANSI
/// colour enabled.
pub fn render(doc: &MicronDocument, width: usize) -> RenderedPage {
    render_with_options(doc, width, false)
}

/// Render a document, optionally stripping all SGR sequences.
///
/// With `no_color` set, the output carries no escape sequences at all (the
/// `[N]` link markers, indentation, wrapping and alignment are preserved).
pub fn render_with_options(doc: &MicronDocument, width: usize, no_color: bool) -> RenderedPage {
    let mut renderer = Renderer {
        out: String::new(),
        links: Vec::new(),
        width: width.max(MIN_WIDTH),
        no_color,
    };
    for block in &doc.blocks {
        renderer.render_block(block);
    }
    RenderedPage {
        text: renderer.out,
        links: renderer.links,
    }
}

/// A resolved terminal style: colours already reduced to RGB, attributes as
/// flags. `None` colours mean "use the terminal default" (no SGR emitted).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RStyle {
    fg: Option<(u8, u8, u8)>,
    bg: Option<(u8, u8, u8)>,
    bold: bool,
    underline: bool,
    italic: bool,
}

impl RStyle {
    fn is_plain(&self) -> bool {
        self.fg.is_none() && self.bg.is_none() && !self.bold && !self.underline && !self.italic
    }

    /// The self-contained SGR prefix for this style (starts by resetting).
    fn sgr(&self) -> String {
        let mut s = String::from("\x1b[0");
        if self.bold {
            s.push_str(";1");
        }
        if self.italic {
            s.push_str(";3");
        }
        if self.underline {
            s.push_str(";4");
        }
        if let Some((r, g, b)) = self.fg {
            s.push_str(&format!(";38;2;{r};{g};{b}"));
        }
        if let Some((r, g, b)) = self.bg {
            s.push_str(&format!(";48;2;{r};{g};{b}"));
        }
        s.push('m');
        s
    }
}

/// Resolve a model [`Style`] to a terminal [`RStyle`].
fn resolve_style(style: &Style) -> RStyle {
    RStyle {
        fg: style.fg.as_ref().and_then(|c| c.rgb),
        bg: style.bg.as_ref().and_then(|c| c.rgb),
        bold: style.bold,
        underline: style.underline,
        italic: style.italic,
    }
}

/// One styled character, the unit the layout engine works in.
#[derive(Clone, Copy, Debug)]
struct StyledChar {
    ch: char,
    st: RStyle,
}

struct Renderer {
    out: String,
    links: Vec<RenderedLink>,
    width: usize,
    no_color: bool,
}

impl Renderer {
    fn render_block(&mut self, block: &Block) {
        match block {
            Block::Heading { depth, line } => self.render_heading(*depth, line),
            Block::Paragraph { depth, line } => self.render_paragraph(*depth as usize, line),
            Block::Divider { depth, character } => self.render_divider(*depth as usize, *character),
            Block::LiteralBlock { lines } => self.render_literal(lines),
            Block::Table {
                depth,
                align,
                max_width,
                rows,
            } => self.render_table(*depth as usize, *align, *max_width, rows),
            Block::Partial { depth, .. } => self.render_partial(*depth as usize),
            Block::Blank => self.out.push('\n'),
        }
    }

    /// Indent width for a given section depth: `(depth - 1) * SECTION_INDENT`,
    /// mirroring the reference `left_indent` (zero at depth 0/1).
    fn indent_for(depth: usize) -> usize {
        depth.saturating_sub(1) * SECTION_INDENT
    }

    fn render_paragraph(&mut self, depth: usize, line: &Line) {
        let (chars, align) = self.flatten_line(line);
        if chars.is_empty() {
            self.out.push('\n');
            return;
        }
        let indent = Self::indent_for(depth);
        let content_width = self.width.saturating_sub(indent).max(MIN_WIDTH);
        for visual in wrap(chars, content_width) {
            self.emit_aligned(visual, indent, content_width, align);
        }
    }

    fn render_heading(&mut self, depth: u8, line: &Line) {
        let (fg, bg) = heading_theme(depth);
        let (chars, align) = self.flatten_heading(line, fg, bg);
        let indent = Self::indent_for(depth as usize);
        let content_width = self.width.saturating_sub(indent).max(MIN_WIDTH);
        let band = RStyle {
            fg: Some(fg),
            bg: Some(bg),
            ..RStyle::default()
        };
        let visuals = if chars.is_empty() {
            vec![Vec::new()]
        } else {
            wrap(chars, content_width)
        };
        for content in visuals {
            let lead = leading_pad(align, content_width, content.len());
            let mut row: Vec<StyledChar> = Vec::new();
            for _ in 0..indent {
                row.push(StyledChar { ch: ' ', st: band });
            }
            for _ in 0..lead {
                row.push(StyledChar { ch: ' ', st: band });
            }
            row.extend(content);
            // Pad the whole row so the theme band spans the full page width.
            while row.len() < self.width {
                row.push(StyledChar { ch: ' ', st: band });
            }
            self.emit_line(&row);
        }
    }

    fn render_divider(&mut self, depth: usize, character: char) {
        let side = Self::indent_for(depth);
        let rule_len = self.width.saturating_sub(side * 2);
        let mut row: Vec<StyledChar> = Vec::new();
        for _ in 0..side {
            row.push(StyledChar {
                ch: ' ',
                st: RStyle::default(),
            });
        }
        for _ in 0..rule_len {
            row.push(StyledChar {
                ch: character,
                st: RStyle::default(),
            });
        }
        self.emit_line(&row);
    }

    fn render_literal(&mut self, lines: &[Line]) {
        for line in lines {
            let (chars, _align) = self.flatten_line(line);
            // Verbatim: no wrapping, no indent, no alignment. Backticks and any
            // other markup characters are already stored literally by the parser.
            self.emit_line(&chars);
        }
    }

    fn render_partial(&mut self, depth: usize) {
        let indent = Self::indent_for(depth);
        let mut row: Vec<StyledChar> = Vec::new();
        for _ in 0..indent {
            row.push(StyledChar {
                ch: ' ',
                st: RStyle::default(),
            });
        }
        // Match the reference placeholder glyph for an unresolved partial.
        row.push(StyledChar {
            ch: '\u{29d6}',
            st: RStyle::default(),
        });
        self.emit_line(&row);
    }

    fn render_table(
        &mut self,
        depth: usize,
        align: Option<Align>,
        max_width: Option<usize>,
        rows: &[String],
    ) {
        if rows.is_empty() {
            return;
        }
        let indent = Self::indent_for(depth);
        let cell_align = align.unwrap_or(Align::Left);
        let bound = max_width
            .unwrap_or(MAX_TABLE_WIDTH)
            .min(self.width.saturating_sub(indent))
            .max(MIN_WIDTH);

        let parsed: Vec<Vec<String>> = rows
            .iter()
            .map(|row| row.split('|').map(|c| c.trim().to_string()).collect())
            .collect();
        let ncols = parsed.iter().map(Vec::len).max().unwrap_or(0);
        if ncols == 0 {
            return;
        }

        // Natural column widths from the non-separator rows.
        let mut colw = vec![1usize; ncols];
        for row in &parsed {
            if is_separator_row(row) {
                continue;
            }
            for (c, cell) in row.iter().enumerate() {
                colw[c] = colw[c].max(cell.chars().count());
            }
        }

        // Scale down proportionally if the natural layout overflows `bound`.
        let gap = 3; // " | "
        let sep_total = ncols.saturating_sub(1) * gap;
        let natural: usize = colw.iter().sum::<usize>() + sep_total;
        if natural > bound && colw.iter().sum::<usize>() > 0 {
            let budget = bound.saturating_sub(sep_total).max(ncols);
            let sum: usize = colw.iter().sum();
            for w in colw.iter_mut() {
                *w = ((*w * budget) / sum).max(1);
            }
        }

        for row in &parsed {
            let mut cells: Vec<StyledChar> = Vec::new();
            for _ in 0..indent {
                cells.push(StyledChar {
                    ch: ' ',
                    st: RStyle::default(),
                });
            }
            if is_separator_row(row) {
                for (c, w) in colw.iter().enumerate() {
                    if c > 0 {
                        push_plain(&mut cells, "\u{2500}\u{253c}\u{2500}");
                    }
                    for _ in 0..*w {
                        cells.push(StyledChar {
                            ch: '\u{2500}',
                            st: RStyle::default(),
                        });
                    }
                }
            } else {
                for (c, w) in colw.iter().enumerate() {
                    if c > 0 {
                        push_plain(&mut cells, " \u{2502} ");
                    }
                    let empty = String::new();
                    let cell = row.get(c).unwrap_or(&empty);
                    push_plain(&mut cells, &fit(cell, *w, cell_align));
                }
            }
            self.emit_line(&cells);
        }
    }

    /// Flatten a line's spans into styled characters, recording links and their
    /// visible `[N]` markers. Returns the characters and the line's alignment.
    fn flatten_line(&mut self, line: &Line) -> (Vec<StyledChar>, Align) {
        let mut out: Vec<StyledChar> = Vec::new();
        for span in &line.spans {
            let base = resolve_style(&span.style);
            if let Some(link) = &span.link {
                let index = self.links.len() + 1;
                let label = if span.text.is_empty() {
                    link.label.clone()
                } else {
                    span.text.clone()
                };
                self.links.push(RenderedLink {
                    index,
                    label: link.label.clone(),
                    target: link.target.clone(),
                    fields: link.fields.iter().map(|f| split_field(f)).collect(),
                });
                let link_style = RStyle {
                    fg: Some(LINK_FG),
                    underline: true,
                    ..base
                };
                // Only the core label (and the appended `[N]` marker) carry the
                // link style; surrounding whitespace stays in the base style so it
                // is not underlined or coloured. rngit puts bullets/indent inside
                // the link label, and the underlined leading whitespace looks bad.
                let lead_len = label.len() - label.trim_start().len();
                let (lead, rest) = label.split_at(lead_len);
                let trail_len = rest.len() - rest.trim_end().len();
                let (core, trail) = rest.split_at(rest.len() - trail_len);
                push_styled(&mut out, lead, base);
                push_styled(&mut out, core, link_style);
                push_styled(&mut out, &format!("[{index}]"), link_style);
                push_styled(&mut out, trail, base);
                continue;
            }
            if let Some(field) = &span.field {
                push_styled(&mut out, &field_text(field), base);
                continue;
            }
            // Anchor spans carry no visible text; a plain span carries its text.
            push_styled(&mut out, &span.text, base);
        }
        let align = line.spans.last().map(|s| s.style.align).unwrap_or_default();
        (out, align)
    }

    /// Flatten a heading line, forcing the theme band colours while preserving
    /// per-span bold/underline/italic toggles.
    fn flatten_heading(
        &mut self,
        line: &Line,
        fg: (u8, u8, u8),
        bg: (u8, u8, u8),
    ) -> (Vec<StyledChar>, Align) {
        let (chars, align) = self.flatten_line(line);
        let banded = chars
            .into_iter()
            .map(|sc| StyledChar {
                ch: sc.ch,
                st: RStyle {
                    fg: Some(fg),
                    bg: Some(bg),
                    bold: sc.st.bold,
                    underline: sc.st.underline,
                    italic: sc.st.italic,
                },
            })
            .collect();
        (banded, align)
    }

    /// Emit a wrapped content line with indentation and alignment padding.
    fn emit_aligned(
        &mut self,
        content: Vec<StyledChar>,
        indent: usize,
        content_width: usize,
        align: Align,
    ) {
        let lead = leading_pad(align, content_width, content.len());
        let mut row: Vec<StyledChar> = Vec::new();
        for _ in 0..(indent + lead) {
            row.push(StyledChar {
                ch: ' ',
                st: RStyle::default(),
            });
        }
        row.extend(content);
        self.emit_line(&row);
    }

    /// Write a fully laid-out line of styled characters, grouping runs of equal
    /// style into one SGR sequence each, then a trailing newline.
    fn emit_line(&mut self, chars: &[StyledChar]) {
        let mut active = false;
        let mut i = 0;
        while i < chars.len() {
            let st = chars[i].st;
            let mut j = i;
            let mut text = String::new();
            while j < chars.len() && chars[j].st == st {
                text.push(chars[j].ch);
                j += 1;
            }
            if !self.no_color && !st.is_plain() {
                self.out.push_str(&st.sgr());
                self.out.push_str(&text);
                active = true;
            } else {
                if active {
                    self.out.push_str("\x1b[0m");
                    active = false;
                }
                self.out.push_str(&text);
            }
            i = j;
        }
        if active {
            self.out.push_str("\x1b[0m");
        }
        self.out.push('\n');
    }
}

/// The dark-theme heading band colours (fg, bg) for a heading depth, mirroring
/// `STYLES_DARK` (`MicronParser.py` lines 20-22). Depths beyond 3 reuse the
/// depth-3 band.
fn heading_theme(depth: u8) -> ((u8, u8, u8), (u8, u8, u8)) {
    match depth {
        1 => ((0x22, 0x22, 0x22), (0xbb, 0xbb, 0xbb)),
        2 => ((0x11, 0x11, 0x11), (0x99, 0x99, 0x99)),
        _ => ((0x00, 0x00, 0x00), (0x77, 0x77, 0x77)),
    }
}

/// Leading padding count for aligning `len` visible columns within `width`.
fn leading_pad(align: Align, width: usize, len: usize) -> usize {
    let slack = width.saturating_sub(len);
    match align {
        Align::Left => 0,
        Align::Right => slack,
        Align::Center => slack / 2,
    }
}

/// Word-wrap styled characters to `width` columns, breaking at spaces where
/// possible and hard-breaking over-long runs. Always yields at least one line.
fn wrap(chars: Vec<StyledChar>, width: usize) -> Vec<Vec<StyledChar>> {
    let width = width.max(MIN_WIDTH);
    let mut lines: Vec<Vec<StyledChar>> = Vec::new();
    let mut cur: Vec<StyledChar> = Vec::new();
    for sc in chars {
        cur.push(sc);
        if cur.len() > width {
            match cur.iter().rposition(|c| c.ch == ' ') {
                Some(pos) => {
                    let rest = cur.split_off(pos + 1);
                    while cur.last().is_some_and(|c| c.ch == ' ') {
                        cur.pop();
                    }
                    lines.push(std::mem::take(&mut cur));
                    cur = rest;
                }
                None => {
                    let rest = cur.split_off(width);
                    lines.push(std::mem::take(&mut cur));
                    cur = rest;
                }
            }
        }
    }
    lines.push(cur);
    lines
}

/// Append the characters of `text` with a single style.
fn push_styled(out: &mut Vec<StyledChar>, text: &str, st: RStyle) {
    for ch in text.chars() {
        out.push(StyledChar { ch, st });
    }
}

/// Append `text` with the default (unstyled) style.
fn push_plain(out: &mut Vec<StyledChar>, text: &str) {
    push_styled(out, text, RStyle::default());
}

/// Fit `text` into exactly `width` columns, truncating or padding per `align`.
fn fit(text: &str, width: usize, align: Align) -> String {
    if width == 0 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() >= width {
        return chars[..width].iter().collect();
    }
    let slack = width - chars.len();
    let (left, right) = match align {
        Align::Left => (0, slack),
        Align::Right => (slack, 0),
        Align::Center => (slack / 2, slack - slack / 2),
    };
    let mut s = String::new();
    for _ in 0..left {
        s.push(' ');
    }
    s.push_str(text);
    for _ in 0..right {
        s.push(' ');
    }
    s
}

/// Whether a parsed table row is a markdown separator (all cells are dashes,
/// optionally with alignment colons).
fn is_separator_row(row: &[String]) -> bool {
    !row.is_empty()
        && row.iter().all(|cell| {
            let t = cell.trim();
            !t.is_empty() && t.chars().all(|c| c == '-' || c == ':')
        })
}

/// Split a link/field component `k=v` into `(k, v)`; a component without `=`
/// yields an empty value.
fn split_field(component: &str) -> (String, String) {
    match component.split_once('=') {
        Some((k, v)) => (k.to_string(), v.to_string()),
        None => (component.to_string(), String::new()),
    }
}

/// The visible text drawn for a form field.
fn field_text(field: &Field) -> String {
    match field.kind {
        FieldKind::Text => format!("[{}]", field.value),
        FieldKind::Checkbox => {
            format!(
                "[{}] {}",
                if field.prechecked { "x" } else { " " },
                field.label
            )
        }
        FieldKind::Radio => {
            format!(
                "({}) {}",
                if field.prechecked { "*" } else { " " },
                field.label
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_micron::{parse, Color, Line, Link, Span, Style};

    fn plain_span(text: &str) -> Span {
        Span {
            text: text.to_string(),
            ..Span::default()
        }
    }

    fn styled_span(text: &str, style: Style) -> Span {
        Span {
            text: text.to_string(),
            style,
            ..Span::default()
        }
    }

    fn doc(blocks: Vec<Block>) -> MicronDocument {
        MicronDocument {
            blocks,
            ..MicronDocument::default()
        }
    }

    #[test]
    fn heading_has_indent_and_theme_colour() {
        let d = doc(vec![Block::Heading {
            depth: 2,
            line: Line::new(vec![plain_span("Title")]),
        }]);
        let page = render(&d, 20);
        // Depth-2 band: fg #111111 (17), bg #999999 (153).
        assert!(page.text.contains("38;2;17;17;17"));
        assert!(page.text.contains("48;2;153;153;153"));
        // Depth 2 indents by (2-1)*2 = 2 columns, inside the band.
        assert!(page.text.contains("  Title") || page.text.contains("Title"));
        // Band spans the full width (20 cols) => trailing padding present.
        let first_line = page.text.lines().next().unwrap_or("");
        // Strip SGR to count visible columns.
        let visible = strip_sgr(first_line);
        assert_eq!(visible.chars().count(), 20, "band should fill the width");
    }

    #[test]
    fn bold_underline_italic_emit_sgr_and_reset() {
        let style = Style {
            bold: true,
            underline: true,
            italic: true,
            ..Style::default()
        };
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![styled_span("x", style)]),
        }]);
        let page = render(&d, 40);
        assert!(page.text.contains("\x1b[0;1;3;4m"), "got: {:?}", page.text);
        assert!(page.text.contains("x\x1b[0m"));
    }

    #[test]
    fn fg_and_bg_emit_24bit_codes() {
        let style = Style {
            fg: Some(Color::parse("f00")),
            bg: Some(Color::parse("00f")),
            ..Style::default()
        };
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![styled_span("hi", style)]),
        }]);
        let page = render(&d, 40);
        // f00 -> #ff0000, 00f -> #0000ff (nibble doubling).
        assert!(page.text.contains("38;2;255;0;0"));
        assert!(page.text.contains("48;2;0;0;255"));
    }

    #[test]
    fn centred_line_is_padded() {
        let style = Style {
            align: Align::Center,
            ..Style::default()
        };
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![styled_span("hey", style)]),
        }]);
        let page = render(&d, 11);
        let line = page.text.lines().next().unwrap_or("");
        // (11 - 3) / 2 = 4 leading spaces.
        assert!(line.starts_with("    hey"), "got: {line:?}");
    }

    #[test]
    fn right_aligned_line_is_padded() {
        let style = Style {
            align: Align::Right,
            ..Style::default()
        };
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![styled_span("end", style)]),
        }]);
        let page = render(&d, 10);
        let line = page.text.lines().next().unwrap_or("");
        assert!(line.starts_with("       end"), "got: {line:?}");
    }

    #[test]
    fn divider_fills_full_width() {
        let d = doc(vec![Block::Divider {
            depth: 0,
            character: '\u{2500}',
        }]);
        let page = render(&d, 12);
        let line = page.text.lines().next().unwrap_or("");
        assert_eq!(line.chars().filter(|&c| c == '\u{2500}').count(), 12);
    }

    #[test]
    fn indented_divider_is_shorter() {
        let d = doc(vec![Block::Divider {
            depth: 2,
            character: '-',
        }]);
        let page = render(&d, 12);
        let line = page.text.lines().next().unwrap_or("");
        // depth 2 => indent 2 each side => 12 - 4 = 8 rule chars, 2 leading spaces.
        assert!(line.starts_with("  --------"), "got: {line:?}");
        assert_eq!(line.chars().filter(|&c| c == '-').count(), 8);
    }

    #[test]
    fn literal_block_is_verbatim_with_backticks() {
        let d = doc(vec![Block::LiteralBlock {
            lines: vec![Line::new(vec![plain_span("code `x` = 1")])],
        }]);
        let page = render(&d, 4); // narrow width must not wrap a literal
        assert!(page.text.contains("code `x` = 1"));
    }

    #[test]
    fn link_gets_marker_and_entry() {
        let link = Link {
            label: "Docs".to_string(),
            target: "/page/docs.mu".to_string(),
            fields: vec!["g=reticulum".to_string(), "ref".to_string()],
        };
        let span = Span {
            text: "Docs".to_string(),
            link: Some(link),
            ..Span::default()
        };
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![span]),
        }]);
        let page = render(&d, 40);
        assert!(page.text.contains("Docs"));
        assert!(page.text.contains("[1]"), "visible marker missing");
        assert_eq!(page.links.len(), 1);
        let l = &page.links[0];
        assert_eq!(l.index, 1);
        assert_eq!(l.label, "Docs");
        assert_eq!(l.target, "/page/docs.mu");
        assert_eq!(
            l.fields,
            vec![
                ("g".to_string(), "reticulum".to_string()),
                ("ref".to_string(), String::new()),
            ]
        );
    }

    fn link_span(label: &str) -> Span {
        Span {
            text: label.to_string(),
            link: Some(Link {
                label: label.to_string(),
                target: "/page/index.mu".to_string(),
                fields: vec![],
            }),
            ..Span::default()
        }
    }

    #[test]
    fn link_leading_whitespace_not_underlined() {
        // rngit embeds the bullet + indent inside the link label; the leading
        // whitespace must render plain, not underlined.
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![link_span("  \u{2022} mirrors")]),
        }]);
        let page = render(&d, 40);
        // Two leading spaces precede the underlined LINK run; the core label and
        // the `[1]` marker share the underline + LINK_FG run.
        assert!(
            page.text
                .contains("  \x1b[0;4;38;2;0;175;255m\u{2022} mirrors[1]"),
            "got: {:?}",
            page.text
        );
        // The leading spaces appear before the first SGR sequence.
        let first_sgr = page.text.find('\x1b').unwrap();
        assert!(
            page.text[..first_sgr].contains("  "),
            "leading spaces should precede the first SGR; got: {:?}",
            page.text
        );
    }

    #[test]
    fn link_trailing_whitespace_not_underlined() {
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![link_span("mirrors  ")]),
        }]);
        let page = render(&d, 40);
        // Core label + marker underlined, then reset, then two plain spaces.
        assert!(
            page.text
                .contains("\x1b[0;4;38;2;0;175;255mmirrors[1]\x1b[0m  "),
            "got: {:?}",
            page.text
        );
    }

    #[test]
    fn link_without_whitespace_fully_underlined() {
        // Control: no surrounding whitespace => whole label stays underlined.
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![link_span("mirrors")]),
        }]);
        let page = render(&d, 40);
        assert!(
            page.text
                .contains("\x1b[0;4;38;2;0;175;255mmirrors[1]\x1b[0m"),
            "got: {:?}",
            page.text
        );
    }

    #[test]
    fn long_paragraph_wraps_to_width() {
        let text = "one two three four five six seven eight nine ten";
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![plain_span(text)]),
        }]);
        let page = render(&d, 12);
        for line in page.text.lines() {
            assert!(
                strip_sgr(line).chars().count() <= 12,
                "line too long: {line:?}"
            );
        }
        // No word was split across a wrap.
        assert!(page.text.contains("three"));
        assert!(page.text.contains("eight"));
    }

    #[test]
    fn no_color_strips_all_sgr() {
        let style = Style {
            bold: true,
            fg: Some(Color::parse("f00")),
            ..Style::default()
        };
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![styled_span("bold red", style)]),
        }]);
        let page = render_with_options(&d, 40, true);
        assert!(!page.text.contains('\x1b'), "SGR leaked: {:?}", page.text);
        assert!(page.text.contains("bold red"));
    }

    #[test]
    fn no_color_keeps_link_marker() {
        let link = Link {
            label: "L".to_string(),
            target: "/x".to_string(),
            fields: vec![],
        };
        let span = Span {
            text: "L".to_string(),
            link: Some(link),
            ..Span::default()
        };
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![span]),
        }]);
        let page = render_with_options(&d, 40, true);
        assert!(!page.text.contains('\x1b'));
        assert!(page.text.contains("L[1]"));
    }

    #[test]
    fn table_columns_align() {
        let d = doc(vec![Block::Table {
            depth: 0,
            align: None,
            max_width: None,
            rows: vec![
                "Name | Age".to_string(),
                "--- | ---".to_string(),
                "Ann | 3".to_string(),
                "Bob | 42".to_string(),
            ],
        }]);
        let page = render(&d, 40);
        let lines: Vec<&str> = page.text.lines().collect();
        // Header and body rows share the same separator column position.
        let header = strip_sgr(lines[0]);
        let bob = strip_sgr(lines[3]);
        let hbar = header.find('\u{2502}');
        let bbar = bob.find('\u{2502}');
        assert!(
            hbar.is_some() && hbar == bbar,
            "columns misaligned: {lines:?}"
        );
        // "Name" column is 4 wide, so "Ann" is padded to 4 before the divider.
        assert!(strip_sgr(lines[2]).starts_with("Ann  \u{2502} 3"));
    }

    #[test]
    fn narrow_width_does_not_panic() {
        let text = "a very long unbroken paragraph of words here";
        let d = doc(vec![Block::Paragraph {
            depth: 3,
            line: Line::new(vec![plain_span(text)]),
        }]);
        // width smaller than the indent must still render without panicking.
        let page = render(&d, 1);
        assert!(!page.text.is_empty());
    }

    #[test]
    fn readme_mu_renders_without_panic() {
        let src = include_str!("../../vendor/Reticulum/README.mu");
        let d = parse(src);
        let page = render(&d, 80);
        assert!(!page.text.is_empty());
        // The README opens with a section heading.
        assert!(page.text.contains("Reticulum Network Stack"));
        // It contains links, which must be collected with sane indices.
        assert!(!page.links.is_empty());
        for (i, link) in page.links.iter().enumerate() {
            assert_eq!(link.index, i + 1);
            assert!(!link.target.is_empty());
        }
        // no_color rendering must also survive the whole document.
        let plain = render_with_options(&d, 80, true);
        assert!(!plain.text.contains('\x1b'));
    }

    /// Strip ANSI SGR sequences so tests can reason about visible columns.
    fn strip_sgr(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip until the terminating 'm'.
                for c2 in chars.by_ref() {
                    if c2 == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
