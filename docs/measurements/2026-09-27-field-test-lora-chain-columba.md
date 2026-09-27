# Field test 2026-09-27: a Columba phone on a three-hop LoRa chain

A phone running Columba, carried on foot for five hours, talked to a base
station through a battery-powered LNode relay over LoRa. Chat worked in both
directions, positions were collected, and two defects fell out of it. This
document records what the logs measured, with the command for every figure in
the appendix.

Refs: Codeberg #255 (field test), #330 (proof over the short lossy way),
#412 (telemetry collector), #433 (link requests aged behind a duty hold),
#350 (hash-order release, fixed).

## 0. Setup, window, and what was captured

Analysis window: **11:20 to 16:15 CEST** (09:20 to 14:15 UTC), 17,700 s,
4 h 55 min. All times below are CEST unless a line is quoted verbatim from a
log, where they are UTC.

The chain, from the phone inward:

| role | what | identity prefix |
|---|---|---|
| phone | Columba on Lew's Fairphone | `0a68951f` |
| carried relay | feld-pocket (LNode Pocket V2), BLE to the phone, LoRa to the base | transport `1d48253f` |
| base radio | feld-t114 (LNode T114) on USB at the base | transport `b2a8bea1` |
| base daemon | `lnsd -c /home/lew/feld`, musl, push-tree `84e099ab4` | `events.log`, `lnsd.log` |
| chat helper | `lxmf-node feld-basis` | `8f35c8d5` |
| collector | `lxmf-node feld-sammler` (#412) | `374729ed` |

A third board, the SolarNode, was carried as well. All three boards had
`--set-telemetry` to `0a68951f` with `--telemetry-profile station`.

Daemon uptime anchor: `lnsd` started 2026-09-26T21:55:24Z, so the `t=` field
in `events.log` and in the `lxmf-node` EVENT lines is milliseconds since that
instant. Calibration check: the last `PKT_TX` carries `t=60422675`, which maps
to 16:42:26.675, and the collector's last `LINK_REQUEST_RX` is stamped
16:42:26. The two agree to under a second.

LoRa PHY for every board: 869.463 MHz, BW 125 kHz, SF8, CR 4/5, programmed
preamble 18 symbols (`derive_preamble_symbols`,
`leviculum-core/src/rnode.rs:918-935`). Airtime figures below are **charged by
the firmware's own cost functions**, not by a transcription of them: a keyed
frame by `frame_airtime_cost_ms` (`rnode.rs:1687`), the same call
`add_airtime` is fed after a successful `transmit()`
(`leviculum-nrf/src/lora.rs:1015-1028`), and a demodulated packet by
`packet_airtime_ms` (`rnode.rs:1656`), which adds back the per-frame header
byte and, above `MAX_SINGLE_PAYLOAD` (`rnode.rs:1980`), the second frame's
whole preamble. The program that does it is in the tree —
`cargo run -p leviculum-std --example lora_airtime_census` — so every
millisecond in sections 1 and 5 can be re-derived from the capture by running
it, and cannot drift from what a board would have counted.

The first version of this document did transcribe the formula, lost a term in
it, and priced every frame about three times too cheap; see the changelog at
the end. The "133.6 ms for a 135 B frame" that version called a wire
validation was that same broken arithmetic's own output, quoted back at
itself — the value at SF8/BW125/CR4:5 with an 18-symbol preamble is 421 ms.

**What was not captured.** The Pocket's debug capture
(`/home/lew/rig-run/ble-drop/feld-pocket.log`) stops at 11:21:55, four seconds
into the window ("READER2 detached"), and resumes at 16:18:12, three minutes
after it closes. For the whole test the relay was off the wire. Its first line
back on the wire is already

```
2026-09-27T14:18:12.771+00:00 [LORA_AIRTIME_LOCK] st=339 lt=360077 holding
```

that is, the board arrived home with its rolling-hour airtime ledger pinned at
the 10 % cap (360,077 ms of 3,600,000 ms) and frames held in its queue. Pass
358 then measured it there directly. **The field logs therefore cannot date
the onset of the duty lock; they can only show its shape from the base side.**
That is why #433's mechanism evidence is post-test, and why section 1 below
argues from the base and from the T114's air log.

**Firmware on the base radio.** `[FW_BUILD] git_sha=ccac3a32 dirty=false`,
built 2026-09-12. The per-carrier byte counters landed in `ab319b6f` on
2026-09-27 00:32, and `ab319b6f` is not an ancestor of `ccac3a32`, so **the
field capture has no per-carrier byte counters**. Per-medium counts in section
5 are reconstructed by counting frames in the capture instead.

## 1. Half the phone's links never completed, and the cause is #433

**Headline: link establishment on the three-hop chain went from 95.0 % to
4.9 % at one instant, 14:07:11.**

### 1.1 The mechanism, quoted from pass 358, not re-derived

Pass 358 measured this after the test with the phone at home and the Pocket
back on the wire (commits `453ba908`, `c06112af`; Codeberg **#433**):

> At the cap, the Pocket's duty lock does not thin traffic, it ages it: the
> queue drains one frame per budget dip (~10 to 15 s), so under sustained load
> every frame reaching the air is minutes old. The three link requests
> measured after the field test were held 145.6 s, 137.7 s and 144.0 s, but a
> relayed link request is only routable for its proof deadline of
> (hops + path_hops + 2) x 6 s, about 30 s, and the phone itself re-sends
> every 16 s and gives up. The base answered every request instantly; every
> proof came back to a relay entry that had expired about 115 s earlier and
> was discarded, until this batch silently. That is the mechanism behind the
> field day's unanswered link requests, a scheduling property of a saturated
> 10 % duty hop, not a lossy air.

Citations for that chain: deadline at `transport.rs:6504-6507` with the 6 s
per-hop constant at `constants.rs:113`; reaping at
`memory_storage.rs:1179-1183`; the silent drop, now counted as
`lrproof_no_link`, at `link_management.rs:1081-1099`.

### 1.2 Field-phase evidence, base side only

**Event totals in the window.** The two daemons proofed *every* request they
received, immediately:

| | `LINK_REQUEST_RX` | `LINK_PROOF_TX` | `LINK_ESTAB` | `LINK_DIED` handshake |
|---|---|---|---|---|
| collector `374729ed` | 183 | 183 | 20 | 159 |
| chat helper `8f35c8d5` | 208 | 208 | 57 | 194 |

Requests received equals proofs sent, exactly, on both sides. Nothing was
refused, rate-limited or queued at the base. **The forward leg is not the
problem and the base is not the problem.**

**The forward leg loses nothing measurable.** The T114's air log holds 391
link-request frames in the window: 201 addressed to `8f35c8d5` and 180 to
`374729ed`, that is 381 for our two destinations, plus 10 for three
destinations belonging to neighbours (`2bcd55a7`, `d1eadaf4`, `4b3411ce`). The
daemons logged 385 requests arriving at three hops, 205 and 180. The collector
side matches the air exactly, 180 against 180; the chat helper side has 4 more
than the air shows, which the capture cannot place (a `LINK_REQUEST_RX` line
names the daemon's own `iface=LocalClient[feld]`, not the medium the frame came
in on, and the T114 also bridges BLE). Either way the air delivered no fewer
requests than the daemons answered: **there is no forward-leg loss to explain
the 85.7 % failure rate below.**

**The return leg, split by path length.** A request arriving with `hops=2`
came over the phone's direct BLE link to the T114 and never touched the
Pocket; `hops=3` went through the Pocket:

| path | requests | established | rate |
|---|---|---|---|
| `hops=2` (phone to T114 over BLE) | 6 | 6 | **100 %** |
| `hops=3` (phone to Pocket to air to T114) | 385 | 55 | **14.3 %** |

The cleanest instance is three requests inside 83 s, same phone, same
destination, same minute:

```
13:09:25Z LINK_REQUEST_RX link=7d40638c dest=374729ed hops=2   -> LINK_ESTAB 13:09:27 (rtt 3223 ms)
13:09:46Z LINK_REQUEST_RX link=099fb7d4 dest=374729ed hops=2   -> LINK_ESTAB 13:09:55 (rtt 8610 ms)
13:10:48Z LINK_REQUEST_RX link=0c8d5eb5 dest=374729ed hops=3   -> LINK_DIED  13:12:14 handshake_timeout
```

Take the Pocket out of the path and the handshake completes in seconds; put it
back in and it dies. Nothing else differs.

**The step change.** Split the window at the first of the sustained failures:

| | all paths | `hops=3` only |
|---|---|---|
| 11:20:00 to 14:07:10 | 40 / 42 = **95.2 %** | 38 / 40 = **95.0 %** |
| 14:07:11 to 16:15:00 | 21 / 349 = **6.0 %** | 17 / 345 = **4.9 %** |

The last three-hop request to establish did so at 14:07:10. Per 30 minutes:

```
11:30   4/4  100.0%      14:00   14/96  14.6%
12:00  10/10 100.0%      14:30    6/70   8.6%
12:30   1/1  100.0%      15:00   11/54  20.4%
13:00   3/3  100.0%      15:30    0/94   0.0%
13:30   8/8  100.0%      16:00    4/51   7.8%
```

A rolling-window budget filling up produces exactly this: no gradual decay,
one knee, and then a self-sustaining regime in which the request volume
*rises* (from about 10 per half hour to about 90) because every failure is
retried, which loads the relay further. The two halves are the same hardware,
the same PHY, the same distance band and the same destination.

**The deaths were on time, not early.** Pairing each `LINK_REQUEST_RX` with
its `LINK_DIED` by link id gives the true age at death: collector n=159,
72.3 s to 89.9 s, median 81.3 s; chat helper n=164, 72.1 s to 89.9 s, median
81.0 s. The `elapsed_since_activity_ms` field printed on those lines is
useless in this build (it reports process uptime, for example 9,581,430 ms on
a link 80 s old); that was fixed in pass 354.

**The base's own outbound direction fails the same way.** `LINK_REQUEST_RETX`
is emitted by the *initiator* re-keying its own request
(`link_management.rs:3402`), so the 100 retransmissions in the chat helper's
log are the **base's**, not the phone's: 47 `LINK_REQUEST_TX` plus 100
`LINK_REQUEST_RETX`, all to `0a68951f`, 16 established as initiator, 30 dead
at handshake timeout. Direction is irrelevant to the mechanism, because the
frame that has to survive the Pocket's queue is the *return* leg either way:
outbound it is the phone's proof coming back through the Pocket.

**The air was busy, and the base was not the one being held back.** The T114
in the window: 1,800 TX frames (233,354 B, **728.7 s** of airtime, **4.12 %**
duty) and 1,781 RX packets (280,113 B, **893.8 s**, **5.05 %**), a combined
channel occupancy of **1,622.6 s, 9.17 % of the window**. Counted the other
way — every frame the modem demodulated, whether or not it reassembled into a
packet the host saw (`[T114_SX_RX]`) — the receive side is 2,531 frames and
960.0 s, 5.42 %. Zero `holding` lines, zero `CARRIER_DROP`, zero
`CARRIER_GATE` in the whole window; positive control on those three greps, the
same capture holds 146 `CARRIER_DROP` and 1,040 `CARRIER_GATE` lines on other
days, so the patterns are right and the zeros are real. So the base never hit
its own lock — but it was not comfortable either: over its busiest rolling
hour it keyed **258.1 s from 12:07:04Z, 72 % of a 360 s budget**. The
constraint that broke the test still sat on the battery-powered relay, which
did reach its cap; the base had margin, not idleness.

**Closed by 364: the ledger is right and the T114's ear cannot bound it.**
The airtime ledger is charged TX-only, from length and PHY, after a successful
`transmit()` (`leviculum-nrf/src/lora.rs:1015-1028`) — held, stale-dropped,
muted and CSMA-retried frames pay nothing. The earlier reading of this
document set the Pocket's 360,077 ms trailing-hour ledger against "83.5 s
demodulated at the T114 in the busiest hour" and called the factor 4.3 an open
question. Both halves of that were wrong. The 83.5 s was the mispriced
arithmetic (the busiest UTC hour is 277.8 s of RX, and the busiest rolling
hour 282.8 s from 12:01:15Z), and the comparison itself is not one a
half-duplex radio admits: a receiver is deaf for every millisecond it is
keying, and the T114 keyed 256.6 s of the very hour it is being asked to
account for, so **its demodulated total can never bound a neighbour's keyed
total**. Pass 364 verified the ledger directly instead, on the Pocket's own
post-test reboot where the bins start empty and every frame is captured:
reported `lt` against the rolling-hour sum recomputed from the logged frame
lengths is **ratio 1.00 across 3.3 hours, twice exact to the millisecond**.
Priced correctly, the T114's own trailing hour at the moment `lt=360077` was
logged (13:18 to 14:18Z) is **249.4 s demodulated, 256.6 s keyed**, against
the Pocket's 360 s. The 110.6 s that separates the two has two admissible
sources the field logs cannot tell apart — the 256.6 s of that hour in which
the T114 was keying and therefore deaf, and Pocket frames it simply did not
decode — and neither is evidence against a ledger 364 checked directly.
There is no open number here; what the lock was rationing is a link-data
retransmission loop, which 364 measured and #433 carries.

### 1.3 `PATH_REBALANCE`, reported as its own observation

The #330 reading is withdrawn: nothing here shows a proof taking a short lossy
way. What the base did log is four rebalances on the phone's path, all with
`next_hop=b2a8bea1` (the T114) throughout: 2 to 3 hops at 12:04:15, 3 to 2 at
14:43:30, 2 to 3 at 15:05:30, 3 to 2 at 16:16:22. The hop count tracks whether
the phone was hanging on the T114's BLE or coming through the Pocket. The
14:43:30 flip is 99 ms before the stuck message batch of section 2 was
released, which is the causal order one expects: path improves, link
establishes, `wake_direct_outbound` frees the queue.

## 2. Messages that waited left in hash order (#350, fixed in `40faf9c4`)

**Headline: the 14:14 status arrived 29 min 51 s late, and second of three.**

Reproduced from `lxmf.log` by pairing each `MessageQueued` id with its
`MessageState` transitions (the `MessageState` lines carry no timestamp of
their own, so each takes the nearest preceding traced timestamp):

```
queued 12:14:01.132994 d6b8db2a   (body "Basis 14:14")
queued 12:24:00.557426 07b41449   (body "Basis 14:24")
queued 12:33:53.883161 7b0d0a50   (body "Basis 14:34")

12:37:40.289458 07b41449 Sending     <- written 14:24
12:37:40.289458 7b0d0a50 Sending     <- written 14:34
12:37:40.289458 d6b8db2a Sending     <- written 14:14, oldest, released last
12:43:30.541352 07b41449 Sending
12:43:30.541352 7b0d0a50 Sending
12:43:30.541352 d6b8db2a Sending
12:43:31.994174 7b0d0a50 Delivered
12:43:51.042355 07b41449 Sending
12:43:51.042355 d6b8db2a Sending
12:43:52.494552 d6b8db2a Delivered
12:44:03.790474 07b41449 Sending
12:44:03.790474 70b59bdc Sending
12:44:04.498360 07b41449 Delivered
12:44:04.747432 70b59bdc Delivered
```

The release order is identical in every tick and identical to ascending
message id, which is `SHA-256(destination || source || payload)`
(`leviculum-lxmf/src/message.rs:127-132`): the outbound map was keyed by it
(`router.rs`, `BTreeMap<[u8;32], OutboundEntry>`), so a waiting batch left in
content-hash order. Write order `d6b8db2a`, `07b41449`, `7b0d0a50`; release
order `07b41449`, `7b0d0a50`, `d6b8db2a`, three ticks in a row, to the
microsecond.

**Columba's receive-time timeline**, which is what Lew actually saw:

| arrival | body | written | age |
|---|---|---|---|
| 14:43:31.994 | Basis **14:34** | 14:33:53 | 9 min 38 s |
| 14:43:52.494 | Basis **14:14** | 14:14:01 | 29 min 51 s |
| 14:44:04.498 | Basis **14:24** | 14:24:00 | 20 min 4 s |
| 14:44:04.747 | Basis 14:44 | 14:44:00 | 5 s |

Newest first, then oldest, then middle. His report at 14:46:48 ("mir ist
aufgefallen, dass die teilweise nicht in der richtigen Reihenfolge ausgeliefert
worden ... manchmal ältere Nachrichten vor den neueren") is an exact rendering
of that table. Fixed in `40faf9c4`: the due list is now sorted by LXMF
timestamp with the message id as tiebreak.

## 3. Out of range: the path never went away

**Headline: `no_path=0` at the base for the entire run.**

The base's final `PKT_DROP_SUMMARY`, after 16 h 47 min of uptime, is

```
announce_rate_limited=14 announce_replay=207 duplicate=6 overheard_transport_id=12 total=239
```

with `no_path=0` and `lrproof_invalid=0`, and no `lrproof_no_link` field at
all, since the field build predates `453ba908`. So the earlier reading that
"the base had no path" does not hold: **not one packet was ever dropped at the
base for want of a path.** Announces from the phone kept arriving through the
Pocket during the gap (`PATH_ADD ... reason=newer_emission source=announce` at
14:30:48, 14:34:45 and 14:41:10), so the Pocket to T114 leg stayed alive
throughout. What was dead was link completion, section 1, not reachability.

The `nopath=` figure in the 10-minute status lines is a different counter
belonging to a different node: `status-lxmf.sh` greps the **T114's** last
`[TRANSPORT]` line out of the debug capture, so `fwd= rx= tx= nopath= dup=
overheard= maxhops= paths=` are all the board's, cumulative since its boot,
and medium-agnostic. Its `nopath` moved four times all day: 0 to 1 at 12:04
(the same minute the path went from 2 to 3 hops), 1 to 2 at 15:04, 2 to 3 at
15:14, 3 to 7 at 16:14. Nothing during the out-of-range stretch.

Timeline of the two out-of-range stretches, from the collector rows, the
outbound establishments, and Lew's own LXMF:

| time | what |
|---|---|
| 14:07:10 | last three-hop link to establish before the collapse |
| 14:08:12 | last position the phone *produced* before the first gap |
| 14:12:10 | last position *received* before the first gap |
| 14:14 / 14:24 / 14:34 | three status messages queued and stuck |
| 14:40:32 | phone produces positions again |
| 14:43:30 | path 3 to 2 hops, batch released, 14:34 message delivered |
| 14:45:10 | positions received again, 33.0 min receive gap closed |
| 14:46:12, 14:46:48 | Lew's two LXMF replies arrive ("ich war aus BLE-Reichweite des Pockets ... den dann aber zeitweilig abgestellt") |
| 15:16:04 | last position received before the second gap |
| 15:47:16 | last position produced in the second gap |
| 16:02:13 | positions received again, 46.1 min receive gap closed |

The two stretches differ in kind, and the difference is worth keeping. In the
first, message timestamps show a 32.3 min hole (14:08:12 to 14:40:32): those
samples never arrived at all, they were lost at the phone, not delayed. In the
second, the phone kept stamping positions through 15:47 and they were delivered
from 16:02 onwards, up to 51.6 min old. So Columba does queue and does
forward a backlog; the first hole is a separate question about what it does
when it has no usable interface at all.

## 4. Positions

**Headline: 187 rows, 163 distinct samples at a 30 s cadence, median 202 s old
on arrival, oldest 51.6 min.**

`collector/storage/telemetry.jsonl` holds **187** rows, every one from
`0a68951f`, first received 13:57:55, last 16:35:25. The GPX built at 16:15
holds **173** `<trkpt>`; that is the same data cut 20 minutes earlier, which is
where the figure 173 comes from. Collapsing rows less than 2 s apart (32 of
them are near-simultaneous pairs) leaves **163 distinct samples**, and their
inter-sample interval has a single clear mode at **30 s** (42 at 30 s, 12 at
29 s, 9 at 31 s), not 16 s. Track extent: latitude 53.105518 to 53.113193,
longitude 9.146830 to 9.159470, roughly 850 m by 850 m. `speed` is 0.00 in
every row; Columba does not fill it.

Received per 10 minutes, against produced per 10 minutes, is the
store-and-forward measure:

```
bucket   received   produced
13:50        7          7
14:00       46         52
14:10        6          -
14:40       11         22
14:50       14         26
15:00       50         27
15:10       15         15
15:20        -          4
15:30        -         16
15:40        -         14
15:50        -          1
16:00       24          1
16:10        -          1
16:20        -          1
16:30       14          -
```

The 15:00 bucket holds 50 received against 27 produced, and inside it
**26 rows land in the single minute 15:09**, matching the earlier reading
exactly. That minute is the phone's queue draining, and it drained over the
two-hop BLE path: the two link requests that carried it arrived at 13:09:25Z
and 13:09:46Z with `hops=2` and established in 1.6 s and 8.7 s (section 1.2),
while the `hops=3` request 62 s later died. The second big catch-up, 24 rows
in the 16:00 bucket, closes the 46.1 min gap.

Arrival age (`received_at` minus `message_timestamp`) over all 187 rows:
minimum -1.5 s (the phone's clock runs marginally ahead), median 201.9 s, mean
564.8 s, p90 1902.5 s, maximum 3098.1 s. **120 of 187 rows arrived more than a
minute after they were stamped, 76 of them more than five minutes after.**
That is what a store-and-forward chain through a saturated relay costs a
16 s-to-30 s telemetry feed.

## 5. Airtime: why so much goes over LoRa

Lew asked over LXMF at 12:00:47: "Warum gehen so viele Pakete über LoRa durch
die Luft? Hier ist mehr Traffic als ich erwartet habe. Ist alles sane?"

**Headline: the air was 9.17 % occupied, the base keyed 72 % of its own hourly
budget in its busiest hour, and 14.7 % of all that occupancy bought nothing at
all.**

Channel occupancy at the T114 over the 17,700 s window, by packet type (the
type is bits 0-1 of the flags byte, `leviculum-core/src/packet.rs:15,37-46`).
TX rows are keyed frames priced with `frame_airtime_cost_ms`; RX rows are
reassembled packets priced with `packet_airtime_ms`, which charges a header
byte per frame and a second preamble above 254 B:

| direction | type | frames | airtime | share of that direction |
|---|---|---|---|---|
| TX | DATA | 340 | 109.4 s | 15.0 % |
| TX | ANNOUNCE | 409 | 233.4 s | 32.0 % |
| TX | LINKREQUEST | 145 | 48.0 s | 6.6 % |
| TX | PROOF | 906 | 337.9 s | 46.4 % |
| RX | DATA | 1140 | 629.0 s | 70.4 % |
| RX | ANNOUNCE | 155 | 96.8 s | 10.8 % |
| RX | LINKREQUEST | 391 | 132.1 s | 14.8 % |
| RX | PROOF | 95 | 35.9 s | 4.0 % |

TX 728.7 s (4.12 % duty), RX 893.8 s (5.05 %), together **1,622.6 s, 9.17 % of
the window**. Regrouped by purpose:

| purpose | airtime | share of occupancy |
|---|---|---|
| link handshakes (requests both ways, link proofs, their returns) | 361.2 s | 22.3 % |
| announces | 330.2 s | 20.3 % |
| payload data | 738.4 s | 45.5 % |
| per-packet receipt proofs | 190.2 s | 11.7 % |
| ten outbound proof frames of other lengths (9 × 54 B, 1 × 230 B) | 2.5 s | 0.2 % |

So the answer to Lew's question is: the volume is not absurd, but the air is
**91 % idle, not 97 %**, and in the busiest rolling hour the base alone keyed
258.1 s against the 360 s a 10 % duty cycle allows it. A fifth of everything
that *is* on the air is handshake overhead, and most of that is waste.
Splitting the 906 outbound proof frames by length and by whose link id they
are addressed to separates the two cleanly:

* **382 frames of 119 B**, one per distinct link id, 382 distinct ids. Packet
  length 118 = 1 flags + 1 hops + 16 destination + 1 context + 99
  `LINK_PROOF_SIZE` (`leviculum-core/src/link/mod.rs:68`). These are the link
  proofs. Only **51 of those 382 links ever established**, so **331 link
  proofs, 86.6 %, were keyed onto the air for handshakes that never came up.**
* **514 frames of 116 B**, addressed to 52 ids, and 511 of them to 49 ids that
  *did* establish, up to 19 for a single link. These are per-packet receipt
  proofs on working links: productive traffic.

Costing the dead handshakes at SF8/BW125/CR5 with an 18-symbol preamble: 331
requests at **339 ms** (a 102 B packet, 103 B on the air — 381 of the 391
inbound requests are exactly that length) plus 331 link proofs at **380 ms**
is **238.0 s, 14.7 % of all channel occupancy in the window**, and it
delivered nothing. That is the airtime price of #433 as the field measured it,
and it is a lower bound, because it counts only the frames the T114 saw.

What the rest is:

* **Payload.** The inbound 259 B packets, 562 of them, are 455.8 s, 72.5 % of
  all RX DATA airtime and 28.1 % of total occupancy. Each one is over the
  254 B single-frame limit, so it is two frames and two preambles on the air:
  728 ms for the 255 B first frame and 83 ms for the 6 B remainder. That is
  the phone's LXMF and telemetry: the largest single honest consumer, as it
  should be.
* **Announces.** 20.3 % of occupancy. The base received **131 announces from
  the phone alone** in the window, one every 135 s, next to 28 and 26 from two
  board destinations and 3 to 10 each from six more. Columba's announce
  cadence, not ours, is the driver here, and it is worth a look next to the
  cadence question in `project_announce_cadence_and_use_cases`.
* **Per-medium split.** The T114's `rx=` counter rose by 4,472 over the window
  while its LoRa RX was 1,781 packets (2,531 demodulated frames) and its BLE
  RX 914, so `rx=` counts every
  medium plus the host serial side. With no per-carrier byte counters on this
  firmware (section 0) a byte-exact split is not available from this capture;
  `ab319b6f` on the boards will give it directly next time.

Per UTC hour, so the ramp is visible:

```
09:00  TX   11 /   6.2 s (0.17 %)   RX   11 /   6.1 s (0.17 %)
10:00  TX  214 /  82.6 s (2.30 %)   RX  198 / 125.6 s (3.49 %)
11:00  TX  222 /  75.9 s (2.11 %)   RX  211 / 128.9 s (3.58 %)
12:00  TX  562 / 236.7 s (6.57 %)   RX  609 / 277.8 s (7.72 %)
13:00  TX  529 / 219.4 s (6.09 %)   RX  511 / 233.1 s (6.47 %)
14:00  TX  262 / 108.0 s (3.00 %)   RX  241 / 122.4 s (3.40 %)
```

(The 09:00 and 14:00 buckets are partial: the window opens at 09:20 and closes
at 14:15.) The busiest clock hour on the air is 12:00 UTC with 277.8 s of RX
and 236.7 s of TX; the busiest *rolling* hour is 282.8 s of RX from 12:01:15Z,
258.1 s of TX from 12:07:04Z, and 534.7 s of the two together from 12:01:37Z,
14.9 % of the channel — with the base's own keying at 72 % of what its duty
cycle permits. The knee in section 1 sits inside it.

## 6. What this means for the Leitstern

The Leitstern is: a board is a full mesh node, speaking Columba's Bluetooth
protocol completely *and* carrying other people's traffic. This test is the
first end-to-end evidence that the first half holds outside the lab. A phone
in motion, on a real walk, held a two-way LXMF conversation with a base
through a battery-powered LoRa relay for five hours over a track 850 m across;
seven chat messages in, 36 status messages out, 163 positions collected
through a store-and-forward chain, and both out-of-range stretches recovered
on their own without anybody touching anything. That is the user-visible
increment: the chain works, unattended, on real ground, with a real phone.

Two defects fell out of it, and exactly one is still open.

**Closed: hash-order release (#350, `40faf9c4`).** A waiting batch left in
content-hash order, so a conversation read out of sequence after every
reconnection. Found by Lew reading his own timeline, confirmed at microsecond
resolution, fixed the same day, with a minimal test whose positive control is
the old ordering.

**Open: link requests aged behind a duty hold (#433).** This is the one that
decides whether the *second* half of the Leitstern is real. A propagation node
that carries other people's traffic will spend its duty budget, and the moment
it does, this test says it stops being a relay and becomes a queue that ages
frames past every deadline in the protocol: 95.0 % to 4.9 % establishment at a
single instant, with the retry storm that follows loading the relay further.
About half the phone's positions survived the stretch after the knee: 124
distinct samples arrived carrying a stamp between 14:07:11 and 16:15, against
the roughly 256 a 30 s cadence produces in 7,669 s, so 49 %. A board at its
cap must degrade by dropping what is already dead, not by
delivering fossils; pass 358 recommends the interface-level age cap for
exactly that reason, and the `lrproof_no_link` counter that landed with it is
how we will see the fix work.

Neither defect is a wire-format or semantics problem. Both are scheduling
inside our own stack, which is where they should be.

## 7. Where the earlier reading did not reproduce

The reviewer's pre-reading was right about the shape everywhere and wrong
about five specifics. Recorded because each one was a plausible mistake.

1. **Event totals were a mid-test snapshot, not the test.** The figures
   109/109/17/91 and 130/130/52/92 are exact, for a read taken between
   13:10:48Z and 13:13:17Z, an hour before the test ended: the 109th collector
   request and the 130th chat-helper request are the *same* packet at
   13:10:48Z. Over the full window they are 183/183/20/159 and
   208/208/57/194; over the whole day 240 and 268. The grep was right, the
   clock was early.
2. **`LINK_REQUEST_RETX` is ours, not the phone's.** The event is emitted by
   the initiator re-keying its own outbound request
   (`link_management.rs:3402`), and all 100 in the window carry
   `dest=0a68951f`. They are the base helper retransmitting to the phone. The
   phone's own retries are invisible to us except as fresh link ids arriving.
3. **The base never lacked a path.** `no_path=0` in the base's drop summary
   for the whole run, and the phone's announces kept arriving through the
   Pocket during the gap. The `nopath=` in the status lines is the **T114's**
   counter, read out of the board's `[TRANSPORT]` line by `status-lxmf.sh`;
   so are `fwd=`, `rx=`, `tx=`, `dup=`, `overheard=`, `maxhops=` and `paths=`.
   None of them is the base daemon's.
4. **173 was the GPX, not the rows.** `telemetry.jsonl` has 187 rows; the GPX
   built at 16:15 has 173 `<trkpt>`. And the tracking cadence is 30 s, not
   16 s: mode 30 s with 42 of 163 intervals there, against one interval at
   16 s.
5. **The backlog flushed before Lew's message, not after it.** Release and
   delivery are at 14:43:30 to 14:44:04; his two LXMF replies arrive at
   14:46:12 and 14:46:48, which is him reporting what he had just seen. The
   "14:58" in the pre-reading is not in the logs.

Two pre-readings did reproduce exactly and are worth saying so: the true
handshake ages (72 s to 90 s, median 81 s, from pairing request to death by
link id, the printed `elapsed` being uptime) and the burst of 26 collector
rows in the minute 15:09.

## Appendix: commands

Window slices used throughout (UTC 09:20:00 to 14:15:00):

```sh
cd /home/lew/feld
awk '{ if (match($0,/2026-09-27T[0-9:.]+Z/)) { t=substr($0,RSTART+11,8);
       if (t>="09:20:00" && t<"14:15:00") print } }' lxmf.log           > base-win.txt
awk '{ if (match($0,/2026-09-27T[0-9:.]+Z/)) { t=substr($0,RSTART+11,8);
       if (t>="09:20:00" && t<"14:15:00") print } }' collector/lxmf.log > coll-win.txt
awk 'substr($0,1,10)=="2026-09-27" { t=substr($0,12,8);
       if (t>="09:20:00" && t<"14:15:00") print }' \
     /home/lew/rig-run/ble-drop/feld-t114.log                           > t114-win.log
```

**Section 0.** Uptime anchor: `head -1 /home/lew/feld/lnsd.log`;
`tail -1 /home/lew/feld/events.log`. Pocket capture gap:
`awk 'substr($0,1,10)=="2026-09-27" {print substr($0,12,2)}' feld-pocket.log | uniq -c`,
then `tail -2` of the `09` hour and `head -2` of the `14` hour. Firmware:
`grep -a -m1 FW_BUILD feld-t114.log` and
`git merge-base --is-ancestor ab319b6f ccac3a32` (exit 1).

**Section 1.** Event totals: `grep -c <EVENT> base-win.txt coll-win.txt`,
with `grep -c 'LINK_DIED.*handshake_timeout'` for the last column. Hop
distribution and destinations:
`grep LINK_REQUEST_RX <win> | grep -o 'hops=[0-9]*' | sort | uniq -c` and the
same for `dest=`. Air-side requests:
`grep -a '\[LORA\] RX' t114-win.log | grep -aoE 'flags=0x[0-9a-f]2 dst=[0-9a-f]{8}' | awk '{print $2}' | sort | uniq -c`.
Establishment by hops, by 30-minute bucket, and split at the knee: one awk or
python pass that keys `LINK_REQUEST_RX link=` and `hops=` into a map and marks
the ids that later appear on `LINK_ESTAB ... initiator=false`. True ages: the
same pairing, printing the timestamp difference for ids that reach
`LINK_DIED ... handshake_timeout` without an `LINK_ESTAB`. Outbound:
`grep -c LINK_REQUEST_TX base-win.txt`, `grep -c LINK_REQUEST_RETX
base-win.txt`, `grep LINK_REQUEST_RETX base-win.txt | grep -o 'dest=[0-9a-f]*'
| sort | uniq -c`. Snapshot boundary:
`grep LINK_REQUEST_RX collector/lxmf.log | sed -n 109p`. Rebalances:
`grep -a PATH_REBALANCE /home/lew/feld/events.log`, `t=` converted with the
anchor above.

**Section 2.** `grep -a MessageQueued lxmf.log` and `grep -a MessageState
lxmf.log`, each byte array joined to hex, each line stamped with the nearest
preceding `2026-09-27T...Z`.

**Section 3.** `grep -a PKT_DROP_SUMMARY events.log | tail -1 | tr ' ' '\n' |
grep -v '=0$'`; `grep -a PATH_ADD events.log | grep 0a68951f`;
`grep -o 'Basis [0-9:]*: fwd=[0-9]* rx=[0-9]* tx=[0-9]* nopath=[0-9]* dup=[0-9]*' status-sent.log`
differenced pairwise; `grep -a '\[TRANSPORT\]' t114-win.log | head -1` for the
counter's true owner; `cat status-lxmf.sh` for where it is read from.
Lew's replies: `lxmf_msg_received src=0a68951f ... body_b64=... t=` decoded
with base64 and the uptime anchor.

**Section 4.** `wc -l collector/storage/telemetry.jsonl`;
`grep -c '<trkpt' collector/feldtest-2026-09-27.gpx`; buckets, gaps, interval
histogram and the `received_at` minus `message_timestamp` distribution from a
python pass over the jsonl with `received_at`/`message_timestamp` in
UTC+02:00.

**Sections 1 and 5, every airtime figure.** One command, and it is in the
tree so it can be re-run:

```sh
cargo run -p leviculum-std --example lora_airtime_census -- \
    /home/lew/rig-run/ble-drop/feld-t114.log 2026-09-27 09:20:00 14:15:00
```

It reads `[T114_TX_FRAME] first8=<16 hex> len=<n>` (a keyed frame, `len` is
the full on-air length), `[LORA] RX <n> bytes ... flags=0x<hh>` (a
REASSEMBLED packet, so `n` is short of the on-air length by a header byte and,
above 254 B, by a second frame's whole preamble) and `[T114_SX_RX] len=<n>`
(every frame the modem demodulated, reassembled or not), takes the packet type
from the low two bits of the flags byte, and charges each frame with
`frame_airtime_cost_ms` and each packet with `packet_airtime_ms` at
125000/8/5/preamble 18. It prints the by-type table, the by-length table, the
per-hour block and the busiest rolling hour. The trailing-hour figures in
section 1.2 are the same command with `13:18:00 14:18:00`; a different PHY is
`--sf/--bw/--cr/--preamble`.

The two numbers the census does not produce: proof-frame attribution
intersects the `dst=` addresses with the link ids from `LINK_REQUEST_RX` and
`LINK_ESTAB` in the two window slices; announce counts are `grep -a ANN_RX`
and `ANN_TX` over the events slice bounded by `t=` in
`[41076000, 58776000]`.

## Changelog

**2026-09-27, second revision (#433, correction of the first).** Every airtime
figure in sections 1 and 5 was recomputed, and the frame census was not
touched. The first revision charged each frame with a hand-written
transcription of the Semtech payload-symbol formula that dropped the `+4` of
the coding-rate factor `(CR-4)+4` (`rnode.rs:1109,1116`): each group of coded
bits cost one symbol instead of five, so a 119 B link proof was priced 126 ms
where the firmware's own `frame_airtime_cost_ms` charges 380 ms, a factor 3.0
on everything but the preamble. Receive-side packets were additionally priced
as if they were frames, which loses a header byte per frame and a whole second
preamble on every packet above 254 B — and 562 of the inbound packets are
259 B. Consequences: channel occupancy 2.79 % becomes 9.17 %, the "97 % idle"
headline goes, the dead-handshake bill 80.1 s becomes 238.0 s, and the base
radio turns out to have keyed 72 % of its lawful hourly budget in its busiest
hour rather than never coming near it. Section 1's "one open number for the
rig" is closed rather than restated: pass 364 verified the Pocket's ledger
directly at ratio 1.00 over 3.3 hours, and the factor 4.3 it was built on was
both mispriced and structurally unavailable, because a half-duplex receiver's
demodulated total cannot bound a neighbour's keyed total. The numbers now come
from the firmware's functions through a committed program
(`leviculum-std/examples/lora_airtime_census.rs`) rather than from a second
copy of the arithmetic, which is the whole reason the first revision could be
wrong while looking self-consistent.
