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

**-v**, **--verbose**
:   Increase verbosity. Repeat for more detail.

**-q**, **--quiet**
:   Decrease verbosity.

**-l**, **--listen**
:   Listen for incoming transfer requests.

**-w** *seconds*
:   Sender timeout before giving up (default: 15).

**-s**, **--save** *dir*
:   Save received files in the specified directory.

**-O**, **--overwrite**
:   Allow overwriting existing files when receiving.

**-n**, **--no-auth**
:   Accept requests from anyone (no authentication).

**-b** *interval*
:   Announce interval in seconds. -1 = never, 0 = once at startup, N = every N seconds (default: 0).

**-p**, **--print-identity**
:   Print identity and destination info and exit.

**-i** *file*
:   Path to identity file to use.

**-S**, **--silent**
:   Disable transfer progress output.

**-C**, **--no-compress**
:   Disable automatic compression.

**-f**, **--fetch**
:   Fetch file from remote listener instead of pushing.

**-F**, **--allow-fetch**
:   Allow authenticated clients to fetch files.

**-j** *path*
:   Restrict fetch requests to the specified path.

## EXAMPLES

Send a file:

    lncp myfile.tar.gz a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4

Listen for incoming files and save to a directory:

    lncp -l -s ~/received/

Listen with verbose logging, accepting from anyone:

    lncp -l -n -v

## SEE ALSO

**lnsd**(1), **lnstest**(1), **lnstatus**(1)
