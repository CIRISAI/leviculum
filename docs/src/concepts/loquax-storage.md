# Loquax: conversation storage

Part of the [Loquax design record](loquax.md).

The library stores one key (`ROUTER_STATE_KEY`, `b"lxmf/router-state"`,
`leviculum-lxmf/src/router.rs:65`) and hands each received message to the
application exactly once. **All history is the client's problem.**

## Options

**A. NomadNet's shape: a directory per conversation, a file per message.**
*For*: trivially crash-safe per message if each write is
temp-plus-rename; human-inspectable; deleting a conversation is `rm -rf`;
no dependency. *Against*: one inode per message forever; listing a
conversation is O(n) `listdir`; no search without reading everything;
NomadNet needed a `.index` sidecar that duplicates every message body on
disk (NomadNet's `Conversation.py`, lines 944-960) and still starves file
descriptors under announce storms (NomadNet commit `7bc6911`). On a
Raspberry Pi with an SD card this is the worst option.

**B. An append-only log per conversation plus a separate index.** Messages
appended as length-prefixed msgpack; an index mapping message ID to offset;
compaction on deletion. *For*: appends are one write and one fsync;
sequential reads are fast; the format is simple enough to recover by hand.
*Against*: you are writing a small database, including the index, the
compaction and the crash-consistency argument between them.

**C. SQLite.** One file, one `messages` table with the columns columba
already proved out. *For*: paging, search (FTS5), indices, transactions and
crash safety all solved by someone else; a 2 GB history is unremarkable;
`sqlite3` on the command line is the debugging tool. *Against*: `rusqlite`
with `bundled` compiles SQLite's C into the binary. `lnomad` treats a C
library in the path of the musl-static `.deb` as disqualifying
(`lnomad/Cargo.toml`, the `ratatui-image` comment), though that was about a
`pkg-config` probe for a *shared* library rather than a vendored static
one. **Checked empirically 2026-08-08**: `rusqlite` with `bundled` compiles
clean against `x86_64-unknown-linux-musl` on the project toolchain —
statically linked binary, SQLite 3.46 embedded, FTS5 verified working by
query, 2.6 MB total, 25 s build. The `lnomad` disqualifier was a
`pkg-config` probe for a shared library and does not apply to the vendored
static build.

**D. A pure-Rust embedded store (`redb`, `sled`, `fjall`).** *For*: no C
toolchain, ACID, ordered keys, so range scans give paging for free.
*Against*: no query language and no full-text search, so search is
hand-rolled; another dependency to trust with the user's mail.

## Decision (2026-08-10)

**C — SQLite**, via `rusqlite` with the `bundled` feature. The packaging
question was answered by the build probe above, so the fallback to D is
retired. The schema below, with its three commitments (identity-scoped
rows, two timestamps sorted on `COALESCE(received_at, timestamp)`, raw
msgpack `fields`), is the starting point; attachments live out of line as
content-addressed files. Retention is settled further down.

The schema to start from, taking columba's two good decisions:

```
messages(
  id BLOB, identity BLOB, conversation BLOB,
  direction INT, state INT, method INT, verification INT,
  timestamp REAL,        -- the sender's clock, from the wire
  received_at REAL,      -- our clock, when we saw it
  title BLOB, content BLOB, fields BLOB,   -- fields as raw msgpack
  error TEXT,
  PRIMARY KEY (id, identity))
```

with an index on `(conversation, identity, COALESCE(received_at, timestamp))`
and one on `(conversation, identity, direction, read)`.

Three reasons for the shape:

1. **Every row scoped by local identity.** A user may hold several
   addresses; columba's composite keys make that free, and retrofitting it
   later means a migration.
2. **Two timestamps, sort on `COALESCE(received_at, timestamp)`.** The wire
   timestamp is the sender's clock, and nothing makes another node's clock
   trustworthy. Sorting on it puts a peer with a wrong clock at the top or
   bottom of your history forever. NomadNet exposes this as a user-facing
   sort toggle, which is not a fix. (Sub-second precision is not the
   problem it once was on our side: the emission timestamp carries it, as
   the reference's `time.time()` does, because at whole-second granularity
   two identical messages created inside one second collapse to one ID —
   `leviculum-lxmf/src/router.rs:593-597`, Codeberg #217. Precision and
   skew are different failures, and only the second one is a sorting
   question.)
3. **`fields` stored as raw msgpack, not exploded into columns.** Unknown
   fields round-trip byte-for-byte in the library
   (`leviculum-lxmf/src/message.rs:5-8`) and must round-trip here too, or a
   reply to a message from a newer client loses information.

**Attachments out of line**, as NomadNet does (its `Conversation.py`, lines
752-812): content-addressed files under an `attachments/` directory, with
the row carrying names and hashes. Blobs in the database make the database
the size of the blobs, and a 2 MB voice message has no business in a row
you page through.

## Retention, decided 2026-08-10

Neither prior-art program prunes, and both will therefore eventually fail
on small hardware. The decision:

- **Message text is kept forever.** A million messages are a few hundred
  megabytes, SQLite territory, and the searchable archive is precisely the
  value the FTS decision bought.
- **Attachments live under a total-bytes budget** (default about 500 MB,
  configurable, numbers stated in the manual), evicting oldest first — the
  same pattern as `lnomad`'s byte-budgeted image cache
  (`lnomad/src/image_cache.rs:11-14`).
- **Evicting an attachment never touches the message row.** The message
  keeps the attachment's name, hash and size and renders "attachment
  (2.1 MB), evicted under the storage budget on <date>". The history does
  not lie, it just gets lighter.
- **Ledger rows** ([the delivery ledger](loquax-ui.md)) share the message's
  retention, so for text they live forever; no extra rule.
- A **per-conversation opt-in age cap** remains open as a possible later
  addition and was deliberately not built now.

## On the 2 GB Raspberry Pi question specifically

With option C, a 2 GB history is roughly ten million short messages, and
the operations that matter are "open the last screenful of a conversation"
and "search". Both are index lookups and neither touches the bulk. With
option A, opening a conversation reads every file in it. That asymmetry is
the whole argument.

## Durability rule, from `lnomad`'s counter-example

Nothing in this program may use `fs::write` on a file it cannot afford to
lose, and nothing may treat a corrupt load as an empty load
(`lnomad/src/bookmarks.rs:121-136`). Bookmarks can be silently forgotten.
Mail cannot.

## Built 2026-10-27 (leviculum 1049)

Option C as decided, in `loquax/src/store.rs` behind the `history` feature
(which `tui` turns on), at `${LNMSG_HOME}/history.sqlite3`: `rusqlite`
0.40 with `bundled`, the version `leviculum-cli` already links, so the
musl-static `.deb` gained no new crate and its third-party notices did not
change. The screen's side is `loquax/src/ui/history.rs`; slice 6 of
[keys, mouse and layout](loquax-keys-and-layout.md) says what the reader
sees, section 5, "Built 2026-10-27, history". Where the build fills a gap
this page leaves, or departs from it:

- **The key.** The schema above keys a message by `(id, identity)`. A
  message we write has no LXMF id until the router queues it, and a send
  that falls back to a propagation node is queued again under a second id, so the
  key is the screen's own message counter (`seq`, `Message::activity`),
  continued at start from the largest the store holds; `id` is a column,
  indexed. `identity` (our delivery address) scopes every row, as
  decided.
- **The columns kept empty.** `title`, `fields` and `timestamp` (the
  sender's clock) exist and stay NULL: the engine's `Received` event hands
  the screen neither the fields nor the wire timestamp, and the screen
  drops the title. Filling them is a change to the event, not to the
  schema. Until then nothing sorts on the sender's clock at all; the order
  is ours, `received_at` and the counter.
- **Three more tables.** `ledger`, a row a line of the delivery ledger,
  deleted with its message (the retention rule above); `conversations`,
  for the pin and the unread count; `names`, the names peers announced,
  so a peer is named after a restart. `message_text` is the FTS5 index,
  external content over `messages` kept by triggers, with the `trigram`
  tokenizer so a query matches a substring, as the screen's own matcher
  does. A query shorter than three characters scans with `LIKE`.
- **Drafts** (schema 8, 1219). `drafts`, a row a conversation: the text
  typed and not sent, the LXMF id a reply being written answers, and the
  attachments to go with it as a JSON array, empty for now. Written once
  typing paused for half a second, or at once when the conversation is
  left, the screen quits or the phone's session ends; deleted when the
  message is sent and with its conversation. A conversation that holds
  nothing but a draft comes back at start.
- **Read receipts** (schema 9, 1230). A message of ours the peer read is
  `state` 8 with the reader's time, unix seconds, in `messages.read_at`;
  `conversations.receipts` is 1 while the peer's latest message carried the
  `lev` receipt mark ([read receipts](loquax-read-receipts.md)). A schema 8
  file gains both columns empty in place.
- **Position shares** (schema 10, 1273). `shares`, a row a conversation
  while our share runs there: its span (`15_min`, `1_h`, `8_h`,
  `until_stopped`), its start and end in unix seconds (no end until
  stopped), and the last report it sent (the fix's integers, when it went,
  whether it went for a move), from which its cadence goes on after a
  restart. Written on every start, report and stop, deleted at the stop
  and with its conversation. A share whose end passed while the app was
  gone ends at the first clock with its end report. A row with a span
  this program does not know, or half a last report, is refused as
  corrupt. A schema 9 file gains the empty table in place.
- **Writes.** The model notes what a fold changed, and the shell writes it
  in one `BEGIN IMMEDIATE` transaction before the next event, rollback
  journal, `synchronous=FULL`, foreign keys on. A crash leaves the store as
  before the write or after it. The engine does no I/O under the core lock,
  so it does not write; this page's "the client's problem" is the screen's
  shell. A write that fails is said in a toast and after the screen, and
  logged as `LNMSG_STORE_FAILED`; the message stays on the screen.
- **The durability rule.** A missing file is a first start and is created
  with the schema. A file that is there is checked before anything reads
  it for the screen: SQLite reads its header and schema (and so refuses a
  file shorter than the page count its header gives, "database disk image
  is malformed", and one that is not SQLite at all), the schema version
  must be this program's (`PRAGMA user_version`, a later one is refused as
  newer, version 0 as not a Loquax store, which covers an empty file), and
  all its tables must be there. Any of these stops `loquax` with the
  file's path and what to do (move it aside, or restore a backup); nothing
  is written over it. The tests cut a real store by its last page and by
  half after opening it whole as the positive control, and refuse an empty
  file, a text file and another program's SQLite file. A page damaged in
  the middle of an otherwise whole file is found when it is read, and is
  then an error, never an empty answer.
- **Paging.** The screen loads a conversation's newest 200 messages and
  asks for 200 more when the reader reaches the top: both index lookups on
  `(identity, conversation, seq)`, as "On the 2 GB Raspberry Pi question"
  argues.
- **One row per received message** (leviculum 1070). A message that came
  directly and comes again from the propagation node, the sender's fallback after
  a delivery it could not prove, shares its LXMF id with the first copy.
  The screen's own check sees only what it loaded, so after a restart the
  older copy is out of its sight; the shell asks the store
  (`Store::holds_received`, over the `id` index) before it folds a
  received message, and drops one the store holds. A write of a received
  message whose id another row already holds is a no-op as well. A
  message the reader deleted is no longer held and would come back.
- **Unverified messages keep their bytes** (leviculum 1079, schema 3). A
  message from a sender whose key this device has not seen cannot have its
  signature checked when it arrives. The engine asks for the sender's path,
  whose answer is the sender's announce with the key, and checks the
  signature once the key is there; the store keeps the message as it came
  in `unverified` until then, so a later run hands it back to the engine
  and the check survives a restart. `messages.verification` is 0
  unverified, 1 verified, 2 invalid (schemas 1 and 2 wrote it as a
  boolean, which reads the same). A message stored unverified by a Loquax
  before schema 3 has no bytes kept, so it stays unverified: the signature
  was never stored.
- **Retention** is as decided: text and its ledger are kept until the
  reader deletes them (`d`, or the palette's `delete conversation`).
  Attachments do not exist yet, so neither does their byte budget.
- **Only the terminal writes.** `loquax send` and `loquax fetch` on the
  command line do not touch the store.
