# Loquax: import and export

Status: slice 1 built (order 1103, Refs #232); the Ratspeak history
importer built (order 1150). The survey of the other
messengers' formats, read from their public source only, is the draft
`docs/drafts/loquax-import-2026-10.md` (order 1091); this page keeps what
is decided and what is built. The user's side is the manual,
`loquax(1)`, "IMPORT AND EXPORT".

## Why

A user switching messengers loses everything today: the address their
correspondents wrote down, the names they gave people, the conversations.
The first start is when a messenger wins or loses a user. Every LXMF app
stores the identity as the same 64 bytes (the X25519 private key, then the
Ed25519 one), so taking the address over is a copy, not a format problem.

## Decided

Lew, 2026-10-29:

- **Adopting a foreign identity** when Loquax already has its own: the old
  identity and its history are set aside, kept and recoverable, and Loquax
  switches. Several identities at once come later.
- **Ratspeak**: identity only, never history. Revised by Lew on
  2026-10-30 for his own move to a Loquax Mailbox: a one-time, lossy
  history importer for the Ratspeak versions in hand, schemas 45 and 33
  (33 is the one his database holds, order 1158), marked as imported and
  not maintained against every Ratspeak release (see "Ratspeak history"
  below).
- **`retain_synced_on_node`** is not offered at import. Loquax only warns
  not to run both apps on one identity at the same time.
- **Attachments** are reported as skipped until Loquax has an attachment
  store.
- **The export** (NomadNet's container shape) is part of slice 1 and is the
  round trip test of the import.

From the draft, kept:

- Transport identities are never imported; Loquax attaches to a daemon and
  owns none.
- The scan looks only at the documented default places, one level deep,
  and reads no more than the line it shows. A path the user names is read
  as whatever it is. Phone data directories are never read; a phone app's
  export file, carried over by hand, is.
- Three questions, in this order: contacts (default yes), history (default
  yes), identity (default no, a typed `yes` after the warning).
- Dedup on the LXMF message id, 1070's rule, so a re-run is idempotent and
  the same history from two apps merges.
- Never key material or message text in a log.

## Built in slice 1

| Source | What comes over |
|---|---|
| NomadNet tree | identity, named peers as petnames, `ignored` as blocks, the propagation node, the history |
| Loquax export | the same, through the same reader |
| raw 64-byte identity file (NomadNet, MeshChat, MeshChatX, Sideband, Ratspeak unsealed) | identity |
| pasted Base32 key, or a file with one | identity |
| Ratspeak `identity.enc` | listed as protected, not read |
| a copy of Ratspeak's `ratspeak.db`, schema 33 or 45 (orders 1150, 1158) | contacts as petnames, blocks, the propagation node, the history of one identity, unsigned |

The code is `loquax/src/import/` and `loquax/src/export.rs`, behind the
`history` feature; the core sees neither files nor formats.

### Where the history goes

Rows of the store are scoped by our own address
(`docs/src/concepts/loquax-storage.md`). Imported history is written under
the address it belongs to, the source's identity. Once that identity is
Loquax's, it is the live history; until then it waits in the store. Showing
a foreign address's history beside the current one belongs with several
identities, which come later.

### Order

The store orders a conversation by `seq`. Imported history is older than
what Loquax wrote, so a conversation's imported rows get keys below the
store's lowest (negative ones), in the source's time order. A second import
into the same conversation lays its whole imported block out again below
the lowest key, in one transaction, so older messages that arrive later
still go first and Loquax's own rows keep their keys. The block is deleted
and inserted, not renumbered, so the text index (keyed by `seq`) follows
through its triggers. The screen's model counts keys unsigned; the store
hands it `seq` with the sign bit flipped, which keeps the order and leaves
every older store's rows where they were.

Schema 6 adds two columns to `messages`: `origin` (the source's tag,
`nomadnet` or `loquax`, NULL for Loquax's own rows) and `origin_verdict`
(what the source said of the signature).

### Verification

Loquax's own verdict comes only from checking the signature itself:

- A message sent by the history's identity is checked against that
  identity's key, which the source tree holds.
- A received message whose bytes are intact, which is when they hash to
  the id the file is named by, goes into `unverified` with its bytes, and
  is settled when its sender's key arrives, as any received message is.
- NomadNet rewrites a message without its attachments and keeps its name,
  so its bytes no longer hash to its id. Such a message is unverified, and
  its ledger says `imported from NomadNet` and `NomadNet said: signature
  verified` (from NomadNet's `.index`): the source's word, never ours, and
  never "invalid" either, since the change is NomadNet's, not a forgery's.

### The identity

The 64 bytes go into Loquax's own envelope
(`leviculum-core/src/identity_store.rs`), written under a temporary name
with mode 0600 and moved into place. An identity already there with
another address is renamed to `identity.<address>.bak`, never deleted; its
history stays under its address, and moving the file back switches back.
On a client of a home node, adoption is refused: the client answers on the
home node's address.

### Ratchets

Order 1185, after Lew's mailbox lost every short message Columba sent it
with two ticks on the phone (2026-10-31). Every Python LXMF router turns
ratchets on for its delivery destination
(`reference/LXMF/LXMF/LXMRouter.py:357`), and so does Ratspeak. A peer that
heard such an announce encrypts every single packet to that ratchet for 30
days (`RNS/Identity.py:69`, `RATCHET_EXPIRY`), and only an announce that
carries a newer ratchet replaces it. An identity that moves to Loquax
without its ratchets therefore loses every short message from such a peer,
which is most messages: a short text goes as one opportunistic packet, and
only links (telemetry, long messages) use their own keys.

So the old app's private ratchet keys come along. `loquax import --ratchets
FILE` takes either format, told apart by content
(`loquax/src/import/ratchets.rs` cites both layouts):

- Python LXMF (NomadNet, Sideband): `<lxmf storage>/ratchets/<destination
  hash>.ratchets`, `{"signature", "ratchets"}`, signed over the key list
  (`RNS/Destination.py:210-220`).
- Ratspeak: `identities/<identity hash>/ratchets/ring`, rsReticulum's
  `RatchetRing` through `rmp_serde`, signed over the current ratchet's
  public key.

The signature has to hold for the identity Loquax answers on, so the
ratchets go in after the identity is adopted (the same command does both,
`loquax import --key --ratchets FILE` or a path with `--ratchets`). NomadNet's
file and Ratspeak's ring are taken along by themselves when the identity is
adopted from their place. They go in behind the ratchets Loquax holds, into
the file its node loads at start (`storage/ratchetkeys/<address>`, mode
0600), within the reference's 512. The import takes the delivery lock: with
Loquax Mailbox or the messenger running it is refused, as a running node
would write its own set over the file at its next rotation. On a Mailbox,
stop it, import, start it again.

Loquax itself announces with ratchets from 1185 on, so a peer that hears
one of its announces replaces the old ratchet anyway; the import covers
the time until then. The way out is the same: `loquax export` writes the
ratchets to `storage/lxmf/ratchets/<address>.ratchets` in the tree, where
NomadNet's router loads them, and an import of that tree takes them back.
After an import that adopted an identity or brought
ratchets, Loquax Mailbox announces every 30 minutes for a day instead of
every 6 hours (`loquax-home-node.md`, "Inbound on the home node").

A packet that no ratchet opens is logged as `LXMF_RX_UNDECRYPTABLE
dst=<address> len=<bytes> ratchets=<held> count=<since start>` and is not
proven, as the reference proves only what it could decrypt
(`RNS/Transport.py:2159-2160`): until 1185 the core proved it anyway, and
the sender showed it as delivered.

### Ratspeak history

Order 1150. `loquax import <copy of ratspeak.db>` reads a database the
user copied out; Ratspeak's data directory is not scanned for it, and the
file is opened read-only. The reader is written from Ratspeak's schema at
commit 7da1fe3 (`crates/ratspeak-db/src/db.rs`, schema 45) and at
39a6323, the last commit at schema 33 (order 1158), and refuses any other
version with the numbers it reads and the one found: "written for
Ratspeak schemas 33 and 45, this db is 46". Schema 33 keeps the same
columns with the same meanings, lacks voice notes and the deletion
record. The citations are in `loquax/src/import/ratspeak.rs`.

- **Which identity.** A Ratspeak database holds one history per profile.
  The import takes the profile whose address is Loquax's (adopt it first
  from Ratspeak's Base32 export with `loquax import --key`), or the only
  profile there is; with several and none matching it refuses and says
  how. The dry run lists every profile with its count and which one is
  imported.
- **What comes.** Messages in both directions, with their id, text, title
  and, for a sent one, where it stood (still sending in Ratspeak is failed
  in Loquax, which will never send it); contacts with Ratspeak's name as
  the petname (Loquax's own word wins), blocks, the propagation node when
  Loquax has none, the display name offered. A message the user deleted in
  Ratspeak (`lxmf_user_deleted`) stays deleted and is counted.
- **Replies and reactions.** A reply whose target is imported too carries
  `FIELD_REPLY_TO` and Ratspeak's preview as the quote; one whose target
  is not comes as text and is counted. Each row of Ratspeak's `reactions`
  table becomes a reaction message (`FIELD_REACTION`) in the target's
  conversation, at the time Ratspeak stored it, with an id derived from
  the row so a second run writes nothing; one whose target is not
  imported is counted and left. On a Loquax Mailbox the archive entry
  carries these fields, so the clients get the links too.
- **What is lost.** Ratspeak keeps no packed message, so every row is
  unverified, and its ledger says `imported from Ratspeak` and
  `imported, no signature: Ratspeak keeps none`. No fields: games and
  telemetry are gone. Attachments, images and voice
  notes are counted as skipped. A received message has only Ratspeak's
  receive time, no sender's clock. Channels (Reticulum Relay Chat) are not
  LXMF and are not read.
- **Time.** Ratspeak's `timestamp` is unix seconds (`SystemTime` since the
  epoch, or the LXMF timestamp of a sent message), which is UTC already.
  The database records no zone and the import applies none, on any
  machine, across any change of daylight saving time. The store keeps
  whole seconds, so within one second a received message may sort before
  a sent one it followed.
- **On a Loquax Mailbox** (a store with an admitted device or archive
  entries), the imported messages are also appended to the archive as
  entries of kind 9, all conversations in one time order, so every client
  takes them on its next `/sync` and writes them into its own store
  (`loquax-home-node.md`, "Imported history"). An import that would push
  the archive past its cap appends nothing and says so, since the cap
  evicts the oldest entries. A NomadNet tree or a Loquax export does the
  same (1162): its rows keep our verdict, NomadNet's and, for a received
  message whose sender's key is not known yet, the signed bytes, so the
  entry is of version 2, which carries all three
  (`loquax/src/import/archive.rs`), and a client stores the row the
  mailbox stores and checks the held bytes when the key comes. The
  imported blocks go as peer states, as a device's block does
  (`loquax-home-node.md`, "Imported history").

### The export

`loquax export <dir>` writes NomadNet's tree with LXMF's
`packed_container` per message, the raw identity (mode 0600), the
contacts as NomadNet's directory and `ignored`, and the propagation node.
A marker file `storage/loquax-export` tells the import the tree is
Loquax's.

The store keeps a message's text and fields, not the bytes it travelled
as, except while a received message waits on its sender's key. So the
export writes those bytes where it has them, signs Loquax's own sent
messages again (their id then differs from the travelled one, and the file
keeps the old id as its name), and gives any other received message an
empty signature plus Loquax's verdict in an extra container key,
`loquax_verification`, which NomadNet ignores.

The round trip, export then import into an empty Loquax with the identity
adopted, brings back the same messages under the same ids: sent messages
verified, held messages held with their bytes, and messages Loquax had
verified on arrival unverified with `Loquax said: signature verified` in
their detail view. That last line is the one place the round trip is not
exact, and the cure is the next item below.

## Next

- Keep the signed bytes of every received message in the store, so the
  export carries them and the round trip is exact.
- Sideband, directory and profile tarball (the phone path; a tar reader).
- MeshChat and MeshChatX, the shared tables and the MeshChatX JSON export.
- Columba's `.columba` (AES-GCM, PBKDF2), with the id check the draft
  names.
- Ratspeak `identity.enc` and `.rsi` (Argon2id). Not in 1150: no Argon2
  crate is in the lock, and Ratspeak exports the key as Base32 once
  unlocked, which the existing paste path takes.
- The palette's `import` as a screen with the three questions and a
  pasted key, instead of contacts and history only.
