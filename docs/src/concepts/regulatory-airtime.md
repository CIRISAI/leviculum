# Regulatory Airtime

Unlicensed LoRa bands are shared under duty-cycle rules. This page
records where the limit is enforced, what a node does when nobody
configured one, what it takes to switch the limit off, and one
measurement pitfall. It is a durable rule for every radio firmware we
write, present and future.

## Enforcement belongs in the firmware, not the host

The firmware is the only place that knows what actually went on the
air: retransmissions, preambles, frames queued by a host that has
since crashed — none of that is visible from above. A host-side
budget can shape traffic, but only the modem firmware can *enforce* a
duty cycle, because only it stands between the queue and the antenna.

The RNode firmware is the model: it accounts every transmitted
frame's airtime into rolling bins, raises `airtime_lock` when the
short- or long-term limit is exceeded
(`reference/RNode_Firmware/RNode_Firmware.ino:1673-1675`), and gates
the transmit queue on it — `if (!airtime_lock && queue_height > 0)`
(`RNode_Firmware.ino:1624`). The limits arrive from the host as
`CMD_ST_ALOCK` / `CMD_LT_ALOCK` (`Framing.h:36-37`), but the
*enforcement* never leaves the device.

Our LNode firmware enforces the same way: `AirtimeTracker`
(`leviculum-core/src/rnode.rs:1246`) mirrors the RNode ledger, and
the nRF TX path holds a queued frame instead of keying the radio
while the tracker is locked (`leviculum-nrf/src/lora.rs:725`),
continuing to listen so RX is not starved.

The host-side airtime credit bucket
(`leviculum-std/src/interfaces/airtime.rs`, see
[Interface Isolation](interface-isolation.md)) is *backpressure*, not
regulation: it keeps the serial queue from absorbing minutes of
backlog. It is a comfort for the stack, not a legal control, and
nothing may treat it as one.

## Lawful by default

A node that is not told otherwise obeys the band it is on. When no
`airtime_limit_long` is configured, the host derives the lawful
long-term limit from the TX frequency (`resolve_lt_alock`,
`leviculum-std/src/driver/mod.rs:329`) and sends it to the modem; a
standalone LNode whose host never sent one derives it in the firmware
from its own frequency (`firmware_default_lt_alock`,
`leviculum-core/src/rnode.rs:1159`). Both read the same table,
`etsi_eu868_duty_cycle` (`leviculum-core/src/rnode.rs:1131`), which
carries the EU 863-870 MHz sub-bands with their 0.1 % / 1 % / 10 %
duty cycles. An explicit configured value always wins — including an
explicit `0`, which the firmware reads as unlimited.

Two honesty notes, both deliberate:

- The table is *attributed* to EN 300 220-2 / ERC 70-03 but has not
  been verified against the standard text itself. If you have the
  standard in front of you, checking the table against it is welcome
  and overdue.
- The table covers only EU 863-870 MHz. Other bands (US 902-928,
  AU/NZ, ...) have no citable source in this tree, so they get *no*
  auto-limit and a warning that says so — a limit invented from
  memory would read as authoritative to exactly the operator who most
  needs it not to be. Supply the citation and the table grows.

Python-Reticulum does not do lawful-by-default; the cap only shapes
local TX and is invisible to receivers, so this is a Priority-1
enhancement under the
[deviation rule](python-rns-compatibility.md#the-deviation-rule).

## Disabling is an operator act, not a test convenience

Switching the limit off is sometimes legitimate — a shielded bench
with dummy loads, a throughput scenario that cannot measure what it
exists to measure at 1 % duty. But it is an *operator decision with a
paper trail*, never a default and never a convenience:

- It requires a written justification. Periculum's
  `[disable_airtime_lock]` section refuses to parse without one
  (`periculum/src/topology.rs:258`, `DisableAirtimeLockDef`).
- Every run that had the limit off must **say so** — in its terminal
  output (the airtime banner prints the rendered limit per frequency,
  whichever route produced it) and in its result document (the
  measurement cell records the policy, the rendered limit, and the
  lawful limit for that frequency, `AirtimeContext` in
  `periculum/src/bench.rs`, with the source — scenario or rig —
  recorded in the results).

Of the three possible outcomes, a silent green under a lifted limit
is the worst:

1. **Red under the lawful limit** is honest: the design exceeds the
   band's budget, and the result says exactly that.
2. **Green with a declared lifted limit** is honest: it measures the
   stack, not the law, and every reader can see which.
3. **Silent green under a lifted limit** is a lie with a green
   checkmark: it reads as evidence that the system works lawfully
   when it never once ran under the law. It also poisons comparisons
   — a figure taken with the lock off next to one taken with it on is
   a comparison of the lock, not of the stack — and it ships that lie
   forward into every document that cites the run.

## Where the bench-level switch lives, and why

The blanket switch for a whole bench lives in the Periculum **rig
profile** (`rig.toml`, `periculum/src/rig.rs`) — *site data*, not
scenario data. Containment is a property of the site: whether the
bench is shielded and on dummy loads is true of THIS rig, not of a
scenario file that travels between benches and operators. A scenario
that must not run unlimited even on such a bench can carry
`[require_airtime_lock]`, which wins. The mechanics are Periculum's
to document; the durable rule here is only the split: *scenario files
describe the experiment, the rig profile describes the site, and the
airtime carve-out belongs to the site.*

## The measurement pitfall: reading the meter restarts it

The duty-cycle history lives in RAM, and on ESP32 targets the RNode
firmware's `startRadio()` zeroes it: it calls `init_channel_stats()`
(`RNode_Firmware.ino:523`), which clears the airtime bins and both
utilisation figures (`reference/RNode_Firmware/Utilities.h:1858`). So
a diagnostic that starts (or restarts) the radio in order to read the
airtime counters measures nothing — the act of taking the reading
destroyed the reading. We hit this in practice.

The general lesson is not radio-specific: **a diagnostic must not
disturb what it measures**, and a diagnostic that can must be checked
for it before its numbers are believed. See
[Evidence and Honesty in Testing](evidence-and-honesty.md).

## The firmware ledger is not a cross-session account

The same fact has a second consequence, and it is the one that decides
where an hour-scale budget lives. Because a radio start clears the
bins, the firmware's long-term figure covers *airtime since the last
radio start*, not the rolling hour. It is a lower bound, and the bound
is zero exactly when the question is worth asking: a harness that
reboots a board to give a test a defined starting state (Periculum
does, before every scenario that binds one) has zeroed it, and the
daemon under test zeroes it again when it brings the radio up. An
offline radio's history survives in RAM and cannot be read out-of-band
at all — the only way to make the firmware emit it is the call that
clears it first.

So: **enforcement belongs to the firmware, but the hour-scale account
belongs to whoever drives the radio.** The board is the only thing that
can refuse to transmit, and the only thing that cannot tell you what it
transmitted an hour ago. Anything that needs to know — a test harness
spacing its runs, a scheduler shaping traffic — keeps its own ledger
and states plainly that the figure is modelled, not measured, and a
floor rather than a total. A reset makes the board forget what it
radiated; it does not make the airtime unspent.

## See also

- [Interface Isolation](interface-isolation.md) — why airtime
  *backpressure* is host-side and per-interface while airtime
  *enforcement* is firmware-side.
- [Python-RNS Compatibility](python-rns-compatibility.md) — the
  deviation rule that lawful-by-default satisfies.
- [Evidence and Honesty in Testing](evidence-and-honesty.md) — the
  wider discipline behind "say so in the output".
