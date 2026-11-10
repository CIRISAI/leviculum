# Loquax: voice messages

Status: slice one built (1222, 2026-11-01). Lew, 2026-10-31: press and
hold, record, release, and the voice message goes compressed, as Signal
does it, and compatible with the other messengers.

## Decisions

| Question | Decision |
|---|---|
| Wire | The LXMF audio field, `0x07`, holding `[mode, bytes]`, the mode one of the reference's audio modes. No field of our own. |
| What goes | Opus in Ogg, `AM_OPUS_OGG` (`0x10`): the one mode Columba and Sideband both play. Mono, 16 kHz, 12 kb/s on average, speech mode, 20 ms frames, ended exactly (the last page's granule says where the recording stops). |
| What plays | Opus in Ogg from anyone, and the Codec2 modes Sideband sends on slow links (3200, 2400, 1600, 1400, 1300, 1200). 700C and the two 450 modes show as a voice message that does not play here. |
| Length | 0.6 s to 10 min. A shorter hold is a tap and sends nothing, the chat says to hold. The cap was 5 min, Columba's limit, until 1276 derived it from what a receiver takes ([recording](loquax-voice-recording.md), section 5). The recorders stop where the core says (`Effect::Stop`), on the phone and the desktop alike. |
| Where the work is done | In Rust: the field, the codec (`loquax::voice`), the length rules and their words, the level bar, which message plays. Java keeps the microphone (`AudioRecord`), the player (`AudioTrack`), the button's gestures and the permission. |
| Settings | None. Every message goes in the one mode every receiver plays. |

## 1. The wire and what the others do

The reference defines `FIELD_AUDIO = 0x07`
(`reference/LXMF/LXMF/LXMF.py:14`) and the audio modes from
`AM_CODEC2_450PWB = 0x01` to `AM_CUSTOM = 0xFF`
(`reference/LXMF/LXMF/LXMF.py:65-89`): Codec2 0x01 to 0x09, Opus 0x10 to
0x19, of which only `AM_OPUS_OGG = 0x10` names a container.

**Columba** sends `listOf(AM_OPUS_OGG, voiceBytes)`
(`columba/app/src/main/java/network/columba/app/viewmodel/MessagingViewModel.kt:1375`,
the constants at
`columba/rns-api/src/main/java/network/columba/app/rns/api/util/LxmfFields.kt:46-49`)
and plays only that mode: `isPlayable = modeEnum ==
AudioAttachmentMode.AM_OPUS_OGG`
(`columba/app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:301`).
Its recordings stop at five minutes
(`columba/app/src/main/java/network/columba/app/audio/VoiceMessageRecorder.kt:214`),
and its interop suite carries an Ogg Opus fixture both ways to Sideband
(`columba/tests/interop/test_audio.py`).

**Sideband** (a clone of github.com/markqvist/Sideband at `ef572b5`,
2026-10-01) sends Opus in Ogg by default: `hq_ptt` defaults to on
(`Sideband/sbapp/sideband/core.py:643`), and the push to talk button picks
`AM_OPUS_OGG` with it and `AM_CODEC2_2400` without
(`Sideband/sbapp/main.py:2475-2476`). Its Codec2 bytes are the codec's
frames back to back with no header (`encode_codec2`,
`Sideband/sbapp/sideband/audioproc.py:117-143`), not LXST's stream frames,
which carry a mode byte (`reference/LXST/LXST/Codecs/Codec2.py:85`). It
plays both (`Sideband/sbapp/main.py:2436-2446`) and drops a recording under
0.6 s (`Sideband/sbapp/main.py:2517`).

So the facts agree with the order: the codec the others accept is Opus in
Ogg, and the Codec2 a Sideband user on a slow link sends is worth playing.

Our bytes for `fields[7] = [0x10, b"OggS"]` are those
`umsgpack.packb([0x10, b"OggS"])` gives:

```
07  92  10    c4 04 4f 67 67 53
key arr mode  bin 4 "OggS"
```

Reading is lenient: the bytes a bin or a str, an element past the second
skipped, a field that does not parse no voice message.

## 2. The codec

The crate is `opus-pure` 0.2.2 (BSD-3-Clause): Opus encoder and decoder
and the Ogg container in Rust, no C and no build script, so the NDK builds
it as it builds the rest. Codec2 is the `codec2` crate 0.3.1, a Rust port
of the codec (LGPL-2.1-only AND MIT, which the AGPL-3.0 binaries can carry;
`about.toml` names the licence). It has no 700C, which is why that mode
does not play.

Measured on the coder host (Ryzen 9 7950X, release build,
`cargo run --release -p loquax --features voice --example voice_cost`,
best of 3), the speech-like test signal of `loquax::voice::test_speech`:

| | |
|---|---|
| Opus, a minute | 72 146 bytes, 1.2 KB/s (`opusinfo`: 9.6 kb/s with the container, 4.9 % overhead) |
| encode a minute | 338 ms |
| decode a minute | 28 ms |
| Codec2 2400, a minute | 18 000 bytes, decoded in 55 ms |
| ten minutes, the longest (1276) | about 720 KB, under the 990 KB a message may carry |

No phone was measured. The encoder runs at complexity 9, opus-pure's
default; a phone that is slow at it is the first thing to lower.

A recording at a rate the encoder does not take (44.1 kHz, the one
Android promises) is resampled to 16 kHz first; 8, 12, 16, 24 and 48 kHz
go as they are. A received Opus message is decoded at 24 kHz for the
player, a Codec2 one at its 8 kHz. A decode stops at ten minutes of sound,
so a hostile stream of one byte packets does not ask for hours of PCM.

## 3. The core

`loquax::ui::voice`, `no_std`:

- `voice_of` reads a message's mode, bytes, length and level bar.
  The length of an Ogg stream is its last granule less the pre-skip
  (RFC 7845, 4.2 and 4.4); a Codec2 message's is its frame count. The
  level bar is 32 heights from 0 to 100 made of the Opus packets' sizes,
  which in a variable rate stream follow the speech; Codec2 frames are
  all one size, so their bar is even.
- `refusal` says why a recording does not go: under 0.6 s "Hold the
  microphone to record, release to send.", past the cap "A voice message
  lasts at most 10:00." In English and German, from the catalogue.
- The model keeps which message plays (`UiModel::playing`).
- `UiEvent::SendVoice`, `PlayVoice`, `PauseVoice`, `VoiceEnded`;
  `UiEffect::PlayVoice`, `PauseVoice`.
- `Intent::SendVoice { peer, pcm, rate }` (the compose bar's microphone),
  `PlayVoice`, `PauseVoice`.
- The scene's `VoiceNode` (id `voice_<key>`): mode, bytes, length in ms
  and as `0:07`, the level bar, whether it plays, its tap (play, or pause
  while it plays; none for a mode that does not play here) and its words,
  "Voice message, 0:07". The compose bar gains `record`, whose words are
  "Hold to record a voice message".
- The list says "voice message 0:07" and `kind` is `voice`.

In the terminal a voice message is a line `♪ voice message · 0:07`
(`[v] voice message - 0:07` in ASCII); `Enter` writes it decoded as
`$LNMSG_HOME/voice/<peer>-<key>.wav` and hands it to the system's player.
The window draws the same line, and a press does the same. Since 1276
both draw the bubble's waveform under it (block characters in the
terminal, bars in the window) and both record: the desktop's microphone
is the system's recorder (`pw-record`, `parecord` or `arecord`, or
`LOQUAX_RECORDER`) read as raw 16 kHz PCM (`loquax/src/mic.rs`), and the
terminal starts a recording with `Alt-v`
([recording](loquax-voice-recording.md)).

## 4. Android

- With nothing typed, the send button is the microphone, as in Signal.
- **Hold** it: at the first press the permission is asked
  (`RECORD_AUDIO`); after that a press records, the status line says
  "● 0:03 ‹ Slide left to cancel", and the phone gives a tap of feedback.
- **Release**: Java hands the PCM to the library (`Request::Voice`, mono
  16-bit little endian and the rate), which codes and sends it, or answers
  why not, which the status line shows.
- **Slide left** by 96 dp: the recording is gone.
- `AudioRecord` asks for 16 kHz, then 48 kHz, then 44.1 kHz.
- A bubble's voice message is a row in the bubble above its words
  (1268), as a file's row is: the play or pause button, the level bar
  (played part bright, the rest dimmed) and the length. A tap asks the
  library for the sound (`Query::Voice`, answered as `VoicePcm`) and
  writes it to an `AudioTrack`; `Request::PlayVoice` tells the library
  it plays, stops or ended, and the button follows the row.
- A debuggable build records a file `voice-fixture.pcm` in the app's files
  instead of the microphone, when there is one: the device walk's fixture,
  since an emulator has no voice to give.

## 5. Proof

- `loquax-android/tests/facade.rs::a_voice_message_travels_both_ways_with_python_lxmf`:
  - Python sends Codec2 2400 made by `c2enc` and Opus in Ogg made by
    `opusenc` to the phone's mailbox.
  - The rows say both with their length and words, and the list says
    "voice message 0:02".
  - The decoded sound is the reference decoder's: against `c2dec` a
    likeness of 0.999, against `opusdec` 0.99999997.
  - The button plays and pauses.
  - Three seconds the phone records reach Python's router as
    `[0x10, OggS…]` (3 642 bytes); `opusdec` plays them back as exactly as
    long as recorded, at a likeness of 0.983 to what was recorded.
  - A tap of 0.3 s is refused with the hint and nothing goes.
  - Positive controls: sending in the Codec2 mode, decoding 2400 as 1200
    and writing the last page without its end each turned it red.
- Unit tests of the field, the Ogg reading (a packet over several
  segments, a cut stream), Codec2's frame lengths, the words, the
  refusals, the codec's round trip at 48, 44.1, 16 and 8 kHz, the bounded
  decode, the WAV header; the scene's node, tap and microphone; the
  terminal's line and `Enter`.
- The device walk's step `voice`: the fixture held for three seconds, the
  new player with its length, the mailbox's send, play to pause and back,
  and a slide that sends nothing.

The tools are `opus-tools` and `codec2` from apt
(`scripts/install-ci.sh`); a runner without them leaves the one test out
(`scripts/integ-prerequisites.txt`).

## 6. Open

1. No phone was measured: the encode time on a phone and the battery a
   minute costs are the reviewer's, on Waydroid and the Pixel.
2. The walk step was not run.
3. A received message is decoded whole before it plays: ten minutes are
   29 MB of PCM for the player at 24 kHz. A streamed decode is the next step if a
   phone minds.
4. No seek. Signal moves the playback with a drag on the bar.
5. Codec2 700C and the 450 modes do not play: the Rust codec has none.
   Sideband sends 2400.
6. A propagation node carries 256 KB a message on to its peers, lxmd
   and lnpnd alike, lnpnd with or without a config: about three and a
   half minutes of voice. (This item said 4 KB, lnpnd's fallback before
   1294; 1663 pins the default by a test.) A longer
   one, up to the ten-minute cap, is stored local only, so an offline
   peer collects it from the node it was uploaded to. A board's node
   takes 4 KB, about three seconds ([board](propagation-node-on-a-board.md),
   section 6).
