//! The render-agnostic micron document model.
//!
//! Parsing a `.mu` page yields a [`MicronDocument`] made of [`Block`]s. Each
//! block carries one or more [`Line`]s, and each line is a sequence of styled
//! [`Span`]s. No terminal styling (ANSI, colour depth, theme) lives here; that
//! is Phase 3's job. Colours are kept as [`Color`] (raw nibbles plus resolved
//! RGB), alignment and text attributes as plain flags.

use crate::color::Color;

/// A fully parsed micron page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MicronDocument {
    /// Top-level blocks in source order.
    pub blocks: Vec<Block>,
}

/// A block-level element, produced one (or zero) per source line, except
/// [`Block::LiteralBlock`] which groups the lines between two `` `= `` toggles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// A section heading. `depth` is the count of leading `>` (1, 2, 3, ...).
    Heading {
        /// Heading depth (number of leading `>`).
        depth: u8,
        /// The heading's inline content.
        line: Line,
    },
    /// A normal text line. `depth` is the current section depth for indentation.
    Paragraph {
        /// Section depth in effect for this line (for renderer indentation).
        depth: u8,
        /// The paragraph's inline content.
        line: Line,
    },
    /// A horizontal divider drawn with `character` (default `\u{2500}`).
    Divider {
        /// Section depth in effect (for renderer indentation).
        depth: u8,
        /// The character the divider is drawn with.
        character: char,
    },
    /// A literal block: text between two `` `= `` toggles, passed through
    /// verbatim with no inline parsing.
    LiteralBlock {
        /// The verbatim lines of the block.
        lines: Vec<Line>,
    },
    /// An empty source line.
    Blank,
}

/// A single line: an ordered list of styled spans.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    /// The spans making up the line, in source order.
    pub spans: Vec<Span>,
}

impl Line {
    /// Build a line from a vector of spans.
    pub fn new(spans: Vec<Span>) -> Line {
        Line { spans }
    }
}

/// A run of text sharing one [`Style`], optionally carrying a link, a form
/// field, or an anchor declaration.
///
/// A link span's `text` is the link label; a field span carries no display
/// text (the renderer draws a widget from [`Field`]); an anchor span is
/// zero-width and only marks a navigation target.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Span {
    /// The visible text of this span.
    pub text: String,
    /// The style in effect for this span.
    pub style: Style,
    /// A hyperlink, when this span is a `` `[..] `` link.
    pub link: Option<Link>,
    /// A form field, when this span is a `` `<..> `` field.
    pub field: Option<Field>,
    /// An anchor name, when this span is a `` `:name `` anchor declaration.
    pub anchor: Option<String>,
}

/// Text alignment for a span/line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    /// Left aligned (the micron default).
    #[default]
    Left,
    /// Centre aligned.
    Center,
    /// Right aligned.
    Right,
}

/// The visual style of a [`Span`].
///
/// `fg`/`bg` are `None` when no explicit colour is set (renderer picks the
/// theme default), matching the reference's `default` colour handling.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// Foreground colour, or `None` for the default.
    pub fg: Option<Color>,
    /// Background colour, or `None` for the default.
    pub bg: Option<Color>,
    /// Bold flag.
    pub bold: bool,
    /// Underline flag.
    pub underline: bool,
    /// Italic flag.
    pub italic: bool,
    /// Text alignment.
    pub align: Align,
}

/// A hyperlink parsed from `` `[Label`target`f1=v1|f2=v2] ``.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Link {
    /// The link label (falls back to the target when no label is given).
    pub label: String,
    /// The link target (a destination hash / page path).
    pub target: String,
    /// Optional `|`-separated field components carried by the link.
    pub fields: Vec<String>,
}

/// The kind of a form [`Field`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldKind {
    /// A text entry field.
    #[default]
    Text,
    /// A checkbox.
    Checkbox,
    /// A radio button.
    Radio,
}

/// A form field parsed from `` `<flags|name`data> ``.
///
/// Mirrors the reference field parse (`make_output` lines 507-598): a text
/// field's `value` is its initial text; a checkbox/radio's `label` is the
/// displayed label and `value` is the explicit value (falling back to the
/// label when none is given).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Field {
    /// The field name (form key).
    pub name: String,
    /// The display width (default 24, clamped to 256).
    pub width: usize,
    /// Whether input is masked (`!` flag on a text field).
    pub masked: bool,
    /// The field kind.
    pub kind: FieldKind,
    /// The field value: initial text for `Text`, explicit value for
    /// checkbox/radio (label when no explicit value).
    pub value: String,
    /// The checkbox/radio label (empty for text fields).
    pub label: String,
    /// Whether a checkbox/radio starts checked (`*` component).
    pub prechecked: bool,
}
