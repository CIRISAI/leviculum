# The link and resource layers, read against the reference (2026-10-12)

An audit in the shape of the RPC audit (2026-10-11) and the air-path
audit: our link layer (`leviculum-core/src/link/`, the node-side
handlers in `leviculum-core/src/node/link_management.rs`) and resource
layer (`leviculum-core/src/resource/`) read line by line against
Python-RNS `Link.py`, `Resource.py`, `Channel.py` and `Buffer.py`,
every divergence pinned to a line on both sides. Nothing was changed;
this document is the deliverable.

Reference tree: `reference/Reticulum` (RNS 1.3.5 per
`reference/Reticulum/RNS/_version.py:1`). Where our code deliberately
pins post-1.3.5 behaviour (the #330 hop rebalance pins 1.5.0) this
audit takes the pin as given. Every `path:line` below is checked by
the citation guard on each run.

Each divergence names: ours and theirs by line, what a peer on the
mesh (or a tool) sees, a class — *wire*, *semantic*, *timing*,
*deviation ours (deliberate)*, *inert* — and the test that would pin
it. The Leitstern context: every Columba message and every propagation
sync is a link plus a resource, so these two layers are where a board
and a Python phone must agree.

## 1. Link establishment

Where we match, verified: the link request payload is the two
ephemeral public keys, 64 bytes, or 67 with the 3-byte MTU/mode
signalling (`create_link_request_with_mtu`,
`leviculum-core/src/link/mod.rs:1295`, against `request_data`
composition at `reference/Reticulum/RNS/Link.py:316` and the two
accepted sizes in `validate_request`,
`reference/Reticulum/RNS/Link.py:186`); the signalling encoding is the
21-bit MTU plus 3-bit mode (`encode_signaling_bytes`,
`leviculum-core/src/link/mod.rs:80`, against `signalling_bytes`,
`reference/Reticulum/RNS/Link.py:150`); the link id is the truncated
hash over the masked flags byte plus the packet body with the
signalling stripped (`calculate_link_id`,
`leviculum-core/src/link/mod.rs:1977`, against
`link_id_from_lr_packet`, `reference/Reticulum/RNS/Link.py:341`), and
one id regardless of header type; the proof is
`signature + x25519_pub + signalling` signed over
`link_id + pub + sig_pub + signalling` with the destination identity
key (`build_proof_packet`, `leviculum-core/src/link/mod.rs:794`,
against `prove`, `reference/Reticulum/RNS/Link.py:371`); the 96-byte
legacy proof without signalling is accepted and verified over the
80-byte prefix (`LINK_PROOF_LEGACY_SIZE`,
`leviculum-core/src/link/mod.rs:72`, against the two shapes gated at
`reference/Reticulum/RNS/Link.py:404` and `:410`); only
`MODE_AES256_CBC` is accepted (`validate_mode`,
`leviculum-core/src/link/mod.rs:102`, against `ENABLED_MODES`,
`reference/Reticulum/RNS/Link.py:133`); the link MDU formula is
Python's to the byte (`compute_link_mdu`,
`leviculum-core/src/link/mod.rs:126`, against `update_mdu`,
`reference/Reticulum/RNS/Link.py:530`, 431 at the default MTU); an
inbound request's signalled MTU is clamped to the receiving
interface's HW MTU exactly as Transport does it (`new_incoming`,
`leviculum-core/src/link/mod.rs:657`); the initiator adopts the
responder's confirmed MTU verbatim with the base-MTU fallback for a
missing value (`process_proof`, `leviculum-core/src/link/mod.rs:2031`,
against the `confirmed_mtu or RNS.Reticulum.MTU` adoption,
`reference/Reticulum/RNS/Link.py:427`); the RTT packet is the
link-encrypted msgpack float64 (`build_rtt_packet`,
`leviculum-core/src/link/mod.rs:2606`, against the `rtt_data` packb at
`reference/Reticulum/RNS/Link.py:440`); the keepalive derivation is
Python's formula (`calculate_keepalive_from_rtt`,
`leviculum-core/src/link/mod.rs:1454`, against
`reference/Reticulum/RNS/Link.py:845`, constants `KEEPALIVE_MAX_RTT`
at `reference/Reticulum/RNS/Link.py:83`); and the identify step is
byte-identical both ways — initiator-only, 128-byte
`pubkey + signature` over `link_id + pubkey` (`identify_link`,
`leviculum-core/src/node/mod.rs:1363`, and `handle_link_identify`,
`leviculum-core/src/node/link_management.rs:1930`, against `identify`,
`reference/Reticulum/RNS/Link.py:459`, and the LINKIDENTIFY arm,
`reference/Reticulum/RNS/Link.py:1010-1031`), blackhole teardown
included (`reference/Reticulum/RNS/Link.py:1021-1023`).

The hop count the link carries (643): the responder freezes
`packet.hops` into the link
(`leviculum-core/src/node/link_management.rs:1012`), the initiator
freezes the path table's count at `connect` time
(`set_hops`, `leviculum-core/src/node/link_management.rs:307`) and re-adopts the
validated proof's count once, path entry included
(`leviculum-core/src/node/link_management.rs:1197-1215`). That
rebalance is a deliberate 1.5.0 pin (`ALLOW_LINK_PATH_REBALANCE`);
1.3.5 keeps the frozen count. Both ends then expose the count to the
establishment timeout exactly as the reference does.

**D1 — the responder's pending-link patience is 54 s where the
reference's is 360 s.** Python arms an incoming link with
`ESTABLISHMENT_TIMEOUT_PER_HOP * max(1, hops) + KEEPALIVE`
(`establishment_timeout`, `reference/Reticulum/RNS/Link.py:207`, with
`KEEPALIVE_MAX` = 360 at `reference/Reticulum/RNS/Link.py:92`). Ours
substitutes a 54 s bonus (`ESTABLISHMENT_RESPONDER_BONUS_MS`,
`leviculum-core/src/constants.rs:286`, applied in
`establishment_timeout_ms`, `leviculum-core/src/link/mod.rs:1131`),
sized to cover our own initiator's RTT-retry budget (D3) with margin.
What a peer sees: a Python initiator whose LRRTT packet takes longer
than `6s×hops + 54s` to arrive (an extreme multi-hop LoRa path, or an
initiator that sleeps between proof and RTT) finds the link gone —
silently, since a pending teardown sends no LINKCLOSE on either stack
(`teardown` only sends when not PENDING,
`reference/Reticulum/RNS/Link.py:704`). Class: **deviation ours
(deliberate), timing**. Test: mvr — responder proves, clock advances
55 s, the late LRRTT is refused; the same scenario against `rnsd`
still establishes.

**D2 — establishment retries with fresh keys, and a jittered
timeout.** Python never retries a link request internally; the
application does. Ours retries up to `max(LINK_REQUEST_MAX_RETRIES,
hops)` times, regenerating the ephemeral keys so the retry survives
duplicate-hash dedup, re-keying the link table and aliasing old ids
(`check_timeouts`, `leviculum-core/src/node/link_management.rs:3483`,
re-key at `:3396`), and every attempt draws a fresh +0..25 % timeout
jitter (`MAX_ESTABLISHMENT_JITTER_PERMILLE`,
`leviculum-core/src/constants.rs:298`). On the wire each retry is
indistinguishable from a new Python `RNS.Link`, and the jitter never
reaches the air. Class: **deviation ours (deliberate), inert on the
wire**. Pinned by the existing #66/#129 tests.

**D3 — RTT packet retransmits, and what they do to a Python
responder.** Python sends LRRTT once
(`reference/Reticulum/RNS/Link.py:440-442`). Ours retransmits it up to
`RTT_RETRY_MAX_ATTEMPTS` = 5 times at `max(3×rtt, 10 s)` until any
inbound packet confirms the responder is alive (`check_rtt_retry`,
`leviculum-core/src/node/link_management.rs:3716`; `needs_rtt_retry`,
`leviculum-core/src/link/mod.rs:1520`) — the fix for the
LRRTT-lost-on-LoRa establishment hole. The side effect on a Python
responder: its LRRTT arm has **no state gate** (`rtt_packet` is called
whenever `not self.initiator`, `reference/Reticulum/RNS/Link.py:1059`),
and `rtt_packet` re-measures `time() - request_time`, takes
`max(measured, wire)` (`reference/Reticulum/RNS/Link.py:540`),
recomputes keepalive from it, and **re-fires the destination's
`link_established` callback** (`reference/Reticulum/RNS/Link.py:550`)
on every duplicate. A retry 10 s after establishment therefore sets
the Python responder's RTT to ~10 s, which pushes its keepalive to the
360 s ceiling and its stale window to 720 s, and runs the peer
application's established-callback a second time. Our retries stop at
the first inbound packet, so against a responder that transmits at all
this stays theoretical; on a one-way-usable path it is real. Class:
**semantic, deviation ours (deliberate) with an undocumented side
effect on the peer**. Test: drop-in A/B — `rnsd` responder behind a
return path that drops the first LRRTT; count `link_established`
invocations and read the responder's RTT; ours should confirm-and-stop
after one retry.

**D4 — the responder takes the initiator's RTT verbatim; the
reference takes the max with its own measurement.** Python:
`self.rtt = max(measured_rtt, rtt)` (`rtt_packet`,
`reference/Reticulum/RNS/Link.py:540`). Ours decrypts, rejects
non-finite and non-positive values, clamps at 3600 s, and stores the
wire value (`process_rtt`, `leviculum-core/src/link/mod.rs:880`). An
initiator that understates its RTT (buggy clock) gives our responder a
too-short keepalive/stale window where Python's own measurement would
floor it; the clamps bound the damage. Class: **timing**. Test: unit —
RTT packet carrying 1 ms against a responder whose proof→RTT gap was
2 s; assert the stale window, compare the reference's.

**D5 — integer truncation in the derived timers.** For the
instruction's example RTT of 500 ms: the reference derives keepalive
102.857 s, stale 205.714 s, and tears down a stale link after
`stale + 4×rtt + STALE_GRACE` ≈ 212.7 s (`STALE_GRACE`,
`reference/Reticulum/RNS/Link.py:88`, watchdog arm
`reference/Reticulum/RNS/Link.py:796-798`). Ours logs — and the
`LINK_ESTAB` line in the instruction shows —
`keepalive_s=102 stale_s=204 inactivity_timeout_ms=211000`: the
keepalive clamps to whole seconds (`calculate_keepalive_from_rtt`,
`leviculum-core/src/link/mod.rs:1454`, the trailing `as u64`), stale is
`2×102`, and the close threshold `stale_close_timeout_secs`
(`leviculum-core/src/link/mod.rs:1443`) lands 1.7 s earlier than the
reference's. Both ends keep their own clocks, nothing crosses the
wire. Class: **timing, inert**.

## 2. Link maintenance

Where we match, verified: only the initiator sends keepalives, 0xFF,
unencrypted (`build_keepalive_packet`,
`leviculum-core/src/link/mod.rs:2654`, against the KEEPALIVE plaintext
arm of Packet.pack, `reference/Reticulum/RNS/Packet.py:206-208`);
the responder echoes 0xFE and only 0xFE
(`process_keepalive`, `leviculum-core/src/link/mod.rs:2780`, against
the echo arm, `reference/Reticulum/RNS/Link.py:1152-1156`); keepalives
refresh the inbound clock but not the data clock
(`record_keepalive_inbound`, `leviculum-core/src/link/mod.rs:1568`,
against `last_data` gating, `reference/Reticulum/RNS/Link.py:979`);
stale is `2 × keepalive` of inbound silence and recovery is any valid
inbound packet (`try_recover_stale`,
`leviculum-core/src/node/link_management.rs:2557`, against
`reference/Reticulum/RNS/Link.py:983-984`), validated data proofs and
resource proofs included (#124, matching the
`max(last_inbound, last_proof, ...)` at
`reference/Reticulum/RNS/Link.py:789`); the close packet is the
link-encrypted link id, accepted from Active and Stale
(`process_close`, `leviculum-core/src/link/mod.rs:2746`, against
`teardown_packet`, `reference/Reticulum/RNS/Link.py:710`); and a
teardown fails both in-flight resources toward the application
(`remove_link`, `leviculum-core/src/node/link_management.rs:525`,
against `link_closed` cancelling both lists,
`reference/Reticulum/RNS/Link.py:725-726`). Our close *reasons* are a
richer local enum (`LinkCloseReason`,
`leviculum-core/src/link/mod.rs:277`) than Python's three
(`reference/Reticulum/RNS/Link.py:116-118`) — local only, nothing on
the wire carries a reason. Pending requests on a closed link are
dropped without individual timeout events
(`remove_pending_requests_for_link`,
`leviculum-core/src/node/link_management.rs:4335`); Python lets each
`RequestReceipt` time out on its own clock
(`reference/Reticulum/RNS/Link.py:1415-1422`). Inert, local API.

**D6 — our keepalive is periodic; the reference's is idle-gated.**
Python sends a keepalive only when the link has been inbound-idle a
whole keepalive interval AND one interval has passed since the last
keepalive (`reference/Reticulum/RNS/Link.py:792-794`). Ours gates on
`last_keepalive` alone (`should_send_keepalive`,
`leviculum-core/src/link/mod.rs:1545`), so an initiator emits one
keepalive per interval no matter what traffic flows. This is a
deliberate superset — the test at
`leviculum-core/src/link/mod.rs:3771`
(`silent_initiator_keeps_sending_keepalives_while_inbound_is_fresh`)
pins why: a one-way-receiving initiator under the reference's gate
would let the *peer's* inbound clock starve. What a peer sees: one
extra ~20-byte frame per 102 s per link even under active traffic —
airtime a LoRa cell pays for. Class: **deviation ours (deliberate),
timing**. Test: already pinned; the airtime side would be an A/B frame
count over an idle hour, lnsd vs rnsd.

**D7 — inbound link packets are not pinned to the attached
interface.** The reference refuses any link-addressed packet that
arrives on a different interface than the link is attached to, loudly
(`reference/Reticulum/RNS/Link.py:975-977`: "Someone might be trying
to manipulate your communication!"). Our dispatch has no interface
check at all — `handle_link_data`
(`leviculum-core/src/node/link_management.rs:1439`) never receives the
arrival interface. HMAC covers every encrypted context, but keepalives,
resource parts (bulk-encrypted slices with no per-packet tag,
`handle_resource_data`,
`leviculum-core/src/node/link_management.rs:2940`) and resource proofs
are not per-packet authenticated: a node bridging two of our
interfaces can replay them cross-interface and we accept, where Python
drops. It also means a link whose path genuinely moved keeps working
here and goes dead on a Python node — a robustness *gain* with a
hardening cost. Class: **semantic**. Test: two-interface mvr — capture
a resource part on iface A, inject on iface B; ours currently accepts,
the reference refuses.

**D8 — keepalive accounting is looser than the reference's by two
small steps.** (a) We refresh the inbound clock before validating the
keepalive byte (`handle_keepalive_packet`,
`leviculum-core/src/node/link_management.rs:1589`), so a malformed or
wrong-direction keepalive still feeds the stale watchdog; Python
validates direction first and, for an initiator, excludes an inbound
0xFF *entirely* — it never even counts as inbound
(`reference/Reticulum/RNS/Link.py:974`). On a broadcast medium that
echoes frames back (BLE bridge, misconfigured repeater), our initiator
can keep a dead link "fresh" on its own reflected keepalives. Class:
**timing**. Test: unit — feed an initiator its own 0xFF; assert the
inbound clock did not move.

## 3. Requests and responses over a link

Where we match, verified: the request is the link-encrypted msgpack
`[timestamp, path_hash, data]` fixarray (`send_request`,
`leviculum-core/src/node/mod.rs:1457`, against the request composition
into `unpacked_request`, `reference/Reticulum/RNS/Link.py:490`); the
packet-request id is the truncated packet hash
(`leviculum-core/src/node/mod.rs:1522`, against `getTruncatedHash`,
`reference/Reticulum/RNS/Link.py:1035`) and the resource-request id
the truncated hash of the packed request
(`send_request_resource`, `leviculum-core/src/node/mod.rs:1558`,
against `reference/Reticulum/RNS/Link.py:515`); the default timeout is
`6 × rtt + 11.25 s` on both sides (ours
`leviculum-core/src/node/mod.rs:1518-1523`; Python
`rtt*TRAFFIC_TIMEOUT_FACTOR + RESPONSE_MAX_GRACE_TIME*1.125`,
`reference/Reticulum/RNS/Link.py:494`); the handler auth ladder is
none/all/allow-list with the allow-list keyed on the identified remote
(`handle_request_payload`,
`leviculum-core/src/node/link_management.rs:2118`, against
`reference/Reticulum/RNS/Link.py:867-873`); the response is
`[request_id, response]` as a packet up to the link MDU and a response
resource beyond it, correlated only on the link that owns the pending
request (`handle_response_payload`,
`leviculum-core/src/node/link_management.rs:2284`; response-resource
acceptance gated on the pending request,
`leviculum-core/src/node/link_management.rs:2681-2704`, against the
pending-request scan at `reference/Reticulum/RNS/Link.py:1075-1088`);
and a completed response resource is delivered as the response — the
metadata/file shape included — not as a generic transfer
(`response_delivery`,
`leviculum-core/src/node/link_management.rs:3072-3106`, against
`response_resource_concluded`,
`reference/Reticulum/RNS/Link.py:939-954`).

**D9 — the packet-vs-resource response choice is the caller's, not
automatic.** Python's `handle_request` packs the response and falls
back to a response resource by size itself
(`reference/Reticulum/RNS/Link.py:898-901`). Ours surfaces
`RequestReceived` and the application calls `send_response`
(`leviculum-core/src/node/mod.rs:1673`, which refuses over-MDU with
`PayloadTooLarge` before allocating) or `send_response_resource`
(`leviculum-core/src/node/mod.rs:1898`). Same threshold (the link
MDU), same wire bytes once chosen. Class: **inert (local API)** — but
every responder we ship must implement the fallback, and a consumer
that forgets it answers large requests with nothing. Test: the
existing request/response integration tests cover both arms; a lint
would be the durable fix.

**D10 — the request timer is progress-based, the reference's is
absolute.** Python arms one timeout at send and fires it regardless of
what else the link carries (`__response_timeout_job`,
`reference/Reticulum/RNS/Link.py:1415`). Ours resets the pending
deadline whenever the link shows inbound progress — resource ADVs and
parts included — and hands liveness to the resource watchdog while a
response resource is in flight
(`reset_pending_requests_on_link`,
`leviculum-core/src/node/link_management.rs:2415`;
`reconcile_request_resource_outcomes`,
`leviculum-core/src/node/link_management.rs:2453`). Rationale is bug
#26: on LoRa the Response packet queues behind resource data and the
absolute timer fired mid-transfer. What a peer sees: nothing — purely
our clock. What an operator sees: our `fetch` survives where `rncp`'s
times out. Class: **deviation ours (deliberate), timing**.

**D11 — split request/response resources are refused.** Python can
segment *any* resource, request and response bodies included (the
split machinery at
`reference/Reticulum/RNS/Resource.py:299-313` is shape-agnostic).
Ours rejects a request/response ADV that is split or not segment 1/1
(`leviculum-core/src/node/link_management.rs:2665-2674`) and refuses to
build one past `RESOURCE_MAX_EFFICIENT_SIZE` on the sender side
(`ensure_single_segment_internal_resource_size`, called at
`leviculum-core/src/node/mod.rs:1612`). What a peer sees: a Python
node answering our request with a >1 MiB (pre-compression) response
resource gets its ADV silently ignored, re-advertises through its
budget, and the request dies as a timeout on our side with nothing
naming the cause. Class: **semantic (bounded)**. Test: drop-in — a
Python responder serving a 1.2 MiB response to `lnstest`/`rnstatus -R`
style request against both daemons.

## 4. Resources

Where we match, verified: the advertisement is the msgpack fixmap(11)
with Python's keys, order, flag bits and nil-able `q`
(`ResourceAdvertisement::pack`,
`leviculum-core/src/resource/mod.rs:558`, against `pack`,
`reference/Reticulum/RNS/Resource.py:1333`, flags at
`reference/Reticulum/RNS/Resource.py:1307`; byte-for-byte vector
pinned in `test_advertisement_python_interop_vector`,
`leviculum-core/src/resource/mod.rs:990`); parts are the bulk-encrypted
stream cut at the resource SDU, sent raw in RESOURCE context
(`build_raw_data_packet`, `leviculum-core/src/link/mod.rs:2482`,
against the RESOURCE arm of Packet.pack,
`reference/Reticulum/RNS/Packet.py:202-204`); the REQ wire format is
`[flag][last_map_hash?][resource_hash][N×4 map hashes]`
(`build_request`, `leviculum-core/src/resource/incoming.rs:386`,
against `request_next`, `reference/Reticulum/RNS/Resource.py:931`);
`HASHMAP_MAX_LEN` is 74, derived from the *standard* link MDU and not
the negotiated one (`HASHMAP_MAX_LEN`,
`leviculum-core/src/resource/mod.rs:236`, against
`reference/Reticulum/RNS/Resource.py:1236`), the collision guard 224
(`COLLISION_GUARD_SIZE`, `leviculum-core/src/resource/mod.rs:241`,
against `reference/Reticulum/RNS/Resource.py:1237`); the HMU is
`resource_hash + msgpack([segment, hashmap])` with checked segment
arithmetic on our side (`handle_hashmap_update`,
`leviculum-core/src/resource/incoming.rs:624`, against
`hashmap_update_packet`, `reference/Reticulum/RNS/Resource.py:483`);
the part count is derived from the advertised transfer size, never
from the peer's `n` (`from_advertisement`,
`leviculum-core/src/resource/incoming.rs:245`, the derive at
`leviculum-core/src/resource/incoming.rs:290`, against
`reference/Reticulum/RNS/Resource.py:187`); the proof is
`resource_hash + full_hash(data + resource_hash)`
(`build_proof`, `leviculum-core/src/resource/incoming.rs:865`, against
`prove`, `reference/Reticulum/RNS/Resource.py:752`), cached and
re-served on an unencrypted CACHE_REQUEST (`handle_cache_request`,
`leviculum-core/src/node/link_management.rs:2368`, against the
cache-request plaintext arm,
`reference/Reticulum/RNS/Packet.py:210-211`); cancellation is ICL from
the sender — ours also sends it when the sender's own watchdog gives
up — and RCL from the receiver, rejection included (`reject_resource`,
`leviculum-core/src/node/mod.rs:2243`, against `reject`,
`reference/Reticulum/RNS/Resource.py:155`, and `cancel`,
`reference/Reticulum/RNS/Resource.py:1075`); segmentation at
`MAX_EFFICIENT_SIZE` with the shared `o`/`d`/`l` fields and
next-segment-on-proof (`OutgoingSegmentPlan`,
`leviculum-core/src/resource/outgoing.rs:1517`, against
`validate_proof` advertising `next_segment`,
`reference/Reticulum/RNS/Resource.py:804-821`); metadata as the 3-byte
length prefix, stripped on segment 1 only
(`leviculum-core/src/resource/incoming.rs:820`, against
`reference/Reticulum/RNS/Resource.py:696-704`); and the salted
`original_hash` (never a content hash — Codeberg #165;
`leviculum-core/src/resource/outgoing.rs:914`, against
`reference/Reticulum/RNS/Resource.py:445-446`).

The sender's three watchdogs now mirror the reference's shapes: the
part-request budget is the global
`6×rtt×16 + 10 s + Σ extra` (`sender_part_budget_ms`,
`leviculum-core/src/resource/outgoing.rs:306`, against
`reference/Reticulum/RNS/Resource.py:631-632`), the proof wait is
`3×rtt + 10 s` per round for 3 cache requests
(`RESOURCE_MAX_PROOF_RETRIES`,
`leviculum-core/src/resource/mod.rs:142`, against `retries_left = 3`,
`reference/Reticulum/RNS/Resource.py:1068`, and the round at
`reference/Reticulum/RNS/Resource.py:644`), and AwaitingProof requires
every *distinct* part sent once (`distinct_parts_sent`,
`leviculum-core/src/resource/outgoing.rs:1183`, against the
first-send-only count, `reference/Reticulum/RNS/Resource.py:1013-1015`).

The receiver's acceptance point: the reference decides *before*
anything is stored — ACCEPT_APP runs the callback synchronously on the
unpacked advertisement and only then calls `Resource.accept`
(`reference/Reticulum/RNS/Link.py:1091-1099`); ours parks the ADV on
the link and emits `ResourceAdvertised`, the application answers
asynchronously with `accept_resource`/`reject_resource`, the parked
ADV at `leviculum-core/src/node/link_management.rs:2764-2772`. Same point in
the protocol — nothing stored, reject is an RCL on both — different
concurrency shape: our decision window can span sender
re-advertisements, which the parked-ADV gate absorbs. 643's cell
description has lnpnd running its recall check in
`begin_sync_resource`, i.e. at this same pre-storage point. Class:
inert (local API), noted because the next engine that moves the check
*after* storage breaks the reference's contract.

**D12 — one incoming resource per link; the reference runs a list.**
*Fixed in 1d727b68.* Python keeps `incoming_resources` as a list, dispatches every RESOURCE
part across it (`reference/Reticulum/RNS/Link.py:1163-1166`) and
dedups only same-hash re-advertisements (`has_incoming_resource`,
`reference/Reticulum/RNS/Link.py:1311`). Ours held exactly one
`Option<IncomingResource>` per link and ignored any ADV for a
*different* hash while one was in flight — the #404 re-REQ answered
only a repeat of the *same* resource. (Fixed in 1d727b68, #657: a link now keeps
a list, `leviculum-core/src/link/mod.rs:530`, a repeat still draws the
re-REQ, `leviculum-core/src/node/link_management.rs:2633`, and a new
hash beyond the per-link bound is refused with an RCL,
`leviculum-core/src/node/link_management.rs:2714`.) What a peer saw: a Python sender that advertises a second resource on the same
link mid-transfer (two queued LXMF messages delivered back to back;
Python's own sender serialises via `ready_for_new_resource`,
`reference/Reticulum/RNS/Link.py:1331`, but *two Python senders'
semantics allow interleaved incoming*) burns its full advertisement
budget and fails the second transfer. Outgoing is equally single-slot
but caller-visible (`TransferInProgress`,
`leviculum-core/src/node/mod.rs:1580`), where Python queues
(`__advertise_job` spins on QUEUED,
`reference/Reticulum/RNS/Resource.py:522-524`). Class: **semantic**.
Test: drop-in — Python sender pushes two resources back to back over
one link to lnsd and to rnsd; mvr — second ADV with a different hash
during a transfer, assert today's silent drop.

**D13 — an advertised size cap the reference does not have.** Ours
refuses any ADV whose `t` exceeds `max_incoming_resource_size`
(default `RESOURCE_MAX_INCOMING_SIZE`, the largest single segment a
stock Python sender can emit — `leviculum-core/src/resource/mod.rs:96`)
*before allocating*, and bounds `d` against `l × MAX_EFFICIENT_SIZE`
(`from_advertisement`, `leviculum-core/src/resource/incoming.rs:245`,
the checks at `leviculum-core/src/resource/incoming.rs:254` and
`leviculum-core/src/resource/incoming.rs:274-278`). Python allocates
`parts`/`hashmap` straight from the
advertisement (`reference/Reticulum/RNS/Resource.py:190`, `:210`).
Class: **deviation ours (deliberate)** — DoS hardening, interop-neutral
for stock senders (pinned by
`default_incoming_limit_admits_the_largest_python_advertisement`,
`leviculum-core/src/resource/mod.rs:1114`).

**D14 — the default receive-window policy is not the reference's
algorithm.** Our default `WindowPolicy::Current`
(`leviculum-core/src/resource/window.rs:70`) grows the window by 1
only after `window + 4` completed rounds, picks the tier from the
REQ-to-first-part rate against thresholds of 50 000 / 15 000 / 1 000
B/s (`FAST_RATE_THRESHOLD`, `leviculum-core/src/resource/mod.rs:151`),
and never shrinks on timeout. The reference grows every completed
round with window-min creep, tiers on whole-round goodput against
6 250 / 250 B/s with 4-round/2-round hysteresis
(`reference/Reticulum/RNS/Resource.py:897-924`, constants `RATE_FAST`
at `reference/Reticulum/RNS/Resource.py:88`), and shrinks window *and*
window_max on every receiver timeout
(`reference/Reticulum/RNS/Resource.py:616-621`). A faithful mirror
exists (`round_complete_pythonlike`,
`leviculum-core/src/resource/window.rs:238`;
`timeout_pythonlike`, `leviculum-core/src/resource/window.rs:267`) and
is selectable per daemon via `LEVICULUM_RESOURCE_WINDOW_POLICY`. The
window is receiver-private — the sender serves whatever the REQ lists
— so this is wire-compatible by construction; what a peer sees is only
our REQ cadence and sizes. The A/B history (#85, the 2026-09-23
window-lock run quoted at `leviculum-core/src/resource/window.rs:10`)
is why Current is the default: the reference's timeout shrink locks at
window_min when the timeout floor is wrong, and the deviation
measurably improves Priority 1. Class: **deviation ours
(deliberate)**. Test: the `mvr_resource_window` harness plus the
`lora_window_ab_*` rig cells already compare the policies.

**D15 — the receiver's part timeout has two floors the reference
lacks.** On top of the reference's policy wait (ours:
`part_policy_wait_ms`, `leviculum-core/src/resource/mod.rs:192`,
mirroring `reference/Reticulum/RNS/Resource.py:604-606` with the same
4→2 factor switch and per-retry delay), `part_timeout_ms`
(`leviculum-core/src/resource/incoming.rs:937`) floors the wait at
(a) what the attached interface says a window costs on the air —
`window × frame_turnaround + acquisition + rtt` (#36/#374) — and
(b) the observed median inter-part gap × window, bounded by the
sender's own budget (#394, for links behind a daemon or relay where no
interface can state its carrier). The reference has neither; its
receiver on a slow half-duplex carrier times out parts still on the
air and shreds its own window. Interface isolation holds: core reads
two numbers the interface declared, and a medium with no post-TX cost
declares zero. Class: **deviation ours (deliberate), timing**. Test:
pinned by the #374/#394 mvrs and the `lora_4node_contention_rust`
cell.

**D16 — the sender's advertisement budget is derived, the
reference's is a fixed count.** Python re-advertises up to
`MAX_ADV_RETRIES` = 4 windows of `timeout + PROCESSING_GRACE` and
gives up (`reference/Reticulum/RNS/Resource.py:573-591`). Ours spends
at least those 4 windows and extends to cover the receiver's second
REQ retry (`advertisement_windows`,
`leviculum-core/src/resource/outgoing.rs:1372`;
`sender_advertisement_budget_ms`,
`leviculum-core/src/resource/outgoing.rs:340`), and our *receiver*
answers a repeated ADV of the in-flight resource with a fresh REQ
instead of ignoring it (`request_again_for_readvertisement`,
`leviculum-core/src/resource/incoming.rs:366`; Python ignores the
repeat, `reference/Reticulum/RNS/Resource.py:237-239`). Both are plain
wire a Python peer already handles — a REQ is what its sender waits
for. Class: **deviation ours (deliberate), timing** (#404).

**D17 — HMU segment misalignment: the reference cancels, we serve.**
Python cancels the whole transfer when the receiver's last-known map
hash does not land on a segment boundary ("Resource sequencing
error", `reference/Reticulum/RNS/Resource.py:1040-1043`). Ours
computes the containing segment and serves it (`handle_request`, HMU
branch, `leviculum-core/src/resource/outgoing.rs:1149-1151`). Against
a stock receiver the boundary always aligns (hashmaps travel in whole
segments), so the arm is dead-on-arrival in practice; against a
degenerate one we recover where Python aborts. Class: **inert**.
Test: unit — REQ with a mid-segment last_map_hash against both; ours
answers, the reference cancels.

## 5. Channel and Buffer

We implement both, and the wire is the reference's: the envelope is
`[msgtype:2][sequence:2][length:2][data]` big-endian
(`Envelope::pack`, `leviculum-core/src/link/channel/envelope.rs:59`,
against `Envelope.pack`, `reference/Reticulum/RNS/Channel.py:192-196`),
the channel MDU is the link MDU minus 6
(`Channel::mdu`, `leviculum-core/src/link/channel/mod.rs:299`, against
`reference/Reticulum/RNS/Channel.py:652-655`), system message types
are >= 0xf000, and the Buffer's `StreamDataMessage` is 0xff00 with the
2-byte `EOF|compressed|stream_id` header
(`StreamDataMessage`, `leviculum-core/src/link/channel/stream.rs:61`,
against `reference/Reticulum/RNS/Buffer.py:44-56`), bz2-compressed
chunks interoperating. A Python peer that uses Channel or Buffer over
a link to us gets a functioning, ordered, retried byte stream.

Behavioural divergences, all tx-local (the window, the retry clock and
the proof policy are each end's own):

**D18 — retry budget 8 versus the reference's 5, then teardown.**
`CHANNEL_MAX_TRIES` (`leviculum-core/src/constants.rs:503`) is 8;
Python's `_max_tries` is 5 (`reference/Reticulum/RNS/Channel.py:295`).
Both tear the link down on exhaustion (ours
`ChannelAction::TearDownLink` handled at
`leviculum-core/src/node/link_management.rs:3959`; Python
`_packet_timeout` escalating to the outlet's `timed_out`,
`reference/Reticulum/RNS/Channel.py:569-584`). We hold a struggling
link ~3 retries longer. Class: **timing**.

**D19 — timeout, window and pacing are a different controller.**
Python's per-packet timeout is
`1.5^(tries-1) × max(2.5×rtt, 25 ms) × (len(tx_ring)+1.5)`
(`_get_packet_timeout_time`,
`reference/Reticulum/RNS/Channel.py:551-552`); window +1 per delivery,
−1 per timeout, tiers promoted after 10 consecutive
fast/medium-RTT deliveries (`reference/Reticulum/RNS/Channel.py:504-533`),
and a link whose RTT exceeds 1.45 s is pinned to window 1 at channel
creation (`reference/Reticulum/RNS/Channel.py:299-303`). Ours:
additive queue-delay RTO `(max(2.5×rtt, 100 ms) + queue×rtt/window) ×
1.5^(tries-1)` (`calculate_timeout`,
`leviculum-core/src/link/channel/mod.rs:98`), window tiers chosen
directly from RTT thresholds 180/750/2000 ms
(`update_window_for_rtt`,
`leviculum-core/src/link/channel/mod.rs:460`), RFC-6298 SRTT with
Karn's rule (`mark_delivered`,
`leviculum-core/src/link/channel/mod.rs:808`), and send pacing at
`rtt/window` with multiplicative backoff (`recalculate_pacing`,
`leviculum-core/src/link/channel/mod.rs:993`) that Python does not
have at all. All of it is our side's transmit discipline over
identical wire bytes. Class: **deviation ours (deliberate)**.

**D20 — proofs are deferred to in-order delivery; the reference
proves on arrival.** Python proves every CHANNEL packet before the
channel even parses it (`packet.prove()` at
`reference/Reticulum/RNS/Link.py:1172`), so a Python sender clears its
retransmit slot for a packet the receiver only *buffered*
out-of-order. Ours proves on delivery (and on drain, and for
duplicates of delivered packets), never for buffered gaps
(`leviculum-core/src/node/link_management.rs:1687-1772`), precisely so
head-of-line blocking keeps the sender retransmitting the gap. Against
a Python *sender* this costs nothing but the retransmits it would have
skipped; against our sender a Python receiver proves early and our
ring trusts it — symmetric, wire-legal, but the recovery dynamics
under reorder differ by direction. Our rx ring is also bounded at 512
with a loud refusal (`CHANNEL_RX_RING_MAX`,
`leviculum-core/src/constants.rs:500`) where Python's deque is
unbounded, and our backward-sequence acceptance window is half the
sequence space with a re-prove
(`Channel::receive`, `leviculum-core/src/link/channel/mod.rs:623`)
where Python validates against `WINDOW_MAX`
(`reference/Reticulum/RNS/Channel.py:431-439`) and re-proves via the
pre-parse prove. Class: **semantic (recovery dynamics), inert on the
wire**. Test: mvr — three-packet reorder against both receivers, count
retransmits and proofs per direction.

## 6. Ratchets and the link

Where the ratchet lives, on both stacks: in the *destination* encrypt
path, not in the link. A SINGLE destination announces a rotating
X25519 ratchet key; a sender encrypts opportunistic (non-link) packets
to the latest ratchet it learned from an announce, and a receiver
whose destination enforces ratchets refuses packets encrypted to the
bare identity key. Ours applies it exactly there: the announce carries
`current_ratchet_public` (`leviculum-core/src/node/mod.rs:987`), the
send path looks up the peer's remembered ratchet
(`remember_known_ratchet`, `leviculum-core/src/node/mod.rs:1018`), and
the receive path records whether a ratchet decrypted the packet and
applies the live enforce policy (`ratchet_used`,
`leviculum-core/src/node/mod.rs:314`). A link needs none of this: its
keys are ephemeral X25519 freshly drawn per link
(`new_outgoing`, `leviculum-core/src/link/mod.rs:576`, against
`reference/Reticulum/RNS/Link.py:278-286`), so every link packet,
resource part and channel envelope is forward-secret without ratchets,
and the reference's only ratchet touch in `Link.receive` is labelling
the packet with the link id as its "ratchet id" for bookkeeping
(`reference/Reticulum/RNS/Link.py:990`).

The July bimodal bug (2026-07-03) in these terms: it was *not* a
link-layer or resource-layer mechanism. The delivery-log distribution
(0 % or 100 %, nothing between) came from the pre-link, opportunistic
phase: a ratchet-enforcing receiver drops every packet whose sender
has not yet learned its ratchet from an announce, so a run was 0 %
when the selftest's blind 2 s wait lost the race to the ratcheted
announce and 100 % when it won — all-or-nothing per run, hence
bimodal. The fix preconditioned the phase on
`known_remote_ratchet` both ways (commit dbfd7c68). The resource
relevance is negative and worth stating: once a link is up, ratchet
state can no longer affect delivery, so a bimodal resource-transfer
distribution today would have to come from the window/timeout
machinery above (as the #374 lock did), never from ratchets.

## The three to fix first

1. **D7, pin inbound link packets to the attached interface.** The
   mechanism is one missing comparison: `handle_link_data` never sees
   `interface_index`, so unauthenticated link frames (keepalives,
   resource parts, resource proofs) are accepted from any interface
   where the reference refuses them. Thread the index through and drop
   mismatches with a counter — one condition, Python's semantics, and
   the one divergence here with a security edge. (Decide deliberately
   whether a *stale* link may re-pin to a new interface, which is the
   robustness our current behaviour accidentally provides.)
2. **D12, accept a second incoming resource or refuse it loudly.**
   Today a Python peer's second concurrent ADV dies silently and costs
   it its whole advertisement budget. Either a short list the way the
   reference does it, or an immediate RCL so the sender fails fast —
   both are small; silence is the one wrong option. This is squarely
   in the propagation-node lane: a sync and a message delivery sharing
   one link is exactly the two-ADV case.
3. **D3, stop RTT retries from reshaping a Python responder.** The
   retry is right (it closes a real LoRa establishment hole), but each
   duplicate LRRTT makes a Python responder re-run its established
   callback and inflate its RTT to the retry interval, with keepalive
   and stale windows following. Cheapest containment: carry the
   *original* measured RTT in every retry (we already do) and cap the
   retry count on links whose first hop is fast (where loss of LRRTT
   is not the failure mode); the real fix is an A/B measurement of a
   Python responder under one lost LRRTT, then a decision with numbers.

## What half-duplex LoRa changes in this picture

Nothing in core may know the carrier, and after this reading nothing
does: every place the link or resource layer behaves differently over
LoRa, it does so through two numbers the *interface* declares —
`frame_turnaround_ms` and `acquisition_max_ms` — stored on the link as
plain figures (`note_frame_turnaround_ms`,
`leviculum-core/src/link/mod.rs:1197`) and consumed as timeout floors
(`part_timeout_ms`, `leviculum-core/src/resource/incoming.rs:937`).
What the medium changes is which divergences carry weight. On TCP, D5,
D6 and D19 are noise. On a half-duplex carrier, timing *is* the
protocol: the receiver's part timeout must exceed what the sender's
radio can physically do (D15 exists because it once didn't), every
gratuitous frame is airtime taken from data (D6's periodic keepalive
and D3's RTT retries are paid in duty cycle), and both ends' watchdogs
must agree within a window-flight of each other or they shred each
other's windows (D14's policy choice is the record of that fight). The
reference reads its own RTT and nothing else, which is why its
receiver locks up on carriers whose per-frame cost exceeds the RTT;
our deviations D14-D16 are all the same deviation — believe the
interface's stated cost and the observed pace over the RTT-only model
— applied at three timers, with the interface boundary intact.

## Wrong claims found in our own doc comments

Reading the reference beside our comments caught four citations that
point at the wrong reference line. All four are prose-only (the
citation guard existence-checks `Resource.py:NNN` spellings but they
carry no `reference/` path, so they were never drift-checked). None
change behaviour; the constants beside them are correct. All four are
fixed in 1d727b68 (#657), rewritten in the `reference/` form at the
live lines; the locations below are where the comments sit now.

1. `leviculum-core/src/resource/mod.rs:166` said `RETRY_GRACE_TIME =
   0.25` is at "Resource.py:133"; it is at
   `reference/Reticulum/RNS/Resource.py:134` (133 is
   `PROCESSING_GRACE`).
2. `leviculum-core/src/resource/mod.rs:179` said `PROCESSING_GRACE =
   1.0` is at "Resource.py:132"; it is at
   `reference/Reticulum/RNS/Resource.py:133` (132 is
   `SENDER_GRACE_TIME`).
3. `leviculum-core/src/resource/incoming.rs:263` cited the
   `total_segments` formula at "Resource.py:296"; line 296 is inside
   the commented-out block above it, the live formula is at
   `reference/Reticulum/RNS/Resource.py:299`.
4. `leviculum-core/src/resource/incoming.rs:266` cited
   `if adv.l > 1` at "Resource.py:203"; the line is
   `reference/Reticulum/RNS/Resource.py:204`.

## Divergence count by class

- wire: **0** — nothing found that changes bytes a peer parses.
- semantic: **4** (D3's peer side effect, D7, D11, D12; D20's recovery
  dynamics are borderline and counted here as the fifth if one wants
  the strict reading).
- timing: **5** (D4, D5, D8, D18, and D6's airtime half).
- deviation ours (deliberate): **9** (D1, D2, D3, D6, D10, D13, D14,
  D15, D16, D19 — several double-booked with timing above where the
  deviation *is* a timing change).
- inert: **3** (D9, D17, the acceptance-point and proof-cache shape
  notes).
