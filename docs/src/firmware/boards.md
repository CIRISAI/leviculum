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

(Interface registration and MTUs:
`set_interface_name` (`leviculum-nrf/src/bin/t114.rs:232-240`) and
`leviculum-nrf/src/bin/rak4631.rs:283-324`. The main loop selecting over
the three RX sources plus a timer deadline begins at
`leviculum-nrf/src/bin/t114.rs:559`.)

Transport routing is enabled in the node builder, so an LNode forwards
packets and serves paths for other peers, exactly like a
transport-enabled `lnsd`.
(`enable_transport` (`leviculum-nrf/src/bin/t114.rs:182`),
`leviculum-nrf/src/bin/rak4631.rs:217`)

## Hardware coverage

We build one firmware per **pinout family**, not per product. A family is
a set of boards whose SX1262 wiring is identical, which happens whenever
the radio ships together with the MCU as one module: every carrier board
built around that module then inherits the same wiring. One build
therefore covers many products, and we only add a build when a board's
radio wiring genuinely differs.

The same principle applies inside a family. Peripherals that a carrier
board adds are detected at run time or degrade to nothing, so a single
image serves the bare module and the fully populated product alike.

The policy behind this page, including when a specialised build is
justified, is
[How far one firmware build reaches](../concepts/board-support-scope.md);
how a board is identified before anything is written is
[Flashing an LNode](../concepts/lnode-flashing.md).

### How to read the tables

| Level | Means |
|---|---|
| **Verified** | We own this board and run it. Failures here are bugs we must fix. |
| **Expected** | Radio wiring checked against the vendor reference and identical to a verified board of the same family. Never run by us. Report results. |
| **Not covered** | Different radio wiring. Our image will not drive the radio; do not flash it. |

**Expected is not a support promise.** It means the one thing that
decides whether the radio comes up at all, the SX1262 wiring, matches.
Everything a carrier adds beyond that, displays, GNSS, Ethernet,
accelerometers, e-paper, is not driven by our firmware on these boards
even where the vendor firmware drives it. The node routes packets; the
extra hardware stays dark.

### Family A: RAK4630 module

`rak4631` binary. Radio pins are internal to the RAK4630 module and
therefore identical across every carrier: NSS `P1.10`, SCK `P1.11`,
MOSI `P1.12`, MISO `P1.13`, BUSY `P1.14`, DIO1 `P1.15`, NRESET `P1.06`,
power enable `P1.05`. DIO2 drives the antenna switch; there is no
external TX or RX enable line on any of them.

| Product | Level | Note |
|---|---|---|
| RAK WisMesh Pocket V2 (RAK19026 carrier) | **Verified** | Display, GNSS and battery supported |
| RAK4631 bare module | **Verified** | Same image, peripherals absent |
| RAK WisMesh Pocket Mini (RAK19003) | Expected | |
| RAK WisMesh Repeater / Hub (RAK2560) | Expected | Solar, IP67 |
| RAK WisMesh Tap | Expected | TFT not driven |
| RAK WisMesh Tag | Expected | |
| WisBlock with RAK13800 Ethernet | Expected | Ethernet not driven |
| WisBlock with RAK14000 e-paper | Expected | E-paper not driven |
| NomadStar Meteor Pro | Expected | |
| MonteOps HW1 | Expected | |
| GAT562 Mesh Trial Tracker | Expected | |
| MeshTiny | Expected | |
| muzi R1 Neo | Expected | |

Verified against Meshtastic's own variant definitions under
`variants/nrf52840/`, where all twelve carriers repeat the same seven pin
numbers, and against the RAK4630 datasheet quoted in
`variants/nrf52840/rak4631/variant.h`.

The non-radio pins this build drives were checked the same way, and they
hold up: the two LEDs on `P1.03` and `P1.04` are the module's own, and
`P0.13` / `P0.14` carry I2C and `P0.15` / `P0.16` the first serial port on
every carrier that defines them at all. That is not luck. The RAK4630
brings these signals out on fixed module pins and the WisBlock carriers
follow that convention, so a module-defined family stays coherent beyond
the radio. No carrier was found driving an output into a pin this build
also drives.

The one carrier worth naming is the e-paper-on-RX/TX variant, which puts
the display's SPI where the others put I2C and the serial port. Nothing
there fights our outputs, but the pins carry traffic that means nothing
to that hardware.

> **Do not confuse RAK4631 with RAK3401.** Meshtastic's `rak3401_1watt`
> variant declares the same PlatformIO board name, but it is a different
> radio module with different pins, its own SPI bus and a 1 W power
> amplifier. Our image would drive the wrong pins on it. This is why
> identification uses the bootloader's `Board-ID`, never a board name
> that vendor trees reuse.

### Family B: Heltec T114

`t114` binary. NSS `P0.24`, SCK `P0.19`, MOSI `P0.22`, MISO `P0.23`,
BUSY `P0.17`, DIO1 `P0.20`, NRESET `P0.25`, TCXO at 1.8 V via DIO3, DIO2
as antenna switch.

| Product | Level | Note |
|---|---|---|
| Heltec Mesh Node T114 | **Verified** | Status display and GNSS (L76K) supported |
| Heltec MeshSolar | **Blocked** | Radio matches, but our status LED sits on the battery controller's emergency-shutdown pin |
| LILYGO T-Echo | **Do not flash** | Two pin conflicts, see below |
| LILYGO T-Echo Plus | **Do not flash** | Same as T-Echo |

> **Matching radio pins are not sufficient, and this family is where that
> becomes concrete.** The `bsp-t114` build drives an ST7789 panel blind,
> because the panel cannot be detected, plus an LED and a GPS UART. Those
> pins are as much part of the image as the radio pins, and on a related
> board they land on whatever that board put there.
>
> On **LILYGO T-Echo** the collisions are severe. Our TFT power-enable
> output `P0.03` meets `PIN_EINK_BUSY`, which is an *output* of the
> e-paper controller, and our TFT clock `P1.08` meets `GPS_TX_PIN`, an
> output of the GPS receiver. Both are two drivers on one line. Our TFT
> data line `P0.12` meets `PIN_POWER_EN`, so the display driver would
> switch the board's peripheral power on and off as a side effect of
> drawing. This is a hardware hazard, not a board that merely fails to
> transmit.
>
> On **Heltec MeshSolar** the radio wiring, the LoRa SPI bus and even the
> GPS UART line up exactly, and none of our TFT pins is occupied. One pin
> spoils it: our status LED `P1.03` is that board's
> `BQ4050_EMERGENCY_SHUTDOWN_PIN`. Blinking a heartbeat onto the battery
> controller's shutdown input is not acceptable, so this stays blocked
> until the LED becomes a board fact that can be left unset.

**Method note.** Membership in a pinout family is a necessary condition,
never a sufficient one. Before any board moves to *Expected*, every pin
the image drives has to be checked against that board's own definition,
not only the seven radio pins. The three entries above passed the radio
check and failed this one.

Unlike family A, this family is not one module: these are separate boards
that happen to share a wiring convention, so a new Heltec or LILYGO model
is not covered by default. Heltec Mesh Pocket, Heltec T1, Heltec T096 and
LILYGO T-Echo Lite each wire the radio differently and are **not
covered** either.

**So today this build serves exactly one product, the T114.** Sharing a
radio pinout turned out to be the easy half.

> **The `Board-ID` does not separate this family from its neighbours, and
> that is a hazard rather than an inconvenience.** Meshtastic records the
> same bootloader product string `HT-n5262` for the T114, for MeshSolar
> and for the Heltec Mesh Pocket, whose radio is wired differently and
> which is not covered here. Both our tools match that string exactly
> (`board_for_id` (`lnflash/src/manifest.rs:485`),
> `leviculum-nrf/tools/uf2-runner.sh:108`), so if the `INFO_UF2.TXT`
> `Board-ID` is identical too, neither can tell a Mesh Pocket from a
> T114. We cannot check that without the hardware. Until someone does,
> treat a `HT-n5262` match as a family hint and confirm the model by
> other means before writing.
>
> This is the general rule behind both this warning and the XIAO case
> below: **a `Board-ID` is only a safe key when it is bound to the same
> unit as the radio wiring.** On the RAK4630 both belong to the module,
> so the key is exact. Heltec binds the identifier to a bootloader shared
> across models while the wiring belongs to the model, and Seeed binds it
> to the MCU module while the radio sits outside it. Both of those
> decouple, and a decoupled key cannot carry a write decision alone.

LILYGO T-Echo and T-Echo Plus are the harmless side of the same coin:
they report a different `Board-ID` (`TTGO_eink` by Meshtastic's record),
so our tools decline them today. The firmware would run; the tooling
needs the identifier before it can.

> **Elecrow ThinkNode M1 is not covered**, although its seven radio pins
> match. It runs its TCXO at 3.3 V where this family uses 1.8 V, and our
> build compiles 1.8 V in. Supporting it needs that value to become a
> board fact rather than a family fact.

### Family C: XIAO nRF52840 + Wio-SX1262

`solarnode` binary. NSS `P0.04`, SCK `P1.13`, MOSI `P1.15`, MISO `P1.14`,
BUSY `P0.29`, DIO1 `P0.03`, NRESET `P0.28`, TCXO at 1.8 V via DIO3, DIO2
as antenna switch **and an external RX enable on `P0.05`**. That last pin
is what this family adds to the shared code: DIO2 steers only the
transmit side of the Wio-SX1262's switch, so the receive side is a host
GPIO the driver asserts for a listening window and releases before every
key-up (`rx_frontend`, `leviculum-nrf/src/sx1262.rs:354`;
`LoRaRxEnable`, `leviculum-nrf/src/boards/solarnode.rs:63`).

| Product | Level | Note |
|---|---|---|
| Seeed SenseCAP Solar Node P1-Pro | **Bring-up** | On the rig since 2026-09-15; radio and GNSS not yet confirmed on the bench |
| Seeed XIAO nRF52840 + Wio-SX1262 kit | Expected | Same two modules, same seven pins plus RXEN |
| Wio Tracker L1 / L1 e-ink | Not covered | Different carrier, LEDs and battery sense not checked |

The image drives, besides the radio, one LED on `P0.19`, the battery
divider on `P0.31`/`P0.14` and the XIAO L76K GNSS on `P1.11`/`P1.12`.
There is no display. That the kit above can be *Expected* at all is
largely because the modules decide the radio and the carrier decides the
rest: a carrier that omits the L76K reports `no-hardware` at run time
rather than needing an image of its own.

The battery sampler is in (Codeberg #233), and it reads *this* board's
divider: the XIAO module's own 1 MΩ over 510 kΩ, which puts the whole
measurable range at 10 660 mV against the T114's 17 698. Two things about
it are this board's alone. Its enable pin `P0.14` is **active low** — it
sinks the low side of the divider rather than switching a load — so the
polarity travels with the pin (`battery::DividerEnable`) instead of being
a shared constant. And its 338 kΩ of source resistance is past the
100 kΩ the nRF52840 specifies the 10 µs acquisition window for, so the
board states its divider as the two resistors rather than as their ratio
and `BatteryScale::for_divider` derives a 20 µs window from them; the
`[BAT] init` line carries the result as `acq_us=`. The other two boards'
dividers are inside the default and their sampling is unchanged.

What the divider does *not* settle is the pack's cell topology, and
nothing in this firmware guesses it. What it does settle is a bound: at
10 660 mV of full scale this board cannot see a series pack above two
cells, since 3S sits above the range and would put more than the ADC's
3.6 V on the pin. A reading above 9 V is rejected as implausible and the
board says `[WARN] [BAT] implausible first reading` rather than
publishing a percentage.

The bootloader cannot tell these apart. `nRF52840-SeeedXiao-v1` names the
MCU module, and a DIY XIAO with an entirely different radio wired to the
same pads reports exactly the same string, so `lnflash` has no *flashing*
entry for this board and must not be given one on that evidence
(Codeberg #233). It does have a control-only catalogue entry, keyed on
the USB ID our own firmware publishes (`1209:0003`), so `--watch`,
`--announce`, `--set-time` and the `--radio-*` flags reach this board
like any other; a bundle carrying an image named after it is refused when
it loads. Writing firmware goes through `just flash-solarnode`, which is
told the `Board-ID` explicitly by a person who can see which board is on
the bench, and for the first flash the image goes onto the mass-storage
volume by hand.

### ESP32 class: Heltec WiFi LoRa 32 V4

**A different crate and a different stage.** Everything above is
`leviculum-nrf` on nRF52840 and describes firmware that routes packets.
The ESP32 class is `leviculum-esp`, and as of step 1 it is a skeleton:
one binary, `heltec_v4`, built for `xtensa-esp32s3-none-elf`. The row
below is in this table so the board is not invisible, not because it is
comparable to the families above.

| Product | Level | Note |
|---|---|---|
| Heltec WiFi LoRa 32 V4 | **Skeleton** | Boots and identifies itself; no radio traffic, no interfaces, no transport |

**What it can do after this step.** Bring the SoC up, open the USB
Serial/JTAG port, and emit `[FW_BUILD] git_sha=<short> dirty=<true|false>
t=<ms>` at boot and every five seconds after it, plus a `[BOARD]` line
naming the board file the image was compiled against. Blink the status
LED. Take the seven radio pins and hold the SX1262's SPI port open, with
`leviculum_core::sx126x`'s `CommandBus` and `RegisterBus` implemented over
esp-hal SPI.

**What it cannot do.** Anything on the air. No opcode is issued to the
radio, the front-end amplifier is never enabled, there is no interface,
no transport, no identity, no persistence and no BLE. It does not
interoperate with anything.

Radio pins, read off the manufacturer's schematic (revisions 4.2 and 4.3,
which agree on all seven): NSS `GPIO8`, SCK `GPIO9`, MOSI `GPIO10`,
MISO `GPIO11`, NRESET `GPIO12`, BUSY `GPIO13`, DIO1 `GPIO14`. The TCXO is
supplied from the SX1262's DIO3 through a ferrite bead; the voltage it
needs is **not established** — the schematic names the part only as
"32MHz" — and the constant is deliberately absent rather than guessed
(`leviculum-esp/src/boards/heltec_v4.rs`).

> **This board has a front end, and the two published schematics disagree
> about how it is steered.** The V4 is the high-power variant: the SX1262
> reaches the antenna through a KCT8103L PA/LNA whose CTX and CPS control
> inputs are wired to *different* sources in revision 4.2 and revision
> 4.3 — in 4.2 the SX1262's DIO2 drives CTX and `GPIO46` drives CPS, in
> 4.3 DIO2 drives CPS and `GPIO5` drives CTX. Only CSD (`GPIO2`) agrees.
> Which is right decides the transmit path, so the board revision has to
> be read off the physical board before anything keys up. Step 1 does not
> need the answer and does not pretend to have it.

There is no `lnflash` entry and no UF2: the ESP32-S3 has no mass-storage
bootloader. The image is written with `espflash` over the same USB port
the banner comes out of, which is the SoC's own USB peripheral — there is
no USB-to-UART bridge on this board.

### Not covered today

Each of these is a separate pinout family, reachable by adding one board
file rather than by changing shared code:

| Family | Products |
|---|---|
| ThinkNode M6 | Elecrow ThinkNode M6, muzi BASE |
| ProMicro + E22 | nRF52 ProMicro DIY, DLS Minimesh Lite |
| Individual wirings | Heltec Mesh Pocket, B&Q Nano G2 Ultra, LILYGO T-Echo Lite, Canary One, MS24SF1, MeshLink, TWC Mesh v4 |

The XIAO family, now family C above, is the one case where the
bootloader cannot answer which board it is: the MCU module is a XIAO and
the radio is a separate part, so a SenseCAP Solar Node and a DIY XIAO
with different radio wiring both report `nRF52840-SeeedXiao-v1`. Having
a build for it does not change that. Boards like that need a second
discriminator before anything may be written.

### Known open question

Our RAK build sets the SX1262 TCXO to 3.3 V, following the RNode
firmware, which selects `MODE_TCXO_3_3V_6X` for this board
(`leviculum-nrf/src/boards/rak4631.rs:39-43`). Meshtastic and MeshCore
both run the same module at 1.8 V. The value lives in the module, so it
applies to every carrier in family A equally. Our Pocket V2 works with
3.3 V, but the divergence against two references is unresolved and should
be settled before the family is presented as broadly supported.

## Cargo features and binaries

Three firmware binaries are defined, one per board family:

```text
[[bin]]
name = "t114"
path = "src/bin/t114.rs"

[[bin]]
name = "rak4631"
path = "src/bin/rak4631.rs"

[[bin]]
name = "solarnode"
path = "src/bin/solarnode.rs"
```

(`leviculum-nrf/Cargo.toml:332-342`)

The board-support-package (BSP) features select the runtime for a given
board. Exactly one BSP feature must be enabled per build; a
`compile_error!` in `lib.rs` enforces the mutual exclusion.
(`leviculum-nrf/src/lib.rs:18-27`)

| Feature | Effect | Cite |
|---------|--------|------|
| `bsp-t114` | T114 BSP (+ SoftDevice BLE + status display + GNSS + battery) | `leviculum-nrf/Cargo.toml:278` |
| `bsp-rak4631` | RAK4631 BSP (+ SoftDevice BLE) | `leviculum-nrf/Cargo.toml:262` |
| `bsp-solarnode` | SenseCAP Solar Node P1-Pro BSP (+ SoftDevice BLE + battery + GNSS). No display | `leviculum-nrf/Cargo.toml:299` |
| `display` | SSD1306 OLED, probed at run time | `leviculum-nrf/Cargo.toml:301` |
| `gnss` | NMEA0183 GNSS (ZOE-M8Q on the V2 baseboard, L76K on the T114 and the Solar Node) | `leviculum-nrf/Cargo.toml:302` |
| `battery` | pack-voltage monitor: the `BATTERY` log line, the panel's voltage and, on the V2, the telemetry field. Unconditional under `bsp-t114` (the divider is on every T114) and under `bsp-solarnode` (it is on the XIAO module), opt-in on the V2 via `rak-baseboard` | `leviculum-nrf/Cargo.toml:308` |
| `rak-baseboard` | aggregate of `display` + `gnss` + `battery` | `leviculum-nrf/Cargo.toml:309` |

> **Note on BLE:** Both firmware entry points register a BLE interface
> and call `leviculum_nrf::ble::init`
> (`leviculum-nrf/src/bin/t114.rs:330`,
> `leviculum-nrf/src/bin/rak4631.rs:387`). The Cargo `softdevice`
> feature, and therefore the BLE stack, is pulled in by *both* BSP
> features (`leviculum-nrf/Cargo.toml:262`,
> `leviculum-nrf/Cargo.toml:139`).

The baseboard peripherals are each gated behind their own Cargo feature
(`leviculum-nrf/Cargo.toml:301-309`) and spawned only when that feature
is on (`leviculum-nrf/src/bin/rak4631.rs:323-350`). Because each of them
either probes for its hardware or degrades to nothing when it is absent,
the aggregate build is what we ship for the whole family rather than a
Pocket-V2-only image.

The mapping from board to binary and features used by the flash recipes:

| Board | Binary | Features |
|-------|--------|----------|
| Heltec Mesh Node T114 | `t114` | `bsp-t114` |
| RAK4631 (bare module) | `rak4631` | `bsp-rak4631` |
| WisMesh Pocket V2 (full baseboard) | `rak4631` | `bsp-rak4631,rak-baseboard` |

(Feature sets as invoked in the `just flash`, `just flash-rak4631`, and
`just flash-rak4631-pocket` recipes: `Justfile:1214`, `Justfile:1242`,
`Justfile:1256`.)

### What the `lnflash` bundle carries

The distributable bundle carries an image for both families: `bsp-t114`
for the T114 and `bsp-rak4631,rak-baseboard` for the RAK4630 module
(Codeberg #261). The RAK row it ships is the last one in the table above,
not the middle one — the bare module runs the baseboard image, and the
paragraph above is why. There is deliberately no way for a user to choose
between them: the manifest cannot express two images for one `Board-ID`,
because a question nobody can answer from looking at their board is not a
question worth asking.

Which boards the bundle knows at all is `lnflash/catalogue.toml`, and it
is a shorter list than the tables above on purpose. A row here says our
image would drive that board's radio; a catalogue entry with a `flashing`
section says the bootloader can be told apart from every other board's,
which is the stricter of the two claims and the only one a write may rest
on. A catalogue entry without one — the Solar Node's, Codeberg #233 —
makes the control commands reach the board and nothing else; a bundle
naming such a board fails to load. See
[Building and flashing](flashing.md), "Which boards the bundle carries".

The two are held together mechanically rather than by care, because the
same board facts now sit in three files and Codeberg #262 records what
that costs here: eleven `Justfile` citations in `flashing.md` had drifted
by roughly 250 lines before anyone noticed. Every board the catalogue
knows has to be named on this page, and every identifier a session rests
on — the `Board-ID` a write matches, the USB IDs a bootloader and a
running application answer on, the drive label a user is told to look
for — has to appear somewhere in this book
(`every_board_the_catalogue_knows_is_named_on_the_coverage_page`,
`lnflash/tests/doc_board_catalogue.rs:171`;
`every_identifier_a_session_rests_on_is_written_down_in_the_book`,
`lnflash/tests/doc_board_catalogue.rs:196`). The check runs in the
direction a board change travels: the catalogue leads and the prose
follows, so adding a board to `lnflash` without writing it down here is
red. It does not claim the sentence around an identifier is right — the
book quotes identifiers on purpose that are not ours and must never be
catalogue keys, Meshtastic's `2886:0059` and LILYGO's `TTGO_eink` among
them (Codeberg #262).

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
| Frequency | 869.463 MHz (ReticulumNet consensus, EU ISM band) |
| Spreading factor | SF8 |
| Bandwidth | 125 kHz |
| Coding rate | CR4/5 |
| TX power | 22 dBm |

(`leviculum-nrf/README.md:8`. The profile the firmware loads at boot,
`eu_medium` (`leviculum-nrf/src/lora.rs:333-362`), applied at
`leviculum-nrf/src/bin/t114.rs:291` and
`leviculum-nrf/src/bin/rak4631.rs:379`.)

See [Flashing](flashing.md) for how to build and write these binaries to
a board, and [Recovery](recovery.md) for the bootloader-entry details.
