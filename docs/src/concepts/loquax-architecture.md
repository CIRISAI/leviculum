# Loquax: architecture

Part of the [Loquax design record](loquax.md). This page carries the ground
truth the design stands on — `lnomad` and `leviculum-lxmf` — the driver
seam that nearly forces the architecture, the process and event-loop
decisions, scriptability, and the triage of what the library does not yet
expose.

## 1. `lnomad`, and one correction to the premise

`lnomad` is described as supporting "Emacs keybindings, vi keybindings,
Firefox keybindings and Firefox mouse behaviour, all at the same time".
That is the observable behaviour, but it is not implemented as four
schemes. It is one keymap with three resolution mechanisms, and only one of
them is table-driven.

**What is table-driven.** The scroll rows. When this page was written they
were `SCROLL_KEYS` in `lnomad/src/app/event.rs`; since leviculum 1030 they are
`SCROLL_ROWS` (`leviculum-ui/src/keymap.rs:666`), shared with `loquax`. A row
is a `Row { desc, group, spellings }` whose chords each carry a modifier
class (`Mods::{Any, Plain, Ctrl, Alt, Exact}`,
`leviculum-ui/src/keymap.rs:92`). One row carries the vi and the emacs and
the arrow spelling of the same motion side by side, and the keys the help
shows are derived from those chords:

```rust
"j / k   ↓ / ↑   Ctrl-n / Ctrl-p"   // SCROLL_ROWS[0].keys()
"scroll a line"
```

Resolution takes the most specific matching chord, the first among equals,
which for these rows is the first-match scan `lnomad` always had
(`best`, `leviculum-ui/src/keymap.rs:569`; `key_to_scroll`,
`lnomad/src/app/event.rs:1924`). The table is read by both the key handler
and the help overlay (`lnomad/src/app/chrome.rs:406`).

**What is not.** Everything else is a hand-written `if`-chain in
`update_browse_key` (`lnomad/src/app/event.rs:640-787`): roughly twenty
sequential `if key.code == ... { return ...; }` statements. There is no
binding map, no user-configurable keymap, no keybinding config file. The
help overlay's non-scroll groups are a second, unlinked static list
(`lnomad/src/app/chrome.rs:426-527`) that can silently drift from the handler.

**How the conflicts are actually resolved.** Three layers, in this order.

1. *Global escapes*, before any mode dispatch
   (`update_key`, `lnomad/src/app/event.rs:390-430`): any key dismisses the
   toast; `Ctrl-C` quits from anywhere; an open help overlay swallows
   everything; an open places panel takes over.
2. *Mode gating*. `Mode::{Browse, Address, Hint, Search, Field}`
   (`lnomad/src/app/model.rs:97-116`), each with its own handler. Text modes
   forward unclaimed keys to a `tui_input::Input` editor. `Mode::Field`
   uses a whitelist rather than a catch-all so that "field editing never
   leaks into browse hotkeys" (`lnomad/src/app/event.rs:438`, whitelist at
   `:1643-1652`).
3. *Modifier discrimination*. Nearly every single-letter binding is guarded
   `&& !ctrl && !alt`, which is what lets `f` (hint mode) and `Ctrl-f`
   (page down), `d` (places) and `Ctrl-d` (half page), `n` (next match) and
   `Ctrl-n` (line down), `g` (top) and `Ctrl-g` (cancel) all coexist.

The ordering is load-bearing. In browse mode `key_to_scroll` is consulted
*last* (`lnomad/src/app/event.rs:781-784`), so single-letter commands claim
their keys first and `j`/`k`/`Space` reach the scroll table only because
nothing above claims them. In the places panel the order is *inverted*
(`lnomad/src/app/event.rs:975`) with a comment explaining why: there `Ctrl-d`
must be a half-page motion, not the `d` that closes the panel.

There is one further principle worth carrying over verbatim. Bare `r` is
deliberately left unbound, because "a mesh reload is expensive and single
letters are reserved for cheap local actions"
(`lnomad/src/app/event.rs:760-763`); reload requires `R`, `Ctrl-R` or `F5`.
That is a cost-aware keymap, and a messenger sends over the same radios.

**Architecture.** Elm-style with an explicit effect list. The core sits in
`lnomad/src/app/` since the #219 cut (2026-10-25): `Model` at
`lnomad/src/app/model.rs:459-618` (`#[derive(Clone, Debug, Default)]` at
`:565`), `Effect` at `:478-532`; `AppEvent` at
`lnomad/src/app/event.rs:25-92` and `update(&mut Model, AppEvent) ->
Vec<Effect>` at `:98-208`. The terminal frontend is `lnomad/src/tui.rs`:
`view(&Model, &ImageStore, &mut Frame)` at `:154-193`, and a single effect
interpreter `run_effects` at `:1855-1960`. `update` mutates rather than
returning a new model, and effects are plain data, not closures. That
combination is what makes the 245 in-file unit tests possible: build a
`Model`, feed a synthetic `AppEvent::Key`, assert on the model and on the
returned `Vec<Effect>`, with no IO anywhere (`lnomad/src/tui.rs:1711`
onwards; helpers at `:2411-2427`). The view is tested against
`ratatui::backend::TestBackend` (`:3240`), and the `--print` path has
byte-identical golden files (`lnomad/tests/render_golden.rs:18-37`).

**Unsolicited inbound events already exist.** This matters more than
anything else for a messenger, and the answer is yes.
`AppEvent::NodeDiscovered` (`lnomad/src/app/event.rs:92`) arrives from
announces with no user action. The chain is: an announce sink installed
before the session is shared so nothing is missed at startup
(`set_announce_sink`, `lnomad/src/fetch.rs:225-227`, wiring comment at
`lnomad/src/tui.rs:1487-1490`), a non-blocking unbounded send on every
recorded announce (`note_announce`, `lnomad/src/fetch.rs:369-389`), a
dedicated background task parked on the shared session in 250 ms lock
slices (`spawn_discovery`, `lnomad/src/tui.rs:1265-1306`), and a
`tokio::select!` arm that folds the result into the model
(`lnomad/src/tui.rs:1646-1648`). The main loop has five arms
(`lnomad/src/tui.rs:1550-1653`), and the timer arm is conditionally enabled
(`, if animate` at `:2229`, driven by `needs_tick()` at
`lnomad/src/app/model.rs:804-807`) so an idle browser does not wake eight
times a second.

**Persistence.** Three small files under
`${XDG_CONFIG_HOME:-~/.config}/lnomad/`: `bookmarks.toml`, `identify.toml`,
and a binary `identity`. Everything else is RAM. The write path is
`fs::write` with no atomic rename, no fsync, and errors deliberately
ignored (`lnomad/src/bookmarks.rs:124-130`, effect handler at
`lnomad/src/tui.rs:1423-1429`), and load treats corrupt exactly like
missing (`lnomad/src/bookmarks.rs:116-121`). For bookmarks that is a
defensible trade. For a message store it is data loss.

`load_or_create` (`lnomad/src/identity.rs:39-53`) silently mints a fresh
identity when the stored one fails to decode. For a browser, whose identity
is disposable, that is right. For a messenger, whose identity *is* the
user's address, silently replacing it breaks every contact's address book
with no warning. That default must be inverted.

**Two caches worth copying.** The page cache stores the parsed document
rather than the laid-out page, because layout depends on width and theme
(`lnomad/src/page_cache.rs:10-13`). The image cache is bounded by *bytes*
rather than count, and the reasoning generalises directly to attachments:
"a cache of 'the last fifty pictures' says nothing about how much memory a
browser is holding, and pictures differ in size by three orders of
magnitude" (`lnomad/src/image_cache.rs:11-14`).

**Rendering.** One layout core, two sinks. `layout_blocks`
(`lnomad/src/render.rs:192-217`) produces `Vec<RLine>` where `RLine`, the
shared styled text model of `leviculum-ui` since 984, is a vector of
`StyledChar { ch, st, link, field }`
(`leviculum-ui/src/style.rs:95-116`): already wrapped, aligned and indented,
one `RLine` per output row, every cell carrying its resolved style and its
owning link index. That IR feeds either `to_ratatui_text`
(`leviculum-ui-ratatui/src/style.rs:148-167`) for the TUI or `emit_ansi`
(`lnomad/src/render.rs:225-231`) for `--print`. Scrolling is a slice, not a
widget scroll, because the page is pre-wrapped
(`lnomad/src/tui.rs:317-324`), and there is one scroll rule shared by
every scrollable window (`scrolled`, `leviculum-ui/src/keymap.rs:639-653`),
which Loquax's help and detail view scroll by too.

Two rendering caveats. Wrapping compares `cur.len() > width`, i.e.
character count rather than display width (`wrap`,
`lnomad/src/render.rs:773-800`), which will overflow on CJK and emoji. No
test covering that was found, so whether it is a known limitation or an
oversight is unclear. And the whole page is laid out eagerly on every
relayout (`lnomad/src/app/model.rs:690-714`), including on every keystroke in a
form field (`:1657-1660`).

**Scriptability, and the absence of settings.** `--print` fetches, renders
and prints once (`print_once`, `lnomad/src/browser.rs:133-141`). Output is
raw ANSI page text and nothing else: no link markers, no legend, and with
`--no-color` links are indistinguishable from body text
(`lnomad/src/render.rs:144-147`). There is no JSON output anywhere in the
crate: `serde_json` is not a dependency. Non-interactive detection is
automatic: `interactive = !args.print && stdin().is_terminal() &&
stdout().is_terminal()` (`lnomad/src/main.rs:174-175`), so piping never
blocks on the UI. Exit codes: 0 success, 1 operational failure, 2 argument
or URL error (`lnomad/src/main.rs:181`, `:188`, `:222`, `:238`).

`lnomad` has no settings file at all. `--config` points at the *Reticulum*
config directory; the `lnomad/` directory holds only data. The theme is
auto-detected via OSC 11 before raw mode is entered
(`lnomad/src/tui.rs:1448-1455`) and toggled at runtime with `t`; theme
colours are hard-coded (`leviculum-ui/src/theme.rs:116-189`).

**The handoff that already exists.** `lnomad` recognises `lxmf@<hash>`
links, and because it has no composer it copies the address to the
clipboard and says so in a toast (`follow_link`,
`lnomad/src/app/event.rs:1568-1577`). The messenger is the natural target of that
handoff, and wiring the two together is an explicit goal.

## 2. `leviculum-lxmf`: what it gives and what it does not

Three layers, all sans-IO: `NodeCore` (Reticulum transport, owned by the
app), `LxmfNode` (`leviculum-lxmf/src/node.rs:375`, the `lxmf.delivery`
destination adapter), and `LxmfRouter`
(`leviculum-lxmf/src/router.rs:464`, the queue, retry scheduler, stamp and
ticket policy, dedup caches and propagation client). The application builds
on `LxmfRouter` and owns both it and the core; the router never owns the
core, every method takes it as a parameter.

Note that `LxmfRouter`, `RouterEvent`, `RouterOutput`, `RouterConfig` and
`MessageState` are *not* re-exported at the crate root — the crate root
exports only `BuiltResource`, `DeliveryStampRequest`, `InboundStampRequest`,
`PendingResourceBuild` and `PropagationStampRequest` from that module
(`leviculum-lxmf/src/lib.rs:132-135`) — so they are reachable as
`leviculum_lxmf::router::*` only.

### Events are return values, not a channel

```rust
#[must_use]
pub struct RouterOutput {          // leviculum-lxmf/src/router.rs:300-303
    pub core: TickOutput,
    pub events: Vec<RouterEvent>,
}
```

Every router method that can produce work returns this. There is no
callback and no channel. The library never drops an event, but it never
retains one either: if the application drops a `RouterOutput`, those events
are gone. `#[must_use]` on both `RouterOutput` and `TickOutput`
(`leviculum-core/src/transport.rs:588`) is the only safety net, and
`TickOutput`'s own doc says dropping it "silently loses outbound packets
and application events" (`leviculum-core/src/transport.rs:557-559`).

There is a re-entrancy obligation that is easy to miss and fatal to get
wrong: `RouterOutput.core.events` contains `NodeEvent`s that must be fed
*back* into `router.handle_event()`, recursively, until the worklist
drains. This is exactly Codeberg #204's subject. `leviculum-lxmf-node`
implements it with a bounded worklist and says why the bound is the
consumer's choice (`MAX_ABSORB_ROUNDS`,
`leviculum-lxmf-node/src/processor.rs:83-93`, `absorb` at `:534-540`). A
client that forgets this will silently never see incoming messages.

### `RouterEvent`

Fifteen variants (`leviculum-lxmf/src/router.rs:297-345`):
`MessageQueued`, `MessageState { message_id, state }`, `MessageReceived`,
`InboundRejected`, `DirectLinkEstablished`, `Duplicate`,
`InvalidSignature`, `InvalidStamp`, `ResourceBuildPending`, `StampPending`,
`InboundStampPending`, `PropagationStampPending`, `PropagationSyncState`,
`PropagationSyncComplete`, `PersistenceRequested`.

What is missing is as informative as what is there. There is no announce
event: `LxmfNodeEvent::PeerAnnounced` carries the destination hash only,
with app data discarded (`leviculum-lxmf/src/node.rs:133-135`,
`:746-754`), and `handle_node_event` does not forward it at all — it falls
into `_ => {}` (`leviculum-lxmf/src/router.rs:1380`). The router does
decode the delivery announce but keeps only `stamp_cost` and
`compression_supported`, discarding the display name
(`leviculum-lxmf/src/router.rs:1218-1228`). **Display-name learning is
entirely the client's job**, from raw `NodeEvent::AnnounceReceived`.

`Sending` does arrive, on the event every verdict travels on:
`RouterEvent::MessageState`, from all three sites that enter the state —
the composed send (`leviculum-lxmf/src/router.rs:1856-1861`), the
built-transfer commit (`leviculum-lxmf/src/router.rs:1042-1047`) and the
upload the transport reports through `UploadSubmitted`
(`leviculum-lxmf/src/router/propagation_runtime.rs:406-418`) — and on the
transition only: a submission onto an entry already in that state reports
nothing. For a direct delivery it is the only thing between being accepted
and being answered. For a propagated one it names the message on a link
`PropagationSyncState` was already narrating, which narrates the link and
cannot say what is on it. An opportunistic message reports it and then
goes quiet until the verdict: the router moves it on to `Sent` in the same
tick, and *that* transition is reported nowhere.

There is still no event for `Outbound` and none for progress: the router
folds `LxmfNodeEvent::Progress` into `OutboundEntry::progress` without
emitting anything (`leviculum-lxmf/src/router.rs:1432-1443`), so progress
must be polled through `outbound()`
(`leviculum-lxmf/src/router.rs:704`).

### `MessageState` and what it honestly means

```rust
pub enum MessageState {          // leviculum-lxmf/src/router.rs:62-71
    Generating = 0x00, Outbound = 0x01, Sending = 0x02, Sent = 0x04,
    Delivered = 0x08, Rejected = 0xfd, Cancelled = 0xfe, Failed = 0xff,
}
```

Discriminants are the Python `LXMessage` constants. Four traps:

1. **`Generating` is dead.** It is only ever produced by snapshot decoding
   (`leviculum-lxmf/src/router.rs:2356`); nothing assigns it.
2. **`Sent` means two different things and never applies to direct
   delivery.** For opportunistic messages it means the packet was handed to
   Reticulum unproven, and the message is still queued and still retryable
   (`leviculum-lxmf/src/router.rs:1338-1352`). For propagated messages it
   means the propagation node accepted the upload, and the entry is
   *deleted* (`leviculum-lxmf/src/router/propagation_runtime.rs:438-445`).
   Direct delivery goes `Outbound -> Sending -> Delivered | Rejected |
   Failed` and never passes through `Sent`, because the `Submitted` handler
   matches only `DeliveryMethod::Opportunistic`
   (`leviculum-lxmf/src/router.rs:1417-1421`).
3. **`Delivered` is a Reticulum transport proof, not an application
   receipt.** It comes from `PacketDeliveryConfirmed` /
   `LinkDeliveryConfirmed` (`leviculum-lxmf/src/node.rs:1297-1319`) or from
   `ResourceCompleted { is_sender: true }`
   (`leviculum-lxmf/src/node.rs:1098-1109`). It proves the bytes arrived at
   the destination identity. It does not prove an LXMF client parsed them
   and it certainly does not prove a human read them. There is no
   read-receipt field in LXMF at all
   (`leviculum-lxmf/src/constants.rs:60-86`).
4. **`Rejected` is ambiguous.** It means either "the receiver cancelled the
   Resource transfer" (`leviculum-lxmf/src/router.rs:1296-1309`) or "the
   propagation node refused the upload for an insufficient stamp"
   (`leviculum-lxmf/src/router/propagation_runtime.rs:470-482`), and the
   event alone cannot distinguish them.

And one omission that shapes the whole UI: **there is no "propagated but
not yet collected" state.** A propagated message reaches `Sent`, its queue
entry is removed, and from then on it is indistinguishable from a message
that vanished.

Terminal states remove the entry from the outbound map
(`remove_outbound`, `leviculum-lxmf/src/router.rs:882-885`; call sites at
`:888`, `:1334`, `:1369` and five in the propagation runtime, among them
`leviculum-lxmf/src/router/propagation_runtime.rs:987`). If the client does
not capture the `Message` at `enqueue` time it cannot render its own sent
message afterwards, and it cannot offer a retry button.
`MAX_DELIVERY_ATTEMPTS` is 5 (`leviculum-lxmf/src/router.rs:50`).

### Propagation: what the router does, and what it refuses to do

Setup requires the client to mint a second `lxmf.propagation` destination
via `PropagationTransport::destination`
(`leviculum-lxmf/src/propagation_client.rs:282-292`),
register it, and hand it to `enable_propagation_client`
(`leviculum-lxmf/src/router.rs:606`); the transport identity must equal the
router's or you get `RouterError::IdentityMismatch`
(`leviculum-lxmf/src/router.rs:611-613`).

Node discovery is automatic from announces (`remember_announce`,
`leviculum-lxmf/src/propagation_client.rs:384-400`, driven from the
announce arm at `:733-742`), and the decoded announce carries `enabled`,
`transfer_limit_kb`, `sync_limit_kb`, `stamp_cost`, `peering_cost` and
`metadata` (`PropagationNodeAnnounce`,
`leviculum-lxmf/src/propagation.rs:520-532`), all of which are directly
displayable. `select_outbound_propagation_node` with `None` auto-ranks by
route, hops, peering cost and stamp cost
(`leviculum-lxmf/src/router/propagation_runtime.rs:1161-1196`).

Once a sync starts, everything is automatic: path request, link, identify,
list request, want/have partitioning, download, acknowledge and purge
(`begin_list_request`,
`leviculum-lxmf/src/router/propagation_runtime.rs:459-551`). The observable
state machine is `PropagationClientState`
(`leviculum-lxmf/src/router/propagation_runtime.rs:87-102`),
wire-compatible with Python's `PR_*` constants: `Idle`, `PathRequested`,
`LinkEstablishing`, `LinkEstablished`, `RequestSent`, `Receiving`,
`ResponseReceived`, `Complete`, `NoPath`, `LinkFailed`, `TransferFailed`,
`NoIdentity`, `NoAccess`, `Failed`. `ResponseReceived` is never assigned in
practice. There is automatic failover to another reachable node when the
selected one loses its route, but not while a sync round is still running on
it: that round ends first, as `LinkFailed` or `NoPath`
(`leviculum-lxmf/src/router/propagation_runtime.rs:911-964`).

What the router will not do:

- **It never schedules a sync.** `request_messages_from_propagation_node`
  (`leviculum-lxmf/src/router/propagation_runtime.rs:1498`) must be called
  by the application every time. `PropagationClientConfig` has three fields
  and none of them is an interval
  (`leviculum-lxmf/src/router/propagation_runtime.rs:56-68`), and
  `next_deadline()` returns `None` in every state except `PathRequested`
  (`leviculum-lxmf/src/router/propagation_runtime.rs:1238-1249`).
- **It does not persist known propagation nodes.** They live in an
  in-memory map (`known_nodes`,
  `leviculum-lxmf/src/propagation_client.rs:267`) and are absent from the
  router snapshot (`snapshot`, `leviculum-lxmf/src/router.rs:2126-2143`).
  The client must persist and replay them via
  `restore_known_propagation_node`
  (`leviculum-lxmf/src/router/propagation_runtime.rs:1450`). The selected
  node is not snapshotted either.
- **It does not clamp the transfer limit against the node's advertised
  one.** The download request carries the local
  `delivery_transfer_limit_kb` (default 1000) regardless of what the node
  announced (`leviculum-lxmf/src/router/propagation_runtime.rs:585-590`).

Default `retain_synced_on_node` is `false`
(`leviculum-lxmf/src/router/propagation_runtime.rs:77`), meaning the client
tells the node to purge what it has collected. That is a user-visible
policy decision disguised as a config default, and
[the mailbox page](loquax-mailbox.md) argues it should be surfaced. Set to
`true`, our client also withholds the acknowledgement after a download
(`leviculum-lxmf/src/router/propagation_runtime.rs:664`), which the
reference client sends regardless of the flag
(`reference/LXMF/LXMF/LXMRouter.py:1623-1638`); only then does a second
client on the same identity still find the message.

The reference holds messages for `MESSAGE_EXPIRY = 30*24*60*60`, i.e.
thirty days (`reference/LXMF/LXMF/LXMRouter.py:38`).

### Storage is a bare key/value trait

```rust
pub trait LxmfStorage {          // leviculum-lxmf/src/storage.rs:18-26
    fn load(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;
    fn store(&mut self, key: &[u8], value: &[u8]) -> Result<(), StorageError>;
    fn remove(&mut self, key: &[u8]) -> Result<(), StorageError>;
    fn keys(&self, prefix: &[u8]) -> Result<Vec<Vec<u8>>, StorageError>;
    fn flush(&mut self) -> Result<(), StorageError> { Ok(()) }
}
```

There is no conversation, thread, contact or history concept in it. Two
implementations exist, both in that file: `MemoryLxmfStorage`
(`leviculum-lxmf/src/storage.rs:42`) and `NoLxmfStorage`
(`leviculum-lxmf/src/storage.rs:116`). The file-backed one is `FileLxmfStorage`
(`leviculum-std/src/file_lxmf_store.rs:27`), in the std crate because the LXMF
crate is `no_std`.

The router writes exactly one key, `b"lxmf/router-state"`
(`ROUTER_STATE_KEY`, `leviculum-lxmf/src/router.rs:67`), holding the
outbound queue, delivered and processed ID windows, stamp costs, tickets
and the ignore set (`leviculum-lxmf/src/router.rs:2126-2143`). A client
should stay off the `lxmf/` prefix and is otherwise free.

Restore resets every queued message to `Outbound` with
`next_attempt_ms = 0` and `progress = 0.01`
(`leviculum-lxmf/src/router.rs:2111-2114`), because in-flight correlation
is expressed in a process-local monotonic clock that does not survive a
restart. A UI therefore cannot show a stable "sending" progress across
restarts, and must not pretend to.

### Features a UI could surface

- **Attachments** (`leviculum-lxmf/src/attachments.rs`): files, one image,
  one audio clip, as `MessageAttachments::into_fields()`
  (`leviculum-lxmf/src/attachments.rs:57`) / `from_fields()`
  (`leviculum-lxmf/src/attachments.rs:86`). Attachments are inline bytes in
  the message, so anything with a real attachment exceeds the packet MDU
  and forces link or Resource delivery (`representation`,
  `leviculum-lxmf/src/node.rs:581-609`).
- **Paper messages** (`leviculum-lxmf/src/paper.rs`): a message encrypted
  to a destination and rendered as an `lxm://` base64 URI (`to_uri`,
  `leviculum-lxmf/src/paper.rs:172`), capped at `PAPER_MDU = 2210` bytes
  (`leviculum-lxmf/src/constants.rs:46`). Ingest via
  `router.ingest_paper(uri)`
  (`ingest_paper`, `leviculum-lxmf/src/router/paper_runtime.rs:17`). No QR
  generation exists; that is the client's job.
- **Tickets** (`leviculum-lxmf/src/ticket.rs`): a 16-byte secret you issue
  to a contact so their future messages skip proof-of-work. Mostly
  invisible and automatic: received tickets are remembered from any
  signature-valid inbound message — `remember_verified_ticket`
  (`leviculum-lxmf/src/router.rs:1518`) — and applied when a message is
  enqueued (`leviculum-lxmf/src/router.rs:823`). Expiry 21 days, renew at 14,
  minimum one day between issuances to the same peer
  (`leviculum-lxmf/src/constants.rs:48-51`). `issue_ticket_field` refuses
  with `RouterError::NoWallClock` when the node's clock is implausible
  (`leviculum-lxmf/src/router.rs:684-685`), and can also legitimately
  return `Ok((None, _))` when rate-limited
  (`leviculum-lxmf/src/router.rs:702`). A UI has to distinguish "granted",
  "not yet, try tomorrow" and "cannot, no clock".
- **Stamps** (`leviculum-lxmf/src/stamp.rs`): proof-of-work over the
  message ID, cost being required leading zero bits, so expected work is
  2^cost hashes plus a workblock expansion of 3000 rounds
  (`WORKBLOCK_EXPAND_ROUNDS`, `leviculum-lxmf/src/constants.rs:53`). Costs
  above about 40 bits are described in-tree as "already unreachable in
  practice" (`leviculum-lxmf/src/router.rs:1154-1155`). No wall-clock
  benchmark exists in the crate and none was run for this document, so any
  UI estimate of mining time must be measured first, not guessed. There is
  no cancellation and no deadline: `generate` loops until it succeeds
  (`leviculum-lxmf/src/stamp.rs:356-367`), and `StampError::Cancelled`
  exists but is never constructed (`leviculum-lxmf/src/stamp.rs:25`).

### Fields with constants but no codec

`leviculum-lxmf/src/constants.rs:60-86` declares the full LXMF field set
including `FIELD_THREAD (0x08)`, `FIELD_RENDERER (0x0F)`,
`FIELD_REPLY_TO (0x30)`, `FIELD_REPLY_QUOTE (0x31)`,
`FIELD_REACTION (0x40)` and `FIELD_COMMENT (0x41)`, but only files, image,
audio, telemetry and, since 1104, reactions (`leviculum-lxmf/src/reaction.rs`,
pinned against the reference by `VEC-MSG-REACTION`) have typed codecs.
Unknown fields round-trip byte-for-byte
(`leviculum-lxmf/src/message.rs:5-8`), so nothing is lost, but a client
wanting replies, threads or renderer-aware display must
hand-roll the msgpack via the exported `msgpack` module.

`RENDERER_MICRON = 0x01` (`reference/LXMF/LXMF/LXMF.py:100`) is interesting
here: `leviculum-micron` already parses micron into a document model
(`leviculum-micron/src/lib.rs:25-27`) and `lnomad` already renders that
model. A messenger in this workspace can honour `FIELD_RENDERER` almost for
free, which no other terminal LXMF client does.

## 3. The driver seam, which nearly forces the architecture

An LXMF client cannot be fed from `leviculum-std`'s public event stream.
The reason is documented at the seam itself: the tap sits on
`output.events` inside `dispatch_output`, *before* the event sink
classifies, and seven of the event types LXMF needs, including
`PacketReceived` and `LinkDataReceived`, are `EventClass::Data` and
therefore droppable under load. A processor fed from `take_event_receiver`
"would silently lose inbound messages with nothing underneath to retransmit
them" (`leviculum-std/src/driver/processor.rs:193-201`, "Where the events
come from").

So the messenger must register a `CoreProcessor`
(`leviculum-std/src/driver/processor.rs:276-304`) on the builder, and the
LXMF router lives inside the driver's tick, under the core mutex. That
carries hard obligations:

- Both hooks run with a non-reentrant mutex held. The processor may not own
  a handle to the node it runs inside; roughly forty synchronous `pub fn`s
  on `ReticulumNode` open with a lock and one of them in a hook body
  deadlocks the node in ordinary safe code.
- Every side effect must be a non-blocking queue push.
  `leviculum-lxmf-node` does exactly this: stdout lines, stderr lines,
  proof-of-work jobs and shutdown are all channel sends
  (`leviculum-lxmf-node/src/processor.rs:15-30`).
- `PROCESSOR_TICK_BUDGET` is 5 ms per hook call
  (`leviculum-std/src/driver/processor.rs:183`), reported rather than
  enforced. Message packing costs about 0.8 ms and unpacking with signature
  verification about 3.2 ms for 1 MiB, per
  [The core lock budget](core-lock-budget.md). `NodeCore::send_resource`
  (`leviculum-core/src/node/mod.rs:1934`) must not be called from a hook:
  141 ms under the lock for 1 MiB.
- The processor needs its own periodic slot to drain its command queue,
  because an event tap can never initiate anything. `leviculum-lxmf-node`
  uses 200 ms (`POLL_INTERVAL_MS`,
  `leviculum-lxmf-node/src/processor.rs:81`).

This is a strong constraint and a gift at the same time: it means the
"model" that talks to the network is a synchronous state machine with a
queue on either side, which is exactly the shape that tests well.

## 4. Decision: process architecture

### Options

**A. One process. TUI plus an in-driver `CoreProcessor`.** The binary
builds a `ReticulumNode` as a shared-instance client with
`core_processor(...)` installed, exactly as `leviculum-lxmf-node` does
(`leviculum-lxmf-node/src/main.rs:389-398`). The processor owns the
`LxmfRouter`; the TUI owns the model. They talk over two unbounded
channels.

*For*: one binary, one config, no IPC to design, matches `lnomad`'s
deployment shape. *Against*: mail is only received while the TUI is
running. Closing the terminal stops collecting.

**B. Two processes. A headless daemon plus a thin TUI client.** A `loquaxd`
holds the router and the store and exposes a local socket; the TUI is a
view onto it. *For*: mail arrives while the UI is closed, several front
ends can attach, and the store has one writer. *Against*: an entire IPC
protocol, a second daemon on a system that already runs `lnsd`, and a
second thing to package and supervise.

**C. One binary, two modes.** `loquax` with a `--daemon` flag, and the TUI
attaching to a running daemon if there is one and otherwise running the
router itself. *For*: option A's simplicity on day one, option B's
availability when the user asks for it. *Against*: two code paths for every
operation, and the temptation to test only one.

### Decision (2026-08-08)

**C, built as A first.** Start with a single process, but put the router
and the store behind an interface from the beginning so that the daemon
mode is a wiring change rather than a rewrite. Whether the daemon mode is
ever built is decided empirically: if syncing with a propagation node on
start plus every N minutes proves sufficient in the mesh we care about, A
alone stays. The reference retention default of thirty days
(`reference/LXMF/LXMF/LXMRouter.py:38`) suggests it might. This aligns with
the standing decision that propagation nodes, not client uptime, are the
answer to offline delivery.

The `lnsd`-resident variant — daemon mode as a `CoreProcessor` registered
inside `lnsd` — is **rejected**, twice over: it would put LXMF knowledge
into the transport daemon, which the Codeberg #196 seam was explicitly
designed to avoid, and it contradicts the standing rule that client
programs do not merge into `lnsd` (there will be more clients than this
one).

### Requirement: the core must not know it has a terminal

Decided 2026-08-08, and binding for `lnomad` too: the messenger will grow
other frontends on other platforms later — a GUI is expected — and that
must be a frontend swap, not a rework.

Concretely, the crate splits into two layers with a hard boundary:

- **`loquax-core`** (or a module boundary with the same discipline until a
  crate split is warranted): the model, `update`, effects, the store, the
  router glue, sync scheduling, trust, delivery bookkeeping. This layer
  never imports crossterm, ratatui, or any terminal type. Everything in it
  is driven by `AppEvent` in and `Effect` out, and is testable headless.
- **The TUI frontend**: rendering, key mapping, terminal lifecycle. It
  translates terminal events into `AppEvent`s and draws the model. A GUI
  frontend later is a second translator and a second renderer over the same
  core — no change to the core's types. Since 2026-10-25 (984) its shared
  half is two crates, which `lnomad` already uses: `leviculum-ui`, the
  terminal-free model (styled text, palette, the programs' own key and
  mouse events with the text-editing table, the footer, help and toast
  models, the spinner frames, the delivery glyph table), which a core may
  take; and `leviculum-ui-ratatui`, the widgets and the shell over it
  (chrome styles, toast, help overlay, status chip, footer strip, spinner
  span, terminal lifecycle with its restore guard, crossterm event
  mapping, background detection, inline pictures behind `images`).

The TEA split below is what makes this cheap: the discipline is not a new
architecture, it is refusing to let the existing one leak. The test for the
boundary is mechanical and should exist from day one: the core compiles
without the TUI dependency tree (feature gate or crate split), and the
headless test suite drives complete user stories through `AppEvent`s alone.

For `loquax` the core is the library without its `std` feature, `no_std`
plus `alloc`, and `just loquax-core-no-std` checks it for the firmware triple
on every `just guards` (Codeberg #232, since 2026-10-25, 993; the plan is
below under "Plan: the Loquax core without std").

For `lnomad` the same requirement holds as a future refactor: its TEA split
already keeps the model headless-testable, but model, update and view live
in one 11,873-line file with crossterm types reachable throughout. When
`lnomad` next gets substantial work, the same core/frontend boundary is
carved there. Tracked as its own issue, not as part of this program.

Done for `lnomad` on 2026-10-25 (#219), as a module boundary inside the
crate: the core is `lnomad/src/app/` (model, `update`, effects, the cell
geometry and the chrome layout, and the browser's own key and mouse types),
the terminal crates sit behind the `tui` feature, and `just
lnomad-core-no-tui` builds the crate without them and fails if any reaches
the core. What still carries the terminal's shape is the unit: the core
hit-tests clicks against a character-cell layout, so a pixel frontend will
add semantic events (follow this link, press this control) beside the
cell-addressed mouse ones. The 251 tests stay in `lnomad/src/tui.rs`, where
the view tests need ratatui; the headless suite that runs without the
feature is the next step.

The widgets that are not the browser's own then moved out of
`lnomad/src/tui.rs` into those two crates on 2026-10-25 (984, Codeberg
#475), behaviour unchanged: a scripted 212-frame session through `update`
and `view` renders byte-identical before and after. `lnomad`'s labels,
actions, help text and screen layout stayed; the crates take them as
arguments. `just ui-no-terminal` holds `leviculum-ui` to the same rule as
`lnomad`'s core. So `loquax`'s TUI starts on the shared widgets rather than
on a copy of them.

### Plan: the Loquax core without std (Codeberg #232)

Decided 2026-10-25, Lew: std-free now; step one measured. The messenger
core is cut `no_std` plus `alloc`, so the M9, a GUI and the terminal run the
same core. The route is a feature, not a crate split: `loquax`'s `std`
feature (on by default) is the shell, and the library without it is the
core, under `#![cfg_attr(not(feature = "std"), no_std)]`. A split into an
`loquax-core` crate is not forced yet: the shell-only modules are few
(`identity`, and the binary, which requires the feature), and the mixed
ones split item by item. If a pass finds an item the feature cannot keep
honest, that is the moment for the crate.

**The module map** (lines including tests, measured 2026-10-25):

| module | lines | side |
|---|---:|---|
| `address` | 156 | core |
| `mailbox` | 273 | core: the selection rule |
| `outbox` | 256 | core: the `Outbox` seam, commands and events |
| `events` | 187 | core: the structured event emitters (tracing is `no_std`) |
| `display_name` | 343 | core `resolve`; shell `from_process` (environment, passwd) |
| `body` | 142 | core trimming and the empty rule; shell the stdin read |
| `config` | 154 | core `parse`; shell `load` (the file) |
| `seen` | 186 | core the set, the order and the compaction rule; shell the file |
| `mailbox_state` | 380 | core `Route`, `Stored`, `Decision`, `MailboxRun`, `routes_from_path_table`; shell `load`, `save`, `unix_now_ms` and the stderr lines |
| `send` | 996 | core: the send and fallback loops over `Outbox` and a `clock::Clock` |
| `fetch` | 598 | core: the propagation node drain over `Outbox` and a `clock::Clock`; messages printed through `Effects::print` |
| `engine` | 1828 | core `Engine` (the router glue: resolves, tracking, stamp bookkeeping, sync scheduling) and `mine`; shell `engine/shell.rs`: the `CoreProcessor` impl, `InProcessOutbox`'s channels, `attach`, the stamp worker thread and its runtime |
| `identity` | 182 | shell: home directory, key file |
| `main.rs` | 543 | shell: CLI, daemon RPC, process exit |

**The gate.** `just loquax-core-no-std` checks the library without the
feature for `thumbv7em-none-eabihf`. Every dependency of the core already
checks there (`leviculum-core`, `leviculum-lxmf` with `pow`, `tracing`,
`serde_json` with `alloc`); the red is Loquax's own code. Its first run,
after swapping the exact `std` re-exports (`fmt`, `error::Error`, `mem`,
`iter`, `cell`, `future`, `time::Duration`, `sync::Arc`, `sync::atomic`,
`collections::{BTreeMap, VecDeque}`) for their `core`/`alloc` paths, holds
164 name-resolution errors:

| cause | errors | where |
|---|---:|---|
| `alloc` prelude (`String`, `Vec`, `Box`, `format!`) | 104 | every core module |
| `std::io` | 12 | `seen`, `config`, `fetch`, `body`, `mailbox_state`, `engine` |
| `std::fs` | 9 | `seen`, `mailbox_state`, `config`, `engine` |
| `tokio` (`time::Instant`, `sleep`, the runtime) | 7 | `send`, `fetch`, `engine` |
| `eprintln!` | 7 | `mailbox_state`, `engine`, `fetch` |
| `std::sync::mpsc` | 5 | `engine` |
| `std::path` | 4 | `seen`, `config`, `mailbox_state`, `engine` |
| `leviculum_std` | 4 | `engine` (driver), `display_name` (passwd), `events` (`Scalar`) |
| `std::env` | 3 | `display_name` |
| `std::thread` | 3 | `engine` |
| `std::time` (`Instant`, `SystemTime`) | 3 | `engine`, `mailbox_state` |
| `rand_core::OsRng` | 2 | `engine` |
| `std::collections::HashSet` | 1 | `seen` |

These are the name-resolution layer only; type checking runs once they are
gone, and is expected to add the trait imports a `no_std` module needs for
`to_string`/`to_owned` (21 call sites outside tests) and nothing structural.

**The passes, in order.** Each leaves the workspace green and the gate's
list shorter; behaviour and the CLI tests do not change.

1. *Mechanical* (~40 lines): the `alloc` imports in each core module, the
   `ToString`/`ToOwned` imports, `HashSet` to `BTreeSet` in `seen`, and
   `Scalar` (pure formatting) moved from `leviculum-std::event_log` into a
   `no_std` home both crates use, with `leviculum-std` re-exporting it.
   Done 2026-10-25 (990).
2. *Files and the environment* (~150 lines): the shell items of `config`,
   `seen`, `mailbox_state`, `body` and `display_name` move into a
   `std`-gated shell module; the core keeps the pure functions and returns
   what to write (the seen store as a set that answers "append this line"
   or "rewrite with these", the propagation node record as its text). Error enums
   lose `io::Error` and `PathBuf`: the core error says what was wrong, the
   shell adds where. The `eprintln!` lines in `mailbox_state` and `fetch`
   become returned notices the shell prints. Done 2026-10-25 (990).
3. *The clock* (~80 lines): `send` and `fetch` take a `Clock` beside the
   `Outbox` (now in milliseconds, an async sleep). The host implementation
   wraps tokio's, so the paused-clock tests keep their meaning; `fetch`
   writes through `core::fmt::Write` or a returned message instead of
   `io::Write`. Done 2026-10-25 (992).
4. *The engine* (~250 lines moved, ~60 changed): `Engine` becomes generic
   over `NodeCore<R, C, S>` (the router, the LXMF node and the propagation
   client already are) and talks to the frontend and the stamp miner
   through queues of its own instead of `mpsc`. The `CoreProcessor` impl
   for `StdNodeCore`, `InProcessOutbox`, `attach`, the stamp worker thread,
   its tokio runtime, `OsRng` and the mining progress lines move to the
   shell. The miner on a board is a different executor over the same jobs.
   Done 2026-10-25 (993).
5. *Green*: the gate passes and joins `guards`, with a positive control (a
   `std::` path injected into a core module is refused). Done 2026-10-25
   (993).

**Status, 2026-10-25 (990).** Passes 1 and 2 are in; the gate lists 26
errors, all of passes 3 and 4. `Scalar` lives in `leviculum-core`'s
`event_scalar`, and `leviculum-std` re-exports it. The shell items stayed
in their modules behind `cfg(feature = "std")`, which kept the callers'
paths; the send and fetch runners reach the host through
`effects::Effects` (the notices, the propagation node record, the seen record, the
wall clock), and the terminal's implementation is `effects::Terminal`. Two
`std` uses outside the five modules remain on purpose: `fetch`'s
`io::Write` output is pass 3's, and `AttachConfig::storage_dir` is a
`PathBuf` because the driver builder takes one, so it moves with `attach`
in pass 4.

**Status, 2026-10-25 (992).** Pass 3 is in; the gate lists 19 errors, all
in `engine` and all of pass 4 (`mpsc` 5, `eprintln!` 3, `thread` 3,
`leviculum_std` 2, the stamp runtime's `tokio` 2, `OsRng` 2, `path` 1,
`time` 1). `scripts/loquax-no-std-groups.py` turns the gate's log into that
table. The runners take a `clock::Clock`: a millisecond reading and an
async millisecond sleep, no `sleep_until`, because both loops sleep a fixed
poll and re-read the clock. The terminal's is `clock::Tokio`, so the
paused-clock tests still drive it. `fetch` renders each message to bytes,
the header through `core::fmt::Write`, and prints it through
`Effects::print`: a body is forwarded byte for byte, so a `fmt::Write`
sink for the whole message would have had to replace what is not UTF-8.

**Status, 2026-10-25 (993).** Passes 4 and 5 are in, and the gate is green
and in `guards` (0.76 s standing alone after a `leviculum-core` touch). The
`Engine` holds no `NodeCore` type of its own: its two hooks are inherent
methods generic over `R: CryptoRngCore, C: Clock, S: Storage`, the bounds
the router, the LXMF node and the propagation transport take. It talks to
its host through four `VecDeque`s (commands in, events out, stamp jobs out,
stamp answers in), unbounded like the channels they replace, because a
hook under the core lock may neither block nor drop and a dropped event can
be a received message; the host drains them around every hook. `mine` is
the core's stamp policy: the cooperative executor, the host's RNG, and the
progress lines as `Effects::notice` on the host's `Clock`. The shell is
`engine/shell.rs`: the `CoreProcessor` for `StdNodeCore` that moves the
queues onto `mpsc`, `InProcessOutbox`, `attach` with `storage_dir`, and the
stamp worker thread with its tokio runtime and `OsRng`. The positive control
(a `std::fs::read` in `mailbox.rs`) turns the gate red with one error.

**What stays std-only, and what the core needs from it.** `leviculum-std`
(the driver, the daemon RPC, the event-log subscriber, identity minting,
the passwd lookup): the core needs `Scalar` (pass 1) and the
`CoreProcessor` hooks' argument type, which becomes the generic `NodeCore`
(pass 4). `tokio`: the core needs a clock and a sleep (pass 3). `clap`:
the binary only. `rand_core`'s `OsRng`: the shell hands the core an RNG.

## 5. Decision: the event loop and the TEA split

`lnomad`'s split survives contact with a messenger with one change.

The shape that follows from the driver seam is three layers, not two:

```
  crossterm events ──┐
  router events    ──┼──> AppEvent ──> update(&mut Model) ──> Vec<Effect>
  timer            ──┘                                              │
                                                                    v
                                                            run_effects
                                                                    │
                                          Command queue ────────────┘
                                                    │
                                                    v
                              CoreProcessor::on_tick / on_event
                                (LxmfRouter, under the core lock)
                                                    │
                                          RouterEvent queue
                                                    │
                                                    └──> AppEvent
```

The processor is not part of the TEA model. It is a second, synchronous
state machine on the far side of two queues, and it is testable on its own
terms without a terminal, exactly as `leviculum-lxmf-node` is.

Three specific things `lnomad` does that must change:

1. **Bottom-anchored scrolling with a pinned flag.** `lnomad`'s `scroll` is
   the index of the top visible line (`scrolled`, `leviculum-ui/src/keymap.rs:639-653`). A
   message list wants a "pinned to bottom" boolean so an inbound message
   appends without yanking the viewport out from under a user who has
   scrolled up. NomadNet gets this wrong: it resets to the bottom on every
   refresh (NomadNet's `Conversations.py`, line 2287).
2. **Windowed layout.** `lnomad` re-lays out the whole page on every
   relayout (`lnomad/src/app/model.rs:690-714`). A ten-thousand-message
   conversation must not do that, and a compose buffer must not trigger it
   per keystroke. Lay out the visible window plus a margin, and cache per
   message keyed by `(message_id, width, theme)`.
3. **The timer must run.** `lnomad` disables its tick when idle
   (`lnomad/src/tui.rs:1649`). A messenger has relative timestamps, a sync
   schedule and retry deadlines. A one-second tick when there is anything
   pending, and a slower one otherwise, driven by `next_deadline()`
   (`leviculum-lxmf/src/router.rs:2057`).

Things to carry over unchanged: the generation counter for stale-result
rejection (`spawn_fetch`, `lnomad/src/tui.rs:861-902`), the tick-counted
toast whose expiry is a pure function and therefore unit-testable without
real time passing (`Toast`, `leviculum-ui/src/toast.rs:23-33`, test at
`lnomad/src/tui.rs:2767`),
the `TerminalGuard` RAII plus panic hook that restores the terminal before
the backtrace prints (`leviculum-ui-ratatui/src/terminal.rs:96-138`), and OSC 52 for the
clipboard so copy works over SSH with no X11 dependency (`osc52`,
`leviculum-ui/src/clipboard.rs:14-42`, shared with `loquax` since 1036).

**One thing to fix from day one**: `lnomad` is 11,873 lines in
`src/tui.rs`. A messenger has strictly more state. Split
`model.rs` / `event.rs` / `update/` / `view/` / `shell.rs` before the first
thousand lines, not after the tenth.

## 6. Decision: scriptability

`lnomad`'s `--print` prints rendered ANSI and nothing machine-readable
(`lnomad/src/render.rs:144-147`); there is no JSON anywhere in the crate.
For a browser that is defensible. For a messenger it is a missed
opportunity: "send me a message when the backup finishes" is a real use and
needs no UI at all.

Non-interactive subcommands from the start, following `lnomad`'s automatic
non-tty detection (`lnomad/src/main.rs:174-175`) and its exit-code
convention (0, 1, 2):

```
loquax send <address> [--title T] [--from NAME] [--attach F] [--via direct|propagated] [-]
loquax read [--conversation A] [--since T] [--unread] [--json]
loquax sync [--json]
loquax contacts [--json]
loquax paper <address> -           # emit an lxm:// URI
loquax ingest <lxm://...>
```

with `--json` producing one object per line so `jq` works, and the exit
code distinguishing "sent" from "queued but not confirmed", which a script
genuinely needs to know.

**Decision (2026-08-10)**: `send` returns immediately with the message ID
on stdout; exit 0 means "queued cleanly" and claims nothing more, so it
never lies. `loquax status <id>` answers at any time (state, ledger,
`--json`). `--wait` opts into blocking until the delivery proof, with a
configurable timeout, and its exit codes distinguish delivered /
still-pending-at-timeout / terminally-failed — a timeout is not reported as
a failure, because the message may still arrive. Rationale: the common case
is a script that must not hang, and enqueueing is the only operation whose
success is knowable immediately; everything after it is a history, not a
result.

**Amended (2026-08-21): the message ID comes off stdout.** A successful
`loquax send` now prints nothing at all and exits 0; errors keep going to
stderr. The ID is not interesting to the person running the command, and
saying nothing on success is the ordinary Unix contract — a cron job that
mails its output should mail nothing when the send worked. The rest of this
decision is untouched: **exit 0 still means "queued cleanly" and claims
nothing about delivery**, and that is now the entire success signal, which is
why the exit code is what the tests assert. The ID does not become
unobtainable, because `loquax status <id>` needs it: `LNMSG_ENQUEUED … id=…`
carries it into the structured event log, which `LEVICULUM_EVENT_LOG=<path>`
turns on and which is written by an unfiltered layer, so the line arrives even
at the `warn` default (`leviculum-std/src/event_log.rs:770-777`). No
`--print-id` flag was added: nothing consumes the ID today, and an option
added against a hypothetical user is an option nobody tests.

**Decision (2026-08-21): the sender's name.** The delivery announce carried
the literal `loquax`, which names the tool rather than the person, so every
recipient saw the same sender for every operator on every host. The default
is now the account name, resolved in this order: `getpwuid(getuid())` first,
then `$USER`, then `$LOGNAME`, then `loquax` as a last resort. The password
database comes first deliberately — the first real consumer is a health
monitor started from cron, whose environment has no `$USER` at all, and a
name that is right interactively and wrong from cron would be discovered
late and by a machine. Resolution never fails: a status line that does not
go out is worse than one from an oddly-named sender.

`--from NAME`, and `LNMSG_DISPLAY_NAME` for the cron case, override it,
with the flag winning. They exist because a bare account name is ambiguous
when the same user runs the monitor on several machines — but what goes in
them is the operator's choice, not a policy of ours: no automatic hostname
suffix and no templating. An empty or whitespace-only override is exit 2,
not a silent fall back to the default, since it was set on purpose.
`loquax/src/display_name.rs` holds the order; `LNMSG_SENDER from=… source=…`
records which step answered, which is what separates a cron run that fell
through to the last resort from an interactive one that read `$USER`.

## 7. Structured event log

[Structured event logs](../structured-event-logs.md) and the project's
debugging discipline call for `EVENT_NAME key=val t=<ms>` lines. A
messenger that can be started with a log file, and whose every protocol
transition appears in it, is debuggable in the field in a way that no
terminal-scrollback client is. `lnomad` has no `tracing` dependency at all.
This one is cheap and is not treated as speculative.

## 8. What the library does not expose

Naming these is useful because each is a candidate issue.

**Decision (2026-08-10) on sequencing:** triage, not either extreme. Gaps
1, 2, 3 and 8 are closed in one library wave **before** `loquax` starts,
because the decided design cannot be built honestly without them: gap 2
blocks the propagation node glyph and the truthful delivery display outright, gap 1
blocks the naming-based trust model, gap 3 is shared infrastructure every
client rewrites, and gap 8 is small and stops two clients wording the same
errors differently. The remaining eight are filed as issues and met in
build order — the Codeberg #196 precedent (the library's biggest gap was
found by building a real consumer) argues for letting `loquax` discover the
gaps nobody has named yet, but waiting to "discover" a gap that is already
understood is delay, not empiricism. Gaps 10 and 11 are already covered by
the queued #204/#202/#203 batch; gap 7 shares its core-side prerequisite
with the S2 test-infrastructure question from the #212 work.

**Closed 2026-08-11.** The four are done: `RouterEvent::PeerAnnounced` (1),
`MessageState::AwaitingCollection` (2), `FileLxmfStorage` in `leviculum-std`
(3), and `Display` plus `core::error::Error` on the error types (8). The
entries below are left as written — they are the record of what was missing,
not a list of open work.

1. **No display name reaches the application.**
   `LxmfNodeEvent::PeerAnnounced` carries the destination hash only
   (`leviculum-lxmf/src/node.rs:133-135`), the router drops the name after
   reading the stamp cost (`leviculum-lxmf/src/router.rs:1218-1228`), and
   `RouterEvent` has no announce variant. Every client will re-implement
   announce filtering and `DeliveryAnnounce::decode`. A
   `RouterEvent::PeerAnnounced { destination, announce }` would remove that
   duplication.
2. **No "propagated, awaiting collection" state.** A propagated message
   reaches `Sent` and its queue entry is deleted
   (`leviculum-lxmf/src/router/propagation_runtime.rs:438-445`), so the
   client cannot distinguish "at a propagation node" from "gone" without keeping its
   own shadow record. This is the single biggest obstacle to an honest
   delivery display.
3. **No file-backed `LxmfStorage`.** Two implementations exist, both
   in-memory or null (`leviculum-lxmf/src/storage.rs:42`,
   `leviculum-lxmf/src/storage.rs:116`). Every host application writes the
   same one.
4. **No periodic sync scheduler and no interval config.**
   `PropagationClientConfig` has three fields
   (`leviculum-lxmf/src/router/propagation_runtime.rs:56-68`). Arguably
   correct for a sans-IO crate, but it means every client invents its own
   policy.
5. **Known propagation nodes and the selection are not in the snapshot**
   (`leviculum-lxmf/src/router.rs:2126-2143`), so every client writes its
   own persistence and replay.
6. **No stamp cancellation or deadline.** `generate` loops until success
   (`leviculum-lxmf/src/stamp.rs:356-367`) and `StampError::Cancelled` is
   declared but never constructed (`leviculum-lxmf/src/stamp.rs:25`). A
   user who starts a message to a high-cost peer and changes their mind has
   no way out.
7. **No inbound Resource cancellation**, stated as deliberate pending core
   support (`leviculum-lxmf/src/node.rs:522-523`). A user receiving a large
   attachment they do not want can only watch.
8. **Most error types are `Debug` only.** `RouterError`
   (`leviculum-lxmf/src/router.rs:362`), `LxmfNodeError`
   (`leviculum-lxmf/src/node.rs:257`), `PropagationTransportError`
   (`leviculum-lxmf/src/propagation_client.rs:144`), `MessageError`
   (`leviculum-lxmf/src/message.rs:40`) and `StorageError` have no
   `Display`. Every user-facing string is the client's to write, and two
   clients will word them differently.
9. **No typed codecs for reply, thread or renderer fields**
   (`leviculum-lxmf/src/constants.rs:68-86`; reactions have one since
   1104, `leviculum-lxmf/src/reaction.rs`), so each client hand-rolls
   msgpack for the same wire structures. This is a compatibility risk more
   than an ergonomics one.
10. **Codeberg #203** (`StampExecutor::generate` returns a `!Send` future)
    applies to us as it applied to `leviculum-lxmf-node`, which worked
    around it with a dedicated thread running a current-thread runtime
    (`leviculum-lxmf-node/src/main.rs:441-492`). We will make the same
    workaround.
11. **Codeberg #204** (a hook owns the events its own core calls return) is
    a documentation gap we will hit on day one. The bounded re-feed loop is
    not optional.
12. **Codeberg #186** (LXMF caches age on wall-clock time and are wiped by
    a timebase jump) matters more for a laptop that suspends than for a
    daemon that runs continuously, and should be checked against the
    suspend-resume path before it is dismissed.
