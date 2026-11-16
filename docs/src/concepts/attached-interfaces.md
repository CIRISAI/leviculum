# Interfaces a host drives

Every built-in interface owns its carrier: the TCP client dials, the RNode
interface talks to its radio. Some hosts have to own the carrier
themselves. A browser holds the WebSocket. An app embeds the node and has
already accepted a connection. A sandboxed module may only reach the
network through the host's imports (`ciris:node/net`). For these cases
`leviculum-std` has two entry points on a running `ReticulumNode`:

- `attach_interface` registers an interface whose frames the host moves by
  hand.
- `attach_host_interface` runs a framed Reticulum interface over a
  connection the host supplies. The connection implements the
  `HostCarrier` trait.

Both are fork code (`driver/attach.rs`, `interfaces/host.rs`), and both work
with the existing tokio driver.

## `attach_interface`

```rust
pub fn attach_interface(&self, cfg: ExternalInterfaceConfig)
    -> Result<ExternalInterface, Error>;
```

`ExternalInterfaceConfig::new(name, kind)` sets the defaults. The public
fields are:

| Field | Meaning | Default |
|---|---|---|
| `name` | The name in stats and logs | (required) |
| `kind` | `InterfaceKind`: status grouping, and the resource window under `ByMedium` | (required) |
| `mode` | `InterfaceMode`: announce propagation rules | `Full` |
| `hw_mtu` | The hardware MTU signalled into link MTU negotiation. `None` signals nothing, so links stay at 500. At least 500 when set | `None` |
| `bitrate` | Registered for announce capping, like a configured `bitrate` key | `None` (uncapped) |
| `up` | Whether the carrier is up at attach | `true` |
| `ifac` | The `IfacConfig`, applied in both directions | `None` |
| `ingress_control` | Whether announce ingress control applies | `false` |
| `buffer_size` | Frames buffered in each direction | 256 |

The call returns `Error::NotRunning` before `start`, and `Error::Config`
for an empty name, an `hw_mtu` below 500, or a zero buffer. It takes the
run lock, not the core lock.

`ExternalInterface::into_parts()` gives three handles:

- **`ExternalInbound`** (`Clone`, `Send`, `Sync`) pushes received frames
  into the node. It has `try_deliver`, `deliver` (async, on any executor),
  `blocking_deliver` (from a thread outside a runtime) and `is_detached`. A
  frame is one Reticulum packet as it came off the carrier, IFAC bytes
  included. A refused frame comes back inside `DeliverError::Full` or
  `DeliverError::Detached`.
- **`ExternalOutbound`** hands out the frames the node wants sent:
  `recv` (async), `poll_recv`, `try_recv` and `blocking_recv`. Each
  `OutboundFrame` has the packet bytes with IFAC applied, as `Arc<[u8]>`,
  and `high_priority`. `None`, or `OutboundRecvError::Detached`, means the
  interface is gone.
- **`ExternalInterfaceGuard`** has `set_up`, `set_down`, `is_up`, `id` and
  `detach`. Dropping it detaches the interface, through the same path as
  `remove_interface`.

An attached interface goes through the same event-loop registration as a
runtime-spawned TCP client or RNode, so it behaves like one in every way
the transport can see:

- it appears in `interface_stats` with its kind, online state and byte
  counts;
- its IFAC is installed in the core and in the driver's dispatch;
- its mode, kind, path rule and transit flag reach the transport, which
  covers announce rebroadcast and the `ByMedium` window;
- the interface-up re-announce runs when it registers.

Up and down follow the TCP client's rules:

- `set_down` marks the interface offline. It stays registered and keeps
  its paths.
- Frames the node sends while the interface is down wait in the outbound
  buffer, up to its size. A full buffer is backpressure to the node. The
  host may send them once the carrier is back, or drop them.
- Every `set_up` after the first re-runs the interface-up path, which
  re-announces this node's destinations, as a TCP reconnect does.
- If the interface was attached down, its first `set_up` does not
  re-announce: the announces from the attach are still in the buffer.

The byte counters count packet bytes. They do not count the host's
framing.

## `attach_host_interface`

```rust
pub fn attach_host_interface<C: HostCarrier + Send + 'static>(
    &self, carrier: C, cfg: HostInterfaceConfig,
) -> Result<HostInterfaceHandle, Error>;
```

A HostInterface carries **one connection**:

1. It waits for `CarrierEvent::Connected`. If `cfg.connected` is set (the
   host accepted the connection), it does not wait.
2. It reads the carrier's `info()` and registers through
   `attach_interface`.
3. It moves frames until the carrier closes or fails, the handle is
   dropped, or the node removes the interface.
4. It closes the carrier and detaches.

It never redials. **The host owns the reconnect policy**:
`HostInterfaceHandle::ended()` resolves with a `HostInterfaceEnd`, and the
host decides whether to dial again and attach the new connection. A new
connection is a new interface, as an accepted TCP or WebSocket connection
is. There are two reasons. A redial can negotiate different framing (a
WebSocket subprotocol), and the hardware MTU a link negotiates is fixed
when the interface registers.

`HostInterfaceConfig::new(name)` sets the defaults. Its other fields are:

- `mode`, default `Full`;
- `ifac`;
- `bitrate`. `None` takes the carrier's `bitrate_hint`. With neither,
  announces are uncapped;
- `connected`;
- `buffer_size`.

The handle offers:

- `state()`, which is `Connecting`, `Up(id)` or `Ended(end)`;
- `interface_id()`;
- `connected().await`;
- `ended().await`;
- `detach()`. Dropping the handle does the same.

`HostInterfaceEnd` is one of these:

| End | When |
|---|---|
| `Closed(CloseReason)` | The carrier closed |
| `Failed(FailReason)` | The dial failed |
| `Unusable(String)` | The carrier's messages are too small, or the config was refused |
| `Detached` | The handle was dropped |
| `Removed` | The node stopped, or `remove_interface` named it |

### Framing

The connection's `info().framing` picks the framing, and the framing sets
the hardware MTU:

| Framing | On the wire | `rns` name | Hardware MTU |
|---|---|---|---|
| `Message` | One Reticulum packet per message | `rns.raw` | `max_message`, capped at the TCP interface's (16384). A carrier whose messages cannot hold a 500-byte packet is `Unusable` |
| `Stream` | The TCP interface's HDLC (flag `0x7E`, escape `0x7D`, no CRC), with the same framer and deframer | `rns.hdlc` | The TCP interface's (16384) |

Some cases on a message carrier:

- An outbound frame larger than `max_message` gets `Invalid`. It is
  dropped and logged at debug level.
- An inbound message larger than the receive bound ends the connection,
  as the native WebSocket interface does with close code 1009. The bound
  is `max_message`, or the hardware MTU plus 64 bytes of IFAC room when
  the carrier names no `max_message`.
- On a stream, a frame longer than the hardware MTU is discarded, as on
  TCP.

The interface kind comes from the carrier:

| Carrier | Kind |
|---|---|
| `tcp` | `Tcp` |
| `udp` | `Udp` |
| `ws` | `WebSocket` |
| `serial` | `Serial` |
| `ble` | `Ble` |
| `webtransport`, `webrtc`, `http-fetch` | `Unknown` |

No new kind was added. `Unknown` keeps the conservative resource window
under `ByMedium`.

## The carrier trait

```rust
pub trait HostCarrier {
    fn info(&self) -> CarrierInfo;
    fn send(&mut self, data: &[u8]) -> Result<usize, CarrierError>;
    fn recv(&mut self, max: usize) -> Result<Vec<u8>, CarrierError>;
    fn close(&mut self);
    fn poll_event(&mut self, cx: &mut Context<'_>) -> Poll<CarrierEvent>;
}
```

It mirrors the `net` `stream` resource of the `ciris:node` WIT
(CIRISVerify `src/ciris-host-abi/wit/ciris-node.wit`, frozen as
`ciris:node@0.1.0` at tag v23.1.0), one to one:

| WIT | Trait |
|---|---|
| `stream.info` → `conn-info` | `info` → `CarrierInfo` (carrier, framing, max-message, reliable, subprotocol, peer, bitrate-hint) |
| `stream.send` → `result<u32, errno>` | `send` → `Result<usize, CarrierError>`: bytes accepted on a stream (a prefix); any `Ok` means the whole message on a message carrier |
| `stream.recv(max)` | `recv(max)`: up to `max` bytes, or one whole message |
| `stream.close` | `close` |
| `event.connected / readable / writable / closed / failed` carrying the token | `poll_event` → `CarrierEvent::{Connected, Readable, Writable, Closed(CloseReason), Failed(FailReason)}` |
| `errno` | `CarrierError`, one variant per `errno` case |
| `close-reason`, `fail-reason`, `carrier`, `framing` | `CloseReason`, `FailReason`, `CarrierKind`, `Framing` |

Readiness is **edge-triggered**, as in the WIT:

- After `Readable`, the interface calls `recv` until it answers
  `WouldBlock` (or `Closed`) before it waits again.
- `Writable` is awaited only after a `send` answered `WouldBlock`.
- The interface starts by draining and treats itself as writable, since
  an edge may have passed before it was listening.
- A spurious event is allowed. It is read as "try again".
- Nothing accepted is `WouldBlock`, never `Ok(0)`.
- On a `Closed` event, the interface reads what is left before it ends.

### Why a poll method, not async methods

The WIT delivers everything as events through one export (`on-event`),
keyed by a token. A poll method with a waker is the shape that lowers onto
that without glue. The phase-3 driver's `ciris_rt::net` adapter files each
event under its token and wakes the waker the interface registered;
`poll_event` reads the queue. An `async fn recv`-style trait would hide the
would-block step. Edge-triggered readiness has to see that step: the
interface must know it drained to `WouldBlock` before it may wait. Async
methods would also cost a boxed future per call, or pin the trait to one
async-trait encoding.

The trait has no tokio types and no `Send` bound. The tokio driver asks
for `Send` because it spawns the interface. A single-threaded driver over
ciris-rt does not need it. The run loop in `interfaces/host.rs` uses only
`poll_fn` and the attached interface's channels, so it runs on any
executor.

## Testing

`leviculum_std::test_support::mem_carrier` is an in-memory carrier pair
that keeps the WIT's rules strictly:

- `Readable` fires only for a reader whose last `recv` answered
  `WouldBlock`;
- `Writable` fires only for a writer whose last `send` did.

A small `capacity` and `max_chunk` force short writes and would-block.
An interface that stops draining early stalls on it, as it would on the
real host. `tests/attached_interface.rs` runs two nodes over it, once
with each framing. The unit tests in `driver/attach.rs` push hundreds of
frames each way through the run loop and check that none are lost or
reordered.
