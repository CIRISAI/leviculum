# How far one firmware build reaches

Every board we support costs a build, a bundle entry, a row in the test
matrix and a place in everyone's head. The policy that keeps that cost
from growing with the hardware catalogue:

> **A firmware build serves a whole family of boards. A build for one
> hardware configuration is only justified where universality is
> unreachable, and the burden of proof lies with the specialised build.**

This is not an aspiration. It is already how the builds we have behave,
and it was decided twice before it was written down.

## Why a family and not a board

Boards differ in dozens of ways and almost none of them matter. What
decides whether firmware runs at all is the SX1262 wiring: seven pins,
plus the TCXO voltage and whether DIO2 owns the antenna switch. Every
other difference is peripheral in the literal sense.

Those seven pins are usually not a property of the product. They are a
property of whatever part carries the radio. When the radio ships
together with the MCU as one module, every carrier board built around
that module inherits identical wiring, and a product family of a dozen
devices collapses to a single set of pins. The RAK4630 is the clean case:
twelve different carriers in Meshtastic's tree, from the Pocket V2 to a
solar repeater to an Ethernet gateway, all repeat the same seven numbers
because the RAK4630 datasheet fixes them.

The unit of support is therefore the pinout family, and the concrete
membership lists live in
[Supported boards](../firmware/boards.md).

## What universality requires of the firmware

A single image only serves a family if everything a carrier adds either
announces itself or costs nothing when absent. That is a design
constraint on peripheral handling, not a hope:

- **Probe where the bus allows it.** The RAK baseboard display is found
  by an I2C address probe (`ack_probe`, `leviculum-nrf/src/display.rs:55`);
  when nothing answers, the task logs and exits
  (`DetectedKind::None`, `leviculum-nrf/src/display.rs:158-164`).
- **Fail into the harmless state.** The user button is configured
  `Pull::Up` (`leviculum-nrf/src/button.rs:36`), so an absent button
  reads as not pressed rather than as noise.
- **Park rather than spin.** The GNSS task awaits a UART that simply
  stays silent when no receiver is fitted.
- **Publish nowhere.** The battery task samples a pin that floats on a
  bare module, but its only subscriber is the display task that is not
  running, so no wrong reading escapes.

Measured on this tree, carrying all of that costs 47.6 KiB of flash and
2.5 KiB of RAM over the stripped build. The RAM figure is affordable by
construction rather than by luck: the same image already runs on a
populated carrier with the same chip and the same memory, and a carrier
board adds peripherals, never RAM.

Where a bus cannot be probed, writing blind is acceptable only for a
known board. The T114 drives its ST7789 panel blind because MISO is not
connected and detection is physically impossible, which is safe because
we know what else is on those pins. The same reasoning does not transfer
to an unfamiliar board, where the identical pins may carry something that
must not be driven.

**Runtime detection is therefore the lever, and a build-time feature is
the fallback.** Every peripheral moved from a feature flag to a probe
removes a reason for a second build. Codeberg #240 does this for GNSS
presence.

## When a specialised build is justified

Three conditions, any one of which is sufficient:

1. **The radio wiring differs.** No amount of runtime detection recovers
   from pins that are simply elsewhere. The third build, `solarnode`
   (Codeberg #233), is this case and nothing more interesting: the
   Wio-SX1262 puts all seven pins elsewhere and adds an eighth, a host
   RX-enable the other two families do not have.
2. **A radio parameter is board-specific rather than family-specific and
   is compiled in.** The Elecrow ThinkNode M1 matches all seven T114 pins
   but runs its TCXO at 3.3 V against the family's 1.8 V. Either the
   value becomes data, or the board needs its own build.
3. **A peripheral is dangerous when mishandled.** A board with an
   external power amplifier needs its enable line driven correctly.
   Silence is not a safe default there, unlike a missing display.

Convenience, code tidiness, and "it would be cleaner to separate them"
are not on this list.

**A frequency region is not on it either.** The compiled default is the
eu868 community profile, but the radio configuration is data, chosen at
flash time: `lnflash` ends every flash with a preset menu — eu868,
us915, au915, or a custom five-number entry — and stores the choice on
the board (see [Flashing an LNode](lnode-flashing.md), "The radio
configuration belongs to the flash"). The presets are the settings each
regional Reticulum community has converged on; a default, not legal
advice.

## Two device classes, one policy

Everything above was written about nRF52840 boards, because for a long
time those were the only boards we had firmware for. `leviculum-esp`
adds a second class — ESP32 and ESP32-S3 — and the policy does not
change: **one build per pinout family** still holds, and a board file
still describes a wiring rather than a product.

What the second class does add is a layer the first one never had to name
out loud, because with one SoC family there was nothing to separate it
from. The split is:

| Layer | Holds | Lives in |
|---|---|---|
| **Class** | the init order, the clock, which peripheral is the debug port, how the build stamp is formatted and emitted, the panic behaviour, the heap | `leviculum-esp/src/lib.rs` and the binary |
| **Board** | which GPIO carries which net, the radio's seven pins, the LED and its polarity, the battery divider, whether a GNSS receiver is fitted, the supply-enable lines | `leviculum-esp/src/boards/<board>.rs` |
| **Protocol** | every SX1262 opcode sequence, register bracket and timing calculation | `leviculum-core::sx126x`, shared with the nRF class |

The third row is the one that earns the split. The sequences that decide
what goes on the air are not per class and not per board; they were
lifted into `leviculum-core` precisely so a host test could run them, and
a second device class must not become a second copy of them. What a class
crate implements is the two traits `leviculum_core::sx126x` reaches the
hardware through — `CommandBus` and `RegisterBus` — and nothing above
them.

**Adding the second board of a class** is therefore: one
`boards/<name>.rs` with its pins and its `BoardConfig`, one
`src/bin/<name>.rs` that hands those pins to the shared init, one
`[[bin]]` stanza, one `Justfile` recipe. No new idiom, and nothing in
`lib.rs` moves. If adding a board does require moving something in
`lib.rs`, that is the signal that the thing being moved was a board fact
sitting in the class layer.

**Where the classes differ, and why that is not a policy exception.** An
ESP32-S3 board has no UF2 bootloader and no mass-storage volume, so
nothing in the ESP class corresponds to the `Board-ID` discussion below;
identification there happens over the serial protocol the ROM speaks, and
the same question — is the identifier bound to the same unit as the
wiring — has to be asked again on its own terms. The class boundary is
about where code lives. It is not a second policy.

## The axis the policy does not have: the transceiver family

Everything above varies two things, the class and the pinout family, and
holds a third fixed without ever saying so: every board in this book
carries an SX1262. The unit of support is a set of seven pins because
seven pins is all that differs once the part itself is settled. A board
with a different transceiver does not sit anywhere on that scale, and the
Seeed SenseCAP Card Tracker T1000-E (Codeberg #406) is the first one we
own: the nRF52840 we already build for, a bootloader and a USB path we
already flash through, and a Semtech LR1110 where every board above has
an SX1262.

The class table above puts every opcode sequence in one row. That row is
where the cost lands, and it is not spread evenly across the code that
looks radio-shaped. Measured on this tree, 2026-09-25:

| What | Lines | Reaches a second radio family |
|---|---|---|
| `leviculum-core/src/sx126x.rs` | 2461 | No: command set, register map, timing arithmetic |
| `leviculum-nrf/src/sx1262.rs` | 1601 | The SPI and pin glue partly, the opcodes not at all |
| `leviculum-nrf/rx-arming/src/lib.rs` | 3879 | Mostly yes, see below |
| `leviculum-nrf/channel-access/src/lib.rs` | 745 | Mostly yes, see below |

**The two pure crates are the cheaper half, and that is worth stating
because their prose names the chip on nearly every page and reads like the
expensive half.** Both were lifted out of the driver so a host test could
drive them, and being liftable is the same property as being portable:

- `rx-arming` reaches a radio only through two traits, `RxPort`
  (`leviculum-nrf/rx-arming/src/lib.rs:155`) and `RxWindowProbe`
  (`leviculum-nrf/rx-arming/src/lib.rs:694`). The driver implements both,
  `RxPort` (`leviculum-nrf/src/sx1262.rs:1430`) and `RxWindowProbe`
  (`leviculum-nrf/src/sx1262.rs:1460`), and a test fake implements
  `RxPort` (`leviculum-nrf/rx-arming/src/lib.rs:1826`) beside it. A second
  family writes a third implementation; it does not fork the crate.
- `channel-access` touches no radio at all. The caller reports what its
  own channel-activity detection said — `cad_clear`
  (`leviculum-nrf/channel-access/src/lib.rs:256`), `cad_busy`
  (`leviculum-nrf/channel-access/src/lib.rs:266`), `cad_error`
  (`leviculum-nrf/channel-access/src/lib.rs:283`) — and the jitter slot is
  derived from bandwidth, spreading factor and coding rate
  (`jitter_slot_ms`, `leviculum-nrf/channel-access/src/lib.rs:97`), which
  are properties of the modulation rather than of the part. Only the
  module's own text names the SX1262
  (`leviculum-nrf/channel-access/src/lib.rs:17`).

**Where the seam would have to open, if it opens.** `RxLatch`
(`leviculum-nrf/rx-arming/src/lib.rs:355`) is three named LoRa interrupt
bits — preamble, header, `RxDone` — read back without being cleared, and
the two bounds computed from them are forwarded to
`leviculum_core::sx126x::tx_defer_ms` and
`leviculum_core::sx126x::false_preamble_ms`. A part that latches those
three the same way slots in behind the existing traits. A part that does
not forces the traits themselves open, and choosing between widening them
and carrying a second implementation is a design decision, not a port.
**That decision is not taken here, and nothing in this tree has measured
an LR11xx part against these traits.** Until one has, the honest estimate
is the driver in full and the two crates untouched.

## The limit that bites

Universality reaches exactly as far as the identification does. A build
may serve twelve carriers, but something has to decide that the board in
front of it is one of those twelve, and that decision is made from the
bootloader (see [Flashing an LNode](lnode-flashing.md)). Where the
bootloader identifier is bound to the same unit as the wiring, the two
line up and the family is safe end to end. Where a vendor shares one
identifier across models with different wiring, a correct universal build
can still be written onto a board it does not fit.

So the reach of a build and the precision of its identification have to
be argued together. A family is only as wide as the narrowest of the two.
