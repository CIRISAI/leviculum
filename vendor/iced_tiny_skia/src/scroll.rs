//! Leviculum's patch (1468): a scroll drawn as a shift of the frame on
//! the buffer. When one layer's content is the last frame's moved by a
//! whole number of pixels up or down (a scrollable's content: its quads
//! are stored where they are drawn, its paragraphs carry the scroll's
//! translation), the rows of that layer's rectangle are moved within the
//! buffer, and only what the move cannot give is drawn: the strip the
//! scroll uncovers, what other layers draw over or under the moved
//! rectangle (a scrollbar, a header floating over the list), and what
//! changed in the other layers. The items that reach a kept pixel are
//! the same items, in the same order, a whole number of pixels further,
//! and a glyph's truncated row moved by as much (cosmic-text truncates
//! toward zero). What is left is tiny-skia's: an anti-aliased edge's
//! partial pixel is not the same byte when a clip cuts the shape or at
//! another height, so a kept edge may differ from the edge a whole draw
//! puts there by a step, rarely more (loquax's
//! `a_scroll_drawn_as_a_shift_is_the_scroll_drawn_whole` holds every
//! frame of the chat's scroll gestures to a twin drawn without a shift).

use crate::Primitive;
use crate::core::renderer::Quad;
use crate::core::{Background, Rectangle, Transformation};
use crate::fast::{self, Px};
use crate::graphics::Image;
use crate::graphics::text::Text;
use crate::layer::{Item, Layer};

/// A layer's content moved by `dy` pixels: which rows of the buffer
/// move, and what must be drawn after they did.
#[derive(Debug, Clone, PartialEq)]
pub struct Shift {
    /// The layer's rectangle on the buffer, physical pixels.
    pub pane: Px,
    /// How far its content moved, physical pixels, down positive.
    pub dy: i32,
    /// What to draw after the move, logical: the uncovered strip, the
    /// other layers' items over or under the pane, the other layers'
    /// changes.
    pub damage: Vec<Rectangle>,
}

/// A layer smaller than this share of the window is not tried as the
/// one that scrolled: a shift saves at most its area.
const LEAST_SHARE: f32 = 0.125;

/// How far an item's pixels may reach beyond its bounds, logical: a
/// glyph's ink past its line box (within its clip), a path's
/// anti-aliased edge, a picture's padded edge.
const TEXT_REACH: f32 = fast::TEXT_INK;
const PRIMITIVE_REACH: f32 = 2.0;
const IMAGE_REACH: f32 = 1.0;

/// The shift from `previous` to `current`, the frame drawn last and
/// the one to draw, on a `width` by `height` buffer at `scale`; `None`
/// when no layer's content moved by a pure vertical translation of
/// whole pixels and nothing else.
pub fn find(
    previous: &[Layer],
    current: &[Layer],
    scale: f32,
    width: u32,
    height: u32,
) -> Option<Shift> {
    if previous.len() != current.len() || !(scale > 0.0) {
        return None;
    }
    let window = Rectangle::new(
        crate::core::Point::ORIGIN,
        crate::core::Size::new(width as f32 / scale, height as f32 / scale),
    );
    let mut damages = Vec::with_capacity(current.len());
    for (a, b) in previous.iter().zip(current) {
        if a.bounds != b.bounds {
            return None;
        }
        damages.push(Layer::damage(a, b));
    }
    let least = LEAST_SHARE * window.width * window.height;
    for (index, (a, b)) in previous.iter().zip(current).enumerate() {
        if damages[index].is_empty() {
            continue;
        }
        let Some(bounds) = b.bounds.intersection(&window) else {
            continue;
        };
        if bounds.width * bounds.height < least {
            continue;
        }
        let Some(dy) = candidate(a, b) else {
            continue;
        };
        if let Some(shift) = verify(
            previous, current, &damages, index, bounds, dy, scale, width,
            height,
        ) {
            return Some(shift);
        }
    }
    None
}

/// The rows of `pixels`, a `width` pixels wide RGBA8 buffer, within
/// `shift.pane` moved by `shift.dy`: the rows that stay inside the pane.
pub fn apply(pixels: &mut [u8], width: u32, shift: &Shift) {
    let stride = width as usize * 4;
    let Px { x0, y0, x1, y1 } = shift.pane;
    let (x0, x1) = (x0 as usize * 4, x1 as usize * 4);
    let dy = shift.dy;
    let rows = (y1 - y0) as i64 - i64::from(dy.unsigned_abs());
    if rows <= 0 {
        return;
    }
    let copy = |pixels: &mut [u8], from: usize, to: usize| {
        let (a, b) = (from * stride, to * stride);
        pixels.copy_within(a + x0..a + x1, b + x0);
    };
    let (y0, y1) = (y0 as usize, y1 as usize);
    let n = dy.unsigned_abs() as usize;
    if dy > 0 {
        // Down: from the bottom up, so no source row is overwritten
        // before it is read.
        for to in (y0 + n..y1).rev() {
            copy(pixels, to - n, to);
        }
    } else {
        for to in y0..y1 - n {
            copy(pixels, to + n, to);
        }
    }
}

/// The vertical move the layer's content seems to have made: the
/// translation between its first paragraphs, else between its first
/// quads of the same size and column.
fn candidate(a: &Layer, b: &Layer) -> Option<f32> {
    let translation = |layer: &Layer| {
        layer.text.iter().find_map(|item| {
            item.as_slice().iter().find_map(|text| match text {
                Text::Paragraph { transformation, .. }
                | Text::Editor { transformation, .. } => Some(
                    (item.transformation() * *transformation).translation(),
                ),
                _ => None,
            })
        })
    };
    if let (Some(ta), Some(tb)) = (translation(a), translation(b))
        && ta.x == tb.x
        && ta.y != tb.y
    {
        return Some(tb.y - ta.y);
    }
    let (first, _) = b.quads.first()?;
    a.quads.iter().find_map(|(quad, _)| {
        (quad.bounds.x == first.bounds.x
            && quad.bounds.width == first.bounds.width
            && quad.bounds.height == first.bounds.height
            && quad.bounds.y != first.bounds.y)
            .then(|| first.bounds.y - quad.bounds.y)
    })
}

#[allow(clippy::too_many_arguments)]
fn verify(
    previous: &[Layer],
    current: &[Layer],
    damages: &[Vec<Rectangle>],
    index: usize,
    bounds: Rectangle,
    dy: f32,
    scale: f32,
    width: u32,
    height: u32,
) -> Option<Shift> {
    let dy_px = dy * scale;
    if dy_px.fract() != 0.0 || dy_px.abs() >= bounds.height * scale {
        return None;
    }
    // A pane on whole pixels only: its rows are then the rows its clip
    // covers, moved and not.
    let edges = [bounds.x, bounds.y, bounds.width, bounds.height];
    if edges.iter().any(|v| (v * scale).fract() != 0.0) {
        return None;
    }
    let dy_px = dy_px as i32;
    let pane =
        fast::covered(bounds * Transformation::scale(scale), width, height)?;
    // The rows the move keeps, logical: the pane and the pane moved.
    let moved = Rectangle {
        y: bounds.y + dy,
        ..bounds
    };
    let kept = bounds.intersection(&moved)?;
    let (a, b) = (&previous[index], &current[index]);
    if !same_moved(a, b, kept, dy, scale) {
        return None;
    }
    let mut damage = Vec::new();
    // The strip the move uncovers.
    if dy > 0.0 {
        damage.push(Rectangle {
            height: dy + 1.0,
            ..bounds
        });
    } else {
        damage.push(Rectangle {
            y: bounds.y + bounds.height + dy - 1.0,
            height: -dy + 1.0,
            ..bounds
        });
    }
    // The other layers: their changes, and their items over or under
    // the pane, where they were (moved with the rows) and where they
    // are. Under the pane, a plain quad covering all of it is the same
    // in every row and moves as nothing.
    for (other, (pa, pb)) in previous.iter().zip(current).enumerate() {
        if other == index {
            continue;
        }
        damage.extend(damages[other].iter().copied());
        let under = other < index;
        for (layer, by) in [(pa, dy), (pb, 0.0)] {
            reaches(layer, bounds, under, &mut |r| {
                let r = Rectangle { y: r.y + by, ..r };
                if let Some(r) = r.intersection(&bounds) {
                    damage.push(r);
                }
            });
        }
    }
    Some(Shift {
        pane,
        dy: dy_px,
        damage,
    })
}

/// Every item of `layer` whose pixels may reach into `pane`, as the
/// rectangle they reach; when `under`, not the plain quads that cover
/// the pane whole.
fn reaches(
    layer: &Layer,
    pane: Rectangle,
    under: bool,
    out: &mut dyn FnMut(Rectangle),
) {
    let Some(clip) = layer.bounds.intersection(&pane.expand(TEXT_REACH)) else {
        return;
    };
    let mut put = |r: Rectangle| {
        if let Some(r) = r.intersection(&clip)
            && r.intersects(&pane)
        {
            out(r);
        }
    };
    for (quad, background) in &layer.quads {
        if under && plain_cover(quad, background, pane) {
            continue;
        }
        put(quad_reach(quad));
    }
    for item in &layer.primitives {
        for r in primitive_reach(item) {
            put(r);
        }
    }
    for image in &layer.images {
        put(whole(image.bounds().expand(IMAGE_REACH)));
    }
    for item in &layer.text {
        for r in text_reach(item) {
            put(r);
        }
    }
}

/// A quad of one colour, without a border or a shadow, that covers every
/// pixel of `pane` whole: its rectangle holds the pane, and holds it a
/// pixel beyond the radius of its corners when they are round.
fn plain_cover(quad: &Quad, background: &Background, pane: Rectangle) -> bool {
    let Background::Color(_) = background else {
        return false;
    };
    let radius = <[f32; 4]>::from(quad.border.radius)
        .into_iter()
        .fold(0.0f32, f32::max);
    let margin = if radius > 0.0 { radius + 1.0 } else { 0.0 };
    let border = quad.border.width > 0.0 && quad.border.color.a > 0.0;
    let shadow = quad.shadow.color.a > 0.0;
    !border && !shadow && pane.expand(margin).is_within(&quad.bounds)
}

/// `r` grown to whole logical pixels: the pixels an anti-aliased shape
/// within it can touch (a superset of the physical ones at any scale).
fn whole(r: Rectangle) -> Rectangle {
    let (x0, y0) = (r.x.floor(), r.y.floor());
    let (x1, y1) = ((r.x + r.width).ceil(), (r.y + r.height).ceil());
    Rectangle {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

fn quad_reach(quad: &Quad) -> Rectangle {
    let mut r = quad.bounds;
    if quad.shadow.color.a > 0.0 {
        let s = quad.shadow;
        let blur = s.blur_radius.abs() + 1.0;
        r = r.union(&Rectangle {
            x: quad.bounds.x + s.offset.x - blur,
            y: quad.bounds.y + s.offset.y - blur,
            width: quad.bounds.width + 2.0 * blur,
            height: quad.bounds.height + 2.0 * blur,
        });
    }
    whole(r)
}

fn primitive_reach(item: &Item<Primitive>) -> Vec<Rectangle> {
    let t = item.transformation();
    let clip = item.clip_bounds();
    item.as_slice()
        .iter()
        .filter_map(|p| {
            let stroke = match p {
                Primitive::Stroke { stroke, .. } => stroke.width,
                Primitive::Fill { .. } => 0.0,
            };
            let r =
                (p.visible_bounds().expand(stroke * 0.5 + PRIMITIVE_REACH)) * t;
            whole(r).intersection(&whole(clip))
        })
        .collect()
}

fn text_reach(item: &Item<Text>) -> Vec<Rectangle> {
    let t = item.transformation();
    item.as_slice()
        .iter()
        .filter_map(|text| match text {
            Text::Paragraph {
                paragraph,
                position,
                clip_bounds,
                transformation,
                ..
            } => ink(
                Rectangle::new(*position, paragraph.min_bounds),
                *clip_bounds,
                t * *transformation,
            ),
            Text::Editor {
                editor,
                position,
                clip_bounds,
                transformation,
                ..
            } => ink(
                Rectangle::new(*position, editor.bounds),
                *clip_bounds,
                t * *transformation,
            ),
            Text::Cached {
                bounds,
                clip_bounds,
                ..
            } => ink(*bounds, *clip_bounds, t),
            // Raw text has no bounds: all of the window.
            Text::Raw { .. } => Some(Rectangle::INFINITE),
        })
        .collect()
}

/// Where a text laid out in `bounds`, clipped to `clip`, both drawn
/// through `t`, can put ink: its box with room for glyphs that reach
/// past it, within its clip.
fn ink(
    bounds: Rectangle,
    clip: Rectangle,
    t: Transformation,
) -> Option<Rectangle> {
    whole((bounds * t).expand(TEXT_REACH)).intersection(&whole(clip * t))
}

/// Whether the items of `a` moved by `dy` are the items of `b`, in the
/// same order, wherever they reach into `kept`. A clip is compared as
/// what it lets through of `kept`: a scrollable's content is clipped to
/// its viewport, which stays where it is on the screen while the
/// content moves.
fn same_moved(
    a: &Layer,
    b: &Layer,
    kept: Rectangle,
    dy: f32,
    scale: f32,
) -> bool {
    let shift = Transformation::translate(0.0, dy);
    let near = |r: Rectangle| r.intersects(&kept);
    let moved = |r: Rectangle| Rectangle { y: r.y + dy, ..r };
    let clips = |ca: Rectangle, cb: Rectangle| {
        moved(ca).intersection(&kept) == cb.intersection(&kept)
    };

    let quads_a = a.quads.iter().filter(|(q, _)| near(moved(quad_reach(q))));
    let quads_b = b.quads.iter().filter(|(q, _)| near(quad_reach(q)));
    if !eq_by(quads_a, quads_b, |(qa, ba), (qb, bb)| {
        let mut q = *qa;
        q.bounds = moved(q.bounds);
        q == *qb && ba == bb
    }) {
        return false;
    }

    let prims_a = a
        .primitives
        .iter()
        .filter(|i| primitive_reach(i).into_iter().any(|r| near(moved(r))));
    let prims_b = b
        .primitives
        .iter()
        .filter(|i| primitive_reach(i).into_iter().any(near));
    if !eq_by(prims_a, prims_b, |x, y| match (x, y) {
        (Item::Group(pa, ca, ta), Item::Group(pb, cb, tb)) => {
            clips(*ca, *cb) && shift * *ta == *tb && pa == pb
        }
        (Item::Cached(pa, ca, ta), Item::Cached(pb, cb, tb)) => {
            clips(*ca, *cb)
                && shift * *ta == *tb
                && (std::sync::Arc::ptr_eq(pa, pb) || pa == pb)
        }
        // A primitive drawn without a group has absolute coordinates
        // the move is not applied to: never the same.
        _ => false,
    }) {
        return false;
    }

    let images_a = a
        .images
        .iter()
        .filter(|i| near(moved(i.bounds().expand(IMAGE_REACH))));
    let images_b = b
        .images
        .iter()
        .filter(|i| near(i.bounds().expand(IMAGE_REACH)));
    if !eq_by(images_a, images_b, |x, y| match (x, y) {
        (
            Image::Raster {
                image: ia,
                bounds: ba,
                clip_bounds: ca,
            },
            Image::Raster {
                image: ib,
                bounds: bb,
                clip_bounds: cb,
            },
        ) => ia == ib && moved(*ba) == *bb && clips(*ca, *cb),
        (
            Image::Vector {
                svg: sa,
                bounds: ba,
                clip_bounds: ca,
            },
            Image::Vector {
                svg: sb,
                bounds: bb,
                clip_bounds: cb,
            },
        ) => sa == sb && moved(*ba) == *bb && clips(*ca, *cb),
        _ => false,
    }) {
        return false;
    }

    let text_a = a
        .text
        .iter()
        .filter(|i| text_reach(i).into_iter().any(|r| near(moved(r))));
    let text_b = b
        .text
        .iter()
        .filter(|i| text_reach(i).into_iter().any(near));
    let texts = |ga: &[Text], ta: Transformation, gb: &[Text], tb| {
        ga.len() == gb.len()
            && ga
                .iter()
                .zip(gb)
                .all(|(x, y)| same_text_moved(x, ta, y, tb, dy, kept, scale))
    };
    eq_by(text_a, text_b, |x, y| match (x, y) {
        (Item::Live(ta), Item::Live(tb)) => same_text_moved(
            ta,
            Transformation::IDENTITY,
            tb,
            Transformation::IDENTITY,
            dy,
            kept,
            scale,
        ),
        (Item::Group(ga, _, ta), Item::Group(gb, _, tb)) => {
            shift * *ta == *tb && texts(ga, *ta, gb, *tb)
        }
        (Item::Cached(ga, _, ta), Item::Cached(gb, _, tb)) => {
            shift * *ta == *tb && texts(ga, *ta, gb, *tb)
        }
        _ => false,
    })
}

/// Whether `a`, drawn through `outer_a`, moved by `dy` is `b` drawn
/// through `outer_b`, wherever it reaches into `kept`. A paragraph or an
/// editor is the same one only when it is the same buffer: iced's own
/// equality of them looks at their sizes, not at their words.
#[allow(clippy::too_many_arguments)]
fn same_text_moved(
    a: &Text,
    outer_a: Transformation,
    b: &Text,
    outer_b: Transformation,
    dy: f32,
    kept: Rectangle,
    scale: f32,
) -> bool {
    // cosmic-text puts a glyph's row at its origin truncated toward zero
    // (`LayoutGlyph::physical`, "hinting in Y axis"): an origin that
    // crosses the window's top edge moves its glyphs a pixel less or more
    // than it moved. The rows the move keeps hold the glyphs where the
    // last frame put them; they are this frame's only if the truncated
    // origins are `dy` pixels apart too.
    let rows = |pa: crate::core::Point,
                ta: Transformation,
                pb: crate::core::Point,
                tb: Transformation| {
        let full = |t: Transformation| Transformation::scale(scale) * t;
        let (ya, yb) = ((pa * full(ta)).y, (pb * full(tb)).y);
        ya.trunc() + dy * scale == yb.trunc()
    };
    let moved = |r: Rectangle| Rectangle { y: r.y + dy, ..r };
    let clips = |ca: Rectangle, cb: Rectangle| {
        moved(ca).intersection(&kept) == cb.intersection(&kept)
    };
    let shift = Transformation::translate(0.0, dy);
    match (a, b) {
        (
            Text::Paragraph {
                paragraph: pa,
                position: xa,
                color: ka,
                clip_bounds: ca,
                transformation: ta,
            },
            Text::Paragraph {
                paragraph: pb,
                position: xb,
                color: kb,
                clip_bounds: cb,
                transformation: tb,
            },
        ) => {
            let (ta, tb) = (outer_a * *ta, outer_b * *tb);
            xa == xb
                && ka == kb
                && shift * ta == tb
                && rows(*xa, ta, *xb, tb)
                && clips(*ca * ta, *cb * tb)
                && pa == pb
                && match (pa.upgrade(), pb.upgrade()) {
                    (Some(a), Some(b)) => std::ptr::eq(a.buffer(), b.buffer()),
                    _ => false,
                }
        }
        (
            Text::Editor {
                editor: ea,
                position: xa,
                color: ka,
                clip_bounds: ca,
                transformation: ta,
            },
            Text::Editor {
                editor: eb,
                position: xb,
                color: kb,
                clip_bounds: cb,
                transformation: tb,
            },
        ) => {
            let (ta, tb) = (outer_a * *ta, outer_b * *tb);
            xa == xb
                && ka == kb
                && shift * ta == tb
                && rows(*xa, ta, *xb, tb)
                && clips(*ca * ta, *cb * tb)
                && ea == eb
                && match (ea.upgrade(), eb.upgrade()) {
                    (Some(a), Some(b)) => std::ptr::eq(a.buffer(), b.buffer()),
                    _ => false,
                }
        }
        (
            Text::Cached {
                content: na,
                bounds: ba,
                color: ka,
                size: sa,
                line_height: la,
                font: fa,
                align_x: xa,
                align_y: ya,
                shaping: ha,
                clip_bounds: ca,
            },
            Text::Cached {
                content: nb,
                bounds: bb,
                color: kb,
                size: sb,
                line_height: lb,
                font: fb,
                align_x: xb,
                align_y: yb,
                shaping: hb,
                clip_bounds: cb,
            },
        ) => {
            na == nb
                && ka == kb
                && sa == sb
                && la == lb
                && fa == fb
                && xa == xb
                && ya == yb
                && ha == hb
                && outer_a.scale_factor() == outer_b.scale_factor()
                && moved(*ba * outer_a) == *bb * outer_b
                // Its origin lies within its bounds (or a height above
                // them, aligned to the bottom); never at a negative row.
                && (*ba * outer_a).y - ba.height * outer_a.scale_factor() >= 0.0
                && (*bb * outer_b).y - bb.height * outer_b.scale_factor() >= 0.0
                && clips(*ca * outer_a, *cb * outer_b)
        }
        // Raw text: its buffer is not to be seen into.
        _ => false,
    }
}

fn eq_by<'a, T: 'a>(
    a: impl Iterator<Item = &'a T>,
    b: impl Iterator<Item = &'a T>,
    eq: impl Fn(&T, &T) -> bool,
) -> bool {
    let mut b = b;
    for x in a {
        match b.next() {
            Some(y) if eq(x, y) => {}
            _ => return false,
        }
    }
    b.next().is_none()
}
