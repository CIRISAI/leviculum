# Loquax: N homes under one identity

Status: concept, decided by Lew on 2026-11-01 and 2026-11-02 (study 1296,
order 1338, Refs #232). Nothing of the N-home model is built yet; the
experiments E1 to E3 run today as periculum cells, red by design. The
study with every claim checked line by line is
`docs/drafts/mailbox-for-friends-study-2026-11.md`. A single home, as
it runs today, is [the home node](loquax-home-node.md); how a phone
reaches it is [Reaching the mailbox from anywhere](loquax-reachability.md).

## Words

The user interface says **"mailbox"** (German **"Postfach"**). This page,
the code and the experiments say **"home"** (German **"Zuhause"**). A
home is a mailbox seen from the inside: a node that holds the identity
and answers for it. The user never meets the word "home", and never
needs to know how many there are.

## The problem

The same person is reachable over the internet one day and off the grid
with friends the next, under one address. With one home the two
contradict each other: a home on a server takes the mail while the phone
is in the forest, so the phone sees nothing there; a home on the phone
takes nothing while the phone is off. A design that makes the user pick
one beforehand is not the simple messenger Loquax is meant to be.

## The model

There is not one home but any number, and they stay in agreement.

- **Every home holds the identity's key and its ratchets.** Each one
  announces the identity, accepts everything addressed to it at any time,
  decrypts it, proves it and archives it. A home never refuses.
- **The network decides by path** which home gets which message. No home
  and no user chooses. In the forest the phone is the only home in
  reach; on the internet it is the server.
- **Homes sync whenever they meet**: archive entries by id in both
  directions, device admissions and removals, the home list, seed changes
  and contacts' keys. The same id twice is one entry; a deletion or a
  removal at one home holds at every home.
- **The user notices nothing.** No setting, no mode, one history on every
  device. The price is the one every messenger pays: the key is on the
  phone.

Invariants (the study's 2.7):

1. No message any home accepted is lost while one home that holds it
   survives.
2. Every archive converges to the union within one sync after two homes
   can link.
3. No home lacks a ratchet another home announced.
4. The user sees one history on every device.
5. A sender never sees "delivered" for a message no home archived.

What exists today for one home and carries over:

- The move to a new device, with each archive entry's kind, id and time:
  `archive_moving` (`loquax/src/store.rs:2638`) and `archive_moved_in`
  (`loquax/src/store.rs:2690`), which adds nothing on a second take-in.
  The sync generalises it to both directions and to paging from a cursor.
- The rule that authority lives with the key, not with an admission:
  `may_move` (`loquax/src/home/server.rs:1294`). Two homes recognise each
  other the same way, by a signature only a key holder can make
  (planned: a `/sibling` request over a link).
- The ratchet export and import of 1185, `take_over`
  (`loquax/src/import/ratchets.rs:177`), which is how the experiments'
  second home adopts the first one's ring today.
- Proofs from either home are valid: the proof is the identity's
  signature (`reference/Reticulum/RNS/Packet.py:326-329`), and a second
  proof matches no receipt.

### Two special cases

- **The friends' mailbox** is the N-home case with two homes: the
  friend's phone stays a home, the host's computer becomes a second home
  of the friend's identity. It is the only shape that keeps the friend
  working in the forest and lets a stock sender reach the friend without
  any hint. The trust sentence on the invitation's accept screen and in
  the profile: "Your messages are also kept on Lew's computer. Lew could
  read them." (German: "Deine Nachrichten liegen auch auf Lews Computer.
  Lew könnte sie lesen.") Planned, after the sync.
- **The sealed mailbox** (a store that never holds the key): **no**. A
  store without the key cannot sign a proof
  (`reference/Reticulum/RNS/Packet.py:326-329` signs with the identity's
  private key), cannot prove a link request and cannot answer a path
  request as the destination, because the answer is a signed announce.
  For every stock sender it is a propagation node by another name, and
  its useful part is the local selective propagation node
  ([the propagation node page](loquax-mailbox.md)).

## The experiments

The study's experiments run on today's code first, as the negative
baseline, in periculum's `regression/` (orders 1304 and 1512) and, for E4,
its `hardware/` corpus. Every red cell is a registered known red against
#232, with what clears it and what falsifies it; E5's control is green:

| Cell | Experiment | Red today at |
|---|---|---|
| `loquax_homes_partition` | E1: two homes on two segments, partitioned while a stock sender on each side sends 100, then rejoined | `in_all_archives`: each home holds its own 100 |
| `loquax_homes_flap` | E2: two homes one hop behind one transport node, announcing in turn every 20 s, a stock sender sending 200 | `in_all_archives`: the split follows the announces |
| `loquax_homes_flap_own_ratchets` | E2's positive control: the second home on a ring of its own | `in_all_archives`, `rings_differ`, `undecryptable` |
| `loquax_homes_ring` | E3: three homes in a line, the two ends never linked | `in_all_archives`: each holds its own 50 |
| `loquax_homes_lost` | E5: three homes; c alone takes in 10 single packets and collects 10 uploads from lnpnd while a and b are cut away, then dies (mailbox killed, store removed, container killed); a and b rejoin | `home_lost`: `HOME_LOST recovered=0 lost=20` |
| `loquax_homes_lost_stock` | E5's control: the reference client as the identity's one home, dying with its storage; its key restored onto a fresh client | green; the same `recovered=0 lost=20` |
| `loquax_homes_lora`, `loquax_homes_ble` (hardware) | E4: E1's shape on two LNodes, LoRa or BLE the only medium between the homes, partitioned by taking one board off the air | `in_all_archives`: expected, first run owed |

Measured on 2026-11-02: nothing lost and every message proven, but no
home holds what reached another. In the own-ratchets arm each mailbox
restart is followed within 50 ms by one `LXMF_RX_UNDECRYPTABLE` at the
home that just announced (the event, `loquax/src/engine.rs:1532`): the
sender sealed to the other home's ratchet before the new announce reached
it. The stock router's retry heals a single packet; a propagated message
has no such retry, which is where the loss lives.

E5 (order 1512, 2026-11-07, docker, leviculum a8703650): a lost home takes
everything it alone held. `HOME_LOST recovered=0 lost=20` for three Loquax
homes, the same line as the reference stack's single home. The 10 single
packets were never anywhere else. The 10 uploads were gone from lnpnd the
moment the dying home's collection confirmed them (`PN_DEPART
reason="collected"`), as the reference node purges a client's "haves". So
today N homes protect nothing that one home would not. Only H2 (a sibling
took the messages in while the home lived) or H4 (the node keeps a home's
collection for the identity's other homes) changes that line. E4
(`loquax_homes_lora`, `loquax_homes_ble`) puts E1 on two LNodes. Its
senders' legs are TCP, so on today's code it is expected to read E1's
numbers; its radio-rate figure, `converge_ms` over the air, exists once H2
does.

Build order after the cells; each slice turns a cell green:

- **H1, the ratchet schedule:** each epoch's ratchet derived from a seed
  that travels with the identity. Clears `loquax_homes_flap_own_ratchets`'
  `rings_differ`.
- **H6, the events:** `HOME_ACCEPT`, `HOME_PROVE`, `HOME_SYNC`,
  `HOME_ANNOUNCE`. ACCEPT, PROVE and ANNOUNCE already show in E5's record
  (`home_accept=20 home_prove=10 home_announce=3`); `HOME_SYNC` waits on H2.
- **H2, the sibling sync:** two-way paging from cursors, forwarding what a
  home took from a sibling. Clears E1 to E3 and E4. For E5, a sibling that
  took the messages in *before* the loss is what recovers them; with the
  cell's shape today (the survivors cut away until after the death), H2
  alone cannot turn `loquax_homes_lost` green, and its register row says
  the cell or its shape changes with H2.
- **H3, the announce rule:** decided (d), mesh-only announce when an
  always-on home exists.
- **H4, the propagation node collection rule:** the node keeps a
  collection for the identity's other homes. Turns E5's
  `recovered_propagated` from 0 to 10.
- **H5, the home list.**

## Decided

Lew, 2026-11-02, on the study's five open points.

### (a) A group key for positions and groups

Positions and group messages are sealed under a group key, derived per
group. It comes after G4 of the group chat study
(`docs/drafts/group-chats-study-2026-11.md`, section 7: proofs and
receipts). Today a board seals its position announce to its owner's
identity only ([Announce cadence, the position
announce](announce-cadence.md#the-position-announce)); the group key is
what lets friends' phones read it too. Planned.

### (b) Ratchets from a shared seed, as a hash chain

Two homes that rotate on their own lose packets. A destination makes a
fresh random ratchet when it announces after its interval
(`rotate_ratchets`, `reference/Reticulum/RNS/Destination.py:227-234`),
every 30 minutes, and keeps 512 (`reference/Reticulum/RNS/Destination.py:85`,
`:90`). A sender keeps one ratchet per destination, the last it heard
(`reference/Reticulum/RNS/Identity.py:440`); ours does the same
(`known_remote_ratchet`, `leviculum-core/src/node/mod.rs:4109`). The
receiver tries its ring and then the identity key
(`reference/Reticulum/RNS/Identity.py:910-912`), which cannot open a
packet sealed to a ratchet. A packet routed to the home without the
newest ratchet is lost. Links are not affected; opportunistic and
propagated messages are.

The decision: every home derives the ratchet of each 30-minute epoch
from one seed. `seed_0` is made with the identity and travels with it;
`seed_{e+1} = H("lev-ratchet" || seed_e)`; the ratchet of epoch `e` is
derived from `seed_e`. A home holds the last 512 epochs and tries the
next one too, for a clock a few minutes ahead, and deletes older seeds.
On the wire a ratchet stays a 32-byte public key in the announce.

The accepted trade: a thief who copies the seed can compute future
ratchets until the next re-seed. A **lost device triggers a re-seed**: a
fresh `seed_0`, signed by the identity, carried to every home at the next
sync. Planned (slice H1).

### (c) A lost phone

Any other device of the user revokes the lost phone, the homes re-seed,
the rest stays: the address, the contacts, the history. With N homes a
lost phone is no longer a lost identity, since another home still holds
the key and a new phone takes a copy by code.

What exists: revoking an admitted client device, `revoke`
(`loquax/src/store.rs:3102`), checked again on every request. What is
new: revoking a device that was itself a home, from any other device, and
the re-seed of (b). The honest limit stays: Reticulum has no revocation
of an identity key, the key is the address. The finder can no longer read
what is sealed to the new ratchets, but still holds the key and can sign
as the user. "Start a new address and tell my contacts" is the way out
for a user who wants it. Planned.

### (d) The phone's home announces on mesh interfaces only

By default, when an always-on home exists, the phone's home announces the
identity and answers path requests for it only on its mesh interfaces
(BLE, LoRa through a board, a USB RNode, the AutoInterface), never on its
TCP leaf to a rendezvous. Its own `loquax.home` is announced everywhere so
the other homes find it. A phone with no always-on home announces
everywhere, as today, and so does a phone whose always-on home has not
been seen for an hour.

The reason is the first correction below: the path table follows the
newest emission, so two homes heard on one segment flap by recency.
With this rule every segment hears one home in normal use: in the forest
the phone, on the internet the server, and on the home LAN both, where a
flap is harmless because both read and sync within seconds. A
non-transport phone never rebroadcasts the server's announce into the
forest (`reference/Reticulum/RNS/Transport.py:1886`), but it does answer
a path request for a local destination with a fresh announce on the
interface the request came in on
(`reference/Reticulum/RNS/Transport.py:2939-2940`), which is why the
path response needs the filter too.

The core can already announce on one interface,
`announce_destination_on_interface` (`leviculum-core/src/node/mod.rs:1024`).
The path response filter and the one-hour fallback are new. Planned
(slice H3).

### (e) N identities on one host: a template unit

A host that keeps homes for several people runs one process per
identity, as a systemd template unit `loquax-mailbox@<name>.service`,
each instance with its own store and its own client connection to
`lnsd`. One friend's crash stays that friend's, and a client connection
per identity is cheap. Today the package ships the single unit
`packaging/loquax/loquax-mailbox.service`. Planned (the friends' mailbox
packaging).

## Two corrections to the concept paper

The concept paper of 2026-11-01 got two claims wrong; the study 1296
corrected them and the decisions above rest on the corrections.

### The path table follows the newest emission, not hops

Between two different announces of one destination the reference keeps
the newer emission. With equal or fewer hops an announce replaces the
path only with an unseen random blob and a strictly newer emission
(`reference/Reticulum/RNS/Transport.py:1772`); with more hops on a live
path a newer emission still wins
(`reference/Reticulum/RNS/Transport.py:1809-1810`). The emission is the
announcing host's clock in whole seconds inside the random hash
(`reference/Reticulum/RNS/Destination.py:282`). Ours keeps the same rule
and lets the same emission over fewer hops win a tie
(`leviculum-core/src/transport.rs:7690-7693`). Hops only decide between
copies of the same emission. Two homes on one segment therefore flap by
recency and by their hosts' clocks, which `loquax_homes_flap` shows.
Hence decision (d).

### Positions in an announce cost airtime, and go sealed

A position in its own announce aspect is one more frame per announce
set, not zero airtime. The study estimated about 0.6 s per announce at
SF8 for a position of about 40 bytes, 0.2 % own duty per moving board,
and about 5 % channel occupancy with five moving boards. An announce is
signed, not encrypted, so a plain position tells everyone in range where
the board is; positions go sealed. Both are built since 1301: the measured
figures (a plain position 605 ms, the sealed one 913 ms in two frames,
pinned by `leviculum-lxmf/tests/lev_position_airtime.rs`) and the sealed
default are on [Announce cadence](announce-cadence.md#what-it-costs). The
group key of decision (a) replaces "sealed to the owner" by "sealed to
the group".
