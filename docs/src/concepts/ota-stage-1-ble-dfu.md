# OTA stage 1: entering BLE DFU on a command from the mesh

**Status: concept, not scheduled.**

Every firmware update this project has ever shipped ended with a hand on
the board: a cable, or a needle in a pinhole
([Flashing an LNode](lnode-flashing.md)). That is fine for a board on a
desk and wrong for the one on the roof, in the hedge, or two valleys
away. Stage 1 splits the problem along the seam where it is cheapest to
split it:

> **The decision travels the mesh. The bytes do not.**

A command over Reticulum — any carrier, any hop count — puts the board
into the bootloader's BLE DFU mode. Somebody then walks up to within
Bluetooth range with a phone and pushes the image. Nothing about the
image itself crosses the radio. [OTA stage 2](ota-stage-2-mesh-image.md)
is the document for when nobody can walk up at all, and it costs orders
of magnitude more.

## What the bootloader already does for us

All three board types carry a factory Adafruit nRF52 bootloader, and
that bootloader — not our firmware — owns DFU. Besides the UF2
mass-storage mode the flash runner uses, it has an over-the-air mode
speaking Nordic legacy DFU over BLE. Both modes are selected the same
way: a magic value in `GPREGRET`, the retained register that survives a
soft reset, read by the bootloader on the next boot.

We already drive one of them. The 1200-baud touch writes
`DFU_MAGIC_UF2_RESET` (`leviculum-nrf/src/usb.rs:230`) and resets, and
the bootloader comes up as a mass-storage volume. The OTA mode is a
different constant written to the same register at the same point by the
same code path.

The tree already knows the OTA mode exists, from a direction that had
nothing to do with flashing: `memory.x` reasons about it because in OTA
mode the bootloader enables the SoftDevice itself, whose RAM then reaches
up over the retained band (the `RETAINED` comment,
`leviculum-nrf/memory.x:142`, and [Recovery](../firmware/recovery.md),
step 2).

**Open, and it must be closed by reading rather than remembering: the
exact magic byte.** `INFO_UF2.TXT` names the bootloader build each board
carries (0.9.0 on the T114, 0.4.3 on the RAK — see
[Flashing an LNode](lnode-flashing.md), "What the bootloader tells you"),
and the constant belongs to that build's source, not to anybody's memory
of the Adafruit tree. Writing a wrong magic is harmless — the bootloader
ignores what it does not recognise and the board boots normally — which
makes this cheap to settle on the bench and unacceptable to guess in the
document.

## The control frame, and why it cannot be the one we have

We already have a control plane to the firmware: the #238 envelope, one
framing for every host command, with `TYPE_RESET` among them
(`leviculum-core/src/envelope.rs:79`). It is unauthenticated, and
deliberately so — read its header
(`leviculum-core/src/envelope.rs`, "Framed control envelope for the LNode
USB channel"): every argument in it is about *framing* and about not
colliding with Reticulum packets, and none is about *who is allowed to
send this*, because the answer was "whoever holds the cable". A frame
that arrives over the mesh has no cable behind it, so the envelope's
trust model does not travel with its wire format.

The pattern that does travel is already in this tree, on the propagation
node's control destination: the request runs over a Link, the remote's
identity hash is taken from that link, and it is checked against an
allow list before any verb is answered — with a typed refusal, not
silence, when it is absent or unlisted (`answer_control`,
`lnpnd/src/engine.rs:988`; the check itself at
`lnpnd/src/engine.rs:1001`). The list is operator configuration
(`control_allowed`, `lnpnd/src/config.rs:532`) and the node's own
identity is always on it. Codeberg #384 part 4 built that because the
reference has it; stage 1 needs exactly the same shape for a different
verb.

What a board needs on top of the daemon's version:

* **The list has to survive a reset**, so it is a flash record, and it
  belongs in the band the bootloader declines to overwrite — the same
  band the telemetry target, the radio config and the identity live in
  (`leviculum-nrf/memory.x`, the `0xEA000`–`0xEC000` pages). The
  telemetry target is the closest existing model for the record shape:
  magic, version, checksum, and a blank page decoding to "nobody"
  rather than to a garbage identity
  (`leviculum-core/src/telemetry_target_store.rs`). A board whose
  allow list decodes to "nobody" accepts no remote DFU command at all,
  which is the correct default for a board that was never configured.
* **A replay of a captured frame must not work.** A Link is not a
  replayable object — it is established, identified on, and forward
  secret ([Cryptographic identity and forward
  secrecy](identity-and-forward-secrecy.md)) — so requiring the command
  to arrive on an identified link already carries most of this. What it
  does not carry is the operator's own mistake: the same command sent
  twice. That is harmless here (a board already in DFU is not made worse
  by being told again) and it is worth stating rather than discovering.
* **Nothing about the carrier.** Whether the command arrived over LoRa,
  BLE or TCP, and over how many hops, is not the board's business
  ([Interface isolation](interface-isolation.md)). A command that only
  works at one hop is a command that does not solve this page's problem.

## What the board does on the command

Validate, acknowledge, write the magic, reset. In that order, and the
order is the whole of it.

The acknowledgement has to leave before the reset, because in this
scenario there is no second channel: on the cable the operator sees the
port disappear and the volume appear, and over the mesh the ack is the
only evidence the command was ever received. A board that resets first
and acks never is indistinguishable, from the far end, from a board that
never heard anything.

The write itself has a hot-path constraint that the UF2 path already
pays and that a mesh path pays in full: between the `GPREGRET` write and
`sys_reset()` there must be no await, no allocation and no logging, and
under a live SoftDevice the write must go through the SoC syscalls
rather than the register, or it records a bogus MWU panic per attempt
(`leviculum-nrf/src/usb.rs:230-258`, Codeberg #249). A mesh-borne
command always arrives with the SoftDevice enabled, so only the syscall
branch of that code is ever exercised — the branch the cable case takes
least often.

## What the bootloader advertises, and what disappears

While the bootloader is in OTA mode, our firmware is not running. Our
BLE identity is gone, the Columba service is gone
(`leviculum-nrf/src/ble/columba.rs:119`), the LoRa interface is gone,
the board is off the mesh. To a phone it is a different device with a
different name, and to the mesh it has vanished. That is not a
side-effect to be engineered away; it is what DFU *is*, and it is the
reason the timeout question below is the important one on this page.

What it advertises — the service UUID, the device name, whether the
address is resolvable — is a **measurement, not a recollection**, and
this project has a sniffer and the tooling to take it
([Bluetooth interfaces](bluetooth-interfaces.md)); nRF Connect on a
phone answers it in one scan without any of that. Nothing downstream
should be designed against a remembered UUID.

## The open question: does DFU mode ever come back on its own

**Open.** It has two possible answers and they differ in how dangerous
stage 1 is:

* **It times out and boots the application again.** Then a command sent
  to a board nobody reaches costs one reboot and one gap in the mesh,
  and the boot counter records it (`BOOT_COUNT n=… reset_reason=…`,
  Codeberg #380). Stage 1 is safe to use on any board, including one
  that cannot be reached physically.
* **It waits indefinitely for a connection.** Then a command sent by
  mistake — or one whose operator's plan changed — takes the board off
  the mesh until somebody physically resets it. On the roof that is a
  ladder; in the hedge it is a walk; two valleys away it is the end of
  that node until spring.

**How to settle it, and it is one afternoon on the bench:** put one
board into OTA mode, connect nothing, and watch the debug port. If the
application comes back, `BOOT_TRACE` and `BOOT_COUNT` say so on the next
boot and the elapsed time is the timeout
(`leviculum-nrf/src/boot_count.rs:122`, Codeberg #380). Hold it for at
least an hour before concluding there is no timeout; a short watch that
sees nothing has measured nothing. The bootloader's own source for the
build on the board is the second reading, and the two together are the
answer.

Until that measurement exists, the honest scope of stage 1 is boards
somebody *can* still reach on foot — which is most of them, and already
worth having, because reaching a board on foot with a phone is much
cheaper than reaching it with a laptop and a cable.

## Failure modes, and why an interrupted transfer is survivable

| What goes wrong | What the board is left as | Recovery |
| --- | --- | --- |
| Command lost in the mesh | Running firmware, nothing happened | Resend |
| Command refused (not on the allow list) | Running firmware, typed refusal returned | Fix the list over the cable |
| Board enters DFU, nobody arrives | Bootloader, off the mesh | The open question above |
| Transfer starts and is interrupted | Bootloader, incomplete image staged | Reconnect and push again |
| Image is for the wrong board | Bootloader, refused or written and dark | Double tap, UF2, as today |

The fourth row is the one that makes stage 1 worth doing at all. A DFU
that dies halfway leaves the *bootloader* intact, because nothing in
this path ever writes the bootloader region: it sits above the
bootloader's own `USER_FLASH_END` and every mechanism here declines
writes there ([Flashing an LNode](lnode-flashing.md), "What a UF2 is
allowed to write"). An interrupted transfer therefore produces a board
sitting in DFU waiting to be told again — not a dark board. A bad *UF2*
write, by contrast, produces a board that boots into nothing and
enumerates nothing, which is the state that costs a pinhole and a
needle.

**What stays physical, unchanged.** A board whose application has
crashed answers no command, over any carrier, because nothing is running
to answer it. The double tap remains the only trigger that works
regardless of what is on the board, it needs a human, and on a Pocket V2
it needs a needle
([Recovery](../firmware/recovery.md), the hidden-pinhole caveat). Stage 1
does not shrink that set; it shrinks the set of *routine* updates that
fall into it.

## lnflash, and the phone that needs nothing from us

A phone running nRF Connect speaks Nordic legacy DFU today. It needs one
`.zip` package from us and no code at all, which means stage 1's useful
half — the control frame — is the only thing that has to be built before
the first remote update is possible.

Teaching `lnflash` the same trick is a separate and much larger decision.
It would need a BLE central stack (BlueZ through `bluer` or `btleplug`),
the legacy DFU control-point and packet characteristics, the init packet
and the package format — and, more expensively, a live D-Bus and a
running `bluetoothd` underneath. `lnflash` today is a single static
binary that shells out to nothing and calls syscalls rather than
programs, and that is a stated property of the bundle, not an accident
(`lnflash/Cargo.toml`, the `libc` dependency's comment;
[Flashing an LNode](lnode-flashing.md)). Adding a BLE central spends
that property. It may still be the right trade one day — for a fleet, a
phone in the loop is the bottleneck — but it is not free and must not
be smuggled in as an implementation detail of this page.

## What stage 1 does not solve

Somebody has to stand within Bluetooth range of the board. Where that is
impossible, the image itself has to travel the mesh, and
[OTA stage 2](ota-stage-2-mesh-image.md) measures what that costs — in
hours of channel time, in hardware two of our three boards do not have,
and in the one piece of code in this system whose bug is an
unrecoverable board.
