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
[below](#the-outbound-rounds-failure-arms-and-the-store-gates)), and 9
in a third ([below](#the-third-wave-and-the-end-of-the-propagation-node-list)),
which empties the propagation-node "none" tier of decisions: the 5 rows
left in it are marked "no decision" with the reason. A fourth wave
([below](#the-ble-wave-faults-only-a-misbehaving-peer-would-cause)) took
13 of the 35 BLE "none" rows to the host and marked 8 more as no
decision; a fifth ([below](#the-ble-wave-a-saturated-board-and-a-dark-carrier))
took the 14 left, so no BLE "none" row is a decision any more. A sixth
([below](#the-lora-wave-the-air-paths-faults-and-host-edges)) took 14 rows
of the LoRa section to the host, the ten dearest decisions of the air path
and four rig-reached arms of the same decisions, and marked 2 more as no
decision; 21 LoRa "none" rows were left, all in `announce.rs` and
`telemetry.rs`. A seventh
([below](#the-telemetry-and-announce-wave-the-reporter-and-the-scheduler-decide-on-the-host))
took 20 of them and two rig-reached arms of the same decisions to the host,
and found the twenty-first (244) host-reached already, so no LoRa "none"
row is left. An eighth
([below](#the-main-loop-wave-a-torn-frame-a-silent-reader-and-a-fault-at-a-chosen-instant))
took the ten dearest of the 27 main-loop and USB "none" rows and two
rig-reached arms of the same decisions to the host, leaving 17. A ninth
([below](#the-stores-and-boot-wave-a-torn-record-a-full-region-a-stalled-consumer))
ranked the 27 stores and boot rows with the 17 main-loop rows left, moved
ten decisions (17 of those rows and five rig-reached arms) and gave every
other "none" row its reason, so every row on this page now names its host
test, its rig cell, or why it has neither. Counted off the tables on
2026-10-11, 125 of the 412 rows carry host tests; the per-wave counts
above added to 98 before the ninth wave, and the tables are the count.
[Closing #413](#closing-413) is the summary.

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
| store errors | 30, 43, 56, 81 | 4 | 3 (30 and 81 in the third wave) |
| elsewhere | 6, 38, 45, 48, 68, 71, 77 | 7 | 6 (third wave) |

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

lnpnd shares the step (#583). Its outbound round in
`lnpnd/src/peering.rs` (`on_tick`, `start_round`, `on_link_established`,
`on_request_timeout`) hands the same events to `round_step` and reads the
same `OUTBOUND_ROUND_DEADLINE_MS`; its own deadline constant and its own
copies of the arms are gone, so a PC and a board end a round on the same
inputs with the same `PN_SYNC result=` value. The arms the step does not
cover (a failed `/offer` send, a failed transfer, the far side closing)
stay in each caller.

**Store mount.** Row 49 is a decision: a due announce is pushed one retry
out and spends nothing, so the store mounting inside the retry announces
at its end. It is now `AnnounceCadence::withhold` in
`leviculum-announce-policy`, beside the `defer` it calls; the log-once
latch beside it only chooses whether a line is printed. Rows 50 and 51
are early returns: one guard and a log line, no state.

**Store errors.** 56 was the silent return the observation below names:
a store that could not be walked at round start booked no backoff, so the
peer stayed due and a node with a broken store redialled it on every
pass. Since #583 the step answers a store failure with no round in flight
as `RoundStep::Backoff(RoundEnd::StoreError)`: the caller books the
peer's sync attempt (`Peer::book_sync_attempt`), logs `PN_SYNC
result=store_error` and leaves the peer Idle, on the board and in lnpnd
alike, and the same failure at link-up ends the round in flight as
`store_error`. The other three were left here as early returns; the third
wave read them again and moved two (30, 81), below.

## The third wave, and the end of the propagation-node list

The seven "elsewhere" rows and the store errors left above were read
again with the same test: the branch reads state, and its outcome shows on
the air or on the debug port. Each decision moved to the crate that
already owns its other half; the firmware calls the moved code with the
inputs it used inline.

| Row | What it decides | Verdict | Now |
|---|---|---|---|
| 6 | which peers a calendar jump refreshes and persists | decision: the 14-day cull reads `last_heard` | `PeerTable::refresh_after_calendar_jump`, `leviculum-lxmf/src/peering.rs` |
| 30 | a batch plan the store failed ends the round in flight | decision, the same one as the link-up store failure | the firmware hands `RoundEvent::StoreFailed` to `round_step` and logs its `RoundEnd`, as lnpnd does at link-up |
| 38 | a second sync envelope while a batch is held closes the sender's link | decision: link close on the air | `SyncBatch::arrival` (`Arrival`), `leviculum-nrf/sync-batch` |
| 43 | a stored body shorter than prefix plus stamp is not pushed | no decision: `PnStore::append` refuses any body under `MIN_BODY_LEN` (16 bytes plus the stamp, `leviculum-lxmf/src/propagation_store.rs`), and only that adapter writes message tags, so the guard is unreachable; the body missing is the message purged between pick and read, which the next pick skips by itself | stays |
| 45 | a push the core refuses: too large joins the bounded too-large list, anything else waits the gap | decision: the pick reads the list, the gap times the next push | `after_refused_push`, `remember_too_large`, `TOO_LARGE_CAP`, `leviculum-lxmf/src/active_delivery.rs` |
| 48 | a miner whose `generate` fails sends no result | no decision: `generate` fails only on cost 255 or a cancel; the miner never cancels, and a peer above `BOARD_REMOTE_PEERING_COST_MAX` (18) is refused at admission (`PeerTable::handle_announce`, `remote_peering_cost_max`), so the arm is unreachable | stays |
| 68 | `/get` over an unidentified link answers `NoIdentity` | decision: an error response on the air | `get_mailbox`, `leviculum-lxmf/src/propagation_node.rs`, which also derives row 69's mailbox |
| 71 | `/offer` over an unidentified link answers `NoIdentity` | decision: an error response on the air | `offer_remote` (`OfferRemote`) and `propagation_hash_of`, `leviculum-lxmf/src/propagation_node.rs`; the board's own copy of `propagation_hash_of` is gone |
| 77 | which sync outcomes count against the sender | decision: a count above zero throttles the peer and closes its link | `UploadOutcome::counts_against_sender`, `leviculum-lxmf/src/propagation_node.rs` |
| 81 | an append the flash did not take is not processed | decision: the client's retry is stored, not proven as a duplicate | `settle_message_op`, `leviculum-nrf/pn-store`, where the store's queue and the role's duplicate cache meet |

Row 81's tests run the field failures through the board's own adapter over
the simulated part: the flash refusing the append, the store not mounted
(the task refuses without touching the part; both arrive as the same
failed op), and a full region, which is no failure at all because the log
reclaims its oldest page, so that id stays remembered and its retry is the
proven duplicate it should be.

What is left in the propagation-node "none" tier decides nothing: 15 (the
far side closing the round's link, the step has nothing to add), 43 and 48
(unreachable, above), 50 and 51 (early returns while the store is
unmounted). The propagation-node decisions a rig cell already reaches (55
rows, the cell named in each) and the "none" rows of the other four
sections (35 BLE, 32 LoRa and radio, 27 main loops and USB, 27 stores and
boot) remain unreached by any host test; they are the next orders' list.
The BLE rows were the next order's, below.

## The BLE wave: faults only a misbehaving peer would cause

All 35 BLE "none" rows share the measure "none", so they were ranked by the
cell their branch would need, dearest first. Four tiers fall out:

| Tier | What the cell would need | Rows |
|---|---|---|
| A | a fault no stock peer produces: a SoftDevice error at a chosen moment, a link dying inside a two-read window, or a peer built to misbehave (wrong layout, wrong identity, refused writes, garbage framing) | 85, 97, 141, 142, 143, 103, 149, 105, 129, 127, 128, 134, 136 |
| B | a stalled peer and a burst on four links, so a queue or the drain table fills | 82, 87, 92, 114, 138, 123, 107, 131 |
| C | a board, a peer, and `--set-media ble=off` mid-session | 83, 91, 101, 116, 144, 146, 150 |
| D | nothing: unreachable, boot-only or HAL glue | 96, 98, 100, 106, 130, 135, 140 |

Within tier A the rows are in the order listed: the SoftDevice faults and
the race first, which no input from the air can cause, then the rows a
purpose-built peer could. The ten dearest decisions are tier A without
143, which on a second read is an early return: a keepalive the
SoftDevice refuses ends the outbound half, and the session ends through
the teardown every other end takes. Two more rows of tier A (134, 136)
are arms of the same classification as 127 and 128, and one row of tier C
(150) is an arm of the same match as 149, so they moved with them. Each
decision moved to the crate that holds its other half; the firmware calls
it with the inputs it used inline.

| Rows | What it decides | Now |
|---|---|---|
| 85 (86) | a route to a slot the drain table no longer claims is a miss, not a flood; a flood takes the claimed slots its mask does not skip | `resolve_fanout` (`Delivery`), `leviculum-nrf/ble-tx/src/registry.rs`, beside `plan_fanout` |
| 97 | the notify path disconnects on a torn packet unless the link is already gone | `abort_disconnects`, `leviculum-nrf/ble-tx/src/lib.rs`, beside `PacketTx` |
| 141, 142 | the write path disconnects on every abort, the first fragment's included | `abort_disconnects`, the same function, so the asymmetry below is pinned in one place |
| 103 | a refused advertisement waits 1 s, a served session none | `advertise_pass_pause_ms`, `leviculum-nrf/ble-tx/src/admission.rs` |
| 149 (148, 150, 151) | a dial or a failed scan waits the dial backoff base, an empty window or a dark carrier goes straight back to the gate | `scan_pass_pause_ms` (`ScanPass`), `leviculum-nrf/ble-tx/src/admission.rs`; the firmware's `CENTRAL_RETRY_BACKOFF_MS` is gone |
| 105 | a garbage frame amid a reassembly abandons the head, so the next tail cannot splice onto it | `BleDefragmenter::process_or_abandon`, `leviculum-core/src/framing/ble.rs` |
| 129 | a dial that reads our own identity is a self link | `judge_read_identity` (`ReadIdentity`), `leviculum-nrf/ble-tx/src/admission.rs` |
| 127, 128, 134, 136 | what a failed setup stage says about the peer: a wrong layout or identity is not Columba, a refused handshake will not handshake, a refused subscription and every silent stage are the air's | `dial_stage_failure` (`DialStage`, `StageError`), `leviculum-nrf/ble-tx/src/admission.rs`, beside 579's `DialFailure` |

The census text of rows 127, 128 and 134 predates 579, which gave each
stage a `DialFailure`; the classification is what moved. Mapping the
SoftDevice's own error types onto `StageError` stays in the firmware as
HAL glue, as lnsd maps BlueZ errors in `connect_failure`. lnsd does not
call `dial_stage_failure` or `process_or_abandon` yet: its setup tags
each stage inline with the same answers, and its receive path parks an
orphan tail between `process` and `abandon` (#434).

Tier D and row 143 are marked in the table with their reason. What is
left in the BLE "none" tier is tier B and tier C without 150: 14 rows, all
reached only through a queue filled on purpose or a carrier switched
mid-session. The next wave took them.

## The BLE wave: a saturated board and a dark carrier

Tier B is the other field case: a stalled phone and a burst on four links
fill the 8-deep peer-event queue, a link's outbound queue, `BLE_INCOMING`
and the drain table, and each full arm decided inline with its own rule.
#388 was a heap failure under that load, and these arms decide what is lost
first, so each queue class now asks one function in
`leviculum-nrf/ble-tx/src/saturation.rs`, with the queue's occupancy
(`Fill`) and the arriving item's class, and the module's head states the
order a saturated board sheds in: a stalled peer's own link queue first,
then the core's outbound queue (the one that answers `BufferFull`), then
the inbound queue, then the peer events, and no arm ends a live session.
The tests run each arm with the board's real `QUEUE_DEPTH` and
`MAX_LINKS` from `leviculum-heap-budget`.

| Rows | What it decides | Now |
|---|---|---|
| 82 | a peer event arriving at a full queue is dropped, `Up` or `Lost`: the oldest pending wins | `peer_event_verdict` (`PeerEventKind`) |
| 87, 123 | a packet arriving at a full link queue is dropped on that link, from the fan-out or a displacement's move alike; the session goes on | `link_queue_verdict` (`LinkItem`), two call sites |
| 91, 92 | a dark carrier swallows an outbound packet as `Ok` however full the queue; a lit one with a full queue answers `BufferFull` | `outbound_verdict` (`Outbound`) |
| 114, 138 | a completed packet arriving at a full `BLE_INCOMING` is dropped and counted, on both roles | `incoming_verdict`, one firmware call site (`offer_incoming`) for both roles |
| 107, 131 | a connection the drain table refuses ends on either role and books nothing against the peer | `drain_table_refusal` (`ClaimRefused`), two call sites |

Rows 114 and 138 were silent: no line and no counter. They now count in
`BLE_COUNTERS`' new last field `rx_full=` (`BLE_RX_QUEUE_FULL`); the other
arms already named themselves (`BLE_PEER_EVENT_DROPPED`,
`BLE_TX_FANOUT_DROP`, the displacement line's `dropped=`, `IFACE_FULL`,
`BLE_DRAIN_TABLE_FULL`). Row 107 changed in one respect: the peripheral
left the refused connection to its drop, which disconnects it a moment
later (`nrf-softdevice`'s `Connection::drop`); it now disconnects at the
refusal, as the central always did.

Tier C is the operator's `lnflash --set-media ble=off` on a live link.
Every path that read `media::ble_active` inline now asks one
`CarrierState` (`leviculum-nrf/ble-tx/src/carrier.rs`) for its
`CarrierPath`: the two gates (101, 146), the advertisement and the scan
in flight (the waiter, 83, and 150), the sessions of both roles (116,
144) and the outbound path (91). On carrier off each stands down
(`StandDown`): advertising stops, scanning stops, a session ends with
`BLE_CARRIER_DROP` and a disconnect, the outbound packet is `Ok` and
counted in the interface's `DropRun`, and none books a pause. The waiter
returns the `StandDown` it resolved with, so a session disconnects on the
verdict and not on its own reading. Rows 88 and 89, rig-reached, read
`CarrierState::is_on` as well. No path needed the task restructured.

What is left in the BLE "none" tier decides nothing: tier D (96, 98, 100,
106, 130, 135, 140: HAL glue, unreachable branches and boot-only returns)
and row 143 (an early return), each marked in the table with its reason.

## The LoRa wave: the air path's faults and host edges

The three open LoRa P1s of the week (#431, #438, #433) were each measured
on the rig because nothing on the host could reach them. Of the 34 LoRa
"none" rows, 2 had moved with the first wave (160, 173); the other 32 were
ranked by the cell their branch would need, dearest first:

| Tier | What the cell would need | Rows |
|---|---|---|
| T | the three-radio, 1800 s delivery-promise topology plus an induced fault, a moving fix, or 33 announcers | 208, 211, 209, 222, 237, 238, 242, 239, 231, 232, 233, 236, 230, 234, 235, 224, 225, 241, 244, 217, 213 |
| R | a radio that fails a command at a chosen moment | 163, 191, 180, 168, 195 |
| H | one board and a host that pushes an edge: a config into a turn with a second window, a dark carrier, a silent config, two spacing sets in one turn | 165, 154, 153, 161, 152, 171 |

Tier T is dearer, and it is not on the air path: its rows are the
telemetry reporter's and the announce scheduler's, and their other halves
are `leviculum-telemetry-policy` and `leviculum-announce-policy`, not the
duty, channel-access or radio crates this wave was for. They are the next
wave. Of tiers R and H, 168 (a boot-only return) and 195 (a guard no caller
reaches) decide nothing, which leaves nine decisions; the tenth is the
dearest rig-reached air-path row, 179, the CSMA backoff window of
`bench_dual_pair_fast` (four radios). Four rig-reached rows are arms of the
same decisions and moved with them: 164 with 163, 178 with 180, 192 and 193
with 191. Each went to the crate that holds its other half; the firmware
calls it with the inputs it used inline.

| Rows | What it decides | Now |
|---|---|---|
| 163 (164) | only a frame that left the air is charged; a refused frame stops the packet, the second split half neither keyed nor charged; the packet counts as sent only if every frame went out | `PacketTx` (`FrameStep`), `leviculum-nrf/queue-budget`, beside the duty hold the charge feeds |
| 191 (192, 193) | a transmit ends in a forced standby unless `TxDone` was latched; the wait for it has a 100 ms floor | `tx_exit` (`TxExit`), `tx_left_the_air`, `tx_wait_ms`, `leviculum-core/src/sx126x.rs`, beside `tx_timeout_ms` |
| 180 (178) | the three CAD outcomes go through one gate entry; a key-up forced by CAD errors goes straight to the next turn, the others open the ack window | `ChannelAccess::cad` (`CadOutcome`), `after_key_up` (`AfterKeyUp`), `leviculum-nrf/channel-access` |
| 179 | a backoff is listened through in whole CSMA slots, at least 1 ms and at most 10 s | `ChannelAccess::backoff_window_ms`, `listen_window_ms`, the same crate |
| 165 | a window entered with a config waiting arms nothing | `window_entry` (`WindowEntry`), `leviculum-nrf/rx-arming` |
| 153 | a dark carrier answers `Ok` however full the queue is; a lit one with a full queue refuses with the bound that bound | `outbound_verdict` (`Outbound`), `leviculum-nrf/queue-budget`, beside `QueueBudget` |
| 154 | a channel refusal after a reservation hands the room back | `QueueBudget::channel_refused`, the same crate |
| 161 | a silent config restarts the mute lease, any other clears it | `MuteLease::apply`, `leviculum-nrf/mute-lease` |
| 152, 171 | a second spacing while one is unread is refused and overwrites nothing; the turn puts a waiting one in force before the next key-up | `SpacingRequest`, `leviculum-nrf/tx-spacing`, beside `TxSpacer`; it replaces the depth-1 `LORA_TX_SPACING` channel |

Row 177's clamp now calls `listen_window_ms` too; its credit floor stays
inline, so the row keeps its rig cell. The CAD dispatch also folded the
three transmit call sites of the CAD match into one, with the same
`[LORA_CAD]` and `[LORA_CSMA_TX]` lines.

The host's RNode interface (`leviculum-std/src/interfaces/rnode.rs`) calls
none of the ten. It does not key a radio: the RNode firmware runs the CAD,
the transmit and the split frames behind the KISS port, and the host's own
half of the air, the acquisition jitter and the duty hold, already goes
through `ChannelAccess` and `DutyHolds`, the same crates.

## The telemetry and announce wave: the reporter and the scheduler decide on the host

Tier T of the LoRa ranking, the 21 rows the sixth wave left, are the
telemetry reporter's and the announce scheduler's. Each needed the
three-radio, 1800 s delivery-promise topology plus an induced fault, a
moving fix or thirty-three announcers; #344 spent three weeks and
twenty-three comments of rig work on the reporter's arms. The announce
arms are also where the open cadence decision will land (see below), and
they are host-tested before that policy changes under them, not after.

Each went to the crate that holds its other half, in a module of its own:
`leviculum_telemetry_policy::reporter` beside `SendPolicy` and
`retry_step`, and `leviculum_announce_policy`'s `scheduler` module beside
`AnnounceCadence` and its `withhold`. The firmware calls them with the
inputs it read inline, and every log line is unchanged. Two rig-reached
rows are arms of the same decisions and moved with them: 216 with 217
(one hand-over for both inbound queues) and 227 with 232 (the debt the
rewrite pays).

| Rows | What it decides | Now |
|---|---|---|
| 208 | a neighbour is new once per boot; the thirty-third displaces the first, round robin | `NeighbourTable`, `leviculum-nrf/announce-policy` |
| 209 | movement evidence: none for a fixed pin, without a receiver, for `valid=false`, without latitude or longitude | `movement_fix` (`GnssReading`), the same crate; it builds the fix through `Fix::from_degrees`, the one conversion the reporter uses too |
| 211 | the movement proof is sampled once per interval, booked from the sample taken | `MovementSampler`, the same crate; the resume itself was `AnnounceCadence::note_fix` already |
| 213 | the own slot is polled before the destination is checked, so a board without one spends the slot | `own_announce`, the same crate |
| 217 (216) | the newest inbound frame replaces an unread one | `deliver_newest`, `leviculum-nrf/telemetry-policy` |
| 222 | a flash write is tried three times, 250 ms after each failure, then Lost | `FlashSave` (`SaveStep`), the same crate |
| 224, 225 | a sensor fix needs latitude and longitude; its accuracy is HDOP times 5 m and absent without HDOP | `Fix::from_degrees`, `accuracy_m`, `HORIZONTAL_UERE_M`, the same crate |
| 230, 231 | no position source sends nothing, before the key chase; awaiting a key asks for a path once a minute | `key_step` (`KeyStep`), `KEY_REQUEST_INTERVAL_MS`, the same crate |
| 232 (227) | a hash-only target owes one rewrite of its record, paid on the first tick that holds the key | `KeyRecordDebt`, the same crate |
| 233 | a retry debt without its payload is forgotten | `owed_retry`, the same crate, beside `retry_step` |
| 234, 235 | a sensor fix only in presence state Fix; the location is none, the pin, or the sensor | `report_position`, `location_source` (`LocationSource`), the same crate |
| 236 to 239, 242 | no calendar time withholds `no-clock` and books nothing; every failure after the pre-report announce books it; no path asks and names `no-path` or `iface-offline` | `report_time`, `path_withhold`, `Withhold`, `SendPolicy::note_withheld`, the same crate |
| 241 | the cadence records a position only if the report carried it | `SendPolicy::reportable_position`, the same crate |

Row 244, only the configured target may request a report, needed no move:
the reporter passes the target as the allowed sender into
`leviculum_lxmf::telemetry::screen_telemetry_request`, whose integration
tests (`leviculum-lxmf/tests/telemetry_request.rs`) already cover no
target, a foreign sender and a target whose key is not held. The census
listed it because the target and its identity are read inline.

Two reads moved earlier in the tick and change nothing on the air: the
target's identity is looked up on every tick rather than only in
awaiting-key, and the path route is looked up before `has_path` rather
than after it fails. Both are reads of the node's tables.

### Where the cadence decision plugs in

How fast a board announces when still and when moving, and what counts as
moving, is still open (300 s mobile, about an hour stationary, a motion
sensor instead of a GPS delta). When it is decided it changes two
functions and no wiring: the intervals are
`AnnounceCadence::configured_interval_ms`
(`leviculum-nrf/announce-policy/src/cadence.rs`), and the evidence is
`movement_fix` in the scheduler module, whose input is the board's fixed-pin
flag and its `GnssReading`. A motion sensor becomes a second input of
`movement_fix`; `MovementSampler` already decides when it is read, and
`AnnounceCadence::note_fix` what a proven movement buys.

## The main-loop wave: a torn frame, a silent reader, and a fault at a chosen instant

The main-loop and USB section holds the arms behind the longest
unreproduced finding, #359: a board dark after a commanded reset, twice on
2026-08-30/31 and never since under census conditions (40 of 40 and 38 of
38 resets came back). Its 27 "none" rows were ranked by what a cell would
have to make happen, dearest first:

| Tier | What the cell would need | Rows |
|---|---|---|
| U | a USB event at a chosen instant: a disconnect or a host that stops writing in the middle of a frame, a host that holds DTR up and stops reading until the queue behind the writer fills, a debug host that does the same, no debug host for the first 30 s | 329, 330, 285, 299, 301 |
| X | a panic or a HardFault at a chosen instant, and the reset and the boot after it | 337, 335, 338, 336, 334 |
| F | a store or consumer fault at a chosen instant: a stalled LoRa task when a second config arrives, a lost flash write on a `lora=off` boot, a lost allow-list write, a storm into a busy store, a spacing while one is unread | 308, 307, 294, 323, 327 |
| H | one board and a host edge no cell sends: a frame, or a boot state | 318, 320, 306, 315, 322, 300, 263, 272, 269, 276, 281, 282 |

Tiers U and X are the ten, and all ten are decisions. Each went to the
crate that holds its other half: the USB side to `leviculum-usb-policy`,
beside the commanded-reset plan and the frame timeout it applies, and the
post-mortem records to a `postmortem` module of `leviculum-boot-trace`,
beside the breadcrumb record that shares `.retained` with them. None is an
envelope decision: the envelope's own classifier and answers were
host-reached already, which is why the census lists none of them. Two
rig-reached rows are arms of the same decisions and moved with them: 312
with 330 (the timeout a read waits) and 298 with 299 and 301 (the DTR gate
and the first host opening the drain).

| Rows | What it decides | Now |
|---|---|---|
| 329, 330 (312) | a read error ends the session in or out of a frame and the next starts with an empty deframer; a timeout drops only an open frame; the read waits 500 ms inside a frame and an hour outside | `reader_step` (`ReadOutcome`, `ReaderStep`), `read_timeout_ms`, `leviculum-nrf/usb-policy` |
| 285 | a packet the full outgoing queue refuses is dropped uncounted; a queued one counts its unframed bytes | `offered` (`Offered`), `OUTGOING_DEPTH`, the same crate; the firmware's channel depth is the constant |
| 299, 301 (298) | the debug port is written only while DTR is up; the first host, or 30 s without one, opens the runtime-drain gate; a debug write that does not land within 50 ms ends the drain until the next wake | `drain_wake` (`DrainWake`), `drain_continues` (`DebugWrite`), `HEADLESS_DRAIN_OPEN_MS`, `DEBUG_WRITE_WITHIN_MS`, `DEBUG_POLL_MS`, the same crate |
| 335, 337 | a fresh record is replayed by one boot and downgraded; the debug-port query reads it fresh or replayed; any other word, the other record's magics included, is no record | `RecordMagic` (`take`, `peek`), `PANIC_RECORD`, `HARDFAULT_RECORD`, `leviculum-nrf/boot-trace` (`postmortem`) |
| 334 | an overlong panic message keeps its last 1024 bytes and is read back in order, the PC and PREGION tail whole | `ring_slot`, `ring_window`, `PANIC_MSG_MAX`, the same module |
| 336, 338 | a fault with no LED armed resets at once, one with an LED blinks 25 times first; the GPIO registers the blink writes | `fault_exit` (`FaultExit`), `led_registers`, the same module; one blink function serves both handlers |

The reader's host tests drive the bytes of a reset as #359 could have
met it: the envelope reset frame, HDLC-framed, torn in half by a
disconnect or by a host that stops writing, and written whole again. The
torn half never reaches the classifier and the retry reads as one reset.
A positive control in the same file feeds the deframer the same bytes
without the step, and the half closes as a garbage frame on the retry's
opening flag and swallows the reset with it; turning the disconnect arm
into `Idle` turned the torn-reset test red. No capture of the two dark
boards survives: nothing under `/home/lew/ci` or `/home/lew/rig-run`
outside git objects carries a date of 2026-08-30 or 08-31, so the sequence
is the one the code admits, not one the field recorded.

One task is gone. The headless opening of the runtime-drain gate was a
timer task of its own; the debug writer, which wakes at least every 100 ms
anyway, now asks `drain_wake` with the uptime and opens the gate on the
first wake at or past 30 s. The gate can open up to one poll period later
than the timer did, and the `[LOG_GATE]` line is unchanged.

## The stores and boot wave: a torn record, a full region, a stalled consumer

The last section with "none" rows decides what a board does when flash is
full, a record is torn, a write is refused, the clock is unseeded or a part
does not answer: the field events behind #380 (a Pocket V2 on battery that
restarts in the field) and #398 (a board that learns its clock from the
air), each read off a board that had already rebooted. Its 27 "none" rows,
the 12 the main-loop wave left in tier H and the 5 in tier F were ranked by
what a cell would have to make happen, dearest first, one line a row:

| Rank | Row | What a cell would need |
|---|---|---|
| 1 | 356 | a store region holding another image's bytes at boot, and its format erase refused |
| 2 | 352 | the same refusal: no store this boot |
| 3 | 357 | a count scan whose read of internal flash fails |
| 4 | 360 | an append refused after its header, mid-record |
| 5 | 351 | the SoftDevice refusing one operation three and four times running |
| 6 | 364 | the same refusals on the radio-profile page |
| 7 | 307 | a `lora=off` boot, a radio config, and its page write lost |
| 8 | 308 | a LoRa task stalled with a config in its slot, then the same and a different config |
| 9 | 294 | an allow-list write lost |
| 10 | 349 | a store that failed to mount, then a PN write |
| 11 | 402 | a debug host that reads nothing for 8 KiB of log |
| 12 | 323 | a storm request while one runs |
| 13 | 350 | a storm request unmounted, empty or oversize |
| 14 | 358 | a storm body larger than its buffer |
| 15 | 359 | a storm read back by index |
| 16 | 327 | a TX spacing while the LoRa task has not read the last |
| 17 | 269 | an unseeded board and a fix whose date the seam refuses |
| 18 | 340 | a receiver dropping its fix: a void RMC after a valid one |
| 19 | 341 | a GGA without a fix that still carries a position |
| 20 | 392 | a Solar Node whose QSPI part answers silence or another id |
| 21 | 389 | a QSPI peripheral error on the wake command |
| 22 | 390 | the same on the ID read |
| 23 | 395 | a QSPI transfer that hangs past the boot budget |
| 24 | 306 | a radio config with a bandwidth the SX1262 has no code for |
| 25 | 315 | an invalid legacy config frame |
| 26 | 318 | a fixed position on a board without a reporter, and one with a lost write |
| 27 | 272 | a fixed position reaching the main loop with no reporter |
| 28 | 263 | a persisted fixed position at boot |
| 29 | 320 | a node name sent in the boot window before the identity is known |
| 30 | 322 | a BLE TX gap |
| 31 | 276 | LoRa traffic after a runtime `lora=off` |
| 32 | 281 | a telemetry tick with a fix value present |
| 33 | 282 | a telemetry tick in each presence state |
| 34 | 300 | the debug-port command bytes |
| 35 | 386 | an identity without its private key to save |
| 36 | 379 | an undecodable boot name |
| 37 | 380 | a runtime name set and cleared |
| 38 | 383 | a byte count for an unregistered interface |
| 39 | 369 | button presses of chosen lengths |
| 40 | 368 | a display task before any power request |
| 41 | 374 | a display power request, and an I2C error applying it |
| 42 | 375 | a frame tick with the display off |
| 43 | 411 | the `store-spike` build with a failing step |
| 44 | 412 | the same for its second candidate |

No row needs a SoftDevice other than the one the image was built for: the
boot does not compare a version banner; `assert_sd_fits_below_retained`
(`leviculum-nrf/src/ble/mod.rs`) checks the RAM the configuration asks
for, and a mismatched SoftDevice fails `Softdevice::enable` before any
decision of the firmware's own. Nor does a profile from an older firmware:
every page record carries a format version, and
`leviculum_core::media_profile_store::decode_media_profile` and
`radio_config_store::decode_radio_config` read a version N−1 record as no
record (`a_wrong_magic_or_version_is_no_record`,
`wrong_version_returns_none`), so the boot falls back to both carriers and
`eu_medium` (rows 377 and 256, rig-reached). Both were host-reached before
this wave.

Ten decisions were moved, the dearest that are decisions, each into the
crate that holds its other half, firmware calling, every log line
unchanged. Rows 357 and 294 rank high but are no decision (below). The
debug-port commands (300) rank low and went with them, because the move
was one function. Five rig-reached rows are arms of the same decisions and
moved with them: 354 with 360, 391 with 392, 403 and 404 with 402, and 249,
the replay of the tail, which all three binaries carried as a copy each.

| Rows | What it decides | Now |
|---|---|---|
| 356, 352 | a region no log of ours wrote is formatted, ours is mounted as it was and counted; a refused format, or a region that does not fit the part, leaves no store this boot | `mount_or_format` (`MountState`, `Mounted`), `leviculum-nrf/record-log` (`board`) |
| 360 (354) | a failed append counts a sealed page only if the page had room before and none after: an append refused mid-record seals, one refused before its first write or at a reclaim does not | `sealed_a_page`, the same module |
| 351 | a refused flash operation is tried four times, 50, 100 and 200 ms apart, with no wait after the last | `retry_after`, `FLASH_ATTEMPTS`, `FLASH_BACKOFF_MS`, the same module |
| 350, 358, 359 (323) | a storm is refused unmounted, while one runs, for zero records or a body no page holds; each synthetic record is clamped to its buffer and carries its index | `storm_admits`, `storm_record`, the same module |
| 364 | the radio page is tried three times, 250 ms after each failure, then Lost | `FlashSave`, `leviculum-nrf/telemetry-policy`: the loop the telemetry page already ran, now one loop for both pages |
| 307, 308 | a boot without LoRa takes a radio config to the page and answers Stored or StoreFailed; behind a full slot, a repeat of the queued config waits for its apply and a different one gets one grace send | `config_route` (`ConfigRoute`), `stored_answer`, `full_slot` (`FullSlot`), `leviculum-nrf/usb-policy` |
| 402, 403, 404 (249) | a writer that laps the runtime ring moves its reader to the oldest byte; any word but the tail's magic re-arms the tail; a boot replays nothing without it, the bytes written before a wrap, the last ring after one, and a torn write position still reads inside the ring; the replay drops the leading partial line | `lapped_reader`, `must_rearm`, `tail_window`, `replay_lines`, `TAIL_MAGIC`, `leviculum-nrf/boot-trace` (`tail`) |
| 340, 341 | a valid RMC sets position, time and movement, a void one clears only the time; a GGA always sets the satellite count, and position, altitude and HDOP only with a fix | `GnssFix` (`fold_rmc`, `fold_gga`), `leviculum-nrf/gnss-time` (`fix`); the firmware re-exports the type |
| 269 | the seam's answer to a GNSS candidate: an accepted seed closes the gate, a refused one leaves it open | `SeedGate::settle`, the same crate |
| 392 (391) | a silent first JEDEC read earns a second; two silent reads are no-answer and ask the bit-banged second opinion; any other wrong id, all ones included, is unexpected-part | `wants_second_read`, `identify` (`Identified`), `leviculum-nrf/qspi-boot` |
| 300 | each of `p`, `s`, `c` runs once per packet wherever it appears; every other byte is ignored | `debug_commands` (`DebugCommands`), `leviculum-nrf/usb-policy` |

The store tests drive the simulated part the record log was built against
(`leviculum_record_log::sim::SimNor`): a power cut in the middle of a
record, a refused erase, a region filled to its last page. A positive
control in each of the two largest moves: answering `Formatted` for every
mount turned `a_reboot_finds_its_own_records` and
`a_record_torn_by_a_power_cut_is_not_counted` red, and treating every full
slot as a different config turned
`a_retry_of_the_queued_config_waits_for_its_apply_while_the_task_stalls`
red.

The radio store's loop changes nothing a capture can see: it ran three
tries 250 ms apart with a wait after the last before, and `FlashSave` runs
the same, so the `persist write failed, attempt n/3` lines are unchanged.

The fold's tests found a defect, fixed in its own commit: nmea0183's
`as_f64` already returns south and west as negative, and the firmware
negated them a second time, so a board south of the equator or west of
Greenwich reported a mirrored position. The rig sits north and east of
both, which is why no capture ever showed it.

Of the 44 rows, 17 moved, 23 are marked no decision with their reason,
and 4 are decisions left: 306 (a bandwidth `parse_radio_config` admits and
the SX1262 has no code for), 320 (a name sent before the board can state
it), 369 and 374 (the display's power toggle). None of the four is behind
a field event; 306 is reachable by a host and is the next candidate.

## Closing #413

The census walked `leviculum-nrf/src` at `ad7eaa2e` on 2026-10-10 and found
580 decisions on the board: 168 were already a call into a host-built
crate, and 412 were listed, 147 of them reached by no rig cell. After nine
waves, counted off the tables on this page on 2026-10-11, 125 of the 412
carry host tests: 124 moved into a seam crate with a host test per arm,
and one (244) was found tested already. Of the 287 without one, 245 are
driven by a rig cell named in their row, the cell that exercises them on
every hardware run, and were never what this issue was for. The other 42
read "none": 35 are no decision, each with its reason in the row (glue
over a host-tested decision, an early return, an unreachable guard), 3 are
rig-only by nature (a hung QSPI transfer, the `store-spike` instrument
build), and 4 are decisions left without a host test or a cell: 306, 320,
369 and 374, none of them behind a field event. Every decision on the
board now says where its host test is or why there is none. The move
also found two defects nobody had reported: a southern or western GNSS
fix reported mirrored (fixed with this wave), and the serial TX counter
counting packets that never left the board (an observation below, its own
issue).

## Observations made during the walk

Read from the code, not measured on a board; each is a candidate for its
own issue, not a finding of this page.

- A miner whose `generate` fails sends no result, so `mining_peer` is never
  cleared and every later sync pass skips (`leviculum-nrf/src/pn.rs:639`).
  The third wave found the arm unreachable (row 48).
- A store error in `start_round` returns before any backoff is booked, so
  the peer stays due on every pass (`leviculum-nrf/src/pn.rs:2606`).
  Fixed by #583: it now books the backoff (row 56).
- The offerable-entries scan is written twice, once where the link comes
  up and once where the round starts: `leviculum-nrf/src/pn.rs:1546`
  and `leviculum-nrf/src/pn.rs:2606`.
- A full `BLE_INCOMING` drops a completed packet with no line and no
  counter, on both GATT roles (`leviculum-nrf/src/ble/columba.rs:1008`,
  `leviculum-nrf/src/ble/columba.rs:2773`). Since the fifth wave it is
  counted, `BLE_COUNTERS rx_full=`.
- The central disconnects on any failed fragment, the first included,
  where the peripheral disconnects only on a torn packet
  (`leviculum-nrf/src/ble/columba.rs:2879`,
  `leviculum-nrf/src/ble/notify.rs:128`). Both rules are now one function,
  `leviculum_ble_tx::abort_disconnects`, whose tests pin the difference.
- A packet forced out after CAD errors skips the ack window and the
  `channel_released` that follows it, so the next acquisition owes no fresh
  jitter, unlike a packet forced out of a busy channel. The sixth wave pinned
  the behaviour as it is (`leviculum_channel_access::after_key_up`); whether
  the skip is wanted is a question for its own issue.
- The serial interface counts a packet's bytes when the outgoing queue takes
  it (`leviculum_usb_policy::offered`), and the transport writer drops a
  queued packet when no host reads (`leviculum-nrf/src/usb.rs`, "TX dropped,
  no host reading"), so the serial TX counter includes packets that never
  left the board. Found in the eighth wave; whether to uncount them is a
  question for its own issue.
- The persistent-log replay drops its first line on every boot, also when
  the previous boot never filled the ring and that line is whole
  (`leviculum_boot_trace::tail::replay_lines`, pinned as it is by
  `the_leading_partial_line_is_dropped`); `tail_window` knows whether the
  ring wrapped and could tell it. Found in the ninth wave.
- A GGA without a fix reaches the fold only from a receiver that still
  reports a position: nmea0183 parses a GGA with empty latitude and
  longitude fields to nothing at all (`vendor/nmea0183/src/gga.rs`), so
  on a receiver that blanks them the satellite count of a no-fix sentence
  is never published. Found in the ninth wave.

## The census

### Propagation node (`pn.rs`)

81 decisions no host test reached on 2026-10-10; 29 more were already a call into a host-built crate and are not listed.

| # | Where | What it decides | State it reads | Cheapest rig cell | s | radios/boards | Host test |
|---|---|---|---|---|---|---|---|
| 1 | `leviculum-nrf/src/pn.rs:312` | persisted PN costs or the compiled defaults | load_pn_config result | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 2 | `leviculum-nrf/src/pn.rs:335` | merge a `--stamp-cost` frame: KEEP field keeps the configured value, else the wire value; persisted | wire frame, CONFIGURED_* atomics | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 3 | `leviculum-nrf/src/pn.rs:879` | walk the workblock at cost 0 only when some peer demands a value | max_peer_min_cost | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 4 | `leviculum-nrf/src/pn.rs:983` | loop wake deadline: now on a parked grind, +25 ms with work pending, else the earliest of announce/maintenance/stats/sync/outbound | grind, work, sync_batch, pending_ops, next_* | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 5 | `leviculum-nrf/src/pn.rs:1021` | seed the wall clock only when implausible and the input is non-zero | has_plausible_wall_clock, unix_secs | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 6 | `leviculum-nrf/src/pn.rs:1053` | after a clock jump refresh and persist every peer heard at or before the old time | peer table | none | - | - | `peering::tests::a_calendar_jump_refreshes_the_peers_heard_on_the_old_calendar`<br>`peering::tests::a_seed_that_does_not_move_the_calendar_forward_refreshes_nothing` |
| 7 | `leviculum-nrf/src/pn.rs:1092` | a PN announce with our name hash and not our own goes to peering | announce name_hash, destination | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 8 | `leviculum-nrf/src/pn.rs:1105` | any other announce, or PathFound, triggers active delivery | event kind | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 9 | `leviculum-nrf/src/pn.rs:1130` | an inbound link to our destination gets ResourceStrategy::AcceptApp | is_initiator, destination | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 10 | `leviculum-nrf/src/pn.rs:1145` | a request timeout on the outbound round ends it (`offer_timeout`) and closes the link | outbound link_id/request_id | none | - | - | `sync_round::tests::only_the_rounds_own_offer_timing_out_ends_it` |
| 11 | `leviculum-nrf/src/pn.rs:1174` | every link packet on our destination is queued as an upload | owns_link | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 12 | `leviculum-nrf/src/pn.rs:1187` | an accepted resource counts as inbound transfer only on a validated peer link (feeds the `/offer` gate) | validated_links | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 13 | `leviculum-nrf/src/pn.rs:1220` | outbound resource done only on the last segment; a failure ends the round | segment_index, total_segments | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 14 | `leviculum-nrf/src/pn.rs:1236` | route a request on our destination by path to `/get` or `/offer` work | destination, path | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 15 | `leviculum-nrf/src/pn.rs:1284` | outbound link closed by the far side: drop the round, peer Idle, no PN_SYNC line | outbound link_id | none | - | - | no decision: the step has nothing to add |
| 16 | `leviculum-nrf/src/pn.rs:1296` | delivery proof for the active push: purge from flash, clear, set the 1 s gap | active.packet_hash | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 17 | `leviculum-nrf/src/pn.rs:1311` | failed push: decrement attempts, give up at 0, else resend | active.attempts_left | none | - | - | `active_delivery::tests::a_failed_push_resends_until_the_last_attempt_and_then_gives_up` |
| 18 | `leviculum-nrf/src/pn.rs:1350` | a link is ours when its destination is our PN destination (guards the upload/request arms) | node.link() | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 19 | `leviculum-nrf/src/pn.rs:1398` | capture the identity keys into a peer that lacks them | peer keys, storage | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 20 | `leviculum-nrf/src/pn.rs:1405` | PeerChange mapping: Added/Updated persist and sync now, Dropped removes from flash, Declined logs on TableFull | PeerChange | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 21 | `leviculum-nrf/src/pn.rs:1502` | act on link-up only for the outbound round's link | outbound link_id | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 22 | `leviculum-nrf/src/pn.rs:1504` | identify_link failed: end the round and close | node result | none | - | - | `sync_round::tests::a_link_that_cannot_identify_ends_the_round_and_keeps_the_peer_deferred` |
| 23 | `leviculum-nrf/src/pn.rs:1516` | no peering key at link-up: end `no_key` and close | peer.peering_key | none | - | - | `sync_round::tests::an_identified_link_to_a_peer_without_a_key_ends_no_key_and_leaves_it_due` |
| 24 | `leviculum-nrf/src/pn.rs:1525` | no link MDU: end `link_gone` and close | node.link() | none | - | - | `sync_round::tests::a_link_gone_before_the_offer_is_sized_ends_link_gone`<br>`sync_round::tests::an_identified_link_with_a_key_offers_against_its_own_mdu` |
| 25 | `leviculum-nrf/src/pn.rs:1546` | offerable entries: sequence above the cursor, sorted; a store error ends `store_error` (twin at 2613) | store, cursor | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 26 | `leviculum-nrf/src/pn.rs:1569` | empty or absent offer plan: conclude the round and step the cursor to the target | plan | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 27 | `leviculum-nrf/src/pn.rs:1591` | send `/offer` (RequestSent) or end `request_failed` and close | node result | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 28 | `leviculum-nrf/src/pn.rs:1623` | match an offer response to the outbound link and request id | outbound | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 29 | `leviculum-nrf/src/pn.rs:1662` | offer reply mapping: Send, Concluded, Backoff (defer and close), Unpeer (remove and close), RemineKey (clear key), Retry | OfferReply | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 30 | `leviculum-nrf/src/pn.rs:1756` | store error while sending the batch: end `store_error` and close | store result | none | - | - | `sync_round::tests::a_store_error_at_link_up_ends_the_round` |
| 31 | `leviculum-nrf/src/pn.rs:1778` | nothing sent: conclude and advance if nothing was withheld, else end `heap_short` | batch.count, batch.withheld | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 32 | `leviculum-nrf/src/pn.rs:1805` | resource sent: record it, lower the cursor target to the batch target; error ends `send_failed` | node result | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 33 | `leviculum-nrf/src/pn.rs:1830` | outbound resource finished: success concludes, failure ends `transfer_failed` | outbound, success | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 34 | `leviculum-nrf/src/pn.rs:1868` | concluding a round: cursor, Idle, next attempt 0, last_heard now, persist, close | peer | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 35 | `leviculum-nrf/src/pn.rs:1953` | an inbound resource is a sync envelope; `multi` means more than one message | resource data | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 36 | `leviculum-nrf/src/pn.rs:1976` | which remote to peer back: the validated hash, else the link identity's PN hash | validated_links, link identity | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 37 | `leviculum-nrf/src/pn.rs:1982` | not a validated peer with a decoded envelope: close on multi, else queue as a client upload | peer, envelope | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 38 | `leviculum-nrf/src/pn.rs:2002` | a second batch while one drains: close the link | sync_batch | none | - | - | `leviculum_sync_batch::tests::an_envelope_with_no_batch_held_is_taken`<br>`leviculum_sync_batch::tests::a_second_envelope_while_a_batch_is_held_closes_the_link` |
| 39 | `leviculum-nrf/src/pn.rs:2051` | peer back from a recalled announce only when not peered yet | peer table | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 40 | `leviculum-nrf/src/pn.rs:2073` | capture keys from the link or the identity cache, persist, sync now | link identity, storage | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 41 | `leviculum-nrf/src/pn.rs:2136` | skip a push while one is in flight or the gap has not passed | active, next_active_at_ms | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 42 | `leviculum-nrf/src/pn.rs:2139` | which stored message to push: same destination, not too large, lowest sequence | store, too_large | `lora_pn_board_sync` | 1800 | 4/2 (>2) | `active_delivery::tests::the_oldest_message_for_the_destination_is_pushed_and_no_other_recipients`<br>`active_delivery::tests::a_message_known_too_large_is_skipped_for_the_next_oldest`<br>`active_delivery::tests::nothing_stored_for_the_destination_pushes_nothing` |
| 43 | `leviculum-nrf/src/pn.rs:2172` | body missing or shorter than prefix plus stamp: no push and no gap | store body | none | - | - | no decision: unreachable guard |
| 44 | `leviculum-nrf/src/pn.rs:2177` | what is pushed: the body minus its 16-byte prefix and the stamp, as one raw packet | body | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 45 | `leviculum-nrf/src/pn.rs:2198` | push result: Ok sets active; TooLarge joins the too-large list (cap 64, oldest out); other errors set the gap | node result | none | - | - | `active_delivery::tests::a_push_too_large_is_left_for_get_and_any_other_refusal_waits_the_gap`<br>`active_delivery::tests::the_too_large_list_holds_an_id_once_and_drops_the_oldest_past_its_cap` |
| 46 | `leviculum-nrf/src/pn.rs:2236` | one work item per pass, else one sync-batch step; an unfinished item goes back to the front | work, sync_batch | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 47 | `leviculum-nrf/src/pn.rs:2253` | a mined key: clear mining_peer, set key and Idle, persist, sync now | MINE_RES | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 48 | `leviculum-nrf/src/pn.rs:639` | miner: a failed generate sends no result, so mining_peer is never cleared | stamper result | none | - | - | no decision: unreachable arm |
| 49 | `leviculum-nrf/src/pn.rs:2285` | store not mounted: withhold the PN announce and defer it 60 s, logged once | record_store::mounted, cadence | none | - | - | `cadence::tests::an_unmounted_store_withholds_the_due_announce_one_retry_at_a_time` |
| 50 | `leviculum-nrf/src/pn.rs:2361` | a commanded announce is refused while the store is unmounted | mounted() | none | - | - | no decision: early return |
| 51 | `leviculum-nrf/src/pn.rs:2398` | the per-BLE-peer PN announce is skipped while unmounted | mounted() | none | - | - | no decision: early return |
| 52 | `leviculum-nrf/src/pn.rs:2416` | maintenance due every 480 s | next_maintenance_at_ms | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 53 | `leviculum-nrf/src/pn.rs:2523` | outbound round past its deadline: end `timeout` and close | sync.deadline_ms | none | - | - | `sync_round::tests::a_round_past_its_deadline_times_out_and_one_at_it_does_not` |
| 54 | `leviculum-nrf/src/pn.rs:2528` | skip the sync pass while a round or mining runs or before it is due; else re-arm the interval | outbound, mining_peer, next_sync_at_ms | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 55 | `leviculum-nrf/src/pn.rs:2583` | key not ready: request a path without an identity hash, else hand the job to the miner (Idle if its channel is full) | peering_key_ready, identity_hash, MINE_REQ | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 56 | `leviculum-nrf/src/pn.rs:2606` | store error in start_round: book the backoff, log `store_error`, Idle | store result | none | - | - | `sync_round::tests::a_store_error_at_round_start_defers_the_peer_one_step` |
| 57 | `leviculum-nrf/src/pn.rs:2642` | probe offer: none returns, an empty plan advances and persists the cursor | plan | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 58 | `leviculum-nrf/src/pn.rs:2651` | no recalled signing key: request a path instead of dialling | recalled identity | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 59 | `leviculum-nrf/src/pn.rs:2658` | book the sync backoff before dialling: accumulator plus one step, next attempt now plus the accumulator | peer backoff | none | - | - | `peering::tests::every_unanswered_sync_attempt_defers_the_peer_one_backoff_step_longer`<br>`peering::tests::a_sync_booking_is_cleared_by_a_link_and_handed_back_by_a_yield` |
| 60 | `leviculum-nrf/src/pn.rs:2669` | connect refused by the link cap: defer, Idle, backoff kept | node result | none | - | - | `sync_round::tests::a_dial_the_link_cap_refuses_defers_the_peer_by_its_booking` |
| 61 | `leviculum-nrf/src/pn.rs:2688` | round deadline is now plus 180 s | now_ms | none | - | - | `sync_round::tests::a_round_past_its_deadline_times_out_and_one_at_it_does_not` |
| 62 | `leviculum-nrf/src/pn.rs:2750` | min_cost 0 and no value needed: accept with value 0 without walking | min_accepted_cost | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 63 | `leviculum-nrf/src/pn.rs:2755` | start a grind if none; undecodable material is Done(None); a resumed grind is not re-tied to its message | grind | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 64 | `leviculum-nrf/src/pn.rs:2779` | min_cost 0 computes the value, otherwise validates against min_cost | min_cost | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 65 | `leviculum-nrf/src/pn.rs:2867` | a parked upload goes back to the front | step | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 66 | `leviculum-nrf/src/pn.rs:2878` | seed the clock from the upload envelope if it decodes | upload data | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 67 | `leviculum-nrf/src/pn.rs:2883` | upload outcome mapping: Accepted flushes; InvalidStamp sends the reject and closes; PeerSyncForm closes; Malformed/StoreFailed log only | UploadOutcome | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 68 | `leviculum-nrf/src/pn.rs:2961` | `/get` without a remote identity: NoIdentity | link identity | none | - | - | `propagation_node::tests::a_get_over_an_unidentified_link_is_answered_no_identity` |
| 69 | `leviculum-nrf/src/pn.rs:2961` | the mailbox is the identity's `lxmf.delivery` hash | identity | `lora_pn_board_sync` | 1800 | 4/2 (>2) | `propagation_node::tests::a_get_over_an_identified_link_drains_that_identitys_delivery_mailbox` |
| 70 | `leviculum-nrf/src/pn.rs:2973` | `/get` outcome: List responds, Fetch flushes purges then serves, Err answers nil | GetOutcome | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 71 | `leviculum-nrf/src/pn.rs:3038` | `/offer` without an identity: NoIdentity | link identity | none | - | - | `propagation_node::tests::an_offer_over_an_unidentified_link_is_answered_no_identity`<br>`propagation_node::tests::an_offer_over_an_identified_link_names_the_identity_and_its_propagation_destination` |
| 72 | `leviculum-nrf/src/pn.rs:3072` | our cost above 0 validates the peering key; a validator error counts as invalid | our_cost, key | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 73 | `leviculum-nrf/src/pn.rs:3090` | mark the link validated (decides 1201 and 1969 later) | link | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 74 | `leviculum-nrf/src/pn.rs:3093` | which offered ids to want; a store `contains` error counts as want | offer ids, store | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 75 | `leviculum-nrf/src/pn.rs:3134` | validate a synced message only when longer than a stamp; transient id is the unstamped hash; parked goes back | message | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 76 | `leviculum-nrf/src/pn.rs:3188` | count as transferred only when not a duplicate and durable | duplicate, durable | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 77 | `leviculum-nrf/src/pn.rs:3196` | InvalidStamp, Malformed and PeerSyncForm count as rejects, StoreFailed does not | UploadOutcome | none | - | - | `propagation_node::tests::a_bad_stamp_a_bad_body_and_a_nested_transfer_count_against_the_sender`<br>`propagation_node::tests::a_store_failure_does_not_count_against_the_sender`<br>`propagation_node::tests::an_accepted_message_does_not_count_against_the_sender` |
| 78 | `leviculum-nrf/src/pn.rs:3211` | batch drained: conclude; any invalid throttles the sender and closes | drained, invalid | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 79 | `leviculum-nrf/src/pn.rs:3249` | fetch response: one packet if it fits (encode error answers nil), else a resource | link MDU | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 80 | `leviculum-nrf/src/pn.rs:3278` | response too large for a packet goes as a resource, other errors are dropped | node result | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 81 | `leviculum-nrf/src/pn.rs:3301` | a failed append forgets its id so the client's retry is stored | flush result | none | - | - | `leviculum_pn_store::tests::an_append_the_flash_refused_forgets_its_id_and_the_retry_is_stored`<br>`leviculum_pn_store::tests::an_append_refused_by_an_unmounted_store_forgets_its_id`<br>`leviculum_pn_store::tests::an_append_into_a_full_region_lands_and_its_retry_is_a_duplicate` |

### BLE and Columba (`ble/`)

70 decisions no host test reached on 2026-10-10; 30 more were already a call into a host-built crate and are not listed.

| # | Where | What it decides | State it reads | Cheapest rig cell | s | radios/boards | Host test |
|---|---|---|---|---|---|---|---|
| 82 | `leviculum-nrf/src/ble/mod.rs:312` | peer Up/Lost event dropped when the 8-deep queue is full | BLE_PEER_EVENTS | none | - | - || `leviculum_ble_tx::saturation::tests::a_carrier_off_teardown_and_its_relinks_fit_the_peer_event_queue_and_the_next_is_dropped` |
| 83 | `leviculum-nrf/src/ble/mod.rs:451` | carrier_off resolves only once the media state reads off | media::ble_active | none | - | - || `leviculum_ble_tx::carrier::tests::every_path_proceeds_with_the_carrier_on`<br>`leviculum_ble_tx::carrier::tests::every_path_stands_down_with_the_carrier_off_and_none_books_a_backoff` |
| 84 | `leviculum-nrf/src/ble/mod.rs:463` | carrier_on blocks the protocol task until media reads on | media::ble_active | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 85 | `leviculum-nrf/src/ble/mod.rs:529` | a Route whose slot is no longer claimed is dropped as a route miss | HVN_DRAIN | none | - | - | `leviculum_ble_tx::registry::delivery::a_route_to_a_slot_released_since_the_plan_is_a_miss`<br>`leviculum_ble_tx::registry::delivery::a_route_to_a_claimed_slot_goes_to_that_link` |
| 86 | `leviculum-nrf/src/ble/mod.rs:584` | flood target filter: skip unclaimed slots and the ingress slot | HVN_DRAIN, SlotMask | `ble_mesh_formation` | 600 | 3/2 (>2) | `leviculum_ble_tx::registry::delivery::a_flood_takes_every_claimed_slot_and_the_ingress_mask_leaves_its_link_out`<br>`leviculum_ble_tx::registry::delivery::a_flood_with_no_live_link_has_no_target` |
| 87 | `leviculum-nrf/src/ble/mod.rs:609` | per-link queue full: drop (BLE_TX_FANOUT_DROP), else count custody | LINK_OUT fill | none | - | - || `leviculum_ble_tx::saturation::tests::a_stalled_peer_loses_packets_on_its_own_link_and_the_healthy_links_lose_none` |
| 88 | `leviculum-nrf/src/ble/mod.rs:683` | interface peer count is 0 with the carrier off, else the registry count | media, LIVE_PEERS | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 89 | `leviculum-nrf/src/ble/mod.rs:708` | online iff carrier on and at least one claimed drain slot | media, HVN_DRAIN | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 90 | `leviculum-nrf/src/ble/mod.rs:723` | a peer hint aims at that peer, none floods | peer hint | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 91 | `leviculum-nrf/src/ble/mod.rs:750` | carrier off swallows an outbound packet as Ok | media::ble_active | none | - | - || `leviculum_ble_tx::saturation::tests::a_full_outbound_queue_answers_buffer_full_and_a_dark_carrier_answers_ok_first`<br>`leviculum_ble_tx::carrier::tests::only_a_session_disconnects_and_only_the_outbound_path_answers_ok` |
| 92 | `leviculum-nrf/src/ble/mod.rs:765` | BLE_OUTGOING full answers BufferFull with an IFACE_FULL line | BLE_OUTGOING | none | - | - || `leviculum_ble_tx::saturation::tests::a_full_outbound_queue_answers_buffer_full_and_a_dark_carrier_answers_ok_first` |
| 93 | `leviculum-nrf/src/ble/mod.rs:1078` | inconclusive SoftDevice RAM probe skips the RAM-floor guard | probe result | `boot_smoke_t114` | 300 | 1/1 |  |
| 94 | `leviculum-nrf/src/ble/mod.rs:1095` | SoftDevice RAM above `__sretained` panics at boot | wanted, sd_ceiling | `boot_smoke_t114` | 300 | 1/1 |  |
| 95 | `leviculum-nrf/src/ble/mod.rs:1284` | columba enabled spawns the BLE tasks, else holds the carrier down | boot media profile | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 96 | `leviculum-nrf/src/ble/notify.rs:79` | notify error class: Resources is QueueFull, Disconnected, other Failed | NotifyValueError | none | - | - | no decision: HAL glue, the SoftDevice error mapped onto `NotifyOutcome`; what each outcome does is `PacketTx`'s, host-tested |
| 97 | `leviculum-nrf/src/ble/notify.rs:128` | a torn packet not caused by a disconnect disconnects the link (BLE_TX_RESYNC) | tx.torn, AbortReason | none | - | - | `leviculum_ble_tx::tests::the_notify_path_disconnects_only_on_a_tear_the_peer_still_holds` |
| 98 | `leviculum-nrf/src/ble/notify.rs:149` | Action::Nothing is reported as drop `internal` | action | none | - | - | no decision: unreachable, the driver feeds only the event the action asked for; the machine side is `leviculum_ble_tx::tests::an_event_the_machine_did_not_ask_for_cannot_provoke_a_second_send` |
| 99 | `leviculum-nrf/src/ble/columba.rs:256` | advertised free slots are PERIPH_SLOTS minus busy | PERIPH_BUSY | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 100 | `leviculum-nrf/src/ble/columba.rs:364` | GATT registration refused: BLE_INIT_FAIL, no BLE tasks | SoftDevice result | none | - | - | no decision: early return at boot, a SoftDevice configuration no air input reaches |
| 101 | `leviculum-nrf/src/ble/columba.rs:471` | peripheral: carrier off stops advertising and waits | media::ble_active | none | - | - || `leviculum_ble_tx::carrier::tests::advertising_and_scanning_stop_at_the_gate_and_in_flight_alike` |
| 102 | `leviculum-nrf/src/ble/columba.rs:506` | advertise outcome: a conn claims a slot, an error yields None, carrier off restarts | advertise result | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 103 | `leviculum-nrf/src/ble/columba.rs:576` | a failed advertise backs off 1000 ms | conn | none | - | - | `leviculum_ble_tx::admission::dial_steps::a_failed_advertisement_pauses_a_second_and_a_served_one_does_not` |
| 104 | `leviculum-nrf/src/ble/columba.rs:671` | supervision-timeout request keeps interval and latency, changes the timeout | ask | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 105 | `leviculum-nrf/src/ble/columba.rs:747` | a defragmenter error abandons the reassembly | DefragResult | none | - | - | `framing::ble::tests::test_a_garbage_frame_abandons_the_head_it_would_otherwise_keep`<br>`framing::ble::tests::test_process_or_abandon_passes_good_frames_and_counts_no_idle_garbage` (`leviculum-core`) |
| 106 | `leviculum-nrf/src/ble/columba.rs:809` | a peripheral conn without a handle abandons the session, no disconnect | conn.handle | none | - | - | no decision: early return, a connection reads no handle only once it is gone |
| 107 | `leviculum-nrf/src/ble/columba.rs:824` | drain table full ends the peripheral link (a disconnect since the fifth wave, the connection's drop before it) | HVN_DRAIN | none | - | - || `leviculum_ble_tx::saturation::tests::a_refused_drain_claim_ends_the_link_on_both_roles_and_books_nothing` |
| 108 | `leviculum-nrf/src/ble/columba.rs:864` | discard the previous tenancy's queue at claim | LINK_OUT[slot] | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 109 | `leviculum-nrf/src/ble/columba.rs:897` | RX classifier: before the handshake a 16-byte write is the identity | handshake_done, len | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 110 | `leviculum-nrf/src/ble/columba.rs:934` | an incoming refusal dead-ends the address, notes the dial ledger, disconnects | link_up result | `ble_room_4` | 900 | 4/4 (>2) |  |
| 111 | `leviculum-nrf/src/ble/columba.rs:951` | payload before the identity is refused and disconnected (BLE_PRE_HANDSHAKE_REFUSE) | handshake_done, len | `ble_unidentified_central_is_dropped` | 600 | 1/1 |  |
| 112 | `leviculum-nrf/src/ble/columba.rs:974` | a write shorter than the fragment header is a keepalive, liveness only | len | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 113 | `leviculum-nrf/src/ble/columba.rs:986` | other payload feeds the liveness clock and the defragmenter | len | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 114 | `leviculum-nrf/src/ble/columba.rs:1008` | a completed packet is dropped when BLE_INCOMING is full (silent until the fifth wave, now `rx_full=`) | BLE_INCOMING | none | - | - || `leviculum_ble_tx::saturation::tests::a_stalled_main_loop_drops_the_inbound_burst_past_the_queue_and_counts_it` |
| 115 | `leviculum-nrf/src/ble/columba.rs:1055` | a packet before hold-ready is held (logged once) until ready | TxHold | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 116 | `leviculum-nrf/src/ble/columba.rs:1121` | session end with carrier off: BLE_CARRIER_DROP and disconnect | carrier | none | - | - || `leviculum_ble_tx::carrier::tests::only_a_session_disconnects_and_only_the_outbound_path_answers_ok` |
| 117 | `leviculum-nrf/src/ble/columba.rs:1137` | Expired with a known identity is a churn departure, Displaced/HandshakeOverdue are not | LinkEnd | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 118 | `leviculum-nrf/src/ble/columba.rs:1105` | a link-over end disconnects | LinkEnd | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 119 | `leviculum-nrf/src/ble/columba.rs:1264` | a displacement wake for this handle ends the session, a stale one is ignored | DISPLACE_WAKES | `ble_room_4` | 900 | 4/4 (>2) |  |
| 120 | `leviculum-nrf/src/ble/columba.rs:1409` | Refused: count, LinkRefused phase event, false | LinkUp | `ble_room_4` | 900 | 4/4 (>2) |  |
| 121 | `leviculum-nrf/src/ble/columba.rs:1436` | Displaced: move the queue, signal the old link, LinkDisplaced phase event | LinkUp | `ble_room_4` | 900 | 4/4 (>2) |  |
| 122 | `leviculum-nrf/src/ble/columba.rs:1469` | Accepted: PeerEvent::Up only on the first link | first | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 123 | `leviculum-nrf/src/ble/columba.rs:1586` | displacement moves queued packets; a full new queue drops | LINK_OUT | none | - | - || `leviculum_ble_tx::saturation::tests::a_displacement_into_a_full_queue_drops_what_does_not_fit_as_the_fanout_does` |
| 124 | `leviculum-nrf/src/ble/columba.rs:1839` | phase reached fallback: clear PHASE_NEIGHBOURS | poll result | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 125 | `leviculum-nrf/src/ble/columba.rs:2230` | advertising report filter: connectable and not a scan response | report type | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 126 | `leviculum-nrf/src/ble/columba.rs:2351` | an Exclusion rejects the candidate (ledger/churn holds logged) | admission outcome | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 127 | `leviculum-nrf/src/ble/columba.rs:2482` | discovery failure disconnects, no dead-end and no ledger note | discover result | none | - | - | `leviculum_ble_tx::admission::dial_steps::a_layout_or_identity_the_peer_answered_wrongly_is_not_columba`<br>`leviculum_ble_tx::admission::dial_steps::a_stage_that_heard_nothing_back_is_the_air_at_every_stage` |
| 128 | `leviculum-nrf/src/ble/columba.rs:2500` | identity read failure disconnects, no dead-end and no ledger note | read result | none | - | - | `leviculum_ble_tx::admission::dial_steps::a_layout_or_identity_the_peer_answered_wrongly_is_not_columba`<br>`leviculum_ble_tx::admission::dial_steps::a_stage_that_heard_nothing_back_is_the_air_at_every_stage` |
| 129 | `leviculum-nrf/src/ble/columba.rs:2521` | a peer presenting our own identity: BLE_LINK_SELF, dead-end, disconnect | peer_id, own identity | none | - | - | `leviculum_ble_tx::admission::dial_steps::a_dial_that_reads_our_own_identity_is_a_self_link` |
| 130 | `leviculum-nrf/src/ble/columba.rs:2544` | a central conn without a handle returns, no disconnect | conn.handle | none | - | - | no decision: early return, as row 106 |
| 131 | `leviculum-nrf/src/ble/columba.rs:2557` | drain table full on the central side disconnects | HVN_DRAIN | none | - | - || `leviculum_ble_tx::saturation::tests::a_refused_drain_claim_ends_the_link_on_both_roles_and_books_nothing` |
| 132 | `leviculum-nrf/src/ble/columba.rs:2568` | discard the previous tenancy's queue before the registry entry (central) | LINK_OUT[slot] | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 133 | `leviculum-nrf/src/ble/columba.rs:2572` | an outgoing duplicate refused: dead-end, dial ledger, disconnect, release | link_up result | `ble_room_4` | 900 | 4/4 (>2) |  |
| 134 | `leviculum-nrf/src/ble/columba.rs:2656` | CCCD subscribe failure disconnects | write result | none | - | - | `leviculum_ble_tx::admission::dial_steps::a_refused_subscription_books_as_the_air` |
| 135 | `leviculum-nrf/src/ble/columba.rs:2673` | a handshake value that cannot be built returns without a disconnect | GattData::new | none | - | - | no decision: unreachable, a 16-byte value is under `GATT_VALUE_MAX` |
| 136 | `leviculum-nrf/src/ble/columba.rs:2678` | handshake write failure disconnects | write result | none | - | - | `leviculum_ble_tx::admission::dial_steps::a_refused_handshake_is_a_peer_that_will_not_handshake` |
| 137 | `leviculum-nrf/src/ble/columba.rs:2743` | central RX: shorter than the header is a keepalive, else payload and defrag | len | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 138 | `leviculum-nrf/src/ble/columba.rs:2773` | central: a completed packet is dropped when BLE_INCOMING is full (silent until the fifth wave, now `rx_full=`) | BLE_INCOMING | none | - | - || `leviculum_ble_tx::saturation::tests::a_stalled_main_loop_drops_the_inbound_burst_past_the_queue_and_counts_it` |
| 139 | `leviculum-nrf/src/ble/columba.rs:2793` | central keepalive is always armed | last_keepalive | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 140 | `leviculum-nrf/src/ble/columba.rs:2806` | a fragment that cannot be built: BLE_TX_DROP, torn | GattData::new | none | - | - | no decision: unreachable, `fragment_mtu` caps every fragment at `GATT_VALUE_MAX` |
| 141 | `leviculum-nrf/src/ble/columba.rs:2831` | write-command error: BLE_TX_DROP, torn | write result | none | - | - | `leviculum_ble_tx::tests::the_write_path_disconnects_on_every_abort` |
| 142 | `leviculum-nrf/src/ble/columba.rs:2879` | torn on any failed fragment, the first included: disconnect | torn | none | - | - | `leviculum_ble_tx::tests::the_write_path_disconnects_on_every_abort` |
| 143 | `leviculum-nrf/src/ble/columba.rs:2889` | keepalive failure ends the outbound half, no disconnect | write result | none | - | - | no decision: early return, the session ends through the teardown every end takes |
| 144 | `leviculum-nrf/src/ble/columba.rs:2914` | central carrier off: BLE_CARRIER_DROP and disconnect | carrier | none | - | - || `leviculum_ble_tx::carrier::tests::only_a_session_disconnects_and_only_the_outbound_path_answers_ok` |
| 145 | `leviculum-nrf/src/ble/columba.rs:2927` | central Expired is a churn departure; any link-over end disconnects | LinkEnd | `ble_reconnect` | 1200 | 3/2 (>2) |  |
| 146 | `leviculum-nrf/src/ble/columba.rs:2959` | central task: carrier off, no scanning or dialling | media::ble_active | none | - | - || `leviculum_ble_tx::carrier::tests::advertising_and_scanning_stop_at_the_gate_and_in_flight_alike` |
| 147 | `leviculum-nrf/src/ble/columba.rs:2983` | chosen target: fallback verdict is FallbackTarget, else StrictTarget | decision | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 148 | `leviculum-nrf/src/ble/columba.rs:3016` | an empty scan window rescans at once | scan result | `ble_mesh_formation` | 600 | 3/2 (>2) | `leviculum_ble_tx::admission::dial_steps::a_dial_or_a_failed_scan_pauses_and_an_empty_window_or_a_dark_carrier_does_not` |
| 149 | `leviculum-nrf/src/ble/columba.rs:3017` | a scan error waits 5 s | scan result | none | - | - | `leviculum_ble_tx::admission::dial_steps::a_dial_or_a_failed_scan_pauses_and_an_empty_window_or_a_dark_carrier_does_not` |
| 150 | `leviculum-nrf/src/ble/columba.rs:3024` | carrier off mid-scan returns to the gate without backoff | carrier | none | - | - | `leviculum_ble_tx::admission::dial_steps::a_dial_or_a_failed_scan_pauses_and_an_empty_window_or_a_dark_carrier_does_not`<br>`leviculum_ble_tx::carrier::tests::advertising_and_scanning_stop_at_the_gate_and_in_flight_alike` |
| 151 | `leviculum-nrf/src/ble/columba.rs:3026` | 5 s backoff after every dial attempt and session end | CENTRAL_RETRY_BACKOFF_MS | `ble_mesh_formation` | 600 | 3/2 (>2) | `leviculum_ble_tx::admission::dial_steps::a_dial_or_a_failed_scan_pauses_and_an_empty_window_or_a_dark_carrier_does_not` |

### LoRa, radio, announce and telemetry (`lora.rs`, `sx1262.rs`, `announce.rs`, `telemetry.rs`)

97 decisions no host test reached on 2026-10-10; 54 more were already a call into a host-built crate and are not listed. Since the seventh wave 40 of the 97 carry host tests (one of them, 244, a test that existed before the census), 2 are marked no decision, and none of the 34 "none" rows is left.

| # | Where | What it decides | State it reads | Cheapest rig cell | s | radios/boards | Host test |
|---|---|---|---|---|---|---|---|
| 152 | `leviculum-nrf/src/lora.rs:135` | refuse a TX-spacing set while one is queued (REFUSE_BUSY) | LORA_TX_SPACING | none | - | - | `leviculum_tx_spacing::tests::a_spacing_set_while_one_is_unread_is_refused_and_overwrites_nothing` |
| 153 | `leviculum-nrf/src/lora.rs:361` | carrier off swallows an outbound packet as Ok | media::lora_active | none | - | - | `leviculum_queue_budget::tests::a_dark_carrier_swallows_a_packet_however_full_the_queue_is`<br>`leviculum_queue_budget::tests::a_lit_carrier_queues_with_room_and_refuses_with_the_bound_that_bound` |
| 154 | `leviculum-nrf/src/lora.rs:428` | channel send failure releases the budget and answers BufferFull | channel | none | - | - | `leviculum_queue_budget::tests::a_channel_refusal_hands_the_reserved_room_back` |
| 155 | `leviculum-nrf/src/lora.rs:573` | a config whose bandwidth has no SX1262 code is rejected | wire.bandwidth_hz | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 156 | `leviculum-nrf/src/lora.rs:588` | coding-rate code is the denominator minus 4, unchecked | wire.cr | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 157 | `leviculum-nrf/src/lora.rs:747` | mirror a duty-hold edge into the core | LORA_DUTY_HOLD | `lora_duty_lock_lawful_default` | 900 | 2/1 |  |
| 158 | `leviculum-nrf/src/lora.rs:751` | no announce-cap registration before the radio is configured | RUNNING_CONFIG | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 159 | `leviculum-nrf/src/lora.rs:812` | open a duty hold once per hold: raise the flag and signal the USB frame | hold state, stale total | `lora_duty_lock_lawful_default` | 900 | 2/1 | `queue_budget tests::a_hold_opens_once_and_the_lift_measures_from_the_first_held_turn` |
| 160 | `leviculum-nrf/src/lora.rs:833` | lift a duty hold only if one is open, once, and state held time and stale drops | hold state, airtime | none (needs a run past one rolling hour) | >3600 | - | `queue_budget tests::a_lift_happens_once_and_never_without_a_hold`<br>`queue_budget tests::the_lift_counts_drops_across_the_counter_wrap` |
| 161 | `leviculum-nrf/src/lora.rs:994` | radio_silent grants (restarts) the mute lease, else clears it | config.radio_silent | none | - | - | `leviculum_mute_lease::tests::a_silent_config_restarts_the_lease_it_finds`<br>`leviculum_mute_lease::tests::a_config_that_is_not_silent_lifts_the_mute_without_an_expiry` |
| 162 | `leviculum-nrf/src/lora.rs:1100` | split-frame sequence nibble from the xorshift state | rng_state | `lora_lnode_split` | 240 | 2/1 |  |
| 163 | `leviculum-nrf/src/lora.rs:1158` | TX ok charges airtime; a TX error aborts, the second split frame is neither sent nor charged | radio result | none | - | - | `leviculum_queue_budget::tests::a_refused_frame_is_not_charged_and_the_packet_is_not_counted`<br>`leviculum_queue_budget::tests::a_split_packet_whose_first_half_fails_keys_and_charges_nothing_more`<br>`leviculum_queue_budget::tests::a_split_packet_whose_second_half_fails_keeps_the_first_charge_and_is_not_counted` |
| 164 | `leviculum-nrf/src/lora.rs:1187` | count TX and flash the display only if every frame went out | tx_ok | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 | `leviculum_queue_budget::tests::a_single_frame_that_leaves_the_air_is_charged_and_counted`<br>`leviculum_queue_budget::tests::a_split_packet_is_counted_once_both_halves_left_the_air` |
| 165 | `leviculum-nrf/src/lora.rs:1388` | no RX window while a config is pending | config_rx | none | - | - | `leviculum_rx_arming::tests::a_window_stands_aside_while_a_config_waits_for_its_turn` |
| 166 | `leviculum-nrf/src/lora.rs:1445` | window outcome: reception true, timeout or error false (drives empty-ack and peer-yield) | rx_result | `bench_dual_pair_fast` | 360 | 4/2 (>2) |  |
| 167 | `leviculum-nrf/src/lora.rs:1606` | runtime reconfig ok: mute, publish, set_phy, limits, CONFIG_APPLIED; error keeps the old PHY, no ack | configure result | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 168 | `leviculum-nrf/src/lora.rs:1712` | init_radio failure ends the LoRa task | radio | none | - | - | no decision: boot-only return, a radio that does not answer its first command leaves the board without LoRa and nothing to decide |
| 169 | `leviculum-nrf/src/lora.rs:1729` | boot configure failure ends the task; ok applies the mute and publishes | radio | `boot_smoke_t114` | 300 | 1/1 |  |
| 170 | `leviculum-nrf/src/lora.rs:1762` | a zero RNG seed is replaced | channel_seed | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 171 | `leviculum-nrf/src/lora.rs:1820` | take a new TX spacing at the top of a turn | spacing_rx | none | - | - | `leviculum_tx_spacing::tests::the_turn_puts_a_waiting_spacing_in_force_before_the_next_key_up` |
| 172 | `leviculum-nrf/src/lora.rs:1833` | apply a runtime config at the top of a turn | config_rx | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 173 | `leviculum-nrf/src/lora.rs:1882` | lift the hold on an idle turn: hold open, nothing pending, not locked | hold state, pending_tx, airtime | none (needs a run past one rolling hour) | >3600 | - | `queue_budget tests::an_idle_turn_lifts_only_an_open_hold_with_nothing_pending_and_no_lock`<br>`queue_budget tests::the_hold_edge_lifts_on_the_first_idle_turn_after_the_rolling_hour_frees_a_bin` |
| 174 | `leviculum-nrf/src/lora.rs:1911` | dequeue the next frame only when none is pending | pending_tx | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 175 | `leviculum-nrf/src/lora.rs:1943` | DropStale: open the hold, count, drop, rate-limited line, next turn | verdict | `lora_duty_lock_lawful_default` | 900 | 2/1 |  |
| 176 | `leviculum-nrf/src/lora.rs:1988` | Hold: open the hold, listen one post-TX window at site Hold, retry | verdict | `lora_duty_lock_lawful_default` | 900 | 2/1 |  |
| 177 | `leviculum-nrf/src/lora.rs:2060` | acquisition jitter: listen through the draw, clamp 1..10000 ms, credit elapsed | access | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 178 | `leviculum-nrf/src/lora.rs:2131` | CAD clear transmits and bumps the burst counters | CAD result | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 | `leviculum_channel_access::tests::the_single_cad_entry_gives_each_outcome_its_own_arm` |
| 179 | `leviculum-nrf/src/lora.rs:2166` | CSMA backoff is slots times the jitter slot, clamped 1..10000 ms | slots | `bench_dual_pair_fast` | 360 | 4/2 (>2) | `leviculum_channel_access::tests::a_backoff_is_listened_through_in_whole_csma_slots`<br>`leviculum_channel_access::tests::a_backoff_of_no_slots_still_arms_a_window`<br>`leviculum_channel_access::tests::no_listening_wait_parks_the_transmit_path_past_the_ceiling` |
| 180 | `leviculum-nrf/src/lora.rs:2158` | CAD error: a forced TX skips the ack window and burst check | access | none | - | - | `leviculum_channel_access::tests::a_radio_that_cannot_cad_is_retried_at_once_then_forced_out`<br>`leviculum_channel_access::tests::only_a_key_up_forced_by_cad_errors_skips_the_ack_window` |
| 181 | `leviculum-nrf/src/lora.rs:2206` | burst continuation pulls the next frame; a mute-dropped one counts as empty | queue, lease | `bench_dual_pair_fast` | 360 | 4/2 (>2) |  |
| 182 | `leviculum-nrf/src/lora.rs:2239` | post-TX ack window counts consecutive empty acks | ack_received | `bench_dual_pair_fast` | 360 | 4/2 (>2) |  |
| 183 | `leviculum-nrf/src/lora.rs:2266` | peer-turn yield after two empty acks, twice the ack window | consecutive_empty_acks | `bench_dual_pair_fast` | 360 | 4/2 (>2) |  |
| 184 | `leviculum-nrf/src/lora.rs:2293` | reset burst accounting; the next acquisition owes fresh jitter | - | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 185 | `leviculum-nrf/src/lora.rs:2314` | idle continuous RX until a frame or an outgoing packet | select | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 186 | `leviculum-nrf/src/lora.rs:2369` | outgoing in idle stands RX down deferring one frame, then admits | latch | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 187 | `leviculum-nrf/src/sx1262.rs:495` | image-calibration band from the frequency | freq_hz | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 188 | `leviculum-nrf/src/sx1262.rs:516` | LDO regulator, DIO2 as RF switch, TCXO timeout | constants | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 189 | `leviculum-nrf/src/sx1262.rs:631` | packet params: explicit header, CRC on, standard IQ, preamble | preamble_len | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 190 | `leviculum-nrf/src/sx1262.rs:750` | sync word 0x1424, TX clamp, StopRxTimerOnPreamble | constants | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 191 | `leviculum-nrf/src/sx1262.rs:816` | TX ok is tx_done, else force standby | on_air result | none | - | - | `leviculum_core::sx126x::tests::only_a_reported_tx_done_leaves_the_transmitter_without_a_forced_standby` |
| 192 | `leviculum-nrf/src/sx1262.rs:833` | TX wait floor 100 ms | timeout_ms | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 | `leviculum_core::sx126x::tests::the_tx_wait_has_a_floor_no_computed_timeout_reaches` |
| 193 | `leviculum-nrf/src/sx1262.rs:841` | TX success only with IRQ_TX_DONE | IRQ flags | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 | `leviculum_core::sx126x::tests::a_frame_left_the_air_only_if_tx_done_is_latched` |
| 194 | `leviculum-nrf/src/sx1262.rs:1052` | RX hardware timeout: 0 single mode, else ms*64 capped at 0xFFFFFF | timeout_ms | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 195 | `leviculum-nrf/src/sx1262.rs:1109` | awaiting an unarmed window is NotArmed | rx_state | none | - | - | no decision: unreachable guard, the one caller (`leviculum_rx_arming::receive_and_hand_up`) awaits only after `ensure_armed` succeeded |
| 196 | `leviculum-nrf/src/sx1262.rs:1114` | software wait: 60 s single mode, else timeout plus 500 ms | timeout_ms | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 197 | `leviculum-nrf/src/sx1262.rs:1150` | latched take: not armed is an error, no RxDone/Timeout is None | rx_state, IRQ | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 198 | `leviculum-nrf/src/sx1262.rs:1282` | the chip left RX on RxDone or Timeout | IRQ flags | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 199 | `leviculum-nrf/src/sx1262.rs:1290` | erratum 15.3 workaround only for timed RX | hw_timeout | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 200 | `leviculum-nrf/src/sx1262.rs:1294` | classify: CRC error drops the payload, RxDone is a frame, Timeout, else WaitExpired | IRQ flags | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 201 | `leviculum-nrf/src/sx1262.rs:1317` | truncate the read to the buffer | len | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 202 | `leviculum-nrf/src/sx1262.rs:1366` | CAD detMin 0x0A, CAD-only exit mode | constants | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 203 | `leviculum-nrf/src/sx1262.rs:1424` | CAD done: busy iff CAD_DETECTED; timeout goes standby with an error | DIO1, IRQ | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 204 | `leviculum-nrf/src/sx1262.rs:1486` | standing-window age (clamped) fed to adoption | rx_state | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 205 | `leviculum-nrf/src/sx1262.rs:1503` | a window ends itself iff its timeout is non-zero | window | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 206 | `leviculum-nrf/src/announce.rs:153` | announce set is 2 frames with the PN role, else 1 | propagation_hash | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 207 | `leviculum-nrf/src/announce.rs:188` | the first announce heard from a destination triggers our own at once | NEIGHBOURS | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 208 | `leviculum-nrf/src/announce.rs:188` | neighbour-known test, round-robin eviction at 32 | slots | none | - | - | `leviculum_announce_policy::scheduler::tests::a_known_neighbour_buys_nothing`<br>`leviculum_announce_policy::scheduler::tests::a_thirty_third_neighbour_displaces_the_first_round_robin` |
| 209 | `leviculum-nrf/src/announce.rs:257` | movement evidence: none for a fixed pin, no GNSS, invalid or no lat/lon | position flags, GNSS_FIX | none | - | - | `leviculum_announce_policy::scheduler::tests::a_fixed_pin_offers_no_movement_evidence_whatever_the_receiver_says`<br>`leviculum_announce_policy::scheduler::tests::a_board_without_gnss_offers_no_movement_evidence`<br>`leviculum_announce_policy::scheduler::tests::an_invalid_fix_offers_no_evidence_even_with_coordinates_kept`<br>`leviculum_announce_policy::scheduler::tests::a_valid_fix_without_latitude_or_longitude_offers_none`<br>`leviculum_announce_policy::scheduler::tests::a_valid_fix_is_evidence_with_its_hdop` |
| 210 | `leviculum-nrf/src/announce.rs:316` | sleep until the earlier of the announce deadline and the movement sample | cadence, sampler | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 211 | `leviculum-nrf/src/announce.rs:326` | sample movement on its interval; a resume announces at once | sampler | none | - | - | `leviculum_announce_policy::scheduler::tests::the_movement_proof_is_sampled_once_per_interval`<br>`leviculum_announce_policy::scheduler::tests::a_resume_after_movement_announces_at_the_sample_that_proved_it`<br>`leviculum_announce_policy::scheduler::tests::a_pinned_board_that_is_carried_never_resumes` |
| 212 | `leviculum-nrf/src/announce.rs:363` | no delivery destination, no peer-up announce | delivery_hash | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 213 | `leviculum-nrf/src/announce.rs:405` | no delivery hash: nothing, after the poll consumed the slot | delivery_hash | none | - | - | `leviculum_announce_policy::scheduler::tests::a_board_without_a_delivery_destination_spends_its_slot_and_sends_nothing` |
| 214 | `leviculum-nrf/src/telemetry.rs:153` | position-source flags gate NoPositionSource | gnss_available, flash | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 215 | `leviculum-nrf/src/telemetry.rs:178` | set or clear the fixed-position bit | present | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 216 | `leviculum-nrf/src/telemetry.rs:194` | inbound target: the newest replaces an unread one | INBOUND_TARGET | `board_delivery_promise` | 1800 | 3/1 (>2) | `leviculum_telemetry_policy::reporter::tests::the_newest_inbound_frame_replaces_an_unread_one`<br>`leviculum_telemetry_policy::reporter::tests::a_queue_that_refuses_after_the_discard_reports_the_frame_not_taken` |
| 217 | `leviculum-nrf/src/telemetry.rs:219` | inbound fixed position: the newest replaces an unread one | channel | none | - | - | `leviculum_telemetry_policy::reporter::tests::the_newest_inbound_frame_replaces_an_unread_one`<br>`leviculum_telemetry_policy::reporter::tests::a_queue_that_refuses_after_the_discard_reports_the_frame_not_taken` |
| 218 | `leviculum-nrf/src/telemetry.rs:437` | persist answer Durable/Lost, Lost after the 2.5 s deadline | gate, deadline | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 219 | `leviculum-nrf/src/telemetry.rs:579` | a save request displaces an unread older one (six record kinds) | PENDING_SAVE_* | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 220 | `leviculum-nrf/src/telemetry.rs:688` | which record the store task rewrites | select result | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 221 | `leviculum-nrf/src/telemetry.rs:720` | skip the erase when the page is unchanged, answer Durable | flash contents | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 222 | `leviculum-nrf/src/telemetry.rs:745` | flash write retried 3 times at 250 ms, then Lost | flash result | none | - | - | `leviculum_telemetry_policy::reporter::tests::a_flash_that_fails_three_times_is_lost_after_three_pauses`<br>`leviculum_telemetry_policy::reporter::tests::a_flash_that_takes_the_second_write_is_written` |
| 223 | `leviculum-nrf/src/telemetry.rs:774` | Durable iff written | written | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 224 | `leviculum-nrf/src/telemetry.rs:845` | a sensor fix needs lat and lon | readings | none | - | - | `leviculum_telemetry_policy::reporter::tests::a_sensor_fix_needs_both_latitude_and_longitude` |
| 225 | `leviculum-nrf/src/telemetry.rs:855` | a sensor location needs HDOP; accuracy is hdop*5 m | readings.hdop | none | - | - | `leviculum_telemetry_policy::reporter::tests::a_sensor_location_is_hdop_times_five_metres`<br>`leviculum_telemetry_policy::reporter::tests::a_sensor_fix_without_hdop_is_a_position_the_policy_will_not_report` |
| 226 | `leviculum-nrf/src/telemetry.rs:1235` | a new target drops the owed proof and retry of the old one | - | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 227 | `leviculum-nrf/src/telemetry.rs:1245` | owe a key persist for a Set with hash only | command, public_key | `board_delivery_promise` | 1800 | 3/1 (>2) | `leviculum_telemetry_policy::reporter::tests::a_hash_only_target_rewrites_its_record_once_when_the_key_resolves`<br>`leviculum_telemetry_policy::reporter::tests::a_target_that_carried_its_key_or_a_clear_owes_no_rewrite` |
| 228 | `leviculum-nrf/src/telemetry.rs:1259` | a key-bearing target remembers the identity or logs the rejection | public_key | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 229 | `leviculum-nrf/src/telemetry.rs:1334` | no target, nothing | target | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 230 | `leviculum-nrf/src/telemetry.rs:1348` | NoPositionSource sends nothing, before the key chase | policy.state | none | - | - | `leviculum_telemetry_policy::reporter::tests::a_target_without_a_position_source_sends_nothing_not_even_a_key_request` |
| 231 | `leviculum-nrf/src/telemetry.rs:1348` | AwaitingKey: a known key is ready, else a path request every 60 s | storage, last_key_request_ms | none | - | - | `leviculum_telemetry_policy::reporter::tests::awaiting_key_asks_for_a_path_once_a_minute_until_the_key_is_known`<br>`leviculum_telemetry_policy::reporter::tests::a_ready_target_goes_on_to_the_report` |
| 232 | `leviculum-nrf/src/telemetry.rs:1380` | rewrite the flash record once with the resolved key | key_persist_owed | none | - | - | `leviculum_telemetry_policy::reporter::tests::a_hash_only_target_rewrites_its_record_once_when_the_key_resolves` |
| 233 | `leviculum-nrf/src/telemetry.rs:1415` | a retry debt without payload is forgotten | retry_payload | none | - | - | `leviculum_telemetry_policy::reporter::tests::a_retry_debt_without_its_payload_is_forgotten` |
| 234 | `leviculum-nrf/src/telemetry.rs:1492` | sensor fix only in presence state Fix | presence_has_fix | none | - | - | `leviculum_telemetry_policy::reporter::tests::a_sensor_fix_counts_only_in_presence_state_fix`<br>`leviculum_telemetry_policy::reporter::tests::a_fixed_pin_replaces_the_sensor_without_gnss_or_with_it` |
| 235 | `leviculum-nrf/src/telemetry.rs:1504` | location source: none, fixed or sensor | include_position, possrc | none | - | - | `leviculum_telemetry_policy::reporter::tests::the_location_is_none_fixed_or_the_sensor` |
| 236 | `leviculum-nrf/src/telemetry.rs:1514` | no calendar time: withhold `no-clock` | unix_secs | none | - | - | `leviculum_telemetry_policy::reporter::tests::no_calendar_time_withholds_no_clock_and_books_nothing` |
| 237 | `leviculum-nrf/src/telemetry.rs:1563` | build failure books airtime and withholds | build result | none | - | - | `leviculum_telemetry_policy::reporter::tests::a_failure_after_the_announce_books_its_airtime_and_withholds` |
| 238 | `leviculum-nrf/src/telemetry.rs:1573` | on-air failure books airtime and withholds | message | none | - | - | `leviculum_telemetry_policy::reporter::tests::a_failure_after_the_announce_books_its_airtime_and_withholds` |
| 239 | `leviculum-nrf/src/telemetry.rs:1582` | no usable path: path request, book airtime, withhold | has_path, path_route | none | - | - | `leviculum_telemetry_policy::reporter::tests::no_usable_path_asks_and_withholds_naming_why`<br>`leviculum_telemetry_policy::reporter::tests::a_failure_after_the_announce_books_its_airtime_and_withholds` |
| 240 | `leviculum-nrf/src/telemetry.rs:1632` | send the report; its route is the interfaces of its own SendPacket actions | send result | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 241 | `leviculum-nrf/src/telemetry.rs:1651` | record the position in the cadence only if included | include_position | none | - | - | `leviculum_telemetry_policy::reporter::tests::the_cadence_records_a_position_only_if_the_report_carried_it` |
| 242 | `leviculum-nrf/src/telemetry.rs:1670` | send error books airtime and withholds | - | none | - | - | `leviculum_telemetry_policy::reporter::tests::a_failure_after_the_announce_books_its_airtime_and_withholds` |
| 243 | `leviculum-nrf/src/telemetry.rs:1759` | only packets to our own delivery destination are screened | destination | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 244 | `leviculum-nrf/src/telemetry.rs:1762` | only the configured target may request a report | target, storage | none | - | - | already a call into `leviculum_lxmf::telemetry::screen_telemetry_request`, which takes the target as its allowed sender; the inline part only reads the target and its identity: `leviculum_lxmf` `tests/telemetry_request.rs`: `with_no_target_configured_nobody_is_allowed`<br>`a_request_from_a_foreign_sender_is_not_allowed`<br>`a_request_without_the_targets_key_is_unverifiable` |
| 245 | `leviculum-nrf/src/telemetry.rs:1766` | not-a-message or no-inbox discards; a request is accepted or rejected | verdict | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 246 | `leviculum-nrf/src/telemetry.rs:1855` | no pending report notes a non-dispatch | pending_line | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 247 | `leviculum-nrf/src/telemetry.rs:1864` | losses are the error, drop and retry interface ids | DispatchResult | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 248 | `leviculum-nrf/src/telemetry.rs:1943` | announce payload: name, no stamp cost, no compression | identity, name | `board_delivery_promise` | 1800 | 3/1 (>2) |  |

### Main loops, USB control, status and panic handling (`bin/*`, `usb.rs`, `lib.rs`, `remote_status.rs`, `mgmt.rs`, `interface.rs`)

90 decisions no host test reached on 2026-10-10; 31 more were already a call into a host-built crate and are not listed. Since the ninth wave 18 of the 90 carry host tests, 10 are marked no decision, and 2 of the 27 "none" rows are decisions left (306, 320).

| # | Where | What it decides | State it reads | Cheapest rig cell | s | radios/boards | Host test |
|---|---|---|---|---|---|---|---|
| 249 | `leviculum-nrf/src/bin/t114.rs:144` | persistent-log replay drops the leading partial line (also rak4631.rs:164, solarnode.rs:171) | snapshot bytes | `boot_smoke_t114` | 300 | 1/1 || `leviculum_boot_trace::tail::tests::the_leading_partial_line_is_dropped`<br>`leviculum_boot_trace::tail::tests::a_last_line_without_its_end_is_kept`<br>`leviculum_boot_trace::tail::tests::a_snapshot_that_is_one_line_is_kept`<br>`leviculum_boot_trace::tail::tests::empty_lines_and_line_ends_are_not_replayed`<br>`leviculum_boot_trace::tail::tests::bytes_that_are_not_utf8_are_named` |
| 250 | `leviculum-nrf/src/bin/t114.rs:185` | node build: transport on, max_links, 8 queued announces, 8 random blobs, probes answered (all three bins) | constants, heap_census | `boot_smoke_t114` | 300 | 1/1 |  |
| 251 | `leviculum-nrf/src/bin/t114.rs:228` | use the flash identity or generate a new one (all three bins) | id_store.load() | `boot_smoke_t114` | 300 | 1/1 |  |
| 252 | `leviculum-nrf/src/bin/t114.rs:260` | mgmt RUNNING flag means the destination was derived (all three bins) | remote_mgmt_dest_hash | `lnode_remote_status` | 900 | 2/1 |  |
| 253 | `leviculum-nrf/src/bin/t114.rs:271` | persist a generated identity, save result ignored (all three bins) | identity_loaded | `reset_loop_quiet` | 300 | 1/1 |  |
| 254 | `leviculum-nrf/src/bin/t114.rs:278` | interface config: MTU 564/508/564, Gateway mode on all three | constants | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 255 | `leviculum-nrf/src/bin/solarnode.rs:397` | probe and mount QSPI only where the board declares a part | CONFIG.qspi_part | `boot_smoke_t114` | 300 | 1/1 |  |
| 256 | `leviculum-nrf/src/bin/t114.rs:378` | radio profile: persisted config, else eu_medium (all three bins) | flash page | `reset_loop_quiet` | 300 | 1/1 |  |
| 257 | `leviculum-nrf/src/bin/t114.rs:403` | spawn the LoRa task or hold the radio down (all three bins) | media.lora_enabled | `boot_smoke_t114` | 300 | 1/1 |  |
| 258 | `leviculum-nrf/src/bin/t114.rs:431` | BLE on or off at init (all three bins) | media.ble_enabled | `boot_smoke_t114` | 300 | 1/1 |  |
| 259 | `leviculum-nrf/src/bin/t114.rs:590` | no IFAC on any interface (all three bins) | - | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 260 | `leviculum-nrf/src/bin/t114.rs:621` | PN role on or off, spawn the miner (all three bins) | pn::Engine::new | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 261 | `leviculum-nrf/src/bin/t114.rs:633` | a telemetry reporter exists only if the delivery destination registered (all three bins) | delivery_hash | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 262 | `leviculum-nrf/src/bin/t114.rs:644` | restore the persisted telemetry target at boot (all three bins) | flash | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 263 | `leviculum-nrf/src/bin/t114.rs:667` | restore the persisted fixed position at boot (all three bins) | flash | none | - | - || no decision: glue, a stored record handed to the reporter; its decode is `leviculum_core::fixed_position_store::decode_fixed_position` and what the pin does is `location_source`, both host-tested |
| 264 | `leviculum-nrf/src/bin/t114.rs:668` | GNSS time seeding off when the watch receiver is unavailable (all three bins) | GNSS_FIX.receiver() | `boot_smoke_t114` | 300 | 1/1 |  |
| 265 | `leviculum-nrf/src/bin/t114.rs:755` | main-loop wake: earliest of node, stats, census, PN deadlines (all three bins) | deadlines | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 266 | `leviculum-nrf/src/bin/t114.rs:797` | arm the 5 s telemetry tick only while a target is set (all three bins) | reporter | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 267 | `leviculum-nrf/src/bin/t114.rs:840` | mirror interface online state into routing (all three bins) | is_online() | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 268 | `leviculum-nrf/src/bin/t114.rs:847` | mirror the BLE peer count into routing (all three bins) | peer_count | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 269 | `leviculum-nrf/src/bin/t114.rs:889` | GNSS seed accepted marks seeded and sets the time source, else refuses (all three bins) | node | none | - | - || `leviculum_gnss_time::tests::a_refused_seed_leaves_the_gate_open_and_an_accepted_one_closes_it`<br>`leviculum_gnss_time::fix::tests::an_invalid_rmc_on_an_unseeded_board_offers_the_gate_nothing` |
| 270 | `leviculum-nrf/src/bin/t114.rs:885` | host wall time: ack or REFUSE_VALUE, answer dropped when the channel is full (all three bins) | node, serial_ctl_tx | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 271 | `leviculum-nrf/src/bin/t114.rs:914` | host telemetry target applied, dropped silently without a reporter (all three bins) | reporter | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 272 | `leviculum-nrf/src/bin/t114.rs:943` | host fixed position applied, dropped silently without a reporter (all three bins) | reporter | none | - | - || no decision: early return, a board without a reporter has no telemetry role this boot (row 261); the host was answered by the serial task with `fixed_position_answer` |
| 273 | `leviculum-nrf/src/bin/t114.rs:970` | host announce: run the PN announce if the role exists and produced actions (all three bins) | pn_engine | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 274 | `leviculum-nrf/src/bin/t114.rs:990` | host announce answer: UNSUPPORTED without delivery, NO_CLOCK, ack, or BUSY (all three bins) | delivery_hash, wall clock | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 275 | `leviculum-nrf/src/bin/t114.rs:1042` | an ack overrides the refusal when the PN announce went out (all three bins) | pn_announced | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 276 | `leviculum-nrf/src/bin/t114.rs:1128` | LoRa RX dropped while LoRa is off at runtime, PN step skipped too (all three bins) | media STATE | none | - | - || no decision: glue, a `continue` on `MediaState::lora_active`, host-tested in `leviculum-media-state` |
| 277 | `leviculum-nrf/src/bin/t114.rs:1145` | attribute BLE RX to the peer the handshake named (all three bins) | peer | `ble_mesh_formation` | 600 | 3/2 (>2) |  |
| 278 | `leviculum-nrf/src/bin/t114.rs:1204` | BLE peer up: PN announce to the phone if the role exists (all three bins) | pn_engine | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 279 | `leviculum-nrf/src/bin/t114.rs:715` | answer `/status` from the PN step on any carrier (all three bins) | events | `lnode_remote_status` | 900 | 2/1 |  |
| 280 | `leviculum-nrf/src/bin/t114.rs:1286` | readings: unix time only with a plausible wall clock (all three bins) | node | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 281 | `leviculum-nrf/src/bin/t114.rs:1317` | readings: GNSS fields copied whenever a fix value exists (all three bins) | GNSS_FIX | none | - | - || no decision: glue, a copy of the snapshot whose fields `GnssFix::fold_rmc` and `fold_gga` decide (rows 340, 341) |
| 282 | `leviculum-nrf/src/bin/t114.rs:1325` | readings: has_fix iff presence is Fix (all three bins) | GNSS_PRESENCE | none | - | - || no decision: glue, the presence state is `leviculum_gnss_presence::PresenceMachine`'s, host-tested |
| 283 | `leviculum-nrf/src/bin/t114.rs:1320` | readings: battery percent only once classified (all three bins) | BATTERY_STATE | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 284 | `leviculum-nrf/src/interface.rs:46` | the serial interface is always online | - | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 285 | `leviculum-nrf/src/interface.rs:45` | a full outgoing channel drops the frame with BufferFull; TX bytes only on accept | channel | none | - | - | `leviculum_usb_policy` `tests/seam.rs`: `a_queued_packet_counts_its_unframed_bytes`<br>`the_packet_past_a_full_queue_is_dropped_uncounted` |
| 286 | `leviculum-nrf/src/remote_status.rs:61` | only RequestReceived events are considered | events | `lnode_remote_status` | 900 | 2/1 |  |
| 287 | `leviculum-nrf/src/remote_status.rs:72` | answer only `/status` at our mgmt destination | path, mgmt dest | `lnode_remote_status` | 900 | 2/1 |  |
| 288 | `leviculum-nrf/src/remote_status.rs:103` | LoRa bitrate only while the radio runs | running_config | `lnode_remote_status` | 900 | 2/1 |  |
| 289 | `leviculum-nrf/src/remote_status.rs:120` | interface status is the core's online mirror | node | `lnode_remote_status` | 900 | 2/1 |  |
| 290 | `leviculum-nrf/src/remote_status.rs:124` | clients reported for BLE only | iface id | `lnode_remote_status` | 900 | 2/1 |  |
| 291 | `leviculum-nrf/src/remote_status.rs:140` | link count only with lstats | include_lstats | `lnode_remote_status` | 900 | 2/1 |  |
| 292 | `leviculum-nrf/src/remote_status.rs:158` | single-packet answer, PayloadTooLarge falls back to a resource, other errors drop | send result | `lnode_remote_status` | 900 | 2/1 |  |
| 293 | `leviculum-nrf/src/mgmt.rs:205` | publish the new allow-list, then request the save | - | `lnode_remote_status` | 900 | 2/1 |  |
| 294 | `leviculum-nrf/src/mgmt.rs:226` | revert the published list to the previous one | previous | none | - | - || no decision: the store that row 325's arm calls; when it is called (wired and Lost) is row 325 |
| 295 | `leviculum-nrf/src/mgmt.rs:254` | no record reports None | STORED | `lnode_remote_status` | 900 | 2/1 |  |
| 296 | `leviculum-nrf/src/usb.rs:66` | which control types are accepted and advertised | const list | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 297 | `leviculum-nrf/src/usb.rs:240` | GPREGRET via SoftDevice call or direct write, then reset | SD enabled | `reset_loop_quiet` | 300 | 1/1 |  |
| 298 | `leviculum-nrf/src/usb.rs:275` | the debug port drains only while DTR is up; the first DTR opens the drain gate | DTR | `lora_path_discovery_fast_mixed` | 90 | 2/1 | `leviculum_usb_policy` `tests/seam.rs`: `the_debug_port_is_written_only_while_dtr_is_up`<br>`the_first_host_opens_the_gate_before_the_headless_deadline` |
| 299 | `leviculum-nrf/src/usb.rs:288` | a drain write times out after 50 ms | USB | none | - | - | `leviculum_usb_policy` `tests/seam.rs`: `a_write_that_did_not_land_ends_the_drain`<br>`the_drain_bounds_are_todays_numbers` |
| 300 | `leviculum-nrf/src/usb.rs:348` | debug bytes: `p` postmortem, `s` stack reset, `c` heap census | RX bytes | none | - | - || `leviculum_usb_policy` `tests/seam.rs`: `each_command_byte_asks_for_its_command`<br>`a_terminal_line_runs_each_command_once`<br>`stray_input_asks_for_nothing` |
| 301 | `leviculum-nrf/src/usb.rs:347` | open the drain gate after 30 s headless | timer | none | - | - | `leviculum_usb_policy` `tests/seam.rs`: `a_headless_boot_opens_the_gate_at_thirty_seconds` |
| 302 | `leviculum-nrf/src/usb.rs:420` | legacy RESET_ACK vs envelope ack | legacy | `reset_loop_quiet` | 300 | 1/1 |  |
| 303 | `leviculum-nrf/src/usb.rs:445` | reset even if the plan did not | - | `reset_loop_quiet` | 300 | 1/1 |  |
| 304 | `leviculum-nrf/src/usb.rs:473` | never write with DTR down; abandon on DTR drop; retry on control change | DTR | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 305 | `leviculum-nrf/src/usb.rs:503` | ZLP when the framed length is a multiple of 64 (twin at 1352) | len | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 306 | `leviculum-nrf/src/usb.rs:556` | invalid radio config answers Invalid | wire | none | - | - || decision left: `RadioConfig::from_wire_config` refuses a bandwidth with no SX1262 register code, which `parse_radio_config` admits; the answer is `envelope::radio_config_answer`, host-tested |
| 307 | `leviculum-nrf/src/usb.rs:560` | LoRa not booted: persist only, Stored or StoreFailed | lora_booted, flash ack | none | - | - || `leviculum_usb_policy` `tests/seam.rs`: `a_boot_without_lora_takes_the_config_to_the_page_not_the_channel`<br>`a_lora_off_boot_whose_page_write_lands_answers_not_running`<br>`a_lora_off_boot_whose_page_write_is_lost_answers_not_persisted` |
| 308 | `leviculum-nrf/src/usb.rs:588` | channel full: the same config waits for apply; a different one gets one grace send, else Undeliverable | channel, pending_config | none | - | - || `leviculum_usb_policy` `tests/seam.rs`: `a_retry_of_the_queued_config_waits_for_its_apply_while_the_task_stalls`<br>`a_different_config_behind_a_stalled_task_gets_one_grace_send`<br>`a_full_slot_with_no_published_config_gets_the_grace_send` |
| 309 | `leviculum-nrf/src/usb.rs:624` | note delivery and request the save only if this call delivered | delivered_here | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 310 | `leviculum-nrf/src/usb.rs:639` | Applied vs Unconfirmed after the apply window | running_config, signal | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 311 | `leviculum-nrf/src/usb.rs:673` | skip persisting a frame that was not taken | took_it | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 312 | `leviculum-nrf/src/usb.rs:711` | 500 ms frame timeout mid-frame, else 3600 s | deframer | `lora_path_discovery_fast_mixed` | 90 | 2/1 | `leviculum_usb_policy` `tests/seam.rs`: `the_read_timeout_is_the_frame_timeout_only_inside_a_frame` |
| 313 | `leviculum-nrf/src/usb.rs:759` | a non-control frame is counted and forwarded with backpressure | channel | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 314 | `leviculum-nrf/src/usb.rs:770` | host-requested reset | action | `reset_loop_quiet` | 300 | 1/1 |  |
| 315 | `leviculum-nrf/src/usb.rs:815` | an invalid legacy config gets no answer | - | none | - | - || no decision: glue, the classifier names the frame invalid (`classify_control_frame`, host-tested) and the board only logs it, the legacy contract being ack-or-silence |
| 316 | `leviculum-nrf/src/usb.rs:826` | wall time: busy refusal while one is pending, else the main loop answers | WALL_TIME_CHANNEL | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 317 | `leviculum-nrf/src/usb.rs:853` | telemetry target: wired and key usable and delivered, persisted, answered | wired, key, channel | `board_delivery_promise` | 1800 | 3/1 (>2) |  |
| 318 | `leviculum-nrf/src/usb.rs:896` | fixed position: the same chain | wired, channel | none | - | - || no decision: glue, each clause is a host crate's: `fixed_position_answer` (wired, delivered, persist) and the persist outcome |
| 319 | `leviculum-nrf/src/usb.rs:926` | media profile persisted if wired, answered from running or configured | media state | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 320 | `leviculum-nrf/src/usb.rs:1012` | node name applied only if wired and the report is ready | name state | none | - | - || decision left: a board that cannot state its names yet applies nothing (`wired && report().is_some()`), so a busy retry is not a second write |
| 321 | `leviculum-nrf/src/usb.rs:1071` | announce: busy refusal while one is pending, else the main loop answers | ANNOUNCE_CHANNEL | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 322 | `leviculum-nrf/src/usb.rs:1095` | BLE TX gap applied and acked unconditionally | - | none | - | - || no decision: an unconditional ack, the bound is the classifier's |
| 323 | `leviculum-nrf/src/usb.rs:1120` | store storm ack vs BUSY | request_storm | none | - | - || `leviculum_record_log::board::tests::a_storm_is_refused_unmounted_running_empty_or_oversize` |
| 324 | `leviculum-nrf/src/usb.rs:1149` | PN config ack vs REFUSE_PERSIST, no wired gate | flash ack | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 325 | `leviculum-nrf/src/usb.rs:1181` | mgmt allow: persist if wired, revert on wired and Lost, report or UNSUPPORTED | wired, persist | `lnode_remote_status` | 900 | 2/1 |  |
| 326 | `leviculum-nrf/src/usb.rs:1218` | mgmt query: report or UNSUPPORTED | wired | `lnode_remote_status` | 900 | 2/1 |  |
| 327 | `leviculum-nrf/src/usb.rs:1248` | TX spacing ack vs BUSY | deliver_tx_spacing | none | - | - || no decision: glue, a full depth-1 channel answers BUSY, the shape every control hand-over shares |
| 328 | `leviculum-nrf/src/usb.rs:1284` | radio query: running config report or BUSY | running_config | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 329 | `leviculum-nrf/src/usb.rs:1290` | USB read error goes back to wait_connection | USB | none | - | - | `leviculum_usb_policy` `tests/seam.rs`: `a_disconnect_ends_the_session_in_or_out_of_a_frame`<br>`a_reset_frame_torn_by_a_disconnect_resets_on_the_retry` |
| 330 | `leviculum-nrf/src/usb.rs:1298` | frame timeout mid-frame drops the partial frame | deframer | none | - | - | `leviculum_usb_policy` `tests/seam.rs`: `a_timeout_drops_only_an_open_frame`<br>`a_reset_frame_torn_by_a_silent_host_resets_on_the_retry`<br>`without_the_step_the_torn_half_swallows_the_retry`<br>`an_idle_timeout_leaves_the_next_frame_alone` |
| 331 | `leviculum-nrf/src/usb.rs:1373` | outgoing packet: drop the rest when the host stops reading; ZLP only after a good write | DTR | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 332 | `leviculum-nrf/src/lib.rs:131` | VBUS assumed present at boot | - | `boot_smoke_t114` | 300 | 1/1 |  |
| 333 | `leviculum-nrf/src/lib.rs:314` | the panic count resets on a cold boot | retained magic | `reset_loop_quiet` | 300 | 1/1 |  |
| 334 | `leviculum-nrf/src/lib.rs:750` | panic-message ring linearisation | len | none | - | - | `leviculum_boot_trace::postmortem::tests::an_overlong_panic_keeps_its_tail_in_order`<br>`leviculum_boot_trace::postmortem::tests::a_message_that_fits_is_read_from_the_start`<br>`leviculum_boot_trace::postmortem::tests::the_window_wraps_at_whole_multiples_of_the_ring` |
| 335 | `leviculum-nrf/src/lib.rs:767` | replay a panic postmortem once at boot; the query accepts both states | magic | none | - | - | `leviculum_boot_trace::postmortem::tests::a_panic_is_replayed_by_one_boot_and_read_by_every_query`<br>`leviculum_boot_trace::postmortem::tests::a_word_that_is_not_this_records_magic_is_no_record` |
| 336 | `leviculum-nrf/src/lib.rs:841` | panic: no LED armed resets at once, else 25 blinks then reset | PANIC_LED_ARMED | none | - | - | `leviculum_boot_trace::postmortem::tests::without_an_led_a_fault_resets_at_once`<br>`leviculum_boot_trace::postmortem::tests::with_an_led_a_fault_blinks_twenty_five_times_then_resets`<br>`leviculum_boot_trace::postmortem::tests::the_blink_writes_the_pins_registers` |
| 337 | `leviculum-nrf/src/lib.rs:949` | HardFault postmortem take/peek, same rule | magic | none | - | - | `leviculum_boot_trace::postmortem::tests::a_hardfault_is_replayed_by_one_boot_and_read_by_every_query`<br>`leviculum_boot_trace::postmortem::tests::a_word_that_is_not_this_records_magic_is_no_record` |
| 338 | `leviculum-nrf/src/lib.rs:1082` | HardFault: no LED resets at once, else blink then reset | HARDFAULT_LED_ARMED | none | - | - | `leviculum_boot_trace::postmortem::tests::without_an_led_a_fault_resets_at_once`<br>`leviculum_boot_trace::postmortem::tests::with_an_led_a_fault_blinks_twenty_five_times_then_resets`<br>`leviculum_boot_trace::postmortem::tests::the_blink_writes_the_pins_registers` |

### Stores, sensors, display and boot plumbing (the remaining modules)

74 decisions no host test reached on 2026-10-10; 24 more were already a call into a host-built crate and are not listed. Since the ninth wave 16 of the 74 carry host tests, 13 of the 27 "none" rows are marked no decision or rig-only by nature, and 2 are decisions left (369, 374).

| # | Where | What it decides | State it reads | Cheapest rig cell | s | radios/boards | Host test |
|---|---|---|---|---|---|---|---|
| 339 | `leviculum-nrf/src/gnss.rs:154` | which init sequencer runs: UBX on ZOE-M8Q, `$PCAS` on L76K | wiring.module | `boot_smoke_t114` | 300 | 1/1 |  |
| 340 | `leviculum-nrf/src/gnss.rs:255` | RMC fold: valid sets lat/lon/time/speed/bearing, invalid clears only unix_secs | rmc, latest | none | - | - || `leviculum_gnss_time::fix::tests::a_valid_rmc_sets_position_time_and_movement`<br>`leviculum_gnss_time::fix::tests::an_invalid_rmc_clears_the_time_and_keeps_the_position`<br>`leviculum_gnss_time::fix::tests::an_invalid_rmc_on_an_unseeded_board_offers_the_gate_nothing` |
| 341 | `leviculum-nrf/src/gnss.rs:277` | GGA fold: valid is quality != NoFix; position, altitude, HDOP only on a fix | gga, latest | none | - | - || `leviculum_gnss_time::fix::tests::a_gga_with_a_fix_sets_position_altitude_and_hdop`<br>`leviculum_gnss_time::fix::tests::a_gga_without_a_fix_counts_satellites_and_keeps_the_rest` |
| 342 | `leviculum-nrf/src/gnss.rs:273` | hold the receiver power enable high, where the board has one | wiring.power_enable | `boot_smoke_t114` | 300 | 1/1 |  |
| 343 | `leviculum-nrf/src/gnss.rs:284` | hold the standby pin high | wiring.standby | `boot_smoke_t114` | 300 | 1/1 |  |
| 344 | `leviculum-nrf/src/gnss.rs:396` | rebuild the UART only for a different baud | pending_baud, configured_baud | `boot_smoke_t114` | 300 | 1/1 |  |
| 345 | `leviculum-nrf/src/gnss.rs:408` | init frames after the baud decision, at most 4 staged per loop, extras dropped | locked, sentences_seen | `boot_smoke_t114` | 300 | 1/1 |  |
| 346 | `leviculum-nrf/src/battery.rs:234` | power the divider only while sampling, 10 ms settle | divider_enable | `boot_smoke_t114` | 300 | 1/1 |  |
| 347 | `leviculum-nrf/src/battery.rs:393` | publish BATTERY_STATE every 5th sample | samples | `boot_smoke_t114` | 300 | 1/1 |  |
| 348 | `leviculum-nrf/src/battery.rs:396` | 30 s report, then reseed the window with the closing sample | samples | `boot_smoke_t114` | 300 | 1/1 |  |
| 349 | `leviculum-nrf/src/record_store.rs:177` | refuse a PN op while unmounted (the engine then forgets the id, no proof) | MOUNTED | none | - | - || no decision: early return; what the engine does with the refusal is `PnStore::op_done(false)`, host-tested in `leviculum-pn-store` |
| 350 | `leviculum-nrf/src/record_store.rs:243` | refuse a storm: unmounted, running, 0 records, oversize, slot full | MOUNTED, STORM_RUNNING, args | none | - | - || `leviculum_record_log::board::tests::a_storm_is_refused_unmounted_running_empty_or_oversize` |
| 351 | `leviculum-nrf/src/record_store.rs:302` | flash retry: 4 attempts at 50/100/200 ms, each failure counted | flash result | none | - | - || `leviculum_record_log::board::tests::a_refused_operation_is_tried_four_times_with_a_doubling_wait`<br>`leviculum_record_log::board::tests::an_erase_refused_three_times_lands_on_the_fourth`<br>`leviculum_record_log::board::tests::an_erase_refused_four_times_is_given_up_without_a_last_wait` |
| 352 | `leviculum-nrf/src/record_store.rs:394` | mount failure ends the task, every later request refused | mount result | none | - | - || `leviculum_record_log::board::tests::a_refused_format_leaves_no_store`<br>`leviculum_record_log::board::tests::a_region_that_does_not_fit_the_part_leaves_no_store` |
| 353 | `leviculum-nrf/src/record_store.rs:407` | serve a storm, a PN op or the stats tick | channels | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 354 | `leviculum-nrf/src/record_store.rs:462` | append failure counts a sealed page when room went from above 0 to 0 | append result, sector_room | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) || `leviculum_record_log::board::tests::an_append_refused_mid_record_seals_its_page`<br>`leviculum_record_log::board::tests::an_append_at_the_regions_limit_whose_reclaim_is_refused_seals_nothing` |
| 355 | `leviculum-nrf/src/record_store.rs:479` | purge only when the record at the offset still has the key | record_at, key | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 356 | `leviculum-nrf/src/record_store.rs:524` | format any region that is not ours; an error leaves no store this boot | mount result | none | - | - || `leviculum_record_log::board::tests::a_fresh_part_is_formatted_and_counts_nothing`<br>`leviculum_record_log::board::tests::a_strangers_bytes_are_formatted_over`<br>`leviculum_record_log::board::tests::a_reboot_finds_its_own_records`<br>`leviculum_record_log::board::tests::a_record_torn_by_a_power_cut_is_not_counted` |
| 357 | `leviculum-nrf/src/record_store.rs:556` | a failed live-count scan aborts the mount | log.count() | none | - | - || no decision: unreachable, a read of internal flash is a `memcpy` that cannot fail or be refused (`ReadNorFlash for Device`); the arm is the `?` on `count` in `leviculum_record_log::board::mount_or_format` |
| 358 | `leviculum-nrf/src/record_store.rs:607` | clamp the storm body to STORE_STORM_MAX_BYTES | size | none | - | - || `leviculum_record_log::board::tests::a_storm_record_is_clamped_to_its_buffer_and_carries_its_index` |
| 359 | `leviculum-nrf/src/record_store.rs:621` | stamp index bytes into a non-empty storm body | n, size | none | - | - || `leviculum_record_log::board::tests::a_storm_record_is_clamped_to_its_buffer_and_carries_its_index`<br>`leviculum_record_log::board::tests::a_storm_record_appends_and_reads_back_by_its_key` |
| 360 | `leviculum-nrf/src/record_store.rs:585` | storm append outcome and sealed-page count | append result | none | - | - || `leviculum_record_log::board::tests::an_append_refused_mid_record_seals_its_page`<br>`leviculum_record_log::board::tests::an_append_refused_before_its_first_write_seals_nothing`<br>`leviculum_record_log::board::tests::an_append_at_the_regions_limit_whose_reclaim_is_refused_seals_nothing`<br>`leviculum_record_log::board::tests::a_full_page_that_fails_is_not_sealed_a_second_time` |
| 361 | `leviculum-nrf/src/radio_store.rs:84` | radio save queue full: the newest request wins | PENDING | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 362 | `leviculum-nrf/src/radio_store.rs:100` | the same for request_save_confirmed | PENDING | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 363 | `leviculum-nrf/src/radio_store.rs:129` | an unchanged page skips the erase and acks Durable | page bytes | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 364 | `leviculum-nrf/src/radio_store.rs:144` | erase and write retried 3 times 250 ms apart | flash result | none | - | - || `leviculum_telemetry_policy::reporter::tests::the_radio_page_lands_on_its_third_try_or_is_lost_after_750_ms`<br>`leviculum_telemetry_policy::reporter::tests::a_flash_that_fails_three_times_is_lost_after_three_pauses` |
| 365 | `leviculum-nrf/src/radio_store.rs:168` | ack Durable or Lost to the waiting host | written | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 366 | `leviculum-nrf/src/boot_count.rs:123` | did the retained record survive | boot.prev | `reset_loop_quiet` | 300 | 1/1 |  |
| 367 | `leviculum-nrf/src/boot_count.rs:138` | erase the page first when the plan says so, then write | plan | `reset_loop_quiet` | 300 | 1/1 |  |
| 368 | `leviculum-nrf/src/button.rs:68` | no display state yet means the display is on | DISPLAY_ON_REQ | none | - | - || no decision: glue, the seed of row 369's state |
| 369 | `leviculum-nrf/src/button.rs:70` | display power: on and any press is off; off and 2 s or more is on; shorter ignored | press duration, state | none | - | - || decision left: the button's display toggle, a local UI no field fault reaches |
| 370 | `leviculum-nrf/src/display.rs:66` | OLED chip from a stabilised status read: SH1106 or SSD1306 | I2C reads | `boot_smoke_t114` | 300 | 1/1 |  |
| 371 | `leviculum-nrf/src/display.rs:90` | first ACKing address wins; unclassifiable means SSD1306 | probe results | `boot_smoke_t114` | 300 | 1/1 |  |
| 372 | `leviculum-nrf/src/display.rs:142` | which driver runs: SSD1306 init, SH1106 or none ends the task | detected kind | `boot_smoke_t114` | 300 | 1/1 |  |
| 373 | `leviculum-nrf/src/display.rs:201` | seed display power on | - | `boot_smoke_t114` | 300 | 1/1 |  |
| 374 | `leviculum-nrf/src/display.rs:220` | apply only a changed power request; an error keeps the old state | request, display_on | none | - | - || decision left: the display applies only a changed power request and keeps its state on an I2C error; a local UI |
| 375 | `leviculum-nrf/src/display.rs:240` | no frame work while the display is off | display_on | none | - | - || no decision: early return while the display is off |
| 376 | `leviculum-nrf/src/events.rs:76` | the first announce from a new neighbour is forwarded to the immediate-announce trigger | NodeEvent | `lora_path_discovery_slow_lnode_pair` | 90 | 2/2 |  |
| 377 | `leviculum-nrf/src/media.rs:144` | no flash profile means both carriers on | page | `boot_smoke_t114` | 300 | 1/1 |  |
| 378 | `leviculum-nrf/src/media.rs:222` | runtime apply: store state, wake BLE, then queue the save | profile | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 379 | `leviculum-nrf/src/name.rs:170` | an undecodable boot name is empty | BOOT_GAP | none | - | - || no decision: glue, `NodeName::decode` falling back to the empty name, which `boot_gap_name` documents as "no BLE name on the air" |
| 380 | `leviculum-nrf/src/name.rs:217` | runtime set or clear, then persist | name | none | - | - || no decision: glue, a store and a save request; the answer is `envelope::node_name_answer`, host-tested |
| 381 | `leviculum-nrf/src/transport_stats.rs:113` | stats due: re-arm from now (no catch-up), close the speed windows, emit | next, now | `lnode_remote_status` | 900 | 2/1 |  |
| 382 | `leviculum-nrf/src/iface_bytes.rs:168` | a window under 1 s keeps the previous speed | at_ms, now | `lnode_remote_status` | 900 | 2/1 |  |
| 383 | `leviculum-nrf/src/iface_bytes.rs:194` | an out-of-range interface index is ignored (also :182) | iface | none | - | - || no decision: unreachable guard, every caller passes one of the three registered interface indices |
| 384 | `leviculum-nrf/src/identity.rs:117` | hashes not published yet answer None (REFUSE_BUSY) | HASHES | `lora_path_discovery_fast_mixed` | 90 | 2/1 |  |
| 385 | `leviculum-nrf/src/identity.rs:62` | whether a PN hash exists (sizes the announce budget) | PN_HASH | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 386 | `leviculum-nrf/src/flash.rs:39` | an identity that does not encode returns Ok with nothing written | identity | none | - | - || no decision: unreachable, the board saves only an identity it generated or loaded with its private key, and `encode_identity` fails only without one |
| 387 | `leviculum-nrf/src/flash.rs:43` | always erase then write, no read-compare | - | `boot_smoke_t114` | 300 | 1/1 |  |
| 388 | `leviculum-nrf/src/qspi.rs:497` | an Untouched part uses single-line FASTREAD/PP | part.quad | `boot_smoke_solarnode` | 300 | 1/1 |  |
| 389 | `leviculum-nrf/src/qspi.rs:518` | wake failure: release-failed, no device | QSPI result | none | - | - || no decision: early return at boot, rig-only by nature: a QSPI peripheral error on the wake command |
| 390 | `leviculum-nrf/src/qspi.rs:531` | ID read failure: read-failed, no device | QSPI result | none | - | - || no decision: early return at boot, rig-only by nature: a QSPI peripheral error on the ID read |
| 391 | `leviculum-nrf/src/qspi.rs:545` | an all-zero first ID gets one retry read | first | `boot_smoke_solarnode` | 300 | 1/1 || `leviculum_qspi_boot` `tests/identify.rs`: `a_part_that_names_itself_is_expected`<br>`a_part_woken_by_the_first_read_is_expected_on_the_second` |
| 392 | `leviculum-nrf/src/qspi.rs:558` | ID mismatch: no device; zeros are no-answer, else unexpected-part | jedec | none | - | - || `leviculum_qspi_boot` `tests/identify.rs`: `a_part_woken_by_the_first_read_is_expected_on_the_second`<br>`two_silent_reads_are_no_answer`<br>`a_foreign_part_is_unexpected_on_either_read`<br>`a_part_that_reads_all_ones_is_a_foreign_part_not_silence` |
| 393 | `leviculum-nrf/src/qspi.rs:581` | StatusBit6 part with QE clear: write status and verify; failure, no device | status byte | `boot_smoke_solarnode` | 300 | 1/1 |  |
| 394 | `leviculum-nrf/src/qspi.rs:1142` | mount only after a successful identify | identify result | `boot_smoke_solarnode` | 300 | 1/1 |  |
| 395 | `leviculum-nrf/src/qspi.rs:1150` | over budget: leak the future, release by hand, log, keep booting | with_timeout | none | - | - || rig-only by nature: a hung QSPI transfer; the deadline and its line are `leviculum_qspi_boot::outcome` and `Timeout`, host-tested |
| 396 | `leviculum-nrf/src/heap_census.rs:254` | refuse to boot when the budget exceeds HEAP_SIZE or links are fewer than BLE sessions | budget values | `boot_smoke_t114` | 300 | 1/1 |  |
| 397 | `leviculum-nrf/src/heap_census.rs:272` | largest allocatable block by binary search (feeds the live serve cap) | allocator | `lora_pn_board_offer_past_the_link` | 3600 | 3/2 (>2) |  |
| 398 | `leviculum-nrf/src/heap_census.rs:126` | census due or requested by debug byte `c` | next, REQUESTED | `ble_pn_board_upload` | 1800 | 3/2 (>2) |  |
| 399 | `leviculum-nrf/src/log.rs:169` | drop gated lines until DTR or 30 s and count them | RUNTIME_DRAIN_OPEN | `boot_smoke_t114` | 300 | 1/1 |  |
| 400 | `leviculum-nrf/src/log.rs:365` | the same gate for tracing events | RUNTIME_DRAIN_OPEN | `boot_smoke_t114` | 300 | 1/1 |  |
| 401 | `leviculum-nrf/src/log.rs:348` | tracing level filter at DEBUG | metadata | `boot_smoke_t114` | 300 | 1/1 |  |
| 402 | `leviculum-nrf/src/log.rs:99` | ring full overwrites the oldest bytes | wp, rp | none | - | - || `leviculum_boot_trace::tail::tests::a_writer_that_laps_the_reader_moves_it_to_the_oldest_byte`<br>`leviculum_boot_trace::tail::tests::the_lap_is_found_across_the_counters_wrap` |
| 403 | `leviculum-nrf/src/log.rs:277` | a foreign tail magic zeroes and re-arms the persistent tail | retained magic | `reset_loop_quiet` | 300 | 1/1 || `leviculum_boot_trace::tail::tests::power_on_ram_and_a_foreign_magic_rearm_the_tail` |
| 404 | `leviculum-nrf/src/log.rs:313` | replay last boot's tail only with a valid magic, wrap handled, magic cleared | retained RAM | `reset_loop_quiet` | 300 | 1/1 || `leviculum_boot_trace::tail::tests::without_the_magic_nothing_is_replayed`<br>`leviculum_boot_trace::tail::tests::a_tail_that_did_not_go_round_is_replayed_from_the_start`<br>`leviculum_boot_trace::tail::tests::a_tail_that_went_round_keeps_its_last_ring_of_bytes`<br>`leviculum_boot_trace::tail::tests::a_torn_write_position_still_reads_inside_the_ring` |
| 405 | `leviculum-nrf/src/log.rs:333` | open the gate once and report the dropped count | swap | `lora_pn_board_sync` | 1800 | 4/2 (>2) |  |
| 406 | `leviculum-nrf/src/rng.rs:130` | an empty request returns | dest | `boot_smoke_t114` | 300 | 1/1 |  |
| 407 | `leviculum-nrf/src/rng.rs:142` | a successful SoftDevice call supplies the bytes | return code | `boot_smoke_t114` | 300 | 1/1 |  |
| 408 | `leviculum-nrf/src/rng.rs:104` | take min(available, wanted); a lost race polls again | avail | `boot_smoke_t114` | 300 | 1/1 |  |
| 409 | `leviculum-nrf/src/vext.rs:63` | a second raise returns after rewriting the deadline | RAIL | `boot_smoke_t114` | 300 | 1/1 |  |
| 410 | `leviculum-nrf/src/vext.rs:79` | no rail, no warm-up wait | WARM_AT_MS | `boot_smoke_t114` | 300 | 1/1 |  |
| 411 | `leviculum-nrf/src/store_spike.rs:109` | spike build: stop at the first failing step (record-log candidate) | flash results | none | - | - || rig-only by nature: the `store-spike` instrument build, in no shipping image |
| 412 | `leviculum-nrf/src/store_spike.rs:144` | spike build: the same for sequential-storage | flash results | none | - | - || rig-only by nature: the `store-spike` instrument build, in no shipping image |
See also: [The firmware's host-test
seam](../concepts/firmware-host-test-seam.md) for the rule and how it is
gated.
