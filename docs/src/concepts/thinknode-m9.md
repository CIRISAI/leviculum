# The ThinkNode M9

The Elecrow ThinkNode M9 is the project's first handheld and its first
ESP32 target to carry the full stack. Lew decided on 2026-10-21, a few
days before the unit arrived, what it is for. This page records that
decision, what exists today, the bring-up plan, what the T1000-E taught
that applies here, and what is still open. The messenger and the map
it is for have their own page ([A messenger whose core runs
anywhere](handheld-messenger.md)).

## The device

| Part | What the M9 carries |
|---|---|
| MCU | ESP32-S3R8: 512 KB SRAM, 8 MB PSRAM, 16 MB NOR flash |
| Storage | microSD slot, a card with map tiles in the box |
| Radio | Semtech LR1110, EU868 |
| Display | 2.4-inch 240x320 ST7789, no touch |
| Input | 37-key QWERTY keyboard behind its own controller |
| Sensors | GNSS, RTC, magnetometer, IMU |
| Other | buzzer, 2300 mAh battery, USB-C through a USB-UART bridge |

## The hardware from the public record

No schematic is public. Elecrow's wiki quotes the V1.0 schematic, and
Elecrow's technical support is the address for it. Until it is here,
the facts below come from five public sources, and each one names the
source it rests on:

- **the wiki**: Elecrow's ThinkNode M9 wiki, Meshtastic edition,
  document version of 2026-08-26, describing "ThinkNode M9 V1.0";
- **the product page**: Elecrow's shop page and its PDF on formatting
  the microSD card;
- **the Meshtastic variant**: `variants/esp32s3/ELECROW-ThinkNode-M9/`
  in the Meshtastic firmware, develop branch;
- **the MeshCore variant**: `variants/thinknode_m9/` in MeshCore, main
  branch;
- **the FCC filing**: FCC ID 2BDNA-TNM9, Shenzhen Elecrow Limited, last
  application 2026-08-10. Only a search snippet was read: the filing
  sits behind a bot wall, so its exhibits (test report, photos) have not
  been read, and the list of exhibits itself is likely, not confirmed. No
  CE document was found.

The two variants were read on 2026-10-21 and again on 2026-10-26 (the
Meshtastic variant at commit 38ae9aab of 2026-10-16), the wiki on
2026-10-26. None of these sources is the
hardware reference (rule `feedback_schematic_is_the_hardware_reference`),
so where they disagree this page keeps the disagreement and does not
pick a side. The device settles it.

### Storage

- **A microSD slot (TF) exists.** The wiki shows it, and both variants
  drive it: chip select on GPIO48 (the Meshtastic variant's build flags,
  the MeshCore variant's `SDCARD_CS`).
- The card shares one SPI bus with the LR1110 and the LCD: SCK 40,
  MOSI 47, MISO 38 (both variants). Only one of the three chip selects
  may be low at a time; the Meshtastic variant drives all three high
  before anything else runs.
- The product page's "16 GB" is the microSD card in the box, which
  carries map tiles. The flash is a ZD25Q128, 16 MB of NOR (the wiki,
  from the schematic), which is what the MeshCore variant's 16 MB
  partition table configures. The "16 GB against 16 MB" question in
  #178 is therefore two different parts, not a contradiction.
- 2026-10-25, on our unit: the flash is confirmed as the ZD25Q128
  (see "What the unit on the rig confirmed"). The microSD card is not
  reachable from outside, so the vendor firmware's SD update path is not
  usable here.

### Display

- 2.4-inch TFT, 240x320, ST7789, no touch. Both variants configure the
  ST7789; the wiki does not name the controller, so the part is likely,
  not confirmed.
- Pins (both variants): CS 16, DC 15, RESET 14, TE 19, backlight 17.
  The backlight is on when the pin is low (`TFT_BACKLIGHT_ON LOW` in
  both).
- The backlight pin drives a MOS switch, not the panel (the wiki's
  `BL_EN`). The Meshtastic variant dims it with PWM at 44 kHz, active
  low (`TFT_PWM_FREQ 44000`, `LGFX_INVERT_LIGHT`).
- Orientation: the panel is 240x320 natively. The Meshtastic variant
  rotates it with `LGFX_ROTATION=3`, the MeshCore variant sets
  `DISPLAY_FLIP_VERTICALLY=1`. The MADCTL byte is stated nowhere; it
  could be derived from LovyanGFX's ST7789 driver at rotation 3, which
  nobody has checked.
- On our unit (2026-10-26): the community port's probe build reports
  `[DISP] ST7789 320x240 madctl=0xb0`. The map-fill measurement firmware
  (`docs/drafts/map-measurement-2026-10.md`) sets MADCTL 0x00.
- Observed 2026-10-26 (Lew, at the display while the opt 3 map-fill
  firmware ran): the map is drawn and clean, but rotated 90 degrees
  clockwise against the host reference `view-full.png`; the river sits
  top left on the device and bottom left on the host. So the panel is
  mounted landscape, 320 wide and 240 high. MADCTL 0x00 addresses it
  portrait; the community port's 0xB0 (MY|MV) is the landscape variant.
  Mirroring cannot be judged without text.
- Peripheral power: GPIO18, on when low (`VEXT_ENABLE`/`VEXT_ON_VALUE`
  in both). MeshCore's board code switches it on to power the screen.

### Keyboard

- 37-key QWERTY plus navigation and function keys (the wiki), read
  through a controller on its own I2C bus, SDA 20 and SCL 21 (both
  variants and the wiki).
- KB_INT on GPIO12 signals a key press and wakes the chip from light
  sleep; the Meshtastic variant says it rests low and rises on a press.
  The keyboard light is on GPIO46 (both variants).
- Register 0x03 takes the long-press time in milliseconds, 16 bits big
  endian; the Meshtastic variant writes 700 (`variant.cpp`).
- **Contradiction:** the wiki names an ESP32-S2 as the controller (U10,
  reading a 5x10 matrix); the Meshtastic variant's comments name an
  STC8H. Which chip it is decides nothing for a port as long as the I2C
  protocol is the same, but neither source documents that protocol; the
  Meshtastic variant writes one command (register 0x03, the long-press
  time).

### Two hardware revisions

The Meshtastic variant tells two revisions apart by the keyboard
controller's I2C address: the controller ACKs at **0x6C on V1.0 and at
0x6D on V1.1** (`m9Version` 1 and 2; `variant.cpp`, commit 38ae9aab,
2026-10-16; when both answer, the later probe wins). The only
difference visible in that code is the GNSS enable line's polarity:
**active low on V1.0, active high on V1.1**. Other differences in
parts or pins are unknown. The MeshCore variant ignores the revision
and knows only active low, which is V1.0; so its GNSS handling on a
V1.1 board is unverified (inferred, not tested). A port has to probe the address at boot, as the
Meshtastic variant does, before it touches the GNSS enable line. The
wiki describes V1.0 only. The unit on our bench is **V1.1**: the
community port's probe build printed `[BOARD] m9_version=2` and found
the keyboard controller at I2C 0x6D (2026-10-25 00:30 CET, build
7a699446 of the community fork, the reviewer's bench notes).

### GNSS

- **Contradiction, three ways:**
  - the wiki names an ATGM336H-5NR32 (and a BM72A);
  - the Meshtastic variant says "AG3352 without RTC_INT" at 115200 baud,
    and puts it to sleep with `$PAIR003` followed by EN low;
  - the MeshCore variant's header declares `GPS_L76K` at 9600 baud,
    while its build file sets 115200 baud, so MeshCore disagrees with
    itself as well.
- Pins (both variants): UART on GPIO2 and GPIO3, 1PPS 4, RESET 5,
  standby or wake line 10, EN 11. The variants name the two UART pins
  in opposite directions, so which one the chip transmits on is read
  off the device. Backup power comes from GPS_BAT (the wiki). The wiki
  lists GPS, BDS, Galileo and GLONASS.
- Our T1000-E GNSS work is for an AG3335 (#456). If the receiver is an
  AG3352, that work may transfer, since it is the same family; if it is
  an ATGM336H or an L76K, it does not. The receiver has to be
  identified on the device before any GNSS code is written for it.
- **Resolved by the bench, in favour of the Airoha family.** According
  to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), an AG3352 answered the Airoha `$PAIR` boot init on the bench
  unit. The three-way contradiction above stays as the history of what
  the public sources said; see "What the community port measured".
  Measured on our unit on 2026-10-26: an Airoha AG3352 (see "Two
  conflicts the device settles").

### Sensors

On a second I2C bus, SCL 6 and SDA 7 (both variants):

- a PCF8563 RTC at 0x51 on GPIO6 and GPIO7 (both variants), backed by
  its own RTC_3V3 supply (the wiki);
- a QMC6309 magnetometer (the wiki);
- a QMI8658B IMU (the wiki).

One review article claims the M9 has no accelerometer. This page
follows the wiki, which names the part.

### Radio

A Semtech LR1110 with a 3.3 V TCXO on DIO3 (both variants), the RF
switch on DIO5 and DIO6 with the same table in both variants (standby
LL, RX HL, TX HH, TX_HP LH, DIO5 first), and NSS 39, BUSY 41, IRQ 42
(DIO9), NRESET 45. DIO7 and DIO8 are "reserved nets in the schematic"
(the wiki). The switch table holds both TX HH and TX_HP LH, so both PA
paths are wired; which switch IC sits there and which DIO drives which
of its inputs is unknown. The MeshCore variant transmits at 22 dBm and
calls `setRegulatorDCDC()` after `begin()`; that is generic LR1110 code
for every board, not proof that the M9 carries the DC-DC inductor. The
Meshtastic variant keeps the LR1110 default ceiling of 22. The
`leviculum-lr11xx` README carries these values as a `Board` sketch,
with what was probed on a bench and what stays unknown without the
schematic: see its section "Worked example: ThinkNode M9 (incomplete)"
(`leviculum-lr11xx/README.md`).

On our unit (2026-10-26) the community port's probe build reports
`[LORA] init ok, lr11xx hw=0x22 type=0x01 fw=0x0303` and clamps a
requested 22 dBm to 17 dBm.

### Power

- A 2300 mAh LiPo with protection (the wiki).
- An LGS4056HDA charger ending the charge at 4.35 V (the wiki). Its
  current is set by a resistor, I = 1000/R (the wiki, quoting the
  schematic); R is unknown, so the charge current is too.
- GPIO18 is `VDD_PERIPH_EN`, which "enables power to sensors/LCD-related
  peripherals" (the wiki). Which of the SD card, the GNSS module and the
  display hang on it is unknown; the GNSS module has its own EN on
  GPIO11.
- Battery voltage on GPIO13 through a 2:1 divider (the wiki; the
  Meshtastic variant's `BATTERY_PIN 13` with `ADC_MULTIPLIER 2.0`).
- USB power detected on GPIO1, low when present (the wiki's 5V_TEST;
  `EXT_PWR_DETECT 1` in both variants).
- A slide switch for on and off, a hidden RESET button, and BOOT on
  GPIO0 (the wiki). No source describes the slide switch's electrical
  role: whether it cuts the battery, the regulator or only signals a
  pin is unknown. Decided 2026-10-25 (Lew): the slide switch is the
  hard off, a board fact and not part of the one-button grammar; the
  8 s gesture on the M9 is sleep (see "Decided").

### USB and the rest

- USB-C reaches the ESP32-S3 through a CH340K USB-UART bridge at full
  speed (the wiki), not through the chip's own USB. There is no
  `uf2-msc` path: flashing goes over the ROM's UART bootloader with an
  esptool-class tool. Both variants upload with `esptool`.
- A buzzer behind a transistor, enabled on GPIO9 (the wiki; `PIN_BUZZER
  9` in both variants).
- 2026-10-24, on our unit: the DTR/RTS auto-reset through the CH340K
  works; esptool entered the ROM loader on the first try, three times,
  with no button pressed.

### Two conflicts the device settles

The public sources disagree on two parts. The first is settled on our
unit since 2026-10-26, the second is still open.

- **The GNSS module: an Airoha AG3352.** The wiki names an
  ATGM336H-5NR32 (and a BM72A); the Meshtastic variant an AG3352 at
  115200 baud, put to sleep with `$PAIR003` and then EN low; the
  MeshCore variant an L76K at 9600 baud. Measured on 2026-10-26 with the
  community port's probe build (fork commit 7a699446): it sweeps the
  GNSS UART at 9600, 38400 and 115200 baud and at 115200 got
  `$PAIR021,AG3352` back (`ack=ok`; the scanner is
  `AirohaInit::ag3352()`, `leviculum-nrf/gnss-init/src/airoha.rs:116,242-247`
  in the fork). 234 NMEA sentences followed in 60 s, with no fix
  indoors. An L76K or an ATGM336H does not answer `$PAIR021` with that
  literal. So Meshtastic was right, and the wiki's ATGM336H/BM72A and
  MeshCore's L76K are wrong for this unit. The build logs the literal
  match and counters, not the sentences themselves.
- **The keyboard controller: open.** The wiki names an ESP32-S2 ("U10");
  the Meshtastic variant's comments an STC8H. All that is measured is
  that something answers at I2C 0x6D; the probe build reads register
  0x01 only on a key press, and logs no chip ID. The measurement that
  settles it: a register read over I2C that only one of the two
  answers, or the marking on the chip once the case is open.

### What the unit on the rig confirmed

Read on 2026-10-24 and 2026-10-25 on schneckenschreck, without writing a
byte to the board (the reviewer's bench notes):

- **The chip:** ESP32-S3 (QFN56), revision v0.2, 8 MB embedded PSRAM.
  This confirms the device table.
- **The flash:** manufacturer 0xba, device 0x4018, the Zetta ZD25Q128,
  16 MB, read by `flash_id`. This confirms "Storage".
- **The eFuses are open:** no secure boot, no flash encryption
  (`SPI_BOOT_CRYPT_CNT` 0), download mode enabled. The whole flash reads
  in plain text and writes back unchanged.
- **The vendor firmware** is "Ripple UI / MeshOS" (meshcore.co.uk),
  MeshOS 1.3.1 by its own strings, derived from MeshCore.
- **The backup:** the full 16 MB image is
  `thinknode-m9_e8f60ab888f4_meshos-vendor_2026-10-25.bin` under
  `/home/lew/firmware-backups/` on schneckenschreck, with its partition
  table, checksums and restore command in that directory's README. Step 2
  of the bring-up plan is done.
- **The board revision is V1.1**, the one fact here that needed a write:
  the chip's revision v0.2 is not the board's. The community port's probe
  build, flashed after the backup, printed `[BOARD]
  m9_version=2` and found the keyboard controller at I2C 0x6D
  (2026-10-25 00:30 CET, build 7a699446 of the community fork). Our unit
  ACKs at 0x6D, so by the probe it is V1.1 and its GNSS enable line is
  active high. That is the probe's reading; the pin itself has not been
  measured yet.
- **The probe image is on disk** (2026-10-26):
  `/home/lew/rig-run/m9-leviculum-7a699446-nosd.bin` on schneckenschreck,
  reproducible byte for byte from the fork's ELF with `espflash
  save-image`. espflash's default partition table (factory at 0x10000,
  length 0xfa0000) ends below the identity sector at 0xff0000 and leaves
  it untouched, which is why the unit's identity survived two map-fill
  flashes and the restore of the probe build.

## What changed since August

Codeberg #178 has held an M9 note since 2026-08-13. Its pinout came from
MeshCore PR #2942. Its order was that the Heltec V4 pays for the ESP32
platform first, and that the LR1110 is the one open-ended item, because
our only radio driver was the SX1262's.

The radio driver is no longer open-ended. `leviculum-lr11xx` exists. Cedric
Rivard contributed it as a0f0c3a0 (#451), and it carries the SenseCAP
T1000-E today: the board file runs the LR1110 through it with the
LR1110MB1DXS shield's PA table (`LR1110MB1DXS_PA_TABLE`,
`leviculum-nrf/src/boards/t1000e.rs:143-145`).

The driver's README has a section headed "Worked example: ThinkNode M9
(incomplete)" (`leviculum-lr11xx/README.md:142-163`). Its values come from
Meshtastic's M9 variant, and some were probed on a bench:

- the TCXO at 3.3 V;
- the RF switch lines, taken from the Meshtastic variant;
- `RegMode::DcDc` with either low-frequency clock initialising with a
  clean `GetErrors`, so a 32.768 kHz crystal is fitted.

On one M9 the driver initialised, ran CAD, transmitted, and received
frames over the air (`leviculum-lr11xx/README.md:261-269`). There is still
no `Board` constant for the M9. The PA table and whether the DC-DC
inductor is fitted cannot be settled without the Elecrow schematic.

The ESP32 class itself is still a skeleton. `leviculum-esp` boots the
Heltec V4 and identifies itself, and nothing goes on the air
(`leviculum-esp/src/lib.rs:9-12`, [Supported
boards](../firmware/boards.md), "ESP32 class").

**A community port of our firmware to the M9 exists.** The next section
takes the facts its bench measured; everything else about it stays with
the port.

## What the community port measured

The community port ran on one M9 unit. The facts below are what it
measured, not what we measured, and each one names that source. Its
board file and README are `leviculum-esp/src/boards/thinknode_m9.rs`
and `leviculum-esp/README.md` on that branch.

### Transmit power

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the M9's high-power PA delivers about 4 to 5 dB
  less than the LR1110 shield's at the same setting.
- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the usable ceiling is about +17 dBm, reached at
  `power = 22`; MeshCore's "22 dBm" is that register value, not an
  output power.
- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the PA table that fits keeps the shield's LP rows
  for -17 to +10 dBm and runs the HP PA with `power = dBm + 5` for +11
  to +17 dBm.

### The keypad

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), INT (GPIO12) is a 15 ms pulse that the controller
  does not raise for every key, so the port polls the controller while
  the panel is lit.
- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), `KB_LED` (GPIO46) lights nothing at either level
  on a version-2 board.

### The keyboard controller, measured

Read on 2026-11-04 21:03 CET by Lew at the device, keys pressed at random
under our image `thinknode_m9` at 2621fd18, which read register 0x01 of
0x6D eight bytes every 10 ms and printed each answer as a `KBRAW <ms>
<hex>` line (the reviewer's capture; its first 60 presses and its 42
distinct answers are in `loquax-m9/tests/keyboard/capture-20261104.txt`
and `distinct-20261104.txt`):

- **It is not a matrix.** 13,838 reads, 139 non-zero. An idle read is all
  zeros; a press makes exactly one read non-zero, and that read carries
  one code in all eight bytes (`6b6b6b6b6b6b6b6b`). Two presses 10 ms
  apart came as two reads (`0x65`, then `0x83`), so the 10 ms poll saw
  every press. So the controller is read one code per press, and the
  same code in every byte says the read does not advance through
  registers: one byte would do. Whether a one- or two-byte read, or
  register 0x00 or 0x02, answers the same is not yet asked of the board;
  the image keeps the eight-byte read the capture proved and refuses an
  answer whose bytes disagree (`[KB] answer=mixed`).
- **The codes are ASCII for the characters:** `0x61`..`0x7a` a to z,
  `0x20` the space, `0x30` zero, `0x24` `$`, `0x26` `&`; `0x0d` Enter,
  `0x08` Backspace. The symbols arrive as their own codes, so the
  controller resolves its layers itself. No upper-case letter was
  pressed in the capture.
- **The keys that are no character** sent `0x81`..`0x86`, `0x90` and
  `0xb4`..`0xb7`. Their roles (`SPECIALS`, `loquax-m9/src/keyboard.rs`)
  are measured since 1459, by the reviewer's walk of 2026-11-06 20:17:

  | Code   | Key            | Role                                        |
  |--------|----------------|---------------------------------------------|
  | `0x81` | two bubbles    | the chats list                              |
  | `0x82` | house          | the start screen                            |
  | `0x83` | square bubble  | a new message to the contact under the cursor |
  | `0x84` | position pin   | named, no action until the position sheet   |
  | `0x85` | map            | the map tab                                 |
  | `0x86` | back arrow     | back                                        |
  | `0xb4` | ring left      | left; in a chat a page up (1529)            |
  | `0xb5` | ring up        | up; in a chat a line up (1529)              |
  | `0xb6` | ring down      | down; in a chat a line down (1529)          |
  | `0xb7` | ring right     | right; in a chat a page down (1529)         |
  | `0x0d` | ring centre    | Enter (ASCII CR, 21:03:13)                  |

  In a chat the ring reads by the line (1529, the rule in
  `loquax/src/ui/pager.rs`, its keys in
  [Loquax: keys, mouse and layout](loquax-keys-and-layout.md), "Moving"):
  up and down move the view a line and the selection follows the lines;
  left and right page a screen less a line. The list stays an entry a
  press. A held ring key scrolls as long as the controller answers its
  code again, each answer a press.

  1440's provisional table had `0x84`..`0x86` rotated (back, pin, map).
  The walk's note named `0xb6` right and `0xb7` down: the codes rise in
  the order the walk pressed the ring (down before right), and the note
  read them the other way round. Lew at the device on 2026-11-07 (1528,
  board log `2026-11-07f`) pressed down and nothing moved, pressed right
  and the selection went down; the table follows his hand since. `0x90`, once seen on
  2026-11-04, is no key the walk pressed and stays unnamed
  (`special_0x90`) until a capture shows it.
- **No key-up, no state byte.** No read carries a release. The last byte
  of the `KB` line the provisional matrix decoder printed beside each
  answer (`86868686868602`) looked like a state byte; it is the code's
  two low bits, the two matrix positions the bitmap table read from the
  answer's byte 6 (`every_captured_answer_is_one_code_and_the_kb_state_byte_was_its_low_bits`).
- **No repeat.** The controller does not repeat a held key: Lew held
  the house key 1.2 s on the 840cdaae bench (2026-11-08 13:0x) and the
  board did what a tap does, it opened the house page (1561). So no
  feature on the board may rely on a held key; the walker's `hold` step
  injects repeats every 200 ms (`HOLD_REPEAT_MS`,
  `loquax-m9/src/kbraw.rs`) and simulates what the hardware does not do,
  which is why window 13 saw 1532's held-key switch fire (`via=hold`)
  where Lew's hand never could. What the long-press time of register
  0x03 makes it send is still the walk's.

**The walk** (for the reviewer, one flash of the current image): attach
the reader, press each key once in layout order, row by row from the top
left, then the controls in the fixed order bubbles, house, square bubble,
back, pin, map, ring left, up, down, right, centre, then hold one letter
for two seconds and press one letter with Shift and one with Sym. Each
press prints `KBRAW <ms> <code x8>` and then `KEY <ms> code=0x6b name=k`;
a control prints its role (`name=chats`, `name=ring_up`),
which the walk checks against the order pressed, and a `special_0x..`
name is a code `SPECIALS` (`loquax-m9/src/keyboard.rs`) does not know
yet. A held letter's lines tell whether the controller repeats.

### GNSS

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), an AG3352 answered the Airoha `$PAIR` boot init on
  the bench unit. That settles the three-way contradiction under "GNSS"
  in favour of the Airoha family, so our AG3335 work transfers.

### Executors

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), running the LoRa task on an esp-rtos interrupt
  executor crashed the board, with stack-guard panics and watchdog
  resets within minutes.
- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the port therefore keeps LoRa on core 0's thread
  executor, at a cost in re-arm latency after a reception: p50 about
  2 ms on the interrupt executor against 142 ms on the thread executor.

### The host link

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the single CH340K UART carries both the debug text
  and the HDLC transport, multiplexed on one line with XON/XOFF flow
  control. The multiplexing rules live in `serial-mux`, a member crate
  of ours since 943 (`leviculum-nrf/serial-mux`).
- 2026-11-01 (1238, 1242, #483): the M9's UART obeys a received XOFF,
  so a raw 0x13 from the host mutes it until a reset. lnsd drives it with
  `serial_mux = yes` on its `SerialInterface` block, which escapes 0x11 and
  0x13 in every frame and strips the board's flow bytes on read; periculum
  escapes every frame it writes to an LNode.
- 2026-11-01 (1249, #483, #471): **plugging the M9 in is enough.** Its
  capability report states `serial_mux=1` (fact `0xF0` behind the accepted
  types, `docs/src/firmware/usb-control-envelope.md`), and its `[BOARD]`
  boot line carries the same token. A `SerialInterface` block without the
  `serial_mux` key asks for the report before any other frame and turns the
  mux on when it reads the fact; the key is only an override. The M9 answered
  the query in 7 ms, 6 of 6 rig runs (1238), and lnsd waits at most two
  queries of one second each. The firmware side is a commit on the port's
  local probe branch on the rig host (on top of f1ebf9dc), not in the port
  yet; a build without it answers without the fact and still needs the key.
- 2026-11-01 (1243, #483): a probe build of the port stops obeying a
  received XOFF (fork commit f1ebf9dc, a local branch on the rig host, not
  in the port yet). Software flow control stays on so the UART can still
  send XOFF when its receive FIFO fills, and `force_xon` is held high so a
  received 0x13 is deleted without stopping the transmitter. The raw 1238
  frame is now refused as malformed and the board keeps talking, 9 of 9
  runs. While the old build was muted, its node loop kept announcing on
  LoRa with the outgoing serial queue full.

### GPIO10

- According to the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10), the board file types GPIO10 as the GNSS idle
  interrupt (an input), while the README says it is "held high" (an
  output). This page keeps the question open until a schematic or a
  probe decides it.

## The board as the UI sees it

*Written for the messenger UI plan (1343; [A messenger whose core runs
anywhere](handheld-messenger.md), "The M9 frontend: the plan, measured").*
One place for the facts a UI slice builds on. Everything here is measured
on our unit or carried from the sourced sections above; the schematic is
still missing ("Bring-up plan", step 1), and nothing below rests on
another firmware's header.

- **Display.** ST7789, 320 wide and 240 high as mounted (landscape; the
  community port's MADCTL 0xB0, the map-fill observation under
  "Display"), RGB565, no touch. A full frame is 153,600 bytes, and its
  flush over the shared 40 MHz SPI bus with DMA was measured at 30.8 ms
  from SRAM and 31.5 ms from PSRAM, 99.8 % of the line rate
  (`docs/drafts/map-measurement-2026-10.md`, "Device rows, measured
  2026-10-26"). So a full-frame UI can never exceed about 32 Hz, and the
  bus is saturated: the lever is partial updates, which the ST7789's
  column/row address window gives for free. The bus is shared with the
  LR1110 and the SD card, so a long flush and a radio interrupt contend;
  what that costs is measured on the board, not assumed.
- **Type.** Two sizes since 1532 (`loquax-m9/src/font.rs`), both from
  the `no_std` atlases of `embedded-graphics-unicodefonts` and measured on
  the host at the panel's 0.1524 mm a pixel. The default is the 9 x 15:
  an x of 7 px (1.07 mm), a capital of 10 px (1.52 mm), 35 x 16 cells,
  five blank pixels at the right. The large size is the 10 x 20 of 1428:
  an x of 8 px (1.22 mm), a capital of 13 px (1.98 mm), 32 x 12 cells.
  The 6 x 10 lacks the delivery ladder's tick and clock, the 7 x 13,
  7 x 14 and 8 x 13 lack the tick, and the 6 x 12 sets the same 5 px x
  that was too small at arm's length (Lew, 2026-11-04). The list, the
  chat, the group screens, the pairing and the overview follow the
  reader's size, the top bar with them, as one cell row of the same
  grid; the map's cells, its hint line, pins and names stay on the
  large grid, which its picture is laid on (1531). Since 1561 a third,
  very small size is offered, the 6 x 12 (53 x 20 cells, x 5 px): Lew
  asked for it on 2026-11-08 although 5 px was hard for him on
  2026-11-04, his choice to offer. The reader switches on the settings
  behind the house key (`loquax-m9/src/house.rs`): the row "Schrift",
  Left or Right steps it ("sehr klein", "klein", "groß"), Enter confirms,
  Back keeps the size. The held house key of 1532 is gone: the
  controller repeats no held key ("No repeat" above). The client
  record's byte 6, bits 0-1, keeps the choice across a power cycle (0
  small, 1 large as 1532's flag was, 2 very small); a board without a
  home has no record and starts small.
  There is no tab bar on the M9 (the scene's `Viewing::tabs` is off):
  the map and the house have keys of their own.
- **Input.** The 37-key QWERTY behind the I2C controller at 0x6D (V1.1,
  "Two hardware revisions") is the whole input: no touch, no encoder.
  The INT line does not fire per key, so the firmware polls while the
  panel is lit, and the keyboard light does nothing on a version-2 board
  (both: "The keypad"). The controller sends one ASCII code per press
  and resolves its layers itself ("The keyboard controller, measured");
  which code each navigation and side key sends is the walk's; a held
  key does not repeat (1561).
- **Memory.** 512 KB SRAM, 8 MB PSRAM (came up with 8,388,608 bytes in
  the map measurement), 16 MB flash. How much SRAM is free beside the
  radio stack — and later WiFi or BLE — is unmeasured ("Open"). The
  `leviculum-esp` heap is a 64 KiB placeholder today (`HEAP_SIZE`,
  `leviculum-esp/src/lib.rs:421`); the UI budgets on the messenger page
  say what has to replace it.
- **Radio.** The LR1110 through `leviculum-lr11xx`, usable ceiling about
  +17 dBm ("What the community port measured"). For the UI it is the
  link the messenger rides; the status screen's facts (mode, announce,
  duty) are the same as every board's.
- **GNSS.** An AG3352 with an own fix ("Two conflicts the device
  settles"). So sharing a position is possible on the M9, which neither
  desktop target can ([Loquax: one messenger on every
  target](loquax-targets.md), "GNSS on the desktop").
- **Battery facts the UI shows.** Voltage on GPIO13 through the 2:1
  divider; the charger ends at 4.35 V, so a gauge that assumes a 4.2 V
  chemistry misreads a full battery; USB power present is GPIO1 low
  (all under "Power").
- **Sound.** A buzzer behind a transistor on GPIO9, nothing else.

What the board lacks, one sentence each, under the all-targets rule
(every feature moves its platform layer on all targets where technically
possible; [Loquax: one messenger on every target](loquax-targets.md),
"The M9" has the per-row list):

- **No microphone, no speaker:** a voice message neither records nor
  plays here; the M9 shows that one arrived and how long it is, and the
  buzzer announces it.
- **No camera:** a contact or admission code is never scanned; it is
  typed on the 37 keys or written over the USB control link, and the M9
  shows its own code as a QR on the display for a phone to scan.
- **No touch:** every intent a sheet or a menu offers must be reachable
  from the key table; a feature that only a gesture can reach on another
  target is refused for the scene, the same consistency rule the
  navigation page applies to views.
- **No file system a user can reach:** attachments are listed and small
  photos drawn, but "open with" and "save to" have no meaning; a file is
  fetched by a paired device instead.

## Decided

- **The full stack, in the two modes of [Operating
  modes](operating-modes.md).** In Mesh mode the M9 is a propagation
  node, like every board ([An LXMF propagation node on a
  board](propagation-node-on-a-board.md)). A handheld is not an
  exception to that.
- **A messenger**, described on [A messenger whose core runs
  anywhere](handheld-messenger.md). If the measurements on that page
  allow it, the M9 also gets **a vector map**. (They do: decided
  2026-10-27, Lew, the vector path holds and the PNG fallback is
  dropped; [Loquax: the map](loquax-map.md), section 7.)
- **The one-button grammar in stage one, the menu grammar in stage
  two.** [One button](one-button.md) applies in stage one, on one
  designated key (2026-10-25, below). The menu grammar, in which the mode
  switch, the status and the position report are menu items, is stage
  two, with the keyboard UI. The position of 2026-10-21 was: "No
  one-button grammar. One button is written for boards with one button.
  With a keyboard, the mode switch, the status and the position report
  are menu items, not press lengths."
- **2026-10-25, Lew: stage one is a board like the others:** the same
  status screen, one-button operation on one key, Mesh mode, propagation
  node. Only when that is stable does the keyboard UI follow. The M9 also
  carries the NomadNet browser, after the messenger. Concept work runs in
  parallel: a messenger GUI concept for the desktop and for a 240x320
  display.
- **2026-10-25, Lew: the slide switch is the hard off, the 8 s gesture
  is sleep.** The switch is a board fact, not part of the one-button
  grammar. The 8 s gesture puts the M9 into deep sleep with a GPIO wake,
  as on every board ([One button](one-button.md)).

## Bring-up plan

Nothing in this section is built yet, except where a dated note says otherwise.

1. **The schematic first.** The hardware reference is Elecrow's schematic.
   Pinouts from other firmware and values probed on a bench do not replace
   it (rule `feedback_schematic_is_the_hardware_reference`). The PA table
   and the DC-DC question above wait for it. No schematic is public (no
   repository, issue or teardown found on 2026-10-26); the address is
   techsupport@elecrow.com, named by the wiki, and the request asks for
   both V1.0 and V1.1. The FCC filing's exhibits are the other source for
   the conducted power, once someone reads them in a browser.
2. **Save the factory firmware** before anything is written to the
   flash. Done 2026-10-25 (see "What the unit on the rig confirmed").
3. **Open a bring-up issue with a boot proof** before any radio work: the
   `[FW_BUILD]` banner the rig already reads from every board.
4. **Give the rig an ESP32 side.** Nothing on the rig handles this chip
   family as an LNode today:
   - `lnflash` knows two transports, `uf2-msc` and `dfu-serial`
     (`Transport`, `lnflash/src/manifest.rs:138-148`). An ESP32-S3 has no
     UF2 and no DFU and is written with `espflash`.
   - periculum's LNode profile identifies a board by its USB vendor string
     and interface numbers (`periculum/periculum/devices/lnode.toml:24-32`).
     The one ESP32-S3 on the bench, the Heltec V4, is a `firmware = "rnode"`
     device (`periculum/rig.toml.example:112-115`).
   - The recipes exist: `check-lr11xx-esp32s3` (`Justfile:154`) and
     `build-esp32` (`Justfile:172`). Neither runs in a gate. The Xtensa
     compiler is optional per host, and `.woodpecker/nightly.yml` has no
     step for either.
   - The host toolchain pin stays at 1.97.1 until esp-rs ships a
     non-prerelease Xtensa toolchain of the same generation (decided
     2026-10-05, `scripts/install-ci.sh:433-437`, `rust-toolchain.toml:17`).
   - **2026-10-25, how the rig should recognise an M9 (#473).** USB
     cannot: the CH340K publishes `1a86:7522`, no manufacturer string and
     no serial number, so it looks like every other CH340, and periculum's
     board key (`ID_SERIAL_SHORT`) is empty. Three candidates were weighed.
     The **by-id path** (`usb-1a86_USB_Serial-if00-port0`) names the
     bridge model, not the unit: a second CH340 without a serial gets the
     same link, and `by-path` holds only while the dock topology does. The
     **chip MAC read through the ROM loader** (`espflash board-info`,
     `esptool chip_id`) is the true identity and works under any firmware,
     but every read pulls EN through the auto-program circuit, so every
     discovery would reset the board and end whatever it was doing; it
     would also reset any stranger's ESP32 modem that sits on a CH340, and
     it cannot run while `lnflash --mux` holds the port. The **banner** is
     what the rig reads from every other LNode: periculum already writes
     `b` on the debug stream and waits for `[FW_BUILD]` (leviculum 911),
     and opening the CH340 raises DTR and RTS together, which resets
     nothing (measured 2026-10-24). **Recommended: the banner**, as a
     confirm handshake on a profile keyed on `1a86:7522`, with the board's
     key taken from the banner group rather than from USB. Its cost: the
     M9 has to run our firmware, its banner group has to carry a unit
     identity (the chip MAC on the `[BOARD]` line is the cheapest; the
     community port prints `m9_version` there today), and a CH340 that
     does not answer has to fall back to the RNode probe instead of being
     dropped. The ROM-loader MAC stays the identity of the one path that
     resets the board anyway: flashing.
   - **2026-10-26, the M9 is on the rig (#473).** The rig.toml entry has
     this shape:

     ```toml
     [devices.thinknode-m9]
     firmware = "lnode-uart"
     vid_pid = "1a86:7522"
     serial = "<identity>"
     ```

     `RigDevice` has no `profile` field: `firmware` names the device
     profile, and the key is the unit's identity from the banner. It
     changes on a flash erase or a vendor restore. `periculum devices
     --probe` confirmed the M9 by its banner (`fw=7a699446`). The T-Beams
     (`1a86:55d4`) were never offered to the banner reader, since the
     profile claims only `1a86:7522`. The probe cost 0.5 to 7 s,
     depending on where the 5 s banner cycle stands (two runs: 0.52 and
     7.07 s). The probe does not reset the M9: `BOOT_COUNT` stayed
     unbroken across two probes. What does reset it is a pyserial
     `open()`, even with `dtr=False` and `rts=False`: pyserial clears DTR
     and then RTS one after the other, and one line moving alone pulls
     EN through the CH340K. A termios reader that opens the port without
     moving the lines (the `m9-listen4.py` shape) does not reset it.
   - **2026-10-25, the flashing half (#473).** `lnflash/src/esp.rs` wrote
     an ESP32 through the ROM loader with `espflash` (preferred, it carries
     its own stubs and takes the ELF) or `esptool`, with `--no-stub` when
     the esptool package lacks the chip's stub. A catalogue entry stated
     the loader's facts as `[board.<name>.esp]`, `chip` and `baud`; the
     M9's rate is 230400, because its CH340K lost bytes at 460800 and
     921600. No M9 entry is in the catalogue yet: lnflash's control half
     assumes the two-CDC composite (debug on if00, transport on if02), and
     a control-only entry keyed on `1a86:7522` would point every control
     session at every CH340 on the bus. The entry lands with the
     single-port host link or with the first bundle that carries an M9
     image. **2026-11-08 (#476):** no flash run reached the module, so it was
     removed with the catalogue section; commit 39071420 holds both for
     the day the M9 entry lands. Until then lnflash names an M9 on the bus
     in one line and leaves it alone.
5. **Acceptance at the rig:**
   - the boot proof;
   - the display. Since 2026-10-25 (980) the status page and its
     one-minute power policy are `leviculum_firmware::display` over the
     `Panel` seam (`leviculum-nrf/firmware/src/seams.rs`), so the M9's
     ST7789 needs a `Panel` of its own, not a page of its own;
   - LoRa against the T1000-E (the same driver family);
   - LoRa against an RNode, with 915's T1000-E/RNode loss measurement as
     the comparison.

## What the T1000-E taught

The T1000-E was the first LR1110 board on the rig. An LR1110 board meets
these three things first:

- **A guessed PA table.** Seeed publishes no characterisation, so the
  T1000-E ships the Semtech shield's table. Every dBm it announces is that
  table's claim (#454). Two more board fields are guesses from
  Meshtastic's variant (#455).
- **`txp_honoured=no`.** The T1000-E drives 4 dBm when asked for 2. The
  status line says so in a word that can be grepped
  (`txp_honoured`, `leviculum-nrf/log-line/src/facts.rs:113`).
- **7 % loss against an SX1276 RNode at -27 dBm.** 915 read five runs of
  the T1000-E/RNode pair cell at a 93 % mean PDR
  (`periculum/hardware/t1000e_rnode_pair.toml:36-38`). It split the losses
  into half-duplex kills, which both firmwares share, and frames the
  T1000-E aired that the RNode never reported. The suspect for the second
  kind is the transmit signal from the uncharacterised PA. 917 added a
  silent witness board to that cell to tell the two apart (#466).

The M9's plan follows from those three. The PA table comes from the
schematic and a measurement, not from a shield, and the RNode run is
compared with the T1000-E's numbers.

## Open

- **The schematic**, asked of Elecrow's technical support: the PA table,
  the DC-DC inductor, and every contradiction under "The hardware from
  the public record" wait for it or for the device.
- **Which revision the unit is**, read off the keyboard controller's
  I2C address at the first boot. Closed 2026-10-25: V1.1, the probe build
  printed `[BOARD] m9_version=2` and found the keyboard controller at
  I2C 0x6D (build 7a699446 of the community fork, 00:30 CET).
- **Which key carries the one-button grammar in stage one** (2026-10-25,
  "Decided"). How its 8 s row meets the slide switch is closed: the
  switch is the hard off, the 8 s gesture is sleep (2026-10-25, Lew,
  "Decided").
- **The GNSS receiver:** AG3352, ATGM336H or L76K, identified on the
  device. Closed 2026-10-26: an AG3352, measured on our unit (see "Two
  conflicts the device settles"), as the community port's bench
  (cedrivard/Leviculum, rp2350-m9-net, 2026-10) had found.
- **GPIO10's direction:** the GNSS idle interrupt (input) or a line held
  high (output); in the community port's bench (cedrivard/Leviculum, rp2350-m9-net, 2026-10) the board file and the README disagree.
- **SRAM after WiFi and BLE:** how much of the 512 KB is left once both
  stacks are up.
- **WiFi as a TCP interface** without a host attached.
- **The keyboard:** which controller it is (ESP32-S2 or STC8H). Its
  protocol is measured (one code per press, "The keyboard controller,
  measured"); open are the names of the eleven codes that are no
  character, repeat, and what Shift, Sym and the long press send, all
  read by the walk.

## Not yet built

All of it, apart from the driver and the Loquax image of 1361 (a board
file and binary that light the panel, read the keyboard controller and
draw the messenger's fixture screens, nothing on the air; [A messenger
whose core runs anywhere](handheld-messenger.md), "Slice four, host
half"): the `Board` constant for the radio, the node on the board, the
menu, the rig's ESP32 flashing and device profile, the gates for the
ESP32 recipes, and the acceptance runs.

## See also

- [A messenger whose core runs anywhere](handheld-messenger.md), for
  what the M9 is for.
- [Operating modes](operating-modes.md), for Mesh and Tracker.
- [One button](one-button.md), the grammar the M9 uses on one key in
  stage one.
- [How far one firmware build reaches](board-support-scope.md), for the
  ESP32 class and the LR11xx driver.
- [Supported boards](../firmware/boards.md), for the T1000-E and the
  Heltec V4.
- [Firmware layers](firmware-layers.md), for the shared orchestration
  the M9's bind crate is meant to sit under.
