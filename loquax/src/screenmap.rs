//! The map on a pixel screen (`docs/src/concepts/loquax-map.md`, slice (d),
//! and 1128): the picture of a view as RGBA, the map's names set into it,
//! and the pins over it with their labels. The window (`loquax gui`) shows
//! the picture as an iced image and draws its pins on a canvas; the Android
//! app hosts the whole of it, pins included ([`render`]), as one bitmap.
//!
//! The terminal's map counts in cells; this one counts in the screen's own
//! pixels, so the model's map functions ([`crate::ui::map`]: the view,
//! fitting, zooming about a point, dragging) run here with one pixel to
//! a "cell" ([`setup`]). The picture is [`crate::picture`]'s, the label
//! rule [`leviculum_map::pins::labels`] in pixels with the screen's font
//! measuring each label. Everything here is a pure function of its inputs,
//! so every screen and the tests draw through the same code and no test
//! needs a display.

use std::string::String;
use std::vec::Vec;

use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::pixelcolor::Rgb565;

use leviculum_map::draw::Ink;
use leviculum_map::labels::Rect as Box2;
use leviculum_map::pins::{self, Mark, Metrics, Pin};
use leviculum_map::viewport::{ExactView, Viewport};
use leviculum_mapfill::frame::rgb888;
use leviculum_mapfill::style::{MapTheme, Style};
use leviculum_ui::Rect;

use crate::mapnames::{self, Typesetter};
use crate::picture::Painter;
use crate::ui::map::{MapPin, MapSetup};

/// A marker's diameter in logical pixels; times the scale on screen.
pub const MARKER: f32 = 10.0;
/// A label's text size in logical pixels.
pub const LABEL: f32 = 14.0;
/// The space between a marker and the label beside it.
pub const GAP: f32 = 4.0;
/// A label's line, as a multiple of its text size: iced's default.
pub const LINE: f32 = 1.3;
/// How far, in logical pixels either way, the pointer goes from where the
/// left button went down before the press is a drag and no longer a click.
pub const DRAG: f32 = 4.0;

/// What the model's map functions are told: one pixel a cell, and the
/// zooms and headers of the installed regions.
pub fn setup(painter: &Painter) -> MapSetup {
    MapSetup {
        cell_px: (1, 1),
        zooms: painter.zooms(),
        regions: painter.headers(),
        ..MapSetup::default()
    }
}

/// The picture's area in screen pixels, as the model's map functions take
/// it.
pub fn area(size: (u32, u32)) -> Rect {
    let clamp = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    Rect::new(0, 0, clamp(size.0), clamp(size.1))
}

/// One view's picture: RGBA rows, and the credit it carries.
pub struct Picture {
    pub view: Viewport,
    pub rgba: Vec<u8>,
    pub attribution: String,
    /// The maps directory's epoch the regions were read at.
    pub epoch: u64,
    /// The style it was drawn in.
    pub style: Style,
    /// What its names were placed after, the pins' boxes; `None` for a
    /// picture drawn without names, while the view moves.
    pub names_after: Option<Vec<Box2>>,
    /// The names set into it.
    pub names: Vec<String>,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (w, h) = self.view.size();
        write!(
            f,
            "Picture({w}x{h} z{} {} names={})",
            self.view.zoom(),
            self.attribution,
            self.names.len()
        )
    }
}

/// `view` drawn in colour and `style` with lines as wide as `scale` asks
/// (2.0 on a screen of twice the density draws them twice as wide), as
/// RGBA; with `names_after` (the pins' boxes, [`taken`]) the map's names
/// set into it after them by `type_` ([`mapnames::set`]), streets along
/// their lines.
pub fn paint(
    painter: &mut Painter,
    view: &Viewport,
    style: Style,
    scale: f32,
    names_after: Option<Vec<Box2>>,
    type_: &mut Typesetter,
) -> Picture {
    let percent = (scale * 100.0).round().max(1.0) as u32;
    let drawn = painter.draw(view, Ink::Colour, style, percent);
    let mut rgba = Vec::with_capacity(drawn.fb.len() * 4);
    for &px in &drawn.fb {
        let [r, g, b] = rgb888(Rgb565::from(RawU16::new(u16::from_be(px))));
        rgba.extend_from_slice(&[r, g, b, 0xff]);
    }
    let mut set = Vec::new();
    if let Some(taken) = &names_after {
        let labels = painter.labels(view, true);
        let (w, h) = view.size();
        set = mapnames::set(&mut rgba, w, h, &labels, taken, scale, style.theme, type_);
    }
    Picture {
        view: *view,
        rgba,
        attribution: drawn.attribution,
        epoch: painter.epoch(),
        style,
        names_after,
        names: set,
    }
}

/// The boxes the pins of `placed` cover, their markers and their names,
/// in screen pixels: what the map's names are placed after.
pub fn taken(placed: &[Placed], scale: f32) -> Vec<Box2> {
    let r = MARKER * scale / 2.0;
    let line = LABEL * LINE * scale;
    let mut out = Vec::new();
    for p in placed {
        out.push(Box2 {
            x0: (p.x - r).floor() as i32,
            y0: (p.y - r).floor() as i32,
            x1: (p.x + r).ceil() as i32,
            y1: (p.y + r).ceil() as i32,
        });
        if let Some((x, y, _)) = &p.label {
            out.push(Box2 {
                x0: x.floor() as i32,
                y0: y.floor() as i32,
                x1: (x + p.width).ceil() as i32,
                y1: (y + line).ceil() as i32,
            });
        }
    }
    out
}

/// A pin as the window draws it, in screen pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub peer: [u8; 16],
    /// The marker's centre.
    pub x: f32,
    pub y: f32,
    /// The label's text and its top left corner, where it has room.
    pub label: Option<(f32, f32, String)>,
    /// The label's width, for a click on it.
    pub width: f32,
    pub stale: bool,
    /// [`MapPin::nearby`]: drawn in a neutral grey.
    pub nearby: bool,
    pub selected: bool,
}

/// The sizes the label rule counts with at `scale`.
pub fn metrics(scale: f32) -> Metrics {
    let px = |v: f32| (v * scale).round().max(1.0) as u32;
    Metrics {
        marker: px(MARKER),
        gap: px(GAP),
        line: px(LABEL * LINE),
    }
}

/// The pins of `pins` that land in `view`, the label of each set by the
/// core's rule where it has room and `selected`'s always; `measure(text,
/// size)` is the width of a label in pixels at that text size. The
/// selected pin comes last, so it is drawn on top.
pub fn place(
    pins: &[MapPin],
    view: &Viewport,
    selected: Option<[u8; 16]>,
    scale: f32,
    measure: impl FnMut(&str, f32) -> u32,
) -> Vec<Placed> {
    let as_pins: Vec<Pin<'_>> = pins
        .iter()
        .map(|p| Pin {
            at: p.at,
            label: &p.label,
            stale: p.stale,
        })
        .collect();
    let landed = pins::place(view, &as_pins)
        .into_iter()
        .map(|p| (p.index, p.x, p.y))
        .collect();
    labelled(pins, landed, view.size(), selected, scale, measure)
}

/// [`place`] in a view between two zoom steps (1134): each pin where the
/// exact scale puts it, the pixel its point lands in.
pub fn place_exact(
    pins: &[MapPin],
    view: &ExactView,
    selected: Option<[u8; 16]>,
    scale: f32,
    measure: impl FnMut(&str, f32) -> u32,
) -> Vec<Placed> {
    let (w, h) = view.size();
    let landed = pins
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let (x, y) = view.to_screen(p.at);
            let (x, y) = (x.floor(), y.floor());
            let inside = (0.0..f64::from(w)).contains(&x) && (0.0..f64::from(h)).contains(&y);
            inside.then_some((i, x as u32, y as u32))
        })
        .collect();
    labelled(pins, landed, (w, h), selected, scale, measure)
}

/// The pins `landed` at their pixels, `(index, x, y)`, labelled by the
/// core's rule in a view of `size`, the selected one last.
fn labelled(
    pins: &[MapPin],
    landed: Vec<(usize, u32, u32)>,
    size: (u32, u32),
    selected: Option<[u8; 16]>,
    scale: f32,
    mut measure: impl FnMut(&str, f32) -> u32,
) -> Vec<Placed> {
    let m = metrics(scale);
    let landed: Vec<(&MapPin, u32, u32, String, u32)> = landed
        .into_iter()
        .filter_map(|(index, x, y)| {
            let pin = pins.get(index)?;
            let words = pin.words();
            let width = measure(&words, LABEL * scale);
            Some((pin, x, y, words, width))
        })
        .collect();
    // The rule takes a marker's top left corner; the core places its centre.
    let marks: Vec<Mark> = landed
        .iter()
        .map(|(_, x, y, _, _)| Mark {
            x: x.saturating_sub(m.marker / 2),
            y: y.saturating_sub(m.line / 2),
        })
        .collect();
    let chosen = selected.and_then(|peer| landed.iter().position(|(p, ..)| p.peer == peer));
    let labels = pins::labels(
        &marks,
        |i| landed.get(i).map_or(0, |(.., width)| *width),
        chosen,
        size,
        m,
    );
    let mut out: Vec<Placed> = landed
        .into_iter()
        .zip(labels)
        .map(|((pin, x, y, words, width), label)| Placed {
            peer: pin.peer,
            x: x as f32,
            y: y as f32,
            label: label.map(|l| (l.x as f32, l.y as f32, words)),
            width: width as f32,
            stale: pin.stale,
            nearby: pin.nearby,
            selected: Some(pin.peer) == selected,
        })
        .collect();
    out.sort_by_key(|p| p.selected);
    out
}

/// The pin under the screen pixel `(x, y)`, its marker or its label, the
/// topmost where they overlap.
pub fn hit(placed: &[Placed], x: f32, y: f32, scale: f32) -> Option<[u8; 16]> {
    let radius = MARKER * scale / 2.0 + 2.0 * scale;
    let line = LABEL * LINE * scale;
    placed
        .iter()
        .rev()
        .find(|p| {
            let on_marker = (p.x - x).abs() <= radius && (p.y - y).abs() <= radius;
            let on_label = p.label.as_ref().is_some_and(|(lx, ly, _)| {
                (*lx..*lx + p.width).contains(&x) && (*ly..*ly + line).contains(&y)
            });
            on_marker || on_label
        })
        .map(|p| p.peer)
}

/// The colours of a pin and its label, RGB and opacity.
struct PinInk {
    marker: ([u8; 3], f64),
    ring: [u8; 3],
    ground: ([u8; 3], f64),
    text: ([u8; 3], f64),
}

/// The window's pin colours (`gui::Map::draw`), the label's ground and
/// text turned for the dark table.
fn pin_ink(pin: &Placed, theme: MapTheme) -> PinInk {
    let ground = match pin.nearby {
        true => [0x80, 0x80, 0x80],
        false => [0x20, 0x50, 0xc0],
    };
    let marker = match (pin.selected, pin.stale) {
        (true, _) => ([0xd0, 0x30, 0x30], 1.0),
        (false, false) => (ground, 1.0),
        (false, true) => (ground, 0.45),
    };
    let (ground, text) = match theme {
        MapTheme::Dark => ([0x18, 0x18, 0x18], [0xe6, 0xe6, 0xe6]),
        _ => ([0xff, 0xff, 0xff], [0x10, 0x10, 0x10]),
    };
    PinInk {
        marker,
        ring: [0xff, 0xff, 0xff],
        ground: (ground, 0.75),
        text: (text, if pin.stale { 0.55 } else { 1.0 }),
    }
}

/// Blend `colour` at `alpha` into the RGBA pixel `(x, y)`, if it is in.
fn blend(rgba: &mut [u8], w: u32, h: u32, x: i64, y: i64, colour: [u8; 3], alpha: f64) {
    if x < 0 || y < 0 || x >= i64::from(w) || y >= i64::from(h) || alpha <= 0.0 {
        return;
    }
    let i = (y as usize * w as usize + x as usize) * 4;
    if let Some(px) = rgba.get_mut(i..i + 3) {
        for (p, &k) in px.iter_mut().zip(colour.iter()) {
            *p = (f64::from(*p) * (1.0 - alpha) + f64::from(k) * alpha).round() as u8;
        }
    }
}

/// A disc of `radius` about `(cx, cy)`, its edge smoothed over a pixel.
#[allow(clippy::too_many_arguments)]
fn disc(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    cx: f32,
    cy: f32,
    radius: f32,
    colour: [u8; 3],
    alpha: f64,
) {
    let reach = radius.ceil() as i64 + 1;
    let (x0, y0) = (cx.floor() as i64, cy.floor() as i64);
    for y in y0 - reach..=y0 + reach {
        for x in x0 - reach..=x0 + reach {
            let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
            let cover = f64::from((radius + 0.5 - d).clamp(0.0, 1.0));
            blend(rgba, w, h, x, y, colour, cover * alpha);
        }
    }
}

/// Draw the pins of `placed` into `rgba`, `w` by `h` pixels, as the window
/// draws them over its picture: a marker with a white ring, and beside it
/// the label on a light ground, dimmed when the fix is stale. In order, so
/// the selected pin, which [`place`] puts last, comes out on top.
pub fn draw_pins(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    placed: &[Placed],
    scale: f32,
    theme: MapTheme,
    type_: &mut Typesetter,
) {
    let radius = MARKER * scale / 2.0;
    let line = LABEL * LINE * scale;
    for pin in placed {
        let ink = pin_ink(pin, theme);
        disc(
            rgba,
            w,
            h,
            pin.x,
            pin.y,
            radius + 1.5 * scale,
            ink.ring,
            1.0,
        );
        disc(rgba, w, h, pin.x, pin.y, radius, ink.marker.0, ink.marker.1);
        let Some((x, y, words)) = &pin.label else {
            continue;
        };
        let (gx0, gx1) = (
            (x - 2.0 * scale).floor(),
            (x + pin.width + 2.0 * scale).ceil(),
        );
        let (gy0, gy1) = (y.floor(), (y + line).ceil());
        for py in gy0 as i64..gy1 as i64 {
            for px in gx0 as i64..gx1 as i64 {
                blend(rgba, w, h, px, py, ink.ground.0, ink.ground.1);
            }
        }
        let mask = type_.mask(words, LABEL * scale, 0, false);
        let centre = (
            f64::from(*x) + mask.w as f64 / 2.0,
            f64::from(*y) + f64::from(line) / 2.0,
        );
        mapnames::paint_faded(rgba, w, h, &mask, centre, 0.0, ink.text.0, ink.text.1);
    }
}

/// One view drawn whole for a screen that only hosts a bitmap: the
/// picture with its names, and the pins drawn in.
#[derive(Debug)]
pub struct Rendered {
    pub picture: Picture,
    /// The pins as drawn, for a tap on one.
    pub placed: Vec<Placed>,
}

/// `view` drawn whole at `scale` in `style`: the pins of `pins` placed by
/// the core's rule with `selected` on top and labelled, the map's names
/// set after them (with `names`; a view still moving goes without), the
/// pins drawn in last. The window does the same in two layers, the
/// picture as an image and the pins on its canvas.
#[allow(clippy::too_many_arguments)]
pub fn render(
    painter: &mut Painter,
    type_: &mut Typesetter,
    view: &Viewport,
    style: Style,
    scale: f32,
    pins: &[MapPin],
    selected: Option<[u8; 16]>,
    names: bool,
) -> Rendered {
    let placed = place(pins, view, selected, scale, |text, size| {
        type_.width(text, size)
    });
    let after = names.then(|| taken(&placed, scale));
    let mut picture = paint(painter, view, style, scale, after, type_);
    let (w, h) = view.size();
    draw_pins(&mut picture.rgba, w, h, &placed, scale, style.theme, type_);
    Rendered { picture, placed }
}
