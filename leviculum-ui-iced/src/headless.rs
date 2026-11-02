//! A window's view without a window: drawn by the software renderer into
//! pixels, and clicked, for the goldens and the tests of the programs'
//! windows (Loquax's since 1145, lnomad's since slice 2 of
//! `docs/src/concepts/lnomad-everywhere.md`).

use iced::advanced::renderer::{self, Headless, Renderer as _};
use iced::advanced::widget::Tree;
use iced::advanced::{clipboard, layout, Layout, Shell};
use iced::{mouse, Color, Element, Event, Point, Rectangle, Renderer, Size, Theme};

/// The software renderer, its default font `font` at 16 pixels; `None`
/// where iced finds no backend to start.
fn software(font: iced::Font) -> Option<Renderer> {
    iced::futures::executor::block_on(<Renderer as Headless>::new(font, iced::Pixels(16.0), None))
}

/// `element` laid out on `w` by `h` and drawn in `theme`, its text in
/// `text` on `background`: the pixels, RGB, row by row; `None` without a
/// renderer.
pub fn render<M>(
    mut element: Element<'_, M>,
    theme: &Theme,
    text: Color,
    background: Color,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    let mut renderer = software(iced::Font::DEFAULT)?;
    let size = Size::new(w as f32, h as f32);
    let mut tree = Tree::new(&element);
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    let viewport = Rectangle::with_size(size);
    renderer.reset(viewport);
    element.as_widget().draw(
        &tree,
        &mut renderer,
        theme,
        &renderer::Style { text_color: text },
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
    Some(
        renderer
            .screenshot(iced::Size::new(w, h), 1.0, background)
            .chunks(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
    )
}

/// A left click at `at` on `element` laid out on `w` by `h`: the pointer
/// moves there, the button goes down and comes up. The messages the
/// widgets published, in order; `None` without a renderer.
pub fn click<M>(mut element: Element<'_, M>, at: Point, w: u32, h: u32) -> Option<Vec<M>> {
    let renderer = software(iced::Font::DEFAULT)?;
    let size = Size::new(w as f32, h as f32);
    let mut tree = Tree::new(&element);
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, size),
    );
    let viewport = Rectangle::with_size(size);
    let cursor = mouse::Cursor::Available(at);
    let mut messages = Vec::new();
    for event in [
        mouse::Event::CursorMoved { position: at },
        mouse::Event::ButtonPressed(mouse::Button::Left),
        mouse::Event::ButtonReleased(mouse::Button::Left),
    ] {
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(event),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &viewport,
        );
    }
    Some(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::flat;
    use iced::widget::{column, text, Space};

    /// A press on a button is its message, a press beside it is none.
    #[test]
    fn a_click_on_a_button_is_its_message() {
        let view = || -> Element<'_, u8> {
            column![
                flat(Space::new().width(100).height(40), 7u8),
                text("under it"),
            ]
            .into()
        };
        assert_eq!(
            click(view(), Point::new(50.0, 20.0), 200, 200),
            Some(vec![7])
        );
        assert_eq!(
            click(view(), Point::new(150.0, 20.0), 200, 200),
            Some(vec![])
        );
    }

    /// The pixels are the size asked, the ground where nothing is drawn.
    #[test]
    fn a_render_is_its_size_on_its_ground() {
        let ground = Color::from_rgb8(10, 20, 30);
        let view: Element<'_, u8> = Space::new().into();
        let px = render(view, &Theme::Light, Color::BLACK, ground, 8, 4).expect("a renderer");
        assert_eq!(px.len(), 8 * 4 * 3);
        assert_eq!(&px[..3], &[10, 20, 30]);
    }
}
