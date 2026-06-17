# Leviculum public API design

Status: living document. This is the design of record for the Leviculum
public API: a curated Rust consumer facade plus a thin C FFI on top of it.
The goal is that a developer consumes Leviculum exactly like a normal Unix
C library:

```sh
apt install leviculum-dev
cc app.c -lleviculum
```

```c
#include <leviculum.h>
```

The C constraints (opaque handles, no generics, no async, byte buffers,
flat enums) are the forcing function. They are applied first; the Rust
facade is whatever those constraints project back into clean Rust. KISS is
the highest virtue.

## Scope and non-goals

In scope for v1: instance lifecycle, identity, destinations and announce,
path and connect, link send and receive and close, datagram send,
request and response, resource transfer, event draining, error strings,
and hex/bytes helpers.

Out of scope for v1: LXMF, LXST, the shared-instance daemon, RPC, fault
injection, and the diagnostic/stats surface (`transport_stats`,
`path_table_entries`, `rate_table_entries`, `drop_all_paths_via`, ...).
These stay internal to `reticulum-std` and never enter the facade.

Binding constraints from project policy: additive changes only to
`reticulum-core` and `reticulum-std` (new module plus re-exports, never a
refactor of signatures `lnsd`/`lns` depend on); stay out of
`reticulum-std/src/interfaces/` and `reticulum-nrf`. The facade must not
break wire or semantic compatibility with Python-RNS peers; it only
re-projects an already battle-tested engine.

## Architecture in one picture

```
   C application (owns its event loop)
        |  leviculum.h  (extern "C", opaque handles, int codes)
   reticulum-ffi  (cdylib + staticlib)
        |  panic guard, runtime, eventfd bridge, buffer marshalling
   reticulum_std::api  (curated Rust facade, own stable types)
        |  thin re-projection, no new behaviour
   reticulum_std::driver::ReticulumNode  (existing engine, unchanged)
        |
   reticulum_core  (state machine, wire format, crypto)
```

The facade adds no behaviour. It selects ~10 app-relevant entry points out
of the ~40 on `ReticulumNode`, gives them stable types that hide core
internals (`DestinationHash`, `LinkId`, `transport::*`), and presents the
event stream in a shape the C layer can project mechanically. The C layer
adds only what C forces: a hidden runtime, a panic guard, a pollable event
fd, and buffer marshalling.

## 1. Object and handle model

Every C type is an opaque pointer with a `create`/`destroy` pair. The app
never sees a struct layout, so the ABI stays stable across versions.

| C handle | Wraps | Created by | Destroyed by |
| --- | --- | --- | --- |
| `leviculum_t` | the node, its runtime, the event bridge | `lev_builder_build` | `lev_free` |
| `lev_builder_t` | node configuration before build | `lev_builder_new` | consumed by build, or `lev_builder_free` |
| `lev_identity_t` | a key pair or public-only identity | `lev_identity_generate` / `lev_identity_from_*` | `lev_identity_free` |
| `lev_destination_t` | a local destination (in or out) | `lev_destination_new` | `lev_destination_free` |
| `lev_link_t` | one established or pending link | `lev_connect` / `lev_accept_link` | `lev_link_free` (also closes if open) |
| `lev_event_t` | one drained event | `lev_next_event` | `lev_event_free` |

Addresses are not handles. A destination hash and a link id are each a
fixed 16-byte value. They cross the boundary as `uint8_t[16]` (or as a hex
string through the helpers), never as an opaque pointer. This keeps the
common "I read an announce, now connect to it" path free of allocation.

`lev_builder_build` consumes the builder conceptually but does not free it;
the caller still calls `lev_builder_free` (or the build takes ownership and
nulls the caller's copy; see OPEN QUESTIONS). The default rule is: every
`_new`/`_generate`/`_from_*`/`_next_*` that returns a handle has a matching
`_free`, and `_free(NULL)` is a no-op.

## 2. Error handling

Classic Unix. Functions that can fail return `int`: `0` on success, a
negative `LEV_ERR_*` code on failure. Constructors that return a handle
return `NULL` on failure and set the thread-local last error.

```c
typedef enum {
    LEV_OK                 =  0,
    LEV_ERR_NULL_PTR       = -1,
    LEV_ERR_INVALID_ARG    = -2,
    LEV_ERR_BUFFER_TOO_SMALL = -3,
    LEV_ERR_NOT_RUNNING    = -4,   /* event loop down */
    LEV_ERR_IO             = -5,
    LEV_ERR_CONFIG         = -6,
    LEV_ERR_CRYPTO         = -7,
    LEV_ERR_NO_PATH        = -8,   /* path unknown, call lev_request_path */
    LEV_ERR_LINK           = -9,   /* link closed/inactive/handshake */
    LEV_ERR_SEND           = -10,  /* would-block/too-large/no-route */
    LEV_ERR_RESOURCE       = -11,
    LEV_ERR_REQUEST        = -12,
    LEV_ERR_TIMEOUT        = -13,
    LEV_ERR_AGAIN          = -14,  /* non-fatal busy/backpressure, retry */
    LEV_ERR_PANIC          = -127, /* caught panic, see no-panic rule */
} lev_error;
```

Two layers of detail:

- `const char *lev_strerror(int code)` returns a static, never-freed string
  for the code. Stable, allocation-free, safe to call any time.
- `const char *lev_last_error(void)` returns a thread-local string with the
  specific failure detail (for example which argument, which address). It
  is owned by the library, valid until the next failing call on the same
  thread, and must not be freed by the caller.

The codes are a flat projection of `reticulum_std::error::Error` plus the
FFI-only `NULL_PTR`, `BUFFER_TOO_SMALL`, `AGAIN`, and `PANIC`. No error
sum type, no `errno` global; the thread-local detail string is the only
mutable error state and it never escapes the thread.

`LEV_ERR_AGAIN` is deliberately distinct from hard errors: it maps the
link `try_send` "Busy"/"PacingDelay" case so an app can poll without
treating backpressure as failure, matching `EAGAIN`.

## 3. Memory ownership

The default is read(2) style: the caller owns the buffer, the library
fills it.

```c
int lev_identity_public_key(const lev_identity_t *id,
                            uint8_t *buf, size_t cap, size_t *out_len);
```

Contract for every such function:

- The caller passes `buf` and its capacity `cap`, plus `out_len`.
- On success the library writes the bytes and sets `*out_len` to the count.
- If `cap` is too small, the library writes nothing, sets `*out_len` to the
  required size, and returns `LEV_ERR_BUFFER_TOO_SMALL`. The caller resizes
  and retries. Passing `buf == NULL` to learn the size is allowed.

Fixed-size outputs (the 16-byte addresses, the 32/64-byte keys) document
their exact length as a constant so callers can stack-allocate:

```c
#define LEV_ADDR_LEN        16
#define LEV_IDENTITY_PRV_LEN 64
#define LEV_IDENTITY_PUB_LEN 64
#define LEV_SIGNATURE_LEN    64
```

Where a result has no caller-knowable bound, the library allocates and owns
it behind a handle that the caller frees. This is the case for events: an
event carries a variable payload (received datagram, link data, completed
resource), so `lev_event_t` is an owned handle and its payload is read out
through accessors into a caller buffer (read(2) style again) before
`lev_event_free`. No raw library-allocated pointer is ever handed to C for
the app to `free()`; freeing always goes through a typed `lev_*_free`. This
removes allocator-mismatch bugs (C `free` vs Rust `dealloc`).

## 4. The event and receive model

This is the central decision. The engine delivers everything inbound, link
data included (`LinkHandle` is send-only; see `driver/stream.rs`), as
`NodeEvent`s on a tokio channel via `EventReceiver`. C has no async and no
tokio. The model must let a C app compose Leviculum with its own event loop
on Linux now and on Android later.

Decision: a pollable event fd plus a drain call.

```c
int  lev_event_fd(const leviculum_t *node);          /* readable fd */
int  lev_next_event(leviculum_t *node, lev_event_t **out);  /* dequeue */
void lev_event_free(lev_event_t *ev);
```

Inside the library, a bridge task running on the hidden runtime owns the
`EventReceiver`, pushes each `NodeEvent` (projected to the facade `Event`)
onto an internal queue, and writes one byte to an `eventfd` (Linux) or the
write end of a `pipe` (portable fallback). The app treats `lev_event_fd` as
a normal readable descriptor:

```c
struct pollfd p = { .fd = lev_event_fd(node), .events = POLLIN };
poll(&p, 1, -1);
lev_event_t *ev;
while (lev_next_event(node, &ev) == LEV_OK && ev) {
    switch (lev_event_type(ev)) { ... }
    lev_event_free(ev);
}
```

`lev_next_event` returns `LEV_OK` with `*out == NULL` when the queue is
empty (drain it until that happens after each wake). The fd is level- or
edge-consistent with the queue: the bridge keeps the fd readable while the
queue is non-empty.

Why a pollable fd and not callbacks:

- The app keeps ownership of its loop. The library never calls into app
  code, so there is no re-entrancy hazard and no rule against calling
  library functions from inside a callback.
- Callbacks would fire on the library's tokio worker thread. Foreign-thread
  upcalls are exactly what language bindings (JNI in particular) handle
  worst; they force thread attach/detach and global-state locking onto the
  app.
- An fd composes with `select`/`poll`/`epoll` and with Android's
  `ALooper_addFd`, which takes any fd. The same code path serves both
  targets. No dependency on one event-loop technology, which the task
  requires.

For simple apps that do not want to run a loop, a convenience blocking
drain is offered as well, implemented on top of the same queue:

```c
int lev_wait_event(leviculum_t *node, lev_event_t **out, int timeout_ms);
```

Callbacks are explicitly the rejected fallback and are recorded as such.

Event payloads are read through typed accessors, never a transparent union
(unions over variable-length data are an ABI and lifetime hazard):

```c
lev_event_type_t lev_event_type(const lev_event_t *ev);
int lev_event_link_id(const lev_event_t *ev, uint8_t out[16]);
int lev_event_dest_hash(const lev_event_t *ev, uint8_t out[16]);
int lev_event_data(const lev_event_t *ev,
                   uint8_t *buf, size_t cap, size_t *out_len);
```

An accessor that does not apply to the event's type returns
`LEV_ERR_INVALID_ARG`. The facade `Event` enum collapses the ~30
`NodeEvent` variants to the v1-relevant set and drops internal fields
(`interface_index` raw values, observability-only `ChannelRetransmit`,
`ControlPlaneOverflow` is surfaced as a dedicated overflow event so loss
stays visible).

## 5. Threading

The tokio runtime is created and owned inside `leviculum_t` and never
exposed. `ReticulumNode` already builds its own single-worker runtime for
the event loop (`driver/mod.rs`); the FFI layer holds one additional
runtime handle to `block_on` the engine's public async methods (`connect`,
`send`, `announce`, `stop`). Each `extern "C"` call that wraps an async
method blocks the calling C thread until it completes.

Thread-safety guarantees, documented in the header:

- `leviculum_t` is thread-safe. Its methods may be called concurrently from
  multiple threads (the engine is `Arc<Mutex<..>>` internally). Sends and
  connects from different threads are serialized correctly.
- The event side is single-consumer. `lev_next_event` / `lev_wait_event` on
  one node must be called from one thread at a time. The fd may be polled
  from anywhere, but draining is not concurrent.
- `lev_identity_t` and `lev_destination_t` are not internally synchronized.
  Treat one handle as owned by one thread, or guard it externally. They are
  plain value objects, so the simple rule is "do not share a single handle
  mutably across threads".
- Every handle is safe to `*_free` exactly once; double free is the app's
  bug, `_free(NULL)` is a no-op.

## 6. No panic across the FFI boundary

Unwinding into C is undefined behaviour. Every `extern "C"` function wraps
its entire body in `std::panic::catch_unwind` and converts a caught panic
into `LEV_ERR_PANIC` (or `NULL` for constructors), recording a detail
string. A single macro enforces this so no function can forget it:

```rust
macro_rules! ffi_guard {
    ($default:expr, $body:block) => {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $body)) {
            Ok(v) => v,
            Err(_) => { set_last_error("panic in libleviculum"); $default }
        }
    };
}
```

`$default` is `LEV_ERR_PANIC` for `int`-returning functions and
`std::ptr::null_mut()` for constructors. The crate keeps `panic = unwind`
(never `abort`) so `catch_unwind` actually catches. A CI lint (a test that
greps the FFI source, plus review) checks that every `#[no_mangle] extern
"C"` body goes through the guard.

## 7. ABI and versioning

Semantic versioning, exposed both at compile time and run time:

```c
#define LEVICULUM_VERSION_MAJOR 0
#define LEVICULUM_VERSION_MINOR 1
#define LEVICULUM_VERSION_PATCH 0
const char *lev_version_string(void);   /* "0.1.0" */
uint32_t    lev_version_number(void);    /* (major<<16)|(minor<<8)|patch */
```

All structs are opaque, so adding fields never breaks the ABI. New
functionality is added as new functions; existing signatures are frozen
once shipped. SONAME plan: `libleviculum.so.0` for the 0.x series, bumping
the major soname only on a breaking ABI change. The versioned header
installs as `leviculum.h`; the macros let a consumer `#if` against the
version it built for.

While the crate version is `0.x`, the C ABI is explicitly unstable and the
header carries a note to that effect. The stability promise begins at 1.0.

## 8. Naming

One prefix on every symbol, because C has no namespaces.

- Functions: `lev_*` (`lev_connect`, `lev_announce`, `lev_identity_generate`).
- Constants and enum values: `LEV_*` (`LEV_OK`, `LEV_ADDR_LEN`,
  `LEV_EVENT_LINK_ESTABLISHED`).
- Version macros: `LEVICULUM_VERSION_*` (the brand spelled out).
- Types: the main handle is `leviculum_t`; all others are `lev_*_t`
  (`lev_identity_t`, `lev_link_t`, `lev_event_t`).

Rename recorded: the existing stub uses the `lns_` prefix and emits
`reticulum.h` and `libreticulum`/`leviculum_ffi`. v1 renames to the
`lev_` prefix, header `leviculum.h`, library `libleviculum.so`. The stub's
`lns_*` identity functions are reimplemented under the new names; no `lns_`
symbol survives into v1. The cdylib output name is set to `leviculum` so
the linker flag is `-lleviculum`.

## 9. Header generation

Decision: cbindgen generates `leviculum.h` from the Rust source on every
build, and the header is a build artifact, not committed (the repo already
gitignores `reticulum-ffi/*.h`). The Rust source is the single source of
truth; packaging installs the freshly generated header.

Rationale. The worst class of C-library bug is a header that disagrees with
the shipped symbols (wrong argument order, stale signature, a function that
no longer exists). Regenerating the header on every build from the same
crate that produces the symbols removes that class entirely and by
construction: there is no committed copy that can go stale. This is
strictly safer than committing the header with a CI no-diff guard, and it
matches the existing repo convention. cbindgen is already wired
(`build.rs`, `cbindgen.toml`); we invest in making the output read like a
hand-written header (doc comments carried over, `cbindgen.toml` controls
ordering and wrapping, opaque types as clean forward declarations).

The one cost is that someone browsing the repo does not see the header
without building. That is acceptable for a generated artifact. If a hand
curated header is ever wanted for ergonomics, the fallback is to commit it
and guard it with a symbol-match CI check (`nm` on the cdylib vs the
declarations). Recorded as the alternative, not chosen for v1.

## 10. The v1 surface

Kept deliberately tiny. Grouped by area; every line is one C function
unless noted.

Instance and version:
- `lev_builder_new`, `lev_builder_free`
- `lev_builder_identity`, `lev_builder_storage_path`,
  `lev_builder_add_tcp_client`, `lev_builder_add_tcp_server`,
  `lev_builder_add_udp`, `lev_builder_add_auto_interface`,
  `lev_builder_enable_transport`
- `lev_builder_build` -> `leviculum_t`
- `lev_start`, `lev_stop`, `lev_is_running`, `lev_free`
- `lev_version_string`, `lev_version_number`

Identity:
- `lev_identity_generate`, `lev_identity_from_private_key`,
  `lev_identity_from_public_key`, `lev_identity_free`
- `lev_identity_hash`, `lev_identity_public_key`,
  `lev_identity_private_key`, `lev_identity_has_private_keys`
- `lev_identity_load_file`, `lev_identity_save_file`

Destinations and announce:
- `lev_destination_new` (identity, direction, type, app_name, aspects),
  `lev_destination_free`
- `lev_destination_hash`
- `lev_register_destination`
- `lev_announce` (dest hash, optional app_data)

Path and connect:
- `lev_has_path`, `lev_request_path`, `lev_hops_to`
- `lev_connect` (dest hash + signing key) -> `lev_link_t`
- `lev_accept_link` (link id from a `LinkRequest` event) -> `lev_link_t`

Link send, receive, close:
- `lev_link_send`, `lev_link_try_send`
- `lev_link_id`, `lev_link_is_closed`
- `lev_link_identify`, `lev_link_remote_identity`
- `lev_close_link`, `lev_link_free`
- receive is via the event stream (`LinkDataReceived`, `MessageReceived`)

Datagram:
- `lev_send_datagram` (dest hash + bytes, single packet) -> packet hash
- delivery confirmation arrives as an event

Request and response:
- `lev_register_request_handler` (dest hash, path, policy)
- `lev_send_request` (link, path, optional data, timeout) -> request id
- `lev_send_response` (link, request id, data)
- request/response arrival and timeout arrive as events

Resource transfer:
- `lev_send_resource` (link, data, optional metadata, auto_compress)
  -> resource hash
- `lev_set_resource_strategy`, `lev_accept_resource`, `lev_reject_resource`
- progress and completion arrive as events

Events:
- `lev_event_fd`, `lev_next_event`, `lev_wait_event`, `lev_event_free`
- `lev_event_type` and typed accessors

Errors and helpers:
- `lev_strerror`, `lev_last_error`
- `lev_hex_encode`, `lev_hex_decode`

## 11. Packaging

Linux first, Android-aware in the design.

Linux v1:
- `crate-type = ["cdylib", "staticlib"]` (already set), output name
  `leviculum` so the link flag is `-lleviculum`.
- SONAME `libleviculum.so.0`, with the usual `libleviculum.so` ->
  `libleviculum.so.0` dev symlink.
- Install layout: `libleviculum.so*` in the lib dir, `leviculum.h` in the
  include dir, and a generated `leviculum.pc` pkg-config file so
  `pkg-config --cflags --libs leviculum` and the `apt install
  leviculum-dev` feel both work.
- A worked C example under `examples/c/` that links the real `.so` and does
  a live send/recv is the per-phase acceptance test.

Android later (designed for, not built in v1):
- Per-ABI `cdylib` via cargo-ndk (arm64-v8a, armeabi-v7a, x86_64).
- No Linux-only assumptions in the code: the event fd uses eventfd with a
  pipe fallback, both present on Android; the hidden runtime is tokio,
  which builds for Android; no `epoll`-specific API leaks into the public
  surface.

## Implementation phases

Test-first throughout. Each phase ends green on `cargo fmt`, `cargo clippy
--workspace -- -D warnings`, `cargo build`, the Rust facade tests, and the
C example link/run test for that phase.

- Phase a: facade skeleton + instance + identity + version + error
  plumbing + the `catch_unwind` guard harness. C example: create node,
  print version, generate identity, round-trip keys.
- Phase b: destinations + announce + the event fd and drain. C example:
  announce, observe an `AnnounceReceived` event from a peer.
- Phase c: path + connect + link send/recv/close. C example: connect to a
  peer, exchange link data.
- Phase d: datagram + request/response.
- Phase e: resource transfer.
- Phase f: packaging (pkg-config, SONAME, header install layout).

## The Rust facade

The facade is a new additive module `reticulum_std::api`, re-exported as
needed. It is the projection target of the C constraints: it exposes only
the v1 surface and gives it stable types that hide core internals.

Proposed types:
- `api::Node`, `api::NodeBuilder` (thin wrappers over `ReticulumNode` /
  `ReticulumNodeBuilder`, exposing only v1 methods).
- `api::Identity` (re-export of the core identity, already clean).
- `api::Address` for the 16-byte destination hash and link id, replacing
  the leaked `DestinationHash` / `LinkId` in the facade signatures with one
  stable byte-addressed newtype.
- `api::Event` (projection of `NodeEvent` to the v1 set, internal fields
  dropped).
- `api::Error` (already close to `reticulum_std::error::Error`; the facade
  re-exports it and the C codes map from it).

The facade adds no new behaviour and no new wire format. It is curation and
re-typing only, which keeps the additive-only and Python-compatibility
constraints trivially satisfied: everything underneath is unchanged.

## OPEN QUESTIONS

- Builder ownership across `lev_builder_build`: does build consume and free
  the builder (nulling the caller's pointer is not possible in C), or does
  the caller still own and free it after build? Leaning toward "build
  borrows, caller still frees" for the least surprising C ownership, with
  the builder reusable for a second node. To be settled when phase a lands.
- Event queue bound: the bridge queue needs a cap and a drop or overflow
  policy mirroring the engine's control/data split (control lossless, data
  droppable). Whether to surface a single `LEV_EVENT_OVERFLOW` or mirror
  the engine's `ControlPlaneOverflow` semantics is to be decided in phase b
  against a real flood test.
- Whether `lev_connect` should auto-request a path when none is known
  (convenience) or strictly return `LEV_ERR_NO_PATH` and leave path
  discovery to the app (explicit). Default for v1 is explicit; revisit if
  the C example proves it too sharp.
