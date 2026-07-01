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

A partial implementation already exists in tree (`leviculum-core/src/framing/ble.rs`,
`leviculum-nrf/src/ble.rs`). It is incomplete and needs finishing.

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

## Combining the protocols

The broadcast and connection oriented protocols are not mutually exclusive. The
useful combination is broadcast for reach (announces, discovery, small packets
to everyone, no connection limit) and a connection for reliable directed bulk.
This maps onto Reticulum's own layering: announces are best effort broadcast,
links and resources are reliable and directed.

The constraint on how to combine them comes from the interface boundary. An
Interface is sent only bytes: `try_send(&[u8])` in
`leviculum-core/src/traits.rs` takes a packet buffer and a priority hint, no
destination and no next hop. The next hop and the choice of interface live one
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
| `ble-reticulum` | partial in core | partial | not yet | Columba | Columba "Protocol v2.2" |
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
- **Idle timeout governs connection management, not packet routing.** Closing
  idle connections to free slots is fine. Triggering a connection open from a
  per packet destination is not.

## See also

- [Interface isolation](interface-isolation.md)
- `docs/ble5-broadcast-protocol3-spike.md`, the ble-leviculum feasibility spike
- Columba discussion 880, the hybrid broadcast and connection proposal:
  https://github.com/torlando-tech/columba/discussions/880
