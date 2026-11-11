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

The node box is the one `Box<NodeCore<Entropy, EmbassyClock,
EmbeddedStorage>>` the firmware allocates at boot (`Entropy` since 959,
`RawHwRng` before; both are zero-sized, so the box did not move). It is the same type
on the T114, the RAK4631 and the solar node, and it measures the same
on all three: **31 744 B** since 1380 (31 752 B since 981, 31 744 B since 867, 31 736 B since 860, 31 712 B since #852, 31 704 B since #666, 31 696 B at 68429da6), read as `size_of` of that type
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
`slack=608 serve_cap=154`; the node box has grown 600 B and `per_link`
16 B since.

## What the boot plan sums

`budget_total` in `heap-budget/src/lib.rs`, on all three boards:

| Term | Per entry | Cap | Total | Source |
|---|---:|---:|---:|---|
| Node box, `NodeCore` | | 1 | **31 744** | `NODE_BOX_BYTES`, measured |
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
| rest of `NodeCore` (transport, node state) | | | 1 720 | node box less storage |
| Endpoint links: boxed `Link` + link-map share | 2 632 + 64 = 2 696 | 4 | 10 784 | `LINK_BYTES` (asserted), `LINK_MAP_BYTES_PER_LINK` |
| BLE sessions: `LINK_OUT` 4 × 564, pump 2 × 564, defragmenter 564 | 3 948 | 4 | 15 792 | `SESSION_BUDGET_BYTES` |
| Propagation role | | 1 | 22 144 | `ROLE_BUDGET_BYTES` |
| of which the sync slice | | 1 | 8 000 | `SYNC_SLICE_BYTES` |
| of which queued uploads | 4 000 | 2 | 8 000 | `BOARD_TRANSFER_LIMIT_KB` |
| of which structural (peer table at 7 peers, offer plan, maps, cache) | | 1 | 6 144 | `ROLE_STRUCTURAL_BYTES` |
| of which the serve reserve | | | 0 | `SERVE_RESERVE_BYTES` |
| Reserve | | | 17 824 | `budget_reserve` |
| of which fragmentation | | | 5 120 | `FRAG_RESERVE_BYTES` |
| of which the shared BLE channels | 2 × 4 × 564 | | 4 512 | `CHANNEL_BUDGET_BYTES` |
| of which one incoming resource | 8 192 | 1 | 8 192 | `MAX_INCOMING_RESOURCE_BYTES` |
| **Total** | | | **98 288** | `budget_total` |
| **Heap** | | | **98 304** | `HEAP_SIZE` |
| **Slack** | | | **16** | `budget_slack` |
| **Boot serve cap** | | | **0** | `budget_serve_cap` |

The heap is full but for 16 B: 31 696 B was exactly the node box at
which four links fit with nothing left, #662 took 16 B a link out
of `Link` (the RTT retry state), so the boundary now sits at 31 760 B
(`max_endpoint_links_at_its_boundaries`), and #666 spent 8 B of the
gain on the boxed split request/response state in `NodeCore`, #852 8 B
more on the boxed loop refusals in `Transport`, 860 24 B more on five
link-layer drop counters in `TransportStats`, 867 8 B more on two
resource-refusal counters there, 981 8 B more on the boxed
torn-down-interface tombstones in `Transport`, and 1380 gave 8 B back
by boxing the path-choice evidence map, 12 B inline, beside the
per-interface declaration it added (never allocated on a board). 17 more bytes in
`NodeCore`, or more than 4 B a link in `per_link`, cost an endpoint link
and fail the binaries' `MAX_ENDPOINT_LINKS >= ble::MAX_LINKS` assert at
compile time. 16 B funds no serve, so the boot serve cap is still 0 B and the propagation
role serves only what the live heap funds at each `/get`
(`live_serve_cap`).

## The serve transient: gated live, not reserved

Answering one `/get` costs the board a transient of its own: the record
it streams, one part's scratch and the encrypted transfer
(`serve_peak_bytes` in `leviculum-lxmf/src/propagation_node.rs`). At the
announced cap that is `SERVE_PEAK_BYTES`, **17 264 B**, and the plan
above funds none of it: the serve reserve is 0 and the slack is 16 B.
Every boot says so in one line after `HEAP_BUDGET`:

```text
HEAP_BUDGET_SERVE mode=live_gated reserve=0 need=17264 free_at_boot=<n>
```

That is a decision (Lead, 2026-11-11, order 1676), not a gap. Funding
the transient would take 17 248 B the plan does not have. With the four
BLE sessions fixed, the remainder the endpoint links are divided out of
is 10 800 B, so no link would be left and the binaries'
`links >= ble_links` assert would refuse the build; taken from links
with their sessions (2 696 + 3 948 = 6 644 B each) it is three of the
four. A propagation node with one link is no propagation node.

The protection is live instead. Every `/get` reads the free heap and its
largest block, takes `SERVE_MARGIN_BYTES` (14 696 B) off for what can
still arrive while the serve is in flight, and serves only what the rest
funds (`live_serve_cap`, `leviculum_pn_store::board_answer_get`; the
`SERVE_CAP` line carries every input). A heap that funds no message
turns the `/get` into a refusal and `PN_REJECT reason=heap_low via=get`
on the board:

* the list form answers `ERROR_THROTTLED` (`0xf6`), which a stock client
  logs as `Invalid message list data received from propagation node` and
  tears the link down on (`reference/LXMF/LXMF/LXMRouter.py:1601-1604`);
* the fetch form answers the empty list, which a stock client completes
  with zero messages and no confirmation (`:1620-1644`).

Either way nothing is purged, the mail stays in the store, and the next
sync after the heap frees is served. The model is measured against the
real path, advertisement and first send included
(`a_streamed_serve_holds_only_the_transfer`,
`leviculum-std/tests/mvr/pn_serve_peak_outgrows_the_board_heap.rs`), so
the gate covers the whole serve. The host proof of the refusal is
`a_heap_that_funds_no_message_refuses_the_get_and_the_next_one_is_served`
in `leviculum-nrf/pn-store/src/tests.rs`.

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
its BLE session) buys 6 644 B but drops a claimable BLE connection,
which is a phone or a peer the board can no longer hold.
