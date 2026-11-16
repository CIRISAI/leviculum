//! Recording a voice message, as something that looks like recording
//! (1276; Lew 2026-11-01: the 11:08 build gave no sign it recorded at all).
//! The page is `docs/src/concepts/loquax-voice-recording.md`.
//!
//! Three states the reader cannot mistake, after Signal's ideas and not
//! its look:
//!
//! - **Held**: the finger is on the microphone. The compose bar is the
//!   recording bar: a pulsing red dot and `REC 00:07`, the live waveform,
//!   "Slide left to cancel"; above the button the lock chip. Release
//!   sends, release slid past [`CANCEL_DP`] discards, a swipe up past
//!   [`LOCK_DP`] (or a tap on the chip) locks.
//! - **Locked**: the thumb is free. The round button is Send, beside it
//!   the bin and the pause.
//! - **Paused**, and **Preview** once the cap stopped it: the waveform
//!   stands still, a play button lets the reader hear it, Send sends, the
//!   bin discards; from Paused, resume goes on with the same recording.
//!
//! The platform owns the microphone and the clock; the model owns
//! everything that is decided. The platform hands in the gestures
//! ([`Input`]) and, every [`WINDOW_MS`], the RMS of the window it just
//! recorded ([`Input::Level`]); the model answers with what the platform
//! is to do ([`Effect`]): start, pause, resume, stop the recorder, hand
//! over or drop the PCM, play the preview, tick the haptics.
//!
//! The levels are kept as the recording's [`LevelTrack`], one level a
//! window. The live bar is its newest [`LIVE_WINDOWS`]; the voice bubble
//! after sending is the same track folded to [`super::voice::BARS`] by
//! the one resampler, [`LevelTrack::bars`]. A received message has no
//! track on the wire, so the shell makes one from the decoded sound
//! ([`LevelTrack::from_pcm`]) and, until it has, the stream's packet
//! sizes stand in ([`LevelTrack::from_packets`]); either way the bubble
//! draws a [`LevelTrack`] through [`LevelTrack::bars`].

use alloc::vec::Vec;

use leviculum_ui::strings::Lang;
use leviculum_ui::toast::ToastKind;

use super::strings::{Arg, StrId, Text};
use super::voice::{LONGEST_MS, SHORTEST_MS};
use super::{UiEffect, UiModel};

/// One window of the level track: the platform's RMS is over this many
/// milliseconds of sound.
pub const WINDOW_MS: u32 = 50;

/// How many windows the live bar shows: the newest three seconds,
/// scrolling right to left.
pub const LIVE_WINDOWS: usize = 60;

/// How long the microphone may say nothing before the bar asks whether
/// it is allowed.
pub const SILENCE_MS: u32 = 2_000;

/// The silence floor, as an RMS of 16-bit samples: a window under it is
/// no sound. Android hands an app digital silence, zeros, when the
/// microphone is blocked by the privacy switch or held by another app,
/// while a working microphone's own noise in a quiet room is tens of
/// dB above one step (the page, "The floor"). So the floor sits just
/// above zero and one step of dither, at about -84 dBFS.
pub const FLOOR_RMS: u16 = 2;

/// The timer turns amber this long before the cap.
pub const AMBER_MS: u32 = 30_000;

/// A slide left this far, in dp, cancels on release (1222's distance).
pub const CANCEL_DP: i32 = 96;

/// A swipe up this far, in dp, locks the recording.
pub const LOCK_DP: i32 = 72;

/// The quietest level the bar draws, in tenths of dBFS: -70 dBFS is
/// level 0, full scale level 100.
pub const QUIET_DB10: i32 = -700;

/// The most windows a track keeps: the cap and a second of slack.
pub const MOST_WINDOWS: usize = ((LONGEST_MS + 1_000) / WINDOW_MS) as usize;

/// The RMS of a window of 16-bit PCM.
pub fn rms(pcm: &[i16]) -> u16 {
    if pcm.is_empty() {
        return 0;
    }
    let sum: u64 = pcm
        .iter()
        .map(|&s| {
            let s = u64::from(s.unsigned_abs());
            s * s
        })
        .sum();
    let mean = sum / pcm.len() as u64;
    u16::try_from(mean.isqrt()).unwrap_or(u16::MAX)
}

/// `rms` in tenths of dBFS (full scale 32768 is 0), to within half a dB:
/// log2 by the bit length and a straight line inside the octave. Zero is
/// below anything the bar draws.
pub fn dbfs10(rms: u16) -> i32 {
    if rms == 0 {
        return QUIET_DB10 - 300;
    }
    let x = i64::from(rms);
    let octave = 63 - i64::from(x.leading_zeros());
    // log2(x) in 1/1024ths.
    let log2 = octave * 1024 + ((x - (1 << octave)) << 10 >> octave);
    // 20 log10(2) = 6.0206 dB an octave, so 60.206 tenths.
    let tenths = (log2 - 15 * 1024) * 60_206 / 1_024_000;
    i32::try_from(tenths).unwrap_or(QUIET_DB10)
}

/// The level, 0 to 100, the bar draws for a window of `rms`: linear in
/// dB from [`QUIET_DB10`] to full scale, so loud is tall.
pub fn level_of(rms: u16) -> u8 {
    let db = dbfs10(rms).clamp(QUIET_DB10, 0);
    u8::try_from((db - QUIET_DB10) * 100 / -QUIET_DB10).unwrap_or(100)
}

/// How [`LevelTrack::bars`] folds the windows of one bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    /// The loudest window: a word stands out, as a waveform shows it.
    Peak,
    /// The mean of the windows.
    Mean,
}

/// A recording's loudness along its length: one level, 0 to 100, every
/// [`WINDOW_MS`]. The live bar, the bubble after sending and the
/// receiver's bubble all draw one of these.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LevelTrack {
    levels: Vec<u8>,
}

impl LevelTrack {
    /// A track of these levels as they are.
    pub fn of(levels: Vec<u8>) -> Self {
        let mut track = LevelTrack { levels };
        track.levels.truncate(MOST_WINDOWS);
        track
    }

    /// One more window, from the RMS the platform measured; past
    /// [`MOST_WINDOWS`] nothing is kept.
    pub fn push_rms(&mut self, rms: u16) {
        if self.levels.len() < MOST_WINDOWS {
            self.levels.push(level_of(rms));
        }
    }

    /// The track of decoded sound: `pcm` at `rate`, cut in
    /// [`WINDOW_MS`] windows, the last one short.
    pub fn from_pcm(pcm: &[i16], rate: u32) -> Self {
        let window = usize::try_from(rate / (1000 / WINDOW_MS))
            .unwrap_or(1)
            .max(1);
        let mut track = LevelTrack::default();
        for chunk in pcm.chunks(window) {
            track.push_rms(rms(chunk));
        }
        track
    }

    /// The stand-in of a stream nobody measured: an Ogg Opus stream's
    /// packet sizes over its `duration_ms`, which in a variable rate
    /// stream follow the speech. Each window is the mean of the packets
    /// that fall in it, scaled from the quietest window (8, so a pause
    /// still shows) to the loudest (100); an even stream is 50.
    pub fn from_packets(sizes: &[usize], duration_ms: u32) -> Self {
        if sizes.is_empty() {
            return LevelTrack::default();
        }
        let windows = match duration_ms {
            0 => sizes.len().div_ceil(5) * 2,
            ms => usize::try_from(ms.div_ceil(WINDOW_MS)).unwrap_or(1),
        }
        .clamp(1, MOST_WINDOWS);
        let mut sums = alloc::vec![(0usize, 0usize); windows];
        for (i, &size) in sizes.iter().enumerate() {
            let w = (i * windows / sizes.len()).min(windows - 1);
            sums[w].0 += size;
            sums[w].1 += 1;
        }
        // A window no packet starts in carries the one before.
        let mut means = Vec::with_capacity(windows);
        let mut last = 0;
        for (sum, count) in sums {
            if let Some(mean) = sum.checked_div(count) {
                last = mean;
            }
            means.push(last);
        }
        let floor = means.iter().copied().min().unwrap_or(0);
        let top = means.iter().copied().max().unwrap_or(0);
        LevelTrack {
            levels: means
                .iter()
                .map(|&m| match top > floor {
                    true => u8::try_from(8 + (m - floor) * 92 / (top - floor)).unwrap_or(100),
                    false => 50,
                })
                .collect(),
        }
    }

    /// The levels, oldest first.
    pub fn levels(&self) -> &[u8] {
        &self.levels
    }

    pub fn len(&self) -> usize {
        self.levels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// The newest `n` windows, oldest first: the live bar, whose newest
    /// bar is at the right edge.
    pub fn live(&self, n: usize) -> &[u8] {
        &self.levels[self.levels.len().saturating_sub(n)..]
    }

    /// The track as `n` bars, each folding its share of the windows: the
    /// one resampler. Fewer windows than bars stretch, a window drawn
    /// as several bars; an empty track is no bars.
    pub fn bars(&self, n: usize, fold: Fold) -> Vec<u8> {
        let len = self.levels.len();
        if len == 0 || n == 0 {
            return Vec::new();
        }
        (0..n)
            .map(|bar| {
                let from = (bar * len / n).min(len - 1);
                let to = ((bar + 1) * len / n).clamp(from + 1, len);
                let part = &self.levels[from..to];
                match fold {
                    Fold::Peak => part.iter().copied().max().unwrap_or(0),
                    Fold::Mean => {
                        let sum: usize = part.iter().map(|&l| usize::from(l)).sum();
                        u8::try_from(sum / part.len()).unwrap_or(100)
                    }
                }
            })
            .collect()
    }
}

/// Where a recording is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// No recording: the compose bar is the field.
    #[default]
    Idle,
    /// The finger holds the microphone since the platform's `since`, in
    /// its milliseconds.
    Held { since: u64 },
    /// Locked by a swipe up or the chip; recording since `since`.
    Locked { since: u64 },
    /// Paused by the reader: the recorder holds what it has.
    Paused,
    /// Stopped at the cap: it can be heard, sent or discarded, not
    /// continued.
    Preview,
    /// Gone without a message: the bar slides away.
    Cancelled,
    /// Handed over to go: the bar gives way to the field.
    Sent,
}

impl Phase {
    /// The recorder runs.
    pub fn running(self) -> bool {
        matches!(self, Phase::Held { .. } | Phase::Locked { .. })
    }

    /// The bar is up.
    pub fn shown(self) -> bool {
        matches!(
            self,
            Phase::Held { .. } | Phase::Locked { .. } | Phase::Paused | Phase::Preview
        )
    }

    /// The facade's word.
    pub fn word(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Held { .. } => "held",
            Phase::Locked { .. } => "locked",
            Phase::Paused => "paused",
            Phase::Preview => "preview",
            Phase::Cancelled => "cancelled",
            Phase::Sent => "sent",
        }
    }
}

/// A verb of the recording bar's buttons: what a tap raises
/// ([`super::intent::Intent::Recording`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Verb {
    /// The lock chip, the plain alternative to the swipe up.
    Lock,
    Pause,
    Resume,
    /// Hear what is recorded so far.
    Play,
    StopPlay,
    Send,
    /// The bin.
    Discard,
    /// The silence hint's way to the microphone's permission (1201's
    /// flow).
    AskMicrophone,
}

impl Verb {
    /// The facade's word, and the device walk's resource id.
    pub fn word(self) -> &'static str {
        match self {
            Verb::Lock => "rec_lock",
            Verb::Pause => "rec_pause",
            Verb::Resume => "rec_resume",
            Verb::Play => "rec_play",
            Verb::StopPlay => "rec_stop_play",
            Verb::Send => "rec_send",
            Verb::Discard => "rec_discard",
            Verb::AskMicrophone => "rec_allow",
        }
    }

    /// What the button says.
    pub fn label(self) -> StrId {
        match self {
            Verb::Lock => StrId::RecLock,
            Verb::Pause => StrId::RecPause,
            Verb::Resume => StrId::RecResume,
            Verb::Play => StrId::RecPlay,
            Verb::StopPlay => StrId::RecStopPlay,
            Verb::Send => StrId::RecSend,
            Verb::Discard => StrId::RecDiscard,
            Verb::AskMicrophone => StrId::RecAllow,
        }
    }
}

/// What the platform hands the model; times are the platform's monotonic
/// milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// The finger lands on the microphone.
    Press { now: u64 },
    /// The finger moved while held, in dp from where it landed: left and
    /// up are negative.
    Slide { dx: i32, dy: i32 },
    /// The finger lifts.
    Release { now: u64 },
    /// The system took the gesture away (ACTION_CANCEL): discarded while
    /// held, nothing once locked.
    Cancel,
    /// One more [`WINDOW_MS`] window was recorded, at this RMS.
    Level { now: u64, rms: u16 },
    /// The clock, between windows.
    Tick { now: u64 },
    /// The preview came to its end.
    Played,
    /// A button of the bar.
    Verb(Verb),
}

/// The tick the phone gives, and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Haptic {
    /// The recording started.
    Start,
    /// It locked.
    Lock,
    /// It stopped at the cap.
    Cap,
}

/// What the platform is to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// A short tick under the thumb.
    Haptic(Haptic),
    /// Start the recorder, from nothing.
    Start,
    /// Pause it, keeping what it has.
    Pause,
    /// Go on recording onto what it has.
    Resume,
    /// Stop it for good at the cap, keeping what it has.
    Stop,
    /// Stop it and hand the PCM over to go
    /// ([`super::intent::Intent::SendVoice`]).
    Finish,
    /// Stop it and drop the PCM.
    Discard,
    /// Play what is recorded.
    Play,
    /// Stop playing it.
    StopPlay,
    /// Ask for the microphone (1201's flow).
    AskMicrophone,
}

/// One recording, the one at most a model has: whose chat it is for, its
/// phase, its track and its clock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recording {
    pub peer: [u8; 16],
    pub phase: Phase,
    pub track: LevelTrack,
    /// The milliseconds recorded before the run that goes now.
    before: u32,
    /// The platform's last time.
    now: u64,
    /// A window reached the floor: the microphone is heard.
    pub heard: bool,
    /// How far the held finger slid, dp.
    pub slide: (i32, i32),
    /// The preview plays.
    pub previewing: bool,
    /// What the bar says under the waveform after it stopped, or why a
    /// recording went without a message.
    pub said: Option<Said>,
}

/// What the bar or a toast says about the recording itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Said {
    /// Released too soon: nothing went.
    TooShort,
    /// Stopped at the cap.
    Cap,
}

impl Recording {
    /// The milliseconds recorded, held and locked runs together, never
    /// past the cap.
    pub fn elapsed(&self) -> u32 {
        let run = match self.phase {
            Phase::Held { since } | Phase::Locked { since } => {
                u32::try_from(self.now.saturating_sub(since)).unwrap_or(u32::MAX)
            }
            _ => 0,
        };
        self.before.saturating_add(run).min(LONGEST_MS)
    }

    /// The timer is amber: the last [`AMBER_MS`] before the cap.
    pub fn amber(&self) -> bool {
        self.elapsed() >= LONGEST_MS - AMBER_MS
    }

    /// Nothing heard [`SILENCE_MS`] into the recording: the bar asks
    /// whether the microphone is allowed, and the dot stops pulsing.
    pub fn silent(&self) -> bool {
        !self.heard && self.elapsed() >= SILENCE_MS
    }

    /// The red dot pulses: recording, and heard or not yet doubted.
    pub fn pulsing(&self) -> bool {
        self.phase.running() && !self.silent()
    }

    /// The held finger is past the cancel distance: a release now
    /// discards.
    pub fn armed(&self) -> bool {
        matches!(self.phase, Phase::Held { .. }) && self.slide.0 <= -CANCEL_DP
    }

    /// How far toward cancelling the held finger is, 0 to 100: the hint
    /// moves with it.
    pub fn cancel_progress(&self) -> u8 {
        let left = (-self.slide.0).clamp(0, CANCEL_DP);
        u8::try_from(left * 100 / CANCEL_DP).unwrap_or(100)
    }

    /// How far toward locking, 0 to 100: the chip rises with it.
    pub fn lock_progress(&self) -> u8 {
        let up = (-self.slide.1).clamp(0, LOCK_DP);
        u8::try_from(up * 100 / LOCK_DP).unwrap_or(100)
    }

    /// Fold `input` for `peer`'s chat in; the platform's work comes back.
    /// A press in another chat while one records there does nothing.
    pub fn apply(&mut self, peer: [u8; 16], input: Input) -> Vec<Effect> {
        if let Input::Press { now } = input {
            if self.phase.shown() {
                return Vec::new();
            }
            *self = Recording {
                peer,
                phase: Phase::Held { since: now },
                now,
                ..Recording::default()
            };
            return alloc::vec![Effect::Haptic(Haptic::Start), Effect::Start];
        }
        if peer != self.peer || !self.phase.shown() {
            return Vec::new();
        }
        match input {
            Input::Press { .. } => Vec::new(),
            Input::Slide { dx, dy } => {
                if !matches!(self.phase, Phase::Held { .. }) {
                    return Vec::new();
                }
                self.slide = (dx, dy);
                match dy <= -LOCK_DP && dx > -CANCEL_DP {
                    true => self.lock(),
                    false => Vec::new(),
                }
            }
            Input::Release { now } => match self.phase {
                Phase::Held { .. } => {
                    self.now = self.now.max(now);
                    match self.armed() {
                        true => self.discard(),
                        false => self.finish(),
                    }
                }
                _ => Vec::new(),
            },
            Input::Cancel => match self.phase {
                Phase::Held { .. } => self.discard(),
                _ => Vec::new(),
            },
            Input::Level { now, rms } => {
                if !self.phase.running() {
                    self.now = self.now.max(now);
                    return Vec::new();
                }
                self.track.push_rms(rms);
                self.heard |= rms >= FLOOR_RMS;
                self.tick(now)
            }
            Input::Tick { now } => self.tick(now),
            Input::Played => {
                self.previewing = false;
                Vec::new()
            }
            Input::Verb(verb) => self.verb(verb),
        }
    }

    fn verb(&mut self, verb: Verb) -> Vec<Effect> {
        match (verb, self.phase) {
            (Verb::Lock, Phase::Held { .. }) => self.lock(),
            (Verb::Pause, Phase::Locked { .. }) => {
                self.before = self.elapsed();
                self.phase = Phase::Paused;
                alloc::vec![Effect::Pause]
            }
            (Verb::Resume, Phase::Paused) => {
                self.phase = Phase::Locked { since: self.now };
                let mut effects = self.stop_play();
                effects.push(Effect::Resume);
                effects
            }
            (Verb::Play, Phase::Paused | Phase::Preview) if !self.previewing => {
                self.previewing = true;
                alloc::vec![Effect::Play]
            }
            (Verb::StopPlay, Phase::Paused | Phase::Preview) => self.stop_play(),
            (Verb::Send, Phase::Locked { .. } | Phase::Paused | Phase::Preview) => {
                let mut effects = self.stop_play();
                effects.extend(self.finish());
                effects
            }
            (Verb::Discard, _) => {
                let mut effects = self.stop_play();
                effects.extend(self.discard());
                effects
            }
            (Verb::AskMicrophone, _) => alloc::vec![Effect::AskMicrophone],
            _ => Vec::new(),
        }
    }

    fn lock(&mut self) -> Vec<Effect> {
        let Phase::Held { since } = self.phase else {
            return Vec::new();
        };
        self.phase = Phase::Locked { since };
        self.slide = (0, 0);
        alloc::vec![Effect::Haptic(Haptic::Lock)]
    }

    fn tick(&mut self, now: u64) -> Vec<Effect> {
        self.now = self.now.max(now);
        if !self.phase.running() || self.elapsed() < LONGEST_MS {
            return Vec::new();
        }
        self.before = LONGEST_MS;
        self.phase = Phase::Preview;
        self.said = Some(Said::Cap);
        alloc::vec![Effect::Stop, Effect::Haptic(Haptic::Cap)]
    }

    fn stop_play(&mut self) -> Vec<Effect> {
        match core::mem::take(&mut self.previewing) {
            true => alloc::vec![Effect::StopPlay],
            false => Vec::new(),
        }
    }

    /// Send what there is, or nothing if it is a tap.
    fn finish(&mut self) -> Vec<Effect> {
        if self.elapsed() < SHORTEST_MS {
            self.said = Some(Said::TooShort);
            return self.discard();
        }
        self.before = self.elapsed();
        self.phase = Phase::Sent;
        alloc::vec![Effect::Finish]
    }

    fn discard(&mut self) -> Vec<Effect> {
        self.before = self.elapsed();
        self.phase = Phase::Cancelled;
        alloc::vec![Effect::Discard]
    }

    /// `REC 00:07`: the timer as the bar shows it, whole seconds counted
    /// up.
    pub fn timer_words(&self) -> Text {
        Text::Key(
            StrId::RecTimer,
            alloc::vec![Arg::Str(timer(self.elapsed()))],
        )
    }
}

impl<B> UiModel<B> {
    /// [`super::UiEvent::Record`]: the input goes to the recording; what
    /// the platform is to do comes back, and a tap that sent nothing says
    /// to hold.
    pub(super) fn record(&mut self, peer: [u8; 16], input: Input) -> Vec<UiEffect> {
        let effects = self.recording.apply(peer, input);
        if effects.contains(&Effect::Discard) && self.recording.said == Some(Said::TooShort) {
            self.toast(
                ToastKind::Info,
                Text::key(StrId::VoiceHold).resolve(Lang::En),
            );
        }
        let peer = self.recording.peer;
        effects
            .into_iter()
            .map(|effect| UiEffect::Recording { peer, effect })
            .collect()
    }

    /// The level track a voice bubble draws: the recording's own or the
    /// decoded sound's where there is one.
    pub fn voice_track(&self, peer: [u8; 16], key: u64) -> Option<&LevelTrack> {
        self.voice_tracks.get(&(peer, key))
    }
}

/// `00:07`, `09:59`: elapsed time as a recorder counts it, whole seconds
/// down.
pub fn timer(ms: u32) -> alloc::string::String {
    let secs = ms / 1000;
    alloc::format!("{:02}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const PEER: [u8; 16] = [7; 16];

    /// A recording held since 1000 with `windows` windows of `rms`.
    fn held(windows: u32, rms: u16) -> Recording {
        let mut r = Recording::default();
        assert_eq!(
            r.apply(PEER, Input::Press { now: 1_000 }),
            vec![Effect::Haptic(Haptic::Start), Effect::Start]
        );
        for w in 1..=windows {
            r.apply(
                PEER,
                Input::Level {
                    now: 1_000 + u64::from(w * WINDOW_MS),
                    rms,
                },
            );
        }
        r
    }

    #[test]
    fn hold_and_release_sends() {
        let mut r = held(40, 900);
        assert_eq!(r.phase, Phase::Held { since: 1_000 });
        assert_eq!(r.elapsed(), 2_000);
        assert_eq!(r.timer_words().resolve(Lang::En), "REC 00:02");
        assert!(r.pulsing() && !r.silent());
        assert_eq!(
            r.apply(PEER, Input::Release { now: 3_000 }),
            vec![Effect::Finish]
        );
        assert_eq!(r.phase, Phase::Sent);
        assert_eq!(r.track.len(), 40);
        // A release once sent does nothing more.
        assert_eq!(r.apply(PEER, Input::Release { now: 3_100 }), vec![]);
    }

    #[test]
    fn a_tap_sends_nothing_and_says_to_hold() {
        let mut r = held(5, 900);
        assert_eq!(
            r.apply(PEER, Input::Release { now: 1_300 }),
            vec![Effect::Discard]
        );
        assert_eq!((r.phase, r.said), (Phase::Cancelled, Some(Said::TooShort)));
    }

    #[test]
    fn a_slide_past_the_threshold_and_a_release_cancels() {
        let mut r = held(30, 900);
        assert_eq!(r.apply(PEER, Input::Slide { dx: -50, dy: 0 }), vec![]);
        assert!(!r.armed());
        assert_eq!(r.cancel_progress(), 52);
        // Back again: a release still sends.
        r.apply(PEER, Input::Slide { dx: -95, dy: 0 });
        assert!(!r.armed());
        r.apply(
            PEER,
            Input::Slide {
                dx: -CANCEL_DP,
                dy: -10,
            },
        );
        assert!(r.armed() && r.cancel_progress() == 100);
        assert_eq!(
            r.apply(PEER, Input::Release { now: 2_600 }),
            vec![Effect::Discard]
        );
        assert_eq!((r.phase, r.said), (Phase::Cancelled, None));
        // The system's cancel while held discards too.
        let mut r = held(30, 900);
        assert_eq!(r.apply(PEER, Input::Cancel), vec![Effect::Discard]);
        assert_eq!(r.phase, Phase::Cancelled);
    }

    #[test]
    fn a_swipe_up_locks_and_the_chip_does_too() {
        let mut r = held(10, 900);
        assert_eq!(r.apply(PEER, Input::Slide { dx: 0, dy: -40 }), vec![]);
        assert_eq!(r.lock_progress(), 55);
        assert_eq!(
            r.apply(
                PEER,
                Input::Slide {
                    dx: -5,
                    dy: -LOCK_DP
                }
            ),
            vec![Effect::Haptic(Haptic::Lock)]
        );
        assert_eq!(r.phase, Phase::Locked { since: 1_000 });
        // Lifting the thumb does nothing, nor does the system's cancel.
        assert_eq!(r.apply(PEER, Input::Release { now: 1_600 }), vec![]);
        assert_eq!(r.apply(PEER, Input::Cancel), vec![]);
        assert_eq!(r.phase, Phase::Locked { since: 1_000 });
        // Send sends.
        r.apply(PEER, Input::Tick { now: 4_000 });
        assert_eq!(r.apply(PEER, Input::Verb(Verb::Send)), vec![Effect::Finish]);
        assert_eq!((r.phase, r.elapsed()), (Phase::Sent, 3_000));

        let mut r = held(10, 900);
        assert_eq!(
            r.apply(PEER, Input::Verb(Verb::Lock)),
            vec![Effect::Haptic(Haptic::Lock)]
        );
        assert_eq!(r.phase, Phase::Locked { since: 1_000 });
        assert_eq!(
            r.apply(PEER, Input::Verb(Verb::Discard)),
            vec![Effect::Discard]
        );
        assert_eq!(r.phase, Phase::Cancelled);
    }

    #[test]
    fn pause_and_resume_keep_the_track_and_the_time() {
        let mut r = held(20, 900);
        r.apply(PEER, Input::Verb(Verb::Lock));
        assert_eq!(r.apply(PEER, Input::Verb(Verb::Pause)), vec![Effect::Pause]);
        assert_eq!((r.phase, r.elapsed()), (Phase::Paused, 1_000));
        // Time passes, levels the platform still sends are not kept.
        r.apply(
            PEER,
            Input::Level {
                now: 9_000,
                rms: 900,
            },
        );
        assert_eq!((r.elapsed(), r.track.len()), (1_000, 20));
        assert!(!r.pulsing());
        assert_eq!(r.apply(PEER, Input::Verb(Verb::Play)), vec![Effect::Play]);
        assert!(r.previewing);
        assert_eq!(r.apply(PEER, Input::Verb(Verb::Play)), vec![]);
        // Resume stops the preview and records on from where it was.
        assert_eq!(
            r.apply(PEER, Input::Verb(Verb::Resume)),
            vec![Effect::StopPlay, Effect::Resume]
        );
        assert_eq!(r.phase, Phase::Locked { since: 9_000 });
        for w in 1..=10u64 {
            r.apply(
                PEER,
                Input::Level {
                    now: 9_000 + w * 50,
                    rms: 4_000,
                },
            );
        }
        assert_eq!((r.elapsed(), r.track.len()), (1_500, 30));
        assert_eq!(r.track.levels()[..20], [level_of(900); 20]);
        assert_eq!(r.track.levels()[20..], [level_of(4_000); 10]);
        assert_eq!(r.apply(PEER, Input::Verb(Verb::Pause)), vec![Effect::Pause]);
        assert_eq!(r.apply(PEER, Input::Verb(Verb::Send)), vec![Effect::Finish]);
        assert_eq!(r.phase, Phase::Sent);
    }

    #[test]
    fn the_cap_stops_it_into_the_preview() {
        let mut r = held(1, 900);
        r.apply(PEER, Input::Verb(Verb::Lock));
        r.apply(
            PEER,
            Input::Tick {
                now: 1_000 + u64::from(LONGEST_MS - AMBER_MS) - 1,
            },
        );
        assert!(!r.amber());
        r.apply(
            PEER,
            Input::Tick {
                now: 1_000 + u64::from(LONGEST_MS - AMBER_MS),
            },
        );
        assert!(r.amber());
        assert_eq!(
            r.apply(
                PEER,
                Input::Tick {
                    now: 1_000 + u64::from(LONGEST_MS)
                }
            ),
            vec![Effect::Stop, Effect::Haptic(Haptic::Cap)]
        );
        assert_eq!((r.phase, r.said), (Phase::Preview, Some(Said::Cap)));
        assert_eq!(r.elapsed(), LONGEST_MS);
        assert_eq!(r.timer_words().resolve(Lang::En), "REC 10:00");
        // A preview does not go on; it plays, sends, or goes.
        assert_eq!(r.apply(PEER, Input::Verb(Verb::Resume)), vec![]);
        assert_eq!(r.apply(PEER, Input::Verb(Verb::Play)), vec![Effect::Play]);
        r.apply(PEER, Input::Played);
        assert!(!r.previewing);
        assert_eq!(r.apply(PEER, Input::Verb(Verb::Send)), vec![Effect::Finish]);
        assert_eq!(r.elapsed(), LONGEST_MS);
    }

    #[test]
    fn one_recording_at_a_time_and_only_in_its_chat() {
        let mut r = held(10, 900);
        assert_eq!(r.apply([1; 16], Input::Press { now: 2_000 }), vec![]);
        assert_eq!(r.apply([1; 16], Input::Release { now: 2_000 }), vec![]);
        assert_eq!(r.phase, Phase::Held { since: 1_000 });
        r.apply(PEER, Input::Release { now: 2_000 });
        // Once it went, a press anywhere starts afresh.
        r.apply([1; 16], Input::Press { now: 5_000 });
        assert_eq!((r.peer, r.track.len(), r.elapsed()), ([1; 16], 0, 0));
    }

    /// The floor: digital silence never counts as heard, one step of
    /// dither neither; a quiet room's noise does.
    #[test]
    fn the_silence_detector_waits_two_seconds_at_the_floor() {
        let r = held(39, 0);
        assert!(!r.silent() && r.pulsing(), "not yet two seconds");
        let r = held(40, 0);
        assert!(r.silent() && !r.pulsing());
        let r = held(40, FLOOR_RMS - 1);
        assert!(r.silent());
        let r = held(40, FLOOR_RMS);
        assert!(!r.silent() && r.pulsing());
        // One window heard is enough, early or late.
        let mut r = held(39, 0);
        r.apply(
            PEER,
            Input::Level {
                now: 3_000,
                rms: 600,
            },
        );
        r.apply(PEER, Input::Level { now: 3_050, rms: 0 });
        assert!(!r.silent());
        // A paused recording still doubts, without the pulse.
        let mut r = held(40, 0);
        r.apply(PEER, Input::Verb(Verb::Lock));
        r.apply(PEER, Input::Verb(Verb::Pause));
        assert!(r.silent() && !r.pulsing());
        assert_eq!(
            r.apply(PEER, Input::Verb(Verb::AskMicrophone)),
            vec![Effect::AskMicrophone]
        );
    }

    #[test]
    fn levels_are_decibels() {
        assert_eq!(rms(&[]), 0);
        assert_eq!(rms(&[3, -4, 3, -4]), 3);
        assert_eq!(rms(&[i16::MIN; 8]), 32_768);
        assert_eq!(dbfs10(32_767), 0);
        assert_eq!(dbfs10(16_384), -60);
        assert_eq!(dbfs10(1), -903);
        assert!((-610..=-590).contains(&dbfs10(33)), "{}", dbfs10(33));
        assert_eq!(level_of(0), 0);
        assert_eq!(level_of(FLOOR_RMS), 0);
        assert_eq!(level_of(32_767), 100);
        assert_eq!(level_of(1_036), 57, "-30 dBFS, speech");
        // Louder is never shorter.
        let mut last = 0;
        for x in (1..=u16::MAX).step_by(97) {
            assert!(level_of(x) >= last);
            last = level_of(x);
        }
    }

    #[test]
    fn the_resampler_folds_100_windows_into_40_bars() {
        let track = LevelTrack::of((0..100).map(|i| i as u8).collect());
        let peak = track.bars(40, Fold::Peak);
        let mean = track.bars(40, Fold::Mean);
        assert_eq!(peak.len(), 40);
        // 2.5 windows a bar: bars of 2 and 3 windows in turn.
        assert_eq!(peak[..4], [1, 4, 6, 9]);
        assert_eq!(mean[..4], [0, 3, 5, 8]);
        assert_eq!((peak[39], mean[39]), (99, 98));
        // Every window falls in exactly one bar.
        let counts: usize = (0..40)
            .map(|b| ((b + 1) * 100 / 40).max(b * 100 / 40 + 1) - b * 100 / 40)
            .sum();
        assert_eq!(counts, 100);
        // Fewer windows than bars stretch.
        let short = LevelTrack::of(vec![10, 90]);
        assert_eq!(short.bars(4, Fold::Peak), vec![10, 10, 90, 90]);
        assert_eq!(LevelTrack::default().bars(40, Fold::Peak), Vec::<u8>::new());
        assert_eq!(track.live(3), [97, 98, 99]);
        assert_eq!(short.live(LIVE_WINDOWS), [10, 90]);
    }

    #[test]
    fn a_track_from_pcm_is_the_platforms_windows() {
        // 16 kHz: 800 samples a window; a quiet then a loud window, and a
        // short last one.
        let mut pcm = vec![10i16; 800];
        pcm.extend(core::iter::repeat_n(1_036i16, 800));
        pcm.extend(core::iter::repeat_n(1_036i16, 100));
        let track = LevelTrack::from_pcm(&pcm, 16_000);
        assert_eq!(
            track.levels(),
            [level_of(10), level_of(1_036), level_of(1_036)]
        );
        let mut live = LevelTrack::default();
        for chunk in pcm.chunks(800) {
            live.push_rms(rms(chunk));
        }
        assert_eq!(live, track, "the platform's feed and the decode agree");
    }

    #[test]
    fn a_track_keeps_no_more_than_the_cap() {
        let mut track = LevelTrack::default();
        for _ in 0..MOST_WINDOWS + 50 {
            track.push_rms(500);
        }
        assert_eq!(track.len(), MOST_WINDOWS);
    }

    #[test]
    fn the_timer_counts_up_in_whole_seconds() {
        assert_eq!(timer(0), "00:00");
        assert_eq!(timer(7_999), "00:07");
        assert_eq!(timer(LONGEST_MS - 1), "09:59");
    }
}
