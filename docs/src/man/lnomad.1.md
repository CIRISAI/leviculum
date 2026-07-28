# lnomad(1)

## NAME

lnomad -- terminal browser for NomadNet micron pages

## SYNOPSIS

**lnomad** [*options*] [*url*]
**lnomad** **--discover** [*seconds*]

## DESCRIPTION

**lnomad** fetches and renders NomadNet micron pages over Reticulum, either interactively in a terminal UI or once to standard output with **--print**. It connects to a running daemon (**lnsd** or **rnsd**) through the shared instance, so one of them must be running.

With **--discover** it does not fetch a page at all: it listens for `nomadnetwork.node` announces and lists the nodes it sees.

A `/file/` URL downloads instead of rendering; see **--output** for where the file lands.

## OPTIONS

*url*
:   Page to open, as `<dest_hash>[:/page/x.mu[`f=v|...]]`. A bare destination hash opens the node's default page. In **--discover** mode this positional is instead an optional listen duration in seconds, equivalent to **--duration**.

**--config** *dir*
:   Reticulum configuration directory. Defaults to the platform default, the same one **lncp**(1) uses.

**--instance** *name*
:   Shared-instance name to connect to, overriding the configuration file.

**--print**
:   Fetch, render and print the page once, then exit. Non-interactive.

**--discover**
:   Discover NomadNet nodes from announces instead of fetching a page.

**--duration** *seconds*
:   How long to listen in **--discover** mode.

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

## EXAMPLES

Open a node's default page interactively:

    lnomad a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4

Print a specific page without entering the UI:

    lnomad --print a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4:/page/index.mu

Listen for node announces for one minute:

    lnomad --discover 60

## SEE ALSO

**lnsd**(1), **lblogd**(1), **lncp**(1)
