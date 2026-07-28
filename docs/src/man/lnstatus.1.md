# lnstatus(1)

## NAME

lnstatus -- Reticulum network stack status

## SYNOPSIS

**lnstatus** [*options*] [*filter*]

## DESCRIPTION

**lnstatus** displays the status of the interfaces on a running Reticulum daemon. It is compatible with Python's **rnstatus** and produces the same per-interface layout. It connects to a running daemon (**lnsd** or **rnsd**) via shared instance IPC, querying `interface_stats` (and `link_count` for **-l**), so `lnstatus | diff rnstatus` against the same daemon passes.

Without a *filter*, all up interfaces are shown. Give a *filter* string to only display interfaces whose name contains it.

With **-R** it queries a remote transport instance over a link, the way `rnstatus -R` does, and feeds the result to the same renderer, so remote and local output match. With **-d**/**-D** it reads the local discovered-interface registry over the RPC and renders the `rnstatus` discovered layout.

## OPTIONS

*filter*
:   Only display interfaces whose name contains this string.

**--config** *dir*
:   Path to alternative Reticulum configuration directory.

**--instance-name** *name*
:   Shared-instance name to query. Defaults to the value from the configuration file, otherwise `default`.

**-a**, **--all**
:   Show all interfaces, including those that are down.

**-A**, **--announce-stats**
:   Show announce statistics.

**-P**, **--pr-stats**
:   Show path request statistics.

**-B**, **--burst**
:   Only show interfaces with active bursts.

**-l**, **--link-stats**
:   Show link statistics (queries `link_count` from the daemon).

**-t**, **--totals**
:   Display traffic totals.

**-s**, **--sort** *key*
:   Sort interfaces by *key*: `rate`, `traffic`, `rx`, `tx`, `rxs`, `txs`, `announces`, `arx`, `atx`, `prx`, `ptx`, or `held`.

**-r**, **--reverse**
:   Reverse the sort order.

**-j**, **--json**
:   Output in JSON format.

**-m**, **--monitor**
:   Continuously monitor status, clearing and redrawing on each interval.

**-I**, **--monitor-interval** *seconds*
:   Refresh interval for monitor mode (default: 1).

**-v**, **--verbose**
:   Increase verbosity. Repeat for more detail.

**--version**
:   Print version and exit.

**-R** *hash*
:   Transport identity hash of a remote instance to query instead of the local one.

**-i** *file*
:   Identity used for remote management.

**-w** *seconds*
:   Timeout before giving up on remote queries.

**-d**, **--discovered**
:   List interfaces discovered on the network.

**-D**
:   Show details and config entries for discovered interfaces.

## EXIT STATUS

**0**
:   Success.

**1**
:   No shared RNS instance available to get status from (could not derive the RPC authkey).

**2**
:   The status query failed.

**20**
:   Remote management (**-R**) was requested but the management identity is missing or unusable.

## EXAMPLES

Show all interfaces:

    lnstatus

Show announce and path request statistics for interfaces named like `eth`:

    lnstatus -A -P eth

Sort interfaces by traffic, most first:

    lnstatus -s traffic -r

Continuously monitor, refreshing every two seconds:

    lnstatus -m -I 2

Emit machine-readable JSON:

    lnstatus -j

## SEE ALSO

**lnsd**(1), **lnstest**(1), **lncp**(1)
