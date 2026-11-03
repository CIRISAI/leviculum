//! What a screen costs, measured where it is drawn, and the budget it must
//! keep (slice four, 1361). The ceilings are the plan's
//! (`handheld-messenger.md`, "Memory budgets, measured", and the tables of
//! slices one and two): about double the 64-bit host's figures over the
//! ugly store, asserted there by `loquax-android/tests/m9_scene_census.rs`.
//! The board measures the same two steps of the same two screens with its
//! own allocator and stack and holds them to the same numbers, the way the
//! nRF heap holds its budget (`leviculum-nrf/heap-budget`); the host does
//! it over the fixture in `tests/firmware.rs`. One routine, [`measure`],
//! and one budget table, so the two cannot drift.

use core::fmt;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::DrawTarget;
use loquax::ui::UiModel;

use crate::field::Field;
use crate::render::{DrawError, Renderer};
use crate::screen::{self, Look};

/// What one step cost: the heap it left behind, its heap peak above where
/// it began, and the stack it took below where it began.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub held: isize,
    pub peak: usize,
    pub stack: usize,
}

/// The platform's half: its allocator's counts and its stack.
pub trait Probe {
    /// Begin a step: note the live heap, reset the peak to it, paint the
    /// stack below the caller.
    fn begin(&mut self);
    /// End the step [`Probe::begin`] began.
    fn end(&mut self) -> Step;
}

/// One screen's two steps: building its scene from the model, and drawing
/// that scene with a renderer already made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Census {
    pub scene: Step,
    pub draw: Step,
}

/// Build what `model` shows and draw it, each step measured by `probe`.
pub fn measure<D>(
    probe: &mut dyn Probe,
    model: &UiModel<Field>,
    renderer: &mut Renderer<'_, D>,
) -> Result<Census, DrawError>
where
    D: DrawTarget<Color = Rgb565> + 'static,
{
    probe.begin();
    let scene = step_scene(model);
    let scene_step = probe.end();
    probe.begin();
    let drawn = step_draw(renderer, &scene, &Look::of(model));
    let draw_step = probe.end();
    drawn?;
    Ok(Census {
        scene: scene_step,
        draw: draw_step,
    })
}

#[inline(never)]
fn step_scene(model: &UiModel<Field>) -> loquax::ui::scene::Scene {
    screen::scene(model)
}

#[inline(never)]
fn step_draw<D>(
    renderer: &mut Renderer<'_, D>,
    scene: &loquax::ui::scene::Scene,
    look: &Look,
) -> Result<(), DrawError>
where
    D: DrawTarget<Color = Rgb565> + 'static,
{
    renderer.draw(scene, look)
}

/// A screen's ceilings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub scene_held: usize,
    pub scene_peak: usize,
    pub scene_stack: usize,
    pub draw_peak: usize,
    pub draw_stack: usize,
}

/// The chats list: a list scene is rows, not the history (held under
/// 60,000 B, peak 70,000 B, stack 100,000 B); a draw keeps nothing and
/// peaks under 180,000 B in 16,000 B of stack.
pub const CHATS: Budget = Budget {
    scene_held: 60_000,
    scene_peak: 70_000,
    scene_stack: 100_000,
    draw_peak: 180_000,
    draw_stack: 16_000,
};

/// The open chat as the M9 builds it: the scene windows the history
/// (held under 250,000 B, peak 320,000 B, stack 160,000 B); a draw lays out
/// only the bubbles on screen (peak 180,000 B, stack 24,000 B).
pub const CHAT: Budget = Budget {
    scene_held: 250_000,
    scene_peak: 320_000,
    scene_stack: 160_000,
    draw_peak: 180_000,
    draw_stack: 24_000,
};

/// What making the renderer may hold: about double 107,584 B (slice one).
pub const RENDERER_HELD: usize = 220_000;

impl Budget {
    /// The first ceiling `census` passes, by its name; none when it keeps
    /// them all. A draw that keeps anything is over too.
    pub fn over(&self, census: &Census) -> Option<&'static str> {
        let held = |step: &Step| usize::try_from(step.held).unwrap_or(0);
        [
            ("scene_held", held(&census.scene) >= self.scene_held),
            ("scene_peak", census.scene.peak >= self.scene_peak),
            ("scene_stack", census.scene.stack >= self.scene_stack),
            ("draw_held", census.draw.held != 0),
            ("draw_peak", census.draw.peak >= self.draw_peak),
            ("draw_stack", census.draw.stack >= self.draw_stack),
        ]
        .into_iter()
        .find_map(|(name, over)| over.then_some(name))
    }
}

/// The census line's words after the screen's name: what both steps cost
/// and the verdict, `ok` or `over=<ceiling>`.
pub struct Line<'a> {
    pub screen: &'a str,
    pub census: &'a Census,
    pub budget: &'a Budget,
}

impl fmt::Display for Line<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Census { scene, draw } = self.census;
        write!(
            f,
            "screen={} scene_held={} scene_peak={} scene_stack={} draw_held={} draw_peak={} draw_stack={} ",
            self.screen, scene.held, scene.peak, scene.stack, draw.held, draw.peak, draw.stack
        )?;
        match self.budget.over(self.census) {
            None => f.write_str("verdict=ok"),
            Some(ceiling) => write!(f, "verdict=over over={ceiling}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn a_census_over_one_ceiling_names_it() {
        let mut census = Census {
            scene: Step {
                held: 1_000,
                peak: 2_000,
                stack: 3_000,
            },
            draw: Step {
                held: 0,
                peak: 4_000,
                stack: 5_000,
            },
        };
        assert_eq!(CHATS.over(&census), None);
        let line = Line {
            screen: "chats",
            census: &census,
            budget: &CHATS,
        };
        assert_eq!(
            format!("{line}"),
            "screen=chats scene_held=1000 scene_peak=2000 scene_stack=3000 \
             draw_held=0 draw_peak=4000 draw_stack=5000 verdict=ok"
        );
        census.draw.held = 8;
        assert_eq!(CHATS.over(&census), Some("draw_held"));
        census.draw.held = 0;
        census.scene.stack = 100_000;
        assert_eq!(CHATS.over(&census), Some("scene_stack"));
        assert_eq!(CHAT.over(&census), None);
    }
}
