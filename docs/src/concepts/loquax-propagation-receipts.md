# Loquax: receipts for propagated post

Status: concept and wire, decided 2026-11-04 (the reviewer's defaults
for Lew over the study, order 1393, Refs #232, #450). The codec is built
(`leviculum-lxmf/src/lev.rs`, key `0x07` and capability bit 2). Order
1395 built the receipt in Loquax (sending, taking, the words on every
target) and the asking rule of section B; order 1409 put the request on
the wire, the home's word of the answer into its archive for its
devices, and the open ids into the store (see "Asking, as built" in
section B and Status at the end). The study behind this page is the reviewer's
`2026-11-04-empfangsbestaetigung-propagation` (German, every claim
cited). The second half of its recommendation, asking lnpnd what became
of a message, is section B below: built in lnpnd by order 1394, asked
by Loquax since 1409.

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
  `leviculum-lxmf/src/router/propagation_runtime.rs:1717`), as the
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
- **With a home node, the home node sends it, when the message reaches
  its archive** (1395). The home node is the recipient's endpoint: it
  collected the message from the propagation node, as a phone's own router
  would. Waiting for the first device to pull it, as the study proposed,
  would make the receipt depend on a phone being online, which a
  transport proof does not. A client of a home node acknowledges nothing
  (`loquax/src/ui/arrival.rs:282`, `arrival_step`, returns at
  `home_client`); the devices take the receipt that comes back from the
  peer as any device does.

### Where it is built (1395)

- **Bit 2 on every send**: `groups::client_caps`
  (`loquax/src/ui/groups.rs:77`), which `mark_sends` puts on every
  message (`loquax/src/ui.rs:3773-3777`); not switchable.
- **The arrival**: the router hands a collected message on as
  `DeliveryMethod::Propagated`; the engine says so after the message
  with `OutboxEvent::Propagated` (`loquax/src/engine.rs:607`).
- **The rule**: `arrival::acknowledges` (`loquax/src/ui/arrival.rs:182`):
  bit 2 in the message's own mark (since 1462 however it came), and not a
  bare receipt of either kind; an answer a receipt rides on is a message
  and is acknowledged (1486), and so is a group's copy (1466).
- **On a device that is its own node**: `arrived_here`
  (`loquax/src/ui/arrival.rs:226`) owes it, once per message (the ledger
  line `arrival acknowledged to the sender`); `arrival_step`
  (`loquax/src/ui/arrival.rs:272`) sends it after the read receipt's
  batch window, 5 s after the first arrival, alone or riding on the next
  message to that sender. Read wins: an id seen while read receipts go to
  that peer is dropped (`drop_read`, `loquax/src/ui/arrival.rs:388`).
- **On a home node** (since 1462): the home acknowledges nothing on its
  own; archiving is no device. A device that pulled the message hands the
  home an arrived mark (`MarkFlag::Arrived`), and the home's `arrived`
  (`loquax/src/home/server.rs`) applies the rule to the archived message
  and signs each receipt alone into the outbox, the first device's mark
  winning, logging `HOME device receipt to=… ids=… received_at=…
  message=…`. Before 1462 the home signed it when the message reached its
  archive, which said "on the device" of a message no device had.
- **Direct post too** (1462): the rule no longer asks how the message
  came. A proof is signed by the recipient's identity, which its mailbox
  holds, so only this receipt says the device has it
  ([the read-receipts page](loquax-read-receipts.md), section 6).
- **Taking it**: `take_lev` (`loquax/src/ui/receipts.rs:212`) hands a
  `0x07` to `apply_arrival` (`loquax/src/ui/arrival.rs:416`) before the
  read-receipts switch is looked at. A `0x07` alone carries no mark and
  does not change what the peer is believed to say about read receipts.

## On the ladder

No new rung. The marks are the ladder of the read-receipts page
(section 6):

| Event | Mark | Words in the detail view |
|---|---|---|
| the node took it | one grey tick, as today | "Waiting in the mailbox" |
| `0x07` came | two grey ticks | "On [name]'s device" (1462; before, "Delivered via the propagation network") |
| the node said `collected` | one grey tick, the arrived rung (1462: the recipient's mailbox may have collected it) | "Collected from the propagation network at 10:31" |
| the node said `expired` or `evicted` | one grey tick | "Not collected at the propagation node; perhaps delivered via another" |
| a proof came | one grey tick (1462; two before) | "Arrived" |
| `0x01` came | two accent ticks | "Read at 10:32", as before |

The screen reader says the words. The time is when this device learned
it. The words in the catalogue (`loquax/src/ui/strings.rs`), English and
German (`info_delivered_via_pn` and `state_delivered_via_pn` went with
1462: a `0x07` is the device's word, "On [name]'s device"):

| Key | English | German |
|---|---|---|
| `info_collected` | Collected from the propagation network at %1$s | Um %1$s aus dem Propagation-Netz abgeholt |
| `info_not_collected` | Not collected at the propagation node; perhaps delivered via another | Am Propagation-Knoten nicht abgeholt; vielleicht über einen anderen zugestellt |
| `state_collected` | Collected from the propagation network | Aus dem Propagation-Netz abgeholt |

**Where the words come from.** The message's ledger keeps the witness as
a line (`on the recipient's device: its receipt`, before 1462 `delivered
via the propagation network`, `collected from the
propagation network`, `not collected at the propagation node; perhaps via
another`), so the words survive a restart; `arrival::witness`
(`loquax/src/ui/arrival.rs:104`) reads it, and a proof that came first
keeps the device's words. The mark itself is the delivered rung of
`DELIVERY_GLYPHS`, unchanged.

**On every target.** The window and the phone show the Info rows of
`ui::info::message_info`, resolved from the catalogue; the phone's Java
detail view draws those rows, and its `strings_scene.xml` is written from
the catalogue (`just loquax-strings`). The terminal's detail view says
the same in its own words (`state  delivered via the propagation network
10:31`). The scene's mark node carries the screen reader's words
(`scene::mark_words`, with the bubble's `witness`). **The M9** draws the
ladder's ASCII rung, `vv` for two ticks, so a receipt and a proof look
alike there, as they do everywhere; it has no Info view yet, so the words
reach it only through the scene's mark words, which it does not draw
today. When the M9 gets an Info, it takes `message_info` as the phone
does and needs nothing of its own. The M9 is a client of its home and
asks no node (1409); its pull does not fold archive entries into its
model yet, so the home's `pn-status` entry reaches it once it does,
through the same `HomePnStatus` fold as the phone's, with nothing of its
own.

## Who does what else

- **A stock recipient** (Sideband, Columba, NomadNet, MeshChat, Ratspeak):
  does nothing. It does not see the mark, sends no `0x07`, and the sender's
  message stays at one tick, which is true.
- **A stock propagation node** (Python `lxmd`): does nothing. It cannot
  read the field; the receipt is an ordinary message to it.
- **lnpnd**: nothing for the receipt, which is end to end. It answers
  the status query of section B.

## B. Asking the node: `/lev/status`

The receipt needs a recipient that sends one. A stock recipient never
does, so for it the sender's only witness is the node it uploaded to:
the node saw the recipient's client collect the message. lnpnd answers a
query for that; a stock node cannot.

**Pull, never push.** A node that told the sender would have to know the
sender, and it does not: the upload comes over a link that never
identifies (`reference/LXMF/LXMF/LXMRouter.py:2889`). Learning it would
tie sender and recipient at the node, the leak upstream keeps the feature
out for (#192 above). So the sender asks, by the message's `transient_id`,
which only the sender and the nodes that hold the message know
(`reference/LXMF/LXMF/LXMessage.py:433-434`).

### What lnpnd keeps

Every id that leaves the store, with when and why, for 30 days after it
left (`PropagationNode::departures`, `leviculum-lxmf/src/propagation_node.rs`),
persisted across restarts in the data directory's `departures` file:

| Reason | When | Answer |
|---|---|---|
| `collected` | the recipient's client listed it in its `haves` and the node deleted it (`LXMRouter.py:1508-1519`) | `collected` |
| `evicted` | displaced to make room (`get_weight`, `LXMRouter.py:1056-1067`) | `evicted` |
| `expired` | older than the 30-day expiry, never collected here | `expired` |
| `delivered` | addressed to the node's own mailbox, delivered to it | `delivered` |

A message still in the store is `held`; an id the node never had, or
that left more than 30 days ago, is `unknown`.

- **File format `LNPNDDP2`** (`lnpnd/src/departures.rs`): the 8-byte
  magic, then 41 bytes an id: the 32-byte `transient_id`, the departure
  time as little-endian u64 seconds, one reason byte (1 collected,
  2 evicted, 3 expired, 4 delivered). The older `LNPNDDP1` (40 bytes, no
  reason) still loads; its ids stay duplicates and answer `unknown`,
  because that file did not say whether an id was collected or
  displaced. The next save writes `LNPNDDP2`.
- **An expired id is no duplicate.** It is kept to answer, but a restart
  does not seed the duplicate cache with it: past the window a peer
  re-offers what it received after us, a new copy here too.
- **The log line** per departure, whichever path removed it (a `/get`
  confirmation, an own-mailbox delivery, an eviction during an upload or
  a peer's sync, an expiry):

  ```
  PN_DEPART id=<transient_id, 64 hex> reason=collected|evicted|expired|delivered t=<ms>
  ```

  and one `PN_STATUS ids=<n> result=answered|refused|throttled` per query.

### The wire

```
request path  "/lev/status"       on lxmf.propagation, path hash 46b17db4be0eb4b3e31ab655f95a87a4
request data  [ bin32 transient_id, ... ]          at most 64 ids
answer        { bin32 transient_id: str word, ... } every asked id, in the order asked
              word = "held" | "collected" | "evicted" | "expired" | "delivered" | "unknown"
refusal       uint error code                     0xf4 malformed or over 64 ids, 0xf6 throttled
```

- **One id, collected** (`a_request_round_trips_and_its_bytes_are_an_array_of_bins`,
  `an_answer_round_trips_and_a_refusal_is_its_code` in
  `leviculum-lxmf/src/lev_status.rs`):

  ```
  request  91 c4 20 <32-byte transient id>                                  35 bytes
  answer   81 c4 20 <32-byte transient id> a9 63 6f 6c 6c 65 63 74 65 64    45 bytes
  ```

  Seven ids are a 239-byte request and an answer under 320 bytes, so a
  home's usual question fits one packet each way on a link.
- A word a reader does not know reads as `unknown`, so a later node can
  add one.
- **Open to any link, never identified.** The node never asks who is on
  the link. A home asks over a link of its own that it does not identify,
  not over its sync link, which identifies before `/get`
  (`leviculum-lxmf/src/router/propagation_runtime.rs:575`), or the node
  could tie its identity to the ids and through them to the recipients.
- **Spaced per link**: a second request within one second on the same
  link is answered `0xf6`. A home asks once per sync round.

### The announce

The capability rides the propagation node's announce, so a client never
asks a stock node, which would leave the request unanswered
(`reference/Reticulum/RNS/Link.py:859-905`, no branch for a missing
handler). Metadata key `PN_META_CUSTOM` 0xFF
(`reference/LXMF/LXMF/LXMF.py:138`):

```
field 6 = { ..., 0xFF: { "lev": { "status": 1 } } }
            cc ff 81 a3 6c 65 76 81 a6 73 74 61 74 75 73 01     16 bytes
```

`1` is the version of this wire. The reference checks only that field 6
is a dict (`LXMF.py:244`), so a stock client keeps taking the announce
and ignores the key (`lev_status_reference` feeds our exact announce to
that function). A board announces none: it keeps no departures and does
not serve the path. Readers look up `{"lev": {"status": _}}` and skip any
other key in either map (`lev_status::announced_version`).

### What the answers mean, and what they cannot

- **`collected`** means the recipient's LXMF router took it from this
  node. With Sideband or Columba the router is on the phone, so it
  reached the device. It does not mean read.
- **`expired` means "not collected here", never "lost".** With peering
  every peer gets a copy (`LXMRouter.py:2519`), so the recipient may
  have collected it at another node, which deletes only its own copy;
  ours expires uncollected. The words must say so (the table in section C of the
  study: "not collected at this node; perhaps delivered via another").
- **`evicted`** the same: gone from here, perhaps delivered elsewhere.
- **`unknown`** after a node's departures file was lost or never
  written: no claim at all.

### Privacy of the query

- The node saw the collection anyway. New is only that it tells whoever
  holds the `transient_id`: the sender, and every node that holds the
  message by peering. A foreign peer node can thus learn when the
  recipient collected at ours. Small, but it is there.
- The query reveals only that someone holding the ids asks. Over an
  unidentified link the node cannot tie the asker to anything.
- **The home's rule (1395):** ask at most once per sync round, only for
  ids still open, and for at most 30 days after the upload; an id that
  reached `collected`, `delivered` or a receipt is never asked again.
  Built as `pn_status::Asker` (`loquax/src/pn_status.rs`): a node is
  asked only after its announce said `{"lev": {"status": _}}`
  (`OutboxEvent::NodeAnnounced.status`, read at `loquax/src/engine.rs:1974`);
  the ids come from the upload's proof (`RouterEvent::PropagationUploaded`
  → `OutboxEvent::Uploaded`); at most 64 ids a request, oldest first;
  `collected`, `delivered`, `expired` and `evicted` close an id, `held`
  and `unknown` leave it open; each answer logs
  `PN_STATUS_ASKED ids=N answered=N`, where answered counts the ids the
  node named with a word other than `unknown`. The answers fold into the
  marks with `UiModel::pn_answered` (`loquax/src/ui/arrival.rs:444`).

### Asking, as built (1409)

- **The request.** `home::client::ask_pn_status`
  (`loquax/src/home/client.rs`) asks the path if there is none, takes the
  node's key from its announce (`ReticulumNode::get_identity`), opens a
  link to the node's `lxmf.propagation` destination and **never
  identifies it**, sends `/lev/status` with `lev_status::encode_request`,
  reads the answer with `pn_status::read_answer` (a refusal or bytes that
  are no answer say nothing) and closes the link. One upload's question
  is 35 bytes (`the_request_for_one_upload_is_35_bytes`):

  ```
  91 c4 20 <32-byte transient id>
  ```

- **When.** At the end of a sync round that synced (one that failed
  found the node silent): `Asker::round` names, per node that holds an
  open id, the ids to ask or why not, and every round that does not ask
  says so: `PN_STATUS_SKIPPED reason=no_capability` (the node never
  announced `/lev/status`, a stock `lxmd`), `no_open_ids` (nothing open
  at all) or `rate` (asked this round, or its question still in flight).
  A failed link or a refusal leaves every id open; the next round asks
  again. The node an id is asked at is the one that proved the upload:
  `OutboxEvent::Uploaded` carries the router's selected node.
- **Settled without asking.** A proof (`State`/`Left` delivered), and a
  read receipt or a propagation receipt naming the id
  (`home::server::settles` on the home, `apply_arrival` on a device),
  close it.
- **On the home.** The daemon asks after each synced round on a task per
  node (`ask_pns`, `loquax/src/home/daemon.rs`, budget 30 s), folds the
  answer into its `Asker` and keeps the words that move a mark,
  `collected`, `delivered`, `expired` and `evicted`, as archive entries
  of the new kind `KIND_PN_STATUS` = 13 (`pn-status` in
  `loquax mailbox archive`): `{ bin32 message id: str word, ... }`, the
  answer's shape keyed by the message id every device knows, at most 64
  an entry (`wire::pn_status_entry`), logged as
  `HOME pn status ids=N seq=S`; the archive wakes the devices. `held` and
  `unknown` are archived for nobody.
- **On the devices.** A client's pull hands the entry to its engine as
  `Command::HomePnStatus`, which says `OutboxEvent::HomePnStatus`, which
  the model folds with `pn_answered`, as a device that asked itself. A
  client from before skips the kind. The phone is always a client of a
  home, its own mailbox in the app or a computer's, so the asking is the
  home's; the phone session asks nothing.
- **A device that is its own node** (the terminal and the window without
  a home): the model keeps its own `Asker` (`UiModel::pn_asker`), asks on
  `UiEvent::FetchDone(Ok)` with `UiEffect::AskPn { node, ids }`, which
  the shell runs over the same `ask_pn_status` and answers with
  `UiEvent::PnAnswered`; `UiEffect::PnLine` carries the log lines.
- **Across a restart.** The store keeps the open ids and the nodes that
  answer (schema 15, tables `pn_asks` and `pn_nodes`, written whole when
  they change: `Asker::state`, `Asker::restore`), so an upload before a
  restart is asked about after it
  (`the_home_asks_its_node_what_became_of_a_message_across_a_restart`).

### For a later cell against lxmd (lane 2)

A periculum cell with a stock `lxmd` beside lnpnd must assert: lxmd
keeps validating lnpnd's announce (it keeps lnpnd as a known propagation
node and peers it, `pn_announce_data_is_valid` true on the wire); lxmd's
own announce carries no `0xFF` key, so a reader finds no capability; and
a `/lev/status` request sent to lxmd gets no answer (the request times
out), while the same request to lnpnd for an id lxmd's peer sync carried
over answers `held`, then `collected` after the recipient's stock client
fetched from lnpnd, and stays `held`, never `collected`, when it fetched
from lxmd instead.

### For the cell on lane 2: Loquax to a stock recipient over lnpnd

A periculum cell beside `lxmf_pn_loquax_leviculum.toml` must assert,
once the request is on the wire:

- the stock Python recipient collects from lnpnd and **sends no `0x07`**:
  no LXMF message from the recipient reaches the sender after the
  collection, and lnpnd's log shows no upload from it;
- lnpnd logs `PN_DEPART id=<the transient id> reason=collected` and, on
  the sender's question, `PN_STATUS ids=1 result=answered`;
- the sender logs `PN_STATUS_ASKED ids=1 answered=1` and its row of that
  message ends on two grey ticks with "Collected from the propagation
  network";
- the control over a Python `lxmd`: the sender never asks
  (`PN_STATUS_ASKED` absent) and the row stays on one tick.

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
- Built (1394): section B in lnpnd. The departure reasons and their
  `LNPNDDP2` file, `PN_DEPART` and `PN_STATUS`, the `/lev/status` handler
  with its spacing, the `0xFF` announce entry, the codec in
  `leviculum-lxmf/src/lev_status.rs`.
- Built (1395): bit 2 on every message; the receipt on a propagated
  arrival, on a device that is its own node and on a home node; taking it,
  the words on the ladder in English and German on the terminal, the
  window and the phone; the asking rule (`pn_status::Asker`), the
  upload's transient id out of the router, the node's capability out of
  its announce, and the fold of the answers into the marks.
- Built (1409): the `/lev/status` request over an unidentified link of
  its own, from the home's sync round and from a device that is its own
  node; the home's word of the answers to its devices as the archive kind
  `pn-status`; the open ids in the store; `PN_STATUS_SKIPPED`. Tests: the
  home against a real lnpnd across a restart and a stock Python recipient
  (`loquax/tests/home_node.rs`), the stock `lxmd` never asked
  (`a_message_the_recipient_cannot_take_goes_to_a_propagation_node_and_arrives_once`),
  the phone's row moving to "Collected from the propagation network"
  through the home (`a_message_collected_at_the_node_shows_collected_on_the_phone`
  in `loquax-android/tests/facade.rs`), the model's cadence with
  `unknown`, a failed link and a stock node (`loquax/src/ui.rs`).
- Not built yet: the M9 folding archive entries; the cells on lane 2.
