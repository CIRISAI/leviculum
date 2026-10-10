# The BLE link gap floor, lnsd to lnsd on btvirt (#379)

**Date:** 2026-10-10.
**Question:** at what inter-packet gap does a BLE link first lose a
packet to its receiver's reassembly, as a function of how many fragments
a packet takes? The pacer holds `DEFAULT_TX_GAP_MS` (100 ms,
`leviculum-nrf/ble-tx/src/gap.rs:35`) between the last fragment of one
packet and the first of the next, which caps a link at ten packets a
second; the receiver keeps one reassembly per link and the 5-byte header
has no packet id (`BleDefragmenter::process`,
`leviculum-core/src/framing/ble.rs`), so two packets whose fragments
interleave can only end in a discard.
**Instrument:** `leviculum-std/tests/ble_link_gap_floor.rs`,
`ble_link_gap_floor` (`#[ignore]`, run by name; the module docs give the
command and the environment that selects the grid). Two release `lnsd`
on `btvirt -L -l2` (the patched build, `/usr/local/bin/btvirt`), one
link, `tx_gap_ms = G` on both. A shared-instance client on the sender
writes N PLAIN broadcasts of L wire bytes in bursts of 16; a client on
the receiver counts distinct sequence numbers. Discards are the
receiver's `BLE_RX_ABANDON lost=` sum and its `bad fragment` debug
lines, per cell.

## Two knobs this needed, both default-off

- `tx_gap_ms` (BLEInterface key): the gap was the compiled constant, so
  it became a config key resolved through the firmware's own
  `effective_tx_gap_ms`. Unset is 100 ms, as before.
- `fragment_mtu` (BLEInterface key): BlueZ to BlueZ exchanges an ATT MTU
  of **512**, at which every Reticulum packet (500 bytes at most) is one
  fragment, and the question is about packets that are not. The key
  caps the MTU the sender sizes fragments by; the grid runs at **253**,
  the board's, 245 payload bytes a fragment. Unset is the exchanged MTU.

At 253 the plan's lengths split 60 → 1, **200 → 1**, 500 → 3 fragments:
200 bytes is two fragments only below an ATT MTU of 208. 300 bytes
(2 fragments) was added so the grid has all three counts.

The `frags=` column is computed; the wire agrees. A btmon capture of
the G = 0 cells (`GAP_FLOOR_BTMON`, N = 20, central sender) holds
ATT Write Commands of exactly the lengths the fragmenter predicts, each
seen once on the sender's controller and once on the receiver's:
60 B → one 67-byte write; 300 B → 252 + 62; 500 B → 252 + 252 + 17.
Replayed through the defragmenter's rules in arrival order, the 120
received writes complete 60 packets with zero out-of-sequence frames.

## The table

N = 200 per cell. Sender n0 is the central (writes, paced per link in
`bluez::CentralTask::session_loop`); sender n1 the peripheral
(notifications, the shared notify pacer in the event loop). Each cell:
delivered of 200 / discards (`BLE_RX_ABANDON` + `bad fragment`) /
seconds for the 200. `BLE_TX_FANOUT_DROP` was 0 in every cell.

| G (ms) | L=60 (1 frag) | L=200 (1) | L=300 (2) | L=500 (3) |
|---|---|---|---|---|
| central sender | | | | |
| 100 | 200 / 0 / 20.2 | 200 / 0 / 20.3 | 200 / 0 / 20.3 | 200 / 0 / 20.4 |
| 50 | 200 / 0 / 10.3 | 200 / 0 / 10.3 | 200 / 0 / 10.3 | 200 / 0 / 10.4 |
| 25 | 200 / 0 / 5.4 | 200 / 0 / 5.3 | 200 / 0 / 5.6 | 200 / 0 / 5.6 |
| 10 | 200 / 0 / 2.5 | 200 / 0 / 2.5 | 200 / 0 / 2.5 | 200 / 0 / 2.5 |
| 5 | 200 / 0 / 1.3 | 200 / 0 / 1.3 | 200 / 0 / 1.5 | 200 / 0 / 1.6 |
| 0 | 200 / 0 / 0.7 | 200 / 0 / 0.7 | 200 / 0 / 0.7 | 200 / 0 / 0.7 |
| peripheral sender | | | | |
| 100 | 200 / 0 / 20.1 | 200 / 0 / 20.2 | 200 / 0 / 20.3 | 200 / 0 / 20.2 |
| 50 | 200 / 0 / 10.2 | 200 / 0 / 10.2 | 200 / 0 / 10.3 | 200 / 0 / 10.2 |
| 25 | 200 / 0 / 5.6 | 200 / 0 / 5.6 | 200 / 0 / 5.6 | 200 / 0 / 5.6 |
| 10 | 200 / 0 / 2.5 | 200 / 0 / 2.5 | 200 / 0 / 2.5 | 200 / 0 / 2.5 |
| 5 | 200 / 0 / 1.3 | 200 / 0 / 1.3 | 200 / 0 / 1.9 | 200 / 0 / 1.9 |
| 0 | 200 / 0 / 0.7 | 200 / 0 / 0.7 | 200 / 0 / 0.7 | 200 / 0 / 0.7 |

No cell lost a packet, so there is no "first loss" cell to repeat. The
three repetitions went to the harshest cells instead, G = 0 at 2 and 3
fragments, each in a fresh room, both senders: 12 cells, 2400 packets,
**2400 delivered, 0 discards**, 0.7 s a cell every time.

The pacer was live, not bypassed: the sender's bring-up line names the
gap and the cap (the test asserts both), the G = 100 room logged 799
`BLE_TX_GAP waited_ms=` lines for its 800 packets (the first of a link
is never deferred), the G = 0 room none, and the seconds column is N × G
to within the per-packet cost (about 3.5 ms here, which is why G = 5 and
G = 10 come out at 1.3 and 2.5 s rather than 1.0 and 2.0).

## Read against the mechanism

**The first discard never appears, and cannot on this path.** Both
lnsd pumps hand a packet's fragments to BlueZ one after another and
take the next packet only after the last one: the notify path awaits
each notification in the event loop
(`leviculum-std/src/interfaces/ble/mod.rs:1102-1103`), the central path
writes a whole queued unit before it receives the next
(`leviculum-std/src/interfaces/ble/bluez.rs:528-529`, the unit enqueued
whole at `mod.rs:1119`). One ATT bearer on one L2CAP channel delivers
in order. So packet k + 1's START is behind packet k's END at the
receiver at ANY gap, including none, and the two lines that turn an
interleave into a loss —
`leviculum-core/src/framing/ble.rs:327` (a START over a partial head,
`discard_partial`) and `:332-334` (a `total` that contradicts the
reassembly in progress) — had nothing to fire on. Neither fired in any
cell.

The prediction in the instruction ("the first discard where the gap is
shorter than one fragment's air plus the notification round trip")
assumes a sender whose next packet can overtake its previous one's
tail. lnsd's sender cannot. btvirt has no air time at all, so this run
says nothing about connection intervals, but the conclusion does not
depend on them: ordering is a property of the bearer, not of its
speed.

## Slack, and what it does not license

Between two lnsd the pacer is entirely slack: G = 0 moved 200 packets
in 0.7 s (about 285 a second) where the default allows 10, with nothing
lost. On this evidence alone the number is **0 ms**.

That is not a case for lowering the default. The 100 ms was never
about lnsd's own receiver: its measured justification is a Columba
phone losing the first of two back-to-back fragmented packets
(2026-09-09 desk run, `gap.rs:27-34`), and the 545 audit's D5 names the
mechanism there — the phone's receive path dispatches fragments through
coroutines and can reorder them, which lnsd and the board do not. The
floor that matters is the phone's, and btvirt cannot stand in for it.
What this run gives that measurement is the instrument: `tx_gap_ms`
lets an lnsd on a desk adapter be the gap-variable sender toward a real
Columba phone, the same grid with the phone as receiver and its
delivered count read off the conversation.

The decision is the reviewer's and Lew's; nothing here changes the
default.
