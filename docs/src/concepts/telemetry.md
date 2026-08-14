# Telemetry

How a node reports where it is and how it is doing, and why almost none
of that format is ours to decide. This applies to every firmware and
every platform port, present and future.

## The goal

A node — a tracker in a rucksack, a solar relay on a roof, a handheld
with a display — produces readings that belong somewhere else: a
position, a battery charge, a link quality. Telemetry is the path from
that reading to a map pin or a database row, over the same mesh the node
already speaks.

The receiving end is not ours. Sideband and Columba display telemetry
today, from any peer, without knowing what produced it. A node that
emits the format they already read is useful on the day it ships; a node
that emits its own format is useful when someone writes a viewer for it.
So the format is adopted first and the node conforms to it, not the
reverse.

## Where the format comes from, and how far it is settled

Telemetry rides inside a normal LXMF message, in the `fields` dictionary
of the encrypted payload — not in a destination of its own, not in a
side protocol. It therefore inherits everything LXMF already provides:
end-to-end encryption, the three delivery methods, propagation nodes,
and receipts. A peer that does not know the field sees a message with no
text, which is the correct failure — subject to the empty-content rule
below.

The field numbers are LXMF's (`leviculum-lxmf/src/constants.rs:43-69`).
Three matter here: `FIELD_TELEMETRY` (0x02) carries one node's readings,
`FIELD_TELEMETRY_STREAM` (0x03) carries many nodes' readings collected
by a third party, and `FIELD_COMMANDS` (0x09) carries the request that
asks for the second one.

The *content* of `FIELD_TELEMETRY` is Sideband's, defined by its
`Telemeter` class in `sense.py`: a msgpack map from sensor ID to that
sensor's packed value. **Sideband is the origin of this format and, for
most of it, the only implementation.** Columba reimplements two sensor
IDs of the twenty-four — time and location — in `TelemeterCodec.kt`,
naming `sense.py` as its own reference. That is corroboration for those
two, not a second independent implementation of the format. For every
other sensor we encode, we are the second implementation, and the
accept-set rule of [Python-RNS
compatibility](python-rns-compatibility.md) applies with full force: we
must read what the origin *emits*, not only what we would have written.

How settled the format is differs by part, and the difference decides
how much freedom a reader has:

| Part | Status |
|------|--------|
| `FIELD_TELEMETRY`, time and location sensors | Two implementations agree. Settled. |
| Other sensor IDs | One implementation. Follow `sense.py` exactly. |
| The collector exchange (`FIELD_COMMANDS` / `FIELD_TELEMETRY_STREAM`) | Two implementations that **disagree today**. See the wire table below. |

> **Why citations to Sideband and Columba carry no line numbers.** This
> is a new rule, stated here rather than inherited: those repositories
> are not in this tree and not pinned by it, so a `path:line` citation
> into them cannot be checked by the citation guard and cannot be
> trusted to still point at what it claims. They are cited by symbol,
> which survives the edit that moves a line, plus the upstream commit
> the reading was taken from — Sideband `2000d81`, Columba `0930293` —
> so a reader can reconstruct the line. [Checks that are actually
> checks](checks-and-citations.md) argues the converse case for
> in-tree references and does not cover out-of-tree ones; this is the
> gap being filled, not an application of that page.

## Which viewer wins a disagreement

Columba is the app this project targets: it is the one whose users we
serve first and the one we can realistically patch. That ordering
decides what we *build for*. It does not decide what is correct on the
wire, and the two questions are answered in this order:

1. **Does one form break the other implementation?** Then that form is a
   bug, not a tie, however confidently it is implemented. Emit the form
   both accept and report the bug to whoever emits the other.
2. **Are both forms legitimate readings of the format?** Then prefer the
   one Columba displays.
3. **Would our output only be understood by a build carrying our own
   patch?** Then it is not permitted, whatever step 2 said.

Step 3 is the operative test, and it is the one that can be checked
before the change lands rather than after: run the output against an
unpatched build of both viewers. Three concrete temptations it rules
out, each of which would work and each of which would leave our node
broken against every implementation but the patched one — a receiver
taught to accept an unverifiable signature so a node can skip announcing
its delivery destination; a receiver taught to tolerate an implausible
timebase so a node can skip the clock chain; a receiver taught to
reinterpret a placeholder coordinate so a node can transmit without a
fix.

## Rules that hold regardless of platform

### Telemetry requires a wall clock that is not merely learned

`SID_TIME` and the location sensor's `last_update` are real UTC seconds,
and they are load-bearing at the receiver in three separate places:

- **Storage keys on them.** Sideband dedups on exact `(source, ts)`
  equality and otherwise inserts, so a wrong timestamp is stored rather
  than rejected — a node emitting uptime seconds appears in the viewer,
  dated 1970.
- **Collection filters on them.** A collector serves only rows strictly
  newer than the requester's stored cursor.
- **The cursor advances over every row seen, saved or not.** So one row
  timestamped in the future raises a requester's cursor past it
  permanently: every honest later reading from that collector is "not
  newer" forever, and nothing anywhere logs a refusal.

That third mechanism is why the rule is strict. [Time and
clocks](time-and-clocks.md) splits wire fields by what the peer does
with them — a field the peer discards in silence is refused at the
writer with a named error, a field it decides nothing on is emitted
regardless. Telemetry is in the first class, alongside the LXMF ticket
expiry that refuses with `NoWallClock`
(`leviculum-lxmf/src/router.rs:366`).

**The floor is higher here than for any other field.** The existing
predicate `has_plausible_wall_clock`
(`leviculum-core/src/node/mod.rs:2474`) tests only that the emission
timebase is above `EMISSION_PLAUSIBLE_MIN_SECS`
(`leviculum-core/src/constants.rs:525`), which a *learned* timebase —
arm 4 of the source chain, adopted from any signature-valid announce in
radio range — passes. For a ticket that is acceptable: a wrong expiry
costs one peer one ticket. For telemetry it is not: a learned timebase
that a neighbour walked into the future poisons the cursor of every
subscriber to that collector, and none of them can tell why. So
**telemetry requires arms 1 to 3 — a platform clock, GNSS, or host
injection — and a learned timebase is not sufficient.** A node
therefore has to know *which* arm produced its time, which is the
provenance requirement of [Time and clocks](time-and-clocks.md#record-the-source);
telemetry is the first feature that cannot be built without it.

### No fix, no position — and absence has one encoding

A sensor with no reading is absent from the map. It is never present
with a placeholder: no zero coordinates, no last-known value restamped
as current, no accuracy invented to fill the field. A viewer cannot
distinguish a placeholder from a measurement, and 0°/0° is a real place
in the Gulf of Guinea.

The format does not decide how absence is spelled, so we do. Sideband
packs every *active* sensor, and a sensor with no data packs as its SID
mapped to `None` — key present, value empty. That is two encodings of
absence in one format, and the choice is ours to fix:

- **We emit the omission.** A sensor without a reading contributes no
  key. `Telemeter.from_packed` instantiates only the sensors present in
  the map, so an omitted key round-trips cleanly.
- **We accept both on read.** A SID mapped to `None` is a sensor without
  a reading, not a malformed message. This is the accept-set half of
  [Python-RNS compatibility](python-rns-compatibility.md#level-1-wire-and-semantic-compatibility):
  the origin emits a form we would not write, and refusing it would be
  our defect.

### The encoder is multi-sensor from the first line

The unit is a Telemeter with *n* sensors, not a position packet with
extras bolted on later. Position, battery, temperature and link quality
are the same code path with different sensor IDs, and every board that
measures anything gets to report it without a second design.

The justification is the format's own extension mechanism, not our
future plans: a viewer ignores a sensor ID it does not know, so emitting
a reading no current viewer displays costs one map entry and breaks
nothing. A sensor left out of the encoder, by contrast, needs a new
design to add later.

### A reporting message carries no text

A telemetry message sets `content` and `title` to empty. This is a hard
wire requirement, not tidiness: Sideband suppresses the notification for
a telemetry-bearing message only when both are empty, so a reporting
node that fills in either one notifies its recipient once per reporting
interval, forever, and the feature is indistinguishable from spam.

### Delivery is opportunistic unless a port shows otherwise

Of LXMF's three methods, a reporting node uses the opportunistic single
packet by default. A direct delivery pays a link setup — three
round trips before any payload — for a payload of roughly fifty bytes,
and a propagated delivery pays a node round trip and adds a delay that
makes a position stale. A port that needs delivery confirmation, or that
reports to a target it can only reach through a propagation node, may
choose differently, and states why.

### Cadence is policy; airtime is the interface's business

A telemetry producer states *when it wants to report*: a minimum
interval, a minimum distance moved, a maximum interval as a heartbeat so
that "stationary" stays distinguishable from "dead", an accuracy
threshold, and a settle time so a cold start does not spend the channel
on a drifting first fix.

It does not state *when the radio may transmit*, and it holds no airtime
figure of its own. Duty cycle, spacing and back-off belong to the
interface — see [Interface isolation](interface-isolation.md) and
[Regulatory airtime](regulatory-airtime.md), which also settles how any
such figure is to be described: modelled, and a floor rather than a
total, because the board's own meter clears when it is read.

### Fan-out is the expensive shape; collection is the cheaper one

Telemetry to *n* recipients is *n* individually addressed and
individually encrypted messages. Mutual reporting in a group of *n* is
therefore *n*(*n*−1) messages per interval — thirty a minute for six
people at a one-minute cadence.

Routing the same group through a collector is *n* reports plus, for
those who want to see the others, *n* request/response pairs: eighteen
rather than thirty for the same six. The saving is real but it is
roughly a third, not fivefold, and it is not free in kind — a stream
response carrying several sources will exceed one packet and become a
link plus a resource transfer, where a report is a single packet. Both
figures belong in a port's own arithmetic; neither is a licence to skip
it.

There is no third shape. Reticulum has no multi-hop broadcast at all —
see [Public channels over
LXMF](public-channels.md#1-why-the-obvious-approach-does-not-work), which
settles this at the protocol level rather than by what two apps happen to
implement. LXMF's `FIELD_GROUP` is conversation metadata and not a
delivery mechanism; it would not produce a broadcast even if every
viewer read it.

### Relaying someone else's readings needs permission

A collector redistributes positions of people who are not asking for
that redistribution. It therefore answers requests only from an explicit
allow-list, empty by default, set only through a path that is not the
radio: on a host that is the config file, as `remote_management_allowed`
is (`leviculum-std/src/config.rs:95`, empty by default at
`leviculum-std/src/config.rs:213`); on a firmware with no filesystem it
is the local control channel, which is the same requirement in a
different envelope.

The same applies to precision. Where a deployment wants a node's
position blurred, the blurring happens at the producer, before the
reading is packed — a receiver's promise to round a coordinate is not
privacy.

### A port may ship send-only

Emitting telemetry needs an encoder, a target and a cadence. Consuming
it needs the inbound LXMF path, a peer table and something to show. A
port may implement the first without the second, and nothing in this
document may be read as requiring both: a tracker that cannot receive is
a complete node for its purpose.

## The collector exchange

Two implementations exist and they disagree, so this table is normative
for us rather than descriptive of them.

| Element | Form | Note |
|---------|------|------|
| Request | `FIELD_COMMANDS` = list of single-entry maps | Sideband's `Commands.TELEMETRY_REQUEST` is key `0x01` |
| Request argument | `[epoch_seconds, is_collector_request]` | An **absolute UTC epoch**, not an interval or an age |
| Legacy request | `{0x01: epoch_seconds}` — bare scalar | Must be **accepted** on read; the origin still emits it and infers `is_collector_request = true` |
| Response | `FIELD_TELEMETRY_STREAM` = list of rows | |
| Row | `[source_hash, timestamp_seconds, packed_telemetry, appearance]` | **Always four elements**, `None` in the fourth when there is no appearance |

The four-element rule is the one that costs something. Sideband indexes
the fourth element unconditionally, so a three-element row raises inside
its ingest and aborts processing of the **whole message**, not just that
row — while Columba's native collector emits three elements when
appearance is absent, and its own Python-backed collector emits four.
Under the ordering above this is step 1, not step 2: one form breaks a
conforming receiver, so we emit four and the three-element form is a bug
to report.

A collector additionally never serves a row whose timestamp lies in the
future relative to its own clock. Serving one permanently starves the
cursor of every requester that sees it.

## What a reporting node owes

- **An announced delivery destination.** A receiver verifies the LXMF
  signature against the sender's public key, which it can only have from
  an announce. A node that reports announces its delivery destination,
  with a display name in the announce data — otherwise the reading is
  unverifiable and the pin, if it appears at all, is a hex string.
- **The target's public key, before anything else.** Encrypting to a
  destination is impossible without it. There is no broadcast around
  this: the reference's transmit-on-all-interfaces branch
  (`reference/Reticulum/RNS/Transport.py:1177-1182`, our equivalent
  `send_on_all_interfaces`, `leviculum-core/src/transport.rs:2439`)
  applies to a packet that already exists, and building one required the
  key. So a port either preconfigures the target identity or waits until
  it has heard the target announce.
- **A path, or a request for one.** `send_to_destination`
  (`leviculum-core/src/transport.rs:2489`) fails without a path entry.
  The primitive for obtaining one is `request_path`
  (`leviculum-core/src/node/mod.rs:2362`); a node with the key but no
  path asks and waits rather than giving up.
- **An out-of-band trust step at the receiver, in the operator's hands.**
  Sideband can be configured to ingest telemetry only from trusted
  peers, and both viewers gate *collector requests* on an explicit
  allow-list. When that step has not been taken, a correctly reporting
  node is silently invisible, and it looks exactly like packet loss.
  Documentation of a reporting port says so; a diagnostic that cannot
  distinguish the two cases is worth building before the third support
  question arrives.

## The extension ladder

The format is fixed by implementations we do not control, so extending
it is a cost with a blast radius, not a design choice. Every addition
climbs from the bottom and stops at the first rung that works.

1. **An existing sensor ID already carries it.** Sideband defines
   twenty-four, well beyond position: battery, temperature, pressure,
   physical link, power production and consumption, and free-text
   information. A reading that fits one of them is not an extension, and
   emitting it costs nothing even where no viewer shows it yet.
2. **`FIELD_CUSTOM_META` (0xFD) carries it — under someone else's
   keys.** This slot is *not* free. LXMF reserves it for private use,
   and Columba has claimed it: it carries an unnamespaced map with the
   keys `cease`, `expires`, `approxRadius` and `ts`, and a truthy
   `cease` makes Columba **delete the sender's whole track**. There is
   one value per message. So a node targeting Columba may emit
   *Columba's* semantics here — a bounded sharing session that expires
   itself is the case that earns it — and may not put its own
   vocabulary in the same map. An extension of our own does not go on
   this rung.
3. **A new field number is genuinely required.** Then it belongs
   upstream in LXMF, proposed as such, and not shipped into one client
   ahead of that. A field number minted by us and understood by one app
   is a fork of the format with a friendlier name.

Two conditions apply at every rung. An extension must be **demonstrated
between two implementations that are both ours** before it is offered to
anyone else — what gets proposed is then a working feature rather than
an idea, and the cost of being wrong stays inside this project. And the
**failure mode on a viewer that does not participate must be written
down**: an extension whose effect on an old build has not been stated
has not been designed.

## Non-goals

- **A second format.** Not even for our own daemon-to-daemon path: one
  encoder, one decoder, one thing to get right.
- **Mirroring a viewer's internals.** Session bookkeeping and collector
  scheduling are each app's business. We match the wire, not the state
  machine — the same distinction [Python-RNS
  compatibility](python-rns-compatibility.md) draws between
  compatibility and parity.
- **Telemetry as a transport diagnostic.** What a node reports about
  itself is not how the mesh is measured; that is periculum's job and
  the status surfaces'.

## Checklist for a port, or for a new sensor

1. **Can this platform produce time from arm 1, 2 or 3 of [Time and
   clocks](time-and-clocks.md), and can it say which?** If not,
   telemetry does not ship on this port. The learned arm does not
   qualify.
2. **Does every sensor have a "no reading" state that omits its key?**
3. **Does an existing sensor ID fit?** Climb the ladder from rung one.
4. **Is the cadence stated as policy alone, with no airtime figure
   inside the telemetry module?**
5. **Are `content` and `title` empty on every reporting message?**
6. **Does the node announce a delivery destination with a name, and does
   it have a defined answer for a target it has never heard?**
7. **If it collects for others: allow-list empty by default, set off the
   radio, four-element rows, no future timestamps served?**
8. **What does a viewer that lacks this sensor show?** Answer before
   emitting.
