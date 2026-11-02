//! The help overlay in a window: a program's keybinding groups
//! ([`leviculum_ui::help`]) on the cell grid of a monospace face, the twin
//! of `leviculum-ui-ratatui`'s. The same lines, wrapped into the same rows
//! and scrolled by the same bounds, so a program's key handler and both
//! renderers agree on what is shown; the keys bold, the titles and the
//! descriptions in the secondary words.

use iced::widget::text::Span;
use iced::widget::{column, container, rich_text, text};
use iced::{border, Background, Element, Font, Pixels, Theme};

use leviculum_ui::help::{cut, overlay_rect, rows, scroll_bounds, segments, HelpGroup, Part};
use leviculum_ui::Rect;

use crate::Palette;

/// The cell grid the overlay is laid on: a monospace face and its bold,
/// their size, and a cell's width and height in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub regular: Font,
    pub bold: Font,
    pub size: f32,
    pub cell_w: f32,
    pub cell_h: f32,
}

/// The overlay over a screen of `area` cells: [`overlay_rect`]'s cells
/// in pixels, `title` on its top border row, the rows from `scroll`
/// (clamped to [`scroll_bounds`]) inside, on the panel's ground. The
/// program places it, centred over its window.
pub fn overlay<'a, M: 'a>(
    groups: &[HelpGroup],
    title: &str,
    scroll: usize,
    area: Rect,
    grid: Grid,
    p: Palette,
) -> Element<'a, M> {
    let rect = overlay_rect(groups, area);
    let (viewport, max) = scroll_bounds(groups, area);
    let segments = segments(groups);
    let line = |spans: Vec<Span<'a, (), Font>>| {
        container(
            rich_text(spans)
                .size(grid.size)
                .line_height(Pixels(grid.cell_h))
                .font(grid.regular)
                .wrapping(text::Wrapping::None),
        )
        .height(grid.cell_h)
    };
    let head = line(vec![Span::new(title.to_string())
        .font(grid.bold)
        .color(p.text)]);
    let body = rows(groups, area)
        .into_iter()
        .skip(scroll.min(max))
        .take(viewport)
        .fold(column![head], |body, row| {
            let spans = cut(&segments[row.line], row.chars)
                .into_iter()
                .map(|(words, part)| {
                    let (font, ink) = match part {
                        Part::Keys => (grid.bold, p.text),
                        Part::Title | Part::Desc => (grid.regular, p.text_secondary),
                    };
                    Span::new(words).font(font).color(ink)
                })
                .collect();
            body.push(line(spans))
        });
    container(body)
        .width(f32::from(rect.width) * grid.cell_w)
        .height(f32::from(rect.height) * grid.cell_h)
        .padding([0.0, grid.cell_w])
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(p.panel)),
            border: border::rounded(8).color(p.text_secondary).width(1),
            ..container::Style::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_ui::help::HelpEntry;

    fn groups() -> Vec<HelpGroup> {
        vec![HelpGroup {
            title: "Move",
            entries: (0..30)
                .map(|_| HelpEntry {
                    keys: "j / k",
                    desc: "line down / up",
                })
                .collect(),
        }]
    }

    /// The overlay is its rectangle's cells in pixels, whatever the
    /// scroll asks for past the end.
    #[test]
    fn the_overlay_is_its_cells_in_pixels() {
        let grid = Grid {
            regular: Font::MONOSPACE,
            bold: Font::MONOSPACE,
            size: 14.0,
            cell_w: 8.0,
            cell_h: 18.0,
        };
        let area = Rect::new(0, 0, 80, 20);
        let rect = overlay_rect(&groups(), area);
        let p = Palette {
            background: iced::Color::WHITE,
            bar: iced::Color::WHITE,
            panel: iced::Color::WHITE,
            text: iced::Color::BLACK,
            text_secondary: iced::Color::BLACK,
            accent: iced::Color::BLACK,
            selected_row: iced::Color::WHITE,
            badge: iced::Color::BLACK,
            badge_text: iced::Color::WHITE,
        };
        let element: Element<'_, ()> = overlay(&groups(), "help", 999, area, grid, p);
        let size = element.as_widget().size();
        assert_eq!(size.width, iced::Length::Fixed(f32::from(rect.width) * 8.0));
        assert_eq!(
            size.height,
            iced::Length::Fixed(f32::from(rect.height) * 18.0)
        );
    }
}
