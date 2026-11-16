# Loquax: the home node

Status: slices 1 and 2 built (orders 1101 and 1106, Refs #232). The research and the
critique behind it are the draft `docs/drafts/home-node-2026-10.md` (order
1096); this page keeps what is decided and what is built.

## Why

Two devices on one LXMF identity lose mail. The reference client deletes
every message it fetched from the propagation node, whatever it is
configured to do (`reference/LXMF/LXMF/LXMRouter.py:1623-1638`), so the
second device never sees what the first collected. Ratchets make it worse:
a message encrypted to one device's newest ratchet cannot be read on the
other, and is deleted there all the same. The draft lists seven ways a
message disappears; most of them hit hardest when two devices share an
identity.

The home node ends the sharing. One always-on Loquax holds the identity and
keeps every message; the user's other devices read from it.

How far the home node is a Reticulum node (an LXMF endpoint behind a full
lnsd, never a transport or a propagation node, devices admitted by pairing
and never by port) is [what a Loquax Mailbox is](loquax-mailbox-roles.md),
invariants M1 to M5.

## Roles and keys

| Role | Holds | Runs |
|---|---|---|
| Home node | the LXMF identity, the archive, the admitted devices | `lnsd` plus `loquax mailbox run` |
| Client | its own RNS device identity, a copy of what it pulled | `loquax` with a home code |
| Senders | nothing new | stock LXMF |

Decided by Lew on 2026-10-29:

- **Only the home node holds the LXMF identity.** Each client has its own
  RNS identity (`$LNMSG_HOME/device-identity`), admitted once and revocable.
  A client never registers a delivery destination at all, so in particular
  never one for the home identity, whose private key it never holds. This is
  enforced in code: a client's engine starts in a state that has no identity
  to register (`Engine::client` in `loquax/src/engine.rs`), and the test
  `a_client_engine_registers_no_delivery_destination` runs the same probe on
  a messenger engine as its positive control.
- **The default home node is the user's own device**: a Raspberry Pi, a
  home PC, anything Linux that stays on. A VPS may run it too, and works
  without NAT trouble, but it puts the identity and every message in
  plaintext on rented hardware. That is a conscious choice for the user to
  make, not a default.
- **The archive is kept forever, under a disk cap.** When the cap is
  reached the oldest entries go. See "The archive".
- **A public propagation node on the home host runs under an identity of
  its own**, never the user's. Not part of slice 1.
- **The phone comes later**, as our own Android app. The clients of slice 1
  are Loquax on a laptop or a desktop. The app's shape and its first build
  (`loquax-android/`) are the study `docs/drafts/loquax-android-2026-10.md`
  (order 1111).

Losing a client: revoke it, the identity is untouched. Losing the home node
loses the identity, since LXMF has one key per identity; keep an offline
backup of `$LNMSG_HOME/identity`, made at setup.

## Admission and revocation

On the home node, `loquax mailbox admit --name laptop` opens one single-use
token and prints a code. The code carries the home identity's public key
and the token, with four bytes of SHA-256 over both so a mistyped code is
refused before anything goes on the wire, in lower-case base32 behind the
prefix `lqxh1` (140 characters). The home destination and the LXMF
address both derive from the public key, so the code needs nothing else.

A device on the home node's own computer reaches it through the
daemon's shared instance (rung 0 of
[the ladder](loquax-reachability.md)). A phone has no shared instance to
reach the home node through, so it needs the home node's TCP server
too. `loquax mailbox admit --server host:port`
writes it behind the code, `lqxh1...@host:port` (an IPv6 address in
brackets): the base32 alphabet has no `@`, so the split is unambiguous, a
code without the address reads as before, and a desktop's `home join`
drops the address. `admit` prints the code as a QR code beside the text,
in Unicode half blocks, the base32 in upper case in the QR alphanumeric
mode (version 6 for the code alone, at error correction level M). The
Android join screen takes the combined form pasted or shared to it from a
QR reader and fills the address from it.
How a phone outside the home reaches that server, through a shared
rendezvous node or any mesh path, is
[Reaching the mailbox from anywhere](loquax-reachability.md).

On the new device, `loquax mailbox join <code>` creates the device identity,
asks for the path to the home destination, opens a link to it, identifies
with the device identity and sends the token. The home node admits the
identity under the name given at `admit`, records when, and deletes the
token, all in one transaction. A token used once is refused the second
time, and so is one older than `home_token_hours` in the config (24 by
default: time to carry the code to the other device, short enough that a
code left in a chat log or a screenshot is dead by the next day). `loquax
home tokens` lists the codes still open, with when each expires, and
`loquax home revoke-token <prefix>` withdraws one by the start of its hash
as that list shows it (four hex characters are enough when unique).

`loquax home devices` lists the admitted devices, `loquax home revoke
<device>` removes one, by its name or by its hash (four hex characters are
enough when unique). Every request checks membership again, as lxmd's
control handlers check their allow list on every call
(`reference/LXMF/LXMF/LXMRouter.py:838-845`), so a device revoked while its
link is up is refused on its next request.

## The home destination

`loquax.home`, a SINGLE destination of the home identity. It is never
announced: a client learns its key from the code and asks for its path, and
the home node answers that path request because the destination is
registered locally, as the reference answers for any local destination
(`reference/Reticulum/RNS/Transport.py:2936-2941`). Peers other than the
user's own devices never see it.

Its six request handlers admit every link at the RNS level; the home node
reads the identity the link was identified with and decides itself. A
request on a link that was not identified is answered with a refusal.

## The client protocol

Link requests on `loquax.home`, msgpack on the wire:

```text
/join  [token: bin16, name: str]   -> [status]
/sync  [cursor: uint, (page: uint, (fold: str))]
                                   -> [status, first: uint, more: bool,
                                       [[seq: uint, kind: uint, at: int, packed: bin], ...],
                                       (left: uint)]
/send  [client_id: bin16, destination: bin16, title: bin, content: bin,
        [[key: int, value: bin], ...]] -> [status, id: bin32]
/mark  [flag: uint, [id: bin32, ...]] -> [status]
/mark  [2, [id: bin32, ...], read_at: uint] -> [status]
/petname [address: bin16, name: str] -> [status]
/watch [cursor: uint]              -> [status, newest: uint]
/key   [address: bin16, public_key: bin64] -> [status]
/move  [token: bin16, cursor: uint] -> [status, private_key: bin64, ratchets: bin,
                                       known: bin, name: str, first: uint, more: bool,
                                       [[seq, kind, id: bin32, at, packed], ...]]
/moved [token: bin16, cursor: uint] -> [status]
/group [client_id: bin16, name: str, [[address: bin16, name: str], ...]]
                                   -> [status, group: bin16]
/groupadd [client_id: bin16, group: bin16, [[address: bin16, name: str], ...]]
                                   -> [status, group: bin16]
/groupdelete [client_id: bin16, group: bin16] -> [status]
```

The status comes first in every answer: 0 ok, 1 not identified, 2 not
admitted, 3 unknown or used token, 4 malformed request, 5 the home node's
store failed, 6 behind (a `/moved` whose cursor is older than the
archive's newest entry).

`/move` and `/moved` (1281) move the mailbox to another device; see
[how Loquax reaches its mailbox](loquax-reachability.md), "Moving to a new
phone". They are answered only to an admitted device whose request
carries the token of the move the owner opened on the mailbox itself
(`loquax/src/home/handover.rs`), never to an admitted device alone.

`/group` (1307) makes a group the home hosts, its owner the admin
(`loquax/src/home/group.rs`, the study
`docs/drafts/group-chats-study-2026-11.md`): the group's key stays on the
home, an invitation from the owner to every member goes through the
outbox, and the group's own engine sends the owner the membership, so
every device learns the group from its archive. The same client id asked
again answers the group it made then. A home that archives an invitation
learns the group's key from it, so a Join from any of its devices goes
out before the group's announce is heard.

The invitation is a text every client shows, in English whatever the
admin's language (1348; a phone of 1307 pass six still sends its
language as a fourth element, which the home reads past and ignores):
"Lew invited you to Familie: lxma://… Reply /leave to leave." A Loquax
says the card in its reader's language from the `lev` key `0x04`
([the `lev` keys](loquax-read-receipts.md)). A member whose newest
message to the owner did not say it understands groups, or that never
wrote, is taken for a stock client (Columba, Sideband), and its text
adds once "Loquax shows members and names:" and the site's address
(`LOQUAX_URL`, the site's root until its download page is published).
The whole text stays under 300 characters with its 168-character code;
names too long for that leave the link out. A Loquax shows the card,
not the text. The admin's member sheet reads where each invitation
stands from its delivery in the member's chat: delivering, waiting at
the propagation node for the member to come online, not delivered, or
plainly invited once delivered.

`/groupadd` (1307 G2) adds members to a group the home hosts and its
owner is admin of: each new one gets an invitation from the owner, the
epoch moves, and every member that understands groups gets the new
membership from the group. A member already in it is skipped, so the
same request again adds nobody. Beside the requests, the home keeps a
group going on its own (G2): a copy's retries survive a restart (open
copies are made again from the archived original, byte for byte; one
whose text rule changed since, so that no copy made again has its id, is
made under the rule the program has and booked under the new id, where
the restart ended it before, 1406), each
group announces every hour on its own clock beside the announce at its
engine's start, a member whose last three proofs each took more than
2 s gets its lines held for 60 s and sent as one digest (`lev` key
`0x06`), and a reaction goes to every other member that is not slow
with its target rewritten to the id each one knows the message by: its
own copy, or its own original when it wrote the line. A stock member
gets it too, since Columba and Sideband draw reactions (1387, Lew
2026-11-03; G2 had given stock members none); a slow one gets none, as
a digest carries text only.

The writer's ticks (1307 pass four): the group's proof says the group
has it, one tick. Once every copy of a line sent alone is proven the
home sends the writer, when it understands groups, the status `lev`
key `0x03` with the counts (two ticks once all are proven), again for
every late copy a digest proves, and once every member that sends read
receipts read it. A read receipt a member sends the group is booked
against its copy (`group_fanout.read_at`); a member counts as a reader
while its newest message to the group said capability bit 0. A copy a
propagation node took is kept so (`group_fanout.propagated`, schema 13),
and a restart tries it direct again but does not upload it twice.

`/groupdelete` (1307 pass four) deletes a group the home hosts and its
owner is admin of, for everyone: every present member gets the last
membership with the standing ended (a stock member the text "Lew ended
Familie."), then the home forgets the group's rows, its archived
conversation and its key file. While the home runs, the group's engine
proves and drops every later message to the address, as a stranger's;
after a restart nothing answers at it, since the key is gone. A group
the home does not host answers `Ok` (deleted before, or never there), so
a lost answer asked again is no error; a group whose admin is someone
else is refused. The owner's devices forget the conversation whole when
the answer or the last membership comes; a member's device keeps the
history, closed, under the line "Lew ended the group.".

A `/sync` answer carries the entries after the cursor, oldest
first, up to 64 KiB of messages (one larger entry still goes, alone), and
`more` when others follow; the client asks again from the last sequence it
got, over the same link. An answer too large for one packet goes as a
response resource. `first` is the oldest sequence the archive still holds,
so a client whose cursor fell behind an eviction says so in its log.

The page bound (1363) and `left` (1415) are a narrow client's: it asks
pages of at most that many bytes and reads how many entries still wait.
The fold (1439) is a device's capability word, what it keeps of the
archive (`wire::Fold`, `loquax/src/home/wire.rs`). A board says `board`
and is served only the kinds `board::fold` reads (a message received or
sent, a mark, a name), none over 16 KiB, and `left` counts only those,
so its "Fetched N of M" is the M it will get: an archive whose first
796 entries are imported history reaches a board's first names on its
first page. The phone and the desktop say no word and keep the whole
archive; a home from before 1439 reads the first two elements and
serves everything, and an unknown word folds nothing away.

- **Fetching never deletes.** Only `/mark` changes state, and it does so
  by appending an entry, not by removing one (see "Read and sent state on
  every client").
- **Idempotent.** The same cursor gets the same entries. A client keeps its
  cursor in `$LNMSG_HOME/home-cursor` and moves it only after what it pulled
  is kept: the pull hands the entries to the engine with a fence behind them
  (`Command::Fence`), and once the fence is answered every event the entries
  caused has gone out before it. The screen saves the cursor when it takes
  the pull's end, after its store wrote those events; `fetch` saves it
  after its seen file. A crash in between pulls the same entries again, and
  the screen and `fetch` drop a message they already hold.
- **Each client has its own cursor**, so any number of them read the same
  archive.
- **The client checks authorship itself.** An entry is the LXMF message as
  the home node received it, signature and all. The client checks the
  signature against the sender's key when it knows it, and otherwise keeps
  the message unverified until the key arrives, as for any received message
  (leviculum 1079). A message addressed to anyone but the home identity, or
  whose signature does not hold, is dropped.

## The wake

Slice 3's first rung, the quiet link (leviculum 1123). Without it a client
learns of new mail only on its next pull: on open, on Refresh, or on the
schedule, fifteen minutes by default.

- **The client keeps one link** to `loquax.home` while it runs (the
  phone's session, the terminal), identified as the device, and asks
  `/watch` on it with its cursor. The answer is the archive's newest
  sequence, 0 for an empty archive, so a client whose cursor is behind
  pulls at once. The link stays up with the link's own keepalive; tuning
  that and the TCP keepalives for the battery is a later, measured step.
  A link that drops, or never comes up, is opened again after 2 s,
  doubling to five minutes, and after 2 s again once one stood
  (`loquax::home::watch::Backoff`).
- **The home node wakes every watching link** when the archive grows: a
  received message, a state change of a sent one, a mark. A name entry
  wakes nobody; it rides along with the next pull (1136). The wake is a
  channel message on that link, no content, only the newest sequence:

  ```text
  ["pull", newest: uint]
  ```

  A burst is coalesced: the first append after a quiet spell wakes at
  once, the rest of the burst is one more wake 500 ms after it, per link
  (`loquax::home::wake`). A link whose device was revoked since is closed
  instead; one that is gone is forgotten.
- **An append by another process wakes too** (1164). `loquax import` on
  the mailbox, or any other local tool, writes the store on a connection
  of its own, beside the running home node. The home node reads SQLite's
  `data_version` once a second (`daemon::OUTSIDE_LOOK`,
  `Store::data_version`): it moves for another connection's commit and
  never for the home node's own. When it moved, the archive's newest
  sequence of every kind is what the watching links are woken for, names
  and blocks included, since on the mailbox no device has them yet. A
  device that only watches therefore has an import within about a second
  plus its pull. A look is the file's shared lock and a read of its
  header, 7 µs on the build host, so an idle mailbox pays 7 µs a second.
- **The client pulls on a wake past its cursor**, with the `/sync` above
  on a link of its own; the phone notifies as for any pull. A wake older
  than the cursor pulls nothing.
- **A wake's pull is quiet** (1136). A pull counts the received messages
  it brought (`Pull::messages`), never entries: the user's own sent
  message coming back, a state change, a mark and a name are entries,
  and none is a new message. The status line says "pulling" only for a
  pull the user asked for (Refresh) or one that runs longer than a second
  (`loquax_android::pull_line`, `QUIET_PULL_MS` on the terminal); a quiet
  pull says its end only when it brought a message, and a result hides
  after four seconds. Before 1136 every heard name woke the phone, which
  pulled one entry and said "1 new message(s)" and "pulling" in turn
  without end.
- **The fallback stays**: pulls on open, on Refresh and on the schedule.
  While a screen of the app is in front (the `front` request of the
  phone's facade; the terminal is in front whenever it runs) and the
  watching link is down, a pull runs every minute.
- **Compatible both ways.** A client from before never opens the link
  and pulls as it did. A home node from before answers no `/watch`: the
  request runs out of time, the backoff grows to five minutes, and the
  client pulls on its schedule and every minute in front. `home_wake = off`
  in the home node's config answers `/watch` and sends no wake.

On loopback a wake reaches the client in the same 10 ms poll in which
the home node's archive line appears (`loquax/tests/home_node.rs`, with
`home_wake = off` as the positive control). The phone's session shows the
message 0.3 s after the sender's `run_send` returns, with no pull asked
(`loquax-android/tests/facade.rs`).

## Sending through the home node

Slice 2. A client hands each message to the home node with `/send`: a
16-byte id of its own making, the destination, title, content and fields
(each field's value as its raw msgpack, so the home node signs what the
client composed). The home node signs the message once with the identity,
puts it in its outbox and answers with the message id. The client is done
then and may sleep.

- **Idempotent by the client's id.** The same id handed in again is
  answered from the outbox with the message it made the first time, also
  after a restart; nothing is signed twice. A client whose answer was lost
  asks again with the same id, three times in all (`home::client::send`,
  the ask's `ATTEMPTS`).
- **The outbox is a table** (`home_outbox`, schema 5 of the store), one row
  a message: the client's id, the message id, the destination, the packed
  bytes, when it came, whether it is still retried, and whether a
  propagation node took a copy. A restart goes on with every message still
  retried, each due direct at once.
- **The policy**, a pure state machine in `loquax/src/home/outgoing.rs` with
  its tests beside it: direct over a link at once, so a recipient that is
  not there fails crisply. When the router's stock attempts end without a
  proof, the same packed message goes to the propagation node (the one
  `--pn`, the config or the last collection selected; with none, the
  message waits for the direct retries alone), and direct delivery is tried
  again on every announce of the recipient and every `outbox_retry_secs`
  (3600 by default), for `outbox_days` (7 by default, Lew's decision of
  2026-10-29). The announce is the retry that matters, since it says the
  recipient is there; the cadence covers an announce the home node missed,
  at one link attempt an hour. The router holds one entry per message id,
  so a message is never on two legs at once.
- **A direct leg nobody answers is given up within 45 s** (1328, 1331).
  The router's stock direct leg against a recipient that does not answer
  runs five link attempts, each retried twice by the core and followed by
  a path request: about six minutes on a 2-hop path, and on miauhaus legs
  ran past the stall bound below. A direct leg whose message has not
  reached `Sending` after `outgoing::ANSWER_SECS` (45 s; the reference's
  single link establishment waits 6 s per hop plus the first hop's time)
  is called off in the router, logged as `HOME outbox direct unanswered`,
  and counts as a direct failure: the propagation node is offered the copy
  on the next tick, before any announce restarts the direct leg. A message
  the node holds already is not offered again, and its line says so
  (`… the propagation node holds a copy`, 1406: the line offered the node
  every message, 22 of 27 at one restart of the field mailbox). The
  announce and hourly retries go on as before. The bound sits under the
  router's own end of such a leg: a recipient whose key the home never
  held fails there after five attempts 12 s apart, 60 s, never under
  50 s, while the bound, checked on a 1 s tick against a start in whole
  seconds, fires by 47 s. At 60 s the two met within a second and the
  daemon's start phase chose the winner (1331). A router that ends an
  unanswered leg first anyway is the same fact: the line is
  `HOME outbox direct unanswered … reason=router: …` and the copy goes
  to the propagation node at once.
- **A path response is no announce** (1330). Every direct leg called off
  or failed ends in a path request of the router's own, which a transport
  node answers from its cache within a minute. The router reports that
  answer as it reports an announce, flagged as a path response (Python's
  `is_path_response`), and the outbox starts nothing on it: taken for the
  recipient, it tried an offline member direct every 100 s on miauhaus
  after the copy was on the node. Only the recipient's own announce and
  the hour try direct again.
- **Every engine selects its own propagation node** (1328). A hosted
  group's copies go out on the group's engine, which runs no collection:
  the home's engine selecting a node selects nothing there, so before a
  copy's propagated leg the group's engine is told the node the home uses
  unless it reported holding it. A propagated leg's lines name the node
  (`pn=`, its first four bytes, as `PN_PULL` does), so a `handed-on` reads
  beside the node's own `PN_ACCEPT` (1330). A failed leg's log line carries the
  engine's reason (`reason=unsent: …`, `reason=rejected`, …), and after the
  router's state the transport's word on the last failed attempt
  (`reason=failed: delivery link closed (Stale)`, `reason=failed: upload
  transfer: …`), or `reason=failed: no attempt got a link` when none
  failed on one (1330).
- **A leg that never ends is given up** (1293). The outbox starts no leg
  for a message already on one, so a leg whose end the router never
  reports (a link that died mid-leg, a transfer that stalled, a message the
  engine refused without naming it) would hold the message forever. After
  `outgoing::STALL_SECS` without a sign of the leg (779 s: the 725 s the
  core takes to close a link that went silent, the router's five attempts
  10 s apart after it, and one 4 s processing interval as margin; every
  state the router reports for the message is a sign and moves the bound)
  the home node calls the leg off in the router, logs `HOME outbox <leg>
  stalled`, and archives one `stalled` entry. A stalled direct leg counts
  as a direct failure: the propagation node is offered the copy once, on
  the next tick, and direct retries go on at the next announce and hour. A
  restart needs nothing of this: the row keeps no leg, so every message
  still retried is due direct at once.
- **No propagation node selected.** With neither `--pn` nor a config entry
  and `--sync-interval off`, nothing selects one: a collection is what
  applies the mailbox rule of `loquax-mailbox.md`. The home node does not
  select one from the outbox path, because the rule reads the daemon's path
  table and may wait for announce replays, and the outbox's path must not
  wait. It logs once per message that only direct retries remain and why.
  With `--pn` it selects that node before the first upload.
- **A propagated copy needs the recipient's key** (1406). The copy on the
  node is encrypted for the recipient, so a recipient never heard (no
  announce ever reached the home node) cannot be propagated to. The leg
  fails at once, `HOME outbox propagated failed … reason=no key for
  <recipient>, path requested`, and the engine asks the recipient's path.
  The answer brings the key: a path response offers the node the copy
  once more, the recipient's own announce tries direct, and without
  either the hour's direct leg asks the path again. Before, the router
  held such a leg without a word (`propagation_runtime.rs`, the
  `Err(RouterError::Node(_))` arm after `ensure_prepared_upload`, which
  asks the path and charges no attempt), the line `HOME outbox propagated
  … pn=` promised an upload that never came, and the leg ended only at
  the stall bound, every hour for seven days. A leg's line is written when
  the router took it, so a leg the engine refuses leaves only its
  `failed` line. A sender writing to a known contact has its key.
- **DELIVERED when a direct proof arrives**, even after the propagation node
  took a copy. Both legs carry the same bytes, so the same hash, and the
  recipient keeps one message per hash
  (`reference/LXMF/LXMF/LXMRouter.py:1906-1908`): the copy it later collects
  is dropped there.
- **Every state change is an archive entry**, appended in the same
  transaction as the outbox row changes: `sent` (kind 1, the packed message
  as signed), `propagated` (2), `delivered` (3), `failed` (4: the seven days
  ended with no propagation node ever taking it) and `retries ended` (5: the
  direct retries ended while a copy waits on the propagation node), and
  `stalled` (11: a leg was given up and the message is tried again; the
  first per message). Kinds 2 to 5 and 11 carry the message id as their
  bytes.

On the client the compose line and `loquax send` go through `/send`. The
screen names the bubble after the id the home node answered with
(`Command::HomeAccepted`, reported as the router's `Queued` would be), and
each pull turns the state entries into the bubble's state: the sent entry
as sent (`Command::HomeSent`; the message left the device and the home node
has it), and through `Command::HomeState` propagated as at a propagation node,
delivered as delivered, failed as failed. A `stalled` entry changes no
state; through `Command::HomeStalled` the detail view's ledger says
"sending stopped without an answer, trying again". `loquax fetch` prints a
line `message <id> <state>` for each.

**A client pulls right after it sends**, so the bubble leaves queued
without waiting for the schedule: the screen starts a pull (sync reason
`send`) when the send runner ends, and another after a pull that was
already running, which may have asked before the home node had the
message. `loquax send` on a client pulls once after the home node took the
message and prints what that brings as `fetch` does, its own `accepted`
line among it: the device has one cursor, and what a pull took is shown or
lost.

## Read and sent state on every client

Slice 4. Every client shows the same conversations: what one reads is read
on all, what one deletes is gone on all, and what one sends appears as a
sent bubble on the others.

- **`/mark`** hands the home node a flag, read (0) or deleted (1), and one
  to 64 message ids. The home node keeps the request's bytes as an archive
  entry of kind 6, under the SHA-256 of those bytes, so the same mark twice,
  from one client or two, is one entry. Every client applies the entry on
  its next pull (`Command::HomeMark`, `OutboxEvent::HomeMarked`).
- **Seen** (flag 2, 1234) carries a third element, the reader's clock: the
  messages were on a device's screen, and the home node signs their read
  receipts (`loquax-read-receipts.md`, section 8). It keeps a seen mark of
  only the ids no earlier seen mark names, so the first device sets the
  read time and a second sends nothing new.
- **Read** means every received message up to the marked one in its
  conversation is read, as opening the conversation there read them: the
  unread count becomes the number of received messages after it. A device
  that has not pulled the message yet counts it unread when it comes, and
  the mark, which follows it in the archive, reads it. The screen marks the
  unread a conversation had when it is opened, and a message that arrives
  in the open conversation, which the screen never counts unread.
- **Deleted** removes the message by its id, from the screen if it shows it
  and from the store either way (`StoreWrite::ForgetId`). On a client the
  delete question says it reaches every device of the home; deleting a
  conversation marks every message of it the screen loaded.
- **Sent** is the `sent` entry of "Sending through the home node": a client
  that has no bubble with that id makes one from the packed message, as
  sent, and later entries move it on like any other.
- **Idempotent.** A mark applied twice changes nothing more, a message
  deleted twice is gone once, and a sent entry pulled again does not take a
  delivered bubble back to sent.

A mark goes on a task of its own once the fold made it; the screen does not
wait for it, and at the end waits for it up to a send's budget. A mark the
home node did not take is said in `tui.log` and not retried in this slice:
the other devices keep the message unread until the reader marks it again.

## Names the home node heard

1132. A client is a leaf with one interface, to the home node: it hears no
announces, so on its own it would never learn a display name and would
name every peer by its hash. The home node hears them, as the messenger
engine on the shared instance it is.

- **Only names that matter** (1136). The home node hangs on a transport
  that hears the whole network's LXMF announces; it archives the name of
  a peer the user has a relation to and of no other
  (`loquax::home::relation`): the sender of a message in the archive, the
  recipient of one sent, a destination a client handed in with `/send`,
  or a petname in the home node's contacts file (read at start). Every
  name heard is remembered, so when a conversation with a new peer
  starts, the peer's newest name goes into the archive at once. On the
  hamster home node before 1136, nine minutes brought 184 name entries of
  strangers and 167 wakes.
- **A name entry**, kind 7, is appended when such a peer announces a display
  name that differs from the last one the home node appended for it:
  `[address: bin16, name: str]`, the name as announced, and the entry's
  time is when the home node heard it. The home node compares the name
  sanitised as every screen sanitises it (`loquax::names::sanitise`) and
  keeps that in its own names table, so an unchanged announce, or one
  whose name sanitises to nothing, appends nothing, and a peer that goes
  back to an earlier name gets an entry again (`Store::archive_name`).
  Only a peer's newest name entry is kept: a new one removes those before
  it. The home node's own announce, echoed back, appends nothing.
- **On pull** the client hands each name entry to its engine
  (`Command::HomeName`), which reports it as `OutboxEvent::Announced`,
  exactly as an announce the client heard itself: the screen's `Names`
  sanitise it, mark a collision and note a change in the conversation as
  for any announce (1047), and the runner keeps it in the client's store.
- **Appearance.** An LXMF announce carries the display name, the stamp
  cost and the supported functions
  (`reference/LXMF/LXMF/LXMRouter.py:1048`), no icon; a peer's icon appearance comes in its messages' fields,
  which the archive already carries whole, so the avatars of 1129 need no
  entry of their own.
- **Before 1132** the archive has no name entries: a client learns a
  peer's name from the first announce the home node hears after it runs
  this version.
- **Every other name** (1643). The names of peers without a relation
  are no archive entries but rows of the mailbox's `home_names` table,
  every LXMF delivery announce's name with its first and last hearing
  and hops, at most 4 096: they ride in the `/pages` delta as row kind
  2, wake nobody, and the client folds them into its names as an
  announce (`loquax-mailbox.md`, section 5).

## The reader's names on every device

1139. A petname, the reader's own name for a peer, is the same on every
device of the home. The phone's address book link is not: which contact
and photo a phone shows stays on that phone
(`docs/drafts/loquax-android-contacts-2026-10.md`, section 6).

- **`/petname`** hands the home node the peer's address and the name, the
  empty string clearing it. The palette's `rename` in the terminal and the
  phone's Rename dialog send it on a client, after the contacts file took
  the name, on a task of its own, as a mark goes (`runner::hand_petnames`).
- **A petname entry**, kind 8, keeps the request's bytes. The same bytes
  as the peer's newest petname entry append nothing; a new one removes
  the peer's older ones, so a client behind the cursor gets only the name
  given last (`Store::archive_petname`). Like a name entry it wakes no
  client (`Store::archive_newest_waking`): it rides along with the next
  pull.
- **On pull** the client hands it to its engine (`Command::HomePetname`,
  `OutboxEvent::HomePetname`); the screen sanitises it as a petname from
  the contacts file, takes it as the reader's word and writes the
  contacts file, and never hands it back. A home node from before 1139
  answers `/petname` as malformed, which the client says in its log.
- **Owed until the home takes it** (1635). A petname the device set is
  owed until the home answers `/petname` with ok: the newest of each peer
  is kept in the device's store (`owed_petnames`, schema 19) and handed
  again with the next hand or after a restart, one hand at a time so the
  home takes a peer's names in the order they were set
  (`UiModel::take_petnames`, `UiEvent::PetnamesHanded`). While owed, a
  pulled entry for the peer changes nothing on screen: the home stamps an
  entry when it takes it, so the owed one will be its newest, and the
  pulled one is older, a name the reader cleared among them. A pulled
  entry that says the same is the home's echo and settles the debt. Once
  taken, the archive holds nothing older for the peer, but a pull already
  under way may; until that pull ends an entry counts only if the home
  took it after the device's answer came (its `at` past the device's
  clock then, `UiModel::home_petname`). A cleared petname is an entry
  with the empty name: the home needs no tombstone of its own, since its
  newest entry of a peer is the reader's last word.
- **Peer states are owed the same way** (1640). A pin, a mute, an
  archive or a block the device set is owed until the home answers
  `/peerstate` with ok, by the same rules and the same code
  (`ui::owed::Owed`, one per kind of word): the newest state of each peer
  in the device's store (`owed_peer_states`, schema 21), handed again
  after a restart, one hand at a time (`UiModel::take_peer_states`,
  `UiEvent::PeerStatesHanded`), over any pulled entry while owed and over
  an entry the home stamped before the answer until the pull ends
  (`UiModel::home_peer_state`, the pull now carries the entry's `at`). A
  pin loosened just before the app stopped no longer comes back from the
  home after the restart. Verified is not among them: it stays on the
  device that compared the key, and nothing of it goes to the home.

## Contact keys from a code

1187. A contact code, `lxma://<address>:<public key>` as Columba and
Sideband share it (`loquax::lxma`), carries the contact's key, so the
first message can go out before any announce of the contact was heard.
The key has to reach the home node, which signs and sends.

- **`/key`** hands the home node the peer's address and its identity's
  64 byte public key. The phone sends it when the user adds a contact
  from a scanned or pasted code (`Request::AddContact`), on a task of its
  own, asked again as a petname is.
- **The home node** refuses a device it never admitted, and a pair whose
  address is not the one the key makes (malformed). It archives nothing:
  the daemon remembers the identity in its node's known destinations,
  which the node keeps on disk as it keeps those it learned from
  announces (`server::key`, `daemon::answer`).
- **A home node from before 1187** answers `/key` as malformed. The phone
  says so once ("Your mailbox did not take the contact's key yet") and the
  first message waits for the contact's announce, as every message did
  before.

## Imported history

1150. An import on the home node (`loquax import <ratspeak.db copy>`,
`loquax-import.md`, "Ratspeak history") writes the imported messages into
the home node's store and appends each as an archive entry of kind 9, all
conversations in one time order. Ratspeak keeps no signed bytes, so the
entry is the row as the store keeps it, origin and peer first
(`loquax/src/import/archive.rs`), never a message anyone signed. The same
message twice is one entry, so a second import appends nothing. 1162: a
NomadNet tree or a Loquax export is archived the same way. Its rows carry
our verdict, the source's and, while the sender's key is not known, the
sender and the signed bytes; such a row is an entry of version 2, the
version 1 row with the three appended, and a row with none of them stays
version 1. The client stores what the home node stores, the held bytes
too, and checks them when the key comes.

- **On pull** the client keeps these entries apart from the engine's
  (`Pull::imported`) and writes them into its store as imported history,
  one conversation at a time, before the cursor may pass them
  (`runner::keep_imported`). 1159: the pull says how many rows were new
  (`PullEnd::imported`), and every frontend that ran it, the terminal, the
  window and the Android session, has its running model take them from the
  store (`runner::show_imported`, `UiModel::grown`): a conversation new to
  the screen is listed, one loaded down to its oldest message gets the
  rows above it, one with more still in the store loads them as the reader
  scrolls. On Android the session lives on in the service, so closing the
  app restarts nothing; the history has to show in the running session.
- **The contacts' names** travel as petnames (1159). An import on the home
  node merges the source's contacts into its contacts file and archives
  each name as a petname entry, the way a client's `/petname` is kept
  (`server::archive_petnames`), for NomadNet and Ratspeak alike. A peer the
  archive already holds a petname entry for, a cleared one too, keeps it:
  the reader's word on that peer was said. A home node that imported
  before 1159 holds the names in its contacts file alone; its start
  archives each one that has no petname entry and says how many in one
  line, `HOME petnames backfilled=N contacts=M blocks=B`. A second start
  archives nothing.
- **The blocks** travel as peer states (1162). Each peer the source
  blocked is archived as a peer state entry, blocked and nothing else,
  the bytes a device's block sends for a peer it holds no other state for
  (`server::archive_blocks`). A peer the archive already holds a peer
  state for keeps it: a device's word on it wins. The start's backfill
  does the same for the contacts file's blocks and counts them as `blocks`
  in its line.
- A client from before 1150 finds no message id in such an entry and
  passes it by, as it passes every kind it does not know. A client from
  before 1162 finds a version 2 entry of another shape, says `HOME pull:
  an imported entry of another shape` in its log and passes it by.
  `loquax fetch` keeps no history and passes them by too.
- **A running mailbox wakes its devices for an import** (1164, "The
  wake"): the import's commits move the store's data version, so a phone
  with the app closed, which only watches, pulls the history, the names
  and the blocks without anything else making it pull. The import is
  several commits; a pull between two of them brings the rest on the next
  wake.

## The archive

A table of the Loquax store (`archive`, schema 4 of
`docs/src/concepts/loquax-storage.md`'s store): one entry a received
message, with a sequence that only grows (`AUTOINCREMENT`, so a number is
never handed out twice, even after the newest entry was evicted), the kind
of entry, when it was recorded and the packed LXMF bytes. A message that
arrives twice, direct and again from the propagation node, is one entry.

**The cap.** 1 GiB by default, `archive_cap_mb` in `$LNMSG_HOME/config`.
That is twice what a stock lxmd propagation node keeps by default
(`reference/LXMF/LXMF/Utilities/lxmd.py:159-163`, 500 MB) for everybody's
mail, here for one person's. A text message packs to well under a
kilobyte, so the default holds about a million of them, and a home host
has the gigabyte to spare. The sent messages and their state changes go into the same log (see
"Sending through the home node"). When an append takes the archive over the cap,
the oldest entries go until it fits again; the entry just appended stays,
even when it alone is larger than the cap.

## Inbound on the home node

To every sender the home node is an ordinary `lxmf.delivery` destination:
senders deliver DIRECT and get their proof. It announces at start and every
360 minutes, the interval lxmd's config template offers for the same
announce (`reference/LXMF/LXMF/Utilities/lxmd.py:1140`). For a day after
its identity moved in (an import adopted it or brought its ratchets,
`$LNMSG_HOME/identity.moved`) it announces every 30 minutes instead, the
reference's ratchet interval, so peers that remember the old program's
ratchet hear a new one soon (order 1185, `loquax-import.md`, "Ratchets").
Its delivery destination has ratchets, as every Python LXMF router's does. It also collects
from its propagation node on the messenger's own schedule
(`loquax-mailbox.md`, "When to sync"), so mail that senders propagated
arrives too; those senders get no direct proof from it.

## Running it

The user sees the home node as **Loquax Mailbox** (Lew, 2026-10-30, order
1153; the onboarding draft `docs/drafts/loquax-onboarding-2026-10.md`,
section 1). The CLI says `loquax mailbox run|enable|disable|admit|join|
devices|revoke|tokens|revoke-token`; `loquax home ...` and `loquax --daemon`
stay as aliases, and the code, the destination `loquax.home`, the code
prefix `lqxh1` and the config keys keep `home`. "Mailbox" means only this;
somebody else's node is a propagation node.

**The system service.** On a PC or server the mailbox runs as a system
service from the package: `packaging/loquax/loquax-mailbox.service`,
installed disabled and not started, `loquax mailbox enable` runs
`systemctl enable --now`. It runs as the system user `loquax` (made by
the package's postinst), with `LNMSG_HOME=/var/lib/loquax-mailbox` (0700,
`StateDirectory=`), `After=lnsd.service`, `Restart=on-failure`, and
`--config /etc/reticulum` for the system `lnsd`'s shared instance. The
socket is abstract and needs no permission; the config is world-readable;
the RPC key is not (lnsd keeps `transport_identity` at 0600 since 1119), so
the service reads no path table and adopts no propagation node by itself,
and the config's `propagation_node` names one. Removing the package keeps
the state; purging deletes it, identity included. Windows and macOS come
later; the package is built for amd64, arm64 and, since 1153, armhf.

`loquax mailbox run` (or `loquax --daemon`) runs it in the foreground
beside a running `lnsd`. It takes the
messenger's `--instance`, `--config`, `--pn`, `--sync-interval` and
`--timeout`. It logs to stderr (`warn,loquax=info`, `RUST_LOG` replaces
it), one `HOME` line for each archived message, admission and refusal, and
never logs a message's text. Each collection from the propagation node
leaves one line, also when it found nothing (1293): `PN_PULL n=<messages>
pn=<first 8 hex of the node> next_in_s=<seconds to the next>`, so the
fifteen-minute cadence shows. Without the package, a systemd user unit:

```ini
# ~/.config/systemd/user/loquax-home.service
[Unit]
Description=loquax home node

[Service]
ExecStart=/usr/bin/loquax mailbox run
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
```

`systemctl --user enable --now loquax-home`, and `loginctl enable-linger
$USER` so it runs without a login. The package's `lnsd` is a system
service, which a user unit cannot be ordered after; a home node started
before `lnsd` exits 1 with "could not join the Reticulum shared instance",
and `Restart=on-failure` tries again ten seconds later. The daemon and the
messenger never run on one home at once: both would register the same
delivery destination, so each takes an `flock` on
`$LNMSG_HOME/delivery.lock` first and the second refuses to start, naming
the first (`loquax/src/home/lock.rs`). The kernel lets go with the
process, so a crash leaves nothing to clean up. `loquax send` and `loquax
fetch` on the home node's own home register the destination too, for their
run: they take the lock shared, so any number of them run side by side, none
runs while the daemon does (each refuses, naming its pid), and the daemon
does not start under one. Beside the messenger they run unlocked, as before
the lock existed.

On a phone the home node runs inside the app (1152): the same
`home::daemon::run`, with `DaemonUpstream::Mesh` instead of a shared
instance, because a phone has no lnsd. Its node dials the entry servers
and serves the app's own screens on a loopback port; the screens join it
with a token it issues itself and stay its client, so the local and the
external case are one code path. A `stop` channel ends it in place of
SIGTERM, which belongs to the app's process, and its propagation node is
selected from its own node's path table (`loquax-android/src/mailbox.rs`).

A client is a laptop that ran `loquax mailbox join`. Its messenger pulls at
start and on the propagation node's schedule (`F5` and `Ctrl-r` too) and shows the
home node's address as its own, as does `loquax address`; `loquax fetch`
pulls and prints. It sends through the home node (see "Sending through the
home node").

## Compatibility with Python peers

No wire change. Senders see an ordinary LXMF destination that announces
and accepts DIRECT, OPPORTUNISTIC and propagated-and-collected mail.
`loquax.home` is a new aspect that is never announced. The end-to-end tests in
`loquax/tests/home_node.rs` run stock Python LXMF peers against the
shipped binary. Slice 1: DIRECT delivery and DELIVERED, two clients pulling
the same message, a reused code refused, a revoked client getting nothing.
Slice 2: a client sends through the home node to a Python recipient that
is offline and exits; the direct leg fails, the recipient comes up later
and announces, the message arrives DIRECT and once, and the client's next
fetch shows it delivered; a second daemon on the same home is refused, and
so are a local `send` and `fetch` beside it. The propagated leg: the
recipient is offline and never heard, the direct leg fails, the home node
uploads the same bytes to a Python propagation node once the recipient has
announced once, the recipient collects it there and announces, the home
node's direct retry is proven, and the recipient holds one message; the
client sees propagated, then delivered. Slice 4: two scripted TUI clients,
one reads a Python sender's message and answers, the other shows it read
and the answer as a sent bubble.

## Slices

1. Home archive and pull. Built.
2. Send through the home node: `/send`, the outbox, direct with a
   propagated copy and seven days of direct retries. Built, with the three
   limits slice 1 left: the delivery lock, the cursor after the store, and
   token expiry.
3. Quiet link: TCP liveness in lnsd, the per-link keepalive, AP mode, a
   wake push. The wake over the kept link is built ("The wake"); the
   keepalive tuning, the TCP liveness and AP mode wait for the battery
   measurement.
4. Multi-client state: `/mark`, read and sent state on every client.
   Built, without a retry for a mark the home node did not take.
5. Pairing UX: the address in the code, the QR code in the terminal,
   `revoke-token`, the combined code on the phone's join screen. Built;
   the QR code in the GUI and the hand-over from a one-device setup are
   open.

The rest of the plan (the M9 as a client, board sleep, Android) is in the
draft's section 5.
