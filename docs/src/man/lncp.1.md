# lncp(1)

## NAME

lncp -- Reticulum file transfer utility

## SYNOPSIS

**lncp** [*options*] *file* *destination*
**lncp** [*options*] **-l** [**-s** *dir*]

## DESCRIPTION

**lncp** transfers files over the Reticulum network. It is compatible with Python's **rncp**. It connects to a running daemon (**lnsd** or **rnsd**) via shared instance IPC.

In send mode, **lncp** sends *file* to the node identified by *destination* (a 32-character hex hash). In listen mode (**-l**), it waits for incoming file transfer requests.

## OPTIONS

*file*
:   File to send (send mode).

*destination*
:   Destination hash, 32 hex characters (send mode).

**--config** *dir*
:   Path to alternative Reticulum configuration directory.

**--no-start**
:   Fail when no shared instance answers, as the tools did before the zero-config step 4, instead of starting **lnsd**(1) as a child. Without it, **lncp** looks for a running shared instance (the abstract socket `@rns/`*instance*, then TCP loopback at the configured `shared_instance_port`, else 37428, for the instance `default` or a configured port) and, when none answers, starts `lnsd -c` *dir* on the same configuration directory and waits until it accepts. The daemon is **lncp**'s child and goes when **lncp** exits; its output goes to `lnsd.log` in the configuration directory. One line on standard error says which, `INSTANCE started=yes via=unix pid=`*pid* after a start, and with **-v** also `INSTANCE found=yes via=unix pid=none`. A configuration with `share_instance = No` starts nothing. The **lnsd** started is `$LEVICULUM_LNSD`, else the one beside **lncp**, else the one on `PATH`.

**-v**, **--verbose**
:   Increase verbosity. Repeat for more detail.

**-q**, **--quiet**
:   Decrease verbosity.

**-l**, **--listen**
:   Listen for incoming transfer requests.

**-w** *seconds*
:   Time out the fetch or transfer phase after this many seconds, counted from the moment the link is established (link establishment has its own internal budget). There is no timeout by default: slow transports such as LoRa need none, so set this only when you want a hard wall-clock bound.

**-s**, **--save** *dir*
:   Save received files in the specified directory.

**-O**, **--overwrite**
:   Allow overwriting existing files when receiving.

**-n**, **--no-auth**
:   Accept requests from anyone (no authentication).

**-a** *hash*
:   Allow a specific identity hash. May be given more than once.

**-b** *interval*
:   Announce interval in seconds. -1 = never, 0 = once at startup, N = every N seconds (default: 0).

**-p**, **--print-identity**
:   Print identity and destination info and exit.

**-i** *file*
:   Path to identity file to use.

**-S**, **--silent**
:   Fully silent: no progress output and no log output at all, equivalent to **-qq**.

**-C**, **--no-compress**
:   Disable automatic compression.

**-f**, **--fetch**
:   Fetch file from remote listener instead of pushing.

**-F**, **--allow-fetch**
:   Allow authenticated clients to fetch files.

**-j** *path*
:   Restrict fetch requests to the specified path.

**-P**, **--phy-rates**
:   Display physical layer transfer rates.

## CONFIGURATION DIRECTORY

Without **--config**, **lncp** takes the first directory of the lookup order **rnsd** uses, stated once in the installation guide, section "Configuration directory" (`docs/src/guide/installation.md:104-117`); the order is what lets Python clients find the system daemon without flags. When **lncp** cannot reach or authenticate to the daemon, it names the directory it took and why, the socket it dialled, and every other candidate directory that holds a `config`, with the **--config** that selects it.

## EXAMPLES

Send a file:

    lncp myfile.tar.gz a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4

Listen for incoming files and save to a directory:

    lncp -l -s ~/received/

Listen with verbose logging, accepting from anyone:

    lncp -l -n -v

## SEE ALSO

**lnsd**(1), **lnstest**(1), **lnstatus**(1)
