//! The interactive browser: the REPL command grammar, the navigation state
//! machine, and the fetch/parse/render/print loop that drives them.
//!
//! Navigation semantics mirror NomadNet's `Browser.py` (`retrieve_url`,
//! `handle_link`, the `history` stack, and `DEFAULT_PATH`): a link target is
//! resolved against the destination of the page currently in view, its preset
//! query fields are carried as `var_*` request variables, and a back stack
//! remembers where the reader came from. Wire-level URL parsing is delegated to
//! [`crate::url::parse_url`], the single source of truth for URL grammar.

use std::io::{BufRead, Write};
use std::time::Duration;

use leviculum_micron::MicronDocument;

use crate::color::ColorDepth;
use crate::fetch::{FetchError, Session};
use crate::render::{render_with_options, RenderedLink, RenderedPage};
use crate::url::{parse_url, Target, UrlError};

/// A parsed REPL command.
///
/// The grammar is a single token (or a number) optionally followed by an
/// argument: a bare number follows a link, single letters drive navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Follow the link with this 1-based index (`N`).
    Follow(usize),
    /// Go back to the previous page (`b`).
    Back,
    /// Reload the current page (`r`).
    Reload,
    /// Navigate to a new URL (`u <url>`).
    Go(String),
    /// List the NomadNet nodes discovered from announces (`d` / `nodes`).
    Nodes,
    /// Open discovered node number `N` (`o N` / `open N`).
    OpenNode(usize),
    /// Print the help text (`h`).
    Help,
    /// Quit the browser (`q` / EOF).
    Quit,
    /// An empty line: redisplay the prompt.
    Empty,
    /// An unrecognised command; the raw input is carried back for the message.
    Unknown(String),
}

/// Parse a line of REPL input into a [`Command`].
///
/// A line that is exactly a non-negative integer follows that link index.
/// Otherwise the first whitespace-delimited token selects the command and the
/// remainder is its argument. Leading/trailing whitespace is ignored.
pub fn parse_command(input: &str) -> Command {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Command::Empty;
    }
    if let Ok(n) = trimmed.parse::<usize>() {
        return Command::Follow(n);
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim();
    match head {
        "b" | "back" => Command::Back,
        "r" | "reload" => Command::Reload,
        "u" | "url" | "go" => {
            if rest.is_empty() {
                Command::Unknown(trimmed.to_string())
            } else {
                Command::Go(rest.to_string())
            }
        }
        "d" | "nodes" => Command::Nodes,
        "o" | "open" => match rest.parse::<usize>() {
            Ok(n) => Command::OpenNode(n),
            Err(_) => Command::Unknown(trimmed.to_string()),
        },
        "h" | "help" | "?" => Command::Help,
        "q" | "quit" | "exit" => Command::Quit,
        _ => Command::Unknown(trimmed.to_string()),
    }
}

/// The navigation state machine: the page currently in view plus a back stack.
///
/// [`visit`](Nav::visit) pushes the current target onto the history before
/// moving on; [`back`](Nav::back) pops it. The current destination is exposed so
/// same-destination (`:/page/x.mu`) links resolve against it, matching
/// `Browser.retrieve_url`.
#[derive(Debug, Default)]
pub struct Nav {
    history: Vec<Target>,
    current: Option<Target>,
}

impl Nav {
    /// A fresh navigator with no current page and an empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// The target currently in view, if any.
    pub fn current(&self) -> Option<&Target> {
        self.current.as_ref()
    }

    /// The destination hash of the current page, used to resolve relative link
    /// targets (a leading `:`).
    pub fn current_dest(&self) -> Option<[u8; 16]> {
        self.current.as_ref().map(|t| t.dest_hash)
    }

    /// The number of entries on the back stack.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Move to `target`, pushing the current page (if any) onto the back stack.
    pub fn visit(&mut self, target: Target) {
        if let Some(cur) = self.current.take() {
            self.history.push(cur);
        }
        self.current = Some(target);
    }

    /// Pop the back stack into the current page, returning the new current
    /// target, or `None` when the history is empty (nothing to go back to).
    pub fn back(&mut self) -> Option<&Target> {
        let prev = self.history.pop()?;
        self.current = Some(prev);
        self.current.as_ref()
    }
}

/// Split a `#anchor` suffix off a link target, returning the base target and the
/// anchor name (if any, and non-empty).
fn split_anchor(target: &str) -> (&str, Option<String>) {
    match target.split_once('#') {
        Some((base, anchor)) if !anchor.is_empty() => (base, Some(anchor.to_string())),
        _ => (target, None),
    }
}

/// The preset (`key=value`) field components of a link, reconstructed into the
/// backtick blob [`parse_url`] understands. Valueless components are form-field
/// placeholders (interactive input, a v1 stub) and are dropped here.
fn preset_field_blob(link: &RenderedLink) -> String {
    link.fields
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("|")
}

/// Resolve a followed link into a fetch [`Target`] and any `#anchor`.
///
/// The link target is resolved against `current_dest` (for `:/page/x.mu`
/// same-destination links) and its preset fields are carried through
/// [`parse_url`], keeping URL grammar in one place.
pub fn resolve_link(
    link: &RenderedLink,
    current_dest: Option<[u8; 16]>,
) -> Result<(Target, Option<String>), UrlError> {
    let (base, anchor) = split_anchor(&link.target);
    let blob = preset_field_blob(link);
    let url = if blob.is_empty() {
        base.to_string()
    } else {
        format!("{base}`{blob}")
    };
    let target = parse_url(&url, current_dest)?;
    Ok((target, anchor))
}

/// Options controlling how pages are fetched and rendered.
#[derive(Debug, Clone, Copy)]
pub struct BrowserOptions {
    /// Render width in columns.
    pub width: usize,
    /// Strip ANSI colour from the rendered output.
    pub no_color: bool,
    /// The terminal colour depth: 24-bit true colour, or the downgraded
    /// xterm-256 palette for terminals without true-colour support.
    pub depth: ColorDepth,
    /// Per-request fetch timeout.
    pub timeout: Duration,
}

/// Fetch a page, parse it, and return the parsed document. The raw bytes are
/// decoded as UTF-8 lossily so a page with stray bytes still renders.
///
/// Public so the TUI shell (`main`) can fetch a page once and lay it out into a
/// [`crate::tui::Model`], sharing the exact fetch/parse path the print sink uses.
pub async fn fetch_document(
    session: &mut Session,
    target: &Target,
    timeout: Duration,
) -> Result<MicronDocument, FetchError> {
    let bytes = session.fetch(target, timeout).await?;
    let source = String::from_utf8_lossy(&bytes);
    Ok(leviculum_micron::parse(&source))
}

/// A short, glanceable page title for the TUI frame: the node's discovered
/// display name when known, else the short dest hex. The page path is not part
/// of the title; it appears once, in the address shown beside it.
pub fn page_title(name: Option<&str>, dest_hash: &[u8; 16], _path: &str) -> String {
    match name {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => short_dest_hex(dest_hash),
    }
}

/// The faint (dim) SGR introducer and the reset, used for the orientation
/// chrome (status bar, dimmed link targets, prompt hint). Reticulum's own
/// renderer has no dim helper, so the raw sequences live here, always gated on
/// [`BrowserOptions::no_color`].
const FAINT: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// A short, glanceable form of a destination hash: the first 8 hex characters
/// (4 bytes) followed by an ellipsis, e.g. `a8d24177…`.
fn short_dest_hex(dest_hash: &[u8; 16]) -> String {
    let mut s = String::with_capacity(9);
    for byte in &dest_hash[..4] {
        s.push_str(&format!("{byte:02x}"));
    }
    s.push('…');
    s
}

/// Write the one-line orientation "address bar" printed at the top of every
/// rendered page: the current node identity (its discovered display name when
/// known, else the short dest hex) and the page path, e.g.
/// `  Node Name · :/page/index.mu`. The line is dimmed when colour is on and
/// truncated to `opts.width`.
fn write_status_bar<W: Write>(
    out: &mut W,
    name: Option<&str>,
    dest_hash: &[u8; 16],
    path: &str,
    opts: &BrowserOptions,
) -> std::io::Result<()> {
    let label = match name {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => short_dest_hex(dest_hash),
    };
    let mut content = format!("  {label} · :{path}");
    if content.chars().count() > opts.width {
        content = content.chars().take(opts.width).collect();
    }
    if opts.no_color {
        writeln!(out, "{content}")
    } else {
        writeln!(out, "{FAINT}{content}{RESET}")
    }
}

/// Write a rendered page's laid-out text to `out`.
///
/// Links carry no visible `[N]` marker and there is no trailing `Links:` legend:
/// a link is set apart in the page text by its underline + colour alone (the
/// interactive TUI adds focus, hints and mouse hit-testing on top of that).
fn write_page<W: Write>(out: &mut W, page: &RenderedPage) -> std::io::Result<()> {
    out.write_all(page.text.as_bytes())
}

/// Write the faint one-line key-hint printed above the `> ` prompt, adapted to
/// context: the `[1-N] open` hint appears only when the page has links, and
/// `b back` only when there is somewhere to go back to. The prompt itself
/// follows on the next line (no trailing newline).
fn write_prompt_hint<W: Write>(
    out: &mut W,
    link_count: usize,
    has_back: bool,
    no_color: bool,
) -> std::io::Result<()> {
    let mut parts: Vec<String> = Vec::new();
    if link_count >= 1 {
        parts.push(format!("[1-{link_count}] open"));
    }
    if has_back {
        parts.push("b back".to_string());
    }
    parts.push("r reload".to_string());
    parts.push("u url".to_string());
    parts.push("d nodes".to_string());
    parts.push("h help".to_string());
    parts.push("q quit".to_string());
    let hint = parts.join(" · ");
    writeln!(out)?;
    if no_color {
        writeln!(out, "{hint}")?;
    } else {
        writeln!(out, "{FAINT}{hint}{RESET}")?;
    }
    write!(out, "> ")
}

/// Note where a `#anchor` resolves in the document. A full scroll TUI is out of
/// scope for v1, so the position is annotated rather than scrolled to.
fn write_anchor_note<W: Write>(out: &mut W, doc: &MicronDocument, anchor: &str) {
    match doc.anchors.get(anchor) {
        Some(block) => {
            let _ = writeln!(
                out,
                "(anchor #{anchor} is at block {block}; scroll is v1 out of scope)"
            );
        }
        None => {
            let _ = writeln!(out, "(anchor #{anchor} not found on this page)");
        }
    }
}

/// Fetch, parse, render and print a single page. Returns the rendered page so
/// its link list can drive the next navigation step.
async fn load_and_show<W: Write>(
    out: &mut W,
    session: &mut Session,
    target: &Target,
    opts: &BrowserOptions,
    anchor: Option<&str>,
) -> Result<RenderedPage, FetchError> {
    let doc = fetch_document(session, target, opts.timeout).await?;
    let page = render_with_options(&doc, opts.width, opts.no_color, opts.depth);
    let _ = write_page(out, &page);
    if let Some(a) = anchor {
        write_anchor_note(out, &doc, a);
    }
    Ok(page)
}

/// A one-shot fetch/render/print, for `--print` mode and non-tty stdout.
///
/// Returns `Ok(())` on success; the caller maps a [`FetchError`] to an exit
/// code. The page is resolved against no current destination, so the URL must
/// name a destination.
pub async fn print_once<W: Write>(
    out: &mut W,
    session: &mut Session,
    target: &Target,
    opts: &BrowserOptions,
) -> Result<(), FetchError> {
    load_and_show(out, session, target, opts, None)
        .await
        .map(|_| ())
}

/// Run node discovery for `duration`, then print the accumulated list once and
/// return. Used for `--discover --print` and non-tty stdout, so a scripted or
/// piped invocation never blocks on a prompt.
pub async fn discover_print<W: Write>(
    out: &mut W,
    session: &mut Session,
    duration: Duration,
) -> std::io::Result<()> {
    session.run_discovery(duration, |_| {}).await;
    write_node_list(out, session)
}

/// Run node discovery interactively: print each newly seen node as it arrives,
/// then let the reader open one by number (or `q` to quit). Opening a node hands
/// off to the full [`run`] browser on that node's default page.
pub async fn discover_interactive<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    session: &mut Session,
    duration: Duration,
    opts: &BrowserOptions,
) -> std::io::Result<()> {
    writeln!(
        out,
        "Discovering NomadNet nodes for {}s...",
        duration.as_secs()
    )?;
    out.flush()?;

    // Print newly discovered nodes as they arrive; re-announces of a known node
    // are folded silently into the registry.
    let mut announced: std::collections::HashSet<[u8; 16]> = std::collections::HashSet::new();
    session
        .run_discovery(duration, |node| {
            if announced.insert(node.dest_hash) {
                let _ = writeln!(
                    out,
                    "  [{}] {}  {}",
                    announced.len(),
                    node.display_name(),
                    node.dest_hex()
                );
            }
        })
        .await;

    writeln!(out)?;
    write_node_list(out, session)?;

    if session.discovered_nodes().is_empty() {
        return Ok(());
    }

    loop {
        write!(out, "\nopen node [N], or q to quit> ")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            writeln!(out)?;
            break;
        }
        match parse_command(&line) {
            Command::Empty => {}
            Command::Quit => break,
            Command::Nodes => write_node_list(out, session)?,
            Command::Help => writeln!(out, "{HELP}")?,
            Command::Follow(n) | Command::OpenNode(n) => match node_target(session, n) {
                Some(target) => return run(input, out, session, target, opts).await,
                None => writeln!(out, "no discovered node [{n}]")?,
            },
            _ => writeln!(out, "enter a node number, or q to quit")?,
        }
    }
    Ok(())
}

/// The help text shown for the `h` command.
const HELP: &str = "\
Commands:
  N          follow link number N
  b          back to the previous page
  r          reload the current page
  u <url>    go to a new URL
  d / nodes  list NomadNet nodes discovered from announces
  o <N>      open discovered node number N
  h          show this help
  q / EOF    quit";

/// A one-line, clean message for a fetch error.
fn error_message(err: &FetchError) -> String {
    match err {
        FetchError::NoPath => "no path to destination".to_string(),
        FetchError::Timeout => "request timed out".to_string(),
        FetchError::NotFound => "page not found on destination".to_string(),
        FetchError::LinkFailed => "link to destination failed".to_string(),
        FetchError::UnsupportedFile => "file downloads are not supported yet".to_string(),
        other => other.to_string(),
    }
}

/// Run the interactive browser loop.
///
/// Loads `initial`, then reads commands from `input`, driving navigation and
/// printing to `out`. A fetch failure prints a clean one-line message and stays
/// in the loop. Returns when the user quits or `input` reaches EOF.
pub async fn run<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    session: &mut Session,
    initial: Target,
    opts: &BrowserOptions,
) -> std::io::Result<()> {
    let mut nav = Nav::new();
    nav.visit(initial);
    // Safe: visit() just set the current target.
    let mut links = show_current(out, session, &nav, opts, None).await;

    loop {
        write_prompt_hint(out, links.len(), nav.history_len() > 0, opts.no_color)?;
        out.flush()?;

        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            writeln!(out)?;
            break; // EOF quits.
        }

        match parse_command(&line) {
            Command::Empty => {}
            Command::Quit => break,
            Command::Help => writeln!(out, "{HELP}")?,
            Command::Unknown(raw) => {
                writeln!(out, "unknown command: {raw:?} (h for help)")?;
            }
            Command::Follow(n) => {
                let Some(link) = links.iter().find(|l| l.index == n).cloned() else {
                    writeln!(out, "no link [{n}] on this page")?;
                    continue;
                };
                let (target, anchor) = match resolve_link(&link, nav.current_dest()) {
                    Ok(resolved) => resolved,
                    Err(_) => {
                        writeln!(out, "link [{n}] has a malformed target: {}", link.target)?;
                        continue;
                    }
                };
                nav.visit(target);
                links = show_current(out, session, &nav, opts, anchor.as_deref()).await;
            }
            Command::Back => {
                if nav.back().is_none() {
                    writeln!(out, "no previous page")?;
                    continue;
                }
                links = show_current(out, session, &nav, opts, None).await;
            }
            Command::Reload => {
                links = show_current(out, session, &nav, opts, None).await;
            }
            Command::Go(url) => {
                let target = match parse_url(&url, nav.current_dest()) {
                    Ok(target) => target,
                    Err(_) => {
                        writeln!(out, "malformed URL: {url}")?;
                        continue;
                    }
                };
                nav.visit(target);
                links = show_current(out, session, &nav, opts, None).await;
            }
            Command::Nodes => {
                write_node_list(out, session)?;
            }
            Command::OpenNode(n) => {
                let target = match node_target(session, n) {
                    Some(target) => target,
                    None => {
                        writeln!(out, "no discovered node [{n}] (try `d` to list)")?;
                        continue;
                    }
                };
                nav.visit(target);
                links = show_current(out, session, &nav, opts, None).await;
            }
        }
    }
    Ok(())
}

/// Resolve discovered node index `n` to a fetch target for its default page.
fn node_target(session: &Session, n: usize) -> Option<Target> {
    let node = session.discovered_node(n)?;
    parse_url(&node.dest_hex(), None).ok()
}

/// Write the discovered-nodes list (or a hint when none are known yet). The
/// numbering matches the `o <N>` command's 1-based index.
pub fn write_node_list<W: Write>(out: &mut W, session: &Session) -> std::io::Result<()> {
    let nodes = session.discovered_nodes();
    if nodes.is_empty() {
        writeln!(
            out,
            "no NomadNet nodes discovered yet (announces arrive as nodes come online)"
        )?;
        return Ok(());
    }
    writeln!(out, "Discovered NomadNet nodes:")?;
    let now = crate::discovery::now_unix_secs();
    for (i, node) in nodes.iter().enumerate() {
        writeln!(out, "  {}", format_node_line(i + 1, node, now))?;
    }
    Ok(())
}

/// Format one discovered-node list line: `[N] <name>  <dest_hash>  hops=H  last-seen Xs ago`.
pub fn format_node_line(index: usize, node: &crate::discovery::DiscoveredNode, now: u64) -> String {
    let hops = match node.hops {
        Some(h) => h.to_string(),
        None => "?".to_string(),
    };
    let age = now.saturating_sub(node.last_seen);
    format!(
        "[{index}] {}  {}  hops={hops}  last-seen {age}s ago",
        node.display_name(),
        node.dest_hex(),
    )
}

/// Load and display the current page, returning its links. On a fetch error a
/// clean message is printed and an empty link list is returned so the loop can
/// continue (the current target stays set, so `r` retries).
async fn show_current<W: Write>(
    out: &mut W,
    session: &mut Session,
    nav: &Nav,
    opts: &BrowserOptions,
    anchor: Option<&str>,
) -> Vec<RenderedLink> {
    let Some(target) = nav.current() else {
        return Vec::new();
    };
    // Orientation "address bar" at the top of the page: the friendly node name
    // when the announce registry knows one, else the short dest hex.
    let name = session.node_name(&target.dest_hash);
    let _ = write_status_bar(out, name.as_deref(), &target.dest_hash, &target.path, opts);
    match load_and_show(out, session, target, opts, anchor).await {
        Ok(page) => page.links,
        Err(err) => {
            let _ = writeln!(out, "error: {}", error_message(&err));
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_HEX: &str = "0123456789abcdef0123456789abcdef";
    const HASH_BYTES: [u8; 16] = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef,
    ];
    const OTHER_HASH: [u8; 16] = [0xaa; 16];

    fn link(target: &str, fields: Vec<(&str, &str)>) -> RenderedLink {
        RenderedLink {
            index: 1,
            label: "L".to_string(),
            target: target.to_string(),
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..RenderedLink::default()
        }
    }

    // --- command parser ---

    #[test]
    fn parses_a_bare_number_as_follow() {
        assert_eq!(parse_command("3"), Command::Follow(3));
        assert_eq!(parse_command("  12 "), Command::Follow(12));
    }

    #[test]
    fn parses_navigation_letters() {
        assert_eq!(parse_command("b"), Command::Back);
        assert_eq!(parse_command("back"), Command::Back);
        assert_eq!(parse_command("r"), Command::Reload);
        assert_eq!(parse_command("h"), Command::Help);
        assert_eq!(parse_command("?"), Command::Help);
        assert_eq!(parse_command("q"), Command::Quit);
        assert_eq!(parse_command("quit"), Command::Quit);
    }

    #[test]
    fn parses_go_with_url_argument() {
        assert_eq!(
            parse_command("u abc:/page/x.mu"),
            Command::Go("abc:/page/x.mu".to_string())
        );
        assert_eq!(
            parse_command("  go   somewhere "),
            Command::Go("somewhere".to_string())
        );
    }

    #[test]
    fn go_without_argument_is_unknown() {
        assert_eq!(parse_command("u"), Command::Unknown("u".to_string()));
    }

    #[test]
    fn parses_nodes_and_open_commands() {
        assert_eq!(parse_command("d"), Command::Nodes);
        assert_eq!(parse_command("nodes"), Command::Nodes);
        assert_eq!(parse_command("o 2"), Command::OpenNode(2));
        assert_eq!(parse_command("open 10"), Command::OpenNode(10));
        // `o` needs a numeric argument; without one it is unknown.
        assert_eq!(parse_command("o"), Command::Unknown("o".to_string()));
        assert_eq!(parse_command("o x"), Command::Unknown("o x".to_string()));
    }

    #[test]
    fn format_node_line_shows_index_name_hash_hops_and_age() {
        let node = crate::discovery::DiscoveredNode {
            dest_hash: [0xab; 16],
            name: Some("Test Node".to_string()),
            first_seen: 100,
            last_seen: 140,
            hops: Some(3),
        };
        let line = format_node_line(1, &node, 150);
        assert_eq!(
            line,
            "[1] Test Node  abababababababababababababababab  hops=3  last-seen 10s ago"
        );
    }

    #[test]
    fn format_node_line_falls_back_when_name_and_hops_absent() {
        let node = crate::discovery::DiscoveredNode {
            dest_hash: [0x01; 16],
            name: None,
            first_seen: 100,
            last_seen: 100,
            hops: None,
        };
        let line = format_node_line(2, &node, 100);
        // No name -> the dest hex is shown; unknown hops -> `?`.
        assert_eq!(
            line,
            "[2] 01010101010101010101010101010101  01010101010101010101010101010101  hops=?  last-seen 0s ago"
        );
    }

    #[test]
    fn empty_line_is_empty() {
        assert_eq!(parse_command(""), Command::Empty);
        assert_eq!(parse_command("   \n"), Command::Empty);
    }

    #[test]
    fn unrecognised_token_is_unknown() {
        assert_eq!(parse_command("xyz"), Command::Unknown("xyz".to_string()));
        // A negative number is not a valid usize, so it is not a Follow.
        assert_eq!(parse_command("-1"), Command::Unknown("-1".to_string()));
    }

    // --- navigation state machine ---

    fn target(dest: [u8; 16], path: &str) -> Target {
        Target {
            dest_hash: dest,
            path: path.to_string(),
            fields: Vec::new(),
            is_file: false,
        }
    }

    #[test]
    fn visit_pushes_previous_onto_history() {
        let mut nav = Nav::new();
        assert_eq!(nav.history_len(), 0);
        nav.visit(target(HASH_BYTES, "/page/a.mu"));
        // First visit has nothing to push.
        assert_eq!(nav.history_len(), 0);
        nav.visit(target(HASH_BYTES, "/page/b.mu"));
        assert_eq!(nav.history_len(), 1);
        assert_eq!(nav.current().unwrap().path, "/page/b.mu");
    }

    #[test]
    fn back_pops_history_into_current() {
        let mut nav = Nav::new();
        nav.visit(target(HASH_BYTES, "/page/a.mu"));
        nav.visit(target(HASH_BYTES, "/page/b.mu"));
        let back = nav.back().expect("history has an entry");
        assert_eq!(back.path, "/page/a.mu");
        assert_eq!(nav.history_len(), 0);
        // Nothing left to go back to.
        assert!(nav.back().is_none());
        // Current is unchanged by a failed back.
        assert_eq!(nav.current().unwrap().path, "/page/a.mu");
    }

    #[test]
    fn current_dest_tracks_the_active_page() {
        let mut nav = Nav::new();
        assert_eq!(nav.current_dest(), None);
        nav.visit(target(HASH_BYTES, "/page/a.mu"));
        assert_eq!(nav.current_dest(), Some(HASH_BYTES));
        nav.visit(target(OTHER_HASH, "/page/b.mu"));
        assert_eq!(nav.current_dest(), Some(OTHER_HASH));
    }

    // --- link resolution ---

    #[test]
    fn resolve_absolute_link_ignores_current_dest() {
        let l = link(&format!("{HASH_HEX}:/page/next.mu"), vec![]);
        let (t, anchor) = resolve_link(&l, Some(OTHER_HASH)).unwrap();
        assert_eq!(t.dest_hash, HASH_BYTES);
        assert_eq!(t.path, "/page/next.mu");
        assert!(anchor.is_none());
    }

    #[test]
    fn resolve_relative_link_uses_current_dest() {
        // A same-destination link (leading `:`) resolves against current_dest.
        let l = link(":/page/rel.mu", vec![]);
        let (t, _) = resolve_link(&l, Some(OTHER_HASH)).unwrap();
        assert_eq!(t.dest_hash, OTHER_HASH);
        assert_eq!(t.path, "/page/rel.mu");
    }

    #[test]
    fn resolve_carries_preset_fields_and_drops_form_placeholders() {
        let l = link(
            &format!("{HASH_HEX}:/page/x.mu"),
            vec![("g", "reticulum"), ("ref", "")],
        );
        let (t, _) = resolve_link(&l, None).unwrap();
        // The preset field is carried with the var_ prefix; the valueless
        // form-field reference is dropped here (the TUI collects its current
        // value as a `field_` entry at submit time instead).
        assert_eq!(
            t.fields,
            vec![("var_g".to_string(), "reticulum".to_string())]
        );
    }

    #[test]
    fn resolve_splits_anchor_off_the_target() {
        let l = link(&format!("{HASH_HEX}:/page/x.mu#section2"), vec![]);
        let (t, anchor) = resolve_link(&l, None).unwrap();
        assert_eq!(t.path, "/page/x.mu");
        assert_eq!(anchor.as_deref(), Some("section2"));
    }

    #[test]
    fn resolve_anchor_with_preset_fields() {
        let l = link(&format!("{HASH_HEX}:/page/x.mu#top"), vec![("a", "1")]);
        let (t, anchor) = resolve_link(&l, None).unwrap();
        assert_eq!(t.path, "/page/x.mu");
        assert_eq!(t.fields, vec![("var_a".to_string(), "1".to_string())]);
        assert_eq!(anchor.as_deref(), Some("top"));
    }

    #[test]
    fn resolve_relative_without_current_is_malformed() {
        let l = link(":/page/x.mu", vec![]);
        assert!(resolve_link(&l, None).is_err());
    }

    // --- visual chrome: status bar, prompt hint ---

    fn opts(width: usize, no_color: bool) -> BrowserOptions {
        BrowserOptions {
            width,
            no_color,
            depth: ColorDepth::Truecolor,
            timeout: Duration::from_secs(1),
        }
    }

    fn render_to_string<F>(f: F) -> String
    where
        F: FnOnce(&mut Vec<u8>) -> std::io::Result<()>,
    {
        let mut buf = Vec::new();
        f(&mut buf).expect("write");
        String::from_utf8(buf).expect("utf8")
    }

    #[test]
    fn write_page_emits_text_with_no_legend() {
        let page = RenderedPage {
            text: "body text\n".to_string(),
            links: vec![RenderedLink {
                index: 1,
                label: "L1".to_string(),
                target: "/page/1.mu".to_string(),
                fields: Vec::new(),
                ..RenderedLink::default()
            }],
        };
        let out = render_to_string(|w| write_page(w, &page));
        assert_eq!(
            out, "body text\n",
            "write_page should emit only the page text"
        );
        // No trailing `Links:` legend and no legend entry.
        assert!(!out.contains("Links:"), "legend block leaked: {out:?}");
        assert!(
            !out.contains("-> /page/1.mu"),
            "legend entry leaked: {out:?}"
        );
    }

    #[test]
    fn status_bar_shows_short_hex_and_path_truncated() {
        let out = render_to_string(|w| {
            write_status_bar(w, None, &HASH_BYTES, "/page/index.mu", &opts(80, false))
        });
        // Short dest hex: first 8 hex chars + ellipsis.
        assert!(out.contains("01234567…"), "got: {out:?}");
        assert!(out.contains(":/page/index.mu"), "got: {out:?}");
        assert!(
            out.starts_with('\x1b'),
            "status bar should be dimmed: {out:?}"
        );
    }

    #[test]
    fn status_bar_prefers_node_name_and_is_plain_under_no_color() {
        let out = render_to_string(|w| {
            write_status_bar(w, Some("Alpha"), &HASH_BYTES, "/page/x.mu", &opts(80, true))
        });
        assert!(!out.contains('\x1b'), "SGR leaked: {out:?}");
        assert!(out.contains("Alpha"), "node name missing: {out:?}");
        assert!(
            !out.contains("01234567"),
            "hex shown despite a name: {out:?}"
        );
    }

    #[test]
    fn status_bar_truncates_to_width() {
        let out = render_to_string(|w| {
            write_status_bar(
                w,
                Some("A very long node name here"),
                &HASH_BYTES,
                "/page/x.mu",
                &opts(10, true),
            )
        });
        // Plain output: the single line (minus newline) is at most `width` chars.
        let line = out.trim_end_matches('\n');
        assert_eq!(line.chars().count(), 10, "not truncated to width: {line:?}");
    }

    #[test]
    fn prompt_hint_includes_link_range_when_links_present() {
        let out = render_to_string(|w| write_prompt_hint(w, 3, false, true));
        assert!(out.contains("[1-3] open"), "got: {out:?}");
        assert!(out.ends_with("> "), "prompt missing: {out:?}");
    }

    #[test]
    fn prompt_hint_omits_link_range_when_no_links() {
        let out = render_to_string(|w| write_prompt_hint(w, 0, false, true));
        assert!(
            !out.contains("open"),
            "link hint shown with no links: {out:?}"
        );
    }

    #[test]
    fn prompt_hint_shows_back_only_with_history() {
        let with_back = render_to_string(|w| write_prompt_hint(w, 1, true, true));
        assert!(with_back.contains("b back"), "got: {with_back:?}");
        let no_back = render_to_string(|w| write_prompt_hint(w, 1, false, true));
        assert!(
            !no_back.contains("b back"),
            "back shown with empty history: {no_back:?}"
        );
    }

    #[test]
    fn prompt_hint_is_dimmed_with_colour_plain_without() {
        let coloured = render_to_string(|w| write_prompt_hint(w, 1, false, false));
        assert!(
            coloured.contains("\x1b[2m"),
            "hint not dimmed: {coloured:?}"
        );
        let plain = render_to_string(|w| write_prompt_hint(w, 1, false, true));
        assert!(!plain.contains('\x1b'), "SGR leaked: {plain:?}");
    }
}
