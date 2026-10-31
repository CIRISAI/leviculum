//! The map's autoload (`docs/src/concepts/loquax-map.md`, section 7,
//! decision 3 as Lew revised it on 2026-10-28; leviculum 1089): the regions
//! the model asks for ([`MapWant`]) cut from a PMTiles archive read by HTTP
//! range requests and written into the maps directory, where every painter
//! picks them up ([`crate::picture::Maps`]).
//!
//! - One load at a time, on a thread of its own, so a key never waits for
//!   one; the others queue, a box asked twice once.
//! - A box that failed backs off, 30 s doubling to an hour, so a source that
//!   is down is not asked again on every key.
//! - No internet is said once, not per box, until a load succeeds again.
//! - The region lands atomically: written beside the directory's regions
//!   under a name a painter does not read, checked whole, then renamed.
//! - Autoloaded regions are a cache under a cap, named [`AUTO_PREFIX`],
//!   evicted least recently viewed first (a painter marks a region viewed
//!   by its file's modification time). A file without the prefix was placed
//!   by hand and is never evicted, nor counted.
//!
//! The source is `map_source` from the config, or the newest Protomaps
//! daily build ([`leviculum_mapcut::http::daily`]) found once a session.

use std::collections::VecDeque;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::string::{String, ToString};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Instant, SystemTime};
use std::vec::Vec;

use leviculum_map::region::{Bounds, Region};
use leviculum_mapcut::http::{self, HttpRange};
use leviculum_mapcut::load::{load, Loaded, TooLarge, Want};
use leviculum_mapcut::pmtiles::Archive;

use crate::picture::{is_auto, Maps, AUTO_PREFIX};
use crate::ui::map::MapWant;

/// The most archive bytes one load fetches: the deepest zoom is lowered
/// until the box's tiles come under it. A dense 12 km city box was 20 MB
/// (Berlin-Mitte, 2026-10-28), a region about two fifths of that.
pub const LOAD_CAP: u64 = 64 << 20;
/// The autoloaded regions' cap when the config names none.
pub const CACHE_DEFAULT_MB: u64 = 500;
/// A failed box's first wait before it is asked again.
pub const BACKOFF_FIRST_MS: u64 = 30_000;
/// The longest a failed box waits.
pub const BACKOFF_MAX_MS: u64 = 3_600_000;
/// The layer profile the autoload cuts under, the map tool's default.
const PROFILE: u8 = leviculum_map::profile::CUT_1;
/// What the region's notice names as the program that cut it.
const BY: &str = "loquax";

/// The language of a locale setting (`de_DE.UTF-8` is `de`), `None` for
/// the C and POSIX locales and an empty one.
pub fn language_of(setting: &str) -> Option<String> {
    let lang: String = setting
        .split(['_', '.', '@', '-'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let letters = (2..=3).contains(&lang.len()) && lang.bytes().all(|b| b.is_ascii_lowercase());
    (letters && lang != "c").then_some(lang)
}

/// The user's language, which the autoload cuts names in beside the local
/// ones: the first of `LC_ALL`, `LC_MESSAGES` and `LANG` that is set, as
/// the C library chooses the language of its messages.
pub fn user_language() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .and_then(|v| language_of(&v))
}

/// Where the archive is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The newest daily build under this base URL.
    Daily(String),
    /// This archive.
    Url(String),
}

impl Source {
    /// `map_source` from the config, or the Protomaps daily builds.
    pub fn of(configured: Option<&str>) -> Self {
        match configured {
            Some(url) => Self::Url(url.to_string()),
            None => Self::Daily(http::PROTOMAPS_BUILDS.to_string()),
        }
    }
}

/// What a step of the loader was, for the shell's log and the model.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Started(MapWant),
    Done {
        want: MapWant,
        landed: Landed,
        elapsed_ms: u64,
    },
    Failed {
        want: MapWant,
        detail: String,
        /// The line to show; `None` when it was said already (no internet).
        toast: Option<String>,
        elapsed_ms: u64,
    },
    /// Not loaded: the box is queued or loading already, or backs off.
    Skipped {
        want: MapWant,
        why: &'static str,
    },
}

/// A region that landed.
#[derive(Debug, PartialEq, Eq)]
pub struct Landed {
    /// Its file name in the maps directory.
    pub name: String,
    pub bytes: u64,
    pub zooms: (u8, u8),
    /// Archive bytes fetched for it.
    pub fetched: u64,
    /// Autoloaded regions removed to stay under the cap.
    pub evicted: Vec<String>,
}

/// A failed box's wait.
struct Backoff {
    bounds: Bounds,
    failures: u32,
    until_ms: u64,
}

struct Running {
    want: MapWant,
    started: Instant,
    done: Receiver<Result<Landed, Failure>>,
}

/// Why a load failed, as the thread hands it back.
struct Failure {
    detail: String,
    unreachable: bool,
    too_large: bool,
}

/// The loader the shell holds.
pub struct Loader {
    maps: Maps,
    source: Source,
    cache_bytes: u64,
    /// The daily build found, kept for the session.
    resolved: Arc<Mutex<Option<String>>>,
    queue: VecDeque<MapWant>,
    running: Option<Running>,
    backoff: Vec<Backoff>,
    offline_said: bool,
}

impl Loader {
    pub fn new(maps: Maps, source: Source, cache_bytes: u64) -> Self {
        Self {
            maps,
            source,
            cache_bytes,
            resolved: Arc::new(Mutex::new(None)),
            queue: VecDeque::new(),
            running: None,
            backoff: Vec::new(),
            offline_said: false,
        }
    }

    /// Whether a load runs or waits.
    pub fn busy(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty()
    }

    /// Whether the last load found no internet and none landed since: the
    /// phone's map says "Offline" while it holds (1200).
    pub fn offline(&self) -> bool {
        self.offline_said
    }

    /// Queue `want`, and start it when nothing runs. `now_ms` is the
    /// shell's monotonic clock.
    pub fn ask(&mut self, want: MapWant, now_ms: u64) -> Vec<Step> {
        let same = |w: &MapWant| w.bounds == want.bounds;
        if self.running.as_ref().is_some_and(|r| same(&r.want)) || self.queue.iter().any(same) {
            return vec![Step::Skipped {
                want,
                why: "queued",
            }];
        }
        if self
            .backoff
            .iter()
            .any(|b| b.bounds == want.bounds && now_ms < b.until_ms)
        {
            return vec![Step::Skipped {
                want,
                why: "backoff",
            }];
        }
        if self.maps.dir().is_none() {
            return vec![Step::Skipped {
                want,
                why: "no-maps-dir",
            }];
        }
        self.queue.push_back(want);
        self.start_next()
    }

    /// What finished since the last call, and the next load started.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Step> {
        let Some(running) = self.running.as_ref() else {
            return self.start_next();
        };
        let result = match running.done.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Vec::new(),
            Err(TryRecvError::Disconnected) => Err(Failure {
                detail: "the load stopped".to_string(),
                unreachable: false,
                too_large: false,
            }),
        };
        let Some(running) = self.running.take() else {
            return Vec::new();
        };
        let elapsed_ms = u64::try_from(running.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let want = running.want;
        let mut steps = Vec::new();
        match result {
            Ok(landed) => {
                self.offline_said = false;
                self.backoff.retain(|b| b.bounds != want.bounds);
                self.maps.changed();
                steps.push(Step::Done {
                    want,
                    landed,
                    elapsed_ms,
                });
            }
            Err(failure) => {
                self.back_off(want.bounds, now_ms);
                let toast = match (failure.unreachable, failure.too_large) {
                    (true, _) if self.offline_said => None,
                    (true, _) => {
                        self.offline_said = true;
                        Some("map: no internet; the map shows what is here".to_string())
                    }
                    (false, true) => Some(format!("map: {}; zoom in", failure.detail)),
                    (false, false) => Some(format!("map: {}", failure.detail)),
                };
                steps.push(Step::Failed {
                    want,
                    detail: failure.detail,
                    toast,
                    elapsed_ms,
                });
            }
        }
        steps.extend(self.start_next());
        steps
    }

    fn back_off(&mut self, bounds: Bounds, now_ms: u64) {
        let failures = self
            .backoff
            .iter()
            .find(|b| b.bounds == bounds)
            .map_or(0, |b| b.failures)
            + 1;
        let wait = BACKOFF_FIRST_MS
            .saturating_mul(1 << (failures - 1).min(16))
            .min(BACKOFF_MAX_MS);
        self.backoff.retain(|b| b.bounds != bounds);
        self.backoff.push(Backoff {
            bounds,
            failures,
            until_ms: now_ms.saturating_add(wait),
        });
    }

    fn start_next(&mut self) -> Vec<Step> {
        if self.running.is_some() {
            return Vec::new();
        }
        let (Some(want), Some(dir)) = (self.queue.pop_front(), self.maps.dir()) else {
            return Vec::new();
        };
        let (tx, rx) = channel();
        let job = Job {
            dir: dir.to_path_buf(),
            source: self.source.clone(),
            resolved: Arc::clone(&self.resolved),
            want: want.clone(),
            cache_bytes: self.cache_bytes,
        };
        let spawned = std::thread::Builder::new()
            .name("map-load".to_string())
            .spawn(move || {
                let _ = tx.send(job.run());
            });
        if let Err(error) = spawned {
            return vec![Step::Failed {
                want,
                detail: error.to_string(),
                toast: Some(format!("map: {error}")),
                elapsed_ms: 0,
            }];
        }
        self.running = Some(Running {
            want: want.clone(),
            started: Instant::now(),
            done: rx,
        });
        vec![Step::Started(want)]
    }
}

/// One load, on its thread.
struct Job {
    dir: PathBuf,
    source: Source,
    resolved: Arc<Mutex<Option<String>>>,
    want: MapWant,
    cache_bytes: u64,
}

impl Job {
    fn run(self) -> Result<Landed, Failure> {
        self.load().map_err(|e| Failure {
            unreachable: http::unreachable(e.as_ref()),
            too_large: e.downcast_ref::<TooLarge>().is_some(),
            detail: e.to_string(),
        })
    }

    fn url(&self) -> leviculum_mapcut::Res<String> {
        match &self.source {
            Source::Url(url) => Ok(url.clone()),
            Source::Daily(base) => {
                let mut held = self.resolved.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(url) = held.as_ref() {
                    return Ok(url.clone());
                }
                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                let url = http::daily(base, now)?.url().to_string();
                *held = Some(url.clone());
                Ok(url)
            }
        }
    }

    fn load(&self) -> leviculum_mapcut::Res<Landed> {
        let archive = Archive::from_source(HttpRange::new(&self.url()?))?;
        std::fs::create_dir_all(&self.dir)?;
        let want = Want {
            bounds: self.want.bounds,
            zooms: self.want.zooms.0..=self.want.zooms.1,
            floor: self.want.floor,
            cap: LOAD_CAP,
            profile: PROFILE,
            locale: user_language(),
        };
        // Not `*.lmr`, so no painter reads it half written.
        let part = self.dir.join(format!(
            ".{}.part",
            auto_name(&want.bounds, self.want.zooms)
        ));
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get() / 2)
            .max(1);
        let write = || -> leviculum_mapcut::Res<Loaded> {
            let mut out = BufWriter::new(File::create(&part)?);
            let loaded = load(&archive, &want, BY, threads, &mut out)?;
            let file = out.into_inner().map_err(|e| e.into_error())?;
            file.sync_all()?;
            let region = Region::open(File::open(&part)?).map_err(|e| format!("{e:?}"))?;
            region
                .verify()
                .map_err(|e| format!("the region does not verify: {e:?}"))?;
            Ok(loaded)
        };
        let written = write();
        let loaded = match written {
            Ok(loaded) => loaded,
            Err(error) => {
                let _ = std::fs::remove_file(&part);
                return Err(error);
            }
        };
        let zooms = (*loaded.zooms.start(), *loaded.zooms.end());
        let name = auto_name(&want.bounds, zooms);
        std::fs::rename(&part, self.dir.join(&name))?;
        let evicted = evict(&self.dir, self.cache_bytes, &name)?;
        Ok(Landed {
            bytes: loaded.stats.bytes,
            name,
            zooms,
            fetched: loaded.fetched,
            evicted,
        })
    }
}

/// An autoloaded region's file name: the box in degrees times 10^7 and the
/// zooms, so the same box asked again replaces its file.
pub fn auto_name(b: &Bounds, zooms: (u8, u8)) -> String {
    format!(
        "{AUTO_PREFIX}{}_{}_{}_{}-z{}-{}.lmr",
        b.west, b.south, b.east, b.north, zooms.0, zooms.1
    )
}

/// Remove autoloaded regions of `dir`, least recently viewed first, until
/// they come to `cap` bytes or less; `keep` (the region just loaded) and
/// every file without [`AUTO_PREFIX`] stay. The names removed.
pub fn evict(dir: &Path, cap: u64, keep: &str) -> std::io::Result<Vec<String>> {
    let mut auto: Vec<(SystemTime, String, u64)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_auto(&name) || !name.ends_with(".lmr") {
            continue;
        }
        let meta = entry.metadata()?;
        let viewed = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        auto.push((viewed, name, meta.len()));
    }
    let mut total: u64 = auto.iter().map(|a| a.2).sum();
    auto.sort();
    let mut evicted = Vec::new();
    for (_, name, len) in auto {
        if total <= cap {
            break;
        }
        if name == keep {
            continue;
        }
        std::fs::remove_file(dir.join(&name))?;
        total = total.saturating_sub(len);
        evicted.push(name);
    }
    Ok(evicted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::map::LoadReason;
    use leviculum_map::mercator::World;
    use leviculum_mapcut::serve::Serve;
    use std::time::Duration;

    const FIXTURE: &[u8] = include_bytes!("../../leviculum-mapcut/fixtures/oltrarno.pmtiles");

    fn want(west: i32) -> MapWant {
        MapWant {
            bounds: Bounds {
                west,
                south: 437_405_000,
                east: west + 210_000,
                north: 437_545_000,
            },
            zooms: (14, 15),
            floor: 14,
            reason: LoadReason::View,
            centre: World { x: 0, y: 0 },
        }
    }

    /// The language a region is cut in comes from the locale settings:
    /// the language part, lower case, and none for C or POSIX.
    #[test]
    fn the_cut_language_is_the_locales() {
        for (setting, want) in [
            ("de_DE.UTF-8", Some("de")),
            ("en_GB", Some("en")),
            ("pt-BR", Some("pt")),
            ("fil_PH.UTF-8", Some("fil")),
            ("sr_RS@latin", Some("sr")),
            ("C", None),
            ("C.UTF-8", None),
            ("POSIX", None),
            ("", None),
        ] {
            assert_eq!(language_of(setting).as_deref(), want, "{setting:?}");
        }
    }

    /// Poll until a step that is not `Started` comes, at most ten seconds.
    fn settle(loader: &mut Loader, now_ms: u64) -> Vec<Step> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let steps = loader.poll(now_ms);
            if steps.iter().any(|s| !matches!(s, Step::Started(_))) {
                return steps;
            }
            assert!(Instant::now() < deadline, "no load ended");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_load_lands_atomically_and_bumps_the_epoch() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps.clone(), Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let w = want(112_615_000);
        assert_eq!(loader.ask(w.clone(), 0), [Step::Started(w.clone())]);
        assert_eq!(
            loader.ask(w.clone(), 0),
            [Step::Skipped {
                want: w.clone(),
                why: "queued"
            }]
        );
        let steps = settle(&mut loader, 0);
        let [Step::Done { landed, .. }] = steps.as_slice() else {
            panic!("{steps:?}");
        };
        assert_eq!(landed.name, auto_name(&w.bounds, (14, 15)));
        assert_eq!(maps.epoch(), 1);
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            std::slice::from_ref(&landed.name),
            "no part file left"
        );
        assert_eq!(maps.headers()[0].bounds, w.bounds);
    }

    #[test]
    fn a_failing_source_toasts_once_per_failure_and_backs_off() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        serve.fail(true);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps.clone(), Source::Url(serve.url("o.pmtiles")), 1 << 30);
        let w = want(112_615_000);
        loader.ask(w.clone(), 0);
        let steps = settle(&mut loader, 0);
        let [Step::Failed {
            toast: Some(toast), ..
        }] = steps.as_slice()
        else {
            panic!("{steps:?}");
        };
        assert!(toast.contains("503"), "{toast}");
        let asked = serve.requests();
        // Asked again inside the wait: nothing goes out.
        for now in [1, 10_000, BACKOFF_FIRST_MS - 1] {
            assert_eq!(
                loader.ask(w.clone(), now),
                [Step::Skipped {
                    want: w.clone(),
                    why: "backoff"
                }]
            );
        }
        assert_eq!(serve.requests(), asked, "no retry storm");
        // After it, one try; failing again doubles the wait.
        assert!(matches!(
            loader.ask(w.clone(), BACKOFF_FIRST_MS).as_slice(),
            [Step::Started(_)]
        ));
        settle(&mut loader, BACKOFF_FIRST_MS);
        assert!(matches!(
            loader.ask(w.clone(), 2 * BACKOFF_FIRST_MS).as_slice(),
            [Step::Skipped { why: "backoff", .. }]
        ));
        assert_eq!(maps.epoch(), 0, "nothing landed");
        assert_eq!(std::fs::read_dir(dir.path()).expect("dir").count(), 0);
    }

    #[test]
    fn no_internet_is_said_once() {
        let serve = Serve::start(Vec::new()).expect("serve");
        let url = serve.url("o.pmtiles");
        drop(serve);
        let dir = tempfile::tempdir().expect("tmp");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps, Source::Url(url), 1 << 30);
        let mut toasts = Vec::new();
        for west in [112_615_000, 112_000_000, 111_000_000] {
            loader.ask(want(west), 0);
            for step in settle(&mut loader, 0) {
                if let Step::Failed { toast, .. } = step {
                    toasts.push(toast);
                }
            }
        }
        assert_eq!(toasts.len(), 3);
        assert!(toasts[0]
            .as_deref()
            .is_some_and(|t| t.contains("no internet")));
        assert_eq!(toasts[1..], [None, None]);
    }

    #[test]
    fn eviction_takes_the_least_recently_viewed_and_never_a_hand_placed_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let put = |name: &str, len: usize, age_s: u64| {
            let path = dir.path().join(name);
            std::fs::write(&path, vec![0u8; len]).expect("write");
            let when = SystemTime::now() - Duration::from_secs(age_s);
            File::open(&path)
                .expect("open")
                .set_modified(when)
                .expect("mtime");
        };
        put("berlin.lmr", 5000, 9000);
        put("auto-old.lmr", 1000, 3000);
        put("auto-mid.lmr", 1000, 2000);
        put("auto-new.lmr", 1000, 10);
        put("auto-oldest-but-just-loaded.lmr", 1000, 5000);
        let evicted = evict(dir.path(), 2000, "auto-oldest-but-just-loaded.lmr").expect("evict");
        assert_eq!(evicted, ["auto-old.lmr", "auto-mid.lmr"]);
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "auto-new.lmr",
                "auto-oldest-but-just-loaded.lmr",
                "berlin.lmr"
            ]
        );
        // A cap below the hand-placed file alone takes every other
        // autoloaded region and leaves the hand-placed one.
        let evicted = evict(dir.path(), 0, "none").expect("evict");
        assert_eq!(evicted.len(), 2);
        assert!(dir.path().join("berlin.lmr").exists());
    }
}
