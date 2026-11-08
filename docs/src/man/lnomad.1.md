# lnomad(1)

## NAME

lnomad -- terminal browser for NomadNet micron pages

## SYNOPSIS

**lnomad** [*options*] [*url*]

## DESCRIPTION

**lnomad** fetches and renders NomadNet micron pages over Reticulum, either interactively in a terminal UI or once to standard output with **--print**. It connects to a running daemon (**lnsd** or **rnsd**) through the shared instance, so one of them must be running.

Node discovery runs continuously while the UI is up: `nomadnetwork.node` announces are folded into the places panel (`d`) as they arrive. Started without a *url* on a terminal, **lnomad** opens its start screen with that panel showing; without a terminal a *url* is required.

A `/file/` URL downloads instead of rendering; see **--output** for where the file lands.

Pictures are drawn in the page. Micron has no image construct, so a page can only link to a picture in its node's file area; **lnomad** recognises such a link by its target — a `/file/` path named `.png`, `.jpg`, `.jpeg` or `.gif` — and, when the link stands alone on its line, fetches it and draws it there. The terminal's own graphics protocol is used when it has one (Kitty, iTerm2 or Sixel); otherwise the picture is drawn with Unicode half-blocks, and where neither is possible a line naming the file, its format and its size takes its place. A page's pictures are fetched after it is displayed, one at a time, so nothing waits for them, `Esc` cancels the rest, and no more than eight are fetched from any one page. Fetched pictures are kept in memory (see **--image-cache**), so stepping back to a page shows them again without spending the airtime twice. Use **--images off** to fetch none at all.

With a picture focused (`Tab`), `Enter` saves it to the download directory and `o` opens it in the system viewer. Neither transfers it a second time.

## NAMES

The address editor (`:`) also takes a name. Anything typed there that is not an address (a 32-hex hash, a local `:` URL, a `nururl:` address, a `scheme://` form or an `lxmf@` address) is a search: a case-insensitive substring match over the names that `nomadnetwork.node` announces carried and over the four-hex-digit short hash shown after each name, so `fa01` finds `Marcel Adingbatponder ·fa01`. The result list shows each site as `name ·hash4` with its hops, when it was last heard, and its pin mark; `Enter` opens one. A name that matches exactly one site exactly opens it directly.

A search is not a lookup. The names searched are the announces heard since **lnomad** started plus the sites already pinned; nothing is asked of any server. The daemon's announce cache is not searched yet: the shared-instance RPC has no query that returns the names it holds, only hashes and sizes.

A pin records the name a site announced and its full hash, the first time that site is opened, in `pins.toml` (see **XDG_CONFIG_HOME**), the way SSH remembers a host key. A later hit announcing a pinned name under a different hash is marked `! name pinned to ·hash4` and listed below the pinned site; opening it asks once, and a yes pins the second hash under the same name beside the first. Nothing is replaced silently. A site that now announces another name than it was pinned under shows `pinned as` *name*.

Names are hints and the hash is the address. Reticulum has no registry because a name anyone can announce cannot be owned without someone to decide who owns it, and the network is built so that nobody has to be trusted to reach anybody; the short hash beside every name is what makes the name checkable.

A `nururl:<hash>[/path][?query]` address opens that page: `nn.`*k*`=`*v* query entries are carried as the request variable `var_`*k*, and any other entry (the hint among them) is dropped with a note.

## OPTIONS

*url*
:   Page to open, as ``<address>[:/page/x.mu[`f=v|...]]``, where *address* is the node's 32-hex-character destination address and everything after the `:` is the request path. A bare address opens the node's default page. Leaving the address out but keeping the `:` (`:/page/x.mu`) makes it a URL to a local page — local to the page in view — so it is only meaningful inside the UI, never on the command line. A bare request path with no `:` names no node and is rejected, as it is by NomadNet. Optional on a terminal, where omitting it opens the start screen.

**--config** *dir*
:   Reticulum configuration directory. Defaults to the platform default, the same one **lncp**(1) uses.

**--instance** *name*
:   Shared-instance name to connect to, overriding the configuration file.

**--no-start**
:   Fail when no shared instance answers, as the tools did before the zero-config step 4, instead of starting **lnsd**(1) as a child. Without it, **lnomad** looks for a running shared instance (the abstract socket `@rns/`*instance*, then TCP loopback at the configured `shared_instance_port`, else 37428, for the instance `default` or a configured port) and, when none answers, starts `lnsd -c` *dir* on the same configuration directory and waits until it accepts. The daemon is **lnomad**'s child and goes when **lnomad** exits; its output goes to `lnsd.log` in the configuration directory. A start is reported on standard error as `INSTANCE started=yes via=unix pid=`*pid*. A configuration with `share_instance = No` starts nothing. The **lnsd** started is `$LEVICULUM_LNSD`, else the one beside **lnomad**, else the one on `PATH`.

**--print**
:   Fetch, render and print the page once, then exit. Non-interactive.

**--output** *path*
:   Where a `/file/` download is saved. An existing directory, or a path ending in `/`, receives the file under the name the server sent, falling back to the URL basename; any other path names the exact file to write. By default the file lands in the current directory, and an existing file is preserved by appending ` (1)`, ` (2)` and so on.

**--width** *columns*
:   Render width. Defaults to the detected terminal width, otherwise 80.

**--timeout** *seconds*
:   Per-request fetch timeout (default: 30).

**--images** *mode*
:   Inline images. `auto` draws a page's pictures where they are linked, using the terminal's graphics protocol or Unicode half-blocks; `off` leaves every image link an ordinary link and fetches nothing. Ignored with **--print** and when output is not a terminal, neither of which draws pictures (default: `auto`).

**--image-cache** *megabytes*
:   How much fetched image data to keep in memory for revisits (default: 10). A page visited again then costs no airtime for pictures it has already shown. When the budget is exceeded, the pictures untouched longest are dropped until it fits again; a single picture larger than the whole budget is never kept. `0` disables the cache. Memory only — nothing is written to disk.

**--no-color**
:   Disable ANSI colour in the rendered output. Also suppresses inline images: a picture painted out of coloured blocks is not what a reader asking for no colour had in mind. **NO_COLOR** set and not empty does the same.

**--theme** *theme*
:   Colour theme for the interactive UI: `auto` detects the terminal background (dark when it does not answer), `light` and `dark` force a theme; `t` swaps them while browsing. Ignored with **--print** and when output is not a terminal (default: `auto`). **lnomad** has no settings file, so unlike **loquax**(1) there is no `theme` key.

**--color** *tier*
:   Terminal colour tier. `auto` picks true colour when `COLORTERM` is `truecolor` or `24bit`, the xterm-256 palette when `TERM` contains `256color`, no colour when `TERM` is `dumb` or unset, and the sixteen ANSI colours otherwise; `truecolor`, `256` and `16` force the tier. A page's own colours are drawn at the nearest the tier has; in 16 colours inline pictures fall back to their text line. **--no-color** and **NO_COLOR** still override this (default: `auto`).

## ENVIRONMENT

**COLORTERM**, **TERM**
:   Consulted by `--color auto` to choose the colour tier.

**NO_COLOR**
:   Set and not empty: no colour, as **--no-color**.

**XDG_CONFIG_HOME**
:   Base for **lnomad**'s own directory (`lnomad/`), holding the bookmarks (`bookmarks.toml`), the pinned site names (`pins.toml`, see **NAMES**), the per-node identify decisions (`identify.toml`) and the identity **lnomad** reveals when identifying (`identity`). Defaults to `~/.config`.

**XDG_DOWNLOAD_DIR**
:   Where a `/file/` link followed inside the UI is saved, inline pictures included. Defaults to `$HOME/Downloads`. The **--output** option covers downloads started from the command line instead.

## EXAMPLES

Open a node's default page interactively:

    lnomad a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4

Print a specific page without entering the UI:

    lnomad --print a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4:/page/index.mu

Open the start screen and pick a node from the places panel:

    lnomad

Find a site by a name it announced, from inside the UI:

    :marcel

Download a file to a chosen directory:

    lnomad --output ~/downloads/ a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4:/file/manual.pdf

## SEE ALSO

**lnsd**(1), **lblogd**(1), **lncp**(1)
