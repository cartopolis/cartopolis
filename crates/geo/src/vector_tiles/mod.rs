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
mod geometry;
mod labels;
mod ladder;
mod ribbon;
mod sniff;
mod water;

pub use geometry::*;
pub use labels::*;
pub use ladder::*;
pub use ribbon::*;
pub use sniff::*;
pub use water::*;

/// One tile body, parsed **once**, with its decoded features memoised per layer.
///
/// Everything that reads an MVT used to take `&[u8]` and open its own [`Reader`],
/// which meant `Reader::new(bytes.to_vec())` — a full copy plus a prost decode of
/// the whole tile — per *function*. A single `map_geometry` cell ran four of them
/// over the same body ([`tessellate_surfaces`], [`tessellate_roads`],
/// [`vegetation::scatter_vegetation`], [`water_shore_field`]) and a globe tile ran
/// two ([`render_mvt`], [`ocean_mask`]). Taking `&TileData` instead makes the
/// parse the caller's, once, and every consumer of that body shares it.
///
/// The per-layer memo matters for a second reason: `Reader::get_features` decodes
/// geometry *and* properties into a fresh `Vec<Feature>` on **every** call, and
/// [`PAINT_ORDER`] lists `streets` twice (casing then fill) and `buildings` twice
/// (fill then outline) — so the two heaviest layers of every rendered tile were
/// decoded twice each. `OnceLock` (not `RefCell`) because a decoded layer is never
/// invalidated; the interior mutability is initialise-once, so `&TileData` stays
/// the shape every consumer wants.
///
/// **`OnceLock`, not `OnceCell`, so this is `Sync`** — which is what lets one
/// parse be shared between worker threads rather than merely between the
/// consumers on one. That was the blocker on collapsing the repeat parses a fresh
/// anchor makes: the overzoom fan-out resolves up to 64 clipmap tiles to the same
/// z14 ancestor, and each was a separate task doing its own full `bytes.to_vec()`
/// and prost decode. `cartopolis`'s `tile_loader::parse_shared` is the cache; the
/// memory question it was waiting on is answered in
/// `docs/notes/allocation-budgets.md` (12.5 MiB peak for a fully-decoded dense
/// z14 tile), which is also why that cache holds `Weak` references and not strong
/// ones.
///
/// The lock is uncontended in the steady state — `get_or_init` on an initialised
/// `OnceLock` is an acquire load — so the sharing costs the single-threaded path
/// nothing measurable.
pub struct TileData {
    reader: Reader,
    layers: Vec<Layer>,
    /// Decoded features per `Layer::layer_index`. Empty for a layer that failed
    /// to decode — the byte-taking callers all treated that as "skip the layer",
    /// and folding it in here keeps that behaviour in one place.
    features: Vec<OnceLock<Vec<Feature>>>,
}

impl TileData {
    /// Parse a tile body. The one `Reader::new` in the client.
    ///
    /// Every byte here arrived over the network from a tile server we do not run
    /// the code of, so the body is checked against [`guard_tile_body`] *before*
    /// `mvt-reader` sees it — an ordinary `Err` for a tile that would otherwise
    /// abort the process. See that function for what is being defended against.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        guard_tile_body(bytes)?;
        let reader = Reader::new(bytes.to_vec()).map_err(|e| format!("mvt parse: {e:?}"))?;
        let layers = reader
            .get_layer_metadata()
            .map_err(|e| format!("mvt layers: {e:?}"))?;
        // Indexed by `layer_index`, which the reader assigns as the layer's
        // position — but size from the maximum rather than the count, so a source
        // that ever numbered them otherwise cannot index out of bounds.
        let slots = layers.iter().map(|l| l.layer_index + 1).max().unwrap_or(0);
        Ok(Self {
            reader,
            layers,
            features: (0..slots).map(|_| OnceLock::new()).collect(),
        })
    }

    /// Every layer's metadata, in tile order.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// A named layer's metadata, or `None` if the tile has no such layer.
    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }

    /// A layer's decoded features, decoded on first ask and reused after.
    pub fn features(&self, layer: &Layer) -> &[Feature] {
        let Some(slot) = self.features.get(layer.layer_index) else {
            return &[];
        };
        slot.get_or_init(|| {
            self.reader
                .get_features(layer.layer_index)
                .unwrap_or_default()
        })
    }

    /// A layer's extent, with Shortbread's default substituted for a source that
    /// reports none. Every consumer needs this and every one of them wrote it out.
    pub fn extent(layer: &Layer) -> f32 {
        if layer.extent == 0 {
            4096.0
        } else {
            layer.extent as f32
        }
    }
}

/// Sharing a parse across worker threads is the whole reason [`TileData`]'s memo
/// is a `OnceLock`, and nothing else in this crate would notice if that stopped
/// being true — the crate is single-threaded, so a regression to `OnceCell` would
/// compile and test green here and fail only in `cartopolis`, at the cache.
const _: fn() = || {
    fn assert_shareable<T: Send + Sync>() {}
    assert_shareable::<TileData>();
};

// ─── Guarding a body we did not author ───────────────────────────────────────

/// Ceiling on a vector-tile body, in bytes.
///
/// Measured against what a real Shortbread z14 tile actually weighs: the
/// Groningen city-centre fixture in `tests/fixtures` — 8,552 buildings, 95
/// merged street features, sixteen other layers — is **495 KB**, and the
/// note in `docs/notes/tile-raster-cost.md` puts a typical body at ~70 KB. So
/// this is roughly 16× the densest tile anyone here has seen, which is headroom
/// rather than a working limit, and it sits an order of magnitude below
/// `platform::http`'s 64 MiB `MAX_BODY` — that cap is a whole-response ceiling
/// shared with imagery and is far too loose to be the only thing between a
/// hostile tile server and the allocator.
pub const MAX_TILE_BYTES: usize = 8 * 1024 * 1024;

/// **There is no capacity ceiling any more, and that is a measurement, not a
/// retreat.** Kept as a named constant only so the note below has somewhere to
/// live and nothing silently re-adds one.
///
/// A guard here shipped on 2026-08-11 and was reverted the same day, because it
/// refused real tiles. What follows is what was measured, so nobody rebuilds it.
///
/// The idea was to walk the protobuf, compute what `mvt-reader` will
/// `Vec::with_capacity`, and refuse a body that asks for too much. The attack is
/// real: a body of `[9, 0, 0]` repeated is all `MoveTo`, and each one reserves the
/// whole feature's command length, so cost grows quadratically. Measured with
/// `VmPeak` across a full `get_features`:
///
/// | body | estimate | actual virtual cost |
/// |------|----------|---------------------|
/// | bomb, 12 KB | 48 M slots | **+320 MiB** |
/// | bomb, 30 KB | 300 M | **+1.9 GiB** |
/// | bomb, 60 KB | 1 200 M | **+6.9 GiB** |
///
/// But the estimate does not predict cost, and that is the finding:
///
/// | body | estimate | worst single feature | actual virtual cost |
/// |------|----------|---------------------|---------------------|
/// | z10 10/526/334, 69 KB | 57 M | 55 M | none measurable |
/// | z10 10/527/336, 79 KB | 92 M | 74 M | none measurable |
/// | z10 10/534/336, 157 KB | **545 M** | **421 M** | **none measurable** |
/// | z14 Groningen, 653 KB | 12 M | 8 M | none measurable |
///
/// A real z10 tile asks for 545 M slots — more than a 30 KB bomb — and costs
/// nothing, because generalised low-zoom geometry is one enormous multi-part
/// coastline whose parts do not stay live at once. A 12 KB bomb asks for 48 M and
/// costs 320 MiB. No threshold on the total separates them; neither does the
/// worst single feature (421 M real against 48 M bomb); neither does slots per
/// body byte (3 463 real against 3 992 for the tamest bomb). Three metrics, three
/// overlaps — the walker models an allocation pattern `mvt-reader` does not
/// actually have.
///
/// So the guard is gone and only [`MAX_TILE_BYTES`] remains. That leaves the tree
/// where it was before, exposed to a tile server serving a deliberate bomb —
/// which is worth stating plainly rather than papering over: the fix is to bound
/// the allocation where it happens (a patched or vendored `mvt-reader` whose
/// capacity is `min(len, remaining)`), not to guess from the outside.
pub const MAX_TILE_COORD_CAPACITY: usize = usize::MAX;

/// Refuse a tile body that would make `mvt-reader` abort the process.
///
/// **The bug being defended against** (`mvt-reader` 2.4.0, `parse_geometry`):
/// every `MoveTo` and every `ClosePath` starts a fresh
/// `Vec::with_capacity(geometry_data.len())` — the length of the *whole* feature's
/// command stream, not of the part still to come — and each of those vectors is
/// then retained in the finished geometry. The command `[9, 0, 0]` is three bytes
/// and produces one such vector, so a body of `n` bytes of that repeated asks for
/// about `8n²/3` bytes of capacity: ~24 TB from a 3 MB body, which is not an
/// allocation that fails gracefully. `handle_alloc_error` aborts, and the shipped
/// profile is `panic = "abort"` with no `catch_unwind` anywhere, so on a phone
/// that is a SIGABRT with a network-controlled trigger and no user interaction.
///
/// **Why the length cap alone is not the fix.** The blow-up is *quadratic* in the
/// body length, so any cap that still admits a real 495 KB tile also admits a
/// ~700 GB request from a crafted one of the same size. The size ceiling is the
/// cheap first line — it bounds the copy, the prost decode and the scan below —
/// and [`geometry_capacity`] is what actually closes it, by computing the exact
/// capacity mvt-reader will ask for before mvt-reader is handed anything.
///
/// **A body the estimator cannot walk is refused, not waved through.** That is
/// the opposite of the instinct — a guard that blanks the map is worse than one
/// that misses — and it is right here for a specific reason: **anything this
/// walker cannot walk, `prost` cannot decode either.** A truncated record, a
/// length past the end of the buffer, an eleven-byte varint, wire type 6 or 7:
/// all of them are `Tile::decode` errors a moment later, so refusing them costs
/// nothing that was not already lost. The single exception is a proto2 **group**
/// (wire types 3 and 4), which prost *skips* and the MVT schema does not contain
/// anywhere — and which, waved through, is a one-byte way to disarm everything
/// below by appending it to a bomb.
fn guard_tile_body(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_TILE_BYTES {
        return Err(format!(
            "mvt body of {} bytes is past the {MAX_TILE_BYTES}-byte ceiling",
            bytes.len()
        ));
    }
    Ok(())
}

/// Edge length in pixels of a rendered tile. Larger than the 256 px raster tiles
/// we used to fetch: the texture is stretched across a whole region on the ground,
/// and re-rendering costs us CPU rather than bandwidth, so we may as well take the
/// resolution.
///
/// This is a **default, not a law.** Every rasteriser here takes an explicit size
/// ([`render_mvt_sized`], [`ocean_mask_sized`]) and this is the value the flat
/// clipmap passes on a desktop; the globe passes its own `GLOBE_TILE_PX`, and a
/// memory-constrained host passes half of this (`quality::Budget::tile_px`) —
/// the clipmap's ~224 resident tiles are ~310 MB of texture at 512² and ~78 MB
/// at 256², and `render_mvt`'s cost is ~area (`docs/notes/tile-raster-cost.md`,
/// `docs/notes/globe-tile-cost.md`). Nothing in this crate chooses between them:
/// it has no idea what it is running on.
pub const TILE_PX: u32 = 512;

/// Highest zoom the Shortbread source actually contains. Anything above this is
/// rendered by magnifying the z14 ancestor.
pub const MAX_SOURCE_ZOOM: u32 = 14;

/// Ceiling on how far [`SubTile::resolve`] will magnify, as a shift.
///
/// A saturating bound, not a policy — the client never asks for more than a
/// handful of levels of overzoom. It exists because `z` arrives from a `TileKey`
/// that nothing range-checks at that boundary, and the unclamped
/// `1i64 << (z - MAX_SOURCE_ZOOM)` panics in debug and produces nonsense in
/// release once `z` reaches 78.
///
/// 31 rather than a rounder number because [`SubTile::factor`] is a `u32`: at a
/// shift of 32 the `factor as u32` narrowing truncates to **zero**, turning an
/// overflow panic into a silently degenerate sub-tile, which is the worse of the
/// two failures. This is the largest shift the field can actually hold.
pub const MAX_OVERZOOM_SHIFT: u32 = 31;

/// How far past its own edge a tile's geometry may legitimately reach, as a
/// fraction of the layer's extent.
///
/// Encoders clip each layer to the tile plus a buffer, so a way crossing the
/// border appears in both tiles and a polygon on the seam has no visible gap.
/// Measured on the Groningen fixture: every line and area layer stops at ±64 of
/// 4096, i.e. **1.6%**, and the widest anything reaches is the `place_labels`
/// point layer at ~22%. So 25% is generous for real data by an order of
/// magnitude — which is the point. It is not a cartographic tolerance, it is a
/// **sanity window**: `mvt-reader` clips its coordinate cursor only at
/// `i32::MAX` and never to `[0, extent]`, so a hostile (or merely corrupt) tile
/// can hand any consumer here a coordinate near 2.1e9. Everything that buckets
/// segments on a uniform grid must reject those *before* bucketing, because the
/// loop that follows is over the bbox and would otherwise insert ~10^14 entries.
pub const TILE_BUFFER_FRAC: f32 = 0.25;

/// Depth-buffer units between adjacent rungs of the coplanar ground stack.
///
/// Re-exported from [`crate::ground_stack`], which is where the ladder itself
/// lives. It stays visible under this name because most of the tree reaches the
/// stack through `systems::vector_tiles`, and because the step is what
/// `assets/shaders/road_depth.wgsl` has to agree with.
pub use crate::ground_stack::DEPTH_BIAS_STEP;

/// Which sub-rectangle of a source tile to draw, when the requested tile is deeper
/// than [`MAX_SOURCE_ZOOM`]. `factor` is 2^(requested_z − source_z); `(x, y)` is the
/// requested tile's position within the source tile, in units of the sub-grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubTile {
    pub factor: u32,
    pub x: u32,
    pub y: u32,
}

impl SubTile {
    /// The identity view: draw the whole source tile.
    pub const WHOLE: Self = Self {
        factor: 1,
        x: 0,
        y: 0,
    };

    /// Resolve a requested tile to the source tile that must be fetched plus the
    /// sub-rectangle of it to draw. Returns `((z, x, y), sub)`.
    ///
    /// `>>` on a signed coordinate is an arithmetic shift, i.e. floor division —
    /// which is the correct ancestor for a negative `x` produced by antimeridian
    /// wrapping, where a truncating divide would pick the tile one to the east.
    #[must_use]
    pub fn resolve(z: u32, x: i64, y: i64) -> ((u32, i64, i64), Self) {
        // `MAX_OVERZOOM_SHIFT`, not `z - MAX_SOURCE_ZOOM`: `z` reaches here from a
        // `TileKey`, which is not range-checked at this boundary, and `1i64 << 63`
        // is a panic in debug and nonsense in release. Clamping keeps an absurd
        // zoom absurd (the sub-tile is a vanishing speck) instead of a crash on a
        // network-adjacent input path.
        if z <= MAX_SOURCE_ZOOM {
            return ((z, x, y), Self::WHOLE);
        }
        let shift = (z - MAX_SOURCE_ZOOM).min(MAX_OVERZOOM_SHIFT);
        let factor = 1i64 << shift;
        let (sx, sy) = (x >> shift, y >> shift);
        (
            (MAX_SOURCE_ZOOM, sx, sy),
            Self {
                factor: factor as u32,
                x: (x - sx * factor) as u32,
                y: (y - sy * factor) as u32,
            },
        )
    }
}

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
pub mod palette {
    /// Land nothing more is known about — and, at globe zooms, all land. A dry
    /// khaki rather than paper white, so the planet reads as ground.
    pub const PAPER: u32 = 0xA0_9A_80;
    /// Water as the raster basemap paints it — and, more to the point, what the
    /// *shaded* water of `systems::water` looks like from far enough that its
    /// waves have merged, the same contract [`FOREST_CANOPIED`] has with the
    /// trees.
    ///
    /// The vector surfaces stream in a 3×3 cell ring around the camera
    /// (`map_geometry::CELL_RADIUS`), so there is always a hard edge where the
    /// geometry stops and this paint takes over. That edge is only invisible if
    /// the two agree. They used to disagree by almost the whole value range —
    /// pale `#A8C4DA` paint meeting water shaded from a near-black albedo — and
    /// it read as a bright cut straight across every lake.
    ///
    /// **Both sides of that comparison have to be *rendered* ones, and until
    /// 2026-08-20 only one of them was.** The value was picked to land on the
    /// water surface's shaded tone (`#36495C`–`#415462`, sampled from a render) —
    /// but this is an albedo as well, painted into a tile texture that is then lit
    /// by the same sun on a `perceptual_roughness: 1.0`, `reflectance: 0.0`
    /// ground quad. So the paint got the sunlight *too*, and arrived on screen
    /// well above the tone it had been matched to. The step came back, quietly,
    /// as a brighter sea outside the vector ring than inside it — which is what a
    /// square of darker water in the middle of the screen is, and it is visible on
    /// a phone at any oblique angle because the ring's far edge crosses the view.
    ///
    /// Measured at 53.07,5.34 from 12 km (nadir, 900×1900, `--shot-mode earth`,
    /// same frame so the lighting is common to both): the ring's water rendered
    /// `#425F7F` and the paint beside it `#5A7690` — a 24-level luminance step,
    /// not the "residual blue cast" this comment used to promise. This value is
    /// that measurement carried back through the ground quad's own lighting, so
    /// the two now agree *on screen* rather than one of them agreeing with the
    /// other's albedo.
    ///
    /// So this is not a free colour choice: move `water::WATER_ALBEDO` and this
    /// has to move with it — and re-sample **both** tones out of one frame, never
    /// this constant against a render of that one.
    ///
    /// The container's software rasteriser is trustworthy for the *step* (one
    /// frame, one light, two materials) and not for the absolute tone, so the
    /// direction and rough size of this move are measured and the last few levels
    /// want an eye on real hardware.
    ///
    /// It is pulled the other way too, and that one is silent. On a *raster*
    /// basemap the globe has no geometry to ask, so it decides what is ocean by
    /// how much bluer than red a texel is (`tile_loader::WATER_BLUE_BIAS`): this
    /// has to stay at least 40 bluer than it is red or the planet quietly loses
    /// its oceans. Matching the shaded tone wants it greyer, that wants it blue,
    /// and this value is where the two settle — the residual blue cast at the ring
    /// edge is the price. `tile_loader`'s
    /// `the_basemaps_water_paint_reads_as_fully_water` holds the line.
    pub const WATER: u32 = 0x2C_48_62;
    pub const FOREST: u32 = 0x4A_5A_36;
    /// Meadow olive: a muted green at the value of mown grass from the air.
    ///
    /// Measured against PDOK's aerial photograph on 2026-10-01: Groningen's
    /// lawns and parks sample at about (60–95, 87–113, 71–96), and the old
    /// `0x6E8A4C` rendered sunlit at about (110, 125, 82) — a third too bright
    /// and leaning yellow where the photo leans blue-green. A lawn is mostly
    /// blades shading each other, which is why it is darker than the paint chip
    /// says. This one tone feeds the flat fill, the image and the blades
    /// (`systems::grass` derives from it), so they move together.
    pub const GRASS: u32 = 0x52_6E_44;
    /// Canopy tone: one step down the [`FOREST`] hue ramp, at the same muted
    /// saturation — a game green here reads as confetti (see the module note).
    pub const CANOPY: u32 = 0x4A_6A_36;
    /// Second canopy tone, alternated per instance. Two tones buy depth without
    /// costing a light.
    pub const CANOPY_DEEP: u32 = 0x3C_5A_2C;
    /// Trunks are a warm grey-brown kept *below* the canopy's value: a trunk
    /// brighter than the leaves over it is the inversion dc01bc5 describes, and
    /// a saturated brown at walking distance is the loudest colour on the map.
    pub const TRUNK: u32 = 0x5A_4E_40;
    /// What a forest polygon looks like from far enough that the individual trees
    /// have merged: [`FOREST`] ground seen through ~65% [`CANOPY`] cover —
    /// `mix(FOREST, CANOPY, 0.65)`, computed, not matched by eye. The fill ramps
    /// to this as the trees fade out, so the aggregate brightness does not step
    /// when the geometry goes away.
    pub const FOREST_CANOPIED: u32 = 0x4A_64_36;
    pub const SAND: u32 = 0xD6_C3_9A;
    /// Glaciers and ice caps, from the land cover — a cool white, the one
    /// near-white left on the land, because from orbit it is the one that is.
    pub const ICE: u32 = 0xEE_F1_F4;
    /// A tunnel's walls and roof. Darker than any surface in this palette on
    /// purpose: it is the one thing here that is never lit by the sun, and a
    /// concrete the tone of a pavement reads as a white box with a hole at each
    /// end. The sky's ambient still reaches the open approach, so the opening
    /// stays lighter than the tunnel without anything modelling it.
    pub const TUNNEL: u32 = 0x6E_6B_66;
    /// A quay wall: weathered brick and stone above a canal, darker than the
    /// street on top of it so the edge reads from across the water.
    pub const QUAY_WALL: u32 = 0x7A_6A_5C;
    pub const RESIDENTIAL: u32 = 0xAB_A6_9C;
    pub const INDUSTRIAL: u32 = 0xA6_A4_A0;
    /// Built-up land as read from altitude. Up close, [`RESIDENTIAL`] and
    /// [`INDUSTRIAL`] sit within a few counts of [`GRASS`] on purpose — streets
    /// and buildings carry the figure there — but from ~z12 out those are
    /// sub-pixel, the land layer is all that is left, and a whole province
    /// rendered as one near-uniform green sheet with no visible cities. These are
    /// the same land uses a hue step warmer and darker, used by the far style
    /// band (`FAR_STYLE_TILE_M`) so a town reads as a town from the air.
    pub const RESIDENTIAL_FAR: u32 = 0x92_8B_80;
    pub const INDUSTRIAL_FAR: u32 = 0x8E_8C_88;
    pub const SITE: u32 = 0xAD_A9_9E;
    pub const BUILDING: u32 = 0x8F_85_7C;
    pub const BUILDING_EDGE: u32 = 0x7A_70_68;
    pub const ROAD_FILL: u32 = 0xFC_FB_F8;
    pub const ROAD_CASING: u32 = 0xCB_C4_B6;
    pub const MOTORWAY_FILL: u32 = 0xF4_DF_B4;
    pub const MOTORWAY_CASING: u32 = 0xD8_BC_86;
    pub const RAIL: u32 = 0xB4_AE_A4;
    /// A bridge deck seen as a structure rather than as a stretch of road: the
    /// concrete-and-asphalt grey of the whole thing, parapets included.
    pub const BRIDGE_DECK: u32 = 0x57_56_59;
    pub const PATH: u32 = 0xC5_C0_B0;
    pub const BOUNDARY: u32 = 0xB0_A6_B4;
    /// Base tone of the street furniture group (`systems::street_furniture`). The
    /// per-part tones below are carried as vertex-colour *multipliers* against
    /// this, exactly as the canopy's second tone is, so one material paints a whole
    /// street's benches, bins and shelters.
    pub const FURNITURE: u32 = 0xB8_B0_A2;
    /// Timber: bench slats, table tops, swing seats. In the [`TRUNK`] family and
    /// muted for the same reason — a saturated brown at walking distance is the
    /// loudest colour on the map.
    pub const FURNITURE_WOOD: u32 = 0xB4_A9_98;
    /// Painted steel: posts, frames, bins.
    pub const FURNITURE_METAL: u32 = 0x9A_98_93;
    /// Roof and sign panels — a step *lighter* than the posts under them, so a
    /// shelter still reads as a shelter from directly overhead, which is where the
    /// orbit camera spends most of its time.
    pub const FURNITURE_ROOF: u32 = 0xC6_C1_B6;
    /// Masonry: a fountain rim, an artwork plinth.
    pub const FURNITURE_STONE: u32 = 0xC0_BA_AE;
    /// Glazing: a car's windows. Dark enough to read as a cabin against any body
    /// colour, and grey rather than blue — a tinted windscreen on a pale map is
    /// the same mistake a saturated bench is.
    pub const VEHICLE_GLASS: u32 = 0x6E_74_79;
    /// Rubber: tyres. The darkest tone in the group, and the one thing that makes
    /// a box on the ground read as a vehicle rather than as a crate.
    pub const VEHICLE_TYRE: u32 = 0x4A_47_45;
}

/// How one source layer is painted.
#[derive(Debug, Clone, Copy)]
enum Draw {
    /// Fill polygons in a single colour.
    Fill(u32),
    /// Fill polygons, colour chosen by the feature's `kind` property.
    FillByKind {
        default: u32,
        kinds: &'static [(&'static str, u32)],
    },
    /// Stroke lines at a fixed width in pixels (at the source tile's own scale).
    Line { color: u32, width: f32 },
    /// Outline polygons.
    Outline { color: u32, width: f32 },
    /// Stroke the `streets` layer, width and colour chosen per road class. `casing`
    /// draws the wider dark pass that goes underneath the fill pass.
    Streets { casing: bool },
}

/// One entry per painted layer, **bottom first**. This table is the map's design;
/// the label layers (`*_labels`, `addresses`, `pois`) are deliberately absent —
/// see the module docs on why text waits for a glyph pipeline.
/// The land table: what colour each `land` kind is painted, **for both land
/// sources** — OSM's `land` layer and the land cover under it
/// ([`landcover_rgb`]). One table so a forest is one green whichever of the two
/// drew it, and the hand-over between them (z8–z10) is invisible.
///
/// Kinds are grouped the way `land_group` groups them for the meshed ground up
/// close — scrub, heath and marsh are grass there, bare rock is sand — so this
/// image and those meshes agree at the ring edge too. That grouping is also why
/// `bare_rock` and `scree` are here: the mesh drew them as sand while this image
/// fell through to the grass default.
const LAND_KINDS: &[(&str, u32)] = &[
    ("forest", palette::FOREST),
    ("wood", palette::FOREST),
    ("grass", palette::GRASS),
    ("grassland", palette::GRASS),
    ("meadow", palette::GRASS),
    ("park", palette::GRASS),
    ("garden", palette::GRASS),
    ("farmland", palette::GRASS),
    ("scrub", palette::GRASS),
    ("heath", palette::GRASS),
    ("marsh", palette::GRASS),
    ("sand", palette::SAND),
    ("beach", palette::SAND),
    ("bare_rock", palette::SAND),
    ("scree", palette::SAND),
    ("shingle", palette::SAND),
    ("glacier", palette::ICE),
    ("residential", palette::RESIDENTIAL),
    ("industrial", palette::INDUSTRIAL),
    ("commercial", palette::INDUSTRIAL),
];

const PAINT_ORDER: &[(&str, Draw)] = &[
    // **`sites` under `land`, as the meshes stack them.** A site is built ground
    // (`SurfaceGroup::Built`, the bottom of `GroundLayer`) and a park or lawn
    // inside it is grass, one rung up — so up close the Martinikerkhof was a lawn
    // and from 500 m, where this image takes over, it was the site's beige
    // (2026-10-01). Painted first, the grass covers it here too.
    ("sites", Draw::Fill(palette::SITE)),
    (
        "land",
        Draw::FillByKind {
            default: palette::GRASS,
            kinds: LAND_KINDS,
        },
    ),
    ("ocean", Draw::Fill(palette::WATER)),
    ("water_polygons", Draw::Fill(palette::WATER)),
    ("dam_polygons", Draw::Fill(palette::BUILDING)),
    ("pier_polygons", Draw::Fill(palette::PAPER)),
    (
        "water_lines",
        Draw::Line {
            color: palette::WATER,
            width: 2.0,
        },
    ),
    ("street_polygons", Draw::Fill(palette::ROAD_FILL)),
    ("streets", Draw::Streets { casing: true }),
    ("streets", Draw::Streets { casing: false }),
    (
        "bridges",
        Draw::Line {
            color: palette::ROAD_CASING,
            width: 1.5,
        },
    ),
    ("buildings", Draw::Fill(palette::BUILDING)),
    (
        "buildings",
        Draw::Outline {
            color: palette::BUILDING_EDGE,
            width: 0.6,
        },
    ),
    (
        "boundaries",
        Draw::Line {
            color: palette::BOUNDARY,
            width: 1.2,
        },
    ),
];

/// A street's raster style: (width in px at the source tile's own scale, fill
/// colour, casing colour). The px width is multiplied by the overzoom factor
/// downstream, so a street keeps its real-world width as you magnify past z14
/// instead of thinning to a hair.
///
/// The width is the **real carriageway width** ([`road_width_m`], the same
/// number the 3D ribbons are built from) converted to source-tile pixels, with a
/// per-class *floor* in pixels. At z14 the real width dominates and the painted
/// road agrees with the textured ribbon drawn on the road plane — the old fixed px
/// widths were 1–2 m wider per side, which put a pale halo around every dark
/// ribbon and pale round blobs at every junction corner. At z10 the real width
/// is far below a pixel and the floor takes over, which is what keeps the road
/// hierarchy legible on a zoomed-out map. One `max()`, two regimes, no band
/// switch to pop at.
fn street_style(feature: &Feature, source_m: f32) -> (f32, u32, u32) {
    let kind = kind_of(feature).unwrap_or("");
    let (floor_px, fill, case) = match kind {
        "motorway" | "trunk" => (3.5, palette::MOTORWAY_FILL, palette::MOTORWAY_CASING),
        "primary" => (3.0, palette::MOTORWAY_FILL, palette::MOTORWAY_CASING),
        "secondary" => (2.5, palette::ROAD_FILL, palette::ROAD_CASING),
        "tertiary" => (2.2, palette::ROAD_FILL, palette::ROAD_CASING),
        "residential" | "unclassified" | "living_street" => {
            (1.8, palette::ROAD_FILL, palette::ROAD_CASING)
        }
        "service" => (1.0, palette::ROAD_FILL, palette::ROAD_CASING),
        "rail" | "light_rail" | "subway" | "tram" => (1.2, palette::RAIL, palette::RAIL),
        "pedestrian" | "footway" | "path" | "track" | "cycleway" | "steps" => {
            (0.8, palette::PATH, palette::PATH)
        }
        _ => (1.5, palette::ROAD_FILL, palette::ROAD_CASING),
    };
    let flags = StreetFlags {
        oneway: flag_of(feature, "oneway") || flag_of(feature, "oneway_reverse"),
        oneway_reverse: flag_of(feature, "oneway_reverse"),
        link: flag_of(feature, "link"),
    };
    // The fill is the mesh's own tarmac/grit tone wherever the mesh path would
    // draw this kind: the image replaces the road meshes above 500 m
    // (`map_geometry`), and a pale cartographic fill there left the roads
    // indistinguishable from the paper around them.
    //
    // A line that was one tone (paths, rail) stays one tone, casing included. A
    // path's fill is under a pixel wide at z14, which tiny-skia draws as a
    // hairline whose placement moves by a pixel with the raster band's offset;
    // inside a casing of another colour that jog shows at every band seam
    // (`banding_the_raster_changes_no_geometry`).
    let one_tone = fill == case;
    let fill = road_class(kind, flags).map_or(fill, RoadClass::flat_rgb);
    let case = if one_tone { fill } else { case };
    let real_px = road_width_m(kind, flags) * TILE_PX as f32 / source_m.max(1.0);
    (real_px.max(floor_px), fill, case)
}

/// The OSM `layer` value of a feature (Shortbread carries it on `streets`), or 0.
/// Vertical order for the raster paint: a flyover strokes over what it crosses.
pub(crate) fn layer_of(feature: &Feature) -> i64 {
    match feature.properties.as_ref().and_then(|p| p.get("layer")) {
        Some(Value::Int(v) | Value::SInt(v)) => *v,
        Some(Value::UInt(v)) => *v as i64,
        Some(Value::Float(v)) => *v as i64,
        Some(Value::Double(v)) => *v as i64,
        _ => 0,
    }
}

fn shader(rgb: u32) -> Shader<'static> {
    Shader::SolidColor(tiny_skia::Color::from_rgba8(
        ((rgb >> 16) & 0xFF) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
        255,
    ))
}

/// Read a feature's `kind` (Shortbread's discriminator for almost every layer).
pub(crate) fn kind_of(feature: &Feature) -> Option<&str> {
    match feature.properties.as_ref()?.get("kind")? {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Ground edge, in metres, of a z14 tile at Dutch latitudes — the nominal
/// `source_m` for a caller (or test) rendering without knowing its tile's real
/// ground size. Street widths drawn from this are exact at z14 over the
/// Netherlands and merely approximate elsewhere, which is what they always were.
pub const NOMINAL_Z14_TILE_M: f32 = 1465.0;

/// Displayed ground metres per tile above which the raster switches to the far
/// style: minor streets and paths are dropped and built-up land uses darken
/// (`palette::RESIDENTIAL_FAR`) so a city reads from the air. A z12 tile at
/// Dutch latitudes shows ~5.9 km, z13 ~2.9 km; the threshold sits between them.
const FAR_STYLE_TILE_M: f32 = 5_000.0;

/// Render an MVT tile body to a `TILE_PX × TILE_PX` RGBA8 buffer.
///
/// `sub` selects the magnified sub-rectangle for overzoomed requests; pass
/// [`SubTile::WHOLE`] to draw the source tile as-is.
pub fn render_mvt(tile: &TileData, sub: SubTile) -> Result<Vec<u8>, String> {
    render_mvt_sized(tile, sub, TILE_PX, NOMINAL_Z14_TILE_M)
}

/// [`render_mvt`] at an arbitrary edge length, for a consumer whose tiles are
/// never shown at [`TILE_PX`].
///
/// The globe quadtree is that consumer. Its tiles are capped at
/// `globe::QT_SPLIT_PX` on screen — a leaf is 170–340 px — so a 512 px raster is
/// 2–3× oversampled per axis and costs 4–9× the scan conversion for pixels
/// nothing can resolve. Since the tile also *lives* as a texture for as long as
/// the view holds it, the size is the quadtree's whole memory bill as well as its
/// whole fill latency.
///
/// **Every stroke width is in pixels**, so they are scaled by `size / TILE_PX`:
/// without that, halving the raster doubles the apparent width of every road and
/// casing, since the texture is stretched over the same ground either way.
///
/// `source_m` is the **source tile's** ground edge in metres — what turns the
/// real carriageway widths into pixels ([`street_style`]) and decides the far
/// style band ([`FAR_STYLE_TILE_M`]). A caller that does not know it can pass
/// [`NOMINAL_Z14_TILE_M`].
pub fn render_mvt_sized(
    tile: &TileData,
    sub: SubTile,
    size: u32,
    source_m: f32,
) -> Result<Vec<u8>, String> {
    render_mvt_overlaid(tile, sub, size, source_m, &[])
}

/// A polygon painted into the raster from outside the tile body — the registered
/// crop parcels (`crate::crops`), which are not in any vector tile. Rings are in
/// the **source tile's** unit square (0..1 across, y down), outer first, holes
/// after; one flat colour.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayFill {
    pub rings: Vec<Vec<[f32; 2]>>,
    pub rgb: u32,
}

/// What the survey (the BGT) says about the ground under a basemap tile, so the
/// image paints the ground the meshes draw rather than a cartographic one.
///
/// **The image takes over from the meshes, so it has to be the same picture.**
/// Above `FLAT_GROUND_OFF_M` the client stops meshing the flat ground and this
/// image is all of it; until 2026-10-01 it was painted from the OSM tile alone,
/// so the Grote Markt went from its surveyed brick red to the near-white of
/// OSM's pedestrian fill, and every street from its paving to a tarmac band, at
/// the moment of the hand-over. The colours here come from the same function the
/// meshes are coloured by (`paving::far_rgb`), averaged over the stone pattern the
/// shader draws.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Survey {
    /// The surveyed paving, in the source tile's unit square, each in the colour
    /// it reads as from the air. Painted over OSM's pedestrian areas and under
    /// its streets, which is the order the meshes stack in.
    pub paving: Vec<OverlayFill>,
    /// The cells whose survey replaces OSM's ribbons, and how far
    /// (`road_texture::SurveyedCover`), as the same decision `map_geometry` takes
    /// per cell.
    pub cells: Vec<CoveredCell>,
}

/// One cell's [`SurveyedCover`], with its bounds in the source tile's unit square
/// (`[x0, y0, x1, y1]`, y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoveredCell {
    pub bounds: [f32; 4],
    pub cover: SurveyedCover,
}

impl Survey {
    /// Whether a street of `kind` starting at `p` (unit square) is drawn by the
    /// survey instead — the raster half of `tessellate_roads_surveyed`. A way is
    /// judged by its first vertex, as the meshes judge a cell's ownership.
    #[must_use]
    pub fn supersedes(&self, kind: &str, p: [f32; 2]) -> bool {
        self.cells.iter().any(|c| {
            p[0] >= c.bounds[0]
                && p[0] < c.bounds[2]
                && p[1] >= c.bounds[1]
                && p[1] < c.bounds[3]
                && c.cover.supersedes(kind)
        })
    }
}

/// The empty survey: what every caller that does not know one passes.
pub static NO_SURVEY: Survey = Survey {
    paving: Vec::new(),
    cells: Vec::new(),
};

/// A geometry's first vertex.
fn first_coord(geom: &Geometry<f32>) -> Option<(f32, f32)> {
    let c = match geom {
        Geometry::LineString(ls) => ls.0.first(),
        Geometry::MultiLineString(m) => m.0.first().and_then(|ls| ls.0.first()),
        Geometry::Polygon(p) => p.exterior().0.first(),
        Geometry::MultiPolygon(m) => m.0.first().and_then(|p| p.exterior().0.first()),
        Geometry::Point(p) => Some(&p.0),
        _ => None,
    }?;
    Some((c.x, c.y))
}

/// Deepest zoom the land-cover tile set is published at. A deeper source tile
/// reads its ancestor at this zoom ([`landcover_fills`]).
pub const LANDCOVER_MAX_ZOOM: u32 = 10;

/// The tone a land-cover class is painted in, or `None` for one this map does
/// not draw (open water, which the map's own water layers own).
///
/// The land cover is VersaTiles' `landcover-vectors`, ESA WorldCover 2021
/// classified into Shortbread-style `kind`s. It exists because the OSM tiles
/// carry **no `land` layer below z7–z8**: from orbit every continent was the
/// bare [`palette::PAPER`], and no palette could make the globe look like Earth.
/// It reads the same [`LAND_KINDS`] table as the OSM land, so the two are one
/// map style; only built-up land takes the far band's tone, since the land
/// cover is only ever the far picture.
#[must_use]
pub fn landcover_rgb(kind: &str) -> Option<u32> {
    match kind {
        "water" => None,
        "residential" => Some(palette::RESIDENTIAL_FAR),
        k => LAND_KINDS
            .iter()
            .find(|(name, _)| *name == k)
            .map(|(_, rgb)| *rgb),
    }
}

/// A land-cover tile's classes as [`OverlayFill`]s in a source tile's unit
/// square, for [`render_mvt_layered`]'s underlay.
///
/// `shift` is how many zooms the source tile sits below the land-cover tile and
/// `(ox, oy)` is its index within it — (0, 0, 0) when they are the same tile.
/// Rings that land wholly off the source tile are dropped, since a z14 tile reads
/// a z10 ancestor and would otherwise paint 256 tiles' worth of polygons to keep
/// one.
#[must_use]
pub fn landcover_fills(tile: &TileData, shift: u32, ox: u32, oy: u32) -> Vec<OverlayFill> {
    let k = (1u32 << shift.min(16)) as f32;
    let mut out = Vec::new();
    for layer_name in ["land", "water_polygons"] {
        let Some(meta) = tile.layer(layer_name) else {
            continue;
        };
        let extent = TileData::extent(meta);
        let to_unit = 1.0 / extent;
        for feature in tile.features(meta) {
            let Some(rgb) = kind_of(feature).and_then(landcover_rgb) else {
                continue;
            };
            let polys: Vec<&geo_types::Polygon<f32>> = match &feature.geometry {
                Geometry::Polygon(p) => vec![p],
                Geometry::MultiPolygon(mp) => mp.iter().collect(),
                _ => continue,
            };
            for poly in polys {
                let rings: Vec<Vec<[f32; 2]>> = clipped_rings(poly, to_unit, extent)
                    .into_iter()
                    .map(|r| {
                        r.into_iter()
                            .map(|p| [p[0] * k - ox as f32, p[1] * k - oy as f32])
                            .collect()
                    })
                    .collect();
                let Some(outer) = rings.first() else {
                    continue;
                };
                let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
                for p in outer {
                    for a in 0..2 {
                        lo[a] = lo[a].min(p[a]);
                        hi[a] = hi[a].max(p[a]);
                    }
                }
                if hi[0] < 0.0 || hi[1] < 0.0 || lo[0] > 1.0 || lo[1] > 1.0 {
                    continue;
                }
                out.push(OverlayFill { rings, rgb });
            }
        }
    }
    out
}

/// [`render_mvt_sized`] with [`OverlayFill`]s painted over the `land` layer and
/// under everything after it — water, sites, streets and buildings — which is
/// where a field sits on the ground stack too.
///
/// The meshed ground below `FLAT_GROUND_ON_M` and this image above it are the
/// same map at two distances; without the overlay a field kept its crop's tone
/// only while the camera was low enough to be drawing meshes, and the whole
/// countryside turned back to one green on the way up.
pub fn render_mvt_overlaid(
    tile: &TileData,
    sub: SubTile,
    size: u32,
    source_m: f32,
    overlay: &[OverlayFill],
) -> Result<Vec<u8>, String> {
    render_mvt_layered(tile, sub, size, source_m, &[], overlay)
}

/// [`render_mvt_overlaid`] with an `underlay` as well: fills painted straight
/// onto the blank paper, under every layer of the tile — the land cover
/// ([`landcover_fills`]), which is what the paper would otherwise show through.
pub fn render_mvt_layered(
    tile: &TileData,
    sub: SubTile,
    size: u32,
    source_m: f32,
    underlay: &[OverlayFill],
    overlay: &[OverlayFill],
) -> Result<Vec<u8>, String> {
    render_mvt_surveyed(tile, sub, size, source_m, underlay, overlay, &NO_SURVEY)
}

/// [`render_mvt_layered`] with the survey's ground painted in ([`Survey`]).
pub fn render_mvt_surveyed(
    tile: &TileData,
    sub: SubTile,
    size: u32,
    source_m: f32,
    underlay: &[OverlayFill],
    overlay: &[OverlayFill],
    survey: &Survey,
) -> Result<Vec<u8>, String> {
    // Cull only when overzooming: at `factor == 1` the window is the whole tile,
    // so every feature passes and the bounds scan is pure overhead.
    render_mvt_banded_surveyed(
        tile,
        sub,
        size,
        source_m,
        sub.factor > 1,
        raster_bands(size),
        underlay,
        overlay,
        survey,
    )
}

/// [`render_mvt_sized`] with the off-screen-feature cull switchable, so a test can
/// assert the two produce identical pixels.
#[cfg(test)]
fn render_mvt_culled(
    tile: &TileData,
    sub: SubTile,
    size: u32,
    source_m: f32,
    cull: bool,
) -> Result<Vec<u8>, String> {
    render_mvt_banded(
        tile,
        sub,
        size,
        source_m,
        cull,
        raster_bands(size),
        &[],
        &[],
    )
}

/// Rows a band renders beyond its own span and then discards.
///
/// Sized by measurement, not by argument: `banding_the_raster_changes_no_geometry`
/// fails at any value too small for the widest stroke that can reach into a kept
/// row, and it fails loudly — a seam every `TILE_PX / bands` rows.
const RASTER_BLEED_PX: u32 = 32;

/// Bands a raster is split into, everywhere, on every machine.
///
/// Gated with the threading it configures: this crate builds `-D unused`, so on
/// a target that renders serially an ungated constant is an error, not a
/// warning.
#[cfg(not(target_arch = "wasm32"))]
const RASTER_BANDS: u32 = 4;

/// Rows of the raster one thread should take.
///
/// One band is the whole tile and the serial path. `None` of this exists on
/// wasm, which has no threads to hand.
fn raster_bands(size: u32) -> u32 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // A band under this is not worth a thread: the paint loop's fixed work
        // — resolving each layer, sorting the streets — is paid per band, so a
        // thin band is mostly overhead.
        const MIN_BAND_ROWS: u32 = 64;
        // **A constant, not the core count.** The bands are not bit-identical to
        // a single pixmap (see `render_rows`), so a band count that varied with
        // the machine would make the basemap itself vary with the machine — and
        // this tree leans on rasters being reproducible.
        let by_rows = (size / MIN_BAND_ROWS).max(1);
        return by_rows.min(RASTER_BANDS).max(1);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = size;
        1
    }
}

/// One-pixmap render, for the instruments.
///
/// `perf::paths` and `perf::passes` are **thread-local** counters, so a banded
/// render records most of its work on threads the measuring code never reads —
/// the counts would silently come back a quarter of the truth. Every budget and
/// every printing hatch that reads those counters goes through here instead, and
/// the wall-clock hatch does not, because what it must time is what ships.
#[cfg(test)]
pub(crate) fn render_mvt_serial(
    tile: &TileData,
    sub: SubTile,
    size: u32,
    source_m: f32,
) -> Result<Vec<u8>, String> {
    render_mvt_banded(tile, sub, size, source_m, sub.factor > 1, 1, &[], &[])
}

/// [`render_mvt_culled`] over a stated number of horizontal bands.
///
/// **Splitting the raster by rows is very nearly exact, and "very nearly" is the
/// honest word.** The first version of this said the split was exact, reasoning
/// that a pixel's anti-aliased coverage comes only from the path inside that
/// pixel. Measuring refuted it twice over. Geometry entering a band from outside
/// is clipped to the pixmap, which decides the boundary row wrongly — 2,443
/// bytes of a 1,048,576 byte raster, all along the seam — and that part is what
/// [`RASTER_BLEED_PX`] removes. What is left after the bleed does not go away
/// with more of it (128 rows still left 96 bytes, 256 left 43): shifting the
/// transform by the band's first row changes the magnitudes the coordinates are
/// computed at, so `f32` rounds a few edge samples differently.
///
/// So a band differs from a single pixmap by at most **8/255 on isolated
/// anti-aliased pixels** — the same figure [`flush_batch`] records for the paint
/// batching, arrived at the same way. `banding_the_raster_changes_no_geometry`
/// bounds that delta, the total differing bytes per seam, and the worst single
/// row, because the failure that matters looks nothing like the residue: a band
/// that lost a road is a long run of one row at full channel range.
///
/// The tests' entry; production goes through [`render_mvt_surveyed`].
#[cfg(test)]
fn render_mvt_banded(
    tile: &TileData,
    sub: SubTile,
    size: u32,
    source_m: f32,
    cull: bool,
    bands: u32,
    underlay: &[OverlayFill],
    overlay: &[OverlayFill],
) -> Result<Vec<u8>, String> {
    render_mvt_banded_surveyed(
        tile, sub, size, source_m, cull, bands, underlay, overlay, &NO_SURVEY,
    )
}

/// [`render_mvt_banded`] with a [`Survey`].
#[allow(clippy::too_many_arguments)]
fn render_mvt_banded_surveyed(
    tile: &TileData,
    sub: SubTile,
    size: u32,
    source_m: f32,
    cull: bool,
    bands: u32,
    underlay: &[OverlayFill],
    overlay: &[OverlayFill],
    survey: &Survey,
) -> Result<Vec<u8>, String> {
    if bands <= 1 {
        return render_rows(
            tile, sub, size, source_m, cull, 0, size, underlay, overlay, survey,
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let rows = size.div_ceil(bands);
        let parts: Vec<Result<Vec<u8>, String>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..bands)
                .map(|b| {
                    let y0 = b * rows;
                    let h = rows.min(size.saturating_sub(y0));
                    s.spawn(move || {
                        if h == 0 {
                            return Ok(Vec::new());
                        }
                        render_rows(
                            tile, sub, size, source_m, cull, y0, h, underlay, overlay, survey,
                        )
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err("raster band panicked".to_string()))
                })
                .collect()
        });
        let mut out = Vec::with_capacity((size * size * 4) as usize);
        for part in parts {
            out.extend(part?);
        }
        return Ok(out);
    }

    #[cfg(target_arch = "wasm32")]
    render_rows(
        tile, sub, size, source_m, cull, 0, size, underlay, overlay, survey,
    )
}

/// `rows` rows of the raster, starting at `y0`. `size` stays the whole tile's
/// pixel size — it is what the transform scales against — and only the pixmap
/// and the vertical shift change.
fn render_rows(
    tile: &TileData,
    sub: SubTile,
    size: u32,
    source_m: f32,
    cull: bool,
    want_y0: u32,
    want_rows: u32,
    underlay: &[OverlayFill],
    overlay: &[OverlayFill],
    survey: &Survey,
) -> Result<Vec<u8>, String> {
    // **Bleed, because a band edge is not neutral.** The first version of this
    // assumed a pixel's anti-aliased coverage depends only on the path inside
    // that pixel, so bands would concatenate exactly. They do not:
    // `banding_the_raster_changes_no_geometry` measured 2,443 bytes of a 1,048,576
    // byte raster differing, all of them along the seam, because tiny-skia clips
    // the geometry to the pixmap and a shape entering from above loses the part
    // that decides the boundary row.
    //
    // So each band renders extra rows it then throws away. The kept rows are
    // interior to the pixmap that produced them, and therefore identical to the
    // single-pixmap render. The bleed has to clear the widest thing that can
    // reach into a row — a motorway's casing at the raster's own scale — and
    // `RASTER_BLEED_PX` is checked by that same test rather than reasoned about.
    let top = RASTER_BLEED_PX.min(want_y0);
    let bottom = RASTER_BLEED_PX.min(size.saturating_sub(want_y0 + want_rows));
    let y0 = want_y0 - top;
    let rows = top + want_rows + bottom;

    let mut pixmap = Pixmap::new(size, rows).ok_or_else(|| "pixmap alloc failed".to_string())?;
    pixmap.fill(tiny_skia::Color::from_rgba8(
        ((palette::PAPER >> 16) & 0xFF) as u8,
        ((palette::PAPER >> 8) & 0xFF) as u8,
        (palette::PAPER & 0xFF) as u8,
        255,
    ));

    // Tile-extent units → pixels, including the overzoom magnification and the
    // shift that brings the requested sub-tile to the origin.
    let f = sub.factor as f32;
    let px = size as f32;
    // Width multiplier: the overzoom magnification, times the raster's own scale
    // against the size the widths below are authored at.
    let wf = f * px / TILE_PX as f32;
    // The displayed region's ground size decides the style band: past
    // `FAR_STYLE_TILE_M` this raster is a map read from the air, not a street's
    // own texture, and it styles for legibility instead of fidelity.
    let far_style = source_m / f > FAR_STYLE_TILE_M;

    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };

    // The overlay goes down as the first thing after `land`, whatever the tile
    // holds: `sites` and `land` open `PAINT_ORDER`, so this is the top of the
    // third pass.
    let overlay_transform = Transform::from_row(
        px * f,
        0.0,
        0.0,
        px * f,
        -(sub.x as f32) * px,
        -(sub.y as f32) * px - y0 as f32,
    );
    let mut overlay_pending = !overlay.is_empty();
    // The land cover goes down first of all, on the bare paper: every layer of
    // the tile paints over it, so it only shows where the map says nothing.
    paint_overlay(&mut pixmap, &mut paint, underlay, &overlay_transform);

    // The survey's paving goes down just under the streets: over OSM's pedestrian
    // areas (which it replaces where it lies) and under the ribbons that stay
    // OSM's — the order `map_geometry` stacks the meshes in.
    let mut paving_pending = !survey.paving.is_empty();
    for (layer_name, draw) in PAINT_ORDER {
        if overlay_pending && !matches!(*layer_name, "sites" | "land") {
            paint_overlay(&mut pixmap, &mut paint, overlay, &overlay_transform);
            overlay_pending = false;
        }
        if paving_pending && *layer_name == "streets" {
            paint_overlay(&mut pixmap, &mut paint, &survey.paving, &overlay_transform);
            paving_pending = false;
        }
        // Timed per pass (not per layer — `streets` and `buildings` are each two
        // passes with very different costs) so `dump_paint_split` attributes the
        // real loop, not a reconstruction of it. See `perf::passes`.
        #[cfg(test)]
        let pass_t0 = std::time::Instant::now();
        #[cfg(test)]
        let pass_label: &'static str = match draw {
            Draw::Streets { casing: true } => "streets (casing)",
            Draw::Streets { casing: false } => "streets (fill)",
            Draw::Outline { .. } => "buildings (outline)",
            _ => layer_name,
        };
        let Some(meta) = tile.layer(layer_name) else {
            continue;
        };
        // Memoised (`TileData::features`), which is what makes the two `streets`
        // passes and the two `buildings` passes one decode each rather than two.
        let features = tile.features(meta);
        if features.is_empty() {
            continue;
        }
        let extent = TileData::extent(meta);
        let scale = px * f / extent;
        let transform = Transform::from_row(
            scale,
            0.0,
            0.0,
            scale,
            -(sub.x as f32) * px,
            // The band's first row is this pixmap's row 0, so everything is
            // lifted by where the band starts. This is the **only** place the
            // split is visible: the scale stays the whole tile's, because that
            // is what the stroke widths and the window below are authored
            // against, and a band that rescaled would draw thinner roads.
            -(sub.y as f32) * px - y0 as f32,
        );

        // The slice of the source tile that lands on the pixmap, in tile-extent
        // units: the transform above puts tile coord `t` at pixel `scale·t −
        // sub·px`, so the visible band is `[sub·extent/f, (sub+1)·extent/f]`.
        let win = (
            sub.x as f32 * extent / f,
            sub.y as f32 * extent / f,
            (sub.x + 1) as f32 * extent / f,
            (sub.y + 1) as f32 * extent / f,
        );

        // The two `streets` passes draw in **attribute order, not tile order**:
        // OSM `layer` first (a flyover paints over what it crosses), bridges
        // above their own layer, wider roads over narrower. Encoder order put a
        // residential fill over a motorway's whenever the tile happened to store
        // it later. Tunnels are dropped outright — the geometry path already
        // refuses them (`drawn_street`), and the raster painting them left a
        // pale band ruled straight across every junction the tunnel passes
        // under, with nothing above it to cover it.
        let street_order: Option<Vec<&Feature>> = matches!(draw, Draw::Streets { .. }).then(|| {
            let mut v: Vec<&Feature> = features
                .iter()
                .filter(|feat| !crate::tunnels::underground(feat))
                .collect();
            v.sort_by(|a, b| {
                layer_of(a)
                    .cmp(&layer_of(b))
                    .then(flag_of(a, "bridge").cmp(&flag_of(b, "bridge")))
                    .then(
                        street_style(a, source_m)
                            .0
                            .total_cmp(&street_style(b, source_m).0),
                    )
            });
            v
        });

        // One batched submission per (colour, width) run instead of one
        // `fill_path`/`stroke_path` per feature. Measured on the Groningen z14
        // fixture (`dump_paint_split`), the per-call cost dominated the paint —
        // the two `buildings` passes alone were 77% of the loop at ~4 µs per
        // call across 8,552 features, most of it tiny-skia's per-call setup
        // (clone + transform the path, build and sort the edge list, raise a
        // blitter) rather than scan conversion. A run only ends where the colour
        // or width changes, so paint order between different styles is
        // untouched; within a run the features share one opaque style, where
        // draw order cannot matter. Batching also renders shared edges
        // watertight — one path accumulates signed coverage analytically, so
        // adjacent buildings no longer show an anti-aliased seam along a party
        // wall, and same-style strokes no longer double-blend where they overlap
        // at a junction.
        let mut batch = PathBuilder::new();
        let mut batch_key: Option<BatchKey> = None;
        let mut batch_points: usize = 0;
        // Where a street runs into a tunnel mouth, in tile units. The tunnel is
        // not painted (above), so a round cap there is a half-disc of road lying
        // on the ground over the lid, just past the mouth — it showed as an
        // orange semicircle at every portal once the ground over a lid was put
        // back at street level (2026-09-23). A street ending there is butt-capped.
        let portals: Vec<(f32, f32)> = if matches!(draw, Draw::Streets { .. }) {
            portal_points(&features)
        } else {
            Vec::new()
        };
        let mut butt_capped: Vec<(BatchKey, &Feature)> =
            Vec::with_capacity(if portals.is_empty() { 0 } else { 64 });
        let near_portal = |geom: &Geometry<f32>| -> bool {
            let tol = extent * PORTAL_CAP_FRAC;
            let hit = |c: Option<&geo_types::Coord<f32>>| {
                c.is_some_and(|c| {
                    portals
                        .iter()
                        .any(|p| (p.0 - c.x).abs() <= tol && (p.1 - c.y).abs() <= tol)
                })
            };
            let ends = |ls: &geo_types::LineString<f32>| hit(ls.0.first()) || hit(ls.0.last());
            match geom {
                Geometry::LineString(ls) => ends(ls),
                Geometry::MultiLineString(mls) => mls.iter().any(ends),
                _ => false,
            }
        };

        let mut ordered: Vec<&Feature> = match street_order {
            Some(list) => list,
            None => features.iter().collect(),
        };
        // **Built land first, then everything that grows on it** — the order the
        // meshes stack in (`GroundLayer::Built` is the bottom rung). In tile
        // order a residential area painted after the park inside it covered the
        // park: the Martinikerkhof was a lawn up close and the residential beige
        // from the air (2026-10-01). Stable, so equal kinds keep the tile's order.
        if *layer_name == "land" {
            ordered.sort_by_key(|f| kind_of(f).and_then(land_group) != Some(SurfaceGroup::Built));
        }
        // A street or pedestrian area the survey draws instead is not painted:
        // its paving is already down, in its own colour.
        let surveyed_layer =
            !survey.cells.is_empty() && matches!(*layer_name, "streets" | "street_polygons");
        for feature in ordered {
            let kind = kind_of(feature);
            if surveyed_layer
                && first_coord(&feature.geometry).is_some_and(|(x, y)| {
                    survey.supersedes(kind.unwrap_or("pedestrian"), [x / extent, y / extent])
                })
            {
                continue;
            }
            // Bounds once per feature, reused by whichever arm draws it. tiny-skia
            // has no early-out of its own: `fill_path` clones and transforms the
            // whole path before any bounds test (its `// TODO: ignore paths
            // outside the pixmap` is still there), and `stroke_path` builds the
            // full stroke outline first. So an off-screen feature costs its
            // share of a path, a transform pass and — for a non-hairline
            // width — a complete round-joined outline, to rasterise nothing.
            let bounds = if cull {
                geometry_bounds(&feature.geometry)
            } else {
                None
            };
            // `half_width_px` is the stroke's reach past its own geometry; the
            // extra pixel covers the anti-aliased edge. A feature with no
            // computable bounds is always drawn.
            let visible = |half_width_px: f32| -> bool {
                let Some((x0, y0, x1, y1)) = bounds else {
                    return true;
                };
                let m = (half_width_px + 1.5) / scale;
                x1 >= win.0 - m && x0 <= win.2 + m && y1 >= win.1 - m && y0 <= win.3 + m
            };
            // What this pass paints the feature with: a colour, and a stroke
            // width for line work (`None` = fill). `None` overall = not drawn.
            let style: Option<(u32, Option<f32>)> = match draw {
                Draw::Fill(rgb) => Some((*rgb, None)),
                Draw::FillByKind { default, kinds } => {
                    let mut rgb = kind
                        .and_then(|k| kinds.iter().find(|(n, _)| *n == k).map(|(_, c)| *c))
                        .unwrap_or(*default);
                    if far_style {
                        rgb = match kind {
                            Some("residential") => palette::RESIDENTIAL_FAR,
                            Some("industrial") | Some("commercial") => palette::INDUSTRIAL_FAR,
                            _ => rgb,
                        };
                    }
                    Some((rgb, None))
                }
                Draw::Line { color, width } => Some((*color, Some(width * wf))),
                Draw::Outline { color, width } => Some((*color, Some(width * wf))),
                Draw::Streets { casing } => {
                    // Past the far threshold the minor network is sub-pixel
                    // noise that greys the whole town; drop it and let the
                    // hierarchy read.
                    if far_style
                        && matches!(
                            kind,
                            Some(
                                "service"
                                    | "track"
                                    | "footway"
                                    | "path"
                                    | "cycleway"
                                    | "steps"
                                    | "pedestrian"
                            )
                        )
                    {
                        None
                    } else {
                        let (w, fill, case) = street_style(feature, source_m);
                        Some(if *casing {
                            (case, Some((w + 0.5) * wf))
                        } else {
                            (fill, Some(w * wf))
                        })
                    }
                }
            };
            let Some((rgb, width)) = style else {
                continue;
            };
            if width.is_some_and(|w| w <= 0.0) {
                continue;
            }
            let butt = width.is_some() && !portals.is_empty() && near_portal(&feature.geometry);
            let key = (rgb, width.map(f32::to_bits), butt);
            // Painted after the pass rather than where it falls: a run broken
            // three ways at every street touching a tunnel end (building
            // passages included) cost the paint a quarter more allocations.
            if butt {
                butt_capped.push((key, feature));
                continue;
            }
            let cap = if width.is_some() {
                STROKE_BATCH_POINTS
            } else {
                FILL_BATCH_POINTS
            };
            if batch_key != Some(key) || batch_points >= cap {
                flush_batch(&mut pixmap, &mut paint, &mut batch, batch_key, &transform);
                batch_key = Some(key);
                batch_points = 0;
            }
            let points = if visible(width.unwrap_or(0.0) * 0.5) {
                let points = match width {
                    None => append_area(&mut batch, &feature.geometry),
                    Some(_) => append_lines(&mut batch, &feature.geometry),
                };
                // Still counted per *feature*, not per flush: `perf::paths`
                // exists to measure the geometry submitted (what the
                // overzoom-cull budget asserts on), and batching changes how it
                // is submitted, not how much.
                #[cfg(test)]
                if points > 0 {
                    crate::perf::paths::record(points);
                }
                points
            } else {
                // Culled — but the run must still advance exactly as if it had
                // drawn, or the chunk boundaries (and with them the
                // anti-aliased overlap blends inside each chunk) would depend
                // on what was culled, and
                // `culling_off_screen_features_changes_no_pixels` holds this
                // path to byte-exact output. Counting coordinates costs a
                // fraction of the path build, transform pass and stroke
                // outline the cull exists to skip. A ring whose bounds miss
                // the window cannot contribute winding inside it, so the
                // missing subpaths cannot change a drawn pixel.
                count_points(&feature.geometry, width.is_none())
            };
            batch_points += points;
        }
        flush_batch(&mut pixmap, &mut paint, &mut batch, batch_key, &transform);
        // Grouped by style so each gets one submission; within one style the
        // order cannot matter, as for any batch. Unstable, so it allocates nothing.
        butt_capped.sort_unstable_by_key(|(k, _)| *k);
        let mut last: Option<BatchKey> = None;
        for (key, feature) in butt_capped {
            if last != Some(key) {
                flush_batch(&mut pixmap, &mut paint, &mut batch, last, &transform);
                last = Some(key);
            }
            append_lines(&mut batch, &feature.geometry);
        }
        flush_batch(&mut pixmap, &mut paint, &mut batch, last, &transform);
        #[cfg(test)]
        crate::perf::passes::record(pass_label, pass_t0.elapsed());
    }
    if overlay_pending {
        paint_overlay(&mut pixmap, &mut paint, overlay, &overlay_transform);
    }

    // Fully opaque throughout (opaque background, opaque paints), so tiny-skia's
    // premultiplied buffer is already straight RGBA.
    // Hand back only the rows the caller asked for; the bleed above and below
    // has done its job by existing.
    let stride = (size * 4) as usize;
    let data = pixmap.take();
    let from = top as usize * stride;
    let to = from + want_rows as usize * stride;
    Ok(data[from..to].to_vec())
}

/// Fill each [`OverlayFill`] in its own colour. `transform` maps the source
/// tile's unit square to pixels.
///
/// **One even-odd path per colour, not per polygon.** Both callers hand over a
/// partition — crop parcels, and land-cover classes, which are cut from one
/// classified raster — so no two fills of a colour overlap, and even-odd within
/// the merged path still opens every hole. Per polygon it was a `fill_path` for
/// each of the ~1,000 land-cover polygons in a tile, and the orbit smoke stopped
/// settling inside its 60 s: the basemap drained at about one tile a second.
/// Colours are painted in the order they first appear, which keeps a
/// deterministic raster.
fn paint_overlay(
    pixmap: &mut Pixmap,
    paint: &mut Paint<'static>,
    overlay: &[OverlayFill],
    transform: &Transform,
) {
    let mut batches: Vec<(u32, PathBuilder)> = Vec::new();
    for fill in overlay {
        let at = match batches.iter().position(|(rgb, _)| *rgb == fill.rgb) {
            Some(i) => i,
            None => {
                batches.push((fill.rgb, PathBuilder::new()));
                batches.len() - 1
            }
        };
        let pb = &mut batches[at].1;
        for ring in &fill.rings {
            if ring.len() < 3 {
                continue;
            }
            let mut pts = ring.iter();
            let Some(first) = pts.next() else {
                continue;
            };
            pb.move_to(first[0], first[1]);
            for p in pts {
                pb.line_to(p[0], p[1]);
            }
            pb.close();
        }
    }
    for (rgb, pb) in batches {
        let Some(path) = pb.finish() else {
            continue;
        };
        paint.shader = shader(rgb);
        pixmap.fill_path(&path, paint, FillRule::EvenOdd, *transform, None);
    }
}

/// Axis-aligned bounds `(min_x, min_y, max_x, max_y)` of a geometry in tile-extent
/// units, or `None` when it holds no finite coordinate or is a kind the painter
/// does not draw — both of which mean "do not cull this".
///
/// Polygon interiors are skipped on purpose: a hole lies inside its exterior ring,
/// so it cannot widen the bounds.
fn geometry_bounds(geom: &Geometry<f32>) -> Option<(f32, f32, f32, f32)> {
    let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut any = false;
    let mut push = |coords: &geo_types::LineString<f32>| {
        for c in coords.coords() {
            if c.x.is_finite() && c.y.is_finite() {
                any = true;
                b.0 = b.0.min(c.x);
                b.1 = b.1.min(c.y);
                b.2 = b.2.max(c.x);
                b.3 = b.3.max(c.y);
            }
        }
    };
    match geom {
        Geometry::LineString(ls) => push(ls),
        Geometry::MultiLineString(mls) => mls.iter().for_each(&mut push),
        Geometry::Polygon(p) => push(p.exterior()),
        Geometry::MultiPolygon(mp) => mp.iter().for_each(|p| push(p.exterior())),
        _ => return None,
    }
    any.then_some(b)
}

/// Append a ring/line to a path builder. Returns false if it was too short to draw.
/// A paint-loop run's identity: colour, plus the stroke width's bit pattern for
/// line work (`None` = fill). Bits rather than the float so the key is `Eq`.
/// The flag asks for butt caps: a street ending at a tunnel mouth.
type BatchKey = (u32, Option<u32>, bool);

/// How close to a tunnel's end a street's end must be to be the same node, as a
/// fraction of the tile extent — a metre and a half on a z14 tile.
const PORTAL_CAP_FRAC: f32 = 0.001;

/// Every end of every tunnel-flagged **carriageway**, in tile units.
///
/// Not the paths: most flagged paths are passages through a building, where a
/// round end lies under the building anyway, and giving each of them a batch of
/// its own cost the paint a fifth more allocations for nothing anybody sees.
fn portal_points(features: &[Feature]) -> Vec<(f32, f32)> {
    let carriageway = |f: &&Feature| {
        crate::tunnels::underground(f)
            && !matches!(
                kind_of(f),
                None | Some(
                    "footway"
                        | "path"
                        | "cycleway"
                        | "steps"
                        | "pedestrian"
                        | "service"
                        | "track"
                        | "bridleway"
                )
            )
    };
    let tunnels = features
        .iter()
        .filter(carriageway)
        .map(|f| match &f.geometry {
            Geometry::MultiLineString(mls) => mls.0.len(),
            _ => 1,
        })
        .sum::<usize>();
    if tunnels == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(tunnels * 2);
    for f in features.iter().filter(carriageway) {
        let mut push = |ls: &geo_types::LineString<f32>| {
            for c in [ls.0.first(), ls.0.last()].into_iter().flatten() {
                if c.x.is_finite() && c.y.is_finite() {
                    out.push((c.x, c.y));
                }
            }
        };
        match &f.geometry {
            Geometry::LineString(ls) => push(ls),
            Geometry::MultiLineString(mls) => mls.iter().for_each(&mut push),
            _ => {}
        }
    }
    out
}

/// Cap on points per batched fill submission. Unbounded fill batches lose what
/// they won: one path holding every building makes tiny-skia's active-edge
/// table span the whole city on every scanline, and the batched fill measured
/// *slower* than per-feature calls. Chunks this size keep the per-call setup
/// amortised while the edge working set stays local (encoder order is roughly
/// spatial).
const FILL_BATCH_POINTS: usize = 4096;

/// Strokes are not capped: a stroked batch rasterises its outline pieces as it
/// goes rather than accumulating one giant edge list, and the measured cost
/// only fell as stroke batches grew.
const STROKE_BATCH_POINTS: usize = usize::MAX;

/// Submit an accumulated run to the rasteriser and reset the builder. A `None`
/// key (nothing accumulated yet) and an empty builder are both no-ops.
fn flush_batch(
    pixmap: &mut Pixmap,
    paint: &mut Paint<'static>,
    pb: &mut PathBuilder,
    key: Option<BatchKey>,
    t: &Transform,
) {
    let Some((rgb, width_bits, butt)) = key else {
        return;
    };
    let Some(path) = std::mem::replace(pb, PathBuilder::new()).finish() else {
        return;
    };
    paint.shader = shader(rgb);
    #[cfg(test)]
    let t0 = std::time::Instant::now();
    match width_bits {
        None => {
            // Winding, not EvenOdd: a batch holds many features, and where two
            // same-colour polygons overlap (Shortbread's `land` does this)
            // EvenOdd XORs the overlap back to background — a pale wedge in a
            // park. Winding unions them, and still cuts holes because MVT
            // mandates interior rings wound opposite to their exterior
            // (verified against the fixture: courtyards render, and the batched
            // raster's max channel delta vs the per-feature one is 8/255,
            // all of it anti-aliased edges).
            pixmap.fill_path(&path, paint, FillRule::Winding, *t, None);
            #[cfg(test)]
            crate::perf::passes::record("Σ fill_path", t0.elapsed());
        }
        Some(bits) => {
            let stroke = Stroke {
                // The batch carries its width in pixels, and tiny-skia reads a
                // stroke width in *path* units and maps it through `t` with the
                // geometry — tile-extent units here, 4096 to a tile. Passed
                // through unscaled, every line came out `px · f / extent` of its
                // width: right only at an 8× overzoom of a 512 px raster, a
                // quarter-pixel hairline for a street in a whole z14 tile, and
                // so the roads all but vanished from the basemap once
                // `map_geometry` stopped drawing their meshes above 500 m. The
                // transform is axis-aligned and uniform, so `sx` is the scale.
                width: f32::from_bits(bits) / t.sx,
                line_cap: if butt { LineCap::Butt } else { LineCap::Round },
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, paint, &stroke, *t, None);
            #[cfg(test)]
            crate::perf::passes::record("Σ stroke_path", t0.elapsed());
        }
    }
}

/// Push one ring/line, skipping non-finite coordinates. Returns how many points
/// were pushed — 0 means the geometry contributed nothing.
fn push_line(pb: &mut PathBuilder, coords: impl Iterator<Item = (f32, f32)>, close: bool) -> usize {
    let mut points = 0;
    for (x, y) in coords {
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        if points > 0 {
            pb.line_to(x, y);
        } else {
            pb.move_to(x, y);
        }
        points += 1;
    }
    if points > 0 && close {
        pb.close();
    }
    points
}

/// Append a geometry's *areas* (polygon rings) to `pb`; returns points appended.
fn append_area(pb: &mut PathBuilder, geom: &Geometry<f32>) -> usize {
    let mut points = 0;
    let mut add_poly = |p: &geo_types::Polygon<f32>, pb: &mut PathBuilder| {
        points += push_line(pb, p.exterior().coords().map(|c| (c.x, c.y)), true);
        for hole in p.interiors() {
            points += push_line(pb, hole.coords().map(|c| (c.x, c.y)), true);
        }
    };
    match geom {
        Geometry::Polygon(p) => add_poly(p, pb),
        Geometry::MultiPolygon(mp) => {
            for p in mp {
                add_poly(p, pb);
            }
        }
        _ => {}
    }
    points
}

/// Append a geometry's *lines* to `pb` — polygon rings count, so the same helper
/// serves `Draw::Outline`. Returns points appended.
fn append_lines(pb: &mut PathBuilder, geom: &Geometry<f32>) -> usize {
    match geom {
        Geometry::LineString(ls) => push_line(pb, ls.coords().map(|c| (c.x, c.y)), false),
        Geometry::MultiLineString(mls) => mls
            .iter()
            .map(|ls| push_line(pb, ls.coords().map(|c| (c.x, c.y)), false))
            .sum(),
        Geometry::Polygon(_) | Geometry::MultiPolygon(_) => append_area(pb, geom),
        _ => 0,
    }
}

/// Exactly the point count [`append_area`] (`area`) or [`append_lines`]
/// (`!area`) would push for this geometry, without building anything. Must stay
/// in lockstep with them: a culled feature advances the batch by this number so
/// chunking does not depend on the cull.
fn count_points(geom: &Geometry<f32>, area: bool) -> usize {
    let finite = |ls: &geo_types::LineString<f32>| -> usize {
        ls.coords()
            .filter(|c| c.x.is_finite() && c.y.is_finite())
            .count()
    };
    let poly = |p: &geo_types::Polygon<f32>| -> usize {
        finite(p.exterior()) + p.interiors().iter().map(finite).sum::<usize>()
    };
    match geom {
        Geometry::Polygon(p) => poly(p),
        Geometry::MultiPolygon(mp) => mp.iter().map(poly).sum(),
        Geometry::LineString(ls) if !area => finite(ls),
        Geometry::MultiLineString(mls) if !area => mls.iter().map(finite).sum(),
        _ => 0,
    }
}

/// Build a path covering a geometry's *areas* (polygon rings), or `None`.
fn area_path(geom: &Geometry<f32>) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    if append_area(&mut pb, geom) == 0 {
        return None;
    }
    pb.finish()
}

/// Returns whether anything was actually filled — i.e. whether the geometry had
/// a fillable area at all. Callers that only want to *know* that (the shore
/// field, deciding whether a tile contains water) would otherwise have to call
/// `area_path` themselves and throw the path away, building every ring twice.
fn fill_geometry(pixmap: &mut Pixmap, paint: &Paint, geom: &Geometry<f32>, t: &Transform) -> bool {
    if let Some(path) = area_path(geom) {
        // Counted like the paint loop's appends: scan conversion barely
        // allocates, so an allocation budget is blind to it; the geometry
        // submitted is what its cost scales with. See `perf::paths`.
        #[cfg(test)]
        crate::perf::paths::record(path.points().len());
        #[cfg(test)]
        let t0 = std::time::Instant::now();
        pixmap.fill_path(&path, paint, FillRule::EvenOdd, *t, None);
        #[cfg(test)]
        crate::perf::passes::record("Σ fill_path", t0.elapsed());
        return true;
    }
    false
}

/// Which surface a tessellated polygon belongs to. One group = one mesh = one
/// material per tile, which is the whole reason this enum is coarse: batching by
/// group is what keeps the geometry path from spraying a mesh per feature at the
/// GPU (mass mesh churn is what loses the device — see the CLAUDE.md notes on
/// `VK_ERROR_DEVICE_LOST`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SurfaceGroup {
    Water,
    Forest,
    Grass,
    Sand,
    Built,
    /// A road ribbon. Textured rather than flat-coloured — see
    /// [`road_texture`](crate::road_texture).
    Road(RoadClass),
    /// The pale kerb band under a [`Self::Road`]: the same centre line extruded
    /// [`ROAD_CASING_M`] wider, flat-coloured, half a depth rung below its own
    /// fill so it shows only as an edge. The raster basemap has always cased its
    /// streets; the geometry path drew bare tarmac, so flying *down* into the
    /// vector ring removed cartographic structure instead of adding it — dark
    /// ribbons lying edgeless on the ground.
    RoadCasing(RoadClass),
    /// A bridge deck: the structure's own surface, standing at its measured
    /// height (`systems::height`) rather than lying in the coplanar ground stack.
    /// Above everything else on the ground, because that is what a bridge is.
    BridgeDeck,
    /// Merged tree canopies (`systems::vegetation`). Per-instance tone and luma
    /// variation ride in the mesh's vertex colours, so this stays **one** group,
    /// one material and one mesh per cell like every other group here.
    Canopy,
    /// Merged tree trunks.
    Trunk,
    /// Merged street furniture (`systems::street_furniture`) — benches, bins,
    /// shelters. Same contract as [`Self::Canopy`]: one group, one material, one
    /// mesh per cell, with the per-object variety carried in vertex colours.
    Furniture,
    /// Surveyed paving beside the carriageway (`crate::paving`) — pavements,
    /// cycle paths, parking bays, verges. Same one-group contract again: the
    /// form's tone rides in the vertex colours.
    Paving,
    /// A registered agricultural field, drawn in the tone of the crop grown on it
    /// (`crate::crops`). Same one-group contract: the crop's tone is per vertex.
    ///
    /// It lies *over* [`Self::Grass`] rather than replacing it — the countryside
    /// keeps its generic green everywhere the register does not reach, which is
    /// every country but this one.
    Crop,
    /// A tunnel's carriageway, under the ground rather than on it
    /// ([`crate::tunnels`]). Its own group and not [`Self::Road`], because the
    /// ground road sheet is a *coplanar* stack settled by
    /// `systems::road_depth`'s rung ladder and this sheet is seven metres below
    /// it: putting the two in one mesh would ask that ladder to arbitrate a
    /// contest that does not exist, and would hand `road_stitch` a ribbon it
    /// could cut a surface street out of.
    TunnelRoad(RoadClass),
    /// A tunnel's walls and roof. One group for both: they are the same concrete
    /// and the same material, and splitting them would double the draw calls of
    /// every tunnel to describe a difference nobody can see.
    TunnelStructure,
    /// The retaining wall between a canal's surface and the street beside it,
    /// built by the client from the water it has placed (`map_geometry::quay_walls`)
    /// rather than tessellated from the tile.
    QuayWall,
}

impl SurfaceGroup {
    /// Base colour for the group's material. Lit by the scene's own sun, unlike the
    /// raster path's baked-in shading — this is the point of ADR-035 option B.
    #[must_use]
    pub fn base_color(self) -> [f32; 3] {
        let rgb = match self {
            // Water's colour when the shading toggle is *off*: `map_geometry`
            // builds a `WaterMaterial` only for shaded water, and falls back to
            // this plain branch otherwise. `palette::WATER` is *tuned* to the
            // tone `water::WATER_ALBEDO` shades to, so the two paths land on
            // the same colour and toggling the shading does not step the sea.
            Self::Water => palette::WATER,
            Self::Forest => palette::FOREST,
            Self::Grass => palette::GRASS,
            Self::Sand => palette::SAND,
            Self::Built => palette::RESIDENTIAL,
            // Roads carry their surface in a texture; tinting it would only fight
            // the paint values chosen there, so the material multiplies by white.
            Self::Road(_) => 0xFF_FF_FF,
            // White for the same reason `Canopy`, `Trunk` and `Furniture` are:
            // one material for the whole layer, and the form's own tone carried
            // in the vertex colours (`crate::paving::PavingKind::rgb`).
            Self::Paving => 0xFF_FF_FF,
            // …and the crop's own tone (`crate::crops::rgb`), same contract.
            Self::Crop => 0xFF_FF_FF,
            // A tunnel's carriageway is a carriageway: same texture, same white.
            Self::TunnelRoad(_) => 0xFF_FF_FF,
            Self::TunnelStructure => palette::TUNNEL,
            Self::QuayWall => palette::QUAY_WALL,
            // Same colours the raster path cases with, so the two pipelines'
            // streets keep matching at the ring edge.
            Self::RoadCasing(RoadClass::Motorway | RoadClass::Carriageway) => {
                palette::MOTORWAY_CASING
            }
            Self::RoadCasing(_) => palette::ROAD_CASING,
            Self::BridgeDeck => palette::BRIDGE_DECK,
            // Vertex colours multiply this, and they are centred on 1.0 — so the
            // palette entry stays the one place the canopy tone is written down.
            Self::Canopy => palette::CANOPY,
            Self::Trunk => palette::TRUNK,
            Self::Furniture => palette::FURNITURE,
        };
        [
            ((rgb >> 16) & 0xFF) as f32 / 255.0,
            ((rgb >> 8) & 0xFF) as f32 / 255.0,
            (rgb & 0xFF) as f32 / 255.0,
        ]
    }

    /// Which rung of [`GroundLayer`] — **the** ground stack — this group draws on.
    ///
    /// One exhaustive `match`, and it is the only thing in the crate that decides
    /// where a surface group stands. Everything else about the ordering — the
    /// depth bias, the millimetre height, the room reserved for the road band — is
    /// derived from the layer, so a group cannot be high in one denomination and
    /// low in another. That disagreement is what every recurring z-fight in this
    /// area turned out to be; `ground_stack`'s module docs have the history.
    ///
    /// Being exhaustive is the load-bearing part: a new `SurfaceGroup` does not
    /// compile until somebody says where in the stack it goes.
    #[must_use]
    pub fn layer(self) -> GroundLayer {
        match self {
            Self::Built => GroundLayer::Built,
            Self::Grass => GroundLayer::Grass,
            Self::Crop => GroundLayer::Crop,
            Self::Forest => GroundLayer::Forest,
            Self::Sand => GroundLayer::Sand,
            Self::Water => GroundLayer::Water,
            Self::Paving => GroundLayer::Paving,
            // Both road groups share the band's foot, and that is the point: the
            // road hierarchy — class, then casing, then width — is `road_rung`,
            // carried per *vertex* inside the band, not a second ladder here. It
            // used to be one, denominated in millimetres, and the two crossed over
            // in the middle distance.
            Self::Road(_) | Self::RoadCasing(_) => GroundLayer::Road,
            Self::BridgeDeck => GroundLayer::BridgeDeck,
            // A tunnel takes the standing rung for the same reason a tree does,
            // and the reason is what the rung means rather than what it is called:
            // these are not sheets in the coplanar stack. Every rung below exists
            // to settle a fight between surfaces lying on one plane, and the one
            // thing a tunnel is never coplanar with is the ground over it — it is
            // metres away, which real depth separates on its own. So it wants no
            // height offset (`is_sheet` is false above `Road`, so it gets none)
            // and it is indifferent to the few depth units it inherits.
            Self::TunnelRoad(_) | Self::TunnelStructure => GroundLayer::Standing,
            // Vertical, and never coplanar with anything: the standing rung.
            Self::QuayWall => GroundLayer::Standing,
            Self::Canopy | Self::Trunk | Self::Furniture => GroundLayer::Standing,
        }
    }

    /// Height in metres, relative to the ground plane at y = 0.
    ///
    /// [`GroundLayer::y_offset`] — derived from the same rung as the depth bias,
    /// so the two cannot contradict each other.
    #[must_use]
    pub fn y_offset(self) -> f32 {
        self.layer().y_offset()
    }

    /// Depth-buffer bias, which is what actually stops the flicker at range.
    ///
    /// [`GroundLayer::depth_bias`]. The unit is a depth-buffer ULP, whose apparent
    /// world shift is `1.19e-7 · bias · z` — it grows with range exactly as the
    /// buffer's own quantum does, which is why the ordering neither decays nor
    /// runs away. The millimetre `y_offset`s cannot do that job on their own: a
    /// geometric gap's significance shrinks as 1/z, so climbing collapses it and
    /// only the ladder is left.
    #[must_use]
    pub fn depth_bias(self) -> f32 {
        self.layer().depth_bias()
    }

    /// The group's rung in the ground stack, low to high. Ordering intent only;
    /// [`Self::depth_bias`] turns it into depth-buffer units.
    #[must_use]
    pub fn depth_rung(self) -> f32 {
        self.layer().rung() as f32
    }

    /// Roughness for the group's material. Water is excluded — it has its own
    /// shader (`systems::water`), which owns all of its surface parameters.
    #[must_use]
    pub fn perceptual_roughness(self) -> f32 {
        match self {
            Self::Road(class) => class.perceptual_roughness(),
            _ => 0.95,
        }
    }
}

/// Which `land` kinds map to which surface. Anything unlisted is skipped rather
/// than guessed — an unstyled polygon is better than a wrong one.
pub(crate) fn land_group(kind: &str) -> Option<SurfaceGroup> {
    Some(match kind {
        "forest" | "wood" => SurfaceGroup::Forest,
        // `grassland`, `cemetery`, `playground` and `plant_nursery` were added on
        // 2026-09-10 and were not an oversight so much as an invisible one: an
        // unhandled kind emits no vector surface at all, so the rasterised tile
        // showed through — and the raster's default *is* `GRASS`, so all four
        // already looked like grass and only a walk over one would tell you they
        // grew no blades. Adding them costs nothing at runtime (the same slots,
        // more ground that passes the coverage mask) and is the cheapest "more
        // grass" there is.
        //
        // The list is the kinds observed in real tiles over Groningen, Amsterdam
        // and Utrecht, not the Shortbread schema read off a page: a kind guessed
        // at is a line nothing exercises. `brownfield` is deliberately left out —
        // derelict land is scrubby rather than green, and the raster default
        // already paints it grass, so it stays consistent either way.
        //
        // `wet_meadow`, `marsh`, `greenfield` and `golf_course` were added on
        // 2026-09-19 from the same kind of evidence, gathered the new way: a
        // `carto-view --check` sweep of the 48 z14 cells around Groningen reports
        // every kind the table does not name, so "what are real tiles asking for?"
        // is now a command rather than a thing somebody has to notice. Counts over
        // that sweep: 19, 2, 1 and 1 features.
        //
        // There is no wetland group to put the first two in and inventing one would
        // be a rung, a palette entry and a texture for two features; a wet meadow is
        // a meadow, and the ground it stands on is the thing that was missing.
        //
        // `vineyard`, `miniature_golf`, `grave_yard` and `swamp` came from widening
        // the same sweep past one city — 71 cells over Amsterdam, Utrecht,
        // Rotterdam, Berlin, London, Paris, Barcelona and Oslo. That is the point of
        // the sweep being a command: one city's tiles only ever ask for one city's
        // land.
        //
        // `grave_yard` is the one worth naming. `cemetery` was already here and
        // these are the same ground under two OSM tags — a municipal cemetery and a
        // churchyard — so the map had been drawing one and not the other since the
        // day `cemetery` was added, which no amount of looking at Groningen would
        // have shown.
        "grass" | "meadow" | "park" | "garden" | "farmland" | "orchard" | "scrub" | "heath"
        | "allotments" | "village_green" | "recreation_ground" | "grassland" | "cemetery"
        | "playground" | "plant_nursery" | "wet_meadow" | "marsh" | "greenfield"
        | "golf_course" | "vineyard" | "miniature_golf" | "grave_yard" | "swamp" => {
            SurfaceGroup::Grass
        }
        // `shingle` is a pebble beach, which is what this group already is.
        "sand" | "beach" | "bare_rock" | "scree" | "shingle" => SurfaceGroup::Sand,
        // `railway` is here for the opposite reason to the four above, and it is
        // the one the repaint made urgent: land the table does not name falls
        // through to the raster's `GRASS` default, which at the old saturation
        // was a pale sage nobody queried and is now a vivid green. A railway yard
        // is ballast, not a meadow.
        // `farmyard` (8 features over the Groningen sweep) is here for exactly the
        // reason `railway` is, and is the only kind in that sweep the map was
        // actively drawing *wrong* rather than merely leaving as a hole: it is the
        // hard-surfaced yard a farm's buildings stand around — concrete, gravel,
        // silage clamps — and an unnamed kind falls through to the raster's `GRASS`
        // default, so every farm in the countryside had a meadow for a yard.
        //
        // `greenhouse_horticulture` and `garages` join it from the eight-city sweep,
        // and both are the same mistake as `farmyard`: ground that is roofed or
        // hard-surfaced, falling through to a grass default. The first is the
        // Westland's glasshouse country, which is one of the least green places in
        // the Netherlands and was being drawn as meadow.
        "residential"
        | "industrial"
        | "commercial"
        | "retail"
        | "quarry"
        | "railway"
        | "farmyard"
        | "greenhouse_horticulture"
        | "garages" => SurfaceGroup::Built,
        _ => return None,
    })
}

/// Every layer in a tile, its feature count, and the `kind` values inside it.
///
/// **The answer to "what is actually in this tile?"**, which is the question that
/// keeps coming back: a surface that is missing, a kind nothing draws, a canal
/// rendered in pieces — each was a guess about the data until somebody counted.
/// It reads the tile and nothing else, so it says what is there rather than what
/// this build makes of it.
///
/// Land features this build draws nothing for, by kind and count.
///
/// **An unhandled kind is a hole in the ground**, and a silent one: [`land_group`]
/// answers `None` and the feature is skipped, so the only evidence is ground that
/// nothing covers. The table above is deliberately built from kinds seen in real
/// tiles rather than read off the schema — which makes "what is this tile asking
/// for that we do not draw?" the question that keeps it honest, and this is how it
/// is asked.
///
/// Counts features, not area: one dropped industrial estate matters more than
/// forty dropped kiosks, but the count is what says *which* kind to go and look
/// at, and the coverage measurement already says how much ground is missing.
#[must_use]
/// One street's centre line, in tile-local metres, with what decides how it is
/// drawn.
///
/// The raw input to `tessellate_roads`, handed out so an artifact in a *shape*
/// can be read as coordinates rather than reconstructed from a picture.
pub struct StreetLine {
    pub kind: String,
    pub flags: String,
    pub closed: bool,
    pub points: Vec<[f32; 2]>,
}

/// Every street centre line in the tile, in tile-local metres.
#[must_use]
pub fn street_lines(tile: &TileData, tile_m: f32) -> Vec<StreetLine> {
    let mut out = Vec::new();
    let Some(meta) = tile.layer("streets") else {
        return out;
    };
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    for feature in tile.features(meta) {
        let flags: Vec<&str> = ["bridge", "tunnel", "oneway", "oneway_reverse", "link"]
            .into_iter()
            .filter(|f| flag_of(feature, f))
            .collect();
        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            _ => continue,
        }
        for ls in lines {
            let points: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x * scale, c.y * scale])
                    .collect();
            if points.len() < 2 {
                continue;
            }
            let closed = {
                let (h, t) = (points[0], points[points.len() - 1]);
                (h[0] - t[0]).hypot(h[1] - t[1]) < 1e-3
            };
            out.push(StreetLine {
                kind: kind_of(feature).unwrap_or("-").to_string(),
                flags: if flags.is_empty() {
                    String::new()
                } else {
                    format!("[{}]", flags.join("+"))
                },
                closed,
                points,
            });
        }
    }
    out
}

pub fn layer_census(tile: &TileData) -> Vec<(String, usize, Vec<(String, usize)>)> {
    let mut out = Vec::new();
    for meta in tile.layers() {
        let mut kinds: Vec<(String, usize)> = Vec::new();
        let mut total = 0usize;
        for feature in tile.features(meta) {
            total += 1;
            // Flags ride with the kind, because on a `streets` layer they are half
            // of what decides how a feature is drawn — a bridge-flagged rail and a
            // tunnel-flagged one take two different paths out of here, and a
            // census that hides that answers "is this drawn?" with the wrong half
            // of the question.
            let mut kind = kind_of(feature).unwrap_or("-").to_string();
            let flags: Vec<&str> = ["bridge", "tunnel", "oneway", "oneway_reverse", "link"]
                .into_iter()
                .filter(|f| flag_of(feature, f))
                .collect();
            if !flags.is_empty() {
                kind.push('[');
                kind.push_str(&flags.join("+"));
                kind.push(']');
            }
            match kinds.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, n)) => *n += 1,
                None => kinds.push((kind, 1)),
            }
        }
        kinds.sort_by(|a, b| b.1.cmp(&a.1));
        out.push((meta.name.clone(), total, kinds));
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

pub fn unhandled_land_kinds(tile: &TileData) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    let Some(meta) = tile.layer("land") else {
        return out;
    };
    for feature in tile.features(meta) {
        let Some(kind) = kind_of(feature) else {
            continue;
        };
        if land_group(kind).is_some() {
            continue;
        }
        match out.iter_mut().find(|(k, _)| k == kind) {
            Some((_, n)) => *n += 1,
            None => out.push((kind.to_string(), 1)),
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

/// A tessellated surface ready to become a Bevy mesh. Positions are **tile-local
/// metres**: origin at the tile's north-west corner, +x east, +z south — which is
/// the same convention `map_stream` places its ground quads in, so the caller only
/// has to position the tile's corner.
///
/// `PartialEq` compares the vertex data bit for bit, `NaN` included — which is
/// what a round trip through [`crate::ground_file`] has to prove, and is the only
/// thing it is used for. It is not a claim that two meshes drawing the same ground
/// are equal.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceMesh {
    pub group: SurfaceGroup,
    // (see `VERTEX_BYTES` below for what one of these costs on the GPU)
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Texture coordinates, **empty for the flat-coloured area surfaces** (their
    /// material has no texture to sample). Roads fill it: `u` runs 0→1 straight
    /// across the carriageway so the markings register with the geometry, and `v`
    /// is metres travelled along the road over
    /// [`RoadClass::texture_length_m`](crate::road_texture::RoadClass::texture_length_m).
    pub uvs: Vec<[f32; 2]>,
    /// Vertex normals, **empty for the flat ground surfaces** — the caller then
    /// fills in a straight-up normal per vertex. Only the vegetation meshes, which
    /// are the one thing here with actual volume, supply their own.
    pub normals: Vec<[f32; 3]>,
    /// Linear RGBA multiplied onto the material's `base_color`, **empty for
    /// everything but vegetation**. This is how thousands of trees get per-instance
    /// tone and brightness variation out of a single shared material: a forest of
    /// one exact green is the giveaway of a procedural scatter.
    pub colors: Vec<[f32; 4]>,
    /// Which rung of the road ladder each vertex draws on, **empty for every group
    /// but [`SurfaceGroup::Road`] and [`SurfaceGroup::RoadCasing`]**.
    ///
    /// This is the *whole* road ordering — class, casing, width band and tie — and
    /// it is deliberately not a height. `systems::road_depth` turns it into a
    /// depth-buffer offset in the vertex stage, which is the only denomination
    /// that settles a coplanar fight at every range; see [`road_rung`] for why the
    /// millimetre ladder it replaced could not.
    pub rungs: Vec<f32>,
}

/// What one vertex of a finished [`SurfaceMesh`] costs on the GPU.
///
/// Position (12) + normal (12) + UV (8) + linear RGBA (16). Every one of those is
/// present by the time the mesh reaches the GPU **even when the fields above are
/// left empty**, because `map_geometry::build_mesh` fills the gaps — a
/// straight-up normal, a planar UV, an opaque white. So this is the real figure
/// and not a lower bound, and it agrees with what `systems::upload_meter` measures
/// off bevy's `RenderMesh::byte_len` (48 B/vertex, checked against a capture).
///
/// It lives in this crate, which may not depend on bevy, because it is a fact
/// about this struct rather than about the renderer. The *limit* a mesh is cut to
/// is the caller's to supply — that one is a frame budget and belongs to
/// `quality`.
///
/// **Road meshes carry a fifth attribute** — the 4-byte depth rung
/// ([`Self::rungs`](SurfaceMesh::rungs)) — and are still inside this figure,
/// because they are also the group that carries *no* vertex colour: 12 + 12 + 8 +
/// 4 = 36. The constant stays an upper bound over every group, which is what a
/// byte budget needs it to be.
pub const VERTEX_BYTES: usize = 12 + 12 + 8 + 16;
/// Indices are `u32`, always — a planted cell runs well past 65 k vertices.
pub const INDEX_BYTES: usize = 4;

/// What this many vertices and indices will cost to upload.
#[must_use]
pub fn mesh_upload_bytes(vertices: usize, indices: usize) -> usize {
    vertices * VERTEX_BYTES + indices * INDEX_BYTES
}

/// Tessellate a tile's area layers into one mesh per [`SurfaceGroup`], **cut so
/// that no two of them cover the same ground**.
///
/// `tile_m` is the tile's ground size in metres, so the output is directly in world
/// units. Only water and landuse are handled here — buildings deliberately are not,
/// because 3DBAG (ADR-027) and the Overpass detail patches (ADR-022) already supply
/// them in 3D, and they are ~75% of a tile's triangles.
///
/// # Why the fills are cut
///
/// [`crate::ground_stack`] orders one layer against another, and that is all a
/// depth ladder can ever do. It cannot order a layer against **itself** — two
/// polygons of one group share a rung by definition — and the tile is full of
/// them: a cemetery inside a park, a playground inside a recreation ground, a
/// village green inside a meadow all arrive as separate `land` features that all
/// resolve to [`SurfaceGroup::Grass`]. Measured on the Groningen z14 fixture
/// before this cut, **9.7 % of the grass and 3.1 % of the built land was covered
/// twice by its own polygons**, and there was nothing anywhere in the tree that
/// could decide which copy to draw.
///
/// That is the same fight, and the same answer, as the road ribbons'
/// ([`crate::road_stitch`], whose module note has the argument in full): a
/// coplanar overlap is not a tie to be broken, it is geometry that should not be
/// there. Each feature keeps only the ground no higher-priority feature claims,
/// and what is drawn is then a partition.
///
/// The cut runs **across groups as well as within them**, which costs nothing
/// extra — the same pass, the same index — and takes the other 10 % out of the
/// ladder's hands too. Measured on the same fixture, a fifth of the woodland lies
/// under grass and nearly a quarter of the water does. The ladder still holds them
/// apart, and now has almost nothing left to hold.
///
/// **Priority, high to low: the layer's rung, then the smaller polygon.** The rung
/// is the map's own order — water over grass, never the other way round. Within a
/// group the smaller feature wins because it is the *more specific* statement
/// about that ground: a cemetery drawn inside a park is a cemetery, and swallowing
/// it into the park is the one outcome nobody wants. The feature index breaks the
/// remaining ties, because the order has to be strict — an order with a draw in it
/// cuts both sides out of the ground they share and opens a hole.
///
/// # What this does *not* let the ladder retire
///
/// The cut is per tile, and a feature reaching across a tile boundary is two
/// features in two tiles that never see each other. The bias is what settles those,
/// as it settles a flyover over the road beneath it. Removing the overlap makes the
/// ladder a backstop instead of the mechanism; it does not make it optional.
///
/// It also means **a group cut out from under another may not be hidden on its
/// own**, or a hole opens where the one above it used to be. No layer toggle
/// reaches these groups today (`DataLayers` has none for water or landuse) and one
/// that did would have to exclude its group from the cut.
/// Half the drawn width of a narrow waterway, by Shortbread `kind`, or `None` for
/// one this build leaves to the basemap.
///
/// **A ditch is deliberately not here.** The Groningen countryside is a lattice of
/// them — thousands of features per cell, each a metre or two wide — and a
/// metre-wide ribbon of water is below the width at which a 3D surface tells you
/// anything a drawn line does not. They stay on the raster basemap underneath,
/// which is where everything this path does not draw already lives.
///
/// The three that are here are the ones a person would call water: a river you
/// cannot jump, a canal a boat uses, a stream with a bank. Widths are the drawn
/// width of the *channel*, not a survey — Shortbread carries no width, so this is
/// the same kind of nominal figure [`road_width_m`] uses for a street.
fn water_line_half_width_m(kind: &str) -> Option<f32> {
    Some(
        match kind {
            "river" => 14.0,
            "canal" => 8.0,
            "stream" => 3.0,
            _ => return None,
        } * 0.5,
    )
}

pub fn tessellate_surfaces(
    tile: &TileData,
    tile_m: f32,
    max_bytes: Option<usize>,
) -> Result<Vec<SurfaceMesh>, String> {
    // One entry per *feature*, because the cut needs to tell them apart. They are
    // merged into one mesh per group afterwards, exactly as before.
    let mut fills: Vec<crate::road_stitch::Ribbon> = Vec::new();

    for meta in tile.layers() {
        // (layer, fixed group) — `land` resolves its group per feature instead.
        let fixed = match meta.name.as_str() {
            "ocean" | "water_polygons" => Some(SurfaceGroup::Water),
            "sites" => Some(SurfaceGroup::Built),
            "land" => None,
            // Narrow water is a *line* in Shortbread, not an area, and drawing
            // only the areas is why a canal renders as a row of disconnected
            // ponds: OSM maps the wide stretches as polygons and the narrow ones
            // as ways, and the join between them is invisible on a flat map and
            // a hole in a 3D one. Handled below rather than here, because it
            // needs a ribbon rather than a fill.
            "water_lines" => None,
            _ => continue,
        };
        let is_water_line = meta.name == "water_lines";
        let extent = TileData::extent(meta);
        let scale = tile_m / extent;

        for feature in tile.features(meta) {
            let group = if is_water_line {
                SurfaceGroup::Water
            } else {
                match fixed {
                    Some(g) => g,
                    None => match kind_of(feature).and_then(land_group) {
                        Some(g) => g,
                        None => continue,
                    },
                }
            };
            let mut positions: Vec<[f32; 3]> = Vec::new();
            let mut indices: Vec<u32> = Vec::new();
            if is_water_line {
                let Some(half) = water_line_half_width_m(kind_of(feature).unwrap_or("")) else {
                    continue;
                };
                let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
                match &feature.geometry {
                    Geometry::LineString(ls) => lines.push(ls),
                    Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
                    _ => {}
                }
                let mut uvs: Vec<[f32; 2]> = Vec::new();
                for ls in lines {
                    let pts: Vec<[f32; 2]> =
                        ls.0.iter()
                            .filter(|c| c.x.is_finite() && c.y.is_finite())
                            .map(|c| [c.x, c.y])
                            .collect();
                    // Clipped, not clamped, exactly as the streets are: the
                    // neighbouring tile carries its own copy of the same
                    // waterway and the two must abut rather than overlap.
                    for run in clip_polyline(&pts, 0.0, extent) {
                        let world: Vec<[f32; 2]> =
                            run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                        // Butt ends. A waterway's end is either the tile border
                        // or the polygon it runs into, and `stitch` settles the
                        // overlap with that polygon — an extension would only
                        // give the cut more to remove.
                        let ends = [EndJoin::default(), EndJoin::default()];
                        ribbon(
                            &world,
                            half,
                            0.0,
                            1.0,
                            ends,
                            false,
                            &mut positions,
                            &mut uvs,
                            &mut indices,
                        );
                    }
                }
            } else {
                match &feature.geometry {
                    Geometry::Polygon(p) => {
                        tessellate_polygon(p, scale, extent, &mut positions, &mut indices);
                    }
                    Geometry::MultiPolygon(mp) => {
                        for p in mp {
                            tessellate_polygon(p, scale, extent, &mut positions, &mut indices);
                        }
                    }
                    _ => {}
                }
            }
            if indices.is_empty() {
                continue;
            }
            let fi = fills.len();
            fills.push(crate::road_stitch::Ribbon {
                group,
                rung: 0.0,
                priority: (
                    group.layer().rung(),
                    smaller_first(&positions, &indices),
                    fi,
                    0,
                    0,
                ),
                // Not a bridge, and neither is anything else here — `stitch`'s
                // bridge flag isolates a deck from the roads under it, and a
                // ground fill has no such partner.
                bridge: false,
                positions,
                // Flat-coloured and untextured: the group's material carries the
                // tone (`SurfaceGroup::base_color`), so there is nothing per
                // vertex for the cut to carry across.
                uvs: Vec::new(),
                colors: Vec::new(),
                indices,
            });
        }
    }

    crate::road_stitch::stitch(&mut fills);

    // Back into one mesh per group, cutting a group loose whenever it passes the
    // frame's upload budget — **after** the feature that crossed it rather than
    // before, because a polygon's vertex count is not known until it has been
    // tessellated, so the choice is to overshoot by one feature or to guess.
    // Overshooting by one is bounded; a whole cell's worth is what this exists to
    // stop. A single feature larger than the budget still goes as one mesh, since
    // bevy writes one whole asset per frame regardless.
    let mut out: Vec<SurfaceMesh> = Vec::new();
    let mut builders: Vec<(SurfaceGroup, Vec<[f32; 3]>, Vec<u32>)> = Vec::new();
    for r in fills {
        if r.indices.is_empty() {
            continue;
        }
        let slot = match builders.iter().position(|(g, _, _)| *g == r.group) {
            Some(i) => i,
            None => {
                builders.push((r.group, Vec::new(), Vec::new()));
                builders.len() - 1
            }
        };
        let (_, positions, indices) = &mut builders[slot];
        let base = positions.len() as u32;
        positions.extend(r.positions);
        indices.extend(r.indices.iter().map(|i| i + base));
        if max_bytes.is_some_and(|max| mesh_upload_bytes(positions.len(), indices.len()) >= max) {
            out.push(SurfaceMesh {
                group: r.group,
                positions: std::mem::take(positions),
                indices: std::mem::take(indices),
                uvs: Vec::new(),
                normals: Vec::new(),
                colors: Vec::new(),
                rungs: Vec::new(),
            });
        }
    }
    out.extend(
        builders
            .into_iter()
            .filter(|(_, p, i)| !p.is_empty() && !i.is_empty())
            .map(|(group, positions, indices)| SurfaceMesh {
                group,
                positions,
                indices,
                uvs: Vec::new(),
                normals: Vec::new(),
                colors: Vec::new(),
                rungs: Vec::new(),
            }),
    );
    Ok(out)
}

/// What fraction of a cell the given surfaces actually cover, sampled on an
/// `n × n` grid.
///
/// **A hole in the ground is otherwise only found by pointing a camera at it**,
/// and that is not a test — twice on 2026-09-19 a hole was measured from a
/// viewpoint that happened to include, or happened to miss, the block it was in.
/// This is per cell, deterministic, and free of any opinion about where to look.
///
/// It will never reach 100 %: the cut runs on a millimetre lattice and this samples
/// texel centres, so a sliver survives along every kerb. What it is for is the
/// difference between that residual and a *block* of missing ground.
///
/// Sampling rather than area: exact polygon area would have to resolve the overlaps
/// the cut is supposed to have removed, and would report 103 % if it had not.
#[must_use]
pub fn surface_coverage(meshes: &[SurfaceMesh], tile_m: f32, n: usize) -> f32 {
    if n == 0 || tile_m <= 0.0 {
        return 0.0;
    }
    let mut hit = vec![false; n * n];
    #[allow(clippy::cast_precision_loss)]
    let scale = n as f32 / tile_m;
    for m in meshes {
        for t in m.indices.chunks_exact(3) {
            let p: Vec<[f32; 2]> = t
                .iter()
                .map(|&i| {
                    let v = m.positions[i as usize];
                    [v[0] * scale, v[2] * scale]
                })
                .collect();
            let (a, b, c) = (p[0], p[1], p[2]);
            // Both ends clamped: a tile carries a buffer, so a ribbon can lie
            // entirely past the grid and a `min` on the maximum alone walks off the
            // end of the row.
            let lo = |v: f32| -> usize {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let i = v.floor().max(0.0) as usize;
                i.min(n - 1)
            };
            let hi = |v: f32| -> usize {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let i = v.ceil().max(0.0) as usize;
                i.min(n - 1)
            };
            let (minx, maxx) = (lo(a[0].min(b[0]).min(c[0])), hi(a[0].max(b[0]).max(c[0])));
            let (miny, maxy) = (lo(a[1].min(b[1]).min(c[1])), hi(a[1].max(b[1]).max(c[1])));
            let side = |u: [f32; 2], v: [f32; 2], q: [f32; 2]| {
                (v[0] - u[0]) * (q[1] - u[1]) - (v[1] - u[1]) * (q[0] - u[0])
            };
            for y in miny..=maxy {
                for x in minx..=maxx {
                    #[allow(clippy::cast_precision_loss)]
                    let q = [x as f32 + 0.5, y as f32 + 0.5];
                    let (s0, s1, s2) = (side(a, b, q), side(b, c, q), side(c, a, q));
                    if (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0)
                        || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0)
                    {
                        hit[y * n + x] = true;
                    }
                }
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    {
        hit.iter().filter(|h| **h).count() as f32 / (n * n) as f32
    }
}

/// A feature's rank by size, **descending** — bigger area, smaller number — so it
/// can go straight into `road_stitch`'s "larger priority wins" ordering and mean
/// "the smaller polygon draws on top".
///
/// Twice the triangle area, summed and saturated into a `u32` of square metres.
/// Quantising to the square metre is deliberate: two features of genuinely equal
/// size then tie here and fall through to the feature index, which is stable,
/// rather than being separated by a float's last bit — the same reason
/// `road_stitch::Ribbon::priority` spells its tie-breaks out.
fn smaller_first(positions: &[[f32; 3]], indices: &[u32]) -> u32 {
    let mut twice_area = 0.0f32;
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (
            positions[t[0] as usize],
            positions[t[1] as usize],
            positions[t[2] as usize],
        );
        twice_area += ((b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0])).abs();
    }
    let sq_m = (twice_area * 0.5) as u32;
    u32::MAX - sq_m.min(u32::MAX)
}

/// One bridge deck: a `bridges` polygon, tessellated but deliberately **not**
/// merged with its neighbours.
///
/// Every other surface here is batched into one mesh per group per cell, and this
/// is the exception, because each deck needs its own *height* — a number that does
/// not exist in the tile and arrives later, per deck, from `systems::height`. So
/// the worker does the tessellation and hands back the pieces; the main thread
/// lifts each one to its measured height and merges them then.
///
/// The rings come back too. They are what the deck's fascia is extruded from, and
/// what says whether a sample of AHN — or a road ribbon — is *on* this deck.
#[derive(Debug, Clone)]
pub struct BridgeDeck {
    /// Deck surface triangles, tile-local metres at `y = 0`.
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Outline rings, clipped and closed (exterior first, then holes).
    pub rings: Vec<Vec<[f32; 2]>>,
    /// Tile-local bounding box of the outline.
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl BridgeDeck {
    /// Is this tile-local point on the deck? Even-odd crossing count over every
    /// ring, so a hole in the structure counts as off it.
    #[must_use]
    pub fn contains(&self, p: [f32; 2]) -> bool {
        if p[0] < self.min[0] || p[0] > self.max[0] || p[1] < self.min[1] || p[1] > self.max[1] {
            return false;
        }
        let mut inside = false;
        for ring in &self.rings {
            for e in ring.windows(2) {
                let (a, b) = (e[0], e[1]);
                if (a[1] > p[1]) != (b[1] > p[1]) {
                    let t = (p[1] - a[1]) / (b[1] - a[1]);
                    if p[0] < a[0] + t * (b[0] - a[0]) {
                        inside = !inside;
                    }
                }
            }
        }
        inside
    }

    /// Longest side of the deck's footprint, in metres — how a caller sizes a
    /// height sample grid to the thing being measured.
    #[must_use]
    pub fn span_m(&self) -> f32 {
        (self.max[0] - self.min[0]).max(self.max[1] - self.min[1])
    }
}

/// Tessellate the `bridges` layer: one [`BridgeDeck`] per structure.
///
/// Shortbread's `bridges` layer is **areas**, not lines — the outline of the
/// structure itself, carriageway and parapets and all. The raster path only ever
/// stroked them as a hairline and the geometry path ignored them entirely, which is
/// why a viaduct was nothing but the road ribbon crossing it. Measured over
/// Groningen station: the Emmaviaduct arrives as 34 × 149 m and 68 × 176 m of
/// footprint. That is a deck you can stand geometry on.
///
/// **Not every bridge has an area.** Where OSM maps the bridge-flagged ways and
/// no `man_made=bridge` outline, the structure is built from the ways
/// themselves ([`street_decks`]) and follows the outlines in the list.
pub fn tessellate_bridges(tile: &TileData, tile_m: f32) -> Result<Vec<BridgeDeck>, String> {
    let mut decks = area_decks(tile, tile_m);
    let from_streets = street_decks(tile, tile_m, &decks);
    decks.extend(from_streets);
    Ok(decks)
}

/// The decks the `bridges` layer draws as areas.
fn area_decks(tile: &TileData, tile_m: f32) -> Vec<BridgeDeck> {
    let Some(meta) = tile.layer("bridges") else {
        return Vec::new();
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;

    let mut decks = Vec::new();
    for feature in features {
        let mut polys: Vec<&geo_types::Polygon<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::Polygon(p) => polys.push(p),
            Geometry::MultiPolygon(mp) => polys.extend(mp.iter()),
            _ => continue,
        }
        for poly in polys {
            let mut positions = Vec::new();
            let mut indices = Vec::new();
            tessellate_polygon(poly, scale, extent, &mut positions, &mut indices);
            let rings = clipped_rings(poly, scale, extent);
            if indices.is_empty() || rings.is_empty() {
                continue;
            }
            let mut min = [f32::MAX, f32::MAX];
            let mut max = [f32::MIN, f32::MIN];
            for p in rings.iter().flatten() {
                min = [min[0].min(p[0]), min[1].min(p[1])];
                max = [max[0].max(p[0]), max[1].max(p[1])];
            }
            decks.push(BridgeDeck {
                positions,
                indices,
                rings,
                min,
                max,
            });
        }
    }
    decks
}

/// How far a deck built from a street reaches past the carriageway on either
/// side, metres: the edge beam and the parapet an outline would have included.
const STREET_DECK_MARGIN_M: f32 = 1.0;

/// Shortest stretch of a bridge-flagged street that is given a deck of its own,
/// metres. Below it the stretch is a sliver of a way overhanging an outline, or a
/// culvert nobody would call a bridge.
const STREET_DECK_MIN_M: f32 = 6.0;

/// Smallest deck built from a street, square metres — a footpath's
/// [`STREET_DECK_MIN_M`] of span is about fifteen.
const STREET_DECK_MIN_AREA_M2: f32 = 8.0;

/// **Decks for the bridges OSM maps only as flagged ways.** Shortbread's `bridges`
/// layer is the `man_made=bridge` areas, and many bridges have none: the
/// Europaplein flyover in Groningen, where the ring road crosses the junction
/// (reported 2026-09-26), is four `bridge=yes` carriageways and no outline. With
/// no deck under them nothing measured their height and nothing lifted them, so
/// the flyover was drawn on the ground across the roads it passes over.
///
/// Each stretch of a bridge-flagged road or railway that no outline covers is buffered
/// into one: the carriageway's own width plus [`STREET_DECK_MARGIN_M`] either
/// side, round at the bends and square at the ends, where the way — and so the
/// structure — stops. Overlapping stretches are merged, so an interchange's slip
/// road joining the main span is one deck rather than two slabs fighting over the
/// same ground, and the result is clipped to the tile the way an outline is.
///
/// From here a deck is a deck: `split_by_deck` puts the ways on it,
/// `extend_approaches` finds their ramps and the height is measured over the
/// outline as it is for any other — which is also what keeps the canal bridges
/// flat, since they measure under `MIN_LIFT_M`.
fn street_decks(tile: &TileData, tile_m: f32, areas: &[BridgeDeck]) -> Vec<BridgeDeck> {
    use i_overlay::core::fill_rule::FillRule;
    use i_overlay::core::overlay::{Overlay, ShapeType};
    use i_overlay::core::overlay_rule::OverlayRule;
    use i_overlay::i_float::int::point::IntPoint;

    let Some(meta) = tile.layer("streets") else {
        return Vec::new();
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    let mm = |p: [f32; 2]| {
        IntPoint::new(
            (p[0] * 1000.0).round() as i32,
            (p[1] * 1000.0).round() as i32,
        )
    };
    // Clipped to the tile the way an outline is — every piece is convex, so
    // Sutherland–Hodgman is exact — and given one winding: under the non-zero rule
    // a clockwise piece over an anticlockwise one would cancel into a hole
    // instead of merging.
    let contour = |pts: Vec<[f32; 2]>| -> Vec<IntPoint<i32>> {
        let pts = clip_ring(&pts, tile_m);
        let twice_area: f32 = pts
            .iter()
            .zip(pts.iter().cycle().skip(1))
            .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
            .sum();
        let mut out: Vec<IntPoint<i32>> = pts.into_iter().map(mm).collect();
        if twice_area < 0.0 {
            out.reverse();
        }
        out
    };

    let mut overlay: Overlay<i32> = Overlay::new(64);
    let mut any = false;
    let mut add = |overlay: &mut Overlay<i32>, pts: Vec<[f32; 2]>| {
        let c = contour(pts);
        if c.len() >= 3 {
            overlay.add_contour(&c, ShapeType::Subject);
            any = true;
        }
    };
    for feature in features {
        if !flag_of(feature, "bridge") {
            continue;
        }
        // Main roads and rail only. A path or a lane is narrow and routinely runs
        // under trees, and the survey over a strip that narrow is the canopy:
        // Groningen's boardwalks and Stadspark lanes measured 11–23 m
        // (2026-09-26), which would have stood them on slabs over the park.
        let Some((class, half)) = drawn_street(feature) else {
            continue;
        };
        if matches!(class, RoadClass::Path | RoadClass::Minor) {
            continue;
        }
        let w = half + STREET_DECK_MARGIN_M;
        let lines: Vec<&geo_types::LineString<f32>> = match &feature.geometry {
            Geometry::LineString(ls) => vec![ls],
            Geometry::MultiLineString(mls) => mls.iter().collect(),
            _ => continue,
        };
        for ls in lines {
            let pts: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x, c.y])
                    .collect();
            let whole_m = crate::tunnels::polyline_len(&pts) * scale;
            if whole_m < STREET_DECK_MIN_M {
                continue;
            }
            for run in clip_polyline(&pts, 0.0, extent) {
                // Asked every couple of metres, because `split_by_deck` only asks
                // at the vertices: a way whose two ends lie just off an outline
                // and whose whole span crosses it otherwise reads as never
                // touching it, and was given a second deck over the first.
                let mut metres: Vec<[f32; 2]> = Vec::new();
                for s in run.windows(2) {
                    let (a, b) = (
                        [s[0][0] * scale, s[0][1] * scale],
                        [s[1][0] * scale, s[1][1] * scale],
                    );
                    let steps = ((b[0] - a[0]).hypot(b[1] - a[1]) / 2.0).ceil().max(1.0) as usize;
                    metres.extend((0..steps).map(|k| {
                        let t = k as f32 / steps as f32;
                        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
                    }));
                }
                if let Some(&last) = run.last() {
                    metres.push([last[0] * scale, last[1] * scale]);
                }
                // A run cut by the tile border is short here and continues next
                // door, where the rest of the same structure is built.
                let at_border = |p: [f32; 2]| {
                    p[0] <= 0.01 || p[1] <= 0.01 || p[0] >= tile_m - 0.01 || p[1] >= tile_m - 0.01
                };
                for (owner, part) in split_by_deck(&metres, areas) {
                    if owner.is_some() || part.len() < 2 {
                        continue;
                    }
                    let clipped = part.first().is_some_and(|p| at_border(*p))
                        || part.last().is_some_and(|p| at_border(*p));
                    if crate::tunnels::polyline_len(&part) < STREET_DECK_MIN_M && !clipped {
                        continue;
                    }
                    // Back to the way's own bends before it is buffered. One
                    // rectangle per two-metre sample meets its neighbour on an edge
                    // computed twice, and after rounding the two copies disagree by
                    // a millimetre: the union came back full of zero-width spikes
                    // and slits, and the fascia hung a sliver of wall from every one
                    // (the Europaplein deck, 2026-09-27).
                    let part = without_collinear(&part);
                    for s in part.windows(2) {
                        let (a, b) = (s[0], s[1]);
                        let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
                        let len = dx.hypot(dz);
                        if len < 1e-3 {
                            continue;
                        }
                        let n = [-dz / len * w, dx / len * w];
                        add(
                            &mut overlay,
                            vec![
                                [a[0] + n[0], a[1] + n[1]],
                                [b[0] + n[0], b[1] + n[1]],
                                [b[0] - n[0], b[1] - n[1]],
                                [a[0] - n[0], a[1] - n[1]],
                            ],
                        );
                    }
                    // Round joins, so a bend is not notched on its outside —
                    // only where it bends, not at every sample laid above.
                    for s in part.windows(3) {
                        let (u, v) = (
                            [s[1][0] - s[0][0], s[1][1] - s[0][1]],
                            [s[2][0] - s[1][0], s[2][1] - s[1][1]],
                        );
                        let turn = (u[0] * v[1] - u[1] * v[0]).abs()
                            / (u[0].hypot(u[1]) * v[0].hypot(v[1])).max(1e-6);
                        if turn < 1e-3 {
                            continue;
                        }
                        let c = s[1];
                        let disc = (0..12)
                            .map(|k| {
                                let t = k as f32 * std::f32::consts::TAU / 12.0;
                                [c[0] + w * t.cos(), c[1] + w * t.sin()]
                            })
                            .collect();
                        add(&mut overlay, disc);
                    }
                }
            }
        }
    }
    if !any {
        return Vec::new();
    }
    // Less the outlines, so no ground is claimed by two decks: a street deck
    // only meets an outline where the way runs off it, and there the two must
    // share an edge rather than overlap at two measured heights.
    for deck in areas {
        for ring in &deck.rings {
            let pts: Vec<IntPoint<i32>> = ring[..ring.len() - 1].iter().map(|p| mm(*p)).collect();
            overlay.add_contour(&pts, ShapeType::Clip);
        }
    }

    let mut decks = Vec::new();
    for shape in overlay.overlay(OverlayRule::Difference, FillRule::NonZero) {
        let rings: Vec<Vec<[f32; 2]>> = shape
            .iter()
            .map(|c| {
                without_spikes(
                    c.iter()
                        .map(|p| [p.x as f32 / 1000.0, p.y as f32 / 1000.0])
                        .collect(),
                )
            })
            .filter(|r| r.len() >= 3)
            .map(|mut ring| {
                ring.push(ring[0]);
                ring
            })
            .collect();
        // What the subtraction leaves along an outline's edge where a way ran
        // onto it is a sliver, not a structure.
        let area = rings.first().map_or(0.0, |r| {
            0.5 * r
                .windows(2)
                .map(|e| e[0][0] * e[1][1] - e[1][0] * e[0][1])
                .sum::<f32>()
                .abs()
        });
        if area < STREET_DECK_MIN_AREA_M2 {
            continue;
        }
        let mut flat: Vec<f64> = Vec::new();
        let mut holes: Vec<usize> = Vec::new();
        for (ri, ring) in rings.iter().enumerate() {
            if ri > 0 {
                holes.push(flat.len() / 2);
            }
            for p in &ring[..ring.len() - 1] {
                flat.extend([f64::from(p[0]), f64::from(p[1])]);
            }
        }
        let Ok(tris) = earcutr::earcut(&flat, &holes, 2) else {
            continue;
        };
        let positions: Vec<[f32; 3]> = flat
            .chunks_exact(2)
            .map(|p| [p[0] as f32, 0.0, p[1] as f32])
            .collect();
        let mut indices: Vec<u32> = Vec::with_capacity(tris.len());
        for t in tris.chunks_exact(3) {
            let [a, b, c] = [positions[t[0]], positions[t[1]], positions[t[2]]];
            // Facing up, as every ground triangle out of `tessellate_polygon` does;
            // the union keeps no winding of its own worth trusting.
            let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
            if cross.abs() < f32::EPSILON {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let t: [u32; 3] = [t[0] as u32, t[1] as u32, t[2] as u32];
            indices.extend(if cross < 0.0 { t } else { [t[0], t[2], t[1]] });
        }
        if indices.is_empty() {
            continue;
        }
        let mut min = [f32::MAX, f32::MAX];
        let mut max = [f32::MIN, f32::MIN];
        for p in rings.iter().flatten() {
            min = [min[0].min(p[0]), min[1].min(p[1])];
            max = [max[0].max(p[0]), max[1].max(p[1])];
        }
        decks.push(BridgeDeck {
            positions,
            indices,
            rings,
            min,
            max,
        });
    }
    decks
}

/// An open ring with its zero-width spikes removed: every vertex the outline
/// runs out to and straight back from.
///
/// Where two carriageways' square-cut ends meet at an angle, the union of their
/// buffers keeps the sliver between them as a slit a few centimetres wide, and
/// the fascia is extruded from every edge of the ring — so the Europaplein deck
/// hung a sliver of wall from each (2026-09-27). A spike covers no ground, so
/// dropping it changes nothing the deck stands over.
fn without_spikes(mut ring: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    loop {
        let n = ring.len();
        if n < 3 {
            return ring;
        }
        let spike = (0..n).find(|&k| {
            let (a, b, c) = (ring[(k + n - 1) % n], ring[k], ring[(k + 1) % n]);
            let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - b[0], c[1] - b[1]]);
            let (lu, lv) = (u[0].hypot(u[1]), v[0].hypot(v[1]));
            lu < 1e-3
                || lv < 1e-3
                || (u[0] * v[0] + u[1] * v[1] < 0.0
                    && (u[0] * v[1] - u[1] * v[0]).abs() < 0.01 * lu * lv)
        });
        match spike {
            Some(k) => {
                ring.remove(k);
            }
            None => return ring,
        }
    }
}

/// A polyline with the vertices it runs straight through dropped: the ends, and
/// every vertex where the direction turns by more than about a twentieth of a
/// degree.
fn without_collinear(pts: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(pts.len());
    for &p in pts {
        if out
            .last()
            .is_some_and(|q| (q[0] - p[0]).hypot(q[1] - p[1]) < 1e-3)
        {
            continue;
        }
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let (u, v) = ([b[0] - a[0], b[1] - a[1]], [p[0] - b[0], p[1] - b[1]]);
            let turn = (u[0] * v[1] - u[1] * v[0]) / (u[0].hypot(u[1]) * v[0].hypot(v[1]));
            let ahead = u[0] * v[0] + u[1] * v[1] > 0.0;
            if ahead && turn.abs() < 1e-3 {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// How far outside a deck's outline a surveyed wall may run and still be that
/// deck's own parapet or screen, metres. Wide, because a deck built from the
/// ways ([`street_decks`]) is only the carriageways' drawn width: the real
/// Europaplein viaduct carries shoulders and a slip lane besides, and its
/// parapets stood 14 m from the carriageway's centre line, nine past the deck.
pub const DECK_WALL_REACH_M: f32 = 10.0;

/// How far from an approach's centre line a surveyed wall may run and still be
/// that ramp's own retaining wall or screen, metres: half a dual carriageway and
/// its verge.
pub const APPROACH_WALL_REACH_M: f32 = 14.0;

/// Tallest measured wall on a deck or beside a ramp that is taken to stand where
/// it is drawn rather than to be the structure's own, metres: a canal bridge's
/// parapet measures about one.
pub const BRIDGE_WALL_MAX_M: f32 = 1.5;

/// Whether a surveyed **wall** belongs to a bridge — a parapet or noise screen on
/// its deck, a retaining wall or screen along the ramp up to it — and so cannot
/// be drawn the way every other boundary is.
///
/// A boundary is a box standing *up from the ground* by its measured height, and
/// the 3D product measures a wall from its lowest foot to its top. For a
/// structure's own walls both halves of that are wrong: the viaduct's screens
/// over Groningen's Europaplein measured 7–9.5 m and were 170–270 m slabs
/// standing on the Europaweg under the deck, and its unmeasured parapets were
/// 48 m walls across the same road at ground level (2026-09-26). It is the
/// bridge's version of [`crate::tunnels::wall_of_a_sunken_road`], and it answers
/// the same way: the line is sampled, and it is the bridge's when most of it lies
/// on a deck (widened by [`DECK_WALL_REACH_M`]) or beside a ramp that climbs to
/// one. Only a wall with no measurement or one taller than [`BRIDGE_WALL_MAX_M`]
/// qualifies: a measured metre on a canal bridge is its parapet, standing where
/// it is drawn, and a low wall by a ramp is a garden wall. An unmeasured one
/// counts on the ramp too: OSM's bridge way routinely stops short of the
/// structure, and the Europaplein's parapets ran half off the deck and stood
/// as 2 m slabs on the Europaweg.
#[must_use]
pub fn wall_of_a_bridge(
    decks: &[BridgeDeck],
    approaches: &[Approach],
    line: &[[f32; 2]],
    height_m: Option<f32>,
) -> bool {
    if line.len() < 2 || decks.is_empty() {
        return false;
    }
    if height_m.is_some_and(|h| h <= BRIDGE_WALL_MAX_M) {
        return false;
    }
    let on_deck = |p: [f32; 2]| {
        decks.iter().any(|d| {
            p[0] >= d.min[0] - DECK_WALL_REACH_M
                && p[0] <= d.max[0] + DECK_WALL_REACH_M
                && p[1] >= d.min[1] - DECK_WALL_REACH_M
                && p[1] <= d.max[1] + DECK_WALL_REACH_M
                && (d.contains(p)
                    || d.rings.iter().any(|r| {
                        r.windows(2)
                            .any(|e| segment_distance(p, e[0], e[1]) <= DECK_WALL_REACH_M)
                    }))
        })
    };
    let by_ramp = |p: [f32; 2]| {
        approaches.iter().any(|a| {
            a.points
                .windows(2)
                .any(|e| segment_distance(p, e[0], e[1]) <= APPROACH_WALL_REACH_M)
        })
    };
    let (mut samples, mut near) = (0usize, 0usize);
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        let steps = ((b[0] - a[0]).hypot(b[1] - a[1]) / 1.5).ceil().max(1.0) as usize;
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            samples += 1;
            if on_deck(p) || by_ramp(p) {
                near += 1;
            }
        }
    }
    near * 5 >= samples * 3
}

/// Distance from `p` to the segment `a`–`b`.
fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dz * dz;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p[0] - a[0] - dx * t).hypot(p[1] - a[1] - dz * t)
}

/// Tessellate a tile's `streets` layer into textured road ribbons, one mesh per
/// [`RoadClass`].
///
/// The companion to [`tessellate_surfaces`]: same tile bytes, same tile-local
/// metre convention, same one-mesh-per-group batching. Roads are lines rather
/// than areas, so the work is different in three ways:
///
/// * **Width is real.** [`road_width_m`] is metres of carriageway, not the raster
///   path's exaggerated stroke weight.
/// * **Lines must be clipped, not clamped.** [`tessellate_polygon`] can flatten a
///   polygon's overhang against the tile border because the neighbour continues
///   the same surface; doing that to a line would drag a road along the seam.
///   [`clip_polyline`] cuts instead, so the neighbouring tile's copy of the same
///   street meets it edge to edge.
/// * **UVs matter.** They are what puts the centre line down the centre.
/// * **Ends have to know what they meet.** A ribbon that simply stops leaves a
///   notch at every junction; one that always overshoots leaves a flap at every
///   dead end. [`street_nodes`] is the difference between the two.
///
/// `decks` are the tile's [`BridgeDeck`]s, and a bridge-flagged street that runs
/// over one **keeps its ribbon** — it comes back in
/// [`RoadTessellation::on_deck`], tagged with the deck, for the caller to lift to
/// that deck's measured height. It used to be dropped instead, on the theory that
/// "the deck is the road surface up there", and the theory does not survive being
/// looked at: a deck is a flat untextured slab, so every viaduct and every canal
/// bridge was a bare grey polygon with the lane markings, the tarmac and the kerb
/// stopping dead at its edge and starting again on the far side. The deck is the
/// *structure*; the road on it is a road.
///
/// So the carriageway is cut **out of** the deck (`decks` is taken by `&mut` for
/// exactly this), which is why the two can share a plane without a depth fight:
/// the same partition rule [`crate::road_stitch`] applies to the ground sheet,
/// applied to the deck. What is left of the slab is the verge and the parapet
/// either side of the road — which is what a bridge looks like from it.
///
/// Only `bridge` features are moved onto a deck, so the road *under* a viaduct
/// keeps every metre of its own ground-level ribbon.
pub fn tessellate_roads(
    tile: &TileData,
    tile_m: f32,
    decks: &mut [BridgeDeck],
    max_bytes: Option<usize>,
    surveyed_paving: bool,
) -> Result<RoadTessellation, String> {
    let cover = if surveyed_paving {
        SurveyedCover::Paths
    } else {
        SurveyedCover::Nothing
    };
    tessellate_roads_surveyed(tile, tile_m, decks, max_bytes, cover)
}

/// [`tessellate_roads`], told how much of the cell the survey draws
/// ([`SurveyedCover`]) so it can leave those ribbons out.
pub fn tessellate_roads_surveyed(
    tile: &TileData,
    tile_m: f32,
    decks: &mut [BridgeDeck],
    max_bytes: Option<usize>,
    cover: SurveyedCover,
) -> Result<RoadTessellation, String> {
    let Some(meta) = tile.layer("streets") else {
        return Ok(RoadTessellation::default());
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    let nodes = street_nodes(&features, scale);
    // The tunnel-flagged ways that are really arcades through a building: they are
    // on the ground and are drawn here, while the rest are tunnels and belong to
    // `crate::tunnels`, which draws them under it.
    let passages = crate::tunnels::building_passages(tile, &features);
    // Rungs for the lines that could otherwise land coplanar, decided across the
    // whole tile before any of them is built — see `assign_road_ties`.
    let ties = assign_road_ties(&features, scale);

    // (group, positions, uvs, indices, rungs) — see `SurfaceMesh::rungs`.
    type RoadBuilder = (
        SurfaceGroup,
        Vec<[f32; 3]>,
        Vec<[f32; 2]>,
        Vec<u32>,
        Vec<f32>,
    );
    let mut builders: Vec<RoadBuilder> = Vec::new();
    // Groups cut loose mid-tile, over the frame's upload budget. See the sweep in
    // the emit loop at the bottom.
    let mut full: Vec<SurfaceMesh> = Vec::new();
    let slot_for = |builders: &mut Vec<RoadBuilder>, group: SurfaceGroup| match builders
        .iter()
        .position(|(g, ..)| *g == group)
    {
        Some(i) => i,
        None => {
            builders.push((group, Vec::new(), Vec::new(), Vec::new(), Vec::new()));
            builders.len() - 1
        }
    };
    // Every ribbon is laid into a buffer of its own first, because the thing that
    // settles a junction now is `road_stitch`, and it can only cut a ribbon it can
    // still tell apart from its neighbours. They are merged into the per-group
    // meshes after the cut.
    let mut staged: Vec<crate::road_stitch::Ribbon> = Vec::new();
    // …and the stretches that ride a deck, one bucket per deck. They are a
    // separate sheet at a separate height, so they are cut among themselves and
    // against their own deck, never against the ground.
    let mut staged_on_deck: Vec<Vec<crate::road_stitch::Ribbon>> =
        (0..decks.len()).map(|_| Vec::new()).collect();
    // The ground stretches that run up to a deck — see [`Approach`].
    let mut approaches: Vec<Approach> = Vec::new();

    for (fi, feature) in features.iter().enumerate() {
        // A tunnel is refused by `drawn_street` because it is not on the ground —
        // unless it is one of the arcades, which are.
        let shape = match drawn_street(feature) {
            Some(shape) => Some(shape),
            None if passages.contains(&fi) => street_shape(feature),
            None => None,
        };
        let Some((class, half)) = shape else {
            continue;
        };
        // Over a cell whose paving was surveyed, the footways, cycle paths and
        // pedestrian streets are drawn from that survey as real shapes
        // (`crate::paving`), so the constant-width ribbon for the same ground must
        // not also be drawn. The test is on the *kind*, not on the class: a
        // pedestrian street is a `RoadClass::Minor`, so a class-level rule left it
        // paving squares in tarmac. Everything else on the vehicle network stays
        // with OSM, marked and cased, everywhere.
        if cover.supersedes(kind_of(feature).unwrap_or("")) {
            continue;
        }
        let bridge = flag_of(feature, "bridge");
        // Traffic runs against the way's own direction, so the across-road axis
        // has to be mirrored before the (asymmetric) carriageway texture lands on
        // it. See the flip at the ribbon below.
        let reversed = flag_of(feature, "oneway_reverse");
        // Kerbed classes get a casing ribbon under the fill; paths and rails
        // have no kerb to draw.
        let cased = !matches!(class, RoadClass::Path | RoadClass::Rail);

        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            // The `streets` layer is lines; a stray polygon belongs to
            // `street_polygons`, which the raster path fills.
            _ => continue,
        }

        for (li, ls) in lines.into_iter().enumerate() {
            let pts: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x, c.y])
                    .collect();
            let (Some(head), Some(tail)) = (pts.first().copied(), pts.last().copied()) else {
                continue;
            };
            // A roundabout, turning circle or loop road closes on itself. Its
            // start and end are one node that the road runs *through*, so it
            // takes no junction stub — an extension there fires a flap of
            // tarmac off along the tangent — and its ribbon closes instead of
            // meeting itself with two square ends.
            let closed = pts.len() >= 4 && (head[0] - tail[0]).hypot(head[1] - tail[1]) <= 1e-4;
            // The tie on the anti-z-fight ladder (see `road_rung`) — per
            // *linestring*, so the crossing parts of one merged feature separate.
            let tie = ties
                .get(&(fi, li))
                .copied()
                .map_or_else(|| line_tie_seed(head, tail), u64::from);
            // The rung is a *depth* offset, not a height: the ribbon itself lies
            // flat on the road plane and `systems::road_depth` biases it in the
            // vertex stage. Lifting it geometrically is what used to stop working
            // past a couple of hundred metres — see `road_rung`.
            let rung = road_rung(class, half * 2.0, bridge, tie) as f32;
            // The casing's own rung, at the foot of the same class block rather
            // than on a separate plane under every fill in the tile.
            let casing_rung = road_casing_rung(class, bridge) as f32;
            // The line's outward direction at each end (tile units — uniform
            // scale, so angles match the metre frame), for the crossing-angle
            // term in `junction_extent`.
            let out_dir = |from: [f32; 2], to: [f32; 2]| -> [f32; 2] {
                let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
                let len = dx.hypot(dy);
                if len > 1e-6 {
                    [dx / len, dy / len]
                } else {
                    [0.0, 0.0]
                }
            };
            let head_out = (pts.len() >= 2).then(|| out_dir(pts[1], pts[0]));
            let tail_out = (pts.len() >= 2).then(|| out_dir(pts[pts.len() - 2], tail));
            for run in clip_polyline(&pts, 0.0, extent) {
                // Only the run holding an *original* end of the line can be at a
                // junction; the rest begin and end at the tile border, where the
                // neighbour's copy of the same street abuts this one exactly and
                // an extension would draw the same tarmac twice.
                //
                // A continuation takes precedence over a stub: where the street
                // runs on through the node as another feature there is no notch
                // to fill, and the mitred cap is the join.
                let end_join = |at: [f32; 2], out: Option<Option<[f32; 2]>>| -> EndJoin {
                    let Some(Some(d)) = out else {
                        return EndJoin::default();
                    };
                    match continuation(&nodes, at, fi, d, class, half) {
                        Some(cont) => EndJoin {
                            extend: 0.0,
                            cont: Some(cont),
                        },
                        None => EndJoin {
                            extend: junction_extent(&nodes, at, fi, d),
                            cont: None,
                        },
                    }
                };
                let join_head = end_join(
                    head,
                    (!closed && run.first() == Some(&head)).then_some(head_out),
                );
                let join_tail = end_join(
                    tail,
                    (!closed && run.last() == Some(&tail)).then_some(tail_out),
                );
                // Only an *unclipped* run is still a ring; a clipped one is an
                // ordinary open polyline that happens to come from a loop.
                let run_is_ring = closed && run.len() == pts.len();
                // Tile units → metres, once, before any geometry is built.
                let metres: Vec<[f32; 2]> =
                    run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                // A bridge's span is carried *by* the deck: the same ribbon, at the
                // deck's height. `split_by_deck` says which stretches those are.
                let parts = if bridge && !decks.is_empty() {
                    split_by_deck(&metres, decks)
                } else {
                    vec![(None, metres)]
                };
                // A run that was split at a deck edge now ends *at* the deck, which
                // is not a junction — extending there would push tarmac out past
                // the structure.
                let whole = parts.len() == 1 && parts[0].1.len() == run.len();
                // **The approaches, before `parts` is consumed.** A ground run
                // that neighbours a deck run *in the same street* is the road that
                // climbs onto that structure — which is the one fact nothing
                // downstream could recover, and the reason a raster ramp leaked
                // into every street near a bridge. `split_by_deck` has always
                // known it and only the deck tag survived.
                //
                // Ordered abutment-first, because the ramp is measured from the
                // deck outwards and a caller cannot tell which end of a run met
                // the structure by looking at it.
                for (pi, (on_deck, part)) in parts.iter().enumerate() {
                    // A way that ends on its deck's edge leaves a stretch of
                    // "ground" as long as the rounding. It points nowhere, so it
                    // cannot be followed down the road, and standing in as the
                    // approach it stopped `extend_approaches` from seeding the
                    // real one: the Europaplein's west ramp went unfound.
                    if on_deck.is_some()
                        || part.len() < 2
                        || crate::tunnels::polyline_len(part) < MIN_APPROACH_M
                    {
                        continue;
                    }
                    // The deck is behind this run if the previous part is on one,
                    // and ahead of it if the next part is. Both is a short ground
                    // stretch between two spans, which is an approach to each.
                    let before = pi.checked_sub(1).and_then(|j| parts[j].0);
                    let after = parts.get(pi + 1).and_then(|p| p.0);
                    for (deck, reversed) in [(before, false), (after, true)] {
                        let Some(d) = deck else { continue };
                        let mut points = part.clone();
                        // Abutment first: a run whose deck is *behind* it already
                        // starts there, and one whose deck is ahead ends there.
                        if reversed {
                            points.reverse();
                        }
                        #[allow(clippy::cast_possible_truncation)]
                        approaches.push(Approach {
                            deck: d as u32,
                            points,
                        });
                    }
                }
                for (pi, (on_deck, part)) in parts.into_iter().enumerate() {
                    let ends = if whole {
                        [join_head, join_tail]
                    } else {
                        [EndJoin::default(); 2]
                    };
                    let lay = |sink: &mut Vec<crate::road_stitch::Ribbon>,
                               group: SurfaceGroup,
                               rung: f32,
                               priority: u32,
                               positions: Vec<[f32; 3]>,
                               uvs: Vec<[f32; 2]>,
                               indices: Vec<u32>| {
                        if indices.is_empty() {
                            return;
                        }
                        sink.push(crate::road_stitch::Ribbon {
                            group,
                            rung,
                            // `cut_band` separates the carriageways from the kerb
                            // bands and `road_rung` is the road hierarchy within
                            // them; the rest only has to make the order total, so
                            // that two ribbons which tie on the hierarchy still
                            // have a definite winner rather than a draw the
                            // renderer would have to break.
                            priority: (cut_band(group), priority, fi, li, pi),
                            bridge,
                            positions,
                            uvs,
                            // A road's tone is in its texture, not its vertices.
                            colors: Vec::new(),
                            indices,
                        });
                    };
                    // A stretch on a deck is staged apart from the ground sheet:
                    // it is cut against that deck and against the other ribbons
                    // riding it, never against the roads passing underneath, and
                    // it is lifted with the deck by the caller.
                    let sink: &mut Vec<crate::road_stitch::Ribbon> = match on_deck {
                        Some(d) => &mut staged_on_deck[d],
                        None => &mut staged,
                    };
                    let mut p = Vec::new();
                    let mut u = Vec::new();
                    let mut i = Vec::new();
                    ribbon(
                        &part,
                        half,
                        0.0,
                        class.texture_length_m(),
                        ends,
                        run_is_ring && whole,
                        &mut p,
                        &mut u,
                        &mut i,
                    );
                    // **Put the across-road axis the way the traffic faces.**
                    //
                    // `ribbon` runs `u` from the way's left to its right, which is
                    // the traffic's left-to-right only while the two agree. On an
                    // `oneway_reverse` feature they are opposite, and the
                    // carriageway texture is no longer symmetric: it carries a hard
                    // shoulder on the right. Unflipped, that shoulder lands against
                    // the median on about half the features, which reads as a
                    // texture bug rather than as a direction one.
                    //
                    // Mirroring `u` rather than reversing the geometry keeps every
                    // vertex, index, join and kerb zip exactly where it was — the
                    // texture is symmetric in `v` and the markings are bands, so
                    // there is nothing else across the road for this to disturb.
                    if reversed {
                        for uv in &mut u {
                            uv[0] = 1.0 - uv[0];
                        }
                    }
                    lay(
                        &mut *sink,
                        SurfaceGroup::Road(class),
                        road_depth_rung(bridge),
                        rung as u32,
                        p,
                        u,
                        i,
                    );
                    // No kerb band on a deck: what lies either side of the
                    // carriageway up there is the structure — its verge and its
                    // parapet — and that is the part of the deck the carriageway
                    // is cut out of, not a band this has to draw.
                    if cased && on_deck.is_none() {
                        // The kerb wraps the fill's stub ends too, so the band
                        // runs around the junction opening instead of stopping
                        // square at it. A mitred end needs no such treatment —
                        // the neighbour's kerb is mitred to the same bisector and
                        // meets this one exactly.
                        let ext = ends.map(|e| EndJoin {
                            extend: if e.extend > 0.0 {
                                e.extend + ROAD_CASING_M
                            } else {
                                0.0
                            },
                            cont: e.cont,
                        });
                        let mut p = Vec::new();
                        let mut u = Vec::new();
                        let mut i = Vec::new();
                        kerb_band(
                            &part,
                            half,
                            class.texture_length_m(),
                            ends,
                            ext,
                            run_is_ring && whole,
                            &mut p,
                            &mut u,
                            &mut i,
                        );
                        lay(
                            &mut *sink,
                            SurfaceGroup::RoadCasing(class),
                            road_depth_rung(bridge),
                            casing_rung as u32,
                            p,
                            u,
                            i,
                        );
                    }
                }
            }
        }
    }

    // Cut every ribbon back to the ground no higher-priority ribbon claims. After
    // this the road sheet is a partition: no two ribbons cover one patch, so
    // nothing is left for the depth ladder to arbitrate at ground level.
    crate::road_stitch::stitch(&mut staged);

    // Each deck settles its own little sheet: the ribbons riding it are cut
    // against each other, and the slab is cut against all of them. The deck goes
    // in as the lowest-priority ribbon in that cut, so it loses every patch of
    // ground a carriageway claims and keeps the rest — the verge and the parapet.
    let mut on_deck: Vec<(u32, SurfaceMesh)> = Vec::new();
    for (di, ribbons) in staged_on_deck.iter_mut().enumerate() {
        if ribbons.is_empty() {
            continue;
        }
        let deck = &mut decks[di];
        // Every road on the deck is lifted one *band* so that the deck can sit at
        // the very bottom of the order and lose to all of them. Shifting the whole
        // set by one keeps their order among themselves exactly as `cut_band` and
        // `road_rung` decided it — and it is the only way to be *strictly* below a
        // hierarchy whose own floor is zero.
        for r in ribbons.iter_mut() {
            r.priority.0 += 1;
        }
        let mut sheet = Vec::with_capacity(ribbons.len() + 1);
        sheet.push(crate::road_stitch::Ribbon {
            group: SurfaceGroup::BridgeDeck,
            rung: 0.0,
            // The weakest claim there is: `stitch` gives the ground to the
            // *larger* priority, so this yields to every road above.
            priority: (0, 0, 0, 0, 0),
            // The deck and the ribbons on it are one sheet, and `stitch` only
            // lets a bridge be cut by another bridge — which is exactly the
            // isolation this wants from the roads passing underneath.
            bridge: true,
            // The slab is flat-coloured and untextured, and both attribute sets
            // are optional — `stitch` carries across only what a ribbon actually
            // has.
            uvs: Vec::new(),
            colors: Vec::new(),
            positions: std::mem::take(&mut deck.positions),
            indices: std::mem::take(&mut deck.indices),
        });
        sheet.append(ribbons);
        crate::road_stitch::stitch(&mut sheet);
        let mut sheet = sheet.into_iter();
        let carved = sheet.next().expect("the deck was pushed first");
        deck.positions = carved.positions;
        deck.indices = carved.indices;

        let mut deck_builders: Vec<RoadBuilder> = Vec::new();
        for r in sheet {
            let slot = slot_for(&mut deck_builders, r.group);
            let (_, positions, uvs, indices, rungs) = &mut deck_builders[slot];
            let base = positions.len() as u32;
            positions.extend(r.positions);
            uvs.extend(r.uvs);
            indices.extend(r.indices.iter().map(|i| i + base));
            rungs.resize(positions.len(), r.rung);
        }
        for (group, positions, uvs, indices, rungs) in deck_builders {
            if positions.is_empty() || indices.is_empty() {
                continue;
            }
            on_deck.push((
                di as u32,
                SurfaceMesh {
                    group,
                    positions,
                    uvs,
                    indices,
                    normals: Vec::new(),
                    colors: Vec::new(),
                    rungs,
                },
            ));
        }
    }

    for r in staged {
        let slot = slot_for(&mut builders, r.group);
        let (_, positions, uvs, indices, rungs) = &mut builders[slot];
        let base = positions.len() as u32;
        positions.extend(r.positions);
        uvs.extend(r.uvs);
        indices.extend(r.indices.iter().map(|i| i + base));
        // One rung per vertex this ribbon just appended, leaving every earlier
        // one alone.
        rungs.resize(positions.len(), r.rung);

        // The cut is after the ribbon, not before it — its vertex count is not
        // known until it has been laid — so the bound is "the ceiling plus at most
        // one ribbon", which is what `scatter`'s test asserts. A road cell over
        // Groningen was 4.2 MB in one mesh (2026-08-19).
        if let Some(max) = max_bytes {
            if mesh_upload_bytes(positions.len(), indices.len()) >= max {
                full.push(SurfaceMesh {
                    group: r.group,
                    positions: std::mem::take(positions),
                    uvs: std::mem::take(uvs),
                    indices: std::mem::take(indices),
                    normals: Vec::new(),
                    colors: Vec::new(),
                    rungs: std::mem::take(rungs),
                });
            }
        }
    }

    Ok(RoadTessellation {
        ground: full
            .into_iter()
            .chain(
                builders
                    .into_iter()
                    .filter(|(_, p, _, i, _)| !p.is_empty() && !i.is_empty())
                    .map(|(group, positions, uvs, indices, rungs)| SurfaceMesh {
                        group,
                        positions,
                        uvs,
                        indices,
                        normals: Vec::new(),
                        colors: Vec::new(),
                        rungs,
                    }),
            )
            .collect(),
        on_deck,
        approaches: extend_approaches(tile, tile_m, decks, approaches),
    })
}

/// Longest an approach is followed from its abutment, metres: a tall viaduct's
/// clearance over the steepest grade an approach takes
/// (`road_grade::MAX_APPROACH_GRADE`) with room to spare. Past where the
/// embankment meets the ground the ramp raises nothing, so the extra is free.
const APPROACH_REACH_M: f32 = 160.0;

/// Shortest ground stretch of a bridge-flagged way that counts as its approach,
/// metres: below it the stretch is the rounding where the way meets its deck.
const MIN_APPROACH_M: f32 = 0.5;

/// The approaches, carried on along the roads beyond the structure.
///
/// **OSM's bridge way stops at the abutment**, so the only approach
/// `split_by_deck` could find was the sliver of the same way that happened to
/// overhang the deck outline — measured over four Groningen tiles on
/// 2026-09-23, every one 0.0–0.3 m long, and many decks (Laan 1940's real
/// abutments, the Beneluxweg's, the Emmaviaduct's north end) had none at all. So
/// the embankment ended at its grading's six-sample dilation, about twenty metres
/// out, in a cliff: 8.7 m of it at Laan 1940.
///
/// Each approach, and each bridge-flagged street's own end at a deck that no
/// approach starts from, is followed along the street that carries on from it
/// (the same walk `tunnels::find_approaches` does for a tunnel's ramp): through
/// further ways while exactly one continues, up to [`APPROACH_REACH_M`].
fn extend_approaches(
    tile: &TileData,
    tile_m: f32,
    decks: &[BridgeDeck],
    mut approaches: Vec<Approach>,
) -> Vec<Approach> {
    if decks.is_empty() {
        return approaches;
    }
    let lines = crate::tunnels::surface_lines(tile, tile_m);
    let near = |a: [f32; 2], b: [f32; 2], r: f32| (a[0] - b[0]).hypot(a[1] - b[1]) <= r;
    // The bridge ends at a deck that no approach starts from.
    let mut seeds: Vec<(u32, Vec<[f32; 2]>)> = Vec::new();
    for l in lines.iter().filter(|l| l.bridge) {
        for k in 0..2 {
            if !l.own[k] || l.pts.len() < 2 {
                continue;
            }
            let (p, inner) = if k == 0 {
                (l.pts[0], l.pts[1])
            } else {
                (l.pts[l.pts.len() - 1], l.pts[l.pts.len() - 2])
            };
            // On the deck, or just inside it along the way.
            let (dx, dz) = (inner[0] - p[0], inner[1] - p[1]);
            let len = dx.hypot(dz).max(1e-6);
            let probe = [p[0] + dx / len * 2.0, p[1] + dz / len * 2.0];
            let Some(d) = decks
                .iter()
                .position(|deck| deck.contains(p) || deck.contains(probe))
            else {
                continue;
            };
            if approaches
                .iter()
                .any(|a| a.points.first().is_some_and(|q| near(*q, p, 3.0)))
            {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            seeds.push((d as u32, vec![inner, p]));
        }
    }
    // Seeds go after the approaches the tile already had, and are kept only if
    // the walk found a road: each carries one segment pointing out of the deck,
    // whose inner vertex is dropped once it has served as a direction.
    let first_seed = approaches.len();
    for (deck, pts) in seeds {
        approaches.push(Approach { deck, points: pts });
    }
    let mut keep = vec![true; approaches.len()];

    let mut branches: Vec<Approach> = Vec::new();
    for (ai, a) in approaches.iter_mut().enumerate() {
        let n = a.points.len();
        if n < 2 {
            continue;
        }
        let (tail, before) = (a.points[n - 1], a.points[n - 2]);
        let (dx, dz) = (tail[0] - before[0], tail[1] - before[1]);
        let len = dx.hypot(dz);
        if len < 1e-3 {
            continue;
        }
        let mut paths = Vec::new();
        follow_approach(
            &lines,
            tail,
            [dx / len, dz / len],
            &mut Vec::new(),
            0.0,
            Vec::new(),
            &mut paths,
        );
        let seeded = ai >= first_seed;
        if paths.is_empty() {
            // A seed that found no road to follow is not an approach.
            keep[ai] = !seeded;
            continue;
        }
        if seeded {
            a.points.remove(0);
        }
        // **Every branch of a fork is an approach of its own.** An interchange
        // arm splits into slip roads right at the abutment, and each of them
        // leaves the deck at the deck's height; stopping at the fork gave such
        // an arm a few metres of ramp and then a cliff — 7.75 m at Laan
        // 1940–1945's north arm (2026-09-24).
        let mut paths = paths.into_iter();
        let first = paths.next().expect("non-empty");
        for extra in paths {
            let mut points = a.points.clone();
            points.extend(extra);
            branches.push(Approach {
                deck: a.deck,
                points,
            });
        }
        a.points.extend(first);
    }
    let mut k = keep.into_iter();
    approaches.retain(|_| k.next().unwrap_or(true));
    approaches.extend(branches);
    approaches
}

/// Most roads one approach is followed through, and most paths it may branch
/// into at forks — a bound on the walk, not a property of any real junction.
const APPROACH_WAYS: usize = 6;
const APPROACH_BRANCHES: usize = 4;

/// Follow the street carrying on from `here` along `heading`, through further
/// ways, pushing each complete path found into `out` (the points past `here`).
/// Where more than one way carries on — a fork — every one is followed, up to
/// [`APPROACH_BRANCHES`] paths in all.
fn follow_approach(
    lines: &[crate::tunnels::SurfaceLine],
    here: [f32; 2],
    heading: [f32; 2],
    used: &mut Vec<usize>,
    walked: f32,
    so_far: Vec<[f32; 2]>,
    out: &mut Vec<Vec<[f32; 2]>>,
) {
    let near = |a: [f32; 2], b: [f32; 2], r: f32| (a[0] - b[0]).hypot(a[1] - b[1]) <= r;
    let done = |out: &mut Vec<Vec<[f32; 2]>>, so_far: Vec<[f32; 2]>| {
        if !so_far.is_empty() && out.len() < APPROACH_BRANCHES {
            out.push(so_far);
        }
    };
    if used.len() >= APPROACH_WAYS || walked >= APPROACH_REACH_M - 0.5 {
        done(out, so_far);
        return;
    }
    let candidates: Vec<(usize, Vec<[f32; 2]>)> = lines
        .iter()
        .enumerate()
        .filter(|(i, l)| !l.bridge && !used.contains(i))
        .flat_map(|(i, l)| {
            [0usize, 1].into_iter().filter_map(move |k| {
                let end = if k == 0 {
                    l.pts[0]
                } else {
                    l.pts[l.pts.len() - 1]
                };
                if !l.own[k] || !near(end, here, 1.5) {
                    return None;
                }
                let mut pts = l.pts.clone();
                if k == 1 {
                    pts.reverse();
                }
                Some((i, pts))
            })
        })
        .filter(|(_, pts)| {
            crate::tunnels::leaving(pts)
                .is_some_and(|d| d[0] * heading[0] + d[1] * heading[1] >= 0.82)
        })
        .collect();
    if candidates.is_empty() {
        done(out, so_far);
        return;
    }
    for (i, mut pts) in candidates {
        if out.len() >= APPROACH_BRANCHES {
            return;
        }
        pts[0] = here;
        let pts = crate::tunnels::cut_at(&pts, APPROACH_REACH_M - walked);
        let m = pts.len();
        let step = crate::tunnels::polyline_len(&pts);
        let mut next_heading = heading;
        if m >= 2 {
            let (ex, ez) = (pts[m - 1][0] - pts[m - 2][0], pts[m - 1][1] - pts[m - 2][1]);
            let el = ex.hypot(ez).max(1e-6);
            next_heading = [ex / el, ez / el];
        }
        let end = pts[m - 1];
        let mut path = so_far.clone();
        path.extend(pts.into_iter().skip(1));
        used.push(i);
        // A way cut by the tile's edge carries on in the next tile, which walks
        // it from there: stop here.
        if !lines[i].own[0] || !lines[i].own[1] {
            done(out, path);
        } else {
            follow_approach(lines, end, next_heading, used, walked + step, path, out);
        }
        used.pop();
    }
}

/// What [`tessellate_roads`] hands back: the road sheet that lies on the ground,
/// and the stretches that ride a bridge deck.
///
/// Two lists rather than one because the second cannot be placed yet. A deck's
/// height is not in the tile — it is measured from AHN, per deck, some frames
/// later (`systems::map_geometry::resolve_bridge_decks`) — so the ribbons that
/// belong to it come back tagged with its index and at `y = 0`, for the caller to
/// lift with the slab they were cut out of.
#[derive(Debug, Default)]
pub struct RoadTessellation {
    /// Roads at ground level: one mesh per [`SurfaceGroup`], as before.
    pub ground: Vec<SurfaceMesh>,
    /// `(deck index, mesh)` — the index is into the `decks` slice that was passed
    /// in, whose slabs have had these meshes cut out of them.
    pub on_deck: Vec<(u32, SurfaceMesh)>,
    /// The ground stretches that run up to a deck, one per abutment.
    pub approaches: Vec<Approach>,
}

/// A road that climbs onto a structure: the ground stretch of a bridge-flagged
/// street immediately either side of the span, with the abutment first.
///
/// # What this is for, and why nothing else could supply it
///
/// A deck stands at a measured height and the road that reaches it is drawn flat,
/// so a viaduct ends in a vertical step. Grading the ground back down from the
/// abutment fixes that, and the question it immediately raises is *which* ground —
/// measured over the Groningen fixture, ramping through the whole road corridor
/// raised 18 % of the city's streets for five bridges, because a raster mask is
/// one connected shape and a ramp travels through everything touching it.
///
/// The distinction it needs is not a height and not a level: the approach and the
/// street passing underneath are both at grade and both unflagged
/// (`bridge-heights.md` — "the approach roads that climb to it in reality are
/// ordinary unflagged ways"), so neither `level_of` nor the survey can separate
/// them. What separates them is that the approach is **the same street feature as
/// the span**, which [`split_by_deck`] establishes and then used to discard.
///
/// Centre lines rather than ribbons, because the consumer rasterises them into a
/// corridor of its own width and a ribbon would have to be un-widened first.
#[derive(Debug, Clone)]
pub struct Approach {
    /// Index into the `decks` slice this run climbs to.
    pub deck: u32,
    /// The run in tile-local metres, **abutment first**. A caller cannot recover
    /// which end met the structure by looking at the geometry, and the ramp is
    /// measured from the deck outwards.
    pub points: Vec<[f32; 2]>,
}

/// Which deck a point stands on, if any.
fn deck_at(decks: &[BridgeDeck], p: [f32; 2]) -> Option<usize> {
    decks.iter().position(|d| d.contains(p))
}

/// How often [`split_by_deck`] asks which deck a segment is over, metres.
const DECK_PROBE_M: f32 = 1.0;

/// How many times the deck boundary is bisected for. Twelve halvings put the seam
/// within a fifty-metre segment's ten-thousandth — under a centimetre, which is an
/// order of magnitude finer than the millimetre lattice `road_stitch` then snaps
/// everything to.
const DECK_EDGE_BISECTIONS: u32 = 12;

/// Split a centre line into runs, each tagged with the deck it lies on (`None`
/// for the ones on the ground).
///
/// Per **stretch** rather than per feature: Shortbread merges ways by attribute,
/// so one `bridge` feature can be a chain of spans with ordinary road between
/// them, and treating the whole feature as one would put the approach roads up on
/// the structure.
///
/// The split lands on the deck's own edge, not on the nearest vertex. Vertices are
/// where the *street* bends and a deck edge is where the *structure* stops, so the
/// two coincide only by luck: cutting at the vertex left a stretch of road up to a
/// segment long belonging to neither piece, which was invisible only for as long as
/// the deck was drawn as an opaque slab over the top of it. The crossing is found
/// by bisecting on [`BridgeDeck::contains`] rather than by intersecting the ring,
/// so a hole, a self-touching outline and two decks that abut are all one case.
///
/// **Asked along each segment, not only at its vertices.** A segment that enters
/// and leaves a deck between two vertices used to be missed — both its ends
/// agree — on the grounds that a structure narrower than a vertex spacing has no
/// deck. A deck built from a way ([`street_decks`]) can be exactly that narrow
/// against a *different* way crossing it, and a long straight segment with both
/// ends just off a deck is ordinary in a generalised tile. Every segment is probed
/// each [`DECK_PROBE_M`], and a change of owner between two probes is bisected to
/// the edge as before.
fn split_by_deck(points: &[[f32; 2]], decks: &[BridgeDeck]) -> Vec<(Option<usize>, Vec<[f32; 2]>)> {
    let mut runs: Vec<(Option<usize>, Vec<[f32; 2]>)> = Vec::new();
    let Some(&first) = points.first() else {
        return runs;
    };
    let mut owner = deck_at(decks, first);
    let mut cur: Vec<[f32; 2]> = vec![first];
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = (b[0] - a[0]).hypot(b[1] - a[1]);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps = (len / DECK_PROBE_M).ceil().max(1.0) as usize;
        let mut prev = a;
        for k in 1..=steps {
            #[allow(clippy::cast_precision_loss)]
            let t = k as f32 / steps as f32;
            let probe = if k == steps {
                b
            } else {
                [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
            };
            let next = deck_at(decks, probe);
            if next != owner {
                // The edge between them, shared exactly by the run that ends on it
                // and the run that starts from it — one computation, so the two
                // cannot land apart.
                let edge = deck_edge_between(decks, prev, owner, probe);
                cur.push(edge);
                if cur.len() >= 2 {
                    runs.push((owner, std::mem::take(&mut cur)));
                } else {
                    cur.clear();
                }
                cur.push(edge);
                owner = next;
            }
            prev = probe;
        }
        cur.push(b);
    }
    if cur.len() >= 2 {
        runs.push((owner, cur));
    }
    runs
}

/// The point on `a`–`b` where the deck underfoot stops being `owner`.
fn deck_edge_between(
    decks: &[BridgeDeck],
    a: [f32; 2],
    owner: Option<usize>,
    b: [f32; 2],
) -> [f32; 2] {
    let at = |t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..DECK_EDGE_BISECTIONS {
        let mid = 0.5 * (lo + hi);
        if deck_at(decks, at(mid)) == owner {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at(0.5 * (lo + hi))
}

/// Read a boolean Shortbread property (`bridge`, `tunnel`, `oneway`, `link`),
/// treating a missing or non-boolean value as `false`.
pub(crate) fn flag_of(feature: &Feature, name: &str) -> bool {
    matches!(
        feature.properties.as_ref().and_then(|p| p.get(name)),
        Some(Value::Bool(true))
    )
}

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
pub(crate) mod synth {
    /// A base-128 varint.
    pub fn varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    /// A length-delimited record: the key, the length, the payload.
    pub fn bytes_field(field: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = varint((field << 3) | 2);
        out.extend(varint(payload.len() as u64));
        out.extend_from_slice(payload);
        out
    }

    /// A varint record.
    pub fn varint_field(field: u64, value: u64) -> Vec<u8> {
        let mut out = varint(field << 3);
        out.extend(varint(value));
        out
    }

    /// Zigzag, which is how a geometry parameter is encoded. The left shift is
    /// done on the unsigned bit pattern so that a coordinate near `i32::MAX` —
    /// which is the whole point of the hostile fixtures — is plainly a discarded
    /// bit rather than something a reader has to convince themselves about.
    fn zigzag(value: i32) -> u64 {
        u64::from(((value as u32) << 1) ^ ((value >> 31) as u32))
    }

    /// One layer holding a single line feature through `points`, in tile-local
    /// integer coordinates.
    pub fn line_layer(name: &str, extent: u32, points: &[(i32, i32)]) -> Vec<u8> {
        // `MoveTo` for the first point, then one `LineTo` covering all the rest;
        // parameters are deltas from the running cursor.
        let mut geometry = varint(9);
        let (mut cx, mut cy) = (0, 0);
        for (i, &(x, y)) in points.iter().enumerate() {
            if i == 1 {
                geometry.extend(varint(((points.len() as u64 - 1) << 3) | 2));
            }
            geometry.extend(varint(zigzag(x - cx)));
            geometry.extend(varint(zigzag(y - cy)));
            (cx, cy) = (x, y);
        }
        let mut feature = varint_field(3, 2); // type = LINESTRING
        feature.extend(bytes_field(4, &geometry));
        layer(name, extent, &[feature])
    }

    /// One layer holding a single polygon through `ring` (not repeated at the
    /// end), in tile-local integer coordinates. Clockwise in tile space (y
    /// down) is an exterior ring, as the MVT specification has it.
    pub fn polygon_layer(name: &str, extent: u32, ring: &[(i32, i32)]) -> Vec<u8> {
        let mut geometry = varint(9);
        let (mut cx, mut cy) = (0, 0);
        for (i, &(x, y)) in ring.iter().enumerate() {
            if i == 1 {
                geometry.extend(varint(((ring.len() as u64 - 1) << 3) | 2));
            }
            geometry.extend(varint(zigzag(x - cx)));
            geometry.extend(varint(zigzag(y - cy)));
            (cx, cy) = (x, y);
        }
        geometry.extend(varint(15)); // ClosePath
        let mut feature = varint_field(3, 3); // type = POLYGON
        feature.extend(bytes_field(4, &geometry));
        layer(name, extent, &[feature])
    }

    /// [`line_layer`] with one string tag on the feature — the shape a
    /// Shortbread `streets` feature really has, where the discriminator is a
    /// `kind` **value** in the layer's value table rather than the layer's own
    /// name. [`super::body_mentions_rail`] is the reader that cares.
    pub fn tagged_line_layer(
        name: &str,
        extent: u32,
        (key, value): (&str, &str),
        points: &[(i32, i32)],
    ) -> Vec<u8> {
        let mut geometry = varint(9);
        let (mut cx, mut cy) = (0, 0);
        for (i, &(x, y)) in points.iter().enumerate() {
            if i == 1 {
                geometry.extend(varint(((points.len() as u64 - 1) << 3) | 2));
            }
            geometry.extend(varint(zigzag(x - cx)));
            geometry.extend(varint(zigzag(y - cy)));
            (cx, cy) = (x, y);
        }
        let mut feature = varint_field(3, 2); // type = LINESTRING
                                              // tags: [key index 0, value index 0], packed.
        let mut tags = varint(0);
        tags.extend(varint(0));
        feature.extend(bytes_field(2, &tags));
        feature.extend(bytes_field(4, &geometry));

        let mut out = bytes_field(1, name.as_bytes());
        out.extend(bytes_field(2, &feature));
        out.extend(bytes_field(3, key.as_bytes()));
        // A `Value` message with its `string_value` (field 1) set.
        out.extend(bytes_field(4, &bytes_field(1, value.as_bytes())));
        out.extend(varint_field(5, u64::from(extent)));
        out.extend(varint_field(15, 2));
        out
    }

    /// A layer out of already-encoded features.
    pub fn layer(name: &str, extent: u32, features: &[Vec<u8>]) -> Vec<u8> {
        let mut out = bytes_field(1, name.as_bytes());
        for feature in features {
            out.extend(bytes_field(2, feature));
        }
        out.extend(varint_field(5, u64::from(extent)));
        // Version is `required`, and mvt-reader refuses anything but 1 or 2.
        out.extend(varint_field(15, 2));
        out
    }

    /// A tile out of already-encoded layers.
    pub fn tile(layers: &[Vec<u8>]) -> Vec<u8> {
        layers.iter().flat_map(|l| bytes_field(3, l)).collect()
    }

    /// The allocation bomb: `moves` repetitions of the three bytes
    /// `[MoveTo(1), 0, 0]`, each of which makes mvt-reader retain one vector
    /// sized for the *whole* stream.
    pub fn geometry_bomb(moves: usize) -> Vec<u8> {
        let geometry: Vec<u8> = (0..moves).flat_map(|_| [9u8, 0, 0]).collect();
        let mut feature = varint_field(3, 2);
        feature.extend(bytes_field(4, &geometry));
        tile(&[layer("streets", 4096, &[feature])])
    }
}

/// Parse-then-call shims, for callers that hold only bytes: the fixture tests
/// (which are asserting about one tile in isolation, where a shared [`TileData`]
/// would only be ceremony) and the `TileData`-vs-bytes equivalence test.
///
/// Production code must **not** use these — a `&[u8]` call site is exactly the
/// duplicate parse `TileData` exists to remove.
#[cfg(test)]
mod bytes {
    use super::*;

    pub fn render_mvt(bytes: &[u8], sub: SubTile) -> Result<Vec<u8>, String> {
        super::render_mvt(&TileData::parse(bytes)?, sub)
    }

    pub fn tessellate_surfaces(bytes: &[u8], tile_m: f32) -> Result<Vec<SurfaceMesh>, String> {
        super::tessellate_surfaces(&TileData::parse(bytes)?, tile_m, None)
    }

    /// The **ground** sheet only. A test that wants the stretches riding a deck
    /// asks [`roads_and_decks`] instead, which is also the one that hands back the
    /// carved slabs.
    pub fn tessellate_roads(
        bytes: &[u8],
        tile_m: f32,
        decks: &[BridgeDeck],
    ) -> Result<Vec<SurfaceMesh>, String> {
        let mut decks = decks.to_vec();
        Ok(
            super::tessellate_roads(&TileData::parse(bytes)?, tile_m, &mut decks, None, false)?
                .ground,
        )
    }

    /// The whole answer: the road sheet, the on-deck stretches, and the decks as
    /// the cut left them.
    pub fn roads_and_decks(
        bytes: &[u8],
        tile_m: f32,
        decks: &[BridgeDeck],
    ) -> Result<(RoadTessellation, Vec<BridgeDeck>), String> {
        let mut decks = decks.to_vec();
        let roads =
            super::tessellate_roads(&TileData::parse(bytes)?, tile_m, &mut decks, None, false)?;
        Ok((roads, decks))
    }

    pub fn tessellate_bridges(bytes: &[u8], tile_m: f32) -> Result<Vec<BridgeDeck>, String> {
        super::tessellate_bridges(&TileData::parse(bytes)?, tile_m)
    }

    pub fn extract_street_labels(
        bytes: &[u8],
        tile_m: f32,
    ) -> Result<Vec<StreetLabelLine>, String> {
        super::extract_street_labels(&TileData::parse(bytes)?, tile_m)
    }

    pub fn ocean_mask(bytes: &[u8], sub: SubTile) -> Result<Vec<u8>, String> {
        super::ocean_mask(&TileData::parse(bytes)?, sub)
    }

    pub fn water_shore_field(bytes: &[u8], tile_m: f32) -> Result<Option<ShoreField>, String> {
        super::water_shore_field(&TileData::parse(bytes)?, tile_m)
    }
}

#[cfg(test)]
mod tests {

    /// **The painted road stops square at a tunnel mouth.** The painter does not
    /// draw tunnels, so a round cap on the road that runs into one is a half-disc
    /// of road on the ground over the lid; once the ground over a lid was put back
    /// at street level it showed as an orange semicircle past every portal of
    /// Groningen's ring (2026-09-23). Measured four metres either side of the
    /// eastbound portal: on the road it is road, on the lid it must not be.
    #[test]
    fn a_road_is_painted_square_at_a_tunnel_mouth() {
        let tile = crate::testdata::groningen_ring();
        let (_, y) = crate::testdata::GRONINGEN_RING_TILE;
        let n = 2f64.powi(14);
        let lat = (std::f64::consts::PI * (1.0 - 2.0 * y as f64 / n))
            .sinh()
            .atan();
        #[allow(clippy::cast_possible_truncation)]
        let tile_m = (40_075_016.686 * lat.cos() / n) as f32;
        // Painted as the finest clipmap level paints it, an eighth of the tile
        // at 512 px, where a road is drawn to scale and its cap is metres wide.
        // At the whole-tile scale a trunk road is two pixels and the cap is lost
        // in one of them, which is how this test first passed without the fix.
        let size = 512u32;
        let sub = SubTile {
            factor: 8,
            x: 5,
            y: 1,
        };
        let rgba = render_mvt_culled(&tile, sub, size, tile_m, false).expect("renders");
        let px = |p: [f32; 2]| -> [u8; 3] {
            let f = sub.factor as f32;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let (x, y) = (
                ((p[0] / tile_m * f - sub.x as f32) * size as f32) as usize,
                ((p[1] / tile_m * f - sub.y as f32) * size as f32) as usize,
            );
            let i = (y * size as usize + x) * 4;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        };
        // The eastbound portal and the way its lid runs, from
        // `tunnels::tests::dump_the_fixture_bores` on this tile.
        let portal = [959.0f32, 338.0];
        let inward = [878.0f32, 395.0];
        let (dx, dy) = (inward[0] - portal[0], inward[1] - portal[1]);
        let l = dx.hypot(dy);
        let at = |m: f32| [portal[0] + dx / l * m, portal[1] + dy / l * m];
        let (on_road, on_lid) = (px(at(-4.0)), px(at(4.0)));
        assert_ne!(
            on_road, on_lid,
            "the ground four metres over the lid is painted the colour of the road \
             that runs into the tunnel — a round cap past the mouth"
        );
    }

    /// **A bridge OSM maps only as flagged ways still gets a deck.** The ring
    /// fixture's Europaplein flyover is four `bridge=yes` trunk carriageways with
    /// no `bridges` outline, and was drawn on the ground across the junction it
    /// crosses (reported 2026-09-26). Every bridge-flagged road here must now
    /// stand on a deck along its whole length, and the flyover's must carry its
    /// carriageway and have an approach to ramp.
    #[test]
    fn a_bridge_mapped_only_as_ways_gets_a_deck() {
        let tile = crate::testdata::groningen_ring();
        let tile_m = 1465.3;
        let areas = super::area_decks(&tile, tile_m);
        let mut decks = super::tessellate_bridges(&tile, tile_m).expect("bridges");
        assert!(
            decks.len() > areas.len(),
            "no deck was built from a street: {} decks, {} outlines",
            decks.len(),
            areas.len()
        );

        let meta = tile.layer("streets").expect("streets");
        let extent = TileData::extent(meta);
        let scale = tile_m / extent;
        let mut checked = 0;
        for f in tile.features(meta) {
            if !flag_of(&f, "bridge")
                || !drawn_street(&f)
                    .is_some_and(|(c, _)| !matches!(c, RoadClass::Path | RoadClass::Minor))
            {
                continue;
            }
            let Geometry::LineString(ls) = &f.geometry else {
                continue;
            };
            let pts: Vec<[f32; 2]> = ls.0.iter().map(|c| [c.x, c.y]).collect();
            for run in clip_polyline(&pts, 0.0, extent) {
                let m: Vec<[f32; 2]> = run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                if crate::tunnels::polyline_len(&m) < STREET_DECK_MIN_M {
                    continue;
                }
                // Every metre of it, bar the ends: a way may overhang an outline by
                // up to `STREET_DECK_MIN_M`, and that sliver is left on the ground.
                for s in m.windows(2) {
                    let (a, b) = (s[0], s[1]);
                    let l = (b[0] - a[0]).hypot(b[1] - a[1]);
                    let steps = l.floor() as usize;
                    for k in 1..steps {
                        let t = k as f32 / l;
                        let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                        let near_end = (p[0] - m[0][0]).hypot(p[1] - m[0][1]) < STREET_DECK_MIN_M
                            || (p[0] - m[m.len() - 1][0]).hypot(p[1] - m[m.len() - 1][1])
                                < STREET_DECK_MIN_M;
                        assert!(
                            near_end || decks.iter().any(|d| d.contains(p)),
                            "a bridge street is off every deck at {p:?}"
                        );
                    }
                }
                checked += 1;
            }
        }
        assert!(checked >= 4, "only {checked} bridge streets checked");

        // Europaplein, between the two main carriageways' spans.
        let europaplein = [1247.9, 125.7];
        let di = decks
            .iter()
            .position(|d| d.contains(europaplein))
            .expect("no deck at the Europaplein flyover");
        assert!(
            di >= areas.len(),
            "the flyover's deck is an outline after all"
        );
        // A clean outline: no edge that runs out and straight back. Those are the
        // zero-width spikes a union of abutting rectangles leaves, and the fascia
        // is extruded from every one of them.
        for d in &decks[areas.len()..] {
            for ring in &d.rings {
                let pts = &ring[..ring.len() - 1];
                for k in 0..pts.len() {
                    let (a, b, c) = (pts[k], pts[(k + 1) % pts.len()], pts[(k + 2) % pts.len()]);
                    let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - b[0], c[1] - b[1]]);
                    let back = u[0] * v[0] + u[1] * v[1] < 0.0
                        && (u[0] * v[1] - u[1] * v[0]).abs()
                            < 0.01 * u[0].hypot(u[1]) * v[0].hypot(v[1]);
                    assert!(!back, "a deck's outline doubles back on itself at {b:?}");
                }
            }
        }
        let roads = super::tessellate_roads(&tile, tile_m, &mut decks, None, false).expect("roads");
        assert!(
            roads.on_deck.iter().any(|(d, _)| *d as usize == di),
            "no carriageway rides the flyover"
        );
        // Both of the N7's own carriageways ride it, not only the slip lane.
        // The midpoints of the westbound and eastbound spans, tile-local.
        let c = [europaplein[0] + 6.4, europaplein[1] + 3.7];
        for (name, mid) in [
            ("westbound", [c[0] - 6.0, c[1] - 3.5]),
            ("eastbound", [c[0] + 2.0, c[1] + 5.5]),
        ] {
            let carried = roads
                .on_deck
                .iter()
                .filter(|(d, _)| *d as usize == di)
                .any(|(_, m)| {
                    m.indices.chunks(3).any(|t| {
                        let p = [0, 1, 2].map(|k| m.positions[t[k] as usize]);
                        let tri = [[p[0][0], p[0][2]], [p[1][0], p[1][2]], [p[2][0], p[2][2]]];
                        let side = |a: [f32; 2], b: [f32; 2]| {
                            (b[0] - a[0]) * (mid[1] - a[1]) - (b[1] - a[1]) * (mid[0] - a[0])
                        };
                        let (s0, s1, s2) = (
                            side(tri[0], tri[1]),
                            side(tri[1], tri[2]),
                            side(tri[2], tri[0]),
                        );
                        (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0)
                            || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0)
                    })
                });
            assert!(
                carried,
                "the N7 {name} runs under its own deck instead of on it"
            );
        }
        let ramp = roads
            .approaches
            .iter()
            .filter(|a| a.deck as usize == di)
            .map(|a| crate::tunnels::polyline_len(&a.points))
            .fold(0.0f32, f32::max);
        assert!(
            ramp >= 40.0,
            "the flyover's longest approach is {ramp:.1} m"
        );
    }

    /// **A segment that crosses a deck between two vertices is split onto it.**
    /// Both ends are off the deck, so asked only at its vertices the whole
    /// segment read as ground.
    #[test]
    fn a_segment_crossing_a_deck_between_its_vertices_rides_it() {
        let ring = vec![
            [0.0, -5.0],
            [60.0, -5.0],
            [60.0, 5.0],
            [0.0, 5.0],
            [0.0, -5.0],
        ];
        let deck = BridgeDeck {
            positions: Vec::new(),
            indices: Vec::new(),
            rings: vec![ring],
            min: [0.0, -5.0],
            max: [60.0, 5.0],
        };
        for line in [[[-10.0, 0.0], [70.0, 0.0]], [[70.0, 0.0], [-10.0, 0.0]]] {
            let on: f32 = split_by_deck(&line, std::slice::from_ref(&deck))
                .iter()
                .filter(|(o, _)| o.is_some())
                .map(|(_, p)| crate::tunnels::polyline_len(p))
                .sum();
            assert!(on > 59.0, "only {on:.1} m of a 60 m span is on its deck");
        }
    }

    /// **A bridge's own walls are not drawn standing on the road under it.** The
    /// Europaplein viaduct's screens measured 7–9.5 m from the Europaweg to their
    /// top and its parapets were unmeasured 48 m walls along the deck; both stood
    /// on the road the deck crosses (2026-09-26). A measured metre on a deck is a
    /// canal bridge's parapet and stays, and so does a low wall beside a ramp.
    #[test]
    fn a_bridge_keeps_its_walls_off_the_road_beneath() {
        // A 60 × 30 m deck along x, and a ramp running on east from it.
        let ring = vec![
            [0.0, 0.0],
            [60.0, 0.0],
            [60.0, 30.0],
            [0.0, 30.0],
            [0.0, 0.0],
        ];
        let deck = BridgeDeck {
            positions: Vec::new(),
            indices: Vec::new(),
            rings: vec![ring],
            min: [0.0, 0.0],
            max: [60.0, 30.0],
        };
        let decks = [deck];
        let ramp = [Approach {
            deck: 0,
            points: vec![[60.0, 15.0], [220.0, 15.0]],
        }];
        let along_deck = [[0.0, 1.0], [60.0, 1.0]];
        let along_ramp = [[40.0, 5.0], [220.0, 5.0]];
        let elsewhere = [[0.0, 80.0], [60.0, 80.0]];
        // Beside the deck, past the drawn carriageway: a wide viaduct's parapet.
        let beside_deck = [[0.0, -9.0], [60.0, -9.0]];

        assert!(wall_of_a_bridge(&decks, &ramp, &along_deck, None));
        assert!(wall_of_a_bridge(&decks, &ramp, &along_deck, Some(9.5)));
        assert!(wall_of_a_bridge(&decks, &ramp, &beside_deck, None));
        assert!(
            !wall_of_a_bridge(&decks, &ramp, &along_deck, Some(1.0)),
            "a measured parapet on a deck stays"
        );
        assert!(wall_of_a_bridge(&decks, &ramp, &along_ramp, Some(7.0)));
        assert!(
            wall_of_a_bridge(&decks, &ramp, &along_ramp, None),
            "an unmeasured parapet running on past the bridge way is the bridge's"
        );
        assert!(!wall_of_a_bridge(&decks, &ramp, &along_ramp, Some(1.2)));
        assert!(wall_of_a_bridge(&decks, &ramp, &along_ramp, Some(2.4)));
        assert!(!wall_of_a_bridge(&decks, &ramp, &elsewhere, Some(9.0)));
        assert!(!wall_of_a_bridge(&[], &[], &along_deck, None));
    }

    /// **A bridge's approach runs along the road beyond its abutment**, not only
    /// along the sliver of the bridge way that overhangs the deck: OSM's bridge
    /// way stops at the abutment, and every approach found that way over four
    /// Groningen tiles was 0.0–0.3 m long (2026-09-23), so each embankment ended
    /// in a cliff. Over the fixture at least one approach must now reach well
    /// past the grading's own twenty-metre dilation.
    #[test]
    fn a_bridge_approach_follows_the_road_past_the_abutment() {
        let tile = crate::testdata::groningen();
        let tile_m = 1471.0;
        let mut decks = super::tessellate_bridges(&tile, tile_m).expect("bridges tessellate");
        assert!(
            !decks.is_empty(),
            "the fixture has no bridge deck to approach"
        );
        let roads = super::tessellate_roads(&tile, tile_m, &mut decks, None, false)
            .expect("roads tessellate");
        let longest = roads
            .approaches
            .iter()
            .map(|a| crate::tunnels::polyline_len(&a.points))
            .fold(0.0f32, f32::max);
        assert!(
            longest >= 40.0,
            "the longest approach is {longest:.1} m — still the overhanging sliver"
        );
        for a in &roads.approaches {
            assert!((a.deck as usize) < decks.len());
            assert!(
                crate::tunnels::polyline_len(&a.points) <= APPROACH_REACH_M + 1.0,
                "an approach ran past the reach"
            );
        }
    }

    /// **A fork past the abutment is followed down every branch.** An arm leaves
    /// the deck heading east and splits 20 m on into two slip roads, one bearing
    /// north-east and one south-east; a road turning off at a right angle is not
    /// the approach. Stopping at the fork graded 20 m of ramp and left the rest
    /// of a 7 m arm as a cliff.
    #[test]
    fn an_approach_is_followed_down_both_branches_of_a_fork() {
        let line = |pts: Vec<[f32; 2]>| crate::tunnels::SurfaceLine {
            pts,
            own: [true, true],
            bridge: false,
        };
        let lines = vec![
            line(vec![[0.0, 0.0], [20.0, 0.0]]),
            line(vec![[20.0, 0.0], [80.0, 25.0]]),
            line(vec![[20.0, 0.0], [80.0, -25.0]]),
            line(vec![[20.0, 0.0], [20.0, 60.0]]),
        ];
        let mut paths = Vec::new();
        super::follow_approach(
            &lines,
            [0.0, 0.0],
            [1.0, 0.0],
            &mut Vec::new(),
            0.0,
            Vec::new(),
            &mut paths,
        );
        assert_eq!(paths.len(), 2, "{paths:?}");
        let ends: Vec<[f32; 2]> = paths.iter().map(|p| *p.last().unwrap()).collect();
        assert!(
            ends.contains(&[80.0, 25.0]) && ends.contains(&[80.0, -25.0]),
            "{ends:?}"
        );
    }

    /// The bridge-flagged streets, decks and approaches of one tile within
    /// `CARTO_BRIDGE_NEAR` metres (default 150) of `CARTO_BRIDGE_AT=lat,lng`, in
    /// tile-local metres — for an approach step `carto-view --site` reports, which
    /// says where it probed but not what the tile holds there.
    ///
    /// ```text
    /// CARTO_BRIDGE_TILE=/tmp/t.mvt CARTO_BRIDGE_XYZ=14/8489/5321 CARTO_BRIDGE_AT=53.2126,6.5422 \
    ///     cargo test -p cartopolis_geo dump_the_bridges_near -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a diagnostic: needs CARTO_BRIDGE_TILE, CARTO_BRIDGE_XYZ and CARTO_BRIDGE_AT"]
    fn dump_the_bridges_near() {
        let bytes =
            std::fs::read(std::env::var("CARTO_BRIDGE_TILE").expect("tile")).expect("reads");
        let tile = TileData::parse(&bytes).expect("parses");
        let v: Vec<f64> = std::env::var("CARTO_BRIDGE_XYZ")
            .expect("xyz")
            .split('/')
            .filter_map(|p| p.parse().ok())
            .collect();
        let (z, x, y) = (v[0], v[1], v[2]);
        let at: Vec<f64> = std::env::var("CARTO_BRIDGE_AT")
            .expect("at")
            .split(',')
            .filter_map(|p| p.parse().ok())
            .collect();
        let near: f32 = std::env::var("CARTO_BRIDGE_NEAR")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(150.0);
        let n = 2f64.powf(z);
        let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
        let tile_m = (40_075_016.686 * lat0.cos() / n) as f32;
        let fx = (at[1] + 180.0) / 360.0 * n;
        let fy = (1.0
            - (at[0].to_radians().tan() + 1.0 / at[0].to_radians().cos()).ln()
                / std::f64::consts::PI)
            / 2.0
            * n;
        let p = [
            ((fx - x) * f64::from(tile_m)) as f32,
            ((fy - y) * f64::from(tile_m)) as f32,
        ];
        let close = |q: &[f32; 2]| (q[0] - p[0]).hypot(q[1] - p[1]) <= near;
        println!("tile_m={tile_m:.1} point={p:?}");
        let mut decks = super::tessellate_bridges(&tile, tile_m).expect("bridges");
        for (i, d) in decks.iter().enumerate() {
            if d.rings.iter().flatten().any(close) {
                println!("deck {i}: min {:?} max {:?}", d.min, d.max);
            }
        }
        for l in crate::tunnels::surface_lines(&tile, tile_m) {
            if l.bridge && l.pts.iter().any(close) {
                let f = |q: [f32; 2]| [q[0].round(), q[1].round()];
                println!(
                    "bridge way: {:?} (own {:?}) .. {:?} (own {:?}), {} points",
                    f(l.pts[0]),
                    l.own[0],
                    f(*l.pts.last().unwrap()),
                    l.own[1],
                    l.pts.len()
                );
            }
        }
        let roads = super::tessellate_roads(&tile, tile_m, &mut decks, None, false).expect("roads");
        for a in &roads.approaches {
            if a.points.iter().any(close) {
                let f = |q: [f32; 2]| [q[0].round(), q[1].round()];
                println!(
                    "approach of deck {}: {:?} .. {:?}, {:.0} m",
                    a.deck,
                    f(a.points[0]),
                    f(*a.points.last().unwrap()),
                    crate::tunnels::polyline_len(&a.points)
                );
            }
        }
    }

    /// Where every bridge deck in a tile is, **on Earth**.
    ///
    /// The sibling of `tunnels::tests::dump_the_fixture_bores`, and it exists for
    /// the same reason: `docs/grade-shots.toml`'s own header records that half
    /// its first bridge cameras missed, one of them photographing the city centre
    /// with the viaduct behind it, and re-aiming them by hand missed again on
    /// 2026-09-22. A deck's position is a thing the tiles know exactly, so it
    /// should be printed rather than estimated off a map.
    ///
    /// ```text
    /// CARTO_BRIDGE_TILE=/tmp/t.mvt CARTO_BRIDGE_XYZ=14/8490/5321 \
    ///     cargo test -p cartopolis_geo dump_the_bridge_decks -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "diagnostic: prints where the tile's viaducts are"]
    fn dump_the_bridge_decks() {
        let owned = std::env::var("CARTO_BRIDGE_TILE")
            .ok()
            .map(|p| std::fs::read(p).expect("tile reads"));
        let tile = match &owned {
            Some(b) => TileData::parse(b).expect("tile parses"),
            None => crate::testdata::groningen(),
        };
        let key: Option<(f64, f64, f64)> = std::env::var("CARTO_BRIDGE_XYZ").ok().and_then(|s| {
            let v: Vec<f64> = s.split('/').filter_map(|p| p.parse().ok()).collect();
            (v.len() == 3).then(|| (v[0], v[1], v[2]))
        });
        // The tile's own ground size, derived from its coordinates rather than
        // written down — the same care the tunnel dump takes.
        let tile_m = key.map_or(1471.0, |(z, _, y)| {
            let n = 2f64.powf(z);
            let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
            (40_075_016.686 * lat0.cos() / n) as f32
        });
        let decks = super::tessellate_bridges(&tile, tile_m).expect("bridges tessellate");
        println!("decks={}", decks.len());
        let mut sorted: Vec<&BridgeDeck> = decks.iter().collect();
        // Biggest first: a viaduct worth photographing is a large structure, and a
        // tile is mostly twenty-metre canal bridges.
        sorted.sort_by(|a, b| {
            let area = |d: &BridgeDeck| (d.max[0] - d.min[0]) * (d.max[1] - d.min[1]);
            area(b).total_cmp(&area(a))
        });
        for (i, d) in sorted.iter().take(8).enumerate() {
            let (cx, cz) = ((d.min[0] + d.max[0]) * 0.5, (d.min[1] + d.max[1]) * 0.5);
            let here = key.map_or(String::new(), |(z, x, y)| {
                let n = 2f64.powf(z);
                let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
                let span = 40_075_016.686 * lat0.cos() / n;
                let (fx, fy) = (x + f64::from(cx) / span, y + f64::from(cz) / span);
                let lat = (std::f64::consts::PI * (1.0 - 2.0 * fy / n))
                    .sinh()
                    .atan()
                    .to_degrees();
                format!("  at {lat:.6},{:.6}", fx / n * 360.0 - 180.0)
            });
            println!(
                "deck {i}: {:.0}x{:.0} m{here}",
                d.max[0] - d.min[0],
                d.max[1] - d.min[1]
            );
        }
    }
    use super::bytes::{
        extract_street_labels, ocean_mask, render_mvt, roads_and_decks, tessellate_bridges,
        tessellate_roads, tessellate_surfaces, water_shore_field,
    };
    use super::*;

    // A real z14 tile over Groningen, from the VersaTiles Shortbread source
    // (now self-hosted at tiles.cartopolis.org; the archive is identical).
    use crate::testdata::GRONINGEN_Z14;

    /// A `TileData` shared by several consumers must produce byte-for-byte what
    /// each of them produced from its own parse. This is the whole safety claim
    /// of step 2 — the outputs feed the map's pixels, its meshes and the globe's
    /// ocean mask, and a wrong hand-off shows up as missing water or roads, which
    /// only a screenshot would otherwise catch.
    #[test]
    fn one_shared_parse_matches_a_parse_per_call() {
        let tile_m = 1500.0;
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse once");

        assert_eq!(
            super::render_mvt(&tile, SubTile::WHOLE).unwrap(),
            render_mvt(GRONINGEN_Z14, SubTile::WHOLE).unwrap(),
        );
        assert_eq!(
            super::ocean_mask(&tile, SubTile::WHOLE).unwrap(),
            ocean_mask(GRONINGEN_Z14, SubTile::WHOLE).unwrap(),
        );
        let same = |a: &[SurfaceMesh], b: &[SurfaceMesh]| {
            assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(b) {
                assert_eq!(x.group, y.group);
                assert_eq!(x.positions, y.positions);
                assert_eq!(x.indices, y.indices);
                assert_eq!(x.uvs, y.uvs);
            }
        };
        same(
            &super::tessellate_surfaces(&tile, tile_m, None).unwrap(),
            &tessellate_surfaces(GRONINGEN_Z14, tile_m).unwrap(),
        );
        same(
            &super::tessellate_roads(&tile, tile_m, &mut [], None, false)
                .unwrap()
                .ground,
            &tessellate_roads(GRONINGEN_Z14, tile_m, &[]).unwrap(),
        );
        assert_eq!(
            super::water_shore_field(&tile, tile_m)
                .unwrap()
                .unwrap()
                .data,
            water_shore_field(GRONINGEN_Z14, tile_m)
                .unwrap()
                .unwrap()
                .data,
        );
        let trees = crate::scatter::scatter_vegetation(&tile, tile_m, 7).unwrap();
        let fresh =
            crate::scatter::scatter_vegetation(&TileData::parse(GRONINGEN_Z14).unwrap(), tile_m, 7)
                .unwrap();
        assert_eq!(trees.len(), fresh.len(), "vegetation scatter diverged");

        // …and asking twice for the same layer really is memoised, not just
        // consistent: the paint order visits `streets` and `buildings` twice.
        let streets = tile.layer("streets").expect("fixture has streets");
        assert!(std::ptr::eq(
            tile.features(streets).as_ptr(),
            tile.features(streets).as_ptr(),
        ));
    }

    /// The same shape, at a size a real feature could have, still parses — which
    /// is what proves the guard is discriminating and, incidentally, that the
    /// test encoder emits a tile mvt-reader accepts at all.
    #[test]
    fn a_small_synthetic_tile_still_parses() {
        let tile = TileData::parse(&synth::geometry_bomb(100)).expect("parse");
        let meta = tile.layer("streets").expect("the synthetic layer");
        assert_eq!(TileData::extent(meta), 4096.0);
        assert_eq!(tile.features(meta).len(), 1);
    }

    #[test]
    fn an_oversized_body_is_refused_by_length_alone() {
        let Err(err) = TileData::parse(&vec![0u8; MAX_TILE_BYTES + 1]) else {
            panic!("oversized parsed");
        };
        assert!(err.contains("mvt body of"), "wrong refusal: {err}");
    }

    /// Two live **low-zoom** tiles must decode.
    ///
    /// This is the regression the capacity guard caused and the reason it was
    /// reverted: z10 geometry is one huge multi-part coastline, which the walker
    /// scored as a bomb, so the coarse basemap came up with holes in it and
    /// `refusing the tile` in the log. Real bytes from `tiles.cartopolis.org`, kept
    /// as fixtures so a future guard has to pass them before it ships.
    #[test]
    fn low_zoom_tiles_decode() {
        for (name, body) in crate::testdata::LOWZOOM_TILES {
            let tile = match TileData::parse(body) {
                Ok(t) => t,
                Err(e) => panic!("{name} must decode — the live basemap serves it: {e}"),
            };
            assert!(
                tile.layer("water").is_some() || tile.layer("land").is_some(),
                "{name} decoded but carries none of the layers a basemap tile has"
            );
        }
    }

    #[test]
    fn fixture_has_expected_shortbread_layers() {
        let reader = Reader::new(GRONINGEN_Z14.to_vec()).expect("parse fixture");
        let names = reader.get_layer_names().expect("layer names");
        for expected in ["streets", "buildings", "water_polygons", "land"] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
    }

    #[test]
    fn renders_a_non_blank_tile() {
        let rgba = render_mvt(GRONINGEN_Z14, SubTile::WHOLE).expect("render");
        assert_eq!(rgba.len() as u32, TILE_PX * TILE_PX * 4);

        // A city tile must not come out as flat background: count distinct colours.
        let mut seen = std::collections::HashSet::new();
        for px in rgba.chunks_exact(4) {
            seen.insert((px[0], px[1], px[2]));
            if seen.len() > 8 {
                break;
            }
        }
        assert!(
            seen.len() > 8,
            "tile rendered nearly flat ({} colours) — style or geometry is wrong",
            seen.len()
        );
        assert!(
            rgba.chunks_exact(4).all(|p| p[3] == 255),
            "alpha must be opaque"
        );
    }

    /// A street is painted at its real width at every magnification, not only at
    /// the one where tile units and pixels happen to coincide.
    ///
    /// The ratio test below cannot see this: it compares the raster against
    /// itself at two sizes, and a width in the wrong units is wrong at both. The
    /// widths went through tiny-skia as path units — 4096 to a tile — so a whole
    /// z14 tile drew a 7 m street a third of a pixel wide and the basemap that
    /// stands in for the road meshes above 500 m had no roads in it.
    #[test]
    fn a_street_is_painted_at_its_real_width_at_every_zoom() {
        let street = synth::tile(&[synth::tagged_line_layer(
            "streets",
            4096,
            ("kind", "residential"),
            &[(-100, 1800), (4196, 1800)],
        )]);
        let real_px = road_width_m("residential", StreetFlags::default()) * TILE_PX as f32
            / NOMINAL_Z14_TILE_M;
        // The tarmac, told apart from the paper and from the paler kerb casing
        // around it by the red channel's midpoint between tarmac and casing.
        let tarmac = RoadClass::Minor.flat_rgb() >> 16;
        let casing = palette::ROAD_CASING >> 16;
        let dark = (tarmac + casing) / 2;
        for (factor, y) in [(1u32, 0u32), (4, 1), (8, 3)] {
            let sub = SubTile { factor, x: 0, y };
            let rgba = render_mvt(&street, sub).expect("render");
            let column = TILE_PX as usize / 2;
            let painted = (0..TILE_PX as usize)
                .filter(|row| u32::from(rgba[(row * TILE_PX as usize + column) * 4]) < dark)
                .count() as f32;
            let want = real_px * factor as f32;
            assert!(
                (painted - want).abs() <= 1.5,
                "at {factor}× the street is {painted} px of tarmac, its real width is {want:.1} px"
            );
        }
    }

    /// Halving the raster must not double every road.
    ///
    /// Stroke widths are in **pixels**, while the texture is stretched over the
    /// same ground whatever its size — so a smaller raster with unscaled widths
    /// draws the same city with roads twice as wide, and the globe (which is the
    /// only caller that asks for a different size) would come up looking like a
    /// different map style from the flat tiles it hands off to.
    ///
    /// Ink coverage is the measure that catches it: the fraction of the tile that
    /// is not background is a scale-free property of the *design*, so it must
    /// survive a change of raster size. It would climb by roughly the size ratio
    /// if `wf` went back to being `f`.
    #[test]
    fn stroke_widths_scale_with_the_raster_size() {
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse");
        let paper = (
            ((palette::PAPER >> 16) & 0xFF) as u8,
            ((palette::PAPER >> 8) & 0xFF) as u8,
            (palette::PAPER & 0xFF) as u8,
        );
        let inked = |size: u32| {
            let rgba =
                render_mvt_sized(&tile, SubTile::WHOLE, size, NOMINAL_Z14_TILE_M).expect("render");
            assert_eq!(rgba.len() as u32, size * size * 4, "wrong buffer size");
            let n = rgba
                .chunks_exact(4)
                .filter(|p| (p[0], p[1], p[2]) != paper)
                .count();
            n as f64 / (size * size) as f64
        };

        let full = inked(TILE_PX);
        let half = inked(TILE_PX / 2);
        assert!(full > 0.2, "fixture rendered nearly blank at {TILE_PX} px");
        // Not exact — anti-aliasing over half the samples genuinely shifts a few
        // percent, and sub-pixel strokes land on more partial texels. A doubling
        // of every width is an order of magnitude outside this.
        assert!(
            (half / full - 1.0).abs() < 0.25,
            "ink coverage moved with the raster size ({full:.3} at {TILE_PX} px, \
             {half:.3} at {} px) — stroke widths are not scaling",
            TILE_PX / 2
        );
    }

    #[test]
    fn overzoom_resolves_to_the_z14_ancestor() {
        // z17 tile directly under z14/8490/5319.
        let (src, sub) = SubTile::resolve(17, 8490 * 8 + 3, 5319 * 8 + 5);
        assert_eq!(src, (14, 8490, 5319));
        assert_eq!(
            sub,
            SubTile {
                factor: 8,
                x: 3,
                y: 5
            }
        );

        // At or below the source zoom nothing is magnified.
        let (src, sub) = SubTile::resolve(12, 100, 200);
        assert_eq!(src, (12, 100, 200));
        assert_eq!(sub, SubTile::WHOLE);
    }

    /// An absurd zoom stays absurd rather than overflowing the shift.
    ///
    /// `z` reaches `resolve` from a `TileKey` nothing range-checks at that
    /// boundary, and the unclamped `1i64 << (z - MAX_SOURCE_ZOOM)` panics in debug
    /// the moment `z` reaches 78 — reachable from a malformed tile source, i.e.
    /// from the network.
    #[test]
    fn an_absurd_zoom_clamps_instead_of_overflowing_the_shift() {
        for z in [78, 100, u32::MAX] {
            let (src, sub) = SubTile::resolve(z, 1, 1);
            assert_eq!(src.0, MAX_SOURCE_ZOOM);
            assert!(sub.factor > 0, "z={z} produced a degenerate sub-tile");
        }
    }

    #[test]
    fn overzoomed_render_differs_from_the_whole_tile() {
        let whole = render_mvt(GRONINGEN_Z14, SubTile::WHOLE).expect("render whole");
        let zoomed = render_mvt(
            GRONINGEN_Z14,
            SubTile {
                factor: 8,
                x: 3,
                y: 5,
            },
        )
        .expect("render sub");
        assert_ne!(whole, zoomed, "overzoom transform had no effect");
    }

    /// Culling a feature whose bounds miss the sub-tile window must be invisible
    /// in the output — the whole point is that it is the *same* picture, cheaper.
    ///
    /// This is the real gate on the margin arithmetic: get the stroke reach or the
    /// anti-alias slack wrong and a road or a building edge that should bleed in
    /// from just outside the window goes missing, which on the map reads as a seam
    /// along a tile border and is exactly what a rendering change must not do.
    /// A crop parcel painted into the basemap shows where it lies, stays out of
    /// the rest of the tile, lands in the same place in every band and in an
    /// overzoomed window, and sits *under* the streets and the water.
    #[test]
    fn an_overlay_fill_lands_under_the_streets_and_nowhere_else() {
        let tile = crate::testdata::groningen();
        const RED: u32 = 0xFF_00_00;
        let is_red = |px: &[u8]| px[0] == 0xFF && px[1] == 0 && px[2] == 0;
        let left_half = [OverlayFill {
            rings: vec![vec![[0.0, 0.0], [0.5, 0.0], [0.5, 1.0], [0.0, 1.0]]],
            rgb: RED,
        }];
        let n = TILE_PX as usize;
        for sub in [
            SubTile::WHOLE,
            SubTile {
                factor: 2,
                x: 0,
                y: 1,
            },
        ] {
            let one = render_mvt_banded(
                &tile,
                sub,
                TILE_PX,
                NOMINAL_Z14_TILE_M,
                false,
                1,
                &[],
                &left_half,
            )
            .expect("renders");
            let banded = render_mvt_banded(
                &tile,
                sub,
                TILE_PX,
                NOMINAL_Z14_TILE_M,
                false,
                4,
                &[],
                &left_half,
            )
            .expect("renders");
            let red_in = |rgba: &[u8], x0: usize, x1: usize| {
                (0..n)
                    .flat_map(|y| (x0..x1).map(move |x| (y * n + x) * 4))
                    .filter(|&i| is_red(&rgba[i..i + 4]))
                    .count()
            };
            // The unit square's left half is the whole of this window for the
            // overzoomed one (x = 0 of 2), and its left half for the whole tile.
            let (inside, outside) = if sub.factor == 1 {
                (0..n / 2 - 2, n / 2 + 2..n)
            } else {
                (0..n, 0..0)
            };
            for rgba in [&one, &banded] {
                let covered = red_in(rgba, inside.start, inside.end);
                let area = n * inside.len();
                assert!(
                    covered > area / 10,
                    "{sub:?}: the fill is missing ({covered} of {area})"
                );
                assert!(covered < area, "{sub:?}: the fill went over the streets");
                assert_eq!(
                    red_in(rgba, outside.start, outside.end),
                    0,
                    "{sub:?}: fill outside itself"
                );
            }
            // Banding moves no overlay pixel.
            let differ = one
                .chunks(4)
                .zip(banded.chunks(4))
                .filter(|(a, b)| is_red(a) != is_red(b))
                .count();
            assert!(
                differ < n,
                "{sub:?}: {differ} pixels changed between banded and single"
            );
        }
    }

    #[test]
    fn banding_the_raster_changes_no_geometry() {
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse fixture");
        // The whole tile and two overzoomed windows, so the band offset is
        // exercised against a non-zero sub-tile shift as well as against zero.
        for (factor, x, y) in [(1, 0, 0), (4, 2, 1), (8, 3, 5)] {
            let sub = SubTile { factor, x, y };
            let whole =
                render_mvt_banded(&tile, sub, TILE_PX, NOMINAL_Z14_TILE_M, false, 1, &[], &[])
                    .expect("whole render");
            // 7 is in the list on purpose: it does not divide TILE_PX, so the
            // last band is short and the row arithmetic has to survive it.
            for bands in [2u32, 3, 4, 7] {
                let split = render_mvt_banded(
                    &tile,
                    sub,
                    TILE_PX,
                    NOMINAL_Z14_TILE_M,
                    false,
                    bands,
                    &[],
                    &[],
                )
                .expect("banded render");
                assert_eq!(
                    split.len(),
                    whole.len(),
                    "factor {factor} in {bands} bands produced a different sized raster"
                );

                let max_delta = split
                    .iter()
                    .zip(&whole)
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap_or(0);
                let differing = split.iter().zip(&whole).filter(|(a, b)| a != b).count();
                let stride = (TILE_PX * 4) as usize;
                let worst_row = split
                    .chunks(stride)
                    .zip(whole.chunks(stride))
                    .map(|(a, b)| a.iter().zip(b).filter(|(p, q)| p != q).count())
                    .max()
                    .unwrap_or(0);

                // **Not bit-equality, and the reason is recorded rather than
                // waved at.** Shifting the transform by the band's first row
                // changes the magnitudes the coordinates are computed at, so f32
                // rounds a few edge samples differently and the anti-aliased
                // coverage moves by a step or two. Bleed rows remove the part
                // that is clipping (2,443 bytes to 312 on this fixture); this
                // residue is arithmetic and does not go away with more of them —
                // 128 rows of bleed still left 96, and 256 left 43.
                //
                // What must not happen is geometry going missing at a seam, and
                // that is a different shape entirely: it would be a whole row
                // disagreeing, at full channel range, not four bytes in a
                // thousand at a delta of three. Both are bounded here so the
                // failure is legible either way.
                // 8/255 is not a number picked to make this pass: it is the
                // figure `flush_batch` already records for the paint batching,
                // whose delta against a per-feature raster is likewise all
                // anti-aliased edges. An overzoomed window rounds coarser than
                // the whole tile — factor 4 reaches 5 where factor 1 reaches 3 —
                // because the coordinates are four times larger before they are
                // shifted.
                //
                // This is the weaker of the two checks. Lost geometry is caught
                // by `worst_row` below: a missing road is a long run of pixels
                // in one row at full channel range, not a handful at a delta of
                // five.
                //
                // Raised 8 → 12 on 2026-09-26, when the stroke widths stopped
                // being read as tile units (`flush_batch`). Until then every
                // street in a whole z14 tile was a sub-pixel hairline, so factor 1
                // had no stroke edges at a real width for the shift to round;
                // with them it measured 12 at seven bands, in at most 21 bytes of
                // any row and 399 of the tile. The seam bounds below are
                // unchanged and all hold.
                //
                // Raised 12 → 16 on 2026-09-27, when the land took realistic
                // tones. The residue is a fraction of the contrast across the
                // edge being rounded, and tarmac against a dark aerial green
                // is more contrast than it was against pale paper: the same
                // seven bands measured 15. The seam and row bounds below are
                // unchanged and hold.
                assert!(
                    max_delta <= 16,
                    "factor {factor} in {bands} bands moved a channel by {max_delta}/255 — past anti-aliasing, so a band lost or gained geometry"
                );
                // Scaled by the number of seams, because that is what the residue
                // is made of: one seam costs ~312 bytes on this fixture and six
                // cost ~1,259. An absolute bound would pass at four bands and
                // fail at seven for nothing worth failing over.
                assert!(
                    differing * 2000 <= whole.len() * bands as usize,
                    "factor {factor} in {bands} bands differs in {differing} of {} bytes — more than rounding at {} seams",
                    whole.len(),
                    bands - 1
                );
                assert!(
                    worst_row * 4 <= stride,
                    "factor {factor} in {bands} bands has a row with {worst_row} of                      {stride} bytes differing — that is a seam, not rounding"
                );
            }
        }
    }

    #[test]
    fn culling_off_screen_features_changes_no_pixels() {
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse fixture");
        // Corners, edges and middle, across the overzoom range map_stream uses
        // (z15/z16/z17 over a z14 source → factor 2/4/8).
        for (factor, x, y) in [
            (2, 0, 0),
            (2, 1, 1),
            (4, 0, 3),
            (4, 2, 1),
            (8, 3, 5),
            (8, 7, 7),
            (8, 0, 0),
        ] {
            let sub = SubTile { factor, x, y };
            let culled = render_mvt_culled(&tile, sub, TILE_PX, NOMINAL_Z14_TILE_M, true)
                .expect("culled render");
            let full = render_mvt_culled(&tile, sub, TILE_PX, NOMINAL_Z14_TILE_M, false)
                .expect("unculled render");
            assert_eq!(
                culled.len(),
                full.len(),
                "factor {factor} ({x},{y}): raster size changed"
            );
            // **One count, and no more.** tiny-skia builds a batch's edges by
            // one of two routes — straight, when the whole path lies inside the
            // pixmap (`path_contained_in_clip`), or through its edge clipper
            // otherwise — and the two round a few edges a hair differently.
            // Culling the off-screen members of a batch can move it from the
            // second route to the first. Measured once real stroke widths
            // reached the overzoomed tiles (2026-09-26): one channel of one
            // pixel, by one, from a `bridges` outline wholly outside factor 4
            // (0,3). A feature the cull wrongly dropped is a different shape — a
            // run of pixels at the stroke's full contrast — and still fails.
            //
            // Counted in *pixels*, not in levels: how far that one edge pixel
            // moves is the contrast between the stroke and the ground under it,
            // which is a palette choice. The earthier ground of 2026-09-27 put
            // the same pixel 5 levels off instead of 1.
            let differing = culled
                .chunks_exact(4)
                .zip(full.chunks_exact(4))
                .filter(|(a, b)| a.iter().zip(b.iter()).any(|(p, q)| p.abs_diff(*q) > 1))
                .count();
            assert!(
                differing <= 1,
                "factor {factor} ({x},{y}): culling changed {differing} pixels by more than rounding"
            );
        }
    }

    /// Not a test — a measurement hatch for the overzoom cull, since its whole
    /// justification is a number. `cargo test -p cartopolis_geo bench_overzoom_cull
    /// -- --ignored --nocapture`.
    #[test]
    #[ignore = "printing hatch: it reports numbers, it does not assert them"]
    fn bench_overzoom_cull() {
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse fixture");
        for factor in [2u32, 4, 8] {
            let subs: Vec<SubTile> = (0..factor)
                .flat_map(|x| (0..factor).map(move |y| SubTile { factor, x, y }))
                .collect();
            for (label, cull) in [("cull ", true), ("full ", false)] {
                let t0 = std::time::Instant::now();
                for &sub in &subs {
                    render_mvt_culled(&tile, sub, TILE_PX, NOMINAL_Z14_TILE_M, cull)
                        .expect("render");
                }
                let dt = t0.elapsed();
                println!(
                    "factor {factor:>2} {label} {:>4} tiles  total {:>8.1} ms  per tile {:>6.2} ms",
                    subs.len(),
                    dt.as_secs_f64() * 1000.0,
                    dt.as_secs_f64() * 1000.0 / subs.len() as f64,
                );
            }
        }
    }

    /// Not a test — a measurement hatch answering "which paint pass is the
    /// 121 ms?", timed inside the real loop (`perf::passes`) because the one
    /// prior per-layer attribution was a reconstruction and summed to more than
    /// double the measured total. `cargo test -p cartopolis_geo --release
    /// dump_paint_split -- --ignored --nocapture`.
    #[test]
    #[ignore = "printing hatch: it reports numbers, it does not assert them"]
    fn dump_paint_split() {
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse fixture");
        // Serial: `perf::passes` is thread-local — see `render_mvt_serial`.
        let whole = || {
            render_mvt_banded(
                &tile,
                SubTile::WHOLE,
                TILE_PX,
                NOMINAL_Z14_TILE_M,
                false,
                1,
                &[],
                &[],
            )
        };
        for _ in 0..2 {
            whole().expect("warmup");
        }
        const N: u32 = 50;
        let (_, passes) = crate::perf::passes::measure(|| {
            for _ in 0..N {
                whole().expect("render");
            }
        });
        let loop_total: f64 = passes
            .iter()
            .filter(|(l, _)| !l.starts_with('Σ'))
            .map(|(_, d)| d.as_secs_f64())
            .sum();
        println!("whole 512 px tile, mean of {N}:");
        for (label, d) in &passes {
            let ms = d.as_secs_f64() * 1000.0 / N as f64;
            if label.starts_with('Σ') {
                println!("  {label:<22} {ms:>7.2} ms/tile  (subset of the passes above)");
            } else {
                let pct = 100.0 * d.as_secs_f64() / loop_total;
                println!("  {label:<22} {ms:>7.2} ms/tile  {pct:>5.1}%");
            }
        }
        println!(
            "  {:<22} {:>7.2} ms/tile",
            "paint loop total",
            loop_total * 1000.0 / N as f64
        );
    }

    /// Not a test — the tiny-skia ↔ `vello_cpu` A/B recorded in
    /// `docs/notes/tile-raster-cost.md`. Renders the same whole z14 tile — same
    /// paint order, colours, widths, round caps and joins, same transform —
    /// through both rasterisers and prints ms/tile, plus writes
    /// `/tmp/vt_vello.png` so the output can be eyeballed against
    /// `dump_png`'s. `cargo test -p cartopolis_geo --release bench_vello_cpu
    /// -- --ignored --nocapture`.
    #[test]
    #[ignore = "printing hatch: it reports numbers, it does not assert them"]
    fn bench_vello_cpu() {
        use vello_cpu::color::{AlphaColor, Srgb};
        use vello_cpu::kurbo::{Affine, BezPath, Cap, Join, Stroke as VStroke};
        use vello_cpu::peniko::Fill;
        use vello_cpu::{Pixmap as VPixmap, RenderContext, Resources};

        let tile = TileData::parse(GRONINGEN_Z14).expect("parse fixture");

        let solid = |rgb: u32| {
            AlphaColor::<Srgb>::from_rgba8(
                ((rgb >> 16) & 0xFF) as u8,
                ((rgb >> 8) & 0xFF) as u8,
                (rgb & 0xFF) as u8,
                255,
            )
        };
        fn bez_area(pb: &mut BezPath, geom: &Geometry<f32>) {
            let ring = |pb: &mut BezPath, ls: &geo_types::LineString<f32>| {
                let mut first = true;
                for c in ls.coords() {
                    if !c.x.is_finite() || !c.y.is_finite() {
                        continue;
                    }
                    if first {
                        pb.move_to((c.x as f64, c.y as f64));
                        first = false;
                    } else {
                        pb.line_to((c.x as f64, c.y as f64));
                    }
                }
                if !first {
                    pb.close_path();
                }
            };
            match geom {
                Geometry::Polygon(p) => {
                    ring(pb, p.exterior());
                    p.interiors().iter().for_each(|h| ring(pb, h));
                }
                Geometry::MultiPolygon(mp) => {
                    for p in mp {
                        ring(pb, p.exterior());
                        p.interiors().iter().for_each(|h| ring(pb, h));
                    }
                }
                _ => {}
            }
        }
        fn bez_lines(pb: &mut BezPath, geom: &Geometry<f32>) {
            let line = |pb: &mut BezPath, ls: &geo_types::LineString<f32>| {
                let mut first = true;
                for c in ls.coords() {
                    if !c.x.is_finite() || !c.y.is_finite() {
                        continue;
                    }
                    if first {
                        pb.move_to((c.x as f64, c.y as f64));
                        first = false;
                    } else {
                        pb.line_to((c.x as f64, c.y as f64));
                    }
                }
            };
            match geom {
                Geometry::LineString(ls) => line(pb, ls),
                Geometry::MultiLineString(mls) => mls.iter().for_each(|ls| line(pb, ls)),
                Geometry::Polygon(_) | Geometry::MultiPolygon(_) => bez_area(pb, geom),
                _ => {}
            }
        }

        // The paint loop mirrored for vello_cpu: same style resolution as
        // `render_mvt_culled` on a whole z14 tile (no cull, no far style),
        // batching by (colour, width) run exactly as the real loop does.
        let vello_render = || -> VPixmap {
            let size = TILE_PX as u16;
            let mut ctx = RenderContext::new(size, size);
            let mut resources = Resources::new();
            let mut target = VPixmap::new(size, size);
            ctx.set_paint(solid(palette::PAPER));
            ctx.fill_path(&BezPath::from_vec(vec![
                vello_cpu::kurbo::PathEl::MoveTo((0.0, 0.0).into()),
                vello_cpu::kurbo::PathEl::LineTo((TILE_PX as f64, 0.0).into()),
                vello_cpu::kurbo::PathEl::LineTo((TILE_PX as f64, TILE_PX as f64).into()),
                vello_cpu::kurbo::PathEl::LineTo((0.0, TILE_PX as f64).into()),
                vello_cpu::kurbo::PathEl::ClosePath,
            ]));
            ctx.set_fill_rule(Fill::NonZero);
            for (layer_name, draw) in PAINT_ORDER {
                let Some(meta) = tile.layer(layer_name) else {
                    continue;
                };
                let features = tile.features(meta);
                if features.is_empty() {
                    continue;
                }
                let extent = TileData::extent(meta);
                let scale = (TILE_PX as f32 / extent) as f64;
                ctx.set_transform(Affine::new([scale, 0.0, 0.0, scale, 0.0, 0.0]));

                let street_order: Option<Vec<&Feature>> = matches!(draw, Draw::Streets { .. })
                    .then(|| {
                        let mut v: Vec<&Feature> = features
                            .iter()
                            .filter(|feat| !crate::tunnels::underground(feat))
                            .collect();
                        v.sort_by(|a, b| {
                            layer_of(a)
                                .cmp(&layer_of(b))
                                .then(flag_of(a, "bridge").cmp(&flag_of(b, "bridge")))
                                .then(
                                    street_style(a, NOMINAL_Z14_TILE_M)
                                        .0
                                        .total_cmp(&street_style(b, NOMINAL_Z14_TILE_M).0),
                                )
                        });
                        v
                    });
                let ordered: Vec<&Feature> = match street_order {
                    Some(list) => list,
                    None => features.iter().collect(),
                };

                let mut batch = BezPath::new();
                let mut batch_key: Option<BatchKey> = None;
                let mut batch_points = 0usize;
                let flush =
                    |ctx: &mut RenderContext, batch: &mut BezPath, key: Option<BatchKey>| {
                        let Some((rgb, width_bits, _)) = key else {
                            return;
                        };
                        if batch.elements().is_empty() {
                            return;
                        }
                        ctx.set_paint(solid(rgb));
                        match width_bits {
                            None => ctx.fill_path(batch),
                            Some(bits) => {
                                ctx.set_stroke(
                                    VStroke::new(f32::from_bits(bits) as f64)
                                        .with_caps(Cap::Round)
                                        .with_join(Join::Round),
                                );
                                ctx.stroke_path(batch);
                            }
                        }
                        batch.truncate(0);
                    };
                for feature in ordered {
                    let kind = kind_of(feature);
                    let style: Option<(u32, Option<f32>)> = match draw {
                        Draw::Fill(rgb) => Some((*rgb, None)),
                        Draw::FillByKind { default, kinds } => Some((
                            kind.and_then(|k| kinds.iter().find(|(n, _)| *n == k).map(|(_, c)| *c))
                                .unwrap_or(*default),
                            None,
                        )),
                        Draw::Line { color, width } => Some((*color, Some(*width))),
                        Draw::Outline { color, width } => Some((*color, Some(*width))),
                        Draw::Streets { casing } => {
                            let (w, fill, case) = street_style(feature, NOMINAL_Z14_TILE_M);
                            Some(if *casing {
                                (case, Some(w + 0.5))
                            } else {
                                (fill, Some(w))
                            })
                        }
                    };
                    let Some((rgb, width)) = style else { continue };
                    let key = (rgb, width.map(f32::to_bits), false);
                    let cap = if width.is_some() {
                        STROKE_BATCH_POINTS
                    } else {
                        FILL_BATCH_POINTS
                    };
                    if batch_key != Some(key) || batch_points >= cap {
                        flush(&mut ctx, &mut batch, batch_key);
                        batch_key = Some(key);
                        batch_points = 0;
                    }
                    match width {
                        None => bez_area(&mut batch, &feature.geometry),
                        Some(_) => bez_lines(&mut batch, &feature.geometry),
                    }
                    batch_points += count_points(&feature.geometry, width.is_none());
                }
                flush(&mut ctx, &mut batch, batch_key);
            }
            ctx.flush();
            ctx.render(&mut target, &mut resources);
            target
        };

        let tiny_render =
            || render_mvt_culled(&tile, SubTile::WHOLE, TILE_PX, NOMINAL_Z14_TILE_M, false);

        for _ in 0..2 {
            tiny_render().expect("warmup");
            vello_render();
        }
        const N: u32 = 20;
        let t0 = std::time::Instant::now();
        for _ in 0..N {
            tiny_render().expect("render");
        }
        let tiny_ms = t0.elapsed().as_secs_f64() * 1000.0 / N as f64;
        let t0 = std::time::Instant::now();
        for _ in 0..N {
            vello_render();
        }
        let vello_ms = t0.elapsed().as_secs_f64() * 1000.0 / N as f64;
        println!("tiny-skia (batched): {tiny_ms:.2} ms/tile");
        println!("vello_cpu (batched): {vello_ms:.2} ms/tile");

        let out = vello_render();
        let rgba: Vec<u8> = out
            .data()
            .iter()
            .flat_map(|p| [p.r, p.g, p.b, p.a])
            .collect();
        image::save_buffer(
            "/tmp/vt_vello.png",
            &rgba,
            TILE_PX,
            TILE_PX,
            image::ExtendedColorType::Rgba8,
        )
        .expect("write png");
        println!("wrote /tmp/vt_vello.png");
    }

    /// Not a test — an eyeball hatch. `cargo test -p cartopolis_geo
    /// dump_png -- --ignored --nocapture` writes the fixture at z14 and at an
    /// overzoomed z17 so the styling can actually be looked at.
    #[test]
    #[ignore = "eyeball hatch: it writes PNGs to look at, it asserts nothing"]
    fn dump_png() {
        for (name, sub) in [
            ("/tmp/vt_z14.png", SubTile::WHOLE),
            (
                "/tmp/vt_z17.png",
                SubTile {
                    factor: 8,
                    x: 3,
                    y: 5,
                },
            ),
        ] {
            let rgba = render_mvt(GRONINGEN_Z14, sub).expect("render");
            image::save_buffer(
                name,
                &rgba,
                TILE_PX,
                TILE_PX,
                image::ExtendedColorType::Rgba8,
            )
            .expect("write png");
            println!("wrote {name}");
        }
    }

    /// The other eyeball hatch: print what a tile actually contains — layers,
    /// feature counts, and one feature's attributes. Useful when a layer renders
    /// blank and the question is "is it the style or the data?".
    #[test]
    #[ignore = "eyeball hatch: it prints a tile's contents, it asserts nothing"]
    fn dump_layers() {
        let owned = std::env::var("CARTO_DUMP_MVT")
            .ok()
            .map(|p| std::fs::read(p).expect("read tile"));
        let tile: &[u8] = owned.as_deref().unwrap_or(GRONINGEN_Z14);
        let reader = Reader::new(tile.to_vec()).unwrap();
        let meta = reader.get_layer_metadata().unwrap();
        println!("tile bytes: {}", tile.len());
        if let Some(l) = meta.iter().find(|l| l.name == "bridges") {
            println!("\n`bridges` layer:");
            for f in reader.get_features(l.layer_index).unwrap().iter().take(8) {
                let shape = match &f.geometry {
                    Geometry::LineString(ls) => format!("LineString {} pts", ls.0.len()),
                    Geometry::MultiLineString(m) => format!("MultiLineString {} parts", m.0.len()),
                    Geometry::Polygon(p) => format!("Polygon {} pts", p.exterior().0.len()),
                    Geometry::MultiPolygon(m) => format!("MultiPolygon {} parts", m.0.len()),
                    g => format!("{g:?}"),
                };
                // Deck footprint in metres over a 1000 m tile: is this an outline
                // we could stand a slab on, or a token?
                let s = 1000.0 / 4096.0;
                let mut area = 0.0f32;
                let mut bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
                if let Geometry::MultiPolygon(mp) = &f.geometry {
                    for poly in mp {
                        let r = poly.exterior();
                        for w in r.0.windows(2) {
                            area += (w[0].x * w[1].y - w[1].x * w[0].y) * 0.5;
                        }
                        for c in &r.0 {
                            bbox = [
                                bbox[0].min(c.x),
                                bbox[1].min(c.y),
                                bbox[2].max(c.x),
                                bbox[3].max(c.y),
                            ];
                        }
                    }
                }
                println!(
                    "  {shape} {:.0} m² bbox {:.0}×{:.0} m — {:?}",
                    area.abs() * s * s,
                    (bbox[2] - bbox[0]) * s,
                    (bbox[3] - bbox[1]) * s,
                    f.properties.as_ref().and_then(|p| p.get("name"))
                );
            }
        }
        // Every bridge-flagged street, with the length of its span in tile units
        // (÷ extent × tile metres for real length) — the number a deck height has
        // to be derived from.
        let idx = meta
            .iter()
            .find(|l| l.name == "streets")
            .unwrap()
            .layer_index;
        println!("\nbridge-flagged streets (length in metres over a 1000 m tile):");
        for f in reader.get_features(idx).unwrap().iter() {
            if !flag_of(f, "bridge") {
                continue;
            }
            let len: f32 = match &f.geometry {
                Geometry::LineString(ls) => {
                    ls.0.windows(2)
                        .map(|w| ((w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2)).sqrt())
                        .sum()
                }
                _ => 0.0,
            };
            println!(
                "  {:>12} {:>7.0} m  {:?}",
                kind_of(f).unwrap_or("?"),
                len * 1000.0 / 4096.0,
                drawn_street(f)
            );
        }
        for l in &meta {
            println!(
                "  {:<26} {:>5} features  extent {}",
                l.name, l.feature_count, l.extent
            );
        }
        let idx = meta
            .iter()
            .find(|l| l.name == "streets")
            .unwrap()
            .layer_index;
        for f in reader.get_features(idx).unwrap().iter().take(400) {
            let props = f.properties.as_ref().unwrap();
            if props.contains_key("kind") {
                println!("\nsample feature from `streets`:");
                let mut kv: Vec<_> = props.iter().collect();
                kv.sort_by(|(a, _), (b, _)| a.cmp(b));
                for (k, v) in kv {
                    println!("    {k} = {v:?}");
                }
                if let Geometry::LineString(ls) = &f.geometry {
                    println!("    geometry = LineString, {} points; first 3:", ls.0.len());
                    for c in ls.0.iter().take(3) {
                        println!("        ({}, {})", c.x, c.y);
                    }
                }
                break;
            }
        }
    }

    /// Sizing data for the geometry path (ADR-035 option B): how many vertices and
    /// triangles each layer would cost if tessellated instead of rasterised, and
    /// what the raster path costs in CPU time for comparison.
    #[test]
    #[ignore = "printing hatch: it reports numbers, it does not assert them"]
    fn dump_geometry_cost() {
        let reader = Reader::new(GRONINGEN_Z14.to_vec()).unwrap();
        let meta = reader.get_layer_metadata().unwrap();

        fn count(geom: &Geometry<f32>) -> (usize, usize, usize) {
            // (vertices, earcut-triangles for areas, ribbon-triangles for lines)
            match geom {
                Geometry::Polygon(p) => {
                    let n: usize = p.exterior().0.len()
                        + p.interiors().iter().map(|r| r.0.len()).sum::<usize>();
                    (n, n.saturating_sub(2), 0)
                }
                Geometry::MultiPolygon(mp) => mp
                    .iter()
                    .map(|p| count(&Geometry::Polygon(p.clone())))
                    .fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2)),
                Geometry::LineString(ls) => (ls.0.len(), 0, ls.0.len().saturating_sub(1) * 2),
                Geometry::MultiLineString(mls) => mls
                    .iter()
                    .map(|ls| count(&Geometry::LineString(ls.clone())))
                    .fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2)),
                _ => (0, 0, 0),
            }
        }

        println!(
            "{:<24} {:>8} {:>9} {:>10}",
            "layer", "features", "vertices", "triangles"
        );
        let mut tri_total = 0usize;
        let mut tri_without_buildings = 0usize;
        for l in &meta {
            let feats = reader.get_features(l.layer_index).unwrap();
            let (v, ta, tl) = feats
                .iter()
                .map(|f| count(&f.geometry))
                .fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2));
            let tris = ta + tl;
            tri_total += tris;
            if l.name != "buildings" {
                tri_without_buildings += tris;
            }
            if v > 0 {
                println!(
                    "{:<24} {:>8} {:>9} {:>10}",
                    l.name, l.feature_count, v, tris
                );
            }
        }
        println!("\ntriangles/tile, all layers      : {tri_total}");
        println!("triangles/tile, minus buildings : {tri_without_buildings}");

        // Routing needs stable feature identity across tile seams — check whether
        // the source preserves OSM ids, and how many streets touch a tile edge.
        let sidx = meta
            .iter()
            .find(|l| l.name == "streets")
            .unwrap()
            .layer_index;
        let streets = reader.get_features(sidx).unwrap();
        let with_id = streets.iter().filter(|f| f.id.is_some()).count();
        let touches_edge = streets
            .iter()
            .filter(|f| match &f.geometry {
                Geometry::LineString(ls) => {
                    ls.0.iter()
                        .any(|c| c.x <= 0.0 || c.y <= 0.0 || c.x >= 4096.0 || c.y >= 4096.0)
                }
                Geometry::MultiLineString(mls) => mls.iter().any(|ls| {
                    ls.0.iter()
                        .any(|c| c.x <= 0.0 || c.y <= 0.0 || c.x >= 4096.0 || c.y >= 4096.0)
                }),
                _ => false,
            })
            .count();
        println!("\nstreets with a feature id : {with_id}/{}", streets.len());
        println!(
            "streets clipped at a seam : {touches_edge}/{}",
            streets.len()
        );

        // Raster path CPU cost, for comparison.
        let t0 = std::time::Instant::now();
        const N: u32 = 20;
        for _ in 0..N {
            let _ = render_mvt(GRONINGEN_Z14, SubTile::WHOLE).unwrap();
        }
        println!(
            "\nrasterise {TILE_PX}px tile: {:.1} ms/tile",
            t0.elapsed().as_secs_f64() * 1000.0 / f64::from(N)
        );

        let t0 = std::time::Instant::now();
        for _ in 0..N {
            let r = Reader::new(GRONINGEN_Z14.to_vec()).unwrap();
            for l in r.get_layer_metadata().unwrap() {
                let _ = r.get_features(l.layer_index).unwrap();
            }
        }
        println!(
            "decode all layers        : {:.1} ms/tile",
            t0.elapsed().as_secs_f64() * 1000.0 / f64::from(N)
        );
    }

    /// Over a cell whose paving was surveyed, the footway and cycle ribbons must
    /// not be drawn — drawing them twice puts a constant-width line down the
    /// middle of a real pavement. Every other *class* is OSM's and must survive
    /// the flag: `Minor` stays, because a residential street is still OSM's even
    /// though the pedestrian streets inside that class now go
    /// ([`surveyed_paving_also_drops_the_pedestrian_ribbons`] is what pins those).
    #[test]
    fn surveyed_paving_drops_the_path_ribbons_and_no_whole_class_else() {
        let tile = TileData::parse(GRONINGEN_Z14).expect("fixture");
        let groups = |surveyed: bool| -> Vec<SurfaceGroup> {
            let mut g: Vec<SurfaceGroup> =
                super::tessellate_roads(&tile, 1472.0, &mut [], None, surveyed)
                    .expect("roads")
                    .ground
                    .into_iter()
                    .map(|m| m.group)
                    .collect();
            g.sort_by_key(|x| format!("{x:?}"));
            g.dedup();
            g
        };
        let with_osm = groups(false);
        let with_survey = groups(true);
        assert!(
            with_osm.contains(&SurfaceGroup::Road(RoadClass::Path)),
            "the fixture must have paths to drop, else this proves nothing"
        );
        assert!(!with_survey.contains(&SurfaceGroup::Road(RoadClass::Path)));
        assert!(!with_survey.contains(&SurfaceGroup::RoadCasing(RoadClass::Path)));
        for group in &with_osm {
            if matches!(
                group,
                SurfaceGroup::Road(RoadClass::Path) | SurfaceGroup::RoadCasing(RoadClass::Path)
            ) {
                continue;
            }
            assert!(
                with_survey.contains(group),
                "{group:?} is OSM's and must still be drawn"
            );
        }
    }

    /// A pedestrian street is paving, and the survey draws it — so its ribbon has
    /// to go the same way a footway's does.
    ///
    /// It is **not** a [`RoadClass::Path`], so the class-level rule above missed
    /// it entirely and laid a 5 m tarmac ribbon with wheel tracks and a kerb band
    /// over every surveyed square. Groningen's Grote Markt is mapped as fourteen
    /// separate `pedestrian` ways, so what it drew was a scatter of tarmac stubs
    /// — each one ending where its way ended, kerbed all the way round the cut —
    /// lying on the pink paving the survey had already put there (2026-09-11).
    ///
    /// The assertion is on the **`Minor` vertex count** rather than on the group
    /// list, because that is the only place the loss can show: pedestrian shares
    /// its class with every residential and service street in the tile, so the
    /// group survives and only gets smaller.
    #[test]
    fn surveyed_paving_also_drops_the_pedestrian_ribbons() {
        let tile = TileData::parse(GRONINGEN_Z14).expect("fixture");
        let pedestrian = tile.layer("streets").map_or(0, |meta| {
            tile.features(meta)
                .iter()
                .filter(|f| kind_of(f) == Some("pedestrian"))
                .count()
        });
        assert!(
            pedestrian > 0,
            "the fixture carries no pedestrian street — this test proves nothing"
        );
        let minor_vertices = |surveyed: bool| -> usize {
            super::tessellate_roads(&tile, 1472.0, &mut [], None, surveyed)
                .expect("roads")
                .ground
                .iter()
                .filter(|m| {
                    matches!(
                        m.group,
                        SurfaceGroup::Road(RoadClass::Minor)
                            | SurfaceGroup::RoadCasing(RoadClass::Minor)
                    )
                })
                .map(|m| m.positions.len())
                .sum()
        };
        let with_osm = minor_vertices(false);
        let with_survey = minor_vertices(true);
        assert!(
            with_survey < with_osm,
            "the pedestrian ribbons must be dropped over surveyed paving \
             ({with_osm} vertices before, {with_survey} after)"
        );
        assert!(
            with_survey > 0,
            "only the pedestrian ways go — the residential streets in the same \
             class are OSM's and must still be drawn"
        );
    }

    #[test]
    fn the_road_sheet_is_a_partition() {
        // The claim `road_stitch` exists to make: no two road ribbons cover the
        // same patch of ground, so nothing is left for the depth buffer to
        // arbitrate and no camera angle can change which one is drawn.
        //
        // Rasterise every road and kerb triangle top-down and count how often each
        // pixel is covered. Before the cut this measured 76.1 % of road pixels
        // covered twice and 12.5 % three times or more (2026-08-26).
        const PX: usize = 1200;
        let tile_m = 1000.0f32;
        let meshes = tessellate_roads(GRONINGEN_Z14, tile_m, &[]).expect("tessellate");
        let mut count: Vec<u16> = vec![0; PX * PX];
        for m in meshes
            .iter()
            .filter(|m| matches!(m.group, SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)))
        {
            for t in m.indices.chunks_exact(3) {
                let p: Vec<[f32; 2]> = t
                    .iter()
                    .map(|i| {
                        let v = m.positions[*i as usize];
                        [v[0] / tile_m * PX as f32, v[2] / tile_m * PX as f32]
                    })
                    .collect();
                let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                    - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
                if area.abs() < 1e-9 {
                    continue;
                }
                let lo = |f: &dyn Fn(&[f32; 2]) -> f32| {
                    p.iter().map(f).fold(f32::MAX, f32::min).floor().max(0.0) as usize
                };
                let hi = |f: &dyn Fn(&[f32; 2]) -> f32| {
                    (p.iter().map(f).fold(f32::MIN, f32::max).ceil()).min(PX as f32 - 1.0) as usize
                };
                let (x0, x1) = (lo(&|q: &[f32; 2]| q[0]), hi(&|q: &[f32; 2]| q[0]));
                let (y0, y1) = (lo(&|q: &[f32; 2]| q[1]), hi(&|q: &[f32; 2]| q[1]));
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        let s = |a: &[f32; 2], b: &[f32; 2]| {
                            (b[0] - a[0]) * (py - a[1]) - (b[1] - a[1]) * (px - a[0])
                        };
                        let (d0, d1, d2) = (s(&p[0], &p[1]), s(&p[1], &p[2]), s(&p[2], &p[0]));
                        let inside = (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0)
                            || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0);
                        if inside {
                            count[y * PX + x] = count[y * PX + x].saturating_add(1);
                        }
                    }
                }
            }
        }
        let covered = count.iter().filter(|c| **c > 0).count();
        let doubled = count.iter().filter(|c| **c > 1).count();
        let frac = doubled as f32 / covered.max(1) as f32;
        println!(
            "road overdraw: {doubled}/{covered} px = {:.2}%",
            frac * 100.0
        );
        // 0.37 % measured (2026-09-11), down from 0.81 % while a ribbon was still
        // exempt from being cut against *itself*. That exemption was excused as
        // invisible — one material, one colour, one run of texture coordinates —
        // and the last clause is false: `v` is metres travelled, so a ribbon that
        // doubles back carries different lane markings on its two layers at the
        // same depth. What is left is the overlap kept on purpose, a flyover and
        // the road beneath it. The bound is a little over the measurement, to
        // catch the cut being skipped rather than a fixture that gains a street.
        assert!(
            frac < 0.01,
            "{:.2}% of covered road pixels are covered twice",
            frac * 100.0
        );
        // **Roads carry no height of their own**, and that is what lets the cut be
        // a plain two-dimensional one: every ribbon lies in the same plane, so
        // "who covers this ground" is the whole question. A stray lift here would
        // put the ribbons back on separate planes and quietly make the cut a lie.
        for m in meshes
            .iter()
            .filter(|m| matches!(m.group, SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)))
        {
            for p in &m.positions {
                assert_eq!(p[1], 0.0, "a road vertex was lifted off the road plane");
            }
            // The rung is a vertex attribute, and a mesh that carries the wrong
            // number of them fails pipeline validation — which bevy answers by
            // skipping the pass, i.e. the roads are simply not drawn, with healthy
            // mesh counts and nothing in the log.
            assert_eq!(
                m.rungs.len(),
                m.positions.len(),
                "a road mesh must carry one rung per vertex",
            );
        }
    }

    /// Eyeball hatch for junction geometry: rasterise the tessellated ribbons
    /// top-down and colour each pixel by how many ribbons cover it, so stubs,
    /// gaps and overlaps are visible without a GPU.
    ///
    /// `cargo test -p cartopolis_geo dump_road_ribbons -- --ignored
    /// --nocapture`; `CARTO_DUMP_WIN="x,z,size"` (tile metres) zooms to a junction.
    #[test]
    #[ignore = "eyeball hatch: it writes a PNG to look at, it asserts nothing"]
    fn dump_road_ribbons() {
        const PX: usize = 1200;
        let tile_m = 1000.0f32;
        let (wx, wz, ws) = match std::env::var("CARTO_DUMP_WIN") {
            Ok(s) => {
                let v: Vec<f32> = s.split(',').map(|p| p.trim().parse().unwrap()).collect();
                (v[0], v[1], v[2])
            }
            Err(_) => (0.0, 0.0, tile_m),
        };
        // `CARTO_DUMP_MVT=/tmp/some.mvt` looks at a tile fetched from the live
        // source instead of the fixture — which is how you answer "is this street
        // missing because of the styling or because of the data?" for a place you
        // are actually looking at.
        let owned = std::env::var("CARTO_DUMP_MVT")
            .ok()
            .map(|p| std::fs::read(p).expect("read tile"));
        let tile: &[u8] = owned.as_deref().unwrap_or(GRONINGEN_Z14);

        // What the tile *says* about its streets, tallied — the tags are what
        // decide how wide each one is drawn and whether it is drawn at all, so
        // this is the difference between "the styling lost that road" and "the
        // data never had it".
        let parsed = TileData::parse(tile).unwrap();
        let streets = parsed.layer("streets").unwrap();
        let mut tally: std::collections::BTreeMap<String, usize> = Default::default();
        for f in parsed.features(streets) {
            let flags = ["bridge", "tunnel", "oneway", "oneway_reverse", "link"]
                .iter()
                .filter(|n| flag_of(f, n))
                .copied()
                .collect::<Vec<_>>()
                .join("+");
            let drawn = match drawn_street(f) {
                Some((class, half)) => format!("{class:?} {:.1} m wide", half * 2.0),
                None => "not drawn".to_string(),
            };
            *tally
                .entry(format!(
                    "{:>12} {flags:<22} → {drawn}",
                    kind_of(f).unwrap_or("?")
                ))
                .or_default() += 1;
        }
        for (what, n) in &tally {
            println!("  {n:>3} × {what}");
        }

        // Decks first, exactly as the streamer does it: a bridge's own span rides
        // the deck at the deck's height, so the ribbons this dumps are the ones
        // that really reach the ground.
        let decks = tessellate_bridges(tile, tile_m).expect("bridges");
        println!("  {} bridge decks", decks.len());
        let meshes = tessellate_roads(tile, tile_m, &decks).expect("tessellate");

        let mut count = vec![0u16; PX * PX];
        for m in &meshes {
            // Per class as well as in total: two ribbons of *different* classes
            // overlapping is a crossing, which the depth rungs settle. Two of the
            // same class is the same road drawn twice.
            let mut own = vec![0u16; PX * PX];
            for tri in m.indices.chunks_exact(3) {
                let p: Vec<[f32; 2]> = tri
                    .iter()
                    .map(|i| {
                        let v = m.positions[*i as usize];
                        [(v[0] - wx) / ws * PX as f32, (v[2] - wz) / ws * PX as f32]
                    })
                    .collect();
                let (x0, x1) = (
                    p.iter()
                        .map(|q| q[0])
                        .fold(f32::MAX, f32::min)
                        .floor()
                        .max(0.0) as usize,
                    p.iter()
                        .map(|q| q[0])
                        .fold(f32::MIN, f32::max)
                        .ceil()
                        .min(PX as f32 - 1.0) as usize,
                );
                let (y0, y1) = (
                    p.iter()
                        .map(|q| q[1])
                        .fold(f32::MAX, f32::min)
                        .floor()
                        .max(0.0) as usize,
                    p.iter()
                        .map(|q| q[1])
                        .fold(f32::MIN, f32::max)
                        .ceil()
                        .min(PX as f32 - 1.0) as usize,
                );
                let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                    - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
                if area.abs() < 1e-9 {
                    continue;
                }
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        let w0 = ((p[1][0] - p[0][0]) * (py - p[0][1])
                            - (px - p[0][0]) * (p[1][1] - p[0][1]))
                            / area;
                        let w1 = ((px - p[0][0]) * (p[2][1] - p[0][1])
                            - (p[2][0] - p[0][0]) * (py - p[0][1]))
                            / area;
                        if w0 >= 0.0 && w1 >= 0.0 && w0 + w1 <= 1.0 {
                            count[y * PX + x] += 1;
                            own[y * PX + x] += 1;
                        }
                    }
                }
            }
            let covered = own.iter().filter(|c| **c > 0).count();
            let doubled = own.iter().filter(|c| **c > 1).count();
            println!(
                "  {:?}: {covered} px, {doubled} drawn twice by its own class ({:.1}%)",
                m.group,
                100.0 * doubled as f32 / covered.max(1) as f32
            );
        }

        let mut rgba = Vec::with_capacity(PX * PX * 4);
        let mut overlapped = 0usize;
        for c in &count {
            let px: [u8; 3] = match c {
                0 => [0x1A, 0x22, 0x1A],
                1 => [0x8A, 0x88, 0x86],
                2 => {
                    overlapped += 1;
                    [0xFD, 0x92, 0x12]
                }
                _ => {
                    overlapped += 1;
                    [0xE0, 0x20, 0x20]
                }
            };
            rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
        }
        let covered = count.iter().filter(|c| **c > 0).count();
        println!(
            "ribbon pixels {covered}, of which overlapped {overlapped} ({:.1}%)",
            100.0 * overlapped as f32 / covered.max(1) as f32
        );
        image::save_buffer(
            "/tmp/ribbons.png",
            &rgba,
            PX as u32,
            PX as u32,
            image::ExtendedColorType::Rgba8,
        )
        .expect("write png");
        println!("wrote /tmp/ribbons.png");

        // …and the same window again, coloured by **what** covers each pixel
        // rather than by how many things do. The overlap image cannot answer the
        // question a junction actually raises — *is the kerb running across the
        // opening of the street that joins here?* — because a kerb crossing a
        // carriageway is a partition either way and shows up as one clean pixel
        // in both. This one draws the carriageways pale and the kerb bands dark,
        // so a opening is either open tarmac or a bar across it.
        let mut kind = vec![0u8; PX * PX];
        for m in &meshes {
            // A footway is drawn apart from a carriageway, because it is not one
            // — it does not break a kerb, and reading it as tarmac makes a kerb
            // running correctly along the edge of a pavement look like a line
            // ruled down the middle of a road.
            let tone = match m.group {
                SurfaceGroup::RoadCasing(_) => 2u8,
                SurfaceGroup::Road(RoadClass::Path) => 3,
                SurfaceGroup::Road(_) => 1,
                _ => continue,
            };
            for tri in m.indices.chunks_exact(3) {
                let p: Vec<[f32; 2]> = tri
                    .iter()
                    .map(|i| {
                        let v = m.positions[*i as usize];
                        [(v[0] - wx) / ws * PX as f32, (v[2] - wz) / ws * PX as f32]
                    })
                    .collect();
                let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                    - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
                if area.abs() < 1e-9 {
                    continue;
                }
                let lo = |f: &dyn Fn(&[f32; 2]) -> f32| {
                    p.iter().map(f).fold(f32::MAX, f32::min).floor().max(0.0) as usize
                };
                let hi = |f: &dyn Fn(&[f32; 2]) -> f32| {
                    p.iter()
                        .map(f)
                        .fold(f32::MIN, f32::max)
                        .ceil()
                        .min(PX as f32 - 1.0) as usize
                };
                for y in lo(&|q| q[1])..=hi(&|q| q[1]) {
                    for x in lo(&|q| q[0])..=hi(&|q| q[0]) {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        let s = |a: &[f32; 2], b: &[f32; 2]| {
                            (b[0] - a[0]) * (py - a[1]) - (b[1] - a[1]) * (px - a[0])
                        };
                        let (d0, d1, d2) = (s(&p[0], &p[1]), s(&p[1], &p[2]), s(&p[2], &p[0]));
                        if (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0)
                            || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
                        {
                            kind[y * PX + x] = tone;
                        }
                    }
                }
            }
        }
        let kerb = kind.iter().filter(|k| **k == 2).count();
        let tarmac = kind.iter().filter(|k| **k == 1).count();
        println!("carriageway {tarmac} px, kerb band {kerb} px");

        // Two numbers the overlap image cannot give, both of which are what a
        // person means by "the joints look wrong":
        //
        // * **kerb over tarmac** — a kerb band surviving on ground a carriageway
        //   also claims. After `cut_band` the only legitimate case is a bridge,
        //   which is cut against bridges alone.
        // * **a crack** — bare ground with road on both sides of it, i.e. a
        //   hairline the cut opened between two pieces that should abut.
        let mut road_cov = vec![false; PX * PX];
        let mut kerb_cov = vec![false; PX * PX];
        for m in &meshes {
            let sink = match m.group {
                SurfaceGroup::RoadCasing(_) => &mut kerb_cov,
                SurfaceGroup::Road(_) => &mut road_cov,
                _ => continue,
            };
            for tri in m.indices.chunks_exact(3) {
                let p: Vec<[f32; 2]> = tri
                    .iter()
                    .map(|i| {
                        let v = m.positions[*i as usize];
                        [(v[0] - wx) / ws * PX as f32, (v[2] - wz) / ws * PX as f32]
                    })
                    .collect();
                let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                    - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
                if area.abs() < 1e-9 {
                    continue;
                }
                let lo = |f: &dyn Fn(&[f32; 2]) -> f32| {
                    p.iter().map(f).fold(f32::MAX, f32::min).floor().max(0.0) as usize
                };
                let hi = |f: &dyn Fn(&[f32; 2]) -> f32| {
                    p.iter()
                        .map(f)
                        .fold(f32::MIN, f32::max)
                        .ceil()
                        .min(PX as f32 - 1.0) as usize
                };
                for y in lo(&|q| q[1])..=hi(&|q| q[1]) {
                    for x in lo(&|q| q[0])..=hi(&|q| q[0]) {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        let s = |a: &[f32; 2], b: &[f32; 2]| {
                            (b[0] - a[0]) * (py - a[1]) - (b[1] - a[1]) * (px - a[0])
                        };
                        let (d0, d1, d2) = (s(&p[0], &p[1]), s(&p[1], &p[2]), s(&p[2], &p[0]));
                        if (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0)
                            || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
                        {
                            sink[y * PX + x] = true;
                        }
                    }
                }
            }
        }
        let both = road_cov
            .iter()
            .zip(kerb_cov.iter())
            .filter(|(r, k)| **r && **k)
            .count();
        let covered_any = road_cov
            .iter()
            .zip(kerb_cov.iter())
            .filter(|(r, k)| **r || **k)
            .count();
        let any = |i: usize| road_cov[i] || kerb_cov[i];
        let mut cracks = 0usize;
        let mut crack_at = vec![false; PX * PX];
        for y in 1..PX - 1 {
            for x in 1..PX - 1 {
                let i = y * PX + x;
                if any(i) {
                    continue;
                }
                let horiz = any(i - 1) && any(i + 1);
                let vert = any(i - PX) && any(i + PX);
                if horiz || vert {
                    cracks += 1;
                    crack_at[i] = true;
                }
            }
        }
        println!(
            "kerb over tarmac {both} px ({:.2} % of the sheet); one-pixel cracks {cracks}",
            100.0 * both as f32 / covered_any.max(1) as f32,
        );
        let mut rgba = Vec::with_capacity(PX * PX * 4);
        for (i, k) in kind.iter().enumerate() {
            // A crack is painted in the brand's accent orange so it is findable
            // at a glance: a one-pixel hairline in a grey-on-grey image is not.
            let px: [u8; 3] = if crack_at[i] {
                [0xFD, 0x60, 0x12]
            } else {
                match k {
                    1 => [0xB4, 0xB1, 0xAC],
                    2 => [0x33, 0x3A, 0x40],
                    3 => [0x6E, 0x6A, 0x63],
                    _ => [0x1A, 0x22, 0x1A],
                }
            };
            rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
        }
        image::save_buffer(
            "/tmp/ribbon_kinds.png",
            &rgba,
            PX as u32,
            PX as u32,
            image::ExtendedColorType::Rgba8,
        )
        .expect("write png");
        println!("wrote /tmp/ribbon_kinds.png");
    }

    /// The `bridges` layer is areas, and those areas are the deck a raised bridge
    /// stands on. Nothing drew them before this, which is why a viaduct was only
    /// ever the road ribbon crossing it.
    #[test]
    fn bridges_tessellate_into_decks_with_outlines() {
        let decks = tessellate_bridges(GRONINGEN_Z14, 1000.0).expect("bridges");
        assert!(!decks.is_empty(), "a city tile should hold bridge decks");
        for d in &decks {
            assert_eq!(d.indices.len() % 3, 0);
            assert!(!d.rings.is_empty(), "a deck with no outline has no fascia");
            assert!(
                d.indices.iter().all(|i| (*i as usize) < d.positions.len()),
                "index out of range"
            );
            // Inside the tile, flat, and a plausible size for a structure.
            for p in &d.positions {
                assert!((0.0..=1000.0).contains(&p[0]) && (0.0..=1000.0).contains(&p[2]));
                assert_eq!(
                    p[1], 0.0,
                    "a deck leaves the worker flat; height is measured"
                );
            }
            let span = d.span_m();
            assert!((1.0..=600.0).contains(&span), "deck span {span} m");
            // The centre of the bounding box is a weak test of `contains`, but a
            // deck that answers "no" everywhere would silently disable both the
            // ribbon suppression and the height sampling.
            let mid = [(d.min[0] + d.max[0]) * 0.5, (d.min[1] + d.max[1]) * 0.5];
            assert!(
                !d.contains([d.min[0] - 5.0, mid[1]]),
                "outside is not inside"
            );
        }
        assert!(
            decks
                .iter()
                .any(|d| d.contains([(d.min[0] + d.max[0]) * 0.5, (d.min[1] + d.max[1]) * 0.5])),
            "no deck contains its own centre — point-in-polygon is broken"
        );
    }

    /// A bridge's span leaves the ground sheet and arrives on the deck — it is
    /// not *lost*. Getting that backwards is what drew every viaduct as a bare
    /// grey slab with the markings stopping at its edge.
    ///
    /// The road *under* a flyover keeps every metre of its own ribbon, which is
    /// what the "only a little of it" bound below is watching.
    #[test]
    fn a_deck_carries_the_street_rather_than_replacing_it() {
        let decks = tessellate_bridges(GRONINGEN_Z14, 1000.0).expect("bridges");
        let bare = tessellate_roads(GRONINGEN_Z14, 1000.0, &[]).expect("roads");
        let (roads, _) = roads_and_decks(GRONINGEN_Z14, 1000.0, &decks).expect("roads");
        let count = |ms: &[SurfaceMesh]| ms.iter().map(|m| m.indices.len()).sum::<usize>();
        assert!(
            count(&roads.ground) < count(&bare),
            "the decks took no ribbon off the ground at all"
        );
        // …but only a little of it: the tile's bridges are a handful of spans, not
        // a fifth of its streets. A big drop here means ordinary roads are being
        // eaten by a deck they merely pass under.
        let lifted = 1.0 - count(&roads.ground) as f32 / count(&bare) as f32;
        assert!(
            lifted < 0.1,
            "decks took {:.0}% of the road mesh off the ground",
            lifted * 100.0
        );
        // And every metre of it is on a deck, drawn as a road: a `Road` group with
        // its texture coordinates intact is the whole point — a deck that carried
        // only casings, or nothing, is the old bare slab wearing a new type.
        let on_deck: usize = roads
            .on_deck
            .iter()
            .filter(|(_, m)| matches!(m.group, SurfaceGroup::Road(_)))
            .map(|(_, m)| m.indices.len())
            .sum();
        assert!(on_deck > 0, "no carriageway rides any deck");
        assert!(
            roads
                .on_deck
                .iter()
                .all(|(_, m)| m.uvs.len() == m.positions.len()),
            "an on-deck ribbon lost its texture coordinates"
        );
    }

    /// The carriageway is cut **out of** the slab it stands on, so the two share a
    /// plane with nothing left to arbitrate. If the subtraction silently did
    /// nothing, the road and the deck would be two coplanar opaque surfaces and
    /// the deck — which sits at the top of the ground ladder — would win, i.e. the
    /// bare slab would be back with no other symptom.
    #[test]
    fn the_carriageway_is_cut_out_of_the_deck() {
        let decks = tessellate_bridges(GRONINGEN_Z14, 1000.0).expect("bridges");
        let (roads, carved) = roads_and_decks(GRONINGEN_Z14, 1000.0, &decks).expect("roads");
        let area = |d: &BridgeDeck| -> f64 {
            d.indices
                .chunks_exact(3)
                .map(|t| {
                    let p: Vec<[f32; 3]> = t.iter().map(|i| d.positions[*i as usize]).collect();
                    f64::from(
                        ((p[1][0] - p[0][0]) * (p[2][2] - p[0][2])
                            - (p[2][0] - p[0][0]) * (p[1][2] - p[0][2]))
                            .abs(),
                    ) * 0.5
                })
                .sum()
        };
        let mut carried = 0;
        for (di, (before, after)) in decks.iter().zip(&carved).enumerate() {
            let rides = roads.on_deck.iter().any(|(d, _)| *d as usize == di);
            if !rides {
                assert!(
                    (area(after) - area(before)).abs() < 1e-3,
                    "deck {di} carries no road but was cut anyway"
                );
                continue;
            }
            carried += 1;
            assert!(
                area(after) < area(before),
                "deck {di} carries a road and kept its whole surface"
            );
            // …and it is not cut away entirely: what is left is the verge and the
            // parapet either side of the carriageway, which is what makes a bridge
            // read as a structure rather than as a strip of tarmac in mid-air.
            assert!(
                area(after) > 0.05 * area(before),
                "deck {di} lost {:.0}% of its surface to the road on it",
                100.0 * (1.0 - area(after) / area(before))
            );
        }
        assert!(carried > 0, "no deck in the fixture carries a road");
    }

    /// The split lands on the deck's edge, not on the nearest vertex — a stretch
    /// of road belonging to neither piece is exactly the gap the slab used to
    /// hide.
    #[test]
    fn a_street_is_split_at_the_deck_edge_not_at_a_vertex() {
        // A 20 m square deck at the origin, and a street crossing it in two long
        // segments whose vertices are nowhere near its edges.
        let ring = vec![
            [-10.0, -10.0],
            [10.0, -10.0],
            [10.0, 10.0],
            [-10.0, 10.0],
            [-10.0, -10.0],
        ];
        let deck = BridgeDeck {
            positions: Vec::new(),
            indices: Vec::new(),
            rings: vec![ring],
            min: [-10.0, -10.0],
            max: [10.0, 10.0],
        };
        let line = [[-60.0, 0.0], [0.0, 0.0], [60.0, 0.0]];
        let runs = split_by_deck(&line, std::slice::from_ref(&deck));
        let owners: Vec<Option<usize>> = runs.iter().map(|(o, _)| *o).collect();
        assert_eq!(owners, vec![None, Some(0), None], "on, off, on");
        // Each seam within a centimetre of x = ±10, and shared exactly by the two
        // runs that meet there.
        assert!((runs[0].1.last().unwrap()[0] + 10.0).abs() < 0.01);
        assert_eq!(runs[0].1.last(), runs[1].1.first());
        assert!((runs[1].1.last().unwrap()[0] - 10.0).abs() < 0.01);
        assert_eq!(runs[1].1.last(), runs[2].1.first());
        // A street that never meets the deck comes back in one piece.
        let clear = split_by_deck(&[[-60.0, 40.0], [60.0, 40.0]], std::slice::from_ref(&deck));
        assert_eq!(clear.len(), 1);
        assert_eq!(clear[0].0, None);
    }

    #[test]
    fn tessellates_water_and_landuse() {
        let tile_m = 1000.0;
        let meshes = tessellate_surfaces(GRONINGEN_Z14, tile_m).expect("tessellate");
        assert!(!meshes.is_empty(), "no surfaces tessellated");

        let water = meshes
            .iter()
            .find(|m| m.group == SurfaceGroup::Water)
            .expect("Groningen has canals");
        assert!(water.indices.len() >= 3);
        assert_eq!(
            water.indices.len() % 3,
            0,
            "indices must be whole triangles"
        );

        for m in &meshes {
            let n = m.positions.len() as u32;
            assert!(
                m.indices.iter().all(|&i| i < n),
                "{:?}: index out of range",
                m.group
            );
            // Tile-local metres, inside the tile, and flat on the ground.
            //
            // **A ribbon is allowed half its width past the edge**, and only a
            // ribbon. A fill is clipped to the tile exactly (`clip_ring`), so it
            // abuts its neighbour's copy on the border line itself. A narrow
            // waterway is a *line* clipped to that same border and then widened,
            // so its cap reaches half a channel beyond — which is what the street
            // ribbons have always done, for the same reason and to the same
            // extent. The allowance is the widest water line this build draws.
            let slack = if m.group == SurfaceGroup::Water {
                water_line_half_width_m("river").unwrap_or(0.0)
            } else {
                0.0
            };
            for p in &m.positions {
                assert!(
                    (-slack..=tile_m + slack).contains(&p[0]),
                    "{:?}: x {} out of tile",
                    m.group,
                    p[0]
                );
                assert!(
                    (-slack..=tile_m + slack).contains(&p[2]),
                    "{:?}: z {} out of tile",
                    m.group,
                    p[2]
                );
                assert_eq!(p[1], 0.0);
            }
            // Every triangle wound to face up, so the ground is not backface-culled.
            for t in m.indices.chunks_exact(3) {
                let (a, b, c) = (
                    m.positions[t[0] as usize],
                    m.positions[t[1] as usize],
                    m.positions[t[2] as usize],
                );
                let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
                assert!(cross <= 0.0, "{:?}: triangle wound downward", m.group);
            }
        }
    }

    /// **No surface group may cover the same ground twice.** This is the half of
    /// the z-fight story `ground_stack` explicitly cannot reach: two polygons of
    /// one group share a rung by definition, so a layer fighting itself has no
    /// tie-break anywhere in the tree.
    ///
    /// It is not hypothetical. Measured on this fixture before
    /// `tessellate_surfaces` started cutting, on 2026-09-15: **9.7 % of the grass
    /// and 3.1 % of the built land** was covered twice by its own polygons — parks
    /// with cemeteries in them, recreation grounds with playgrounds in them, every
    /// one a separate `land` feature resolving to the same group. And the
    /// 2026-08-29 ghost pavements were the same shape of bug in the surveyed
    /// layer, found on a screen a fortnight after they shipped.
    ///
    /// The ceiling is not zero, and the reason is the tile buffer: a feature
    /// clipped to the tile plus a buffer can reach past its own edge, and the cut
    /// works on the millimetre lattice, so a sliver of a texel survives here and
    /// there. It is well under a percent, which is the difference between a
    /// residual and a layer.
    #[test]
    fn no_group_overlaps_itself() {
        const CEILING: f32 = 1.0;
        for (group, covered, twice) in group_coverage() {
            if covered == 0 {
                continue;
            }
            let pct = 100.0 * twice as f32 / covered as f32;
            assert!(
                pct <= CEILING,
                "{group:?} covers {twice} of its own {covered} texels twice ({pct:.1} %) — \
                 no depth rung can order a layer against itself"
            );
        }
    }

    /// …and no two groups share ground either, which takes the other half out of
    /// the ladder's hands.
    ///
    /// Measured before the cut: a fifth of the woodland lay under grass, nearly a
    /// quarter of the water over it, and a tenth of the built land under it. The
    /// ladder still orders what is left — a flyover over the road beneath it, and
    /// anything that crosses a tile boundary, which is two features in two tiles
    /// that never see each other — so this is a backstop becoming a backstop, not
    /// a ladder becoming unnecessary.
    #[test]
    fn no_two_groups_share_ground() {
        const CEILING: f32 = 1.0;
        let masks = group_masks();
        for (i, (ga, a)) in masks.iter().enumerate() {
            for (gb, b) in masks.iter().skip(i + 1) {
                let both = a.iter().zip(b.iter()).filter(|(x, y)| **x && **y).count();
                let area = a.iter().filter(|x| **x).count().max(1);
                let pct = 100.0 * both as f32 / area as f32;
                assert!(
                    pct <= CEILING,
                    "{ga:?} and {gb:?} share {both} texels ({pct:.1} % of {ga:?})"
                );
            }
        }
    }

    /// One coverage count per group: texels its own triangles cover at all, and
    /// texels they cover more than once.
    fn group_coverage() -> Vec<(SurfaceGroup, usize, usize)> {
        const N: usize = 1024;
        let meshes = bytes::tessellate_surfaces(GRONINGEN_Z14, 1000.0).unwrap();
        let mut counts: Vec<(SurfaceGroup, Vec<u16>)> = Vec::new();
        for m in &meshes {
            let slot = match counts.iter().position(|(g, _)| *g == m.group) {
                Some(i) => i,
                None => {
                    counts.push((m.group, vec![0u16; N * N]));
                    counts.len() - 1
                }
            };
            rasterise(
                m,
                N,
                |cell| *cell = cell.saturating_add(1),
                &mut counts[slot].1,
            );
        }
        counts
            .into_iter()
            .map(|(g, c)| {
                (
                    g,
                    c.iter().filter(|v| **v >= 1).count(),
                    c.iter().filter(|v| **v >= 2).count(),
                )
            })
            .collect()
    }

    /// The same, as a plain covered/not-covered mask per group.
    fn group_masks() -> Vec<(SurfaceGroup, Vec<bool>)> {
        const N: usize = 1024;
        let meshes = bytes::tessellate_surfaces(GRONINGEN_Z14, 1000.0).unwrap();
        let mut masks: Vec<(SurfaceGroup, Vec<bool>)> = Vec::new();
        for m in &meshes {
            let slot = match masks.iter().position(|(g, _)| *g == m.group) {
                Some(i) => i,
                None => {
                    masks.push((m.group, vec![false; N * N]));
                    masks.len() - 1
                }
            };
            rasterise(m, N, |cell| *cell = true, &mut masks[slot].1);
        }
        masks
    }

    /// **And no ground fill may lie under a carriageway either.** This was the
    /// largest overlap left anywhere in the ground stack and the last one to be
    /// removed.
    ///
    /// It is a different pair from the two above: those are fills against fills,
    /// settled inside [`tessellate_surfaces`]. The roads are built by a separate
    /// pass that needs the bridge decks first, so a fill and a ribbon had never
    /// met — and a landuse polygon covers the street through it, because that is
    /// what a residential block *is*. Measured on this fixture before
    /// [`crate::road_stitch::cut_under_roads`] was given the fills, 2026-09-19:
    ///
    /// | | of the road sheet's area |
    /// |---|---|
    /// | built land under road | **67.1 %** |
    /// | grass under road | 7.2 % |
    /// | woodland under road | 1.7 % |
    ///
    /// Three quarters of every carriageway in the city had a second opaque
    /// coplanar surface beneath it, held apart by half a millimetre of
    /// `GroundLayer` clearance. That works while the ground is flat and stops the
    /// moment it has a gradient: each sheet is bent onto the measured ground
    /// against its own triangles, so the two disagree by the field's curvature
    /// across an edge — decimetres on a bank, up to 2.8 m on a handset — and the
    /// lower one stands through the upper one in jagged polygons that follow
    /// triangle edges. That is the phone report of 2026-09-19.
    ///
    /// The ceiling is the same sub-one-percent residual the two tests above allow,
    /// and for the same reason: the cut runs on a millimetre lattice and this
    /// samples texel centres, so a sliver survives along every kerb.
    #[test]
    fn no_ground_fill_lies_under_a_road() {
        const N: usize = 1024;
        const CEILING: f32 = 1.0;
        let roads = bytes::tessellate_roads(GRONINGEN_Z14, 1000.0, &[]).unwrap();
        let mut road = vec![false; N * N];
        for m in &roads {
            rasterise(m, N, |cell| *cell = true, &mut road);
        }

        let mut fills = bytes::tessellate_surfaces(GRONINGEN_Z14, 1000.0).unwrap();
        let before = shared_with(&fills, &road, N);
        assert!(
            before > 0,
            "the fixture no longer has a fill under a road — this test is measuring nothing"
        );

        crate::road_stitch::cut_under_roads(&mut fills, &roads);
        let after = shared_with(&fills, &road, N);
        let pct = 100.0 * after as f32 / before as f32;
        assert!(
            pct <= CEILING,
            "{after} of {before} texels under a carriageway still carry a ground fill \
             ({pct:.1} %) — no depth rung can order two sheets on measured ground"
        );
    }

    /// Texels covered both by one of `meshes` and by the road mask.
    fn shared_with(meshes: &[SurfaceMesh], road: &[bool], n: usize) -> usize {
        let mut mask = vec![false; n * n];
        for m in meshes {
            rasterise(m, n, |cell| *cell = true, &mut mask);
        }
        mask.iter().zip(road).filter(|(a, b)| **a && **b).count()
    }

    /// Scan-convert a surface mesh onto an `N × N` grid over the tile, calling
    /// `mark` on each covered cell. Centre sampling and no anti-aliasing: this
    /// measures *coverage*, and a half-covered texel counted as covered by both of
    /// two neighbours would read as an overlap that is not there.
    fn rasterise<T>(m: &SurfaceMesh, n: usize, mark: impl Fn(&mut T), grid: &mut [T]) {
        for t in m.indices.chunks_exact(3) {
            let p: Vec<[f32; 2]> = t
                .iter()
                .map(|&i| {
                    let v = m.positions[i as usize];
                    [v[0] / 1000.0 * n as f32, v[2] / 1000.0 * n as f32]
                })
                .collect();
            let (a, b, c) = (p[0], p[1], p[2]);
            // Both ends clamped, not just the far one: a tile carries a buffer, so a
            // ribbon can start past the grid's far edge entirely and a `min` that only
            // bounded the maximum left the loop indexing off the end.
            let minx = (a[0].min(b[0]).min(c[0]).floor().max(0.0) as usize).min(n - 1);
            let maxx = ((a[0].max(b[0]).max(c[0]).ceil()).max(0.0) as usize).min(n - 1);
            let miny = (a[1].min(b[1]).min(c[1]).floor().max(0.0) as usize).min(n - 1);
            let maxy = ((a[1].max(b[1]).max(c[1]).ceil()).max(0.0) as usize).min(n - 1);
            let side = |u: [f32; 2], v: [f32; 2], q: [f32; 2]| {
                (v[0] - u[0]) * (q[1] - u[1]) - (v[1] - u[1]) * (q[0] - u[0])
            };
            for y in miny..=maxy.max(miny) {
                for x in minx..=maxx.max(minx) {
                    let q = [x as f32 + 0.5, y as f32 + 0.5];
                    let (s0, s1, s2) = (side(a, b, q), side(b, c, q), side(c, a, q));
                    if (s0 >= 0.0 && s1 >= 0.0 && s2 >= 0.0)
                        || (s0 <= 0.0 && s1 <= 0.0 && s2 <= 0.0)
                    {
                        mark(&mut grid[y * n + x]);
                    }
                }
            }
        }
    }

    #[test]
    fn buildings_are_not_tessellated() {
        // 3DBAG and the Overpass detail patches already draw these in 3D, and they
        // are ~75% of a tile's triangles — pulling them in here would double the
        // geometry for no gain.
        let meshes = tessellate_surfaces(GRONINGEN_Z14, 1000.0).unwrap();
        let tris: usize = meshes.iter().map(|m| m.indices.len() / 3).sum();
        assert!(
            tris < 12_000,
            "{tris} triangles — buildings likely leaked into the surface pass"
        );
    }

    /// The real tile must produce ribbons for several classes, and every one of
    /// them must be a well-formed textured mesh.
    #[test]
    fn roads_tessellate_into_textured_ribbons() {
        let meshes = tessellate_roads(GRONINGEN_Z14, 1000.0, &[]).expect("tessellate");
        assert!(
            meshes.len() >= 3,
            "a city tile should yield several road classes, got {}",
            meshes.len()
        );
        assert!(
            meshes
                .iter()
                .any(|m| matches!(m.group, SurfaceGroup::RoadCasing(_))),
            "kerbed classes must emit their casing band"
        );
        for m in &meshes {
            assert!(
                matches!(m.group, SurfaceGroup::Road(_) | SurfaceGroup::RoadCasing(_)),
                "{:?}",
                m.group
            );
            assert_eq!(
                m.uvs.len(),
                m.positions.len(),
                "every road vertex needs a UV or the markings cannot register"
            );
            assert!(m.indices.len() % 3 == 0 && !m.indices.is_empty());
            assert!(
                m.indices.iter().all(|i| (*i as usize) < m.positions.len()),
                "index out of range"
            );
            // `u` is the across-road axis and must span the full carriageway.
            assert!(m.uvs.iter().any(|uv| uv[0] == 0.0));
            assert!(m.uvs.iter().any(|uv| uv[0] == 1.0));
            // Ground geometry: exactly flat — the anti-z-fight ladder is a depth
            // offset now and takes no height at all — and inside the tile plus
            // the junction stubs.
            assert!(m.positions.iter().all(|p| p[1] == 0.0));
            assert_eq!(
                m.rungs.len(),
                m.positions.len(),
                "every road vertex needs a ladder rung or it cannot be ordered"
            );
            assert!(m
                .positions
                .iter()
                .all(|p| p[0] > -40.0 && p[0] < 1040.0 && p[2] > -40.0 && p[2] < 1040.0));
        }
    }

    /// **The image paints the surveyed ground and drops what it replaces.** Over a
    /// cell whose survey carries carriageways, the residential streets the meshes
    /// draw from the survey are not stroked in OSM's tarmac, and the paving shows
    /// in its own colour where they were; the streets that stay OSM's still draw.
    #[test]
    fn the_survey_is_painted_and_replaces_its_streets() {
        let tile = crate::testdata::groningen();
        let size = 256;
        let minor = RoadClass::Minor.flat_rgb();
        let count = |px: &[u8], rgb: u32| {
            px.chunks_exact(4)
                .filter(|p| u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]) == rgb)
                .count()
        };
        let plain = render_mvt_surveyed(
            &tile,
            SubTile::WHOLE,
            size,
            NOMINAL_Z14_TILE_M,
            &[],
            &[],
            &NO_SURVEY,
        )
        .unwrap();
        let brick = 0x98_64_52;
        let survey = Survey {
            paving: vec![OverlayFill {
                rings: vec![vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]],
                rgb: brick,
            }],
            cells: vec![CoveredCell {
                bounds: [0.0, 0.0, 1.0, 1.0],
                cover: SurveyedCover::Streets,
            }],
        };
        let surveyed = render_mvt_surveyed(
            &tile,
            SubTile::WHOLE,
            size,
            NOMINAL_Z14_TILE_M,
            &[],
            &[],
            &survey,
        )
        .unwrap();
        // `tertiary` shares the minor tone and stays OSM's (it carries markings),
        // so what must go is the rest: everything the residential network drew.
        let only_paths = Survey {
            cells: vec![CoveredCell {
                bounds: [0.0, 0.0, 1.0, 1.0],
                cover: SurveyedCover::Paths,
            }],
            ..survey.clone()
        };
        let paths = render_mvt_surveyed(
            &tile,
            SubTile::WHOLE,
            size,
            NOMINAL_Z14_TILE_M,
            &[],
            &[],
            &only_paths,
        )
        .unwrap();
        let (before, after) = (count(&paths, minor), count(&surveyed, minor));
        assert!(
            before > 500,
            "the fixture strokes residential streets: {before}"
        );
        assert!(
            after * 10 < before * 7,
            "the survey's streets are not stroked in OSM's tarmac: {before} → {after}"
        );
        assert!(
            count(&paths, RoadClass::Path.flat_rgb()) * 2
                < count(&plain, RoadClass::Path.flat_rgb()),
            "over a surveyed cell the paths are the survey's too"
        );
        assert!(
            count(&surveyed, brick) > count(&plain, brick) + 1000,
            "the paving shows in its own colour"
        );
        assert!(
            count(&surveyed, RoadClass::Major.flat_rgb()) * 2
                >= count(&plain, RoadClass::Major.flat_rgb()),
            "the marked roads stay OSM's"
        );
    }
}
