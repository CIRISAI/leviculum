//! `leviculum-map`: the map core every frontend draws through (Codeberg
//! #474, `docs/src/concepts/loquax-map.md`, section 3).
//!
//! - [`region`]: the region file, a tile pyramid of `leviculum-mapfill`'s
//!   flat geometry behind an index, read in place through a [`Storage`].
//! - [`mercator`]: Web Mercator between degrees and a 32-bit world
//!   coordinate.
//! - [`blend`]: the world view's projection (1710): Equal Earth or the
//!   orthographic globe at world zooms, blending into Mercator as the
//!   view zooms in, and the Newton inverse for hits.
//! - [`viewport`]: centre, zoom, pan, fit, and the tiles a view needs.
//! - [`profile`]: which classes each zoom of a region keeps, and how its
//!   geometry was simplified.
//! - [`pins`]: pins and their labels placed in a view, as data.
//! - [`buildings`]: the building footprints a region carries (LMR3), per
//!   z15 tile in a 4 x 4 grid of cells.
//! - [`labels`]: the names a region carries (LMR2) and the one rule every
//!   frontend places them by.
//! - [`draw`]: the tiles of one view drawn into any `DrawTarget`, class by
//!   class across the tiles, through `leviculum-mapfill`, in colour or as
//!   lines alone, at the line widths of the view's zoom.
//! - [`outline`]: the world outline, Natural Earth 1:50m land, where no
//!   region covers the view.
//! - [`countries`]: the country a point lies in, Natural Earth 1:50m, and
//!   its emergency numbers, driving side and currency, from Wikidata.
//! - [`ladder`]: the frame budget, which passes a moving frame runs and
//!   how a view at rest grows back to the full picture.
//! - [`cells`]: a view as terminal cells, the text techniques of the
//!   terminal map, for every terminal frontend.
//! - [`choice`]: which installed region a view draws, and the zooms a
//!   frontend offers.
//! - [`area`]: the regions a board carries in a flash partition of their
//!   own, and the directory that names them (1477).
//! - [`timezones`]: the world's time zones, which zone a point lies in,
//!   where a view writes each zone's time, and the clock rules: TZif and
//!   POSIX TZ strings, the system's tzdata, the board's table.
//! - [`declination`]: magnetic declination and field from the World
//!   Magnetic Model WMM2025, for turning a compass bearing into a true one.
//! - [`geoid`]: the EGM2008 geoid at 15', so a GNSS ellipsoid height
//!   becomes a height above sea level.
//! - [`elevation`]: height pieces, ground heights in metres per z12 tile,
//!   the coarser zooms averaged on the device (1740).
//! - [`style`]: the colour tables, `leviculum-mapfill`'s own.
//!
//! `no_std` with `alloc`; the feature `std` adds only a [`Storage`] for a
//! file. Text is never drawn here: labels are data a frontend places.

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub mod area;
pub mod blend;
pub mod buildings;
pub mod cells;
pub mod choice;
pub mod countries;
pub mod declination;
pub mod draw;
pub mod elevation;
pub mod geoid;
pub mod labels;
pub mod ladder;
pub mod mercator;
pub mod outline;
pub mod pins;
pub mod pois;
pub mod profile;
pub mod region;
pub mod timezones;
pub mod viewport;

/// The colour tables and classes, one table for the map and the filler.
pub use leviculum_mapfill::style;

pub use mercator::{LatLon, World};
pub use region::{Region, RegionError, Storage, TileId};
pub use viewport::Viewport;
