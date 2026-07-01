# LNode Firmware: Building and Flashing

This page covers the prerequisites, the build, and every `just flash*`
recipe — what each one does and when to reach for it.

> **Physical-device steps.** The author of this page cannot flash a
> board, so any step that writes to or resets real hardware is marked
> **derived from source — requires the physical device**. The commands
> themselves are quoted verbatim from the `Justfile` and
> `leviculum-nrf/README.md`; only the *outcome* on hardware is
> un-verified here.

## Prerequisites

Install the Rust embedded toolchain, the ARM cross-compiler (needed by
`nrf-sdc` for C-header bindgen), and add your user to the `dialout`
group for serial-port access. Log out and back in after the `usermod`
so the new group membership takes effect.

```sh
rustup target add thumbv7em-none-eabihf
rustup component add llvm-tools
sudo apt install gcc-arm-none-eabi
sudo usermod -aG dialout $USER
```

(`leviculum-nrf/README.md:12-19`)

## `--release` is mandatory

Always build and flash with `--release`. The debug profile does not fit
the nRF52840 flash — the image overflows FLASH by several hundred KB at
link time.

> The debug profile does not fit the nRF52840 flash (the image overflows
> FLASH by several hundred KB at link time) — always build and flash with
> `--release`; all `just flash-*` recipes already do.
> (`leviculum-nrf/README.md:64-67`)

Every `just flash*` recipe already passes `--release`, so following the
recipes below keeps you safe. The release profile is size-optimized
(`opt-level = "z"`, `lto = true`, `codegen-units = 1`); DWARF debug info
is kept in the `.elf` (`strip = "none"`, `debug = true`) for HardFault
post-mortem analysis, but the UF2 only carries loadable sections, so the
debug info does not bloat what lands on the device.
(`leviculum-nrf/Cargo.toml:123-133`)

## The build/flash workflow

The firmware crate `leviculum-nrf` is its own Cargo workspace, separate
from the repo-root workspace, and is cross-compiled. The flash recipes
therefore `cd leviculum-nrf` before invoking cargo. (`Justfile:274-278`)

A plain build (no flash) is:

```sh
cargo build --release
```

(`leviculum-nrf/README.md:23`)

Flashing wraps `cargo run`: the runner builds the release binary, then
copies the resulting UF2 onto each board's UF2 bootloader drive. The
UF2 conversion and copy happen inside the `cargo run` step — a bare
`cargo build` produces only the ELF.

> Build the firmware with `cargo build --release`. Flash with `just
> flash` (from the repo root), which wraps `cargo run --release --bin
> t114`.
> (`leviculum-nrf/README.md:23`)

## Touch-free vs. manual double-tap

For the **T114**, flashing is touch-free in the common case: the host
opens the board's transport CDC port at 1200 baud, the firmware
intercepts the line-coding change, writes a retained-register magic, and
soft-resets into the Adafruit UF2 bootloader. No button press.
(`leviculum-nrf/README.md:27`)

A **physical double-tap of RESET** is still needed when the firmware on a
specific T114 has crashed or never reached USB init (panic before the
handler is installed, stack overflow, hardware fault). The runner detects
this per device via a UF2-drive-polling timeout and prompts for that
specific board only; the rest of the batch keeps flashing touch-free.
(`leviculum-nrf/README.md:38`)

The **WisMesh Pocket V2 (RAK4631)** running stock Meshtastic has no
1200-baud-touch handler and no externally accessible RESET pin, so its
*first* flash needs either `just dfu-rak4631` (a Meshtastic admin
command, below) or the manual needle double-tap in the hidden pinhole.
Once our firmware is on the board, subsequent flashes use the touch path
automatically. (`Justfile:287-289`, `Justfile:306-311`. See
[Recovery](recovery.md) for the pinhole detail.)

## The flash recipes

Each recipe below is quoted from the `Justfile`. The cargo invocation is
**derived from source — requires the physical device** to actually write
firmware (it builds the same on any host, but only does something useful
with a board attached).

### `just flash` — every T114

Flashes **every attached T114** sequentially. Flashing all of them is
deliberate: if only one were flashed, a later multi-node test could run
against mixed firmware versions. Use this as your default for T114s.

```sh
cd leviculum-nrf && cargo run --release --bin t114 --features bsp-t114
```

(`Justfile:277-278`; rationale `leviculum-nrf/README.md:25`)

### `just flash-one PORT` — a single T114

Flashes one T114 by port path or udev symlink. Use it for A/B firmware
testing (one board on a new build, one on the old).

```sh
just flash-one /dev/leviculum-transport
just flash-one /dev/ttyACM3
```

Expands to:

```sh
cd leviculum-nrf && LEVICULUM_FLASH_ONLY=<PORT> cargo run --release --bin t114 --features bsp-t114
```

(`Justfile:280-285`; usage forms `leviculum-nrf/README.md:31-36`)

### `just flash-rak4631` — every RAK4631 (bare module)

Flashes every attached RAK4631 / WisMesh Pocket V2 with the bare-module
build (no baseboard peripherals).

```sh
cd leviculum-nrf && LEVICULUM_USB_PID=0002 LEVICULUM_BOARD_NAME=RAK4631 \
  LEVICULUM_UF2_BOARD_ID=WisBlock-RAK4631-Board \
  cargo run --release --bin rak4631 --features bsp-rak4631
```

(`Justfile:291-292`)

### `just flash-rak4631-one PORT` — a single RAK4631

Flashes one RAK4631 by port path or udev symlink.

```sh
just flash-rak4631-one /dev/ttyACM0
just flash-rak4631-one /dev/leviculum-rak-transport
```

Expands to:

```sh
cd leviculum-nrf && LEVICULUM_FLASH_ONLY=<PORT> LEVICULUM_USB_PID=0002 \
  LEVICULUM_BOARD_NAME=RAK4631 LEVICULUM_UF2_BOARD_ID=WisBlock-RAK4631-Board \
  cargo run --release --bin rak4631 --features bsp-rak4631
```

(`Justfile:294-298`)

### `just flash-rak4631-pocket` — WisMesh Pocket V2, full baseboard

Flashes with all RAK19026 baseboard peripherals enabled (display, GNSS,
battery). `--features rak-baseboard` aggregates the three baseboard
features. Use this for a complete WisMesh Pocket V2.

```sh
cd leviculum-nrf && LEVICULUM_USB_PID=0002 LEVICULUM_BOARD_NAME=RAK4631 \
  LEVICULUM_UF2_BOARD_ID=WisBlock-RAK4631-Board \
  cargo run --release --bin rak4631 --features bsp-rak4631,rak-baseboard
```

(`Justfile:303-304`; `rak-baseboard` aggregate
`leviculum-nrf/Cargo.toml:121`)

### `just dfu-rak4631 PORT` — DFU entry for stock Meshtastic

Triggers the Adafruit UF2 bootloader on a stock-Meshtastic WisMesh
Pocket V2 in software. Stock Meshtastic has no 1200-bps-touch handler and
the device has no externally accessible RESET pin, so this firmware-side
admin command is the only software-only DFU entry. Needed **only** for
the first flash from Meshtastic; after our firmware lands,
`just flash-rak4631` uses the touch path and this recipe is no longer
needed. Requires the `meshtastic` CLI on PATH (`pip install meshtastic`).

```sh
just dfu-rak4631 /dev/ttyACM0
```

Runs:

```sh
meshtastic --port /dev/ttyACM0 --enter-dfu
```

(`Justfile:306-314`)

## A note on disconnecting consumers

Flashing a board takes over its transport serial port. Any running
consumer of that port (for example an active `lnsd` pointed at it) loses
its connection when the board is flashed. The flash action is explicit
and active; no persistence is promised across it.
(`leviculum-nrf/README.md:40`)

The device keeps its Reticulum identity in internal flash and preserves
it across firmware updates, so re-flashing does not change the node's
address. (`leviculum-nrf/README.md:42`. More in [Recovery](recovery.md).)

## Verifying the build before you flash

`cargo build --release` (above) confirms the image links and fits flash.
If you want to lint the firmware as CI does:

```sh
just lint-nrf
```

(Builds both BSP feature sets under clippy with `-D warnings`:
`Justfile:38-40`.)

Next: [Serial ports](serial-ports.md) for wiring the flashed board into
`lnsd`.
