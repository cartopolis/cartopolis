//! Client-side vector-tile rendering: Mapbox Vector Tile (MVT) → RGBA pixels.
//!
//! The map streamer wants a *texture* per [`TileKey`]; until now that came from
//! `tile.openstreetmap.org` as a pre-rendered PNG. VersaTiles serves the same
//! world as **Shortbread-schema MVT** — protobuf geometry, no pixels — which is
//! 62 GiB for the planet instead of the ~100 TB a rendered raster planet would
//! be. That is the whole reason to self-host (see ADR-035): we ship geometry and
//! draw it here.
//!
//! Everything in this module is pure CPU and pure Rust (`mvt-reader` + `tiny-skia`,
//! no C deps), so it runs on the tile worker pool exactly where `image::load_from_memory`
//! used to, and it survives the wasm32 target (ADR-033).
//!
//! Two things the raster path never had to do:
//!
//! * **Overzoom.** Shortbread tops out at z14 but `map_stream` streams to z17, so
//!   a request above [`MAX_SOURCE_ZOOM`] fetches the z14 ancestor and renders the
//!   sub-rectangle that corresponds to the requested tile ([`SubTile`]). Vector
//!   data stays sharp under that magnification — that is the payoff.
//! * **Styling.** There is no cartography in the data. [`PAINT_ORDER`] *is* the
//!   map's look, bottom layer first.

use std::sync::OnceLock;

use geo_types::Geometry;
use mvt_reader::feature::{Feature, Value};
use mvt_reader::layer::Layer;
use mvt_reader::Reader;
use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Shader, Stroke, Transform,
};

use std::collections::HashMap;

use crate::ground_stack::GroundLayer;
use crate::road_texture::{road_class, road_width_m, RoadClass, StreetFlags, SurveyedCover};

// One file per responsibility. Every submodule reads its siblings through
// `use super::*`, and everything public is re-exported here, so
// `cartopolis_geo::vector_tiles::X` (and the client's `systems::vector_tiles::X`)
// name the same items they did when this was one file.
mod bridges;
mod geometry;
mod labels;
mod ladder;
mod landcover;
mod painted;
mod raster;
mod ribbon;
mod roads;
mod sniff;
mod surfaces;
mod tile;
mod water;

pub use bridges::*;
pub(crate) use geometry::*;
pub use labels::*;
pub use ladder::*;
pub use landcover::*;
pub use painted::*;
pub use raster::*;
pub(crate) use ribbon::*;
pub use roads::*;
pub use sniff::*;
pub use surfaces::*;
pub use tile::*;
pub use water::*;

/// Depth-buffer units between adjacent rungs of the coplanar ground stack.
///
/// Re-exported from [`crate::ground_stack`], which is where the ladder itself
/// lives. It stays visible under this name because most of the tree reaches the
/// stack through `systems::vector_tiles`, and because the step is what
/// `assets/shaders/road_depth.wesl` has to agree with.
pub use crate::ground_stack::DEPTH_BIAS_STEP;

/// 0xRRGGBB palette. Deliberately flatter and less saturated than osm-carto: this
/// is a texture on the ground of a 3D world, not a map you read at arm's length —
/// high-contrast cartography reads as noise once it is lit and viewed at a grazing
/// angle. Sits under the same lighting as everything else.
///
/// **Realistic: the ground as it looks from the air** (2026-09-27). Two styled
/// ends were tried and withdrawn: pale cartographic paper (grass at `#D2DEC6`,
/// value 0.87, saturation 0.11), which lit by the sun rendered the countryside
/// and the globe white with grey lines on them; and a game green at saturation
/// 0.54 (bc1dfdb, reverted in dc01bc5), which read as confetti. The land tones
/// are now taken from aerial and satellite imagery — pasture, forest canopy,
/// ripe grain, desert sand, the grey-brown of a town — while the roads, water
/// and labels stay the map's. What this map adds over a photograph is the
/// drawing, not the colour.
pub mod palette;

/// A hand-rolled MVT encoder, for the tests that need a body no tile server
/// would send.
///
/// The Groningen fixture answers "does this work on real data"; this answers the
/// other half — what happens when the data is hostile, or merely unusual (two
/// layers at different extents, a coordinate past the tile). Kept to the four
/// fields anything here actually reads: a layer's name and extent, a feature's
/// type and geometry. `pub(crate)` because `furniture` and `scatter` need the
/// same hostile tiles.
#[cfg(test)]
pub(crate) mod synth;

/// Parse-then-call shims, for callers that hold only bytes: the fixture tests
/// (which are asserting about one tile in isolation, where a shared [`TileData`]
/// would only be ceremony) and the `TileData`-vs-bytes equivalence test.
///
/// Production code must **not** use these — a `&[u8]` call site is exactly the
/// duplicate parse `TileData` exists to remove.
///
/// This is also the one test-helper module the submodules' test suites share.
#[cfg(test)]
mod bytes;
