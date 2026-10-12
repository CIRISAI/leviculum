# The board heap budget

Every LNode boots with a `HEAP_BUDGET` line that adds up the worst case
of everything the firmware holds on its 96 KiB heap and refuses to run
a configuration that does not fit (Codeberg #388). The arithmetic lives
in `leviculum-nrf/heap-budget`, where host tests pin it
(`heap-budget/tests/budget.rs`); the firmware calls into it and asserts
the target sizes it cannot measure on a host (`Link`, the node box, the
held-announce entry) at compile time in `leviculum-nrf/src/heap_census.rs`.

This page is the table the budget adds up, with the source for each
number. Sizes are `size_of` on `thumbv7em-none-eabihf` unless a row
says otherwise.

## How the node box is measured

The node box is the one `Box<NodeCore<RawHwRng, EmbassyClock,
EmbeddedStorage>>` the firmware allocates at boot. It is the same type
on the T114, the RAK4631 and the solar node, and it measures the same
on all three: **31 696 B** at 68429da6, read as `size_of` of that type
in the release build of each binary (`cargo build --release --features
bsp-t114 --bin t114`, and the `bsp-rak4631,rak-baseboard` and
`bsp-solarnode` equivalents) off a compile error that prints it. At
runtime the board prints the same figure as `size_of_val(&*node)`: the
`node_box=` of every `[HEAP_CENSUS]` line, and the argument the
binaries hand `log_budget_and_assert`.

It is pinned as `leviculum_heap_budget::NODE_BOX_BYTES` and the
firmware asserts it, so a change to the node box fails `just lint-nrf`
until the pin moves. Until #661 the host tests used a stated 31 040 B
instead; it had gone stale by 656 B, so they pinned 656 B of slack and
a 170 B serve cap while every board built from the tree had neither.
The field T114 on ed0754e4 (2026-10-07) printed `node_box=31152` and
`slack=608 serve_cap=154`; the node box has grown 544 B and `per_link`
16 B since.

## What the boot plan sums

`budget_total` in `heap-budget/src/lib.rs`, on all three boards:

| Term | Per entry | Cap | Total | Source |
|---|---:|---:|---:|---|
| Node box, `NodeCore` | | 1 | **31 696** | `NODE_BOX_BYTES`, measured |
| of which `EmbeddedStorage` | | 1 | 29 968 | measured |
| `packet_cache` + `packet_cache_prev` | 16 B key | 2 × 256 | 11 272 | measured, 5 636 each |
| `known_identities` | `Identity` 504 B, 533 B a slot | 8 | 4 264 | measured |
| `path_table` | `PathEntry` 64 B, 100.5 B a slot | 32 | 3 216 | measured |
| `announce_rate_table` | 24 B, 60.5 B a slot | 32 | 1 936 | measured |
| `announce_table` | `AnnounceEntry` 56 B, 93 B a slot | 16 | 1 488 | measured |
| `path_request_tag_set` | 32 B key, 44.25 B a slot | 32 | 1 416 | measured |
| `link_table` (transit links) | `LinkEntry` 112 B, 150 B a slot | 8 | 1 200 | measured |
| `reverse_table` | `ReverseEntry` 32 B, 69 B a slot | 16 | 1 104 | measured |
| `path_states` | 32.25 B a slot | 32 | 1 032 | measured |
| `receipts` | `PacketReceipt` 88 B, 126 B a slot | 8 | 1 008 | measured |
| `announce_cache` | `Vec` header, 40.5 B a slot | 16 | 648 | measured |
| `known_ratchets` | 78 B a slot | 8 | 624 | measured |
| `path_requests` | 46 B a slot | 8 | 368 | measured |
| `discovery_path_requests` | 56 B a slot | 4 | 224 | measured |
| `dest_ratchet_keys` | `Vec` header, 42 B a slot | 4 | 168 | measured |
| rest of `NodeCore` (transport, node state) | | | 1 728 | node box less storage |
| Endpoint links: boxed `Link` + link-map share | 2 648 + 64 = 2 712 | 4 | 10 848 | `LINK_BYTES` (asserted), `LINK_MAP_BYTES_PER_LINK` |
| BLE sessions: `LINK_OUT` 4 × 564, pump 2 × 564, defragmenter 564 | 3 948 | 4 | 15 792 | `SESSION_BUDGET_BYTES` |
| Propagation role | | 1 | 21 120 | `ROLE_BUDGET_BYTES` |
| of which the sync slice | | 1 | 8 000 | `SYNC_SLICE_BYTES` |
| of which queued uploads | 4 000 | 2 | 8 000 | `BOARD_TRANSFER_LIMIT_KB` |
| of which structural (peer table, offer plan, maps, cache) | | 1 | 5 120 | `ROLE_STRUCTURAL_BYTES` |
| of which the serve reserve | | | 0 | `SERVE_RESERVE_BYTES` |
| Reserve | | | 18 848 | `budget_reserve` |
| of which fragmentation | | | 6 144 | `FRAG_RESERVE_BYTES` |
| of which the shared BLE channels | 2 × 4 × 564 | | 4 512 | `CHANNEL_BUDGET_BYTES` |
| of which one incoming resource | 8 192 | 1 | 8 192 | `MAX_INCOMING_RESOURCE_BYTES` |
| **Total** | | | **98 304** | `budget_total` |
| **Heap** | | | **98 304** | `HEAP_SIZE` |
| **Slack** | | | **0** | `budget_slack` |
| **Boot serve cap** | | | **0** | `budget_serve_cap` |

The heap is full to the byte: 31 696 B is exactly the node box at which
four links fit with nothing left (`max_endpoint_links_at_its_boundaries`).
One more byte in `NodeCore`, or in `Link`, costs an endpoint link and
fails the binaries' `MAX_ENDPOINT_LINKS >= ble::MAX_LINKS` assert at
compile time. The boot serve cap is 0 B, so the propagation role serves
only what the live heap funds at each `/get` (`live_serve_cap`).

## What the boot plan does not sum

These live on the heap the board has, not in `budget_total`:

| Term | Per entry | Cap | Worst case | Source |
|---|---:|---:|---:|---|
| Held announces (ingress limiter), LoRa and BLE | 32 B entry + raw ≤ 500 B | 6 per interface, 2 interfaces | 7 256 of an 8 192 slice | `held_announce_bytes`, `HELD_ANNOUNCE_SLICE_BYTES` |
| Queued announces (LoRa announce cap) | ~220 B | 8 | ~1 800 | estimate in `src/bin/t114.rs` beside `max_queued_announces(8)` |
| `announce_table` raw packets | ≤ 500 B | 16 | ≤ 8 000 | `AnnounceEntry::raw_packet`, `MTU` |
| `announce_cache` raw packets | ≤ 500 B | 16 | ≤ 8 000 | `announce_cache: …Vec<u8>, 16` |
| Path random blobs | 10 B | 8 per path, 32 paths | ≥ 2 560 | `max_random_blobs(8)`, `RANDOM_HASHBYTES` |
| Incoming resources beyond the one reserved | 8 192 | 1 per link, 4 links | 3 more than funded | `BOARD_INCOMING_RESOURCES_PER_LINK` |

The last row is worth reading twice: the reserve funds one incoming
resource at the 8 KiB cap, while the board admits one per endpoint link,
so a second, third and fourth transfer in flight at once have no term.
The role's queued uploads (2 × 4 000 B) price what an upload holds once
it has arrived, not the transfer that carries it.

What the live heap actually reached: the field T114 on ed0754e4,
2026-10-08 to 2026-10-11, 2 388 `[HEAP_CENSUS]` lines, `used=` at most
59 640 B, `transport=` at most 11 032 B, `storage=` at most 2 407 B
(`rig-run/ble-drop/feld-t114.log`).

## Statics and the stack

The BLE dial and churn ledgers are statics, not heap: `DIAL_LEDGER`
336 B and `CHURN_LEDGER` 400 B (measured), in `src/ble/columba.rs`.

The stack is the other half of #50's open question, because the heap is
a static and growing it shrinks the flip-link stack region below it by
the same amount. Painted-stack watermarks (`[STACK]` line,
`leviculum-nrf/src/lib.rs`):

| Board | Region | Peak used | Never touched | Source |
|---|---:|---:|---:|---|
| T114, field | 95 024 | 26 412 | 68 612 | `rig-run/ble-drop/feld-t114.log`, 2026-10-11 |
| T114, boot capture | 94 984 | 27 692 | 67 292 | `rig-run/ble-drop/t114-boot.log` |
| RAK4631 (Pocket), field | 96 640 | 15 368 | 81 272 | `rig-run/ble-drop/feld-pocket.log` |
| Solar node | | | | not captured |
| T114, 2026-09-10 | 104 464 | 32 184 | 72 280 | `docs/src/concepts/multi-peer-interfaces.md` |

The worst peak in these captures is 32 184 B. #50 measured 107 KiB by
the same paint on 2026-07-02, read through the SWD probe; nothing on
this page says what removed the difference, so a heap that grows into
the stack region should be preceded by a fresh painted capture under
the two-phone propagation load, not by this table.

## Where a kilobyte could come from

None of these is made; each is a decision.

* **Fragmentation reserve 6 → 5 KiB** buys 1 024 B of slack and changes
  no code path. The cost is elbow room: the reserve exists because the
  #388 field board refused 340 B with 4 760 B free.
* **`known_identities` 8 → 6** buys 1 066 B in the node box. The cost:
  the slots serve the two phones' delivery identities plus the
  neighbours whose announces are being verified; at six that working
  set has less room, and an evicted identity is missing until its
  owner announces again (300 s for a phone), so a message to it waits
  that long.
* **`path_request_tag_set` 32 → 8** buys 1 062 B in the node box. The
  cost: under a path-request flood the dedup window is four times
  shorter, so a relayed request the board has already acted on is acted
  on again once eight others came in between, which spends airtime on a
  repeated answer or forward.

Two that look like candidates and are not: the hold cap 6 → 5 saves
1 000 B of the held-announce slice, which is live heap outside
`budget_total`, so the boot slack does not move. One fewer link (with
its BLE session) buys 6 660 B but drops a claimable BLE connection,
which is a phone or a peer the board can no longer hold.
