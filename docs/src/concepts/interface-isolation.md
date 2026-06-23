# Interface Isolation

The single most important architectural rule in Leviculum:

> **Only the interface knows the quirks of its carrier medium. The
> core, the transport, and the daemon are media-agnostic.**

A packet is a packet. At the boundary where the core hands bytes to
an interface, there is no distinction between an announce, a link
request, a data packet, or a resource chunk. They are all just bytes.

## What "media-agnostic core" means

`reticulum-core` decides *what* to send and to *which* interface. It
never decides *when* to put a frame on the wire, never spaces
transmissions, and never reasons about contention. The core processes
every packet with zero delay and emits an `Action::SendPacket` or
`Action::Broadcast` immediately (see [Architecture](../architecture.md)).

Because the core is the same code on a Linux daemon, an Android app,
and an nRF52 firmware image, it cannot afford to know whether the
medium underneath is a fibre-fast TCP socket or a half-duplex LoRa
radio whose airtime budget is measured in minutes. Medium awareness
lives entirely on the far side of the
[`Interface` trait](../architecture.md#interface-trait).

## What an interface is allowed to know

A LoRa interface knows it cannot transmit and receive at the same
time. It knows its `RadioSettings` (bandwidth, spreading factor,
coding rate) and therefore the airtime cost of any given frame. It
holds packets back, applies its own randomised pre-TX jitter on top of
the RNode firmware's CSMA, and refuses new frames when its airtime
budget is exhausted. Concretely:

- **Send-side jitter** — packets are queued, not sent immediately; the
  jitter window is sized from the radio parameters so two nodes do not
  re-collide (`reticulum-std/src/interfaces/rnode.rs:130`, the
  `compute_jitter_max_ms` doc comment, and the jitter queue at
  `:448`).
- **CSMA** — radio-level carrier sensing is handled by the RNode
  firmware; the interface defers collision avoidance to it rather than
  the core (`reticulum-std/src/interfaces/rnode.rs:451`).
- **Airtime backpressure** — a per-interface credit bucket charges
  every send by its airtime cost and signals `BufferFull` rather than
  flooding the serial queue
  (`reticulum-std/src/interfaces/airtime.rs:1`). This explicitly
  "never leaks into `reticulum-core`, so the `no_std` core stays free
  of host-side backpressure concerns" (same file).

A TCP interface has none of this. It just writes bytes
(`reticulum-std/src/interfaces/tcp.rs`).

## Why the rule is hard, not advisory

The rule binds anyone writing a fix. If a proposed fix for a
collision, contention, or duplex problem introduces an awareness flag
or counter in `transport.rs`, the `node/` modules, or the daemon
("is a link in flight?", "am I forwarding a link request?"), it is at
the *wrong layer*. Such a fix must be redirected into the interface.

Interface implementations are therefore free to diverge from
Python-Reticulum's thin serial-writer style — that divergence is
exactly where medium-specific intelligence belongs, and it satisfies
the project's [deviation rule](python-rns-compatibility.md#the-deviation-rule)
as long as wire and semantic compatibility are preserved.

## Consequences

- The same routing logic runs unchanged over LoRa, TCP, UDP, serial,
  and the in-process local socket.
- New media are added by implementing one trait, not by threading
  medium-specific cases through the protocol core.
- Collision-avoidance bugs are debugged in one place — the interface —
  instead of being smeared across six stack layers.

See also: [Storage and Embedding](storage-and-embedding.md) for the
parallel isolation of persistence and time, and the
[RNode protocol](../rnode-protocol.md) page for the LoRa carrier
details an interface must handle.
