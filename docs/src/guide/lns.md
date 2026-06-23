# lns

`lns` is the Reticulum command-line utility. It manages identities,
copies files, runs an interactive session against a daemon, exercises
the stack with a self-test, and collects diagnostic bundles.

```text
Reticulum command-line utility

Usage: lns [OPTIONS] <COMMAND>

Commands:
  status      Show status of the Reticulum network
  path        Show or request paths to destinations
  identity    Identity management
  probe       Probe a destination
  interfaces  Show interface information
  selftest    Run integration self-test through relay node(s)
  cp          Copy files over Reticulum (compatible with rncp)
  connect     Interactive session: connect to rnsd and enter command loop
  diag        Collect a diagnostic bundle from a running lnsd (or rnsd) for bug reports
  help        Print this message or the help of the given subcommand(s)

Options:
  -c, --config <CONFIG>                Configuration file path
  -v, --verbose                        Enable verbose logging
      --corrupt-every <CORRUPT_EVERY>  Corrupt ~1 byte per N bytes on TCP write (fault injection)
  -h, --help                           Print help
  -V, --version                        Print version
```

`-c/--config`, `-v/--verbose`, and `--corrupt-every` are global flags
available on every subcommand. `--corrupt-every` is a fault-injection
tool for testing and should be left off in normal use.

## identity

Manage Reticulum identities. An identity file holds the 64-byte private
key (X25519 + Ed25519); the public half and the 16-byte hash are derived
from it.

```text
Usage: lns identity <COMMAND>

Commands:
  generate  Generate a new identity
  show      Show identity information
```

### identity generate

Creates a fresh identity. With `-o/--output FILE` the private key is
written to the file and the hash is printed; without it, the hash and
public key are printed and nothing is saved (`lns.rs:1126-1145`).

```sh
lns identity generate -o my-identity.bin
```

```text
Generated new identity
Hash: 0123456789abcdef0123456789abcdef
Saved to: my-identity.bin
```

Without `-o`, the public key is printed instead of being saved
(`lns.rs:1140-1144`):

```sh
lns identity generate
```

```text
Generated new identity
Hash: 0123456789abcdef0123456789abcdef
Public key: <64 hex bytes>
```

### identity show

Loads a saved identity file and prints its path, hash, and public key
(`lns.rs:1147-1158`):

```sh
lns identity show my-identity.bin
```

```text
Identity: my-identity.bin
Hash: 0123456789abcdef0123456789abcdef
Public key: <64 hex bytes>
```

## cp

Copy files over Reticulum, wire-compatible with Python's `rncp`. This is
the same engine as the standalone [`lncp`](lncp.md) tool, exposed as an
`lns` subcommand.

Because `-v/--verbose` and `-c/--config` are global `lns` flags, `cp`
uses `-V/--cp-verbose` and `--cp-config` for its own verbosity and
config overrides (`rncp` itself uses `-v` and has no `--config`). Other
options match `lncp`: `-l/--listen`, `-w TIMEOUT`, `-s/--save`,
`-O/--overwrite`, `-n/--no-auth`, `-b ANNOUNCE_INTERVAL`. See the
[`lncp`](lncp.md) page for the full option set and worked examples.

```sh
# Send a file
lns cp report.pdf 0123456789abcdef0123456789abcdef

# Listen for incoming transfers
lns cp -l
```

## connect

Open an interactive session against a running daemon (`lnsd` or `rnsd`)
and enter a command loop. The address is the daemon's TCP interface
(`host:port`); `lns` verifies TCP connectivity before building the node
(`lns.rs:554-560`).

```text
Usage: lns connect [OPTIONS] <ADDR>

Arguments:
  <ADDR>  Address of the rnsd to connect to (host:port)

Options:
  -c, --config <CONFIG>      Configuration file path
      --identity <IDENTITY>  Path to identity file (default: generate ephemeral)
```

With no `--identity`, an ephemeral identity is generated for the session
(`lns.rs:564-573`). On connect, the session announces itself and prints
its identity and destination hashes (`lns.rs:616-619`):

```text
Identity: <hash>
Destination: <hash>
Announced as lns-cli
Type /help for commands.
>
```

### Interactive commands

The command loop accepts these (`lns.rs:534-546`):

| Command | Action |
|---------|--------|
| `/peers` | List discovered destinations |
| `/link <hash>` | Initiate a link to a destination (32-char hex) |
| `/target <hash>` | Set a single-packet destination (32-char hex) |
| `/untarget` | Clear the single-packet target |
| `/send <msg>` | Send data on the active link or to the target |
| `/close` | Close the active link |
| `/announce` | Re-announce this destination |
| `/quiet` | Hide announce/path messages |
| `/verbose` | Show announce/path messages |
| `/status` | Show node status (identity, destination, paths, peers) |
| `/help` | Show this help |
| `/quit` | Exit |
| `<bare text>` | Send as data on the active link or to the target |

## selftest

Run an end-to-end self-test through one or two relay nodes you control.
The relay addresses are given as `host:port`.

```text
Usage: lns selftest [OPTIONS] [TARGETS]...

Arguments:
  [TARGETS]...  Address(es) of relay node(s) (host:port). One or two addresses

Options:
  -c, --config <CONFIG>
          Configuration file path
      --duration <DURATION>
          Test duration in seconds [default: 180]
      --rate <RATE>
          Messages per second per direction [default: 1]
      --mode <MODE>
          Which test phases to run [default: all]
      --discovery-timeout <DISCOVERY_TIMEOUT>
          Discovery timeout in seconds (Phase 2: mutual path discovery) [default: 60]
```

The `--mode` flag selects which phases run; the values are `all`,
`link`, `packet`, `ratchet-basic`, `ratchet-enforced`,
`bulk-transfer`, and `ratchet-rotation` (default `all`). `--duration`
defaults to 180 seconds, `--rate` to 1 message per second per
direction, and `--discovery-timeout` to 60 seconds.

```sh
# Full self-test through one relay
lns selftest 192.0.2.10:4965

# Just the link phase, two relays, shorter run
lns selftest --mode link --duration 60 192.0.2.10:4965 192.0.2.11:4965
```

## diag

Collect a self-contained diagnostic bundle from a running daemon for bug
reports. `diag` queries the shared-instance RPC for the daemon's live
view (interface stats, path table, link count), bundles it with the
secret-redacted config, version and build info, and system info, and
prints to stdout (or to a file with `--output`).

```text
Usage: lns diag [OPTIONS]

Options:
  -c, --config <CONFIG>
          Configuration file path
      --output <OUTPUT>
          Write the bundle to this path instead of stdout
      --instance-name <INSTANCE_NAME>
          Shared-instance name to query (default: from config, else "default")
      --event-log <EVENT_LOG>
          Tail this structured event-log file into the bundle
      --no-rpc
          Skip the daemon RPC queries; emit only config / versions / system
```

`--no-rpc` (`lns.rs:491-493`) skips the daemon queries — useful for
checking config parse status when the daemon is down.

A bundle is assembled from these sections in order (`diag.rs:63-190`):

- **Versions / build** — `lns` version, build profile, target. (The
  daemon version is not exposed by the RPC; check the daemon's startup
  log if needed.)
- **Config** — config dir and file, parse status, then the *effective*
  config rendered as TOML with secrets redacted. The raw file is never
  included.
- **Interfaces (configured)** — each configured interface from the
  parsed config.
- **Daemon view (shared-instance RPC)** — instance name, RPC socket
  path `\0rns/<name>/rpc`, and live `interface_stats`, `path_table`,
  and `link_count` queries.
- **System** — OS, kernel, distro, and the daemon's pid / RSS / open
  fds.
- **Recent events** — tail of the structured event log if
  `--event-log` is given.

A trimmed bundle (your transport id, paths, and counters will differ):

```text
===== Leviculum diagnostic bundle =====

----- Versions / build -----
lns version: 0.7.0
build profile: release
target: x86_64 / linux
daemon version: not exposed by the shared-instance RPC ...

----- Config -----
config dir:  /etc/reticulum
config file: /etc/reticulum/config
config file: present, parsed OK

Effective config (TOML, secrets redacted; the raw file is NOT included
because it may contain secrets):
[reticulum]
enable_transport = true
shared_instance = true
instance_name = "default"
...

----- Daemon view (shared-instance RPC) -----
instance name: default
RPC socket:    \0rns/default/rpc
authkey:       derived from /etc/reticulum/storage/transport_identity (not shown)

## interface_stats
transport id: 0123456789abcdef0123456789abcdef
daemon uptime: 12m 34s (754s)
interfaces (3):
  - Shared Instance[rns/default]  type=LocalServerInterface status=up rxb=0 txb=0 clients=1
  - AutoInterface[Default Interface/eth0/aabbccdd]  type=AutoInterface status=up rxb=482 txb=917 peers=2
  - TCPInterface[RNS TCP Node Germany 002/193.26.158.230:4965]  type=TCPClientInterface status=up rxb=14211 txb=8332

## path_table
known paths: 7
[ ... JSON array of {hash, via, hops, expires, ...} ... ]

## link_count
active links: 0

----- System -----
os: linux  kernel: 6.12.73+deb13-amd64
distro: Debian GNU/Linux 13 (trixie)
lnsd pid: 12345
lnsd VmRSS: 18432 kB
lnsd open fds: 27

----- Recent events -----
No structured event-log file specified ...

===== end of diagnostic bundle =====
```

### Secret redaction

The bundle is designed to be safe to attach to a public tracker. IFAC
`passphrase` and `networkname` are redacted before the config is
serialised, and the node's private key is never read into the bundle —
only its hash is used internally to derive the RPC authkey
(`diag.rs:119-128`, `162-168`). The bundle still contains your
hostnames, configured TCP targets, byte counters, and known-paths table,
so review it once before posting if your topology is sensitive.

```sh
lns diag --config /etc/reticulum --output /tmp/lns-diag.txt
```

See the [lnsd Quickstart](../lnsd-quickstart.md) for how to read the
bundle as a health check.

## Planned commands

`status`, `path`, `probe`, and `interfaces` are present as placeholders
but are **not implemented yet** — they print a "Not implemented yet"
notice and exit 0 (`lns.rs:1104-1174`). They are tracked as Codeberg
issue #22. Until they land:

- For status, use `lns diag` or the Python `rnstatus`.
- For paths and interfaces, use the `interface_stats` and `path_table`
  sections of `lns diag`, or `rnpath` / `rnstatus`.
- For probing a destination, use the Python `rnprobe`.

```sh
lns status
```

```text
Reticulum Status
================

Status: Not implemented yet
```
