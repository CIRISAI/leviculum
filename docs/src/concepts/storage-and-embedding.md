# Storage and Embedding

`reticulum-core` is `#![no_std]` with only `alloc`
(`reticulum-core/src/lib.rs:59`). It contains no I/O, no clock, no
filesystem, and no async runtime. That is what lets the *exact same*
protocol code run on a Linux daemon, a future Android app, and a
bare-metal nRF52 firmware image. The bridge to the outside world is a
small set of traits the core depends on but does not implement.

## Three injected dependencies

The core declares its platform needs as traits in
`reticulum-core/src/traits.rs` and takes implementations from the
driver:

- **`Clock`** (`traits.rs:162`) — supplies `now_ms()`. The core never
  calls a system clock; time is handed in. On the host this is wall
  time; on the nRF52 it is the Embassy timer
  (`reticulum-nrf/src/clock.rs:8`).
- **`Storage`** (`traits.rs:196`) — supplies persistence and lookup
  for every collection the protocol maintains. `flush()` defaults to a
  no-op (`traits.rs:466`) so a RAM-only backend needs to implement
  nothing extra.
- **`Interface`** (`traits.rs:97`) — supplies framing and the wire (see
  [Interface Isolation](interface-isolation.md) and the
  [Interface trait](../architecture.md#interface-trait)).

Randomness is injected the same way, as an explicit
`rng: &mut impl CryptoRngCore` parameter rather than a global
(`reticulum-core/src/lib.rs`, "Platform Dependencies").

## The Storage trait

Rather than a generic key/value blob store, `Storage` exposes
**type-safe methods grouped by collection** — packet-dedup hashes, the
path table, the reverse table, link/announce tables, receipts, and
ratchets — with typed entries from `storage_types.rs`. The full method
inventory is tabulated in
[Architecture](../architecture.md#storage-trait).

This shape was a deliberate decision. The deep analysis of every
method — who calls it, how often, and whether it matters on an
embedded target — is in
[Storage Trait Split Analysis](../storage-trait-analysis.md). Read that
page before changing the trait surface.

## Three backends, one core

The same `NodeCore` is parameterised over its `Storage`
implementation, so embedding is a matter of choosing a backend
(`reticulum-core/src/node/mod.rs:143`,
`NodeCore<R: CryptoRngCore, C: Clock, S: Storage>`):

| Backend | Where | Behaviour |
|---------|-------|-----------|
| `NoStorage` | tiny / stateless | no-op |
| `MemoryStorage` | host / tests | `BTreeMap`, RAM only (inner store of `FileStorage`) |
| `EmbeddedStorage` | embedded (nRF52) | `heapless::FnvIndexMap`, fixed capacity, no allocator for maps |
| `FileStorage` | host (`reticulum-std`) | wraps `MemoryStorage` + disk |

`FileStorage` persists only what must survive a restart — known
destinations, the packet dedup hashlist, and ratchet keys — and keeps
the rest (paths, reverses, links, announces, receipts) in RAM,
rebuilt from the network on restart. The file formats and flush
strategy are in
[Architecture](../architecture.md#filestorage-persistence).

## What the split buys you

- **Host vs. embedded from one source tree.** `reticulum-std` builds a
  tokio driver around the core; `reticulum-nrf` builds an Embassy
  driver around the *same* core
  (`reticulum-nrf/src/bin/t114.rs`,
  `reticulum-nrf/src/bin/rak4631.rs`, both `#![no_std]` and both
  constructing the core via `NodeCoreBuilder`).
- **Testability.** Because time and storage are injected, the core is
  driven deterministically in tests — feed bytes and a fixed clock,
  drain the `TickOutput`, assert. This is the basis of the
  minimal-reproducer tests under `reticulum-std/tests/mvr/`.
- **No host concerns in the core.** Backpressure, airtime budgeting,
  and serial queueing live host-side in `reticulum-std` and never leak
  into the `no_std` core (`reticulum-std/src/interfaces/airtime.rs:1`).

See [Architecture](../architecture.md) for the sans-IO core diagram and
the driver event loop that pumps these traits.
