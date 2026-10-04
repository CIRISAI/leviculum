# A new link carries the announces it missed — the #383 design, measured (#434)

**Date:** 2026-10-04.
**Question:** when a BLE link comes up, which announces should the node
emit toward the new peer, and under what cap?
**Instrument:** seed 6's own room of the 2026-09-27 sweep, replayed —
`leviculum-nrf/ble-tx/tests/graph_formation.rs`,
`seed6_replay_design_table` and
`the_bridge_carries_the_stored_announce_or_nothing_ever_does` (both
pinned).

## The timeline, from the room's log

`ble_room_10_2026-09-27T21-36-11Z.log` pairs its twenty `BLE_LINK_UP`
lines into ten edges (the emulator address encodes the node index). Ms
after 21:33:30Z:

```
 1160 4–8    2742 0–2    3022 8–9    3464 2–3     4903 3–4
 6414 1–4   11650 5–6   17718 6–7   24450 7–8    52770 7–9
```

That is the 377 report's edge list — chain 0–2–3–4–8, cycle 7–8–9, tail
5–6–7 — plus one fact the report had no use for: **edge 7–9 landed at
52.8 s**, after the 7–8 bridge (24.5 s), which is why node 9 held no
relay of node 6's announce either.

## The model

An announce emission is one packet on the emitter's broadcast domain,
heard by every neighbour whose link exists at emission time. A receiver
that holds nothing fresher adopts and relays: two emissions
(`PATHFINDER_RETRIES = 1`, constants.rs:157), at receipt + 0.2 s and
+ 5.5 s — node 7's measured relay times for node 6's announce (47.9 and
53.2 against receipt at 47.7). An emission the receiver already holds
is dropped and not relayed (packet-hash dedup / the not-newer rule),
which is what keeps a stored re-offer quiet everywhere it is not news.
A node's boot announce fires at its first link-up (an announce with no
link is unobservable in the room).

Candidates, per link-up of edge (u, v), both directions:

- **none** — pre-#376 control: nothing.
- **own** — shipped #376, the instruction's (a): one FRESH own
  announce on the new link alone.
- **stored** — candidate (b): own, plus every stored announce u holds a
  live path and cached announce for, EXCEPT v's own destination and
  entries learned via v (the #168 bounce-back rule).
- **full** — candidate (c): own plus the whole table, no exclusions.

## The table (pinned in `seed6_replay_design_table`)

| policy | dark-to-6         | dark pairs | announce TX | bridge TX | max link-up TX |
|--------|-------------------|-----------:|------------:|----------:|---------------:|
| none   | 0,1,2,3,4,7,8,9   |         44 |         112 |         0 |              0 |
| own    | 0,1,2,3,4,8,9     |         30 |         176 |         2 |              2 |
| stored | —                 |          0 |         280 |        10 |             18 |
| full   | —                 |          0 |         282 |        10 |             20 |

Calibration: the `own` row IS the shipped stack, and its dark-to-6 set
is the run's five red probes {0,1,2,3,4} plus {8,9} — exactly the two
nodes the measure phase repaired through node 7's one-hop path
responses, which sit outside the announce model. The model reproduces
the red from the timeline alone.

## Verdict: (b), `stored`

- `own` cannot carry a THIRD node's announce across a late bridge: the
  bridge endpoints exchange themselves and nothing else. Seed 6's red
  is precisely that shape, and the sweep put its rate at about one room
  in twelve (377 report).
- `stored` lights the whole room, within seconds of the last edge.
- `full` buys nothing over `stored`: its extra packets are pure
  bounce-backs (both of them, on edge 7–9, are announces the peer
  itself had supplied), and that waste grows with degree and room age.

Airtime, worst link-up of the room: 18 packets × 183 B (the room's own
relayed-announce `BLE_TX_FLOOD len=`) ≈ 3.3 kB on the one BLE link that
came up — ~38 ms at the interface's 700 kbit/s bitrate guess. On LoRa:
nothing, by construction — the re-offer is emitted with the #376
delivery hint on the link that came up; a LoRa interface beside it
never carries the re-offer itself, only a neighbour's ordinary
rebroadcast of what was news to it, which the #402 announce cap paces
where one is registered.

## The reference, read for the occasion

Python-RNS 1.3.5 does nothing when an interface or link appears — there
is no interface-up hook in `Transport` at all. Its job loop retires an
announce-table entry at `retries > PATHFINDER_R` (Transport.py:585-587,
`PATHFINDER_R = 1` at :68), and its interface bookkeeping only culls:
paths whose attached interface no longer exists are REMOVED
(Transport.py:785-787). After retirement a stored announce is re-emitted
on exactly one occasion, a path request. The re-offer is therefore a
deliberate deviation; all three clauses hold (wire: ordinary Header-2
transit announces; semantics: a Python peer dedups or adopts them like
any announce; P1: the table above).

## What ships

`Transport::reoffer_stored_announces_to_peer` (leviculum-core), called
from `NodeCore::handle_interface_peer_up` — the one peer-up entry point
both stacks already share, so lnsd and the boards get the rule in the
same commit. Emission is `ANN_TX occasion=reoffer`; a cap-held re-offer
queues with its delivery hint and drains to the same peer. mvr:
`leviculum-core/src/node/mvr_link_up_reoffer.rs` (red-proven against
the shipped stack, then green).

Predicted readings: `ble_room_10` seed 6 GREEN (all 20 endpoints), the
other seeds unchanged, and each room's `ANN_TX` count up by roughly the
table's `stored`−`own` margin (~100 packets per room, all on link-local
BLE emissions and the relays that were news).
