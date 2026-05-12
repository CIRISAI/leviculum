# lnsd Quickstart for Beta Testers

This page gets you from "I have the `.deb` file" to "my node is on the
mesh and I know how to tell if it isn't", plus the one-liner you run when
something is off so the bug report has everything we need.

For the protocol itself, the upstream [Reticulum Manual](https://reticulum.network/manual/)
is the reference. This page is about getting `lnsd` running on your
machine.

## Prerequisites

- Linux, x86_64 or aarch64. (macOS and embedded targets exist but are
  out of scope for the beta `.deb` path.)
- The nightly `.deb` for your architecture. Download links are on the
  [releases page](https://codeberg.org/Lew_Palm/leviculum/releases).
  The binaries inside are statically linked against musl, so the
  package installs on Debian ≥ 9 and Ubuntu ≥ 16.04 regardless of host
  glibc.
- A few free TCP/UDP ports on your machine for the configured
  interfaces (default ports below).

You do **not** need to install Rust, Python, or Docker for the beta
flow.

## Install

```sh
sudo apt install ./leviculum-nightly-amd64.deb       # or -arm64
```

The package:

- Installs `lnsd`, `lns`, and `lncp` under `/usr/bin/`.
- Creates a system user `leviculum` and a group of the same name.
- Drops a default config at `/etc/reticulum/config` (mode 2775,
  group-writable + setgid, so everything created inside it inherits
  the group).
- Enables and starts the `lnsd.service` systemd unit.

For the native tools (`lns`, `lncp`) and Python tools (`rnstatus`,
`rnpath`, `rnprobe`, Sideband, Nomadnet, …) to talk to the running
daemon, your user has to be in the `leviculum` group:

```sh
sudo usermod -aG leviculum "$USER"
# log out and back in, or `newgrp leviculum` for this shell only
```

Verify the installation:

```sh
lnsd --version          # e.g. 0.6.3-nightly.20260419-5a5df20
lns  --version
systemctl is-active lnsd
```

`is-active` should print `active`. If it prints `failed`, jump to
[Troubleshooting](#troubleshooting).

## Minimum-viable config

The default `/etc/reticulum/config` is conservative: it brings up a
single `AutoInterface` for local LAN peers, with transport routing
disabled. That's enough to talk to other Reticulum nodes on the same
LAN, but it does not connect you to the wider mesh.

A reasonable beta-tester config has two interfaces: one for the LAN, one
TCP uplink to a public entrypoint. Edit `/etc/reticulum/config` to:

```ini
[reticulum]
  # Pass announces and serve paths for other peers. Leave off if your
  # machine is mobile or sleeps a lot.
  enable_transport = Yes

  # Required for `lns diag`, `rnstatus`, Sideband etc. to attach to
  # this daemon. The default config already sets this.
  share_instance = Yes

[interfaces]

  # 1. Local mesh: discovers and talks to every other Reticulum node
  # on the same broadcast domain. No router/DHCP needed. Multicast
  # has to reach the link (most home LANs do; corporate Wi-Fi often
  # does not).
  [[Default Interface]]
    type = AutoInterface
    enabled = Yes

  # 2. TCP uplink to a public entrypoint. Pick a node from the
  # community directory: https://directory.rns.recipes/  (entrypoints
  # rotate; for redundancy add two or three, and see the Reticulum
  # manual's "Bootstrapping Connectivity" section for the
  # discover_interfaces auto-peering option). Example below: the
  # RNS TCP Node Germany 002 entry.
  [[RNS TCP Node Germany 002]]
    type = TCPClientInterface
    enabled = Yes
    target_host = 193.26.158.230
    target_port = 4965
```

Then restart the daemon so it picks up the new config:

```sh
sudo systemctl restart lnsd
```

`lns diag` (below) is the easiest way to confirm both interfaces came
up.

## Start the daemon

The systemd unit handles this for you on install. The relevant commands:

```sh
sudo systemctl start lnsd      # or restart
sudo systemctl stop lnsd
sudo systemctl status lnsd
journalctl -u lnsd -f          # live log tail
journalctl -u lnsd --since '10 min ago'
```

Logs go to the journal. Increase verbosity by editing the unit's
`ExecStart` to add `-v` (debug) or `-vv` (trace), then
`sudo systemctl daemon-reload && sudo systemctl restart lnsd`. The
`RUST_LOG` environment variable also works (see `lnsd(1)`).

To run `lnsd` by hand without systemd (useful for ad-hoc debugging):

```sh
sudo systemctl stop lnsd
sudo -u leviculum /usr/bin/lnsd -v --config /etc/reticulum
```

## Check it's working

Three commands. Run them as a user that is in the `leviculum` group.

### 1. `lns diag`

This is the main health-check. It connects to the running daemon over
the shared-instance socket and renders a single-file diagnostic bundle:

```sh
lns diag --config /etc/reticulum
```

A healthy bundle looks roughly like this (your `transport id`, paths,
and byte counters will differ):

```
===== Leviculum diagnostic bundle =====

----- Versions / build -----
lns version: 0.6.3
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

What to look at first:

- **`status=up` on every interface** in the `interface_stats` section.
  An interface that came up but lost its medium reports `status=down`.
- **Non-zero `rxb` / `txb`** on the interfaces you expect traffic on
  (`AutoInterface` once any other Reticulum node is on the same LAN,
  `TCPInterface` as soon as it connects).
- **`peers=…`** on the `AutoInterface` line: how many other Reticulum
  nodes are visible on the LAN.
- **`known paths: N`** with N > 0 once announces have crossed the
  mesh. Brand-new daemons that haven't heard any announces yet show
  `known paths: 0` for the first few seconds — that's normal.
- **`transport id`** is your node's identity (the public half). It is
  safe to share; the private half lives in
  `/etc/reticulum/storage/transport_identity` and is never included
  in `lns diag` output.

### 2. `lns selftest --help`

Sanity-checks that `lns` itself is installed and runnable:

```sh
lns selftest --help
```

The actual `lns selftest` exercise needs one or two relay nodes you
control. The full command and options live in `lns(1)`.

### 3. `rnstatus` (optional — Python tools)

The `.deb` does **not** install Python Reticulum. If you want `rnstatus`
/ `rnpath` / `rnprobe` / Sideband:

```sh
sudo apt install python3 python3-pip
pip3 install --user rns
rnstatus
```

Python tools auto-detect `/etc/reticulum/config` and connect to the
running `lnsd` through the same shared-instance socket. No extra flags
are needed. (`lns status` exists as a placeholder but is not implemented
yet — use `rnstatus` or `lns diag` until it lands.)

## Connect to the wider mesh

With the config above, two things happen as soon as `lnsd` starts:

1. **Announcing.** Your node sends an announce for its probe destination
   on every enabled interface. Other transport-enabled nodes pass
   that announce on, so within seconds your node is visible to peers
   on the LAN and within a few minutes to peers reachable through
   the TCP uplink.
2. **Learning paths.** When other nodes announce, your daemon stores
   a path to each announced destination (hash, next hop, hop count,
   expiry). `lns diag`'s `known paths: N` is that table's size.

When you want to talk to a specific destination (e.g. send a file with
`lncp`), the daemon either has a path already (immediate) or requests
one (a path-request packet, a few seconds, then immediate). You don't
have to do anything to make path discovery happen — it runs whenever
the daemon is up.

For the protocol-level picture, read [Bootstrapping
Connectivity](https://reticulum.network/manual/gettingstartedfast.html#bootstrapping-connectivity)
in the upstream Reticulum manual.

## Troubleshooting

### lnsd will not start

```sh
systemctl status lnsd
journalctl -u lnsd --since '10 min ago' | tail -50
```

Common causes:

- **Config not parsed.** Look for a "Failed to parse config" line in
  the journal. `lns diag --no-rpc` shows the parse status without
  needing the daemon up:
  ```
  config file: present but FAILED to parse: <details>
  ```
- **Abstract socket already in use.** Another `lnsd` or `rnsd` is
  running under the same `instance_name`. Stop it
  (`sudo systemctl stop lnsd` then `pkill -f rnsd` if applicable),
  or set a unique `instance_name` in your config.
- **Permission on the storage directory.** The `leviculum` user has
  to be able to write `/etc/reticulum/storage/`. The `.deb` sets the
  permissions correctly on install; a manual `chown` to `root:root`
  breaks the daemon. Fix:
  ```sh
  sudo chown -R leviculum:leviculum /etc/reticulum
  sudo chmod 2775 /etc/reticulum
  ```

### No peers found / `known paths: 0`

Check `lns diag`'s `interface_stats` section:

- **`AutoInterface` shows `peers: 0` and `rxb: 0`** — multicast isn't
  reaching the link. Likely causes: corporate Wi-Fi (multicast blocked);
  a Linux bridge or container network without multicast forwarding;
  no other Reticulum node on the segment.
- **`TCPClientInterface` shows `status=up` but `rxb: 0`** — TCP
  connected but the remote isn't sending anything, which usually means
  the remote is up but has no transport peers itself, or the
  entrypoint has been retired. Try a different entrypoint, or rely on
  AutoInterface + a TCP uplink to a known-good node you control.
- **`TCPClientInterface` not listed at all** — the daemon hasn't
  connected yet (look for `Establishing TCP connection` lines in
  `journalctl -u lnsd`) or DNS for the target host doesn't resolve.

Give it ~30 seconds after starting `lnsd` before concluding there's a
problem — the first round of announces and the initial TCP connect
take a moment.

### Native and Python tools cannot reach lnsd

Symptom: `lns diag` shows `<unavailable: …>` in the daemon-view
section, or `rnstatus` errors with "Reticulum is not running".

- Confirm your user is in the `leviculum` group:
  ```sh
  id | tr , '\n' | grep leviculum
  ```
  If not, `sudo usermod -aG leviculum "$USER"` and log out / back in.
- Confirm the daemon really is up and has `share_instance = Yes`:
  ```sh
  systemctl is-active lnsd
  grep -i share_instance /etc/reticulum/config
  ```
- Confirm both client and daemon are using the same config directory.
  The client defaults to `/etc/reticulum` if it exists, then
  `~/.config/reticulum`, then `~/.reticulum`. `lns diag --config
  /etc/reticulum` is explicit.

### Submitting a bug report

Run `lns diag` and attach its output to your report:

```sh
lns diag --config /etc/reticulum --output /tmp/lns-diag.txt
```

The bundle is plain UTF-8 text, designed to be safe to attach: IFAC
`passphrase` and `networkname` are redacted before serialisation; the
node identity private key is never read into the bundle (only its
SHA-256 is used, internally, to derive the shared-instance RPC
authkey). The bundle does contain your node's hostnames, configured
TCP targets, byte counters, and known-destinations table — review it
once before posting to a public tracker if your topology is sensitive.

If `lnsd` is in a structured event-log run
(`LEVICULUM_EVENT_LOG=/var/log/lnsd-events.log` in the service unit's
`Environment=`), include the tail of that file too:

```sh
lns diag --event-log /var/log/lnsd-events.log \
         --output /tmp/lns-diag.txt
```

Otherwise the bundle already points the reviewer at `journalctl -u
lnsd`, which is enough.

## See also

- `lnsd(1)`, `lns(1)`, `lncp(1)` man pages.
- [Configuration](guide/configuration.md) for the format reference.
- [Installation](guide/installation.md) for the source-build path.
- The upstream [Reticulum Manual](https://reticulum.network/manual/)
  for the protocol itself.
