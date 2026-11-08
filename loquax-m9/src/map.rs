//! The map on the M9 (slice seven, 1375): the model's map, drawn in pixels
//! under the top bar, the pins and the legend in the screen's own atlas.
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
//! A draw is the cells first, the bar and the legend with the picture's
//! rows blank, then the map over those rows, then the pins. ratatui draws
//! only cells that changed, so the next map's blank rows are left alone
//! and the map is drawn over them again; the renderer clears its cells
//! when it leaves the map, so a list drawn after it redraws every cell
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
use leviculum_map::region::{Header, Region, RegionError};
use leviculum_map::viewport::Viewport;
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::render::{NoClock, RenderStats, Work};
use leviculum_mapfill::style::{Profile, Style};
use leviculum_ui::color::Ink as UiInk;
use leviculum_ui::input::{InputBuffer, KeyCode, KeyEvent, KeyModifiers};
use leviculum_ui::style::{RLine, RStyle};
use leviculum_ui::text::fit_width;
use leviculum_ui::Rect;
use loquax::keys::MapCmd;
use loquax::ui::map::{self as core_map, MapSetup, PinCell};
use loquax::ui::strings::{StrId, Text as Words};
use loquax::ui::{Pane, UiModel};

use crate::keys::M9Key;
use crate::screen::{cut, fill, muted, painted, push, Look, COLS, ROWS as GRID_ROWS};

/// The pixels of one cell of the atlas ([`crate::font`]): what the model
/// scales the picture's cells by, so its viewport is the picture's pixels.
pub use crate::font::CELL_PX;

/// The picture's cells: the whole width under the top bar, above the
/// legend and the keys. 32 x 9 cells, 320 x 180 pixels.
pub const PICTURE: Rect = Rect::new(0, 1, COLS as u16, (GRID_ROWS - 3) as u16);

/// The region the board shows before it has one of its own (1375): the
/// map core's Florence fixture, zooms 11 to 15, 197,972 B, read in place
/// from the image's flash. OpenStreetMap data under the ODbL, its notice
/// in the file and in `leviculum-map/fixtures/README.md`.
pub const FIXTURE: &[u8] = include_bytes!("../../leviculum-map/fixtures/firenze.lmr");

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
    MapSetup {
        cell_px: CELL_PX,
        zooms: (header.min_zoom, header.max_zoom.saturating_add(OVERZOOM)),
        regions: vec![*header],
        drawings: Vec::new(),
        drawing: 0,
    }
}

/// Tell `model` the board's map: the region the image carries
/// ([`FIXTURE`]) and the picture's cells. The fixture's model and a home's
/// (1476) both, so a home's map is the same picture under the same keys.
pub fn ready<B>(model: &mut UiModel<B>)
where
    B: InputBuffer + Default,
{
    if let Ok(region) = Region::from_bytes(FIXTURE) {
        loquax::ui::update(model, loquax::ui::UiEvent::MapSetup(setup(region.header())));
    }
    model.map_picture = Some(PICTURE);
}

/// Whether `model` shows the map.
pub fn shown<B>(model: &UiModel<B>) -> bool {
    model.focus == Pane::Map && model.map.is_some()
}

/// The map's key table: the verb, the M9 key, and the terminal's command
/// it runs once [`key`] made it the terminal's key. Every row is proven
/// through the shared resolver (`tests/map.rs`).
pub const ROWS: [(&str, M9Key, MapCmd); 11] = [
    ("pan west", M9Key::Left, MapCmd::Pan(-1, 0)),
    ("pan east", M9Key::Right, MapCmd::Pan(1, 0)),
    ("pan north", M9Key::Up, MapCmd::Pan(0, -1)),
    ("pan south", M9Key::Down, MapCmd::Pan(0, 1)),
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
/// Tab and Shift-Tab. Every other key is the terminal's already (the
/// arrows pan, `+` and `-` zoom, `0` fits, Enter opens, Back closes).
pub fn key<B>(model: &UiModel<B>, key: KeyEvent) -> KeyEvent {
    if !shown(model) {
        return key;
    }
    match key.code {
        KeyCode::Char(' ') => KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        KeyCode::Backspace => KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        _ => key,
    }
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

/// Whether the board's region lies under `view` (1476): the fixture is
/// drawn only where the view is in it. Elsewhere the picture is no plain
/// grey but says there is no map there ([`lines`] with `bare`).
pub fn covers(region: &Header, view: &Viewport) -> bool {
    loquax::ui::radar::any_region(core::slice::from_ref(region), view)
}

/// The map screen's cells, `cols` x `rows`: the top bar (the title and
/// the zoom, the data's credit at its right end, which the ODbL asks for
/// on the view), the picture's rows blank in the map's background, the
/// legend and the keys. The chosen pin is told apart on the picture, not
/// in the bar: its name would leave the credit no room.
///
/// `bare`: no region lies under the view (1476). The picture's rows then
/// say so in the words of 1447 §5, "Keine Karte für diese Gegend", in
/// their second row, above the own pin the view is centred on, and their
/// last row how a region gets onto the board; no
/// credit, as no data of anybody's is shown.
pub fn lines<B: InputBuffer>(
    model: &UiModel<B>,
    look: &Look,
    attribution: &str,
    bare: bool,
    cols: usize,
    rows: usize,
) -> Vec<RLine> {
    let chrome = painted(look.tokens.chrome);
    // The zoom only under the debug overlay (1447, walk 1 finding 23).
    let title = match (model.debug_overlay, model.map.as_ref()) {
        (true, Some(state)) => format!(
            " {} z{} ",
            Words::key(StrId::Map).resolve(look.lang),
            state.zoom
        ),
        _ => format!(" {} ", Words::key(StrId::Map).resolve(look.lang)),
    };
    let credit = match bare {
        true => String::new(),
        false => format!(
            "{} ",
            cut(
                short_credit(attribution),
                cols.saturating_sub(fit_width(&title) + 1)
            )
        ),
    };
    let gap = cols.saturating_sub(fit_width(&title) + fit_width(&credit));
    let mut bar = RLine::default();
    push(
        &mut bar,
        &title,
        RStyle {
            bold: true,
            ..chrome
        },
    );
    push(&mut bar, &" ".repeat(gap), chrome);
    push(&mut bar, &credit, painted(look.tokens.chrome_muted));
    let mut out = vec![fill(bar, cols, chrome)];
    let ground = RStyle {
        bg: Some(ui_ink(style().theme.palette().background)),
        ..RStyle::default()
    };
    let height = usize::from(PICTURE.height);
    for at in 0..height {
        let words = match (bare, at) {
            // Above the middle, where the own pin sits (the view is
            // centred on it).
            (true, 1) => Some((StrId::MapNoRegion, chrome.fg)),
            (true, at) if at + 1 == height => Some((StrId::MapRegionHow, muted(look).fg)),
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
    // The keys in two pages that each fit (1447): the board turns the
    // page every KEYS_PAGE_TICKS ticks.
    let keys = match (model.tick / KEYS_PAGE_TICKS) % 2 {
        0 => StrId::MapKeys,
        _ => StrId::MapKeysPin,
    };
    for (id, style) in [(StrId::MapLegend, muted(look)), (keys, chrome)] {
        let mut line = RLine::default();
        push(
            &mut line,
            &format!(" {}", cut(&Words::key(id).resolve(look.lang), cols - 1)),
            style,
        );
        out.push(fill(line, cols, style));
    }
    out.truncate(rows);
    while out.len() < rows {
        out.push(fill(RLine::default(), cols, RStyle::default()));
    }
    out
}

/// The ticks one page of the map's key help stays (1447).
pub const KEYS_PAGE_TICKS: u64 = 8;

/// The data's credit as the M9's bar has room for it (1447, walk 1
/// finding 23): "© OSM" for OpenStreetMap's, which the bar cut to
/// "© OpenStreetMap contr…"; any other credit whole, cut where it must be.
pub fn short_credit(attribution: &str) -> &str {
    match attribution.contains("OpenStreetMap") {
        true => "© OSM",
        false => attribution,
    }
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
    /// The storage for a picture of [`PICTURE`]'s height.
    pub fn new() -> Self {
        Self {
            edges: vec![Edge::EMPTY; EDGES],
            active: vec![0; EDGES],
            heads: vec![0; usize::from(PICTURE.height * CELL_PX.1)],
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
    /// What the filler counted.
    pub fn picture<D>(
        &mut self,
        region: &Region<&[u8]>,
        view: &Viewport,
        panel: &mut D,
    ) -> Result<RenderStats, MapError>
    where
        D: DrawTarget<Color = Rgb565>,
    {
        let style = style();
        let top = i32::from(PICTURE.y * CELL_PX.1);
        let height = u32::from(PICTURE.height * CELL_PX.1);
        let whole = Rectangle::new(
            Point::new(0, top),
            Size::new(panel.bounding_box().size.width, height),
        );
        panel
            .fill_solid(&whole, style.theme.palette().background)
            .map_err(|_| MapError::Draw)?;
        let (w, h) = view.size();
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
        draw_in_scaled(
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
        .map_err(|_| MapError::Draw)
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

/// The panel colour `c` as the cells' colour.
fn ui_ink(c: Rgb565) -> UiInk {
    let c = Rgb888::from(c);
    UiInk::Rgb(c.r(), c.g(), c.b())
}

/// The map the board shows: its region, read in place, and the painter's
/// storage. Made once, beside the renderer.
pub struct Map<'r> {
    pub region: Region<&'r [u8]>,
    pub painter: Painter,
}

impl<'r> Map<'r> {
    /// The map of the region `bytes`.
    pub fn new(bytes: &'r [u8]) -> Result<Self, MapError> {
        Ok(Self {
            region: Region::from_bytes(bytes).map_err(MapError::Region)?,
            painter: Painter::new(),
        })
    }

    /// The credit the region carries, which the top bar shows.
    pub fn credit(&self) -> String {
        String::from(self.region.attribution())
    }
}

#[cfg(test)]
mod words_1447 {
    use super::*;
    use loquax::ui::strings::Lang;

    /// 1447: the legend and both pages of the keys fit the board's line
    /// after its leading space in either language, and OSM's credit is
    /// the short one.
    #[test]
    fn the_map_lines_fit_the_board_and_the_credit_is_short() {
        for lang in [Lang::En, Lang::De] {
            for id in [StrId::MapLegend, StrId::MapKeys, StrId::MapKeysPin] {
                let words = Words::key(id).resolve(lang);
                assert!(fit_width(&words) < crate::screen::COLS, "{lang:?} {words}");
            }
        }
        assert_eq!(short_credit("© OpenStreetMap contributors"), "© OSM");
        assert_eq!(short_credit("© Natural Earth"), "© Natural Earth");
        assert_eq!(clip("Jürgen ·4242 · vor 5 Min.", 8), "Jürgen …");
        assert_eq!(clip("Mum", 8), "Mum");
    }
}
