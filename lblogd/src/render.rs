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
//! | `~~struck~~`            | `` `F777 `` dim foreground toggle             |
//! | task list item          | `\u{2611}` / `\u{2610}` instead of the bullet      |
//! | `==highlight==`         | `` `B550 `` background toggle                 |
//! | `H~2~O`, `X^2^`         | Unicode `H\u{2082}O`, `X\u{b2}` where they exist       |
//! | definition list         | bold term, definitions indented two spaces    |
//! | footnote `[^1]`         | `[1]` marker, definitions at the end of page  |
//!
//! Degradations (micron has no equivalent; never panics):
//!
//! - inline code: micron has no inline literal, so code is set off with a
//!   `` `B333 `` background colour toggle (a dark neutral that reads on the
//!   dark NomadNet default theme) and closed with `` `b ``
//! - images: an image naming a file in the file area becomes a link to it
//!   (`` `[alt`:/file/name] ``), which is the only form micron has; any other
//!   image (an external URL) stays `[image: alt]` plain text
//! - tables: emitted as a micron `` `t `` table — a header row, the alignment
//!   row micron reads as the table's second line, then the data rows, cells
//!   separated by `|` with any literal pipe escaped
//! - blockquotes: two-space indented text per nesting level
//! - raw HTML: emitted as escaped plain text, except `<mark>`, `<del>`,
//!   `<s>`, `<sub>` and `<sup>`, which have a micron mapping and take the
//!   same one their Markdown spelling gets
//! - strikethrough: micron has no struck text, so it is dimmed instead —
//!   the one signal micron has for "this no longer applies"
//! - footnotes: micron has neither anchors nor in-page links, so a reference
//!   becomes the bare marker `[1]` and every definition is collected into a
//!   block behind a divider at the end of the page, in source order. Nothing
//!   is lost; only the jump between the two is
//! - heading identifiers (`## Text {#id}`): the HTML side emits the `id`, the
//!   micron side has nothing to attach it to and drops it — there is no
//!   in-page anchor in micron to link to it with either
//! - sub/superscript: micron has no baseline shift. A run of digits and
//!   arithmetic signs has a complete Unicode equivalent and uses it
//!   (`H\u{2082}O`, `X\u{b2}`); anything else keeps its `^`/`~` markers as plain
//!   text, because Unicode's superscript alphabet has holes
//! - emoji shortcodes (`:joy:`): declined. Rendering them faithfully needs
//!   the full CLDR shortcode table as a new dependency, and a hand-picked
//!   subset would render some shortcodes and leave the rest literal, which
//!   reads worse than leaving all of them literal. They stay as written
//!
//! Syntax from the cheat sheet that the parser does not implement is added
//! by `extended_events` on the event stream both renderers read, so the two
//! sides cannot drift apart: bare URLs and e-mail addresses become links,
//! `==x==` becomes a highlight, and the cheat sheet's intra-word `H~2~O` /
//! `X^2^` become sub/superscript (pulldown-cmark's own extension takes only
//! the flanked form `H ~2~ O`). Each is recognised within a single text run
//! and never inside code or a link label; a pair split by other inline markup
//! stays literal, which is plain text rather than a half-open construct.
//!
//! Plain text is escaped so it can never be misread as micron markup:
//! backslashes and backticks are `\`-escaped inline, and a text line that
//! would start with a line-level control character (`>`, `#`, `-`, `<`) gets
//! a leading `\` line escape.

use pulldown_cmark::{
    html, Alignment, CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd,
};

use crate::files;
use crate::post::{slugify, Date, Post};

/// Micron heading depth is meaningful for 1-3 `>`; deeper Markdown headings
/// clamp here.
const MAX_MICRON_HEADING_DEPTH: usize = 3;

/// What a reader learns about the blog itself, independent of any one post.
///
/// Assembled once from the config and the resolved destination, then rendered
/// into every page on both sides. Optional fields are simply omitted when
/// absent rather than rendered empty, so a minimal configuration produces a
/// clean page rather than a page with blanks in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlogMeta {
    /// The blog's name; the heading of every page.
    pub title: String,
    /// Who writes it, shown unless a post names its own author.
    pub author: Option<String>,
    /// One sentence on what this is.
    pub description: Option<String>,
    /// BCP 47 language tag for the HTML `lang` attribute.
    pub language: String,
    /// The blog's clearnet URL, shown on the NomadNet side so mesh readers
    /// can find the web version.
    pub web_url: Option<String>,
    /// The blog's NomadNet destination hash, shown on the web side so
    /// clearnet readers can find the mesh version.
    pub nomadnet_address: Option<String>,
    /// Contact address for the about page.
    pub email: Option<String>,
    /// LXMF destination hash for the about page, 32 hex characters.
    pub lxmf: Option<String>,
    /// Whether an about page exists, and therefore whether the author's name
    /// is a link.
    ///
    /// It exists as soon as there is anything to put on it: an address, a
    /// hash, or a text file. Without any of those a link would lead to an
    /// empty page, so the name stays plain text.
    pub has_about: bool,
    /// Whether a landing page has taken the site root, which moves the post
    /// index to `/blog` and `:/page/blog.mu`.
    pub has_landing: bool,
    /// The nav line, in order, or empty when there is nowhere to navigate to
    /// besides the posts themselves.
    ///
    /// Empty is the state of every blog configured before pages and links
    /// existed, and it renders to nothing at all — which is what keeps those
    /// blogs' served bytes exactly what they were.
    pub nav: Vec<NavEntry>,
    /// What every page tells a reader about the program serving it: its
    /// version, its licence, and where its source is.
    ///
    /// Not an `Option`, because there is no configuration in which this is
    /// absent — see [`SourceOffer`].
    pub source: SourceOffer,
}

/// The source offer AGPL section 13 requires, carried by every page on both
/// sides.
///
/// Section 13 obliges anyone who lets users interact with a modified AGPL
/// program "remotely through a computer network" to offer those users the
/// Corresponding Source, prominently. A blog server is that case in its
/// purest form: every reader of a served page is such a user, on the web and
/// on the mesh alike. An operator therefore has to publish the offer, and the
/// software they publish it with is this one.
///
/// So the offer is not a feature to switch on. It has a [`Default`] that is
/// the whole compliant answer for an unmodified build — this crate's version,
/// its licence, its repository — and it is rendered unconditionally. What is
/// configurable is only the URL, and it has to be: an operator running a
/// *modified* lblogd owes their readers their own tree, and pointing them at
/// ours would be a false offer rather than a compliant one.
///
/// There is deliberately no way to suppress it. An operator who needs the
/// footer gone is one who has stopped meeting section 13, and a config key
/// for that would be lblogd helping them do it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceOffer {
    /// The running build, as `lblogd --version` reports it. Named so the
    /// offer points at the source of *this* binary rather than at whatever
    /// happens to be at the top of the repository.
    pub version: String,
    /// The licence the program is under, as an SPDX expression.
    pub license: String,
    /// Where the Corresponding Source is.
    pub url: String,
}

/// The program name the footer uses. Its own constant rather than
/// `CARGO_PKG_NAME` inline, so the two renderers cannot disagree.
const PROGRAM_NAME: &str = env!("CARGO_PKG_NAME");

impl Default for SourceOffer {
    /// The offer an unmodified build makes: this crate's own metadata.
    ///
    /// Hand-written rather than derived because the derived default would be
    /// three empty strings — a footer that names no licence and links
    /// nowhere, which is worse than no footer at all. Every `BlogMeta` built
    /// anywhere, including in tests, therefore carries a real offer.
    fn default() -> SourceOffer {
        SourceOffer {
            // build.rs, not CARGO_PKG_VERSION: the shipped string carries the
            // nightly build id and the git hash, so the offer names the exact
            // commit whose source corresponds to the running binary.
            version: env!("LEVICULUM_VERSION").to_string(),
            license: env!("CARGO_PKG_LICENSE").to_string(),
            url: env!("CARGO_PKG_REPOSITORY").to_string(),
        }
    }
}

impl SourceOffer {
    /// The offer as one sentence of plain text, without markup.
    ///
    /// Both renderers build their line from this, so the web reader and the
    /// mesh reader are told the same thing in the same words; only the link
    /// differs, because only one of the two sides has links.
    fn sentence(&self) -> String {
        format!(
            "Served by {PROGRAM_NAME} {}, free software under {}.",
            self.version, self.license
        )
    }
}

/// One entry of the nav line: what it is called and where it points on each
/// side.
///
/// Both paths are resolved once, when the snapshot is built, rather than at
/// render time: the two sides must agree about where a page lives, and the
/// surest way to make them agree is to give them one list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NavEntry {
    /// What the link says.
    pub label: String,
    /// Its HTTP path.
    pub web: String,
    /// Its micron request target, including the leading `:`.
    pub micron: String,
}

impl BlogMeta {
    /// The author to credit for `post`: its own, else the blog's.
    fn author_of<'a>(&'a self, post: &'a Post) -> Option<&'a str> {
        post.author.as_deref().or(self.author.as_deref())
    }

    /// The blog author's name as HTML, linked to the about page when there is
    /// one.
    ///
    /// Only the blog's own author is linked. A guest author's name pointing
    /// at the blog author's about page would simply be wrong, and a page per
    /// author is more machinery than a blog with one writer needs.
    fn author_html(&self, name: &str) -> String {
        let escaped = escape_html(name);
        match self.has_about && Some(name) == self.author.as_deref() {
            true => format!("<a href=\"{ABOUT_HTML_PATH}\">{escaped}</a>"),
            false => escaped,
        }
    }

    /// The post index's HTTP path: the site root, unless a landing page has
    /// taken it.
    pub fn index_html_path(&self) -> &'static str {
        match self.has_landing {
            true => BLOG_HTML_PATH,
            false => "/",
        }
    }

    /// The post index's micron request target, the mirror of
    /// [`index_html_path`](Self::index_html_path).
    pub fn index_micron_target(&self) -> &'static str {
        match self.has_landing {
            true => BLOG_MICRON_TARGET,
            false => INDEX_MICRON_TARGET,
        }
    }

    /// The same for micron, linking to the local about page.
    fn author_micron(&self, name: &str) -> String {
        match self.has_about && Some(name) == self.author.as_deref() {
            true => format!("`[{}`{ABOUT_MICRON_PATH}]", sanitize_link_part(name)),
            false => escape_micron_text(name),
        }
    }
}

/// The HTTP path of the about page.
pub const ABOUT_HTML_PATH: &str = "/about";

/// The micron request path of the about page.
pub const ABOUT_MICRON_PATH: &str = ":/page/about.mu";

/// The HTTP path the post index moves to once a landing page takes `/`.
pub const BLOG_HTML_PATH: &str = "/blog";

/// The micron request target of the post index once a landing page takes
/// `:/page/index.mu`.
pub const BLOG_MICRON_TARGET: &str = ":/page/blog.mu";

/// The micron request target of the site root: the landing page when there is
/// one, the post index otherwise.
pub const INDEX_MICRON_TARGET: &str = ":/page/index.mu";

/// The micron background colour used to set off inline code (12-bit form).
const INLINE_CODE_BG: &str = "333";

/// The micron background colour of highlighted text: the closest micron has
/// to a marker pen, dark enough that the default foreground still reads.
const HIGHLIGHT_BG: &str = "550";

/// The micron foreground colour of struck text. Micron has no strikethrough,
/// and dimming is the one signal it has for "no longer applies".
const STRIKE_FG: &str = "777";

/// The pulldown-cmark options used by both renderers: every extension of the
/// standard Markdown feature set the parser implements. What it does not
/// implement is added by [`extended_events`].
///
/// Smart punctuation, math, wikilinks and metadata blocks stay off: none of
/// them belongs to that feature set, and a metadata block would additionally
/// swallow the `+++` front matter every post already carries.
fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT
}

/// Render a Markdown fragment to an HTML fragment (no surrounding document).
pub fn markdown_to_html(md: &str) -> String {
    let events = extended_events(md)
        .into_iter()
        .map(demote_heading)
        .map(resolve_file_ref);
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// The parser's event stream with adjacent text runs joined and the cheat
/// sheet syntax pulldown-cmark does not implement expanded into events both
/// renderers already understand (see the module docs).
///
/// Joining the runs first is what makes the expansion see whole words: the
/// parser hands out `X^2` and `^` as two events when it has rejected a
/// superscript, and a pass looking at them one at a time would find no pair.
///
/// Text inside a code block, a link label or an image's alt text is left
/// exactly as the parser produced it: a URL in a link label must not become a
/// second link, and code is quoted precisely so nothing rewrites it.
fn extended_events(md: &str) -> Vec<Event<'_>> {
    let mut out = Vec::new();
    let mut run = String::new();
    let mut literal = 0usize;
    for event in Parser::new_ext(md, markdown_options()) {
        if let Event::Text(text) = &event {
            run.push_str(text);
            continue;
        }
        // Flushed before the depth moves, so the run is expanded (or not)
        // under the rule that held where it stood.
        push_run(&mut run, literal > 0, &mut out);
        match &event {
            Event::Start(Tag::CodeBlock(_) | Tag::Link { .. } | Tag::Image { .. }) => literal += 1,
            Event::End(TagEnd::CodeBlock | TagEnd::Link | TagEnd::Image) => {
                literal = literal.saturating_sub(1)
            }
            _ => {}
        }
        out.push(event);
    }
    push_run(&mut run, literal > 0, &mut out);
    out
}

/// Emit one collected text run, expanded unless it is quoted text.
fn push_run<'a>(run: &mut String, literal: bool, out: &mut Vec<Event<'a>>) {
    if run.is_empty() {
        return;
    }
    let text = std::mem::take(run);
    match literal {
        true => out.push(Event::Text(text.into())),
        false => expand_text(&text, out),
    }
}

/// Expand one prose text run into events: `==highlight==`, the cheat sheet's
/// intra-word `H~2~O` / `X^2^`, and bare URLs and e-mail addresses.
///
/// Sub- and superscript are emitted as the inline HTML the parser's own
/// extension would have produced, and the micron writer maps those tags the
/// same way it maps the parser's. One expansion, both sides.
fn expand_text<'a>(text: &str, out: &mut Vec<Event<'a>>) {
    let mut plain = String::new();
    let mut i = 0;
    while let Some(c) = text[i..].chars().next() {
        let rest = &text[i..];
        // `==highlight==`. The content is expanded in turn, so a formula or a
        // URL inside a highlight is still one.
        if let Some(inner) = rest
            .strip_prefix("==")
            .and_then(|r| delimited(r, "=="))
            .filter(|inner| !inner.is_empty())
        {
            flush_plain(&mut plain, out);
            out.push(Event::InlineHtml("<mark>".into()));
            expand_text(inner, out);
            out.push(Event::InlineHtml("</mark>".into()));
            i += inner.len() + 4;
            continue;
        }
        // `H~2~O` and `X^2^`. Only the intra-word form: everything else is
        // the parser's, and taking it here would catch `~5 or ~10`.
        if matches!(c, '~' | '^') && plain.chars().next_back().is_some_and(char::is_alphanumeric) {
            let (delim, open, close) = match c {
                '^' => ("^", "<sup>", "</sup>"),
                _ => ("~", "<sub>", "</sub>"),
            };
            if let Some(inner) = delimited(&rest[1..], delim) {
                if !inner.is_empty() && !inner.contains(char::is_whitespace) {
                    flush_plain(&mut plain, out);
                    out.push(Event::InlineHtml(open.into()));
                    out.push(Event::Text(inner.to_string().into()));
                    out.push(Event::InlineHtml(close.into()));
                    i += inner.len() + 2;
                    continue;
                }
            }
        }
        // A bare URL, at a word boundary so `xhttps://...` stays text.
        if !plain.chars().next_back().is_some_and(char::is_alphanumeric) {
            let url = bare_url(rest);
            if !url.is_empty() {
                flush_plain(&mut plain, out);
                push_autolink(url, url, out);
                i += url.len();
                continue;
            }
        }
        // A bare e-mail address, found at its `@` and completed backwards out
        // of the text already collected.
        if c == '@' {
            if let Some((local_len, domain)) = bare_email(&plain, &rest[1..]) {
                let local = plain.split_off(plain.len() - local_len);
                let address = format!("{local}@{domain}");
                flush_plain(&mut plain, out);
                push_autolink(&format!("mailto:{address}"), &address, out);
                i += domain.len() + 1;
                continue;
            }
        }
        plain.push(c);
        i += c.len_utf8();
    }
    flush_plain(&mut plain, out);
}

/// The text up to the next `delim`, or `None` when the run does not close.
fn delimited<'a>(s: &'a str, delim: &str) -> Option<&'a str> {
    s.find(delim).map(|end| &s[..end])
}

/// Emit what has been collected as plain text, if anything.
fn flush_plain<'a>(plain: &mut String, out: &mut Vec<Event<'a>>) {
    if !plain.is_empty() {
        out.push(Event::Text(std::mem::take(plain).into()));
    }
}

/// Emit a link whose label is the address itself.
fn push_autolink<'a>(dest: &str, label: &str, out: &mut Vec<Event<'a>>) {
    out.push(Event::Start(Tag::Link {
        link_type: LinkType::Autolink,
        dest_url: dest.to_string().into(),
        title: String::new().into(),
        id: String::new().into(),
    }));
    out.push(Event::Text(label.to_string().into()));
    out.push(Event::End(TagEnd::Link));
}

/// The bare `http(s)` URL starting at `s`, or `""` when there is none.
///
/// Sentence punctuation after a URL belongs to the sentence, and a closing
/// bracket only belongs to the URL when the URL opened one: `(see
/// https://example.com/a)` is the common case and its `)` is not part of the
/// address.
fn bare_url(s: &str) -> &str {
    if !(s.starts_with("http://") || s.starts_with("https://")) {
        return "";
    }
    let end = s
        .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`'))
        .unwrap_or(s.len());
    let mut url = &s[..end];
    loop {
        let trimmed = url.trim_end_matches(['.', ',', ';', ':', '!', '?']);
        let trimmed = match trimmed.ends_with(')')
            && trimmed.matches(')').count() > trimmed.matches('(').count()
        {
            true => &trimmed[..trimmed.len() - 1],
            false => trimmed,
        };
        if trimmed.len() == url.len() {
            // A scheme with nothing after it is not an address.
            return match url.ends_with("//") {
                true => "",
                false => url,
            };
        }
        url = trimmed;
    }
}

/// The e-mail address around an `@`: how many bytes of `plain` its local part
/// takes, and the domain that follows.
///
/// The domain has to end in a plausible TLD; without that rule every `@`
/// followed by a word would become a link.
fn bare_email<'a>(plain: &str, after: &'a str) -> Option<(usize, &'a str)> {
    let local_len = plain.len() - plain.trim_end_matches(is_email_local).len();
    let local = &plain[plain.len() - local_len..];
    if local.is_empty() || local.starts_with('.') {
        return None;
    }
    let end = after
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '.' | '-')))
        .unwrap_or(after.len());
    let domain = after[..end].trim_end_matches(['.', '-']);
    let (_, tld) = domain.rsplit_once('.')?;
    match tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()) {
        true => Some((local_len, domain)),
        false => None,
    }
}

/// Whether `c` may appear in the local part of an e-mail address.
fn is_email_local(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

/// Point an image or link at the file area's web route when it names a file
/// there.
///
/// An author writes `![Antenne](antenne.jpg)` and means "the picture next to
/// my posts". On the web that is `/files/antenne.jpg`; on the mesh the same
/// reference becomes a `:/file/antenne.jpg` link (see
/// [`MicronWriter::end`]). Anything [`files::file_ref`] does not recognise —
/// an external URL above all — is left exactly as written.
fn resolve_file_ref(event: Event<'_>) -> Event<'_> {
    match event {
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: match files::file_ref(&dest_url) {
                Some(name) => files::web_path(&name).into(),
                None => dest_url,
            },
            title,
            id,
        }),
        other => other,
    }
}

/// Push a Markdown heading down one level.
///
/// The page template already gives the post its `<h1>`, so a `# Heading` in
/// the body would produce a second one and leave the document with two
/// competing top-level headings. Demoting means an author can write `#` for
/// their first section, as Markdown habit dictates, and still get a correctly
/// nested document.
///
/// Both the start and the end event carry the level, and moving only one of
/// them emits mismatched tags like `<h2>Text</h1>`.
fn demote_heading(event: Event<'_>) -> Event<'_> {
    match event {
        Event::Start(Tag::Heading {
            level,
            id,
            classes,
            attrs,
        }) => Event::Start(Tag::Heading {
            level: one_level_down(level),
            id,
            classes,
            attrs,
        }),
        Event::End(TagEnd::Heading(level)) => Event::End(TagEnd::Heading(one_level_down(level))),
        other => other,
    }
}

/// Render a post body to HTML with every link and image made absolute.
///
/// A feed entry is read somewhere else entirely, so a relative link in it
/// resolves against the reader's own address and lands nowhere. `base` is the
/// blog's root and `page` the post's own URL, which is what a document-
/// relative reference resolves against.
fn markdown_to_html_absolute(md: &str, base: &str, page: &str) -> String {
    let events = extended_events(md)
        .into_iter()
        .map(demote_heading)
        // File references resolve to the web route BEFORE absolutising, so a
        // feed entry's picture points at `<base>/files/x.jpg` rather than at
        // a name resolved against the post's own URL, where nothing is.
        .map(resolve_file_ref)
        .map(|event| absolutize(event, base, page));
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// Rewrite the destination of a link or image event to an absolute URL.
fn absolutize<'a>(event: Event<'a>, base: &str, page: &str) -> Event<'a> {
    match event {
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: absolute_url(&dest_url, base, page).into(),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: absolute_url(&dest_url, base, page).into(),
            title,
            id,
        }),
        other => other,
    }
}

/// Resolve one reference against the blog root and the containing page.
///
/// Only the forms that occur in a post are handled, deliberately rather than
/// implementing RFC 3986: anything already carrying a scheme (`https:`,
/// `mailto:`) or a network path (`//host/x`) is left alone, a root-relative
/// path resolves against the blog root, a fragment against the page it sits
/// in, and anything else against the page's directory.
fn absolute_url(url: &str, base: &str, page: &str) -> String {
    if url.is_empty() || has_scheme(url) || url.starts_with("//") {
        return url.to_string();
    }
    if let Some(fragment) = url.strip_prefix('#') {
        // An in-page anchor would otherwise jump inside the reader's own page.
        return format!("{page}#{fragment}");
    }
    if let Some(path) = url.strip_prefix('/') {
        return format!("{base}/{path}");
    }
    // Document-relative: resolve against the directory the page sits in.
    let dir = page.rsplit_once('/').map(|(d, _)| d).unwrap_or(page);
    format!("{dir}/{url}")
}

/// Whether a reference starts with a URL scheme, e.g. `https:` or `mailto:`.
///
/// A scheme is a letter followed by letters, digits, `+`, `-` or `.`, then a
/// colon. Checking the shape rather than a list of known schemes avoids
/// mangling anything exotic an author writes on purpose.
fn has_scheme(url: &str) -> bool {
    let Some((prefix, _)) = url.split_once(':') else {
        return false;
    };
    let mut chars = prefix.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The next heading level down; `h6` has nowhere to go and stays put.
fn one_level_down(level: pulldown_cmark::HeadingLevel) -> pulldown_cmark::HeadingLevel {
    use pulldown_cmark::HeadingLevel::*;
    match level {
        H1 => H2,
        H2 => H3,
        H3 => H4,
        H4 => H5,
        H5 | H6 => H6,
    }
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

/// The built-in stylesheet, used when the operator configures none. Minimal,
/// theme-neutral and readable.
pub const DEFAULT_STYLE: &str = "\
body{margin:0 auto;max-width:42rem;padding:1rem;font-family:system-ui,sans-serif;\
line-height:1.6;color:#222;background:#fdfdfd}\
h1,h2,h3{line-height:1.25}\
code,pre{font-family:ui-monospace,monospace;background:#eee}\
pre{padding:.75rem;overflow-x:auto}\
a{color:#1a5fb4}\
mark{background:#ffe066;color:#222}\
dt{font-weight:600}\
li:has(>input[type=checkbox]){list-style:none;margin-left:-1rem}\
li>input[type=checkbox]{margin-right:.35rem}\
.footnote-definition{font-size:.9rem;color:#444}\
.footnote-definition p{display:inline}\
.tagline{color:#444}\
.byline,.date{color:#666;font-size:.9rem}\
ul.posts{list-style:none;padding:0}\
ul.posts li{margin:.5rem 0}\
footer{margin-top:3rem;border-top:1px solid #ddd;padding-top:1rem;\
color:#666;font-size:.9rem}\
footer p{margin:.25rem 0}\
footer code{background:none}";

/// Wrap `body` in a complete HTML document.
///
/// `title` is the browser-tab title, which is the post title on a post page
/// and the blog title on the index; `css` is inlined rather than linked so a
/// page is always styled by the stylesheet it was rendered with.
fn html_document(meta: &BlogMeta, css: &str, title: &str, body: &str) -> String {
    let description = match &meta.description {
        Some(d) => format!(
            "<meta name=\"description\" content=\"{}\">\n",
            escape_html(d)
        ),
        None => String::new(),
    };
    // Feed autodiscovery, so a reader finds the feed from any page. Only
    // emitted when there is a feed to find, which mirrors the route.
    let feed = match meta.web_url {
        Some(_) => format!(
            "<link rel=\"alternate\" type=\"application/atom+xml\" \
             title=\"{}\" href=\"{FEED_PATH}\">\n",
            escape_html(&meta.title)
        ),
        None => String::new(),
    };
    format!(
        "<!doctype html>\n<html lang=\"{}\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         {}{}<title>{}</title>\n<style>{}</style>\n</head>\n<body>\n{}{}\n</body>\n</html>\n",
        escape_html(&meta.language),
        description,
        feed,
        escape_html(title),
        css,
        nav_html(meta),
        body
    )
}

/// The nav line as HTML, or nothing at all when there is nowhere to go.
///
/// Rendered inside [`html_document`], so every page that is a document has it
/// and none can be forgotten. It needs no stylesheet of its own: the links
/// inherit the built-in `a` rule, and `class="site-nav"` is there for an
/// operator's stylesheet to take hold of. Adding a rule to
/// [`DEFAULT_STYLE`] instead would change the bytes of every page on every
/// existing blog, which is exactly what this feature must not do.
fn nav_html(meta: &BlogMeta) -> String {
    if meta.nav.is_empty() {
        return String::new();
    }
    let links: Vec<String> = meta
        .nav
        .iter()
        .map(|entry| {
            format!(
                "<a href=\"{}\">{}</a>",
                escape_html(&entry.web),
                escape_html(&entry.label)
            )
        })
        .collect();
    format!(
        "<nav class=\"site-nav\">\n{}\n</nav>\n",
        links.join(" &middot; ")
    )
}

/// The nav line as micron, or nothing at all when there is nowhere to go.
///
/// The mirror of [`nav_html`], but each micron renderer has to place it
/// itself: micron has no document wrapper to hide it in.
fn nav_micron(meta: &BlogMeta) -> String {
    if meta.nav.is_empty() {
        return String::new();
    }
    let links: Vec<String> = meta
        .nav
        .iter()
        .map(|entry| {
            format!(
                "`[{}`{}]",
                sanitize_link_part(&entry.label),
                sanitize_link_part(&entry.micron)
            )
        })
        .collect();
    format!("{}\n\n", links.join(" \u{b7} "))
}

/// The footer shown on every HTML page: where to find the blog on the mesh,
/// and where to find the source of the program serving it.
///
/// A reader on the clearnet side has no way to discover the NomadNet
/// destination otherwise, and it is the more interesting half of this blog.
/// The source line is the AGPL section 13 offer and is unconditional, which
/// is why a blog that configures nothing still has a footer.
fn html_footer(meta: &BlogMeta) -> String {
    let mut lines = Vec::new();
    if let Some(address) = &meta.nomadnet_address {
        lines.push(format!(
            "<p>Also on NomadNet over Reticulum: <code>{}</code></p>",
            escape_html(address)
        ));
    }
    lines.push(format!(
        "<p class=\"source\">{} <a href=\"{}\">Source code</a>.</p>",
        escape_html(&meta.source.sentence()),
        escape_html(&meta.source.url)
    ));
    format!("\n<footer>\n{}\n</footer>", lines.join("\n"))
}

/// Render the post index as a complete HTML document: who this is, what it
/// is about, and the posts. Posts link to `/posts/<slug>`.
pub fn render_index_html(meta: &BlogMeta, css: &str, posts: &[Post]) -> String {
    let mut body = format!("<h1>{}</h1>\n", escape_html(&meta.title));
    if let Some(author) = &meta.author {
        body.push_str(&format!(
            "<p class=\"byline\">by {}</p>\n",
            meta.author_html(author)
        ));
    }
    if let Some(description) = &meta.description {
        body.push_str(&format!(
            "<p class=\"tagline\">{}</p>\n",
            escape_html(description)
        ));
    }

    body.push_str("<ul class=\"posts\">\n");
    for post in posts {
        // Only name an author who differs from the blog's: repeating the same
        // name on every line is noise, a guest post is information.
        let byline = match &post.author {
            Some(author) if Some(author.as_str()) != meta.author.as_deref() => {
                format!(
                    " <span class=\"byline\">by {}</span>",
                    meta.author_html(author)
                )
            }
            _ => String::new(),
        };
        body.push_str(&format!(
            "<li><span class=\"date\">{}</span> <a href=\"/posts/{}\">{}</a>{}</li>\n",
            post.date,
            escape_html(&post.slug),
            escape_html(&post.title),
            byline
        ));
    }
    body.push_str("</ul>");
    body.push_str(&html_footer(meta));
    html_document(meta, css, &meta.title, &body)
}

/// Render one post as a complete HTML document, with a way back to the index.
pub fn render_post_html(meta: &BlogMeta, css: &str, post: &Post) -> String {
    let byline = match meta.author_of(post) {
        Some(author) => format!(" &middot; {}", meta.author_html(author)),
        None => String::new(),
    };
    // Back to the list this post was found in, which moves with it once a
    // landing page takes the root.
    let body = format!(
        "<article>\n<h1>{}</h1>\n<p class=\"date\">{}{}</p>\n{}</article>\n\
         <p><a href=\"{}\">&larr; {}</a></p>{}",
        escape_html(&post.title),
        post.date,
        byline,
        markdown_to_html(&post.body_md),
        meta.index_html_path(),
        escape_html(&meta.title),
        html_footer(meta)
    );
    html_document(meta, css, &post.title, &body)
}

/// Render the about page as a complete HTML document.
///
/// `text` is the optional Markdown file, parsed exactly like a post. Its
/// date, slug and author are ignored: an about page is not a dated entry, so
/// showing a publication date and a byline on it would be misleading.
pub fn render_about_html(meta: &BlogMeta, css: &str, text: Option<&Post>) -> String {
    let heading = about_heading(meta, text);
    let mut body = format!("<h1>{}</h1>\n", escape_html(&heading));

    if let Some(email) = &meta.email {
        body.push_str(&format!(
            "<p class=\"contact\">Email: <a href=\"mailto:{0}\">{0}</a></p>\n",
            escape_html(email)
        ));
    }
    if let Some(lxmf) = &meta.lxmf {
        // No link: a browser has nothing to do with an LXMF address. The hash
        // is what a reader copies into their own client.
        body.push_str(&format!(
            "<p class=\"contact\">LXMF: <code>{}</code></p>\n",
            escape_html(lxmf)
        ));
    }
    if let Some(text) = text {
        body.push_str(&markdown_to_html(&text.body_md));
    }

    body.push_str(&format!(
        "<p><a href=\"/\">&larr; {}</a></p>",
        escape_html(&meta.title)
    ));
    body.push_str(&html_footer(meta));
    html_document(meta, css, &heading, &body)
}

/// Render the about page as a micron page.
///
/// The LXMF address becomes a `lxmf@<hash>` link, which NomadNet opens as a
/// conversation with that address.
pub fn render_about_micron(meta: &BlogMeta, text: Option<&Post>) -> String {
    let heading = about_heading(meta, text);
    let mut out = nav_micron(meta);
    out.push_str(&format!(">{}\n\n", escape_micron_text(&heading)));

    if let Some(email) = &meta.email {
        out.push_str(&format!("Email: {}\n", escape_micron_text(email)));
    }
    if let Some(lxmf) = &meta.lxmf {
        out.push_str(&format!(
            "LXMF: `[{0}`lxmf@{0}]\n",
            sanitize_link_part(lxmf)
        ));
    }
    if meta.email.is_some() || meta.lxmf.is_some() {
        out.push_str("\n-\n\n");
    }
    if let Some(text) = text {
        out.push_str(&markdown_to_micron(&text.body_md));
        out.push('\n');
    }

    out.push_str(&format!(
        "\n`[\u{2190} {}`{ABOUT_BACK_PATH}]\n",
        sanitize_link_part(&meta.title)
    ));
    out.push_str(&micron_footer(meta));
    out
}

/// The micron request path of the index, used by the about page's back link.
const ABOUT_BACK_PATH: &str = ":/page/index.mu";

/// The about page's heading.
///
/// The text file's title when there is one, which the loader already defaults
/// to [`default_about_title`], so a file without frontmatter still lands on
/// the author's name rather than on its own file name.
fn about_heading(meta: &BlogMeta, text: Option<&Post>) -> String {
    match text {
        Some(text) => text.title.clone(),
        None => default_about_title(meta.author.as_deref()),
    }
}

/// The title an about page carries when nothing names one: the author, or a
/// plain "About".
///
/// A name that slugifies to nothing is skipped, because the post parser
/// requires a usable slug and would otherwise reject the file over a title it
/// never asked for.
pub fn default_about_title(author: Option<&str>) -> String {
    author
        .filter(|a| !slugify(a).is_empty())
        .unwrap_or("About")
        .to_string()
}

/// Render a static page as a complete HTML document.
///
/// A page is a post in file format and nothing else: no date, no byline, no
/// place in the index or the feed. The landing page is rendered by exactly
/// this function, because a landing page is a static page that happens to be
/// called `index`.
pub fn render_page_html(meta: &BlogMeta, css: &str, page: &Post) -> String {
    let mut body = format!("<h1>{}</h1>\n", escape_html(&page.title));
    body.push_str(&markdown_to_html(&page.body_md));
    body.push_str(&html_footer(meta));
    html_document(meta, css, &page.title, &body)
}

/// Render a static page as a micron page.
pub fn render_page_micron(meta: &BlogMeta, page: &Post) -> String {
    let mut out = nav_micron(meta);
    out.push_str(&format!(">{}\n\n", escape_micron_text(&page.title)));
    out.push_str(&markdown_to_micron(&page.body_md));
    out.push('\n');
    out.push_str(&micron_footer(meta));
    out
}

/// Render a link's mesh page: its own text, when `pages_dir` holds a page of
/// the same name, and then the URL as plain text.
///
/// As text rather than as a micron link, because there is nothing a NomadNet
/// client could do with it: micron's link targets are Reticulum paths, and a
/// browser is not a thing the mesh side can reach for. What the reader gets
/// is the address to type somewhere else, which is the honest answer. The web
/// side never renders this — it answers the same name with a redirect.
pub fn render_link_micron(meta: &BlogMeta, label: &str, url: &str, page: Option<&Post>) -> String {
    let mut out = nav_micron(meta);
    out.push_str(&format!(">{}\n\n", escape_micron_text(label)));
    if let Some(page) = page {
        out.push_str(&markdown_to_micron(&page.body_md));
        out.push_str("\n\n-\n\n");
    }
    out.push_str("On the web:\n\n");
    out.push_str(&escape_micron_text(url));
    out.push('\n');
    out.push_str(&micron_footer(meta));
    out
}

/// The path the Atom feed is served under.
pub const FEED_PATH: &str = "/feed.xml";

/// Render the Atom feed, or `None` when the blog has no public URL.
///
/// A feed is only meaningful with absolute links, and those need a domain.
/// A plaintext development run has none, so it serves no feed rather than a
/// feed full of links that resolve against whatever the reader happens to be
/// looking at.
///
/// Atom rather than RSS 2.0: entry identity is explicit rather than
/// conventional, and timestamps are RFC 3339 rather than RFC 822. Every
/// reader handles both.
pub fn render_feed_atom(meta: &BlogMeta, posts: &[Post]) -> Option<String> {
    let base = meta.web_url.as_deref()?;

    let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    out.push_str("<feed xmlns=\"http://www.w3.org/2005/Atom\">\n");
    out.push_str(&format!("<title>{}</title>\n", escape_html(&meta.title)));
    if let Some(description) = &meta.description {
        out.push_str(&format!(
            "<subtitle>{}</subtitle>\n",
            escape_html(description)
        ));
    }
    out.push_str(&format!("<id>{}/</id>\n", escape_html(base)));
    out.push_str(&format!(
        "<link rel=\"alternate\" type=\"text/html\" href=\"{}/\"/>\n",
        escape_html(base)
    ));
    out.push_str(&format!(
        "<link rel=\"self\" type=\"application/atom+xml\" href=\"{}{}\"/>\n",
        escape_html(base),
        FEED_PATH
    ));
    // Posts are newest first, so the first one dates the feed. With no posts
    // there is no date to give and the epoch stands in; `updated` is
    // mandatory, and an empty feed is not worth a special case.
    let updated = posts
        .first()
        .map(|p| rfc3339(&p.date))
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
    out.push_str(&format!("<updated>{updated}</updated>\n"));
    if let Some(author) = &meta.author {
        out.push_str(&format!(
            "<author><name>{}</name></author>\n",
            escape_html(author)
        ));
    }

    for post in posts {
        out.push_str(&feed_entry(meta, base, post));
    }
    out.push_str("</feed>\n");
    Some(out)
}

/// One `<entry>`, carrying the post's full text.
///
/// Full text rather than a teaser: this is a text blog, and a feed a reader
/// can actually read in their reader is the point of having one.
fn feed_entry(meta: &BlogMeta, base: &str, post: &Post) -> String {
    let url = format!("{base}/posts/{}", post.slug);
    let mut entry = String::from("<entry>\n");
    entry.push_str(&format!("<title>{}</title>\n", escape_html(&post.title)));
    // The entry id has to stay stable, or readers show the post again as new.
    // It is the post URL, which means it moves when an untitled-slug post is
    // retitled; see the README note on pinning `slug` once published.
    entry.push_str(&format!("<id>{}</id>\n", escape_html(&url)));
    entry.push_str(&format!(
        "<link rel=\"alternate\" type=\"text/html\" href=\"{}\"/>\n",
        escape_html(&url)
    ));
    let stamp = rfc3339(&post.date);
    entry.push_str(&format!("<published>{stamp}</published>\n"));
    entry.push_str(&format!("<updated>{stamp}</updated>\n"));
    // Atom lets entries inherit the feed's author, so only a differing one
    // needs saying. With no feed author, the post's own is all there is.
    if let Some(author) = meta.author_of(post) {
        if Some(author) != meta.author.as_deref() {
            entry.push_str(&format!(
                "<author><name>{}</name></author>\n",
                escape_html(author)
            ));
        }
    }
    entry.push_str(&format!(
        "<content type=\"html\">{}</content>\n",
        escape_html(&markdown_to_html_absolute(&post.body_md, base, &url))
    ));
    entry.push_str("</entry>\n");
    entry
}

/// A post's date as an RFC 3339 timestamp.
///
/// Posts are dated to the day, so the time is always midnight UTC. Two posts
/// on one day therefore carry identical timestamps and a reader may order
/// them either way; our own index breaks that tie by title, a reader cannot.
fn rfc3339(date: &Date) -> String {
    format!("{date}T00:00:00Z")
}

/// Convert a Markdown fragment to valid micron markup. See the module docs
/// for the mapping and degradation table. Never panics.
pub fn markdown_to_micron(md: &str) -> String {
    let mut writer = MicronWriter::default();
    for event in extended_events(md) {
        writer.event(event);
    }
    writer.finish()
}

/// Render the post index as a micron page: the blog's identity, then one link
/// per post targeting the local page `:/page/<slug>.mu` (NomadNet's same-node
/// link form, as resolved by lnomad and NomadNet).
pub fn render_index_micron(meta: &BlogMeta, posts: &[Post]) -> String {
    let mut out = nav_micron(meta);
    out.push_str(&format!(">{}\n\n", escape_micron_text(&meta.title)));
    if let Some(author) = &meta.author {
        out.push_str(&format!("by {}\n", meta.author_micron(author)));
    }
    if let Some(description) = &meta.description {
        out.push_str(&format!("{}\n", escape_micron_text(description)));
    }
    if meta.author.is_some() || meta.description.is_some() {
        out.push('\n');
    }

    for post in posts {
        // Same rule as HTML: name an author only where it differs.
        let byline = match &post.author {
            Some(author) if Some(author.as_str()) != meta.author.as_deref() => {
                format!(" by {author}")
            }
            _ => String::new(),
        };
        out.push_str(&format!(
            "`[{}`:/page/{}.mu]\n",
            sanitize_link_part(&format!("{} {}{}", post.date, post.title, byline)),
            sanitize_link_part(&post.slug)
        ));
    }
    out.push_str(&micron_footer(meta));
    out
}

/// Render one post as a micron page: title heading, date and author line,
/// divider, body, and a link back to the index.
pub fn render_post_micron(meta: &BlogMeta, post: &Post) -> String {
    let byline = match meta.author_of(post) {
        Some(author) => format!(" \u{b7} {}", meta.author_micron(author)),
        None => String::new(),
    };
    format!(
        "{}>{}\n\n{}{}\n-\n\n{}\n\n`[\u{2190} {}`{}]\n{}",
        nav_micron(meta),
        escape_micron_text(&post.title),
        post.date,
        byline,
        markdown_to_micron(&post.body_md),
        sanitize_link_part(&meta.title),
        meta.index_micron_target(),
        micron_footer(meta)
    )
}

/// The footer shown on every micron page: where to find the blog on the web,
/// and where to find the source of the node serving it.
///
/// The mirror of [`html_footer`]; a mesh reader who wants to share the blog
/// with someone off-mesh needs the clearnet URL. The source offer is here for
/// the same reason it is there — a NomadNet reader interacts with the same
/// program over the same kind of network, and section 13 does not distinguish
/// between the two.
///
/// The URL is plain text rather than a `` `[label`target] `` link, as in
/// [`render_link_micron`]: micron link targets are Reticulum paths, and a
/// NomadNet client has nothing to do with a web address. What the reader gets
/// is the address to type somewhere else.
fn micron_footer(meta: &BlogMeta) -> String {
    let mut lines = Vec::new();
    if let Some(url) = &meta.web_url {
        lines.push(format!("Also on the web: {}", escape_micron_text(url)));
    }
    lines.push(format!(
        "{} Source: {}",
        escape_micron_text(&meta.source.sentence()),
        escape_micron_text(&meta.source.url)
    ));
    format!("\n-\n\n{}\n", lines.join("\n"))
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

/// The table currently being emitted.
#[derive(Clone, Debug, Default)]
struct OpenTable {
    /// The column alignments, for the separator row micron wants as the
    /// table's second line.
    alignments: Vec<Alignment>,
    /// Cells emitted so far in the current row.
    cells: usize,
}

/// The image currently being collected: its alt text, and where it points.
#[derive(Clone, Debug, Default)]
struct OpenImage {
    /// Alt text buffered until the closing event.
    alt: String,
    /// The destination as the author wrote it.
    dest: String,
}

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
    /// Alt text buffered while an image is open, with the image's target.
    image: Option<OpenImage>,
    /// The table currently being emitted, if any.
    table: Option<OpenTable>,
    /// The cell currently being collected. Holds finished output fragments,
    /// not source text, so a link or a style toggle inside a cell survives;
    /// only plain text picks up the extra `|` escaping on the way in.
    cell: Option<String>,
    /// Finished footnote-definition lines, held back until the end of the
    /// page: micron has no anchor to put them next to their reference.
    footnotes: Vec<String>,
    /// Whether finished lines currently go to `footnotes` instead of `out`.
    in_footnote: bool,
    /// Sub/superscript text buffered until its closing event, because the
    /// Unicode substitution is decided over the whole run.
    script: Option<(Script, String)>,
    /// Indent levels beyond the blockquote depth, two spaces each. Used by
    /// definition lists, whose terms carry their definitions indented.
    indent: usize,
}

/// Which way a buffered script run shifts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Script {
    /// `~2~`, rendered with Unicode subscripts where they exist.
    Sub,
    /// `^2^`, rendered with Unicode superscripts where they exist.
    Sup,
}

/// One character in `kind` position, or `None` when Unicode has none.
///
/// Digits and the arithmetic signs are complete in both positions; the
/// alphabet is not, which is why a run is either fully substituted or left
/// with its markers.
fn script_char(kind: Script, c: char) -> Option<char> {
    let digits = match kind {
        Script::Sub => {
            "\u{2080}\u{2081}\u{2082}\u{2083}\u{2084}\u{2085}\u{2086}\u{2087}\u{2088}\u{2089}"
        }
        Script::Sup => "\u{2070}\u{b9}\u{b2}\u{b3}\u{2074}\u{2075}\u{2076}\u{2077}\u{2078}\u{2079}",
    };
    if let Some(d) = c.to_digit(10) {
        return digits.chars().nth(d as usize);
    }
    let signs = match kind {
        Script::Sub => "\u{208a}\u{208b}\u{208c}\u{208d}\u{208e}",
        Script::Sup => "\u{207a}\u{207b}\u{207c}\u{207d}\u{207e}",
    };
    "+-=()".find(c).and_then(|i| signs.chars().nth(i))
}

/// A whole script run: the Unicode form when every character has one, else
/// the source text with its markers, which still reads as what it means.
fn script_text(kind: Script, text: &str) -> String {
    let unicode: Option<String> = text.chars().map(|c| script_char(kind, c)).collect();
    match unicode {
        Some(s) => s,
        None => match kind {
            Script::Sub => format!("~{text}~"),
            Script::Sup => format!("^{text}^"),
        },
    }
}

impl MicronWriter {
    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => self.inline_code(&t),
            Event::Html(t) => self.text(&t),
            Event::InlineHtml(t) => self.inline_html(&t),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.block_sep();
                self.push_raw("-");
                self.flush_line();
            }
            Event::FootnoteReference(label) => self.footnote_reference(&label),
            Event::TaskListMarker(done) => self.task_marker(done),
            // Math is not part of the standard feature set and its extension
            // stays off; listed for totality, never emitted.
            Event::InlineMath(_) | Event::DisplayMath(_) => {}
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
                // Demoted for the same reason as on the HTML side: the page
                // template already used `>` for the post title, so a body
                // heading starts one level in. Micron has only three levels,
                // so this also means `>` is reserved for the title alone.
                let depth = (level as usize + 1).min(MAX_MICRON_HEADING_DEPTH);
                self.push_raw(&">".repeat(depth));
            }
            Tag::BlockQuote(_) => {
                self.block_sep();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(CodeBlockKind::Fenced(_) | CodeBlockKind::Indented) => {
                self.block_sep();
                self.sink().push("`=".to_string());
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
            Tag::Table(alignments) => {
                self.block_sep();
                self.sink().push("`t".to_string());
                self.table = Some(OpenTable {
                    alignments,
                    cells: 0,
                });
            }
            Tag::TableHead | Tag::TableRow => {
                self.flush_line();
                if let Some(table) = self.table.as_mut() {
                    table.cells = 0;
                }
            }
            Tag::TableCell => {
                let first = self.table.as_ref().is_none_or(|t| t.cells == 0);
                if !first {
                    self.push_raw(" | ");
                }
                if let Some(table) = self.table.as_mut() {
                    table.cells += 1;
                }
                // Collect the cell rather than writing straight to the line:
                // a literal `|` in its text has to be escaped, and that can
                // only be decided per cell.
                self.cell = Some(String::new());
            }
            Tag::Emphasis => self.style_toggle("`*"),
            Tag::Strong => self.style_toggle("`!"),
            Tag::Strikethrough => self.style_toggle(&format!("`F{STRIKE_FG}")),
            Tag::Superscript => self.start_script(Script::Sup),
            Tag::Subscript => self.start_script(Script::Sub),
            Tag::FootnoteDefinition(label) => self.start_footnote(&label),
            Tag::DefinitionList => self.block_sep(),
            Tag::DefinitionListTitle => {
                self.flush_line();
                self.style_toggle("`!");
            }
            Tag::DefinitionListDefinition => {
                self.flush_line();
                self.indent += 1;
            }
            Tag::Link { dest_url, .. } => {
                self.link_url = Some(dest_url.to_string());
                self.link_label.clear();
            }
            Tag::Image { dest_url, .. } => {
                self.image = Some(OpenImage {
                    alt: String::new(),
                    dest: dest_url.to_string(),
                })
            }
            Tag::HtmlBlock => self.block_sep(),
            // Metadata blocks: the extension stays off, so the tag is never
            // constructed and the front matter never reaches a renderer.
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
                self.sink().push("`=".to_string());
            }
            TagEnd::List(_) => {
                self.flush_line();
                self.list_stack.pop();
            }
            TagEnd::TableCell => {
                let cell = self.cell.take().unwrap_or_default();
                self.push_raw(&cell);
            }
            TagEnd::TableHead => {
                self.flush_line();
                // Micron takes the table's second line as the alignment row,
                // exactly as Markdown does (RNS rngit util.py:534-535), so it
                // is emitted rather than inferred.
                let alignments = self
                    .table
                    .as_ref()
                    .map(|t| t.alignments.clone())
                    .unwrap_or_default();
                let cols = alignments.len().max(1);
                let row: Vec<&str> = (0..cols)
                    .map(|i| match alignments.get(i) {
                        Some(Alignment::Center) => ":---:",
                        Some(Alignment::Right) => "---:",
                        _ => "---",
                    })
                    .collect();
                self.sink().push(row.join(" | "));
            }
            TagEnd::TableRow => self.flush_line(),
            TagEnd::Table => {
                self.flush_line();
                self.sink().push("`t".to_string());
                self.table = None;
            }
            TagEnd::Emphasis => self.style_toggle("`*"),
            TagEnd::Strong => self.style_toggle("`!"),
            TagEnd::Strikethrough => self.style_toggle("`f"),
            TagEnd::Superscript | TagEnd::Subscript => self.end_script(),
            TagEnd::FootnoteDefinition => {
                self.flush_line();
                self.in_footnote = false;
            }
            TagEnd::DefinitionListTitle => {
                self.style_toggle("`!");
                self.flush_line();
            }
            TagEnd::DefinitionListDefinition => {
                self.flush_line();
                self.indent = self.indent.saturating_sub(1);
            }
            TagEnd::DefinitionList => self.flush_line(),
            TagEnd::Link => {
                let url = sanitize_link_part(&self.link_url.take().unwrap_or_default());
                let label = sanitize_link_part(self.link_label.trim());
                let label = if label.is_empty() { url.clone() } else { label };
                self.push_raw(&format!("`[{label}`{url}]"));
            }
            TagEnd::Image => {
                let image = self.image.take().unwrap_or_default();
                let alt = image.alt.trim();
                // A picture in the file area becomes an ordinary micron link
                // to it. NomadNet shows a link that saves the file to the
                // reader's download directory; lnomad draws it in the page.
                // Micron has no image construct, so this is the whole of what
                // is available, and inventing one would render as raw markup
                // in every other browser.
                if let Some(name) = files::file_ref(&image.dest) {
                    let label = match alt.is_empty() {
                        true => name.clone(),
                        false => alt.to_string(),
                    };
                    let label = sanitize_link_part(&label);
                    let target = sanitize_link_part(&files::micron_target(&name));
                    self.push_raw(&format!("`[{label}`{target}]"));
                } else if alt.is_empty() {
                    self.push_text("[image]");
                } else {
                    self.push_text(&format!("[image: {alt}]"));
                }
            }
            _ => {}
        }
    }

    /// Route text to whatever is currently collecting it: image alt, link
    /// label, literal block, or the current line (escaped). Embedded
    /// newlines (raw HTML, code text) split lines.
    fn text(&mut self, t: &str) {
        if let Some(image) = self.image.as_mut() {
            image.alt.push_str(t);
            return;
        }
        if self.link_url.is_some() {
            self.link_label.push_str(t);
            return;
        }
        if let Some((_, buffered)) = self.script.as_mut() {
            buffered.push_str(t);
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
        if let Some(image) = self.image.as_mut() {
            image.alt.push_str(code);
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

    /// The inline HTML tags our own expansion emits, plus the same tags
    /// written by hand: micron has a mapping for each, which is a better
    /// reading than the escaped tag text every other raw HTML degrades to.
    fn inline_html(&mut self, t: &str) {
        match t {
            "<mark>" => self.style_toggle(&format!("`B{HIGHLIGHT_BG}")),
            "</mark>" => self.style_toggle("`b"),
            "<del>" | "<s>" => self.style_toggle(&format!("`F{STRIKE_FG}")),
            "</del>" | "</s>" => self.style_toggle("`f"),
            "<sub>" => self.start_script(Script::Sub),
            "<sup>" => self.start_script(Script::Sup),
            "</sub>" | "</sup>" => self.end_script(),
            other => self.text(other),
        }
    }

    /// Start collecting a sub/superscript run, unless something else is
    /// already collecting text.
    fn start_script(&mut self, kind: Script) {
        if self.image.is_none() && self.link_url.is_none() && self.script.is_none() {
            self.script = Some((kind, String::new()));
        }
    }

    /// Emit a collected sub/superscript run. Tolerates never having been
    /// started, so a stray closing tag cannot lose the text after it.
    fn end_script(&mut self) {
        if let Some((kind, text)) = self.script.take() {
            let rendered = script_text(kind, &text);
            self.push_text(&rendered);
        }
    }

    /// A footnote reference: the bare marker, since micron has nowhere to
    /// jump to. The definition it names is emitted at the end of the page.
    fn footnote_reference(&mut self, label: &str) {
        let marker = format!("[{label}]");
        if let Some(image) = self.image.as_mut() {
            image.alt.push_str(&marker);
        } else if self.link_url.is_some() {
            self.link_label.push_str(&marker);
        } else if let Some((_, buffered)) = self.script.as_mut() {
            buffered.push_str(&marker);
        } else {
            self.push_text(&marker);
        }
    }

    /// Divert output into the footnote block and open the definition with the
    /// marker its references carry.
    fn start_footnote(&mut self, label: &str) {
        self.flush_line();
        self.in_footnote = true;
        if !self.footnotes.is_empty() {
            self.footnotes.push(String::new());
        }
        // Written straight into the line, the way a list item's marker is, so
        // the definition's first paragraph continues it instead of starting a
        // block of its own.
        self.line = format!("[{}] ", escape_micron_text(label));
        self.line_is_text = false;
    }

    /// A task-list item's checkbox, which replaces the bullet rather than
    /// joining it: the box is the item's marker.
    fn task_marker(&mut self, done: bool) {
        const BULLET: &str = "\u{2022} ";
        if self.cell.is_none() && self.line.ends_with(BULLET) {
            let keep = self.line.len() - BULLET.len();
            self.line.truncate(keep);
        }
        match done {
            true => self.push_raw("\u{2611} "),
            false => self.push_raw("\u{2610} "),
        }
    }

    /// Emit a style toggle unless a link/image is collecting text (labels run
    /// raw to the closing bracket, so styles inside them are dropped).
    fn style_toggle(&mut self, toggle: &str) {
        if self.image.is_none() && self.link_url.is_none() {
            self.push_raw(toggle);
        }
    }

    /// Append micron markup we emit deliberately (never escaped).
    fn push_raw(&mut self, s: &str) {
        if let Some(cell) = self.cell.as_mut() {
            cell.push_str(s);
            return;
        }
        if self.line.is_empty() {
            self.line_is_text = false;
        }
        self.line.push_str(s);
    }

    /// Append plain text, escaped so micron reads it verbatim.
    fn push_text(&mut self, s: &str) {
        let escaped = escape_micron_text(s);
        if let Some(cell) = self.cell.as_mut() {
            // Inside a table cell an unescaped `|` would start the next
            // column. The reference honours `\|` (rngit util.py:641-647).
            cell.push_str(&escaped.replace('|', "\\|"));
            return;
        }
        if self.line.is_empty() {
            self.line_is_text = true;
        }
        self.line.push_str(&escaped);
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
        let indent = self.quote_depth + self.indent;
        if indent > 0 {
            line = format!("{}{line}", "  ".repeat(indent));
        }
        self.sink().push(line);
    }

    /// Finish one verbatim literal-block line. Only the block toggle itself
    /// needs care: a content line reading `` `= `` must be emitted as
    /// `` \`= `` (the parser unescapes it inside literal blocks).
    fn flush_code_line(&mut self) {
        let line = std::mem::take(&mut self.line);
        match line == "`=" {
            true => self.sink().push("\\`=".to_string()),
            false => self.sink().push(line),
        }
    }

    /// Separate blocks with one blank line (never at the start of output).
    fn block_sep(&mut self) {
        self.flush_line();
        if self.sink().last().is_some_and(|l| !l.is_empty()) {
            self.sink().push(String::new());
        }
    }

    /// Where finished lines go: the footnote block while a definition is
    /// open, the page itself otherwise.
    fn sink(&mut self) -> &mut Vec<String> {
        match self.in_footnote {
            true => &mut self.footnotes,
            false => &mut self.out,
        }
    }

    /// Flush pending state and return the final micron source.
    fn finish(mut self) -> String {
        // A script run left open by unbalanced raw HTML must not swallow the
        // text it collected.
        self.end_script();
        self.flush_line();
        self.in_footnote = false;
        if !self.footnotes.is_empty() {
            if self.out.last().is_some_and(|l| !l.is_empty()) {
                self.out.push(String::new());
            }
            self.out.push("-".to_string());
            self.out.push(String::new());
            self.out.append(&mut self.footnotes);
        }
        let mut out = self.out.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }
}
