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
  (`no-path`, `plain-group-multihop`, `forward-max-hops`,
  `same-interface-relay`, ...).
- `same-interface-relay` is the one drop reason that reports a
  DELIBERATE non-forward: on a shared medium a relay's only outbound
  interface can be the one the packet arrived on, and putting it back on
  that air is suppressed on purpose.  It is still where the packet died,
  and a journey that simply stopped at the relay's `PKT_RX` could not be
  told apart from a lost log line, so the suppression emits a `PKT_DROP`
  with the arrival interface as `iface_in`.  Seeing it is not an error.
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

2. **Add a catalogue entry** in
   `leviculum-std/src/test_support/event_log.rs`'s `EVENT_CATALOG`:

   ```rust
   EventSchema {
       name: "FOO",
       required_keys: &["iface", "dst", "hops", "len"],
   },
   ```

   `required_keys` should list every field the call site sets.
   The subscriber checks that every catalogued event's emission
   includes all required keys; missing keys produce a
   `EVENT_SCHEMA_VIOLATION` line in the dumped buffer alongside
   the original event.

Catalogue entries without a live emitting site are explicitly
discouraged: the runtime-validation layer can't detect them, so
they silently rot.  Only add entries you have a corresponding
emit for.

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
