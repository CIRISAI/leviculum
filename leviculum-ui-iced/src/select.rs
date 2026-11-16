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
//! A text inside a button (a list row, a position line that opens the
//! map) is made selectable with [`Selectable::in_button`] (1475): a plain
//! press there is the button's, so the widget says nothing of it and
//! lets it through; the press becomes a selection's only once the pointer
//! moved more than [`DRAG_PX`] from where it went down (then the press is
//! reported where it went down, and the drag from there), or when it is a
//! double or a triple click or Shift was held. The program drops the
//! button's own press that the release of such a drag still sends.
//!
//! The pointer is mapped with a second paragraph of the same text, cut
//! into spans at the newlines and the selection's ends, laid out with the
//! inner text's own font, size and bounds: its spans tell which line a
//! point is on (iced's hit test gives the offset within a line only) and
//! where the selection's rectangles are. It is built only for a text
//! that is selected or under the pointer, never for the others.

use std::borrow::Cow;
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

/// Where an unselected line's highlight quad lies: outside every clip, so
/// it draws nothing and a change of it damages nothing.
const NOWHERE: Rectangle = Rectangle {
    x: -1.0e6,
    y: -1.0e6,
    width: 0.0,
    height: 0.0,
};

/// How far, in logical pixels, the pointer moves from a plain press on a
/// text in a button before the press is a selection's drag.
pub const DRAG_PX: f32 = 4.0;

/// `inner`, a text widget showing `content`, made selectable: `range`
/// drawn in `highlight` under it, the button's [`Pick`]s through `on`.
/// `dragging`: a press went down on some selectable text and the button
/// is still held, so this one reports the pointer over it too.
pub fn selectable<'a, M, Theme, Renderer>(
    inner: impl Into<Element<'a, M, Theme, Renderer>>,
    content: impl Into<Cow<'a, str>>,
    range: Option<Range<usize>>,
    highlight: Color,
    dragging: bool,
    on: impl Fn(Pick) -> M + 'a,
) -> Selectable<'a, M, Theme, Renderer> {
    Selectable {
        inner: inner.into(),
        content: content.into(),
        range: range.filter(|r| r.start < r.end),
        highlight,
        dragging,
        in_button: false,
        on: Box::new(on),
    }
}

impl<M, Theme, Renderer> Selectable<'_, M, Theme, Renderer> {
    /// The text is inside a button: a plain press is the button's, and
    /// only a drag past [`DRAG_PX`], a double or triple click or a
    /// Shift+click selects.
    pub fn in_button(mut self) -> Self {
        self.in_button = true;
        self
    }
}

/// See [`selectable`].
pub struct Selectable<'a, M, Theme, Renderer> {
    inner: Element<'a, M, Theme, Renderer>,
    content: Cow<'a, str>,
    range: Option<Range<usize>>,
    highlight: Color,
    dragging: bool,
    in_button: bool,
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
    /// In a button: a plain press not reported yet, where it went down
    /// and the byte there; it is reported once it is a drag.
    held: Option<(Point, usize)>,
}

impl<P> Default for State<P> {
    fn default() -> Self {
        Self {
            cut: None,
            last_click: None,
            pressed: false,
            shift: false,
            last_drag: None,
            held: None,
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

    /// The height of a line of the text, as its layout set it.
    fn line_height(&self, tree: &Tree) -> Option<f32> {
        let text = self.plain(tree)?.as_text();
        Some(text.line_height.to_absolute(text.size).0).filter(|h| *h > 0.0)
    }

    /// How many visual lines the text was laid out in: one at least.
    fn lines(&self, tree: &Tree) -> usize {
        match (self.plain(tree), self.line_height(tree)) {
            (Some(plain), Some(line)) => {
                ((plain.min_bounds().height / line).round() as usize).max(1)
            }
            _ => 1,
        }
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
                c.content == *self.content && c.range == self.range && c.bounds == bounds
            });
        if fresh {
            return;
        }
        let starts = span_starts(&self.content, self.range.as_ref());
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
            return Some(offset_of(&self.content, line_of(span), index.unwrap_or(0)));
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
                &self.content,
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
                        let clicks = match click.kind() {
                            ClickKind::Single => 1,
                            ClickKind::Double => 2,
                            ClickKind::Triple => 3,
                        };
                        let shift = state.shift;
                        // A plain press on a text in a button is the
                        // button's until it moves.
                        if self.in_button && clicks == 1 && !shift {
                            state.held = Some((p, at));
                            state.pressed = true;
                            state.last_drag = Some(at);
                        } else {
                            state.held = None;
                            state.pressed = true;
                            state.last_drag = Some(at);
                            shell.publish((self.on)(Pick::Press { at, clicks, shift }));
                            shell.capture_event();
                            return;
                        }
                    }
                }
            }
            // A plain press in a button moved far enough: a selection's
            // press where it went down, and the drag from there.
            // (The cursor, not the event's position: inside a scrollable
            // the cursor is the one moved by the scroll, as the press's.)
            Event::Mouse(mouse::Event::CursorMoved { .. })
                if tree
                    .state
                    .downcast_ref::<State<Renderer::Paragraph>>()
                    .held
                    .zip(cursor.position())
                    .is_some_and(|((from, _), now)| from.distance(now) > DRAG_PX) =>
            {
                let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
                if let Some((_, at)) = state.held.take() {
                    shell.publish((self.on)(Pick::Press {
                        at,
                        clicks: 1,
                        shift: false,
                    }));
                }
                if let Some(p) = over {
                    self.cut(tree);
                    if let Some(at) = self.hit(tree, p - Vector::new(bounds.x, bounds.y)) {
                        let state = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
                        if state.last_drag != Some(at) {
                            state.last_drag = Some(at);
                            shell.publish((self.on)(Pick::Drag { at }));
                        }
                    }
                }
            }
            // The program's drag, or this text's own press still held
            // (its first moves come before the program's next view says
            // a drag is on).
            Event::Mouse(mouse::Event::CursorMoved { .. })
                if self.dragging || {
                    let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
                    state.pressed && state.held.is_none()
                } =>
            {
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
                    // A press that stayed the button's: its click, not a
                    // selection's release.
                    if state.held.take().is_none() {
                        shell.publish((self.on)(Pick::Release));
                    }
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
        // One quad a visual line, selected or not (1475): iced's damage
        // compares a layer's quads by their place in the list, so a quad
        // that came or went with the selection moved every quad after it
        // and damaged the window from there down. With a count that is
        // the text's own, a drag damages the lines whose highlight
        // changed and nothing else. An unselected line's quad lies far
        // outside any clip: it draws nothing and damages nothing.
        let origin = layout.bounds().position();
        let mut rows: Vec<Option<Rectangle>> = vec![None; self.lines(tree)];
        if let Some(range) = &self.range {
            let state = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
            if let (Some(cut), Some(line)) = (
                state
                    .cut
                    .as_ref()
                    .filter(|c| c.range.as_ref() == Some(range)),
                self.line_height(tree),
            ) {
                for (span, &start) in cut.starts.iter().enumerate() {
                    if start < range.start || start >= range.end {
                        continue;
                    }
                    for r in cut.paragraph.span_bounds(span) {
                        let n = ((r.y + r.height / 2.0) / line).floor().max(0.0) as usize;
                        if n >= rows.len() {
                            rows.resize(n + 1, None);
                        }
                        rows[n] = Some(rows[n].map_or(r, |had| had.union(&r)));
                    }
                }
            }
        }
        for row in rows {
            let (bounds, colour) = match row {
                Some(r) => (
                    Rectangle::new(Point::new(origin.x + r.x, origin.y + r.y), r.size()),
                    self.highlight,
                ),
                None => (NOWHERE, Color::TRANSPARENT),
            };
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    ..renderer::Quad::default()
                },
                colour,
            );
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

    /// What a text in a button tells, and the button's own press.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Said {
        Pick(Pick),
        Opened,
    }

    /// The words in a button that opens, as a list row's last line.
    fn in_a_button() -> Element<'static, Said> {
        column![crate::control::flat(
            selectable(
                text(WORDS).size(20),
                WORDS,
                None,
                Color::from_rgba(0.0, 0.0, 1.0, 0.3),
                false,
                Said::Pick,
            )
            .in_button(),
            Said::Opened,
        )]
        .into()
    }

    /// In a button (1475): a plain click is the button's alone, a move of
    /// up to [`DRAG_PX`] still is, a drag past it is a selection's press
    /// where the button went down and its drag, and a double click
    /// selects. The release of a drag still reaches the button: the
    /// program drops that open.
    #[test]
    fn in_a_button_a_click_opens_and_a_drag_or_a_double_click_selects() {
        let at = Point::new(4.0, 15.0);
        let said = headless::clicks(in_a_button(), at, 1, 240, 40).expect("a renderer");
        assert_eq!(said, vec![Said::Opened]);
        let still = Point::new(at.x + DRAG_PX - 1.0, at.y);
        let said = headless::drag(in_a_button(), at, still, 2, 240, 40).expect("a renderer");
        assert_eq!(said, vec![Said::Opened], "a shaking hand is a click");
        let far = Point::new(at.x + 80.0, at.y);
        let said = headless::drag(in_a_button(), at, far, 8, 240, 40).expect("a renderer");
        assert!(
            matches!(
                said.first(),
                Some(Said::Pick(Pick::Press {
                    at: 0,
                    clicks: 1,
                    shift: false
                }))
            ),
            "{said:?}"
        );
        assert!(
            said.iter()
                .any(|s| matches!(s, Said::Pick(Pick::Drag { at }) if *at > 3)),
            "{said:?}"
        );
        let release = said.iter().position(|s| *s == Said::Pick(Pick::Release));
        let opened = said.iter().position(|s| *s == Said::Opened);
        assert!(
            matches!((release, opened), (Some(r), Some(o)) if r < o),
            "the release comes first, the button's open after it: {said:?}"
        );
        let said = headless::clicks(in_a_button(), at, 2, 240, 40).expect("a renderer");
        assert_eq!(
            said,
            vec![
                Said::Opened,
                Said::Pick(Pick::Press {
                    at: 0,
                    clicks: 2,
                    shift: false
                }),
                Said::Pick(Pick::Release),
            ],
            "the first click opens, the second selects the word"
        );
    }

    /// Twelve selectable lines, each on its own ground, `picked` the
    /// selected bytes of each.
    fn lines_with(picked: &[Option<Range<usize>>]) -> Element<'static, Pick> {
        let rows = (0..12).fold(column![], |rows, n| {
            let words: &'static str = "a line of words";
            rows.push(
                iced::widget::container(selectable(
                    text(words).size(16),
                    words,
                    picked.get(n).cloned().flatten(),
                    Color::from_rgba(0.0, 0.0, 1.0, 0.3),
                    false,
                    |p| p,
                ))
                .style(|_: &Theme| iced::widget::container::Style {
                    background: Some(iced::Background::Color(Color::from_rgb(0.9, 0.9, 0.8))),
                    ..iced::widget::container::Style::default()
                })
                .height(30),
            )
        });
        rows.into()
    }

    /// A drag that moves the selection's head damages the lines whose
    /// highlight changed and nothing under them (1475): the selection grows
    /// from the fourth line into the fifth, and every damaged rectangle
    /// lies above the sixth. Before, the fifth line's new quad moved every
    /// ground quad after it in the layer's list, and the damage ran to the
    /// window's foot.
    #[test]
    fn a_growing_selection_damages_only_the_lines_it_changed() {
        use crate::headless::{software, Surface};
        use iced::advanced::renderer::Renderer as _;
        let (w, h) = (240u32, 360u32);
        let mut renderer = software(iced::Font::DEFAULT).expect("a renderer");
        let mut surface = Surface::new(w, h).expect("a surface");
        let size = Size::new(w as f32, h as f32);
        // One tree across the frames, as the window keeps it: its texts'
        // paragraphs stay the same, so only what changed is damaged.
        let mut tree: Option<Tree> = None;
        let mut frame = |picked: &[Option<Range<usize>>]| {
            let mut view = lines_with(picked);
            let tree = match &mut tree {
                Some(tree) => {
                    tree.diff(&view);
                    tree
                }
                None => tree.insert(Tree::new(&view)),
            };
            let node = view.as_widget_mut().layout(
                tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, size),
            );
            renderer.reset(Rectangle::with_size(size));
            view.as_widget().draw(
                tree,
                &mut renderer,
                &Theme::Light,
                &renderer::Style {
                    text_color: Color::BLACK,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &Rectangle::with_size(size),
            );
            surface.present(&mut renderer, Color::WHITE);
            surface.rects().to_vec()
        };
        let mut picked = vec![None; 12];
        picked[3] = Some(2..6);
        frame(&picked);
        picked[3] = Some(2..15);
        picked[4] = Some(0..4);
        let damage = frame(&picked);
        assert!(!damage.is_empty(), "the highlight changed");
        let foot = damage.iter().map(|r| r.y + r.height).fold(0.0, f32::max);
        assert!(
            foot <= 5.0 * 30.0 + 2.0,
            "the damage reaches {foot}, past the fifth line: {damage:?}"
        );
    }
}
