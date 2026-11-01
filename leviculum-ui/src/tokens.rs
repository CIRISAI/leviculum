//! The colour tokens: one table, a value for each [`Tier`] and [`Theme`]
//! (`docs/src/concepts/loquax-keys-and-layout.md`, section 5, "Colour",
//! decided 2026-10-26).
//!
//! A token is a [`Paint`]: a foreground, a background, or reverse video,
//! each optional. The pane itself is never painted, so a token that sets
//! only a foreground is drawn on the terminal's own background. The table is
//! a struct per tier and theme, so a missing token does not compile, and the
//! 256 and 16 tiers name their own indexes rather than the nearest ones of
//! the truecolor values: the nearest index does not keep contrast.
//!
//! loquax reads every token; lnomad reads the shared ones (muted, selection,
//! toast, error, the chrome bars) and keeps NomadNet's heading bands and its
//! link colour in [`Theme`]. A GUI frontend takes its palette from the same
//! table.
//!
//! The contrast every pair must hold is a test here, computed with the WCAG
//! 2.x formula on linearised sRGB; the theme's own luminance, which only
//! decides light or dark, is not reused for it.

use crate::color::{Ink, Tier};
use crate::delivery::Tone;
use crate::theme::Theme;

/// What a token sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Paint {
    /// The text colour, or the terminal's own.
    pub fg: Option<Ink>,
    /// The background, or the pane's.
    pub bg: Option<Ink>,
    /// Reverse video: the selection and the toast where there is no colour
    /// for them.
    pub reverse: bool,
}

impl Paint {
    /// Nothing: the pane's own colours.
    pub const NONE: Paint = Paint {
        fg: None,
        bg: None,
        reverse: false,
    };
    /// Reverse video.
    pub const REVERSE: Paint = Paint {
        fg: None,
        bg: None,
        reverse: true,
    };

    /// Whether the token sets nothing.
    pub fn is_none(&self) -> bool {
        *self == Paint::NONE
    }
}

/// The delivery marks' colours, one a state of the glyph table
/// ([`crate::delivery`]). `None` takes the block's text colour, which is
/// what the 16-colour tier does, since no ANSI colour holds 3:1 over both
/// own tints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tones {
    /// Grey: on its way, in the mailbox, delivered.
    pub pending: Option<Ink>,
    /// Green: read.
    pub read: Option<Ink>,
    /// Red: not sent.
    pub failed: Option<Ink>,
}

impl Tones {
    const NONE: Tones = Tones {
        pending: None,
        read: None,
        failed: None,
    };

    /// The colour of a glyph row's tone.
    pub fn of(&self, tone: Tone) -> Option<Ink> {
        match tone {
            Tone::Gray => self.pending,
            Tone::Green => self.read,
            Tone::Red => self.failed,
        }
    }

    /// Every tone, for the contrast test.
    pub fn all(&self) -> [Option<Ink>; 3] {
        [self.pending, self.read, self.failed]
    }
}

/// The table for one tier and theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tokens {
    /// Muted text on the pane: `you`, the `nothing earlier` line, a picture's
    /// status line in lnomad.
    pub muted: Paint,
    /// Our own block: its text over its tint.
    pub own: Paint,
    /// The time inside our own block.
    pub own_time: Paint,
    /// The other side's block.
    pub their: Paint,
    /// The time inside their block.
    pub their_time: Paint,
    /// Our own block, selected.
    pub own_selected: Paint,
    /// Their block, selected.
    pub their_selected: Paint,
    /// The selection in a list: reverse video in every tier.
    pub selection: Paint,
    /// The date pill under a date rule's label.
    pub date_pill: Paint,
    /// The unread rule and its count.
    pub unread: Paint,
    /// A toast's box and text.
    pub toast: Paint,
    /// Error text, and the error toast's border.
    pub error: Paint,
    /// lnomad's top bar and status bar.
    pub chrome: Paint,
    /// An unavailable control on the chrome bar.
    pub chrome_muted: Paint,
    /// The delivery marks.
    pub tones: Tones,
}

const fn hex(v: u32) -> Option<Ink> {
    Some(Ink::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

const fn idx(n: u8) -> Option<Ink> {
    Some(Ink::Indexed(n))
}

const fn fg(ink: Option<Ink>) -> Paint {
    Paint {
        fg: ink,
        bg: None,
        reverse: false,
    }
}

const fn pair(fg: Option<Ink>, bg: Option<Ink>) -> Paint {
    Paint {
        fg,
        bg,
        reverse: false,
    }
}

/// Monochrome: reverse video for the selections and the toast, nothing else.
pub const MONO: Tokens = Tokens {
    muted: Paint::NONE,
    own: Paint::NONE,
    own_time: Paint::NONE,
    their: Paint::NONE,
    their_time: Paint::NONE,
    own_selected: Paint::REVERSE,
    their_selected: Paint::REVERSE,
    selection: Paint::REVERSE,
    date_pill: Paint::NONE,
    unread: Paint::NONE,
    toast: Paint::REVERSE,
    error: Paint::NONE,
    chrome: Paint::REVERSE,
    chrome_muted: Paint::NONE,
    tones: Tones::NONE,
};

/// Truecolor on a dark terminal.
pub const TRUECOLOR_DARK: Tokens = Tokens {
    muted: fg(hex(0x9aa1ad)),
    own: pair(hex(0xf2f5fa), hex(0x1d3f6e)),
    own_time: pair(hex(0xa9c1e6), hex(0x1d3f6e)),
    their: pair(hex(0xe6e8ec), hex(0x2f333b)),
    their_time: pair(hex(0xaab0ba), hex(0x2f333b)),
    own_selected: pair(hex(0xffffff), hex(0x285a9e)),
    their_selected: pair(hex(0xffffff), hex(0x454b56)),
    selection: Paint::REVERSE,
    date_pill: pair(hex(0xd0d4dc), hex(0x3a3f4a)),
    unread: fg(hex(0xf0a030)),
    toast: pair(hex(0xe6e8ec), hex(0x3a3f4a)),
    error: fg(hex(0xff6b6b)),
    chrome: pair(hex(0xcdd2dc), hex(0x262831)),
    chrome_muted: fg(hex(0x8c96a5)),
    tones: Tones {
        pending: hex(0xb4bcc8),
        read: hex(0x6fe39a),
        failed: hex(0xff8a80),
    },
};

/// Truecolor on a light terminal.
pub const TRUECOLOR_LIGHT: Tokens = Tokens {
    muted: fg(hex(0x5f6672)),
    own: pair(hex(0x0b1a2e), hex(0xd3e3fd)),
    own_time: pair(hex(0x3b5578), hex(0xd3e3fd)),
    their: pair(hex(0x1a1c20), hex(0xebedf0)),
    their_time: pair(hex(0x565c66), hex(0xebedf0)),
    own_selected: pair(hex(0x0b1a2e), hex(0xb3cdf6)),
    their_selected: pair(hex(0x1a1c20), hex(0xd3d8e0)),
    selection: Paint::REVERSE,
    date_pill: pair(hex(0x2b2f36), hex(0xe4e7ec)),
    unread: fg(hex(0xa34f00)),
    toast: pair(hex(0xffffff), hex(0x2b2f36)),
    error: fg(hex(0xb3261e)),
    chrome: pair(hex(0x282c34), hex(0xe1e4eb)),
    chrome_muted: fg(hex(0x5f6470)),
    tones: Tones {
        pending: hex(0x4f5866),
        read: hex(0x176a35),
        failed: hex(0xa3211a),
    },
};

/// The xterm-256 palette on a dark terminal.
pub const ANSI256_DARK: Tokens = Tokens {
    muted: fg(idx(246)),
    own: pair(idx(255), idx(24)),
    own_time: pair(idx(153), idx(24)),
    their: pair(idx(255), idx(236)),
    their_time: pair(idx(248), idx(236)),
    own_selected: pair(idx(231), idx(25)),
    their_selected: pair(idx(231), idx(239)),
    selection: Paint::REVERSE,
    date_pill: pair(idx(252), idx(238)),
    unread: fg(idx(214)),
    toast: pair(idx(255), idx(238)),
    error: fg(idx(203)),
    chrome: pair(idx(188), idx(235)),
    chrome_muted: fg(idx(103)),
    tones: Tones {
        pending: idx(250),
        read: idx(121),
        failed: idx(217),
    },
};

/// The xterm-256 palette on a light terminal.
pub const ANSI256_LIGHT: Tokens = Tokens {
    muted: fg(idx(242)),
    own: pair(idx(16), idx(189)),
    own_time: pair(idx(238), idx(189)),
    their: pair(idx(235), idx(254)),
    their_time: pair(idx(241), idx(254)),
    own_selected: pair(idx(16), idx(153)),
    their_selected: pair(idx(235), idx(252)),
    selection: Paint::REVERSE,
    date_pill: pair(idx(237), idx(253)),
    unread: fg(idx(94)),
    toast: pair(idx(231), idx(237)),
    error: fg(idx(124)),
    chrome: pair(idx(236), idx(254)),
    chrome_muted: fg(idx(241)),
    tones: Tones {
        pending: idx(240),
        read: idx(22),
        failed: idx(124),
    },
};

/// The sixteen ANSI colours on a dark terminal: their blocks and the date
/// untinted, there being no grey background among the eight.
pub const ANSI16_DARK: Tokens = Tokens {
    muted: fg(idx(7)),
    own: pair(idx(15), idx(4)),
    own_time: pair(idx(15), idx(4)),
    their: Paint::NONE,
    their_time: fg(idx(7)),
    own_selected: Paint::REVERSE,
    their_selected: Paint::REVERSE,
    selection: Paint::REVERSE,
    date_pill: Paint::NONE,
    unread: fg(idx(11)),
    toast: Paint::REVERSE,
    error: fg(idx(9)),
    chrome: pair(idx(7), idx(0)),
    chrome_muted: fg(idx(8)),
    tones: Tones::NONE,
};

/// The sixteen ANSI colours on a light terminal.
pub const ANSI16_LIGHT: Tokens = Tokens {
    muted: Paint::NONE,
    own: pair(idx(0), idx(6)),
    own_time: pair(idx(0), idx(6)),
    their: pair(idx(0), idx(7)),
    their_time: pair(idx(0), idx(7)),
    own_selected: Paint::REVERSE,
    their_selected: Paint::REVERSE,
    selection: Paint::REVERSE,
    date_pill: pair(idx(0), idx(7)),
    unread: fg(idx(4)),
    toast: Paint::REVERSE,
    error: fg(idx(1)),
    chrome: pair(idx(0), idx(7)),
    chrome_muted: fg(idx(8)),
    tones: Tones::NONE,
};

/// The table for a tier and a theme. Monochrome has one for both themes.
pub fn tokens(tier: Tier, theme: Theme) -> &'static Tokens {
    match (tier, theme) {
        (Tier::Mono, _) => &MONO,
        (Tier::Ansi16, Theme::Dark) => &ANSI16_DARK,
        (Tier::Ansi16, Theme::Light) => &ANSI16_LIGHT,
        (Tier::Ansi256, Theme::Dark) => &ANSI256_DARK,
        (Tier::Ansi256, Theme::Light) => &ANSI256_LIGHT,
        (Tier::Truecolor, Theme::Dark) => &TRUECOLOR_DARK,
        (Tier::Truecolor, Theme::Light) => &TRUECOLOR_LIGHT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{ansi256_rgb, rgb_to_ansi16, rgb_to_ansi256, XTERM_16};

    /// The WCAG 2.x contrast ratio of two colours, `(L1 + 0.05) / (L2 +
    /// 0.05)` over the relative luminance of linearised sRGB.
    fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// WCAG 2.x relative luminance: each channel linearised from sRGB,
    /// then weighted.
    fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
        let lin = |c: u8| {
            let c = f64::from(c) / 255.0;
            match c <= 0.04045 {
                true => c / 12.92,
                false => ((c + 0.055) / 1.055).powf(2.4),
            }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }

    /// The VGA palette of the Linux console.
    const VGA_16: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xaa, 0x00, 0x00),
        (0x00, 0xaa, 0x00),
        (0xaa, 0x55, 0x00),
        (0x00, 0x00, 0xaa),
        (0xaa, 0x00, 0xaa),
        (0x00, 0xaa, 0xaa),
        (0xaa, 0xaa, 0xaa),
        (0x55, 0x55, 0x55),
        (0xff, 0x55, 0x55),
        (0x55, 0xff, 0x55),
        (0xff, 0xff, 0x55),
        (0x55, 0x55, 0xff),
        (0xff, 0x55, 0xff),
        (0x55, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];

    /// The reference panes each theme's pane foregrounds are checked on.
    fn panes(theme: Theme) -> [(u8, u8, u8); 2] {
        match theme {
            Theme::Dark => [(0x00, 0x00, 0x00), (0x28, 0x2c, 0x34)],
            Theme::Light => [(0xff, 0xff, 0xff), (0xfd, 0xf6, 0xe3)],
        }
    }

    /// An ink's value on a palette for the 16, the xterm values otherwise.
    fn value(ink: Ink, sixteen: &[(u8, u8, u8); 16]) -> (u8, u8, u8) {
        match ink {
            Ink::Rgb(r, g, b) => (r, g, b),
            Ink::Indexed(n) if n < 16 => sixteen[usize::from(n)],
            Ink::Indexed(n) => ansi256_rgb(n),
        }
    }

    fn fails_of(tier: Tier, theme: Theme, sixteen: &[(u8, u8, u8); 16]) -> Vec<String> {
        let t = tokens(tier, theme);
        let v = |ink: Ink| value(ink, sixteen);
        let mut fails = Vec::new();
        let mut check = |what: &str, a: (u8, u8, u8), b: (u8, u8, u8), bar: f64| {
            let ratio = contrast(a, b);
            if ratio < bar {
                fails.push(format!("{tier:?} {theme:?} {what}: {ratio:.2} < {bar}"));
            }
        };
        // The text pairs: every token that sets both sides.
        for (what, paint) in [
            ("own", t.own),
            ("own time", t.own_time),
            ("their", t.their),
            ("their time", t.their_time),
            ("own selected", t.own_selected),
            ("their selected", t.their_selected),
            ("date pill", t.date_pill),
            ("toast", t.toast),
        ] {
            if let (Some(f), Some(b)) = (paint.fg, paint.bg) {
                check(what, v(f), v(b), 4.5);
            }
        }
        if tier == Tier::Ansi16 {
            // Only the text pairs: the marks there are text.
            return fails;
        }
        // The pane foregrounds against the two reference panes.
        for (what, paint) in [("muted", t.muted), ("unread", t.unread), ("error", t.error)] {
            match paint.fg {
                Some(f) => {
                    for pane in panes(theme) {
                        check(&format!("{what} on {pane:?}"), v(f), pane, 4.5);
                    }
                }
                None => check(&format!("{what} has no colour"), (0, 0, 0), (0, 0, 0), 4.5),
            }
        }
        // The marks, a graphic: 3:1 over the own tint and the selected one.
        for tone in t.tones.all() {
            for (what, under) in [("own", t.own.bg), ("own selected", t.own_selected.bg)] {
                match (tone, under) {
                    (Some(tone), Some(under)) => check(
                        &format!("tone {tone:?} over {what}"),
                        v(tone),
                        v(under),
                        3.0,
                    ),
                    _ => check(
                        "a tone or a tint without a colour",
                        (0, 0, 0),
                        (0, 0, 0),
                        3.0,
                    ),
                }
            }
        }
        fails
    }

    #[test]
    fn every_pair_of_every_tier_and_theme_holds_its_bar() {
        let mut fails = Vec::new();
        for theme in [Theme::Dark, Theme::Light] {
            for tier in [Tier::Truecolor, Tier::Ansi256] {
                fails.extend(fails_of(tier, theme, &XTERM_16));
            }
            fails.extend(fails_of(Tier::Ansi16, theme, &XTERM_16));
            fails.extend(fails_of(Tier::Ansi16, theme, &VGA_16));
        }
        assert!(fails.is_empty(), "{fails:#?}");
    }

    #[test]
    fn the_formula_is_the_wcag_one() {
        assert!((contrast((0, 0, 0), (255, 255, 255)) - 21.0).abs() < 1e-9);
        assert!((contrast((255, 255, 255), (255, 255, 255)) - 1.0).abs() < 1e-9);
        // The section's own numbers.
        let r = contrast((0xf2, 0xf5, 0xfa), (0x1d, 0x3f, 0x6e));
        assert!((r - 9.66).abs() < 0.01, "{r}");
        let r = contrast((0xa9, 0xc1, 0xe6), (0x1d, 0x3f, 0x6e));
        assert!((r - 5.76).abs() < 0.01, "{r}");
        // And a mid grey, where the linearisation matters: 18.4 % luminance.
        let l = relative_luminance((0x77, 0x77, 0x77));
        assert!((l - 0.1845).abs() < 0.001, "{l}");
    }

    #[test]
    fn the_block_tokens_of_each_table_are_the_section_s() {
        assert_eq!(TRUECOLOR_DARK.own.bg, hex(0x1d3f6e));
        assert_eq!(TRUECOLOR_LIGHT.own.bg, hex(0xd3e3fd));
        assert_eq!(ANSI256_DARK.own, pair(idx(255), idx(24)));
        assert_eq!(ANSI256_LIGHT.own, pair(idx(16), idx(189)));
        assert_eq!(ANSI16_DARK.own, pair(idx(15), idx(4)));
        assert_eq!(ANSI16_LIGHT.own, pair(idx(0), idx(6)));
        // Their blocks untinted on dark in 16 only, and the selection is
        // reverse video there and in monochrome.
        assert!(ANSI16_DARK.their.is_none());
        assert!(!ANSI16_LIGHT.their.is_none());
        for t in [&MONO, &ANSI16_DARK, &ANSI16_LIGHT] {
            assert_eq!(t.own_selected, Paint::REVERSE);
            assert_eq!(t.toast, Paint::REVERSE);
            assert_eq!(t.tones, Tones::NONE);
        }
        assert_eq!(tokens(Tier::Mono, Theme::Light), &MONO);
    }

    /// The chrome bars keep lnomad's colours: the truecolor values are the
    /// ones it drew before the table, and the lower tiers are where its
    /// depth post-pass lands them, so the bar does not move with the table.
    #[test]
    fn the_chrome_tokens_are_lnomad_s_bars_at_every_depth() {
        for (theme, tc, n256, n16) in [
            (Theme::Dark, &TRUECOLOR_DARK, &ANSI256_DARK, &ANSI16_DARK),
            (
                Theme::Light,
                &TRUECOLOR_LIGHT,
                &ANSI256_LIGHT,
                &ANSI16_LIGHT,
            ),
        ] {
            for (paint, p256, p16) in [
                (tc.chrome, n256.chrome, n16.chrome),
                (tc.chrome_muted, n256.chrome_muted, n16.chrome_muted),
            ] {
                for (ink, i256, i16) in [(paint.fg, p256.fg, p16.fg), (paint.bg, p256.bg, p16.bg)] {
                    let Some(Ink::Rgb(r, g, b)) = ink else {
                        assert_eq!((ink, i256, i16), (None, None, None), "{theme:?}");
                        continue;
                    };
                    assert_eq!(i256, idx(rgb_to_ansi256(r, g, b)), "{theme:?} 256");
                    assert_eq!(i16, idx(rgb_to_ansi16(r, g, b)), "{theme:?} 16");
                }
            }
        }
    }

    #[test]
    fn a_tone_reads_its_glyph_row() {
        use crate::delivery::{glyph, DeliveryState};
        let t = &TRUECOLOR_DARK.tones;
        let tone = |state| t.of(glyph(state).tone);
        assert_eq!(tone(DeliveryState::Queued), t.pending);
        assert_eq!(tone(DeliveryState::Delivered), t.pending);
        assert_eq!(tone(DeliveryState::Read { at: 0 }), t.read);
        assert_eq!(tone(DeliveryState::Failed), t.failed);
        assert_ne!(t.read, t.pending);
    }
}
