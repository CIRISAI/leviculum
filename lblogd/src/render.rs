//! Renderers: Markdown to HTML and Markdown to Micron, plus the complete
//! index/post page templates for both output formats.
//!
//! The HTML side is a thin wrapper over pulldown-cmark's HTML renderer wrapped
//! in a minimal, theme-neutral document template with a tiny inline stylesheet.
//!
//! The Micron side walks the pulldown-cmark event stream and emits micron
//! markup as defined by the `leviculum-micron` parser (the authority on what
//! valid micron is). The mapping:
//!
//! | Markdown                | Micron                                        |
//! |-------------------------|-----------------------------------------------|
//! | heading level 1/2/3     | `>` / `>>` / `>>>` (deeper clamped to `>>>`)  |
//! | `**bold**`              | `` `! `` toggles                              |
//! | `*italic*`              | `` `* `` toggles                              |
//! | `` `inline code` ``     | `` `B333 `` background toggle (see below)     |
//! | fenced/indented code    | `` `= `` literal block                        |
//! | `[text](url)`           | `` `[text`url] ``                             |
//! | bullet list             | `\u{2022} item` lines, nested lists indented  |
//! | numbered list           | `1. item` lines, nested lists indented        |
//! | `---` rule              | `-` divider line                              |
//! | paragraph break         | blank line                                    |
//! | hard break              | line break                                    |
//!
//! Degradations (micron has no equivalent; never panics):
//!
//! - inline code: micron has no inline literal, so code is set off with a
//!   `` `B333 `` background colour toggle (a dark neutral that reads on the
//!   dark NomadNet default theme) and closed with `` `b ``
//! - images: `[image: alt]` plain text (or `[image]` with no alt text)
//! - tables: plaintext rows, cells joined with ` | ` (micron's `` `t `` table
//!   is a NomadNet extension still stubbed in our parser, so we stay plain)
//! - blockquotes: two-space indented text per nesting level
//! - raw HTML: emitted as escaped plain text
//! - strikethrough/footnotes/task lists: extensions not enabled, so their
//!   syntax passes through as plain text
//!
//! Plain text is escaped so it can never be misread as micron markup:
//! backslashes and backticks are `\`-escaped inline, and a text line that
//! would start with a line-level control character (`>`, `#`, `-`, `<`) gets
//! a leading `\` line escape.

use pulldown_cmark::{html, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::post::Post;

/// Micron heading depth is meaningful for 1-3 `>`; deeper Markdown headings
/// clamp here.
const MAX_MICRON_HEADING_DEPTH: usize = 3;

/// The micron background colour used to set off inline code (12-bit form).
const INLINE_CODE_BG: &str = "333";

/// The pulldown-cmark options used by both renderers. Tables are the only
/// extension: everything else degrades better as plain text.
fn markdown_options() -> Options {
    Options::ENABLE_TABLES
}

/// Render a Markdown fragment to an HTML fragment (no surrounding document).
pub fn markdown_to_html(md: &str) -> String {
    let parser = Parser::new_ext(md, markdown_options());
    let mut out = String::new();
    html::push_html(&mut out, parser);
    out
}

/// Escape text for inclusion in HTML element content or attribute values.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The shared minimal inline stylesheet. Theme-neutral and readable.
const STYLE: &str = "\
body{margin:0 auto;max-width:42rem;padding:1rem;font-family:system-ui,sans-serif;\
line-height:1.6;color:#222;background:#fdfdfd}\
h1,h2,h3{line-height:1.25}\
code,pre{font-family:ui-monospace,monospace;background:#eee}\
pre{padding:.75rem;overflow-x:auto}\
a{color:#1a5fb4}\
.date{color:#666;font-size:.9rem}\
ul.posts{list-style:none;padding:0}\
ul.posts li{margin:.5rem 0}";

/// Wrap `body` in a complete minimal HTML document titled `title`.
fn html_document(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{}</title>\n<style>{}</style>\n</head>\n<body>\n{}\n</body>\n</html>\n",
        escape_html(title),
        STYLE,
        body
    )
}

/// Render the post index as a complete HTML document. Posts link to
/// `/posts/<slug>` (the HTTP route shape; wired up in a later batch).
pub fn render_index_html(posts: &[Post]) -> String {
    let mut body = String::from("<h1>Posts</h1>\n<ul class=\"posts\">\n");
    for post in posts {
        body.push_str(&format!(
            "<li><span class=\"date\">{}</span> <a href=\"/posts/{}\">{}</a></li>\n",
            post.date,
            escape_html(&post.slug),
            escape_html(&post.title)
        ));
    }
    body.push_str("</ul>");
    html_document("Posts", &body)
}

/// Render one post as a complete HTML document.
pub fn render_post_html(post: &Post) -> String {
    let body = format!(
        "<article>\n<h1>{}</h1>\n<p class=\"date\">{}</p>\n{}</article>",
        escape_html(&post.title),
        post.date,
        markdown_to_html(&post.body_md)
    );
    html_document(&post.title, &body)
}

/// Convert a Markdown fragment to valid micron markup. See the module docs
/// for the mapping and degradation table. Never panics.
pub fn markdown_to_micron(md: &str) -> String {
    let parser = Parser::new_ext(md, markdown_options());
    let mut writer = MicronWriter::default();
    for event in parser {
        writer.event(event);
    }
    writer.finish()
}

/// Render the post index as a micron page: a heading plus one link per post
/// targeting the local page `:/page/<slug>.mu` (NomadNet's same-node link
/// form, as resolved by lnomad and NomadNet).
pub fn render_index_micron(posts: &[Post]) -> String {
    let mut out = String::from(">Posts\n\n");
    for post in posts {
        out.push_str(&format!(
            "`[{}`:/page/{}.mu]\n",
            sanitize_link_part(&format!("{} {}", post.date, post.title)),
            sanitize_link_part(&post.slug)
        ));
    }
    out
}

/// Render one post as a micron page: title heading, date line, divider, body.
pub fn render_post_micron(post: &Post) -> String {
    format!(
        ">{}\n\n{}\n-\n\n{}",
        escape_micron_text(&post.title),
        post.date,
        markdown_to_micron(&post.body_md)
    )
}

/// Escape plain text so micron's inline parser reads it verbatim: `\` and
/// `` ` `` are `\`-escaped.
fn escape_micron_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '`' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Sanitize text for use inside a micron `` `[label`target] `` link, whose
/// contents run raw to the closing bracket: backticks and `]` would end the
/// label/target early, so they degrade to close lookalikes.
fn sanitize_link_part(s: &str) -> String {
    s.replace('`', "'").replace(']', ")")
}

/// Line-level micron control characters: a plain-text line must not start
/// with one of these, or it would parse as a heading (`>`), comment (`#`),
/// divider (`-`) or depth reset (`<`). `` ` `` needs no entry because inline
/// escaping already turns it into `` \` ``.
const LINE_CONTROL_CHARS: [char; 4] = ['>', '#', '-', '<'];

/// The streaming Markdown-event-to-micron writer.
#[derive(Default)]
struct MicronWriter {
    /// Finished output lines.
    out: Vec<String>,
    /// The line being built.
    line: String,
    /// Whether `line` began with plain text (needs the line-start escape
    /// check at flush) rather than with markup we emitted deliberately.
    line_is_text: bool,
    /// Blockquote nesting depth; each level indents flushed lines two spaces.
    quote_depth: usize,
    /// Open lists: `None` for a bullet list, `Some(next_index)` for numbered.
    list_stack: Vec<Option<u64>>,
    /// Inside a `` `= `` literal block: lines pass through verbatim.
    in_code_block: bool,
    /// Target URL of the link currently open, if any.
    link_url: Option<String>,
    /// Label text buffered while a link is open.
    link_label: String,
    /// Alt text buffered while an image is open.
    image_alt: Option<String>,
    /// Cells emitted so far in the current (degraded) table row.
    table_cells: usize,
}

impl MicronWriter {
    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => self.inline_code(&t),
            Event::Html(t) | Event::InlineHtml(t) => self.text(&t),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.block_sep();
                self.push_raw("-");
                self.flush_line();
            }
            // Extensions we do not enable; listed for totality, never emitted.
            Event::FootnoteReference(_)
            | Event::TaskListMarker(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            // A paragraph opening right after a list-item marker (or a table
            // cell) continues that line; otherwise it starts a fresh,
            // blank-separated block.
            Tag::Paragraph if self.line.is_empty() => self.block_sep(),
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.block_sep();
                let depth = (level as usize).min(MAX_MICRON_HEADING_DEPTH);
                self.push_raw(&">".repeat(depth));
            }
            Tag::BlockQuote(_) => {
                self.block_sep();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(CodeBlockKind::Fenced(_) | CodeBlockKind::Indented) => {
                self.block_sep();
                self.out.push("`=".to_string());
                self.in_code_block = true;
            }
            Tag::List(start) => {
                if self.list_stack.is_empty() {
                    self.block_sep();
                } else {
                    // A nested list starts inside its parent item's line.
                    self.flush_line();
                }
                self.list_stack.push(start);
            }
            Tag::Item => {
                self.flush_line();
                let indent = "  ".repeat(self.list_stack.len().saturating_sub(1));
                let marker = match self.list_stack.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "\u{2022} ".to_string(),
                };
                self.push_raw(&format!("{indent}{marker}"));
            }
            Tag::Table(_) => self.block_sep(),
            Tag::TableHead | Tag::TableRow => {
                self.flush_line();
                self.table_cells = 0;
            }
            Tag::TableCell => {
                if self.table_cells > 0 {
                    self.push_raw(" | ");
                }
                self.table_cells += 1;
            }
            Tag::Emphasis => self.style_toggle("`*"),
            Tag::Strong => self.style_toggle("`!"),
            Tag::Link { dest_url, .. } => {
                self.link_url = Some(dest_url.to_string());
                self.link_label.clear();
            }
            Tag::Image { .. } => self.image_alt = Some(String::new()),
            Tag::HtmlBlock => self.block_sep(),
            // Extensions we do not enable (footnotes, definition lists,
            // strikethrough, sub/superscript, metadata): contents degrade to
            // the plain text events pulldown-cmark still emits.
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item | TagEnd::HtmlBlock => {
                self.flush_line();
            }
            TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if !self.line.is_empty() {
                    self.flush_code_line();
                }
                self.in_code_block = false;
                self.out.push("`=".to_string());
            }
            TagEnd::List(_) => {
                self.flush_line();
                self.list_stack.pop();
            }
            TagEnd::TableHead | TagEnd::TableRow => self.flush_line(),
            TagEnd::Emphasis => self.style_toggle("`*"),
            TagEnd::Strong => self.style_toggle("`!"),
            TagEnd::Link => {
                let url = sanitize_link_part(&self.link_url.take().unwrap_or_default());
                let label = sanitize_link_part(self.link_label.trim());
                let label = if label.is_empty() { url.clone() } else { label };
                self.push_raw(&format!("`[{label}`{url}]"));
            }
            TagEnd::Image => {
                let alt = self.image_alt.take().unwrap_or_default();
                if alt.trim().is_empty() {
                    self.push_text("[image]");
                } else {
                    self.push_text(&format!("[image: {}]", alt.trim()));
                }
            }
            _ => {}
        }
    }

    /// Route text to whatever is currently collecting it: image alt, link
    /// label, literal block, or the current line (escaped). Embedded
    /// newlines (raw HTML, code text) split lines.
    fn text(&mut self, t: &str) {
        if let Some(alt) = self.image_alt.as_mut() {
            alt.push_str(t);
            return;
        }
        if self.link_url.is_some() {
            self.link_label.push_str(t);
            return;
        }
        for (i, segment) in t.split('\n').enumerate() {
            if i > 0 {
                if self.in_code_block {
                    self.flush_code_line();
                } else {
                    self.flush_line();
                }
            }
            if self.in_code_block {
                self.line.push_str(segment);
            } else if !segment.is_empty() {
                self.push_text(segment);
            }
        }
    }

    /// Inline code: no micron inline literal exists, so set it off with a
    /// background colour toggle (degradation documented in the module docs).
    fn inline_code(&mut self, code: &str) {
        if let Some(alt) = self.image_alt.as_mut() {
            alt.push_str(code);
            return;
        }
        if self.link_url.is_some() {
            self.link_label.push_str(code);
            return;
        }
        self.push_raw(&format!("`B{INLINE_CODE_BG}"));
        self.push_text(code);
        self.push_raw("`b");
    }

    /// Emit a style toggle unless a link/image is collecting text (labels run
    /// raw to the closing bracket, so styles inside them are dropped).
    fn style_toggle(&mut self, toggle: &str) {
        if self.image_alt.is_none() && self.link_url.is_none() {
            self.push_raw(toggle);
        }
    }

    /// Append micron markup we emit deliberately (never escaped).
    fn push_raw(&mut self, s: &str) {
        if self.line.is_empty() {
            self.line_is_text = false;
        }
        self.line.push_str(s);
    }

    /// Append plain text, escaped so micron reads it verbatim.
    fn push_text(&mut self, s: &str) {
        if self.line.is_empty() {
            self.line_is_text = true;
        }
        self.line.push_str(&escape_micron_text(s));
    }

    /// Finish the current line: line-escape a leading control character on
    /// plain-text lines, apply blockquote indentation, and emit.
    fn flush_line(&mut self) {
        if self.line.is_empty() {
            return;
        }
        let mut line = std::mem::take(&mut self.line);
        if self.line_is_text && line.starts_with(LINE_CONTROL_CHARS) {
            line.insert(0, '\\');
        }
        if self.quote_depth > 0 {
            line = format!("{}{line}", "  ".repeat(self.quote_depth));
        }
        self.out.push(line);
    }

    /// Finish one verbatim literal-block line. Only the block toggle itself
    /// needs care: a content line reading `` `= `` must be emitted as
    /// `` \`= `` (the parser unescapes it inside literal blocks).
    fn flush_code_line(&mut self) {
        let line = std::mem::take(&mut self.line);
        if line == "`=" {
            self.out.push("\\`=".to_string());
        } else {
            self.out.push(line);
        }
    }

    /// Separate blocks with one blank line (never at the start of output).
    fn block_sep(&mut self) {
        self.flush_line();
        if self.out.last().is_some_and(|l| !l.is_empty()) {
            self.out.push(String::new());
        }
    }

    /// Flush pending state and return the final micron source.
    fn finish(mut self) -> String {
        self.flush_line();
        let mut out = self.out.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }
}
