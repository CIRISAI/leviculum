//! Text that can be selected (1444): iced's text widgets draw and do not
//! select, so a text a program wants selectable is wrapped in
//! [`selectable`]. The wrapper draws the text exactly as it was given (the
//! inner widget lays it out and draws it, so nothing moves by a pixel), the
//! selected bytes under it in a highlight, and turns the left button on
//! it into [`Pick`]s at byte offsets of the text: the press with how many
//! clicks it was (a double click, a triple click) and whether Shift was
//! held, the pointer while the program says a drag is on, the release.
//!
//! What a press selects (the word, the line) and the selection across
//! several texts are the program's: it keeps the state, this widget only
//! maps the pointer to a place in the text and draws the range it is
//! given. Loquax keeps it in its core (`loquax::ui::selection`).
//!
//! The pointer is mapped with a second paragraph of the same text, cut
//! into spans at the newlines and the selection's ends, laid out with the
//! inner text's own font, size and bounds: its spans tell which line a
//! point is on (iced's hit test gives the offset within a line only) and
//! where the selection's rectangles are. It is built only for a text
//! that is selected or under the pointer, never for the others.

use std::ops::Range;

use iced::advanced::mouse::click::Kind as ClickKind;
use iced::advanced::mouse::Click;
use iced::advanced::text::{self, Paragraph as _, Span};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{layout, overlay, renderer, Clipboard, Layout, Shell, Widget};
use iced::{keyboard, mouse, Color, Element, Event, Length, Point, Rectangle, Size, Vector};

/// What the left button did on a selectable text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// The button went down before byte `at`: `clicks` is 1, 2 or 3 for a
    /// single, a double or a triple click; `shift` whether Shift was held.
    Press { at: usize, clicks: u8, shift: bool },
    /// The pointer is over this text before byte `at`, while the program
    /// said a drag is on.
    Drag { at: usize },
    /// The button came up after a press on this text.
    Release,
}

/// `inner`, a text widget showing `content`, made selectable: `range`
/// drawn in `highlight` under it, the button's [`Pick`]s through `on`.
/// `dragging`: a press went down on some selectable text and the button
/// is still held, so this one reports the pointer over it too.
pub fn selectable<'a, M, Theme, Renderer>(
    inner: impl Into<Element<'a, M, Theme, Renderer>>,
    content: &'a str,
    range: Option<Range<usize>>,
    highlight: Color,
    dragging: bool,
    on: impl Fn(Pick) -> M + 'a,
) -> Selectable<'a, M, Theme, Renderer> {
    Selectable {
        inner: inner.into(),
        content,
        range: range.filter(|r| r.start < r.end),
        highlight,
        dragging,
        on: Box::new(on),
    }
}

/// See [`selectable`].
pub struct Selectable<'a, M, Theme, Renderer> {
    inner: Element<'a, M, Theme, Renderer>,
    content: &'a str,
    range: Option<Range<usize>>,
    highlight: Color,
    dragging: bool,
    on: Box<dyn Fn(Pick) -> M + 'a>,
}

/// The second paragraph and what it was built for.
struct Cut<P> {
    paragraph: P,
    content: String,
    range: Option<Range<usize>>,
    bounds: Size,
    /// Each span's first byte in the text.
    starts: Vec<usize>,
}

struct State<P> {
    cut: Option<Cut<P>>,
    last_click: Option<Click>,
    pressed: bool,
    shift: bool,
    last_drag: Option<usize>,
}

impl<P> Default for State<P> {
    fn default() -> Self {
        Self {
            cut: None,
            last_click: None,
            pressed: false,
            shift: false,
            last_drag: None,
        }
    }
}

/// Where the spans of `content` begin: at every line's start and at the
/// ends of `range`, in order, without repeats.
pub fn span_starts(content: &str, range: Option<&Range<usize>>) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(content.match_indices('\n').map(|(i, _)| i + 1));
    if let Some(r) = range {
        starts.push(r.start.min(content.len()));
        starts.push(r.end.min(content.len()));
    }
    starts.sort_unstable();
    starts.dedup();
    starts.retain(|&s| s < content.len() || s == 0);
    starts
}

/// The byte of `content` a hit at `index` of its line `line` is, the line
/// counted by its newlines, clamped to the line and onto a character.
pub fn offset_of(content: &str, line: usize, index: usize) -> usize {
    let start = match line {
        0 => 0,
        n => content
            .match_indices('\n')
            .nth(n - 1)
            .map_or(content.len(), |(i, _)| i + 1),
    };
    let end = content[start..]
        .find('\n')
        .map_or(content.len(), |i| start + i);
    let mut at = (start + index).min(end);
    while !content.is_char_boundary(at) {
        at -= 1;
    }
    at
}

impl<'a, M, Theme, Renderer> Selectable<'a, M, Theme, Renderer>
where
    Renderer: text::Renderer,
{
    /// The inner text's paragraph, as its layout left it.
    fn plain<'t>(&self, tree: &'t Tree) -> Option<&'t text::paragraph::Plain<Renderer::Paragraph>> {
        let inner = tree.children.first()?;
        (inner.tag == tree::Tag::of::<text::paragraph::Plain<Renderer::Paragraph>>()).then(|| {
            inner
                .state
                .downcast_ref::<text::paragraph::Plain<Renderer::Paragraph>>()
        })
    }

    /// The second paragraph for the text and the range as they are now,
    /// built anew when either or the bounds changed.
    fn cut(&self, tree: &mut Tree) {
        let Some(plain) = self.plain(tree) else {
            return;
        };
        let text = plain.as_text();
        let bounds = text.bounds;
        let fresh = tree
            .state
            .downcast_ref::<State<Renderer::Paragraph>>()
            .cut
            .as_ref()
            .is_some_and(|c| {
                c.content == self.content && c.range == self.range && c.bounds == bounds
            });
        if fresh {
            return;
        }
        let starts = span_starts(self.content, self.range.as_ref());
        let spans: Vec<Span<'_, (), Renderer::Font>> = starts
            .iter()
            .enumerate()
            .map(|(n, &s)| {
                let e = starts.get(n + 1).copied().unwrap_or(self.content.len());
                Span::new(&self.content[s..e])
            })
            .collect();
        let paragraph = Renderer::Paragraph::with_spans(iced::advanced::Text {
            content: spans.as_slice(),
            bounds,
            size: text.size,
            line_height: text.line_height,
            font: text.font,
            align_x: text.align_x,
            align_y: text.align_y,
            shaping: text.shaping,
            wrapping: text.wrapping,
        });
        tree.state.downcast_mut::<State<Renderer::Paragraph>>().cut = Some(Cut {
            paragraph,
            content: self.content.to_string(),
            range: self.range.clone(),
            bounds,
            starts,
        });
    }

    /// The byte under `point`, relative to the text's top left.
    fn hit(&self, tree: &Tree, point: Point) -> Option<usize> {
        let cut = tree
            .state
            .downcast_ref::<State<Renderer::Paragraph>>()
            .cut
            .as_ref()?;
        let line_of = |span: usize| self.content[..cut.starts[span]].matches('\n').count();
        let index = cut.paragraph.hit_test(point).map(|h| h.cursor());
        // On a glyph the span says the line.
        if let Some(span) = cut.paragraph.hit_span(point) {
            return Some(offset_of(self.content, line_of(span), index.unwrap_or(0)));
        }
        // Beside the glyphs, the span nearest in height.
        let mut best: Option<(f32, usize)> = None;
        for span in 0..cut.starts.len() {
            for r in cut.paragraph.span_bounds(span) {
                let d = if point.y < r.y {
                    r.y - point.y
                } else if point.y > r.y + r.height {
                    point.y - r.y - r.height
                } else {
                    0.0
                };
                if best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, span));
                }
            }
        }
        match best {
            Some((_, span)) => Some(offset_of(
                self.content,
                line_of(span),
                index.unwrap_or(usize::MAX),
            )),
            None => Some(0),
        }
    }
}

impl<M, Theme, Renderer> Widget<M, Theme, Renderer> for Selectable<'_, M, Theme, Renderer>
where
    Renderer: text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Renderer::Paragraph>::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.inner)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.inner));
    }

    fn size(&self) -> Size<Length> {
        self.inner.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = self
            .inner
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        // Only a selected text keeps its second paragraph; the rest drop
        // theirs, so a long chat holds one per text it shows selected.
        match self.range.is_some() {
            true => self.cut(tree),
            false => {
                let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
                if !state.pressed {
                    state.cut = None;
                }
            }
        }
        node
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.inner
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
        let over = cursor
            .position_over(bounds)
            .filter(|p| viewport.contains(*p));
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                tree.state
                    .downcast_mut::<State<Renderer::Paragraph>>()
                    .shift = m.shift();
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = over {
                    self.cut(tree);
                    if let Some(at) = self.hit(tree, p - Vector::new(bounds.x, bounds.y)) {
                        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
                        let click = Click::new(p, mouse::Button::Left, state.last_click);
                        state.last_click = Some(click);
                        state.pressed = true;
                        state.last_drag = Some(at);
                        let clicks = match click.kind() {
                            ClickKind::Single => 1,
                            ClickKind::Double => 2,
                            ClickKind::Triple => 3,
                        };
                        let shift = state.shift;
                        shell.publish((self.on)(Pick::Press { at, clicks, shift }));
                        shell.capture_event();
                        return;
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if self.dragging => {
                if let Some(p) = over {
                    self.cut(tree);
                    if let Some(at) = self.hit(tree, p - Vector::new(bounds.x, bounds.y)) {
                        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
                        if state.last_drag != Some(at) {
                            state.last_drag = Some(at);
                            shell.publish((self.on)(Pick::Drag { at }));
                        }
                    }
                } else {
                    tree.state
                        .downcast_mut::<State<Renderer::Paragraph>>()
                        .last_drag = None;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
                if state.pressed {
                    state.pressed = false;
                    state.last_drag = None;
                    shell.publish((self.on)(Pick::Release));
                }
            }
            _ => {}
        }
        self.inner.as_widget_mut().update(
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
        if cursor.is_over(layout.bounds()) {
            return mouse::Interaction::Text;
        }
        self.inner.as_widget().mouse_interaction(
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
        if let Some(range) = &self.range {
            let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
            if let Some(cut) = state
                .cut
                .as_ref()
                .filter(|c| c.range.as_ref() == Some(range))
            {
                let origin = layout.bounds().position();
                for (span, &start) in cut.starts.iter().enumerate() {
                    if start < range.start || start >= range.end {
                        continue;
                    }
                    for r in cut.paragraph.span_bounds(span) {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle::new(
                                    Point::new(origin.x + r.x, origin.y + r.y),
                                    r.size(),
                                ),
                                ..renderer::Quad::default()
                            },
                            self.highlight,
                        );
                    }
                }
            }
        }
        self.inner.as_widget().draw(
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
        self.inner.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, M, Theme, Renderer> From<Selectable<'a, M, Theme, Renderer>>
    for Element<'a, M, Theme, Renderer>
where
    M: 'a,
    Theme: 'a,
    Renderer: text::Renderer + 'a,
{
    fn from(s: Selectable<'a, M, Theme, Renderer>) -> Self {
        Element::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless;
    use iced::widget::{column, text};
    use iced::Theme;

    const WORDS: &str = "hello wide world";

    fn one(range: Option<Range<usize>>) -> Element<'static, Pick> {
        column![selectable(
            text(WORDS).size(20),
            WORDS,
            range,
            Color::from_rgba(0.0, 0.0, 1.0, 0.3),
            false,
            |p| p,
        )]
        .into()
    }

    fn shot(element: Element<'_, Pick>) -> Vec<u8> {
        headless::render(element, &Theme::Light, Color::BLACK, Color::WHITE, 240, 40)
            .expect("the software renderer")
    }

    /// Wrapped and unselected, the text is drawn pixel for pixel as the
    /// bare text widget draws it; a range draws under it, and only there.
    #[test]
    fn unselected_it_is_the_bare_text_and_a_range_draws_under_it() {
        let bare = shot(column![text(WORDS).size(20)].into());
        assert_eq!(shot(one(None)), bare);
        assert_eq!(shot(one(Some(3..3))), bare, "an empty range draws nothing");
        let picked = shot(one(Some(6..10)));
        assert_ne!(picked, bare);
        // The highlight is blue-ish: some pixel has more blue than red.
        let blue = picked
            .chunks(3)
            .filter(|p| p[2] > p[0].saturating_add(20))
            .count();
        assert!(blue > 50, "the highlight shows: {blue} blue pixels");
        // Nothing left of the selection's start changes: the first word.
        let w = 240usize;
        let first_word_differs = (0..40)
            .flat_map(|y| (0..20).map(move |x| (y * w + x) * 3))
            .any(|i| picked[i..i + 3] != bare[i..i + 3]);
        assert!(!first_word_differs, "\"hello\" stays as it was");
    }

    /// A press on the text is its byte; two in a row a double click, three
    /// a triple; each with its release.
    #[test]
    fn a_double_click_is_two_presses_the_second_counted_twice() {
        let at = Point::new(2.0, 10.0);
        let picks = headless::clicks(one(None), at, 3, 240, 40).expect("a renderer");
        assert_eq!(
            picks,
            vec![
                Pick::Press {
                    at: 0,
                    clicks: 1,
                    shift: false
                },
                Pick::Release,
                Pick::Press {
                    at: 0,
                    clicks: 2,
                    shift: false
                },
                Pick::Release,
                Pick::Press {
                    at: 0,
                    clicks: 3,
                    shift: false
                },
                Pick::Release,
            ]
        );
        // Further right, further into the text; beside the text nothing.
        let mid = headless::click(one(None), Point::new(80.0, 10.0), 240, 40).expect("a renderer");
        let Some(Pick::Press { at, .. }) = mid.first().copied() else {
            panic!("no press: {mid:?}");
        };
        assert!(at > 3 && at < WORDS.len(), "{at}");
        assert_eq!(
            headless::click(one(None), Point::new(230.0, 30.0), 240, 40),
            Some(vec![])
        );
    }

    #[test]
    fn spans_begin_at_lines_and_at_the_selection() {
        assert_eq!(span_starts("ab\ncd", None), vec![0, 3]);
        assert_eq!(span_starts("hello world", Some(&(6..11))), vec![0, 6]);
        assert_eq!(span_starts("hello world", Some(&(2..4))), vec![0, 2, 4]);
        assert_eq!(span_starts("a\nbc\nd", Some(&(1..4))), vec![0, 1, 2, 4, 5]);
        assert_eq!(span_starts("", None), vec![0]);
    }

    #[test]
    fn a_hit_in_a_line_is_a_byte_of_the_text() {
        let text = "one\ntwö\nthree";
        assert_eq!(offset_of(text, 0, 2), 2);
        assert_eq!(offset_of(text, 1, 0), 4);
        // Past the line's end, its end; inside the ö, its start.
        assert_eq!(offset_of(text, 1, 99), 8);
        assert_eq!(offset_of(text, 1, 3), 6);
        assert_eq!(offset_of(text, 2, 5), 14);
        assert_eq!(offset_of(text, 9, 0), text.len());
    }
}
