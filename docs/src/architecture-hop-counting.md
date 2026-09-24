# Hop counting

## Why this document exists

The hop counter is one unsigned byte in the packet header. It is also load bearing. It decides
which header form a packet takes, when a circulating packet is killed, which path replaces which,
and whether a link proof is accepted. Two stacks that disagree about it cannot establish links with
each other.

This page records the rules as the reference implements them, and where leviculum diverges. Every
claim cites a line in `reference/Reticulum/RNS/Transport.py` (or `Packet.py` / `Link.py`) so it can be
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

leviculum matches this as of 2026-07-10 (D3, fixed on branch `path-response-hops`). When a transport
node answers a path request from a network peer (`handle_path_request` case 2b, `transport.rs:6765`)
it now emits `self.storage.get_path(&requested_hash).map(|p| p.hops)`, the receipt-incremented stored
count, exactly as `:2956` does. It previously emitted `cached_packet.hops`, the AS-RECEIVED wire byte
(`set_announce_cache` stores the raw pre-increment buffer; the receipt increment at `transport.rs:1986`
touches only the in-memory packet). That value is `stored - 1`, so every peer learning through our
transport path response was one hop short, and the deficit COMPOUNDED on each re-learn through a
leviculum transport. Case 1 (local dest) and case 2a (local-client answer, explicit `+1`) were already
correct.

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

**A stack that suppresses the drop also suppresses the LINK-FAILURE healing path.** A relay that
rewrites a mismatching hop count so the proof is accepted makes the link succeed once and blocks
`clean_link_table` from ever re-requesting the path for that entry. It does NOT guarantee the entry
is never corrected at all: a fresh equal-or-shorter announce still replaces it via rule 6,
independent of the link-failure loop. So recurrence is a FIELD property (observed: a five-minute
heartbeat on hamster, 2026-07-10) — evidence that no corrective announce arrived, not a guarantee
the code makes it inevitable.

## Where leviculum diverges

Recorded 2026-07-10 against `reference/Reticulum` as vendored.

| Rule | Reference | leviculum | Verdict |
|------|-----------|-----------|---------|
| Receipt increment | `:1498` | `transport.rs:1891` | matches |
| IPC exception, instance side | `:1523` | `transport.rs:1750` | matches |
| IPC exception, client side | `:1525` | `transport.rs:1750` (else-arm of the `has_local_clients` gate) | matches — **fixed 2026-07-10 (D2, commit `06aadaff`); was absent** |
| Announce rebroadcast | `:2050` | `transport.rs:6708` | matches |
| Path table store | `:1909`, `:2055` | `transport.rs:3446` | matches |
| Path acceptance | `:1806`, `:2412` | `transport.rs:3542` (`should_update`) | matches |
| Path-response hop emission | `:2997` (`packet.hops = path_table[dst][IDX_PT_HOPS]`), `:618` | `transport.rs:6765` (case 2b emits the stored path-table count) | matches — **fixed 2026-07-10 (D3, commit `path-response-hops`); previously emitted `cached_packet.hops` = the pre-increment wire byte (`stored - 1`)** |
| Link entry fields | `:1615-1625` | `storage_types.rs:60 (destination_hash at :76)` | matches, including the destination hash |
| LRPROOF relay check | `:2215-2206` (single `== remaining_hops`, drop else; the `:1697` disjunction is gated OUT for LRPROOF at `:1687`) | `transport.rs:4213`; rewritten by default, DROPPED behind `lrproof_rewrite_on_asymmetry=false` | **deliberate deviation** (default); the flagged strict branch drops like the reference, but see the mapping caveat below |
| Healing, no path | `:737` | `transport.rs:7198` | matches |
| Healing, local client link (`taken_hops == 0`) | `:744` | `transport.rs:7455` | matches — **fixed 2026-07-10 (D1, commit `74ac655`); was absent** |
| Healing, destination direct | `:753` | `transport.rs:7206` | matches |
| Healing, initiator direct (`taken_hops == 1`) | `:775` | `transport.rs:7474` | matches |

### The deliberate deviation, and its cost

On a mismatch we log a warning and REWRITE the forwarded proof's hop count to the frozen value, so
that a strict Python client accepts it (`transport.rs:4237`, commit `5d0833d7`). It buys
interoperability today: without it, NomadNet cannot establish a link through our relay.

It also costs three things:

1. It suppresses the sensor. The link validates, `clean_link_table` skips it (`if entry.validated
   { continue; }`), no path request is issued, and the wrong path survives. Measured in the field:
   the same warning recurs on an exact five minute heartbeat, indefinitely.
2. It sometimes LOWERS the counter. Measured on miauhaus 2026-07-10: `packet_hops=7` rewritten to
   `3`. That is four hops of extra life handed to a packet that `max_hops` was meant to kill.
3. It overwrites a measurement with an assertion. Downstream consumers of `hops` receive what this
   relay believes rather than what the packet did.

The rewrite must stay until the cause is fixed and the warning is shown to fall silent. What is
MEASURED is that the warning recurs every ~300 s with the rewrite ON. That removing it would break
NomadNet is an INFERENCE (drop -> strict client rejects the proof -> link fails), not yet a
measurement: no flag-off live run has been done. Do not deploy the flag off without one.

### The strict behaviour now exists behind a flag

The reference-exact strict check is implemented behind `TransportConfig.lrproof_rewrite_on_asymmetry`
(`transport.rs`), default `true`. The default keeps the rewrite above unchanged, so this is a no-op
in the field. Set to `false`, the forward site DROPS a proof whose hop count matches neither frozen
operand rather than rewriting it:

* the `next_hop` direction (destination -> initiator) drops unless `packet.hops == remaining_hops`.
  For a proof this maps to `Transport.py:2176` — the SINGLE `== IDX_LT_REM_HOPS` check whose only
  else (`:2206`) drops. This is the arm the field case takes.

MAPPING CAVEAT (found by adversarial review 2026-07-10): the `:1656` disjunction does NOT apply to
LRPROOF at all — its transit block is gated at `Transport.py:1646` with `packet.context !=
RNS.Packet.LRPROOF`. So for proofs the reference has exactly ONE relay path (`:2174-2206`, single
check, drop else) and NO initiator-side LRPROOF forwarding. Our `received`-direction arm therefore
has no LRPROOF counterpart in the reference; it is practically moot (proofs flow
destination -> initiator), but it is a leviculum choice, not reference parity. Earlier drafts of
this page and a code comment mis-cited the `:1656`/`:1664`/`:1668` arms for proofs — corrected.

The drop is the healing SENSOR. Whether the loop actually CLOSES is NOT yet established. The mvr
(`mvr_hop_asymmetry.rs`, flag off) shows the sensor fires — the proof is dropped, the link stays
unvalidated, and `clean_link_table` issues a path request — but its convergence step is CIRCULAR and
must not be read as proof of healing: the path request is discarded (`handle_timeout()` result
dropped, no node answers it), and the short arm is relearned only because the test HAND-FEEDS a
fresh announce. That same injected announce would heal the rewrite-ON world identically (rule 6),
so the mvr does not isolate the flag as the cause of convergence. In the field, a path RESPONSE
inherits the responder's STORED count (rule 7, "staleness propagates"), so a re-request can relearn
the SAME stale count and loop "fail, request, fail". Convergence is guaranteed only for one-level
divergence answered by the correct next hop. This is the open risk the interop A/B and a live
flag-off run must settle before the default can change.

The flag stays `false`-capable but `true`-default until an interop A/B and a live NomadNet-retry
check confirm the strict drop heals on the air as it does in the mvr; only then can `false` become
the default.

### Upstream changed its mind: 1.5.x re-balances instead of dropping (Codeberg #330)

Everything above this line describes the reference as of 1.3.5, which is what
`reference/Reticulum` is pinned to and therefore what every interop test in this tree
measures against. RNS 1.5.x replaced the strict drop with a re-balance. The source facts,
read against 1.5.2 (`ea98db4f`, 2026-08-29) — every Python line number in this section is
1.5.2's and does NOT resolve inside the pinned `reference/Reticulum`, so the citation guard
cannot check it; re-read them against a 1.5.x checkout, never the submodule:

* `Transport.py:153` — `ALLOW_LINK_PATH_REBALANCE = True`, a class constant, no config surface.
* **Relay site, `Transport.py:2614-2634`.** When `packet.hops != link_entry[IDX_LT_REM_HOPS]`
  and the proof arrived on `IDX_LT_NH_IF`, the signature is validated *first*; if it is valid
  and the entry is not yet `IDX_LT_VALIDATED`, the relay ADOPTS the measurement —
  `link_entry[IDX_LT_REM_HOPS] = packet.hops` (`:2632`) and
  `path_entry[IDX_PT_HOPS] = packet.hops` for the link's destination (`:2634`). Control then
  falls into the unchanged `packet.hops == IDX_LT_REM_HOPS` forward arm, which now matches, so
  the proof is forwarded **carrying its own true hop count**. The re-balance happens at most
  once per link entry: the forward arm sets `IDX_LT_VALIDATED`, and the re-balance is gated on
  that flag being unset. That gate, not a hop equality, is 1.5.x's loop breaker here.
* **Terminus site, `Transport.py:2680-2707`.** For a pending link with
  `packet.hops != link.expected_hops` and `status == PENDING`, the signature is validated
  against `link_id + peer_pub + peer_sig_pub + signalling_bytes`; if valid and `link.rebalanced`
  is unset, `link.expected_hops = packet.hops` (`:2704`) and the path entry's hops follow
  (`:2707`). The unchanged `== expected_hops` check then matches and `validate_proof` runs.
  `Link.py:267-268` adds the two fields; `Link.py:525` re-adopts `expected_hops` from the RTT
  packet once the link is active.
* **No third site.** The general link-table repeat arm is untouched, and LRPROOF is still
  excluded from it. The MAPPING CAVEAT above still holds in 1.5.x: the reference has exactly
  one relay path for proofs and no initiator-side LRPROOF forwarding.

What that means for the three sites we have:

1. **Cross-interface relay arm.** We already deliver — that is the #38 rewrite. The difference
   is not delivery, it is bookkeeping: 1.5.x heals `remaining_hops` *and* the path entry and
   then tells the truth on the wire; we heal neither and rewrite the wire instead. 1.5.x's
   re-balance is the healing loop this page says the rewrite suppresses, reached without the
   link having to fail first.
2. **Terminus.** We have no hop gate at all. `handle_link_proof`
   (`node/link_management.rs`) checks phase, state and signature, never a hop count, and
   `expected_hops` does not exist anywhere in `leviculum-core`. So the half of #330 that reads
   "links over asymmetric paths form on Python but not on us" does not describe our initiator:
   ours accepts any hop count and always has. What ours does not do is 1.5.x's table healing.
3. **Shared-medium arm.** This is the one place we drop where 1.5.x forwards. `NH_IF` and
   `RCVD_IF` are the same interface there, so 1.5.x's `receiving_interface == IDX_LT_NH_IF`
   test passes and the re-balance arm fires (source read, not measured). We drop the proof as
   an echo, because on one medium the strict hop match is our only loop breaker — the
   `lora_3node_relay` storm of 2026-08-12, pinned by `mvr_lrproof_echo_storm.rs`. Adopting
   1.5.x here swaps that loop breaker for the `IDX_LT_VALIDATED` gate. That is a rig question,
   not a desk one.

**Why this is not a port.** Forwarding the proof with its true hop count is exactly what a
1.3.5 initiator rejects: `Transport.py:2228` in the pinned reference gates on
`packet.hops == link.expected_hops`. Adopting the relay site verbatim therefore re-opens #38
against every 1.3.5 peer in the mesh, and `lrproof_hop_undercount_interop_tests.rs` — which
drives a real Python initiator out of `reference/Reticulum` — passes today only because the
relay rewrites the count down to the frozen value. Upstream can do this because it fixed both
ends in the same release; we cannot assume both ends.

So #330 is a choice between three behaviours, and rule 5 below no longer decides it on its own
now that "the reference" names two generations that disagree:

* **a) keep the rewrite** — links form for 1.3.5 and 1.5.x initiators alike, tables stay stale,
  we keep lying about the count on the wire;
* **b) adopt 1.5.x verbatim** — tables heal, the wire is honest, links through us stop forming
  for 1.3.5 initiators over asymmetric paths;
* **c) heal the tables, keep the rewrite** — correct the *path* entry from the proof's
  measurement while still forwarding the frozen count, so the next link over that destination
  freezes the right `remaining_hops` and the asymmetry drains within one link lifetime. Neither
  reference does this, so it is a deviation-rule argument and needs the deviation-rule evidence.

Deciding between them is a measurement, not a reading: the interop A/B this page already
demands for the strict flag, run against both a 1.3.5 and a 1.5.x peer. The fixture for the
relay half already exists — `mvr_hop_asymmetry.rs` builds the honest asymmetric topology and
asserts both arms of `lrproof_rewrite_on_asymmetry` — so a fix pass starts from a working
reproduction, not from scratch.

## The ceiling, and what 1.5.x does at it

`PATHFINDER_M` is 128. In 1.3.5 that is a reachability limit and nothing else: a hop byte of 128 or
more is parsed, delivered and forwarded, and only the announce gate at `Transport.py:1750` cares.
In 1.5.x it is also a parse limit — `Packet.py:248` raises on a received hop byte of 128 or above
and `Transport.py:1356` refuses to emit one — so the same byte that merely travels too far on a
1.3.5 peer is unreadable to a 1.5.x one. Our receipt increment can reach it from a legal wire
value, which is why the emit gates ask `Transport::hop_ceiling()` rather than `config.max_hops`.
Receipt stays liberal. The walk is in
[Four things RNS 1.5.x changed](protocol-notes/rns-1-5-x-audit.md), which also records what
`local_hops_delta` does to the meaning of `hops == 0`.

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
   reference is right. Where 1.3.5 and 1.5.x disagree with EACH OTHER, this rule names no
   winner: see the 1.5.x re-balance section above before invoking it.

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
