//! A list that lays out only the rows on screen (1487): Lew's rule of
//! 2026-11-07, only what is on screen is laid out and drawn. iced's
//! column inside a scrollable lays out every child on every layout and
//! hands every event to every child (`column.rs`, `update`: 3000 bubbles,
//! 0.8 to 1.9 ms a frame of the chat's scroll, 1441 and 1487); its draw
//! already skips what is out of view.
//!
//! [`rows`] is that column, for a scrollable, with its children named
//! instead of given: an [`Item`] per row (a key that names the row across
//! frames, and a stamp that changes when what the row shows changes) and
//! a function that builds row `i`. The widget keeps each row's size,
//! measured once (the row built and laid out) and again only when its
//! stamp or the list's width changes; it builds, lays out and hands
//! events only to the rows within the viewport and a margin of half a
//! viewport above and below it, and places them where the column would
//! (the same sums in the same order, so every pixel is where the column
//! put it). Its height is every row's, so the scrollbar's geometry is the
//! column's.
//!
//! Where the rows are: the scrollable hands its content the visible part
//! (`viewport`) with every event, and every frame begins with one
//! (`RedrawRequested`). The widget keeps it; when the visible part comes
//! within a quarter viewport of the edge of what is built, or the built
//! rows reach far past it, it asks for a layout (`invalidate_layout`),
//! which iced runs before the frame is drawn, and the layout builds the
//! rows about the new place. A scroll that moves less than that in a
//! frame lays out nothing. Until the widget has seen a viewport (a frame
//! laid out and drawn without an event, as the goldens are) it builds
//! every row, as the column does. A row the left button or a finger went
//! down on stays built until it comes up, so a selection's drag that
//! scrolls its start out of sight still hears its release.
//!
//! With [`Rows::anchor_end`] (a chat, its scrollable anchored at the
//! bottom) the place is kept as its distance from the end, as the
//! scrollable keeps its offset, so rows added at the end lay out where
//! the scrollable will show them.
//!
//! With [`Rows::on_shown`] (1457) the widget says which rows have their end
//! on screen, by the read rule the terminal applies in rows
//! ([`leviculum_ui::read::end_shown`]), each time that set changed and then
//! held still for [`SETTLE`]: the window's chat tells the model what was
//! read from it. Not on every frame of a scroll: a message published makes
//! iced build the whole view again, which 1431 took out of the scroll's
//! frames, and a row that flies past is not read. The widget asks for a
//! frame at the end of the wait, so the set is said without another event.

use std::collections::HashMap;

use iced::advanced::overlay::Group;
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{layout, overlay, renderer, Clipboard, Layout, Shell, Widget};
use iced::time::{Duration, Instant};
use iced::{mouse, touch, window, Element, Event, Length, Padding, Rectangle, Size, Vector};

/// A row of the list: `key` names it from frame to frame (unique within
/// the list), `stamp` changes whenever what it shows could change its
/// size (its words, what it carries, the reader's language).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Item {
    pub key: u64,
    pub stamp: u64,
}

/// Rows `items`, row `i` built by `build(i)`, laid out as a column with
/// no spacing, a list in a vertical scrollable.
pub fn rows<'a, M, Theme, Renderer>(
    items: Vec<Item>,
    build: impl Fn(usize) -> Element<'a, M, Theme, Renderer> + 'a,
) -> Rows<'a, M, Theme, Renderer> {
    Rows {
        items,
        build: Box::new(build),
        padding: Padding::ZERO,
        anchor_end: false,
        on_shown: None,
        built: Vec::new(),
        laid: false,
    }
}

/// The list [`rows`] makes.
pub struct Rows<'a, M, Theme, Renderer> {
    items: Vec<Item>,
    build: Box<dyn Fn(usize) -> Element<'a, M, Theme, Renderer> + 'a>,
    padding: Padding,
    anchor_end: bool,
    /// What the rows whose end is on screen are told as, by their places
    /// in `items` (1457).
    #[allow(clippy::type_complexity)]
    on_shown: Option<Box<dyn Fn(Vec<usize>) -> M + 'a>>,
    /// The rows the last layout built: their place in `items` and the
    /// element, in order; `tree.children` holds their states in the same
    /// order and the layout's children their places.
    built: Vec<(usize, Element<'a, M, Theme, Renderer>)>,
    /// This view's rows were laid out once: a layout again (a scroll's)
    /// lays out the same list, so it is not compared with the last.
    laid: bool,
}

impl<'a, M, Theme, Renderer> Rows<'a, M, Theme, Renderer> {
    /// The padding around the rows, as a column's.
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// The scrollable around is anchored at the end (`anchor_bottom`): the
    /// place on screen is kept as its distance from the end.
    pub fn anchor_end(mut self) -> Self {
        self.anchor_end = true;
        self
    }

    /// Say the rows whose end is on screen ([`leviculum_ui::read::end_shown`]
    /// in pixels), by their places in the items, as `shown` of them,
    /// whenever that set changed (1457).
    pub fn on_shown(mut self, shown: impl Fn(Vec<usize>) -> M + 'a) -> Self {
        self.on_shown = Some(Box::new(shown));
        self
    }
}

/// Where the visible part was, in the list's own coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Seen {
    /// The visible part's top below the list's top.
    top: f32,
    height: f32,
    /// Below the visible part's bottom, the list's height left.
    from_end: f32,
}

/// A row's size as last measured, and the stamp it was measured under.
#[derive(Clone, Copy, Debug)]
struct Measure {
    stamp: u64,
    size: Size,
}

/// What a test counts (1487): the rows the widget built and laid out, the
/// rows it handed an event, the events it was handed, the layouts it ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub built: usize,
    pub laid_out: usize,
    pub handed: usize,
    pub events: usize,
    pub layouts: usize,
}

#[derive(Default)]
struct State {
    sizes: HashMap<u64, Measure>,
    /// The width the sizes were measured at.
    width: f32,
    seen: Option<Seen>,
    /// The keys of the rows built, in `tree.children`'s order.
    keys: Vec<u64>,
    /// The span of the list the built rows cover, top and bottom.
    span: (f32, f32),
    /// The list's height at the last layout.
    total: f32,
    /// The row the button or a finger went down on, while it is down.
    held: Option<u64>,
    /// The list as last laid out: its rows, each row's size, and where
    /// each row ends (the column's running sum from the padding's top).
    items: Vec<Item>,
    heights: Vec<Size>,
    ends: Vec<f32>,
    /// The widest row, or the list's width.
    cross: f32,
    /// The padding's top the sums began at.
    pad_top: f32,
    /// The keys of the rows last said shown ([`Rows::on_shown`]), none
    /// before the first.
    shown: Option<Vec<u64>>,
    /// The set on screen now, by keys and places, and since when: said
    /// once it held still for [`SETTLE`].
    pending: Option<(Vec<u64>, Vec<usize>, Instant)>,
    /// The states of rows measured or built before and not built now, by
    /// key: a row that scrolls in again keeps its shaped text, as every
    /// row of a column keeps its own. At most [`PARKED`].
    parked: HashMap<u64, Tree>,
}

/// How many rows' states [`State::parked`] keeps at most; past it, the
/// half furthest from the rows on screen are dropped.
const PARKED: usize = 32_768;

thread_local! {
    static COUNTS: std::cell::Cell<Option<Counts>> = const { std::cell::Cell::new(None) };
}

/// Start counting on this thread what every [`Rows`] does (for a test);
/// [`counted`] reads and resets.
pub fn count(on: bool) {
    COUNTS.with(|c| c.set(on.then(Counts::default)));
}

/// What the [`Rows`] on this thread did since the last call, while
/// [`count`] is on.
pub fn counted() -> Counts {
    COUNTS.with(|c| {
        let now = c.get().unwrap_or_default();
        if c.get().is_some() {
            c.set(Some(Counts::default()));
        }
        now
    })
}

fn tally(f: impl FnOnce(&mut Counts)) {
    COUNTS.with(|c| {
        if let Some(mut n) = c.get() {
            f(&mut n);
            c.set(Some(n));
        }
    });
}

impl State {
    /// The visible part's top for a list `total` high.
    fn top(&self, seen: Seen, total: f32, anchor_end: bool) -> f32 {
        let top = match anchor_end {
            true => total - seen.from_end - seen.height,
            false => seen.top,
        };
        top.min(total - seen.height).max(0.0)
    }
}

/// How long the rows on screen hold still before [`Rows::on_shown`] says
/// them (1457).
pub const SETTLE: Duration = Duration::from_millis(250);

/// The rows whose end lies in the view `top..top + height` (the list's
/// own coordinates, below the padding's top) by the read rule, given each
/// row's end from the padding's corner (`ends`, in order).
fn ends_shown(ends: &[f32], pad_top: f32, top: f32, height: f32) -> Vec<usize> {
    let first = ends.partition_point(|&e| e - pad_top <= top);
    (first..ends.len())
        .take_while(|&i| ends[i] - pad_top <= top + height)
        .filter(|&i| {
            let start = if i == 0 { pad_top } else { ends[i - 1] };
            leviculum_ui::read::end_shown(start - pad_top, ends[i] - pad_top, top, height)
        })
        .collect()
}

/// How far past the visible part, above and below, rows are built: half
/// the visible height.
fn margin(height: f32) -> f32 {
    (height / 2.0).max(64.0)
}

/// A row built and laid out: its state, its place and size, its element.
type Laid<'a, M, Theme, Renderer> = (Tree, layout::Node, Element<'a, M, Theme, Renderer>);

/// `build(i)`, reconciled with `tree` and laid out within `limits`.
fn build_row<'a, M, Theme, Renderer>(
    i: usize,
    tree: &mut Tree,
    renderer: &Renderer,
    limits: &layout::Limits,
    build: &dyn Fn(usize) -> Element<'a, M, Theme, Renderer>,
) -> (layout::Node, Element<'a, M, Theme, Renderer>)
where
    Renderer: renderer::Renderer,
{
    let mut element = build(i);
    tree.diff(&element);
    let node = element.as_widget_mut().layout(tree, renderer, limits);
    tally(|c| {
        c.built += 1;
        c.laid_out += 1;
    });
    (node, element)
}

impl<M, Theme, Renderer> Widget<M, Theme, Renderer> for Rows<'_, M, Theme, Renderer>
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
        Vec::new()
    }

    /// The rows' states stay: a new view's rows are built at its layout
    /// and reconciled there with the state of the row of the same key.
    fn diff(&self, _tree: &mut Tree) {}

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        tally(|c| c.layouts += 1);
        // As `layout::flex::resolve` lays out a column of width Fill and
        // height Shrink without spacing: every row within the width less
        // the padding, compressed as the scrollable compresses, placed
        // one under the other from the padding's corner.
        let limits = limits
            .width(Length::Fill)
            .height(Length::Shrink)
            .shrink(self.padding);
        let max_cross = limits.max().width;
        let compression = Size::new(false, limits.compression().height);
        let available = limits.max().height;
        let pad_top = self.padding.top;
        let row_limits = |above: f32| {
            layout::Limits::with_compression(
                Size::ZERO,
                Size::new(max_cross, available - above),
                compression,
            )
        };
        let state = tree.state.downcast_mut::<State>();
        if state.width != max_cross || state.pad_top != pad_top {
            state.sizes.clear();
            state.items.clear();
            state.width = max_cross;
            state.pad_top = pad_top;
        }
        // The old rows' states by key, built or parked, to reconcile the
        // new rows with.
        let mut old: HashMap<u64, Tree> = std::mem::take(&mut state.parked);
        old.extend(
            std::mem::take(&mut state.keys)
                .into_iter()
                .zip(std::mem::take(&mut tree.children)),
        );
        let state = tree.state.downcast_mut::<State>();
        let all = state.seen.is_none();
        let changed = all
            || state.items.len() != self.items.len()
            || (!self.laid && state.items != self.items);
        self.laid = true;
        // Rows built and laid out to be measured, kept for the window.
        let mut fresh: HashMap<usize, Laid<'_, M, Theme, Renderer>> = HashMap::new();
        // The list changed since the last layout (a new view with other
        // rows or stamps), or every row is built: each row's size again,
        // measured where its stamp moved, and the column's sums.
        if changed {
            let mut above = 0.0;
            state.heights.clear();
            state.ends.clear();
            let mut main = pad_top;
            for (i, item) in self.items.iter().enumerate() {
                let known = state
                    .sizes
                    .get(&item.key)
                    .filter(|m| m.stamp == item.stamp)
                    .map(|m| m.size);
                let size = match known {
                    Some(size) if !all => size,
                    _ => {
                        let mut row_tree = old.remove(&item.key).unwrap_or_else(Tree::empty);
                        let (node, element) =
                            build_row(i, &mut row_tree, renderer, &row_limits(above), &self.build);
                        let size = node.size();
                        state.sizes.insert(
                            item.key,
                            Measure {
                                stamp: item.stamp,
                                size,
                            },
                        );
                        fresh.insert(i, (row_tree, node, element));
                        size
                    }
                };
                above += size.height;
                main += size.height;
                state.heights.push(size);
                state.ends.push(main);
            }
            state.items.clone_from(&self.items);
            state.cross = state
                .heights
                .iter()
                .fold(max_cross, |cross, size| cross.max(size.width));
            // Sizes of rows gone from the list are forgotten once they
            // outnumber the list.
            if state.sizes.len() > 2 * self.items.len() + 64 {
                let present: std::collections::HashSet<u64> =
                    self.items.iter().map(|item| item.key).collect();
                state.sizes.retain(|key, _| present.contains(key));
            }
        }
        let n = self.items.len();
        let end = state.ends.last().copied().unwrap_or(pad_top);
        let top_of = |ends: &[f32], i: usize| if i == 0 { pad_top } else { ends[i - 1] };
        // The window: the visible part and a margin, or every row; the
        // rows within it found by halving, the sums being in order.
        let (first, last) = match state.seen {
            Some(seen) if !all => {
                let top = state.top(seen, end - pad_top, self.anchor_end) + pad_top;
                let m = margin(seen.height);
                let (w0, w1) = (top - m, top + seen.height + m);
                let first = state.ends.partition_point(|&e| e < w0);
                let last = match pad_top <= w1 {
                    true => 1 + state.ends[..n.saturating_sub(1)].partition_point(|&e| e <= w1),
                    false => 0,
                };
                (first, last.min(n))
            }
            _ => (0, n),
        };
        let held = state
            .held
            .and_then(|key| self.items.iter().position(|item| item.key == key))
            .filter(|i| *i < first || *i >= last);
        let window = held
            .into_iter()
            .filter(|i| *i < first)
            .chain(first..last.max(first))
            .chain(held.into_iter().filter(|i| *i >= last));
        let mut built = Vec::new();
        let mut keys = Vec::new();
        let mut children = Vec::new();
        let mut nodes = Vec::new();
        let mut moved: Option<usize> = None;
        for i in window {
            let item = self.items[i];
            let (row_tree, node, element) = match fresh.remove(&i) {
                Some(done) => done,
                None => {
                    let mut row_tree = old.remove(&item.key).unwrap_or_else(Tree::empty);
                    let above = top_of(&state.ends, i) - pad_top;
                    let (node, element) =
                        build_row(i, &mut row_tree, renderer, &row_limits(above), &self.build);
                    (row_tree, node, element)
                }
            };
            let size = node.size();
            // A row laid out now may measure other than it did: the sums
            // below it move.
            if state.heights[i] != size {
                state.heights[i] = size;
                state.cross = state.cross.max(size.width);
                moved = Some(moved.map_or(i, |m| m.min(i)));
                state.sizes.insert(
                    item.key,
                    Measure {
                        stamp: item.stamp,
                        size,
                    },
                );
            }
            built.push((i, element));
            keys.push(item.key);
            children.push(row_tree);
            nodes.push(node);
        }
        if let Some(from) = moved {
            let mut main = top_of(&state.ends, from);
            for i in from..n {
                main += state.heights[i].height;
                state.ends[i] = main;
            }
        }
        // What was measured or built and is not built now is parked, the
        // rows still in the list only.
        old.extend(
            fresh
                .into_iter()
                .map(|(i, (row_tree, _, _))| (self.items[i].key, row_tree)),
        );
        if changed {
            let present: std::collections::HashSet<u64> =
                self.items.iter().map(|item| item.key).collect();
            old.retain(|key, _| present.contains(key));
        }
        // Past the bound, the half furthest from the window goes at once,
        // so that the walk over the list is paid once in many layouts and
        // the rows the reader scrolls to next keep their states. Not while
        // every row is built: there is no window to be near yet.
        if old.len() > PARKED && !all {
            let distance = |i: usize| match i < first {
                true => first - i,
                false => i.saturating_sub(last),
            };
            let mut parked: Vec<(usize, u64)> = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| old.contains_key(&item.key))
                .map(|(i, item)| (distance(i), item.key))
                .collect();
            let keep = PARKED / 2;
            if parked.len() > keep {
                parked.select_nth_unstable(keep);
                for (_, key) in &parked[keep..] {
                    old.remove(key);
                }
            }
        }
        state.parked = old;
        for ((i, _), node) in built.iter().zip(&mut nodes) {
            node.move_to_mut((self.padding.left, top_of(&state.ends, *i)));
        }
        let end = state.ends.last().copied().unwrap_or(pad_top);
        state.span = match (nodes.first(), nodes.last()) {
            (Some(first), Some(last)) => (
                first.bounds().y - pad_top,
                last.bounds().y + last.bounds().height - pad_top,
            ),
            _ => (0.0, 0.0),
        };
        state.total = end - pad_top;
        state.keys = keys;
        let cross = state.cross;
        tree.children = children;
        self.built = built;
        let size = limits.resolve(
            Length::Fill,
            Length::Shrink,
            Size::new(cross, end - pad_top),
        );
        layout::Node::with_children(size.expand(self.padding), nodes)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for (((_, child), state), layout) in self
                .built
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(state, layout, renderer, operation);
            }
        });
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
        let pad_top = self.padding.top;
        let state = tree.state.downcast_mut::<State>();
        // Where the visible part is now, in the list's coordinates.
        let top = viewport.y - bounds.y - pad_top;
        let seen = Seen {
            top,
            height: viewport.height,
            from_end: state.total - top - viewport.height,
        };
        let first = state.seen.is_none();
        state.seen = Some(seen);
        // The rows whose end is on screen (1457), said once they changed
        // and held still: a frame's own time, else the clock's.
        if let Some(on_shown) = &self.on_shown {
            if state.ends.len() == self.items.len() {
                let now = match event {
                    Event::Window(window::Event::RedrawRequested(at)) => *at,
                    _ => Instant::now(),
                };
                let shown = ends_shown(&state.ends, pad_top, top, viewport.height);
                let keys: Vec<u64> = shown.iter().map(|&i| self.items[i].key).collect();
                let moved = state.pending.as_ref().is_none_or(|(p, _, _)| *p != keys);
                if moved {
                    if state.shown.as_ref() != Some(&keys) {
                        shell.request_redraw_at(window::RedrawRequest::At(now + SETTLE));
                    }
                    state.pending = Some((keys, shown, now));
                }
                if let Some((keys, shown, since)) = &state.pending {
                    if state.shown.as_ref() != Some(keys)
                        && now.saturating_duration_since(*since) >= SETTLE
                    {
                        state.shown = Some(keys.clone());
                        shell.publish(on_shown(shown.clone()));
                    }
                }
            }
        }
        let (lo, hi) = (top.max(0.0), (top + viewport.height).min(state.total));
        let edge = margin(viewport.height) / 2.0;
        let short = lo - edge < state.span.0 - 0.5 && state.span.0 > 0.0
            || hi + edge > state.span.1 + 0.5 && state.span.1 < state.total - 0.5;
        let far = state.span.0 < lo - 3.0 * margin(viewport.height)
            || state.span.1 > hi + 3.0 * margin(viewport.height);
        if first || short || far {
            shell.invalidate_layout();
        }
        // The row a press went down on stays built until the release.
        let pressed = matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(_))
                | Event::Touch(touch::Event::FingerPressed { .. })
        );
        let released = matches!(
            event,
            Event::Mouse(mouse::Event::ButtonReleased(_))
                | Event::Touch(touch::Event::FingerLifted { .. })
                | Event::Touch(touch::Event::FingerLost { .. })
        );
        if pressed {
            state.held = layout
                .children()
                .zip(&state.keys)
                .find(|(row, _)| cursor.is_over(row.bounds()))
                .map(|(_, key)| *key);
        }
        let n = self.built.len();
        for (((_, child), row_tree), row) in self
            .built
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                row_tree, event, row, cursor, renderer, clipboard, shell, viewport,
            );
        }
        tally(|c| {
            c.handed += n;
            c.events += 1;
        });
        if released {
            let state = tree.state.downcast_mut::<State>();
            state.held = None;
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.built
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|(((_, child), tree), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(tree, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        if layout.bounds().intersection(viewport).is_none() {
            return;
        }
        for (((_, child), tree), layout) in self
            .built
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .filter(|(_, layout)| layout.bounds().intersects(viewport))
        {
            child
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        let children = self
            .built
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
            .filter_map(|(((_, child), state), layout)| {
                child
                    .as_widget_mut()
                    .overlay(state, layout, renderer, viewport, translation)
            })
            .collect::<Vec<_>>();
        (!children.is_empty()).then(|| Group::with_children(children).overlay())
    }
}

impl<'a, M: 'a, Theme: 'a, Renderer> From<Rows<'a, M, Theme, Renderer>>
    for Element<'a, M, Theme, Renderer>
where
    Renderer: renderer::Renderer + 'a,
{
    fn from(rows: Rows<'a, M, Theme, Renderer>) -> Self {
        Element::new(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::software;
    use crate::select::{selectable, Pick};
    use iced::advanced::clipboard;
    use iced::advanced::renderer::Headless as _;
    use iced::widget::{column, container, scrollable, text, Space};
    use iced::{window, Background, Color, Point, Renderer, Theme};
    use iced_runtime::user_interface::{Cache, UserInterface};

    type El<'a> = Element<'a, Pick, Theme, Renderer>;

    /// Row `i` of a test list: rows of three kinds and many heights, each
    /// on its own ground so a row out of place shows.
    fn row(i: usize) -> El<'static> {
        let words = "word ".repeat(1 + (i * 7) % 40);
        let ground = Color::from_rgb8((i * 37 % 256) as u8, (i * 91 % 256) as u8, 200);
        match i % 3 {
            0 => Space::new().height((i % 13) as f32 * 1.7).into(),
            1 => container(text(format!("{i}: {words}")).size(15))
                .padding([3, 8])
                .style(move |_: &Theme| container::Style {
                    background: Some(Background::Color(ground)),
                    ..container::Style::default()
                })
                .into(),
            _ => container(selectable(
                text(format!("{i} pick")).size(13.3),
                format!("{i} pick"),
                None,
                Color::BLACK,
                false,
                |pick| pick,
            ))
            .center_x(Length::Fill)
            .padding(iced::Padding::ZERO.top(5.5).bottom(2.25))
            .into(),
        }
    }

    fn items(n: usize) -> Vec<Item> {
        (0..n)
            .map(|i| Item {
                key: i as u64,
                stamp: 0,
            })
            .collect()
    }

    /// The list as a column, or as [`rows`], in a scrollable.
    fn list(n: usize, virtual_: bool, end: bool) -> El<'static> {
        let content: El<'static> = match virtual_ {
            true => {
                let r = rows(items(n), row).padding([0, 12]);
                match end {
                    true => r.anchor_end().into(),
                    false => r.into(),
                }
            }
            false => (0..n)
                .fold(column![].padding([0, 12]), |c, i| c.push(row(i)))
                .into(),
        };
        let s = scrollable(content).height(Length::Fill).width(Length::Fill);
        match end {
            true => s.anchor_bottom().into(),
            false => s.into(),
        }
    }

    const W: f32 = 420.0;
    const H: f32 = 300.0;

    fn redraw() -> Event {
        Event::Window(window::Event::RedrawRequested(std::time::Instant::now()))
    }

    fn wheel(y: f32) -> Event {
        Event::Mouse(mouse::Event::WheelScrolled {
            delta: mouse::ScrollDelta::Pixels { x: 0.0, y },
        })
    }

    /// `events` through iced's user interface as its loop runs them, every
    /// frame drawn after a redraw: the frames' pixels and the messages.
    fn frames(view: El<'_>, events: &[Event], at: Point) -> (Vec<Vec<u8>>, Vec<Pick>) {
        let mut renderer = software(iced::Font::DEFAULT).expect("a renderer");
        let size = Size::new(W, H);
        let mut ui = UserInterface::build(view, size, Cache::default(), &mut renderer);
        let cursor = mouse::Cursor::Available(at);
        let mut out = Vec::new();
        let mut messages = Vec::new();
        for event in events {
            let _ = ui.update(
                std::slice::from_ref(event),
                cursor,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
                ui.draw(
                    &mut renderer,
                    &Theme::Light,
                    &renderer::Style {
                        text_color: Color::BLACK,
                    },
                    cursor,
                );
                out.push(renderer.screenshot(
                    iced::Size::new(W as u32, H as u32),
                    1.0,
                    Color::WHITE,
                ));
            }
        }
        (out, messages)
    }

    /// A scroll in steps of `by`, `n` of them, each frame drawn.
    fn scroll(by: f32, n: usize) -> Vec<Event> {
        let mut events = vec![redraw()];
        for _ in 0..n {
            events.push(wheel(by));
            events.push(redraw());
        }
        events
    }

    /// 1457: the rows whose end is on screen are said, by the read rule,
    /// once they held still for [`SETTLE`], and again only when the set
    /// moved: rows of 50 pixels in a view of 300 have rows 0 to 5 ending
    /// in it (the sixth's end on the bottom edge counts), after a scroll of
    /// 75 rows 1 to 6 (row 0 ended above, row 7 ends below). A frame
    /// before the wait is over says nothing, nor one that moved nothing.
    /// Control: the list without [`Rows::on_shown`] says nothing at all.
    #[test]
    fn the_rows_whose_end_is_on_screen_are_said_when_they_change() {
        type Shown = Vec<usize>;
        let run = |said: bool| -> (Vec<Shown>, Vec<usize>) {
            let r = rows(items(40), |_| Space::new().height(50.0).into());
            let r = match said {
                true => r.on_shown(|shown| shown),
                false => r,
            };
            let view: Element<'_, Shown, Theme, Renderer> = scrollable(r)
                .height(Length::Fill)
                .width(Length::Fill)
                .into();
            let mut renderer = software(iced::Font::DEFAULT).expect("a renderer");
            let mut ui =
                UserInterface::build(view, Size::new(W, H), Cache::default(), &mut renderer);
            let cursor = mouse::Cursor::Available(Point::new(W / 2.0, H / 2.0));
            let t0 = Instant::now();
            let at = |ms: u64| {
                Event::Window(window::Event::RedrawRequested(
                    t0 + Duration::from_millis(ms),
                ))
            };
            let events = [
                at(0),
                at(100),
                at(300),
                wheel(-75.0),
                at(400),
                at(600),
                at(700),
                at(900),
            ];
            let mut messages = Vec::new();
            let mut per_event = Vec::new();
            for event in events {
                let before = messages.len();
                let _ = ui.update(
                    std::slice::from_ref(&event),
                    cursor,
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
                per_event.push(messages.len() - before);
            }
            (messages, per_event)
        };
        let (said, per_event) = run(true);
        assert_eq!(said, [vec![0, 1, 2, 3, 4, 5], vec![1, 2, 3, 4, 5, 6]]);
        // Said 250 ms after the set came: at 300 for the one of 0 ms, at
        // 700 for the one the scroll brought at 400.
        assert_eq!(per_event, [0, 0, 1, 0, 0, 0, 1, 0]);
        assert!(run(false).0.is_empty());
    }

    /// The rows draw every frame of a scroll as the column draws it, to the
    /// byte: down from the top in steps of every size (a third of a pixel,
    /// a row, past the margin, a screen and more), and up from the bottom
    /// with the scrollable anchored there.
    #[test]
    fn the_rows_draw_every_frame_as_the_column_does() {
        let at = Point::new(W / 2.0, H / 2.0);
        for (end, by) in [
            (false, -0.33),
            (false, -17.0),
            (false, -97.0),
            (false, -260.0),
            (false, -700.0),
            (true, 0.33),
            (true, 41.0),
            (true, 333.0),
        ] {
            let events = scroll(by, 24);
            let (column, _) = frames(list(400, false, end), &events, at);
            let (rows, _) = frames(list(400, true, end), &events, at);
            assert_eq!(column.len(), rows.len());
            for (n, (a, b)) in column.iter().zip(&rows).enumerate() {
                assert!(
                    a == b,
                    "frame {n} of a scroll by {by} (anchored at the end: {end}) differs"
                );
            }
            // The control: the scroll moved what was drawn.
            assert!(
                column.first() != column.last(),
                "the scroll by {by} moved nothing"
            );
        }
    }

    /// After the first frame, a frame of the scroll hands its event to the
    /// rows within the viewport and its margin only, and lays out only
    /// those it had not built: the same for 300 rows and for 30,000.
    #[test]
    fn a_frame_costs_the_same_rows_for_300_and_30000() {
        let at = Point::new(W / 2.0, H / 2.0);
        let mut per_frame = Vec::new();
        for n in [300, 30_000] {
            let mut renderer = software(iced::Font::DEFAULT).expect("a renderer");
            let mut ui = UserInterface::build(
                list(n, true, false),
                Size::new(W, H),
                Cache::default(),
                &mut renderer,
            );
            let cursor = mouse::Cursor::Available(at);
            let mut messages: Vec<Pick> = Vec::new();
            let mut step = |events: &[Event]| {
                let _ = ui.update(
                    events,
                    cursor,
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
            };
            // The first frame: every row measured once.
            step(&[redraw()]);
            count(true);
            let mut seen = Vec::new();
            for _ in 0..40 {
                step(&[wheel(-23.0)]);
                step(&[redraw()]);
                seen.push(counted());
            }
            count(false);
            let most_handed = seen.iter().map(|c| c.handed).max().unwrap_or(0);
            let built: usize = seen.iter().map(|c| c.built).sum();
            println!("{n} rows: at most {most_handed} handed an event a frame, {built} built over 40 frames");
            assert!(
                most_handed < 120,
                "{n} rows: {most_handed} rows handed an event in a frame"
            );
            per_frame.push(seen);
        }
        assert_eq!(
            per_frame[0], per_frame[1],
            "300 and 30,000 rows cost different frames"
        );
    }

    /// The column hands every frame's event to all its rows: the bar the
    /// rows are measured against.
    #[test]
    fn a_row_measured_once_is_not_built_again_until_its_stamp_moves() {
        let mut renderer = software(iced::Font::DEFAULT).expect("a renderer");
        let size = Size::new(W, H);
        let at = mouse::Cursor::Available(Point::new(W / 2.0, H / 2.0));
        let mut messages: Vec<Pick> = Vec::new();
        count(true);
        let mut ui = UserInterface::build(
            list(500, true, false),
            size,
            Cache::default(),
            &mut renderer,
        );
        let _ = ui.update(
            &[redraw()],
            at,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let first = counted();
        // A new view of the same rows: only the rows in view are built.
        let cache = ui.into_cache();
        let mut ui = UserInterface::build(list(500, true, false), size, cache, &mut renderer);
        let _ = ui.update(
            &[redraw()],
            at,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let again = counted();
        // The same view with every stamp moved: all of them once more.
        let cache = ui.into_cache();
        let moved = rows(
            items(500)
                .into_iter()
                .map(|i| Item { stamp: 1, ..i })
                .collect(),
            row,
        )
        .padding([0, 12]);
        let view: El<'_> = scrollable(moved).height(Length::Fill).into();
        let ui = UserInterface::build(view, size, cache, &mut renderer);
        let stamped = counted();
        count(false);
        drop(ui);
        println!("first {first:?}, a new view {again:?}, new stamps {stamped:?}");
        assert!(first.built >= 500, "{first:?}");
        assert!(again.built < 100, "{again:?}");
        assert!(stamped.built >= 500, "{stamped:?}");
    }

    /// A row on screen that grew while its stamp stayed (a stamp that
    /// missed a change): its new height is measured where it is laid out,
    /// and the rows under it move as they do in the column.
    #[test]
    fn a_row_that_grew_under_its_old_stamp_moves_the_rows_below() {
        let grown = |i: usize| -> El<'static> {
            match i {
                7 => text("seven\nseven\nseven\nseven").size(15).into(),
                _ => row(i),
            }
        };
        let at = mouse::Cursor::Available(Point::new(W / 2.0, H / 2.0));
        let shoot = |ui: &mut UserInterface<'_, Pick, Theme, Renderer>, renderer: &mut Renderer| {
            ui.draw(
                renderer,
                &Theme::Light,
                &renderer::Style {
                    text_color: Color::BLACK,
                },
                at,
            );
            renderer.screenshot(iced::Size::new(W as u32, H as u32), 1.0, Color::WHITE)
        };
        let mut renderer = software(iced::Font::DEFAULT).expect("a renderer");
        let size = Size::new(W, H);
        let mut messages = Vec::new();
        let mut ui = UserInterface::build(
            list(300, true, false),
            size,
            Cache::default(),
            &mut renderer,
        );
        let _ = ui.update(
            &[redraw()],
            at,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let before = shoot(&mut ui, &mut renderer);
        let cache = ui.into_cache();
        let view: El<'_> = scrollable(rows(items(300), grown).padding([0, 12]))
            .height(Length::Fill)
            .width(Length::Fill)
            .into();
        let mut ui = UserInterface::build(view, size, cache, &mut renderer);
        let _ = ui.update(
            &[redraw()],
            at,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let after = shoot(&mut ui, &mut renderer);
        let column: El<'_> =
            scrollable((0..300).fold(column![].padding([0, 12]), |c, i| c.push(grown(i))))
                .height(Length::Fill)
                .width(Length::Fill)
                .into();
        let mut ui = UserInterface::build(column, size, Cache::default(), &mut renderer);
        let _ = ui.update(
            &[redraw()],
            at,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let expected = shoot(&mut ui, &mut renderer);
        assert!(before != after, "the grown row changed nothing on screen");
        assert!(
            after == expected,
            "the rows under the grown row are not where the column puts them"
        );
    }

    /// A selection pressed on a row, the list then scrolled far past it:
    /// the row stays built, and its release reaches it.
    #[test]
    fn a_pressed_row_hears_its_release_after_it_scrolled_away() {
        // Where a selectable line is near the top: the first height a
        // press on the column picks at.
        let press = vec![
            redraw(),
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        ];
        let y = (0..120)
            .map(|y| y as f32)
            .find(|y| {
                !frames(list(400, false, false), &press, Point::new(W / 2.0, *y))
                    .1
                    .is_empty()
            })
            .expect("a selectable line near the top");
        let mut events = press;
        events.extend(scroll(-500.0, 6));
        events.push(Event::Mouse(mouse::Event::ButtonReleased(
            mouse::Button::Left,
        )));
        events.push(redraw());
        let at = Point::new(W / 2.0, y);
        let (_, virtual_) = frames(list(400, true, false), &events, at);
        let (_, column) = frames(list(400, false, false), &events, at);
        println!("rows: {virtual_:?}; column: {column:?}");
        assert!(
            matches!(virtual_.first(), Some(Pick::Press { .. })),
            "{virtual_:?}"
        );
        assert_eq!(virtual_.last(), Some(&Pick::Release), "{virtual_:?}");
        assert_eq!(virtual_, column);
    }
}
