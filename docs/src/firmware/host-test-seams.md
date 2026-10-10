# Firmware decisions only the rig can test

This page is the tracker for leviculum#413: the decisions the board
firmware takes that no host test reaches, each with the cheapest rig cell
that exercises it, and the order in which they move into a host-built
crate. The policy behind it is [The firmware's host-test
seam](../concepts/firmware-host-test-seam.md); this is its inventory. The
next order that moves a decision picks from the table below and updates
its row.

## How the census was taken

Every file under `leviculum-nrf/src` was walked on 2026-10-10 at
`ad7eaa2e`, except the two instrument binaries `bin/sd-ram-probe.rs` and
`bin/qspi-selftest.rs`. A **decision** is a branch whose outcome can be
seen on the air or on the debug port: what to send, when, to whom, what to
drop, what to persist. Rendering, HAL and register glue, and branches that
only choose whether a log line is printed are not decisions.

A decision counts as **host-reached** when its condition is a call into a
crate that builds and tests on the host: `leviculum-core`,
`leviculum-lxmf`, or one of the pure members of the `leviculum-nrf`
workspace that `just lint-nrf` tests on the host triple. Those are counted
per section and not listed. A decision whose condition is written inline
is listed, even when parts of it call a host crate. A decision the three
board binaries duplicate is listed once, at its `bin/t114.rs` line.

**580 decisions: 168 already host-reached, 412 not.** Of those
412, 147 are reached by no rig cell at all. 6 rows, the three
decisions below, moved to the host with this page and carry their host
tests in the last column; 8 more, the outbound round's failure arms and
the unmounted-store announce, followed in the next wave (leviculum#413,
[below](#the-outbound-rounds-failure-arms-and-the-store-gates)).

## The rig-cost measure

For each row, the cheapest cell under `periculum/hardware/` that drives
the branch. **s** is the cell's own budget, the first `timeout_secs` in
its file. **radios/boards** counts its `[nodes.*]` tables and, of those,
the ones with `serial = true`, the LNodes the firmware runs on; `(>2)`
marks a cell that needs more than two radios on the bench. A cell was
mapped to a row by the needles it asserts and the subsystem it runs, so
the column says which cell exercises the branch, not that the cell goes
red on every regression in it. **none** means no cell drives the branch:
it is a failure, an edge or a configuration no cell induces.

| Cell | s | radios/boards | What it drives here |
|---|---|---|---|
| `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 | the board's LoRa transmit and receive path |
| `lora_path_discovery_fast_mixed` | 90 | 2/1 | the USB control envelope the harness configures a board with |
| `lora_lnode_split` | 240 | 2/1 | split frames |
| `boot_smoke_t114`, `boot_smoke_solarnode` | 300 | 1/1 | boot, sensors, the QSPI part on the Solar Node |
| `reset_loop_quiet` | 300 | 1/1 | commanded resets and what survives them |
| `bench_dual_pair_fast` | 360 | 4/2 (>2) | channel contention: CAD busy, bursts, empty acks |
| `ble_mesh_formation` | 600 | 3/2 (>2) | BLE links, handshake, admission |
| `ble_unidentified_central_is_dropped` | 600 | 1/1 | the pre-handshake refusal |
| `lnode_remote_status` | 900 | 2/1 | `/status` and the management allow-list |
| `lora_duty_lock_lawful_default` | 900 | 2/1 | a duty hold opening |
| `ble_room_4` | 900 | 4/4 (>2) | duplicate identities, displacement, refusal |
| `ble_reconnect` | 1200 | 3/2 (>2) | link ends, churn, rescans |
| `ble_pn_board_upload` | 1800 | 3/2 (>2) | the propagation role, uploads and its announce |
| `lora_pn_board_sync` | 1800 | 4/2 (>2) | peering, sync rounds, active delivery, `/get` |
| `board_delivery_promise` | 1800 | 3/1 (>2) | the telemetry target and the delivery destination |
| `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) | sync rounds past one link's offer, the live serve cap |

## The three dearest, and where they went

No cell outranks "none": a branch no cell drives costs a cell that does
not exist yet. Within "none" the rows are ordered by the cell their
topology would need, and that puts the propagation-node rows (a
two-board, four-radio cell of 1800 s, plus an induced failure) and the
duty-hold lift (a cell that runs past a whole rolling hour) above the BLE
and LoRa rows, whose cells run 90 to 900 s.

1. **The duty-hold lift edge.** A hold lifts once, on a `Key` verdict or
   on an idle turn with the lock disengaged, and reports how long it held
   and how many frames it aged out. No cell sees a lift:
   `lora_duty_lock_lawful_default` says why in its header ("WHY THE LIFT
   IS NOT HERE"), a fresh ledger frees nothing for an hour, and the two
   `duty_held_link_request_relay` cells (5000 s and 5700 s, the dearest
   in the corpus) stop short of it too. Was the bodies of `start_duty_hold`
   and `lift_duty_hold` and the idle-turn condition in `lora_task`; now
   `HoldEdge` in `leviculum-queue-budget`, beside the `hold_verdict` and
   `DutyHolds` it completes. Interface-side by the isolation rule, so not
   in `leviculum-core`.
2. **The sync backoff booking.** Every round books one more
   `SYNC_BACKOFF_STEP_SECS` before it dials, so an unreachable peer is
   retried 12, 24, 36 minutes apart. No cell holds a peer unreachable.
   Was two lines in `start_round`; now `Peer::book_sync_attempt` in
   `leviculum-lxmf/src/peering.rs`, beside `note_link_established` and
   `release_yielded_round`, the two host-tested ways a booking is taken
   back, which until now were tested against a booking the tests wrote
   by hand.
3. **Active delivery: the give-up and the pick.** A failed push resends
   until its third failure and then gives up (no cell makes a push
   fail); the pick names the oldest stored message for the destination
   that is not known to be too large (`lora_pn_board_sync`, 1800 s on four
   radios, the only cell with a `PN_DELIVER` needle). Was the
   `DeliveryFailed` arm and the scan in `trigger_active`; now
   `leviculum_lxmf::active_delivery` (`ActiveCandidate`,
   `after_failed_push`).

Within the propagation-node "none" tier, which holds about thirty rows of
equal measure, these two were taken because each completes a seam whose
other half was already host-tested. The firmware calls the moved code
with the inputs it used inline; `just lint-nrf` builds all three boards
against it, and no type the heap census pins changed size.

## The outbound round's failure arms, and the store gates

The next wave took the rest of that tier, grouped by the state machine
each row belongs to. Of the 24 propagation-node rows marked "none", 2
were already moved above (17, 59); the other 22 fall into four groups:

| Group | Rows | Count | Moved |
|---|---|---|---|
| outbound round | 10, 15, 22, 23, 24, 53, 60, 61 | 8 | 7 |
| store mount | 49, 50, 51 | 3 | 1 |
| store errors | 30, 43, 56, 81 | 4 | 0 |
| elsewhere | 6, 38, 45, 48, 68, 71, 77 | 7 | 0 |

**Outbound round.** What the round does next on a link-up that cannot
identify, a peer without a peering key, a link gone before the offer is
sized, its own `/offer` timing out, its deadline passing, or a dial the
link cap refuses is now one step function, `round_step` in
`leviculum-lxmf/src/sync_round.rs`, with the deadline it reads
(`round_deadline_ms`, `OUTBOUND_ROUND_DEADLINE_MS`). The firmware hands
it the event and acts on the step: log the end, peer Idle, close the
link. The tests drive a real `Peer` booked the way `start_round` books
it, so each arm also pins what it leaves for the next pass: an identify
that fails or a deadline before link-up leaves the peer deferred by its
whole accumulator, an end after link-up leaves it due. Row 15, the far
side closing the round's link, stays: it drops the round without a
`PN_SYNC` line and decides nothing the step would add.

**Store mount.** Row 49 is a decision: a due announce is pushed one retry
out and spends nothing, so the store mounting inside the retry announces
at its end. It is now `AnnounceCadence::withhold` in
`leviculum-announce-policy`, beside the `defer` it calls; the log-once
latch beside it only chooses whether a line is printed. Rows 50 and 51
are early returns: one guard and a log line, no state.

**Store errors.** Not moved, all four early returns. 30 ends the round
on the only result a failed batch plan has; 43 is one length check on a
read body; 56 is the silent return the observation above already names
as a candidate bug, a behaviour question, not a seam; 81 forgets an id on
a failed append, the pairing of `PnStore::op_done` with
`PropagationNode::forget_processed`, a two-crate seam for its own order.

## Observations made during the walk

Read from the code, not measured on a board; each is a candidate for its
own issue, not a finding of this page.

- A miner whose `generate` fails sends no result, so `mining_peer` is never
  cleared and every later sync pass skips (`leviculum-nrf/src/pn.rs:644`).
- A store error in `start_round` returns before any backoff is booked, so
  the peer stays due on every pass (`leviculum-nrf/src/pn.rs:2618`).
- The offerable-entries scan is written twice, once where the link comes
  up and once where the round starts: `leviculum-nrf/src/pn.rs:1561`
  and `leviculum-nrf/src/pn.rs:2618`.
- A full `BLE_INCOMING` drops a completed packet with no line and no
  counter, on both GATT roles (`leviculum-nrf/src/ble/columba.rs:975`,
  `leviculum-nrf/src/ble/columba.rs:2718`).
- The central disconnects on any failed fragment, the first included,
  where the peripheral disconnects only on a torn packet
  (`leviculum-nrf/src/ble/columba.rs:2816`,
  `leviculum-nrf/src/ble/notify.rs:127`).

## The census

### Propagation node (`pn.rs`)

81 decisions no host test reached on 2026-10-10; 29 more were already a call into a host-built crate and are not listed.

| # | Where | What it decides | State it reads | Cheapest rig cell | s | radios/boards | Host test |
|---|---|---|---|---|---|---|---|
| 1 | `leviculum-nrf/src/pn.rs:308` | persisted PN costs or the compiled defaults | load_pn_config result | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 2 | `leviculum-nrf/src/pn.rs:331` | merge a `--stamp-cost` frame: KEEP field keeps the configured value, else the wire value; persisted | wire frame, CONFIGURED_* atomics | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 3 | `leviculum-nrf/src/pn.rs:884` | walk the workblock at cost 0 only when some peer demands a value | max_peer_min_cost | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 4 | `leviculum-nrf/src/pn.rs:988` | loop wake deadline: now on a parked grind, +25 ms with work pending, else the earliest of announce/maintenance/stats/sync/outbound | grind, work, sync_batch, pending_ops, next_* | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 5 | `leviculum-nrf/src/pn.rs:1026` | seed the wall clock only when implausible and the input is non-zero | has_plausible_wall_clock, unix_secs | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 6 | `leviculum-nrf/src/pn.rs:1057` | after a clock jump refresh and persist every peer heard at or before the old time | peer table | none | - | - |  |
| 7 | `leviculum-nrf/src/pn.rs:1107` | a PN announce with our name hash and not our own goes to peering | announce name_hash, destination | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 8 | `leviculum-nrf/src/pn.rs:1120` | any other announce, or PathFound, triggers active delivery | event kind | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 9 | `leviculum-nrf/src/pn.rs:1130` | an inbound link to our destination gets ResourceStrategy::AcceptApp | is_initiator, destination | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 10 | `leviculum-nrf/src/pn.rs:1160` | a request timeout on the outbound round ends it (`offer_timeout`) and closes the link | outbound link_id/request_id | none | - | - | `sync_round::tests::only_the_rounds_own_offer_timing_out_ends_it` |
| 11 | `leviculum-nrf/src/pn.rs:1189` | every link packet on our destination is queued as an upload | owns_link | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 12 | `leviculum-nrf/src/pn.rs:1202` | an accepted resource counts as inbound transfer only on a validated peer link (feeds the `/offer` gate) | validated_links | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 13 | `leviculum-nrf/src/pn.rs:1220` | outbound resource done only on the last segment; a failure ends the round | segment_index, total_segments | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 14 | `leviculum-nrf/src/pn.rs:1236` | route a request on our destination by path to `/get` or `/offer` work | destination, path | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 15 | `leviculum-nrf/src/pn.rs:1299` | outbound link closed by the far side: drop the round, peer Idle, no PN_SYNC line | outbound link_id | none | - | - |  |
| 16 | `leviculum-nrf/src/pn.rs:1311` | delivery proof for the active push: purge from flash, clear, set the 1 s gap | active.packet_hash | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 17 | `leviculum-nrf/src/pn.rs:1326` | failed push: decrement attempts, give up at 0, else resend | active.attempts_left | none | - | - | `active_delivery::tests::a_failed_push_resends_until_the_last_attempt_and_then_gives_up` |
| 18 | `leviculum-nrf/src/pn.rs:1365` | a link is ours when its destination is our PN destination (guards the upload/request arms) | node.link() | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 19 | `leviculum-nrf/src/pn.rs:1413` | capture the identity keys into a peer that lacks them | peer keys, storage | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 20 | `leviculum-nrf/src/pn.rs:1420` | PeerChange mapping: Added/Updated persist and sync now, Dropped removes from flash, Declined logs on TableFull | PeerChange | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 21 | `leviculum-nrf/src/pn.rs:1502` | act on link-up only for the outbound round's link | outbound link_id | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 22 | `leviculum-nrf/src/pn.rs:1519` | identify_link failed: end the round and close | node result | none | - | - | `sync_round::tests::a_link_that_cannot_identify_ends_the_round_and_keeps_the_peer_deferred` |
| 23 | `leviculum-nrf/src/pn.rs:1531` | no peering key at link-up: end `no_key` and close | peer.peering_key | none | - | - | `sync_round::tests::an_identified_link_to_a_peer_without_a_key_ends_no_key_and_leaves_it_due` |
| 24 | `leviculum-nrf/src/pn.rs:1540` | no link MDU: end `link_gone` and close | node.link() | none | - | - | `sync_round::tests::a_link_gone_before_the_offer_is_sized_ends_link_gone`<br>`sync_round::tests::an_identified_link_with_a_key_offers_against_its_own_mdu` |
| 25 | `leviculum-nrf/src/pn.rs:1561` | offerable entries: sequence above the cursor, sorted; a store error ends `store_error` (twin at 2613) | store, cursor | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 26 | `leviculum-nrf/src/pn.rs:1580` | empty or absent offer plan: conclude the round and step the cursor to the target | plan | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 27 | `leviculum-nrf/src/pn.rs:1602` | send `/offer` (RequestSent) or end `request_failed` and close | node result | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 28 | `leviculum-nrf/src/pn.rs:1634` | match an offer response to the outbound link and request id | outbound | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 29 | `leviculum-nrf/src/pn.rs:1673` | offer reply mapping: Send, Concluded, Backoff (defer and close), Unpeer (remove and close), RemineKey (clear key), Retry | OfferReply | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 30 | `leviculum-nrf/src/pn.rs:1767` | store error while sending the batch: end `store_error` and close | store result | none | - | - |  |
| 31 | `leviculum-nrf/src/pn.rs:1785` | nothing sent: conclude and advance if nothing was withheld, else end `heap_short` | batch.count, batch.withheld | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 32 | `leviculum-nrf/src/pn.rs:1812` | resource sent: record it, lower the cursor target to the batch target; error ends `send_failed` | node result | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 33 | `leviculum-nrf/src/pn.rs:1830` | outbound resource finished: success concludes, failure ends `transfer_failed` | outbound, success | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 34 | `leviculum-nrf/src/pn.rs:1875` | concluding a round: cursor, Idle, next attempt 0, last_heard now, persist, close | peer | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 35 | `leviculum-nrf/src/pn.rs:1960` | an inbound resource is a sync envelope; `multi` means more than one message | resource data | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 36 | `leviculum-nrf/src/pn.rs:1983` | which remote to peer back: the validated hash, else the link identity's PN hash | validated_links, link identity | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 37 | `leviculum-nrf/src/pn.rs:1989` | not a validated peer with a decoded envelope: close on multi, else queue as a client upload | peer, envelope | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 38 | `leviculum-nrf/src/pn.rs:2008` | a second batch while one drains: close the link | sync_batch | none | - | - |  |
| 39 | `leviculum-nrf/src/pn.rs:2057` | peer back from a recalled announce only when not peered yet | peer table | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 40 | `leviculum-nrf/src/pn.rs:2079` | capture keys from the link or the identity cache, persist, sync now | link identity, storage | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 41 | `leviculum-nrf/src/pn.rs:2142` | skip a push while one is in flight or the gap has not passed | active, next_active_at_ms | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 42 | `leviculum-nrf/src/pn.rs:2145` | which stored message to push: same destination, not too large, lowest sequence | store, too_large | `lora_pn_board_sync` | 1800 | 4/2 (>2) | `active_delivery::tests::the_oldest_message_for_the_destination_is_pushed_and_no_other_recipients`<br>`active_delivery::tests::a_message_known_too_large_is_skipped_for_the_next_oldest`<br>`active_delivery::tests::nothing_stored_for_the_destination_pushes_nothing` |
| 43 | `leviculum-nrf/src/pn.rs:2175` | body missing or shorter than prefix plus stamp: no push and no gap | store body | none | - | - |  |
| 44 | `leviculum-nrf/src/pn.rs:2183` | what is pushed: the body minus its 16-byte prefix and the stamp, as one raw packet | body | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 45 | `leviculum-nrf/src/pn.rs:2184` | push result: Ok sets active; TooLarge joins the too-large list (cap 64, oldest out); other errors set the gap | node result | none | - | - |  |
| 46 | `leviculum-nrf/src/pn.rs:2248` | one work item per pass, else one sync-batch step; an unfinished item goes back to the front | work, sync_batch | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 47 | `leviculum-nrf/src/pn.rs:2265` | a mined key: clear mining_peer, set key and Idle, persist, sync now | MINE_RES | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 48 | `leviculum-nrf/src/pn.rs:644` | miner: a failed generate sends no result, so mining_peer is never cleared | stamper result | none | - | - |  |
| 49 | `leviculum-nrf/src/pn.rs:2297` | store not mounted: withhold the PN announce and defer it 60 s, logged once | record_store::mounted, cadence | none | - | - | `cadence::tests::an_unmounted_store_withholds_the_due_announce_one_retry_at_a_time` |
| 50 | `leviculum-nrf/src/pn.rs:2373` | a commanded announce is refused while the store is unmounted | mounted() | none | - | - |  |
| 51 | `leviculum-nrf/src/pn.rs:2410` | the per-BLE-peer PN announce is skipped while unmounted | mounted() | none | - | - |  |
| 52 | `leviculum-nrf/src/pn.rs:2428` | maintenance due every 480 s | next_maintenance_at_ms | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 53 | `leviculum-nrf/src/pn.rs:2535` | outbound round past its deadline: end `timeout` and close | sync.deadline_ms | none | - | - | `sync_round::tests::a_round_past_its_deadline_times_out_and_one_at_it_does_not` |
| 54 | `leviculum-nrf/src/pn.rs:2540` | skip the sync pass while a round or mining runs or before it is due; else re-arm the interval | outbound, mining_peer, next_sync_at_ms | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 55 | `leviculum-nrf/src/pn.rs:2595` | key not ready: request a path without an identity hash, else hand the job to the miner (Idle if its channel is full) | peering_key_ready, identity_hash, MINE_REQ | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 56 | `leviculum-nrf/src/pn.rs:2618` | store error in start_round: silent return, no backoff booked | store result | none | - | - |  |
| 57 | `leviculum-nrf/src/pn.rs:2639` | probe offer: none returns, an empty plan advances and persists the cursor | plan | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 58 | `leviculum-nrf/src/pn.rs:2648` | no recalled signing key: request a path instead of dialling | recalled identity | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 59 | `leviculum-nrf/src/pn.rs:2655` | book the sync backoff before dialling: accumulator plus one step, next attempt now plus the accumulator | peer backoff | none | - | - | `peering::tests::every_unanswered_sync_attempt_defers_the_peer_one_backoff_step_longer`<br>`peering::tests::a_sync_booking_is_cleared_by_a_link_and_handed_back_by_a_yield` |
| 60 | `leviculum-nrf/src/pn.rs:2666` | connect refused by the link cap: defer, Idle, backoff kept | node result | none | - | - | `sync_round::tests::a_dial_the_link_cap_refuses_defers_the_peer_by_its_booking` |
| 61 | `leviculum-nrf/src/pn.rs:2685` | round deadline is now plus 180 s | now_ms | none | - | - | `sync_round::tests::a_round_past_its_deadline_times_out_and_one_at_it_does_not` |
| 62 | `leviculum-nrf/src/pn.rs:2747` | min_cost 0 and no value needed: accept with value 0 without walking | min_accepted_cost | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 63 | `leviculum-nrf/src/pn.rs:2752` | start a grind if none; undecodable material is Done(None); a resumed grind is not re-tied to its message | grind | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 64 | `leviculum-nrf/src/pn.rs:2776` | min_cost 0 computes the value, otherwise validates against min_cost | min_cost | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 65 | `leviculum-nrf/src/pn.rs:2864` | a parked upload goes back to the front | step | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 66 | `leviculum-nrf/src/pn.rs:2875` | seed the clock from the upload envelope if it decodes | upload data | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 67 | `leviculum-nrf/src/pn.rs:2880` | upload outcome mapping: Accepted flushes; InvalidStamp sends the reject and closes; PeerSyncForm closes; Malformed/StoreFailed log only | UploadOutcome | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 68 | `leviculum-nrf/src/pn.rs:2958` | `/get` without a remote identity: NoIdentity | link identity | none | - | - |  |
| 69 | `leviculum-nrf/src/pn.rs:2965` | the mailbox is the identity's `lxmf.delivery` hash | identity | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 70 | `leviculum-nrf/src/pn.rs:2970` | `/get` outcome: List responds, Fetch flushes purges then serves, Err answers nil | GetOutcome | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 71 | `leviculum-nrf/src/pn.rs:3035` | `/offer` without an identity: NoIdentity | link identity | none | - | - |  |
| 72 | `leviculum-nrf/src/pn.rs:3068` | our cost above 0 validates the peering key; a validator error counts as invalid | our_cost, key | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 73 | `leviculum-nrf/src/pn.rs:3086` | mark the link validated (decides 1201 and 1969 later) | link | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 74 | `leviculum-nrf/src/pn.rs:3089` | which offered ids to want; a store `contains` error counts as want | offer ids, store | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 75 | `leviculum-nrf/src/pn.rs:3130` | validate a synced message only when longer than a stamp; transient id is the unstamped hash; parked goes back | message | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 76 | `leviculum-nrf/src/pn.rs:3184` | count as transferred only when not a duplicate and durable | duplicate, durable | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 77 | `leviculum-nrf/src/pn.rs:3190` | InvalidStamp, Malformed and PeerSyncForm count as rejects, StoreFailed does not | UploadOutcome | none | - | - |  |
| 78 | `leviculum-nrf/src/pn.rs:3208` | batch drained: conclude; any invalid throttles the sender and closes | drained, invalid | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 79 | `leviculum-nrf/src/pn.rs:3246` | fetch response: one packet if it fits (encode error answers nil), else a resource | link MDU | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 80 | `leviculum-nrf/src/pn.rs:3275` | response too large for a packet goes as a resource, other errors are dropped | node result | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 81 | `leviculum-nrf/src/pn.rs:3296` | a failed append forgets its id so the client's retry is stored | flush result | none | - | - |  |

### BLE and Columba (`ble/`)

70 decisions no host test reached on 2026-10-10; 30 more were already a call into a host-built crate and are not listed.

| # | Where | What it decides | State it reads | Cheapest rig cell | s | radios/boards | Host test |
|---|---|---|---|---|---|---|---|
| 82 | `leviculum-nrf/src/ble/mod.rs:300` | peer Up/Lost event dropped when the 8-deep queue is full | BLE_PEER_EVENTS | none | - | - |  |
| 83 | `leviculum-nrf/src/ble/mod.rs:419` | carrier_off resolves only once the media state reads off | media::ble_active | none | - | - |  |
| 84 | `leviculum-nrf/src/ble/mod.rs:429` | carrier_on blocks the protocol task until media reads on | media::ble_active | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 85 | `leviculum-nrf/src/ble/mod.rs:498` | a Route whose slot is no longer claimed is dropped as a route miss | HVN_DRAIN | none | - | - |  |
| 86 | `leviculum-nrf/src/ble/mod.rs:556` | flood target filter: skip unclaimed slots and the ingress slot | HVN_DRAIN, SlotMask | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 87 | `leviculum-nrf/src/ble/mod.rs:578` | per-link queue full: drop (BLE_TX_FANOUT_DROP), else count custody | LINK_OUT fill | none | - | - |  |
| 88 | `leviculum-nrf/src/ble/mod.rs:651` | interface peer count is 0 with the carrier off, else the registry count | media, LIVE_PEERS | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 89 | `leviculum-nrf/src/ble/mod.rs:676` | online iff carrier on and at least one claimed drain slot | media, HVN_DRAIN | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 90 | `leviculum-nrf/src/ble/mod.rs:691` | a peer hint aims at that peer, none floods | peer hint | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 91 | `leviculum-nrf/src/ble/mod.rs:714` | carrier off swallows an outbound packet as Ok | media::ble_active | none | - | - |  |
| 92 | `leviculum-nrf/src/ble/mod.rs:727` | BLE_OUTGOING full answers BufferFull with an IFACE_FULL line | BLE_OUTGOING | none | - | - |  |
| 93 | `leviculum-nrf/src/ble/mod.rs:1041` | inconclusive SoftDevice RAM probe skips the RAM-floor guard | probe result | `boot_smoke_t114` | 300 | 1/1 |  |
| 94 | `leviculum-nrf/src/ble/mod.rs:1058` | SoftDevice RAM above `__sretained` panics at boot | wanted, sd_ceiling | `boot_smoke_t114` | 300 | 1/1 |  |
| 95 | `leviculum-nrf/src/ble/mod.rs:1243` | columba enabled spawns the BLE tasks, else holds the carrier down | boot media profile | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 96 | `leviculum-nrf/src/ble/notify.rs:78` | notify error class: Resources is QueueFull, Disconnected, other Failed | NotifyValueError | none | - | - |  |
| 97 | `leviculum-nrf/src/ble/notify.rs:127` | a torn packet not caused by a disconnect disconnects the link (BLE_TX_RESYNC) | tx.torn, AbortReason | none | - | - |  |
| 98 | `leviculum-nrf/src/ble/notify.rs:144` | Action::Nothing is reported as drop `internal` | action | none | - | - |  |
| 99 | `leviculum-nrf/src/ble/columba.rs:253` | advertised free slots are PERIPH_SLOTS minus busy | PERIPH_BUSY | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 100 | `leviculum-nrf/src/ble/columba.rs:361` | GATT registration refused: BLE_INIT_FAIL, no BLE tasks | SoftDevice result | none | - | - |  |
| 101 | `leviculum-nrf/src/ble/columba.rs:468` | peripheral: carrier off stops advertising and waits | media::ble_active | none | - | - |  |
| 102 | `leviculum-nrf/src/ble/columba.rs:503` | advertise outcome: a conn claims a slot, an error yields None, carrier off restarts | advertise result | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 103 | `leviculum-nrf/src/ble/columba.rs:565` | a failed advertise backs off 1000 ms | conn | none | - | - |  |
| 104 | `leviculum-nrf/src/ble/columba.rs:666` | supervision-timeout request keeps interval and latency, changes the timeout | ask | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 105 | `leviculum-nrf/src/ble/columba.rs:714` | a defragmenter error abandons the reassembly | DefragResult | none | - | - |  |
| 106 | `leviculum-nrf/src/ble/columba.rs:781` | a peripheral conn without a handle abandons the session, no disconnect | conn.handle | none | - | - |  |
| 107 | `leviculum-nrf/src/ble/columba.rs:785` | drain table full abandons the peripheral session, no disconnect | HVN_DRAIN | none | - | - |  |
| 108 | `leviculum-nrf/src/ble/columba.rs:831` | discard the previous tenancy's queue at claim | LINK_OUT[slot] | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 109 | `leviculum-nrf/src/ble/columba.rs:864` | RX classifier: before the handshake a 16-byte write is the identity | handshake_done, len | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 110 | `leviculum-nrf/src/ble/columba.rs:901` | an incoming refusal dead-ends the address, notes the dial ledger, disconnects | link_up result | `ble_room_4` | 900 | 4/4 (>2) |  |
| 111 | `leviculum-nrf/src/ble/columba.rs:918` | payload before the identity is refused and disconnected (BLE_PRE_HANDSHAKE_REFUSE) | handshake_done, len | `ble_unidentified_central_is_dropped` | 600 | 1/1 |  |
| 112 | `leviculum-nrf/src/ble/columba.rs:941` | a write shorter than the fragment header is a keepalive, liveness only | len | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 113 | `leviculum-nrf/src/ble/columba.rs:953` | other payload feeds the liveness clock and the defragmenter | len | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 114 | `leviculum-nrf/src/ble/columba.rs:975` | a completed packet is dropped silently when BLE_INCOMING is full | BLE_INCOMING | none | - | - |  |
| 115 | `leviculum-nrf/src/ble/columba.rs:1022` | a packet before hold-ready is held (logged once) until ready | TxHold | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 116 | `leviculum-nrf/src/ble/columba.rs:1085` | session end with carrier off: BLE_CARRIER_DROP and disconnect | carrier | none | - | - |  |
| 117 | `leviculum-nrf/src/ble/columba.rs:1102` | Expired with a known identity is a churn departure, Displaced/HandshakeOverdue are not | LinkEnd | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 118 | `leviculum-nrf/src/ble/columba.rs:1105` | a link-over end disconnects | LinkEnd | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 119 | `leviculum-nrf/src/ble/columba.rs:1221` | a displacement wake for this handle ends the session, a stale one is ignored | DISPLACE_WAKES | `ble_room_4` | 900 | 4/4 (>2) |  |
| 120 | `leviculum-nrf/src/ble/columba.rs:1366` | Refused: count, LinkRefused phase event, false | LinkUp | `ble_room_4` | 900 | 4/4 (>2) |  |
| 121 | `leviculum-nrf/src/ble/columba.rs:1393` | Displaced: move the queue, signal the old link, LinkDisplaced phase event | LinkUp | `ble_room_4` | 900 | 4/4 (>2) |  |
| 122 | `leviculum-nrf/src/ble/columba.rs:1426` | Accepted: PeerEvent::Up only on the first link | first | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 123 | `leviculum-nrf/src/ble/columba.rs:1542` | displacement moves queued packets; a full new queue drops | LINK_OUT | none | - | - |  |
| 124 | `leviculum-nrf/src/ble/columba.rs:1794` | phase reached fallback: clear PHASE_NEIGHBOURS | poll result | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 125 | `leviculum-nrf/src/ble/columba.rs:2185` | advertising report filter: connectable and not a scan response | report type | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 126 | `leviculum-nrf/src/ble/columba.rs:2306` | an Exclusion rejects the candidate (ledger/churn holds logged) | admission outcome | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 127 | `leviculum-nrf/src/ble/columba.rs:2437` | discovery failure disconnects, no dead-end and no ledger note | discover result | none | - | - |  |
| 128 | `leviculum-nrf/src/ble/columba.rs:2454` | identity read failure disconnects, no dead-end and no ledger note | read result | none | - | - |  |
| 129 | `leviculum-nrf/src/ble/columba.rs:2474` | a peer presenting our own identity: BLE_LINK_SELF, dead-end, disconnect | peer_id, own identity | none | - | - |  |
| 130 | `leviculum-nrf/src/ble/columba.rs:2497` | a central conn without a handle returns, no disconnect | conn.handle | none | - | - |  |
| 131 | `leviculum-nrf/src/ble/columba.rs:2501` | drain table full on the central side disconnects | HVN_DRAIN | none | - | - |  |
| 132 | `leviculum-nrf/src/ble/columba.rs:2518` | discard the previous tenancy's queue before the registry entry (central) | LINK_OUT[slot] | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 133 | `leviculum-nrf/src/ble/columba.rs:2522` | an outgoing duplicate refused: dead-end, dial ledger, disconnect, release | link_up result | `ble_room_4` | 900 | 4/4 (>2) |  |
| 134 | `leviculum-nrf/src/ble/columba.rs:2606` | CCCD subscribe failure disconnects | write result | none | - | - |  |
| 135 | `leviculum-nrf/src/ble/columba.rs:2619` | a handshake value that cannot be built returns without a disconnect | GattData::new | none | - | - |  |
| 136 | `leviculum-nrf/src/ble/columba.rs:2624` | handshake write failure disconnects | write result | none | - | - |  |
| 137 | `leviculum-nrf/src/ble/columba.rs:2688` | central RX: shorter than the header is a keepalive, else payload and defrag | len | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 138 | `leviculum-nrf/src/ble/columba.rs:2718` | central: a completed packet is dropped silently when BLE_INCOMING is full | BLE_INCOMING | none | - | - |  |
| 139 | `leviculum-nrf/src/ble/columba.rs:2738` | central keepalive is always armed | last_keepalive | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 140 | `leviculum-nrf/src/ble/columba.rs:2751` | a fragment that cannot be built: BLE_TX_DROP, torn | GattData::new | none | - | - |  |
| 141 | `leviculum-nrf/src/ble/columba.rs:2776` | write-command error: BLE_TX_DROP, torn | write result | none | - | - |  |
| 142 | `leviculum-nrf/src/ble/columba.rs:2816` | torn on any failed fragment, the first included: disconnect | torn | none | - | - |  |
| 143 | `leviculum-nrf/src/ble/columba.rs:2830` | keepalive failure ends the outbound half, no disconnect | write result | none | - | - |  |
| 144 | `leviculum-nrf/src/ble/columba.rs:2762` | central carrier off: BLE_CARRIER_DROP and disconnect | carrier | none | - | - |  |
| 145 | `leviculum-nrf/src/ble/columba.rs:2866` | central Expired is a churn departure; any link-over end disconnects | LinkEnd | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 146 | `leviculum-nrf/src/ble/columba.rs:2898` | central task: carrier off, no scanning or dialling | media::ble_active | none | - | - |  |
| 147 | `leviculum-nrf/src/ble/columba.rs:2922` | chosen target: fallback verdict is FallbackTarget, else StrictTarget | decision | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 148 | `leviculum-nrf/src/ble/columba.rs:2954` | an empty scan window rescans at once | scan result | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 149 | `leviculum-nrf/src/ble/columba.rs:2955` | a scan error waits 5 s | scan result | none | - | - |  |
| 150 | `leviculum-nrf/src/ble/columba.rs:2961` | carrier off mid-scan returns to the gate without backoff | carrier | none | - | - |  |
| 151 | `leviculum-nrf/src/ble/columba.rs:2963` | 5 s backoff after every dial attempt and session end | CENTRAL_RETRY_BACKOFF_MS | `ble_mesh_formation` | 600 | 3/2 (>2) |  |

### LoRa, radio, announce and telemetry (`lora.rs`, `sx1262.rs`, `announce.rs`, `telemetry.rs`)

97 decisions no host test reached on 2026-10-10; 54 more were already a call into a host-built crate and are not listed.

| # | Where | What it decides | State it reads | Cheapest rig cell | s | radios/boards | Host test |
|---|---|---|---|---|---|---|---|
| 152 | `leviculum-nrf/src/lora.rs:127` | refuse a TX-spacing set while one is queued (REFUSE_BUSY) | LORA_TX_SPACING | none | - | - |  |
| 153 | `leviculum-nrf/src/lora.rs:328` | carrier off swallows an outbound packet as Ok | media::lora_active | none | - | - |  |
| 154 | `leviculum-nrf/src/lora.rs:371` | channel send failure releases the budget and answers BufferFull | channel | none | - | - |  |
| 155 | `leviculum-nrf/src/lora.rs:535` | a config whose bandwidth has no SX1262 code is rejected | wire.bandwidth_hz | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 156 | `leviculum-nrf/src/lora.rs:550` | coding-rate code is the denominator minus 4, unchecked | wire.cr | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 157 | `leviculum-nrf/src/lora.rs:709` | mirror a duty-hold edge into the core | LORA_DUTY_HOLD | `lora_duty_lock_lawful_default` | 900 | 2/1 |  |
| 158 | `leviculum-nrf/src/lora.rs:713` | no announce-cap registration before the radio is configured | RUNNING_CONFIG | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 159 | `leviculum-nrf/src/lora.rs:774` | open a duty hold once per hold: raise the flag and signal the USB frame | hold state, stale total | `lora_duty_lock_lawful_default` | 900 | 2/1 | `queue_budget tests::a_hold_opens_once_and_the_lift_measures_from_the_first_held_turn` |
| 160 | `leviculum-nrf/src/lora.rs:795` | lift a duty hold only if one is open, once, and state held time and stale drops | hold state, airtime | none (needs a run past one rolling hour) | >3600 | - | `queue_budget tests::a_lift_happens_once_and_never_without_a_hold`<br>`queue_budget tests::the_lift_counts_drops_across_the_counter_wrap` |
| 161 | `leviculum-nrf/src/lora.rs:960` | radio_silent grants (restarts) the mute lease, else clears it | config.radio_silent | none | - | - |  |
| 162 | `leviculum-nrf/src/lora.rs:1070` | split-frame sequence nibble from the xorshift state | rng_state | `lora_lnode_split` | 240 | 2/1 |  |
| 163 | `leviculum-nrf/src/lora.rs:1125` | TX ok charges airtime; a TX error aborts, the second split frame is neither sent nor charged | radio result | none | - | - |  |
| 164 | `leviculum-nrf/src/lora.rs:1152` | count TX and flash the display only if every frame went out | tx_ok | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 165 | `leviculum-nrf/src/lora.rs:1353` | no RX window while a config is pending | config_rx | none | - | - |  |
| 166 | `leviculum-nrf/src/lora.rs:1408` | window outcome: reception true, timeout or error false (drives empty-ack and peer-yield) | rx_result | `bench_dual_pair_fast` | 360 | 4/2 (>2) |  |
| 167 | `leviculum-nrf/src/lora.rs:1569` | runtime reconfig ok: mute, publish, set_phy, limits, CONFIG_APPLIED; error keeps the old PHY, no ack | configure result | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 168 | `leviculum-nrf/src/lora.rs:1676` | init_radio failure ends the LoRa task | radio | none | - | - |  |
| 169 | `leviculum-nrf/src/lora.rs:1693` | boot configure failure ends the task; ok applies the mute and publishes | radio | `boot_smoke_t114` | 300 | 1/1 |  |
| 170 | `leviculum-nrf/src/lora.rs:1726` | a zero RNG seed is replaced | channel_seed | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 171 | `leviculum-nrf/src/lora.rs:1784` | take a new TX spacing at the top of a turn | spacing_rx | none | - | - |  |
| 172 | `leviculum-nrf/src/lora.rs:1798` | apply a runtime config at the top of a turn | config_rx | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 173 | `leviculum-nrf/src/lora.rs:1847` | lift the hold on an idle turn: hold open, nothing pending, not locked | hold state, pending_tx, airtime | none (needs a run past one rolling hour) | >3600 | - | `queue_budget tests::an_idle_turn_lifts_only_an_open_hold_with_nothing_pending_and_no_lock`<br>`queue_budget tests::the_hold_edge_lifts_on_the_first_idle_turn_after_the_rolling_hour_frees_a_bin` |
| 174 | `leviculum-nrf/src/lora.rs:1876` | dequeue the next frame only when none is pending | pending_tx | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 175 | `leviculum-nrf/src/lora.rs:1908` | DropStale: open the hold, count, drop, rate-limited line, next turn | verdict | `lora_duty_lock_lawful_default` | 900 | 2/1 |  |
| 176 | `leviculum-nrf/src/lora.rs:1953` | Hold: open the hold, listen one post-TX window at site Hold, retry | verdict | `lora_duty_lock_lawful_default` | 900 | 2/1 |  |
| 177 | `leviculum-nrf/src/lora.rs:2025` | acquisition jitter: listen through the draw, clamp 1..10000 ms, credit elapsed | access | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 178 | `leviculum-nrf/src/lora.rs:2068` | CAD clear transmits and bumps the burst counters | CAD result | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 179 | `leviculum-nrf/src/lora.rs:2131` | CSMA backoff is slots times the jitter slot, clamped 1..10000 ms | slots | `bench_dual_pair_fast` | 360 | 4/2 (>2) |  |
| 180 | `leviculum-nrf/src/lora.rs:2159` | CAD error: a forced TX skips the ack window and burst check | access | none | - | - |  |
| 181 | `leviculum-nrf/src/lora.rs:2210` | burst continuation pulls the next frame; a mute-dropped one counts as empty | queue, lease | `bench_dual_pair_fast` | 360 | 4/2 (>2) |  |
| 182 | `leviculum-nrf/src/lora.rs:2243` | post-TX ack window counts consecutive empty acks | ack_received | `bench_dual_pair_fast` | 360 | 4/2 (>2) |  |
| 183 | `leviculum-nrf/src/lora.rs:2270` | peer-turn yield after two empty acks, twice the ack window | consecutive_empty_acks | `bench_dual_pair_fast` | 360 | 4/2 (>2) |  |
| 184 | `leviculum-nrf/src/lora.rs:2297` | reset burst accounting; the next acquisition owes fresh jitter | - | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 185 | `leviculum-nrf/src/lora.rs:2318` | idle continuous RX until a frame or an outgoing packet | select | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 186 | `leviculum-nrf/src/lora.rs:2373` | outgoing in idle stands RX down deferring one frame, then admits | latch | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 187 | `leviculum-nrf/src/sx1262.rs:495` | image-calibration band from the frequency | freq_hz | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 188 | `leviculum-nrf/src/sx1262.rs:516` | LDO regulator, DIO2 as RF switch, TCXO timeout | constants | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 189 | `leviculum-nrf/src/sx1262.rs:631` | packet params: explicit header, CRC on, standard IQ, preamble | preamble_len | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 190 | `leviculum-nrf/src/sx1262.rs:750` | sync word 0x1424, TX clamp, StopRxTimerOnPreamble | constants | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 191 | `leviculum-nrf/src/sx1262.rs:815` | TX ok is tx_done, else force standby | on_air result | none | - | - |  |
| 192 | `leviculum-nrf/src/sx1262.rs:831` | TX wait floor 100 ms | timeout_ms | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 193 | `leviculum-nrf/src/sx1262.rs:839` | TX success only with IRQ_TX_DONE | IRQ flags | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 194 | `leviculum-nrf/src/sx1262.rs:1050` | RX hardware timeout: 0 single mode, else ms*64 capped at 0xFFFFFF | timeout_ms | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 195 | `leviculum-nrf/src/sx1262.rs:1107` | awaiting an unarmed window is NotArmed | rx_state | none | - | - |  |
| 196 | `leviculum-nrf/src/sx1262.rs:1112` | software wait: 60 s single mode, else timeout plus 500 ms | timeout_ms | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 197 | `leviculum-nrf/src/sx1262.rs:1148` | latched take: not armed is an error, no RxDone/Timeout is None | rx_state, IRQ | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 198 | `leviculum-nrf/src/sx1262.rs:1280` | the chip left RX on RxDone or Timeout | IRQ flags | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 199 | `leviculum-nrf/src/sx1262.rs:1288` | erratum 15.3 workaround only for timed RX | hw_timeout | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 200 | `leviculum-nrf/src/sx1262.rs:1292` | classify: CRC error drops the payload, RxDone is a frame, Timeout, else WaitExpired | IRQ flags | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 201 | `leviculum-nrf/src/sx1262.rs:1315` | truncate the read to the buffer | len | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 202 | `leviculum-nrf/src/sx1262.rs:1364` | CAD detMin 0x0A, CAD-only exit mode | constants | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 203 | `leviculum-nrf/src/sx1262.rs:1422` | CAD done: busy iff CAD_DETECTED; timeout goes standby with an error | DIO1, IRQ | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 204 | `leviculum-nrf/src/sx1262.rs:1484` | standing-window age (clamped) fed to adoption | rx_state | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 205 | `leviculum-nrf/src/sx1262.rs:1501` | a window ends itself iff its timeout is non-zero | window | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 206 | `leviculum-nrf/src/announce.rs:149` | announce set is 2 frames with the PN role, else 1 | propagation_hash | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 207 | `leviculum-nrf/src/announce.rs:184` | the first announce heard from a destination triggers our own at once | NEIGHBOURS | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 208 | `leviculum-nrf/src/announce.rs:264` | neighbour-known test, round-robin eviction at 32 | slots | none | - | - |  |
| 209 | `leviculum-nrf/src/announce.rs:292` | movement evidence: none for a fixed pin, no GNSS, invalid or no lat/lon | position flags, GNSS_FIX | none | - | - |  |
| 210 | `leviculum-nrf/src/announce.rs:350` | sleep until the earlier of the announce deadline and the movement sample | cadence, next_sample_ms | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 211 | `leviculum-nrf/src/announce.rs:360` | sample movement on its interval; a resume announces at once | next_sample_ms | none | - | - |  |
| 212 | `leviculum-nrf/src/announce.rs:398` | no delivery destination, no peer-up announce | delivery_hash | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 213 | `leviculum-nrf/src/announce.rs:444` | no delivery hash: nothing, after the poll consumed the slot | delivery_hash | none | - | - |  |
| 214 | `leviculum-nrf/src/telemetry.rs:148` | position-source flags gate NoPositionSource | gnss_available, flash | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 215 | `leviculum-nrf/src/telemetry.rs:173` | set or clear the fixed-position bit | present | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 216 | `leviculum-nrf/src/telemetry.rs:189` | inbound target: the newest replaces an unread one | INBOUND_TARGET | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 217 | `leviculum-nrf/src/telemetry.rs:213` | inbound fixed position: the newest replaces an unread one | channel | none | - | - |  |
| 218 | `leviculum-nrf/src/telemetry.rs:429` | persist answer Durable/Lost, Lost after the 2.5 s deadline | gate, deadline | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 219 | `leviculum-nrf/src/telemetry.rs:571` | a save request displaces an unread older one (six record kinds) | PENDING_SAVE_* | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 220 | `leviculum-nrf/src/telemetry.rs:681` | which record the store task rewrites | select result | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 221 | `leviculum-nrf/src/telemetry.rs:713` | skip the erase when the page is unchanged, answer Durable | flash contents | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 222 | `leviculum-nrf/src/telemetry.rs:726` | flash write retried 3 times at 250 ms, then Lost | flash result | none | - | - |  |
| 223 | `leviculum-nrf/src/telemetry.rs:764` | Durable iff written | written | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 224 | `leviculum-nrf/src/telemetry.rs:849` | a sensor fix needs lat and lon | readings | none | - | - |  |
| 225 | `leviculum-nrf/src/telemetry.rs:864` | a sensor location needs HDOP; accuracy is hdop*5 m | readings.hdop | none | - | - |  |
| 226 | `leviculum-nrf/src/telemetry.rs:1241` | a new target drops the owed proof and retry of the old one | - | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 227 | `leviculum-nrf/src/telemetry.rs:1251` | owe a key persist for a Set with hash only | command, public_key | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 228 | `leviculum-nrf/src/telemetry.rs:1263` | a key-bearing target remembers the identity or logs the rejection | public_key | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 229 | `leviculum-nrf/src/telemetry.rs:1338` | no target, nothing | target | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 230 | `leviculum-nrf/src/telemetry.rs:1349` | NoPositionSource sends nothing, before the key chase | policy.state | none | - | - |  |
| 231 | `leviculum-nrf/src/telemetry.rs:1357` | AwaitingKey: a known key is ready, else a path request every 60 s | storage, last_key_request_ms | none | - | - |  |
| 232 | `leviculum-nrf/src/telemetry.rs:1387` | rewrite the flash record once with the resolved key | key_persist_owed | none | - | - |  |
| 233 | `leviculum-nrf/src/telemetry.rs:1423` | a retry debt without payload is forgotten | retry_payload | none | - | - |  |
| 234 | `leviculum-nrf/src/telemetry.rs:1495` | sensor fix only in presence state Fix | presence_has_fix | none | - | - |  |
| 235 | `leviculum-nrf/src/telemetry.rs:1507` | location source: none, fixed or sensor | include_position, possrc | none | - | - |  |
| 236 | `leviculum-nrf/src/telemetry.rs:1515` | no calendar time: withhold `no-clock` | unix_secs | none | - | - |  |
| 237 | `leviculum-nrf/src/telemetry.rs:1548` | build failure books airtime and withholds | build result | none | - | - |  |
| 238 | `leviculum-nrf/src/telemetry.rs:1564` | on-air failure books airtime and withholds | message | none | - | - |  |
| 239 | `leviculum-nrf/src/telemetry.rs:1575` | no usable path: path request, book airtime, withhold | has_path, path_route | none | - | - |  |
| 240 | `leviculum-nrf/src/telemetry.rs:1634` | send the report; its route is the interfaces of its own SendPacket actions | send result | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 241 | `leviculum-nrf/src/telemetry.rs:1654` | record the position in the cadence only if included | include_position | none | - | - |  |
| 242 | `leviculum-nrf/src/telemetry.rs:1673` | send error books airtime and withholds | - | none | - | - |  |
| 243 | `leviculum-nrf/src/telemetry.rs:1763` | only packets to our own delivery destination are screened | destination | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 244 | `leviculum-nrf/src/telemetry.rs:1766` | only the configured target may request a report | target, storage | none | - | - |  |
| 245 | `leviculum-nrf/src/telemetry.rs:1770` | not-a-message or no-inbox discards; a request is accepted or rejected | verdict | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 246 | `leviculum-nrf/src/telemetry.rs:1859` | no pending report notes a non-dispatch | pending_line | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 247 | `leviculum-nrf/src/telemetry.rs:1868` | losses are the error, drop and retry interface ids | DispatchResult | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 248 | `leviculum-nrf/src/telemetry.rs:1947` | announce payload: name, no stamp cost, no compression | identity, name | `board_delivery_promise` | 1800 | 3/1 (>2) |  |

### Main loops, USB control, status and panic handling (`bin/*`, `usb.rs`, `lib.rs`, `remote_status.rs`, `mgmt.rs`, `interface.rs`)

90 decisions no host test reached on 2026-10-10; 31 more were already a call into a host-built crate and are not listed.

| # | Where | What it decides | State it reads | Cheapest rig cell | s | radios/boards | Host test |
|---|---|---|---|---|---|---|---|
| 249 | `leviculum-nrf/src/bin/t114.rs:144` | persistent-log replay drops the leading partial line (also rak4631.rs:164, solarnode.rs:171) | snapshot bytes | `boot_smoke_t114` | 300 | 1/1 |  |
| 250 | `leviculum-nrf/src/bin/t114.rs:202` | node build: transport on, max_links, 8 queued announces, 8 random blobs, probes answered (all three bins) | constants, heap_census | `boot_smoke_t114` | 300 | 1/1 |  |
| 251 | `leviculum-nrf/src/bin/t114.rs:240` | use the flash identity or generate a new one (all three bins) | id_store.load() | `boot_smoke_t114` | 300 | 1/1 |  |
| 252 | `leviculum-nrf/src/bin/t114.rs:272` | mgmt RUNNING flag means the destination was derived (all three bins) | remote_mgmt_dest_hash | `lnode_remote_status` | 900 | 2/1 |  |
| 253 | `leviculum-nrf/src/bin/t114.rs:283` | persist a generated identity, save result ignored (all three bins) | identity_loaded | `reset_loop_quiet` | 300 | 1/1 |  |
| 254 | `leviculum-nrf/src/bin/t114.rs:290` | interface config: MTU 564/508/564, Gateway mode on all three | constants | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 255 | `leviculum-nrf/src/bin/solarnode.rs:405` | probe and mount QSPI only where the board declares a part | CONFIG.qspi_part | `boot_smoke_t114` | 300 | 1/1 |  |
| 256 | `leviculum-nrf/src/bin/t114.rs:386` | radio profile: persisted config, else eu_medium (all three bins) | flash page | `reset_loop_quiet` | 300 | 1/1 |  |
| 257 | `leviculum-nrf/src/bin/t114.rs:411` | spawn the LoRa task or hold the radio down (all three bins) | media.lora_enabled | `boot_smoke_t114` | 300 | 1/1 |  |
| 258 | `leviculum-nrf/src/bin/t114.rs:439` | BLE on or off at init (all three bins) | media.ble_enabled | `boot_smoke_t114` | 300 | 1/1 |  |
| 259 | `leviculum-nrf/src/bin/t114.rs:598` | no IFAC on any interface (all three bins) | - | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 260 | `leviculum-nrf/src/bin/t114.rs:629` | PN role on or off, spawn the miner (all three bins) | pn::Engine::new | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 261 | `leviculum-nrf/src/bin/t114.rs:641` | a telemetry reporter exists only if the delivery destination registered (all three bins) | delivery_hash | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 262 | `leviculum-nrf/src/bin/t114.rs:652` | restore the persisted telemetry target at boot (all three bins) | flash | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 263 | `leviculum-nrf/src/bin/t114.rs:658` | restore the persisted fixed position at boot (all three bins) | flash | none | - | - |  |
| 264 | `leviculum-nrf/src/bin/t114.rs:676` | GNSS time seeding off when the watch receiver is unavailable (all three bins) | GNSS_FIX.receiver() | `boot_smoke_t114` | 300 | 1/1 |  |
| 265 | `leviculum-nrf/src/bin/t114.rs:763` | main-loop wake: earliest of node, stats, census, PN deadlines (all three bins) | deadlines | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 266 | `leviculum-nrf/src/bin/t114.rs:805` | arm the 5 s telemetry tick only while a target is set (all three bins) | reporter | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 267 | `leviculum-nrf/src/bin/t114.rs:848` | mirror interface online state into routing (all three bins) | is_online() | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 268 | `leviculum-nrf/src/bin/t114.rs:855` | mirror the BLE peer count into routing (all three bins) | peer_count | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 269 | `leviculum-nrf/src/bin/t114.rs:880` | GNSS seed accepted marks seeded and sets the time source, else refuses (all three bins) | node | none | - | - |  |
| 270 | `leviculum-nrf/src/bin/t114.rs:892` | host wall time: ack or REFUSE_VALUE, answer dropped when the channel is full (all three bins) | node, serial_ctl_tx | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 271 | `leviculum-nrf/src/bin/t114.rs:921` | host telemetry target applied, dropped silently without a reporter (all three bins) | reporter | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 272 | `leviculum-nrf/src/bin/t114.rs:934` | host fixed position applied, dropped silently without a reporter (all three bins) | reporter | none | - | - |  |
| 273 | `leviculum-nrf/src/bin/t114.rs:977` | host announce: run the PN announce if the role exists and produced actions (all three bins) | pn_engine | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 274 | `leviculum-nrf/src/bin/t114.rs:997` | host announce answer: UNSUPPORTED without delivery, NO_CLOCK, ack, or BUSY (all three bins) | delivery_hash, wall clock | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 275 | `leviculum-nrf/src/bin/t114.rs:1049` | an ack overrides the refusal when the PN announce went out (all three bins) | pn_announced | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 276 | `leviculum-nrf/src/bin/t114.rs:1119` | LoRa RX dropped while LoRa is off at runtime, PN step skipped too (all three bins) | media STATE | none | - | - |  |
| 277 | `leviculum-nrf/src/bin/t114.rs:1152` | attribute BLE RX to the peer the handshake named (all three bins) | peer | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 278 | `leviculum-nrf/src/bin/t114.rs:1211` | BLE peer up: PN announce to the phone if the role exists (all three bins) | pn_engine | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 279 | `leviculum-nrf/src/bin/t114.rs:723` | answer `/status` from the PN step on any carrier (all three bins) | events | `lnode_remote_status` | 900 | 2/1 |  |
| 280 | `leviculum-nrf/src/bin/t114.rs:1293` | readings: unix time only with a plausible wall clock (all three bins) | node | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 281 | `leviculum-nrf/src/bin/t114.rs:1308` | readings: GNSS fields copied whenever a fix value exists (all three bins) | GNSS_FIX | none | - | - |  |
| 282 | `leviculum-nrf/src/bin/t114.rs:1316` | readings: has_fix iff presence is Fix (all three bins) | GNSS_PRESENCE | none | - | - |  |
| 283 | `leviculum-nrf/src/bin/t114.rs:1327` | readings: battery percent only once classified (all three bins) | BATTERY_STATE | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 284 | `leviculum-nrf/src/interface.rs:39` | the serial interface is always online | - | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 285 | `leviculum-nrf/src/interface.rs:45` | a full outgoing channel drops the frame with BufferFull; TX bytes only on accept | channel | none | - | - |  |
| 286 | `leviculum-nrf/src/remote_status.rs:61` | only RequestReceived events are considered | events | `lnode_remote_status` | 900 | 2/1 |  |
| 287 | `leviculum-nrf/src/remote_status.rs:72` | answer only `/status` at our mgmt destination | path, mgmt dest | `lnode_remote_status` | 900 | 2/1 |  |
| 288 | `leviculum-nrf/src/remote_status.rs:103` | LoRa bitrate only while the radio runs | running_config | `lnode_remote_status` | 900 | 2/1 |  |
| 289 | `leviculum-nrf/src/remote_status.rs:120` | interface status is the core's online mirror | node | `lnode_remote_status` | 900 | 2/1 |  |
| 290 | `leviculum-nrf/src/remote_status.rs:124` | clients reported for BLE only | iface id | `lnode_remote_status` | 900 | 2/1 |  |
| 291 | `leviculum-nrf/src/remote_status.rs:140` | link count only with lstats | include_lstats | `lnode_remote_status` | 900 | 2/1 |  |
| 292 | `leviculum-nrf/src/remote_status.rs:158` | single-packet answer, PayloadTooLarge falls back to a resource, other errors drop | send result | `lnode_remote_status` | 900 | 2/1 |  |
| 293 | `leviculum-nrf/src/mgmt.rs:205` | publish the new allow-list, then request the save | - | `lnode_remote_status` | 900 | 2/1 |  |
| 294 | `leviculum-nrf/src/mgmt.rs:226` | revert the published list to the previous one | previous | none | - | - |  |
| 295 | `leviculum-nrf/src/mgmt.rs:254` | no record reports None | STORED | `lnode_remote_status` | 900 | 2/1 |  |
| 296 | `leviculum-nrf/src/usb.rs:61` | which control types are accepted and advertised | const list | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 297 | `leviculum-nrf/src/usb.rs:234` | GPREGRET via SoftDevice call or direct write, then reset | SD enabled | `reset_loop_quiet` | 300 | 1/1 |  |
| 298 | `leviculum-nrf/src/usb.rs:275` | the debug port drains only while DTR is up; the first DTR opens the drain gate | DTR | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 299 | `leviculum-nrf/src/usb.rs:288` | a drain write times out after 50 ms | USB | none | - | - |  |
| 300 | `leviculum-nrf/src/usb.rs:322` | debug bytes: `p` postmortem, `s` stack reset, `c` heap census | RX bytes | none | - | - |  |
| 301 | `leviculum-nrf/src/usb.rs:347` | open the drain gate after 30 s headless | timer | none | - | - |  |
| 302 | `leviculum-nrf/src/usb.rs:404` | legacy RESET_ACK vs envelope ack | legacy | `reset_loop_quiet` | 300 | 1/1 |  |
| 303 | `leviculum-nrf/src/usb.rs:429` | reset even if the plan did not | - | `reset_loop_quiet` | 300 | 1/1 |  |
| 304 | `leviculum-nrf/src/usb.rs:457` | never write with DTR down; abandon on DTR drop; retry on control change | DTR | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 305 | `leviculum-nrf/src/usb.rs:487` | ZLP when the framed length is a multiple of 64 (twin at 1352) | len | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 306 | `leviculum-nrf/src/usb.rs:541` | invalid radio config answers Invalid | wire | none | - | - |  |
| 307 | `leviculum-nrf/src/usb.rs:545` | LoRa not booted: persist only, Stored or StoreFailed | lora_booted, flash ack | none | - | - |  |
| 308 | `leviculum-nrf/src/usb.rs:573` | channel full: the same config waits for apply; a different one gets one grace send, else Undeliverable | channel, pending_config | none | - | - |  |
| 309 | `leviculum-nrf/src/usb.rs:607` | note delivery and request the save only if this call delivered | delivered_here | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 310 | `leviculum-nrf/src/usb.rs:622` | Applied vs Unconfirmed after the apply window | running_config, signal | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 311 | `leviculum-nrf/src/usb.rs:656` | skip persisting a frame that was not taken | took_it | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 312 | `leviculum-nrf/src/usb.rs:709` | 500 ms frame timeout mid-frame, else 3600 s | deframer | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 313 | `leviculum-nrf/src/usb.rs:729` | a non-control frame is counted and forwarded with backpressure | channel | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 314 | `leviculum-nrf/src/usb.rs:740` | host-requested reset | action | `reset_loop_quiet` | 300 | 1/1 |  |
| 315 | `leviculum-nrf/src/usb.rs:787` | an invalid legacy config gets no answer | - | none | - | - |  |
| 316 | `leviculum-nrf/src/usb.rs:796` | wall time: busy refusal while one is pending, else the main loop answers | WALL_TIME_CHANNEL | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 317 | `leviculum-nrf/src/usb.rs:823` | telemetry target: wired and key usable and delivered, persisted, answered | wired, key, channel | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 318 | `leviculum-nrf/src/usb.rs:868` | fixed position: the same chain | wired, channel | none | - | - |  |
| 319 | `leviculum-nrf/src/usb.rs:896` | media profile persisted if wired, answered from running or configured | media state | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 320 | `leviculum-nrf/src/usb.rs:984` | node name applied only if wired and the report is ready | name state | none | - | - |  |
| 321 | `leviculum-nrf/src/usb.rs:1041` | announce: busy refusal while one is pending, else the main loop answers | ANNOUNCE_CHANNEL | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 322 | `leviculum-nrf/src/usb.rs:1067` | BLE TX gap applied and acked unconditionally | - | none | - | - |  |
| 323 | `leviculum-nrf/src/usb.rs:1092` | store storm ack vs BUSY | request_storm | none | - | - |  |
| 324 | `leviculum-nrf/src/usb.rs:1119` | PN config ack vs REFUSE_PERSIST, no wired gate | flash ack | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 325 | `leviculum-nrf/src/usb.rs:1151` | mgmt allow: persist if wired, revert on wired and Lost, report or UNSUPPORTED | wired, persist | `lnode_remote_status` | 900 | 2/1 |  |
| 326 | `leviculum-nrf/src/usb.rs:1188` | mgmt query: report or UNSUPPORTED | wired | `lnode_remote_status` | 900 | 2/1 |  |
| 327 | `leviculum-nrf/src/usb.rs:1220` | TX spacing ack vs BUSY | deliver_tx_spacing | none | - | - |  |
| 328 | `leviculum-nrf/src/usb.rs:1254` | radio query: running config report or BUSY | running_config | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 329 | `leviculum-nrf/src/usb.rs:1290` | USB read error goes back to wait_connection | USB | none | - | - |  |
| 330 | `leviculum-nrf/src/usb.rs:1296` | frame timeout mid-frame drops the partial frame | deframer | none | - | - |  |
| 331 | `leviculum-nrf/src/usb.rs:1343` | outgoing packet: drop the rest when the host stops reading; ZLP only after a good write | DTR | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 332 | `leviculum-nrf/src/lib.rs:131` | VBUS assumed present at boot | - | `boot_smoke_t114` | 300 | 1/1 |  |
| 333 | `leviculum-nrf/src/lib.rs:314` | the panic count resets on a cold boot | retained magic | `reset_loop_quiet` | 300 | 1/1 |  |
| 334 | `leviculum-nrf/src/lib.rs:750` | panic-message ring linearisation | len | none | - | - |  |
| 335 | `leviculum-nrf/src/lib.rs:767` | replay a panic postmortem once at boot; the query accepts both states | magic | none | - | - |  |
| 336 | `leviculum-nrf/src/lib.rs:841` | panic: no LED armed resets at once, else 25 blinks then reset | PANIC_LED_ARMED | none | - | - |  |
| 337 | `leviculum-nrf/src/lib.rs:949` | HardFault postmortem take/peek, same rule | magic | none | - | - |  |
| 338 | `leviculum-nrf/src/lib.rs:1082` | HardFault: no LED resets at once, else blink then reset | HARDFAULT_LED_ARMED | none | - | - |  |

### Stores, sensors, display and boot plumbing (the remaining modules)

74 decisions no host test reached on 2026-10-10; 24 more were already a call into a host-built crate and are not listed.

| # | Where | What it decides | State it reads | Cheapest rig cell | s | radios/boards | Host test |
|---|---|---|---|---|---|---|---|
| 339 | `leviculum-nrf/src/gnss.rs:154` | which init sequencer runs: UBX on ZOE-M8Q, `$PCAS` on L76K | wiring.module | `boot_smoke_t114` | 300 | 1/1 |  |
| 340 | `leviculum-nrf/src/gnss.rs:255` | RMC fold: valid sets lat/lon/time/speed/bearing, invalid clears only unix_secs | rmc, latest | none | - | - |  |
| 341 | `leviculum-nrf/src/gnss.rs:277` | GGA fold: valid is quality != NoFix; position, altitude, HDOP only on a fix | gga, latest | none | - | - |  |
| 342 | `leviculum-nrf/src/gnss.rs:323` | hold the receiver power enable high, where the board has one | wiring.power_enable | `boot_smoke_t114` | 300 | 1/1 |  |
| 343 | `leviculum-nrf/src/gnss.rs:334` | hold the standby pin high | wiring.standby | `boot_smoke_t114` | 300 | 1/1 |  |
| 344 | `leviculum-nrf/src/gnss.rs:446` | rebuild the UART only for a different baud | pending_baud, configured_baud | `boot_smoke_t114` | 300 | 1/1 |  |
| 345 | `leviculum-nrf/src/gnss.rs:458` | init frames after the baud decision, at most 4 staged per loop, extras dropped | locked, sentences_seen | `boot_smoke_t114` | 300 | 1/1 |  |
| 346 | `leviculum-nrf/src/battery.rs:234` | power the divider only while sampling, 10 ms settle | divider_enable | `boot_smoke_t114` | 300 | 1/1 |  |
| 347 | `leviculum-nrf/src/battery.rs:393` | publish BATTERY_STATE every 5th sample | samples | `boot_smoke_t114` | 300 | 1/1 |  |
| 348 | `leviculum-nrf/src/battery.rs:396` | 30 s report, then reseed the window with the closing sample | samples | `boot_smoke_t114` | 300 | 1/1 |  |
| 349 | `leviculum-nrf/src/record_store.rs:177` | refuse a PN op while unmounted (the engine then forgets the id, no proof) | MOUNTED | none | - | - |  |
| 350 | `leviculum-nrf/src/record_store.rs:243` | refuse a storm: unmounted, running, 0 records, oversize, slot full | MOUNTED, STORM_RUNNING, args | none | - | - |  |
| 351 | `leviculum-nrf/src/record_store.rs:302` | flash retry: 4 attempts at 50/100/200 ms, each failure counted | flash result | none | - | - |  |
| 352 | `leviculum-nrf/src/record_store.rs:394` | mount failure ends the task, every later request refused | mount result | none | - | - |  |
| 353 | `leviculum-nrf/src/record_store.rs:411` | serve a storm, a PN op or the stats tick | channels | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 354 | `leviculum-nrf/src/record_store.rs:462` | append failure counts a sealed page when room went from above 0 to 0 | append result, sector_room | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 355 | `leviculum-nrf/src/record_store.rs:483` | purge only when the record at the offset still has the key | record_at, key | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 356 | `leviculum-nrf/src/record_store.rs:524` | format any region that is not ours; an error leaves no store this boot | mount result | none | - | - |  |
| 357 | `leviculum-nrf/src/record_store.rs:556` | a failed live-count scan aborts the mount | log.count() | none | - | - |  |
| 358 | `leviculum-nrf/src/record_store.rs:607` | clamp the storm body to STORE_STORM_MAX_BYTES | size | none | - | - |  |
| 359 | `leviculum-nrf/src/record_store.rs:621` | stamp index bytes into a non-empty storm body | n, size | none | - | - |  |
| 360 | `leviculum-nrf/src/record_store.rs:631` | storm append outcome and sealed-page count | append result | none | - | - |  |
| 361 | `leviculum-nrf/src/radio_store.rs:84` | radio save queue full: the newest request wins | PENDING | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 362 | `leviculum-nrf/src/radio_store.rs:100` | the same for request_save_confirmed | PENDING | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 363 | `leviculum-nrf/src/radio_store.rs:131` | an unchanged page skips the erase and acks Durable | page bytes | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 364 | `leviculum-nrf/src/radio_store.rs:144` | erase and write retried 3 times 250 ms apart | flash result | none | - | - |  |
| 365 | `leviculum-nrf/src/radio_store.rs:167` | ack Durable or Lost to the waiting host | written | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 366 | `leviculum-nrf/src/boot_count.rs:123` | did the retained record survive | boot.prev | `reset_loop_quiet` | 300 | 1/1 |  |
| 367 | `leviculum-nrf/src/boot_count.rs:138` | erase the page first when the plan says so, then write | plan | `reset_loop_quiet` | 300 | 1/1 |  |
| 368 | `leviculum-nrf/src/button.rs:68` | no display state yet means the display is on | DISPLAY_ON_REQ | none | - | - |  |
| 369 | `leviculum-nrf/src/button.rs:70` | display power: on and any press is off; off and 2 s or more is on; shorter ignored | press duration, state | none | - | - |  |
| 370 | `leviculum-nrf/src/display.rs:66` | OLED chip from a stabilised status read: SH1106 or SSD1306 | I2C reads | `boot_smoke_t114` | 300 | 1/1 |  |
| 371 | `leviculum-nrf/src/display.rs:90` | first ACKing address wins; unclassifiable means SSD1306 | probe results | `boot_smoke_t114` | 300 | 1/1 |  |
| 372 | `leviculum-nrf/src/display.rs:142` | which driver runs: SSD1306 init, SH1106 or none ends the task | detected kind | `boot_smoke_t114` | 300 | 1/1 |  |
| 373 | `leviculum-nrf/src/display.rs:201` | seed display power on | - | `boot_smoke_t114` | 300 | 1/1 |  |
| 374 | `leviculum-nrf/src/display.rs:220` | apply only a changed power request; an error keeps the old state | request, display_on | none | - | - |  |
| 375 | `leviculum-nrf/src/display.rs:240` | no frame work while the display is off | display_on | none | - | - |  |
| 376 | `leviculum-nrf/src/events.rs:76` | the first announce from a new neighbour is forwarded to the immediate-announce trigger | NodeEvent | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 377 | `leviculum-nrf/src/media.rs:144` | no flash profile means both carriers on | page | `boot_smoke_t114` | 300 | 1/1 |  |
| 378 | `leviculum-nrf/src/media.rs:222` | runtime apply: store state, wake BLE, then queue the save | profile | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 379 | `leviculum-nrf/src/name.rs:170` | an undecodable boot name is empty | BOOT_GAP | none | - | - |  |
| 380 | `leviculum-nrf/src/name.rs:217` | runtime set or clear, then persist | name | none | - | - |  |
| 381 | `leviculum-nrf/src/transport_stats.rs:113` | stats due: re-arm from now (no catch-up), close the speed windows, emit | next, now | `lnode_remote_status` | 900 | 2/1 |  |
| 382 | `leviculum-nrf/src/iface_bytes.rs:149` | a window under 1 s keeps the previous speed | at_ms, now | `lnode_remote_status` | 900 | 2/1 |  |
| 383 | `leviculum-nrf/src/iface_bytes.rs:175` | an out-of-range interface index is ignored (also :182) | iface | none | - | - |  |
| 384 | `leviculum-nrf/src/identity.rs:117` | hashes not published yet answer None (REFUSE_BUSY) | HASHES | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 385 | `leviculum-nrf/src/identity.rs:62` | whether a PN hash exists (sizes the announce budget) | PN_HASH | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 386 | `leviculum-nrf/src/flash.rs:39` | an identity that does not encode returns Ok with nothing written | identity | none | - | - |  |
| 387 | `leviculum-nrf/src/flash.rs:43` | always erase then write, no read-compare | - | `boot_smoke_t114` | 300 | 1/1 |  |
| 388 | `leviculum-nrf/src/qspi.rs:497` | an Untouched part uses single-line FASTREAD/PP | part.quad | `boot_smoke_solarnode` | 300 | 1/1 |  |
| 389 | `leviculum-nrf/src/qspi.rs:518` | wake failure: release-failed, no device | QSPI result | none | - | - |  |
| 390 | `leviculum-nrf/src/qspi.rs:531` | ID read failure: read-failed, no device | QSPI result | none | - | - |  |
| 391 | `leviculum-nrf/src/qspi.rs:545` | an all-zero first ID gets one retry read | first | `boot_smoke_solarnode` | 300 | 1/1 |  |
| 392 | `leviculum-nrf/src/qspi.rs:558` | ID mismatch: no device; zeros are no-answer, else unexpected-part | jedec | none | - | - |  |
| 393 | `leviculum-nrf/src/qspi.rs:584` | StatusBit6 part with QE clear: write status and verify; failure, no device | status byte | `boot_smoke_solarnode` | 300 | 1/1 |  |
| 394 | `leviculum-nrf/src/qspi.rs:1145` | mount only after a successful identify | identify result | `boot_smoke_solarnode` | 300 | 1/1 |  |
| 395 | `leviculum-nrf/src/qspi.rs:1150` | over budget: leak the future, release by hand, log, keep booting | with_timeout | none | - | - |  |
| 396 | `leviculum-nrf/src/heap_census.rs:235` | refuse to boot when the budget exceeds HEAP_SIZE or links are fewer than BLE sessions | budget values | `boot_smoke_t114` | 300 | 1/1 |  |
| 397 | `leviculum-nrf/src/heap_census.rs:253` | largest allocatable block by binary search (feeds the live serve cap) | allocator | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 398 | `leviculum-nrf/src/heap_census.rs:119` | census due or requested by debug byte `c` | next, REQUESTED | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 399 | `leviculum-nrf/src/log.rs:170` | drop gated lines until DTR or 30 s and count them | RUNTIME_DRAIN_OPEN | `boot_smoke_t114` | 300 | 1/1 |  |
| 400 | `leviculum-nrf/src/log.rs:372` | the same gate for tracing events | RUNTIME_DRAIN_OPEN | `boot_smoke_t114` | 300 | 1/1 |  |
| 401 | `leviculum-nrf/src/log.rs:355` | tracing level filter at DEBUG | metadata | `boot_smoke_t114` | 300 | 1/1 |  |
| 402 | `leviculum-nrf/src/log.rs:99` | ring full overwrites the oldest bytes | wp, rp | none | - | - |  |
| 403 | `leviculum-nrf/src/log.rs:277` | a foreign tail magic zeroes and re-arms the persistent tail | retained magic | `reset_loop_quiet` | 300 | 1/1 |  |
| 404 | `leviculum-nrf/src/log.rs:313` | replay last boot's tail only with a valid magic, wrap handled, magic cleared | retained RAM | `reset_loop_quiet` | 300 | 1/1 |  |
| 405 | `leviculum-nrf/src/log.rs:340` | open the gate once and report the dropped count | swap | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 406 | `leviculum-nrf/src/rng.rs:130` | an empty request returns | dest | `boot_smoke_t114` | 300 | 1/1 |  |
| 407 | `leviculum-nrf/src/rng.rs:142` | a successful SoftDevice call supplies the bytes | return code | `boot_smoke_t114` | 300 | 1/1 |  |
| 408 | `leviculum-nrf/src/rng.rs:104` | take min(available, wanted); a lost race polls again | avail | `boot_smoke_t114` | 300 | 1/1 |  |
| 409 | `leviculum-nrf/src/vext.rs:63` | a second raise returns after rewriting the deadline | RAIL | `boot_smoke_t114` | 300 | 1/1 |  |
| 410 | `leviculum-nrf/src/vext.rs:79` | no rail, no warm-up wait | WARM_AT_MS | `boot_smoke_t114` | 300 | 1/1 |  |
| 411 | `leviculum-nrf/src/store_spike.rs:109` | spike build: stop at the first failing step (record-log candidate) | flash results | none | - | - |  |
| 412 | `leviculum-nrf/src/store_spike.rs:144` | spike build: the same for sequential-storage | flash results | none | - | - |  |
See also: [The firmware's host-test
seam](../concepts/firmware-host-test-seam.md) for the rule and how it is
gated.
