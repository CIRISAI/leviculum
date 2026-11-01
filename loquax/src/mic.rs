//! The desktop's microphone (1276, pass two): what records a voice
//! message in the terminal and the window
//! (`docs/src/concepts/loquax-voice-recording.md`, section 7).
//!
//! The desktop has no recorder of its own, so the system's records: a
//! [`Recorder`] is a program that writes raw mono 16-bit little-endian PCM
//! at [`voice::RATE`] to its standard output. `pw-record` (PipeWire),
//! `parecord` (PulseAudio) and `arecord` (ALSA) are looked for on `PATH`
//! in that order ([`RECORDERS`]); [`RECORDER_ENV`] names another, the
//! whole command line. The program is spawned supervised, so it dies with
//! Loquax, and read on a thread of its own ([`Process`]).
//!
//! The model decides; the microphone only runs what it answers
//! ([`Mic::effect`]: start, pause, resume, stop at the cap, finish,
//! discard, play) and hands it the sound's level: every [`WINDOW_MS`] of
//! samples kept is one [`Input::Level`] with the window's RMS, and the
//! clock between ([`Mic::pump`]). The shell runs both on its loop.
//!
//! The source is a trait ([`Source`]), so a test drives the same
//! microphone with fixed PCM instead of a program ([`Fixture`]).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};

use loquax::ui::recording::{rms, Effect, Input, Verb, WINDOW_MS};
use loquax::ui::strings::Lang;
use loquax::ui::UiEvent;
use loquax::voice::{self, Pcm, LONGEST_MS, RATE};

/// The environment variable that names the recorder: a command line that
/// writes raw mono s16le PCM at 16 kHz to its standard output, split at
/// white space. Unset, the first of [`RECORDERS`] on `PATH` records.
pub(crate) const RECORDER_ENV: &str = "LOQUAX_RECORDER";

/// The recorders looked for, in order, each with the flags that make it
/// write raw mono s16le PCM at [`voice::RATE`] to its standard output:
/// `pw-cat(1)` ("When the FILE is - ... raw data from STDIN and STDOUT"),
/// `pacat(1)` (`--record` "to STDOUT if none is specified", `--raw`),
/// `arecord(1)` (`-t raw`, "the standard output ... is used" without a
/// file name).
pub(crate) const RECORDERS: [(&str, &[&str]); 3] = [
    (
        "pw-record",
        &["--rate", "16000", "--channels", "1", "--format", "s16", "-"],
    ),
    (
        "parecord",
        &["--raw", "--format=s16le", "--rate=16000", "--channels=1"],
    ),
    (
        "arecord",
        &["-q", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c", "1"],
    ),
];

/// The samples of one level window.
const WINDOW: usize = (RATE / (1000 / WINDOW_MS)) as usize;

/// The most samples a recording keeps: the cap and a second, as the
/// level track keeps its windows ([`loquax::ui::recording::MOST_WINDOWS`]).
const MOST_SAMPLES: usize = ((LONGEST_MS as u64 + 1_000) * RATE as u64 / 1_000) as usize;

/// How much of a recorder's standard error is kept for the notice.
const STDERR_KEPT: usize = 4_096;

/// A program that records: its name for the notices and its command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recorder {
    pub name: String,
    pub argv: Vec<String>,
}

impl Recorder {
    /// The recorder of this host: [`RECORDER_ENV`]'s command line when it
    /// names one, else the first of [`RECORDERS`] found on `path`.
    pub(crate) fn find(env: Option<&str>, path: Option<&std::ffi::OsStr>) -> Option<Recorder> {
        let argv: Vec<String> = env
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        if let Some(program) = argv.first() {
            return Some(Recorder {
                name: Path::new(program)
                    .file_name()
                    .map_or_else(|| program.clone(), |n| n.to_string_lossy().into_owned()),
                argv: argv.clone(),
            });
        }
        let dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(p).collect())?;
        RECORDERS.iter().find_map(|(name, flags)| {
            let program = dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())?;
            let mut argv = vec![program.to_string_lossy().into_owned()];
            argv.extend(flags.iter().map(|f| f.to_string()));
            Some(Recorder {
                name: name.to_string(),
                argv,
            })
        })
    }

    /// This process's recorder: its environment and its `PATH`.
    pub(crate) fn of_process() -> Option<Recorder> {
        let env = std::env::var(RECORDER_ENV).ok();
        Recorder::find(env.as_deref(), std::env::var_os("PATH").as_deref())
    }
}

/// Where the samples come from: a recording program, or a test's PCM.
pub(crate) trait Source: Send {
    /// The samples that arrived since the last call, mono at
    /// [`voice::RATE`].
    fn take(&mut self) -> Vec<i16>;
    /// Why it stopped giving samples, said once: the program ended or
    /// could not be read.
    fn ended(&mut self) -> Option<String>;
    /// Stop it for good.
    fn stop(&mut self);
}

/// Opens a source for a new recording; an error is the notice's words.
pub(crate) type Opener = Box<dyn FnMut() -> Result<Box<dyn Source>, String> + Send>;

/// A recording program, its standard output read on a thread into
/// samples, its standard error kept for the notice.
pub(crate) struct Process {
    name: String,
    child: Child,
    samples: Receiver<Vec<i16>>,
    /// What [`Source::ended`] read while looking for the end.
    unread: Vec<i16>,
    stderr: Arc<Mutex<String>>,
    /// The standard error was read to its end.
    stderr_done: Arc<std::sync::atomic::AtomicBool>,
    /// The calls since the program was seen to have exited: its words are
    /// waited for a few, never for ever (a child of its own may hold the
    /// pipe).
    exited_for: u32,
    said: bool,
}

impl Process {
    /// `recorder` started, supervised: it dies with Loquax.
    pub(crate) fn start(recorder: &Recorder) -> Result<Process, String> {
        let (program, args) = recorder
            .argv
            .split_first()
            .ok_or_else(|| format!("{RECORDER_ENV} names no program"))?;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = leviculum_std::process::spawn_supervised(cmd)
            .map_err(|e| format!("{} did not start: {e}", recorder.name))?;
        let (tx, samples) = channel();
        if let Some(mut out) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut bytes = [0u8; 4_096];
                let mut odd: Option<u8> = None;
                loop {
                    let n = match out.read(&mut bytes) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let mut chunk = Vec::with_capacity(n / 2 + 1);
                    let mut at = 0;
                    if let Some(low) = odd.take() {
                        chunk.push(i16::from_le_bytes([low, bytes[0]]));
                        at = 1;
                    }
                    let pairs = bytes[at..n].chunks_exact(2);
                    odd = pairs.remainder().first().copied();
                    chunk.extend(pairs.map(|p| i16::from_le_bytes([p[0], p[1]])));
                    if tx.send(chunk).is_err() {
                        break;
                    }
                }
            });
        }
        let stderr = Arc::new(Mutex::new(String::new()));
        let stderr_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Some(mut err) = child.stderr.take() {
            let kept = Arc::clone(&stderr);
            let done = Arc::clone(&stderr_done);
            std::thread::spawn(move || {
                let mut bytes = [0u8; 512];
                while let Ok(n) = err.read(&mut bytes) {
                    if n == 0 {
                        break;
                    }
                    let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
                    if kept.len() < STDERR_KEPT {
                        kept.push_str(&String::from_utf8_lossy(&bytes[..n]));
                    }
                }
                done.store(true, std::sync::atomic::Ordering::SeqCst);
            });
        } else {
            stderr_done.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(Process {
            name: recorder.name.clone(),
            child,
            samples,
            unread: Vec::new(),
            stderr,
            stderr_done,
            exited_for: 0,
            said: false,
        })
    }
}

impl Source for Process {
    fn take(&mut self) -> Vec<i16> {
        let mut out = std::mem::take(&mut self.unread);
        while let Ok(chunk) = self.samples.try_recv() {
            out.extend(chunk);
        }
        out
    }

    fn ended(&mut self) -> Option<String> {
        if self.said {
            return None;
        }
        // Ended only once its output is read to the end and it exited.
        match self.samples.try_recv() {
            Ok(chunk) => {
                self.unread.extend(chunk);
                return None;
            }
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {}
        }
        let status = self.child.try_wait().ok().flatten()?;
        self.exited_for += 1;
        if !self.stderr_done.load(std::sync::atomic::Ordering::SeqCst) && self.exited_for < 50 {
            return None;
        }
        self.said = true;
        let stderr = self
            .stderr
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let why = stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map_or_else(|| status.to_string(), str::to_string);
        Some(format!("{} stopped: {why}", self.name))
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A test's source: fixed PCM, as much a call as the test's clock made
/// due since the last ([`Fixture::due`]), nothing past its end; it never
/// ends by itself.
#[cfg(test)]
pub(crate) struct Fixture {
    pub pcm: Vec<i16>,
    pub at: usize,
    /// The samples recorded since the last call: the test adds a
    /// window's worth for every 50 ms it runs.
    pub due: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl Source for Fixture {
    fn take(&mut self) -> Vec<i16> {
        let due = self.due.swap(0, std::sync::atomic::Ordering::SeqCst);
        let to = (self.at + due).min(self.pcm.len());
        let out = self.pcm[self.at..to].to_vec();
        self.at = to;
        out
    }

    fn ended(&mut self) -> Option<String> {
        None
    }

    fn stop(&mut self) {}
}

/// What one effect asks of the shell after the microphone ran it.
#[derive(Debug, Default)]
pub(crate) struct Done {
    /// Events for the model: the voice message to send, a notice, the
    /// preview's end, a recording that cannot record discarded.
    pub events: Vec<UiEvent>,
    /// A WAV file for the system's player (the preview).
    pub play: Option<PathBuf>,
}

/// What one pump of the microphone hands the model.
#[derive(Debug, Default)]
pub(crate) struct Pumped {
    /// Whose recording it is.
    pub peer: [u8; 16],
    /// The windows' levels and the clock.
    pub inputs: Vec<Input>,
    /// The recorder stopped by itself, in its own words: said once.
    pub stopped: Option<String>,
}

/// The microphone: the source of the recording that runs, its PCM, and
/// where its next level window starts.
pub(crate) struct Mic {
    /// The recorder's name, for the notices: `pw-record`; none when this
    /// host has none.
    name: Option<String>,
    open: Opener,
    source: Option<Box<dyn Source>>,
    peer: [u8; 16],
    /// Samples arriving now are the recording's: off while paused, after
    /// the cap and once it ended.
    keeping: bool,
    pcm: Vec<i16>,
    /// The first sample of `pcm` not yet in a level window.
    windowed: usize,
    /// What the recorder said when it stopped, for the hint's notice.
    failed: Option<String>,
}

impl Mic {
    /// The microphone of this host: [`Recorder::of_process`], or one that
    /// says it has none when started.
    pub(crate) fn of_process() -> Mic {
        let recorder = Recorder::of_process();
        let name = recorder.as_ref().map(|r| r.name.clone());
        let open: Opener = Box::new(move || match &recorder {
            Some(recorder) => Process::start(recorder).map(|p| Box::new(p) as Box<dyn Source>),
            None => Err(format!(
                "No recorder found: install pipewire-bin (pw-record), pulseaudio-utils \
                 (parecord) or alsa-utils (arecord), or name one in {RECORDER_ENV}"
            )),
        });
        Mic::new(name, open)
    }

    /// A microphone over `open`, its recorder called `name`.
    pub(crate) fn new(name: Option<String>, open: Opener) -> Mic {
        Mic {
            name,
            open,
            source: None,
            peer: [0; 16],
            keeping: false,
            pcm: Vec::new(),
            windowed: 0,
            failed: None,
        }
    }

    /// Whether this host has a recorder: the window offers the microphone
    /// only then.
    pub(crate) fn available(&self) -> bool {
        self.name.is_some()
    }

    /// The recording's samples so far.
    #[cfg(test)]
    pub(crate) fn pcm(&self) -> &[i16] {
        &self.pcm
    }

    /// Run `effect` of `peer`'s recording; `home` is where the preview's
    /// WAV file goes.
    pub(crate) fn effect(&mut self, peer: [u8; 16], effect: Effect, home: &Path) -> Done {
        let mut done = Done::default();
        match effect {
            Effect::Start => {
                self.end();
                self.pcm.clear();
                self.windowed = 0;
                self.failed = None;
                self.peer = peer;
                match (self.open)() {
                    Ok(source) => {
                        self.source = Some(source);
                        self.keeping = true;
                    }
                    // Nothing records: said, and the bar goes.
                    Err(line) => {
                        tracing::warn!("RECORD start failed: {line}");
                        done.events.push(UiEvent::Notice(line));
                        done.events.push(UiEvent::Record {
                            peer,
                            input: Input::Verb(Verb::Discard),
                        });
                    }
                }
            }
            // The program keeps running while paused; what it gives
            // meanwhile is dropped, so the recording goes on where it
            // stood.
            Effect::Pause => {
                self.drain();
                self.keeping = false;
            }
            Effect::Resume => self.keeping = true,
            Effect::Stop => {
                self.drain();
                self.end();
            }
            Effect::Finish => {
                self.drain();
                self.end();
                let pcm = Pcm {
                    rate: RATE,
                    samples: std::mem::take(&mut self.pcm),
                };
                self.windowed = 0;
                done.events.extend(finished(peer, &pcm));
            }
            Effect::Discard => {
                self.end();
                self.pcm.clear();
                self.windowed = 0;
            }
            Effect::Play => {
                match self.preview(home) {
                    Ok(path) => done.play = Some(path),
                    Err(line) => done.events.push(UiEvent::Notice(line)),
                }
                // The system's player tells no end: the button goes back
                // at once, as a received message's does.
                done.events.push(UiEvent::Record {
                    peer,
                    input: Input::Played,
                });
            }
            Effect::AskMicrophone => done.events.push(UiEvent::Notice(self.allow_words())),
            Effect::StopPlay | Effect::Haptic(_) => {}
        }
        done
    }

    /// The level of every whole window kept since the last call, then the
    /// clock at `now`, for the recording's peer; nothing while none runs.
    pub(crate) fn pump(&mut self, now: u64) -> Pumped {
        let Some(source) = self.source.as_mut() else {
            return Pumped::default();
        };
        let samples = source.take();
        let mut inputs = Vec::new();
        if self.keeping {
            let room = MOST_SAMPLES.saturating_sub(self.pcm.len());
            self.pcm.extend(samples.into_iter().take(room));
            while self.pcm.len() - self.windowed >= WINDOW {
                let window = &self.pcm[self.windowed..self.windowed + WINDOW];
                inputs.push(Input::Level {
                    now,
                    rms: rms(window),
                });
                self.windowed += WINDOW;
            }
        }
        // The clock goes on while paused too: the recording resumes from
        // the time it is resumed at, not from the time it was paused at.
        inputs.push(Input::Tick { now });
        let stopped = source.ended();
        if let Some(line) = &stopped {
            tracing::warn!("RECORD {line}");
            self.failed = Some(line.clone());
        }
        Pumped {
            peer: self.peer,
            inputs,
            stopped,
        }
    }

    /// Keep what arrived and is still unread.
    fn drain(&mut self) {
        if !self.keeping {
            return;
        }
        if let Some(source) = self.source.as_mut() {
            let room = MOST_SAMPLES.saturating_sub(self.pcm.len());
            self.pcm.extend(source.take().into_iter().take(room));
        }
    }

    /// Stop the program; nothing more is kept.
    fn end(&mut self) {
        if let Some(mut source) = self.source.take() {
            source.stop();
        }
        self.keeping = false;
    }

    /// The recording so far as a WAV file under `home`'s `voice/`, for the
    /// system's player: one file, written over by the next preview.
    fn preview(&self, home: &Path) -> Result<PathBuf, String> {
        let dir = home.join(voice::DIR);
        let path = dir.join(format!(
            "recording-{}.wav",
            loquax::address::to_hex(&self.peer)
        ));
        let wav = voice::wav(&Pcm {
            rate: RATE,
            samples: self.pcm.clone(),
        });
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, wav))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }

    /// The hint's notice: which recorder hears nothing, what it said if it
    /// stopped, and how a desktop lets the microphone through.
    fn allow_words(&self) -> String {
        let name = self.name.as_deref().unwrap_or("the recorder");
        let said = match &self.failed {
            Some(line) => format!("{line}. "),
            None => format!("{name} hears only silence. "),
        };
        format!(
            "{said}Unmute the microphone and choose it as the input in the sound settings \
             (pavucontrol, Recording; or wpctl status), check its mute switch, and allow the \
             microphone where a sandbox (Flatpak, Snap) asks; {RECORDER_ENV} names another \
             recorder."
        )
    }
}

impl Drop for Mic {
    fn drop(&mut self) {
        self.end();
    }
}

/// What a finished recording becomes: the voice message, coded as Opus in
/// Ogg, or the reason it does not go.
fn finished(peer: [u8; 16], pcm: &Pcm) -> Vec<UiEvent> {
    if let Some(why) = loquax::ui::voice::refusal(pcm.ms()) {
        return vec![UiEvent::Notice(why.resolve(Lang::En))];
    }
    match voice::encode(&pcm.samples, pcm.rate) {
        Ok(data) => {
            tracing::info!(
                "RECORD sent ms={} bytes={} peer={}",
                pcm.ms(),
                data.len(),
                loquax::address::to_hex(&peer)
            );
            vec![UiEvent::SendVoice { peer, data }]
        }
        Err(error) => vec![UiEvent::Notice(format!(
            "the voice message was not coded: {error}"
        ))],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loquax::ui::recording::{Fold, LevelTrack, Phase};
    use loquax::ui::scene::{LayoutClass, NodeId};
    use loquax::ui::strings::StrId;
    use loquax::ui::{update, UiEffect};

    use crate::shell::Model;

    const PEER: [u8; 16] = [0x5e; 16];

    type Due = Arc<std::sync::atomic::AtomicUsize>;

    /// A microphone whose recording is `pcm`, as fast as the test's clock
    /// says.
    fn fixture(pcm: Vec<i16>) -> (Mic, Due) {
        let due = Due::default();
        let clock = Arc::clone(&due);
        let mut pcm = Some(pcm);
        let mic = Mic::new(
            Some("fixture".to_string()),
            Box::new(move || {
                Ok(Box::new(Fixture {
                    pcm: pcm.take().unwrap_or_default(),
                    at: 0,
                    due: Arc::clone(&clock),
                }) as Box<dyn Source>)
            }),
        );
        (mic, due)
    }

    /// The model with `PEER`'s chat open, and what it asked to send.
    struct Bench {
        model: Model,
        mic: Mic,
        due: Due,
        home: tempfile::TempDir,
        sent: Vec<UiEffect>,
        notices: Vec<String>,
        played: Vec<PathBuf>,
        now: u64,
    }

    impl Bench {
        fn new((mic, due): (Mic, Due)) -> Bench {
            let mut model = Model::new(&[PEER]);
            update(&mut model, UiEvent::Open { peer: PEER });
            Bench {
                model,
                mic,
                due,
                home: tempfile::tempdir().expect("a temp home"),
                sent: Vec::new(),
                notices: Vec::new(),
                played: Vec::new(),
                now: 1_000,
            }
        }

        /// `event` folded, and the microphone run on what comes back, as
        /// the shell runs it.
        fn apply(&mut self, event: UiEvent) {
            if let UiEvent::Notice(line) = &event {
                self.notices.push(line.clone());
            }
            for effect in update(&mut self.model, event) {
                match effect {
                    UiEffect::Recording { peer, effect } => {
                        let done = self.mic.effect(peer, effect, self.home.path());
                        self.played.extend(done.play);
                        for event in done.events {
                            self.apply(event);
                        }
                    }
                    send @ UiEffect::Send { .. } => self.sent.push(send),
                    _ => {}
                }
            }
        }

        fn input(&mut self, input: Input) {
            self.apply(UiEvent::Record { peer: PEER, input });
        }

        /// Start locked, as the terminal's key does.
        fn start(&mut self) {
            self.input(Input::Press { now: self.now });
            self.input(Input::Verb(Verb::Lock));
        }

        /// `ms` of the shell's loop: a window recorded and a pump every
        /// 50 ms.
        fn run(&mut self, ms: u64) {
            for _ in 0..ms / u64::from(WINDOW_MS) {
                self.now += u64::from(WINDOW_MS);
                self.due
                    .fetch_add(WINDOW, std::sync::atomic::Ordering::SeqCst);
                let pumped = self.mic.pump(self.now);
                for input in pumped.inputs {
                    self.apply(UiEvent::Record {
                        peer: pumped.peer,
                        input,
                    });
                }
            }
        }
    }

    #[test]
    fn the_recorders_write_raw_mono_at_the_coders_rate() {
        let rate = RATE.to_string();
        for (name, flags) in RECORDERS {
            assert!(
                flags.iter().any(|f| f.ends_with(rate.as_str())),
                "{name}: {flags:?}"
            );
        }
        assert_eq!(WINDOW, 800);
    }

    #[test]
    fn the_env_names_the_recorder_else_the_first_on_the_path() {
        let dir = tempfile::tempdir().expect("a dir");
        for name in ["arecord", "parecord"] {
            std::fs::write(dir.path().join(name), "").expect("a program");
        }
        let path = std::env::join_paths([dir.path()]).expect("a PATH");
        let found = Recorder::find(None, Some(&path)).expect("one found");
        assert_eq!(
            found.name, "parecord",
            "pw-record is missing, parecord next"
        );
        assert_eq!(
            found.argv[1..],
            ["--raw", "--format=s16le", "--rate=16000", "--channels=1"]
        );
        let named = Recorder::find(Some("/opt/rec -x 1"), Some(&path)).expect("named");
        assert_eq!(named.name, "rec");
        assert_eq!(named.argv, ["/opt/rec", "-x", "1"]);
        let blank = Recorder::find(Some("  "), Some(&path)).expect("the PATH's");
        assert_eq!(blank.name, "parecord", "a blank variable is unset");
        let empty = tempfile::tempdir().expect("a dir");
        let none = std::env::join_paths([empty.path()]).expect("a PATH");
        assert_eq!(Recorder::find(None, Some(&none)), None);
    }

    /// Three seconds of speech, locked, sent: one voice message goes, its
    /// PCM is the source's, and its bubble draws the recording's own
    /// track through the one resampler.
    #[test]
    fn a_three_second_recording_sends_one_voice_message_with_its_track() {
        let speech = voice::test_speech(RATE, 3_000);
        let mut bench = Bench::new(fixture(speech.clone()));
        bench.start();
        assert!(matches!(bench.model.recording.phase, Phase::Locked { .. }));
        bench.run(3_000);
        assert_eq!(bench.mic.pcm(), speech.as_slice());
        assert_eq!(bench.model.recording.track.len(), 60);
        let track = bench.model.recording.track.clone();
        assert_eq!(track, LevelTrack::from_pcm(&speech, RATE));
        bench.input(Input::Verb(Verb::Send));
        assert_eq!(bench.model.recording.phase, Phase::Sent);
        assert_eq!(bench.sent.len(), 1, "one message: {:?}", bench.sent);
        let UiEffect::Send { to, body, fields } = &bench.sent[0] else {
            unreachable!()
        };
        assert_eq!((*to, body.as_str()), (PEER, ""));
        let (mode, data) = loquax::ui::voice::voice_parts(fields).expect("a voice field");
        let back = voice::decode(mode, data).expect("it decodes");
        assert!((2_950..=3_050).contains(&back.ms()), "{} ms", back.ms());
        let scene = bench.model.scene(LayoutClass::Wide);
        let chat = scene.chat.expect("the chat");
        let bubble = chat
            .groups
            .iter()
            .flat_map(|g| &g.bubbles)
            .find_map(|b| b.voice.as_ref())
            .expect("a voice bubble");
        assert_eq!(bubble.levels, track.bars(32, Fold::Peak));
        assert!(chat.compose.recording.is_none(), "the bar gave way");
    }

    /// Paused, the program's samples are dropped; resumed, the recording
    /// goes on onto the same PCM, and the track and the timer with it.
    #[test]
    fn pause_and_resume_keep_one_continuous_recording() {
        let speech = voice::test_speech(RATE, 4_000);
        let mut bench = Bench::new(fixture(speech.clone()));
        bench.start();
        bench.run(1_000);
        bench.input(Input::Verb(Verb::Pause));
        assert_eq!(bench.model.recording.phase, Phase::Paused);
        bench.run(1_000);
        assert_eq!(bench.mic.pcm().len(), 16_000, "nothing kept while paused");
        bench.input(Input::Verb(Verb::Resume));
        bench.run(1_000);
        let pcm = bench.mic.pcm();
        assert_eq!(pcm.len(), 32_000);
        assert_eq!(&pcm[..16_000], &speech[..16_000]);
        // The second second is the source's third: the paused one dropped.
        assert_eq!(&pcm[16_000..], &speech[32_000..48_000]);
        assert_eq!(bench.model.recording.track.len(), 40);
        assert_eq!(bench.model.recording.elapsed(), 2_000);
        bench.input(Input::Verb(Verb::Send));
        assert_eq!(bench.sent.len(), 1);
    }

    /// The bin: nothing goes, nothing is kept.
    #[test]
    fn a_discarded_recording_sends_nothing() {
        let mut bench = Bench::new(fixture(voice::test_speech(RATE, 3_000)));
        bench.start();
        bench.run(2_000);
        bench.input(Input::Verb(Verb::Discard));
        assert_eq!(bench.model.recording.phase, Phase::Cancelled);
        assert!(bench.sent.is_empty());
        assert!(bench.mic.pcm().is_empty());
        bench.run(500);
        assert!(bench.sent.is_empty());
    }

    /// Two seconds of digital silence: the bar's line asks whether the
    /// microphone is allowed, and its button says which recorder hears
    /// nothing and how to let the microphone through.
    #[test]
    fn a_silent_source_shows_the_line_and_says_how_to_allow_it() {
        let mut bench = Bench::new(fixture(vec![0; 16_000 * 3]));
        bench.start();
        bench.run(1_950);
        let bar = |bench: &Bench| {
            bench
                .model
                .scene(LayoutClass::Wide)
                .chat
                .and_then(|c| c.compose.recording)
                .expect("the bar")
        };
        assert!(bar(&bench).notice.is_none(), "not yet at 1.95 s");
        bench.run(50);
        let notice = bar(&bench).notice.expect("the line");
        assert_eq!(notice.id, NodeId::RecNotice);
        assert_eq!(
            notice.words.resolve(Lang::En),
            StrId::RecSilent.text(Lang::En)
        );
        assert!(!bar(&bench).pulsing);
        bench.input(Input::Verb(Verb::AskMicrophone));
        let said = bench.notices.last().expect("a notice");
        assert!(said.starts_with("fixture hears only silence."), "{said}");
        assert!(said.contains(RECORDER_ENV), "{said}");
    }

    /// Paused, Play writes what is there as a WAV file for the system's
    /// player, and the button comes back.
    #[test]
    fn play_writes_the_recording_for_the_player() {
        let mut bench = Bench::new(fixture(voice::test_speech(RATE, 2_000)));
        bench.start();
        bench.run(1_000);
        bench.input(Input::Verb(Verb::Pause));
        bench.input(Input::Verb(Verb::Play));
        let path = bench.played.last().expect("a file to play").clone();
        let wav = std::fs::read(&path).expect("the file");
        assert_eq!(wav.len(), 44 + 2 * 16_000);
        assert!(!bench.model.recording.previewing);
    }

    /// No recorder: the start says so and the bar goes.
    #[test]
    fn no_recorder_says_so_and_discards() {
        let mic = Mic::new(None, Box::new(|| Err("No recorder found".to_string())));
        assert!(!mic.available());
        let mut bench = Bench::new((mic, Due::default()));
        bench.start();
        assert_eq!(bench.model.recording.phase, Phase::Cancelled);
        assert_eq!(bench.notices, ["No recorder found"]);
        assert!(bench.sent.is_empty());
    }

    /// A recorder that cannot reach a sound server: the program's own
    /// words come back as the reason, once. A shell script stands in for
    /// it, so the test needs no sound server.
    #[test]
    fn a_recorder_that_fails_says_why_once() {
        let recorder = Recorder {
            name: "sh".to_string(),
            argv: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 'error: no sound server' >&2; exit 1".to_string(),
            ],
        };
        let mut process = Process::start(&recorder).expect("sh starts");
        let mut said = None;
        for _ in 0..200 {
            assert!(process.take().is_empty());
            said = process.ended();
            if said.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(said.as_deref(), Some("sh stopped: error: no sound server"));
        assert_eq!(process.ended(), None, "said once");
    }

    /// A program's bytes split anywhere still make the same samples.
    #[test]
    fn a_program_s_samples_are_its_bytes_in_order() {
        let recorder = Recorder {
            name: "printf".to_string(),
            argv: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf '\\001\\000\\377\\377\\000\\200'".to_string(),
            ],
        };
        let mut process = Process::start(&recorder).expect("sh starts");
        let mut samples = Vec::new();
        for _ in 0..200 {
            samples.extend(process.take());
            if process.ended().is_some() {
                samples.extend(process.take());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(samples, [1, -1, i16::MIN]);
    }
}
