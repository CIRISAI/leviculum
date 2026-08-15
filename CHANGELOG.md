# Changelog

All notable changes to this project will be documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- CIRIS fork releases carry a `+ciris.N` build-metadata marker so their tags
never collide with upstream's own version line. Downstream (CIRISEdge) pins the
git tag, not the version string. -->

<<<<<<< HEAD
## [Unreleased] — CIRIS fork

### Added

**leviculum#42 — completion futures resolved at the dispatch layer.** The
driver's `dispatch_output` now feeds every event through a leaf-level
`CompletionRegistry` (Arc-shared like `iface_stats_map`, std mutex held for
map ops only, never nested inside the node lock), ahead of `EventSink::emit`
so daemon mode resolves futures too. New `ReticulumNode` API, all additive:
`connect_awaited`, `send_resource_awaited`, `send_request_awaited` — race-free
by register-before-dispatch — plus after-the-fact `await_link_established`,
`await_resource_sent`, `await_request_response`, backed by a bounded
established-links mirror (cap 1024, FIFO evict + warn) and a 256-entry
recent-terminal-outcomes ring checked under the same mutex observation takes.
Futures are oneshot-backed, `select!`/cancel-safe (Drop unregisters), and
resolve with a typed `CompletionError` on `LinkClosed`, `ResourceFailed`,
`RequestTimedOut`, or node stop — a waiter never hangs on a dead object; the
caller owns wall-clock timeouts. This deletes CIRISEdge's six poll loops:
every await/poll path here takes no node lock at all (upstream #199 pressure
shrinks). Wire format untouched.

**leviculum#42 surface (b) — bounded multi-consumer event tap.**
`ReticulumNode::subscribe_events()` returns an `EventTap`: a secondary
observer fed clones at the dispatch layer, BEFORE the two-plane sink, so it
never consumes from or races the primary `EventReceiver`, sees events the
data plane is entitled to drop, and works on a daemon-mode node built
`without_events()`. Backed by a lazy `tokio::broadcast` (256 slots):
drop-oldest on overrun, surfaced as `TapEvent::Lagged(n)` with a cumulative
`lost()` counter; `filtered()` adds consumer-side event filtering. With no
live subscriber the per-event cost is one atomic load. Requires `Clone` on
`NodeEvent`/`ReceivedAnnounce` in leviculum-core — derive-only, no_std-clean,
no wire change.
=======
## [Unreleased]

### Changed

**leviculum#44 — periodic storage flush IO off the node lock.** The event
loop's periodic flush (hourly by default) held the node lock across the full
known-destinations read+merge+write and the packet-hashlist write; on slow
storage that was a recurring deaf window for every inbound packet and outbound
call. The flush now runs in three phases: a brief lock hold snapshots the
dirty state (memory ops only), the file read+merge+write runs on tokio's
blocking pool, and a second brief hold clears the dirty flags —
generation-guarded, so anything dirtied mid-write stays dirty for the next
interval, and a failed or torn-down write simply retries. The in-flight
write's JoinHandle doubles as the overlap guard (a timer fire during a write
re-arms and does nothing), and the shutdown path joins it before `stop()`'s
synchronous flush so a stale background rename can never clobber the shutdown
write. Shutdown behavior, on-disk formats, and the Python-compatible disk
merge are unchanged.
>>>>>>> team/44-offlock-flush

## [0.15.0+ciris.1] — CIRIS fork

Catch-up to upstream master @ `0d93b29` (+107 since 0.8.1: LnFlash tooling,
LXMF file storage + per-transfer limits, the driver-tick CoreProcessor,
path-handling reference parity, radio PHY fixes — see upstream `0.8.1` notes;
the fork's #29 stage 1, #35 telemetry, #38 host codecs, and both test-hygiene
PRs are upstream code now, credited in their changelog).

### Changed

**leviculum#29 stages 2-3 — inbound crypto off the node lock.** The driver
classifies each inbound packet from a header peek and precomputes announce
Ed25519 verification and Single-destination X25519 decryption BEFORE taking
the node mutex; the in-lock apply consumes the memo (`PrecomputedRx`) instead
of recomputing. Measured per-packet lock-hold (release, 20-client floods):
single-dest 35 µs → **1.2-2.3 µs** (~20×), announce 44 µs → **~21 µs** (~2×),
link data unchanged (A/B control). With stage 1 (off-lock resource builds),
every expensive crypto class now runs without the lock — the #29 exclusion
between inbound crypto and outbound calls is structurally gone. Every memo is
advisory and self-authenticating: a stale snapshot or failed off-lock step
falls back to the in-lock path (never skips a check). Wire format untouched
(rnsd interop 316/316). New API: `verify_announce_packet`,
`Destination::export_decryptor`/`SingleDestDecryptor`, `PrecomputedRx`,
`handle_packet_precomputed`. A parallel worker-pool variant was measured and
rejected (task overhead ≥ the crypto moved; announce throughput regressed 2×).

### Fixed

**Windows checkout of the tree (upstream #244).** Upstream 0.8.1's lnflash
sysfs fixtures carry `:` in path names (`3-2.3.1:1.0`), which NTFS cannot
represent — `git clone` and every cargo fetch of this repo as a Windows git
dependency (CIRISEdge's `win_amd64` wheel lane) failed at checkout. The
committed fixture tree now encodes `:` as `+`; lnflash tests materialize the
decoded tree into a tempdir before walking it, so the enumeration code still
sees real sysfs names. Reported upstream as Lew_Palm/leviculum#244.

## [0.14.1+ciris.1] — CIRIS fork

### Fixed

A live-node failure chain on large-MTU links (leviculum#39): `Channel`'s MDU is
now capped at the envelope's `u16` wire ceiling, so an oversized send returns
`ChannelError::TooLarge` instead of passing the link-MDU-only guard and hitting
the `Envelope` length assert as a panic in a lock-holding delivery thread
("envelope data length 115764 exceeds maximum 65535" in the field). The channel
send path constructs envelopes via the new fallible `Envelope::try_new`, and the
three production `lock().unwrap()` sites (`link_is_established`,
`link_destination`, rpc `derive_authkey`) use `lock_recover()`, so a poisoned
mutex can no longer turn one panic into a deaf node. Wire format untouched.
Edge-side counterpart: fragment against `min(link_mdu, 65535)` (CIRISEdge).

## [0.14.0+ciris.1] — CIRIS fork

### Added

The propagation-node HOST direction of the LXMF propagation protocol
(leviculum#38, for CIRISEdge#169 — a CIRIS fabric node hosting mailboxes for
asleep mobiles). Promoted from `#[cfg(test)] pub(crate)` to `pub`:
`MessageGetRequest::decode`, `MessageListResponse::encode`,
`MessageGetResponse::encode`, `PropagationNodeAnnounce::encode`,
`PeerError::code`, and the `PropagatedMessage` type; plus a new
`PropagationUpload::decode` — the inverse of `encode()` for upload envelopes
received off the wire, recomputing the transient ID from the unstamped bytes so
a host validates stamps against the same ID an honest client derived
(singleton-only; the multi-message `/offer` sync form stays out of scope).
Wire format untouched. Offered upstream as Lew_Palm/leviculum#201.

## [0.13.0+ciris.1] — CIRIS fork

Catch-up to upstream master @ `8e3dbef` (+102 since 0.8.0): announce emission
timestamps carried in unix seconds (#155/#160 — cross-restart path replacement),
the driver-tick state-machine seam (a consumer can run a protocol state machine
inside the driver's tick), discovery stamp-cost fix, ingress-control inheritance,
workspace-wide test gates, and the documented core-lock budget.

Everything from the fork's third upstream-PR round is absorbed and dropped from
the carry: the #29 stage-1 off-lock resource build + inbound pre-hash (#152),
both `now_ms()` de-flakes and the macOS absent-daemon error kind (#153), and the
complete #35 per-link delivery telemetry (#154). The carry is now: the
explicit-hash listen API (leviculum#30), fork CI/bench/tooling, and release
bookkeeping.

## [0.12.0+ciris.1] — CIRIS fork

### Added

Per-link delivery telemetry on `LinkStats` (leviculum#35, for CIRISEdge's A/V
ALM passive capacity estimator): `bytes_delivered` (proof-confirmed channel
envelopes + completed outgoing resources — the BBR-style delivery-rate
numerator), `srtt_ms`/`rttvar_ms` (the channel's existing Karn-gated RFC 6298
smoothed RTT, now exported), `min_rtt_ms` (floor of Karn-valid samples, new),
the handshake `rtt_ms`, and the backpressure counters
`busy_rejections`/`pacing_rejections`/`iface_pacing_rejections` (the
app-limited-vs-congestion-limited signal, by source). Cumulative counters the
caller samples ~1 Hz and differences; read-only, no wire change, no new task.

## [0.11.0+ciris.1] — CIRIS fork

Catch-up to upstream **0.8.0** (`Lew_Palm/leviculum` master @ `e023994`): the
LXMF client messaging stack, raw link packets + request Resources, byte-channel
interfaces with runtime hot-plug, runtime add/remove of every interface kind,
new interfaces (Pipe/KISS/AX25KISS/RNodeMulti/I2P), per-interface propagation
modes and rate limits, interface auto-discovery, remote management, tunnel
synthesis, segmented >1 MiB resources, radio PHY fixes, and the Python-compat
dedup/LRPROOF/unpadding fixes — see upstream's `## [0.8.0]` section below.

Everything upstream absorbed from the fork's carry is dropped: `FramesDropped`
(#125), the alias-resolving link accessors (#126), the platform-portability
tests (#127), and the reverse-path contention repro (#128) are now upstream
code. The fork's carry shrinks to: the explicit-hash listen API (leviculum#30),
the leviculum#29 stage-1 off-lock resource build + inbound pre-hash (offered
upstream next), the two `now_ms()` de-flakes upstream's copy still lacks, and
fork CI/bench/tooling.

Merge notes: the off-lock pre-hash now composes with upstream's packet-journey
hash (one hash per packet, driver-precomputed when the bytes are unchanged);
the phased resource-build API carries over onto upstream's new
`new_request`/timeout-watchdog constructor surface; the sync script fetches
upstream `--no-tags` (upstream's independent tags collide with fork-marked
ones); fork CI initializes `reference/Reticulum` + `reference/LXMF` for the
interop suite.

## [0.10.2+ciris.1] — CIRIS fork

Stage 1 of the transport concurrency work (leviculum#29, the transport-side
root of CIRISEdge#370 "rounds time out at scale").

### Changed

- **Resource sends no longer stall the node.** `send_resource`'s bulk build —
  bz2 compress, bulk token encrypt, full/map hashing — previously ran inside
  the one `Mutex<StdNodeCore>` critical section: 141 ms for a 1 MiB
  incompressible (sealed-envelope) payload with compression on, in release,
  during which every inbound decrypt/route and every other outbound call
  blocked. Measured as a **32% inbound throughput stall** while round-sized
  sends run (20-link flood); now **~0%** — the build runs off-lock via a new
  phased core API (`resource_send_params` → `prepare_resource_send` →
  `commit_resource_send`, with a token-key epoch guard so a #66 mid-build
  re-key is caught and retried rather than shipping stale ciphertext).
  `NodeCore::send_resource` composes the same phases, so no_std/FFI callers
  and the wire format are unchanged (rnsd interop 295/295 against Python RNS).
- The per-packet dedup SHA-256 is computed by the driver before taking the
  node lock (`handle_packet_prehashed`); recomputed internally when an IFAC
  strip rewrites the bytes.

### Added

- `ResourceError::LinkStateChanged` — retryable: the link re-keyed between an
  off-lock resource build and its commit.
- Benchmark: `outbound_resource_latency_under_flood` mode (send-call latency +
  inbound-dip under N-link flood, `COMPRESS=1` for the incompressible+bz2
  field case); the published bench (cirisai.github.io/leviculum) now runs in
  **release** and renders the outbound-under-flood metrics.

## [0.10.1+ciris.1] — CIRIS fork

Restores the explicit-hash **listen** API that the `v0.10.0+ciris.1` re-anchor
dropped by mistake. The `#16` commit bundled explicit-hash destinations *and*
`AnnounceControl` (#17); upstream absorbed only `AnnounceControl`, so dropping
the commit as "absorbed" lost the explicit-hash half. This was the sole blocker
for CIRISEdge adopting `leviculum-*` (leviculum#30 / CIRISEdge#371).

### Restored (CIRIS-only, not upstream)

- **`Destination::with_explicit_hash(...)`** — build a Single destination indexed
  by a caller-supplied 16-byte hash (e.g. `sha256(fed_pubkey)[..16]`) instead of
  the derived `truncated_hash(name_hash || identity_hash)`. Identity crypto is
  untouched; only the routing index changes.
- **`Destination::is_explicit_hash()`** and the never-announce guard:
  `Destination::announce()` returns `AnnounceError::ExplicitHashCannotAnnounce`
  for such a destination, and the node's scheduled-announce paths skip it — so an
  explicit-hash destination is reachable only by direct link (opaque hash on the
  wire) and the announce stream stays Python-RNS compatible.

Not restored (edge migrated to upstream-native equivalents): `connect_at`,
`register_destination_at`, `send_on_link`.

## [0.10.0+ciris.1] — CIRIS fork

Re-anchored the CIRIS fork on upstream `Lew_Palm/leviculum` (master @ `fdf8d50`,
crates `0.7.1`), adopting upstream's `reticulum-* → leviculum-*` crate rename.
Everything the fork had upstreamed — explicit-hash destinations (#16),
`AnnounceControl` suppression (#17), the `RNodeChannelFactory` byte-channel
interface (#19), the `announce_app_data_budget` / `packed_size` announce fixes,
the `destination_data` RPC (#12), and `send_on_link` — is now carried by
upstream itself and dropped from the fork's patch series.

### Carried forward (CIRIS-only, not yet upstream)

- **`driver::link_is_established`** — alias-resolving establishment gate
  (CIRISEdge#342).
- **`driver::link_destination`** — alias-resolving accessor for the destination
  a link dialed (CIRISEdge#353).
- **`FramesDropped` node event** — the driver emits `FramesDropped` with a
  `FrameDropReason` instead of silently destroying in-flight frames when an
  interface dies mid-send (#25). Being offered upstream.

## [Unreleased]

### Changed

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

## [0.8.1] - 2026-08-12

### Added

- `lnflash`, a new LNode flashing tool: the full bootloader/SoftDevice
  sequence with Nordic's S140 7.3.0 vendored (licence included), refusal
  of an image that would soft-brick the board, `just lnflash-bundle` for
  the distributable tarball.
- `lnflash` sets the radio configuration at flash time: prompted values,
  `--radio-*` flags, or a preset menu (`--radio-preset` eu868/us915/au915).
- The LNode stores its radio settings in flash, so a host-set
  configuration survives a reset instead of reverting to the compiled
  default.
- `lnstatus` shows a radio interface's last RSSI and SNR (#76).
- `lnstatus -j --tables` exposes the transport's routing tables as
  structured JSON (#174).
- Per-link delivery telemetry — delivery rate, RTT, backpressure — as
  read-only counters; `lev_link_stats` in the C API (#154, emoore).
- The propagation-node HOST direction is public, so an external crate
  can operate a propagation node instead of only being a client of one
  (#201, emoore).
- `lxmf-node`, a new crate running `leviculum-lxmf` as a shared-instance
  client of `lnsd` or `rnsd`, speaking periculum's LXMF helper protocol
  (#196).
- `leviculum-std` runs a consumer `CoreProcessor` inside the driver's
  tick, panic-contained and self-deadlock-reporting (#196, #198).
- LXMF: `FileLxmfStorage` persists state to a directory,
  `MessageState::AwaitingCollection` reports a mailboxed message,
  `RouterEvent::PeerAnnounced` carries a peer's decoded display name,
  error types implement `Error`, `StampExecutor::generate` is `Send`
  (#203).
- Auto-connected discovered peers inherit the bootstrap interface's
  IFAC (#151).
- T114: status screen on the board's optional ST7789 TFT, default-on.
- The LNode honours a host-side reboot frame on its control channel.
- A TCP server on port 0 reports its kernel-assigned address (#221).
- lblogd: a file area so a post can carry pictures, Markdown tables
  rendered as micron tables, per-day served-request counts.
- lnomad: pictures drawn inline (Kitty/iTerm2/Sixel or half-blocks),
  with a bounded in-memory cache (`--image-cache`).

### Changed

- BREAKING: an interface with no `txpower` asks for the board maximum
  (22 dBm) instead of 0 dBm, capped by the lawful ERP limit for the
  frequency (14 dBm on the EU 25 mW sub-bands, 10 dBm on 433 MHz); an
  explicit `txpower` wins and is logged. A deliberate, documented
  deviation from Python-Reticulum.
- The `lnflash` EU default is the ReticulumNet consensus channel:
  869.463 MHz, SF8, BW125, CR4/5, 22 dBm.
- COMPAT: LXMF enforces Python's per-transfer limits in both directions:
  over-limit sends are refused before any build, incoming delivery
  Resources above 1 MB are refused by default; both configurable (#218).
- A carrier touching one of the narrowband alarm bands between the EU
  sub-bands is a config error; the derived airtime limit covers
  433.05-434.79 MHz at 10 % duty cycle; a TX power the SX1262 cannot
  set is rounded down and logged, not silently 14 dBm.
- The LNode firmware's compiled profile transmits at 22 dBm, not 17.
- Resource sends build off the node lock and inbound packets are
  pre-hashed outside it (#29 stage 1, emoore); a resource build can be
  handed to the caller, and superseded builds are refused (#196,
  PAzter1101).

### Fixed

- A transport relay forwards path-directed packets back onto the
  receiving interface, so multi-hop over a single shared LoRa channel
  delivers (A-B-C repeater).
- COMPAT: path handling matches the reference — a shared-instance
  client's destination answers with a fresh path response (#171), a
  never-announced local destination answers its first request (#169),
  no response to the requesting next hop (#168), a targeted response
  transmits once (#192), a pending rebroadcast survives serving a
  response (#170), and re-originated path requests honour the
  per-interface egress limit (#172).
- COMPAT: announces carry wall-clock unix time (#155), the learned
  timebase of a clockless node resists capture (#160), request
  timestamps carry epoch seconds (#164), and `app_data` survives
  re-announce paths.
- COMPAT: LXMF timestamps — any msgpack number is accepted and the
  payload hashed as received (#183), non-finite is refused at signing
  (#184), wall time comes from the node's timebase (#182), microsecond
  precision keeps back-to-back messages distinct (#217).
- COMPAT: propagation length guards reject where Python's do (#201);
  the resource advertisement `o` field carries the salted per-transfer
  hash (#165); a dot in `app_name` or an aspect is rejected (#163).
- One direct LXMF delivery cycle consumes one attempt, and a failed
  outgoing Resource tears its link down before the retry (#179, nilu96).
- A stamp cost outside Python's window is neither announced nor mined,
  and an impossible cost fails instead of hanging the node (#181).
- An accepted connection inherits its listener's `ingress_control`,
  default-on like the reference (#189).
- `lnstest selftest` sizes its delivery windows from the link's own
  bitrate and asks the daemon that owns the radio (#190);
  `interface_stats` reports the radio's on-air bitrate, TX jitter and
  the daemon's listeners (#177, #190).
- `UDPInterface` accepts a hostname in `forward_ip`, re-resolved at
  runtime (#148).
- A corrupt discovery record is warned about once, not per scan (#157).
- The nRF firmware no longer builds `NodeCore` on the stack: the 94 KB
  `main` frame that ate the T114's stack margin is gone, and a gate
  fails any firmware frame above 16 KB.
- lnomad sizes half-block pictures by half-block geometry and renders
  table cells as the inline micron they are.

### Internal

- Spawned test and bridge processes die with their parent
  (`PR_SET_PDEATHSIG`), gate wrappers time out and report survivors,
  the nightly tier runs the whole workspace by construction, test ports
  come from one per-host counter, citation guards cover Rust source.

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
