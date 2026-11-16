//! The chrome the scene's shared nodes draw as (1218): the top bar, a
//! count badge, a tab and the tabs' rail or bar, a list item, the search
//! field.

use iced::widget::{button, column, container, row, text, text_input};
use iced::{border, Alignment, Background, Color, Element, Length, Theme};

use crate::icon::{drawing, Icon};
use crate::paint::Palette;

/// The tab rail's width in the wide layout, in logical pixels.
pub const RAIL: f32 = 72.0;

/// A top bar `height` high: the `lead` (an avatar, a back), the `title`
/// filling the line, the `actions` at the right.
pub fn top_bar<'a, M: 'a>(
    lead: Option<Element<'a, M>>,
    title: String,
    actions: impl IntoIterator<Item = Element<'a, M>>,
    height: f32,
    p: Palette,
) -> Element<'a, M> {
    let mut line = row![].spacing(12).align_y(Alignment::Center);
    if let Some(lead) = lead {
        line = line.push(lead);
    }
    line = line.push(text(title).size(22).color(p.text).width(Length::Fill));
    let actions = actions
        .into_iter()
        .fold(row![].align_y(Alignment::Center), |row, a| row.push(a));
    container(line.push(actions))
        .height(height)
        .padding([0, 12])
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .into()
}

/// A count on a rounded ground of `corner`, its words `size` high; the
/// caller pads and sizes it.
pub fn badge<'a, M: 'a>(
    count: impl std::fmt::Display,
    size: f32,
    corner: f32,
    p: Palette,
) -> container::Container<'a, M> {
    container(text(count.to_string()).size(size).color(p.badge_text)).style(move |_: &Theme| {
        container::Style {
            background: Some(Background::Color(p.badge)),
            border: border::rounded(corner),
            ..container::Style::default()
        }
    })
}

/// One tab: its icon over its `label`, the `badge` beside the icon, the
/// active one in the accent on the selected row's ground.
pub fn tab<'a, M: Clone + 'a>(
    icon: Icon,
    label: String,
    badge: Option<container::Container<'a, M>>,
    active: bool,
    on: M,
    p: Palette,
) -> Element<'a, M> {
    let ink = match active {
        true => p.accent,
        false => p.text_secondary,
    };
    let mut top = row![drawing(
        icon,
        ink,
        (Color::TRANSPARENT, Color::TRANSPARENT),
        26.0
    )]
    .spacing(2)
    .align_y(Alignment::Center);
    if let Some(badge) = badge {
        top = top.push(badge);
    }
    let item = column![top, text(label).size(12).color(ink)]
        .spacing(2)
        .align_x(Alignment::Center);
    let ground = match active {
        true => p.selected_row,
        false => Color::TRANSPARENT,
    };
    button(container(item).center_x(Length::Fill).padding([8, 0]))
        .on_press(on)
        .padding(0)
        .width(Length::Fill)
        .style(move |_: &Theme, _| button::Style {
            background: Some(Background::Color(ground)),
            border: border::rounded(12),
            ..button::Style::default()
        })
        .into()
}

/// The tabs on the bar's ground: a rail [`RAIL`] wide at the left when
/// `rail`, else a bar along the bottom.
pub fn tabs<'a, M: 'a>(
    items: impl IntoIterator<Item = Element<'a, M>>,
    rail: bool,
    p: Palette,
) -> Element<'a, M> {
    let ground = move |_: &Theme| container::Style {
        background: Some(Background::Color(p.bar)),
        ..container::Style::default()
    };
    match rail {
        true => container(column(items).spacing(8).padding([12, 6]))
            .width(RAIL)
            .height(Length::Fill)
            .style(ground)
            .into(),
        false => container(row(items).spacing(8).padding([6, 12]))
            .width(Length::Fill)
            .style(ground)
            .into(),
    }
}

/// A list item over `line`, the whole row its button: the selected one on
/// the selected row's ground, another on the bar's while hovered.
pub fn list_item<'a, M: Clone + 'a>(
    line: impl Into<Element<'a, M>>,
    selected: bool,
    on: M,
    p: Palette,
) -> Element<'a, M> {
    let ground = match selected {
        true => p.selected_row,
        false => Color::TRANSPARENT,
    };
    button(
        container(line)
            .height(Length::Shrink)
            .padding([12, 16])
            .width(Length::Fill),
    )
    .on_press(on)
    .padding(0)
    .width(Length::Fill)
    .style(move |_: &Theme, status| button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered if !selected => p.bar,
            _ => ground,
        })),
        ..button::Style::default()
    })
    .into()
}

/// A text field as the search is drawn: `hint` until there is a `value`,
/// on the bar's ground, rounded by `corner`.
pub fn search_field<'a, M: Clone + 'a>(
    hint: &str,
    value: &str,
    corner: f32,
    p: Palette,
) -> text_input::TextInput<'a, M> {
    text_input(hint, value)
        .size(15)
        .padding([8, 14])
        .style(move |_: &Theme, _| text_input::Style {
            background: Background::Color(p.bar),
            border: border::rounded(corner),
            icon: p.text_secondary,
            placeholder: p.text_secondary,
            value: p.text,
            selection: p.selected_row,
        })
}
