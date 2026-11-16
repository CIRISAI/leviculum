# Map data between neighbours, against the reference (study, order 1688)

Refs #232. Study only, no product code, no periculum cell. It answers the
four questions the expedition-map concept (draft
`expedition-map-concept-2026-11.md`, section 6, "Sharing map data over
Reticulum") left open, and ends with an order text for the
implementation.

Sources. Python-RNS 1.3.5 is the pinned `reference/Reticulum` tree; every
bare `Transport.py:N`, `Link.py:N`, `Resource.py:N`, `Destination.py:N`
and `Interface.py:N` below is a line in that tree (`RNS/`, and
`RNS/Interfaces/` for `Interface.py`). Lines marked **1.5.2** are in the
1.5.2 checkout at `/home/lew/coding/Reticulum` (`ea98db4`); they do not
resolve in the pinned tree. Our core is cited by `path:line` at master
`c9f7f3ba6`. Airtime numbers come from a Python mirror of
`airtime_ms_with_preamble` (`leviculum-core/src/rnode.rs:1116`) and
`packet_airtime_ms` (`leviculum-core/src/rnode.rs:1668`) at the house
profile 869.463 MHz, SF8, BW 125 kHz, CR 4/5, derived preamble 18
symbols (`derive_preamble_symbols`, `leviculum-core/src/rnode.rs:964`),
one RNode header byte per frame. Nothing in this study was run on a
radio; every number is arithmetic over those functions, and every
behavioural claim is a reading of the code, not a measurement.

## Short answers

1. **Answer format.** A plain answer that carries the answerer's map
   destination hash, followed by an ordinary path request from the
   asker. The path response is the one reference mechanism that installs
   a one-hop path everywhere we need it (both stacks, shared instance on
   both sides), skips the announce rate limit and the ingress hold, and
   is never rebroadcast. An unsolicited announce or path response from
   the answerer does not work behind a shared instance (section 1.2).
2. **Keeping the question off LoRa.** Not possible in the reference
   without the app knowing the medium: a client's plain broadcast leaves
   its daemon on every interface, with no mode or bitrate check, in 1.3.5
   and 1.5.2 alike. Recommended: accept the question and its answers on
   every medium, bounded by rate, and enforce "nothing automatic over
   LoRa" at the first point where the core already knows the medium,
   the path entry's next-hop bitrate, before any link request. A
   daemon-side filter would be a deviation that fails rule 2 of the
   deviation rule for Python clients on lnsd (section 2).
3. **Rate limits.** Plain data is outside every ingress limiter, so the
   bound has to be the app's own: one question per coarse tile per
   10 minutes with backoff, one answer per nonce, at most three path
   requests per question. Random answer delay uniform over 0 to 2 s,
   with suppression after three heard answers; at SF8 that is about ten
   answer airtimes of spread, on top of the LoRa interface's own jitter
   and CSMA (section 3).
4. **Measurement ladder.** Link RTT comes free with link setup; a tiny
   request costs about 0.7 s airtime round trip on LoRa; after that the
   first coarse piece is the measurement, there is no separate probe.
   Today's 4 KiB probe is nine resource parts at MTU 500, about 13.7 s of
   airtime at SF8, the whole 10 % duty budget of 137 s of wall clock on
   our default sub-band; it is also compressible if its bytes are fixed,
   in which case it measures nothing (section 4).

## 1. Which answer lets a link come up without a prior announce

### 1.1 What a link needs in the reference

- A link request is sent to a `Destination` object built from the
  peer's public key. Over a known path it goes to the path's interface
  (`Transport.py:1121`); without one it is broadcast as HEADER_1 on every
  interface (`Transport.py:1180`).
- A HEADER_1 link request is never forwarded by a transport node: only
  a packet whose `transport_id` names the node is relayed
  (`Transport.py:1559`). So a path-less link request reaches exactly the
  one-hop neighbours, which is what we want, but on every medium.
- A shared-instance client cannot send a path-less link request at all
  in the reference: its daemon relays a client's non-plain packet only
  through the path, link or reverse tables (`Transport.py:1538`,
  `Transport.py:1559`, `Transport.py:2174`). Clients therefore request a
  path first; that is the normal client flow, not a workaround.
- Link MTU upgrade (fast neighbours over AutoInterface, TCP) needs the
  next-hop interface's hardware MTU, which only a path entry supplies
  (`Link.py:310`, `next_hop_interface_hw_mtu` at `Transport.py:2679`).
  Without a path the link stays at the 500-byte base MTU, which costs a
  fast neighbour most of its speed for large resources.

So whatever the answer carries, the asker wants a path entry for the
answerer's destination, with hops 1 and the right receiving interface.

### 1.2 What `Transport` accepts as a path source

Only announces install path entries (`Transport.py:2013`). Three kinds
of announce were weighed.

**(a) An ordinary announce from the answerer.** Installs the path but is
rebroadcast by every transport node (`Transport.py:1886`): the whole
mesh pays for one neighbour's answer. Rejected; avoiding exactly that
flood is why the question goes to a plain destination.

**(b) An unsolicited path response from the answerer**
(`announce(path_response=True, attached_interface=...)`,
`Destination.py:243`). Path responses skip the announce rate limit
(`Transport.py:1838`) and are never queued for rebroadcast
(`Transport.py:1886`), which is ideal. But the answerer is a Loquax
behind its own daemon, and a path response from a local client leaves
the daemon only when that daemon holds a pending path request for the
destination (`pending_local_path_requests`, `Transport.py:1910`; ours
mirrors it, `forwards_pending_response` at
`leviculum-core/src/transport.rs:8061`). An unsolicited one stops at the
answerer's own daemon. It would also be held by ingress control at the
asker's side when the destination is unknown and the interface is in an
announce burst (`Transport.py:1696`, ours at
`leviculum-core/src/transport.rs:7315`). Rejected.

**(c) A path response to the asker's own path request.** This is the
flow every client already uses. Walking it end to end:

1. The answerer's map destination is registered with its daemon without
   flooding: a shared-instance client announces every new SINGLE
   destination with `path_response=True` 250 ms after registration
   (`register_destination`, `Transport.py:2417`; same in **1.5.2**
   `Transport.py:2905`). The daemon installs the path entry
   (`Transport.py:2013`) and does not rebroadcast it
   (`Transport.py:1886`; ours leaves such a path response out of the
   announce table on purpose, see the #255 note at
   `leviculum-core/src/transport.rs:8066`).
2. The asker requests a path. From a client, its daemon forwards the
   request on all other interfaces (`Transport.py:3006`).
3. The answerer's daemon finds the destination on a local client, notes
   the requesting interface (`Transport.py:2925`) and forwards the
   request to its clients (`Transport.py:3043`); the client answers with
   a path response (`Transport.py:2940`); the daemon sends it once
   (`Transport.py:1910`). A transport-enabled daemon may instead answer
   from its announce cache on the requesting interface only
   (`Transport.py:2943`), after `PATH_REQUEST_GRACE` for non-local
   destinations, so direct peers answer first (`Transport.py:81`).
4. At the asker, the path response is exempt from the ingress hold
   because a path request is waiting (`Transport.py:1701`, ours
   `awaiting_pr` at `leviculum-core/src/transport.rs:7297`), exempt from
   the rate limit, not rebroadcast, and it installs the path with hops 1
   on the interface it arrived on.

Then `Identity.recall` has the key, the path has the interface and its
hardware MTU, and the link request goes to one interface. Everything
above is stock API on both sides, so a Python map server could
implement the same protocol.

### 1.3 Recommended answer format

The answer is a plain broadcast, not a reply addressed to the asker
(plain destinations carry no addressee): every one-hop neighbour of the
answerer hears it, and an asker matches it by nonce. Two plain
destinations, both IN/OUT PLAIN with no identity, so their hashes are
fixed constants:

| Packet | Destination | Payload | Size on wire |
| --- | --- | --- | --- |
| Question | `lev.maps.ask` | version (1), nonce (4), map-server key id (4, truncated hash of the pinned signing key), coarse tile z/x/y at zoom 6 (3) | 19 + 12 = 31 B |
| Answer | `lev.maps.offer` | version (1), nonce echo (4), map destination hash (16), coverage at that tile: max zoom (1), pieces held (2), flags (1: busy, low battery) | 19 + 25 = 44 to 46 B |

The 19 bytes are the HEADER_1 overhead: flags, hops, destination hash,
context. The map-server key id lets a neighbour answer only for data
signed by the server the asker pins (concept section 6, "Only signed
data is shared"). Zoom 6 is a tile of about 600 km at the equator,
coarse enough that the clear-text area says little more than the
country.

**Use a separate map identity** for the answerer's `lev.maps`
destination. The path response carries the full public key, and with
the user's own identity it would hand every stranger who asks the key
behind their LXMF address. A per-device map identity, unrelated to the
messaging identity, makes the answer reveal nothing but "this device
holds map pieces".

What the answer must not do is carry an announce or a public key in
place of the path request: the asker could build the `Destination`
from it, but behind a shared instance the link request would still not
leave its daemon (section 1.1), and the link would lose its MTU upgrade.

### 1.4 Two things to check first in the implementation

- Whether our shared-instance client (`leviculum-std`) self-registers a
  new SINGLE destination with a path response as the reference does
  (`Transport.py:2430`). Our daemon accepts it (the #255 note above
  names the case), but I found no client-side code that sends it in
  this pass. If it is missing, the first path request for a fresh map
  destination is lost, and the gap is a client bug worth its own mvr.
- That our daemon answers the forwarded path request with the
  pending-forward path end to end, behind a non-transport lnsd on both
  sides. This is the #171 case
  (`leviculum-core/src/transport.rs:8048`); the first implementation
  cell should cover it over AutoInterface.

## 2. Can the question stay off LoRa without the app knowing the medium

### 2.1 What the reference does

- A plain broadcast from a local client is sent by its daemon "directly
  on all attached interfaces" (`Transport.py:1518`), with no mode,
  bitrate or announce-cap check; **1.5.2** is the same
  (`Transport.py:1977`). Ours mirrors it
  (`leviculum-core/src/transport.rs:9328`).
- Interface modes (full, gateway, access point, roaming, boundary, and
  internal in **1.5.2**) gate only announces and path discovery
  (`Transport.py:1195`; **1.5.2** `Transport.py:1469` to `:1516`).
  A plain data packet goes out on every interface whatever its mode.
- The client cannot pick an interface: the shared-instance framing
  carries bare packets, and the client's own `Transport.interfaces` holds
  only its link to the daemon.
- On receipt, a plain packet with more than one hop is dropped
  (`Transport.py:1354`); **1.5.2** also records it as a protocol
  violation against the interface (`Transport.py:1644`). A question is
  never relayed, so it never costs a second hop's airtime, but nothing
  stops it on the first.

So in the reference the question reaches LoRa whenever the asker's
daemon has a LoRa interface, and the answer reaches LoRa whenever the
answerer's daemon has one.

### 2.2 What the interface layer could decide

Interface isolation puts the medium knowledge in the interface: the LoRa
interface knows its radio settings, airtime and duty budget
(`apply_airtime_limits`, `leviculum-nrf/src/lora.rs:1136`; duty hold at
`leviculum-nrf/src/lora.rs:306`). It could refuse client-originated
plain broadcasts. But that is a per-interface rule on a packet class,
and it changes what a Python client gets from lnsd: a stock app that
broadcasts on a plain destination through lnsd would reach LoRa
neighbours through `rnsd` and not through `lnsd`. That breaks rule 2
of the deviation rule (semantic compatibility). And it would make the
interface aware of a packet type, which the isolation rule refuses for
anything that looks like collision avoidance.

### 2.3 Recommendation: bound the discovery, gate the data

The core already knows the medium at the right moment without the app
knowing it: once the path response has arrived, the path entry names
the next-hop interface, and the daemon reports its bitrate
(`next_hop_interface_bitrate`, `leviculum-core/src/transport.rs:11081`;
the client gets it the way Loquax's board sync already does, classified
by `LinkClass::of`, `loquax/src/home/board.rs:127`). So:

- Question, answers and the path exchange go out on every medium. That
  is the medium-agnostic part, and it is cheap and bounded (below).
- **Before any link request**, an answerer whose next hop is
  `LinkClass::Radio` is dropped from the automatic race. "Nothing
  automatic over LoRa" then holds for every byte of map data and for
  the link setup; only discovery chatter crosses the radio.
- The power-user setting for LoRa map fetches (concept section 6) is
  the one place where a Radio answerer is kept.

Cost on LoRa per question, at SF8 with the sizes above: question 155 ms;
each LoRa-heard answer 196 ms; each path request 206 ms (247 ms with a
transport id); each path response 585 ms with a ratchet. With at most
three answerers followed up (section 3), the worst case is
155 + 3 × (196 + 206 + 585) ≈ 3.1 s of airtime per question. At one
question per 10 minutes that is 0.5 % of channel time, under a twentieth
of the 10 % duty budget of our default sub-band
(`etsi_eu868_duty_cycle`, `leviculum-core/src/rnode.rs:1544`). The
common case on LoRa is cheaper: answerers rarely exist, and with none
the cost is the 155 ms question.

**A decision for the reviewer to confirm:** the concept says "LoRa:
nothing automatic". Read strictly, even the question is automatic. This
study proposes reading it as "no map data and no link setup", with
discovery bounded as above. If the strict reading stands, the only
compatible option left is that the app asks its daemon whether any
interface is radio-class (`interface_bitrate_entries`,
`leviculum-core/src/transport.rs:12257`, already served as an RPC) and
does not ask neighbours at all while one is up. That loses every WLAN
neighbour on a device that also has a LoRa board, which is the
expedition case, so it is not recommended.

A second path response also crosses LoRa for no use: the reference's
pending-forward sends it without an attached interface, so a WLAN
answerer whose daemon also has a LoRa interface answers on both
(`Transport.py:1930` stores `attached_interface`, which is `None` from
`Transport.py:1871`; our #171 note at
`leviculum-core/src/transport.rs:8048` says the same). Attaching the
desiring interface would fix that in our daemon, wire-compatible and
semantically invisible to the requester; it qualifies under the
deviation rule only once the saved airtime is measured, so it is noted
here, not proposed for the first slice.

## 3. Rate limit of the question, and the random answer delay

### 3.1 Which limiters see these packets

- **Announce ingress control** counts announces only
  (`interface.received_announce()`, `Transport.py:1693`; thresholds
  `IC_BURST_FREQ_NEW` 3/s and `IC_BURST_FREQ` 10/s, `Interface.py:76`;
  ours `IC_BURST_FREQ_NEW_HZ`, `leviculum-core/src/transport.rs:126`).
  Question and answer are plain data and are not counted. The path
  responses are announces and are counted, at every neighbour on the
  interface they cross.
- **Path-request ingress control** counts path requests
  (`received_path_request`, `Transport.py:2895`; `IC_PR_BURST_FREQ_NEW`
  3/s, `Interface.py:78`). Above it a transport node stops recursive
  discovery for the burst (`Transport.py:3019`), and **1.5.2** adds an
  egress limit on path requests.
- **Plain data has no limiter anywhere.** Neither stack rate-limits a
  plain broadcast on ingress or egress. The question's bound is
  therefore the app's own, and so is the answerer's.

### 3.2 Proposed limits

Asker:
- one question per coarse tile per 10 minutes while the map is open;
  a new tile (the view crossed a zoom-6 boundary) asks at once;
- with no answer, back off 1, 2, 5, 10, then 30 minutes per tile;
- follow up at most three answers per question, ranked by coverage,
  their path requests spaced by at least 1 s. Three requests stay under
  `IC_PR_BURST_FREQ_NEW` at every neighbour, so no transport node's
  recursive discovery is switched off by our burst.

Answerer:
- answer only when it holds pieces for the tile signed by the named
  server key, is not on low battery, and has no own messages queued
  (concept section 6, "Messages come first");
- one answer per nonce, and at most one answer per 10 s overall, so a
  hostile asker can make a device spend at most one 46-byte packet per
  10 s;
- delay the answer by a uniform draw in [0, 2 s]; cancel it if three
  answers to the same nonce were heard first (the asker follows up only
  three).

### 3.3 The delay against the LoRa interface's half-duplex rules

The app-level delay only spreads the answers into the interface's queue.
On the radio, our LoRa interface already adds an acquisition jitter and
a CSMA wait before every frame, and opens an RX window after each
transmission sized to one full reply airtime plus the peer's turnaround
(`post_tx_rx_window_ms`, `leviculum-nrf/src/lora.rs:1035`). Bursts are
cut at 16 frames or 8 s of airtime (`MAX_BURST_FRAMES`,
`MAX_BURST_AIRTIME_MS`, `leviculum-nrf/src/lora.rs:998`). A 2 s window
is about ten answer airtimes (196 ms each), so three answerers rarely
draw the same slot, and when they do the CSMA resolves it. Nothing about
the question needs to be known to the interface: it is one more small
frame in its queue, which is how the isolation rule wants it.

On WLAN or Ethernet the 2 s window is pure latency, and it does not
matter there: the asker has already asked the Internet server in
parallel (the concept's race), and a neighbour that answers within 2 s
still wins any fetch longer than a few seconds.

## 4. The measurement ladder, and why 4 KiB is too dear for LoRa

### 4.1 The ladder

1. **Link RTT, free.** The reference measures it during setup: link
   request, proof, RTT packet (`Link.py:424`; ours `rtt_ms`,
   `leviculum-core/src/link/mod.rs:1345`). No extra bytes. On LoRa this
   rung is never reached automatically (section 2.3); the RTT is the
   first measurement on every other medium.
2. **A tiny request.** `/regions` for the one tile, or an empty `/ping`
   handler: one request packet and one response packet, about 115 and
   99 bytes. At SF8 that round trip is 699 ms of airtime; on WLAN it is
   a few milliseconds. RTT and this round trip together bound the
   per-packet latency without moving any payload.
3. **The first coarse piece is the measurement.** No separate probe:
   "coarse before fine" (concept section 6) means the first piece is
   small anyway, and timing its resource gives a transfer rate measured
   on bytes the user wanted. The next piece is asked only if that rate
   fits the time budget; the switch from the Internet server to a
   neighbour happens on this number.

### 4.2 What 4 KiB costs on LoRa

At MTU 500 a resource part carries `link.mtu - HEADER_MAXSIZE -
IFAC_MIN_SIZE` = 500 − 35 − 1 = 464 bytes (`Resource.py:338`). 4096
bytes plus the resource's random-hash prefix and the token overhead come
to about 4160 bytes, so **nine parts**, not "two LoRa-class response
packets" as `loquax-map-over-reticulum.md` section 3 says. Each part is a
483-byte packet, which our LoRa interface sends as two frames
(`MAX_SINGLE_PAYLOAD` 254, `leviculum-core/src/rnode.rs:2027`).

| Item at SF8 / BW 125 kHz / CR 4/5 | Airtime |
| --- | --- |
| one 483-byte resource part (two frames) | 1384 ms |
| advertisement (about 200 B) | 585 ms |
| part request (about 100 B) | 329 ms |
| resource proof (83 B) | 288 ms |
| **4 KiB probe, total** | **about 13.7 s** |
| path request + path response + link request + proof + RTT packet | 1757 ms |
| tiny request + response | 699 ms |
| 100 KB as resource parts (216 parts) | about 299 s |

13.7 s of airtime is the whole budget of 137 s of wall clock at the
10 % duty cycle of sub-band P, where our default frequency sits, and
the budget of nearly 23 minutes on a 1 % sub-band. It is also more than
the 8 s burst ceiling of our LoRa interface, so the probe is cut in two
by a forced yield. A rung that eats the channel for over ten seconds
just to learn the channel is slow is not a probe. The 100 KB row
confirms the concept's "over four minutes".

Two further faults of a fixed 4 KiB probe, on any medium:

- **It compresses.** A resource is bz2-compressed whenever that makes it
  smaller (`Resource.py:400`), and fixed bytes compress to a few dozen.
  A probe of fixed bytes sent with compression on measures one packet.
  If a probe survives anywhere (the server's `/probe` of S2), it must be
  random bytes or sent with compression off (the `compress` parameter of
  `send_response_resource_with`, `leviculum-core/src/node/mod.rs:2047`).
- **It measures slow start.** A resource starts with a window of four
  parts (`WINDOW`, `Resource.py:58`) and only grows towards
  `WINDOW_MAX_FAST` 75 after sustained fast rounds (`Resource.py:70`).
  Nine parts never leave the start, so on a fast link 4 KiB
  underestimates the rate a large piece would get. The first coarse
  piece, if it is a few hundred kilobytes, measures the real thing.

## 5. Proposed order text (implementation)

> **K11 (implementation): map pieces from neighbours (Refs #232).**
> Study `docs/drafts/map-neighbours-study-2026-11.md`. Lane 1, P2.
> Cells first, red, then the code.
>
> Cells (periculum, tier 2, two Loquax instances behind two lnsd, no
> Internet route):
> - `maps_neighbour_found`: B holds signed pieces for a tile, A opens
>   the map there; A's question, B's answer, A's path request, the path
>   response with hops 1 on the AutoInterface, a link, `/regions`, one
>   piece verified against the pinned server key and drawn.
> - `maps_neighbour_radio_manual`: the same with only a LoRa-class
>   interface between them (the tier-2 throttled stand-in): question and
>   answer cross, no link request is sent, no piece moves; with the
>   power-user setting on, one piece moves and the fetch ceiling holds.
> - `maps_neighbour_unsigned_refused`: B's pieces signed by a different
>   key: B does not answer A's question (key id mismatch); a forged
>   answer that leads to a piece with a bad signature is deleted and
>   counted failed.
> - `maps_question_bounded`: A pans across tiles for 30 minutes; count
>   questions, answers and path requests on the wire against section
>   3.2; B answers one nonce once.
> - `maps_python_neighbour_unaffected`: a stock rnsd neighbour on the
>   same segment logs no protocol violation and no dropped plain packet
>   with more than one hop, and its announce ingress limiter stays off.
>
> Code:
> 1. Loquax map core: plain destinations `lev.maps.ask` and
>    `lev.maps.offer` with the formats of study section 1.3; a separate
>    per-device map identity with the `lev.maps` SINGLE destination and
>    the request handlers of the server (`/regions`, `/region`).
> 2. Answerer: the rules of study section 3.2, delay [0, 2 s] with
>    suppression after three answers.
> 3. Asker: the race of the concept; at most three path requests per
>    question; drop a Radio-class next hop before the link request
>    (`LinkClass::of`); ladder RTT, tiny request, first coarse piece;
>    no separate probe for neighbours.
> 4. Check section 1.4 first: our shared-instance client's path-response
>    registration of a new destination, with an mvr if it is missing.
>
> Not in this order: a daemon filter for plain broadcasts on radio
> interfaces (refused, study section 2.2); attaching the desiring
> interface to forwarded path responses (needs a measurement first,
> section 2.3); the server's own `/probe` (S2 of the map-over-Reticulum
> concept, where the section 4.2 faults apply).
