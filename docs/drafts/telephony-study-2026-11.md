# Telephony over Reticulum: a study before any code

Status: draft for Lew, order 1257, 2026-11-01. Refs #232. Study only; no
product code is changed by this page. Lew's brief (2026-10-31): calls over
Reticulum's own protocol (LXST), as simple as possible for the user, as few
hops as possible, a direct connection for latency; investigate first.

References read: LXST 0.4.5 at `reference/LXST` (commit 1194c901, the
submodule), LXST 0.5.4 and Sideband (`sbapp`) 2.2.0 as downloaded from PyPI
on 2026-11-01 (outside the tree, read only), RNS 1.3.5 at
`reference/Reticulum`, LXMF at `reference/LXMF`, leviculum at 076ead82.
Paths into the two downloads are given as `lxst-0.5.4:LXST/...` and
`sbapp-2.2.0:sbapp/...`. Figures marked [DERIVED] are worked out here;
[MEASURED] were run on schneckenschreck for this page (method in the
appendix); [UNVERIFIED] are facts this pass could not check against code.

Context pages: [reaching the mailbox](../src/concepts/loquax-reachability.md)
(1195), [the home node](../src/concepts/loquax-home-node.md),
[phone power, tier P](../src/concepts/loquax-phone-power.md) (1177). The
order names a phone node page, `loquax-phone-node.md` (1194); no such file
exists at 076ead82, so this page leans on the three above.

## 0. Verdict

- **LXST is small and fully readable**: a link to `lxst.telephony` of the
  callee's identity, msgpack packets `{0: [signals]}` and `{1: frame}` on
  it, a one-byte codec header per frame, seven status codes. A Rust
  implementation that speaks it is a few hundred lines on top of our links.
  Its licence is CC BY-NC-ND 4.0, so we write it from the protocol, never
  from the code.
- **IP carries a call easily, LoRa at our defaults does not.** Each lnsd
  relay adds about 0.1 ms on loopback [MEASURED]; a call through our own
  rendezvous `leviculum.network` measured 31 ms frame round trip, two IP
  round trips of 15.5 ms [MEASURED]. On SF8/125 kHz a full-duplex Codec2
  call needs 1.9 to 4.9 times the channel [DERIVED], and the 10 % duty
  cycle of the 869.4-869.65 MHz band caps one direction at a tenth of it.
  LoRa gets voice messages, not calls.
- **The real design problem is the identity, not the latency.** Sideband
  dials `lxst.telephony` of the contact's LXMF identity
  (`sbapp-2.2.0:sbapp/sideband/voice.py:227`). A Loquax phone does not hold
  that identity; only the home node does
  ([the home node](../src/concepts/loquax-home-node.md), "Only the home
  node holds the LXMF identity"). So either the home node answers calls
  and bridges them to the phone, or Sideband users cannot call a Loquax
  phone. Section 5 asks Lew to choose.
- **Recommendation:** build Loquax-to-anyone calls over IP first, with the
  home node as the call endpoint that bridges to the admitted phone, Opus
  "Medium Quality" as the default, the same LXST framing so Sideband
  interoperates, and a voice message (1222) whenever no IP path exists.

## 1. What LXST is

### Licence

`CC BY-NC-ND 4.0` (`reference/LXST/README.md:44`; the 0.5.4 wheel carries
the same text, `lxst-0.5.4.dist-info/licenses/`, classified "Other/
Proprietary License"). NoDerivatives forbids a port of the code; a wire-
compatible implementation written from the protocol description below is
the only route for an AGPL project. Section 5 lists it as a decision.

### The link and the destination

- **Destination**: `SINGLE`, aspects `lxst.telephony` (`APP_NAME`,
  `reference/LXST/LXST/__init__.py:1`; `PRIMITIVE_NAME`,
  `reference/LXST/LXST/Primitives/Telephony.py:17`), proof strategy
  `PROVE_NONE` (`Telephony.py:142`). Sideband creates it with the same
  identity it registers for LXMF delivery
  (`sbapp-2.2.0:sbapp/sideband/core.py:5855` and `:4777`), so the call
  address of a contact follows from the LXMF identity alone:
  `hash_from_name_and_identity("lxst.telephony", identity_hash)`
  (`sbapp-2.2.0:sbapp/sideband/voice.py:227`).
- **Announce**: every 3 h (`ANNOUNCE_INTERVAL`, `Telephony.py:122`), no app
  data. Sideband announces it together with its LXMF announce
  (`sbapp-2.2.0:sbapp/sideband/core.py:4094`). Voice is off by default in
  Sideband (`core.py:617`).
- **Transport**: one ordinary Reticulum link, initiated by the caller; the
  caller identifies itself on the link (`LINKIDENTIFY`,
  `reference/Reticulum/RNS/Link.py:459`). Every LXST packet is a plain link
  DATA packet without a receipt (`reference/LXST/LXST/Network.py:29`,
  `:64`). No resources, no channels, no retransmission: a lost frame is
  lost.

### Framing

Two msgpack map keys (`Network.py:10-11`):

| Key | Value | Sent by |
|---|---|---|
| `0x00` `FIELD_SIGNALLING` | list of unsigned ints (0.4.5 sends a one-element list, `Network.py:26`; 0.5.4 may send several, `lxst-0.5.4:LXST/Network.py:28-29`) | `SignallingReceiver.signal` |
| `0x01` `FIELD_FRAMES` | bytes: one codec header byte, then the codec's frame (`Network.py:62-64`); a receiver also accepts a list of such (`Network.py:116`) | `Packetizer.handle_frame` |

Codec header byte (`reference/LXST/LXST/Codecs/__init__.py:8-11`):
`0x00` raw, `0x01` Opus, `0x02` Codec2, `0xFF` null. A Codec2 frame
carries a second header byte, its mode (`Codec2.py:25-31`, `:85`): `0x00`
700C, `0x01` 1200, `0x02` 1300, `0x03` 1400, `0x04` 1600, `0x05` 2400,
`0x06` 3200, followed by as many Codec2 frames as the frame time holds.
An Opus frame is the raw Opus packet, no container. The receiver switches
decoder whenever the header byte changes (`Network.py:118-124`), so either
side may change codec mid-call; each side picks its own transmit codec.

The wire shape of one voice packet, outermost first [DERIVED]:

```
19 B  link packet header (2 header, 16 link id, 1 context)
16 B  token IV
n  B  AES-CBC ciphertext, PKCS7, of:
        0x81 0x01 0xc4 <len>          msgpack {1: bin8}
        <codec header> [<c2 mode>] <codec frame(s)>
32 B  token HMAC
```

The calculator Mark ships (`reference/LXST/docs/rns_audio_call_calc.py`)
uses the same 19 B header and 48 B token overhead.

### Codecs and profiles

The telephony primitive offers eight profiles (`Telephony.py:19-94`); the
default is `QUALITY_MEDIUM` (`Telephony.py:29`). Packet size is the Opus
byte ceiling (`Opus.py:83-97`) or the Codec2 rate times the frame time,
then the shape above [DERIVED]:

| Profile (id) | Codec | Frame | Codec bytes | Packet | Packets/s | Wire, one way |
|---|---|---|---|---|---|---|
| ULBW `0x10` | Codec2 700C | 400 ms | 1+40 | 115 B | 2.5 | 2.3 kbit/s |
| VLBW `0x20` | Codec2 1600 | 320 ms | 1+64 | 147 B | 3.1 | 3.7 kbit/s |
| LBW `0x30` | Codec2 3200 | 200 ms | 1+80 | 163 B | 5 | 6.5 kbit/s |
| MQ `0x40` (default) | Opus voice, 24 kHz, ≤8 kbit/s | 60 ms | ≤60 | 147 B | 16.7 | 19.6 kbit/s |
| HQ `0x50` | Opus voice, 48 kHz, ≤16 kbit/s | 60 ms | ≤120 | 195 B | 16.7 | 26 kbit/s |
| SHQ `0x60` | Opus voice, stereo, ≤32 kbit/s | 60 ms | ≤240 | 323 B | 16.7 | 43 kbit/s |
| LL `0x80` | Opus as MQ | 20 ms | ≤20 | 99 B | 50 | 40 kbit/s |
| ULL `0x70` | Opus as MQ | 10 ms | ≤10 | 83 B | 100 | 66 kbit/s |

Codec2 bytes per codec frame (8 B per 20 ms at 3200, 6 B per 20 ms at
2400, 8 B per 40 ms at 1600, 6 B per 40 ms at 1200, 4 B per 40 ms at
700C) follow from the mode rates and Codec2's frame lengths [UNVERIFIED:
`pycodec2` is not installed here; `Codec2.py:69` and `:97` read them from
the library at run time]. The overhead dominates: at MQ, 60 B of voice
ride in 147 B, so a "6-8 kbit/s" codec costs 19.6 kbit/s each way.
Both ends filter the microphone (band pass 250-8500 Hz, AGC,
`Telephony.py:608`), and the receive mixer holds at most two frames
(`Telephony.py:622`), the whole jitter buffer.

The README claims latencies "below 10 milliseconds" and voice from
700 bit/s to 3200 bit/s with Codec2 and from about 4.5 kbit/s with Opus
(`reference/LXST/README.md:16-31`). The 10 ms is the ULL frame time, not a
mouth-to-ear figure.

### Signalling: offer, accept, end

Status codes (`Telephony.py:102-112`): `0x00` BUSY, `0x01` REJECTED,
`0x02` CALLING, `0x03` AVAILABLE, `0x04` RINGING, `0x05` CONNECTING,
`0x06` ESTABLISHED. A signal `0xFF + p` asks for profile `p`
(`PREFERRED_PROFILE`, so `0x13F` for MQ, a msgpack uint16); 0.5.4 adds
`0xF0 + m` for the mode, `0xF1` full duplex and `0xF2` half duplex
(`lxst-0.5.4:LXST/Primitives/Telephony.py:144-145`; half duplex squelches
the transmitter, push to talk).

```
caller                                   callee
  path request if no path (wait ≤70 s)
  LINKREQUEST ───────────────────────────▶
  ◀─────────────────────────────── LRPROOF        link up
  ◀────────────────── {0:[AVAILABLE]}             or BUSY + teardown
  LINKIDENTIFY ──────────────────────────▶       caller's identity
  ◀──────────────────── {0:[RINGING]}             or BUSY if not allowed
  {0:[0xFF+profile, 0xF0+mode]} ─────────▶       caller's choice
                 ... ringing, ≤60 s ...
  ◀────────────────── {0:[CONNECTING]}            user answered
  ◀───────────────── {0:[ESTABLISHED]}
  {1:frame} ◀──────────────────────────▶ {1:frame}
  hang up: link teardown (either side); a callee that declines while
  ringing sends {0:[REJECTED]} first
```

Sources: `__incoming_link_established` (`Telephony.py:318`),
`__caller_identified` (`:336`), `answer` (`:404`), `hangup` (`:425`),
`call` (`:648`), `signalling_received` (`:683`). Timers on the caller: the
whole call attempt 70 s (`WAIT_TIME`), the callee must be RINGING within
5 s of the call starting (`CONNECT_TIME`, `Telephony.py:117`, checked by
`__timeout_outgoing_establishment_at`, `:309`). The 5 s count from before
the path wait, so a caller that had to ask for a path for more than 5 s
gives up as soon as the link exists. Sideband shortens ringing to 30 s and
the wait to 60 s (`sbapp-2.2.0:sbapp/sideband/voice.py:25-26`) and refuses
to dial without a path (`voice.py:228-236`).

0.4.5 to 0.5.4 changed no framing: signals may come in a list, the
packetizer can be squelched, and the mode signal is new. 0.5.4's
receiver tests `>= 0xFF` before `>= 0xF0` (`lxst-0.5.4:LXST/Primitives/
Telephony.py:905-909`), so the two ranges do not collide.

### What Sideband's "Voice" is

A thin wrapper (`ReticulumTelephone`, `sbapp-2.2.0:sbapp/sideband/voice.py:14`)
around the LXST `Telephone` primitive, with a call log and a ringtone. It
dials by identity hash, shows the hop count before connecting
(`voice.py:229-231`), and passes profile and mode through. Its voice
*messages* are a separate thing: LXMF `FIELD_AUDIO` (`0x07`,
`reference/LXMF/LXMF/LXMF.py:14`) with the audio modes at `LXMF.py:62-89`,
a store-and-forward attachment that any path, the mailbox included,
carries.

## 2. Which paths can carry a call

### IP: measured

A Python-RNS 1.3.5 echo endpoint and a caller (both from
`reference/Reticulum`, no shared instance, no transport) open a link
through 0 to 3 lnsd relays on 127.0.0.1, then send 200 packets of 64 B
every 60 ms (the MQ cadence) and time each echo. The WAN row runs both
ends against `leviculum.network:4242`, the rendezvous of
[reaching the mailbox](../src/concepts/loquax-reachability.md), way B.
lnsd is the release build of 076ead82. [MEASURED, 2026-11-01]

| Path | Hops | Link setup | Link RTT | Frame RTT p50 / p95 / max |
|---|---|---|---|---|
| TCP direct, no relay | 1 | 1.1 ms | 0.61 ms | 0.55 / 0.76 / 3.69 ms |
| one lnsd relay | 2 | 1.4 ms | 0.90 ms | 0.69 / 0.92 / 4.18 ms |
| two lnsd relays | 3 | 1.7 ms | 1.11 ms | 0.82 / 1.01 / 2.27 ms |
| three lnsd relays | 4 | 4.3 ms | 3.68 ms | 0.87 / 1.15 / 1.49 ms |
| two relays, 500 × 120 B every 20 ms | 3 | 1.6 ms | 1.04 ms | 0.60 / 0.80 / 1.79 ms |
| rendezvous `leviculum.network` | 2 | 38.3 ms | 37.2 ms | 31.2 / 37.3 / 65.8 ms |

Every run echoed all its packets. `ping leviculum.network` from the same
host: 15.5 ms average over 20. A relay costs about 0.1 ms of forwarding;
the rendezvous row is two IP round trips (caller to node, node to callee,
here the same host) and nothing else. The path request took 0.40 s
through any lnsd relay, which is the reference's grace before a transport
node answers from its table (`PATH_REQUEST_GRACE`,
`reference/Reticulum/RNS/Transport.py:81`; ours
`leviculum-core/src/constants.rs:209`), and 0.01 s without one.

So on IP the hop count does not matter, the geography of the legs does.
Mouth-to-ear for MQ through the rendezvous [DERIVED]: 60 ms framing +
about 16 ms one way + up to 120 ms of receive buffer + the platform's
audio input and output, i.e. 80-200 ms before the platform, inside the
150-400 ms that ITU-T G.114 calls acceptable [UNVERIFIED citation, from
memory]. Two phones on two different rendezvous add the leg between the
two nodes.

Not measured, and needed before C of the reachability page: a direct
UDP path after hole punching, and a phone on mobile data (jitter, not
mean, is what breaks a two-frame buffer).

### LoRa: computed

A port, in integer arithmetic, of `airtime_ms_with_preamble`
(`leviculum-core/src/rnode.rs:1127`), `packet_airtime_ms` (`:1702`) and
`derive_preamble_symbols` (`:964`); it reproduces the core's pinned
10 691 ms (`rnode.rs:3616`). At our defaults (869.463 MHz, SF8, 125 kHz,
CR 4:5, 18 preamble symbols) one LXST packet costs [DERIVED]:

| Codec2 mode | Frame time | Packet | Air time | One way, share of channel | Full duplex |
|---|---|---|---|---|---|
| 1200 | 200 ms | 115 B | 370 ms | 1.85 | 3.70 |
| 1200 | 400 ms | 147 B | 452 ms | 1.13 | 2.26 |
| 1200 | 1000 ms | 227 B | 656 ms | 0.66 | 1.31 |
| 2400 | 200 ms | 147 B | 452 ms | 2.26 | 4.52 |
| 2400 | 400 ms | 195 B | 574 ms | 1.44 | 2.87 |
| 3200 | 200 ms (LBW) | 163 B | 493 ms | 2.46 | 4.93 |
| 3200 | 400 ms | 243 B | 697 ms | 1.74 | 3.48 |
| 700C | 400 ms (ULBW) | 115 B | 370 ms | 0.93 | 1.85 |

Above 1.0 the sender cannot keep up even alone on the channel. **LoRa at
SF8/125 kHz carries no live voice in any mode**, not even half duplex.
The fixed costs are why: 37 ms of preamble per packet and 67 B of link
and token overhead around 20-80 B of voice. Lengthening the frame does
not save it either; at 1 s frames 1200 is still 0.66 one way, and a
second of delay per frame is no call.

What would carry it, full duplex share of the channel [DERIVED]:

| SF / BW | 3200 @ 200 ms | 1200 @ 400 ms | 700C @ 400 ms |
|---|---|---|---|
| SF7 / 125 kHz | 2.83 | 1.29 | 1.06 |
| SF5 / 125 kHz | 1.13 | 0.53 | 0.45 |
| SF7 / 250 kHz | 1.54 | 0.70 | 0.59 |
| SF5 / 250 kHz | 0.51 | 0.23 | 0.20 |
| SF8 / 500 kHz | 1.38 | 0.64 | 0.54 |
| SF7 / 500 kHz | 0.89 | 0.41 | 0.35 |
| SF5 / 500 kHz | 0.29 | 0.14 | 0.12 |

A usable call needs the sum well below 1 (CSMA, turnaround, a relay
that repeats on the same channel doubles it): SF7 at 500 kHz or SF5-6 at
250-500 kHz, Codec2 1200 or 700C. None of that is lawful here at a
useful duty: the one EU 868 sub-band with 10 % is 869.4-869.65 MHz
([regulatory airtime](../src/concepts/regulatory-airtime.md); table
`etsi_eu868_duty_cycle`, `leviculum-core/src/rnode.rs:1570`), 250 kHz
wide, so 500 kHz does not fit and 10 % caps each speaker at a share of
0.10, which even 700C at SF5/250 kHz (0.10 one way) only just meets. SF5
and SF6 exist on the SX126x only, and the range at SF5/250 kHz is a
fraction of SF8/125 kHz. Voice over our LoRa is a voice message: a
Codec2 1200 message of 10 s is 1 500 B of audio, one small resource
sent once.

Aside: the doc comment of `announce_cap_bitrate_bps`
(`leviculum-core/src/rnode.rs:1765-1766`) says a 183 B announce takes 2.05 s
at SF10/125 kHz/CR 4:5; the formula it cites gives 1.76 s at CR 4:5 and
2.07 s at CR 4:6. The comment's coding rate looks wrong; the code is not
affected.

## 3. The user path

1. **The button.** A call button in a chat, shown only when the contact
   can be called at all (an `lxst.telephony` destination is known for
   their identity, from its announce or a path answer). No separate call
   address: it follows from the contact's LXMF identity
   (`hash_from_name_and_identity`, as Sideband does).
2. **Finding the path.** On tap, a path request for the call
   destination, every time, as rule 2 of the reachability page does for
   `loquax.home`; the client never relies on an announce reaching it
   behind an access point. Where the path comes from, in the reachability
   page's order: E (same WiFi, AutoInterface, one relay or none), B (both
   on a rendezvous; two IP legs), A (a mesh path of N hops). The client
   reads the hop count and the first hop's interface class from the path
   table.
3. **Quality before dialling.** One line under the button, from what the
   path says, before the user taps "call":
   - only IP interfaces on the path: "Call" (MQ by default);
   - a LoRa hop anywhere on the path, or a link whose measured RTT is
     above a threshold: no call button, "Voice message" instead. Our
     transport knows the receiving interface of the path but not the
     media of later hops; the link's RTT after setup is the honest
     signal, so the decision is final only after the link is up (the
     caller can still say "this path is too slow, send a voice message").
   - Hops are shown as Sideband does ("over 2 hops"), never as jargon:
     "direct", "via the internet", "via the mesh, slow".
4. **Only a mailbox reachable.** The phone is offline, the home node is
   not. No call; the caller is offered a voice message (1222), which the
   mailbox stores like any message. Later the home node can answer the
   call itself as voicemail (MeshChatX advertises "custom voicemail",
   `reference/Reticulum/docs/source/software.rst:128`), section 5.
5. **Incoming.** The phone rings only while it holds its rendezvous or LAN
   connection. That is the push channel of the power page; a call adds no
   new wake mechanism. If the phone is asleep in a way that drops the TCP
   connection, the call fails over to voicemail or a missed-call message.
6. **Battery.** A call keeps microphone, codec and radio busy: 16.7
   packets a second each way at MQ, so a cellular modem never leaves its
   high power state (`radio_on_frac` = 1 by construction, the tier P
   proxy). The cost to measure is mAh per minute of call on a real phone,
   Loquax against Sideband on the same network, in the tier R plan of
   1177 ([phone power](../src/concepts/loquax-phone-power.md)). Codec2
   profiles at 2.5-5 packets a second are the lever if it is large.

## 4. What Loquax would have to build

### What stays platform, what is Rust

| Layer | Where |
|---|---|
| microphone capture, speaker and earpiece playback, audio focus, proximity sensor, the ringer, the Android call notification and foreground service, Bluetooth headsets | platform (Android `AAudio`/`Oboe` through the NDK, ALSA/PipeWire via `cpal` on the desktop) |
| resampling, band pass, AGC, jitter buffer, mixer, tones | Rust |
| Opus and Codec2 encode/decode | Rust, binding the C libraries (libopus BSD-3, codec2 LGPL-2.1) or a pure Rust port; which crate is a slice-1 decision [UNVERIFIED: no candidate was vetted in this pass] |
| LXST framing, signalling state machine, profiles | Rust, `leviculum-lxst` as named in [the core lock budget](../src/concepts/core-lock-budget.md) |
| links, path requests, identify | the existing core |

That follows Lew's rule of 2026-10-31, the core maximal and the platform
layer minimal: the platform draws, captures and plays, everything else is shared by the TUI,
the GUI, Android and later the M9 (which has no audio path and gets
voice messages only).

### Slices

1. **`leviculum-lxst`, framing and signalling, no audio.** The packet
   shapes and the state machine above, with a test double for audio.
   Interop cells against Python LXST 0.5.4 (`rnphone` or the `Telephone`
   primitive driven by a script, both directions, both in the docker
   cells): call, answer, reject, busy, hang up from either side, profile
   and mode switch. Positive and negative per call state. This slice is
   where the licence boundary is held: the implementation is written from
   this page.
2. **Codecs.** Opus MQ and HQ, Codec2 700C/1600/3200; byte-exact frame
   sizes against the table above; decode of frames Python LXST encoded.
3. **Desktop call in Loquax (TUI and GUI).** `cpal` capture and playback,
   the call button, the quality line of section 3, call log. A call
   between two Loquax desktops on one LAN and through `leviculum.network`,
   and against Sideband.
4. **The home node as call endpoint** (if Lew picks it, section 5): the
   home node registers `lxst.telephony` for the LXMF identity, answers,
   and bridges to the admitted phone over its home link, frames copied
   without decoding. One more IP leg, no re-encoding.
5. **Android.** Oboe through the NDK, a foreground service for the call,
   the system call UI (`ConnectionService`) if it is worth its weight.
   Then the tier R battery number.
6. **Voicemail** on the home node, later: answer after N seconds when no
   phone is connected, record into a 1222 voice message.

### Compatibility with Sideband

Same framing, same destination aspects, same signals, same profile ids:
nothing in LXST needs a wire change, and nothing we would add needs one.
The packet shapes are those of section 1. Two constraints follow from the
reference's timers: a Loquax callee must reach RINGING within 5 s of a
Sideband caller starting (AVAILABLE and RINGING right after the link and
the identify, no wait on the user), and a Loquax caller must have the path
before it opens the link, or a Python callee is fine but a Sideband user
interface shows "no path" (`voice.py:236`).

## 5. Decisions

For Lew:

1. **Who answers a call to a Loquax user.** Sideband rings the LXMF
   identity's `lxst.telephony`; only the home node holds that identity.
   (a) The home node answers and bridges to the phone (slice 4): Sideband
   and every LXST client reach Loquax users, one extra IP leg (about
   2 × 15 ms on our measurement). (b) Each phone announces its own
   `lxst.telephony` under its device identity, the home node publishes
   the mapping to contacts: no extra leg, but no Sideband user can call a
   Loquax phone. (c) Both, Loquax callers taking (b) when they know the
   device. **Recommended: (a)** first, since it is the only one that
   interoperates, and (c) later if the leg is measurably bad.
2. **The licence.** LXST is CC BY-NC-ND. **Recommended:** a clean
   implementation from this page's protocol description, no line of LXST
   code copied, and the interop cells run Python LXST only as an
   external test peer, never shipped. If Lew wants certainty, ask a
   lawyer, not Mark (who does not answer).
3. **LoRa.** **Recommended:** no live calls over LoRa, ever, at the
   lawful EU settings; the call button disappears behind any LoRa hop and
   the voice message takes over. Half-duplex push to talk (0.5.4 mode
   `0xF2`) at SF7/250 kHz or faster stays an experiment for later, not a
   product promise.
4. **Default profile.** **Recommended:** MQ (Opus, 60 ms) as LXST does;
   LBW (Codec2 3200) offered automatically when the measured link RTT is
   high or the user is on a metered network.

Recommended without needing Lew:

- The call address follows from the LXMF identity; no new contact field.
- A path request on every dial, never a wait for an announce.
- The home node, not the phone, is the 3 h `lxst.telephony` announcer,
  if (a) is taken.
- The quality line in the chat uses words, not hop numbers.

## Appendix: how the numbers were taken

Scripts (outside the tree, in order 1257's scratch directory on
schneckenschreck): `probe.py` (echo and caller roles, Python RNS from
`reference/Reticulum`), `run.py` (writes configs, starts 0-3 lnsd relays
chained by TCP on 127.0.0.1:47310 up, with `[auto]` all `no`, runs the
caller in the foreground and kills every child), `airtime.py` (the port of
the core's airtime functions). The WAN run used one announce of a throwaway
destination on `leviculum.network:4242`; its access point port 4343
refused connections on 2026-11-01, so the caller used 4242 as well.
