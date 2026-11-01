//! The map's style: what a feature is ([`Class`]), the two colour tables
//! every frontend reads ([`LIGHT`] and [`DARK`], one [`Palette`] each), the
//! two device finishes ([`Style::M9`], [`Style::PC`]) and the two layer
//! profiles.
//!
//! Draw order is the class order, the order the variants are declared in:
//! earth under forest, forest under parks, parks under water, water under
//! buildings, and the lines over all areas, minor roads under secondary
//! roads under major roads. The class byte a feature is written as is a
//! separate thing ([`Class::wire`]): the format came first and keeps its
//! numbers, and the classes split out of an older one since (forest from
//! park, secondary from major roads) are the older class's byte with
//! [`crate::fixture::VARIANT`] set, which a reader that predates
//! them never looks at and draws as the older class.

use embedded_graphics::pixelcolor::{Rgb565, RgbColor};

use crate::fill::SUBPIXEL;
use crate::fixture::VARIANT;

/// What a feature is, as far as drawing it goes, in draw order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    Earth,
    Urban,
    /// Wood and forest, apart from parks since 1099.
    Forest,
    Park,
    Pedestrian,
    Sand,
    Water,
    Building,
    Waterway,
    Path,
    MinorRoad,
    /// Protomaps' `major_road` and `medium_road`, apart from the
    /// highways since 1099.
    SecondaryRoad,
    /// Protomaps' `highway`: motorways and trunk roads.
    MajorRoad,
    Rail,
}

/// Area classes, [`Class::Earth`] to [`Class::Building`].
pub const AREAS: usize = 8;
/// Line classes, [`Class::Waterway`] to [`Class::Rail`].
pub const LINES: usize = 6;

impl Class {
    /// Every class in draw order: the areas, then the lines.
    pub const ALL: [Class; AREAS + LINES] = [
        Self::Earth,
        Self::Urban,
        Self::Forest,
        Self::Park,
        Self::Pedestrian,
        Self::Sand,
        Self::Water,
        Self::Building,
        Self::Waterway,
        Self::Path,
        Self::MinorRoad,
        Self::SecondaryRoad,
        Self::MajorRoad,
        Self::Rail,
    ];

    /// The class byte and the flag bits this class is written as. The
    /// bytes rise with the draw order, so a blob sorted by class is sorted
    /// for a reader of either age.
    pub const fn wire(self) -> (u8, u8) {
        match self {
            Self::Earth => (0, 0),
            Self::Urban => (1, 0),
            Self::Forest => (2, VARIANT),
            Self::Park => (2, 0),
            Self::Pedestrian => (3, 0),
            Self::Sand => (4, 0),
            Self::Water => (5, 0),
            Self::Building => (6, 0),
            Self::Waterway => (16, 0),
            Self::Path => (17, 0),
            Self::MinorRoad => (18, 0),
            Self::SecondaryRoad => (19, VARIANT),
            Self::MajorRoad => (19, 0),
            Self::Rail => (20, 0),
        }
    }

    /// The class of a class byte and its feature's flags, `None` for an
    /// unknown byte. [`VARIANT`] on a class that has none is ignored, as
    /// every flag a reader does not know is.
    pub fn from_wire(byte: u8, flags: u8) -> Option<Self> {
        let variant = flags & VARIANT != 0;
        Some(match byte {
            0 => Self::Earth,
            1 => Self::Urban,
            2 if variant => Self::Forest,
            2 => Self::Park,
            3 => Self::Pedestrian,
            4 => Self::Sand,
            5 => Self::Water,
            6 => Self::Building,
            16 => Self::Waterway,
            17 => Self::Path,
            18 => Self::MinorRoad,
            19 if variant => Self::SecondaryRoad,
            19 => Self::MajorRoad,
            20 => Self::Rail,
            _ => return None,
        })
    }

    /// Whether features of this class are areas (filled) rather than lines.
    pub fn is_area(self) -> bool {
        self < Self::Waterway
    }

    /// This class's place among the areas, `None` for a line.
    pub fn area_index(self) -> Option<usize> {
        self.is_area().then_some(self as usize)
    }

    /// This class's place among the lines, waterway first; `None` for an
    /// area.
    pub fn line_index(self) -> Option<usize> {
        (self as usize).checked_sub(Self::Waterway as usize)
    }

    /// Whether this class is a road whose casing the PC finish draws on
    /// the ground as well as on a bridge.
    pub fn is_cased(self) -> bool {
        matches!(self, Self::SecondaryRoad | Self::MajorRoad)
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgb565 {
    Rgb565::new(r >> 3, g >> 2, b >> 3)
}

/// One colour table: every colour the map draws in, and which of the
/// sixteen ANSI colours each is drawn in on a 16-colour terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The background a frame is cleared to, the land where nothing is.
    pub background: Rgb565,
    /// Country borders over the world outline.
    pub border: Rgb565,
    /// Each area class's fill, in class order.
    pub areas: [Rgb565; AREAS],
    /// Each line class's ink, in class order.
    pub lines: [Rgb565; LINES],
    /// The casing of major and secondary roads.
    pub road_casing: Rgb565,
    /// The casing of every other line on a bridge.
    pub casing: Rgb565,
    /// The outline of a building footprint, a step darker than its fill
    /// ([`Class::Building`]'s area colour), between the fill and the
    /// ground in the dark table, the CoMaps rule (1191).
    pub building_edge: Rgb565,
    /// The ANSI indices: background, border, road casing, casing, the
    /// muted ink of a tunnel and the building outline, in this order.
    pub ansi_fixed: [u8; 6],
    /// The ANSI index of each area class.
    pub ansi_areas: [u8; AREAS],
    /// The ANSI index of each line class.
    pub ansi_lines: [u8; LINES],
}

/// The light table (1099): warmer land, forest a tone of its own apart
/// from parks, water a step greener, roads in three steps (major orange,
/// secondary yellow, minor white) and paths brown, the study's `LIGHT`
/// (`docs/drafts/map-renderer-roadmap-2026-10.md`, section 4).
///
/// The sixteen colours are chosen per class, not the nearest of the
/// sixteen, which turns parks, earth and water all into white or grey
/// (`docs/drafts/terminal-map-rendering-2026-10.md`, section 2, `-16`
/// against `-16tok`): the rule of `leviculum_ui::color`'s 16-colour tokens,
/// applied to the map's classes.
pub const LIGHT: Palette = Palette {
    background: rgb(245, 241, 233),
    border: rgb(170, 140, 175),
    areas: [
        rgb(245, 241, 233),
        rgb(234, 229, 222),
        rgb(173, 209, 158),
        rgb(205, 230, 180),
        rgb(236, 233, 240),
        rgb(242, 231, 194),
        rgb(160, 204, 230),
        rgb(221, 213, 205),
    ],
    lines: [
        rgb(120, 176, 220),
        rgb(160, 120, 90),
        rgb(255, 255, 255),
        rgb(252, 214, 132),
        rgb(246, 163, 95),
        rgb(130, 130, 136),
    ],
    road_casing: rgb(214, 160, 80),
    casing: rgb(200, 194, 186),
    building_edge: rgb(194, 184, 175),
    ansi_fixed: [7, 5, 3, 8, 8, 8],
    ansi_areas: [7, 7, 2, 10, 7, 11, 12, 8],
    ansi_lines: [4, 8, 15, 11, 3, 0],
};

/// The dark table (1099), the study's `DARK`: the same classes a step
/// apart on a near-black ground, casings in the ground's own tone. The
/// building a step lighter than the study's (1192): at its first tone it
/// was a step off the ground alone and footprints barely showed.
pub const DARK: Palette = Palette {
    background: rgb(30, 32, 36),
    border: rgb(128, 100, 136),
    areas: [
        rgb(30, 32, 36),
        rgb(40, 42, 47),
        rgb(32, 50, 36),
        rgb(36, 54, 40),
        rgb(48, 48, 54),
        rgb(58, 54, 44),
        rgb(32, 52, 74),
        rgb(62, 62, 70),
    ],
    lines: [
        rgb(52, 84, 120),
        rgb(120, 100, 80),
        rgb(86, 88, 96),
        rgb(150, 124, 72),
        rgb(176, 112, 64),
        rgb(110, 110, 118),
    ],
    road_casing: rgb(24, 26, 30),
    casing: rgb(24, 26, 30),
    building_edge: rgb(46, 46, 52),
    ansi_fixed: [0, 5, 0, 0, 8, 8],
    ansi_areas: [0, 0, 2, 2, 8, 3, 4, 8],
    ansi_lines: [4, 3, 7, 3, 11, 8],
};

/// Halfway between `a` and `b`, channel by channel.
fn mix(a: Rgb565, b: Rgb565) -> Rgb565 {
    Rgb565::new(
        (a.r() + b.r()) / 2,
        (a.g() + b.g()) / 2,
        (a.b() + b.b()) / 2,
    )
}

impl Palette {
    /// Area class `c`'s fill; the background for a line class.
    pub fn area(&self, c: Class) -> Rgb565 {
        c.area_index()
            .and_then(|i| self.areas.get(i))
            .copied()
            .unwrap_or(self.background)
    }

    /// Line class `c`'s ink; the background for an area class.
    pub fn line(&self, c: Class) -> Rgb565 {
        c.line_index()
            .and_then(|i| self.lines.get(i))
            .copied()
            .unwrap_or(self.background)
    }

    /// Line class `c`'s ink in a tunnel: halfway to the background.
    pub fn tunnel(&self, c: Class) -> Rgb565 {
        mix(self.line(c), self.background)
    }

    /// The casing under line class `c`.
    pub fn casing_of(&self, c: Class) -> Rgb565 {
        match c.is_cased() {
            true => self.road_casing,
            false => self.casing,
        }
    }

    /// The index among the sixteen ANSI colours this table's colour `c` is
    /// drawn in; `None` for a colour the table does not hold.
    pub fn ansi16(&self, c: Rgb565) -> Option<u8> {
        let [bg, border, road, casing, tunnel, edge] = self.ansi_fixed;
        let fixed = [
            (self.background, bg),
            (self.border, border),
            (self.road_casing, road),
            (self.casing, casing),
            (self.building_edge, edge),
        ];
        let areas = self.areas.iter().copied().zip(self.ansi_areas);
        let lines = self.lines.iter().copied().zip(self.ansi_lines);
        let tunnels = Class::ALL[AREAS..]
            .iter()
            .map(|&l| (self.tunnel(l), tunnel));
        fixed
            .into_iter()
            .chain(areas)
            .chain(lines)
            .chain(tunnels)
            .find(|&(colour, _)| colour == c)
            .map(|(_, i)| i)
    }

    /// Every colour a line pixel of this table can be: the inks, the
    /// tunnels' and the casings.
    pub fn inks(&self) -> [Rgb565; 2 * LINES + 2] {
        let mut out = [self.casing; 2 * LINES + 2];
        for (i, &l) in Class::ALL[AREAS..].iter().enumerate() {
            out[i] = self.line(l);
            out[LINES + i] = self.tunnel(l);
        }
        out[2 * LINES] = self.road_casing;
        out
    }
}

/// Which of the two colour tables a view is drawn in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MapTheme {
    #[default]
    Light,
    Dark,
}

impl MapTheme {
    /// The table.
    pub fn palette(self) -> &'static Palette {
        match self {
            Self::Light => &LIGHT,
            Self::Dark => &DARK,
        }
    }
}

/// How a device draws: the colour table, whether roads get casings
/// and round joins (`docs/drafts/map-renderer-roadmap-2026-10.md`,
/// section 2), and whether building footprints are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub theme: MapTheme,
    /// Casings under major and secondary roads and under every bridge,
    /// round joins and caps, and dashed paths: the PC finish. Off on the M9, whose
    /// stroke cost is the open question of the draft's section 5.
    pub cased: bool,
    /// Building footprints from a region's building sections, filled and
    /// outlined at view zoom 16 and deeper (1191, 1192): on for the phone
    /// and the window ([`Style::SCREEN`]), off on the M9 and in the
    /// terminal, whose cells would only paint city blocks grey.
    pub buildings: bool,
}

impl Style {
    /// The M9's finish in the light table: square caps, no casings.
    pub const M9: Self = Self {
        theme: MapTheme::Light,
        cased: false,
        buildings: false,
    };
    /// The PC's finish in the light table: casings and round joins, no
    /// buildings; what the terminal's sixel and kitty pictures draw.
    pub const PC: Self = Self {
        theme: MapTheme::Light,
        cased: true,
        buildings: false,
    };
    /// The finish of the phone and the window: the PC's with buildings.
    pub const SCREEN: Self = Self {
        buildings: true,
        ..Self::PC
    };

    /// The same finish in `theme`.
    pub fn in_theme(self, theme: MapTheme) -> Self {
        Self { theme, ..self }
    }
}

/// The light table's background, the colour a frame is cleared to.
pub const BACKGROUND: Rgb565 = LIGHT.background;

/// The light table's country borders: a muted mauve, apart from the
/// water and coasts it crosses and from every road and rail colour.
pub const BORDER: Rgb565 = LIGHT.border;

/// A width in sub-pixels for each line class, waterway to rail in class
/// order; 0 leaves the class out. The map core picks one per zoom band
/// (`leviculum_map::profile::line_widths`).
pub type LineWidths = [i32; LINES];

/// The deep zooms' widths and every fixture's: waterway 1.5 px, path 1,
/// minor road 2.5, secondary road 3.5, major road 4, rail 1.
pub const LINE_WIDTHS: LineWidths = [
    3 * SUBPIXEL / 2,
    SUBPIXEL,
    5 * SUBPIXEL / 2,
    7 * SUBPIXEL / 2,
    4 * SUBPIXEL,
    SUBPIXEL,
];

/// The extra width of a casing over its line, both sides together, at
/// the deep zooms: three quarters of a pixel a side.
pub const CASING: i32 = 3 * SUBPIXEL / 2;

/// Class `c`'s width in `widths`; 0 for an area class.
pub fn line_width(widths: &LineWidths, c: Class) -> i32 {
    c.line_index()
        .and_then(|i| widths.get(i))
        .copied()
        .unwrap_or(0)
}

/// Which features a view draws: the draft's two layer profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Every class in the fixture.
    Full,
    /// Without buildings, the profile the first measurement found fast
    /// enough (`docs/drafts/map-measurement-2026-10.md`, "Simplifications").
    /// The draft also strips `pois` and `physical_point`; the fixture holds
    /// no point features, so buildings are what is left to drop.
    Stripped,
}

impl Profile {
    /// Whether this profile draws `c`.
    pub fn includes(self, c: Class) -> bool {
        match self {
            Self::Full => true,
            Self::Stripped => c != Class::Building,
        }
    }

    /// The name the measurement lines print.
    pub fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Stripped => "stripped",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every class comes back from its bytes, and the class bytes never
    /// fall along the draw order: a blob sorted for this reader is sorted
    /// for one that reads the class byte alone.
    #[test]
    fn the_wire_bytes_round_trip_and_follow_the_draw_order() {
        let mut last = 0;
        for c in Class::ALL {
            let (byte, flags) = c.wire();
            assert_eq!(Class::from_wire(byte, flags), Some(c), "{c:?}");
            assert!(byte >= last, "{c:?}");
            assert_eq!(c.is_area(), byte < 16, "{c:?}");
            last = byte;
        }
        // Without the flag the older class comes back.
        assert_eq!(Class::from_wire(2, 0), Some(Class::Park));
        assert_eq!(Class::from_wire(19, 0), Some(Class::MajorRoad));
        assert_eq!(Class::from_wire(7, 0), None);
    }

    /// Each table names an ANSI index for every colour it draws, and no
    /// two of a table's area or line colours or the building outline are
    /// one Rgb565 value, so a colour in a frame says which class it is.
    #[test]
    fn every_colour_has_its_sixteen_colour_index() {
        for p in [&LIGHT, &DARK] {
            for c in Class::ALL {
                let colour = if c.is_area() { p.area(c) } else { p.line(c) };
                assert!(p.ansi16(colour).is_some(), "{c:?}");
                if !c.is_area() {
                    assert!(p.ansi16(p.tunnel(c)).is_some(), "{c:?}");
                }
            }
            assert!(p.ansi16(p.building_edge).is_some());
            let mut all: [Rgb565; AREAS + LINES] = [p.background; AREAS + LINES];
            all[..AREAS - 1].copy_from_slice(&p.areas[1..]);
            all[AREAS - 1..AREAS - 1 + LINES].copy_from_slice(&p.lines);
            all[AREAS - 1 + LINES] = p.building_edge;
            for (i, a) in all.iter().enumerate() {
                assert!(!all[i + 1..].contains(a), "{a:?}");
            }
        }
    }
}
