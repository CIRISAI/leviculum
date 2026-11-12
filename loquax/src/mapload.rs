//! The map's autoload (`docs/src/concepts/loquax-map.md`, section 7,
//! decision 3 as Lew revised it on 2026-10-28; leviculum 1089): the regions
//! the model asks for ([`MapWant`]) cut from a PMTiles archive read by HTTP
//! range requests and written into the maps directory, where every painter
//! picks them up ([`crate::picture::Maps`]).
//!
//! - One load at a time, on a thread of its own, so a key never waits for
//!   one; the others queue, a box asked twice once.
//! - The queue is ordered by what is on screen (1292,
//!   [`crate::ui::viewload`]): a box asked for by hand or for the screen
//!   first, then the boxes next to it, then those the view left, at most
//!   [`MAX_AWAY`] of them. A load whose area left the screen cannot be
//!   paused (the cut reads the archive in one call), so a load for the
//!   screen starts beside it, two at most.
//! - The screen's own box at its zoom (1527, [`LoadReason::Screen`]) goes
//!   before the cells under the screen and starts beside any load but
//!   another for the screen; a cell for the screen waits until it landed,
//!   so the screen's tiles draw before a byte of the cell arrives. A
//!   screen's box the view left is dropped while it waits: the cells
//!   hold its area. A screen's box a running load brings is not asked
//!   twice, and that load ranks as the screen's, so the cells wait for
//!   it too (1616).
//! - A box that failed backs off, 30 s doubling to an hour, so a source that
//!   is down is not asked again on every key. A box that found no internet
//!   does not: the next view asks again, and "offline" holds only until a
//!   load starts.
//! - No internet is said once, not per box, until a load succeeds again.
//! - The region is written in place under its own name, as its tiles
//!   arrive (1291): coarse to fine, a round at a time, and a painter draws
//!   every tile there, the nearest coarser one standing in for the rest
//!   ([`leviculum_map::region::Region::stand_in`]). Each round bumps
//!   [`Maps::progress`], at most every [`PROGRESS_EVERY_MS`], so a frame
//!   on screen draws again; once complete it is checked whole and lands
//!   ([`Maps::changed`], by the shell). A load that fails removes its file.
//! - A load whose name is taken, a region installed before that it
//!   replaces, writes beside it under [`PART_SUFFIX`] and renames over it
//!   once the new file verifies (1727): the old region draws until then,
//!   and a load that fails leaves it as it was.
//! - Autoloaded regions are a cache under a cap, named [`AUTO_PREFIX`],
//!   evicted least recently viewed first (a painter marks a region viewed
//!   by its file's modification time). A file without the prefix was placed
//!   by hand and is never evicted, nor counted.
//!
//! The source is `map_source` from the config, or the newest Protomaps
//! daily build ([`leviculum_mapcut::http::daily`]) found once a session.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::string::{String, ToString};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Instant, SystemTime};
use std::vec::Vec;

use leviculum_map::region::{Bounds, Region};
use leviculum_mapcut::cut::Progress;
use leviculum_mapcut::http::{self, HttpRange};
use leviculum_mapcut::load::{cut_planned, plan, Loaded, TooLarge, Want};
use leviculum_mapcut::pmtiles::Archive;

use crate::picture::{is_auto, Maps, AUTO_PREFIX};
use crate::ui::map::{LoadReason, MapWant};

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
/// The most loads that run at once: one for an area the view left, which
/// runs on, and one for the screen.
pub const MAX_RUNNING: usize = 2;
/// The most queued boxes the view left that are kept, the newest.
pub const MAX_AWAY: usize = 8;
/// The layer profile the autoload cuts under, the map tool's default:
/// profile 2 (1707), whose deepest zoom keeps the source's geometry. A
/// region cut the old way no longer covers the screen
/// ([`crate::ui::viewload`]), so an autoloaded one is asked for again
/// and replaced in place under its own name.
const PROFILE: u8 = leviculum_map::profile::CUT_2;
/// The suffix of a region written beside the one it replaces (1727): not
/// `.lmr`, so no painter opens it and no eviction counts it.
const PART_SUFFIX: &str = ".part";
/// What the region's notice names as the program that cut it.
const BY: &str = "loquax";
/// The least time between two progress bumps of a region being written
/// (1291): a frame on screen is drawn again at most four times a second.
pub const PROGRESS_EVERY_MS: u64 = 250;

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

/// What a [`Step`] did, without its detail: what a frontend keeps of
/// the loader's steps for a test to read their order by (1616).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    Started,
    Done,
    Failed,
    Skipped,
}

impl Step {
    /// What the step did.
    pub fn kind(&self) -> StepKind {
        match self {
            Self::Started(_) => StepKind::Started,
            Self::Done { .. } => StepKind::Done,
            Self::Failed { .. } => StepKind::Failed,
            Self::Skipped { .. } => StepKind::Skipped,
        }
    }

    /// The box the step is about.
    pub fn want(&self) -> &MapWant {
        match self {
            Self::Started(want)
            | Self::Done { want, .. }
            | Self::Failed { want, .. }
            | Self::Skipped { want, .. } => want,
        }
    }
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

/// Where a box stands in the queue (1292), first first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    /// Asked for by hand: a place, the pins, "load this area".
    Asked,
    /// The screen itself at its zoom (1527).
    Screen,
    /// Under the screen.
    Here,
    /// Next to the screen.
    Ahead,
    /// An area the view left.
    Away,
}

impl Rank {
    /// Whether the box is for what the user looks at.
    fn on_screen(self) -> bool {
        self <= Self::Here
    }
}

struct Queued {
    want: MapWant,
    rank: Rank,
}

/// A failed box's wait.
struct Backoff {
    bounds: Bounds,
    failures: u32,
    until_ms: u64,
}

struct Running {
    want: MapWant,
    rank: Rank,
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
    queue: Vec<Queued>,
    running: Vec<Running>,
    backoff: Vec<Backoff>,
    /// The last load that ended found no internet, and none started since.
    offline: bool,
    /// No internet was said, and no load landed since.
    offline_said: bool,
}

impl Loader {
    pub fn new(maps: Maps, source: Source, cache_bytes: u64) -> Self {
        Self {
            maps,
            source,
            cache_bytes,
            resolved: Arc::new(Mutex::new(None)),
            queue: Vec::new(),
            running: Vec::new(),
            backoff: Vec::new(),
            offline: false,
            offline_said: false,
        }
    }

    /// Whether a load runs or waits.
    pub fn busy(&self) -> bool {
        !self.running.is_empty() || !self.queue.is_empty()
    }

    /// Whether a load runs or waits for what the user looks at: a box
    /// asked for by hand or under the screen (1292). The phone's chip says
    /// "Loading the map of this area" for these only.
    pub fn loading_here(&self) -> bool {
        self.running.iter().any(|r| r.rank.on_screen())
            || self.queue.iter().any(|q| q.rank.on_screen())
    }

    /// Whether the last load that ended found no internet and none started
    /// since: the phone's map says "Offline" while it holds (1200), and
    /// the next load asked tests it again (1292).
    pub fn offline(&self) -> bool {
        self.offline
    }

    /// The boxes queued and running, the running first, each with its
    /// rank: what a test reads the order from.
    pub fn order(&self) -> Vec<(Bounds, Rank, bool)> {
        self.running
            .iter()
            .map(|r| (r.want.bounds, r.rank, true))
            .chain(self.queue.iter().map(|q| (q.want.bounds, q.rank, false)))
            .collect()
    }

    /// Whether `bounds` is queued or loading.
    fn has(&self, bounds: &Bounds) -> bool {
        self.running.iter().any(|r| r.want.bounds == *bounds)
            || self.queue.iter().any(|q| q.want.bounds == *bounds)
    }

    /// Why `want` cannot be queued now, if it cannot.
    fn refuse(&self, want: &MapWant, now_ms: u64) -> Option<&'static str> {
        if self.has(&want.bounds) {
            return Some("queued");
        }
        if self
            .backoff
            .iter()
            .any(|b| b.bounds == want.bounds && now_ms < b.until_ms)
        {
            return Some("backoff");
        }
        self.maps.dir().is_none().then_some("no-maps-dir")
    }

    /// Queue `want`, asked for by hand, before every box the view asked
    /// for, and start it when nothing runs. `now_ms` is the shell's
    /// monotonic clock.
    pub fn ask(&mut self, want: MapWant, now_ms: u64) -> Vec<Step> {
        if let Some(why) = self.refuse(&want, now_ms) {
            return vec![Step::Skipped { want, why }];
        }
        let at = self
            .queue
            .iter()
            .position(|q| q.rank > Rank::Asked)
            .unwrap_or(self.queue.len());
        self.queue.insert(
            at,
            Queued {
                want,
                rank: Rank::Asked,
            },
        );
        self.start_next()
    }

    /// The scheduler's decision (1292, [`crate::ui::viewload`]): `here`
    /// the boxes under the screen, `ahead` those next to it, each in the
    /// order to load them. Every box queued or running takes the rank of
    /// the list it is in now, or is one the view left; the queue is
    /// ordered by rank, the lists' order within it, the boxes the view
    /// left in the order they came, at most [`MAX_AWAY`]; then a load for
    /// the screen starts when nothing for it runs. A box asked for by
    /// hand keeps its place.
    pub fn schedule(&mut self, here: Vec<MapWant>, ahead: Vec<MapWant>, now_ms: u64) -> Vec<Step> {
        let place = |b: &Bounds| {
            here.iter()
                .position(|w| w.bounds == *b)
                .map(|i| (Self::rank_here(&here[i]), i))
                .or_else(|| {
                    ahead
                        .iter()
                        .position(|w| w.bounds == *b)
                        .map(|i| (Rank::Ahead, i))
                })
        };
        for running in &mut self.running {
            if running.rank != Rank::Asked {
                // A load that brings the screen's box is the screen's
                // (1616): the cells wait for it as for the box itself.
                let screens = here.iter().any(|w| brings(&running.want, w));
                running.rank = match place(&running.want.bounds) {
                    _ if screens => Rank::Screen,
                    Some(p) => p.0,
                    None => Rank::Away,
                };
            }
        }
        let mut keyed: Vec<((Rank, usize), Queued)> = Vec::new();
        for (n, mut queued) in std::mem::take(&mut self.queue).into_iter().enumerate() {
            let key = match (queued.rank, place(&queued.want.bounds)) {
                (Rank::Asked, _) => (Rank::Asked, n),
                (_, Some(key)) => key,
                // A screen's box the view left: the cells hold its area.
                (_, None) if queued.want.reason == LoadReason::Screen => continue,
                (_, None) => (Rank::Away, n),
            };
            queued.rank = key.0;
            keyed.push((key, queued));
        }
        for (i, want) in here.into_iter().enumerate() {
            if self.refuse(&want, now_ms).is_none()
                && !keyed.iter().any(|k| k.1.want.bounds == want.bounds)
                && !self.brought(&want)
            {
                let rank = Self::rank_here(&want);
                keyed.push(((rank, i), Queued { want, rank }));
            }
        }
        for (i, want) in ahead.into_iter().enumerate() {
            if self.refuse(&want, now_ms).is_none()
                && !keyed.iter().any(|k| k.1.want.bounds == want.bounds)
            {
                keyed.push((
                    (Rank::Ahead, i),
                    Queued {
                        want,
                        rank: Rank::Ahead,
                    },
                ));
            }
        }
        keyed.sort_by_key(|k| k.0);
        let away = keyed.iter().filter(|k| k.0 .0 == Rank::Away).count();
        let mut drop = away.saturating_sub(MAX_AWAY);
        self.queue = keyed
            .into_iter()
            .filter(|k| {
                let gone = k.0 .0 == Rank::Away && drop > 0;
                drop -= usize::from(gone);
                !gone
            })
            .map(|k| k.1)
            .collect();
        self.start_next()
    }

    /// Whether `want`, the screen's own box (1527), is what a load
    /// running already brings ([`brings`]). Two loads of the same tiles
    /// share the link and both arrive later.
    fn brought(&self, want: &MapWant) -> bool {
        self.running.iter().any(|r| brings(&r.want, want))
    }

    /// The rank of `want` in the scheduler's `here`: the screen's own box
    /// first (1527), the cells after it.
    fn rank_here(want: &MapWant) -> Rank {
        match want.reason {
            LoadReason::Screen => Rank::Screen,
            _ => Rank::Here,
        }
    }

    /// What finished since the last call, and the next loads started.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut n = 0;
        while n < self.running.len() {
            let result = match self.running[n].done.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => {
                    n += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err(Failure {
                    detail: "the load stopped".to_string(),
                    unreachable: false,
                    too_large: false,
                }),
            };
            let running = self.running.remove(n);
            steps.push(self.ended(running, result, now_ms));
        }
        steps.extend(self.start_next());
        steps
    }

    /// The step of `running`, which ended with `result`.
    fn ended(&mut self, running: Running, result: Result<Landed, Failure>, now_ms: u64) -> Step {
        let elapsed_ms = u64::try_from(running.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let want = running.want;
        match result {
            Ok(landed) => {
                self.offline = false;
                self.offline_said = false;
                self.backoff.retain(|b| b.bounds != want.bounds);
                self.maps.changed();
                Step::Done {
                    want,
                    landed,
                    elapsed_ms,
                }
            }
            Err(failure) => {
                // No internet is no fault of the box: the next view asks
                // again, and the state holds until then.
                self.offline = failure.unreachable;
                if !failure.unreachable {
                    self.back_off(want.bounds, now_ms);
                }
                let toast = match (failure.unreachable, failure.too_large) {
                    (true, _) if self.offline_said => None,
                    (true, _) => {
                        self.offline_said = true;
                        Some("map: no internet; the map shows what is here".to_string())
                    }
                    (false, true) => Some(format!("map: {}; zoom in", failure.detail)),
                    (false, false) => Some(format!("map: {}", failure.detail)),
                };
                Step::Failed {
                    want,
                    detail: failure.detail,
                    toast,
                    elapsed_ms,
                }
            }
        }
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

    /// Start the queue's first boxes: one when nothing runs, one for the
    /// screen beside loads for areas the view left, and the screen's own
    /// box (1527) beside any load but another for the screen itself.
    fn start_next(&mut self) -> Vec<Step> {
        let mut steps = Vec::new();
        while let Some(first) = self.queue.first() {
            let beside = match first.rank {
                Rank::Screen => !self.running.iter().any(|r| r.rank <= Rank::Screen),
                rank if rank.on_screen() => !self.running.iter().any(|r| r.rank.on_screen()),
                _ => false,
            };
            let free = self.running.is_empty() || self.running.len() < MAX_RUNNING && beside;
            if !free {
                break;
            }
            let queued = self.queue.remove(0);
            steps.extend(self.start(queued));
        }
        steps
    }

    /// Start `queued` on a thread of its own.
    fn start(&mut self, queued: Queued) -> Vec<Step> {
        let Queued { want, rank } = queued;
        let Some(dir) = self.maps.dir() else {
            return Vec::new();
        };
        let (tx, rx) = channel();
        let job = Job {
            dir: dir.to_path_buf(),
            maps: self.maps.clone(),
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
        self.offline = false;
        self.running.push(Running {
            want: want.clone(),
            rank,
            started: Instant::now(),
            done: rx,
        });
        vec![Step::Started(want)]
    }
}

/// Whether a load of `load` brings `want`, the screen's own box (1527):
/// its box and zooms inside the load's.
fn brings(load: &MapWant, want: &MapWant) -> bool {
    want.reason == LoadReason::Screen
        && load.bounds.holds(&want.bounds)
        && load.zooms.0 <= want.zooms.0
        && load.zooms.1 >= want.zooms.1
}

/// One load, on its thread.
struct Job {
    dir: PathBuf,
    maps: Maps,
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
            // The phone and the window draw them from z16 (1191); the
            // terminal and the M9 never read the section.
            buildings: true,
            // Every point of interest rides along as a section a reader
            // that shows none skips for free (1685).
            pois: true,
        };
        let planned = plan(&archive, &want)?;
        let zooms = (*planned.zooms.start(), *planned.zooms.end());
        let name = auto_name(&want.bounds, zooms);
        // Under its own name from the first byte (1291): a painter draws
        // what has arrived, a version 4 file says what that is. A load
        // that replaces an installed region writes beside it instead and
        // takes its place only once the new file verifies (1727), so a
        // broken link or a failed check never costs the region there.
        let path = self.dir.join(&name);
        let replaces = path.exists();
        let written = match replaces {
            true => self.dir.join(format!("{name}{PART_SUFFIX}")),
            false => path.clone(),
        };
        // As wide as the tile painters (1473): one core count for the map.
        let threads = crate::picture::cores();
        let maps = self.maps.clone();
        let mut bumped: Option<Instant> = None;
        let mut progress = |p: Progress| {
            let due =
                bumped.is_none_or(|at| at.elapsed().as_millis() >= u128::from(PROGRESS_EVERY_MS));
            // A file written beside the region it replaces is no painter's
            // until it lands.
            if !p.complete && due && !replaces {
                bumped = Some(Instant::now());
                maps.arrived();
            }
        };
        let write = || -> leviculum_mapcut::Res<Loaded> {
            let mut out = BufWriter::new(File::create(&written)?);
            let loaded = cut_planned(
                &archive,
                &want,
                planned,
                BY,
                threads,
                &mut out,
                &mut progress,
            )?;
            let file = out.into_inner().map_err(|e| e.into_error())?;
            file.sync_all()?;
            let region = Region::open(File::open(&written)?).map_err(|e| format!("{e:?}"))?;
            region
                .verify()
                .map_err(|e| format!("the region does not verify: {e:?}"))?;
            if replaces {
                std::fs::rename(&written, &path)?;
            }
            Ok(loaded)
        };
        let loaded = match write() {
            Ok(loaded) => loaded,
            Err(error) => {
                let _ = std::fs::remove_file(&written);
                // A region written in place was drawn as it arrived and is
                // gone now; one written beside the old was never seen.
                if !replaces {
                    self.maps.changed();
                }
                return Err(error);
            }
        };
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
    fn a_load_lands_in_place_and_bumps_the_epoch() {
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
        // The z14 round told the painters before the region was whole
        // (1291).
        assert!(maps.progress() >= 1);
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

    /// Cut the fixture's box of `w` under `profile` into `dir` under the
    /// name the autoload gives it: a region installed before.
    fn installed(serve: &Serve, dir: &Path, w: &MapWant, profile: u8) -> PathBuf {
        let archive =
            Archive::from_source(HttpRange::new(&serve.url("o.pmtiles"))).expect("archive");
        let opts = leviculum_mapcut::cut::CutOptions {
            bounds: w.bounds,
            zooms: w.zooms.0..=w.zooms.1,
            profile,
            attribution: "test".to_string(),
            notice: "test".to_string(),
            threads: 1,
            locale: None,
            buildings: false,
            pois: false,
            round: leviculum_mapcut::cut::ROUND,
        };
        let path = dir.join(auto_name(&w.bounds, w.zooms));
        let file = File::create(&path).expect("create");
        leviculum_mapcut::cut::cut(&archive, &opts, BufWriter::new(file)).expect("cut");
        path
    }

    /// The profile of the region file at `path`.
    fn profile_of(path: &Path) -> u8 {
        Region::open(File::open(path).expect("open"))
            .expect("region")
            .header()
            .profile
    }

    /// A load that replaces an installed region writes beside it and
    /// takes its place only once the new file verifies (1727, the audit
    /// of 1707): the link breaking mid-body leaves the old region byte
    /// for byte, and a later load that succeeds replaces it, no part
    /// file left.
    #[test]
    fn a_load_that_breaks_leaves_the_region_it_was_to_replace() {
        let serve = Serve::start(vec![("o.pmtiles".into(), FIXTURE.to_vec())]).expect("serve");
        let w = want(112_615_000);
        // What a whole load sends, measured on a directory of its own.
        let measure = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(
            Maps::new(Some(measure.path().to_path_buf())),
            Source::Url(serve.url("o.pmtiles")),
            1 << 30,
        );
        loader.ask(w.clone(), 0);
        assert!(matches!(
            settle(&mut loader, 0).as_slice(),
            [Step::Done { .. }]
        ));
        let whole = serve.sent();

        let dir = tempfile::tempdir().expect("tmp");
        let path = installed(&serve, dir.path(), &w, leviculum_map::profile::CUT_1);
        let old = std::fs::read(&path).expect("read");
        assert_eq!(profile_of(&path), leviculum_map::profile::CUT_1);
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        let mut loader = Loader::new(maps.clone(), Source::Url(serve.url("o.pmtiles")), 1 << 30);
        serve.drop_at(Some(serve.sent() + whole / 2));
        loader.ask(w.clone(), 0);
        let steps = settle(&mut loader, 0);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { .. }]),
            "{steps:?}"
        );
        assert_eq!(
            std::fs::read(&path).expect("still there"),
            old,
            "byte for byte"
        );
        let names = |d: &Path| -> Vec<String> {
            let mut n: Vec<String> = std::fs::read_dir(d)
                .expect("dir")
                .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
                .collect();
            n.sort();
            n
        };
        let name = auto_name(&w.bounds, w.zooms);
        assert_eq!(names(dir.path()), std::slice::from_ref(&name), "no part file left");

        serve.drop_at(None);
        loader.ask(w.clone(), BACKOFF_FIRST_MS);
        let steps = settle(&mut loader, BACKOFF_FIRST_MS);
        let [Step::Done { landed, .. }] = steps.as_slice() else {
            panic!("{steps:?}");
        };
        assert_eq!(landed.name, name);
        assert_eq!(names(dir.path()), [name], "replaced in place");
        assert_eq!(profile_of(&path), leviculum_map::profile::CUT_2);
        assert_ne!(std::fs::read(&path).expect("read"), old);
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

    /// A source that takes the connection and never answers: a load
    /// started stays running for the test.
    fn stalling() -> (std::net::TcpListener, Source) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("its address").port();
        (
            listener,
            Source::Url(format!("http://127.0.0.1:{port}/o.pmtiles")),
        )
    }

    /// The `n`th of a row of boxes.
    fn nth(n: i32) -> MapWant {
        want(112_615_000 + n * 1000)
    }

    /// The `n`th box as the screen's own load (1527).
    fn screen(n: i32) -> MapWant {
        MapWant {
            reason: LoadReason::Screen,
            ..nth(n)
        }
    }

    /// The queue as the numbers of [`nth`]'s boxes.
    fn order(loader: &Loader) -> Vec<(i32, Rank, bool)> {
        loader
            .order()
            .into_iter()
            .map(|(b, rank, running)| ((b.west - 112_615_000) / 1000, rank, running))
            .collect()
    }

    /// Two boxes queued for the screen, the view moves: the box under the
    /// new screen goes first, the one still on it next, the one the view
    /// left last (1292). A box asked for by hand keeps its place.
    #[test]
    fn the_box_on_screen_now_goes_first() {
        let (_source, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        let (p, a, b, c) = (nth(1), nth(2), nth(3), nth(4));
        assert!(matches!(loader.ask(p, 0).as_slice(), [Step::Started(_)]));
        assert!(loader
            .schedule(vec![a.clone(), b.clone()], Vec::new(), 0)
            .is_empty());
        assert_eq!(
            order(&loader),
            [
                (1, Rank::Asked, true),
                (2, Rank::Here, false),
                (3, Rank::Here, false)
            ]
        );
        loader.schedule(vec![c, b], Vec::new(), 250);
        assert_eq!(
            order(&loader),
            [
                (1, Rank::Asked, true),
                (4, Rank::Here, false),
                (3, Rank::Here, false),
                (2, Rank::Away, false)
            ]
        );
        assert!(loader.loading_here());
        // The screen's own box (1527) goes before the cells under it; the
        // view moves, the old screen box is dropped, not kept as left.
        let (s, t) = (screen(6), screen(7));
        loader.schedule(vec![s.clone(), nth(4), nth(3)], Vec::new(), 500);
        assert_eq!(
            order(&loader),
            [
                (1, Rank::Asked, true),
                (6, Rank::Screen, false),
                (4, Rank::Here, false),
                (3, Rank::Here, false),
                (2, Rank::Away, false)
            ]
        );
        loader.schedule(vec![t, nth(3)], Vec::new(), 750);
        assert_eq!(
            order(&loader),
            [
                (1, Rank::Asked, true),
                (7, Rank::Screen, false),
                (3, Rank::Here, false),
                (4, Rank::Away, false),
                (2, Rank::Away, false)
            ],
            "the screen box the view left is gone: {s:?}"
        );
    }

    /// The screen's own box loads first and alone (1527): the cell under
    /// the screen waits until it landed, so no byte of the cell comes
    /// before the screen's tiles; a screen's box starts beside a cell for
    /// the screen that runs already, never beside another screen box.
    #[test]
    fn the_screens_own_box_loads_before_the_cell_around_it() {
        let (_source, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        let (s, a, n) = (screen(1), nth(2), nth(5));
        let steps = loader.schedule(vec![s.clone(), a.clone()], vec![n.clone()], 0);
        assert_eq!(steps, [Step::Started(s.clone())], "the screen alone");
        assert_eq!(
            order(&loader),
            [
                (1, Rank::Screen, true),
                (2, Rank::Here, false),
                (5, Rank::Ahead, false)
            ]
        );
        assert!(loader.loading_here());
        // A cell for the screen runs (the screen's box landed meanwhile,
        // here: was never asked); a new screen box starts beside it.
        let (_source, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        assert_eq!(
            loader.schedule(vec![a.clone()], Vec::new(), 0),
            [Step::Started(a.clone())]
        );
        let t = screen(3);
        assert_eq!(
            loader.schedule(vec![t.clone(), a.clone()], Vec::new(), 250),
            [Step::Started(t.clone())],
            "beside the cell"
        );
        // A second screen box waits for the first, which the view left:
        // it runs on as left, the new one waits for a free slot.
        let u = screen(4);
        assert!(loader
            .schedule(vec![u.clone(), a.clone()], Vec::new(), 500)
            .is_empty());
        assert_eq!(
            order(&loader),
            [
                (2, Rank::Here, true),
                (3, Rank::Away, true),
                (4, Rank::Screen, false)
            ]
        );
    }

    /// A screen's box a running load brings is not loaded twice (1527),
    /// and that load is then the screen's: the cell under the screen waits
    /// until it landed, as for the screen's own box (1616). The window's
    /// first-tile walk caught the cell starting beside it: a drag's last
    /// screen box held the box of the view it stopped on, ranked as left,
    /// and the cell took half the link from the screen's tiles.
    #[test]
    fn a_running_load_that_brings_the_screens_box_holds_the_cell() {
        let (_source, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        let (s, a) = (screen(1), nth(30));
        assert_eq!(
            loader.schedule(vec![s.clone(), a.clone()], Vec::new(), 0),
            [Step::Started(s.clone())]
        );
        // The view moved on inside the box loading: its screen's box is
        // that load's.
        let inner = MapWant {
            bounds: Bounds {
                west: s.bounds.west + 1000,
                east: s.bounds.east - 1000,
                ..s.bounds
            },
            ..s.clone()
        };
        let steps = loader.schedule(vec![inner, a.clone()], Vec::new(), 250);
        assert!(steps.is_empty(), "the cell waits: {steps:?}");
        assert_eq!(
            order(&loader),
            [(1, Rank::Screen, true), (30, Rank::Here, false)]
        );
        assert!(loader.loading_here());
    }

    /// A load whose area the view left runs on, as the cut cannot pause;
    /// the box under the new screen starts beside it, and the chip's
    /// "here" names it, not the old one (1292).
    #[test]
    fn a_load_the_view_left_does_not_hold_up_the_screen() {
        let (_source, source) = stalling();
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(Maps::new(Some(dir.path().to_path_buf())), source, 1 << 30);
        let (a, b, n) = (nth(2), nth(3), nth(5));
        let steps = loader.schedule(vec![a.clone()], vec![n.clone()], 0);
        assert_eq!(steps, [Step::Started(a.clone())]);
        assert!(loader.loading_here());
        // The view moved on: a is left, b is on screen.
        let steps = loader.schedule(vec![b.clone()], Vec::new(), 250);
        assert_eq!(steps, [Step::Started(b.clone())], "beside the old load");
        assert_eq!(
            order(&loader),
            [
                (2, Rank::Away, true),
                (3, Rank::Here, true),
                (5, Rank::Away, false)
            ]
        );
        // Away only: no chip for the screen.
        loader.schedule(Vec::new(), Vec::new(), 500);
        assert!(loader.busy() && !loader.loading_here());
        // Never more than the queue's cap of left boxes.
        let left: Vec<MapWant> = (10..30).map(nth).collect();
        loader.schedule(Vec::new(), left, 750);
        loader.schedule(Vec::new(), Vec::new(), 1000);
        let queued = order(&loader).iter().filter(|o| !o.2).count();
        assert_eq!(queued, MAX_AWAY);
        // The screen's own box (1527) goes before every queued box, and
        // waits only for a free slot: both run, for areas left.
        let s = screen(40);
        assert!(loader
            .schedule(vec![s.clone()], Vec::new(), 1250)
            .is_empty());
        assert_eq!(order(&loader)[2], (40, Rank::Screen, false));
        assert!(loader.loading_here());
    }

    /// "Offline" is a state, not a latch (1292): a load that found no
    /// internet sets it, the next view's ask tries the same box again at
    /// once, with no wait, and while that load runs the state is unknown.
    #[test]
    fn offline_is_tested_again_by_the_next_ask() {
        let serve = Serve::start(Vec::new()).expect("serve");
        let url = serve.url("o.pmtiles");
        drop(serve);
        let dir = tempfile::tempdir().expect("tmp");
        let mut loader = Loader::new(
            Maps::new(Some(dir.path().to_path_buf())),
            Source::Url(url),
            1 << 30,
        );
        let a = nth(2);
        loader.schedule(vec![a.clone()], Vec::new(), 0);
        let steps = settle(&mut loader, 0);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { toast: Some(_), .. }]),
            "{steps:?}"
        );
        assert!(loader.offline() && !loader.busy());
        let steps = loader.schedule(vec![a.clone()], Vec::new(), 1);
        assert_eq!(steps, [Step::Started(a.clone())], "tried again, no backoff");
        assert!(!loader.offline(), "unknown while it runs");
        let steps = settle(&mut loader, 1);
        assert!(
            matches!(steps.as_slice(), [Step::Failed { toast: None, .. }]),
            "said once: {steps:?}"
        );
        assert!(loader.offline());
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
