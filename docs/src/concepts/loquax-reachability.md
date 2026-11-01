# Loquax: reaching the mailbox from anywhere

Status: concept, decided by Lew on 2026-10-31 (order 1195, Refs #232,
#471). The implementation is order 1196; the rules below are its
acceptance, and "Decided" near the end records the open points it closed. The mailbox itself is [the home node](loquax-home-node.md).

## The problem

A Loquax Mailbox runs at home, on a Raspberry Pi or a PC behind the
household's internet router. The phone that reads from it is somewhere
else: on mobile data, in an office WiFi, in a hotel. Both sit behind a
router that translates addresses (NAT). Such a router lets its devices
call out, and refuses every call that comes in unasked. So the phone
cannot call the mailbox, and the mailbox cannot call the phone. The usual
cures are a fixed public address, a DynDNS name with a forwarded port, or
an overlay network such as a VPN. Most users have none of them, and
Loquax must not ask for any.

Reticulum already holds the answer: both ends call out to a third
machine that both can reach, and that machine carries the traffic
between them, still end to end encrypted. Loquax picks one of three
ways, in order, all without a question to the user: a shared public
node (the rendezvous), any mesh path between the phone's node and the
mailbox's node, and the home WiFi when the phone is at home. Two more
come later or stay for experts. The user never configures any of them:
the join code carries what the phone needs.

## The ways, in order of preference

### B: the rendezvous (default)

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
`leviculum-core/src/transport.rs:12492`). lnsd widens the gate only for
interfaces that report live peers, and today only BLE does
(`leviculum-std/src/interfaces/ble/mod.rs:1811`). Without an announce of
`loquax.home` no relay holds its path either. So A worked only when the
mailbox's own daemon was node A's direct neighbour, and failed when a
third relay stood between them. 1196 closes it by announcing
`loquax.home` (below, "Decided"); its cells show both, red with the
announce held back and green with it.

### E: at home, in the same WiFi

The phone's AutoInterface finds the mailbox's daemon on the local
network ([zero config](zero-config.md), #471) and the path request is
answered over the LAN, one relay away. Nobody outside sees anything.
Nothing fails that B would not catch: when the WiFi is gone, the phone
is back on B. Power: the LAN costs less than the cellular modem.

### C: a direct channel (later)

The rendezvous could serve as the signalling channel for UDP hole
punching or a direct IPv6 connection, with B as the fallback. That
saves the relay hop and the rendezvous's bandwidth, not the phone's
battery, which a held connection costs in either case. Before it is
built, measure the share of home routers that let a punched UDP flow
through (and how many households have working inbound IPv6), from a
sample of real Loquax installs that opt in. Below a useful share, C is
not worth its code.

### D: a fixed address or a router pinhole

For users who have one: the mailbox's own TCP server is reachable from
outside, and the phone connects to it directly. It lives under
"Advanced", never on the main path, and needs nothing new: the join
code's address already names it (`lqxh1...@host:port`,
[the home node](loquax-home-node.md), "Admission and revocation").

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
   with one address reads as before.
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

Before C: the share of real home routers that pass a punched UDP flow
and the share with inbound IPv6, as above.

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
second shape is the catch under A; it stays red until the open decision
below is made and built.

## Decided (the reviewer, 2026-10-31, order 1196)

- **`loquax.home` is announced** on the delivery destination's cadence,
  and again by a mailbox each time its rendezvous connection comes up.
  Every relay then holds the path, and A works beyond the first relay.
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

## What this page does not decide

- **The rendezvous list beyond `leviculum.network`**: who else is in the
  shipped list, and whether a mailbox's list is the shipped list, its
  own server, or both. Today it is both: `loquax mailbox admit --server`
  puts the mailbox's own phone port first, the shipped list behind it.
- **Whether C is ever built**, after the measurement.
