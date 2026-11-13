# Loquax: reaching the mailbox from anywhere

Status: concept. Ways A and B were decided by Lew on 2026-10-31 (order
1195, Refs #232, #471) and built in order 1196; "Decided" near the end
records the points that order closed. Lew widened the page on
2026-11-04 (order 1402): the mailbox lives on whatever device of the
user stays on the network that matters to them (the internet, the LAN or
their own LoRa mesh), the phone reaches it on the most direct way there
is, automatically and with nothing to configure, and falls back to the
indirect ways only when nothing direct works. "We should cover all the
ways." This page is that ladder, the race that runs it, and the slices
that build it. The mailbox itself is [the home node](loquax-home-node.md).

## The problem

A Loquax Mailbox runs at home, on a Raspberry Pi or a PC behind the
household's internet router, or on a public server, or on a phone. The
device that reads from it is somewhere else: on mobile data, in an
office WiFi, in a hotel, in the same room, on the same computer. Both
usually sit behind a router that translates addresses (NAT). Such a
router lets its devices call out, and refuses every call that comes in
unasked. So the phone cannot call the mailbox, and the mailbox cannot
call the phone. The usual cures are a fixed public address, a DynDNS
name with a forwarded port, or an overlay network such as a VPN. Most
users have none of them, and Loquax must not ask for any.

Reticulum already holds the fallback that always works: both ends call
out to a third machine that both can reach, and that machine carries the
traffic between them, still end to end encrypted (B, the rendezvous).
It costs a relay hop, the rendezvous's bandwidth, and a stranger's view
of both IP addresses. Every more direct way that happens to exist saves
those, and many exist more often than one would think: the same
computer, the same room, the same WiFi, a household with IPv6, a router
that grants a port on request. The ladder below lists them, most direct
first; the race after it runs them all at once and keeps the best that
answers. The user never configures any of them: the join code carries
what the device needs, and what it learns later it learns on its own.

## The ladder

| rung | way | header word | status |
|---|---|---|---|
| 0 | same machine: the shared instance, or in-process | local | built (desktop); verified by slice 1 |
| 1 | Bluetooth, our Columba mesh link | Bluetooth | needs the phone's BLE ([phone node](loquax-phone-node.md), step 3); slice 5 |
| 2a | the same WiFi by AutoInterface (E) | home WiFi | built on lnsd; on the phone with the phone node's step 2 |
| 2b | the same WiFi by the remembered LAN address | home WiFi | slice 3 |
| 2c | the same WiFi by mDNS/DNS-SD `_loquax._tcp` | home WiFi | later, after 2b's numbers |
| 3 | IPv6 direct, the address learned over the relayed link | direct | slice 6 |
| 4 | router pinhole, automatic (UPnP-IGD, PCP, NAT-PMP) | direct | slice 7 |
| 5 | UDP hole punching, the IPv4 half of the old C | direct | after the measurement, slice 9 |
| B | the rendezvous, a public transport node (default fallback) | relayed | built (1196) |
| 6 | the rendezvous on 443 behind TLS | relayed | slice 10 (client); operations are the reviewer's |
| A | the general mesh, any path over any media; 9, the own board as a bridge, is its off-grid case | mesh | built (1196) |
| 7 | the mailbox's endpoint list (not a way: how the device learns 2b, 3 and 4) | | slice 2 (over the link), slice 8 (in the announce) |
| 8 | I2P or Tor hidden service | relayed | under Advanced, never built on the main path |
| D | a fixed address or a hand-made pinhole | direct | built, under Advanced (`home_address`) |

The rung number is the tie-break of the race, not a sequence: all
candidates start at once (below, "The race").

One fact shapes everything after it. **A Reticulum node keeps one path
per destination, and the path follows the announces, not the fewest
hops.** An announce with an equal or lower hop count replaces the path
only when it was emitted later than the one the path holds
(`reference/Reticulum/RNS/Transport.py:1765`); one with more hops
replaces it when the path has expired or the announce is newer
(`reference/Reticulum/RNS/Transport.py:1776-1800`). A link then runs
along the path that stood when it was opened. So two ways up at once do
not by themselves make the direct one carry the traffic: the race below
steers by bringing interfaces up and taking them down on the device's
own node, which Loquax already owns on a phone (the rendezvous rotation,
`loquax/src/home/rendezvous.rs`), and by reopening its links when the
path's interface changes. Nothing medium-aware enters transport
([Interface isolation](interface-isolation.md)).

### 0. Same machine: local

- **Mechanism.** When Loquax and the mailbox run on one computer, both
  are clients of the same daemon's shared instance, an abstract AF_UNIX
  socket `\0rns/<instance_name>` (`shared_instance`,
  `leviculum-std/src/config.rs:102-121`); TCP loopback only where a
  configuration asks for it, as Python does
  (`reference/Reticulum/RNS/Reticulum.py:683`). The path request and the
  link run inside the daemon over its local interfaces: zero network
  hops. A packet from a local client counts no hop at the daemon
  (`reference/Reticulum/RNS/Transport.py:1482-1484`), so the path reads
  `hops=0`.
- **What the code does today (read 2026-11-04, not yet measured).** A
  desktop Loquax that is a client of a mailbox attaches to the shared
  instance as `Role::Client` (`attach_as`, `loquax/src/shell.rs:1826`;
  `Upstream::Shared`, `loquax/src/engine/shell.rs:433`) and dials
  nothing: the only callers of the rendezvous rotation (`dial`,
  `loquax/src/engine/shell.rs:616`) are the leaf, the phone's own
  mailbox and the mailbox daemon. The daemon on a shared instance dials
  its rendezvous as a peer (`DaemonUpstream::Shared`,
  `loquax/src/home/daemon.rs:381`), which it needs for devices that are
  elsewhere, not for the one beside it. So on one lnsd the client should
  find the mailbox through the local interface with no rendezvous
  dialled. Slice 1 turns this reading into a cell before anything else
  is built on it; a client that dials the rendezvous anyway is the
  ladder's first bug.
- **The opposite gap.** A desktop client on a laptop away from home has
  no rendezvous of its own today: it reaches the mailbox only if its
  lnsd has a path (a configured interface, or the server list of
  [zero config](zero-config.md), step 3, once built). Slice 1 records it;
  the race closes it for desktop clients that run without a shared
  instance of their own.
- **A phone that is its own mailbox's device** (1264's case): the
  screens' leaf dials the phone mailbox's node on a loopback port
  (`loquax-android/src/mailbox.rs:552`). That is a socket, but it never
  leaves the phone, and no network change touches it. It reads "local".
  An in-process channel instead of loopback TCP saves a socket, not a
  hop; it is a later cleanup, not part of the ladder.
- **Who sees what:** nobody outside the machine.
- **Failure:** the mailbox stops. The client must report the mailbox
  unreachable and must not fall back to dialling the internet for a
  destination that lives on its own machine (slice 1's unhappy cell).
- **Power:** none beyond the daemon's.

### 1. Bluetooth

- **Mechanism.** The mailbox is a board or a computer with Bluetooth in
  range. Our Columba mesh link is the shortest way there is: one BLE
  hop. The BLE interface reports a peer's arrival and departure to the
  driver (`report_peer_up`, `leviculum-std/src/interfaces/ble/mod.rs:1804`;
  `report_peer_lost`, `leviculum-std/src/interfaces/ble/mod.rs:1780`).
  When the path to `loquax.home` stands on the BLE interface, the race
  names the way Bluetooth and drops the rendezvous. The device needs no
  list of the mailbox's BLE identity: the path's interface tells it.
- **Who sees what:** someone in radio range sees that two devices talk
  (advertisements, connection timing); the content is the link's,
  encrypted end to end.
- **Failure:** out of range. The BLE link expires, `report_peer_lost`
  fires, the race sees the kept way gone and brings every other rung up
  at once (slice 5's unhappy cell: no message lost on the way).
- **Power:** scanning dominates on a phone ([phone node](loquax-phone-node.md),
  section 2); BLE is used only when the phone's BLE medium is on anyway,
  never switched on by the race.
- **Status:** on a desktop, lnsd's BLE carries it today, and the client
  sees it as a path like any other. On a phone it waits for the phone
  node's BLE (step 3 there).

### 2. The same WiFi, three ways

**2a: AutoInterface (E, built on lnsd).** The device's AutoInterface
finds the mailbox's daemon on the local network over IPv6 link-local
multicast ([zero config](zero-config.md), #471), and the path request is
answered over the LAN, one relay away. Nobody outside sees anything.
When the WiFi is gone, the device is back on B. Power: the LAN costs
less than the cellular modem, but AutoInterface's own announces every
1.6 s and the multicast lock on Android do not
([phone node](loquax-phone-node.md), section 2). On a phone it comes with
the phone node's step 2.

**2b: the remembered LAN address (proposed first of the two additions).**
Many WiFi routers block or thin out multicast between clients, and
Android filters it while the screen is off. The mailbox's phones' port
(`loquax/src/home/phone_server.rs`) is a TCP server in access point mode
on the mailbox's host; the device learns its LAN address from the
endpoint list (7) and remembers it per network. When the device joins a
network it has remembered an address for, it dials that address at
once. It costs one TCP connect per network change and needs no
multicast, no permission and no new code on the mailbox's side beyond
the list.

- **The network's identity** is the key the address is remembered
  under. Android gives the default network's `LinkProperties` without a
  permission: the gateways of its routes (the router's IPv4 address and
  its IPv6 link-local address, which many routers derive from their MAC
  and keep), the DNS servers and the search domain. The SSID and BSSID
  are redacted without the location permission (Android 8.1 and later),
  so the key does not use them. Linux gives the gateway's MAC from the
  neighbour table (`/proc/net/arp`, `ip neigh`) without privileges, and
  the SSID through NetworkManager. The key is a hash over what the
  platform gave; a key that changes (a new router) costs one failed
  remembered dial and one fresh list. What exactly Android hands over is
  the first thing slice 3 measures on a device, not an assumption it
  builds on.
- **Failure:** the address now belongs to another host, or nothing
  listens: the connect fails or the link to `loquax.home` does not come
  up within the race's window; the way is backed off for this network
  (below) and the rendezvous stays.

**2c: mDNS/DNS-SD `_loquax._tcp` (later).** The mailbox registers its
phones' port as a service; the device browses for it. It works without
IPv6 multicast but needs IPv4 multicast (224.0.0.251), which the same
routers often thin out as well, and on Android `NsdManager` holds the
WiFi awake while it browses. It finds a mailbox the device has never
seen on this network, which 2b cannot. Built only if slice 3's numbers
show many first visits to a home WiFi without a usable list.

### 3. IPv6 direct

- **Mechanism.** The mailbox tells the device its global IPv6 address
  and the phones' port in the endpoint list (7); the device dials it
  directly over its own IPv6 (German mobile networks hand out IPv6). The
  rendezvous is only the signalling channel, and the signal is the
  relayed link itself: the list travels over it as a request, so the
  rendezvous needs no code of its own and a Python `rnsd` still serves.
  DS-Lite households, common in Germany, have no IPv4 way in but do have
  this one.
- **How the mailbox learns its address:** from its own interfaces, the
  global-scope IPv6 addresses that are not temporary privacy addresses
  (on Linux the `mngtmpaddr` or stable address, not `temporary`; on
  Android `LinkProperties`). The address a rendezvous observed is not
  used: a Python rendezvous does not report it, and the mailbox's dial to
  the rendezvous may leave over IPv4.
- **The router's inbound IPv6 firewall.** Most home routers drop
  unsolicited inbound IPv6 by default. Either rung 4 opens a pinhole
  (PCP or UPnP-IGD's `WANIPv6FirewallControl`), or the user has opened
  the port by hand (D), or the dial times out and the way is backed off;
  then 5 or B carry it.
- **Who sees what:** the device's and the mailbox's network operators,
  as for any connection; no relay.
- **Failure:** the address is withdrawn (a prefix change, which some
  German providers do daily), or the firewall drops the connect. The
  link falls to the next rung; the list is fetched again on the next
  relayed link.

### 4. Router pinhole, automatic

- **Mechanism.** The mailbox asks its router for an IPv4 port mapping or
  an IPv6 firewall pinhole for its phones' port, and puts the result
  into its endpoint list. D becomes automatic, with nothing under
  Advanced for the common case.
- **Protocol subset:** PCP (RFC 6887), opcode `MAP` only, for IPv4 and
  IPv6; NAT-PMP (RFC 6886), the mapping requests only, as PCP's IPv4
  fallback; UPnP-IGD, `AddAnyPortMapping` (v2) or `AddPortMapping` (v1)
  for IPv4 and `AddPinhole` of `WANIPv6FirewallControl` for IPv6, with
  `GetExternalIPAddress`. Nothing else of either protocol. In the
  interface layer of the mailbox's host, Rust, std, no `no_std`. Slice 7
  picks between an existing crate and our own subset after reading the
  candidates' code and licences; the subset above is small enough for
  either.
- **Renewal:** request a two-hour lifetime, renew at half the granted
  one (routers grant less, or ignore the request), and map again after
  every change of the mailbox's own network and after a router restart
  (a PCP epoch that went backwards).
- **When the router says no** (no gateway answers, the protocol is off,
  the request is refused): logged once as `PINHOLE proto= result=refused
  code=`, asked again after an hour and on the next network change,
  never in a loop. The way stays absent from the list; nothing else
  changes.
- **Who sees what:** the router, and anyone who scans the public address
  finds an open port that speaks Reticulum: a TCP server in access point
  mode that relays nothing to strangers, since transport is off on a
  mailbox host's node and the access point mode carries no announces.

### 5. UDP hole punching

The IPv4 half of the old C: both ends send UDP to each other's public
address and port at the same moment, the rendezvous's relayed link as
the signalling; the NATs see an outbound flow and let the answer in. It
needs a UDP interface with punching on both sides, and fails on a
symmetric NAT (carrier-grade NAT on mobile networks often is one). It
saves a relay hop, not the phone's battery: a held connection costs the
same. It stays behind the measurement this page demanded from the start
(below, "What is measured").

### B: the rendezvous (relayed; the default that always stands)

- **The phone** connects to a public Reticulum transport node over TCP,
  on that node's **access point** port, asks for the path to its mailbox
  destination (`loquax.home`) and opens a link to it.
- **The mailbox** keeps an ordinary TCP connection to the same node, as
  a normal peer.
- **The rendezvous** answers or forwards the phone's path request and
  relays the link. An access point interface never carries announce
  broadcasts to the client (`reference/Reticulum/RNS/Transport.py:1193-1196`),
  so the phone is not woken by the network's announce traffic, and
  because the mode is in `DISCOVER_PATHS_FOR`
  (`reference/Reticulum/RNS/Interfaces/Interface.py:54`) the node looks
  for a path it does not know on the phone's behalf
  (`reference/Reticulum/RNS/Transport.py:2918`,
  `reference/Reticulum/RNS/Transport.py:3015`). The mailbox's daemon
  receives the forwarded request and hands it to its local client,
  Loquax (`reference/Reticulum/RNS/Transport.py:3043-3048`), which
  answers because the destination is its own
  (`reference/Reticulum/RNS/Transport.py:2940`).
- **Hops:** two relays, the rendezvous and the mailbox host's daemon.
- **Who sees what:** the rendezvous sees destination hashes, packet
  sizes and timing, and the two IP addresses. It sees no content and no
  name: the link is end to end encrypted. Since 1196 `loquax.home` is
  announced (below, "Decided"), so the rendezvous also learns that this
  identity, whose key the LXMF announce publishes anyway, runs a Loquax
  mailbox.
- **Failure:** rendezvous down: the phone and the mailbox each move to
  the next entry of the mailbox's rendezvous list (below). Mailbox
  offline: the path request goes unanswered and the phone shows the
  mailbox as unreachable, as for a propagation node
  ([the propagation node page](loquax-mailbox.md), "What unreachable
  means"). A symmetric NAT on either end does not matter, since both
  ends only call out.
- **Power:** one TCP connection, which is what any push channel costs,
  once the long TCP and link keepalives of order 1176 are in. The
  measurement plan is the 1177 power design: the default keepalives
  today (a 5 s TCP probe, a link keepalive of `rtt × 360 / 1.75` seconds,
  `reference/Reticulum/RNS/Link.py:845`) keep a cellular modem awake.

Our own node, `leviculum.network`, is preconfigured; the Android app
already ships it as its default server (`DEFAULT_SERVERS`,
`loquax-android/src/mailbox.rs:42`). The list may hold several. Anyone
who runs a mailbox on a public server has a rendezvous for their own
phones by that fact, and other operators may offer one. Order 1190
hardens the rendezvous (caps per source, silent clients timed out).

### A: the general mesh (always possible, never degraded)

- **The phone** may sit on node A and the mailbox on node B, two nodes
  that only have a mesh path between them: TCP, LoRa, BLE, anything.
  The phone does exactly what it does in B: an explicit path request,
  then a link. It accepts any hop count the transport delivers.
- **The mailbox** does nothing different.
- **The nodes** carry the path request, the path response and the link
  over N hops with the ordinary Reticulum mechanics.
- **Hops:** N, whatever the mesh offers.
- **Who sees what:** every relay on the path, what the rendezvous sees
  in B.
- **Failure:** no path between A and B: unreachable, as above. A LoRa
  hop makes the link slow, and the client must not time out a request on
  a TCP clock.
- **Power:** the same as B on the phone; the radio hops cost the relays.

**The catch, and it is real.** Node A searches for the unknown path
because the request arrived on its access point port. A further relay
that receives the forwarded request on an ordinary (`Full`) interface,
holds no path and has no local clients, re-originates nothing: the
reference gates the search on the receiving interface's mode
(`reference/Reticulum/RNS/Transport.py:2918`), and so does lnsd
(`discovers_paths`, `leviculum-core/src/traits.rs:115`;
`leviculum-core/src/transport.rs:12958`). lnsd widens the gate only for
interfaces that report live peers, and today only BLE does
(`leviculum-std/src/interfaces/ble/mod.rs:1812`). Without an announce of
`loquax.home` no relay holds its path either. So A worked only when the
mailbox's own daemon was node A's direct neighbour, and failed when a
third relay stood between them. 1196 closes it by announcing
`loquax.home` (below, "Decided"); its cells show both, red with the
announce held back and green with it.

**9. The own board as a bridge** is A's off-grid case, named here so
the reader finds it on the ladder: the phone talks by Bluetooth to its
own LNode, the LNode by LoRa to the mailbox's LNode, and the mailbox's
host sits behind that. Nothing new: the path request and the link run
over BLE and LoRa as over any mesh, the client's timeouts already scale
with the path (rule 3), and the header says "mesh".

### 6. The rendezvous on 443 behind TLS

- **Mechanism.** Hotel WiFi, company networks and some carriers let only
  443 through. The rendezvous is reachable on 443 behind TLS as a second
  entry of the shipped list, the way that works where B on 4343 does
  not. On the client: a TCP client interface wrapped in TLS (rustls,
  the server's certificate checked against the web PKI and the host
  name), framed as the TCP interface frames today; nothing else changes
  above it. WebSocket over TLS only if a measured network turns out to
  pass TLS but not a non-HTTP stream on 443.
- **Operations** on `leviculum.network` (the certificate, the listener on
  443 in front of the access point port) are the reviewer's, not this
  page's.
- **Who sees what:** as B; the network in between sees a TLS session to
  `leviculum.network` and its size and timing.
- **Failure:** a TLS-intercepting proxy presents another certificate:
  the client refuses the session and says so once
  (`RENDEZVOUS_TLS refused reason=certificate`); it never accepts a
  certificate it cannot verify.
- **Power:** as B, plus the TLS handshake on each reconnect.
- **In the race:** started together with B, not after it; on a network
  where B answers first, the 443 connection is dropped as soon as B
  stands (the tie-break prefers B, which costs no TLS).

### 7. The mailbox's endpoint list

Not a way of its own: how the device learns 2b, 3 and 4.

- **Content:** a short list of `(kind, address, port)`: the LAN address
  per IP family, the global IPv6 address, the pinholed or mapped address
  and port, an I2P destination when configured, and a counter that grows
  when the list changes.
- **First over the link (slice 2).** An admitted device asks for it with
  a request on its link to `loquax.home` (`/endpoints`), as it asks for
  everything else; the mailbox checks membership as on every request.
  Only admitted devices see it, nothing new travels in the clear, and the
  rendezvous's relayed link is exactly the signalling channel rungs 3
  and 4 need.
- **Then in the announce (slice 8).** The generalisation: `loquax.home`'s
  announce carries the list in its app data, signed as the announce is,
  so every way that delivers an announce teaches the device every direct
  way, and the rendezvous stays the bootstrap. `loquax.home` is announced
  without app data today (`announce_home`, `loquax/src/home/daemon.rs:290`),
  and no Python program registers a handler for it, so a Python peer
  relays the announce and ignores the blob. The LXMF delivery announce
  is not touched: its app data is LXMF's.
- **Size.** An announce without ratchet and app data is about 167 bytes
  in a header-1 packet, a ratchet adds 32, a transport header 16. A
  plaintext list of three endpoints is about 40 bytes in msgpack; the
  target is the whole announce in one 255-byte LoRa frame. Slice 8
  measures the real sizes before choosing what goes in.
- **What it leaks.** In plaintext, anyone who hears the announce learns
  the household's global IPv6 address and its open port, and the mesh
  carries announces far. The default is that the list in the announce is
  encrypted to the admitted devices with a key the join hands them (the
  Reticulum token format, 48 bytes of overhead and padding), and the
  list that does not fit one frame then is cut to the global and pinholed
  endpoints, since the LAN address is learned on the LAN anyway. Open
  question 1 below.
- **Malformed or oversized lists** (from a buggy or hostile mailbox
  identity's announce, or a corrupted answer) are ignored whole, logged
  once, and the device keeps running on what it had.

### 8. I2P or Tor hidden service (Advanced, unbuilt)

A stable address derived from a key, NAT irrelevant, and the network
itself is the meeting point: no list of ours is needed. Reticulum has an
I2P interface (`I2PInterface`, a discoverable type in
`reference/Reticulum/RNS/Discovery.py:37`). The cost is latency in
seconds and a phone that holds tunnels open. It needs the I2P router on
both ends (the mailbox host's and the phone's, an embedded router on
Android), an `[[I2P]]` interface in the mailbox's daemon, and the
destination in the endpoint list. An option under Advanced for users who
put privacy above speed, never the main path. Left unbuilt.

### D: a fixed address or a hand-made pinhole

For users who have one: the mailbox's own TCP server is reachable from
outside, and the phone connects to it directly. It lives under
"Advanced", never on the main path, and needs nothing new: the join
code's address already names it (`lqxh1...@host:port`,
[the home node](loquax-home-node.md), "Admission and revocation"). Rung 4
makes the common case automatic.

## The race

All candidates start at once, as Happy Eyeballs does (RFC 8305), and
the device keeps the most direct way that answers.

- **What runs.** On every connect, reconnect and network change, the
  device's node brings up every candidate interface the device knows for
  this network: the remembered LAN address (2b), the global IPv6 address
  (3), the pinholed address (4), the rendezvous (B) and its 443 entry
  (6). AutoInterface (2a) and BLE (1) are up anyway when their medium is
  on. Then the client asks for the path to `loquax.home`, as rule 2
  demands.
- **What wins.** A way is up when a link to `loquax.home` came up over
  it. The race waits a short grace (250 ms after the first link, as
  Happy Eyeballs waits for the preferred family) for a more direct rung
  to answer, then keeps the most direct one that did, and takes every
  less direct interface down: the rendezvous connection goes as soon as
  something more direct stands. Its paths go with it, so the next link
  takes the direct path. The ladder's order is the tie-break, never a
  sequence.
- **A desktop client on a shared instance** owns no interfaces; lnsd's
  path table decides. The race there only reads which interface the
  path stands on (the path table's interface, over RPC) and names the
  way in the header. Its own steering applies where Loquax owns the node:
  the phone's leaf, the phone's mailbox, a desktop client without a
  shared instance.
- **Losing the kept way.** When the kept way's interface goes down (BLE
  out of range, WiFi gone, the IPv6 address withdrawn), the race starts
  again at once, every candidate in parallel, the rendezvous among them.
  The outbox's retries carry a send that was cut off; a message is never
  dropped by a change of way, and never sent twice: a send repeated
  after a cut is matched to the first by its client id
  (`outbox_by_client`, `loquax/src/home/server.rs:249`).
- **The network change** (1298's `Network` word, below) restarts the
  race; the settle of 1298 folds a burst of callbacks into one race.
- **The re-race.** A device kept on an indirect way (relayed or mesh)
  tries the direct candidates again every 920 s, the fourth link
  keepalive of the device link (230 s, [phone power](loquax-phone-power.md),
  "The device link's keepalives"): it rides on a wake the modem already
  has, so it costs a few packets inside that wake and no wake of its
  own. Tier P's `radio_on_frac` measures it against B without it; it
  may not move the number by more than the noise of the mode's other
  runs, and the mechanism if it does.
- **Backoff.** A way that failed on this network is not tried again at
  once: one minute, doubling to an hour, per pair of network identity
  (2b) and way, kept across restarts. A network change switches to the
  other network's table; it does not clear it. A direct way that came up
  clears its own entry.
- **The header word.** One word, never a setting on the main path:
  "local" (0), "Bluetooth" (1), "home WiFi" (2), "direct" (3, 4, 5, D),
  "relayed" (B, 6, 8), "mesh" (A, 9). The words go to all four targets'
  catalogues, in English on the wire nowhere: the header is local.
- **The line.** Every race logs `REACH race network=<id> candidates=<ways>`,
  each way's end as `REACH way=<w> result=up|failed|timeout|backoff ms=<n>`,
  and the decision as `REACH kept=<w> word=<word> dropped=<ways>`. The
  cells below read these lines; `MAILBOX_PATH via=<w> hops=<n>
  rendezvous_dials=<n>` is the per-connect summary slice 1 adds first.

## The rendezvous without a list

The shipped rendezvous list is a bootstrap, not an architecture (Lew,
2026-11-04). It shrinks to a first-contact reserve:

- **The join code carries the mailbox's meeting points.** The code
  already carries the mailbox's rendezvous list behind the `@` (rule 5).
  It carries instead the transport nodes the mailbox is connected to at
  the moment it is made, its own server first, so the shipped list is
  used only when all of them are gone. The mailbox refreshes the device's
  copy over the link (the endpoint list, 7), so a device that joined a
  year ago still knows where its mailbox meets.
- **The candidates come from Reticulum's interface discovery,** not from
  a Loquax list (Lew, 2026-11-04). Transport nodes announce their
  reachable interfaces under `rnstransport.discovery.interface`
  (`get_interface_announce_data`, `reference/Reticulum/RNS/Discovery.py:96-186`),
  every node keeps the registry, and ours is byte compatible with
  Python's (`list_discovered_interfaces`,
  `leviculum-std/src/discovery.rs:461`). Autoconnect with a ceiling is
  the zero-config design ([zero config](zero-config.md), "The server
  list"), and it takes only transport nodes as Python does
  (`leviculum-std/src/autoconnect.rs:379`). Loquax reads that registry
  for the phone and the mailbox; the shipped list stays what the node
  already makes it, `bootstrap_only`. No pool of our own.
- **The one design question: the access point port.** A discovery
  announce names host, port and the transport flag, not whether the node
  offers an access point port for clients. The decision is a marker: the
  rendezvous's discovery announce for its peer port carries one more key
  in its msgpack map, the access point port. Python's receiver reads the
  keys it knows and builds its record from them
  (`received_announce`, `reference/Reticulum/RNS/Discovery.py:214`; the
  record at `reference/Reticulum/RNS/Discovery.py:263`), so an unknown
  key is dropped from the record and changes nothing it does; the
  announce's signature covers its app data, so a relay can neither strip
  the key nor forge one. The alternative, the phone on the peer port,
  loses the access point mode's announce shielding, and every announce of the mesh then
  wakes the phone's modem. Slice 4 measures that cost (phone wake-ups
  per hour on the peer port of a busy node, tier P's proxies against
  `leviculum.network`'s 4242) before the alternative may be chosen
  anywhere, and checks the marker's claim against a Python receiver and
  our own (`list_discovered_interfaces` must keep the key or drop it,
  never refuse the record).
- **Friends' homes as meeting points, opt-in.** A mailbox on a public
  host offers its access point port to the homes it trusts: the members
  of its groups and the contacts marked as home-sharing. Their devices
  learn it as a rendezvous candidate through the endpoint list. The cost
  to the offering home: a relayed link per device that uses it, its
  bandwidth and one TCP connection each, capped as order 1190 caps any
  source. What that home sees: what any rendezvous sees (the destination
  hashes, sizes and timing, the IP addresses), now of a friend's devices.
  Off by default on both sides; the offer and the use are each one
  switch.
- **The mesh as discovery (A)** is the rendezvous-free path: a device
  that shares any mesh with the mailbox needs no meeting point at all.
- **I2P and Tor (8)**: there the network is the meeting point, and no
  list of ours is needed.

## Rules for the implementation (order 1196's acceptance)

1. **No shared node is assumed.** Nothing in the client may expect its
   entry node to know the mailbox already, or to be the mailbox's node.
2. **An explicit path request on every connect and reconnect** for the
   `loquax.home` destination; the client never waits for an announce,
   which does not reach it behind an access point anyway.
3. **Any hop count is accepted**, and request timeouts scale with the
   path, not with a TCP round trip.
4. **A rendezvous list, ordered, with failover.** The first entry is
   tried first; a dead entry moves both ends to the next.
5. **The join code carries the mailbox's rendezvous list.** The code
   (`lqxh1…@host:port`, order 1121) grows from one address to the list,
   so a phone that joins from outside gets the right entry node. A code
   with one address reads as before. Order 1402 turns the list into the
   mailbox's meeting points ("The rendezvous without a list").
6. **The access point port is the phone's default; the ordinary port is
   the mailbox's.**
7. **A mailbox on a public server is a rendezvous for its own phones**,
   with zero extra configuration: its own TCP server runs in access
   point mode, and its join code names it first.

## Compatibility

Everything here is reference behaviour: the access point mode
(`MODE_ACCESS_POINT`, `reference/Reticulum/RNS/Interfaces/Interface.py:47`,
parsed from the config at `reference/Reticulum/RNS/Reticulum.py:722`),
recursive path discovery from an access point interface
(`reference/Reticulum/RNS/Transport.py:3015`), path expiry of one day
for paths learned on it (`AP_PATH_TIME`,
`reference/Reticulum/RNS/Transport.py:72`), and links. A Python `rnsd`
works as a rendezvous, unchanged. A Python client that has no mailbox
still reaches a Loquax mailbox's LXMF address the normal way: the
mailbox announces its delivery destination and accepts DIRECT and
propagated mail like any LXMF node ([the home node](loquax-home-node.md),
"Compatibility with Python peers"). No wire change.

## What is measured, and the three cells

Before rung 5 (hole punching, the old C): the share of real home
routers that pass a punched UDP flow and the share with inbound IPv6;
slice 9 below says how it is measured and logged.

Order 1196 delivers three conformance cells. Each runs a phone client
behind an access point port and a mailbox, and passes only when a
message stored on the mailbox reaches the client over the home link; the
negative control in each removes the path (stops the mailbox) and must
turn red.

1. **Same node:** phone and mailbox on one rendezvous.
2. **Two nodes, a TCP link between them:** phone on node A, mailbox on
   node B.
3. **Two nodes, the emulated LoRa link between them:** a LoRa-only path
   carries the home link.

Cells 2 and 3 each run with the mailbox's own daemon as node B, and
with a separate relay as node B and the mailbox one hop behind it. The
second shape is the catch under A; it is red with the announce of
`loquax.home` held back and green with it (below, "Decided").

## Decided (the reviewer, 2026-10-31, order 1196)

- **`loquax.home` is announced** on the delivery destination's cadence,
  and again by a mailbox each time its rendezvous connection comes up.
  Every relay then holds the path, and A works beyond the first relay.
  The second is the node's own re-announce on a new or reconnected
  interface, which carries the delivery destination too, once each: a
  third announce at connect is what a stock Python entry holds as a burst
  of a new interface, and it held the delivery announce for half a minute
  when that came third (order 1274,
  `reference/Reticulum/RNS/Transport.py:1705`).
  The client still asks for the path on every connect. The residual case
  is a mailbox behind a third party's plain port whose announce has not
  reached the phone's entry node yet: the client asks again with a
  doubling wait (`loquax/src/home/client.rs`, `PATH_RETRY`) and logs each
  request.
- **The access point port on `leviculum.network` is 4343**; 4242 stays
  the peer port. The shipped list is port 4343 of `leviculum.network`,
  then its port 4242 as the last fallback for a server from before the
  access point port (`loquax/src/home/rendezvous.rs`, `DEFAULT`).
- **One rendezvous at a time**, with failover and return
  (`loquax/src/home/rendezvous.rs`, `Rotation`).
- **C** waits for the measurement; **D** stays under Advanced
  (`home_address` in Loquax's config).

## Decided (the reviewer, 2026-11-04, order 1402)

The reviewer's defaults for the ladder, Lew's word on 2026-11-04 as the
frame:

- **The order of building:** the shared instance verified first (0),
  then BLE (1) once the phone has it, then the remembered LAN address
  (2b), then IPv6 with the relayed link as signalling (3), then the
  pinhole automation (4). Hole punching (5) only after the measurement.
  The 443 relay (6) runs in parallel as operations, its client side as a
  slice. I2P (8) stays under Advanced, unbuilt.
- **The remembered LAN address before mDNS:** it costs nothing and needs
  no multicast at all; mDNS waits for numbers.
- **The endpoint list over the link first, in the announce second,** and
  in the announce encrypted to the admitted devices unless Lew decides
  otherwise (open question 1).
- **The race, not a sequence:** all candidates at once, the rung order as
  tie-break, the rendezvous dropped as soon as something more direct
  stands, the re-race on the fourth device keepalive (920 s), a backoff
  per network and way.
- **The rendezvous from discovery:** the join code's meeting points
  first, then the discovered transport nodes that carry the access point
  marker, then the shipped list as `bootstrap_only`. The marker is a key
  in the discovery announce, not a second announced interface.
- **Friends' homes** as meeting points are opt-in on both sides.

## Not chosen

- **A Mainline-DHT lookup** of the mailbox's address (the reviewer's
  first idea): Reticulum's interface discovery already makes every
  announcing public transport node a meeting point without anyone's
  consent or a list of ours, and a foreign DHT would add a dependency
  for nothing.
- **The rendezvous reporting the mailbox's observed address** for rung 3:
  it would need code on the rendezvous, which a Python `rnsd` does not
  have, and the dial it observed may have left over IPv4.
- **The phone on a rendezvous's peer port** by default: unless slice 4's
  measurement says the announce traffic there costs nothing, the access
  point port stays the phone's.

## The slices

Each is one coder pass, in dependency order. Each lands only with its
periculum cells green, the happy paths and the unhappy paths both (Lew,
2026-11-04: everything built gets periculum cells for the happy and the
unhappy paths; a slice without an unhappy cell is incomplete). Every cell
names its measure line; a cell asserts on the line, never on a log's
tail. Cells that need a link filter, a NAT or a router are written
first, red, against slice 0's tools.

**0. periculum: the network shapes** (periculum, no Leviculum code).
Link filters on a node's namespace: drop IPv6 and IPv4 multicast, drop
inbound IPv6, egress only to 443, an IPv4 NAT that is endpoint
independent and one that is symmetric (nftables masquerade with and
without fixed ports), a dual-stack pair with routed global IPv6
(documentation prefix), and a router node running `miniupnpd` with PCP
and UPnP-IGD on or off. Acceptance: one positive and one negative control
per shape (`shape_multicast_blocked`: AutoInterface finds the neighbour
with the filter off, not with it on; `shape_symmetric_nat`: two
destinations see two source ports; `shape_upnp_router`: `upnpc -a`
maps with the service on, fails with it off; `shape_egress_443`: a TCP
connect to 4242 fails, to 443 succeeds). Each prints `SHAPE name=
control=pos|neg result=`.

**1. Rung 0: the shared instance, verified, and the measure line.**
Loquax logs `MAILBOX_PATH via=<way> hops=<n> rendezvous_dials=<n>` once
per connect of a client to its mailbox, `via=` from the path's interface
(the local interface reads `local`). The desktop header word reads from
the same.
- Happy: `reach_local_same_lnsd`: one lnsd, `loquax mailbox --daemon`
  and a terminal Loquax client on it; asserts `via=local hops=0
  rendezvous_dials=0`, and that the client process opened no TCP
  connection (its `REACH` lines and the host's `ss` for its pid).
  `reach_local_phone_own_mailbox`: the facade test shape, the phone's
  leaf on its own mailbox's loopback port, `via=local`.
- Control: `reach_local_control_second_node`: the same client, the
  mailbox on a second node behind a periculum rendezvous; `via=rendezvous
  hops=2` (red if the cell's measure cannot tell the two apart).
- Unhappy: `reach_local_mailbox_stopped`: the mailbox stops; the client
  says unreachable within its path timeout, `rendezvous_dials=0`, and no
  connection leaves the host. `reach_local_laptop_away`: a desktop client
  on an lnsd without any path to the mailbox; says unreachable, and the
  line records the gap (red once the race of slice 3 serves desktop
  clients without a shared instance, then turned around).

**2. The endpoint list over the link.** The `/endpoints` request on
`loquax.home`, membership checked; the mailbox's LAN addresses, global
IPv6 (stable, not temporary), the phones' port, the counter; the device
stores it per network identity (2b's key, measured on a device here:
what Android's `LinkProperties` and Linux hand over without a prompt,
logged as `NETWORK_ID source= fields=`).
- Happy: `endpoints_admitted_device_gets_list`: `ENDPOINTS kinds=lan4,lan6,global6
  port= counter=` on the device, equal to the mailbox's interfaces.
- Unhappy: `endpoints_not_admitted_refused` (a revoked device gets the
  refusal, `ENDPOINTS refused`); `endpoints_malformed_answer_ignored`
  (a test mailbox answers garbage, an oversized list and a wrong type;
  the device logs `ENDPOINTS ignored reason=` and keeps its old list and
  its link); `endpoints_old_mailbox` (a mailbox from before the slice
  answers "unknown path"; the device goes on with B, no retry loop).

**3. The race, and rung 2b.** The race controller in `loquax/src/home/`
beside the rendezvous rotation, the `REACH` lines, the backoff table, the
header word on all four targets, the re-race on the fourth keepalive.
First candidates: B and the remembered LAN address.
- Happy: `reach_lan_multicast_blocked`: device and mailbox on one LAN
  with multicast dropped (slice 0), a rendezvous beside; the first race
  goes `kept=rendezvous`, the list arrives, the reconnect races again
  and keeps `lan`, `dropped=rendezvous`, word `home WiFi`.
  `reach_race_two_ways_direct_wins`: both up from the start, the LAN
  link answers within the grace; `kept=lan`, the rendezvous connection
  closed (its TCP count goes to zero on the rendezvous node).
- Unhappy: `reach_lan_gone_midlink`: the LAN link is cut while a message
  is being sent; the race restarts, `kept=rendezvous`, the message
  arrives once (`outbox_by_client` matched the repeat).
  `reach_lan_address_stale`: the remembered address now answers with
  another host; `result=failed`, backoff entered, no second dial within
  the minute, B kept. `reach_lan_other_network`: the device on a network
  with another identity; the remembered address is not dialled at all.
  `reach_rendezvous_first_entry_down`: B's first entry refuses; the next
  entry is taken within the rotation's timing (1196's rotation held
  under the race).

**4. The rendezvous from discovery and the join code's meeting points.**
The access point marker in our discovery announce; Loquax reads the
registry; the join code carries the mailbox's connected transport nodes.
- Measurement first: `peer_port_wakeups`: tier P's proxies for a phone on
  a busy node's peer port against its access point port, `segs_in_h`
  and `radio_on_frac` per hour; it informs, it does not gate.
- Happy: `rendezvous_from_discovery`: the shipped list pointed at a dead
  address, a periculum transport node announcing its interfaces with the
  marker; the device takes its access point port, `RENDEZVOUS chosen=
  source=discovery`. `join_code_meeting_points_first`: the code names
  node X, the shipped list Y; `source=code` X.
- Unhappy: `rendezvous_all_code_points_gone`: every point of the code is
  down; the discovered node, then the shipped list, `source=shipped`.
  `discovery_marker_python_receiver`: a Python `rnsd` receives our
  announce with the marker; `rnstatus -d` lists the interface normally,
  with the peer port, and its log holds no error.
  `discovery_unmarked_or_not_transport_skipped`: a discovered node
  without the marker, and one with `transport` false, are not used as
  rendezvous.

**5. Rung 1, Bluetooth** (after the phone node's BLE, step 3 there; on
a desktop client without a shared instance before that).
- Happy: `reach_ble_direct_btvirt`: device and mailbox node on btvirt
  BLE and both on a rendezvous; `kept=ble`, word `Bluetooth`,
  `dropped=rendezvous`.
- Unhappy: `reach_ble_out_of_range_midlink`: the btvirt link removed
  during a send; the interface reports the peer lost, the race keeps
  the rendezvous, the message arrives once. `reach_ble_peer_is_not_the_mailbox`: a BLE peer
  that is another node; the path to `loquax.home` does not stand on BLE,
  no `kept=ble`.

**6. Rung 3, IPv6 direct.**
- Happy: `reach_ipv6_dual_stack`: a dual-stack pair, the mailbox's
  inbound IPv6 open, IPv4 behind NAT, a rendezvous; `kept=global6`, word
  `direct`.
- Unhappy: `reach_ipv6_withdrawn_midlink`: the mailbox's global address
  removed during a send; fall to the rendezvous, the message once, the
  next list carries no `global6`. `reach_ipv6_firewalled`: inbound IPv6
  dropped; `result=timeout`, backoff, `kept=rendezvous`, word
  `relayed`.

**7. Rung 4, the router pinhole.**
- Happy: `pinhole_upnp_ipv4` and `pinhole_pcp_ipv6`: the router node
  grants the mapping; `PINHOLE proto= result=granted lifetime=`, the
  list carries it, the device keeps `pinhole` from outside;
  `pinhole_renewed`: the router restarts and forgets, the mailbox maps
  again within half a lifetime.
- Unhappy: `pinhole_router_refuses`: the service off, then a deny rule;
  `result=refused`, exactly one request per hour, the device on the
  rendezvous. `pinhole_router_lies`: the router grants a mapping that
  does not forward; the device's dial fails, backoff, B kept.

**8. The endpoint list in the announce.** Size measured first (the
announce's bytes with and without ratchet, plaintext against encrypted),
`ANNOUNCE_SIZE dest=home bytes= frames=`; then the chosen form.
- Happy: `endpoints_announce_teaches_direct`: a device that never had a
  link learns the LAN address from a heard announce and races it.
  `endpoints_announce_python_listener`: a Python program with an announce
  handler for every aspect receives `loquax.home`'s announce, logs no
  error, and a stock Python transport node relays it.
- Unhappy: `endpoints_announce_oversized`, `endpoints_announce_malformed`:
  ignored whole, `ENDPOINTS ignored reason=`, the node keeps running and
  keeps its path. `endpoints_stock_python_peer`: a stock Python LXMF peer
  that neither announces nor parses our fields messages the mailbox's
  address as before, both ways. `endpoints_announce_not_for_us`: with
  encryption, a non-admitted listener cannot read the list
  (`ENDPOINTS undecryptable`).

**9. The measurement for hole punching**, then perhaps rung 5. Each
mailbox and device whose user opted in records once per network:
`REACH_SAMPLE network=<hash> side=mailbox|device nat=independent|dependent|none
ipv6_inbound=ok|blocked|none pinhole=pcp|natpmp|upnp|refused|none
punch=ok|failed|untried`, in its own event log; the NAT type from a UDP
probe to two ports of `leviculum.network` (operations). With the opt-in,
one LXMF message a week to a collection address of ours carries the
samples; without it nothing leaves. Cells: `reach_sample_independent_nat`,
`reach_sample_symmetric_nat` (both read their shape right),
`reach_sample_opt_out_sends_nothing` (unhappy: no message, no probe).
Rung 5 itself, if the numbers carry it: `punch_independent_nat` (happy,
`kept=punch`), `punch_symmetric_nat_falls_back` (unhappy, `result=failed`,
B kept, no second attempt within the backoff).

**10. Rung 6, the client side of 443 with TLS.**
- Happy: `relay_443_only_egress`: egress filtered to 443; B fails,
  `kept=rendezvous443`, word `relayed`.
- Unhappy: `relay_443_bad_certificate`: a self-signed listener on 443;
  `RENDEZVOUS_TLS refused reason=certificate`, no data sent.
  `relay_443_and_4343_both_up`: on an open network, the 443 connection is
  dropped once 4343 stands.

## A second device for a phone's mailbox (1264)

A phone that holds the mailbox ("On this phone") links another device
from its profile: **Link a device** shows a join code and its QR, and
the new device scans it at its first start under **Join an existing
mailbox** (walk finding 7, `docs/drafts/ninety-percent-walk-2026-11.md`).

- **The code is the computer's.** The phone calls the function
  `loquax mailbox admit` calls (`loquax/src/home/admit.rs`, `open`): a
  single-use token in the mailbox's store, the `lqxh1` body, the
  rendezvous list behind the `@`, good for `home_token_hours` (a day by
  default). The token is issued without a name, so the device is listed
  under the name its join brings (`Store::redeem_token_as`); the first
  phone polls for it and says "Device linked: <name>".
- **The list is what the phone's mailbox dials**, as another device
  dials it (`loquax-android/src/mailbox.rs`, `code_list`): with the
  shipped entry server (port 4242 of leviculum.network, the peer port) that is
  the shipped list, port 4343 first; with a servers file, its entries as
  they stand.
- **What works today: B, the rendezvous.** The phone's mailbox is behind
  NAT and the carrier's CGNAT; it dials the rendezvous as a peer, the
  new device dials it on the access point port, and the join, the pull
  of the history and the wakes run over that relay, end to end
  encrypted. `a_second_phone_links_to_the_mailbox_on_the_first_and_gets_its_history`
  in `loquax-android/tests/facade.rs` runs it with a Rust transport node
  as the rendezvous; the device walk's `link_device` runs it with the
  walk's lnsd, the host's `loquax mailbox join` as the second device.
- **What does not work yet: E, the same WiFi.** The phone has no
  AutoInterface until the phone full node
  ([the phone as a full node](loquax-phone-node.md)) lands; two devices on one
  WiFi still meet at the rendezvous. A device that has no internet and
  no rendezvous cannot link to a phone's mailbox.

## Moving to a new phone (1281)

The old phone's profile has **Move to a new phone** where the mailbox is
on that phone; the new phone's first start has **Move from my old phone**
(walk finding 24, `docs/drafts/ninety-percent-walk-2026-11.md`). The
onboarding draft's section 4 is the model: one identity never answers on
two devices at once.

- **The code is a link code with a move opened beside it.** The old phone
  makes the same `lqxh1` code as "Link a device" (`mailbox::move_code`)
  and writes the move file under its mailbox (`loquax/src/home/handover.rs`,
  `open`): the token and its end. Only the mailbox's own disk holds that
  authority, so an admitted device alone never gets the identity.
- **What travels:** the new phone joins with the token, then pages
  `/move` until no more follow. Each page carries the identity's private
  key, the delivery destination's own ratchets (the signed file the node
  keeps), the node's known identities as it last wrote them (the
  contacts' keys), the mailbox's name, and the archive after the cursor
  with each entry's kind, id and time: the history, the read and delete
  marks, the petnames, the blocks and pins, the names heard. The new
  phone writes them as its own mailbox (`mailbox::move_in`), marks the
  identity as moved in (so it announces every 30 minutes for a day), and
  only then asks `/moved` with the newest sequence it holds.
- **The hand-over:** the old mailbox answers `/moved` with "behind" while
  its archive grew past that cursor (the new phone pages the rest and asks
  again); otherwise it writes the gone file, answers ok, and ends after two
  seconds. A mailbox with the gone file does not start
  (`home::daemon::run`), so the old phone sends nothing, and its first
  screen says "This phone has moved" with **Use this phone again**, which
  takes it back behind a warning that names the risk. The new phone
  becomes the mailbox only on that ok; any failure before it removes what
  it wrote, and the old phone still has everything.
- **What does not travel:** the old mailbox's outbox (sends still being
  retried stop with the old phone), the Android address book links (they
  belong to that phone's contacts), and contact keys the node learned
  since it last wrote its known identities, which the peer's next announce
  brings. If the ok is lost on the way, both phones are stopped: the old
  one took the word, the new one says the move did not work; "Use this
  phone again" on the old one is the way back.
- **The paths:** as for linking a device, **B, the rendezvous**, works
  today: both phones dial the code's list and the move runs over that relay,
  end to end encrypted. **E, the same WiFi**, does not until the phone
  full node brings the AutoInterface. `a_new_phone_takes_the_mailbox_over_and_the_old_one_stops`
  in `loquax-android/tests/facade.rs` runs it with a Rust transport node as
  the rendezvous and the ugly store in the archive; the device walk's
  `move_phone` runs it with the walk's lnsd and the host's `walk_move` as
  the new phone.
- **The restore's guard:** a restore from a backup ("I already have a
  Loquax address") first looks whether the backup's mailbox answers right
  now: a link to its home destination over the phone's code list, within
  15 s (`mailbox::answers_elsewhere`). If one comes up, the first start
  warns ("This address is in use on another device right now…") and goes
  on only on **Restore anyway**. A path the rendezvous still remembers is
  no answer; only a link that comes up is.
  `a_restore_of_an_address_live_elsewhere_warns_first` holds both ways.

## A network change (1298)

The phone-only user has the mailbox on the phone and no server. Their
measure is to do better than Columba. Columba's Python background service
chokes when the network changes, and Lew restarts it by hand. Here
nothing has to be touched: no restart, no user action.

**Before 1298:**
- After a change, the connection to the rendezvous stayed on a socket
  whose source address the phone no longer had. It died only when the
  TCP keepalive of order 1176 gave up on it, 24 s and more.
- A rotation that had failed meanwhile, while there was no network, then
  waited out its doubled backoff (up to 5 min) after the network was back.

**The event.** The platform says one thing: `UiEvent::Network { kind,
changed }`.
- **Where it comes from:** `NetWatch` in the Android app. It uses the
  default network callback, `onAvailable` and `onLost` in
  `loquax-android/android/src/network/leviculum/loquax/NetWatch.java`.
- **`kind`:** `none`, `wifi`, `mobile` or `other`
  (`loquax::home::NetworkKind`).
- **`changed`:** true when the default network is another one than
  before.
- **After Doze:** the end of Doze (`ACTION_DEVICE_IDLE_MODE_CHANGED`) says
  the same network again, with `changed` off. So the first wake re-runs
  the same evaluation.
- **The path to the rotations:**
  1. The model answers with `UiEffect::Network`.
  2. The shell says it to the session's one
     `loquax::home::rendezvous::Network` (`Network::say`).
  3. Every rotation started with that handle hears it: the phone's leaf,
     or the phone's own mailbox (`DaemonOptions::network`). The leaf of a
     phone mailbox dials the loopback address, which no network change
     touches, so it gets a handle of its own that nobody speaks to.

**The evaluation.** It runs in the rotation's loop, `run` in
`loquax/src/home/rendezvous.rs`, logged as `HOME_NETWORK kind= changed=
action= entry=`.

First, the settle folds a burst of callbacks into one evaluation
(`Settle`): the evaluation runs once the changes have been quiet for
1.5 s, and at the latest 6 s after the first, so a flapping network is
still evaluated. A phone leaving a WLAN says lost, available and
capabilities within a second.

Then `evaluate` decides what to do:

| the platform says | the connection | the rotation |
|---|---|---|
| no network | any | drops it and dials nothing until a network comes (`pause`); a dial into no network would only grow the backoff |
| another network | any | drops it and dials the first entry at once, the backoff started over (`redial`) |
| the same network | down | dials the first entry at once, cutting a running backoff short (`redial`) |
| the same network | up | keeps it and asks the home path again (`ask`) |

- **Re-announce:** once per destination. The node's own up handling
  re-announces every destination on the new interface, as on any other
  up (`handle_interface_up`).
- **Path request:** the phone side asks for the home path on that up
  (rule 2).
- **The backoff wait** is part of the loop, so a change always cuts it
  short.
- **When the AutoInterface comes:** once the phone runs a full node, its
  AutoInterface joins the same path. Its re-evaluation is a reaction to
  the same `Network` word.
- **The line:** the session tells the app as `Event::Network`, which says
  `reconnecting` while a rotation dropped its connection for the change
  and is not up again, then `back`. The status line says "Reconnecting…"
  and then is gone.
- **The keepalive levers of 1176 stay:** they find a dead connection that
  no callback announced.

**What holds it:**
- `network_changes_redial_the_first_entry_once_each` and
  `the_wake_after_doze_cuts_the_backoff_short` in
  `loquax-android/tests/home_reach.rs`. They drive the five changes
  against two fake rendezvous, bare TCP listeners that count connections,
  and assert:
  - the first entry is dialled first;
  - a burst is one dial (red with the settle at 0);
  - no network dials nothing;
  - the same network with the connection up is kept;
  - a running backoff is cut short.
- `a_network_change_reconnects_the_phone_without_a_restart` in
  `loquax-android/tests/facade.rs` goes from Java's request to the line
  and on to a send after it.
- On a device:
  - the walk's steps `network_wifi_toggle`, `network_mobile_switch` and
    `flight_mode` ([the walk](loquax-device-walk.md));
  - `loquax-android/scripts/network-baseline.py`, which measures the five
    changes for any app that holds an address on the phone, Columba's
    service or Loquax's phone mailbox. The measure is the time from the
    network's return to the first proven delivery, plus what the app
    showed meanwhile.

## What this page does not decide

- **Whether a message left at the propagation node reached its
  recipient.** That is a receipt the recipient sends:
  [receipts for propagated post](loquax-propagation-receipts.md).
- **Whether rung 5 is ever built**, after slice 9's measurement.

Open for Lew, each with the default the slices take until Lew says
otherwise:

1. **The endpoint list in the announce: plaintext, encrypted, or not at
   all.** Plaintext tells everyone who hears the announce the
   household's global IPv6 address and its open port. Default:
   encrypted to the admitted devices with a key the join hands them, and
   cut to what fits one LoRa frame (slice 8 measures).
2. **Rendezvous from strangers.** A discovered transport node with the
   access point marker is run by someone the user has no relation to,
   who then sees the device's IP address and timing as
   `leviculum.network` does today. Default: allowed, after the join
   code's points and before the shipped list, and the profile names the
   node in use.
3. **The re-race's budget.** Default: on the fourth device keepalive
   (920 s) and only while on an indirect way; if tier P shows a cost,
   the next step is the eighth (about 30 min), not a separate wake.
