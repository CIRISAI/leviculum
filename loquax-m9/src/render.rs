//! The screen's lines through ratatui into the panel, mousefood the
//! backend: the 2026-10-25 decision (`docs/src/concepts/gui-toolkit.md`,
//! "Decision"), priced by the measurements of 1343
//! (`handheld-messenger.md`, "The renderer, measured").
//!
//! The renderer is made once and lives as long as the panel it borrows:
//! ratatui keeps the last frame's cells and mousefood draws only the cells
//! that changed, so a moved selection redraws two entries, not the screen.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use leviculum_ui_ratatui::style::to_ratatui_text;
use loquax::ui::scene::Scene;
use mousefood::{EmbeddedBackend, EmbeddedBackendConfig};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

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
}

impl<'d, D> Renderer<'d, D>
where
    D: DrawTarget<Color = Rgb565> + 'static,
{
    /// A renderer over `panel`, in mousefood's Unicode 6 x 10 atlas: 53 x 24
    /// cells on the M9's 320 x 240.
    pub fn new(panel: &'d mut D) -> Result<Self, DrawError> {
        let backend = EmbeddedBackend::new(panel, EmbeddedBackendConfig::default());
        let terminal = Terminal::new(backend).map_err(|_| DrawError)?;
        Ok(Self { terminal })
    }

    /// Draw `scene` as [`screen::lines`] words it, at the grid's size.
    pub fn draw(&mut self, scene: &Scene, look: &Look) -> Result<(), DrawError> {
        self.terminal
            .draw(|frame| {
                let area = frame.area();
                let lines = screen::lines(scene, look, area.width.into(), area.height.into());
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
