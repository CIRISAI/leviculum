# Operating modes

A board with a battery runs in one of two modes: **Mesh**, the default,
where it is a full node, and **Tracker**, where it takes no part in the
mesh and spends everything on battery life (Lew's decision of
2026-10-20). Only Mesh exists today. This page records what is decided,
why, what is still open, and what a tracker needs from Reticulum to
deliver without listening. The user switches between the modes with the
button ([One button](one-button.md)).

## Mesh, the default

In Mesh mode the board is a full node, which is what every board does
today:

- **LoRa transport, with the receiver always on.** When the queue is
  empty the radio task parks in continuous receive and does not re-arm a
  window (`leviculum-nrf/src/lora.rs:2602-2604`). There is no receive
  duty cycle anywhere in the image.
- **BLE for Columba** ([Bluetooth interfaces](bluetooth-interfaces.md)).
- **The propagation node role** ([An LXMF propagation node on a
  board](propagation-node-on-a-board.md)). `pn.rs` is in every image: all
  four board binaries load its configuration at boot
  (`leviculum-nrf/src/node_loop.rs:720`,
  `leviculum-nrf/src/bin/rak4631.rs:652`,
  `leviculum-nrf/src/bin/solarnode.rs:707`,
  `leviculum-nrf/src/bin/t1000e.rs:626`).
- **Telemetry at the announce cadence** ([Announce
  cadence](announce-cadence.md), [Telemetry](telemetry.md)).

Nothing ever sleeps. The T1000-E board file says so in as many words:
the GNSS receiver's RTC supply goes low only in Meshtastic's deep sleep,
"which this firmware has none of" (`leviculum-nrf/src/boards/t1000e.rs:176-177`).

## Tracker

In Tracker mode the board does not take part in the mesh. It relays
nothing, and it does not listen except in a wake window. The receiver
and the radio sleep, and the GNSS receiver sleeps while the board is
still (#457 step two, see [Announce cadence](announce-cadence.md)). The
board wakes on motion, on a timer, or on the button, sends its position
at the cadence [Announce cadence](announce-cadence.md) already defines,
and sleeps again. Every choice in this mode is made for battery life.

## How a tracker delivers without listening

A board that does not listen still has to address its packets, and
Reticulum gives it what it needs only from packets it once heard. The
facts, with the reference lines:

- **A packet to a SINGLE destination is encrypted to that destination's
  public key.** The key comes from the destination's announce, once, or
  is provisioned.
- **A destination more than one hop away needs a path.** With a known
  path of more than one hop, a sender inserts the packet into transport:
  it rewrites a HEADER_1 packet into HEADER_2 with the next hop's
  transport id (`Transport.py:1120-1146`). A transport node relays only a
  packet whose transport id is its own (`Transport.py:1559-1573`). A
  HEADER_1 packet, which is what a sender without a path writes,
  therefore reaches direct neighbours only. The path, like the key, comes
  from the announce once.
- **LXMF opportunistic delivery is one packet, without a link,** with
  room for 295 bytes of content at default parameters
  (`LXMessage.py:68-79`). A position report fits; the telemetry page's
  report is about fifty bytes ([Telemetry](telemetry.md), "Delivery is
  opportunistic").
- **An upload to a propagation node needs a link**, but the node holds
  the message until the recipient fetches it.

The consequence for the design: the tracker keeps key and path across
sleep, in flash, and a wake window refreshes them. When the real
recipient is a phone that is not always in reach, the tracker's
recipient is a propagation node in reach, and the phone fetches from
there.

### What the mesh does with a tracker's announces

Transport nodes rate-limit the announces they rebroadcast. With
transport enabled and the interface leaving the keys unset, the target
is 3600 s with a grace of 5 (`Interface.py:89-91`, applied at
`Reticulum.py:830-833`; ours are `DEFAULT_AR_TARGET` and
`DEFAULT_AR_GRACE`, `leviculum-core/src/transport.rs:201-205`). Each
announce that arrives sooner than the target after the last one passed
counts a violation, one that arrives later forgives one, and a sixth
outstanding violation blocks rebroadcast until the last passed announce
plus the target (`Transport.py:1853-1865`, ours `update_announce_rate`,
`leviculum-core/src/transport.rs:14624-14656`). Worked through for the
300 s moving cadence: the first six announces, over 25 minutes, are
rebroadcast by each transit node; after that about one in thirteen is,
one every 65 minutes. The tracker's own neighbours hear every announce;
the mesh beyond them learns of its movement about once an hour. That is
a property of every Reticulum mesh, not of this firmware, and it is
another reason the position goes to a recipient rather than riding on
announces.

## Switching and persistence

The user switches with the long press ([One button](one-button.md)). The
mode persists in flash across reboot and power-off. The default is Mesh,
and `lnflash` sets the default at flash time the way it sets the
propagation node's stamp cost (`lnflash/src/main.rs:159`).

Decided 2026-10-25 (Lew): the 8 s press puts the board to sleep in
either mode, and a press wakes it ([One button](one-button.md)). A
sleeping board is no propagation node and no relay, so it is neither
mode while it sleeps.

## Open

Not decided yet, and marked as such:

- **BLE in Tracker mode.** The reviewer proposes: off, except for a one
  minute window after a button press or after motion.
- **The wake window lengths**, for the refresh of key and path and for
  the listening window a press opens.
- **The stationary timer cadence** in Tracker mode.
- **The persistence format** of the mode in flash.

## Prerequisite: a current measurement

The project has no current measurement of any board. The reviewer's
recommendation, recorded here: before the first Tracker pass, measure
today's draw in Mesh mode as a baseline, with a Nordic PPK2 as the
instrument. Without the baseline, no Tracker change can say what it
saved.

## Not yet built

All of Tracker mode: the mode itself and its persistence, receiver and
radio sleep, the GNSS sleep while still (#457 step two), the wake
sources, the stored key and path, and the `lnflash` default.

## See also

- [One button](one-button.md), for the gestures that switch the mode.
- [Announce cadence](announce-cadence.md), for the cadence a tracker
  reports at and the accelerometer gate.
- [Telemetry](telemetry.md), for the report a tracker sends.
- [Supported boards](../firmware/boards.md), for which boards carry a
  battery, a button and an accelerometer.
- [The ThinkNode M9](thinknode-m9.md), a handheld that runs both modes
  with a keyboard instead of one button.
- [Firmware layers](firmware-layers.md), for where the mode logic lives
  once it runs on more than one chip family.
