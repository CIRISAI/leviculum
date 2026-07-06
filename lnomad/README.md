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

### Options

- `--instance <name>`  shared-instance name (overrides the config file's)
- `--config <dir>`     Reticulum config directory (default: the platform default)
- `--no-color`         disable ANSI colour
- `--width <n>`        render width (default: detected terminal width, else 80)
- `--timeout <s>`      per-request timeout in seconds (default 30)
- `--print`            fetch, render and print once, then exit

When stdout is not a terminal, `lnomad` prints once and exits even without
`--print`, so piping and redirection never block on the prompt.

## Interactive commands

On a terminal, `lnomad` renders the page, lists its links as `[N] label ->
target`, and reads commands at the `>` prompt:

- `N`         follow link number `N`
- `b`         back to the previous page
- `r`         reload the current page
- `u <url>`   go to a new URL
- `h`         help
- `q` / EOF   quit

Same-destination links (`:/page/x.mu`) resolve against the page currently in
view. A followed link carries its preset (`f=v`) fields as `var_*` request
variables.

## v1 limits

- Interactive form-field input (fields the reader must type) is a stub: a link
  is followed with its preset fields only, and a note is printed when a link
  carries form fields.
- A `#anchor` in a target is resolved against the page's anchors and its
  position is annotated; a scrolling TUI is out of scope for v1.
- `/file/` downloads are not supported.
