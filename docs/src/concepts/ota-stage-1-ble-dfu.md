# OTA stage 1: entering BLE DFU on a command from the mesh

**Status: stage 1 active since 2026-11-09 (Lew).** Most of what it
needs on the board was built for Codeberg #235, and the board's `/dfu`
verb since; what is left is one sender, one package per board and the
cells that prove them (the last two sections).

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

| Question | 0.9.0 (T114) | 0.4.3 (RAK4631) |
| --- | --- | --- |
| Magic that selects BLE DFU | `0xA8`, `DFU_MAGIC_OTA_RESET` "entered by soft reset, SD is not inited yet" (`Adafruit_nRF52_Bootloader/src/main.c:110`); UF2 is `0x57` (`Adafruit_nRF52_Bootloader/src/main.c:112`), the value our touch writes (`leviculum-nrf/src/usb.rs:236-237`) | Same: `ldr.w r4, [r3, #1308]` reads `0x4000051C` (`GPREGRET`) at `0xF483A`, `cmp r4, #177` (`0xB1`) at `0xF483E`, `sub.w r1, r4, #168` (`0xA8`, the compare-to-zero idiom) at `0xF484A`, `0x4E` and `0x57` at `0xF486C`/`0xF4870` |
| How the magic is consumed | `_ota_dfu` is set from it (`Adafruit_nRF52_Bootloader/src/main.c:229`) and the register is cleared on entry (`Adafruit_nRF52_Bootloader/src/main.c:243`) | Cleared on entry: `str.w r1, [r2, #1308]` with `r1 = 0` at `0xF489C` |
| Does it leave DFU mode by itself | **No.** OTA entry calls `bootloader_dfu_start(_ota_dfu, 0, false)` (`Adafruit_nRF52_Bootloader/src/main.c:305`); the only startup timer is armed on the serial branch, never when `ota` is true (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/bootloader.c:333-347`); advertising runs with `BLE_GAP_ADV_TIMEOUT_GENERAL_UNLIMITED` (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:58`); the one advertising timeout that ends DFU counts down directed advertising (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:808-815`), which is used only when an application handed the bootloader bond data (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:1034`); our firmware never does | **No, as far as the image shows.** The call site takes the timeout `3000` only when the application asked for single-tap reset or the magic was `0x57` or `0x4E`, and `0` otherwise (`movw r1, #3000` at `0xF4A98`, `mov r1, r2` with `r2 = 0` at `0xF4AA0`, `bl` at `0xF4AA4`), the same shape as `Adafruit_nRF52_Bootloader/src/main.c:301-306`. The advertising duration inside the transport is not decoded; the bench hour settles it |
| What it advertises | Name `AdaDFU` (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:50`, put into the advertisement at `Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:645`); the Nordic legacy DFU service `0x1530` (`Adafruit_nRF52_Bootloader/lib/sdk11/components/ble/ble_services/ble_dfu/ble_dfu.h:36`) on base `23D1BCEA-5F78-2315-DEEF-1212…` (`Adafruit_nRF52_Bootloader/lib/sdk11/components/ble/ble_services/ble_dfu/ble_dfu.c:472`), i.e. `00001530-1212-EFDE-1523-785FEABCD123`, as a 128-bit UUID (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:646`); connectable undirected every 25 ms (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:57`) | `AdaDFU` at file offset `0xB72C` of the image and the same 128-bit base at `0xB761`; interval not decoded |
| Address | The SoftDevice's default address with its lowest byte plus one (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:1047`): a static random address, not resolvable, and not the one our firmware advertises under, so a phone sees a new device | Not decoded; the sniffer answers it |
| An aborted transfer | **The old application is gone.** The build is single-bank (`Adafruit_nRF52_Bootloader/Makefile:159`); the start packet erases the application area in OTA mode (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_single_bank.c:99`) and the bootloader records bank 0 invalid (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/bootloader.c:292`). A dropped link restarts advertising (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_transport_ble.c:764-770`), but a second start packet is refused until a reset, because the state is no longer idle (`Adafruit_nRF52_Bootloader/lib/sdk11/components/libraries/bootloader_dfu/dfu_single_bank.c:313`). After any reset `GPREGRET` is `0`, the application is invalid, and the bootloader enters DFU on the **USB** branch, not BLE (`Adafruit_nRF52_Bootloader/src/main.c:288-296`) | Same outcome, by RAK's own account: their 0.4.4 changelog lists "if a BLE OTA firmware update fails or is interrupted, the device stays in BLE OTA / DFU mode so another attempt can be made (instead of getting stuck in USB DFU/UF2 boot mode)" (WisBlock `d3da249c`, `bootloader/RAK4630/README.md` line 14) |

Two consequences follow, and both change what this page used to say.

**DFU mode does not end by itself.** A board told to enter it and never
visited advertises `AdaDFU` until power is lost or it is reset. A reset
of any kind brings the application back, because the bootloader cleared
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
published does. Updating the bootloader itself is out of scope here.

## What is built (Codeberg #235 and its neighbours)

| Piece | Where |
| --- | --- |
| Allow-list as a flash record in the protected band; a blank page decodes to "nobody" | `LMGA` record, 136 B, `leviculum-nrf/src/telemetry.rs:253`; the codec in `leviculum-core/src/mgmt_allow_store.rs:61` (magic) and `leviculum-core/src/mgmt_allow_store.rs:84` (eight identities at most) |
| The board loads it at boot and says so | `load_at_boot`, `leviculum-nrf/src/mgmt.rs:126`; the `[MGMT] disabled (no allow-list)` banner, `leviculum-nrf/src/mgmt.rs:163` |
| "Identified link plus allow-list" before any verb is answered, in the core and path-generic | `enable_remote_management`, `leviculum-core/src/node/mod.rs:907`, registering `/status` under `RequestPolicy::AllowList` at `leviculum-core/src/node/mod.rs:946` |
| A board answering a request over the mesh | `/status`, `leviculum-nrf/src/remote_status.rs` |
| Setting the list over the cable | `lnflash --management-identity` / `--clear-management`, `lnflash/src/mgmt.rs:253` |
| Writing a `GPREGRET` magic and resetting under a live SoftDevice, with no await between | `write_gpregret`, `leviculum-nrf/src/usb.rs:409`, the one seam both the UF2 touch (`leviculum-nrf/src/usb.rs:236-238`) and `/dfu` call, Codeberg #249 |
| The `/dfu` verb: registered beside `/status` under the same `RequestPolicy::AllowList`, only on a node built with `remote_dfu` (the boards; `lnsd` never serves it) | `leviculum-core/src/node/mod.rs:942`, the builder flag at `leviculum-core/src/node/builder.rs:287`, the decision in the engine-pass macro at `leviculum-nrf/src/node_loop.rs:828` and the exit at the top of the main loop, `leviculum-nrf/src/node_loop.rs:869`, expanded once (in the macro, per arm, it cost 6.4 KiB of flash), the same in the T1000-E and solar node binaries |
| A refused request is visible: still dropped, nothing leaves because of it, but the core raises `NodeEvent::RequestRefused` with the requester (or none) | `leviculum-core/src/node/link_management.rs:2462`, the variant at `leviculum-core/src/node/event.rs:523` |
| What a `/dfu` event means, and the typed reply: one msgpack fixint, `0` accepted, `1` not listed, `2` no identity | `dfu_command`, `leviculum-core/src/node/remote_dfu.rs:131`; `DfuVerdict`, `leviculum-core/src/node/remote_dfu.rs:61` |
| The exit order: reply handed over, waited for until every carrier's queue reports it gone (bounded by 20 s, above the LoRa queue's 18 s stale-drop age), then `0xA8`, then reset | `exit_to_dfu`, `leviculum-core/src/node/remote_dfu.rs:280`, behind `DfuExit` (`leviculum-core/src/node/remote_dfu.rs:254`); the magic at `leviculum-core/src/node/remote_dfu.rs:42`, the bound at `leviculum-core/src/node/remote_dfu.rs:51`; the board's side in `leviculum-nrf/src/remote_dfu.rs:131` (hand-over) and `leviculum-nrf/src/remote_dfu.rs:136` (LoRa settled through the reply, serial queue empty, BLE census empty) |
| Both lines on the debug port, before the reply | `MGMT_DFU accepted=yes from=<identity> reason=accepted` and `MGMT_DFU accepted=no from=<identity\|none> reason=not-listed\|no-identity`, the body in `leviculum-nrf/log-line/src/lib.rs:195`; host side the `MGMT_DFU` trace event ([Structured event logs](../structured-event-logs.md)) |
| A boot counter that survives the trip | `record_at_boot`, `leviculum-nrf/src/boot_count.rs:76`, Codeberg #380 |
| A legacy DFU package and the init-packet constants the bootloader checks | for the T1000-E only, `scripts/t1000e-dfu-package.sh`; the constants checked are `ADAFRUIT_DEVICE_TYPE` (`Adafruit_nRF52_Bootloader/src/dfu_init.c:84`) and the SoftDevice id (`Adafruit_nRF52_Bootloader/src/dfu_init.c:178`) |

This page, written two days before #235 landed, said the control frame
"cannot be the one we have" and asked for the shape the propagation
node's control destination has: request over a link, identity taken
from the link, checked against an operator list, typed refusal when
absent (`answer_control`, `lnpnd/src/engine.rs:1533`; the check at
`lnpnd/src/engine.rs:1543`). That shape now exists on the board itself,
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

**The sender.** A new Leviculum client beside `lnstatus` in
`leviculum-cli` (working name `lnmgmt dfu -R <transport hash> -i
<identity>`), not `lnflash --dfu`:

* it is a Reticulum client and needs everything `lnstatus -R` already
  does: a shared instance, a link to `rnstransport.remote.management`,
  identifying with the management identity, one request
  (`fetch_remote_status`, `leviculum-std/src/remote_status.rs:187`; the
  `-R` and `-i` flags, `leviculum-cli/src/lnstatus.rs:145`). `lnflash`
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
one `.zip` from us and no code. The T1000-E script already builds it;
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
source.

* **One hour in DFU, nobody connecting**, one T114 and one RAK (and
  the Pocket's `INFO_UF2.TXT` read beside them), entered with `0xA8` (until the verb exists, a debug build or the
  probe writes it). Watch the debug port: if the application comes
  back on its own, `BOOT_COUNT` says so on the next boot and the
  elapsed time is the timeout. `BOOT_COUNT` lives in flash and survives;
  `BOOT_TRACE` lives in retained RAM, which the SoftDevice the
  bootloader enables in OTA mode overwrites (`leviculum-nrf/memory.x:149`),
  so `prev_magic=absent` after a DFU trip is expected, not a fault. The
  source predicts no return within the hour.
* **Advertising per sniffer**: the name (`AdaDFU`), the 128-bit service
  UUID, the address and its type, and the interval (25 ms predicted),
  on both builds ([Bluetooth interfaces](bluetooth-interfaces.md)).
  nRF Connect on the rig phone answers the first three in one scan.
* **Reset recovers**: one pin reset during the idle DFU hour brings the
  application back (predicted by `Adafruit_nRF52_Bootloader/src/main.c:243`).

## Failure modes

| What goes wrong | What the board is left as | Recovery |
| --- | --- | --- |
| Command lost in the mesh | Running firmware, nothing happened | Resend |
| Command refused (not on the allow list, or no identity) | Running firmware, typed refusal returned and logged | Fix the list over the cable |
| Board enters DFU, nobody arrives | Bootloader, advertising `AdaDFU` indefinitely, off the mesh | Any reset or power cycle returns the application |
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
