# Group chats hosted by a mailbox: a study

Order 1299, Lew 2026-11-01. Status: study, nothing built. Read first: the
reviewer's paper "Loquax: Zuhause, Mesh und Nutzer" (**the paper**,
section 4), the study of order 1296
([mailbox-for-friends-study-2026-11](mailbox-for-friends-study-2026-11.md),
**1296** below), [loquax-mailbox](../src/concepts/loquax-mailbox.md) and
[loquax-read-receipts](../src/concepts/loquax-read-receipts.md) (the `lev`
namespace).

Paths: `LXMF/` is `reference/LXMF/LXMF/`, `RNS/` is
`reference/Reticulum/RNS/` (1.3.5). The foreign clients were cloned for
this study into a scratch directory, shallow, at these commits; their
paths are given from the repository root:

| client | repository | commit |
|---|---|---|
| Sideband | `markqvist/Sideband` | `ef572b5` (2026-10-01) |
| Columba | `torlando-tech/columba` | `767bf2e` (2026-11-01) |
| NomadNet | `markqvist/NomadNet` | `b2f852c` (2026-09-30) |
| MeshChat | `liamcottle/reticulum-meshchat` | `a9f9259` (2026-10-07) |

MeshChatX (`Sudo-Ivan/reticulum-meshchatX`) could not be cloned (the
forge asked for credentials, so the path has moved or gone private) and
is **not checked**. Lew's own Columba checkout on this host is from
2026-08-14 (`6674ae87`); the fresh clone is the one cited.

## 0. The answer in short

- **Verdict: build it for Loquax, with the list interop for stock
  clients, hosted by one home first.** A group hosted by a single always-on
  mailbox needs nothing from the N-home experiments: one home has one
  ratchet ring and one path, so neither loss of 1296 (1.1, 1.2) can occur.
  When N homes lands, a group becomes one more identity those homes carry,
  with one added rule (2.4). Do not wait for E1 to E3; do not go
  Loquax-only, because the list costs stock members nothing and the
  reviewer's shape needs no stock change.
- **The reviewer's shape, item by item:** 1 confirmed with two
  corrections (no group id in the field, no home-issued sequence number),
  2 confirmed with a breakage list, 3 confirmed with the trust sentence
  below, 4 counted, 5 computed and one rule changed (the home cannot see
  "LoRa", it measures the member), 6 corrected (ordering by home
  acceptance time, dedup by LXMF's own message hash plus one key), 7 the
  slices of section 7.
- **Taps** (section 4): create a group of k friends **4 + k taps and a
  typed name**, Signal the same; join **2 taps** (Signal 0 to 1); leave 3
  (Signal 4); admin adds k: 3 + k (Signal 4 + k); admin removes one: 4
  (Signal 4). A stock Columba member joins in 2 taps, a Sideband member in
  0 (the group simply appears as a conversation) or about 5 to name it.
- **Interop:** a stock member sees a contact "Familie (Gruppe)" whose
  messages read "Lew: …"; it replies to that contact and the home fans
  out. The name prefix is the right choice, not nothing: Columba keeps no
  title (`MessageEntity`, 3.1), so the content is the only place a name
  survives in every client. What breaks for stock members: reactions
  (not sent to them in v1), read receipts (they send none, so they never
  count), "who is in the group" (a text line, no list); what survives:
  replies in Columba (the home rewrites the reply id per recipient),
  photos with captions, order, dedup.
- **Airtime** at the default PHY (869.463 MHz, SF8, BW125, CR 4/5, 18
  preamble symbols): a 31-byte line from a member to the group is 243 B,
  697 ms, plus the 288 ms proof; each copy to a Loquax member is 275 B,
  852 ms (two LoRa frames), to a stock member 243 B, 697 ms. **A group of
  five, all four others behind one LoRa interface: the home transmits 3.4
  s and the channel carries 5.5 s per line.** Under the 10 % lock of band
  P that is **105 lines an hour** from the home's radio; on a 1 % band
  10; at SF12 8 and 0. Hence the digest for slow members (5.3).

## 1. What exists today

### 1.1 LXMF

- `FIELD_GROUP = 0x0B` is allocated (`LXMF/LXMF.py:18`) and used nowhere
  in the reference: a grep over `reference/LXMF` for `FIELD_GROUP` finds
  the definition only. No semantics, no format, no client reads it
  (1.2). Taking it would mean writing a format into the reference's
  namespace that upstream never defined; the `lev` pair is ours
  (`loquax-read-receipts.md`, section 1). **The group field lives in
  `lev`.**
- `FIELD_EMBEDDED_LXMS = 0x01` (`LXMF/LXMF.py:8`) would let the home
  forward the member's original, signed message. It costs the whole
  original (at least 96 bytes of source, signature and framing) per
  copy, and no stock client renders it. Not chosen for v1 (2.1, sender
  authenticity).
- A message whose source the receiver does not know is still delivered,
  unverified: `unpack_from_bytes` sets `SOURCE_UNKNOWN`
  (`LXMF/LXMessage.py:814-816`), it does not drop. Who drops is the
  client (1.2, Sideband's switch).
- Duplicates are dropped per router by message hash
  (`LXMF/LXMRouter.py:1906-1911`, 1296 1.4). Section 6 builds on that.

### 1.2 What the other messengers call a group

| client | groups | evidence |
|---|---|---|
| Sideband | **none.** A conversation type `CONV_GROUP = 0x02` is declared (`sbapp/sideband/core.py:107`) and never used: a grep for `CONV_GROUP` over `sbapp` finds only that line; conversations are made `CONV_P2P` (`core.py:2798`) | `grep -rn -E 'FIELD_GROUP\|CONV_GROUP' sbapp` |
| Columba | **none.** `DestinationType.GROUP` (`rns-backend-py/.../PythonRnsCore.kt:1466`) is the RNS destination type, not a chat; no `FIELD_GROUP`, no group conversation in `app/`, `data/`, `rns-backend-kt/`. What Columba calls a group is the **Group Tracker** ("Group Tracker shares your location with a group host", `app/.../SettingsScreen.kt:749`; the card in `settings/cards/LocationSharingCard.kt:646`): Sideband's telemetry collector under another name, positions sent to one host and fetched back by `FIELD_COMMANDS` 0x01 (`PythonRnsTelemetry.kt:134`), no messages | `grep -rn -i -E 'group ?chat\|groupconversation\|isGroup\|FIELD_GROUP'` |
| MeshChat | **none found.** The only hits are an interface's `group_id` (`meshchat.py:475`, `AddInterfacePage.vue:1021`) | `grep -rn -i -E 'group ?chat\|FIELD_GROUP\|groups'` over `*.py *.js *.vue`, `node_modules` excluded |
| NomadNet | **RRC, a live chat over a hub, not LXMF.** "Built-in RRC client for live, many-to-many chat" (`README.md:40`). The client opens an `RNS.Link` to a hub destination (`nomadnet/RRC.py:428`), sends CBOR envelopes as link packets (`RRC.py:595-603`), joins rooms by name (`RRC.py:605`); the protocol constants are copied from `kc1awv/rrcd` (`RRC.py:41`). History is the client's own file per room (`RRC.py:14`). Whether the hub keeps messages for an absent member is the hub's business (`rrcd`, not read here); the client has no store-and-forward path | `grep -rn -i group nomadnet` |

So no Reticulum messenger has an LXMF group today. RRC is the only
many-to-many chat, and it is IRC: present or absent. A mailbox-hosted
group is the first that delivers to an absent member, and it reaches
every LXMF client without any of them changing, which RRC cannot.

### 1.3 How a stock client shows a message (the facts the interop rests on)

- **Conversation key = source hash.** Sideband files every message
  under `message.source_hash` (`sbapp/sideband/core.py:5205`); Columba
  under `conversationHash`, "Peer's destination hash"
  (`data/src/main/java/network/columba/app/data/db/entity/MessageEntity.kt:35`).
  A message the home sends **as the group** lands in one conversation,
  the group's.
- **The title.** Sideband shows a non-empty title as a "Title" line in
  the bubble heading (`sbapp/ui/messages.py:377-378`). Columba's
  `MessageEntity` has no title column (`MessageEntity.kt:33-60`): a title
  is gone. **Only the content carries a name everywhere.**
- **Unknown source.** Sideband drops a message from a sender it has no
  conversation with only when `lxmf_ignore_unknown` is set, default off
  (`core.py:475`, the check `core.py:5206-5209`). Columba's
  `blockUnknownSenders` defaults to off
  (`app/src/main/java/network/columba/app/viewmodel/SettingsViewModel.kt:167`).
- **Order.** Sideband sorts by receive time (`ORDER BY rx_ts`,
  `core.py:3003`); Columba by `COALESCE(receivedAt, timestamp)`
  (`data/src/main/java/network/columba/app/data/db/dao/MessageDao.kt:20`).
  Both show arrival order.
- **Dedup.** Sideband keys messages by `lxm_hash PRIMARY KEY`
  (`core.py:2085`); Columba by "Message hash" (`MessageEntity.kt:34`).
- **Replies.** Columba reads `fields[0x30]`, the replied message's hash
  (`app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:116`,
  `:136`). Sideband has no reply: a grep for `REPLY` over
  `sbapp/ui/messages.py` and `sbapp/sideband/core.py` finds nothing.
- **Contact codes.** Columba reads `lxma://` (`AndroidManifest.xml`,
  `app/src/main/java/network/columba/app/util/IdentityQrCodeUtils.kt`);
  Sideband does not (`grep -rn lxma sbapp`: nothing).
- **Unknown `lev` keys are invisible** in every client checked
  (`loquax-read-receipts.md`, "What other clients do with it"). An
  empty message carrying only fields is not: Sideband makes a
  conversation and an empty notification, NomadNet an empty line. So no
  copy to a stock member may be fields-only.

## 2. The model (item 1: confirmed, two corrections)

### 2.1 A group is an identity hosted by the admin's homes

- The group has its own RNS identity and its own `lxmf.delivery`
  destination; its address is the group's address. The admin's home
  makes it and holds the private key. The store already keys the archive
  by identity (`archive(identity, kind, id, …)`, `loquax/src/store.rs:219-225`),
  so a group is one more identity in the same tables, as the friend's
  identity of 1296 (4.1) would be.
- **Members never hold the group key.** Only homes of the admin do. This
  is what makes removal complete (4.6) and what makes the host able to
  read (3).
- Members write to the group's address like to any contact. The home
  **proves after archiving**, as the mailbox does today (1296, 2.7
  invariant 5), so the sender's proof means "the group has it", and it
  is the group identity's signature (`RNS/Identity.py:966-971`), valid
  from any home of the group (1296 1.3). **Confirmed.**
- The home sends one LXMF message per other member, **as the group**
  (source = group address), through the home outbox it already has
  (`home_outbox`, `loquax/src/store.rs:247`): direct first, then the
  member's propagation node, with that member's stamp paid.

### 2.2 The fan-out field

The reviewer's field held group id, sender, sequence number and member
list hash. Two of the four go:

- **No group id.** The copy's LXMF source **is** the group (2.1); a
  group id beside it is 18 bytes of repetition on every copy, 85 ms at
  SF8. **Corrected.**
- **No sequence number from the home.** With N homes two partitioned
  homes would hand out the same number (6.1). **Corrected**; ordering and
  dedup take other keys (section 6).
- **Kept:** the sender, and a membership epoch instead of a list hash
  (one varint, not 8 bytes; it says the same: "your list is stale").
- **Added:** the sender's own timestamp, the dedup key of 6.2.

```
fields[0xFB] = bin "lev"
fields[0xFC] = map {
    0x00: uint   capabilities; bit 0 read receipts (as today), bit 1 "I understand groups"
    0x02: [ bin16  sender's LXMF address,               in group copies only
            uint64 sender's timestamp, milliseconds,
            uint   membership epoch ]
    0x03: [ bin32 original id, uint delivered, uint read, uint members ]   group status, home to sender
    0x04: [ bin16 group address, bin32 group public key, str name,
            str admin's name, uint epoch ]                                  invitation
    0x05: [ uint epoch, [ [ bin16 address, str name, uint flags ], ... ] ]  membership, home to Loquax members
    0x06: [ [ bin16 sender, uint64 ts_ms, str text ], ... ]                 digest, home to a slow member (5.3)
}
```

Keys 0x00 and 0x01 are today's (`leviculum-lxmf/src/lev.rs:34`, `:37`);
the read-receipts page's rule holds: a reader skips a key it does not
know, and a key never changes meaning. Bit 1 rides beside bit 0
(`CAP_READ_RECEIPTS`, `lev.rs:40`); a Loquax with receipts off sends
`{0x00: 2}` and still gets groups. Vectors written by the reference, as
`gen_lev_vector` does today, are part of slice G1.

**Sender authenticity.** The copy is signed by the group, not by the
sender: a member trusts the host that "Lew" wrote it, exactly as it
trusts the host not to read (3). Carrying the sender's own signature
(64 bytes plus the original's framing, about 330 ms per copy at SF8) or
the whole original in `FIELD_EMBEDDED_LXMS` is the step that removes that
trust; it belongs with the sender-key layer of section 3, not in v1.

### 2.3 Proofs and receipts (item 1, the bookkeeping)

- **One tick** for the sender: the group's proof (2.1).
- **Two ticks:** the home has a proof from every member's copy. The home
  tells the sender with one `0x03` status message when the last copy is
  proven, not per member.
- **Read:** a Loquax member reads a copy and sends a read receipt
  (`0x01`, unchanged) **to the group**, naming the copy's id; the copy
  carried the group's mark with bit 0 when the original sender's did, so
  gating and reciprocity are today's rule. The home maps copy id to
  original (5.4, the fan-out table) and sends the sender one `0x03` when
  every member that sends receipts has read; "read by 3 of 5" and who is
  in the detail view, fetched from the home over a link on opening it,
  not pushed on the air. Stock members send no receipts and are listed
  as "doesn't send read receipts", as the read-receipts page already
  says of a contact without them. This settles the page's parked row
  ("read once all have read"): **all who send receipts**, never all,
  because with one Sideband member "all" would never come.

### 2.4 N homes for a group (the paper's section 4)

The group's homes are the admin's homes: the server mailbox, and the
admin's phone once it is a home (1296, 2.3). Everything of 1296 section 2
applies to the group identity as to the admin's own (one ratchet
schedule, the announce rule, sibling sync). One rule is new:

- **The home that accepted (proved) a member's message fans it out**,
  with the acceptance time as the copies' timestamp. Its outbox rows are
  that home's, as 1296 2.1 says of any outbox; a home that learns by sync
  that a copy was proven stops retrying it.
- If that home is lost before its fan-out finished, another home takes
  the rows over from the synced archive and **produces the same copies
  byte for byte**: the copy is a deterministic function of the original,
  the acceptance time, the member and the group key (Ed25519 is
  deterministic, 1296 1.3), so a member that already has a copy drops the
  second one by hash (1.1). This needs our msgpack encoding of a copy to
  be canonical; slice G2 pins it with a test.

## 3. Privacy and interop

### 3.0 Privacy (item 3: confirmed)

- The group key lives at the host, so **the host can read the group**,
  as the mailbox host can read its owner's mail today and as 1296's
  friend host can. It can also write as the group and put words in a
  member's mouth (2.2). Members learn each other's addresses (2.2, 0x05),
  as Signal members see each other's numbers.
- Every announce is public. The group's announce carries its display
  name, so "Familie (Gruppe)" is visible to every node that hears it,
  though not who is in it. Decision 3 in section 9.
- A Signal-style sender-key layer (members hold a group key, the host
  fans out ciphertext it cannot read) is a later step. It **shuts stock
  clients out**, since they would receive ciphertext in a field they do
  not read. Confirmed as the reviewer wrote it.
- The trust sentence, shown on the create screen under the name field
  (no extra tap) and on the invitation's join screen:
  - at creation: "Familie is kept on your computer. Messages in the
    group can be read there." (German: "Familie liegt auf deinem
    Computer. Dort sind die Nachrichten der Gruppe lesbar.")
  - at join: "Familie is kept on Lew's computer. Lew could read the
    group's messages." (German: "Familie liegt auf Lews Computer. Lew
    könnte die Nachrichten der Gruppe lesen.")
  - an admin without an always-on home (a phone-only user): "Familie is
    kept on your phone. While it is off, messages wait." (German:
    "Familie liegt auf deinem Handy. Solange es aus ist, warten die
    Nachrichten.")

### 3.1 Interop for stock clients (item 2: confirmed, with what breaks)

What the home sends a member that has never shown bit 1 (Sideband,
Columba, NomadNet, MeshChat, an old Loquax):

- content `"<sender name>: <text>"`; the name is the one the admin gave
  that member when adding them, the same for everyone; no title; no
  `lev` group key (it would cost 32 bytes and nobody reads it);
- the copy's timestamp is the home's acceptance time (6.1);
- `FIELD_IMAGE`, `FIELD_FILE_ATTACHMENTS`, `FIELD_AUDIO` pass through; a
  photo without caption gets the content `"<name>:"`, which still names
  the sender (Sideband words the notification "Image" only when the
  text is under two characters, `core.py:4977-4978`; four characters of
  name are shown as they are);
- `FIELD_REPLY_TO` (0x30) is rewritten to **that recipient's** copy id of
  the replied message, which the fan-out table knows; `FIELD_REPLY_QUOTE`
  (0x31) passes;
- membership changes as text: "Lew added Anna.", "Anna left.".

What a stock member does: writes to the contact "Familie (Gruppe)", as to
any contact. The home fans that out with the prefix. To leave, it writes
`/leave` (the invitation text says so) or simply blocks the contact,
which the home sees as copies never proven and, after seven days of
that, as having left.

| feature | stock member receives | stock member sends |
|---|---|---|
| text | works, "Lew: …" | works |
| photo with caption | works, "Lew: caption" with the image | works |
| reply threading | works in Columba (id rewritten); Sideband has no replies (1.3) | Columba's reply id is mapped by the home |
| order | arrival order, as the home fanned out | |
| duplicates | dropped by hash; one rare case in 6.2 | |
| reactions | **not sent in v1**: a reaction is fields-only, and Sideband shows a fields-only message as an empty notification (1.3); Lew may decide to send them as text ("Lew 👍 'Bin gleich …'") | Columba's reaction is mapped like a reply |
| read receipts | none | none; counted as "doesn't send read receipts" |
| who is in the group | the join text and "added/left" lines only | |
| sender verification | the group's signature verifies once the group's announce or `lxma://` key is known; before that "source unknown", shown anyway (1.3) | |

**The name prefix is acceptable**, and better than sending stock members
nothing: Columba keeps no title, the prefix is how every bridge (IRC,
Matrix) shows a relayed speaker, and the alternative leaves a Sideband
user out of the family's group altogether. **No stock client needs a
change. Confirmed.**

## 4. The flows, counted (item 4)

Signal's numbers are its documented Android flow (new chat, "New group",
pick, next, name, create), **not measured on a device here**. Loquax's
are counted from the design; every Loquax screen named is **new**.

| flow | Loquax | Signal | stock member |
|---|---|---|---|
| create, k friends | 4 + k and a typed name (4.1) | 4 + k and a typed name | |
| invite reaches a member | 0 (sent at create) | 0 | 0 |
| join | 2 (4.2) | 0 for a contact, 1 for a message request | Columba 2, Sideband 0 or about 5 (4.3) |
| leave | 3 | 4 | 1 message `/leave` |
| admin adds k | 3 + k | 4 + k | |
| admin removes one | 4 | 4 | |
| admin's mailbox gone | 0 with a second home, else 2 to restart (4.7) | 0 (the server) | 0 |

### 4.1 Create

| # | screen | taps |
|---|---|---|
| 1 | Chats: the new-chat button | 1 |
| 2 | **new** "New group" at the top of the contact list | 1 |
| 3 | tick the friends | k |
| 4 | Next | 1 |
| 5 | **new** name field (typed), the trust sentence under it, "Create" | 1 |
| | **total** | **4 + k, one typed name** |

Beneath: the client asks its home over the existing link to make the
group (a `/group` request, slice G2); the home makes the identity,
announces it (5.2), and sends each friend the invitation. No home
reachable: the phone hosts it (the third trust sentence) and hands it
over to the server home when they sync (2.4).

### 4.2 Join, a Loquax member

The invitation is an LXMF message from the admin carrying `0x04`, with
the content as text for any client that is not Loquax ("Lew invited you
to Familie: lxma://…; reply /leave to leave"). Loquax shows it as a card,
"Lew lädt dich zu Familie ein", the trust sentence, "Join".

Languages (decided 2026-11-03, 1348): every text a home or a client
composes without a human typing it is English on the wire, whatever the
admin's or the reader's language: the invitation, "Lew ended Familie.",
"Lew removed Anna.", "Anna left.", "You are no longer in Familie.", a
digest's "Name: " prefixes and its "sent a photo (too large for the
radio)" line, the stock member's Loquax sentence. Every stock client
shows that English. A Loquax reader says the same in its own language
from the `lev` data beside it: the card from `0x04`'s inviter and name,
the membership lines and the end from `0x05`, a digest's left-out line
from `0x06`'s fourth element. The `/group` request carries no language
(the fourth element pass six sent is read past and ignored).

| # | screen | taps |
|---|---|---|
| 1 | the notification or the chat with Lew | 1 |
| 2 | "Join" on the card | 1 |
| | **total** | **2** |

The join is a message to the group with bit 1 set; the home answers with
`0x05`. Signal adds a known contact without asking (0) and asks a
stranger once (1). Loquax asks always, because the trust sentence must be
read once; that is the one tap more.

### 4.3 Join, a stock member

The home sends copies to every added member from the start; consent is
the admin's word, as in Signal for contacts.

- **Columba:** tap the `lxma://` link in the invitation text (1), save
  the contact (1): **2**, then the group has its name and verifies.
- **Sideband:** **0**: the first copy opens a conversation from the
  group's address, named by the group's announce once heard
  (`peer_display_name`, `core.py:1147`, the announced name at `:1171`). To name it before the
  announce: copy the address, new conversation, paste, name, save,
  about 5, not counted on a device.

### 4.4 Leave

Group header (1), "Leave group" (1), confirm (1): **3**. The client sends
the group a message carrying a `0x05` that lists only its own address,
flagged as gone (the exact form is fixed in G1); the home stops fanning
out to it and tells the others.

### 4.5 Admin adds k

Group header (1), "Add members" (1), tick k, "Add" (1): **3 + k**. Each
gets the invitation; Loquax members join (4.2), stock members simply
start receiving.

### 4.6 Admin removes one

Group header (1), the member (1), "Remove" (1), confirm (1): **4**. The
home stops the copies at once. **Removal is complete without rekeying**,
because a member never held the group key (2.1): a removed member can
still write to the address, the home proves (so its client stops
retrying) and drops it, and answers once with "You are no longer in
Familie." That is simpler than Signal's, which must rotate keys.

### 4.7 The admin's mailbox gone

- **With N homes:** the admin's phone is a home of the group (2.4); it
  fans out while it is reachable and a member's message goes to it by
  path. 0 taps. Until N homes lands, the group has one home.
- **Gone for good with no other home:** members' messages wait in their
  outboxes and then at their propagation nodes (the outbox's policy,
  2.1), and nothing fans out. A Loquax member holds the member list
  (`0x05`) and can "Start again with the same people" (2 taps: the button
  on the dead group's banner, then Create); stock members get a new
  invitation. History stays on each member's device.

### 4.8 A member whose only home is the phone, in the forest

The member's phone is its own home; the group's home is the admin's
server. In the forest the member has no path to the group: its messages
to the group wait in its outbox (clock icon, "waiting for Familie"),
nothing new arrives. Back in range everything is delivered both ways and
sorted as 6.1 says. If the **admin's** phone is in the same forest and is
a home of the group (2.4), the forest is a group of its own for that
time, and the two homes merge when they meet (1296 E1 decides that this
works). A member that is not the admin cannot host: it does not hold the
group key, and giving it the key makes it a co-host the host trust
sentence must then name. **The admin may make a member a co-host**
(slice G6, a later decision, 9.2).

## 5. Cost on the air (item 5)

### 5.1 The numbers

Computed with the airtime formula of `airtime_ms_with_preamble`
(`leviculum-core/src/rnode.rs:1127`) and the frame split of
`packet_airtime_ms` (`rnode.rs:1702`, one frame up to `MAX_SINGLE_PAYLOAD`
= 254 bytes, `rnode.rs:2061`), at the default PHY with the derived 18
preamble symbols (`derive_preamble_symbols`, `rnode.rs:964`; the floor
`LORA_PREAMBLE_SYMBOLS_MIN`, `rnode.rs:901`). Packet sizes are an
opportunistic LXMF single packet as the reference builds it: 19 bytes of
header, then the encryption of source, signature and the msgpack payload
(32 ephemeral key, 16 IV, PKCS7 to 16, 32 HMAC). The check against the
read-receipts page: a receipt alone with one id computes to 240 bytes of
token, the page's "about 240 bytes". The line is "Bin gleich da, bringe
Brot mit." (31 bytes).

| packet | bytes | frames | SF8 ms |
|---|---|---|---|
| the proof back | 83 | 1 | 288 |
| member to group, Loquax (mark only) | 243 | 1 | 697 |
| copy to a stock member, "Lew: " prefix | 243 | 1 | 697 |
| copy to a Loquax member, `0x02` key | 275 | 2 | 852 |
| digest of 4 lines to a Loquax member (`0x06`) | 467 | 2 | 1343 |
| digest of 4 lines to a stock member (text, one per line) | 355 | 2 | 1057 |
| the group's announce, about 200 bytes | 200 | 1 | 585 |

The `0x02` key costs 32 bytes and 155 ms per copy, and it pushes any line
over one LoRa frame (a copy with more than 7 bytes of content needs two;
a plain message fits one up to 37). Shortening the sender to 8 bytes
saves one 16-byte block; not worth a second address form.

A group of five, one member writing, the four others all behind the same
LoRa interface:

| SF | home transmits per line | channel per line (incl. proofs and the sender) | lines an hour, 10 % lock | 1 % lock |
|---|---|---|---|---|
| 8 | 3.4 s | 5.5 s | 105 | 10 |
| 10 | 11.3 s | 18.3 s | 31 | 3 |
| 12 | 44.6 s | 72.5 s | 8 | 0 |

The locks are the ETSI duty cycles our firmware derives from the
frequency: 10 % in band P, 869.4 to 869.65 MHz, where the default
869.463 MHz sits, 1 % in L, M and Q (`etsi_eu868_duty_cycle`,
`rnode.rs:1571`). "Lines an hour" is the home's own radio budget
(360 s an hour at 10 %); the channel is shared with everything else on
it, and a chat of one line a minute at SF8 takes 9 % of the channel.

**The reviewer's "four airtimes" holds only when all four are behind the
same radio.** A server home on the internet reaches internet members for
free and spends LoRa airtime only where a transport node forwards a copy
onto LoRa; there each LoRa member costs one copy and one proof.

### 5.2 What cadence and announce rules allow

- The announce rules limit announces, not data: the default cap is 2 % of
  an interface's capacity for announces (`DEFAULT_ANNOUNCE_CAP_PERCENT`,
  `leviculum-core/src/constants.rs:403`). One group adds one destination
  that announces as the mailbox's own identity does; at 585 ms an
  announce at SF8 an hourly announce is 0.016 % of the hour. Loquax and
  Columba members get the key from the `lxma://` code and need no
  announce; only a Sideband member needs one, for the name and the
  signature, and a path request answers that on demand.
- Data has no cap except the radio's airtime lock. A home that sends
  until the lock bites makes the board queue and drop everyone's
  traffic, not only the group's. **The home must not rely on the lock.**

### 5.3 Where the home refuses or batches

The home is an application; it does not know which member is behind
LoRa, and by the interface-isolation rule it must not ask the transport
"is this LoRa". It **measures the member**: the time from sending a copy
to its proof, which every copy produces anyway.

- **Slow member:** the last three proofs took more than 2 s (at SF8 a
  copy and its proof over one hop are 1.1 s; over the internet tens of
  milliseconds). For a slow member the home holds lines for **60 s** and
  sends one digest (`0x06` to Loquax, text lines "Lew: …\nAnna: …" to
  stock), so four lines cost 1.3 s instead of 3.4 s at SF8.
- **Refusal:** to a slow member no image or file above 2 KB; it gets the
  line "Lew sent a photo (too large for the radio)" instead, and fetches
  it later over a fast path if it ever has one (a later slice).
- **The sender's side:** a sender on LoRa sends one message to the group,
  never N-1. That is the gain over a client-side group (send to each
  member): the sender's airtime is constant in the group size.
- **No home-side refusal of text.** Throttling a family chat would break
  "nothing disappears silently"; the digest keeps every line and bounds
  the frames.

### 5.4 What the store needs (item 6, the store part)

| table | columns | why |
|---|---|---|
| `groups` | group identity, name, admin, epoch, created_at | the group itself; the key in the existing identity store |
| `group_members` | group, address, name, flags (Loquax bit 1 seen, slow, co-host), added_at, left_at | membership, the epoch's content, the stock/Loquax split |
| `group_fanout` | group, original id, member, copy id, state, sent_at, proven_at, read_at | copy id to original (replies, reactions, receipts), the two ticks, "read by" |
| archive rows | kind "group message", id = the original's LXMF id, `at` = acceptance time | the history; synced by 1296's paging as any archive entry |

The outbox (`home_outbox`) carries the copies as it carries any message;
`group_fanout` is the bookkeeping over it.

## 6. Ordering and dedup (item 6: corrected)

### 6.1 Order

- A home-issued sequence number does not survive N homes: two homes in a
  partition (the server and the admin's phone in the forest, 4.8) would
  both issue 17. A per-home counter is a pair (home, n) and orders
  nothing across homes.
- **The order is the home's acceptance time**, written as the copies'
  LXMF timestamp. For one home that is the order the group happened in,
  the same for every member, as Signal's server order. Stock clients sort
  by arrival anyway (1.3), which is the home's order unless a copy was
  retried late.
- Loquax sorts a group by the copies' timestamp. A copy that arrives
  after newer ones is placed by its time and gets a small "arrived late"
  under it, so nobody misses it above the fold; the chat's unread marker
  counts it.
- After a partition of two homes, each home's messages are in its own
  order, interleaved by time when they meet. A reply that crossed the
  partition may sit before its question for the members that saw both
  halves late; the reply's quote (0x31) keeps it readable.

### 6.2 Dedup

| case | what happens |
|---|---|
| the same copy twice (outbox retry, a second home's takeover, 2.4) | byte-identical copy, same hash, LXMF drops it (1.1); every client |
| a member's message accepted by **two** homes (the sender retried after a lost proof and its path flipped to the other home) | two acceptances, two acceptance times, two different copies. Loquax drops the second by the `0x02` pair (sender, sender's timestamp); the homes drop it at the next sync by the original's id and stop its fan-out. **A stock member may see it twice** in that window; rare (one lost proof and a path flip in the same minute), stated, not hidden |
| a stock member's message accepted twice | the same, with the original's id at the homes; no `0x02` from a stock sender, so the homes use the original's LXMF id, which is the same for both acceptances |

## 7. Slices and cells (item 7)

| slice | what | passes |
|---|---|---|
| G1 | the wire: `0x02` to `0x06` and bit 1 in `lev.rs`, vectors from the reference, the canonical copy encoding | 1 |
| G2 | the home: group identity, `/group` create, accept and prove, fan-out over the outbox, the stock prefix, reply and reaction id rewrite, `/leave`, removal | 2 |
| G3 | the client in the core, then the four targets: "New group", the group chat with sender names, the invitation card, members, add, remove, leave, the trust sentences | 2 to 3 |
| G4 | proofs and receipts: the `0x03` status, "read by" in the detail view over the link | 1 |
| G5 | the slow member: proof-time measure, the 60 s digest, the image refusal | 1 |
| G6 | the group on N homes (2.4), after 1296's H1 to H3; co-hosts | 0.5 plus a decision |

About 7 to 8 passes for a group on one home; G6 rides on N homes.

periculum cells, each with its negative control:

1. **A stock client in the group** (docker, std): a Python LXMF client
   (the reference router, the stand-in for Sideband and Columba), two
   Loquax members, one home. The Python member receives every line with
   the prefix, its replies reach the Loquax members, its reply id is
   rewritten. Negative: with the prefix off, the cell sees no sender in
   the Python member's content and goes red.
2. **Five Loquax members** (docker): 100 lines each, random order of
   writers; every member has 400 others' lines exactly once, in the
   home's order, two ticks and "read by 4 of 4" at every sender.
   Negative: a member offline for the run gets everything on return
   through its propagation node, and "read by" stays at 3 until it reads.
3. **A LoRa member** (rig): the home on the rig host, one member behind an
   LNode over LoRa, three on TCP. The LoRa member is marked slow, gets
   digests, the home's LoRa airtime per line is under the table of 5.1,
   nothing is lost. Negative: digests off, the airtime counter of the
   board goes over by the factor of 5.1.
4. **Double acceptance** (docker, after 1296's E1 pieces): a proof
   dropped by a test hook and a path flipped to a second home; Loquax
   members show one line, the homes converge to one entry.

## 8. Verdict

1. **Build for Loquax with the list interop**, on one home: G1, G2, G3,
   G4, then G5, with cells 1 and 2 green after G2 and G3, cell 3 after
   G5.
2. **Not Loquax-only:** the list costs stock members nothing, they need
   no change, and Lew's bar ("Loquax-only is acceptable") is not reached
   because the stock path works as text.
3. **Do not wait for the N-home experiments.** A group on one home has
   none of their losses. G6 waits for them, and cell 4 for E1.
4. **Sender keys later**, with the explicit cost that stock members drop
   out.

## 9. Decisions for Lew

1. Reactions to stock members: not at all (v1 here) or as a text line.
2. Co-hosts: may the admin give a member the group key (it can then read
   and host; the trust sentence names it)?
3. The group's name in its announce: announce "Familie (Gruppe)" (stock
   members see a name, every node can read it) or announce no name (stock
   members see an address until they name it). Recommended: announce the
   name; a group's name is less than its members, which no announce
   shows.
4. The slow-member threshold and window: 2 s and 60 s as here, or a
   setting under the group's Advanced.
