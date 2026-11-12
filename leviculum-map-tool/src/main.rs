//! `leviculum-map-tool cut | info | render`.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Parser, Subcommand};
use embedded_graphics::prelude::*;
use leviculum_map::draw::{draw_in, draw_tiles, Ink};
use leviculum_map::mercator::{project, LatLon};
use leviculum_map::region::{Bounds, Region};
use leviculum_map::viewport::Viewport;
use leviculum_mapcut::cut::{cut, CutOptions, ZoomStats};
use leviculum_mapcut::http::HttpRange;
use leviculum_mapcut::pmtiles::{Archive, RangeRead};
use leviculum_mapcut::{notice, source_line, Res, TileSource, ATTRIBUTION};
use leviculum_mapfill::fill::{Edge, Scratch};
use leviculum_mapfill::frame::Frame;
use leviculum_mapfill::render::{NoClock, Work};
use leviculum_mapfill::style::{Profile, BACKGROUND};

/// See the manifest: musl's allocator made the cut kernel-bound.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(about = "Cut and inspect leviculum map region files")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Cut a region file from Protomaps PMTiles archives: one, or the parts
    /// of one build downloaded separately; an `http(s)://` input is read by
    /// range requests, only the box's tiles.
    Cut {
        #[arg(short, long)]
        output: PathBuf,
        #[arg(required = true)]
        inputs: Vec<String>,
        /// west,south,east,north in degrees.
        #[arg(long, allow_hyphen_values = true)]
        bbox: String,
        #[arg(long, default_value_t = 0)]
        min_zoom: u8,
        #[arg(long, default_value_t = 15)]
        max_zoom: u8,
        #[arg(long, default_value_t = leviculum_map::profile::CUT_2)]
        profile: u8,
        /// Worker threads; the output does not depend on it.
        #[arg(long)]
        threads: Option<usize>,
        /// Keep names in this language (`de`, `en`) beside the local ones
        /// where they differ; without it the local names alone.
        #[arg(long)]
        locale: Option<String>,
        /// Cut the z15 tiles' building footprints into building sections
        /// (region version 3); `--buildings false` writes version 2.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        buildings: bool,
        /// Cut every point of interest of the deepest zoom into poi
        /// sections, with the travel attributes the source carries
        /// (region version 5, 1685); `--pois false` writes version 4.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        pois: bool,
    },
    /// Copy the tiles of a box of a PMTiles archive into a smaller one, as
    /// they are: the test fixture of the autoload (leviculum-mapcut/fixtures).
    Extract {
        #[arg(short, long)]
        output: PathBuf,
        input: String,
        /// west,south,east,north in degrees.
        #[arg(long, allow_hyphen_values = true)]
        bbox: String,
        #[arg(long, default_value_t = 0)]
        min_zoom: u8,
        #[arg(long, default_value_t = 15)]
        max_zoom: u8,
    },
    /// Print a region's header and per-zoom sizes, after checking it whole.
    Info { region: PathBuf },
    /// Print the names a region holds at one zoom, a line each.
    Labels {
        region: PathBuf,
        #[arg(long)]
        zoom: u8,
    },
    /// Draw one view of a region into a binary PPM.
    Render {
        region: PathBuf,
        output: PathBuf,
        #[arg(long, allow_hyphen_values = true)]
        lat: f64,
        #[arg(long, allow_hyphen_values = true)]
        lon: f64,
        #[arg(long)]
        zoom: u8,
        #[arg(long, default_value_t = 240)]
        width: u32,
        #[arg(long, default_value_t = 320)]
        height: u32,
    },
}

fn parse_bbox(s: &str) -> Res<Bounds> {
    let v: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()?;
    let [w, so, e, n] = v[..] else {
        return Err("--bbox wants west,south,east,north".into());
    };
    let e7 = |d: f64| (d * 1e7).round() as i32;
    Ok(Bounds {
        west: e7(w),
        south: e7(so),
        east: e7(e),
        north: e7(n),
    })
}

fn print_zoom(label: &str, s: &ZoomStats) {
    println!(
        "MAPTOOL zoom={label} tiles={} entries={} empty={} shared={} bytes={} features_in={} features_out={} vertices_in={} vertices_out={} over_device={} clamped={} resimplified={} point_labels={} line_labels={} label_bytes={}",
        s.tiles, s.entries, s.empty, s.shared, s.bytes, s.features_in, s.features_out,
        s.vertices_in, s.vertices_out, s.over_device, s.clamped, s.resimplified,
        s.point_labels, s.line_labels, s.label_bytes
    );
    if s.buildings_in > 0 {
        println!(
            "MAPTOOL buildings zoom={label} pieces_in={} footprints={} vertices={} sections={} bytes={} degenerate={} over_device={}",
            s.buildings_in, s.buildings_out, s.building_vertices, s.building_sections,
            s.building_bytes, s.buildings_degenerate, s.buildings_over_device
        );
    }
}

/// What `cut` keeps of its source, beside the box and the zooms.
struct Keep {
    profile: u8,
    threads: Option<usize>,
    locale: Option<String>,
    buildings: bool,
    pois: bool,
}

fn cmd_cut(inputs: &[String], output: &Path, bbox: &str, zooms: (u8, u8), keep: Keep) -> Res<()> {
    let Keep {
        profile,
        threads,
        locale,
        buildings,
        pois,
    } = keep;
    let t = Instant::now();
    let bounds = parse_bbox(bbox)?;
    let remote = |i: &String| i.starts_with("http://") || i.starts_with("https://");
    let archives = inputs
        .iter()
        .map(|i| match remote(i) {
            true => Archive::from_source(Input::Http(HttpRange::new(i))),
            false => Archive::from_source(Input::File(File::open(i)?)),
        })
        .collect::<Res<Vec<_>>>()?;
    let opts = CutOptions {
        bounds,
        zooms: zooms.0..=zooms.1,
        profile,
        attribution: ATTRIBUTION.to_owned(),
        notice: notice("leviculum-map-tool", profile, &source_line(&archives[0])),
        threads: threads
            .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
            .unwrap_or(1),
        locale,
        buildings,
        pois,
        round: leviculum_mapcut::cut::ROUND,
    };
    let file = BufWriter::new(File::create(output)?);
    // A remote archive's tiles of the box are fetched first, in few
    // ranges, where a tile at a time would cost a request each; files are
    // read as the cut goes, so a country's box never sits in memory.
    let (stats, input_bytes) = match inputs.iter().any(remote) {
        true => {
            let mut fetched = Vec::new();
            for a in &archives {
                fetched.push(a.fetch(&a.locate_in(&bounds, opts.zooms.clone())?)?);
            }
            let bytes = fetched.iter().map(|f| f.bytes).sum();
            (cut(&Parts(&fetched), &opts, file)?, bytes)
        }
        false => {
            let mut bytes = 0;
            for p in inputs {
                bytes += std::fs::metadata(p)?.len();
            }
            (cut(archives.as_slice(), &opts, file)?, bytes)
        }
    };
    let cut_s = t.elapsed().as_secs_f64();
    for (z, s) in &stats.zooms {
        print_zoom(&z.to_string(), s);
    }
    let region = Region::open(File::open(output)?).map_err(|e| format!("{e:?}"))?;
    region
        .verify()
        .map_err(|e| format!("written region does not verify: {e:?}"))?;
    println!(
        "MAPTOOL cut inputs={} input_bytes={input_bytes} region_bytes={} entries={} strings={} string_bytes={} threads={} seconds={cut_s:.2}",
        inputs.len(),
        stats.bytes,
        region.header().tiles,
        stats.strings,
        stats.string_bytes,
        opts.threads
    );
    Ok(())
}

/// An input of `cut`: a file, or an archive on a server.
enum Input {
    File(File),
    Http(HttpRange),
}

impl RangeRead for Input {
    fn read_range(&self, offset: u64, len: u64) -> Res<Vec<u8>> {
        match self {
            Self::File(f) => f.read_range(offset, len),
            Self::Http(h) => h.read_range(offset, len),
        }
    }
}

/// The fetched parts of one build read as one, a tile from the first part
/// that holds it.
struct Parts<'a>(&'a [leviculum_mapcut::pmtiles::Fetched]);

impl TileSource for Parts<'_> {
    fn tiles_in(
        &self,
        bounds: &Bounds,
        zooms: std::ops::RangeInclusive<u8>,
    ) -> Res<Vec<leviculum_map::region::TileId>> {
        let mut all = Vec::new();
        for f in self.0 {
            all.extend(f.tiles_in(bounds, zooms.clone())?);
        }
        Ok(all)
    }

    fn mvt(&self, t: leviculum_map::region::TileId) -> Res<Option<Vec<u8>>> {
        for f in self.0 {
            if let Some(b) = f.mvt(t)? {
                return Ok(Some(b));
            }
        }
        Ok(None)
    }
}

fn cmd_extract(
    input: &str,
    output: &Path,
    bbox: &str,
    zooms: std::ops::RangeInclusive<u8>,
) -> Res<()> {
    let archive = match input.starts_with("http://") || input.starts_with("https://") {
        true => Archive::from_source(Input::Http(HttpRange::new(input)))?,
        false => Archive::from_source(Input::File(File::open(input)?))?,
    };
    let located = archive.locate_in(&parse_bbox(bbox)?, zooms)?;
    let fetched = archive.fetch(&located)?;
    let mut tiles = std::collections::BTreeMap::new();
    for t in fetched.tiles() {
        if let Some(raw) = fetched.raw(t) {
            tiles.insert(t, raw.to_vec());
        }
    }
    // The metadata's text fields alone (name, version, attribution, the
    // OSM date): its layer lists and statistics are most of a small
    // archive, and nothing here reads them.
    let metadata: serde_json::Map<String, serde_json::Value> = match archive.metadata()? {
        serde_json::Value::Object(m) => m.into_iter().filter(|(_, v)| v.is_string()).collect(),
        _ => serde_json::Map::new(),
    };
    let out = BufWriter::new(File::create(output)?);
    leviculum_mapcut::pmtiles::write(
        &tiles,
        archive.header.tile_compression,
        &serde_json::Value::Object(metadata),
        out,
    )?;
    println!(
        "MAPTOOL extract tiles={} fetched_bytes={} requests={} bytes={}",
        tiles.len(),
        fetched.bytes,
        fetched.requests,
        std::fs::metadata(output)?.len()
    );
    Ok(())
}

fn cmd_info(path: &Path) -> Res<()> {
    let region = Region::open(File::open(path)?).map_err(|e| format!("{e:?}"))?;
    region.verify().map_err(|e| format!("{e:?}"))?;
    let h = region.header();
    println!(
        "MAPTOOL info bytes={} zooms={}..={} profile={} tiles={} bounds={},{},{},{}",
        h.len,
        h.min_zoom,
        h.max_zoom,
        h.profile,
        h.tiles,
        h.bounds.west,
        h.bounds.south,
        h.bounds.east,
        h.bounds.north
    );
    println!("MAPTOOL attribution {}", region.attribution());
    println!("MAPTOOL notice {}", region.notice());
    let mut per = std::collections::BTreeMap::<u8, (u64, u64)>::new();
    let mut seen = std::collections::HashSet::new();
    for n in 0..h.tiles {
        // A version 4 slot saying nothing is there counts no tile.
        let e = match region.slot(n).map_err(|e| format!("{e:?}"))? {
            (_, leviculum_map::region::Slot::Here(e)) => e,
            (_, leviculum_map::region::Slot::Empty) => continue,
            (id, leviculum_map::region::Slot::Pending) => {
                return Err(format!("tile {}/{}/{} has not arrived", id.z, id.x, id.y).into())
            }
        };
        let z = per.entry(e.id.z).or_default();
        z.0 += 1;
        if seen.insert(e.offset) {
            z.1 += u64::from(e.len);
        }
    }
    let mut labels = std::collections::BTreeMap::<u8, (u64, u64)>::new();
    if let Some(l) = h.labels {
        for n in 0..l.sections {
            let e = region.label_entry(n).map_err(|e| format!("{e:?}"))?;
            let z = labels.entry(e.id.z).or_default();
            z.0 += 1;
            z.1 += u64::from(e.len);
        }
        println!(
            "MAPTOOL info version={} strings={} string_bytes={} label_sections={} building_sections={}",
            h.version_of(),
            l.strings,
            l.table_len,
            l.sections,
            region.building_sections()
        );
    }
    let mut built = std::collections::BTreeMap::<u8, (u64, u64)>::new();
    for n in 0..region.building_sections() {
        let e = region.building_entry(n).map_err(|e| format!("{e:?}"))?;
        let z = built.entry(e.id.z).or_default();
        z.0 += 1;
        z.1 += u64::from(e.len);
    }
    for (z, (n, bytes)) in per {
        let (sections, label_bytes) = labels.get(&z).copied().unwrap_or_default();
        let (building_sections, building_bytes) = built.get(&z).copied().unwrap_or_default();
        println!(
            "MAPTOOL info zoom={z} entries={n} blob_bytes={bytes} label_sections={sections} label_bytes={label_bytes} building_sections={building_sections} building_bytes={building_bytes}"
        );
    }
    Ok(())
}

fn cmd_labels(path: &Path, zoom: u8) -> Res<()> {
    let bytes = std::fs::read(path)?;
    let region = Region::from_bytes(&bytes).map_err(|e| format!("{e:?}"))?;
    let err = |e: leviculum_map::RegionError| format!("{e:?}");
    let sections = region.header().labels.map_or(0, |l| l.sections);
    for n in 0..sections {
        let e = region.label_entry(n).map_err(err)?;
        if e.id.z != zoom {
            continue;
        }
        let Some(s) = region.labels(e.id).map_err(err)? else {
            continue;
        };
        let alt = |n: u16| -> Res<String> {
            Ok(match s.alternate(n) {
                Some(a) => format!(" alt={:?}", region.string(a).map_err(err)?),
                None => String::new(),
            })
        };
        let t = e.id;
        for (n, p) in s.points() {
            println!(
                "MAPTOOL label tile={}/{}/{} point kind={:?} rank={} min_zoom={} at={},{} name={:?}{}",
                t.z, t.x, t.y, p.kind, p.rank, p.min_zoom, p.x, p.y,
                region.string(p.string).map_err(err)?, alt(n)?
            );
        }
        for (n, l) in s.lines() {
            println!(
                "MAPTOOL label tile={}/{}/{} line kind={:?} min_zoom={} feature={} name={:?}{}",
                t.z,
                t.x,
                t.y,
                l.kind,
                l.min_zoom,
                l.feature,
                region.string(l.string).map_err(err)?,
                alt(n)?
            );
        }
    }
    Ok(())
}

fn cmd_render(path: &Path, out: &Path, at: LatLon, zoom: u8, w: u32, h: u32) -> Res<()> {
    let region = Region::open(File::open(path)?).map_err(|e| format!("{e:?}"))?;
    let view = Viewport::new(project(at), zoom, w, h);
    let mut read = Vec::new();
    region
        .read_view(&view, &mut read)
        .map_err(|e| format!("{e:?}"))?;
    let tiles = draw_tiles(&read).map_err(|e| format!("{e:?}"))?;
    let mut fb = vec![0u16; (w * h) as usize];
    let mut edges = vec![Edge::default(); 4096];
    let mut active = vec![0u16; 4096];
    let mut heads = vec![0u16; h as usize];
    let mut points = vec![Point::zero(); 4096];
    let mut ring_ends = vec![0u16; 1024];
    let mut frame = Frame::new(&mut fb, w, h).ok_or("frame")?;
    let mut scratch = Scratch {
        edges: &mut edges,
        heads: &mut heads,
        active: &mut active,
    };
    let mut work = Work {
        points: &mut points,
        ring_ends: &mut ring_ends,
    };
    // Drawn as a frontend draws it: into the region's box, the rest of
    // the view left at the background (1077).
    frame.clear(BACKGROUND)?;
    let area = region.area(&view);
    let t = Instant::now();
    let st = draw_in(
        &tiles,
        area,
        Profile::Full,
        Ink::Colour,
        &mut frame,
        &mut scratch,
        &mut work,
        &mut NoClock,
    )?;
    let ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "MAPTOOL render tiles={} features={} culled={} vertices={} overflows={} fill_pixels={} stroke_pixels={} ms={ms:.3}",
        tiles.len(), st.features, st.culled, st.vertices, st.overflows, st.fill.pixels, st.stroke.pixels
    );
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    frame.rgb888_into(&mut ppm);
    std::fs::write(out, ppm)?;
    Ok(())
}

fn main() -> Res<()> {
    match Cli::parse().cmd {
        Cmd::Cut {
            inputs,
            output,
            bbox,
            min_zoom,
            max_zoom,
            profile,
            threads,
            locale,
            buildings,
            pois,
        } => cmd_cut(
            &inputs,
            &output,
            &bbox,
            (min_zoom, max_zoom),
            Keep {
                profile,
                threads,
                locale,
                buildings,
                pois,
            },
        ),
        Cmd::Extract {
            output,
            input,
            bbox,
            min_zoom,
            max_zoom,
        } => cmd_extract(&input, &output, &bbox, min_zoom..=max_zoom),
        Cmd::Info { region } => cmd_info(&region),
        Cmd::Labels { region, zoom } => cmd_labels(&region, zoom),
        Cmd::Render {
            region,
            output,
            lat,
            lon,
            zoom,
            width,
            height,
        } => cmd_render(
            &region,
            &output,
            LatLon::from_degrees(lat, lon),
            zoom,
            width,
            height,
        ),
    }
}
