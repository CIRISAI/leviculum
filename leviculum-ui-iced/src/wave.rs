//! The waveform of a voice message (1276), recording and standing.

use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path};
use iced::{mouse, Color, Element, Length, Point, Rectangle, Renderer, Theme};

/// A waveform (1276): the levels, 0 to 100, as rounded bars side by side,
/// loud is tall, the quietest a dot; the live bar's newest at the right.
struct Wave {
    /// How a level track longer than the room is folded to `room` bars.
    fold: fn(Vec<u8>, usize) -> Vec<u8>,
    levels: Vec<u8>,
    ink: Color,
    /// How many bars the width is cut into: fewer levels stand at the
    /// right, as the live bar scrolls in from there.
    slots: usize,
    /// It records: on a narrow canvas the newest are kept; standing, the
    /// whole is folded by the one resampler.
    moving: bool,
}

/// The narrowest a bar's pitch gets before the waveform keeps fewer.
const WAVE_PITCH: f32 = 4.0;

impl<M> canvas::Program<M> for Wave {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let room = ((bounds.width / WAVE_PITCH) as usize).max(1);
        let folded;
        let levels = match (self.levels.len() > room, self.moving) {
            (false, _) => self.levels.as_slice(),
            (true, true) => &self.levels[self.levels.len() - room..],
            (true, false) => {
                folded = (self.fold)(self.levels.clone(), room);
                folded.as_slice()
            }
        };
        let slots = self.slots.min(room).max(levels.len()).max(1);
        let pitch = bounds.width / slots as f32;
        let bar = (pitch * 0.6).max(1.0);
        let first = slots - levels.len();
        for (i, &level) in levels.iter().enumerate() {
            let h = (bounds.height * f32::from(level.min(100)) / 100.0).max(bar);
            let x = (first + i) as f32 * pitch + (pitch - bar) / 2.0;
            let y = (bounds.height - h) / 2.0;
            frame.fill(
                &Path::rounded_rectangle(
                    Point::new(x, y),
                    iced::Size::new(bar, h),
                    (bar / 2.0).into(),
                ),
                self.ink,
            );
        }
        vec![frame.into_geometry()]
    }
}

/// `levels` as a waveform `slots` bars wide, `width` by `height`; a
/// standing track longer than the room is folded by `fold`.
pub fn wave<'a, M: 'a>(
    levels: &[u8],
    fold: fn(Vec<u8>, usize) -> Vec<u8>,
    slots: usize,
    moving: bool,
    ink: Color,
    width: Length,
    height: f32,
) -> Element<'a, M> {
    Canvas::new(Wave {
        levels: levels.to_vec(),
        fold,
        ink,
        slots,
        moving,
    })
    .width(width)
    .height(height)
    .into()
}
