# Announce dedup and path replacement (Python-RNS reference facts)

The reference facts behind the #376 desk measurement (a board's announce
reaches Columba only through the other board, never on the direct BLE
link). Four questions, answered strictly from the vendored
`reference/Reticulum` tree (Python-RNS 1.3.5), with citations, plus a
fifth section (added for Codeberg #231) on what 1.5.2 — the version a
periculum `rnsd` arm pins — does differently in the same arm. This page
states what the reference does; it decides nothing.

Sibling pages: [Hop counting](../architecture-hop-counting.md),
[Broadcast Python-RNS parity](../architecture-broadcast-python-parity.md).

## 1. Direct vs. forwarded copy: same packet hash

**Question.** An announce received directly (header type 1, wire hops 0)
versus the same announce forwarded by a transport node (header type 2,
wire hops 1, transport id inserted): same packet hash?

**Answer: yes, the hash is identical.** The hashable part is built by
`get_hashable_part` (`reference/Reticulum/RNS/Packet.py:355`):

```python
def get_hashable_part(self):
    hashable_part = bytes([self.raw[0] & 0b00001111])
    if self.header_type == Packet.HEADER_2:
        hashable_part += self.raw[(RNS.Identity.TRUNCATED_HASHLENGTH//8)+2:]
    else:
        hashable_part += self.raw[2:]
    return hashable_part
```

Three exclusions make the two copies hash the same:

* **The hop count is excluded entirely.** It is header byte 1 — `hops`
  (`reference/Reticulum/RNS/Packet.py:245`) — and both branches above
  start at byte 2 or later.
* **The transport id is excluded.** For header type 2 the slice starts
  after the 16-byte transport id (`TRUNCATED_HASHLENGTH//8 + 2` = 18).
* **The header-type and transport-type bits are masked off.** Byte 0 is
  packed as `packed_flags` (`reference/Reticulum/RNS/Packet.py:171`) —
  `header_type << 6 | context_flag << 5 | transport_type << 4 |
  destination.type << 2 | packet_type` — and the `& 0b00001111` mask
  keeps only destination type and packet type. Header type (bit 6),
  transport type (bit 4) and the on-air IFAC flag (bit 7) all vanish.

So a relay changing hops, inserting its transport id and flipping the
header to type 2 does not change the packet hash: the two copies are the
same announce to every dedup structure keyed on `packet_hash`.

## 2. The second copy in `Transport.inbound`: order, and the announce exemption

**Order.** The dedup check runs **first**, the path-table update later,
and both copies pass the dedup:

1. `packet.hops` (`reference/Reticulum/RNS/Transport.py:1457`) is
   incremented for every inbound packet, right after unpack.
2. The filter runs — `packet_filter`
   (`reference/Reticulum/RNS/Transport.py:1486`) gates all further
   processing.
3. On acceptance the hash is remembered at once — `add_packet_hash`
   (`reference/Reticulum/RNS/Transport.py:1506`) — before any announce
   processing (deferred only for link-table traffic and LR proofs).
4. Announce processing, including the path-table update, comes much
   later in the same call — `validate_announce`
   (`reference/Reticulum/RNS/Transport.py:1691`).

**The exemption.** Inside `packet_filter`
(`reference/Reticulum/RNS/Transport.py:1336`), a packet whose hash is
already in `packet_hashlist`
(`reference/Reticulum/RNS/Transport.py:1376`) is dropped — **except an
announce for a SINGLE destination, which is accepted anyway**:

```python
if not packet.packet_hash in Transport.packet_hashlist and ...: return True
else:
    if packet.packet_type == RNS.Packet.ANNOUNCE:
        if packet.destination_type == RNS.Destination.SINGLE:
            return True
```

So the second copy of the same announce is **not** discarded by the
hashlist. It runs the full announce path again, and what it may change
is decided there, by the random-blob replay check and the hops
comparison of §3 — not by dedup. For #376 this matters in both
directions: hashlist dedup cannot explain a missing direct announce, and
hearing the relayed copy first does not inoculate the node against the
direct copy.

## 3. The path replacement rule

All of this sits under the hop cap and non-local condition —
`PATHFINDER_M` (`reference/Reticulum/RNS/Transport.py:1750`) — with
`announce_emitted` (`reference/Reticulum/RNS/Transport.py:1753`) the
emission timestamp read out of the announce's random blob (bytes 5..10;
`announce_emitted`, `reference/Reticulum/RNS/Transport.py:3191`), and
the table side aggregated as the maximum over the recorded blobs
(`timebase_from_random_blobs`,
`reference/Reticulum/RNS/Transport.py:3182`).

**Unknown destination:** added unconditionally — `should_add`
(`reference/Reticulum/RNS/Transport.py:1831`).

**Fewer or equal hops** than the table entry — `path_table`
(`reference/Reticulum/RNS/Transport.py:1765`):

```python
if packet.hops <= Transport.path_table[packet.destination_hash][IDX_PT_HOPS]:
    path_timebase = Transport.timebase_from_random_blobs(random_blobs)
    if not random_blob in random_blobs and announce_emitted > path_timebase:
        should_add = True
```

(`path_timebase`, `reference/Reticulum/RNS/Transport.py:1772`.) Two
conditions, **both** required:

* the random blob must be new — the *same* announce heard again, e.g.
  the direct copy after the relayed copy, has the same blob and does
  **not** replace the path, however many hops it saves;
* the emission timestamp must be **strictly newer** than the newest one
  recorded for the destination. A later announce whose clock is behind
  the recorded one loses even at fewer hops — which is why the #376
  announce instrument keeps the telemetry path's clock gate.

**More hops** than the table entry: ignored, unless one of three
escapes fires, in order —

* the path has expired: `path_expires`
  (`reference/Reticulum/RNS/Transport.py:1793`), still requiring an
  unseen blob;
* the emission is strictly newer: `path_announce_emitted`
  (`reference/Reticulum/RNS/Transport.py:1809`), still requiring an
  unseen blob;
* same emission, but the recorded path has been marked unresponsive:
  `path_is_unresponsive`
  (`reference/Reticulum/RNS/Transport.py:1822`).

## 4. No hops-0 drop rule

**Question.** Does Python drop or downgrade announces received with
hops 0 on any interface class — anything that could make Columba ignore
a direct board announce?

**Answer: no such rule; searched `Transport.inbound`.** There is no
condition anywhere in `Transport.inbound` that keys on `hops == 0` for
an announce or treats a directly received announce worse than a relayed
one. (`packet.hops`, `reference/Reticulum/RNS/Transport.py:1457`,
increments every inbound packet before any decision, so a direct
announce is processed at `hops == 1`; the only decrements are the two
shared-instance IPC cases — `is_local_client_interface`,
`reference/Reticulum/RNS/Transport.py:1482` — which are not radio
interfaces.)

The gates that *do* exist on the way to the path table, for any hops
value, are:

* **Signature validation** — `validate_announce`
  (`reference/Reticulum/RNS/Identity.py:532`): fails, and the announce
  is silently dropped.
* **Ingress limiting** — `should_ingress_limit`
  (`reference/Reticulum/RNS/Transport.py:1705`): on an interface with
  `ingress_control` enabled (`should_ingress_limit`,
  `reference/Reticulum/RNS/Interfaces/Interface.py:145`), an announce
  for an *unknown* destination can be held — `hold_announce`
  (`reference/Reticulum/RNS/Transport.py:1706`) — rather than
  processed; a pending path request bypasses the hold.
* **PLAIN/GROUP announces** are always invalid
  (`packet_filter`, `reference/Reticulum/RNS/Transport.py:1336`);
  `lxmf.delivery` announces are SINGLE and unaffected.
* **The §3 replacement conditions**, in particular the strict
  emission-timestamp comparison.

Sections 1 to 4 recorded 2026-09-09 against `reference/Reticulum` as
vendored (1.3.5).

## 5. 1.5.2's same-emission arm: gravity, not hops

§3 answers for 1.3.5, the vendored tree. It is not the whole answer for
the Python a comparison run actually faces: periculum's `rnsd` arm pins
1.5.2 (`rns_pin` in every `pathchoice_*_rnsd.json`), and 1.5.2 has an
acceptance path on EQUAL emission that 1.3.5 does not. Codeberg #231's
readings 308 and 309 disagreed because each had read one of the two
versions, so both are recorded here.

**The line references in this section are deliberately NOT written in
the `path:line` citation form.** 1.5.2 is not vendored — the tree read
was `~/coding/Reticulum` at `_version.py` 1.5.2 — so the citation guard
would resolve a `Transport.py` line citation written here against the
1.3.5 copy and pass it on existence alone, a green that means nothing
(docs/src/concepts/checks-and-citations.md §"could not be checked").
Prose line numbers are checkable by a reader and by nothing else, which
is the truth about them.

The outer gate is unchanged — `packet.hops <=` the known hop count — and
the first test inside it is the 1.3.5 one, verbatim: 1.5.2
`Transport.py` lines 2237-2238, against
`reference/Reticulum/RNS/Transport.py:1771-1772` here. What 1.5.2 adds
is the `else`, at 1.5.2 `Transport.py` lines 2245-2252:

```python
# If the same announce is received later on an interface
# with higher gravity, allow updating the path table to
# use this interface instead.
if   announce_emitted != path_timebase: should_add = False
elif announce_gravity == None or current_gravity == None: should_add = False
else:
    if announce_gravity <= current_gravity: should_add = False
    else:
        ... should_add = True
```

Three facts follow:

* **Hop count is not in the acceptance test, in either version.** It
  appears only in the `packet.hops <=` gate above. A second copy of an
  emission already installed does not replace the path for having taken
  fewer hops in 1.3.5 or in 1.5.2.
* **What 1.5.2 does accept on equal emission is a higher-`gravity`
  interface.** `gravity` is an operator-configured per-interface
  preference — 1.5.2 `Reticulum.py` lines 798-799 read it out of the
  interface stanza, and `_add_interface` (lines 1134-1136) fills the
  `default_gravity` when the stanza names none. It is a configured
  ranking, not a measurement.
* **On a default config 1.5.2 behaves exactly like 1.3.5 here.**
  1.5.2 `Interfaces/Interface.py` line 75 sets `DEFAULT_GRAVITY = 0`, so
  `announce_gravity <= current_gravity` holds and the arm rejects. An
  `rnsd` arm that sets no `gravity` anywhere is measuring the strict
  1.3.5 rule — and that is every `pathchoice_*` cell: no `gravity` key
  appears anywhere under periculum's `emulated/`, `conformance/` or
  `periculum/adapters/`, and each node in those cells has exactly one
  interface to begin with.

Recorded 2026-09-27 against `~/coding/Reticulum` at 1.5.2. The nearest
thing we have to this arm is our own same-emission clause
(`transport.rs`, `SameEmissionRule::FewerHopsWins`, Codeberg #231),
which ranks by hop count rather than by a configured preference. It is
live, as a deviation, on two measurements:

* On the three relayed lnsd arms (`pathchoice_loss30/50/70`, 41 to 43
  `PATH_ADD` each, 2026-09-27) `reason="same_emission_fewer_hops"`
  fired 0 times, because on one shared radio the direct copy, when it
  arrives at all, arrives first. There the clause costs nothing.
* With the clause retired for a day (`SAME_EMISSION_RULE =
  StrictlyNewerOnly`, the 1.3.5 rule above, 2026-10-13), the land gate
  on 8f33581e went red on `conformance/link_failure_recovery`:
  `VERDICT link_failure_recovery RED step 9 (wait_for_path) failed:
  path to ba8c7f6d... did not settle at 1 hop(s) before timeout, last
  seen: 2 hop(s)`. When a blocked direct link comes back, the direct
  copy is the emission the node already holds via the relay, a replay
  under Python's rule whatever its hop count, so the node relays
  through a third node until the destination announces again. That is
  a third node's airtime for every packet for up to an announce
  interval, and the clause is what heals it.

Wire format and semantics are unchanged (a copy is accepted or not,
nothing new is sent), so the deviation rule admits it. The enum keeps
`StrictlyNewerOnly` and a `FewerHopsOnOtherInterface` candidate
compiled and tested on the pure function.

## 6. The measured same-emission arm: `path_choice = hops_and_loss`

Since 2026-10-05 (Codeberg #230) the same-emission clause has a second
rule beside the hop ranking (§5): `path_choice = hops_and_loss` (the
`[reticulum]` key; `TransportConfig::path_choice`, `PathChoice` in
`transport.rs`). Since order 1380 it is the default on radio interfaces:
the media-aware driver declares it per interface
(`Transport::set_interface_path_choice`, from `InterfaceInfo::path_choice`
in leviculum-std), RNode and BLE declare `hops_and_loss`, every other
medium `hops`, and the `[reticulum]` key, when set, overrides every
interface. A same-emission race is decided by the cost rule when either
the installed path's interface or the copy's prices loss, and the penalty
prices only a direct candidate on such an interface. Where §5's 1.5.2 `gravity` re-ranks
equal-emission copies by an operator-CONFIGURED preference, this rule
re-ranks them by a MEASURED one: the direct link is priced at its
bidirectional ETX, estimated for free from the announce re-hear ratio —
of the emissions this transport provably learned about, the fraction
that also arrived as a direct copy (`DirectLinkEvidence`,
`lower_route_cost`). A clean two-hop route then beats a direct link
losing more than ~29 % of announces. Three properties bound it:

* **Scope.** Only the same-emission comparison changes. A newer
  emission still installs unconditionally in both hop arms (§3's rules,
  which is also the liveness guarantee when the relay dies), and the
  random-blob replay gate lets a same-blob copy through when the cost
  arm will accept it, or when it took fewer hops (it is direct-link
  evidence even when it loses). Path responses and local-client copies
  feed no evidence: a response is served once over one route, so the
  absence of a direct copy of it proves nothing.
* **Degradation.** Below `DIRECT_EVIDENCE_MIN_KNOWN` known emissions —
  a fresh destination, or a mesh whose announce cadence is too sparse
  to score — the rule decides exactly like `hops`. The default is
  `hops` on every non-radio interface; rnsd ignores the key in a shared
  config.
* **Evidence.** The pinned replay `mvr_pathchoice_rule_table`
  (leviculum-core, node/) drives both knob settings through the real
  acceptance gate over the `pathchoice_loss*` room shape at five loss
  levels and eight seeds: identical at 0 % loss, more delivered and
  fewer flaps at every measured loss ≥ 30 %, and the Python reference
  row (§3/§5 semantics as a pure fold) matches the shipped rule
  exactly, which pins the replay to the gate. The companion
  `mvr_pathchoice_direct_vs_route` holds the norelay control: with no
  competing route the penalty can never unroute the only path.

## 7. A local client's own announce in the same second

Since order 1488 the equal-or-fewer-hops arm (§3) takes one more copy:
an announce from a local shared-instance client, its random blob unseen,
whose emission second equals the newest one recorded
(`reason="local_client_same_second"` on `PATH_ADD`, in `handle_announce`
in `leviculum-core/src/transport.rs`). §3's rule refuses it, and since a
refused announce is not rebroadcast, no peer ever hears it.

The case is ordinary on a shared instance. An emission has one-second
resolution, so a client that announces twice inside one second, or two
short-lived clients of one identity back to back (two `loquax send` runs,
the second with `--from`), reach the instance in the same second. The
first client's hops-0 path stands until the instance has handled its IPC
disconnect, and the second announce, with the new display name or a new
ratchet, met the strict `>` and was dropped. The land gate on 058c8878
showed it: Python heard the first name and never the second in 40 s.
The test is `a_clients_second_announce_in_one_second_reaches_the_network`
in `transport.rs`, beside its replay partner
`a_clients_repeated_announce_is_not_forwarded_twice`.

Only the holder of the destination's key signs a fresh blob, and a
local client is on our side of the IPC hop, so the copy is news and not
an echo. A seen blob still dies at the replay gate, and an announce from
a network interface is untouched. Deviation rule: the wire is unchanged,
a Python peer stores whatever valid announce it is sent
(`Identity.validate_announce` runs before its path rule), and the change
delivers the client's newest app data, which §3's rule would lose.

## 8. An echo cancels only the announce it echoes

The reference stops retransmitting an announce when it hears a
neighbour pass it on (`Transport.py:1724`): a copy one hop further
counts towards `LOCAL_REBROADCASTS_MAX`, and a copy two hops further,
while `now < retransmit_timeout` (`:1736`), ends the entry at once.
Both arms key on the destination hash alone.

Since order 1617 `handle_announce` counts a copy for either arm only
when its packet hash equals the one of the announce the entry holds,
and the two-hop arm wants the retransmit still in the future, as the
reference does; ours had asked only that one was pending. The case it
closes: a local client announces A1, the instance sends it, the client
disconnects (its path goes), and the next client of the same identity
announces A2 with a new display name. A2's entry is due at once. In
the same millisecond, before the scheduler ran, the Python peer's
relay of A1 came back two hops on, matched the destination, and
cancelled A2; the peer kept the old name
(`lnmsg_send_reaches_a_python_lxmf_receiver`, 1611's dump). A copy of
another emission says nothing about whether this one was passed on.

The mvr is `leviculum-std/tests/mvr/an_older_emissions_echo_keeps_the_renamed_announce.rs`:
the field case, the packet half alone on a relayed retransmit, the
timing half alone, and the control that a copy of the same announce
still cancels. The wire is unchanged; the deviation only keeps an
announce on its way that nobody has forwarded yet.

One difference stays, noted here because the control has to work
around it: our replay gate runs before this check and the reference's
after it (`:1772`), so an identical copy normally never reaches either
arm here.

## 9. Same emission against the installed count, and a dead next hop

**The invariant.** A second copy of an emission we already hold wins on
hop count only if it is shorter than the count we installed that
emission at — the count every rebroadcast and path response of ours
then told our neighbours. A neighbour that learned the emission through
us offers it at that count plus at least one, so its copy can never be
"fewer hops", however a link proof has moved our own count since.

The reference has no hop count in the same-emission test at all
(`Transport.py:1772` wants an unseen blob and a strictly newer emission;
the only same-emission acceptance is the unresponsive one at
`Transport.py:1822`), so its 1.5 link-proof rebalance cannot turn an
echo into a better route. Ours has one (§5, `FewerHopsWins`), and until
order 1672 it compared against the REBALANCED count. On miauhaus
(2026-11-11) a proof at 6 hops moved a 3-hop path to 6
(`PATH_REBALANCE from=3 to=6`), and the next answer, the same emission
at 4 hops from another neighbour, was adopted as
`same_emission_fewer_hops`; every link request after it died on that
neighbour. Since 1672 `same_emission_hop_floor` in
`leviculum-core/src/transport.rs` reads the installed count from the
announce cache (the accepted copy, cached with its wire count) and both
the replay gate and the acceptance arms compare against it; the refused
copy is logged as `PATH_REFUSED reason=longer_than_installed`. What the
deviation still allows is the case it was made for: a copy shorter than
the installed count, the direct copy after a relay, which heals
`conformance/link_failure_recovery`.

**A dead next hop.** The reference marks a path unresponsive when a
forwarded link request expires unproven only if the destination or the
initiator was one hop away (`Transport.py:724` onward); for a request
from a local client it only asks the path again (`Transport.py:715`).
A daemon whose links come from its own clients (a mailbox host, a
desktop running lnomad) therefore never left a relayed next hop that
stopped delivering: every answer to the path request was the emission it
already held, and the longer copies were refused while the path was not
unresponsive. Since 1672 such an expiry marks the path unresponsive when
it has a next hop (`PATH_UNRESPONSIVE`), and refuses that next hop's
copies of the held emission until the destination announces anew
(`PATH_REFUSED reason=next_hop_unresponsive`, the table leviculum#852's
loop refusals live in). The unresponsive rule then takes another next
hop's copy, more hops or not, and the dead hop's shorter copy cannot
take the path back. A link toward the same destination proven after the
expired one was set outranks the expiry and nothing is marked: on
miauhaus the first of two requests expired 1.6 s after the second one's
proof came back over the same next hop.

The mvr is `leviculum-core/src/node/mvr_path_answer_through_us.rs`: the
three-node loop (a neighbour that routes the destination through us
hands our link request back), the heal after a rebalance, the five
expiries of the field, and the later-proof race with its positive
control. Wire and semantics are unchanged: a copy is accepted or not,
a path entry is ours alone.
