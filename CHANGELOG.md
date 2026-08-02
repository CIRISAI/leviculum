# Changelog

All notable changes to this project will be documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- The resource advertisement `o` field carries the salted per-transfer
  hash like the reference, so a Python receiver can no longer append
  two transfers of identical content into one reassembly file (#165).

- UDPInterface accepts a hostname in `forward_ip` like rnsd, resolves
  it at runtime with periodic re-resolution, and reports resolution
  failures as interface errors instead of config errors (#148).
- Announces now carry wall-clock unix time in the emission timestamp
  instead of process uptime, so Python peers order our paths correctly
  and a restarted node reclaims its own path entries (#155). Clockless
  nodes (LNode) learn the timebase from received announces or a host
  injection.
- The learned emission timebase refuses implausible values, a single
  announce can only advance an existing timebase by a bounded step,
  and emitted timestamps saturate at the 40-bit field maximum, so a
  crafted or wrong-clock announce can no longer capture a clockless
  node's timebase or truncate its emissions (#160).

### Added

- lblogd serves a file area, so a post can carry pictures. Micron has
  no image construct, so `![Mast](mast.jpg)` publishes the file at
  `/files/mast.jpg` on the web and at `/file/mast.jpg` on the mesh, in
  NomadNet's `serve_file` wire form, linked from the page. Configured
  with `files_dir` and bounded per file by `max_file_bytes` (10 MiB).
- lnomad draws those pictures in the page: a `/file/` link naming a
  decodable format and standing alone on its line is fetched and drawn
  through the terminal's graphics protocol (Kitty, iTerm2, Sixel), else
  Unicode half-blocks, else a line naming the file, format and size —
  which is also what a failed decode and `--no-color` give. Fetched
  after the page is on screen, one at a time, at most eight per page;
  `Esc` cancels the queue and `--images off` disables it. `Enter` saves
  a picture and `o` opens it, neither transferring it again.
- Fetched pictures are kept in a byte-bounded in-memory LRU cache, so a
  revisit costs no airtime: `--image-cache <megabytes>` (default 10, `0`
  disables). Memory only, never disk.
- A Markdown table in a post becomes a micron `` `t `` table instead of
  plaintext rows: header, the alignment row micron reads as the second
  line, then the data rows, with a literal `|` in a cell escaped as the
  reference parser expects.

### Fixed

- lnomad sizes a half-block picture by half-block geometry. A cell there
  carries one pixel across and two down, but the fit was measured against
  the terminal's font box, so a 300x300 portrait was drawn as 38x38
  pixels — recognisable as nothing. It now fills the page width (78x78
  for that portrait), may run up to two screenfuls tall since height is
  resolution on that backend, and asks the protocol for that area with
  `Resize::Scale`, which `Resize::Fit` had been quietly undoing.
- lnomad renders a table cell's contents as the inline micron they are.
  Cells were pushed to the screen as plain text, so a style, a colour or
  a link inside one showed its markup — `` `B333code`b `` instead of
  `code` — and the column was sized to the markup rather than to the
  text. Each cell is now parsed and flattened, as the reference does by
  re-parsing every formatted row line, which also makes a link in a cell
  followable and sizes columns by visible width. An escaped `\|` splits
  no column and loses its backslash.

## [0.8.0] - 2026-08-01

### Added

- LXMF client messaging stack: new `leviculum-lxmf` crate (no_std core,
  std runtimes), opportunistic/direct/propagated delivery, stamps,
  tickets, paper messages; locked against Python LXMF 1.1.0 (#138, nilu96).
- Raw link packets (`send_packet_on_link`) and oversized link requests
  carried as request Resources, both Python-canonical (#138).
- Byte-channel interfaces over a caller-supplied duplex, including
  RNode with runtime hot-plug (#141, PAzter1101).
- Runtime add/remove of interfaces of every kind (#135, PAzter1101).
- Outbound-socket hook on every TCP dial including the I2P SAM bridge,
  fail-closed (#142, PAzter1101).
- New interfaces Pipe, KISS, AX25KISS, RNodeMulti, I2P (#95-#99);
  Backbone names, multiple AutoInterfaces, UDP multi-address (#89, #7, #4).
- Per-interface propagation modes, announce rate limits, bitrate
  weighting, ingress control, IFAC enforcement (#8, #90-#93, #104).
- Interface auto-discovery with PoW-stamped, optionally encrypted
  announces, Python-interoperable (#32, #106, #107).
- Remote management: lnsd serves `rnstatus -R`; client `-R/-i/-w` (#86).
- Tunnel synthesis and path restore on TCP connect/reconnect (#64).
- Resources over 1 MiB send segmented like Python (#27).
- Destination announces re-sent on a recovered interface (#132).
- EU 868 lawful-by-default airtime cap in the LNode firmware (#55);
  RNode TX gated during the firmware airtime lock (#121); radio stats (#25).
- Config/CLI parity with rnsd (loglevel, ConfigObj quirks, instance
  ports); lnsd `-s/--service` and `--exampleconfig`.
- FFI: dropped event fields projected with accessors (interface id,
  close reason, sizes, segments), stats-snapshot ids, delivery errors.

### Changed

- BREAKING: `lns` is renamed to `lnstest`; placeholder subcommands
  removed, file transfer lives in `lncp`.
- BREAKING: request handlers keyed by (destination, path); deregister
  takes the destination, `RequestReceived` carries `destination_hash`.
- BREAKING: `transport::DropReason` is `#[non_exhaustive]`.
- Oversized single-segment responses fail closed (`ResourceTooLarge`);
  response correlation is per-link like Python (#138).

### Fixed

- COMPAT: the six link-traffic contexts Python exempts from packet
  dedup are exempt; idle Python-initiated links no longer die stale.
- COMPAT: relays rewrite forwarded LRPROOF hops so strict Python
  clients establish; shared-instance hop counting matches (#38, #119).
- COMPAT: token unpadding matches Python; microReticulum peers decrypt.
- COMPAT: `multicast_loopback` defaults true like Python (carrier flap fix).
- SECURITY: msgpack recursion DoS via resource advertisements (#23).
- Radio PHY matches the RNode firmware: airtime-scaled TX/CAD timeouts
  (SF12 was undeliverable), derived preamble (#143), working RX-extend
  guard (#144), preamble-charged airtime accounting (#149),
  symbol-duration LDRO (#150; wire note: SF11/BW125 LDRO now off).
- Links: healthy idle links survive (#123), inbound proofs count as
  activity (#124), establishment jitter breaks lockstep (#129).
- Expired paths re-originate discovery, with bounded retry (#117, #44).
- Per-interface `announce_cap` now takes effect (was parsed and dropped).
- A poisoned mutex no longer crashes the daemon; TCP reconnects back off.
- Runtime-attached interfaces apply the configured IFAC.
- An unverifiable delivery proof reports `InvalidProof`, not `LinkFailed`.

### Internal

- Wire-parser fuzz harness, Python-interop suite growth, periculum
  test-framework extraction, lintian-clean debs, RUSTSEC bumps.

## [0.7.0] - 2026-06-22

### Added

A comprehensive C API (`leviculum.h`) covering node lifecycle and
config-file / shared-instance daemon setup, destinations, links
(connect, send, receive, identify), datagrams and request/response,
resource transfer, identity sign/verify/encrypt/decrypt with
ratchets, delivery-proof strategies, read-only diagnostics, an event
stream over an event fd, RNode and serial interfaces, and
packaging/hex helpers. Ships with C examples and a large test suite
(Codeberg #29). A new `leviculum-std::api` safe API module and
driver builder back the binding.

Configurable link keepalive interval on `TransportConfig`.

Structured event-log observability: `ANN_TX` records announce
rebroadcasts, and `PKT_DROP_SUMMARY` carries a complete drop-reason
taxonomy (the total equals the sum of the reasons).

Radio-config wire format gained a `radio_silent` flag (byte 15 of
the payload, backward-compatible parse down to 13 bytes). When set,
the T114 firmware drops outgoing LoRa packets at the driver
boundary — the radio keeps listening but never transmits. The
integration-test runner uses this to neutralize every T114 the
scenario does not bind, so single-pair LoRa benchmarks stop
seeing the idle T114's Reticulum announces as CSMA-busy. Bug #2
CA-ON single-pair PDR distribution collapses from σ≈21 to σ≈4
(mean 79 → 97 %, min 44 → 88 %).

### Changed

BREAKING: auto-accept is the only link model. The manual accept path
(`NodeEvent::LinkRequest`, `accept_link`) was removed and replaced by
`link_handle`. A destination can decline inbound links via
`accepts_links = false` (`lev_destination_set_accepts_links`).

The Debian package now installs `/etc/reticulum/` and
`/etc/reticulum/storage/` with mode 2775 (group-writable + setgid).
This makes the directory a true single source of truth shared by
lnsd, the native `lns`/`lncp` clients, the Python tooling
(`rnstatus`, `rncp`, `rnpath`, `rnprobe`, Sideband, Nomadnet, …)
and — if the operator ever swaps daemons — Python's `rnsd`. Any
user in the `leviculum` group can persist Reticulum state under
the shared configdir, and Python's `RNS.Reticulum()` auto-detect
of `/etc/reticulum` then completes without permission errors. No
per-user configuration step is needed.

### Fixed

Announces above `PATHFINDER_MAX_HOPS` (hops > 128) are now gated,
matching Python-RNS. Responder-initiated graceful close is now
reliably delivered via the driver shutdown drain (Codeberg #77).
In-flight resources are failed with `ResourceFailed` on link
teardown instead of being silently dropped (Codeberg #78). The
structured event log is now well-formed by construction (no more
corrupt `LINK_ENTRY_SET` lines or field violations). Stale detection
now works for links established at uptime second 0.

RNode `flow_control = true` no longer deadlocks the send path. The
I/O task previously waited for a `CMD_READY` from the firmware that
only arrives after a TX, producing a chicken-and-egg stall (no TX
ever fires, the send queue saturates and emits "send queue full,
dropping oldest" until the upstream traffic source goes away).
`interface_ready` is now `true` at io-task start, mirroring Python
`RNodeInterface.py` after `validateRadioState()`.

### Internal

The event-log sink moved out of `test_support` into a production
module. Added local `.deb` build recipes (`just build-deb*`).
Integration-test runs are now hermetic across processes
(process-unique docker names).

## [0.6.3] - 2026-04-01

### Fixed

Fix: plain broadcast packets forwarded through shared instance — local clients can now send and receive unencrypted broadcasts via the daemon

## [0.6.1] - 2026-03-21

### Fixed

Fix: resource transfer proof retry over LoRa — sender sends CacheRequest when proof is lost, receiver re-sends cached proof

## [0.6.0] - 2026-03-20

### Added

Link requests are now retried up to three times on establishment timeout with exponential backoff (E34). When a link proof is lost, the responder re-sends the cached proof on receiving a duplicate link request. Three-node shared medium LoRa tests cover bidirectional transfer, contention, and relay scenarios. The LoRa test matrix now includes size sweep, frame loss, link-under-loss, bidirectional, and cross-implementation tests across all Rust and Python pairings. Proxy rules gained `max_size`, `min_size`, and `skip` filters for targeting specific packet types by size range.

The `lncp` tool gained fetch mode (`-f`, `-F`, `-j`) with jail path restriction and identity-based authentication, physical layer rate display (`-P`), compression toggle (`-C`), and silent flag (`-S`). It works as a shared instance client connecting to a running daemon via Unix socket.

Link request/response provides single-packet RPC over established links. Link identity verification proves ownership via Ed25519 signature. Resource transfers show real-time progress with speed and percentage.

LoRa reliability improved through send queue priority (link traffic before announces), first-hop timeout accounting for airtime, RTT packet retry confirmed by inbound traffic, discovery path request retry, interface backpressure with retry queue, per-hop establishment timeout scaling, and reduced responder handshake timeout from 360s to 54s.

The integration test framework gained Docker-based multi-node scenarios with TOML-defined topologies, dual-cluster tests up to 10 nodes, ratchet selftest modes with disk persistence, link failure simulation via iptables, negative assertions, and env-var radio overrides for LoRa profiles. RPC compatibility with Python CLI tools (`rnstatus`, `rnpath`, `rnprobe`) is complete. AutoInterface provides zero-config LAN discovery via IPv6 multicast.

### Fixed

Resource retransmit timing now matches Python with adaptive timeout factors, progressive backoff, and grace times. Receiver retransmit requests are rebuilt with only missing parts instead of re-requesting already-received data. The retransmit timeout resets correctly between retries. Shared-instance resource retransmissions are no longer blocked by packet dedup. Multi-segment resource receive handles dynamic buffer sizes, correct hashmap lengths, and proper metadata parsing. The `lncp` listener accepts incoming links. Resource API actions are dispatched immediately. RNode serial heartbeat prevents idle-correlated LoRa failures after prolonged silence. Channel SRTT is seeded to prevent retransmit storms.

The selftest no longer overwrites the daemon's transport identity. Path requests are re-originated at each hop matching Python behavior. Hops are incremented on receipt so direct neighbors show as one hop. Cached announces are converted to the correct header format when forwarded to local clients. Path request responses reach local clients correctly. AutoInterface peer identity, source port, and discovery all work across machines. Announce replay protection allows better-hop paths through, and rate-limited announces still update the path table. Vendored Python RNS ingress_control inheritance is fixed.

### Changed

Jitter ceiling is now airtime-based with exponential backoff on announce collisions. The `WindowFull` error is renamed to `Busy` across all types. All Transport and NodeCore collections live behind the type-safe Storage trait. `MemoryStorage` is the production embedded implementation and `FileStorage` wraps it with persistence. Announce rebroadcast is immediate, removing per-hop latency. FileStorage packet cache uses HashSet with a 50k identity cap.

## [0.5.19] - 2026-02-15

### Fixed

Pacing interval used handshake RTT instead of measured SRTT.

## [0.5.18] - 2026-02-15

### Changed

Timeout computation uses current queue length instead of frozen send-time values. Smoothed RTT from proof round-trips uses RFC 6298 with Karn's algorithm. Maximum channel retries increased from five to eight, and the first retransmit skips pacing decrease.

## [0.5.17] - 2026-02-14

### Added

Sender-side pacing with AIMD congestion control spaces sends evenly across the RTT instead of bursting until busy.

## [0.5.16] - 2026-02-14

### Fixed

Retransmitted messages were permanently rejected when the proof was lost due to sequence wrap-around.

## [0.5.15] - 2026-02-14

### Fixed

Channel retransmissions never triggered because duplicate Channel instances existed per link. Unified into one.

## [0.5.14] - 2026-02-13

### Fixed

ConnectionStream silently dropped messages when busy. It now returns WouldBlock. The selftest closed links before messages were confirmed and counted Busy as permanent failure.

## [0.5.13] - 2026-02-13

### Fixed

The peers display showed unknown hop counts and garbled app_data from Python msgpack formats.

## [0.5.12] - 2026-02-12

### Added

PacketEndpoint handle provides fire-and-forget delivery to single-packet destinations.

### Fixed

Single-packet delivery through relays was broken. Packets are now converted from Type1 to Type2 format for relay paths.

## [0.5.11] - 2026-02-12

### Changed

`Identity::encrypt()` returns Result instead of panicking on failure. Selective re-exports from leviculum-std replace the blanket `pub use leviculum_core::*`.

## [0.5.10] - 2026-02-12

### Changed

ConnectionStream is send-only. Received data is delivered exclusively via NodeEvent.

## [0.5.9] - 2026-02-12

### Fixed

Channel data proofs were not generated on the responder because the signing key was gated on proof strategy. On the initiator, the wrong signing key was consulted.

## [0.5.8] - 2026-02-12

### Added

The `lns connect` command provides an interactive CLI for diagnostics, link management, and data exchange.

### Fixed

Links in Stale state now recover to Active on inbound traffic, matching Python.

## [0.5.6] - 2026-02-11

### Fixed

MessageReceived events were silently dropped so channel data never reached ConnectionStream.

## [0.5.5] - 2026-02-11

### Fixed

Link-addressed Data and proof packets were dropped on non-transport nodes. Channel mark_delivered was never called, breaking the proof delivery chain. ConnectionStream close did not send LINKCLOSE.

## [0.5.4] - 2026-02-11

### Fixed

PathRequestReceived emitted an incorrect PathFound event with fabricated data.

## [0.5.3] - 2026-02-11

### Fixed

Multi-hop link initiation from non-transport nodes used the wrong header format. LRPROOF delivery to local pending links was silently dropped.

## [0.5.2] - 2026-02-11

### Fixed

Four hop off-by-one bugs in forwarding thresholds caused by Python/Rust hop semantics mismatch.

## [0.5.1] - 2026-02-11

### Fixed

Multi-hop link forwarding through mixed relay chains failed due to premature header stripping and wrong transport_id.

## [0.5.0] - 2026-02-11

### Changed

All NodeCore mutation methods return TickOutput for immediate action dispatch.

## [0.4.4] - 2026-02-10

### Added

Per-destination announce rate limiting matches Python with violation, grace, and penalty phases.

## [0.4.3] - 2026-02-10

### Fixed

Path rediscovery was dead code because the event handler was empty.

## [0.4.2] - 2026-02-09

### Added

Expired links trigger path rediscovery with unresponsive state tracking.

## [0.4.1] - 2026-02-08

### Added

`NodeCore::announce_destination()` broadcasts registered destinations.

### Fixed

Outbound packets were not cached for dedup so the node learned paths to itself via echo.

## [0.4.0] - 2026-02-07

### Added

Embedded skeleton for the Heltec Mesh Node T114 (nRF52840 + SX1262). Channel-based InterfaceHandle and InterfaceRegistry with async event loop.

## [0.3.1] - 2026-02-06

### Fixed

`send_on_connection()` dropped the first packet and `connect()` never sent the link request.

## [0.3.0] - 2026-02-06

### Changed

Sans-I/O architecture introduced. `handle_packet()`, `handle_timeout()`, and the Action enum replace direct I/O. The driver owns all interfaces. The Context trait is removed in favor of direct `rng` and `now_ms` parameters.

## [0.2.8] - 2026-02-04

### Fixed

Transport enable flag was not wired. Relay hop count, destination hash, proof routing, and announce replay all corrected.

## [0.2.6] - 2026-02-03

### Fixed

Keepalive packets were encrypted instead of sent as plaintext, causing rejection by Python peers.

## [0.2.5] - 2026-02-03

### Added

Link-level data proof system with PROVE_ALL, PROVE_APP, and PROVE_NONE strategies.

### Changed

DestinationHash and LinkId are now newtypes. Packet queues unified in LinkManager.

## [0.2.3] - 2026-02-01

### Added

High-level Node API with NodeCore, NodeCoreBuilder, ReticulumNode, and ConnectionStream. Channel system, packet proofs, ratchets, IFAC, link keepalive, and graceful close.

## [0.2.0] - 2026-01-30

### Added

Destination announce, link responder, LinkManager API, and event system.

## [0.1.0] - 2025-XX-XX

### Added

Initial release with cryptography, identity, packets, announce, link state machine, HDLC framing, TCP interface, and transport layer. Full interoperability with Python rnsd.

[0.6.0]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.19...v0.6.0
[0.5.19]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.18...v0.5.19
[0.5.18]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.17...v0.5.18
[0.5.17]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.16...v0.5.17
[0.5.16]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.15...v0.5.16
[0.5.15]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.14...v0.5.15
[0.5.14]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.13...v0.5.14
[0.5.13]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.12...v0.5.13
[0.5.12]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.11...v0.5.12
[0.5.11]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.10...v0.5.11
[0.5.10]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.9...v0.5.10
[0.5.9]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.8...v0.5.9
[0.5.8]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.6...v0.5.8
[0.5.6]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.5...v0.5.6
[0.5.5]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.4...v0.5.5
[0.5.4]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.3...v0.5.4
[0.5.3]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.2...v0.5.3
[0.5.2]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.1...v0.5.2
[0.5.1]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.5.0...v0.5.1
[0.5.0]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.4.4...v0.5.0
[0.4.4]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.4.3...v0.4.4
[0.4.3]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.4.2...v0.4.3
[0.4.2]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.4.1...v0.4.2
[0.4.1]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.4.0...v0.4.1
[0.4.0]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.3.1...v0.4.0
[0.3.1]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.3.0...v0.3.1
[0.3.0]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.2.8...v0.3.0
[0.2.8]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.2.6...v0.2.8
[0.2.6]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.2.5...v0.2.6
[0.2.5]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.2.3...v0.2.5
[0.2.3]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.2.0...v0.2.3
[0.2.0]: https://codeberg.org/Lew_Palm/leviculum/compare/v0.1.0...v0.2.0
