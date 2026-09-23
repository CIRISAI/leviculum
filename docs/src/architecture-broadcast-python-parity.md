# Broadcast behaviour: Python-RNS parity reference

This document is the source-of-truth reference that our Rust
`leviculum-core` broadcast code must match. It records what
Python-Reticulum does for every broadcast-related mechanism,
citing `reference/Reticulum/RNS/Transport.py` (and neighbouring files)
by line. The companion mapping table at the end records the
Rust-side implementation or intentional divergence for each item.

The rule (Lew, 2026-04-15): Leviculum matches Python-RNS exactly
for on-wire packet counts, packet types, and protocol semantics.
Timing may diverge — jitter-window shape and interface pacing are
free — as long as the counts and types stay identical.

**State of the citations (2026-09-23).** Every Rust-side citation on
this page was re-resolved in that pass and is current. The Python side
was only spot-checked, and the vendored `RNS/` tree has moved under it
since the page was written: the checks that were made are corrected in
place below, but **the line numbers into `Transport.py`, `Packet.py`,
`Destination.py`, `Interface.py` and `Reticulum.py` that are not
mentioned here have not been re-established** and must be treated as
stale until someone walks them. Confirmed still correct: the constants
at `Transport.py:68`, `:69`, `:70` and `:83`, the dedup storage at
`:106`, `:107` and `:175`, the retry loop at `:576-591`,
`Transport.request_path` at `:2771`, and the management-announce
citations `:193`, `:194`, `:283` and `:963`. Corrected below: the
dedup check site, `packet_hashlist_prev`, the `PATHFINDER_RW`
constant, the announce-table insert and its local-client special case,
and the `outbound`/`inbound` entry points. Everything else on the
Python side is unverified. Section 14 says why this is its own task.

## 1. Overview: what can appear on the wire

Python-Reticulum emits five distinct packet classes that can be
broadcast or unicast:

| Class | Packet type | Scope | Who originates |
|---|---|---|---|
| Self-announce | `Packet.ANNOUNCE` | Broadcast | `Destination.announce()` |
| Forwarded announce | `Packet.ANNOUNCE` | Broadcast | Transport relay on received announce |
| Path-request | `Packet.DATA` with `transport_type = BROADCAST` | Broadcast | `Transport.request_path()` or client call |
| Path-response | `Packet.ANNOUNCE` with `context = PATH_RESPONSE` | Targeted | Transport answering a path-request |
| Link-request | `Packet.LINKREQUEST` | Unicast | `Link.__init__` on initiator |

Everything below walks each class.

## 2. Self-originated announce

### Trigger

`Destination.announce(app_data, path_response=False, ...)` at
`reference/Reticulum/RNS/Destination.py:243`. Builds an announce
packet, calls `announce_packet.send()` once at line 322.

### On-wire behaviour

`Packet.send()` at `reference/Reticulum/RNS/Packet.py:273-299` calls
`Transport.outbound(self)` exactly once and returns a receipt (or
`False`). There is **no retry loop** on the send path. A second
call on the same packet raises `IOError` (Packet.py guard).

### Fan-out across interfaces

Inside `Transport.outbound()` at
`reference/Reticulum/RNS/Transport.py:1092` (the interior line numbers
in this section are unverified, see the banner above): for broadcast
packets (the "else" branch after the targeted-path and
transport-id branches), the code iterates `Transport.interfaces`
(line 1027) and transmits on each. There is **no
`if interface != packet.receiving_interface` filter** in the
announce path. For self-originated announces `receiving_interface`
is `None` anyway (the packet was created locally) so the question
is moot, but the point is relevant when we contrast with the
forwarded-announce path below.

Mode-based filtering is applied in this loop at lines 1040-1084
for `MODE_ACCESS_POINT`, `MODE_ROAMING`, `MODE_BOUNDARY`. These
modes suppress the rebroadcast on specific interfaces depending
on where the destination sits in the mesh. Bandwidth-cap logic
(lines 1089-1162) defers transmissions when the interface is
saturated.

**Summary: Python self-announce = exactly 1 on-wire broadcast per
call, via one-shot `Packet.send()`. Count = 1.**

## 3. Received-for-forwarding announce

### Reception

`Transport.inbound(raw, interface)` at `Transport.py:1389` is
the entry point for everything received on an interface. The
packet-hash dedup check at line 1376 is:

```python
if not packet.packet_hash in Transport.packet_hashlist and
   not packet.packet_hash in Transport.packet_hashlist_prev:
    return True
```

`Transport.packet_hashlist` at `Transport.py:106` is `set()`.
`Transport.packet_hashlist_prev` at line 107 is the rolling
previous window used to keep the dedup memory constant-bounded.
A duplicate return here bails out of `inbound()` before any
announce-specific handling. This is the only mechanism that
prevents the same packet from being processed twice — critical
for the broadcast-back-to-source echo pattern that B1 relies on.

### Insertion into announce_table

For announces (`packet.packet_type == ANNOUNCE`) that pass dedup,
the code path at `Transport.py:1866-1908` initialises an
`announce_table` entry:

```python
retries            = 0                                # line 1866
local_rebroadcasts = 0                                # line 1868
block_rebroadcasts = False                            # line 1869
attached_interface = None                             # line 1870
retransmit_timeout = now + (RNS.rand() * PATHFINDER_RW)  # line 1872
```

`PATHFINDER_RW = 0.5` (seconds) at line 70, so the first
retransmission is scheduled within 0–500 ms of receipt.

Line 1891-1895 is the special case for announces that arrived
**from a local client** (shared-instance peer over the local
socket):

```python
if Transport.from_local_client(packet):
    retransmit_timeout = now
    retries = Transport.PATHFINDER_R
```

This sets `retries = 1` right away. Combined with the retry-loop
guard below, this makes local-client-sourced announces fire
**only 1 time** from the scheduler, not 2.

### Retry loop

The periodic job at `Transport.py:576-591` walks `announce_table`:

```python
for destination_hash in Transport.announce_table:
    announce_entry = Transport.announce_table[destination_hash]
    if announce_entry[IDX_AT_RETRIES] > 0 and
       announce_entry[IDX_AT_RETRIES] >= Transport.LOCAL_REBROADCASTS_MAX:
        # "local rebroadcast limit reached"
        completed_announces.append(destination_hash)
    elif announce_entry[IDX_AT_RETRIES] > Transport.PATHFINDER_R:
        # "retry limit reached"
        completed_announces.append(destination_hash)
    else:
        if time.time() > announce_entry[IDX_AT_RTRNS_TMO]:
            announce_entry[IDX_AT_RTRNS_TMO] =
                time.time() + Transport.PATHFINDER_G + Transport.PATHFINDER_RW
            announce_entry[IDX_AT_RETRIES] += 1
            # ... build rebroadcast packet and send
```

With the constants:

| Constant | Value | Citation |
|---|---:|---|
| `PATHFINDER_R` | 1 | `Transport.py:68` |
| `PATHFINDER_G` | 5 s | `Transport.py:69` |
| `PATHFINDER_RW` | 0.5 s | `Transport.py:70` |
| `LOCAL_REBROADCASTS_MAX` | 2 | `Transport.py:77` |

### Deterministic walk — non-local-client source

Entry inserted with `retries = 0, retransmit_at = now + rand*0.5s`.

| Tick | retries in | Guard A | Guard B | Action | retries out |
|---|---:|---|---|---|---:|
| 1 | 0 | `0 > 0 && 0 >= 2` = false | `0 > 1` = false | fire, schedule next | 1 |
| 2 | 1 | `1 > 0 && 1 >= 2` = false | `1 > 1` = false | fire, schedule next | 2 |
| 3 | 2 | `2 > 0 && 2 >= 2` = **true** | — | remove | — |

**Count = 2 rebroadcasts per received non-local-client announce.**

### Deterministic walk — local-client source

Entry inserted with `retries = 1, retransmit_at = now`.

| Tick | retries in | Guard A | Guard B | Action | retries out |
|---|---:|---|---|---|---:|
| 1 | 1 | `1 > 0 && 1 >= 2` = false | `1 > 1` = false | fire, schedule next | 2 |
| 2 | 2 | `2 > 0 && 2 >= 2` = **true** | — | remove | — |

**Count = 1 rebroadcast per received local-client-sourced announce.**

### Immediate local-client forward

Lines 1788-1833: after the table insertion, Python also emits the
announce immediately to every local-client interface that is
**not** the receiving interface:

```python
for local_interface in Transport.local_client_interfaces:
    if packet.receiving_interface != local_interface:
        new_announce = RNS.Packet(...)
        new_announce.send()
```

This is the only place in the announce path where `receiving_interface`
filtering happens. It only applies to local-client interfaces — the
fanout onto LoRa, TCP, UDP interfaces is unfiltered. This confirms
that for the mixed LoRa-Serial + LoRa-RF topology our tests care
about, Python does **not** skip the received interface when
rebroadcasting.

### Fan-out per rebroadcast fire

Each fire builds a new announce packet (lines 540-561), calls
`send()` → `Transport.outbound()`, which applies the mode
filtering and bandwidth-cap logic. The receiving interface is
implicitly included in the `for interface in Transport.interfaces`
loop (no exclusion check). Echoes are absorbed by the
packet_hashlist check at line 1376 when they arrive back.

### Block-rebroadcasts path

`announce_entry[IDX_AT_BLCK_RBRD]` set to `True` (indices at
line 557 of the retry loop) reroutes the rebroadcast as a
`PATH_RESPONSE` packet (`announce_context = PATH_RESPONSE`,
line 537). This is how path-responses ride the same scheduler.

## 4. Path-request

### Trigger

`Transport.request_path(destination_hash, ...)` at
`Transport.py:2771` is the main producer. Clients call into it
via `Destination.request_path()` or explicit transport calls.

### On-wire behaviour

At line 2561-2587: builds a `Packet` with
`packet_type = Packet.DATA` and
`transport_type = Transport.BROADCAST`, then calls
`packet.send()` once. Same one-shot pattern as self-announce.

Fan-out goes through the same `Transport.outbound()` broadcast
loop at `Transport.py:1180-1317`.

**Count = 1 on-wire broadcast per path-request call. No
retries in the scheduler for path-requests.**

### Rate-limiting

Path-requests are subject to `PATH_REQUEST_MI = 20` seconds
minimum interval per destination (`Transport.py:83`) — clients
requesting the same path more often are throttled upstream of
`Transport.outbound()`.

## 5. Path-response

### Trigger

Two paths produce a `PATH_RESPONSE`:

1. **Active answer**: Transport receives a path-request, has the
   path, calls `Destination.announce(path_response=True, tag=...)`
   with the matching identity. This produces a
   `Packet.ANNOUNCE` with `context = PATH_RESPONSE`
   (`Destination.py:309-310, 319-322`) and sends it once.
2. **Rebroadcast with block_rebroadcasts**: the retry loop at
   `Transport.py:576-604` emits path-responses when
   `announce_entry[IDX_AT_BLCK_RBRD]` is set. Same 2-fire count
   as a regular received-announce rebroadcast.

### On-wire semantics

Path-responses are a packet-type subset of announces. The
fan-out logic is the same as announces. Consumers distinguish
by `packet.context == PATH_RESPONSE`.

### Special routing

In `Transport.outbound()` at lines 1167+ (targeted-transport
branch), a packet with `transport_id` set AND a known next-hop
in `path_table` is routed to a single specific interface via
`SendPacket`, not broadcast. This is what happens when a
path-response is specifically addressed to the path-requester
rather than broadcast. In our Rust code the answering site stamps
the requesting interface onto the announce-table entry it inserts —
`target_interface` (`transport.rs:9345`) — and the retry scheduler
hands an entry carrying one to that interface alone instead of
broadcasting it: `target_iface` (`transport.rs:9998-10007`).

(Re-read 2026-09-23. The citation this paragraph carried was
written backwards, 4336 down to 4286, and pointed at neither site:
4286 sits inside `announce_table_entries` (`transport.rs:4164`), an
RPC export. The only non-test `target_interface: Some(...)` in the
file is the one cited above.)

### Held announces during path-response scheduling

Inserting the path-response entry into `announce_table` would
overwrite an announce for the same destination still waiting in
its rebroadcast grace. Python holds any such entry in
`Transport.held_announces` before the insertion
(`Transport.py:2991-2999`) and reinserts it when the response
entry fires in the retry loop (`Transport.py:630-633`), so the
targeted response goes out first and the network-wide
rebroadcast afterwards. The Rust counterpart is
`hold_displaced_announce` plus the reinsertion sites in
`check_announce_rebroadcasts` (Codeberg #170), with one
declared deviation: a held network-wide rebroadcast is never
displaced by a later path-response entry, where Python
overwrites the held slot on every request and can lose the
rebroadcast to back-to-back requests.

## 6. Link-request

### Trigger

`Link.__init__(destination=...)` on the initiator. Internally
calls `Packet(destination, link_data, Packet.LINKREQUEST, ...)`
and sends it.

### On-wire behaviour

`Packet.LINKREQUEST` (`Packet.py:62`) is **unicast**, not
broadcast. At `Transport.py:2091`: local-destination link
requests are dispatched to the destination's attached interface
directly. Non-local paths route through next-hop. There is no
broadcast fanout.

**Count = 1 unicast packet per link initiation. Not relevant to
broadcast parity directly, but enumerated here for completeness.**

## 7. Dedup (packet_hashlist)

| Item | Value | Citation |
|---|---|---|
| Storage | `set()` | `Transport.py:106` |
| Previous-window storage | `set()` | `Transport.py:107` |
| Max size | 1 000 000 entries | `Transport.py:175` |
| Check site | line 1376 | `Transport.py` |
| Rotation | half-cleared when reaches `hashlist_maxsize/2` | approximate, see cull job |

The dedup check is the **only** mechanism that prevents the
self-heard echo when we (Rust) stop excluding the receiving
interface from the rebroadcast fanout. Verifying the check fires
reliably is a hard requirement for B1.

## 8. ANNOUNCE_CAP — per-interface rate limiter

### Constants

| Constant | Value | Citation |
|---|---:|---|
| `Reticulum.ANNOUNCE_CAP` | 2 (percent of bandwidth) | `Reticulum.py:114` |

Interface instances set
`interface.announce_cap = Reticulum.ANNOUNCE_CAP/100.0 = 0.02`
at `Reticulum.py:819`. Each interface also has
`interface.bitrate` (bps).

### Logic

The rate limiter is consulted **only** for forwarded announces
(`packet.hops > 0`). Self-originated announces bypass it because
they only fire once and are not worth deferring.

At `Transport.py:1091-1161`:

```python
if (packet.hops > 0):
    if not hasattr(interface, "announce_cap"): ...
    if not hasattr(interface, "announce_allowed_at"):
        interface.announce_allowed_at = 0

    if time.time() >= interface.announce_allowed_at and interface.bitrate:
        tx_time    = len(packet.raw) * 8 / interface.bitrate
        wait_time  = tx_time / interface.announce_cap
        interface.announce_allowed_at = time.time() + wait_time
        # proceed with immediate TX
    else:
        # queue for later
        if not len(interface.announce_queue) >= Reticulum.MAX_QUEUED_ANNOUNCES:
            interface.announce_queue.append(packet)
```

`wait_time = tx_time / 0.02 = 50 × tx_time`: each forwarded
announce "books" 50× its own airtime on the interface before the
next forwarded announce is allowed immediate TX.

### Queue drain

When `announce_allowed_at` rolls past and there are queued
announces, the interface's `process_announce_queue()` pops the
next one and emits it. This is a per-interface deferred-send
mechanism, not a transport-wide one.

## 9. LOCAL_REBROADCASTS_MAX

Covered in section 3 (retry loop). The enforcement sites are:

- `Transport.py:582`: retry-loop guard A. Prevents emission when
  `retries >= LOCAL_REBROADCASTS_MAX`.
- `Transport.py:1728`: secondary site that removes an entry from
  `announce_table` when a duplicate announce arrives and the
  local rebroadcast counter has saturated. This is the "I'm
  hearing too many copies of this announce from others, stop
  my own rebroadcast too" path.

## 10. Management announce keepalive

### Constants

| Constant | Value | Citation |
|---|---:|---|
| `mgmt_announce_interval` | 7 200 s (2 h) | `Transport.py:194` |
| Initial-fire trick | `last_mgmt_announce = now - interval + 15` | `Transport.py:283` |

### Behaviour

`Transport.py:283` runs at startup and sets `last_mgmt_announce`
to 15 seconds ago minus the full interval, so the next check at
`Transport.py:963` fires ~15 s after startup. Each fire walks
`Transport.mgmt_destinations` (a list of transport-control
destinations like probe responders and blackhole destinations,
populated at lines 220-241, 367 during `Transport.start()`) and
announces each.

After each successful batch the code updates
`Transport.last_mgmt_announce = time.time()`.

### Purpose

Without this keepalive, a node that loses its initial one-shot
`Destination.announce()` is unreachable until the next manual
announce. The 2-h re-announce gives the mesh a periodic refresh
without flooding the network with announce traffic.

## 11. Interface modes

Python-Reticulum distinguishes five interface modes
(`Interfaces/Interface.py:45-50`):

| Mode | Constant | Intent |
|---|---|---|
| `MODE_FULL` | 0x01 | Default. Fully participating transport node. |
| `MODE_POINT_TO_POINT` | 0x02 | Directed link, no announce flooding. |
| `MODE_ACCESS_POINT` | 0x03 | Gateway to clients. Special path expiry. |
| `MODE_ROAMING` | 0x04 | Mobile node. Selective rebroadcast. |
| `MODE_BOUNDARY` | 0x05 | Edge between mesh segments. Selective rebroadcast. |
| `MODE_GATEWAY` | 0x06 | Inter-mesh gateway. |

These are consulted in `Transport.outbound()` at lines 1040-1084
to suppress rebroadcast on specific interfaces.
`block_rebroadcasts` at the announce-table entry level is a
related per-entry flag.

**Leviculum does not implement interface modes.** All interfaces
behave as `MODE_FULL`. This is a documented divergence that Phase
A audit records; if a future scenario surfaces that requires
mode behaviour, a separate task lands them. Until then, our
fanout is "unfiltered over the broadcast-capable interface set",
which is behaviourally equivalent to Python with all interfaces
in `MODE_FULL`.

## 12. Rust ↔ Python parity matrix

Legend: ✓ matches, ≈ matches in count/semantics with timing or
structural divergence, ⚠ gap not yet addressed, ✗ does not match.

| Mechanism | Python reference | Rust today | Status | Notes |
|---|---|---|---|---|
| Self-announce one-shot | `Destination.py:322`, `Packet.py:294` | one emission per call: `send_on_all_interfaces` (`transport.rs:3257`) | ✓ | History: the 3 extra retries this row recorded were removed by B3, 08128e97 (2026-04-15) |
| Self-announce on-wire count | 1 | 1 for an ordinary destination; 2 for a management destination | ≈ | B3 brought it to 1; 2d9234da (2026-09-01) gave management destinations the reference's local-client second emission — declared deviation, see section 13 |
| Self-announce fanout | all interfaces (MODE_FULL assumed) | no exclusion argument | ✓ | `send_on_all_interfaces` (`transport.rs:3257`) |
| Received-announce rebroadcast count | 2 (non-local-client), 1 (local-client) | 2 and 1: the insert picks the start value from the source, `retries` (`transport.rs:5498-5502`) | ✓ | History: B2, 79ac5204 (2026-04-15), dropped `PATHFINDER_RETRIES` to 1 and reordered the init |
| Received-announce fanout | all interfaces; echo dedup'd on RX | `send_on_all_interfaces` (no exclude) | ✓ | Matches Python. B1 verified by `test_announces_forwarded_through_transport`. |
| Packet-hash dedup on RX | `Transport.py:1227` | `has_packet_hash` (`transport.rs:3002`) | ✓ | Identical semantics, rolling window |
| `PATHFINDER_G` grace | 5 s | 5 000 ms | ✓ | `PATHFINDER_G_MS` (`constants.rs:126`) |
| `PATHFINDER_RW` jitter | 0.5 s | 500 ms (+ optional airtime factor) | ≈ | Option α permitted timing divergence |
| `LOCAL_REBROADCASTS_MAX` | 2 | 2 | ✓ | `LOCAL_REBROADCASTS_MAX` (`constants.rs:142`); enforced in the retry loop, `local_rebroadcasts` (`transport.rs:9829`), and on a duplicate arrival, `local_rebroadcasts` (`transport.rs:5212`) |
| `ANNOUNCE_CAP` | 2 % | 2 % | ✓ | `DEFAULT_ANNOUNCE_CAP_PERCENT` (`constants.rs:322`); state in `InterfaceAnnounceCap` (`transport.rs:601-608`), holdoff at `allowed_at_ms` (`transport.rs:10182-10194`) |
| `announce_queue` / deferred-send | `interface.announce_queue` | `InterfaceAnnounceCap.queue` | ✓ | Same intent, Rust-side uses Vec |
| `mgmt_announce_interval` | 7 200 s | 7 200 000 ms | ✓ | `MGMT_ANNOUNCE_INTERVAL_MS` (`constants.rs:159`); `check_mgmt_announces` (`node/mod.rs:2360-2452`) |
| mgmt-announce initial 15 s trick | `Transport.py:283` | `schedule_initial_mgmt_announce` (`node/mod.rs:2346-2352`) with `MGMT_ANNOUNCE_INITIAL_DELAY_MS` (`node/mod.rs:203`) | ≈ | Verified by B4 audit; Rust adds a per-node draw on top, `MGMT_ANNOUNCE_INITIAL_JITTER_MS` (`node/mod.rs:222`) |
| mgmt-announce iterates all dests | Python walks `mgmt_destinations` | `check_mgmt_announces` walks `mgmt_destinations` | ✓ | Verified by B4 audit |
| Path-request one-shot broadcast | `Transport.py:2771-2809` | `transport.rs` (to verify in B7) | ≈ | B7 audit |
| Path-response targeted | targeted-transport branch, section 5 | `target_iface` (`transport.rs:9998-10007`) | ✓ | Preserved |
| Interface modes (FULL/ROAMING/…) | 5 modes | none (all = FULL) | ⚠ | Documented gap; separate task |
| `block_rebroadcasts` | per-entry flag | `AnnounceEntry.block_rebroadcasts` | ✓ | Verified by B7 audit |

## 13. Phase A resolutions of semantic ambiguities

### B2 retry-count alignment

**Question**: `PATHFINDER_R = 1` — does this mean 1 retry after
the initial or 1 TX total?

**Resolution** (walking the Python loop, section 3): Python fires
**2 times** per received non-local-client announce, bounded by
`LOCAL_REBROADCASTS_MAX = 2` not by `PATHFINDER_R`. The
`PATHFINDER_R` guard (`retries > PATHFINDER_R`) would fire at
`retries = 2` but `LOCAL_REBROADCASTS_MAX` fires first at
`retries >= 2`. In other words, for the default constants the
`PATHFINDER_R` guard is redundant with `LOCAL_REBROADCASTS_MAX`
in the non-local-client path.

**Rust equivalent target**: 2 fires per received non-local-client
announce. Achievable in two ways:

- A. Set `PATHFINDER_RETRIES = 1` **and** change the entry-insert
  so a received non-local-client announce starts at `retries = 0`
  rather than `retries = 1`. The retry-loop guards already read
  `retries > PATHFINDER_RETRIES` and `local_rebroadcasts >=
  LOCAL_REBROADCASTS_MAX`; both fire at the right count.
- B. Set `PATHFINDER_RETRIES = 2` and leave the insert at
  `retries = 1`. Same on-wire count.

**B2 committed path A** — it more closely mirrors Python's
constants and counter semantics, so future upstream-audit
readers see 1:1 constants.

**Path A is history, not an outstanding step** (re-read
2026-09-23). B2 landed as 79ac5204 (2026-04-15); `retries: 1` no
longer occurs anywhere in `transport.rs`, so the edit this step
describes cannot be made against the tree and the line it used to
cite now holds unrelated code. Where the mechanism lives today:

- the insert picks the start value from the source of the
  announce — `PATHFINDER_RETRIES` for a local client,
  `0` otherwise — at `retries` (`transport.rs:5498-5502`);
- the two guards are `PATHFINDER_RETRIES` (`transport.rs:9828`)
  and `local_rebroadcasts` (`transport.rs:9829`), in
  `check_announce_rebroadcasts`;
- `PATHFINDER_RETRIES` (`constants.rs:115`) is 1.

### B1 fanout alignment

**Question**: if we remove `exclude_iface`, can dedup reliably
catch the self-echo, and does it play well with Python peers?

**Resolution**: yes. Outgoing broadcasts go through
`send_on_all_interfaces` (`transport.rs:3257`), which calls
`self.storage.add_packet_hash()` before emitting the
`Action::Broadcast`. The check that reads that set on arrival is
`has_packet_hash` (`transport.rs:3002`), in `process_incoming`. The only edge case is the
dedup window rollover at `HASHLIST_MAXSIZE = 1 000 000` entries —
a packet that is ~1M packets old could theoretically come back.
Not a concern in practice for single-day bench runs.

**Python interop subtlety** (discovered 2026-04-15 when the B1
change was first landed, caused a 3-node TCP relay test to fail,
then resolved by spacing out the test's announce emissions): the
Python reference has a per-interface **ingress control** at
`reference/Reticulum/RNS/Interfaces/Interface.py:117-138`. When two
announces arrive on the same interface faster than
`IC_BURST_FREQ_NEW = 3.5/s` (≈ 285 ms apart), Python activates
burst mode for at least `IC_BURST_HOLD = 60 s` then penalises for
`IC_BURST_PENALTY = 300 s`. Held announces are released by
`process_held_announces` every `interface_jobs_interval = 5 s`,
but only once the cooldown expires.

In a LoRa topology the multi-second airtime per transmit naturally
spaces announces below this threshold, so ingress control never
activates. In a TCP relay topology a Rust node that receives
announces from both peers in rapid succession — and with B1
fans them both out on every interface, with only the retry
scheduler's 0-500 ms jitter spacing them — can trip Python's
ingress control on the receiving side.

This is not a Rust bug; it is Python's intended rate-limit
behaviour that naive TCP-only tests can expose. The regression
guard test `test_announces_forwarded_through_transport` spaces
its two `announce_destination` calls by two seconds to keep
the spawned-peer interface's `ia_freq` below 3.5 /s. Production
scenarios where two daemons announce in tight succession through
a Rust relay remain subject to Python's ingress limits — exactly
as they would be through a Python relay.

### Management announces get a second emission (2026-09-01)

**Decision**: a management destination's announce is emitted once
by `send_on_all_interfaces` (`transport.rs:3257`) and then once more
by `schedule_own_announce_retry` (`transport.rs:3328`), which inserts
an announce-table entry at `retries = PATHFINDER_RETRIES` so the
scheduler fires it exactly once and retires it. Landed as 2d9234da.

**Why it is not a parity break**: the reference gives the same two
emissions to any announce reaching `rnsd` from a shared-instance
client — inserted at `retries = PATHFINDER_R` and fired once more by
the job loop. Only an announce originated *inside* the transport
process misses out, and our management destinations live inside the
daemon. The deviation rule holds on all three clauses: the wire bytes
are the same announce re-emitted, a peer's packet-hash dedup already
absorbs the duplicate, and the P1 gain was measured on the residual
`ble_lora_transport` reds of 2026-09-01. The retry is cancelled as
soon as a neighbour is heard passing the announce on, so a healthy
mesh pays nothing. Full argument and citations in the doc comment on
`schedule_own_announce_retry` (`transport.rs:3328`).

**Scope**: management destinations only. An ordinary
`Destination.announce()` is still one-shot, matching Python exactly.

### Mode-less Rust

**Decision**: Leviculum continues without interface modes.
Documented as a deliberate scope reduction. Our scenarios and the
Python peer we interop against all use `MODE_FULL` implicitly.
A future Bug that requires `MODE_ROAMING` or similar gets its
own task; this parity doc predates and outscopes that work.

## 14. Usage

This document is the audit target for both sides:

- When we upgrade the vendored `RNS/` tree to a new upstream
  release, the Python line numbers here are the first thing to
  re-verify. A changed line number is a hint the behaviour may
  have shifted; a changed mechanism is a new parity task.
- When we add a new broadcast code path to
  `leviculum-core`, we extend the parity matrix (section 12) and
  add a test under `leviculum-std/tests/rnsd_interop/` that
  verifies the new path matches what a live Python peer sees.

The parity matrix is the contract. Everything else in this
document is the reading behind the entries.
