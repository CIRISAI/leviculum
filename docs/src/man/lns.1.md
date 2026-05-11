# lns(1)

## NAME

lns -- Reticulum network utility

## SYNOPSIS

**lns** [**-c** *dir*] [**-v**] *command* [*args*...]

## DESCRIPTION

**lns** is a multi-tool for interacting with a running Reticulum network. It combines the functionality of Python's **rnstatus**, **rnpath**, **rnprobe**, and more into a single binary.

## GLOBAL OPTIONS

**-c**, **--config** *dir*
:   Path to the Reticulum configuration directory.

**-v**, **--verbose**
:   Enable verbose logging.

## COMMANDS

### lns status

Show status of the Reticulum network. Connects to the running daemon via shared instance and displays transport information. Equivalent to Python's **rnstatus**.

### lns path [*destination*]

Show or request paths to destinations. Without an argument, lists all known paths. With a destination hash (hex), requests a path to that destination. Equivalent to Python's **rnpath**.

### lns probe *destination*

Probe a destination by sending a probe packet and measuring round-trip time. Equivalent to Python's **rnprobe**.

### lns interfaces

Show information about configured network interfaces.

### lns identity generate [**-o** *file*]

Generate a new Reticulum identity and write it to *file*.

### lns identity show *file*

Show the hash and public keys of the identity in *file*.

### lns cp [*options*] [*file*] [*destination*]

Copy files over Reticulum, compatible with Python's **rncp**. See **lncp**(1) for the full option reference.

### lns connect *addr*

Open an interactive session to a Reticulum daemon at *addr* (host:port). Supports link establishment, message exchange, and announce discovery. Type `/help` in the session for available commands.

### lns selftest *target* [*target*]

Run integration self-tests through one or two relay nodes. Tests link establishment, channel data, ratchet operation, and bulk transfer.

Options:

**--duration** *seconds*
:   Test duration (default: 180).

**--rate** *n*
:   Messages per second per direction (default: 1).

**--mode** *mode*
:   Which phases to run: all, link, packet, ratchet-basic, ratchet-enforced, bulk-transfer, ratchet-rotation (default: all).

### lns diag

Collect a self-contained diagnostic bundle from a running **lnsd** (or **rnsd**) for attaching to bug reports: versions/build, the secret-redacted config and configured interfaces, the daemon's live view via the shared-instance RPC (interface stats, path table, link count), best-effort system info, and an event-log pointer. Printed to stdout by default. Use the global **-c**/**--config** to point at the daemon's config directory.

Secrets are redacted — IFAC `passphrase` and `networkname` never appear, and the node identity private key (`storage/transport_identity`) is never read into the bundle (it is used only to derive the RPC authkey). Queries the daemon doesn't support (e.g. when run against Python **rnsd**) are reported as unavailable rather than failing.

Options:

**--output** *path*
:   Write the bundle to *path* instead of stdout (a one-line confirmation is printed to stderr).

**--instance-name** *name*
:   Shared-instance name to query (default: from the config, else `default`).

**--event-log** *path*
:   Tail this structured event-log file into the bundle (when **lnsd** was started with `LEVICULUM_EVENT_LOG` set).

**--no-rpc**
:   Skip the daemon RPC queries; emit only the config, versions, and system sections.

## EXAMPLES

Show network status:

    lns status

Request a path:

    lns path a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4

Probe a destination:

    lns probe a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4

Generate a new identity:

    lns identity generate -o my_identity

## SEE ALSO

**lnsd**(1), **lncp**(1)
