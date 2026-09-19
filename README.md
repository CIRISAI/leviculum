# Leviculum

Leviculum is an independent reimplementation based on the [Reticulum protocol specification](docs/src/appendix/reticulum-specification.md).
It is wire-compatible with Reticulum and runs on Linux, macOS, and embedded devices.

## What is Reticulum?

Reticulum is a networking stack for building resilient, encrypted mesh networks over any transport medium. It works over LoRa radios, TCP, UDP, serial links, or anything that can carry bytes. Every node gets a cryptographic identity. Every connection is end-to-end encrypted. No servers, no accounts, no infrastructure required.

## What does Leviculum do?

Leviculum provides the same functionality as Python Reticulum but compiled to native code. The `lnsd` daemon is a drop-in replacement for `rnsd`, `lncp` replaces `rncp`, `lnstatus` replaces `rnstatus`, `lnprobe` replaces `rnprobe`, and `lnpath` covers `rnpath`'s path query, wait and drop. Python CLI tools like `rnstatus`, `rnpath`, and `rnprobe` also work against a running `lnsd` without modification.

The protocol core (`leviculum-core`) compiles as `no_std` with only `alloc`, so it runs on microcontrollers. The same code powers the Linux daemon, a future Android app, and embedded firmware.

## Status

Leviculum is in active development. The protocol implementation is functionally complete: routing, path discovery, link establishment, encrypted channels, file transfer, forward secrecy ratchets, and LoRa radio support all work and are tested against Python Reticulum on real hardware. (One caveat: sending files larger than one megabyte as multiple segments is not implemented yet — Codeberg #27; receiving multi-segment transfers works.) It is not yet production-ready.

## Getting started

### Nightly Debian / Ubuntu package (recommended)

```sh
# amd64
wget https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/leviculum-nightly-amd64.deb
sudo apt install ./leviculum-nightly-amd64.deb

# arm64
wget https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/leviculum-nightly-arm64.deb
sudo apt install ./leviculum-nightly-arm64.deb
```

Sets up `lnsd` as a systemd service under a dedicated `leviculum` user, with its config directory at `/etc/reticulum`. The directory is mode 2775 (group-writable, setgid), so any user in the `leviculum` group shares it as a single source of truth — no per-user config or extra flags. To opt in:

```sh
sudo usermod -aG leviculum "$USER"
# log out and back in for the group to apply
```

That covers everything: the native Rust clients (`lnstatus`, `lnstest`, `lncp`, `lnprobe`, `lnpath`) talk to lnsd via the shared-instance socket, and Python Reticulum tools (`rnstatus`, `rncp`, `rnpath`, `rnprobe`, Sideband, Nomadnet, …) auto-detect `/etc/reticulum` (per `RNS.Reticulum.__init__`'s standard lookup) and connect through the same socket. If you ever swap lnsd out for the Python `rnsd` daemon, the same configdir keeps working — `lnsd` and `rnsd` are wire- and config-compatible.

The binaries are statically linked against musl, so the package installs on Debian ≥ 9 and Ubuntu ≥ 16.04 (amd64 + arm64) regardless of host glibc. SHA-256 checksums are published alongside every `.deb` with the suffix `.sha256`. The exact build is embedded in the binaries — `lnsd --version` prints e.g. `0.7.1-nightly.20260419-5a5df20`.

### Applications

Two programs ship as their own nightly packages, so neither drags in the auto-started `lnsd` service that `leviculum` sets up. Both are versioned independently of the stack — their version numbers track the programs, not the protocol core.

```sh
# lnomad — Nomadnet terminal browser
sudo apt install ./lnomad-nightly-amd64.deb

# lblogd — dev-blog server (NomadNet page node + clearnet web)
sudo apt install ./lblogd-nightly-amd64.deb
```

Download them from the same [nightly release](https://codeberg.org/Lew_Palm/leviculum/releases/tag/nightly), as `.deb` or as a userspace `.tar.gz`, for amd64 and arm64. `lnomad` is a plain terminal program. `lblogd` installs and starts a systemd service that serves on `http://127.0.0.1:8180/` until you point it at a domain in `/etc/lblogd/config.toml`. Both need a running Reticulum instance for their mesh side — `lnsd` from the package above, or the Python `rnsd`.

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

See the [installation guide](https://codeberg.org/Lew_Palm/leviculum/src/branch/master/docs/src/guide/installation.md) for all cargo aliases and test levels.

#### Flashing LoRa hardware (optional)

**The embedded LNode firmware ships prebuilt — you do not need this checkout
to put it on a board.** The nightly release carries `lnflash`, a self-contained
bundle holding the flasher, one firmware image per supported board, and
Nordic's S140 SoftDevice with its licence for the board that can need it:

```sh
wget https://codeberg.org/Lew_Palm/leviculum/releases/download/nightly/lnflash-nightly-amd64.tar.gz
tar xzf lnflash-nightly-amd64.tar.gz
cd lnflash-*
sudo ./lnflash            # add --dry-run first to see what it would do
```

Nothing is downloaded and nothing is installed; everything it writes to the
board is in that directory. The flasher binary is amd64, the images inside it
are not architecture-specific. The boards it carries an image for:

| board | hardware | built from source by |
| --- | --- | --- |
| `t114` | Heltec Mesh Node T114 | `just flash` |
| `rak4631` | RAK4631 — the WisMesh Pocket V2 and the other carriers built around that module | `just flash-rak4631` |

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
[docs/src/concepts/lnode-flashing.md](https://codeberg.org/Lew_Palm/leviculum/src/branch/master/docs/src/concepts/lnode-flashing.md).

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

## Bugs and contributions

Report bugs and ask questions at
<https://codeberg.org/Lew_Palm/leviculum/issues>. When the daemon is
involved, attach the output of `lnstest diag` (see the
[quickstart](docs/src/lnsd-quickstart.md) for the exact command). For
patches, see [CONTRIBUTING.md](CONTRIBUTING.md).

A suspected security vulnerability is the one thing that does not belong in
the tracker: an issue is public from the moment it is filed, which hands the
flaw to everyone who might use it. [SECURITY.md](SECURITY.md) names the
private contact, the acknowledgement window, and what is in scope.

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE) for the full text.
