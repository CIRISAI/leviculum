# The USB control envelope

The LNode's transport CDC carries HDLC-framed Reticulum packets, plus a
small out-of-band control plane between an attached host (`lnflash`,
`lnsd`) and the firmware. Until Codeberg #238 that control plane was one
hand-cut magic per feature — a radio-config frame and a reset frame, each
recognised by shape. Three pending features each wanted a third magic,
which is how a channel becomes unextendable. This page documents the one
envelope every control frame rides in now, and how the two legacy magics
retire.

Wire truth lives in `leviculum-core/src/envelope.rs`; this page explains
it. If they disagree, the code and its tests win.

## Frame layout

One envelope per HDLC frame:

```text
[0xA4, 0xA5] [type: u8] [len: u16 BE] [payload: len bytes]
```

The length is strict: a frame whose payload is shorter or longer than
`len` is malformed. A reader that knows the envelope but not the type
answers a named refusal and stays in sync — the HDLC delimiter bounds the
frame, the header names what was skipped. Nothing envelope-shaped is ever
answered with silence; the legacy magics predate that rule and keep their
old manners (below).

## Frame types

Commands (host → board):

| type | name             | payload                                        |
|------|------------------|------------------------------------------------|
| 0x01 | RADIO_CONFIG     | the legacy frame's parameter block (13–19 B), no magic |
| 0x02 | RESET            | empty                                          |
| 0x03 | WALL_TIME        | unix seconds, u64 BE (8 B)                     |
| 0x04 | CAPABILITIES     | empty (a query)                                |
| 0x05 | TELEMETRY_TARGET | see below — set or clear the telemetry target  |
| 0x06 | TX_SPACING       | on-air transmit spacing in ms, u16 BE (2 B)    |

Responses (board → host):

| type | name              | payload                                   |
|------|-------------------|-------------------------------------------|
| 0x81 | ACK               | `[acked_type]`                            |
| 0x82 | REFUSAL           | `[refused_type, reason]`                  |
| 0x83 | CAPABILITY_REPORT | `[version, accepted types...]`            |

Refusal reasons: `0x01` unknown type, `0x02` malformed, `0x03` value
refused, `0x04` busy. The version in the capability report (`1`) names
the envelope framing itself; new frame types extend the accepted list
without bumping it.

The wall-time frame calls the calendar seam
(`set_wall_time_unix_secs(.., TimeSource::Host)`); the seam's sanity
window decides between the ack and a `value refused` refusal, and an
accepted seed logs `[TIME_SEED] source=host` and flips the banner's
`[TIME_SOURCE]` to `host` — the exact mirror of the GNSS path.

### The transmit-spacing frame (#345)

```text
[spacing_ms: u16 BE]
```

The gap the board's LoRa interface leaves between the end of one packet's
airtime and the key-up of the next. It is applied inside
`transmit_all_frames`, the last thing before the radio is keyed, so it is a
gap between two packets on the air rather than between two hand-overs, and
whatever the transmit path already spent since the previous packet ended
(the CAD, the SPI traffic, the log lines) is counted against the requested
gap rather than added to it. The split frames of one packet are unaffected:
they still go out back-to-back, because the receiver's reassembler requires
that.

Every u16 value is legal, `0` included — `0` is the compiled default and
imposes nothing, so the only malformed frame is one of the wrong length.
The value is **not persisted**: it is a measurement instrument (the sweep of
the telemetry announce/report spacing, #345), and a reset returns the board
to the default. The board logs `[LORA_TX_SPACING] intended_ms=… waited_ms=…
gap_ms=…` at every key-up; `gap_ms` is the gap that was measured, and `-1`
is the first packet since boot, which has no previous airtime edge to be
measured from.

`lnflash --set-tx-spacing <MS>` is the host side.

### The telemetry-target frame (#236)

```text
[profile: u8] [dest_hash: 16] [key_present: u8] ([public_key: 64])
```

`key_present` is `0x00` or `0x01`, never inferred from the length: per
the #236 UX decisions (2026-08-22) the public key is optional and
hash-only is the common case — the user knows the LXMF address, the node
resolves the key over the air.

Profile ids:

| id   | name    | meaning                                              |
|------|---------|------------------------------------------------------|
| 0x00 | OFF     | **clear the target** — telemetry off                 |
| 0x01 | TRACKER | movement-driven cadence                              |
| 0x02 | STATION | slow heartbeat only; the default profile             |

`0x00` is the clear encoding. It rides in the profile slot rather than
in a magic destination hash because that slot's whole job is to say
which cadence applies, and "none" belongs in its vocabulary; the rest of
the payload is still parsed and must still be well formed, so a clear
frame is not a licence to send a short one. The destination hash and key
of a clear frame are ignored, and `encode_telemetry_clear` zeroes them
rather than echoing a target back for no reason.

An id the firmware does not know is **not** a refusal: the destination
is kept and the default profile's cadence runs, because a newer host's
cadence preference is not worth losing a configured target over. Which
profile is actually running is in the board's `[TELEMETRY]` banner.

Firmware from before #236 answers this type with an `unknown type`
refusal and leaves it out of its capability report, which is precisely
how a #236-aware host detects a pre-#236 board.

#### How `lnflash` drives it

Telemetry is configuration, not firmware, so the same frame is reachable
from the flash flow and without flashing anything:

| flag                                | effect                                                        |
|-------------------------------------|---------------------------------------------------------------|
| *(none)*                            | after the radio step: `Send telemetry? [y/N]`, default **no**  |
| `--telemetry <ADDRESS>`             | implies yes; 32 hex chars, spaces/colons/case tolerated        |
| `--telemetry-profile <tracker\|station>` | which cadence; default `station`                         |
| `--telemetry-key <128 hex>`         | the key-present form; absent = hash-only, the common case      |
| `--no-telemetry`                    | send profile `0x00` — clear whatever the board had stored      |
| `--set-telemetry`                   | the same configuration on running boards, no flash             |

Answering *no* at the prompt sends **nothing**; `--no-telemetry` sends a
clear frame. The difference matters on a board that already has a target:
silence leaves it, the clear frame removes it.

A yes needs exactly one input — the LXMF address — because that is what
users have. Nothing detects a terminal: `Ui::ask` answers "no answer" for
`--yes` and for a piped or closed stdin alike, and every prompt treats
that as its stated default, so a scripted run cannot block.

What the host reports back is the ack. The node's own
`[TELEMETRY] target=… state=off|awaiting-key|ready` line goes to the
debug CDC (if00), which `lnflash` holds open only for the post-flash boot
check — so it is named as the place to read the rest rather than read
back over a second connection.

## Why an envelope frame can never be a packet

The channel's other occupant is HDLC-framed Reticulum traffic, so every
control frame must be unmistakable. Three facts hold it:

1. The first magic byte `0xA4` has the IFAC bit set, and this channel
   runs without IFAC — no peer on it emits a packet whose first byte
   matches, and firmware from before the envelope drops a received
   envelope frame in packet parsing for the same reason.
2. Every frame a host may send *before* it knows the peer speaks the
   envelope — the capability probe, wall time, reset — is shorter than
   the 19-byte minimum Reticulum wire packet, so it cannot be
   packet-shaped at all.
3. Frames longer than that (radio config at 24 B, telemetry target at up
   to 87 B) are only sent after a capability report proved the peer is
   envelope-speaking firmware. This ordering is load-bearing: an
   envelope speaker must probe before it sends any envelope frame of 19
   bytes or more.

## Compatibility window, and how it retires

The two legacy magics stay accepted, with their legacy answers, so both
field directions keep working:

- **Old host tool → new firmware:** the legacy 21-byte config magic and
  the 4-byte reset magic are classified ahead of the envelope
  (`classify_control_frame`) and answered with the legacy two-byte-style
  acks (`RADIO_CONFIG_ACK`, `RADIO_RESET_ACK`). An invalid legacy config
  keeps its historical silence; audible refusals begin with the envelope.
- **New host tool → old firmware:** `lnflash` opens every control
  conversation with a capability probe. Firmware that answers gets
  envelope frames; firmware that stays silent (pre-envelope) gets the
  legacy config magic as a fallback, and `--set-time` reports "this
  firmware predates the control envelope" by name instead of guessing.

`lnsd` still speaks the legacy config magic on every connect; it migrates
to the envelope in its own batch.

Retirement happens in that order: first `lnsd` and every shipped host
tool speak the envelope (probing, with fallback), then — after a release
cycle in which lnflash bundles only envelope-speaking firmware, so any
field board a current tool meets accepts it — the firmware drops the two
legacy classifier arms and the host tools drop the fallback. Each step is
observable: a host that still needs the fallback logs it, and a board
that still receives legacy magics is running firmware older than the
bundle that introduced the envelope.

## Adding a fourth frame type

The definition of done for #238: allocate the next type constant in
`leviculum-core/src/envelope.rs`, give it a payload codec with tests, add
a `ControlAction` variant and its executor arm in
`leviculum-nrf/src/usb.rs`, and append the type to
`ACCEPTED_CONTROL_TYPES` so the capability report advertises it. The
framing, the refusal path, the probe, and both host speakers stay
untouched.
