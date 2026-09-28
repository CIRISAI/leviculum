# Seed 6 of the arrival-order sweep: the route that was never there, and what the tie-break cannot buy

The 2026-09-27 arrival-order sweep (lnsd `da310574`, seeds 2..13, patched
btvirt) left one red: seed 6, `5 of 90 probes unanswered`, all five aimed at
node 6 from nodes 0..4. This document reconstructs the room from the run's
own logs, names the mechanism, and measures the three candidate tie-breaks
of #434 in the `graph_formation` harness. Verdict up front: the five probes
died of **no path, not of path length**; the graph's longest way (7 hops)
answered in under 3 s against the 20 s timeout; and no strict-class
tie-break moves the number the red actually lost on, while both candidates
split rooms the shipped key never splits. The recommendation is (c), leave
the tie-break, and fix the announce side instead.

Refs: Codeberg #434 (this analysis), #432 (the asymmetric cap whose first
sweep this was), #375 (the window), 363 §4 (the prediction this tests).
Room log: `/home/lew/ci/periculum-gate/periculum/logs/ble_room_10_2026-09-27T21-36-11Z.log`
(the seed-6 run's own "Logs saved to" line; the 21-38-43Z file named in
early notes is seed 7's). Sweep logs: `/home/lew/ci/p363/seed-{2..13}.log`.

## 1. The room seed 6 built

Order `4,8,0,9,2,7,6,5,3,1`, spacing 500 ms, settle 130 s. Edge list from
the ten daemons' `BLE_LINK_UP` pairs (identity prefix → node:
3c95f2fa=0, 0783d6d0=1, eedd2f95=2, 61b5281d=3, fb920d9f=4, 7e004eba=5,
016b5d17=6, 94a90d62=7, 7c39b530=8, fc8de803=9), with formation times:

| edge | up at (UTC) | | edge | up at |
|---|---|---|---|---|
| 4–8 | 21:33:31.2 | | 1–4 | 21:33:36.4 |
| 0–2 | 21:33:32.7 | | 5–6 | 21:33:41.6 |
| 8–9 | 21:33:33.0 | | 6–7 | 21:33:47.7 |
| 2–3 | 21:33:33.5 | | **7–8** | **21:33:54.5** |
| 3–4 | 21:33:34.9 | | 7–9 | 21:34:22.8 |

Ten nodes, ten edges, one component: the chain `0–2–3–4–8` and the chain
`5–6–7` joined to the cycle `7–8–9`. Diameter 7 (the pair 0↔5). Degrees
match the harness `BLE_ROOM_LINKS` lines exactly.

Hop distance to node 6: nodes 5,7 = 1; 8,9 = 2; 4 = 3; 1,3 = 4; 2 = 5;
0 = 6. The five failed probes are **exactly the five nodes ≥ 3 hops from
node 6**, and nothing else:

- every probe INTO 6 from 0..4 failed;
- every probe FROM 6 outward succeeded, including 6 hops to node 0
  (2457 ms) — so forwarding across the whole chain works in that direction;
- the diameter pair itself probed green both ways: 0→5 at 2337 ms and
  5→0 at 2872 ms over **7 hops**. Worst answered RTT in the room:
  2.9 s. The 20 s probe timeout was never touched by any answered probe.

## 2. What the five died of: no path

The five probers' logs each show, for node 6's probe destination
`7e7b0fa4`: `Path request ... from local client, forwarding to all other
interfaces` — the no-path branch — while every other destination in the
same run answers `path is known`. The path table of nodes 0..4 never held
node 6. Why:

1. Node 6's announce reached node 7 at 21:33:47.7 (the instant the 6–7
   link came up) and again at 21:33:51.8 (6's own retry 1). Its retry
   ladder then ended: `PATHFINDER_RETRIES = 1` (constants.rs:157), two
   emissions per relay, matching Python's `PATHFINDER_R = 1`
   (Transport.py:68). The "retries=2 scheduled" log lines are dead
   bookkeeping — 115 scheduled in this room, zero fired, because
   `check_announce_rebroadcasts` retires an entry at `retries >
   PATHFINDER_RETRIES` before it can fire.
2. Node 7 relayed the announce at 21:33:47.9 and 21:33:53.2 — both
   emissions **before the 7–8 bridge existed at 21:33:54.5**. By 1.2
   seconds, node 6's announce missed the only edge that could carry it
   into the 0–4 cluster. Nothing re-offers a stored announce when a new
   link comes up, so the miss is permanent for the rest of the settle.
3. Contrast node 5 (one hop deeper): its own retry 1 happened to fire
   after the bridge was up, restarting fresh two-emission ladders at
   every hop — its announce reached node 0 at 21:34:06 as `hops=7`.
   The one-position difference between "5 is reachable by everyone"
   and "6 is not" is scheduling luck, not topology.
4. The measure phase could not repair it. Nodes 0..4 flooded path
   requests at 21:35:45–48; only node 7 knew the path and answered; its
   path response reached its direct neighbours 8 and 9 (whose own
   pending requests it satisfied — both their probes then passed), and
   stopped there: path responses are not rebroadcast
   (`should_rebroadcast=false`, and the reference behaves the same —
   Transport.py:1886 excludes `PATH_RESPONSE` from the announce table,
   and recursive path discovery is gated on `DISCOVER_PATHS_FOR`
   interface modes, i.e. access-point/gateway, not default). Our
   behaviour here is reference-faithful; a default-config Python room
   has the same hole.

So 363 §4 was half right: the sparse room did form a long, chain-like
graph, and the probe stage did fail toward node 6 — but not because
20 s ran out over a long path. The chain shape's real cost is that it
forms **late and serially**, and a bridge that lands after an announce
ladder's ~25 s life leaves a permanent one-way hole that neither the
remaining settle (75 s of silence) nor the probes' path requests can
close.

## 3. The tie-break candidates, measured (#434)

`graph_formation.rs` gained the diameter and mean-path statistic, a
"round the room first became one component" statistic, and the two
candidate keys, all through the real `CandidateTable`
(`the_strict_tie_break_decides_the_room_diameter`, cells pinned).
1000 orders per cell, board shape (1 outgoing + 3 incoming), empty room:

| tie-break        |  n | diam p50/p90/max | mean path | connected-at-round p50/p90/max | split | linkless | dark |
|------------------|---:|------------------|-----------|-------------------------------|------:|---------:|-----:|
| lowest (shipped) | 10 | 6 / 7 / 8        | 2.66      | 9 / 9 / 16                    |     0 |        0 |  498 |
| (a) highest      | 10 | 6 / 7 / 8        | 2.69      | 9 / 10 / 16                   |     0 |        0 |  726 |
| (b) pair-hash    | 10 | 6 / 7 / 8        | 2.67      | 9 / 9 / 16                    |     6 |        0 |  604 |
| lowest (shipped) | 20 | 10 / 13 / 16     | 4.36      | 19 / 25 / 26                  |     0 |        0 |  914 |
| (a) highest      | 20 | 8 / 10 / 12      | 3.96      | 19 / 24 / 26                  |    90 |        0 | 1582 |
| (b) pair-hash    | 20 | 9 / 11 / 14      | 4.13      | 19 / 25 / 26                  |    62 |        0 | 1188 |

(The shipped row's dark cells, 498 and 914, equal the eager/mostfree
cells of the #375 item 3 table — on an empty room of static addresses
the shipped key is that key, which calibrates the new instrument
against the old one.)

Three readings:

- **At ten boards the tie-break does not move the shape.** All three
  diameter distributions are the same three percentiles; seed 6's
  diameter-7 room is the shipped distribution's own top decile, not a
  pathology the tie-break created.
- **Both candidates split rooms the shipped key never splits** (90 and
  62 per 1000 at n=20; the hash even 6/1000 at n=10). The room-wide
  agreement on one preference order is what closed the saturated-cycle
  lock, and any tie-break that weakens it — a private order per
  searcher, or one that piles dials onto the top of the room until it
  saturates and goes dark (726 and 1582 dark boards) — buys diameter
  at the cost of the primary guarantee.
- **No tie-break touches the number the red lost on.** The
  connected-at-round column is flat: the last connecting edge lands
  when the late arrivals' slots let it (~45 s median, up to ~80 s
  at n=10 at 5 s per round), under every dial order. An announce
  ladder that dies ~25 s after boot loses that race with some seeds no
  matter how the dials are ordered.

## 4. Recommendation

1. **Tie-break: (c), leave it.** Neither candidate wins; both regress
   Priority 1. The pinned table is the record.
2. **The real fix is announce-side**: when a BLE link comes up (or
   generally, when an interface gains a peer), re-offer the node's own
   announces — and, for a transport node, the stored announces of the
   destinations it knows — on that interface. Wire-compatible (a
   normal announce packet), semantically what a Python peer already
   tolerates, and it removes the race structurally: a bridge formed at
   t+54 s carries the announce at t+54 s instead of never. This is a
   deviation from the reference under the deviation rule (the reference
   has the same hole; measurably improves P1). Needs its own issue and
   design pass — it is deliberately NOT part of the #434 batch.
3. **The probe timeout stays 20 s.** Worst measured RTT over the worst
   shape (7 hops) is 2.9 s, ~400 ms per hop worst-case; even a
   diameter-9 path projects to ~4 s. The timeout was not the binding
   constraint and relaxing or tightening it changes nothing about this
   failure class.

## Appendix: commands behind the figures

Edge list and roles: `awk '/^=== node [0-9]+ daemon log ===/{node=$3} /BLE_LINK_UP/...' <room log>` — each daemon section's `BLE_LINK_UP peer= role=` lines, peer prefix mapped through the sections' `IDENTITY` lines.
Probe matrix: `sed -n '14,106p' <room log>` (`BLE_ROOM_PROBE` lines).
No-path proof: `grep 7e7b0fa4` in each prober's section — `forwarding to all other interfaces` vs `path is known` everywhere else.
Retry ladder: `grep -c 'announce retry scheduled.*retries=2'` (115) vs `grep -c 'announce retry firing.*retries=2'` (0).
Node 5's hops=7 contrast: node 0's `PATH_ADD dst=d428...` at 21:34:06.
Simulation: `cd leviculum-nrf && cargo test -p leviculum-ble-tx --target x86_64-unknown-linux-gnu --test graph_formation the_strict_tie_break -- --nocapture`.
