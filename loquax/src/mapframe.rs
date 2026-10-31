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
//! as sprites; a frame while the view moves draws them where the view
//! moved them ([`moved`]), so they never leave the picture. A frame that
//! zooms holds them to the ladder of its own zoom: a name whose
//! `min_zoom` the zoom no longer reaches is not drawn (1205). The pins are
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

use std::time::Instant;

use leviculum_map::labels::{LabelKind, Rect, SUBPIXEL};
use leviculum_map::viewport::{ExactView, MILLI};
use leviculum_mapfill::style::{MapTheme, Style};

use crate::mapnames::{self, Sprite, Typesetter};
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
    /// The frontend scales a frame on screen to the view it keeps, with a
    /// filter (the phone's Canvas, the window's image): a frame while a
    /// pinch or a wheel zooms (moving, at a zoom other than the last frame
    /// at rest's) is then drawn at its tile step one to one
    /// ([`Shot::shows`]), the tiles copied and not resampled, and the
    /// frontend scales it the rest of the way (1155). A pan at the zoom
    /// at rest stays drawn at it, its names as sharp as at rest.
    pub scaled_while_moving: bool,
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
    /// Tiles it needed and did not draw: ask again.
    pub missing: usize,
    /// The credit of the regions drawn, the outline's when none.
    pub attribution: String,
    /// The maps directory's epoch it was drawn at.
    pub epoch: u64,
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
            attribution: framed.attribution,
            epoch: framed.epoch,
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
            attribution: self.attribution,
            epoch: self.epoch,
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
    pub attribution: String,
    pub epoch: u64,
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
    /// The names the last frame at rest placed.
    names: Vec<Sprite>,
    /// The frame they were placed in.
    names_view: Option<ExactView>,
    /// The pan's velocity and the last zoom's way (1135).
    motion: Motion,
    /// The last frame, when it was at rest and owed no tile, with the
    /// zooms the regions allow.
    rest: Option<(TileFrame, (u8, u8))>,
    /// What the prefetch draws for that frame, made on its first call.
    plan: Option<Plan>,
    /// The pins the last frame placed, in its pixels.
    placed: Vec<Placed>,
    /// The names the last frame drew, by their index in `names`, each
    /// moved by whole pixels from where it was placed.
    drawn_names: Vec<(usize, (i64, i64))>,
    /// What the last frame that zoomed spent on the names it carried.
    carry: CarryCost,
}

/// What a frame that zooms spends on the names of the last frame at rest
/// (1205): a measuring tool's row.
#[derive(Clone, Copy, Debug, Default)]
pub struct CarryCost {
    /// The names it carried.
    pub carried: usize,
    /// Each moved and held to the ladder of the frame's zoom, in
    /// nanoseconds.
    pub ladder_ns: u64,
}

/// A name a frame drew ([`Frames::drawn_names`]): where, in the frame's
/// pixels, and what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawnName {
    pub text: String,
    pub kind: LabelKind,
    pub min_zoom: u8,
    /// The boxes it was placed by, moved with it.
    pub boxes: Vec<Rect>,
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
            motion: Motion::default(),
            rest: None,
            plan: None,
            placed: Vec::new(),
            drawn_names: Vec::new(),
            carry: CarryCost::default(),
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
        self.rest = None;
        self.plan = None;
        self.painter.refresh();
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
        let rest_at = self.names_view.map(|v| v.zoom_milli());
        let zooming = ask.moving && rest_at != Some(view.zoom_milli());
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
        if !ask.moving && missing == 0 {
            self.rest = Some((asked, ask.zooms));
        }
        let (w, h) = drawn.size();
        let (scale, theme) = (ask.scale, ask.style.theme);
        let type_ = &mut self.type_;
        let placed = match ask.measure {
            Some(measure) => screenmap::place_exact(ask.pins, &drawn, ask.selected, scale, measure),
            None => screenmap::place_exact(ask.pins, &drawn, ask.selected, scale, |t, s| {
                type_.width(t, s)
            }),
        };
        if !ask.moving {
            // At rest the names are placed anew over the frame, at the
            // exact scale, after the pins, and kept in world terms.
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
            let taken = screenmap::taken(&placed, scale);
            let names = mapnames::place(w, h, &labels, px, &taken, scale, &mut self.type_);
            #[cfg(test)]
            if self.type_.unkept {
                self.names = names.iter().map(Sprite::of_whole_reach).collect();
            } else {
                self.names = Sprite::of_all(&names, self.tiles.workers());
            }
            #[cfg(not(test))]
            {
                self.names = Sprite::of_all(&names, self.tiles.workers());
            }
            self.names_view = Some(drawn);
        }
        self.drawn_names.clear();
        if let Some(rest) = self.names_view {
            // A frame that zooms away from the names' frame draws those
            // the ladder of its own zoom shows: the step a frame at rest
            // there would look its names up at. A pan at that zoom keeps
            // them all, as at rest.
            let zoomed = ask.moving && rest.zoom_milli() != view.zoom_milli();
            let band = rest_zoom;
            let started = Instant::now();
            for (i, name) in self.names.iter().enumerate() {
                if zoomed && name.min_zoom > band {
                    continue;
                }
                let (x, y) = moved(&rest, &drawn, name.centre);
                let by = ((x - name.centre.0).round(), (y - name.centre.1).round());
                self.drawn_names.push((i, (by.0 as i64, by.1 as i64)));
            }
            if zoomed {
                self.carry = CarryCost {
                    carried: self.names.len(),
                    ladder_ns: nanos(started),
                };
            }
            for &(i, (dx, dy)) in &self.drawn_names {
                self.names[i].draw(rgba, w, h, theme, dx, dy);
            }
        }
        if ask.draw_pins {
            screenmap::draw_pins(rgba, w, h, &placed, scale, theme, &mut self.type_);
        }
        self.placed = placed;
        Framed {
            view: drawn,
            shows,
            tile_zoom,
            rest_zoom,
            missing,
            attribution,
            epoch: self.painter.epoch(),
        }
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
        self.drawn_names
            .iter()
            .map(|&(i, (dx, dy))| {
                let name = &self.names[i];
                let by = |v: i64| i32::try_from(v).unwrap_or(0);
                DrawnName {
                    text: name.text.clone(),
                    kind: name.kind,
                    min_zoom: name.min_zoom,
                    boxes: name
                        .boxes
                        .iter()
                        .map(|b| Rect {
                            x0: b.x0 + by(dx),
                            y0: b.y0 + by(dy),
                            x1: b.x1 + by(dx),
                            y1: b.y1 + by(dy),
                        })
                        .collect(),
                }
            })
            .collect()
    }

    /// What the last frame that zoomed spent on the names it carried.
    pub fn carry_cost(&self) -> CarryCost {
        self.carry
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
            measure: None,
            scaled_while_moving: false,
        }
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
            ..ask(view, true, 2.75, theme)
        };
        let mut pinch = florence();
        pinch.frame(&ask(at(15_000), false, 2.75, theme));
        let got = pinch.frame(&scaled(at(15_400)));
        assert_eq!((got.tile_zoom, got.shows.zoom_milli()), (15, 15_000));
        assert_eq!(got.shows.centre, at(15_400).centre);
        assert_eq!(got.shows.size(), (900, 700));
        assert_eq!(got.view.size(), (900 + 2 * 225, 700 + 2 * 175));
        let mut pan = florence();
        pan.frame(&ask(at(15_000), false, 2.75, theme));
        let want = pan.frame(&ask(at(15_000), true, 2.75, theme));
        assert!(got.rgba == want.rgba, "the step's frame, byte for byte");
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

    /// One frame of a pinch: its zoom, the names drawn, and those the
    /// ladder of its zoom does not show.
    #[derive(Debug)]
    struct PinchFrame {
        zoom_milli: u32,
        drawn: usize,
        below: usize,
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
                ..ask(view, true, SCALE, theme)
            });
            let drawn = frames.drawn_names();
            let band = view.tile_zoom();
            out.push(PinchFrame {
                zoom_milli: view.zoom_milli(),
                drawn: drawn.len(),
                below: drawn.iter().filter(|n| n.min_zoom > band).count(),
            });
        }
        for f in &out {
            eprintln!("{f:?}");
        }
        out
    }

    /// 1205: a pinch out draws no name the ladder of the frame's zoom
    /// does not show, so the streets of z15 are gone below z14.5. Before,
    /// every frame drew all eight names of the frame at rest, seven of
    /// them below the ladder from z14.4 and all eight from z13.35.
    #[test]
    fn a_pinch_out_holds_the_names_to_the_ladder() {
        let frames = pinch_out();
        let below: Vec<_> = frames.iter().filter(|f| f.below > 0).collect();
        assert!(below.is_empty(), "names below the ladder: {below:?}");
        let at = |z: u32| frames.iter().find(|f| f.zoom_milli == z).map(|f| f.drawn);
        assert_eq!(at(14_850), Some(8), "the streets while z15's ladder holds");
        assert!(at(14_400) < at(14_850), "fewer from z14.4");
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
            ..ask(at(14_600), true, 2.75, theme)
        });
        assert_eq!(got.tile_zoom, 14);
        assert_eq!(got.missing, 0);
        assert!(got.rgba.chunks_exact(4).all(|p| p[3] == 0xff));
    }
}
