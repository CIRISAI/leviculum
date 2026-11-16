//! The buttons: a bare one, a word as a link, a bar's icon action.

use iced::widget::{button, container, text, tooltip};
use iced::{border, Background, Color, Element, Theme};

use crate::icon::{drawing, Icon};
use crate::paint::Palette;

/// A bare button over `content`: the colours are the content's.
pub fn flat<'a, M: Clone + 'a>(content: impl Into<Element<'a, M>>, on: M) -> button::Button<'a, M> {
    button(content)
        .on_press(on)
        .padding(0)
        .style(|_: &Theme, _| button::Style::default())
}

/// A word on the bars, as a link in `accent`.
pub fn link<'a, M: Clone + 'a>(words: &str, on: M, accent: Color) -> Element<'a, M> {
    flat(text(words.to_string()).size(15).color(accent), on)
        .padding([4, 8])
        .into()
}

/// An action of a bar as a drawn icon with its label on hover.
pub fn icon_action<'a, M: Clone + 'a>(
    icon: Icon,
    label: &str,
    on: M,
    p: Palette,
) -> Element<'a, M> {
    let face = drawing(
        icon,
        p.accent,
        (Color::TRANSPARENT, Color::TRANSPARENT),
        24.0,
    );
    tooltip(
        flat(face, on).padding(8),
        container(text(label.to_string()).size(13).color(p.text))
            .padding([2, 6])
            .style(move |_: &Theme| container::Style {
                background: Some(Background::Color(p.panel)),
                border: border::rounded(4),
                ..container::Style::default()
            }),
        tooltip::Position::Bottom,
    )
    .into()
}
