//! The map on the M9 (slice seven, 1375): the model's map, drawn in pixels
//! over the whole panel, the place and street names, the pins, and a row
//! of words at the bottom while there is something to say.
//!
//! The map is the one screen the cell renderer does not draw. The model
//! holds it as it holds it for every target (`loquax::ui::map`): the
//! state, the pins from the positions (a share's, a `lev.position`
//! announce's, the board's own GNSS fix), the fit, the pan and the zoom
//! the terminal's keys run. What is the M9's own is where the picture is
//! ([`PICTURE`], told to the model as [`UiModel::map_picture`]), the pixels
//! of a cell of its atlas ([`CELL_PX`]), the keys it has no terminal key
//! for ([`key`]), and the painter: the map core's filler
//! (`leviculum_map::draw`) straight into the panel, not cells through
//! mousefood. 1375 measured both for this screen
//! (`handheld-messenger.md`, "Slice seven, host half"): through mousefood
//! the map is braille or quadrant cells, 106 x 84 dots at best on the
//! 53 x 21 cells of the 6 x 10 atlas then (64 x 36 on today's 32 x 9 of
//! the 10 x 20, 1428), and slower to make than the pixels it stands for.
//!
//! The chrome (1531): no title row and no legend. The picture is rows 0
//! to 10 for the model (its fit and its pins); row 11 is the bottom row,
//! which shows the key hint for a few seconds after the map opens and
//! after a key the map does not know ([`Chrome`]), else the chosen pin's
//! card with the data's credit, else more of the picture. The credit
//! also stands in the picture's top right corner for five seconds the
//! first time the map opens after a boot.
//!
//! A draw is the cells first, every row in the map's ground, then the
//! map over them, the names, the pins and the words. ratatui draws only
//! cells that changed, so the next map's blank rows are left alone and
//! the map is drawn over them again; the renderer clears its cells when
//! it leaves the map, so a list drawn after it redraws every cell
//! ([`crate::render::Renderer::show`]).

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use embedded_graphics::mono_font::{MonoFont, MonoTextStyleBuilder};
use embedded_graphics::pixelcolor::{Rgb565, Rgb888};
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use embedded_graphics::text::{Baseline, Text};
use leviculum_map::draw::{draw_in_scaled, Ink};
use leviculum_map::labels::{place, Candidate, LabelKind, Rect as LabelRect, Rule, ViewLabel};
use leviculum_map::outline;
use leviculum_map::region::{Header, Region, RegionError};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::fill::{Edge, Filler, Scratch};
use leviculum_mapfill::render::{NoClock, RenderStats, Work};
use leviculum_mapfill::style::{Profile, Style};
use leviculum_ui::color::Ink as UiInk;
use leviculum_ui::input::{InputBuffer, KeyCode, KeyEvent, KeyModifiers};
use leviculum_ui::keymap::Ctx;
use leviculum_ui::style::{RLine, RStyle};
use leviculum_ui::text::fit_width;
use leviculum_ui::Rect;
use loquax::keys::{panes_of, resolve, MapCmd};
use loquax::ui::map::{self as core_map, MapSetup, NameCell, PinCell};
use loquax::ui::strings::{Lang, StrId, Text as Words};
use loquax::ui::{Pane, UiModel};

use crate::keys::M9Key;
use crate::screen::{cut, fill, push, Look, COLS, ROWS as GRID_ROWS};

/// The pixels of one cell of the atlas ([`crate::font`]): what the model
/// scales the picture's cells by, so its viewport is the picture's pixels.
pub use crate::font::CELL_PX;

/// The picture's cells as the model knows them: the whole width, rows 0
/// to 10, 32 x 11 cells, 320 x 220 pixels (1531). The model fits and
/// places its pins in these; the bottom row ([`BOTTOM`]) is the map's too
/// whenever it has no words to show.
pub const PICTURE: Rect = Rect::new(0, 0, COLS as u16, (GRID_ROWS - 1) as u16);

/// The bottom row: the key hint, the chosen pin's card, or the picture.
pub const BOTTOM: u16 = (GRID_ROWS - 1) as u16;

/// The region the board shows before it has one of its own (1375): the
/// map core's Florence fixture, zooms 11 to 15, 197,972 B, read in place
/// from the image's flash. OpenStreetMap data under the ODbL, its notice
/// in the file and in `leviculum-map/fixtures/README.md`.
pub const FIXTURE: &[u8] = include_bytes!("../../leviculum-map/fixtures/firenze.lmr");

/// The world on the board (1477): the map core's own cut of the whole
/// world, zooms 0 to 3, coasts, land, the major roads and the cities'
/// names (no country borders: the cut's profile has no class for them),
/// 755,245 B, read in place from the image's flash like
/// [`FIXTURE`]. Drawn wherever no detailed region lies under the view, up
/// to [`leviculum_map::viewport::MAX_OVERZOOM`] past its deepest zoom, so
/// any view at any place shows a map. How it was cut:
/// `leviculum-map/fixtures/README.md`, "The world overview".
pub const WORLD: &[u8] = include_bytes!("../../leviculum-map/data/world-overview.lmr");

/// How far past a region's deepest zoom the M9 zooms in: two steps, where
/// the lines are still the region's and a street is a hand wide.
pub const OVERZOOM: u8 = 2;

/// The map's finish on the M9: its square caps, no casings and no
/// buildings, in the light table (1531: Lew read the dark one on the
/// board as grey-brown and far too dark). The UI around it keeps its dark
/// tokens until the house page's theme switch.
pub fn style() -> Style {
    Style::M9
}

/// What the board tells the model it draws the map with: a cell of the
/// atlas, the region's zooms and two steps past its deepest. No ways of
/// drawing to cycle through: the M9 has one.
pub fn setup(header: &Header) -> MapSetup {
    setup_of(core::slice::from_ref(header))
}

/// [`setup`] for every region the board carries (1477): the zooms from
/// the shallowest region's to two steps past the deepest's.
pub fn setup_of(headers: &[Header]) -> MapSetup {
    let low = headers.iter().map(|h| h.min_zoom).min().unwrap_or(0);
    let high = headers.iter().map(|h| h.max_zoom).max().unwrap_or(0);
    MapSetup {
        cell_px: CELL_PX,
        zooms: (low, high.saturating_add(OVERZOOM)),
        regions: headers.to_vec(),
        drawings: Vec::new(),
        drawing: 0,
    }
}

/// Tell `model` the board's map: the regions the image carries
/// ([`FIXTURE`] and the [`WORLD`]) and the picture's cells. The fixture's
/// model and a home's (1476) both, so a home's map is the same picture
/// under the same keys.
pub fn ready<B>(model: &mut UiModel<B>)
where
    B: InputBuffer + Default,
{
    let headers: Vec<Header> = [FIXTURE, WORLD]
        .iter()
        .filter_map(|bytes| Region::from_bytes(bytes).ok().map(|r| *r.header()))
        .collect();
    if !headers.is_empty() {
        loquax::ui::update(model, loquax::ui::UiEvent::MapSetup(setup_of(&headers)));
    }
    model.map_picture = Some(PICTURE);
}

/// Whether `model` shows the map.
pub fn shown<B>(model: &UiModel<B>) -> bool {
    model.focus == Pane::Map && model.map.is_some()
}

/// The map's key table: the verb, the M9 key, and the terminal's command
/// it runs once [`key`] made it the terminal's key. Every row is proven
/// through the shared resolver (`tests/map.rs`). T and Y are the two keys
/// right under the ring (1531, Lew): `+` and `-` sit on the controller's
/// Sym layer; I and O are the usual in and out.
pub const ROWS: [(&str, M9Key, MapCmd); 15] = [
    ("pan west", M9Key::Left, MapCmd::Pan(-1, 0)),
    ("pan east", M9Key::Right, MapCmd::Pan(1, 0)),
    ("pan north", M9Key::Up, MapCmd::Pan(0, -1)),
    ("pan south", M9Key::Down, MapCmd::Pan(0, 1)),
    ("zoom in", M9Key::Char('t'), MapCmd::ZoomIn),
    ("zoom out", M9Key::Char('y'), MapCmd::ZoomOut),
    ("zoom in", M9Key::Char('i'), MapCmd::ZoomIn),
    ("zoom out", M9Key::Char('o'), MapCmd::ZoomOut),
    ("zoom in", M9Key::Char('+'), MapCmd::ZoomIn),
    ("zoom out", M9Key::Char('-'), MapCmd::ZoomOut),
    ("next pin", M9Key::Char(' '), MapCmd::Step(true)),
    ("previous pin", M9Key::Backspace, MapCmd::Step(false)),
    ("every pin", M9Key::Char('0'), MapCmd::Fit),
    ("open the pin", M9Key::Enter, MapCmd::Open),
    ("close", M9Key::Back, MapCmd::Close),
];

/// The terminal's key for a key the M9 has none for, on the map: Space
/// steps to the next pin and Backspace to the one before, the terminal's
/// Tab and Shift-Tab; T and I zoom in as `+` does, Y and O out as `-`
/// (1531). Every other key is the terminal's already (the arrows pan, `+`
/// and `-` zoom, `0` fits, Enter opens, Back closes).
pub fn key<B>(model: &UiModel<B>, key: KeyEvent) -> KeyEvent {
    if !shown(model) {
        return key;
    }
    let plain = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
    match key.code {
        KeyCode::Char(' ') => KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        KeyCode::Backspace => KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        KeyCode::Char('t' | 'T' | 'i' | 'I') => plain('+'),
        KeyCode::Char('y' | 'Y' | 'o' | 'O') => plain('-'),
        _ => key,
    }
}

/// Whether the open map does something with `event`: a key it does not
/// know brings the key hint back ([`Chrome::nudge`]). Off the map every
/// key is "known".
pub fn bound<B>(model: &UiModel<B>, event: &KeyEvent) -> bool {
    !shown(model) || resolve(&key(model, *event), panes_of(Pane::Map), Ctx::NONE).is_some()
}

/// The open map's view over [`PICTURE`] and its pins in cells, the
/// markers in ASCII (`*` and `@`): the atlas has `●` but not the selected
/// pin's `◉`. `None` while the map is closed.
pub fn view<B: InputBuffer>(model: &UiModel<B>) -> Option<(Viewport, Vec<PinCell>)> {
    let state = model.map.as_ref()?;
    let area = model.map_area();
    let viewport = core_map::viewport(state, area, &model.map_setup);
    let pins = core_map::place(
        &model.map_pins(),
        &viewport,
        area,
        &model.map_setup,
        state.selected,
        true,
    );
    Some((viewport, pins))
}

/// `view` grown by the bottom row, its top left corner kept: the picture,
/// which always takes the bottom row; its words are drawn over it (1554).
pub fn with_bottom(view: &Viewport) -> Viewport {
    let (w, h) = view.size();
    let mut tall = *view;
    tall.resize(w, h + u32::from(CELL_PX.1));
    tall.pan(0, i32::from(CELL_PX.1 / 2));
    tall
}

/// Whether the board's region lies under `view` (1476): the fixture is
/// drawn only where the view is in it. Elsewhere the picture is no plain
/// grey but says there is no map there ([`lines`] with `bare`).
pub fn covers(region: &Header, view: &Viewport) -> bool {
    loquax::ui::radar::any_region(core::slice::from_ref(region), view)
}

/// The map screen's cells, `cols` x `rows`: every row blank in the map's
/// ground, which the picture is drawn over. The words of the screen (the
/// hint, the card, the credit) are drawn in pixels after the picture
/// ([`words`]).
///
/// `bare`: no region lies under the view (1476). The picture's rows then
/// say so in the words of 1447 §5, "Keine Karte für diese Gegend", in
/// their second row, above the own pin the view is centred on, and their
/// last row how a region gets onto the board, dark on the light ground.
pub fn lines(look: &Look, bare: bool, cols: usize, rows: usize) -> Vec<RLine> {
    let ground = RStyle {
        bg: Some(ui_ink(style().theme.palette().background)),
        ..RStyle::default()
    };
    let height = usize::from(PICTURE.height);
    let mut out = Vec::with_capacity(rows);
    for at in 0..rows {
        let words = match (bare, at) {
            // Above the middle, where the own pin sits (the view is
            // centred on it).
            (true, 1) => Some((StrId::MapNoRegion, Some(UiInk::Rgb(0x20, 0x20, 0x20)))),
            (true, at) if at + 1 == height => {
                Some((StrId::MapRegionHow, Some(UiInk::Rgb(0x60, 0x60, 0x60))))
            }
            _ => None,
        };
        let mut line = RLine::default();
        if let Some((id, fg)) = words {
            let text = Words::key(id).resolve(look.lang);
            let text = cut(&text, cols);
            let left = cols.saturating_sub(fit_width(&text)) / 2;
            push(&mut line, &" ".repeat(left), ground);
            push(&mut line, &text, RStyle { fg, ..ground });
        }
        out.push(fill(line, cols, ground));
    }
    out
}

/// What the map shows over its picture besides the pins (1531): the key
/// hint in the bottom row, and the credit in the top right corner. The
/// board's [`Chrome`] says which; a fresh look has neither.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    /// The hint's page, [`StrId::MapKeys`] or [`StrId::MapKeysPin`].
    pub hint: Option<StrId>,
    /// The credit in the picture's corner.
    pub credit: bool,
    /// What the receiver says of the sky (1477), in the top left corner:
    /// "Suche Satelliten… 4", then the fix's age. The board's loop sets
    /// it; [`Chrome::overlay`] leaves it off.
    pub sky: crate::gnss::Sky,
}

/// How long a page of the key hint stands, in milliseconds (1531: three
/// seconds after the map opens and after a key it does not know; the
/// pins' page after it for as long, when there is a pin).
pub const HINT_MS: u64 = 3_000;

/// How long the credit stands in the picture's corner the first time
/// the map opens after a boot: five seconds, the least the OSMF's
/// attribution guidelines ("Licence/Attribution Guidelines", interactive
/// maps) allow before an attribution collapses; their startup clause
/// lets it stand once, not on every look.
pub const CREDIT_MS: u64 = 5_000;

/// The board's clock for the map's words (1531): when the map opened,
/// when a key it does not know was pressed, and whether this boot showed
/// the credit yet. The board's loop says what it sees ([`Chrome::seen`],
/// [`Chrome::nudge`]) and draws what [`Chrome::overlay`] says, again
/// whenever that changes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Chrome {
    /// When the hint's three seconds started.
    since: Option<u64>,
    /// Until when the credit stands; set once a boot.
    credit_until: Option<u64>,
    /// The map was shown at the last [`Chrome::seen`].
    open: bool,
}

impl Chrome {
    /// The board's loop at `now_ms`, the map `shown` or not: a map that
    /// just opened starts the hint, and on the boot's first open the
    /// credit.
    pub fn seen(&mut self, shown: bool, now_ms: u64) {
        if shown && !self.open {
            self.since = Some(now_ms);
            if self.credit_until.is_none() {
                self.credit_until = Some(now_ms + CREDIT_MS);
            }
        }
        if !shown {
            self.since = None;
        }
        self.open = shown;
    }

    /// A key the open map does not know, at `now_ms`: the hint again.
    pub fn nudge(&mut self, now_ms: u64) {
        if self.open {
            self.since = Some(now_ms);
        }
    }

    /// What the map shows over its picture at `now_ms`; `pins` says
    /// whether the map has a pin, asked only while the pins' page is due.
    pub fn overlay(&self, now_ms: u64, pins: impl FnOnce() -> bool) -> Overlay {
        if !self.open {
            return Overlay::default();
        }
        let age = self.since.map(|since| now_ms.saturating_sub(since));
        let hint = match age {
            Some(age) if age < HINT_MS => Some(StrId::MapKeys),
            Some(age) if age < 2 * HINT_MS && pins() => Some(StrId::MapKeysPin),
            _ => None,
        };
        Overlay {
            hint,
            credit: self.credit_until.is_some_and(|until| now_ms < until),
            sky: crate::gnss::Sky::Off,
        }
    }
}

/// The data's credit as the M9 shows it (1531): "© OpenStreetMap" for
/// OpenStreetMap's, the form the OSMF's attribution guidelines accept
/// where the full notice has no room; any other credit whole.
pub fn short_credit(attribution: &str) -> &str {
    match attribution.contains("OpenStreetMap") {
        true => "© OpenStreetMap",
        false => attribution,
    }
}

/// `words` on one row of `cols` cells after a leading space; where Lew's
/// words do not fit (the hint's German in the 10 x 20 atlas, 1531), the
/// double spaces between its pairs become single ones, then the leading
/// space goes, and only then are they cut.
pub fn squeeze(words: &str, cols: usize) -> String {
    let spaced = format!(" {words}");
    if fit_width(&spaced) <= cols {
        return spaced;
    }
    let single = words.replace("  ", " ");
    let spaced = format!(" {single}");
    if fit_width(&spaced) <= cols {
        return spaced;
    }
    cut(&single, cols)
}

/// The chosen pin's card (1531): its name and its age at the left, the
/// data's `credit` at the right end. The age and the credit stay whole;
/// the name gets what is left, and is left out when that is less than
/// four cells (the pin on the picture carries it, reversed).
pub fn card(name: &str, age: Option<&str>, credit: Option<&str>, cols: usize) -> String {
    let right = credit.map(|c| format!("{c} ")).unwrap_or_default();
    let age = age.unwrap_or("");
    let room = cols.saturating_sub(fit_width(&right) + 1);
    let whole = match age.is_empty() {
        true => format!(" {name}"),
        false => format!(" {name} · {age}"),
    };
    let left = match fit_width(&whole) <= room {
        true => whole,
        false => {
            let tail = match age.is_empty() {
                true => String::new(),
                false => format!(" · {age}"),
            };
            let left_for_name = room.saturating_sub(1 + fit_width(&tail));
            match (left_for_name >= 4, age.is_empty()) {
                (true, _) => format!(" {}{tail}", cut(name, left_for_name)),
                (false, false) => cut(&format!(" {age}"), room),
                (false, true) => cut(&format!(" {name}"), room),
            }
        }
    };
    let gap = cols.saturating_sub(fit_width(&left) + fit_width(&right));
    format!("{left}{}{right}", " ".repeat(gap))
}

/// The bottom row's words for `model` under `overlay`: the hint while it
/// stands, else the chosen pin's card, with `credit` when data is shown.
/// `None`: the bottom row is the picture's.
pub fn bottom<B: InputBuffer>(
    model: &UiModel<B>,
    overlay: &Overlay,
    credit: Option<&str>,
    lang: Lang,
) -> Option<String> {
    let cols = usize::from(PICTURE.width);
    if let Some(id) = overlay.hint {
        return Some(squeeze(&Words::key(id).resolve(lang), cols));
    }
    let chosen = model.map.as_ref()?.selected?;
    let pins = model.map_pins();
    let pin = pins.iter().find(|p| p.peer == chosen)?;
    Some(card(&pin.label, pin.age.as_deref(), credit, cols))
}

/// The names the label pass sets at most on one screen (1531): eight,
/// the most the 32 x 12 cells hold apart without covering the streets
/// they name.
pub const NAMES_MAX: usize = 8;

/// One name per block of this many cells on the M9 (1531): the
/// terminal's 20 x 6 left three of its 32 x 11 cells' blocks.
pub const NAME_BLOCK: (i32, i32) = (11, 3);

/// From this zoom on the names of parks and waters are set too (1531).
pub const POI_ZOOM: u8 = 15;

/// The shortest straight stretch of a street that carries its name, in
/// pixels (1531): three cells, so a stub between two junctions is not
/// named.
pub const STRETCH_PX: i64 = 30;

/// The straight stretches of a street tried for its name, longest first.
const STRETCHES: usize = 3;

/// What the label pass did, for the board's `[MAP]` line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NameStats {
    /// The names the region holds for the view and its zoom shows.
    pub found: usize,
    /// The names set.
    pub drawn: usize,
}

impl NameStats {
    /// The names found and not set: no room, no straight stretch, or past
    /// the cap.
    pub fn dropped(&self) -> usize {
        self.found.saturating_sub(self.drawn)
    }
}

/// The candidates of `labels` on a picture of `cols` x `rows` cells: a
/// point's name centred on its anchor, a street's or a river's centred on
/// the midpoint of one of its [`STRETCHES`] longest straight stretches,
/// horizontal (the atlas cannot turn text). A name that does not fit the
/// picture whole is no candidate.
fn candidates(labels: &[ViewLabel], cols: i64, rows: i64) -> Vec<Candidate> {
    let sub = leviculum_map::labels::SUBPIXEL;
    let (cw, ch) = (i64::from(CELL_PX.0) * sub, i64::from(CELL_PX.1) * sub);
    let mut out = Vec::new();
    for l in labels {
        let len = fit_width(&l.text) as i64;
        if len == 0 {
            continue;
        }
        let centres: Vec<(i64, i64)> = match l.kind.is_line() {
            false => vec![l.anchor],
            true => {
                let mut stretches: Vec<(i64, (i64, i64))> = l
                    .line
                    .iter()
                    .flat_map(|piece| piece.windows(2))
                    .map(|w| {
                        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
                        let (dx, dy) = ((x1 - x0) / sub, (y1 - y0) / sub);
                        (dx * dx + dy * dy, ((x0 + x1) / 2, (y0 + y1) / 2))
                    })
                    .filter(|(d2, _)| *d2 >= STRETCH_PX * STRETCH_PX)
                    .collect();
                stretches.sort_by_key(|(d2, at)| (core::cmp::Reverse(*d2), *at));
                stretches
                    .into_iter()
                    .take(STRETCHES)
                    .map(|(_, at)| at)
                    .collect()
            }
        };
        for (x, y) in centres {
            let col = x.div_euclid(cw) - len / 2;
            let row = y.div_euclid(ch);
            if col < 0 || col + len > cols || !(0..rows).contains(&row) {
                continue;
            }
            let (Ok(x0), Ok(y0), Ok(n)) =
                (i32::try_from(col), i32::try_from(row), i32::try_from(len))
            else {
                continue;
            };
            out.push(Candidate {
                kind: l.kind,
                rank: l.rank,
                text: l.text.clone(),
                at: l.at,
                boxes: vec![LabelRect {
                    x0,
                    y0,
                    x1: x0 + n,
                    y1: y0 + 1,
                }],
            });
        }
    }
    out
}

/// The names of `view` in `region` placed on the cells of `rows` rows of
/// the picture (1531), by the map core's placement rule
/// (`leviculum_map::labels::place`) in three rounds: places (city, town,
/// village, suburb, as the region's zooms let them in), then streets and
/// rivers on a straight stretch of their line, then from [`POI_ZOOM`]
/// parks and waters. Never over `taken` (the pins and the words over the
/// picture: col, row, width) nor a name of an earlier round, one per
/// [`NAME_BLOCK`] in a round, at most [`NAMES_MAX`] in all.
///
/// With `overview` the world region is what is shown, whose own names are
/// only the places at z3: the world view's names ([`outline::NAMES_M9`],
/// countries, seas, ranges, deserts and its places) join them (1683). A
/// name both tables carry is placed once, by its text.
pub fn names(
    region: &Region<&[u8]>,
    view: &Viewport,
    rows: u16,
    taken: &[(u16, u16, u16)],
    overview: bool,
) -> (Vec<NameCell>, NameStats) {
    let mut found: Vec<ViewLabel> = Vec::new();
    if region.view_labels(view, true, &mut found).is_err() {
        found.clear();
    }
    if overview {
        outline::names(outline::NAMES_M9, view, &mut found);
    }
    let zoom = view.zoom();
    let poi = |k: LabelKind| matches!(k, LabelKind::Park | LabelKind::Water);
    found.retain(|l| zoom >= POI_ZOOM || !poi(l.kind));
    let all = candidates(&found, i64::from(PICTURE.width), i64::from(rows));
    let mut boxes: Vec<LabelRect> = taken
        .iter()
        .map(|&(col, row, width)| LabelRect {
            x0: i32::from(col),
            y0: i32::from(row),
            x1: i32::from(col) + i32::from(width),
            y1: i32::from(row) + 1,
        })
        .collect();
    let rule = Rule {
        margin: 1,
        block: NAME_BLOCK,
        repeat: i32::from(PICTURE.width),
    };
    let rounds: [fn(LabelKind) -> bool; 3] = [
        |k| !k.is_line() && !matches!(k, LabelKind::Park | LabelKind::Water),
        |k| k.is_line(),
        |k| matches!(k, LabelKind::Park | LabelKind::Water),
    ];
    let mut placed: Vec<NameCell> = Vec::new();
    for round in rounds {
        let these: Vec<Candidate> = all.iter().filter(|c| round(c.kind)).cloned().collect();
        let set = place(&these, &boxes, rule);
        for i in set {
            if placed.len() >= NAMES_MAX {
                break;
            }
            let Some(c) = these.get(i) else {
                continue;
            };
            let Some(b) = c.boxes.first() else {
                continue;
            };
            if placed.iter().any(|p| p.text == c.text) {
                continue;
            }
            boxes.push(*b);
            let (Ok(col), Ok(row)) = (u16::try_from(b.x0), u16::try_from(b.y0)) else {
                continue;
            };
            placed.push(NameCell {
                col,
                row,
                text: c.text.clone(),
                kind: c.kind,
            });
        }
    }
    let stats = NameStats {
        found: found.len(),
        drawn: placed.len(),
    };
    (placed, stats)
}

/// The cells `pins` cover, for [`names`]: each marker's text and each
/// name set apart.
pub fn pin_cells(pins: &[PinCell]) -> Vec<(u16, u16, u16)> {
    let width = |t: &str| u16::try_from(fit_width(t)).unwrap_or(u16::MAX);
    pins.iter()
        .flat_map(|p| {
            core::iter::once((p.col, p.row, width(&p.text)))
                .chain(p.apart.as_ref().map(|a| (a.col, a.row, width(&a.text))))
        })
        .collect()
}

/// The ink of a name of `kind` on the light map: water and rivers blue,
/// parks green, every other dark grey (the terminal's light colours).
fn name_ink(kind: LabelKind) -> Rgb565 {
    match kind {
        LabelKind::Water | LabelKind::River | LabelKind::Sea => {
            Rgb888::new(0x1f, 0x4e, 0x8c).into()
        }
        LabelKind::Park => Rgb888::new(0x2e, 0x6b, 0x2e).into(),
        _ => Rgb888::new(0x20, 0x20, 0x20).into(),
    }
}

/// Draw `names` in the atlas, each on a white box as wide as it, so a
/// name stays readable over the streets it crosses.
pub fn draw_names<D>(names: &[NameCell], font: &MonoFont<'_>, panel: &mut D) -> Result<(), MapError>
where
    D: DrawTarget<Color = Rgb565>,
{
    for name in names {
        let style = MonoTextStyleBuilder::new()
            .font(font)
            .text_color(name_ink(name.kind))
            .background_color(Rgb565::WHITE)
            .build();
        let at = Point::new(
            i32::from(name.col * CELL_PX.0),
            i32::from(name.row * CELL_PX.1),
        );
        Text::with_baseline(&crate::screen::narrow(&name.text), at, style, Baseline::Top)
            .draw(panel)
            .map_err(|_| MapError::Draw)?;
    }
    Ok(())
}

/// The words over the picture (1531): `bottom` on the bottom row, and
/// `credit` in the top right corner, both on the bar's colours.
pub fn words<D>(
    bottom: Option<&str>,
    credit: Option<&str>,
    look: &Look,
    font: &MonoFont<'_>,
    panel: &mut D,
) -> Result<(), MapError>
where
    D: DrawTarget<Color = Rgb565>,
{
    let chrome = look.tokens.chrome;
    let style = MonoTextStyleBuilder::new()
        .font(font)
        .text_color(rgb565(chrome.fg))
        .background_color(rgb565_or(chrome.bg, Rgb565::BLACK))
        .build();
    let mut text = |col: usize, row: u16, words: &str| {
        let at = Point::new(
            i32::try_from(col).unwrap_or(0) * i32::from(CELL_PX.0),
            i32::from(row * CELL_PX.1),
        );
        Text::with_baseline(&crate::screen::narrow(words), at, style, Baseline::Top)
            .draw(panel)
            .map(|_| ())
            .map_err(|_| MapError::Draw)
    };
    if let Some(bottom) = bottom {
        let cols = usize::from(PICTURE.width);
        let line = format!(
            "{bottom}{}",
            " ".repeat(cols.saturating_sub(fit_width(bottom)))
        );
        text(0, BOTTOM, &line)?;
    }
    if let Some(credit) = credit {
        let corner = format!(" {credit} ");
        let col = usize::from(PICTURE.width).saturating_sub(fit_width(&corner));
        text(col, 0, &corner)?;
    }
    Ok(())
}

/// The cells the overview's words take ([`notes`]): its second row and
/// its last, whole, for the label pass to keep clear of.
pub fn overview_rows() -> [(u16, u16, u16); 2] {
    [
        (0, 1, PICTURE.width),
        (0, PICTURE.height - 1, PICTURE.width),
    ]
}

/// The words over a picture drawn from the world overview (1477): "Nur
/// Übersicht" in the picture's second row, where "Keine Karte für diese
/// Gegend" stood, and how a region gets onto the board in its last; and
/// the receiver's `sky` words in the top left corner. On the bar's
/// colours, as the other words over the picture.
pub fn notes<D>(
    overview: bool,
    sky: Option<&str>,
    look: &Look,
    font: &MonoFont<'_>,
    panel: &mut D,
) -> Result<(), MapError>
where
    D: DrawTarget<Color = Rgb565>,
{
    let chrome = look.tokens.chrome;
    let style = MonoTextStyleBuilder::new()
        .font(font)
        .text_color(rgb565(chrome.fg))
        .background_color(rgb565_or(chrome.bg, Rgb565::BLACK))
        .build();
    let cols = usize::from(PICTURE.width);
    let mut text = |col: usize, row: u16, words: &str| {
        let at = Point::new(
            i32::try_from(col).unwrap_or(0) * i32::from(CELL_PX.0),
            i32::from(row * CELL_PX.1),
        );
        Text::with_baseline(&crate::screen::narrow(words), at, style, Baseline::Top)
            .draw(panel)
            .map(|_| ())
            .map_err(|_| MapError::Draw)
    };
    if overview {
        let [(_, first, _), (_, last, _)] = overview_rows();
        for (id, row) in [(StrId::MapOverview, first), (StrId::MapRegionHow, last)] {
            let words = format!(" {} ", Words::key(id).resolve(look.lang));
            let words = cut(&words, cols);
            text(cols.saturating_sub(fit_width(&words)) / 2, row, &words)?;
        }
    }
    if let Some(sky) = sky {
        text(0, 0, &cut(&format!(" {sky} "), cols))?;
    }
    Ok(())
}

/// Why the map was not drawn.
#[derive(Debug)]
pub enum MapError {
    /// The region's bytes are not a region, or a tile is corrupt.
    Region(RegionError),
    /// The panel refused a pixel.
    Draw,
}

/// The filler's storage, made once, as large as the window's painter
/// makes it (`loquax::picture`): 217,508 B on the heap, which on the board
/// is PSRAM.
pub struct Painter {
    edges: Vec<Edge>,
    active: Vec<u16>,
    heads: Vec<u16>,
    points: Vec<Point>,
    ring_ends: Vec<u16>,
}

/// The filler's edge slots, and as many active-list slots.
pub const EDGES: usize = 8192;
/// The stroker's points.
pub const POINTS: usize = 8192;
/// The ends of a feature's rings.
pub const RING_ENDS: usize = 2048;

impl Default for Painter {
    fn default() -> Self {
        Self::new()
    }
}

impl Painter {
    /// The storage for a picture of the panel's height, the bottom row
    /// with it ([`with_bottom`]).
    pub fn new() -> Self {
        Self {
            edges: vec![Edge::EMPTY; EDGES],
            active: vec![0; EDGES],
            heads: vec![0; GRID_ROWS * usize::from(CELL_PX.1)],
            points: vec![Point::zero(); POINTS],
            ring_ends: vec![0; RING_ENDS],
        }
    }

    /// The bytes it holds.
    pub fn bytes(&self) -> usize {
        self.edges.len() * core::mem::size_of::<Edge>()
            + (self.active.len() + self.heads.len() + self.ring_ends.len()) * 2
            + self.points.len() * core::mem::size_of::<Point>()
    }

    /// Draw `view` of `region` into `panel`'s picture rows: the
    /// background, then the tiles the region holds there, class by class.
    /// What the filler counted. With `overview` — the world region, which
    /// carries no boundary class — the country borders and the disputed
    /// boundary lines go over it too (1683), from the world outline's
    /// tables.
    pub fn picture<D>(
        &mut self,
        region: &Region<&[u8]>,
        view: &Viewport,
        panel: &mut D,
        overview: bool,
    ) -> Result<RenderStats, MapError>
    where
        D: DrawTarget<Color = Rgb565>,
    {
        let style = style();
        let top = i32::from(PICTURE.y * CELL_PX.1);
        let (w, h) = view.size();
        let whole = Rectangle::new(Point::new(0, top), Size::new(w, h));
        panel
            .fill_solid(&whole, style.theme.palette().background)
            .map_err(|_| MapError::Draw)?;
        let mut target = panel.cropped(&Rectangle::new(Point::new(0, top), Size::new(w, h)));
        let tiles = region.view(view).map_err(MapError::Region)?;
        let mut scratch = Scratch {
            edges: &mut self.edges,
            heads: &mut self.heads,
            active: &mut self.active,
        };
        let mut work = Work {
            points: &mut self.points,
            ring_ends: &mut self.ring_ends,
        };
        let stats = draw_in_scaled(
            &tiles,
            region.area(view),
            Profile::Full,
            Ink::Colour,
            style,
            100,
            &mut target,
            &mut scratch,
            &mut work,
            &mut NoClock,
        )
        .map_err(|_| MapError::Draw)?;
        if overview {
            // The overview has no boundary class, so the countries are
            // parted by the world outline's own lines (1683), drawn over
            // the land the region filled. The scratch is this painter's,
            // freed again by the pass just gone.
            let mut scratch = Scratch {
                edges: &mut self.edges,
                heads: &mut self.heads,
                active: &mut self.active,
            };
            let mut filler = Filler::new(&mut scratch, target.bounding_box());
            let _ = outline::bounds(
                &outline::BOUNDARIES,
                view,
                Point::new(0, top),
                style.theme.palette().border,
                &mut filler,
                &mut target,
            );
        }
        Ok(stats)
    }
}

/// The pins over the picture: each pin's text where the model placed it,
/// in the atlas, on the bar's colours; the chosen one reversed, an old
/// one's and a node's nearby muted.
pub fn pins<D>(
    pins: &[PinCell],
    look: &Look,
    font: &MonoFont<'_>,
    panel: &mut D,
) -> Result<(), MapError>
where
    D: DrawTarget<Color = Rgb565>,
{
    let chrome = look.tokens.chrome;
    let (fg, bg) = (rgb565(chrome.fg), rgb565(chrome.bg));
    let dim = rgb565(look.tokens.muted.fg);
    // Where every text starts, so a text is cut before the next one on
    // its row (1447): two names never overprint each other.
    let starts: Vec<(u16, u16)> = pins
        .iter()
        .flat_map(|p| {
            core::iter::once((p.row, p.col)).chain(p.apart.as_ref().map(|a| (a.row, a.col)))
        })
        .collect();
    let room = |col: u16, row: u16| -> usize {
        let next = starts
            .iter()
            .filter(|(r, c)| *r == row && *c > col)
            .map(|(_, c)| *c)
            .min()
            .unwrap_or(crate::screen::COLS as u16);
        usize::from(next.min(crate::screen::COLS as u16).saturating_sub(col))
    };
    for pin in pins {
        let (fg, bg) = match (pin.selected, pin.stale || pin.nearby) {
            (true, _) => (bg, fg),
            (false, true) => (dim, bg),
            (false, false) => (fg, bg),
        };
        let style = MonoTextStyleBuilder::new()
            .font(font)
            .text_color(fg)
            .background_color(bg)
            .build();
        let mut text = |col: u16, row: u16, words: &str| {
            let at = Point::new(i32::from(col * CELL_PX.0), i32::from(row * CELL_PX.1));
            Text::with_baseline(&crate::screen::narrow(words), at, style, Baseline::Top)
                .draw(panel)
                .map(|_| ())
                .map_err(|_| MapError::Draw)
        };
        text(pin.col, pin.row, &clip(&pin.text, room(pin.col, pin.row)))?;
        if let Some(apart) = &pin.apart {
            text(
                apart.col,
                apart.row,
                &clip(&apart.text, room(apart.col, apart.row)),
            )?;
        }
    }
    Ok(())
}

/// `words` cut to `cols` cells with an ellipsis where they do not fit; a
/// text that fits is drawn as it is.
pub fn clip(words: &str, cols: usize) -> String {
    crate::screen::cut(words, cols.max(1))
}

/// A token's colour on the panel; white where the token has none.
fn rgb565(ink: Option<UiInk>) -> Rgb565 {
    match ink {
        Some(UiInk::Rgb(r, g, b)) => Rgb888::new(r, g, b).into(),
        _ => Rgb565::WHITE,
    }
}

/// A token's colour on the panel; `or` where the token has none.
fn rgb565_or(ink: Option<UiInk>, or: Rgb565) -> Rgb565 {
    match ink {
        Some(UiInk::Rgb(..)) => rgb565(ink),
        _ => or,
    }
}

/// The panel colour `c` as the cells' colour.
fn ui_ink(c: Rgb565) -> UiInk {
    let c = Rgb888::from(c);
    UiInk::Rgb(c.r(), c.g(), c.b())
}

/// The map the board shows: its regions, read in place, and the painter's
/// storage. Made once, beside the renderer.
pub struct Map<'r> {
    /// The first detailed region: the fixture.
    pub region: Region<&'r [u8]>,
    /// The other detailed regions: those written into the flash's region
    /// area (1477, `lnflash --region`).
    pub more: Vec<Region<&'r [u8]>>,
    /// The world overview ([`WORLD`]), where the board carries it.
    pub world: Option<Region<&'r [u8]>>,
    pub painter: Painter,
    /// The picture and its names as last drawn, once the board keeps
    /// them ([`Map::keep_pictures`], 1507); `None` draws every time.
    pub kept: Option<Kept>,
}

/// Which of the board's regions a view draws ([`Map::choose`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drawn {
    /// A detailed region: [`Map::region`] (0) or one of [`Map::more`].
    Detail(usize),
    /// The world overview: no detailed region here.
    Overview,
    /// Nothing: the view is past every region's zooms.
    Bare,
}

impl<'r> Map<'r> {
    /// The map of the region `bytes`.
    pub fn new(bytes: &'r [u8]) -> Result<Self, MapError> {
        Ok(Self {
            region: Region::from_bytes(bytes).map_err(MapError::Region)?,
            more: Vec::new(),
            world: None,
            painter: Painter::new(),
            kept: None,
        })
    }

    /// The board's map (1477): the detailed region `bytes`, the regions
    /// of `more` that read (a region area's, each its own bytes), and the
    /// [`WORLD`] under them.
    pub fn board(bytes: &'r [u8], more: &[&'r [u8]]) -> Result<Self, MapError> {
        let mut map = Self::new(bytes)?;
        map.more = more
            .iter()
            .filter_map(|b| Region::from_bytes(b).ok())
            .collect();
        map.world = Region::from_bytes(WORLD).ok();
        Ok(map)
    }

    /// Every region's header, the detailed ones first: what the model's
    /// map setup holds.
    pub fn headers(&self) -> Vec<Header> {
        core::iter::once(&self.region)
            .chain(self.more.iter())
            .chain(self.world.iter())
            .map(|r| *r.header())
            .collect()
    }

    /// The region `view` draws (1477): the detailed region under it whose
    /// zooms reach the view's, the deepest first; else the world while
    /// its zooms reach (two past its deepest and the map core's overzoom
    /// beyond); else a detailed region under the view at any zoom, as the
    /// board drew before it carried the world; else nothing.
    pub fn choose(&self, view: &Viewport) -> Drawn {
        let details = core::iter::once(&self.region).chain(self.more.iter());
        let under: Vec<(usize, &Region<&'r [u8]>)> = details
            .enumerate()
            .filter(|(_, r)| covers(r.header(), view))
            .collect();
        let useful = under
            .iter()
            .filter(|(_, r)| leviculum_map::choice::useful_zooms(r.header()).contains(&view.zoom()))
            .max_by_key(|(i, r)| (r.header().max_zoom, core::cmp::Reverse(*i)));
        if let Some((i, _)) = useful {
            return Drawn::Detail(*i);
        }
        let world = self.world.as_ref().is_some_and(|w| {
            leviculum_map::choice::useful_zooms(w.header()).contains(&view.zoom())
        });
        if world {
            return Drawn::Overview;
        }
        match under.first() {
            Some((i, _)) => Drawn::Detail(*i),
            None => Drawn::Bare,
        }
    }

    /// The region `drawn` names.
    pub fn of(&self, drawn: Drawn) -> Option<&Region<&'r [u8]>> {
        region_of(&self.region, &self.more, self.world.as_ref(), drawn)
    }

    /// Keep the picture between draws from here on (1507): a page switch
    /// back to the map, a key that moves no pin and a word that comes or
    /// goes (1554) copy it back instead of drawing it. Its bytes, a panel's
    /// worth, on the heap.
    pub fn keep_pictures(&mut self) -> usize {
        let kept = Kept::new();
        let bytes = kept.bytes();
        self.kept = Some(kept);
        bytes
    }

    /// The credit the region carries, which the top bar shows.
    pub fn credit(&self) -> String {
        String::from(self.region.attribution())
    }

    /// The credit of the region `drawn`; empty for none.
    pub fn credit_of(&self, drawn: Drawn) -> String {
        self.of(drawn)
            .map(|r| String::from(r.attribution()))
            .unwrap_or_default()
    }
}

/// The region `drawn` names among a [`Map`]'s fields: for a caller that
/// holds the painter and the kept picture mutably at the same time.
pub fn region_of<'a, 'r>(
    first: &'a Region<&'r [u8]>,
    more: &'a [Region<&'r [u8]>],
    world: Option<&'a Region<&'r [u8]>>,
    drawn: Drawn,
) -> Option<&'a Region<&'r [u8]>> {
    match drawn {
        Drawn::Detail(0) => Some(first),
        Drawn::Detail(i) => more.get(i - 1),
        Drawn::Overview => world,
        Drawn::Bare => None,
    }
}

/// The cells a picture's names keep clear of: column, row, width.
pub type Clear = Vec<(u16, u16, u16)>;

/// The map's picture with its names, as last drawn (1507, move 8 of the
/// 2026-11-07 structure review's M9 list): a panel's worth of RGB565 and
/// what it shows. Its key is the view and the pins' cells (1554); the
/// region is the [`Map`]'s, one for the map's life. The words over the
/// picture (the key hint, the pin's card, the corner credit) are no part
/// of it: they are drawn over the copy, so one that goes shows the
/// picture kept under it. Drawn again only when the key moved: a pan, a
/// zoom, a pin that came or went.
pub struct Kept {
    pixels: Vec<u16>,
    /// The view and the pins' cells of what `pixels` holds.
    holds: Option<(Viewport, Clear)>,
    /// The picture's size.
    size: (u32, u32),
    /// Pictures drawn, and draws that copied a kept picture back instead
    /// of drawing it (a draw's own copy is not counted), since it was made.
    pub drawn: u32,
    pub copied: u32,
}

impl Default for Kept {
    fn default() -> Self {
        Self::new()
    }
}

impl Kept {
    /// Room for a picture as large as the panel, holding none.
    pub fn new() -> Self {
        Self {
            pixels: vec![0; (crate::frame::WIDTH * crate::frame::HEIGHT) as usize],
            holds: None,
            size: (0, 0),
            drawn: 0,
            copied: 0,
        }
    }

    /// The bytes it holds.
    pub fn bytes(&self) -> usize {
        self.pixels.len() * 2
    }

    /// Whether it holds `view` drawn clear of `taken`.
    pub fn holds(&self, view: &Viewport, taken: &[(u16, u16, u16)]) -> bool {
        self.holds
            .as_ref()
            .is_some_and(|(v, t)| v == view && t.as_slice() == taken)
    }

    /// Draw `view` of `map`'s region and its names into the kept pixels,
    /// clear of `taken`, with `font`: what [`Renderer::show`] draws on the
    /// panel without a cache. The label pass's count. `overview` says the
    /// view shows the world overview, which brings the border table and
    /// the world's names (1683).
    ///
    /// [`Renderer::show`]: crate::render::Renderer::show
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        painter: &mut Painter,
        region: &Region<&[u8]>,
        view: &Viewport,
        taken: &[(u16, u16, u16)],
        font: &MonoFont<'_>,
        overview: bool,
        clock: &dyn Fn() -> u64,
    ) -> Result<(NameStats, u64, u64), MapError> {
        self.holds = None;
        self.size = view.size();
        let t0 = clock();
        painter.picture(region, view, self, overview)?;
        let t1 = clock();
        let rows = (view.size().1 / u32::from(CELL_PX.1)) as u16;
        let (names, found) = names(region, view, rows, taken, overview);
        draw_names(&names, font, self)?;
        let t2 = clock();
        self.holds = Some((*view, taken.to_vec()));
        self.drawn = self.drawn.saturating_add(1);
        Ok((found, t1.saturating_sub(t0), t2.saturating_sub(t1)))
    }

    /// Copy the kept picture onto `panel`, where it was drawn.
    pub fn copy_to<D>(&mut self, panel: &mut D) -> Result<(), MapError>
    where
        D: DrawTarget<Color = Rgb565>,
    {
        let (w, h) = self.size;
        let top = i32::from(PICTURE.y * CELL_PX.1);
        let area = Rectangle::new(Point::new(0, top), Size::new(w, h));
        let width = crate::frame::WIDTH as usize;
        let colours = (0..h as usize).flat_map(|y| {
            self.pixels[y * width..y * width + w as usize]
                .iter()
                .map(|&raw| Rgb565::from(embedded_graphics::pixelcolor::raw::RawU16::new(raw)))
        });
        panel
            .fill_contiguous(&area, colours)
            .map_err(|_| MapError::Draw)
            .map(|_| ())
    }
}

impl OriginDimensions for Kept {
    fn size(&self) -> Size {
        Size::new(crate::frame::WIDTH, crate::frame::HEIGHT)
    }
}

/// The kept pixels as the panel's: the picture's rows from [`PICTURE`]'s
/// top, so the painter and the names draw here as they draw there.
impl DrawTarget for Kept {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let top = i32::from(PICTURE.y * CELL_PX.1);
        let width = crate::frame::WIDTH as i32;
        let height = crate::frame::HEIGHT as i32;
        for Pixel(at, colour) in pixels {
            let y = at.y - top;
            if (0..width).contains(&at.x) && (0..height).contains(&y) {
                self.pixels[(y * width + at.x) as usize] =
                    embedded_graphics::pixelcolor::raw::RawU16::from(colour).into_inner();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod words_1531 {
    use super::*;

    /// 1447, 1531: both pages of the key hint fit the bottom row in
    /// either language (the German one with its double spaces single),
    /// and OSM's credit is the form the guidelines accept on a small
    /// screen.
    #[test]
    fn the_hint_fits_the_board_and_the_credit_is_osms_short_form() {
        for lang in [Lang::En, Lang::De] {
            for id in [StrId::MapKeys, StrId::MapKeysPin] {
                let words = Words::key(id).resolve(lang);
                let row = squeeze(&words, COLS);
                assert!(fit_width(&row) <= COLS, "{lang:?} {row}");
                assert!(!row.contains('…'), "never cut: {row}");
            }
        }
        assert_eq!(
            squeeze("T näher  Y weiter  Pfeile schieben", COLS),
            "T näher Y weiter Pfeile schieben"
        );
        assert_eq!(
            squeeze("T in  Y out  arrows pan", COLS),
            " T in  Y out  arrows pan"
        );
        assert_eq!(
            short_credit("© OpenStreetMap contributors"),
            "© OpenStreetMap"
        );
        assert_eq!(short_credit("© Natural Earth"), "© Natural Earth");
        assert_eq!(clip("Jürgen ·4242 · vor 5 Min.", 8), "Jürgen …");
        assert_eq!(clip("Mum", 8), "Mum");
    }

    /// The card keeps the age and the credit whole, the name gets what is
    /// left, and goes when that is under four cells.
    #[test]
    fn the_card_keeps_the_age_and_the_credit() {
        let osm = Some("© OpenStreetMap");
        let full = card("Mum", Some("vor 3 h"), osm, COLS);
        assert_eq!(fit_width(&full), COLS);
        assert!(full.starts_with(" Mum · vor 3 h"), "{full}");
        assert!(full.ends_with("© OpenStreetMap "), "{full}");
        let long = card("Jürgen ·4242", Some("vor 25 Min."), osm, COLS);
        assert_eq!(fit_width(&long), COLS);
        assert!(long.starts_with(" vor 25 Min."), "{long}");
        assert!(long.ends_with("© OpenStreetMap "), "{long}");
        let cut_name = card("Jürgen ·4242", Some("vor 3 h"), osm, COLS);
        assert!(cut_name.starts_with(" Jür"), "{cut_name}");
        assert!(cut_name.contains("· vor 3 h"), "{cut_name}");
        let bare = card("Mum", Some("vor 3 h"), None, COLS);
        assert!(!bare.contains('©'), "{bare}");
    }

    /// The clock: the hint stands three seconds after the map opens, the
    /// pins' page three more when there is a pin, none after; a key the
    /// map does not know brings it back; the credit stands five seconds
    /// on the boot's first open only.
    #[test]
    fn the_hint_and_the_credit_go_by_the_clock() {
        let mut chrome = Chrome::default();
        assert_eq!(chrome.overlay(0, || true), Overlay::default());
        chrome.seen(true, 1_000);
        let at = |c: &Chrome, ms, pins| c.overlay(ms, || pins);
        assert_eq!(
            at(&chrome, 1_000, true),
            Overlay {
                hint: Some(StrId::MapKeys),
                credit: true,
                sky: crate::gnss::Sky::Off,
            }
        );
        assert!(at(&chrome, 5_999, true).credit);
        assert!(!at(&chrome, 6_000, true).credit);
        assert_eq!(at(&chrome, 3_999, true).hint, Some(StrId::MapKeys));
        assert_eq!(at(&chrome, 4_000, true).hint, Some(StrId::MapKeysPin));
        assert_eq!(at(&chrome, 4_000, false).hint, None);
        assert_eq!(at(&chrome, 7_000, true), Overlay::default());
        chrome.nudge(9_000);
        assert_eq!(at(&chrome, 9_500, true).hint, Some(StrId::MapKeys));
        chrome.seen(false, 10_000);
        chrome.seen(true, 20_000);
        assert_eq!(at(&chrome, 20_000, true).hint, Some(StrId::MapKeys));
        assert!(!at(&chrome, 20_000, true).credit, "once a boot");
    }
}
