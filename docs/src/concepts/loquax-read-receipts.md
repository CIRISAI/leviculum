# Loquax: read receipts

Status: decided 2026-10-31 (Lew), with the reviewer's decisions of
2026-11-01 over the study; slices R1 to R3 built (orders 1230 to 1232,
Refs #232): the wire, the mark on every message, the peer's flag, the Read
state, the read event and its sending, and the tick ladder on every
target. The
study behind it is the reviewer's `2026-10-31-lesebestaetigungen-konzept`
(German, every claim cited); this page keeps what is decided and points at
the code as it stands. Groups are parked.

Read receipts are a Leviculum extension. Sideband, Columba, Ratspeak,
NomadNet and MeshChat keep working unchanged and do not notice it: nothing
on the wire is visible to them, and none of them is ever sent a receipt.
Loquax sends them by default once the reader has looked at the chat; the
switch is under Privacy (`loquax-android-navigation.md`, Settings).

## Decisions

| Question | Decision | Why |
|---|---|---|
| Wire | the `lev` namespace in the custom field pair: `0xFB` = `b"lev"`, `0xFC` = a msgpack map; the mark `{0x00: 1}` in every message, a receipt `{0x01: [read_at, [ids]]}` | the pair is what LXMF keeps for client protocols (`reference/LXMF/LXMF/LXMF.py:34-45`); keys above 0xFF break Ratspeak; invisible in every client checked |
| Gating | receipts go only to senders whose message carried the mark | a foreign client is never sent one; nothing needs to be detected in advance |
| Alone or piggybacked | both: on the next message when a reply is being written (60 s at most), otherwise alone after 5 s or on leaving the chat | LoRa airtime, about 1 s for a receipt alone, against freshness |
| Id length | the full 32-byte LXMF message id | as `REACTION_TO` and `FIELD_REPLY_TO`; seven ids fit one packet |
| Through the mailbox | yes, with the time of reading | partners that are online in turns still learn it |
| Read event | per bubble: half visible, chat in the foreground, held 1 s; the core decides | "what was on screen", as Signal, Telegram and Matrix; the second keeps a flick through from counting |
| Mark as read from the list | sends no receipt; a reply from the notification does | a receipt says "was on screen" |
| Glyphs | the tick ladder below | the language the 90 % user knows; shape and weight carry it, colour only adds |
| Ratchets | no glyph; at most a line under Advanced in the detail view | every message is encrypted; a lock on some would be a lie about the others |
| Reciprocity | yes | one rule, enforced by the wire, saves airtime |
| A contact without receipts | stays at two grey ticks, the detail view says so | never pretend |
| Publish the namespace | yes, on this page | so other clients can adopt it; an offer, not an upstream report |
| Groups | parked | Loquax has none; when they come: read once all have read, "read by 2 of 3" in the detail view |

## 1. The wire: the `lev` namespace

This section is the namespace's public description. Any client may write
and read it.

### Where it lives

LXMF has no field with read or delivery meaning. Its table
(`reference/LXMF/LXMF/LXMF.py:8-27`, `:44-51`) has commands and results
(`0x09`, `0x0A`), which are no receipt, and the transport proof is an RNS
packet, not a field: it says the bytes reached the destination identity,
not that a person read them.

For experiments the reference suggests keys above 0xFF
(`LXMF.py:29-32`). They are no option: rsLXMF, the library under
Ratspeak, truncates every field key to a `u8` (rsLXMF `769d0eb`, in
its `crates/lxmf-core/src/message.rs` lines 552 to 557, the field type
`BTreeMap<u8, Vec<u8>>` at line 63; a sibling repository, not checked
out here), so 0x1FB would arrive there as 0xFB and
overwrite that client's own field. **Leviculum never writes a field key
above 0xFF.**

For a client's own protocol LXMF keeps exactly one pair: a type in
`FIELD_CUSTOM_TYPE` (0xFB), the payload in `FIELD_CUSTOM_DATA` (0xFC); "it
is up to the client application to correctly discern … any data embedded
using this mechanism" (`LXMF.py:34-45`). Ratspeak uses the pair the same
way (`ratspeak.chat.v1`, `v2`) and ignores a type it does not know.
Unknown fields survive a Python unpack whole: `unpack_from_bytes` hands
the field dict on (`reference/LXMF/LXMF/LXMessage.py:768`, `:787`), and the router
delivers the message with it. A fifth payload element is taboo: Python
takes it for the stamp and cuts it off (`LXMessage.py:755-758`). So the
namespace lives in the field dict and nowhere else.

### The format

```
fields[0xFB] = bin "lev"                         FIELD_CUSTOM_TYPE
fields[0xFC] = map {                             FIELD_CUSTOM_DATA, the map itself, not a bin around it
    0x00: uint   capabilities; bit 0 = "I send read receipts and want them"
    0x01: [ uint32 read_at,                      unix seconds, the reader's clock
            [ bin32 message id, ... ] ]          in receipts only, at most 7 ids
}
```

- **The type `lev`**, three bytes: the mark rides in every message, over
  LoRa too, where each byte costs about 3 ms at SF8/BW125. Versions move by
  map keys, never by the type: **a reader skips a key it does not know**,
  and a key never changes meaning. A `str` type is read like the `bin`.
- **The map directly in 0xFC**, not a `bin` holding msgpack as Ratspeak
  writes it: two bytes less, and no other client reads our type.
- **Full 32-byte message ids**, as `REACTION_TO` and `FIELD_REPLY_TO` use
  them. The id is the LXMF message id; the direct copy and the mailbox copy
  of a message carry the same one (`loquax/src/seen.rs:11-14`).
- **One time per receipt**: when the first of the listed bubbles came on
  screen. The reader's clock may be wrong; the sender keeps `read_at`
  within when it sent the message and when the receipt came.
- **At most seven ids per receipt** (`RECEIPT_IDS_MAX`): a single-packet
  message carries 295 bytes of content (`LXMessage.py:76-85`,
  `leviculum-lxmf/src/constants.rs:43`), each id costs 34. More go as
  several receipts.
- A `lev` type whose data is not a map says nothing. A receipt that is not
  `[uint32, [bin, ...]]` is none; an id of another length in it is passed
  over.

### The bytes

The mark in a message's field map, 12 bytes after the map header
(`VEC-MSG-LEV-MARK`):

```
cc fb c4 03 6c 65 76          0xFB: bin "lev"
cc fc 81 00 01                0xFC: {0x00: 1}
```

A receipt with the mark, one id, `read_at` 1700000100
(`VEC-MSG-LEV-RECEIPT`, 53 bytes; each further id 34 more):

```
cc fb c4 03 6c 65 76
cc fc 82 00 01 01 92 ce 65 53 f1 64 91 c4 20 <32-byte message id>
```

Both are frozen vectors in `docs/src/appendix/lxmf/vectors/vectors.json`,
written by the reference itself (`gen_vectors.py`, `gen_lev_vector`): the
reference packs the fields, unpacks the message with
`LXMessage.unpack_from_bytes`, and what it read back packs again to the
same bytes. `leviculum-lxmf/tests/lev_vectors.rs` holds both directions;
the codec is `leviculum-lxmf/src/lev.rs`.

### What other clients do with it

| | Sideband 2.0.1 | Columba main | NomadNet | Ratspeak | Python LXMF |
|---|---|---|---|---|---|
| the pair in a normal text | shown as usual; its UI asks only for known keys | as usual; fields kept as JSON, shown by known keys | as usual; reads only the renderer and attachments | as usual: an unknown type is no extension | as usual |
| an empty message with only the pair | no bubble, but the conversation is made and marked unread, and a trusted contact raises an empty notification | dropped quietly since v2.0.0-beta | an **empty line** in the conversation, and a notification | an unknown type is no extension | delivered to the app |

So the mark is invisible everywhere, and an empty receipt is not: that is
why a receipt only ever goes to a sender whose own message carried the
mark (section 2). The study cites each cell to the client's source at the
commits it read.

### What was not chosen

- `FIELD_COMMANDS`: quiet in Sideband, but it takes Sideband's command
  namespace and logs "Unauthorized command" there.
- The announce's app data: Python reads it by position and would not
  break, but the functionality codes belong upstream, and an announce
  tells everyone on the network who has receipts on.

## 2. Gating and reciprocity

The message that is read carries its sender's mark. When B reads a marked
message of A's, B knows in that moment that A understands receipts and
wants them; an unmarked message (a foreign client, or Loquax with receipts
off) never causes one. No detection in advance.

The latest message from a peer decides (`Flags::receipts`,
`loquax/src/ui/history.rs`): a peer that turns receipts off stops being
sent any with its next message. A receipt proves support too, even from a
peer whose earlier message had no mark.

Reciprocity, as WhatsApp and Signal: who turns receipts off sends no mark
(bit 0 off, or no namespace), so nobody sends them receipts, and no
airtime goes to receipts nobody shows. Loquax with the switch off shows no
read states, old ones neither.

## 3. Sending a receipt (built, R2)

- **One batch per chat visit.** It goes when the chat is left, the app
  goes to the background, or 5 s after the last bubble newly seen. With a
  draft in the compose field (`loquax-storage.md`, drafts) the batch waits
  up to 60 s for the send and rides **on that message**; after that it goes
  alone.
- **Alone** it is an LXMF message with empty content and title and only the
  namespace's pair, sent as any Loquax message: opportunistic first (one
  packet), then without a proof on to the sender's mailbox, as the home
  node's outbox does (`loquax-home-node.md`, the policy). A propagation
  node needs a stamp for it as for any message; at cost 13 that is a
  fraction of a second in a release build. A stamp the recipient asks for
  is paid as for any message.
- **No receipt of a receipt.** A receipt is never a bubble, so it is never
  read; it gets the ordinary transport proof.
- **Days later, through the mailbox:** A writes, B is offline, the message
  waits at the mailbox. B collects it three days later and reads it: the
  receipt carries the time of reading. If A is offline then, it goes to A's
  mailbox, and A sees "Read Thu 10:32" on the next collection.

On air a receipt alone with one id is about 240 bytes plus the proof back,
just under a second at SF8/BW125. Hence the batch and the piggyback.

As built (1231, `loquax/src/ui/receipts.rs`):

- The batch is one a peer, the ids in the order they were seen, and its
  read time is the reader's clock when the first of them was seen. Up to
  seven ids a receipt (`RECEIPT_IDS_MAX`); more go as several.
- It goes alone `BATCH_MS` (5 s) after the last bubble seen, at once when
  the chat is left or the app goes behind, and with a draft in that chat
  `DRAFT_WAIT_MS` (60 s) after the last bubble seen, left or not.
- The piggyback: the first message the model sends to that peer while the
  batch waits, a text, a reply, a reaction or a shared position, carries
  it in its own custom pair (`Lev::mark_with`); the rest of a batch above
  seven goes alone at once.
- Alone, it takes a slot of the send queue the bubbles' sends share
  (`RECEIPT_SEND`), so the runner sends it as any message, direct and then
  by the mailbox, and its router id, its end and its failure touch no
  bubble and say nothing on screen; a failure is a `tui.log` line. The
  terminal writes a `RECEIPT send` line for each one, alone or on a
  message.
- None goes to a peer that stopped taking them in the meantime, to a
  blocked one, with the switch off, or from a client of a home node, which
  holds no identity to sign it (R5).

## 4. Taking a receipt (built, R1)

- **Only from the destination.** A receipt reads only messages of ours in
  the receipt sender's own conversation, so only the peer a message went to
  can say it read it. Every other id is dropped with a log line
  (`RECEIPT dropped id=… from=…`): nobody forges another's state.
- **Read is monotone.** A second receipt and a late proof change nothing; a
  receipt before the proof, through the mailbox, sets Read at once, since
  read includes arrived. An id the screen does not hold is dropped.
- **A state, not a message**, as a position report (1209): a receipt alone
  makes no bubble, no unread, no toast, no notification, no conversation,
  and moves nothing in the list. A receipt on a text shows the text as
  usual and reads ours too.
- **The time** is the reader's, kept within when the message was sent and
  when the receipt came.

Code: the decode is `Lev::from_fields` (`leviculum-lxmf/src/lev.rs`), the
model's side `loquax/src/ui/receipts.rs` (`take_lev`, `apply_receipt`),
called from the `OutboxEvent::Received` arm of `loquax/src/ui.rs`, which
takes a receipt alone before any message is made. The mark goes on every
send in `update` (`receipts::mark_sends`), so a text, a reaction, a reply,
a forward and a shared position all carry it; `loquax send` from a script
does not, since it sends no receipts. The state is
`DeliveryState::Read { at }` (`leviculum-ui/src/delivery.rs`); the store
keeps it as state 8 with `messages.read_at`, and the peer's flag as
`conversations.receipts` (schema 9, `loquax/src/store.rs`).

## 5. The read event (built, R2)

A received message counts as **seen** when

1. its bubble is **at least half** in the chat's visible area (a bubble
   taller than the area: at least a third of the area),
2. the chat is in the **foreground** (Android: the activity resumed, which
   a screen turned off pauses; the window: focused and not minimised; the
   terminal: below),
3. and that holds **for one second** without a break.

Not seen: position reports, reactions, messages of blocked contacts, and
"mark as read" from the list. A **reply from the notification** counts for
the messages its text showed. Marking unread again takes no receipt back.

| Target | visible | foreground |
|---|---|---|
| Android | the bubble in the RecyclerView's area (the platform reports the keys) | `Intent::Front(true)` |
| window | bubbles in the scroll area | window focused |
| terminal | the visible message area | the terminal's focus report (`CSI ?1004h`, FocusGained/FocusLost); a terminal that reports none: visible counts once a key or the mouse was used in the chat |
| M9 | bubbles on the display | backlight on |

**The core decides.** The platform reports facts only,
`Intent::Viewport { peer, keys }` ("bubbles X..Y visible", new) and
`Intent::Front(bool)`; first-seen times, the second, which keys count
(received, marked, no report, not blocked, switch on) and the batch per
visit are core (a `no_std` `loquax/src/ui/receipts.rs`, as `scene.rs`).
The effect is a command to the engine, on a home-node client a `/mark`
with the flag "seen".

As built (1231):

- `UiEvent::Viewport { peer, keys }` and `UiEvent::Front(bool)`, the
  facade's `Request::Viewport` and `Request::Front` (`Intent::Viewport`,
  `Intent::Front`). A front never said counts as in front once a key or
  the mouse was used in the open chat.
- The clock is the model's monotonic one (`UiEvent::Elapsed`), `SEEN_MS`
  (1 s). A bubble leaving the viewport, or the front going, starts its
  second again.
- Seen is the received message's ledger line `seen`, kept in the store, so
  a restart sends no second receipt and the detail view says when.
- The terminal reports its own viewport (`ui::hit::viewport`, from the
  same layout it draws, a third of the pane for a taller bubble), none
  while the map, the help or a detail view covers the conversation, and
  asks for focus reports. The script's `focus in` and `focus out` play
  them.
- Not yet: the window reports no viewport, so it sends no receipts; the
  Android app sends no `viewport` request before R4; the reply from a
  notification.

## 6. The glyph ladder (built, R3)

The reviewer's decision of 2026-11-01 over the study: the mailbox is a
status of its own (Lew), and unproven-sent stays apart from it, so the
rule that the three truths are never collapsed (`loquax-ui.md`, section
2) holds in the glyph as in the words.

| State (core) | For the user | Android `Glyph.Kind` | Terminal | ASCII | Tone | Screen reader (en) |
|---|---|---|---|---|---|---|
| Queued, Stamping, InFlight | being sent | `CLOCK` | `◷ `, the braille spinner while the machine works | `.` / `*` | grey | "Sending" |
| Sent (opportunistic, no proof) | left this device, nobody confirmed it | `CLOCK` | `◷ ` | `.` | grey | "Sent, not confirmed yet" |
| InMailbox | the mailbox confirmed it: the upload to the propagation node is done | `TICK` | `✓ ` | `v` | grey | "Waiting in the mailbox" |
| Delivered | on the recipient's device (proof) | `TICKS` | `✓✓` | `vv` | grey | "Delivered" |
| Read | read | `TICKS_READ`, heavier stroke | `✔✔`, bold | `VV` | accent | "Read at 10:32" |
| Rejected, Failed | not sent | `FAILED`, red circle with `!` | `! `, bold | `!` | red | "Not sent. Double tap to send again" |

No eye, no ratchet glyph. Two grey ticks are also the end state for every
contact without receipts, which is honest: nothing is claimed that is not
known. The accent ticks carry weight as well as colour (Android stroke 2.6
against 2.0, the terminal's heavy `✔` and bold), so red-green weak eyes,
screen readers and monochrome tell them apart. The terminal's column
becomes two cells wide, every glyph padded to two. The detail view keeps
the words: "Sent. Nobody has confirmed it arrived yet.", "In the mailbox,
delivered once [name] is online.", "Delivered 10:31.", "Read 10:32." under
"Delivered 10:31.", and for a contact without receipts "[name] does not
send read receipts."

As built (1232):

- **One table.** `DELIVERY_GLYPHS` in `leviculum-ui/src/delivery.rs` holds
  the terminal's rows, two cells each, with tone (grey, green, red) and
  weight; `leviculum-ui/src/tokens.rs` keeps three tones, the values the
  queued, delivered and refused tones had. The pixel screens read
  `loquax::ui::chat::Mark` (six marks, five glyphs: sending and
  sent-unconfirmed share the clock) and its `Shape`, the lines on a
  24-unit grid that the window (`loquax/src/gui/chat.rs`) and the phone
  (`Glyph.java`) both draw; `loquax-android/tests/glyph.rs` reads the
  numbers out of `Glyph.java` and fails on a drift.
- **The double tick.** Bare ticks, no circle, the second 6.5 units right
  of the first. The study's second tick (short leg from `(10.5, 15)`)
  runs into the first's long leg once both strokes are counted (1.9 units
  between the centre lines against 2.0 of stroke, 2.6 for read); its short
  leg now starts at `(11.6, 16.1)`, which leaves 0.84 units clear at read's
  stroke. At 16 dp on a 2x and a 3x screen the two ticks are two pieces,
  the study's drawing one (`two_ticks_stay_two_at_sixteen_dp`).
- **Colours.** In a bubble the grey rungs are the bubble's meta colour;
  read is `read_mark`, `#b2ff59` in both themes, 5.3:1 (light) and 5.1:1
  (dark) on our green bubble, where the accent itself has 1.3:1. In the
  list read is the accent, 5.0:1 on white and 8.8:1 on the dark ground.
  `every_mark_holds_three_to_one_on_its_ground` in `loquax/src/ui/chat.rs`
  holds every mark at 3:1 on its ground in both themes.
- **The home node is the mailbox.** On a client of a home node the home's
  `Sent` (it took the message) shows one tick, not the clock
  (`loquax::ui::shown_delivery`): for the reader the home node is "the
  mailbox" (`loquax-ui.md`, the settings' Mailbox). Without a home node a
  propagation node's acceptance is the one tick.
- **Words.** The catalogue's `state_sending`, `state_unconfirmed`,
  `state_mailbox`, `state_delivered`, `state_read` (with the reader's time)
  and `state_failed`, in English and German; the scene's `MarkNode`
  carries them, the phone's `Ui.state` reads them. The detail view says
  "sent; nobody has confirmed it arrived yet", "in the mailbox, delivered
  once [name] is online", "delivered: it arrived on [name]'s device",
  "read 10:32, delivered before", "not sent: … its mark sends it again".
- **Send again.** The failed mark's tap is `Intent::SendAgain`, the core's
  `UiEvent::SendAgain`: the failed message leaves the device (on a home's
  client from every device of the home) and its text and fields go again
  as a new message. The phone's request is `{"t": "sendagain", "peer",
  "key"}`; the window presses the mark. The terminal has no key for it yet.
- **The walk.** The ugly store's imported conversations carry one sent
  message per rung; the walk's `marks` step reads the five mark ids
  (`state_clock`, `state_tick`, `state_ticks`, `state_ticks_read`,
  `state_failed`) and their words off the phone's screen.

Not yet: the "[name] does not send read receipts" line (R4); the M9 has
no Loquax screen yet, its renderer will read the same `Mark` and `Shape`;
an imported read message loses its time in the import's archive entry
(state code 8 without `read_at`), so it says "Read at" the epoch.

## 7. Settings

- **One switch** under Settings › Privacy, first, above the blocked
  contacts: "Read receipts", "Others see when you have read their messages.
  Turned off, you do not see it from others either." On by default.
- **Terminal and window**: the same value in the config
  (`read_receipts = on`, the default, or `off`; read since R2), switched by
  a palette command (R4).
- **With a home node** the value belongs to the identity and lives on the
  home node; every device shows and changes it there.
- No Reticulum words on the way: "read receipts", "read", "delivered",
  "mailbox". "Propagation node", "proof", "ratchet" at most in the detail
  view's technical part.

## 8. With a home node

The devices do not hold the LXMF identity (`loquax-home-node.md`, Roles
and keys), so the home node signs the receipt. A device reports the read
through `/mark` with a new flag "seen" and `read_at` (today's "read" means
"everything up to here is read" for the counter, which is something else).
The home node puts the receipt into its outbox. Incoming receipts become
archive entries that every device applies on its next pull, as the state
entries are today.

## 9. Proof of compatibility

In the cargo run: the vectors and `lev_vectors.rs` (section 1), the
model's tests in `loquax/src/ui.rs` (a receipt alone is no message, the
latest message decides the peer's flag, only the destination reads), the
store's round trip and migration of schema 9, and the phone's rows in
`loquax-android/tests/facade.rs`.

periculum cells, to come with R2 and R6:

| Cell | Setup | Holds |
|---|---|---|
| `loquax_tui_receipts_stock` | Alice `loquax tui`, Bob the Python `lxmf_node.py` | Bob receives Alice's text with the pair as specified; Bob's message read by Alice brings Bob nothing but Alice's texts; Alice's bubble stays at two grey ticks. Positive control: Bob sends with a mark set by hand, then exactly one receipt arrives |
| `loquax_receipts_pair` | two `loquax tui`, one lnsd | read at A shows at B with its time; a reply inside the batch time carries the receipt and none goes alone; B's switch off: B's messages without bit 0, A sends B none, B shows none |
| `lxmf_pn_loquax_receipts` | A writes, B offline, collected through lnpnd; A offline, B reads; A collects | A shows read at B's time of reading, not the collection's; a control twin against the Python node |
| `loquax_receipts_mixed` | Alice sends one text to Loquax Bob and Python Carol | Bob confirms, Carol receives only text |

periculum cannot run Sideband's or NomadNet's screens; the cells prove a
foreign client is never sent a receipt, which makes them irrelevant. A
hand test with Sideband and Columba on a phone after R3 belongs in its
acceptance report.

## 10. Slices

| # | Order | Status |
|---|---|---|
| R1 | the wire and the state in the core: codec and vectors, the mark on every send, the peer's flag, `DeliveryState::Read { at }`, the store, the facade's `read`, `read_at` and `receipts` | built, 1230 |
| R2 | the read event and sending: `Intent::Viewport`, the second, the batch per visit, the piggyback, opportunistic then mailbox; the terminal's visible area and focus; the first two cells | built, 1231, without the cells |
| R3 | the tick ladder on every target, one table in `delivery.rs`, the terminal's two cells, Android's `Glyph.java` drawn anew, the words en/de | built, 1232 |
| R4 | the switch and reciprocity: Privacy, config and palette, "does not send read receipts"; Android reports `Viewport` | |
| R5 | the home node: `/mark` "seen", the receipt from the home node's outbox, incoming receipts as archive entries, the switch on the home node | |
| R6 | the PN and mixed cells | |

What R1 does not do yet: a receipt for one of ours the screen has not
loaded (older than the store's window) is dropped like an unknown one; the
store learns only what the screen holds.
