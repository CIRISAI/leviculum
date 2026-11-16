# A mailbox for friends, a sealed mailbox, a group off the grid, and N homes under one identity: a study

Order 1296, Lew 2026-11-01. Status: study, nothing built. The English
concept page `docs/src/concepts/loquax-homes.md` follows from this in a
later order. Sources read: the concept pages
[loquax-mailbox](../src/concepts/loquax-mailbox.md),
[loquax-reachability](../src/concepts/loquax-reachability.md),
[loquax-phone-node](../src/concepts/loquax-phone-node.md), the drafts
`loquax-onboarding-2026-10.md` and `ninety-percent-walk-2026-11.md`, the
reports of 1264 (link a device) and 1281 (move to a new phone), the
reviewer notes "Mailbox und lnpnd" and "Propagation: Verluste und
Skalierung", and the reviewer's concept paper "Loquax: Zuhause, Mesh und
Nutzer" (called **the paper** below).

Paths: `RNS/` is `reference/Reticulum/RNS/` (1.3.5), `LXMF/` is
`reference/LXMF/LXMF/`. Code cited as **homenode** lives in the worktree
`libreticulum-home` on branch `homenode` (commits of 1264 and 1281) and is
not on master yet; everything else is master at ee9541af.

## 0. The answer in short

- **Product 1, a mailbox for friends: build it, but as a special case of
  N homes, not as a move.** The friend's phone stays a home; Lew's
  computer becomes a second home of the friend's identity. Reason: it is
  the only shape in which the friend keeps working in the forest and a
  stock sender reaches the friend with no hint at all.
- **Product 2, a sealed mailbox: do not build.** Reason: a store without
  the key can neither prove, nor accept a link, nor answer a path
  request as the destination (sections 1.3, 4.2), so for every stock
  sender it is a propagation node by another name; the useful part of it
  is the local selective propagation node the paper already plans.
- **Third use case, the group off the grid: build positions as group
  announces and the phone's full node; it needs no mailbox decision at
  all**, because with N homes the phone is always a home.
- **N homes: build, after E1 to E3 are green.** Two of the paper's claims
  are wrong as written and change the design (1.1 and 1.2): the
  network does **not** decide by hops between two homes, it follows the
  newest announce; and two homes that rotate ratchets on their own
  **lose** packets routed to the home that lacks the newest ratchet. Both
  have a wire-neutral fix: one announce rule and one ratchet schedule
  derived from a shared seed.
- **Tap counts** (section 3): the friend, from install to "first message
  delivered while the phone is off", **8 taps and no typing** (WhatsApp:
  about 6 taps plus a typed phone number and a typed name). The host,
  once set up (1266), **3 taps per friend**. The off-grid group, from
  unboxing to "I see my friend move on the map", **9 taps on the phone
  and one button press per tracker, plus 2 taps per tracker to add it to
  the group**; WhatsApp: impossible without internet.
- **The trust sentence**, on the invitation's accept screen and in the
  profile's "Where your messages are kept":
  "Your messages are also kept on Lew's computer. Lew could read them."
  (German: "Deine Nachrichten liegen auch auf Lews Computer. Lew könnte
  sie lesen.")

## 1. The paper's claims, checked

Sections 4 to 6 of the paper, claim by claim. **Confirmed** means the
reference and our code say what the paper says; **corrected** means they
do not, with the line that decides.

### 1.1 "Who gets what, the network decides by hops": corrected

The reference path table follows the **newest emission**, not the fewest
hops, whenever two different announces of one destination compete:

- Equal or fewer hops: replaced only by an unseen random blob **and** a
  strictly newer emission (`RNS/Transport.py:1765`, `:1772`).
- More hops on a live path: replaced whenever the emission is newer
  (`RNS/Transport.py:1809-1810`); an expired path takes any unseen
  announce (`:1793-1797`).
- The emission is the announcing host's wall clock in whole seconds,
  bytes 5 to 10 of the random hash (`RNS/Destination.py:282`,
  `RNS/Transport.py:3178-3179`).

Hops therefore only decide between copies of the **same** emission. Two
homes of one identity are two emitters; on a segment that hears both,
every node's path points at whichever home announced last, at any hop
count, and flips at the next announce of the other. Our transport keeps
the same rule and adds that the same emission over fewer hops wins
(`leviculum-core/src/transport.rs:7282-7284`, `:7335-7338`, `:331`); that
refines ties, it does not bring hops back as the decider.

Two consequences the paper did not have:

- **Clock skew decides.** A home whose clock runs ahead wins every
  comparison until the other's clock passes it; we only warn about an
  implausible clock and never alter it
  (`leviculum-core/src/transport.rs:5714`, comment above it). Phones and
  servers run NTP; a board never is a home in this design.
- **The paper's segments still work, for another reason.** A node that
  is not a transport node does not rebroadcast announces
  (`RNS/Transport.py:1886`, `transport_enabled() or is_from_local_client`).
  The phone node is not a transport node (paper section 7,
  `loquax-phone-node.md`), so the server's announce never reaches the
  forest through the phone, and the forest only ever hears the phone.
  On the internet side the phone's own announce **does** compete with the
  server's, and so does its path response: a local destination answers
  every path request on the interface it came in on
  (`RNS/Transport.py:2938-2940`), with a fresh emission.

**The rule that follows (section 2.3):** the phone home announces and
answers path requests for the identity on its mesh interfaces only. Then
every segment has exactly one home in normal use, and the flap is
confined to a segment that really holds two (the home LAN, where the
server's daemon and the phone's AutoInterface meet). Our core already
has the call (`NodeCore::announce_destination_on_interface`,
`leviculum-core/src/node/mod.rs:1000`); the reference has the same
argument (`RNS/Destination.py:243`, `attached_interface`). The path
response filter is new.

### 1.2 "Both hold the same ratchet ring; both decrypt anything": corrected

The ring is not shared state that stays shared:

- A destination makes a fresh random ratchet when it announces and the
  interval has passed (`RNS/Destination.py:227-234`), and the announce
  carries the newest one (`:284-286`). `RATCHET_INTERVAL` is 30 minutes,
  the ring holds 512 (`:85`, `:90`).
- A sender keeps **one** ratchet per destination, the last one it heard
  (`RNS/Identity.py:440`), and encrypts single packets and propagated
  messages to it (`RNS/Destination.py:596-599`). Ours does the same
  (`NodeCore::known_remote_ratchet`, `leviculum-core/src/node/mod.rs:3940`).
- The receiver tries every ratchet of its ring (`RNS/Identity.py:890`);
  the fallback to the identity key (`:910-912`) cannot open a packet
  encrypted to a ratchet. With `enforce_ratchets` it drops at once
  (`:904-905`).

So two homes that rotate on their own make different ratchets. A sender
who heard home A's newest announce and is routed to home B (B announced
later at some relay, or the sender's cached ratchet is older than the
relays' path) produces a packet B cannot open: **lost**, with no
sender-visible failure for a propagated message and a missing proof for
an opportunistic one. Syncing rings when homes meet does not close it,
because the window opens exactly when they are partitioned and closes
only at the next meeting.

What is unaffected: **messages over a link** (the mailbox's DIRECT path,
93 % of outgoing on miauhaus per the reviewer note). A link is keyed by
the link request's ephemeral exchange and proven by the identity's
signature, not by a ratchet.

**The fix that holds by construction (section 2.2):** both homes derive
the same ratchet for the same 30-minute epoch from a shared seed, so no
home ever lacks a ratchet another home announced. On the wire a ratchet
stays a 32-byte public key in the announce; how it was made is nobody's
business.

### 1.3 Proofs from either home: confirmed

The proof is the identity's signature over the packet hash
(`RNS/Identity.py:966-971`); the receipt validates against the
destination's identity (`RNS/Packet.py:504`, `:532`), so either home's
proof is valid. The first valid proof removes the receipt
(`RNS/Transport.py:2279-2281`); a second one matches nothing. Ed25519 is
deterministic, so a second home's proof is the same bytes and is very
likely dropped by the packet hash filter first. Duplicate proofs do no
harm. A single packet travels one path, so normally only one home sees
it at all.

### 1.4 "Received at two homes counts as one": confirmed, with the id named

LXMF drops a duplicate by message hash, per router
(`LXMF/LXMRouter.py:1906-1911`, `has_message` at `:1667-1668`). Two
homes are two routers, so each archives it once; the archive merge must
key on the same hash. The 1281 move already carries each entry's kind,
id and time and adds nothing on a second take-in (homenode
`loquax/src/store.rs:1796`, `archive_moved_in`, and its test in
`server.rs`, "a second time adds nothing").

### 1.5 Propagation node collection by two homes: not in the paper, a rule is needed

A PN deletes what a client acknowledges (`LXMF/LXMRouter.py:1503-1508`,
the client side at `:1627-1633`), keyed only on the destination hash. Two
homes share that hash: whichever collects first takes the message, the
other never sees it there. With the archive sync that is fine **unless the
collecting home is lost before it meets another** (E5). Our client can
withhold the acknowledgement (`retain_synced_on_node`,
`leviculum-lxmf/src/router/propagation_runtime.rs:664`). The rule: a home
that is not always on (the phone) collects with retain on; the always-on
home acknowledges. Wire-neutral, as the mailbox page argues for retain
already.

### 1.6 "A lost phone is a lost identity, at Signal no different": corrected

With N homes a lost phone is **not** a lost identity: the server home
still holds the key, and a new phone takes a copy by code. What is lost
is control: the finder holds the key too, and Reticulum has no
revocation, the key is the address (`RNS/Identity.py:584-587`). Signal
re-registers by phone number and contacts see a safety number change;
we cannot. The honest default: after a lost phone the user keeps the
address and is told the risk; "start a new address and tell my contacts"
is one button that sends a signed "I moved to" message from the old
identity to every contact. This needs a decision (section 9).

### 1.7 Positions in the board's announce, "zero extra airtime": corrected

- **Not inside the LXMF delivery announce**: confirmed. Its app_data is a
  positional msgpack list that every client reads by index
  (`LXMF/LXMF.py:156-161` display name, `:181-182` stamp cost), and the
  board builds it from `DeliveryAnnounce` (`leviculum-nrf/src/telemetry.rs:1995`).
- **Zero extra airtime**: wrong for a separate aspect. A separate
  destination is a separate announce frame. The board's announce set is
  one frame, or two with the propagation role
  (`leviculum-nrf/src/announce.rs:149-157`), so a position aspect adds
  one frame per set: +100 % or +50 % announce airtime. The numbers are in
  section 4.3.
- **Public**: an announce is signed, not encrypted. Anyone in radio range
  reads a position in plain app_data. The paper does not say so. Section
  4.3 encrypts it under a group key.

### 1.8 Sealed home "the wrong trade": confirmed, with the mechanism

A home without the key cannot sign a proof (`RNS/Packet.py:328-329`
needs `identity.prv`), cannot prove a link request (the link proof is
the identity's signature), and cannot answer a path request as the
destination (the answer is an announce, signed). Section 4.2.

### 1.9 Section 6, the propagation node figures: taken over, not re-measured

The miauhaus figures (124 955 accepted in 2.23 days, 4 from own clients,
28 delivered, about six copies per message) are the reviewer note's; this
study did not measure them again. They support what the study needs from
them: the PN is a rescue ring, not a way, and a design whose normal path
runs through it is the wrong one. That is a further argument against
product 2.

### 1.10 Section 7, the phone as a node: confirmed, one point added

The order TCP, WiFi AutoInterface, BLE, USB to an RNode, LoRa through a
board is `docs/src/concepts/loquax-phone-node.md:158-163`, and the phone
is not a transport node there. "Relays for the group when it has
internet" (instruction) contradicts that and is section 4.4.

## 2. The N-home model, as rules

### 2.1 What every home holds

| item | today | N homes |
|---|---|---|
| identity private key | one place; 1281 moves it (homenode `loquax/src/home/server.rs:38-43`, `Handover`) | every home |
| ratchet seed | none; random ratchets, signed file (`RNS/Destination.py:214`) | every home, same seed (2.2) |
| ratchet ring | per home | derived, identical (2.2) |
| archive with ids | one store (`loquax/src/store.rs:1436`, `archive_entry`) | every home, converging to the union |
| seen marks, read times | archive entries (`loquax/src/store.rs:1449`, `SEEN_MARK_PREFIX` in `loquax/src/home/wire.rs:94`) | archive entries, merged as entries |
| device list | `home_devices` (`loquax/src/store.rs:1837`, `is_admitted`) | every home, union with removals as entries |
| home list | none | every home: the homes of this identity, each a `loquax.home` destination of its own node, with name and last seen |
| outbox | per home, does not travel (1281 report) | per home; a sent message is an archive entry, so every home knows it was sent, and only the home that holds the outbox row retries it |

### 2.2 Ratchets: one schedule, no exchange

- `seed_0` is made once, when the identity is made, and travels with it
  (the 1281 move, the backup, the invitation of section 3).
- Epoch `e = floor(unix_seconds / 1800)`; `seed_{e+1} = H("lev-ratchet" ||
  seed_e)`; the ratchet private key of epoch `e` is `KDF(seed_e)`.
- A home announces the ratchet of the current epoch and holds those of
  the last 512 epochs (the reference's ring size). On receive it tries
  the ring and the next epoch, which covers a sender whose announce came
  from a home with a clock a few minutes ahead.
- A home deletes seeds older than the ring. Old ratchets cannot be
  recomputed from a later seed, so forward secrecy for the past is what
  the ring gives today.
- **The trade**, and Lew decides it (section 9): a thief who copies the
  seed can compute future ratchets until the seed changes. Today a thief
  who copies the ratchet file can read the past 512 epochs and nothing
  later. The heal: homes make a new seed when they meet and every home
  agrees (a fresh `seed_0` signed by the identity), and a lost device
  triggers one.
- **Deviation rule.** Wire: a ratchet is a 32-byte X25519 public key in
  the announce, as before. Semantics: senders encrypt to the newest
  announced ratchet and it opens, as before. Priority 1: it removes the
  loss of 1.2. All three hold.

### 2.3 What a home announces, and where

- The always-on home (server, Raspberry Pi) announces `lxmf.delivery` and
  its `loquax.home` on every interface, as the mailbox does today.
- The phone home announces the identity on its **mesh interfaces only**
  (BLE, LoRa through a board, USB RNode, AutoInterface), and answers
  path requests for it only there. On its TCP leaf to a rendezvous it
  announces nothing of the identity and stays a client of the always-on
  home. Its `loquax.home` is announced everywhere, so the other homes
  find it.
- A phone with **no** always-on home (the phone-only user, group 2 of the
  paper) announces everywhere, as today.
- Result: in the forest the phone is the only home heard; on the
  internet the server is the only home heard; on the home LAN both are
  heard and the newer announce wins, which is harmless because both
  read and they are one hop apart and in sync within seconds.
- **Deviation rule.** Wire: unchanged. Semantics: a Python peer on the
  internet still reaches the identity through the server; a silence to a
  path request is what any destination that is not there produces.
  Priority 1: keeps internet senders on the home that is always on, so a
  message never lands on a phone that leaves the network a minute later.

### 2.4 How two homes recognise each other

- Each home runs its own `loquax.home` destination on its own node
  identity (as today), and the home list names them.
- Two homes of identity X meet when one sees the other's `loquax.home`
  announce or reaches it through the rendezvous.
- The opener sends, over a link, a request `/sibling` carrying
  `sign_X(home_hash || nonce || time)`. The answerer checks the signature
  against X and answers with its own. Only a holder of X's key can pass;
  a device that is merely admitted cannot (1281's rule, that the
  authority lives with the key, homenode `loquax/src/home/server.rs:613`,
  `may_move`).

### 2.5 What they exchange

In both directions, as pages over the link, each side from the other's
last cursor:

1. Archive entries with kind, id and time (1281's `/move` paging,
   homenode `loquax/src/store.rs:1745` `archive_moving` and `:1796`
   `archive_moved_in`, generalised from one direction to two and from
   "from 0" to "from the cursor I last took from you").
2. Device admissions and removals, as entries.
3. The home list.
4. Seed changes (2.2), if any.
5. Known identities (contacts' keys), as 1281 moves them.

The 1164 watch (`loquax/src/store.rs:1651` `data_version`, `WATCH_PATH`
in `loquax/src/home.rs:153`) wakes the devices of each home when the
sync wrote.

### 2.6 Conflict rules

| case | rule |
|---|---|
| same id twice | one entry; the earlier `at` wins, the rest is identical by construction (the id is the LXMF hash over the content) |
| a message received at two homes | same id, one entry |
| a deletion at one home | a delete mark is an entry; it wins over the message on every home, and the message body is dropped after the merge |
| a seen mark at two homes | the earliest read time wins; the read receipt is signed by the first home that saw it (today's rule, 1234 "the first device to see wins") and a second home that sees the mark already set sends none |
| a device admitted at one home | admitted everywhere after the merge |
| a device removed at one home | removed everywhere; a removal is an entry that wins over an older admission |
| a petname or block set at two homes | the later `at` wins |
| the outbox | not merged; a home that sees the "delivered" entry of a message it still retries stops retrying |

### 2.7 Invariants

1. No message accepted by any home (archived and proven, or collected
   from a PN) is lost while at least one home that holds it survives.
2. Every home's archive converges to the union within one sync after
   two homes can link.
3. No home lacks a ratchet another home announced (2.2, by construction).
4. The user sees one history on every device.
5. A sender never sees "delivered" for a message no home archived (the
   mailbox already proves only after archiving).

## 3. The flows, counted

Taps are counted from the walk's measured screens where they exist
(`ninety-percent-walk-2026-11.md`, 2.2 and 2.4) and from the design
where they do not; new screens are marked **new**.

### 3.1 Product 1: the friend

From "install" to "first message delivered while the phone is off".
Precondition: Lew sent the friend an invitation (a link in another
messenger, or a QR on Lew's screen).

| # | screen (words) | taps |
|---|---|---|
| 1 | store: Install | 1 |
| 2 | first start, **new** button under the two choices: "I have an invitation" | 1 |
| 3 | Android camera permission (skipped when the invitation came as a link) | 1 |
| 4 | the scan; nothing to tap | 0 |
| 5 | **new** "Lew invites you. Your messages will also be kept on Lew's computer, so they reach you while your phone is off. Lew could read them." with "Accept" and "Not now" | 1 |
| 6 | "Let Loquax run in the background", Continue | 1 |
| 7 | Android's notification and battery dialogs | 2 |
| 8 | "Your address", Done (the name comes prefilled from the invitation, which carries the name Lew gave) | 1 |
| | **total** | **8, no typing** |

What happens beneath, without a screen: the phone makes the identity
and seed (as 2.2 today), copies them with its archive to Lew's computer
over the invitation's link (the 1281 paging, but a copy, both stay
homes), the computer starts the friend's home and announces. From then
on a sender on the internet reaches the computer (2.3); the phone may be
off.

WhatsApp for the same steps: install 1, agree 1, phone number typed and
next 1, SMS code (often read by itself) 0 to 1, name typed and next 1,
contacts permission 1: **about 6 taps plus two typed fields** (the walk's
table, section 4). Loquax has two taps more and nothing typed; the two
are Android's dialogs, which WhatsApp also asks later.

### 3.2 Product 1: the host

Once: `loquax mailbox setup` (1266, five steps, one sudo password) and
the phone joined as today. Per friend, on Lew's phone:

| # | screen | taps |
|---|---|---|
| 1 | profile: **new** "Invite a friend" | 1 |
| 2 | **new** "Who is it for?" a name field, "Make invitation" | 1 and a typed name |
| 3 | the QR and "Share", Android's share sheet to the messenger of choice | 2 |
| | **total** | **3 to 4, one typed name** |

The friend's acceptance shows "Anna joined" on Lew's screen (as 1264's
"Device linked"). On the computer: nothing, if the mailbox runs N
identities (section 7, slice P1).

### 3.3 The off-grid group

From "unbox two trackers and a phone" to "I see my friend move on the
map", no internet anywhere. Preconditions: the trackers ship with the
firmware (lnflash, or flashed by the seller), the phone has the app
(installed earlier; the store needs internet once), and the phone full
node with BLE (1194 step 3) and the group announce (4.3) are built.

| # | where | action | taps |
|---|---|---|---|
| 1 | tracker 1 and 2 | press the button once | 1 press each |
| 2 | phone | first start "Use this phone" (walk 2.2: 5 taps) | 5 |
| 3 | phone | Android "Nearby devices" permission (BLE) | 1 |
| 4 | phone | **new** profile "Add a tracker", scan the QR on the tracker's box or label, "Add to my group" | 2 per tracker |
| 5 | phone | Map tab | 1 |
| 6 | phone | Android location permission (for the own dot) | 1 |
| 7 | phone | tap the friend's pin | 1 |
| | **total** | | **9 taps plus 2 per tracker, 1 press per tracker** |

Step 4 joins the tracker to the group key over BLE (4.3). Without a
group key (plain positions) step 4 drops away and every stranger in
radio range sees the trackers; that must not be the default.

WhatsApp: impossible without internet. Columba today: the trackers send
LXMF telemetry to one identity that must be configured on the board
(`leviculum-nrf/src/telemetry.rs:1`, a target per board), so the
equivalent costs a computer and lnflash per tracker.

### 3.4 What the walk must measure

The device walk (`loquax-device-walk.md`) gets three steps, `invite_accept`,
`invite_issue` and `add_tracker`, each counting taps by the screens it
passes, and failing above the numbers here. One session with one
non-technical person per flow (product 1 friend, the group), filmed
screen only, the observer silent; pass = no question asked and no wrong
tap that needed help.

## 4. The products, one by one

### 4.1 Product 1: a mailbox for friends (as N homes)

**Fit.** Every rule is reference behaviour plus 2.2 and 2.3: any key
holder may announce and answer (`RNS/Identity.py:566`, `:579-587`); the
proof validates from either home (1.3). The deviation rule holds as in
2.2 and 2.3. The instruction's original shape (the identity **moves** to
the host, the friend's phone becomes a client) also fits Reticulum, as
1281 shows, but leaves the friend with nothing in the forest and a host
crash as a total outage; N homes is the same work plus the sync.

**Several identities on one host.** Today one mailbox process per
store, one `loquax-mailbox.service` (`packaging/loquax/`). N friends need
either a template unit `loquax-mailbox@<name>.service`, each its own
store and client node, or one process with several identities. The
template is simpler and isolates a friend's crash; it costs a client
connection to lnsd per friend, which is cheap.

**What goes wrong, and what the user sees.**

| event | the friend sees | the design must |
|---|---|---|
| Lew's computer dies for days | nothing while the phone is on; messages sent to them while the phone was off wait at the senders or a PN; "Lew's computer has not been seen for 3 days" in the profile, never as a toast | phone announces everywhere when no always-on home was seen for an hour (2.3 falls back to the phone-only rule) |
| Lew's computer dies forever | the same; after 30 days "Lew's computer is gone. Remove it?" | removal is an entry; nothing else changes |
| the friend leaves (own server, another friend) | "Add a home" by the new host's invitation, then "Remove Lew's computer" | removal makes the leaving home delete the identity and archive (it is asked to; a malicious host keeps them, and the screen says so) |
| the host turns malicious or careless | nothing until it happens; it can read, and it can send as the friend | the trust sentence at accept; a seed change on removal (2.2) so it cannot read new single packets; it keeps the key, so sending as the friend stays possible: the honest way out is a new address (1.6) |
| the friend loses the phone | Lew's computer still holds everything; a new phone takes it by the "Move from my old phone" flow pointed at the computer home | 1.6: offer "keep the address" or "new address and tell my contacts", plus a seed change |
| two phones | both homes; one history | 2.5 |
| the host updates, the friend does not | nothing | the sibling protocol is additive and versioned; an unknown page kind is skipped, a home too old to sync is shown in the profile as "needs an update" |
| a stock-client sender | nothing; it reaches whichever home it hears, as for any LXMF peer | nothing extra, which is the argument for product 1 over 2 |

### 4.2 Product 2: a sealed mailbox

**Fit.** What a home without the key cannot do in Reticulum:

- prove a packet (`RNS/Packet.py:328-329` signs with `identity.prv`);
- accept a link to the destination (the link proof is the identity's
  signature; a link request without it times out at the sender);
- answer a path request or announce as the destination (an announce is
  signed, `RNS/Identity.py:566-579`);
- decrypt anything (single packets and propagated messages are encrypted
  to the destination's key or ratchet, 1.2).

So it can only store ciphertext someone chose to give it. A stock LXMF
sender gives ciphertext to exactly one place: its own outbound PN
(`LXMF/LXMRouter.py:133`, one `outbound_propagation_node`). A hint in the
friend's announce does not change that: the `lev` namespace is a field
pair inside messages (`docs/src/concepts/loquax-read-receipts.md:23`),
not an announce aspect, and no stock client reads a new aspect. Loquax
senders could read a `loquax.home`-like hint and upload there, which is
a PN upload with a destination filter.

**The flow** would be the same 8 taps as product 1, with a different
trust sentence ("Lew's computer keeps your messages locked; it cannot
read them"), and a worse result: the sender sees "waiting" instead of
"delivered" until the phone is back, because nobody can prove.

**What goes wrong.** The store fills (a PN's weight eviction,
reviewer note section 1, applies); a stock sender never learns the hint
and its PN must peer with the store, or the message never arrives; a
host crash loses messages that the senders already count as handed over.

**Verdict: do not build.** Its useful core, "a computer near me keeps my
mail and wakes my phone", is a local selective PN with a destination
filter and the existing wake (paper section 6), which serves stock
senders through peering and needs no new product.

### 4.3 The third use case: positions as group announces

**Fit.** A board registers a second destination `lev.position` (SINGLE,
the board's identity) and announces it with the delivery announce, in
the same set and on the same cadence (5 min moving, 1 h still,
`leviculum-nrf/announce-policy/src/cadence.rs:50`, `:60`). Its app_data:
`[version, group_tag(4), nonce(12), ciphertext]`, the ciphertext an
AEAD under the group key of `[lat_e7, lon_e7, alt_m, fix_time, speed]`.
Wire: a valid announce with opaque app_data, which every Reticulum node
relays and no LXMF client parses (it is not `lxmf.delivery`, so
`LXMF/LXMF.py:156` never sees it). Semantics: unchanged for everyone
else. Priority 1: positions reach every node in the mesh with no
mailbox and no recipient list. Deviation rule holds.

**Airtime.** An announce without ratchet is 19 header bytes, 148 of
key, name hash, random hash and signature, plus app_data. With about
40 bytes of sealed position: about 207 bytes, 0.58 to 0.62 s at SF8, BW 125 kHz,
CR 4/5 (0.53 s for 187 bytes with a 20-byte plain position, preamble 8
to 24 symbols, the LoRa time-on-air formula). Per board:

| state | interval | own duty |
|---|---|---|
| moving | 5 min | 0.20 % |
| still | 1 h | 0.016 % |

Every transport node relays an announce once, so on one channel with
`n` moving boards the position frames cost about `n × n × 0.59 s` per
5 min: 5 boards, 15 s per 300 s, **5 % channel occupancy**, as much again
as the delivery announces cost today. The cadence's duty budget
already prices the announce set and stretches it
(`leviculum-nrf/src/announce.rs:149-157`, `frames_per_set`), so a busy
channel slows the map instead of breaking the band. The reference caps
relayed announces at 2 % per interface (`RNS/Reticulum.py:114`,
`ANNOUNCE_CAP`). Against LXMF telemetry: one sealed announce reaches the
whole group; telemetry to `k` people is `k` messages and `k` proofs.

**Privacy.** Plain app_data would publish every tracker's position to
anyone in range. The group key is made on the phone at first start,
travels to trackers over BLE at "Add a tracker" and to friends' phones
in the contact QR (`lxma://` grows a field, or a group QR). This is new
and is section 9's first decision.

**The phone's local node** puts every decrypted position on the map,
whether any mailbox is reachable or not. LXMF telemetry to the identity
stays for Columba and for the history (the map keeps the archive's
positions; the announce positions are state, kept as the latest per
board).

### 4.4 The phone node relaying for the group

As a non-transport node (paper section 7) the phone carries only its own
traffic. To relay the group's messages to the internet it would have to
run transport, and then every announce it hears on the internet is
offered to the LoRa interface, capped at 2 % of its airtime
(`RNS/Reticulum.py:114`) and queued behind it, with the battery cost of
a transport table and of a held TCP connection. What the group gains: a
member's message to someone outside reaches the internet while the phone
has coverage. **Not by default**; an explicit "share my connection with
the group" later, after 1194 step 3 and a tier R measurement. Messages
to the group's own members need no relay: they are in the mesh.

### 4.5 "Two homes that never lose", and N

The shape the instruction asks to test first, checked against the rules
above: both hold the key (yes); the same ring (only with 2.2); both
accept and decrypt (yes, with 2.2); the network decides by hops (no,
by recency, 1.1; 2.3 makes it decide by segment); the archive sync makes
it one mailbox (yes, 2.5); proofs per home (1.3). With those two
corrections it holds, and N follows without new rules: every rule above
is pairwise, the sync is per pair, the home list is the union. Three
homes in a ring converge through the middle one because each sync takes
everything the other holds, not only what the other received itself
(E3 proves it). Products 1 and 2 are special cases, as the paper says;
product 2 is the case that fails.

**The simpler rule** the instruction asks for ("the phone is the home
whenever it has no internet; the server takes over when it does") is
2.3 seen from the user: with the phone announcing on mesh only, the
phone receives exactly what reaches it by radio, and the server what
reaches it by internet, with internet on or off. No switch is needed,
and none is shown.

**What it costs the user:** the key is on the phone, as in the
phone-only mode today. Nothing else: no setting, no mode.

## 5. Prior art, briefly

| | what it got right for the non-technical user | what we take (ideas, no code) |
|---|---|---|
| Matrix homeserver | a friend hosts accounts for friends; keys stay on devices for end-to-end rooms; cross-signing makes a new device trusted by an old one | the social shape of product 1 (a person you know hosts); a new device is admitted by an old one, never by the host alone (2.4) |
| Delta Chat with chatmail relays | one tap: scanning a QR creates the account on the relay, no address typed; "Add second device" moves the keys by QR from the old device | the invitation as the whole onboarding (3.1); 1264/1281 already copy the second-device QR |
| Signal | no hosting choice at all; linked devices by QR; per-device keys, the sender encrypts to each device | the QR for a device; per-device keys need the sender to fan out, which one LXMF destination per identity cannot do, which is why a sealed store has no future here |
| e-mail | the provider holds everything, so every device sees one mailbox; billions accept "the provider can read it" | one history on every device; an honest trust sentence is enough, the user accepts a host they know |

## 6. The experiments, as periculum cells

Pieces that exist: TOML cells with `[nodes]`, `[links]` and `[[steps]]`;
`block_link`/`restore_link` cut a pair both ways for docker and netns
(`periculum/src/topology.rs:4486-4492`, executor `:1508-1514`);
`media_silence`/`media_restore` take a board off the air
(`topology.rs:4458-4485`); `expect_in_log` counts with `count=0` as
absence and a poll window (`topology.rs:5782-5796`); `log_mark` and
`since_mark` count after a moment (`:5870`); `EVENT_COUNTS` sums tokens
(`periculum/src/event_counts.rs:12`). The loquax cells nearest to this:
`lxmf_moved_identity_ratchets` (a mailbox on an identity shared with an
old client, `periculum/conformance/lxmf_moved_identity_ratchets.toml:51`)
and `lxmf_pn_loquax_fallback_leviculum` (cut and rejoin). Hardware
shapes with a host node beside boards: `ble_lora_transport.toml` and
`board_delivery_promise.toml`.

Pieces that are missing, the order after this study:

- a docker rendezvous node (today only the Rust tests in
  `loquax-android/tests/home_reach.rs` have one, and `home_reach_*` are
  not periculum cells, though line 7 there says they mirror some);
- a step `loquax_archive {on, expect_ids|count}` that dumps a home's
  archive ids, and `loquax_ratchets {on}` that prints the ring's hash;
- structured events on the home: `HOME_ACCEPT id= via=`, `HOME_PROVE
  id=`, `HOME_SYNC peer= in= out=`, `HOME_ANNOUNCE iface=` (the outbox
  path logs free text today, reviewer note section 1);
- `lxmf_send` from a stock Python sender in opportunistic mode with a
  count, to hit the ratchet path (1.2).

Every cell runs **today's code first** as the negative baseline: two
`loquax mailbox run` on one copied identity is exactly the two live
copies 1281 guards against, and the cells must show the loss mechanisms
of 1.1 and 1.2 red before the N-home code lands.

### E1 partition and rejoin (docker, std)

- Nodes: `rv` (Rust transport, the rendezvous), `server` (home 1),
  `s1` (Python LXMF sender) on segment A; `phone` (home 2, mesh
  interfaces only per 2.3), `s2` (Python sender) on segment B, joined to
  A by one TCP link `phone-rv`.
- Steps: both homes up, sync seen (`HOME_SYNC`); `block_link phone rv`
  both ways; 100 messages from `s1` (50 DIRECT, 50 opportunistic) and
  100 from `s2`; `log_mark rejoin`; `restore_link`; poll until both
  archives hold 200.
- Assert: every id exactly once in both archives; `HOME_PROVE` count per
  home equals what reached it; ratchet hashes equal; senders' proofs =
  200; time from `rejoin` to convergence reported.
- Positive control: the sync disabled by a config flag, must go red on
  "archives differ". Negative control: today's code, must go red on
  missing archive entries.

### E2 flap

- Nodes: `x` (Python sender), `t` (transport), both homes one hop
  behind `t`, both announcing everywhere (2.3's LAN case forced).
- Steps: homes announce alternately every 20 s (a step `announce {on}`);
  200 opportunistic messages from `x` at 2 per second.
- Assert: no loss; which home proved each (`HOME_PROVE` per home, both
  > 0, so the flap really happened); the archive union is 200 on both.
- Positive control: 2.2 off (random ratchets), must lose messages (the
  1.2 mechanism; the home's decrypt failures counted by a new event).
  Negative control: one home stopped, the other proves all 200.

### E3 three homes in a ring

- Homes `a`, `b`, `c`; links `a-b`, `b-c`; `block_link a c` for the whole
  run. A sender per home's segment, 50 messages each, while `b` is cut
  from both; then `b` rejoins `a`, later `c`.
- Assert: `a` and `c` hold all 150 without ever linking.
- Positive control: the sync taking only "what I received myself"
  (a flag) must leave `a` without `c`'s 50.

### E4 hardware

- Rig profile `lnode_pair` (`periculum/rig.toml:241`), shaped like
  `ble_lora_transport.toml`: two LNodes on LoRa as the forest, the host
  node with `ble = true` running the phone home (the Android session's
  engine runs on Linux too), the server home in docker behind a
  rendezvous container, the host's TCP link to it cut and restored every
  5 minutes (`block_link`).
- 100 messages from each board's side (board telemetry and a Python
  sender on the server side), positions on the map state of the host
  node counted.
- Assert as E1, plus the board positions decrypted on the host while
  the TCP link is down.
- Reviewer window; the rig lock and the docker lock apply (CLAUDE.md,
  Test flow).

### E5 the lost home

- E1's topology. After convergence, the server home is stopped for good
  (container removed, store deleted).
- Assert: the phone home keeps receiving from both segments (it falls
  back to announcing everywhere after the hour of 4.1, shortened by
  config in the cell); a new device links to the phone home by code
  (1264's code) and pulls the whole archive.
- Positive control: a message collected from a PN by the server home
  with retain off and the server deleted before a sync must be missing
  on the phone (1.5); with the rule of 1.5 it must be present.

## 7. Cost

Counted in coder passes of the usual size.

**Exists:** the move with ids and the guard against two copies (1281,
homenode), link a device (1264, homenode), the setup wizard (1266,
master), ratchet export and import (1185, `loquax/src/import/ratchets.rs:177`),
the archive watch (1164), seen marks (1234), `announce_destination_on_interface`
(core), the announce cadence and its duty budget (#401), the PN client's
retain switch.

| slice | what | passes |
|---|---|---|
| H1 | ratchet seed and epoch schedule (2.2), in core behind a destination option, plus an interop cell: a Python sender, our receiver, rotation across epochs | 2 |
| H2 | the sibling protocol: `/sibling`, two-way paging from cursors, conflict rules (2.4 to 2.6), on 1281's paging | 3 |
| H3 | the announce rule (2.3) with the path response filter and the one-hour fallback | 1 |
| H4 | the PN collection rule (1.5) | 0.5 |
| H5 | the home list, device admissions as entries, the profile's "Where your messages are kept" | 1 |
| H6 | the events of section 6 | 0.5 |
| E | periculum: rendezvous node, archive and ratchet steps, E1 to E3 (lane 2), E5; E4 at the rig | 4 + a reviewer window |
| N | the phone full node, 1194 steps 1 to 3 | 4 to 6 (the 1194 plan) |
| P1 | product 1 packaging: the invitation (a `lqxh1` variant that copies instead of moves), N identities on one host by a template unit, the host's "Invite a friend", the friend's "I have an invitation" | 3 |
| G | positions as group announces: board `lev.position`, the group key, "Add a tracker", the map's state layer | 3 to 4 |
| S | product 2, for comparison: a filtered store, the hint aspect, the wake, the PN peering | 4, not recommended |

N homes with its proof: about 12 passes plus the phone node. Product 1
on top: 3. The group: 3 to 4 plus the phone node.

## 8. Verdict

1. **Run E1 to E3 on today's code first** and show the two loss
   mechanisms red (1.1 flap with random ratchets, 1.2 divergence). This
   needs only the periculum pieces of section 6 and no Loquax change.
2. **Then build N homes** in the order H1, H6, H2, H3, H4, H5, each slice
   turning one of E1 to E3 green; E5 and E4 last. H1 first because
   without it E2 cannot be green whatever the sync does.
3. **Product 1** as the packaging P1, after H2. Not the move-to-host
   shape of the original question.
4. **Product 2: not built.** The local selective PN of the paper covers
   its use.
5. **The group**: G and the phone node, independent of the mailbox
   decision; G can start now on the board side.
6. **Proof the flow is simple**: the device walk's steps of 3.4 under the
   tap counts of section 3, and one session with one non-technical person
   per flow, filmed, the observer silent.

## 9. Decisions for Lew

1. The group key for positions (4.3): yes, and positions are never
   plain by default.
2. The ratchet seed trade (2.2): future ratchets computable from a stolen
   seed until the next re-seed, against losing packets between homes.
3. A lost phone (1.6): keep the address by default, with "new address and
   tell my contacts" as one button, or the reverse.
4. The phone announcing on mesh only when an always-on home exists (2.3):
   the default, as this study recommends.
5. Several identities on one host by template unit (4.1), not one process.
