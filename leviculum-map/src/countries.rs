//! Countries for travel: which country a point lies in, and the facts a
//! traveller there wants first, the emergency numbers, the side of the road
//! and the currency (order 1684, `docs/src/concepts/loquax-expedition-map.md`).
//!
//! Two tables are part of the binary, provenance in `fixtures/README.md`:
//!
//! - [`COUNTRIES_50M`], Natural Earth's 1:50m countries, version 5.1.2,
//!   public domain, baked by `scripts/bake-country-outline.py`: 239
//!   countries, 1,631 rings and 97,910 points in 200,354 bytes. `LCO1`, a
//!   country count, then per country its ISO 3166-1 alpha-2 code (two
//!   bytes) and its ring count (a LEB128 varint), then the rings as one
//!   ring table of [`crate::outline`] (`LWO2`) at 16 bits, a step 611 m at
//!   the equator. A country's rings follow each other in the order of the
//!   codes.
//! - [`FACTS`], Wikidata's emergency numbers (P2852), driving side (P1622)
//!   and currencies (P38, ISO 4217 by P498), CC0 1.0, baked by
//!   `scripts/bake-country-facts.py`: UTF-8 text, the line `LCF1 <rows>`,
//!   then per country sorted by code `code TAB name TAB numbers TAB side
//!   TAB currencies`. Numbers are space separated, a service number with a
//!   colon and its services' letters (`110:p`, p police, a ambulance, f
//!   fire), the general ones first; the side is `L`, `R` or empty; the
//!   currencies are space separated, the main one first. A field Wikidata
//!   has no value for is empty.
//!
//! The lookup is even-odd over the rings of one country, so a hole (Lesotho
//! in South Africa) is outside it, with each ring's box as a quick reject;
//! it allocates nothing and reads the table in place. A table cut short or
//! malformed gives `None`, never a panic.

use crate::mercator::{project, LatLon};
use crate::outline::{self, varint};

/// Natural Earth 1:50m countries, `LCO1`.
pub const COUNTRIES_50M: &[u8] = include_bytes!("../data/ne_50m_admin_0_countries.lco");

/// Wikidata's travel facts per country, `LCF1`.
pub const FACTS: &str = include_str!("../data/country-facts.lcf");

const MAGIC_COUNTRIES: &[u8; 4] = b"LCO1";
const MAGIC_FACTS: &str = "LCF1 ";

/// The country `(lat_deg, lon_deg)` lies in, as its ISO 3166-1 alpha-2
/// code, from [`COUNTRIES_50M`]; `None` at sea and where no country is.
pub fn country_at(lat_deg: f64, lon_deg: f64) -> Option<[u8; 2]> {
    country_in(COUNTRIES_50M, lat_deg, lon_deg)
}

/// [`country_at`] over another country table.
pub fn country_in(table: &[u8], lat_deg: f64, lon_deg: f64) -> Option<[u8; 2]> {
    if !lat_deg.is_finite() || !lon_deg.is_finite() {
        return None;
    }
    let (heads, rings) = split(table)?;
    let bits = outline::bits(rings)?;
    let shift = 32 - bits;
    let p = project(LatLon::from_degrees(lat_deg, lon_deg));
    let (px, py) = (i64::from(p.x), i64::from(p.y));
    let mut all = outline::rings(rings);
    for (code, count) in heads {
        let mut inside = false;
        for _ in 0..count {
            let ring = all.next()?;
            let [x0, y0, x1, y1] = ring.bbox.map(|v| i64::from(v) << shift);
            if px < x0 || px > x1 || py < y0 || py > y1 {
                continue;
            }
            inside ^= contains(&ring, shift, px, py);
        }
        if inside {
            return Some(code);
        }
    }
    None
}

/// Whether the ring holds the point, both in world units: a ray to the
/// east crosses its edges an odd number of times.
fn contains(ring: &outline::Ring<'_>, shift: u32, px: i64, py: i64) -> bool {
    let mut pts = ring
        .points()
        .map(|(x, y)| (i64::from(x) << shift, i64::from(y) << shift));
    let Some(first) = pts.next() else {
        return false;
    };
    let mut inside = false;
    let mut a = first;
    for b in pts.chain(core::iter::once(first)) {
        if (a.1 > py) != (b.1 > py) {
            // The edge's x at py lies east of px: (px - ax) / (bx - ax) <
            // (py - ay) / (by - ay), cross-multiplied with the sign of
            // by - ay; i128 since each product needs 65 bits.
            let lhs = i128::from(px - a.0) * i128::from(b.1 - a.1);
            let rhs = i128::from(b.0 - a.0) * i128::from(py - a.1);
            if (b.1 > a.1 && lhs < rhs) || (b.1 < a.1 && lhs > rhs) {
                inside = !inside;
            }
        }
        a = b;
    }
    inside
}

/// The countries of a country table, each code with its ring count, and
/// the ring table after them; `None` for no country table.
fn split(table: &[u8]) -> Option<(Heads<'_>, &[u8])> {
    if table.get(..4)? != MAGIC_COUNTRIES {
        return None;
    }
    let mut at = 4;
    let count = varint(table, &mut at)?;
    let start = at;
    for _ in 0..count {
        table.get(at..at + 2)?;
        at += 2;
        varint(table, &mut at)?;
    }
    let heads = Heads {
        table: &table[..at],
        at: start,
    };
    Some((heads, &table[at..]))
}

struct Heads<'a> {
    table: &'a [u8],
    at: usize,
}

impl Iterator for Heads<'_> {
    type Item = ([u8; 2], u32);

    fn next(&mut self) -> Option<Self::Item> {
        let code = self.table.get(self.at..self.at + 2)?;
        let code = [code[0], code[1]];
        self.at += 2;
        let count = varint(self.table, &mut self.at)?;
        Some((code, count))
    }
}

/// The countries of a country table, each code with its ring count, in the
/// table's order; none for a table that is not one.
pub fn countries(table: &[u8]) -> impl Iterator<Item = ([u8; 2], u32)> + '_ {
    split(table).map(|(heads, _)| heads).into_iter().flatten()
}

/// The ring table inside a country table, readable by
/// [`outline::rings`]; empty for a table that is not one.
pub fn ring_table(table: &[u8]) -> &[u8] {
    split(table).map(|(_, rings)| rings).unwrap_or(&[])
}

/// Which side of the road traffic keeps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// One emergency number. A number for no particular service is general.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Number<'a> {
    pub digits: &'a str,
    pub police: bool,
    pub ambulance: bool,
    pub fire: bool,
}

impl Number<'_> {
    /// A number for every emergency, not for one service.
    pub fn is_general(&self) -> bool {
        !(self.police || self.ambulance || self.fire)
    }
}

/// What a traveller in a country wants to know first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts<'a> {
    /// ISO 3166-1 alpha-2.
    pub code: [u8; 2],
    /// The English name, Wikidata's label.
    pub name: &'a str,
    /// The emergency numbers as the table holds them, `"112 110:p"`; see
    /// [`Facts::numbers`].
    pub emergency: &'a str,
    pub driving: Option<Side>,
    /// The main currency, ISO 4217.
    pub currency: Option<&'a str>,
    currencies: &'a str,
}

impl<'a> Facts<'a> {
    /// The emergency numbers, the general ones first, then police,
    /// ambulance, fire.
    pub fn numbers(&self) -> impl Iterator<Item = Number<'a>> {
        self.emergency.split_ascii_whitespace().map(|token| {
            let (digits, services) = token.split_once(':').unwrap_or((token, ""));
            Number {
                digits,
                police: services.contains('p'),
                ambulance: services.contains('a'),
                fire: services.contains('f'),
            }
        })
    }

    /// The first general number, else the first number of any service.
    pub fn main_number(&self) -> Option<&'a str> {
        self.numbers().next().map(|n| n.digits)
    }

    /// Every current currency, ISO 4217, the main one first.
    pub fn currencies(&self) -> impl Iterator<Item = &'a str> {
        self.currencies.split_ascii_whitespace()
    }
}

/// The facts of the country `code` in [`FACTS`].
pub fn facts(code: [u8; 2]) -> Option<Facts<'static>> {
    facts_in(FACTS, code)
}

/// [`facts`] over another facts table.
pub fn facts_in(table: &str, code: [u8; 2]) -> Option<Facts<'_>> {
    all_facts(table).find(|f| f.code == code)
}

/// Every row of a facts table, in its order; a row that does not parse is
/// skipped, a table that is not one has none.
pub fn all_facts(table: &str) -> impl Iterator<Item = Facts<'_>> {
    let mut lines = table.lines();
    let ok = lines.next().is_some_and(|l| l.starts_with(MAGIC_FACTS));
    lines.filter(move |_| ok).filter_map(row)
}

fn row(line: &str) -> Option<Facts<'_>> {
    let mut fields = line.split('\t');
    let mut next = || fields.next();
    let (code, name, emergency, side, currencies) = (next()?, next()?, next()?, next()?, next()?);
    if next().is_some() {
        return None;
    }
    let code = match code.as_bytes() {
        &[a, b] if a.is_ascii_uppercase() && b.is_ascii_uppercase() => [a, b],
        _ => return None,
    };
    let driving = match side {
        "L" => Some(Side::Left),
        "R" => Some(Side::Right),
        _ => None,
    };
    Some(Facts {
        code,
        name,
        emergency,
        driving,
        currency: currencies.split_ascii_whitespace().next(),
        currencies,
    })
}
