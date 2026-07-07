# lnomad

A terminal browser for NomadNet micron (`.mu`) pages. It connects to a running
`lnsd`/`rnsd` shared instance, fetches a page over the Reticulum
request/response path, renders the micron markup to ANSI, and lets you follow
links interactively.

## Usage

```
lnomad <url> [options]
```

The URL selects a destination and a page path:

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
not a page URL: `lnomad --discover 5` and `lnomad --discover --duration 5` are
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

### Options

- `--instance <name>`  shared-instance name (overrides the config file's)
- `--config <dir>`     Reticulum config directory (default: the platform default)
- `--no-color`         disable ANSI colour
- `--theme <t>`        colour theme: `auto` (default), `light`, or `dark`. `auto`
  detects the terminal background (OSC 11, with a `COLORFGBG` fallback) and picks
  the matching theme, defaulting to dark when it cannot tell. Ignored with
  `--print` or non-tty output.
- `--width <n>`        render width (default: detected terminal width, else 80)
- `--timeout <s>`      per-request timeout in seconds (default 30)
- `--print`            fetch, render and print once, then exit
- `--discover`         list NomadNet nodes seen from announces (no URL needed)
- `--duration <s>`     `--discover` listen window in seconds (default 30); may
  also be given as the bare positional, e.g. `lnomad --discover <s>`

When stdout is not a terminal, `lnomad` prints once and exits even without
`--print`, so piping and redirection never block on the prompt.

## Interactive keys

On a terminal, `lnomad` opens a full-screen browser: a top-bar (page title,
back / forward / reload controls, address slot), the scrollable page, and a
status bar. Links carry no `[N]` marker and there is no link legend; a link is
set apart by its underline and colour, and is reached by focus, hint or click:

- `j` / `k`, arrows, `Ctrl-f` / `Ctrl-b`, `g` / `G`  scroll
- `Tab` / `Shift-Tab`  move the focus cursor across links (auto-scrolls)
- `Enter`     follow the focused link
- `f`         hint mode: type the label shown over a link (or the link's text)
- `/`         in-page search: type a query, `Enter` highlights every match and
  jumps to the first; `n` / `N` cycle to the next / previous match, `Esc` clears
- click       follow a link, or activate a top-bar control
- `:`         enter an address
- `R`         reload the page
- `t`         toggle the light / dark theme (correct a wrong auto-detection)
- `M-←` / `M-→`  back / forward
- `Esc` / `Ctrl-g`  cancel a load
- `?`         toggle the help overlay
- `q` / `Ctrl-c`  quit

The focused or hovered link's target is shown in the status bar. Same-destination
links (`:/page/x.mu`) resolve against the page currently in view; a followed link
carries its preset (`f=v`) fields as `var_*` request variables.

A link whose target is an external URL (an `http`, `https` or `mailto` scheme)
is not fetched in-mesh: it is handed to the platform default handler (`xdg-open`
on Linux). Any other scheme (`file`, `javascript`, custom schemes) is refused
and reported in the status bar, since a page comes from an untrusted node and an
arbitrary URI must never reach a system handler.

## v1 limits

- Interactive form-field input (fields the reader must type) is a stub: a link
  is followed with its preset fields only, and a note is printed when a link
  carries form fields.
- A `#anchor` in a target (a followed link or the initial URL) is resolved
  against the page's anchors and scrolled to on load; an unknown anchor falls
  back to the top of the page with a status note.
- `/file/` downloads are not supported.
