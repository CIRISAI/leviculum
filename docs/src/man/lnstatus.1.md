# lnstatus(1)

## NAME

lnstatus -- Reticulum network stack status

## SYNOPSIS

**lnstatus** [*options*] [*filter*]

## DESCRIPTION

**lnstatus** displays the status of the interfaces on a running Reticulum daemon. It is compatible with Python's **rnstatus** and produces the same per-interface layout. It connects to a running daemon (**lnsd** or **rnsd**) via shared instance IPC, querying `interface_stats` (and `link_count` for **-l**), so `lnstatus | diff rnstatus` against the same daemon passes.

Without a *filter*, all up interfaces are shown. Give a *filter* string to only display interfaces whose name contains it.

Only local shared-instance status is supported. Remote management (**-R**/**-i**/**-w**) and discovered interfaces (**-d**/**-D**) are accepted for compatibility but not yet implemented; requesting them prints a notice and exits non-zero.

## OPTIONS

*filter*
:   Only display interfaces whose name contains this string.

**--config** *dir*
:   Path to alternative Reticulum configuration directory.

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

The following options are accepted for **rnstatus** compatibility but are **not yet supported**; passing them prints a notice and exits non-zero:

**-R** *hash*
:   Transport identity hash of a remote instance to query (deferred).

**-i** *file*
:   Identity used for remote management (deferred).

**-w** *seconds*
:   Timeout before giving up on remote queries (deferred).

**-d**, **--discovered**
:   List discovered interfaces (deferred).

**-D**
:   Show details and config entries for discovered interfaces (deferred).

## EXIT STATUS

**0**
:   Success.

**1**
:   No shared RNS instance available to get status from (could not derive the RPC authkey).

**2**
:   The status query failed, or an unsupported option (**-R**/**-i**/**-w**/**-d**/**-D**) was requested.

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
