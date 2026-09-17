# lnpath(1)

## NAME

lnpath -- Reticulum path query utility

## SYNOPSIS

**lnpath** [*options*] *destination_hash*

## DESCRIPTION

**lnpath** queries the path to a Reticulum destination, waits for it to arrive, and drops one a running daemon holds. It connects to a running daemon (**lnsd** or **rnsd**) via shared instance IPC. Without **-d** it requests a path if none is known, waits out the **-w** window, and reports the hop count, the next hop and the interface the traffic leaves on. With **-d** it removes the path from the daemon's table, which is the table that routes; the client's own copy dies with the process and dropping it would change nothing.

The tool implements the path-query verb of Python's **rnpath** -- query, wait, drop -- with the reference tool's arguments, output and exit codes for those three. It deliberately offers no flag for the reference tool's other roles: the path and rate *views* (`-t`, `-r`, `-m`), the blackhole administration verbs (`-b`, `-B`, `-U`, `-p`), and remote management of another instance (`-R`, `-i`, `-W`). Use `rnpath` for those, or `lnstatus --tables`, which prints the same path table `rnpath -t` shows plus the tables the reference tool cannot reach.

## OPTIONS

**--config** *dir*
:   Path to alternative Reticulum configuration directory.

**-d**, **--drop**
:   Remove the path to the destination from the daemon's path table.

**-w** *seconds*
:   Timeout before giving up on the path request. Default 15 seconds, the reference stack's `PATH_REQUEST_TIMEOUT`. This argument governs the whole wait: nothing is added to it and no floor is applied, so a caller that asks for two seconds waits two seconds and one that asks for sixty waits sixty.

**-v**, **--verbose**
:   Raise log verbosity. Diagnostics go to standard error; standard output carries only the verdict line.

## EXIT STATUS

0 when a path was found, or when one was dropped. 1 when no path was found within the window, when the destination argument is not a 32-character hexadecimal hash, when there was no path to drop, or when the daemon could not be reached.

## EXAMPLES

Query a path, waiting up to the default 15 seconds:

    lnpath 6a1ab9ea64747f298c1f205dfcf0f5a3

Wait a minute for a path over a slow LoRa hop:

    lnpath -w 60 6a1ab9ea64747f298c1f205dfcf0f5a3

Drop a path so the next request rediscovers it:

    lnpath -d 6a1ab9ea64747f298c1f205dfcf0f5a3

## SEE ALSO

**lnsd**(1), **lnstest**(1), **lnstatus**(1), **lncp**(1), **lnprobe**(1)
