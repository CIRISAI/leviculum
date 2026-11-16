# Loquax: read receipts

Status: decided 2026-10-31 (Lew), with the reviewer's decisions of
2026-11-01 over the study; slices R1 to R5 built (orders 1230 to 1234,
Refs #232): the wire, the mark on every message, the peer's flag, the Read
state, the read event and its sending, the tick ladder on every target,
the switch with its reciprocity, and the home node signing the receipts of
its devices. The
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
| A contact without receipts | stays at one grey tick (its proof) or two (its device's `0x07`), the detail view says it sends no read receipts | never pretend |
| What two ticks mean (1462) | one tick is the proof, two the recipient's device saying it has it (`0x07`), read the accent | the recipient's mailbox signs the proof with the recipient's identity, so a proof cannot say "on the device" |
| Publish the namespace | yes, on this page | so other clients can adopt it; an offer, not an upstream report |
| Groups | built (1307, 1466) | read once every member that sends receipts read it, "read by 2 of 3" in the Info; one tick once every copy is proven, two once every copy is on its member's device (section 6) |

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

### The device receipt

Capability bit 2 and key `0x07` say "these messages of yours are on my
device". Built first for post that arrived through the propagation network,
which has no proof the sender ever sees
([receipts for propagated post](loquax-propagation-receipts.md)); since
1462 for every message on the device, direct, propagated or pulled from the
recipient's mailbox. The recipient's device sends it, not switchable, only
to a sender whose message set bit 2. Every message is acknowledged that
way except a bare receipt, one with no text and no field but the `lev`
pair, so an answer that carries a receipt gets its own. A client of a mailbox holds no
identity, so it hands its mailbox an arrived mark and the mailbox signs the
receipt; the mailbox never sends one on its own, archiving is no device.
Section 6 says why the proof is not enough.

### The writer key and relays

Key `0x08`, `bin16 writer` (1482), names the address that wrote the map.
Every map a Loquax device sends carries it, and a mailbox names itself
in the receipts it signs. A reader takes a map as the word of the
message's source only when the writer is that source. A map without the
key, from a writer before 1482, still counts as the source's, unless the
text starts with a relay's prefix `Name\n<hash>\n` naming another
address (`leviculum-lxmf/src/relay.rs`, `own_lev`).

Why: a relay bot such as LXMF-Tools' `lxmf_distribution_group_extended`
passes a member's fields through unchanged into the copies it signs itself.
Taken as the bot's, the member's mark would make every Loquax member send
the bot device and read receipts, and the bot would hand each of them back
to everyone as an empty post. So a relay gets no receipt, and a ticket
passed through is not remembered as the relay's (`router.rs`,
`remember_verified_ticket`). The capability was not moved into the announce
(see "What was not chosen"): a mark naming its writer is enough to tell a
relay's copy apart.

### The group keys

Group chats (`docs/drafts/group-chats-study-2026-11.md`, built from 1307)
add capability bit 1, "I understand groups", and five keys; a reader
that knows only the keys above skips them. A copy a group's home sends a
member that understands groups, writer `11…11`, its clock
1700000000123 ms, epoch 1 (`VEC-MSG-LEV-GROUP-COPY`, 33 bytes of map):

```
cc fc 82 00 03 02 93 c4 10 <16-byte writer> cf 00 00 01 8b cf e5 68 7b 01
```

`0x04` is the invitation `[bin16 group, bin64 public key, str name,
str admin's name, uint epoch]` (`VEC-MSG-LEV-GROUP-INVITE`); the study
wrote `bin32` for the key, but an identity's public key is 64 bytes, as
the `lxma://` code carries it. `0x05` is the membership `[uint epoch,
[[bin16 address, str name, uint flags], ...]]`, flags bit 0 left, bit 1
admin, bit 2 joined (`VEC-MSG-LEV-GROUP-MEMBERS`); a member that leaves
sends itself alone, flagged left (`VEC-MSG-LEV-GROUP-LEAVE`), and the
admin removes a member by sending the group that member alone, flagged
left. Every Loquax client sets bit 1 on every message it sends, with
read receipts off too (`{0x00: 2}`). `0x06` is the digest
`[[bin16 writer, uint64 ms, str text, uint left_out?], ...]` a group's
home sends a slow member in place of one copy a line
(`VEC-MSG-LEV-GROUP-DIGEST`); its text is the same lines as
`"Name: text"`, one a line, which is all a stock member gets. A line that
stands for what the home left out carries a fourth element: 1 a photo,
2 a voice message, 3 a file, each too large for the radio, 4 a message
no longer there; its text is then the English sentence ("sent a photo
(too large for the radio)", the caption after `": "`). Every other line
keeps three elements, and a reader before it reads the first three.

Every copy, to a member that understands groups or not, carries the
text as `"Name: text"`, one wire shape for all: a Loquax reader takes the
writer from `0x02` and the name from the membership and strips exactly
`"Name: "`, so a name with a colon in it stays whole; the prefix is what
still names the writer when the field is lost. A copy without words
says what it carries and from whom instead, `"Photo from Lew"`, `"Voice
message from Lew"`, `"File from Lew"` or `"Position from Lew"`, where it
said a bare `"Lew:"` before (1388, 2026-11-04). A Loquax reader knows the
sentence by `0x02`, the writer's name and the kind the fields carry and
draws the bubble without it: the photo under its writer, and on the
terminal `"Lew:"` over it as every copy names its writer there.

Wire text is English. Localisation happens on the reader from the lev
data. The invitation's text, "Lew invited you to Familie: lxma://…
Reply /leave to leave.", is English whatever the admin's language, and
so is every other text a home or a client composes without a human
typing it (the group's own lines, a digest's prefixes and left-out
lines); a stock client shows that English. A Loquax reader renders the
card from `0x04`'s admin's name and group name in its own catalogue
("Lew lädt dich zu Familie ein" on a German phone), the membership lines
and the end from `0x05` ("Anna ist beigetreten."), and a left-out digest
line from `0x06`'s fourth element; the English text stays the message's
body, archived for search and for every other client.

`0x03` is the group status a home sends the writer of one message
`[bin32 original, uint delivered, uint read, uint members, uint
readers]` (`VEC-MSG-LEV-GROUP-STATUS`): the writer's own message id, how
many copies their members proved, how many a member said it read, how
many were made, and how many of those members send read receipts. The
study wrote four elements; `readers` is appended so "read by N of M"
counts only the members whose receipts are on, and a member with them
off is never waited for. The home sends it, empty with the pair alone,
once the last copy is proven (the writer's one tick, since 1466), again
when a late copy is proven after a digest, when the last reader read it
(the writer's read mark), and since 1466 when every member whose device
says so has said it. A status of four elements is no status.

1466 appends two elements: `uint on_device`, the copies whose member's
device said it has them (its `0x07` to the group, or its read receipt,
which includes it), and `[[bin16 member, uint proven_at, uint
on_device_at, uint read_at], ...]`, each counted member's rungs in unix
seconds, 0 for not yet. A status of five elements is a home's from before
1466; it never says a device, so its writer stays at one tick. A reader of
five skips the rest, as for any appended element.

A deleted group's last membership carries a third element, the standing
`1` (ended): `[uint epoch, [[...], ...], 1]`, every member flagged left
(`VEC-MSG-LEV-GROUP-ENDED`). A membership without it is a live group's,
so every membership written before keeps its bytes; a reader that does
not know the element skips it, as for any appended one.

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

### The announce aspect `lev.position`

The namespace has one destination of its own beside the field pair
(order 1301): `lev.position`, app name `lev`, aspect `position`, a SINGLE
destination of a node's identity. Its announce carries where the node is,
so a map shows the nodes of a mesh without a message. Its own aspect and
never the `lxmf.delivery` announce's `app_data`, which every reader parses
as LXMF's list (`reference/LXMF/LXMF/LXMF.py:151-172`); a Python node
stores and relays an announce whose name hash it has no handler for and
does nothing else with it.

```
app_data = map {                    plain; sealed is bin(encrypt(this map))
    0x00: int    latitude,  degrees times 1e7
    0x01: int    longitude, degrees times 1e7
    0x02: int    altitude, metres                       when the fix has one
    0x03: uint   ground speed, centimetres a second     when the fix has one
    0x04: uint   course over ground, degrees times 100  when the fix has one
    0x05: uint   age of the fix when the announce left, seconds
    0x06: uint   source: 0 GNSS, 1 manual
    0x07: str    the node's display name                when it has one
}
```

- **Two forms, told apart by the first byte.** Sealed, the default, is a
  `bin` (`0xc4`) holding the map encrypted to one identity the way
  Reticulum encrypts to a SINGLE destination (`Identity::encrypt`:
  ephemeral X25519 key, AES-256-CBC, HMAC-SHA256); whoever holds that
  identity's key opens it, nobody else learns anything but its length.
  Plain is the map itself, the opt-in "share my position with everyone
  nearby".
- **The same rules as the field pair**: a reader skips a key it does not
  know, a key never changes meaning, a repeated key resolves to its last
  occurrence. A map without both coordinates, or one out of range, is no
  position; a known key of the wrong type makes the whole map none.
- **Authentic as the identity**: the announce's signature covers the
  `app_data`, so the position is the node's own statement.
- **The age, not a time**: the announce carries its own emission time;
  the fix is `0x05` seconds older than that.

The codec is `leviculum-lxmf/src/lev_position.rs` (`Position`,
`AppData::parse`, `open`), which a reader decodes with as the board
encodes with. When a board sends it, and to whom it is sealed, is
[Announce cadence, the position announce](announce-cadence.md#the-position-announce).

## 2. Gating and reciprocity

The message that is read carries its sender's mark. When B reads a marked
message of A's, B knows in that moment that A understands receipts and
wants them; an unmarked message (a foreign client, or Loquax with receipts
off) never causes one, and is never named in one: a stock client's plain
reply beside a marked message stays unnamed though the marked one makes
the peer take receipts (`owes_receipt`, `loquax/src/ui/receipts.rs`, 1322).
No detection in advance.

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
  blocked one, or with the switch off. A client of a home node signs
  none: it hands the home node a seen mark instead (section 8).

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
- **The reply from a notification (1506).** Whoever answers has read what
  the notification showed. The phone's message notification carries an
  inline reply; it sends `{"t": "noticereply", "peer", "key", "body"}`,
  `key` the row's `notice_key` the notification was posted with (the
  newest message of the peer's, `UiModel::notice_key`). The core's
  `UiEvent::NoticeReply` (`UiModel::notice_reply`,
  `loquax/src/ui/receipts.rs`) marks every message of the chat up to that
  key seen through `UiModel::mark_seen`, the one function the viewport's
  second calls too, reads the unread up to it (`UiModel::read_up_to`), and
  sends the reply, on which the receipt rides as on any reply; a client of
  a mailbox hands the seen mark instead. A message that came after the
  notification stays unread and unseen. The open chat, its cursor and its
  draft stay as they were. Cells: `a_reply_from_the_notification_*` in
  `loquax/tests/device_receipts.rs` (the sender sees read, a later message
  stays unread, the mailbox client's seen mark, the switch off).
- Not yet: the window reports no viewport, so it sends no receipts; the
  desktop notification (window and terminal) carries no actions, so there
  is no reply from it.

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
| Arrived (1462) | the recipient's identity proved it, its mailbox or its device (proof, or the propagation node saw it collected) | `TICK` | `✓ ` | `v` | grey | "Arrived" |
| Delivered | on the recipient's device: its device receipt, `lev` `0x07` | `TICKS` | `✓✓` | `vv` | grey | "On the recipient's device" |
| Read | read | `TICKS_READ`, heavier stroke | `✔✔`, bold | `VV` | accent | "Read at 10:32" |
| Rejected, Failed | not sent | `FAILED`, red circle with `!` | `! `, bold | `!` | red | "Not sent. Double tap to send again" |

No eye, no ratchet glyph. One grey tick is the end state for every
contact that sends no device receipt (a stock client, an older Loquax), and
two for one whose device says so without read receipts, which is honest:
nothing is claimed that is not known. The accent ticks carry weight as well as colour (Android stroke 2.6
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
  `state_mailbox`, `state_arrived` (1462), `state_delivered`, `state_read` (with the reader's time)
  and `state_failed`, in English and German; the scene's `MarkNode`
  carries them, the phone's `Ui.state` reads them. The detail view says
  "sent; nobody has confirmed it arrived yet", "in the mailbox, delivered
  once [name] is online", "arrived: [name]'s mailbox or device has it",
  "on [name]'s device: it said so" (1462),
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

**A read at no known time (1506).** An imported message its source kept as
read carries no time (the archive entry holds the state code 8 alone).
`DeliveryState::Read { at: 0 }` is that read: the store keeps
`messages.read_at` NULL for it (the column takes NULL since schema 9, so
no schema step), every screen says "Read" (`state_read_untimed`,
"Gelesen") without a time, and the Info sheet has no read-time row. The
import never invents a time; one the store learned since (a receipt for an
imported message) survives a later import's new layout of the block. Test:
`an_imported_read_message_has_no_read_time_and_never_the_epoch` in
`loquax/src/store.rs`.

Not yet: the M9 has no Loquax screen yet, its renderer will read the same `Mark` and `Shape`.

### The ticks tell the truth (1462)

Measured on 2026-11-06 (1445, finding 25): the double tick came 0.4 s after
a send to a recipient with a mailbox. The mailbox proves a message once it
archived it (`loquax/src/home/daemon.rs`, `Command::Prove`), with the
recipient's identity, so the writer cannot tell its proof from a device's;
two ticks for a proof said "on the device" when nothing was on a device.

- **One tick is the proof** (`DeliveryState::Arrived`,
  `leviculum-ui/src/delivery.rs`; `loquax::ui::delivery_of` maps the
  router's `Delivered` to it): "Arrived", neutral, at the mailbox or the
  device. A propagation node's `collected` is the same rung.
- **Two ticks are the device's word**, `lev` `0x07`
  (`loquax/src/ui/arrival.rs`): a device sends it for every message whose
  mark carries bit 2, however it came, after the batch window or on a
  reply; read wins. One rule, no guess about who has a mailbox: a
  recipient without one sends it as well. A client of a mailbox hands its
  mailbox an arrived mark (`MarkFlag::Arrived`, `loquax/src/home/wire.rs`,
  code 3) and the mailbox signs, the first device's mark winning
  (`loquax/src/home/server.rs`, `arrived`). History a new device's first
  pull hands it owes none. Without bit 2 in the writer's mark, or from a
  stock recipient, two ticks never come, and no timer turns one into two.
- **Read** (`0x01`) stays the accent ticks.
- **The Info sheet names the times**: Sent, Arrived, On the device, Read,
  each once known (`loquax/src/ui/info.rs`). The ledger line of a proof is
  `arrived: proof received`, of a device receipt `on the recipient's
  device: its receipt`.
- **Kept from before.** A stored message at two ticks whose ledger shows a
  proof, a mailbox-signed propagation receipt or a collection, and no
  device receipt, loads at one tick (`arrival::before_1462`); a group's
  status keeps its own.
- **Cells** (`loquax/tests/device_receipts.rs`, and the stock Python
  recipient in `loquax/tests/home_node.rs`): a mailbox-backed recipient
  shows two ticks only after its device pulls; without the capability one
  tick forever; without a mailbox two ticks on the device's receipt; a lost
  receipt stays one tick, a week on too; a stock recipient one tick on its
  proof, never two. The scene golden `scene-ladder.json` pins every rung,
  one tick and two, for the four targets that draw the scene.
- **Groups (1466)** tell the same truth. Every member's copy proven is one
  tick, "Arrived"; two ticks, "On the devices", come only once every
  member's copy is on its member's device; read is unchanged (every member
  that sends receipts read it). A group's copy carries its writer's bit 2,
  so a member's device answers the group with `0x07` (a client of a
  mailbox through its mailbox's signature, as above); the home books it
  (`group_fanout.device_at`, store schema 16) and counts it in the
  status. A member whose device never says it (a stock client, whose copy
  has no `lev`, or a Loquax without bit 2) keeps the writer at one tick;
  so does a lost receipt, and no timer flips it. The Info sheet has a row
  per member with its rung and since when. A group line stored at two
  ticks from a status that counted proofs loads at one
  (`groups::before_1466`). Cells: the group cells in
  `loquax/tests/device_receipts.rs` and the stock Python member in
  `loquax/tests/home_node.rs`; goldens `scene-group-ticks.json`,
  `tui-group-ticks.txt`, `gui-group-ticks.png`, `m9-group-ticks.png` and
  the phone's `group-ticks.json`.

## 7. Settings (built, R4)

- **One switch**, "Send read receipts" ("Lesebestätigungen senden"), with
  one line under it: "Others see when you have read their messages. Off,
  you do not see it from others either." On by default. The words are the
  catalogue's `read_receipts` and `read_receipts_text`
  (`loquax/src/ui/strings.rs`), so every target says the same.
- **What off means** (`UiModel::set_receipts`,
  `loquax/src/ui/receipts.rs`): no mark on our messages, no receipt sent
  (one being gathered is dropped), and none received is shown: a receipt
  that comes while off is logged (`RECEIPT ignored`) and not kept, and a
  message read before shows two grey ticks (`shown_delivery`,
  `loquax/src/ui.rs`), its stored state kept. On again restores all
  three; a message read before shows read again.
- **Where it is.** Android: Settings › Privacy, the first row, above the
  blocked contacts, a switch (`privacy_receipts`); the page asks the core
  with `{"t": "receipts", "set"?}` and shows what it answers
  (`ReceiptsChoice`). The window: the gear on the list's top bar opens a
  settings page with the same row and a toggler. The terminal has no
  settings page; the palette's `read receipts` turns it off and on and
  says which. Each writes the config's `read_receipts = on|off`
  (`UiEffect::Remember`), which the next start reads. The M9 has no
  settings page yet and keeps the default, on.
- **A contact without receipts** (a foreign client, or Loquax with the
  switch off; the wire does not tell them apart): our messages stay at two
  grey ticks, and the detail view says "[name] sends no read receipts".
  With our own switch off it says "read receipts are off: you send none
  and see none". Neither says "unread" or anything that looks like
  waiting.
- **With a home node** each device keeps its own switch in its config for
  now: off, a device sends no seen mark and shows no read. The study puts
  the switch on the home node, as the identity's; that move is not built
  (section 8).
- No Reticulum words on the way: "read receipts", "read", "delivered",
  "mailbox". "Propagation node", "proof", "ratchet" at most in the detail
  view's technical part.

## 8. With a home node

Built (1234). The devices do not hold the LXMF identity
(`loquax-home-node.md`, Roles and keys), so the home node signs the
receipt.

- **The device says what it saw.** The read event and the batch are the
  same as on a device of its own (section 5); when the receipt would go
  alone, a client of a home node hands the home node a seen mark instead:
  `/mark [2, [id, ...], read_at]` (`MarkFlag::Seen`), with the reader's
  clock at the first bubble seen. "Read" (flag 0) stays what it was, the
  counter's "everything up to here is read". Nothing rides on a reply from
  a home client and a draft holds nothing up: the reply is signed by the
  home node, which keeps no draft.
- **The home node signs.** For each id no seen mark in its archive names
  yet, the home node looks up the received message; each sender whose
  message carried the mark gets one receipt alone, signed with the home
  identity, batched by seven as everywhere, put into the outbox and
  delivered as every message a client hands in, direct first and then by
  the propagation node (`loquax::home::server`, `seen`). A message of a
  stock client, an imported one, or one the archive's cap evicted is seen
  and gets none. A home node from before 1234 refuses flag 2 as malformed,
  and nothing goes.
- **The first device wins.** The home node keeps a seen mark of the new
  ids alone, at the first device's clock; a mark with nothing new appends
  nothing and sends nothing. Every device applies a seen mark on its next
  pull (`UiModel::home_seen`): the message is seen in its ledger and leaves
  the batch it gathers, so a second viewport sends nothing new.
- **Incoming receipts** are received messages like any other: the home
  node archives them, every device pulls them, and they read our message as
  on a device of its own (section 4). The receipt the home node signed
  comes back to the devices as its sent entry and is no bubble. A bubble a
  device learns from another device's send carries the send's own time, so
  the read time is kept within the right bounds there too.
- **Not built:** the switch on the home node. Each device's own switch
  decides whether it hands in seen marks and shows read; the home node signs
  for whichever device asks.

Tests: `loquax/tests/mailbox_receipts.rs` drives the home node's answers
over its store with real identities (one receipt with the home's
signature and the right ids, the first device wins, a receipt arriving
reads the message on every device, a read mark sends none), and the
model's tests in `loquax/src/ui.rs` hold the device's side.

## 9. Proof of compatibility

In the cargo run: the vectors and `lev_vectors.rs` (section 1), the
model's tests in `loquax/src/ui.rs` (a receipt alone is no message, the
latest message decides the peer's flag, only the destination reads; the
switch's round trip, a message sent while off without the mark, a receipt
received while off changing nothing shown, the detail view's lines), the
store's round trip and migration of schema 9, and the phone's rows in
`loquax-android/tests/facade.rs`.

periculum cells, to come with R2 and R6:

| Cell | Setup | Holds |
|---|---|---|
| `loquax_tui_receipts_stock` | Alice `loquax tui`, Bob the Python `lxmf_node.py` | Bob receives Alice's text with the pair as specified; Bob's message read by Alice brings Bob nothing but Alice's texts; Alice's bubble stays at one grey tick (1462). Positive control: Bob sends with a mark set by hand, then exactly one receipt arrives |
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
| R4 | the switch and reciprocity: Privacy, config and palette, "does not send read receipts"; Android reports `Viewport` | built, 1233, without Android's `Viewport` |
| R5 | the home node: `/mark` "seen", the receipt from the home node's outbox, incoming receipts as archive entries, the switch on the home node | built, 1234, without the switch on the home node |
| R6 | the PN and mixed cells | |

What R1 does not do yet: a receipt for one of ours the screen has not
loaded (older than the store's window) is dropped like an unknown one; the
store learns only what the screen holds.
