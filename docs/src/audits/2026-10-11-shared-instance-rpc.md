# The shared-instance RPC and the config file, read against rnsd (2026-10-11)

An audit in the shape of the air-path audits: the drop-in surface —
the shared-instance RPC server, the LocalInterface data channel, and
the Python config-file parser — read side by side against what a
Python-RNS `rnsd` and its client tools (`rnstatus`, `rnprobe`,
`rnpath`, `rncp`, `rnid`) expect, every divergence pinned to a line on
both sides. Nothing was changed; this document is the deliverable.

Reference tree: `reference/Reticulum` (RNS 1.3.5 per
`reference/Reticulum/RNS/_version.py:1`, carrying local test patches
at `RNS/Reticulum.py:307-313` and `:451-466` that this audit reads
around). Where our code deliberately pins post-1.3.5 behaviour it
names Reticulum 1.5.2, measured, and this audit takes those pins as
given rather than re-measuring a tree we do not vendor. Every
`path:line` below is checked by the citation guard on each run.

Each divergence names: ours and theirs by line, what a Python tool or
a user's config sees, a class — *wrong answer*, *missing op or
answer*, *silently ignored config*, *deviation ours (deliberate)*,
*extra (additive)* — the issue it bears on, and the test that would
show it (the drop-in test: the same tool against both daemons, per
the A/B rule).

## 1. The RPC transport: socket, framing, authentication

Where we match, verified: the control socket is the abstract Unix
socket `\0rns/{instance_name}/rpc` (`spawn_rpc_server`,
`leviculum-std/src/rpc/mod.rs:109-121`, against
`reference/Reticulum/RNS/Reticulum.py:349`), with the TCP-loopback
fallback on Windows (`loopback_addr`,
`leviculum-std/src/interfaces/local.rs:204`, against the AF_INET arm,
`reference/Reticulum/RNS/Reticulum.py:352`); the framing is the
CPython `multiprocessing.connection` 4-byte big-endian length prefix
(`read_message_bounded`, `leviculum-std/src/rpc/connection.rs:82`);
the handshake is the bidirectional HMAC challenge in both dialects —
raw HMAC-MD5 for Python < 3.12 and tagged `{sha256}` for 3.12+
(`deliver_challenge`, `leviculum-std/src/rpc/connection.rs:140`,
exercised against simulated legacy clients in the same file's tests);
the default authkey is SHA-256 over the transport identity's private
key (`rpc_authkey`, `leviculum-std/src/rpc/mod.rs:245`, against
`reference/Reticulum/RNS/Reticulum.py:355-356`), which is what lets a
Python client sharing the daemon's storage derive the same key; one
request per connection, answered and closed
(`handle_rpc_connection`, `leviculum-std/src/rpc/mod.rs:198`, against
the accept-recv-reply-close loop,
`reference/Reticulum/RNS/Reticulum.py:1207-1260`); and a verb the
server does not know gets no reply and a closed connection on both
sides (`request_from_value`, `leviculum-std/src/rpc/pickle.rs:316-320`
and `:394`; Python matches no arm and closes,
`reference/Reticulum/RNS/Reticulum.py:1213-1260`). The codec is
sniffed per request and the response encoded to match, so both the
msgpack dialect current upstream speaks and the older pickle dialect
work (`detect_codec`, `leviculum-std/src/rpc/pickle.rs:35`).

**D1 — the `rpc_key` config key is silently ignored, and a config
that sets it locks every Python tool out.** Python reads `rpc_key`
from `[reticulum]` as hex and keys BOTH sides of the RPC with it — the
daemon's listener and every client connection
(`reference/Reticulum/RNS/Reticulum.py:509-515`, applied at `:355-359`
and `:1265`). Our `[reticulum]` parser has no arm for the key, so it
falls into the tolerated-unknown catch-all
(`apply_reticulum_key`, `leviculum-std/src/ini_config.rs:568`) and the
daemon keys its listener with the identity-derived default
unconditionally (`leviculum-std/src/rpc/mod.rs:245-253`). What a tool
sees: `rnstatus` reads the same config file, derives the configured
key, and fails our HMAC handshake — "Could not get RNS status" against
a daemon that is up and healthy, with nothing in either log naming the
key mismatch. Class: **silently ignored config, drop-in break when
set**. Bears on: the drop-in design goal (CLAUDE.md) directly. Test:
drop-in — one config with `rpc_key = <hex>`, `rnstatus` against both
daemons; unit — `parse_ini` stores the key and `spawn_rpc_server` is
keyed with it, wrong-key client refused (the existing
`test_rpc_auth_failure` shape).

*Fixed in e5f78966.* `rpc_key` is parsed as `bytes.fromhex` parses it,
with the same fallback on a bad value (`parse_rpc_key`), and keys both
ends through one resolver (`resolve_rpc_authkey`): the daemon's
listener, a client node's first-hop-timeout query, and our client
tools, whose `resolve_authkey` now prefers the config's key — keying
the listener alone would have locked `lnstatus` out where `rnstatus`
got in. Pinned by `test_rnstatus_with_rpc_key_against_rust_daemon`
(`rnstatus` from the reference tree and lnsd built from one config
file; a client without the key is refused),
`configured_rpc_key_keys_the_listener`, `test_rpc_key_parsed_as_hex`
and `a_configured_rpc_key_wins_over_the_identity_file`.

**D2 — `shared_instance_type = tcp` is parsed but not acted on, on
the platforms where rnsd acts on it.** Python, on any platform, moves
BOTH channels to TCP loopback (37428/37429 or the configured ports)
when the key says `tcp` (`reference/Reticulum/RNS/Reticulum.py:683-694`).
We parse and store the key (`leviculum-std/src/ini_config.rs:400-409`)
and apply its socket-override rule, but on Unix the daemon serves the
abstract AF_UNIX pair regardless; only the Windows build reads the
ports (`CONFIGURED_LOOPBACK_PORTS`,
`leviculum-std/src/interfaces/local.rs:136`, and the field's own
honest doc, `leviculum-std/src/config.rs:69-76`). What a tool sees: on
a Linux host whose config says `tcp`, Python clients dial
127.0.0.1:37429 and find nothing; lnsd listens where they do not look.
Class: **silently ignored config (Unix), drop-in break when set**.
Documented on the config field, so it is a known gap, not a surprise —
but the configuration guide's drop-in paragraph (see §6) does not
carry the caveat. Test: drop-in — `shared_instance_type = tcp` on
Linux, `rnstatus` against both daemons.

**D3 — message ceilings where Python has none.** We refuse a request
whose length prefix exceeds 64 KiB before allocating
(`MAX_REQUEST_LEN`, `leviculum-std/src/rpc/connection.rs:48`) and a
response above 128 MiB (`MAX_RESPONSE_LEN`,
`leviculum-std/src/rpc/connection.rs:63`); Python allocates whatever
the prefix claims, up to 2 GiB. The whole legitimate request
vocabulary measures under 130 bytes
(`rpc_request_vocabulary_fits_under_the_ceiling`,
`leviculum-std/src/rpc/pickle.rs:990`), so no real client can hit the
ceiling. Class: deviation ours (deliberate, a pre-auth allocation
fix). No tool-visible consequence.

Platform note, already stated in the code: the non-Linux Unix fallback
binds a filesystem socket under the temp directory
(`bind_rpc_listener`, `leviculum-std/src/rpc/mod.rs:55-72`), which no
Python client dials; the macOS/BSD path is community-supported and
outside the drop-in contract (`leviculum-std/src/rpc/mod.rs:29-31`).

## 2. The `get` vocabulary and the answers, field by field

Where we match, verified against the reference and the tools that
read the answers:

* **`interface_stats`** — the full per-interface dict
  (`row_fields`, `leviculum-std/src/rpc/handlers.rs:456`, against
  `reference/Reticulum/RNS/Reticulum.py:1332-1490`), including the
  conditional keys Python gates on `hasattr` (parent pair, radio
  stats, battery) and the post-1.3.5 key set a 1.5.2 `rnstatus`
  indexes unguarded (`mtu`, `txdrp`, the sort keys; pinned with types
  and defaults in `test_rpc_interface_stats_round_trip`,
  `leviculum-std/src/rpc/mod.rs:719`). Display names reproduce the
  reference's `str(interface)` forms byte for byte, including the
  prefixes `rnstatus` hides rows by and the `Shared Instance[` prefix
  its client arithmetic keys on (`tcp_spawned_name`,
  `leviculum-std/src/interfaces/inventory.rs:67`;
  `shared_instance_name`, `leviculum-std/src/interfaces/inventory.rs:85`;
  against `reference/Reticulum/RNS/Utilities/rnstatus.py:393-401` and
  `:429-438`). The live client count decrements on disconnect
  (`retire_from_inventory`, `leviculum-std/src/driver/mod.rs:4129`),
  so the `Serving : N programs` line (clients − 1,
  `reference/Reticulum/RNS/Utilities/rnstatus.py:432`) reads the same
  against either daemon. Announce-rate defaults follow Python's
  transport branch (3600/0/5 when transport is on, None when off;
  pinned in `leviculum-std/src/rpc/mod.rs:917-962`).
* **`path_table`** — six keys, units, the direct-path `via` spelled as
  the destination hash exactly as Python's `received_from` makes it
  (`build_path_table`, `leviculum-std/src/rpc/handlers.rs:1065`,
  against `reference/Reticulum/RNS/Reticulum.py:1516-1538` and
  `reference/Reticulum/RNS/Transport.py:1741`), `max_hops` honoured
  with None as no-limit, and our own client always sending the key
  because Python's server `KeyError`s without it
  (`rpc_query`, `leviculum-std/src/rpc/mod.rs:401-411`).
* **`link_count` / `first_hop_timeout` / `next_hop`** — the transport
  link table's size, the reference's bitrate-scaled timeout formula,
  and the direct-path destination-hash answer
  (`leviculum-std/src/rpc/handlers.rs:51`, `first_hop_timeout_secs`,
  `leviculum-std/src/rpc/handlers.rs:1682`, `get_next_hop`,
  `leviculum-std/src/rpc/handlers.rs:1786`; against
  `reference/Reticulum/RNS/Reticulum.py:1638-1646`,
  `reference/Reticulum/RNS/Transport.py:2700-2703`).
* **`drop: path` / `drop: all_via`** — bool and dropped-count answers
  (`drop_path`, `leviculum-std/src/rpc/handlers.rs:1819`, against
  `reference/Reticulum/RNS/Reticulum.py:1561-1585`).
* **The blackhole set** — insert/duplicate/lift/absent return values
  (`True`/`None`), the listing dict keyed by hash bytes with
  `source`/`until`/`reason`, and the membership bool
  (`build_blackholed_identities`,
  `leviculum-std/src/rpc/handlers.rs:1840`, round-tripped in
  `test_rpc_blackhole_round_trip`, `leviculum-std/src/rpc/mod.rs:1041`;
  consumed by `rnpath -b/-B/-U`,
  `reference/Reticulum/RNS/Utilities/rnpath.py:185-199`).
* **`destination_data` and `identity_data`** — all five ops parsed and
  served with Python's booleans
  (`leviculum-std/src/rpc/pickle.rs:363-392`,
  `leviculum-std/src/rpc/handlers.rs:1920-1966`, against
  `reference/Reticulum/RNS/Reticulum.py:1248-1258`). The
  `identity_data` arm closes the gap the 2026-06-29 conformance pass
  recorded (the memory `project_identity_data_rpc_gap`, now stale):
  `rnid`'s retain no longer falls through to a dropped connection.
* **Leviculum-only verbs** (`link_table`, `transport_tables`,
  `identities`, `discovered_interfaces`, `lowest_interface_bitrate`,
  `medium_path_timeout`, `active_link_count`) — additive; a Python
  `rnsd` asked any of them closes the connection without replying, and
  our callers treat that as "does not know the question"
  (`rpc_query`'s contract, `leviculum-std/src/rpc/mod.rs:375-384`).

**D4 — `rate_table` answers an always-empty `timestamps` list, and
`rnpath -r` renders an error for every row.** Python's rate entries
carry up to 16 announce timestamps
(`MAX_RATE_TIMESTAMPS`, `reference/Reticulum/RNS/Transport.py:96`,
maintained at `:1841-1849`) and serves them per row
(`reference/Reticulum/RNS/Reticulum.py:1540-1559`). Our export has no
timestamps at all, and the handler hardcodes the empty list
(`build_rate_table`, `leviculum-std/src/rpc/handlers.rs:1653-1662`;
`RateTableExport`, `leviculum-core/src/transport.rs:1593`). What the
tool sees: `rnpath -r` reads `entry["timestamps"][0]`
(`reference/Reticulum/RNS/Utilities/rnpath.py:344`), the IndexError is
caught per entry, and every row prints "Error while processing entry
for <hash>" (`reference/Reticulum/RNS/Utilities/rnpath.py:371-373`) —
the rate view is unusable against lnsd while the same command against
rnsd prints announce rates. Class: **wrong answer**. Bears on: the
drop-in rule; any A/B that reads announce-rate enforcement. Test:
drop-in — `rnpath -r` against both daemons with one rate entry
present; unit — a rate entry with a `last` answers a non-empty
`timestamps` whose first element is a float.

*Fixed in e5f78966.* Every rate check records its announce time, blocked
ones included as Python's append precedes its block test, capped at
`MAX_RATE_TIMESTAMPS`; the times live in MemoryStorage's slot beside the
entry (the board's `AnnounceRateEntry` is unchanged) and
`build_rate_table` serves them, `[last]` for a storage that keeps none.
Pinned by `test_rnpath_rate_table_prints_a_row_for_a_rate_entry`
(`rnpath -r` from the reference tree against lnsd prints the rate row;
red before with "list index out of range"),
`test_announce_rate_timestamps_keep_the_newest_sixteen` and
`announce_rate_timestamps_ride_with_their_entry`.

**D5 — `drop: announce_queues` does nothing and answers `true`.**
Python walks every interface's announce queue and empties it
(`reference/Reticulum/RNS/Reticulum.py:1587-1595` →
`reference/Reticulum/RNS/Transport.py:3165-3175`); the (standalone)
return is None either way, and `rnpath -D` prints "Dropping announce
queues on all interfaces..." trusting the daemon did it
(`reference/Reticulum/RNS/Utilities/rnpath.py:386-387`). Our dispatch
arm is a constant: `pickle_bool(true)` with no call into the core
(`leviculum-std/src/rpc/handlers.rs:113`) — while the core does hold
per-interface announce-cap queues that the verb is about
(`drain_announce_queues`, `leviculum-core/src/transport.rs:12578`).
What the operator sees: the command reports success and the queued
announces still go out. Class: **missing op claiming success** — the
worst of the three possible answers (refusing would at least be
honest). Bears on: announce propagation debugging under a saturated
cap (the D14/D15 family of the air-path audit). Test: unit — queue an
announce under a capped interface, issue the RPC, assert the queue is
empty and nothing transmits at the next drain.

**D6 — `packet_rssi` / `packet_snr` / `packet_q` always answer
None, so `rnprobe` against lnsd never prints RSSI/SNR on a LoRa
path.** Python caches the last 512 per-packet radio readings exactly
so local clients can ask after the fact
(`reference/Reticulum/RNS/Transport.py:166-168`, filled at
`:1458-1479`), and `rnprobe` decorates a delivered probe with them
(`reference/Reticulum/RNS/Utilities/rnprobe.py:166-178`). Ours parses
the three verbs and answers None unconditionally
(`leviculum-std/src/rpc/handlers.rs:120-137`) even though the RNode
interface does track per-interface last RSSI/SNR
(`radio_stat_fields`, `leviculum-std/src/rpc/handlers.rs:335`): the
per-PACKET keyed cache does not exist. What the tool sees: the same
probe over the same radio prints `[RSSI -61 dBm] [SNR 5.5 dB]`
against rnsd and nothing against lnsd. Class: **missing answer**.
Bears on: every rig A/B that reads rnprobe output; the measurement
discipline's same-driver rule is kept, but the two daemons answer
differently. Test: drop-in — `rnprobe` over the RNode pair against
both daemons; unit — a packet delivered over an interface with radio
stats answers its hash's `packet_rssi` with the stamped value.

*Fixed in e5f78966.* The cap is 512, not 1000:
`LOCAL_CLIENT_CACHE_MAXSIZE` is set at
`reference/Reticulum/RNS/Transport.py:168`. The RNode interface stamps
each received frame with its last RSSI/SNR report and the reference's
link quality (`packet_radio_stats`, `link_quality`), the event loop
files them under the packet hash, unwrapped on an IFAC interface
(`record_packet_radio_stats`), and the three verbs answer from that
cache (`leviculum-std/src/rpc/radio_cache.rs`). Pinned by
`test_rnprobe_prints_the_radio_readings_of_its_proof`: `rnprobe` from
the reference tree against lnsd, the probe answered by a second node
over a scripted pair of RNode modems, prints `[RSSI -61 dBm] [SNR 5.5
dB] [Link Quality 96.7%]`, and nothing of it without the cache. The
multi-vport RNode path does not stamp yet; it tracks no per-vport
RSSI/SNR to stamp from.

**D7 — `announce_queue` is always None in `interface_stats`, so
`rnstatus -A` can never show queued announces.** Python reports
`len(interface.announce_queue)` whenever the interface holds one
(`reference/Reticulum/RNS/Reticulum.py:1463-1467`) and `rnstatus -A`
renders "Queued : N announces"
(`reference/Reticulum/RNS/Utilities/rnstatus.py:542-547`). Ours emits
the key as None unconditionally
(`leviculum-std/src/rpc/handlers.rs:643`) while the core maintains
exactly that queue per interface (the announce-cap queue,
`leviculum-core/src/transport.rs:12578`). Class: **missing answer**
(None where a number exists). Bears on: observing a saturated LoRa
cap, which is the state the air-path audit's D14/D15 lived in — the
operator-visible gauge for it is missing. Test: unit — queue one
capped announce, `interface_stats` answers `announce_queue: 1`;
drop-in — `rnstatus -A` under a saturated cap on both daemons.

**D8 — the path answers name interfaces by driver-internal label, not
by the reference display name the same daemon's `interface_stats`
uses.** Python's `path_table` rows carry `str(interface)` —
`TCPInterface[Client on ...]`, `RNodeInterface[...]`
(`reference/Reticulum/RNS/Reticulum.py:1534`). Ours carry the driver's
internal name — `tcp_client_0` for a configured TCP client (the format
lives at `leviculum-std/src/driver/mod.rs:859`), `udp_5`,
`rnode_channel_2` — because the handlers read `core.interface_name`
(`build_path_table`, `leviculum-std/src/rpc/handlers.rs:1065`, the
lookup at `leviculum-std/src/rpc/handlers.rs:1087-1089`;
same in `build_transport_tables` and `get_next_hop_if_name`), while
the reference-style names live only in the reporting inventory that
`interface_stats` reads. What the tool sees: `rnpath -t` prints "on
tcp_client_0"; a script joining `rnpath -tj` rows against
`rnstatus -j` interface names by string never matches — against
either daemon's own other answer. Class: **wrong answer (cosmetic on
screen, breaking for scripts that join)**. Test: drop-in — `rnpath
-t` against both daemons over one TCP path, compare the `interface`
field; unit — a path row's `interface` equals the `name` the same
daemon's `interface_stats` reports for that interface.

**D9 — the no-path `next_hop_if_name` answer is `"unknown"`, Python's
is the string `"None"`.** Python's standalone arm is
`str(Transport.next_hop_interface(dest))`, which stringifies the
missing case to `"None"`
(`reference/Reticulum/RNS/Reticulum.py:1597-1605`), and `rnprobe`
tests for exactly that spelling before printing an `on <interface>`
suffix (`reference/Reticulum/RNS/Utilities/rnprobe.py:127`). Ours
answers `"unknown"` (`get_next_hop_if_name`,
`leviculum-std/src/rpc/handlers.rs:1801-1815`), so a verbose rnprobe
with no resolved path prints "on unknown" where rnsd prints nothing.
Class: wrong answer, edge. Test: covered by the D6 drop-in run's
verbose mode.

Two permissive edges, noted and harmless: `try_into_hash` accepts and
truncates an over-long hash where Python's client-side guards refuse
it (`leviculum-std/src/rpc/handlers.rs:2057-2065`); a request dict
carrying both `get` and `drop` answers the `get` only, where Python
would answer both in sequence
(`request_from_value`, `leviculum-std/src/rpc/pickle.rs:255`;
`reference/Reticulum/RNS/Reticulum.py:1213-1236`) — no real client
sends either shape.

## 3. The LocalInterface: the data channel and its transport semantics

Where we match, verified: the data socket is `\0rns/{instance_name}`
with HDLC framing (flag 0x7E, escape 0x7D;
`spawn_local_server`, `leviculum-std/src/interfaces/local.rs:262`,
against `reference/Reticulum/RNS/Interfaces/LocalInterface.py:42-51`
and `:386`); HW_MTU 262144 (`LOCAL_HW_MTU`,
`leviculum-std/src/interfaces/local.rs:237`, against
`reference/Reticulum/RNS/Interfaces/LocalInterface.py:71`); the
reported bitrate 1 Gbps (`LOCAL_BITRATE`,
`leviculum-std/src/interfaces/local.rs:241`, against
`reference/Reticulum/RNS/Interfaces/LocalInterface.py:432`); an
accepted client is never ingress-limited, which the reference fixes by
overriding `should_ingress_limit`
(`leviculum-std/src/interfaces/local.rs:397` and the client side at
`:621`, against
`reference/Reticulum/RNS/Interfaces/LocalInterface.py:137-138`); the
server's short name is "Reticulum" and the client label is the live
client count (`leviculum-std/src/interfaces/local.rs:285-288` and
`:313-319`, against
`reference/Reticulum/RNS/Interfaces/LocalInterface.py:391` and
`:441`, decremented on disconnect as the reference does at `:355`);
Python's two-flag keepalive
(`send_keepalive`,
`reference/Reticulum/RNS/Interfaces/LocalInterface.py:195-209`) is
absorbed by our deframer as a frame start rather than surfacing an
empty frame (`process_byte`,
`leviculum-core/src/framing/hdlc.rs:311-321`).

The transport semantics behind the socket, verified against Python's
local-client special cases:

* **Hop spoofing** — increment on receipt, then undo for the IPC hop,
  with Python's exact if/elif exclusivity between "I am the shared
  instance" and "I am a client of one" (`incoming_hop_count`,
  `leviculum-core/src/transport.rs:9989-9999`, against
  `reference/Reticulum/RNS/Transport.py:1481-1484`).
* **Announce relay to clients** — every accepted announce is re-packed
  HEADER_2 / transport-id = this daemon, hops preserved, to every
  client but the source (`hand_announce_to_local_clients`,
  `leviculum-core/src/transport.rs:9900`, against
  `reference/Reticulum/RNS/Transport.py:1935-1978`).
* **Path requests** — Python's five branches including the
  forward-to-local-clients arm
  (`leviculum-core/src/transport.rs:11519`, against
  `reference/Reticulum/RNS/Transport.py:2911-3052`), with the pending
  local-PR table (`leviculum-core/src/transport.rs:3060`, against
  `reference/Reticulum/RNS/Transport.py:2926-2936`).
* **Plain broadcasts** — from a local client to all interfaces, from
  the network to all local clients
  (`leviculum-core/src/transport.rs:8194-8217`, against
  `reference/Reticulum/RNS/Transport.py:1521-1534`).
* **Disconnect/reconnect signalling** — the client drops cached
  routing state on loss and re-announces on recovery, mirroring
  `shared_connection_disappeared`/`reappeared`
  (`LocalClientConfig`, `leviculum-std/src/interfaces/local.rs:469-486`,
  against `reference/Reticulum/RNS/Interfaces/LocalInterface.py:281`
  and `:187`).

**D10 — sub-minimum frames are handed to the core instead of dropped
at the interface.** The reference discards any deframed IPC frame of
`HEADER_MINSIZE` (19 bytes) or less
(`reference/Reticulum/RNS/Interfaces/LocalInterface.py:267`); ours
forwards every non-empty frame
(`local_interface_task`, `leviculum-std/src/interfaces/local.rs:773`,
the forward at `leviculum-std/src/interfaces/local.rs:811-815`)
and relies on packet parsing to refuse it. Semantically a no-op — a
short frame is unparseable either way — recorded so a future
malformed-client fuzz finding is not hunted as a transport bug.
Class: deviation ours, inert.

**D11 — the reconnect cadence is ours, deliberately.** Python waits
`RECONNECT_WAIT` = 8 s before its first retry
(`reference/Reticulum/RNS/Interfaces/LocalInterface.py:63` and
`:167`); ours starts at 250 ms and backs off to Python's own 8 s
ceiling (`LOCAL_RECONNECT_BASE`,
`leviculum-std/src/interfaces/local.rs:456`, `LOCAL_RECONNECT_MAX`,
`leviculum-std/src/interfaces/local.rs:466`). Local-only timing, no
wire or peer-visible change; a `systemctl restart lnsd` costs a
client a fraction of a second instead of 8+. Class: deviation ours
(deliberate, argued at the constants).

## 4. The config file, key by key

Where we match, verified: the ConfigObj grammar — three section
depths, invalid lines refuse the whole file exactly as `rnsd` panics
on them (`parse_ini`, `leviculum-std/src/ini_config.rs:207-209`,
against `reference/Reticulum/RNS/Reticulum.py:328-333`), inline
comments and quote stripping to ConfigObj's own rules
(`strip_value`, `leviculum-std/src/ini_config.rs:973`); the config
directory lookup order `/etc/reticulum` → `~/.config/reticulum` →
`~/.reticulum` (`resolve_config_dir`,
`leviculum-std/src/config.rs:1112`, against
`reference/Reticulum/RNS/Reticulum.py:229-236`); `ifac_size` in bits
with the ≥ 8 floor (`leviculum-std/src/ini_config.rs:812-818`,
against `reference/Reticulum/RNS/Reticulum.py:747-750`) and the
per-type default IFAC sizes pinned class by class
(`default_ifac_size`, `leviculum-std/src/driver/mod.rs:578`);
`announce_cap` kept only in (0, 100]
(`leviculum-std/src/ini_config.rs:792-794`, against
`reference/Reticulum/RNS/Reticulum.py:819-822`); `bitrate` gated on
MINIMUM_BITRATE (`leviculum-std/src/ini_config.rs:695-701`);
Backbone-to-TCP normalization including the `port`/`remote`/
`listen_on` aliases (`normalize_backbone_interface`,
`leviculum-std/src/ini_config.rs:898`, against
`reference/Reticulum/RNS/Reticulum.py:960-972`); the three UDP config
forms of leviculum#279 — single `port`, bind-only, forward-only, plus
the `device` broadcast fill — each built and pinned end to end from
an rnsd-style file
(`leviculum-std/src/driver/interface_build/udp.rs:29-47` and its
tests at `:327`, against
`reference/Reticulum/RNS/Interfaces/UDPInterface.py:68-72` and
`:82-110`); the UDP MTU 1064 of leviculum#357, matched with the
receive-bound caveat documented where it lives
(`leviculum-std/src/interfaces/udp.rs:21-26`); both `mode` spellings
including Python's `interface_mode = gateway` copy/paste bug handled
as a strict superset (`leviculum-std/src/ini_config.rs:795-803`,
against `reference/Reticulum/RNS/Reticulum.py:717-745`); and the rule
that an unknown key never makes lnsd refuse a config rnsd loads
(`leviculum-std/src/ini_config.rs:577-582`).

**D12 — three defaults flip against a config that omits the key, and
each flips a different layer of the daemon.** All three are
documented on our side as choices; what none of the docs state is the
drop-in consequence, which is the same file producing a different
daemon:

1. **`share_instance`** — Python defaults true
   (`reference/Reticulum/RNS/Reticulum.py:287`); ours defaults false
   (`leviculum-std/src/config.rs:63-64`, resolved unchanged by the
   builder, `leviculum-std/src/driver/builder.rs:813-815`). A config
   that never names the key gives rnsd a shared instance and lnsd
   none: every Python tool answers "No shared RNS instance available"
   against lnsd. This is the central drop-in switch defaulting to the
   non-drop-in position.
2. **`enable_transport`** — Python defaults false
   (`reference/Reticulum/RNS/Reticulum.py:254`); ours defaults true
   (`leviculum-std/src/config.rs:51-52`, "lnsd is a daemon"). This
   one is mesh-visible: a key-less config makes lnsd retransmit
   announces and serve path requests where rnsd stays a leaf.
3. **Per-interface `enabled`** — Python synthesizes an interface only
   when `enabled` or `interface_enabled` is explicitly true and skips
   the section otherwise
   (`reference/Reticulum/RNS/Reticulum.py:950` and `:1056`); ours
   defaults the field true (`leviculum-std/src/config.rs:455-456`), so
   a section with no `enabled` key is live under lnsd and dead under
   rnsd.

Class, all three: **deviation ours** — 1 and 2 deliberate per their
field docs, 3 looks like a serde-default convenience that nothing
argues against the reference (the INI path inherits it silently).
Mitigation in practice: every config we ship or generate writes all
three keys explicitly (`example_config`,
`leviculum-cli/src/lnsd.rs:365`). Bears on: the drop-in goal; any
stranger's minimal config. Test: drop-in — a config of nothing but
`[interfaces]` with one keyless section, both daemons, compare
`rnstatus` reachability, transport state, and interface count.

**D13 — `panic_on_interface_error` is silently ignored.** Python
reads it (`reference/Reticulum/RNS/Reticulum.py:571-573`) and an
unrecoverable interface teardown then kills the daemon
(`reference/Reticulum/RNS/Interfaces/LocalInterface.py:360-363` shows
the pattern every interface follows). Ours drops the key into the
tolerated-unknown arm (`leviculum-std/src/ini_config.rs:568`) and the
daemon's interfaces reconnect forever by design. An operator who
configured fail-fast gets keep-trying. Class: **silently ignored
config**. The deviation itself (supervised restarts beat in-process
panic) may well be the better behaviour — but then the key deserves a
startup warning naming it, like the IFAC warnings beside it
(`leviculum-std/src/ini_config.rs:239-283`). Test: unit — parsing a
config with the key logs a visible "read but not honoured" line.

**D14 — tolerant booleans accept files rnsd refuses.** ConfigObj's
`as_bool` raises on anything outside its vocabulary, and rnsd exits
255 on the parse error (`reference/Reticulum/RNS/Reticulum.py:328-333`
via `as_bool` at `:487`); our `parse_bool` maps every unknown
spelling to false (`leviculum-std/src/ini_config.rs:953-958`). A
config with `enable_transport = maybe` is a startup error under rnsd
and a silently transport-less daemon under lnsd. The inverse
direction of D12: we accept what the reference refuses. Class:
deviation ours (unintended edge of a deliberate tolerance). Test:
unit — a bool key with an unrecognised value warns by name, or
refuses like the invalid-line arm does.

**D15 — the discovery position and radio keys are parsed by rnsd and
dropped by us on the PRODUCING side.** Python reads `latitude`,
`longitude`, `height`, `discovery_stamp_value`,
`discovery_frequency`, `discovery_bandwidth`,
`discovery_modulation` per discoverable interface and publishes them
in its discovery announces
(`reference/Reticulum/RNS/Reticulum.py:857-867`); our interface
parser has no arms for them (they fall to the debug-logged unknown-key
arm, `leviculum-std/src/ini_config.rs:835-837`), while our RECEIVING
side faithfully serves exactly these fields out of foreign announces
(`build_discovered_interfaces`,
`leviculum-std/src/rpc/handlers.rs:213`). A configured position on an
lnsd hub simply never goes on the air, and `rnstatus -d` elsewhere
shows "Location: Unknown" for it. Class: **silently ignored config**.
Bears on: discovery parity (Codeberg #32 family). Test: drop-in — one
discoverable interface with latitude/longitude on both daemons, read
the announce on a third node.

**D16 — the global announce-rate and ingress-tuning keys are
tolerated but inert.** Python's `[reticulum]` `default_ar_target` /
`default_ar_penalty` / `default_ar_grace` seed every
transport-enabled interface's announce-rate enforcement
(`reference/Reticulum/RNS/Reticulum.py:622-633` applied at
`:830-833`), and the `ic_*`/`ec_pr_freq`/`egress_control` family
tunes the ingress/egress limiters globally
(`reference/Reticulum/RNS/Reticulum.py:635-677`). We honour the
per-interface spellings (`leviculum-std/src/ini_config.rs:778-788`)
but the global ones fall into the catch-all, so our
transport-enabled defaults are the hardcoded 3600/0/5
(`leviculum-std/src/rpc/mod.rs:798-813` pins them). A config that
raises the global target gets Python's enforcement and ours at the
defaults — different rebroadcast rate limiting on the same mesh.
Class: **silently ignored config**. Test: drop-in — a config with
`default_ar_target` set, `rnstatus -A` against both daemons, compare
the `(t:...)` suffix.

Deliberate, documented deviations, re-confirmed as stated: our
`discover_interfaces` defaults true against Python's false, collecting
being locally observable only (`leviculum-std/src/config.rs:204-219`);
ingress control defaults off for dial-out point-to-point types where
Python is flat-true (`ingress_control_default_for_type`,
`leviculum-std/src/config.rs:969`); the discoverable-mode promotion
fills only an UNSET mode where Python overrides an explicit one
(`promote_discoverable_modes`, `leviculum-std/src/ini_config.rs:357`).
Also ignored without a current consumer: `link_mtu_discovery` (no
link-MTU discovery exists on our side to enable) and
`force_shared_instance_bitrate` (a Python test knob,
`reference/Reticulum/RNS/Reticulum.py:567-569`); both inert-by-absence
rather than misleading.

## 5. What each Python tool sees, summarised

* **`rnstatus`** — connects (storage-derived authkey), renders our
  interface rows with the reference names, modes, rates and radio
  stats; `-l` gets both counts. Visible gaps: `-A` never shows queued
  announces (D7); `-d`/`-D` read the LOCAL discovery registry files,
  not the RPC, and our on-disk registry format is ours — not compared
  in this audit (coverage, below). Config-dependent lockouts: D1, D2,
  D12.1.
* **`rnprobe`** — works end to end (probe destination via
  `respond_to_probes`, timeout from `first_hop_timeout`); loses the
  RSSI/SNR/Q decoration on radio paths (D6) and prints "on unknown"
  where rnsd prints nothing (D9).
* **`rnpath`** — `-t` works with internal interface names (D8); `-r`
  prints an error per row (D4); `-D` claims success and does nothing
  (D5); `-d`, `-x`, `-b`, `-B`, `-U` work with Python's exact return
  semantics.
* **`rncp`** — pure data path over the LocalInterface plus
  `first_hop_timeout`; both verified above; nothing rncp reads is in
  the divergence list.
* **`rnid`** — the `identity_data` retain round-trip now answered
  (closing the 2026-06-29 gap).
* Our own clients (`lnstatus`, `lnprobe`, `lnpath`, `lncp`,
  `lnstest`) drive the same verbs through one helper surface
  (`rpc_query`, `leviculum-std/src/rpc/mod.rs:394`), and periculum's
  `rns` client manifest points the Python tools at lnsd via
  `--client-adapter` — every divergence above is therefore also an
  A/B-validity question, not only a stranger's-first-contact one.

## 6. Wrong claims in our own tree, verified or refuted

1. **"The format is a drop-in: a config that `rnsd` accepts, `lnsd`
   accepts, and the two share the shared-instance IPC socket so
   client tools attach to either daemon without changes"
   (`docs/src/guide/configuration.md:3-12`) — overstated on four
   counts.** A config with `rpc_key` set locks the tools out (D1);
   `shared_instance_type = tcp` on Linux parts the two daemons' IPC
   (D2); a config omitting `share_instance` attaches against rnsd and
   not lnsd (D12.1); and the acceptance claim is also true in a
   direction the sentence does not mean — lnsd accepts bool spellings
   rnsd refuses (D14). The same page's own key table states the
   `share_instance` default `false` honestly
   (`docs/src/guide/configuration.md:104`), so the document contradicts
   its own opening paragraph. *Rewritten in e5f78966*: the paragraph now
   says what the drop-in holds, and a second one says where it does
   not (D12, D2, D14); D1 is fixed in the same commit.
2. **"It is a drop-in replacement for Python's rnsd"
   (`docs/src/man/lnsd.1.md:13-15`) — holds for the deployed
   configuration shape, with the D12 defaults as the unstated
   proviso.** Every config we generate writes the three flipped keys
   explicitly, so the claim is true of our fleet and untrue of a
   minimal foreign config. *Rewritten in e5f78966* to name the D12, D2
   and D14 provisos.
3. **The memory `project_identity_data_rpc_gap` ("the shared-instance
   RPC does not parse the identity_data retain key") — stale,
   refuted.** The arm exists with tests in both codecs
   (`leviculum-std/src/rpc/pickle.rs:381-392`, tests at `:1167-1242`)
   and real retain semantics behind it
   (`identity_data_retain`, `leviculum-std/src/rpc/handlers.rs:1950`).
   The memory is updated alongside this audit.
4. **Claims that held, spot-checked.** The `lnstatus`/`rnstatus`
   byte-identical rendering claim
   (`docs/src/guide/lnstatus.md:5-7`) is pinned by the render-parity
   corpus under `leviculum-cli/src/lnstatus_render/` and was not
   re-measured here. The authkey-derivation comment
   (`leviculum-std/src/rpc/mod.rs:239-244`) matches
   `reference/Reticulum/RNS/Reticulum.py:355-356` exactly. The
   first-hop-timeout formula comment names Transport.py:2700 and the
   reference line is the function it claims
   (`reference/Reticulum/RNS/Transport.py:2700-2703`). The
   interface_stats key citations into `rnstatus.py` resolve to the
   gates they describe.

## The three I would fix first

All three are fixed in e5f78966; see the *Fixed in* note under each.

1. **`rate_table` timestamps (D4).** Mechanism: carry a bounded
   timestamp deque (Python's cap is 16,
   `reference/Reticulum/RNS/Transport.py:96`) on the core's rate
   entries beside the `last`/`violations` already kept
   (`leviculum-core/src/transport.rs:1593`), and serve it in
   `build_rate_table` (`leviculum-std/src/rpc/handlers.rs:1653-1662`);
   the minimum honest interim is `[last]`, which already stops the
   per-row exception. Test: unit — a rate entry answers a non-empty
   float list; drop-in — `rnpath -r` prints a rate line, not an
   error, against both daemons. This is the one divergence where a
   named reference tool is visibly broken today.
2. **The packet radio-stat cache (D6).** Mechanism: a capped
   (512-entry, first written here as 1000, matching `LOCAL_CLIENT_CACHE_MAXSIZE`,
   `reference/Reticulum/RNS/Transport.py:168`) packet-hash → rssi/snr
   map filled where the RNode interface already stamps per-interface
   RSSI (`apply_radio_stat` feeds `radio_stat_fields`,
   `leviculum-std/src/rpc/handlers.rs:335`), answered from the three
   existing verbs (`leviculum-std/src/rpc/handlers.rs:120-137`).
   Test: unit per verb; drop-in — `rnprobe` over the rig pair shows
   RSSI/SNR against both daemons. Directly serves the rig's A/B
   measurement discipline.
3. **The `rpc_key` lockout (D1).** Mechanism: one parse arm in
   `apply_reticulum_key` (`leviculum-std/src/ini_config.rs:389`)
   storing the hex key, used in place of the identity-derived default
   when keying the listener (`leviculum-std/src/rpc/mod.rs:109-126`)
   and the client helpers. Python's fallback-on-invalid behaviour
   (`reference/Reticulum/RNS/Reticulum.py:513-515`) comes along for
   free. Test: unit — configured key authenticates, identity-derived
   key is refused while the config sets one; negative control — no
   key configured keeps today's derivation. Smallest fix of the
   three, and it closes the only divergence that denies ALL tool
   access at once.

D5 (the announce-queue drop no-op) and D7 (the None gauge) are the
next pair and share one seam: both want the core's announce-cap
queues exposed to the RPC layer, so they are one batch, not two.

## Coverage

Compared in full: the RPC server's transport, framing, handshake and
every dispatch arm (`leviculum-std/src/rpc/mod.rs`, `connection.rs`,
`pickle.rs`, `handlers.rs`, non-test halves) against the reference's
`rpc_loop`, its `get_*` producers and the
`multiprocessing.connection` contract; the LocalInterface server,
client and reconnect loop (`leviculum-std/src/interfaces/local.rs`)
against `LocalInterface.py`, and the core's local-client transport
semantics (hop spoofing, announce relay, path-request branches,
plain-broadcast fan-out, disconnect handling) against the
corresponding `Transport.py` paths; the INI parser and the config
model (`leviculum-std/src/ini_config.rs`, `leviculum-std/src/config.rs`)
against `__apply_config`, `_synthesize_interface` and the vendored
ConfigObj's observed rules; the four Python tools' full read paths
(`rnstatus.py`, `rnprobe.py`, `rnpath.py`, `rncp.py` for its RPC
touchpoints). Reference side read in the pinned 1.3.5 tree
throughout; the 1.5.2-shaped keys were taken as our code pins them.

Not compared, and why: the discovered-interface registry's ON-DISK
format against Python's (`rnstatus -d` reads files, not RPC; whether
a Python rnstatus pointed at an lnsd config dir lists our registry is
untested and likely a gap — it needs its own pass with both formats
on disk); the remote-management link protocol (`rnstatus -R`,
`rnpath -R` — a Link/request surface, not the shared-instance IPC
this order names); the I2P, KISS, AX.25 and Serial interface
IMPLEMENTATIONS behind their config keys (the keys were compared, the
carriers belong to the air-path audits); Sideband/Nomadnet as clients
(they exercise the same LocalInterface + RPC surface, nothing
additional was read); and the TOML config dialect (Leviculum-only, no
reference to diff against). Issue leviculum#321 (the
known_destinations round-trip stripping Python-side fields) was taken
as characterised in the order — it lives in the storage round-trip,
not in the RPC answers read here, and was not re-derived.
