//! The cut: tiles of a source in a bounding box and a zoom range, under a
//! layer profile, into a region file.
//!
//! Deterministic: the tiles are visited in index order whatever the source
//! lists, each tile's blob depends on that tile's bytes alone, the worker
//! threads only compute blobs that are then written in order, and nothing
//! of the time or the machine goes into the file. Same input, same bytes.
//!
//! The file is version 4 (1291): the header and every tile's slot first,
//! then the tiles a round at a time, coarse to fine, each round's slots
//! filled once its blobs are written and flushed, the names and buildings
//! last and the file length after them. A reader opening the file while
//! the cut still writes it draws every tile written so far
//! ([`cut_streamed`] says when a round is out).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Seek, SeekFrom, Write};
use std::ops::RangeInclusive;

use leviculum_map::buildings::{self, BUILDING_ZOOM};
use leviculum_map::labels::{encode_section, encode_strings, LabelKind, LineLabel, PointLabel};
use leviculum_map::mercator::{x_of_lon, y_of_lat};
use leviculum_map::profile::{self, ZoomLayers};
use leviculum_map::region::{
    Bounds, BuildingTables, Entry, Header, LabelTables, TileId, EMPTY_SLOT, ENTRY_LEN,
    HEADER_LEN_3, LEN_AT, TILE_VIEW,
};
use leviculum_mapfill::fixture::{write, Kind};
use leviculum_mapfill::style::Class;

use crate::mvt::{self, Name};
use crate::simplify::{area2, douglas_peucker, quantise_path, simplify_ring, Pt};
use crate::{Res, TileSource};

/// Blobs up to this size are stored once and shared by every tile that
/// has the same bytes: the uniform tiles of sea, forest and farmland.
const SHARE_MAX: usize = 4096;
/// Tiles handed to the workers per round, and written before a reader
/// sees them: a quarter of Berlin's z15 columns at 1024, so 256 lets the
/// detail fill in a sixteenth at a time (1291).
pub const ROUND: usize = 256;
/// The device's work capacity per feature (`leviculum-mapfill/m9`): a
/// feature with more vertices is skipped there as an overflow.
pub const DEVICE_POINTS: usize = 4096;
/// The device's rings per feature (`leviculum-mapfill/m9`).
pub const DEVICE_RINGS: usize = 1024;
/// The coarsest tolerance a feature over [`DEVICE_POINTS`] is simplified
/// at, in sub-pixels: four pixels.
pub const MAX_TOLERANCE: i64 = 64;
/// The tolerance building footprints are simplified at, in sub-pixels of
/// a z15 tile: half a pixel, about 1.5 m in Frankfurt, as profile 1
/// simplifies every other class at z15. A building is four to six
/// vertices, so a coarser tolerance saves little (the 1191 study: 1.89 M
/// vertices at source detail, 1.49 M at this one, over Frankfurt's box).
pub const BUILDING_TOLERANCE: i64 = 8;

/// What to cut.
pub struct CutOptions {
    pub bounds: Bounds,
    pub zooms: RangeInclusive<u8>,
    pub profile: u8,
    /// One line, shown on every map view.
    pub attribution: String,
    /// The licence and the source.
    pub notice: String,
    pub threads: usize,
    /// The language a name is kept in beside the local one where the two
    /// differ (`de`, `en`); `None` keeps the local names alone. The
    /// caller's own: loquax passes its user's.
    pub locale: Option<String>,
    /// Cut the building footprints of the z15 tiles into building
    /// sections (version 3, [`leviculum_map::buildings`]); without them,
    /// or without z15 in the zooms, the section count is 0.
    pub buildings: bool,
    /// Tiles a round at most ([`ROUND`], and a round ends with its zoom,
    /// [`rounds`]): what a reader of the file still being written sees
    /// arrive at once. The bytes do not depend on it.
    pub round: usize,
}

/// The view zoom from which a street name of `kind` shows: major and
/// secondary roads from z14, the rest from z15 (the roadmap's placement,
/// step 4). The cut keeps street names in tiles of z14 and deeper only,
/// and in the region's deepest zoom, which deeper views overzoom.
pub fn road_min_zoom(kind: LabelKind) -> u8 {
    match kind {
        LabelKind::MinorRoad => 15,
        _ => 14,
    }
}

/// The shallowest tile zoom that keeps street names, unless it is the
/// region's deepest.
pub const ROAD_LABEL_ZOOM: u8 = 14;

/// What a tile's names come from: the locale, and the tile's place in the
/// region's zooms.
#[derive(Clone, Copy, Debug)]
pub struct Names<'a> {
    pub locale: Option<&'a str>,
    pub z: u8,
    /// The tile is of the region's deepest zoom, which views past it
    /// overzoom: it keeps every name a deeper view would show.
    pub deepest: bool,
}

/// One tile's names before the region's string table numbers them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TileLabels {
    /// Kind, rank, min zoom, name, anchor at extent 4096.
    pub points: Vec<(LabelKind, u8, u8, Name, [i16; 2])>,
    /// Feature index in the tile's blob, kind, min zoom, name.
    pub lines: Vec<(u16, LabelKind, u8, Name)>,
}

impl TileLabels {
    pub fn is_empty(&self) -> bool {
        self.points.is_empty() && self.lines.is_empty()
    }
}

/// Counts for one zoom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ZoomStats {
    /// Source tiles in the box.
    pub tiles: u64,
    /// Index entries written.
    pub entries: u64,
    /// Tiles with nothing left under the profile, not written.
    pub empty: u64,
    /// Entries that reuse an earlier blob.
    pub shared: u64,
    /// Blob bytes written for this zoom.
    pub bytes: u64,
    pub features_in: u64,
    pub features_out: u64,
    pub vertices_in: u64,
    pub vertices_out: u64,
    /// Parts over [`DEVICE_POINTS`] vertices or [`DEVICE_RINGS`] rings even
    /// at [`MAX_TOLERANCE`].
    pub over_device: u64,
    /// Vertices clamped to within one extent of their tile.
    pub clamped: u64,
    /// Doublings of the tolerance for parts the device could not hold.
    pub resimplified: u64,
    /// Point labels (places, waters, parks) and line labels (streets) kept.
    pub point_labels: u64,
    pub line_labels: u64,
    /// Label section bytes written for this zoom.
    pub label_bytes: u64,
    /// Building pieces in the source tiles: a building a tile edge cuts
    /// counts once per tile.
    pub buildings_in: u64,
    /// Footprints, their vertices, and the building sections and their
    /// bytes written.
    pub buildings_out: u64,
    pub building_vertices: u64,
    pub building_sections: u64,
    pub building_bytes: u64,
    /// Footprints whose outer ring simplified to nothing: under a pixel.
    pub buildings_degenerate: u64,
    /// Footprints over the device's capacity even at [`MAX_TOLERANCE`],
    /// left out.
    pub buildings_over_device: u64,
}

impl core::ops::AddAssign for ZoomStats {
    fn add_assign(&mut self, o: Self) {
        self.tiles += o.tiles;
        self.entries += o.entries;
        self.empty += o.empty;
        self.shared += o.shared;
        self.bytes += o.bytes;
        self.features_in += o.features_in;
        self.features_out += o.features_out;
        self.vertices_in += o.vertices_in;
        self.vertices_out += o.vertices_out;
        self.over_device += o.over_device;
        self.clamped += o.clamped;
        self.resimplified += o.resimplified;
        self.point_labels += o.point_labels;
        self.line_labels += o.line_labels;
        self.label_bytes += o.label_bytes;
        self.buildings_in += o.buildings_in;
        self.buildings_out += o.buildings_out;
        self.building_vertices += o.building_vertices;
        self.building_sections += o.building_sections;
        self.building_bytes += o.building_bytes;
        self.buildings_degenerate += o.buildings_degenerate;
        self.buildings_over_device += o.buildings_over_device;
    }
}

/// What the cut wrote.
#[derive(Debug, Default)]
pub struct CutStats {
    pub zooms: BTreeMap<u8, ZoomStats>,
    /// The whole file.
    pub bytes: u64,
    /// The string table: strings and bytes.
    pub strings: u64,
    pub string_bytes: u64,
}

/// Columns (one range, or two across the antimeridian) and rows of the
/// tiles at zoom `z` that touch `b`.
pub fn tile_range(b: &Bounds, z: u8) -> (Vec<RangeInclusive<u32>>, RangeInclusive<u32>) {
    let s = 32 - u32::from(z);
    let col = |lon| (u64::from(x_of_lon(lon)) >> s) as u32;
    let row = |lat| (u64::from(y_of_lat(lat)) >> s) as u32;
    let (x0, x1) = (col(b.west), col(b.east));
    let last = ((1u64 << z) - 1) as u32;
    let xs = if b.west < b.east {
        vec![x0..=x1]
    } else {
        vec![x0..=last, 0..=x1]
    };
    (xs, row(b.north)..=row(b.south))
}

/// Whether tile `t` touches `b`.
pub fn in_bounds(t: TileId, b: &Bounds) -> bool {
    let (xs, ys) = tile_range(b, t.z);
    ys.contains(&t.y) && xs.iter().any(|r| r.contains(&t.x))
}

/// One tile's blob under `layers`, `None` when nothing is left; names
/// left out.
pub fn cut_tile(raw: &[u8], layers: ZoomLayers) -> Res<(Option<Vec<u8>>, ZoomStats)> {
    let (blob, _, st) = cut_tile_named(raw, layers, None)?;
    Ok((blob, st))
}

/// What [`cut_tile_full`] makes of one tile.
#[derive(Debug, Default)]
pub struct CutTile {
    /// The geometry blob, `None` when nothing is left under the profile.
    pub blob: Option<Vec<u8>>,
    pub labels: TileLabels,
    /// The building section, `None` when buildings were not asked for or
    /// the tile has none.
    pub buildings: Option<Vec<u8>>,
    pub stats: ZoomStats,
}

/// One tile's blob under `layers`, `None` when nothing is left, and with
/// `names` its labels: places of the named classes, waters and parks whose
/// anchor lies in the tile (a point in the buffer belongs to its
/// neighbour) and whose `min_zoom` the tile reaches, the named roads from
/// [`ROAD_LABEL_ZOOM`] and the named rivers, canals and streams from their
/// own `min_zoom`, each piece a reference to its own line feature in the
/// blob.
pub fn cut_tile_named(
    raw: &[u8],
    layers: ZoomLayers,
    names: Option<Names<'_>>,
) -> Res<(Option<Vec<u8>>, TileLabels, ZoomStats)> {
    let t = cut_tile_full(raw, layers, names, false)?;
    Ok((t.blob, t.labels, t.stats))
}

/// One z15 source tile's building pieces as footprints the building
/// section holds ([`buildings::encode_section`]): every ring simplified at
/// [`BUILDING_TOLERANCE`], a ring that collapses dropped (a hole), or its
/// footprint with it (the outer ring), every vertex clamped to within one
/// extent of the tile. A footprint over the device's capacity is
/// simplified at doubled tolerance up to [`MAX_TOLERANCE`], as an area is.
pub fn footprints(parts: &[Vec<Vec<Pt>>], st: &mut ZoomStats) -> Vec<Vec<Vec<(i16, i16)>>> {
    let range = buildings::COORDS.start..=buildings::COORDS.end - 1;
    let at = |part: &[Vec<Pt>], tol: i64| -> Vec<Vec<Pt>> {
        let mut rings = Vec::new();
        for (i, ring) in part.iter().enumerate() {
            let r = quantise_path(&simplify_ring(ring, tol), 0, true);
            let keep = r.len() >= 3 && area2(&r) != 0;
            if i == 0 && !keep {
                break;
            }
            if keep {
                rings.push(r);
            }
        }
        rings
    };
    let fits = |rings: &[Vec<Pt>]| {
        rings.iter().map(Vec::len).sum::<usize>() <= buildings::MAX_VERTICES
            && rings.len() <= buildings::MAX_RINGS
    };
    let mut out = Vec::new();
    for part in parts {
        st.buildings_in += 1;
        let mut t = BUILDING_TOLERANCE;
        let mut rings = at(part, t);
        while !fits(&rings) && t < MAX_TOLERANCE {
            t = (t * 2).min(MAX_TOLERANCE);
            rings = at(part, t);
            st.resimplified += 1;
        }
        if rings.is_empty() {
            st.buildings_degenerate += 1;
            continue;
        }
        if !fits(&rings) {
            st.buildings_over_device += 1;
            continue;
        }
        let rings: Vec<Vec<(i16, i16)>> = rings
            .into_iter()
            .map(|r| {
                r.into_iter()
                    .map(|[x, y]| {
                        let c = [
                            x.clamp(*range.start(), *range.end()),
                            y.clamp(*range.start(), *range.end()),
                        ];
                        if c != [x, y] {
                            st.clamped += 1;
                        }
                        // In range of i16: COORDS is.
                        (c[0] as i16, c[1] as i16)
                    })
                    .collect()
            })
            .collect();
        st.buildings_out += 1;
        st.building_vertices += rings.iter().map(|r| r.len() as u64).sum::<u64>();
        out.push(rings);
    }
    out
}

/// [`cut_tile_named`], and with `with_buildings` the tile's building
/// section ([`footprints`]), cut from the same decode.
pub fn cut_tile_full(
    raw: &[u8],
    layers: ZoomLayers,
    names: Option<Names<'_>>,
    with_buildings: bool,
) -> Res<CutTile> {
    let mut st = ZoomStats {
        tiles: 1,
        ..ZoomStats::default()
    };
    let decoded = mvt::decode_named(raw, names.and_then(|n| n.locale), names.is_some())?;
    let mut labels = TileLabels::default();
    if let Some(n) = names {
        for p in &decoded.points {
            let inside = (0..mvt::EXTENT as i32).contains(&p.at[0])
                && (0..mvt::EXTENT as i32).contains(&p.at[1]);
            if inside && (p.min_zoom <= n.z || n.deepest) {
                labels.points.push((
                    p.kind,
                    p.rank,
                    p.min_zoom,
                    p.name.clone(),
                    [p.at[0] as i16, p.at[1] as i16],
                ));
            }
        }
    }
    let roads_named = names.is_some_and(|n| n.z >= ROAD_LABEL_ZOOM || n.deepest);
    // The road label of each feature pushed, by its source feature.
    let mut named: Vec<Option<(LabelKind, u8, Name)>> = Vec::new();
    let k = 12 - u32::from(layers.extent_log2);
    let extent = 1i32 << layers.extent_log2;
    let tol = i64::from(layers.tolerance);
    // Square tile units per square pixel at this extent, times two for the
    // doubled shoelace area.
    let units_per_px = i64::from(extent / i32::from(TILE_VIEW)).max(1);
    let min_area2 = 2 * i64::from(layers.thin_px2) * units_per_px * units_per_px;
    let mut feats: Vec<write::Feature> = Vec::new();
    let mut building_parts: Vec<Vec<Vec<Pt>>> = Vec::new();
    for f in decoded.features {
        st.features_in += 1;
        let label = f
            .label
            .clone()
            .and_then(|(kind, source_zoom, name)| match kind.is_road() {
                true => roads_named.then(|| (kind, road_min_zoom(kind), name)),
                // A river is named in every tile its own `min_zoom` reaches,
                // and in the deepest, which deeper views overzoom.
                false => names
                    .is_some_and(|n| source_zoom <= n.z || n.deepest)
                    .then_some((kind, source_zoom, name)),
            });
        st.vertices_in += f
            .parts
            .iter()
            .flatten()
            .map(|r| r.len() as u64)
            .sum::<u64>();
        if with_buildings && f.class == Class::Building && f.kind == Kind::Area {
            building_parts.extend(f.parts.iter().cloned());
        }
        if !layers.keeps(f.class) {
            continue;
        }
        let thin = layers.thins(f.class);
        // One part simplified at `tol`: an area's outer ring and the holes
        // that survive, or the pieces of one line.
        let part_at = |part: &[Vec<Pt>], tol: i64| -> Vec<Vec<Pt>> {
            let mut rings: Vec<Vec<Pt>> = Vec::new();
            match f.kind {
                Kind::Area => {
                    for (i, ring) in part.iter().enumerate() {
                        let r = quantise_path(&simplify_ring(ring, tol), k, true);
                        let a = area2(&r).abs();
                        let keep = r.len() >= 3 && a > 0 && !(thin && a < min_area2);
                        if i == 0 && !keep {
                            // Holes without their outer ring would fill.
                            break;
                        }
                        if keep {
                            rings.push(r);
                        }
                    }
                }
                Kind::Line => {
                    for line in part {
                        let l = quantise_path(&douglas_peucker(line, tol), k, false);
                        // A line longer than the device holds is cut into
                        // pieces sharing their end points.
                        let mut at = 0;
                        while l.len() >= 2 && at + 1 < l.len() {
                            let end = (at + DEVICE_POINTS).min(l.len());
                            rings.push(l[at..end].to_vec());
                            at = end - 1;
                        }
                    }
                }
            }
            rings
        };
        let fits = |rings: &[Vec<Pt>]| {
            rings.iter().map(Vec::len).sum::<usize>() <= DEVICE_POINTS
                && rings.len() <= DEVICE_RINGS
        };
        let mut parts: Vec<Vec<Vec<Pt>>> = Vec::new();
        for part in &f.parts {
            // A polygon the device cannot hold whole is not drawn there at
            // all, so it is simplified harder, doubling the tolerance up to
            // [`MAX_TOLERANCE`], until it fits.
            let mut t = tol;
            let mut rings = part_at(part, t);
            while !fits(&rings) && f.kind == Kind::Area && t < MAX_TOLERANCE {
                t = (t * 2).clamp(1, MAX_TOLERANCE);
                rings = part_at(part, t);
                st.resimplified += 1;
            }
            match f.kind {
                // Each piece of a line is a part of its own, so the packing
                // below can put the pieces of a long line in separate
                // features.
                Kind::Line => parts.extend(rings.into_iter().map(|r| vec![r])),
                Kind::Area => {
                    if !fits(&rings) {
                        st.over_device += 1;
                    }
                    if !rings.is_empty() {
                        parts.push(rings);
                    }
                }
            }
        }
        // The parts packed into as few features as the device's capacity
        // allows; whole parts only, so no outer ring loses its holes.
        let mut chunks: Vec<Vec<Vec<Pt>>> = Vec::new();
        for rings in parts {
            let (n, r) = (rings.iter().map(Vec::len).sum::<usize>(), rings.len());
            let room = chunks.last().is_some_and(|c| {
                c.iter().map(Vec::len).sum::<usize>() + n <= DEVICE_POINTS
                    && c.len() + r <= DEVICE_RINGS
            });
            match chunks.last_mut() {
                Some(c) if room => c.extend(rings),
                _ => chunks.push(rings),
            }
        }
        let range = -extent..=2 * extent - 1;
        for rings in chunks {
            if rings.len() > usize::from(u16::MAX)
                || rings.iter().any(|r| r.len() > usize::from(u16::MAX))
            {
                continue;
            }
            let mut out = Vec::with_capacity(rings.len());
            for r in rings {
                st.vertices_out += r.len() as u64;
                out.push(
                    r.into_iter()
                        .map(|[x, y]| {
                            let c = [
                                x.clamp(*range.start(), *range.end()),
                                y.clamp(*range.start(), *range.end()),
                            ];
                            if c != [x, y] {
                                st.clamped += 1;
                            }
                            // In range of i16: the extent is at most 4096.
                            (c[0] as i16, c[1] as i16)
                        })
                        .collect(),
                );
            }
            st.features_out += 1;
            // Every piece of a named line carries its label, so a reader
            // joins the pieces back into the line its name goes along
            // (`leviculum_map::labels::join_pieces`).
            named.push(label.clone());
            feats.push(write::Feature {
                kind: f.kind,
                class: f.class,
                flags: f.flags,
                tile: 0,
                rings: out,
            });
        }
    }
    let section = match with_buildings {
        true => {
            let fp = footprints(&building_parts, &mut st);
            match fp.is_empty() {
                true => None,
                false => Some(
                    buildings::encode_section(&fp)
                        .map_err(|e| format!("building section: {e:?}"))?,
                ),
            }
        }
        false => None,
    };
    if feats.is_empty() {
        // No entry, so no label section either: a reader looks names up
        // by the tiles it draws.
        st.empty = 1;
        return Ok(CutTile {
            blob: None,
            labels: TileLabels::default(),
            buildings: section,
            stats: st,
        });
    }
    // Draw order: by class, stable, so source order holds within a class.
    let mut order: Vec<usize> = (0..feats.len()).collect();
    order.sort_by_key(|&i| feats[i].class);
    for (at, &i) in order.iter().enumerate() {
        if let (Some((kind, min_zoom, name)), Ok(at)) = (named[i].take(), u16::try_from(at)) {
            labels.lines.push((at, kind, min_zoom, name));
        }
    }
    let mut slots: Vec<Option<write::Feature>> = feats.into_iter().map(Some).collect();
    let feats: Vec<write::Feature> = order.iter().filter_map(|&i| slots[i].take()).collect();
    st.point_labels = labels.points.len() as u64;
    st.line_labels = labels.lines.len() as u64;
    let blob = write::fixture(TILE_VIEW, TILE_VIEW, layers.shift(), &[(0, 0)], &feats);
    Ok(CutTile {
        blob: Some(blob),
        labels,
        buildings: section,
        stats: st,
    })
}

/// One tile's label section, its strings numbered by the region's sorted
/// table.
fn section(labels: &TileLabels, table: &[&str]) -> Res<Vec<u8>> {
    let id = |s: &str| -> Res<u32> {
        let i = table
            .binary_search_by(|t| t.as_bytes().cmp(s.as_bytes()))
            .map_err(|_| "a name missing from the table")?;
        Ok(u32::try_from(i)?)
    };
    let mut points = Vec::new();
    let mut alts = Vec::new();
    for (n, (kind, rank, min_zoom, name, [x, y])) in labels.points.iter().enumerate() {
        points.push(PointLabel {
            kind: *kind,
            rank: *rank,
            min_zoom: *min_zoom,
            flags: 0,
            string: id(&name.local)?,
            x: *x,
            y: *y,
        });
        if let Some(a) = &name.alternate {
            alts.push((u16::try_from(n)?, id(a)?));
        }
    }
    let mut lines = Vec::new();
    for (n, (feature, kind, min_zoom, name)) in labels.lines.iter().enumerate() {
        lines.push(LineLabel {
            feature: *feature,
            kind: *kind,
            min_zoom: *min_zoom,
            string: id(&name.local)?,
        });
        if let Some(a) = &name.alternate {
            alts.push((u16::try_from(points.len() + n)?, id(a)?));
        }
    }
    Ok(encode_section(&points, &lines, &alts))
}

/// How far a cut has come: after each round of tiles is written, and
/// once at the end with every tile and the file complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Tiles written (slots filled), of `tiles`.
    pub done: usize,
    pub tiles: usize,
    /// The deepest zoom written so far.
    pub zoom: u8,
    /// Bytes of the file so far.
    pub bytes: u64,
    /// The names, the buildings and the file length are written.
    pub complete: bool,
}

/// `ids` (ascending) in rounds of at most `round` tiles, a round ending
/// where a zoom does: the shallow zooms land each in a round of its own,
/// so a reader draws the outline after a few kilobytes, not after the
/// first round's worth of the deeper zooms (1291).
pub fn rounds(ids: &[TileId], round: usize) -> Vec<&[TileId]> {
    let mut out = Vec::new();
    let mut rest = ids;
    while let Some(first) = rest.first() {
        let same = rest.iter().take_while(|t| t.z == first.z).count();
        let (head, tail) = rest.split_at(same.min(round.max(1)));
        out.push(head);
        rest = tail;
    }
    out
}

/// Cut `src` into `out` under `opts`.
pub fn cut<S: TileSource + ?Sized, W: Write + Seek>(
    src: &S,
    opts: &CutOptions,
    out: W,
) -> Res<CutStats> {
    cut_streamed(src, opts, out, &mut |_| {})
}

/// [`cut`], telling `progress` after each round, when a reader of the
/// file sees the round's tiles.
pub fn cut_streamed<S: TileSource + ?Sized, W: Write + Seek>(
    src: &S,
    opts: &CutOptions,
    mut out: W,
    progress: &mut dyn FnMut(Progress),
) -> Res<CutStats> {
    let (lo, hi) = (*opts.zooms.start(), *opts.zooms.end());
    if lo > hi || hi > leviculum_map::region::MAX_ZOOM {
        return Err(format!("zoom range {lo}..={hi}").into());
    }
    let mut layers = BTreeMap::new();
    for z in lo..=hi {
        let l =
            profile::layers(opts.profile, z).ok_or(format!("unknown profile {}", opts.profile))?;
        layers.insert(z, l);
    }
    let mut ids: Vec<TileId> = src
        .tiles_in(&opts.bounds, opts.zooms.clone())?
        .into_iter()
        .filter(|t| opts.zooms.contains(&t.z) && in_bounds(*t, &opts.bounds))
        .collect();
    ids.sort_unstable();
    ids.dedup();

    let with_buildings = opts.buildings && opts.zooms.contains(&BUILDING_ZOOM);
    // The header, the texts and a slot per tile, all before any tile, so
    // a reader opens the file from the first flush on.
    let mut pos = HEADER_LEN_3 as u64;
    let text = |s: &str, pos: &mut u64| -> Res<(u32, u32)> {
        let at = u32::try_from(*pos)?;
        *pos += s.len() as u64;
        Ok((at, u32::try_from(s.len())?))
    };
    let attribution = text(&opts.attribution, &mut pos)?;
    let notice = text(&opts.notice, &mut pos)?;
    let index = pos;
    let mut header = Header {
        bounds: opts.bounds,
        min_zoom: lo,
        max_zoom: hi,
        profile: opts.profile,
        attribution,
        notice,
        tiles: u32::try_from(ids.len())?,
        index,
        len: 0,
        labels: None,
        buildings: None,
        streamed: true,
    };
    out.write_all(&header.encode())?;
    out.write_all(opts.attribution.as_bytes())?;
    out.write_all(opts.notice.as_bytes())?;
    for id in &ids {
        let slot = Entry {
            id: *id,
            offset: 0,
            len: 0,
        };
        out.write_all(&slot.encode())?;
    }
    pos += (ids.len() * ENTRY_LEN) as u64;
    out.flush()?;

    let mut stats = CutStats::default();
    let mut shared: HashMap<Vec<u8>, (u64, u32)> = HashMap::new();
    let mut named: Vec<(TileId, TileLabels)> = Vec::new();
    let mut built: Vec<(TileId, Vec<u8>)> = Vec::new();
    let threads = opts.threads.max(1);
    let locale = opts.locale.as_deref();
    let mut done = 0;
    for round in rounds(&ids, opts.round) {
        src.want(round)?;
        let per = round.len().div_ceil(threads);
        let results: Vec<Res<CutTile>> = std::thread::scope(|s| {
            let handles: Vec<_> = round
                .chunks(per)
                .map(|part| {
                    let layers = &layers;
                    s.spawn(move || {
                        part.iter()
                            .map(|t| {
                                let raw = src.mvt(*t)?.ok_or("listed tile missing")?;
                                let names = Names {
                                    locale,
                                    z: t.z,
                                    deepest: t.z == hi,
                                };
                                let b = with_buildings && t.z == BUILDING_ZOOM;
                                cut_tile_full(&raw, layers[&t.z], Some(names), b)
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| match h.join() {
                    Ok(v) => v,
                    Err(_) => vec![Err("cut worker panicked".into())],
                })
                .collect()
        });
        let mut slots: Vec<Entry> = Vec::with_capacity(round.len());
        for (t, r) in round.iter().zip(results) {
            let CutTile {
                blob,
                labels,
                buildings: section,
                stats: mut st,
            } = r.map_err(|e| format!("tile {}/{}/{}: {e}", t.z, t.x, t.y))?;
            if !labels.is_empty() {
                named.push((*t, labels));
            }
            if let Some(section) = section {
                built.push((*t, section));
            }
            // A tile with nothing in it says so in its slot.
            let mut slot = Entry {
                id: *t,
                offset: EMPTY_SLOT,
                len: 0,
            };
            if let Some(blob) = blob {
                let len = u32::try_from(blob.len())?;
                let reuse = if blob.len() <= SHARE_MAX {
                    shared.get(&blob).copied()
                } else {
                    None
                };
                let offset = match reuse {
                    Some((offset, _)) => {
                        st.shared += 1;
                        offset
                    }
                    None => {
                        let offset = pos;
                        out.write_all(&blob)?;
                        pos += blob.len() as u64;
                        st.bytes += blob.len() as u64;
                        if blob.len() <= SHARE_MAX {
                            shared.insert(blob, (offset, len));
                        }
                        offset
                    }
                };
                st.entries += 1;
                slot = Entry {
                    id: *t,
                    offset,
                    len,
                };
            }
            slots.push(slot);
            *stats.zooms.entry(t.z).or_default() += st;
        }
        // The round's blobs are out; then the slots' offsets, then their
        // lengths, so a reader never sees a length before its offset nor a
        // slot before its bytes.
        out.flush()?;
        let first = index + (done * ENTRY_LEN) as u64;
        out.seek(SeekFrom::Start(first))?;
        let mut offsets = Vec::with_capacity(slots.len() * ENTRY_LEN);
        for s in &slots {
            offsets.extend_from_slice(&Entry { len: 0, ..*s }.encode());
        }
        out.write_all(&offsets)?;
        out.flush()?;
        for (i, s) in slots.iter().enumerate().filter(|(_, s)| s.len > 0) {
            out.seek(SeekFrom::Start(first + (i * ENTRY_LEN) as u64 + 16))?;
            out.write_all(&s.len.to_le_bytes())?;
        }
        out.seek(SeekFrom::Start(pos))?;
        out.flush()?;
        done += round.len();
        progress(Progress {
            done,
            tiles: ids.len(),
            zoom: round.last().map_or(lo, |t| t.z),
            bytes: pos,
            complete: false,
        });
    }

    // The names: every string once, sorted, numbered by its place in the
    // table; the label sections after the tiles, then the table, then the
    // label index right before the building index, which ends the file.
    let strings: BTreeSet<&str> = named
        .iter()
        .flat_map(|(_, l)| {
            let points = l.points.iter().map(|p| &p.3);
            let lines = l.lines.iter().map(|p| &p.3);
            points.chain(lines)
        })
        .flat_map(|n| std::iter::once(n.local.as_str()).chain(n.alternate.as_deref()))
        .collect();
    let table: Vec<&str> = strings.into_iter().collect();
    let mut label_entries: Vec<Entry> = Vec::new();
    for (t, labels) in &named {
        let bytes = section(labels, &table)?;
        out.write_all(&bytes)?;
        label_entries.push(Entry {
            id: *t,
            offset: pos,
            len: u32::try_from(bytes.len())?,
        });
        pos += bytes.len() as u64;
        stats.zooms.entry(t.z).or_default().label_bytes += bytes.len() as u64;
    }
    // The building sections after the label sections, before the table.
    let mut building_entries: Vec<Entry> = Vec::new();
    for (t, bytes) in &built {
        out.write_all(bytes)?;
        building_entries.push(Entry {
            id: *t,
            offset: pos,
            len: u32::try_from(bytes.len())?,
        });
        pos += bytes.len() as u64;
        let z = stats.zooms.entry(t.z).or_default();
        z.building_sections += 1;
        z.building_bytes += bytes.len() as u64;
    }
    let (count, bytes) = encode_strings(table.iter().copied());
    let strings_at = pos;
    out.write_all(&bytes)?;
    pos += bytes.len() as u64;
    stats.strings = u64::from(count);
    stats.string_bytes = bytes.len() as u64;
    let label_index = pos;
    for e in &label_entries {
        out.write_all(&e.encode())?;
    }
    pos += (label_entries.len() * ENTRY_LEN) as u64;
    let building_index = pos;
    for e in &building_entries {
        out.write_all(&e.encode())?;
    }
    pos += (building_entries.len() * ENTRY_LEN) as u64;
    out.flush()?;

    // The names' and buildings' fields, then the length alone: a reader
    // takes the file as complete only once the length is there.
    header.labels = Some(LabelTables {
        index: label_index,
        sections: u32::try_from(label_entries.len())?,
        strings: count,
        table: strings_at,
        table_len: bytes.len() as u64,
    });
    header.buildings = Some(BuildingTables {
        index: building_index,
        sections: u32::try_from(building_entries.len())?,
    });
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&header.encode())?;
    out.flush()?;
    out.seek(SeekFrom::Start(LEN_AT))?;
    out.write_all(&pos.to_le_bytes())?;
    out.seek(SeekFrom::Start(pos))?;
    out.flush()?;
    stats.bytes = pos;
    progress(Progress {
        done,
        tiles: ids.len(),
        zoom: hi,
        bytes: pos,
        complete: true,
    });
    Ok(stats)
}
