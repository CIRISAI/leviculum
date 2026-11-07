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
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

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
    terminal: Terminal<EmbeddedBackend<'d, D, Rgb565>>,
    /// The atlas the cells are set in, for the map's pins.
    font: MonoFont<'static>,
    /// The last draw put the map's pixels over the cells.
    map_drawn: bool,
    /// Where the last draw's screen stood (1533).
    window: screen::Window,
}

impl<'d, D> Renderer<'d, D>
where
    D: DrawTarget<Color = Rgb565> + 'static,
{
    /// A renderer over `panel`, in the 10 x 20 atlas ([`crate::font`]):
    /// 32 x 12 cells on the M9's 320 x 240.
    pub fn new(panel: &'d mut D) -> Result<Self, DrawError> {
        // Every field named: the config's `Default` would build (and leak)
        // mousefood's own 6 x 10 atlas and keep its glyphs in the image.
        let config = EmbeddedBackendConfig {
            flush_callback: alloc::boxed::Box::new(|_| {}),
            font_regular: crate::font::atlas(),
            font_bold: None,
            font_italic: None,
            vertical_alignment: TerminalAlignment::Start,
            horizontal_alignment: TerminalAlignment::Start,
            color_theme: ColorTheme::default(),
            cursor: CursorConfig::default(),
        };
        // The atlas leaks each time it is made; the config's is the one.
        let font = config.font_regular;
        let backend = EmbeddedBackend::new(panel, config);
        let mut terminal = Terminal::new(backend).map_err(|_| DrawError)?;
        // ratatui draws only the cells that differ from its last frame,
        // which starts blank: whatever the panel's frame held (the
        // power-on splash, 1485) would stay under every blank cell. The
        // frame is cleared to the ground here, in memory; the panel keeps
        // the splash until the first redraw flushes.
        terminal.clear().map_err(|_| DrawError)?;
        Ok(Self {
            terminal,
            font,
            map_drawn: false,
            window: screen::Window::default(),
        })
    }

    /// Draw `scene` as [`screen::lines`] words it, at the grid's size.
    pub fn draw(&mut self, scene: &Scene, look: &Look) -> Result<(), DrawError> {
        self.leave_map()?;
        let mut window = screen::Window::default();
        self.cells(|cols, rows| {
            let (lines, at) = screen::lines_at(scene, look, cols, rows);
            window = at;
            lines
        })?;
        self.window = window;
        Ok(())
    }

    /// Where the last draw's screen stood in what it shows (1533): the
    /// list's rows, the chat's topmost bubble; nothing for the map and
    /// the board's own screens.
    pub fn window(&self) -> screen::Window {
        self.window
    }

    /// Draw what `model` shows: the map in pixels when it is open
    /// ([`map::shown`]), else the scene of it ([`screen::scene`]) in cells.
    pub fn show(
        &mut self,
        model: &UiModel<Field>,
        look: &Look,
        map: &mut Map<'_>,
    ) -> Result<(), DrawError> {
        let Some((view, pins)) = map::shown(model).then(|| map::view(model)).flatten() else {
            return self.draw(&screen::scene(model), look);
        };
        let credit = map.credit();
        // No region under the view (1476): the cells say so, no picture.
        let bare = !map::covers(map.region.header(), &view);
        self.window = screen::Window::default();
        self.cells(|cols, rows| map::lines(model, look, &credit, bare, cols, rows))?;
        let panel = self.terminal.backend_mut().display_mut();
        if !bare {
            map.painter
                .picture(&map.region, &view, panel)
                .map_err(|_| DrawError)?;
        }
        map::pins(&pins, look, &self.font, panel).map_err(|_| DrawError)?;
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
        self.cells(lines)
    }

    /// Cells drawn after the map's pixels must all be drawn, not only those
    /// ratatui saw change: its last frame holds the picture's rows blank.
    fn leave_map(&mut self) -> Result<(), DrawError> {
        if self.map_drawn {
            self.terminal.clear().map_err(|_| DrawError)?;
            self.map_drawn = false;
        }
        Ok(())
    }

    /// The lines `lines` makes for the grid's size, through ratatui.
    fn cells(
        &mut self,
        lines: impl FnOnce(usize, usize) -> alloc::vec::Vec<RLine>,
    ) -> Result<(), DrawError> {
        self.terminal
            .draw(|frame| {
                let area = frame.area();
                let lines = lines(area.width.into(), area.height.into());
                frame.render_widget(Paragraph::new(to_ratatui_text(&lines)), area);
            })
            .map(|_| ())
            .map_err(|_| DrawError)
    }

    /// The panel, to read back what was drawn.
    pub fn panel(&self) -> &D {
        self.terminal.backend().display()
    }
}
