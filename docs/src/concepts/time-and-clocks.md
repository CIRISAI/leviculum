# Time and Clocks

How a node keeps calendar time, why it must always be able to, and how
a wrong calendar heals. This applies to every firmware and every
platform port, present and future. Issues come and go; this concept
stays.

This page is the binding spec of the anchor model. It replaces the
earlier doctrine "no verified clock, no authorship": the rule is now
that **every instance always authors, stamped with its best honest
estimate, biased backwards when unsure, never forwards** — and that
foreign garbage timestamps must never break an instance. The sections
below define the estimate, the one filter every time source passes,
and the loop that heals a wrong calendar.

## The two poisons

Calendar failures hurt in exactly two directions, and they are not
symmetric.

**Too old, self-consistently.** A Reticulum peer orders
same-destination paths by the announce emission timestamp. Python-RNS
stamps `int(time.time())` — epoch seconds — into the announce random
hash (`reference/Reticulum/RNS/Destination.py:282`) and replaces a
stored path only when a new announce carries a *newer* emission than
the stored one (`announce_emitted > path_timebase`,
`reference/Reticulum/RNS/Transport.py:1772` and `:1809`). The value
we stamp is therefore compared on other machines, across our reboots,
against values our earlier selves emitted. A clock that is merely
self-consistent — process uptime — restarts from zero on every reboot
and loses that comparison forever: the node keeps announcing, and no
peer ever updates its path entry again (Codeberg #155). Obtaining
usable time is a protocol obligation, not a platform convenience.
#155 is one instance of a general failure class — a generated field
that is self-consistent between our writer and our reader and means
something else to a peer; the class and the audit method for it are
in [Wire Field Semantics](wire-field-semantics.md).

**Too far in the future.** The reverse error is worse, because it
poisons *others*, silently and permanently. Receivers advance
monotonic cursors over the stamps they ingest — the worked case is
the telemetry collector cursor ([Telemetry](telemetry.md)), which one
future-stamped row raises past every honest later reading, forever,
with no refusal logged anywhere. A past-stamped message, by contrast,
merely sorts too far back: visible, attributable, self-limiting.

These two poisons shape the whole model. A node must always be able
to author — an instance that falls silent for lack of a clock fails
the switch-on-and-it-works requirement exactly when it is needed —
and when its calendar is uncertain, the error must land in the benign
direction: backwards, never forwards.

## Two clocks, strictly separated

A node runs two clocks with disjoint jobs, and no time correction
ever crosses from one to the other.

**The stopwatch** is the monotonic tick counter, `Clock::now_ms`. All
timeout and deadline arithmetic stays on it (`traits.rs:323`) —
retries, link timeouts, announce cadences. No anchor change, GNSS
fix, or healing step ever stretches or shrinks a protocol timer.

**The calendar clock is an estimate, not a measurement:** an anchor
(unix seconds, obtained from some source) plus the stopwatch time
elapsed since that anchor was seated. Two properties follow by
construction:

- **It never stands still.** Between anchors it advances with the
  stopwatch, so repeated identical stamps — the #217 class of
  same-timestamp ID collisions — cannot come back.
- **It moves in jumps only when a better anchor re-seats it.** An
  anchor change is an explicit, recordable event with a source, not
  a drift.

The clockless arm of the implementation already has this shape: a
learned floor advanced by the monotonic clock (`transport.rs:2846`).
On std platforms `SystemClock` answers from `SystemTime`
(`leviculum-std/src/clock.rs:45`) — the same estimate with the
anchor-keeping delegated to the OS and its NTP discipline.

## One value, one producer

`Transport::emission_secs` (`leviculum-core/src/transport.rs:2841`)
is the single point that turns the calendar estimate into the
unix-seconds value for wire fields that peers compare across our
process lifetimes: announce emission timestamps
(`leviculum-core/src/announce.rs:156`) and request timestamps
(`leviculum-core/src/node/mod.rs:1054`, `:1133`, Codeberg #164). Any
new wire field with cross-lifetime semantics draws from it too —
never from the monotonic `Clock::now_ms`, which is a timer, not a
calendar.

**Crates layered on `NodeCore` reach the same producer, they do not
take a parameter.** `NodeCore::emission_secs` exposes it; LXMF's three
cross-lifetime fields — the message timestamp, the ticket expiry and
the propagation upload timestamp — resolve through it inside the
router (`leviculum-lxmf/src/router.rs`, Codeberg #182). They used to
arrive as a `now_unix: f64` argument on `enqueue`, `tick`,
`handle_event` and `issue_ticket_field`, which is the #155 shape with
the defect moved into the caller: nothing about the signature stops a
clockless node from passing uptime seconds. An API that cannot be
called wrongly beats a doc comment warning about it. Pinned at
`leviculum-lxmf/tests/wall_clock_producer.rs`, structurally (no public
router entry point takes an `f64`) as well as by value.

### One producer, two resolutions

The field decides the resolution, not the clock. `emission_secs`
returns whole seconds for fields that are whole seconds on the wire —
the 5-byte announce emission timestamp above all. `emission_micros`
returns the same instant in microseconds, and
`NodeCore::emission_secs_f64` divides it back into fractional unix
seconds for fields that are floats. They are the same producer with
the same source ranking:
`emission_micros(now) / 1_000_000 == emission_secs(now)` on every arm.

The float resolution exists because LXMF hashes the message timestamp
into the message ID. At whole seconds, two identical messages created
inside one second are one ID, and the second is refused as a duplicate
of the first — a message the reference, writing `time.time()`
(`reference/LXMF/LXMF/LXMessage.py:357`), would have sent (Codeberg
#217).

**Microseconds, not milliseconds.** The collision is between two calls
in one code path, not two user actions. Two consecutive
`LxmfRouter::create_message` calls measure ~115 µs apart — each signs
an Ed25519 message — so a millisecond value collides on every such
pair; that was measured at 20 pairs out of 20 before this unit was
chosen. Microseconds is also what the reference effectively produces:
`time.time()` is an f64 of unix seconds, resolving to ~0.24 µs at
present-day timestamps.

On the clockless arm the sub-second part comes from our own monotonic
clock, and `Clock::now_ms` is the only monotonic source there. So
`emission_micros` returns `floor_secs * 1_000_000` plus the
*milliseconds* elapsed since the floor was set, scaled up: monotonic,
separating two instants one millisecond apart, and honest about the
resolution the platform actually has. It is not a claim to know the
wall-clock microsecond, and nothing reads it as one. Platforms whose
`Clock` implements only `wall_unix_secs` get the trait default —
`secs * 1_000_000` — and keep working unchanged, with no precision to
gain and none invented.

### The one refusal left: a field the peer discards in silence

Authorship is never refused — that is the headline rule of this page.
One narrow refusal survives it, and it blocks no message.

The LXMF ticket expiry is compared on the *peer's* clock: a peer
keeps a ticket only while `time.time() < expires` on its own machine
(`reference/LXMF/LXMF/LXMRouter.py:1854`) and says nothing when it
does not. A backwards-biased expiry from an unhealed calendar is
therefore already expired on arrival — issuing it is emitting a field
the peer silently discards. `LxmfRouter::issue_ticket_field` returns
`RouterError::NoWallClock` while the calendar sits below the
plausibility floor of the
[sanity window](#the-sanity-window)
rather than issue one: a named error is a diagnosis; a discarded
ticket is a mystery that surfaces months later as "replies from this
peer are slow".

Fields the peer decides nothing on are always emitted. The LXMF
message timestamp (displayed and sorted, `LXMessage.py:357`
unvalidated) and the propagation upload timestamp (bound and dropped,
`LXMRouter.py:2238-2240`) both flow regardless of clock state — a
backwards-biased stamp mis-sorts, and mis-sorting is the accepted
cost, not a failure. Withholding a message because our clock is
uncertain would be the far worse failure.

The refusal is always on *writing*, never on reading: a peer's ticket
is remembered and used regardless of the state of our own clock.

## The sanity window

Every candidate anchor, from every source — RTC, GNSS, host
injection, network-learned time — passes one filter. No source gets a
special case, and no source bypasses it.

- **Lower bound: the build timestamp.** The firmware or binary build
  time is compiled in: free, always present, and incorruptible by any
  runtime input. Real time is always after it, so any source claiming
  a moment before it — a 1999 RTC with a dead backup cell — is
  deterministically garbage, not merely suspicious.
- **Upper bound: a generous margin above the best known anchor**, on
  the order of decades above the build floor. It only has to separate
  values a real clock could hold from values none can; its exact size
  is a tunable practice parameter (see
  [Practice parameters](#practice-parameters-not-dogma)), not dogma.

A source that fails the window is refused as an anchor — never
"corrected" — and the calendar keeps running on the best anchor it
has. GNSS gets no bypass: it is simply the highest-ranked source
inside the same filter, trusted by default and rejected when
implausible. A receiver subtly shifted *within* the window (a spoofed
or faulty fix that still looks plausible) is an accepted residual
risk, time-bounded by the [healing loop](#the-healing-loop).

**Implementation status.** Today the window is two fixed-date
constants: the lower bound `EMISSION_PLAUSIBLE_MIN_SECS`
(`constants.rs:525`, 2020) and the upper bound
`EMISSION_LEARN_CEILING_SECS` (`constants.rs:507`, 2200-01-01),
enforced on learning and host injection. They approximate the binding
bounds with constants that need no build plumbing; deriving the floor
from the build timestamp was already named as the tightening in #161
§1 and is spec under this model. Until a port plumbs its build
timestamp through, the fixed dates stand in.

## The source ranking

The calendar takes its anchor from the best source available, and a
higher-ranked source re-seats an anchor from a lower-ranked one — the
arms 1 to 4 of the old chain survive here as ranks. The order is by
how hard the source is to fool, not by precision: a GNSS fix is a
live measurement, a host injection is an explicit claim by an
operator, an RTC is whatever it was last set to, and network-learned
time is arbitrary input from anyone in radio range. Every arm passes
the same [sanity window](#the-sanity-window). For each: what it
costs, when it is unavailable, what it guarantees.

### Arm 1: GNSS

Where the board has a receiver — today the WisMesh Pocket V2's
u-blox ZOE-M8Q (`leviculum-nrf/src/gnss.rs`). The NMEA RMC sentence
carries UTC date and time in every fix.

- **Cost:** receiver power, a sky view, and cold-start acquisition
  time (seconds to minutes).
- **Unavailable:** indoors or shadowed — a node without sky view
  never gets a fix, so GNSS *seeds* the calendar, it never replaces
  the ranking below it.
- **Guarantees:** UTC to well under a second, far beyond the
  one-second wire granularity. Trusted by default; a fix outside the
  sanity window is refused like any other source.
- **Status:** the firmware parses RMC but consumes only position and
  validity (`GnssFix`, `leviculum-nrf/src/baseboard.rs:26`, has no
  time field yet). Seeding the calendar from RMC is tracked as a
  firmware implementation issue, not here. See
  [GNSS specifics](#gnss-specifics-for-a-board-bring-up) below.

### Arm 2: Host injection

`Node::set_wall_time_unix_secs` (`leviculum-core/src/node/mod.rs:672`
→ `transport.rs:2664`), for deployments where a clockless node has a
host that does know wall time — e.g. a control frame on the LNode
serial channel (the radio-config envelope of
`leviculum-core/src/rnode.rs`).

- **Cost:** one control-channel frame; requires a host that itself
  has a trustworthy clock.
- **Unavailable:** standalone nodes with no host attached.
- **Guarantees:** host-clock quality, sanity-gated: values outside
  `[EMISSION_PLAUSIBLE_MIN_SECS, EMISSION_LEARN_CEILING_SECS]`
  are refused (`transport.rs:2672`, pinned at `transport.rs:15548`
  and `:15655`), because an injection *claims* to know wall time, so
  a value no real clock can hold is self-refuting.
- **Status:** the core API exists and is pinned by tests; no
  production caller wires it to the LNode control channel yet — a
  separate issue tracks that.

### Arm 3: Platform clock passing sanity

`Clock::wall_unix_secs` (`leviculum-core/src/traits.rs:338`). On std
platforms `SystemClock` answers from `SystemTime`
(`leviculum-std/src/clock.rs:45`) — effectively the OS's NTP-managed
clock. On a board it is a battery-backed RTC, which under this model
also carries anchors *back*: see
[the healing loop](#the-healing-loop) for the write-back.

- **Cost:** none.
- **Unavailable:** on MCUs without an RTC. The LNode's
  `EmbassyClock` (`leviculum-nrf/src/clock.rs`) keeps the trait
  default of `None` — that is correct, not a gap: returning
  uptime-derived values from `wall_unix_secs` would be lying to the
  transport.
- **Guarantees:** whatever the platform clock guarantees — NTP
  quality on a host, last-set-plus-drift on an RTC. An RTC counts
  only when its value passes the sanity window; below the build
  floor it is a dead cell, not a time source. Note the trait
  contract: this is *not* a timer source; all timeout and deadline
  arithmetic stays on the monotonic `now_ms` (`traits.rs:323`).
- **Status:** the implementation consults this arm first when it
  answers (`transport.rs:2612`). No current platform offers both a
  platform clock and GNSS or injection, so the difference in order
  has no behavioural effect today; a port that has both follows this
  ranking.

### Arm 4: Network-learned

The sourceless fallback: `learn_emission_timebase`
(`transport.rs:2976`) adopts the highest emission timestamp seen in
any signature-valid announce as the calendar anchor, then advances it
with the monotonic clock (`transport.rs:2846`). This includes the
node's *own* pre-restart announce echoing back from a neighbour —
learning deliberately runs before the own-destination echo drop, so a
rebooted node re-seeds past exactly the value its next announce must
exceed (pinned at `transport.rs:16662`).

- **Cost:** nothing — no hardware, no host.
- **Unavailable:** on a mesh where no participant has a clock, or
  before the first plausible traffic arrives.
- **Guarantees:** only as good as radio-range neighbours, and
  `validate()` proves only that the announce signs itself — the
  field is arbitrary input from anyone in range. Hence every
  hardening rule below and the median rule of the
  [healing loop](#the-healing-loop). The learned anchor is in-memory
  only on RTC-less boards: after a reboot the node starts from the
  build floor again until live traffic re-seeds it.

### Arm 5: The build floor

The birth state of every instance with no better source: the
calendar anchors at the build timestamp, advanced by uptime. This
replaces raw uptime seconds as the bottom of the ranking, and it is a
*valid* state, not a defect: the stamp is recognisably old, but
unique, monotonic, and non-toxic — it can never poison a peer's
cursor, because it errs backwards. Nobody stays silent, nobody
poisons; the cost is confined to
[the one honest cost](#the-one-honest-cost) below.

- **Cost:** none; the build timestamp is compiled in.
- **Unavailable:** never — that is the point.
- **Guarantees:** uniqueness and monotonicity within the boot, a
  value that is always in the past, and a floor the sanity window
  can trust.
- **Status:** spec. Today an instance with no source emits raw
  uptime seconds (`transport.rs:2619`), which every cross-restart
  comparison loses (#155) and which the old doctrine treated as a
  defect to declare. Plumbing the build timestamp into each port
  retires that state entirely.

## Stamping: always author, biased backwards

Every instance always stamps outgoing fields with its best honest
estimate of now — the calendar clock, whatever its anchor. When the
calendar is uncertain, the uncertainty lands backwards, never
forwards: the poison was never wrong time but *future* time (the
cursor mechanism in [the two poisons](#the-two-poisons) is why —
permanent, silent, hurts others), while past time sorts too far back
and hurts only presentation. A source-less instance therefore anchors
at build-time plus uptime and authors anyway.

The old rule — no verified clock, no LXMF authorship — is withdrawn.
It bought cursor safety by silencing exactly the instances a mesh is
for, and the same safety is now had cheaper: backwards-biased
stamping at the writer, [ingress clamping](#ingress-clamp-for-semantics-keep-for-display)
at the reader.

### The one honest cost

Between cold start and first healing, outgoing messages carry
recognisably old stamps and sort backwards at receivers — a Sideband
conversation shows them out of order until the calendar heals.
Visible, harmless, and it ends at the first plausible contact:
the first GNSS fix, host injection, or plausible foreign message
re-anchors the calendar and normal stamping resumes. We state the
cost rather than hide it, because the alternatives are the two
poisons: silence or invented time.

## Ingress: clamp for semantics, keep for display

The mirror image of backwards-biased stamping protects us from
everyone else. An incoming stamp beyond the local plausible-now is
**clamped to receive time for everything semantic**: indexing,
dedup keys, and above all cursor advancement — in our collector
(#239) and in any future propagation node. The original stamp is
kept alongside as display information, so nothing is destroyed and a
viewer can still show what the sender claimed.

The reason is the cursor mechanism: a cursor that advances over a
raw foreign stamp hands every sender a lever to starve it. Clamped,
the worst a garbage stamp can do is index as "arrived now" — wrong
by presentation, harmless by mechanism. Foreign garbage can no
longer break us, which is the other half of the always-author rule:
authorship without ingress protection would just move the poison
one hop.

Scope: this clamp lives at the application layer — what we index,
serve, and advance cursors over. It does **not** touch announce path
ordering, which stays raw for reference parity; see the
non-behaviours below.

## Rules that hold regardless of source

### The wire field is 40 bits; every producer saturates

The announce timestamp field holds
`8 * RANDOM_HASH_TIMESTAMP_SIZE = 40` bits. A larger value would
silently drop its high bits on the wire and sort *below* every stored
path entry — the node instantly loses path replacement everywhere.
`EMISSION_TIMESTAMP_MAX_SECS`
(`leviculum-core/src/constants.rs:498`) caps it, enforced at the
point of resolution (`transport.rs:2627`) and again at the wire
producer (`announce.rs:167`), so truncation is unrepresentable
regardless of which source produced the value. Incident: Codeberg
#160. Pinned at `transport.rs:15746`.

### The timebase never moves backwards, and adoption is windowed

Within arm 4, an older emission never regresses the anchor
(`emitted_secs <= current`, `transport.rs:2707`), and adoption is
bounded by the sanity window: values above
`EMISSION_LEARN_CEILING_SECS` (`constants.rs:507`, 2200-01-01) cannot
come from a real clock and are refused outright
(`transport.rs:2703`); the lower bound `EMISSION_PLAUSIBLE_MIN_SECS`
(`constants.rs:525`, 2020) separates real wall clocks from
uptime-derived values, which sit orders of magnitude apart.
Incidents: #160, #161. Pinned at `transport.rs:15258` and `:15662`.
The per-announce no-backwards guard is not contradicted by the
healing loop: a median re-anchor is a deliberate, multi-sender event,
not one announce dragging the anchor down.

### The FIRST adoption is unbounded while the timebase is implausible

While the current anchor sits below the plausibility floor, adoption
is deliberately unbounded (`transport.rs:2718`): a node starting at
the build floor — or one that adopted a rebooting peer's uptime
seconds as its first anchor — must climb to real unix time in one
step. First-plausible-wins is correct *here*, and only here: an
implausible calendar has nothing worth defending, and instant
recovery beats attack resistance for it. **Capping this was a real
regression** (#161 §1): with the bounded advance applied to an
implausibly low anchor, recovery crawled at one day per announce —
about 20 602 announces, ~429 days at a 30-minute LoRa cadence —
where a single credible announce used to recover the node instantly.
Do not re-introduce that cap. The no-backwards guard above keeps the
unbounded branch from being abused downwards. Pinned at
`transport.rs:15445` and `:15486`.

### Advance after plausibility is bounded — per announce, not per peer

Once the anchor is plausible, one announce may advance it by at
most `EMISSION_LEARN_MAX_ADVANCE_SECS` (`constants.rs:539`, one
day), so a peer whose clock is decades wrong cannot capture the
calendar in one announce. State the protection level honestly:
learning runs before the per-destination announce rate limit and the
rebroadcast dedup, so *N announces advance the floor by N × cap*
regardless of how many identities or destinations they came from.
The real cap on the walk rate is announces-per-second on the air —
nothing identity-shaped. This measured reality is pinned at
`transport.rs:15588`; the walk terminates at the learn ceiling,
pinned at `transport.rs:15662`. The durable defence against the walk
is the median rule of the [healing loop](#the-healing-loop): a
calendar walked forward by one sender deviates from the median of
the others and gets pulled back.

### We do not validate our own clock, or incoming timestamps

Two deliberate non-behaviours, both reference parity. The anchor
model does not soften them — it validates *anchors* on the way into
the calendar, never emissions on the way out or announces on the way
into the path table:

- **Our own calendar estimate is emitted verbatim.** Python fills the
  field from `time.time()` unvalidated (`Destination.py:282`);
  bounding, substituting, or withholding our value at emission time
  would desynchronise us from a network that does not validate.
  Under the anchor model an "implausible own clock" collapses to "no
  anchor better than the build floor" — and that state emits too,
  per the stamping rule. The once-per-process operator warning
  (`Transport::announce_emission_secs`, `transport.rs:2641`) remains
  the only reaction to an implausible value — never an altered
  emission. Pinned at `transport.rs:15783` and `:15831`.
- **Incoming emission timestamps are not plausibility-checked on
  path acceptance.** Ordering is per-destination comparison only,
  exactly `announce_emitted > path_timebase`
  (`Transport.py:1772`/`:1809`). A clockless peer's uptime-seconds
  announce must enter the path table (that is how a #155 node is
  reachable at all), and an absurdly high emission must win the
  newer-emission comparison. Python peers accept both; filtering
  would only desynchronise our path tables from every other node's
  view of the same announces. Pinned at `transport.rs:15902`. The
  [ingress clamp](#ingress-clamp-for-semantics-keep-for-display)
  operates strictly above this layer — on what we index and serve,
  never on what we route.

The sanity window exists for *anchor adoption* — learning, host
injection, GNSS, RTC — never for emission or path acceptance.

## The healing loop

The protocol self-heals wherever it can; a wrong calendar is a
transient, not a fate. The loop, binding as spec:

1. **Collect.** Plausible foreign times are gathered from live
   traffic the node already receives — LXMF message stamps,
   propagation announces. Plausible means: passes the sanity window.
2. **Compare against the median.** When the own calendar deviates
   grossly from the **median of several distinct senders**, the
   calendar re-anchors to that median. Median, not
   first-neighbour-wins: a single attacker (or a single broken peer)
   cannot walk the calendar anywhere, because it takes a majority of
   the cohort to move the median. The cohort size and the
   gross-deviation threshold are practice parameters. Re-anchoring
   works in both directions; the forward walk is the attack the
   median exists to stop.
3. **Write back to the RTC.** Every better anchor is also written
   into a present RTC, so the hardware clock itself heals and the
   next boot starts from the healed value instead of the build
   floor. The write-back is what turns arm 3 from
   "whatever it was last set to" into "whatever we last verified".

The cold-start story then reads: a device with nothing stamps from
the build floor; the first plausible message, injection, or GNSS fix
pulls the calendar straight; the RTC (where present) keeps it across
power cycles; and a calendar later walked wrong is pulled back by
the median of its neighbourhood. No operator action at any step —
switch on and it works.

**Implementation status.** Arm 4's single-announce learning
(`learn_emission_timebase`, `transport.rs:2976`) implements the
first-adoption half today. The median re-anchor, the collection of
LXMF-stamp evidence, and the RTC write-back are spec, tracked as
implementation issues per platform.

## GNSS specifics for a board bring-up

- **Use NMEA UTC, never raw GPS time.** GPS system time does not
  observe leap seconds and is currently 18 s ahead of UTC. The
  receiver applies the broadcast UTC offset before it builds the RMC
  sentence, so RMC date + time *is* UTC — take it from there.
  Getting this wrong is silent: an 18-second skew breaks nothing
  visibly and is indistinguishable from clock drift in the field.
- **Acquire, seed, let the receiver sleep.** One fix seeds the
  calendar; the monotonic clock carries it forward. Crystal drift
  (tens of ppm — under half an hour per year of isolation) is
  irrelevant at one-second wire granularity. Keeping the receiver
  powered buys nothing for time.
- **Seed through the sanity window.** Route the fix through the same
  filter as every other source so a garbage fix cannot wedge the
  calendar; a valid RMC should pass it trivially. A subtly shifted
  fix inside the window is the accepted residual risk named above,
  time-bounded by the healing loop.
- **GNSS never replaces the ranking.** A node without sky view never
  gets a fix. Arms 2–5 must behave exactly as if no receiver were
  fitted.

## Record the source

A node should be able to state, at any moment, where its notion of
time came from: GNSS, host injection, platform clock, learned from
traffic (whose, and when), a median re-anchor (over which cohort), or
the build floor. Diagnosis of a path-ordering problem starts with
"what did this node think the time was, and who told it" — without
provenance, a wrong timestamp in a peer's path table cannot be
attributed to a dead RTC, a lying neighbour, or a boot-order race.
The healing loop raises the stakes: a re-anchor is a calendar jump,
and an unattributed jump is indistinguishable from a bug. Today the
only breadcrumb is the once-per-process implausible-own-clock warning
(`transport.rs:2646`); exposing the current source and its origin
(status RPC, control-channel query) is part of implementing this
concept on each platform.

## Practice parameters, not dogma

The model fixes mechanisms; these values tune them. Each is a
practice parameter: chosen to work, changed by measurement and a
reasoned commit, never load-bearing for the model itself.

- **The upper sanity margin** above the best known anchor — order of
  decades; today the fixed date in `EMISSION_LEARN_CEILING_SECS`
  (`constants.rs:507`).
- **The lower bound stand-in** `EMISSION_PLAUSIBLE_MIN_SECS`
  (`constants.rs:525`) until build-timestamp plumbing retires it.
- **The per-announce advance cap**
  `EMISSION_LEARN_MAX_ADVANCE_SECS` (`constants.rs:539`).
- **The healing cohort**: how many distinct senders form a median,
  and how large a deviation counts as gross.

## Checklist for a new firmware port

1. **Inventory the arms.** Which of the five can this platform
   offer? (GNSS receiver? an attached host? OS/RTC clock?)
2. **Implement `Clock::wall_unix_secs` only if the platform has a
   real wall clock.** Returning `None` is correct and engages the
   ranking. Never return an uptime-derived value from it.
3. **Plumb the build timestamp.** It is the sanity floor and the
   birth anchor; a port without it is still living in the raw-uptime
   state the model retires.
4. **Wire every available better source.** GNSS: seed from RMC UTC
   through the sanity window. Attached host: implement the
   control-channel frame that calls `set_wall_time_unix_secs`.
5. **Write healed anchors back to the RTC**, where one exists.
6. **Do not touch announce learning.** It comes with the core for
   free. Do not disable it, and do not "improve" it with local
   filtering of incoming timestamps — that is a semantic deviation
   from the reference (see the non-behaviours above).
7. **Never stamp cross-lifetime wire fields from `now_ms`.** New
   fields go through `Transport::emission_secs`.
8. **Expose the time source** for diagnosis (see
   [Record the source](#record-the-source)).
9. **Leave the pins green.** The tests cited throughout this
   document are the contract; a correct port never needs to change
   them.
