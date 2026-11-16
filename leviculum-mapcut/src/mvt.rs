//! Mapbox Vector Tiles to classed geometry: the one MVT decoder of the
//! tree. The cut uses it, and so does the measurement harness
//! (`leviculum-mapfill/measure`, `extract`), which is where it came from.
//!
//! Layer and kind names are the Protomaps Basemap's, both schemas the tree
//! meets: 3.x (the Florence sample, kind under `pmap:kind`, waterways in
//! `physical_line`, `natural`) and 4.x (the daily builds, kind under
//! `kind`, waterways as lines of `water`, `landcover`).

use leviculum_map::labels::LabelKind;
use leviculum_mapfill::fixture::{Kind, BRIDGE, TUNNEL};
use leviculum_mapfill::style::Class;
use prost::Message;
use tinymvt::geometry::GeometryDecoder;
use tinymvt::vector_tile::tile::{Feature, Layer, Value};
use tinymvt::vector_tile::Tile;

use crate::Res;

/// The only extent the decoder takes, Protomaps' and the fixture's.
pub const EXTENT: u32 = 4096;

const POLYGON: i32 = 3;
const LINESTRING: i32 = 2;
const POINT: i32 = 1;

/// A name as the cut keeps it: the local one, and the one shown in the
/// locale the region is cut for where that differs: `name:<locale>`, else
/// `name:en` where the locale cannot read `name` ([`readable`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub local: String,
    pub alternate: Option<String>,
}

/// Attribute keys the cut keeps on a point of interest when the source
/// carries them (1685, `loquax-expedition-map.md` section 4: supply with
/// its opening hours). The daily build today carries `kind_detail` and
/// `elevation`; the rest are kept the day a source ships them, with no
/// code change.
pub const POI_ATTRS: &[&str] = &["kind_detail", "elevation", "opening_hours", "access", "fee"];

/// Attribute keys the cut keeps on a way: the state of a way (surface,
/// track grade, hiking difficulty, trail visibility, fords, access) and
/// a marked route's marks (`ref`, `network`, `route`). The daily build
/// today carries `kind_detail`, `ref`, `network`, `route`, `service` and
/// `oneway`; the way-state keys are kept the day a source ships them.
pub const WAY_ATTRS: &[&str] = &[
    "kind_detail",
    "surface",
    "tracktype",
    "sac_scale",
    "trail_visibility",
    "ford",
    "access",
    "ref",
    "network",
    "route",
    "service",
    "oneway",
];

/// Area kinds whose identity the drawn class collapses and the cut keeps
/// as a `kind` attribute: protected and military areas (1685).
pub const PROTECTED_KINDS: &[&str] = &[
    "protected_area",
    "nature_reserve",
    "national_park",
    "military",
    "naval_base",
    "danger_area",
];

/// A point of interest of the `pois` layer, every kind kept verbatim
/// (1685: all points of interest go into the data).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePoi {
    /// The source's kind: `drinking_water`, `fuel`, `shelter`, `bench`.
    pub kind: String,
    /// The source's `min_zoom`, rounded down; 0 where it has none.
    pub min_zoom: u8,
    /// The name, where it has one.
    pub name: Option<Name>,
    /// In tile units, possibly in the tile's buffer.
    pub at: [i32; 2],
    /// The [`POI_ATTRS`] the feature carries, in that order.
    pub attrs: Vec<(&'static str, String)>,
}

/// A named point the map labels: a place, a water, a park.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePoint {
    pub kind: LabelKind,
    /// `population_rank`, 0 where the source has none.
    pub rank: u8,
    /// The source's `min_zoom`, rounded down; 0 where it has none.
    pub min_zoom: u8,
    pub name: Name,
    /// In tile units, possibly in the tile's buffer.
    pub at: [i32; 2],
}

/// One feature: its kind, class and parts. An area's part is a polygon,
/// outer ring first, then its holes; a line's part is one line string.
pub struct SourceFeature {
    pub kind: Kind,
    pub class: Class,
    /// [`BRIDGE`] or [`TUNNEL`] for a line that runs on one or in one.
    pub flags: u8,
    pub parts: Vec<Vec<Vec<[i32; 2]>>>,
    /// A named road's or waterway's kind, the source's `min_zoom` (0 where
    /// it has none) and name, which the cut labels.
    pub label: Option<(LabelKind, u8, Name)>,
    /// The travel attributes the cut keeps (1685): a line's [`WAY_ATTRS`],
    /// and for an area of a [`PROTECTED_KINDS`] kind its `kind` and
    /// access. Empty unless the decode was asked for pois.
    pub attrs: Vec<(&'static str, String)>,
}

/// A decoded tile: features in tile order (layer by layer, feature by
/// feature), the named points the map labels, and how many features had no
/// class or the wrong geometry.
pub struct Decoded {
    pub features: Vec<SourceFeature>,
    pub points: Vec<SourcePoint>,
    /// The points of interest, when the decode was asked for them.
    pub pois: Vec<SourcePoi>,
    pub skipped: usize,
}

/// A feature's properties as text or number, looked up by key.
struct Props<'a> {
    layer: &'a Layer,
    feature: &'a Feature,
}

impl<'a> Props<'a> {
    fn value(&self, key: &str) -> Option<&'a Value> {
        self.feature.tags.chunks_exact(2).find_map(|c| {
            (self.layer.keys.get(c[0] as usize)? == key)
                .then(|| self.layer.values.get(c[1] as usize))
                .flatten()
        })
    }

    /// The first of `keys` the feature carries as text.
    fn text(&self, keys: &[&str]) -> Option<&'a str> {
        keys.iter()
            .find_map(|k| self.value(k)?.string_value.as_deref())
    }

    /// The first of `keys` the feature carries as a number.
    fn number(&self, keys: &[&str]) -> Option<f64> {
        keys.iter().find_map(|k| {
            let v = self.value(k)?;
            v.double_value
                .or(v.float_value.map(f64::from))
                .or(v.int_value.map(|i| i as f64))
                .or(v.sint_value.map(|i| i as f64))
                .or(v.uint_value.map(|i| i as f64))
                .or(v.string_value.as_deref().and_then(|s| s.parse().ok()))
        })
    }

    /// `key` (or schema 3.x's `pmap:<key>`) as text: a string as it is,
    /// a number formatted, a boolean as `yes`/`no`.
    fn any_text(&self, key: &str) -> Option<String> {
        let v = self
            .value(key)
            .or_else(|| self.value(&format!("pmap:{key}")))?;
        if let Some(s) = v.string_value.as_deref() {
            return (!s.trim().is_empty()).then(|| s.to_owned());
        }
        if let Some(i) = v
            .int_value
            .or(v.sint_value)
            .or_else(|| v.uint_value.and_then(|u| i64::try_from(u).ok()))
        {
            return Some(i.to_string());
        }
        if let Some(f) = v.double_value.or(v.float_value.map(f64::from)) {
            return Some(f.to_string());
        }
        v.bool_value.map(|b| match b {
            true => "yes".to_owned(),
            false => "no".to_owned(),
        })
    }

    /// The keys of `list` the feature carries, in `list`'s order.
    fn attrs(&self, list: &[&'static str]) -> Vec<(&'static str, String)> {
        list.iter()
            .filter_map(|k| Some((*k, self.any_text(k)?)))
            .collect()
    }

    /// The name, and the one `locale` shows where it differs; `None`
    /// without a name or with an empty one. The chain: `name:<locale>`;
    /// else `name:en` where `name` is in a script the locale does not read
    /// and `name:en` is in one it does ([`readable`]: a German map of
    /// Athens shows "Athens", not "Αθήνα"); else `name` alone.
    fn name(&self, locale: Option<&str>) -> Option<Name> {
        let local = self.text(&["name"]).filter(|n| !n.trim().is_empty())?;
        let given = |key: &str| self.text(&[key]).filter(|a| !a.trim().is_empty());
        let alternate = locale
            .and_then(|l| {
                given(&format!("name:{l}")).or_else(|| {
                    let script = locale_script(l);
                    given("name:en").filter(|en| !readable(local, script) && readable(en, script))
                })
            })
            .filter(|a| *a != local)
            .map(str::to_owned);
        Some(Name {
            local: local.to_owned(),
            alternate,
        })
    }
}

/// The scripts the label language tells apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    Latin,
    Greek,
    Cyrillic,
    /// Han, kana and Hangul.
    Cjk,
    /// Every other script: Arabic, Hebrew, the Indic ones, Thai, ...
    Other,
}

/// The script of a letter; `None` for what is no letter (digits, spaces,
/// punctuation, combining marks), which every reader reads.
pub fn script_of(c: char) -> Option<Script> {
    if !c.is_alphabetic() {
        return None;
    }
    let u = u32::from(c);
    Some(match u {
        0x41..=0x5a | 0x61..=0x7a | 0xaa | 0xba | 0xc0..=0x24f | 0x250..=0x2af => Script::Latin,
        0x1e00..=0x1eff | 0x2c60..=0x2c7f | 0xa720..=0xa7ff | 0xff21..=0xff5a => Script::Latin,
        0x370..=0x3ff | 0x1f00..=0x1fff => Script::Greek,
        0x400..=0x52f | 0x1c80..=0x1c8f | 0x2de0..=0x2dff | 0xa640..=0xa69f => Script::Cyrillic,
        0x1100..=0x11ff | 0x3040..=0x30ff | 0x3130..=0x318f | 0x31f0..=0x31ff => Script::Cjk,
        0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff => Script::Cjk,
        0x20000..=0x3ffff => Script::Cjk,
        _ => Script::Other,
    })
}

/// The script a locale's language (`de`, `ru`) is written in: Latin for
/// every language not named here.
pub fn locale_script(locale: &str) -> Script {
    let lang = locale.split(['_', '-', '.']).next().unwrap_or("");
    match lang {
        "el" => Script::Greek,
        "ru" | "uk" | "be" | "bg" | "mk" | "sr" | "kk" | "ky" | "mn" | "tg" | "tt" | "ba"
        | "cv" => Script::Cyrillic,
        "zh" | "ja" | "ko" => Script::Cjk,
        "ar" | "fa" | "ur" | "he" | "yi" | "hi" | "mr" | "ne" | "bn" | "th" | "ka" | "hy"
        | "am" | "ta" | "te" | "kn" | "ml" | "gu" | "pa" | "si" | "km" | "lo" | "my" => {
            Script::Other
        }
        _ => Script::Latin,
    }
}

/// Whether a reader of `script` reads `text`: every letter in Latin, which
/// every map reader is taken to read, or in `script`.
pub fn readable(text: &str, script: Script) -> bool {
    text.chars()
        .filter_map(script_of)
        .all(|s| s == Script::Latin || s == script)
}

/// The label kind of a place of Protomaps `kind` and `kind_detail`: the
/// classes the map names (`docs/drafts/map-renderer-roadmap-2026-10.md`,
/// section 1, "Placement"). Localities, hamlets, isolated dwellings and
/// farms are left out: in Frankfurt they are 1,774 of the 1,933 places of
/// a z13 tile, field and area names that bury the towns.
pub fn place_kind(kind: &str, detail: &str) -> Option<LabelKind> {
    if kind == "macrohood" {
        return Some(LabelKind::District);
    }
    Some(match detail {
        "city" => LabelKind::City,
        "town" => LabelKind::Town,
        "quarter" => LabelKind::District,
        "village" => LabelKind::Village,
        "suburb" => LabelKind::Suburb,
        "neighbourhood" => LabelKind::Neighbourhood,
        _ => return None,
    })
}

/// The label kind of a named waterway line of Protomaps `kind`: rivers,
/// canals and streams; drains and ditches are not named.
pub fn waterway_kind(kind: &str) -> Option<LabelKind> {
    match kind {
        "river" | "canal" | "stream" => Some(LabelKind::River),
        _ => None,
    }
}

/// The label kind of a named road of Protomaps `kind`; paths, railways and
/// the rest are not named.
pub fn road_kind(kind: &str) -> Option<LabelKind> {
    Some(match kind {
        "highway" => LabelKind::MajorRoad,
        "major_road" | "medium_road" => LabelKind::SecondaryRoad,
        "minor_road" => LabelKind::MinorRoad,
        _ => return None,
    })
}

/// The class of a landuse, natural or landcover area of `kind`, `other`
/// for a kind none of the classes names: the study's mapping
/// (`docs/drafts/map-renderer-roadmap-2026-10.md`, section 4), wood and
/// forest apart from the parks, meadows and fields with them.
fn cover(kind: &str, other: Class) -> Class {
    use Class::*;
    match kind {
        "wood" | "forest" | "scrub" => Forest,
        "garden" | "grass" | "park" | "playground" | "pitch" | "greenfield" | "meadow"
        | "cemetery" | "golf_course" | "allotments" | "farmland" | "grassland" => Park,
        "pedestrian" => Pedestrian,
        "sand" | "beach" | "barren" => Sand,
        "urban_area" => Urban,
        _ => other,
    }
}

/// The class of a feature of `layer` with `kind`, drawn as an area for a
/// polygon and as a line for a line string.
pub fn class_of(layer: &str, kind: &str, geometry: i32) -> Option<(Kind, Class)> {
    use Class::*;
    Some(match layer {
        "earth" => (Kind::Area, Earth),
        "landuse" => (Kind::Area, cover(kind, Urban)),
        "natural" | "landcover" => (Kind::Area, cover(kind, Park)),
        "water" if geometry == LINESTRING => (Kind::Line, Waterway),
        "water" => (Kind::Area, Water),
        "buildings" => (Kind::Area, Building),
        "roads" => (
            Kind::Line,
            match kind {
                "highway" => MajorRoad,
                "major_road" | "medium_road" => SecondaryRoad,
                "path" => Path,
                // 4.x carries the railways here, not in `transit`.
                "rail" => Rail,
                _ => MinorRoad,
            },
        ),
        "transit" => (Kind::Line, Rail),
        "physical_line" => (Kind::Line, Waterway),
        _ => return None,
    })
}

/// The flags of a line from its properties: [`BRIDGE`] and [`TUNNEL`] from
/// `is_bridge` and `is_tunnel` (schema 4.x) or from `pmap:level`, 1 and -1
/// (3.x).
fn line_flags(l: &Layer, f: &Feature) -> u8 {
    let mut flags = 0;
    for c in f.tags.chunks_exact(2) {
        let (Some(key), Some(v)) = (l.keys.get(c[0] as usize), l.values.get(c[1] as usize)) else {
            continue;
        };
        let yes = v.bool_value == Some(true);
        let level = v.sint_value.or(v.int_value).unwrap_or(0);
        flags |= match key.as_str() {
            "is_bridge" if yes => BRIDGE,
            "is_tunnel" if yes => TUNNEL,
            "pmap:level" if level > 0 => BRIDGE,
            "pmap:level" if level < 0 => TUNNEL,
            _ => 0,
        };
    }
    flags
}

/// Decode one uncompressed MVT tile, names and pois left out.
pub fn decode(raw: &[u8]) -> Res<Decoded> {
    decode_named(raw, None, false, false)
}

/// Decode one uncompressed MVT tile, and when `names` is set the names the
/// map labels as well: the local name, with `locale`'s (`de`, `en`) where
/// it differs. When `pois` is set, every point of the `pois` layer and
/// the features' travel attributes too (1685).
pub fn decode_named(raw: &[u8], locale: Option<&str>, names: bool, pois: bool) -> Res<Decoded> {
    let tile = Tile::decode(raw)?;
    let mut out = Decoded {
        features: Vec::new(),
        points: Vec::new(),
        pois: Vec::new(),
        skipped: 0,
    };
    for l in &tile.layers {
        if l.extent.unwrap_or(EXTENT) != EXTENT {
            return Err(format!("layer {} extent {:?}", l.name, l.extent).into());
        }
        let kidx = l
            .keys
            .iter()
            .position(|k| k == "pmap:kind")
            .or_else(|| l.keys.iter().position(|k| k == "kind"));
        for f in &l.features {
            let mut kind = "";
            if let Some(ki) = kidx {
                for c in f.tags.chunks(2) {
                    if c[0] as usize == ki {
                        kind = c
                            .get(1)
                            .and_then(|v| l.values.get(*v as usize))
                            .and_then(|v| v.string_value.as_deref())
                            .unwrap_or("");
                    }
                }
            }
            let geometry = f.r#type.unwrap_or(0);
            let props = Props {
                layer: l,
                feature: f,
            };
            if geometry == POINT {
                let mut used = false;
                // A park or water poi is a label point AND a poi: the
                // label names the map, the poi record is the data.
                if pois && l.name == "pois" {
                    if let Some(p) = poi(kind, &props, f, locale)? {
                        out.pois.push(p);
                        used = true;
                    }
                }
                if names {
                    if let Some(p) = point(l.name.as_str(), kind, &props, f, locale)? {
                        out.points.push(p);
                        used = true;
                    }
                }
                if !used {
                    out.skipped += 1;
                }
                continue;
            }
            let Some((k, class)) = class_of(&l.name, kind, geometry) else {
                out.skipped += 1;
                continue;
            };
            let mut d = GeometryDecoder::new(&f.geometry);
            let parts: Vec<Vec<Vec<[i32; 2]>>> = match (geometry, k) {
                (POLYGON, Kind::Area) => d.decode_polygons()?,
                (LINESTRING, Kind::Line) => d
                    .decode_linestrings()?
                    .into_iter()
                    .map(|l| vec![l])
                    .collect(),
                _ => {
                    out.skipped += 1;
                    continue;
                }
            };
            let flags = match k {
                Kind::Line => line_flags(l, f),
                Kind::Area => 0,
            };
            // The names of lines: roads, and waterways (4.x keeps them as
            // the lines of `water`, 3.x in `physical_line`); a river's name
            // is on its line alone, its area is unnamed.
            let line_kind = match (names, k, l.name.as_str()) {
                (true, Kind::Line, "roads") => road_kind(kind),
                (true, Kind::Line, "water" | "physical_line") => waterway_kind(kind),
                _ => None,
            };
            let label = line_kind.and_then(|lk| {
                let min_zoom = props.number(&["min_zoom", "pmap:min_zoom"]);
                Some((lk, small(min_zoom), props.name(locale)?))
            });
            let attrs = match (pois, k) {
                (true, Kind::Line) => props.attrs(WAY_ATTRS),
                (true, Kind::Area) if PROTECTED_KINDS.contains(&kind) => {
                    let mut a = kind_attr(kind);
                    a.extend(props.attrs(&["kind_detail", "access"]));
                    a
                }
                _ => Vec::new(),
            };
            out.features.push(SourceFeature {
                kind: k,
                class,
                flags,
                parts,
                label,
                attrs,
            });
        }
    }
    Ok(out)
}

/// A source number as a byte, rounded down; 0 where there is none.
fn small(v: Option<f64>) -> u8 {
    v.map_or(0, |v| v.floor().clamp(0.0, 255.0) as u8)
}

/// The one `kind` attribute of a protected or military area.
fn kind_attr(kind: &str) -> Vec<(&'static str, String)> {
    vec![("kind", kind.to_owned())]
}

/// The point of interest a feature of the `pois` layer is: every kind,
/// named or not, kept verbatim (1685).
fn poi(kind: &str, props: &Props<'_>, f: &Feature, locale: Option<&str>) -> Res<Option<SourcePoi>> {
    if kind.is_empty() {
        return Ok(None);
    }
    let Some(at) = GeometryDecoder::new(&f.geometry)
        .decode_points()?
        .first()
        .copied()
    else {
        return Ok(None);
    };
    Ok(Some(SourcePoi {
        kind: kind.to_owned(),
        min_zoom: small(props.number(&["min_zoom", "pmap:min_zoom"])),
        name: props.name(locale),
        at,
        attrs: props.attrs(POI_ATTRS),
    }))
}

/// The labelled point a feature of `layer` with `kind` is, if it is one: a
/// place of the named classes, a named water point, a named park.
fn point(
    layer: &str,
    kind: &str,
    props: &Props<'_>,
    f: &Feature,
    locale: Option<&str>,
) -> Res<Option<SourcePoint>> {
    let label = match layer {
        "places" => place_kind(
            kind,
            props
                .text(&["kind_detail", "pmap:kind_detail"])
                .unwrap_or(""),
        ),
        "water" => Some(LabelKind::Water),
        // 4.x names lakes, ponds and harbours as `pois` of kind `water`
        // from z12; its `water` points start at z16.
        "pois" if kind == "water" => Some(LabelKind::Water),
        "pois" if kind == "park" => Some(LabelKind::Park),
        _ => None,
    };
    let Some(label) = label else {
        return Ok(None);
    };
    let Some(name) = props.name(locale) else {
        return Ok(None);
    };
    let Some(at) = GeometryDecoder::new(&f.geometry)
        .decode_points()?
        .first()
        .copied()
    else {
        return Ok(None);
    };
    Ok(Some(SourcePoint {
        kind: label,
        rank: small(props.number(&["population_rank", "pmap:population_rank"])),
        min_zoom: small(props.number(&["min_zoom", "pmap:min_zoom"])),
        name,
        at,
    }))
}
