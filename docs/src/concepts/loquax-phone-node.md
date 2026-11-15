# Loquax on a phone as a full node

Status: concept, nothing built (1194, Lew 2026-10-31, low priority; Refs
#232, #471). Lew's rule: Loquax on a phone must be able to run as a
complete Reticulum node of its own, with no mailbox, on every medium the
phone has, and also as a mailbox client, the mode it has today. The two
must live side by side. This is not urgent. The page exists so that every
change to the interface seam keeps the way to the full node open.

## 1. The two modes and what each needs

| | Mailbox client (today) | Full node (this page) |
|---|---|---|
| Engine role | `Role::Client` (`loquax/src/engine/shell.rs:157`) | `Role::Messenger`, as on a computer |
| Destinations on the phone | none | the LXMF delivery destination of the user's identity |
| Interfaces | one TCP client to the mailbox, from the rendezvous list | TCP, WiFi AutoInterface, BLE, USB serial to an RNode, later LoRa through a board, each opt-in |
| Where messages wait while the phone sleeps | in the mailbox's archive | at a propagation node, as for any LXMF peer |

**The client has no destination of its own.** `Engine::client`
(`loquax/src/engine.rs:399`) says it "registers nothing, by
construction", and a guard test, `a_client_engine_registers_no_delivery_destination`,
holds that. The session attaches through `attach_leaf`
(`loquax-android/src/session.rs:574`) with `Role::Client`. That node runs
with transport off and has no interface from the config: the rendezvous
rotation dials one TCP client at a time (`Upstream::Tcp`,
`loquax/src/engine/shell.rs:838`). The only destination it talks to is the
mailbox's `loquax.home`, which the client reaches over links that it
opens itself (`loquax/src/home/client.rs`, `loquax/src/home/watch.rs`).
Nothing under `loquax/src/home/` registers a destination on the client's
side. The one `register_destination` sits in the attach path and runs
only for the home role (`loquax/src/engine/shell.rs:952`).

**The full node needs nothing new above the seam.** The phone already
runs the messenger engine in one case: the mailbox on the phone
(`loquax-android/src/mailbox.rs`) is `loquax --daemon`'s home node inside
the app, with a node of its own that dials the entry servers
(`DaemonUpstream::Mesh`, `loquax/src/home/daemon.rs:197`). So the full
node is the engine the phone's mailbox already uses, without
`loquax.home`, on a node whose interface list holds more than TCP.

**The two coexist the way the mailbox on the phone already does.** One
process may run a client session and a node of its own side by side,
each with its own directory and store. There are two shapes:

- **Full node only.** The screens talk to the messenger engine directly,
  and no mailbox is involved.
- **Full node and a remote mailbox.** The node's local media serve the
  people in the room. The mailbox keeps the archive and receives while
  the phone sleeps. The client session reaches it over any interface the
  node has, because a path to `loquax.home` does not care which medium
  carries it.

Section 5 leaves open which of the two shapes is the default. Which of
the node's media carries the client's link to the mailbox, and how the
most direct one is kept, is [Reaching the mailbox from anywhere](loquax-reachability.md):
each medium this page adds (WiFi AutoInterface, BLE) becomes a rung of
that ladder.

## 2. One interface per medium, behind the host seam

The seam is the core's `Interface` trait
(`leviculum-core/src/traits.rs:323`) as the std driver implements it,
one tokio task per interface. The rule is
[Interface isolation](interface-isolation.md): only the interface knows
its medium. For the phone that means the following:

- Locks, scan duty cycles and permission state live in the interface and
  its thin Java shim.
- An "on battery" or "in Doze" profile reaches the interface as
  configuration, never as a flag in transport, the core or the engine.
- Java owns the Android objects, the permission dialogs and the system
  callbacks. Rust owns everything that decides. Bytes cross JNI in both
  directions and carry no meaning on the way.

| Medium | Android API | Thread or callback owner | Standby cost |
|---|---|---|---|
| TCP | `INTERNET`, already declared (`loquax-android/android/AndroidManifest.xml:11`); Rust sockets work directly | the existing TCP interface's tokio task, unchanged | keepalive probes; the levers in [tier P](loquax-phone-power.md), `tcp_keepalive_idle_secs` (`leviculum-std/src/interfaces/tcp.rs:78`) among them |
| WiFi AutoInterface | IPv6 link-local multicast, `WifiManager.MulticastLock` (`CHANGE_WIFI_MULTICAST_STATE`); NICs found by `enumerate_nics` (`leviculum-std/src/interfaces/auto_interface/mod.rs:171`), group joined by `join_multicast_v6` (`leviculum-std/src/interfaces/auto_interface/mod.rs:429`) | the AutoInterface's own task; Java only acquires and releases the lock when the interface asks for it, through a handle | high. It announces every `ANNOUNCE_INTERVAL_SECS` (1.6 s, `leviculum-std/src/interfaces/auto_interface/mod.rs:32` from `ANNOUNCE_INTERVAL_MS` (`leviculum-core/src/auto_wire.rs:37`), the reference's `AutoInterface.py:62`), and while the lock is held the WiFi multicast filter is off, so any LAN multicast can wake the device (assumption) |
| BLE, central and peripheral | `BluetoothLeScanner`, `BluetoothLeAdvertiser`, `BluetoothGattServer`; `BLUETOOTH_SCAN`, `BLUETOOTH_ADVERTISE`, `BLUETOOTH_CONNECT` (Android 12 and later) | the binder threads deliver GATT callbacks to Java, which hands each event over JNI into a channel. A Rust orchestrator task owns all link state, as on the host | scanning dominates; the levers are the scan mode (`SCAN_MODE_LOW_POWER` against `LOW_LATENCY`) and stopping scans once enough peers are linked (assumption, to be measured) |
| USB OTG serial to an RNode | `UsbManager`, a device filter, the host permission dialog, a USB serial driver for CDC-ACM and the common bridges | Java opens the device and hands it over. Rust reads and writes it, either through the file descriptor from `UsbDeviceConnection.getFileDescriptor()` or through a JNI byte pipe around the bulk endpoints when the bridge needs a control setup | the RNode draws from the phone during continuous LoRa receive (assumption: tens of mA), and the held device keeps the USB host controller awake |

What already fits the seam:

- **USB serial needs no new interface.** `RNodeChannelFactory`
  (`leviculum-std/src/interfaces/rnode.rs:3628`) exists for exactly this
  case: a host that never sees `/dev/ttyACM*`, Android named. The phone
  supplies `open`. The KISS detection and configuration, the airtime
  bucket and the reconnect loop stay the host RNode interface's own, and
  `spawn_rnode_channel_interface` (`leviculum-std/src/driver/mod.rs:3194`)
  hot-plugs the radio when the cable goes in.
- **BLE needs a second carrier binding, not a second interface.** The
  host BLE interface is split into a pure half, `links` (admission,
  framing state, keepalive and expiry, fan-out), and a carrier binding,
  `bluez` (`leviculum-std/src/interfaces/ble/mod.rs`). Today the module
  only builds on Linux with the `ble` feature
  (`leviculum-std/src/interfaces/mod.rs:17-18`). The phone binding
  replaces `bluez` with JNI events. Before that can happen, the
  orchestrator has to stop naming `bluer` types: the module imports
  `bluer::Address` and `CharacteristicNotifier` today. That cut is the
  first piece of work for BLE.

Nothing medium-aware may sit above the seam. A phone that sleeps is a
reason to configure an interface, not a reason for a branch in transport.

## 3. What the Columba protocol already covers

The board's `columba.rs` (`leviculum-nrf/src/ble/columba.rs`) holds
everything a Columba peer expects to see on the air:

- the GATT service and its three characteristics under the `37145b00-…`
  UUID space,
- the 16-byte identity handshake,
- the 1-byte keepalive,
- the advertisement: the service UUID, the `LN-<hex8>` name and the
  v0.3.0 capability record,
- the address-sort rule that decides which side initiates.

The decisions under all of this are host-tested in `leviculum-nrf/ble-tx`:
admission, `should_initiate` (`leviculum-nrf/ble-tx/src/peer.rs:321`)
with the scan fallback, `dial_preference`
(`leviculum-nrf/ble-tx/src/window.rs:312`), the dead-end and backoff
tables, and advertisement parsing. Fragmentation lives in
`leviculum-core/src/framing/ble.rs`. The host BLE interface already
drives both halves from `links`
([Bluetooth interfaces](bluetooth-interfaces.md)).

A phone peripheral would reuse:

- the GATT layout and the identity characteristic, served by
  `BluetoothGattServer`,
- the handshake, the keepalive and the framing, unchanged,
- the advertisement records, through `BluetoothLeAdvertiser`,
- the dial rule, the fallback and the dead-end table, as the decision
  logic of the central half.

Two things differ on a phone:

- **Android rotates its own address** and does not give the app that
  address. So the address sort needs the side's identity hash as its key,
  or the capability override. The identity characteristic exists for
  exactly this tracking across rotation.
- **Android shares about eight connections across all apps**, and 3 to 4
  of them are reliable ([Bluetooth interfaces](bluetooth-interfaces.md),
  "ble-reticulum"). The link table's admission must take that ceiling as
  configuration.

## 4. The order of work

Each step lands only when two things are green:

- its acceptance, a device-walk step ([the device walk](loquax-device-walk.md))
  or a conformance run against a real peer,
- its power check, mode `A` of [tier P](loquax-phone-power.md) with the
  medium named and, before the medium becomes a default, tier R (§3 of
  the research note) against the baseline.

Tier P's mode `A` fails at setup today, because the app starts no
AutoInterface. That is the first red to turn green.

| # | Medium | What it takes | Acceptance | Power |
|---|---|---|---|---|
| 0 | TCP | exists, as a leaf | the walk's existing steps | tier P mode `B` and `Bprime` |
| 1 | the full node over TCP | `Role::Messenger` on the phone with a delivery destination, beside the client session; a start choice | walk step: a peer on hamster sends straight to the phone's own address with no mailbox and the bubble arrives; negative: with the node off, nothing arrives | tier P mode `A` over TCP only, reported over `Bprime` |
| 2 | WiFi AutoInterface | the lock handle, interface names on Android | walk step on Waydroid's bridge (multicast there is an assumption, tier P checks it), then on a phone against lnsd and Python `rnsd` on the same LAN; negative: lock released, no peer | tier P mode `A`; tier R on a phone, WiFi series, lock held against released |
| 3 | BLE | `bluer` out of the orchestrator, the JNI binding, the identity-keyed sort | conformance against an LNode board and against Columba on the second phone, both roles; negative: Bluetooth off, the link drops and the interface reports offline | tier R only (Waydroid has no BLE): scan mode `LOW_POWER` against `LOW_LATENCY`, with and without linked peers |
| 4 | USB serial to an RNode | `RNodeChannelFactory` over the Android USB device | walk step on a phone with a rig RNode at the default PHY (869.463 MHz, BW 125 kHz, SF8, CR 5): a board across the room receives; negative: cable out, interface offline, cable in, it comes back on its own | tier R, phone current with the RNode attached and receiving, against without |
| 5 | LoRa through a board | the board as a radio for the phone, over USB first (the same `RNodeChannelFactory`, an LNode in RNode mode) and later over BLE | as step 4 with an LNode | as step 4 |

Each medium stays opt-in until its tier R number is known. Columba takes
the same approach for AutoInterface, on WiFi only.

## 5. What this page does not decide

For Lew:

1. **Whether the full node is a transport node.** A phone that forwards
   for others spends battery on other people's traffic. The node runs
   with transport off today, as every leaf does.
2. **The default shape on first start.** Full node only, mailbox client
   only (today's default), or both together.
3. **The power budget per medium.** Tier P and tier R measure. The
   research note's 1.7 and 4 mAh/h thresholds were set for the mailbox
   client's wake, not for a node's media.
4. **Whether the phone's own identity in the full node is the mailbox
   address or a second identity.** Columba keeps two identities, one for
   messages and one for transport.
5. **Whether a full node also keeps a propagation node's role** for the
   people it meets, which the [propagation node page](propagation-node-on-a-board.md)
   decides for boards only.
6. **Which USB serial driver to take**, a Java library or a
   Rust driver for the bulk endpoints, once the bridges in use are known.
