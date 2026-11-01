//! The window's photo whole (1269): the picture over the window, zoomed
//! by the wheel about the pointer, dragged while zoomed, a double click
//! toggling 1x and 2x at the click; Esc goes to 1x and, at 1x, closes.
//! The state is [`loquax::ui::zoom::Zoom`], the phone's viewer's too, in
//! the window's logical pixels; this file only maps the mouse onto it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::advanced::image::{FilterMethod, Handle};
use iced::widget::canvas::{self, Canvas, Frame, Geometry};
use iced::widget::{button, column, container, row, text};
use iced::{mouse, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use loquax::ui::zoom::Zoom;

use super::Message;

/// A photo's pixels as the shell decoded them for the window: what
/// [`super::Seen::photo`] carries. A new `serial` is a new press on a
/// bubble's photo, the same photo again or not.
#[derive(Clone, Debug)]
pub struct Picture {
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    /// The picture's own size before the decode scaled it: the zoom's
    /// bound.
    pub full: (u32, u32),
    pub rgba: Arc<Vec<u8>>,
}

impl PartialEq for Picture {
    /// The serial alone: the pixels are not compared on every tick.
    fn eq(&self, other: &Self) -> bool {
        self.serial == other.serial
    }
}

/// What the mouse did on the viewer, in logical pixels from its top left.
#[derive(Clone, Copy, Debug)]
pub enum Input {
    Size(Size),
    Down(Point),
    Move(Point),
    Up,
    /// The wheel or the touchpad turned at a point, by notches: towards
    /// the reader zooms in.
    Wheel(Point, f32),
}

/// What one wheel notch zooms by.
pub const NOTCH: f64 = 1.25;
/// Pixels of a touchpad's scroll a notch takes.
const PX_PER_NOTCH: f32 = 120.0;
/// Two presses this close in time and place are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const DOUBLE_CLICK_PX: f32 = 6.0;

/// The viewer while it is open.
pub struct Viewer {
    handle: Handle,
    pub zoom: Zoom,
    /// Where the mouse button went down and last was, while it is down.
    press: Option<Point>,
    /// The last press, for the double click.
    last_press: Option<(Instant, Point)>,
}

impl Viewer {
    /// `picture` over a viewer of `size`, at 1x.
    pub fn new(picture: &Picture, size: Size) -> Self {
        let full = match picture.full {
            (0, _) | (_, 0) => (picture.width, picture.height),
            full => full,
        };
        Self {
            handle: Handle::from_rgba(picture.width, picture.height, picture.rgba.to_vec()),
            zoom: Zoom::new(
                f64::from(size.width),
                f64::from(size.height),
                f64::from(full.0),
                f64::from(full.1),
            ),
            press: None,
            last_press: None,
        }
    }

    /// Fold what the mouse did at `now`.
    pub fn input(&mut self, input: Input, now: Instant) {
        match input {
            Input::Size(size) => self
                .zoom
                .resize(f64::from(size.width), f64::from(size.height)),
            Input::Down(p) => {
                let double = self.last_press.is_some_and(|(at, q)| {
                    now.saturating_duration_since(at) <= DOUBLE_CLICK
                        && (p.x - q.x).abs() <= DOUBLE_CLICK_PX
                        && (p.y - q.y).abs() <= DOUBLE_CLICK_PX
                });
                if double {
                    self.zoom.double_tap(f64::from(p.x), f64::from(p.y));
                    self.last_press = None;
                } else {
                    self.last_press = Some((now, p));
                }
                self.press = Some(p);
            }
            Input::Move(p) => {
                if let Some(last) = self.press {
                    self.zoom
                        .pan_by(f64::from(p.x - last.x), f64::from(p.y - last.y));
                    self.press = Some(p);
                }
            }
            Input::Up => self.press = None,
            Input::Wheel(p, notches) => {
                self.zoom.pinch(
                    NOTCH.powf(f64::from(notches)),
                    f64::from(p.x),
                    f64::from(p.y),
                );
            }
        }
    }

    /// Esc: zoomed, 1x again; whether it was at 1x, so the viewer closes.
    pub fn back(&mut self) -> bool {
        self.zoom.back()
    }

    /// The viewer over the whole window: the picture on black, a bar with
    /// the zoom and Close above it.
    pub fn view(&self) -> Element<'_, Message> {
        let bar = row![
            container(text(format!("{:.1}x", self.zoom.scale())).size(13))
                .width(Length::Fill)
                .padding([4, 8]),
            button(text("Close").size(13))
                .on_press(Message::ViewerClose)
                .padding([2, 10])
                .style(button::text),
        ];
        let picture = container(
            Canvas::new(Drawing { viewer: self })
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true);
        container(column![bar, picture])
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Color::BLACK.into()),
                text_color: Some(Color::WHITE),
                ..container::Style::default()
            })
            .into()
    }
}

/// The picture where the zoom puts it, and the mouse.
struct Drawing<'a> {
    viewer: &'a Viewer,
}

/// What the canvas remembers between events.
#[derive(Default)]
pub struct DrawingState {
    size: Option<Size>,
    pressed: bool,
}

impl canvas::Program<Message> for Drawing<'_> {
    type State = DrawingState;

    fn update(
        &self,
        state: &mut DrawingState,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let size = bounds.size();
        if state.size != Some(size) {
            state.size = Some(size);
            return Some(canvas::Action::publish(Message::Viewer(Input::Size(size))));
        }
        let iced::Event::Mouse(event) = event else {
            return None;
        };
        let input = match (event, cursor.position_in(bounds)) {
            (mouse::Event::ButtonPressed(mouse::Button::Left), Some(p)) => {
                state.pressed = true;
                Input::Down(p)
            }
            (mouse::Event::CursorMoved { .. }, _) if state.pressed => {
                let p = cursor.position()?;
                Input::Move(Point::new(p.x - bounds.x, p.y - bounds.y))
            }
            (mouse::Event::ButtonReleased(mouse::Button::Left), _) if state.pressed => {
                state.pressed = false;
                Input::Up
            }
            (mouse::Event::WheelScrolled { delta }, Some(p)) => {
                let notches = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => y / PX_PER_NOTCH,
                };
                if notches == 0.0 {
                    return None;
                }
                Input::Wheel(p, notches)
            }
            _ => return None,
        };
        Some(canvas::Action::publish(Message::Viewer(input)).and_capture())
    }

    fn draw(
        &self,
        _state: &DrawingState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let r = self.viewer.zoom.rect();
        frame.draw_image(
            Rectangle {
                x: r.left as f32,
                y: r.top as f32,
                width: r.width as f32,
                height: r.height as f32,
            },
            canvas::Image::new(self.viewer.handle.clone()).filter_method(FilterMethod::Linear),
        );
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 400 by 200 picture on an 800 by 800 viewer: 800 by 400 at 1x.
    fn viewer() -> Viewer {
        let picture = Picture {
            serial: 1,
            width: 400,
            height: 200,
            full: (400, 200),
            rgba: Arc::new(vec![0; 400 * 200 * 4]),
        };
        Viewer::new(&picture, Size::new(800.0, 800.0))
    }

    #[test]
    fn the_wheel_zooms_about_the_pointer() {
        let mut v = viewer();
        let t = Instant::now();
        assert!(v.zoom.at_rest());
        // Four notches: 1.25^4 = 2.44.
        v.input(Input::Wheel(Point::new(400.0, 400.0), 4.0), t);
        assert!((v.zoom.scale() - NOTCH.powi(4)).abs() < 1e-9);
        // Away from the reader, far: back to 1x, never below.
        v.input(Input::Wheel(Point::new(10.0, 10.0), -40.0), t);
        assert!(v.zoom.at_rest());
        // Many notches in: held at 4x (the picture's own is 0.5x).
        v.input(Input::Wheel(Point::new(400.0, 400.0), 40.0), t);
        assert!((v.zoom.scale() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn a_drag_pans_while_zoomed_and_stops_at_the_edge() {
        let mut v = viewer();
        let t = Instant::now();
        v.input(Input::Down(Point::new(100.0, 100.0)), t);
        v.input(Input::Move(Point::new(300.0, 100.0)), t);
        v.input(Input::Up, t);
        assert_eq!(v.zoom.pan(), (0.0, 0.0), "at 1x nothing pans");
        v.input(Input::Wheel(Point::new(400.0, 400.0), 4.0), t);
        let left = v.zoom.rect().left;
        let later = t + Duration::from_secs(1);
        v.input(Input::Down(Point::new(400.0, 400.0)), later);
        v.input(Input::Move(Point::new(450.0, 400.0)), later);
        assert!((v.zoom.rect().left - (left + 50.0)).abs() < 1e-9);
        v.input(Input::Move(Point::new(5000.0, 400.0)), later);
        assert!(v.zoom.rect().left.abs() < 1e-9, "{:?}", v.zoom.rect());
        v.input(Input::Up, later);
        // A move without the button down moves nothing.
        let before = v.zoom;
        v.input(Input::Move(Point::new(0.0, 0.0)), later);
        assert_eq!(v.zoom, before);
    }

    #[test]
    fn a_double_click_toggles_and_esc_unzooms_then_closes() {
        let mut v = viewer();
        let t = Instant::now();
        let p = Point::new(600.0, 400.0);
        v.input(Input::Down(p), t);
        v.input(Input::Up, t);
        assert!(v.zoom.at_rest(), "one click is no zoom");
        v.input(Input::Down(p), t + Duration::from_millis(200));
        v.input(Input::Up, t + Duration::from_millis(200));
        assert!((v.zoom.scale() - 2.0).abs() < 1e-9);
        // Two clicks too far apart in time are none.
        v.input(Input::Down(p), t + Duration::from_secs(2));
        v.input(Input::Down(p), t + Duration::from_secs(3));
        assert!((v.zoom.scale() - 2.0).abs() < 1e-9);
        assert!(!v.back());
        assert!(v.zoom.at_rest());
        assert!(v.back());
    }

    #[test]
    fn a_resize_keeps_the_zoom() {
        let mut v = viewer();
        let t = Instant::now();
        v.input(Input::Wheel(Point::new(400.0, 400.0), 4.0), t);
        let scale = v.zoom.scale();
        v.input(Input::Size(Size::new(1200.0, 600.0)), t);
        assert_eq!(v.zoom.scale(), scale);
        assert_eq!(v.zoom.view(), (1200.0, 600.0));
    }
}
