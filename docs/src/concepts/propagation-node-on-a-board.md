# An LXMF propagation node on the boards' internal flash

This page used to cost the store on a QSPI NOR part — 1 MB on the Pocket
V2, 2 MB on the T114. Neither board carries one: both beliefs came from
an `EXTERNAL_FLASH_DEVICES` line that is a template default on both
vendors, and three units answered nothing to a JEDEC read (081522b2; the
evidence sits in the board files, `CONFIG`,
`leviculum-nrf/src/boards/t114.rs:167` and `CONFIG`,
`leviculum-nrf/src/boards/rak4631.rs:145`). So the store went where there
is flash: **16 pages of the nRF52840's own flash between the firmware
image and the persistence pages** (`STORE`, `leviculum-nrf/memory.x:85`,
landed in 81fcb46e; the log format chosen in 59c36129). Every capacity,
endurance and scan figure below is recomputed for that region, and the
region is two orders of magnitude smaller than the part this page first
costed: **176 field-sized messages, not 5 544.** If a board or an add-on
ever brings a QSPI part, the arithmetic is the same arithmetic with a
bigger region and a tenfold larger erase budget; nothing below assumes
one.

Codeberg #384 asks whether such a store should hold an LXMF propagation
node, so the mesh has somewhere to put a message when the recipient is
not reachable. The walk that prompted it had a link built from one phone
to another across two of our nodes and a hill, which is exactly the
topology where a store matters.

This page establishes what the role obliges us to, measures what it
costs on the region we now have, sets out the options, and recommends
one. It is a design document. Nothing here is a status page; what is
open belongs on the tracker.

The framing binds the whole argument. The reference is a source of
ideas, never a blueprint. What binds us is wire and semantic
compatibility: **a Python or Sideband peer must be able to use our node
as a propagation node without knowing it is small.** Everything else is
ours to design, and a board in a pocket is not a server in a basement.

## 1. What the role obliges us to

Established against `reference/LXMF` at 1.1.0.

### The destinations, and the verbs on them

A propagation node owns one inbound SINGLE destination,
`lxmf.propagation`, created from the router identity
(`propagation_destination`, `LXMRouter.py:190`). Two request handlers
hang off it, and they are the entire public protocol:

| Verb | Path | Who calls it | Handler |
|---|---|---|---|
| offer | `/offer` | another propagation node | `offer_request` (`LXMRouter.py:2266`) |
| get | `/get` | a client (Sideband, `lnmsg`, `rnsd`) | `message_get_request` (`LXMRouter.py:1482`) |

The paths are constants on the peer
(`OFFER_REQUEST_PATH`, `LXMPeer.py:14`; `MESSAGE_GET_PATH`,
`LXMPeer.py:15`). A third destination, `lxmf.propagation.control`,
carries the operator verbs `/pn/get/stats`, `/pn/peer/sync` and
`/pn/peer/unpeer` (`STATS_GET_PATH`, `LXMRouter.py:89`;
`SYNC_REQUEST_PATH`, `LXMRouter.py:90`; `UNPEER_REQUEST_PATH`,
`LXMRouter.py:91`) and is behind an allow-list, so it is not part of
what a stranger can drive.

Messages move in three shapes, and only three:

1. **A client uploads one message.** A single link packet carrying
   `[timestamp, [lxmf_data || stamp]]` (`propagation_packet`,
   `LXMRouter.py:2234`). No peering key is needed for a single
   message. The node proves the packet — and it proves it *after*
   storing, not before (`packet.prove`, `LXMRouter.py:2255`).
2. **A peer offers a batch.** `/offer` carries
   `[peering_key, [transient_id, …]]`; the node answers `True` (want
   all), `False` (want none), or the sublist it wants
   (`offer_request`, `LXMRouter.py:2266`). The bodies then follow as
   one Reticulum Resource.
3. **A client drains its mailbox.** `/get` with both fields `None`
   returns the list of transient IDs held for that client's delivery
   destination; a second `/get` with `[wants, haves, limit]` returns
   the bodies and *deletes* everything in `haves`
   (`message_get_request`, `LXMRouter.py:1482`). The client sends the
   purge only after it has taken local delivery
   (`message_get_response`, `LXMRouter.py:1607`).

A transient ID is `SHA-256(lxmf_data)` where `lxmf_data` is
`destination_hash || destination-encrypted payload`. We already
implement the client half of this exchange (`MESSAGE_GET_PATH`,
`leviculum-lxmf/src/propagation.rs:25`).

### What is protocol, and what is that implementation's bookkeeping

Per message the reference keeps seven fields
(`propagation_entries`, `LXMRouter.py:2518`): destination hash,
file path, receive timestamp, size, handled peers, unhandled peers,
stamp value.

Of those, **three are protocol**: the destination hash (it decides who
may `/get` the message), the transient ID (the key of every exchange),
and the bytes themselves. The stamp value is protocol-adjacent — a
peer drops messages whose stamp value is below its requirement
(`sync`, `LXMPeer.py:267`) — but a node that requires nothing needs
only to remember zero. The receive timestamp is local policy: it feeds
expiry and the cull weight (`clean_message_store`, `LXMRouter.py:1144`),
and no peer ever sees it. The file path and the two peer lists are pure
bookkeeping of *that* design.

Per peer, `to_bytes` (`LXMPeer.py:138`) persists twenty-odd fields.
Only four of them are visible on the wire in any form: the peer's
destination hash, its peering key, its announced limits, and its
announced costs. Everything else — link establishment rate, sync
transfer rate, rx/tx byte counters, offered/outgoing/incoming counts,
backoff state — is statistics. **And two of them are the problem:**
`handled_ids` and `unhandled_ids`, a pair of 32-byte-per-message sets
*per peer*, because every message the node accepts is enqueued into
every other peer's unhandled set
(`flush_peer_distribution_queue`, `LXMRouter.py:2472`).

That distinction decides what we may drop. We may drop all of the
statistics and both peer sets. We may not drop the destination hash,
the transient ID, or the bytes.

### What it advertises, and whether a peer believes it

The propagation announce is a seven-element msgpack list
(`get_propagation_node_app_data`, `LXMRouter.py:324`):

| # | Field | Reference default |
|---|---|---|
| 0 | legacy PN support | `False` |
| 1 | node timebase | now |
| 2 | propagation node state | `True` |
| 3 | per-transfer limit, kilobytes | 256 (`PROPAGATION_LIMIT`, `LXMRouter.py:55`) |
| 4 | per-sync limit, kilobytes | 10240 (`SYNC_LIMIT`, `LXMRouter.py:59`) |
| 5 | `[stamp cost, flexibility, peering cost]` | `[16, 3, 18]` (`PROPAGATION_COST`, `LXMRouter.py:54`; `PEERING_COST`, `LXMRouter.py:50`) |
| 6 | metadata dict | name |

**A node can honestly announce a small capacity, and peers respect
it.** Field 3 is enforced by the *offering* peer: a message larger than
our advertised transfer limit is dropped from its queue for us and
marked handled, so it is never retried (`sync`, `LXMPeer.py:267`).
Field 4 is enforced by us: an inbound resource larger than the
advertised sync limit is refused before it transfers
(`propagation_resource_advertised`, `LXMRouter.py:2206`). Field 5 is
read by both, and a client mines its stamp to the cost we name.

Two caveats, and they matter.

- **For a client, field 3 is advisory.** Nothing on the client side
  checks a node's transfer limit before uploading; the only
  enforcement is our refusal of the resource, which the client reports
  as a failed sync rather than as "too big".
- **We would be the first to advertise cheap.** The reference clamps
  its own configured cost up to `PROPAGATION_COST_MIN`
  (`LXMRouter.py:52`, applied at `LXMRouter.py:137`), so a Python node
  never announces below 13. Announcing 0 is wire-legal and semantically
  honoured — a peer's accepted cost is `max(0, our_cost − flexibility)`
  — but it is a policy nobody else in the mesh runs, and it hands away
  the only spam brake the protocol has.

The announce also has a switch: field 2 false makes every Python router
*unpeer* us on the next announce (`LXMFPropagationAnnounceHandler`,
`Handlers.py:35`). That is the clean way to leave the role, and it is
also the reason a node that serves only static peers is invisible to
clients: the reference computes field 2 as "propagation node **and
not** static-only".

### What a peer expects when a node forgets. This is the crux

**There is no verb for it.** Say it plainly, because the design has to
be built around the absence.

The error space is rich and none of it means "I dropped it":
`ERROR_NO_IDENTITY`, `ERROR_NO_ACCESS`, `ERROR_INVALID_KEY`,
`ERROR_INVALID_DATA`, `ERROR_INVALID_STAMP`, `ERROR_THROTTLED`
(`LXMPeer.py:29`), `ERROR_NOT_FOUND` (`LXMPeer.py:30`),
`ERROR_TIMEOUT`. `ERROR_NOT_FOUND` is defined and never returned by
either request handler.

What actually happens when a message is gone:

- On `/get` **list**, it is simply not in the returned list. The client
  cannot tell "never arrived" from "arrived and was dropped".
- On `/get` **fetch**, a wanted ID that is no longer in the store is
  skipped silently and the response is shorter than the request
  (`message_get_request`, `LXMRouter.py:1482`). No error, no gap
  marker.
- On the peer side the same thing happens in reverse: the offering
  peer discovers on its next sync that an ID it had queued is gone
  from its own store and quietly drops it (`sync`, `LXMPeer.py:267`).

And the reference already forgets, routinely and silently: messages
expire after 30 days (`MESSAGE_EXPIRY`, `LXMRouter.py:38`) and, when
the store exceeds its configured limit, entries are culled by a weight
of `age × size` until enough bytes are free (`clean_message_store`,
`LXMRouter.py:1144`). Peers vanish after 14 days unreachable
(`MAX_UNREACHABLE`, `LXMPeer.py:39`).

Two conclusions follow, and they point in opposite directions.

**Forgetting is normal, so a small node forgets faster, not
differently.** There is no promise in the protocol that we would be
breaking. Sideband's own retry behaviour already has to cope with a
node that dropped something.

**But acceptance is proven and retention is not.** The node proves the
upload packet (`packet.prove`, `LXMRouter.py:2255`), so the sender is
told "accepted" and is never told "and then discarded". A store that
accepts more than it can plausibly hold converts a proof of acceptance
into a lie by omission. **So our design must avoid promising**: accept
less rather than accept and drop, and make the advertised limits small
enough that the acceptance is honest.

The one honest back-pressure verb that does exist is
`ERROR_THROTTLED`, and a peer handles it correctly by deferring its
next sync (`LXMPeer.py:421`). "Not now" is expressible. "Not ever" is
not.

## 2. The numbers

### Where the store lives

One file decides it. `leviculum-nrf/memory.x` carves `STORE` out of the
top of the application window and exports `__srecord_store` /
`__erecord_store`; `region` (`leviculum-nrf/src/record_store.rs:220`)
reads those two symbols, and nothing else in the tree knows the
addresses.

| Address | Length | What |
|---|---|---|
| `0x00000` | 4 KiB | MBR |
| `0x01000` | 152 KiB | SoftDevice S140 v7.3.0 |
| `0x27000` | `0xB3000`, 716 KiB | `FLASH` — the firmware image window |
| `0xDA000` | `0x10000`, 64 KiB, **16 pages** | `STORE` — the record log |
| `0xEA000` | 4 KiB | telemetry target / fixed position / media profile |
| `0xEB000` | 4 KiB | radio configuration |
| `0xEC000` | 4 KiB | identity |
| `0xED000` | 28 KiB | Heltec license/version data, T114 only |
| `0xF4000` | — | bootloader |

Source for every row: the map at the head of `memory.x` (`FLASH`,
`leviculum-nrf/memory.x:78`; `STORE`, `leviculum-nrf/memory.x:85`).

**The gap between image end and store start**, as
`scripts/check-nrf-store-gap.sh` reports it on every `just fast` — this
run, on the tree at 81fcb46e:

```text
[store-gap] t114     image ends 0xa55a0, store 0xda000..0xea000 (16 pages), gap 215648 B (210 KiB)
[store-gap] rak4631  image ends 0xa6c08, store 0xda000..0xea000 (16 pages), gap 209912 B (204 KiB)
```

The gate measures the PT_LOAD segments the `.uf2` is built from, not the
sections the linker charged to `FLASH`, and it reads the region's bounds
from the symbols the firmware itself mounts. An image that grew into the
region would be a link error before it could be a lost store: three
`ASSERT`s in `memory.x` hold the edges (`ASSERT`,
`leviculum-nrf/memory.x:223`).

**Why the region survives a UF2 update — and what is not yet proven.**
The store sits *inside* the bootloader's writable window, so
`USER_FLASH_END` (`0xEA000`) does not protect it the way it protects the
three persistence pages above. What protects it is that the Adafruit
bootloader erases only the pages it writes: `flash_nrf5x_write` buffers
one page and `flash_nrf5x_flush` (upstream `src/flash_nrf5x.c`) erases
and programs exactly that page, and only when its content differs. Our
`.uf2` carries blocks from `0x27000` to the end of the image and none
above it, so no page of the store is ever a block's target and no erase
reaches one (`docs/src/concepts/lnode-flashing.md`, §What a UF2 is
allowed to write).

That is an argument from the bootloader's source, and it is **not yet a
board proof**: nobody has written records to a board, flashed a new
`.uf2` over it and remounted. Until that run exists, treat "the store
survives a firmware update" as expected rather than as established. It
is in the batch list in §4.

### What our messages actually weigh

Measured, not assumed. Source: the two field logs from the 2026-09-09
walk, `/home/lew/rig-run/feld-archiv/pocket-lauf10.log` and
`t114-lauf10.log`, 12.69 h of wall clock each (10:52 to 23:33).

Method: for each `[TELEMETRY] report target=` line, take the packet the
node emitted within the next 14 log lines, excluding the two lengths
that are the nodes' own `lxmf.delivery` announces (181 and 183). 284
messages, which agrees with the 288 `report` lines to within the four
that straddle a log boundary.

| On-wire packet (Type 1) | Count |
|---|---|
| 227 B | 6 |
| 259 B | 80 |
| 275 B | 198 |

Median 275 B, worst case 275 B, minimum 227 B. A Type 1 header is 19 B
(`HEADER_MINSIZE`, `leviculum-core/src/constants.rs:69`), and the
propagation form re-prepends the 16-byte destination hash, so
`lxmf_data` is the packet length minus 3: **median 272 B, range 224 to
272 B.** With the 32-byte propagation stamp appended, the **stored
object is 304 B median, 256 to 304 B over the run.**

Cross-check against the relayed form: the same message crossing a hop
was logged as `[LORA] TX split 291 bytes (254+37)` with a Type 2
header, and 291 − 35 = 256 = 275 − 19. The two framings agree.

**No text messages were present.** The walk carried telemetry only, so
this distribution is a telemetry distribution and nothing else. A
Sideband text message is `LXMF_OVERHEAD` = 112 B
(`LXMF_OVERHEAD`, `LXMessage.py:63`) plus the RNS encryption overhead
plus the text, so a one-line message lands in the same 250 to 350 B
band; anything with an image or an audio field is one to two orders of
magnitude larger and is exactly what the advertised transfer limit
exists to refuse.

### How many fit

From the record log as built, not from the costing this page first did.
The header is still the 42 bytes that costing tabulated; what changed
with the part is the region, the page header, and that every offset is a
multiple of a word because `sd_flash_write` takes a length in words.

| Field | Bytes |
|---|---|
| body length, u16 LE | 2 |
| key — the transient ID | 32 |
| timestamp, u32 LE | 4 |
| tag — the stamp value | 1 |
| flags: `0xFF` uncommitted, `0xFE` live, `0xFC` purged | 1 |
| CRC-16 over the header and the body | 2 |
| **header total** (`HEADER_LEN`) | **42** |
| body | `len` |
| padding to a multiple of 4 | 0 to 3 |

(`HEADER_LEN`, `leviculum-nrf/record-log/src/lib.rs:218`; the layout is
tabulated at `leviculum-nrf/record-log/src/lib.rs:108-119`.) The
destination hash is not a field: it is the first 16 bytes of the body,
as it is in the reference, which reads it back from the head of its file
(`LXMRouter.py:2498`).

Each page carries a 12-byte header written once per erase
(`SECTOR_HEADER_LEN`, `leviculum-nrf/record-log/src/lib.rs:220`), which
leaves 4 084 B of the 4 096 for records (`SECTOR_PAYLOAD`,
`leviculum-nrf/record-log/src/lib.rs:225`). A record never straddles a
page.

At the measured median body of 304 B the stride is
`align_up(42 + 304)` = **348 B** (`record_stride`,
`leviculum-nrf/record-log/src/lib.rs:259`), so 4 084 / 348 = 11 records
to a page with 256 B of tail (6.3 %).

| Body | Stride | Per page | In the 16-page region |
|---|---|---|---|
| 304 B — the field median | 348 B | 11 | **176** |
| 256 B — the smallest the walk produced | 300 B | 13 | 208 |
| 4 042 B — the largest the format allows (`MAX_BODY`, `leviculum-nrf/record-log/src/lib.rs:227`) | 4 084 B | 1 | 16 |

The 176 is the count at the brim. Reclaim is round-robin — when the
active page cannot fit the next record the *next* page is erased and
becomes active — so a store in steady state holds between 166 (just
after a reclaim: fifteen full pages and one record) and 176.

Where the 64 KiB goes at that fill: 53 504 B of message body, 7 392 B of
record headers, 352 B of record padding, 192 B of page headers and
4 096 B of per-page tail. **52 KiB of the 64 is message.**

For scale in the other direction: one message at the reference's default
per-transfer limit of 256 kB (`PROPAGATION_LIMIT`, `LXMRouter.py:55`) is
63 times the largest body this format can hold at all. That is the
argument for announcing a small field 3, and it is now an argument about
a hard bound rather than a preference — see *What we announce* below.

### Endurance

The budget got an order of magnitude worse with the part: **10 000 erase
cycles per page** on the nRF52840, against 100 000 on the NOR parts this
page first costed (nRF52840 Product Specification, NVMC chapter, quoted
at `leviculum-nrf/record-log/src/lib.rs:37`).

Duty, measured: 284 messages from two moving trackers over 12.69 h =
22.4 messages/hour = 196 224 a year. At 11 field-sized records to a
page, that is 17 838 page erases a year, and where they land is the
whole design:

| | Erases/year | Budget | Life |
|---|---|---|---|
| Spread over the 16 pages | 1 115 per page | 10 000 | **9 years** |
| Spread over 68 pages (the whole window, for scale) | 262 per page | 10 000 | 38 years |
| One fixed metadata page | 196 224 | 10 000 | **18.6 days** |

(The table is the spike's, recomputed for the region as landed:
`leviculum-nrf/record-log/src/lib.rs:55-59`.)

Read the last row twice. **The message data is not the endurance risk; a
fixed metadata page is.** A store that keeps its head pointer, its index
or its sequence counter at a fixed address and rewrites it on every
accepted record spends its entire budget in eighteen days at the duty we
actually measured in the field. On the external part the same line read
six months, which is long enough to sound survivable. It is why this
format has no superblock, no index page and no head pointer: everything
the log needs to mount itself is recovered by reading the page headers,
and a page header is written exactly once per erase of the page it
heads.

**16 pages is a size choice, and it has a price.** 68 pages — the rest
of the application window — would have bought 38 years and cost the
image 212 KiB of headroom it may want for BLE and LXMF; the gate above
says 210 KiB of gap is what remains at 16. Widening the region upward is
impossible, because `0xEA000` is the bootloader's `USER_FLASH_END`, and
widening it downward moves every record, so a later resize is a
reformat. That is the deliberate price of the smaller default, and
`mount` (`leviculum-nrf/src/record_store.rs:522`) already treats a
region that is not ours as unformatted rather than as corrupt, so the
reformat is a boot line and not an incident.

The 9 years is at the field walk's telemetry duty and scales with it:
ten times that duty is **11 months**, a hundred times is **33 days**.
Those are the numbers to re-run when a real message mix exists rather
than a telemetry one.

### Writing next to the radio

With the SoftDevice enabled the NVMC is *Restricted*: only
`sd_flash_write` / `sd_flash_page_erase` may touch this flash (S140 SDS,
Hardware peripherals), which is also where a word becomes the only
program unit and two writes per word between erases the only budget
(`leviculum-nrf/record-log/src/lib.rs:37-41`). A page erase is 85 ms, a
word write 41 µs (same source).

Per median record the log does three program runs — the header up to the
commit word, then the CRC and the body from the far side of it, then the
commit word itself — 87 words in all, 3.6 ms of NVMC time, and one
85 ms erase every eleventh record.

**But NVMC time is not the cost that matters here.** The SoftDevice
schedules flash work between radio events and fails the operation
outright when it finds no gap (S140 SDS, Flash API timing), so a refusal
is a statement about the next few milliseconds of radio traffic and not
about the part. The store answers it with four attempts and a doubling
delay — 50, 100, 200 ms (`FLASH_ATTEMPTS`,
`leviculum-nrf/src/record_store.rs:207`) — and prints every refusal and
a running count on its debug port.

A refusal part way through an append **seals the page**: the rest of it
is given up, because programming over bytes already down would have to
raise bits. The spike's sweep puts a number on how often that is the
outcome — of the twenty places a refused operation can land inside one
append, three leave the page usable and seventeen give up the rest of it
(`sealed`, `leviculum-nrf/store-spike/tests/record_log.rs:360`). What
sealing costs in wear is the endurance arithmetic with fewer records to
a page: a board that sealed on every append would erase a page per
message, 196 224 erases a year over 16 pages, and spend the nine years
in **10 months**. That is the upper bound, not an expectation; it is
also the reason the refusal counter is on the debug port rather than
silent.

**What the erase storm does to BLE and LoRa is to be measured on the
rig, and this page will not predict it.** The instrument is already in
the firmware: `STORE_STORM` (`TYPE_STORE_STORM`,
`leviculum-core/src/envelope.rs:250`) appends N synthetic records of a
given size, bounded at 1 000 records of 1 024 B
(`STORE_STORM_MAX_BYTES`, `leviculum-core/src/envelope.rs:272`) and
tagged so a later batch can purge exactly those (`TAG_BENCH`,
`leviculum-nrf/src/record_store.rs:81`); `lnflash --store-storm
COUNT[,BYTES]` sends it (`--store-storm`, `lnflash/src/main.rs:327`).
The numbers owed are a connected phone's throughput and a LoRa link's
delivery rate across a storm, measured against the same run without one.

### Scan, and what it costs in RAM

**Reads do not go through the SoftDevice's flash scheduler at all.** The
internal flash is memory-mapped and the read is a `memcpy` that cannot
fail or be refused (`read`, `leviculum-nrf/src/record_store.rs:336`) —
unlike a write or an erase, it never waits for a gap between radio
events. That single fact removes the RAM index the QSPI costing needed:
a lookup is a scan, and a scan is free of the radio.

The RAM it would have competed with, measured on the Pocket at the end
of the 12.69 h field run:

```text
[HEAP] used=58612 free=39692 watermark=58996 size=98304
```

96 KiB of heap, 58 996 B at the high-water mark, so **39 308 B of free
heap in the worst observed moment.** Against a 176-message region:

| | Bytes | Against 39 308 B free |
|---|---|---|
| A reference-shaped full index: 32 B key + 16 B destination + 4 B offset + 2 B size + 4 B timestamp + 1 B stamp = 59 B an entry | 10 384 | fits |
| The reference's per-message peer sets, 20 peers × 2 sets × 16 B a hash (`MAX_PEERS`, `LXMRouter.py:43`) | 112 640 | does not fit |

So the arithmetic that killed the RAM index on a 2 MB part no longer
kills it on 64 KiB: a full index of this region would fit in a quarter
of the free heap. It is still not worth having — the heap has other
claimants and the scan that replaces it is cheap — but the honest
statement is "unnecessary", not "impossible". What remains impossible is
the second row: the peer sets scale with peers, and we control neither
how many peer with us nor, therefore, that number.

**What a full scan costs.** `for_each`
(`leviculum-nrf/record-log/src/lib.rs:546`) walks every page, reads each
record's 42-byte header and then its body, because `probe_record`
(`leviculum-nrf/record-log/src/lib.rs:906`) checks the CRC over both. A
full region is therefore one pass over at most 64 KiB.

**This is arithmetic, not a measurement, and the assumption is stated:**
the read is a `memcpy` from mapped flash, so the work is the bit-serial
CRC-16 at eight shift-and-test steps a byte (`crc16_update`,
`leviculum-nrf/record-log/src/lib.rs:271`) — 524 288 steps for the whole
region, and at one to four cycles a step on the Cortex-M4 at 64 MHz that
is **8 to 33 ms**. The measured number is owed and nearly free, because
the mount already performs exactly this scan and reports what it found
(`count`, `leviculum-nrf/record-log/src/lib.rs:411`).

Either way the conclusion is the same and it is not close: a `/get` list
request costing tens of milliseconds of CPU, and nothing of the radio
scheduler, is not a design constraint. The on-flash directory is the
design.

### Draining it: LoRa and BLE

**LoRa.** Measured, from the field log: a telemetry message crossing a
hop is 291 bytes on the wire, split into 254 + 37 byte frames, and the
firmware reported `op=tx duration_ms=903..905` for 13 of the 15 such
transmissions in the run (SF8, BW 125 kHz, CR 4:5, 18-symbol preamble).
Our airtime model agrees: computed 543 ms for the 184-byte frame the
same log reports as `airtime_ms=544` (`airtime_ms_with_preamble`,
`leviculum-core/src/rnode.rs:909`; preamble from
`derive_preamble_symbols`, `leviculum-core/src/rnode.rs:839`, which
floors at 18, `LORA_PREAMBLE_SYMBOLS_MIN`,
`leviculum-core/src/rnode.rs:776`).

At 904 ms per message and the 10 % duty-cycle cap the firmware enforces
(`[LORA_AIRTIME_LOCK] lt=1000 lt_cap=10.00%` in the same run), the full
region is 176 × 904 ms = **2.7 minutes of pure airtime, 27 minutes of
wall clock** — with zero retransmissions, zero link setup and no other
traffic on the channel.

That reverses a conclusion the QSPI costing drew. A full 2 MB store was
13.9 h of wall clock at the duty cap, which is a museum; **a 64 KiB
store is drainable in half an hour.** In a walk-past it is still only
the delta between two nodes that moves, but the whole store is no longer
out of reach, and that makes the single-message upload path (§3) a
usable way for two boards to meet rather than a consolation prize.

**BLE.** The negotiated MTU is bounded by measurement rather than
assumed: the SoftDevice is configured with an ATT MTU ceiling of 256
(`CONN_GATT`, `leviculum-nrf/src/ble/mod.rs:764`), but the field log
shows a 183-byte packet fragmenting into 2 and a 275-byte packet also
into 2, which brackets the payload per fragment to 138 to 182 bytes and
the MTU to 146 to 190 — consistent with the 185 default
(`DEFAULT_MTU`, `leviculum-core/src/framing/ble.rs:89`;
`payload_per_fragment`, `leviculum-core/src/framing/ble.rs:107`). At
177 bytes per fragment a 304-byte message is 2 notifications, so the
full region is 352 notifications.

**The sustained notification rate is still not measured and this page
will not invent it.** The field run carried sparse traffic — the
tightest observed spacing is two packets in the same millisecond, which
is a burst, not a rate. The shape is all that can be said: at 10
notifications/s the full region is 35 seconds. BLE is not the binding
constraint on a store this size, and the measurement is owed rather than
critical. It is named in §4.

### What accepting a message costs in CPU

A propagation stamp is validated by expanding a 1 000-round workblock
from the transient ID and hashing it with the stamp
(`WORKBLOCK_EXPAND_ROUNDS_PN`, `LXStamper.py:13`; `stamp_workblock`,
`LXStamper.py:49`; `validate_pn_stamp`, `LXStamper.py:84`). Each round
is one SHA-256 over the salt input plus one HKDF-SHA256 producing 256
bytes: one extract HMAC and eight expand HMACs, four SHA-256
compressions each. **37 compressions per round, 37 000 for the
workblock, plus 4 000 for the final digest over the 250 KiB workblock:
41 000 SHA-256 compressions, 2.62 MB hashed, per message.**

Two things follow.

**The reference materialises the 250 KiB workblock in RAM. We do not
have to, and already do not.** `workblock_hasher`
(`leviculum-lxmf/src/stamp.rs:143`) streams the HKDF blocks straight
into the digest and keeps one 256-byte block. The RAM objection to
stamp validation is already solved in our tree; only the CPU cost
remains.

**At an advertised cost of 0 the cost is not incurred at all.** Our
validator short-circuits before the workblock when the cost is zero
(`validate_stamp`, `leviculum-lxmf/src/stamp.rs:198`), and the firmware
already runs this way for delivery stamps: the LXMF dependency is
pulled with default features off, so the node "advertises a zero stamp
cost and mines nothing" (`leviculum-nrf/Cargo.toml:31`).

The expensive case is *peering out* to a Python node, which requires
mining a key at that node's advertised peering cost, default 18, over a
25-round workblock (`WORKBLOCK_EXPAND_ROUNDS_PEERING`,
`LXStamper.py:14`; `generate_peering_key`, `LXMPeer.py:242`). With the
precomputed-digest-state trick our miner already uses, that is 925
compressions for the workblock plus about two per trial over 2^18
expected trials: **525 000 compressions, 33.6 MB hashed, once per peer,
and the result is persistable.** The reference's own miner rehashes the
6.4 KB workblock every trial and so hashes 1.7 GB for the same key;
this is a legitimate deviation under the deviation rule, since the
stamp produced is byte-identical.

Converting compressions to seconds needs a SHA-256 throughput on the
nRF52840 at 64 MHz that **we have not measured**. For orientation only,
at 20 / 40 / 60 cycles per byte the stamp validation is 0.8 / 1.6 /
2.5 s and the peering key is 10 / 21 / 32 s. The measurement is owed
(§4); the conclusion that survives any plausible value is that
per-message stamp validation at a nonzero cost is seconds of the only
core we have, and a peering key is a one-off we can afford.

### What we announce

Field 3 of the propagation announce, the per-transfer limit, is parsed
with `int()` (`propagation_transfer_limit`,
`reference/LXMF/LXMF/Handlers.py:61`), so the only values that exist on
the wire are whole kilobytes. The offering peer enforces it against
`lxm_size + 16` and reads a kilobyte as 1 000 bytes
(`propagation_transfer_limit`, `reference/LXMF/LXMF/LXMPeer.py:370`),
where `lxm_size` is the stored object — `lxmf_data` with the stamp
appended, which is exactly what our record body holds
(`propagation_entries`, `LXMRouter.py:2518`).

Our hard bound is one page: a record never straddles one, so a body
above `MAX_BODY` = 4 042 B cannot be stored at all. Against the
reference's arithmetic that bounds field 3:

| Announced field 3 | Largest `lxm_size` a peer will offer | Fits a page? |
|---|---|---|
| 3 | 2 984 B | yes, 1 058 B spare |
| **4** | **3 984 B** | **yes, 58 B spare** |
| 5 | 4 984 B | no — 942 B over |

**Announce 4.** It is the largest whole kilobyte whose worst case still
fits the page a record may not straddle, and it is thirteen times the
measured field median. Announcing the reference's 256 would be the
failure §1 names: a proof of acceptance the store cannot honour.

Field 4, the per-sync limit, bounds one resource rather than one
message, and its bound is the region. 176 messages is 53 504 B of body,
so a sync allowed to carry more than that laps the log inside a single
transfer and overwrites its own earlier records. **Announce 32** — about
a hundred median messages, well under a lap, and about three times what
five minutes of a LoRa walk-past can carry at the duty cap.

Field 5, the stamp cost, stays open: it is the one field whose right
value depends on a measurement we do not have (SHA-256 throughput on the
board, above), and both the cost and the reason for it belong in §4.

## 3. The options

Four, and the fourth is doing nothing on the board.

| | A. Full propagation node | B. Bounded node, honest limits | C. Courier for recently-seen peers | D. Nothing on the board; `lnsd` carries it |
|---|---|---|---|---|
| What Sideband sees | a normal propagation node | a normal propagation node with small limits | nothing; not a PN | the PC's node, if in range |
| Announces `lxmf.propagation` | yes | yes | no | n/a |
| RAM at capacity | 10 KiB index + 113 KiB of peer sets at 20 peers | an on-flash directory, scanned; no index | same as B | 0 |
| Peers | autopeer, up to 20 | autopeer, capped low | none | as configured |
| Stamp cost advertised | 16 | 0, or a low nonzero once measured | n/a | 16 |
| Two boards meet, no phone | works, if both can peer | **works** | works, but only between our own boards | **does not work** |
| Board switched off mid-transfer | client retries; nothing lost | client retries; nothing lost | our own protocol, our own problem | n/a |
| Verdict | impossible | viable | not compatible | insufficient |

**A, the full node, is still out, but the smaller region moved which
argument does it.** On the 2 MB costing the RAM index alone was eight
times the whole heap; on 176 messages it is 10 KiB against 39 KiB free,
so message count no longer decides anything. **Peer count does.** The
handled/unhandled sets are held per message as lists of peer hashes
(`propagation_entries`, `LXMRouter.py:2518`), which is 112 640 B at
176 messages and the reference's 20 peers, and there is no cap we
control on who peers with us: any Python router within four hops that
hears our announce peers automatically (`AUTOPEER_MAXDEPTH`,
`LXMRouter.py:45`; `LXMFPropagationAnnounceHandler`, `Handlers.py:35`).
A number we do not control is not a budget.

The count that is untouched by the region shrinking is the one that
actually kills A: every message we accept is enqueued for every peer
(`flush_peer_distribution_queue`, `LXMRouter.py:2472`), so a store
filled from a phone over BLE in seconds would be re-offered over a
10 %-duty LoRa link to everyone in range, at 904 ms a message. That is
not a tuning problem.

**B, the bounded node, is the only option that satisfies the framing.**
Everything it needs is already expressible in the announce: a small
field 3 and field 4 that peers and clients honour, a stamp cost we
choose, and a `max_peers` of our own. It costs the spam brake — a node
advertising cost 0 can be filled by anyone — which the small transfer
limit and the size cull bound but do not remove. It is the only option
where a Sideband user gets the thing they expect without knowing the
node is small.

**C, the courier, is out on compatibility, not on cost.** Holding
messages only for destinations we have recently seen is a good policy
and would fit the RAM budget comfortably. But there is no verb for it:
a node that does not announce `lxmf.propagation` is invisible to
Sideband, and a node that announces it and then behaves as a courier is
lying about field 2. C is a *policy inside B*, not an alternative to
it — and as a policy inside B it is exactly the right one for the
bounded case.

**D is what we do today and it is insufficient for the case that
prompted the issue.** Two boards on a hill with no PC in range have no
store between them.

### The case with no phone present

Explicitly, because it is the operator's case. Under B, two boards that
meet with no phone can exchange messages by two paths, and the cheap
one is worth naming:

- **Full peering.** Both announce as propagation nodes, autopeer within
  four hops, mine a peering key at each other's cost (which, since we
  choose our own, can be low between our own boards), and sync over a
  Link and a Resource. Correct, and bounded by the 10 % duty cycle: the
  delta, not the store.
- **Single-message client upload.** `propagation_packet`
  (`LXMRouter.py:2234`) accepts one message per link packet with no
  peering key at all. Two boards can hand each other one message at a
  time with no peering, no Resource, and no mining. For a walk-past on
  LoRa, where 904 ms of airtime per message is the real budget, this is
  the path that matches the medium.

Under A the same is true but the store re-offer makes it unusable.
Under C it works only between our own boards. Under D it does not work.

### Switched off mid-transfer

The protocol is safe against our disappearance at every point, and the
reason is worth recording because it constrains our implementation:

- **Mid-upload**, the client's packet is proven only after the message
  is stored (`packet.prove`, `LXMRouter.py:2255`). If we die first, the
  client gets no proof and retries. **This makes "persist before you
  prove" a rule, not a preference** — a proof written before the record
  is durable converts a power cut into a lost message.
- **Mid-`/get`**, the node deletes only on the client's explicit
  `haves` purge, and the client sends that purge only after local
  delivery (`message_get_response`, `LXMRouter.py:1607`). If we die
  during the transfer, nothing is deleted and the client repeats the
  exchange.
- **Mid-sync with a peer**, the offering peer marks nothing handled
  until the transfer concludes, and a failed request tears the link
  down and backs off (`request_failed`, `LXMPeer.py:395`).

The one thing that is *not* safe is a store whose own recovery is
unsound. A power cut in the middle of an append must leave a store that
reopens with every completed record and no partial one, and the log as
built delivers that with a one-word commit rather than with a
probability: a record counts as present only if its flags byte reads
live or purged, that byte sits in a single word programmed last, and a
word cannot be half-written (`FLAG_LIVE`,
`leviculum-nrf/record-log/src/lib.rs:238`). Any cut before that word
leaves the record invisible, deterministically; the CRC is then
catching a dropped bit rather than standing in for a commit protocol.

## 4. The recommendation

**Option B, and the store it needs now exists.** The region is 176
field-sized messages with 9 years of page-erase budget at the measured
field duty, provided nothing is ever written to a fixed page — a fixed
metadata page dies in 18.6 days, and the format has none. Reads never
touch the SoftDevice's flash scheduler, so a lookup is a scan of tens of
milliseconds and there is no RAM index to fit. A full region drains over
LoRa in half an hour of wall clock at the duty cap, which is the
difference between a store and a museum. None of those conclusions is
about LXMF; all of them are about the store, which is why the store came
first and is why it is worth having whether or not the propagation node
ever exists — `lnmsg`'s mailbox and telemetry retention want the same
16 pages.

What stands between here and the role is not arithmetic. It is three
things nobody has measured on a board: what an erase storm does to BLE
and LoRa while it runs, whether the region really survives a UF2, and
the two rates that decide the stamp cost and the phone drain.

### The sequence, as it now stands

1. **Store region and mount — done.** The log format was chosen against
   the internal flash in 59c36129 and given its region, its linker
   symbols and its boot-time mount in 81fcb46e. Nothing stores messages
   in it: no LXMF, nothing announced. The gap gate prints the remaining
   headroom for both bins on every `just fast`.
2. **The erase storm under BLE and LoRa load, on the rig — owed, and it
   is the next batch.** The instrument is in the firmware already
   (`STORE_STORM`, above). Acceptance: a phone connected over BLE and a
   LoRa link under traffic, each run twice — once with a storm of
   field-sized records and once without — reported as throughput and
   delivery rate with the event volumes on both sides, not as pass or
   fail. A storm that costs the radio nothing measurable and a storm
   that costs it everything are both results; a run that cannot tell
   them apart is not.
3. **UF2 survival, on a board — owed, and cheap.** Write records, flash
   a `.uf2` built from a different commit, remount, and assert the same
   record count and a byte-exact digest of the region. The bootloader
   source says it must survive (§2); this is the run that makes it
   established rather than expected. It belongs with the next firmware
   flash on the rig, not in a batch of its own.
4. **Then the role, on top of a store that has been measured.** The
   accept path with the limits this page recommends (field 3 = 4, field
   4 = 32), `/get` list and fetch answered from a scan rather than an
   index, `/offer` with a peer cap of our own, and no per-peer sets
   anywhere. Acceptance: a Sideband client and a Python `rnsd` both use
   the board as their propagation node without knowing it is small, and
   a power cut during an upload leaves a store that reopens with every
   completed record and no partial one.

Steps 2 and 3 are measurements, step 4 is the feature, and the order is
not negotiable: a propagation node that lands before the storm is
measured is a node whose failure mode is a radio that stutters when
somebody sends a message.

### The open questions, none of them closed by this page

- **Whether the region survives a UF2 on a board.** Argued from the
  bootloader's source, not yet run. Step 3 above.
- **What an erase storm costs BLE and LoRa.** The one number that could
  still make a propagation node on the board a bad idea. Step 2 above.
- **SHA-256 throughput on the nRF52840 at 64 MHz.** Decides whether we
  can advertise a nonzero stamp cost and keep the only spam brake the
  protocol has. At 20 / 40 / 60 cycles a byte, validating one stamp is
  0.8 / 1.6 / 2.5 s of the only core we have; the spread is too wide to
  decide on.
- **The sustained BLE notification rate.** Decides whether a phone can
  drain 352 notifications in a usable time. Expected to be comfortable,
  unmeasured.
- **The message-size distribution beyond telemetry.** Every body figure
  on this page comes from 284 telemetry messages. A run carrying real
  Sideband text, and a phone that sends an image, will move the median
  and will show how often field 3 actually bites.
- **Whether autopeering can be bounded in practice.** The reference
  peers with anyone within four hops (`AUTOPEER_MAXDEPTH`,
  `LXMRouter.py:45`). B assumes a cap we enforce ourselves keeps the
  peer-set arithmetic survivable; three Python routers in range would
  show whether it does.
- **Whether 16 pages is the right size.** 68 would buy 38 years and cost
  the image 212 KiB of headroom. The number lives in `memory.x` and the
  trade is argued there; changing it later is a reformat, which `mount`
  handles as an unformatted region.

### What this page corrected, and what it still owes

Codeberg #384 observed that a search of `leviculum-nrf` finds no QSPI.
It was right, and for a reason neither board file admitted at the time:
a vendor variant header's `EXTERNAL_FLASH_DEVICES` line was read as a
statement that a part is fitted, when on both vendors it is a template
default under a comment denying one. Neither board answered a JEDEC read
on any unit we own; both sets of pin aliases are gone and the reasons
are in the board files (`CONFIG`,
`leviculum-nrf/src/boards/t114.rs:167`; `CONFIG`,
`leviculum-nrf/src/boards/rak4631.rs:145`).

The larger correction was this page's own premise. It costed a store on
two parts that do not exist, and the recommendation rested on figures
that were an order of magnitude too generous in capacity and an order of
magnitude too generous in erase budget. The protocol half needed no
change, which is the useful lesson: the analysis that was about LXMF
survived the part being wrong, and everything that was about a
datasheet did not.

What it still owes is a board. Three of the numbers above are arithmetic
or datasheet figures — the scan time, the erase storm's cost, the UF2
survival — and a rig run replaces each of them with a measurement.

## 5. Peering: the design part 2 built

Peering is the core of the role — a node that does not peer is a
mailbox, not a mesh (Lead decision, 2026-09-11). This section was the
binding design for part 2 of leviculum#384 and is now updated to what
part 2 built: first what the reference actually keeps and exchanges,
measured against the pinned tree (795fdaa), then our design inside this
page's constraints, with every number re-derived from the code as
landed. The protocol half is `leviculum-lxmf/src/peering.rs`
(`no_std + alloc`, behind the `PeerStore` trait the board implements in
part 3); the host glue is `lnpnd/src/peering.rs`.

### What the reference keeps, per peer and per message

`LXMPeer.to_bytes` (`LXMPeer.py:138-175`) persists, per peer: the
destination hash, the peering key and its value, the peering timebase,
alive flag, last-heard, sync strategy, metadata, the announced transfer
and sync limits, the announced stamp cost, flexibility and peering
cost, the last sync attempt, and six statistics counters (link
establishment rate, sync transfer rate, offered / outgoing / incoming,
rx/tx bytes) — plus the two sets the §1 analysis flagged:
`handled_ids` and `unhandled_ids`. Those two are not stored on the
peer at all at runtime: they live *per message*, as lists of peer
hashes in `propagation_entries[4]` and `[5]`
(`LXMRouter.py:2518`; membership filtered per peer in
`LXMPeer.handled_messages`, `LXMPeer.py:574-588`), 16 bytes per peer
per message, filled by `flush_peer_distribution_queue`
(`LXMRouter.py:2472`) which enqueues every accepted message for every
peer. At this store's 176 messages and the reference's 20-peer default
that is the 112 640 B that §2 measured against 39 308 B of free heap:
the one reference structure we cannot carry.

A sync round exchanges three things (`LXMPeer.sync`,
`LXMPeer.py:267-390`): a `/offer` request carrying
`[peering_key, [transient_id, …]]` with the ids filtered by the peer's
minimum stamp value and packed under its announced limits
(`:334-385`); the response `True` / `False` / wanted-sublist
(`offer_request`, `LXMRouter.py:2266-2329`, which answers out of its
own `propagation_entries` membership); then one Reticulum Resource
whose body is `msgpack([timestamp, [lxmf_data ‖ stamp, …]])`
(`:457-468`). Only on the concluded transfer are the sent ids moved
handled (`resource_concluded`, `LXMPeer.py:492-517`); ids the peer
declined were moved handled already at the response (`offer_response`,
`LXMPeer.py:443-448`). An id purged from the store before its offer is
silently dropped at the next sync (`:348-352`) — forgetting needs no
verb between peers either.

### Our peer record, and the cap

Per peer we keep what is wire-visible plus the minimum liveness state,
and nothing statistical. As built (`Peer` / `PeerRecord`,
`leviculum-lxmf/src/peering.rs`), the packed persistable record weighs:

| Field | Bytes |
|---|---|
| destination hash | 16 |
| identity hash (the peering-key material's first half, `LXMPeer.py:258`) | 16 |
| peering key + value | 34 |
| announced limits (transfer, sync) | 8 |
| announced costs (stamp, flexibility, peering) | 3 |
| peering timebase, last heard | 12 |
| cursor into the store sequence | 8 |
| static flag | 1 |
| **per peer** | **98**, call it 104 aligned |

The design's 80 grew to ~104 in the build: the identity hash joined
the record (mining material must survive a restart or the key is
useless), and the cursor widened to the u64 the host store's sequence
uses (the board packs the same pair into 6 bytes, below). Re-derived
against the same budget: during one sync (one at a time on the board,
`lnpnd/src/peering.rs` holds one round in flight) the offer list is
bounded at **6 144 B** (`OFFER_BYTES_LIMIT`) — 34 B per encoded id, so
at most 179 ids per round, re-offering the rest next round. Against
§2's worst observed free heap of 39 308 B, the same 8 KiB peering
slice gives `104·N + 6 144 ≤ 8 192`, N ≤ 19. **Board cap: 16 peers**
still holds, now with less margin; **host config default: 20**, the
reference's own `MAX_PEERS` (`LXMRouter.py:43`), settable as
`max_peers`. The full-table policy is deterministic and documented on
`DeclineReason::TableFull`: first heard wins, a full table declines
new candidates (the reference's own behaviour, `LXMRouter.py:2032`),
and slots free only by the 14-day unreachability cull
(`MAX_UNREACHABLE`, `LXMPeer.py:39`), an unpeer, or the peer leaving
the role.

The host persists its table in one msgpack file
(`FilePeerStore`, `leviculum-std/src/file_peer_store.rs`), as the
reference does (`LXMRouter.py:599-631`). The board's `PeerStore`
implementation is part 3's: the trait demands only upsert-by-key
(append a new tagged record, purge the old), full-scan load, and
nothing rewritten in place — no fixed page, which is §2's endurance
rule. Mined peering keys ride the same record, so the grind happens
once per peer, not per reboot.

### The cursor, instead of per-peer sets

The record log is append-ordered: pages carry a monotone sequence
written once per erase (`SECTOR_HEADER_LEN` header,
`leviculum-nrf/record-log/src/lib.rs:220`), records within a page are
ordered by offset. **A store position is therefore the pair
`(page_sequence: u32, offset: u16)`, and each peer holds one cursor:
everything at or below it has been offered and concluded.** As built,
the store trait carries this as `StoredMessage::sequence: u64`
(`leviculum-lxmf/src/propagation_store.rs`): the board maps
`page_sequence << 16 | offset` into it, the host store assigns a
monotone append counter persisted in its file names
(`leviculum-std/src/file_propagation_store.rs`), so cursors survive a
host restart too. A sync offers every live id newer than the cursor
(one `for_each` scan, §2 prices it at 8-33 ms; `build_offer`,
`leviculum-lxmf/src/peering.rs`); on the concluded transfer — or on a
"want none" response — the cursor advances to the plan's target
(`resource_concluded` is the reference's own only-on-conclusion rule,
`LXMPeer.py:492-517`). That replaces both per-peer sets with one
integer per peer, and it cannot lose messages: a message is either at
or below a concluded cursor (offered once), evicted (absent
everywhere, the reference's own behaviour at `:348-352`), or ahead of
the cursor (offered next round).

Three cursor semantics the build pinned down, tested in
`leviculum-lxmf/src/peering_tests.rs`:

- **Permanent skips advance the cursor.** An entry whose stamp value
  is below the peer's minimum (`LXMPeer.py:340`) or whose size exceeds
  the peer's per-message limit (`:370-373`) is stepped past for good —
  exactly the ids the reference marks handled without sending.
- **Resumable stops do not.** The peer's per-sync limit and the
  6 144 B offer bound end the round *without* advancing past what they
  excluded; the walk is in append order, so nothing above the target
  was withheld for a resumable reason. (The reference offers
  weight-sorted and keeps scanning past a sync-limit hit; ours stops
  there — a selection-order deviation with no wire effect, and the
  property that lets a single integer replace the sets.)
- **A stale cursor is a bounded full re-offer.** A cursor naming a
  reclaimed page (board) or a reset store (host) orders below
  everything live, so the next round re-offers everything — ≤ 6 KiB of
  ids — and the peer answers "want none" for what it holds. The
  conformance cells drive this path explicitly (`lxmf_pn_reoffer`).

What a Python peer observes: offers that may include ids it already
holds — including messages it itself sent us, since a cursor cannot
encode the reference's `from_peer` exclusion
(`flush_peer_distribution_queue`, `LXMRouter.py:2484`). That is
wire-legal and self-limiting: `offer_request` answers out of its own
store membership (`:2317-2318`) and declines them, and the cost is
offer-list bytes, not message bodies. The round-robin page reclaim
also means a cursor's page can be erased and reused while the cursor
still names the old sequence; page sequences are monotone, so a
cursor pointing into a reclaimed page simply reads as "older than
everything live" and the next offer is a full offer — the reboot case
again, bounded the same way.

**One accept-path consequence, decided in the design and built as
decided:** part 1 stored stamp value 0 for messages accepted at cost 0
(the validator short-circuits, where the reference computes the true
value even at cost 0, `LXStamper.py:95`). The offering side drops ids
whose stored value is below the *peer's* minimum (`LXMPeer.py:340`),
and a default Python peer's minimum is 16 − 3 = 13, so a store full of
value-0 records would offer that peer nothing. Part 2 therefore
computes the true stamp value at accept time whenever any known peer
requires more than 0 (`PropagationNode::set_compute_stamp_value`,
driven from the peer table; the measuring validator is
`CooperativeStamper::measure_stamp`) — 41 000 SHA-256 compressions per
message, §2's orientation says 0.8-2.5 s on the board, free on the
host — and keeps the shortcut otherwise. The record tag is written
once at append, so the decision is per-message at accept, not
retrofittable; a store accepted cheap stays cheap until it turns over
(at most 30 days). Note the practical consequence the chain cell ran
into: a true value of a *free* stamp is small (geometric, expected ~1
bit), so computing it honestly does not make a cost-0 store
propagatable through a default stock node — a node that wants its
store to travel through default peers must announce a stamp cost whose
minimum clears theirs (the cells use 16).

### Peering *with* a Python node: the price of its key

Outbound peering requires mining a key at the peer's announced peering
cost over the 25-round peering workblock
(`WORKBLOCK_EXPAND_ROUNDS_PEERING`, `LXStamper.py:14`;
`generate_peering_key`, `LXMPeer.py:242-265`). The reference announces
18 by default and accepts configuration up to 26 (`PEERING_COST`,
`MAX_PEERING_COST`, `LXMRouter.py:50-51`). With our precomputed-
digest-state miner (§2): 925 compressions for the workblock plus ~2
per trial, expected 2^cost trials —

| Peer's cost | Compressions | On the board (at §2's 20/40/60 cycles/byte orientation) |
|---|---|---|
| 18 | ~5.3 × 10^5 | 10 s / 21 s / 32 s |
| 26 | ~1.3 × 10^8 | **45 min / 89 min / 134 min** |

One-off per peer and persistable — a cost-26 Python neighbour would
otherwise cost the better part of an hour of the board's single core
*per reboot*. Part 2 therefore persists the mined key inside the peer
record itself (the `PeerStore` boundary above): on the host that is
the peer file, on the board a ~104 B tagged record — append-only, no
fixed page, a negligible tenant of the region. The host mines on a
worker thread, as the reference does (`LXMPeer.py:285-286`), never
under the core lock; costs above `remote_peering_cost_max` (default
26, `MAX_PEERING_COST`, `LXMRouter.py:51`) are refused at the table,
so the grind is bounded by configuration. The SHA-256 throughput
figure that pins this table's real column is §4's owed measurement,
still owed here.

Our own announced peering cost defaults to 0, the same policy as the
stamp cost and this time without even a reference counter-argument:
the `PROPAGATION_COST_MIN` clamp applies to the propagation cost only
(`LXMRouter.py:137`); the peering cost is passed through unclamped, so
0 is a value the reference itself can be configured to and validates
trivially (`validate_peering_key` with target 0 accepts any key,
`LXStamper.py:73-82`).

Two falsy-zero quirks of announcing cost 0, both observed against the
reference and both ours to route around:

- **Client side** (part 1's interop run): `get_outbound_propagation_cost`
  treats 0 as falsy (`LXMRouter.py:429`), re-requests the path, logs
  "stamp cost still unavailable" — and then proceeds correctly, mining
  a free stamp and uploading. Cost 0 is honoured on the wire; the
  reference client just grumbles first.
- **Peer side, and this one is a dead end**: `LXMPeer.peering_key_ready`
  short-circuits false on a falsy peering cost (`LXMPeer.py:228`), so a
  stock node's sync toward a cost-0 peer postpones forever on "peering
  key has not been generated yet" — the key IS generated, the readiness
  check just never looks at it. **A stock lxmd can never sync toward a
  node announcing peering cost 0.** Our own outbound side deviates
  (any key is ready at cost 0, `Peer::peering_key_ready`,
  `leviculum-lxmf/src/peering.rs` — wire format untouched, the
  validator side accepts any key at target 0, `LXStamper.py:79-82`),
  so rust-to-rust peering at 0 works; a node that wants *stock* peers
  to sync to it announces at least 1, which is what the conformance
  chain cells do and why. Upstream is not told (standing policy);
  the workaround is a one-bit cost.
