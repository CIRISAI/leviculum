//! The stateful micron parser.
//!
//! Ported from NomadNet `MicronParser.py`: [`parse`] mirrors
//! `markup_to_attrmaps`/`parse_line` for line-level constructs and
//! `make_output` for inline formatting. Formatting and colour state persists
//! across lines until reset, exactly as in the reference. The parser is
//! lenient: unterminated sequences degrade gracefully and no input can panic.

use crate::color::Color;
use crate::model::{Align, Block, Field, FieldKind, Line, Link, MicronDocument, Span, Style};

/// The persistent parser state, mirroring the reference `default_state`.
///
/// Foreground/background default to `None` (terminal default) rather than a
/// concrete theme colour, keeping the model render-agnostic.
#[derive(Clone, Debug)]
struct State {
    literal: bool,
    depth: u8,
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    underline: bool,
    italic: bool,
    default_align: Align,
    align: Align,
}

impl Default for State {
    fn default() -> State {
        State {
            literal: false,
            depth: 0,
            fg: None,
            bg: None,
            bold: false,
            underline: false,
            italic: false,
            default_align: Align::Left,
            align: Align::Left,
        }
    }
}

impl State {
    /// The current [`Style`], captured at span-emit time (like `make_style`).
    fn style(&self) -> Style {
        Style {
            fg: self.fg.clone(),
            bg: self.bg.clone(),
            bold: self.bold,
            underline: self.underline,
            italic: self.italic,
            align: self.align,
        }
    }
}

/// Parse a micron (`.mu`) document into a [`MicronDocument`].
///
/// Stateful and line-oriented: the document is split on `\n` and each line is
/// processed in turn, with formatting/colour state carried across lines. Never
/// panics on malformed input.
pub fn parse(input: &str) -> MicronDocument {
    let mut state = State::default();
    let mut blocks: Vec<Block> = Vec::new();
    // Consecutive literal lines are grouped into a single LiteralBlock.
    let mut literal_lines: Vec<Line> = Vec::new();

    for raw_line in input.split('\n') {
        // The `= toggle is checked first and works in and out of literal mode,
        // mirroring the reference (parse_line lines 93-96). It emits nothing.
        if raw_line == "`=" {
            let was_literal = state.literal;
            state.literal = !state.literal;
            if was_literal && !state.literal && !literal_lines.is_empty() {
                blocks.push(Block::LiteralBlock {
                    lines: std::mem::take(&mut literal_lines),
                });
            }
            continue;
        }

        if state.literal {
            literal_lines.push(make_literal_line(&state, raw_line));
            continue;
        }

        // Empty lines never reach parse_line in the reference; they are blanks.
        if raw_line.is_empty() {
            blocks.push(Block::Blank);
            continue;
        }

        if let Some(block) = parse_line(raw_line, &mut state) {
            blocks.push(block);
        }
    }

    // An unterminated literal block is flushed at end of input (lenient).
    if !literal_lines.is_empty() {
        blocks.push(Block::LiteralBlock {
            lines: literal_lines,
        });
    }

    MicronDocument { blocks }
}

/// Build a single verbatim literal line (mirrors `make_output` literal branch,
/// lines 446-449): the escaped toggle `` \`= `` is unescaped to `` `= ``.
fn make_literal_line(state: &State, raw_line: &str) -> Line {
    let text = if raw_line == "\\`=" {
        "`=".to_string()
    } else {
        raw_line.to_string()
    };
    Line::new(vec![Span {
        text,
        style: state.style(),
        ..Span::default()
    }])
}

/// Handle one non-empty, non-literal source line (mirrors `parse_line`).
fn parse_line(line: &str, state: &mut State) -> Option<Block> {
    let first = line.chars().next()?;

    // Escape: strip the leading `\` and emit the rest literally-armed.
    if first == '\\' {
        let rest = &line[1..];
        let spans = make_output(state, rest, true);
        if spans.is_empty() {
            return None;
        }
        return Some(Block::Paragraph {
            depth: state.depth,
            line: Line::new(spans),
        });
    }

    // Comment: dropped entirely.
    if first == '#' {
        return None;
    }

    // Section-heading depth reset: reset depth then re-parse the remainder.
    if first == '<' {
        state.depth = 0;
        let rest = &line[1..];
        if rest.is_empty() {
            return None;
        }
        return parse_line(rest, state);
    }

    // Section heading: depth = count of leading `>`.
    if first == '>' {
        let depth = line.bytes().take_while(|&b| b == b'>').count();
        state.depth = depth as u8;
        let rest = &line[depth..];
        if rest.is_empty() {
            return None;
        }
        // The reference temporarily swaps in the theme heading style, renders,
        // then restores the pre-heading colour/format state. We drop the theme
        // (heading styling is applied by depth at render time) but preserve the
        // restore so inline commands inside a heading do not leak onto later
        // lines.
        let saved = (
            state.fg.clone(),
            state.bg.clone(),
            state.bold,
            state.underline,
            state.italic,
        );
        let spans = make_output(state, rest, false);
        state.fg = saved.0;
        state.bg = saved.1;
        state.bold = saved.2;
        state.underline = saved.3;
        state.italic = saved.4;

        if spans.is_empty() {
            return None;
        }
        return Some(Block::Heading {
            depth: state.depth,
            line: Line::new(spans),
        });
    }

    // Horizontal divider: any line starting with `-`. A two-character line
    // `-X` uses X as the divider glyph (control chars fall back to `\u{2500}`);
    // otherwise the default glyph is used. (Reference parse_line lines 148-159;
    // note it keys on the first char, not an exact `-` line.)
    if first == '-' {
        let chars: Vec<char> = line.chars().collect();
        let character = if chars.len() == 2 {
            let c = chars[1];
            if (c as u32) < 32 {
                '\u{2500}'
            } else {
                c
            }
        } else {
            '\u{2500}'
        };
        return Some(Block::Divider {
            depth: state.depth,
            character,
        });
    }

    // Normal text line.
    let spans = make_output(state, line, false);
    if spans.is_empty() {
        return None;
    }
    Some(Block::Paragraph {
        depth: state.depth,
        line: Line::new(spans),
    })
}

/// Parsing sub-mode inside `make_output`.
enum Mode {
    Text,
    Formatting,
}

/// Emit a styled text span with the current state.
fn make_part(state: &State, part: &str) -> Span {
    Span {
        text: part.to_string(),
        style: state.style(),
        ..Span::default()
    }
}

/// Find the position of `needle` in `chars` at or after `from`.
fn find_char(chars: &[char], needle: char, from: usize) -> Option<usize> {
    chars
        .iter()
        .skip(from)
        .position(|&c| c == needle)
        .map(|p| p + from)
}

/// Whether `c` is a valid anchor-name character.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Parse the inline content of a line into spans (mirrors `make_output`).
///
/// `pre_escape` arms the escape state for the first character (used by the
/// line-level `\` escape). Formatting commands mutate `state` so their effect
/// persists across lines.
fn make_output(state: &mut State, line: &str, pre_escape: bool) -> Vec<Span> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut output: Vec<Span> = Vec::new();
    let mut part = String::new();
    let mut mode = Mode::Text;
    let mut escape = pre_escape;
    let mut skip: usize = 0;

    for i in 0..n {
        let c = chars[i];
        if skip > 0 {
            skip -= 1;
        } else {
            match mode {
                Mode::Formatting => {
                    match c {
                        '_' => state.underline = !state.underline,
                        '!' => state.bold = !state.bold,
                        '*' => state.italic = !state.italic,
                        'F' if n >= i + 4 => {
                            let color: String = chars[i + 1..i + 4].iter().collect();
                            state.fg = Some(Color::parse(&color));
                            skip = 3;
                        }
                        'f' => state.fg = None,
                        'B' if n >= i + 4 => {
                            let color: String = chars[i + 1..i + 4].iter().collect();
                            state.bg = Some(Color::parse(&color));
                            skip = 3;
                        }
                        'b' => state.bg = None,
                        '`' => {
                            state.bold = false;
                            state.underline = false;
                            state.italic = false;
                            state.fg = None;
                            state.bg = None;
                            state.align = state.default_align;
                        }
                        'c' => {
                            state.align = if state.align != Align::Center {
                                Align::Center
                            } else {
                                state.default_align
                            };
                        }
                        'l' => {
                            state.align = if state.align != Align::Left {
                                Align::Left
                            } else {
                                state.default_align
                            };
                        }
                        'r' => {
                            state.align = if state.align != Align::Right {
                                Align::Right
                            } else {
                                state.default_align
                            };
                        }
                        'a' => state.align = state.default_align,
                        ':' => {
                            // Anchor declaration `:name` (name until a non-name
                            // char). Not present in this reference version
                            // (which treats `:` as a no-op); implemented here
                            // per the micron spec as a lenient superset.
                            let mut j = i + 1;
                            while j < n && is_name_char(chars[j]) {
                                j += 1;
                            }
                            let name: String = chars[i + 1..j].iter().collect();
                            skip = j - (i + 1);
                            output.push(Span {
                                anchor: Some(name),
                                style: state.style(),
                                ..Span::default()
                            });
                        }
                        '<' => {
                            if let Some((span, consumed)) = parse_field(state, &chars, i) {
                                output.push(span);
                                skip = consumed;
                            }
                        }
                        '[' => {
                            if let Some((span, consumed)) = parse_link(state, &chars, i) {
                                if let Some(span) = span {
                                    output.push(span);
                                }
                                skip = consumed;
                            }
                        }
                        _ => {} // Unknown command: ignored, like the reference.
                    }

                    mode = Mode::Text;
                    if !part.is_empty() {
                        output.push(make_part(state, &part));
                        part.clear();
                    }
                }
                Mode::Text => {
                    if c == '\\' {
                        if escape {
                            part.push('\\');
                            escape = false;
                        } else {
                            escape = true;
                        }
                    } else if c == '`' {
                        if escape {
                            part.push('`');
                            escape = false;
                        } else {
                            mode = Mode::Formatting;
                            if !part.is_empty() {
                                output.push(make_part(state, &part));
                                part.clear();
                            }
                        }
                    } else {
                        part.push(c);
                        escape = false;
                    }
                }
            }
        }

        // End-of-line flush runs every iteration's last index (even under skip).
        if i == n - 1 && !part.is_empty() {
            output.push(make_part(state, &part));
            part.clear();
        }
    }

    output
}

/// Parse a `` `<flags|name`data> `` form field starting at the `<` at index
/// `open`. Returns the span and the number of following characters to skip, or
/// `None` when the field is malformed (missing `` ` `` or `>`). Mirrors the
/// reference field parse (`make_output` lines 507-598).
fn parse_field(state: &State, chars: &[char], open: usize) -> Option<(Span, usize)> {
    let field_start = open + 1;
    let backtick_pos = find_char(chars, '`', field_start)?;
    let field_end = find_char(chars, '>', backtick_pos)?;

    let field_content: String = chars[field_start..backtick_pos].iter().collect();
    let field_data: String = chars[backtick_pos + 1..field_end].iter().collect();

    let mut masked = false;
    let mut width: usize = 24;
    let mut kind = FieldKind::Text;
    let mut name = field_content.clone();
    let mut value = String::new();
    let mut prechecked = false;

    if field_content.contains('|') {
        let components: Vec<&str> = field_content.split('|').collect();
        let mut flags = components[0].to_string();
        // components[0] is flags; components[1] is the name (may be absent).
        name = components.get(1).map(|s| s.to_string()).unwrap_or_default();

        if flags.contains('^') {
            kind = FieldKind::Radio;
            flags = flags.replace('^', "");
        } else if flags.contains('?') {
            kind = FieldKind::Checkbox;
            flags = flags.replace('?', "");
        } else if flags.contains('!') {
            masked = true;
            flags = flags.replace('!', "");
        }

        if !flags.is_empty() {
            if let Ok(w) = flags.parse::<i64>() {
                width = w.clamp(0, 256) as usize;
            }
        }

        if components.len() > 2 {
            value = components[2].to_string();
        }
        if components.len() > 3 && components[3] == "*" {
            prechecked = true;
        }
    }

    let consumed = field_end - open;
    let field = match kind {
        FieldKind::Text => Field {
            name,
            width,
            masked,
            kind,
            value: field_data,
            label: String::new(),
            prechecked: false,
        },
        FieldKind::Checkbox | FieldKind::Radio => {
            let resolved_value = if value.is_empty() {
                field_data.clone()
            } else {
                value
            };
            Field {
                name,
                width,
                masked,
                kind,
                value: resolved_value,
                label: field_data,
                prechecked,
            }
        }
    };

    Some((
        Span {
            field: Some(field),
            style: state.style(),
            ..Span::default()
        },
        consumed,
    ))
}

/// Parse a `` `[Label`target`fields] `` link starting at the `[` at index
/// `open`. Returns `(Option<Span>, skip)`: the span is `None` when the link has
/// no target (reference emits nothing) but the bracket span is still consumed.
/// Returns `None` only when there is no closing `]`. Mirrors `make_output`
/// lines 601-660.
fn parse_link(state: &State, chars: &[char], open: usize) -> Option<(Option<Span>, usize)> {
    let close = find_char(chars, ']', open)?;
    let endpos = close - open; // offset of `]` from `[`, matches reference skip.
    let link_data: String = chars[open + 1..close].iter().collect();

    let components: Vec<&str> = link_data.split('`').collect();
    let (label, url, fields_raw) = match components.len() {
        1 => (String::new(), components[0].to_string(), String::new()),
        2 => (
            components[0].to_string(),
            components[1].to_string(),
            String::new(),
        ),
        3 => (
            components[0].to_string(),
            components[1].to_string(),
            components[2].to_string(),
        ),
        _ => (String::new(), String::new(), String::new()),
    };

    if url.is_empty() {
        // No target: reference emits no output but still consumes the bracket.
        return Some((None, endpos));
    }

    let label = if label.is_empty() { url.clone() } else { label };
    let fields = if fields_raw.is_empty() {
        Vec::new()
    } else {
        fields_raw.split('|').map(|s| s.to_string()).collect()
    };

    Some((
        Some(Span {
            text: label.clone(),
            link: Some(Link {
                label,
                target: url,
                fields,
            }),
            style: state.style(),
            ..Span::default()
        }),
        endpos,
    ))
}
