# LNode Firmware: Supported Boards

The LNode firmware turns an nRF52840-based board into a standalone
Reticulum transport node. It runs the same `leviculum-core` transport
engine that powers the Linux daemon, cross-compiled for Cortex-M4F, and
routes packets between three interfaces: USB serial (HDLC framing to a
host), the SX1262 LoRa radio, and BLE. There is no PC in the data path;
the device is a router in its own right.

> The transport engine is the same `leviculum-core` library that powers
> the Linux daemon, compiled for Cortex-M4F.
> (`leviculum-nrf/README.md:4`)

On the wire the firmware speaks the RNode LoRa framing protocol, so an
LNode and an RNode interoperate on the same LoRa network. On the host
side it connects to `lnsd` or `rnsd` over USB serial with HDLC framing.
On the BLE side it implements the Columba v2.2 protocol for the Columba
Android app. (`leviculum-nrf/README.md:6`,
`leviculum-nrf/src/bin/t114.rs:3-8`)

## What the firmware does

Each firmware binary registers exactly three Reticulum interfaces and
runs an event-driven main loop that dispatches packets between them:

| Interface | ID | Medium | HW MTU |
|-----------|----|--------|--------|
| `serial_usb` | 0 | USB CDC-ACM, HDLC framing to host | 564 |
| `lora_sx1262` | 1 | SX1262 LoRa radio | 255 |
| `ble` | 2 | BLE peripheral, Columba v2.2 | 564 |

(Interface registration and MTUs: `leviculum-nrf/src/bin/t114.rs:157-162`
and `leviculum-nrf/src/bin/rak4631.rs:195-200`. The main loop selecting
over the three RX sources plus a timer deadline:
`leviculum-nrf/src/bin/t114.rs:256-307`.)

Transport routing is enabled in the node builder
(`.enable_transport(true)`), so an LNode forwards packets and serves
paths for other peers, exactly like a transport-enabled `lnsd`.
(`leviculum-nrf/src/bin/t114.rs:123-128`)

## Supported boards

| Board | SoC + radio | BLE | Baseboard peripherals |
|-------|-------------|-----|-----------------------|
| Heltec Mesh Node T114 | nRF52840 + SX1262 | yes (Columba v2.2) | none |
| RAK4631 / WisMesh Pocket V2 | nRF52840 + SX1262 | yes (Columba v2.2) | optional display, GNSS, battery |

Both boards are nRF52840 + SX1262 and both run BLE through the Nordic
S140 SoftDevice. (`leviculum-nrf/README.md:1-6`,
`leviculum-nrf/Cargo.toml:65-77`)

> **Note on BLE:** Both firmware entry points register a BLE interface
> and call `leviculum_nrf::ble::init`
> (`leviculum-nrf/src/bin/t114.rs:206-232`,
> `leviculum-nrf/src/bin/rak4631.rs:243-270`). The Cargo `softdevice`
> feature — and therefore the BLE stack — is pulled in by *both* BSP
> features (`bsp-t114 = ["softdevice"]`, `bsp-rak4631 = ["softdevice"]`,
> `leviculum-nrf/Cargo.toml:102-116`).

The optional baseboard peripherals (display, GNSS, battery telemetry)
exist only on the RAK19026 baseboard of the WisMesh Pocket V2 and are
each gated behind their own Cargo feature, so the bare-module build stays
unchanged. (`leviculum-nrf/Cargo.toml:52-63`,
`leviculum-nrf/src/bin/rak4631.rs:274-298`)

## Cargo features and binaries

Two firmware binaries are defined, one per board family:

```text
[[bin]]
name = "t114"
path = "src/bin/t114.rs"

[[bin]]
name = "rak4631"
path = "src/bin/rak4631.rs"
```

(`leviculum-nrf/Cargo.toml:135-142`)

The board-support-package (BSP) features select the runtime for a given
board. Exactly one BSP feature must be enabled per build; a
`compile_error!` in `lib.rs` enforces the mutual exclusion.
(`leviculum-nrf/Cargo.toml:94-116`)

| Feature | Effect | Cite |
|---------|--------|------|
| `bsp-t114` | T114 BSP (+ SoftDevice BLE) | `Cargo.toml:116` |
| `bsp-rak4631` | RAK4631 BSP (+ SoftDevice BLE) | `Cargo.toml:115` |
| `display` | SSD1306 OLED on baseboard | `Cargo.toml:118` |
| `gnss` | NMEA0183 GNSS on baseboard | `Cargo.toml:119` |
| `battery` | battery telemetry on baseboard | `Cargo.toml:120` |
| `rak-baseboard` | aggregate of `display` + `gnss` + `battery` | `Cargo.toml:121` |

The mapping from board to binary + features used by the flash recipes:

| Board | Binary | Features |
|-------|--------|----------|
| Heltec Mesh Node T114 | `t114` | `bsp-t114` |
| RAK4631 (bare module) | `rak4631` | `bsp-rak4631` |
| WisMesh Pocket V2 (full baseboard) | `rak4631` | `bsp-rak4631,rak-baseboard` |

(Feature sets as invoked in the `just flash`, `just flash-rak4631`, and
`just flash-rak4631-pocket` recipes: `Justfile:278`, `Justfile:292`,
`Justfile:304`.)

## Build target

All firmware builds target the hard-float Cortex-M4 triple:

```sh
thumbv7em-none-eabihf
```

(`leviculum-nrf/README.md:15`. Add it with `rustup target add
thumbv7em-none-eabihf`.)

## Default radio profile

The radio parameters are compiled into the firmware and must match the
RNode configuration on the same LoRa network.

| Parameter | Value |
|-----------|-------|
| Frequency | 869.525 MHz (EU ISM band) |
| Spreading factor | SF7 |
| Bandwidth | 125 kHz |
| Coding rate | CR4/5 |
| TX power | 17 dBm |

(`leviculum-nrf/README.md:8`. The `eu_medium` profile the firmware loads
at boot: `leviculum-nrf/src/lora.rs:124-138`, applied at
`leviculum-nrf/src/bin/t114.rs:200` and
`leviculum-nrf/src/bin/rak4631.rs:239`.)

See [Flashing](flashing.md) for how to build and write these binaries to
a board, and [Recovery](recovery.md) for the bootloader-entry details.
