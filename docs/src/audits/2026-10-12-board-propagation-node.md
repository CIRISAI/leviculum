# The board as a propagation node, read against lnpnd and the reference (2026-10-12)

An audit in the shape of this week's three (the RPC audit 2026-10-11,
the air-path audit, the link-and-resource audit 2026-10-12): the
board's propagation role — `leviculum-nrf/src/pn.rs` over the shared
crates `leviculum-lxmf/src/propagation_node.rs` and
`leviculum-lxmf/src/peering.rs`, budgeted by
`leviculum-nrf/heap-budget/src/lib.rs` — read line by line against
`lnpnd` and against the reference (`reference/LXMF` at pin 795fdaa).
Nothing was changed; this document is the deliverable.

Each divergence names: the board and its counterpart by line, what a
stock peer or a Columba phone sees, a class — *wire*, *semantic*,
*timing*, *deviation ours (deliberate)*, *inert* — the test that would
pin it, and — this audit's own addition — its **price** on the board:
heap bytes with the budget term they move
(`docs/src/firmware/heap-budget.md` is the table they move in), stack,
flash, airtime. The Leitstern context: the board IS the propagation
node, and this week every propagation decision was taken for lnpnd
with the board as the exception — rotation (0a1d7e38), host pricing
(8f470aa7), load-time re-pricing (645e3580), depth 6 (leviculum#442
D10), the link-hops sync rule (8756a5df), the 180 s idle teardown.
This is the one place those exceptions stand beside what they cost to
close, which is the table the heap decision of #661 needs.

The budget frame every price below moves in, from the boot plan
(`budget_total`, `leviculum-nrf/heap-budget/src/lib.rs:408`): heap
96 KiB (`HEAP_SIZE`, `leviculum-nrf/heap-budget/src/lib.rs:49`), node
box 31 704 B (`NODE_BOX_BYTES`,
`leviculum-nrf/heap-budget/src/lib.rs:829`), role budget 21 120 B
(`ROLE_BUDGET_BYTES`, `leviculum-nrf/heap-budget/src/lib.rs:298` =
sync slice 8 000 + two queued uploads 8 000 + structural 5 120 + serve
reserve 0), four endpoint links with four BLE sessions, reserve
18 848 B, **slack 56 B** (#661, #662, #666). Every "fund it from the
budget" below is a claim against those 56 bytes unless it names the
term it shrinks instead.

## 1. Peering

Where the board and lnpnd are the same code, verified: the announce
decision tree with the #417 path-response rule (`handle_announce`,
`leviculum-lxmf/src/peering.rs:896`, against
`reference/LXMF/LXMF/Handlers.py:56-99` and `peer`/`unpeer`,
`reference/LXMF/LXMF/LXMRouter.py:2004-2058`); timebase-ordered
updates (`admit`, `leviculum-lxmf/src/peering.rs:1068`, against
`reference/LXMF/LXMF/LXMRouter.py:2016`); the full-table decline
(`leviculum-lxmf/src/peering.rs:1102`, against
`reference/LXMF/LXMF/LXMRouter.py:2032`); the 14-day cull (`cull`,
`leviculum-lxmf/src/peering.rs:1138`, `MAX_UNREACHABLE_SECS`,
`leviculum-lxmf/src/peering.rs:92`, run from the board's maintenance
pass every 480 s, `tick_maintenance`,
`leviculum-nrf/src/pn.rs:2616`, against
`reference/LXMF/LXMF/LXMRouter.py:2136-2140`), epoch-guarded on the
board against the clock seed (`refresh_after_calendar_jump`,
`leviculum-lxmf/src/peering.rs:808`, a board-only addition with no
reference counterpart because the reference always has a calendar);
the inbound `/offer` gate in the reference's order — sequential
validation, concurrent syncs, per-peer throttle, static-only
(`InboundGate::admit`, `leviculum-lxmf/src/peering.rs:1514`, against
`reference/LXMF/LXMF/LXMRouter.py:2274-2295`); the 180 s
invalid-stamp throttle (`PN_STAMP_THROTTLE_SECS`,
`leviculum-lxmf/src/peering.rs:87`, applied at
`leviculum-nrf/src/pn.rs:3502`, against
`reference/LXMF/LXMF/LXMRouter.py:2430-2450`); the peering-key
material order (`peering_key_material`,
`leviculum-lxmf/src/peering.rs:1336`, against
`reference/LXMF/LXMF/LXMPeer.py:258` and
`reference/LXMF/LXMF/LXMRouter.py:2300`); the key mined once at the
peer's announced cost, persisted with the peer, reused while its
value clears the cost (`peering_key_ready`,
`leviculum-lxmf/src/peering.rs:489`, against
`reference/LXMF/LXMF/LXMPeer.py:227-236` — with the shared,
documented cost-0 deviation at the same lines); and the backoff
ladder, one `SYNC_BACKOFF_STEP_SECS` per dial
(`leviculum-lxmf/src/peering.rs:114`, against
`reference/LXMF/LXMF/LXMPeer.py:321-322`) with the shared deliberate
clear-on-link-up deviation (`note_link_established`,
`leviculum-lxmf/src/peering.rs:439`, measured on the rig 2026-09-17).

Keys and stamps on the board, as built: the peer's X25519‖Ed25519
keys are captured with the peer at announce time and persisted
(#388 pass 3, `Peer::capture_identity`,
`leviculum-lxmf/src/peering.rs:505`; `PnPeerStore`,
`leviculum-nrf/pn-store/src/lib.rs:966`), because the board's
`known_identities` cache is 8 slots against up to 16 peers. The
peering key is mined on its own task, cooperatively
(`miner_task`, `leviculum-nrf/src/pn.rs:639`), 10-32 s at the capped
cost 18, logged `PN_KEY`, persisted, never re-mined across a reset.

**D1 — the peer table caps at 16 where the reference and lnpnd cap
at 20.** Board: `BOARD_MAX_PEERS`,
`leviculum-nrf/heap-budget/src/lib.rs:322`, wired at
`leviculum-nrf/pn-store/src/lib.rs:1380`; reference `MAX_PEERS`,
`reference/LXMF/LXMF/LXMRouter.py:43`; the shared default is 20
(`MAX_PEERS_DEFAULT`, `leviculum-lxmf/src/peering.rs:62`). The §5
derivation (`docs/src/concepts/propagation-node-on-a-board.md`,
re-checked at `leviculum-nrf/heap-budget/src/lib.rs:330-337`) admits
N ≤ 19 in the 8 KiB peering slice; 16 keeps margin.
What a peer sees: in a mesh with more than 16 propagation nodes
inside depth, the 17th is declined (`PN_PEER action=decline
reason=table_full`) and — because the board also does not rotate, D3
— never admitted until a 14-day cull or an unpeer frees a slot.
Class: deviation ours (deliberate), compounded by D3.
Price to raise to 20: +4 × 104 B persisted records (flash, nil) and
roughly +1 KiB of table RAM by the census model; the compile assert
at `leviculum-nrf/heap-budget/src/lib.rs:330` refuses 20 at the
current slice, so the peering slice or `OFFER_BYTES_LIMIT` moves
first — ~1 KiB against `ROLE_STRUCTURAL_BYTES` (5 120 B), which has
no 1 KiB of slack: it comes out of a link.
Test: the table-full decline is already pinned
(`leviculum-lxmf/src/peering_tests.rs`); the price needs none.

**D2 — autopeer depth 4 on the board, 6 on lnpnd, 6 in the fielded
stock config.** Board: `AUTOPEER_MAXDEPTH_DEFAULT`,
`leviculum-lxmf/src/peering.rs:70`, the router's code default
(`reference/LXMF/LXMF/LXMRouter.py:45`); lnpnd deliberately runs 6
(`DEFAULT_AUTOPEER_MAXDEPTH`, `lnpnd/src/config.rs:293`) because
lxmd's *generated* config ships 6
(`reference/LXMF/LXMF/Utilities/lxmd.py:995`), so stock nodes in the
field run 6 (leviculum#442 D10).
What a peer sees: a stock node five or six hops out peers with the
board and syncs its store in; the board declines it (`too_deep`) and
never syncs back — the flow is one-directional, exactly the
asymmetry D10 named for lnpnd and lnpnd closed.
Class: semantic (against the fielded mesh; wire-legal).
Price of moving to 6: 0 B of heap; the costs are more sync rounds
toward distant peers (airtime, bounded by the duty share, D16) and
more candidates pressing on the 16-slot table (D1).
Test: an mvr driving `handle_announce` at hops 5-6 asserting
`Added`, board config — one assertion beside lnpnd's
`a_sync_from_a_client_or_from_beyond_the_depth_peers_nobody`
(`lnpnd/src/engine.rs:2406`).
**Fixed in d2a9dcfb**: the board builds its table from
`board_peering_config` (`leviculum-nrf/pn-store`), whose
`BOARD_AUTOPEER_MAXDEPTH` is 6 with lxmd's generated config as its
reason, as lnpnd's 668. Pinned by
`the_board_peers_a_node_six_hops_out_and_declines_seven`
(`leviculum-nrf/pn-store/src/tests.rs`), red at 4 on the hop-6 row.

**D3 — the board never rotates its peer table, and keeps no traffic
counters to rotate on.** The rule is in the shared crate
(`PeerTable::rotate`, `leviculum-lxmf/src/peering.rs:1187`, the
reference's `rotate_peers`,
`reference/LXMF/LXMF/LXMRouter.py:2060-2126`, every 22 min 24 s,
`ROTATION_INTERVAL_SECS`, `leviculum-lxmf/src/peering.rs:109`);
lnpnd runs it on its tick (`rotate_peers`,
`lnpnd/src/peering.rs:384`, landed 0a1d7e38). The board never calls
it (the arm that names the reason says so,
`leviculum-nrf/src/pn.rs:1558-1561`), and the inputs do not exist
there: `PeerTraffic` is a host-side map persisted with the host's
records, while the board's record codec restores zeros
(`leviculum-nrf/pn-store/src/lib.rs:949-950`).
What a peer sees: with D1, a full board table is first-come-forever.
The reference keeps ~10 % headroom by unpeering the peers that
accept the least; a board keeps whoever came first, however dead
their acceptance rate, and a newcomer PN never receives the board's
store.
Class: semantic.
Price to close (why #668 refused it): the rotation inputs are
`offered`, `outgoing`, `last_sync_attempt`, `alive` per peer —
26-32 B a peer, 128-256 B at a working table of 4-8 and ~512 B at
the cap, against `ROLE_BUDGET_BYTES`'s structural term, whose slack
the keys of #388 pass 3 already ate; plus ~26 B per persisted record
(flash, nil) and a transient sort of ≤ 16 references (~300 B) per
pass. CPU and airtime: none (rotation only unpeers).
Test: the shared-crate rule is pinned
(`a_restored_full_table_rotates_out_its_two_lowest_acceptance_peers_on_the_tick`,
`lnpnd/src/peering.rs:1950`); a board engine test would assert the
counters survive `PeerRecord` round-trips through the record log.

**D4 — no static peers on the board.** lnpnd and the reference take
a configured list — never culled, never declined by the cap, gates
bypassed, path requested at start (`activate_static_peers`,
`leviculum-lxmf/src/peering.rs:842`, against
`reference/LXMF/LXMF/LXMRouter.py:633-642`). The board built its
`PeeringConfig` with the default empty list (`board_peering_config`,
`leviculum-nrf/pn-store/src/lib.rs:1379-1387`, which takes a list since
the fix below) and never called `activate_static_peers`; there was no
frame to configure one.
What an operator sees: a board cannot be pinned to a home
propagation node; its peerings are whatever announces reach it
first. A phone sees nothing — this is node-to-node only.
Class: semantic (a configuration surface the board lacks).
Price: ~16 B flash per configured hash in the config page plus the
`lnflash` plumbing; 0 B of heap beyond the table entries the slots
already fund; code only.
Test: board engine test — a configured static hash survives
`cull` at +15 days and bypasses `InboundGate` throttles.
**Fixed in 0e02ccde**: the static peers are one record of the PN log,
`TAG_STATIC` (0xA3), its body the destination hashes, 16 B each, at most
16 (`BOARD_LIST_MAX`, the table's cap), written by `lnflash
--static-peer` over the USB-only frame `TYPE_PN_LIST` (0x16). The engine
reads it at boot into `board_peering_config`, then follows the
reference's order as lnpnd does: restore, `activate_static_peers`, and a
path request for each static peer never heard on the first settle pass
(`PN_PEER action=add reason=static`, `PN_LISTS static_peers=`). The
board has no rotation; the cull is the removal static peers are exempt
from. The record-store task is the record's only writer and rewrites it
when its copy ages into the older half of the region, so reclaim never
takes it. Heap: 16 B per configured hash in the config list, the table
entries as before; 0 B unset. Pinned by
`a_static_peer_set_at_flash_time_is_in_the_table_at_boot_and_never_culled`
(`leviculum-nrf/pn-store/src/tests.rs`: boot read, table entry before
any announce, path owed, cull at +15 days and a rotation pass both
empty), red before the fix (no record, no list), and the reclaim rule by
`a_board_list_ageing_into_the_older_half_is_rewritten_unchanged`.

**D5 — the peer-back recall is RAM-only and does not survive a
reset.** Both stacks peer back a node whose sync lands by recalling
its last announce (`peer_from_recalled_announce`,
`leviculum-nrf/src/pn.rs:2172` and `lnpnd/src/peering.rs:1140`,
against `reference/LXMF/LXMF/LXMRouter.py:2355-2375`). The
reference recalls from `Identity.known_destinations`, which is
persisted; the board recalls from `EmbeddedStorage`'s 16-entry RAM
announce cache, swept of pathless destinations — the honest bound is
*while a path lives, and not across a reset*
(`leviculum-nrf/src/pn.rs:2152-2157`).
What a peer sees: after a board reset — an ordinary event — a
neighbour that announced before the reset syncs in and is answered
`NotANode`; nothing flows back until its next announce, up to six
hours away for a stock lxmd (`announce_interval = 360`,
`reference/LXMF/LXMF/Utilities/lxmd.py:981`).
Class: semantic.
Price to close: persist the recalled `app_data` with the peer record
— the announce app-data observed on the rig is well under 100 B, so
~100 B per peer of flash (nil against the 64 KiB region) and 0 B of
heap; or accept the window, which the peer's own cadence bounds.
Test: the board seam (`docs/src/firmware/host-test-seams.md`) — a
restore-then-sync mvr asserting `PN_PEER action=add
reason=inbound_sync` without a fresh announce.
**Fixed in d2a9dcfb**: the board keeps the announce `app_data` of up to
16 propagation nodes inside its depth as `TAG_RECALL` records (0xA2) in
the PN log (`PnPeerStore::remember_announce`, `leviculum-nrf/pn-store`),
path responses included, since a node heard only that way is exactly
one a sync later peers back. Not in the peer record: a held peer is
restored at boot and never reaches the recall. A record is the 42 B
header plus the app data, refused above 255 B, so about 100-150 B a
node and at most 16 × 300 B of the region; 0 B of heap, the writes ride
the peer store's existing flush queue. Wear: a write only when the
announce says something different (timebase aside, it moves every
announce) or its copy has aged into the older half of the region, where
round-robin reclaim would take it within half a lap. The sync's recall
asks the RAM cache first and the region second
(`recall_for_inbound_sync`). Pinned by
`a_propagation_announce_is_recalled_after_a_reset` and, through the
seams in the engine's order (reset, link hops by 683, app data from
flash), `after_a_reset_a_syncing_neighbour_is_peered_from_its_recalled_announce`,
both red against the RAM-only recall; wear, slots and ageing by three
more tests beside them.

**D6 — lnpnd judges an inbound sync by its link's hops when no path
is known; the board does not.** lnpnd's deliberate deviation 556
(8756a5df, documented at `lnpnd/src/peering.rs:1121-1139`): the
reference reads only the path table, which answers `PATHFINDER_M`
for an unknown path, so after a restart a recallable neighbour is
"too deep" until its next announce; the link the sync arrived over
has just proven the distance, so lnpnd takes `Link::hops` as the
fallback. The board passed only `node.hops_to` (now the first
argument of `sender_depth`, `leviculum-nrf/src/pn.rs:2201`).
What a peer sees: on the board the case is mostly masked by D5 —
the RAM recall itself requires a live path — but a path dropped
between the announce and the sync reproduces it: declined
`too_deep` on the board, peered on lnpnd.
Class: semantic (board vs lnpnd; the board matches the reference).
Price to adopt: 0 B — the link is still up when
`on_inbound_resource` runs on the board's synchronous event path, so
its hop count is a read, not a new field.
Test: the lnpnd rule's test shape
(`lnpnd/src/engine.rs:2558-2565`) replayed through the board engine.
**Fixed in 00ac770b**: the board reads the link's hop count at the
resource advertisement, while the link certainly exists, into one slot
(`AdvertisedHops`, `leviculum-nrf/sync-batch`; one because D14 admits
one incoming resource at a time, asserted beside the import), and
`sender_depth` takes it when no path is known; a known path still
wins. The validated hash stays in `validated_links` until the
`LinkClosed`, the keys in the identity cache, so the identity needs no
slot. A link id and a byte in the engine, no heap. Pinned by
`a_pathless_node_syncing_over_a_link_is_judged_by_the_links_hops`
(`leviculum-nrf/sync-batch/src/lib.rs`), the lnpnd rows plus a link
closed before the decision, red against the path-only rule.

**D7 — the board mines peering keys up to cost 18 where the
reference and lnpnd accept 26.** `BOARD_REMOTE_PEERING_COST_MAX`,
`leviculum-nrf/src/pn.rs:414`, against `MAX_PEERING_COST`,
`reference/LXMF/LXMF/LXMRouter.py:51` and the shared default
(`REMOTE_PEERING_COST_MAX_DEFAULT`,
`leviculum-lxmf/src/peering.rs:75`).
What a peer sees: a node announcing peering cost 19-26 is declined
(`cost_too_high`) by the board and peered by everyone else. The
reference's own announced default is 18
(`PEERING_COST`, `reference/LXMF/LXMF/LXMRouter.py:50`), so stock
nodes sit exactly at the board's cap; only operators who raised the
knob fall outside it.
Class: deviation ours (deliberate, priced at the constant).
Price to match 26: no bytes; 45-134 *minutes* of the single core per
such peer, once — the whole radio's CPU for the duration, sliced or
not.
Test: none needed; the bound is a named constant with its reasoning.

**D8 — one key mine stalls all sync scheduling.** While a key
grinds, `tick_sync` returns before picking any peer
(`self.mining_peer.is_some()` in its early return until the fix
below, `leviculum-nrf/src/pn.rs:2786`), so
peers whose keys are ready wait out the mine — 10-32 s at cost 18.
The reference mines on a thread per peer and its scheduler keeps
picking others (`generate_peering_key` off the sync thread,
`reference/LXMF/LXMF/LXMPeer.py:285-286`); lnpnd likewise holds only
the mining peer (`MiningJob`, `lnpnd/src/peering.rs:93`).
What a peer sees: its due sync starts up to half a minute late, once
per newly met peer. Class: timing.
Price to close: 0 B (scheduling change: skip the mining peer instead
of returning); the single-mine constraint itself is the one-deep
channel (`MINE_REQ`, `leviculum-nrf/src/pn.rs:621`) and is right for
this core.
Test: board engine test — two peers, one mining, the other's round
still dials.
**Fixed in 00ac770b**, by skipping: the pick is
`PeerTable::next_dialable` (`leviculum-lxmf`), which serves every due
peer except those that would need the busy miner, and `start_round`
refuses a second job while one runs. No state: the mining peer is
already `KeyMining`, and `mining_peer` already says the miner is busy.
Pinned by `a_busy_miner_holds_only_the_peers_that_need_it`
(`leviculum-lxmf/src/peering_tests.rs`), red against the early return.
lnpnd's tick carries the same early return
(`lnpnd/src/peering.rs:1238`, `self.mining.is_some()`), so the
sentence above about lnpnd does not hold; not changed here.

## 2. The store, pricing, and what the board announces

Where the code is shared, verified: one accept path for uploads and
synced messages (`handle_upload`,
`leviculum-lxmf/src/propagation_node.rs:1001`, and `accept_stamped`,
`leviculum-lxmf/src/propagation_node.rs:1027`, against
`reference/LXMF/LXMF/LXMRouter.py:2234-2255` and `:2430-2436`);
dedup by store membership plus the processed-id cache
(`reference/LXMF/LXMF/LXMRouter.py:2496`); displacement by
`age × size` weight (`make_room`,
`leviculum-lxmf/src/propagation_node.rs:1364`, against `get_weight`,
`reference/LXMF/LXMF/LXMRouter.py:1056-1067`); 30-day expiry
(`MESSAGE_EXPIRY_SECS`,
`leviculum-lxmf/src/propagation_node.rs:52`), epoch-guarded on the
board so the first clock seed does not mass-expire a birth-stamped
store (`tick`, `leviculum-lxmf/src/propagation_node.rs:1332`); the
seven-field announce built by one encoder for both stacks
(`announce_app_data`, `leviculum-lxmf/src/propagation_node.rs:949`,
against `reference/LXMF/LXMF/LXMRouter.py:324-336`); and the `/get`
serve with purge-only-on-haves (`handle_get`,
`leviculum-lxmf/src/propagation_node.rs:1138`), bounded by the
client's limit, the board's serve limit and the live heap cap in
composition (`leviculum-lxmf/src/propagation_node.rs:1213-1224`) —
the serve side is audited to death in #388/#384 and its numbers live
at `SERVE_RESERVE_BYTES`
(`leviculum-nrf/heap-budget/src/lib.rs:200`) and `live_serve_cap`
(`leviculum-nrf/heap-budget/src/lib.rs:532`).

What the board accepts, by path: a raw link packet (bounded by the
link MDU, 431 B on LoRa), or a resource the advertisement gate
admits at up to the announced sync limit
(`accepts_resource_of`,
`leviculum-lxmf/src/propagation_node.rs:977`, wired through
`board_accepts_resource_of` at `leviculum-nrf/src/pn.rs:1269`). Storage is slice-at-a-time: the
record goes to the flash log through the work queue, one item per
settle pass, the stamp workblock expanded 20 rounds per pass
(`GRIND_SLICE_ROUNDS`, `leviculum-nrf/src/pn.rs:485`) so the LoRa
hand-off never goes dark behind a 3.7 s validation; the RAM is the
role's sync slice (8 000 B) plus two queued uploads (8 000 B),
priced in `ROLE_BUDGET_BYTES`.

**D9 — the store is 64 KiB of flash in ≤ 4 042 B records, against
lnpnd's 500 MB default.** The region is one linker number
(`memory.x:91`, 16 pages); a record never straddles a page
(`MAX_BODY`, `leviculum-nrf/record-log/src/lib.rs:229` = 4096 − 12 −
42), and the peer records share the region. lnpnd defaults to 500 MB
(`message_storage_limit`, `lnpnd/src/config.rs:357`); the reference
is unlimited until configured
(`reference/LXMF/LXMF/LXMRouter.py:139`).
What a peer sees: displacement evictions orders of magnitude
earlier — the region holds on the order of 150 field-sized messages
(the memory.x comment prices 11 per page), a pocket, not a village.
Nothing on the wire distinguishes it: eviction is silent in the
reference too.
Class: deviation ours (deliberate — §2's endurance design).
Price to grow: flash pages only, no heap — the scan is memory-mapped.
Each page added is ~11 more field records; the endurance math
(`memory.x:68-71`) has 16 pages at 9 years at the measured field
duty, and more pages only lengthen that. The cost is application
image room: the region is carved from the top of the application
window.
Test: none — one number in one file, asserted by the linker.

**D10 — everything a fresh board accepts before its first demanding
peer is priced 0 and never offered to a stock peer.** The board
skips the workblock at cost 0 while no peer demands a value
(`always_compute_stamp_value: false` in `Engine::new` as audited, now
`true` in `board_role_config`,
`leviculum-nrf/pn-store/src/lib.rs:390`; the gate at
`leviculum-lxmf/src/propagation_node.rs:1058-1069`), storing stamp
value 0 as the record's tag. A stock peer announces cost 16 with
flexibility 3 (`PROPAGATION_COST`,
`reference/LXMF/LXMF/LXMRouter.py:54`), so it takes only value ≥ 13;
the offering side drops lower-valued ids permanently —
`build_offer` steps the cursor past them
(`leviculum-lxmf/src/peering.rs:1638-1641`, the reference's own
never-retry at `reference/LXMF/LXMF/LXMPeer.py:340`). When a
demanding peer later joins, `set_compute_stamp_value(true)` (from
`on_pn_announce` as audited; the calls are gone with the fix) prices
new accepts only: the old
records keep tag 0 forever. The host closed both halves this week —
it always computes (8f470aa7, leviculum#442 D6,
`leviculum-lxmf/src/propagation_node.rs:399-406`) and re-prices an
old store once at start (645e3580, `reprice_stored_values`,
`lnpnd/src/validation.rs:106`, run at `lnpnd/src/main.rs:622-633`).
What a stock peer sees: a fresh board's whole early store never
syncs out of it. The messages are served to their *recipients* on
`/get` unharmed (the serve does not filter by value); what is lost
is propagation — the second half of the Leitstern, for exactly the
mailbox a board accumulates before it meets its first peer.
Class: semantic (wire-legal; the loss is in what is offered).
Price to close, both halves: **zero heap, zero stack** — the
validator streams (`WorkblockStream`), the grind is already sliced.
Computing at accept costs the 1000-round workblock per accepted
message, 3655-3727 ms of CPU spread over ~50 settle passes
(`PN_STAMP ms=`), i.e. upload-to-durable latency, not airtime — and
the proof already left at receipt (#397), so the phone's round trip
is untouched. Re-pricing an old store at boot costs the same CPU per
stored record *plus one re-append-and-purge per record*, because the
value is the record's tag (`leviculum-nrf/pn-store/src/lib.rs:693`)
and the log is append-only: a full store re-priced is roughly one
extra lap of the region's write pointer, ~150 appends and their
share of page erases, once.
Test: a board-config twin of
`a_message_stored_at_value_zero_is_offered_once_repriced`
(`lnpnd/src/validation.rs:322`).
**Fixed in 5c82832d**, both halves: `board_role_config` prices every
accept, and `RepriceWalk` prices an older store once at boot in one
lap over the existing sliced grind, re-appending each valuable value-0
record under its true tag, then a `TAG_PRICED` marker. Pinned by
`a_message_accepted_at_cost_0_before_any_demanding_peer_is_offered_to_one`
and `a_store_at_value_0_is_offered_once_the_boot_walk_priced_it`
(`leviculum-nrf/pn-store/src/tests.rs`), both red against the old
behaviour.

**D11 — a single-message upload of 4 043-5 000 B is admitted on the
wire and cannot be stored.** The advertisement gate admits a
resource up to the announced sync limit, 5 000 B (D12); the record
log refuses a body over `MAX_BODY` = 4 042 B
(`leviculum-nrf/pn-store/src/lib.rs:685-688`, surfacing as
`StorageError::Full` that no eviction can cure). By then the
resource protocol has already acknowledged the transfer, and the
store failure sends nothing — the reference has no signal for it
either (`UploadOutcome::StoreFailed` doc,
`leviculum-lxmf/src/propagation_node.rs:485-491`).
What a sender sees: nothing — the upload concluded; the message is
gone. The only trace is `PN_REJECT reason=store` beside
`durable=0`. The window is unreachable by conforming peers (their
packing stops strictly under the 4 000 B transfer limit + 16,
`reference/LXMF/LXMF/LXMPeer.py:370-376`) and by packet uploads
(MDU-bound); it takes a client that ignores the announced
per-transfer limit and ships one big message as a resource.
Class: semantic (a silent-loss window of our capacity shape).
Price of leaving: one nonconforming sender's message lost with a log
line. Price to narrow: 0 B — the sync-limit gate cannot see the
message count at advertisement time, but the judgement path can
refuse-and-teardown like the invalid-stamp arm
(`leviculum-nrf/src/pn.rs:3196-3215`) so at least the link's death
tells the sender something; shrinking the announced sync limit to
4 would close it entirely and cost legitimate two-message batches.
Test: an mvr uploading a 4 500 B single-message resource, asserting
`PN_REJECT reason=store` today and the chosen behaviour after.
**Fixed in 00ac770b**, at the gate: `board_accepts_resource_of`
(`leviculum-nrf/pn-store`) holds a resource from a link without a
validated peering key, a client upload and one message by
construction, to `MAX_UPLOAD_RESOURCE_BYTES` (`MAX_BODY` plus the
thinnest envelope, 4 048 B), and refuses past it with the RCL before
anything is acknowledged. The sync limit stays 5: at 4 it would break
the assertion that a message at the transfer limit is syncable, and a
stock peer would skip such a message toward the board forever. A peer
packing one message above the transfer limit it was told is the case
left. Pinned by `a_client_upload_the_gate_admits_is_one_the_store_holds`
(`leviculum-nrf/pn-store/src/tests.rs`), red at 4 500 B against the
old gate.

**D12 — the board announces transfer 4 KB and sync 5 KB where a
stock node announces 256 and 10 240 KB.** Board: fields 3 and 4 from
`BOARD_TRANSFER_LIMIT_KB`
(`leviculum-nrf/heap-budget/src/lib.rs:126`) and the derived
`BOARD_SYNC_LIMIT_KB` (`leviculum-nrf/heap-budget/src/lib.rs:137`,
derivation `derive_sync_limit_kb`,
`leviculum-nrf/heap-budget/src/lib.rs:611`, pinned at 5 by
`leviculum-nrf/heap-budget/tests/budget.rs:64`); lnpnd announces the
crate defaults 4 and 32; reference `PROPAGATION_LIMIT` 256 and
`SYNC_LIMIT` 256 × 40 (`reference/LXMF/LXMF/LXMRouter.py:55` and
`:59`).
What a peer sees: a stock peer packs rounds of at most 5 000 B
toward the board and marks any message over ~3 984 B handled-forever
toward it (`reference/LXMF/LXMF/LXMPeer.py:370-373`) — attachments
do not travel *through* a board. A phone uploading to the board as
its outbound node is bounded the same way by its own reading of
field 3. The 4 is also the flash-page bound (D9/D11): raising it
without changing the record layout is not possible.
Class: deviation ours (deliberate, derived — the board announces
only what it can also answer, order 511).
Price to raise transfer to 8 KB: +8 000 B in `ROLE_BUDGET_BYTES`
(two queued uploads) — three of the four endpoint links — plus a
record format that spans pages (a design change, not a constant).
Test: the derivation is pinned
(`leviculum-nrf/heap-budget/tests/peaks.rs:758-762`).

**D13 — the board announces every 5 minutes moving and hourly at
rest; a stock node announces every 6 hours.** Board:
`MOVING_ANNOUNCE_INTERVAL_MS`
(`leviculum-nrf/announce-policy/src/cadence.rs:50`) and
`STILL_ANNOUNCE_INTERVAL_MS`
(`leviculum-nrf/announce-policy/src/cadence.rs:60`), first
announce 20 s after boot (`PN_ANNOUNCE_INITIAL_DELAY_MS`,
`leviculum-nrf/announce-policy/src/cadence.rs:460`, the reference's
`NODE_ANNOUNCE_DELAY`, `reference/LXMF/LXMF/LXMRouter.py:41`);
stock: `announce_interval = 360` minutes
(`reference/LXMF/LXMF/Utilities/lxmd.py:981`). Both withheld while
the store is unmounted (`leviculum-nrf/src/pn.rs:2484-2500`); the
propagation slot is deliberately not clock-gated
(`leviculum-nrf/src/pn.rs:2502-2517`).
What a peer sees: a mobile board's peers refresh `last_heard` often
and re-schedule syncs on every announce heard; the airtime is the
cost, bounded by the duty budget the cadence crate carries.
Class: deviation ours (deliberate, #401's rule). Price: airtime
only — one announce frame per interval per carrier; no heap.
Test: cadence rules are pinned in
`leviculum-nrf/announce-policy/src/cadence.rs` tests.

## 3. Sync rounds

Verified shared behaviour: the scheduler pass every 24 s
(`SYNC_INTERVAL_SECS`, `leviculum-lxmf/src/peering.rs:119`, the
reference's 6 × 4 s), one outbound round in flight, deterministic
round-robin instead of the reference's random-from-fastest
(`next_due`, `leviculum-lxmf/src/peering.rs:1272` — a shared,
documented selection deviation with nothing on the wire); the offer
planned against the live link's MDU
(`offer_budget_for_mdu`, `leviculum-lxmf/src/peering.rs:178`, ten
ids on a LoRa link); the cursor replacing the reference's per-peer
sets, with its documented offer-order deviation
(`build_offer`, `leviculum-lxmf/src/peering.rs:1620`); offer
responses, unpeer-on-`NO_ACCESS`, throttle backoff and key re-mining
all through the shared `offer_reply`
(`leviculum-lxmf/src/peering.rs:1753`, against
`reference/LXMF/LXMF/LXMPeer.py:400-452`); and the inbound drain
through the same accept path as uploads, one message per settle
pass, batch outliving its link exactly as the reference's sender
tears down on conclusion (`reference/LXMF/LXMF/LXMPeer.py:499-503`;
the board rule is `leviculum-sync-batch`'s `after_close`).

The #388 shape — two phones and a peer at once — as the code stands:
phone A's and B's upload resources and the peer's sync resource can
all be in flight together (each on its own link, one incoming
resource per link, `BOARD_INCOMING_RESOURCES_PER_LINK`,
`leviculum-nrf/heap-budget/src/lib.rs:58`). The envelope decoded,
the peer's batch takes the sync slice (≤ 5 000 B inside the 8 000 B
`SYNC_SLICE_BYTES`), the uploads queue as work items (2 × ≤ 4 000 B,
funded). The engine then drains strictly serially: one settle pass
processes one work item or one batch message, each stamp 50 slices
of 20 rounds, ~3.7 s per message of CPU interleaved with the loop's
channels. New `/offer`s during the drain are answered
`ERROR_THROTTLED` — the batch gate sits at
`leviculum-nrf/src/pn.rs:3350` — the reference's
own postponement (`reference/LXMF/LXMF/LXMRouter.py:2274-2278`), on
which a stock peer waits 180 s. The phones' proofs left at receipt
(#397), so their round trips do not sit behind the grind. What is
NOT funded is the transfers themselves beyond the first — the next
divergence.

**D14 — the gate admits up to four concurrent incoming resources;
the budget funds one.** The reserve funds one incoming resource at
the 8 KiB cap (`budget_reserve`,
`leviculum-nrf/heap-budget/src/lib.rs:370`); the gate admits up to
`max_inbound_syncs` = 3 peer transfers
(`MAX_INBOUND_SYNCS_DEFAULT`, `leviculum-lxmf/src/peering.rs:80`,
the reference's `reference/LXMF/LXMF/LXMRouter.py:58`) and client
upload resources are not counted against it at all (only validated
peer links enter `inbound_transfers`,
`leviculum-nrf/src/pn.rs:1282-1283`). Worst case, one resource per
endpoint link: 4 × 8 192 B in flight, 24 576 B of it unfunded — the
3 × 8 192 B hole #661 named, and the one row of the heap-budget
page's "does not sum" table that can kill (the live serve cap and
the hold are already held to the live heap; this is not).
What a peer sees when it bites: an allocation failure mid-transfer —
in the field's shape, links dying under load, the #388 crash class.
Class: semantic (a capacity promise the gate makes and the plan does
not keep).
Price to fund: 24 576 B — not purchasable; the heap has 56 B.
Price to close by admission instead: 0 B — count every accepted
resource (client uploads included) in one budget and refuse the
advertisement past what the reserve funds, the same move #657 made
for the second resource on one link. Cost: a second phone's
resource upload is refused while a sync transfer runs; the client
retries, a peer waits its throttle — latency, not loss.
Test: an mvr advertising three resources on three links, asserting
today's admit and the chosen cap after; the heap model halves exist
(`inbound_resource_peak`, `leviculum-nrf/heap-budget/src/peak.rs`).
**Fixed in 5c82832d**, by admission: `admits_incoming_resource`
(`leviculum-nrf/heap-budget`) takes an advertisement only while fewer
than `INCOMING_RESOURCES_FUNDED` (one) incoming resources are in
flight over every link, client uploads included; the rest get the RCL.
The reserve term is written over the same constant and did not move.
Pinned by `four_links_advertising_at_once_get_what_the_reserve_funds`
(`leviculum-nrf/heap-budget/tests/budget.rs`), red against the old gate.

**D15 — an idle inbound link lives ~12 minutes on the board, 3 on
lnpnd and the reference.** lnpnd tears down a propagation link
without payload after `PROPAGATION_LINK_MAX_INACTIVITY_SECS` = 180 s
(`leviculum-lxmf/src/constants.rs:30`, applied `close_idle_links`,
`lnpnd/src/engine.rs:954`, the reference's
`reference/LXMF/LXMF/LXMRouter.py:968-974`). The board has no such
pass; its inbound links fall to the core's generic stale rule,
keepalive 360 s × 2 + 5 s grace
(`leviculum-core/src/constants.rs:255-261`).
What a phone sees: nothing directly. What the *next* phone sees: the
board holds exactly four links
(`max_endpoint_links`, `leviculum-nrf/heap-budget/src/lib.rs:394`),
so a client that walked away without closing pins a slot ~9 minutes
longer than it would on lnpnd, and the fifth connect in that window
is refused (`LINK_REFUSED`, retried after the initiator's
establishment timeout).
Class: timing (with a slot-scarcity consequence the host never
feels).
Price to close: 0 B of heap — a settle-pass walk over the role's
links reading `no_data_for_ms`, the lnpnd loop transplanted; a few
hundred bytes of flash.
Test: lnpnd's
`an_idle_inbound_link_is_torn_down_after_the_reference_bound`
(`lnpnd/src/engine.rs:3042`) through the board engine.
**Fixed in 5c82832d**: one rule for both stacks, `link_idle_past_bound`
(`leviculum-lxmf`), used by lnpnd's `close_idle_links` and by the
board's sweep in `settle` (`tick_idle_links`, at most 15 s late on a
quiet board). Pinned by
`an_idle_inbound_link_is_let_go_strictly_above_the_reference_bound`;
the board engine itself has no host target.

**D16 — the board yields its own sync traffic to a duty-share
budget; lnpnd never does.** Every site where the board originates
propagation traffic asks the rolling-hour airtime share first
(`own_traffic_go`, `leviculum-nrf/src/pn.rs:2742`, leviculum#494):
a round is not dialled, a transfer not started, an offer answered
`ERROR_THROTTLED` past the share, each logged `PN_YIELD` and the
booked backoff handed back (`release_yielded_round`,
`leviculum-lxmf/src/peering.rs:455`).
What a peer sees: the reference's own throttle vocabulary at times
the reference would have sent — postponement, never loss.
Class: deviation ours (deliberate, and correctly placed: the budget
is the LoRa carrier's, read from the interface's counters, the
interface-isolation rule's shape).
Price: this one is paid in the other direction — it is the board
*spending less* airtime; the cost is sync latency under a busy hour.
Test: covered by `leviculum-queue-budget`'s own tests.

## 4. The control plane

What the board serves on its propagation destination: `/get` (list,
fetch, purge-on-haves) and `/offer`, registered at
`leviculum-nrf/src/pn.rs:858-867` — a phone and a stock lxmd client
get full mailbox service, with the serve bounded by the live heap
(`SERVE_CAP`, `read_serve_cap`, `leviculum-nrf/src/pn.rs:3101`) and
shortfalls re-listed on the client's next round.

**D17 — no `lxmf.propagation.control` destination on the board.**
lnpnd registers the reference's control destination with
`/pn/get/stats`, `/pn/peer/sync`, `/pn/peer/unpeer` behind the
identity allow list (`register_control_handlers`,
`lnpnd/src/engine.rs:635-638`, the codec in
`leviculum-lxmf/src/control.rs:35-39`, against
`reference/LXMF/LXMF/LXMRouter.py:672-676` and `:89-91`). The board
registers nothing of it.
What an operator sees: `lxmd --status`/`--peers`/`--sync`/`--break
--remote <board>` dies on "path not found" — the destination is
never announced because it does not exist. The board's substitutes
are local: the serial `PN_CONFIG` frame for costs
(`apply_config`, `leviculum-nrf/src/pn.rs:337`, next-boot
semantics), the host-commanded announce
(`announce_now`, `leviculum-nrf/src/pn.rs:2560`), the `PN_STATS`
log line every 300 s (`tick_stats`,
`leviculum-nrf/src/pn.rs:2713`) and the display's store-fill field.
There is no remote peer list, no remote sync trigger, no remote
unpeer, and no way to read any of it over the mesh.
Class: semantic (a whole reference surface absent).
Price to add: the codec is already `no_std` and in the image's
dependency tree, so flash is the handler code (~2-4 KiB); heap is
one more registered destination plus the stats response transient —
order 2.5 KiB at a 16-peer table if buffered, near zero if built on
the streamed-response path the fetch already uses; the allow list is
16 B per admitted identity. The honest blocker is none of these: it
is that the role budget's structural term has no slack and every
new resident must name its bytes.
Test: drop-in — the same `lxmd --status --remote` the lnpnd
conformance cells already run, pointed at a board.

**D18 — no client allow/ignore lists on the board.** lnpnd carries
the reference's `auth_required`/`allowed` and `ignored` lists
(`lnpnd/src/main.rs:642-657`); on the board every identified client
may `/get` its mailbox and anyone may upload.
What an operator sees: a board mailbox cannot be restricted to a
user set. Class: semantic. Price: 16 B flash per hash plus a lookup
on `/get`; 0 heap if read from the config page.
Test: board twin of lnpnd's auth tests once the surface exists.
**Fixed in 0e02ccde**, both lists, as `TAG_AUTH` (0xA4) and
`TAG_CONTROL` (0xA5) records of the PN log, 16 B per hash, at most 16
each, written by `lnflash --auth-allowed` / `--control-allowed` (and
`--clear-*`) like the static peers. The engine reads the auth list at
boot; a `/get` from an identity off a non-empty list is answered
`ERROR_NO_ACCESS` (`board_get_mailbox`, `leviculum-nrf/pn-store`), the
answer lnpnd gives from its `auth_allowed` check before `handle_get`,
and an empty list is open as before. Heap: 16 B per listed identity, 0 B
unset. The control list is stored and counted (`PN_LISTS
control_allowed=`) but not held: the board has no control destination
to consult it until D17. Uploads stay open, as on lnpnd. Pinned by
`an_unlisted_client_is_refused_its_mailbox_and_a_listed_one_served` and
`each_list_is_its_own_record_and_an_over_long_one_is_refused`.

## 5. Restart

What a board recalls across a reset, verified at the code: the peer
table with identity hashes, public keys, mined peering keys, limits,
costs, timebase, `last_heard` and the cursor (`PeerRecord` via
`TAG_PEER` records, `leviculum-nrf/pn-store/src/lib.rs:962`,
restored at `leviculum-nrf/src/pn.rs:908-910`); the message store
(a flash scan); the announced costs
(`load_config_at_boot`, `leviculum-nrf/src/pn.rs:315`). Backoff and
transport phase start fresh (`into_peer`,
`leviculum-lxmf/src/peering.rs:616`), as the reference's restore
also drops link state.

What it forgets, and who notices: the processed-id duplicate cache
is RAM-only on *both* our stacks where the reference persists it —
the shared, documented deviation
(`leviculum-lxmf/src/propagation_node.rs:824-837`): a client
re-uploading an already-drained message after a reset is re-stored,
which its own `has_message` filter tolerates. The announce recall
cache (D5). The traffic counters (D3's input; lnpnd persists them
with the record, the board's codec zeroes them,
`leviculum-nrf/pn-store/src/lib.rs:949-950`).

How long until it is a mailbox again: the store mounts in the boot
path; the propagation announce fires 20 s after boot
(`PN_ANNOUNCE_INITIAL_DELAY_MS`) — withheld only if the store did
not mount, not for a missing clock. How long until it syncs again:
the first settle pass — `next_sync_at_ms` starts at 0 — picks any
restored peer with records above its persisted cursor; the peering
key is persisted, so no re-mine. In practice a board with a backlog
dials its first round seconds after boot and the scheduler then
paces at 24 s per pass. The gap is inbound (D5): nodes that knew
the board resume toward it on their own cadence, but a node the
board cannot recall is answered `NotANode` until its next announce.

## 6. The price table, the three to fix first, and the kilobytes

Heap prices name the term of `budget_total` they move; "0 B" means
code and flash only. Airtime "—" means no change on the air.

| # | Gap | Heap (term) | Flash | Airtime | Left as is, the user sees |
|---|---|---|---|---|---|
| D1 | table cap 16 | ~1 KiB (`ROLE_STRUCTURAL_BYTES`, slice assert moves) | +104 B/peer | — | 17th PN in depth never syncs with the board |
| D2 | depth 4 vs fielded 6 | 0 B | — | more rounds | 5-6-hop stock nodes get one-way flow |
| D3 | no rotation, no counters | 128-512 B (`ROLE_STRUCTURAL_BYTES`) | +26 B/record | — | full table is first-come-forever |
| D4 | no static peers | 0 B | 16 B/peer config | — | board cannot be pinned to a home PN |
| D5 | recall dies at reset | 0 B | ~100 B/peer | — | post-reset one-way syncs, up to 6 h |
| D6 | no link-hops fallback | 0 B | small | — | live sync declined `too_deep` after path loss |
| D7 | peering cost cap 18 | 0 B | — | — | cost 19-26 nodes never peered (CPU: 45-134 min each if raised) |
| D8 | mine stalls scheduler | 0 B | small | — | other peers' syncs late by 10-32 s |
| D9 | 64 KiB store | 0 B | pages (11 msgs each) | — | early displacement evictions |
| D10 | value-0 pricing | **0 B** | reprice: ~1 region lap once | — | fresh board's store never propagates to stock peers (CPU 3.7 s/msg) |
| D11 | 4 043-5 000 B loss window | 0 B | — | — | nonconforming big upload silently lost |
| D12 | limits 4/5 vs 256/10 240 | +8 000 B for 8 KB (3 links) | record redesign | — | attachments never travel via boards |
| D13 | announce 5 min/1 h vs 6 h | 0 B | — | 1 frame/interval | (board pays, peers benefit) |
| D14 | 4 resources admitted, 1 funded | 24 576 B to fund — or **0 B** to cap admission | small | — | #388-class allocation death under 2 phones + peer |
| D15 | idle links 725 s vs 180 s | 0 B | small | — | dead client pins 1 of 4 slots ~9 min longer |
| D16 | duty-share yields (board-only) | — | — | spends less | sync latency in busy hours |
| D17 | no control destination | ~0.6 KiB + response transient | 2-4 KiB | — | no remote status/sync/unpeer at all |
| D18 | no client auth lists | ~0 B | 16 B/hash | — | mailbox cannot be restricted |

**The three to fix first.**

1. **D10, both halves on the board.** It is the only gap that
   silently defeats the Leitstern's second half — a board that
   *carries* messages but never *forwards* what it accepted while
   alone — and its price is the best on the table: zero bytes, the
   grind already sliced, the latency hidden behind the #397 proof.
   The boot re-price is the same one-shot lnpnd just landed, paid in
   one lap of a 16-page log.
2. **D14, closed by admission, not by funding.** It is the one row
   that ends in an allocation death rather than a delay, it is
   exactly #388's two-phones-plus-peer shape, and the 0 B form (one
   budget over all incoming resources, refuse the advertisement past
   the funded one) is the same move #657 already made within one
   link.
3. **D15, the 180 s teardown.** Four link slots are the board's
   scarcest resource after the 56 B; nine minutes of a dead phone's
   link is a ninth of an hour in which a real phone bounces off
   `LINK_REFUSED`. The fix is a transplanted loop with no new state.

**Where 1 KiB, 4 KiB and 16 KiB would come from** (the paragraph
#661 asked for; the menu is
`docs/src/firmware/heap-budget.md` §"Where a kilobyte could come
from", prices re-stated here unchanged). **1 KiB**: the
fragmentation reserve 6 → 5 KiB buys 1 024 B of slack and changes no
code path; the cost is allocator elbow room on a heap that once
refused 340 B with 4 760 B free. **4 KiB**: that plus
`known_identities` 8 → 6 (1 066 B; an evicted identity waits its
owner's next announce, 300 s for a phone), `path_request_tag_set`
32 → 8 (1 062 B; a four-times-shorter dedup window under path-request
floods), and `announce_rate_table` 32 → 16 (968 B) — 4 120 B, every
piece a named regression, none a crash risk. **16 KiB**: no
combination of trims reaches it. The sources are one endpoint link
with its BLE session (6 644 B — a phone the board can no longer
hold) and growing `HEAP_SIZE` into the flip-link stack region. The
recorded stack watermarks say the region looks rich — peaks 26 412 B
and 27 692 B of ~95 KiB on the T114, 15 368 B on the Pocket, worst
ever recorded 32 184 B (`rig-run/ble-drop/feld-t114.log`,
`t114-boot.log`, `feld-pocket.log`; the table is in
`docs/src/firmware/heap-budget.md`) — but #50 measured a 107 KiB
peak by the same paint on 2026-07-02 and nothing on record says what
removed the difference. So a heap grown into the stack region needs
a fresh painted capture FIRST, under the load this audit is about,
and the capture must show: the `[STACK]` watermark on a T114 and a
Pocket across a run that holds, concurrently, two phone uploads with
stamp grinds in progress, an inbound sync batch draining, one
streamed `/get` serve cutting parts, LoRa TX/RX live and all four
BLE links claimed — plus one boot cycle, because boot has its own
stack shape — with `peak_used` staying under the shrunken region
(95 024 − 16 384 B) by at least the margin between the field peaks
and the worst recorded one (~6 KiB). A capture that cannot drive all
of that at once does not answer the question.

## Wrong claims found in our own comments

1. **`leviculum-nrf/heap-budget/src/lib.rs:32` opened "Heap size in
   bytes (64 KiB)" over a constant that is 96 KiB.** The doc
   comment's narrative (the Bug #32-era bump down to 64 KiB) was
   true of an earlier value; `HEAP_SIZE` is `96 * 1024`
   (`leviculum-nrf/heap-budget/src/lib.rs:49`) and every budget
   number in this audit, the boot assertion and the heap-budget page
   say 96 KiB. The headline and the history paragraph describe a
   configuration the tree no longer builds.
2. **`leviculum-lxmf/src/peering.rs:56-57` derived the board's cap
   from "80·N + 6144 ≤ 8192 … N ≤ 25".** The design's 80 B record
   grew to 104 B when the identity hash and the announce facts
   joined it; the authoritative derivation is the compile assert
   (`leviculum-nrf/heap-budget/src/lib.rs:330-337`, N ≤ 19) and the
   concept page says so
   (`docs/src/concepts/propagation-node-on-a-board.md:935`). The
   cap 16 itself is right; the stated arithmetic beside it is the
   stale draft.

Both comments fixed in 5c82832d.

## Divergence count by class

- wire: **0** — nothing found that changes bytes a peer parses; the
  board's announce, offers, envelopes and errors are the shared
  encoders.
- semantic: **10** (D2, D3, D4, D5, D6, D10, D11, D14, D17, D18).
- timing: **2** (D8, D15).
- deviation ours (deliberate): **6** (D1, D7, D9, D12, D13, D16).
- inert: **0** counted; the shared scheduler/selection deviations
  (round-robin, backoff clearing, cursor ordering) are documented at
  their constants and belong to both stacks equally, so they are not
  board divergences and are not numbered here.
