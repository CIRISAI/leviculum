# Loquax: the propagation node, and who you talk to

Part of the [Loquax design record](loquax.md). Two decisions live here:
when and how the client talks to a propagation node, and how it names and
trusts the people on the other end. They belong together because the trust
model is what gates the choice of node.

A word on the word (Lew, 2026-10-30, order 1153): "mailbox" means only the
user's own always-on machine, Loquax Mailbox
([the home node](loquax-home-node.md) in the code). Somebody else's
store-and-forward node is a **propagation node** in the UI, the man page
and this page; it was called the mailbox here and on the screen before
1153. The file name of this page and the code's names (`loquax::mailbox`,
`MailboxRun`, the `MAILBOX` events) keep the old word, because they are
cited and grepped.

## 1. Decision: propagation node interaction

This is where a naive design produces a client that silently loses mail,
and the library has arranged things so that the naive design is the
default: **nothing syncs unless the application asks**
(`request_messages_from_propagation_node`,
`leviculum-lxmf/src/router/propagation_runtime.rs:1498`, and
`next_deadline()` returns `None` outside `PathRequested`,
`leviculum-lxmf/src/router/propagation_runtime.rs:1238-1249`).

### When to sync

**Options**: on demand only; on a timer; on start plus timer; on announce
of the selected node; adaptive.

NomadNet uses a six-hour timer with a limit of eight messages and no sync
on start (NomadNet's `NomadNetworkApp.py`, lines 148-150 and 456-471). Six
hours is a very long time for something calling itself a messenger. columba
makes the interval configurable.

**Decision (2026-08-10)**: sync on start, on resume from suspend, on a
manual key, opportunistically when the selected node announces (free
evidence that it is reachable right now), and on a timer with **asymmetric
adaptivity**:

- The configured interval — default fifteen minutes — is a **hard upper
  bound that is never exceeded**. That is the promise the user can rely on:
  mail is at worst one interval old, always.
- After activity (a sync that returned something, or an outbound send), the
  client syncs more often for a while — on the order of every two minutes —
  decaying back to the bound. This is the chat-feel half of adaptivity
  ([the input-model decision](loquax-ui.md)), applied to the propagation node.
- The dangerous half — silently lengthening the interval when syncs come
  back empty — **does not exist**. Full adaptivity was rejected because an
  interval that quietly stretches is exactly the "why didn't I get that
  message for two hours" machine, and unpredictability in a messenger is a
  breach of trust.

Never sync while a direct link to the peer is up and working, because that
spends airtime to learn nothing.

The bound is configurable, its cost is stated in the manual in airtime
rather than in seconds (on LoRa a fifteen-minute poll is not cheap), and
the status line always shows when the next sync is due — the current
cadence is visible, never inferred.

### The schedule as built (order 1068, 2026-10-27)

The rule above lives in `loquax/src/sync.rs`, a pure function of events
and a monotonic clock in the `no_std` core; the terminal hands it the
clock and runs the fetch it finds due, one at a time. The constants:

- **Bound** fifteen minutes (`DEFAULT_BOUND_MS`), from `sync_interval` in
  the config file or `--sync-interval`; `off` leaves start, resume and the
  key. The next sync begins no later than the bound after the last one
  *began*, whatever else happened; only a sync that itself runs longer
  than the bound gets past it.
- **Activity** every two minutes for ten minutes after a sync that
  returned something or an outbound send (`FAST_MS`,
  `ACTIVITY_WINDOW_MS`), then the bound. Two minutes is the shortest wait
  in which a reply to what was just sent is plausibly back at the node,
  since the peer has to read and answer it first; ten minutes covers a
  conversation's quick exchanges and then lets go, so a burst is five
  syncs, about 11 s of airtime on LoRa at SF8 (loquax(1), PROPAGATION NODE). A step
  rather than a gradual decay, because the cadence the header shows must
  be one a reader can predict. Below the default bound both constants
  shrink with it at the same ratio, so a burst stays five syncs whatever
  the bound, and the bound is never undercut by its own "faster" mode.
  The activity cadence never pushes a sync past the bound: a send a
  minute before the bound keeps the bound.
- **Announce** of the selected node syncs, unless a sync began within
  sixty seconds (`ANNOUNCE_GAP_MS`): a node that re-announces on every
  interface change cannot drive syncs. It is a timer sync, off with the
  timer.
- **Failure** retries after thirty seconds, doubling, never later than
  the bound (`RETRY_FIRST_MS`); a success ends the backoff. The reader is
  told the first failure of a row, not every retry's.
- **Resume** from suspend is a wall clock that ran thirty seconds or more
  ahead of the monotonic one between two ticks (`RESUME_GAP_MS`): Linux's
  monotonic clock stands still in suspend.

The node's announce reaches the screen as `OutboxEvent::NodeAnnounced`,
which the engine emits for every `lxmf.propagation` announce it sees; the
screen compares it with the node it shows. The rule "never sync while
a direct link to the peer is up" is not built yet: a sync is per node,
not per peer, and the engine does not report its direct links to the
screen.

### What the user sees

The library hands over a fourteen-state machine
(`PropagationClientState`,
`leviculum-lxmf/src/router/propagation_runtime.rs:87-102`), progress as an
`f32`, transfer size, and a result of `{ received, duplicates }`
(`PropagationSyncResult`,
`leviculum-lxmf/src/router/propagation_runtime.rs:105-110`). That is more
than enough to be honest.

A permanent one-line status, taking NomadNet's best idea (its
`Conversations.py`, lines 517-548) and refusing its modal dialog. Something
like:

```
propagation node  a1b2c3d4 Node-Name   3 hops   last sync 4m ago, 2 new   next in 11m
```

and during a sync the same line becomes the progress display, naming the
state in words: "asking the network where the node is", "connecting",
"asking what it has", "downloading 4 of 7". columba's plain-English state
descriptions are better than NomadNet's terse ones and both are better than
a bare progress bar.

**When there is no reachable node**, the line must say which of the several
different failures happened, because they need different fixes:

| Library state | What the user must be told |
|---|---|
| no node selected | "no propagation node chosen"; offer the picker |
| `NoPath` (`leviculum-lxmf/src/router/propagation_runtime.rs:96`) | "cannot find a route to the propagation node"; it may come back |
| `LinkFailed` (`leviculum-lxmf/src/router/propagation_runtime.rs:97`) | "the propagation node did not answer" |
| `NoAccess` (`leviculum-lxmf/src/router/propagation_runtime.rs:100`) | "the propagation node refused you"; this one will not fix itself |
| `NoIdentity` (`leviculum-lxmf/src/router/propagation_runtime.rs:99`) | "the propagation node does not know your key" |
| `TransferFailed` (`leviculum-lxmf/src/router/propagation_runtime.rs:98`) | "the transfer broke"; will retry |

A single "sync failed" for all six is the lie this section exists to
prevent.

### The non-interactive slice's selection order (decided 2026-09-12)

The shipped `loquax send`/`loquax fetch` have no picker to offer, so the
CLI resolves the node in a fixed order: the `--pn` flag, then the
`propagation_node` key in `${LNMSG_HOME}/config` (the persisted
default), then the most recently announced node heard while attached.
Recency rather than the library's route/cost ranking, because for a
short-lived CLI the node that just announced is the one whose
reachability is *evidence* rather than cache; the reference
`LXMRouter` ships no autoselection at all — its clients pick, each
with their own rule (NomadNet by hops among trusted, columba by hops).
Nothing is auto-adopted silently in the TUI sense: a cron job's
operator wrote `--pn` or the config key, and the announced fallback is
for the interactive shell where the operator reads the `LNMSG_PN`
line (since order 967 that line's source word is `mailbox`, `adopted`
since 1153, and a run whose path table cannot be read keeps a stored node
but adopts no new one, as below). What is persisted where: the identity at
`${LNMSG_HOME}/identity`, the node default in `${LNMSG_HOME}/config`, the cross-run seen-message
ids in `${LNMSG_HOME}/seen`; the selection itself is per-run and never
written back. (Superseded 2026-10-25: without `--pn` or the config
key the adoption rule below decides, and its choice is written to
`${LNMSG_HOME}/propagation_node`, `${LNMSG_HOME}/mailbox` before 1153.)

### Which node, and the trust question

NomadNet auto-selects the fewest-hops node **whose trust level is
TRUSTED** (its `NomadNetworkApp.py`, lines 607-631). columba auto-selects
the fewest-hops node, full stop. The library's own auto-selection ranks by
route, hops, peering cost and stamp cost
(`select_outbound_propagation_node`,
`leviculum-lxmf/src/router/propagation_runtime.rs:1269-1311`) with no trust
input at all, because it has no notion of trust.

Your propagation node sees the *envelope* of every message sent to you: who sent it
and when, even though it cannot read the content. Handing that to whoever
happens to be nearest is a real privacy decision, and neither prior-art
program presents it as one.

**Never auto-adopt silently.** On first run, and whenever the selected node
becomes unreachable, present a picker with the candidates, their hop
counts, their advertised limits and costs (`PropagationNodeAnnounce`,
`leviculum-lxmf/src/propagation.rs:520-532`), and require one keystroke to
accept. Automatic *failover* between nodes the user has already approved is
fine and the library already does it
(`leviculum-lxmf/src/router/propagation_runtime.rs:825-846`); automatic
*adoption* of a stranger is not.

**Amended 2026-10-25, Lew.** The paragraph above is superseded: zero-config
is the rule for the messenger too ([Zero-config](zero-config.md)), and a
picker in front of the first message is a settings dialog by another name.
The messenger now adopts a node by itself, on first run and whenever the
selected node becomes unreachable, and **shows** the choice; the picker is
where the user changes it, not a gate before it. Adoption makes a node
*selected*, never *trusted*; the envelope argument above still stands, and
it is why the choice is shown rather than hidden.

One node is never adopted: **the one on this device**. A propagation node is where
others deliver while the messenger is off, and a node that is off whenever
the messenger is holds nothing for it. The exception is the household: a
stationary board or host that stays on is a fine propagation node for a phone or a
laptop messenger, so the rule is "not the node on this device", not "not
our node". A user who names the own node explicitly keeps it.

How Loquax tells the node on this device apart, measured 2026-10-25 (order
967): a propagation node beside the daemon reaches it through a local
client, so the daemon's path table holds it **zero hops away behind
`LocalInterface[…]`**; a node anywhere else is at least one hop out on a
real interface. lnsd with lnpnd and rnsd with lxmd give the same entry,
so Loquax reads the path table over the shared-instance RPC
(`leviculum-std/src/daemon_rpc.rs`) and needs no new surface. The rule is
`loquax/src/mailbox.rs`; its inputs, the persisted selection
(`${LNMSG_HOME}/propagation_node`, the hash and `adopted` or `user`;
renamed once from `mailbox` at start) and the `propagation node:` line are
`loquax/src/mailbox_state.rs`. A client hears only what
is announced while it is attached, so a first run asks the daemon to
replay the announces of its nearest path-table entries and selects once
they are in (`DISCOVERY_LIMIT`, `loquax/src/engine.rs`). A daemon whose
path table Loquax cannot read (another user's `transport_identity`) gets
no new adoption, since nothing then tells the own node from another; a
stored node, `adopted` or `user`, is kept and used, because it was vetted
against the table when it was written or chosen by the user, and an
identity does not move onto this device by itself. The run then prints
`propagation node: <8 hex> kept (path table unreadable)`.

The replay is lnsd answering from its cache: a path request from a local
client for a destination the path table holds is answered with the cached
announce, sent to the requesting interface alone, and the handler returns
without forwarding the request (`handle_path_request`,
`leviculum-core/src/transport.rs:12312`, the local-client arm at
`:12157-12202`, the send at `:12195`; pinned by
`test_path_request_from_local_client_returns_cached_announce`, `:30745`,
which asserts no send to the network interface and no
broadcast). A table entry whose cached announce was evicted falls through
and is forwarded (`:12211-12218`), so such an entry costs one request on
the network.

Limits, measured and reasoned:

- A transport node with a large table: a propagation node beyond the 32
  nearest entries (`DISCOVERY_LIMIT`) is not seen on a first run. The
  picker covers it.
- The replay costs up to 2 s of `--timeout` (`DISCOVERY_WINDOW_MS`) when
  entries do not answer.
- The 32 entries and the 2 s are starting values, confirmed by Lew on
  2026-10-25: the picker covers the gap, and a field finding raises the
  number or pre-sorts the propagation node candidates.
- A client attached over a non-loopback shared-instance TCP port counts as
  this device, because the daemon names it `LocalInterface[...]` too
  (`leviculum-std/src/interfaces/inventory.rs:102-107`). This is rare: the
  reference binds the shared-instance port to loopback
  (`LocalInterface.py:408`).

**What "unreachable" means (order 1072, 2026-10-27).** Two things. A
selected node with no entry in the daemon's path table is unreachable at
once, as before. A node with an entry that does not answer is unreachable
after **three syncs in a row that failed before a link to it came up**: no
path, or the link not established before the link timeout or the fetch's
budget (`UNANSWERED_LIMIT`, `loquax/src/mailbox.rs`). A failure after the
link, a refused request or a broken transfer, is an answer and ends the
streak, as does a completed sync. One miss is no verdict: on a path of
several hops a link request or its proof is lost now and then (of nine
link requests to a node five hops out, measured on hamster 2026-10-27, at
most three were answered). Three in a row, at the schedule's retries of
30 s and 60 s, is three to five minutes in which the node answered nothing.
The node is then passed over for an hour (`UNANSWERED_EXCLUSION_MS`): the
next selection adopts the most recently heard other candidate by the rule
above, and the messenger shows `propagation node <new> adopted, <old> did
not answer` as a toast and in `tui.log`. With no other candidate the silent
node is kept; a node that might answer later beats none. A node the user
chose (`--pn`, the config, or `user` in the selection's file) is never
replaced; the messenger only says it did not answer. The streak lives in
the running messenger, which syncs again and again; a single CLI run has
no streak.

Note that NomadNet's trust propagation makes this worse: trusting a person
auto-trusts their node (its `Directory.py`, lines 198-202), which makes it
eligible as your propagation node. Do not inherit that.

### The purge default

`retain_synced_on_node` defaults to `false`
(`leviculum-lxmf/src/router/propagation_runtime.rs:77`), so by default the
client tells the node to delete what it has collected. That is the right
default for privacy and for the node operator's disk, and it is the wrong
default for a user who runs two clients on the same identity, because the
first one to sync takes the mail. This must be a visible setting with the
consequence spelled out, not a config-file default nobody reads. Loquax
does not expose it yet: it enables the client with
`PropagationClientConfig::default()` (`loquax/src/engine.rs:1543`).

Set to `true`, our client keeps every message on the node. That is a
deviation from the reference client, whose flag only keeps messages it
already holds out of the have-list
(`reference/LXMF/LXMF/LXMRouter.py:1581-1583`) and which still
acknowledges every download unconditionally (`LXMRouter.py:1623-1638`,
"TODO: Add option to keep messages on node"); the node deletes on that
acknowledgement (`LXMRouter.py:1508-1519`). So with the reference client
the flag does not keep a fresh download on the node, and a second client
would never see it. Ours withholds the acknowledgement as well
(`leviculum-lxmf/src/router/propagation_runtime.rs:664`). Wire-neutral:
the node holds an unacknowledged message until expiry, exactly as for a
client that drops before acknowledging. The same client does not fetch it
again, since the message is in its router's store and the next list's
have-path filters it. The cost is the node's disk: retained mail stays
until the thirty-day expiry or eviction, and a node that caps its list
response (a board's serve limit) lists only the smallest messages, so a
large backlog of retained mail can push a newer, larger message off the
list until older ones expire.

### What the client must implement itself

- The sync schedule (there is none in the library).
- Persistence of known propagation nodes and of the selection, since
  neither is in the router snapshot (`snapshot`,
  `leviculum-lxmf/src/router.rs:2126-2143`); replay via
  `restore_known_propagation_node`
  (`leviculum-lxmf/src/router/propagation_runtime.rs:1450`).
- Re-selection after restart.
- Proof-of-work for `PropagationStampPending`, off the core lock.
- Calling `persist()` on `PersistenceRequested`.

### The propagation node as a visible relationship (proposal)

The propagation node is currently magic in every client: something chosen
for you, syncing on a schedule you did not set, holding mail you cannot
see. Make it a first-class object in the UI, with its own screen: who it
is, how many hops away, what it advertises, when you last spoke to it, what
it is holding for you if it will say, and whether it is purging what you
collect. columba's trick of modelling the relay as a contact with an
`isMyRelay` flag is a cheap way to get there.

The honest version of this includes telling the user what the node
learns about them: the envelope of every message they receive. No prior
program says this out loud.

## 2. Decision: identity, contacts, and names

The address is the identity. A 16-byte destination hash, rendered as 32 hex
characters, is the only thing that is true about a peer.

The delivery announce carries a display name as arbitrary bytes
(`DeliveryAnnounce`, `leviculum-lxmf/src/announce.rs:39-44`), and
`display_name()` strips NUL and trims but does nothing else
(`leviculum-lxmf/src/announce.rs:164-168`). The announce *is* signed: the
Reticulum announce signature covers the app data
(`leviculum-core/src/announce.rs:104`, `:213`: "the signature covers
`destination_hash + public_key + name_hash + random_hash + [ratchet] +
app_data`"). So a verified announce proves that the holder of that key
chose that name. It proves nothing about uniqueness, and there is no naming
authority in Reticulum. Two identities can both announce "Lew", and one of
them can be doing it on purpose.

Two further facts a UI must not paper over. `Message::verification` can be
`Unverified` when the source identity has never been announced to us
(`leviculum-lxmf/src/message.rs:232-234`), and such messages **are
delivered to the application anyway**
(`leviculum-lxmf/src/router.rs:1485-1488`). And the router discards the
display name from announces entirely, so **the client must maintain its own
hash-to-name map** from raw `NodeEvent::AnnounceReceived`.

### Options for naming

**A. Announced name only.** What most chat UIs do. Simple, and
impersonation is trivial.

**B. Local petname only.** Nothing is displayed until the user names the
contact; strangers show as a hash prefix. Maximally safe, maximally
tedious, and hostile to the case where someone new writes to you.

**C. Petname wins, announced name shown as provenance.** columba's
precedence chain (nickname, then announced, then stored), with the
announced name still visible somewhere.

### Decision

**C, plus NomadNet's collision check, plus a hash that never fully
disappears.**

1. **Display**: petname if set, otherwise the announced name, and in both
   cases a short hash suffix. NomadNet suppresses the hash for trusted
   peers (its `Directory.py`, lines 277-297); we shorten it rather than
   suppress it, because a four-character hash costs nothing and makes
   "wait, that is not the Lew I know" possible at a glance.
2. **Collision warning**, the one genuinely novel thing in the prior art:
   if an announced name is already claimed by a different address, mark it
   (NomadNet's `Directory.py`, lines 306-320). Extend it to a
   **name-change warning**: if an address you have talked to announces a
   different name than last time, say so once in the conversation. That is
   cheap, and it is the actual impersonation vector.
3. **Sanitise names.** They are arbitrary bytes off the wire. Strip control
   characters, normalise, cap the display width, and refuse to let a name
   contain something that renders as a checkmark or as another contact's
   name. NomadNet does this because micron markup in a name would otherwise
   render (its `util.py`, `strip_modifiers`).
4. **Unverified messages must look different.** NomadNet's "Unknown Origin"
   / "Invalid Signature" plain-English rendering (its `Conversation.py`,
   lines 620-630) is right; a coloured glyph alone is not.
5. **Refuse to compose to an address whose key is unknown**, with the
   reason and a "ask the network" action, as NomadNet does (its
   `Conversations.py`, lines 2186-2204). The library will tell you: our own
   helper checks `core.storage().get_identity(&peer)` before composing and
   reports which call was skipped rather than timing out later
   (`leviculum-lxmf-node/src/processor.rs:1241-1250`).
6. **Contact status as persisted state**, columba's `ACTIVE` /
   `PENDING_IDENTITY` / `UNRESOLVED`, rather than a live lookup, so the
   list can be rendered without touching the network.

**Built 2026-10-26 for `loquax tui`** (leviculum 1047): points 1 to 3 and
the three trust states below, in `loquax/src/names.rs`, from the router's
`PeerAnnounced` (the hash-to-name map is the client's, as this section
asks). Petnames come from `${LNMSG_HOME}/contacts`. Where the build fills a
gap this page leaves (the width cap, the collision glyph, what blocking
does) is listed in [keys, mouse and layout](loquax-keys-and-layout.md),
section 5, "Built 2026-10-26, names".

### Blocking, and names from the palette (Lew, 2026-10-28)

**Decision**: blocked means **stored silently**, as Signal does. A message
from a blocked address is accepted and proven as any other (the sender sees
it delivered, and nothing on the wire differs), and it is stored; it is not
shown in the list, not counted unread, raises no toast or notification, and
its telemetry pins nothing on the map. The palette's `blocked` lists the
blocked conversations, so nothing is lost, and unblocking shows the
conversation with its history.

Petnames and blocks are set from the palette: `rename` takes the petname as
text in the palette (an empty line clears it), `block` and `unblock` act on
the open conversation, which is also the list's selected entry. The
contacts file stays the storage, written atomically, with its comments and
the lines about other peers kept as they are.

**Built 2026-10-28** (leviculum 1082): `loquax/src/ui/contacts.rs` holds
the palette's side and the rule what a blocked peer's conversation is
hidden from (`UiModel::shows`: the list, the search, the unread count, the
toasts, the map pins); the receive path in `loquax/src/ui.rs` stores the
message before it asks. `names::edit_contacts` rewrites only the peer's own
`name` and `block` lines, and `names::save_contact` writes the file through
a rename. The petname to edit stands in the palette when it opens, so a
second `Enter` keeps it rather than clearing it. Positions from a blocked
peer are kept with their messages, and the map leaves them out until the
block is lifted. What the wire does is unchanged by construction: blocking
lives in the screen, below which the engine and the router never learn of
it.

### Trust levels: how many?

NomadNet has four — `WARNING`, `UNTRUSTED`, `UNKNOWN`, `TRUSTED` (its
`Directory.py`, lines 410-413); columba has none. Four levels with a radio
group is more ceremony than most users will perform.

**Decision (2026-08-10)**: exactly three states that a user reaches by
accident and understands: **known** (I named this contact), **unknown** (I
have not), and **blocked**. Trust is not a thing to configure but a thing
that follows from having named someone, which is an action people take
anyway.

A derived fourth state ("suspicious", computed from name collisions and
name changes) was considered and **rejected**: on a mesh, two identities
sharing a display name and a contact renaming themselves are normal
occurrences, not indicators of attack, and a state that cries wolf on
normal behaviour trains the user to ignore it. The signals themselves are
not discarded — they are a *rendering* matter, not a trust matter: when two
contacts share a display name the list must disambiguate them (short hash
suffix), and identity, not name, is always what messages are keyed by. The
columba defect this section opened with was the missing trust anchor for
relay auto-selection, and three states cover that: only **known** contacts
qualify.

The identity file itself must not behave like `lnomad`'s
(`load_or_create`, `lnomad/src/identity.rs:39-53`, silently regenerating on
a decode failure). A corrupt identity is a refusal to start with a clear
message, because minting a new one silently changes the user's address.
