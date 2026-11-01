# Firmware layers: one orchestration, many boards

The board firmware is meant to run on three chip families, the nRF52840,
the ESP32-S3 and the RP2350, without a second copy of anything above the
chip. Lew decided on 2026-10-23 that no code is duplicated and that the
best architecture wins; Cedric Rivard's fork is wanted for what it does,
and its code is not owed. This page measures where the duplication is
today, draws the three strata the firmware is cut into, names the seams
between them and their shape, and orders the extraction. The dated
decisions are at the end.

Line counts and `path:line` citations into this tree are against master
at the commit that adds this page. Citations into the fork are against
the branch `cedrivard/Leviculum` `rp2350-m9-net` at `7a699446`; they are
written as file plus line words, because that tree is not this one and
the citation guard cannot hold them.

The rule this page serves is `rule:interface-isolation` one level down
([Interface isolation](interface-isolation.md)): only the bind crate
knows its chip.

## The problem, measured

The board firmware is already two strata. Thirty-five host-tested policy
crates under `leviculum-nrf/*` (the `[workspace] members` of
`leviculum-nrf/Cargo.toml`: announce-policy, rx-arming, tx-spacing,
channel-access, pn-store, record-log, heap-budget, feedback, usb-policy,
telemetry-policy, gnss-time, gnss-presence, boot-count, boot-trace,
time-floor, mute-lease, queue-budget, drop-budget, log-line, node-name,
persist-ack, media-state, motion, battery-scale, gnss-init, screen, ...)
plus `leviculum-core` and `leviculum-lxmf` hold the behaviour. Five of
the thirty-five are consumed by the host daemons too: `leviculum-std`
lists ble-tx, channel-access, announce-policy, queue-budget and
heap-budget (`leviculum-std/Cargo.toml:92`, `:95`, `:99`, `:103`, `:164`),
and lnpnd takes announce-policy, one of the five
(`lnpnd/Cargo.toml:159`). `leviculum-core` itself depends on
telemetry-policy (`leviculum-core/Cargo.toml:55`). So "nRF" in their
path is a misstatement of ownership, not a description.

Above them, `leviculum-nrf/src` (32,740 lines with the binaries at 961) glues
the policy to the nRF52840, and the glue has three shapes mixed in one
tree:

- **The node loop is not a module. It is four copies.** Each board
  binary carries its own `main` with the loop inside it:
  `leviculum-nrf/src/bin/t1000e.rs:53-1287` is a 1,235-line `main` with a
  537-line `loop { select ... dispatch ... settle }` at
  `leviculum-nrf/src/bin/t1000e.rs:750-1286` and 110 references into
  `leviculum_nrf::`; `bin/t114.rs`, `bin/rak4631.rs` and
  `bin/solarnode.rs` carry the same loop, and `t114` and `rak4631`
  differ by 500 lines. Beside the four board binaries sit two probes
  (`bin/sd-ram-probe.rs` 294, `bin/qspi-selftest.rs` 634).
- **Orchestration modules** that know no chip but call each other
  freely: `announce.rs` 587, `mgmt.rs` 259, `media.rs` 320, `name.rs`
  259, `events.rs` 261, `heap_census.rs` 398, `remote_status.rs` 139,
  `transport_stats.rs` 169, `chtm.rs` 71, `banner.rs` 56,
  `telemetry.rs` 2062, `pn.rs` 3759, `lora.rs` 2708. Moved out to
  `leviculum-nrf/firmware/src` so far: `log.rs` in step 1 (954), and
  in 957 `iface_bytes.rs` (177 lines there since 958, which dissolved
  its 15-line name table into the board files, because the LoRa entry
  names the board's radio), `dispatch.rs` 49, `clock.rs` (104 since 958
  added the time-source state), `boot_trace.rs` (120 there; the 27-line
  reset-cause register read stays) and `identity.rs` 119; in 958
  `interface.rs` 77; in 959 `board_state.rs` 133 (`baseboard.rs` until 961), the board state the
  sensor tasks feed, and the new `entropy.rs` 148; in 961 `heap.rs`
  120, the global allocator with the two heap facts the census and the
  propagation node read and the `[HEAP]` loop (the crate's `lib.rs` is
  55).
- **Backends** that know the chip: the SoftDevice BLE host
  (`ble/columba.rs` 3168, `ble/mod.rs` 1300, `ble/notify.rs` 238), USB
  CDC (`usb.rs` 1464, which also holds the control-frame executor, see
  [the seams](#the-seams-and-their-shape)), QSPI and internal flash
  (`qspi.rs` 1188, `record_store.rs` 216 since step 1 moved its policy
  half, 665 lines now, to `leviculum-nrf/firmware/src/record_store.rs`,
  `flash.rs` 79), the SX1262 driver (`sx1262.rs` 1704), entropy from the
  SoftDevice (`rng.rs` 214, whole: since 959 its fill is what stratum 2's
  `Entropy` forwards to), the heap pool and the panic post-mortem
  (`lib.rs` 734: the pool's memory at `leviculum-nrf/src/lib.rs:510`,
  handed once at boot to stratum 2's allocator; the panic
  counter and the post-mortem records moved in step 1 to
  `leviculum-nrf/firmware/src/postmortem.rs`, the panic handler and
  HardFault stay at `leviculum-nrf/src/lib.rs:591-734`),
  `boot_count.rs` (writes through `embassy_nrf::nvmc::Nvmc`,
  `leviculum-nrf/src/boot_count.rs:45`), the board files (`boards/*.rs`
  1300, each with its interface-name table since 958).

Cedric Rivard's fork needed the same glue for the RP2350 and the
ESP32-S3. It could not reuse ours, and wrote `leviculum-fw`
(`leviculum-fw/src`: 26,651 lines in 52 files, 36 modules declared in
its `lib.rs` plus the test module), `#![no_std]`, on the same policy
crates, imported from `../leviculum-nrf/` by path (25 of them). Twenty
of its module names are our file names: announce, chtm, clock,
dispatch, events, gnss, identity, iface_bytes, interface, log, lora,
media, mgmt, name, pn, radio_store, record_store, remote_status,
telemetry, transport_stats; `ble` is a directory module in both. The
fork did not extract a node loop either: its loop is per family too
(`leviculum-esp/src/node.rs` in the fork, 531 lines, and
`leviculum-rp`'s own `node::run`); the fork's `leviculum-fw/src/node.rs`
holds the interface registry (`Ifaces`, `register_interface`,
`on_link_wake`).

So the duplication is located, and so is its shape: two trees each
carry one copy of the orchestration modules and several copies of the
node loop, and neither carries the stratum this page draws. The module
boundaries agree across both trees, which is what makes the extraction
below a relocation rather than a design.

## The target: three strata

| stratum | what | crates | knows |
|---|---|---|---|
| 1 Policy | every protocol and behaviour decision, host-tested | `leviculum-core`, `leviculum-lxmf`, the thirty-five policy crates | no executor, no HAL, no board. One chip lives here already and stays: `leviculum-core/src/sx126x.rs` (2,906 lines) is the SX126x command set behind `RegisterBus` (`leviculum-core/src/sx126x.rs:1065`) and `CommandBus` (`leviculum-core/src/sx126x.rs:924`), implemented by our `Sx1262` (`leviculum-nrf/src/sx1262.rs:1689`, `leviculum-nrf/src/sx1262.rs:1709`) and by the fork's `leviculum-esp/src/sx1262.rs`; a chip's wire protocol is policy, its bus is a backend |
| 2 Orchestration | the node loop, once; every module of the orchestration list above; the control-frame executor | one new crate, working name `leviculum-firmware` | `embassy-executor`, `embassy-time`, `embassy-sync`, `alloc`, the seams below; no HAL crate, no chip name, no `cfg(feature = "bsp-*")` |
| 3 Bind | pins, backends, `main` | `leviculum-nrf`, `leviculum-esp`, `leviculum-rp` | one chip family each |

Stratum 2 is **extracted** from `leviculum-nrf/src` and from the four
binaries, with the behaviour it has today. It is not written anew and
not adopted from the fork. The reason is evidence: our glue carries
months of rig measurement (the `#438` collision floor, `#466` and the
rx-arming latch settle, the PN cells, the BLE room cells); a rewrite
would re-verify it on the rig, a relocation changes nothing the census
and the cells pin. The fork's `leviculum-fw` is the specification of
what stratum 2 must offer a second and a third family, and the source
of the ESP32-S3 and RP2350 backends; it is not the layer. Where the fork
cut a module better than we did (the control-frame executor, below),
its cut is taken.

## The seams, and their shape

Both trees today connect their modules the same way: free functions
over static state, and `embassy_sync` channels between tasks
(`ble_active` (`leviculum-nrf/src/media.rs:205`) and `note_duty_budget`
(`leviculum-nrf/src/announce.rs:152`) here; `lora::channels()`,
`BleChannels`, `LoRaChannels` in the fork). The seams take two shapes,
chosen by what crosses them:

- **Message seams** where a backend is a task: the LoRa part, the BLE
  controller, the host port. The orchestration owns the channels and
  the types on them (the fork already does this for LoRa configuration
  and BLE); the backend task consumes and produces. No generics climb
  into the orchestration modules; `announce.rs` keeps calling
  `crate::media::ble_active()` exactly as today, with the function now
  reading orchestration-owned state that the backend task feeds.
- **Trait seams** where a backend is a thing the orchestration calls
  synchronously or awaits once: flash, entropy, clock, sensors, sinks.
  Here an ecosystem trait is used when it exists and has two
  implementers: `embedded_storage_async::nor_flash::MultiwriteNorFlash`
  for flash (the fork's `leviculum-fw/src/record_store.rs` already binds
  it at lines 57, 268 and 323), `embedded_hal_async::spi::SpiDevice` for
  buses, `rand_core` for entropy. A project trait is written only where
  no ecosystem trait fits (the post-mortem sink, the one-button and
  keypad input, the display).

Rules for both: no `dyn`; no `cfg` per chip inside stratum 2
(`leviculum-nrf/src/radio.rs:8-10` already states this for the radio,
and selects by cargo feature; the seam makes the selection a type or a
task); every seam has a host fake, so stratum 2 is host-tested like
stratum 1 (precedents: the rx-arming fake radio harness, the fork's
`just test-fw`); a seam is cut only where a second implementer exists.

The set-once static is the exception, not a third shape: the interface
names (958, `iface_bytes::set_names`) and the entropy source (959,
`entropy::set_source`) are things the bind crate hands up once at boot,
before the orchestration runs, and never changes again. A seam the
orchestration logic uses at run time is a trait or a message, never a
set-once static: the heap (961) moved whole instead, because the
allocator is stratum 2's, and the sinks, the transceiver and the BLE
link are cut as traits and messages with host fakes. The feedback sink (973) is such a trait: the policy,
`Feedback<S: Sinks>`, is host-tested over its own fake, and only the board's
one instance reaches the bind crate's implementation through a set-once
registration of its two methods (`feedback::set_sinks`), because the
callers that raise a pattern cannot name a bind-crate type; it goes when the
node loop (step 7) holds a `Feedback<S>` of its own. The button (976)
needs no registration: the bind crate's task owns the pin, so it hands
its `ButtonInput` to `leviculum_firmware::button::run` by value, and the
press handling, `Button<S: Sinks>`, is host-tested over a scripted input,
the fake clock and its own signals; the board's one instance raises on
the registered feedback and the `ANNOUNCE_PRESS` and `DISPLAY_WAKE`
statics, until the node loop holds its own. The display (980) is the same
shape: each panel's task builds its `Panel` and hands it to
`leviculum_firmware::display::run` by value, and the page and power policy,
`Display<P: Panel>`, is host-tested over `RecordingPanel`, the fake clock
and its own wake signal. What the page shows that only the bind crate's
backends know (the LoRa counters, the BLE links, the propagation store
fill) comes up as a value, `StatusFacts`, from a fill function the bind
crate hands to `run` and the page calls once per frame tick, so the
orchestration names none of those backends. The battery (991) closes the
small seams the same way: the bind crate's task configures the SAADC and
the divider's enable line, builds `SaadcGauge` over the board's
`BatteryScale` (the gauge's construction argument, not policy) and hands it
to `leviculum_firmware::battery::run` by value, with the two facts the
`[BAT] init` line states about it (`GaugeFacts`: the full scale and the
acquisition window) as a value beside it. The cadences, the classification
at boot, the filter, the window, the lines and the publish to
`BATTERY_STATE` are `monitor`, host-tested over `FakeBatteryGauge` and the
fake clock with the events it hands to a function of the test's own.

The `today` column below is a reading; the measured call list per seam is
`scripts/nrf-seam-vocabulary.txt` (958), every item a bind-crate file
names below the bind line with code and doc comments counted apart,
regenerated and checked by `just nrf-module-reach`, so the step orders
are written against that file and not against this table.

| seam | shape | covers | today | from the fork |
|---|---|---|---|---|
| Transceiver | message, plus the type set `Radio`/`RxStatus`/`Error`/`FrontEnd` of `radio.rs` as the vocabulary | bring-up, configure, CAD, TX, RX arming, IRQ facts | `sx1262.rs` over core's bus traits; `leviculum-lr11xx` (method set name for name, `leviculum-nrf/src/radio.rs:3-6`) | the fork has no transceiver seam: `leviculum-fw/src/lora.rs` names `leviculum_lr11xx::Lr11xx` (lines 21, 53, 742, 1151), its Heltec SX1262 path is a separate `leviculum-esp/src/{lora,radio,sx1262}.rs`. The seam is ours to cut |
| Control port | trait, the fork's `ControlPort` (`leviculum-fw/src/control.rs`, lines 166 to 188: two methods always, five more behind its `pn`, `bench` and `sd-storage` features) under a stratum-2 control-frame executor (the fork's `control.rs`, 795 lines) | the framed host link: bytes, DTR/RTS or XON/XOFF | `usb.rs` is today the executor AND the port: it reaches sixteen sibling modules and two `lib.rs` functions and runs `ControlAction` dispatch (`leviculum-nrf/src/usb.rs:25`, `leviculum-nrf/src/usb.rs:791`), only seventeen lines touch `embassy_usb` | `serial-mux` on the M9 (one UART, debug text and HDLC), RP2350 USB CDC. The fork's cut is adopted |
| BLE link | trait down, messages up (1003): `trait BleLink` with `send(aim, packet)` and `set_advertised_name`; `seams::BleLinkEvent` (peer up/lost with role, inbound packets) up; carrier on/off and the pacing gap are pushed values, nothing is per fragment | the Columba link as the orchestration sees it | SoftDevice: the GATT service is declared by `nrf_softdevice` macros (`leviculum-nrf/src/ble/columba.rs:135`, `leviculum-nrf/src/ble/columba.rs:151`, `leviculum-nrf/src/ble/columba.rs:1811`) and raw S140 structs (`leviculum-nrf/src/ble/columba.rs:780`, `leviculum-nrf/src/ble/columba.rs:2325`); every upward call goes through `leviculum_firmware` — the carrier read, the pacing value, the link table and the census publishes through `ble_link` since 998; the peer transitions and inbound packets as `seams::BleLinkEvent` through `ble_link::deliver`, and the `[MEDIA]` drop-run/held-down lines through `leviculum_firmware::media` since 999, with `BleInterface` and the node-facing channels living in `ble_link`; since 1003 the node's link table (`ble_link::LinkTable`: identity, our role, since when, fed by the peer events) lives there too, and the fan-out loop (`ble_link::run_fanout`) hands each packet down through `BleLink::send`, implemented by `ble::SoftDeviceLink` over the slot map that stays below (`PeerRegistry`: identity to drain slot, the duplicate rules of #360 and #412); the GAP and advertisement name is `init`'s argument, and the boot phases at `leviculum-nrf/src/ble/mod.rs:880,890` are stratum 2's since 957, calls down rather than up; `TxAim::Flood` crosses as "flood, no exclusion" and the backend resolves it over every claimed slot, a connection that has not handshaked included, while `Peer` and `FloodExcept` hand identities down | TrouBLE over `bt_hci::Transport` (the fork's `leviculum-fw/src/ble/mod.rs`, lines 81 to 91) on CYW43439 and on the ESP32-S3 radio. The seam sits above GATT because the SoftDevice is not an HCI controller; the fork's `HciTransport` stays inside its backend |
| Flash | `MultiwriteNorFlash`, plus `pn-store::Region` (`leviculum-nrf/pn-store/src/lib.rs:160`) and `record-log` on top | the record log, the PN store, settings, identity, boot count, time floor (`time_floor.rs` writes through `crate::flash::SharedFlash`, `leviculum-nrf/src/time_floor.rs:61`, beside the regions of `boot_count` and `record_store`) | internal flash and QSPI (`record_store.rs`, `qspi.rs`, `flash.rs`), `boot_count.rs` over `Nvmc` | RP2350 flash with a PSRAM index, ESP32 flash, `leviculum-sd-store` on a card |
| Clock | trait | monotonic time, calendar sources and their rank | embassy time, GNSS time, host time; the source the board reports (`set_time_source`, in stratum 2's `clock` since 958); the monotonic half, `now_ms` and `wait_until`, on `leviculum_firmware::seams::Clock` since 976, implemented by `clock::EmbassyClock` | the same plus SNTP (`TimeSource::Ntp`, the fork's `leviculum-fw/src/lib.rs` line 120) and the PCF8563 |
| Entropy | `rand_core` | what core's crypto draws | `leviculum_firmware::entropy::Entropy` (959), a zero-sized `RngCore` over the fill the bind crate registers at boot (`set_source`); here that fill is `rng.rs`'s (`leviculum_nrf::rng::fill_entropy`, SoftDevice `sd_rand_application_bytes_available_get`, `leviculum-nrf/src/rng.rs:110`) | the ESP32 and RP2350 hardware RNGs |
| Sensors | traits per sensor | GNSS UART, motion, battery, die temperature, later magnetometer | AG3335 (`gnss.rs`), QMA6100P (`motion.rs`), `nrf_softdevice::temperature_celsius` (`leviculum-nrf/src/telemetry.rs:946`); the sensor STATE (fix, presence, motion, battery watches) lives in stratum 2 since 959 (`leviculum_firmware::board_state`), the sensor TASKS that feed it stay below, except the battery's: since 2026-10-25 (991) the battery is cut, `leviculum_firmware::seams::BatteryGauge` (`sample_mv`) with the host fake `FakeBatteryGauge`, the sampling task, its 1 s, 5 and 30 cadences, the classification at boot and the `[BAT]` lines in `leviculum-nrf/firmware/src/battery.rs`, the SAADC, the divider's enable line and the implementation `SaadcGauge` in `leviculum-nrf/src/battery.rs` | AG3352 (same Airoha init), MAX17048 (`battery-scale`); the M9's QMI8658B IMU ([The ThinkNode M9](thinknode-m9.md)) has no driver in the fork yet |
| Sinks | traits | LED, buzzer, display, keypad or button, the debug log (`log.rs` 366, to CDC today), the post-mortem store (`leviculum-nrf/firmware/src/postmortem.rs` since step 1, filled by the handlers at `leviculum-nrf/src/lib.rs:587-730`; the fork's `diag/postmortem.rs`, 398 lines) | `led.rs`, `feedback.rs`, SSD1306 and ST7789, `button.rs`; since 2026-10-25 (973) the feedback sinks are cut: `leviculum_firmware::seams::Sinks` (`present`, `play`) with the host fake `RecordingSinks`, the policy in `leviculum-nrf/firmware/src/feedback.rs`, the LED and buzzer players and their implementation `NrfSinks` in `leviculum-nrf/src/feedback.rs`; since 2026-10-25 (976) the button is an input rather than a sink: `leviculum_firmware::seams::ButtonInput` (`wait_for_edge`, `is_pressed`) with the host fake `FakeButtonInput` on the `FakeClock`, the press handling (the gate, the `BTN_PRESS` and `BTN_HOLD` lines, the display wake, the feedback, `ANNOUNCE_PRESS`) in `leviculum-nrf/firmware/src/button.rs`, the pin, its pull and the implementation `NrfButton` in `leviculum-nrf/src/button.rs`; since 2026-10-25 (980) the display is cut: `leviculum_firmware::seams::Panel` (`frame`, `flush`, `has_power_control`, `set_power`) with the host fake `RecordingPanel`, the page composition and the one-minute power policy in `leviculum-nrf/firmware/src/display.rs`, the I2C probe and `Ssd1306Panel` in `leviculum-nrf/src/display.rs`, the SPI driver and `St7789Panel` in `leviculum-nrf/src/st7789.rs`, the page facts only the backends know in `leviculum-nrf/src/panel_facts.rs` | the M9's ST7789 and keypad (menus, [One button](one-button.md)) |
| Watchdog | trait | feed and timeout | none fed: the ten matches in `leviculum-nrf/src` are four comments about the outbound link watchdog in the board binaries and `qspi-selftest`'s read of the WDT's RUNSTATUS | per family |
| Net (optional) | the fork's `leviculum-net` as a stratum-3 crate | TCP, UDP, NTP over `embassy-net` | none on nRF52840 | shared by esp and rp |

Power management has zero references in today's firmware and gets no
seam until there is something to put behind it.

The protocol inside each seam stays below it: the Columba framing and
defragmenter in `leviculum-core/src/framing/ble.rs`, the notify pump in
`ble-tx` (the fork's `pump_fragments` addition to that crate is its
shared form), the LoRa channel discipline in `rx-arming`,
`channel-access`, `tx-spacing`. A fix to behaviour never touches a
backend; a fix to a chip never touches behaviour.

## The extraction, bottom-up

The orchestration modules import the backends (`announce.rs` imports
pn, telemetry, lora and button; `mgmt.rs` telemetry,
name and usb; `media.rs` ble and telemetry; `name.rs` ble, media and
telemetry; `events.rs` announce, pn and transport_stats;
`heap_census.rs` ble and pn; at 959, from the census below), so moving them
first does not compile. The order is therefore bottom-up: the seams
first, the modules that use them after, the node loop last.

The graph this reads from is measured, not described:
`scripts/nrf-module-reach.txt` lists every file of both trees with its
line count and the `crate::` names it reaches, and `just
nrf-module-reach` fails when the tree and the file disagree, so every
move regenerates it and its history is the extraction's record. It
re-placed step 2: `usb.rs`, which holds the control-frame executor,
reaches `lora`, `telemetry`, `mgmt`, `media`, `name` and eight more
siblings, and the fork's cut separates only the port from that
executor, so step 2 runs after steps 5 and 6, keeping its number.

Each step: cut the seam, let `leviculum-nrf` implement it, move the
files that now depend only on seams, prove zero behaviour change by the
existing tests, the compile-time census (`lint-nrf`, `nrf-stack-frames`
read the ELF and need no board), the runtime `[HEAP_CENSUS]` line on a
board, one rig cell per touched subsystem, and a debug-log diff over
one scripted scenario with these keys masked: `t=`, `host_ms=`, `rssi=`,
`snr=`, every counter and the `[HEAP_CENSUS]` numbers (the capture
exists in `scripts/debug-witness.sh`; the masking is step 0's
[`scripts/log-diff-mask.py`](../firmware/host-test-seams.md#the-log-diff-masking)). One coder pass per step.

The order, decided 2026-10-24 (Lew, plan B; [decision
record](#decision-record)): 957 to 959 measured that the sixteen
orchestration modules form one strongly connected set, which cannot
move one module at a time without temporary interfaces that would be
deleted again. So: (1) the remaining small seams first, each landable
alone: the allocator (961), then the sinks (LED, button, display,
battery) as traits with host fakes (the feedback sink, LED and
buzzer, done in 973 on 2026-10-25; the button, an input seam, in 976,
the display in 980 and the battery in 991, the same day: part (1) is
done); (2) the BLE
link as a message seam (step 4, the largest); (3) the whole cluster in
ONE move on a branch (a lane-2 clone), with the transceiver seam (step 3)
and the PN store (step 6) as commits inside it and the control executor
(step 2) last, landing only after the rig has measured the branch green;
(4) step 7, one node loop, as its own order per binary, with a date.
Every module moves once, every seam is cut once at the place the census
shows, with a host fake and a second implementer, and the bench
measures one large move instead of seven intermediate states. Not
taken: moving `lora.rs` now (temporary interfaces), or pausing #469
(the M9 arrived on 2026-10-24 and needs exactly these seams). The step
numbers below are kept as names; the last sentence of each row says
where the step sits in that order.

| step | does | seams | pin | target |
|---|---|---|---|---|
| 0 | the embassy pin rule across the three families (ours: embassy-executor 0.9 / embassy-sync 0.7; the fork: 0.10 / 0.8 plus a `[patch.crates-io]` git rev in the fork's `leviculum-fw/Cargo.toml`, lines 146 to 154); the log-diff masking script; the crate skeleton in the `leviculum-nrf` workspace (excluded from the root, so the guards below live there) | none | builds; the guards below green on an empty crate | - |
| 1 | Flash, Entropy, Clock, Watchdog, Sinks (debug log, post-mortem, LED, buzzer, display, button) as traits with host fakes; `boot_count.rs`, `rng.rs`, `log.rs`, `lib.rs`'s post-mortem, `time_floor.rs`, `record_store.rs`'s policy half move Since 2026-10-24 order part (1): the sinks (LED, button, display, battery) are the small seams still to cut here, after 961's allocator. On 2026-10-25 973 cut the feedback sink (LED, notification LED, buzzer), 976 the button (the `ButtonInput` seam, the press handling in `leviculum_firmware::button`), and 980 the display (the `Panel` seam, the page and its power in `leviculum_firmware::display`, both panels at once), and 991 the battery (the `BatteryGauge` seam, the sampling task and its lines in `leviculum_firmware::battery`). All small seams cut: feedback 973, button 976, display 980, battery 991. | 5 | census, the boot-count and time-floor host tests, one boot on each board (the rig's `boot-proof-both.sh`, which lives outside this tree) | - |
| 2 | runs after steps 5 and 6 (957, by the census above): the control-frame executor out of `usb.rs` into stratum 2 (the fork's `control.rs` as the template), `usb.rs` shrinks to the port; the `ControlPort` trait is declared when the executor moves, not before Since 2026-10-24 order part (3): the last commit inside the cluster branch. | Control port | lnflash round trips over every control frame, the serial-mux host tests | - |
| 3 | Transceiver as a message seam; `lora.rs` and `radio.rs` move; `sx1262.rs` and `leviculum-lr11xx` stay below Since 2026-10-24 order part (3): a commit inside the cluster branch, not a move of its own. | Transceiver | `t1000e_lora_pair`, `t1000e_rnode_pair` with the witness, the `#438` cells; `lora_pn_board_sync` only with its register standing stated (it is in periculum's `hardware/KNOWN-REDS.toml`, retired twice) | - |
| 4 | BLE link as a message seam; the upward calls of `ble/mod.rs` and `columba.rs` become messages; `ble/` stays the SoftDevice backend Since 2026-10-24 order part (2): the next large seam after the small ones, landing alone. On 2026-10-25 998 measured the call surface and landed the first slice, `leviculum_firmware::ble_link`: the carrier gate the media profile pushes (`set_active` replaces the backend's upward `media::ble_active` read and `note_media_changed`), the tx-gap value, the drain/link table (`HVN_DRAIN`) and the held-bytes census the backend publishes; the GAP and advertisement name is `init`'s argument. The measurement found the command half is three latest-value states, not messages — the per-fragment work stays direct calls on `ble_link`'s values. On 2026-10-25 999 landed the second slice: the node-facing channels and the peer/inbound events move up as `seams::BleLinkEvent` (the backend reports through `ble_link::deliver`), `BleInterface` moves into `ble_link` with the registry's distinct-peer count as a published value, and the `[MEDIA]` drop-run and held-down lines get their stratum-2 home (`leviculum_firmware::media`, shared with `lora.rs`) — `ble/*`'s upward code edges now all go through `leviculum_firmware` (the census's remaining `media`/`radio_store`/`transport_stats` mentions are doc-only). **Step 4 done on 2026-10-26 (1003)**: the link table moved up (`ble_link::LinkTable`, fed by `PeerUp`/`PeerLost`, read by `BleInterface::peer_count`; the published count of 999 is gone), the slot map stayed below, and `trait BleLink` has its final shape: `send(aim, packet)`, where `Flood` is resolved by the backend over all claimed slots (the reviewer's decision on 999, so a link that has not handshaked keeps receiving floods) and `Peer`/`FloodExcept` hand identities down to the same `plan_fanout` scan as before, and `set_advertised_name`, called once at bring-up. Carrier on/off and the pacing gap stay pushed values. `fakes::FakeBleLink` runs the real `PeerRegistry` and `DrainRouter` and drives the four host tests (flood with a pre-handshake link, targeted send, lost link, the #412 duplicate). What the seam now admits besides TrouBLE: a BT5 broadcast backend (extended advertising on the Coded PHY), which implements `send` over its own carrier and reports peers through the same events. | BLE link | the BLE room cells, `ble_pn_board_upload`, a two-phone session | - |
| 5 | the orchestration cluster moves. In the census these modules are not pure: `announce`, `banner`, `button`, `chtm`, `events`, `feedback`, `heap_census`, `lora`, `media`, `mgmt`, `name`, `pn`, `telemetry` and `transport_stats` form one strongly connected set of 22 together with the backends that call back into them (`ble`, `usb`, `boards`, `radio`, `radio_store`, `record_store`, `sx1262`, `led`), and `remote_status` and `panel_facts` (the display's page facts, since 980; `display` itself before) reach into it; so no member moves alone, and the cluster moves once those backend edges, the `lib.rs` helpers and the sinks are seams (steps 3, 4 and 1). Of the six leaves that reached none of it, five moved in 957: `iface_bytes` (its name table stayed), `dispatch`, `clock`, `boot_trace` (its reset-cause register stays) and `identity`; `interface` did not, it read the name table. 958 moved three things that needed no new seam: the serial interface, whose `name()` reads `names()` (`leviculum-nrf/firmware/src/interface.rs:39`), once the name table was dissolved into the board files (`IFACE_NAMES`, handed to `leviculum_firmware::iface_bytes::set_names` at boot), and the time-source state, into `clock`; and it switched `announce`, `heap_census` and `pn` to name the heap-budget constants from their crate rather than through `ble`. 959 moved the board state (`baseboard`, renamed `board_state` in 961) and gave Entropy its stratum-2 type, so neither `board_state` nor `rng` holds a cluster file any more. What still holds the cluster down is `scripts/nrf-seam-vocabulary.txt`. `telemetry.rs` with the reporter moved into `leviculum-lxmf` behind a `reporter` feature, as the fork did. 953 took the fork's first two halves of this: the byte and speed arithmetic is `leviculum_core::interface_counters` and the `/status` matching and sending is `leviculum_core::node::remote_status`, so `iface_bytes` and `remote_status` keep only the board's table, locks and rows Since 2026-10-24 order part (3): the whole cluster in one move on a lane-2 branch, landing after the rig measured it green. | Sensors | the telemetry cells, 914's double press, `/status` round trips | - |
| 6 | `pn.rs` over Flash and the PN store Since 2026-10-24 order part (3): a commit inside the cluster branch. | none new | `lora_pn_board_sync_t1000e`, the PN census, the stock-client cells | - |
| 7 | **the node loop, once**: the four binaries' loops are diffed line by line; every difference is either a board fact (moves to the board file) or a behaviour decision (named in the report, decided before it is unified); the loop moves to stratum 2 and `bin/*.rs` become wiring Since 2026-10-24 order part (4): one order per binary, each with a date. The diff is `docs/drafts/node-loop-diff-2026-10.md` (1017); Lew decided its D1 to D7 on 2026-10-28. On 2026-10-28 1085 extracted the skeleton into the bind crate first, `leviculum_nrf::node_loop` (`enter`, `pre_usb`, `boot_banner`, `build_node`, `lora_up`, `ble_up`, `log_post_init`, `run`), because the loop still names `lora`, `pn`, `telemetry` and the other cluster modules of step 5; the T114 moved onto it, its boot lines pinned against the pre-move binary (`leviculum-std/tests/t114_boot_lines.rs`). The solar node, the T1000-E and the RAK4631 follow, one order each. | none new | every rig cell of the four boards, before and after; this is the riskiest step and gets its own order per binary | order written by (date: reviewer) |
| 8 | the ESP32-S3 and RP2350 bind crates onto the extracted crate; `leviculum-fw` retires module by module, its backends adapted to the seams | none new | the fork's bench results re-run on the M9 and the RedBoard | - |

Out of scope for every step: a behaviour change under the cover of a
move. In scope as its own mechanical step, early: moving the thirty-five
policy crates out of the `leviculum-nrf/` path ([the problem,
measured](#the-problem-measured) says why the path is wrong; the fork
and the daemons both import them from there).

## What is taken from the fork now, independent of the extraction

As PRs by crate against master, each with its own gates (audit 927
lists the file spans):

1. Core hardening: resource parts bounded by the SDU and the remaining
   transfer, request-id exactly 16 bytes, an announce longer than the
   base MTU refused; each PR text carries the three-part deviation
   argument.
2. Bounded BLE reassembly (`framing/ble.rs`, `ble_reassembly_budget.rs`,
   the nRF and lnsd instantiations), with a BLE room cell before and
   after.
3. The boot-count torn-slot scan fix.
4. The host-tested member crates: `serial-mux`, `rtc-pcf8563`,
   `battery-scale` (MAX17048), `gnss-init` (Airoha),
   `record-log::legacy`, `ble-tx`'s `pump_fragments`.
5. `leviculum-sd-spi` and `leviculum-sd-store` as a Flash-seam backend
   after step 1; the core `sd-storage` control plane and the lnflash
   provisioning with them.
6. `leviculum-net` as the WiFi families' stratum-3 crate, with the
   `net_config` codec moved there out of core (audit 927, isolation
   finding).

Not taken as-is: `leviculum-fw` as a layer ([the target](#the-target-three-strata));
`leviculum-ble5` and the vendored TrouBLE copy (the CYW43439 could not
prove periodic advertising on the fork's own bench); the lnpnd owner
shortcut in `admit_control` without a stated decision; the 100-plus
non-test `expect` in the new crates.

## Gates the new stratum brings

- `firmware-no-hal`: `cargo tree` of `leviculum-firmware` carries no
  `embassy-nrf`, `nrf-softdevice`, `esp-hal`, `embassy-rp`, `cyw43`,
  `trouble-host`; precedent `scripts/check-nrf-evt-max-size.sh:42`;
  positive-controlled with an injected dependency. Built in step 0:
  `scripts/check-firmware-no-hal.sh`, in `just guards`.
- `firmware-no-chip-cfg`: no `cfg(feature = "bsp-` and no chip name in
  stratum 2 sources. The existing source-invariant tests are
  root-workspace targets (`scripts/source-invariant-targets.txt`) and
  `leviculum-nrf` is excluded from the root (`Cargo.toml:59`); the new
  crate lives in the `leviculum-nrf` workspace, so this guard is a
  script under `just guards`, not a root-workspace test. Built in step 0:
  `scripts/check-firmware-no-chip.sh`; doc comments are exempt, so a
  seam's doc can name its implementers.
- The census unchanged, in both its forms: the compile-time size
  asserts in `lint-nrf` and `nrf-stack-frames` (no board), and the
  runtime `[HEAP_CENSUS]` line (a board). A step's commit message states
  the before and after numbers; a difference is measured, never
  estimated.
- The log diff of [the extraction](#the-extraction-bottom-up), attached
  to every step's report.

## Risks and open questions

- Flash and stack on the nRF52840 (flip-link, the 96 KiB heap, `#50`)
  are the budget that says no first. The message seams add channels,
  the trait seams add nothing at runtime; the figures per step are
  measurements, none is given here.
- The SoftDevice is not a controller behind a thin trait; step 4 may
  end with a wider BLE link vocabulary than TrouBLE needs. Decide at
  step 4 with both implementers on the table, not before.
- The node loop (step 7): four copies, each pair differing by between
  about 370 and 650 lines; every difference is a decision. This is
  where "zero behaviour change" is hardest to keep and where the review
  time goes.
- Cedric Rivard: the plan asks the fork to retire its layer and keep
  its backends and leaf crates. Whether that happens is Cedric's call,
  and Lew's conversation.
- Time: steps 0 to 7 are weeks of coder passes beside the main lane.
  The M9 port needs exactly these seams, so the work is not a detour;
  it is still second-lane work under `rule:workflow-priorities`.

## Decision record

- 2026-10-23, Lew: no duplicated code, the best architecture; the
  fork's functionality is wanted, its code is not owed; take parts, be
  inspired, need not be immediate.
- 2026-10-23: extraction over adoption ([the
  target](#the-target-three-strata)); two seam shapes, message and
  trait, with ecosystem traits where they exist ([the
  seams](#the-seams-and-their-shape)); bottom-up order with the node
  loop last ([the extraction](#the-extraction-bottom-up)); the fork's
  control-frame cut adopted.
- 2026-10-23, review of the first draft: five facts corrected (crate
  count, line counts, binaries, the node loop's location, the fork's
  missing transceiver seam), the step order inverted, four seams added
  (entropy, watchdog, debug log, post-mortem), the embassy rule moved to
  step 0.
- 2026-10-23: this page is tracked by Codeberg #469 (firmware layers);
  the fork integration by Codeberg #470.
- 2026-10-24, reviewer: step 2 re-placed after steps 5 and 6 by
  measurement. The fork's `ControlPort` (its `control.rs`, lines 166 to
  188) is the port only; its executor reaches fourteen sibling modules of
  `leviculum-fw`, and ours (`retic_serial_task`, `usb.rs`) dispatches
  `ControlAction` into `mgmt`, `media`, `name`, `telemetry`, `lora`,
  `pn`, `ble`, `chtm`, `identity`, `iface_bytes` and `record_store`. An
  executor in stratum 2 now would need a trait method per call, deleted
  again as those modules arrive. The standing evidence is
  `scripts/nrf-module-reach.txt`; five of the six free leaves moved
  first (957), `interface` stays on the bind crate's name table.
- 2026-10-24, reviewer (958): the interface names are a board fact,
  a table in each board file set once at boot; the T1000-E says
  `lora_lr1110`, the three SX1262 boards keep `lora_sx1262`, and its node
  registration and `[ANNOUNCE_CAP]` line read the same table, so the
  `lora_lr11xx` its registration said before is gone too.
  `scripts/nrf-seam-vocabulary.txt` is the standing evidence for steps 3
  to 6: what each still-bound module is held by, item by item.
- 2026-10-24, reviewer (959): board state is stratum 2 state fed by
  backend tasks (`leviculum_firmware::board_state`, behind the capability
  features `gnss`, `battery`, `display`); Entropy is a stratum-2
  zero-sized type over a set-once source (`entropy::set_source`); the
  set-once pattern, the interface names and now the entropy source, is
  the standing shape for what the bind crate hands up at boot.
- 2026-10-24, Lew (plan B): the order of [the
  extraction](#the-extraction-bottom-up). The remaining small seams
  first, each landable alone (the allocator and the radio-settings
  store, then the sinks as traits with host fakes); the BLE link as a message seam (step 4); the whole
  cluster in one move on a lane-2 branch, with the transceiver seam
  (step 3) and the PN store (step 6) as commits inside it and the
  control executor (step 2) last, landing only after the rig measured
  the branch green; step 7 as its own order per binary, with a date.
  Every module moves once and every seam is cut once, where the census
  shows it, with a host fake and a second implementer. Not taken:
  moving `lora.rs` now (temporary interfaces), pausing #469 (the M9
  arrived that day and needs these seams). Also Lew, the same night:
  `baseboard` is renamed `board_state` at once, because the RAK19026
  carrier is a chip-family fact inside the layer that must know none.
- 2026-10-25, coder (961): the allocator, `heap_stats`,
  `heap_watermark_sample` and the `[HEAP]` loop are stratum 2's
  (`leviculum_firmware::heap`); the pool's memory and the executor task
  stay in the bind crate, and the allocator attribute is on
  `target_os = "none"` only, so a host build keeps the system allocator.
  The radio-settings store did not move: its persist bookkeeping
  (`PersistSlot`, `PendingSave`, `confirm`) is `telemetry.rs`'s own
  Embassy wrapper around `leviculum-persist-ack`, with the bounded wait
  and its `[TELEMETRY]` line, so `radio_store` is cluster material and
  moves with `telemetry` in part (3).
- 2026-10-25, coder (980): the display's page and power are stratum 2's
  (`leviculum_firmware::display`), over a `Panel` that lends its drawn
  buffer and pushes it; both drivers stay in the bind crate, which also
  fills the page's `StatusFacts`. The orchestration crate's `display`
  capability now means "the board has a status panel", so `bsp-t114`
  forwards it too; the bind crate's own `display` feature stays the
  Pocket's SSD1306 driver.
- 2026-10-25, coder (991): the battery monitor is stratum 2's
  (`leviculum_firmware::battery`), over a `BatteryGauge` whose one method
  is a pack reading in millivolts; the SAADC, the divider's enable and
  settle and the board's `BatteryScale` stay with the implementation in
  the bind crate, which also hands up the `[BAT] init` line's two gauge
  facts. With it plan B part (1) is done: all small seams are cut.
- Open: the BLE link vocabulary; whether the policy crates' path move
  comes before or after step 1. (The order of steps 3 to 7, open until
  2026-10-24, is closed by plan B above.)

## See also

- [Interface isolation](interface-isolation.md), the rule this page
  carries down to the chip.
- [How far one firmware build reaches](board-support-scope.md), for the
  board families a bind crate serves.
- [The firmware's host-test seam](firmware-host-test-seam.md), for how
  the policy crates are tested without a board.
- [The ThinkNode M9](thinknode-m9.md), the ESP32-S3 handheld that needs
  these seams first.
