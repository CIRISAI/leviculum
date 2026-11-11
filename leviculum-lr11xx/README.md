# leviculum-lr11xx

Async `no_std` driver for Semtech LR11xx LoRa transceivers (LR1110, LR1120,
LR1121), generic over `embedded-hal` 1.0 / `embedded-hal-async` 1.0, with
timeouts from `embassy-time` 0.5.

## 1. Scope

- LoRa only: no GFSK, LR-FHSS, GNSS scanning, WiFi scanning or sleep.
- Sub-GHz only: image calibration uses the 430-440, 470-510, 779-787,
  863-870 and 902-928 MHz bands, and the shipped PA table drives the LP and
  HP PAs. `PaSel::Hf` (the LR1120/LR1121 2.4 GHz PA) is expressible in a
  `Board`, but the LoRa path never selects 2.4 GHz.
- `Board::chip` accepts LR1110, LR1120 or LR1121. Only an LR1110 board,
  `board::LR1110MB1DXS`, is shipped and verified.

## 2. Hardware interface

| Signal | Direction | Driver type | Notes |
| --- | --- | --- | --- |
| SPI (SCK, MOSI, MISO, NSS) | MCU ↔ LR11xx | `SpiDevice` | Mode 0 (CPOL 0, CPHA 0), at most 16 MHz (LR1110 datasheet). The `SpiDevice` owns NSS. |
| BUSY | LR11xx → MCU | `InputPin + Wait` | High while the part cannot take a command. |
| DIO9 (IRQ) | LR11xx → MCU | `InputPin + Wait` | Level-high while a routed interrupt is pending. |
| NRESET | MCU → LR11xx | `OutputPin` | Active low. |

On a shared bus, give the LR11xx `SpiDevice` its own chip select and its own
clock setting. Other devices on the bus may run faster than 16 MHz.

Every command waits for BUSY low first. A read uses two transactions, as in
SWDR001 `lr11xx_hal_read`: the opcode and arguments, then, after BUSY falls,
a transfer of NOPs that clocks out Stat1 and the response. `GetStatus` is a
single transfer of six NOP bytes that returns Stat1, Stat2 and the 32-bit
interrupt word.

## 3. Layers

- `cmd`: opcodes, interrupt bits, `GetErrors` bits, and pure argument
  encoders and response decoders. Each encoder is unit-tested against the
  byte layout in SWDR001 v3.0.0.
- `board`: the board facts as typed values (`Board`, `Chip`, `Tcxo`,
  `TcxoVoltage`, `RfSwitch`, `RegMode`, `LfClk`, `PaTable`,
  `RssiCalibrationSet`) and the `LR1110MB1DXS` constant.
- `driver`: `Lr11xx<SPI, NRST, BUSY, IRQ>`, the async driver. It implements
  `leviculum_rx_arming::{RxPort, RxWindowProbe}` for the receive-arming
  policy.

```rust
let mut radio = Lr11xx::new(spi, nreset, busy, dio9, &board::LR1110MB1DXS);
radio.set_log(my_log); // optional: fn(&'static str, core::fmt::Arguments)
radio.reset().await?;
let version = radio.init_radio().await?;
let tx = radio.configure_lora(869_525_000, 8, 0x04, 0x01, 14, 18).await?;
radio.transmit(&frame, 2_000).await?;
```

`configure_lora` takes the SX126x LoRa codes for bandwidth and coding rate.
The LR11xx shares those codes (SWDR001 `lr11xx_radio_types.h`).

## 4. Initialisation sequence

`reset()` drives NRESET low for 5 ms, waits 10 ms, then waits up to 500 ms
for BUSY to fall. `init_radio()` then runs SWSD003 v2.4.0
`apps_common_lr11xx_system_init` (`lr11xx/common/apps_common.c`), followed by
the LoRa radio settings:

1. `GetVersion`, classified in this order:
   - type `0xDF` gives `Bootloader`;
   - a type equal to `Board::chip` continues;
   - a different LR11xx type (1-3) gives `WrongChip`;
   - any other type gives `NoRadio`.

   Then Stat1 `Fail` or `Perr` gives `Command`.
2. `SetRegMode(board.reg_mode)`
3. `SetDioAsRfSwitch(board.rf_switch)`
4. `SetTcxoMode(voltage, startup_steps)`, if `board.tcxo` is `Some`.
5. `CfgLfClk(board.lf_clk, board.wait_32k_ready)`, then a BUSY wait of up
   to 1 s.
6. `ClearErrors`
7. `Calibrate(0x3F)`, all blocks, then a BUSY wait of up to 500 ms.
8. `GetErrors`. Stat1 `Fail`/`Perr` gives `Command`, and a nonzero word
   gives `Calibration { errors }`.
9. `ClearErrors`
10. `ClearIrq(all)`
11. `SetPacketType(LoRa)`
12. `SetRxTxFallbackMode(standby RC)`
13. `SetRxBoosted(board.rx_boosted)`
14. `SetLoRaPublicNetwork(private)`

A calibration error is fatal. The bits that matter at bring-up are
`HF_XOSC_START` (bit 5), usually a wrong TCXO voltage or start-up time, and
`LF_XOSC_START` (bit 6), `LfClk::Xtal` on a board without a 32.768 kHz
crystal.

`configure_lora` first stands down any receive window, then:

1. `CalibImage` for the band containing the frequency, only when the band
   differs from the last one calibrated. `init_radio` forgets the band.
2. `SetRfFrequency`
3. `SetRssiCalibration` from `board.rssi_calibration.for_freq(freq)`, on
   every call, if the board has a calibration set.
4. `SetPaCfg` and `SetTxParams` from the PA table row (clamped to the
   table), `SetModulationParams` (LDRO from `leviculum_core::sx126x`),
   `SetPacketParams`, `StopTimeoutOnPreamble(true)`.

## 5. Board description

| Field | Command | How to derive it from a schematic |
| --- | --- | --- |
| `name` | — | Free text, used in logs. |
| `chip` | `GetVersion` check | The part fitted. |
| `tcxo` | `SetTcxoMode` | `Some` if a TCXO is powered from the VTCXO pin. Set `voltage` to the TCXO's supply rating and `startup_steps` to its start-up time in 32.768 kHz steps (30.5 µs each). `None` for a 32 MHz crystal. |
| `rf_switch` | `SetDioAsRfSwitch` | Which of DIO5/6/7 (RFSW0/1/2) drive the RF switch, and their levels in standby, RX, LP TX, HP TX, HF TX, GNSS and WiFi, from the switch's truth table. |
| `reg_mode` | `SetRegMode` | `DcDc` if the DC-DC inductor is fitted, else `Ldo`. |
| `lf_clk` / `wait_32k_ready` | `CfgLfClk` | `Xtal` if a 32.768 kHz crystal sits on DIO10/DIO11, `Ext` for an external 32 kHz clock, otherwise `Rc`. |
| `rx_boosted` | `SetRxBoosted` | Firmware choice: more sensitivity for more RX current. |
| `rssi_calibration` | `SetRssiCalibration` | Per-design tables, or `SEMTECH_RSSI_CALIBRATION` for a Semtech reference layout. |
| `pa` | `SetPaCfg`, `SetTxParams` | One `Row` per dBm from `min_dbm`, from the design's PA characterisation. HP rows need the HP PA supply (VBAT) and its current. |

`PaTable::new` refuses an empty table, or a table whose top row would be
above +127 dBm. In a `const` initialiser that is a compile-time error.
`pa_for_dbm` clamps to the table and cannot index out of range.

### LR1110MB1DIS / LR1110MB1DJS (`LR1110MB1DXS`)

Values come from SWSD003 v2.4.0 `libs/smtc-shields/lr11xx/src/`. The
evidence is the schematic and BOM of reference design e516v03a.

| Field | Value | SWSD003 v2.4.0 | Board evidence (e516v03a) |
| --- | --- | --- | --- |
| `chip` | `Lr1110` | — | LR1110 fitted |
| `tcxo` | `V3_0`, 300 steps | `smtc_shield_lr11x0_common.c:86-90` | TCXO Q2 (NDK NT2016SF, END4263D) on VTCXO; 32 MHz crystal Q1 not fitted |
| `rf_switch` | enable RFSW0-2; RX RFSW0; TX RFSW0+RFSW1; TX HP RFSW1; GNSS RFSW2 | `smtc_shield_lr11xx_common.c:121-130` | SKY13588 with DIO5 = V1, DIO6 = V2. V1/V2 = 1/0 is RX (J2), 1/1 is LP TX (J3), 0/1 is HP TX (J1). DIO7 enables the BGA524N6 GNSS LNA. |
| `reg_mode` | `DcDc` | `smtc_shield_lr11xx_common.c:195-198` | L1, a 15 µH DC-DC inductor, fitted |
| `lf_clk` | `Xtal`, wait for ready | `smtc_shield_lr11xx_common.c:132-135` | Q3, a 32.768 kHz crystal (NX3215SA), on DIO10/DIO11 |
| `rx_boosted` | `false` | `lr11xx/common/apps_configuration.h:73-74` | — |
| `rssi_calibration` | `SEMTECH_RSSI_CALIBRATION` | `smtc_shield_lr11xx_common.c:57-119,173-188` | — |
| `pa` | -17 to +22 dBm, 40 rows | `smtc_shield_lr1110mb1dxs_common.c:55-416` | VBAT = VDD_RADIO = host 3.3 V; the HP rows (+16 dBm and up) run the HP PA from VBAT |

The shield's LNA_CTRL_MCU header pin is not connected to anything on
e516v03a.

### Worked example: ThinkNode M9 (incomplete)

The M9 (ESP32-S3 with an LR1110) has a provisional `Board` constant since
order 1478, `leviculum_esp::boards::thinknode_m9::LR1110`, which states its
choices field by field. These values are known from Meshtastic's
`ELECROW-ThinkNode-M9` variant:

| Field | Value |
| --- | --- |
| `chip` | `Lr1110` |
| `tcxo` | `TcxoVoltage::V3_3`, about 164 steps (RadioLib's 5 ms default) |
| `rf_switch` | enable RFSW0+RFSW1; standby 0; RX RFSW0; TX RFSW0+RFSW1; TX HP RFSW1; GNSS 0; WiFi 0 |
| Pins | NSS 39, BUSY 41, IRQ (DIO9) 42, NRESET 45 |
| SPI | SCK 40, MOSI 47, MISO 38. The bus is shared with the LCD (CS 16) and the SD card (CS 48), which Meshtastic clocks at up to 75 MHz, so the LR1110 device needs its own clock of at most 16 MHz. |

Bench-probed on one M9 (LR1110 fw 0x0303): `RegMode::DcDc` with `LfClk::Rc`
and with `LfClk::Xtal` (`wait_32k_ready`) both init with a clean `GetErrors`,
so a 32.768 kHz crystal is fitted. Still unknown without the Elecrow
schematic: whether the DC-DC inductor is fitted (Meshtastic runs LDO), the
PA table and the exact TCXO start-up time. Both PA paths are wired and the
firmware uses 22 dBm; the PA table stays unknown until the schematic or the
FCC report. A wrong guess usually fails at init: a wrong TCXO voltage
shows up as `Calibration(errors=0x0020)`, and
`LfClk::Xtal` without a crystal as `Calibration(errors=0x0040)`.

## 6. Receive model

- `arm_rx(timeout_ms, site)` reopens the payload length to 255 if a
  transmit narrowed it (an explicit header longer than the programmed length
  ends in `HEADER_ERROR`), routes `RX_DONE | CRC_ERROR | TIMEOUT |
  HEADER_ERROR` to DIO9, clears IRQs and issues `SetRx`.
  - `timeout_ms == 0` is single mode: the window runs until a frame
    arrives, with a 60 s software bound.
  - Any other value is a timed window. The RTC count saturates at
    `0xFFFFFE`, because `0xFFFFFF` would mean continuous RX.
  - `StopTimeoutOnPreamble` keeps a reception that has started from being
    cut by the hardware timeout.
- How a window ends:
  - `RX_DONE` reads the payload. A frame longer than the caller's buffer
    gives `BufferTooSmall` without a `ReadBuffer8`.
  - `RX_DONE | CRC_ERROR` gives `Crc` with the length, RSSI and SNR. The
    payload stays in the chip.
  - `TIMEOUT` gives `Timeout`. The chip has already fallen back to standby.
  - `HEADER_ERROR` alone ends the window with `HeaderError`: standby, then
    a clear, as in SWSD003 `main_ping_pong.c`.
- Extension and deferral: when the software bound expires with a preamble or
  header latched, the wait is extended by the frame time computed in
  `leviculum_core::sx126x::rx_extend_ms`. A transmit or CAD that would
  replace a standing window goes through
  `leviculum_rx_arming::stand_down_for_tx`:
  - `DeferPolicy::OneFrame` waits out one frame.
  - `DeferPolicy::ReleaseFalsePreamble` releases a bare preamble after the
    programmed preamble and header time.
  - A completed frame is harvested into the caller's `FrameSink`, not
    discarded.
- Cancellation: `wait_rx_ready()` only waits for DIO9 or the software bound,
  so it can be cancelled in a `select`. `read_ready_rx(buf)` settles and reads
  out the window and must not be cancelled. `await_rx(buf)` runs both halves.
- Channel sampler: `set_channel_sampler(period, f)` reads the interrupt word
  and `GetRssiInst` every `period` during receive waits, without clearing
  IRQs or leaving RX. A failed read publishes no sample. Sample reads count
  against the same absolute deadline.

## 7. CAD

`cad(sf, buf, sink)` stands down a standing window first, with
`ReleaseFalsePreamble`. It then issues `SetCadParams` with:

- 4 symbols up to SF9 and 8 from SF10. The LR11xx takes a plain symbol count,
  not the SX126x's encoded field;
- detection peak 50 and minimum 10, from SWSD003 v2.4.0
  `lr11xx/apps/cad/main_cad.h`;
- exit to standby RC.

Then `SetCad`. The result is `true` when `CAD_DETECTED` is set. The timeout is
`leviculum_core::sx126x::cad_timeout_ms` for the programmed bandwidth.

## 8. Errors

| Variant | Meaning |
| --- | --- |
| `Spi` | The `SpiDevice` returned an error. |
| `Gpio` | NRESET, BUSY or DIO9 returned an error. |
| `Busy` | BUSY stayed high past its bound: 100 ms per command, 500 ms after reset or a calibration, 1 s after `CfgLfClk`. |
| `Timeout` | No TX done, no CAD done, or a receive window ended without a frame. |
| `Crc(CrcErrFrame)` | Payload CRC failed. Carries the length, RSSI and SNR. |
| `NotArmed` | A receive half was called with no window standing. |
| `NoRadio { hw, kind, fw }` | `GetVersion` type is neither an LR11xx nor the bootloader (absent part: `0x00` or `0xFF`). |
| `Bootloader { hw, fw }` | Type `0xDF`: the bootloader is running. The part has no valid firmware, or its strapping held it in the bootloader. |
| `WrongChip { expected, hw, kind, fw }` | An LR11xx other than `Board::chip` answered. |
| `Command(CmdStatus)` | Stat1 reported `Fail` or `Perr` for `GetVersion` or `GetErrors`. |
| `Calibration { errors }` | `GetErrors` was nonzero after `Calibrate`. The bits are in `cmd::errors`. |
| `HeaderError` | The receive window ended on a LoRa header error. |
| `PayloadTooLong { len }` | `transmit` got more than 255 bytes. Nothing was sent on the bus. |
| `BufferTooSmall { len }` | A received frame of `len` bytes does not fit the buffer. |

## 9. Limitations

- **High ACP.** SWDR001 v3.0.0 README, "Known limitations": LR1110 firmware
  0x0303-0x0307 and LR1120 0x0101 show high adjacent-channel power after
  waking from sleep with retention. The workaround clears bit 30 of register
  `0x00F30054`. It does not apply here, because the driver never issues
  `SetSleep`.
- **Image calibration** runs on the first `configure_lora` and again whenever
  the frequency moves to another band. A retune within a band reuses it.
- **RSSI calibration.** `SEMTECH_RSSI_CALIBRATION` is Semtech's table for
  its reference shields. Other layouts need their own measured tables, or
  `None`.
- **Unknown boards.** A board whose values differ from the shipped constant
  needs its own `Board` (for example LDO, no 32 kHz crystal, or a different
  TCXO or RF switch).

## 10. Verification status

- **Host:** `cargo +1.97 test --target x86_64-unknown-linux-gnu`. Covers the
  encoders against SWDR001 v3.0.0 byte layouts, the board tables, and full
  command sequences on a scripted SPI bus with a mock clock: init and its
  refusals, image-band tracking, TX, RX, CRC and header errors, CAD, the
  extension and deferral policy, sampling, and reset timeout.
- **LR1110MB1DIS (e516v03a, LR1110 fw 0x0303) on an RP2350 host:** init, TX,
  CAD and the RSSI noise floor.
- **ThinkNode M9 (LR1110 fw 0x0303, ESP32-S3), provisional board values
  above, 914.875 MHz SF8 BW125:** init, CAD, TX received by the RedBoard
  (SNR 11-12), and over-the-air RX of the RedBoard's 200-byte frames (RSSI
  -91 to -96 dBm, SNR 14-16). The receive interrupt word read
  `0x38` (`RX_DONE | PREAMBLE_DETECTED | SYNC_WORD_HEADER_VALID`), and the
  channel sampler saw `header=true`: flags not routed to DIO9 do read back
  through `GetStatus`. Header errors were observed on hardware: before the
  payload-length fix every received frame longer than the last transmitted
  one ended in `HeaderError` (flags `0x50`).
- **ESP32-S3 build:** `cargo +esp check --target xtensa-esp32s3-none-elf
  -Z build-std=core,alloc`; `alloc` is needed for the `leviculum-core`
  dependency.

## 11. References

- Semtech SWDR001 v3.0.0, LR11xx driver:
  <https://github.com/Lora-net/SWDR001/tree/v3.0.0>
- Semtech SWSD003 v2.4.0, LR11xx SDK and shield files:
  <https://github.com/Lora-net/SWSD003/tree/v2.4.0>
- LR1110 datasheet DS.LR1110.W.APP and user manual UM.LR1110.W.APP:
  <https://www.semtech.com/products/wireless-rf/lora-edge/lr1110>
- LR1121 user manual UM.LR1121.W.APP rev 1.2
- LR1110MB1DIS shield, reference design e516v03a (schematic and BOM), design
  files under the kit page:
  <https://www.semtech.com/products/wireless-rf/lora-edge/lr1110mb1lbks>
- Skyworks SKY13588-460LF datasheet:
  <https://www.skyworksinc.com/-/media/SkyWorks/Documents/Products/2201-2300/SKY13588_460LF_203512D.pdf>
