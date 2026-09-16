# Changelog

All notable changes to this project will be documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The SenseCAP Solar Node P1-Pro reports its pack voltage on the `[BAT]`
  line like every other board, read through its own 1 MΩ/510 kΩ divider
  rather than the T114's and sampled with the longer acquisition window
  that divider's 338 kΩ of source resistance needs (Codeberg #233).

- A node's BLE advertisement now carries the first four bytes of its
  identity hash, so a board or `lnsd` recognises a peer it already holds
  a link to before it dials it, and no longer spends its scarce outgoing
  link on that peer under a new Bluetooth address. It reaches as far as
  the record does: a peer that advertises no capability record, an
  Android Columba among them, is dialled exactly as before
  (Codeberg #412).

- `lnflash` knows the SenseCAP Solar Node P1-Pro, so every control command
  reaches it: `--watch`, `--announce`, `--set-time`, `--set-name`,
  `--set-ble-tx-gap` and the `--radio-*` flags. The board runs our firmware
  on the rig and answered none of them before, because no catalogue entry
  claimed its USB ID (`1209:0003`). Flashing it by manifest stays refused and
  is now refused by construction: a catalogue entry is split into a control
  half and an optional flashing half, and this board has none — its
  bootloader publishes `nRF52840-SeeedXiao-v1`, which is the XIAO module's
  identifier and not this product's, so any DIY XIAO with a differently
  wired radio answers to it. A bundle carrying an image for such a board
  fails to load, `--board solarnode` is refused before the bus is read, and a
  flash session that meets it on the bus names it, says why, and does not
  reboot it. It is flashed with `just flash-solarnode` (Codeberg #233).

- The C API serves a file and retires a handler. `lev_send_file_response`
  sends a response Resource of the RAW bytes plus msgpack metadata, with no
  `[request_id, response]` wrapper — the wire form a NomadNet `/file/`
  download has, and the one of the three response calls whose name does not
  say how it differs, so the doc comment says it instead. The requester reads
  the bytes with `lev_event_data` and the `{"name": ...}` with
  `lev_event_metadata` off one response event.
  `lev_deregister_request_handler` takes a served path back down without
  restarting the node, and answers which of the two things happened:
  `LEV_OK` when it removed a handler, the new `LEV_ERR_NO_HANDLER` (-16) when
  there was none. `lev_register_request_handler`'s doc no longer states that
  there is no unregister.

- `MessageState::Sending` is reported on `RouterEvent::MessageState` from
  every path that enters it — direct, opportunistic and propagated — so a
  message is observable while it is in flight instead of only when it is
  answered. On the transition only.
- lnmsg speaks to propagation nodes. `--via auto` (the new default)
  tries the direct delivery link first and, when none comes up inside
  the timeout, uploads the message to a propagation node instead — the
  fallback Columba's `tryPropagationOnFail` and Sideband perform. `--via
  direct` and `--via propagated` stay as explicit choices; the exit code
  distinguishes handed on directly (0), held by a node for collection
  (3), and neither (1), and the decision is logged as one structured
  `LNMSG_VIA method=… reason=…` line. Node selection: `--pn <hash>`,
  then the `propagation_node` key in lnmsg's own config
  (`${LNMSG_HOME}/config`), then the most recently announced node heard
  while attached; the client mines the propagation stamp at the node's
  announced cost with elapsed-time progress on stderr above cost 10.
  `lnmsg fetch` drains the mailbox with the genuine list/fetch/confirm
  round and prints what is new, de-duplicated across runs against
  messages already delivered directly (message ids persisted in
  `${LNMSG_HOME}/seen`); `lnmsg address` prints the persistent LXMF
  address without needing a daemon.

- Peering logs a refresh, not only an addition. Every announce that
  refreshes a peer already in the table emits `PN_PEER action=refresh
  reason=announce`, on the boards and in `lnpnd` alike and with the
  same field order in both. `action=add` is a once-per-peer-lifetime
  line — the table is persisted and restored across reboots and
  flashes — so until now nothing in the log said that a running node
  still hears its peers.

### Changed

- A board's announce cadence follows whether it moves, not a profile
  chosen when it was flashed. A board that moves announces every five
  minutes; one that does not falls back to one announce per hour, slow but
  never silent. Movement resuming, or the first announce heard from a
  destination this boot has not heard before, each buy one immediate
  announce rather than a raised cadence. Movement has to be proven, not
  measured once: the displacement must exceed the threshold across three
  consecutive fixes, the accuracy gate must be satisfied, and the fast
  state expires fifteen minutes after the last position confirming it, so
  a fixed board whose receiver wanders costs airtime at worst and never
  reachability. The board's own destination and the propagation role are
  decided together, because a board whose own announce is withheld is
  unreachable as a recipient while still usable as a mailbox.
- A board's own announces may use at most a tenth of its lawful duty
  budget. Locally originated announces bypass the transit announce cap by
  design, matching the reference, so nothing governed them; the board now
  stretches its configured interval until the announce set fits and writes
  one `[ANNOUNCE_DUTY]` line naming the configured interval, the interval
  actually used and the arithmetic that forced it. At a fast carrier this
  changes nothing; at the slowest, five minutes becomes roughly half an
  hour.
- A shared-instance client's announce goes out at once, as the reference
  does. The core held the first announce of every local-client destination
  for 250 ms to batch a start-up burst; wire format and semantics were
  untouched by it, but the deviation rule's third condition — a measurable
  improvement of priority 1 — never had a measurement, and collision
  avoidance belongs to the interface, which already spaces its own
  transmissions. Measured against `rnsd` with the same client on both
  stacks, 12 registrations each: median client-announce-to-on-air 1 ms for
  `lnsd` against 699 ms for the reference, whose job loop is what it waits
  on. A five-client simultaneous burst onto a half-duplex LoRa interface
  still reaches the radio complete and 51 ms apart, which is the
  interface's own spacing.

### Fixed

- A board holds back the announces it relays instead of repeating every
  one at once: its LoRa interface registers what a frame costs on the
  carrier it is running, so the announce bandwidth cap engages in the
  firmware too, and payload stops queueing behind other people's
  announces on a slow PHY (#402).

- An `lnsd` with an RNode interface holds back its transit announces
  whether or not the config spells out a `bitrate`: the interface now
  registers the announce-cap bitrate its live radio settings imply, which
  is what a Python `RNodeInterface` on the same channel has always done
  (#404).

- SF5 and SF6 are refused with a reason instead of being configured and
  then carrier-sensed against the SF7 threshold: no `cadDetPeak` for
  either is published in anything this tree holds, and a wrong
  detection threshold makes a node either transmit over a frame in the
  air or back off on noise (#350).

- A board's `[LORA] active config` line reports the transmit power it
  programmed, not the one it was asked for: `txp` is the effective dBm,
  beside `txp_requested` and a `txp_honoured=yes|no` that names a
  substitution in a word (#349).

- A board whose telemetry target is unreachable no longer announces at
  the tick rate. The telemetry tick puts the delivery announce on the
  air before the report exists, and the paths that give up after it —
  no readings, no path, an interface that is offline, a send the core
  refuses — booked nothing, so the attempt floor stayed where the last
  successful report left it and the next 5 s tick spent another
  announce. Measured at its worst as one announce every 6.7 s from a
  single node against a 60 s policy, roughly 20 % channel occupancy.
  Every one of those paths now books the airtime it spent
  (`SendPolicy::note_airtime_spent`), and the tick asks
  `SendPolicy::may_spend_airtime` at the spend itself. The report is
  still owed and still goes out on the first tick past the floor; only
  the retry rate changes. The `on_air()` branch that used to return in
  silence now says `report withheld reason=no-on-air`.

- A phone's rotated BLE address no longer strands it behind its own
  dead link (#360, board side). When an identity we already hold a
  link to handshakes on a new connection, the newer connection now
  wins in both roles — the old link is torn down as
  `BLE_LINK_REPLACED` and its queued packets move over — unless the
  old link carried real payload within one keepalive interval
  (`LINK_ACTIVE_DATA_MS`, 15 s), which still refuses the newcomer to
  protect a transfer in flight. Before, an outgoing-origin duplicate
  was refused unconditionally: the board dialled the rotated address,
  learned the same identity, refused its own dial and kept the
  abandoned link until the 45 s expiry, leaving the phone linkless
  ~45 s of every ~90 s rotation cycle. The rule (`judge_duplicate`)
  is shared by the firmware and lnsd, matches the reference driver's
  accept-newer behaviour (`ble-reticulum` `BLEInterface.py`,
  `_check_duplicate_identity`), and logs the consulted payload
  recency as `old_data_silence_ms=` on every duplicate line.

- Raw Link packet receipts are no longer failed at the literal
  `max(rtt × 6, 5 ms)` deadline. The reference computes that formula but
  only checks receipts once per second, so no Python receipt can fail
  that early; enforcing it exactly meant that on a 1 ms-RTT TCP link an
  upload to a Python propagation node was declared failed 6 ms after
  sending — while the node was still validating the PoW stamp before
  proving — and the LXMF layer then tore down the link the proof was
  about to arrive on, every retry alike. The enforced deadline is now
  floored at the reference's own check cadence
  (`RAW_RECEIPT_TIMEOUT_FLOOR_MS`, 1 s); LoRa-scale RTTs are unaffected.

- lnpnd grows to lxmd's full scope (#384 part 4). Remote management:
  the node registers lxmd's `lxmf.propagation.control` destination with
  the `/pn/get/stats`, `/pn/peer/sync` and `/pn/peer/unpeer` request
  paths behind the same identity allow list (`control_allowed`), and
  answers with the reference's stats map — `lxmd --status --peers
  --sync --break --remote` drives an lnpnd node, and lnpnd carries the
  same client verbs with output in lxmd's shape, driving stock lxmd
  nodes in return (both directions in the conformance corpus, the
  payload encodings pinned byte-exact against `umsgpack` in
  `VEC-PN-CONTROL`). The daemon's own mailbox: an LXMF delivery
  destination on the node identity, announced with `display_name` and
  `stamp_cost`, each received message written in the reference's
  packed-container file format and handed to the `on_inbound` hook;
  propagated uploads addressed to the node's own mailbox deliver
  locally instead of rotting in the store, the reference's own
  short-circuit. Configuration: an lxmd-format config directory
  (`config`, `identity`, `allowed`, `ignored`, `storage/`) with lxmd's
  sections and key names, flags as overrides, `--exampleconfig`, and
  `auth_required` gating `/get`; the keys lnpnd accepts but does not
  act on are named in lnpnd(1) with reasons. Packaging: lnpnd joins
  the `.deb` builds with a hardened systemd unit, a dedicated service
  user, `--service` file logging and a manual page. Propagation-stamp
  validation moved off the core lock onto a single worker thread
  (arrival order preserved — the proof still leaves only after the
  store append), so a cost-13 default no longer spends ~10 ms of the
  core mutex per accepted message; the validated sync-peer
  association is captured when the resource concludes, so a peer
  that tears its link down before the verdicts drain — stock lxmd
  does — cannot orphan the inbound batch.

- Propagation-node peering (#384 part 2): lnpnd now peers with other
  propagation nodes — including stock Python `lxmd` at its default
  peering cost — and syncs its store both ways. The protocol half lives
  in `leviculum-lxmf::peering` (`no_std + alloc`, board-ready behind a
  `PeerStore` trait): a capped peer table fed by propagation announces
  (autopeer within the configured hop depth, static list, deterministic
  first-heard-wins policy at the cap, 14-day unreachability cull), the
  `/offer` wire codec and inbound gate (peering-key validation at our
  announced cost, throttling, `from_static_only`), and one append-order
  cursor per peer instead of the reference's per-message peer sets —
  everything at or below the cursor has been offered and concluded, a
  stale cursor is a bounded full re-offer. Outbound rounds mine the
  peer's peering key once on a worker thread, persist it, offer within
  a 6144-byte bound, and advance the cursor only on the concluded
  resource. Messages accepted at cost 0 get true stamp values computed
  whenever any peer filters offers by value. New lnpnd flags use lxmd's
  key names: `--max-peers`, `--static-peers`, `--autopeer`,
  `--autopeer-maxdepth`, `--remote-peering-cost-max`,
  `--max-inbound-syncs`, `--from-static-only`. Structured events:
  `PN_PEER`, `PN_OFFER`, `PN_SYNC`. Verified end-to-end in an all-Rust
  two-node loopback (autopeer, key mine, offer, sync, re-offer answered
  "want none") and by new periculum conformance cells against genuine
  stock `lxmd` peers, including a mixed-stack relay chain in both
  directions, key reuse at stock cost 18, the peer cap, and store
  overflow during an inbound sync.

- lnpnd, an LXMF propagation node daemon (#384): the store-and-forward
  mailbox role on a running Reticulum shared instance. It announces
  `lxmf.propagation`, accepts client uploads (proving each packet only
  after the message is durably stored), answers `/get` with the
  list/fetch/confirm rounds of the reference protocol, and evicts by the
  reference's age-times-size weight. Stamp and peering costs default to 0
  and are settable (`--stamp-cost`, `--peering-cost`); announced limits
  default to the small honest values the propagation-node concept page
  derives (4 kB per transfer, 32 kB per sync). Verified byte-for-byte
  against the Python reference's own handlers and end-to-end against
  genuine Python LXMF clients.

- The propagation-node role and its store boundary in `leviculum-lxmf`
  (#384): `PropagationNode` (announce, upload accept, mailbox drain,
  eviction) over a `PropagationStore` trait whose verbs mirror the
  boards' record log one-for-one, so the on-board node of a later part is
  an adapter rather than a redesign. Host stores: in-memory, and a
  file-backed one that fsyncs before the upload proof leaves — a power
  cut during an upload reopens with every completed message and no
  partial one.

- The `lxmf-node` test helper speaks the propagation protocol (#384):
  `pn_enable` runs lnpnd's engine, and `set_pn` / `send_propagated` /
  `sync` drive the router's propagation client, mirroring the same verbs
  in periculum's Python helper so one driver tests either stack's node.

- LNode boards keep a message store on their internal flash (#384): 64 KiB
  behind the firmware image, mounted at boot and formatted once, not yet
  used for messages. `lnflash --store-storm <count>[,<bytes>]` appends
  synthetic records to it, which is the instrument for measuring what the
  store's page erases cost Bluetooth throughput and LoRa airtime.

### Changed

- The T114 no longer drives six pins as a flash bus (#384). It fits no
  QSPI part: Heltec's own board support package for HT-n5262 — the board
  id our bootloader reports — has the QSPI pins commented out, the
  sibling HT-n5262G variant gives two of them to the GPS reset and the
  display backlight, all six nets are on the expansion header where a
  user's own hardware sits, the two published pin maps disagree on
  IO2/IO3, and on the rig every pin followed our drive while nothing
  answered `9Fh`, `05h`, `90h` or a reset. The firmware now declares
  `qspi_part: None` for the board, never configures those pins, and says
  so once at boot instead of probing:
  `[QSPI] NONE board=t114 reason=not-fitted-see-boards-t114-rs`. The
  `[STG] qspi-init` stage marker is gone with the stage on that board.

- Neither board probes a QSPI flash any more, because neither of them
  carries one (#384): RAK's own board support package says "No onboard
  flash" over the RAK4631's QSPI pins and marks them "occupied by
  GPIO's", the `EXTERNAL_FLASH_DEVICES IS25LP080D` line under it being a
  template default rather than a fitted part — the same artefact as
  Heltec's, and the field Pocket answers `05h`, `9Fh`, `90h` and the
  datasheet reset exactly as silently as both T114s. The Pocket now
  declares `qspi_part: None` too, drops its aliases and its
  `[STG] qspi-init` stage, and says
  `[QSPI] NONE board=rak4631 reason=no-onboard-flash-see-boards-rak4631-rs`
  once at boot.

- A battery percentage that cannot be checked from far away is no longer
  sent (#380). The cell count is decided from one reading at boot and
  held for the boot, and every per-cell voltage after it is the pack
  voltage divided by it — so a wrong count halves or doubles the
  percentage, and the percentage carries neither a unit nor the count it
  was divided by. Both boards now report a charge percentage only while
  the measured pack voltage stays inside the band its classification
  implies; outside it the `BATTERY` line reads `pct=none`, the telemetry
  report carries no battery sensor at all, and the panel shows `--%`
  beside the voltage. The state change is said once, on its own line,
  `BATTERY_PCT reportable=0|1 pack_mv=<n> cells=<n>S band_lo_mv=<n>
  band_hi_mv=<n>`, rather than once per sample. The band comes from the
  OCV curve that already computes the percentage, so the guard and the
  percentage cannot disagree about what a cell is: its ceiling is the
  curve's 100 % point carried one step of its own top segment further
  (4.33 V per cell), and its floor sits deliberately BELOW the curve's
  floor, at the 2.5 V per cell protection cut-off — a pack between 2.5
  and 3.0 V is nearly empty, which is a real state that must report 0 %
  rather than go quiet exactly when the battery is about to give out.
  The voltage itself is never withheld: it is a measurement, not a
  derivation. Nor is the classification revised at runtime; this only
  declines to build on it.

- A second BLE connection from an identity a node already holds a link
  to is decided by who opened it (#376, #382). An INCOMING duplicate
  displaces the old link: a peer that opens a second connection has, by
  its own one-link-per-identity rule, given up on the first, and it has
  already built the replacement. An OUTGOING one — our own dial — is
  refused unless the old link has delivered nothing at all, payload and
  keepalives alike, for the link timeout (45 s); a node cannot recognise
  its own peer before connecting, because an advertisement carries no
  identity and phones rotate their address, so its fallback dial reaches
  a peer it is already linked to. The two 2026-09-09 field failures were
  the two directions: refusing an incoming duplicate cost a phone every
  announce, displacing on an outgoing one cost it its working link every
  ~95 seconds.

  The intervening rule displaced any link that had received no frame
  other than a keepalive for 30 seconds, and that fires on healthy
  links: measured beside a Columba phone over 14.1 hours, the gaps
  between received non-keepalive packets from a peer that was present
  throughout had a median of 51 s, a 90th percentile of 182 s and a
  maximum of 5590 s. Payload silence is what an idle phone looks like.
  Keepalives are what a live peer sends regardless, so liveness is now
  measured on every inbound frame, against the link timeout the
  interfaces already expire links on — one clock instead of two, shared
  by the firmware and lnsd. `BLE_LINK_DUP` says `action=refuse` or
  `action=displace` with `origin=` and the old link's
  `old_silence_ms`, on both stacks; `BLE_COUNTERS` carries `refused=`
  and `displaced=`, and a refused address enters the dead-end table so
  the scanner stops re-dialling it.

### Added

- A board asks for a supervision timeout it can survive (#385). A link
  a peripheral board holds is the one end that can do anything about
  the parameters it was handed, and the two cases where those
  parameters are bad or unknown are exactly the two where the board is
  the peripheral: an lnsd central gives it BlueZ's 420 ms — nine
  connection events at the 45 ms interval both stacks measured, and 36
  link deaths in 16.2 hours on the bench — and a phone gives it
  something nobody has measured yet. The board now compares the
  timeout the link came up with against a 2000 ms floor and, only if
  it falls short, asks the central for 4000 ms, the same value the
  firmware's own central role already asks for. Conditional by design:
  a central that negotiates something sane is left alone, because an
  update request that fights a good value is a regression. Both the
  decision and its outcome are logged as `BLE_CONN_PARAMS_REQ conn=<h>
  timeout_ms=<n> result=sent|refused|skipped`, and because a central
  may honour the request, ignore it, or answer with something else
  entirely, a peripheral link re-reads its parameters at teardown and
  repeats `BLE_CONN_PARAMS` marked `when=close`: a link that opened at
  420 ms and closed at 4000 ms was granted what it asked for, one that
  closed at 420 ms was not, and the request line alone says neither.
  Board-to-board links are untouched — their central already asks for
  4 s, so they only ever log `result=skipped`.

- The boards say what their battery is doing (#380). Once at boot and
  then every 30 s, both the Pocket V2 and the T114 log `BATTERY
  mv=<n> min_mv=<n> max_mv=<n> pct=<n>|none cells=<n>S`. The pack is
  sampled at 1 Hz and the line reports the extremes of the period as
  well as the filtered value, so a sag under transmit load is visible
  instead of averaged away. Before this the monitor fed the display and
  said nothing else: a Pocket V2 that restarted twice during a 90
  minute field walk on battery produced zero battery lines, so how
  close the pack had been to the edge could not be asked afterwards.
  This is a margin instrument, not a brownout detector — the sampler is
  a second apart, the transient that resets a board is microseconds
  wide, and the reset takes the log with it.

- The T114 reads its own battery at all (#380). The monitor was typed
  to the Pocket V2's ADC pin and had the Pocket's 1.73 divider baked
  into its arithmetic; it now takes the pin, the divider multiplier and
  the divider-enable pin from the board file, which was already
  declaring all three. The T114's enable pin is held high only for the
  duration of a sample, so its 490 kΩ divider does not sit across the
  pack between them. The pack voltage also appears on the T114's status
  panel, which used to render "(no feature)".

- Every BLE link on a board says what it actually runs at (#385). At
  the connection event, in both roles, the firmware logs
  `BLE_CONN_PARAMS conn=<h> role=central|peripheral interval_ms=<n.nn>
  latency=<n> timeout_ms=<n>` — the connection interval, slave latency
  and supervision timeout the central chose, since neither stack
  requests any of them. The supervision timeout is how long a
  disturbance may last before the link dies, and until now the only way
  to learn it was to read the peer's kernel: possible for a BlueZ
  central on the bench (where a board and lnsd a metre apart lost their
  link 36 times in 16.2 hours, every connection at a 45 ms interval
  with a 420 ms supervision timeout), impossible for a phone. Both time
  fields are converted from their two different raw scales (1.25 ms
  steps for the interval, 10 ms for the timeout) so the line carries
  milliseconds only. No parameter is requested or changed by this: it
  is a measurement. lnsd has no counterpart, because BlueZ publishes
  none of the three over D-Bus.

- A board announces itself on a new BLE connection and on a timer, not
  only before a telemetry report (#376). When a peer completes its
  identity handshake the node announces its delivery destination to
  that peer over that link alone, at most once per peer identity per 15
  minutes; independently of telemetry it announces on every interface
  every 30 minutes. Both are withheld while the board has no plausible
  wall clock, because an announce stamped from uptime can never replace
  a path at the receiver. A phone that connects between two reports now
  sees the board at one hop straight away.

- `lnflash --watch` records a board's debug log for field testing: it
  opens the debug CDC with DTR and RTS raised, prefixes every line
  with a wall-clock ISO-8601 timestamp, appends to `--out` flushed per
  line, and reconnects with bounded backoff when the port vanishes
  (reset, reflash, unplug), logging the gap as its own line. `lnflash
  --summarize <file>` reads a watch file back and prints receptions
  per class per hour (announce, data, path request) plus the last line
  seen per class.

- Every LoRa receive line says how strong the frame was, and what it
  was (#364). lnsd's RNode RX line and its `LORA_RX` trace event carry
  `rssi=`/`snr=` from the stat frames the RNode firmware indicates
  before each data frame, paired the way Python's `RNodeInterface`
  pairs them. The LNode firmware's `[LORA] RX` line adds `flags=` (the
  packet's header flags byte) and `dst=` (first 8 hex of the
  destination hash), so `lnflash --summarize` classifies announce,
  data, path request and proof straight from a watch file.

- An LNode now answers a Sideband telemetry request: an LXMF message
  from the configured target carrying the `TELEMETRY_REQUEST` command
  triggers an immediate report, rate-limited to one request-triggered
  report per profile minimum interval. Requests from any other sender
  are ignored and logged.

- `lnstatus --identities` (`-N`) lists every identity the daemon has
  learned from announces — identity hash, announced destination, name
  (only for aspects the daemon registered itself), hops, via and last
  seen — plus the `lxmf.delivery` and `rnstransport.probe` destinations
  derived from each identity hash, ready to paste into `lnprobe`. Until
  now that derivation had to be done by hand from `rnpath -t` output.
  Served by a new additive `identities` RPC verb on the shared-instance
  socket; a daemon without it (Python `rnsd`, older `lnsd`) closes the
  connection and the tool reports the error.

- `lnprobe`, a drop-in for Python's `rnprobe`: probes a destination
  through a running `lnsd` **or** `rnsd` over the shared instance,
  reporting round-trip time, hop count and packet loss from delivery
  proofs — the same command line, output and exit codes as the
  reference. The per-probe timeout asks the daemon for its first-hop
  timeout like `rnprobe` does, so probes over slow media wait longer by
  default.
- A board says which hashes to probe: one `[IDENTITY]
  identity=… probe=… lxmf=…` line on the boot-critical log path (both
  BSPs), repeated in the periodic banner so a reader attached later
  still sees it — and the same three hashes over a new control-envelope
  identity query (frame `0x0E`), which `lnflash --set-name` prints in
  its read-back, so no debug-port reader is needed.

- lnsd joins the Columba BLE mesh: a new `BLEInterface` type
  (`[[BLE Interface]]` config section) speaks the `ble-reticulum`
  protocol v2.2 with the v0.3.0 capability record over BlueZ, in both
  GATT roles at once — it advertises and serves the Columba GATT layout
  under an `LN-<hex8>` name derived from the daemon identity like the
  boards do, and it scans for and connects to peers under the same
  connection-direction rule, so a PC participates in the same BLE mesh
  as LNodes and phones. One interface is one broadcast domain across
  all live BLE links; fragmentation and the connection decision reuse
  the firmware's own host-tested code. Disabled unless configured.

- A user can set a fixed position on an LNode — `lnflash --set-position
  LAT,LON[,ALT]`, envelope frame `0x08` — which replaces the position
  sensor in its telemetry reports until `--clear-position` returns it to
  sensor reporting. Persisted beside the telemetry target, applied at
  boot and at runtime, marked `possrc=fixed|gnss` in the report line,
  and encoded in Sideband's own fixed-location shape (accuracy 0.01 m).

- The gap the LoRa interface leaves between two packets on the air is
  settable without a reflash — `lnflash --set-tx-spacing <MS>`, envelope
  frame `0x06`. The default imposes nothing (#345).
- The transmit power is settable without a reflash — `lnflash
  --set-tx-power <DBM>`, which reads the board's current radio settings
  back (envelope frame `0x07`) and returns them with only the power
  changed. Persisted (#349).
- A board states the transmit power it programmed, and whether the
  request was clamped, on its critical log path (#349).

### Fixed

- A battery reading can no longer be rescaled by a change nobody
  notices (#380). The conversion divided by a full scale of 3600 mV,
  which was true only because `ChannelConfig::single_ended` happens to
  default to the internal 0.6 V reference and a gain of 1/6; nothing in
  our code said so and nothing checked it, so an embassy release that
  moved either default would have shifted every reading on every board
  by a fixed factor and left all of them plausible. Both are now set
  explicitly, and the full-scale millivolts are derived from the
  configured gain rather than written down beside it, with host tests
  pinning what that gain makes of each board's divider.

- An implausible battery reading no longer classifies the pack as 2S
  (#380). The classifier's doc promised a warning on an out-of-range
  reading and the code had no way to give one. It mattered little at
  the Pocket V2's 6.2 V range and a great deal at the T114's 14.7 V:
  an unenabled divider or a floating input reads far above any pack,
  was called 2S, and then halved every per-cell voltage for the rest of
  the boot. Such a reading now falls back to 1S and says so.

- `lnflash` confirms a flash against a build line the board emitted
  *after* the reset the tool triggered, and no longer reports a good
  flash as a failed one (#378). It used to read a 12 s window off a
  bare `/dev/ttyACM` number and keep the last `[FW_BUILD]` in it, so a
  line already in the port's input queue — filled before the question
  was asked — could answer it: on 2026-09-09 a RAK4631 that was running
  the new firmware was reported as `WrongBuild { saw: "ead0bce" }`,
  the sha it had been running before the flash, and the run exited
  non-zero. Now the bootloader must be gone from the bus (a board still
  sitting in it never rebooted, which is `Absent`), the debug port is
  resolved to its `by-id` path and the open proved against the board's
  bus identity, the input queue is flushed, and only a complete line
  arriving afterwards counts — a half-read `git_sha=daa8b8e` parses as
  `daa8` and would be a failure manufactured out of a partial read. The
  budget is three banner periods (15 s); the firmware emits one every
  5 s, so a healthy board answers in the first. A confirmation that
  cannot decide says the running build is unknown and names no sha at
  all. The exit code now distinguishes the three: 0 confirmed, 1 the
  flash failed (no board back, or a different build named, or nothing
  written), 2 written but not read back.

- `lnflash --watch` no longer stamps a torn first line as evidence:
  the port's buffer can hold a partial line written before DTR was
  raised, gluing two board lines at the tear. Bytes up to the first
  newline after each (re)open are discarded and accounted on one
  `[WATCH] discarded partial first line (<n> bytes)` line.

- The `leviculum` .deb builds again: `lnprobe` was declared as a package
  asset but missing from the build script's binary list, so `cargo deb`
  aborted unable to resolve it. The binary now rides in the build, the
  packaging check asserts it (with its manual page), and the nightly
  binary tarballs carry it too.

- The LNode LoRa transmit path runs channel access on every key-up
  instead of only when a host set `csma_enabled`: a randomised, listened
  pre-TX jitter (the RNode firmware's idle-channel CSMA draw, DIFS plus
  0-13 slots) de-tiles senders whose transmissions share a trigger, and
  CAD listen-before-talk with the bounded retry gate runs regardless of
  the flag, whose backward-compat default of `false` had been switching
  all collision avoidance off. The jitter and backoff randomness is now
  seeded per board from the hardware RNG — the previous fixed seed made
  co-booted boards draw identical backoffs. The flag is still parsed and
  reported; the transmit path no longer obeys it.

- A TCP interface signals the hardware MTU `rnsd` signals, 16384, instead
  of the 262144 class constant. Python derives it from the interface
  bitrate at interface post-init, so the class value never reaches the
  wire; we read the constant. A Python client on an `lnsd` shared
  instance therefore negotiated a 262144-byte link MTU across a TCP hop
  where the same client gets 16384 from `rnsd`, and put frames on that
  hop at 32x the size any Reticulum 1.5.x peer accepts on its own receive
  path (#355).

- `rnstatus -l` reads the same link-table line off `lnsd` as it does off
  `rnsd`. The `link_count` and `active_link_count` RPC verbs answered with
  the links the daemon terminates; upstream they report the transport link
  table, i.e. the links it relays. An operator watching a relayed link saw
  "0 entries in link table" against `lnsd` and "1 entry in link table
  (1 active)" against `rnsd` for the same mesh state (#329).

- An LNode no longer panics and resets when a phone connects over BLE. The
  event buffer was left at its 128-byte default, and any peer negotiating a
  large ATT MTU overflowed it on its first full-size write (#354).

- An LNode transmits the power it was configured with. The requested
  value never reached `SetTxParams`, so only four powers were reachable
  and a configured 2 dBm went out at roughly 14 (#349).

- The LNode radio is listening again before a received frame is handed to
  the stack, instead of after it has been processed.
- The LoRa loop keeps a receive window that already has the parameters it
  wants instead of standing it down and arming an identical one, so a
  frame arriving 20 ms behind another is no longer ended mid-air by the
  loop's own next decision (#276).
- A transmit that would end a receive window holding an arriving frame
  waits for that frame first, bounded by one maximum-size frame's airtime
  at the live modulation, and delivers it (#276).
- A telemetry report whose dispatch was lost is retried no sooner than the
  policy's own `min_interval_ms`, instead of on the next main-loop tick
  (#344).
- The applied radio settings and the lawful duty-cycle cap the firmware
  derived from its frequency now survive a boot nobody was watching: both
  are emitted on the log path that bypasses the debug port's runtime gate,
  at bring-up and on every reconfiguration.
- A board states the airtime limits it is enforcing and who chose each of
  them on every boot, instead of only when it derived the cap itself — an
  explicit host `0` used to switch the cap off silently.
- A board says when it could not transmit at the power it was given,
  naming the request and the power the PA was actually programmed with —
  a request below 14 dBm rounds up, and the line reporting it used to be
  dropped on a boot nobody was watching (#349).
- The citation guard reads the `` (`ident`, `path:line`) `` spelling as
  naming its subject, so 130 citations that were existence-checked are
  drift-checked; 30 that had drifted are corrected.

### Changed

- Every firmware debug line ends in `t=<uptime-ms>`, stamped on the board
  when the line is formatted, so a capture measures the board and not the
  USB drain loop (#344).

- The firmware's outbound LoRa queue holds 64 packets or 6 KiB, whichever
  binds first, instead of four packets; a refusal names which bound it hit
  (#344).

### Added

- The firmware logs every arming of the receiver as `[SX_RX_ARM] site=
  timeout_ms= dark_ms=`, so a capture says how long the radio was not
  listening between two windows instead of leaving it to be inferred (#344).

- Standing the receiver down logs `[SX_RX_TEARDOWN] site= preamble=
  header= rxdone= armed_ms=` from every caller, `site=` naming the caller,
  so a capture says whether a frame was already arriving when the window
  came down (#276).

- A receive window that is kept instead of re-armed logs `[SX_RX_ADOPT]
  latched= preamble= header= rxdone= stood_ms=`, which counts the
  receptions the previous firmware destroyed.

- A transmit that waits for an arriving frame logs `[SX_TX_DEFER]
  waited_ms= reason= outcome=`, so the airtime the wait costs and what it
  bought are one ratio in the capture (#276).

- The firmware receives at boosted SX1262 gain and applies the errata-15.4
  IQ correction, and prints both registers before and after it writes them
  (`[SX_REG]`, `[SX_REG_IQ]`), so the change is visible in a capture rather
  than taken on trust (#258).

- The nRF firmware prints its transport counters every 30 s as
  `[TRANSPORT] fwd= rx= tx= nopath= dup= overheard= maxhops= paths=`, so
  a board that does not relay a packet says which decision discarded it
  (#344).

- A transport-id mismatch names both ids it compared, and a packet
  dropped that way for a destination this node serves locally is
  reported per packet instead of only counted (#344).

- The lnflash bundle carries the RAK4631 as well as the T114, so a
  WisMesh Pocket V2 can be flashed and configured from the tarball
  (#261).

- `just nrf-shellcheck` runs shellcheck over the flash-runner scripts and
  is part of `just fast` (#345).

- The Pocket V2 reports its position and battery over LXMF: an
  announced `lxmf.delivery` destination, a target set by address alone
  (the node resolves the key over the air and says `awaiting-key` until
  it has), tracker and station cadence profiles, and one immediate
  report when a target becomes usable (#236).

- Every published artifact now carries `THIRD-PARTY-NOTICES`, generated
  from the lockfiles, so the MIT- and BSD-licensed crates linked into
  the binaries travel with their required notices (#288).

- The GNSS wake ends with an explicit UBX-CFG-ANT step: antenna supply
  on, every automatic power-down path off, so the init no longer
  depends on what the factory clear left behind (#324).

- The GNSS heartbeat reports satellites in view and best C/N0 (`sv=`,
  `cno=`) from GSV, so a receiver that hears the sky but never fixes is
  distinguishable from a deaf antenna (#324).

- The Pocket V2 wakes its GNSS module at boot with a minimal UBX init
  (factory clear, cold start, full power), so a persisted module
  configuration from earlier firmware cannot suppress acquisition (#324).

- One framed control envelope on the LNode USB channel (type, length,
  named refusals, capability report); radio config and reset migrated,
  legacy magics stay accepted for a transition window (#238).
- `lnflash --set-time` teaches a running LNode wall time over the
  envelope; the banner then reports `[TIME_SOURCE] source=host` (#166).
- The Pocket V2 firmware seeds its calendar from the GNSS receiver's RMC
  UTC, and every LNode states its time source (`[TIME_SOURCE]` beside
  `[FW_BUILD]`) (#166).
- Single-destination decrypt misses are now counted (`single-decrypt-fail`
  in `PKT_DROP_SUMMARY`) and journey-logged instead of dropped silently.

- `Destination::with_explicit_hash`: a Single destination indexed by a
  caller-supplied 16-byte hash; never announced, reachable by direct link
  only (#254).
- Driver completion futures (`connect_awaited`, `send_resource_awaited`,
  `send_request_awaited`) and a bounded multi-consumer event tap, replacing
  consumer poll loops (#253).
- `lnmsg`, a new LXMF messenger: `lnmsg send <address>` queues one message
  through a running `lnsd`/`rnsd` shared instance and says nothing. Exit 0
  means queued, never delivered.

### Changed

- The GNSS wake no longer forces a UBX-CFG-RST cold start on every boot,
  so a reboot keeps the module's assistance data and refixes in seconds
  instead of re-downloading the sky (#324).

### Removed

- The host-side airtime gate on the RNode interface (#121). Duty-cycle
  enforcement is the firmware's; the host no longer holds packets back
  when it sees the firmware's lock in `CMD_STAT_CHTM`.

### Fixed

- A frame the firmware could not hand to an interface is no longer lost
  in silence: `DispatchResult` is `#[must_use]`, every call site reports
  what it lost, an action addressed to an unknown interface is counted
  as `no-such-interface` instead of vanishing, and a full outbound queue
  says so at the interface (#344).

- A telemetry report the dispatch lost no longer counts as sent, so the
  next tick reports again instead of the node going quiet for a whole
  cadence interval (#344).

## [0.8.1] - 2026-08-16

### Added

- `no_std` Telemeter codec in `leviculum-lxmf`: encode/decode of Sideband's
  `FIELD_TELEMETRY` sensor map and `FIELD_TELEMETRY_STREAM` rows, with
  golden vectors verified against Sideband and Columba (#237).
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
- `api::NodeBuilder` installs a `CoreProcessor` and re-exports
  `TickOutput`, so the facade covers the processor seam (#222, PAzter1101).
- The LNode debug port replays the panic count and stored post-mortem
  block on demand.

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
- Announce verify and single-destination decrypt run off the node lock
  (#29 stages 2-3; #243, emoore); the verified memo is discarded when
  the IFAC strip rewrites the bytes, and ratchet enforcement is read
  live at the consume site.

### Fixed

- `lnflash --set-time` and `--set-telemetry` run without a firmware
  bundle on disk: the board catalogue is compiled into the binary and
  only the flashing paths need images (#342).

- The flash runner reads the firmware back off the board before it names
  one, so the summary reports which board actually received the image
  instead of whichever candidate its USB enumeration reached first
  (#343).

- The flash runner enumerates every attached bootloader volume and picks
  the one whose `Board-ID` matches, instead of taking the first it finds,
  so a board parked in its bootloader no longer blocks flashing every
  other board; volumes it mounts are always unmounted again (#341).

- The BLE interface waits for the SoftDevice notification queue to
  drain instead of discarding the refusal, so a packet larger than one
  fragment — every announce — reaches the peer; what still cannot be
  sent is reported as `BLE_TX_DROP` (#264).

- A relay forwards packets whose context byte it does not know instead
  of dropping them at parse time; only local delivery abstains, counted
  as `unknown-context` (#332).

- Discovery announces are minted at stamp value 16, so RNS 1.5.0
  listeners no longer discard them; the receive gate stays at 14 so
  1.3.5 neighbours still decode (#328).

- A transport relay forwards path-directed packets back onto the
  receiving interface, so multi-hop over a single shared LoRa channel
  delivers (A-B-C repeater).
- Shared-medium relays no longer echo-storm a link: a link request is
  transported only by its designated hop, link DATA repeats are
  deduplicated, and same-interface echoes drop as `LinkRepeatEcho`
  (#226, #227).
- A local client no longer hears an echo of every link data packet it
  sends through its relay (#226).
- A relayed link's timeout is a rolling inactivity window: the
  link-table entry refreshes on every repeat, so a held link survives
  past 15 minutes (#226).
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
- COMPAT: HKDF derives past the RFC 5869 block limit, as Python does
  (#225, PAzter1101).
- A channel send past the u16 envelope wire ceiling is refused instead
  of panicking with the node lock held (#242, emoore).
- The 1200-baud flash touch writes GPREGRET through SoftDevice
  syscalls (#249), and the RNG never falls back to peripheral
  registers while the SoftDevice runs (#250).
- The repository checks out on Windows: no ':' in committed fixture
  paths (#244/#245, emoore); the supervised-spawn probe builds off
  Linux (#246, emoore).

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
