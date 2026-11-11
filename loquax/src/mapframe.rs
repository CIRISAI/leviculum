//! The map on a pixel screen frame by frame (1134, 1135, 1144): what the
//! phone's map screen and the window share between the view a frontend
//! keeps and the RGBA it shows.
//!
//! A frontend keeps an [`ExactView`], the zoom between the steps a pinch
//! or a wheel leaves, held to one world and the regions' zooms by
//! [`hold`], and asks [`Frames::frame`] for it with the pins and whether
//! the view is moving ([`Ask`]). The frame is drawn with margins past the
//! screen, so a pan reveals drawn map while the next frame is drawn. The
//! map comes from [`TileCache`]: the tiles of the step nearest the zoom,
//! scaled to it, and while the view moves the step the last frame was
//! composed from as long as the zoom is within a step of it, with at most
//! [`MOVING_TILES_PER_WORKER`] missing tiles drawn a worker, the rest
//! shown from cached steps and owed ([`Shot::missing`]: ask again).
//!
//! The names are placed at rest, over the frame after the pins, and kept
//! as sprites; a pan at the zoom at rest draws them where the view moved
//! them ([`moved`]), so they never leave the picture. A frame that zooms
//! places them anew, as a frame at rest at its zoom would (1225): the
//! ladder of its zoom, the anchors and the lines of its own geometry, the
//! text at its screen size, the rule's collisions; into a frame the
//! frontend scales ([`Ask::scaled_while_moving`]) each at the inverse of
//! that scale ([`Sprite::scaled`]), so it shows at the size a frame at
//! rest gives it. The names it looks up are kept for a while: those of a
//! wider view at its step, until the zoom leaves the step or the view
//! leaves them ([`PlaceCost::looked_up`]). A name that enters fades in
//! over [`FADE_MS`] and one that leaves fades out where the map moves it,
//! by the frontend's clock ([`Ask::now_ms`]); while one fades the frame
//! says so ([`Shot::fading`]: ask again), and a frame at rest asked again
//! keeps its placement. The pins are
//! placed in every frame, and drawn in unless the frontend sets them
//! itself ([`Ask::draw_pins`]).
//!
//! A frame at rest that owes no tile leaves the view to prefetch (1135):
//! [`Frames::prefetch`] then draws one batch of [`tiles::plan`]'s tiles a
//! call, one a worker, ordered by the pan's direction ([`Motion`], fed by
//! [`Frames::pan`] and [`Frames::zoom`]), up to the plan's budget. Any
//! frame drops the plan; the next frame at rest makes a new one.
//!
//! The cache drops its tiles when the maps directory's epoch or the style
//! changes (the theme, the screen's density), so a frontend needs no
//! invalidation of its own.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use leviculum_map::labels::{LabelKind, Rect, ViewLabel, SUBPIXEL};
use leviculum_map::ladder::{Ladder, Layers, Pass};
use leviculum_map::viewport::{ExactView, Viewport, MILLI};
use leviculum_mapfill::style::{MapTheme, Style};

use crate::mapnames::{self, Mask, Mip, PlacedName, Sprite, Typesetter, MIP_BELOW};
use crate::picture::{Maps, Painter};
use crate::screenmap::{self, Placed};
use crate::tiles::{self, Frame as TileFrame, Motion, Plan, Stats, TileCache, BUDGET_MB};
use crate::ui::map::MapPin;

/// Missing tiles a frame draws while the view moves, per worker thread:
/// the rest are shown from the step before and drawn in the frames that
/// follow.
pub const MOVING_TILES_PER_WORKER: usize = 2;

/// One frame asked of [`Frames::frame`].
#[derive(Clone, Copy)]
pub struct Ask<'a> {
    /// The screen's view, held ([`hold`]).
    pub view: ExactView,
    /// Pixels drawn past the screen on the left and right, and on the top
    /// and bottom.
    pub margin: (u32, u32),
    /// A finger or a pointer moves the view: the names are kept, the
    /// tiles drawn a few a frame.
    pub moving: bool,
    pub style: Style,
    /// Screen pixels a logical one.
    pub scale: f32,
    pub pins: &'a [MapPin],
    /// The tracks, drawn over the names and under the pins (1385).
    pub tracks: &'a [crate::ui::tracks::Track],
    /// The radar where the view has no region data (1386): the ground
    /// drawn blank with its rings, no names; the tracks and the pins as
    /// ever ([`crate::ui::radar`]).
    pub radar: Option<&'a crate::ui::radar::Radar>,
    pub selected: Option<[u8; 16]>,
    /// The zooms the regions allow, the floor the screen's width sets
    /// first: how far a prefetch goes out and in.
    pub zooms: (u8, u8),
    /// Draw the pins into the picture; a frontend that sets them itself
    /// over it asks without.
    pub draw_pins: bool,
    /// The width of a pin's label as the frontend sets it, when it sets
    /// them itself; the map's own typesetter's otherwise.
    pub measure: Option<fn(&str, f32) -> u32>,
    /// The frontend's clock in milliseconds, which the names fade in and
    /// out by ([`FADE_MS`], 1205); `None` for a still picture, a test or a
    /// measuring tool, where a name shows or goes at once.
    pub now_ms: Option<u64>,
    /// The frontend scales a frame on screen to the view it keeps, with a
    /// filter (the phone's Canvas, the window's image): a frame while a
    /// pinch or a wheel zooms (moving, at a zoom other than the last frame
    /// at rest's) is then drawn at its tile step one to one
    /// ([`Shot::shows`]), the tiles copied and not resampled, and the
    /// frontend scales it the rest of the way (1155). A pan at the zoom
    /// at rest stays drawn at it, its names as sharp as at rest.
    pub scaled_while_moving: bool,
    /// The time zone view at this instant, in seconds since the epoch
    /// (1684): the zones' borders over the composed frame and each zone's
    /// clock in place of the names; `None` for the map as ever.
    pub time_zones: Option<i64>,
}

/// A frame drawn.
pub struct Shot {
    /// RGBA rows, `view`'s size.
    pub rgba: Vec<u8>,
    /// The frame drawn: the screen's view, the margins around it, the
    /// same centre, on a whole pixel at a whole step.
    pub view: ExactView,
    /// The screen's view the frame shows, before its margins: the view
    /// asked, or while it zooms with [`Ask::scaled_while_moving`] the
    /// view at the tile step, the screen's size in the step's pixels,
    /// about the same centre. What a frontend places the frame by.
    pub shows: ExactView,
    /// The zoom step its tiles are.
    pub tile_zoom: u8,
    /// The step nearest the view's zoom: the data's zoom.
    pub rest_zoom: u8,
    /// Tiles it needed and did not draw, and one more while a frame at
    /// rest still adds passes (1422): ask again.
    pub missing: usize,
    /// Tiles drawn while a region in them was still arriving (1291): ask
    /// again as the region gains tiles.
    pub filling: usize,
    /// A name is fading in or out ([`FADE_MS`]): ask again.
    pub fading: bool,
    /// The credit of the regions drawn, the outline's when none.
    pub attribution: String,
    /// The maps directory's epoch it was drawn at.
    pub epoch: u64,
    /// The rung of the frame budget a moving frame runs at (1422,
    /// [`Ladder`]), for a frontend's own passes to follow.
    pub rung: u8,
    /// The passes this frame ran: all of them unless it moved at a lower
    /// rung, or it is a frame at rest that still adds passes, which then
    /// counts in [`missing`](Self::missing) as one owed.
    pub layers: Layers,
}

impl Shot {
    /// The frame `framed` with its pixels.
    pub fn of(rgba: Vec<u8>, framed: Framed) -> Self {
        Self {
            rgba,
            view: framed.view,
            shows: framed.shows,
            tile_zoom: framed.tile_zoom,
            rest_zoom: framed.rest_zoom,
            missing: framed.missing,
            filling: framed.filling,
            fading: framed.fading,
            attribution: framed.attribution,
            epoch: framed.epoch,
            rung: framed.rung,
            layers: framed.layers,
        }
    }

    /// Its pixels and the rest.
    pub fn split(self) -> (Vec<u8>, Framed) {
        let framed = Framed {
            view: self.view,
            shows: self.shows,
            tile_zoom: self.tile_zoom,
            rest_zoom: self.rest_zoom,
            missing: self.missing,
            filling: self.filling,
            fading: self.fading,
            attribution: self.attribution,
            epoch: self.epoch,
            rung: self.rung,
            layers: self.layers,
        };
        (self.rgba, framed)
    }
}

/// A frame drawn into a frontend's own pixels ([`Frames::frame_into`]):
/// a [`Shot`] without its buffer.
#[derive(Clone, Debug)]
pub struct Framed {
    pub view: ExactView,
    pub shows: ExactView,
    pub tile_zoom: u8,
    pub rest_zoom: u8,
    pub missing: usize,
    pub filling: usize,
    pub fading: bool,
    pub attribution: String,
    pub epoch: u64,
    pub rung: u8,
    pub layers: Layers,
}

/// Where [`Frames`] composes a frame: a buffer of its own, handed out
/// with the [`Shot`], or a frontend's.
enum Target<'a> {
    Own(&'a mut Vec<u8>),
    Into(&'a mut [u8]),
}

/// The size of the frame drawn for `ask`: the screen's view and the
/// margins on both sides. What a frontend sizes the pixels it hands
/// [`Frames::frame_into`] by.
pub fn frame_size(ask: &Ask<'_>) -> (u32, u32) {
    let (sw, sh) = ask.view.size();
    let (mx, my) = ask.margin;
    (
        sw.saturating_add(mx.saturating_mul(2)),
        sh.saturating_add(my.saturating_mul(2)),
    )
}

/// One prefetch call's work.
#[derive(Clone, Copy, Debug, Default)]
pub struct Prefetched {
    /// Tiles drawn.
    pub drawn: usize,
    /// More is to draw.
    pub more: bool,
    /// The frame at rest the plan was made for, on the call that made it:
    /// what a frontend asks the autoload past.
    pub planned: Option<TileFrame>,
}

/// The frames of one maps directory, kept from frame to frame: the tile
/// cache, the painter the names are looked up in, the typesetter, the
/// names the last frame at rest placed, the motion, the prefetch's plan.
pub struct Frames {
    painter: Painter,
    type_: Typesetter,
    /// The map without names and pins, in tiles (1134).
    tiles: TileCache,
    /// The zoom step the last frame was composed from.
    last_zoom: Option<u8>,
    /// The names the last placement set, as sprites.
    names: Vec<Sprite>,
    /// The frame the sprites are drawn in.
    names_view: Option<ExactView>,
    /// The same names as placed, at screen size.
    set: Vec<PlacedName>,
    /// The frame they were placed in: the screen's view and its margins.
    set_view: Option<ExactView>,
    /// The names a frame that zooms looked up last, kept for the next.
    found: Option<Found>,
    /// The screen's view and margins of the last frame.
    last_view: Option<ExactView>,
    /// The zoom of the last frame at rest, in thousandths: a frame that
    /// moves at another is a zooming one, drawn at its step when the
    /// frontend scales it ([`Ask::scaled_while_moving`]).
    rested: Option<u32>,
    /// The pan's velocity and the last zoom's way (1135).
    motion: Motion,
    /// The last frame, when it was at rest and owed no tile, with the
    /// zooms the regions allow.
    rest: Option<(TileFrame, (u8, u8))>,
    /// What the prefetch draws for that frame, made on its first call.
    plan: Option<Plan>,
    /// The pins the last frame placed, in its pixels.
    placed: Vec<Placed>,
    /// How each name of `names` fades.
    fades: Vec<Fade>,
    /// Names an earlier placement set and the last one did not, fading
    /// out.
    leaving: Vec<Leaving>,
    /// The frame at rest the names were placed for, kept while they fade.
    rest_key: Option<RestKey>,
    /// A name of the last frame was fading in or out.
    fading: bool,
    /// The names the last frame drew, by their index in `names`, each
    /// moved by whole pixels from where it was placed, and the leaving
    /// ones by their index in `leaving`.
    drawn_names: Vec<Carried>,
    drawn_leaving: Vec<Carried>,
    /// The clock of a frontend that keeps none ([`Frames::clock_ms`]).
    origin: Instant,
    /// What the last frame that zoomed spent placing its names.
    cost: PlaceCost,
    /// No names placed nor drawn: a measuring tool's ground.
    names_off: bool,
    /// The [`Mip`] of each name shown, by its mask, which it holds.
    mips: HashMap<usize, (Arc<Mask>, Arc<Mip>)>,
    /// The frame budget (1422): the rung moving frames run at, from the
    /// passes' measured times.
    ladder: Ladder,
    /// The passes the frames at rest since the last moving one ran, and
    /// those of them skipped for memory, at the view they ran at.
    rest_done: Layers,
    rest_skipped: Layers,
    rest_view: Option<ExactView>,
    /// The bytes the passes with buffers of their own may take, beside
    /// what the system has free: a measuring tool's low memory.
    heap_limit: Option<usize>,
    /// Each pass the last frame ran and its microseconds.
    trace: Vec<(Pass, u32)>,
}

/// What a frame that zooms spends placing its names (1225): a measuring
/// tool's row.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlaceCost {
    /// The names looked up near the frame, and those it placed.
    pub labels: usize,
    pub names: usize,
    /// The names were looked up in the map for this frame, not kept from
    /// an earlier one.
    pub looked_up: bool,
    /// Looked up, or taken from those kept, in nanoseconds.
    pub labels_ns: u64,
    /// Placed by the rule, with their masks and halos, in nanoseconds.
    pub place_ns: u64,
    /// Their sprites sampled, in nanoseconds.
    pub sprites_ns: u64,
}

/// The names of a view at a step, wider than a frame, kept while a pinch
/// stays at the step (1225): looking them up reads the region file, and
/// a frame of the phone at z13 asks a thousand.
struct Found {
    epoch: u64,
    /// The view they were looked up in.
    wide: Viewport,
    labels: Vec<ViewLabel>,
    /// Each one's box in `wide`'s sub-pixels: its anchor, or its line's.
    reach: Vec<[i64; 4]>,
}

/// How much wider each way the view [`Found`] keeps is than the one a
/// frame looks its names up in. A pinch through a step grows that one
/// twofold, from 0.71 of the frame to 1.41, so this looks them up about
/// twice a step.
const FOUND_WIDER: f64 = 1.6;

/// How long a name takes to fade in or out, in milliseconds (1205).
pub const FADE_MS: u64 = 150;

/// How a name fades (1205): the alpha out of 255 it had at `since_ms`,
/// and whether it is fading in, towards 255, or out, towards nothing,
/// over [`FADE_MS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fade {
    from: u8,
    since_ms: u64,
    shown: bool,
}

impl Fade {
    /// Shown, whole.
    const SHOWN: Fade = Fade {
        from: 255,
        since_ms: 0,
        shown: true,
    };

    /// Fading in from nothing at `at` milliseconds.
    fn new_from_nothing(at: u64) -> Self {
        Fade {
            from: 0,
            since_ms: at,
            shown: true,
        }
    }

    /// Its alpha at `now`; without a clock, where it is going.
    fn alpha(&self, now: Option<u64>) -> u8 {
        let Some(now) = now else {
            return if self.shown { 255 } else { 0 };
        };
        let step = (now.saturating_sub(self.since_ms) * 255 / FADE_MS).min(255) as u8;
        match self.shown {
            true => self.from.saturating_add(step),
            false => self.from.saturating_sub(step),
        }
    }

    /// Fade in, or out, from the alpha it has at `now`.
    fn turn(&mut self, shown: bool, now: Option<u64>) {
        if self.shown == shown {
            return;
        }
        let from = self.alpha(now);
        *self = Fade {
            from,
            since_ms: now.unwrap_or(0),
            shown,
        };
    }

    /// Whether it is where it was going at `now`.
    fn settled(&self, now: Option<u64>) -> bool {
        self.alpha(now) == if self.shown { 255 } else { 0 }
    }
}

/// A name the last placement no longer set, fading out where the map
/// moves it, at its screen size.
struct Leaving {
    name: PlacedName,
    /// The frame it was placed in.
    set_view: ExactView,
    sprite: Sprite,
    /// The frame its sprite is drawn in, and the zoom of the screen's
    /// view it was drawn for.
    view: ExactView,
    screen: u32,
    fade: Fade,
}

/// A frame at rest's names were placed for: the frame, the density, the
/// pins' boxes, the maps' epoch.
#[derive(PartialEq)]
struct RestKey {
    view: ExactView,
    scale: u32,
    taken: Vec<Rect>,
    epoch: u64,
    /// The minute the time zone view's clocks show, which places them
    /// anew as it turns (1684).
    clock: Option<i64>,
}

/// A carried name in a frame: which, moved by how many whole pixels,
/// whether the frame keeps it, and the alpha it is drawn at.
#[derive(Clone, Copy, Debug)]
struct Carried {
    i: usize,
    by: (i64, i64),
    keep: bool,
    alpha: u8,
}

/// `r` moved by `(dx, dy)` pixels.
fn shifted(r: &Rect, (dx, dy): (i64, i64)) -> Rect {
    let by = |v: i64| i32::try_from(v).unwrap_or(0);
    Rect {
        x0: r.x0 + by(dx),
        y0: r.y0 + by(dy),
        x1: r.x1 + by(dx),
        y1: r.y1 + by(dy),
    }
}

/// A name a frame drew ([`Frames::drawn_names`]): where, in the frame's
/// pixels, and what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawnName {
    pub text: String,
    pub kind: LabelKind,
    pub min_zoom: u8,
    /// Its middle in the screen's view with its margins, where a frame at
    /// rest there places names, and its angle.
    pub at: (f64, f64),
    pub angle: f64,
    /// The boxes it was placed by, moved with it, in the frame's pixels.
    pub boxes: Vec<Rect>,
    /// The alpha it was drawn at, out of 255.
    pub alpha: u8,
    /// The frame kept it: shown, or fading in; else fading out.
    pub shown: bool,
}

impl Frames {
    /// The frames over `maps`, the cache within [`BUDGET_MB`].
    pub fn new(maps: Maps) -> Self {
        Self {
            painter: Painter::new(maps.clone()),
            type_: Typesetter::new(),
            tiles: TileCache::new(maps, BUDGET_MB << 20),
            last_zoom: None,
            names: Vec::new(),
            names_view: None,
            set: Vec::new(),
            set_view: None,
            found: None,
            last_view: None,
            rested: None,
            motion: Motion::default(),
            rest: None,
            plan: None,
            placed: Vec::new(),
            fades: Vec::new(),
            leaving: Vec::new(),
            rest_key: None,
            fading: false,
            drawn_names: Vec::new(),
            drawn_leaving: Vec::new(),
            origin: Instant::now(),
            cost: PlaceCost::default(),
            names_off: false,
            mips: HashMap::new(),
            ladder: Ladder::new(),
            rest_done: Layers::NONE,
            rest_skipped: Layers::NONE,
            rest_view: None,
            heap_limit: None,
            trace: Vec::new(),
        }
    }

    /// The painter the names are looked up in: the regions a fit counts.
    pub fn painter(&self) -> &Painter {
        &self.painter
    }

    /// Read the regions again when the directory changed; whether it did.
    pub fn refresh(&mut self) -> bool {
        self.painter.refresh()
    }

    /// The view was panned by `(dx, dy)` screen pixels at `now_ms`.
    pub fn pan(&mut self, dx: f64, dy: f64, now_ms: u64) {
        self.motion.pan(dx, dy, now_ms);
    }

    /// The view was zoomed by `by_milli` thousandths of a step.
    pub fn zoom(&mut self, by_milli: i64) {
        self.motion.zoom(by_milli);
    }

    /// A frame was asked at `now_ms`, before it is drawn: the first since
    /// the last pan freezes the heading the prefetch plans by (1189).
    pub fn asked(&mut self, now_ms: u64) {
        self.motion.asked(now_ms);
    }

    /// The way the view last moved, screen pixels a second, while it
    /// still moves: what the autoload orders the cells next to the screen
    /// by (1292).
    pub fn heading(&self, now_ms: u64) -> Option<(f64, f64)> {
        self.motion.heading(now_ms)
    }

    /// The motion a frontend kept itself, where a pan cannot wait for
    /// the frame being drawn (the window's worker holds the frames).
    pub fn set_motion(&mut self, motion: Motion) {
        self.motion = motion;
    }

    /// One frame for `ask`.
    pub fn frame(&mut self, ask: &Ask<'_>) -> Shot {
        let mut rgba = Vec::new();
        let framed = self.draw(ask, Target::Own(&mut rgba));
        Shot::of(rgba, framed)
    }

    /// One frame for `ask` composed into `rgba`, pixels a frontend owns
    /// and shows (the phone's Bitmap, 1155), [`frame_size`] RGBA rows; it
    /// may hold anything, an earlier frame or garbage. The same bytes as
    /// [`frame`](Self::frame)'s, without a buffer of the frame's own or a
    /// copy out of it. None, and nothing drawn, when `rgba` has another
    /// size.
    pub fn frame_into(&mut self, ask: &Ask<'_>, rgba: &mut [u8]) -> Option<Framed> {
        let (w, h) = frame_size(ask);
        if rgba.len() != w as usize * h as usize * 4 {
            return None;
        }
        Some(self.draw(ask, Target::Into(rgba)))
    }

    /// One frame for `ask` into `target`.
    fn draw(&mut self, ask: &Ask<'_>, target: Target<'_>) -> Framed {
        let started = Instant::now();
        self.rest = None;
        self.plan = None;
        self.painter.refresh();
        self.trace.clear();
        let layers = self.layers(ask);
        let view = ask.view;
        let rest_zoom = view.tile_zoom();
        // At a whole step the frame's centre sits on a whole pixel of it:
        // the tiles are that step's pixels shown one to one, so the pins
        // and the names land on the map as a view drawn whole puts them.
        // Not held to one world itself, so a margin past the world's edge
        // stays empty rather than moving the view.
        let (sw, sh) = view.size();
        // While the view moves the frame keeps the step it was composed
        // from as long as the zoom is within a step of it, the tiles
        // scaled, and draws a few missing tiles a frame.
        let kept = self
            .last_zoom
            .filter(|&z| ask.moving && (view.zoom() - f64::from(z)).abs() <= 1.0);
        let mut tile_zoom = kept.unwrap_or(rest_zoom);
        // A frontend that scales the frame itself gets it at the step,
        // the tiles copied whole: a pinch's frame then costs a pan's, not
        // a resample of the whole frame (the render study of 2026-10-30).
        // Never a step above the view's zoom, so the frontend scales the
        // frame up, by two at most, and it covers at least the screen and
        // the margins asked.
        let zooming = ask.moving && self.rested != Some(view.zoom_milli());
        let shows = match zooming && ask.scaled_while_moving {
            true => {
                let below = u8::try_from(view.zoom_milli() / MILLI).unwrap_or(u8::MAX);
                tile_zoom = tile_zoom.min(below);
                ExactView::new(view.centre, u32::from(tile_zoom) * MILLI, sw, sh)
            }
            false => view,
        };
        let mut drawn = shows.on_whole_pixel();
        let (fw, fh) = frame_size(ask);
        drawn.resize(fw, fh);
        // Scaled up by the frontend, the step's frame shows only its middle,
        // the screen and the margins over the scale (1163): the tiles
        // outside it held are copied, the others not drawn nor owed.
        let shown = (shows.zoom_milli() != view.zoom_milli()).then(|| {
            let s = view.scale_at(tile_zoom).max(1.0);
            let span = |n: u32| {
                let half = f64::from(n) / 2.0;
                let reach = (half / s).ceil() + 1.0;
                let from = (half - reach).floor().max(0.0) as u32;
                let to = ((half + reach).ceil() as u32).min(n);
                (from, to)
            };
            let ((x0, x1), (y0, y1)) = (span(fw), span(fh));
            [x0, y0, x1, y1]
        });
        let asked = TileFrame {
            view: drawn,
            zoom: tile_zoom,
            style: ask.style,
            scale: ask.scale,
            render_limit: ask
                .moving
                .then(|| self.tiles.workers() * MOVING_TILES_PER_WORKER),
            fallback: self.last_zoom,
            moving: ask.moving,
            asked: shown,
            // At rest a tile is drawn smoothed at once; while moving only
            // at a rung that smooths, the others drafts (1422).
            smooth: !ask.moving || layers.has(Pass::Smooth),
            resmooth: layers.has(Pass::Smooth),
        };
        // The frontend's pixels are the frame's size (`frame_into`).
        let (rgba, (attribution, missing)) = match target {
            Target::Own(own) => {
                let composed = self.tiles.compose(&asked);
                *own = composed.rgba;
                (own.as_mut_slice(), (composed.attribution, composed.missing))
            }
            Target::Into(into) => {
                let composed = self.tiles.compose_into(&asked, into, false);
                (into, composed.unwrap_or_default())
            }
        };
        self.last_zoom = Some(tile_zoom);
        let refining = ask.now_ms.is_some() && !ask.moving && self.rest_done != Layers::ALL;
        if !ask.moving && missing == 0 && !refining {
            self.rest = Some((asked, ask.zooms));
        }
        let mut at = self.passed(Pass::Ground, started);
        let (w, h) = drawn.size();
        let (scale, theme) = (ask.scale, ask.style.theme);
        let type_ = &mut self.type_;
        let placed = match ask.measure {
            Some(measure) => screenmap::place_exact(ask.pins, &drawn, ask.selected, scale, measure),
            None => screenmap::place_exact(ask.pins, &drawn, ask.selected, scale, |t, s| {
                type_.width(t, s)
            }),
        };
        let now = ask.now_ms;
        // No region here: the radar's blank ground and rings instead of
        // the outline and its names (1386).
        if let Some(radar) = ask.radar.filter(|_| layers.has(Pass::Radar)) {
            let per_px = crate::ui::radar::metres_per_unit(radar.centre) * drawn.unit();
            screenmap::draw_radar(
                rgba,
                w,
                h,
                radar,
                |p| drawn.to_screen(p),
                per_px,
                scale,
                theme,
                &mut self.type_,
            );
            at = self.passed(Pass::Radar, at);
        }
        let names_off = self.names_off || ask.radar.is_some();
        // The time zone view (1684): its borders over the composed ground,
        // under the names, which are then the zones' clocks.
        self.painter.set_time_zones(ask.time_zones);
        if ask.time_zones.is_some() && ask.radar.is_none() {
            screenmap::draw_time_zones(rgba, w, h, &drawn, scale, theme);
        }
        // Placed anew only where the rung or the view at rest has them;
        // otherwise the names ride along as the sprites they were.
        let place = layers.has(Pass::Labels);
        // The screen's view and its margins: where a frame at rest at this
        // zoom places its names, and the frame drawn unless the frontend
        // scales it.
        let mut exact = view.on_whole_pixel();
        exact.resize(fw, fh);
        if !names_off && !ask.moving && place {
            // At rest the names are placed anew over the frame, at the
            // exact scale, after the pins, and kept in world terms; asked
            // again while they fade, the same frame keeps them.
            let taken = screenmap::taken(&placed, scale);
            let key = RestKey {
                view: drawn,
                scale: scale.to_bits(),
                taken,
                epoch: self.painter.epoch(),
                clock: ask.time_zones.map(|t| t.div_euclid(60)),
            };
            if !(self.fading && self.rest_key.as_ref() == Some(&key)) {
                let base = drawn.at_zoom(rest_zoom);
                let labels = self.painter.labels(&base, true);
                let (bw, bh) = base.size();
                let s = drawn.scale_at(rest_zoom);
                let sub = SUBPIXEL as f64;
                let px = |v: (i64, i64)| {
                    (
                        (v.0 as f64 / sub - f64::from(bw) / 2.0) * s + f64::from(w) / 2.0,
                        (v.1 as f64 / sub - f64::from(bh) / 2.0) * s + f64::from(h) / 2.0,
                    )
                };
                let names = mapnames::place(w, h, &labels, px, &key.taken, scale, &mut self.type_);
                let sprites = self.sprites_of(&names);
                self.carry_over(names, sprites, (drawn, drawn), now);
                self.rest_key = Some(key);
            }
            self.rested = Some(view.zoom_milli());
        } else if !names_off && ask.moving && place {
            // A frame that moves at another zoom than its names were
            // placed at, or into a frame of another step, places them
            // anew (1225); a pan at their zoom moves them.
            let set_at = self.set_view.map(|v| v.zoom_milli());
            let drawn_at = self.names_view.map(|v| v.zoom_milli());
            if set_at != Some(exact.zoom_milli()) || drawn_at != Some(drawn.zoom_milli()) {
                self.place_zooming(ask, (&exact, &drawn), rest_zoom, now);
                self.rest_key = None;
            }
        }
        self.last_view = Some(exact);
        self.drawn_names.clear();
        self.drawn_leaving.clear();
        if ask.radar.is_some() {
            self.leaving.clear();
        }
        if let Some(from) = self.names_view.filter(|_| ask.radar.is_none()) {
            for (i, name) in self.names.iter().enumerate() {
                let alpha = self.fades[i].alpha(now);
                if alpha == 0 {
                    continue;
                }
                let (x, y) = moved(&from, &drawn, name.centre);
                let by = ((x - name.centre.0).round(), (y - name.centre.1).round());
                self.drawn_names.push(Carried {
                    i,
                    by: (by.0 as i64, by.1 as i64),
                    keep: true,
                    alpha,
                });
            }
        }
        // The names the last placement no longer set, fading out where
        // the map moves them, under the others: a pan moves each sprite by
        // whole pixels, a zoom samples it again at its screen size.
        self.leaving.retain(|l| l.fade.alpha(now) > 0);
        let k = drawn.scale_at(0) / exact.scale_at(0);
        for (j, l) in self.leaving.iter_mut().enumerate() {
            let by =
                match l.view.zoom_milli() == drawn.zoom_milli() && l.screen == exact.zoom_milli() {
                    true => {
                        let (x, y) = moved(&l.view, &drawn, l.sprite.centre);
                        let by = (
                            (x - l.sprite.centre.0).round(),
                            (y - l.sprite.centre.1).round(),
                        );
                        (by.0 as i64, by.1 as i64)
                    }
                    false => {
                        let at = moved(&l.set_view, &exact, l.name.centre);
                        l.sprite = Sprite::scaled(&l.name, moved(&exact, &drawn, at), k, None);
                        l.view = drawn;
                        l.screen = exact.zoom_milli();
                        (0, 0)
                    }
                };
            let alpha = l.fade.alpha(now);
            l.sprite.draw_faded(rgba, w, h, theme, by, alpha);
            self.drawn_leaving.push(Carried {
                i: j,
                by,
                keep: false,
                alpha,
            });
        }
        for c in &self.drawn_names {
            self.names[c.i].draw_faded(rgba, w, h, theme, c.by, c.alpha);
        }
        // A name fading in from nothing is not drawn yet, but fades.
        self.fading = !self.leaving.is_empty() || self.fades.iter().any(|fade| !fade.settled(now));
        // Names that only rode along are no measure of placing them.
        at = match !names_off && place {
            true => self.passed(Pass::Labels, at),
            false => Instant::now(),
        };
        if layers.has(Pass::Tracks) && !ask.tracks.is_empty() {
            screenmap::draw_tracks(
                rgba,
                w,
                h,
                ask.tracks,
                |p| drawn.to_screen(p),
                scale,
                theme,
                &mut self.type_,
            );
            at = self.passed(Pass::Tracks, at);
        }
        if ask.draw_pins && layers.has(Pass::Pins) {
            screenmap::draw_pins(
                rgba,
                w,
                h,
                &placed,
                ask.tracks,
                scale,
                theme,
                &mut self.type_,
            );
            self.passed(Pass::Pins, at);
        }
        self.placed = placed;
        if ask.now_ms.is_some() && ask.moving {
            self.ladder.motion_frame(micros(started));
        }
        Framed {
            view: drawn,
            shows,
            tile_zoom,
            rest_zoom,
            missing: missing + usize::from(refining),
            filling: self.tiles.filling(),
            fading: self.fading,
            attribution,
            epoch: self.painter.epoch(),
            rung: self.ladder.rung(),
            layers,
        }
    }

    /// The passes `ask` runs (1422): every one for a frame without a
    /// clock (a still picture, a test, a measuring tool); a moving frame
    /// those of the ladder's rung; a frame at rest those it has run since
    /// the view came to rest and the next that fit one budget (all of
    /// them under a held rung), less those skipped for memory. A gesture starts at the first
    /// moving frame and ends at the first frame at rest.
    fn layers(&mut self, ask: &Ask<'_>) -> Layers {
        if ask.now_ms.is_none() {
            return Layers::ALL;
        }
        if ask.moving {
            self.ladder.gesture_start();
            self.rest_done = Layers::NONE;
            self.rest_skipped = Layers::NONE;
            self.rest_view = None;
            return self.ladder.motion_layers();
        }
        self.ladder.gesture_end();
        if self.rest_view != Some(ask.view) {
            self.rest_done = Layers::NONE;
            self.rest_skipped = Layers::NONE;
            self.rest_view = Some(ask.view);
        }
        let (fw, fh) = frame_size(ask);
        let names = match self.sprite_bytes() {
            0 => fw as usize * fh as usize,
            n => n,
        };
        let free = self.free_bytes();
        let bytes = |p| match p {
            Pass::Labels => names,
            _ => 0,
        };
        // A held rung is a measuring tool's or a test's: its frame at
        // rest is the full one at once.
        let (run, skipped) = match self.ladder.held() {
            Some(_) => self.ladder.rest_all(free, bytes),
            None => self.ladder.rest_plan(self.rest_done, free, bytes),
        };
        self.rest_done = run;
        self.rest_skipped = self.rest_skipped.union(skipped);
        run.without(self.rest_skipped)
    }

    /// `pass` ran from `from` to now: timed, traced and told the ladder.
    /// Now, for the next pass.
    fn passed(&mut self, pass: Pass, from: Instant) -> Instant {
        let now = Instant::now();
        let us = u32::try_from(now.duration_since(from).as_micros()).unwrap_or(u32::MAX);
        self.ladder.observe(pass, us);
        self.trace.push((pass, us));
        now
    }

    /// The bytes free for a pass's own buffer: what the system says is
    /// available, within the limit a measuring tool set less the tiles
    /// held.
    pub fn free_bytes(&self) -> usize {
        let system = available_bytes().unwrap_or(usize::MAX);
        match self.heap_limit {
            Some(limit) => system.min(limit.saturating_sub(self.tiles.bytes())),
            None => system,
        }
    }

    /// The frame budget's planner.
    pub fn ladder(&self) -> &Ladder {
        &self.ladder
    }

    /// Hold moving frames at `rung`, and run every pass in each frame at
    /// rest, or with `None` let the times pick both again.
    pub fn hold_rung(&mut self, rung: Option<u8>) {
        self.ladder.hold(rung);
    }

    /// The passes the last frame ran, each with its microseconds.
    pub fn trace(&self) -> &[(Pass, u32)] {
        &self.trace
    }

    /// The passes the view at rest skipped for memory.
    pub fn rest_skipped(&self) -> Layers {
        self.rest_skipped
    }

    /// Limit the bytes the passes with buffers of their own may take,
    /// the tiles held counted against it: a measuring tool's low memory.
    pub fn set_heap_limit(&mut self, bytes: Option<usize>) {
        self.heap_limit = bytes;
    }

    /// The sprites of `names`, placed in the frame they are drawn in.
    fn sprites_of(&self, names: &[PlacedName]) -> Vec<Sprite> {
        #[cfg(test)]
        if self.type_.unkept {
            return names.iter().map(Sprite::of_whole_reach).collect();
        }
        Sprite::of_all(names, self.tiles.workers())
    }

    /// Place the names of a frame that moves at another zoom anew, as a
    /// frame at rest at its zoom places them (1225): in `exact`, the
    /// screen's view with its margins, after the pins, from the names
    /// [`Found`] keeps near it at `step`; drawn into `drawn`, the frame
    /// composed, each at the scale between the two.
    fn place_zooming(
        &mut self,
        ask: &Ask<'_>,
        (exact, drawn): (&ExactView, &ExactView),
        step: u8,
        now: Option<u64>,
    ) {
        let started = Instant::now();
        let (fw, fh) = exact.size();
        let scale = ask.scale;
        let taken = match ask.pins.is_empty() {
            true => Vec::new(),
            false => {
                let type_ = &mut self.type_;
                let pins = match ask.measure {
                    Some(measure) => {
                        screenmap::place_exact(ask.pins, exact, ask.selected, scale, measure)
                    }
                    None => screenmap::place_exact(ask.pins, exact, ask.selected, scale, |t, s| {
                        type_.width(t, s)
                    }),
                };
                screenmap::taken(&pins, scale)
            }
        };
        let base = exact.at_zoom(step);
        let looked_up = self.find(&base);
        let Some(found) = self.found.as_ref() else {
            return;
        };
        // The frame's middle and the view a frame at rest looks its names
        // up in, in the kept view's sub-pixels: the same offsets from the
        // middle as a frame at rest's own lookup gives.
        let c = found.wide.to_screen_sub(exact.centre);
        let (bw, bh) = base.size();
        let half = (i64::from(bw) * SUBPIXEL / 2, i64::from(bh) * SUBPIXEL / 2);
        let area = [c.0 - half.0, c.1 - half.1, c.0 + half.0, c.1 + half.1];
        let near: Vec<&ViewLabel> = found
            .labels
            .iter()
            .zip(&found.reach)
            .filter(|(_, r)| {
                r[0] <= area[2] && r[2] >= area[0] && r[1] <= area[3] && r[3] >= area[1]
            })
            .map(|(l, _)| l)
            .collect();
        let s = exact.scale_at(step);
        let sub = SUBPIXEL as f64;
        let (mx, my) = (f64::from(fw) / 2.0, f64::from(fh) / 2.0);
        let px = |v: (i64, i64)| {
            (
                (v.0 - c.0) as f64 / sub * s + mx,
                (v.1 - c.1) as f64 / sub * s + my,
            )
        };
        let labels = near.len();
        let labels_ns = nanos(started);
        let started = Instant::now();
        let names = mapnames::place(fw, fh, near, px, &taken, scale, &mut self.type_);
        let place_ns = nanos(started);
        let started = Instant::now();
        let sprites = match exact == drawn {
            true => self.sprites_of(&names),
            false => {
                let k = drawn.scale_at(0) / exact.scale_at(0);
                let mips = self.mips_of(&names, k);
                let made: Vec<(&PlacedName, Option<&Mip>)> = names
                    .iter()
                    .zip(mips.iter().map(Option::as_deref))
                    .collect();
                Sprite::all_of(&made, self.tiles.workers(), |&(n, mip)| {
                    Sprite::scaled(n, moved(exact, drawn, n.centre), k, mip)
                })
            }
        };
        self.cost = PlaceCost {
            labels,
            names: names.len(),
            looked_up,
            labels_ns,
            place_ns,
            sprites_ns: nanos(started),
        };
        self.carry_over(names, sprites, (*exact, *drawn), now);
    }

    /// The [`Mip`] of each of `names` when they are drawn at `k` below
    /// [`MIP_BELOW`]: those kept by their mask, the others made; the
    /// names before them no more kept.
    fn mips_of(&mut self, names: &[PlacedName], k: f64) -> Vec<Option<Arc<Mip>>> {
        if k >= MIP_BELOW {
            return vec![None; names.len()];
        }
        let mut was = std::mem::take(&mut self.mips);
        names
            .iter()
            .map(|n| {
                let key = Arc::as_ptr(&n.mask) as usize;
                let kept = match was.remove(&key).or_else(|| self.mips.remove(&key)) {
                    Some(kept) => kept,
                    None => (n.mask.clone(), Arc::new(Mip::of(n))),
                };
                let mip = kept.1.clone();
                self.mips.insert(key, kept);
                Some(mip)
            })
            .collect()
    }

    /// Keep the names of a view wider than `base` at its step ([`Found`]):
    /// looked up anew when none are kept at that step, the maps changed,
    /// or `base` reaches past them. Whether they were.
    fn find(&mut self, base: &Viewport) -> bool {
        let epoch = self.painter.epoch();
        let (bw, bh) = base.size();
        let (bw, bh) = (i64::from(bw) * SUBPIXEL, i64::from(bh) * SUBPIXEL);
        let fits = self.found.as_ref().is_some_and(|f| {
            let (ww, wh) = f.wide.size();
            let (ww, wh) = (i64::from(ww) * SUBPIXEL, i64::from(wh) * SUBPIXEL);
            let c = f.wide.to_screen_sub(base.centre);
            f.epoch == epoch
                && f.wide.zoom() == base.zoom()
                && c.0 - bw / 2 >= 0
                && c.1 - bh / 2 >= 0
                && c.0 + bw / 2 <= ww
                && c.1 + bh / 2 <= wh
        });
        if fits {
            return false;
        }
        let side = |n: u32| (f64::from(n) * FOUND_WIDER).ceil() as u32;
        let mut wide = *base;
        let (w, h) = base.size();
        wide.resize(side(w), side(h));
        let labels = self.painter.labels(&wide, true);
        let reach = labels.iter().map(reach_of).collect();
        self.found = Some(Found {
            epoch,
            wide,
            labels,
            reach,
        });
        true
    }

    /// Take `set`, the names a placement set in the frame `set_view`, and
    /// their `sprites` in the frame `view`, from the names before them
    /// (1205): one shown before (by its text and kind), or fading out, keeps
    /// its alpha and fades in from there, a new one fades in from nothing,
    /// and one shown before and placed no more fades out where the map
    /// moves it. Without a clock, or with no names before, they show at
    /// once.
    fn carry_over(
        &mut self,
        set: Vec<PlacedName>,
        sprites: Vec<Sprite>,
        (set_view, view): (ExactView, ExactView),
        now: Option<u64>,
    ) {
        let was = self.set_view.zip(self.names_view);
        self.set_view = Some(set_view);
        self.names_view = Some(view);
        let (Some(at), Some((was_set, was_view))) = (now, was) else {
            self.fades = vec![Fade::SHOWN; sprites.len()];
            self.names = sprites;
            self.set = set;
            self.leaving.clear();
            return;
        };
        /// Where a name shown before is: shown, or fading out.
        enum Before {
            Shown(usize),
            Leaving(usize),
        }
        let mut taken = vec![false; self.names.len()];
        let mut back = vec![false; self.leaving.len()];
        let fades: Vec<Fade> = {
            let mut before: HashMap<(&str, LabelKind), Vec<Before>> = HashMap::new();
            for (k, l) in self.leaving.iter().enumerate().rev() {
                if l.fade.alpha(now) > 0 {
                    let key = (l.name.text.as_str(), l.name.kind);
                    before.entry(key).or_default().push(Before::Leaving(k));
                }
            }
            // Pushed last, a name shown is taken before one leaving.
            for (j, name) in self.names.iter().enumerate().rev() {
                if self.fades[j].alpha(now) > 0 {
                    let key = (name.text.as_str(), name.kind);
                    before.entry(key).or_default().push(Before::Shown(j));
                }
            }
            sprites
                .iter()
                .map(|s| {
                    match before
                        .get_mut(&(s.text.as_str(), s.kind))
                        .and_then(Vec::pop)
                    {
                        Some(Before::Shown(j)) => {
                            taken[j] = true;
                            self.fades[j]
                        }
                        Some(Before::Leaving(k)) => {
                            back[k] = true;
                            let mut fade = self.leaving[k].fade;
                            fade.turn(true, now);
                            fade
                        }
                        None => Fade::new_from_nothing(at),
                    }
                })
                .collect()
        };
        let mut k = 0;
        self.leaving.retain(|_| {
            k += 1;
            !back[k - 1]
        });
        let old = std::mem::replace(&mut self.names, sprites);
        let old_set = std::mem::replace(&mut self.set, set);
        let gone = old.into_iter().zip(old_set).zip(self.fades.drain(..));
        for (((sprite, name), mut fade), taken) in gone.zip(taken) {
            if !taken && fade.alpha(now) > 0 {
                fade.turn(false, now);
                self.leaving.push(Leaving {
                    name,
                    set_view: was_set,
                    sprite,
                    view: was_view,
                    screen: was_set.zoom_milli(),
                    fade,
                });
            }
        }
        self.fades = fades;
    }

    /// Hand a shot's buffer back once it is shown (1154): the next frame
    /// is composed into it, so its pages stay mapped and a frame of the
    /// same size allocates nothing.
    pub fn recycle(&mut self, rgba: Vec<u8>) {
        self.tiles.recycle(rgba);
    }

    /// Drop the names' sizes, masks and halos kept: a measuring tool's
    /// first frame at rest.
    pub fn forget_names(&mut self) {
        self.type_.forget();
    }

    /// The names' masks and halos kept, and their bytes.
    pub fn names_kept(&self) -> (usize, usize) {
        self.type_.kept()
    }

    /// The bytes the names of the last frame at rest hold as sprites.
    pub fn sprite_bytes(&self) -> usize {
        self.names.iter().map(Sprite::bytes).sum()
    }

    /// Draw the names of the last frame at rest again into `rgba`, `w` by
    /// `h` pixels, where they were placed: a measuring tool's sprite draw.
    pub fn draw_names(&self, rgba: &mut [u8], w: u32, h: u32, theme: MapTheme) {
        for name in &self.names {
            name.draw(rgba, w, h, theme, 0, 0);
        }
    }

    /// Whether the last frame was at rest and owed nothing: a prefetch
    /// has something to draw around.
    pub fn at_rest(&self) -> bool {
        self.rest.is_some()
    }

    /// One batch of the prefetch around the last frame at rest (1135):
    /// on the first call the plan, ordered by the pan's direction when
    /// the frame was asked ([`asked`](Self::asked)), at `now_ms` when no
    /// frame was; then a tile a worker.
    pub fn prefetch(&mut self, now_ms: u64) -> Prefetched {
        let Some((frame, zooms)) = self.rest else {
            return Prefetched::default();
        };
        let mut planned = None;
        if self.plan.is_none() {
            let heading = self.motion.rest_heading(now_ms);
            self.plan = Some(tiles::plan(&frame, heading, self.motion.zoomed_out, zooms));
            planned = Some(frame);
        }
        let workers = self.tiles.workers();
        let (drawn, more) = match self.plan.as_mut() {
            Some(plan) => (self.tiles.prefetch(plan, workers), !plan.done()),
            None => (0, false),
        };
        Prefetched {
            drawn,
            more,
            planned,
        }
    }

    /// The pins the last frame placed, in its pixels, margins included.
    pub fn placed(&self) -> &[Placed] {
        &self.placed
    }

    /// The names the last frame at rest placed, as text.
    pub fn names(&self) -> Vec<String> {
        self.names.iter().map(|n| n.text.clone()).collect()
    }

    /// The names the last frame drew, in the order drawn, where it drew
    /// them.
    pub fn drawn_names(&self) -> Vec<DrawnName> {
        let Some(screen) = self.last_view else {
            return Vec::new();
        };
        let leaving = self.drawn_leaving.iter().map(|c| {
            let l = &self.leaving[c.i];
            (&l.sprite, &l.name, &l.set_view, c)
        });
        let names = self.drawn_names.iter().filter_map(|c| {
            let set_view = self.set_view.as_ref()?;
            Some((&self.names[c.i], &self.set[c.i], set_view, c))
        });
        leaving
            .chain(names)
            .map(|(sprite, name, set_view, c)| DrawnName {
                text: sprite.text.clone(),
                kind: sprite.kind,
                min_zoom: sprite.min_zoom,
                at: moved(set_view, &screen, name.centre),
                angle: name.angle,
                boxes: sprite.boxes.iter().map(|b| shifted(b, c.by)).collect(),
                alpha: c.alpha,
                shown: c.keep,
            })
            .collect()
    }

    /// Milliseconds since these frames were made: the clock of a frontend
    /// that keeps none of its own ([`Ask::now_ms`]).
    pub fn clock_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Place and draw no names, or again: a measuring tool's ground.
    pub fn set_names_off(&mut self, off: bool) {
        self.names_off = off;
    }

    /// What the last frame that placed its names while it moved spent.
    pub fn place_cost(&self) -> PlaceCost {
        self.cost
    }

    /// What the tile cache did.
    pub fn tile_stats(&self) -> Stats {
        self.tiles.stats
    }

    /// The tile cache's tiles and bytes.
    pub fn tile_memory(&self) -> (usize, usize) {
        (self.tiles.len(), self.tiles.bytes())
    }

    /// The threads the cache draws on.
    pub fn workers(&self) -> usize {
        self.tiles.workers()
    }

    /// Draw the tiles on `n` threads at most: a measuring tool's one-core
    /// row.
    pub fn set_workers(&mut self, n: usize) {
        self.tiles.set_workers(n);
    }

    /// Drop every tile: a measuring tool's cold frame.
    pub fn clear_tiles(&mut self) {
        self.tiles.clear();
    }
}

/// A name's reach in the sub-pixels of the view it was looked up in: its
/// anchor's, or its line's.
fn reach_of(l: &ViewLabel) -> [i64; 4] {
    let mut r = [l.anchor.0, l.anchor.1, l.anchor.0, l.anchor.1];
    for &(x, y) in l.line.iter().flatten() {
        r = [r[0].min(x), r[1].min(y), r[2].max(x), r[3].max(y)];
    }
    r
}

/// Microseconds since `started`.
fn micros(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_micros()).unwrap_or(u32::MAX)
}

/// The memory the system has available, from `/proc/meminfo`; `None`
/// where it does not say.
fn available_bytes() -> Option<usize> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib: usize = info
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kib.saturating_mul(1024))
}

/// The nanoseconds since `started`.
fn nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// `view` as every frontend shows it: one world at most, the zoom between
/// `floor` and `ceiling` (the regions' deepest, never above the floor's
/// clamp) about the centre.
pub fn hold(view: ExactView, floor: u8, ceiling: u8) -> ExactView {
    let (floor, ceiling) = (u32::from(floor) * MILLI, u32::from(ceiling) * MILLI);
    let mut v = view.one_world();
    let z = v.zoom_milli().clamp(floor, ceiling.max(floor));
    let (cx, cy) = (f64::from(v.size().0) / 2.0, f64::from(v.size().1) / 2.0);
    v.zoom_about(z, cx, cy);
    v.one_world()
}

/// Where the point `c` of the frame `rest` is in the frame `now`: the
/// pan and the zoom between them. A pan of whole pixels at the same zoom
/// moves it by whole pixels, so a name drawn again is the same pixels.
pub fn moved(rest: &ExactView, now: &ExactView, c: (f64, f64)) -> (f64, f64) {
    let k = now.scale_at(0) / rest.scale_at(0);
    let (rw, rh) = rest.size();
    let (nw, nh) = now.size();
    let (cx, cy) = now.to_screen(rest.centre);
    let (mut dx, mut dy) = (cx - f64::from(nw) / 2.0, cy - f64::from(nh) / 2.0);
    if rest.zoom_milli() == now.zoom_milli() {
        let whole = |v: f64| match (v - v.round()).abs() < 1e-3 {
            true => v.round(),
            false => v,
        };
        (dx, dy) = (whole(dx), whole(dy));
    }
    (
        (c.0 - f64::from(rw) / 2.0) * k + f64::from(nw) / 2.0 + dx,
        (c.1 - f64::from(rh) / 2.0) * k + f64::from(nh) / 2.0 + dy,
    )
}

/// Where the frame `drawn` lands on the screen's view `screen`, which
/// may have moved since it was asked for: its top left corner in the
/// screen's pixels, and how many screen pixels one of its pixels is. A
/// frontend shows the last frame there until the next one lands.
pub fn onto(drawn: &ExactView, screen: &ExactView) -> (f64, f64, f64) {
    let k = screen.scale_at(0) / drawn.scale_at(0);
    let (cx, cy) = screen.to_screen(drawn.centre);
    let (w, h) = drawn.size();
    (cx - f64::from(w) / 2.0 * k, cy - f64::from(h) / 2.0 * k, k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};
    use leviculum_mapfill::style::{MapTheme, Style};
    use std::path::PathBuf;

    /// The frames over the committed Florence region, which carries its
    /// names.
    fn florence() -> Frames {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures");
        Frames::new(crate::picture::Maps::new(Some(dir)))
    }

    /// The frames as they were before 1154: every name shaped and
    /// rasterised anew, its halo widened over the whole disc, its sprite
    /// sampled over its whole reach, every frame composed into a fresh
    /// buffer pixel by pixel.
    fn reference() -> Frames {
        let mut frames = florence();
        frames.tiles.reference = true;
        frames.type_.unkept = true;
        frames
    }

    fn ask(view: ExactView, moving: bool, scale: f32, theme: MapTheme) -> Ask<'static> {
        Ask {
            view,
            margin: (view.size().0 / 4, view.size().1 / 4),
            moving,
            style: Style { theme, ..Style::PC },
            scale,
            pins: &[],
            selected: None,
            zooms: (10, 17),
            draw_pins: true,
            tracks: &[],
            radar: None,
            measure: None,
            now_ms: None,
            scaled_while_moving: false,
            time_zones: None,
        }
    }

    /// 1684: the time zone view strokes the zones' borders over the world
    /// view in their own violet, which the map without it never has, and
    /// its names are then the zones' clocks, `HH:MM` each, Paris's among
    /// them at its offset; the next minute places them anew.
    #[test]
    fn the_time_zone_view_draws_the_zones_and_their_clocks() {
        let centre = project(LatLon::from_degrees(30.0, 10.0));
        let view = ExactView::new(centre, 2 * MILLI, 1000, 700);
        let violet = crate::zoneclock::line_rgb(MapTheme::Light);
        let near = |rgba: &[u8]| {
            rgba.chunks_exact(4)
                .filter(|p| {
                    let d = |a: u8, b: u8| i32::from(a).abs_diff(i32::from(b));
                    d(p[0], violet[0]) < 24 && d(p[1], violet[1]) < 24 && d(p[2], violet[2]) < 24
                })
                .count()
        };
        let mut frames = florence();
        let plain = frames.frame(&ask(view, false, 1.0, MapTheme::Light));
        let plain_names = frames.names();
        // 2026-07-01T12:00:00Z.
        let noon = 1_782_907_200;
        let zones = frames.frame(&Ask {
            time_zones: Some(noon),
            ..ask(view, false, 1.0, MapTheme::Light)
        });
        let clocks = frames.names();
        assert!(near(&plain.rgba) < 50, "{}", near(&plain.rgba));
        assert!(near(&zones.rgba) > 2000, "{}", near(&zones.rgba));
        assert!(plain_names.iter().all(|n| !is_clock(n)), "{plain_names:?}");
        assert!(clocks.len() > 5, "{clocks:?}");
        assert!(clocks.iter().all(|n| is_clock(n)), "{clocks:?}");
        assert!(clocks.iter().any(|n| n == "14:00"), "{clocks:?}");
        let later = frames.frame(&Ask {
            time_zones: Some(noon + 60),
            ..ask(view, false, 1.0, MapTheme::Light)
        });
        assert!(near(&later.rgba) > 2000);
        assert!(frames.names().iter().any(|n| n == "14:01"));
    }

    fn is_clock(name: &str) -> bool {
        let b = name.as_bytes();
        b.len() == 5
            && b[2] == b':'
            && b.iter()
                .enumerate()
                .all(|(i, c)| i == 2 || c.is_ascii_digit())
    }

    /// 1154: the names pipeline (kept sizes, masks and halos, the halo
    /// widened by spans, the sprite's boxed loop on worker threads) and
    /// the compose fast path (the buffer reused, whole rows copied at a
    /// whole step, the lane bilinear between steps) give the frames of
    /// the path before them byte for byte: at rest at a whole step and
    /// between steps, panned while moving, through a pinch and back, at
    /// two densities and in both themes, every buffer handed back dirty.
    #[test]
    fn the_fast_paths_give_the_frames_of_the_old_ones() {
        let centre = project(LatLon::from_degrees(43.7731, 11.2560));
        for (scale, theme) in [(1.0, MapTheme::Light), (2.75, MapTheme::Dark)] {
            // Both on the same workers: a moving frame draws
            // `MOVING_TILES_PER_WORKER` missing tiles a worker, so a side
            // left on the host's count owes others where it has few cores
            // (1167).
            let mut old = reference();
            old.set_workers(4);
            let mut new = florence();
            new.set_workers(4);
            let mut view = ExactView::new(centre, 15 * MILLI, 900, 700);
            let mut steps: Vec<(ExactView, bool)> = vec![(view, false)];
            for i in 0..4 {
                view.pan(if i % 2 == 0 { 40 } else { -17 }, 23);
                steps.push((view, true));
            }
            steps.push((view, false));
            for z in [15_300, 15_700, 16_000] {
                let mut v = view;
                v.zoom_about(z, 450.0, 350.0);
                steps.push((v, true));
                steps.push((v, false));
            }
            steps.push((view, false));
            let mut names = 0;
            for (i, &(v, moving)) in steps.iter().enumerate() {
                let a = ask(v, moving, scale, theme);
                let want = old.frame(&a);
                let got = new.frame(&a);
                assert_eq!(got.missing, want.missing, "step {i}");
                assert!(
                    got.rgba == want.rgba,
                    "step {i} at scale {scale}: the frame differs"
                );
                assert_eq!(new.names(), old.names(), "step {i}");
                names = names.max(new.names().len());
                // Hand the buffer back poisoned: the next frame must not
                // show what it held.
                let mut spent = got.rgba;
                spent.fill(0x5a);
                new.recycle(spent);
            }
            eprintln!("scale {scale}: up to {names} names a frame");
            assert!(names > 2, "the frames placed names ({names})");
            let (kept, bytes) = new.names_kept();
            assert!(
                kept > 0 && bytes <= mapnames::MASK_BYTES_KEPT,
                "{kept} {bytes}"
            );
        }
    }

    /// 1155: a frontend that scales the frame itself gets a pinch's frame
    /// at the tile step one to one: the bytes of the frame a pan at that
    /// step gives, the names where it puts them. Out below the step kept,
    /// the step below. A pan at the zoom at rest is drawn at that zoom,
    /// as without the switch.
    #[test]
    fn a_pinch_frame_is_its_step_one_to_one() {
        let centre = project(LatLon::from_degrees(43.7731, 11.2560));
        let theme = MapTheme::Light;
        let at = |z: u32| {
            let mut v = ExactView::new(centre, 15 * MILLI, 900, 700);
            v.zoom_about(z, 450.0, 350.0);
            v
        };
        let scaled = |view: ExactView| Ask {
            scaled_while_moving: true,
            time_zones: None,
            ..ask(view, true, 2.75, theme)
        };
        let mut pinch = florence();
        pinch.frame(&ask(at(15_000), false, 2.75, theme));
        let got = pinch.frame(&scaled(at(15_400)));
        assert_eq!((got.tile_zoom, got.shows.zoom_milli()), (15, 15_000));
        assert_eq!(got.shows.centre, at(15_400).centre);
        assert_eq!(got.shows.size(), (900, 700));
        assert_eq!(got.view.size(), (900 + 2 * 225, 700 + 2 * 175));
        // The ground, byte for byte; the names are placed for the
        // pinch's zoom since 1225, so the pixels under them are left out.
        let pinch_names = pinch.drawn_names();
        let mut pan = florence();
        pan.frame(&ask(at(15_000), false, 2.75, theme));
        let want = pan.frame(&ask(at(15_000), true, 2.75, theme));
        let (w, _) = got.view.size();
        let under: Vec<Rect> = pinch_names
            .iter()
            .chain(&pan.drawn_names())
            .flat_map(|n| n.boxes.iter().map(|b| b.grown(16)))
            .collect();
        assert!(!under.is_empty(), "names drawn");
        let differ = got
            .rgba
            .chunks_exact(4)
            .zip(want.rgba.chunks_exact(4))
            .enumerate()
            .filter(|(i, (a, b))| {
                let (x, y) = ((i % w as usize) as i32, (i / w as usize) as i32);
                let named = under
                    .iter()
                    .any(|r| (r.x0..r.x1).contains(&x) && (r.y0..r.y1).contains(&y));
                a != b && !named
            })
            .count();
        assert_eq!(differ, 0, "the step's frame, byte for byte");
        let out = pinch.frame(&scaled(at(14_600)));
        assert_eq!((out.tile_zoom, out.shows.zoom_milli()), (14, 14_000));
        // At rest between steps, then a pan there: drawn at that zoom.
        let mut rest = florence();
        let mut plain = florence();
        rest.frame(&ask(at(15_400), false, 2.75, theme));
        plain.frame(&ask(at(15_400), false, 2.75, theme));
        let mut moved = at(15_400);
        moved.pan(31, -12);
        let got = rest.frame(&scaled(moved));
        let want = plain.frame(&ask(moved, true, 2.75, theme));
        assert_eq!(got.shows, moved);
        assert!(
            got.rgba == want.rgba,
            "a pan at the zoom at rest is as before"
        );
    }

    /// One frame of a pinch: its zoom, the names drawn, the pairs of
    /// them whose boxes overlap, those the ladder of its zoom does not
    /// show, and the names a frame at rest of its view places.
    #[derive(Debug)]
    struct PinchFrame {
        zoom_milli: u32,
        drawn: usize,
        overlapping: usize,
        below: usize,
        budget: usize,
    }

    /// The overlapping pairs of `names`: two of them whose boxes cover
    /// a pixel both.
    fn overlapping(names: &[DrawnName]) -> usize {
        let mut pairs = 0;
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                let meet = a
                    .boxes
                    .iter()
                    .any(|p| b.boxes.iter().any(|q| p.overlaps(q)));
                pairs += usize::from(meet);
            }
        }
        pairs
    }

    /// A pinch out from z15 to z12 over twenty frames on the Florence
    /// fixture, the phone's way (the frame at the step, scaled by the
    /// frontend), after a frame at rest at z15: each frame's names.
    fn pinch_out() -> Vec<PinchFrame> {
        const SCALE: f32 = 1.0;
        let centre = project(LatLon::from_degrees(43.7757, 11.2603));
        let theme = MapTheme::Light;
        let rest = ExactView::new(centre, 15 * MILLI, 1280, 960);
        let mut frames = florence();
        frames.frame(&ask(rest, false, SCALE, theme));
        assert!(frames.names().len() > 5, "{:?}", frames.names());
        let mut out = Vec::new();
        for i in 1..=20u32 {
            let mut view = rest;
            view.zoom_about(15_000 - 150 * i, 640.0, 480.0);
            frames.frame(&Ask {
                scaled_while_moving: true,
                time_zones: None,
                ..ask(view, true, SCALE, theme)
            });
            let drawn = frames.drawn_names();
            let band = view.tile_zoom();
            let budget = match drawn.is_empty() {
                true => 0,
                false => {
                    let mut fresh = florence();
                    fresh.frame(&ask(view, false, SCALE, theme));
                    fresh.names().len()
                }
            };
            out.push(PinchFrame {
                zoom_milli: view.zoom_milli(),
                drawn: drawn.len(),
                overlapping: overlapping(&drawn),
                below: drawn.iter().filter(|n| n.min_zoom > band).count(),
                budget,
            });
        }
        for f in &out {
            eprintln!("{f:?}");
        }
        out
    }

    /// 1205: a pinch out draws no carpet: in every frame no name the
    /// ladder of the frame's zoom does not show, no two names whose
    /// boxes overlap, and no more names than a frame at rest of its view
    /// places. Before, every frame drew all eight names of the frame at
    /// rest, nine pairs of them overlapping from z14.85, twenty from
    /// z13.95 and twenty-eight from z12.9, seven below the ladder from
    /// z14.4 and all eight from z13.35, against six to none at rest.
    #[test]
    fn a_pinch_out_draws_no_carpet() {
        let frames = pinch_out();
        let below: Vec<_> = frames.iter().filter(|f| f.below > 0).collect();
        assert!(below.is_empty(), "names below the ladder: {below:?}");
        let overlapping: Vec<_> = frames.iter().filter(|f| f.overlapping > 0).collect();
        assert!(overlapping.is_empty(), "names overlap: {overlapping:?}");
        let over: Vec<_> = frames.iter().filter(|f| f.drawn > f.budget).collect();
        assert!(over.is_empty(), "more names than at rest: {over:?}");
        assert!(frames[0].drawn > 1, "the first frame keeps names");
        assert_eq!(frames.last().map(|f| f.zoom_milli), Some(12_000));
    }

    /// The frames over the Berlin fixture, names from z10 to z14.
    fn berlin() -> Frames {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../leviculum-map/fixtures/berlin");
        Frames::new(crate::picture::Maps::new(Some(dir)))
    }

    /// The names a frame at rest at `view` places, as drawn.
    fn at_rest(frames: fn() -> Frames, view: ExactView, scale: f32) -> Vec<DrawnName> {
        let mut fresh = frames();
        fresh.frame(&ask(view, false, scale, MapTheme::Light));
        fresh.drawn_names()
    }

    /// 1225: a pinch out from z14 to z10 in eight steps, the phone's way,
    /// never clears the names: every frame shows the names a frame at
    /// rest at its zoom places, each within a pixel of where that frame
    /// puts it. Before, from z13 on none (1205's step limit), and the
    /// frames that kept some carried them from z14.
    #[test]
    fn a_pinch_out_from_z14_to_z10_thins_the_names_and_never_clears_them() {
        let centre = project(LatLon::from_degrees(52.5175, 13.3950));
        let rest = ExactView::new(centre, 14 * MILLI, 1280, 960);
        let mut frames = berlin();
        frames.frame(&ask(rest, false, 1.0, MapTheme::Light));
        let mut counts = Vec::new();
        for i in 1..=8u32 {
            let mut view = rest;
            view.zoom_about(14_000 - 500 * i, 640.0, 480.0);
            frames.frame(&Ask {
                scaled_while_moving: true,
                time_zones: None,
                ..ask(view, true, 1.0, MapTheme::Light)
            });
            let shown: Vec<DrawnName> = frames
                .drawn_names()
                .into_iter()
                .filter(|n| n.shown)
                .collect();
            let want = at_rest(berlin, view, 1.0);
            let z = view.zoom_milli();
            assert!(!want.is_empty(), "z{z}: the frame at rest places names");
            assert!(!shown.is_empty(), "z{z}: no name, at rest {}", want.len());
            for n in &shown {
                let near = want.iter().find(|m| {
                    m.text == n.text
                        && m.kind == n.kind
                        && (m.at.0 - n.at.0).abs() <= 1.0
                        && (m.at.1 - n.at.1).abs() <= 1.0
                });
                assert!(
                    near.is_some(),
                    "z{z}: {} at {:?}, at rest {want:?}",
                    n.text,
                    n.at
                );
            }
            let names = |v: &[DrawnName]| {
                let mut t: Vec<String> = v.iter().map(|n| n.text.clone()).collect();
                t.sort();
                t
            };
            assert_eq!(
                names(&shown),
                names(&want),
                "z{z}: the names of the frame at rest"
            );
            counts.push((z, shown.len()));
        }
        eprintln!("{counts:?}");
        assert_eq!(counts.last().map(|c| c.0), Some(10_000));
    }

    /// 1225: a street's name in a pinch's frame is laid along the street
    /// as the frame's own zoom draws it: its middle and its angle where a
    /// frame at rest at that zoom lays it, and its glyphs drawn into the
    /// step's frame at the inverse of the frontend's scale, so on screen
    /// they are as long as at rest; carried, they were the step's.
    #[test]
    fn a_street_name_follows_its_street_through_a_pinch() {
        let centre = project(LatLon::from_degrees(43.7731, 11.2560));
        let theme = MapTheme::Light;
        let rest = ExactView::new(centre, 15 * MILLI, 900, 700);
        let mut frames = florence();
        frames.frame(&ask(rest, false, 2.75, theme));
        let mut streets = 0;
        for z in [15_250, 15_600, 15_900] {
            let mut view = rest;
            view.zoom_about(z, 450.0, 350.0);
            let got = frames.frame(&Ask {
                scaled_while_moving: true,
                time_zones: None,
                ..ask(view, true, 2.75, theme)
            });
            let k = got.view.scale_at(0) / view.scale_at(0);
            assert!(k < 0.96, "z{z}: drawn at the step ({k})");
            let want = at_rest(florence, view, 2.75);
            let span = |n: &DrawnName| {
                let x0 = n.boxes.iter().map(|b| b.x0).min().unwrap_or(0);
                let x1 = n.boxes.iter().map(|b| b.x1).max().unwrap_or(0);
                let y0 = n.boxes.iter().map(|b| b.y0).min().unwrap_or(0);
                let y1 = n.boxes.iter().map(|b| b.y1).max().unwrap_or(0);
                f64::from(x1 - x0).hypot(f64::from(y1 - y0))
            };
            for n in frames.drawn_names().iter().filter(|n| n.kind.is_line()) {
                let Some(m) = want.iter().find(|m| m.text == n.text && m.kind == n.kind) else {
                    panic!("z{z}: {} is not at rest", n.text);
                };
                assert!(
                    (m.at.0 - n.at.0).abs() <= 1.0 && (m.at.1 - n.at.1).abs() <= 1.0,
                    "z{z}: {} at {:?}, at rest {:?}",
                    n.text,
                    n.at,
                    m.at
                );
                assert!((m.angle - n.angle).abs() < 1e-9, "z{z}: {}", n.text);
                // Its run on screen, the frame scaled by 1 / k, against
                // the run at rest: within the boxes' rounding.
                let (on_screen, at_rest) = (span(n) / k, span(m));
                assert!(
                    (on_screen - at_rest).abs() <= 4.0 / k,
                    "z{z}: {} {on_screen} against {at_rest}",
                    n.text
                );
                streets += 1;
            }
        }
        assert!(streets > 2, "street names laid ({streets})");
    }

    /// 1163: a pinch's frame at the step below covers more than the
    /// frontend shows of it scaled; the tiles outside the screen and the
    /// margins are not owed. After a frame at rest at z14.3 the step's
    /// frame owes nothing a pinch in, where it owed the tiles past them.
    #[test]
    fn a_pinch_owes_only_the_tiles_it_shows() {
        let centre = project(LatLon::from_degrees(43.7731, 11.2560));
        let theme = MapTheme::Light;
        let at = |z: u32| {
            let mut v = ExactView::new(centre, 15 * MILLI, 900, 700);
            v.zoom_about(z, 450.0, 350.0);
            v
        };
        let mut pinch = florence();
        pinch.set_workers(1);
        pinch.frame(&ask(at(14_300), false, 2.75, theme));
        let got = pinch.frame(&Ask {
            scaled_while_moving: true,
            time_zones: None,
            ..ask(at(14_600), true, 2.75, theme)
        });
        assert_eq!(got.tile_zoom, 14);
        assert_eq!(got.missing, 0);
        assert!(got.rgba.chunks_exact(4).all(|p| p[3] == 0xff));
    }

    /// 1422: a frame at rest on a device whose passes are slow adds them
    /// back as the budget holds them, owing one more frame until the
    /// picture is full; a pass whose buffer the memory cannot hold is
    /// skipped and the picture still ends full; a moving frame runs the
    /// held rung's passes; a frame without a clock runs every one. The
    /// costs are set, not measured, so the host's speed decides nothing.
    #[test]
    fn a_frame_at_rest_adds_the_passes_its_budget_holds() {
        use leviculum_map::ladder::BUDGET_US;
        let centre = project(LatLon::from_degrees(43.7731, 11.2560));
        let view = ExactView::new(centre, 15 * MILLI, 480, 320);
        let at = |now: Option<u64>, moving: bool, view: ExactView| Ask {
            now_ms: now,
            ..ask(view, moving, 1.0, MapTheme::Light)
        };

        let mut frames = florence();
        // A slow device: the ground and the names a budget each.
        frames.ladder.observe(Pass::Ground, BUDGET_US);
        frames.ladder.observe(Pass::Labels, BUDGET_US);
        let first = frames.frame(&at(Some(0), false, view));
        assert!(first.layers.has(Pass::Ground) && first.layers.has(Pass::Pins));
        assert!(!first.layers.has(Pass::Labels), "the names wait a frame");
        assert!(first.missing >= 1, "owing the names, it asks again");
        let second = frames.frame(&at(Some(16), false, view));
        assert!(second.layers.has(Pass::Labels));
        assert!(second.missing >= 1, "the smoothing, owed");
        let third = frames.frame(&at(Some(32), false, view));
        assert_eq!(third.layers, Layers::ALL);
        assert_eq!(third.missing, 0, "full");
        assert_eq!(
            frames.tile_stats().smoothed,
            0,
            "drawn smoothed at rest at once"
        );

        // Too little memory for the names' sprites: skipped, not owed.
        let mut frames = florence();
        frames.set_heap_limit(Some(0));
        let low = frames.frame(&at(Some(0), false, view));
        assert!(!low.layers.has(Pass::Labels));
        assert_eq!(frames.rest_skipped(), Layers::NONE.with(Pass::Labels));
        assert_eq!(low.missing, 0, "full without them");
        frames.set_heap_limit(None);

        // Moving at the minimal rung the names ride, at the full one
        // they are placed.
        let mut frames = florence();
        let _ = frames.frame(&at(Some(0), false, view));
        frames.hold_rung(Some(2));
        let mut moved = view;
        moved.pan(40, 0);
        frames.clear_tiles();
        let before = frames.tile_stats().rendered;
        let minimal = frames.frame(&at(Some(16), true, moved));
        assert_eq!((minimal.rung, minimal.layers), (2, Layers::of_rung(2)));
        // The tiles it drew are drafts: the frame at rest draws them
        // again smoothed, and the picture differs at the edges only.
        let drafted = frames.tile_stats().rendered - before;
        assert!(drafted > 0);
        let rest = frames.frame(&at(Some(32), false, moved));
        assert_eq!(rest.missing, 0);
        assert_eq!(frames.tile_stats().smoothed, drafted, "every draft again");
        assert_ne!(minimal.rgba, rest.rgba, "smoothed edges");
        frames.hold_rung(None);
        assert_eq!(frames.frame(&at(None, true, moved)).layers, Layers::ALL);
    }
}
