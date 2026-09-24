# Bluetooth interfaces

Reticulum uses Bluetooth in three distinct ways. They are not variants of one
interface, they are three separate carrier protocols with different connection
models, different peers, and different scaling behaviour. This chapter names
them, maps them to what Python-RNS and the Columba app call the same things,
and records the design decisions behind them.

The actionable status and open work for each lives on Codeberg, not here. This
chapter is the durable concept; the tracker is the source of truth for what is
done.

## The three protocols at a glance

| Name | What it is | Connection model | The peer is | Scales |
|------|------------|------------------|-------------|--------|
| RNode over BLE | Drive a dumb RNode radio over a transparent BLE link | Connection oriented (GATT) | a radio, not a node | n/a |
| `ble-reticulum` | A nearby device is a full Reticulum node, one link per peer | Connection oriented, one GATT link per peer | a Reticulum node | no, 3 to 4 reliable links |
| `ble-leviculum` | Reticulum broadcasts ride BLE 5 extended advertising | Connectionless | many nodes in range | yes |

### Naming and lineage

Two of these already exist in the wider ecosystem, so we adopt their names to
keep wire compatibility obvious. The third is our own invention.

| Our protocol | Python-RNS calls it | Columba calls it |
|--------------|---------------------|------------------|
| RNode over BLE | `BLEConnection` inside `RNodeInterface`, `ble://`, via `bleak` | `BluetoothLeConnection`, `RNodeInterface[BLE]` |
| `ble-reticulum` | no direct equivalent (closest is the new `WeaveInterface` / `WDCL`, but different) | the `ble-reticulum` Python package: `BLEInterface` plus `BLEPeerInterface`, wire spec "Protocol v2.2" |
| `ble-leviculum` | none, genuinely new | none |

`ble-leviculum` is a BLE 5 connectionless broadcast carrier for Reticulum
packets. It is leviculum originated and is not an upstream RNS standard. We
chose a name in our own namespace, not `ble5-reticulum`, on purpose: this
protocol interoperates with nobody yet, and the `*-reticulum` namespace is not
ours to reserve. The name itself marks it as ours, which sets it apart from the
two protocols above whose names we adopted because we must match their wire.

## The no_std layering

Every Bluetooth interface splits into two layers, and the split follows the
interface isolation rule (see [Interface isolation](interface-isolation.md)).

- **Carrier logic, no_std.** Framing, fragmentation and reassembly, the
  protocol state machine. This belongs in `leviculum-core`, which is no_std and
  already builds for `thumbv6m`. BLE framing already lives in
  `leviculum-core/src/framing/ble.rs`. Keeping the carrier logic no_std means
  the same code runs on the nRF firmware and on the host.
- **Platform binding.** The radio and OS specific glue. On nRF this is the
  SoftDevice glue in `leviculum-nrf` (no_std). On `lnsd` this is a Linux BLE
  stack, candidate `bluer` over BlueZ via DBus (necessarily std). On a phone it
  is the OS BLE API.

The goal is no_std carrier logic wherever possible so it runs on embedded
devices. One known exception: the RNode over BLE byte channel seam currently
lives in `leviculum-std` with tokio traits, so it is std only. A no_std variant
over `embedded-io-async` would be needed for on device use, tracked separately.

## RNode over BLE

BLE is used purely as a cable. The far end is an RNode radio that speaks the
RNode KISS protocol; leviculum still drives detection, configuration and the
radio lifecycle. The peer is not a Reticulum node.

The enabling work is a generic byte channel seam: drive the RNode lifecycle
over any duplex byte channel instead of a serial port path, so a process that
never sees a serial device (Android USB host, BLE GATT, iOS BLE) can still run
an RNode. Compatibility is unaffected, the serial path is unchanged and the
wire format does not change.

## ble-reticulum (BLE 4 link mesh)

Each nearby device is a full Reticulum node. A node opens one connection
oriented GATT link per peer, acting as both peripheral (GATT server) and
central (scan and connect). Columba implements this as the `ble-reticulum`
Python package with a `BLEInterface` for protocol handling and one
`BLEPeerInterface` per connected peer.

To interoperate with Columba we must match its wire spec, "Protocol v2.2": a
fixed service UUID `37145b00-442d-4a94-917f-8f42c5da28e3`, RX and TX and
Identity characteristics, and the connection handshake. The Identity
characteristic carries a stable Reticulum transport identity hash so peers can
be tracked across the BLE MAC address rotation that phones perform for privacy.

The hard limit of this protocol is the number of simultaneous links. Columba
caps at `MAX_CONNECTIONS = 7` and Android allows about 8 BLE connections total
across all apps; in practice 3 to 4 links are reliable. This protocol therefore
does not scale to a dense mesh, which is the motivation for `ble-leviculum`.

### Incoming link slots and the slot-contention policy

An LNode accepts three incoming (peripheral-role) links and initiates
one outgoing (central-role) link (#372); lnsd bounds both roles
together at `max_links = 4`. Three incoming slots dissolve the
single-slot field failure where two boards beside a phone paired with
each other first and the phone could only reach the board whose one
slot was still free: with slots to spare, a neighbour board and a
phone link to the same relay simultaneously.

The SoftDevice RAM cost of this configuration (`conn_count = 4`,
periph 3, central 1) is measured, not extrapolated: the S140 wants an
app RAM base of `0x20005DA0` (23 968 B), 928 B under the linked
ceiling in `leviculum-nrf/memory.x` (rig T114 `SD_RAM_FLOOR`,
2026-09-08). The margin is deliberately small — the requirement is a
fixed, deterministic property of this exact configuration and the
boot-time `SD_RAM_FLOOR` assert refuses any config that outgrows it.

There is deliberately NO preference of a phone over a board for the
last free slot. With more slots than nearby peers the policy would
decide nothing, and deciding it well needs information a connect-time
policy does not have (which peer carries traffic the mesh needs). It
becomes worth revisiting when a deployment has more adjacent boards
than a relay has slots — the boards can then occupy every slot before
a phone arrives, which is the single-slot failure again, one layer up.
The firmware keeps advertising while any slot is free and goes silent
when full, so a scanner not seeing the relay is the honest signal of
that state.

### Initiation direction, the fallback, cycles and duplicate links

Who initiates is the v2.2 address sort: the lower BLE address dials, the
higher one advertises and waits (with the v0.3.0 capability override for
peripheral-only peers). The sort alone does not reliably connect a room
of boards (#375): a full board stops advertising, so the highest
addresses can run out of permitted targets and sit scanning forever
while the sort forbids them to dial anyone lower. A Monte Carlo of
random arrival orders puts ten boards at a 21 % chance of a
disconnected BLE graph under the pure sort.

The fallback closes the stranded-board gap: a central task that has
scanned for `SCAN_FALLBACK_AFTER_MS` (30 s) without a single initiate
verdict, and without a connection event in either role, accepts any
advertising Reticulum peer (`BLE_SCAN_FALLBACK` marks the switch, once
per strict phase, and the verdict logs as `rule=initiate_fallback` in
`BLE_SCAN_DECISION`). Full boards do not advertise, so a fallback dial
only ever lands on a free slot. The rule stays pure and host tested in
`leviculum-nrf/ble-tx` (`should_initiate` with a `ScanMode` input);
the firmware measures the time and hands the mode in.

The clock is eager: it runs whenever the outgoing slot is free, live
links notwithstanding, so a board that holds links but keeps losing
the sort can still dial a third party and merge two components. Two
guards make that safe. A live connection's address is excluded before
it can leave the scanner (the registry's `addr_linked`): the Core Spec
permits one connection per address pair (Vol 6 Part B §4.5), so such a
dial could only time out — the rig once showed exactly that, a doomed
5 s dial at an already-linked peer every ~20 s, forever. And a
fallback target that does not even connect goes into a dead-end table
for two minutes (`BLE_DIAL_DEAD_END`, sized to the RPA rotation
timescale), so a vanished advertiser is not re-dialled every backoff.
The interim alternative — suspending the clock while any link is live
— was shipped briefly and measurably cost merges: 28 and 78 all-linked
splits per 1000 arrival orders at 10 and 20 boards, against eager's
zero, because a component whose boards all hold some link can never
initiate a cross-component dial.

Which eligible advertiser gets dialled is not first-heard-wins: the
scanner collects one bounded window (`BLE_SCAN_WINDOW`) and dials the
best eligible candidate, via a `CandidateTable` shared by the firmware,
lnsd and the simulation. Best is, in order: strict verdicts before
fallback verdicts, then the peer with the MOST free incoming slots,
then the lowest address. First-heard-wins is what produced the
saturated-cycle lock — fallback dials closing cycles inside their own
component until nobody scans — measured at 48 of 1000 arrival orders
for twenty boards; the window's choice removes it entirely.

The free-slot count is the middle term, and boards put it on the air
themselves: capability bits 1-2 of the v0.3.0 record carry how many of
the three incoming slots are still free, bit 3 says the count is
present at all (`leviculum-nrf/ble-tx/src/adv.rs`). Without it a
searching board picks blind between a peer with three free slots and
one with its last one free, so several searchers elect the same board
and all but one are refused. In the arrival-order simulation the
preference leaves connectivity untouched (0 of 1000 orders
disconnected at both sizes, as before) and halves the boards that end
saturated: 976 to 498 at ten boards, 2538 to 914 at twenty.

Three properties keep it honest:

- **It is a hint, never a permission.** Nothing in the duplicate or
  refusal path reads it. A board that advertised a free slot and has
  none by the time the connection lands refuses exactly as before.
- **It can understate, never overstate.** The advertisement is rebuilt
  at each advertising start, and an incoming link can only land on the
  board that is currently advertising — which then stops and lets the
  next free task advertise the new, lower count. A slot freed while
  another task is mid-advertisement stays unannounced until that
  advertisement resolves, so the air can lag behind a board that got
  emptier, never behind one that got fuller. No running advertisement
  is stopped to rewrite it, so no advertising interval is dropped.
- **Silence is not zero.** A peer that advertises no count — an older
  board, a phone, lnsd, another implementation — is ranked as if all
  its slots were free, the same "assume full capability" the v0.3.0
  §3.2 rule applies to the capability flags, so it keeps exactly its
  pre-#375 standing. Bit 3 exists precisely so that an older record
  with only bit 0 set cannot be read as "zero slots free".

lnsd reads the count but advertises none: its capacity is one budget
shared by both roles (`max_links`), not the boards' three dedicated
incoming slots, so the same bits would mean a different quantity.

Two consequences of dialling against the sort are deliberate:

- **Cycles in the BLE graph are harmless.** Reticulum treats every
  interface as a lossy broadcast domain and deduplicates packets at the
  transport, so a packet arriving over two paths costs one discarded
  duplicate, not a loop; announce rebroadcast is suppressed the same
  way. Connectivity is what the graph owes the mesh, minimal edge count
  is not.
- **A second link to an already linked identity is decided by the rule
  the PEER runs.** It adds no reachability, burns one of three incoming
  slots and the airtime of a connect, so both roles resolve the
  identity at connect — read from the Identity characteristic as
  central, presented in the handshake as peripheral — and then apply
  one rule, `judge_duplicate` in
  `leviculum-nrf/ble-tx/src/registry.rs`, which lnsd calls too. Three
  branches, and the `rule=` token on every duplicate line says which
  one fired:

  1. `rule=abandoned` — the old link has delivered NOTHING, payload and
     keepalives alike, for `LINK_ABANDONED_MS` (30 s, two keepalive
     intervals). Its peer has walked away from it; the newcomer wins
     (`BLE_LINK_REPLACED`). This is the one case the peer's own
     arbitration never sees, so nothing it decides can contradict us.
  2. `rule=same_role` — both connections carry the same role, which a
     rotated address makes possible in either direction (the pre-dial
     exclusion is address-keyed, this rule identity-keyed). The peer
     then holds both in one role and has no central-vs-peripheral
     choice to make, so we decide alone: our own second dial is refused
     (it reaches nothing the live link does not), the peer's second
     dial wins (a node that dials again is done with the connection it
     has).
  3. `rule=columba_mtu` / `rule=columba_identity` — the general case:
     `preferred_ble_role`, a port of Columba's own
     `preferredBleRole(centralMtu, peripheralMtu, localIdentity,
     peerIdentity)`, evaluated from the PEER's perspective. It keeps
     the role with the larger usable MTU and breaks a tie on identity
     order (`localIdentity < peerIdentity` keeps central), and we keep
     whichever connection it keeps. "Usable MTU" is the peer's own
     conversion, bounds included: `usableValueLength(rawAttMtu) =
     (rawAttMtu - 3).coerceIn(20, 512)`
     (`columba/rns-host/src/main/kotlin/network/columba/app/rns/host/ble/model/BleConstants.kt:87-88`).
     The
     ceiling is not cosmetic — at the top of the range ATT 517 and ATT
     515 both read 512, so a pair holding those two connections is a
     TIE the identity order decides; reading the 517 as 514 would
     decide it by MTU instead, and the two sides would keep different
     links.

  Whichever branch fires, the LOSING link is disconnected by us at the
  decision, in either role — never left to the expiry.

  Why the peer's function and not a rule of our own (#360 round 2): the
  only duplicate rule that never leaves the pair linkless is one both
  sides compute identically from the same inputs. On 2026-09-12 at
  21:41 the field T114 and a Pixel running Columba 2.2.4-beta each
  deduplicated in the OPPOSITE direction within one second. The board
  kept the newer connection because the old one's last payload was
  15 185 ms old; the phone kept its own central because its ledger
  still read the new connection's MTU at the floor
  (`centralPeerMtus[address] ?: BleConstants.MIN_USABLE_MTU`, so
  `MTU=20` although the ATT exchange had long settled at 517). Each
  side then closed the link the other had kept, and the phone's cancel
  did not even drop the ACL: our outgoing link stayed up for 45 s
  collecting `Rejecting pre-identity write` refusals until our own
  expiry. 45 s of dead air per rotation cycle — the same hole round 1
  closed from the other direction.

  What that capture showed at the floor was a FRESH connection, one the
  phone had not bookkept yet, and the rule is built on it being
  transient: our own dial enters the peer's comparison at
  `MIN_USABLE_MTU` and is refused pre-handshake, while every other
  number in the comparison is the ATT MTU we negotiated, assumed to be
  the number the peer holds too. #377 puts that assumption in doubt from
  the peripheral end: on the desk 2026-09-09 a Columba peer listed a
  link the BOARD had dialled at "MTU 20 bytes" long after the exchange
  had settled, i.e. a peripheral-role ledger stuck at the floor for the
  life of the link. If its arbitration reads that number, a pair whose
  two connections negotiated the same MTU is a tie to us — settled by
  identity order, keeping the link we dialled — and an MTU decision to
  the peer, keeping the link it dialled, and the pair ends up linkless
  again in the identity order where those differ. Unconfirmed on the
  device: #377 waits for the capture #376 is taking, and whether our
  rule should model a peer's bookkeeping bug is that issue's decision,
  not a silent change of input here. The arithmetic of the divergence is
  asserted in `a_peer_reading_its_peripheral_link_at_the_floor_decides_the_pair_the_other_way`
  (`leviculum-nrf/ble-tx/src/registry.rs`), so a confirmation has one
  place to land.

  Why the liveness test is any-frame and not payload: Columba sends a
  1-byte keepalive every 15 s on every connection it holds, so "any
  traffic within two intervals" is what distinguishes a link its peer
  still holds from one it has abandoned. Payload silence is what an
  IDLE phone looks like — it is not evidence of anything, and round 1
  reading it is precisely what displaced a link the phone was actively
  keepaliving. `old_data_silence_ms` is still measured and logged, so a
  round-1-vs-round-2 capture stays greppable, and consulted by nothing.

  A link that has really stopped answering is removed by the
  **expiry**, not by a replacement: `last_heard_ms` counts every
  inbound frame including the peer's keepalive, and a link that
  delivers neither payload nor keepalive for `LINK_TIMEOUT_MS` (45 s,
  three missed keepalives) is torn down by lnsd's `LinkTable::expire`
  and by the firmware session's `link_silent` arm — whether or not
  anybody dials the identity (`BLE_LINK_EXPIRE role=<r> slot=<n>
  conn=<h> silence_ms=<n>`). The replacement handles only the case the
  expiry is too slow for: the peer is here, on a new connection, asking
  to be reachable now. A REFUSED address goes into the dead-end table
  for its TTL so the scanner does not immediately re-offer it; a
  replaced link's queued packets move to the new link first.

  Every duplicate line carries `rule=`, `origin=` (the role map the
  arbitration is computed over), `old_mtu=`/`new_mtu=` (the usable MTUs
  as compared, in the peer's ledger), `old_silence_ms=` (the any-frame
  measurement the abandonment test reads) and `old_data_silence_ms=` —
  round 1's input, reported for comparison, `never` for a link that
  carried none.

  What the expiry bound itself replaced is a measurement (#382). Over
  14.1 h beside a Columba phone (`ble-accept-rns/lnsd.log`,
  2026-08-30) the gaps between received non-keepalive packets from a
  peer that was demonstrably present throughout ran to a median of
  51 s, a 90th percentile of 182 s and a maximum of 5590 s; 502 links
  outlived 45 s with no payload at all. Payload silence is what an
  idle phone looks like, which is why round 2 removed it from the rule
  entirely. The decisions are pinned by
  `a_live_old_links_own_dial_is_refused_before_the_handshake`,
  `an_abandoned_rotated_link_is_displaced_by_our_dial`,
  `payload_recency_no_longer_flips_the_verdict`,
  `keepalives_keep_a_link_out_of_the_abandoned_branch`,
  `a_same_role_pair_is_decided_without_the_peers_arbitration` and
  `preferred_ble_role_is_kotlinblebridge_44` in the registry, by the
  two-sided `registry::duel` scenarios that assert the pair is never
  linkless for longer than 2 s, and by
  `an_idle_link_is_alive_and_our_redundant_dial_is_refused`,
  `payload_recency_no_longer_flips_admission_in_either_role`,
  `our_dial_against_the_peers_live_central_link_is_refused` and
  `the_peers_second_dial_displaces_its_own_first_link`
  in `leviculum-std/src/interfaces/ble/links.rs`.

The simulation that motivated the fallback is a host test:
`leviculum-nrf/ble-tx/tests/graph_formation.rs` replays random arrival
orders through the real rule with the real slot limits. It reproduces
the issue's strict-rule disconnection rates exactly, asserts that no
fallback configuration ever leaves a board linkless, pins the quiet
suspension's measured cost as the record, and holds the shipped
configuration — eager clock, lowest-eligible window — to zero
disconnected orders at both sizes.

Three implementations exist in tree: the shared carrier logic
(`leviculum-core/src/framing/ble.rs`, plus the advertisement/decision
logic in `leviculum-nrf/ble-tx`), the firmware's dual-role
implementation (`leviculum-nrf/src/ble/`), and lnsd's dual-role BlueZ
interface (`leviculum-std/src/interfaces/ble/`, `type = BLEInterface`,
via `bluer`). The lnsd interface treats all live BLE links as one
broadcast domain behind one Reticulum interface — which is also the
only shape BlueZ supports on the peripheral side, where a GATT
notification reaches every subscribed central at once.

### Notifications are flow controlled, not fired and forgotten

The GATT server sends a packet as a sequence of notifications, one per
fragment. The SoftDevice queues those per connection, and the queue is one
entry deep by default (`BLE_GATTS_HVN_TX_QUEUE_SIZE_DEFAULT = 1` on S140).
The second `sd_ble_gatts_hvx` of a packet is therefore refused with
`NRF_ERROR_RESOURCES` until the first one has actually gone out over the air.

A loop that pushes every fragment back to back and ignores the return value
consequently delivers fragment 0 and drops the rest, silently, on both sides:
the peer sits on an assembly that never completes and the node believes it
transmitted. That is what the interface did until Codeberg #264, and it is why
only single-fragment traffic — anything below one fragment payload, 177 bytes
at the default MTU — ever arrived. An announce did not.

The rule that replaces it: **a fragment is offered again after the queue
drains, and a fragment that cannot be sent is reported, never discarded.**

- The wait is on the SoftDevice's own `BLE_GATTS_EVT_HVN_TX_COMPLETE`, not on
  a guessed interval. `nrf-softdevice` surfaces it as
  `gatt_server::Server::on_notify_tx_complete`, whose default implementation
  throws the event away and which the `#[gatt_server]` macro does not
  generate — so the server type implements `Server` by hand.
- The wait is bounded, so a peer that stops listening cannot wedge the
  outbound task. On expiry the packet is abandoned like any other failure.
- Every abandonment emits `BLE_TX_DROP` (see
  [Structured event logs](../structured-event-logs.md)) and bumps a counter.
  A dropped fragment is never again indistinguishable from a sent one.

The decision itself — retry, abort, report, and the exactly-once ordering —
is pure and lives in `leviculum-nrf/ble-tx`, unit-tested on the host against a
scripted notification sink; the firmware only performs the actions. This is
the same split as the GNSS and telemetry policies, for the same reason: the
interesting states are queue-full-then-drains, queue-full-then-times-out and
hard-error-mid-packet, and none of them are reachable on demand with a real
phone in the loop.

### Pacing on a link

Two packets whose fragments leave back to back on one connection can
cost the receiver the first packet: the 2026-09-09 desk measurement
(#376) showed a Columba phone one hop from two boards losing exactly
the first of two fragmented packets arriving back to back, on both
boards' links, reproducibly — and receiving both once the sender left
100 ms between the packets.

Every pump therefore serves a per-link **inter-packet gap**, measured
from the last fragment of one packet to the first fragment of the next
on the same connection: the compiled default is 100 ms
(`leviculum-ble-tx`'s `DEFAULT_TX_GAP_MS`). The number is the measured
value, not a derived one — one to two connection intervals (30 to
50 ms) may suffice but was not measured. The knob stays for
measurement: `lnflash --set-ble-tx-gap <ms>` overrides the default on a
running board (`0` disables the gap entirely), is never persisted, and
a reset restores the default. The first packet of a connection is never
deferred, an idle link pays nothing, and keepalives are neither paced
nor slide the window. Every actual wait logs
`BLE_TX_GAP conn=<h> waited_ms=<n>`. lnsd's Columba interface serves
the same default on its notify pipe and each central link — it is the
phone stand-in on the rig and must behave like a board toward a real
phone.

Related, from the same desk session: the peripheral pump **drains
nothing before the peer can receive**. The first notify on a fresh
connection, sent before the central had written the TX CCCD, fails
inside the SoftDevice (`sd_error code=13313`,
`BLE_ERROR_GATTS_SYS_ATTR_MISSING`) and the packet dies. The pump now
holds the drain until both the CCCD subscription and the identity
handshake have happened — packets queued before that wait, they are not
dropped — and logs `BLE_TX_HELD conn=<h> reason=not-subscribed` once
per connection when it actually held one. Policy host-tested in
`leviculum-ble-tx`'s `hold` module.

## ble-leviculum (BLE 5 broadcast mesh)

Reticulum broadcasts are sent as real BLE 5 connectionless extended
advertisements, so any number of devices in range form a mesh without per peer
links. This sidesteps the 3 to 4 link ceiling entirely.

To the core this is just another lossy broadcast medium, the same model LoRa
already uses, so the existing robustness logic applies. A BLE 5 broadcast
interface is a normal lossy broadcast Interface; the connectionless and size
limited nature is a carrier quirk handled inside the interface.

Feasibility was confirmed by a read only spike (see
`docs/ble5-broadcast-protocol3-spike.md` in the repository). Key results, valid
for `nrf-softdevice` rev 5949a5b and SoftDevice S140 v7.0.0:

- Connectionless extended advertising is supported, including the pure
  broadcast type `EXTENDED_NONCONNECTABLE_NONSCANNABLE_UNDIRECTED`.
- One advertisement carries at most 255 bytes, about 245 usable after framing.
- The receive side (extended advertising scan) is supported but gated behind
  the `ble-central` feature, currently off.
- Periodic advertising is absent in S140 7.0.0. It is optional; repeated
  extended advertising suffices for a broadcast mesh.

The consequence is fragmentation. Small packets such as announces fit in one
advertisement. A full 500 byte Reticulum packet (the MTU) does not and must be
fragmented across two advertisements and reassembled. Because broadcast is
lossy, a fragmented large packet only arrives if both fragments do; reliable
large transfers use links over a connection oriented path, not broadcast, so
this is acceptable.

### Coded PHY: automatic range extension, first-class

Coded PHY (S=2/S=8) is a first-class part of the `ble-leviculum` design, not an
optional add-on. The reason is the range and rate frontier: room scale BLE at
1M on one end, km scale LoRa at kbit/s on the other, and nothing in between.
Coded PHY at S=8 trades 1/8 rate for roughly 4x range and fills exactly that
empty middle, about 200 to 800 m at a ~100 kbit/s class rate.

The architecture keeps it automatic. 1M extended advertising is the universal
floor: every device transmits and scans it. Controllers that support LE Coded
(runtime feature detection: the LE Coded feature bit; on Android
`isLeCodedPhySupported()`) additionally dual advertise the same payload on a
coded primary advertising chain and scan both PHYs (`scan_phys = 1M | Coded`,
the controller time shares the scan windows). No user configuration, and no
parallel meshes: coded capable nodes bridge by construction because 1M always
stays on. Double reception of the same payload is absorbed by the normal
Reticulum packet hash dedup.

Costs to tune, stated here and not solved here. Coded TX is about 8x airtime
at S=8, so coded repeats are rate limited, for example one coded transmission
per N 1M intervals. Splitting the scan budget across two PHYs lengthens
discovery latency. Android background scan limits apply.

Capability is unevenly distributed, and the design accounts for that honestly.
Recent Android flagships largely support Coded PHY, often both scan and
advertise; midrange devices are mixed, sometimes scan only; iOS exposes no
Coded PHY at all; cheap BT5 USB dongles often omit it. The nRF52840 on our
boards and test dongles supports it fully. This asymmetry is why the design
makes coded an automatic bonus above the 1M floor, never a requirement.

## Combining the protocols

The broadcast and connection oriented protocols are not mutually exclusive. The
useful combination is broadcast for reach (announces, discovery, small packets
to everyone, no connection limit) and a connection for reliable directed bulk.
This maps onto Reticulum's own layering: announces are best effort broadcast,
links and resources are reliable and directed.

The constraint on how to combine them comes from the interface boundary. An
Interface is sent only bytes: `try_send(&[u8])`
(`leviculum-core/src/traits.rs:280`, plus a prioritized variant that adds a
priority hint) takes a packet buffer, no destination and no next hop. The next hop and the choice of interface live one
layer up in the transport. So an interface cannot decide "open a connection
because this packet is for node X" without reading the destination out of the
packet bytes, which is the link awareness the interface isolation rule forbids.
The Columba maintainer raised the same objection on the original proposal (see
the discussion linked below).

The clean way to combine them is therefore to keep the broadcast versus
connection decision in the transport, which is allowed to be path aware, and to
run two dumb interfaces rather than one clever one. Two staged options:

- **Stage A, broadcast only.** Run `ble-leviculum` alone. Reliability for large
  or important traffic comes from Reticulum's existing link and resource layers
  riding on top of the lossy broadcast, exactly as they already do over LoRa.
  The interface stays a pure `try_send(bytes)` broadcast pipe, fully isolated,
  unbounded in scale, with a minimal failure surface. Build this first and
  measure throughput.
- **Stage B, broadcast plus per peer connections.** If Stage A throughput is
  not enough, run `ble-leviculum` and `ble-reticulum` together. Model
  `ble-reticulum` as one ordinary byte only interface per connected peer (the
  Columba `BLEPeerInterface` shape): each GATT link is a normal interface that
  sends the bytes it is given over its one connection, and the transport routes
  over the set of interfaces normally. Which peers to connect is a neighbour and
  discovery policy with an idle timeout to free connection slots, not a per
  packet trigger. The hybrid benefit then emerges from running both planes at
  once and letting the transport choose, with no clever single interface and no
  change to the byte only interface boundary.

Power shapes the deployment. Continuous advertising and scanning is costly on
phones, so powered nodes (`lnsd`, stationary RTNodes) run the broadcast plane,
while phones connect sparingly over `ble-reticulum` to a nearby powered relay.

True on demand, opening a connection because a packet needs to reach node X,
belongs in the transport, which knows the next hop, via a control path beyond
`try_send`. That changes the media agnostic interface boundary and is deferred
until measurement shows Stage A and Stage B are not enough.

This analysis follows a proposal and debate in the Columba project, discussion
880, a hybrid broadcast and on demand connection model. The points above record
why a single hybrid interface is not the chosen path here.

## Capability matrix

| Protocol | no_std carrier | nRF | lnsd | Phone | Interop with |
|----------|----------------|-----|------|-------|--------------|
| RNode over BLE | seam is std today | planned | planned | n/a | Python-RNS, Columba |
| `ble-reticulum` | in core + `ble-tx` | yes | yes (`BLEInterface`, via `bluer`) | Columba | Columba "Protocol v2.2" |
| `ble-leviculum` | planned in core | feasible, spike done | via `bluer` | hardware dependent | leviculum only |

## Decisions

- **Broadcast instead of more BLE 4 links.** The connection oriented model caps
  at 3 to 4 reliable links, which does not scale to a dense mesh. BLE 5
  connectionless advertising removes the ceiling, hence `ble-leviculum`.
- **Fragmentation for full size packets.** One extended advertisement holds 255
  bytes on S140 7.0.0, the Reticulum MTU is 500, so the `ble-leviculum`
  interface fragments and reassembles. This is carrier logic, it lives in the
  interface, the core stays unaware.
- **Name in our own namespace.** `ble-leviculum`, not `ble5-reticulum`, because
  the protocol is our unilateral invention, interoperates with nobody yet, and
  the `*-reticulum` namespace is controlled upstream.
- **no_std carrier logic.** So the same protocol code runs on embedded nRF and
  on the host. Only the radio and OS binding is platform specific.
- **Combine by two interfaces plus transport, not one hybrid interface.** The
  interface boundary is bytes only, so a single interface that opened
  connections per destination would need link awareness, which the isolation
  rule forbids. Keep the broadcast versus connection choice in the transport.
- **Stage broadcast first, then measure.** Build `ble-leviculum` alone, let
  Reticulum's link and resource layers provide reliability over it, and only add
  per peer connections if measured throughput requires it.
- **Coded PHY is first class and automatic.** It fills the empty middle of the
  range and rate frontier between 1M BLE and LoRa. 1M stays the universal
  floor; coded capable nodes dual advertise and dual scan on top of it, so the
  mesh never partitions and no user configures anything.
- **Idle timeout governs connection management, not packet routing.** Closing
  idle connections to free slots is fine. Triggering a connection open from a
  per packet destination is not.

## See also

- [Interface isolation](interface-isolation.md)
- `docs/ble5-broadcast-protocol3-spike.md`, the ble-leviculum feasibility spike
- Columba discussion 880, the hybrid broadcast and connection proposal:
  https://github.com/torlando-tech/columba/discussions/880
