# Architecture

Driver internals, packet flow, and platform abstraction details.

## Sans-I/O Core

```
                     ┌─────────────────────────────────┐
                     │         reticulum-core           │
                     │                                  │
  handle_packet() ──►│  NodeCore<R, C, S>               │──► TickOutput {
  (iface_id, data)   │    ├── Transport (routing)       │      actions: Vec<Action>,
                     │    ├── Links + Channels           │      events: Vec<NodeEvent>,
  handle_timeout() ─►│    └── Destinations              │    }
                     │                                  │
  next_deadline() ──►│  Returns: Option<u64>            │
                     └─────────────────────────────────┘

  Action::SendPacket { iface, data }     — send to one interface
  Action::Broadcast { data, exclude }    — send to all interfaces (except one)
```

## Driver Event Loop

The `reticulum-std` driver has 6 `select!` branches:

```rust
loop {
    select! {
        // 1. Packet from any interface
        (iface_id, data) = registry.recv_any() => {
            output = core.handle_packet(iface_id, &data);
            post_dispatch(output);
        }
        // 2. External action (connect, send, announce)
        output = action_dispatch_rx.recv() => { post_dispatch(output); }
        // 3. Timer fires
        _ = sleep_until(next_poll) => {
            output = core.handle_timeout();
            post_dispatch(output);
        }
        // 4. Shutdown
        _ = shutdown.changed() => break
        // 5. New interface (TCP accept, local client connect)
        handle = new_interface_rx.recv() => {
            registry.register(handle);
            output = core.handle_interface_up(iface_idx);
            post_dispatch(output);
        }
        // 6. Periodic storage flush (crash protection, hourly)
        _ = sleep_until(next_flush) => { core.storage_mut().flush(); }
    }
}
```

### Post-dispatch (after every core call)

1. `dispatch_actions(&mut ifaces, &output.actions)` — routes Actions to interfaces (protocol logic in core)
2. React to errors — `BufferFull`: log. `Disconnected`: call `handle_interface_down()`
3. Forward `output.events` to the application
4. Schedule `handle_timeout()` from `output.next_deadline_ms`

## Interface Trait

```rust
pub trait Interface {
    fn id(&self) -> InterfaceId;
    fn name(&self) -> &str;
    fn mtu(&self) -> usize;
    fn is_online(&self) -> bool;
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError>;
}
```

Send-only. Receive is driver-specific (tokio: `mpsc::poll_recv`, Embassy:
interrupt DMA, bare-metal: poll FIFO). `try_send` is fire-and-forget:
Reticulum is best-effort, higher layers retransmit.

`dispatch_actions()` lives in core (not the driver) because action routing
(broadcast exclusion, interface selection) is protocol knowledge.

In `reticulum-std`, `InterfaceHandle` wraps `tokio::sync::mpsc::Sender`
behind the trait. An embedded driver implements it directly on a radio struct.

Core processes packets with zero delay. Collision avoidance (jitter, CSMA)
is the interface's responsibility — fast interfaces (TCP) transmit immediately,
slow interfaces (LoRa) apply send-side jitter.

## Writing a Driver

### 1. Create interface objects
Implement `Interface` on your outbound channel. Register with your own
bookkeeping. Core references interfaces by `InterfaceId` only.

### 2. Run the event loop
Minimum 3 branches: receive, timer, shutdown. Feed everything through
the post-dispatch sequence above.

### 3. Handle the receive path
Driver-specific. On complete packet: `core.handle_packet(iface_id, &data)`
→ post-dispatch. On disconnect: `core.handle_interface_down(iface_id)`.

## Packet Flow

### Incoming
```
Interface → deframe → mpsc → recv_any() → handle_packet()
  → Transport::process_incoming() → TickOutput
  → dispatch_actions() → interfaces → wire
  → events → application
```

### Outgoing
```
Application → connect/send/announce → TickOutput (via action_dispatch)
  → dispatch_actions() → interfaces → wire
```

### Local Client (Shared Instance)
```
lns/lncp → Unix socket → LocalInterface (HDLC)
  → handle_packet() with is_local_client=true
  → local_client_known_dests updated (6h TTL)
```

### RPC (rnstatus, rnpath, rnprobe)
```
Python CLI → Unix socket → RPC server (multiprocessing.connection, pickle)
  → handlers query NodeCore state or trigger probe
  → pickle response → CLI
```

### IPC platform support

The shared-instance data channel and the RPC control channel use abstract
Unix sockets on Linux, filesystem Unix sockets on macOS/BSD, and TCP loopback
on Windows (mirroring Python-RNS's AF_INET fallback). Linux is the tested path
and is the one exercised by our CI; macOS/Windows IPC is community-supported
and not exercised by our CI.

## Storage Trait

Type-safe methods organized by collection:

| Collection | Key methods |
|------------|-------------|
| Packet dedup | `has_packet_hash`, `add_packet_hash` |
| Path table | `get_path`, `set_path`, `remove_path`, `expire_paths` |
| Reverse table | `get_reverse`, `set_reverse`, `remove_reverse` |
| Link table | `get_link_entry`, `set_link_entry`, `remove_link_entry` |
| Announce table | `get_announce`, `set_announce`, `remove_announce` |
| Announce cache | `get_announce_cache`, `set_announce_cache` |
| Receipts | `get_receipt`, `set_receipt`, `remove_receipt` |
| Ratchets | `load_ratchet`, `store_ratchet`, `list_ratchet_keys` |
| Cleanup | `expire_*` per collection |

Shared types in `storage_types.rs`: `PathEntry`, `ReverseEntry`, `LinkEntry`,
`AnnounceEntry`, `PacketReceipt`.

Three implementations: `NoStorage` (no-op), `MemoryStorage` (BTreeMap, embedded),
`FileStorage` (wraps MemoryStorage + disk).

### FileStorage Persistence

| File | Format | Strategy | Contents |
|------|--------|----------|----------|
| `known_destinations` | msgpack map | Batch flush (hourly + shutdown) | Identity → destination |
| `packet_hashlist` | msgpack array | Batch flush | 32-byte dedup hashes |
| `ratchets/{hash}` | msgpack map | Write-through | Receiver ratchet keys |
| `ratchetkeys/{hash}` | signed msgpack | Write-through | Sender ratchet private keys |

Non-persistent collections (paths, reverses, links, announces, receipts)
are RAM-only and rebuilt from network on restart.

## Logging

Sentence-style messages with inline context. Good:
```
Destination <81b22f60> is now 4 hops away via <ecc35451> on iface 1
Answering path request for <4c0c6c7f> on iface 1, path is known
```
Bad:
```
path updated dest=81b22f60 hops=4
```

Use `HexShort` for hashes. Always explain drop reasons ("rate limited",
"duplicate packet", "no path known").

| Component | What | Level |
|-----------|------|-------|
| transport process_incoming | Packet dispatch, drop reasons | `trace!` |
| transport handle_announce | Path updates, rebroadcast decisions | `debug!` |
| transport forward_packet | Forwarding decisions | `debug!` |
| node/link_management | Link lifecycle, RTT retry | `debug!` |
| driver | Startup, interface registration | `info!` |
| interfaces | Connection events, I/O errors | `info!`/`warn!` |
