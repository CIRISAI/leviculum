# Loquax: map data over Reticulum

Lew's decision, 2026-11-07 (leviculum 1481): every Loquax variant can
fetch map data over Reticulum itself, so the map server must be
reachable as a Reticulum destination. The Internet stays preferred and
is tried first, automatically; only when it fails does map data come
over the mesh, and automatically only when the connection to the map
server is measured fast enough. When it works but is slow, the user is
asked before a byte of region data moves. This page decides the server,
the client ladder, the numbers, and the slices.

## 1. What exists

Today a region is cut on the device: the autoload (`Loader`,
`loquax/src/mapload.rs:243`) reads a PMTiles archive by HTTP range
requests — the newest Protomaps daily build (`PROTOMAPS_BUILDS`,
`leviculum-mapcut/src/http.rs:17`) or the archive `map_source` names
(`Source::of`, `loquax/src/mapload.rs:118`) — and writes an `.lmr`
region into the maps directory. One request's whole budget is 60 s
(`TIMEOUT`, `leviculum-mapcut/src/http.rs:21`); a load fetches at most
64 MB of archive (`LOAD_CAP`, `loquax/src/mapload.rs:64`); a failed box
backs off 30 s doubling to an hour (`BACKOFF_FIRST_MS`,
`loquax/src/mapload.rs:68`); autoloaded regions are a cache under
500 MB by default (`CACHE_DEFAULT_MB`, `loquax/src/mapload.rs:66`),
evicted least recently viewed (`evict`, `loquax/src/mapload.rs:718`).

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
`leviculum-core/src/constants.rs:467`), carries every catalogue region
except country cuts, and regions stay plain `.lmr` on disk and on the
HTTP side: the transfer compresses, the artefact does not.

## 2. The map server as a Reticulum destination

**The destination is `lev.maps`**: our own `lev` app name, as the
position beacon already uses it (`APP_NAME`,
`leviculum-lxmf/src/lev_position.rs:66`), a SINGLE destination,
announced on the propagation node's cadence. Not an `lxmf.*` name —
this is not LXMF traffic.

**Its own daemon, `lnmapd`, not a mode of `lnpnd`.** The propagation
node's rules are ported to boards ([Propagation node on a
board](propagation-node-on-a-board.md)), where a region archive never
fits, so folding a file server into `lnpnd` drags dead weight onto the
firmware seam; and a map catalogue outage must not restart message
propagation. `lnmapd` runs on the workorse host (`leviculum.network`,
the small VPS of [Self-hosted
infrastructure](self-hosted-infrastructure.md)) beside the propagation
node, attaches to the host's `lnsd` shared instance exactly as every
client tool does, and is configured with one directory of `.lmr` files
plus a generated index. The same directory is served at
`https://leviculum.network/maps/` by the existing nginx: one artefact,
two transports, and the HTTP side is what a tier-2 cell tests without
a mesh.

**A catalogue, not arbitrary boxes.** Over HTTP the device keeps
cutting exact view boxes as today. Over Reticulum the server answers
from pre-cut regions (`leviculum-map-tool cut`, cities and Bundesländer
of interest, re-cut when the archive updates): a fixed catalogue makes
`/region` answers cacheable and hash-stable, and cutting on demand
from the whole planet archive is server work the VPS does not owe
every stranger. The client takes the smallest catalogue region
covering its view.

**The request API**, over a Link on `lev.maps`, with handlers
registered per path (`register_request_handler`,
`leviculum-core/src/node/mod.rs:1548`) and every answer a Resource
(`send_response_resource`, `leviculum-core/src/node/mod.rs:2030`):

- `/regions` — no request data; the index as msgpack: per region id,
  name, bounds, zoom range, bytes, and the SHA-256 hash of the file.
- `/region` — request data is the id; the `.lmr` bytes as a Resource,
  whose automatic bz2 takes the ~35 % measured above in flight.
- `/probe` — no request data; 4 KiB of fixed bytes, the speed probe.

**How it is found:** by its announce, which every zero-config client
hears through the shipped TCP entry ([Zero configuration](
zero-config.md) — `leviculum.network` is the list's first entry). The
client config pins `map_destination = <hash>`; the shipped default pin
is Lew's server. A heard `lev.maps` announce that is not the pinned one
is offered, never auto-used: map data is low-risk, but a silently
substituted map is still wrong data.

## 3. The client ladder

The ladder lives once, in the shared Loquax core beside the autoload;
every frontend only renders its question.

1. **Internet, as today.** The HTTP cut with its 60 s budget. Only
   when it fails unreachable does rung 2 start — a 404 or a too-large
   box is an HTTP-world error and stays one.
2. **Path.** The pinned destination's path is requested; no path
   within the transport's patience is "does not work": the map shows
   what is here, with the sentence "The map server is not reachable
   over the mesh (no path)." The existing per-box backoff governs
   retries; like "no internet" today, a fresh view asks again.
3. **Link and probe.** Link up, `/probe` asked: 4 KiB over the link's
   RTT gives measured bytes per second. 4 KiB is two LoRa-class
   response packets — cheap enough to repeat, big enough that the
   rate is a transfer rate and not an RTT echo.
4. **The decision, in numbers.** Estimated time = the index's bytes
   for the region ÷ the measured rate. **Fast enough means the region
   arrives within 2 minutes.** Frankfurt's 9.9 MB in 120 s needs
   82 kB/s: a LAN- or WLAN-class link passes, and the fetch starts
   automatically. A LoRa link at 1 kB/s would take 2¾ hours for the
   same region: that is slow, so the user is asked. Independently of
   the estimate, **a radio-class link always asks** — the class the
   board's page fit already discriminates (`LinkClass`,
   `loquax/src/home/board.rs:115`) — so no fetch ever starts over
   LoRa without the answer, not even a tiny one.
5. **The question**, rendered by each platform, sizes and time from
   the index and the probe, never a hash on screen: English master
   "Load the map of Berlin over the mesh? 1.2 MB, about 25 minutes —
   Load / Later", in German "Karte für Berlin über das Funknetz
   laden? 1,2 MB, etwa 25 Minuten. Laden / Später". **Later** holds
   for that region for the session; the next session may ask again.
   A fetch under way shows the autoload's usual progress and lands,
   verifies and evicts exactly as an HTTP region does; a hash that
   does not match the index deletes the file and counts as a failed
   load.

## 4. Fairness on the mesh

A region over LoRa occupies the channel, and the pacing lives where
[interface isolation](interface-isolation.md) puts it: the LoRa
interface's airtime rules space the parts, and the Resource's part
size follows the link's class and MTU, as the board's sync pages
already do (`Executor::fitted`, `loquax/src/home/board.rs:223`; the
1439 table in [The handheld messenger](handheld-messenger.md)). No
map-awareness enters transport. And map regions are link traffic
between one client and the server, never propagation payloads: a
propagation node stores, peers and relays LXMF messages, and it never
relays map resources it did not itself ask for — there is no
store-and-forward rung for maps.

## 5. All targets

One ladder in the core; the window shows a dialog, the terminal a
prompt line, the phone a sheet, the M9 its two-softkey question on the
panel. The M9 reaches the server over WLAN, BLE or LoRa as its
interfaces come up (the 1476/1477 batch) and lands regions on its
card, where a city (10 MB) and a Bundesland (0.4 GB) fit at every
zoom; only a city-sized cut fits the 16 MB NOR, so the card is the
region store and a cardless M9 is asked nothing — it draws the radar
and what it has.

## 6. The slices

Each slice lands with happy and unhappy periculum cells.

- **S1 — the server.** `lnmapd`, `lev.maps`, `/regions` and `/region`.
  Cells: `maps_index_served` (announce heard, index read, one region
  fetched, hash verified); `maps_server_down` (daemon stopped: NoPath,
  the sentence, backoff — no retry storm); `maps_hash_mismatch` (a
  tampered file: deleted, load counts failed).
- **S2 — the ladder on window and terminal.** Probe, thresholds,
  question. Cells: `maps_fast_link_auto` (TCP-class link: no
  question, region lands inside the bound); `maps_slow_link_asks`
  (throttled link: the question with the numbers, "Load" lands it);
  `maps_later_holds` ("Later": no bytes move, the session does not
  ask that region again); `maps_link_lost` (link dropped mid-resource:
  no part file remains, next view retries).
- **S3 — phone and M9.** The same cells through the Android bridge
  and the M9 executor; M9 adds `maps_m9_card` (region on the card,
  drawn after reboot) and the cardless refusal.
- **S4 — pacing on LoRa, on the rig.** A city-region fetch beside a
  running chat on the same channel: the chat's delivery rate stays at
  the lab bar, the fetch paces by the interface's airtime rules,
  measured with `LEVICULUM_DELIVERY_LOG` before and during.
