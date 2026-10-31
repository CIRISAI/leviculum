//! The map screen's one call (1128): Java hosts a bitmap, Rust draws it.
//!
//! The frame is loquax's own map on a pixel screen: the regions of the
//! app's maps directory and the world outline under them
//! ([`loquax::picture`]), composed from loquax's tile cache
//! ([`loquax::tiles`]), the map's names set in Fira Sans
//! ([`loquax::mapnames`]), the pins placed by the core's rule
//! ([`leviculum_map::pins`]) and drawn in ([`loquax::screenmap`]), in the
//! map's dark table when the phone is dark, or in the table the map
//! screen's own switch chose (1133, the config's `map_theme`). The window
//! draws through the same functions, so the phone shows the map the
//! desktop shows.
//!
//! Java keeps the last frame's view and hands it back with what the finger
//! did since: a pan in pixels, a zoom in thousandths of a step about a
//! point, a tap. The view keeps the zoom a pinch leaves, between the steps
//! ([`ExactView`], 1134); fitting is the model's (`loquax::ui::map`:
//! [`fit`], [`fit_at`], one world at most), one pixel a cell as in the
//! window. The pins are the running session's, or the store's when none
//! runs ([`pins_in`]).
//!
//! A frame is drawn with margins past the screen (1131), so a finger that
//! pans reveals drawn map while the next frame is drawn; Java asks again
//! as the finger moves and shows each frame as it lands, moved by what the
//! finger did since it asked. The map comes from the tiles of the step
//! nearest the zoom, scaled to it; the names are placed at rest and kept,
//! and a frame while the finger moves draws them where the view moved
//! them, so they never leave the picture (1134). While a pinch stays
//! within a step of the tiles it started on, it keeps them, and a frame
//! draws at most [`MOVING_TILES_PER_WORKER`] missing tiles a worker,
//! showing the step before where the rest are owed. The frame itself is
//! [`loquax::mapframe`]'s, which the window draws through too (1144).
//!
//! A frame at rest that owes no tile leaves the view to prefetch (1135):
//! Java then asks with `prefetch` while no finger is down and nothing else
//! is owed, and each such call draws one batch of [`loquax::tiles::plan`]'s
//! tiles, one a worker, ordered by the pan's direction ([`Motion`](loquax::tiles::Motion)), up
//! to the plan's budget. Any frame drops the plan; the next frame at rest
//! makes a new one. When the ring reaches past the regions that hold the
//! frame, the autoload is asked for the area the ring covers.
//!
//! The autoload (1089) is the terminal's and the window's: the places of
//! the first pins when they first fit, a report's place when it opens the
//! map, the view on "load this area", one load at a time into the maps
//! directory under the config's `map_source` and `map_cache_mb`. A frame
//! says when a load runs, and Java asks again until it landed.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use leviculum_map::mercator::{project, LatLon, World};
use leviculum_map::viewport::{ExactView, MILLI};
use leviculum_mapfill::style::{MapTheme, Style};
use leviculum_ui::theme::{Theme, ThemeFlag};
use loquax::mapframe::{frame_size, hold, Ask, CarryCost, Framed, Frames};
use loquax::mapload::{Loader, Source, Step, CACHE_DEFAULT_MB};
use loquax::picture::{map_theme, Maps};
use loquax::screenmap;
use loquax::tiles::{Frame as TileFrame, Stats, TILE};
use loquax::ui::map::{fit, fit_at, want_pins, want_place, want_view, zoom_floor, MapPin, MapWant};
use loquax::when::WallTime;

use crate::friends;
use crate::schema::{
    MapMark, MapRegions, MapRequest, MapShown, MapThemeChoice, RegionRow, VERSION,
};
use crate::session::pins_in;

pub use loquax::mapframe::MOVING_TILES_PER_WORKER;

/// What the map screen says on "load this area" over a view the regions
/// hold already: the terminal's words.
pub const LOADED_ALREADY: &str = "map: this area is loaded already";

/// Pixels a frontend hands the renderer to compose a frame into (1155):
/// the app's Bitmap, locked while the frame is drawn (`bitmap.rs`, in the
/// Android build only), so the frame is never copied on its way to the
/// screen.
pub trait Canvas {
    /// `w` x `h` RGBA rows, tightly packed, to compose into: what they
    /// hold before does not matter. A line for the user when there are
    /// none of that size.
    fn pixels(&mut self, w: u32, h: u32) -> Result<&mut [u8], String>;
}

/// The renderer of one app directory, kept from frame to frame: the
/// frames with their tile cache and names, the loader, the fit.
pub struct Renderer {
    dir: PathBuf,
    /// The directory the frames and the loader share: a region deleted is
    /// told to both (1201).
    maps: Maps,
    frames: Frames,
    loader: Loader,
    autoload: bool,
    theme: Option<ThemeFlag>,
    origin: Instant,
    /// The clock a test holds instead of [`origin`](Self::origin)'s
    /// ([`tune_clock`]).
    held_ms: Option<u64>,
    /// The view the last fit chose: a region that lands fits again while
    /// the view is still there.
    fitted_at: Option<(World, u32)>,
    /// Whether a fit with pins asked for their places already.
    pins_asked: bool,
    /// A line a prefetch's region load left for the next frame.
    notice_owed: Option<String>,
}

impl Renderer {
    /// A renderer over `dir`: its `maps/`, its config.
    pub fn new(dir: &Path) -> Self {
        let config = loquax::config::load(dir).unwrap_or_default();
        let maps = Maps::new(Some(dir.join("maps")));
        let source = Source::of(config.map_source.as_deref());
        let cache = config.map_cache_mb.unwrap_or(CACHE_DEFAULT_MB) << 20;
        Self {
            dir: dir.to_path_buf(),
            maps: maps.clone(),
            frames: Frames::new(maps.clone()),
            notice_owed: None,
            loader: Loader::new(maps, source, cache),
            autoload: config.map_autoload.unwrap_or(true),
            theme: config.map_theme,
            origin: Instant::now(),
            held_ms: None,
            fitted_at: None,
            pins_asked: false,
        }
    }

    fn now_ms(&self) -> u64 {
        self.held_ms
            .unwrap_or_else(|| u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    /// Ask for `want` when the autoload is on; a line for the user when it
    /// failed at once.
    fn ask(&mut self, want: MapWant, notice: &mut Option<String>) {
        if !self.autoload {
            return;
        }
        let now = self.now_ms();
        let steps = self.loader.ask(want, now);
        self.steps(steps, notice);
    }

    /// The loader's steps: a landed region is read before the frame is
    /// drawn, a failure's line is the frame's notice. Whether a region
    /// landed.
    fn steps(&mut self, steps: Vec<Step>, notice: &mut Option<String>) -> bool {
        let mut landed = false;
        for step in steps {
            match step {
                Step::Done {
                    want,
                    landed: l,
                    elapsed_ms,
                } => {
                    tracing::info!(
                        "MAPLOAD done zooms={}-{} bytes={} file={} elapsed_ms={elapsed_ms} reason={}",
                        l.zooms.0,
                        l.zooms.1,
                        l.bytes,
                        l.name,
                        want.reason.word()
                    );
                    landed = true;
                }
                Step::Failed { detail, toast, .. } => {
                    tracing::info!("MAPLOAD failed detail={detail:?}");
                    if toast.is_some() {
                        *notice = toast;
                    }
                }
                Step::Started(want) => {
                    tracing::info!("MAPLOAD start reason={}", want.reason.word());
                }
                Step::Skipped { why, .. } => tracing::info!("MAPLOAD skipped why={why}"),
            }
        }
        if landed {
            self.frames.refresh();
        }
        landed
    }

    /// The map's own light or dark choice (1133): a toggle stores the
    /// table other than the one the map shows over `screen`, "follow the
    /// phone" stores `auto`; both in the config, so the choice outlives
    /// the app. A write that failed keeps the old choice and says so.
    fn choose_theme(&mut self, request: &MapRequest, screen: Theme, notice: &mut Option<String>) {
        let next = if request.follow_phone {
            ThemeFlag::Auto
        } else if request.toggle_theme {
            match map_theme(self.theme.or(Some(ThemeFlag::Auto)), Some(screen)) {
                MapTheme::Dark => ThemeFlag::Light,
                MapTheme::Light => ThemeFlag::Dark,
            }
        } else {
            return;
        };
        if let Err(error) = self.store_theme(next) {
            *notice = Some(error);
        }
    }

    /// Store `next` as the config's `map_theme` and draw in it from the
    /// next frame; a write that failed keeps the old choice and says why.
    fn store_theme(&mut self, next: ThemeFlag) -> Result<(), String> {
        loquax::config::save(&self.dir, "map_theme", theme_word(Some(next)))
            .map_err(|error| format!("map: the choice was not stored: {error}"))?;
        self.theme = Some(next);
        Ok(())
    }

    /// The Appearance page's map theme (1199, 1207): `set`, a word of
    /// the config's `map_theme`, stored when given; the choice in force
    /// back, no frame drawn.
    pub fn theme(&mut self, set: Option<&str>) -> Result<MapThemeChoice, String> {
        if let Some(word) = set {
            let next = match word {
                "auto" => ThemeFlag::Auto,
                "light" => ThemeFlag::Light,
                "dark" => ThemeFlag::Dark,
                other => return Err(format!("map: no theme {other:?}")),
            };
            self.store_theme(next)?;
        }
        Ok(MapThemeChoice {
            v: VERSION,
            ok: true,
            theme_chosen: matches!(self.theme, Some(ThemeFlag::Light | ThemeFlag::Dark)),
            theme: theme_word(self.theme).to_string(),
        })
    }

    /// The Maps page (1201, 1207): `delete` and `set_autoload` done when
    /// asked, then the regions of the directory, newest first, their
    /// bytes together and whether the autoload is on; no frame drawn. A
    /// name that is no region file of the directory deletes nothing.
    pub fn regions(
        &mut self,
        delete: Option<&str>,
        set_autoload: Option<bool>,
    ) -> Result<MapRegions, String> {
        if let Some(on) = set_autoload {
            let word = if on { "on" } else { "off" };
            loquax::config::save(&self.dir, "map_autoload", word)
                .map_err(|error| format!("map: the choice was not stored: {error}"))?;
            self.autoload = on;
        }
        let maps = self
            .maps
            .dir()
            .map_or_else(|| self.dir.join("maps"), Path::to_path_buf);
        if let Some(name) = delete {
            let plain = !name.is_empty()
                && !name.contains(['/', '\\'])
                && !name.starts_with('.')
                && name.ends_with(".lmr");
            if !plain || !maps.join(name).is_file() {
                return Err(format!("map: no region {name:?}"));
            }
            std::fs::remove_file(maps.join(name))
                .map_err(|error| format!("map: {name} was not deleted: {error}"))?;
            tracing::info!("MAPLOAD deleted file={name}");
            self.maps.changed();
            self.frames.refresh();
            self.fitted_at = None;
        }
        let (installed, _) = loquax::picture::load(&maps);
        let regions: Vec<RegionRow> = loquax::picture::newest_first(&installed)
            .into_iter()
            .map(|region| {
                let header = region.region.header();
                let b = header.bounds;
                // The bounds are degrees times ten million; the page's
                // middle is times a million, as every place that crosses.
                let middle = |a: i32, z: i32| ((i64::from(a) + i64::from(z)) / 20) as i32;
                RegionRow {
                    name: region.name.clone(),
                    bytes: std::fs::metadata(maps.join(&region.name)).map_or(0, |m| m.len()),
                    modified: region
                        .modified
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)),
                    lat_e6: middle(b.south, b.north),
                    lon_e6: middle(b.west, b.east),
                    min_zoom: header.min_zoom,
                    max_zoom: header.max_zoom,
                    auto: loquax::picture::is_auto(&region.name),
                }
            })
            .collect();
        Ok(MapRegions {
            v: VERSION,
            ok: true,
            region_bytes: regions.iter().map(|r| r.bytes).sum(),
            regions,
            autoload: self.autoload,
        })
    }

    /// One frame for `request` over `pins`, in a buffer of its own.
    pub fn frame(&mut self, request: &MapRequest, pins: &[MapPin]) -> (MapShown, Vec<u8>) {
        let mut rgba = Vec::new();
        let shown = self.draw(request, pins, |frames, ask| {
            let (pixels, framed) = frames.frame(ask).split();
            rgba = pixels;
            Ok(framed)
        });
        (shown, rgba)
    }

    /// One frame for `request` over `pins`, composed straight into the
    /// pixels `canvas` gives for its size (the app's Bitmap, 1155): no
    /// buffer of the frame's own, no copy.
    pub fn frame_into(
        &mut self,
        request: &MapRequest,
        pins: &[MapPin],
        canvas: &mut dyn Canvas,
    ) -> MapShown {
        self.draw(request, pins, |frames, ask| {
            let (w, h) = frame_size(ask);
            let pixels = canvas.pixels(w, h)?;
            frames
                .frame_into(ask, pixels)
                .ok_or_else(|| format!("map: the pixels are not {w}x{h}"))
        })
    }

    /// One frame for `request` over `pins`, drawn by `draw` from the
    /// frames and the frame asked; a failed draw's line is the answer.
    fn draw(
        &mut self,
        request: &MapRequest,
        pins: &[MapPin],
        draw: impl FnOnce(&mut Frames, &Ask<'_>) -> Result<Framed, String>,
    ) -> MapShown {
        let started = Instant::now();
        let mut notice = self.notice_owed.take();
        let now = self.now_ms();
        let polled = self.loader.poll(now);
        let landed = self.steps(polled, &mut notice);
        let setup = screenmap::setup(self.frames.painter());
        let area = screenmap::area((request.w.max(1), request.h.max(1)));
        let scale = (request.density_milli.max(100) as f32) / 1000.0;
        let mut selected = request
            .selected
            .as_deref()
            .and_then(|hex| loquax::address::parse(hex).ok());
        let floor = u32::from(zoom_floor(area, &setup)) * MILLI;
        let ceiling = u32::from(setup.zooms.1) * MILLI;
        let zooms = (zoom_floor(area, &setup), setup.zooms.1);
        let asked = request
            .zoom_milli
            .or(request.zoom.map(|z| u32::from(z) * MILLI));
        let mut view = ExactView::new(
            World {
                x: request.x.unwrap_or(0),
                y: request.y.unwrap_or(0),
            },
            asked.unwrap_or(u32::from(setup.zooms.0) * MILLI),
            u32::from(area.width),
            u32::from(area.height),
        );
        let at = match (request.at_lat_e6, request.at_lon_e6) {
            (Some(lat), Some(lon)) => Some(project(LatLon::from_e6(lat, lon))),
            _ => None,
        };
        let first = request.x.is_none() || request.y.is_none() || asked.is_none();
        let fitted = |view: &ExactView| (view.centre, view.zoom_milli());
        let refit = landed && self.fitted_at == Some(fitted(&view));
        let fit_to = |view: &mut ExactView, (centre, zoom): (World, u8)| {
            *view = ExactView::new(
                centre,
                u32::from(zoom) * MILLI,
                view.size().0,
                view.size().1,
            );
        };
        if let Some(at) = at {
            match request.at_zoom_milli {
                // The crosshair's zoom and follow mode's (1201), held to
                // the regions' zooms below.
                Some(zoom) => {
                    view = ExactView::new(
                        at,
                        zoom.clamp(floor, ceiling.max(floor)),
                        view.size().0,
                        view.size().1,
                    )
                }
                None => fit_to(&mut view, fit_at(at, area, &setup)),
            }
            self.fitted_at = None;
            if let Some(want) = want_place(at, &setup.regions) {
                self.ask(want, &mut notice);
            }
        } else if first || request.fit || refit {
            fit_to(&mut view, fit(pins, area, &setup));
            self.fitted_at = Some(fitted(&view));
            if selected.is_none() {
                selected = pins.first().map(|p| p.peer);
            }
            if !self.pins_asked && !pins.is_empty() {
                self.pins_asked = true;
                for want in want_pins(pins, &setup.regions) {
                    self.ask(want, &mut notice);
                }
            }
        }
        let held = |v: ExactView| hold(v, zooms.0, zooms.1);
        view = held(view);
        if request.pan_x != 0 || request.pan_y != 0 {
            self.frames
                .pan(f64::from(request.pan_x), f64::from(request.pan_y), now);
            view.pan(request.pan_x, request.pan_y);
            view = held(view);
        }
        let by = i64::from(request.zoom_by) * i64::from(MILLI) + i64::from(request.zoom_by_milli);
        self.frames.zoom(by);
        self.frames.asked(now);
        if by != 0 {
            let zoom = (i64::from(view.zoom_milli()) + by)
                .clamp(i64::from(floor), i64::from(ceiling.max(floor)));
            let about = |v: i32, of: u16| f64::from(v.clamp(0, i32::from(of)));
            view.zoom_about(
                u32::try_from(zoom).unwrap_or(floor),
                about(request.about_x, area.width),
                about(request.about_y, area.height),
            );
            view = held(view);
        }
        // At most a screen on each side: a frame of nine screens is as
        // far as any gesture between two frames reaches.
        let (mx, my) = (
            request.margin_x.min(area.width.into()),
            request.margin_y.min(area.height.into()),
        );
        let mut tapped = None;
        if let (Some(x), Some(y)) = (request.tap_x, request.tap_y) {
            // The pins were placed in the last frame, margins included.
            let (x, y) = (x as f32 + mx as f32, y as f32 + my as f32);
            tapped = screenmap::hit(self.frames.placed(), x, y, scale);
            if tapped.is_some() {
                selected = tapped;
            }
        }
        let rest_zoom = view.tile_zoom();
        if request.load {
            match want_view(&view.at_zoom(rest_zoom), &setup.regions) {
                Some(want) => self.ask(want, &mut notice),
                None => notice = Some(LOADED_ALREADY.to_string()),
            }
        }
        let screen = match request.dark {
            true => Theme::Dark,
            false => Theme::Light,
        };
        self.choose_theme(request, screen, &mut notice);
        // Unset follows the phone; the config's `map_theme` still wins.
        let flag = self.theme.or(Some(ThemeFlag::Auto));
        let table = map_theme(flag, Some(screen));
        let style = Style::PC.in_theme(table);
        let ask = Ask {
            view,
            margin: (mx, my),
            moving: request.moving,
            style,
            scale,
            pins,
            selected,
            zooms,
            draw_pins: !request.avatars,
            measure: None,
            scaled_while_moving: true,
        };
        let shot = match draw(&mut self.frames, &ask) {
            Ok(shot) => shot,
            Err(error) => return failed(error),
        };
        let (w, h) = shot.view.size();
        let marks = match request.avatars {
            true => marks(pins, &shot.shows, (mx, my), scale, ceiling.max(floor)),
            false => Vec::new(),
        };
        let no_map =
            !self.loader.busy() && !any_region(&setup.regions, &shot.shows.at_zoom(rest_zoom));
        let name = |peer: [u8; 16]| pins.iter().find(|p| p.peer == peer).map(|p| p.words());
        let own = match (request.own_lat_e6, request.own_lon_e6) {
            (Some(lat), Some(lon)) => {
                let (x, y) = shot.shows.to_screen(project(LatLon::from_e6(lat, lon)));
                let pixel =
                    |v: f64| v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
                Some((pixel(x), pixel(y)))
            }
            _ => None,
        };
        let shown = MapShown {
            v: VERSION,
            ok: true,
            error: None,
            w,
            h,
            margin_x: mx,
            margin_y: my,
            // The view the frame shows: while a pinch moves, at the tile
            // step, and the Canvas scales it to the fingers' zoom (1155).
            x: shot.shows.centre.x,
            y: shot.shows.centre.y,
            zoom: rest_zoom,
            zoom_milli: shot.shows.zoom_milli(),
            tile_zoom: shot.tile_zoom,
            pending: shot.missing > 0,
            min_zoom: zoom_floor(area, &setup),
            max_zoom: setup.zooms.1,
            selected: selected.map(|p| loquax::address::to_hex(&p)),
            attribution: shot.attribution,
            pins: u32::try_from(self.frames.placed().len()).unwrap_or(u32::MAX),
            tapped: tapped.map(|p| loquax::address::to_hex(&p)),
            tapped_name: tapped.and_then(name),
            loading: self.loader.busy(),
            notice,
            ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
            dark: table == MapTheme::Dark,
            theme_chosen: matches!(self.theme, Some(ThemeFlag::Light | ThemeFlag::Dark)),
            theme: theme_word(self.theme).to_string(),
            prefetching: self.frames.at_rest(),
            prefetched: 0,
            marks,
            offline: self.loader.offline(),
            no_map,
            own_x: own.map(|o| o.0),
            own_y: own.map(|o| o.1),
            autoload: self.autoload,
        };
        shown
    }

    /// Take a frame's pixels back once they are copied out: the next
    /// frame is composed into them.
    pub fn recycle(&mut self, rgba: Vec<u8>) {
        self.frames.recycle(rgba);
    }

    /// One batch of the prefetch around the last frame at rest (1135):
    /// on the first call the plan, ordered by the pan's direction now, and
    /// the region past the frame's asked for; then a tile a worker. The
    /// answer has no pixels; `prefetching` says whether more is to draw.
    pub fn prefetch(&mut self) -> MapShown {
        let started = Instant::now();
        if !self.frames.at_rest() {
            return MapShown {
                v: VERSION,
                ok: true,
                ..MapShown::default()
            };
        }
        let now = self.now_ms();
        let done = self.frames.prefetch(now);
        if let Some(frame) = done.planned {
            self.ask_past(&frame);
        }
        MapShown {
            v: VERSION,
            ok: true,
            prefetching: done.more,
            prefetched: u32::try_from(done.drawn).unwrap_or(u32::MAX),
            loading: self.loader.busy(),
            ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
            ..MapShown::default()
        }
    }

    /// When the regions hold `frame` and not the ring around it, ask the
    /// autoload for the ring's area: it caps the size and backs off as for
    /// any view. A frame the regions do not hold loads nothing unasked.
    fn ask_past(&mut self, frame: &TileFrame) {
        let setup = screenmap::setup(self.frames.painter());
        if want_view(&frame.view.at_zoom(frame.zoom), &setup.regions).is_some() {
            return;
        }
        let mut grown = frame.view;
        let (w, h) = grown.size();
        let ring = (2.0 * f64::from(TILE) * frame.view.scale_at(frame.zoom)).ceil() as u32;
        grown.resize(
            w.saturating_add(ring.saturating_mul(2)),
            h.saturating_add(ring.saturating_mul(2)),
        );
        if let Some(want) = want_view(&grown.at_zoom(frame.zoom), &setup.regions) {
            let mut notice = None;
            self.ask(want, &mut notice);
            if notice.is_some() {
                self.notice_owed = notice;
            }
        }
    }

    /// What the tile cache did, for a test and the measuring tool.
    pub fn tile_stats(&self) -> Stats {
        self.frames.tile_stats()
    }

    /// The tile cache's tiles and bytes.
    pub fn tile_memory(&self) -> (usize, usize) {
        self.frames.tile_memory()
    }

    /// The names the last frame at rest placed, as text.
    pub fn names(&self) -> Vec<String> {
        self.frames.names()
    }
}

/// The avatar pins of `pins` in the screen's view `shows` (1200), in
/// view pixels, clustered by [`friends::cluster`] with circles of
/// [`friends::PIN_DP`] at `scale`: those that land in the frame, its
/// `margin` on each side included; each cluster with the zoom that parts
/// it, no deeper than `max_milli`.
pub fn marks(
    pins: &[MapPin],
    shows: &ExactView,
    margin: (u32, u32),
    scale: f32,
    max_milli: u32,
) -> Vec<MapMark> {
    let (w, h) = shows.size();
    let (mx, my) = (f64::from(margin.0), f64::from(margin.1));
    let in_frame = |(x, y): (f64, f64)| {
        (-mx..f64::from(w) + mx).contains(&x) && (-my..f64::from(h) + my).contains(&y)
    };
    let landed: Vec<(&MapPin, (f64, f64))> = pins
        .iter()
        .map(|pin| (pin, shows.to_screen(pin.at)))
        .filter(|(_, at)| in_frame(*at))
        .collect();
    let points: Vec<(f64, f64)> = landed.iter().map(|(_, at)| *at).collect();
    let diameter = f64::from(friends::PIN_DP) * f64::from(scale);
    friends::cluster(&points, diameter)
        .into_iter()
        .map(|members| {
            let (x, y) = points[members[0]];
            MapMark {
                x: x.round() as i32,
                y: y.round() as i32,
                peers: members
                    .iter()
                    .map(|&i| loquax::address::to_hex(&landed[i].0.peer))
                    .collect(),
                split_milli: friends::split_zoom(
                    &points,
                    &members,
                    shows.zoom_milli(),
                    diameter,
                    max_milli,
                ),
            }
        })
        .collect()
}

/// Whether any of `regions` reaches into `view` at some zoom: without one
/// the map shows only the world's outline there, "No map here yet"
/// (1200). A box across the antimeridian counts as reaching in.
fn any_region(
    regions: &[leviculum_map::region::Header],
    view: &leviculum_map::viewport::Viewport,
) -> bool {
    let b = view.bounds();
    regions.iter().any(|h| {
        let r = h.bounds;
        r.west > r.east
            || b.west > b.east
            || (r.west <= b.east && b.west <= r.east && r.south <= b.north && b.south <= r.north)
    })
}

/// The process's renderers, one a directory: the app has one; a test
/// process one a test.
static RENDERERS: Mutex<Vec<Renderer>> = Mutex::new(Vec::new());

/// One map frame for `request`, as [`crate::schema::frame`] lays it out.
/// The pins are `pins`' when given, the running session's, or the store's
/// in the request's directory. The host's path, the tests' and the
/// measuring tools': the app composes into its Bitmap ([`render_into`]).
pub fn render(request: &MapRequest, pins: Option<Vec<MapPin>>, now: WallTime) -> Vec<u8> {
    with_renderer(request, pins, now, |renderer, pins| {
        if request.prefetch {
            return crate::schema::frame(&renderer.prefetch(), &[]);
        }
        let (shown, rgba) = renderer.frame(request, pins);
        let framed = crate::schema::frame(&shown, &rgba);
        // Copied into the answer: the next frame is composed into it (1154).
        renderer.recycle(rgba);
        framed
    })
}

/// One map frame for `request` composed straight into `canvas`, the
/// app's Bitmap (1155), and what it shows; a prefetch draws into none.
/// The pins as for [`render`].
pub fn render_into(
    request: &MapRequest,
    pins: Option<Vec<MapPin>>,
    now: WallTime,
    canvas: &mut dyn Canvas,
) -> MapShown {
    with_renderer(request, pins, now, |renderer, pins| {
        match request.prefetch {
            true => renderer.prefetch(),
            false => renderer.frame_into(request, pins, canvas),
        }
    })
}

/// The Appearance page's map theme (1207) of the app directory `dir`:
/// [`Renderer::theme`] on the renderer the map draws with.
pub fn theme(dir: &Path, set: Option<&str>) -> Result<MapThemeChoice, String> {
    with_dir(dir, |renderer| renderer.theme(set))
}

/// The Maps page (1207) of the app directory `dir`: [`Renderer::regions`]
/// on the renderer the map draws with, so a deleted region leaves its
/// frames and its loader too.
pub fn regions(
    dir: &Path,
    delete: Option<&str>,
    set_autoload: Option<bool>,
) -> Result<MapRegions, String> {
    with_dir(dir, |renderer| renderer.regions(delete, set_autoload))
}

/// `f` with the renderer of the request's directory, made on its first
/// frame, and the pins: `pins` when given, the store's otherwise.
fn with_renderer<T>(
    request: &MapRequest,
    pins: Option<Vec<MapPin>>,
    now: WallTime,
    f: impl FnOnce(&mut Renderer, &[MapPin]) -> T,
) -> T {
    let dir = PathBuf::from(&request.dir);
    let pins = match pins {
        Some(pins) => Ok(pins),
        None => pins_in(&dir, now),
    };
    let pins = match pins {
        Ok(pins) => pins,
        Err(error) => {
            tracing::info!("map: no pins: {error}");
            Vec::new()
        }
    };
    with_dir(&dir, |renderer| f(renderer, &pins))
}

/// `f` with the renderer of `dir`, made on first use.
fn with_dir<T>(dir: &Path, f: impl FnOnce(&mut Renderer) -> T) -> T {
    let mut renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let at = match renderers.iter().position(|r| r.dir == dir) {
        Some(at) => at,
        None => {
            renderers.push(Renderer::new(dir));
            renderers.len() - 1
        }
    };
    f(&mut renderers[at])
}

/// What the tile cache of the renderer over `dir` did, when one runs:
/// for a test that a pin costs no tile.
pub fn tile_stats(dir: &Path) -> Option<Stats> {
    let renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    renderers
        .iter()
        .find(|r| r.dir == dir)
        .map(Renderer::tile_stats)
}

/// What the last frame that zoomed of the renderer over `dir` spent on
/// the names it carried (1205): the measuring tool's row.
pub fn carry_cost(dir: &Path) -> Option<CarryCost> {
    let renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    renderers
        .iter()
        .find(|r| r.dir == dir)
        .map(|r| r.frames.carry_cost())
}

/// The tiles and bytes the tile cache of the renderer over `dir` holds.
pub fn tile_memory(dir: &Path) -> Option<(usize, usize)> {
    let renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    renderers
        .iter()
        .find(|r| r.dir == dir)
        .map(Renderer::tile_memory)
}

/// The bytes the names of the last frame at rest of the renderer over
/// `dir` hold as sprites, and the milliseconds drawing them again into a
/// frame of `w` by `h` pixels takes: the measuring tool's sprite row.
pub fn sprite_cost(dir: &Path, w: u32, h: u32) -> Option<(usize, f64)> {
    let renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let r = renderers.iter().find(|r| r.dir == dir)?;
    // Written before the clock starts, so its pages are mapped.
    let mut rgba = vec![255u8; w as usize * h as usize * 4];
    let started = Instant::now();
    r.frames.draw_names(&mut rgba, w, h, MapTheme::Light);
    let ms = started.elapsed().as_secs_f64() * 1e3;
    std::hint::black_box(&rgba);
    Some((r.frames.sprite_bytes(), ms))
}

/// Draw the tiles of the renderer over `dir` on `n` threads at most, and
/// with `clear` drop the tiles it holds: the measuring tool's one-core
/// row and its cold frames.
pub fn tune_tiles(dir: &Path, n: Option<usize>, clear: bool) {
    let mut renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(r) = renderers.iter_mut().find(|r| r.dir == dir) {
        if let Some(n) = n {
            r.frames.set_workers(n);
        }
        if clear {
            r.frames.clear_tiles();
        }
    }
}

/// Hold the clock of the renderer over `dir` at `now_ms`, or with `None`
/// let it run again. The pan's velocity, which orders the prefetch
/// (1135), is pixels over these milliseconds: a test that reads the
/// order holds them, or how fast the host draws decides it (1167).
pub fn tune_clock(dir: &Path, now_ms: Option<u64>) {
    let mut renderers = RENDERERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(r) = renderers.iter_mut().find(|r| r.dir == dir) {
        r.held_ms = now_ms;
    }
}

/// The config's word for a map theme; unset follows the phone.
fn theme_word(flag: Option<ThemeFlag>) -> &'static str {
    match flag {
        None | Some(ThemeFlag::Auto) => "auto",
        Some(ThemeFlag::Light) => "light",
        Some(ThemeFlag::Dark) => "dark",
    }
}

/// A frame that says why there is no picture.
pub fn failed(error: impl Into<String>) -> MapShown {
    MapShown {
        v: VERSION,
        ok: false,
        error: Some(error.into()),
        ..MapShown::default()
    }
}
