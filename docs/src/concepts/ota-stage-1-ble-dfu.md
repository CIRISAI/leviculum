# OTA stage 1: entering BLE DFU on a command from the mesh

**Status: stage 1 active since 2026-11-09 (Lew).** Most of what it
needs on the board was built for Codeberg #235, and the board's `/dfu`
verb and its sender `lnmgmt dfu` since; what is left is one package per
board and the cells that prove them (the last two sections).

Every firmware update this project has ever shipped ended with a hand on
the board: a cable, or a needle in a pinhole
([Flashing an LNode](lnode-flashing.md)). That is fine for a board on a
desk and wrong for the one on the roof, in the hedge, or two valleys
away. Stage 1 splits the problem along the seam where it is cheapest to
split it:

> **The decision travels the mesh. The bytes do not.**

A command over Reticulum, on any carrier and over any number of hops,
puts the board into the bootloader's BLE DFU mode. Somebody then walks
up to within Bluetooth range with a phone and pushes the image. Nothing
about the image itself crosses the radio. [OTA stage 2](ota-stage-2-mesh-image.md)
is the design record for when nobody can walk up at all; it is struck
from the register.

## What the bootloader does, read from its source

All our nRF52 boards carry an Adafruit nRF52 bootloader, and that
bootloader, not our firmware, owns DFU. `INFO_UF2.TXT` names the build
on each board ([Flashing an LNode](lnode-flashing.md), "What the
bootloader tells you"): `0.9.0-2-g836c8dc-dirty` on the T114 and
`0.4.3`, `Date: May 20 2023`, on the RAK4631. The Pocket V2 shares the
RAK4631's `Board-ID`; which build its bootloader is has not been
recorded, and the bench measurement below reads it first.

**Which source each claim below is read from.**

* **0.9.0.** The Adafruit tree at tag `0.9.0`, commit `a73fe207`
  (github.com/adafruit/Adafruit_nRF52_Bootloader). The T114's build is
  `836c8dc`, two commits later; both change only
  `.github/workflows/githubci.yml`, so every source line below is the
  same in it. The `-dirty` suffix says the vendor built from a tree with
  local changes nobody published; the bench measurement below is what
  covers that gap. Citations are `Adafruit_nRF52_Bootloader/<path>:<line>`
  at that tag. The citation guard counts them as external and cannot
  open them (`leviculum-std/tests/doc_citations.rs`, `EXTERNAL_PREFIXES`);
  a reader re-checks them against the tag.
* **0.4.3 (RAK).** This is RAKwireless's own version number, not an
  Adafruit tag: Adafruit has no `0.4.3`, and RAK publishes binaries
  only ("Full bootloader source is maintained separately",
  github.com/RAKWireless/WisBlock, commit `d3da249c`,
  `bootloader/RAK4630/README.md` line 16). It is not Adafruit 0.4.1
  either: the binary compares `GPREGRET` against `0x6D`
  (`DFU_MAGIC_SKIP`), which Adafruit introduced in 0.5.0. So the 0.4.3
  claims below are read from RAK's published image,
  `bootloader/RAK4630/Old/V0.4.3/wiscore_rak4631_board_bootloader-0.4.3.hex`
  at the same WisBlock commit (sha256 `36525835…b885f6`), disassembled
  with `arm-none-eabi-objdump -M force-thumb` at its load address
  `0xF4000`. Its `INFO_UF2.TXT` string carries `Date: May 20 2023`, the
  same date the rig RAK reports, so it is the build on our board.
  One correction to "binaries only": a source tree does sit beside the
  images, `bootloader/RAK4630/Latest/WisCore_RAK4631_Bootloader/`. It
  arrived with the 0.4.3 commit (`adbeffe`, 2023-05-20), its changelog
  stops at Adafruit 0.6.2, and it is what the staging addresses in
  [Bootloader update](#bootloader-update) are read from. For 0.4.4 RAK
  published no source.

| Question | 0.9.0 (T114) | 0.4.3 (RAK4631) |
| --- | --- | --- |
| Magic that selects BLE DFU | `0xA8`, `DFU_MAGIC_OTA_RESET` "entered by soft reset, SD is not inited yet" (`Adafruit_nRF52_Bootloader/src/main.c:110`); UF2 is `0x57` (`Adafruit_nRF52_Bootloader/src/main.c:112`), the value our touch writes (`leviculum-nrf/src/usb.rs:236-237`) | Same: `ldr.w r4, [r3, #1308]` reads `0x4000051C` (`GPREGRET`) at `0xF483A`, `cmp r4, #177` (`0xB1`) at `0xF483E`, `sub.w r1, r4, #168` (`0xA8`, the compare-to-zero idiom) at `0xF484A`, `0x4E` and `0x57` at `0xF486C`/`0xF4870` |
| How the magic is consumed | `_ota_dfu` is set from it (`Adafruit_nRF52_Bootloader/src/main.c:229`) and the register is cleared on entry (`Adafruit_nRF52_Bootloader/src/main.c:243`) | Cleared on entry: `str.w r1, [r2, #1308]` with `r1 = 0` at `0xF489C` |
| Does it leave DFU mode by itself | **No, measured.** On the T114 (bench 2026-11-10, below) the bootloader advertised for 61.5 minutes after `lnmgmt dfu` with no reboot, no debug line and no USB enumeration, until it was sent out by hand. The source agrees: OTA entry calls `bootloader_dfu_start(_ota_dfu, 0, false)` (`Adafruit_nRF52_Bootloader/src/main.c:305`); the only startup timer is armed on the serial branch, never when `ota` is true (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/bootloader.c:333-347`); advertising runs with `BLE_GAP_ADV_TIMEOUT_GENERAL_UNLIMITED` (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:58`); the one advertising timeout that ends DFU counts down directed advertising (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:808-815`), which is used only when an application handed the bootloader bond data (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:1034`); our firmware never does | **No, as far as the image shows.** The call site takes the timeout `3000` only when the application asked for single-tap reset or the magic was `0x57` or `0x4E`, and `0` otherwise (`movw r1, #3000` at `0xF4A98`, `mov r1, r2` with `r2 = 0` at `0xF4AA0`, `bl` at `0xF4AA4`), the same shape as `Adafruit_nRF52_Bootloader/src/main.c:301-306`. The advertising duration inside the transport is not decoded, and the RAK has not had its bench hour yet |
| What it advertises | **Measured on the T114, and not what the source says: the name `HT-n5262-OTA`, and no service UUID in the advertisement.** A connectable `ADV_IND` with flags `0x06` and the complete name as its only other field (`02 01 06 0d 09 48 54 2d 6e 35 32 36 32 2d 4f 54 41`), an empty scan response; 311 sightings in 273 back-to-back `btmgmt find` rounds of about 11.5 s (`scanloop.log`). The DFU service `0x1530` is there only in GATT (handles `0x000E..0x0015`: Packet `0x1532`, Control Point `0x1531` at `0x0012` with its CCCD at `0x0013`, Version `0x1534` reading `08 00`); Device Information names `Heltec AutoMation`, `HT-n5262` and firmware `0.9.0-2-g836c8dc-dirty s140 6.1.1`, which runs under the installed S140 7.3.0. The interval is consistent with 25 ms (268 `ADV_IND` in 10.1 s on a Realtek host adapter that bundles reports, so a lower bound on the rate). The `-dirty` vendor tree is where name and advertisement changed; upstream 0.9.0 reads: name `AdaDFU` (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:50`, put into the advertisement at `Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:645`); the Nordic legacy DFU service `0x1530` (`Adafruit_nRF52_Bootloader/lib/sdk11/components/ble/ble_services/ble_dfu/ble_dfu.h:36`) on base `23D1BCEA-5F78-2315-DEEF-1212…` (`Adafruit_nRF52_Bootloader/lib/sdk11/components/ble/ble_services/ble_dfu/ble_dfu.c:472`), i.e. `00001530-1212-EFDE-1523-785FEABCD123`, as a 128-bit UUID (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:646`); connectable undirected every 25 ms (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:57`) | `AdaDFU` at file offset `0xB72C` of the image and the same 128-bit base at `0xB761`; interval not decoded. **Unmeasured on a board**: the T114 shows that the string in an image is not proof of the advertisement |
| Address | **Confirmed on the T114**: application `E1:AC:9E:B3:F0:5C`, bootloader `E1:AC:9E:B3:F0:5D`, random static. The source: the SoftDevice's default address with its lowest byte plus one (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:1047`): a static random address, not resolvable, and not the one our firmware advertises under, so a phone sees a new device | Not decoded and not measured |
| An aborted transfer | **The old application is gone.** The build is single-bank (`Adafruit_nRF52_Bootloader/Makefile:159`); the start packet erases the application area in OTA mode (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_single_bank.c:99`) and the bootloader records bank 0 invalid (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/bootloader.c:292`). A dropped link restarts advertising (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:764-770`), but a second start packet is refused until a reset, because the state is no longer idle (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_single_bank.c:313`). After any reset `GPREGRET` is `0`, the application is invalid, and the bootloader enters DFU on the **USB** branch, not BLE (`Adafruit_nRF52_Bootloader/src/main.c:288-296`) | Same outcome, by RAK's own account: their 0.4.4 changelog lists "if a BLE OTA firmware update fails or is interrupted, the device stays in BLE OTA / DFU mode so another attempt can be made (instead of getting stuck in USB DFU/UF2 boot mode)" (WisBlock `d3da249c`, `bootloader/RAK4630/README.md` line 14) |

Two consequences follow, and both change what this page used to say.

**DFU mode does not end by itself**, measured for an hour on the T114. A
board told to enter it and never visited advertises its DFU name
(`HT-n5262-OTA` on the T114) until power is lost or it is reset. A reset
of any kind brings the application back, the one sent over Bluetooth
included ([Leaving DFU without hands](#leaving-dfu-without-hands)), because the bootloader cleared
`GPREGRET` on entry and the application is still valid as long as no
transfer started (`Adafruit_nRF52_Bootloader/src/main.c:202`). Our
firmware runs no watchdog, and one would not help: the bootloader feeds
a running watchdog in its event loop
(`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/bootloader.c:120-122`).
On a battery or solar board, a 25 ms advertising interval for as long
as nobody comes is the cost of a mistaken command.

**An interrupted transfer is not survivable over BLE on either build.**
Once the first start packet is accepted, the old image has been erased;
if the transfer then dies and the board resets, it comes up in USB
DFU, which needs a cable. A board somebody can walk up to with a phone
is a board somebody can usually walk up to with a cable too, so stage 1
stays worth having, but the "reconnect and push again" this page
promised before holds only as long as the board is not reset in
between, and even then the bootloader refuses the second start packet.
RAK's 0.4.4 fixes this on the RAK; for the T114 family nothing
published does. Lifting the RAK to 0.4.4 is the next section.

## Leaving DFU without hands

Measured on the T114 `DEC9947` on 2026-11-10 (files under
`/home/lew/rig-run/bench/2026-11-10-ota/` on schneckenschreck, the
board's debug log `/home/lew/rig-run/ble-drop/t114-boot.log` lines 843823
to 845742):

1. `lnmgmt dfu -R <transport hash> -i <identity>` answered `accepted: the
   board resets into BLE DFU now`, the board's debug port said
   `MGMT_DFU accepted=yes`, and the board dropped off USB within a second.
2. It then advertised `HT-n5262-OTA` from the application's address plus
   one for 61.5 minutes, with nothing on the debug port and no USB
   enumeration (the table above).
3. A host connected to that address, enabled notifications on the Control
   Point's CCCD (`0x0013`, `0100`) and wrote the Legacy DFU opcode `06`,
   SYS_RESET, to the Control Point (`0x0012`). No start packet was sent,
   so nothing was erased, and the application was back 1.5 s later, intact.

So a board somebody sent into DFU by mistake does not need a visit with a
needle or a power switch: anybody within Bluetooth range with a BlueZ host
brings it back. `scripts/dfu-exit.py` is that step as a tool: it finds the
bootloader by a name ending in `-OTA` or by the application's address plus
one (`--app-address`), connects, finds the Control Point by its UUID
rather than trusting the T114's handles, writes `06`, and scans until the
advertisement is gone. `--from-capture` reads a captured scan instead and
only says what it would do; `just dfu-exit-selftest` holds its parsers
against the bench captures. `lnmgmt` has no Bluetooth path (it is a
Reticulum client, and a board in its bootloader is off the mesh), so
`lnmgmt dfu --help` names the script instead of growing a verb. nRF Connect
does the same by hand: connect, Legacy DFU service, Control Point,
notifications on, write `06`.

**Which name to look for, per board family.**

| Board | DFU advertisement | Source |
| --- | --- | --- |
| Heltec T114 (`0.9.0-2-g836c8dc-dirty`) | `HT-n5262-OTA`, no service UUID, from app + 1 | measured 2026-11-10 |
| RAK4631, Pocket V2 (RAK `0.4.3`, `0.4.4`) | unmeasured; the image carries `AdaDFU` and the 128-bit base | read from the image only |
| T1000-E | unmeasured | none |

A phone or a script that filters the scan for `AdaDFU`, or for the DFU
service UUID `1530` in the advertisement, does **not** find a T114: the name
is the vendor's and the UUID is only in GATT. Look for the name ending in
`-OTA`, or for the address one above the one the board's application
advertised under (nRF Connect shows both), and connect to see `1530`.

**One board's allow-list.** The bench found the T114 still holding the
management identity of the periculum cell `lnode_remote_status` from
2026-11-09, and the only tool that reads or clears a list,
`lnflash --management-identity` / `--clear-management`, acts on every
attached LNode, so the bench scripted the envelope by hand. With `--port`
naming one board's serial port (any of its CDC ports, a by-id link works)
the same flags act on that board alone: `lnflash --management-identity
--port <tty>` lists its list, `lnflash --clear-management --port <tty>`
empties it, every other board on the bus is left alone
(`flow::set_mgmt`'s `only`, `lnflash/src/flow.rs`). The cell's own cleanup
is periculum's.

## Bootloader update

**Decided by Lew on 2026-11-09: the RAK4631 bootloader goes from 0.4.3 to
0.4.4.** On 0.4.3 an interrupted BLE transfer leaves the board in USB DFU
(the table above); on 0.4.4, by RAK's changelog, it stays in BLE DFU and the
phone can try again. Nothing below has been run on a board yet: the bench
window at the end is what turns it from read to measured. The Pocket V2
shares the `Board-ID`, and its build is read first and decided on its own.

### Which file, from where

All from github.com/RAKWireless/WisBlock at commit `d3da249c` (2026-07-23,
"Update RAK4631 bootloader to V0.4.4 with BLE OTA recovery."), vendored
byte for byte with their licence under
`lnflash/tests/fixtures/bootloader/rak4631/`:

| File | Path at `d3da249c` | sha256 | Covers |
| --- | --- | --- | --- |
| `update-wiscore_rak4631_board_bootloader-0.4.4_nosd.uf2` | `bootloader/RAK4630/Latest/` | `6795b91c…d8297001` | 136 blocks, family `0xd663823c`: MBR `0x0..0xB00`, bootloader `0xF4000..0xFBB00`, config page `0xFD800..0xFD900`, UICR block `0x10001000` |
| `wiscore_rak4631_board_bootloader-0.4.4.hex` | `bootloader/RAK4630/Latest/` | `29635d90…e1ca5bd` | bootloader `0xF4000..0xFBAB0`, config `0xFD800..0xFD858`, UICR words `0x10001014`/`0x10001018` |
| `wiscore_rak4631_board_bootloader-0.4.3.hex` | `bootloader/RAK4630/Old/V0.4.3/` | `36525835…b885f6` | the way back, same layout, code to `0xFBFE8` |

The INFO string the 0.4.4 image carries, at `0xFB90B`: `UF2 Bootloader
0.4.4`, `Model: WisBlock RAK4631 Board`, `Board-ID: WisBlock-RAK4631-Board`,
`Date: Jun 17 2026`, `Ver: 0.4.4`. Its config page is byte-identical to
0.4.3's, including the bootloader ID `0x239A0029` that the running 0.4.3
compares a self-update against before taking it (`write_block` in
`src/usb/uf2/ghostfat.c` of RAK's tree).

**The SoftDevice.** 0.4.4 is built against S140 6.1.1, as 0.4.3 was
(`SD_VERSION = 6.1.1`, `Makefile` line 18 of RAK's tree); RAK's README says
"softdevice version 140.6.1.1" (line 8). The bootloader does not bring its
own SoftDevice in the `_nosd` UF2: the file's only blocks below the
bootloader are the MBR's, `0x0..0xB00`, which the running bootloader skips
(`ghostfat.c` line 557), so S140 7.3.0 stays where it is, and
`INFO_UF2.TXT` keeps reporting it, as it does today under the same 6.1.1
build of 0.4.3. **Refused here**: `wiscore_rak4631_board_bootloader-0.4.4_s140_6.1.1.zip`
(its `manifest.json` is a `softdevice_bootloader` package with
`sd_size` 151016, the 6.1.1 SoftDevice, and RAK's README drives it with
`adafruit-nrfutil dfu serial`) and the `_s140_6.1.1.hex` (it carries
`0x1000..0x25DE8`): both put 6.1.1 back, the downgrade that was the
"Schein-Brick" of September. Whether the bootloader's own BLE DFU runs under
7.3.0 is not something either build has been measured doing; that is the
bench hour above, on 0.4.4 as on 0.4.3.

**The stage-1 verb still fits.** Disassembled like 0.4.3 (load address
`0xF4000`): `GPREGRET` is read at `0xF4830` (`ldr.w r7, [r2, #1308]`) and
compared with `0xB1` (`0xF483A`), `0xA8` (`0xF4848`), `0x4E` (`0xF4854`),
`0x57` (`0xF487E`) and `0x6D` (`0xF4892`), and cleared on the
`0xB1`/`0xA8` path (`str.w r4, [r2, #1308]`, `r4 = 0`, `0xF4876`).
`AdaDFU` is at `0xFB5DC`, the 128-bit service base at `0xFB621`. What is
new is a store 0.4.3 does not have: `movs r2, #168` and
`str.w r2, [r3, #1308]` at `0xF48DA`/`0xF48DC`, followed by
`bl 0xF4770`, which is `NVIC_SystemReset` (`0x05FA0004` into
`0xE000ED0C`). The bootloader writes `0xA8` itself and resets into its own
BLE DFU, on the path where the application-validity check called at
`0xF48A4` fails (the function at `0xF5468` reads the SoftDevice's info
magic `0x51B1E5DB` at `0x3004`, then the bank state and its CRC). That is
the mechanism behind RAK's changelog line. Which entries reach that path is
not fully decoded; the bench reads it.

### What the update writes, and where

The file's own addresses are not where the bytes go. The **running** 0.4.3
stages every bootloader block lower, inside the application window, and only
after the last one asks the MBR to copy the staged image over itself: the
`CFG_UF2_FAMILY_BOOT_ID` arm of `write_block` writes to
`targetAddr - (BOOTLOADER_ADDR_END - USER_FLASH_END)`
(`src/usb/uf2/ghostfat.c` line 545 in RAK's tree), and
`SD_MBR_COMMAND_COPY_BL` copies `DFU_BL_IMAGE_MAX_SIZE` bytes from
`BOOTLOADER_ADDR_NEW_RECIEVED` (`src/usb/msc_uf2.c` lines 200 to 211;
the constants at `src/usb/uf2/uf2cfg.h` lines 21, 27 and 30). On the RAK,
`0xFE000 - 0xEA000 = 0x14000`, so:

| Step | Range | What is there on our board |
| --- | --- | --- |
| MBR blocks | `0x0..0xB00` | skipped |
| Staging, code | `0xE0000..0xE7B00`, pages `0xE0000..0xE8000` | record log (`STORE`), pages 6 to 13 of 16 |
| Staging, config page | `0xE9800..0xE9900`, page `0xE9000..0xEA000` | record log, page 15 |
| UICR block | `0x10001000` | compared against `0xF4000`/`0xFE000`, not written |
| MBR copy | `0xF4000..0xFE000` | the bootloader itself, below the MBR parameter page `0xFE000` and the settings page `0xFF000` |

Each staged page is read, patched and rewritten whole (`flash_nrf5x_write`,
`src/flash_nrf5x.c` line 69), so the rest of page `0xE7000` and of page
`0xE9000` keeps its bytes, but a page header the record log needs on
`0xE0000` to `0xE7000` is gone. **The records on those nine pages are
lost.** The log mounts past foreign pages (`foreign_region_is_survivable`
and `a_page_of_plausible_headers_is_sealed_rather_than_believed` in
`leviculum-nrf/record-log/src/tests.rs`), so the board comes up with what
the other seven held. On the bench RAK that is test traffic; on a board
that carries other people's messages it is not, and the recipe asks.

**The identity is outside every step.** Staging cannot reach above
`USER_FLASH_END` (`0xEA000`) by construction, and the identity
(`0xEC000`), radio (`0xEB000`) and telemetry (`0xEA000`) pages sit at or
above it; the copy starts at `0xF4000`. Nothing touches `0x1000..0xD7000`
(SoftDevice and application), the calendar ring or the boot record.

`lnflash::bootloader_update::plan` is this table as code, and
`lnflash/tests/bootloader_update.rs` holds it against the vendored UF2 and
`leviculum-nrf/memory.x`: green on the real file, red on a block on the
identity page, a bundled SoftDevice, an application image, a UICR naming
another bootloader start, a missing UICR block, and a receiver whose staging
would land on the identity page; protecting `STORE` refuses the real file,
which is how the test knows the overlap is real. It also checks that the
0.4.4 hex the SWD recovery writes is byte for byte what the UF2 installs.

### The recipes

* `just bootloader-info <port>` (or `-` to double-tap by hand): enters the
  bootloader with the 1200-baud touch, mounts the volume read-only under
  `/run/leviculum-uf2/`, prints `INFO_UF2.TXT` and the window top read
  from `CURRENT.UF2`'s size, unmounts. The board stays in its bootloader;
  its flash recipe brings the application back. A Pocket V2 does not leave
  the bootloader on a power cycle, the battery holds it up.
* `just bootloader-update-rak <uf2> [port]` (`scripts/lnode-bootloader.sh`):
  1. the planner (`lnflash/examples/bootloader_plan.rs`) reads the UF2
     against `memory.x` and refuses anything but staging into the record
     log; it prints the record-log pages lost and the run asks for a
     `yes` (`LEVICULUM_BL_ACCEPT_STORE_LOSS=1` says it ahead);
  2. with a port, the board's `[IDENTITY]` line off its debug port; no
     identity, no write;
  3. touch (or double-tap) into 0.4.3; `INFO_UF2.TXT` must say
     `WisBlock-RAK4631-Board`, `S140 7.3.0` and a window top of `0xEA000`,
     the receiver the planner modelled;
  4. the UF2 onto the volume mounted `-o flush`, no `sync` (the bootloader
     resets under the last block, and a global sync would wait on a device
     that is gone), and the volume must vanish within 30 s;
  5. back into the bootloader (the touch again once the application is
     back, or a double-tap); INFO must name the planner's version, the
     same SoftDevice and the same window top;
  6. our application through the runner `just flash-rak4631-one` uses,
     which proves `[FW_BUILD]`; then `[IDENTITY]` must be the one from
     step 2.

### Recovery over SWD

The bootloader is written by the MBR copy in one go; a board that loses
power inside it, or a 0.4.4 that misbehaves, is recovered with the
Raspberry Pi Debug Probe on the RAK's SWD pads
([Probe debugging](../firmware/probe-debugging.md) has the wiring and the
`--probe 2e8a:000c` selection). From the repo root, with
`P="--chip nRF52840_xxAA --probe 2e8a:000c --protocol swd"`:

```
~/.cargo/bin/probe-rs info $P
# keep the identity, radio and telemetry pages before writing anything
~/.cargo/bin/probe-rs read $P b32 0xEA000 3072 > rak-appdata-0xEA000.txt
# the UICR must already name the bootloader: expect 000f4000 000fe000
~/.cargo/bin/probe-rs read $P b32 0x10001014 2
# the bootloader alone, its UICR records stripped (.sec3 is the UICR pair);
# the way back is the 0.4.3 hex in the same directory, stripped the same way
mkdir -p target/tmp
arm-none-eabi-objcopy -I ihex -O ihex --remove-section=.sec3 \
    lnflash/tests/fixtures/bootloader/rak4631/wiscore_rak4631_board_bootloader-0.4.4.hex \
    target/tmp/rak-bootloader-0.4.4-no-uicr.hex
~/.cargo/bin/probe-rs download $P --binary-format hex target/tmp/rak-bootloader-0.4.4-no-uicr.hex
# only if the SoftDevice is gone too (INFO names none, or the MBR reads blank)
~/.cargo/bin/probe-rs download $P --binary-format hex \
    lnflash/payload/t114/s140_nrf52_7.3.0_softdevice.hex
~/.cargo/bin/probe-rs reset $P
```

then a double-tap and `just flash-rak4631-one`. The stripped hex covers
`0xF4000..0xFBAB0` and `0xFD800..0xFD858`, so the pages it erases are the
bootloader's own and nothing at or above `0xFE000` (the MBR parameter page,
the bootloader settings with the application's validity). The UICR is left
alone on purpose: it already holds `0xF4000`/`0xFE000`, and changing a UICR
word means erasing the whole UICR, the board's other configuration words
with it. Never `--chip-erase` or `--allow-erase-all`, which take the
identity too. The 7.3.0 SoftDevice hex is the one `lnflash` ships (MBR at
`0x0..0xB00`, SoftDevice at `0x1000..0x26498`). **A Pocket V2 is not reset
by power-cycling it**: the battery keeps it running, so its way back into
the bootloader is the needle in the pinhole.

### At the bench, in this order

Lew has the Debug Probe connected to the RAK before step 3.

1. `just bootloader-info <pocket port>`, then `just flash-rak4631-one` on it
   to bring it back. Its build decides whether the Pocket follows; nothing
   is written to it in this window.
2. `just bootloader-info <rak port>`, then `just flash-rak4631-one`: 0.4.3,
   `S140 7.3.0`, window top `0xEA000` expected.
3. `just bootloader-update-rak lnflash/tests/fixtures/bootloader/rak4631/update-wiscore_rak4631_board_bootloader-0.4.4_nosd.uf2 <rak port>`;
   the `BL_PLAN`, `BL_INFO` and `BL_UPDATE` lines are the record.
4. One double-tap on the RAK: the UF2 drive must come up (the USB way back
   still works), then `just flash-rak4631-one`.
5. The DFU hour and the sniffer above, on 0.4.4, and then the case this is
   for: a transfer started from the phone and cut off, after which the RAK
   must still advertise its DFU name (`AdaDFU` per the image, unmeasured;
   the T114 showed the image string is not the advertisement) and take a
   second, complete transfer.

## What is built (Codeberg #235 and its neighbours)

| Piece | Where |
| --- | --- |
| Allow-list as a flash record in the protected band; a blank page decodes to "nobody" | `LMGA` record, 136 B, `leviculum-nrf/src/telemetry.rs:253`; the codec in `leviculum-core/src/mgmt_allow_store.rs:61` (magic) and `leviculum-core/src/mgmt_allow_store.rs:84` (eight identities at most) |
| The board loads it at boot and says so | `load_at_boot`, `leviculum-nrf/src/mgmt.rs:126`; the `[MGMT] disabled (no allow-list)` banner, `leviculum-nrf/src/mgmt.rs:163` |
| "Identified link plus allow-list" before any verb is answered, in the core and path-generic | `enable_remote_management`, `leviculum-core/src/node/mod.rs:1025`, registering `/status` under `RequestPolicy::AllowList` at `leviculum-core/src/node/mod.rs:1064` |
| A board answering a request over the mesh | `/status`, `leviculum-nrf/src/remote_status.rs` |
| Setting the list over the cable | `lnflash --management-identity` / `--clear-management`, `lnflash/src/mgmt.rs:253`; with `--port`, on one board only |
| Bringing a board out of DFU without a transfer | `scripts/dfu-exit.py` (BlueZ: `btmgmt` scan, `gatttool` write of SYS_RESET), selftest `just dfu-exit-selftest`; named in `lnmgmt dfu --help` |
| Writing a `GPREGRET` magic and resetting under a live SoftDevice, with no await between | `write_gpregret`, `leviculum-nrf/src/usb.rs:409`, the one seam both the UF2 touch (`leviculum-nrf/src/usb.rs:236-238`) and `/dfu` call, Codeberg #249 |
| The `/dfu` verb: registered beside `/status` under the same `RequestPolicy::AllowList`, only on a node built with `remote_dfu` (the boards; `lnsd` never serves it) | `leviculum-core/src/node/mod.rs:1060`, the builder flag at `leviculum-core/src/node/builder.rs:287`, the decision in the engine-pass macro at `leviculum-nrf/src/node_loop.rs:833` and the exit at the top of the main loop, `leviculum-nrf/src/node_loop.rs:874`, expanded once (in the macro, per arm, it cost 6.4 KiB of flash), the same in the T1000-E and solar node binaries |
| A refused request is visible: still dropped, nothing leaves because of it, but the core raises `NodeEvent::RequestRefused` with the requester (or none) | `leviculum-core/src/node/link_management.rs:2835`, the variant at `leviculum-core/src/node/event.rs:622` |
| What a `/dfu` event means, and the typed reply: one msgpack fixint, `0` accepted, `1` not listed, `2` no identity | `dfu_command`, `leviculum-core/src/node/remote_dfu.rs:131`; `DfuVerdict`, `leviculum-core/src/node/remote_dfu.rs:61` |
| The exit order: reply handed over, waited for until every carrier's queue reports it gone (bounded by 20 s, above the LoRa queue's 18 s stale-drop age), then `0xA8`, then reset | `exit_to_dfu`, `leviculum-core/src/node/remote_dfu.rs:280`, behind `DfuExit` (`leviculum-core/src/node/remote_dfu.rs:254`); the magic at `leviculum-core/src/node/remote_dfu.rs:42`, the bound at `leviculum-core/src/node/remote_dfu.rs:51`; the board's side in `leviculum-nrf/src/remote_dfu.rs:131` (hand-over) and `leviculum-nrf/src/remote_dfu.rs:136` (LoRa settled through the reply, serial queue empty, BLE census empty) |
| Both lines on the debug port, before the reply | `MGMT_DFU accepted=yes from=<identity> reason=accepted` and `MGMT_DFU accepted=no from=<identity\|none> reason=not-listed\|no-identity`, the body in `leviculum-nrf/log-line/src/lib.rs:195`; host side the `MGMT_DFU` trace event ([Structured event logs](../structured-event-logs.md)) |
| The sender, `lnmgmt dfu -R <transport hash> -i <identity> [-w <secs>]`: the verdict in words (`accepted: the board resets into BLE DFU now`, `refused: not listed`, `refused: no identity`), exit 0 only on accepted, a timeout said as one | the binary, `leviculum-cli/src/lnmgmt.rs:126` (words) and `leviculum-cli/src/lnmgmt.rs:135` (timeouts and failures); the library call the `lnflash` GUI can reuse, `send_dfu`, `leviculum-std/src/remote_dfu.rs:51`, on the flow `lnstatus -R` runs, extracted as `request_on_management`, `leviculum-std/src/remote_status.rs:232`; manual page `lnmgmt(1)`, shipped in the `leviculum` package |
| A boot counter that survives the trip | `record_at_boot`, `leviculum-nrf/src/boot_count.rs:76`, Codeberg #380 |
| A legacy DFU package and the init-packet constants the bootloader checks | for the T1000-E only, `scripts/t1000e-dfu-package.sh`; the constants checked are `ADAFRUIT_DEVICE_TYPE` (`Adafruit_nRF52_Bootloader/src/dfu_init.c:84`) and the SoftDevice id (`Adafruit_nRF52_Bootloader/src/dfu_init.c:178`) |

This page, written two days before #235 landed, said the control frame
"cannot be the one we have" and asked for the shape the propagation
node's control destination has: request over a link, identity taken
from the link, checked against an operator list, typed refusal when
absent (`answer_control`, `lnpnd/src/engine.rs:1535`; the check at
`lnpnd/src/engine.rs:1545`). That shape now exists on the board itself,
on the `rnstransport.remote.management` destination. The unauthenticated
USB envelope (#238, `TYPE_RESET` at `leviculum-core/src/envelope.rs:79`)
stays what it was, a cable protocol, and stage 1 does not touch it.

## What is left

**The `/dfu` verb is built** (table above). What the host tests hold
(`leviculum-core/src/node/remote_dfu.rs`, `mod tests`): a listed identity
is accepted and told so, an unlisted one and a link without identity are
refused with their typed reply, an empty list refuses the identity it
would otherwise hold, a node built without `remote_dfu` does not serve the
verb, a refused `/status` is not read as `/dfu`; the reply is handed over
and waited for before the magic, a carrier that never transmits is given
up on at exactly the bound, and the magic is `0xA8`. What they cannot
hold is the syscall and the reset on a board: that is the bench window's
(a T114 and a RAK into DFU, the hour below, a pin reset brings the
application back).

Two properties of the verb, unchanged from the design. Sending `/dfu`
twice is harmless: the second one either reaches a board already in DFU,
which is not listening, or a board that came back. Nothing about the
carrier matters to the board ([Interface isolation](interface-isolation.md));
the wait reads all three carriers' queues rather than the one the reply
went out on, so the core keeps the routing decision.

**The sender is built** (table above): `lnmgmt dfu`, a Leviculum client
beside `lnstatus` in `leviculum-cli`, not `lnflash --dfu`. Its `-R` is the
board's transport identity hash, the value `lnstatus -R` takes. What the
host tests hold (`leviculum-std/tests/remote_dfu_sender.rs`): a host node
standing in for a board, built with `remote_dfu` and an allow-list and
running the core's own `dfu_command` and `send_dfu_reply` from a core
processor, answers a listed identity with accepted and an unlisted one
with not-listed, and the sender reads both wire replies; on the same
scenario a node without the verb (as `lnsd`) leaves the sender with a
timeout and decides nothing. The CLI's own tests hold the arguments, the
words and the exit statuses. Why a separate binary:

* it is a Reticulum client and needs everything `lnstatus -R` already
  does: a shared instance, a link to `rnstransport.remote.management`,
  identifying with the management identity, one request. `lnflash`
  has none of that, and it is a USB tool that is a single static binary
  calling syscalls rather than daemons; giving it a Reticulum client
  spends that property for one verb;
* not inside `lnstatus` itself: that tool is held byte-identical to
  `rnstatus` for drop-in use, and a state-changing verb in a status tool
  is a surprise nobody should meet by mistyping a flag;
* the `lnflash` GUI can later call the same library function, which is
  where a "update this board" button belongs; the mesh request is a
  library call, not a binary.

**A package per board.** nRF Connect speaks Nordic legacy DFU and needs
one `.zip` from us and no code. The phone finds the board by its DFU name
(`HT-n5262-OTA` on a T114), not by `AdaDFU` and not by a service filter on
`1530`; see [Leaving DFU without hands](#leaving-dfu-without-hands). The T1000-E script already builds it;
the T114 image and the RAK4631 and Pocket images need the same wrapper
with their own `--sd-req` (the T114 and the rig RAK both report S140
7.3.0, the same `0x0123`; the Pocket's is read on the bench), and
`--dev-type 0x0052` for all.

**The phone needs nothing from us.** That stays true and stays the
point. Teaching `lnflash` to push the image over BLE would need a BLE
central (BlueZ through `bluer` or `btleplug`), a live D-Bus and a
running `bluetoothd`, and it spends the static-binary property
(`lnflash/Cargo.toml`, the `libc` dependency's comment). It may be the
right trade one day for a fleet; it is not part of stage 1.

## The measurement the reviewer owes on the bench

The source answers what the bootloader is written to do. The bench
answers what the builds on our boards actually do, and it is owed
because the T114's build is `-dirty` and the RAK's has no published
source. **The T114's half is done** (2026-11-10, results in the table at
the top and in [Leaving DFU without hands](#leaving-dfu-without-hands)):
no timeout in 61.5 minutes, the advertisement measured, the address rule
confirmed, and the way out was the Bluetooth SYS_RESET rather than a pin
reset. The RAK4631, the Pocket V2 and the T1000-E are still owed.

* **One hour in DFU, nobody connecting**, one T114 and one RAK (and
  the Pocket's `INFO_UF2.TXT` read beside them), entered with `0xA8` (until the verb exists, a debug build or the
  probe writes it). Watch the debug port: if the application comes
  back on its own, `BOOT_COUNT` says so on the next boot and the
  elapsed time is the timeout. `BOOT_COUNT` lives in flash and survives;
  `BOOT_TRACE` lives in retained RAM, which the SoftDevice the
  bootloader enables in OTA mode overwrites (`leviculum-nrf/memory.x:149`),
  so `prev_magic=absent` after a DFU trip is expected, not a fault. The
  source predicts no return within the hour.
* **Advertising per sniffer**: the name (the T114's is `HT-n5262-OTA`,
  not the source's `AdaDFU`), whether any service UUID is advertised (the
  T114 advertises none), the address and its type, and the interval
  (25 ms predicted), on both builds
  ([Bluetooth interfaces](bluetooth-interfaces.md)). nRF Connect on the
  rig phone answers the first three in one scan; `scripts/dfu-exit.py
  --dry-run` (or `--from-capture` on a btmon decode) prints them too.
* **Reset recovers**: one pin reset during the idle DFU hour brings the
  application back (predicted by `Adafruit_nRF52_Bootloader/src/main.c:243`).

## Failure modes

| What goes wrong | What the board is left as | Recovery |
| --- | --- | --- |
| Command lost in the mesh | Running firmware, nothing happened | Resend |
| Command refused (not on the allow list, or no identity) | Running firmware, typed refusal returned and logged | Fix the list over the cable |
| Board enters DFU, nobody arrives | Bootloader, advertising its DFU name (`HT-n5262-OTA` on the T114) indefinitely, off the mesh | `scripts/dfu-exit.py` from within Bluetooth range (SYS_RESET, measured on the T114), or any reset or power cycle |
| Transfer starts and is interrupted, no reset | Bootloader, old image erased, refuses a second start | Reset puts it in USB DFU: cable |
| Transfer interrupted, board resets | Bootloader in USB DFU, no BLE | Cable and UF2 (the RAK on 0.4.4 would stay in BLE DFU) |
| Image is for the wrong board | Refused by the init packet, or written and dark | Double tap, UF2, as today |

**What stays physical, unchanged.** A board whose application has
crashed answers no command, because nothing is running to answer it.
The double tap remains the only trigger that works regardless of what
is on the board, it needs a human, and on a Pocket V2 it needs a needle
([Recovery](../firmware/recovery.md), the hidden-pinhole caveat). Stage
1 does not shrink that set; it shrinks the set of *routine* updates
that fall into it.

## What stage 1 does not solve

Somebody has to stand within Bluetooth range of the board, and with
the bootloaders we carry an interrupted transfer turns that visit into
a cable visit. Where nobody can stand at all, the image itself would
have to travel the mesh; [OTA stage 2](ota-stage-2-mesh-image.md)
records what that would cost and why it is not on the register.
