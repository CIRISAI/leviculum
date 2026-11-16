# lnprobe(1)

## NAME

lnprobe -- Reticulum probe utility

## SYNOPSIS

**lnprobe** [*options*] *full_name* *destination_hash*

## DESCRIPTION

**lnprobe** measures the reachability of a Reticulum destination. It is compatible with Python's **rnprobe**: the same command line, the same output, the same exit codes. It connects to a running daemon (**lnsd** or **rnsd**) via shared instance IPC, requests a path to the destination if none is known, then sends probe packets and reports the round-trip time and hop count taken from the delivery proof the probed destination signs for each probe.

*full_name* is the destination's full dotted name (for probing a transport node's probe responder: `rnstransport.probe`); *destination_hash* is its 32-character hexadecimal hash. The probed node answers only if it runs a probe responder (`respond_to_probes` in its configuration, or an LNode's built-in responder — the hash is on the board's `[IDENTITY]` boot line and in the `lnflash --set-name` read-back).

## OPTIONS

**--config** *dir*
:   Path to alternative Reticulum configuration directory.

**-s**, **--size** *bytes*
:   Size of the probe packet payload in bytes. Default 16.

**-n**, **--probes** *count*
:   Number of probes to send. Default 1.

**-t**, **--timeout** *seconds*
:   Timeout before giving up, per probe and for the initial path request. Default 12 seconds plus the daemon's first-hop timeout for the destination, which scales with the next-hop interface's bitrate — a probe over a slow LoRa hop waits longer by default.

**-w**, **--wait** *seconds*
:   Time to wait between probes. Default 0.

**--no-start**
:   Fail when no shared instance answers, as the tools did before the zero-config step 4, instead of starting **lnsd**(1) as a child. Without it, **lnprobe** looks for a running shared instance (the abstract socket `@rns/`*instance*, then TCP loopback at the configured `shared_instance_port`, else 37428, for the instance `default` or a configured port) and, when none answers, starts `lnsd -c` *dir* on the same configuration directory and waits until it accepts. The daemon is **lnprobe**'s child and goes when **lnprobe** exits; its output goes to `lnsd.log` in the configuration directory. One line on standard error says which, `INSTANCE started=yes via=unix pid=`*pid* after a start, and with **-v** also `INSTANCE found=yes via=unix pid=none`. A configuration with `share_instance = No` starts nothing. The **lnsd** started is `$LEVICULUM_LNSD`, else the one beside **lnprobe**, else the one on `PATH`.

**-v**, **--verbose**
:   Show the next hop and interface for each probe; repeat to raise log verbosity.

## CONFIGURATION DIRECTORY

Without **--config**, **lnprobe** takes the first directory of the lookup order **rnsd** uses, stated once in the installation guide, section "Configuration directory" (`docs/src/guide/installation.md:104-117`); the order is what lets Python clients find the system daemon without flags. When **lnprobe** cannot reach or authenticate to the daemon, it names the directory it took and why, the socket it dialled, and every other candidate directory that holds a `config`, with the **--config** that selects it.

## EXIT STATUS

0 when every probe was answered. 1 when no path to the destination could be found. 2 when at least one probe went unanswered (the summary line reports the loss). 3 when the requested probe size does not fit the Reticulum MTU.

## EXAMPLES

Probe a transport node's probe responder:

    lnprobe rnstransport.probe 6a1ab9ea64747f298c1f205dfcf0f5a3

Send 10 probes of 100 bytes, one second apart:

    lnprobe -n 10 -s 100 -w 1 rnstransport.probe 6a1ab9ea64747f298c1f205dfcf0f5a3

## SEE ALSO

**lnsd**(1), **lnstest**(1), **lnstatus**(1), **lncp**(1), **lnpath**(1)
