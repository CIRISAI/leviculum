# Loquax: map data over Reticulum

Lew's decision, 2026-11-07 (leviculum 1481): every Loquax variant can
fetch map data over Reticulum itself, so the map server must be
reachable as a Reticulum destination. Rewritten on 2026-11-11 (order
1682) to the rules of [the expedition map](loquax-expedition-map.md),
section 6, which replace the older ones of this page:

1. **Only signed data is shared.** The server cuts the pieces and signs
   their index of hashes; a device verifies whatever arrives against
   the server's key, whoever carried it.
2. **Every Loquax is a map server,** always, on by default. The server
   and every Loquax speak one request protocol and differ in what they
   hold.
3. **A measured fast neighbour beats the Internet.** Server and
   neighbours are asked as a race, so the user never waits.
4. **Nothing automatic over LoRa.**
5. **The server's Reticulum address is the single trust anchor,**
   `map_destination`.
6. **`map_source` is an override,** not a second anchor.

Retired with them: the Internet as the rung that must fail before the
mesh is tried, the rule that a foreign map source is offered and never
used by itself, and the question to the user on a slow link (section
11 of the expedition map: no setting appears as a question in the
flow). This page decides the server, the client race, the numbers, and
the slices.

## 1. What exists

Today a region is cut on the device: the autoload (`Loader`,
`loquax/src/mapload.rs:254`) reads a PMTiles archive by HTTP range
requests — the newest Protomaps daily build (`PROTOMAPS_BUILDS`,
`leviculum-mapcut/src/http.rs:17`) or the archive `map_source` names
(`Source::of`, `loquax/src/mapload.rs:129`) — and writes an `.lmr`
region into the maps directory. One request's whole budget is 60 s
(`TIMEOUT`, `leviculum-mapcut/src/http.rs:21`); a load fetches at most
64 MB of archive (`LOAD_CAP`, `loquax/src/mapload.rs:68`); a failed box
backs off 30 s doubling to an hour (`BACKOFF_FIRST_MS`,
`loquax/src/mapload.rs:72`); autoloaded regions are a cache under
500 MB by default (`CACHE_DEFAULT_MB`, `loquax/src/mapload.rs:70`),
evicted least recently viewed (`evict`, `loquax/src/mapload.rs:746`).
None of the Reticulum side below is built.

The sizes, measured 2026-11-01 from the Protomaps build of 2026-10-28
([Loquax: the map](loquax-map.md), section "Regions"): a city region,
Frankfurt am Main z0–15 under profile 1, is 9,878,095 bytes; the
country region, Germany under the same cut, is 3,914,890,897 bytes; a
Bundesland to z15 about 0.4 GB. A city is mesh-plausible; a country is
Internet material, full stop.

`.lmr` compresses. Measured 2026-11-07 on the committed fixtures:
`leviculum-map/fixtures/firenze.lmr` 197,972 → 126,476 bytes under
bzip2 (36 % smaller), `berlin/mitte.lmr` 181,205 → 119,970 (34 %) —
the flat geometry inside is not pre-compressed the way gzipped MVT
tiles are. So the Resource's own compression (`bz2_compress`,
`leviculum-core/src/resource/compression.rs:18`), automatic below
64 MB (`RESOURCE_AUTO_COMPRESS_MAX`,
`leviculum-core/src/constants.rs:467`), carries every piece, and
pieces stay plain on disk and on the HTTP side: the transfer
compresses, the artefact does not.

## 2. The map server as a Reticulum destination

**The destination is `lev.maps`**: our own `lev` app name, as the
position beacon already uses it (`APP_NAME`,
`leviculum-lxmf/src/lev_position.rs:66`), a SINGLE destination,
announced on the propagation node's cadence. Not an `lxmf.*` name —
this is not LXMF traffic. Every Loquax holds a `lev.maps` destination
of its own as well (rule 2); what tells the server apart is the
address the device trusts, not the name.

**Its own daemon, `lnmapd`, not a mode of `lnpnd`.** The propagation
node's rules are ported to boards ([Propagation node on a
board](propagation-node-on-a-board.md)), where a map store never fits,
so folding a file server into `lnpnd` drags dead weight onto the
firmware seam; and a map outage must not restart message propagation.
`lnmapd` attaches to its host's `lnsd` shared instance exactly as every
client tool does and serves one directory of pieces plus their signed
index. The same directory is served over HTTP by the host's nginx: one
artefact, two transports, and the HTTP side is what a tier-2 cell
tests without a mesh. Where it runs is the expedition map's section 9:
the complete store at home, `leviculum.network` ([Self-hosted
infrastructure](self-hosted-infrastructure.md)) as the public front
door with a cache that fetches what it lacks from home once.

**Signed pieces, not arbitrary boxes.** Over Reticulum nothing is cut
on demand: the server answers from pieces cut beforehand, and their
index lists every piece with its hash, signed by the server's identity.
Pieces never change once cut, so any cache, mirror or neighbour can
serve them without being trusted. Whether a piece is a catalogue
region, as this page first planned, or a tile of a pyramid per kind of
data (the expedition map, section 5, proposed) is order K4's to
decide. Pieces a device cut itself from Protomaps over HTTP are not
signed and are never shared.

**The request API**, over a Link on `lev.maps`, with handlers
registered per path (`register_request_handler`,
`leviculum-core/src/node/mod.rs:1548`) and every answer a Resource
(`send_response_resource`, `leviculum-core/src/node/mod.rs:2030`):

- `/index` — no request data; the signed index as msgpack: per piece
  its id, bounds, zoom range, bytes and SHA-256 hash; the server's
  signature over it; proposed, the server's own HTTP address.
- `/piece` — request data is the id; the piece's bytes as a Resource,
  whose automatic bz2 takes the ~35 % measured above in flight.
- `/probe` — the speed probe, sized to the step it measures
  (section 3).

A neighbour answers the same paths with what it holds: the server's
signed index, or the part of it its pieces cover, and the pieces.
The server side is built in [The map server](map-server.md) (K4,
1686), which adds per-piece verification against the pinned key and
coarse-before-fine inside a time budget.

**The single trust anchor** is the server's Reticulum address, the
client config's `map_destination = <hash>`; the shipped default is
Lew's server, and a user may name another. The address is a hash of
the server's public key, so a key that arrives with an announce or
with a signed index either hashes to the configured address or is
rejected. No key is entered beside it. A piece is used iff its hash is
in an index that key signed, whoever carried it.

**`map_source` is an override.** Proposed: the signed index names the
server's own HTTP address, so the one value `map_destination`
configures both paths; `map_source = <url>` overrides the HTTP side,
as it names the PMTiles archive today. It anchors no trust: what it
serves is used as the autoload uses it now, cut on the device and
never shared.

## 3. The client race

The race lives once, in the shared Loquax core beside the autoload;
every frontend only renders its progress.

1. **Ask everyone at once.** The server over HTTP while the Internet
   answers, and beside it the neighbours: a broadcast to a plain
   destination, which Reticulum never carries past one hop
   (`reference/Reticulum/docs/source/understanding.rst`, "Plain
   destinations are only reachable directly"), naming the area
   coarsely since it is unencrypted. Whoever holds pieces answers after
   a random delay; then an ordinary encrypted link carries the
   protocol of section 2.
2. **Measure in steps.** A neighbour is one hop, and one hop is not
   fast: a LoRa neighbour is one hop too. The link's own round trip
   first, a tiny request next, more only after that was fast. The
   earlier fixed 4 KiB probe is too dear for LoRa and is not the first
   step.
3. **A measured fast neighbour wins.** Once a neighbour is measured
   faster than the server, the fetch moves to it, and it is remembered
   while it stays reachable. Without Internet: neighbours, then the
   server over Reticulum by path and link; no path within the
   transport's patience means the map shows what it has, and the
   existing per-box backoff governs retries.
4. **Coarse before fine, inside a time budget** (decided in outline).
   The device estimates from the measured rate and the index's sizes
   what arrives in a fixed time, fetches coarse pieces first and stops
   at the budget. A slow link therefore loads less depth by rule; the
   user is not asked.
5. **Nothing automatic over LoRa.** About 400 bytes a second before
   duty cycle; 100 KB is over four minutes of airtime on a shared
   channel. A radio-class link — the class the board's page fit
   already discriminates (`LinkClass`, `loquax/src/home/board.rs:115`)
   — never starts a map fetch by itself, not even a tiny one. A power
   user may allow it in the settings, off by default, with a fixed
   ceiling per fetch.
6. **Verify and land.** Every piece is checked against the signed
   index before it is kept; a piece whose hash does not match is
   deleted and counts as a failed load, and an index whose signature
   does not verify against `map_destination` is dropped with every
   piece that came with it. A fetch under way shows the autoload's
   usual progress and lands and evicts as an HTTP region does.

**Serving** (rule 2): a Loquax answers neighbours from the signed
pieces it holds. Messages come first (proposed): one transfer at a
time, paused while its own messages wait, and a giver on low battery
serves nothing. Open, for a study against the reference (K10): the
answer format that lets a link come up without a prior announce;
whether the question can stay off LoRa without the app knowing the
medium; the rate limit of the question.

## 4. Fairness on the mesh

A piece over LoRa occupies the channel, and the pacing lives where
[interface isolation](interface-isolation.md) puts it: the LoRa
interface's airtime rules space the parts, and the Resource's part
size follows the link's class and MTU, as the board's sync pages
already do (`Executor::fitted`, `loquax/src/home/board.rs:223`; the
1439 table in [The handheld messenger](handheld-messenger.md)). No
map-awareness enters transport. And map pieces are link traffic
between one device and one holder, the server or a neighbour, never
propagation payloads: a propagation node stores, peers and relays LXMF
messages, and it never relays map resources it did not itself ask for
— there is no store-and-forward rung for maps.

## 5. All targets

One race in the core; each frontend shows the map building up as
pieces arrive and asks nothing. The power user's LoRa allowance lives
in each target's settings. The M9 reaches the server and its
neighbours over WLAN, BLE or LoRa as its interfaces come up (the
1476/1477 batch) and lands pieces on its card, where a city (10 MB)
and a Bundesland (0.4 GB) fit at every zoom; only a city-sized cut fits
the 16 MB NOR, so the card is the store and a cardless M9 fetches
nothing — it draws the world overview and what it has.

## 6. The slices

Each slice lands with happy and unhappy periculum cells.

- **S1 — the server.** `lnmapd`, `lev.maps`, the signed `/index` and
  `/piece`. Cells: `maps_index_served` (announce heard, the index's
  signature verified against `map_destination`, one piece fetched, hash
  verified); `maps_server_down` (daemon stopped: no path, the map shows
  what it has, backoff — no retry storm); `maps_hash_mismatch` (a
  tampered piece: deleted, load counts failed); `maps_foreign_key`
  (an index signed by a key that does not hash to `map_destination`:
  dropped, nothing kept).
- **S2 — the race on window and terminal.** Stepped measurement and
  the time budget. Cells: `maps_fast_link_auto` (TCP-class link: the
  piece lands inside the bound); `maps_slow_link_coarse_first`
  (throttled link: coarse pieces land, the budget stops the fine
  ones, no question); `maps_lora_nothing_automatic` (radio-class link
  only: no map byte moves; with the power user's allowance, the
  ceiling holds); `maps_link_lost` (link dropped mid-resource: no part
  file remains, next view retries).
- **S3 — neighbours.** Every Loquax serving its signed pieces. Cells:
  `maps_neighbour_beats_internet` (a LAN neighbour measured faster
  than the server takes over the fetch); `maps_neighbour_one_hop` (the
  plain question is not carried past one hop);
  `maps_unsigned_not_shared` (a region the device cut from Protomaps
  is never offered); `maps_neighbour_tampered` (a neighbour's piece
  with a wrong hash: deleted, the server asked instead).
- **S4 — phone and M9.** The same cells through the Android bridge
  and the M9 executor; M9 adds `maps_m9_card` (piece on the card,
  drawn after reboot) and the cardless refusal.
- **S5 — pacing on LoRa, on the rig.** With the power user's
  allowance, a fetch beside a running chat on the same channel: the
  chat's delivery rate stays at the lab bar, the fetch paces by the
  interface's airtime rules and stops at its ceiling, measured with
  `LEVICULUM_DELIVERY_LOG` before and during.
