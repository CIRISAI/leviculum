//! The map's picture, for every desktop frontend (`docs/src/concepts/
//! loquax-map.md`, sections 6 and 7): the region files the reader placed by
//! hand, and one view of them drawn into an Rgb565 frame with the world
//! outline under whatever no region covers. The terminal encodes the frame
//! as sixel, kitty, half-blocks or braille (`tui::map` in the binary); the
//! GUI shows it as an image (`gui::map`). Both draw through this module, so
//! the region a view chooses and the credit under it are one rule.
//!
//! Regions are read from `$LNMSG_HOME/maps/` when `LNMSG_HOME` is set and
//! from `${XDG_DATA_HOME:-~/.local/share}/loquax/maps/` otherwise: every
//! `*.lmr` there, by file name. `LNMSG_HOME` replaces the whole default base
//! for the identity and the store ([`crate::identity::home_dir`]), so it
//! replaces this one too; a test or a second profile then sees only its own
//! maps. Where no region covers the view the world outline is drawn
//! ([`leviculum_map::outline`]).
//!
//! The directory changes while the messenger runs: the autoload
//! ([`crate::mapload`], 1089) adds regions and evicts old ones. [`Maps`] is
//! the handle every painter and the shell share; a change bumps its epoch,
//! and a painter that sees a new epoch reads the directory again before its
//! next paint. Every region that reaches into a view is drawn, each into its
//! own box, so regions loaded side by side join up.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::string::{String, ToString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use std::vec::Vec;

use embedded_graphics::prelude::{Dimensions, Point};
use embedded_graphics::primitives::Rectangle;
use leviculum_map::choice::{drawn, view_zooms};
use leviculum_map::draw::{draw_in_scaled, draw_tiles, Ink};
use leviculum_map::labels::ViewLabel;
use leviculum_map::outline;
use leviculum_map::region::{Header, Region};
use leviculum_map::viewport::{ViewTile, Viewport};
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{NoClock, Work};
use leviculum_mapfill::style::{MapTheme, Profile, Style};
use leviculum_ui::theme::{Theme, ThemeFlag};

use crate::identity::HOME_ENV;

/// Where the region files are read from.
pub fn maps_dir() -> Option<PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    if let Some(home) = set(HOME_ENV) {
        return Some(PathBuf::from(home).join("maps"));
    }
    let data = set("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".local/share")))?;
    Some(data.join("loquax").join("maps"))
}

/// The map's colour table for the config's `map_theme` (`flag`) and the
/// theme the screen is drawn in, `None` for a frontend that has none:
/// unset is light, and `auto` follows the screen, light without one.
pub fn map_theme(flag: Option<ThemeFlag>, screen: Option<Theme>) -> MapTheme {
    match (flag, screen) {
        (Some(ThemeFlag::Dark), _) | (Some(ThemeFlag::Auto), Some(Theme::Dark)) => MapTheme::Dark,
        _ => MapTheme::Light,
    }
}

/// The file name prefix of a region the autoload wrote: those are a cache,
/// evicted least recently viewed first; a file without it was placed by
/// hand and is never touched.
pub const AUTO_PREFIX: &str = "auto-";

/// How often a painter marks an autoloaded region it drew as viewed, by its
/// file's modification time, which the eviction orders by.
const VIEWED_EVERY: Duration = Duration::from_secs(60);

/// Whether `name` is a region the autoload wrote.
pub fn is_auto(name: &str) -> bool {
    name.starts_with(AUTO_PREFIX)
}

/// The maps directory as every painter and the shell share it: where it
/// is, and an epoch bumped whenever a region is added or removed.
#[derive(Clone, Debug, Default)]
pub struct Maps {
    inner: Arc<MapsInner>,
}

#[derive(Debug, Default)]
struct MapsInner {
    dir: Option<PathBuf>,
    epoch: AtomicU64,
}

impl Maps {
    /// The regions of `dir`; `None` for none at all.
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(MapsInner {
                dir,
                epoch: AtomicU64::new(0),
            }),
        }
    }

    pub fn dir(&self) -> Option<&Path> {
        self.inner.dir.as_deref()
    }

    /// The directory's version: a painter holding an older one reads it
    /// again.
    pub fn epoch(&self) -> u64 {
        self.inner.epoch.load(Ordering::SeqCst)
    }

    /// A region was added or removed.
    pub fn changed(&self) {
        self.inner.epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Every region there now, and a line for each that did not open.
    pub fn load(&self) -> (Vec<Installed>, Vec<String>) {
        self.dir().map(load).unwrap_or_default()
    }

    /// The headers of the regions there now, newest first
    /// ([`newest_first`]).
    pub fn headers(&self) -> Vec<Header> {
        let (regions, _) = self.load();
        newest_first(&regions)
            .into_iter()
            .map(|r| *r.region.header())
            .collect()
    }
}

/// One region file, open.
pub struct Installed {
    pub name: String,
    pub region: Region<File>,
    /// The file's modification time, which a view of an autoloaded region
    /// renews ([`Painter`]); `None` where the file system keeps none.
    pub modified: Option<SystemTime>,
}

/// `regions`, the newest file first, ties by name: a map opened without
/// pins shows the first (`crate::ui::map::fit`), the region last installed
/// or viewed.
pub fn newest_first(regions: &[Installed]) -> Vec<&Installed> {
    let mut order: Vec<&Installed> = regions.iter().collect();
    order.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.name.cmp(&b.name)));
    order
}

/// Every `*.lmr` in `dir`, by name, and a line for each that did not open.
/// A missing directory is no regions, not an error.
pub fn load(dir: &Path) -> (Vec<Installed>, Vec<String>) {
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "lmr"))
            .collect(),
        Err(_) => return (Vec::new(), Vec::new()),
    };
    paths.sort();
    let (mut regions, mut problems) = (Vec::new(), Vec::new());
    for path in paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        match File::open(&path)
            .map_err(|e| e.to_string())
            .and_then(|f| Region::open(f).map_err(|e| format!("{e:?}")))
        {
            Ok(region) => regions.push(Installed {
                name,
                region,
                modified,
            }),
            Err(error) => problems.push(format!("map {}: {error}", path.display())),
        }
    }
    (regions, problems)
}

/// One view drawn: its pixels, Rgb565 in the frame's byte order, and the
/// credit the picture carries.
pub struct Drawn {
    pub fb: Vec<u16>,
    pub attribution: String,
}

/// The installed regions and the buffers a view is read into, kept from
/// one view to the next.
pub struct Painter {
    maps: Maps,
    /// The epoch `regions` were read at.
    epoch: u64,
    regions: Vec<Installed>,
    read: Vec<(ViewTile, Vec<u8>)>,
    /// The filler's storage, kept from draw to draw: a tile cache's
    /// painter draws hundreds of tiles, and allocating this for each cost
    /// about a tenth of a cold frame's tile time (the render study of
    /// 2026-10-30).
    scratch: FillScratch,
    /// When each autoloaded region was last marked viewed.
    viewed: HashMap<String, Instant>,
}

/// The filler's storage for one region drawn ([`region_into`]). The filler
/// sets up what it reads before each fill (every bucket head of the clip
/// is reset as a fill begins), so what an earlier draw left is never seen.
struct FillScratch {
    edges: Vec<Edge>,
    active: Vec<u16>,
    heads: Vec<u16>,
    points: Vec<Point>,
    ring_ends: Vec<u16>,
}

impl FillScratch {
    fn new() -> Self {
        Self {
            edges: vec![Edge::default(); 8192],
            active: vec![0u16; 8192],
            heads: Vec::new(),
            points: vec![Point::zero(); 8192],
            ring_ends: vec![0u16; 2048],
        }
    }
}

impl Painter {
    /// A painter over the regions of `maps`, read now.
    pub fn new(maps: Maps) -> Self {
        let epoch = maps.epoch();
        let (regions, _) = maps.load();
        Self {
            maps,
            epoch,
            regions,
            read: Vec::new(),
            scratch: FillScratch::new(),
            viewed: HashMap::new(),
        }
    }

    /// Read the directory again when it changed since; whether it did.
    pub fn refresh(&mut self) -> bool {
        let epoch = self.maps.epoch();
        if epoch == self.epoch {
            return false;
        }
        self.epoch = epoch;
        self.regions = self.maps.load().0;
        true
    }

    /// The epoch of the regions this painter draws.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn regions(&self) -> &[Installed] {
        &self.regions
    }

    /// The regions' headers, which say how deep a fit goes, newest first
    /// ([`newest_first`]).
    pub fn headers(&self) -> Vec<Header> {
        newest_first(&self.regions)
            .into_iter()
            .map(|r| *r.region.header())
            .collect()
    }

    /// The zooms a frontend offers over these regions and the outline.
    pub fn zooms(&self) -> (u8, u8) {
        let zooms = view_zooms(self.regions.iter().map(|r| r.region.header()));
        (*zooms.start(), *zooms.end())
    }

    /// Draw `view` in `ink` and `style` with lines `percent` per cent as
    /// wide as the zoom's ([`leviculum_mapfill::render::Look::scaled`]): the world
    /// outline under what no region covers, then every region that reaches
    /// into the view ([`drawn`]), each inside its own box, the deepest last.
    /// A region whose tile does not read is left out. The directory is read
    /// again first when it changed.
    pub fn draw(&mut self, view: &Viewport, ink: Ink, style: Style, percent: u32) -> Drawn {
        self.refresh();
        let (w, h) = view.size();
        let mut fb = vec![0u16; (w as usize) * (h as usize)];
        let order = drawn(
            self.regions
                .iter()
                .map(|r| (r.name.as_str(), r.region.header())),
            view,
        );
        let mut attribution = outline::ATTRIBUTION.to_string();
        let mut shown: Vec<usize> = Vec::new();
        if let Some(mut frame) = Frame::new(&mut fb, w, h) {
            // The outline goes under the view unless one region fills it.
            let whole = frame.bounding_box();
            let filled = order
                .iter()
                .any(|&i| self.regions[i].region.area(view) == whole);
            if !filled {
                let Ok(_) = outline::draw(
                    outline::WORLD_50M,
                    outline::BORDERS_50M,
                    view,
                    ink,
                    style.theme,
                    &mut frame,
                );
            }
            for &i in &order {
                let installed = &self.regions[i];
                let area = installed.region.area(view);
                if region_into(
                    &installed.region,
                    view,
                    area,
                    ink,
                    style,
                    percent,
                    &mut self.read,
                    &mut self.scratch,
                    &mut frame,
                ) {
                    attribution = installed.region.attribution().to_string();
                    shown.push(i);
                }
            }
        }
        for i in shown {
            self.viewed(i);
        }
        Drawn { fb, attribution }
    }

    /// The names `view` shows from every region that reaches into it, in
    /// the order [`Painter::draw`] draws them, streets too with `lines`; a
    /// region whose names do not read is left out. A version 1 region has
    /// none. Placed by the frontend, in its unit, by the core's rule
    /// ([`leviculum_map::labels::place`]).
    pub fn labels(&mut self, view: &Viewport, lines: bool) -> Vec<ViewLabel> {
        self.refresh();
        let order = drawn(
            self.regions
                .iter()
                .map(|r| (r.name.as_str(), r.region.header())),
            view,
        );
        let mut out = Vec::new();
        for i in order {
            let mut found = Vec::new();
            if self.regions[i]
                .region
                .view_labels(view, lines, &mut found)
                .is_ok()
            {
                out.extend(found);
            }
        }
        out
    }

    /// Mark the autoloaded region `i` viewed, at most every
    /// [`VIEWED_EVERY`]: its file's modification time is the order the
    /// cache is evicted in ([`crate::mapload`]).
    fn viewed(&mut self, i: usize) {
        let Some(name) = self.regions.get(i).map(|r| r.name.clone()) else {
            return;
        };
        if !is_auto(&name) {
            return;
        }
        let now = Instant::now();
        if self
            .viewed
            .get(&name)
            .is_some_and(|t| now.duration_since(*t) < VIEWED_EVERY)
        {
            return;
        }
        if let Some(dir) = self.maps.dir() {
            if let Ok(file) = File::open(dir.join(&name)) {
                let _ = file.set_modified(SystemTime::now());
            }
        }
        self.viewed.insert(name, now);
    }
}

/// Draw `region`'s tiles of `view` into `area` of `frame`; `false` when a
/// tile could not be read, and the region is left out.
#[allow(clippy::too_many_arguments)]
fn region_into(
    region: &Region<File>,
    view: &Viewport,
    area: Rectangle,
    ink: Ink,
    style: Style,
    percent: u32,
    read: &mut Vec<(ViewTile, Vec<u8>)>,
    fill: &mut FillScratch,
    frame: &mut Frame<'_>,
) -> bool {
    if region.read_view(view, read).is_err() {
        return false;
    }
    let Ok(tiles) = draw_tiles(read) else {
        return false;
    };
    let (_, h) = view.size();
    // A bucket head a row of the view.
    fill.heads.resize(h as usize, 0);
    let mut scratch = Scratch {
        edges: &mut fill.edges,
        heads: &mut fill.heads,
        active: &mut fill.active,
    };
    let mut work = Work {
        points: &mut fill.points,
        ring_ends: &mut fill.ring_ends,
    };
    let Ok(_) = draw_in_scaled(
        &tiles,
        area,
        Profile::Full,
        ink,
        style,
        percent,
        frame,
        &mut scratch,
        &mut work,
        &mut NoClock,
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_map_theme_is_light_unless_asked_and_auto_follows_the_screen() {
        use ThemeFlag::{Auto, Dark, Light};
        for (flag, screen, want) in [
            (None, Some(Theme::Dark), MapTheme::Light),
            (Some(Light), Some(Theme::Dark), MapTheme::Light),
            (Some(Dark), None, MapTheme::Dark),
            (Some(Auto), Some(Theme::Dark), MapTheme::Dark),
            (Some(Auto), Some(Theme::Light), MapTheme::Light),
            (Some(Auto), None, MapTheme::Light),
        ] {
            assert_eq!(map_theme(flag, screen), want, "{flag:?} {screen:?}");
        }
    }

    #[test]
    fn the_maps_directory_follows_the_home_rule() {
        // Read only, no environment written: the rule is checked on what
        // this process has.
        let dir = maps_dir();
        match std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            Some(home) => assert_eq!(dir, Some(PathBuf::from(home).join("maps"))),
            None => assert!(dir.is_none_or(|d| d.ends_with("loquax/maps"))),
        }
        let empty = tempfile::tempdir().expect("tmp");
        let (regions, problems) = load(empty.path());
        assert!(regions.is_empty() && problems.is_empty());
        std::fs::write(empty.path().join("broken.lmr"), b"not a region").expect("write");
        let (regions, problems) = load(empty.path());
        assert!(regions.is_empty());
        assert_eq!(problems.len(), 1, "{problems:?}");
    }

    /// The regions newest first, ties by name (1131): the first view of a
    /// map without pins goes to the region last installed or viewed.
    #[test]
    fn regions_are_listed_newest_first() {
        const FLORENCE: &[u8] = include_bytes!("../../leviculum-map/fixtures/firenze.lmr");
        let dir = tempfile::tempdir().expect("tmp");
        for name in ["a.lmr", "b.lmr", "c.lmr"] {
            std::fs::write(dir.path().join(name), FLORENCE).expect("write");
        }
        let age = |name: &str, secs: u64| {
            let file = File::options()
                .write(true)
                .open(dir.path().join(name))
                .expect("open");
            file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
                .expect("mtime");
        };
        age("a.lmr", 1_000);
        age("b.lmr", 3_000);
        age("c.lmr", 1_000);
        let (regions, _) = load(dir.path());
        let names: Vec<&str> = newest_first(&regions)
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(names, ["b.lmr", "a.lmr", "c.lmr"]);
    }

    /// Two regions loaded side by side draw together (1089): over a view
    /// across their shared edge each half shows its own region, where one
    /// region alone leaves the other half to the outline. Cut here from the
    /// autoload's fixture, the west and the east half of its box.
    #[cfg(feature = "mapload")]
    #[test]
    fn two_adjacent_regions_draw_together() {
        use leviculum_map::mercator::{project, LatLon};
        use leviculum_map::region::Bounds;
        use leviculum_mapcut::cut::{cut, CutOptions};
        use leviculum_mapcut::pmtiles::Archive;

        const FIXTURE: &[u8] = include_bytes!("../../leviculum-mapcut/fixtures/oltrarno.pmtiles");
        let dir = tempfile::tempdir().expect("tmp");
        let source = dir.path().join("o.pmtiles");
        std::fs::write(&source, FIXTURE).expect("write");
        let archive = Archive::open(&source).expect("open");
        let half = |name: &str, west: i32, east: i32| {
            let maps = dir.path().join(name);
            std::fs::create_dir_all(&maps).expect("dir");
            let bounds = Bounds {
                west,
                south: 437_405_000,
                east,
                north: 437_545_000,
            };
            let opts = CutOptions {
                bounds,
                zooms: 14..=15,
                profile: leviculum_map::profile::CUT_1,
                attribution: "© OpenStreetMap contributors".to_string(),
                notice: "test".to_string(),
                threads: 1,
                locale: None,
                buildings: true,
            };
            let out = File::create(maps.join(format!("{name}.lmr"))).expect("create");
            cut(&archive, &opts, std::io::BufWriter::new(out)).expect("cut");
            maps
        };
        let (west, east) = (
            half("west", 112_615_000, 112_720_000),
            half("east", 112_720_000, 112_825_000),
        );
        let both = dir.path().join("both");
        std::fs::create_dir_all(&both).expect("dir");
        for (from, name) in [(&west, "west.lmr"), (&east, "east.lmr")] {
            std::fs::copy(from.join(name), both.join(name)).expect("copy");
        }
        let view = Viewport::new(
            project(LatLon::from_degrees(43.7475, 11.2720)),
            15,
            200,
            100,
        );
        let paint = |dir: &Path| {
            Painter::new(Maps::new(Some(dir.to_path_buf())))
                .draw(&view, Ink::Colour, Style::M9, 100)
                .fb
        };
        let (w, b, e) = (paint(&west), paint(&both), paint(&east));
        let side = |fb: &[u16], right: bool| -> Vec<u16> {
            fb.chunks(200)
                .flat_map(|row| match right {
                    false => row[..90].to_vec(),
                    true => row[110..].to_vec(),
                })
                .collect()
        };
        assert_eq!(
            side(&b, false),
            side(&w, false),
            "the west half is the west region"
        );
        assert_eq!(
            side(&b, true),
            side(&e, true),
            "the east half is the east region"
        );
        assert_ne!(
            side(&b, true),
            side(&w, true),
            "not the outline east of the west region"
        );
        assert_ne!(
            side(&b, false),
            side(&e, false),
            "nor west of the east region"
        );
    }
}
