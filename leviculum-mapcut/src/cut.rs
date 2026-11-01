//! The cut: tiles of a source in a bounding box and a zoom range, under a
//! layer profile, into a region file.
//!
//! Deterministic: the tiles are visited in index order whatever the source
//! lists, each tile's blob depends on that tile's bytes alone, the worker
//! threads only compute blobs that are then written in order, and nothing
//! of the time or the machine goes into the file. Same input, same bytes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Seek, SeekFrom, Write};
use std::ops::RangeInclusive;

use leviculum_map::labels::{encode_section, encode_strings, LabelKind, LineLabel, PointLabel};
use leviculum_map::mercator::{x_of_lon, y_of_lat};
use leviculum_map::profile::{self, ZoomLayers};
use leviculum_map::region::{
    Bounds, Entry, Header, LabelTables, TileId, ENTRY_LEN, HEADER_LEN_2, TILE_VIEW,
};
use leviculum_mapfill::fixture::{write, Kind};

use crate::mvt::{self, Name};
use crate::simplify::{area2, douglas_peucker, quantise_path, simplify_ring, Pt};
use crate::{Res, TileSource};

/// Blobs up to this size are stored once and shared by every tile that
/// has the same bytes: the uniform tiles of sea, forest and farmland.
const SHARE_MAX: usize = 4096;
/// Tiles handed to the workers per round.
const ROUND: usize = 1024;
/// The device's work capacity per feature (`leviculum-mapfill/m9`): a
/// feature with more vertices is skipped there as an overflow.
pub const DEVICE_POINTS: usize = 4096;
/// The device's rings per feature (`leviculum-mapfill/m9`).
pub const DEVICE_RINGS: usize = 1024;
/// The coarsest tolerance a feature over [`DEVICE_POINTS`] is simplified
/// at, in sub-pixels: four pixels.
pub const MAX_TOLERANCE: i64 = 64;

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
    if feats.is_empty() {
        // No entry, so no label section either: a reader looks names up
        // by the tiles it draws.
        st.empty = 1;
        return Ok((None, TileLabels::default(), st));
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
    Ok((Some(blob), labels, st))
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

/// Cut `src` into `out` under `opts`.
pub fn cut<S: TileSource + ?Sized, W: Write + Seek>(
    src: &S,
    opts: &CutOptions,
    mut out: W,
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

    out.write_all(&[0u8; HEADER_LEN_2])?;
    let mut pos = HEADER_LEN_2 as u64;
    let mut text = |s: &str, pos: &mut u64| -> Res<(u32, u32)> {
        let at = u32::try_from(*pos)?;
        out.write_all(s.as_bytes())?;
        *pos += s.len() as u64;
        Ok((at, u32::try_from(s.len())?))
    };
    let attribution = text(&opts.attribution, &mut pos)?;
    let notice = text(&opts.notice, &mut pos)?;

    let mut stats = CutStats::default();
    let mut entries: Vec<Entry> = Vec::new();
    let mut shared: HashMap<Vec<u8>, (u64, u32)> = HashMap::new();
    let mut named: Vec<(TileId, TileLabels)> = Vec::new();
    let threads = opts.threads.max(1);
    let locale = opts.locale.as_deref();
    type Tile = (Option<Vec<u8>>, TileLabels, ZoomStats);
    for round in ids.chunks(ROUND) {
        let per = round.len().div_ceil(threads);
        let results: Vec<Res<Tile>> = std::thread::scope(|s| {
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
                                cut_tile_named(&raw, layers[&t.z], Some(names))
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
        for (t, r) in round.iter().zip(results) {
            let (blob, labels, mut st) =
                r.map_err(|e| format!("tile {}/{}/{}: {e}", t.z, t.x, t.y))?;
            if !labels.is_empty() {
                named.push((*t, labels));
            }
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
                entries.push(Entry {
                    id: *t,
                    offset,
                    len,
                });
            }
            *stats.zooms.entry(t.z).or_default() += st;
        }
    }

    // The names: every string once, sorted, numbered by its place in the
    // table; the label sections after the tiles, then the table, then the
    // label index right before the tile index.
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

    let index = pos;
    for e in &entries {
        out.write_all(&e.encode())?;
    }
    pos += (entries.len() * ENTRY_LEN) as u64;
    let header = Header {
        bounds: opts.bounds,
        min_zoom: lo,
        max_zoom: hi,
        profile: opts.profile,
        attribution,
        notice,
        tiles: u32::try_from(entries.len())?,
        index,
        len: pos,
        labels: Some(LabelTables {
            index: label_index,
            sections: u32::try_from(label_entries.len())?,
            strings: count,
            table: strings_at,
            table_len: bytes.len() as u64,
        }),
        buildings: None,
    };
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&header.encode())?;
    out.flush()?;
    stats.bytes = pos;
    Ok(stats)
}
