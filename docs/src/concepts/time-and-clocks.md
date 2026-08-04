# Time and Clocks

How a node obtains wall-clock time, and why it must. This applies to
every firmware and every platform port, present and future. Issues
come and go; this concept stays.

## The problem

A Reticulum peer orders same-destination paths by the announce
emission timestamp. Python-RNS stamps `int(time.time())` — epoch
seconds — into the announce random hash
(`reference/Reticulum/RNS/Destination.py:282`) and replaces a stored
path only when a new announce carries a *newer* emission than the
stored one (`announce_emitted > path_timebase`,
`reference/Reticulum/RNS/Transport.py:1772` and `:1809`). The value
we stamp is therefore compared on other machines, across our reboots,
against values our earlier selves emitted. A clock that is merely
self-consistent — process uptime — restarts from zero on every reboot
and loses that comparison forever: the node keeps announcing, and no
peer ever updates its path entry again (Codeberg #155). Obtaining
real time is a protocol obligation, not a platform convenience.
#155 is one instance of a general failure class — a generated field
that is self-consistent between our writer and our reader and means
something else to a peer; the class and the audit method for it are
in [Wire Field Semantics](wire-field-semantics.md).

## One value, one producer

`Transport::emission_secs` (`leviculum-core/src/transport.rs:2668`)
is the single point that turns whatever time the platform has into
the unix-seconds value for wire fields that peers compare across our
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

### Refuse a field the peer discards in silence

The producer carries no plausibility guarantee — on a clockless node
with nothing learned it is uptime seconds, and that is *correct*
behaviour for it (see the non-behaviours below: we never alter our own
emission). What changes with the field is whether emitting a known-bad
value is better than emitting nothing.

Split on what the peer does with it:

- **The peer discards the field, silently.** Refuse, with a named
  error. The LXMF ticket expiry is the case: a peer keeps a ticket
  only while `time.time() < expires` on its own clock
  (`reference/LXMF/LXMF/LXMRouter.py:1854`) and says nothing when it
  does not. `LxmfRouter::issue_ticket_field` returns
  `RouterError::NoWallClock` below
  [`EMISSION_PLAUSIBLE_MIN_SECS`](#the-timebase-never-moves-backwards-and-adoption-is-windowed)
  rather than issue one. A named error is a diagnosis; a discarded
  ticket is a mystery that surfaces months later as "replies from this
  peer are slow".
- **The peer decides nothing on it.** Emit it. The LXMF message
  timestamp (displayed and sorted, `LXMessage.py:357` unvalidated) and
  the propagation upload timestamp (bound and dropped,
  `LXMRouter.py:2238-2240`) both stay. Withholding a message because
  our clock is wrong is a far worse failure than a mis-sorted one.

The refusal is always on *writing*, never on reading: a peer's ticket
is remembered and used regardless of the state of our own clock.

## The source priority chain

`emission_secs` resolves its value in this order. For each source:
what it costs, when it is unavailable, what it guarantees.

### 1. Platform wall clock

`Clock::wall_unix_secs` (`leviculum-core/src/traits.rs:338`). On std
platforms `SystemClock` answers from `SystemTime`
(`leviculum-std/src/clock.rs:45`) — effectively the OS's NTP-managed
clock. When it answers, it always wins (`transport.rs:2603`).

- **Cost:** none.
- **Unavailable:** on MCUs without an RTC. The LNode's
  `EmbassyClock` (`leviculum-nrf/src/clock.rs`) keeps the trait
  default of `None` — that is correct, not a gap: returning
  uptime-derived values from `wall_unix_secs` would be lying to the
  transport.
- **Guarantees:** whatever the host clock guarantees, typically NTP
  quality. Note the trait contract: this is *not* a timer source;
  all timeout and deadline arithmetic stays on the monotonic
  `now_ms` (`traits.rs:323`).

### 2. GNSS

Where the board has a receiver — today the WisMesh Pocket V2's
u-blox ZOE-M8Q (`leviculum-nrf/src/gnss.rs`). The NMEA RMC sentence
carries UTC date and time in every fix.

- **Cost:** receiver power, a sky view, and cold-start acquisition
  time (seconds to minutes).
- **Unavailable:** indoors or shadowed — a node without sky view
  never gets a fix, so GNSS *seeds* the chain, it never replaces it.
- **Guarantees:** UTC to well under a second, far beyond the
  one-second wire granularity.
- **Status:** the firmware parses RMC but consumes only position and
  validity (`GnssFix`, `leviculum-nrf/src/baseboard.rs:26`, has no
  time field yet). Seeding the timebase from RMC is tracked as a
  firmware implementation issue, not here. See
  [GNSS specifics](#gnss-specifics-for-a-board-bring-up) below.

### 3. Host injection

`Node::set_wall_time_unix_secs` (`leviculum-core/src/node/mod.rs:662`
→ `transport.rs:2655`), for deployments where a clockless node has a
host that does know wall time — e.g. a control frame on the LNode
serial channel (the radio-config envelope of
`leviculum-core/src/rnode.rs`).

- **Cost:** one control-channel frame; requires a host that itself
  has a trustworthy clock.
- **Unavailable:** standalone nodes with no host attached.
- **Guarantees:** host-clock quality, but plausibility-gated: values
  outside `[EMISSION_PLAUSIBLE_MIN_SECS, EMISSION_LEARN_CEILING_SECS]`
  are refused (`transport.rs:2663`, pinned at `transport.rs:15548`
  and `:15655`), because an injection *claims* to know wall time, so
  a value no real clock can hold is self-refuting. A no-op in effect
  on platforms whose `wall_unix_secs` already answers.
- **Status:** the core API exists and is pinned by tests; no
  production caller wires it to the LNode control channel yet — a
  separate issue tracks that.

### 4. Learned from validated announces

The clockless fallback: `learn_emission_timebase`
(`transport.rs:2756`) adopts the highest emission timestamp seen in
any signature-valid announce as the node's timebase, then advances it
with the monotonic clock (`transport.rs:2608`). This includes the
node's *own* pre-restart announce echoing back from a neighbour —
learning deliberately runs before the own-destination echo drop, so a
rebooted node re-seeds past exactly the value its next announce must
exceed (pinned at `transport.rs:15355`).

- **Cost:** nothing — no hardware, no host.
- **Unavailable:** on a mesh where no participant has a clock, or
  before the first announce arrives.
- **Guarantees:** only as good as radio-range neighbours, and
  `validate()` proves only that the announce signs itself — the
  field is arbitrary input from anyone in range. Hence every
  hardening rule in the next section. The learned floor is in-memory
  only: after a reboot the node emits uptime seconds again until the
  first validated announce re-seeds it.

### 5. Uptime seconds

`now_ms / 1000` (`transport.rs:2610`). Monotonic within one boot,
loses every cross-restart comparison. This is the state every
clockless node is *born* in, and it is acceptable only as a
transient while the chain works upward. **A firmware that can offer
none of sources 1–4 and silently stays here is defective by
policy:** after every reboot it poisons its own entries in every
peer's path table (the #155 failure mode). If a port genuinely ends
here, it must say so — in its documentation and at boot — not ship
it as normal.

## Rules that hold regardless of source

### The wire field is 40 bits; every producer saturates

The announce timestamp field holds
`8 * RANDOM_HASH_TIMESTAMP_SIZE = 40` bits. A larger value would
silently drop its high bits on the wire and sort *below* every stored
path entry — the node instantly loses path replacement everywhere.
`EMISSION_TIMESTAMP_MAX_SECS`
(`leviculum-core/src/constants.rs:498`) caps it, enforced at the
point of resolution (`transport.rs:2618`) and again at the wire
producer (`announce.rs:167`), so truncation is unrepresentable
regardless of which source produced the value. Incident: Codeberg
#160. Pinned at `transport.rs:15746`.

### The timebase never moves backwards, and adoption is windowed

An older emission never regresses the floor (`emitted_secs <=
current`, `transport.rs:2698`). Adoption is bounded by a plausibility
window: values above `EMISSION_LEARN_CEILING_SECS`
(`constants.rs:507`, 2200-01-01) cannot come from a real clock and
are refused outright (`transport.rs:2694`); the lower bound
`EMISSION_PLAUSIBLE_MIN_SECS` (`constants.rs:525`, 2020) separates
real wall clocks from uptime-derived values, which sit orders of
magnitude apart. Incidents: #160, #161. Pinned at
`transport.rs:15258` and `:15662`.

### The FIRST adoption is unbounded while the timebase is implausible

While the current timebase sits below `EMISSION_PLAUSIBLE_MIN_SECS`,
adoption is deliberately unbounded (`transport.rs:2709`): a node
booting at uptime seconds — or one that adopted a rebooting peer's
uptime seconds as its first floor — must climb to real unix time in
one step. **Capping this was a real regression** (#161 §1): with the
bounded advance applied to an implausibly low floor, recovery
crawled at one day per announce — about 20 602 announces, ~429 days
at a 30-minute LoRa cadence — where a single credible announce used
to recover the node instantly. Do not re-introduce that cap. The
no-backwards guard above keeps the unbounded branch from being
abused downwards. Pinned at `transport.rs:15445` and `:15486`.

### Advance after plausibility is bounded — per announce, not per peer

Once the timebase is plausible, one announce may advance it by at
most `EMISSION_LEARN_MAX_ADVANCE_SECS` (`constants.rs:539`, one
day), so a peer whose clock is decades wrong cannot capture the
timebase in one announce. State the protection level honestly:
learning runs before the per-destination announce rate limit and the
rebroadcast dedup, so *N announces advance the floor by N × cap*
regardless of how many identities or destinations they came from.
The real cap on the walk rate is announces-per-second on the air —
nothing identity-shaped. This measured reality is pinned at
`transport.rs:15588`; the walk terminates at the learn ceiling,
pinned at `transport.rs:15662`.

### We do not validate our own clock, or incoming timestamps

Two deliberate non-behaviours, both reference parity:

- **Our own wall clock is emitted verbatim.** Python fills the field
  from `time.time()` unvalidated (`Destination.py:282`); bounding,
  substituting, or withholding our value would desynchronise us from
  a network that does not validate. The correct response to an
  implausible own clock (dead RTC, restored snapshot) is a
  once-per-process operator warning
  (`Transport::announce_emission_secs`, `transport.rs:2632`) — never
  an altered emission. Pinned at `transport.rs:15783` and `:15831`.
- **Incoming emission timestamps are not plausibility-checked on
  path acceptance.** Ordering is per-destination comparison only,
  exactly `announce_emitted > path_timebase`
  (`Transport.py:1772`/`:1809`). A clockless peer's uptime-seconds
  announce must enter the path table (that is how a #155 node is
  reachable at all), and an absurdly high emission must win the
  newer-emission comparison. Python peers accept both; filtering
  would only desynchronise our path tables from every other node's
  view of the same announces. Pinned at `transport.rs:15902`.

The plausibility window exists for *timebase learning* and *host
injection* only — never for emission or acceptance.

## GNSS specifics for a board bring-up

- **Use NMEA UTC, never raw GPS time.** GPS system time does not
  observe leap seconds and is currently 18 s ahead of UTC. The
  receiver applies the broadcast UTC offset before it builds the RMC
  sentence, so RMC date + time *is* UTC — take it from there.
  Getting this wrong is silent: an 18-second skew breaks nothing
  visibly and is indistinguishable from clock drift in the field.
- **Acquire, seed, let the receiver sleep.** One fix seeds the
  timebase; the monotonic clock carries it forward. Crystal drift
  (tens of ppm — under half an hour per year of isolation) is
  irrelevant at one-second wire granularity. Keeping the receiver
  powered buys nothing for time.
- **Seed through the plausibility-gated path.** Route the fix
  through the same window as host injection so a garbage fix cannot
  wedge the timebase; a valid RMC should pass it trivially.
- **GNSS never replaces the chain.** A node without sky view never
  gets a fix. Sources 3–5 must behave exactly as if no receiver were
  fitted.

## Record the source

A node should be able to state, at any moment, where its notion of
time came from: platform clock, GNSS, host injection, learned from an
announce (whose, and when), or uptime. Diagnosis of a path-ordering
problem starts with "what did this node think the time was, and who
told it" — without provenance, a wrong timestamp in a peer's path
table cannot be attributed to a dead RTC, a lying neighbour, or a
boot-order race. Today the only breadcrumb is the once-per-process
implausible-own-clock warning (`transport.rs:2637`); exposing the
current source and its origin (status RPC, control-channel query) is
part of implementing this concept on each platform.

## Checklist for a new firmware port

1. **Inventory the sources.** Which of the five can this platform
   offer? (OS/RTC clock? GNSS receiver? an attached host?)
2. **Implement `Clock::wall_unix_secs` only if the platform has a
   real wall clock.** Returning `None` is correct and engages the
   chain. Never return an uptime-derived value from it.
3. **Wire every available better source.** GNSS: seed from RMC UTC
   through the plausibility-gated path. Attached host: implement the
   control-channel frame that calls `set_wall_time_unix_secs`.
4. **Do not touch announce learning.** It comes with the core for
   free. Do not disable it, and do not "improve" it with local
   filtering of incoming timestamps — that is a semantic deviation
   from the reference (see the non-behaviours above).
5. **Never stamp cross-lifetime wire fields from `now_ms`.** New
   fields go through `Transport::emission_secs`.
6. **If the port ends at uptime seconds, declare the defect.** Boot
   log plus documentation; silence is the failure mode.
7. **Expose the time source** for diagnosis (see
   [Record the source](#record-the-source)).
8. **Leave the pins green.** The tests cited throughout this
   document are the contract; a correct port never needs to change
   them.
