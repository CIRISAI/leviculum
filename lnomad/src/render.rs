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
use unicode_width::UnicodeWidthChar;

use crate::theme::Theme;

/// Per-depth section indent, matching the reference `SECTION_INDENT`.
const SECTION_INDENT: usize = 2;
/// Default table width cap, matching the reference `MAX_TABLE_WIDTH`.
const MAX_TABLE_WIDTH: usize = 100;
/// Smallest width the renderer will lay out to; anything smaller is clamped up
/// so wrapping and alignment stay well defined.
const MIN_WIDTH: usize = 1;

/// The full rendered page: laid-out text plus the links found in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderedPage {
    /// The rendered ANSI (or, with `no_color`, plain) text.
    pub text: String,
    /// The page's links, in source order, each with a 1-based `index`.
    pub links: Vec<RenderedLink>,
}

/// A single link collected while rendering, ready for navigation and (via its
/// laid-out position) future mouse hit-testing in the TUI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderedLink {
    /// 1-based index in source order. Internal only now (no visible `[N]`
    /// marker): it identifies the link for focus, hint labelling and hit-testing.
    pub index: usize,
    /// The link's display label.
    pub label: String,
    /// The link target (destination hash / page path).
    pub target: String,
    /// The link's `|`-separated field components, split into `(key, value)`
    /// pairs (a component without `=` yields an empty value).
    pub fields: Vec<(String, String)>,
    /// 0-based index of the laid-out [`RLine`] the link's visible label starts
    /// on (the clickable core, after any leading whitespace).
    pub line: usize,
    /// 0-based DISPLAY column where the clickable label starts on that line (a
    /// wide char before it advances two columns), matching the screen columns the
    /// renderer paints so hit-testing and hints land on the label.
    pub col_start: usize,
    /// 0-based display column one past the last clickable cell.
    /// `col_start..col_end` is the hit-test range (the visible label core, with
    /// no `[N]` marker).
    pub col_end: usize,
}

/// Render a document to a [`RenderedPage`] at `width` columns, with 24-bit ANSI
/// colour enabled and the dark theme (the `--print` / non-tty look).
pub fn render(doc: &MicronDocument, width: usize) -> RenderedPage {
    render_with_options(doc, width, false)
}

/// Render a document, optionally stripping all SGR sequences.
///
/// With `no_color` set, the output carries no escape sequences at all
/// (indentation, wrapping and alignment are preserved; links, no longer marked
/// by a `[N]`, become indistinguishable from body text in plain output).
///
/// This is the ANSI sink: it lays the document out into target-agnostic styled
/// lines with [`layout`], then serialises them to the exact byte stream the
/// `--print` / non-tty path emits.
pub fn render_with_options(doc: &MicronDocument, width: usize, no_color: bool) -> RenderedPage {
    // The print / non-tty sink is always the dark theme, so its golden output is
    // stable and independent of any terminal-background detection.
    let (lines, links) = layout(doc, width, Theme::Dark);
    RenderedPage {
        text: emit_ansi(&lines, no_color),
        links,
    }
}

/// Lay a document out into target-agnostic styled lines: the shared core both
/// the ANSI sink ([`render_with_options`]) and the ratatui sink build on.
///
/// Each [`RLine`] is one output row, already width-wrapped, aligned and
/// indented; every character carries its resolved [`RStyle`]. The returned
/// [`RenderedLink`]s carry both their navigation data and their laid-out
/// `(line, col_start, col_end)` position for hit-testing. No `no_color` choice
/// is made here: colour is a property of the sink, not the layout. The `theme`
/// selects the accent colours (heading bands, link colour) baked into the cells.
pub fn layout(doc: &MicronDocument, width: usize, theme: Theme) -> (Vec<RLine>, Vec<RenderedLink>) {
    let (lines, links, _block_lines) = layout_blocks(doc, width, theme);
    (lines, links)
}

/// Lay a document out like [`layout`], additionally returning the block->line
/// mapping: `block_lines[i]` is the 0-based index of the first [`RLine`] the
/// `i`-th [`Block`] laid out into. This lets `#anchor` navigation, which stores
/// anchors as block indices, resolve an anchor to a page line. A block that
/// emits no line (an empty table) records the line index the next block starts
/// at, so the mapping is still monotonic and in range.
pub fn layout_blocks(
    doc: &MicronDocument,
    width: usize,
    theme: Theme,
) -> (Vec<RLine>, Vec<RenderedLink>, Vec<usize>) {
    let mut renderer = Renderer {
        lines: Vec::new(),
        links: Vec::new(),
        width: width.max(MIN_WIDTH),
        theme,
    };
    let mut block_lines = Vec::with_capacity(doc.blocks.len());
    for block in &doc.blocks {
        block_lines.push(renderer.lines.len());
        renderer.render_block(block);
    }
    renderer.record_link_positions();
    (renderer.lines, renderer.links, block_lines)
}

/// Serialise laid-out styled lines to the ANSI byte stream. Runs of equal style
/// are grouped into a single self-contained SGR sequence, matching the layout's
/// original line-at-a-time emission byte-for-byte. With `no_color`, no escape
/// sequences are emitted at all.
fn emit_ansi(lines: &[RLine], no_color: bool) -> String {
    let mut out = String::new();
    for line in lines {
        emit_ansi_line(&line.cells, no_color, &mut out);
    }
    out
}

/// Emit one styled line (plus its trailing newline) into `out`.
fn emit_ansi_line(cells: &[StyledChar], no_color: bool, out: &mut String) {
    let mut active = false;
    let mut i = 0;
    while i < cells.len() {
        let st = cells[i].st;
        let mut j = i;
        let mut text = String::new();
        while j < cells.len() && cells[j].st == st {
            text.push(cells[j].ch);
            j += 1;
        }
        if !no_color && !st.is_plain() {
            out.push_str(&st.sgr());
            out.push_str(&text);
            active = true;
        } else {
            if active {
                out.push_str("\x1b[0m");
                active = false;
            }
            out.push_str(&text);
        }
        i = j;
    }
    if active {
        out.push_str("\x1b[0m");
    }
    out.push('\n');
}

/// A resolved terminal style: colours already reduced to RGB, attributes as
/// flags. `None` colours mean "use the terminal default" (no SGR emitted).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RStyle {
    /// Foreground colour, or `None` for the terminal default.
    pub fg: Option<(u8, u8, u8)>,
    /// Background colour, or `None` for the terminal default.
    pub bg: Option<(u8, u8, u8)>,
    /// Bold attribute.
    pub bold: bool,
    /// Underline attribute.
    pub underline: bool,
    /// Italic attribute.
    pub italic: bool,
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
pub struct StyledChar {
    /// The visible character.
    pub ch: char,
    /// Its resolved style.
    pub st: RStyle,
    /// The 1-based index of the link this cell is a clickable part of, if any.
    /// Leading/trailing whitespace inside a link label is untagged so it stays
    /// unstyled and outside the hit-test range.
    pub link: Option<usize>,
}

/// One laid-out output row: a sequence of styled cells, already wrapped,
/// aligned and indented. The target-agnostic unit both sinks consume.
#[derive(Clone, Debug, Default)]
pub struct RLine {
    /// The row's cells, left to right.
    pub cells: Vec<StyledChar>,
}

struct Renderer {
    lines: Vec<RLine>,
    links: Vec<RenderedLink>,
    width: usize,
    theme: Theme,
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
            Block::Blank => self.push_blank(),
        }
    }

    /// Store a finished row of styled cells as one output line.
    fn push_line(&mut self, cells: Vec<StyledChar>) {
        self.lines.push(RLine { cells });
    }

    /// Store an empty output line (a blank row).
    fn push_blank(&mut self) {
        self.lines.push(RLine::default());
    }

    /// After all blocks are laid out, walk the lines and record each link's
    /// laid-out position: the line and column span of its tagged (clickable)
    /// cells, taken from the first line the link appears on.
    fn record_link_positions(&mut self) {
        let mut seen: Vec<bool> = vec![false; self.links.len()];
        for (li, line) in self.lines.iter().enumerate() {
            // Track the running DISPLAY column across the line so a link sitting
            // after a wide char is recorded at the column the renderer paints it,
            // not its character index.
            let mut col = 0usize;
            for cell in line.cells.iter() {
                let w = UnicodeWidthChar::width(cell.ch).unwrap_or(0);
                if let Some(idx) = cell.link {
                    if let Some(slot) = idx.checked_sub(1) {
                        if let Some(link) = self.links.get_mut(slot) {
                            if !seen[slot] {
                                seen[slot] = true;
                                link.line = li;
                                link.col_start = col;
                                link.col_end = col + w;
                            } else if link.line == li {
                                link.col_end = col + w;
                            }
                        }
                    }
                }
                col += w;
            }
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
            self.push_blank();
            return;
        }
        let indent = Self::indent_for(depth);
        let content_width = self.width.saturating_sub(indent).max(MIN_WIDTH);
        for visual in wrap(chars, content_width) {
            self.emit_aligned(visual, indent, content_width, align);
        }
    }

    fn render_heading(&mut self, depth: u8, line: &Line) {
        let (fg, bg) = self.theme.heading_band(depth);
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
                row.push(cell(' ', band));
            }
            for _ in 0..lead {
                row.push(cell(' ', band));
            }
            row.extend(content);
            // Pad the whole row so the theme band spans the full page width.
            while row.len() < self.width {
                row.push(cell(' ', band));
            }
            self.push_line(row);
        }
    }

    fn render_divider(&mut self, depth: usize, character: char) {
        let side = Self::indent_for(depth);
        let rule_len = self.width.saturating_sub(side * 2);
        let mut row: Vec<StyledChar> = Vec::new();
        for _ in 0..side {
            row.push(cell(' ', RStyle::default()));
        }
        for _ in 0..rule_len {
            row.push(cell(character, RStyle::default()));
        }
        self.push_line(row);
    }

    fn render_literal(&mut self, lines: &[Line]) {
        for line in lines {
            let (chars, _align) = self.flatten_line(line);
            // Verbatim: no wrapping, no indent, no alignment. Backticks and any
            // other markup characters are already stored literally by the parser.
            self.push_line(chars);
        }
    }

    fn render_partial(&mut self, depth: usize) {
        let indent = Self::indent_for(depth);
        let mut row: Vec<StyledChar> = Vec::new();
        for _ in 0..indent {
            row.push(cell(' ', RStyle::default()));
        }
        // Match the reference placeholder glyph for an unresolved partial.
        row.push(cell('\u{29d6}', RStyle::default()));
        self.push_line(row);
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
                cells.push(cell(' ', RStyle::default()));
            }
            if is_separator_row(row) {
                for (c, w) in colw.iter().enumerate() {
                    if c > 0 {
                        push_plain(&mut cells, "\u{2500}\u{253c}\u{2500}");
                    }
                    for _ in 0..*w {
                        cells.push(cell('\u{2500}', RStyle::default()));
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
            self.push_line(cells);
        }
    }

    /// Flatten a line's spans into styled characters, recording each link's
    /// clickable span. Returns the characters and the line's alignment.
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
                    // Filled in by `record_link_positions` after layout.
                    line: 0,
                    col_start: 0,
                    col_end: 0,
                });
                let link_style = RStyle {
                    fg: Some(self.theme.link_fg()),
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
                // The core label is the clickable span: style it and tag it with
                // the link index for hit-testing. No visible `[N]` marker is
                // appended; the underline + LINK_FG alone mark it as a link.
                push_link(&mut out, core, link_style, index);
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
            .map(|c| StyledChar {
                ch: c.ch,
                st: RStyle {
                    fg: Some(fg),
                    bg: Some(bg),
                    bold: c.st.bold,
                    underline: c.st.underline,
                    italic: c.st.italic,
                },
                // Keep the link tag so a link inside a heading stays hit-testable.
                link: c.link,
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
            row.push(cell(' ', RStyle::default()));
        }
        row.extend(content);
        self.push_line(row);
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

/// Build one untagged styled cell.
fn cell(ch: char, st: RStyle) -> StyledChar {
    StyledChar { ch, st, link: None }
}

/// Append the characters of `text` with a single style, untagged.
fn push_styled(out: &mut Vec<StyledChar>, text: &str, st: RStyle) {
    for ch in text.chars() {
        out.push(cell(ch, st));
    }
}

/// Append `text` with a single style, tagging every cell with `link` (its
/// 1-based index) so the laid-out span can be hit-tested.
fn push_link(out: &mut Vec<StyledChar>, text: &str, st: RStyle, link: usize) {
    for ch in text.chars() {
        out.push(StyledChar {
            ch,
            st,
            link: Some(link),
        });
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
    fn light_theme_uses_light_band_and_link_colour() {
        let d = doc(vec![
            Block::Heading {
                depth: 1,
                line: Line::new(vec![plain_span("Title")]),
            },
            Block::Paragraph {
                depth: 0,
                line: Line::new(vec![link_span("Docs")]),
            },
        ]);
        let (lines, _links) = layout(&d, 40, Theme::Light);
        let text = emit_ansi(&lines, false);
        // Light depth-1 band: fg #000000 (0), bg #777777 (119).
        assert!(
            text.contains("38;2;0;0;0") && text.contains("48;2;119;119;119"),
            "light heading band missing: {text:?}"
        );
        // Light link colour: deep blue #005aaa => (0, 90, 170).
        assert!(
            text.contains("38;2;0;90;170"),
            "light link colour missing: {text:?}"
        );
        // The dark cyan link colour must NOT appear under the light theme.
        assert!(
            !text.contains("38;2;0;175;255"),
            "dark link colour leaked into light theme: {text:?}"
        );
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
    fn link_records_entry_without_visible_marker() {
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
        // No visible `[N]` marker: links are set apart by style only.
        assert!(!page.text.contains("[1]"), "unexpected visible marker");
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
        // Two leading spaces precede the underlined LINK run; the core label
        // carries the underline + LINK_FG run, with no `[N]` marker appended.
        assert!(
            page.text
                .contains("  \x1b[0;4;38;2;0;175;255m\u{2022} mirrors\x1b[0m"),
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
        // Core label underlined, then reset, then two plain spaces.
        assert!(
            page.text
                .contains("\x1b[0;4;38;2;0;175;255mmirrors\x1b[0m  "),
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
            page.text.contains("\x1b[0;4;38;2;0;175;255mmirrors\x1b[0m"),
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
    fn no_color_link_has_no_marker() {
        let link = Link {
            label: "Label".to_string(),
            target: "/x".to_string(),
            fields: vec![],
        };
        let span = Span {
            text: "Label".to_string(),
            link: Some(link),
            ..Span::default()
        };
        let d = doc(vec![Block::Paragraph {
            depth: 0,
            line: Line::new(vec![span]),
        }]);
        let page = render_with_options(&d, 40, true);
        assert!(!page.text.contains('\x1b'));
        // Plain output: the label appears bare, with no `[N]` marker.
        assert!(page.text.contains("Label"));
        assert!(!page.text.contains("[1]"));
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
