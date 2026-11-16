//! A list that scrolls softly under the wheel (1431): a wheel's notch
//! moves the list three lines, not in one jump but eased out over about
//! a tenth of a second, as the phone's list follows a fling; a touchpad's
//! pixels pass through as they come, exactly; the keys keep their own
//! steps. The easing runs inside the widget on the screen's frames, so a
//! frame of it redraws the screen and builds no view: a chat of thousands
//! of messages scrolls at the cost of the rows in sight.
//!
//! While a text selection's drag is on ([`Smooth::edge_scroll`], 1475), a
//! pointer within [`EDGE_PX`] of the list's top or bottom edge, or past
//! it, scrolls the list that way on every frame, faster the deeper it is
//! ([`edge_step`]), and the text under the edge is told the pointer is
//! there, so the selection follows into what scrolls in.
//!
//! Wrap the [`iced::widget::scrollable`] the list is in: `smooth(scrollable(rows),
//! line)`, with the height of a line of the list's text.

use std::time::Instant;

use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{layout, overlay, renderer, Clipboard, Layout, Shell, Widget};
use iced::{mouse, window, Element, Event, Length, Point, Rectangle, Size, Vector};

/// Lines a wheel's notch scrolls.
pub const LINES_PER_NOTCH: f32 = 3.0;
/// How quickly the rest of a notch is scrolled: after this many
/// milliseconds a little under two thirds of what is left is done, so a
/// notch is all but done after 100 ms.
pub const EASE_MS: f32 = 33.0;

/// The list `content`, its wheel eased.
pub struct Smooth<'a, M, Theme, Renderer> {
    content: Element<'a, M, Theme, Renderer>,
    /// Pixels a notch scrolls.
    notch: f32,
    /// A selection's drag is on: the edges scroll.
    edge: bool,
}

/// How near the list's top or bottom edge, in logical pixels, the pointer
/// of a selection's drag scrolls the list.
pub const EDGE_PX: f32 = 32.0;

/// Pixels a frame of 16.7 ms the edge scrolls with the pointer
/// [`EDGE_PX`] deep, and three times that at most, further out.
pub const EDGE_SPEED: f32 = 12.0;

/// What is left to scroll, and the frame it was last moved on.
#[derive(Debug, Default)]
struct State {
    left: Vector,
    last: Option<Instant>,
    /// Where the pointer is, while a selection's drag is on.
    pointer: Option<Point>,
    /// The edge's last frame.
    edge_last: Option<Instant>,
}

/// What the edge scrolls in a frame `ms` after the last, the pointer at
/// `y` over a list from `top` to `bottom`: positive towards the top,
/// negative towards the bottom, 0 between the edges.
pub fn edge_step(y: f32, top: f32, bottom: f32, ms: f32) -> f32 {
    let depth = match () {
        _ if y < top + EDGE_PX => top + EDGE_PX - y,
        _ if y > bottom - EDGE_PX => -(y - (bottom - EDGE_PX)),
        _ => return 0.0,
    };
    let speed = (depth.abs() / EDGE_PX).clamp(0.25, 3.0) * EDGE_SPEED;
    depth.signum() * speed * ms / 16.7
}

/// `content`, a scrollable, its wheel eased: a notch scrolls three lines
/// of `line` pixels.
pub fn smooth<'a, M, Theme, Renderer>(
    content: impl Into<Element<'a, M, Theme, Renderer>>,
    line: f32,
) -> Smooth<'a, M, Theme, Renderer> {
    Smooth {
        content: content.into(),
        notch: line * LINES_PER_NOTCH,
        edge: false,
    }
}

impl<M, Theme, Renderer> Smooth<'_, M, Theme, Renderer> {
    /// While `on` (a text selection's drag is on), the pointer at an edge
    /// scrolls the list.
    pub fn edge_scroll(mut self, on: bool) -> Self {
        self.edge = on;
        self
    }
}

/// Of `left`, what to scroll in a frame `ms` after the last: all of it
/// once under half a pixel is left.
pub fn step(left: f32, ms: f32) -> f32 {
    if left.abs() < 0.5 {
        return left;
    }
    let by = left * (1.0 - (-ms / EASE_MS).exp());
    // At least a pixel a frame, so the tail does not crawl.
    match by.abs() < 1.0 {
        true => left.signum() * left.abs().min(1.0),
        false => by,
    }
}

impl<M, Theme, Renderer> Smooth<'_, M, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    /// A frame of the edge: with the pointer at an edge, the list scrolled
    /// by [`edge_step`] and the text under the edge told the pointer is
    /// there; another frame asked while it stays.
    #[allow(clippy::too_many_arguments)]
    fn edge_frame(
        &mut self,
        tree: &mut Tree,
        now: Instant,
        layout: Layout<'_>,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<State>();
        let Some(pointer) = state.pointer else {
            return;
        };
        let (top, bottom) = (bounds.y, bounds.y + bounds.height);
        if edge_step(pointer.y, top, bottom, 1.0) == 0.0 {
            state.edge_last = None;
            return;
        }
        let ms = state.edge_last.map_or(16.7, |last| {
            now.saturating_duration_since(last).as_secs_f32() * 1e3
        });
        state.edge_last = Some(now);
        let by = edge_step(pointer.y, top, bottom, ms.min(100.0));
        let scroll = Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0.0, y: by },
        });
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            &scroll,
            layout,
            mouse::Cursor::Available(bounds.center()),
            renderer,
            clipboard,
            shell,
            viewport,
        );
        // The pointer held inside the list, a pixel from its edge: the
        // text there takes it as the drag's head.
        let inside = Point::new(
            pointer
                .x
                .clamp(bounds.x + 1.0, bounds.x + bounds.width - 1.0),
            pointer.y.clamp(top + 1.0, bottom - 1.0),
        );
        let moved = Event::Mouse(mouse::Event::CursorMoved { position: inside });
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            &moved,
            layout,
            mouse::Cursor::Available(inside),
            renderer,
            clipboard,
            shell,
            viewport,
        );
        shell.request_redraw();
    }
}

impl<M, Theme, Renderer> Widget<M, Theme, Renderer> for Smooth<'_, M, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        match event {
            // A notch over the list: added to what is left, scrolled on
            // the frames to come.
            Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x, y },
            }) if cursor.is_over(bounds) => {
                let state = tree.state.downcast_mut::<State>();
                state.left += Vector::new(x * self.notch, y * self.notch);
                state.last = None;
                shell.capture_event();
                shell.request_redraw();
                return;
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if self.edge => {
                let state = tree.state.downcast_mut::<State>();
                state.pointer = cursor.position();
                if state
                    .pointer
                    .is_some_and(|p| edge_step(p.y, bounds.y, bounds.y + bounds.height, 1.0) != 0.0)
                {
                    shell.request_redraw();
                }
            }
            Event::Window(window::Event::RedrawRequested(now)) if self.edge => {
                self.edge_frame(tree, *now, layout, renderer, clipboard, shell, viewport);
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let state = tree.state.downcast_mut::<State>();
                state.pointer = None;
                state.edge_last = None;
            }
            _ => {}
        }
        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                let state = tree.state.downcast_mut::<State>();
                if state.left != Vector::ZERO {
                    // The first frame of a notch moves by a frame's worth.
                    let ms = state.last.map_or(16.7, |last| {
                        now.saturating_duration_since(last).as_secs_f32() * 1e3
                    });
                    let by = Vector::new(step(state.left.x, ms), step(state.left.y, ms));
                    state.left -= by;
                    state.last = Some(*now);
                    // The scrollable takes the step as a touchpad's
                    // pixels, with the pointer over it.
                    let scroll = Event::Mouse(mouse::Event::WheelScrolled {
                        delta: mouse::ScrollDelta::Pixels { x: by.x, y: by.y },
                    });
                    self.content.as_widget_mut().update(
                        &mut tree.children[0],
                        &scroll,
                        layout,
                        mouse::Cursor::Available(bounds.center()),
                        renderer,
                        clipboard,
                        shell,
                        viewport,
                    );
                    let state = tree.state.downcast_mut::<State>();
                    if state.left != Vector::ZERO {
                        shell.request_redraw();
                    } else {
                        state.last = None;
                    }
                }
            }
            // A touchpad's pixels or a drag on the scroller stop a notch
            // still under way, and go through as they are.
            Event::Mouse(mouse::Event::WheelScrolled { .. })
            | Event::Mouse(mouse::Event::ButtonPressed(_))
                if cursor.is_over(bounds) =>
            {
                let state = tree.state.downcast_mut::<State>();
                state.left = Vector::ZERO;
                state.last = None;
            }
            _ => {}
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M: 'a, Theme: 'a, Renderer> From<Smooth<'a, M, Theme, Renderer>>
    for Element<'a, M, Theme, Renderer>
where
    Renderer: renderer::Renderer + 'a,
{
    fn from(smooth: Smooth<'a, M, Theme, Renderer>) -> Self {
        Element::new(smooth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::Offscreen;
    use iced::widget::{column, container, scrollable};
    use iced::{Background, Color, Point, Theme};
    use std::time::Duration;

    /// A hundred bands of 20 pixels, each its own grey, in a scrollable
    /// 200 high and 100 wide, wrapped or not.
    fn bands(wrapped: bool) -> Element<'static, ()> {
        let rows = (0..100u8).fold(column![], |rows, i| {
            rows.push(
                container(iced::widget::Space::new().width(100).height(20)).style(
                    move |_: &Theme| container::Style {
                        background: Some(Background::Color(Color::from_rgb8(i, i, i))),
                        ..container::Style::default()
                    },
                ),
            )
        });
        let list = scrollable(rows).height(200).width(100);
        match wrapped {
            true => smooth(list, 20.0).into(),
            false => list.into(),
        }
    }

    /// The band at the list's top left pixel.
    fn top(rgb: &[u8]) -> u8 {
        rgb[0]
    }

    fn redraw(at: Instant) -> Event {
        Event::Window(window::Event::RedrawRequested(at))
    }

    fn notch(lines: f32) -> Event {
        Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Lines { x: 0.0, y: lines },
        })
    }

    /// A notch down scrolls three lines, 60 pixels, over the frames that
    /// follow, not at once: the first frame moves less than half of it,
    /// every frame moves on, and after 150 ms of frames the list stands
    /// three bands down. The control: the same notch on the bare
    /// scrollable jumps its 60 pixels in the one update.
    #[test]
    fn a_notch_scrolls_three_lines_eased() {
        let mut screen = Offscreen::new(100, 200).expect("the software renderer");
        let start = Instant::now();
        let mut events = vec![notch(-1.0)];
        for i in 1..=9 {
            events.push(redraw(start + Duration::from_millis(16 * i)));
        }
        let at = Point::new(50.0, 100.0);
        let mut tops = Vec::new();
        for n in 1..=events.len() {
            let (_, rgb, _) = screen.redraws(
                bands(true),
                &events[..n],
                at,
                &Theme::Light,
                Color::BLACK,
                Color::WHITE,
            );
            // The pixel row at the top: which band, and how far into it.
            let first = (0..200).find(|&y| rgb[y * 300] != top(&rgb)).unwrap_or(200);
            tops.push(u32::from(top(&rgb)) * 20 + (20 - first as u32));
        }
        // `tops` is the scroll offset in pixels after each event.
        assert_eq!(tops[0], 0, "the notch itself moves nothing yet: {tops:?}");
        assert!(
            tops[1] > 0 && tops[1] < 30,
            "the first frame eases in: {tops:?}"
        );
        assert!(tops.windows(2).all(|w| w[1] >= w[0]), "{tops:?}");
        assert_eq!(*tops.last().expect("a frame"), 60, "{tops:?}");
        let (_, bare, _) = screen.redraws(
            bands(false),
            &[notch(-1.0)],
            at,
            &Theme::Light,
            Color::BLACK,
            Color::WHITE,
        );
        assert_eq!(top(&bare), 3, "the bare list jumps its 60 pixels");
    }

    /// A touchpad's pixels go through exactly, at once: 7 pixels down is
    /// the list 7 pixels down in the same update.
    #[test]
    fn a_touchpads_pixels_go_through_exactly() {
        let mut screen = Offscreen::new(100, 200).expect("the software renderer");
        let pixels = Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0.0, y: -7.0 },
        });
        let (_, rgb, _) = screen.redraws(
            bands(true),
            &[pixels],
            Point::new(50.0, 100.0),
            &Theme::Light,
            Color::BLACK,
            Color::WHITE,
        );
        assert_eq!(top(&rgb), 0);
        let first = (0..200).find(|&y| rgb[y * 300] != 0).unwrap_or(200);
        assert_eq!(first, 13, "band 0 shows its last 13 rows");
    }

    /// The step: all of what is left once under half a pixel, at least a
    /// pixel a frame, about a third of it in a frame of 16 ms.
    #[test]
    fn the_step_eases_out() {
        assert_eq!(step(0.4, 16.0), 0.4);
        assert_eq!(step(-1.5, 1.0), -1.0);
        let by = step(60.0, 16.7);
        assert!(
            (by - 60.0 * (1.0 - (-16.7f32 / EASE_MS).exp())).abs() < 1e-3,
            "{by}"
        );
        assert!(by > 15.0 && by < 30.0, "{by}");
    }

    /// While a selection's drag is on, the pointer in the bottom edge
    /// scrolls the list down on every frame, and one deeper scrolls faster
    /// (1475). The controls: the same pointer with no drag on, and one
    /// in the middle of the list, scroll nothing.
    #[test]
    fn a_selections_drag_at_the_edge_scrolls_the_list() {
        let mut screen = Offscreen::new(100, 200).expect("the software renderer");
        let start = Instant::now();
        let offset = |screen: &mut Offscreen, edge: bool, y: f32| {
            let at = Point::new(50.0, y);
            let mut events = vec![Event::Mouse(mouse::Event::CursorMoved { position: at })];
            for i in 1..=10 {
                events.push(redraw(start + Duration::from_millis(16 * i)));
            }
            let list = scrollable((0..100u8).fold(
                column![],
                |rows: iced::widget::Column<'_, ()>, i| {
                    rows.push(
                        container(iced::widget::Space::new().width(100).height(20)).style(
                            move |_: &Theme| container::Style {
                                background: Some(Background::Color(Color::from_rgb8(i, i, i))),
                                ..container::Style::default()
                            },
                        ),
                    )
                },
            ))
            .height(200)
            .width(100);
            let (_, rgb, _) = screen.redraws(
                smooth(list, 20.0).edge_scroll(edge).into(),
                &events,
                at,
                &Theme::Light,
                Color::BLACK,
                Color::WHITE,
            );
            let first = (0..200).find(|&y| rgb[y * 300] != top(&rgb)).unwrap_or(200);
            u32::from(top(&rgb)) * 20 + (20 - first as u32)
        };
        let near = offset(&mut screen, true, 190.0);
        assert!(near > 0, "the edge scrolled nothing");
        let deep = offset(&mut screen, true, 199.0);
        assert!(deep > near, "deeper is faster: {deep} against {near}");
        assert_eq!(offset(&mut screen, false, 190.0), 0, "no drag on");
        assert_eq!(offset(&mut screen, true, 100.0), 0, "the middle");
    }

    #[test]
    fn the_edge_steps_towards_its_side_and_not_between() {
        assert_eq!(edge_step(100.0, 0.0, 200.0, 16.7), 0.0);
        assert!(edge_step(5.0, 0.0, 200.0, 16.7) > 0.0, "the top scrolls up");
        assert!(edge_step(195.0, 0.0, 200.0, 16.7) < 0.0);
        // Past the edge, three times the speed at most.
        assert_eq!(edge_step(-500.0, 0.0, 200.0, 16.7), 3.0 * EDGE_SPEED);
    }
}
