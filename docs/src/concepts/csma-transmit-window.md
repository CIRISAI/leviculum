# The randomised pre-transmit window

Two nodes released by the same event reach their radios at the same
instant. Carrier sense cannot separate them, because both probe a
channel on which neither has keyed yet. The only thing that can is a
randomised wait drawn before the probe, and the only question worth
arguing about is what that wait is made of.

This page is the study Codeberg #347 asked for: five questions, each
with a number, the reason for it, and the artifact that settled it. It
is written after the fact. The window landed while the study was still
open, in `a_directed_packet_is_jittered_on_acquisition_and_free_in_a_burst`
(`leviculum-std/src/interfaces/rnode.rs:4981`)
and the firmware policy behind it, so four of the five questions are
answered by code rather than by argument. The fifth is not, and is
stated as open at the end.

Question 6 was asked separately, as Codeberg #40 against the reference
firmware, and it is the same question about the other end of the window:
not what the wait is made of but where it starts. It is answered here
because the answer is made of the same five artifacts.

## What we build it out of

| Term | Value | Where |
|---|---|---|
| Slot | 12 symbol times, clamped to `[24, 100]` ms, floor 6 ms above 30 kbps | `jitter_slot_ms` (`leviculum-nrf/channel-access/src/lib.rs:97`) |
| DIFS | 2 slots (SIFS is 0) | `JITTER_DIFS_SLOTS` (`leviculum-nrf/channel-access/src/lib.rs:75`) |
| Contention window | uniform over 0..=13 slots | `JITTER_CW_SLOTS` (`leviculum-nrf/channel-access/src/lib.rs:79`) |
| Owed when | once per channel acquisition, never per packet | `channel_released` (`leviculum-nrf/channel-access/src/lib.rs:195`) |
| Discharged by | listening it through, not by being asked for it | `jitter_spent` (`leviculum-nrf/channel-access/src/lib.rs:238`) |

## 1. What is the window sized from?

**From symbol time, not from milliseconds.** A slot is 12 symbol times,
so it tracks the modulation the way the frames it separates do. What
that yields, at the PHYs the corpus actually runs:

| PHY | Slot | DIFS + widest draw | Mean wait |
|---|---|---|---|
| SF7/125 kHz | 24 ms | 48..360 ms | 204 ms |
| SF8/125 kHz (project default) | 24 ms | 48..360 ms | 204 ms |
| SF9/125 kHz | 49 ms | 98..735 ms | 416 ms |
| SF10/125 kHz | 98 ms | 196..1470 ms | 833 ms |
| SF12/125 kHz | 100 ms | 200..1500 ms | 850 ms |
| SF5/500 kHz | 6 ms | 12..90 ms | 51 ms |

The table is pinned, not quoted:
`the_widest_acquisition_wait_is_a_function_of_the_modulation`
(`leviculum-nrf/channel-access/src/lib.rs:336`).

A millisecond constant would have been wrong in both directions. At
SF12 the clamp is what binds and 12 symbol times would be 393 ms, so
the ceiling is doing real work; at SF5/500 kHz the floor is what binds
and the raw figure is under a millisecond. Between them the value moves
by a factor of four.

**What confirms this is the right relation rather than a transcription
of the reference.** #344 measured the reference's own inter-frame gap
off the air: never closer than ~80 ms, median 205 ms over 81 gaps. The
model above predicts a median of DIFS plus the median draw, 48 + 156 =
**204 ms**, and a floor of DIFS alone, 48 ms. The median agrees to one
millisecond; the floor is a bound the observation respects rather than
a prediction it confirms. Our firmware put two packets 15 ms apart
before the window existed, which is below the model's floor by a
factor of three, and that is the defect the window closed.

## 2. Does it widen under load?

**No, and the reason is that we already widen on something better.**

The reference keys four bands to a measured airtime average and walks
the window from 0..14 slots up to 45..59 (`update_csma_parameters`,
`reference/RNode_Firmware/RNode_Firmware.ino:1603`). We mirror band 1
only. Under sustained contention our CAD retry gate doubles its own
contention window per busy probe, from `CAD_CW_INITIAL` to
`CAD_CW_MAX` (`leviculum-nrf/channel-access/src/lib.rs:71`), which
reacts to a channel observed busy rather than to an airtime average
computed over the last several seconds. Adding the band escalation on
top would widen the window twice for the same congestion.

This is a deviation under the project's deviation rule: the wire format
is untouched, a peer expects no particular window from a neighbour, and
reacting to the observed channel is what Priority 1 asks for.

**One number in this area is not settled, and it is an off-by-one in the
reference rather than in us.** `update_csma_parameters` assigns
`cw_min`/`cw_max` only when the band *changes*
(`reference/RNode_Firmware/RNode_Firmware.ino:1616`). A board boots in
band 1 with `cw_max` declared as `CSMA_CW_PER_BAND_WINDOWS`, i.e. 15
(`reference/RNode_Firmware/Config.h:127`), and only an excursion into
band 2 and back rewrites it to `band * 15 - 1`, i.e. 14. So the
reference has two band-1 windows depending on its history: 15 equally
likely draws as booted, 14 after an excursion. We mirror the second.
The two predict a median gap of 216 ms and 204 ms; the bench measured
205. That is suggestive and not decisive at n=81, and changing
`JITTER_CW_SLOTS` is a radio-behaviour change, so it stays open.

## 3. Every access, or only contended ones?

**Every acquisition, and no packet inside a burst.**

"Acquisition" is the unit, not "packet": a frame that continues a burst
we are already transmitting owes nothing, because the frame before it
served the wait. The wait comes back when the channel is handed back,
which the transmit path does after its post-TX listening window
(`leviculum-nrf/src/lora.rs:1902`).

Asking for the wait does not discharge it. The wait is spent listening
and the listen returns early on a reception, so a wait cut short by an
incoming frame has de-tiled nothing: the frame that ended it released
every other waiting node at the same instant. Only listening it through
counts (`acquisition_jitter_ms`,
`leviculum-nrf/channel-access/src/lib.rs:222`).

**What it costs.** At the project default PHY the mean cost is 204 ms
per acquisition and the worst case 360 ms. A link setup is three
acquisitions on each side, so the window adds roughly 0.6 s of median
latency to a link establishment at SF8/125 kHz and up to 1.1 s in the
tail. At SF10 those become 2.5 s and 4.4 s. That is the price, and it
is paid against a collision whose cost is a whole frame plus a
retransmission timeout.

There is no "the channel was clear, skip it" shortcut, and there must
not be one: the case the window exists for is precisely the one where
the channel *is* clear for both senders.

**There is also no packet-type shortcut.** A high-priority frame at the
head of an idle queue used to key the radio outright, which meant only
announces were ever jittered. That is type-awareness in collision
avoidance and it is the thing the interface-isolation rule forbids; see
[Interface isolation](interface-isolation.md). Both halves of it are
pinned now, the answering half and the queue jumper's.

## 4. What does it do to the ack window, the burst yield, and link setup?

The pre-review audit named the post-TX receive window as the one thing a
transmit window could break, and the arithmetic says it mostly does not.

The window the transmit path opens after every transmission is one full
single-frame reply airtime plus a turnaround margin
(`post_tx_rx_window_ms`, `leviculum-nrf/src/lora.rs:697`). Its
turnaround term budgets the peer's DIFS and nothing else, so it was
written before the peer had a contention window to draw. The right
comparison is against the peer's time to *key up*, not to finish its
frame: the receiver stops its timeout on preamble detect and then runs
to packet completion regardless of length
(`SET_STOP_RX_TIMER_ON_PREAMBLE`, `leviculum-nrf/src/sx1262.rs:753`).

| PHY | Post-TX window | Peer's widest wait | Covered |
|---|---|---|---|
| SF7/125 kHz | 666 ms | 360 ms | yes |
| SF8/125 kHz | 1094 ms | 360 ms | yes |
| SF9/125 kHz | 1866 ms | 735 ms | yes |
| SF10/125 kHz | 3338 ms | 1470 ms | yes |
| SF12/125 kHz | 10000 ms (clamped) | 1500 ms | yes |
| SF7/250 kHz | 396 ms | 360 ms | yes, by 36 ms |
| SF7/500 kHz | 270 ms | 360 ms | **no** |
| SF5/500 kHz | 189 ms | 90 ms | yes |

The window is airtime-derived and the wait is slot-derived, so they
diverge exactly where the slot's 24 ms floor stops tracking a shrinking
airtime: at wide bandwidths and low spreading factors. SF7/500 kHz is
the one PHY in the table where the listening window closes before the
peer can be expected to have keyed.

Even there the peer is not necessarily missed, because our own next
acquisition owes its jitter immediately afterwards and that wait is also
spent listening, so the composite listen is 270 + 48..360 ms. Missing
the reply needs the peer to draw high and us to draw low in the same
exchange. That is a probability, not a guarantee, and a probability is
not what an ack window should rest on.

`burst_should_yield` (`leviculum-core/src/rnode.rs:1573`) is unaffected:
it bounds a burst by frame count and accumulated airtime, and the window
is spent before the burst starts rather than inside it.

## 5. Do we listen during the wait?

**Yes, and it is the whole reason the wait is safe to impose.** A wait
that went deaf would trade a collision for a missed frame, which is the
same loss at the layer that counts. The transmit path arms the receiver
for the drawn duration and reports back what it actually listened
through, and a reception that cuts the wait short leaves the debt
standing (`leviculum-nrf/src/lora.rs:1650`).

## 6. Does the window's floor matter?

**No, and that is worth stating because it is the remedy an observer
reaches for first.** Codeberg #40 recorded the reference's light-traffic
window as `cw_min = 0` (`CSMA_CW_MIN`,
`reference/RNode_Firmware/Config.h:109`) and proposed raising the floor to
two or three slots "so a competing node's preamble falls into the other's
CAD window". Our draw mirrors that window, so the proposal reads as a
proposal about us too. Three things about it do not hold, and none of them
needs a new measurement.

**A term both ends owe cancels out of what separates them.** Two nodes
released by the same event are separated by the *difference* of their
waits, not by either wait, so a constant added to every wait moves the
whole distribution of waits and leaves the distribution of separations
exactly where it was. That is not an argument but a pin, and it was
already made for a different proposal of the same shape: a deferral of one
frame airtime on every answer, refused in
`direction_3_a_deferral_that_is_a_constant_cancels_and_buys_nothing`
(`leviculum-std/tests/mvr/two_responders_overlap_inside_one_airtime.rs:503`),
which asserts the deferred census equal to the baseline one count for
count and says in place that a constant common to both cancels the way
DIFS already does. A floor is that constant, spelled in slots instead of
airtimes.

What a floor does buy is latency: at SF10/125 kHz two more slots cost
every acquisition another 196 ms out of the budget priced in question 3.
Its one second-order effect points the same way rather than the other: a
longer wait is longer exposed to a *third* node's frame arriving inside
it, and a wait cut short that way re-anchors both ends on that frame at
the same instant — the same mechanism that makes deliberate carrier sense
a losing direction here, priced in
`direction_4_carrier_sense_re_anchors_the_pair_and_doubles_the_odds`
(`leviculum-std/tests/mvr/two_responders_overlap_inside_one_airtime.rs:551`).

**Our pre-TX wait has never been able to be zero anyway.** The draw's own
floor is zero, but a floor of zero draws is not a floor of zero
milliseconds: every acquisition also owes DIFS unconditionally, two slots
(`JITTER_DIFS_SLOTS`, `leviculum-nrf/channel-access/src/lib.rs:75`), which
is the `narrowest` column of question 1's table — 48 ms at the bench PHY,
12 ms at SF5/500 kHz, 200 ms at SF12
(`the_widest_acquisition_wait_is_a_function_of_the_modulation`,
`leviculum-nrf/channel-access/src/lib.rs:336`), and pinned again through
the draw itself for a thousand seeds in
`boot_owes_jitter_and_the_draw_is_difs_plus_a_bounded_window`
(`leviculum-nrf/channel-access/src/lib.rs:393`). The reference is no
different: `tx_queue_handler`
(`reference/RNode_Firmware/RNode_Firmware.ino:1623`) waits `difs_ms`
(`reference/RNode_Firmware/Config.h:119`), also two slots, and waits it
while sensing: a medium that goes busy clears `difs_wait_start`, so the
DIFS restarts from the top, while the contention countdown only freezes
(`cw_wait_passed` survives and is reset at the flush, not at the
interruption). `cw_min = 0` means the *contention* term can be zero, not
that a node transmits the instant it is handed a frame.

**And the colliding set is not "both drew zero".** #40 priced the risk at
`1/15²`, which is the chance of that one pair. Two ends that draw the same
value, whichever value it is, end their waits in the same slot, so the
colliding set is every equal pair and its size is one over the number of
draws: 1/15 in the reference as booted and 1/14 after an excursion
(question 2), and 1/14 for us. That figure is the one the arms are
measured against —
`FrameClass` (`leviculum-std/src/interfaces/rnode.rs:376`) takes a
same-class pair to 1/56 and states in the same place that the count alone
leaves it at 1/14, i.e. that a change which does not increase the number
of distinguishable outcomes buys nothing. A floor does not increase it.
Width, quantisation against the frame, and per-identity pinning do, and
which of those wins is the open A/B, not this.

None of it is reachable from the host in any case, which is what #40
concluded and still holds: the reference's whole command set carries one
CSMA opcode and it is a read-only stat, `CMD_STAT_CSMA`
(`reference/RNode_Firmware/Framing.h:48`); the nearest thing to a setter,
`CMD_DIS_IA` (`reference/RNode_Firmware/Framing.h:67`), switches
interference avoidance off and never touches the window. The only window
we can change is our own.

## What is still open

1. The post-TX receive window's turnaround term still budgets the peer's
   DIFS without its contention window, and at SF7/500 kHz that is 90 ms
   short. Sizing it is a radio-behaviour change and wants a rig
   measurement with the other medium switched off, so it belongs in its
   own issue rather than here.
2. `JITTER_CW_SLOTS` mirrors the reference's post-excursion band-1
   window, 14 draws, where a freshly booted reference uses 15. See
   question 2.
3. `CSMA_DIFS_MS` and `CSMA_MAX_CW_MS` (`leviculum-core/src/rnode.rs:948`)
   are millisecond constants pinned to a 24 ms slot and are therefore
   wrong at every SF above 8. Their only consumer is `compute_spacing_ms`
   (`leviculum-core/src/rnode.rs:1047`), which has no caller: the host
   interface prices the same shape from the modem's reported slot
   instead (`tx_hold`, `leviculum-std/src/interfaces/rnode.rs:732`).
   Nothing is broken by them today and something would be by the next
   caller.
