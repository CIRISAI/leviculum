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
//!
//! A region the autoload is still writing is in the directory too (1291):
//! its tiles draw as they arrive, each one not there yet stood in for by
//! the nearest coarser one ([`leviculum_map::region::Region::stand_in`]).
//! The loader bumps [`Maps::progress`] as rounds land, at most four times
//! a second; a painter that sees it reads the directory again, and a view
//! with a tile not there yet says so ([`Drawn::pending`]), so a tile
//! cache holds it only until the next progress.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::string::{String, ToString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use std::vec::Vec;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::Point;
use embedded_graphics::primitives::Rectangle;
use leviculum_map::blend::{Blend, Shape};
use leviculum_map::choice::{drawn, view_zooms};
use leviculum_map::draw::{attach_buildings, draw_in_scaled, draw_tiles, draws_buildings, Ink};
use leviculum_map::labels::ViewLabel;
use leviculum_map::outline;
use leviculum_map::region::{Header, Region, TileId};
use leviculum_map::timezones;
use leviculum_map::viewport::{ViewTile, Viewport};
use leviculum_mapfill::coverage::{Coverage, Hard};
use leviculum_mapfill::fill::{Edge, Filler, Scratch};
use leviculum_mapfill::frame::{Frame, RgbaFrame};
use leviculum_mapfill::render::{NoClock, Work};
use leviculum_mapfill::style::{MapTheme, Profile, Style};
use leviculum_ui::theme::{Theme, ThemeFlag};

/// The cores this process may run on, read once (1473, 1442 addenda 7
/// and 8): the map's tile painters ([`crate::tiles::TileCache`]) and the
/// region loader ([`crate::mapload`]) each run this many threads, without
/// a cap of their own and without halving it, so one number says how wide
/// the map works on every host, and a host with more cores uses them.
pub fn cores() -> usize {
    static CORES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CORES.get_or_init(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
}

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

/// The world projection the product ships (1710):
/// [`crate::ui::map::world_shape`], where the terminal's core reaches it
/// too.
pub fn world_shape() -> Shape {
    crate::ui::map::world_shape()
}

/// Whether `name` is a region the autoload wrote.
pub fn is_auto(name: &str) -> bool {
    name.starts_with(AUTO_PREFIX)
}

/// The maps directory as every painter and the shell share it: where it
/// is, an epoch bumped whenever a region is added or removed, and a
/// progress count bumped as a region being written gains tiles.
#[derive(Clone, Debug, Default)]
pub struct Maps {
    inner: Arc<MapsInner>,
}

#[derive(Debug, Default)]
struct MapsInner {
    dir: Option<PathBuf>,
    epoch: AtomicU64,
    progress: AtomicU64,
}

impl Maps {
    /// The regions of `dir`; `None` for none at all.
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(MapsInner {
                dir,
                epoch: AtomicU64::new(0),
                progress: AtomicU64::new(0),
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

    /// How often a region being written gained tiles (1291): a painter
    /// holding an older count reads the directory again, a tile cache
    /// draws again what it drew before those tiles were there.
    pub fn progress(&self) -> u64 {
        self.inner.progress.load(Ordering::SeqCst)
    }

    /// A region being written gained tiles.
    pub fn arrived(&self) {
        self.inner.progress.fetch_add(1, Ordering::SeqCst);
    }

    /// Every region there now, and a line for each that did not open.
    pub fn load(&self) -> (Vec<Installed>, Vec<String>) {
        self.dir().map(load).unwrap_or_default()
    }

    /// How many region files there are now and their bytes together, by
    /// the directory alone, none opened: the Maps page's "Delete all
    /// downloaded maps (1.2 GB)" (1290).
    pub fn usage(&self) -> (usize, u64) {
        let lens: Vec<u64> = self
            .dir()
            .map(region_files)
            .unwrap_or_default()
            .iter()
            .map(|path| std::fs::metadata(path).map_or(0, |m| m.len()))
            .collect();
        (lens.len(), lens.iter().sum())
    }

    /// Delete every region file, the hand placed ones too, and tell every
    /// painter (1290); how many went. One that will not go stops it with
    /// a line, the ones before it are gone. A region the autoload is
    /// writing lands after, as any area looked at does.
    pub fn delete_all(&self) -> Result<usize, String> {
        let paths = self.dir().map(region_files).unwrap_or_default();
        let mut gone = 0;
        let result = paths.iter().try_for_each(|path| {
            std::fs::remove_file(path)
                .map_err(|error| format!("map: {} was not deleted: {error}", path.display()))?;
            gone += 1;
            Ok(())
        });
        if gone > 0 {
            tracing::info!("MAPLOAD deleted all={gone}");
            self.changed();
        }
        result.map(|()| gone)
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

/// The paths of every `*.lmr` in `dir`, by name; none for a missing
/// directory.
fn region_files(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "lmr") && p.is_file())
            .collect(),
        Err(_) => return Vec::new(),
    };
    paths.sort();
    paths
}

/// Every `*.lmr` in `dir`, by name, and a line for each that did not open.
/// A missing directory is no regions, not an error.
pub fn load(dir: &Path) -> (Vec<Installed>, Vec<String>) {
    let paths = region_files(dir);
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

/// One view drawn into opaque RGBA ([`Painter::draw_rgba`]): its bytes,
/// the credit, the tiles owed as [`Drawn`] has them.
pub struct DrawnRgba {
    pub rgba: Vec<u8>,
    pub attribution: String,
    pub pending: usize,
}

/// One view drawn: its pixels, Rgb565 in the frame's byte order, and the
/// credit the picture carries.
pub struct Drawn {
    pub fb: Vec<u16>,
    pub attribution: String,
    /// Tiles of the view a region still being written does not hold yet,
    /// stood in for or left out (1291): draw it again on its next
    /// [`Maps::progress`].
    pub pending: usize,
}

/// The installed regions and the buffers a view is read into, kept from
/// one view to the next.
pub struct Painter {
    maps: Maps,
    /// The epoch `regions` were read at.
    epoch: u64,
    /// The progress `regions` were read at.
    progress: u64,
    regions: Vec<Installed>,
    read: Vec<(ViewTile, Vec<u8>)>,
    /// The building sections a view that draws them read (1192).
    buildings: Vec<(TileId, Vec<u8>)>,
    /// The filler's storage, kept from draw to draw: a tile cache's
    /// painter draws hundreds of tiles, and allocating this for each cost
    /// about a tenth of a cold frame's tile time (the render study of
    /// 2026-10-30).
    scratch: FillScratch,
    /// When each autoloaded region was last marked viewed.
    viewed: HashMap<String, Instant>,
    /// Place no names ([`Painter::set_names_off`]): a measuring tool's
    /// ground.
    names_off: bool,
    /// The time zone view (1684): the zones' borders over the picture and
    /// their clocks at this instant, in seconds since the epoch, in place
    /// of the names.
    time_zones: Option<i64>,
    /// Blend world views through this projection (1710), `None` for
    /// Mercator throughout: the default, which a tile cache's painters
    /// keep — a square tile of one step must stay Mercator-linear
    /// whatever its zoom. The whole-view frontends set the shipped
    /// [`world_shape`].
    world_shape: Option<Shape>,
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
        let progress = maps.progress();
        let (regions, _) = maps.load();
        Self {
            maps,
            epoch,
            progress,
            regions,
            read: Vec::new(),
            buildings: Vec::new(),
            scratch: FillScratch::new(),
            viewed: HashMap::new(),
            names_off: false,
            time_zones: None,
            world_shape: None,
        }
    }

    /// Read the directory again when it changed since, or a region in it
    /// gained tiles; whether it did.
    pub fn refresh(&mut self) -> bool {
        let (epoch, progress) = (self.maps.epoch(), self.maps.progress());
        if epoch == self.epoch && progress == self.progress {
            return false;
        }
        self.epoch = epoch;
        self.progress = progress;
        self.regions = self.maps.load().0;
        true
    }

    /// The directory it reads, as the shell and the loader share it.
    pub fn maps(&self) -> &Maps {
        &self.maps
    }

    /// The epoch of the regions this painter draws.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn regions(&self) -> &[Installed] {
        &self.regions
    }

    /// Place and draw no names, or again: a measuring tool's ground.
    pub fn set_names_off(&mut self, off: bool) {
        self.names_off = off;
    }

    /// Show the time zone view at `unix` seconds since the epoch, or not
    /// (1684): [`draw`](Self::draw) strokes the zones' borders over the
    /// picture and [`labels`](Self::labels) gives each zone's clock in
    /// place of the names. The tile cache's painters never get it: the
    /// window and the phone stroke the borders over the composed frame
    /// ([`crate::screenmap::draw_time_zones`]), so no cached tile holds
    /// them.
    pub fn set_time_zones(&mut self, unix: Option<i64>) {
        self.time_zones = unix;
    }

    /// Blend world views through `shape` (1710), or through none: the
    /// whole-view frontends pass [`world_shape`]; a painter that draws
    /// square tiles for a cache never sets one.
    pub fn set_world_shape(&mut self, shape: Option<Shape>) {
        self.world_shape = shape;
    }

    /// The blended world mapping of `view` at its own whole zoom, when
    /// this painter blends world views at all
    /// ([`set_world_shape`](Self::set_world_shape)), the blend is not
    /// yet Mercator there, and
    /// no region reaches into the view — the view that shows the world
    /// outline alone, so no region tile is ever reprojected.
    pub fn world_blend(&mut self, view: &Viewport) -> Option<Blend> {
        let shape = self.world_shape?;
        self.world_blend_as(shape, view, f64::from(view.zoom()))
    }

    /// [`world_blend`](Self::world_blend) with the shape and the exact
    /// zoom given: what a tile cache asks for a frame between the steps.
    pub fn world_blend_as(&mut self, shape: Shape, view: &Viewport, zoom: f64) -> Option<Blend> {
        let (w, h) = view.size();
        let blend = Blend::new(shape, view.centre, zoom, w, h);
        if blend.t() >= 1.0 {
            return None;
        }
        self.world_only(view).then_some(blend)
    }

    /// Whether `view` shows the world picture alone: no installed region
    /// reaches into it at its zoom. The blend holds only there, so no
    /// region tile is ever reprojected.
    pub fn world_only(&mut self, view: &Viewport) -> bool {
        self.refresh();
        drawn(
            self.regions
                .iter()
                .map(|r| (r.name.as_str(), r.region.header())),
            view,
        )
        .is_empty()
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
        let blend = self.world_blend(view);
        self.draw_with(view, blend.as_ref(), ink, style, percent)
    }

    /// [`draw`](Self::draw) with the blend chosen by the caller (1710):
    /// the terminal's supersampled source draws one zoom deeper at a
    /// finer grain but must keep the nominal view's blend, `t` included
    /// ([`Blend::resized`]); `None` keeps Mercator whatever the
    /// painter's shape.
    pub fn draw_with(
        &mut self,
        view: &Viewport,
        blend: Option<&Blend>,
        ink: Ink,
        style: Style,
        percent: u32,
    ) -> Drawn {
        let (w, h) = view.size();
        let mut fb = vec![0u16; (w as usize) * (h as usize)];
        let (attribution, pending) = match Frame::new(&mut fb, w, h) {
            Some(mut frame) => match blend {
                Some(b) => self.draw_world_into(b, ink, style, &mut frame),
                None => self.draw_into(view, ink, style, percent, &mut frame),
            },
            None => (outline::ATTRIBUTION.to_string(), 0),
        };
        Drawn {
            fb,
            attribution,
            pending,
        }
    }

    /// [`draw`](Self::draw) into opaque 8-bit RGBA, four bytes a pixel:
    /// the same picture, each pixel the Rgb565 one widened by
    /// [`leviculum_mapfill::frame::rgb888`], without a pass to widen it
    /// afterwards (1422). What the tile cache holds. With `smooth` the
    /// fills' and lines' edges are blended by their coverage
    /// ([`leviculum_mapfill::coverage`]); without, the picture is the
    /// Rgb565 one exactly.
    pub fn draw_rgba(
        &mut self,
        view: &Viewport,
        ink: Ink,
        style: Style,
        percent: u32,
        smooth: bool,
    ) -> DrawnRgba {
        let (w, h) = view.size();
        // Opaque black, as the Rgb565 frame's zeros widen.
        let mut rgba = [0u8, 0, 0, 0xff].repeat((w as usize) * (h as usize));
        let blend = self.world_blend(view);
        let (attribution, pending) = match RgbaFrame::new(&mut rgba, w, h) {
            Some(frame) => match &blend {
                Some(b) => self.draw_world_into(b, ink, style, &mut frame.smoothed(smooth)),
                None => self.draw_into(view, ink, style, percent, &mut frame.smoothed(smooth)),
            },
            None => (outline::ATTRIBUTION.to_string(), 0),
        };
        DrawnRgba {
            rgba,
            attribution,
            pending,
        }
    }

    /// The blended world view drawn into `rgba`, opaque RGBA of the
    /// blend's size (1710): what a tile cache shows for a frame the blend
    /// holds — drawn whole each time, since no square tile of one step
    /// can compose a curved picture. The outline alone (the blend holds
    /// only where no region reaches), the time zone borders over it when
    /// that view is on; `smooth` antialiases the coasts and the globe's
    /// limb. What `rgba` held does not matter: the outline clears it
    /// first. Straight into the frontend's pixels (1797): a buffer of the
    /// frame's own beside them, copied over, was 10.7 MB a frame for the
    /// turning globe on a 1224 x 2197 screen and 24 MB for Equal Earth
    /// with its margins. The credit and the tiles owed; `None` for a
    /// buffer too short, left as it is (the tile cache sizes it).
    pub fn draw_world_onto(
        &mut self,
        blend: &Blend,
        ink: Ink,
        style: Style,
        smooth: bool,
        rgba: &mut [u8],
    ) -> Option<(String, usize)> {
        let (w, h) = blend.size();
        let frame = RgbaFrame::new(rgba, w, h)?;
        Some(self.draw_world_into(blend, ink, style, &mut frame.smoothed(smooth)))
    }

    /// [`draw_world_onto`](Self::draw_world_onto)'s picture into `frame`.
    fn draw_world_into<D>(
        &mut self,
        blend: &Blend,
        ink: Ink,
        style: Style,
        frame: &mut D,
    ) -> (String, usize)
    where
        D: Coverage<Color = Rgb565, Error = core::convert::Infallible>,
    {
        self.refresh();
        let mut attribution = outline::ATTRIBUTION.to_string();
        let Ok(_) = outline::draw_world_view(&outline::WORLD, blend, ink, style.theme, frame);
        if self.time_zones.is_some() {
            let (_, h) = blend.size();
            self.scratch.heads.resize(h as usize, 0);
            let mut scratch = Scratch {
                edges: &mut self.scratch.edges,
                heads: &mut self.scratch.heads,
                active: &mut self.scratch.active,
            };
            let mut filler = Filler::new(&mut scratch, frame.bounding_box());
            let Ok(_) = outline::bounds_view(
                &timezones::BOUNDARIES,
                blend,
                Point::zero(),
                crate::zoneclock::line_colour(style.theme),
                crate::zoneclock::line_colour(style.theme),
                &mut filler,
                frame,
            );
            attribution = format!("{attribution} · {}", timezones::ATTRIBUTION);
        }
        (attribution, 0)
    }

    /// [`draw`](Self::draw)'s picture into `frame`: its credit and the
    /// tiles owed.
    fn draw_into<D>(
        &mut self,
        view: &Viewport,
        ink: Ink,
        style: Style,
        percent: u32,
        frame: &mut D,
    ) -> (String, usize)
    where
        D: Coverage<Color = Rgb565, Error = core::convert::Infallible>,
    {
        self.refresh();
        let order = drawn(
            self.regions
                .iter()
                .map(|r| (r.name.as_str(), r.region.header())),
            view,
        );
        let mut attribution = outline::ATTRIBUTION.to_string();
        let mut shown: Vec<usize> = Vec::new();
        let mut pending = 0;
        // The outline goes under the view unless one region fills it.
        let whole = frame.bounding_box();
        let filled = order
            .iter()
            .any(|&i| self.regions[i].region.area(view) == whole);
        if !filled {
            // Hard, as the tile cache draws it under a composed frame
            // (`tiles::outline_under`): the two pictures agree there.
            let Ok(_) =
                outline::draw_world(&outline::WORLD, view, ink, style.theme, &mut Hard(frame));
        }
        for &i in &order {
            let installed = &self.regions[i];
            let area = installed.region.area(view);
            if let Some(owed) = region_into(
                &installed.region,
                view,
                area,
                ink,
                style,
                percent,
                &mut self.read,
                &mut self.buildings,
                &mut self.scratch,
                frame,
            ) {
                attribution = installed.region.attribution().to_string();
                shown.push(i);
                pending += owed;
            }
        }
        for i in shown {
            self.viewed(i);
        }
        if self.time_zones.is_some() {
            let (_, h) = view.size();
            self.scratch.heads.resize(h as usize, 0);
            let mut scratch = Scratch {
                edges: &mut self.scratch.edges,
                heads: &mut self.scratch.heads,
                active: &mut self.scratch.active,
            };
            let mut filler = Filler::new(&mut scratch, frame.bounding_box());
            let Ok(_) = outline::bounds(
                &timezones::BOUNDARIES,
                view,
                Point::zero(),
                crate::zoneclock::line_colour(style.theme),
                crate::zoneclock::line_colour(style.theme),
                &mut filler,
                frame,
            );
            attribution = format!("{attribution} · {}", timezones::ATTRIBUTION);
        }
        (attribution, pending)
    }

    /// The names `view` shows from every region that reaches into it, in
    /// the order [`Painter::draw`] draws them, streets too with `lines`; a
    /// region whose names do not read is left out. A version 1 region has
    /// none. Placed by the frontend, in its unit, by the core's rule
    /// ([`leviculum_map::labels::place`]).
    ///
    /// Where no region fills the view, the world view's own names follow
    /// (1683): the countries, seas, ranges, deserts and places of
    /// [`outline::NAMES_50M`] that the zoom shows, up to
    /// [`outline::MAX_ZOOM`], which is as deep as that view reaches.
    pub fn labels(&mut self, view: &Viewport, lines: bool) -> Vec<ViewLabel> {
        if self.names_off {
            return Vec::new();
        }
        // The blended world view's names and clocks anchor through the
        // blend (1710), the same condition as the picture's, so a name
        // sits on its blended ground.
        let blend = self.world_blend(view);
        if let Some(unix) = self.time_zones {
            return match &blend {
                Some(b) => crate::zoneclock::labels_view(b, unix),
                None => crate::zoneclock::labels(view, unix),
            };
        }
        if let Some(b) = &blend {
            let mut out = Vec::new();
            outline::names_view(outline::NAMES_50M, b, &mut out);
            return out;
        }
        self.refresh();
        let order = drawn(
            self.regions
                .iter()
                .map(|r| (r.name.as_str(), r.region.header())),
            view,
        );
        let mut out = Vec::new();
        for i in order.iter().copied() {
            let mut found = Vec::new();
            if self.regions[i]
                .region
                .view_labels(view, lines, &mut found)
                .is_ok()
            {
                out.extend(found);
            }
        }
        // Under the world picture, as `draw_into` drew it: no region fills
        // the view, so the outline's own names are what is on screen.
        let (w, h) = view.size();
        let whole = Rectangle::new(Point::zero(), (w, h).into());
        if !order
            .iter()
            .any(|&i| self.regions[i].region.area(view) == whole)
        {
            outline::names(outline::NAMES_50M, view, &mut out);
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

/// Draw `region`'s tiles of `view` into `area` of `frame`, with their
/// building footprints where the view draws them ([`draws_buildings`]);
/// how many of its tiles are not there yet (1291), `None` when a tile
/// could not be read, and the region is left out.
#[allow(clippy::too_many_arguments)]
fn region_into<D>(
    region: &Region<File>,
    view: &Viewport,
    area: Rectangle,
    ink: Ink,
    style: Style,
    percent: u32,
    read: &mut Vec<(ViewTile, Vec<u8>)>,
    buildings: &mut Vec<(TileId, Vec<u8>)>,
    fill: &mut FillScratch,
    frame: &mut D,
) -> Option<usize>
where
    D: Coverage<Color = Rgb565, Error = core::convert::Infallible>,
{
    let Ok(pending) = region.read_view(view, read) else {
        return None;
    };
    let Ok(mut tiles) = draw_tiles(read) else {
        return None;
    };
    buildings.clear();
    if draws_buildings(style, ink, view.zoom()) {
        // A section that does not read leaves the buildings out, never
        // the map under them.
        if region.read_buildings(&tiles, buildings).is_err()
            || attach_buildings(&mut tiles, buildings).is_err()
        {
            tiles.iter_mut().for_each(|t| t.buildings = None);
        }
    }
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
    Some(pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1683: where no region fills the view the world view's own names
    /// come with its picture — a country's, a sea's and a range's at the
    /// zooms the outline draws — and none past its deepest zoom, nor
    /// where a region fills the view.
    #[test]
    fn the_world_view_brings_its_names_where_no_region_fills_the_view() {
        use leviculum_map::labels::LabelKind;
        use leviculum_map::mercator::{project, LatLon};
        let dir = tempfile::tempdir().expect("tmp");
        let mut painter = Painter::new(Maps::new(Some(dir.path().to_path_buf())));
        let at = project(LatLon::from_degrees(50.0, 10.0));
        for zoom in [2u8, 6] {
            let view = Viewport::new(at, zoom, 480, 320);
            let labels = painter.labels(&view, false);
            println!("z{zoom}: {labels:?}");
            assert!(
                labels.iter().any(|l| l.kind == LabelKind::Country),
                "z{zoom}: no country"
            );
            // At z6 the view is central Germany: no sea, and the Alps'
            // label lies south of it, so the waters and the ranges are
            // asked of z2 and the places of z6, Frankfurt among them.
            if zoom == 6 {
                assert!(
                    labels
                        .iter()
                        .any(|l| l.text == "Frankfurt" && l.kind.is_place()),
                    "z6: no Frankfurt"
                );
            }
            if zoom == 2 {
                assert!(
                    labels
                        .iter()
                        .any(|l| matches!(l.kind, LabelKind::Sea | LabelKind::Range)),
                    "z{zoom}: no sea or range"
                );
            }
            // Every one is anchored in the view, in its sub-pixels.
            let (w, h) = view.size();
            let sub = i64::from(leviculum_mapfill::SUBPIXEL);
            for l in &labels {
                assert!(
                    (0..i64::from(w) * sub).contains(&l.anchor.0)
                        && (0..i64::from(h) * sub).contains(&l.anchor.1),
                    "z{zoom}: {l:?} outside the view"
                );
            }
        }
        // The control: past the outline's deepest zoom there is nothing to
        // name, and the picture is the outline's alone.
        let past = Viewport::new(at, 12, 480, 320);
        assert!(painter.labels(&past, false).is_empty());
        // And a region that fills the view names the view itself.
        let fixture =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures");
        let mut painter = Painter::new(Maps::new(Some(fixture)));
        painter.refresh();
        let region = painter
            .regions
            .first()
            .expect("the Florence cut")
            .region
            .header();
        let view = Viewport::new(
            project(LatLon {
                lat_e7: (region.bounds.north + region.bounds.south) / 2,
                lon_e7: (region.bounds.west + region.bounds.east) / 2,
            }),
            15,
            480,
            320,
        );
        let labels = painter.labels(&view, false);
        assert!(
            labels.iter().all(|l| !matches!(l.kind, LabelKind::Country)),
            "a region's view took the world's country names: {labels:?}"
        );
    }

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
                profile: leviculum_map::profile::CUT_2,
                attribution: "© OpenStreetMap contributors".to_string(),
                notice: "test".to_string(),
                threads: 1,
                locale: None,
                buildings: true,
                pois: true,
                round: leviculum_mapcut::cut::ROUND,
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
