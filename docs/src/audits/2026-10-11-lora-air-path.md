# The LoRa air path, read against the reference (2026-10-11)

An audit in the shape of the propagation-node and BLE audits: three
implementations of "the same" channel access, airtime accounting and
duty rules — the host's RNode interface, the LNode firmware's air path,
and the RNode reference firmware — read side by side against what a
Python-RNS 1.3.5 peer expects, with every divergence pinned to a line on
both sides. Nothing was changed; this document is the deliverable.

Reference trees: `reference/Reticulum` (RNS 1.3.5) and
`reference/RNode_Firmware` (the pinned mirror; this version keeps its
radio logic in `RNode_Firmware.ino` and `Utilities.h` — there is no
`Radio.cpp` in it). Every `path:line` below is checked by the citation
guard on each run, so a figure that drifts goes red rather than stale.

Each divergence names: ours and theirs by line, what differs **on the
air**, a class — *deviation ours (deliberate)*, *deviation ours
(unintended)*, *reference defect*, *spec gap* — the open issue or red
cell it bears on, and the test that would show it.

## 1. CSMA: slot, DIFS, contention window, carrier sense

Where the three agree, verified: the slot is 12 symbol times clamped to
[24, 100] ms with the 6 ms fast floor (`csma_slot_ms`,
`leviculum-core/src/rnode.rs:1051`, against
`reference/RNode_Firmware/Utilities.h:1244-1252`, pinned by exhaustion
over the reference's own PHY grid); DIFS is two slots with SIFS 0
(`JITTER_DIFS_SLOTS`, `leviculum-nrf/channel-access/src/lib.rs:165`,
against `reference/RNode_Firmware/Config.h:119`); the preamble
derivation is the reference's duration-targeted one
(`derive_preamble_symbols`, `leviculum-core/src/rnode.rs:964`). The
host interface and the LNode draw from one shared policy crate
(`leviculum-channel-access`), so those two cannot diverge from each
other.

**D1 — the hosts differ in kind: Python writes immediately, ours runs
channel access.** Python's `process_outgoing` writes the KISS frame to
the serial port the moment it is asked
(`reference/Reticulum/RNS/Interfaces/RNodeInterface.py:708-728`; the
Android variant is the same model), leaving all channel access to the
modem — and the modem runs its CSMA once per queue drain and then
flushes everything it holds with no carrier sense between frames
(`flush_queue`, `reference/RNode_Firmware/RNode_Firmware.ino:575`; the
decision at `:1644` makes the full flush the behaviour at every bitrate
below 14 kbps, i.e. every PHY we run). Our host instead serves an
acquisition jitter per channel acquisition (`arm_owed_jitter_ms`,
`leviculum-std/src/interfaces/rnode.rs:740`) and holds the next frame
for the previous one's priced busy time (`tx_hold`,
`leviculum-std/src/interfaces/rnode.rs:1063`), so the modem's queue
never holds more than one frame and every frame gets its own CSMA
contest. On the air: a Python host with a deep queue keys bursts deaf,
back to back; ours does not. Class: deviation ours (deliberate, the
core of #36/#40/#430). Bears on: the rotation reds (`sent=10 recv=4`,
2026-09-23) that motivated it; #438. Tests: pinned throughout
`leviculum-std/src/interfaces/rnode.rs` and
`docs/src/concepts/csma-transmit-window.md`.

**D2 — the band-1 window is one slot narrower than a freshly booted
reference.** Ours draws uniformly over 0..=13 slots (`JITTER_CW_SLOTS`,
`leviculum-nrf/channel-access/src/lib.rs:169`). The reference boots with
`cw_max` declared as 15 (`reference/RNode_Firmware/Config.h:127`) and
only a band excursion rewrites it to 14 (`update_csma_parameters`,
`reference/RNode_Firmware/RNode_Firmware.ino:1603-1619`), so a
reference board draws over 0..=14 until its first excursion above 7 %
airtime and over 0..=13 after it; Arduino `random` is upper-exclusive
(`tx_queue_handler`, `reference/RNode_Firmware/RNode_Firmware.ino:1626`).
On the air: one slot of extra spread on a fresh reference neighbour.
Class: deviation ours (deliberate, we mirror the post-excursion
window). Known and open — `docs/src/concepts/csma-transmit-window.md`
question 2 / open item 2. Test: none needed beyond the existing pins;
changing it is a radio-behaviour change wanting a rig A/B.

**D3 — no airtime-band escalation; a doubling CAD window instead.** The
reference widens its contention window by short-term airtime band, up
to 45..=58 slots at 85 % airtime
(`reference/RNode_Firmware/RNode_Firmware.ino:1603-1619`,
`reference/RNode_Firmware/Config.h:108-112`). Ours keeps band 1 and
doubles a separate contention window per observed CAD-busy, 2 up to 64
slots (`CAD_CW_MAX`, `leviculum-nrf/channel-access/src/lib.rs:122`). On
the air: on a channel that is *loaded on average but momentarily
clear*, a reference neighbour defers wide and we do not — our node wins
more than its share of contests against loaded RNode neighbours, and
the asymmetry grows with load. Class: deviation ours (deliberate; the
crate docs argued the substitution). Bears on: periculum#61 (the channel-loaded note) — this
is the one CSMA divergence that only shows under sustained load, which
no current cell measures. Test that would show it: an emulator census
with asymmetric offered load (one leviculum node, one scripted
reference-policy node, airtime held above 7 %), counting contest wins;
then one rig window on the dual-pair cell with background traffic.
Fixed in 20241367: the board draws in the reference's band for its own
short airtime (the input is our keyed share, not the channel load), and
the `[LORA_JITTER]` and `[LORA_CSMA_TX]` lines carry `band=`; tests
`the_band_follows_the_reference_map_to_the_percent` and
`an_acquisition_draws_in_the_band_of_its_own_airtime`. One figure above
is off: with the map's `+ 7` shift band 4 starts at 78 % airtime, not
85 %. The CAD gate's doubling window stays as a second mechanism.

**D4 — the wait, interrupted: the reference freezes its countdown, we
keep a debt.** The reference's CW countdown pauses while the medium is
busy and resumes where it stood (`cw_wait_passed` survives the
interruption, `reference/RNode_Firmware/RNode_Firmware.ino:1641`; a
busy medium restarts only DIFS, `:1635`). Our board spends the draw
listening and, when a reception cuts the window short, keeps the
remainder owed and serves it on the next turn
(`acquisition_jitter_ms` / `jitter_spent`,
`leviculum-nrf/channel-access/src/lib.rs:443-465`;
`leviculum-nrf/src/lora.rs:2052-2085`) — same shape, same reason. Our
*host* serves its drawn wait as an uninterruptible sleep
(`leviculum-std/src/interfaces/rnode.rs:2790-2805`): it cannot sense
the medium, the modem under it can and does. On the air: equivalent
within one draw. Class: agreement in substance; noted because the three
spell it differently.

**D5 — bounded forced TX vs the reference's unbounded wait.** After 8
busy CADs ours transmits anyway (`CAD_MAX_RETRIES`,
`leviculum-nrf/channel-access/src/lib.rs:111`); the reference waits
indefinitely and lets the airtime lock be the only bound
(`tx_queue_handler`, `reference/RNode_Firmware/RNode_Firmware.ino:1623`
has no give-up). On the air: under a permanently keyed interferer, our
node eventually talks over it where a reference node starves silently.
Class: deviation ours (deliberate; documented in the crate header,
`leviculum-nrf/channel-access/src/lib.rs:16-26` — a silent starvation
is indistinguishable from packet loss to every layer above). Test:
host-level scripted-CAD tests exist; the starving branch is untestable
on the reference by construction.

**D6 — what "busy" means: latched preamble IRQ plus an RSSI term,
versus a CAD probe.** The reference's `medium_free()` is two terms:
`dcd` — on SX126x a *latched* preamble/header IRQ read-back
(`reference/RNode_Firmware/sx126x.cpp:495-521`), on SX127x a live
`SIG_DETECT|SIG_SYNCED` register read
(`reference/RNode_Firmware/sx127x.cpp:197-204`) — and interference
avoidance, current RSSI above its sampled noise floor plus 11 dB
(`update_modem_status`,
`reference/RNode_Firmware/RNode_Firmware.ino:1394-1431`,
`CSMA_INFR_THRESHOLD_DB`, `reference/RNode_Firmware/Config.h:113`).
Our board probes with the SX1262's CAD per attempt
(`leviculum-nrf/src/lora.rs:2098`) and keeps **no noise floor, no
interference detection, and no DCD sampling at all**. Three on-air
consequences: (a) a reference node defers to non-LoRa interference, we
do not; (b) the reference's latched preamble holds "busy" for up to a
preamble-plus-header time after a false detect
(`reference/RNode_Firmware/sx126x.cpp:508`), where our fresh CAD does
not; (c) a host driving an LNode gets no `channel_load_*` or noise
floor in its stats, because the board never measures them — an RNode
supplies all three (`kiss_indicate_channel_stats`,
`reference/RNode_Firmware/Utilities.h:959`). Class: (a)/(b) deviation
ours (deliberate at CAD adoption); (c) **spec gap ours** — the
LNode-side blindness is nowhere decided, and periculum#61's
channel-load question cannot even be asked of an LNode. Bears on: #431
(its instrument needs the receive side's view of the channel), and the
RSSI carrier-sense experiment: the arm the rig A/B'd is exactly the
reference's interference-avoidance term (toggled per modem via
`CMD_DIS_IA`, read back from the EEPROM DIA byte), ON being the stock
default, and it has been OFF on all four rig modems since 2026-09-26
after a factor-of-~7 loss on a held-responder cell
(`periculum/hardware/KNOWN-REDS.toml`, the
`bench_single_pair_slow_ca_rnode_only` entry). Test for (c): none
exists; the fix is a feature (a cheap RSSI sample in the RX window into
a DCD-like ring), and the first test is `lnstatus` showing a non-empty
channel-load row against an LNode.

## 2. The airtime ledgers

Where the three agree, verified: two 7 500 ms bins over a 15 s
short-term window and a 480-bin rolling hour
(`AIRTIME_BINLEN_MS`, `leviculum-core/src/rnode.rs:1506`, against
`reference/RNode_Firmware/Config.h:176-183`); the lock engages at
`>= limit` with 0 and anything ≥ 100 % read as unlimited
(`alock_u16_to_fraction`, `leviculum-core/src/rnode.rs:1519`, against
`reference/RNode_Firmware/RNode_Firmware.ino:948-951` and
`:1673-1675`); the lock defers and never discards, in all three.

**D7 — the per-frame cost formula differs by four symbol times and the
rounding.** The reference charges
`preamble_symbols + 0.25 + 8` plus the payload term, in float, with no
ceiling (`add_airtime`,
`reference/RNode_Firmware/RNode_Firmware.ino:654-691`). Ours charges
the Semtech formula — preamble tail 4.25 symbols, payload term
ceiling-divided, each packet rounded up to the millisecond
(`airtime_ms_with_preamble`, `leviculum-core/src/rnode.rs:1127`;
charged via `frame_airtime_cost_ms`,
`leviculum-core/src/rnode.rs:1733`). Ours therefore books ~4 symbol
times more per packet — 8 ms at SF8/125 kHz, 131 ms at SF12/125 kHz —
and our lock engages marginally earlier at the same cap. On the air:
nothing a peer can see; a few per mille of duty budget in the lawful
direction. Class: deviation ours (deliberate — the ledger is the legal
meter, and it must not undercount). The host's accounting observable
already refuses to threshold on the difference (`judge_airtime`,
`leviculum-std/src/interfaces/rnode.rs:1319`, "any rise is Keyed").
Test: a unit comparing the two formulas over the reference's PHY grid
would pin the sign (ours ≥ theirs); nothing today asserts it.

**D8 — the reference re-counts skipped bins; ours retires them.** The
reference zeroes only the bin *after* the current one on each add and
each update (`reference/RNode_Firmware/RNode_Firmware.ino:688` and
`:698`), which is sound only while the loop lands in every bin: a bin
skipped — a loop parked longer than 7.5 s — keeps the airtime it held
an hour earlier and is counted again. Ours clears the whole skipped
range (`retire_skipped_bins`, `leviculum-core/src/rnode.rs:1950`). On
the air: a reference node can hold a duty lock on air it spent in the
*previous* hour; ours cannot. Class: reference defect (leviculum#495),
deviation ours to fix it. Test: exists —
`leviculum-nrf/queue-budget/src/tests.rs` and the mvr pin the lift.

**D9 — our extensions on top of the shared lock: the 18 s stale rule,
the duty-hold signal, the propagation-node share.** All three are ours
alone: a frame held by the lock past `HOLD_MAX_AGE_MS`
(`leviculum-nrf/queue-budget/src/lib.rs:283`) is dropped instead of
keyed (#433); the hold is reported to the core and suppresses relayed
announces and path responses (leviculum#493,
`announce_allowed_on_interface`,
`leviculum-core/src/transport.rs:9408-9430`); a propagation node stops
originating at half the long-term cap (`OWN_TRAFFIC_SHARE_PERMILLE`,
`leviculum-nrf/queue-budget/src/lib.rs:525`, leviculum#494). The
reference holds silently, forever, and signals nothing over KISS — the
host-side inference from CMD_READY gate closures (`DutyHolds`,
`leviculum-nrf/queue-budget/src/lib.rs:358`;
`duty_hold_min_ms`, `leviculum-std/src/interfaces/rnode.rs:189`) exists
*because* of that silence. On the air: we key current traffic where the
reference keys fossils (measured 145.6 s old on 2026-09-27), and we
withhold route advertisements a held relay cannot honour. Class:
deviation ours (deliberate, documented in
`docs/src/concepts/regulatory-airtime.md`). Bears on: #433, #384.
Tests: exist (queue-budget tests, the duty-lock rig cell, the mvrs).

## 3. The TX queue and flush behaviour

**D10 — overflow: silent drop there, named refusal here.** The
reference bounds its queue twice (200 packets / 6 144 bytes,
`reference/RNode_Firmware/Boards.h:836-837` for the T114) and drops
silently on overflow — the frame-accept path and the per-byte path both
lack an else (`serial_callback`,
`reference/RNode_Firmware/RNode_Firmware.ino:769-783` and `:805-809`).
Our board reserves room first and answers `BufferFull` naming which
bound refused (`outbound_verdict`,
`leviculum-nrf/queue-budget/src/lib.rs:636`); our host bounds its
flow-control queue at 64 frames with counted, named drops
(`FLOW_CONTROL_QUEUE_LIMIT`,
`leviculum-std/src/interfaces/rnode.rs:843`). On the air: identical —
the difference is whether anyone can see the loss. Class: reference
defect (#344's finding); deviation ours to fix it. Note one mirror-image
hazard: the RNode's silent drop is *upstream* of our host's accounting,
which is exactly what the CMD_READY query protocol and the
airtime-ledger settlement (`settle_handovers`,
`leviculum-std/src/interfaces/rnode.rs:1427`) exist to catch.

**D11 — flush under the cap.** When the airtime lock lifts, the
reference's next `tx_queue_handler` pass flushes its **whole queue** in
one acquisition (`should_flush`,
`reference/RNode_Firmware/RNode_Firmware.ino:1644-1645`); every frame
it aged under the lock goes out, oldest first, deaf. Ours: the host
never lets the modem hold more than one frame (D1), sweeps frames the
hold aged past 18 s (`pop_live_frame`,
`leviculum-std/src/interfaces/rnode.rs:241`), and the board drops stale
frames at dequeue (`leviculum-nrf/src/lora.rs:1933-1977`). On the air: a
reference relay exiting a duty hold emits a burst of minutes-old
traffic; ours emits one current frame. Class: deviation ours
(deliberate; #433). Test: exists (the stale-drop mvr and the field
measurement in `docs/src/concepts/regulatory-airtime.md`).

## 4. The announce cap and its release

Where we match Python, verified: 2 % default cap over the interface
bitrate (`DEFAULT_ANNOUNCE_CAP_PERCENT`,
`leviculum-core/src/constants.rs:398`, against
`reference/Reticulum/RNS/Reticulum.py:114`); local announces
(`hops == 0`) bypass the cap (`leviculum-core/src/transport.rs:12290`,
against `reference/Reticulum/RNS/Transport.py:1246-1250`); the queue
holds one slot per destination, a newer emission replacing an older
(`leviculum-core/src/transport.rs:12488-12506`, against
`reference/Reticulum/RNS/Transport.py:1267-1281`); release picks lowest
hops, then oldest (`drain_announce_queues`,
`leviculum-core/src/transport.rs:12651-12661`, against
`reference/Reticulum/RNS/Interfaces/Interface.py:339-343`).

**D12 — the cap's bitrate is the effective airtime rate, not the
nominal symbol rate.** `announce_cap_bitrate_bps`
(`leviculum-core/src/rnode.rs:1779`) prices a 183-byte announce by the
airtime it actually costs; Python registers the nominal bitrate
(`updateBitrate`,
`reference/Reticulum/RNS/Interfaces/RNodeInterface.py:693-699`), which
at SF10/125 kHz buys a third less silence than it believes. Class:
deviation ours (deliberate, #402/#404; the deviation argument is in the
function's docs). Test: exists in core.

**D13 — the release draw.** After each capped emission ours adds a
randomised draw to the holdoff (`cap_release_draw_ms`,
`leviculum-core/src/transport.rs:10442`, applied at `:12433` and
`:12613`); Python re-arms its timer at exactly the holdoff
(`reference/Reticulum/RNS/Interfaces/Interface.py:356-358`), so two
saturated relays on one channel drain on two grids of the same period
and keep their first collision's offset for the whole backlog — eight
pairs within 1.1 ms on `lora_dual_cluster_rust` at 3032da10. Class:
deviation ours (deliberate, leviculum#560). Test: exists.

**D14 — a fresh announce can jump the cap queue (unintended).** Python
transmits immediately only when the holdoff has passed **and the queue
is empty** (`if not queued_announces and outbound_time > ...`,
`reference/Reticulum/RNS/Transport.py:1256-1257`); a non-empty queue
forces queueing, so release order is always the min-hops draw. Ours
transmits immediately whenever `now >= cap.allowed_at_ms`
(`leviculum-core/src/transport.rs:12471`, before 9b4515b1) **without consulting the
queue**, and the queue only drains on the poll cadence
(`drain_announce_queues`, `leviculum-core/src/transport.rs:12610`,
called from `poll`, `leviculum-core/src/transport.rs:4614`). In the
window between the holdoff elapsing and the next poll — up to one poll
interval, per holdoff cycle — a fresh announce, whatever its hop count,
overtakes every queued one and re-arms the holdoff on top of them. On the air: hop-priority
inversion under a saturated LoRa cap, and extra queue delay for exactly
the low-hop announces the selection rule exists to favour. Class:
**deviation ours, unintended** — nothing in the code or docs claims it,
and it contradicts the min-hops dequeue eleven lines below. Bears on:
announce propagation under load (#384's propagation goal; any
dual-cluster saturation cell). Test that would show it: a core unit —
cap armed and queue non-empty, advance the clock past `allowed_at_ms`
without running the periodic job, broadcast a fresh 3-hop announce,
assert the queued 1-hop announce still leaves first.
Fixed in 9b4515b1: the immediate send also requires an empty queue, in
the broadcast path and the peer re-offer path alike; test
`fresh_announce_does_not_jump_the_cap_queue`.

**D15 — queued announces never go stale (unintended).** Python drops
queue entries older than `QUEUED_ANNOUNCE_LIFE` = 24 h at release time
(`reference/Reticulum/RNS/Interfaces/Interface.py:330-337`,
`reference/Reticulum/RNS/Reticulum.py:112`). Our drain has no age check
(`leviculum-core/src/transport.rs:12610-12675`): an entry leaves only
by being sent or replaced by a newer emission. The per-destination slot
bounds the damage, but a destination that announced once and went away
stays queued indefinitely, and at a saturated LoRa cap (a 183-byte
announce ≈ 100 s of holdoff at SF10) the backlog horizon is days, not
the hours Python serves. On the air: we eventually key announces a
Python relay would have discarded as dead. Class: **spec gap ours**.
Test: core unit — queue an announce, advance the clock past 24 h, run
the drain, assert it is dropped rather than transmitted.
Fixed in 9b4515b1: `QUEUED_ANNOUNCE_LIFE_MS` (24 h) prunes the queue at
release with an `ANN_QUEUE_STALE` debug line; test
`queued_announce_older_than_its_life_is_dropped_at_release`.

## 5. The receive window after TX

**D16 — continuous RX there, windowed RX here.** The reference returns
to continuous receive immediately after every queue drain — the
`lora_receive` call at `reference/RNode_Firmware/RNode_Firmware.ino:599`
(and its twin at `:640`) — and does nothing else: no ack window, no
burst bound, no peer yield.
`CSMA_POST_TX_YIELD_SLOTS` (`reference/RNode_Firmware/Config.h:103`) is
defined and **used nowhere** in the pinned firmware — see the wrong
claim below. Our board runs windowed RX with an explicit post-TX
listening window of one reply airtime plus the peer's whole key-up wait
(`post_tx_rx_window_ms`, `leviculum-nrf/channel-access/src/lib.rs:250`,
the #423 fix), burst bounds of 16 frames / 8 s
(`leviculum-nrf/src/lora.rs:855-860`), and a peer-turn yield after two
empty ack windows (`leviculum-nrf/src/lora.rs:2257`). The window
teardown/adoption/harvest discipline (`leviculum-nrf/rx-arming`) exists
only because our RX is windowed; the reference has no teardown to get
wrong. On the air: ours trades continuous listening for bounded turns;
every residual loss of a frame that arrives *during our own
turn-taking* is a failure mode the reference cannot have. Class:
deviation ours (deliberate at the architecture level; the residue is
measured, not argued — `[SX_RX_ADOPT]`/`[SX_RX_TEARDOWN]` carry the
counterfactual). Bears on: **#431 directly** — "single frames lost
between two quiet radios: the receiving modem branch" is, on this
reading, most plausibly a window-boundary loss class, and the rx-arming
lines are the instrument that decides it. Also #423, #426. Test: the
teardown/adopt census over a rig window of the single-pair cell, arm
OFF (the KNOWN-REDS entries say which runs are comparable).

**D17 — what is still open on the window.** Whether the reply-airtime
term belongs in the window at all is open
(`docs/src/concepts/csma-transmit-window.md`, "What is still open"
item 1); dropping it would cut the SF10 window from 3 948 ms to
1 570 ms of listening per yield. Not re-argued here; the audit only
confirms the open item is still real in the code as read.

## 6. Frame sizing and the 508-byte case

**D18 — the two firmwares key different packet sequences for the same
handover.** The RNode firmware writes a header byte and flushes a
packet every time its counter reaches 255, so a 508-byte host frame —
exactly 2 × 254 — keys **three** packets of 255, 255 and 1 bytes, the
third carrying the header alone (`transmit`,
`reference/RNode_Firmware/RNode_Firmware.ino:716-751`, modelled by
`keyed_packet_layout`, `leviculum-core/src/rnode.rs:1199`). Our LNode
splits into at most **two** frames of up to 255 bytes with one shared
header (`build_lora_frames`, `leviculum-core/src/rnode.rs:2069`), so
the same 508 bytes cost two preambles instead of three. Both reassemble
correctly at every receiver — the reference's four RX cases
(`reference/RNode_Firmware/RNode_Firmware.ino:374-446`) and ours
(`SplitReassembler`, `leviculum-core/src/rnode.rs:2097`) deliver
254+254 and ignore nothing that matters. On the air: different airtime
for the same packet; the host prices the RNode's version
(`host_frame_airtime_ms`, `leviculum-core/src/rnode.rs:1231`, the #430
fix) and the board prices its own (`packet_airtime_ms`,
`leviculum-core/src/rnode.rs:1702`), and the two functions coexist in
one file *because* the two modems genuinely differ. Class: deviation
ours (deliberate and wire-compatible). Bears on: #430 (closed by the
hold fix; the model split is its residue).

**D19 — the reference's header-only third packet leaves a receiving
reference node primed to truncate.** After delivering 254+254, the
reference receiver takes the trailing header-only packet as the *first
part of a new split* (`reference/RNode_Firmware/RNode_Firmware.ino:374-391`
with an empty payload) and latches its sequence. The next split packet
whose 4-bit sequence collides (p = 1/16) is then treated as the second
part and delivered truncated, while its real second half dangles. Our
reassembler is immune — a frame under 2 bytes is ignored
(`leviculum-core/src/rnode.rs:2122-2124`). Reachable only when a host
hands over an exact multiple of 254 bytes; Reticulum's own 500-byte MTU
never does, test tooling at `HW_MTU` = 508 does. Class: reference
defect, edge. Bears on: nothing open; recorded so a future
508-byte-frame red on a mixed pair is not hunted as our loss. Test: the
periculum emulator can script the 255/255/1 sequence plus a same-seq
split pair and assert the truncation on the reference model — worth
doing only if such a red ever appears.

## 7. The stat commands and what the host reads back

Verified byte-for-byte against the firmware's indications: CHTM
(2+2+2+2+1+1+1, scaled 100×100, RSSI offset 157;
`decode_channel_stats`, `leviculum-core/src/rnode.rs:610`, against
`kiss_indicate_channel_stats`,
`reference/RNode_Firmware/Utilities.h:959-982`), PHYPRM (six u16s;
`decode_phy_params`, `leviculum-core/src/rnode.rs:694`, against
`reference/RNode_Firmware/Utilities.h:996-1013`), CSMA stats (band,
cw_min, cw_max; consumed into the hold pricing via `FirmwareCsma`,
`leviculum-std/src/interfaces/rnode.rs:915` — Python only logs them),
RSSI/SNR (offset 157, SNR × 0.25; `apply_radio_stat`,
`leviculum-std/src/interfaces/rnode.rs:1941`, matching
`reference/Reticulum/RNS/Interfaces/RNodeInterface.py:878-880`), and
the CMD_READY query protocol
(`reference/RNode_Firmware/RNode_Firmware.ino:1003-1008`,
`reference/RNode_Firmware/Utilities.h:1157-1169`).

Two asymmetries, neither a defect of ours:

* **CMD_STAT_CSMA is edge-triggered** — the firmware sends it only when
  the band changes (`reference/RNode_Firmware/RNode_Firmware.ino:1614-1618`),
  so a run that stays in band 1 never reports `cw_max` and the hold
  falls back to its own derivation. Already documented at
  `FirmwareCsma` (`leviculum-std/src/interfaces/rnode.rs:895-923`).
* **Python's CMD_STAT_RX/TX parsers are dead code that would crash.**
  They call ord() on what Python 3 already hands them as an int, which
  raises a TypeError
  (`reference/Reticulum/RNS/Interfaces/RNodeInterface.py:861` and
  `:875`) — inside the read loop's catch-all, so one such frame would
  take the whole interface offline. It never fires because the host
  never requests those stats. We implement them not at all, which is
  the same coverage with less code. Class: reference defect, dormant;
  recorded so nobody cites Python's RX/TX stat readback as a behaviour
  to match.

## Wrong claims in our own tree, verified or refuted

1. **"The RNode emits CMD_STAT_CHTM every ~2 s" — wrong, it is ~1 s.**
   `leviculum-std/src/interfaces/rnode.rs:847` (corrected in 35034019; the
   `TX_GATED_EVENT_AFTER` rationale) and `:873` (the `READY_POLL_MAX`
   rationale, "one CHTM stat cadence") both claim ~2 s. The firmware's
   idle cadence is `UTIL_UPDATE_INTERVAL_MS` = 1000
   (`reference/RNode_Firmware/Config.h:179`, folded in
   `check_modem_status`,
   `reference/RNode_Firmware/RNode_Firmware.ino:1445-1465`), plus one
   CHTM per queue drain. The same file's `CHTM_PERIOD`
   (`leviculum-std/src/interfaces/rnode.rs:1169`) documents 1000 ms
   correctly, so the file disagrees with itself. No functional
   consequence — both constants remain sensible at the true cadence —
   but the two rationales are the kind that get copied.
2. **"It even reserves a fixed post-TX yield (CSMA_POST_TX_YIELD_SLOTS)"
   — wrong, the reference never uses it.**
   `leviculum-nrf/src/lora.rs:876-877` (corrected in 35034019) claimed the reference firmware
   reserves a post-TX yield. `CSMA_POST_TX_YIELD_SLOTS` is defined at
   `reference/RNode_Firmware/Config.h:103` and referenced nowhere else
   in the firmware (checked by exhaustive grep over the mirror). The
   surrounding paragraph's substance stands — the reference does return
   to RX and gate the next TX behind DIFS+CW — but the named constant
   is decoration there, and our explicit window mirrors a discipline
   the reference does not actually implement.
3. **`CSMA_MAX_CW_MS` says "cw_max=15" and prices 15 slots — the
   longest band-1 draw is 14.** `leviculum-core/src/rnode.rs:1087-1092`
   derives 360 ms from 15 × 24 ms; `random(cw_min, cw_max)` is
   upper-exclusive, so even the boot-state window's widest draw is 14
   slots (336 ms), and the 24 ms slot itself only holds through
   SF8/125 kHz. Dead code in practice — its only consumer,
   `compute_spacing_ms` (`leviculum-core/src/rnode.rs:1263`), has no
   production caller (tests only), as
   `docs/src/concepts/csma-transmit-window.md` open item 3 already
   records. The same check found `JITTER_MAX_MS` equally caller-less.
   Corrected in 35034019: the comment states the true widths and why
   the value stays; `JITTER_MAX_MS` is deleted.
4. **Claims that held.** Spot-checked against the mirror and correct:
   the CMD_READY query protocol citations
   (`leviculum-std/src/interfaces/rnode.rs:859-869`), the
   flush-vs-pop decision and its 14 kbps threshold
   (`tx_hold` docs, `leviculum-std/src/interfaces/rnode.rs:998-1062`),
   the CHTM-follows-every-drain argument in `apply_radio_stat`
   (`leviculum-std/src/interfaces/rnode.rs:1968-2050`), the ESP32
   `init_channel_stats` reset
   (`docs/src/concepts/regulatory-airtime.md`, against
   `reference/RNode_Firmware/RNode_Firmware.ino:523`), the two-bin
   arithmetic citations in `AIRTIME_WINDOW_MS`
   (`leviculum-std/src/interfaces/rnode.rs:1143-1148`), and the
   channel-access crate's derivation table
   (`leviculum-nrf/channel-access/src/lib.rs:28-59`). The unsourced
   `cadDetPeak` ladder is already honestly declared where it lives
   (`cad_params`, `leviculum-core/src/sx126x.rs:387-455`, open on
   #351) — the audit adds nothing to it.

## The three I would fix first

1. **The announce-cap queue jump (D14).** Mechanism: the immediate-send
   branch at `leviculum-core/src/transport.rs:12471` (before 9b4515b1) tested only the
   holdoff; Python additionally requires the queue empty
   (`reference/Reticulum/RNS/Transport.py:1256-1257`). One added
   condition (`&& cap.queue.is_empty()`) restores the min-hops release
   order; the drain eleven lines below already does the right thing.
   Test: the core unit named in D14 — queue a 1-hop announce under an
   elapsed holdoff, broadcast a 3-hop one, assert order. Smallest
   possible change with a mesh-visible win under exactly the load the
   propagation Leitstern cares about.
2. **Stale cap-queue entries (D15).** Mechanism: no age check in
   `drain_announce_queues` (`leviculum-core/src/transport.rs:12610`);
   add Python's `QUEUED_ANNOUNCE_LIFE` prune at release (drop entries
   older than 24 h before selection, as
   `reference/Reticulum/RNS/Interfaces/Interface.py:330-337` does).
   Test: clock-advance unit asserting a 25-hour-old entry is dropped,
   not transmitted, and that a fresher same-destination replacement
   resets the age.
3. **The two self-disagreeing rationales (wrong claims 1 and 2).**
   Mechanism: comment-only edits — "~2 s" → "~1 s
   (`UTIL_UPDATE_INTERVAL_MS`)" at
   `leviculum-std/src/interfaces/rnode.rs:847` and `:873` (done in 35034019), and the
   post-TX-yield sentence at `leviculum-nrf/src/lora.rs:876-877` (likewise)
   rewritten to what the reference does (returns to continuous RX;
   the yield constant is unused). Test: the citation guard anchors the
   corrected lines; no behaviour moves. Cheap, and it is exactly the
   kind of copied-forward rationale this audit exists to catch before
   the next reader prices a poll on it.

## Coverage

Compared in full: the host RNode interface's transmit path, channel
access, hold pricing, duty-hold inference and airtime settlement
(`leviculum-std/src/interfaces/rnode.rs`, non-test half); the shared
model (`leviculum-core/src/rnode.rs`: bitrate, slot, preamble, airtime,
split framing, `AirtimeTracker`, announce-cap bitrate); the policy
crates `leviculum-channel-access`, `leviculum-queue-budget`,
`leviculum-tx-spacing`, `leviculum-mute-lease` and the module contract
of `leviculum-rx-arming`; the LNode transmit loop
(`leviculum-nrf/src/lora.rs:1686-2291`); the core announce cap
(`leviculum-core/src/transport.rs`, the cap/drain/hold paths); the CAD
and window arithmetic in `leviculum-core/src/sx126x.rs`. Reference
side: `RNodeInterface.py` (serial fully, Android spot-checked for the
TX model — same immediate-write shape), `Interface.py` and
`Transport.py` announce-cap paths, `Reticulum.py` constants, and the
firmware mirror's `RNode_Firmware.ino`, `Config.h`, `Utilities.h`,
`sx126x.cpp` (CSMA, airtime, queue, stats; `sx127x.cpp` only for
`dcd`).

Not compared, and why: the rx-arming crate's full 4 k lines beyond its
contract (the reference has no counterpart to diff against — D16 states
the architectural divergence and its instrument); the SX127x/SX128x
driver families (no board of ours runs them); the BLE/TCP transports of
`RNodeInterface.py` (carrier plumbing, not air behaviour); the
multi-vport RNode path beyond confirming it runs the same gates as the
single-radio task (`rnode_multi_io_task` mirrors the same `DutyHolds`,
ready-poll and hold code); `periculum`'s emulator model (a fourth
implementation, out of scope by the order's terms); and the three open
P1s' full ledgers — the issue texts were taken as characterised in the
order (431: single frames lost between quiet radios, receiving side;
438: two senders keying inside one preamble; 433: stale frames behind
the duty lock) plus the KNOWN-REDS entries, not re-derived. One stale
note outside the repo surfaced in passing: the rig RNodes' RSSI
carrier-sense arm has been OFF since 2026-09-26, which supersedes the
September note that it was running.
