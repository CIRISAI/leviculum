# Loquax: recording a voice message looks like recording

Status: built (1276, 2026-11-01). Pass one wrote this page and the
core's state machine with its tests; pass two built Android, the window
and the terminal from it (section 7). Not yet run on a phone: the device
walk's four steps and the floor's numbers (section 4) are the
reviewer's.

Lew, on the 11:08 build of 2026-11-01: pressing the microphone gave no
visual feedback, so you could not tell whether it recorded at all. He
wants a recording mode you cannot miss, that also looks good: the length
visible, a live picture of the sound so you see at once that the
microphone works, hold for short messages, and for long ones a lock so
the thumb can let go, with a clear way to end it. Signal does this with
a red microphone, a timer, "slide to cancel" and a lock chip you swipe up
to. We take the ideas, not the look, and make it clearer.

This page builds on [voice messages](loquax-voice.md) (1222: the wire,
the codec, the recorder and the bubble's player) and the compose bar of
1270 (the microphone inside the field).

## Decisions

| Question | Decision |
|---|---|
| States | Three you cannot mistake: **held**, **locked**, and **paused** (or stopped at the cap). |
| Hold | The whole compose bar turns into the recording bar the moment the finger lands. A short haptic tick marks the start. |
| Release | Sends, as every messenger does. If the finger slid left past 96 dp, the release discards and the bar slides away. |
| Lock | Swipe up 72 dp, or tap the lock chip above the button. A second tick. The button becomes a big Send, with the bin and a pause beside it. |
| Pause | The waveform stands still. Play lets you hear it, Send sends, the bin discards, and resume goes on with the same recording. |
| Waveform | The recording's own level track, one level every 50 ms. Live, it is the newest three seconds, scrolling right to left. After sending, the same track is the bubble's waveform, through one resampler. |
| Nothing heard | If 2 s pass with no window at or above the floor, a line under the waveform says "Nothing heard. Is the microphone allowed?", offers the permission, and the dot stops pulsing. |
| Cap | 10 minutes: what a receiver takes (990 000 bytes) at our Opus rate, counted high. The timer turns amber in the last 30 s. At the cap, recording stops with "That is as long as a voice message can be", and you are in the preview. |
| Where it is decided | In the core, `no_std`: `loquax::ui::recording`. The platform only hands in gestures and levels, and runs what the model answers. |

## 1. The three states

### Held

The finger is on the microphone. The field, its emoji, camera and
position buttons are gone. The bar shows:

- a red dot that pulses, and `REC 00:07` in large digits;
- the live waveform filling the middle;
- "Slide left to cancel" with a chevron at the field's right edge;
- the round button, grown and red, with the microphone under the finger;
- above that button, the lock chip: a lock and an up chevron.

```text
                                                   ┌───┐
                                                   │ 🔒│
                                                   │ ˄ │
                                                   └───┘
( ● REC 00:07  ▂▃▅▇▅▃▂▁▂▅▇█▇▅▃▂▁▁▂▃   ‹ Slide left to cancel )  (🎤)
```

As the finger slides left, the hint follows it (`cancel_progress`, 0 to
100). Past 96 dp it reads "Release to cancel". As the finger slides up,
the chip rises (`lock_progress`). At 72 dp it locks.

### Locked

The thumb can let go. The chip shows closed, and the bar stays. Where
the microphone was, the round button is a big **Send**. Left of it are
the bin and the pause.

```text
                                                   ┌───┐
                                                   │ 🔒│ locked
                                                   └───┘
( ● REC 01:12  ▃▅▇▅▃▂▁▂▅▇█▇▅▃▂▁▁▂▃▅▇▅▃▂▁▂▅▇           🗑  ⏸ )  (➤)
```

### Paused, and stopped at the cap

The waveform stands still and shows the whole recording, folded to the
bar's width. The dot does not pulse. Play lets you hear what is there.
Resume goes on with the same recording, and the timer and the waveform
carry on from where they were. Send sends, and the bin discards.

```text
( ○ REC 01:12  ▁▂▃▅▇▅▃▂▁▂▅▇█▇▅▃▂▁▁▂▃▅▇▅▃▂▁   ▶   🗑  ● resume )  (➤)
```

The cap leads here too, but without resume (the **preview**). The line
under the waveform then says "That is as long as a voice message can be".

## 2. Gestures

| | Android | Window, mouse | Terminal |
|---|---|---|---|
| Start | Press and hold the microphone | Press and hold the microphone | `Alt-v` in compose (starts locked: a terminal has no hold) |
| Send | Release; or Send once locked | Release; or Send once locked | `v` or `Alt-v` again, or the send chord (`Enter`) |
| Cancel | Slide left past 96 dp, then release; or the bin | Drag left past the hint, then release; or the bin | `Esc` or `Ctrl-g` |
| Lock | Swipe up 72 dp, or tap the chip | Drag up to the lock chip and release over it | (already locked) |
| Pause, resume | The pause button; resume | The same buttons | `p` |
| Listen | Play in the paused bar | Play | `Space` while paused |
| Wheel | (none) | No part in the recording: the chat scrolls as ever | (none) |

Why `Alt-v` and not `v`: in the compose line `v` is a letter, and a
message that begins with one ("vielleicht") must not start a recording.
An empty-field rule would still fire on that first letter, and `Ctrl-v`
is paste in many terminals. Once the bar is up the field takes no
typing, so the plain keys `v`, `p` and `Space` are free there.

Why the window locks on release over the chip: one mouse button cannot
hold the microphone and click the chip at once. The phone's second
finger on the chip works, and so does TalkBack's click on it.

Every gesture becomes a `recording::Input`, and every button a
`recording::Verb` (`Intent::Recording`). What the model answers is a
list of `recording::Effect`s: `Haptic`, `Start`, `Pause`, `Resume`,
`Stop`, `Finish`, `Discard`, `Play`, `StopPlay`, `AskMicrophone`. The
haptic ticks are effects like the others, so a test sees them: one at
the start, one on lock, one at the cap.

## 3. Data flow

```text
 platform                     core (no_std)                        renderer
 ────────                     ─────────────                        ────────
 AudioRecord, 50 ms window
   RMS ────────── Input::Level{now, rms} ─▶ Recording.track ─┐
 touch: down/move/up ─ Input::Press/Slide/Release ─▶ phase   │
 bar buttons ───────── Intent::Recording{verb} ──▶ phase     │
                                                             ▼
                          Scene: Compose.recording = RecordingBar
                            timer (REC 00:07, amber), wave (levels,
                            moving), cancel hint, held mic, lock chip,
                            buttons, notice ───────────────────────▶ draws
 ◀── UiEffect::Recording{effect}: start/pause/resume/stop/finish/
     discard/play/haptic/ask the permission
 Finish: the platform hands the PCM over (Intent::SendVoice, 1222);
 the model sends it and keeps the track as the bubble's waveform
 (UiModel::voice_tracks).
```

**One data structure, one resampler.** `LevelTrack` holds one level,
from 0 to 100, every `WINDOW_MS` = 50 ms. A level is the window's RMS in
dBFS, linear from -70 dBFS (0) to full scale (100), so loud is tall.
`LevelTrack::bars(n, fold)` is the only resampler. It folds the windows
into `n` bars, by peak or by mean. Where there are fewer windows than
bars, it stretches them.

- The live bar is `track.live(60)`: the newest three seconds, newest at
  the right.
- The paused bar is `track.bars(60, Peak)`: the whole recording at the
  same width, standing still.
- The bubble after sending is `track.bars(32, Peak)`. The track moves
  from the recording into `UiModel::voice_tracks`, keyed by the peer and
  the message, when the PCM comes back as `SendVoice`.
- A received message has no track on the wire, and none of the others
  sends one. The shell decodes it (it does already, to play it) and
  hands in `LevelTrack::from_pcm(pcm, rate)` as `UiEvent::VoiceTrack`.
  The 50 ms windows and the RMS are the same as the recorder's, and
  `from_pcm` agrees with the platform's feed window for window
  (`a_track_from_pcm_is_the_platforms_windows`). Until the shell has done
  that, the stream's packet sizes stand in (`LevelTrack::from_packets`,
  the 1222 bar), through the same `bars`.
- The terminal draws the same bars as `▁▂▃▄▅▆▇█`, eight steps of the
  0 to 100 level.

## 4. The floor

`FLOOR_RMS` = 2, in units of a 16-bit sample, which is about -84 dBFS.
A window below it is no sound.

Why it sits there: Android gives an app digital silence, all zeros, when
the microphone is blocked by the privacy switch, when another app holds
it, or when the permission is gone mid-session. A working phone
microphone is never that quiet. Its own noise, after the platform's gain,
sits tens of dB above one step even in a quiet room. So the floor sits
just above zero and one step of dither. It catches the blocked
microphone and never fires on a quiet room. A false "Nothing heard"
while the person simply has not started talking would be worse than a
late one.

**Not measured yet.** The Pixel's and the Fairphone's quiet-room floors
are still open, because the coder host has no phone attached. Pass two
measures them:

- Record 10 s in a quiet room with the walk's debug build logging each
  `Input::Level`.
- Record 10 s with the microphone switched off by the privacy toggle.
- Write the four numbers here, as the minimum and median RMS per window.

If a phone's quiet room comes within 6 dB of the floor, the floor moves
down to zero, which is "only digital silence".

The detector is `Recording::silent`: not one window at or above the floor
(`heard` is still false), and at least `SILENCE_MS` = 2 000 ms recorded.
One heard window clears it for good. The hint's button raises
`Verb::AskMicrophone`, which reaches the platform as
`Effect::AskMicrophone`. The platform then runs the permission path as
1201 runs the location one: ask while it may, otherwise open the app's
settings.

## 5. Limits

The cap comes from what a receiver takes, not from a habit:

- A message may carry 990 000 bytes (`loquax::ui::files::LARGEST`, the
  photo and file budget: a receiver refuses past 1000 KB).
- Our Opus runs at 12 kb/s on average (`OPUS_BITRATE`). Counted high,
  with 5 % for the Ogg pages (1222 measured 4.9 %), that is 1 575 B/s
  (`OPUS_BYTES_PER_S`).
- 990 000 / 1 575 = 628 s. In whole minutes that is **10 minutes**
  (`loquax::ui::voice::LONGEST_MS`). At the 1 202 B/s that 1222
  measured on speech, ten minutes come to about 720 KB.

1222's limit was five minutes, which was Columba's own recorder limit.
Columba plays a longer message like any other. Sideband has no cap on
what it plays.

- The timer turns amber at 9:30 (`AMBER_MS` = 30 s before the cap).
- At 10:00 the recording stops (`Effect::Stop` and a haptic tick) into
  the preview, with the line "That is as long as a voice message can be".
- A release under 0.6 s (`SHORTEST_MS`, Sideband's) sends nothing and
  shows the toast "Hold the microphone to record, release to send.".
- A track keeps no more than the cap plus one second of windows
  (`MOST_WINDOWS`), so a platform that forgets to stop cannot grow it
  without end.

A propagation node, lxmd or lnpnd with or without a config, carries
256 KB a message on to its peers, about three and a half minutes of
voice; a longer one is stored local only, collected from the node it
was uploaded to ([files](loquax-files.md), section 2). A board's node
takes 4 KB. Those are the nodes' limits, not the recorder's.

## 6. The core, built in pass one

`loquax::ui::recording` (`loquax/src/ui/recording.rs`), `no_std`:

- `Phase`: `Idle`, `Held{since}`, `Locked{since}`, `Paused`, `Preview`,
  `Cancelled`, `Sent`.
- `Recording::apply(peer, Input) -> Vec<Effect>`. There is one recording
  at a time. A press in another chat while one is up does nothing.
- `LevelTrack`: `push_rms`, `from_pcm`, `from_packets`, `live`, `bars`.
- `rms`, `dbfs10` (log2 by bit length, to within half a dB, no float
  library) and `level_of`.
- The constants `WINDOW_MS`, `LIVE_WINDOWS`, `SILENCE_MS`, `FLOOR_RMS`,
  `AMBER_MS`, `CANCEL_DP`, `LOCK_DP`.

The model:

- `UiModel::recording` and `UiModel::voice_tracks`.
- Events: `UiEvent::Record{peer, input}` and `UiEvent::VoiceTrack`.
- Effect: `UiEffect::Recording{peer, effect}`.
- Intent: `Intent::Recording{peer, verb}`.

The scene:

- `Compose.recording: Option<RecordingBar>`. Its parts are nodes:
  - `rec_bar`
  - `rec_timer` (words `REC 00:07`, `amber`)
  - `rec_wave` (levels, `moving`)
  - `rec_cancel` (words, progress, armed)
  - the held microphone, still `record`
  - `rec_lock` (open with its action while held, closed once locked)
  - the buttons `rec_discard`, `rec_pause`, `rec_resume`, `rec_play`,
    `rec_stop_play`, `rec_send`
  - `rec_notice` (nothing heard, with its action; or the cap)
- The voice bubble's `levels` are the track's bars where the model has a
  track.

There are 16 new catalogue strings, in English and German; the cancel hint keeps 1222's `voice_cancel`.

Tests:

- `ui::recording`:
  - the transitions: hold and release sends; a tap sends nothing; a slide
    past the threshold and a release cancels; a swipe up and the chip
    lock; locked, pause and resume keep the track and the time; the cap
    stops into the preview; one recording at a time;
  - the silence detector at the floor's numbers;
  - the dB levels;
  - the resampler, 100 windows to 40 bars, peak and mean;
  - `from_pcm` against the platform's feed;
  - the track's bound;
  - the timer.
- `ui::scene`:
  - the bar in each state, its JSON, its actions;
  - the bubble's waveform equals the track resampled;
  - the decoded track replaces the stream's;
  - the silent microphone's line and its way to the permission;
  - a tap's toast.

## 7. What pass two built per target

**Android** (`loquax-android`):

- The facade: `Request::Record{peer, input, now, dx, dy, rms}` carries
  an `Input` (press, slide in dp, release, cancel, level, tick, played),
  `Request::Recording{peer, verb}` a button by its resource word
  (`rec_lock` to `rec_allow`), and `Query::Recording{peer}` answers the
  bar as the scene has it (phase, dot, timer, wave, hint, held, lock,
  buttons, notice), the distances `cancel_dp`, `lock_dp`, the window and
  the cap from the core, and the effects since the last ask, once each.
  `Messages.recording` and `Bar.recording` carry the phase while the bar
  is up. After `Query::Voice` has decoded a received message, its
  `LevelTrack::from_pcm` goes in as `UiEvent::VoiceTrack`.
- Java: `RecordFrame` keeps the finger that landed on the microphone for
  the whole gesture; `RecordingBar` draws the dot, the timer, the
  waveform (`Voice.Wave`, newest at the right, loud is tall and
  brighter), the hint that follows the finger, the held microphone, the
  rising lock chip, the buttons and the notice, and runs the effects:
  the recorder, the preview's player, the haptics and the permission.
  `Voice.Recorder` feeds a level every 50 ms (`Levels.java`, the core's
  `rms` window for window, checked on the host by
  `fling::the_recorders_windows_are_the_librarys`). `Voice.LONGEST_MS`
  is gone: the recorder stops on the core's `Stop`.
- The bubble draws `voice_levels` as bars, as 1222 did; they are now the
  recording's track.
- The colours `rec_red` and `rec_amber` are tokens of
  `loquax::ui::chat::Colours`, in `colors.xml` for both themes.

**Window and terminal** (`loquax`):

- The desktop's microphone (`loquax/src/mic.rs`) is the system's
  recorder as a child process writing raw mono 16 kHz PCM to its output:
  `LOQUAX_RECORDER`, else the first on `PATH` of `pw-record`, `parecord`
  and `arecord`. The shell cuts it in 50 ms windows into `Input::Level`,
  keeps the samples while it records, drops them while paused, and on
  `Finish` codes the PCM and hands it over as `UiEvent::SendVoice`.
  `Play` writes `voice/recording-<peer>.wav` for the system's player.
  With no recorder, starting says what to install and discards.
- The window: a press on the microphone is `Press`, the pointer's moves
  while held are `Slide` (logical pixels as dp), the release is
  `Release`, released over the chip `Verb::Lock`. The bar draws the
  same parts as the phone's, and the bubble its bars.
- The terminal: `Alt-v` starts locked; the compose line becomes
  `● REC 00:07  ▁▂▃▅▇…  p pause · Esc cancel · Enter send`, red, the timer
  amber in the last 30 s, the waveform in the eight block characters
  (`_.-:=+*#` with `--ascii`), the notice in a second line. The words
  take a click. A voice bubble draws its 32 bars under its line.

**Proof:**

- Device walk steps (`loquax-device-walk.md`): `voice_hold_release`,
  `voice_slide_cancel`, `voice_lock_send`, `voice_silence_hint` (the
  fixture is eight seconds of zeros). Not yet run on a device.
- The facade test
  `a_recording_is_held_slid_away_locked_paused_and_sent_from_its_bar`.
- `loquax::mic` tests: three seconds of a fixture source send one voice
  message whose bubble is the track's bars; pause and resume keep one
  continuous recording; a discard sends nothing; a silent source shows
  the line at 2 s and not at 1.95 s.
- Window goldens `gui-recording-held.png`, `gui-recording-locked.png`,
  `gui-recording-paused.png`; the TUI snapshot
  `loquax/tests/fixtures/tui_recording.frames`.
- Open: the floor on the Pixel and the Fairphone (section 4).
