# The I2P interface

Users ask for the one interface they believe rnsd has and lnsd lacks:
`I2PInterface`, Reticulum over the I2P anonymous network. This page
records what the reference does, what lnsd already does, where the two
still differ, how both can be tested without the public I2P network, and
the build order for what is left.

The first finding decides the shape of everything else: **lnsd has the
interface.** It landed on 2026-07-04 as `bc0d3be5` (Codeberg #99), speaks
SAM v3 directly with no I2P crate, and was hardened since by #142 (socket
hook), #271 (deframer cap), #284 (time bounds) and 1119 (key file mode).
What it never had is a test against a Python peer, a test bed that runs
without the public network, and the operator-facing surface rnsd gives
(`rnstatus`, discovery, a stable address across a stack switch). The work
left is that, not a port.

## The reference, exactly

Python's interface is `RNS/Interfaces/I2PInterface.py` (1029 lines) on top
of a vendored copy of `i2plib` 0.0.14 (`RNS/vendor/i2plib/`, MIT), a
generic asyncio tunnel library. Line numbers below are against the
reference tree at 1.3.5.

### Configuration

| Key | Read at | Meaning |
|---|---|---|
| `peers` | `reference/Reticulum/RNS/Interfaces/I2PInterface.py:851` | comma list (`as_list`) of `.b32.i2p` addresses, one outbound peer each |
| `connectable` | `reference/Reticulum/RNS/Interfaces/I2PInterface.py:852` | open an inbound endpoint with a persistent address, default no |
| `ifac_size`, `ifac_netname`, `ifac_netkey` | `reference/Reticulum/RNS/Reticulum.py:987` | injected by the config loader, as for every interface |
| `storagepath` | `reference/Reticulum/RNS/Reticulum.py:988` | injected, `<configdir>/storage`; keys go to its `i2p/` |
| the common keys | `reference/Reticulum/RNS/Reticulum.py:879` | mode, outgoing, bitrate, announce cap, ingress control, discovery, as on every interface |

There is **no config key for the SAM bridge address**. It comes from the
environment variable `I2P_SAM_ADDRESS`, default `127.0.0.1:7656`
(`get_sam_address`, `reference/Reticulum/RNS/vendor/i2plib/utils.py:29`;
`DEFAULT_ADDRESS`, `reference/Reticulum/RNS/vendor/i2plib/sam.py:18`).
There are no tunnel options either: every `SESSION CREATE` goes out with
`options={}` (`create_session`,
`reference/Reticulum/RNS/vendor/i2plib/aiosam.py:73-76`), so tunnel length
and count are the router's defaults.

### Keys and the address

The endpoint's private key lives in `<storagepath>/i2p/<hex>.i2p`
(`reference/Reticulum/RNS/Interfaces/I2PInterface.py:90`), as the base64
private key on one line. The file name is
`sha256(sha256(name) + sha256(transport_identity.hash))`, where `name` is
the config section's name (`i2p_dest_hash_nf`,
`reference/Reticulum/RNS/Interfaces/I2PInterface.py:273`); an older
`sha256(sha256(name))` file is preferred when present
(`i2p_dest_hash_of`,
`reference/Reticulum/RNS/Interfaces/I2PInterface.py:269`). A missing file
is filled by `DEST GENERATE SIGNATURE_TYPE=7`, an Ed25519 destination
(`dest_generate`, `reference/Reticulum/RNS/vendor/i2plib/sam.py:78`;
`default_sig_type`, `reference/Reticulum/RNS/vendor/i2plib/sam.py:96`),
written before any session exists. The `.b32.i2p` address is the
lowercase base32 of `sha256(public destination)`, 52 characters.

### The SAM dialect

Every SAM socket opens with `HELLO VERSION MIN=3.1 MAX=3.1`
(`DEFAULT_MIN_VER`, `reference/Reticulum/RNS/vendor/i2plib/sam.py:19`).
Then, per role:

- **peer (client):** a fresh Ed25519 destination by `DEST GENERATE`, a
  `SESSION CREATE STYLE=STREAM ID=reticulum-<6 letters>` with it
  (`generate_session_id`,
  `reference/Reticulum/RNS/vendor/i2plib/utils.py:37`), `NAMING LOOKUP`
  of the `.b32.i2p` name, and `STREAM CONNECT ... SILENT=false`
  (`stream_connect`,
  `reference/Reticulum/RNS/vendor/i2plib/aiosam.py:122`). i2plib proxies
  that stream to a local TCP listener the interface then connects to
  (`ClientTunnel`,
  `reference/Reticulum/RNS/Interfaces/I2PInterface.py:148`).
- **endpoint (`connectable`):** `SESSION CREATE` with the stored key,
  then a loop of `STREAM ACCEPT ... SILENT=false`
  (`stream_accept`,
  `reference/Reticulum/RNS/vendor/i2plib/aiosam.py:149`); the first line
  on an accepted socket is the caller's destination
  (`reference/Reticulum/RNS/vendor/i2plib/tunnel.py:142`), the rest is
  proxied to the interface's local `ThreadingI2PServer`
  (`ServerTunnel`,
  `reference/Reticulum/RNS/Interfaces/I2PInterface.py:306`), which
  spawns one `I2PInterfacePeer` per connection
  (`incoming_connection`,
  `reference/Reticulum/RNS/Interfaces/I2PInterface.py:944`).

`STREAM FORWARD` and `DATAGRAM`/`RAW` sessions are never used.

### On the stream

HDLC framing, byte-identical to the TCP interfaces
(`reference/Reticulum/RNS/Interfaces/I2PInterface.py:656`), with
`HW_MTU = 1064` (`reference/Reticulum/RNS/Interfaces/I2PInterface.py:403`)
and a nominal bitrate of 256 kbit/s (`BITRATE_GUESS`,
`reference/Reticulum/RNS/Interfaces/I2PInterface.py:838`). The upstream
manual calls the I2PInterface and an `i2p_tunneled` TCP interface
"interchangeably compatible", which is the same statement.

### Timers, staleness and reconnect

| What | Value | Where |
|---|---|---|
| keepalive: an empty frame `7E 7E` when write-idle | 10 s | `reference/Reticulum/RNS/Interfaces/I2PInterface.py:692` |
| tunnel state STALE when nothing was read | 20 s | `reference/Reticulum/RNS/Interfaces/I2PInterface.py:681` |
| socket torn down when nothing was read (`I2P_READ_TIMEOUT`) | 110 s | `reference/Reticulum/RNS/Interfaces/I2PInterface.py:394` |
| peer reconnect wait (`RECONNECT_WAIT`), no cap | 15 s | `reference/Reticulum/RNS/Interfaces/I2PInterface.py:386` |
| peer tunnel-setup retry | 8 s | `reference/Reticulum/RNS/Interfaces/I2PInterface.py:493` |
| endpoint tunnel-setup retry | 15 s | `reference/Reticulum/RNS/Interfaces/I2PInterface.py:927` |

The read watchdog (`read_watchdog`,
`reference/Reticulum/RNS/Interfaces/I2PInterface.py:672`) is the only
thing that notices a dead tunnel: the socket goes to the local proxy, and
the proxy stays up whatever happened to the tunnel behind it.

**No SAM error is terminal.** The nine `RESULT` codes are mapped to
exception classes (`SAM_EXCEPTIONS`,
`reference/Reticulum/RNS/vendor/i2plib/exceptions.py:33`), logged by
name, and every one of them, a refused SAM connection included, leads to
"Retrying tunnel setup later". A peer interface is torn down for good
only after `max_reconnect_tries`, which the I2P interface leaves at None.

### What `rnstatus` shows

The status dictionary gets `i2p_connectable`, `i2p_b32` and `tunnelstate`
(`reference/Reticulum/RNS/Reticulum.py:1346-1367`). Measured against rnsd on
the bed below:

```text
 I2PInterface[I2P]
    Status    : Up
    Peers     : 1 connected I2P endpoint
    Rate      : 256.00 kbps
    I2P B32   : p377jcvi6whf5keegjjbtv6mrpruhlyp56lebraya4hv7dvbbkqq.b32.i2p

 I2PInterfacePeer[Connected peer on I2P]
    I2P       : Tunnel Active

 I2PInterfacePeer[I2P to p377jcvi6whf5keegjjbtv6mrpruhlyp56lebraya4hv7dvbbkqq.b32.i2p]
    I2P       : Tunnel Active
```

A non-connectable `I2PInterface[...]` row and the `Connected peer` rows
are hidden unless `-a`. A connectable interface also publishes its b32 as
`reachable_on` in an interface discovery announce
(`reference/Reticulum/RNS/Discovery.py:143`).

## What lnsd has, and where it differs

lnsd's interface is `leviculum-std/src/interfaces/i2p/` with its builder
in `leviculum-std/src/driver/interface_build/i2p.rs`. Production code,
counted without comments and tests: 286 lines of SAM client (`sam.rs`),
510 of interface (`mod.rs`), 90 of builder; 700 lines of tests beside
them. It reads the SAM socket directly instead of proxying through a
second local TCP socket, which changes no byte on the I2P stream. Same
HDLC, same `HW_MTU` (`I2P_HW_MTU`,
`leviculum-std/src/interfaces/i2p/mod.rs:61`), same 10 s keepalive and
110 s read timeout (`I2P_KEEPALIVE`,
`leviculum-std/src/interfaces/i2p/mod.rs:76`; `I2P_READ_TIMEOUT`,
`leviculum-std/src/interfaces/i2p/mod.rs:87`), same 20 s stale mark
(`I2P_STALE_AFTER`, `leviculum-std/src/interfaces/i2p/mod.rs:99`, since
1588), same 15 s reconnect
(`I2P_DEFAULT_RECONNECT_WAIT`,
`leviculum-std/src/interfaces/i2p/mod.rs:64`), same session nick shape,
same `I2P_SAM_ADDRESS` variable
(`leviculum-std/src/driver/interface_build/i2p.rs:29`), and on top a
120 s bound on every SAM command the reference does not have
(`SAM_COMMAND_TIMEOUT`, `leviculum-std/src/interfaces/i2p/sam.rs:73`).

The differences, each measured on the bed below. The first two are closed
since 1576, the third and fourth since 1588:

1. **Signature type, closed.** lnsd asked for `DESTINATION=TRANSIENT`
   without `SIGNATURE_TYPE`, and i2pd 2.56 answers that with a DSA-SHA1
   destination: measured on the private bed, `cert_type=0 sig_type=0`,
   where the same request with `SIGNATURE_TYPE=7` gave `cert_type=5
   sig_type=7`, as rnsd's key does. Since 1576 every TRANSIENT session
   asks for type 7 (`session_create`,
   `leviculum-std/src/interfaces/i2p/sam.rs:365`), the endpoint logs the
   type of the key it serves, and `just i2p-live` checks the type of a
   fresh destination. A key stored before stays what it is: a stored key
   goes to SAM without a type, so no running node changes its address.
2. **Key file name, closed.** lnsd stored the key as
   `storage/i2p/i2p_iface_<index>.i2p`, keyed by the section's position in
   the file, so an rnsd user who switched to lnsd on the same config
   directory got a new address. Since 1576 the key file is named as the
   reference names it, old format first (`resolve_keyfile`,
   `leviculum-std/src/interfaces/i2p/mod.rs:847`), and an existing
   `i2p_iface_<index>.i2p` is renamed to the new name once, logged, so no
   lnsd user loses an address either. Measured on the bed: a DSA key left
   as `i2p_iface_0.i2p` was adopted (`I2P key adopted: .../i2p_iface_0.i2p
   renamed to .../f25f4882...e8882.i2p (interface 'I2P'), the address is
   unchanged`), lnsd served the same b32 with signature type 0, and rnsd
   started next on the same directory published that b32 in `rnstatus`;
   the other way round, the key rnsd wrote (`37afcf9e...d659.i2p`, type 7)
   was the one lnsd served.
3. **Status surface, closed.** lnsd registered no parent
   `I2PInterface[...]` row and emitted none of the three I2P fields;
   Python's `rnstatus` against lnsd showed `i2p_0/<b32>.b32.i2p` and
   `i2p_0_to_<b32>.b32.i2p` at 10 Mbps, and the operator's own b32 only in
   lnsd's log. Since 1588 an I2P section is a parent row in the reporting
   inventory (`add_i2p_parent` in `leviculum-std/src/interfaces/inventory.rs`)
   with `i2p_connectable`, `i2p_b32` and the count of the connections it
   accepted; its configured peers are `I2PInterfacePeer[<section> to <peer>]`
   and add nothing to that count or its bytes (`parent_count = False`), its
   accepted ones are `I2PInterfacePeer[Connected peer on <section>]` and take
   the section's mode as the reference's do
   (`reference/Reticulum/RNS/Interfaces/I2PInterface.py:992`); every peer
   carries `tunnelstate`, and every row the 256 kbit/s guess. A stored key
   names the b32 before any session exists, a fresh one once its first
   `SESSION CREATE` returns. The tunnel state is the reference's: active
   while bytes arrive, stale after more than 20 s without one, logged as
   `I2P_TUNNEL_STALE` and `I2P_TUNNEL_ACTIVE`; the teardown still waits for
   the 110 s read timeout, so nothing on the wire changed. Measured on the
   bed below (`just i2p-live`, stage `rows`): rnstatus against lnsd and
   against rnsd print the same I2P rows, tunnel state aside, and lnstatus
   prints what rnstatus prints against lnsd.
4. **Discovery, closed.** A `discoverable` I2P interface was skipped with
   "is not discoverable" (`descriptor_from_config`,
   `leviculum-std/src/discovery.rs:64`). Since 1588 a connectable one
   publishes its b32, bare as the reference sends it, as `reachable_on`.
   The announce **waits for the endpoint**: its job is not due until the b32
   is known and the payload stamped off the event loop, because an I2P
   announce without `reachable_on` is one the reference's receivers refuse
   (`unpacked[REACHABLE_ON]`, `reference/Reticulum/RNS/Discovery.py:297`,
   raises) and the next one would be an announce interval (6 h by default)
   later. With a stored key the b32 is there at start and the first
   announce carries it; with a fresh key the first announce goes out at the
   first job tick after the session. A section that is not connectable
   publishes none, as in the reference. Measured on the bed: rnsd with
   `discover_interfaces = yes` stored lnsd's I2P interface with its b32 as
   the address.
5. **Endpoint key timing.** rnsd writes its key before any session (by
   `DEST GENERATE`); lnsd writes it when the first `SESSION CREATE`
   returns, which on the bed was 9 to 24 s after start. Harmless on its
   own; it matters for item 2's migration and for any harness that reads
   the key to learn the address.

None of these is a wire-format break.

## Crates or none

The order's prior was zero new crates. The tree already meets it; for
completeness, what exists (crates.io, 2026-11-08, `cargo tree -e normal`):

| Crate | Version, last release | Normal-dep tree | New to our `Cargo.lock` | State |
|---|---|---|---|---|
| `yosemite` | 0.7.0, 2026-03-31 | 45 crates | 1 (itself) | maintained, async SAMv3, MIT |
| `i2p_client` | 0.2.9, 2020-04-14 | 27 crates | 10 | abandoned, no licence field |
| `i2p` | 0.0.1, 2017-04-30 | 7 crates | 1 | abandoned |

`yosemite` is the only live one and would add almost nothing to the lock,
but it would replace 286 working, fuzzed lines with a second abstraction
whose error and timeout behaviour we would have to learn and pin again.
**Decision: no crate.** SAM is a line protocol over TCP; the interface
uses five of its commands.

A Cargo feature `i2p` was asked for. It gates no dependency at all (the
interface uses tokio and the core's HDLC, both always present), so it buys
only the ~900 lines of code out of a minimal binary. Since 1576 it exists,
on by default in leviculum-std, lnsd (leviculum-cli), lnpnd and lnomad;
without it an `I2PInterface` section refuses to start the daemon and names
the feature, as `ble` and `serial` do, and `just lnsd-minimal` builds
without it.

## The test bed

Until 1576 `just i2p-live` started one fresh i2pd that reseeded from the
public network, which made it slow, networked and not a cell. A **private
I2P network** needs neither, and since 1576 it is in the tree:
`scripts/i2p-privnet.sh [N] -- <command>` brings one up, runs the command
with `SAM_1`..`SAM_N` (and `SAM_A`, `SAM_B`) in its environment and tears
it down; `just i2p-live` runs both live tests on it, the loopback test's
two ends on two different routers.

### Measured: a private net on loopback

i2pd 2.56.0 from Debian (`apt install i2pd`; Java I2P is not packaged in
Debian 13). N routers on 127.0.0.1, each with its own data directory
under `~/.i2pd/` (Debian's AppArmor profile forbids `/tmp`), its own
NTCP2 and SAM port, `netid = 97` so nothing can join the public network,
`reservedrange = false` so loopback addresses are accepted, `nat =
false`, `floodfill = true`, SSU2 off, and each router on **its own
loopback address** (`127.97.0.<i>`, `--host` and `--address4`; see the
shared-address trap below). Bootstrap without a reseed server:
start every router once so it writes its `router.info`, stop them, zip
the `router.info` files, restart all with `reseed.zipfile` pointing at
the zip. No root, no docker, no netns.

Times from restart, client sessions with router defaults (`options={}` as
in the reference), one SESSION CREATE on router 2, one on router 1,
`NAMING LOOKUP` of the first's b32 from the second, `STREAM CONNECT`, and
a byte string across:

| Routers | first SESSION CREATE | second | stream round trip done |
|---|---|---|---|
| 5 | 12.0 s | +6.0 s | 18.3 s |
| 5 | 18.0 s | +6.0 s | 24.4 s |
| 5 | 24.0 s | +9.0 s | 33.4 s |
| 5 | 27.0 s | +6.0 s | 33.4 s |
| 5 | 18.0 s | (unhappy run) | |
| 4 | 39.0 s | +15.0 s | 54.4 s |
| 3 | 12.0 s | +12.0 s | 24.4 s |
| 2 | 9.0 s | +9.0 s | 18.4 s |

The bring-up before that is 1.0 to 1.1 s, and SAM answers 0.1 s after the
restart. Every tunnel time is a multiple of 3 s: that is i2pd's tunnel
manager tick, not noise. Two routers already work; whether i2pd then
builds shorter tunnels than its default three hops was not checked. Once
the LeaseSet is published, `NAMING LOOKUP` answers in under 0.1 s.

**The wall for a cell: 60 s from restart to a usable stream, measured
worst 54.4 s; put 120 s in the timeout ledger** so a slow tick sequence
does not make a red.

The tree script on 2026-11-08 (1576, five routers): first start 1.1 to
1.2 s, SAM up 0.1 s after the restart; `just i2p-live` had its server
session after 12.0 s, the client session on the second router and the
stream after 23.0 s, the frame across after 23.1 s, both tests done in
38.1 s.

**The shared-address trap.** 1568's bed put every router on 127.0.0.1.
i2pd refuses an inbound NTCP2 handshake while another from the same IP is
still pending, and says so: `NTCP2: Incoming session from 127.0.0.1 is
already pending`. On one address every router is the same IP to every
other, and at the start all of them dial at once. In 15 runs of the A/B
below on that bed the line appeared 1 to 10 times per run, and the runs
with the most of it were the bad ones: 10 refusals and 44 failed tunnel
tests in the run that took 110.7 s to its first probe; 7 refusals and 85
failed tunnel tests in the one run whose link never carried a byte (below).
With one address per router the line is gone (0 in 20 runs) and so was
the dead run.

### Measured: rnsd and lnsd against each other

The A/B driver is the same for both stacks: one config text (rnsd
ignores lnsd's `[auto]` section, which turns the zero-config media off),
a connectable daemon S on router 2 and a `peers =` daemon C on router 1,
`respond_to_probes = yes`, and Python's `rnprobe` against C's shared
instance toward S's probe destination. S's b32 is read from the key file
S writes. Five routers.

| S | C | first probe after C start | then 10 probes |
|---|---|---|---|
| rnsd | rnsd | 26.4 s | 10/10 |
| rnsd | lnsd | 27.4 s | 10/10 |
| lnsd | rnsd | 38.9 s | **6/10** |
| lnsd | rnsd | 17.7 s | 10/10 |
| lnsd | rnsd | 33.1 s | 10/10 |
| lnsd | lnsd | 9.4 s | 10/10 |

All four arms interoperate. 1568's 6/10 run: probes 1-5 answered in 2 to
10 ms, probe 6 in 10.3 s, 7-10 never; neither daemon logged a stream loss,
and the run's i2pd logs went with the bed. Two reruns of the same arm were
clean.

1576 reran the arm, S=lnsd C=rnsd, with i2pd at `loglevel=info` and every
bed kept (`/home/lew/scratch/1576`, driver `ab.sh`). **On the 1568 bed (one
address), 15 runs:** 14 came up, all 14 answered 10/10 (140/140), first
probe 8.9 to 110.7 s after C's start; one never carried a byte. In that run
rnsd's tunnel reported setup complete 6 s after its start, then `I2P tunnel
became unresponsive` 22 s later, a broken pipe at 110 s, a rebuild, the
same again every two minutes; lnsd as S logged its endpoint and never one
accepted stream. The routers name the cause below both daemons: C's router
logged `Streaming: Resend #1` to `#4` and `Streaming: No packets have been
received yet` for the SYN of that stream, and all five routers failed 85
tunnel tests between them, 34 on S's. Neither daemon lost a packet that
reached it; the stream never reached S. **On the per-address bed, 20
runs:** 200/200, first probe 8.9 to 72.7 s, no refusal line. The three
slow ones (42.7, 62.2, 72.7 s) are rnsd's `NAMING LOOKUP` answered
`INVALID_KEY` before S's LeaseSet had spread, then rnsd's own retry; no
probe was lost in any of them.

The 6/10 shape itself (a link that carried probes and then went silent)
did not recur in 34 links that came up; its logs are gone, so its cause
cannot be shown, but it is the shape a stream takes when the tunnels under
it fail as above. In three of four runs
with lnsd as S, `rnstatus` against S printed nothing within 20 s
(against rnsd as S it printed both times); also unexplained, and not
I2P-specific until shown.

Two harness traps found on the way, both of which a cell must avoid:
`rnprobe` exits 0 when `timeout` sends it SIGTERM (RNS's signal handler
calls its clean exit), so the verdict has to be its "Sent N, received M"
line, never its exit code; and rnsd's stdout is block-buffered into a
file, so its log needs `PYTHONUNBUFFERED=1`.

### Measured: the unhappy paths

- **No router.** `I2P_SAM_ADDRESS` at a closed port, 50 s. Neither stack
  stops trying or crashes. rnsd retries the peer every 8 s and the
  endpoint every 15 s; lnsd both every 15 s, with the interface offline.
- **Destination unknown.** `NAMING LOOKUP` of an unpublished b32 answered
  `RESULT=INVALID_KEY` after 6.4 s (not `KEY_NOT_FOUND`).
- **Destination gone after lookup.** `STREAM CONNECT` to the full
  destination of a session that had closed 3 s earlier answered
  `RESULT=OK` at once. A SAM `OK` is therefore no proof the peer is
  there; only the read watchdog finds out.
- **SAM refuses.** A second session under a taken nick answered
  `DUPLICATED_ID`, a connect on an unknown nick `INVALID_ID`, both at
  once.
- **Stale tunnel.** S's router frozen with SIGSTOP for 130 s after the
  link was up, then thawed. lnsd as C saw its stream end 82 s into the
  freeze, rebuilt it 21 s later (the rebuild succeeded while S's router
  was still frozen, the same `OK`-without-peer as above), and the first
  probe after the thaw answered 9.7 s later. rnsd as C answered the first
  probe 0.5 s after the thaw; its log was buffered, so how it got there
  is not observed.
- **Stale mark (1588).** S=lnsd on router 2 frozen for 45 s with an lnsd
  and an rnsd client on router 1: lnsd's `I2P_TUNNEL_STALE` came 11.9 s
  into the freeze with `idle_ms=20998` (the last keepalive had arrived 9 s
  before it), its `Tunnel Unresponsive` in rnstatus the same second, no
  teardown before the thaw, and `I2P_TUNNEL_ACTIVE` 1.0 s after it. In a
  first run rnsd's client turned `Tunnel Unresponsive` in the same second
  as lnsd's (17.3 s in); in the second it already read so at the freeze,
  as 1589 saw (its state lags its link).
- **Restarted server (1588, from 1589's 420 s observation).** S stopped
  and started again on its key, with a client of each stack that had
  carried a probe before (old), and one of each started with the restart
  on another router (fresh): 0 of 4 reached S in 240 s, with rnsd as S and
  with lnsd as S. A fresh client that never knew the old tunnels fails as
  the old one does, so this is no reconnect gap in either client. With
  lnsd as S and i2pd at `loglevel=info`, S's router logged `Local
  destination <S>.b32.i2p exists` at the restart and from then on `Can't
  publish LeasetSet because destination is not ready` with `Publish
  confirmation was not received` every 1.8 s (132 times in 4 min): i2pd
  2.56 hands the new session the destination the closed one left behind
  and never publishes its LeaseSet. The rnsd-as-S run was at
  `loglevel=warn` and logged no such line, so its mechanism is not shown.
- **Restarted server, the mechanism and the hold (1654).** i2pd 2.56
  (source at tag 2.56.0, fb90b01f) defers the destruction of a closed SAM
  session by five seconds (`SAMBridge::CloseSession`,
  `i2pd/libi2pd_client/SAM.cpp:1484-1491`, `sam.singlethread` defaults to
  true, `i2pd/libi2pd/Config.cpp:174`). The session's destructor deletes
  and stops its local destination (`~SAMSingleSession`,
  i2pd/libi2pd_client/SAM.cpp:1261-1264,
  `ClientContext::DeleteLocalDestination`,
  i2pd/libi2pd_client/ClientContext.cpp:372-385), but until then the
  destination stays in the router's table keyed by its ident hash, and a
  `SESSION CREATE` with the same key is handed it (`Local destination ...
  exists`, i2pd/libi2pd_client/ClientContext.cpp:402-411). Five seconds
  after the old control socket closed, the old session's destructor stops
  that destination under the new session: its pending `STREAM ACCEPT` is
  reset without its socket closing (`I2P acceptor has been reset`,
  i2pd/libi2pd_client/SAM.cpp:1136), its tunnel pool is gone, and every
  publish attempt ends in `destination is not ready`. The session id plays
  no part (lnsd already takes a fresh random one per session,
  `generate_session_id`); the reuse is by key. The reference creates its
  endpoint session as soon as the transport identity exists
  (`RNS/Interfaces/I2PInterface.py:264-310`) and retries only on an error
  status, which this state never raises, so it has the same exposure. lnsd
  now holds its first session on a stored key for `I2P_STORED_KEY_HOLD` =
  7 s (`interfaces/i2p/mod.rs`); a fresh key cannot collide and an
  in-process rebuild already waits the 15 s reconnect interval. Measured
  with `just i2p-live` stage `restart` (`I2P_LIVE_STAGES=restart
  RESTART_STACKS=lnsd RESTART_WALL_S=150 LOGLEVEL=info`, knob
  `RESTART_GAP_S`), S=lnsd on router 2, one old and one fresh client of
  each stack, probe wall 150 s:

  | lnsd S | gap | router reused the destination | SESSION CREATE answered | old lnsd | old rnsd | fresh lnsd | fresh rnsd |
  |---|---|---|---|---|---|---|---|
  | before the hold | 2 s | yes, acceptor reset at +5 s | 2 ms (no tunnel build) | no | no | no | no |
  | before the hold | 60 s | no | not kept | 85.9 s | no | 17.4 s | no |
  | with the hold | 2 s | no | 7 s hold + 12 s | 19.3 s | no | 144.4 s | no |

  With the hold the endpoint comes up exactly as after a long pause. The
  remaining delay is the clients': a client that dials while the old
  LeaseSet is still in the network gets a stream that `STREAM CONNECT`
  answers OK with nothing behind it, goes stale at 20 s and is torn down
  at the read timeout (100 s after it came up on lnsd, 102 s on rnsd), and only its next
  dial reaches the new destination (lnsd's fresh client at 144 s, both
  rnsd clients re-established at about 130 s, after the 150 s probe wall
  had little left). The `not ready` count in the router log is no
  indicator on its own: the stopped old destination keeps logging it
  after it has been replaced; `reused=` is.

## The cells

All on the private bed, all A/B where a stack is involved, one driver.

Happy:

- `i2p_peer_link` — four arms (S, C) in {rnsd, lnsd}²: C reaches S's
  probe destination, then 20 probes, 20/20.
- `i2p_connectable_by_b32` — the address S announces in `rnstatus` (and,
  after item 4, in discovery) is the one C dials, and it is stable across
  a restart of S.
- `i2p_key_dropin` — rnsd starts on a config directory, stops; lnsd starts
  on the same directory and publishes the same b32 (needs item 2). And the
  reverse.

Unhappy:

- `i2p_router_absent` — no SAM bridge: daemon up, interface offline,
  retrying, no crash; the router started 60 s later and the link comes up
  within the cell wall.
- `i2p_sam_refuses` — deterministic, so an mvr against the existing
  mock SAM (`leviculum-std/src/interfaces/i2p/tests.rs`), not a periculum
  cell: `I2P_ERROR`, `DUPLICATED_ID`, `INVALID_KEY` and a HELLO
  `NOVERSION` each lead to a retry, none to a dead interface. Since 1588:
  the `i2p_sam_refuses_*` tests in
  `leviculum-std/src/interfaces/i2p/tests.rs`.
- `i2p_peer_unreachable` — `peers =` an unpublished b32: `INVALID_KEY`,
  retried, the daemon's other interfaces unaffected.
- `i2p_tunnel_stale` — freeze S's router past the 110 s read timeout,
  thaw, a probe answers within 30 s; on both C stacks, with C's log
  showing the teardown and the rebuild.

## Items, in build order

| # | Item | Coder hours | Depends on |
|---|---|---|---|
| 1 | Private bed as a script (`scripts/i2p-privnet.sh`), and `just i2p-live` on it instead of the public network (**done, 1576**) | 2 | — |
| 2 | (**done, 1576**) Ed25519: `SIGNATURE_TYPE=7` on every TRANSIENT session; an existing DSA key is kept, so no address changes; mock-SAM test on the command, live check on the stored key's type | 1 | — |
| 3 | (**done, 1576**) Key file named as the reference names it (section name and transport identity, old-format fallback), and an existing `i2p_iface_<n>.i2p` adopted once so no lnsd user loses an address | 2 | — |
| 4 | (**done, 1588**) Status surface: a parent `I2PInterface[<name>]` row with `i2p_b32`, `i2p_connectable` and its client count; peer rows named as the reference names them; `tunnelstate` from the read clock (20 s stale); bitrate 256 kbit/s | 4 | 3 |
| 5 | (**done, 1588**) Discovery: a connectable I2P interface publishes its b32 as `reachable_on` once known (runtime value, not config) | 2 | 3 |
| 6 | periculum cells above, with i2pd in the cell image or host-run on the bed | 6 | 1, 2, 3, 4 |
| 7 | (**done, 1576**) `i2p` Cargo feature, default on, if the minimal build wants the code out (gates no dependency) | 1 | — |
| 8 | (**done, 1576**) Root-cause the 6/10 run: `i2p_peer_link` S=lnsd C=rnsd 20×, beds kept, i2pd at `loglevel=info`; found the bed's shared address, 200/200 without it | 2 | 1 |

Items 1, 2, 3 and 7 are independent and fit one batch. Nothing here
touches the core: every item stays inside the interface, its builder and
the reporting inventory, as [Interface isolation](interface-isolation.md)
requires.
