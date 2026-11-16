# Structured event logs

Test-harness scaffolding (Codeberg #39 piece 1, Stage 6) for capturing
mesh-protocol events as parseable lines so multi-node failures can be
diagnosed from a single merged log instead of N hand-correlated
process traces.

## Format

Each emitted event renders to a single line:

```text
EVENT_NAME node=<n> key1=val1 key2=val2 ... t=<rel-ms>
```

Rules:

- `EVENT_NAME` first.  Comes from the literal string passed as the
  `event` field in a `tracing::debug!` call.
- `node=` second.  Value comes from `LEVICULUM_EVENT_NODE`
  environment variable; defaults to `local`.
- All other keys appear alphabetically sorted between `node=` and
  `t=`.
- `t=` last.  Millisecond offset from layer registration time.

Records that don't carry an `event = "..."` field are silently
dropped, so the legacy printf-style `tracing::debug!("[FOO] ...")`
sites stay valid alongside the converted ones.

## Per-packet journey contract

The packet-level events `PKT_TX`, `PKT_RX`, `PKT_FORWARD`, `PKT_DROP`
and `DEDUP_DROP` form the journey contract an external collector uses
to stitch one packet's path across nodes:

- They are emitted on the dedicated tracing target
  `leviculum_core::pkt` (DEBUG), so a collector can enable exactly this
  stream via `RUST_LOG=leviculum_core::pkt=debug` without the rest of
  the transport noise.  The event-log layer sees every record
  regardless of target.
- Each carries `ph`, the first 16 hex chars of the dedup packet hash
  (SHA-256 over the hashable part, which strips `hops` and
  `transport_id`).  `ph` is therefore stable across hops and across
  Type1/Type2 header conversion: the same value appears in the
  sender's `PKT_TX`, every relay's `PKT_RX`/`PKT_FORWARD` and the
  receiver's `PKT_RX`, or in the `PKT_DROP`/`DEDUP_DROP` where the
  packet died.
- `PKT_DROP` renders its `reason` as the kebab-case `DropReason`
  (`no-path`, `plain-group-multihop`, `forward-max-hops`, ...).
  `unknown-context` is the one reason that says nothing about the
  packet's validity: it means the packet was addressed to US and
  carries a context byte this build assigns no meaning to, so nothing
  above transport could interpret it.  The same packet addressed to
  someone else is relayed normally and never reaches this counter —
  the context byte is semantic, not routing information.  A rising
  `unknown-context` on a node that is also an endpoint means a peer
  speaks a dialect (newer RNS, third implementation) we do not.
- Every `PKT_DROP` carries the same keys: `ph`, `dst`, `type`, `hops`,
  `iface_in` and `reason` (one catalogue shape, enforced against the
  source by `just event-catalog`).  `iface_in=None` marks a packet this
  node originated.
- `no-such-interface` is the one `PKT_DROP` on the OUTBOUND path: the
  action was addressed to an interface the driver's dispatch slice does
  not contain.  It reads `dst`, `type` and `hops` from the frame's own
  bytes, writes `iface_in=None`, and adds `iface_out` (the index it
  wanted) and `len`.  On a node whose interfaces come and go at runtime
  (autoconnect, TCP sessions) a rare one is expected: a link this node
  holds keeps its attached interface after that interface went, and an
  output committed from an API task can reach the dispatch after the
  driver removed the interface (919,
  `leviculum-core/src/node/mvr_interface_gone_dispatch.rs`).  The path
  table is never the source; `handle_interface_down` removes its entries.
  A steady rate on a node with fixed interfaces means the driver and the
  core disagree about interface numbering.  It does not share a counter
  with `no-path`.
- The link half of that is limited to teardowns that may return (931,
  Codeberg #468).  Each teardown logs `IFACE_DOWN` with `idx` (the number
  a later drop names as `iface_out`), `teardown=final|transient` and
  `links_closed`.  A `final` teardown closes every own link on the
  interface at once, `LinkClosed` with reason `interface-removed` and no
  close frame, so its links produce no further drop; a `transient` one
  keeps them for an interface that comes back under the same index.  The
  driver's table (`TeardownCause::class`, `leviculum-std/src/driver/mod.rs`):

  | Teardown site | Class | Why |
  | --- | --- | --- |
  | `remove_interface` (operator) | `final` | taken away for good |
  | auto-connect peer retired (record expired, offline past the threshold) | `final` | a return draws a fresh index |
  | `bootstrap_only` detach | `transient` | re-attached under its original index |
  | incoming channel closed (TCP session ended, serial, BLE, RNode task ended) | `transient` | the handle does not say whether it returns |

  When a site cannot be sure it is `transient`: a wrong `transient`
  costs a link that lingers to its stale timeout, a wrong `final` closes
  one that would have carried on.  The peer sees the same either way:
  silence, then its own stale timeout.
- The interface-up half (981): a reconnect notification crosses the
  teardown of the interface it names in a driver channel (a retired
  auto-connect client whose reconnect raced the detach poll), and
  honouring it would re-announce every local destination to the gone
  index — one `no-such-interface` drop per destination, same
  millisecond.  The core tombstones every index it tears down and
  refuses a later `handle_interface_up` for it with `IFACE_UP_STALE
  idx=`; the next registration under that index
  (`set_interface_name`) clears the tombstone
  (`leviculum-core/src/node/mvr_interface_up_after_teardown.rs`).
  The driver drops such a reconnect, and a per-peer transition for
  the same index, before it touches the core at all, with
  `IFACE_EVENT_STALE idx= kind=reconnect|peer` (989): the core's
  refusal covers the re-announce, not the IFAC config the reconnect
  branch re-applies or the peer count and path pulls a peer event
  sets.
- A relay whose outbound path points back out of the arrival interface
  forwards there — same-interface relay on a shared medium is a normal
  hop, not a drop (see
  [Python-RNS Compatibility](concepts/python-rns-compatibility.md#same-interface-relay-on-shared-media)).
  Its `PKT_FORWARD` carries `iface_out` equal to `iface_in`.
- `PKT_TX` on a `Broadcast` action reports `iface=bcast`: the sans-I/O
  core does not know the concrete interface set the driver expands the
  broadcast to; journeys stitch by `ph`.
- `PKT_TX` and `PKT_RX` both carry `hops`, but they are counted at
  different points and the difference is the contract:
  - `PKT_TX hops` is the hop count of the packet **as transmitted** —
    the byte that goes on the wire, read from the packed buffer being
    handed to the driver.
  - `PKT_RX hops` is the hop count **after receipt**, i.e. after the
    receiver's increment (`Transport::incoming_hop_count`, mirroring
    Python `Transport.py:1457`).

  So for one `ph` crossing one radio hop, `rx hops = tx hops + 1`.  A
  collector reads a journey's DIRECTION from exactly that relation: the
  node observing the packet at the lower hop count transmitted it, the
  node at one more heard that transmission.  Without `hops` on
  `PKT_TX`, a node that ORIGINATES a packet (a firmware node, or a
  daemon's own announces, path requests and link proofs) contributes no
  hop count at all and drops out of that relation.

  The relation is deliberately +1 only for a real medium crossing.  Over
  the local-IPC hop — a `LocalClient` interface, or the uplink to a
  shared instance — `incoming_hop_count` undoes its own increment, so
  there `rx hops = tx hops`.  That is Python's behaviour (`Transport.py`
  :1481-1484) and it is correct: the IPC hop is not a network hop.  A
  collector pairing on `+1` therefore ignores IPC hops, which is what it
  should do.
- The hash is never computed twice for one packet: emission sites
  reuse the dedup/cache hash where it exists and otherwise hash only
  while the `leviculum_core::pkt` target is enabled.  With the target
  disabled the whole contract is zero-cost.
- Deliberate exclusions: the high-volume overheard drop
  (`overheard-transport-id`) and IFAC drops stay counter-only
  (`PKT_DROP_SUMMARY`); announce-pipeline drops (replay, rate-limit,
  ingress-burst, over-max-hops, blackhole) are covered by the
  announce event family and the summary counters.

## Architecture

**All test threads, including tokio multi-thread workers, route
through the same global subscriber registered once via Layer
composition.**

Specifically: `tracing_setup::init_tracing_with_event_log()` builds a
`Registry::default().with(fmt_layer).with(event_log_layer)` chain
and installs it via `set_global_default` once per process (Once-
guarded).  Every thread, every spawned future, every tokio worker
inherits this global subscriber.  This is the load-bearing
architectural choice that lets a `#[tokio::test(multi_thread)]` mvr
see events emitted from worker threads.

Per-test buffer isolation is built on top: `init_event_log()`
returns an `EventLogHandle` whose `Arc<Mutex<Vec<String>>>` buffer
is registered in the layer's active-handles list.  The layer's
`on_event` iterates the active list and pushes the formatted line
to every active buffer.  When the test's handle drops, it removes
itself from the list.

Concurrency consequence: every active buffer receives every event
the layer sees, regardless of which test emitted it.  Tests that
assert on buffer contents must filter by event name to avoid
cross-test pollution.  Use disjoint event names per test
(`EV_BASIC`, `EV_VIOLATION`, …); mvr tests already enforce
`--test-threads=1` so this only affects unit tests.

## How to wire a test

Inside any test, before the test body runs:

```rust
let _evlog = leviculum_std::test_support::event_log::init_event_log();
```

The handle is RAII: when the binding goes out of scope it removes
itself from the layer's active-handles list.  If the test thread
is panicking at drop-time, the buffer dumps to stderr with a
`=== EVENT LOG DUMP …` banner that `cargo test` surfaces in the
failure listing.

Use `init_event_log_to_file(path)` instead of `init_event_log()`
when the test wants to assert on the dumped content directly
(`std::fs::read_to_string(path)`).

To make the test fail loud on undocumented schema gaps, end the
test body with:

```rust
leviculum_std::assert_no_schema_violations!(_evlog);
```

It panics if any `EVENT_SCHEMA_VIOLATION` line appears in the
buffer.

## How to add an event

Two steps, both in the same commit:

1. **Convert the call site.**  Replace the printf-style
   `tracing::debug!` with structured fields:

   ```rust
   tracing::debug!(
       event = "FOO",
       iface = %iface_name,
       dst   = %HexShort(&dst_hash),
       hops  = packet.hops,
       len   = bytes.len(),
   );
   ```

   `%` for Display, `?` for Debug.  Values must be ASCII without
   whitespace, `=`, or non-printable characters — otherwise the
   field-value validator fires (see below).  For Rust keywords
   like `type`, use the raw identifier `r#type`.

   **No trailing message.**  `tracing::debug!(event = "FOO", a = 1,
   "some prose")` renders the prose under a `message` field whose
   spaces split the line for every token-based parser.  What the
   sentence would have said belongs in a structured field or in a
   comment at the call site.

2. **Add a catalogue entry** in
   `leviculum-std/src/event_log.rs`'s `EVENT_CATALOG`:

   ```rust
   EventSchema {
       name: "FOO",
       required_keys: &["iface", "dst", "hops", "len"],
   },
   ```

   `required_keys` is the INTERSECTION of the keys the name's call
   sites set — the contract is "present on every emission".  Where
   sites differ in a way worth checking, the name gets several
   entries and a record passes if any one shape is fully present
   (Codeberg #320); note that a shorter shape dominates a longer one
   that merely extends it, so a second entry only earns its place
   when neither shape contains the other.  The subscriber checks
   that every catalogued event's emission satisfies some declared
   shape; a record that satisfies none produces a
   `EVENT_SCHEMA_VIOLATION` line in the dumped buffer alongside the
   original event.

Step 2 is not on the honour system.  `leviculum-std`'s
`#[cfg(test)] mod event_catalog_completeness` walks the
`tracing::*!(event = "...")` sites of every workspace member's `src/`
and fails on a name `EVENT_CATALOG` is missing, and on a site that
passes a message argument.  It runs under `cargo test --workspace
--lib`, which is what `just fast` and the forge gate run.

It exists because the honour system had already failed: the miauhaus
soak of 2026-09-18 (397 023 881 events) found eight emitted names
undeclared, `LINK_ENTRY_SET` among them at 612 639 emissions.  An
undeclared name is not cosmetic — the layer validates a shape only for
a name it finds in the catalogue, so an undeclared event can lose a
required field forever and nothing says a word.  What caught
`LINK_ENTRY_SET`'s broken `next_hop`, 252 669 times, was the
field-VALUE check, which runs regardless of the catalogue.

Catalogue entries without a live emitting site are explicitly
discouraged: the runtime-validation layer can't detect them, so
they silently rot.  Only add entries you have a corresponding
emit for.  (That direction is still unchecked, and the catalogue
carries entries whose emitter lives outside this workspace:
`SILENCE_LNODE_ENTER`/`SILENCE_LNODE_EXIT` are periculum's.)

## Firmware-side events

The nRF firmware emits the same line grammar, but not through this
machinery.  `leviculum-nrf` is `no_std` and cannot depend on
`leviculum-std`, so there is no `tracing` subscriber, no `node=`
field, and no runtime schema validation; the line goes into the
debug-CDC ring buffer behind the module prefix that the rest of the
firmware log uses:

```text
[BLE ] BLE_TX_DROP kind=packet len=312 frag=1 of=2 sent=1 reason=stalled code=0 waits=0 dropped=3 t=48210
```

The prefix does not disturb the grammar — the event name is still
one whitespace-delimited token and `grep BLE_TX_DROP` over a
captured debug-port log still works — but the event deliberately
does **not** appear in `EVENT_CATALOG`.  A catalogue entry the
subscriber can never see emitted is exactly the silent rot the rule
above forbids.  Firmware events are documented here and at their
call site instead.

### Who writes `t=` on a firmware line

Nobody at the call site.  Since #344 the firmware's log formatter
(`leviculum-nrf/firmware/src/log.rs`, shape in `leviculum-log-line`) appends
` t=<uptime-ms>` to **every** line it emits — event lines, plain
`[LORA]`/`[INFO]` lines, boot banners, `tracing` records alike.  A
call site that also writes its own `t=` renders the field twice;
`BLE_TX_DROP` did, and stopped.

The stamp is taken when the line is *formatted*, not when it is
drained: the ring is emptied in 64-byte USB packets on a 100 ms
loop, so host arrival times measure that loop and nothing else.

**A line may still legitimately carry two `t=` fields.** The boot
replay of the persistent tail wraps a line from the previous boot,
its stamp included, inside a line of this boot:

```text
[INFO!] [PERSISTENT_LOG] [LORA] RX 41 bytes t=91422 t=137
```

Both are true.  A line's own stamp is always its **last** `t=` —
the same rule `merge_event_logs` already applies, so the two agree.

**Epochs do not.**  A firmware `t=` is milliseconds of board uptime;
a host-side `t=` is milliseconds since that subscriber's init.
Merging a debug-port capture into a host event log with
`merge_event_logs` therefore orders each stream correctly within
itself and says nothing across the two.

Current firmware events:

| Event | Emitted by | Meaning |
|-------|-----------|---------|
| `BLE_TX_DROP` |  `leviculum-nrf/src/ble/notify.rs` | A BLE packet or keepalive was abandoned part-way through its fragments.  `frag=` is the fragment that failed, `sent=` how many did go out, `reason=` one of `stalled` (no HVN-TX-COMPLETE within the bound), `disconnected`, `budget`, `sd_error` (with the raw code in `code=`), `internal`.  `conn=` is the SoftDevice connection handle the TX targeted — the fan-out sends one copy per live link, and without this key the desk log of 2026-09-08 could not say WHICH link ate an `sd_error` (#365).  `dropped=` is the cumulative counter, so one line states both the incident and the running total. |
| `BLE_TX_PKT` | `leviculum-nrf/src/ble/notify.rs` (notify pump) and `leviculum-nrf/src/ble/columba.rs` (central write loop); line rendered by `leviculum-ble-tx`'s `TxPktLine`, verbatim-pinned by its host test | **One line per multi-fragment packet handed to a link, whatever became of it (#373).**  `conn=` the SoftDevice connection handle, `len=` the whole packet, `frags=` how many fragments it split into, `sent=` how many the stack accepted.  A healthy hand-over reads `sent==frags`; `sent<frags` is a loss on this node and is always accompanied by a `BLE_TX_DROP` naming the reason.  The desk log that motivated it had two of five relayed two-fragment packets vanish on the BLE hop with **no** line anywhere — `BLE_TX_DROP` only fires on an abandoned packet, so a fully-accepted packet that still never arrived left nothing to grep.  Single-fragment packets and keepalives stay unlogged (they are the bulk of the traffic and the failure mode needs ≥2 fragments). |
| `BLE_RX_ABANDON` | `leviculum-nrf/src/ble/columba.rs` (both GATT roles) | A link's reassembly was discarded before completion (#373): a new START over an unfinished head, a `total` contradicting the reassembly in progress, or the hard reset after a garbage frame.  Each is one or more whole Reticulum packets this receiver lost — previously a silent state reset.  `slot=` the link's drain slot, `lost=` what this frame cost, `total=` the link's running count (`BleDefragmenter::abandoned_count`).  lnsd's Columba interface emits the same event name identity-keyed (see the host-side section below), so a merged bench timeline carries both receivers. |
| `BLE_TX_RESYNC` | `leviculum-nrf/src/ble/notify.rs` | The connection was dropped deliberately after a torn fragment stream, because the wire protocol has no abort marker and a reconnect is the only in-band reset of the peer's reassembler (#255). |
| `BLE_TX_GAP` | `leviculum-nrf/src/ble/columba.rs` (both pumps) | The per-link inter-packet gap deferred a packet (#376): `conn=` the connection handle, `waited_ms=` how long the packet's first fragment was held back after the previous packet's last.  The gap defaults to 100 ms (`leviculum-ble-tx`'s `DEFAULT_TX_GAP_MS`, the measured desk value); `lnflash --set-ble-tx-gap` overrides it for measurement, `0` disables.  On a paced link with real traffic this line is the expected signature; its absence under back-to-back traffic means the knob was set to 0.  lnsd emits the same event name on its notify pipe (`link=notify`) and central links (`addr=`), and takes the same override as the BLEInterface key `tx_gap_ms`. |
| `BLE_TX_HELD` | `leviculum-nrf/src/ble/columba.rs` (peripheral pump) | The drain held a packet because the peer cannot receive yet (#376): `conn=`, `reason=not-subscribed`.  Once per connection, however many packets wait.  Background: a notify before the central writes the TX CCCD fails with `sd_error code=13313` (`BLE_ERROR_GATTS_SYS_ATTR_MISSING`) and the packet dies — the field T114 lost the first packet of a fresh connection this way at 11:31:18 on 2026-09-09.  Held packets wait in the link's queue and drain after the subscription (or the handshake, whichever is later); policy host-tested in `leviculum-ble-tx`'s `hold` module. |
| `MGMT_DFU` | `leviculum-nrf/src/remote_dfu.rs`; body `MgmtDfuBody` in `leviculum-log-line`, verbatim-pinned by its host test | **The bench window greps this (OTA stage 1, #235, [OTA stage 1](concepts/ota-stage-1-ble-dfu.md)).** One line per `/dfu` request on the management destination, printed before the reply is built: `MGMT_DFU accepted=yes from=<32 hex> reason=accepted` for an allow-listed identity, after which the board sends its reply, waits until every carrier's queue reports it gone (20 s at most) and resets into BLE DFU; `MGMT_DFU accepted=no from=<32 hex\|none> reason=not-listed\|no-identity` for a refusal, which is answered with its typed reply and changes nothing else. `from` is the whole identity hash, so it reads against the list `lnflash --management-identity` wrote. No line follows an accepted one: a line between the wait and the reset could be cut off by the reset, and its formatting cost the bytes that held the RAK4631 above its flash floor; the wait for the reply drains the `MGMT_DFU` line itself. lnsd would emit `MGMT_DFU` with the same keys as a trace event (in `EVENT_CATALOG`), but only a node built with `remote_dfu` serves the verb, and no daemon is. |
| `[ANNOUNCE]` | `leviculum-nrf/src/announce.rs`, and the `TYPE_ANNOUNCE` arm of each binary | **Frozen shape — the desk recipe reads this.** When the board announced one of its destinations, and why (#376, #384). `[ANNOUNCE] sent dst=<hex8> reason=<r>`; `dst` says WHICH destination and `reason` says what occasioned it. For `lxmf.delivery`: `reason=peer-up peer=<hex8>` (a BLE peer finished its identity handshake; the announce goes on that peer's link alone and appears as `BLE_TX_ROUTE` beside it, never as `BLE_TX_FLOOD`), `reason=periodic` (the timer, every 30 minutes, on every interface, so it is a `BLE_TX_FLOOD`) or `reason=host` (`lnflash --announce`, periculum's `announce_board`) or `reason=button` (a double press, #459, see `BTN_PRESS`). For `lxmf.propagation` on a board running the role: `reason=pn-periodic` (the role's own 300 s interval) `reason=pn-button` (a double press) or `reason=pn-host` (the same host command — one `TYPE_ANNOUNCE` frame produces both lines, because a client needs the propagation destination to address the mailbox and the delivery announce does not carry it). For `lev.position` on a board that shares its position: `reason=position-periodic` (the position slot's tick) or `reason=position-button` (a double press with a fix, order 1316, [Announce cadence](concepts/announce-cadence.md#the-double-press)). The telemetry path's own pre-report announce is not named here; it is the broadcast that precedes a `[TELEMETRY] send` line. `[ANNOUNCE] withheld reason=no-clock` is the clock gate — an announce stamped from uptime can never replace a path at the receiver, so a board without a plausible wall clock says why it is silent instead of poisoning path tables; written once per change, not once per retry. `reason=rate-limited peer=<hex8>` under the `[BLE ]` prefix is the per-identity 15-minute limit, which is why a phone rotating its BLE address every minute does not buy an announce every minute. `lnsd` emits the same two occasions as the trace events `ANNOUNCE_TX reason=peer-up peer= iface= count=` and `ANNOUNCE_WITHHELD reason= peer= iface=`. |
| `BLE_TX_ROUTE` / `BLE_TX_FLOOD` / `BLE_TX_ROUTE_MISS` | `leviculum-nrf/src/ble/mod.rs` (the fan-out task) | What the core's per-packet delivery hint made of one outbound packet (#376).  Exactly one of the three per packet, so a capture accounts for everything the interface was handed.  `BLE_TX_ROUTE peer=<hex8> conn=<h> slot=<n> len=<n>` — the packet was addressed at one peer and went on that peer's link alone.  The core takes the peer from the path it routed over (`via_peer`, Codeberg #365), or, for a proof, from the arrival it is answering: the ingress peer travels with the deferred `ProofRequested` event, so a probe's proof leaves as `BLE_TX_ROUTE` and not as a flood.  This is what stops a report for the phone from also travelling to the board beside it, which used to forward it back and give the phone two copies.  `BLE_TX_FLOOD links=<n> len=<n>` — no hint, so a broadcast: announces and path requests still reach every live link.  `BLE_TX_ROUTE_MISS peer=<hex8> len=<n>` — the addressed peer holds no live link here and the packet is dropped, NOT flooded (flooding would spend the other links' airtime on a packet they cannot deliver and rebuild the relayed duplicate); its running total is `route_miss=` on `BLE_COUNTERS`, and a rising value means the path table outlived a link the #365 cull should have taken.  lnsd emits the same three names with `iface=` and, on `BLE_TX_ROUTE`, `conn=central\|peripheral` instead of a SoftDevice handle. |
| `BLE: RX` | `leviculum-nrf/src/ble/columba.rs` (both GATT roles) | One line per reassembled inbound packet: `BLE: RX <n>B conn=<h> frags=<k>` — `conn=` tells the phone's link from the neighbour board's, `frags=` how the PEER fragmented the packet (`BleDefragmenter::last_completed_fragments`), which is the only place a peer's real fragment size is visible (#376: Columba as peripheral claimed "MTU 20 bytes" while our central negotiated a large ATT MTU).  The binaries' former `BLE RX <n> bytes` line was dropped for it: one reception, one line. |
| `BLE_CONN_PARAMS` | `leviculum-nrf/src/ble/columba.rs` (both roles); line rendered by `leviculum-ble-tx`'s `ConnParamsLine`, byte-pinned by its host tests | **What a link actually runs at (#385).**  One line per link, at the connection event, beside `BLE: connected` on the peripheral side and beside the successful dial on the central one: `BLE_CONN_PARAMS conn=<h> role=central\|peripheral interval_ms=<n.nn> latency=<n> timeout_ms=<n>`.  Neither stack requests connection parameters, so these are the central's choice inherited whole — and the supervision timeout among them is exactly how long a radio disturbance may last before the link dies (the #385 bench: a board and lnsd one metre apart lost their link 36 times in 16.2 hours, every connection at a 45 ms interval with a 420 ms supervision timeout, every death HCI reason 0x08).  Both time fields are printed in **milliseconds**, converted from the two different raw scales the controller reports (interval in 1.25 ms steps, hence the two decimals; supervision timeout in 10 ms steps), so no reader of a field log has to remember which scale belongs to which field.  `latency=` is a count of skippable connection events, not a time, and is printed unconverted.  A **peripheral** link emits the line a second time when it ends, marked `when=close` and carrying the same `conn=` (#385): the SoftDevice updates the connection's stored parameters on `BLE_GAP_EVT_CONN_PARAM_UPDATE` without handing the event to application code (`nrf-softdevice`'s `gap.rs`), so a renegotiation cannot be logged as it happens — but the stored copy survives the disconnect, so re-reading it at teardown states the values the link actually ended on.  That pair is the only evidence of what a central did with the board's update request (below): opened 420 ms, closed 4000 ms means honoured; closed 420 ms means not.  A central link emits the open line only.  lnsd cannot emit this event at all; see the host-side section below. |
| `BLE_CONN_PARAMS_REQ` | `leviculum-nrf/src/ble/columba.rs` (peripheral role only); line rendered by `leviculum-ble-tx`'s `ConnParamsReqLine`, decision in its `judge_supervision_timeout`, both byte- and boundary-pinned by host tests | **Whether the board asked its central for a supervision timeout it can survive, and what came of asking (#385).**  One line per peripheral link, right after the link's `BLE_CONN_PARAMS`: `BLE_CONN_PARAMS_REQ conn=<h> timeout_ms=<n> result=sent\|refused\|skipped`.  The board is the peripheral in exactly the two cases that are bad or unknown — an lnsd central gives it BlueZ's 420 ms, a phone gives it something unmeasured — and the host cannot set these values through the API lnsd uses, so the peripheral asking is the only lever.  The rule is conditional: below a 2000 ms floor the link asks for 4000 ms (the same value `ConnectConfig::default` asks for in the central role, so the two roles agree), at or above the floor it asks for nothing, because an update request that fights an already-good value is a regression.  `timeout_ms=` is the value asked for on `sent`/`refused` and the value that passed the floor on `skipped`.  `result=sent` says only that the request left the board — on a peripheral it is an L2CAP connection parameter update request, which a central may honour, ignore, or answer with something else entirely, and only the `when=close` line says which.  `refused` is a local refusal by the SoftDevice and is not retried.  Board-to-board links never emit anything but `skipped`: their central already asks for 4 s.  No lnsd counterpart, for the same BlueZ reason as `BLE_CONN_PARAMS`. |
| `BLE_LINK_DOWN` | `leviculum-nrf/src/ble/columba.rs` `log_link_down` (both roles); line rendered by `leviculum-ble-tx`'s `LinkDownLine`, reason mapped by its `DisconnectReason::from_hci`, both byte-pinned by host tests | **One line for every link end on the board, whichever side ended it and in whichever role (912).**  `BLE_LINK_DOWN role=central\|peripheral conn=<h> peer=<hex8\|none> reason=<r>`, gate-free.  `conn=` is the handle the link's `BLE_CONN_PARAMS` lines carry; `peer=` is the identity the handshake presented (peripheral: the 16-byte write, central: the Identity read), `none` when the link ended before it, and named even when the registry then refused it.  `reason=` is the HCI status of the SoftDevice's disconnect event: `supervision-timeout` (0x08, nothing heard for the link's supervision timeout: the peer walked out of range or its radio died), `remote-user` (0x13, the peer closed it: a phone switching Bluetooth off closes every board's link within milliseconds), `local-host` (0x16, the board closed it: an expiry, a replacement, a refusal, a carrier switch, a handshake timeout, each with its own line before this one), `connection-failed-to-establish` (0x3e), `mic-failure` (0x3d), `other-0x<hh>` for any other status, and `unknown` when a link the board was closing produced no disconnect event within its supervision timeout plus 1 s.  A peripheral link prints it after the session, before `BLE: disconnected` and the `when=close` parameters; a central link prints it after `BLE_CENTRAL_DOWN` when the session was reached, and after the `BLE_CENTRAL_FAIL`, `BLE_LINK_SELF` or refusal line on a dial that gave up before it.  A dial whose connect never succeeded had no link and prints only `BLE_CENTRAL_FAIL stage=connect`.  `BLE_CENTRAL_DOWN` keeps its own job, the session length the dial ledger is judged by, and prints for registered central sessions only.  lnsd's `BLE_LINK_DOWN` has the same name and `role=`/`peer=` keys with lnsd's own reason vocabulary; see the host-side section below. |
| `BLE_LINK_FORGOTTEN` | `leviculum-nrf/src/ble/columba.rs` `log_link_down` (both roles); line rendered by `leviculum-ble-tx`'s `LinkForgottenLine`, byte-pinned by a host test | **A link whose disconnect event never came, given up without dropping its `Connection` (#472).**  `BLE_LINK_FORGOTTEN role=central\|peripheral conn=<h> waited_ms=<n>`, gate-free, after that link's `BLE_LINK_DOWN reason=unknown`.  The teardown holds the `Connection` past `BLE_LINK_DOWN` while the SoftDevice has not reported the disconnect, because dropping the last handle mid-disconnect is what panicked boards in upstream nrf-softdevice; at `forget_deadline_ms` (the longest supervision timeout, 32 s, plus 1 s, counted from the first `disconnect()`) it leaks the handle instead, so the connection slot it held stays taken until the next boot.  `conn=` is the handle of the `BLE_LINK_DOWN` line before it; `waited_ms` is the time from the first `disconnect()` to the give-up.  Never seen on the bench so far: every `BLE_LINK_DOWN` since 912 had its reason inside the settle bound, where this wait does not run. |
| `BLE_DRAIN_TABLE_FULL` | `leviculum-nrf/src/ble/columba.rs` | A connection could not claim a per-connection HVN drain slot; `slots=` is the table size.  Expected never: it means more live connections than `ble::MAX_LINKS`. |
| `BLE_GATT_WRITE_OVERSIZE` / `BLE_GATT_NOTIFY_OVERSIZE` | `leviculum-nrf/src/ble/columba.rs` (peripheral write path / central notification path); line rendered by `leviculum-ble-tx`'s `OversizeLine`, verbatim-pinned by its host test | An inbound GATT value exceeded the characteristic bound and was dropped whole (#387): `conn=` the SoftDevice connection handle, `len=` the peer's wire length, `max=` the bound (`GATT_VALUE_MAX`, 253 = our ATT MTU grant of 256 − 3).  Truncating instead of dropping would hand the defragmenter a cut Columba fragment that completes a reassembly with garbage.  Before #387 this input was not a line but a board panic — the vendored `GattValue` conversion unwraps on length (nrf-softdevice gatt_traits.rs:97), and the field T114 died on it twice on 2026-09-12; the same event also disproved the assumption that the SoftDevice rejects over-`max_len` writes before an event exists.  Running total is `oversize=` on `BLE_COUNTERS`.  Expected never from a healthy peer: a full-MTU Columba fragment is exactly 253 bytes and is accepted (the old width of 251, the link-layer DLE payload, was 2 bytes short of that, which is how a healthy phone panicked the board). |
| `[FW_BUILD]` | `leviculum-nrf/src/banner.rs`, spawned by each board binary's `fw_build_banner` task; cadence host-tested in `leviculum-log-line`'s `banner` module | **Frozen shape — lnflash, the flash runners, the boot proof and periculum's discovery read this.** Which image the board runs: `[FW_BUILD] git_sha=<sha> dirty=true\|false`, the `FW_BUILD_STAMP` literal the runner also greps out of the image it writes.  Printed once in the boot block and then as the head of the banner group, `[FW_BUILD]`, `[TIME_SOURCE]`, `[MEDIA]`, `[NAME ]`, `[IDENTITY]`, all five on one timer: every 5 s for the first 120 s of uptime (`BANNER_FAST_S`), every 300 s after that (`BANNER_SLOW_S`), so 311 groups a day instead of 17 280 (911).  Every reader that waits for the next banner reads inside the first two minutes after a reset it caused: lnflash within its 15 s `FRESH_BANNER_BUDGET`, `tools/fw-readback.sh` and `flash-lnodes-from-head.sh` within three 8 s windows, the boot proof 45 s after its flash.  A reader of a board it did not reset sends a `b` byte on the debug port, which prints the group at once (`leviculum-usb-policy`'s `debug_commands`); firmware before 911 ignores the byte and still repeats every 5 s.  `[BOOT] path_table_initial_len=` is not part of the group: it repeats six times at 10 s after boot and stops. |
| `[IDENTITY]` | `leviculum-nrf/firmware/src/identity.rs` (`log_banner`) | The board's published hashes: `[IDENTITY] identity=<hex32> probe=<hex32\|none> lxmf=<hex32\|none> lxmf_propagation=<hex32\|none>`.  Not in the boot block: its first line is the banner group's first, 5 s after boot, then on the `[FW_BUILD]` cadence.  periculum's `ble_participants` resolves a board's seat from it. |
| `[MEDIA]` | `leviculum-nrf/src/media.rs` | **Frozen shape — assertions read this.** Which carriers this node meshes over: `lora=on\|off ble=on\|off src=default\|flash`.  The two carrier fields are what the board is **running** (a carrier configured on but not started this boot reads `off` here, which is the honest answer), and `src=` says whether the profile came off the flash page or from the both-on default.  Emitted once per boot after both spawn decisions, then re-emitted with `[FW_BUILD]` (see that row for the cadence) so a capture attached after the boot window still reads the carriers off the board.  A carrier held down by the profile also emits `carrier=<lora\|ble> state=down reason=profile` under the same prefix at the point its bring-up would have been, and packets dropped because their medium is off emit `MEDIA_TX_DROP iface=<name> packets=<n> bytes=<n> reason=carrier-off` — not one line per packet but on the first drop of a run and at each decade after it (the 1st, 10th, 100th …), because every log line also writes the 2 KiB post-crash tail and a carrier that is off drops one packet per announce; `MEDIA_TX_RESUMED iface=<name> packets=<n> bytes=<n>` closes the run with its exact totals when the carrier takes a packet again (host tests in `leviculum-nrf/media-state`).  See `docs/src/concepts/media-profiles.md`. |
| `BATTERY` | `leviculum-nrf/firmware/src/battery.rs` (since #469 991; the SAADC gauge in `leviculum-nrf/src/battery.rs`) on all three boards; line rendered by `leviculum-battery-scale`'s `BatteryLine`, byte-pinned by its host tests | **What the pack is doing (#380).**  `BATTERY mv=<n> min_mv=<n> max_mv=<n> pct=<n>\|none cells=<n>S`, once at boot and then every 30 s, under the `[BAT] ` prefix.  `mv=` is the filtered pack voltage — the same number the status panel shows — while `min_mv`/`max_mv` are the lowest and highest of the raw 1 Hz samples in the period, which is where a sag under transmit load appears instead of being averaged into the mean.  `pct=` comes from the per-cell LiPo OCV curve, `cells=` from the classification made on the boot's first reading — and `pct=none` is the case where the pack voltage is outside the band that classification implies, see `BATTERY_PCT` below.  Before this the module fed the display and said nothing: a Pocket V2 that restarted twice on a 90 minute field walk produced zero battery lines, so how close the pack had been to the edge could not be asked afterwards.  It is a margin instrument and not a brownout detector — the sampler is a second apart, a brownout is microseconds wide, and the reset takes the log with it (that walk's every boot came up `reset_reason=0x00000000`, `BOOT_TRACE prev_magic=absent`).  A first reading outside anything a LiPo pack can be (below 2.5 V or above 9 V — on the T114 the divider reaches 17.7 V and on the Solar Node 10.7 V, so a floating input lands there) is NOT classified as 2S: it falls back to 1S and says so as `[WARN] [BAT] implausible first reading`.  The `[BAT] init` line beside it carries `full_scale_mv=`, the board's whole measurable range, which is 6228 on the Pocket V2, 17698 on the T114 and 10660 on the Solar Node, and `acq_us=`, the SAADC acquisition window the board's divider needs — 10 on the first two and 20 on the Solar Node, whose 1 M∕510 k divider presents 338 kΩ of source where the part specifies 10 µs for 100 kΩ (#233).  The two boot lines bypass the `RUNTIME_DRAIN_OPEN` gate and the periodic ones do not: the gate stays shut until DTR-assert or 30 s of uptime, and a board on battery in a field has no host to assert DTR — which is precisely the run whose first statement is worth keeping. |
| `BATTERY_PCT` | `leviculum-nrf/firmware/src/battery.rs` on all three boards; line rendered by `leviculum-battery-scale`'s `BatteryPercentLine`, band and boundaries pinned by its host tests | **Whether the charge percentage can be believed (#380).**  `BATTERY_PCT reportable=0\|1 pack_mv=<n> cells=<n>S band_lo_mv=<n> band_hi_mv=<n>`, under the `[BAT] ` prefix, said only when that answer CHANGES — plus once at boot if the answer is already no.  The cell count is decided from one reading at boot and held for the boot, and every per-cell voltage after it is the pack voltage divided by it; a percentage carries neither a unit nor the count it was divided by, so a wrong one is indistinguishable from a right one at the far end of a mesh.  While the pack voltage stays inside the band its classification implies, `pct=` carries a number; outside it the board publishes no percentage (`pct=none`, and no battery sensor in the telemetry report at all) and emits this line once.  The band's ceiling is the OCV curve's own 100 % point carried one step of its top segment further (4.33 V per cell), so the guard and the percentage cannot disagree about what a cell is.  Its floor is deliberately BELOW the curve's floor, at the 2.5 V per cell protection cut-off: between 2.5 and 3.0 V a pack is nearly empty, which is a real state that must report 0 % rather than go quiet exactly when the battery is about to give out.  The voltage is never withheld and the classification is never revised — this declines to build on a boot-time decision, it does not re-take it.  Critical rather than drain-gated, like the `[BAT] init` line: one line per transition is rare, and a field board on battery has no host to open the gate on the run where the gap appears. |
| `[NAME ]` | `leviculum-nrf/src/name.rs` | What this board is called on each of its two display surfaces: `mesh=<name> ble=<name> src=derived\|flash`.  `mesh=` is the display name the LXMF announces carry (what Columba lists), `ble=` the GAP/advertised name a scanner sees, and `src=` whether both come off the flash page or from the names derived from the identity (`LNode-<hex8>` and `LN-<hex8>`, which are different strings, not a truncation of one another).  The two differ when an operator's name is longer than the BLE bound of 11 bytes — visibly, which is the point.  Emitted once per boot beside the `[MEDIA]` banner, then re-emitted with `[FW_BUILD]` (same cadence), so a capture says under which names the board is visible without an operator having to remember what they set.  Set over the control envelope with `lnflash --set-name` (`docs/src/firmware/usb-control-envelope.md`, NODE_NAME 0x0C). |
| `SD_RAM_FLOOR` | `leviculum-nrf/src/ble/mod.rs` | One line per boot, before `Softdevice::enable`.  `wanted=` is the app RAM base the S140 says this BLE configuration needs, `floor=` is `ORIGIN(RETAINED)` from `memory.x` (the SoftDevice's ceiling — the retained cross-boot records and then the flip-link stack sit above it), `margin=` their signed difference.  `fits=0` never appears — the boot panics instead. |
| `ADV` | `leviculum-nrf/src/ble/columba.rs` | One line per boot when the advertising payloads are built.  `adv_bytes=`/`scan_bytes=` are the built PDU sizes against `cap=31`, `peripheral_only=` is the v0.3.0 capability bit, `periph_links=` the number of incoming link slots the payloads serve (#372), and `free_slots=` how many of them the boot advertisement offers — the live count the record carries in capability bits 1-3 (#375), which is why the advertisement is rebuilt at every advertising start rather than once here.  Emitted on the critical log path (like `SD_RAM_FLOOR`): it fires before the host's DTR-assert opens the runtime drain, and the gated path would silently drop it. |
| `BLE_SCAN_DECISION` | `leviculum-nrf/src/ble/columba.rs` | The scanner saw a Columba peer and applied the v2.2 address sort with the v0.3.0 capability override.  `addr=` is the peer's current address as 12 hex digits, `caps_record=` whether a readable capability record was present (`caps=` is meaningless when 0), `free_slots=` the DECODED free incoming-slot count the peer advertised or the token `unknown` when it advertised none (#375 — `unknown` is not `0`: a peer that says nothing is preferred as if all its slots were free, so reading it as zero inverts the conclusion), `rule=` the decision rule that fired, `initiate=` whether this side dials.  Emitted once per (address, decision) change, not per PDU.  The count orders the collection window (most free slots first, then lowest address) and nothing else — no admission or refusal reads it. |
| `BLE_CENTRAL_*`, `BLE_LINK_SELF`, `BLE_LINK_DUP` | `leviculum-nrf/src/ble/columba.rs` | Central-role connection lifecycle: `BLE_CENTRAL_ADDR`/`CONNECT`/`FAIL`/`UP`/`DOWN`, plus the two admission decisions.  `BLE_LINK_SELF addr=<a> action=disconnect` — the peer presented our own identity.  `BLE_LINK_EXPIRE role=peripheral\|central slot=<n> conn=<h> silence_ms=<n>` — the board's link expiry (#382): this link delivered nothing at all, payload AND keepalives, for `LINK_TIMEOUT_MS` (45 s), and the session disconnected it.  It is the board's counterpart of lnsd's `BLE_LINK_DOWN … reason=timeout` and the one mechanism that clears a link nobody dials; `silence_ms` says how far past the bound it ran.  `BLE_HANDSHAKE_TIMEOUT role=peripheral slot=<n> conn=<h> waited_ms=<n>` — the pre-handshake bound (#548): the connection never wrote its 16-byte identity within `HANDSHAKE_TIMEOUT_MS` (30 s, the constant lnsd expires its pending handshakes on), and the session disconnected it; keepalives do not extend it.  `BLE_PRE_HANDSHAKE_REFUSE role=peripheral slot=<n> conn=<h> len=<n> action=disconnect` — a payload write arrived before the identity; it is dropped, never forwarded, and the connection is closed, as Columba's GATT server closes it.  Running total is `unidentified=` on `BLE_COUNTERS`.  `BLE_LINK_DUP peer=<hex8> addr=<a> action=refuse rule=<r> origin=incoming\|outgoing old_conn=<h> new_conn=<h> old_mtu=<n> new_mtu=<n> old_silence_ms=<n> old_data_silence_ms=<n\|never>` — that identity already holds a link the decision kept, so the newcomer is dropped (#360 round 2).  `BLE_LINK_REPLACED peer=<hex8> addr=<a> rule=<r> origin=incoming\|outgoing old_conn=<h> new_conn=<h> old_mtu=<n> new_mtu=<n> old_silence_ms=<n> old_data_silence_ms=<n\|never> moved=<n> dropped=<n>` — the NEWER connection took the peer over and the old link was disconnected by us at the decision; `moved=`/`dropped=` are the packets carried over from the old link's queue.  One rule behind both lines and `rule=` says which branch decided: `abandoned` — the old link delivered nothing at all, keepalives included, for `LINK_ABANDONED_MS` (30 s, two keepalive intervals), so its peer has walked away from it; `same_role` — both connections carry the same role (a rotated address dialled twice in one direction), so the peer has no role preference to copy and we decide alone, keeping our own old dial or the peer's new one; `columba_mtu` / `columba_identity` — `preferred_ble_role`, the port of Columba's `preferredBleRole`, evaluated from the PEER's perspective, deciding on the usable-MTU comparison or on its identity tie-break.  `old_mtu=`/`new_mtu=` are the usable MTUs as compared, in the peer's ledger (a connection the peer has not bookkept reads `MIN_USABLE_MTU`, 20).  `old_silence_ms=` (any frame, keepalives included) is the abandonment test's input; `old_data_silence_ms` was round 1's input and is now reported only — `never` for a link that carried no payload.  A link that has really stopped answering entirely is cleared by `BLE_LINK_EXPIRE`, dial or no dial.  A refusal also emits `BLE_DIAL_DEAD_END addr=<a> reason=dup_refused ttl_s=<n>`: the address is backed off, or the scanner re-offers it within seconds.  Running totals are `refused=` and `displaced=` on `BLE_COUNTERS`; beside a rotating Columba phone `displaced=` climbs about once per ~90 s rotation and `refused=` stays near zero. |
| `[LORA] RX` | `leviculum-nrf/src/lora.rs`; fields read by `leviculum_core::packet::peek_wire_class`, offsets host-tested in `packet.rs` | One line per reassembled reception: `RX <n> bytes rssi=<dBm> snr=<dB> flags=0x<hh> dst=<hex8> ctx=0x<hh>`.  `flags=` is the packet's Reticulum header flags byte (its low two bits are the packet type) and `dst=` the first 8 hex digits of the destination hash — the two keys `lnflash --summarize` classifies announce, data, path request and proof from.  `ctx=` is the **context byte**, and it is the only field that separates the two announce shapes a relay hands down: a relayed announce reads `flags=0x51 ctx=0x00`, a path response carrying the same announce reads `flags=0x51 ctx=0x0b` (`PacketContext::PathResponse`) — identical `flags=`, identical `dst=`, and the second is the one `Transport::handle_announce` keeps out of the announce table by design.  Without `ctx=` a capture cannot tell "the board should have passed this up" from "by design it did not".  A packet too short to carry the header its flags claim (flags, hops, the destination its header-type bit implies, and the context byte after it) keeps the bare `RX <n> bytes rssi= snr=` shape.  lnsd's RNode interface emits the same `rssi=`/`snr=` keys on its `LORA_RX iface=… len=…` trace event and its `RX … bytes from radio` line, paired to the data frame the way the reference interface pairs them: the firmware indicates `CMD_STAT_RSSI`/`CMD_STAT_SNR` immediately before each data frame, so the last-seen stat values are that frame's own report (keys absent until the first stat frame). |
| `LORA_RX_FRAME` | `leviculum-nrf/src/lora.rs` (`note_frame_decoded`, called from the frame sink for every frame handed up, the teardown harvests included), every board on either radio; line rendered by `leviculum-channel-access`'s `rx_census::RxFrameLine`, byte-pinned by its host tests | **One line per frame the radio decoded (Codeberg #466).**  `LORA_RX_FRAME ok=1 len=<n> rssi=<dBm> snr=<dB> ph=<hex8>\|none`.  `len=` is the frame as the radio reported it, split header byte included (so it is the `len=` a failed frame's `LORA_RX_ERR` carries, not `[LORA] RX`'s packet length); `rssi=`/`snr=` are the chip's packet status.  `ph=` is the first 4 bytes of the hash of the packet this frame completed, a prefix of the 16-hex `ph=` every journey line carries, so one grep finds the sender's `PKT_TX` and every board that heard the frame; `ph=none` is a split part that completed nothing yet.  Unlike `[LORA] RX`, which prints once per reassembled packet, this is per frame, and it is the half of the witness reading that says "aired, and decoded here".  Gated, so a board without a reader prints nothing and skips the hash. |
| `LORA_RX_ERR` | `leviculum-nrf/src/lora.rs` (`note_window_failed`, in `rx_window`'s error arms), every board on either radio; reason mapped by `leviculum-channel-access`'s `rx_census::rx_err_reason`, line rendered by its `RxErrLine`, both pinned by its host tests | **One line per frame the radio heard and did not decode (Codeberg #466).**  `LORA_RX_ERR reason=crc\|header\|timeout\|other-0x<hhhh> rssi=<dBm>\|none snr=<dB>\|none len=<n>\|none`.  The reason is read off the IRQ flags the window latched, in the SX126x's 16-bit positions (`leviculum_core::sx126x::IRQ_*`; the LR11xx driver maps its word onto them, `leviculum_lr11xx::cmd::irq::to_sx126x`): `crc` is `RxDone` with `CrcErr`, the header decoded and the payload did not; `header` is `HeaderErr` (on the SX1262 it is latched and not routed, `DIO1_RX`, so it is found when the window ends for another reason; on the LR11xx it ends the window, `DIO_RX`); `timeout` is a window whose timer ran out on a latched preamble or valid header that never became a frame, false preamble detections included; `other-0x<hhhh>` is any other driver error with the flag word it latched.  Only `crc` carries figures: its header passed its own CRC, so `len=` is what the sender keyed, and the packet status was latched at the end of a reception; a header error or a bare carrier ended no reception, so `none` rather than the last good frame's numbers.  A corrupt frame at -27 dBm is a transmit-quality finding, one at -110 is range.  A window that timed out on nothing is silence and prints nothing.  Not covered: a frame that fails while a transmit or CAD stands the window down, which the `SX_RX_HARVEST`/`SX_TX_DEFER outcome=abandoned` lines account for (a silent witness never transmits).  The `[LORA] RX err: ...` line beside it is unchanged. |
| `[LORA_CHTM]` (board) | `leviculum-nrf/src/chtm.rs`; ledger in `leviculum-channel-access`'s `load`, counters in its `rx_census::RxCensus`, both host-tested | The board's once-a-second channel line: `airtime_short=<pct> airtime_long=<pct> channel_load_short=<pct> channel_load_long=<pct> current_rssi=<dBm>\|none noise_floor=<dBm>\|none interference=<dBm>\|none rx_ok=<n> rx_crc=<n> rx_hdr=<n> rx_tmo=<n> rx_other=<n>`.  The first four keys are the host's `LORA_CHTM` line's, so one reader takes either.  The `rx_*` keys (Codeberg #466) count `LORA_RX_FRAME` and the four `LORA_RX_ERR` reasons, cumulative since boot and wrapping, so a long capture is summed from its last line without the per-frame lines, which the gate drops before a reader attaches.  Volatile: kept out of the persistent crash tail. |
| `LINK_CLOSED` | `leviculum-nrf/src/events.rs`, from the core's `NodeEvent::LinkClosed`; line rendered by `leviculum-log-line`'s `LinkClosedBody`, byte-pinned by its host tests | One line per link this board's node ended or lost (867): `LINK_CLOSED reason=<kebab> link=<hex8> dest=<hex8> initiator=<0\|1>`.  `reason=` is the core's `LinkCloseReason::kebab`: `peer` for a close the other end sent, `normal` for our own, `timeout`, `stale`, `invalid-proof`, `channel-exhausted`, `blackholed`.  Until 867 a terminus whose peer closed the link printed nothing, and 858 and 866 had to read the close out of packet sizes. |
| `MOTION_INIT` | `leviculum-nrf/src/motion.rs`, T1000-E only; line rendered by `leviculum-motion`'s `InitLine`, byte-pinned by its host tests | **Whether the QMA6100P accelerometer answered (#457).**  `MOTION_INIT ok src=qma6100p`, or `MOTION_INIT absent src=qma6100p reason=no-answer\|chip-id-0x<hh>\|bus-error\|readback-0x<reg>-0x<val>`, once at boot about 100 ms in.  `absent` means the task has released the bus and the part's rail and returned, so the board runs exactly as it did before the driver.  Critical rather than drain-gated: it is written long before a host can attach. |
| `BTN_PRESS` | `leviculum-nrf/firmware/src/button.rs` (the pin in `leviculum-nrf/src/button.rs`), every board (Pocket V2, T1000-E, T114, Solar Node); rule in `leviculum-announce-policy`'s `ButtonGate`, pinned by its host tests | **A gesture on the user button and what it did (#459, 889; the gestures are [One button](concepts/one-button.md)).**  `BTN_PRESS board=<id> action=status\|announce\|rate-limited\|debounced\|long\|hold8 held_ms=<n> t=<ms>`, critical, one line per gesture; the `t=` is the formatter's, the call site writes none.  `status`: a short press (under 2 s) with no second press starting within 400 ms of its release, written when that window closes, so 400 ms after the release; it sends nothing, wakes the Pocket V2's display and plays the battery (`FEEDBACK pattern=battery-<n>`).  `announce`: a double press (a second press starting within 400 ms of a short one's release) outside the 30 s window, written at the second release, `held_ms` the second press; the `[ANNOUNCE] sent ... reason=button` (delivery, clock-gated) and `reason=pn-button` (propagation role) lines follow, then `reason=position-button` when the board shares its position and has a fix (1316), or `[ANNOUNCE] deferred reason=duty-budget occasion=button wait_ms=<n>` when the carrier's budget does not allow the set yet and the cadence's own tick sends it; then the `[TELEMETRY] report=armed\|skipped ... trigger=button` line says what became of the press's position report (914).  `rate-limited`: a double press inside the window, dropped.  `debounced`: edges that settled released, not a press.  `long`: held 2 s or more and under 8 s, written at the release once the contact has been quiet for 50 ms; does nothing yet (it becomes the mode switch).  `hold8`: held 8 s or more, written the same way; the board switches off (order 1511): `POWER_OFF` follows, then System OFF, and a press wakes it.  Every gesture but `debounced` lights the Pocket V2's display for a minute.  The bring-up line is `[BTN] init pin=P<p>.<nn> pull=up\|down active=low\|high debounce=50ms`; a board without a button says `[BTN] none` (only the bare RAK4631 module now). |
| `BTN_HOLD` | `leviculum-nrf/firmware/src/button.rs`, every board; rule in `ButtonGate` | **A press still held has reached a threshold (889).**  `BTN_HOLD board=<id> ms=2000\|8000 t=<ms>`, critical, written at the moment the button has been down 2 s and again at 8 s, while it is still held, so the rig can see when the long press and the very long press become available.  At 2000 the board plays `FEEDBACK pattern=release-now`, at 8000 `FEEDBACK pattern=power-off` (order 1511).  The gesture itself is the `BTN_PRESS` line at the release (`long` or `hold8`).  A press whose release began before the threshold writes none. |
| `FEEDBACK` | `leviculum-nrf/src/feedback.rs`, every board; the patterns and timings in `leviculum-feedback` (`leviculum-nrf/feedback/src/lib.rs`), pinned by its host tests | **A feedback pattern started on one sink (892; the grammar is [One button](concepts/one-button.md)).**  `FEEDBACK pattern=<name> sink=led\|notify\|buzzer t=<ms>`, gated (not critical), one line per pattern a player starts, including one that replaces a pattern still playing.  `pattern=`: `battery-1` to `battery-4` (a short press, the battery in quarters), `none` (a short press on a board with no battery percentage plays nothing and writes `FEEDBACK pattern=none reason=no-percentage sink=led t=<ms>` instead, so a quiet board reads as quiet by rule, not as a hook that did not fire; rule in `leviculum_feedback::status`), `error` (the flicker, defined, played by nothing since 893), `release-now` (a held button reaching 2 s, beside `BTN_HOLD ms=2000`), `notify` (the propagation role stored a message for this board's own `lxmf.delivery` destination, after its `PN_ACCEPT ... dup=0`), `report-delivered` (two short on `led` and `buzzer`: the proof of a report a double press sent, after its `[TELEMETRY] proof` line, 914; a proof of any other report plays nothing, and no proof plays nothing), `announce-sent` (one short and one blip on `led` and `buzzer`: the last LoRa packet of a double press's announces was keyed, after its `BTN_PRESS action=announce` and `[ANNOUNCE] sent` lines, 930; a press whose last announce is dropped, deferred or refused plays nothing, and with LoRa off it plays at the dispatch), `power-off` (one long, one short, one blip on `led` and `buzzer`: a held button reaching 8 s, beside `BTN_HOLD ms=8000`; the board switches off at the release, 1511), `reset-warning` (three seconds of flicker on `led` only, critical, written by the boot-time factory reset before the players exist: the button has been held through the boot for 10 s, and the wipe follows the flicker unless the button is let go, 1511), `mode-tracker` and `mode-mesh` (defined, played by nothing yet).  `sink=`: `led` (every board), `notify` (the Pocket V2's blue LED2 and the Solar Node's blue LED; elsewhere `notify` plays on `led`), `buzzer` (the T1000-E, beside its `led` line).  A board writes no line for a sink it does not have.  The rig reads the pattern name, not the light. |
| `POWER_OFF` | `leviculum-nrf/src/button.rs`, the switch in `leviculum-nrf/src/power.rs`, every board with a button | **The board is switching off (order 1511, [One button](concepts/one-button.md)).**  `POWER_OFF board=<id> wake=button pin=P<p>.<nn> t=<ms>`, critical, written at the release of a `hold8` press, about 1.5 s before System OFF (the `power-off` figure is let finish first).  Nothing follows it on this boot: the board's off pins go to their off level (the T114's panel rail, backlight and VEXT, the Pocket V2's 3V3-S rail and LoRa front-end power, the LEDs, the T1000-E's buzzer enable), the button is armed as the only wake source, and the chip enters System OFF.  The next line from the board is a boot, with `WAKE reason=button`. |
| `WAKE` | `leviculum-nrf/src/power.rs`; the decode in `leviculum-boot-trace` (`ResetReasons::wake`), pinned by its host tests | **Why this boot happened, in one word (order 1511).**  `WAKE reason=button\|usb\|power\|reset t=<ms>`, critical, once per boot beside `[RESET_REASON]`.  `button`: woken from System OFF by the button (RESETREAS `off`); `usb`: woken from System OFF by a USB cable (`vbus`); `power`: no bit latched, a power-on or a brownout; `reset`: any other cause, a flash, a commanded reset, the reset pin, the watchdog. |
| `FACTORY_RESET` | `leviculum-nrf/src/power.rs`; the hold rule in `leviculum-announce-policy` (`BootHold`), pinned by its host tests | **The button was held through the boot (order 1511).**  Written once in the boot banner, only on a boot that found the button pressed.  `FACTORY_RESET done=1 pages=identity,radio,settings t=<ms>`: held 10 s, through the `reset-warning` flicker, and the identity, radio profile and settings pages were erased; the board boots with a new identity, which announces on the ordinary boot schedule.  `FACTORY_RESET done=0 reason=released held_ms=<n>`: let go before the wipe (the press that woke a switched-off board ends here too).  `FACTORY_RESET done=0 reason=erase-failed`: the wipe was due and an erase failed. |
| `MOTION` | `leviculum-nrf/src/motion.rs`, T1000-E only; rule and line in `leviculum-motion`'s `MotionDetector` and `Transition`, pinned by its host tests | **Whether the board is moving (#457, step one).**  `MOTION state=moving\|still hold_ms=<n> src=qma6100p`, only on a change.  `moving` on the first sample (four a second) that differs from the one before by more than 30 mg summed over the axes; `hold_ms=` is how long the board had been in the state it left.  `still` once no sample has done so for 30 s; `hold_ms=` is that quiet time.  The first line after boot is either, since the detector starts unknown.  Each edge also goes to the GNSS task, which sleeps the receiver on `still` and wakes it on `moving` (step two, `GNSS_POWER` below); the #401 announce rule reads nothing of it ([Announce cadence](concepts/announce-cadence.md)).  `MOTION_READ state=failed\|ok src=qma6100p` marks the edges of a part that stops answering after a good init. |
| `MOTION_STAT` | `leviculum-nrf/src/motion.rs`, T1000-E only; window and line in `leviculum-motion`'s `MotionDetector::stat_due` and `Stat`, pinned by its host tests | **The detector's noise floor, every 30 s whatever the state (881).**  `MOTION_STAT samples=<n> max_delta=<counts> over=<n> state=moving\|still\|unknown quiet_ms=<ms> src=qma6100p`, drain-gated like the routine `[BAT]` lines.  Over the window since the previous line: `samples=` successful reads (120 at four a second; fewer means failed reads), `max_delta=` the largest change between successive samples in LSB summed over the axes (the threshold is 122, 30 mg at 2 g), `over=` how many samples exceeded it.  `state=` is the detector's state at the end of the window and `quiet_ms=` the time since the last sample over the threshold.  It tells a board stuck at `moving` because a sample over the threshold recurs inside every 30 s hold (`over=` non-zero, `quiet_ms=` under 30 000) from one whose `still` edge was due and not printed (`over=0`, `quiet_ms=` past 30 000, `state=moving`).  It carries `src=` because `MOTION_INIT` is written about 0.1 s after boot and the T1000-E's debug CDC comes up about 13 s later, so a rig capture never holds the init line; no `MOTION_STAT` at all after 30 s means the part did not answer or the drain gate is still shut. |
| `GNSS_POWER` | `leviculum-nrf/src/gnss.rs`, T1000-E only; decision in `leviculum-gnss-presence`'s `PowerGate`, line in its `PowerEdge`, both pinned by its host tests | **The GNSS receiver sleeps while the board is still (#457, step two).**  `GNSS_POWER state=sleep reason=still t=<ms>` and `GNSS_POWER state=wake reason=moving t=<ms>`, critical, only on a change; `t=` is uptime.  Sleep follows a `MOTION state=still` edge at once, except before the first fix: until the receiver has fixed or `GNSS_FIRST_FIX_GRACE_S` (600 s) has passed since boot, `still` keeps it awake, so a board that boots and lies still prints its sleep line at the first fix or at about `t=600000`.  Sleep is `$PAIR650,0` (Backup mode) and the power enable low with VRTC held up; wake is the enable high, a 20 ms pulse on the RTC wake line and the `$PAIR` setup again, so a wake is followed by `[GNSS_INIT] step=pair-probe` and the rest of the boot sequence.  Between the two lines the `[GNSS] bytes=` heartbeat stops, and `[GNSS_PRESENCE]` and the fix hold their last values.  No line at all on a board without the accelerometer, or whose part said `MOTION_INIT absent`: its receiver stays awake. |
| `ANNOUNCE_LEARNED_NOT_RELAYED` | `leviculum-nrf/src/events.rs`, from the core's `NodeEvent::AnnounceLearnedNotRelayed`; line rendered by `leviculum-log-line`'s `AnnounceLearnedNotRelayedBody`, byte-pinned by its host tests | **Not a loss — read the name literally.**  `ANNOUNCE_LEARNED_NOT_RELAYED closed=<r> discovery=none\|open\|expired dest=<hex8>`.  An announce reached this node, was validated, and updated its path table; the node then had no route to pass it on.  Nothing was discarded, so no drop bucket moves and nothing in `PKT_DROP_SUMMARY` will ever account for it — which is exactly why the line exists, because a route that was never taken otherwise leaves no trace at all.  It matters most on a **board**: a board registers no shared-instance local client, so `handle_announce`'s local-client forward cannot run there and the announce table is the only general route to its serial host (`leviculum-core/src/node/mvr_board_announce_uplink.rs`).  `closed=` says which term of the announce-table gate closed — `path_response` (the announce carried `PacketContext::PATH_RESPONSE`, which the table excludes by design, Python `Transport.py:1886`), `rate_blocked`, or `rate_limited`.  The event fires only on a node that relays announces at all (`enable_transport`, or an announce from a local client): on a node with transport off, "learned and not relayed" is the configured steady state of every announce it hears, and a line per reception would say nothing.  `discovery=` reports the discovery TABLE at the moment the announce arrived, not the history of who asked: a pending request is reaped on the first tick past `DISCOVERY_TIMEOUT_MS`, so a genuinely late path answer reads `none`, and **`none` does not distinguish "nobody asked" from "somebody asked and the record is gone"**.  `expired` is the narrow case of an announce that beat the reaper.  A duplicate announce inside the rate window never reaches this line at all — it leaves `handle_announce` at the earlier rate-limited return, which is a counted drop.  Off the boards the same event is a `crate::tracing::debug!` with `event="ANNOUNCE_LEARNED_NOT_RELAYED"`.  On a daemon the emission is additionally gated on having **no shared-instance client attached** (`has_local_clients` — with one attached, the local-client forward is where such an announce goes), so the per-hour rate tracks client uptime, which is binary; a run-to-run rate shift of orders of magnitude says a client sat on the old run and not on the new one, not that relaying changed (981: 0.3/h against 4 600/h across two miauhaus soaks with the gate and its inputs textually unchanged). |
| `PKT_RELAY` | `leviculum-nrf/src/events.rs`, from the core's `NodeEvent::RelayDecided`; line rendered by `leviculum-log-line`'s `RelayDecidedBody`, byte-pinned by its host tests | What this board did with ONE packet a neighbour addressed to it for relay (#346).  `PKT_RELAY outcome=forwarded\|no-path\|duplicate\|forward-max-hops ph=<hex16> dst=<hex8> hops=<n> iface_out=<n>\|none`.  Off the boards the same decision is already readable as the journey events `PKT_FORWARD`, `PKT_DROP` and `DEDUP_DROP`; on a board none of them exists, because the firmware builds leviculum-core without `tracing` and every `debug!`/`trace!` in the core is a no-op there (`leviculum-core/src/lib.rs:83-100`).  Until this line the only transport-layer account a board could give was the periodic `[TRANSPORT]` counter line, which says how many packets were forwarded or dropped in the last 30 s and never which — chasing #344 cost a 5.5 h receiver log, a three-port millisecond capture and packet-length arithmetic to establish something the board could have said in one line.  `ph=` is the FULL 8-byte journey correlator, byte-identical to the `ph=` on a peer's `lnsd` journey events, so the two logs stitch on one id; `dst=` is the first 4 bytes of the destination hash, a prefix of the host events' 16-byte `dst=`.  **Scope is the ADDRESSED relay path only, and the absence of a line is itself a reading.**  A packet whose transport header names another node produces nothing: on a shared medium a relay hears every packet routed via its neighbours, and an event per reception is the 99 %-noise problem the counter-only overheard path exists to avoid (`drops_overheard_transport_id` still accounts for it, and `[TRANSPORT] overheard=` still prints it).  Relayed announces and broadcasts are addressed to nobody and are likewise not reported here.  So "a peer says it sent this packet and no `PKT_RELAY` names it" means the board either never heard it or was never named as its next hop — not that the relay swallowed it. |
| `[DROP]` | `leviculum-nrf/src/events.rs`, from the core's `NodeEvent::PacketDropped`; line rendered by `leviculum-log-line`'s `PacketDroppedBody`, byte-pinned by its host tests; rate limit in `leviculum-nrf/drop-budget` | One packet this board heard and threw away, with the taxonomy's own reason (#346) and — since #421 — the packet's name.  `[DROP] reason=<kebab> ph=<hex16>\|none dst=<hex8> iface=<n> ht=<1\|2>[ transport_id=<hex8>]`.  `ht=` is the header type and `transport_id=` the first 4 bytes of a HEADER_2 packet's transport id, absent on HEADER_1 (860): `overheard-transport-id` is recorded from three sites, and these two fields say which one fired and which neighbour the packet was addressed to.  Since 860 the link layer's own refusals are lines too: `link-not-active`, `link-decrypt-fail`, `rtt-invalid`, `request-no-handler`, `channel-decrypt-fail`, and `link-data-no-link` for a link that vanished under a handler.  Since 867 a resource advertisement a link refuses with an RCL is one too: `resource-full` when the link already carries its incoming resources, `resource-compressed` when the board cannot decompress it.  The complement of `PKT_RELAY`, never a second copy: the core's two event sites are disjoint, so one dropped packet is one line, on one of the two.  `ph=` is the FULL 8-byte journey correlator, byte-identical to the `ph=` on a peer's `lnsd` journey events and on `PKT_RELAY`, carried wherever the deciding site already held a hash — the link-request, link-echo, addressed-data and node-layer (decrypt-miss / dead-link, #421's new coverage) drops.  `ph=none` names the two sites that decide without ever hashing: the HEADER_2 overheard gate (whose 99 %-volume path deliberately computes no per-packet SHA-256) and the path-request refusals; there `dst=` plus `t=` remain the correlator.  So “did the relay forward my report, and if not, why” is one grep for the 16-hex `ph` value over a board capture and the sender's log: it returns the sender's `PKT_TX`, the relay's `PKT_RELAY` or `[DROP]`, and nothing at all means the board never heard it — the answer #344 needed a 5.5 h receiver log, a three-port capture and packet-length arithmetic to reconstruct.  (That grep is also what a periculum cell would assert on; the parser is deliberately not part of #421.)  Rate-limited to `LINES_PER_WINDOW` per `WINDOW_MS` window, 2 per 2 s since 860 (`leviculum-nrf/drop-budget`, derivation pinned by `the_ring_derivation_still_holds`); a clipped window closes with `[DROP] suppressed=<n> window_ms=<w>`, so a storm and a trickle never read the same. |
| `ANN_TX` | `leviculum-nrf/src/events.rs`, from the core's `NodeEvent::AnnounceTransmitted`; line rendered by `leviculum-log-line`'s `AnnounceTransmittedBody`, byte-pinned by its host tests | Why this board just talked (#405).  `ANN_TX occasion=transit\|local\|uncapped\|path-response\|reoffer dst=<hex8> hops=<n> iface=<n>\|all`, one line per announce transmission.  Since #402 the board registers an airtime cap on its LoRa interface, whose holdoff allows two or three transit announces in a window where a capture may show fifteen announce-sized transmissions — and every kind of announce transmission used to leave the same trace, so none of them could be attributed.  `occasion=transit` is a relayed announce the cap passed (immediately, or out of the cap's queue once the holdoff expired); `local` one this board originated, which bypasses the cap by design and whose rate is the announce policy's, never the cap's (`hops=0` on the same line says so); `uncapped` a relayed announce on an interface carrying no cap at all — the board's BLE and serial interfaces beside a capped LoRa one, and the reading to expect if a PHY was never registered; `path-response` an answer somebody asked for, which is requested rather than propagated and does not mean this board relays announces at all.  `reoffer` (#383) is a stored announce handed to one peer whose first link on a multi-peer interface just came up — the announce a relay ladder that retired before the link existed would otherwise never deliver; it goes on that peer's link alone and is paced by the #402 cap where one is registered.  **Transmissions only.**  An announce the cap held back or dropped emits nothing here — that is `ANN_TX_SUPPRESSED`, which exists only in the core's tracing, and the boards have no tracing; silence on this line is not evidence a board stayed quiet.  The board's own `[ANNOUNCE] sent reason=` line is the other half and stays as it is: it names the occasion of the announces this board ORIGINATES (`peer-up`, `periodic`, `host`), at the moment the policy decides, where this line is one per transmission at the moment it leaves.  Off the boards the same statement is the host event `ANN_TX`, which carries the same `occasion=` beside `dst=<hex32> hops= iface=<name>`.  The core logs none for an announce it sends on every interface, so `lnpnd` writes one for its own node announce, `occasion=start\|periodic\|host hops=0 iface=all` (1294): the two start announces, the interval, and one an operator or the control verb asked for. |
| `[TELEMETRY] send` | `leviculum-nrf/src/telemetry.rs` | **Frozen shape — the #365 proofs read this.** One line per telemetry report attempt, emitted at the moment of the routing decision: `send dst=<hex8> via=<iface-name> next_hop=<hex8\|direct> online=<y\|n>`.  "Sent to a live carrier" is this line (`online=y`) followed by the `report target=…` line once the dispatch settles; "not sent" is `report withheld target=<hex8> reason=<r>` instead, where `reason=no-path` means no path entry at all and `reason=iface-offline` means an entry exists but its interface is offline (carrier off, or a peerless BLE domain) — both are followed by a path request on the carriers still online.  lnsd states the same decision for **every** originated packet as the host events `OUTBOUND_ROUTE dst= iface= next_hop= online=` and `OUTBOUND_WITHHELD dst= iface= next_hop= reason=` (schema-validated via `EVENT_CATALOG`, asserted in `leviculum-std/tests/obs_outbound_route_events.rs`). |
| `[TELEMETRY] report` | `leviculum-nrf/src/telemetry.rs`; the trigger and the press rule in `leviculum-nrf/telemetry-policy` (`ReportTrigger`, `SendPolicy::note_button_press`), pinned by its host tests | **A report that went out, and what a double press did about one (914).**  `[TELEMETRY] report target=<hex8> reason=immediate\|movement\|heartbeat trigger=cadence\|target-set\|position-set\|request\|button position=0\|1 possrc=fixed\|gnss unix=<s> src=<time-source>`, critical, written once the dispatch settles the report as sent.  `reason=` is the rule of the policy's poll that fired; `trigger=` is who asked: `cadence` for movement and heartbeat, and for `reason=immediate` the occasion that armed it, `target-set` (a target set with its key, or its key arrived), `position-set` (the fixed position set or cleared), `request` (the target's `TELEMETRY_REQUEST`), `button` (a double press).  One report answering several occasions is named `button` if a press is among them, since the press's proof is the one the board plays (`FEEDBACK pattern=report-delivered`).  A double press writes, after its `BTN_PRESS action=announce` and the announce lines, `[TELEMETRY] report=armed trigger=button wait_ms=<n>` (armed; the next telemetry tick past the attempt floor sends it, `wait_ms=0` when the floor is clear) or `[TELEMETRY] report=skipped reason=no-recipient\|awaiting-key\|no-position-source trigger=button` (nothing armed: no target, a target still resolving its key, or no position source).  Firmware only. |
| `[TELEMETRY] proof` / `path dropped, asking again` / `retry` / `gave up` | `leviculum-nrf/src/telemetry.rs` | **Frozen shape — the #373 proofs read these.** The proof wait of a sent report (state machine host-tested in `leviculum-nrf/telemetry-policy`): the transport tracks a receipt per single packet, and a report that gets no proof within the receipt timeout (`leviculum-core/src/transport.rs` `compute_receipt_timeout`) is retransmitted ONCE with the identical payload — same position, same time — then given up.  `[TELEMETRY] proof pkt=<hex8> after=<ms>` on a verified proof (`after=` measured from the first send, whether the first send or the retransmission landed); `[TELEMETRY] path dropped, asking again dst=<hex8> pkt=<hex8>` when the proof failed to come while a path was held: the path is dropped and asked for once before the retry (#344, the reference's `drop_path` then `request_path`, `reference/LXMF/LXMF/LXMRouter.py:2743-2751`), and the retry waits for the answer, so a capture tells a rediscovered retry from a plain one; `[TELEMETRY] retry pkt=<hex8> reason=no-proof` at the moment the retransmission goes out; `[TELEMETRY] gave up pkt=<hex8> after=<ms>` after the second loss, and the next scheduled report carries on.  `pkt=` is always the FIRST send's packet hash, so the two or three lines of one report correlate on one id.  The retry goes through the ordinary send path (an offline path is re-resolved, the interface applies its airtime rules) and moves no cadence: `min_interval_ms` for the next report still counts from the original attempt.  Firmware only — lnsd has no telemetry sender. |
| `[TELEMETRY] discarded` | `leviculum-nrf/src/telemetry.rs` | What arrived at the board's `lxmf.delivery` destination and could not be kept. A board announces that destination because a receiver verifies its reports against the key the announce carries, but it has no inbox, no message store and no links: the reporter keeps a Sideband telemetry request and discards everything else. `[TELEMETRY] discarded from=<hex8> len=<n> reason=no-inbox` is a real LXMF message thrown away — somebody wrote to this board and nobody will ever read it; `[TELEMETRY] discarded len=<n> reason=not-a-message` is bytes encrypted to that hash that are no LXMF message at all.  The destination proves NOTHING (`ProofStrategy::None`, via `LxmfNode::delivery_destination_without_inbox`), so neither case is confirmed to its sender; the line exists because the version that dropped these in silence let a board mark a peer's message DELIVERED and then throw it away. Firmware only. |
| `BOOT_TRACE` | `leviculum-nrf/firmware/src/boot_trace.rs` (shape host-tested in `leviculum-nrf/boot-trace`) | One line per boot, first thing in the boot banner: what the PREVIOUS boot's breadcrumb record in retained RAM (`.retained`, a `memory.x` region the Adafruit bootloader provably never touches — its original `.uninit` home sat under the bootloader's stack and was wiped on every reset) says.  `BOOT_TRACE prev_magic=ok\|absent prev_phase=<milestone> prev_boot=<n> reset_reason=<hex> prev_lora=<step>`.  `prev_phase` is the last boot milestone the previous boot COMPLETED (`enter-main`, `persist-read`, `usb-up`, `lora-task`/`lora-skipped`, `sd-enabled`, `ble-task`, `main-loop`) — a healthy reboot reads `main-loop`; anything earlier says the previous boot HUNG right after that milestone, which is the whole point: a boot that dies before USB enumerates is otherwise invisible (ledger local-pocket-dark-d66209e).  Three more name the step `ble::init` was ENTERING when it stopped: `sd-probe` (the SoftDevice RAM probe), `sd-settle` (the blocking delay after it), `sd-enable` (`Softdevice::enable`); they sit between `lora-task` and `sd-enabled` (#469, order 1056, after two Pocket boots hung there).  `prev_lora` is a byte of its own, because the LoRa task runs on an interrupt executor beside `main` and both writing one byte would keep the last writer rather than the one that hung: the radio bring-up step the task was entering (`lora-reset`, `lora-busy-wait`, `lora-standby`, `lora-setup`, `lora-calib`, `lora-calib-image`, `lora-init`, `lora-config`, then `lora-ready` once the task enters its loop), `none` when the task never touched the radio (LoRa off, or the boot stopped before the task first ran), `absent` with `prev_magic=absent`.  It is appended after `reset_reason` so readers of the four older keys are unaffected.  `prev_magic=absent` (with `prev_phase=absent prev_boot=0`) is the honest first-boot-after-power-loss answer, never a fabricated phase; `prev_phase=unknown-0x<byte>` marks a record left by an image with a different phase table.  `reset_reason` is raw `POWER.RESETREAS` at entry to `main` (cleared after the read so each boot reports only its own cause); the decoded per-bit view follows on the `[RESET_REASON]` line, now on both boards. |
| `LORA_TX_MUTED` / `LORA_TX_UNMUTED` | `leviculum-nrf/src/lora.rs` (`admit_for_transmit`); lines rendered by `leviculum-log-line`'s `lora_tx_muted` / `lora_tx_unmuted`, byte-pinned by its host tests | **Whether this board's transmitter is switched off (#410).**  The host's `radio_silent` flag drops every outgoing LoRa frame at the driver boundary while the receiver keeps running, so a muted board has a live receive loop, `[MEDIA] lora=on`, a climbing transport counter and no `[LORA] TX` — and used to have no line saying why either: the frame is swallowed before the acquisition jitter, the CAD, the CSMA verdict and the airtime lock, each of which logs.  `LORA_TX_MUTED packets=<n> bytes=<n> reason=host-silent` on the first frame of a run and at each decade after it (the 1st, 10th, 100th …), for `MEDIA_TX_DROP`'s reason — every line also writes the 2 KiB post-crash tail and a muted board drops one frame per announce; `LORA_TX_UNMUTED packets=<n> bytes=<n>` closes the run with its exact totals when a frame reaches the radio again.  The companion fact is `silent=` on the `[LORA] active config:` line, which states the flag at the moment a host sets or clears it.  `radio_silent` is deliberately not persisted (`leviculum_core::radio_config_store`), so a reset always ends a mute — which is why the mute is not visible in a capture taken after one. |
| `LORA_QUEUE_DROP` | `leviculum-nrf/src/lora.rs` (the hold branch of `lora_task`); rule and its derivation in `leviculum-nrf/queue-budget` (`HOLD_MAX_AGE_MS`, `hold_verdict`), lines rendered by `leviculum-log-line`'s `lora_queue_drop_stale` / `lora_queue_drop_suppressed`, both byte-pinned by their host tests | **A frame the regulatory duty lock aged out instead of keying (#433).**  Read the companion fact first: a duty-cycle hold does not thin traffic, it ages it — FIFO under a hold means each dip of the ledger below the cap admits one frame and re-pins the lock, so under sustained load the queue drains at the cap's rate with its order intact and everything reaching the air is as old as the standing backlog (measured at 145.6 s on the Pocket V2, 2026-09-27).  `LORA_QUEUE_DROP reason=stale age_ms=<n> bytes=<n> total=<n>` says the interface threw such a frame away where it waited: `age_ms=` is the wait since `LoRaInterface::try_send` accepted it, `total=` the count since boot.  Only while `[LORA_AIRTIME_LOCK] … holding` is the reason for the wait — a frame held by CSMA, by an acquisition-jitter draw or behind a burst gap is keyed whatever its age.  Type-blind: an age and a length, never a packet kind, because the interface reads neither.  Rate-limited by `leviculum-drop-budget` exactly like the core's `[DROP]` lines, so a purge cannot evict the `[STACK]`/`[TRANSPORT]`/panic lines a capture was taken for; `LORA_QUEUE_DROP suppressed=<n> window_ms=<n>` names what a clipped window held back.  The running total is also the `lora_stale=` field on the periodic `[TRANSPORT]` line — the one field there that is not a core counter, because an interface-level loss has none by design.  Why the rule exists and where its 18 s comes from: `docs/src/concepts/regulatory-airtime.md`. |
| `LORA_TX_STALE` | `leviculum-std/src/interfaces/rnode.rs` (`count_stale_drop`, reached from `pop_live_frame` and `pop_live_vport_frame`); rule in `leviculum-nrf/queue-budget` (`HOLD_MAX_AGE_MS`, `DutyHolds`) | **The lnsd twin of `LORA_QUEUE_DROP reason=stale` (#433).**  `LORA_TX_STALE iface=<name> age_ms=<n> len=<n> held_ms=<n> ph=<hex>` says the RNode interface's host-side send queue threw a frame away at dequeue instead of handing it to the modem: `age_ms=` is the wait since the interface accepted it, `len=` the payload length, `held_ms=` how much of that wait overlapped a duty hold, `ph=` the packet's journey correlator as the core tagged it (`-` when the journey target was off; T-0013).  The host cannot read the modem's airtime lock, so a duty hold here is what the lock looks like from the serial line: the CMD_READY gate held shut with frames waiting for longer than a full frame's airtime plus one re-query interval explains.  Only a frame past `HOLD_MAX_AGE_MS` (18 s) whose wait overlapped such a hold is dropped; a frame old for any other reason is handed over.  Type-blind: an age, a length and an opaque tag, never a packet kind.  Counted as `tx_stale_drops` on the interface's `interface_stats` row and in `tx_queue_drops` (it is a loss); `lnstatus` renders it as `TX stale`.  WARN level, one line per drop. |
| `BOOT_COUNT` | `leviculum-nrf/src/boot_count.rs` (layout, wrap and line host-tested in `leviculum-nrf/boot-count`) | **How often this board restarted while nobody was watching (#380).**  One line per boot, beside `BOOT_TRACE` and `[RESET_REASON]`: `BOOT_COUNT n=<n> reset_reason=0x<hex> retained=0\|1 since_erase=<n>`.  `n=` is the boot number, and it is the one number here that survives a POWER loss — `BOOT_TRACE`'s `prev_boot=` comes out of retained RAM and restarts at 1 whenever the supply goes away, which is exactly the case a field board produces (the 90 minute walk that motivated this reported `reset_reason=0x00000000 prev_magic=absent` on every one of its boots).  `retained=` is that same RAM's verdict carried onto this line, so the pair separates a power loss (`retained=0` with `reset_reason=0x00000000`) from a watchdog or a commanded reset.  The count lives in one page of internal flash, `0xD9000` on all three boards — `memory.x`'s `BOOT` region, directly below the record store — one 16-byte record appended per boot; `since_erase=` is how many of the page's 256 slots are spent, so it counts up to 256 and the boot that finds the page full erases it and carries `n=` forward — a restart of `since_erase=` with `n=` unbroken is that erase and not a lost count.  Ungated like `[MEDIA]` and `[QSPI]`: a board on battery in a field has no host to open the drain gate.  `BOOT_COUNT_WRITE_FAILED n=<n>` follows the line when the flash refused the write — the boot number is still stated, it simply will not be there next time. |
| `[TIME_RESTORE]` | `leviculum-nrf/src/time_floor.rs` (ring, gate and lines host-tested in `leviculum-nrf/time-floor`) | **The learned calendar coming back after a power cycle (#166 item 4).**  Once per boot, ungated, beside `[TIME_SOURCE]`: `[TIME_RESTORE] value=<unix> seq=<n>` when the newest valid record of the `TIME` checkpoint ring (`0xD7000`, two pages) was seated by `restore_emission_floor`, which also turns `[TIME_SOURCE]` to `source=restored-floor`; `[TIME_RESTORE_REFUSED] value=<unix> seq=<n> reason=seam` when the seam refused it (outside the sanity window, or not above the build floor plus uptime), `value=0 seq=0 reason=empty` when the ring holds no readable record.  The restored value is a floor at the birth rank, so the first credible announce still heals the calendar in one step.  At runtime `[TIME_CHECKPOINT] value=<unix> seq=<n> erased=<page>\|-` says a record was appended (at most hourly, only while the calendar ranks 4 or better and has advanced an hour since the last record), `[TIME_CHECKPOINT_FAILED] value=<unix> seq=<n>` that the flash refused it; the next attempt is an hour later. |

### Host-side BLE events

lnsd's Columba BLE interface (`leviculum-std/src/interfaces/ble/`)
emits `BLE_SCAN_DECISION` **with the same fields as the firmware's
line of the same name** — `addr=`, `caps_record=`, `caps=`,
`free_slots=`, `rule=`, `initiate=` — so a merged rig timeline shows both sides of one
mutual sighting deciding, and the two `rule=` values must be
complementary (one `initiate…`, one `wait…`).  A sighting the
decision discards is `BLE_SCAN_DROPPED iface=<name> addr=<a>
reason=<r> hint=<hex8|unknown> rssi=<dBm>` at DEBUG, rate-limited like
`BLE_SCAN_DECISION` (once per address and drop change, and again after
the address was decided in between; a drop never makes an unchanged
decision line print twice): `reason=no_service_uuid` is a device that lists no
Columba service and carries no Columba record, and
`reason=record_without_service_uuid` one that carries our record while
BlueZ's stored `UUIDs` lack the service, the #443 signature of a
device object poisoned by an earlier connection (577).  A dial that
gave up is booked in the per-address backoff both stacks share
(`leviculum_ble_tx::DialBackoff`, 579) and says so as `BLE_DIAL_BACKOFF
iface=<name> addr=<a> failure=transport|not_columba|no_handshake
strikes=<n> backoff_ms=<n> until_ms=<n>`: a transport failure (connect
failed, setup timed out) pauses the address a flat 5 s, a failure that
names the peer (no Columba GATT layout, the identity write refused, or
#548's pre-handshake timeout on a connection it made to us) doubles its
pause from 10 s to the 120 s cap; a handshake in either role, or an
absence from the sightings longer than the cap, ends the run.  While
the hold stands, a sighting the verdict would dial prints, once per
hold, `BLE_SCAN_DECISION addr=<a> hint=<hex8|unknown> initiate=0
rule=backoff until_ms=<n> in_ms=<n> strikes=<n>`; `until_ms` is on the
interface's own clock (milliseconds since it came up).  The firmware
prints both lines with the same keys, on its uptime clock.  Three
lnsd lines record what ended a dial, so a dial census counts events
rather than debug prose (758): `BLE_SETUP_TIMEOUT iface=<name>
addr=<a> after_ms=20000` is a central setup (connect, discovery,
identity read, subscribe, handshake) that outlived its budget;
`BLE_CONNECT_UNANSWERED iface=<name> addr=<a> after_ms=5000` is a
connect nothing answered within the connect bound (732), the
signature of a peer that went dark after the sighting that chose it;
both book as `failure=transport`.  `BLE_LINK_REJECT iface=<name>
peer=<hex8> addr=<a> reason=full origin=incoming|outgoing` is an
identity handshake refused because the link table had no slot for
it; the other refusals keep their own names (`BLE_LINK_SELF`,
`BLE_LINK_DUP`, `BLE_LINK_NOT_ADMITTED`).  Its link lifecycle is
`BLE_LINK_UP` / `BLE_LINK_DOWN` (with `role=central|peripheral`,
`peer=<hex8>`, the same hex the firmware logs and the `LN-<hex8>`
name carries, and lnsd's own `reason=timeout|displaced|disconnected|adapter_lost`,
`leviculum-std/src/interfaces/ble/mod.rs` `log_link_down`; the board's
`BLE_LINK_DOWN` shares the name and the `role=`/`peer=` keys but
carries `conn=` and the SoftDevice's HCI reason instead, see its row
in the firmware table), the admission decisions are the firmware's
`BLE_LINK_SELF` / `BLE_LINK_DUP` / `BLE_LINK_REPLACED` names — lnsd's
lines carry `rule=`, `origin=incoming|outgoing`, `old_mtu=`/`new_mtu=`,
`old_silence_ms=` and the reported `old_data_silence_ms=` exactly as
the firmware's do (#360 round 2), plus `old_role=` on a replacement —
and one lnsd has no firmware counterpart for:
`BLE_LINK_NOT_ADMITTED peer=<hex8> identity=<hex32> addr=<a>
role=peripheral listed=<n> action=disconnect` — the `accept_only`
config key refused this incoming link at its identity handshake, so
the peer never became a link.  `identity=` carries the whole hash
beside `peer=`'s four bytes because both are spellings the key
accepts, and `listed=` is how many peers the list names, so a capture
shows that a list is in force without the config beside it.  The line
exists so a measuring host that turned strangers away does not read
like a host nobody tried; its dialling-side sibling is
`BLE_DIAL_NOT_ALLOWED`, which fires when `initiate_only` is what
stopped a dial.  Two more are lnsd's alone, because a board's radio
cannot vanish from under its firmware: `BLE_ADAPTER_LOST iface=<name>
hci=<hciN> links=<n> error=<text>` (WARN) — the adapter's D-Bus object
is gone (bluetoothd restarted, the dongle re-enumerated; 600), seen
either as bluetoothd's `InterfacesRemoved` for it (`error=InterfacesRemoved`)
or as a scan window's discovery failing on the missing object while a
read of the adapter fails too (`error=` is that failure); every link it held goes
down as `BLE_LINK_DOWN reason=adapter_lost`, and the interface
re-resolves the adapter by its bring-up rule once per
`discovery_interval`; `BLE_ADAPTER_BACK iface=<name> hci=<hciN>
down_ms=<n> forgot=<n>` is the session that found it again, advertising
and scanning anew.  `forgot=` counts the peers' stored device objects it
removed before either role started: bluetoothd restores a connected
peer's object, cached GATT database included, with the adapter, and a
dial at the restored object finds no Columba layout, so the removal the
link-down path owes (#443) is paid here.  The churn penalty (#417) is the firmware's
`BLE_CHURN_LEDGER` with the bare token spelled `state=`:
`BLE_CHURN_LEDGER state=armed identity=<hex8> run=<n>
reason=expiry_central|expiry_peripheral|replaced window_ms=<n>
cooldown_ms=<n>` when a rotating-addressed identity's run of
departures (lnsd's `BLE_LINK_DOWN reason=timeout` or a `BLE_LINK_REPLACED`)
closes the rotating fallback class, and `BLE_CHURN_LEDGER state=held
run=<n> until_ms=<n> now_ms=<n>` once per cooldown when that closed a
candidate out of the collection window.  Same ledger, same constants
(`ChurnPolicy::MEASURED`), no A/B switch on lnsd.  A fan-out drop on a congested
link is `BLE_TX_FANOUT_DROP`.  The delivery-hint decision
is the firmware's `BLE_TX_ROUTE` / `BLE_TX_FLOOD` /
`BLE_TX_ROUTE_MISS` (#376), with `conn=central|peripheral` naming the
role of the link rather than a SoftDevice handle; routing at a
peripheral-role peer still reaches the other subscribed centrals,
because BlueZ fans one notification out to every subscriber and the
Columba service has a single notify characteristic.  A reassembly discarded
before completion is `BLE_RX_ABANDON` (#373) with `iface=`,
`peer=<hex8>`, `lost=` (packets this frame cost) and `total=` (the
link's running count) — the firmware's line of the same name is
slot-keyed instead of identity-keyed, everything else matches.
There is no lnsd counterpart of the firmware's `BLE_CONN_PARAMS`
(#385), and the gap is in BlueZ rather than in this interface: the
D-Bus `Device1` interface publishes no connection interval, slave
latency or supervision timeout — `bluer`'s device properties (`bluer`
0.17.4, `src/device.rs`) run from `Name` and `Rssi` to `ServicesResolved`
and `BatteryPercentage` and contain none of the three — and neither does
sysfs, whose per-connection directory (`/sys/class/bluetooth/hci<n>:<handle>`)
carries only `uevent`.  The values exist one layer down, in the LE
Connection Complete and LE Connection Update Complete events on the HCI
transport, reachable only through an HCI monitor socket (what `btmon`
reads) with the privileges that implies.  So on a bench with an lnsd
central the parameters are read from the capture or the central's kernel,
as #385 did; on a link to a phone the board's line is the only source,
which is why the firmware half exists.  The same blindness applies to
the answer lnsd gives: when a board asks its lnsd central for a longer
supervision timeout (`BLE_CONN_PARAMS_REQ`), whether the kernel granted
it is readable from the board's `when=close` line or from an HCI
capture, and from nothing lnsd itself logs.

These are host events, so
unlike the firmware's they do appear in `EVENT_CATALOG` and are
schema-validated.

### Reading "was the radio listening at instant X"

`[SX_RX_ARM]` is emitted at the `SetRx` that arms the SX1262, once
per receive window:

```text
[SX_RX_ARM] site=idle timeout_ms=0 dark_ms=3 t=123456
```

- `t=` is the instant the receiver went live.  It is *not* derivable
  from the window's completion line: `[T114_LORA_LOOP] op=rx_*
  duration_ms=` brackets the whole `receive()` call, IRQ setup and
  buffer readout included, so `t - duration_ms` lands before the
  arming, not on it.
- `dark_ms` is the gap back to the previous window's end, computed on
  the board.  The boot arm has no previous window and says
  `dark_ms=first` rather than a digit.
- `site` is which of the loop's six listening windows this is —
  `idle`, `ack`, `csma`, `jitter`, `hold`, `yield` — because their
  timeouts overlap and the length alone does not identify them.

The two together close the span: window *n* was listening from its
own `t=` until `t(n+1) - dark_ms(n+1)`.  Everything outside those
spans is standby, including CAD and TX.  So the last window in a
capture has no closing instant — its successor is what supplies it.

The line is written through `log_fmt`, so a board with nothing
attached to the debug CDC (`RUNTIME_DRAIN_OPEN == false`) never
formats it.

### Reading a `LINK_DIED` age

`LINK_DIED` comes from the core (`link_management.rs`, target
`leviculum_core::link`) and carries its age in milliseconds, to be
read against the `threshold_ms` on the same line:

- `elapsed_since_activity_ms` is the time since the link's last
  inbound packet, and reads `none` for a link that never had one —
  every handshake that did not complete.  There the expected number
  does not exist, and the raw subtraction would print the process
  uptime in its place: the field base of 2026-09-27 reported 91
  responder culls as `elapsed_since_activity_ms=9581430` beside
  `threshold_ms=85824`, for culls that were 72 to 90 s old and
  therefore on time (#354).
- `since_request_ms` appears on `detail=handshake_timeout` and is the
  age of the handshake itself, measured from the request — the
  initiator's send, the responder's proof.  That is the clock
  `establishment_timeout_ms()` and hence `threshold_ms` is measured
  on, so those two are the pair to compare: a handshake culled on
  time shows a difference of one tick.

## Validation behaviour

Two violation classes, both non-blocking — the original event
line is never suppressed.

### Schema violation (per-handle)

`EVENT_SCHEMA_VIOLATION event=<NAME> missing=[a,b] caller=file:line t=<ms>`

Emitted when a catalogued event misses required keys at emission.
Each active handle's catalogue lookup chains the production
`EVENT_CATALOG` with the handle's own `extra_schemas`, so
test-only schemas don't pollute the production catalogue.

### Field-value violation (per-event)

`EVENT_FIELD_VIOLATION event=<NAME> field=<key> value_problem=<kind> caller=file:line t=<ms>`

Emitted when a field's stringified value contains ASCII
whitespace, `=`, or non-printable characters.  Such values break
the whitespace-tokenised parser used by Stage-7's
`jl --filter <key>=<value>` filter.  `<kind>` is one of
`whitespace`, `equals`, `non_printable`.

The fix at the call site is to pick a value form that doesn't
need escaping — substitute `_` for spaces, drop `=` from value
strings, etc.  The original event line is still emitted; the
tester sees the violation alongside, treats it as a bug.

### User-named values: render them at the emission site

A value that can carry text a user chose — an interface name from
the config file (`[[TCP Uplink]]`) or from discovery
(`autoconnect/Dark Doodad 23`), a filesystem path, an instance
name — is not a source bug when it contains a space: it is
legitimate input that the EMISSION SITE has to render as a single
token.  Wrap it in `leviculum_std::event_log::Scalar` (or, inside
`leviculum-core`, `event_scalar::Scalar`; the interface-name
formatters `IfaceName` / `IfaceNameOpt` already do it for every
`iface = %…` field):

```rust
tracing::debug!(event = "BLE_LINK_UP", iface = %Scalar(&self.name), …);
```

The sink still rescues an unwrapped value (`sanitize_scalar`), and
for the handful of fields it can recognise as names by key
(`iface`, `iface_in`, `iface_out`, `in_iface`, `out_iface`) it does
so without raising a violation.  That list cannot be completed
from the sink side — `next_hop` carries an interface name at one
site and a hash at another — so a name wrapped at the site is the
only form that is correct at every field.  Substitution, not
quoting: `jl`, `jldiff` and every `awk`/`grep` one-liner split on
whitespace, so a quoted value with a space is still several tokens
to all of them.

## Multi-process workflow

Spawned subprocesses (e.g. an `lnsd` child of an integration
test) emit to a per-process file when given two env vars:

```sh
LEVICULUM_EVENT_LOG=/tmp/leviculum-events-<pid>.log \
LEVICULUM_EVENT_NODE=node-a \
    ./lnsd ...
```

- `LEVICULUM_EVENT_LOG=<path>` — child appends each event line
  (and any field-violations) to `<path>` as it emits.  When
  unset, the subscriber writes only to the in-memory buffer used
  for panic-dump.
- `LEVICULUM_EVENT_NODE=<name>` — supplies the `node=` value.

After the children exit, the parent merges all per-process files:

```rust
use leviculum_std::test_support::event_log::merge_event_logs;
let merged: Vec<String> = merge_event_logs(&[
    PathBuf::from("/tmp/leviculum-events-12345.log"),
    PathBuf::from("/tmp/leviculum-events-12346.log"),
]);
```

`merge_event_logs` reads every input, parses the trailing `t=<n>`
token of each line, and returns the union sorted by `t` (stable
on tie).  Lines without parseable `t=` sort to the end with
their relative order preserved.

Per-process clock note: `t=` values are millisecond offsets from
each subscriber's local init time, not a shared wall clock.
Merged ordering is monotone across the union but doesn't directly
say which real-world event came first across hosts.  For
wall-clock correlation add a per-emission timestamp field to the
catalogue (`ts=<unix-ms>`) and sort on that instead.

### Production-daemon integration

`lnsd` honours both env vars at startup via
`leviculum_std::test_support::event_log::install_global_subscriber()`.
When `LEVICULUM_EVENT_LOG` is unset, the install path is
functionally equivalent to the previous
`tracing_subscriber::fmt().init()` call — no event-log layer is
built, so runtime overhead is whatever the fmt layer would
otherwise impose.

`rnsd` is Python-side (reference/Reticulum); structured event
capture for it is out of scope for the current Rust-side work.

## See also

- Codeberg #39 piece 1 (this document's spec).
- `leviculum-std/src/test_support/event_log.rs` (implementation +
  catalogue).
- `leviculum-std/src/test_support/tracing_setup.rs` (Registry
  composition + Once-guard).
- `leviculum-std/tests/event_log_subscriber.rs` (unit tests).
- `leviculum-std/tests/event_log_multiprocess.rs` (multi-process
  merge integration test).
- Stage 7: `jl` / `jldiff` filter tools that consume this format.
