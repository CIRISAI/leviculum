# Announce cadence

A board decides for itself how often it announces. There is no stationary
or mobile profile to pick at flash time: a board observes whether it moves
and announces every five minutes while it does and once an hour while it
does not, with an immediate announce when movement resumes or an unknown
neighbour is heard, and the whole set stretched when the PHY makes it
unaffordable (Codeberg #401, decided 2026-09-15). The rule and its
constants live in one place, `leviculum-nrf/announce-policy/src/cadence.rs`,
whose module doc is the reference for the five rules.

## Movement has to be proven

"Moving" is not a single observation. `MovementDetector` in that file
counts a board as moving only when three consecutive usable fixes lie past
the 50 m threshold (`MOVEMENT_MIN_CONSECUTIVE_FIXES`), a fix without a good
HDOP counts as no fix at all, and the fast state ends 15 minutes after the
last confirmation (`MOVEMENT_CONFIRM_TIMEOUT_MS`). The reason is the board
on a mast or a tree: its position wanders while it stands still, and a
single implausible fix must not be able to switch it to the fast cadence
and spend the channel.

## The accelerometer gates the proof, it does not replace it

The SenseCAP T1000-E carries a QMA6100P accelerometer, so the obvious
question is whether it should say "moving" directly (Codeberg #457). Lew's
decision of 2026-10-20: **accelerometer motion gates the GNSS movement
proof. It neither replaces the proof nor adds to it.**

- *Not a replacement:* a tilt, a gust of wind on a mast, a truck passing
  the post or a hand brushing the case all move an accelerometer. Letting
  any of them switch the cadence is exactly the failure the three-fix proof
  was built against.
- *Not an alternative path (OR):* for the same reason. Either source alone
  would then be enough, and the weaker one decides.
- *A gate (AND):* the displacement proof stays the only thing that decides
  "moving", exactly as #401 specifies. The accelerometer decides whether it
  is worth running at all: while the board is still, the GNSS receiver can
  sleep, and motion wakes it so the proof runs as it does today.

What that buys is energy, not a different cadence: a still tracker no
longer keeps a receiver awake to prove, every few seconds, that it has not
moved.

## In two steps

1. **The sensor reads and reports** (#457, step one). The T1000-E image
   drives the part and prints `MOTION state=moving|still hold_ms=<n>
   src=qma6100p` on each change, with `MOTION_INIT ok|absent` at boot
   ([Structured event logs](../structured-event-logs.md)). Moving is one
   sample (of four a second) differing from the last by more than 30 mg;
   still is 30 s without such a sample. In this step nothing read the
   state; step two gave it its one reader. The driver and the
   rule are `leviculum-nrf/motion`, host-tested.
2. **The receiver sleeps while still** (#457, step two). `motion_task`
   writes each edge to a watch the GNSS task selects against its UART read;
   on `still` the task puts the AG3335 into Backup mode (`$PAIR650,0`, then
   its power enable low, VRTC held up so the wake is a warm start), on
   `moving` it raises the enable, pulses the RTC wake line and sends the
   `$PAIR` setup again. Each edge prints `GNSS_POWER state=sleep|wake
   reason=still|moving t=<ms>`. Until the first fix, or
   `GNSS_FIRST_FIX_GRACE_S` (10 min) after boot, `still` keeps the
   receiver awake, so a board that boots lying still still learns where it
   is. The decision is `leviculum-gnss-presence`'s `PowerGate`,
   host-tested. The proof's inputs and thresholds do not change; it simply
   gets no fixes while the board is still, which is what a still board
   would have produced anyway: the fix snapshot holds its last position.

A board without an accelerometer, or one whose part does not answer
(`MOTION_INIT absent`), keeps its receiver awake and runs the proof as
today. The gate only ever removes work; it never adds a way to become
"moving".

## The position announce

A board that shares its position announces a third destination on the
same cadence (order 1301): `lev.position`, a SINGLE destination of its own
identity, whose announce `app_data` carries its fix. Every node in range
hears it, so a map shows the mesh's nodes without a mailbox and without a
message. The wire is the `lev` namespace's
([Read receipts, section 1](loquax-read-receipts.md#the-announce-aspect-levposition));
the codec is `leviculum-lxmf/src/lev_position.rs`, the rules below are
`leviculum-nrf/announce-policy/src/position.rs` and the slot `Position` of
`cadence.rs`, the wiring `leviculum-nrf/src/position.rs`.

Never in the delivery announce's `app_data`: the reference reads that one
as LXMF's fixed list `[name, stamp cost, [capabilities]]`, and a payload
whose first byte is no list marker is decoded whole as a UTF-8 name
(`reference/LXMF/LXMF/LXMF.py:151-172`), so a map there is a broken name
on every Python and Sideband peer.

### Who reads it

An announce is plaintext: a position in the clear tells everyone in radio
range where the board is. The rule:

| The board | By default | Configured (`lnflash --share-position`) |
|---|---|---|
| reports its position by LXMF telemetry as a tracker (a telemetry target with the tracker profile) | **sealed to the owner**: encrypted to the identity its telemetry already goes to, so the owner's homes and devices read it and nobody else | `off`, `owner`, `everyone` |
| any other board, a plain mesh node, a station | **nothing** | `owner` (needs a telemetry target), `everyone` |

`everyone` is the opt-in "share my position with everyone nearby", the
plain form, and never a default. The setting is persisted on the config
page and in force at once; the board writes `[POSITION] share=<whom>
setting=<what>` on each change. A sealed announce whose owner key the
board does not know yet waits for it (`[POSITION] withheld
reason=no-owner-key`); the telemetry reporter already asks the mesh for
it. The next step of the study 1296 is a group key shared with an
off-grid group, so friends' phones read it too.

### When it goes out

- **The decided cadence**: every five minutes while moving, every hour
  while still, at once on movement resuming or a new neighbour, the whole
  set stretched by rule 5. The first one 50 s after boot, ten after the
  board's own, so the boot announces each have their airtime window.
- **No fix, no announce**: a due position announce without a fix (no
  valid GNSS fix inside the tracker's HDOP gate, no fixed position) sends
  nothing and looks again a minute later (`[POSITION] withheld
  reason=no-fix`). A fixed position somebody set is shared as source
  `manual`.
- **The 25 m advance**: a fix 25 m or more from the one last announced
  brings the next position announce forward to the earliest moment the
  duty budget allows (the budget's spacing, never under a minute), never
  past the cap and never later than it already was.
- **Thinning**: when the channel load the CHTM measures over the hour
  (`channel_load_long`) passes the announce cap's 2 %, the position
  interval stretches in proportion (twice the load, twice the interval),
  up to the hourly floor. The delivery announce is not thinned. Above the
  threshold the mesh's position announces stay roughly constant in number
  however many boards move.

The duty budget's set counts the position: two frames for the sealed form,
one for the plain.

### The double press

A double press of the user button announces the position too (order
1316, Lew's one-button rule of 2026-10-20: short press status, double
press announce), on the same press as the delivery and propagation
announces and after them. It does so when the share resolves to `owner`
or `everyone` and a fix or a fixed position is there, with a plausible
clock and, sealed, the owner's key; the readiness check is
`for_button` (`leviculum-nrf/src/position.rs:399`). The press is booked
in the one cadence (`AnnounceCadence::manual`,
`leviculum-nrf/announce-policy/src/cadence.rs:792`): the delivery and
propagation halves decide whether it goes out now under rule 5's
spacing, and the position joins them when its own last announce is at
least that spacing ago; otherwise its slot is brought forward and its
own tick answers as early as the band allows. The line is `[ANNOUNCE]
sent ... reason=position-button` (`leviculum-nrf/src/announce.rs:549`).
Without a fix the press adds nothing new: the existing `[POSITION]
withheld reason=no-fix` line, once per change, is the answer. With
sharing off a press books the two slots it booked before. Host tests:
`a_double_press_with_position_shared_books_the_position_slot` and
`a_double_press_with_sharing_off_books_only_the_two_halves` in
`cadence.rs`.

### What it costs

At our defaults (869.463 MHz, BW 125 kHz, SF8, CR 4/5, 18-symbol preamble),
pinned by `leviculum-lxmf/tests/lev_position_airtime.rs`:

| Announce | Bytes | Airtime direct | Airtime relayed |
|---|---|---|---|
| `lxmf.delivery`, a 14-byte name | 186 | 554 ms | 595 ms |
| `lev.position`, plain (42-byte map) | 209 | 605 ms | 646 ms |
| `lev.position`, sealed (130-byte `app_data`) | 297, two LoRa frames | 913 ms | 954 ms |

The seal costs 88 bytes (32 ephemeral key, 16 IV, the map padded to 48,
32 HMAC, the `bin8` header) and 308 ms: 297 bytes do not fit one 254-byte
LoRa frame, and no sealed form can on a 167-byte announce.

Per hour, one board:

| | Before | With the sealed position | With the plain position |
|---|---|---|---|
| moving (12 sets) | 6.6 s, 0.18 % | 17.6 s, 0.49 % | 13.9 s, 0.39 % |
| still (1 set) | 0.55 s | 1.5 s | 1.2 s |

Walking with the 25 m advance adds about nine position announces an hour
(the scripted walk in `cadence.rs`), 0.72 % in all, inside the tenth of the
10 % sub-band a board's own announces may spend (1 %).

The mesh, N moving boards each forwarded once by one relay, whose announce
cap (2 % of its interface) is 72 s an hour:

| Moving boards | Channel occupancy | Relayed announce load at the relay |
|---|---|---|
| 1 | 1.0 % | 18.6 s, inside the cap |
| 5 | 5.0 % | 93 s, past the cap |
| 10 | 10.1 % | 186 s, past the cap |

Without thinning the relay's cap is full at **four** moving boards (it was
ten before the position announce). The thinning starts where the measured
load passes 2 %, at two moving boards in this mesh, and holds the mesh's
position announces at about 24 an hour, so with the delivery announces the
cap holds up to six moving boards; past that the delivery announces alone
approach it, which is the cadence's business, not the position's.

### Cells to come

Register for the hardware lane (lane 2, after this lands):

| Cell | Setup | Holds |
|---|---|---|
| `lora_position_announce_relayed` | a T1000-E with a GNSS fix (outdoors, or a fixed position) and a telemetry target with the tracker profile; a T114 relaying; a host `lnsd` with a listener holding the owner identity | the listener receives a sealed `lev.position` announce through the T114 and opens it to the T1000-E's fix and name; a second listener with another identity receives the same announce and reads no position (positive control: `--share-position everyone`, then it does) |
| `lora_position_announce_no_fix` | the T1000-E in the shielded room, no fix, no fixed position | no `lev.position` announce on the air for an hour; `[POSITION] withheld reason=no-fix` once |
| `lora_position_announce_off_by_default` | a T114 with no telemetry target | no `lev.position` announce, `[POSITION] share=off setting=default` at boot |

## See also

- [Regulatory airtime](regulatory-airtime.md), for rule 5's duty budget.
- [Telemetry](telemetry.md), whose tracker policy supplies the distance and
  HDOP thresholds the proof reuses.
- [Operating modes](operating-modes.md), for Tracker mode, which reports
  at this cadence and sleeps in between.
- [One button](one-button.md), for the double press that announces.
- [Flashing an LNode](lnode-flashing.md#the-position-share), for
  `--share-position`.
