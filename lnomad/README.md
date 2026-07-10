# lnomad

A terminal browser for NomadNet micron (`.mu`) pages. It connects to a running
`lnsd`/`rnsd` shared instance, fetches a page over the Reticulum
request/response path, renders the micron markup to ANSI, and lets you follow
links interactively.

## Usage

```
lnomad <address> [options]
```

The address selects a destination and a page path:

```
<dest_hash>                     open the destination's /page/index.mu
<dest_hash>:/page/about.mu      open a specific page
<dest_hash>:/page/x.mu`a=1|b=2  carry preset query fields (var_a, var_b)
```

`<dest_hash>` is 32 hex characters (the 16-byte truncated destination hash).

## Discovering nodes

Without a destination hash, `lnomad --discover` finds NomadNet page-hosting
nodes by listening for their announces. Every NomadNet node announces the
`nomadnetwork.node` destination, so the announces can be recognised and their
destination hash and display name collected without knowing anything in advance:

```
lnomad --discover                 listen (default 30s) and list nodes found
lnomad --discover 60              listen for 60 seconds (bare positional)
lnomad --discover --duration 60   listen for 60 seconds (explicit flag)
```

In `--discover` mode the positional argument is the listen duration in seconds,
not a page address: `lnomad --discover 5` and `lnomad --discover --duration 5` are
equivalent. A non-numeric positional is rejected, and giving both a positional
and a `--duration` that disagree is an error.

On a terminal, each node is printed as it is first seen, then a list is shown:

```
[N] <name>  <dest_hash>  hops=H  last-seen Xs ago
```

Enter a number to open that node's `/page/index.mu` in the browser, or `q` to
quit. With `--print` or non-tty stdout, the accumulated list is printed after the
listen window and the command exits. The discovered list is also reachable from
the browser with the `d` (`nodes`) command, and `o <N>` opens a listed node.

In the browser, discovery runs continuously in the background from startup: node
announces are folded into the places panel whether or not a page is loading, so
the list keeps filling while you read, scroll, or have a panel open. The registry
is a bounded FIFO of the 500 most recent nodes (re-announces update in place; once
full, the oldest-seen node is evicted), and is held in RAM only.

### Options

- `--instance <name>`  shared-instance name (overrides the config file's)
- `--config <dir>`     Reticulum config directory (default: the platform default)
- `--no-color`         disable ANSI colour
- `--theme <t>`        colour theme: `auto` (default), `light`, or `dark`. `auto`
  detects the terminal background (OSC 11, with a `COLORFGBG` fallback) and picks
  the matching theme, defaulting to dark when it cannot tell. Ignored with
  `--print` or non-tty output.
- `--color <d>`        colour depth: `auto` (default), `truecolor`, or `256`.
  `auto` emits 24-bit true colour when `COLORTERM` is `truecolor`/`24bit` and
  otherwise downgrades every colour to the nearest xterm-256 palette index, so
  terminals without true-colour support still render sensibly. `--no-color`
  overrides this and drops to monochrome.
- `--width <n>`        render width (default: detected terminal width, else 80)
- `--timeout <s>`      per-request timeout in seconds (default 30)
- `--print`            fetch, render and print once, then exit
- `--discover`         list NomadNet nodes seen from announces (no address needed)
- `--duration <s>`     `--discover` listen window in seconds (default 30); may
  also be given as the bare positional, e.g. `lnomad --discover <s>`

When stdout is not a terminal, `lnomad` prints once and exits even without
`--print`, so piping and redirection never block on the prompt.

## Interactive keys

On a terminal, `lnomad` opens a full-screen browser: a one-row top-bar (the page
title, a `·`, and the address, with a right-aligned status cluster: a bookmark
star, a cache bolt, and the hop count to the node), the scrollable page, and a
footer. The footer is a strip of clickable button-hints where a keybinding and a
button are the same thing: the navigation trio (`Alt-← back`, `Alt-→ forward`,
`R reload`) first, then the current mode's actions. Each button's key reads
bright and bold, its label muted; press the key or click the button. On a narrow
terminal the footer drops the lowest-priority buttons and, if still too tight,
collapses the rest to their keys. Links carry no `[N]` marker and there is
no link legend; a link is set apart by its underline and colour, and is reached
by focus, hint or click:

- `j` / `k`, arrows, `Ctrl-f` / `Ctrl-b`, `g` / `G`  scroll
- `Tab` / `Shift-Tab`  move the focus cursor across links AND form fields, in
  document order (auto-scrolls)
- `Enter`     follow the focused link
- form fields, when focused: type to edit a text field, `Space` to toggle a
  checkbox / radio, `Esc` to leave field editing; a click focuses a field too
- `f`         hint mode: type the label shown over a link (or the link's text)
- `/`         in-page search: type a query, `Enter` highlights every match and
  jumps to the first; `n` / `N` cycle to the next / previous match, `Esc` clears
- click       follow a link, activate a top-bar control, or press a footer button
- `:`         enter an address
- `R` / `Ctrl-R` / `F5`  reload the page (always refetches, bypassing the cache)
- `t`         toggle the light / dark theme (correct a wrong auto-detection)
- `Alt-←` / `Alt-→`  back / forward
- mouse back / forward side buttons  back / forward
- `Esc` / `Ctrl-g`  cancel a load
- `?`         toggle the help overlay
- `q` / `Ctrl-c`  quit

The focused or hovered link's target appears in a small floating field at the
bottom-left of the content, just above the footer, so it never covers the
clickable button-hints. Same-destination links (`:/page/x.mu`) resolve against
the page currently in view; a followed link carries its preset (`f=v`) fields as
`var_*` request variables.

Recently viewed pages are held in an in-RAM cache (the last 50 distinct pages),
so revisiting one, including stepping back and forward through history, renders
instantly from memory with your last scroll position restored. The cache is
transparent: `R` always refetches (bypassing it and refreshing the stored copy),
and non-idempotent form submits are never cached. A shown page served from the
cache carries a subtle `⚡` bolt in the top-bar status cluster.

The places panel (`d`) takes the same up/down motions as the page scroll applied
to its selection: `j` / `k`, `Ctrl-n` / `Ctrl-p`, arrows step a row; `Ctrl-f` /
`Ctrl-b` and `Ctrl-d` / `Ctrl-u` jump several; `g` / `G`, `Home` / `End` go to
the first / last entry. `Enter` opens the selection, `x` deletes the selected
bookmark, `Esc` / `d` close the panel.

### Form fields and submitting

A page can carry input fields (`` `<name`> `` text, `` `<?|name`Label> ``
checkbox, `` `<^|name`Label> `` radio). They render as input boxes, initialised
from their prefill; `Tab` reaches them and, once focused, they edit in place. A
link that references a field by name (e.g. `` `[Submit`:/page/s.mu`name] ``, or
`*` for every field) is a submit: following it collects the current values of the
referenced fields and sends them as NomadNet expects, each under a `field_<name>`
request variable, alongside any `var_*` presets. This interoperates with a real
NomadNet node, whose page handler reads the same `field_*` / `var_*` variables.

A link whose target is an external URL (an `http`, `https` or `mailto` scheme)
is not fetched in-mesh: it is handed to the platform default handler (`xdg-open`
on Linux). Any other scheme (`file`, `javascript`, custom schemes) is refused
and reported in a transient toast, since a page comes from an untrusted node and
an arbitrary URI must never reach a system handler.

The two bottom surfaces split cleanly. The bottom-left floating field carries the
current pointer/page state (a focused/hovered link's target, or the loading
spinner and path during a fetch) and stays as long as it applies. Transient notes
(a fetch error, a refused link, "copied", "bookmarked", "cancelled") appear as an
auto-dismissing toast floated at the bottom-right of the content; a toast clears
after a few seconds or on the next key press. Neither covers the footer, which
always keeps its clickable button-hints.

## v1 limits

- A `#anchor` in a target (a followed link or the initial address) is resolved
  against the page's anchors and scrolled to on load; an unknown anchor falls
  back to the top of the page with a toast note.
- `/file/` downloads are not supported.
