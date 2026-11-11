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

use leviculum_map::draw::Ink;
use leviculum_map::labels::Rect as Box2;
use leviculum_map::pins::{self, Mark, Metrics, Pin};
use leviculum_map::viewport::{ExactView, Viewport};
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

/// [`setup`] with the radar's zooms (1386) when there are `pins` to draw
/// it about: deeper than the outline reaches where no region is. Without
/// a pin there is no radar, and the zooms stay the regions' and the
/// outline's.
pub fn setup_for(painter: &Painter, pins: &[MapPin]) -> MapSetup {
    let mut setup = setup(painter);
    if !pins.is_empty() {
        setup.zooms = crate::ui::radar::zooms(setup.zooms);
    }
    setup
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
/// RGBA, the edges smoothed; with `names_after` (the pins' boxes, [`taken`]) the map's names
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
    // Straight into RGBA, the edges smoothed as a tile at rest has them
    // (1422).
    let drawn = painter.draw_rgba(view, Ink::Colour, style, percent, true);
    let mut rgba = drawn.rgba;
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
    /// [`MapPin::low_battery`] (1386): a battery beside the marker.
    pub low_battery: bool,
    /// [`MapPin::own`]: the reader's own position, drawn as the blue dot
    /// ([`OWN_DOT`]) without a label (1449 item 2, walk 1 finding 8).
    pub own: bool,
}

/// The own position's blue, as the maps people know draw it, on every
/// target's map (1449 item 2): the window's canvas, its picture, the
/// terminal's picture; the phone's `R.color.own_dot` and the terminal's
/// text map say the same.
pub const OWN_DOT: [u8; 3] = [0x1a, 0x73, 0xe8];

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
            // Our own position is the dot alone (1449 item 2).
            let words = match pin.own {
                true => String::new(),
                false => pin.words(),
            };
            let width = match pin.own {
                true => 0,
                false => measure(&words, LABEL * scale),
            };
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
            label: label
                .filter(|_| !pin.own)
                .map(|l| (l.x as f32, l.y as f32, words)),
            width: width as f32,
            stale: pin.stale,
            nearby: pin.nearby,
            selected: Some(pin.peer) == selected,
            low_battery: pin.low_battery(),
            own: pin.own,
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

/// The hue of `peer`'s track in `tracks` (1385, rule 6), for its pin's
/// marker and name; `None` for our own, grey track, and for a peer without
/// one: its pin keeps the pins' blue.
pub fn pin_hue(
    tracks: &[crate::ui::tracks::Track],
    peer: &[u8; 16],
    dark: bool,
) -> Option<[u8; 3]> {
    let hue = tracks.iter().find(|t| t.peer == *peer)?.hue?;
    Some(crate::ui::tracks::colour(Some(hue), dark))
}

/// The window's pin colours (`gui::Map::draw`), the label's ground and
/// text turned for the dark table; a peer with a track in its hue, the
/// name too.
fn pin_ink(pin: &Placed, theme: MapTheme, hue: Option<[u8; 3]>) -> PinInk {
    let ground = match (pin.nearby, hue) {
        (true, _) => [0x80, 0x80, 0x80],
        (false, Some(hue)) => hue,
        (false, None) => [0x20, 0x50, 0xc0],
    };
    let marker = match (pin.selected, pin.stale) {
        (true, _) => ([0xd0, 0x30, 0x30], 1.0),
        (false, stale) if pin.own => (OWN_DOT, if stale { 0.45 } else { 1.0 }),
        (false, false) => (ground, 1.0),
        (false, true) => (ground, 0.45),
    };
    let (ground, text) = match theme {
        MapTheme::Dark => ([0x18, 0x18, 0x18], [0xe6, 0xe6, 0xe6]),
        _ => ([0xff, 0xff, 0xff], [0x10, 0x10, 0x10]),
    };
    let text = match (pin.nearby, hue) {
        (false, Some(hue)) => hue,
        _ => text,
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
/// the label on a light ground, dimmed when the fix is stale; a peer with
/// a track in `tracks` in its hue ([`pin_hue`]). In order, so the selected
/// pin, which [`place`] puts last, comes out on top.
#[allow(clippy::too_many_arguments)]
pub fn draw_pins(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    placed: &[Placed],
    tracks: &[crate::ui::tracks::Track],
    scale: f32,
    theme: MapTheme,
    type_: &mut Typesetter,
) {
    let radius = MARKER * scale / 2.0;
    let line = LABEL * LINE * scale;
    for pin in placed {
        let hue = pin_hue(tracks, &pin.peer, theme == MapTheme::Dark);
        let ink = pin_ink(pin, theme, hue);
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
        if pin.low_battery {
            draw_battery(rgba, w, h, pin.x, pin.y, scale, theme);
        }
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

/// The low battery's size in logical pixels, its body without the nub
/// (1386).
pub const BATTERY: (f32, f32) = (11.0, 6.0);

/// Where the low battery sits from a marker's centre, its body's top left
/// corner in screen pixels: centred above the marker, clear of the label
/// beside it.
pub fn battery_at(x: f32, y: f32, scale: f32) -> (f32, f32) {
    (
        x - BATTERY.0 / 2.0 * scale,
        y - (MARKER / 2.0 + BATTERY.1 + 3.0) * scale,
    )
}

/// The low battery beside a marker at `(x, y)` (1386): a body with a nub
/// at its right, outlined in the label's ink on its ground, a fifth of it
/// filled red.
pub fn draw_battery(rgba: &mut [u8], w: u32, h: u32, x: f32, y: f32, scale: f32, theme: MapTheme) {
    let (ground, ink) = match theme {
        MapTheme::Dark => ([0x18, 0x18, 0x18], [0xe6, 0xe6, 0xe6]),
        _ => ([0xff, 0xff, 0xff], [0x10, 0x10, 0x10]),
    };
    let (x0, y0) = battery_at(x, y, scale);
    let (bw, bh) = (BATTERY.0 * scale, BATTERY.1 * scale);
    let nub = (1.5 * scale).max(1.0);
    let t = scale.max(1.0).round();
    let fill = |rgba: &mut [u8], x0: f32, y0: f32, x1: f32, y1: f32, c: [u8; 3], a: f64| {
        for py in y0.round() as i64..y1.round() as i64 {
            for px in x0.round() as i64..x1.round() as i64 {
                blend(rgba, w, h, px, py, c, a);
            }
        }
    };
    fill(
        rgba,
        x0 - t,
        y0 - t,
        x0 + bw + nub + t,
        y0 + bh + t,
        ground,
        0.85,
    );
    fill(rgba, x0, y0, x0 + bw, y0 + bh, ink, 1.0);
    fill(rgba, x0 + t, y0 + t, x0 + bw - t, y0 + bh - t, ground, 1.0);
    let inner = bw - 2.0 * t;
    fill(
        rgba,
        x0 + t,
        y0 + t,
        x0 + t + (inner / 5.0).max(1.0),
        y0 + bh - t,
        [0xd0, 0x30, 0x30],
        1.0,
    );
    fill(
        rgba,
        x0 + bw,
        y0 + bh / 3.0,
        x0 + bw + nub,
        y0 + bh * 2.0 / 3.0,
        ink,
        1.0,
    );
}

/// A track's line width in logical pixels (1385).
pub const TRACK_WIDTH: f32 = 3.0;
/// An outlier's dot's radius in logical pixels.
pub const DOT_RADIUS: f32 = 2.0;
/// A place's opacity: faint, under everything else.
pub const PLACE_ALPHA: f32 = 0.22;
/// A late segment's dash and gap in logical pixels (1386).
pub const DASH: (f32, f32) = (6.0, 4.0);
/// The wedge's length and half width in logical pixels, beyond the
/// marker's ring.
pub const WEDGE: (f32, f32) = (8.0, 5.0);

/// One track's coverage, the strongest a pixel gets, so two segments
/// meeting at a fix do not darken it: the track is blended once.
struct Cover {
    w: u32,
    h: u32,
    alpha: Vec<u8>,
    /// The pixels touched, to blend and clear.
    reach: (i64, i64, i64, i64),
}

impl Cover {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            alpha: vec![0; w as usize * h as usize],
            reach: (i64::MAX, i64::MAX, i64::MIN, i64::MIN),
        }
    }

    fn put(&mut self, x: i64, y: i64, a: f32) {
        if x < 0 || y < 0 || x >= i64::from(self.w) || y >= i64::from(self.h) || a <= 0.0 {
            return;
        }
        let i = y as usize * self.w as usize + x as usize;
        let v = (a.min(1.0) * 255.0).round() as u8;
        if v > self.alpha[i] {
            self.alpha[i] = v;
        }
        let r = &mut self.reach;
        *r = (r.0.min(x), r.1.min(y), r.2.max(x), r.3.max(y));
    }

    /// The segment from `a` to `b`, `width` wide, at `alpha`, its edges
    /// smoothed over a pixel.
    fn line(&mut self, a: (f64, f64), b: (f64, f64), width: f32, alpha: f32) {
        let half = f64::from(width) / 2.0;
        let (w, h) = (f64::from(self.w), f64::from(self.h));
        let pad = half + 1.0;
        let (x0, x1) = (a.0.min(b.0) - pad, a.0.max(b.0) + pad);
        let (y0, y1) = (a.1.min(b.1) - pad, a.1.max(b.1) + pad);
        if x1 < 0.0 || y1 < 0.0 || x0 > w || y0 > h {
            return;
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        for y in y0.max(0.0).floor() as i64..=y1.min(h).ceil() as i64 {
            for x in x0.max(0.0).floor() as i64..=x1.min(w).ceil() as i64 {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let t = match len2 > 0.0 {
                    true => (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0),
                    false => 0.0,
                };
                let d = (px - (a.0 + t * dx)).hypot(py - (a.1 + t * dy));
                let c = (half + 0.5 - d).clamp(0.0, 1.0) as f32;
                self.put(x, y, c * alpha);
            }
        }
    }

    /// The segment from `a` to `b` in dashes of `dash` and gaps of `gap`
    /// pixels (1386), `phase` pixels into the pattern at `a`; the phase at
    /// `b`, so a run of short segments is one dashed line.
    fn dashed(
        &mut self,
        (a, b): ((f64, f64), (f64, f64)),
        width: f32,
        alpha: f32,
        (dash, gap): (f64, f64),
        phase: f64,
    ) -> f64 {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = dx.hypot(dy);
        let period = dash + gap;
        if period <= 0.0 || len <= 0.0 {
            return phase;
        }
        let at = |d: f64| (a.0 + dx * d / len, a.1 + dy * d / len);
        // From the start of the pattern's period the phase is in.
        let mut d = -phase.rem_euclid(period);
        while d < len {
            let (from, to) = (d.max(0.0), (d + dash).min(len));
            if to > from {
                self.line(at(from), at(to), width, alpha);
            }
            d += period;
        }
        (phase + len).rem_euclid(period)
    }

    /// A disc of `radius` about `c`, at `alpha`.
    fn disc(&mut self, c: (f64, f64), radius: f32, alpha: f32) {
        self.line(c, c, radius * 2.0, alpha);
    }

    /// Blend `colour` through the coverage into `rgba` and clear it.
    fn blend(&mut self, rgba: &mut [u8], colour: [u8; 3]) {
        let (x0, y0, x1, y1) = self.reach;
        if x0 > x1 {
            return;
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                let i = y as usize * self.w as usize + x as usize;
                let a = self.alpha[i];
                if a > 0 {
                    blend(rgba, self.w, self.h, x, y, colour, f64::from(a) / 255.0);
                    self.alpha[i] = 0;
                }
            }
        }
        self.reach = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    }
}

/// The time zone view's borders (1684) over a frame already composed for
/// `view`, `w` by `h`: every border line of
/// [`leviculum_map::timezones::BORDERS`] whose box reaches the frame, in
/// [`crate::zoneclock::line_rgb`], `scale` pixels a logical one. Points
/// closer than a pixel and a half to the last one drawn are passed over, so
/// the world view strokes about what the screen can show and not the
/// table's 300,000 points; a step across the antimeridian is not drawn as
/// a line across the frame. The tile cache never holds them: the frame is
/// composed first and the borders go over it, like the tracks.
pub fn draw_time_zones(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    view: &ExactView,
    scale: f32,
    theme: MapTheme,
) {
    use leviculum_map::mercator::World;
    use leviculum_map::{outline, timezones};
    let Some(bits) = outline::bits(timezones::BORDERS) else {
        return;
    };
    if w == 0 || h == 0 || rgba.len() < w as usize * h as usize * 4 {
        return;
    }
    let shift = 32u32.saturating_sub(bits);
    let world = |(x, y): (u32, u32)| World {
        x: x.checked_shl(shift).unwrap_or(u32::MAX),
        y: y.checked_shl(shift).unwrap_or(u32::MAX),
    };
    let world_px = 4_294_967_296.0 / view.unit();
    let (fw, fh) = (f64::from(w), f64::from(h));
    let mut cover = Cover::new(w, h);
    let width = 1.5 * scale;
    let step = 1.5 * f64::from(scale);
    for line in outline::lines(timezones::BORDERS) {
        let [west, north, east, south] = line.bbox;
        let (x0, y0) = view.to_screen(world((west, north)));
        let (x1, y1) = view.to_screen(world((east, south)));
        // A box wrapped round the antimeridian is never passed over.
        if x0 <= x1 && (x1 < 0.0 || x0 > fw || y1 < 0.0 || y0 > fh) {
            continue;
        }
        let mut last: Option<(f64, f64)> = None;
        for p in line.points() {
            let at = view.to_screen(world(p));
            let Some(from) = last else {
                last = Some(at);
                continue;
            };
            let (dx, dy) = (at.0 - from.0, at.1 - from.1);
            if dx.abs() > world_px / 2.0 {
                last = Some(at);
                continue;
            }
            if dx * dx + dy * dy < step * step {
                continue;
            }
            cover.line(from, at, width, 0.85);
            last = Some(at);
        }
    }
    cover.blend(rgba, crate::zoneclock::line_rgb(theme));
}

/// Draw `tracks` into `rgba`, `w` by `h` pixels, under the pins (1385,
/// `loquax-tracks.md`): the places faint, labelled where they are among
/// the largest; the lines at their age's opacity; the dwells as blobs;
/// the outliers as dots; the wedge beside the newest fix's marker. The
/// marker's own label carries its age, one per position (1447). `at(world)` is where a point lands on the frame.
#[allow(clippy::too_many_arguments)]
pub fn draw_tracks(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    tracks: &[crate::ui::tracks::Track],
    at: impl Fn(leviculum_map::mercator::World) -> (f64, f64),
    scale: f32,
    theme: MapTheme,
    type_: &mut Typesetter,
) {
    use crate::ui::tracks::{colour, Age};
    if tracks.is_empty() || w == 0 || h == 0 {
        return;
    }
    let dark = theme == MapTheme::Dark;
    let mut cover = Cover::new(w, h);
    // The places of every track first, under every line.
    for t in tracks {
        for p in &t.places {
            cover.disc(at(p.at), f32::from(p.radius) * scale, PLACE_ALPHA);
        }
        cover.blend(rgba, colour(t.hue, dark));
    }
    let (ground, text) = match dark {
        true => ([0x18, 0x18, 0x18], [0xe6, 0xe6, 0xe6]),
        false => ([0xff, 0xff, 0xff], [0x10, 0x10, 0x10]),
    };
    let size = LABEL * 0.85 * scale;
    for t in tracks {
        for p in &t.places {
            let Some(words) = &p.label else {
                continue;
            };
            let (x, y) = at(p.at);
            let mask = type_.mask(words, size, 0, false);
            let centre = (x, y + f64::from(f32::from(p.radius) * scale + size * 0.7));
            mapnames::paint_faded(rgba, w, h, &mask, centre, 0.0, ground, 0.6);
            mapnames::paint_faded(rgba, w, h, &mask, centre, 0.0, text, 0.7);
        }
    }
    let width = TRACK_WIDTH * scale;
    for t in tracks {
        let hue = colour(t.hue, dark);
        // The oldest first, so a newer line is over an older one.
        for age in [Age::Old, Age::Faint, Age::Half, Age::Strong, Age::Fresh] {
            // A late run's dashes go on across its segments (1386).
            let dash = (f64::from(DASH.0 * scale), f64::from(DASH.1 * scale));
            let mut phase = 0.0;
            let mut last = None;
            for s in t.segments.iter().filter(|s| s.age == age) {
                if !s.late {
                    cover.line(at(s.from), at(s.to), width, age.alpha());
                    last = None;
                    continue;
                }
                if last != Some(s.from) {
                    phase = 0.0;
                }
                phase = cover.dashed((at(s.from), at(s.to)), width, age.alpha(), dash, phase);
                last = Some(s.to);
            }
        }
        for d in &t.dwells {
            let r = f32::from(d.radius) * scale;
            cover.disc(at(d.at), r, d.age.alpha() * 0.45);
            // Its rim at the full step, so a blob reads as a stay.
            let c = at(d.at);
            let steps = 48;
            for k in 0..steps {
                let a0 = k as f64 / steps as f64 * core::f64::consts::TAU;
                let a1 = (k + 1) as f64 / steps as f64 * core::f64::consts::TAU;
                let p = |a: f64| (c.0 + f64::from(r) * a.cos(), c.1 + f64::from(r) * a.sin());
                cover.line(p(a0), p(a1), 1.5 * scale, d.age.alpha());
            }
        }
        for d in &t.dots {
            cover.disc(at(d.at), DOT_RADIUS * scale, 0.45);
        }
        if let Some(wedge) = &t.wedge {
            let c = at(wedge.at);
            let b = f64::from(wedge.bearing).to_radians();
            let (ux, uy) = (b.sin(), -b.cos());
            let base = f64::from(MARKER * scale / 2.0 + 2.5 * scale);
            let (len, half) = (f64::from(WEDGE.0 * scale), f64::from(WEDGE.1 * scale));
            let root = (c.0 + ux * base, c.1 + uy * base);
            let tip = (root.0 + ux * len, root.1 + uy * len);
            // A filled triangle: lines from the tip to every point of the
            // base, wide enough to leave no gap.
            let n = (half * 2.0).ceil().max(2.0) as i32;
            for k in 0..=n {
                let s = -half + 2.0 * half * f64::from(k) / f64::from(n);
                let p = (root.0 - uy * s, root.1 + ux * s);
                cover.line(p, tip, 1.5 * scale, 1.0);
            }
        }
        cover.blend(rgba, hue);
    }
}

/// The radar's ground, light and dark (1386): the map's paper, blank.
pub fn radar_ground(theme: MapTheme) -> [u8; 3] {
    match theme {
        MapTheme::Dark => [0x16, 0x16, 0x16],
        _ => [0xf2, 0xef, 0xe9],
    }
}

/// The radar's rings' ink, light and dark.
fn radar_ink(theme: MapTheme) -> [u8; 3] {
    match theme {
        MapTheme::Dark => [0x9a, 0x9a, 0x9a],
        _ => [0x6a, 0x6a, 0x6a],
    }
}

/// Draw `radar` into `rgba`, `w` by `h` pixels, as the ground where no
/// region is (1386, `loquax-tracks.md`, "Radar without a map"): the frame
/// blank, the rings about the centre faint with their distance on their
/// north crossing, a cross at the centre, "N" with an arrow at the top
/// right, and the radar's note at the top left. `at(world)` is where a
/// point lands on the frame, `per_px` the metres a screen pixel spans.
#[allow(clippy::too_many_arguments)]
pub fn draw_radar(
    rgba: &mut [u8],
    w: u32,
    h: u32,
    radar: &crate::ui::radar::Radar,
    at: impl Fn(leviculum_map::mercator::World) -> (f64, f64),
    per_px: f64,
    scale: f32,
    theme: MapTheme,
    type_: &mut Typesetter,
) {
    if w == 0 || h == 0 || per_px <= 0.0 {
        return;
    }
    let ground = radar_ground(theme);
    for px in rgba.chunks_exact_mut(4) {
        px[..3].copy_from_slice(&ground);
        px[3] = 0xff;
    }
    let ink = radar_ink(theme);
    let c = at(radar.centre);
    let mut cover = Cover::new(w, h);
    for &r in &radar.rings {
        let px = f64::from(r) / per_px;
        let steps = ((px * core::f64::consts::TAU / 4.0).ceil() as usize).clamp(32, 4096);
        for k in 0..steps {
            let a0 = k as f64 / steps as f64 * core::f64::consts::TAU;
            let a1 = (k + 1) as f64 / steps as f64 * core::f64::consts::TAU;
            let p = |a: f64| (c.0 + px * a.sin(), c.1 - px * a.cos());
            cover.line(p(a0), p(a1), scale, 0.55);
        }
    }
    let arm = f64::from(6.0 * scale);
    cover.line((c.0 - arm, c.1), (c.0 + arm, c.1), 1.5 * scale, 1.0);
    cover.line((c.0, c.1 - arm), (c.0, c.1 + arm), 1.5 * scale, 1.0);
    // North: an arrow up at the top right, "N" under it.
    let size = LABEL * 0.85 * scale;
    let (nx, top) = (
        f64::from(w) - f64::from(18.0 * scale),
        f64::from(8.0 * scale),
    );
    let long = f64::from(14.0 * scale);
    cover.line((nx, top), (nx, top + long), 1.5 * scale, 1.0);
    let barb = f64::from(4.0 * scale);
    cover.line((nx, top), (nx - barb, top + barb), 1.5 * scale, 1.0);
    cover.line((nx, top), (nx + barb, top + barb), 1.5 * scale, 1.0);
    cover.blend(rgba, ink);
    let north = type_.mask("N", size, 0, false);
    let below = top + long + f64::from(size * LINE) / 2.0;
    mapnames::paint_faded(rgba, w, h, &north, (nx, below), 0.0, ink, 1.0);
    for &r in &radar.rings {
        let px = f64::from(r) / per_px;
        let words = crate::ui::radar::distance(f64::from(r));
        let mask = type_.mask(&words, size, 0, false);
        let centre = (c.0, c.1 - px);
        let half_w = mask.w as f64 / 2.0 + f64::from(2.0 * scale);
        let half_h = f64::from(size * LINE) / 2.0;
        for py in (centre.1 - half_h).floor() as i64..(centre.1 + half_h).ceil() as i64 {
            for qx in (centre.0 - half_w).floor() as i64..(centre.0 + half_w).ceil() as i64 {
                blend(rgba, w, h, qx, py, ground, 1.0);
            }
        }
        mapnames::paint_faded(rgba, w, h, &mask, centre, 0.0, ink, 1.0);
    }
    let note = radar.note();
    let mask = type_.mask(&note, size, 0, false);
    let pad = f64::from(6.0 * scale);
    let centre = (
        pad + mask.w as f64 / 2.0,
        pad + f64::from(size * LINE) / 2.0,
    );
    mapnames::paint_faded(rgba, w, h, &mask, centre, 0.0, ink, 1.0);
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
/// tracks over the names ([`draw_tracks`]), the pins drawn in last. The
/// window does the same in two layers, the picture as an image and the
/// pins on its canvas.
#[allow(clippy::too_many_arguments)]
pub fn render(
    painter: &mut Painter,
    type_: &mut Typesetter,
    view: &Viewport,
    style: Style,
    scale: f32,
    pins: &[MapPin],
    tracks: &[crate::ui::tracks::Track],
    selected: Option<[u8; 16]>,
    names: bool,
) -> Rendered {
    let placed = place(pins, view, selected, scale, |text, size| {
        type_.width(text, size)
    });
    let after = names.then(|| taken(&placed, scale));
    let mut picture = paint(painter, view, style, scale, after, type_);
    let (w, h) = view.size();
    let sub = leviculum_mapfill::SUBPIXEL as f64;
    let at = |p| {
        let (x, y) = view.to_screen_sub(p);
        (x as f64 / sub, y as f64 / sub)
    };
    draw_tracks(
        &mut picture.rgba,
        w,
        h,
        tracks,
        at,
        scale,
        style.theme,
        type_,
    );
    draw_pins(
        &mut picture.rgba,
        w,
        h,
        &placed,
        tracks,
        scale,
        style.theme,
        type_,
    );
    Rendered { picture, placed }
}
