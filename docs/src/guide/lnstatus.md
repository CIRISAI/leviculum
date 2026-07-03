# lnstatus

`lnstatus` is the Reticulum status tool. It shows the interfaces of a
running daemon and their traffic, announce, path-request, and link
statistics, and is output-compatible with Python's `rnstatus` — fed the
same `interface_stats` from the same daemon, `lnstatus` and `rnstatus`
render byte-identical output, so `lnstatus | diff rnstatus` passes.

```text
Reticulum Network Stack Status

Usage: lnstatus [OPTIONS] [FILTER]

Arguments:
  [FILTER]  Only display interfaces with names including filter

Options:
      --config <CONFIG>                  Path to alternative Reticulum config directory
  -a, --all                              Show all interfaces
  -A, --announce-stats                   Show announce stats
  -P, --pr-stats                         Show path request stats
  -l, --link-stats                       Show link stats
  -B, --burst                            Only show interfaces with active bursts
  -t, --totals                           Display traffic totals
  -s, --sort <SORT>                      Sort interfaces by [rate, traffic, rx, tx, rxs, txs, announces, arx, atx, prx, ptx, held]
  -r, --reverse                          Reverse sorting
  -j, --json                             Output in JSON format
  -m, --monitor                          Continuously monitor status
  -I, --monitor-interval <INTERVAL>      Refresh interval for monitor mode [default: 1]
  -v, --verbose...                       Increase verbosity
  -h, --help                             Print help
  -V, --version                          Print version
```

`lnstatus` needs a running daemon (`lnsd` or `rnsd`) on the same shared
instance to query — see the
[lnsd Quickstart](../lnsd-quickstart.md).

## Running it against a daemon

With `lnsd` (or Python `rnsd`) running, `lnstatus` with no arguments
prints every up interface and its counters:

```sh
lnstatus
```

It resolves the daemon exactly like the other tools: the config
directory (default lookup, or `--config <DIR>`) gives the shared-instance
name and the RPC authkey. If no shared instance is reachable, it reports
`No shared RNS instance available to get status from` and exits non-zero.

Give a `FILTER` to restrict the output to interfaces whose name contains
it:

```sh
lnstatus eth
```

## Common flags

### Extra statistics

`-A/--announce-stats` and `-P/--pr-stats` add announce and path-request
columns; `-l/--link-stats` adds link counts (queried separately from the
daemon); `-t/--totals` appends traffic totals:

```sh
lnstatus -A -P -l -t
```

`-a/--all` also shows interfaces that are currently down, and
`-B/--burst` restricts the output to interfaces with active bursts.

### Sorting

`-s/--sort <KEY>` orders the interfaces by one of `rate`, `traffic`,
`rx`, `tx`, `rxs`, `txs`, `announces`, `arx`, `atx`, `prx`, `ptx`, or
`held`; `-r/--reverse` flips the order:

```sh
lnstatus -s traffic -r
```

### Monitor mode

`-m/--monitor` clears the screen and re-renders on each interval;
`-I/--monitor-interval <SECONDS>` sets the refresh period (default `1`):

```sh
lnstatus -m -I 2
```

### JSON output

`-j/--json` emits the status as JSON instead of the rendered table, for
scripting:

```sh
lnstatus -j
```

## Not yet supported

The remote-management flags `-R/-i/-w` and the discovered-interface flags
`-d/-D` are accepted for `rnstatus` compatibility but not yet
implemented. Passing any of them prints a clear notice and exits
non-zero rather than silently doing nothing.

## Examples

### Full picture of a local daemon

```sh
lnstatus -a -A -P -l -t
```

### Watch one interface live

```sh
lnstatus -m -I 2 rnode
```

`lnstatus` needs a running daemon (`lnsd` or `rnsd`) on the same shared
instance to reach the mesh — see the
[lnsd Quickstart](../lnsd-quickstart.md).
