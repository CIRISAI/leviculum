//! The map's tile cache (1134, `docs/drafts/map-renderer-roadmap-2026-10.md`,
//! section 5): the map without its names and pins drawn in square tiles of
//! one zoom step's pixel grid, kept in memory by last use, and a screen
//! composed from them at any zoom between the steps.
//!
//! A tile is [`TILE`] pixels of zoom `z` on a side, tile `(x, y)` the
//! pixels `x * TILE ..` and `y * TILE ..` of the world at that zoom, drawn
//! by [`Painter::draw`] as a view of its own. A view of the world is the
//! same pixels however it is cut, so tiles side by side make the picture
//! one view makes, seam for seam. The names are not baked in: they are
//! placed across the whole view in world terms and drawn over the tiles
//! (`crate::mapnames::paint_placed`), as the pins are, so a pin that
//! moves costs no tile and a name never breaks at a seam.
//!
//! A frame takes the tiles its view needs from the cache and draws the
//! missing ones on worker threads, one [`Painter`] each, as many as the
//! process may run on ([`crate::picture::cores`]). A frame may be told to draw only a
//! few ([`Frame::render_limit`]): the ones it leaves are taken from
//! other zooms' tiles that are cached, scaled, so a pinch that crosses a
//! step keeps showing the map until the new step's tiles land (1143): the
//! coarser steps first, scaled up, the nearest first; where none is held,
//! the finer ones scaled down, 2x2 of the next step or 4x4 of the one
//! after, which is what a zoom out has. Where no cached tile covers a
//! pixel the world outline shows ([`outline::draw`], the theme's
//! colours), and off the projection the theme's water: a frame never
//! shows black. While the view moves ([`Frame::moving`], 1163) the stand
//! ins and the outline cover only the owed tiles' own pixels, a cached
//! step taken nearest, its pixels doubled or halved; at rest they are
//! scaled bilinear over the frame. A tile wholly outside the pixels a
//! frontend shows ([`Frame::asked`]) is used when held, and otherwise
//! neither drawn nor owed: its pixels are water.
//!
//! The cache is dropped when the maps directory changes (its epoch) and
//! when the style does (the light or dark table, the line widths of the
//! screen's density). A tile drawn while a region in it was still being
//! written, some of its data stood in for ([`crate::picture::Drawn::pending`],
//! 1291), is held until the region gains tiles ([`Maps::progress`]) and
//! then drawn again, so the map fills in while the region arrives. [`BUDGET_MB`] bounds it; a prefetched tile no frame
//! used yet goes first, then the least recently used.
//!
//! A view at rest prefetches (1135): [`plan`] lists the zoom out of the
//! whole frame first after a pinch that went out (1143), the ring of tiles
//! around the frame, then the next zoom in over its central part, then a
//! second ring, each ordered
//! by the last pan's direction ([`Motion`]), and
//! [`TileCache::prefetch`] draws them a batch a call, within
//! [`PREFETCH_BUDGET`] tiles a plan, never evicting a tile the last frame
//! showed nor one prefetched since.

use std::collections::HashMap;
use std::string::{String, ToString};
use std::sync::Arc;
use std::time::Instant;
use std::vec::Vec;

use leviculum_map::blend::{Blend, Shape};
use leviculum_map::draw::Ink;
use leviculum_map::mercator::World;
use leviculum_map::outline;
use leviculum_map::viewport::{ExactView, Viewport, MAX_VIEW_ZOOM};
use leviculum_mapfill::frame::{rgb888, Frame as Fb};
use leviculum_mapfill::style::{MapTheme, Style};

use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::pixelcolor::Rgb565;

use crate::picture::{Maps, Painter};

/// A tile's side in pixels of its zoom step.
pub const TILE: u32 = 256;
/// The cache's bound: 384 tiles of 256 KiB, a 1080 x 2400 screen with a
/// quarter screen of margin three times over.
pub const BUDGET_MB: usize = 96;
/// A tile's bytes.
pub const TILE_BYTES: usize = (TILE * TILE * 4) as usize;
/// Tiles one view at rest prefetches at most (1135): 32 MB, about the
/// ring and the zoom in around a phone's frame with its margins.
pub const PREFETCH_BUDGET: usize = 128;
/// The pan velocity's moving average reaches a new speed within about
/// this many milliseconds of panning.
pub const MOTION_SMOOTH_MS: f64 = 250.0;
/// At rest the velocity decays by `e` every this many milliseconds:
/// from a fling of 5000 pixels a second below [`MOTION_FLOOR`] in 2.3 s.
pub const MOTION_REST_MS: f64 = 500.0;
/// A velocity below this many screen pixels a second has no direction.
pub const MOTION_FLOOR: f64 = 50.0;
/// The time between two pans the average counts at most: a pan after a
/// rest is one frame's worth of motion, not one spread over the rest.
const MOTION_GAP_MS: u64 = 250;
/// The coarser steps a frame takes cached tiles from for those it does
/// not draw: three, a coarse tile scaled up eight times at most.
pub const COARSER_STEPS: u8 = 3;
/// The finer steps likewise: two, 4x4 tiles scaled into one.
pub const FINER_STEPS: u8 = 2;
/// The steps from its own whose tiles a frame at rest that owes none
/// keeps (1797): the step out and the step in, which the prefetch draws
/// for the next pinch ([`plan`]). The others belong to views the user
/// left. Held to the budget, they kept the phone's map 80 MB above its
/// start after a zoom out to the world and back at Lew's 1224 x 2197
/// (`loquax-android/tests/map_heap.rs`, on the host, 2026-11-13), on the
/// day the low-memory killer ended Loquax in the foreground.
pub const NEAR_STEPS: u8 = 1;

/// What tells one tile from another: where it is, and what it was drawn
/// in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub zoom: u8,
    pub x: u32,
    pub y: u32,
    pub style: Style,
    /// The line widths, per cent of the zoom's.
    pub percent: u32,
}

/// A tile as a frame holds it: its pixels and its credit.
type Held = (Arc<Vec<u8>>, Arc<str>);

/// Where a pixel samples along one axis: the tile and the offset in it of
/// the sample and of its next neighbour, and the neighbour's weight out
/// of 256.
type Step = ((usize, usize), (usize, usize), u32);

/// The pixels a grid's tile columns and rows are sampled for,
/// `(from, to)` each: empty for one only a neighbour reads.
type Spans = (Vec<(u32, u32)>, Vec<(u32, u32)>);

/// Pixels of a frame: the columns `x0..x1` of the rows `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    fn has_row(&self, y: u32) -> bool {
        (self.y0..self.y1).contains(&y)
    }
}

/// What [`Grid::sample`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    /// Every tile is there: every pixel, what the buffer held not read
    /// (1154).
    Whole,
    /// At a scale of one, the pixels of the tiles there, whatever the
    /// buffer held; the others left as they are (1163).
    Held,
    /// The pixels nothing covers yet (alpha zero) whose tile is there.
    Under,
}

struct Entry {
    rgba: Arc<Vec<u8>>,
    attribution: Arc<str>,
    used: u64,
    /// Drawn ahead by [`TileCache::prefetch`] and not used by a frame
    /// since: evicted first.
    prefetched: bool,
    /// Drawn while a region in it was still arriving (1291): dropped on
    /// the next progress.
    provisional: bool,
    /// Drawn without its edges smoothed, while the view moved at a rung
    /// that leaves them out (1422): a frame that smooths draws it again.
    draft: bool,
}

/// What the cache did since it was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Tiles a frame found cached.
    pub hits: u64,
    /// Tiles a frame did not find.
    pub misses: u64,
    /// Tiles a frame drew.
    pub rendered: u64,
    /// Draft tiles a smoothing frame drew again (1422).
    pub smoothed: u64,
    /// Tiles drawn ahead by [`TileCache::prefetch`].
    pub prefetched: u64,
    /// Hits on a prefetched tile no frame had used before.
    pub prefetch_hits: u64,
    /// Tiles dropped for room.
    pub evicted: u64,
    /// Times the whole cache was dropped: a new epoch or style.
    pub cleared: u64,
    /// Provisional tiles dropped to be drawn again as a region arrived
    /// (1291).
    pub redrawn: u64,
    /// Tiles a frame at rest dropped for being more than a step from its
    /// zoom (1797, [`NEAR_STEPS`]).
    pub left_behind: u64,
    /// Microseconds the workers spent drawing tiles, each worker's own
    /// time summed: on one worker the CPU a cold frame's tiles cost; on
    /// more it is that or above, as the workers share cores.
    pub render_us: u64,
}

/// One frame asked of the cache.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// The view, at its exact zoom.
    pub view: ExactView,
    /// The zoom step whose tiles it is composed from.
    pub zoom: u8,
    pub style: Style,
    /// Screen pixels a logical one: the line widths.
    pub scale: f32,
    /// Draw at most this many missing tiles; all with `None`.
    pub render_limit: Option<usize>,
    /// A zoom step whose cached tiles stand in for those not drawn, as
    /// the coarser and finer steps every frame looks at do, in their
    /// order however far it is.
    pub fallback: Option<u8>,
    /// A finger or a pointer moves the view: the tiles not drawn are
    /// stood in for over their own pixels only, nearest (1163).
    pub moving: bool,
    /// The pixels a frontend shows of the frame, `[x0, y0, x1, y1)`, when
    /// not all of them: a tile wholly outside that is not held is neither
    /// drawn nor owed, its pixels water (1163).
    pub asked: Option<[u32; 4]>,
    /// Tiles missing drawn with their edges smoothed
    /// ([`Painter::draw_rgba`]); without, as drafts (1422).
    pub smooth: bool,
    /// The drafts held drawn again, smoothed, as far as the render limit
    /// leaves room; the others shown as they are and owed. Asks
    /// [`smooth`](Self::smooth) too.
    pub resmooth: bool,
}

impl Frame {
    /// The line widths, per cent of the zoom's.
    fn percent(&self) -> u32 {
        (self.scale * 100.0).round().max(1.0) as u32
    }
}

/// A frame composed.
#[derive(Debug)]
pub struct Composed {
    /// RGBA rows, the view's size.
    pub rgba: Vec<u8>,
    /// The credit of the regions drawn, the outline's when none.
    pub attribution: String,
    /// Tiles the frame needed and did not draw: ask again.
    pub missing: usize,
}

/// The cache over one maps directory.
pub struct TileCache {
    maps: Maps,
    budget: usize,
    epoch: u64,
    /// The maps' progress the provisional tiles were drawn at.
    progress: u64,
    /// Provisional tiles the last frame showed.
    filling: usize,
    style: Option<(Style, u32)>,
    entries: HashMap<TileKey, Entry>,
    bytes: usize,
    tick: u64,
    /// The tick the last frame started at: an entry used since is on
    /// screen, or prefetched since.
    shown_from: u64,
    painters: Vec<Painter>,
    workers: usize,
    /// Blend world views through this projection (1710), `None` for
    /// Mercator throughout: a frame the blend holds is drawn whole by a
    /// painter instead of composed from tiles — no square tile of one
    /// step can show a curved picture. The painters themselves keep
    /// drawing tiles Mercator-linear.
    world_shape: Option<Shape>,
    /// The screen the frames are drawn for ([`set_screen`](Self::set_screen)):
    /// a frame with margins blends at the screen's `t` (1796).
    screen: Option<(u32, u32)>,
    pub stats: Stats,
    /// A frame's buffer handed back ([`recycle`](Self::recycle)), composed
    /// into next.
    spare: Vec<u8>,
    /// Compose as before 1154, no buffer reused and no fast path: the
    /// reference a test holds the fast path to.
    #[cfg(test)]
    pub(crate) reference: bool,
}

impl TileCache {
    /// A cache over `maps` of at most `budget` bytes.
    pub fn new(maps: Maps, budget: usize) -> Self {
        let workers = crate::picture::cores();
        Self {
            epoch: maps.epoch(),
            progress: maps.progress(),
            filling: 0,
            maps,
            budget,
            style: None,
            entries: HashMap::new(),
            bytes: 0,
            tick: 0,
            shown_from: 0,
            painters: Vec::new(),
            workers,
            world_shape: None,
            screen: None,
            stats: Stats::default(),
            spare: Vec::new(),
            #[cfg(test)]
            reference: false,
        }
    }

    /// Take `rgba` back, a frame's buffer once shown: the next frame of
    /// the same size is composed into it.
    pub fn recycle(&mut self, rgba: Vec<u8>) {
        self.spare = rgba;
    }

    /// Draw on `n` threads at most (at least one).
    pub fn set_workers(&mut self, n: usize) {
        self.workers = n.max(1);
    }

    /// Blend world views through `shape` (1710), or through none.
    pub fn set_world_shape(&mut self, shape: Option<Shape>) {
        self.world_shape = shape;
    }

    /// The screen the next frames are drawn for, `None` for frames that
    /// are their screen. A frame with margins past the screen is larger
    /// than it, and the blend's start follows the size
    /// ([`world_floor`](leviculum_map::blend::world_floor), 1796): without
    /// this a frame with margins would still curve where the screen's view
    /// is Mercator already, and be placed as a flat one.
    pub fn set_screen(&mut self, screen: Option<(u32, u32)>) {
        self.screen = screen;
    }

    /// The blended mapping of `view` when the blend holds there: the
    /// shape is set, `t` is below one at the view's exact zoom, and no
    /// region reaches into it (the painter's own check, so the frame and
    /// the whole-view drawers agree on the condition). The blend carries
    /// the view's own size and the screen's `t`
    /// ([`set_screen`](Self::set_screen)); the region check widens to the
    /// whole zoom below, which only errs towards Mercator.
    pub fn world_blend(&mut self, view: &ExactView) -> Option<Blend> {
        let shape = self.world_shape?;
        let zoom = view.zoom();
        let (w, h) = view.size();
        let (sw, sh) = self.screen.unwrap_or((w, h));
        let t = Blend::new(shape, view.centre, zoom, sw, sh).t();
        let blend = Blend::with_t(shape, view.centre, zoom, w, h, t);
        if blend.t() >= 1.0 {
            return None;
        }
        if self.painters.is_empty() {
            self.painters.push(Painter::new(self.maps.clone()));
        }
        let floor = view.tile_zoom().min(zoom as u8);
        let whole = view.at_zoom(floor);
        self.painters[0].world_only(&whole).then_some(blend)
    }

    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Tiles held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Drop every tile.
    pub fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.stats.cleared += 1;
        }
        self.entries.clear();
        self.bytes = 0;
    }

    /// Whether the tile of `key` is held.
    pub fn holds(&self, key: &TileKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Tiles the last frame showed that were drawn while a region in them
    /// was still arriving (1291): the frame is drawn again as it gains
    /// tiles.
    pub fn filling(&self) -> usize {
        self.filling
    }

    /// Drop the cache when the directory or the style changed since the
    /// last frame, and the provisional tiles when a region gained tiles.
    fn check(&mut self, style: Style, percent: u32) {
        let epoch = self.maps.epoch();
        if epoch != self.epoch {
            self.epoch = epoch;
            self.clear();
        }
        let progress = self.maps.progress();
        if progress != self.progress {
            self.progress = progress;
            let before = self.entries.len();
            self.entries.retain(|_, e| !e.provisional);
            self.bytes = self.entries.values().map(|e| e.rgba.len()).sum();
            self.stats.redrawn += (before - self.entries.len()) as u64;
        }
        if self.style.is_some_and(|s| s != (style, percent)) {
            self.clear();
        }
        self.style = Some((style, percent));
    }

    /// `frame` composed from the tiles of its zoom step.
    pub fn compose(&mut self, frame: &Frame) -> Composed {
        let (w, h) = frame.view.size();
        let len = w as usize * h as usize * 4;
        #[cfg(test)]
        if self.reference {
            self.spare = Vec::new();
        }
        let reused = self.spare.len() == len;
        let mut rgba = match reused {
            true => std::mem::take(&mut self.spare),
            false => vec![0u8; len],
        };
        // Sized here to the view, so it is composed.
        let (attribution, missing) = self
            .compose_into(frame, &mut rgba, !reused)
            .unwrap_or_else(|| (outline::ATTRIBUTION.to_string(), 0));
        Composed {
            rgba,
            attribution,
            missing,
        }
    }

    /// [`compose`](Self::compose) into `rgba`, the view's size, which a
    /// frontend owns (the phone's Bitmap, 1155); `blank` when it is all
    /// zero, otherwise it may hold anything. The credit and the tiles
    /// owed; none for a buffer of another size, which is left as it is.
    pub fn compose_into(
        &mut self,
        frame: &Frame,
        rgba: &mut [u8],
        blank: bool,
    ) -> Option<(String, usize)> {
        let percent = frame.percent();
        self.check(frame.style, percent);
        self.shown_from = self.tick + 1;
        let (w, h) = frame.view.size();
        if rgba.len() != w as usize * h as usize * 4 {
            return None;
        }
        // A world view the blend holds (1710) is drawn whole, Equal
        // Earth or the globe flowing into Mercator: no square tile of
        // one step can compose the curved picture, and the outline is
        // cheap enough to draw each frame.
        if let Some(blend) = self.world_blend(&frame.view) {
            // Straight into `rgba` (1797): no frame-sized buffer beside it.
            let smooth = frame.smooth || frame.resmooth;
            let drawn =
                self.painters[0].draw_world_onto(&blend, Ink::Colour, frame.style, smooth, rgba);
            if !frame.moving {
                self.keep_near(frame.zoom);
            }
            return drawn;
        }
        let grid = Grid::of(&frame.view, frame.zoom);
        let keys = grid.keys(frame.style, percent);
        // The pixels each tile column and row is sampled for: what a tile
        // missing leaves to cover, and whether it is asked at all.
        let spans = (frame.moving || frame.asked.is_some()).then(|| grid.spans(&frame.view));
        let asked = |i: usize| match (frame.asked, &spans) {
            (Some(a), Some(spans)) => grid
                .rect(spans, i)
                .is_some_and(|r| r.x0 < a[2] && a[0] < r.x1 && r.y0 < a[3] && a[1] < r.y1),
            _ => true,
        };
        let mut got: Vec<Option<Held>> = Vec::with_capacity(keys.len());
        let mut missing: Vec<usize> = Vec::new();
        let mut unasked = false;
        for (i, key) in keys.iter().enumerate() {
            let found = key.and_then(|k| self.get(&k));
            match (key, &found) {
                (Some(_), Some(_)) => self.stats.hits += 1,
                // Outside what the frontend shows: neither drawn nor owed.
                (Some(_), None) if !asked(i) => unasked = true,
                (Some(_), None) => {
                    self.stats.misses += 1;
                    missing.push(i);
                }
                (None, _) => {}
            }
            got.push(found);
        }
        let limit = frame.render_limit.unwrap_or(usize::MAX).min(missing.len());
        let (draw, left) = missing.split_at(limit);
        // The drafts a smoothing frame draws again, within what the limit
        // leaves; the others are shown as they are and owed (1422).
        let drafts: Vec<usize> = match frame.resmooth {
            true => keys
                .iter()
                .enumerate()
                .filter(|(_, k)| k.is_some_and(|k| self.entries.get(&k).is_some_and(|e| e.draft)))
                .map(|(i, _)| i)
                .collect(),
            false => Vec::new(),
        };
        let room = frame
            .render_limit
            .unwrap_or(usize::MAX)
            .saturating_sub(draw.len());
        let (redraw, drafts_left) = drafts.split_at(room.min(drafts.len()));
        let wanted: Vec<TileKey> = draw.iter().chain(redraw).filter_map(|&i| keys[i]).collect();
        let smooth = frame.smooth || frame.resmooth;
        let drawn = self.render(&wanted, smooth);
        self.stats.rendered += draw.len() as u64;
        self.stats.smoothed += redraw.len() as u64;
        for (&i, (key, rgba, attribution, provisional)) in draw.iter().chain(redraw).zip(drawn) {
            let tile = (Arc::new(rgba), Arc::<str>::from(attribution));
            self.put(key, tile.0.clone(), tile.1.clone(), provisional, !smooth);
            got[i] = Some(tile);
        }
        self.filling = keys
            .iter()
            .flatten()
            .filter(|k| self.entries.get(k).is_some_and(|e| e.provisional))
            .count();
        // Every tile there: the grid writes every pixel, so the buffer
        // need not start blank and nothing is left to cover after.
        let whole = got.iter().all(Option::is_some);
        #[cfg(test)]
        let whole = whole && !self.reference;
        match (&spans, frame.moving && (!left.is_empty() || unasked)) {
            (Some(spans), true) => {
                self.cover_gaps(frame, percent, &grid, spans, &got, left, rgba, blank);
            }
            _ => self.cover_frame(frame, percent, &grid, &got, left, rgba, blank, whole),
        }
        let attribution = got
            .iter()
            .flatten()
            .map(|(_, a)| a)
            .find(|a| a.as_ref() != outline::ATTRIBUTION)
            .map_or_else(|| outline::ATTRIBUTION.to_string(), |a| a.to_string());
        let owed = left.len() + drafts_left.len();
        // At rest and whole, no tile of another step stands in any more.
        if !frame.moving && owed == 0 {
            self.keep_near(frame.zoom);
        }
        Some((attribution, owed))
    }

    /// The tiles `got` over `rgba`, the stand ins and the outline where
    /// one is missing, scaled over the whole frame: a frame at rest, or
    /// one that owes nothing.
    #[allow(clippy::too_many_arguments)]
    fn cover_frame(
        &mut self,
        frame: &Frame,
        percent: u32,
        grid: &Grid,
        got: &[Option<Held>],
        left: &[usize],
        rgba: &mut [u8],
        blank: bool,
        whole: bool,
    ) {
        if !blank && !whole {
            // Alpha zero is a pixel nothing covers yet: a tile's are opaque.
            rgba.fill(0);
        }
        let fill = match whole {
            true => Fill::Whole,
            false => Fill::Under,
        };
        grid.sample(&frame.view, got, rgba, self.workers, fill);
        if !left.is_empty() {
            for z in stand_ins(frame) {
                if covered(rgba) {
                    break;
                }
                let under = Grid::of(&frame.view, z);
                let keys = under.keys(frame.style, percent);
                if !keys.iter().flatten().any(|k| self.holds(k)) {
                    continue;
                }
                let tiles: Vec<Option<Held>> =
                    keys.iter().map(|k| k.and_then(|k| self.get(&k))).collect();
                under.sample(&frame.view, &tiles, rgba, self.workers, Fill::Under);
            }
        }
        if !whole && !covered(rgba) {
            match left.is_empty() {
                // Only the margin off the projection is left.
                true => {
                    let [r, g, b] = rgb888(outline::water(frame.style.theme));
                    fill_uncovered(rgba, |_| [r, g, b, 0xff]);
                }
                false => {
                    outline_under(&frame.view, frame.style.theme, rgba, self.workers, None);
                }
            }
        }
    }

    /// A frame while the view moves that owes tiles or leaves some out
    /// (1163): the tiles held over their pixels, the missing ones' pixels
    /// alone covered, the owed from the cached steps taken nearest, then
    /// the outline, those not asked and off the projection with water.
    /// The resample of the whole frame for a few owed tiles cost a pinch
    /// on one worker some 60 ms (1155).
    #[allow(clippy::too_many_arguments)]
    fn cover_gaps(
        &mut self,
        frame: &Frame,
        percent: u32,
        grid: &Grid,
        spans: &Spans,
        got: &[Option<Held>],
        left: &[usize],
        rgba: &mut [u8],
        blank: bool,
    ) {
        let (w, _) = frame.view.size();
        let owed: Vec<Rect> = left.iter().filter_map(|&i| grid.rect(spans, i)).collect();
        let water: Vec<Rect> = got
            .iter()
            .enumerate()
            .filter(|(i, t)| t.is_none() && !left.contains(i))
            .filter_map(|(i, _)| grid.rect(spans, i))
            .collect();
        let [r, g, b] = rgb888(outline::water(frame.style.theme));
        let sea = [r, g, b, 0xff];
        let nearest = grid.scale == 1.0;
        if !blank && !nearest {
            // Between steps a pixel blends a neighbour's tile: the pixel
            // by pixel path reads alpha zero as not covered yet.
            rgba.fill(0);
        }
        by_rows(rgba, w, self.workers, |y, out| {
            for rect in owed.iter().filter(|r| r.has_row(y)) {
                out[rect.x0 as usize * 4..rect.x1 as usize * 4].fill(0);
            }
            for rect in water.iter().filter(|r| r.has_row(y)) {
                for px in out[rect.x0 as usize * 4..rect.x1 as usize * 4].chunks_exact_mut(4) {
                    px.copy_from_slice(&sea);
                }
            }
        });
        let fill = match nearest {
            true => Fill::Held,
            false => Fill::Under,
        };
        grid.sample(&frame.view, got, rgba, self.workers, fill);
        for z in stand_ins(frame) {
            if covered_in(rgba, w, &owed) {
                return;
            }
            let under = Grid::of(&frame.view, z);
            let keys = under.keys(frame.style, percent);
            let cols: Vec<Step> = (0..w).map(|x| under.col(&frame.view, x, true)).collect();
            let rows: Vec<Step> = (0..frame.view.size().1)
                .map(|y| under.row(&frame.view, y, true))
                .collect();
            // Only the tiles of this step the owed pixels sample.
            let mut tiles: Vec<Option<Held>> = vec![None; keys.len()];
            let mut any = false;
            for rect in &owed {
                let (c0, c1) = (cols[rect.x0 as usize].0 .0, cols[rect.x1 as usize - 1].0 .0);
                let (r0, r1) = (rows[rect.y0 as usize].0 .0, rows[rect.y1 as usize - 1].0 .0);
                for row in r0..=r1 {
                    for col in c0..=c1 {
                        let i = row * under.cols + col;
                        if tiles[i].is_none() {
                            tiles[i] = keys[i].and_then(|k| self.get(&k));
                            any |= tiles[i].is_some();
                        }
                    }
                }
            }
            if any {
                under.stand_in(&tiles, &cols, &rows, rgba, &owed, self.workers);
            }
        }
        if !covered_in(rgba, w, &owed) {
            outline_under(
                &frame.view,
                frame.style.theme,
                rgba,
                self.workers,
                Some(&owed),
            );
        }
    }

    fn get(&mut self, key: &TileKey) -> Option<Held> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.entries.get_mut(key)?;
        entry.used = tick;
        if entry.prefetched {
            entry.prefetched = false;
            self.stats.prefetch_hits += 1;
        }
        Some((entry.rgba.clone(), entry.attribution.clone()))
    }

    fn put(
        &mut self,
        key: TileKey,
        rgba: Arc<Vec<u8>>,
        attribution: Arc<str>,
        provisional: bool,
        draft: bool,
    ) {
        self.tick += 1;
        self.bytes += rgba.len();
        let old = self.entries.insert(
            key,
            Entry {
                rgba,
                attribution,
                used: self.tick,
                prefetched: false,
                provisional,
                draft,
            },
        );
        if let Some(old) = old {
            self.bytes -= old.rgba.len();
        }
        while self.bytes > self.budget && self.entries.len() > 1 {
            if !self.evict(|_| true) {
                break;
            }
        }
    }

    /// Drop the tiles more than [`NEAR_STEPS`] from `zoom`, after a frame
    /// at rest there that owes no tile (a world frame owes none): it needs
    /// no stand in any more, the prefetch draws the steps next to it, and
    /// the others only hold memory (1797).
    fn keep_near(&mut self, zoom: u8) {
        let before = self.entries.len();
        let mut bytes = self.bytes;
        self.entries.retain(|key, e| {
            let near = key.zoom.abs_diff(zoom) <= NEAR_STEPS;
            if !near {
                bytes -= e.rgba.len();
            }
            near
        });
        self.bytes = bytes;
        self.stats.left_behind += (before - self.entries.len()) as u64;
    }

    /// Drop the entry `may` allows that goes first: a prefetched one no
    /// frame used, then the least recently used. Whether one went.
    fn evict(&mut self, may: impl Fn(&Entry) -> bool) -> bool {
        let Some(first) = self
            .entries
            .iter()
            .filter(|(_, e)| may(e))
            .min_by_key(|(_, e)| (!e.prefetched, e.used))
            .map(|(k, _)| *k)
        else {
            return false;
        };
        if let Some(e) = self.entries.remove(&first) {
            self.bytes -= e.rgba.len();
            self.stats.evicted += 1;
        }
        true
    }

    /// Hold a prefetched tile, making room only from tiles neither the
    /// last frame showed nor a prefetch since drew. Whether it is held.
    fn put_prefetched(
        &mut self,
        key: TileKey,
        rgba: Vec<u8>,
        attribution: String,
        provisional: bool,
    ) -> bool {
        let shown_from = self.shown_from;
        while self.bytes + rgba.len() > self.budget {
            if !self.evict(|e| e.used < shown_from) {
                return false;
            }
        }
        self.tick += 1;
        self.bytes += rgba.len();
        let old = self.entries.insert(
            key,
            Entry {
                rgba: Arc::new(rgba),
                attribution: Arc::from(attribution),
                used: self.tick,
                prefetched: true,
                provisional,
                draft: false,
            },
        );
        if let Some(old) = old {
            self.bytes -= old.rgba.len();
        }
        true
    }

    /// Draw the next tiles of `plan` not held, at most `n` (a batch, one
    /// a worker so a gesture waits for one tile's time at most), as
    /// prefetched tiles. A tile that wants spare room stops the plan when
    /// the cache has none; a tile that finds no room it may take stops it
    /// too. The tiles drawn.
    pub fn prefetch(&mut self, plan: &mut Plan, n: usize) -> usize {
        if let Some(first) = plan.wants.first() {
            self.check(first.key.style, first.key.percent);
        }
        let mut batch: Vec<TileKey> = Vec::new();
        while batch.len() < n && plan.drawn + batch.len() < plan.budget {
            let Some(want) = plan.wants.get(plan.next).copied() else {
                break;
            };
            if self.holds(&want.key) || batch.contains(&want.key) {
                plan.next += 1;
                continue;
            }
            let after = self.bytes + (batch.len() + 1) * TILE_BYTES;
            if want.spare_only && after > self.budget {
                plan.next = plan.wants.len();
                break;
            }
            plan.next += 1;
            batch.push(want.key);
        }
        let mut stored = 0;
        for (key, rgba, attribution, provisional) in self.render(&batch, true) {
            if !self.put_prefetched(key, rgba, attribution, provisional) {
                plan.next = plan.wants.len();
                break;
            }
            stored += 1;
        }
        self.stats.prefetched += stored as u64;
        plan.drawn += stored;
        if plan.drawn >= plan.budget {
            plan.next = plan.wants.len();
        }
        stored
    }

    /// Draw `keys`, split over the workers, each with its own painter.
    fn render(&mut self, keys: &[TileKey], smooth: bool) -> Vec<Rendered> {
        if keys.is_empty() {
            return Vec::new();
        }
        let n = self.workers.min(keys.len());
        while self.painters.len() < n {
            self.painters.push(Painter::new(self.maps.clone()));
        }
        // Each worker's own time drawing: its share of the CPU.
        let timed = |painter: &mut Painter, chunk: &[TileKey]| {
            let started = Instant::now();
            let drawn: Vec<_> = chunk
                .iter()
                .map(|k| draw_tile(painter, k, smooth))
                .collect();
            let us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            (drawn, us)
        };
        let (drawn, us) = if n == 1 {
            timed(&mut self.painters[0], keys)
        } else {
            let per = keys.len().div_ceil(n);
            std::thread::scope(|s| {
                let jobs: Vec<_> = self
                    .painters
                    .iter_mut()
                    .zip(keys.chunks(per))
                    .map(|(painter, chunk)| s.spawn(move || timed(painter, chunk)))
                    .collect();
                jobs.into_iter().map(|j| j.join().unwrap_or_default()).fold(
                    (Vec::new(), 0u64),
                    |(mut all, us), (drawn, t)| {
                        all.extend(drawn);
                        (all, us.saturating_add(t))
                    },
                )
            })
        };
        self.stats.render_us = self.stats.render_us.saturating_add(us);
        drawn
    }
}

/// The steps whose cached tiles stand in for those `frame` does not draw,
/// the first wanted first (1143): the coarser steps from the nearest, then
/// the finer ones from the nearest, the frame's own fallback among them.
fn stand_ins(frame: &Frame) -> Vec<u8> {
    let z = frame.zoom;
    let mut coarser: Vec<u8> = (1..=COARSER_STEPS)
        .filter_map(|d| z.checked_sub(d))
        .collect();
    let mut finer: Vec<u8> = (1..=FINER_STEPS)
        .map(|d| z.saturating_add(d))
        .filter(|&f| f <= MAX_VIEW_ZOOM && f != z)
        .collect();
    match frame.fallback {
        Some(f) if f < z && !coarser.contains(&f) => coarser.push(f),
        Some(f) if f > z && !finer.contains(&f) => finer.push(f),
        _ => {}
    }
    coarser.sort_unstable_by(|a, b| b.cmp(a));
    finer.sort_unstable();
    coarser.into_iter().chain(finer).collect()
}

/// Whether every pixel of `rgba` is covered.
fn covered(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).all(|p| p[3] != 0)
}

/// Whether every pixel of `rects` in `rgba`, `w` wide, is covered.
fn covered_in(rgba: &[u8], w: u32, rects: &[Rect]) -> bool {
    let stride = w as usize * 4;
    rects.iter().all(|r| {
        (r.y0..r.y1).all(|y| {
            let row = &rgba[y as usize * stride..(y as usize + 1) * stride];
            covered(&row[r.x0 as usize * 4..r.x1 as usize * 4])
        })
    })
}

/// `one_row(y, row)` over the rows of `rgba`, `w` wide, split into bands
/// over `workers` threads; on the calling thread with one.
fn by_rows(rgba: &mut [u8], w: u32, workers: usize, one_row: impl Fn(u32, &mut [u8]) + Sync) {
    let stride = w as usize * 4;
    let h = rgba.len().checked_div(stride).unwrap_or(0);
    let bands = workers.clamp(1, h.max(1));
    let per = h.div_ceil(bands).max(1);
    if bands == 1 {
        for (y, out) in rgba.chunks_exact_mut(stride).enumerate() {
            one_row(y as u32, out);
        }
        return;
    }
    std::thread::scope(|s| {
        for (b, band) in rgba.chunks_mut(per * stride).enumerate() {
            let one_row = &one_row;
            s.spawn(move || {
                for (i, out) in band.chunks_exact_mut(stride).enumerate() {
                    one_row((b * per + i) as u32, out);
                }
            });
        }
    });
}

/// Set every pixel not covered to `colour` of its index.
fn fill_uncovered(rgba: &mut [u8], colour: impl Fn(usize) -> [u8; 4]) {
    for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
        if px[3] == 0 {
            px.copy_from_slice(&colour(i));
        }
    }
}

/// A pixel of a drawn view, RGBA.
fn opaque(px: u16) -> [u8; 4] {
    let [r, g, b] = rgb888(Rgb565::from(RawU16::new(u16::from_be(px))));
    [r, g, b, 0xff]
}

/// The world outline in `theme` under the pixels of `rgba`, `view`'s,
/// nothing covers: drawn at the step nearest the view's zoom over its
/// area and taken nearest, rows split over `workers` (1143). Water off
/// the projection, as the outline clears to. With `only`, the pixels of
/// those rectangles alone are looked at (1163).
fn outline_under(
    view: &ExactView,
    theme: MapTheme,
    rgba: &mut [u8],
    workers: usize,
    only: Option<&[Rect]>,
) {
    let z = view.tile_zoom();
    let base = view.at_zoom(z);
    let (bw, bh) = base.size();
    let mut fb = vec![0u16; bw as usize * bh as usize];
    if let Some(mut target) = Fb::new(&mut fb, bw, bh) {
        let Ok(_) = outline::draw_world(&outline::WORLD, &base, Ink::Colour, theme, &mut target);
    }
    let s = view.scale_at(z);
    let (w, h) = view.size();
    let at = |p: u32, n: u32, bn: u32| {
        let u = f64::from(bn) / 2.0 + (f64::from(p) + 0.5 - f64::from(n) / 2.0) / s;
        (u.floor() as i64).clamp(0, i64::from(bn) - 1) as usize
    };
    let cols: Vec<usize> = (0..w).map(|x| at(x, w, bw)).collect();
    let one_row = |y: u32, out: &mut [u8]| {
        let src = &fb[at(y, h, bh) * bw as usize..];
        let mut put = |from: usize, to: usize| {
            for (px, &c) in out[from * 4..to * 4]
                .chunks_exact_mut(4)
                .zip(&cols[from..to])
            {
                if px[3] == 0 {
                    px.copy_from_slice(&opaque(src[c]));
                }
            }
        };
        match only {
            None => put(0, w as usize),
            Some(rects) => {
                for r in rects.iter().filter(|r| r.has_row(y)) {
                    put(r.x0 as usize, r.x1 as usize);
                }
            }
        }
    };
    by_rows(rgba, w, workers, one_row);
}

/// A tile drawn: its key, RGBA, its credit, and whether a region in it
/// was still arriving.
type Rendered = (TileKey, Vec<u8>, String, bool);

/// The tile of `key` drawn: RGBA, its credit, and whether a region in it
/// was still arriving (1291).
fn draw_tile(painter: &mut Painter, key: &TileKey, smooth: bool) -> Rendered {
    let shift = 24 - u32::from(key.zoom);
    let mid = |t: u32| ((u64::from(t) * u64::from(TILE) + u64::from(TILE / 2)) << shift) as u32;
    let view = Viewport::new(
        World {
            x: mid(key.x),
            y: mid(key.y),
        },
        key.zoom,
        TILE,
        TILE,
    );
    // Straight into RGBA (1422): no Rgb565 buffer, no pass to widen it.
    let drawn = painter.draw_rgba(&view, Ink::Colour, key.style, key.percent, smooth);
    (*key, drawn.rgba, drawn.attribution, drawn.pending > 0)
}

/// The pan's velocity, a moving average in screen pixels a second that
/// decays at rest (1135): what [`plan`] orders the tiles by, those ahead
/// of the last movement first. And whether the last pinch went out.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motion {
    velocity: (f64, f64),
    at_ms: Option<u64>,
    /// When the first frame since the last pan was asked: the heading
    /// the prefetch plans by is frozen there (1189).
    asked_ms: Option<u64>,
    /// The last zoom the fingers made went out.
    pub zoomed_out: bool,
}

impl Motion {
    /// The view moved by `(dx, dy)` screen pixels at `now_ms`.
    pub fn pan(&mut self, dx: f64, dy: f64, now_ms: u64) {
        let (vx, vy) = self.velocity(now_ms);
        let dt = self
            .at_ms
            .map_or(MOTION_GAP_MS, |at| now_ms.saturating_sub(at))
            .clamp(1, MOTION_GAP_MS) as f64;
        let a = 1.0 - (-dt / MOTION_SMOOTH_MS).exp();
        let (ix, iy) = (dx * 1000.0 / dt, dy * 1000.0 / dt);
        self.velocity = (vx + a * (ix - vx), vy + a * (iy - vy));
        self.at_ms = Some(now_ms);
        self.asked_ms = None;
    }

    /// A frame was asked at `now_ms`. The first since the last pan
    /// freezes the heading [`rest_heading`](Self::rest_heading) gives:
    /// the finger's last direction, not what is left of it once a slow
    /// renderer has drawn the frame at rest (1189).
    pub fn asked(&mut self, now_ms: u64) {
        self.asked_ms.get_or_insert(now_ms);
    }

    /// The fingers zoomed by `by_milli` thousandths of a step.
    pub fn zoom(&mut self, by_milli: i64) {
        if by_milli != 0 {
            self.zoomed_out = by_milli < 0;
        }
    }

    /// The velocity at `now_ms`, decayed since the last pan.
    pub fn velocity(&self, now_ms: u64) -> (f64, f64) {
        let Some(at) = self.at_ms else {
            return (0.0, 0.0);
        };
        let k = (-(now_ms.saturating_sub(at) as f64) / MOTION_REST_MS).exp();
        (self.velocity.0 * k, self.velocity.1 * k)
    }

    /// The direction the view last moved in, when it still moves faster
    /// than [`MOTION_FLOOR`].
    pub fn heading(&self, now_ms: u64) -> Option<(f64, f64)> {
        let (vx, vy) = self.velocity(now_ms);
        (vx.hypot(vy) >= MOTION_FLOOR).then_some((vx, vy))
    }

    /// The heading a prefetch at `now_ms` plans by: the one when the
    /// first frame since the last pan was asked ([`asked`](Self::asked)),
    /// the one at `now_ms` when none was.
    pub fn rest_heading(&self, now_ms: u64) -> Option<(f64, f64)> {
        self.heading(self.asked_ms.map_or(now_ms, |at| at.min(now_ms)))
    }
}

/// One tile a [`Plan`] draws ahead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Want {
    pub key: TileKey,
    /// Drawn only into room the cache has spare: the second ring.
    pub spare_only: bool,
}

/// The tiles a view at rest draws ahead, in order, and how far
/// [`TileCache::prefetch`] got.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub wants: Vec<Want>,
    /// The next want to look at.
    pub next: usize,
    /// Tiles drawn so far.
    pub drawn: usize,
    /// Tiles it draws at most.
    pub budget: usize,
}

impl Plan {
    /// Nothing left to draw.
    pub fn done(&self) -> bool {
        self.next >= self.wants.len()
    }
}

/// What `frame`, a view at rest, draws ahead (1135): the step out (no
/// shallower than `zooms.0`) over the whole frame first when the last
/// pinch went out, so the next pinch out has its tiles (1143), the ring of
/// tiles around its tiles at its step, the step in (no deeper than
/// `zooms.1`) over the frame's central half, and a second ring into spare
/// room only. Each group ordered by its tiles' offset from the view's
/// centre along `heading`, ahead first, nearest first between equals and
/// without a heading.
pub fn plan(frame: &Frame, heading: Option<(f64, f64)>, zoomed_out: bool, zooms: (u8, u8)) -> Plan {
    let percent = frame.percent();
    let mut wants: Vec<Want> = Vec::new();
    let mut add = |spots: Vec<Spot>, spare_only: bool| {
        for spot in ordered(spots, heading) {
            let key = TileKey {
                zoom: spot.zoom,
                x: spot.x,
                y: spot.y,
                style: frame.style,
                percent,
            };
            if !wants.iter().any(|w| w.key == key) {
                wants.push(Want { key, spare_only });
            }
        }
    };
    let z = frame.zoom;
    if zoomed_out && z > zooms.0 {
        add(Grid::of(&frame.view, z - 1).ring(0), false);
    }
    add(Grid::of(&frame.view, z).ring(1), false);
    if z < zooms.1.min(MAX_VIEW_ZOOM) {
        let mut inner = frame.view;
        let (w, h) = inner.size();
        inner.resize(w / 2, h / 2);
        add(Grid::of(&inner, z + 1).ring(0), false);
    }
    add(Grid::of(&frame.view, z).ring(2), true);
    Plan {
        wants,
        next: 0,
        drawn: 0,
        budget: PREFETCH_BUDGET,
    }
}

/// A tile of a ring and its centre's offset from the view's centre in
/// screen pixels.
#[derive(Clone, Copy, Debug)]
struct Spot {
    zoom: u8,
    x: u32,
    y: u32,
    offset: (f64, f64),
}

/// `spots` with those furthest along `heading` first; nearest first
/// between equals, and without one.
fn ordered(mut spots: Vec<Spot>, heading: Option<(f64, f64)>) -> Vec<Spot> {
    let unit = heading.map(|(x, y)| {
        let n = x.hypot(y).max(f64::MIN_POSITIVE);
        (x / n, y / n)
    });
    // Ahead by whole tiles of screen, so a tile a little to the side of
    // the heading is no later than one straight ahead of it.
    let ahead = |s: &Spot| {
        unit.map_or(0, |(ux, uy)| {
            -((s.offset.0 * ux + s.offset.1 * uy) / f64::from(TILE)).round() as i64
        })
    };
    let near = |s: &Spot| s.offset.0.hypot(s.offset.1);
    spots.sort_by(|a, b| ahead(a).cmp(&ahead(b)).then(near(a).total_cmp(&near(b))));
    spots
}

/// The tiles of one zoom step a view covers, and how its pixels sample
/// them.
struct Grid {
    zoom: u8,
    /// The view's centre in pixels of the step's world.
    cx: f64,
    cy: f64,
    /// Screen pixels a pixel of the step.
    scale: f64,
    /// The first tile column and row, and how many.
    tx0: i64,
    ty0: i64,
    cols: usize,
    rows: usize,
}

impl Grid {
    fn of(view: &ExactView, zoom: u8) -> Self {
        let unit = f64::from(1u32 << (24 - u32::from(zoom)));
        let scale = view.scale_at(zoom);
        let (w, h) = view.size();
        let (cx, cy) = (
            f64::from(view.centre.x) / unit,
            f64::from(view.centre.y) / unit,
        );
        let t = f64::from(TILE);
        // A pixel's sample point and its right or lower neighbour.
        let span = |c: f64, n: u32| {
            let first = c + (0.5 - f64::from(n) / 2.0) / scale - 0.5;
            let last = c + (f64::from(n) - 0.5 - f64::from(n) / 2.0) / scale + 0.5;
            ((first / t).floor() as i64, (last / t).floor() as i64)
        };
        let (tx0, tx1) = span(cx, w);
        let (ty0, ty1) = span(cy, h);
        Self {
            zoom,
            cx,
            cy,
            scale,
            tx0,
            ty0,
            cols: (tx1 - tx0 + 1) as usize,
            rows: (ty1 - ty0 + 1) as usize,
        }
    }

    /// The tiles' keys, row by row; `None` off the projection.
    fn keys(&self, style: Style, percent: u32) -> Vec<Option<TileKey>> {
        let n = 1i64 << self.zoom;
        let mut out = Vec::with_capacity(self.cols * self.rows);
        for r in 0..self.rows as i64 {
            for c in 0..self.cols as i64 {
                let (tx, ty) = (self.tx0 + c, self.ty0 + r);
                out.push((0..n).contains(&ty).then(|| TileKey {
                    zoom: self.zoom,
                    x: tx.rem_euclid(n) as u32,
                    y: ty as u32,
                    style,
                    percent,
                }));
            }
        }
        out
    }

    /// The tiles `k` tiles outside the grid's on every side, all of them
    /// with `k` zero, and their centres' offsets from the view's centre
    /// in its pixels; none off the projection, each once.
    fn ring(&self, k: i64) -> Vec<Spot> {
        let n = 1i64 << self.zoom;
        let t = f64::from(TILE);
        let (x0, y0) = (self.tx0 - k, self.ty0 - k);
        let (x1, y1) = (
            self.tx0 + self.cols as i64 - 1 + k,
            self.ty0 + self.rows as i64 - 1 + k,
        );
        let mut out: Vec<Spot> = Vec::new();
        for ty in y0..=y1 {
            for tx in x0..=x1 {
                let edge = tx == x0 || tx == x1 || ty == y0 || ty == y1;
                if (k > 0 && !edge) || !(0..n).contains(&ty) {
                    continue;
                }
                let x = tx.rem_euclid(n) as u32;
                if out.iter().any(|s| s.x == x && s.y == ty as u32) {
                    continue;
                }
                let offset = (
                    ((tx as f64 + 0.5) * t - self.cx) * self.scale,
                    ((ty as f64 + 0.5) * t - self.cy) * self.scale,
                );
                out.push(Spot {
                    zoom: self.zoom,
                    x,
                    y: ty as u32,
                    offset,
                });
            }
        }
        out
    }

    /// Where pixel `p` of `n` samples along one axis, the grid's first
    /// tile `first` of `count` and the centre `centre` in the step's
    /// pixels: nearest, or the pixel below and the weight of the next.
    fn step(&self, centre: f64, first: i64, count: usize, n: u32, p: u32, nearest: bool) -> Step {
        let t = i64::from(TILE);
        let u = centre + (f64::from(p) + 0.5 - f64::from(n) / 2.0) / self.scale - 0.5;
        let (q, f) = match nearest {
            true => ((u + 0.5).floor() as i64, 0),
            false => (u.floor() as i64, ((u - u.floor()) * 256.0) as u32),
        };
        let at = |q: i64| {
            let c = (q.div_euclid(t) - first) as usize;
            (c.min(count - 1), q.rem_euclid(t) as usize)
        };
        (at(q), at(q + 1), f)
    }

    /// Where column `x` of `view` samples: the tile column and offset of
    /// the sample and of its right neighbour, and the neighbour's weight
    /// out of 256.
    fn col(&self, view: &ExactView, x: u32, nearest: bool) -> Step {
        self.step(self.cx, self.tx0, self.cols, view.size().0, x, nearest)
    }

    /// Where row `y` of `view` samples, as [`col`](Self::col).
    fn row(&self, view: &ExactView, y: u32, nearest: bool) -> Step {
        self.step(self.cy, self.ty0, self.rows, view.size().1, y, nearest)
    }

    /// The pixels of `view` each tile column and row is sampled for, as
    /// [`sample`](Self::sample) samples them.
    fn spans(&self, view: &ExactView) -> Spans {
        let nearest = self.scale == 1.0;
        let (w, h) = view.size();
        let along = |n: u32, count: usize, step: &dyn Fn(u32) -> Step| {
            let mut out = vec![(0u32, 0u32); count];
            for p in 0..n {
                let ((i, _), _, _) = step(p);
                let span = &mut out[i];
                match span.0 == span.1 {
                    true => *span = (p, p + 1),
                    false => span.1 = p + 1,
                }
            }
            out
        };
        (
            along(w, self.cols, &|x| self.col(view, x, nearest)),
            along(h, self.rows, &|y| self.row(view, y, nearest)),
        )
    }

    /// The pixels tile `i` (in [`keys`](Self::keys)' order) is sampled
    /// for; none when it is no pixel's.
    fn rect(&self, spans: &Spans, i: usize) -> Option<Rect> {
        let (x0, x1) = spans.0[i % self.cols];
        let (y0, y1) = spans.1[i / self.cols];
        (x0 < x1 && y0 < y1).then_some(Rect { x0, y0, x1, y1 })
    }

    /// The pixels of `rects` in `rgba` nothing covers yet, from `tiles`
    /// taken nearest at `cols` and `rows` ([`col`](Self::col)), the step's
    /// pixels doubled or halved: the stand ins while the view moves
    /// (1163). A pixel whose tile is absent is left as it is.
    fn stand_in(
        &self,
        tiles: &[Option<Held>],
        cols: &[Step],
        rows: &[Step],
        rgba: &mut [u8],
        rects: &[Rect],
        workers: usize,
    ) {
        let w = cols.len() as u32;
        by_rows(rgba, w, workers, |y, out| {
            let ((r, oy), _, _) = rows[y as usize];
            for rect in rects.iter().filter(|rect| rect.has_row(y)) {
                let (x0, x1) = (rect.x0 as usize, rect.x1 as usize);
                let pixels = out[x0 * 4..x1 * 4].chunks_exact_mut(4);
                for (px, &((c, ox), _, _)) in pixels.zip(&cols[x0..x1]) {
                    if px[3] != 0 {
                        continue;
                    }
                    if let Some(Some((tile, _))) = tiles.get(r * self.cols + c) {
                        let j = (oy * TILE as usize + ox) * 4;
                        px.copy_from_slice(&tile[j..j + 4]);
                    }
                }
            }
        });
    }

    /// Sample `tiles` (in [`keys`](Self::keys)' order) into `rgba`, the
    /// view's pixels: nearest at a scale of one, so a frame at a whole
    /// step is the tiles' pixels, bilinear between steps. A pixel already
    /// covered (opaque) or whose tile is absent is left as it is; a
    /// neighbour whose tile is absent gives way to the nearest sample.
    /// Rows split over `workers`.
    ///
    /// With [`Fill::Whole`] every tile is there and every pixel is
    /// written, what `rgba` held before not read (1154): at a scale of one
    /// a row is copied in runs, one a tile it crosses; between steps the
    /// bilinear sums run two channels at once in the 32-bit halves of a
    /// `u64`. The same bytes as the pixel by pixel path, which a frame with
    /// a tile missing takes. [`Fill::Held`] copies the runs of the tiles
    /// there at a scale of one and leaves the rest (1163).
    fn sample(
        &self,
        view: &ExactView,
        tiles: &[Option<Held>],
        rgba: &mut [u8],
        workers: usize,
        fill: Fill,
    ) {
        let (w, _) = view.size();
        let nearest = self.scale == 1.0;
        let cols: Vec<Step> = (0..w).map(|x| self.col(view, x, nearest)).collect();
        // At a scale of one, the runs of columns that read consecutive
        // pixels of one tile column: `(x, n, c, ox)`, `n` columns from `x`
        // reading tile column `c` from its pixel `ox`.
        let mut runs: Vec<(usize, usize, usize, usize)> = Vec::new();
        if fill != Fill::Under && nearest {
            for (x, &((c, ox), _, _)) in cols.iter().enumerate() {
                match runs.last_mut() {
                    Some(run) if run.2 == c && run.3 + run.1 == ox => run.1 += 1,
                    _ => runs.push((x, 1, c, ox)),
                }
            }
        }
        let tile = |r: usize, c: usize| tiles.get(r * self.cols + c).and_then(|t| t.as_ref());
        // Pixel row `o` of every tile of tile row `r`, when all are there.
        let tile_rows = |r: usize, o: usize| -> Option<Vec<&[u8]>> {
            let line = TILE as usize * 4;
            (0..self.cols)
                .map(|c| tile(r, c).and_then(|(rgba, _)| rgba.get(o * line..(o + 1) * line)))
                .collect()
        };
        if fill == Fill::Held && nearest {
            // A pass of its own: in the row below, this branch slowed the
            // bilinear rows by a fifth (1163).
            let line = TILE as usize * 4;
            by_rows(rgba, w, workers, |y, out| {
                let ((r, o), _, _) = self.row(view, y, true);
                for &(x, n, c, ox) in &runs {
                    if let Some((rgba, _)) = tile(r, c) {
                        let src = &rgba[o * line..(o + 1) * line];
                        out[x * 4..(x + n) * 4].copy_from_slice(&src[ox * 4..(ox + n) * 4]);
                    }
                }
            });
            return;
        }
        let whole = fill == Fill::Whole;
        let one_row = |y: u32, out: &mut [u8]| {
            let ((r0, o0), (r1, o1), fy) = self.row(view, y, nearest);
            if whole && nearest {
                if let Some(top) = tile_rows(r0, o0) {
                    for &(x, n, c, ox) in &runs {
                        out[x * 4..(x + n) * 4].copy_from_slice(&top[c][ox * 4..(ox + n) * 4]);
                    }
                    return;
                }
            }
            if whole && !nearest {
                if let (Some(top), Some(bottom)) = (tile_rows(r0, o0), tile_rows(r1, o1)) {
                    bilinear_row(&cols, &top, &bottom, fy, out);
                    return;
                }
            }
            let px = |r: usize, oy: usize, c: usize, ox: usize| {
                tile(r, c).map(|(rgba, _)| {
                    let i = (oy * TILE as usize + ox) * 4;
                    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
                })
            };
            for (x, &((c0, x0), (c1, x1), fx)) in cols.iter().enumerate() {
                let i = x * 4;
                if out[i + 3] != 0 {
                    continue;
                }
                let Some(a) = px(r0, o0, c0, x0) else {
                    continue;
                };
                if nearest {
                    out[i..i + 4].copy_from_slice(&a);
                    continue;
                }
                let b = px(r0, o0, c1, x1).unwrap_or(a);
                let c = px(r1, o1, c0, x0).unwrap_or(a);
                let d = px(r1, o1, c1, x1).unwrap_or(c);
                for k in 0..4 {
                    let top = u32::from(a[k]) * (256 - fx) + u32::from(b[k]) * fx;
                    let bottom = u32::from(c[k]) * (256 - fx) + u32::from(d[k]) * fx;
                    out[i + k] = ((top * (256 - fy) + bottom * fy + (1 << 15)) >> 16) as u8;
                }
            }
        };
        by_rows(rgba, w, workers, one_row);
    }
}

/// One row of a frame between steps with every tile there (1154): each
/// pixel `out` holds the bilinear mix of the four tile pixels its column's
/// [`Step`] and the row's weight `fy` name, `top` and `bottom` the pixel
/// rows of the two tile rows, a slice a tile column. The sums of
/// [`Grid::sample`]'s pixel by pixel path, red and blue in the two 32-bit
/// halves of one `u64` and green and alpha in another: a channel's sum is
/// below 2^24, so the halves never carry into each other.
fn bilinear_row(cols: &[Step], top: &[&[u8]], bottom: &[&[u8]], fy: u32, out: &mut [u8]) {
    const LOW: u64 = 0x0000_00ff_0000_00ff;
    const HALF: u64 = 0x0000_8000_0000_8000;
    // Red and blue, and green and alpha, of the pixel at `x` of `p`.
    let lanes = |p: &[u8], x: usize| {
        let q = &p[x * 4..x * 4 + 4];
        (
            u64::from(q[0]) | (u64::from(q[2]) << 32),
            u64::from(q[1]) | (u64::from(q[3]) << 32),
        )
    };
    let (wy1, wy0) = (u64::from(fy), u64::from(256 - fy));
    for (px, &((c0, x0), (c1, x1), fx)) in out.chunks_exact_mut(4).zip(cols) {
        let (wx1, wx0) = (u64::from(fx), u64::from(256 - fx));
        let mix = |a: u64, b: u64, c: u64, d: u64| {
            let upper = a * wx0 + b * wx1;
            let lower = c * wx0 + d * wx1;
            ((upper * wy0 + lower * wy1 + HALF) >> 16) & LOW
        };
        let (a0, a1) = lanes(top[c0], x0);
        let (b0, b1) = lanes(top[c1], x1);
        let (c0_, c1_) = lanes(bottom[c0], x0);
        let (d0, d1) = lanes(bottom[c1], x1);
        let even = mix(a0, b0, c0_, d0);
        let odd = mix(a1, b1, c1_, d1);
        px.copy_from_slice(&[even as u8, odd as u8, (even >> 32) as u8, (odd >> 32) as u8]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_map::mercator::{project, LatLon};
    use leviculum_map::viewport::MILLI;

    const FLORENCE: &[u8] = include_bytes!("../../leviculum-map/fixtures/firenze.lmr");

    fn maps() -> (tempfile::TempDir, Maps) {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("firenze.lmr"), FLORENCE).expect("write");
        let maps = Maps::new(Some(dir.path().to_path_buf()));
        (dir, maps)
    }

    fn duomo() -> World {
        project(LatLon::from_degrees(43.7731, 11.2560))
    }

    fn frame(view: ExactView, zoom: u8) -> Frame {
        Frame {
            view,
            zoom,
            style: Style::PC,
            scale: 1.0,
            render_limit: None,
            fallback: None,
            moving: false,
            asked: None,
            smooth: true,
            resmooth: true,
        }
    }

    /// A centre on a whole pixel of zoom `z`, so a view drawn whole and
    /// the tiles sample the same pixels.
    fn on_pixel(w: World, z: u8) -> World {
        let shift = 24 - u32::from(z);
        World {
            x: (w.x >> shift) << shift,
            y: (w.y >> shift) << shift,
        }
    }

    /// At a whole step the composed frame is the view drawn whole, pixel
    /// for pixel but where a line's antialiased end meets a seam; the
    /// edges smoothed in both (1422).
    #[test]
    fn a_composed_frame_is_the_view_drawn_whole() {
        let (_dir, maps) = maps();
        let z = 15;
        let centre = on_pixel(duomo(), z);
        let (w, h) = (600, 500);
        let mut cache = TileCache::new(maps.clone(), BUDGET_MB << 20);
        let view = ExactView::new(centre, u32::from(z) * MILLI, w, h);
        let composed = cache.compose(&frame(view, z));
        assert_eq!(composed.missing, 0);
        let mut painter = Painter::new(maps);
        let whole = painter.draw_rgba(
            &Viewport::new(centre, z, w, h),
            Ink::Colour,
            Style::PC,
            100,
            true,
        );
        let mut differ = 0usize;
        let pixels = whole.rgba.len() / 4;
        for (i, px) in whole.rgba.chunks_exact(4).enumerate() {
            let [r, g, b] = [px[0], px[1], px[2]];
            let got = &composed.rgba[i * 4..i * 4 + 3];
            let d = got
                .iter()
                .zip([r, g, b])
                .map(|(&a, b)| a.abs_diff(b))
                .max()
                .unwrap_or(0);
            if d > 8 {
                differ += 1;
            }
        }
        let share = differ as f64 / pixels as f64;
        assert!(share < 0.002, "{differ} pixels of {pixels} differ");
        assert_eq!(composed.attribution, whole.attribution);
    }

    #[test]
    fn a_second_frame_hits_and_a_pan_draws_only_what_it_uncovers() {
        let (_dir, maps) = maps();
        let mut cache = TileCache::new(maps, BUDGET_MB << 20);
        let mut view = ExactView::new(duomo(), 15 * MILLI, 512, 512);
        cache.compose(&frame(view, 15));
        let first = cache.stats;
        assert!(first.misses > 0 && first.hits == 0);
        assert_eq!(first.rendered, first.misses);
        cache.compose(&frame(view, 15));
        assert_eq!(cache.stats.rendered, first.rendered, "nothing drawn again");
        assert_eq!(cache.stats.hits, first.misses);
        view.pan(300, 0);
        cache.compose(&frame(view, 15));
        let drawn = cache.stats.rendered - first.rendered;
        assert!(
            (1..=3).contains(&drawn),
            "a pan of 300 pixels draws one or two new columns of tiles: {drawn}"
        );
    }

    #[test]
    fn the_cache_stays_in_its_budget() {
        let (_dir, maps) = maps();
        let tile = (TILE * TILE * 4) as usize;
        let mut cache = TileCache::new(maps, 6 * tile);
        let mut view = ExactView::new(duomo(), 15 * MILLI, 512, 512);
        for _ in 0..6 {
            cache.compose(&frame(view, 15));
            view.pan(512, 0);
            assert!(cache.bytes() <= 6 * tile, "{} bytes", cache.bytes());
            assert!(cache.len() <= 6);
        }
        assert!(cache.stats.evicted > 0);
    }

    #[test]
    fn a_new_epoch_or_theme_drops_the_tiles() {
        let (dir, maps) = maps();
        let mut cache = TileCache::new(maps.clone(), BUDGET_MB << 20);
        let view = ExactView::new(duomo(), 15 * MILLI, 300, 300);
        cache.compose(&frame(view, 15));
        let held = cache.len();
        assert!(held > 0);
        let dark = Frame {
            style: Style::PC.in_theme(leviculum_mapfill::style::MapTheme::Dark),
            ..frame(view, 15)
        };
        cache.compose(&dark);
        assert_eq!(cache.stats.cleared, 1);
        assert_eq!(cache.len(), held, "only the dark tiles are held");
        std::fs::write(dir.path().join("again.lmr"), FLORENCE).expect("write");
        maps.changed();
        cache.compose(&dark);
        assert_eq!(cache.stats.cleared, 2);
        assert_eq!(cache.stats.rendered, 3 * held as u64);
    }

    /// Pure black, which a frame never shows (1143).
    const BLACK: [u8; 4] = [0, 0, 0, 0xff];

    /// `view` composed at step `zoom` drawing nothing: only what `cache`
    /// holds, and the outline under the rest.
    fn owed(cache: &mut TileCache, view: ExactView, zoom: u8) -> Composed {
        cache.compose(&Frame {
            render_limit: Some(0),
            ..frame(view, zoom)
        })
    }

    /// With nothing cached a frame is the world outline in the theme's
    /// colours, water off the projection, and never black; at a whole
    /// step it is the outline drawn whole, pixel for pixel.
    #[test]
    fn an_empty_cache_shows_the_outline_and_never_black() {
        let (_dir, maps) = maps();
        for theme in [
            leviculum_mapfill::style::MapTheme::Light,
            leviculum_mapfill::style::MapTheme::Dark,
        ] {
            let style = Style::PC.in_theme(theme);
            let mut cache = TileCache::new(maps.clone(), BUDGET_MB << 20);
            let view = ExactView::new(duomo(), 15 * MILLI, 400, 300);
            let blank = cache.compose(&Frame {
                style,
                render_limit: Some(0),
                ..frame(view, 15)
            });
            assert!(blank.missing > 0);
            assert!(!blank.rgba.chunks_exact(4).any(|p| p == BLACK));
            let whole = view.whole().expect("a whole step");
            let mut fb = vec![0u16; 400 * 300];
            let mut target = Fb::new(&mut fb, 400, 300).expect("a frame");
            let Ok(_) =
                outline::draw_world(&outline::WORLD, &whole, Ink::Colour, theme, &mut target);
            let drawn: Vec<u8> = fb.iter().flat_map(|&p| opaque(p)).collect();
            assert!(blank.rgba == drawn, "the outline, {theme:?}");

            // Zoom 1 at 600 x 800: the world is 512 high, the rest water.
            let world = ExactView::new(duomo(), 1_400, 600, 800);
            let wide = cache.compose(&Frame {
                style,
                render_limit: Some(0),
                ..frame(world, 1)
            });
            assert!(!wide.rgba.chunks_exact(4).any(|p| p == BLACK));
            let [r, g, b] = rgb888(outline::water(theme));
            assert_eq!(&wide.rgba[..4], &[r, g, b, 0xff], "water off the world");
            // Drawn whole, the margin off the projection is water too.
            let full = cache.compose(&Frame {
                style,
                ..frame(world, 1)
            });
            assert_eq!(full.missing, 0);
            assert_eq!(&full.rgba[..4], &[r, g, b, 0xff]);
        }
    }

    /// A frame told to draw nothing shows the step before's cached tiles,
    /// scaled, the very pixels that step's frame shows, and says how many
    /// it still owes.
    #[test]
    fn a_frame_not_drawn_yet_shows_the_step_before() {
        let (_dir, maps) = maps();
        let mut cache = TileCache::new(maps, BUDGET_MB << 20);
        let at14 = ExactView::new(duomo(), 14_600, 400, 400);
        let blank = owed(&mut cache, at14, 15);
        let coarse = cache.compose(&frame(at14, 14));
        assert!(blank.missing > 0);
        let shown = owed(&mut cache, at14, 15);
        assert_eq!(shown.missing, blank.missing);
        assert!(shown.rgba == coarse.rgba, "the z14 tiles fill it");
        assert!(shown.rgba != blank.rgba, "not the outline");
    }

    /// A zoom out has the finer steps cached: 2x2 tiles of the next one
    /// or 4x4 of the one after stand in, scaled down, as their own frames
    /// show them; a coarser step held goes before a finer one.
    #[test]
    fn a_zoom_out_is_drawn_from_the_finer_tiles() {
        let (_dir, maps) = maps();
        let view = ExactView::new(duomo(), 15 * MILLI, 300, 300);
        for finer in [16, 17] {
            let mut cache = TileCache::new(maps.clone(), BUDGET_MB << 20);
            let blank = owed(&mut cache, view, 15);
            let fine = cache.compose(&frame(view, finer));
            assert_eq!(fine.missing, 0);
            let shown = owed(&mut cache, view, 15);
            assert!(shown.rgba == fine.rgba, "the z{finer} tiles fill it");
            assert!(shown.rgba != blank.rgba, "not the outline");
            assert!(!shown.rgba.chunks_exact(4).any(|p| p == BLACK));
        }
        let mut cache = TileCache::new(maps, BUDGET_MB << 20);
        cache.compose(&frame(view, 16));
        let coarse = cache.compose(&frame(view, 14));
        let shown = owed(&mut cache, view, 15);
        assert!(shown.rgba == coarse.rgba, "the coarser step first");
    }

    /// A frame at rest that owes no tile keeps the tiles of its own step
    /// and the steps next to it, and drops those of the views the user
    /// left (1797); a moving frame, and one at rest that still owes tiles
    /// and stands in for them, drop nothing.
    #[test]
    fn a_frame_at_rest_keeps_the_steps_next_to_its_own() {
        let (_dir, maps) = maps();
        let view = ExactView::new(duomo(), 14 * MILLI, 300, 300);
        let mut cache = TileCache::new(maps, BUDGET_MB << 20);
        let zooms = |cache: &TileCache| {
            let mut z: Vec<u8> = cache.entries.keys().map(|k| k.zoom).collect();
            z.sort_unstable();
            z.dedup();
            z
        };
        for z in [11, 12, 13, 15, 16] {
            cache.compose(&Frame {
                moving: true,
                ..frame(view, z)
            });
        }
        assert_eq!(zooms(&cache), vec![11, 12, 13, 15, 16], "moving: all kept");
        owed(&mut cache, view, 14);
        assert_eq!(cache.stats.left_behind, 0, "owing tiles: all kept");
        let shown = cache.compose(&frame(view, 14));
        assert_eq!(shown.missing, 0);
        assert_eq!(zooms(&cache), vec![13, 14, 15]);
        assert!(cache.stats.left_behind > 0);
        let held: usize = cache.entries.values().map(|e| e.rgba.len()).sum();
        assert_eq!(cache.bytes, held, "the count follows the tiles");
    }

    /// Part of a frame cached: the tiles held show, the stand ins fill
    /// what they can, the outline the rest, and no pixel is black.
    #[test]
    fn a_partly_cached_frame_has_no_black_holes() {
        let (_dir, maps) = maps();
        let mut cache = TileCache::new(maps, BUDGET_MB << 20);
        let mut view = ExactView::new(duomo(), 15_300, 500, 400);
        cache.compose(&frame(view, 15));
        view.pan(700, 300);
        let shown = owed(&mut cache, view, 15);
        assert!(shown.missing > 0);
        assert!(!shown.rgba.chunks_exact(4).any(|p| p == BLACK));
        assert!(shown.rgba.chunks_exact(4).all(|p| p[3] == 0xff));
    }

    /// The pixels of `view`'s tiles at `zoom` not held in `cache`.
    fn not_held(cache: &TileCache, view: &ExactView, zoom: u8) -> Vec<Rect> {
        let grid = Grid::of(view, zoom);
        let spans = grid.spans(view);
        let keys = grid.keys(Style::PC, 100);
        (0..keys.len())
            .filter(|&i| keys[i].is_some_and(|k| !cache.holds(&k)))
            .filter_map(|i| grid.rect(&spans, i))
            .collect()
    }

    /// Whether `rgba`'s rows, `w` wide, come in pairs of equal rows from
    /// row 0 or from row 1: a step's pixels doubled.
    fn rows_doubled(rgba: &[u8], w: usize) -> bool {
        let rows: Vec<&[u8]> = rgba.chunks_exact(w * 4).collect();
        (0..2).any(|o| {
            (o..rows.len() - 1)
                .step_by(2)
                .all(|y| rows[y] == rows[y + 1])
        })
    }

    /// The columns of `rgba` likewise.
    fn cols_doubled(rgba: &[u8], w: usize) -> bool {
        let rows: Vec<&[u8]> = rgba.chunks_exact(w * 4).collect();
        (0..2).any(|o| {
            (o..w - 1).step_by(2).all(|x| {
                rows.iter()
                    .all(|r| r[x * 4..x * 4 + 4] == r[x * 4 + 4..x * 4 + 8])
            })
        })
    }

    /// 1163: while the view moves a frame owing tiles shows the tiles held
    /// as a frame drawn whole does, and over the owed tiles' pixels alone
    /// the step before, its pixels doubled (nearest, not bilinear as at
    /// rest), the very pixels a moving frame of only that step shows.
    #[test]
    fn a_moving_frame_stands_in_only_over_the_owed_tiles() {
        let (_dir, maps) = maps();
        let mut view = ExactView::new(on_pixel(duomo(), 15), 15 * MILLI, 500, 400);
        let moving = |view: ExactView| Frame {
            moving: true,
            render_limit: Some(0),
            ..frame(view, 15)
        };
        // Held: z14 over the frame, z15 for the view before a pan.
        let mut cache = TileCache::new(maps.clone(), BUDGET_MB << 20);
        cache.compose(&frame(view, 15));
        view.pan(300, 200);
        cache.compose(&frame(view, 14));
        let owed = not_held(&cache, &view, 15);
        assert!(!owed.is_empty());
        let shown = cache.compose(&moving(view));
        assert_eq!(shown.missing, owed.len());
        assert!(shown.rgba.chunks_exact(4).all(|p| p[3] == 0xff));
        // Every tile drawn: the pixels of the tiles held.
        let full = TileCache::new(maps.clone(), BUDGET_MB << 20).compose(&frame(view, 15));
        // Only z14 held: what stands in.
        let mut only = TileCache::new(maps.clone(), BUDGET_MB << 20);
        only.compose(&frame(view, 14));
        let stood = only.compose(&moving(view));
        assert!(rows_doubled(&stood.rgba, 500) && cols_doubled(&stood.rgba, 500));
        let at_rest = only.compose(&Frame {
            render_limit: Some(0),
            ..frame(view, 15)
        });
        assert!(
            at_rest.rgba != stood.rgba,
            "at rest the step is scaled bilinear"
        );
        let mut stood_in = 0;
        for (i, px) in shown.rgba.chunks_exact(4).enumerate() {
            let (x, y) = ((i % 500) as u32, (i / 500) as u32);
            let in_owed = owed
                .iter()
                .any(|r| r.has_row(y) && (r.x0..r.x1).contains(&x));
            let want = match in_owed {
                true => &stood.rgba[i * 4..i * 4 + 4],
                false => &full.rgba[i * 4..i * 4 + 4],
            };
            assert_eq!(px, want, "pixel {x},{y}, owed {in_owed}");
            stood_in += usize::from(in_owed);
        }
        assert!(stood_in > 0 && stood_in < 500 * 400);
    }

    /// 1163: a tile wholly outside the pixels a frontend shows is neither
    /// drawn nor owed, its pixels water; one held there is used.
    #[test]
    fn tiles_outside_the_asked_pixels_are_neither_drawn_nor_owed() {
        let (_dir, maps) = maps();
        let view = ExactView::new(on_pixel(duomo(), 15), 15 * MILLI, 900, 900);
        let asked = Frame {
            moving: true,
            asked: Some([400, 400, 500, 500]),
            smooth: true,
            resmooth: true,
            ..frame(view, 15)
        };
        let grid = Grid::of(&view, 15);
        let spans = grid.spans(&view);
        let inside = (0..grid.cols * grid.rows)
            .filter_map(|i| grid.rect(&spans, i))
            .filter(|r| r.x0 < 500 && 400 < r.x1 && r.y0 < 500 && 400 < r.y1)
            .count();
        assert!(inside < grid.cols * grid.rows);
        let mut cache = TileCache::new(maps.clone(), BUDGET_MB << 20);
        let owed = cache.compose(&Frame {
            render_limit: Some(0),
            ..asked
        });
        assert_eq!(owed.missing, inside, "only the tiles asked are owed");
        let shown = cache.compose(&asked);
        assert_eq!(shown.missing, 0);
        assert_eq!(cache.stats.rendered, inside as u64, "only those drawn");
        let [r, g, b] = rgb888(outline::water(Style::PC.theme));
        assert_eq!(&shown.rgba[..4], &[r, g, b, 0xff], "water outside");
        let full = TileCache::new(maps.clone(), BUDGET_MB << 20).compose(&frame(view, 15));
        let mid = (450 * 900 + 450) * 4;
        assert_eq!(&shown.rgba[mid..mid + 4], &full.rgba[mid..mid + 4]);
        // Every tile held: the frame drawn whole.
        let mut held = TileCache::new(maps, BUDGET_MB << 20);
        held.compose(&frame(view, 15));
        let shown = held.compose(&asked);
        assert!(shown.rgba == full.rgba, "the tiles held outside are used");
    }

    /// The steps a frame not drawn takes its stand ins from, in order.
    #[test]
    fn the_stand_ins_are_coarser_first_then_finer() {
        let view = ExactView::new(duomo(), 15 * MILLI, 300, 300);
        assert_eq!(stand_ins(&frame(view, 15)), [14, 13, 12, 16, 17]);
        let far = Frame {
            fallback: Some(19),
            ..frame(view, 15)
        };
        assert_eq!(stand_ins(&far), [14, 13, 12, 16, 17, 19]);
        let near = Frame {
            fallback: Some(14),
            ..frame(view, 1)
        };
        assert_eq!(stand_ins(&near), [0, 2, 3, 14]);
        assert_eq!(stand_ins(&frame(view, 20)), [19, 18, 17]);
    }

    /// The world pixel of `key`'s centre at its zoom, against the view's
    /// centre there, in screen pixels.
    fn offset(view: &ExactView, key: &TileKey) -> (f64, f64) {
        let unit = f64::from(1u32 << (24 - u32::from(key.zoom)));
        let s = view.scale_at(key.zoom);
        let c =
            |t: u32, at: u32| ((f64::from(t) + 0.5) * f64::from(TILE) - f64::from(at) / unit) * s;
        (c(key.x, view.centre.x), c(key.y, view.centre.y))
    }

    fn ring_one(plan: &Plan) -> Vec<TileKey> {
        let z = plan.wants[0].key.zoom;
        plan.wants
            .iter()
            .take_while(|w| w.key.zoom == z && !w.spare_only)
            .map(|w| w.key)
            .collect()
    }

    /// A pan north puts the tiles north of the view first, east the east
    /// ones; no heading puts the nearest first.
    #[test]
    fn the_tiles_ahead_of_the_last_pan_come_first() {
        let view = ExactView::new(duomo(), 15 * MILLI, 600, 900);
        let f = frame(view, 15);
        let mut north = Motion::default();
        for i in 0..10 {
            north.pan(0.0, -40.0, 1000 + i * 16);
        }
        let heading = north.heading(1200).expect("a heading");
        assert!(heading.1 < -500.0 && heading.0 == 0.0, "{heading:?}");
        let ring = ring_one(&plan(&f, Some(heading), false, (0, 20)));
        let top = ring.iter().map(|k| k.y).min().expect("a ring");
        let row = ring.iter().filter(|k| k.y == top).count();
        assert!(row >= 4);
        assert!(
            ring[..row].iter().all(|k| k.y == top),
            "the northern row first: {:?}",
            &ring[..row]
        );
        assert!(ring[ring.len() - row..].iter().all(|k| k.y > top + 2));

        let mut east = Motion::default();
        for i in 0..10 {
            east.pan(40.0, 0.0, 1000 + i * 16);
        }
        let ring = ring_one(&plan(&f, east.heading(1200), false, (0, 20)));
        let right = ring.iter().map(|k| k.x).max().expect("a ring");
        assert!(ring[0].x == right && ring[1].x == right, "{:?}", &ring[..3]);

        let ring = ring_one(&plan(&f, None, false, (0, 20)));
        let near: Vec<f64> = ring
            .iter()
            .map(|k| {
                let (x, y) = offset(&view, k);
                x.hypot(y)
            })
            .collect();
        assert!(
            near.windows(2).all(|w| w[0] <= w[1] + 1e-6),
            "nearest first: {near:?}"
        );
    }

    /// The plan's groups: the step out over the whole frame first and only
    /// after a pinch out, the ring at the view's step, the step in over the
    /// centre, the second ring last and into spare room only.
    #[test]
    fn a_plan_is_the_ring_the_step_in_and_a_second_ring() {
        let view = ExactView::new(duomo(), 15 * MILLI, 600, 900);
        let f = frame(view, 15);
        let zooms = |p: &Plan| {
            let mut z: Vec<(u8, bool)> =
                p.wants.iter().map(|w| (w.key.zoom, w.spare_only)).collect();
            z.dedup();
            z
        };
        let p = plan(&f, None, false, (0, 20));
        assert_eq!(zooms(&p), [(15, false), (16, false), (15, true)]);
        let p = plan(&f, None, true, (0, 20));
        assert_eq!(
            zooms(&p),
            [(14, false), (15, false), (16, false), (15, true)],
            "the step out first after a pinch out"
        );
        let out: Vec<TileKey> = Grid::of(&view, 14)
            .keys(Style::PC, 100)
            .into_iter()
            .flatten()
            .collect();
        assert!(
            out.iter()
                .all(|k| p.wants[..out.len()].iter().any(|w| w.key == *k)),
            "the whole frame's step out"
        );
        let p = plan(&f, None, true, (15, 15));
        assert_eq!(zooms(&p), [(15, false), (15, true)], "the regions' zooms");
        let grid = Grid::of(&view, 15);
        let held: Vec<TileKey> = grid.keys(Style::PC, 100).into_iter().flatten().collect();
        assert!(
            p.wants.iter().all(|w| !held.contains(&w.key)),
            "none on screen"
        );
    }

    /// The heading a prefetch plans by is the one when the first frame
    /// since the last pan was asked; a later ask moves it no further, a
    /// pan lets it go (1189).
    #[test]
    fn the_rest_heading_freezes_at_the_first_ask() {
        let mut m = Motion::default();
        for i in 0..20 {
            m.pan(0.0, -80.0, i * 16);
        }
        assert!(m.rest_heading(320 + 3000).is_none(), "no ask, no freeze");
        m.asked(320);
        m.asked(2000);
        assert_eq!(m.rest_heading(320 + 3000), m.heading(320));
        assert!(m.rest_heading(320 + 3000).is_some());
        m.pan(0.0, 0.0, 3320);
        assert_eq!(m.rest_heading(3320 + 3000), None);
    }

    /// The velocity decays at rest: no direction a few seconds after the
    /// finger stopped.
    #[test]
    fn a_heading_fades_at_rest() {
        let mut m = Motion::default();
        assert_eq!(m.heading(0), None);
        for i in 0..20 {
            m.pan(0.0, -80.0, i * 16);
        }
        assert!(m.heading(320).is_some());
        assert!(m.heading(320 + 1000).is_some());
        assert_eq!(m.heading(320 + 3000), None, "{:?}", m.velocity(3320));
        m.zoom(-300);
        assert!(m.zoomed_out);
        m.zoom(0);
        assert!(m.zoomed_out);
        m.zoom(200);
        assert!(!m.zoomed_out);
    }

    #[test]
    fn a_prefetch_stays_in_its_budget_a_batch_at_a_time() {
        let (_dir, maps) = maps();
        let mut cache = TileCache::new(maps, BUDGET_MB << 20);
        let view = ExactView::new(duomo(), 15 * MILLI, 512, 512);
        let f = frame(view, 15);
        cache.compose(&f);
        let mut p = plan(&f, None, false, (0, 20));
        p.budget = 5;
        assert_eq!(cache.prefetch(&mut p, 2), 2);
        assert_eq!(cache.prefetch(&mut p, 2), 2);
        assert_eq!(cache.prefetch(&mut p, 2), 1);
        assert!(p.done());
        assert_eq!(cache.prefetch(&mut p, 2), 0);
        assert_eq!(cache.stats.prefetched, 5);
        assert_eq!(cache.stats.rendered, cache.stats.misses, "a frame's own");
        assert!(p.wants[..5].iter().all(|w| cache.holds(&w.key)));
    }

    /// A prefetch never evicts a tile the frame shows, nor one it drew
    /// itself; a frame evicts the prefetched tiles no frame used first,
    /// and a prefetched tile a frame uses counts as a hit.
    #[test]
    fn prefetched_tiles_go_first_and_never_push_out_the_view() {
        let (_dir, maps) = maps();
        let view = ExactView::new(duomo(), 15 * MILLI, 512, 512);
        let f = frame(view, 15);
        let shown: Vec<TileKey> = Grid::of(&view, 15)
            .keys(Style::PC, 100)
            .into_iter()
            .flatten()
            .collect();
        let mut cache = TileCache::new(maps, (shown.len() + 3) * TILE_BYTES);
        cache.compose(&f);
        let mut p = plan(&f, None, false, (0, 20));
        while !p.done() {
            cache.prefetch(&mut p, 4);
        }
        assert_eq!(cache.stats.prefetched, 3, "only the room left");
        assert_eq!(cache.stats.evicted, 0);
        assert!(shown.iter().all(|k| cache.holds(k)));

        // A pan of one tile: the frame draws a column, evicting the
        // prefetched tiles it does not use before any it showed.
        let mut moved = view;
        moved.pan(TILE as i32, 0);
        cache.compose(&frame(moved, 15));
        let ahead: Vec<TileKey> = p.wants[..3].iter().map(|w| w.key).collect();
        let now: Vec<TileKey> = Grid::of(&moved, 15)
            .keys(Style::PC, 100)
            .into_iter()
            .flatten()
            .collect();
        for k in &ahead {
            assert!(now.contains(k) || !cache.holds(k), "{k:?} unused but held");
        }
        let used = ahead.iter().filter(|k| now.contains(k)).count() as u64;
        assert_eq!(cache.stats.prefetch_hits, used);
        assert!(used < 3, "an unused prefetched tile to evict");
        assert!(now.iter().all(|k| cache.holds(k)), "the frame's tiles held");
    }

    /// The second ring draws into spare room only: in a full cache it
    /// stops the plan rather than evict.
    #[test]
    fn the_second_ring_takes_spare_room_only() {
        let (_dir, maps) = maps();
        let view = ExactView::new(duomo(), 15 * MILLI, 300, 300);
        let f = frame(view, 15);
        let mut cache = TileCache::new(maps, BUDGET_MB << 20);
        cache.compose(&f);
        let mut p = plan(&f, None, false, (15, 15));
        let first = p.wants.iter().filter(|w| !w.spare_only).count();
        let room = cache.len() + first + 2;
        cache.budget = room * TILE_BYTES;
        p.budget = usize::MAX;
        while !p.done() {
            cache.prefetch(&mut p, 3);
        }
        assert_eq!(cache.stats.prefetched as usize, first + 2);
        assert_eq!(cache.stats.evicted, 0);
        assert!(cache.bytes() <= room * TILE_BYTES);
    }

    /// 1422: a tile drawn straight into RGBA is the Rgb565 picture of the
    /// same view, every pixel [`opaque`]'s bytes for it, at three zooms
    /// and both themes, the buildings' fill and edge included.
    #[test]
    fn a_tile_drawn_in_rgba_is_the_rgb565_tile_widened() {
        let dir = tempfile::tempdir().expect("tmp");
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../leviculum-map/fixtures/buildings/firenze.lmr");
        std::fs::copy(fixture, dir.path().join("firenze.lmr")).expect("the region");
        let mut painter = Painter::new(Maps::new(Some(dir.path().to_path_buf())));
        for z in [13u8, 15, 17] {
            for style in [
                Style::PC,
                Style {
                    theme: MapTheme::Dark,
                    ..Style::PC
                },
            ] {
                let view = Viewport::new(duomo(), z, TILE, TILE);
                let fb = painter.draw(&view, Ink::Colour, style, 100).fb;
                let want: Vec<u8> = fb.iter().flat_map(|&p| opaque(p)).collect();
                let got = painter
                    .draw_rgba(&view, Ink::Colour, style, 100, false)
                    .rgba;
                assert!(got == want, "z{z} {:?}: a pixel differs", style.theme);
            }
        }
    }

    /// A painter that kept its filler's storage from tile to tile, after a
    /// taller view too, draws the bytes a fresh painter draws for each
    /// (1155): what an earlier fill left is never read.
    #[test]
    fn a_painter_that_kept_its_scratch_draws_as_a_fresh_one() {
        let (_dir, maps) = maps();
        let mut kept = Painter::new(maps.clone());
        let tall = Viewport::new(duomo(), 15, 300, 700);
        let _ = kept.draw(&tall, Ink::Colour, Style::PC, 100);
        let mut keys = Vec::new();
        for z in [13u8, 15, 16] {
            let view = ExactView::new(duomo(), u32::from(z) * MILLI, 700, 700);
            let grid = Grid::of(&view, z);
            keys.extend(grid.keys(Style::PC, 100).into_iter().flatten());
        }
        assert!(keys.len() > 20, "{} tiles", keys.len());
        for key in &keys {
            let again = draw_tile(&mut kept, key, true);
            let fresh = draw_tile(&mut Painter::new(maps.clone()), key, true);
            assert!(again == fresh, "tile {key:?} differs");
        }
    }
}
