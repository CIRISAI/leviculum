# The BLE link gap swept on a relayed transfer, btvirt (#379)

**Date:** 2026-10-12.
**Question:** what does a relayed 64 KiB copy cost as the BLE pacer gap
goes from the default 100 ms down to 0, and where, if anywhere, do frames
start to be lost or the defragmenter start to abandon?
**Context:** the eight-hour soak of 2026-10-11
(`/home/lew/ci/ble-soak-state/2026-10-11/logs/run.log`) copied 64 KiB over
two hops in 15 277 to 15 632 ms with `BLE_TX_GAP waited_ms=86..100` on
every data frame: the gap is the throughput. The two-node grid of
2026-10-10 ([2026-10-10-ble-link-gap-floor.md](2026-10-10-ble-link-gap-floor.md))
found no loss down to 0 ms on broadcasts; this run adds a relay and a
resource transfer.

## The knob

`tx_gap_ms` under a `BLEInterface` (ec387295): parsed at
`leviculum-std/src/ini_config.rs:909`, carried as
`InterfaceConfig::tx_gap_ms` (`leviculum-std/src/config.rs:879`), resolved
by `effective_tx_gap_ms` (`leviculum-nrf/ble-tx/src/gap.rs`) in both of
lnsd's pumps: the shared notify pacer (`interfaces/ble/mod.rs:524`, served
at `:1458-1475`) and each central link's writer
(`interfaces/ble/bluez.rs:559`). Unset is 100 ms, `0` disables it; the
bring-up line prints the gap in force. Documented in
`docs/src/guide/configuration.md`. The default is unchanged.

## Instrument

periculum `regression/ble_room_transfer_relayed_gap0.toml`, the soak's
transfer step (three lnsd pinned into the chain 0 - 1 - 2, `lncp` 64 KiB
from node 0 to node 2) with the new room key `tx_gap_ms` (periculum
eb2c760, 735a244). The room writes the key under every node and reds a
node whose bring-up line prints another gap (`BLE_ROOM_TX_GAP node=
written= served=`, all 66 node readings matched). Each setting ran with
`seeds = [1, 2, 3]`, three rooms of one copy each. btvirt is bluez 5.82
+ 4ff7deaf8c + adv-every-interval, md5 bbcd9427; lnsd is release, tree
61010976. `transfer_ms` is lncp's wall time polled every 50 ms; the rate
is bytes per second (the soak's "4.2" is 4.2 kB/s, 34 kbit/s).

## The table

Every room: hops 2, file intact; census
(`frags/packets@mtu/abandoned/bad/tx_drops`) node 0 31-34/31-34@512,
node 1 178-184/178-184@512, node 2 147-150/147-150@517, all `/0/0/0`;
142 parts of 483 bytes at nodes 1 and 2. Counted over each whole room
log: `BLE_RX_ABANDON` 0, `bad fragment` 0, `BLE_TX_FANOUT_DROP` 0,
`BLE_LINK_DOWN` 0, `RESOURCE_REQ_AGAIN` 0, `RESOURCE_PART_REJECT` 0.

| tx_gap_ms | transfer_ms (3 rooms) | rate B/s | BLE_TX_GAP lines per room log |
|---|---|---|---|
| 100 | 15250, 15433, 15380 | 4246-4297 | 160-170 |
| 50 | 7965, 7962, 7913 | 8227-8282 | 168-185 |
| 25 | 4106, 4107, 4157 | 15765-15961 | 154-165 |
| 10 | 2253, 2253, 2304 | 28444-29088 | 121-123 |
| 5 | 1703, 1702, 1652 | 38482-39670 | 119-122 |
| 0 | 951, 951, 951 | 68912 | 0 |
| 0, `fragment_mtu = 253` | 1001, 1001, 1001 | 65470 | 0 |

The last row cuts every part into two fragments at the board's MTU
(node 1 284/142 and node 2 284/142 frags/parts), so each reassembly holds
a partial packet for every part: nothing abandoned either.

`transfer_ms` fits 0.95 s + 144 x G: the sender's pacer holds every part
it sends, and the relay's pacing of its forwarded copies overlaps it.

## The floor on btvirt

**No loss at any gap, so there is no first-loss cell.** The reason is
the one the two-node run gave: both pumps hand a packet's fragments to
BlueZ whole before taking the next packet, and one ATT bearer delivers in
order, so a gap of zero cannot interleave two packets at a receiver.

**What bounds the copy at 0 ms is the resource's request round trip, and
half of it is bluetoothd.** On the relay (seed 1 room), the 142 parts
arrived in 29 windows of 3 to 7 parts over 655 ms, and every window
waited for the 131-byte request that asks for it. The two directions of
a hop cost very different times:

- data, node 0 to 1 to 2, the central's Write Commands: 0.3 ms a hop
  from `BLE_TX_ROUTE` on the sender to `driver: received` on the next
  node;
- the request, node 2 to 1 to 0, the peripheral's notifications:
  10.6 ms a hop, every time.

A btmon capture of one room at 0 ms places the 10.6 ms: node 2 logs
`BLE_TX_ROUTE len=131` at 14:35:47.190994Z, its controller's ACL TX
carrying it is at 16:35:47.201688 local (10.7 ms later), and node 1's
controller RX and lnsd receipt follow within 0.25 ms. Every one of the
192 notifications in the capture is opcode 0x23, Handle Multiple Value
Notification. bluetoothd 5.82 buffers a notification to a client that
set the Multiple Handle Value Notifications feature and flushes the
buffer on a 10 ms timer (`src/shared/gatt-server.c`,
`NFY_MULT_TIMEOUT 10`, `bt_gatt_server_send_notification`; the feature
bit is read in `src/gatt-database.c` and set by bluetoothd's own GATT
client in `src/shared/gatt-client.c`). A 483-byte part fills the
511-byte buffer and flushes the one before it, so a stream of parts is
not delayed; a lone request is, by the full 10 ms. 28 round trips of
about 23 ms are the 655 ms.

This is a host-stack property of lnsd as a peripheral notifying a BlueZ
central, not of the pacer and not of btvirt's air (btvirt has none).

## What the rig run has to add

- **The phone as receiver.** The 100 ms is justified by a Columba phone
  losing the first of two back-to-back fragmented packets (2026-09-09,
  `leviculum-nrf/ble-tx/src/gap.rs:27-34`); neither lnsd nor btvirt can
  stand in for its receive path. The same grid with the phone receiving,
  delivered counts read off the conversation, is the decision.
- **Connection events.** A board's pumps send per connection event;
  below one connection interval the gap stops
  being the limit and the controller's queue becomes it. That is where a
  sub-interval gap can still matter, and only the rig has it.
- **Whether an Android central sets the Multiple Handle Value
  Notifications feature** toward an lnsd peripheral: if it does, every
  lone notification lnsd sends it waits the same 10 ms in bluetoothd.
  A board's SoftDevice has no such buffer.

Nothing here changes the default; on btvirt the floor is 0 ms.
