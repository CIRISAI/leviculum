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
use mousefood::{EmbeddedBackend, EmbeddedBackendConfig};
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
}

impl<'d, D> Renderer<'d, D>
where
    D: DrawTarget<Color = Rgb565> + 'static,
{
    /// A renderer over `panel`, in mousefood's Unicode 6 x 10 atlas: 53 x 24
    /// cells on the M9's 320 x 240.
    pub fn new(panel: &'d mut D) -> Result<Self, DrawError> {
        let config = EmbeddedBackendConfig::default();
        // The atlas leaks each time it is made; the config's is the one.
        let font = config.font_regular;
        let backend = EmbeddedBackend::new(panel, config);
        let terminal = Terminal::new(backend).map_err(|_| DrawError)?;
        Ok(Self {
            terminal,
            font,
            map_drawn: false,
        })
    }

    /// Draw `scene` as [`screen::lines`] words it, at the grid's size.
    pub fn draw(&mut self, scene: &Scene, look: &Look) -> Result<(), DrawError> {
        self.leave_map()?;
        self.cells(|cols, rows| screen::lines(scene, look, cols, rows))
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
        self.cells(|cols, rows| map::lines(model, look, &credit, cols, rows))?;
        let panel = self.terminal.backend_mut().display_mut();
        map.painter
            .picture(&map.region, &view, panel)
            .map_err(|_| DrawError)?;
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
