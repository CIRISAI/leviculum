//! The screen's lines through ratatui into the panel, mousefood the
//! backend: the 2026-10-25 decision (`docs/src/concepts/gui-toolkit.md`,
//! "Decision"), priced by the measurements of 1343
//! (`handheld-messenger.md`, "The renderer, measured").
//!
//! The renderer is made once and lives as long as the panel it borrows:
//! ratatui keeps the last frame's cells and mousefood draws only the cells
//! that changed, so a moved selection redraws two entries, not the screen.
//! The map is the one screen drawn in pixels over the cells
//! ([`crate::map`], slice seven, 1375): [`Renderer::show`] draws whichever
//! the model shows.
//!
//! Two sizes since 1532 ([`Size`]): mousefood sets its atlas when its
//! backend is made and has no way to hand the panel back, so the renderer
//! keeps ratatui's two buffers itself (what `Terminal` keeps) and makes
//! the backend over the panel for each draw, in the size the draw is set
//! in. Diffing against the last frame is as before; a draw in the other
//! size clears the panel and draws whole. Both sizes' atlases and buffers
//! are made with the renderer, so a draw in either keeps nothing on the
//! heap, as before (the census's `draw_held`). The map's cells are always the
//! large size's grid ([`crate::map`]); the reader's size is everything
//! else's.

use embedded_graphics::mono_font::MonoFont;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use leviculum_ui::style::RLine;
use leviculum_ui_ratatui::style::to_ratatui_text;
use loquax::ui::scene::Scene;
use loquax::ui::UiModel;
use mousefood::{
    ColorTheme, CursorConfig, EmbeddedBackend, EmbeddedBackendConfig, TerminalAlignment,
};
use ratatui::backend::Backend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};

use crate::font::Size;

use crate::field::Field;
use crate::map::{self, Map};
use crate::screen::{self, Look};

/// Why a draw did not reach the panel: mousefood could not draw a cell.
#[derive(Debug)]
pub struct DrawError;

/// ratatui over mousefood over the panel `D`.
pub struct Renderer<'d, D>
where
    D: DrawTarget<Color = Rgb565> + 'static,
{
    panel: &'d mut D,
    /// Each size's last frame's cells and the one being drawn, ratatui's
    /// two buffers, in that size's grid; in [`Size::ALL`]'s order.
    grids: [Grid; 3],
    /// The size the panel's cells are in now.
    drawn: Size,
    /// The size the reader chose: everything but the map is set in it.
    size: Size,
    /// Each size's atlas, made once (each making leaks its table).
    fonts: [MonoFont<'static>; 3],
    /// The last draw put the map's pixels over the cells.
    map_drawn: bool,
    /// Where the last draw's screen stood (1533).
    window: screen::Window,
    /// The keys of the chat's bubbles whose last line the last draw put on
    /// screen (1541); none when it drew no chat.
    seen: Option<alloc::vec::Vec<u64>>,
    /// The board's microsecond clock, which times the map's passes for
    /// the `[MAP]` line (1531); none on the host.
    clock: Option<fn() -> u64>,
    /// What the last map draw did; taken by the board's log.
    map_stats: Option<MapStats>,
    /// The names the picture last drawn carries, said again when it is
    /// copied back (1507).
    names_kept: map::NameStats,
    /// Where the last cell draw's time went (1591), by the clock.
    split: DrawSplit,
}

/// Where a cell draw's time went (1591, the board's `[LOOP] core=ui`
/// line), microseconds by [`Renderer::set_clock`]'s clock, 0 without
/// one: the scene built of the model (the list's rows and the chat's
/// bubbles, `rows_us`), its lines laid into the grid's cells
/// (`layout_us`: the words fitted, the styles, ratatui's paragraph), and
/// the cells that changed drawn into the frame (`blit_us`: the diff and
/// mousefood's glyphs). The flush to the panel is the board's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrawSplit {
    pub scene_us: u64,
    pub layout_us: u64,
    pub blit_us: u64,
}

/// One size's two buffers: the last frame's cells and the next one's.
struct Grid {
    last: Buffer,
    next: Buffer,
}

impl Grid {
    fn of(size: Size) -> Self {
        let (cols, rows) = size.grid();
        let area = Rect::new(0, 0, cols as u16, rows as u16);
        Self {
            last: Buffer::empty(area),
            next: Buffer::empty(area),
        }
    }
}

/// One map draw, for the board's `[MAP]` line (1531): the zoom, the
/// picture's and the label pass's microseconds (0 without a clock), and
/// the names the pass found and set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MapStats {
    pub zoom: u8,
    pub picture_us: u64,
    pub names_us: u64,
    pub names: map::NameStats,
    /// The picture was copied back from [`Map::kept`] (1507), not drawn.
    pub kept: bool,
}

impl<'d, D> Renderer<'d, D>
where
    D: DrawTarget<Color = Rgb565> + 'static,
{
    /// A renderer over `panel`, in [`Size::DEFAULT`]. The panel's frame
    /// is cleared to the ground here, in memory: the cells draw only what
    /// differs from the last frame, which starts blank, so whatever the
    /// frame held (the power-on splash, 1485) would stay under every blank
    /// cell. The panel keeps the splash until the first redraw flushes.
    pub fn new(panel: &'d mut D) -> Result<Self, DrawError> {
        let mut renderer = Self {
            panel,
            grids: Size::ALL.map(Grid::of),
            drawn: Size::DEFAULT,
            size: Size::DEFAULT,
            fonts: Size::ALL.map(Size::atlas),
            map_drawn: false,
            window: screen::Window::default(),
            seen: None,
            clock: None,
            map_stats: None,
            names_kept: map::NameStats::default(),
            split: DrawSplit::default(),
        };
        renderer.set_grid(Size::DEFAULT)?;
        Ok(renderer)
    }

    /// The size the reader chose (1532).
    pub fn size(&self) -> Size {
        self.size
    }

    /// Set what follows in `size`: the next draw that is no map's.
    pub fn set_size(&mut self, size: Size) {
        self.size = size;
    }

    /// The atlas of `size`.
    fn font(&self, size: Size) -> MonoFont<'static> {
        self.fonts[size.index()]
    }

    /// mousefood over the panel in `size`'s atlas, for one draw. Every
    /// field named: the config's `Default` would build (and leak)
    /// mousefood's own 6 x 10 atlas and keep its glyphs in the image.
    fn backend(&mut self, size: Size) -> EmbeddedBackend<'_, D, Rgb565> {
        let config = EmbeddedBackendConfig {
            flush_callback: alloc::boxed::Box::new(|_| {}),
            font_regular: self.font(size),
            font_bold: None,
            font_italic: None,
            vertical_alignment: TerminalAlignment::Start,
            horizontal_alignment: TerminalAlignment::Start,
            color_theme: ColorTheme::default(),
            cursor: CursorConfig::default(),
        };
        EmbeddedBackend::new(&mut *self.panel, config)
    }

    /// The panel in `size`'s grid: cleared to the ground and the grid's
    /// buffers blank, so the next draw draws every cell that is not blank.
    fn set_grid(&mut self, size: Size) -> Result<(), DrawError> {
        self.backend(size).clear().map_err(|_| DrawError)?;
        let grid = &mut self.grids[size.index()];
        grid.last.reset();
        grid.next.reset();
        self.drawn = size;
        Ok(())
    }

    /// Draw `scene` as [`screen::lines`] words it, at the grid's size.
    pub fn draw(&mut self, scene: &Scene, look: &Look) -> Result<(), DrawError> {
        self.leave_map()?;
        let mut window = screen::Window::default();
        let mut seen = None;
        self.seen = None;
        self.cells(|cols, rows| {
            let (lines, at, keys) = screen::lines_seen(scene, look, cols, rows);
            window = at;
            seen = keys;
            lines
        })?;
        self.window = window;
        self.seen = seen;
        Ok(())
    }

    /// Where the last draw's screen stood in what it shows (1533): the
    /// list's rows, the chat's topmost bubble; nothing for the map and
    /// the board's own screens.
    pub fn window(&self) -> screen::Window {
        self.window
    }

    /// The keys of the open chat's bubbles whose last line the last draw
    /// showed (1541), what the board says as the model's viewport
    /// ([`crate::read::Reader::say`]); none when the last draw showed no
    /// chat: the list, a sheet, the map, the board's own screens.
    pub fn seen(&self) -> Option<&[u64]> {
        self.seen.as_deref()
    }

    /// Time the map's passes with `clock`, microseconds since any start.
    pub fn set_clock(&mut self, clock: fn() -> u64) {
        self.clock = Some(clock);
    }

    /// Where the last cell draw's time went (1591), once.
    pub fn take_split(&mut self) -> DrawSplit {
        core::mem::take(&mut self.split)
    }

    /// The scene of the next cell draw took `us` to build: for a caller
    /// that builds it itself before [`Renderer::draw`] (the bench's
    /// `[OPEN]` split).
    pub fn scene_took(&mut self, us: u64) {
        self.split.scene_us = us;
    }

    /// The clock's microseconds now, 0 without one.
    fn now(&self) -> u64 {
        self.clock.map_or(0, |clock| clock())
    }

    /// What the last map draw did, once: the board says it in a `[MAP]`
    /// line.
    pub fn take_map_stats(&mut self) -> Option<MapStats> {
        self.map_stats.take()
    }

    /// Draw what `model` shows: the map in pixels when it is open
    /// ([`map::shown`]), else the scene of it ([`screen::scene`]) in cells.
    pub fn show(
        &mut self,
        model: &UiModel<Field>,
        look: &Look,
        map: &mut Map<'_>,
    ) -> Result<(), DrawError>
    where
        D: leviculum_mapfill::coverage::Coverage,
    {
        let Some((view, pins)) = map::shown(model).then(|| map::view(model)).flatten() else {
            let t0 = self.now();
            let scene = screen::scene(model);
            self.split.scene_us = self.now().saturating_sub(t0);
            return self.draw(&scene, look);
        };
        // The region the view draws (1477): a detailed one, else the
        // world overview, else none.
        let drawn = map.choose(&view);
        let attribution = map.credit_of(drawn);
        let font = self.font(Size::Large);
        // No region under the view (1476): the cells say so, no picture,
        // and no credit, as nobody's data is shown.
        let bare = drawn == map::Drawn::Bare;
        let credit = (!bare).then(|| map::short_credit(&attribution));
        let bottom = map::bottom(model, &look.map, credit, look.lang);
        // The picture always takes the bottom row (1554): the hint and the
        // card are drawn over it, so a word that comes or goes leaves the
        // picture as it is and only the copy shows what was under it.
        let painted = map::with_bottom(&view);
        let corner = credit.filter(|_| look.map.credit);
        self.window = screen::Window::default();
        self.seen = None;
        // The map's cells are the large size's grid, whatever the reader
        // chose: its picture, pins and names are laid on that grid (1531).
        self.cells_in(Size::Large, |cols, rows| map::lines(look, bare, cols, rows))?;
        let clock = self.clock;
        let now = || clock.map_or(0, |c| c());
        let panel = &mut *self.panel;
        let mut stats = MapStats {
            zoom: view.zoom(),
            ..MapStats::default()
        };
        // The names where no pin is (1531). The words (the hint, the card,
        // the corner credit) are overlays (1554): drawn over the picture
        // after it, never part of its key, so a word that goes uncovers
        // the picture kept under it.
        let mut taken = map::pin_cells(&pins);
        // The overview's two rows of words (1477) keep names off them.
        if drawn == map::Drawn::Overview {
            taken.extend(map::overview_rows());
        }
        // The fields apart: the region is read while the painter and the
        // kept picture are written.
        let map::Map {
            region: first,
            more,
            world,
            painter,
            glyphs,
            kept,
        } = map;
        let region = map::region_of(first, more, world.as_ref(), drawn);
        match (kept, region) {
            (_, None) => {}
            // The kept picture (1507, 1554): drawn when the view or the
            // pins moved, copied back every time.
            (Some(kept), Some(region)) => {
                if kept.holds(&painted, &taken) {
                    kept.copied = kept.copied.saturating_add(1);
                    stats.kept = true;
                    stats.names = self.names_kept;
                } else {
                    let (found, picture_us, names_us) = kept
                        .draw(painter, glyphs, region, &painted, &taken, &now)
                        .map_err(|_| DrawError)?;
                    stats.picture_us = picture_us;
                    stats.names_us = names_us;
                    stats.names = found;
                }
                kept.copy_to(panel).map_err(|_| DrawError)?;
            }
            (None, Some(region)) => {
                let t0 = now();
                painter
                    .picture(region, &painted, panel)
                    .map_err(|_| DrawError)?;
                let t1 = now();
                let (names, found) = map::names(region, &painted, &taken, glyphs);
                map::draw_names(&names, panel);
                let t2 = now();
                stats.picture_us = t1.saturating_sub(t0);
                stats.names_us = t2.saturating_sub(t1);
                stats.names = found;
            }
        }
        map::pins(&pins, look, &font, panel).map_err(|_| DrawError)?;
        map::words(bottom.as_deref(), corner, look, &font, panel).map_err(|_| DrawError)?;
        let sky = crate::gnss::sky_words(look.map.sky, look.lang);
        map::notes(
            drawn == map::Drawn::Overview,
            sky.as_deref(),
            look,
            &font,
            panel,
        )
        .map_err(|_| DrawError)?;
        self.names_kept = stats.names;
        self.map_stats = Some(stats);
        self.map_drawn = true;
        Ok(())
    }

    /// Draw `lines`, made for the grid's size: a screen of the M9's own
    /// that is no scene, the pairing (1415).
    pub fn text(
        &mut self,
        lines: impl FnOnce(usize, usize) -> alloc::vec::Vec<RLine>,
    ) -> Result<(), DrawError> {
        self.leave_map()?;
        self.window = screen::Window::default();
        self.seen = None;
        self.cells(lines)
    }

    /// Cells drawn after the map's pixels must all be drawn, not only those
    /// the last frame saw change: it holds the picture's rows blank.
    fn leave_map(&mut self) -> Result<(), DrawError> {
        if self.map_drawn {
            self.set_grid(self.drawn)?;
            self.map_drawn = false;
        }
        Ok(())
    }

    /// The lines `lines` makes for the reader's size's grid.
    fn cells(
        &mut self,
        lines: impl FnOnce(usize, usize) -> alloc::vec::Vec<RLine>,
    ) -> Result<(), DrawError> {
        self.cells_in(self.size, lines)
    }

    /// The lines `lines` makes for `size`'s grid, drawn as ratatui's
    /// `Terminal::draw` does: rendered into the next buffer, only the
    /// cells that differ from the last one sent to the panel, the two
    /// swapped. A grid other than the last draw's starts from a cleared
    /// panel.
    fn cells_in(
        &mut self,
        size: Size,
        lines: impl FnOnce(usize, usize) -> alloc::vec::Vec<RLine>,
    ) -> Result<(), DrawError> {
        if self.drawn != size {
            self.set_grid(size)?;
        }
        let at = size.index();
        let area = self.grids[at].next.area;
        let t0 = self.now();
        let lines = lines(area.width.into(), area.height.into());
        Paragraph::new(to_ratatui_text(&lines)).render(area, &mut self.grids[at].next);
        let t1 = self.now();
        // The backend borrows the panel and the atlas, the diff the grid:
        // the grid is moved out for the draw and back after.
        let Grid { last, next } = core::mem::replace(
            &mut self.grids[at],
            Grid {
                last: Buffer::default(),
                next: Buffer::default(),
            },
        );
        let drawn = {
            let updates = last.diff(&next);
            let mut backend = self.backend(size);
            backend
                .draw(updates.into_iter())
                .and_then(|()| backend.flush())
        };
        let mut spare = last;
        spare.reset();
        self.grids[at] = Grid {
            last: next,
            next: spare,
        };
        let t2 = self.now();
        self.split.layout_us = t1.saturating_sub(t0);
        self.split.blit_us = t2.saturating_sub(t1);
        drawn.map_err(|_| DrawError)
    }

    /// The panel, to read back what was drawn.
    pub fn panel(&self) -> &D {
        self.panel
    }
}
