# OTA stage 2: the image itself over the mesh

**Status: concept, not scheduled. Conditional on hardware two of our
three boards do not have.**

[OTA stage 1](ota-stage-1-ble-dfu.md) moves the *decision* over the mesh
and leaves the *bytes* to somebody standing within Bluetooth range. This
page is for the board where nobody can stand: the image travels the mesh
too, as a signed Reticulum Resource to the board's control destination,
is staged on external flash, and is written into the application window
by a copier that runs from RAM. A golden image and the boot counter
undo it when the new image does not come back.

Two numbers decide almost everything below, and neither is negotiable:
the application window is smaller than two images, and a 620 KB transfer
costs hours of a channel everybody else is also using.

## Why internal flash cannot do it

The nRF52840 has 1 MB of internal flash and it is already spoken for.
The SoftDevice sits at the bottom, the bootloader at the top, and what
is left is the application window: origin `0x27000`, length `0xB2000` —
729 088 B, 712 KiB (`FLASH`, `leviculum-nrf/memory.x:84`). Above it the
boot record takes a page (`BOOT`, `leviculum-nrf/memory.x:108`, Codeberg
#380) and the record store sixteen more (`STORE`,
`leviculum-nrf/memory.x:91`, Codeberg #384), then the three persistence
pages.

The image that `lnflash` wrote to a T114 on 2026-09-24 was 2 419 UF2
blocks of 256 payload bytes: **619 264 B, 605 KiB**. A staged second copy
beside the running one needs 1 238 528 B, 1 210 KiB. That does not fit in
712 KiB, and it does not fit even if the boot record and the record store
are given up and the whole writable window above the SoftDevice is
claimed: `0x27000` to `0xEA000` is 798 720 B, 780 KiB
([Flashing an LNode](lnode-flashing.md), "What a UF2 is allowed to
write") — still 440 KB short of two banks.

So there is no A/B bank internally, and the alternative — erase the
running image and write the new one from a transfer held in RAM — is not
an alternative at all. It fails on two counts: the board has 96 KiB of
heap (`HEAP_SIZE`, `leviculum-nrf/src/lib.rs:253`) against a 605 KiB
image, and a power cut during the write leaves a board with no image and
no copy of one. **Staging on flash the copier does not erase is not an
optimisation; it is the property that makes the operation survivable.**

## Which boards are in, and on what evidence

**T114 and RAK4631: out.** Neither carries QSPI flash. That is measured,
not read off a header — three units answered nothing to `05h`, `9Fh`,
`90h` or the datasheet reset while every pin followed our drive, and both
board files state `qspi_part: None`
(`leviculum-nrf/src/boards/t114.rs:184`,
`leviculum-nrf/src/boards/rak4631.rs:158`). The `EXTERNAL_FLASH_DEVICES`
lines in the vendor variant headers that once suggested otherwise sit
under comments denying the part — RAK's own reads "No onboard flash"
(`leviculum-nrf/src/boards/rak4631.rs:111-112`) — and
`leviculum-nrf/src/qspi.rs` carries the whole account. For these two
boards stage 1 is the entire answer, and that is why the two stages are
separate documents rather than two halves of one.

**SolarNode: conditional, and the condition is a boot line.** The XIAO
nRF52840 module's `CONFIG.qspi_part` is `Some(&crate::qspi::P25Q16H)`
(`leviculum-nrf/src/boards/solarnode.rs:402`), 2 MB
(`P25Q16H`, `leviculum-nrf/src/qspi.rs:357`). But that part number comes
from Seeed's variant headers and the wiring from Seeed's schematic, and
the schematic for the plain XIAO v1.1 draws the same footprint marked
**DNP** — do not populate. So the firmware does not assert the part, it
asks: `identify_at_boot` (`leviculum-nrf/src/qspi.rs:379`) reads the
JEDEC id once at boot and prints `[QSPI] JEDEC … state=ok` or does not.

**Nothing on this page is true of our SolarNode until a unit prints that
line.** The probe landed in the firmware on 2026-09-24 and has not been
run on our hardware. If it comes back with no part, stage 2 has no
hardware at all in this project and stays a document until some board
does.

## The budget on a 2 MB part, and the other claimant

Staging plus golden is 1 238 528 B of 2 097 152 — 59 % of the part —
leaving about 838 KiB. That is enough, and it is not so much that the
region layout can be left implicit, because **the same part is already
wanted by something else**: the record log, the message store of Codeberg
#384, mounts over the whole part today
(`log_store`, `leviculum-nrf/src/qspi.rs:963`, read-only and formatting
nothing, precisely because that decision had not been taken). Two
claimants and one part means one region map, decided once, in one place —
not two mounts that each believe they own sector 0. Whichever batch
first writes to that part owns the map, and stage 2 must not be that
batch by accident.

Erase granularity is 4 KiB (`ERASE_SIZE`, `leviculum-nrf/src/qspi.rs`),
so staging an image is 152 sector erases. Against the part's rated
endurance that is free; it is the *pattern* that matters, not the count —
a staging region rewritten in place from sector 0 every time wears one
end of the part and nothing else, which is the same argument the record
log already makes for itself
([An LXMF propagation node on a board](propagation-node-on-a-board.md)).

## The image arrives as a Resource and is never held

The image is a Reticulum Resource on a Link to the board's control
destination — stock Reticulum, the same mechanism `rncp` and `lncp` use.
The board writes each part to the staging region as it arrives and holds
none of the image in RAM.

That is not a preference. The board has 96 KiB of heap and the cost of
holding a copy of anything is measured: a 5 427 B propagation response
held 51 662 B live before Codeberg #384 B1 and 29 842 B after it, and
before either, a board died on a 5 446 B allocation between serving a
request and answering it — `PN_GET … bytes=5376` as the last line of one
boot and `PANIC_PMRT … "memory allocation of 5446 bytes failed"` on the
next (`leviculum-nrf/src/pn.rs:445-470`, pinned by
`leviculum-std/tests/mvr/pn_serve_peak_outgrows_the_board_heap.rs`). A
path that holds one whole copy of 5 KB killed a board; 605 KB is not a
question of tuning.

Two consequences follow and both are design constraints, not details:

* **The transfer must be resumable across a reboot.** Five hours of
  channel time (below) is far longer than the interval at which a field
  board reboots for its own reasons. The staged region and its header
  *are* the resume state; a transfer that starts from zero after every
  reset never completes on a board that reboots at all.
* **The staging region is untrusted until the whole image verifies.**
  Partial contents are exactly what an interrupted transfer leaves, and
  they must be indistinguishable from garbage to everything downstream.

## Header and signature: refuse before the first erase

Ahead of the staged copy sits a header, and it is checked in full before
the copier touches the application window:

| Field | Why it is there |
| --- | --- |
| magic + format version | A staging region holding a foreign or half-written thing reads as foreign, the same argument the boot record makes for its own magic (`leviculum-nrf/src/boot_count.rs`) |
| board family | An image for another pinout family bricks this board. The flash runner already refuses a wrong-SoftDevice board and a wrong UF2 volume before writing (`nrf-sd-guard`, `Justfile:209`); this is the same refusal, without an operator to read it |
| image length | Bounds the copy, and is what "the transfer is complete" is decided against |
| version | What the board says it is running, and what a rollback is a rollback *from* |
| hash over the image | Catches the interrupted transfer and the bad sector |
| Ed25519 signature over the header and the hash | Catches everything else |

`ed25519_dalek` is already a dependency of the core and builds for the
firmware target (`leviculum-core/src/identity.rs:63-65`), so verification
costs a public key compiled into the image and one pass over the staged
bytes. That pass is cheap against the five hours the transfer took.

**Why a signature, when stage 1's allow list already says who may
command.** The allow list authenticates a *peer on a live link*. The
staged image outlives that link, the reboot, and possibly the operator's
key: what the copier reads at 3 a.m. after a power cut has no link behind
it and no peer to ask. The allow list says who may command; the signature
says what may run. Neither substitutes for the other.

**And the order is load-bearing.** A signature checked after the erase is
not a check — by then the board has nothing to fall back to but the
golden image, which turns a refusable mistake into a rollback. Verify,
then erase.

## The copier, and what it must never depend on

One routine, running from RAM with the SoftDevice disabled: erase
`0x27000` for the image's length, write from the staging region, verify,
mark done, reset.

It must not depend on:

* **the image it is erasing** — no call into it, no vector table in it,
  no panic handler in it, no string in it;
* **the SoftDevice**, which owns flash timing while it is enabled and
  which the boot record already steps around by writing before
  `Softdevice::enable` (`leviculum-nrf/src/boot_count.rs`);
* **the heap, USB, BLE, the radio, the log path** — anything that might
  route through a region being erased or an allocator that might fail;
* **interrupts** it did not itself arm;
* **the transfer that produced the staged copy**, which finished hours
  or reboots ago.

It must be **idempotent under a power cut**, because that is the failure
it exists to survive: a cut halfway through leaves a partly written
application window, and the only correct behaviour on the next boot is to
copy again from the same source. That means "a copy is in progress" is
itself a flash record, written before the first erase, in a page the
copier never erases — the boot record's neighbourhood
(`BOOT`, `leviculum-nrf/memory.x:108`) is the shape, for the same reason
the boot record chose it: it must survive a power loss, which is the case
it exists for.

This is the one piece of code in the system whose bug is an
unrecoverable board in a place nobody can reach. It should be the
smallest, dullest, most heavily host-tested thing we own — the same
disposition `leviculum_boot_count` took, where a host test can cut the
power at every word boundary (`leviculum-nrf/src/boot_count.rs`).

## Golden image and the rollback trigger

The golden image is the last one that came back healthy, kept on the same
part. The trigger for restoring it is the boot counter that landed on
2026-09-24 (`record_at_boot`, `leviculum-nrf/src/boot_count.rs:122`,
Codeberg #380): one 16-byte record per boot with the raw `RESETREAS` and
whether retained RAM survived, appended to the `BOOT` page, one erase per
256 boots. It exists because a Pocket V2 restarted twice on a field walk
and every boot read `reset_reason=0x00000000` with `prev_magic=absent` —
a power loss takes retained RAM with it, and a rollback trigger that
lives in RAM is a rollback trigger that a sagging battery switches off.

The rule's shape: the copier marks the new image unproven; the
application clears the mark once it reaches a defined healthy point; N
consecutive boots with the mark still set restores the golden image.

Two parts of that are decisions this page deliberately does not take, and
naming them is more useful than guessing them:

* **What "healthy" means.** It must be later than "reached `main`" — a
  board that boots and cannot bring up its radio is precisely the failure
  that needs rolling back, and it reaches `main` every time. It must
  *not* be "heard a peer", or a quiet mesh looks like a broken image and
  the board rolls back a perfectly good update because nobody was
  talking.
* **What N is.** Too small and one unlucky brown-out undoes a good
  update; too large and a boot-looping board spends a day looping before
  it heals.

The counter is also the only evidence anybody will ever get from a board
in a hedge: `BOOT_COUNT n=… reset_reason=… retained=… since_erase=…`,
read on the next visit or over the mesh, says what the board did while
nobody was watching.

## What it costs the channel, in hours

Measured on 2026-09-24, `lora_lncp_push_to_python_50kb` on the rig:
five 50 KiB pushes at 869.525 MHz, SF7, BW 62.5 kHz, CR 4:5, **212–333 s
of wall clock each** with the airtime lock deliberately switched off for
the cell, and about **165 s of transmitter airtime each** — 109 parts of
491 B at roughly 1.50 s of airtime apiece.

Scaling to the 619 264 B image is a factor of 12.1:

| | at the cell's PHY (SF7/BW62.5) | at our default PHY (SF8/BW125) |
| --- | --- | --- |
| coded rate | 2 734 bit/s (342 B/s) | 3 125 bit/s (391 B/s) |
| transmitter airtime for one image | ~2 000 s (33 min) | ~1 750 s (29 min) |
| wall clock with no duty limit | 43–67 min | ~40–60 min |
| **wall clock under the lawful cap** | **~5.5 h** | **~4.9 h** |

869.463 MHz — our default carrier — sits in the 869.4–869.65 MHz
sub-band, whose lawful duty cycle is 10 %
(`etsi_eu868_duty_cycle`, `leviculum-core/src/rnode.rs:1285`;
[Regulatory airtime](regulatory-airtime.md)). Ten percent of an hour is
360 s of airtime, so 1 750–2 000 s of airtime is five hours of wall clock
however fast the modem is. **The PHY does not change the answer; the
regulation does.**

And it is worse than "five hours", because that budget is not spare
capacity:

* **The sending node spends its entire lawful hour on the image, for
  five hours.** It forwards nobody's traffic while it does. A transport
  node updating a neighbour stops being a transport node for an
  afternoon.
* **Every relay on the path pays it again**, once per hop, and pays it
  serially. A two-hop image is ten hours of two nodes' budgets.
* **Everyone else on the channel pays too.** The band is occupied ten
  percent of the time, continuously, for hours; every other node's
  pre-transmit window finds a busy channel more often and backs off
  ([The randomised pre-transmit window](csma-transmit-window.md)).

**Compression halves it at best.** The measurement above is deliberately
incompressible — `/dev/urandom`, so that 50 KiB of payload is 50 KiB on
the air and the durations compare — while a real ARM firmware image does
compress. How much is *unmeasured here*, and the measurement is one
command on the image we already build (`xz -9 < firmware.bin | wc -c`),
so no figure is invented for it: assume a halving, expect less, and
notice that halving five hours leaves two and a half. It changes the
scheduling of the operation and not its nature.

The arithmetic forces the conclusion, and the conclusion is the point of
the page: **stage 2 is for the board nobody can reach at all.** An update
over the mesh is an event the mesh is told about in advance, planned
around, and done once. A fleet update over LoRa is not a thing that
happens. Wherever somebody can get within Bluetooth range,
[stage 1](ota-stage-1-ble-dfu.md) does the same job for the cost of one
control frame.

## Python-RNS compatibility: untouched

Nothing here is on the wire between stacks. The transfer is a Resource on
a Link to a destination resolved the ordinary way, which is stock
Reticulum in both directions — a Python peer could be the sender without
knowing what it is sending. The header, the signature, the staging
layout, the copier and the rollback rule are all *behind* our own control
destination, visible to nothing but us, in the same sense the propagation
node's control destination is
([Python-RNS compatibility](python-rns-compatibility.md)). No new packet
type, no announce semantics, no change to any field a Python node reads.
