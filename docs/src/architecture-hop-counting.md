# Hop counting

## Why this document exists

The hop counter is one unsigned byte in the packet header. It is also load bearing. It decides
which header form a packet takes, when a circulating packet is killed, which path replaces which,
and whether a link proof is accepted. Two stacks that disagree about it cannot establish links with
each other.

This page records the rules as the reference implements them, and where leviculum diverges. Every
claim cites a line in `vendor/Reticulum/RNS/Transport.py` (or `Packet.py` / `Link.py`) so it can be
checked rather than believed.

## The invariant

`packet.hops` counts the links a packet has traversed. Each node that receives it adds one,
including the receiving node itself. The IPC connection between a shared instance and one of its
local clients is not a link on the mesh and is never counted.

## Life of a hop counter

### 1. Birth

`Packet.py:135` sets `self.hops = 0`. It travels as header byte 1 (`Packet.py:181` on pack,
`Packet.py:245` on unpack). It is outside the signature, so a relay may legally change it.

### 2. Receipt

`Transport.py:1457`: `packet.hops += 1`, unconditionally, for every inbound packet.

### 3. The two IPC exceptions

`Transport.py:1478-1484`:

```python
if len(Transport.local_client_interfaces) > 0:
    if   Transport.is_local_client_interface(interface):    packet.hops -= 1
elif     Transport.interface_to_shared_instance(interface): packet.hops -= 1
```

Read the structure carefully. The `elif` belongs to the OUTER `if`. A node that has local clients
(it IS a shared instance) subtracts only for packets arriving from a client. A node with no local
clients (it IS a client of some instance) subtracts for packets arriving from that instance. The two
branches are mutually exclusive. The net effect is that an IPC hop is free in both directions.

After this step the counter has a meaning that the rest of the stack relies on:

* `hops == 0` the packet came from a local client
* `hops == 1` the packet came from a direct neighbour

### 4. Announce rebroadcast

`Transport.py:2009`: `new_announce.hops = packet.hops`. The already incremented value goes back on
the wire. Each relay therefore contributes exactly one, never two.

### 5. Path table

`Transport.py:1868`: `announce_hops = packet.hops`, written to `IDX_PT_HOPS` at
`Transport.py:2014`. A path entry records the length of the route the ANNOUNCE travelled to reach
us. This is not necessarily the length of the route a packet to that destination will take. See
"What remaining_hops actually means" below.

### 6. Path acceptance

`Transport.py:1765`: `if packet.hops <= Transport.path_table[dst][IDX_PT_HOPS]:` and
`Transport.py:2371`: `if announce_hops <= old_hops or time.time() > old_expires:`.

A path is replaced only by an equal or shorter one, or once the old one has expired. This rule is
what drives every node toward the same shortest tree, and it is why in a homogeneous mesh a stored
hop count and a live route length agree.

### 7. Cache re-emission and path responses

`Transport.py:326` and `:379` increment a cached announce on reload, with the comment "reading a
packet from cache is equivalent to receiving it again over an interface".

`Transport.py:2956`: `packet.hops = Transport.path_table[destination_hash][IDX_PT_HOPS]` when
answering a path request, and `Transport.py:618`: `new_packet.hops = announce_entry[4]`.

A path learned from a path response therefore inherits the responder's STORED count, not a freshly
measured one. Staleness propagates through this channel.

### 8. Link table

Built at `Transport.py:1615-1625`, keyed by the link id:

| Index | Contents | Source |
|-------|----------|--------|
| 3 `IDX_LT_REM_HOPS` | remaining hops | `path_table[dst][IDX_PT_HOPS]` (`:1563`) |
| 5 `IDX_LT_HOPS` | taken hops | `packet.hops` of the LinkRequest |
| 6 `IDX_LT_DSTHASH` | original destination hash | `packet.destination_hash` |

Note the trap: for a link packet, `packet.destination_hash` IS the link id (`Transport.py:1498`
looks the link table up with it). The address of the actual destination survives only at index 6,
and the healing loop below depends on it.

### 9. Link proof validation, the strict check

`Transport.py:1656`:

```python
if packet.hops == link_entry[IDX_LT_REM_HOPS] or packet.hops == link_entry[IDX_LT_HOPS]:
```

and `:1664` / `:1668` use WHICH of the two matched to choose the forwarding direction. On the
local client link path, `Transport.py:2176` applies the single `== IDX_LT_REM_HOPS` check. A proof
matching neither frozen value is dropped.

### 10. Endpoint check

`Link.py:282` sets `expected_hops = Transport.hops_to(destination)`, and `Transport.py:2228` checks
`packet.hops == link.expected_hops or link.expected_hops == PATHFINDER_M`. The establishment timeout
also scales with hops (`Link.py:207`).

### 11. Loop bound

`PATHFINDER_M = 128` (`Transport.py:63`). `Transport.py:1750` requires
`packet.hops < PATHFINDER_M + 1`. The counter is the only thing that terminates a circulating
packet. Lowering it hands the packet extra life.

### 12. Header form for local clients

`Transport.py:1356`, `:1367`, `:1565-1577`. `hops == 0` means the destination is directly
reachable, send Header1. `hops == 1` means it needs transport, convert to Header2 and attach a
transport id. A counter that is off by one changes the packet form.

## What `remaining_hops` actually means

It is the hop count of the route the ANNOUNCE took to reach this relay. It is frozen into the link
table when the LinkRequest is forwarded. The route the link then uses is chosen hop by hop by the
`next_hop` entry of every relay along the way. The two coincide only while all those relays agree
on the same tree. Rule 6 is what makes them agree in a homogeneous mesh.

Therefore a mismatch between `packet.hops` of a returning proof and the frozen `remaining_hops` is
not an arithmetic error. It is a statement that this relay's view of the topology disagrees with
the topology the packet actually traversed.

## The control loop that makes strictness safe

The strict check of step 9 is not a bare guard. It is the SENSOR of a healing loop:

1. A proof whose hop count matches neither frozen value is dropped.
2. The link is therefore never validated, and expires (`Transport.py:693`, `LINK_TIMEOUT`).
3. `clean_link_table` requests a fresh path for the ORIGINAL destination (index 6), throttled by
   `PATH_REQUEST_MI = 20` seconds (`Transport.py:83`), under four conditions:
   * `:710` no path is known
   * `:717` the failed link was initiated by a LOCAL CLIENT (`lr_taken_hops == 0`)
   * `:726` the destination was previously direct (`hops_to(dst) == 1`)
   * `:748` the initiator was direct (`lr_taken_hops == 1`)

   and marks the path unresponsive (`Transport.py:2721`) when transport is enabled.
4. The path is relearned. The next attempt agrees, and the link establishes.

**A stack that suppresses the drop also suppresses the healing.** That is the single most important
sentence on this page. A relay that rewrites a mismatching hop count so the proof is accepted makes
the link succeed once and guarantees that the wrong path is never corrected. The mismatch then
recurs for the lifetime of the path entry.

## Where leviculum diverges

Recorded 2026-07-10 against `vendor/Reticulum` as vendored.

| Rule | Reference | leviculum | Verdict |
|------|-----------|-----------|---------|
| Receipt increment | `:1457` | `transport.rs:1588` | matches |
| IPC exception, instance side | `:1482` | `transport.rs:1592` | matches |
| IPC exception, client side | `:1484` | absent | **defect**, a client of a shared instance counts one hop too many for everything it learns |
| Announce rebroadcast | `:2009` | `transport.rs:6235` | matches |
| Path table store | `:1868`, `:2014` | `transport.rs:3140` | matches |
| Path acceptance | `:1765`, `:2371` | `transport.rs:2610`, `:3056` | matches |
| Link entry fields | `:1615-1625` | `storage_types.rs:60 (destination_hash at :76)` | matches, including the destination hash |
| Strict proof check | `:1656` disjunction | `transport.rs:3746` per direction; mismatch rewritten by default, DROPPED behind `lrproof_rewrite_on_asymmetry=false` | **deliberate deviation** (default), reference-exact behind the flag, see below |
| Healing, no path | `:710` | `transport.rs:6542` | matches |
| Healing, local client link (`taken_hops == 0`) | `:717` | absent | **defect** |
| Healing, destination direct | `:726` | `transport.rs:6550` | matches |
| Healing, initiator direct (`taken_hops == 1`) | `:748` | `transport.rs:6558` | matches |

### The deliberate deviation, and its cost

On a mismatch we log a warning and REWRITE the forwarded proof's hop count to the frozen value, so
that a strict Python client accepts it (`transport.rs:3770`, commit `5d0833d7`). It buys
interoperability today: without it, NomadNet cannot establish a link through our relay.

It also costs three things:

1. It suppresses the sensor. The link validates, `clean_link_table` skips it (`if entry.validated
   { continue; }`), no path request is issued, and the wrong path survives. Measured in the field:
   the same warning recurs on an exact five minute heartbeat, indefinitely.
2. It sometimes LOWERS the counter. Measured on miauhaus 2026-07-10: `packet_hops=7` rewritten to
   `3`. That is four hops of extra life handed to a packet that `max_hops` was meant to kill.
3. It overwrites a measurement with an assertion. Downstream consumers of `hops` receive what this
   relay believes rather than what the packet did.

The rewrite must stay until the cause is fixed and the warning is shown to fall silent. Removing it
today breaks NomadNet within five minutes. That is a measurement, not a guess.

### The strict behaviour now exists behind a flag

The reference-exact strict check is implemented behind `TransportConfig.lrproof_rewrite_on_asymmetry`
(`transport.rs`), default `true`. The default keeps the rewrite above unchanged, so this is a no-op
in the field. Set to `false`, the forward site DROPS a proof whose hop count matches neither frozen
operand rather than rewriting it:

* the `next_hop` direction (destination -> initiator) drops unless `packet.hops == remaining_hops`,
  mapping to `Transport.py:2176` (LRPROOF / local-client-link) and `Transport.py:1664` (transit
  link) — both reference arms check `remaining_hops` on this direction;
* the `received` direction (initiator -> destination) drops unless `packet.hops == taken_hops`,
  mapping to `Transport.py:1668` (the transit-link arm of the `:1656` disjunction).

The drop is the healing sensor of "The control loop that makes strictness safe" above, and the mvr
proves the loop closes: `leviculum-core/src/node/mvr_hop_asymmetry.rs` runs the honest asymmetry with
the flag off and shows convergence — the first proof is dropped and the link stays unvalidated, the
timed-out local-client link makes `clean_link_table` request a fresh path for the original
destination, the short arm is relearned (A's path to R drops from 3 to 2 hops), and a second link
attempt agrees (`packet_hops == remaining_hops`, no warning) and establishes. Fail once, heal,
succeed.

The flag stays `false`-capable but `true`-default until an interop A/B and a live NomadNet-retry
check confirm the strict drop heals on the air as it does in the mvr; only then can `false` become
the default.

## Rules to obey

1. Never doctor a hop count to make a check pass. The check exists to expose a disagreement, and
   something downstream is listening for that disagreement.
2. `remaining_hops` is not the length of the route a packet will take.
3. For a link packet, `packet.destination_hash` is the link id. The original destination is a
   separate field. Do not use one where the other belongs. This mistake produced a silently useless
   diagnostic on 2026-07-10.
4. Any change to hop counting is checked against the reference first, and lands behind a test that
   fails before the change and passes after it.
5. When the reference and leviculum disagree about a compatibility relevant mechanism, the
   reference is right.

## Field evidence, 2026-07-10

Two relays, both running the same build, both logging both frozen counts and the interface branch.

```
hamster   packet_hops=4  hops=0  remaining_hops=5  dir=next_hop   (five times, one every 300 s)
hamster   packet_hops=4  hops=0  remaining_hops=3  dir=next_hop
miauhaus  packet_hops=7  hops=1  remaining_hops=3  dir=next_hop
miauhaus  packet_hops=4  hops=1  remaining_hops=3  dir=next_hop
```

`hops == 0` identifies a link initiated by a local client. Both signs of the mismatch occur, and the
magnitude reaches four. No constant per relay counting error can produce that, and the counting was
shown above to match the reference. What remains is the meaning of `remaining_hops`.
