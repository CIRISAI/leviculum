//! Micron colour values.
//!
//! Micron encodes a colour as three characters following an `F` (foreground)
//! or `B` (background) inline command. The raw three-character form is kept
//! verbatim, plus a best-effort resolved 24-bit RGB triple. Resolution mirrors
//! NomadNet `MicronParser.py` `high_color` (lines 366-415): a leading `g`
//! selects a two-digit grayscale level, otherwise the three characters are hex
//! nibbles that are doubled to form `#rrggbb`.

/// A parsed micron colour.
///
/// `raw` is the exact three-character micron form (e.g. `"f00"`, `"g50"`).
/// `rgb` is the resolved 24-bit colour, or `None` when the raw form is not a
/// valid micron colour. `None` mirrors the reference behaviour of falling back
/// to the terminal default; resolution stays render-agnostic (no theme here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Color {
    /// The three-character micron colour form, kept verbatim.
    pub raw: String,
    /// Resolved 24-bit RGB, or `None` for the terminal default fallback.
    pub rgb: Option<(u8, u8, u8)>,
}

impl Color {
    /// Parse a three-character micron colour form into a [`Color`].
    ///
    /// Never fails: an unparseable form yields `rgb = None` (default fallback),
    /// matching the reference `high_color` `try/except` behaviour.
    pub fn parse(raw: &str) -> Color {
        Color {
            raw: raw.to_string(),
            rgb: resolve_rgb(raw),
        }
    }
}

/// Resolve a three-character micron colour to 24-bit RGB.
///
/// Mirrors `high_color` for the three-character case: grayscale `gNN` maps the
/// two decimal digits to a 0-99 level scaled onto 0-255; otherwise the three
/// hex nibbles are each doubled (`f` -> `0xff`).
fn resolve_rgb(raw: &str) -> Option<(u8, u8, u8)> {
    let cs: Vec<char> = raw.chars().collect();
    if cs.len() != 3 {
        return None;
    }

    if cs[0] == 'g' {
        // Grayscale: two decimal digits form a 0-99 level (reference builds
        // the urwid `gNN` spec from two `parseval_dec` digits).
        let d1 = cs[1].to_digit(10)?;
        let d2 = cs[2].to_digit(10)?;
        let level = d1 * 10 + d2; // 0..=99
        let v = (level * 255 / 100) as u8;
        Some((v, v, v))
    } else {
        // Three hex nibbles, each doubled into a byte.
        let r = cs[0].to_digit(16)?;
        let g = cs[1].to_digit(16)?;
        let b = cs[2].to_digit(16)?;
        Some(((r * 17) as u8, (g * 17) as u8, (b * 17) as u8))
    }
}
