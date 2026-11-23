# Leviculum

Leviculum is an independent reimplementation based on the [Reticulum protocol specification](https://leviculum.network/docs/appendix/reticulum-specification.html).
It is wire-compatible with Reticulum and runs on Linux, macOS, and embedded devices.

## Bugs and contributions

Report bugs and ask questions at
<https://codeberg.org/Lew_Palm/leviculum/issues>. A question is an
ordinary issue; the tracker has no `question` label, so leave the labels
to the maintainers. When the daemon is
involved, attach the output of `lnstest diag` (see the
[quickstart](https://leviculum.network/docs/lnsd-quickstart.html) for the exact command). For
patches, see [CONTRIBUTING.md](CONTRIBUTING.md).

A suspected security vulnerability is the one thing that does not belong in
the tracker: an issue is public from the moment it is filed, which hands the
flaw to everyone who might use it. [SECURITY.md](SECURITY.md) names the
private contact, the acknowledgement window, and what is in scope.

## What is Reticulum?

Reticulum is a networking stack for building resilient, encrypted mesh networks over any transport medium. It works over LoRa radios, TCP, UDP, serial links, or anything that can carry bytes. Every node gets a cryptographic identity. Every connection is end-to-end encrypted. No servers, no accounts, no infrastructure required.

## What does Leviculum do?

Leviculum provides the same functionality as Python Reticulum but compiled to native code. The `lnsd` daemon is a drop-in replacement for `rnsd`, `lncp` replaces `rncp`, `lnstatus` replaces `rnstatus`, `lnprobe` replaces `rnprobe`, and `lnpath` covers `rnpath`'s path query, wait and drop. Python CLI tools like `rnstatus`, `rnpath`, and `rnprobe` also work against a running `lnsd` without modification.

The protocol core (`leviculum-core`) compiles as `no_std` with only `alloc`, so it runs on microcontrollers. The same code powers the Linux daemon, a future Android app, and embedded firmware.

## This fork: CIRISAI/leviculum (`+ciris`)

This is the CIRIS fork of [Leviculum](https://codeberg.org/Lew_Palm/leviculum). Releases are tagged `vX.Y.Z+ciris.N`; every fork-only change is marked `+ciris` in its doc comment, so it can be traced and offered back upstream. The main addition is **direct links: NAT traversal for Reticulum links**. Two nodes that share a Reticulum link (over a TCP or UDP transport node, or any relay) can move it onto a direct UDP path between them. Once it has moved, its traffic no longer crosses the relay; if the direct path dies, the link falls back to the relayed path.

- **Wire-compatible with rns-rs through rns-net v0.13.0** direct links (leviculum#70): UDP hole punching coordinated over the existing link, with a facilitator probe for each side's public mapping. The fork's additions are extra keys an rns-rs decoder skips. (rns-rs changed its wire in v0.14; that later wire is not implemented.)
- **Two-sided commit** (leviculum#89): each side tells the other how its punch went, and the link moves only when both succeeded.
- **Pair agreement by nomination** (two-sided commit peers, `"n"`): each punch names the target it was sent to, so the initiator counts only an ack for a punch it sent to that very address; it then nominates the pair it settled on with a flagged punch on it, and the responder settles on exactly that pair (RFC 8445's controlling agent). Two ends can no longer commit to different pairs and drop each other's frames. Peers without the commit settle each on its own, as before.
- **NAT classification from any two peers** (#100), and **router port mapping** (UPnP-IGD / NAT-PMP / PCP) on by default (#101), from v0.35.0+ciris.2.
- **Own facilitator only.** Each side probes the facilitator it was configured with; a responder never probes one only the peer named, and with none of its own it declines and the link stays relayed.
- **NAT classification** from the session socket's mapping toward two observers: the facilitator and a second observer (`probe_addr2`; a facilitator can answer on its next UDP port too, `probe_second_port`). Two ports on one IP detect a port-dependent mapping, not an address-only one; observers of different address families are never compared. With no second observer that answers, a node falls back on its own evidence: its allocation probes look random and the punch has heard nothing for 1.5 s.
- **Port prediction** for endpoint-dependent NATs that allocate ports in sequence (two-observer evidence only): the side behind it measures its allocation step and advertises the next 16 ports it expects to use (never below 1024).
- **Birthday spray** for endpoint-dependent NATs that allocate ports at random, at HyperDHT's caps. The random side opens up to 256 sockets toward the peer's stable address, in tranches: 64 once none of its own punches has been acked for 1 s (two-observer evidence) or 1.5 s (own evidence), heard or not, then 64 more every 500 ms while still nothing is acked. It stops at the first hit and closes every socket but the winner at once; a bind or send that fails (a carrier NAT's per-subscriber port quota, a host out of sockets) stops the ramp where it is. Each socket sends one punch every 500 ms. The whole process holds at most 512 birthday sockets (two sessions' full sets); a third concurrent session gets what is left and punches on its session socket whatever it gets. On the default trust rule (no host decision) at most two punches escalate at once; a third punches plain (logged as `escalation_capped`). After 1.5 s of silence the peer sends a **knock** (16 random bytes, told apart by length alone: no session id, no token, no magic) to at most 1,750 random ports on the random side's IP, 32 every 100 ms (320 a second, so the spray lasts about 5.5 s and ends by about 7 s), and stops at the first answer. The knock only opens the peer's own NAT filter, so the random side's next punch from one of its sockets gets in; tokened punches go only to addresses the peer advertised over the link or that proved the token. Collision odds follow the birthday paradox, about 99.9% over a full 16-bit port range with all 256 sockets open ([Tailscale, "How NAT traversal works"](https://tailscale.com/blog/how-nat-traversal-works), "The benefits of birthdays"). The peer's "b" is only a hint: each side escalates on its own evidence, and says "b" only if its own gate lets it open sockets.
- **Punch windows.** A plain punch runs 10 s, as rns-rs's; unchanged. A punch is **escalated** when either side advertised "b" or "q" and both negotiated nomination; then both ends, deriving it from the same exchanged keys, punch for 13 s, so the side that only answers an escalation never gives up while the other still sprays. Under nomination the responder waits 2 s past its window for the nomination, and a settled initiator nominates as long. An escalated attempt, end to end (a host showing "connecting directly…" waits at most this long before `DirectLinkFailed`):

| when (from the responder's READY; the initiator starts on its arrival) | what happens |
|---|---|
| before: the proposal | each side probes ITS OWN facilitator: one round trip when it answers, at most 6 s (3 attempts of 2 s); the second observer and the allocation probes add at most 1.5 s and are skipped once the main probe took over 3 s |
| 0 to 1.5 s | the responder starts punching RTT/2 after its READY (at most 1.5 s), so both ends start together |
| +1.0 s (two-observer evidence) or +1.5 s (own evidence) | the random side, if none of its punches is acked yet: the first 64 birthday sockets |
| +1.5 s | if nothing is heard: knocks to predicted ports, and the spray (320 knocks a second); the random side's second tranche (128 sockets) |
| +2.0 s, +2.5 s | third and fourth tranches (192, then all 256), while nothing is acked |
| about +7.0 s | the spray's 1,750 knocks are spent |
| +13 s | the escalated window ends (a plain punch: +10 s, as rns-rs) |
| +15 s | the nomination grace ends: the responder stops waiting for a nomination, the settled initiator stops nominating (plain with nomination: +12 s) |
| +18 s after the session began punching | the core's punch and commit waits: no verdict means `DirectLinkFailed` (plain: 12 s, 14 s with nomination; an older peer without `"n"` sees exactly the 10 s window and 12 s waits it always did) |
| after both COMPLETEs | the link moves, one relay trip later |

- **Who may escalate.** Escalation (knocks, the spray, birthday sockets) is decided by trust in the peer, not by how a node answers: the host decides per link (`set_direct_link_escalation`, kept across the link's establishment retries); by default a node escalates toward a peer it chose (it initiated the link) and toward a peer whose request the host accepted under `ask_host`; never under `accept_all`, and not under `identified_only` (an identity costs nothing to make), nor because this node proposed the upgrade. The socket caps are the same on both evidence kinds; the own-evidence fallback ends once a second observer resolves.
- **Escalation budget.** Each link, and each peer identity, may escalate 3 times per 5 minutes, whatever it targets (a retry after a birthday miss escalates again within these), decided before anything is bound; each pair may send one target at most its three escalations' worth, and each target (an IPv4 address, an IPv6 /64) receives at most twice that from all pairs together, over a sliding window; the whole process sends at most 1,664 escalated datagrams a second (two escalations at their peak of 320 knocks and 512 birthday punches a second), each active pair keeping a fair share never below what it actually has open. All are hard caps, spent per datagram, and the traffic flows only between the two peers. A facilitator answers each source (IP and port) at most 10 times a second, each IP at most 200 (a carrier-NAT address is many subscribers), and 5,000 in all, on both ports; a full source table forgets its least recently used entries rather than refusing newcomers.
- **Diagnostics without addresses:** one INFO line per punch, per facilitator probe and per REJECT received, on target `direct_link_diag`, in counts, classes and milliseconds only (the window, the tranche reached, knocks sent and knocks before the first answer, the winning socket's index, time to first heard and first acked, heard-not-acked and acked-not-heard counts, the settled pair's class, the nomination, the probe's attempts and round trip, the REJECT's reason and arrival). `punch-probe` (in `leviculum-cli`) runs the same attempt live between two machines over a relay.
- **Keepalives** toward a peer without the two-sided commit (rns-rs) every 15 s, its path given up after 60 s of silence; with a commit peer, acked probes every 5 s and 15 s, as before.

## Status

Leviculum is in active development. The protocol implementation is functionally complete: routing, path discovery, link establishment, encrypted channels, file transfer, forward secrecy ratchets, and LoRa radio support all work and are tested against Python Reticulum on real hardware. (One caveat: sending files larger than one megabyte as multiple segments is not implemented yet — Codeberg #27; receiving multi-segment transfers works.) It is not yet production-ready.

## Getting started

### Nightly Debian / Ubuntu package (recommended)

```sh
# amd64
wget https://leviculum.network/releases/nightly/latest/leviculum-nightly-amd64.deb
# mirror: https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/leviculum-nightly-amd64.deb
sudo apt install ./leviculum-nightly-amd64.deb

# arm64
wget https://leviculum.network/releases/nightly/latest/leviculum-nightly-arm64.deb
# mirror: https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/leviculum-nightly-arm64.deb
sudo apt install ./leviculum-nightly-arm64.deb
```

To check a download, fetch the `.sha256` beside it and let `sha256sum` compare (the same works for every file below):

```sh
wget https://leviculum.network/releases/nightly/latest/leviculum-nightly-amd64.deb.sha256
sha256sum -c leviculum-nightly-amd64.deb.sha256
```

Sets up `lnsd` as a systemd service under a dedicated `leviculum` user, with its config directory at `/etc/reticulum`. The directory is mode 2775 (group-writable, setgid), so any user in the `leviculum` group shares it as a single source of truth — no per-user config or extra flags. To opt in:

```sh
sudo usermod -aG leviculum "$USER"
# log out and back in for the group to apply
```

That covers everything: the native Rust clients (`lnstatus`, `lnstest`, `lncp`, `lnprobe`, `lnpath`) talk to lnsd via the shared-instance socket, and Python Reticulum tools (`rnstatus`, `rncp`, `rnpath`, `rnprobe`, Sideband, Nomadnet, …) auto-detect `/etc/reticulum` (per `RNS.Reticulum.__init__`'s standard lookup) and connect through the same socket. If you ever swap lnsd out for the Python `rnsd` daemon, the same configdir keeps working — `lnsd` and `rnsd` are wire- and config-compatible.

The binaries are statically linked against musl, so the package installs on Debian ≥ 9 and Ubuntu ≥ 16.04 (amd64 + arm64) regardless of host glibc. The exact build is embedded in the binaries — `lnsd --version` prints e.g. `0.7.1-nightly.20260419-5a5df20`.

### Applications

Four programs ship as their own nightly packages, so none of them drags in the auto-started `lnsd` service that `leviculum` sets up. All four are versioned independently of the stack — their version numbers track the programs, not the protocol core.

```sh
# lnomad — Nomadnet terminal browser
sudo apt install ./lnomad-nightly-amd64.deb

# loquax — LXMF messenger: plain `loquax` for people, `loquax send` for scripts
sudo apt install ./loquax-nightly-amd64.deb

# lnpnd — LXMF propagation node, drop-in for Python's lxmd
sudo apt install ./lnpnd-nightly-amd64.deb

# lblogd — dev-blog server (NomadNet page node + clearnet web)
sudo apt install ./lblogd-nightly-amd64.deb
```

Download them from the same [nightly directory](https://leviculum.network/releases/nightly/latest/) (mirror: the [Codeberg release](https://codeberg.org/Lew_Palm/leviculum/releases/tag/nightly)), as `.deb` or as a userspace `.tar.gz`, for amd64 and arm64. `lnomad` and `loquax` are plain terminal programs. `lnpnd` installs a systemd service that is enabled but not started, so you can put the node's identity at `/etc/lnpnd/identity` before it first goes on the air. `lblogd` installs and starts a systemd service that serves on `http://127.0.0.1:8180/` until you point it at a domain in `/etc/lblogd/config.toml`. All four need a running Reticulum instance for their mesh side — `lnsd` from the package above, or the Python `rnsd`.

The sixth package, `lnflash`, is the firmware flasher; see [Flashing LoRa hardware](#flashing-lora-hardware-optional) below.

### Build from source

Tested on current Debian (trixie). Install the toolchain once. Rust comes
from rustup because the Debian cargo is usually too old; everything else is
apt packages. No file outside these packages and the cloned repo is needed.

```sh
# Rust:
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"

# Build, test, and flash tooling:
sudo apt install just docker.io docker-compose build-essential pkg-config \
    python3 python3-venv python3-pip python3-serial python-is-python3 esptool

# Let the interop and integration tests run containers without sudo:
sudo usermod -aG docker "$USER"   # then log out and back in
```

Then clone and build:

```sh
git clone https://codeberg.org/Lew_Palm/leviculum.git
cd leviculum
git submodule update --init reference/Reticulum
cargo build --release --bin lnsd --bin lnstatus --bin lncp --bin lnstest --bin lnprobe --bin lnpath
./target/x86_64-unknown-linux-musl/release/lnsd --version
```

The workspace pins `x86_64-unknown-linux-musl` as its build target (see
`.cargo/config.toml` for why), so the binaries land under
`target/x86_64-unknown-linux-musl/release/`, not `target/release/`. No
system C libraries are linked into the daemon.

**On a non-x86_64 host** — a Raspberry Pi or any other arm64 board — that
pin is the wrong architecture, and cargo has no way to make it follow the
host. The build succeeds and the binaries then refuse to run. Name your
own target instead, once per shell or in `~/.cargo/env`:

```sh
rustup target add aarch64-unknown-linux-musl
export CARGO_BUILD_TARGET=aarch64-unknown-linux-musl
cargo build --release --bin lnsd --bin lnstatus --bin lncp --bin lnstest --bin lnprobe --bin lnpath
```

The binaries then land under `target/aarch64-unknown-linux-musl/release/`.
Building without it prints a warning naming this override. The `.deb`
packages above are already built for arm64 and need none of this.

Run the test tiers:

```sh
cargo test-core      # unit tests
cargo test-interop   # against Python Reticulum (needs Docker and the submodule)
```

See the [installation guide](https://leviculum.network/docs/guide/installation.html) for all cargo aliases and test levels.

#### Flashing LoRa hardware (optional)

**The embedded LNode firmware ships prebuilt — you do not need this checkout
to put it on a board.** The nightly release carries `lnflash`, a self-contained
bundle holding the flasher, one firmware image per supported board, and
Nordic's S140 SoftDevice with its licence for the board that can need it:

```sh
wget https://leviculum.network/releases/nightly/latest/lnflash-nightly-amd64.tar.gz
# mirror: https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnflash-nightly-amd64.tar.gz
tar xzf lnflash-nightly-amd64.tar.gz
cd lnflash-*
sudo ./lnflash            # add --dry-run first to see what it would do
```

Nothing is downloaded and nothing is installed; everything it writes to the
board is in that directory. The flasher binary is amd64, the images inside it
are not architecture-specific.

The same bundle ships as a package, which puts `lnflash` in `/usr/bin` and the
images under `/usr/share/lnflash`, where it finds them from any directory:

```sh
wget https://leviculum.network/releases/nightly/latest/lnflash-nightly-amd64.deb
# mirror: https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnflash-nightly-amd64.deb
sudo apt install ./lnflash-nightly-amd64.deb
sudo lnflash --dry-run
```

The boards it carries an image for:

| board | hardware | built from source by |
| --- | --- | --- |
| `t114` | Heltec Mesh Node T114 | `just flash` |
| `rak4631` | RAK4631 — the WisMesh Pocket V2 and the other carriers built around that module | `just flash-rak4631` |
| `t1000e` | SenseCAP T1000-E, at level `expected`: wiring from the vendor firmware, not yet run on our rig; its bootloader may offer serial DFU only, which `lnflash` speaks (`docs/src/firmware/flashing.md`) | `just t1000e-dfu-package` |

`lnflash` and not a bare UF2 you drag onto the bootloader drive, because the
images have a precondition that cannot be checked by dragging: our firmware
places its application above an S140 7.x SoftDevice, and a factory T114 ships
S140 6.1.1, which puts the boundary one page lower. Writing the image onto
such a board produces a device that never reaches USB — no serial ports, no
drive, nothing on the bus. It is recoverable with a double-tap of RESET, and
it looks exactly like dead hardware while it lasts. `lnflash` reads the
installed version off the board before it writes anything: on a T114 it
installs the SoftDevice it ships beside the image, and on a RAK — for which it
deliberately carries no SoftDevice, because every RAK we have met already runs
a usable one — it stops and tells you what it found rather than flashing into
a brick. The mechanism, and the board that spent weeks written off
as bricked, are in
[Flashing an LNode](https://leviculum.network/docs/concepts/lnode-flashing.html).

To build the firmware from this checkout instead, install the embedded
toolchain once — the target itself, `flip-link` (the firmware's linker), and
`llvm-tools` (provides the `llvm-objcopy` the UF2 flasher uses) — then flash
attached devices over USB with the recipes in the table above:

```sh
rustup target add thumbv7em-none-eabihf
rustup component add llvm-tools
cargo install --locked flip-link
just flash            # every attached T114
just flash-rak4631    # every attached RAK4631
```

For the RNode radios (LilyGO T-Beam, Heltec, ESP32 family), extract Mark
Qvist's signed firmware off a known-good RNode once, then flash. The ESP32
cannot be bricked, a failed flash is always recoverable by re-running
flash-rnode:

```sh
just flash-rnode-setup                  # once per machine: a pinned esptool
just flash-rnode-extract /dev/ttyACM6   # once, from a trusted RNode
just flash-rnode /dev/ttyACM6
```

The chip and the flash offsets follow the board. The optional last argument
names it (`tbeam`, `heltec-v4`, or a chip name); left out, the chip is read
off the device. To back up or restore a whole board, image and all:

```sh
just flash-rnode-read-image /dev/ttyACM6 board.bin
just flash-rnode-write-image /dev/ttyACM6 board.bin
```

#### Cross-built .deb packages (optional)

Only for producing the static musl `.deb` artifacts via `just build-deb`:

```sh
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo install cargo-zigbuild cargo-deb
pip install --user ziglang
```

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE) for the full text.
[THIRD-PARTY.md](THIRD-PARTY.md) names the other licences in the tree: the
`reference/` submodules (read by tests, never built or shipped) and the
patched crates under `vendor/`.
