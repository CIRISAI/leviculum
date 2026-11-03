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

/// What a client session may take on the heap while it pulls (1363):
/// the ask itself, one answer's page read from it, the next request. Over
/// thirty LoRa pages of one 200-byte message each the 64-bit host
/// measures a peak of 392 B and nothing held between pages
/// (`loquax-android/tests/m9_fixture_census.rs`), so the ceilings are
/// about double that. A page of the home's own bound (64 KiB, 84,576 B
/// peak) is far over, the host census's positive control. One entry
/// larger than a page still comes alone and costs its own length; that is
/// the message, which the store keeps, not the session.
pub const SESSION_PEAK: usize = 1_000;
/// What the session may hold between two pages: the ask and its verb.
pub const SESSION_HELD: usize = 256;

/// A client session's pull, measured: an ask of `/sync` in LoRa pages
/// from the home whose key this is, fed `answers` one after another as a
/// link would bring them, each page handed out and dropped as the
/// platform drops it once it is kept. The answers are made before the
/// step begins; what the link layer holds is the node's, not counted.
pub fn session(
    probe: &mut dyn Probe,
    home: &[u8; 64],
    answers: &[alloc::vec::Vec<u8>],
) -> Result<Step, loquax::home::ask::HomeError> {
    use loquax::home::ask::{Ask, Step as AskStep, Verb, LORA_PAGE_BYTES};
    probe.begin();
    let verb = Verb::Pull {
        cursor: 0,
        page_bytes: Some(LORA_PAGE_BYTES as u64),
    };
    let mut ask = Ask::new(home, verb, 0, 60_000)?;
    let mut step = match ask.poll(0, Some(1)) {
        AskStep::Connect { .. } => match ask.linked(Ok(())) {
            AskStep::Identify => ask.identified(Ok(())),
            other => other,
        },
        other => other,
    };
    for (at, answer) in answers.iter().enumerate() {
        if !matches!(step, AskStep::Request { .. }) {
            break;
        }
        drop(step);
        step = match ask.answered(Ok(answer)) {
            AskStep::Page(page) => {
                core::hint::black_box(&page);
                drop(page);
                ask.poll(at as u64 + 1, Some(1))
            }
            other => other,
        };
    }
    drop(step);
    let measured = probe.end();
    core::hint::black_box(&ask);
    drop(ask);
    Ok(measured)
}

/// The session census line: what the pull cost and the verdict.
pub struct SessionLine<'a>(pub &'a Step);

impl SessionLine<'_> {
    /// The first ceiling the session passes.
    pub fn over(&self) -> Option<&'static str> {
        let held = usize::try_from(self.0.held).unwrap_or(0);
        match (held >= SESSION_HELD, self.0.peak >= SESSION_PEAK) {
            (true, _) => Some("session_held"),
            (_, true) => Some("session_peak"),
            _ => None,
        }
    }
}

impl fmt::Display for SessionLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "session=pull held={} peak={} stack={} ",
            self.0.held, self.0.peak, self.0.stack
        )?;
        match self.over() {
            None => f.write_str("verdict=ok"),
            Some(ceiling) => write!(f, "verdict=over over={ceiling}"),
        }
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
