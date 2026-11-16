//! The small drawings (1145, 1218, 1270, 1276): the delivery marks, the
//! bars' and the compose bar's icons, drawn on a canvas, not set, so they
//! need no font that has them.

use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::{mouse, Color, Element, Point, Rectangle, Renderer, Theme};
use leviculum_ui::glyph::{Shape, GRID};

/// The small drawings: the delivery marks, the send arrow, the emoji
/// button's face, the bars' and the compose bar's icons.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Icon {
    /// A glyph on the [`GRID`]: a delivery mark (1232), drawn from its
    /// shape; `failed` draws its sign in the failed mark's colour.
    Glyph {
        shape: &'static Shape,
        failed: bool,
    },
    Send,
    Face,
    /// A bar's back: a chevron to the left.
    Back,
    /// The search: a lens.
    Search,
    /// A new conversation: a pencil.
    Pencil,
    /// The theme's toggle: a disc half filled.
    Theme,
    /// The chats' tab: a speech bubble.
    Chats,
    /// The map's tab: a pin.
    Map,
    /// The settings: a gear.
    Settings,
    /// Fetch the messages now (1457): an arrow down into a tray.
    Fetch,
    /// The compose bar's buttons beyond the emoji and Send (1270): the
    /// plus, the position button's crosshair, the camera, the microphone.
    Plus,
    Locate,
    Camera,
    Mic,
    /// The recording bar's (1276): the bin, pause, play, stop listening,
    /// and the lock chip, open with its up chevron or closed.
    Bin,
    Pause,
    Play,
    Stop,
    Lock {
        closed: bool,
    },
}

pub struct Drawing {
    icon: Icon,
    ink: Color,
    /// The failed mark's circle and sign.
    failed: (Color, Color),
}

impl<M> canvas::Program<M> for Drawing {
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
        let s = bounds.width.min(bounds.height);
        let p = |x: f32, y: f32| Point::new(x * s, y * s);
        let line = Stroke::default().with_color(self.ink).with_width(s / 10.0);
        match self.icon {
            // The ladder (1232) from the shape the phone draws too.
            Icon::Glyph { shape, failed } => {
                let u = s / GRID;
                let at = |(x, y): (f32, f32)| Point::new(x * u, y * u);
                let sign = match failed {
                    true => self.failed.1,
                    false => self.ink,
                };
                if let Some(disc) = shape.disc {
                    let path = Path::circle(at((disc.x, disc.y)), disc.r * u);
                    frame.fill(&path, self.failed.0);
                }
                let pen = Stroke::default()
                    .with_color(sign)
                    .with_width(shape.width * u)
                    .with_line_cap(canvas::LineCap::Round)
                    .with_line_join(canvas::LineJoin::Round);
                if let Some(ring) = shape.ring {
                    frame.stroke(&Path::circle(at((ring.x, ring.y)), ring.r * u), pen);
                }
                for line in shape.lines {
                    let path = Path::new(|b| {
                        if let Some((&first, rest)) = line.split_first() {
                            b.move_to(at(first));
                            for &point in rest {
                                b.line_to(at(point));
                            }
                        }
                    });
                    frame.stroke(&path, pen);
                }
                if let Some(dot) = shape.dot {
                    frame.fill(&Path::circle(at((dot.x, dot.y)), dot.r * u), sign);
                }
            }
            Icon::Send => {
                let arrow = Path::new(|b| {
                    b.move_to(p(0.3, 0.25));
                    b.line_to(p(0.78, 0.5));
                    b.line_to(p(0.3, 0.75));
                    b.line_to(p(0.38, 0.5));
                    b.close();
                });
                frame.fill(&arrow, self.ink);
            }
            Icon::Back => {
                let chevron = Path::new(|b| {
                    b.move_to(p(0.62, 0.2));
                    b.line_to(p(0.32, 0.5));
                    b.line_to(p(0.62, 0.8));
                });
                frame.stroke(&chevron, line);
            }
            Icon::Search => {
                frame.stroke(&Path::circle(p(0.43, 0.43), s * 0.25), line);
                frame.stroke(&Path::line(p(0.61, 0.61), p(0.85, 0.85)), line);
            }
            Icon::Pencil => {
                let body = Path::new(|b| {
                    b.move_to(p(0.2, 0.8));
                    b.line_to(p(0.25, 0.6));
                    b.line_to(p(0.68, 0.17));
                    b.line_to(p(0.83, 0.32));
                    b.line_to(p(0.4, 0.75));
                    b.close();
                });
                frame.stroke(&body, line);
            }
            Icon::Theme => {
                frame.stroke(&Path::circle(p(0.5, 0.5), s * 0.33), line);
                let half = Path::new(|b| {
                    b.arc(canvas::path::Arc {
                        center: p(0.5, 0.5),
                        radius: s * 0.33,
                        start_angle: iced::Radians(-std::f32::consts::FRAC_PI_2),
                        end_angle: iced::Radians(std::f32::consts::FRAC_PI_2),
                    });
                    b.close();
                });
                frame.fill(&half, self.ink);
            }
            Icon::Chats => {
                let bubble = Path::new(|b| {
                    b.move_to(p(0.2, 0.25));
                    b.line_to(p(0.8, 0.25));
                    b.line_to(p(0.8, 0.65));
                    b.line_to(p(0.45, 0.65));
                    b.line_to(p(0.28, 0.8));
                    b.line_to(p(0.3, 0.65));
                    b.line_to(p(0.2, 0.65));
                    b.close();
                });
                frame.stroke(&bubble, line);
            }
            Icon::Map => {
                let pin = Path::new(|b| {
                    b.move_to(p(0.3, 0.5));
                    b.line_to(p(0.5, 0.86));
                    b.line_to(p(0.7, 0.5));
                });
                frame.stroke(&pin, line);
                frame.stroke(&Path::circle(p(0.5, 0.38), s * 0.22), line);
                frame.fill(&Path::circle(p(0.5, 0.38), s * 0.07), self.ink);
            }
            Icon::Settings => {
                frame.stroke(&Path::circle(p(0.5, 0.5), s * 0.2), line);
                for i in 0..6 {
                    let a = i as f32 * std::f32::consts::FRAC_PI_3;
                    let (sin, cos) = a.sin_cos();
                    frame.stroke(
                        &Path::line(
                            p(0.5 + 0.2 * cos, 0.5 + 0.2 * sin),
                            p(0.5 + 0.36 * cos, 0.5 + 0.36 * sin),
                        ),
                        line,
                    );
                }
            }
            Icon::Fetch => {
                frame.stroke(&Path::line(p(0.5, 0.15), p(0.5, 0.6)), line);
                let head = Path::new(|b| {
                    b.move_to(p(0.32, 0.43));
                    b.line_to(p(0.5, 0.61));
                    b.line_to(p(0.68, 0.43));
                });
                frame.stroke(&head, line);
                let tray = Path::new(|b| {
                    b.move_to(p(0.2, 0.62));
                    b.line_to(p(0.2, 0.82));
                    b.line_to(p(0.8, 0.82));
                    b.line_to(p(0.8, 0.62));
                });
                frame.stroke(&tray, line);
            }
            Icon::Plus => {
                frame.stroke(&Path::line(p(0.5, 0.22), p(0.5, 0.78)), line);
                frame.stroke(&Path::line(p(0.22, 0.5), p(0.78, 0.5)), line);
            }
            Icon::Locate => {
                frame.stroke(&Path::circle(p(0.5, 0.5), s * 0.27), line);
                frame.fill(&Path::circle(p(0.5, 0.5), s * 0.09), self.ink);
                for (a, b) in [
                    ((0.5, 0.08), (0.5, 0.23)),
                    ((0.5, 0.77), (0.5, 0.92)),
                    ((0.08, 0.5), (0.23, 0.5)),
                    ((0.77, 0.5), (0.92, 0.5)),
                ] {
                    frame.stroke(&Path::line(p(a.0, a.1), p(b.0, b.1)), line);
                }
            }
            Icon::Camera => {
                let body = Path::new(|b| {
                    b.move_to(p(0.12, 0.33));
                    b.line_to(p(0.31, 0.33));
                    b.line_to(p(0.38, 0.23));
                    b.line_to(p(0.62, 0.23));
                    b.line_to(p(0.69, 0.33));
                    b.line_to(p(0.88, 0.33));
                    b.line_to(p(0.88, 0.79));
                    b.line_to(p(0.12, 0.79));
                    b.close();
                });
                frame.stroke(&body, line);
                frame.stroke(&Path::circle(p(0.5, 0.54), s * 0.15), line);
            }
            Icon::Mic => {
                frame.fill(
                    &Path::rounded_rectangle(
                        p(0.38, 0.12),
                        iced::Size::new(s * 0.24, s * 0.5),
                        (s * 0.12).into(),
                    ),
                    self.ink,
                );
                let cup = Path::new(|b| {
                    b.arc(canvas::path::Arc {
                        center: p(0.5, 0.46),
                        radius: s * 0.25,
                        start_angle: iced::Radians(0.0),
                        end_angle: iced::Radians(std::f32::consts::PI),
                    });
                });
                frame.stroke(&cup, line);
                frame.stroke(&Path::line(p(0.5, 0.71), p(0.5, 0.88)), line);
            }
            Icon::Bin => {
                let can = Path::new(|b| {
                    b.move_to(p(0.28, 0.3));
                    b.line_to(p(0.34, 0.85));
                    b.line_to(p(0.66, 0.85));
                    b.line_to(p(0.72, 0.3));
                });
                frame.stroke(&can, line);
                frame.stroke(&Path::line(p(0.2, 0.28), p(0.8, 0.28)), line);
                frame.stroke(&Path::line(p(0.42, 0.18), p(0.58, 0.18)), line);
                frame.stroke(&Path::line(p(0.45, 0.42), p(0.46, 0.72)), line);
                frame.stroke(&Path::line(p(0.55, 0.42), p(0.54, 0.72)), line);
            }
            Icon::Pause => {
                for x in [0.32, 0.56] {
                    frame.fill(
                        &Path::rectangle(p(x, 0.24), iced::Size::new(s * 0.12, s * 0.52)),
                        self.ink,
                    );
                }
            }
            Icon::Play => {
                let wedge = Path::new(|b| {
                    b.move_to(p(0.32, 0.22));
                    b.line_to(p(0.78, 0.5));
                    b.line_to(p(0.32, 0.78));
                    b.close();
                });
                frame.fill(&wedge, self.ink);
            }
            Icon::Stop => {
                frame.fill(
                    &Path::rectangle(p(0.28, 0.28), iced::Size::new(s * 0.44, s * 0.44)),
                    self.ink,
                );
            }
            Icon::Lock { closed } => {
                frame.fill(
                    &Path::rounded_rectangle(
                        p(0.26, 0.46),
                        iced::Size::new(s * 0.48, s * 0.36),
                        (s * 0.06).into(),
                    ),
                    self.ink,
                );
                // The shackle: shut, or lifted on the left.
                let shackle = Path::new(|b| {
                    b.move_to(p(0.36, 0.46));
                    b.line_to(p(0.36, 0.32));
                    b.arc(canvas::path::Arc {
                        center: p(0.5, 0.32),
                        radius: s * 0.14,
                        start_angle: iced::Radians(std::f32::consts::PI),
                        end_angle: iced::Radians(2.0 * std::f32::consts::PI),
                    });
                    match closed {
                        true => b.line_to(p(0.64, 0.46)),
                        false => b.line_to(p(0.64, 0.36)),
                    }
                });
                frame.stroke(&shackle, line);
                if !closed {
                    let chevron = Path::new(|b| {
                        b.move_to(p(0.38, 0.14));
                        b.line_to(p(0.5, 0.04));
                        b.line_to(p(0.62, 0.14));
                    });
                    frame.stroke(&chevron, line);
                }
            }
            Icon::Face => {
                frame.stroke(&Path::circle(p(0.5, 0.5), s * 0.38), line);
                frame.fill(&Path::circle(p(0.38, 0.42), s * 0.05), self.ink);
                frame.fill(&Path::circle(p(0.62, 0.42), s * 0.05), self.ink);
                let smile = Path::new(|b| {
                    b.arc(canvas::path::Arc {
                        center: p(0.5, 0.52),
                        radius: s * 0.2,
                        start_angle: iced::Radians(0.4),
                        end_angle: iced::Radians(std::f32::consts::PI - 0.4),
                    });
                });
                frame.stroke(&smile, line);
            }
        }
        vec![frame.into_geometry()]
    }
}

/// `icon` drawn in `ink` on a square of `side`; `failed` is the failed
/// mark's circle and sign, transparent for every other icon.
pub fn drawing<'a, M: 'a>(
    icon: Icon,
    ink: Color,
    failed: (Color, Color),
    side: f32,
) -> Element<'a, M> {
    Canvas::new(Drawing { icon, ink, failed })
        .width(side)
        .height(side)
        .into()
}
