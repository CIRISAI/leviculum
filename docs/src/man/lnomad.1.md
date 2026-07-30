# lnomad(1)

## NAME

lnomad -- terminal browser for NomadNet micron pages

## SYNOPSIS

**lnomad** [*options*] [*url*]

## DESCRIPTION

**lnomad** fetches and renders NomadNet micron pages over Reticulum, either interactively in a terminal UI or once to standard output with **--print**. It connects to a running daemon (**lnsd** or **rnsd**) through the shared instance, so one of them must be running.

Node discovery runs continuously while the UI is up: `nomadnetwork.node` announces are folded into the places panel (`d`) as they arrive. Started without a *url* on a terminal, **lnomad** opens its start screen with that panel showing; without a terminal a *url* is required.

A `/file/` URL downloads instead of rendering; see **--output** for where the file lands.

## OPTIONS

*url*
:   Page to open, as `<dest_hash>[:/page/x.mu[`f=v|...]]`. A bare destination hash opens the node's default page. Optional on a terminal, where omitting it opens the start screen.

**--config** *dir*
:   Reticulum configuration directory. Defaults to the platform default, the same one **lncp**(1) uses.

**--instance** *name*
:   Shared-instance name to connect to, overriding the configuration file.

**--print**
:   Fetch, render and print the page once, then exit. Non-interactive.

**--output** *path*
:   Where a `/file/` download is saved. An existing directory, or a path ending in `/`, receives the file under the name the server sent, falling back to the URL basename; any other path names the exact file to write. By default the file lands in the current directory, and an existing file is preserved by appending ` (1)`, ` (2)` and so on.

**--width** *columns*
:   Render width. Defaults to the detected terminal width, otherwise 80.

**--timeout** *seconds*
:   Per-request fetch timeout (default: 30).

**--no-color**
:   Disable ANSI colour in the rendered output.

**--theme** *theme*
:   Colour theme for the interactive UI: `auto` detects the terminal background, `light` and `dark` force a theme. Ignored with **--print** and when output is not a terminal (default: `auto`).

**--color** *depth*
:   Terminal colour depth. `auto` picks true colour when `COLORTERM` is `truecolor` or `24bit` and otherwise falls back to the xterm-256 palette; `truecolor` and `256` force the depth. **--no-color** still overrides this (default: `auto`).

## ENVIRONMENT

**COLORTERM**
:   Consulted by `--color auto` to choose between true colour and the xterm-256 palette.

**XDG_CONFIG_HOME**
:   Base for **lnomad**'s own directory (`lnomad/`), holding the bookmarks (`bookmarks.toml`), the per-node identify decisions (`identify.toml`) and the identity **lnomad** reveals when identifying (`identity`). Defaults to `~/.config`.

**XDG_DOWNLOAD_DIR**
:   Where a `/file/` link followed inside the UI is saved. Defaults to `$HOME/Downloads`. The **--output** option covers downloads started from the command line instead.

## EXAMPLES

Open a node's default page interactively:

    lnomad a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4

Print a specific page without entering the UI:

    lnomad --print a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4:/page/index.mu

Open the start screen and pick a node from the places panel:

    lnomad

Download a file to a chosen directory:

    lnomad --output ~/downloads/ a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4:/file/manual.pdf

## SEE ALSO

**lnsd**(1), **lblogd**(1), **lncp**(1)
