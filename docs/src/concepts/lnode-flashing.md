# Flashing an LNode

Every board we flash is an nRF52840 carrying a factory UF2 bootloader.
That bootloader, not the firmware on top of it, is the part that decides
what a flashing tool can do. The rule that follows:

> **The application is not the board's identity. The bootloader is.
> Anything a tool needs to know before writing, it learns from the
> bootloader, never from the firmware currently running.**

A T114 may arrive carrying Meshtastic, Meshcore, microReticulum, RNode
firmware or our own. Each picks its own USB identity, and a crashed one
picks none. The bootloader underneath is the same in all five cases,
answers on a fixed USB ID, and publishes what it is in a text file.

## The three states a board can be in

**Application.** Our firmware enumerates `1209:0001` (T114) or
`1209:0002` (RAK4631), from `usb_vid`/`usb_pid` in
`leviculum-nrf/src/boards/t114.rs:137` and
`leviculum-nrf/src/boards/rak4631.rs:125`. Two CDC ports: interface 00
is the debug log, interface 02 the Reticulum transport. Both IDs are
squatted pid.codes test IDs, flagged as a TODO at
`leviculum-nrf/src/usb.rs:102`. Heltec stock firmware uses `239a:8071`.

**Bootloader (UF2/DFU).** A different USB ID entirely, which is why an
application-ID match can never be true while a board sits in DFU
(`leviculum-nrf/tools/uf2-runner.sh:306`). Measured on the rig:

| board | bootloader USB ID | mass-storage label |
| --- | --- | --- |
| T114 | `239a:0071` "Adafruit HT-n5262" | `HT-n5262` |
| RAK4631 | `239a:0029` "Adafruit WisBlock RAK4631" | `RAK4631` |

**Dark.** Crashed firmware never enumerates at all. No touch reaches it;
only a physical double-tap does.

## Getting into the bootloader

Two mechanisms, and only one of them is ours to control.

**1200-baud touch.** The host opens a CDC port at exactly 1200 baud.
Our firmware answers the resulting `SET_LINE_CODING` in
`leviculum-nrf/src/usb.rs:167`, writes `DFU_MAGIC_UF2_RESET` (`0x57`) to
`GPREGRET` at `0x4000_051C` and resets. The bootloader reads that
retained register on the next boot and stays in mass-storage mode.
Measured latency from `stty ... 1200` to the bootloader appearing on
USB: 5 s on the T114, 3 s on the RAK.

**Double-tap RESET.** The bootloader's own mechanism, independent of any
firmware: it sets a RAM flag at startup and stays in DFU if a second
reset arrives while the flag is still live.

The touch only exists if the running firmware implements it. Ours does.
Stock Meshtastic does not, which is why a first flash away from
Meshtastic needs the manual double-tap (`Justfile:492`); for that case
Meshtastic offers its own admin command, wrapped as `just dfu-rak4631`.
For Meshcore, microReticulum and RNode firmware on nRF we have not
measured it.

**This is the load-bearing limit for any "fully automatic" tool.** There
is no universal software trigger. The double-tap is the only mechanism
that works regardless of what is running, and it needs a human. A tool
can be fully automatic for boards already carrying our firmware, which
is every re-flash, and must fall back to one clearly announced key press
otherwise.

## What the bootloader tells you

Mounting the mass-storage drive gives three files: `INFO_UF2.TXT`,
`INDEX.HTM` and `CURRENT.UF2`. The first is the entire basis for
deciding whether a board can be flashed. Read from the rig, verbatim
(CRLF line endings):

```
UF2 Bootloader 0.9.0-2-g836c8dc-dirty lib/nrfx (v2.0.0) lib/tinyusb (0.12.0-145-g9775e7691) lib/uf2 (remotes/origin/configupdate-9-gadbb8c7)
Model: HT-n5262
Board-ID: HT-n5262
Date: Jul  9 2024
SoftDevice: S140 7.3.0
```

```
UF2 Bootloader 0.4.3
Model: WisBlock RAK4631 Board
Board-ID: WisBlock-RAK4631-Board
Date: May 20 2023
Ver: 0.4.3
SoftDevice: S140 7.3.0
```

The `SoftDevice:` line is generated at runtime by the bootloader from
the SoftDevice actually installed, so it answers the version question
directly rather than by inference. The T114's `Board-ID` is exactly
`HT-n5262`, not a substring of something longer, which retroactively
justifies the substring match at
`leviculum-nrf/tools/uf2-runner.sh:206`.

Note the T114 bootloader build date matches Heltec's published
`HT-n5262-bootloader-20240709.hex`, so that board still carries its
factory bootloader.

## What a UF2 is allowed to write

The bootloader validates every block's target address before writing.
From `src/usb/uf2/uf2cfg.h` upstream:

```c
#define USER_FLASH_START   MBR_SIZE   // skip MBR included in SD hex
#define USER_FLASH_END     (BOOTLOADER_REGION_START - DFU_APP_DATA_RESERVED)
```

`in_app_space()` accepts `USER_FLASH_START <= addr < USER_FLASH_END`;
blocks below the window are skipped silently while still reporting
success, and blocks at or above it are rejected outright.

Both bounds are measurable without reading a single constant, because
the bootloader generates `CURRENT.UF2` from exactly that window. On both
rig boards it covers `0x1000` to `0x000EA000`. So `USER_FLASH_START` is
`0x1000`, one MBR page, and `USER_FLASH_END` is `0xEA000`.

The consequence is the important part: **the writable window opens
directly above the MBR and includes the whole SoftDevice region.** An
image converted from a SoftDevice hex has its MBR blocks declined and
the rest installed, which is precisely what the `skip MBR included in SD
hex` comment describes. A SoftDevice can therefore be replaced through
the ordinary mass-storage path, without touching the bootloader.

**This contradicts a comment in `leviculum-nrf/memory.x:14`**, which
computes safe application space as `0xEC000 - 0x27000` (788 KiB). The
bootloader stops accepting at `0xEA000`, so the real ceiling is
`0xC3000` (780 KiB). Our image is 330 KiB
(`0x27000`-`0x79900`), so nothing is broken today, but a firmware that
grew past 780 KiB would hit rejected blocks rather than the 788 KiB the
comment promises. The 8 KiB difference is `DFU_APP_DATA_RESERVED`, which
is also why the identity page at `0xEC000`
(`leviculum-nrf/src/boards/t114.rs:144`) survives every UF2 flash: it
lies above what the bootloader will write.

Family IDs seen in practice:

| family | meaning | writes |
| --- | --- | --- |
| `0xADA52840` | nRF52840 application | application region |
| `0x239A0071` / `0x239A0029` | board-specific application | application region |
| `0xD663823C` | bootloader self-update | MBR, `0xF4000` region, `0xFD800`, UICR `0x10001000` |

Only the last one touches the bootloader itself. **A flashing tool must
never emit it.** With the bootloader intact, an interrupted flash is
recoverable: the board comes back into DFU and can be rewritten. Replace
the bootloader and a failure needs SWD to undo. There is a precedent for
the general risk: a bad application build once left the RAK4631 in a
HardFault boot loop that required opening the case to reach the internal
reset button (commit `43d25830`).

## The SoftDevice

Our firmware links against S140 v7.x and places its application at
`0x27000` (`leviculum-nrf/memory.x:15`). A board carrying S140 6.1.1
puts the boundary at `0x26000` instead, so the version is not cosmetic.

**The image lives outside this repo.** No SoftDevice binary, download
URL or checksum is stored here; the crate dependency
(`leviculum-nrf/Cargo.toml:76`) supplies Rust bindings, not the blob.
The only copy on our machines is inside the Meshtastic firmware checkout
at `~/coding/meshtastic/bin/s140_nrf52_7.3.0_softdevice.hex`, present on
both hosts. Parsed, it covers `0x0`-`0xB00` (MBR) and
`0x1000`-`0x26498` (the SoftDevice itself, 149 KiB), which lands exactly
below our `0x27000` origin once page-aligned.

### The mismatch is a soft brick, not a dead board

Flashing our application onto a board still carrying 6.1.1 produces a
device that goes dark: the old SoftDevice forwards to `0x26000`, finds
no vector table there because our image starts a page higher, and
crashes before USB initialises. No CDC ports, no bootloader drive,
nothing on the bus. It looks hardware-dead and is not. The bootloader
region at `0xF4000` is never touched by an application flash, so a
physical double-tap always brings the UF2 drive back. The 1200-baud
touch is useless here, because the application never runs far enough to
answer it.

Confirmed on 2026-08-08 on the T114 with serial `183004F712B4A7FE`,
which had been written off as bricked for weeks. Its `INFO_UF2.TXT` read
`SoftDevice: S140 6.1.1`. That board was never part of the 2026-05
spike, which names only `DEC9947DAD9D2869`, so it is direct evidence for
the factory state: **factory T114 boards ship S140 6.1.1**, as
`leviculum-nrf/memory.x:5` claims.

### Installing the SoftDevice

`adafruit-nrfutil`'s serial DFU protocol does not work against the
Heltec bootloader; the 2026-05 spike got "Timed out waiting for
acknowledgement" (`f517a172`). Do not retry it. The mass-storage path
works, and needs no SWD probe:

    python3 ~/coding/meshtastic/bin/uf2conv.py -f 0xADA52840 -c \
      -o s140_7.3.0.uf2 \
      ~/coding/meshtastic/bin/s140_nrf52_7.3.0_softdevice.hex

Copy the result onto the mounted bootloader drive and `sync`. The
application flashed earlier boots immediately afterwards; it was intact
all along, only the SoftDevice beneath it was wrong.

The conversion is deterministic, so its output can be checked before
anything is written. Reproduced 2026-08-09:

| property | value |
| --- | --- |
| output size | 311 296 bytes, 608 blocks |
| family | `0xADA52840` |
| ranges | `0x0`-`0xB00` and `0x1000`-`0x26500` |
| highest byte touched | `0x26500`, below the `0x27000` app base |

The last row is the one that matters: the update cannot reach the
application, which is why the app survives it.

**Eleven of those 608 blocks are never written.** They carry the MBR
below `0x1000` and the bootloader declines them, silently and with a
success return. The block counter still sees all 608 arrive and reboots
on the last one, so nothing about the transfer looks unusual. This is
harmless, since the MBR is already present and identical, but it means
an application-family UF2 can never replace an MBR, and a report that
counts copied blocks is not evidence that all of them landed.

## CURRENT.UF2 as a backup

The bootloader exposes the installed flash as `CURRENT.UF2`, 1.9 MB
covering `0x1000`-`0xEA000` under the board-specific family ID. Filtering
it to blocks at or above `0x27000` and renumbering `blockNo`/`numBlocks`
yields a restorable application image. Verified on both rig boards on
2026-08-09: read, filtered to 3120 of 3728 blocks, written back, and
each board returned with its original serial and firmware
(`[FW_BUILD] git_sha=bb7c4f64` on the T114), `PANIC_COUNT total=0`.

That makes a flash reversible for the user who wants their previous
firmware back, at the cost of one file copy before writing. The
SoftDevice portion of the dump is not needed for restore and is filtered
out; keeping it would only re-write identical bytes.

## Practical details that bite

**The USB serial number changes between modes.** The T114 reports
`183004F712B4A7FE` as an application and `12B4A7FE183004F7` in the
bootloader: the two 32-bit words are swapped. Anything correlating a
device across app to bootloader to app must know both forms. The
existing runner is unaffected because it only compares serials in
application mode (`leviculum-nrf/tools/uf2-runner.sh:314`).

**Writing needs root.** The mass-storage device appears as `/dev/sdX`
owned `root:disk`. Automounting assumes a desktop stack that a headless
host does not have. A single self-contained binary can read USB identity
from sysfs and issue the touch through termios without any external
tool, but it cannot write the drive unprivileged.

**A successful write ends in a kernel error.** The bootloader reboots
the moment the final UF2 block lands, while the filesystem still wants
to flush metadata, producing `device offline error ... lost async page
write`. This is the normal completion path, not a failure
(`leviculum-nrf/tools/uf2-runner.sh:274`).

**A copy returning 0 does not mean the flash took.** Verify that the
application re-enumerated and that the bootloader drive is gone
(`leviculum-nrf/tools/uf2-runner.sh:335`). Stronger still, read the
periodic `[FW_BUILD]` banner off the debug port and compare the git SHA,
as `scripts/flash-lnodes-from-head.sh:133` does.

## Open questions

- Does the touch handler exist in Meshcore, microReticulum or RNode
  firmware on nRF? Unmeasured; assume no, and fall back to the
  double-tap prompt.
- May we ship the Nordic SoftDevice blob ourselves? Meshtastic vendors
  it under GPL-3, which is precedent rather than a licence reading. Until
  that is settled, the image is reachable only through a Meshtastic
  checkout, which is a hidden dependency the clone-and-deploy policy does
  not tolerate.
- Whether boards leaving the factory *today* still carry 6.1.1 is
  unknown; the measured board is one unit from one batch. A tool must
  read `INFO_UF2.TXT` and decide, never assume a version.
