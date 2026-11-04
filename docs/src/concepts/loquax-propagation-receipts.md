# Loquax: receipts for propagated post

Status: concept and wire, decided 2026-11-04 (the reviewer's defaults
for Lew over the study, order 1393, Refs #232, #450). The codec is built
(`leviculum-lxmf/src/lev.rs`, key `0x07` and capability bit 2); **no
Loquax behaviour yet**: no client sets bit 2, sends `0x07` or reads it.
That is order 1395. The study behind this page is the reviewer's
`2026-11-04-empfangsbestaetigung-propagation` (German, every claim
cited); the second half of its recommendation, a status query to lnpnd,
is order 1394 and not on this page.

## The problem

A message handed to a propagation node is out of sight. The sender learns
that the node took it, and nothing after: not when the recipient
collected it, not whether it ever did. On the screen it stays at one tick,
the same tick a message that the node later dropped shows.

## What LXMF offers

Nothing for this, by design.

- A propagated message ends at the sender in `SENT`, never in
  `DELIVERED`. The node's proof of the upload (packet,
  `reference/LXMF/LXMF/LXMessage.py:494-503`, or resource,
  `:608-610`) calls `__mark_propagated`, which sets `state = SENT`
  (`:573-575`). States and methods: `LXMessage.py:15-22`, `:30-33`.
- `DELIVERED` comes only from the recipient's transport proof of a direct
  or opportunistic delivery (`LXMessage.py:466-482`); the recipient proves
  before it looks at the message (`LXMRouter.py:1926-1927`).
- Collecting from a node sends one signal, to the node alone: the client's
  list of ids it has, which the node deletes (`LXMRouter.py:1507-1517`).
  No field means a receipt (`reference/LXMF/LXMF/LXMF.py:8-51`).
- The node does not know the sender. Source, signature and content are
  encrypted for the recipient (`LXMessage.py:429-433`), and the sender
  uploads over a link that never identifies. So a node cannot tell the
  sender anything without the sender giving itself away to it.
- Upstream left it out on purpose: "When using propagation nodes, no
  message delivery notifications will be sent in return", "intentional,
  and done for privacy purposes" (Mark Qvist, Reticulum Discussion #192,
  2023-01-27, <https://github.com/markqvist/Reticulum/discussions/192>).
  LXMF up to 1.2.0 added nothing of the kind.

Sideband, NomadNet, Columba and Ratspeak all show "on the propagation
network" honestly and stop there; none of them has a receipt on the wire.

## The decision

The **recipient** acknowledges, inside the `lev` namespace
([read receipts](loquax-read-receipts.md), section 1), and only

- messages that **arrived by propagation**: our router hands them on as
  `DeliveryMethod::Propagated` (`leviculum-lxmf/src/message.rs:25-30`,
  set where a collected message is unpacked,
  `leviculum-lxmf/src/router/propagation_runtime.rs:1711`), as the
  reference does with `method=LXMessage.PROPAGATED`
  (`LXMRouter.py:2508`); and
- whose sender set **capability bit 2**.

The sender cannot mark "only via propagation" itself: a home node signs a
message once and sends the same bytes direct and to the node
(`loquax-home-node.md`, the policy); a field added later would be a new
signature and a new message id. A message that arrived direct has its
proof already and gets no `0x07`.

**Not switchable.** It behaves like the transport proof, which nobody can
turn off, not like read receipts. It does not follow the read-receipts
switch. Only a client that sends no `lev` mark at all neither asks for nor
sends one.

## The wire

```
fields[0xFC] = map {
    0x00: uint   capabilities; bit 2 = "I acknowledge propagated post and want it acknowledged"
    0x07: [ uint32 received_at,                unix seconds, the recipient's clock
            [ bin32 message id, ... ] ]        at most 7 ids
}
```

- **Bit 2**, `CAP_PROPAGATION_RECEIPTS = 4`, beside bit 0 (read receipts)
  and bit 1 (groups). The value stays a positive fixint: asking costs
  **0 bytes**.
- **Key `0x07`**, `KEY_PROPAGATION_RECEIPT`: the shape of the read receipt
  `0x01`, under its own key, so no reader ever takes arrival for reading.
  `received_at` is when the first of the listed messages arrived on the
  recipient's device. A `0x07` of another shape is none; an id of another
  length in it is passed over, as in `0x01`.
- **The bytes**, capabilities 5 (bits 0 and 2), `received_at` 1700000100,
  one id (`a_propagation_receipt_is_the_hand_built_bytes` in `lev.rs`):

  ```
  82 00 05 07 92 ce 65 53 f1 64 91 c4 20 <32-byte message id>
  ```

- **Cost to answer**: alone, without the mark, the field pair with its two
  keys is **18 bytes plus 34 per id** (`c4 20` and the 32 bytes). On a
  message that carries the mark anyway it adds 8 bytes plus 34 per id.
  As an LXMF message alone with one id it is about the 240 bytes of a read
  receipt alone, just under a second at SF8/BW125.
- **An older reader skips it**: versions move by keys, and a reader skips
  a key it does not know (`a_decoder_without_0x07_skips_it_and_reads_the_rest`).

## Sending and taking

- **Sent by the recipient as its own LXMF message to the sender**, the same
  path as a read receipt alone: direct first, then the sender's mailbox,
  batched the same way, seven ids at most a receipt
  ([read receipts](loquax-read-receipts.md), section 3). It may ride on a
  message to that sender as a read receipt does.
- **Read wins.** A message read inside the batch window gets only the read
  receipt `0x01`; read includes arrived.
- **No receipt of a receipt.** A receipt is never a bubble, and it arrives
  with whatever method; it is never answered.
- **Taken only from the destination**, as a read receipt (section 4 of the
  read-receipts page): an id that is not ours in that sender's conversation
  is dropped.
- **Monotone.** It moves a message to the delivered rung and never back; a
  read receipt or a late proof after it changes nothing that is already
  higher.
- With a home node, the study proposes the home node sends it when the
  first device pulls the message, as with seen marks (section 8 of the
  read-receipts page). Order 1395 settles it.

## On the ladder

No new rung. The marks are the ladder of the read-receipts page
(section 6):

| Event | Mark | Words in the detail view |
|---|---|---|
| the node took it | one grey tick, as today | "Waiting in the mailbox" |
| `0x07` came | two grey ticks | "Delivered via the propagation network 10:31" |
| `0x01` came | two accent ticks | "Read 10:32", as before |

The screen reader says the words.

## Who does what else

- **A stock recipient** (Sideband, Columba, NomadNet, MeshChat, Ratspeak):
  does nothing. It does not see the mark, sends no `0x07`, and the sender's
  message stays at one tick, which is true.
- **A stock propagation node** (Python `lxmd`): does nothing. It cannot
  read the field; the receipt is an ordinary message to it.
- **lnpnd**: nothing either; the receipt is end to end.

## Privacy

The receipt tells the sender when the recipient's device was online and
took the message. A direct delivery's proof tells the sender exactly that
today. No new party learns anything: the node sees one more encrypted
message, as for any reply. Nothing identifies the sender to the node.

## Status

- Built (1393): the codec, `CAP_PROPAGATION_RECEIPTS`,
  `KEY_PROPAGATION_RECEIPT`, `PropagationReceipt` with its batches, the
  hand-built vector and the older-reader test, in
  `leviculum-lxmf/src/lev.rs`.
- Not built: setting bit 2, sending `0x07` on a propagated arrival, taking
  it, the words on the ladder (1395); a status query to lnpnd by
  transient id (1394).
