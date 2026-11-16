//! An avatar: initials on a disc, a ring, a status dot at its foot.

use iced::widget::{container, stack, text, Space};
use iced::{border, Background, Color, Element, Theme};

/// The ring's width around an avatar (1209): thin.
const RING: f32 = 2.0;

/// A circle of `diameter` with `initials` in `fg` on `bg`, ringed in
/// `ring` when there is one.
pub fn avatar<'a, M: 'a>(
    initials: String,
    fg: Color,
    bg: Color,
    ring: Option<Color>,
    diameter: f32,
) -> Element<'a, M> {
    container(text(initials).size(diameter * 0.4).color(fg))
        .width(diameter)
        .height(diameter)
        .center_x(diameter)
        .center_y(diameter)
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(bg)),
            border: match ring {
                Some(ink) => border::rounded(diameter / 2.0).width(RING).color(ink),
                None => border::rounded(diameter / 2.0),
            },
            ..container::Style::default()
        })
        .into()
}

/// `face`, an avatar of `diameter`, with a dot in `ink` at its foot,
/// edged in `edge` (the bar's ground) to lift it off the face.
pub fn with_dot<'a, M: 'a>(
    face: Element<'a, M>,
    diameter: f32,
    ink: Color,
    edge: Color,
) -> Element<'a, M> {
    let dot = diameter * 0.3;
    let dot =
        container(Space::new().width(dot).height(dot)).style(move |_: &Theme| container::Style {
            background: Some(Background::Color(ink)),
            border: border::rounded(dot / 2.0).width(2.0).color(edge),
            ..container::Style::default()
        });
    stack![
        face,
        container(dot)
            .width(diameter)
            .height(diameter)
            .align_right(diameter)
            .align_bottom(diameter),
    ]
    .into()
}
