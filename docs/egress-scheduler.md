# The egress scheduler (+ciris)

One queue per node between the core's `dispatch_actions` and the interface
channels. It gives what a node sends a priority order, an optional pacing
rate set by the owner, and a staleness bound for live media. It is always
on, and has no runtime off switch. Instead it has a property, held by unit
witnesses: with nothing queued it adds no delay and no reordering, and with
no rate set it never queues.

Code: `leviculum-core/src/egress.rs` (the class read from the header),
`leviculum-std/src/egress.rs` (the queue, the pacer, the pump),
`leviculum-std/src/driver/egress.rs` (the node API), and
`LinkHandle::send_unordered_as` in `leviculum-std/src/driver/stream/datagram.rs`.

## Why

Under a capped uplink (edge's mesh bench, 300 kbit/s) live call media arrived
6 to 19 s late. Everything a node sent went straight into its interface
channels and from there into the kernel's socket queue, in arrival order. A
sender-side budget above leviculum (edge 45.3) fixed most of the video
latency, but three things are below its reach:

1. **The first burst.** In the first second of a sudden cap about 450
   packets (7.7 s of queue) reached the kernel FIFO before anyone could
   react, and nothing can recall them from there.
2. **Loop delay.** The owner's only backlog signal was a Channel round trip,
   which sees a queue only after the queue's own depth, so a budget built on
   it oscillates.
3. **Unbudgeted bytes.** leviculum's own proofs (about 28 kbit/s for a
   member's inbound control) and the rest of a Resource transfer already
   started.

With the scheduler, frames wait in a queue that knows their class instead
of in a FIFO that does not: a rate set below the cap keeps the kernel queue
short, the queue's depth can be read directly, and every byte the node
sends is in it.

## Classes

The class is read in `dispatch_actions` from the wire type and the context
byte, before the IFAC wrap masks the header, and handed to the interface
beside the frame (`Interface::hint_next_frame`, next to the journey tag).
IFAC'd interfaces are therefore scheduled exactly like the others. Nothing
is decrypted and nothing is added to the wire.

| Header | Core `FrameClass` | Scheduled as |
|---|---|---|
| DATA, Channel / Request / Response / plain / anything else | Control | Control, or the link's hint |
| PROOF (delivery, LRPROOF, resource proof) | Proof | Proof |
| LINKREQUEST; DATA LRRTT / LinkIdentify / LinkClose / Keepalive | LinkSetup | LinkSetup |
| DATA Datagram (leviculum#102) | Media | Audio or Video |
| ANNOUNCE (path responses too); a path request | Announce | Announce |
| DATA Resource, ResourceAdv/Req/Hmu/Prf/Icl/Rcl | Bulk | Bulk |

The order, highest first: **Control and Proof** (one tier, oldest first
between them), **LinkSetup**, **Audio**, **Video**, **Announce**, **Bulk**.
Within a class, FIFO.

The header cannot tell a Channel on an A/V link from one on any other link,
nor audio from video, so the owner says so, without any wire change:

- **Per link**, `ReticulumNode::set_link_egress_class(link, Some(class))`:
  the link's Control frames are scheduled as `class`; with `Audio` or `Video`
  its datagrams default to that kind. Proofs, link setup and resource frames
  keep their own class. Forgotten when the link closes.
- **Per send**, `LinkHandle::send_unordered_as(data, MediaKind::Audio)` (and
  `try_send_unordered_as`) for a datagram, and
  `LinkHandle::send_as(data, EgressClass::Bulk)` (and `try_send_as`) for a
  Channel message: the frame is tagged by a fingerprint of its bytes (its
  length and the head of the token's random IV), looked up once when it
  reaches the scheduler. So one link can carry urgent control (Control) and
  bulk replication (Bulk) apart. A Channel frame sent as a media class is
  still never dropped by age. A Resource needs no hint: its frames are Bulk
  by their context.

An untagged datagram on an unmarked link is Video.

## The floor

Strict priority would let a full load of media starve Announce and Bulk.
The two together are guaranteed a share: every byte sent from a higher class
earns them `share / (100 - share)` bytes of credit (default share 10 %, so
they get a tenth of what is sent), and credit also accrues with time at an
absolute minimum (default 4 kbit/s, capped at the rate). When their head
frame is covered by the credit it goes ahead of everything else. Credit
banks no more than one frame (at least 1500 bytes) while they wait, so the
floor is a share, not a burst. `ReticulumNode::set_egress_floor`.

## Pacing

`ReticulumNode::set_egress_rate(Some(bits_per_second))` paces everything
the node's network interfaces send; `None` (the default) paces nothing. A
token bucket of 10 ms of the rate (at least 1500 bytes) lets a frame go when
its balance is not negative; a frame larger than the balance goes and the
debt is paid before the next. The rate counts the frames as handed to the
interfaces: IFAC included, HDLC/IP/UDP/TCP overhead not, so set it a little
under the uplink. A frame that has to wait is sent by a pump task, one per
node, alive only while something is queued.

## Head-drop by age

A datagram older than its class's bound when it reaches the head of the
queue is dropped and counted, never sent (default 800 ms for both Audio and
Video, the live-media staleness budget;
`ReticulumNode::set_egress_media_age_bound`). Only a frame the header said
is a datagram is ever dropped by age. Control, Proof, LinkSetup, Announce,
Bulk, and a Channel frame a link hint moved into a media class, never are.

## The depth reading

`ReticulumNode::egress_depth() -> EgressDepth`: the rate in force, and per
class the queued packets, the queued bytes and the age of the oldest queued
packet. `egress_stats() -> EgressStats`: per class the packets and bytes
sent, dropped (by age, or because their interface went away) and refused,
plus how many frames went straight through and how many waited. Both are
copies taken under the scheduler's lock in one pass, with no allocation.

## Idle means no change

- Nothing queued for the frame's interface at its class or above, and (with
  a rate) nothing queued anywhere at its class or above, the bucket not in
  debt and the floor not due: the frame goes to the interface channel on the
  same call, in order. Witness: `idle_adds_no_delay_and_no_reordering`.
- No rate: nothing is ever queued. A full interface channel refuses the
  frame with `BufferFull` exactly as before, so the driver's retry queue,
  the datagram shed (leviculum#102) and the per-peer circuit breaker
  (leviculum#66) see what they saw. Witness:
  `no_rate_leaves_throughput_unchanged` (the scheduler accepts and refuses
  exactly what a bare `try_send` would).

## Interfaces: what is observable

The scheduler respects each interface channel's backpressure: it never
hands a channel more than the channel takes. A frame the channel refuses
under a rate stays at the head of its queue, the interface is skipped for
2 ms, and order holds (`a_full_channel_holds_frames_in_order`). A queue is
bounded per interface and class (256 KiB for Control, Proof and LinkSetup,
128 KiB per media class, 64 KiB for Announce, 256 KiB for Bulk); past it a
frame is refused with `BufferFull`, so a peer that stops draining is refused
and its breaker opens as before, rather than growing the heap.

What the scheduler can and cannot see, per kind:

| Kind | Observable | Not observable |
|---|---|---|
| TCP | channel occupancy; the writer task's `write_all` blocks when the kernel send buffer is full, so a full kernel buffer shows as a full channel | the bytes in the kernel send buffer (`SIOCOUTQ` is not read), anything past the host |
| UDP | channel occupancy | the socket's `send_to` almost never blocks: the qdisc, the NIC and a bottleneck past the host are invisible, so pacing is the only control |
| Punched direct UDP | as UDP | as UDP; and its punch, ack, MTU-probe and keepalive frames are written by the interface task itself and bypass the scheduler (one small frame per keepalive interval) |

Exempt, and sent as before: serial, RNode, KISS, BLE, pipe and local IPC.
The radios carry their own airtime credit and high/normal queue, and pacing
a loopback client at the uplink's rate would be wrong. TCP, UDP (punched
too), I2P, Auto, byte-channel and unknown kinds are scheduled.

## Known limits

- Channel retransmits are scheduled as Control: a retransmit is encrypted
  afresh, so the per-send hint (keyed by the first frame's bytes) does not
  survive it. Safe (Control is never dropped), but not shaped. A Resource
  is always Bulk; a class for a Resource, or retransmits that keep their
  class, would need the core to stamp every frame it emits (deferred).

- The class of a frame the driver's retry queue re-sends after `BufferFull`
  is read again from its bytes; on an IFAC'd interface those are masked, so
  it is scheduled as Control (never dropped).
- A per-link hint is keyed by the link's wire id. After an establishment
  retry the initiator's events keep the first id; the hint is resolved
  through `wire_link_id` when set, but the close event names the first id,
  so a re-keyed link's hint stays until the bound (4096) or until cleared.
- Frames queued when the node stops are dropped with the scheduler.
- There is no test-only shaping seam in the production crates; the witnesses
  drive the scheduler with explicit instants.
