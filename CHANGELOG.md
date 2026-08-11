# Changelog

All notable changes to this project will be documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- An LNode keeps its radio configuration in flash, so a host-set frequency
  survives a reset instead of falling back to the compiled default.

- `lnflash` sets that configuration at flash time: the EU868 defaults, five
  prompted values, or the `--radio-*` flags.

- The `leviculum-lxmf` error types implement `Display` and
  `core::error::Error`, so a client shows an error instead of wording one.

- `FileLxmfStorage` in `leviculum-std` persists LXMF state to a directory,
  so a host application no longer writes its own `LxmfStorage`.

- `MessageState::AwaitingCollection` reports a message a propagation node
  holds for a recipient who has not collected it, instead of the `Sent` a
  direct delivery gets. Local bookkeeping; nothing changes on the wire.

- `RouterEvent::PeerAnnounced` reports a peer's decoded delivery announce,
  so an application reads a display name without filtering announces and
  decoding app data itself.

- The T114 firmware drives the board's optional ST7789 TFT with the same
  status screen the WisMesh Pocket V2 shows, blind and default-on (the
  write-only panel cannot be probed), painted by a shared host-tested
  crate with dirty-rectangle SPI updates.

- A TCP server interface configured with port 0 reports its kernel-assigned
  address: `tcp_listen_addrs()` on the node, `lev_tcp_listen_addr` in the C
  API (Codeberg #221).

- `lblogd` counts what it served, one appended `key=value` record per UTC
  day in `<data_dir>/counts.log` (`[counter]` moves it or switches it
  off). Requests and links, named as such: a Reticulum link is a session
  and not a person, `RequestReceived` carries no identity at all, and the
  web side never reads a peer address — so there is no visitor number,
  because neither side can honestly produce one. Each record holds that
  day's whole running total and the last record for a date wins, so a
  `kill -9` mid-append costs at most the line it was writing and never an
  earlier day; a clock stepping backwards keeps the open day open and
  says so in `clock_behind` rather than rewriting a day already on disk.
  The day is written every five minutes, at each rollover and on
  `SIGTERM`, a restart resumes the day from the file, and each start
  compacts it to one record per date.

### Fixed

- The nRF firmware no longer builds its `NodeCore` on the stack: the
  94 KB `main` frame that left the T114 ~13 KB of stack margin is gone.
  New `NodeCoreBuilder::build_boxed`, plus a `just nrf-stack-frames` gate
  that fails any firmware frame above 16 KB.

- LXMF message timestamps carry microsecond precision, so two identical
  messages sent back to back are two messages and not a duplicate
  (Codeberg #217).

### Changed

- BREAKING: an interface with no `txpower` asks for the board maximum
  (22 dBm) instead of 0 dBm, capped by the lawful ERP limit for the
  frequency from ERC 70-03 (14 dBm on the EU 25 mW sub-bands, 10 dBm on
  433 MHz). A board that can do less clamps and says so; an explicit
  `txpower` wins even above the cap and is logged; `txpower = 0` still
  means 0. A deliberate deviation from Python-Reticulum, pinned in the
  compatibility document.

- A carrier whose occupied bandwidth touches one of the narrowband alarm
  bands between the EU sub-bands (868.6-868.7 MHz and siblings) is a
  config error at interface build, not an unlimited band.

- The derived airtime limit covers 433.05-434.79 MHz at 10% duty cycle.

- The LNode firmware's compiled profile transmits at 22 dBm, not 17.

- A configured TX power the SX1262 has no PA setting for is rounded down
  to the nearest one and logged, instead of silently becoming 14 dBm.

- `StampExecutor::generate` returns a `Send` future, so a host can spawn a
  stamp mine on a work-stealing runtime. Custom executors and `Yield`
  implementations must be `Send` (Codeberg #203).

- Every long-lived external process this repository spawns now dies with
  its parent because the kernel kills it, not because a destructor got to
  run. `leviculum_std::process::spawn_supervised` sets
  `PR_SET_PDEATHSIG` to `SIGKILL` on the child between `fork` and `exec`,
  and forks from one dedicated thread that never exits — the flag is
  per-task and fires when the *forking task* exits, so forking from a
  tokio worker would have killed daemons mid-test. The child also
  re-reads `getppid()` after setting the flag and stands down if it lost
  its parent inside that window. It covers the Python `TestDaemon` and
  its `socat` pty pair, `PyDaemon`, the C `lnsd`/`lncp`/`levcat`
  programs, `lnsd` and the vendored `rnsd` in the mvr, load-test,
  reverse-RPC, status-parity and instance-conflict harnesses, and the
  production `PipeInterface` bridge program. Seven orphaned
  `scripts/test_daemon.py` processes were found alive on 2026-08-07, the
  oldest over four hours old and from several different runs, one of
  which held a pipe that kept `just standard` alive for two hours after
  its verdict was decided.
- `just fast` gained both halves of that property: a census over the
  sources pinning every remaining bare `Command::new(..).spawn()` per
  file (`scripts/supervised-spawn-counts.txt`), and a test that SIGKILLs
  a parent and requires its child to be gone within a bounded deadline —
  paired with the same experiment on a bare spawn, where the child must
  survive.
- A gate wrapped by `scripts/run-with-manifest.py` waits for its command
  to exit rather than for the output pipe to close, so a process a test
  leaked can no longer hold it open: `just standard` spent two hours
  alive and silent on 2026-08-07 holding a red it had already decided.
  The command's process group is killed once it has exited, every
  survivor is reported by pid and command line, and a gate that exceeds
  its budget (1800 s by default, `--timeout` per gate,
  `LEVICULUM_GATE_TIMEOUT` globally) exits 124 naming itself, how long it
  waited and what was still alive. A standing canary spawns a child that
  leaks a grandchild holding the pipe and fails if the wrapper stops
  terminating.
- `just complete`, in the `extensive` tier, runs the whole workspace by
  construction instead of the tiers naming packages; 327 tests were
  executed by no gate (#194).
- Test listener ports come from one counter per host, so concurrent test
  processes are never handed the same port (#194).
- `just standard` runs the `status_parity` two-daemon suite (#191).
- The number of `#[ignore]`d tests is pinned per test unit and checked
  in `just standard` (#191).
- A connection accepted by a listener inherits the listener's
  `ingress_control` instead of always having it off, and a listener
  defaults it on as the reference does (#189).
- `lnstest selftest` sizes its single-packet delivery window from the
  link's own bitrate and pre-TX jitter ceiling instead of a fixed sleep,
  and reports an expiry as a budget expiry rather than as loss (#190).
- `lnstest -c <dir> selftest` asks the daemon that owns the radio for
  that link profile; without it the phases keep the fixed wait (#190).
- The drain budget prices the frame as it crosses the air, including the
  address field a forwarder inserts, not as the tool packed it (#190).
- `interface_stats` reports a radio interface's own on-air bitrate
  instead of the TCP `BITRATE_GUESS`, and adds `tx_jitter_max` (#190).
- `interface_stats` reports the listeners the daemon runs (shared
  instance, TCP server) next to its routable interfaces, and names every
  accepted connection like the reference does (#177).
- Signing an LXMF message with a `NaN` or infinite timestamp is refused
  (#184).
- The LXMF router resolves wall time from `NodeCore::emission_secs`
  instead of a `now_unix` parameter, and refuses to issue a ticket on a
  node with no plausible clock (#182).

### Fixed

- `PyDaemon::drop` reaches the kill on every path. It sent the polite
  `shutdown` RPC through a helper that `expect`ed a JSON response, and a
  daemon that is already stopping answers with an empty body — so the
  destructor panicked, and a panic in a destructor already running during
  unwinding aborts the process, which meant `child.kill()` two lines
  below never ran. That is the second, independent reason the same daemon
  leaked on 2026-08-07 (#215). The polite shutdown now swallows every
  error it can produce and the kill is unconditional.
- A propagation upload is refused at the length Python refuses it at.
  `PropagationUpload::decode` guarded at `STAMP_SIZE +
  DESTINATION_LENGTH` (48) where `validate_pn_stamp` guards at
  `LXMF_OVERHEAD + STAMP_SIZE` (144), so a host on this decoder stored
  bodies in the band 49..=144 that every Python propagation node
  discards — and that its own `PropagatedMessage::from_unstamped_bytes`
  then refused. That parser was the same bound off by one from the other
  side, accepting exactly `LXMF_OVERHEAD`; both now reject where the
  reference does (#201).
- `generate_stamp` refuses a cost above the 256-bit hash width instead
  of searching for a stamp that cannot exist: `stamp_valid` rejects every
  candidate at that cost, so the loop could not terminate at all and one
  off-by-one past the legal maximum left the node permanently dead.
- A `CoreProcessor`'s `on_tick` output is dispatched on its own instead
  of being merged into the core's, so it no longer re-enters the event
  tap, the `/status` responder — which answers on the strength of a
  core-side authorisation that never ran for a synthesised request — or
  the discovery registry, which would persist a synthesised announce.
  The two hooks are now isolated identically (#196).
- A `CoreProcessor` sees the `FramesDropped` its own send caused. The
  notice is built inside `dispatch_output` and never passes through
  `handle_packet`, so it was the one #25 loss signal a processor could
  not learn any other way (#196).
- The processor budget report is measured from inside the core lock, so
  another thread's contention is no longer charged to a processor that
  did nothing (#196).
- A `next_deadline_ms` returned from `on_event` is honoured, as one
  returned from `on_tick` already was (#196).
- The `status_parity` freeze waits out the 1 Hz traffic sampler, so a
  speed that has not been sampled yet is no longer read as an idle one
  (#195).
- Every gate wrapper keeps its full output on disk, and a failure's copy
  survives the green runs after it (#195).
- A targeted path response is transmitted once instead of twice, so a
  requester no longer gets a duplicate announce five seconds later
  (#192).
- An LXMF message whose timestamp is any msgpack number, not only
  float64, is accepted, and an unstamped payload is hashed as received
  instead of re-encoded; both dropped messages from writers other than
  Python LXMF (#183).
- One direct LXMF delivery cycle now consumes one delivery attempt
  instead of two, and a failed outgoing Resource tears its link down
  before the retry; a receiver's cancel is terminal and keeps the link
  (#179, contributed by nilu96).
- A repeated propagation stamp request no longer writes an identical
  router snapshot every processing interval (#179, contributed by
  nilu96).
- An announced LXMF stamp cost outside the reference's `0 < cost < 255`
  window is no longer sent, and an announced 255 from a peer is no
  longer mined; both would run forever (#181).
- A re-originated recursive path request now honours the per-interface
  egress limit, so an interface already saturated with path requests is
  skipped instead of carrying every one (#172).
- Creating a destination with a dot in `app_name` or an aspect is
  rejected like the reference, closing a destination-hash collision
  (#163).
- A corrupt discovery record is warned about once per record instead of
  on every scan pass (#157).
- A path request for a destination hosted on a shared-instance client
  is now forwarded to that client and answered with the client's fresh
  path response instead of only the cached announce (#171).
- A local destination that has not announced since process start now
  answers its first path request instead of only the retry (#169).
- A path request from the transport instance that is our own next hop
  toward the requested destination is no longer answered, matching the
  reference's loop-avoidance rule (#168).
- The resource advertisement `o` field carries the salted per-transfer
  hash like the reference, so a Python receiver can no longer append
  two transfers of identical content into one reassembly file (#165).
- Request timestamps carry epoch seconds from the emission timebase
  instead of process uptime, so a Python peer's request handlers see a
  real `requested_at` (#164).

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

- The propagation-node HOST direction is public: `PropagationUpload::
  decode`, `MessageGetRequest::decode`, `MessageListResponse::encode`,
  `MessageGetResponse::encode`, `PropagationNodeAnnounce::encode`,
  `PropagationSignal::encode`, `PeerError::code` and `PropagatedMessage`,
  so an external crate can operate a propagation node and not only be a
  client of one (#201, leviculum#38). `PropagationSignal::encode` emits
  the `[LXMPeer.ERROR_INVALID_STAMP]` packet a node sends when it refuses
  an upload; `PropagationError::MultipleMessages` reports the peer
  `/offer` sync form distinctly from a malformed length.
- `lxmf-node`, a new `leviculum-lxmf-node` crate, runs `leviculum-lxmf`
  as a shared-instance client of `lnsd` or `rnsd` and speaks periculum's
  LXMF helper protocol — the same stdin commands and `EVENT` lines as the
  Python `lxmf_node.py`. periculum's six LXMF verbs now drive either
  messaging stack via `lxmf_start`'s `helper` field, so a scenario can
  hold the daemon constant and vary only the messaging stack. Built
  entirely on the two crates' public APIs, which is the acceptance
  evidence for #196's first criterion (#196).

- `leviculum-std` lets a consumer register a `CoreProcessor` that runs
  inside the driver's tick: it receives `&mut StdNodeCore` plus each
  `NodeEvent` and returns a `TickOutput` the driver dispatches on its own
  send path, so `leviculum-lxmf` can be driven on the async runtime
  without forking the driver. The events are tapped ahead of the lossy
  application sink, so a processor sees the `EventClass::Data` events the
  sink is entitled to drop (#196).

- A panic in a registered `CoreProcessor` no longer kills the node. The
  driver catches the unwind, detaches the processor for good and reports
  `NodeEvent::CoreProcessorPanicked` on the control plane, so a consumer
  learns its state machine stopped instead of waiting on an event stream
  nothing will ever close again (#196).

- `lnstatus -j --tables` adds the transport's path, reverse, link,
  announce, announce-cache and tunnel tables to the JSON output, so a
  test can assert on routing state instead of scraping logs. A daemon
  that cannot answer omits the key rather than reporting empty tables
  (#174).

- The LNode firmware honours a host-side reboot frame on its control
  channel and ACKs before resetting, so a test harness can start every
  run from a defined board state.

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
