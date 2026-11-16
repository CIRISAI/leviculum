//! The map view as a model (`docs/src/concepts/loquax-map.md`, section 1,
//! "The map view", and slice (c)): which pins there are, in which order
//! `Tab` steps them, where the view is and where each pin lands in cells.
//!
//! The model knows no pixels of its own. The shell tells it how many image
//! pixels one cell carries for the backend it drew with, and which zooms
//! its installed regions reach ([`MapSetup`]); from those and the pane's
//! cells the model makes the [`Viewport`] the shell draws, and places the
//! pins in the cells the reader clicks. So a click and the frame cannot
//! disagree, and a test of the model needs no terminal.
//!
//! A pin is a peer with a known position, our own among them when one is
//! known, named by the display rule ([`crate::names::Names::label`]) with
//! the fix's age beside it, dimmed once it is stale
//! ([`crate::telemetry::STALE_AFTER_SECS`]).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use leviculum_map::blend::{world_floor_milli, Shape};
use leviculum_map::choice::fit_ceiling;
use leviculum_map::labels::{self as names, Candidate, LabelKind, ViewLabel};
use leviculum_map::mercator::{project, unproject, LatLon, World};
use leviculum_map::pins::{self, Mark, Metrics, Pin, Side};
use leviculum_map::region::{Bounds, Header};
use leviculum_map::viewport::{Viewport, MILLI};
use leviculum_ui::Rect;

use crate::telemetry;

/// What the shell drew with: image pixels per cell, and the zooms the
/// installed regions and the outline reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapSetup {
    /// Pixels across and down one cell: (1, 2) for half-blocks, (2, 4) for
    /// braille, the font's cell for a graphics protocol.
    pub cell_px: (u16, u16),
    /// The shallowest and the deepest zoom the reader may go to.
    pub zooms: (u8, u8),
    /// The installed regions' headers, which say how deep a fit goes
    /// ([`fit`]).
    pub regions: Vec<Header>,
    /// The ways this shell can draw the map, in the order a key cycles
    /// through them (1093); none where there is no choice.
    pub drawings: Vec<Drawing>,
    /// Which of [`MapSetup::drawings`] draws the map now.
    pub drawing: usize,
}

/// One way the shell can draw the map, as a key and the palette offer it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drawing {
    /// Its name, as `--graphics` and the map's title spell it.
    pub name: String,
    /// Whether the shell expects it to work here. One it does not is still
    /// offered, marked: the reader's eyes beat the detection.
    pub expected: bool,
}

impl Default for MapSetup {
    /// Braille's cell over the outline's zooms: what a shell that said
    /// nothing gets.
    fn default() -> Self {
        Self {
            cell_px: (2, 4),
            zooms: (0, leviculum_map::outline::MAX_ZOOM),
            regions: Vec::new(),
            drawings: Vec::new(),
            drawing: 0,
        }
    }
}

/// Why a region is loaded (1089): the reader went to a place, or asked for
/// the visible area; or the scheduler asked for the screen itself at its
/// zoom, which loads before anything else (1527, [`super::viewload`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadReason {
    Place,
    View,
    Screen,
}

impl LoadReason {
    /// `tui.log`'s `reason=`.
    pub fn word(self) -> &'static str {
        match self {
            Self::Place => "place",
            Self::View => "view",
            Self::Screen => "screen",
        }
    }
}

/// A region the model asks the shell to load: a box, the zooms, and how
/// far the deepest zoom may be lowered to stay under the shell's size cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapWant {
    pub bounds: Bounds,
    /// The shallowest and the deepest zoom asked for.
    pub zooms: (u8, u8),
    /// The shallowest the deepest zoom may be lowered to.
    pub floor: u8,
    pub reason: LoadReason,
    /// The place, or the view's centre: a place's load fits the map there
    /// again when it lands and the map has not moved.
    pub centre: World,
}

/// A place's region reaches this far to each side: a box of 12 by 12 km,
/// a town with its surroundings. Scaled from Frankfurt's 576 km² box of
/// 9.9 MB (loquax-map.md, section 4), a city's is 2 to 3 MB.
pub const PLACE_HALF_M: u32 = 6_000;
/// A place counts as loaded when a region holds this much around it at
/// the deepest zoom: no load for a fix a street away from the last.
pub const PLACE_MARGIN_M: u32 = 3_000;
/// A place's zooms. Below z10 its box is a few pixels, and the world
/// outline draws there anyway.
pub const PLACE_ZOOMS: (u8, u8) = (10, 15);
/// The deepest zoom a place's load lowers to under the size cap.
pub const PLACE_FLOOR: u8 = 13;
/// The deepest zoom any load asks for: the Protomaps builds end there.
pub const DEEPEST_LOAD: u8 = 15;
/// A load of the visible area starts this many zooms above the view's,
/// so zooming out a little still finds the region.
pub const VIEW_BELOW: u8 = 3;
/// The most places one opening of the map asks for, its first pins in
/// `Tab` order: a list of peers across a continent is not a download of it.
pub const MAX_PIN_LOADS: usize = 4;

/// Whether a region of `regions` holds `b` at `zooms`.
pub fn covered(regions: &[Header], b: &Bounds, zooms: (u8, u8)) -> bool {
    regions
        .iter()
        .any(|h| h.bounds.holds(b) && h.min_zoom <= zooms.0 && h.max_zoom >= zooms.1)
}

/// The region to load around `at`, unless `regions` hold it already
/// ([`PLACE_MARGIN_M`] around it at the deepest zoom).
pub fn want_place(at: World, regions: &[Header]) -> Option<MapWant> {
    let place = unproject(at);
    let inner = Bounds::around(place, PLACE_MARGIN_M);
    if covered(regions, &inner, (PLACE_ZOOMS.1, PLACE_ZOOMS.1)) {
        return None;
    }
    Some(MapWant {
        bounds: Bounds::around(place, PLACE_HALF_M),
        zooms: PLACE_ZOOMS,
        floor: PLACE_FLOOR,
        reason: LoadReason::Place,
        centre: at,
    })
}

/// The places to load for `pins`, at most [`MAX_PIN_LOADS`], none held by
/// `regions` or by a place asked for before it.
pub fn want_pins(pins: &[MapPin], regions: &[Header]) -> Vec<MapWant> {
    let mut wants: Vec<MapWant> = Vec::new();
    for pin in pins {
        if wants.len() >= MAX_PIN_LOADS {
            break;
        }
        let inner = Bounds::around(unproject(pin.at), PLACE_MARGIN_M);
        if wants.iter().any(|w| w.bounds.holds(&inner)) {
            continue;
        }
        wants.extend(want_place(pin.at, regions));
    }
    wants
}

/// The region to load for `view`: its box, from [`VIEW_BELOW`] zooms above
/// its zoom to [`DEEPEST_LOAD`], the deepest lowered no further than the
/// view's own. `None` when `regions` hold the box at the view's zoom.
pub fn want_view(view: &Viewport, regions: &[Header]) -> Option<MapWant> {
    let zoom = view.zoom().min(DEEPEST_LOAD);
    let bounds = view.bounds();
    if covered(regions, &bounds, (zoom, zoom)) {
        return None;
    }
    Some(MapWant {
        bounds,
        zooms: (zoom.saturating_sub(VIEW_BELOW), DEEPEST_LOAD),
        floor: zoom,
        reason: LoadReason::View,
        centre: view.centre,
    })
}

/// The open map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapState {
    pub centre: World,
    pub zoom: u8,
    /// The selected pin's peer, which `Enter` opens.
    pub selected: Option<[u8; 16]>,
}

/// One pin, in `Tab` order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapPin {
    pub peer: [u8; 16],
    pub at: World,
    /// The display rule's name, `you` for our own position.
    pub label: String,
    /// How old the fix is, when the shell has said the time.
    pub age: Option<String>,
    pub stale: bool,
    /// A node the mesh announced that is no contact (1302): a neutral pin,
    /// grey, with its announced name and no avatar.
    pub nearby: bool,
    /// Our own position (1386): what the radar centres on.
    pub own: bool,
    /// The fix time, unix seconds: the newest peer is what the radar
    /// centres on without an own position.
    pub fix_at: Option<i64>,
    /// The battery's charge in percent, when the fix's Telemeter carried a
    /// battery sensor (1386): below [`LOW_BATTERY`] the pin gets a glyph.
    pub battery: Option<u8>,
}

/// A pin whose battery is below this many percent gets the battery glyph
/// (1386).
pub const LOW_BATTERY: u8 = 20;

/// The low battery's glyph after a pin's name in the terminal, a battery
/// on its side's outline; `!` in ASCII. A pixel screen draws a battery
/// ([`crate::screenmap::draw_pins`]).
pub const BATTERY_GLYPH: char = '▯';

impl MapPin {
    /// Whether its battery is low ([`LOW_BATTERY`]).
    pub fn low_battery(&self) -> bool {
        self.battery.is_some_and(|b| b < LOW_BATTERY)
    }

    /// What is set beside the marker: the name, and the fix's age when
    /// the shell has said the time.
    pub fn words(&self) -> String {
        match &self.age {
            Some(age) => format!("{} · {age}", self.label),
            None => self.label.clone(),
        }
    }
}

/// A pin that lands in the picture, in screen cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinCell {
    pub peer: [u8; 16],
    /// Where the text starts: the marker's cell, or left of it when the
    /// name is set before the marker.
    pub col: u16,
    pub row: u16,
    /// The marker, with the name and the age beside it when they are set
    /// on the marker's row, as drawn from `col` on.
    pub text: String,
    /// The name and the age on the row above or below the marker, where
    /// neither side of it had room.
    pub apart: Option<LabelCell>,
    pub stale: bool,
    /// [`MapPin::nearby`].
    pub nearby: bool,
    pub selected: bool,
    /// [`MapPin::own`]: the reader's own position, the blue dot alone as
    /// the maps people know draw it, never a name or an avatar (1449 item 2,
    /// walk 1 finding 8).
    pub own: bool,
}

/// A pin's name set on a row of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelCell {
    pub col: u16,
    pub row: u16,
    pub text: String,
}

/// A name of the map set in cells: a place, a water, a park, a river, a
/// street.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameCell {
    pub col: u16,
    pub row: u16,
    pub text: String,
    pub kind: LabelKind,
}

/// The terminal's density rule (`docs/drafts/map-renderer-roadmap-2026-10.md`,
/// section 1, "Placement"): at most one name a block of this many cells.
pub const NAME_BLOCK: (u16, u16) = (20, 6);

/// How far apart, in cells, two names of one street or river stand.
pub const NAME_REPEAT: u16 = 60;

/// The horizontal runs of `line` (sub-pixels) in cells of `cw` by `ch`
/// sub-pixels, in the order the line passes them: (row, first col, last
/// col) of each stretch the line stays in one row, walked at a quarter of
/// a cell, so a street that climbs a row every few cells gives runs that
/// short.
pub fn cell_runs(line: &[(i64, i64)], cw: i64, ch: i64) -> Vec<(i64, i64, i64)> {
    let mut runs: Vec<(i64, i64, i64)> = Vec::new();
    let mut add = |col: i64, row: i64| match runs.last_mut() {
        Some(r) if r.0 == row => {
            r.1 = r.1.min(col);
            r.2 = r.2.max(col);
        }
        _ => runs.push((row, col, col)),
    };
    let cell = |(x, y): (i64, i64)| (x.div_euclid(cw), y.div_euclid(ch));
    if let Some(&p) = line.first() {
        let (c, r) = cell(p);
        add(c, r);
    }
    for w in line.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        let steps = ((x1 - x0).abs() * 4 / cw).max((y1 - y0).abs() * 4 / ch) + 1;
        for i in 1..=steps {
            let (c, r) = cell((x0 + (x1 - x0) * i / steps, y0 + (y1 - y0) * i / steps));
            add(c, r);
        }
    }
    runs
}

/// Where the names of `labels` go in `area`, `cell_px` image pixels a cell,
/// by the map core's one rule ([`names::place`]) in cells: one name per
/// [`NAME_BLOCK`], one free cell around each, never over `taken` (the
/// pins' markers and names, placed first), never cut: a name that does not
/// fit the area whole is left out. Places, waters and parks are centred on
/// their anchor. A street's name is set on a horizontal run of its line at
/// least two cells longer than the name ([`cell_runs`]), centred on it,
/// and repeats [`NAME_REPEAT`] cells apart; cells cannot turn text, so a
/// street that never runs level is not named. A river's name goes where
/// its line crosses the middle row of a block row, or on a run as a
/// street's. Each `taken` is (col, row, width) of a one-row run, relative
/// to `area`; so is each name returned to the area's own origin added.
pub fn place_names(
    labels: &[ViewLabel],
    area: Rect,
    cell_px: (u16, u16),
    taken: &[(u16, u16, u16)],
) -> Vec<NameCell> {
    let (cw, ch) = (i64::from(cell_px.0.max(1)), i64::from(cell_px.1.max(1)));
    let sub = names::SUBPIXEL;
    let (w, h) = (i64::from(area.width), i64::from(area.height));
    let band = i64::from(NAME_BLOCK.1);
    let mut candidates: Vec<Candidate> = Vec::new();
    for l in labels {
        let len = l.text.chars().count() as i64;
        if len == 0 {
            continue;
        }
        // (col, row) of each place the name may start at, best first.
        let mut starts: Vec<(i64, i64)> = Vec::new();
        match l.kind.is_line() {
            false => {
                let col = l.anchor.0.div_euclid(sub * cw);
                let row = l.anchor.1.div_euclid(sub * ch);
                starts.push((col - len / 2, row));
            }
            true => {
                let mut runs: Vec<(i64, i64, i64)> = l
                    .line
                    .iter()
                    .flat_map(|piece| cell_runs(piece, sub * cw, sub * ch))
                    .filter(|&(row, c0, c1)| {
                        let level = c1 - c0 + 1 >= len + 2;
                        let crossing =
                            l.kind == LabelKind::River && row.rem_euclid(band) == band / 2;
                        level || crossing
                    })
                    .collect();
                // The longest first, so a street's own best run is tried
                // before its others.
                runs.sort_by_key(|&(row, c0, c1)| (core::cmp::Reverse(c1 - c0), row, c0));
                starts.extend(
                    runs.iter()
                        .map(|&(row, c0, c1)| ((c0 + c1 + 1) / 2 - len / 2, row)),
                );
            }
        }
        for (x0, y0) in starts {
            let whole = x0 >= 0 && x0 + len <= w && (0..h).contains(&y0);
            let (Ok(x0), Ok(y0), Ok(n)) =
                (i32::try_from(x0), i32::try_from(y0), i32::try_from(len))
            else {
                continue;
            };
            if whole {
                candidates.push(Candidate {
                    kind: l.kind,
                    rank: l.rank,
                    text: l.text.clone(),
                    at: l.at,
                    boxes: alloc::vec![names::Rect {
                        x0,
                        y0,
                        x1: x0 + n,
                        y1: y0 + 1
                    }],
                });
            }
        }
    }
    let taken: Vec<names::Rect> = taken
        .iter()
        .map(|&(col, row, width)| names::Rect {
            x0: i32::from(col),
            y0: i32::from(row),
            x1: i32::from(col) + i32::from(width),
            y1: i32::from(row) + 1,
        })
        .collect();
    let rule = names::Rule {
        margin: 1,
        block: (i32::from(NAME_BLOCK.0), i32::from(NAME_BLOCK.1)),
        repeat: i32::from(NAME_REPEAT),
    };
    names::place(&candidates, &taken, rule)
        .into_iter()
        .filter_map(|i| {
            let c = candidates.get(i)?;
            let b = c.boxes.first()?;
            Some(NameCell {
                col: area.x + u16::try_from(b.x0).ok()?,
                row: area.y + u16::try_from(b.y0).ok()?,
                text: c.text.clone(),
                kind: c.kind,
            })
        })
        .collect()
}

/// The map as drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapView {
    /// The cells the picture covers.
    pub area: Rect,
    /// The view the picture shows, `area` times the cell's pixels.
    pub viewport: Viewport,
    pub cell_px: (u16, u16),
    /// The pins in view, the selected one last so it is drawn on top.
    pub pins: Vec<PinCell>,
    /// The tracks' cells under the pins (1385).
    pub tracks: Vec<crate::ui::tracks::cells::TrackCell>,
    /// The radar where no region touches the view (1386): the picture is
    /// left blank and these drawn instead.
    pub radar: Option<RadarCells>,
    /// The tracks' time window's word when it is not the default (1419):
    /// on the credit's row, so the reader knows why older lines show.
    pub window_word: Option<String>,
    /// A pointer drags the view (1422): the frame budget's gesture, from
    /// the button going down past a drag to it coming up.
    pub moving: bool,
}

/// The radar in the terminal's cells (1386).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadarCells {
    /// [`crate::ui::radar::Radar::note`], on the credit's row.
    pub note: String,
    /// The rings, their distances and the centre
    /// ([`crate::ui::radar::cells`]).
    pub cells: Vec<crate::ui::tracks::cells::TrackCell>,
}

/// Pixels the fit leaves free at each side, so a pin is not on the edge.
const FIT_MARGIN_CELLS: u32 = 2;

/// The marker before a pin's name; `*` in ASCII.
pub const MARKER: char = '●';

/// The selected pin's marker, which `Enter` opens; `@` in ASCII. A marker
/// of its own and not only reverse video, so braille and a monochrome
/// screen show it, and a text frame does too.
pub const SELECTED: char = '◉';

/// The pins of `positions` (peer to fix) in `Tab` order, `own` named `you`,
/// each fix's age in the catalogue's words in `lang` ("vor 28 Std.", 1447):
/// the one age a position has, the card's.
pub fn pins_of(
    positions: &alloc::collections::BTreeMap<[u8; 16], telemetry::Fix>,
    label: impl Fn(&[u8; 16]) -> String,
    nearby: impl Fn(&[u8; 16]) -> bool,
    own: Option<[u8; 16]>,
    now: Option<i64>,
    lang: super::strings::Lang,
) -> Vec<MapPin> {
    let all: Vec<MapPin> = positions
        .iter()
        .map(|(peer, fix)| {
            let at = project(LatLon::from_e6(
                fix.location.latitude_e6,
                fix.location.longitude_e6,
            ));
            let age = now
                .filter(|_| !fix.ahead())
                .map(|now| super::scene::ago(now - fix.fix_time()).resolve(lang));
            MapPin {
                peer: *peer,
                at,
                label: match Some(*peer) == own {
                    true => super::strings::StrId::MapYou.text(lang).to_string(),
                    false => label(peer),
                },
                age,
                stale: now.is_some_and(|now| fix.stale(now)),
                nearby: Some(*peer) != own && nearby(peer),
                own: Some(*peer) == own,
                fix_at: Some(fix.fix_time()),
                battery: fix.battery(),
            }
        })
        .collect();
    let as_pins: Vec<Pin<'_>> = all
        .iter()
        .map(|p| Pin {
            at: p.at,
            label: &p.label,
            stale: p.stale,
        })
        .collect();
    let order = pins::order(&as_pins, |i| all[i].peer);
    order.into_iter().map(|i| all[i].clone()).collect()
}

/// The picture's cells in the conversation pane `pane` (its outer
/// rectangle): inside the border, less the bottom row, which carries the
/// attribution.
pub fn picture_area(pane: Rect) -> Rect {
    Rect::new(
        pane.x.saturating_add(1),
        pane.y.saturating_add(1),
        pane.width.saturating_sub(2),
        pane.height.saturating_sub(2),
    )
}

/// The world projection the product ships (1710): Equal Earth, Lew's
/// first decision; the orthographic globe behind the `globe-world` build
/// switch until he has seen both on pictures.
pub fn world_shape() -> Shape {
    match cfg!(feature = "globe-world") {
        true => Shape::Globe,
        false => Shape::EqualEarth,
    }
}

/// The view of `state` over `area` at `setup`'s cell size, as every
/// frontend shows it: one world at most ([`Viewport::one_world`]), however
/// far the state was zoomed out or panned north or south, and the world
/// never smaller than the picture: the whole zoom at or past the
/// [`world_floor_milli`] of the product's [`world_shape`] (1796), the
/// window's and the phone's floor rounded up to the terminal's steps.
pub fn viewport(state: &MapState, area: Rect, setup: &MapSetup) -> Viewport {
    let (w, h) = pixels(area, setup);
    let floor = world_floor_milli(world_shape(), w, h).div_ceil(MILLI);
    let floor = u8::try_from(floor).unwrap_or(u8::MAX);
    Viewport::new(state.centre, state.zoom.max(floor), w, h).one_world()
}

/// `state` moved to the view it shows ([`viewport`]): a zoom out past the
/// one-world floor stops there, a pan past the north or south edge stops
/// at it, and the next step starts from what the reader sees.
pub fn settle(state: &mut MapState, area: Rect, setup: &MapSetup) {
    let view = viewport(state, area, setup);
    state.centre = view.centre;
    state.zoom = view.zoom();
}

/// The shallowest zoom the reader may go to over `area`: `setup`'s, or
/// deeper where the area is wider than the world there
/// ([`Viewport::min_zoom`]); never past `setup`'s deepest.
pub fn zoom_floor(area: Rect, setup: &MapSetup) -> u8 {
    let (w, _) = pixels(area, setup);
    Viewport::min_zoom(w).max(setup.zooms.0).min(setup.zooms.1)
}

fn pixels(area: Rect, setup: &MapSetup) -> (u32, u32) {
    (
        u32::from(area.width) * u32::from(setup.cell_px.0.max(1)),
        u32::from(area.height) * u32::from(setup.cell_px.1.max(1)),
    )
}

/// The view that shows every pin, at the deepest zoom of `setup` that
/// does, but no deeper than the region under its centre has data for
/// ([`fit_ceiling`]): one pin fits at that region's max zoom, and `+` goes
/// on into the overzoom. Where no region holds the centre the radar draws
/// (1386): the fit goes to [`crate::ui::radar::FIT_ZOOM`] where `setup`
/// reaches it, the outline's deepest where it does not. With no pin, the first of
/// `setup`'s regions, which the shells list newest first; with none of
/// those either, the world at the [`zoom_floor`].
pub fn fit(pins: &[MapPin], area: Rect, setup: &MapSetup) -> (World, u8) {
    let mut points: Vec<World> = pins.iter().map(|p| p.at).collect();
    if points.is_empty() {
        if let Some(region) = setup.regions.first() {
            let b = region.bounds;
            points.push(project(LatLon {
                lat_e7: b.north,
                lon_e7: b.west,
            }));
            points.push(project(LatLon {
                lat_e7: b.south,
                lon_e7: b.east,
            }));
        }
    }
    fit_points(&points, area, setup)
}

/// The pins "fit all" brings into view (1386): those of `inside`, the
/// peers with a position inside the tracks' time window
/// ([`crate::ui::tracks::inside`]); every pin when none is.
pub fn in_window(pins: &[MapPin], inside: &[[u8; 16]]) -> Vec<MapPin> {
    let kept: Vec<MapPin> = pins
        .iter()
        .filter(|p| inside.contains(&p.peer))
        .cloned()
        .collect();
    match kept.is_empty() {
        true => pins.to_vec(),
        false => kept,
    }
}

/// The view centred on one point, at the zoom [`fit`] gives a single pin
/// there: the deepest the region under it has data for, or the outline's
/// without one. Where a report's fix opens the map (1076).
pub fn fit_at(at: World, area: Rect, setup: &MapSetup) -> (World, u8) {
    fit_points(&[at], area, setup)
}

fn fit_points(points: &[World], area: Rect, setup: &MapSetup) -> (World, u8) {
    let (w, h) = pixels(area, setup);
    let margin = FIT_MARGIN_CELLS * u32::from(setup.cell_px.0.max(1));
    let floor = zoom_floor(area, setup);
    let (centre, zoom) = match Viewport::fit(points, w, h, margin, floor..=setup.zooms.1) {
        Some(view) => {
            // Where no region holds the centre the radar draws (1386): the
            // fit goes as deep as its rings read, not the outline's.
            let at = unproject(view.centre);
            let held = setup.regions.iter().any(|h| h.bounds.contains(at));
            let ceiling = fit_ceiling(&setup.regions, view.centre);
            let ceiling = match held {
                true => ceiling,
                false => ceiling.max(crate::ui::radar::FIT_ZOOM.min(setup.zooms.1)),
            }
            .max(floor);
            (view.centre, view.zoom().min(ceiling))
        }
        None => (
            World {
                x: 1 << 31,
                y: 1 << 31,
            },
            floor,
        ),
    };
    let view = Viewport::new(centre, zoom, w, h).one_world();
    (view.centre, view.zoom())
}

/// The pins of `pins` that land in `view` drawn over `area`, as cells,
/// their names set by [`pins::labels`] in cells.
pub fn place(
    pins: &[MapPin],
    view: &Viewport,
    area: Rect,
    setup: &MapSetup,
    selected: Option<[u8; 16]>,
    ascii: bool,
) -> Vec<PinCell> {
    let as_pins: Vec<Pin<'_>> = pins
        .iter()
        .map(|p| Pin {
            at: p.at,
            label: &p.label,
            stale: p.stale,
        })
        .collect();
    let landed: Vec<(&MapPin, (u16, u16), u16, String)> = pins::place(view, &as_pins)
        .into_iter()
        .filter_map(|placed| {
            let pin = pins.get(placed.index)?;
            let col = u16::try_from(placed.x / u32::from(setup.cell_px.0.max(1))).ok()?;
            let row = u16::try_from(placed.y / u32::from(setup.cell_px.1.max(1))).ok()?;
            // A battery below a fifth after the name (1386). Our own
            // position is the dot alone, no name (1449 item 2).
            let words = match (pin.low_battery(), ascii) {
                _ if pin.own => String::new(),
                (false, _) => pin.words(),
                (true, false) => format!("{} {BATTERY_GLYPH}", pin.words()),
                (true, true) => format!("{} !", pin.words()),
            };
            let len = u16::try_from(words.chars().count()).unwrap_or(u16::MAX);
            Some((pin, (col, row), len, words))
        })
        .collect();
    let marks: Vec<Mark> = landed
        .iter()
        .map(|(_, (col, row), _, _)| Mark {
            x: u32::from(*col),
            y: u32::from(*row),
        })
        .collect();
    let chosen = selected.and_then(|peer| landed.iter().position(|(p, ..)| p.peer == peer));
    let sides = pins::labels(
        &marks,
        |i| landed.get(i).map_or(0, |(_, _, len, _)| u32::from(*len)),
        chosen,
        (u32::from(area.width), u32::from(area.height)),
        Metrics::CELLS,
    );
    let mut out: Vec<PinCell> = landed
        .into_iter()
        .zip(sides)
        .map(|((pin, (ask_col, ask_row), len, words), label)| {
            let side = label.map(|l| l.side);
            let selected = Some(pin.peer) == selected;
            let marker = match (ascii, selected) {
                (false, false) => MARKER,
                (false, true) => SELECTED,
                (true, false) => '*',
                (true, true) => '@',
            };
            let (col, row) = (area.x + ask_col, area.y + ask_row);
            let apart = |row: u16| {
                Some(LabelCell {
                    col: area.x + ask_col.min(area.width.saturating_sub(len)),
                    row,
                    text: words.clone(),
                })
            };
            let (col, text, apart) = match side {
                _ if pin.own => (col, marker.to_string(), None),
                Some(Side::Right) => (col, format!("{marker} {words}"), None),
                Some(Side::Left) => (
                    col.saturating_sub(len + 1),
                    format!("{words} {marker}"),
                    None,
                ),
                Some(Side::Above) => (col, marker.to_string(), apart(row - 1)),
                Some(Side::Below) => (col, marker.to_string(), apart(row + 1)),
                None => (col, marker.to_string(), None),
            };
            PinCell {
                peer: pin.peer,
                col,
                row,
                text,
                apart,
                stale: pin.stale,
                nearby: pin.nearby,
                selected,
                own: pin.own,
            }
        })
        .collect();
    out.sort_by_key(|p| p.selected);
    out
}

/// How far, in cells either way, the pointer goes from where the left
/// button went down on the map before the press is a drag and no longer a
/// click. One: the terminal reports the pointer by the cell, so a report
/// of the next cell is a hand that moved, and a hand that held still sends
/// none.
pub const DRAG_CELLS: u16 = 1;

/// The left button held down on the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapPress {
    /// The cell it went down on.
    pub from: (u16, u16),
    /// The view's centre then.
    pub centre: World,
    /// The pin it went down on, which a release without a drag opens.
    pub pin: Option<[u8; 16]>,
    /// The pointer went [`DRAG_CELLS`] or more from `from`: the release
    /// opens nothing, and it stays a drag when the pointer comes back.
    pub dragging: bool,
}

impl MapPress {
    pub fn new(from: (u16, u16), centre: World, pin: Option<[u8; 16]>) -> Self {
        Self {
            from,
            centre,
            pin,
            dragging: false,
        }
    }

    /// The pointer is at `(col, row)`: a drag from there on once it is far
    /// enough from where the button went down.
    pub fn reach(&mut self, col: u16, row: u16) {
        let far = |a: u16, b: u16| a.abs_diff(b) >= DRAG_CELLS;
        self.dragging |= far(col, self.from.0) || far(row, self.from.1);
    }

    /// The centre with the pointer at `(col, row)`: the view at the press
    /// moved the other way by the pointer's distance in the backend's
    /// pixels, so the point under the pointer when the button went down is
    /// under it again. The press's own centre while it is not a drag.
    pub fn centre_at(&self, col: u16, row: u16, state: &MapState, setup: &MapSetup) -> World {
        if !self.dragging {
            return self.centre;
        }
        let cells = |to: u16, from: u16| i32::from(to) - i32::from(from);
        let mut view = Viewport::new(self.centre, state.zoom, 1, 1);
        view.pan(
            -cells(col, self.from.0) * i32::from(setup.cell_px.0.max(1)),
            -cells(row, self.from.1) * i32::from(setup.cell_px.1.max(1)),
        );
        view.centre
    }
}

/// `state` at `zoom`, about the cell `(col, row)` when it is on the
/// picture `area`: the point at the centre of that cell stays under it,
/// as the wheel under the pointer expects. Off the picture, about the
/// centre. Never past one world ([`settle`]).
pub fn zoom_at(state: &mut MapState, area: Rect, setup: &MapSetup, col: u16, row: u16, zoom: u8) {
    let on = (area.x..area.x + area.width).contains(&col)
        && (area.y..area.y + area.height).contains(&row);
    if !on {
        state.zoom = zoom;
        settle(state, area, setup);
        return;
    }
    let (cw, ch) = (
        i32::from(setup.cell_px.0.max(1)),
        i32::from(setup.cell_px.1.max(1)),
    );
    let x = i32::from(col - area.x) * cw + cw / 2;
    let y = i32::from(row - area.y) * ch + ch / 2;
    let mut view = viewport(state, area, setup);
    view.zoom_about(zoom, x, y);
    let view = view.one_world();
    state.centre = view.centre;
    state.zoom = view.zoom();
    // Past the world's floor (1796) the zoom stops there, about the centre.
    settle(state, area, setup);
}

/// The pin drawn at a cell, its marker or its name, the topmost where
/// texts overlap.
pub fn pin_at(cells: &[PinCell], area: Rect, col: u16, row: u16) -> Option<[u8; 16]> {
    let hit = |at: u16, on: u16, text: &str| {
        let end = at
            .saturating_add(text.chars().count() as u16)
            .min(area.x + area.width);
        on == row && (at..end).contains(&col)
    };
    cells
        .iter()
        .rev()
        .find(|p| {
            hit(p.col, p.row, &p.text)
                || p.apart.as_ref().is_some_and(|a| hit(a.col, a.row, &a.text))
        })
        .map(|p| p.peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn name(kind: LabelKind, rank: u8, text: &str, col: i64, row: i64) -> ViewLabel {
        // The anchor in the middle of its cell, braille's 2 x 4 pixels.
        let sub = names::SUBPIXEL;
        ViewLabel {
            kind,
            rank,
            min_zoom: 0,
            text: text.to_string(),
            local: text.to_string(),
            at: (col as u32, row as u32),
            anchor: (col * 2 * sub + sub, row * 4 * sub + sub),
            line: Vec::new(),
        }
    }

    /// The density rule in cells: one place a 20 x 6 block, the larger
    /// first, a free cell around each, never cut at the pane's edge, never
    /// over a pin, never a street without a line; and the frame it gives
    /// pinned as text.
    #[test]
    fn names_are_set_by_the_density_rule() {
        let area = Rect::new(2, 1, 60, 18);
        let labels = [
            name(LabelKind::Village, 4, "Grassina", 12, 3),
            name(LabelKind::City, 12, "Firenze", 10, 2),
            name(LabelKind::Town, 6, "Fiesole", 30, 2),
            // Would be cut at the right edge: left out.
            name(LabelKind::Town, 6, "Bagno a Ripoli", 57, 9),
            // On the pin's run.
            name(LabelKind::Town, 6, "Prato", 45, 8),
            name(LabelKind::Water, 0, "Arno", 22, 13),
            name(LabelKind::Suburb, 1, "Gavinana", 50, 14),
            name(LabelKind::MinorRoad, 0, "Via dei Servi", 5, 16),
        ];
        let pin = (43, 8, 9);
        let got = place_names(&labels, area, (2, 4), &[pin]);
        let mut frame = vec![vec![' '; 60]; 18];
        for (x, ch) in (pin.0..pin.0 + pin.2).zip("● alice ".chars().cycle()) {
            frame[usize::from(pin.1)][usize::from(x)] = ch;
        }
        for n in &got {
            let (col, row) = (usize::from(n.col - area.x), usize::from(n.row - area.y));
            for (i, ch) in n.text.chars().enumerate() {
                frame[row][col + i] = ch;
            }
        }
        let text: Vec<String> = frame
            .iter()
            .map(|r| r.iter().collect::<String>().trim_end().to_string())
            .collect();
        assert_eq!(
            text,
            [
                "",
                "",
                "       Firenze             Fiesole",
                "",
                "",
                "",
                "",
                "",
                "                                           ● alice ●",
                "",
                "",
                "",
                "",
                "                    Arno",
                "                                              Gavinana",
                "",
                "",
                "",
            ],
            "{got:?}"
        );
        let mut blocks: Vec<(u16, u16)> = got
            .iter()
            .map(|n| {
                (
                    (n.col - area.x) / NAME_BLOCK.0,
                    (n.row - area.y) / NAME_BLOCK.1,
                )
            })
            .collect();
        let all = blocks.len();
        blocks.sort_unstable();
        blocks.dedup();
        assert_eq!(blocks.len(), all, "one name a block");
    }

    /// A line label whose line runs through the middles of `cells`, as
    /// braille's 2 x 4 pixels a cell.
    fn along(kind: LabelKind, text: &str, at: u32, cells: &[(i64, i64)]) -> ViewLabel {
        let sub = names::SUBPIXEL;
        let line: Vec<(i64, i64)> = cells
            .iter()
            .map(|&(c, r)| (c * 2 * sub + sub, r * 4 * sub + 2 * sub))
            .collect();
        ViewLabel {
            kind,
            rank: 0,
            min_zoom: 0,
            text: text.to_string(),
            local: text.to_string(),
            at: (at, 0),
            anchor: line[0],
            line: vec![line],
        }
    }

    /// Street and river names in cells (1126): a street on a level run at
    /// least two cells longer than its name, centred on it, the major road
    /// first; a street that climbs a row every few cells and one that runs
    /// straight down are not named; a river where it crosses a block row's
    /// middle, once in the repeat distance; never over a place's name or a
    /// pin; and the frame pinned as text.
    #[test]
    fn streets_on_level_runs_and_rivers_where_they_cross_a_block_row() {
        let area = Rect::new(0, 0, 60, 18);
        // Climbs a row every three cells: no run is long enough.
        let stairs: Vec<(i64, i64)> = (0..6).map(|i| (22 + 3 * i, 8 + i)).collect();
        let labels = [
            name(LabelKind::City, 12, "Firenze", 10, 1),
            along(LabelKind::MinorRoad, "Via Rosa", 1, &[(2, 9), (19, 9)]),
            // Its run is the name and one cell: too short.
            along(LabelKind::MinorRoad, "Via Corta", 2, &[(2, 15), (11, 15)]),
            along(
                LabelKind::MajorRoad,
                "Viale Lavagnini",
                3,
                &[(0, 14), (21, 14)],
            ),
            along(LabelKind::SecondaryRoad, "Via Scesa", 4, &stairs),
            along(
                LabelKind::SecondaryRoad,
                "Via Dritta",
                5,
                &[(30, 0), (30, 17)],
            ),
            along(LabelKind::River, "Arno", 6, &[(50, 0), (48, 17)]),
            // Centred on its run it would cover the pin: left out.
            along(LabelKind::MinorRoad, "Via Nuova", 7, &[(20, 4), (58, 4)]),
        ];
        let pin = (28, 4, 9);
        let got = place_names(&labels, area, (2, 4), &[pin]);
        let mut frame = vec![vec![' '; 60]; 18];
        for (x, ch) in (pin.0..pin.0 + pin.2).zip("● alice ".chars().cycle()) {
            frame[usize::from(pin.1)][usize::from(x)] = ch;
        }
        for n in &got {
            for (i, ch) in n.text.chars().enumerate() {
                frame[usize::from(n.row)][usize::from(n.col) + i] = ch;
            }
        }
        let text: Vec<String> = frame
            .iter()
            .map(|r| r.iter().collect::<String>().trim_end().to_string())
            .collect();
        assert_eq!(text, PINNED_STREETS, "{got:?}");
        let kinds: Vec<LabelKind> = got.iter().map(|n| n.kind).collect();
        assert_eq!(
            kinds,
            [
                LabelKind::City,
                LabelKind::River,
                LabelKind::MajorRoad,
                LabelKind::MinorRoad,
            ]
        );
    }

    const PINNED_STREETS: [&str; 18] = [
        "",
        "       Firenze",
        "",
        "                                                Arno",
        "                            ● alice ●",
        "",
        "",
        "",
        "",
        "       Via Rosa",
        "",
        "",
        "",
        "",
        "    Viale Lavagnini",
        "",
        "",
        "",
    ];

    /// The cells `place` draws for two pins on one spot: neither name
    /// covers the other, and the selected one is drawn last.
    #[test]
    fn place_sets_two_names_on_one_spot_apart() {
        let at = leviculum_map::mercator::project(LatLon::from_degrees(43.775, 11.261));
        let pin = |peer: u8, label: &str| MapPin {
            peer: [peer; 16],
            at,
            label: label.to_string(),
            age: None,
            stale: false,
            nearby: false,
            own: false,
            fix_at: None,
            battery: None,
        };
        let pins = [pin(1, "you"), pin(2, "them")];
        let area = Rect::new(1, 1, 40, 10);
        let setup = MapSetup::default();
        let view = viewport(
            &MapState {
                centre: at,
                zoom: 4,
                selected: None,
            },
            area,
            &setup,
        );
        let cells = place(&pins, &view, area, &setup, Some([2; 16]), false);
        let texts: Vec<&str> = cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["you ●", "◉ them"]);
        assert_eq!(cells[0].col + 4, cells[1].col, "one marker cell");
        let (marker, row) = (cells[1].col, cells[1].row);
        assert_eq!(pin_at(&cells, area, marker, row), Some([2; 16]), "on top");
        assert_eq!(pin_at(&cells, area, marker - 4, row), Some([1; 16]));
    }

    /// `0` with one pin fits at the deepest zoom the region under it has
    /// data for, the Florence fixture's z15, not at the overzoom the
    /// setup offers past it (1074, Lew: one pin in Frankfurt fitted at z20,
    /// plain land); `+` still goes on. Without a region under the pin, at
    /// the outline's z10.
    #[test]
    fn one_pin_fits_at_the_regions_max_zoom_not_past_it() {
        use leviculum_map::choice::view_zooms;
        use leviculum_map::region::Region;
        const FLORENCE: &[u8] = include_bytes!("../../../leviculum-map/fixtures/firenze.lmr");
        let header = *Region::from_bytes(FLORENCE).expect("open").header();
        let zooms = view_zooms([&header]);
        let setup = MapSetup {
            cell_px: (9, 19),
            zooms: (*zooms.start(), *zooms.end()),
            regions: vec![header],
            ..MapSetup::default()
        };
        assert!(setup.zooms.1 > header.max_zoom, "overzoom is offered");
        let area = Rect::new(1, 1, 60, 20);
        let pin = |lat: f64, lon: f64| MapPin {
            peer: [1; 16],
            at: project(LatLon::from_degrees(lat, lon)),
            label: "bob".to_string(),
            age: None,
            stale: false,
            nearby: false,
            own: false,
            fix_at: None,
            battery: None,
        };
        let (_, zoom) = fit(&[pin(43.7731, 11.2560)], area, &setup);
        assert_eq!(zoom, header.max_zoom);
        // Frankfurt, outside the fixture: the radar draws there (1386),
        // and the fit goes to its zoom where the setup reaches it, the
        // outline's deepest where it does not.
        let (_, zoom) = fit(&[pin(50.1106, 8.6821)], area, &setup);
        assert_eq!(
            zoom,
            crate::ui::radar::FIT_ZOOM
                .min(setup.zooms.1)
                .max(leviculum_map::outline::MAX_ZOOM)
        );
        let none = MapSetup {
            regions: Vec::new(),
            ..setup.clone()
        };
        let (_, zoom) = fit(&[pin(43.7731, 11.2560)], area, &none);
        assert_eq!(
            zoom,
            crate::ui::radar::FIT_ZOOM
                .min(setup.zooms.1)
                .max(leviculum_map::outline::MAX_ZOOM)
        );
        let outline_only = MapSetup {
            zooms: (0, leviculum_map::outline::MAX_ZOOM),
            ..none
        };
        let (_, zoom) = fit(&[pin(43.7731, 11.2560)], area, &outline_only);
        assert_eq!(zoom, leviculum_map::outline::MAX_ZOOM);
    }

    /// The first view without pins (1131): the first region of the setup,
    /// which the shells list newest first, fitted whole; with no region,
    /// the world at the one-world floor, which on a portrait phone is z3
    /// and not the z0 that showed the world five times.
    #[test]
    fn the_first_view_without_pins_is_the_newest_region_else_one_world() {
        use leviculum_map::region::Region;
        const FLORENCE: &[u8] = include_bytes!("../../../leviculum-map/fixtures/firenze.lmr");
        let florence = *Region::from_bytes(FLORENCE).expect("open").header();
        let mut elsewhere = florence;
        elsewhere.bounds = Bounds {
            west: 85_000_000,
            south: 500_000_000,
            east: 87_000_000,
            north: 502_000_000,
        };
        let phone = Rect::new(0, 0, 1080, 2400);
        let setup = MapSetup {
            cell_px: (1, 1),
            zooms: (0, 20),
            regions: vec![florence, elsewhere],
            ..MapSetup::default()
        };
        let (centre, zoom) = fit(&[], phone, &setup);
        let view = viewport(
            &MapState {
                centre,
                zoom,
                selected: None,
            },
            phone,
            &setup,
        );
        let b = florence.bounds;
        for (lat, lon) in [(b.north, b.west), (b.south, b.east)] {
            let corner = project(LatLon {
                lat_e7: lat,
                lon_e7: lon,
            });
            assert!(view.contains(corner), "the box at z{zoom}");
        }
        assert!(zoom >= 14, "fitted to the region, not the world: z{zoom}");
        // The second region first: the view goes there.
        let swapped = MapSetup {
            regions: vec![elsewhere, florence],
            ..setup.clone()
        };
        let (centre, _) = fit(&[], phone, &swapped);
        let at = unproject(centre);
        assert!((at.lon_e7 - 86_000_000).abs() < 1_000_000, "{at:?}");
        // No region: one world on the phone, centred.
        let bare = MapSetup {
            regions: Vec::new(),
            ..setup
        };
        let (centre, zoom) = fit(&[], phone, &bare);
        assert_eq!(zoom, 3);
        assert_eq!(centre.y, 1 << 31);
        assert_eq!(zoom_floor(phone, &bare), 3);
    }

    /// The terminal's world at its smallest is as wide as its picture
    /// (1796): settled from z0 the product's world (Equal Earth, the globe
    /// in the globe build) spans at least the picture's width, the globe
    /// its shorter side; a step further out settles to the same zoom.
    #[test]
    fn settle_keeps_the_smallest_world_as_wide_as_the_picture() {
        use leviculum_map::blend::Blend;
        let shape = world_shape();
        let setup = MapSetup {
            cell_px: (2, 4),
            zooms: (0, 20),
            ..MapSetup::default()
        };
        for (cols, rows) in [(495u16, 550u16), (200, 60), (80, 24), (612, 549)] {
            let area = Rect::new(0, 0, cols, rows);
            let (w, h) = (u32::from(cols) * 2, u32::from(rows) * 4);
            let mut state = MapState {
                centre: project(LatLon::from_degrees(43.77, 11.26)),
                zoom: 0,
                selected: None,
            };
            settle(&mut state, area, &setup);
            let view = viewport(&state, area, &setup);
            let blend = Blend::new(shape, view.centre, f64::from(view.zoom()), w, h);
            let pi = core::f64::consts::PI;
            let (across, side) = match shape {
                Shape::Globe => (2.0 * blend.radius(), w.min(h)),
                Shape::EqualEarth => (
                    blend.to_screen_angles(pi, 0.0).0 - blend.to_screen_angles(-pi, 0.0).0,
                    w,
                ),
            };
            assert!(
                across >= f64::from(side) - 0.5,
                "{w}x{h}: the world {across:.1} across, the picture {side}"
            );
            let zoom = state.zoom;
            state.zoom = zoom.saturating_sub(1);
            settle(&mut state, area, &setup);
            assert_eq!(state.zoom, zoom, "{w}x{h}: a step further out");
        }
    }

    /// A zoom out past the floor stops there, and a pan past the north
    /// edge stops at it: the state is what the reader sees.
    #[test]
    fn settle_stops_the_zoom_and_the_pan_at_one_world() {
        let setup = MapSetup {
            cell_px: (1, 1),
            zooms: (0, 20),
            ..MapSetup::default()
        };
        let phone = Rect::new(0, 0, 1080, 2400);
        let mut state = MapState {
            centre: project(LatLon::from_degrees(84.0, 0.0)),
            zoom: 0,
            selected: None,
        };
        settle(&mut state, phone, &setup);
        // Mercator's and Equal Earth's floor is z3 on 1080 pixels; the
        // globe's disk needs z3.73 to be 1080 across (1796), z4 in whole
        // steps.
        let floor = match world_shape() {
            Shape::Globe => 4,
            Shape::EqualEarth => 3,
        };
        assert_eq!(state.zoom, floor);
        match 256u32 << floor <= 2400 {
            true => assert_eq!(state.centre.y, 1 << 31, "shorter than the view: centred"),
            false => assert_eq!(
                viewport(&state, phone, &setup).top_left_sub().1,
                0,
                "taller than the view: stopped at the north edge"
            ),
        }
        state.zoom = 6;
        state.centre = project(LatLon::from_degrees(84.0, 0.0));
        settle(&mut state, phone, &setup);
        let view = viewport(&state, phone, &setup);
        assert_eq!(view.top_left_sub().1, 0, "stopped at the north edge");
        assert_eq!(view.centre, state.centre);
    }
}
