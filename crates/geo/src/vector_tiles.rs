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
use crate::road_texture::{
    road_class, road_width_m, surveyed_paving_supersedes, RoadClass, StreetFlags,
};

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
pub mod palette {
    pub const PAPER: u32 = 0xEE_EA_E2;
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
    pub const FOREST: u32 = 0xC3_D3_BB;
    pub const GRASS: u32 = 0xD2_DE_C6;
    /// Canopy tone: one step down the [`FOREST`] hue ramp. The map is pale
    /// cartographic paper, so a saturated green here reads as confetti scattered
    /// on a pastel drawing rather than as trees — see `systems::vegetation`.
    pub const CANOPY: u32 = 0xA9_BF_9E;
    /// Second canopy tone, alternated per instance. Two tones buy depth without
    /// costing a light.
    pub const CANOPY_DEEP: u32 = 0x93_AC_8A;
    /// Trunks are a warm grey in the [`BUILDING_EDGE`] family, deliberately *not*
    /// brown: at walking distance a saturated brown is the loudest colour on the
    /// whole map.
    pub const TRUNK: u32 = 0xB0_A5_94;
    /// What a forest polygon looks like from far enough that the individual trees
    /// have merged: [`FOREST`] ground seen through ~65% [`CANOPY`] cover. The
    /// fill ramps to this as the trees fade out, so the aggregate brightness does
    /// not step when the geometry goes away.
    pub const FOREST_CANOPIED: u32 = 0xB2_C6_A8;
    pub const SAND: u32 = 0xE6_DC_C2;
    /// A tunnel's walls and roof. Darker than any surface in this palette on
    /// purpose: it is the one thing here that is never lit by the sun, and a
    /// concrete the tone of a pavement reads as a white box with a hole at each
    /// end. The sky's ambient still reaches the open approach, so the opening
    /// stays lighter than the tunnel without anything modelling it.
    pub const TUNNEL: u32 = 0x6E_6B_66;
    /// A quay wall: weathered brick and stone above a canal, darker than the
    /// street on top of it so the edge reads from across the water.
    pub const QUAY_WALL: u32 = 0x7A_6A_5C;
    pub const RESIDENTIAL: u32 = 0xE6_E1_D8;
    pub const INDUSTRIAL: u32 = 0xE0_DC_D8;
    /// Built-up land as read from altitude. Up close, [`RESIDENTIAL`] and
    /// [`INDUSTRIAL`] sit within a few counts of [`GRASS`] on purpose — streets
    /// and buildings carry the figure there — but from ~z12 out those are
    /// sub-pixel, the land layer is all that is left, and a whole province
    /// rendered as one near-uniform green sheet with no visible cities. These are
    /// the same land uses a hue step warmer and darker, used by the far style
    /// band (`FAR_STYLE_TILE_M`) so a town reads as a town from the air.
    pub const RESIDENTIAL_FAR: u32 = 0xDE_D4_C2;
    pub const INDUSTRIAL_FAR: u32 = 0xD6_D1_CA;
    pub const SITE: u32 = 0xE3_E0_D5;
    pub const BUILDING: u32 = 0xD3_CC_C0;
    pub const BUILDING_EDGE: u32 = 0xBE_B5_A6;
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
const PAINT_ORDER: &[(&str, Draw)] = &[
    (
        "land",
        Draw::FillByKind {
            default: palette::GRASS,
            kinds: &[
                ("forest", palette::FOREST),
                ("wood", palette::FOREST),
                ("grass", palette::GRASS),
                ("meadow", palette::GRASS),
                ("park", palette::GRASS),
                ("garden", palette::GRASS),
                ("farmland", palette::GRASS),
                ("sand", palette::SAND),
                ("beach", palette::SAND),
                ("residential", palette::RESIDENTIAL),
                ("industrial", palette::INDUSTRIAL),
                ("commercial", palette::INDUSTRIAL),
            ],
        },
    ),
    ("sites", Draw::Fill(palette::SITE)),
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
    // Cull only when overzooming: at `factor == 1` the window is the whole tile,
    // so every feature passes and the bounds scan is pure overhead.
    render_mvt_banded(
        tile,
        sub,
        size,
        source_m,
        sub.factor > 1,
        raster_bands(size),
        overlay,
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
    render_mvt_banded(tile, sub, size, source_m, cull, raster_bands(size), &[])
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
    render_mvt_banded(tile, sub, size, source_m, sub.factor > 1, 1, &[])
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
fn render_mvt_banded(
    tile: &TileData,
    sub: SubTile,
    size: u32,
    source_m: f32,
    cull: bool,
    bands: u32,
    overlay: &[OverlayFill],
) -> Result<Vec<u8>, String> {
    if bands <= 1 {
        return render_rows(tile, sub, size, source_m, cull, 0, size, overlay);
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
                        render_rows(tile, sub, size, source_m, cull, y0, h, overlay)
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
    render_rows(tile, sub, size, source_m, cull, 0, size, overlay)
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
    overlay: &[OverlayFill],
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
    // holds: `land` is first in `PAINT_ORDER`, so this is the top of the second
    // pass — or after the loop, for an order that ever ends at `land`.
    let overlay_transform = Transform::from_row(
        px * f,
        0.0,
        0.0,
        px * f,
        -(sub.x as f32) * px,
        -(sub.y as f32) * px - y0 as f32,
    );
    let mut overlay_pending = !overlay.is_empty();

    for (layer_name, draw) in PAINT_ORDER {
        if overlay_pending && *layer_name != "land" {
            paint_overlay(&mut pixmap, &mut paint, overlay, &overlay_transform);
            overlay_pending = false;
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

        let ordered: Vec<&Feature> = match street_order {
            Some(list) => list,
            None => features.iter().collect(),
        };
        for feature in ordered {
            let kind = kind_of(feature);
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

/// Fill each [`OverlayFill`] in its own colour, one even-odd path per polygon so
/// a hole stays open. `transform` maps the source tile's unit square to pixels.
fn paint_overlay(
    pixmap: &mut Pixmap,
    paint: &mut Paint<'static>,
    overlay: &[OverlayFill],
    transform: &Transform,
) {
    for fill in overlay {
        let mut pb = PathBuilder::new();
        for ring in &fill.rings {
            let mut pts = ring.iter();
            let Some(first) = pts.next() else {
                continue;
            };
            if ring.len() < 3 {
                continue;
            }
            pb.move_to(first[0], first[1]);
            for p in pts {
                pb.line_to(p[0], p[1]);
            }
            pb.close();
        }
        let Some(path) = pb.finish() else {
            continue;
        };
        paint.shader = shader(fill.rgb);
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

/// Clip one ring to the square `[0, extent]²` (Sutherland–Hodgman), returning it
/// **open** — no repeated last point — with consecutive duplicates dropped.
///
/// This exists instead of a per-vertex `clamp` because clamping is not a clip: it
/// projects every outside vertex onto the border and throws away the order in
/// which the ring left and re-entered. A canal that runs out through the tile's
/// west edge and back comes home as a *fold* — the outbound and return banks both
/// collapse onto `x = 0` and cross each other — and earcut on a self-intersecting
/// ring emits bow-ties: finite-area triangles outside the polygon. On the ground
/// those are the dark water wedges lying across a lawn or a carriageway a hundred
/// metres from the nearest canal, and the pale spikes over the water beside them.
/// Measured over the cached Groningen z14 tiles, the clamp produced 145 such
/// triangles (23,000 m² of misplaced water) across 4,970 water polygons; the clip
/// produces none.
///
/// Sutherland–Hodgman inserts the true border crossings and walks the border
/// between an exit and the next entry, so a concave shape comes back with
/// zero-width connectors along the seam rather than a fold. Those degenerate into
/// zero-area triangles, which cost a little raster time and are invisible — the
/// trade the clamp was reaching for.
fn clip_ring(points: &[[f32; 2]], extent: f32) -> Vec<[f32; 2]> {
    // Each pass clips against one half-plane of the square, feeding the next.
    let mut ring: Vec<[f32; 2]> = points.to_vec();
    // Rings arrive closed; the walk below re-closes implicitly via the modulo.
    if ring.len() >= 2 && ring[0] == ring[ring.len() - 1] {
        ring.pop();
    }
    for edge in 0..4 {
        if ring.len() < 3 {
            return Vec::new();
        }
        let inside = |p: &[f32; 2]| match edge {
            0 => p[0] >= 0.0,
            1 => p[0] <= extent,
            2 => p[1] >= 0.0,
            _ => p[1] <= extent,
        };
        // Where segment a→b crosses this edge. Only called when the two ends sit
        // on opposite sides, so the denominator cannot be zero.
        let crossing = |a: &[f32; 2], b: &[f32; 2]| -> [f32; 2] {
            match edge {
                0 | 1 => {
                    let at = if edge == 0 { 0.0 } else { extent };
                    let t = (at - a[0]) / (b[0] - a[0]);
                    [at, a[1] + t * (b[1] - a[1])]
                }
                _ => {
                    let at = if edge == 2 { 0.0 } else { extent };
                    let t = (at - a[1]) / (b[1] - a[1]);
                    [a[0] + t * (b[0] - a[0]), at]
                }
            }
        };
        let src = std::mem::take(&mut ring);
        for i in 0..src.len() {
            let prev = src[(i + src.len() - 1) % src.len()];
            let cur = src[i];
            match (inside(&prev), inside(&cur)) {
                (true, true) => ring.push(cur),
                (true, false) => ring.push(crossing(&prev, &cur)),
                (false, true) => {
                    ring.push(crossing(&prev, &cur));
                    ring.push(cur);
                }
                (false, false) => {}
            }
        }
    }

    // The clip lands successive vertices on the same border point whenever the
    // ring grazes a corner; earcut turns a duplicate into a degenerate ear.
    /// Two clipped vertices close enough that earcut would see one.
    fn near(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() <= 1e-5 && (a[1] - b[1]).abs() <= 1e-5
    }
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(ring.len());
    for p in ring {
        if out.last().is_none_or(|q: &[f32; 2]| !near(p, *q)) {
            out.push(p);
        }
    }
    // Closing vertex, on the same tolerance the interior dedup above uses. An exact
    // `==` here left a ring whose ends differ by less than 1e-5 — but not by
    // nothing — carrying a near-duplicate seam vertex into earcut, which is exactly
    // the degenerate ear this function exists to prevent, arrived at from the one
    // direction the loop above cannot see.
    while out.len() >= 2 && near(out[0], out[out.len() - 1]) {
        out.pop();
    }
    if out.len() < 3 {
        return Vec::new();
    }
    out
}

/// A polygon's rings in tile-local metres, clipped to the tile exactly the way
/// [`tessellate_polygon`] clips them, and **closed** (last point repeats the
/// first) so `windows(2)` walks every edge.
///
/// This is the boundary the vegetation scatter insets from. It has to come from
/// the same clip as the fill, or the trees would stand off an edge the visible
/// green does not have.
pub(crate) fn clipped_rings(
    poly: &geo_types::Polygon<f32>,
    scale: f32,
    extent: f32,
) -> Vec<Vec<[f32; 2]>> {
    let ring = |r: &geo_types::LineString<f32>| -> Vec<[f32; 2]> {
        let raw: Vec<[f32; 2]> = r.0.iter().map(|c| [c.x, c.y]).collect();
        let mut pts: Vec<[f32; 2]> = clip_ring(&raw, extent)
            .into_iter()
            .map(|p| [p[0] * scale, p[1] * scale])
            .collect();
        if pts.len() >= 3 {
            pts.push(pts[0]);
        }
        pts
    };
    std::iter::once(poly.exterior())
        .chain(poly.interiors())
        .map(ring)
        .filter(|r| r.len() >= 4)
        .collect()
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
            .filter(|c| c.len() >= 3)
            .map(|c| {
                let mut ring: Vec<[f32; 2]> = c
                    .iter()
                    .map(|p| [p.x as f32 / 1000.0, p.y as f32 / 1000.0])
                    .collect();
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

/// How far a ribbon may be pushed past the end of its centre line, in metres, to
/// fill the notch at a junction.
///
/// Streets arrive as separate features that merely *share an endpoint*, so a road
/// tee-ing into another stops at that road's centre line and leaves a wedge of
/// bare ground either side of it. The stub that fills the wedge has to reach the
/// **far kerb of the road being joined**, which is that road's half-width — not
/// this one's. Extending by its own half-width (what this used to do) is right
/// only when the two are the same width; a motorway ending at a service road threw
/// an 11 m flap of tarmac across a 4 m street, and every road that simply *ends*
/// — a cul-de-sac, a track petering out — grew the same flap into open ground.
///
/// So the length now comes from [`junction_extent`], and this is only the cap that
/// keeps a pathological node (a footpath meeting a motorway) from painting a stub
/// longer than anything it could plausibly be joining.
const JUNCTION_EXTEND_MAX: f32 = 12.0;

/// How many depth-buffer ULPs one rung of the road ladder is worth.
///
/// Ribbons **overlap by construction** — a junction stub crosses whatever it tees
/// into, Shortbread's merged ways cross each other freely, and a flyover is drawn
/// on the same ground plane as the road it flies over. Two coplanar overlapping
/// triangles have depth values that agree only to the last few bits, so which one
/// wins flips per pixel as the camera moves: the flicker along a junction. The
/// material's own `depth_bias` cannot settle it, being a per-material constant
/// while the fight is between two features sharing a material — which is exactly
/// what this per-vertex rung supplies.
///
/// **The unit is the fix.** A rung used to be 24 µm of real height, and a
/// geometric gap's significance in the depth buffer decays as 1/z: 24 µm is 2.0
/// ULP at 100 m, 1.0 at 200 m and nothing at all beyond that. So the within-class
/// ordering — which ribbon of a junction is on top — simply *dissolved* in the
/// middle distance, and every same-class overlap (6 % of `Minor`, 14 % of
/// `Carriageway` on the fixture) went back to being an exact per-pixel coin flip
/// that re-rolls as the camera moves. That is the shimmer along a street, and no
/// amount of tie-colouring could reach it, because the rungs it was assigning had
/// stopped meaning anything.
///
/// Denominated in ULPs the rung is **range-invariant** by construction: the
/// apparent world shift is `1.19e-7 · ulps · z`, so it grows exactly as fast as
/// the depth buffer's own quantum.
///
/// It is sixty-four now and was four, and the change came with
/// [`crate::road_stitch`] taking the overlap away. Four was chosen as "enough to
/// clear the rasteriser's interpolation noise" and was never measured; the
/// sibling ladder that orders grass against water against road needs
/// [`DEPTH_BIAS_STEP`] — sixty-four — and its own comment records sixteen being
/// tried and found wanting at the shallow angles a low camera looks at the middle
/// distance with. On the Groningen fixture 1.9 % of road pixels were held apart by
/// a single four-unit rung, and a headless capture over the city confirmed the
/// consequence: raising this constant alone changed which ribbon was drawn along
/// every road past about two kilometres.
///
/// A rung can afford to be sixteen times taller because there are two of them
/// instead of 399 ([`ROAD_DEPTH_RUNGS`]) — the ordering that used to need all
/// those rungs is a *cut* now, not a tie-break.
pub const ROAD_RUNG_ULP: f32 = 64.0;

/// How many rungs the road band actually holds: ground, then bridge.
///
/// `road_stitch` leaves the ground roads as a partition — no two of them cover the
/// same patch — so nothing there needs separating in depth at all. The one
/// overlap it deliberately keeps is a flyover over the road it crosses, which is
/// two real surfaces in one place, and one rung is what holds them apart.
///
/// The 399-rung ladder this replaces was `class × width band × tie colouring`, all
/// of it in aid of deciding which of two overlapping ribbons to draw. That decision
/// is made in geometry now, and [`road_rung`] survives only as the *order the cut
/// runs in* — the hierarchy is still exactly as it was, it simply no longer has to
/// be expressed in the depth buffer.
pub const ROAD_DEPTH_RUNGS: u32 = 2;

/// Which of the two depth rungs a ribbon draws on.
#[must_use]
pub fn road_depth_rung(bridge: bool) -> f32 {
    u32::from(bridge) as f32
}

/// Rungs reserved at the top of the ladder for roads carried on a bridge deck.
/// One casing plus a full set of ties, so a flyover's ribbons separate from each
/// other as well as from the ground.
const BRIDGE_LAYERS: u32 = 1 + ROAD_TIE_LAYERS;

/// Tie-breaking rungs within one width band. Eight, not the four that fitted
/// under the old millimetre ceiling.
const ROAD_TIE_LAYERS: u32 = 8;

/// Width sub-bands *inside* one class block — the full [`ROAD_WIDTH_BANDS`]
/// resolution.
///
/// This was 2 under the old ladder, and the reason was purely the ceiling: rungs
/// were real height and the road sheet had 1.5 mm to spend before it reached the
/// buildings' base, so six class blocks could not also afford eight width bands.
/// A rung costs no height now, so the compression is simply unnecessary — and it
/// was not free. Measured on the Groningen fixture, restoring the eight bands
/// takes coplanar overlap 0.89 % → 0.62 %, and the wider tie set takes it to
/// 0.51 %.
const ROAD_WIDTH_SUBBANDS: u32 = 8;

/// How many rungs the ladder has, top to bottom. Ordered, not arbitrary (see
/// [`road_rung`]), so which of two overlapping features wins is a property of the
/// roads rather than of the order the tile happened to store them in.
const ROAD_LAYERS: u32 = ROAD_CLASS_COUNT * ROAD_RUNGS_PER_CLASS + BRIDGE_LAYERS;

/// How many classes [`road_class_rank`] ranks.
const ROAD_CLASS_COUNT: u32 = 6;

/// Rungs one class owns: one for its casings, then its fills.
///
/// The casing takes a single rung because same-class casings that overlap are
/// the same colour — their fight is invisible, which is the one place in this
/// ladder where a tie costs nothing. Fills need the width sub-band and the tie
/// colouring, so they take the rest.
///
/// Nothing bounds this but the road band's share of the depth range
/// (`SurfaceGroup::depth_rung`), which is why it can be generous now: a rung is
/// no longer a slice of the 1.5 mm the road sheet had under the buildings.
const CASING_RUNGS_PER_CLASS: u32 = 1;
const ROAD_RUNGS_PER_CLASS: u32 = CASING_RUNGS_PER_CLASS + ROAD_WIDTH_SUBBANDS * ROAD_TIE_LAYERS;

/// The highest depth bias any road vertex can reach: the foot of the road band
/// plus the whole per-vertex ladder standing on it.
///
/// This is what anything drawn *over* a road has to clear — a bridge deck, a
/// tree, a bench, a zebra crossing. It is [`GroundLayer::depth_bias_ceiling`],
/// and `ground_stack`'s `the_road_band_holds_every_road_vertex` checks the whole
/// stack against it, so growing the road ladder can never again silently push it
/// through the things above.
#[must_use]
pub fn road_rung_ceiling() -> f32 {
    GroundLayer::Road.depth_bias_ceiling()
}

/// A class's block index, low to high — **the** road hierarchy.
///
/// It used to have to agree with a second ranking in
/// [`SurfaceGroup::depth_rung`], and every way the two could disagree was a bug
/// (see [`road_rung`]). `depth_rung` gives every road group one shared value now,
/// so this is the only thing in the tree that orders the classes.
fn road_class_rank(class: RoadClass) -> u32 {
    match class {
        RoadClass::Path => 0,
        RoadClass::Minor => 1,
        RoadClass::Major => 2,
        RoadClass::Carriageway => 3,
        RoadClass::Motorway => 4,
        RoadClass::Rail => 5,
    }
}

/// Width-band thresholds (metres) for [`road_rung`]: a road's band is the count
/// of thresholds at or below its width. 2 m steps where the urban network is
/// dense-packed — service 4, living street 6, residential 7, tertiary 9 all land
/// in different bands now, where the old single 6 m band held all of them —
/// and coarser above 10 m, where within-class widths barely vary and the band
/// only needs to keep a primary under a motorway.
const ROAD_WIDTH_BANDS: [f32; 7] = [2.0, 4.0, 6.0, 8.0, 10.0, 14.0, 20.0];

/// Which rung of the road ladder a line draws on — **the** road ordering, whole.
///
/// **This does not reach the GPU.** It is the order the *cut* runs in: which of
/// two overlapping ribbons keeps its ground when `road_stitch` partitions them.
/// The per-vertex attribute `systems::road_depth` spends as a depth offset comes
/// from [`road_depth_rung`], which has [`ROAD_DEPTH_RUNGS`] = 2 values — this one
/// has 399 and would be read as an enormous bias by anything that confused them.
/// Until 2026-09-18 this comment said the opposite, describing the arrangement
/// from before the ladder was collapsed on 2026-08-26, and `geo::tunnels` was
/// written against it three weeks later: a tunnel deck asked for rung 391–398,
/// i.e. ~25,900 depth units, and drew over the ground it runs under from a
/// kilometre away. A stale doc comment is a load-bearing part of an interface.
///
/// It is not a height either, and must never become one again; see the two
/// failures below.
///
/// Two rules, in order:
///
/// * A **bridge** takes the reserved top of the ladder. A flyover and the road
///   under it are both flat sheets on the same ground plane here, so the only
///   thing that can say which is on top is this; without it an interchange
///   resolves per-pixel and the deck and the road beneath interleave.
/// * Otherwise the **class decides, then width, then the tie colouring** — one
///   key, most significant first, exactly as it reads.
///
/// ## Why there is only one ladder
///
/// There used to be two, denominated differently, and every bug in this area was
/// a disagreement between them. `SurfaceGroup::depth_rung` ranked the *classes*
/// in depth-buffer ULPs, whose apparent world shift **grows** with range
/// (`1.19e-7 · bias · z`); this function ranked features in 24 µm steps of real
/// height, whose significance **shrinks** as 1/z. They failed in both directions:
///
/// * **Crossover.** When the two orders differed — a wide residential (Minor)
///   against a narrow tertiary (Major) — the terms crossed somewhere in the
///   middle distance and the winner flipped as the camera moved. Not flicker at a
///   seam: the loser's tarmac vanished over whole patches. Reported as "sometimes
///   part of the roads become invisible, the order is reversed". Making the class
///   the major key fixed *this* half, at the cost of compressing width onto two
///   sub-bands to fit under the ceiling.
/// * **Dissolution**, which the first fix could not touch. 24 µm is worth 2.0 ULP
///   at 100 m and 1.0 at 200 m, so past a couple of hundred metres the *entire*
///   within-class ladder — width, ties, casing — was below the depth buffer's
///   resolution and every same-class overlap went back to being an exact coin
///   flip. On the fixture that is 6 % of `Minor` and 14 % of `Carriageway`
///   fighting per pixel, re-rolled every frame the view matrix moves: the shimmer
///   down a street and across its junctions. Colouring the ties more cleverly
///   could not help, because the rungs being assigned had stopped meaning
///   anything.
///
/// Denominating *everything* in ULPs removes both at once. There is no second
/// ladder to contradict, and no range at which the ordering fades, because the
/// unit and the buffer that arbitrates are now the same unit. It also lifts the
/// ceiling that forced the width compression — a rung costs no height, so
/// [`ROAD_WIDTH_SUBBANDS`] is back to the full eight and [`ROAD_TIE_LAYERS`] to
/// eight.
///
/// Lines that tie on both are separated by `tie_seed`. `tessellate_roads` hands
/// this a rung chosen across the whole tile by [`assign_road_ties`], which is
/// what stops two overlapping same-band lines landing on one rung; the seed is a
/// **hash of the line's own endpoints** (`line_tie_seed`) wherever no
/// coordinated answer exists, and that hash is what the assignment itself starts
/// from. The seed is deliberately not the feature's index in the tile: a
/// Shortbread `MultiLineString` whose parts cross each other gave every part the
/// same index — a guaranteed exact-coplanar z-fight (measured at up to 37 %
/// self-overlap within a class at a single junction). Neither the hash nor the
/// assignment is stable across a tile seam (each tile clips its own copy of a
/// shared street, so no whole-line function can be), but a junction lives inside
/// one tile far more often than on a seam.
pub(crate) fn road_rung(class: RoadClass, width_m: f32, bridge: bool, tie_seed: u64) -> u32 {
    let tie = (tie_seed % ROAD_TIE_LAYERS as u64) as u32;
    if bridge {
        // The bridge block is laid out like a class block: its casing on the foot
        // rung, its fills above. Under the old ladder the fills started *on* the
        // foot, so a deck's kerb band and its own tarmac shared a rung whenever
        // the tie came out zero — one flyover in four, fighting with itself.
        // [`BRIDGE_LAYERS`] reserves the extra rung this skips.
        return ROAD_LAYERS - BRIDGE_LAYERS + CASING_RUNGS_PER_CLASS + tie;
    }
    let block = road_class_rank(class) * ROAD_RUNGS_PER_CLASS;
    block + CASING_RUNGS_PER_CLASS + road_subband(width_m) * ROAD_TIE_LAYERS + tie
}

/// The rung a class's *casing* ribbons draw on: the foot of that class's block,
/// so it lies under its own tarmac and over every narrower class's tarmac.
///
/// A whole class's casings share this one rung on purpose: two overlapping
/// casings of one class are the same flat colour, so their fight is the one
/// collision in this ladder that cannot be seen.
///
/// It settles a road's kerb against *its own* tarmac and nothing wider than
/// that: which of two **different** roads' surfaces wins is [`cut_band`]'s, and
/// this rung is only the tie-break inside a band.
fn road_casing_rung(class: RoadClass, bridge: bool) -> u32 {
    if bridge {
        // A deck's kerb rides with the deck, under its own fills.
        return ROAD_LAYERS - BRIDGE_LAYERS;
    }
    road_class_rank(class) * ROAD_RUNGS_PER_CLASS
}

/// Which sheet of the road surface a ribbon belongs to — the leading term of
/// `road_stitch::Ribbon::priority`, above the whole class hierarchy.
///
/// **A kerb is the edge between a carriageway and what is not one.** Where
/// another carriageway abuts, there is no kerb — that is a junction opening, and
/// it is not optional: every side street in a city has one. The cut used to
/// settle a kerb band against a carriageway on `road_rung` alone, which puts the
/// *whole* of a higher class above the whole of a lower one, casing included. So
/// a residential street tee-ing into a primary road lost the argument to that
/// road's kerb band: its tarmac stopped at the far side of the kerb and the kerb
/// line ran on unbroken across the opening, walling the street off from the road
/// it joins. That is what a junction looked like from the pavement everywhere
/// two classes met — which is most junctions, since Shortbread's classes are
/// what make a street a side street.
///
/// Three bands, and the two that are not the ordinary case are there because
/// they are genuinely different surfaces:
///
/// * **2 — rails.** A level crossing is track laid *over* a road: the sleepers
///   and ballast are continuous and the tarmac is what stops. `road_rung`
///   already put rail at the top of the ladder; this keeps it there rather than
///   promoting every carriageway over it.
/// * **1 — carriageways**, every class a kerb is drawn for.
/// * **0 — kerb bands, and footways.** A footway is not a carriageway, so it
///   does not break a kerb; within this band `road_rung` decides exactly as it
///   did before, which leaves a path crossing a road settled the old way.
///
/// The bands must be a *total* order together with the rung, and that is the
/// point of expressing this as a band rather than as a "may block" predicate: an
/// asymmetric rule where a kerb outranks a carriageway that outranks the kerb
/// cuts both out of the ground they share and leaves a hole in the road.
fn cut_band(group: SurfaceGroup) -> u32 {
    match group {
        SurfaceGroup::Road(RoadClass::Rail) => 2,
        SurfaceGroup::Road(RoadClass::Path) => 0,
        SurfaceGroup::Road(_) => 1,
        // Kerb bands, and anything else that is ever staged into the road sheet
        // (a bridge deck), at the foot.
        _ => 0,
    }
}

/// Which width sub-band inside a class block a width falls in: the eight
/// [`ROAD_WIDTH_BANDS`] compressed onto [`ROAD_WIDTH_SUBBANDS`].
fn road_subband(width_m: f32) -> u32 {
    let bands = ROAD_WIDTH_BANDS.len() as u32 + 1;
    (road_band(width_m) * ROAD_WIDTH_SUBBANDS / bands).min(ROAD_WIDTH_SUBBANDS - 1)
}

/// Which [`ROAD_WIDTH_BANDS`] band a width falls in: the count of thresholds at
/// or below it.
fn road_band(width_m: f32) -> u32 {
    ROAD_WIDTH_BANDS.iter().filter(|t| width_m >= **t).count() as u32
}

/// Tie-break seed for one linestring: a mix of its endpoint coordinates, so two
/// crossing lines almost never share a rung even when they share a feature (a
/// merged `MultiLineString`), a width band and a tile.
fn line_tie_seed(head: [f32; 2], tail: [f32; 2]) -> u64 {
    let mut h: u64 = 0x9E37_79B9_7F4A_7C15;
    for v in [head[0], head[1], tail[0], tail[1]] {
        h ^= u64::from(v.to_bits());
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 29;
    }
    h
}

/// One line's entry in the tie-assignment pass.
struct TieLine {
    /// Lines can only collide within one band, and bridges take their own
    /// reserved block, so this is the whole of "could these two ever tie".
    key: (u32, bool),
    /// Whole-line bounds in tile units, widened by the ribbon's half-width — the
    /// cheap reject before the per-segment test.
    min: [f32; 2],
    max: [f32; 2],
    /// Per-segment bounds, same padding. **The whole-line box is far too blunt to
    /// colour against**: a long diagonal street's box covers a quarter of the
    /// tile and "overlaps" almost every other street in its band, which builds a
    /// nearly complete graph, exhausts the four colours and sends most lines back
    /// to the hash. Testing segments recovers the real adjacency — measured on
    /// the fixture, the whole-line box left 0.79 % of road pixels coplanar where
    /// segments leave far less.
    segs: Vec<([f32; 2], [f32; 2])>,
    seed: u64,
    at: (usize, usize),
}

impl TieLine {
    /// Do these two ribbons share any ground?
    fn touches(&self, other: &TieLine) -> bool {
        let apart = |amin: [f32; 2], amax: [f32; 2], bmin: [f32; 2], bmax: [f32; 2]| {
            amax[0] < bmin[0] || bmax[0] < amin[0] || amax[1] < bmin[1] || bmax[1] < amin[1]
        };
        if apart(self.min, self.max, other.min, other.max) {
            return false;
        }
        self.segs.iter().any(|(amin, amax)| {
            other
                .segs
                .iter()
                .any(|(bmin, bmax)| !apart(*amin, *amax, *bmin, *bmax))
        })
    }
}

/// Choose each line's tie rung so that two roads which share a width band *and*
/// overlap on the ground never land on the same one.
///
/// **The hash alone cannot do this, and the ladder cannot be made bigger.**
/// `line_tie_seed` is drawn independently per line, so two roads in one band
/// collide one time in [`ROAD_TIE_LAYERS`] — and a collision here is not a
/// settled order but an exact-coplanar pair, i.e. an opaque z-fight that
/// flickers per pixel as the camera moves. Measured on the Groningen z14
/// fixture, that left 1.60 % of road-covered pixels fighting. The obvious
/// answer — more rungs — *was* unavailable when a rung was 24 µm of real height:
/// [`ROAD_LAYERS`] of them already reached 1.44 mm against the 1.5 mm the road
/// sheet had under the buildings. A rung costs no height now, so the ladder has
/// grown ([`ROAD_TIE_LAYERS`] is 8 and [`ROAD_WIDTH_SUBBANDS`] is back to the
/// full 8) and this colouring has correspondingly less to do — but it is still
/// what keeps two crossing streets of one width band off a single rung, and more
/// rungs alone plateaus at 0.51 % where the colouring reaches it.
///
/// So the assignment is *coordinated* rather than drawn: within a band the
/// overlapping lines form a graph, and this is a greedy colouring of it. A line
/// keeps its hashed rung whenever that rung is free, which leaves the
/// non-colliding majority exactly where it was and moves only what must move;
/// where every colour is taken the hash decides as before, so the result is
/// never worse than the old behaviour and is usually exact.
///
/// Bounds are compared rather than the geometry: a bounding box is a superset of
/// the ribbon, so this can invent an edge between two roads that merely pass
/// near one another. That costs a little colour diversity and never correctness.
/// O(n²) over the lines of one tile — 95 street features on the fixture, so a
/// few thousand box tests against a ~121 ms raster.
fn assign_road_ties(
    features: &[Feature],
    scale: f32,
) -> std::collections::HashMap<(usize, usize), u32> {
    let mut lines: Vec<TieLine> = Vec::new();
    for (fi, feature) in features.iter().enumerate() {
        let Some((_, half)) = drawn_street(feature) else {
            continue;
        };
        let bridge = flag_of(feature, "bridge");
        let mut geoms: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => geoms.push(ls),
            Geometry::MultiLineString(mls) => geoms.extend(mls.iter()),
            _ => continue,
        }
        // `half` is metres; these bounds are in tile units.
        let pad = if scale > 0.0 { half / scale } else { 0.0 };
        for (li, ls) in geoms.iter().enumerate() {
            let pts: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x, c.y])
                    .collect();
            let (Some(h), Some(t)) = (pts.first().copied(), pts.last().copied()) else {
                continue;
            };
            let mut min = [f32::MAX, f32::MAX];
            let mut max = [f32::MIN, f32::MIN];
            for p in &pts {
                min[0] = min[0].min(p[0]);
                min[1] = min[1].min(p[1]);
                max[0] = max[0].max(p[0]);
                max[1] = max[1].max(p[1]);
            }
            let segs: Vec<([f32; 2], [f32; 2])> = pts
                .windows(2)
                .map(|w| {
                    (
                        [w[0][0].min(w[1][0]) - pad, w[0][1].min(w[1][1]) - pad],
                        [w[0][0].max(w[1][0]) + pad, w[0][1].max(w[1][1]) + pad],
                    )
                })
                .collect();
            if segs.is_empty() {
                continue;
            }
            lines.push(TieLine {
                key: (road_band(half * 2.0), bridge),
                min: [min[0] - pad, min[1] - pad],
                max: [max[0] + pad, max[1] + pad],
                segs,
                seed: line_tie_seed(h, t),
                at: (fi, li),
            });
        }
    }

    // Adjacency once. The colouring consults it repeatedly and orders itself by
    // it, so computing it up front is both cheaper than testing pairs inside the
    // loop and what makes the ordering below possible.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); lines.len()];
    for i in 0..lines.len() {
        for j in (i + 1)..lines.len() {
            if lines[i].key == lines[j].key && lines[i].touches(&lines[j]) {
                adj[i].push(j);
                adj[j].push(i);
            }
        }
    }

    // Welsh–Powell: colour the most constrained lines first, so a crowded
    // junction gets its colours before the four are gone and the sparse edges of
    // the tile take what is left. Ties broken by the hash and then by position,
    // so the answer never depends on the order the tile stored its features in.
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by(|&a, &b| {
        adj[b]
            .len()
            .cmp(&adj[a].len())
            .then(lines[a].seed.cmp(&lines[b].seed))
            .then(lines[a].at.cmp(&lines[b].at))
    });

    let mut chosen: Vec<Option<u32>> = vec![None; lines.len()];
    let mut out = std::collections::HashMap::new();
    for &i in &order {
        let mut used = [false; ROAD_TIE_LAYERS as usize];
        for &j in &adj[i] {
            if let Some(t) = chosen[j] {
                used[t as usize] = true;
            }
        }
        let hashed = (lines[i].seed % ROAD_TIE_LAYERS as u64) as u32;
        let tie = if used[hashed as usize] {
            (0..ROAD_TIE_LAYERS)
                .find(|t| !used[*t as usize])
                .unwrap_or(hashed)
        } else {
            hashed
        };
        chosen[i] = Some(tie);
        out.insert(lines[i].at, tie);
    }
    out
}

/// The across-road `u` span a junction stub samples: a column of plain road
/// surface clear of every class's markings (the nearest marking edge is at
/// u = 0.037 on a motorway and the wheel tracks start at 0.215 on a minor
/// road). A stub carrying the full cross-section ruled its kerb lines and
/// centre dash across the road being joined; a stub carrying only surface
/// tone reads as the open tarmac a real junction is.
const STUB_PLAIN_U: (f32, f32) = (0.06, 0.16);

/// How far a [`SurfaceGroup::RoadCasing`] ribbon reaches past its fill on each
/// side, in metres — a kerb-and-verge band, not a second road.
const ROAD_CASING_M: f32 = 0.75;

/// Miter limit: how far a join may push the **outer** corner out relative to the
/// half-width before it is bevelled off instead. Unbounded miters spike to
/// infinity at a hairpin, which OSM has plenty of (motorway sliproads,
/// switchbacks).
///
/// Past this the join emits two outer vertices and a fill triangle rather than a
/// single stretched one — see [`ribbon`] on why clamping the miter *in place* is
/// not the same thing.
///
/// **2.5 was a stroke-join default, not a number about roads**, and it is too
/// generous by a wide margin: a join may reach `2.5 · half` from the centre line,
/// which is an 11 m spike on a 9 m street and a 27 m one on a motorway. The
/// Groningen fixture has a worked example — feature 69's first linestring is a
/// `tertiary` turning head that reverses through **128.2°**, whose miter of
/// `1 / cos(64.1°) = 2.29` slipped under the old limit and drew a ten-metre
/// tongue of tarmac out into the field past the road it serves.
///
/// 1.5 instead, for one reason that can be stated: a right angle mitres at
/// `1 / cos 45° = 1.414`, and a street grid's right angles are corners the map is
/// asserting (the same licence [`ROUND_MAX_DEG`] draws). So every corner up to
/// ~96° keeps its point and nothing in an ordinary city moves; past that the
/// point stops being a shape a road has, and a bevel — the chord between the two
/// square corners — is what a turning head actually looks like.
///
/// **It has to stay a ratio, not a distance.** [`kerb_band`] zips the carriageway
/// against a second, wider run of this same code and relies on every join
/// decision being independent of the width; a limit in metres would let the fill
/// and its kerb bevel at different corners, desynchronise the two chains and drop
/// the band to its slab fallback.
const MITER_LIMIT: f32 = 1.5;

/// Fraction of the shorter adjacent segment that the **inner** corner may
/// backtrack over. See [`ribbon`]: the inner miter runs back `half / tan(θ/2)`
/// along both segments, and past their far end the ribbon folds through itself.
///
/// Half, not all, because the segment's *other* end has a join of its own eating
/// into the same length — two joins each taking half meet exactly, and never
/// cross.
const INNER_BACKTRACK_BUDGET: f32 = 0.5;

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
        if surveyed_paving && surveyed_paving_supersedes(kind_of(feature).unwrap_or("")) {
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
                    if on_deck.is_some() || part.len() < 2 {
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
/// A segment that enters *and* leaves a deck between two vertices is still missed —
/// both its ends agree — but that is a structure narrower than one vertex spacing,
/// which at z14 is a structure too small to have a deck polygon in the first place.
fn split_by_deck(points: &[[f32; 2]], decks: &[BridgeDeck]) -> Vec<(Option<usize>, Vec<[f32; 2]>)> {
    let mut runs: Vec<(Option<usize>, Vec<[f32; 2]>)> = Vec::new();
    let Some(&first) = points.first() else {
        return runs;
    };
    let mut owner = deck_at(decks, first);
    let mut cur: Vec<[f32; 2]> = vec![first];
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let next = deck_at(decks, b);
        if next == owner {
            cur.push(b);
            continue;
        }
        // The edge between them, shared exactly by the run that ends on it and the
        // run that starts from it — one computation, so the two cannot land apart.
        let edge = deck_edge_between(decks, a, owner, b);
        cur.push(edge);
        if cur.len() >= 2 {
            runs.push((owner, std::mem::take(&mut cur)));
        } else {
            cur.clear();
        }
        cur.push(edge);
        cur.push(b);
        owner = next;
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

/// A named street as text-ready geometry: the centre line a label is laid along,
/// in tile-local metres.
///
/// The name comes from Shortbread's `street_labels` layer and not from `streets`,
/// because that is where the source puts it — the two layers are merged and
/// generalised independently, so there is no id to join on (see
/// `routing::LabelSeg`, which pays the same price for the turn list). For a
/// *label* that costs nothing: the line only has to run roughly down the street
/// it names, and `street_labels` is exactly a per-name polyline.
#[derive(Debug, Clone)]
pub struct StreetLabelLine {
    pub name: String,
    /// Drawing priority, low first: a motorway keeps its label when the screen
    /// is too crowded to fit an alley's.
    pub rank: u8,
    /// Polyline in tile-local metres (x east, y south) — the frame the surface
    /// meshes are built in, so the same cell origin places both.
    pub points: Vec<[f32; 2]>,
}

/// Vertices closer together than this are merged. z14 street lines carry
/// junction-accurate detail a text baseline cannot use, and every vertex is a
/// point projected per frame.
const LABEL_SIMPLIFY_M: f32 = 6.0;

/// Lines shorter than this are dropped outright: nothing that fits a name at any
/// readable size, so they would only cost projection work to fail the on-screen
/// length test later.
const LABEL_MIN_LEN_M: f32 = 15.0;

/// Read the tile's `street_labels` layer as one polyline per named street.
///
/// Clipped to the tile like the road ribbons are ([`clip_polyline`]), so a street
/// crossing a cell boundary contributes its own stretch to each cell rather than a
/// full copy to both — the label renderer's same-name spacing rule then thins the
/// repeats by *screen* distance, which is the only place that judgement can be made.
pub fn extract_street_labels(tile: &TileData, tile_m: f32) -> Result<Vec<StreetLabelLine>, String> {
    let Some(meta) = tile.layer("street_labels") else {
        return Ok(Vec::new());
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;

    let mut out = Vec::new();
    for feature in features {
        // A tunnel's name is not written on the ground above it, for exactly the
        // reason its carriageway is not drawn there (`drawn_street`): the label
        // would be laid across whatever genuinely *is* up there — the junction the
        // tunnel passes under, or the park it runs beneath. Shortbread puts `tunnel`
        // on `street_labels` as well as on `streets`, so this costs nothing.
        //
        // There is no matching rule for a bridge, and it is not an oversight:
        // `street_labels` carries no `bridge` attribute (checked 2026-08-20 on the
        // Groningen fixture), so a name on a viaduct cannot be told from one on the
        // road underneath it. See `docs/notes/grade-separation.md`.
        // The bare `tunnel` flag here, not `tunnels::underground`: that predicate
        // also reads `layer`, which Shortbread carries on `streets` and **not**
        // on `street_labels`, so asking for it would answer "level 0" for every
        // name in the layer and write every tunnel's name across the park over it.
        // The cost is that an arcade's name is still withheld, which is the
        // behaviour this layer has always had.
        if flag_of(feature, "tunnel") {
            continue;
        }
        let Some(name) = str_prop(feature, "name").filter(|n| !n.is_empty()) else {
            continue;
        };
        let rank = label_rank(kind_of(feature).unwrap_or(""));
        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            _ => continue,
        }
        for ls in lines {
            let pts: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x, c.y])
                    .collect();
            for run in clip_polyline(&pts, 0.0, extent) {
                let metres: Vec<[f32; 2]> =
                    run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                let points = simplify_polyline(&metres, LABEL_SIMPLIFY_M);
                if polyline_len(&points) < LABEL_MIN_LEN_M {
                    continue;
                }
                out.push(StreetLabelLine {
                    name: name.to_string(),
                    rank,
                    points,
                });
            }
        }
    }
    Ok(out)
}

/// Label priority from Shortbread's `kind`. Same ordering the road styling uses,
/// collapsed to five bands — this only has to decide who loses a collision.
fn label_rank(kind: &str) -> u8 {
    match kind {
        "motorway" | "trunk" => 0,
        "primary" => 1,
        "secondary" | "tertiary" => 2,
        "residential" | "unclassified" | "living_street" | "pedestrian" => 3,
        _ => 4,
    }
}

/// Read a string property off a feature.
fn str_prop<'a>(feature: &'a Feature, key: &str) -> Option<&'a str> {
    match feature.properties.as_ref()?.get(key)? {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Drop vertices within `min_step` of the one before, always keeping the last —
/// so the line's total length and end direction survive.
fn simplify_polyline(points: &[[f32; 2]], min_step: f32) -> Vec<[f32; 2]> {
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        let last = out.last().copied();
        let keep = match last {
            None => true,
            Some(q) => i == points.len() - 1 || (p[0] - q[0]).hypot(p[1] - q[1]) >= min_step,
        };
        if keep {
            out.push(*p);
        }
    }
    // The final vertex may have landed on top of its predecessor.
    if out.len() >= 2 {
        let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
        if (a[0] - b[0]).hypot(a[1] - b[1]) < 1e-3 {
            out.pop();
        }
    }
    out
}

fn polyline_len(points: &[[f32; 2]]) -> f32 {
    points
        .windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .sum()
}

/// A street's drawn class and half-width, or `None` if it is not drawn as ground
/// geometry at all.
///
/// **Tunnels are not drawn.** A tunnel is tarmac the camera cannot see: painting
/// it on the ground plane lays a road across whatever is genuinely up there —
/// which at an interchange is the junction the tunnel passes under, and in a city
/// is a park or a canal with a carriageway ruled through it. `road_class` already
/// refuses `subway` for the same reason; the `tunnel` flag is that rule applied to
/// the kinds that can be either.
pub(crate) fn drawn_street(feature: &Feature) -> Option<(RoadClass, f32)> {
    // A tunnel leaves the ground sheet because it is not on the ground — it is
    // drawn under it by `crate::tunnels`. An arcade through a building is
    // flagged a tunnel too and *is* on the ground, so it stays here; see
    // `tunnels::underground`.
    if crate::tunnels::underground(feature) {
        return None;
    }
    street_shape(feature)
}

/// A street's class and half-width, with no judgement about whether it is drawn.
///
/// The half of [`drawn_street`] that is about the road itself. Split out for the
/// one caller that has to overrule the tunnel refusal above — `tessellate_roads`,
/// which has the tile and so can tell an arcade from a tunnel.
pub(crate) fn street_shape(feature: &Feature) -> Option<(RoadClass, f32)> {
    let kind = kind_of(feature).unwrap_or("");
    let flags = StreetFlags {
        oneway: flag_of(feature, "oneway") || flag_of(feature, "oneway_reverse"),
        oneway_reverse: flag_of(feature, "oneway_reverse"),
        link: flag_of(feature, "link"),
    };
    Some((road_class(kind, flags)?, road_width_m(kind, flags) * 0.5))
}

/// The half-width [`drawn_street`] would answer for a way of this `kind` with no
/// flags set, `None` on a kind it refuses.
///
/// The kind-only half of the same rule, for a caller holding a class name rather
/// than a decoded feature — `WayField::push_line`, which synthesises ways in
/// tests. It shares the two functions above rather than restating a number, so a
/// width edit cannot land on one path and miss the other.
#[cfg(test)]
pub(crate) fn drawn_half_width(kind: &str) -> Option<f32> {
    let flags = StreetFlags::default();
    road_class(kind, flags).map(|_| road_width_m(kind, flags) * 0.5)
}

/// One road arriving at one node, as [`street_nodes`] records it.
#[derive(Clone, Copy)]
struct NodeArm {
    /// Which feature this is, so a road never treats itself as a junction.
    feature: usize,
    /// Half the carriageway width, in metres.
    half: f32,
    class: RoadClass,
    /// The road's direction *through* the node, sign-free — only the crossing
    /// angle is ever read from it.
    axis: [f32; 2],
    /// Set when the node is one of this line's own **ends**: the direction the
    /// road leaves the node in, and how long its first segment is, in metres.
    /// `None` for an interior vertex — a road that merely passes through cannot
    /// be the thing that *continues* another one.
    leaves: Option<([f32; 2], f32)>,
}

/// Every vertex of every drawn street in the tile, keyed on the integer lattice,
/// carrying the features that touch it and how wide each of them is.
///
/// Keyed on the raw tile units *rounded to an integer*, with no tolerance, because
/// none is needed: MVT quantises all of a tile's geometry onto one integer lattice,
/// so two ways sharing an OSM node share the exact coordinate (the same property
/// `routing::RoadGraph` leans on to stitch tiles without a fuzzy match). A tolerance
/// would only start fusing genuinely distinct nodes in a dense junction.
///
/// *Every* vertex goes in, not just endpoints: a side street tees into the middle
/// of a through road, so the node it shares is an interior vertex of that road.
/// Whether the node *is* an end is recorded ([`NodeArm::leaves`]) rather than
/// used to filter, because the two questions asked of this table want opposite
/// answers — [`junction_extent`] wants every road at the node, and
/// [`continuation`] wants only the ones that end there.
///
/// Keys are tile units; `scale` converts those to the metres the ribbon builder
/// works in.
/// How far the fillet at vertex `i` of a raw MVT linestring reaches back along
/// each of its segments, in **tile units**.
///
/// The scale is uniform, so [`corner_cut`]'s answer in tile units times the scale
/// is its answer in metres — which is what lets [`street_nodes`] report the same
/// number `ribbon` will measure without converting the whole line first.
fn fillet_units(line: &[geo_types::Coord<f32>], i: usize, half_units: f32) -> f32 {
    if i == 0 || i + 1 >= line.len() {
        return 0.0;
    }
    let (a, p, b) = (line[i - 1], line[i], line[i + 1]);
    let (l_in, l_out) = ((p.x - a.x).hypot(p.y - a.y), (b.x - p.x).hypot(b.y - p.y));
    if l_in < 1e-6 || l_out < 1e-6 {
        return 0.0;
    }
    let cos =
        (((p.x - a.x) * (b.x - p.x) + (p.y - a.y) * (b.y - p.y)) / (l_in * l_out)).clamp(-1.0, 1.0);
    corner_cut(l_in, l_out, cos.acos(), half_units)
}

fn street_nodes(features: &[Feature], scale: f32) -> HashMap<(i32, i32), Vec<NodeArm>> {
    let mut nodes: HashMap<(i32, i32), Vec<NodeArm>> = HashMap::new();
    for (fi, feature) in features.iter().enumerate() {
        let Some((class, half)) = drawn_street(feature) else {
            continue;
        };
        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            _ => continue,
        }
        for ls in &lines {
            let n = ls.0.len();
            for (j, c) in ls.0.iter().enumerate() {
                if !c.x.is_finite() || !c.y.is_finite() {
                    continue;
                }
                // The road's direction through this node (sign-free — only the
                // crossing *angle* is ever read), from whichever adjacent
                // segment exists.
                let (ax, ay) = if j + 1 < n {
                    (ls.0[j + 1].x - c.x, ls.0[j + 1].y - c.y)
                } else if j > 0 {
                    (c.x - ls.0[j - 1].x, c.y - ls.0[j - 1].y)
                } else {
                    (0.0, 0.0)
                };
                let len = ax.hypot(ay);
                let d = if len > 1e-6 && len.is_finite() {
                    [ax / len, ay / len]
                } else {
                    [0.0, 0.0]
                };
                // At an end, `axis` already points from the node into the line
                // (`j + 1 < n` at the head, `j > 0` reversed at the tail), so
                // the leaving direction is the axis or its negation.
                //
                // The length is the one **`ribbon` will actually measure**, i.e.
                // after the fillet at the next vertex along has eaten its way
                // back into this segment — see [`corner_cut`]. Reporting the raw
                // length here and the cut one there is how the two halves of a
                // continuation would come to clamp their shared inner corner
                // against different numbers, which opens the crack the mitre
                // exists to close.
                let leaves = match (j, len > 1e-6 && len.is_finite()) {
                    (0, true) => Some((d, (len - fillet_units(&ls.0, 1, half / scale)) * scale)),
                    (j, true) if j + 1 == n => Some((
                        [-d[0], -d[1]],
                        (len - fillet_units(&ls.0, n - 2, half / scale)) * scale,
                    )),
                    _ => None,
                };
                let at = nodes.entry(node_key([c.x, c.y])).or_default();
                // One entry per feature: a way that doubles back through its own
                // node is not a junction with itself.
                if !at.iter().any(|a| a.feature == fi) {
                    at.push(NodeArm {
                        feature: fi,
                        half,
                        class,
                        axis: d,
                        leaves,
                    });
                }
            }
        }
    }
    nodes
}

fn node_key(p: [f32; 2]) -> (i32, i32) {
    (p[0].round() as i32, p[1].round() as i32)
}

/// Below this crossing sine (≈15°) the road at the far side of the node is not
/// **crossing** this one, it is *continuing* it, and a continuation needs no
/// stub at all: the two ribbons already abut exactly at the shared node.
///
/// This is the common case, not an edge one. Shortbread merges ways by
/// attribute, so an ordinary street arrives as a chain of features that meet
/// end to end — measured on the Groningen fixture, **58 % of all junction ends
/// are collinear joins like that** (418 of 722). They used to be the *worst*
/// case rather than a free one: the old form of this floored the sine at 0.35
/// and divided by it, so two collinear features each fired the longest stub the
/// cap allowed — 10–12 m of [`STUB_PLAIN_U`] tarmac laid over its neighbour,
/// from both sides. What that looks like on the ground is a street whose kerb
/// lines and centre dashes stop dead for twenty metres every time the name
/// changes, over a doubled ribbon that fights in depth. Flooring the sine
/// bounded the spear; it did not stop the code aiming one down every street.
const MERGE_SIN: f32 = 0.26;

/// The sine at and above which the stub is the honest `half / sin` (≈30°, so at
/// most `2 × half`). Between here and [`MERGE_SIN`] the stub ramps to zero, so
/// a shallow join loses its stub gradually rather than at a threshold that
/// would show as one street in a row of similar ones being drawn differently.
const JUNCTION_SIN: f32 = 0.5;

/// How far past `end` the feature's ribbon has to run to fill the junction
/// there, in metres: for each **other** road meeting at that node, its
/// half-width over the sine of the crossing angle — the distance to its far
/// kerb along *this* road's own direction — and the largest of those wins.
/// Plain `half` (what this used to return) is only right at 90°: a road
/// crossed at 30° left a notch on the acute side while overshooting on the
/// obtuse one.
///
/// Two ends get nothing. A **dead end** keeps a square end on the ground
/// instead of a flap of tarmac reaching into the field beyond it (nothing else
/// meets there, so the fold starts and finishes at `0.0`), and a **collinear
/// continuation** keeps its markings running through — see [`MERGE_SIN`].
fn junction_extent(
    nodes: &HashMap<(i32, i32), Vec<NodeArm>>,
    end: [f32; 2],
    feature_index: usize,
    out_dir: [f32; 2],
) -> f32 {
    // Bisecting switch: `CARTO_NO_STUB=1` lays every ribbon with a square end, so
    // a shape at a junction can be told apart from the stub that fills it. Read
    // once — this runs at every junction end, and the perf tests time it.
    #[cfg(test)]
    {
        static NO_STUB: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *NO_STUB.get_or_init(|| std::env::var("CARTO_NO_STUB").is_ok()) {
            return 0.0;
        }
    }
    nodes
        .get(&node_key(end))
        .map(|at| {
            at.iter()
                .filter(|a| a.feature != feature_index)
                .map(|a| {
                    let sin = (out_dir[0] * a.axis[1] - out_dir[1] * a.axis[0]).abs();
                    // The far-kerb distance, and how much of a *junction* this
                    // crossing angle actually is.
                    let reach = a.half / sin.max(JUNCTION_SIN);
                    let crossing = ((sin - MERGE_SIN) / (JUNCTION_SIN - MERGE_SIN)).clamp(0.0, 1.0);
                    reach * crossing
                })
                .fold(0.0, f32::max)
        })
        .unwrap_or(0.0)
        .min(JUNCTION_EXTEND_MAX)
}

/// The smallest cosine — i.e. the widest bend — at which two features that end
/// at the same node still count as one street continuing through it. 40° is
/// well past what a road bends at a single OSM vertex and comfortably short of
/// the 90° that would let a genuine side road be mistaken for a continuation.
const CONTINUATION_COS: f32 = 0.77;

/// The road that **continues** this one past `end`, if there is one: the
/// direction it leaves the node in, and how long its first segment is.
///
/// Shortbread merges ways by attribute, so one street reaches the tessellator as
/// a chain of features that meet end to end — 272 such joins in the Groningen
/// fixture, 55 of them turning by 10° or more. Each half used to stop dead at
/// the shared node with a square butt cap, and two square caps meeting at an
/// angle leave a wedge of bare ground on the outside of the bend and a doubled
/// overlap on the inside: measured up to 1.28 m across on a 7 m street, which is
/// most of what "the roads don't connect" looks like from the pavement. (It went
/// unnoticed for as long as it did because the old stub buried the seam under
/// ten metres of blank tarmac, which was worse.)
///
/// Mitring closes it exactly rather than approximately, and that is the point of
/// returning the neighbour's *segment length* alongside its direction: both
/// halves then feed [`ribbon`] the identical `(d_in, d_out)` pair and the
/// identical inner-backtrack budget, so both place their two end vertices on the
/// same two points. The join is watertight by construction, not by tolerance.
///
/// Only a road that **ends** here can continue this one (a road passing through
/// is being crossed, not joined), only one of the same class and width can
/// (grafting a 4 m service road onto a 22 m motorway is not a continuation), and
/// only one leaving in roughly the direction this one is pointing.
fn continuation(
    nodes: &HashMap<(i32, i32), Vec<NodeArm>>,
    end: [f32; 2],
    feature_index: usize,
    out_dir: [f32; 2],
    class: RoadClass,
    half: f32,
) -> Option<([f32; 2], f32)> {
    let mut best: Option<(f32, [f32; 2], f32)> = None;
    for arm in nodes.get(&node_key(end))? {
        if arm.feature == feature_index || arm.class != class {
            continue;
        }
        if (arm.half - half).abs() > 0.15 * half {
            continue;
        }
        let Some((leaves, seg_m)) = arm.leaves else {
            continue;
        };
        let cos = leaves[0] * out_dir[0] + leaves[1] * out_dir[1];
        if cos < CONTINUATION_COS {
            continue;
        }
        if best.is_none_or(|(c, _, _)| cos > c) {
            best = Some((cos, leaves, seg_m));
        }
    }
    best.map(|(_, leaves, seg_m)| (leaves, seg_m))
}

/// What a ribbon meets at one of its two ends. The two cases are exclusive by
/// construction — a street that continues through a node has no notch there for
/// a stub to fill, so [`tessellate_roads`] never asks for both.
#[derive(Clone, Copy, Default)]
struct EndJoin {
    /// Metres of junction stub past the end, from [`junction_extent`]: how far
    /// this road has to run *into* whatever it meets there. Zero at a dead end,
    /// at a tile border and wherever `cont` is set.
    extend: f32,
    /// The feature that continues this road past the end, from
    /// [`continuation`]: the direction it leaves in and its first segment's
    /// length. The end is mitred into it instead of butt-capped.
    cont: Option<([f32; 2], f32)>,
}

/// Below this turn there is no corner worth rounding: the kink is finer than the
/// ribbon's own kerb line is wide and rounding it would only add vertices.
const ROUND_MIN_DEG: f32 = 2.0;

/// Above this turn the corner is **real** and is kept. A street grid's right
/// angles, a hairpin and a switchback are all things the map is asserting; a
/// fillet there would cut the corner off a road that genuinely turns there, and
/// at a junction it would pull the carriageway away from the node it meets.
const ROUND_MAX_DEG: f32 = 60.0;

/// How far a fillet may pull the carriageway away from the vertex the map
/// recorded, in metres.
///
/// The whole licence this rounding takes is that a generalised vertex is an
/// artefact of drawing the map small, not a statement that the road has a kink
/// there. That licence runs out somewhere, and this is where: a metre is inside
/// the width of the lane the vertex sits in, so no fillet can move a road onto
/// ground the map says is something else.
const ROUND_MAX_OFFSET_M: f32 = 1.0;

/// How far back along each adjacent segment a fillet may reach, as a fraction of
/// the **shorter** of the two.
///
/// Well under the `INNER_BACKTRACK_BUDGET` the miter join is allowed, so a
/// rounded corner can never eat the segment its neighbour's join is measured
/// against.
const ROUND_TANGENT_FRAC: f32 = 0.35;

/// The largest turn one segment of a fillet may carry. The sagitta it leaves is
/// `r · (1 − cos(step/2))` — four centimetres on a five-metre radius at this
/// step, i.e. under the width of the painted line that runs along the edge.
const ROUND_STEP_DEG: f32 = 8.0;

/// How far back along its two segments the fillet at a corner reaches, in metres,
/// or `0` where the corner is kept.
///
/// Shared with [`street_nodes`], and that is not tidiness — it is the whole
/// reason the mitred continuation joins stay watertight. `ribbon` clamps the
/// inner corner against the *shorter* of the two segments meeting there, and at
/// a continuation one of those two comes over from the neighbouring feature as
/// `NodeArm::leaves`. If this rounding shortened the segment `ribbon` measures
/// while `NodeArm` still reported the raw one, the two halves of one street
/// would clamp against different numbers and land on different points — the
/// hairline crack `two_halves_of_one_street_meet_on_the_same_two_points` exists
/// to forbid. So `street_nodes` reports the length *after* this cut, computed by
/// this same function.
/// `half` is the carriageway's half-width, and it is what decides whether the
/// corner may be rounded at all — see the fold rule below.
fn corner_cut(prev_len: f32, next_len: f32, turn_rad: f32, half: f32) -> f32 {
    let deg = turn_rad.to_degrees();
    if !(ROUND_MIN_DEG..=ROUND_MAX_DEG).contains(&deg) {
        return 0.0;
    }
    let tan_half = (turn_rad * 0.5).tan();
    if tan_half <= 1e-6 {
        return 0.0;
    }
    // As far back as the two segments can spare…
    let budget = ROUND_TANGENT_FRAC * prev_len.min(next_len);
    // …but never so far that the carriageway leaves the vertex the map recorded
    // by more than [`ROUND_MAX_OFFSET_M`]. A fillet's departure from its corner
    // is `r · (sec(θ/2) − 1)`, and `t = r · tan(θ/2)`, so the tangent that lands
    // exactly on the allowance is this.
    let sec_minus_one = 1.0 / (turn_rad * 0.5).cos() - 1.0;
    let by_offset = if sec_minus_one > 1e-9 {
        ROUND_MAX_OFFSET_M / sec_minus_one * tan_half
    } else {
        f32::MAX
    };
    let t = budget.min(by_offset);
    // **A fillet tighter than the road is wide folds the inner edge through
    // itself.** The ribbon is offset `half` either side of the centre line, so
    // on the inside of an arc of radius `r` the edge has radius `r − half`: at
    // `r < half` that is negative and the strip turns inside out — the doubled,
    // self-intersecting triangles `a_tight_corner_does_not_fold_the_ribbon`
    // exists to forbid. `r = t / tan(θ/2)`, so the corner is roundable only
    // while `t ≥ half · tan(θ/2)`, and where it is not the kink is kept. That is
    // what makes this safe on a 22 m motorway and generous on an alley, with one
    // rule rather than a width table.
    if t < half * tan_half {
        return 0.0;
    }
    t
}

/// Replace each moderate kink in a centre line with a circular fillet.
///
/// **A road is not a polygon, and at street level this is what says so.**
/// Shortbread generalises its geometry for a map read from above, where a 20°
/// kink in a street is a pixel; extruded into a ribbon and seen from the
/// pavement it is a visible corner in the kerb and in the painted edge line, and
/// the whole street reads as a chain of straight boards. Measured on the
/// Groningen z14 fixture, **79 % of interior vertices turn by 2–70° and 49 % by
/// ten degrees or more** — this is the ordinary case, not an edge one.
///
/// Three properties are load-bearing:
///
/// * **The endpoints never move.** A road's ends are its junction nodes, and
///   every join in this module — the mitred continuation, the junction stub, the
///   half-open tile ownership — is anchored on them.
/// * **The end *directions* never change either.** A fillet at the second vertex
///   pulls that vertex back *along* the first segment, so the first segment gets
///   shorter and keeps its bearing. `junction_extent` and `continuation` read
///   the bearing and are untouched.
/// * **A sharp corner is kept** ([`ROUND_MAX_DEG`]). Rounding a right angle
///   would move the carriageway off a corner the map is asserting is there.
fn round_corners(pts: &[[f32; 2]], half: f32) -> Vec<[f32; 2]> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let len_of = |a: [f32; 2], b: [f32; 2]| (b[0] - a[0]).hypot(b[1] - a[1]);
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(pts.len() * 2);
    out.push(pts[0]);
    for i in 1..pts.len() - 1 {
        let (p, a, b) = (pts[i], pts[i - 1], pts[i + 1]);
        let (l_in, l_out) = (len_of(a, p), len_of(p, b));
        if l_in < 1e-4 || l_out < 1e-4 {
            continue;
        }
        let d_in = [(p[0] - a[0]) / l_in, (p[1] - a[1]) / l_in];
        let d_out = [(b[0] - p[0]) / l_out, (b[1] - p[1]) / l_out];
        let cos = (d_in[0] * d_out[0] + d_in[1] * d_out[1]).clamp(-1.0, 1.0);
        let turn = cos.acos();
        let t = corner_cut(l_in, l_out, turn, half);
        if t <= 0.0 {
            out.push(p);
            continue;
        }
        // The fillet runs from `t` back along the incoming segment to `t` along
        // the outgoing one. Sampled as a quadratic Bézier through the corner,
        // which is tangent to both segments at exactly those two points and is
        // within a fraction of a millimetre of the true arc over this angle —
        // and needs no centre, no radius and no sign of the turn.
        let start = [p[0] - d_in[0] * t, p[1] - d_in[1] * t];
        let end = [p[0] + d_out[0] * t, p[1] + d_out[1] * t];
        let steps = (turn.to_degrees() / ROUND_STEP_DEG).ceil().max(1.0) as usize;
        for k in 0..=steps {
            let s = k as f32 / steps as f32;
            let w = 1.0 - s;
            out.push([
                w * w * start[0] + 2.0 * w * s * p[0] + s * s * end[0],
                w * w * start[1] + 2.0 * w * s * p[1] + s * s * end[1],
            ]);
        }
    }
    out.push(pts[pts.len() - 1]);
    // Consecutive duplicates would produce a NaN normal downstream; a fillet
    // whose tangent point lands on its neighbour's is the case that can make one.
    out.dedup_by(|a, b| len_of(*a, *b) <= 1e-4);
    out
}

/// Extrude a centre line into a flat ribbon of `2 × half` metres at height `y`,
/// writing positions, UVs and indices into shared buffers.
///
/// Joins are mitered from the average of the two adjacent segment normals, which
/// keeps the carriageway a constant width around a bend — the naive
/// quad-per-segment alternative leaves a wedge of gap on the outside of every
/// corner, and OSM road geometry is nothing but corners.
///
/// **The two sides of a corner are not symmetric**, and treating them as one
/// number is what made junctions look bad. A single `ext` along the bisector,
/// clamped by [`MITER_LIMIT`], fails at both ends of the range:
///
/// * **Outside.** Clamping pulls the *outer* edge in along with the inner one, so
///   a sharp bend necks the carriageway down instead of just blunting the corner:
///   a 22 m motorway measured 14.2 m through a 150° turn and 4.8 m through a 170°
///   hairpin. The outer corner therefore keeps the full half-width and is
///   **bevelled** past the limit — two vertices, one square on each segment, with
///   a fill triangle across the gap. That cannot spike (the same reasoning
///   `systems::navigation::ribbon` documents for the route line) and cannot pinch.
/// * **Inside.** The inner miter backtracks `half / tan(θ/2)` along *both*
///   adjacent segments, and nothing bounded it against how long they actually
///   are. On a 22 m motorway a 60° turn overruns a 6 m segment — and z14
///   Shortbread geometry is full of sub-10 m segments — so the ribbon folded
///   through itself: doubled triangles whose two layers carry different `v`, which
///   is the centre line fighting its own reflection along the bend. The inner
///   extension is capped at [`INNER_BACKTRACK_BUDGET`] of the shorter neighbour.
///   A capped inner corner is slightly narrow on the inside of a turn tighter than
///   the road is wide; a fold is a rendering artefact.
///
/// `y` is the height of the ribbon's plane, and for a road it is **always zero**:
/// the ladder that keeps two overlapping ribbons from flickering against each
/// other at a junction is a depth offset carried per vertex ([`road_rung`]), not
/// a lift. The parameter survives because the tests lay ribbons at other heights.
///
/// `ends` is what the ribbon meets at its head and its tail — see [`EndJoin`].
///
/// `ring` says the centre line closes on itself — a roundabout, a turning
/// circle, a loop road. Its two "ends" are the same point and are not ends at
/// all, so each takes its miter from the segment on the other side of the join
/// instead of a butt cap. Without that the ring meets itself at an angle with
/// two square ends: a wedge of bare ground bitten out of the outer kerb and a
/// doubled overlap on the inner one, which is what put a notch in every
/// roundabout. Position and width match on both sides of the seam, so the
/// closure is watertight; only `v` wraps, and one texture discontinuity
/// somewhere on a loop is unavoidable unless its length happens to be a whole
/// number of texture repeats.
fn ribbon(
    points: &[[f32; 2]],
    half: f32,
    y: f32,
    tex_len_m: f32,
    ends: [EndJoin; 2],
    ring: bool,
    positions: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
) -> RibbonEdges {
    let extend = [ends[0].extend, ends[1].extend];
    // Consecutive duplicates have no direction and would produce NaN normals.
    let mut pts: Vec<[f32; 2]> = Vec::with_capacity(points.len());
    for p in points {
        if pts
            .last()
            .is_none_or(|q: &[f32; 2]| (p[0] - q[0]).hypot(p[1] - q[1]) > 1e-4)
        {
            pts.push(*p);
        }
    }
    if pts.len() < 2 || half <= 0.0 {
        return RibbonEdges::default();
    }
    // A ring needs three distinct points plus the repeat to have an interior;
    // and the caller's flag is only a hint until the geometry agrees with it
    // (a clipped run of a closed line is an ordinary open polyline).
    let ring = ring
        && pts.len() >= 4
        && (pts[0][0] - pts[pts.len() - 1][0]).hypot(pts[0][1] - pts[pts.len() - 1][1]) <= 1e-4;
    // A generalised centre line is a chain of straight segments, and the ribbon
    // built on it inherits every kink as a corner in the kerb and in the painted
    // edge line. Round them first — see `round_corners`. Both of `kerb_band`'s
    // runs come through here, so a carriageway and its kerb are rounded from the
    // same points and cannot drift apart.
    //
    // **Not a ring.** A roundabout's polygon is not a generalisation artefact
    // the way a kink in a street is: its vertices lie *on* the circle the
    // junction actually is, so cutting its corners would shrink the roundabout
    // rather than smooth it. Its seam is already mitred closed.
    if !ring {
        pts = round_corners(&pts, half);
        if pts.len() < 2 {
            return RibbonEdges::default();
        }
    }
    let last = pts.len() - 1;

    let dir = |a: [f32; 2], b: [f32; 2]| -> [f32; 2] {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy).max(1e-6);
        [dx / len, dy / len]
    };

    let seg_len =
        |i: usize| -> f32 { (pts[i + 1][0] - pts[i][0]).hypot(pts[i + 1][1] - pts[i][1]) };
    // The `(u = 0, u = 1)` vertex pair the *incoming* quad closes on and the pair
    // the *outgoing* quad opens from, per point. A miter join shares one pair; a
    // bevel splits the outer vertex in two and keeps the inner one shared, so the
    // strip can no longer be indexed as `base + i * 2`.
    let mut joins: Vec<((u32, u32), (u32, u32))> = Vec::with_capacity(pts.len());
    let mut travelled = 0.0f32;

    for i in 0..pts.len() {
        if i > 0 {
            travelled += (pts[i][0] - pts[i - 1][0]).hypot(pts[i][1] - pts[i - 1][1]);
        }
        // On a ring the first point's incoming segment is the *last* one and the
        // last point's outgoing segment is the first, so both ends miter into
        // the join rather than butt-ending against it. Failing that, an end with
        // a continuation borrows the neighbouring feature's direction: `leaves`
        // points *away* from the node into that road, so the traffic arriving
        // along it runs the other way.
        let d_in = if i > 0 {
            dir(pts[i - 1], pts[i])
        } else if ring {
            dir(pts[last - 1], pts[last])
        } else if let Some((leaves, _)) = ends[0].cont {
            [-leaves[0], -leaves[1]]
        } else {
            dir(pts[0], pts[1])
        };
        let d_out = if i + 1 < pts.len() {
            dir(pts[i], pts[i + 1])
        } else if ring {
            dir(pts[0], pts[1])
        } else if let Some((leaves, _)) = ends[1].cont {
            leaves
        } else {
            d_in
        };
        // Left normal of a direction in tile space (x east, y south).
        let n_in = [-d_in[1], d_in[0]];
        let n_out = [-d_out[1], d_out[0]];
        let mut m = [n_in[0] + n_out[0], n_in[1] + n_out[1]];
        let m_len = m[0].hypot(m[1]);
        // A perfect reversal (a spike in the source geometry) cancels the two
        // normals; fall back to the incoming one rather than dividing by zero.
        if m_len < 1e-4 {
            m = n_in;
        } else {
            m = [m[0] / m_len, m[1] / m_len];
        }
        // Lengthen the miter so the *edge* stays `half` from the centre line
        // through the join.
        let cos_half = (m[0] * n_out[0] + m[1] * n_out[1]).abs().max(1e-3);
        let miter = 1.0 / cos_half;

        // Which side of the bisector the corner opens away from. `m` points into
        // the inside of the turn when the cross product is positive, so the outer
        // side is the other one; a straight point has no inside and takes neither
        // branch below.
        let cross = d_in[0] * d_out[1] - d_in[1] * d_out[0];
        let inner_s = if cross > 0.0 { 1.0f32 } else { -1.0f32 };
        let outer_s = -inner_s;

        // Outside: full half-width always, bevelled rather than stretched.
        let ext_outer = half * miter.min(MITER_LIMIT);
        // Inside: the same miter, but never further back than the segments it
        // eats into are long.
        let back_per_m = (m[0] * d_in[0] + m[1] * d_in[1]).abs();
        // The neighbour's segment length comes over with its direction, so both
        // halves of a continuation clamp the inner corner against the same pair
        // of lengths and land on the same vertex.
        let prev_len = if i > 0 {
            Some(seg_len(i - 1))
        } else if ring {
            Some(seg_len(last - 1))
        } else {
            ends[0].cont.map(|(_, seg_m)| seg_m)
        };
        let next_len = if i + 1 < pts.len() {
            Some(seg_len(i))
        } else if ring {
            Some(seg_len(0))
        } else {
            ends[1].cont.map(|(_, seg_m)| seg_m)
        };
        let neighbour = match (prev_len, next_len) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => 0.0,
        };
        let mut ext_inner = half * miter;
        let budget = INNER_BACKTRACK_BUDGET * neighbour;
        if back_per_m * ext_inner > budget {
            ext_inner = budget / back_per_m.max(1e-3);
        }

        let v = travelled / tex_len_m;
        // `u = 0` is the `-m` side and `u = 1` the `+m` side — the across-road
        // axis the centre line registers against (see `road_texture`).
        let (u_outer, u_inner) = if outer_s > 0.0 {
            (1.0, 0.0)
        } else {
            (0.0, 1.0)
        };
        let bevelled = miter > MITER_LIMIT;

        let inner_p = [
            pts[i][0] + inner_s * m[0] * ext_inner,
            pts[i][1] + inner_s * m[1] * ext_inner,
        ];
        // One outer vertex for a miter, two square ones for a bevel.
        let (outer_in_p, outer_out_p) = if bevelled {
            (
                [
                    pts[i][0] + outer_s * n_in[0] * half,
                    pts[i][1] + outer_s * n_in[1] * half,
                ],
                [
                    pts[i][0] + outer_s * n_out[0] * half,
                    pts[i][1] + outer_s * n_out[1] * half,
                ],
            )
        } else {
            let p = [
                pts[i][0] + outer_s * m[0] * ext_outer,
                pts[i][1] + outer_s * m[1] * ext_outer,
            ];
            (p, p)
        };

        let inner = positions.len() as u32;
        positions.push([inner_p[0], y, inner_p[1]]);
        uvs.push([u_inner, v]);
        let outer_in = positions.len() as u32;
        positions.push([outer_in_p[0], y, outer_in_p[1]]);
        uvs.push([u_outer, v]);
        let outer_out = if bevelled {
            let idx = positions.len() as u32;
            positions.push([outer_out_p[0], y, outer_out_p[1]]);
            uvs.push([u_outer, v]);
            idx
        } else {
            outer_in
        };

        // Order a pair as (u = 0 vertex, u = 1 vertex) so the strip stitches the
        // same way whichever direction the road turns.
        let pair = |outer: u32| {
            if outer_s > 0.0 {
                (inner, outer)
            } else {
                (outer, inner)
            }
        };
        joins.push((pair(outer_in), pair(outer_out)));

        if bevelled {
            // The wedge between the two square ends, filled back to the shared
            // inner vertex. This is the bevel.
            push_triangle_up(positions, indices, outer_in, outer_out, inner);
        }
        if i > 0 {
            let ((a0, a1), (b0, b1)) = (joins[i - 1].1, joins[i].0);
            push_triangle_up(positions, indices, a0, b0, b1);
            push_triangle_up(positions, indices, a0, b1, a1);
        }
    }

    // The area this ribbon covers, read straight off the strip it just laid.
    //
    // `joins` holds, per centre-line point, the `(u = 0, u = 1)` vertex pair the
    // incoming quad closes on and the pair the outgoing one opens from — so the
    // two sides of the ribbon *are* the two chains through those pairs, and the
    // boundary is one walked forward and the other walked back. Reading it out of
    // the strip rather than recomputing the offsets is the point: `road_stitch`
    // cuts against exactly the triangles that were laid, never against a second
    // opinion about where they are.
    let n = joins.len();
    // On a ring the last join is the first one again; keeping it would close the
    // contour on a doubled vertex.
    let span = if ring { n.saturating_sub(1) } else { n };
    let mut side_a: Vec<[f32; 2]> = Vec::with_capacity(span * 2);
    let mut side_b: Vec<[f32; 2]> = Vec::with_capacity(span * 2);
    fn push_flat(chain: &mut Vec<[f32; 2]>, positions: &[[f32; 3]], idx: u32) {
        let p = positions[idx as usize];
        let q = [p[0], p[2]];
        if chain
            .last()
            .is_none_or(|l: &[f32; 2]| (l[0] - q[0]).hypot(l[1] - q[1]) > 1e-6)
        {
            chain.push(q);
        }
    }
    for j in joins.iter().take(span) {
        push_flat(&mut side_a, positions, j.0 .0);
        push_flat(&mut side_a, positions, j.1 .0);
        push_flat(&mut side_b, positions, j.0 .1);
        push_flat(&mut side_b, positions, j.1 .1);
    }
    let sides = [side_a.clone(), side_b.clone()];
    let mut contours: Vec<Vec<[f32; 2]>> = Vec::new();
    if ring {
        // A closed ribbon is an annulus, so its two sides are two separate
        // contours — the outer boundary and the hole in the middle of the
        // roundabout — not one loop walked out and back.
        contours.push(side_a);
        contours.push(side_b);
    } else {
        side_b.reverse();
        side_a.extend(side_b);
        contours.push(side_a);
    }

    // Junction stubs, as standalone quads rather than extra strip points, for
    // two reasons. They sample [`STUB_PLAIN_U`] — a marking-free column of the
    // class texture — so the kerb lines and centre dashes *break* at the
    // junction instead of being ruled straight across the road being joined,
    // ending in a hard white T at its far kerb (the single most wrong-looking
    // thing about the old junctions). And a standalone quad reuses the strip's
    // own butt-end coordinates (the ends are unmitred: `d_out == d_in` there),
    // so the seam is watertight by construction.
    let total = travelled;
    let mut stub_contours: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut stub = |end: [f32; 2], d: [f32; 2], len: f32, v0: f32| {
        let n = [-d[1], d[0]];
        let far = [end[0] + d[0] * len, end[1] + d[1] * len];
        let v1 = v0 + len / tex_len_m;
        let base = positions.len() as u32;
        let corners = [
            (
                [end[0] + n[0] * half, end[1] + n[1] * half],
                STUB_PLAIN_U.0,
                v0,
            ),
            (
                [end[0] - n[0] * half, end[1] - n[1] * half],
                STUB_PLAIN_U.1,
                v0,
            ),
            (
                [far[0] - n[0] * half, far[1] - n[1] * half],
                STUB_PLAIN_U.1,
                v1,
            ),
            (
                [far[0] + n[0] * half, far[1] + n[1] * half],
                STUB_PLAIN_U.0,
                v1,
            ),
        ];
        for (p, u, v) in corners {
            positions.push([p[0], y, p[1]]);
            uvs.push([u, v]);
        }
        stub_contours.push(corners.iter().map(|(p, _, _)| *p).collect());
        push_triangle_up(positions, indices, base, base + 1, base + 2);
        push_triangle_up(positions, indices, base, base + 2, base + 3);
    };
    if extend[0] > 0.0 {
        stub(pts[0], dir(pts[1], pts[0]), extend[0], 0.0);
    }
    if extend[1] > 0.0 {
        stub(
            pts[last],
            dir(pts[last - 1], pts[last]),
            extend[1],
            total / tex_len_m,
        );
    }
    drop(stub);
    contours.extend(stub_contours);
    // One winding for every solid contour and the other for a hole, because the
    // boolean below reads them under a non-zero fill rule and a contour wound the
    // wrong way there subtracts itself from its own neighbours.
    let outer = contours
        .iter()
        .map(|c| ring_area(c).abs())
        .fold(0.0f32, f32::max);
    for c in &mut contours {
        let a = ring_area(c);
        // The roundabout's hole is the one contour that must run the other way;
        // it is the small one, and `ring` is the only thing that produces a pair.
        let want_positive = !ring || ring_area(c).abs() >= outer - f32::EPSILON;
        if (a > 0.0) != want_positive {
            c.reverse();
        }
    }
    contours.retain(|c| c.len() >= 3);
    RibbonEdges { contours, sides }
}

/// The boundary of what a ribbon covers, as [`ribbon`] laid it.
///
/// Both fields exist for [`kerb_band`], which is the only caller that reads them:
/// `sides` are the two offset chains, one per side of the centre line and in
/// centre-line order, which it zips against a second, wider pair to make the kerb
/// a band rather than a slab; `contours` is the closed form, whose entries past
/// the strip itself are the junction stub quads.
///
/// **`road_stitch` does not use either.** It cuts the ribbons' *triangles* — the
/// geometry about to be drawn — rather than a separate description of where that
/// geometry is, which is one fewer thing that can disagree.
#[derive(Default)]
struct RibbonEdges {
    contours: Vec<Vec<[f32; 2]>>,
    sides: [Vec<[f32; 2]>; 2],
}

/// Lay a road's kerb band: the strip of [`ROAD_CASING_M`] outside each edge of
/// the carriageway, and **not** the carriageway itself.
///
/// The kerb used to be a second ribbon the full width of road-plus-kerb, drawn
/// underneath the tarmac. That is a slab, and it made every carriageway pixel in
/// the tile a place where two opaque coplanar surfaces met — 70 % of the road
/// sheet, and by a long way the largest overlap in the scene. `road_stitch` will
/// cut it back to the two side strips whatever this does, but cutting a shape you
/// did not have to draw is work: laying the slab and subtracting it again was 60 %
/// of the whole cut's cost on the Groningen fixture.
///
/// The band is zipped between two runs of [`ribbon`] rather than offset by hand,
/// and that is what makes its inner edge *exactly* the carriageway's outer edge:
/// the same join code, the same miter, the same bevel decision and the same inner
/// backtrack clamp produce both, so the two meet on shared coordinates and the
/// seam cannot open. If the two runs ever disagree about how many points a side
/// has — they cannot today, since every one of those decisions is independent of
/// the width — this falls back to the slab, which `road_stitch` then cuts as
/// before. Slower, never wrong.
///
/// Kerb geometry carries no texture: `RoadCasing`'s material is flat-coloured
/// (see `road_base_material`), so the texture coordinates here only have to
/// exist.
#[allow(clippy::too_many_arguments)]
fn kerb_band(
    points: &[[f32; 2]],
    half: f32,
    tex_len_m: f32,
    ends: [EndJoin; 2],
    kerb_ends: [EndJoin; 2],
    ring: bool,
    positions: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
) {
    let mut sink = (Vec::new(), Vec::new(), Vec::new());
    let inner = ribbon(
        points,
        half,
        0.0,
        tex_len_m,
        ends,
        ring,
        &mut sink.0,
        &mut sink.1,
        &mut sink.2,
    );
    sink = (Vec::new(), Vec::new(), Vec::new());
    let outer = ribbon(
        points,
        half + ROAD_CASING_M,
        0.0,
        tex_len_m,
        kerb_ends,
        ring,
        &mut sink.0,
        &mut sink.1,
        &mut sink.2,
    );
    let zips = inner
        .sides
        .iter()
        .zip(outer.sides.iter())
        .all(|(a, b)| a.len() == b.len() && a.len() >= 2);
    if !zips {
        // Fall back to the slab; the cut removes the doubled area either way.
        let base = positions.len() as u32;
        positions.append(&mut sink.0);
        uvs.append(&mut sink.1);
        indices.extend(sink.2.iter().map(|i| i + base));
        return;
    }

    for (a, b) in inner.sides.iter().zip(outer.sides.iter()) {
        let base = positions.len() as u32;
        for (k, (p, q)) in a.iter().zip(b.iter()).enumerate() {
            let v = k as f32 / (a.len() - 1) as f32;
            positions.push([p[0], 0.0, p[1]]);
            uvs.push([0.0, v]);
            positions.push([q[0], 0.0, q[1]]);
            uvs.push([1.0, v]);
        }
        for k in 0..a.len() - 1 {
            let (i0, o0) = (base + (k as u32) * 2, base + (k as u32) * 2 + 1);
            let (i1, o1) = (i0 + 2, o0 + 2);
            push_triangle_up(positions, indices, i0, o0, o1);
            push_triangle_up(positions, indices, i0, o1, i1);
        }
    }
    // A kerb also wraps the opening of a junction stub, where the carriageway ends
    // square against the road it joins rather than being mitred into it. Those
    // patches are not side strips, so they are laid whole — as the quads `ribbon`
    // made them — and the general cut takes the carriageway back out of them.
    // There are at most two per ribbon, so the cut has almost nothing to do.
    let strips = if ring { 2 } else { 1 };
    for stub in outer.contours.into_iter().skip(strips) {
        if stub.len() != 4 {
            continue;
        }
        let base = positions.len() as u32;
        for p in &stub {
            positions.push([p[0], 0.0, p[1]]);
            uvs.push([0.0, 0.0]);
        }
        push_triangle_up(positions, indices, base, base + 1, base + 2);
        push_triangle_up(positions, indices, base, base + 2, base + 3);
    }
}

/// Twice the signed area of a closed ring — positive counter-clockwise in the
/// tile's (x east, y south) frame. Only the sign and the ordering of magnitudes
/// are used, so the factor of two is left in.
fn ring_area(ring: &[[f32; 2]]) -> f32 {
    let mut acc = 0.0f32;
    for i in 0..ring.len() {
        let a = ring[i];
        let b = ring[(i + 1) % ring.len()];
        acc += a[0] * b[1] - b[0] * a[1];
    }
    acc
}

/// Append a triangle wound so its face points **+Y**, whatever order the caller
/// supplies. Ground geometry is single-sided and every surface here faces up, so
/// the winding is derived rather than reasoned about at each call site (the same
/// rule [`tessellate_polygon`] applies to its earcut output).
fn push_triangle_up(positions: &[[f32; 3]], indices: &mut Vec<u32>, a: u32, b: u32, c: u32) {
    let (pa, pb, pc) = (
        positions[a as usize],
        positions[b as usize],
        positions[c as usize],
    );
    let cross = (pb[0] - pa[0]) * (pc[2] - pa[2]) - (pb[2] - pa[2]) * (pc[0] - pa[0]);
    if cross.abs() < f32::EPSILON {
        return; // degenerate — a zero-area triangle only costs raster time
    }
    if cross < 0.0 {
        indices.extend([a, b, c]);
    } else {
        indices.extend([a, c, b]);
    }
}

/// Clip a polyline to the square `[lo, hi]²`, returning the runs that survive.
///
/// MVT geometry runs past the tile edge into a buffer zone so that a stroked line
/// has something to join to; kept as-is, both neighbours would draw the same
/// stretch of road and the overlap z-fights. Clipping cuts each crossing at the
/// border, so the two tiles' ribbons abut. A run breaks whenever a segment leaves
/// and re-enters, which is why the result is a list rather than one line.
pub(crate) fn clip_polyline(points: &[[f32; 2]], lo: f32, hi: f32) -> Vec<Vec<[f32; 2]>> {
    let mut runs: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    let flush = |cur: &mut Vec<[f32; 2]>, runs: &mut Vec<Vec<[f32; 2]>>| {
        if cur.len() >= 2 {
            runs.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };

    for seg in points.windows(2) {
        match clip_segment(seg[0], seg[1], lo, hi) {
            None => flush(&mut cur, &mut runs),
            Some((a, b)) => {
                let continues = cur
                    .last()
                    .is_some_and(|q| (q[0] - a[0]).hypot(q[1] - a[1]) <= 1e-4);
                if !continues {
                    flush(&mut cur, &mut runs);
                    cur.push(a);
                }
                cur.push(b);
            }
        }
    }
    flush(&mut cur, &mut runs);
    runs
}

/// Liang–Barsky segment clip against the square `[lo, hi]²`. `None` if the
/// segment misses the square entirely.
fn clip_segment(a: [f32; 2], b: [f32; 2], lo: f32, hi: f32) -> Option<([f32; 2], [f32; 2])> {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [
        (-dx, a[0] - lo),
        (dx, hi - a[0]),
        (-dy, a[1] - lo),
        (dy, hi - a[1]),
    ] {
        if p == 0.0 {
            // Parallel to this edge: inside or entirely out, no clipping to do.
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let r = q / p;
        if p < 0.0 {
            if r > t1 {
                return None;
            }
            t0 = t0.max(r);
        } else {
            if r < t0 {
                return None;
            }
            t1 = t1.min(r);
        }
    }
    if t1 <= t0 {
        return None;
    }
    Some((
        [a[0] + dx * t0, a[1] + dy * t0],
        [a[0] + dx * t1, a[1] + dy * t1],
    ))
}

/// Earcut one polygon (exterior + holes) into an existing position/index buffer.
pub(crate) fn tessellate_polygon(
    poly: &geo_types::Polygon<f32>,
    scale: f32,
    extent: f32,
    positions: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
) {
    // MVT geometry runs past the tile edge into a buffer zone, so neighbouring
    // tiles would overlap — and coplanar overlapping ground z-fights. [`clip_ring`]
    // cuts the overhang off against the border, which is invisible because the
    // adjacent tile continues the same surface. It returns the ring open (earcut
    // wants it open; a repeated vertex there produces degenerate triangles) and
    // deduplicated.
    let ring_coords = |ring: &geo_types::LineString<f32>| -> Vec<f64> {
        let raw: Vec<[f32; 2]> = ring.0.iter().map(|c| [c.x, c.y]).collect();
        clip_ring(&raw, extent)
            .iter()
            .flat_map(|p| [f64::from(p[0]), f64::from(p[1])])
            .collect()
    };

    let mut flat = ring_coords(poly.exterior());
    if flat.len() < 6 {
        return; // fewer than 3 points
    }
    let mut holes = Vec::new();
    for interior in poly.interiors() {
        let h = ring_coords(interior);
        if h.len() < 6 {
            continue;
        }
        holes.push(flat.len() / 2);
        flat.extend(h);
    }

    let Ok(tris) = earcutr::earcut(&flat, &holes, 2) else {
        return;
    };

    let base = positions.len() as u32;
    positions.extend(
        flat.chunks_exact(2)
            .map(|c| [c[0] as f32 * scale, 0.0, c[1] as f32 * scale]),
    );
    // Earcut keeps the input ring's orientation, which MVT does not guarantee to be
    // the one we want; a ground surface always faces up, so wind for +Y.
    for t in tris.chunks_exact(3) {
        let p = |i: usize| -> [f32; 3] {
            [
                flat[i * 2] as f32 * scale,
                0.0,
                flat[i * 2 + 1] as f32 * scale,
            ]
        };
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        // Cross product's Y component decides the winding (all y are 0 here).
        let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
        if cross < 0.0 {
            indices.extend([base + t[0] as u32, base + t[1] as u32, base + t[2] as u32]);
        } else {
            indices.extend([base + t[0] as u32, base + t[2] as u32, base + t[1] as u32]);
        }
    }
}

/// Layers whose polygons are water. Shared by [`tessellate_surfaces`],
/// [`water_shore_field`] and [`ocean_mask`] so the meshes, the shore field and
/// the globe's ocean can never disagree about what counts as water — a mismatch
/// would put a shoreline in open water.
pub const WATER_LAYERS: [&str; 2] = ["ocean", "water_polygons"];

/// The layers whose presence means the cell holds water of any kind:
/// [`WATER_LAYERS`] plus `water_lines`, which is drawn as a ribbon rather than
/// filled and so is deliberately not in that list.
pub const WATER_HINT_LAYERS: [&str; 3] = ["ocean", "water_polygons", "water_lines"];

/// Does this **undecoded** tile body mention a water layer at all?
///
/// A conservative pre-filter, not a parse, and the asymmetry is the whole point:
/// a false positive costs a caller one request it did not need, and a false
/// negative is impossible, because an MVT stores each layer's name verbatim as a
/// UTF-8 string in the body — a layer that exists cannot fail to be mentioned.
///
/// It exists because the decode belongs on a worker and this question has to be
/// answered before it. `map_geometry` fetches a cell's tile on the I/O pool and
/// hands the bytes to [`platform::workers`] to parse (ADR-033), so a consumer
/// that wants to skip a *network request* for a dry cell — `systems::nav_marks`
/// is the one — cannot wait for [`TileData::parse`] without moving a ~120 ms
/// decode onto the frame loop on wasm.
///
/// [`platform::workers`]: https://code.garage44.eu/jeroen/cartopolis
#[must_use]
pub fn body_mentions_water(body: &[u8]) -> bool {
    WATER_HINT_LAYERS
        .iter()
        .any(|name| contains_bytes(body, name.as_bytes()))
}

/// The `streets` `kind` values that are track rather than road. Every one of
/// them contains `rail` except `tram`, which is why [`body_mentions_rail`] tests
/// for two needles and not four.
const RAIL_HINTS: [&str; 2] = ["rail", "tram"];

/// Does this **undecoded** tile body mention a railway at all?
///
/// [`body_mentions_water`]'s sibling, for [`crate::crossings`], and it carries
/// the same asymmetry with one difference worth stating: water is a **layer
/// name**, while rail is a `kind` **value** inside `streets`. Both are stored
/// verbatim as UTF-8 in the body — a layer in its name table, a value in its
/// value table — so a false negative is impossible either way: a tile holding a
/// `kind = rail` street cannot fail to contain the bytes `rail`.
///
/// The false *positives* differ, and are wider here. `light_rail` contains
/// `rail`, which is wanted; a street **named** "Railroad Avenue", a POI called
/// "Tramhalte" or an attribute key nobody here reads all match too, and are not.
/// Each of those costs one request that comes back with no crossings in it —
/// which is the trade this predicate exists to make, and the right way round: a
/// missed railway would silently empty a layer, and a spare request over a street
/// with `tram` in its name would not.
#[must_use]
pub fn body_mentions_rail(body: &[u8]) -> bool {
    RAIL_HINTS
        .iter()
        .any(|name| contains_bytes(body, name.as_bytes()))
}

/// Naive substring search. The bodies are at most [`MAX_TILE_BYTES`] and the
/// needles are three short distinctive words, so this is a few hundred
/// microseconds and needs no dependency.
fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    let first = needle[0];
    hay[..=hay.len() - needle.len()]
        .iter()
        .enumerate()
        .any(|(i, b)| *b == first && &hay[i..i + needle.len()] == needle)
}

/// Rasterise just the water coverage of a tile: `TILE_PX²` bytes, 255 where the
/// surface is water and 0 where it is land, in the same frame (and under the same
/// overzoom [`SubTile`]) as [`render_mvt`].
///
/// This is the globe tier's half of the water story (`systems::globe_ocean`).
/// From orbit a wave is many orders of magnitude below a pixel, so nothing about
/// the local wave shader transfers — what makes an ocean read as an ocean from
/// space is that it is *glossy and dark* where the land is matte, and that needs
/// one bit per texel, not a height field.
///
/// Anti-aliased on purpose: the mask feeds a roughness blend, and a hard-edged
/// coastline would put a stair-stepped gloss boundary along every shore.
pub fn ocean_mask(tile: &TileData, sub: SubTile) -> Result<Vec<u8>, String> {
    ocean_mask_sized(tile, sub, TILE_PX)
}

/// [`ocean_mask`] at an arbitrary edge length. It rides in the alpha channel of
/// the imagery, so it must be rasterised at whatever size
/// [`render_mvt_sized`] was given — a mask of a different length is dropped by
/// the caller and the tile silently loses its ocean.
pub fn ocean_mask_sized(tile: &TileData, sub: SubTile, size: u32) -> Result<Vec<u8>, String> {
    let mut pixmap = Pixmap::new(size, size).ok_or_else(|| "pixmap alloc failed".to_string())?;
    let paint = Paint {
        anti_alias: true,
        shader: Shader::SolidColor(tiny_skia::Color::WHITE),
        ..Paint::default()
    };

    let f = sub.factor as f32;
    let px = size as f32;
    for meta in tile.layers() {
        if !WATER_LAYERS.contains(&meta.name.as_str()) {
            continue;
        }
        let scale = px * f / TileData::extent(meta);
        let transform = Transform::from_row(
            scale,
            0.0,
            0.0,
            scale,
            -(sub.x as f32) * px,
            -(sub.y as f32) * px,
        );
        for feature in tile.features(meta) {
            fill_geometry(&mut pixmap, &paint, &feature.geometry, &transform);
        }
    }

    Ok(pixmap.pixels().iter().map(|p| p.alpha()).collect())
}

/// Edge length of the shore-distance field. At a z14 tile's ~1.5 km this is
/// ~1.5 m per texel, which is what a 10 m city canal needs: at half that
/// resolution its entire width is three texels and the shoreline treatment reads
/// as a blocky stripe instead of a bank.
pub const SHORE_FIELD_PX: u32 = 1024;

/// Distance from shore at which the field saturates. Everything past this is
/// simply "open water", and 60 m is comfortably more than the width of the
/// canals and harbour basins the near-field treatment is for.
pub const SHORE_MAX_DIST_M: f32 = 60.0;

/// Apron of texels rasterised around the shore field so banks just across the
/// tile border are seen by the distance transform. Sized to the ground the MVT
/// buffer actually carries: Shortbread encodes a 64/4096-of-extent buffer, and
/// `1024 / 64 = 16` texels covers exactly that.
pub const SHORE_FIELD_PAD: u32 = SHORE_FIELD_PX / 64;

/// Edge length of the bank-obstruction field.
///
/// A quarter of [`SHORE_FIELD_PX`] — ~5.9 m per texel at z14 — because the thing
/// it measures is a building, and the smallest building in the tile is wider than
/// that. The shore field needs its 1.5 m to resolve a 10 m canal; this one is
/// read at a single point, through a linear sampler, by a term that then softens
/// its answer over [`SHORE_MAX_DIST_M`]-scale distances. At the shore field's
/// size it would be the largest image the client holds per water cell all over
/// again, for no more information.
pub const BANK_FIELD_PX: u32 = SHORE_FIELD_PX / 4;

/// The same apron as [`SHORE_FIELD_PAD`], at the bank field's resolution: a
/// building just over the tile line is exactly as much in the mirror as one just
/// inside it.
pub const BANK_FIELD_PAD: u32 = BANK_FIELD_PX / 64;

/// A tile's two distance fields, both in the same tile-local frame as
/// [`tessellate_surfaces`]' output (origin north-west, +x east, +y south) and
/// both quantised 0 at the seed to 255 at [`SHORE_MAX_DIST_M`] or beyond.
#[derive(Debug, Clone)]
pub struct ShoreField {
    /// Edge length of [`Self::data`].
    pub size: u32,
    /// Distance to the nearest **bank**: `size²` bytes, 0 where the water ends.
    pub data: Vec<u8>,
    /// Edge length of [`Self::bank`].
    pub bank_size: u32,
    /// Distance to the nearest **building**, at [`BANK_FIELD_PX`].
    ///
    /// This is the half of the bank-in-the-mirror term that used to be a guess.
    /// The shader asks what a reflected ray runs into on its way off the water,
    /// and the shore field can only answer "land" — so the term assumed a
    /// storey or two of quay stood on every bank, which over a city canal is
    /// true and over a lake with a mown bank is not. A reflection darkened by a
    /// building that is not there is not a reflection; it is a rim of shadow
    /// around every shore, and from a few hundred metres up that is exactly how
    /// it read.
    ///
    /// Buildings only, and deliberately not the `land` layer's wood and park
    /// polygons: those are canopy *areas* whose edge is where the trees start,
    /// a treeline dims a reflection rather than stopping it, and seeding the
    /// transform with them would put most of the rim straight back. All 255 for
    /// a tile with no buildings in it at all, which is the honest answer and
    /// also the one that switches the term off.
    pub bank: Vec<u8>,
}

/// Rasterise the tile's water polygons and turn them into a distance-to-shore
/// field — plus a coarser distance-to-building one — or `None` when the tile
/// holds no water at all.
///
/// This is the piece of information the geometry alone cannot give the water
/// shader. Earcut emits nothing but the polygon's own boundary vertices, so
/// every triangle corner sits *on* the shore and a per-vertex distance would be
/// identically zero; the answer has to come from the interior of the shape,
/// which means rasterising it. With it, the shader can shelter a canal from the
/// swell, colour the shallows and lap a foam line against the bank — the three
/// things that separate "a blue surface with waves on it" from water in a place.
///
/// Distances are measured against land inside this tile **and its MVT buffer**.
/// For the interior that is simply the truth: in MVT, land is the absence of a
/// water polygon. Near the border it used to overestimate badly — the raster
/// stopped at the tile edge, so a bank two metres into the neighbouring tile
/// was invisible, the border strip saturated to "open water", and the shallow
/// tint (the most visible thing the field drives) stepped in a straight line
/// down every z14 cell edge. The tile's geometry already runs past the edge
/// into the buffer zone (that is what `clip_ring` exists to cut off), so
/// rasterising a [`SHORE_FIELD_PAD`]-texel apron around the field and cropping
/// it away after the transform makes the border strip truthful out to the
/// buffer's reach (~23 m at z14 — past `SHALLOW_M` and the foam band) for one
/// extra fill and a 6 % larger transform. Note the apron pads with the
/// *neighbour's actual geometry*, not with land: padding with land (the
/// obvious trick) would draw a shoreline straight down the middle of every
/// river that crosses a tile edge.
pub fn water_shore_field(tile: &TileData, tile_m: f32) -> Result<Option<ShoreField>, String> {
    let size = SHORE_FIELD_PX;
    let pad = SHORE_FIELD_PAD;
    let psize = size + 2 * pad;
    let mut pixmap = Pixmap::new(psize, psize).ok_or_else(|| "pixmap alloc failed".to_string())?;
    let paint = Paint {
        anti_alias: false,
        shader: Shader::SolidColor(tiny_skia::Color::WHITE),
        ..Paint::default()
    };

    let mut any_water = false;
    for meta in tile.layers() {
        if !WATER_LAYERS.contains(&meta.name.as_str()) {
            continue;
        }
        let extent = TileData::extent(meta);
        let s = size as f32 / extent;
        let transform = Transform::from_row(s, 0.0, 0.0, s, pad as f32, pad as f32);
        for feature in tile.features(meta) {
            // The fill itself answers "was there any water here", so the ring is
            // walked into a `PathBuilder` once instead of twice. This runs at
            // `SHORE_FIELD_PX + 2·PAD` (1056²) over every ocean/water polygon, and
            // on a low-zoom ocean tile the water polygon *is* the feature set.
            any_water |= fill_geometry(&mut pixmap, &paint, &feature.geometry, &transform);
        }
    }

    // **The narrow waterways too, stroked at the width they are drawn at.**
    //
    // `tessellate_surfaces` draws a river, canal or stream that Shortbread
    // carries as a *line* rather than an area, and this field is what every water
    // surface asks "how far am I from the bank?". A ribbon the field does not
    // know about samples as land along its whole length — zero distance from
    // shore, which is shallow water and foam everywhere. That is the mismatch
    // `WATER_LAYERS`' own doc warns of, in its other direction: not a shoreline in
    // open water, but open water that thinks it is all shoreline.
    if let Some(meta) = tile.layer("water_lines") {
        let extent = TileData::extent(meta);
        let s = size as f32 / extent;
        let transform = Transform::from_row(s, 0.0, 0.0, s, pad as f32, pad as f32);
        for feature in tile.features(meta) {
            let Some(half_m) = water_line_half_width_m(kind_of(feature).unwrap_or("")) else {
                continue;
            };
            // Field texels per metre: the stroke has to be the ribbon's width in
            // the *field's* units, not the tile's.
            let width_px = 2.0 * half_m * (extent / tile_m.max(1.0)) * s;
            let stroke = Stroke {
                width: width_px.max(1.0),
                line_cap: LineCap::Butt,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
            match &feature.geometry {
                Geometry::LineString(ls) => lines.push(ls),
                Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
                _ => continue,
            }
            for ls in lines {
                let mut pb = PathBuilder::new();
                let mut started = false;
                for c in &ls.0 {
                    if !c.x.is_finite() || !c.y.is_finite() {
                        continue;
                    }
                    if started {
                        pb.line_to(c.x, c.y);
                    } else {
                        pb.move_to(c.x, c.y);
                        started = true;
                    }
                }
                if let Some(path) = pb.finish() {
                    pixmap.stroke_path(&path, &paint, &stroke, transform, None);
                    any_water = true;
                }
            }
        }
    }
    if !any_water {
        return Ok(None);
    }
    // Seed the transform from every *land* texel — the unpainted ones.
    let data = distance_field(&pixmap, size, pad, tile_m, Seed::Unpainted);

    // And a second, coarser field of what *stands* on the bank, for the
    // reflection term. Only buildings seed it; see `ShoreField::bank`.
    let bank_size = BANK_FIELD_PX;
    let bank_pad = BANK_FIELD_PAD;
    let bpsize = bank_size + 2 * bank_pad;
    let mut banks = Pixmap::new(bpsize, bpsize).ok_or_else(|| "pixmap alloc failed".to_string())?;
    for meta in tile.layers() {
        if meta.name != "buildings" {
            continue;
        }
        let s = bank_size as f32 / TileData::extent(meta);
        let transform = Transform::from_row(s, 0.0, 0.0, s, bank_pad as f32, bank_pad as f32);
        for feature in tile.features(meta) {
            fill_geometry(&mut banks, &paint, &feature.geometry, &transform);
        }
    }
    let bank = distance_field(&banks, bank_size, bank_pad, tile_m, Seed::Painted);

    Ok(Some(ShoreField {
        size,
        data,
        bank_size,
        bank,
    }))
}

/// Which texels of a rasterised mask the distance is measured *from*.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seed {
    /// The painted ones — "how far to the nearest building".
    Painted,
    /// The unpainted ones — "how far to the nearest bit of land", where the
    /// paint is the water.
    Unpainted,
}

/// Distance transform one padded mask into a cropped, quantised field.
///
/// `pixmap` is `size + 2·pad` square; the result is `size` square, in metres
/// scaled so 255 is [`SHORE_MAX_DIST_M`]. The apron is cropped away *after* the
/// transform, which is the whole reason it is rasterised: a seed just over the
/// tile line is as real as one just inside it, and without it every field
/// saturates in a straight line down the cell edge.
fn distance_field(pixmap: &Pixmap, size: u32, pad: u32, tile_m: f32, seed: Seed) -> Vec<u8> {
    let psize = size + 2 * pad;
    // "Unreached" is a large *finite* distance, not infinity: the envelope
    // subtracts one sample's value from another's, and ∞ − ∞ is a NaN that
    // quietly poisons a whole scanline. Two texel-grids is past any real answer
    // (nothing inside the field can be further than `psize` from a seed) while
    // staying exactly representable in f32 — and it is also what a mask with no
    // seeds at all leaves behind, which quantises to 255 everywhere, i.e. "there
    // is none of this anywhere near", which is the right answer.
    let unreached = (2 * psize) as f32;
    let mut sq = vec![0.0f32; (psize * psize) as usize];
    for (d, px) in sq.iter_mut().zip(pixmap.pixels()) {
        let is_seed = (px.alpha() > 127) == (seed == Seed::Painted);
        *d = if is_seed { 0.0 } else { unreached };
    }
    squared_distance_transform(&mut sq, psize as usize);

    let m_per_texel = tile_m / size as f32;
    let mut out = Vec::with_capacity((size * size) as usize);
    for y in pad..pad + size {
        let row = &sq[(y * psize + pad) as usize..(y * psize + pad + size) as usize];
        out.extend(row.iter().map(|d2| {
            let m = d2.sqrt() * m_per_texel;
            (m / SHORE_MAX_DIST_M).clamp(0.0, 1.0).mul_add(255.0, 0.5) as u8
        }));
    }
    out
}

/// Exact squared Euclidean distance transform, in place over a `size × size`
/// grid holding `0` at each seed and a saturation distance of at least `size`
/// everywhere else. On return each cell holds the squared distance to the
/// nearest seed.
///
/// Exact and O(n) — the same cost as the chamfer approximation people reach for
/// first, without its error, which here would show up as a shoreline band whose
/// width visibly depends on the bank's angle.
///
/// Felzenszwalb & Huttenlocher do this as two passes of a 1-D parabola lower
/// envelope, one per axis. Only the second pass needs one. The first sees
/// nothing but the binary seed mask, and the 1-D squared distance transform of a
/// binary function is just "distance to the nearest seed, squared" — two min
/// sweeps, no parabolas, and no division (the envelope divides once per sample,
/// which at a megapixel is where its time actually goes).
///
/// Running that first axis *down the rows* rather than along them is what makes
/// it worth doing: the recurrence is along y, so for a fixed y every x is
/// independent and contiguous, and the sweep vectorises. The original walked
/// columns with a `size`-element stride, gathering one f32 at a time.
///
/// Together: ~3x off the whole transform, for bit-identical output. That matters
/// because this is the single most expensive thing a water tile does.
fn squared_distance_transform(grid: &mut [f32], size: usize) {
    if size < 2 {
        return;
    }

    // Pass 1, along y. Forward then backward min-sweep leaves each cell holding
    // its distance to the nearest seed in its own column; squaring it gives the
    // 1-D transform the envelope would have produced.
    for y in 1..size {
        let (head, tail) = grid.split_at_mut(y * size);
        let above = &head[(y - 1) * size..];
        for (d, a) in tail[..size].iter_mut().zip(above) {
            *d = d.min(a + 1.0);
        }
    }
    for y in (0..size - 1).rev() {
        let (head, tail) = grid.split_at_mut((y + 1) * size);
        for (d, b) in head[y * size..].iter_mut().zip(&tail[..size]) {
            *d = d.min(b + 1.0);
        }
    }
    for d in grid.iter_mut() {
        *d *= *d;
    }

    // Pass 2, along x. Its input is an arbitrary function now, not a mask, so
    // this one really is the parabola envelope — over contiguous rows.
    //
    // **Every row is independent here, and pass 1 was not.** The y-sweeps above
    // carry a running minimum from one row to the next, so they have to be
    // walked in order; the envelope reads one row and writes that same row, with
    // scratch that belongs to the row. So this pass — and only this pass — is
    // split across threads. The shore field is 1,056 rows of 1,056 samples and
    // the envelope is the expensive half of the transform.
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Below this the whole transform is well under a millisecond and the
        // threads cost more than the sweep.
        const PARALLEL_MIN_SIZE: usize = 256;
        let threads = crate::road_stitch::available_threads().min(4);
        if threads > 1 && size >= PARALLEL_MIN_SIZE {
            let rows = size.div_ceil(threads);
            std::thread::scope(|s| {
                for band in grid.chunks_mut(rows * size) {
                    s.spawn(move || {
                        // Per thread: the envelope's vertices and breakpoints are
                        // rebuilt for each row anyway, so sharing them would buy
                        // nothing and cost a lock.
                        let mut f = vec![0.0f32; size];
                        let mut d = vec![0.0f32; size];
                        let mut v = vec![0usize; size];
                        let mut z = vec![0.0f32; size + 1];
                        for row in band.chunks_mut(size) {
                            f.copy_from_slice(row);
                            lower_envelope(&f, &mut d, &mut v, &mut z);
                            row.copy_from_slice(&d);
                        }
                    });
                }
            });
            return;
        }
    }

    let mut f = vec![0.0f32; size];
    let mut d = vec![0.0f32; size];
    // Parabola vertices and the breakpoints between them; both need one extra slot.
    let mut v = vec![0usize; size];
    let mut z = vec![0.0f32; size + 1];
    for y in 0..size {
        f.copy_from_slice(&grid[y * size..(y + 1) * size]);
        lower_envelope(&f, &mut d, &mut v, &mut z);
        grid[y * size..(y + 1) * size].copy_from_slice(&d);
    }
}

/// One-dimensional squared distance transform: the lower envelope of the
/// parabolas rooted at each sample.
fn lower_envelope(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    let mut k: usize = 0;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32))
                / (2.0 * q as f32 - 2.0 * p as f32);
            if s <= z[k] && k > 0 {
                k -= 1;
            } else {
                k += 1;
                v[k] = q;
                z[k] = s;
                z[k + 1] = f32::INFINITY;
                break;
            }
        }
    }
    k = 0;
    // `q` walks the samples while `k` walks the envelope's segments — the two
    // advance independently, so this is not an iteration over `d`.
    #[allow(clippy::needless_range_loop)]
    for q in 0..n {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        let dq = q as f32 - p as f32;
        d[q] = dq * dq + f[p];
    }
}

/// True if these bytes look like an image the `image` crate should decode, rather
/// than an MVT body. Sniffing beats configuration here: the tile URL template is a
/// plain `{z}/{x}/{y}` string that carries no format, and mixing raster and vector
/// sources (VersaTiles' satellite layer *is* raster) then needs no extra plumbing.
#[must_use]
pub fn looks_like_raster(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || (bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
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
        let roads = super::tessellate_roads(&tile, tile_m, &mut decks, None, false).expect("roads");
        assert!(
            roads.on_deck.iter().any(|(d, _)| *d as usize == di),
            "no carriageway rides the flyover"
        );
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
            feature: 0,
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

    /// The pre-filter `systems::nav_marks` skips a network request on, checked
    /// against the two answers that matter: a real canal city says yes, and a
    /// body with no water layer in it says no.
    ///
    /// The direction that must never be wrong is the second one — a false
    /// negative would silently un-draw a fairway's marks, while a false positive
    /// only costs a request.
    #[test]
    fn the_water_probe_reads_an_undecoded_body() {
        assert!(
            body_mentions_water(GRONINGEN_Z14),
            "a canal city's tile carries water layers"
        );
        let dry = synth::tile(&[synth::line_layer(
            "streets",
            4096,
            &[(100, 100), (900, 900)],
        )]);
        assert!(
            TileData::parse(&dry)
                .expect("dry tile parses")
                .layer("streets")
                .is_some(),
            "the synthetic tile is a tile"
        );
        assert!(
            !body_mentions_water(&dry),
            "a body with no water layer must not be probed as wet"
        );
    }

    /// The rail pre-filter `systems::crossings` skips a network request on.
    ///
    /// The direction that must never be wrong is the negative one, and it is
    /// harder to be sure of here than for water: rail is a `kind` **value**
    /// rather than a layer name, so this builds a tile whose *only* mention of a
    /// railway is that value — no layer called anything rail-ish, no other tag —
    /// and checks the probe still finds it. A body with an ordinary street in it
    /// and nothing else must answer no, or the layer would fetch the register
    /// over the whole planet.
    #[test]
    fn the_rail_probe_reads_a_kind_value_in_an_undecoded_body() {
        let railway = synth::tile(&[synth::tagged_line_layer(
            "streets",
            4096,
            ("kind", "rail"),
            &[(100, 100), (900, 900)],
        )]);
        assert!(
            TileData::parse(&railway)
                .expect("the railway tile parses")
                .layer("streets")
                .is_some(),
            "the synthetic tile is a tile"
        );
        assert!(
            body_mentions_rail(&railway),
            "a kind = rail street must never be probed as railless"
        );
        // …and the same for a tram, which is the one rail kind whose name does
        // not contain `rail` and so needs a needle of its own.
        let tramway = synth::tile(&[synth::tagged_line_layer(
            "streets",
            4096,
            ("kind", "tram"),
            &[(100, 100), (900, 900)],
        )]);
        assert!(body_mentions_rail(&tramway), "a tram is track too");

        let roads = synth::tile(&[synth::tagged_line_layer(
            "streets",
            4096,
            ("kind", "residential"),
            &[(100, 100), (900, 900)],
        )]);
        assert!(
            !body_mentions_rail(&roads),
            "a body with no railway in it must not be probed as railed"
        );
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

    /// The ocean mask rides in the imagery's alpha, so it has to come out at the
    /// size the imagery did. A mismatch is not loud: `GlobeTilePayload::decode`
    /// drops a wrong-length mask and the tile silently loses its ocean.
    #[test]
    fn the_ocean_mask_follows_the_raster_size() {
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse");
        for size in [TILE_PX, TILE_PX / 2, 128] {
            let mask = ocean_mask_sized(&tile, SubTile::WHOLE, size).expect("mask");
            assert_eq!(mask.len() as u32, size * size, "mask size {size} is wrong");
        }
    }

    /// A tunnel's name must not be written on the ground over it.
    ///
    /// Same rule the carriageway already follows (`drawn_street`) and the same
    /// consequence for missing it: the label lands across whatever genuinely *is* up
    /// there — the junction the tunnel passes under, the park it runs beneath.
    ///
    /// Asserted on geometry rather than on names, because Shortbread merges by
    /// attribute and a street with a tunnel section keeps a *separate* surface
    /// feature under the same name — the fixture has nine tunnel-flagged label
    /// features and not one name that is tunnel-only. So "Kastanjelaan is not
    /// labelled" is the wrong claim; "nothing is labelled along the buried stretch
    /// of it" is the right one.
    ///
    /// There is no matching rule for bridges, and that is not an oversight:
    /// `street_labels` carries no `bridge` attribute (checked 2026-08-20 on this
    /// fixture), so a name on a viaduct cannot be told from one on the road beneath.
    /// `docs/notes/grade-separation.md` has it.
    #[test]
    fn a_tunnel_gets_no_street_label_along_its_buried_stretch() {
        const TILE_M: f32 = 1500.0;
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse");
        let meta = tile.layer("street_labels").expect("street_labels layer");
        let scale = TILE_M / TileData::extent(meta);

        // A point in the middle of each tunnel run, in the metres frame the output
        // uses.
        let mut probes: Vec<(String, [f32; 2])> = Vec::new();
        for f in tile.features(meta) {
            if !flag_of(f, "tunnel") {
                continue;
            }
            let Some(name) = str_prop(f, "name").filter(|n| !n.is_empty()) else {
                continue;
            };
            let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
            match &f.geometry {
                Geometry::LineString(ls) => lines.push(ls),
                Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
                _ => continue,
            }
            for ls in lines {
                // The midpoint of the middle *segment*, never a vertex. A vertex
                // would routinely be the tunnel's entrance, which the surface run of
                // the same street shares — and a probe sitting on a shared node
                // reports a hit of exactly 0.0 m against a label that is correctly
                // drawn.
                if ls.0.len() < 2 {
                    continue;
                }
                let m = (ls.0.len() - 1) / 2;
                let (a, b) = (ls.0[m], ls.0[m + 1]);
                probes.push((
                    name.to_string(),
                    [(a.x + b.x) * 0.5 * scale, (a.y + b.y) * 0.5 * scale],
                ));
            }
        }
        assert!(
            !probes.is_empty(),
            "the fixture carries no named tunnel label — this test proves nothing, \
             and a source that stopped shipping `tunnel` on `street_labels` would \
             look exactly like this"
        );

        // Half the simplification tolerance: a surviving label vertex lands within a
        // few metres of the centre line it came from, and nothing else on the map is
        // that close to a tunnel's midpoint while carrying the tunnel's own name.
        let near_m = LABEL_SIMPLIFY_M * 0.5;
        let drawn = super::extract_street_labels(&tile, TILE_M).expect("extract");
        for (name, probe) in &probes {
            for line in drawn.iter().filter(|l| &l.name == name) {
                for p in &line.points {
                    let d = ((p[0] - probe[0]).powi(2) + (p[1] - probe[1]).powi(2)).sqrt();
                    assert!(
                        d > near_m,
                        "{name} is labelled {d:.1} m from the middle of its own \
                         tunnel run — the buried stretch was drawn on the surface"
                    );
                }
            }
        }
    }

    #[test]
    fn street_labels_come_out_named_and_in_metres() {
        let tile_m = 1500.0;
        let labels = extract_street_labels(GRONINGEN_Z14, tile_m).expect("extract");
        assert!(
            labels.len() > 50,
            "a city tile should carry plenty of named streets, got {}",
            labels.len()
        );
        assert!(labels.iter().all(|l| !l.name.trim().is_empty()));
        // Tile-local metres, clipped to the tile: a line escaping these bounds
        // would be drawn into the neighbouring cell's streets.
        for l in &labels {
            assert!(l.points.len() >= 2, "{}: degenerate line", l.name);
            for p in &l.points {
                assert!(
                    (-0.5..=tile_m + 0.5).contains(&p[0]) && (-0.5..=tile_m + 0.5).contains(&p[1]),
                    "{} escapes the tile at {p:?}",
                    l.name
                );
            }
            assert!(polyline_len(&l.points) >= LABEL_MIN_LEN_M);
        }
        // The rank is what decides who survives a crowded frame, so the tile has
        // to actually carry a spread of classes rather than one default.
        let ranks: std::collections::HashSet<u8> = labels.iter().map(|l| l.rank).collect();
        assert!(
            ranks.len() > 2,
            "every street came out the same class: {ranks:?}"
        );
    }

    #[test]
    fn simplify_keeps_the_shape_and_both_ends() {
        // A line with a burst of near-coincident vertices in the middle: the
        // dense stretch collapses, the end points and the length do not.
        let mut points = vec![[0.0f32, 0.0]];
        for i in 1..20 {
            points.push([i as f32 * 0.5, 0.0]);
        }
        points.push([100.0, 0.0]);
        let simplified = simplify_polyline(&points, LABEL_SIMPLIFY_M);
        assert!(simplified.len() < points.len() / 2);
        assert_eq!(simplified.first(), Some(&[0.0, 0.0]));
        assert_eq!(simplified.last(), Some(&[100.0, 0.0]));
        assert!((polyline_len(&simplified) - 100.0).abs() < 1e-3);
    }

    #[test]
    fn shore_field_measures_distance_into_the_water() {
        let tile_m = 1500.0;
        let field = water_shore_field(GRONINGEN_Z14, tile_m)
            .expect("shore field")
            .expect("fixture has water");
        assert_eq!(field.data.len(), (field.size * field.size) as usize);

        // A canal city: some texels are dry land (0) and some sit far enough from
        // any bank to read as open water. A field that is all one value means the
        // rasterisation or the transform silently did nothing.
        assert!(field.data.iter().any(|&d| d == 0), "no land texels");
        assert!(
            field.data.iter().any(|&d| d > 8),
            "no texel more than a few metres from shore"
        );
    }

    /// The bank field answers a different question from the shore field, and the
    /// way it fails is by quietly answering the same one.
    ///
    /// Seed it from the water polygons by mistake — or from nothing, and let the
    /// unreached value leak through as a small number — and the reflection term
    /// darkens either every shore or none of them, which is precisely the bug it
    /// exists to end. So: buildings and land must disagree somewhere in a canal
    /// city, and a stretch of open water must be able to read "nothing standing
    /// anywhere near".
    #[test]
    fn the_bank_field_measures_buildings_and_not_the_shore() {
        let tile_m = 1500.0;
        let field = water_shore_field(GRONINGEN_Z14, tile_m)
            .expect("shore field")
            .expect("fixture has water");
        assert_eq!(
            field.bank.len(),
            (field.bank_size * field.bank_size) as usize
        );
        assert!(field.bank.iter().any(|&d| d == 0), "no building texels");
        assert!(
            field.bank.iter().any(|&d| d == u8::MAX),
            "every texel is within {SHORE_MAX_DIST_M} m of a building"
        );

        // And it must not be the shore field wearing a smaller size. Seed the
        // transform from the water polygons by mistake and every assert above
        // still holds — the giveaway is that the zeros would then coincide, so
        // look for **land that is not a building**: a street, a garden, a quay.
        // The first version of this test asked for open water instead, meaning a
        // texel 60 m from any bank, and a canal city does not have one.
        let step = field.size / field.bank_size;
        let mut land_without_a_building = 0usize;
        for by in 0..field.bank_size {
            for bx in 0..field.bank_size {
                let shore = field.data[((by * step) * field.size + bx * step) as usize];
                let bank = field.bank[(by * field.bank_size + bx) as usize];
                if shore == 0 && bank > 0 {
                    land_without_a_building += 1;
                }
            }
        }
        assert!(
            land_without_a_building > 0,
            "every land texel is also a building texel, so the bank field is \
             carrying the shore field's answer"
        );
    }

    #[test]
    fn shore_field_is_none_without_water() {
        // An empty MVT body parses to zero layers, so there is nothing to measure
        // against — the caller must get `None` rather than an all-shore field,
        // which would wrap the whole tile in foam.
        assert!(matches!(water_shore_field(&[], 1500.0), Ok(None)));
    }

    #[test]
    fn distance_transform_is_exact_for_a_single_seed() {
        // 1D-separable transforms are easy to get subtly wrong (off-by-one in the
        // breakpoints shows up as a distance field that leans one way), so check
        // against the closed form rather than against a golden image.
        let size = 16;
        let far = (4 * size * size) as f32;
        let mut grid = vec![far; size * size];
        grid[5 * size + 6] = 0.0;
        squared_distance_transform(&mut grid, size);
        for y in 0..size {
            for x in 0..size {
                let (dx, dy) = ((x as f32) - 6.0, (y as f32) - 5.0);
                assert!(
                    (grid[y * size + x] - (dx * dx + dy * dy)).abs() < 1e-3,
                    "wrong distance at ({x}, {y})"
                );
            }
        }
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
            let whole = render_mvt_banded(&tile, sub, TILE_PX, NOMINAL_Z14_TILE_M, false, 1, &[])
                .expect("whole render");
            // 7 is in the list on purpose: it does not divide TILE_PX, so the
            // last band is short and the row arithmetic has to survive it.
            for bands in [2u32, 3, 4, 7] {
                let split =
                    render_mvt_banded(&tile, sub, TILE_PX, NOMINAL_Z14_TILE_M, false, bands, &[])
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
                assert!(
                    max_delta <= 12,
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
            let differing = culled
                .iter()
                .zip(full.iter())
                .filter(|(a, b)| a.abs_diff(**b) > 1)
                .count();
            assert_eq!(
                differing, 0,
                "factor {factor} ({x},{y}): culling changed {differing} bytes by more than rounding"
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

    /// Even–odd point-in-polygon over a set of closed rings.
    /// The distance transform is the shore field's whole cost, so it was
    /// rewritten (binary min-sweep down the columns, envelope only along the
    /// rows). It is supposed to be *exact*, so check it against the definition
    /// itself — the O(n^4) brute force — rather than against the old code.
    #[test]
    fn distance_transform_is_exact() {
        for (size, seed_every) in [(16usize, 7usize), (32, 23), (17, 5), (2, 3)] {
            let unreached = (2 * size) as f32;
            let is_seed = |i: usize| i % seed_every == 0;
            let mut grid: Vec<f32> = (0..size * size)
                .map(|i| if is_seed(i) { 0.0 } else { unreached })
                .collect();
            squared_distance_transform(&mut grid, size);

            for y in 0..size {
                for x in 0..size {
                    let want = (0..size)
                        .flat_map(|sy| (0..size).map(move |sx| (sx, sy)))
                        .filter(|(sx, sy)| is_seed(sy * size + sx))
                        .map(|(sx, sy)| {
                            let (dx, dy) = (x as f32 - sx as f32, y as f32 - sy as f32);
                            dx * dx + dy * dy
                        })
                        .fold(f32::INFINITY, f32::min);
                    assert_eq!(
                        grid[y * size + x],
                        want,
                        "size {size}, seed every {seed_every}, at ({x}, {y})"
                    );
                }
            }
        }
    }

    /// A tile that is water all the way across has no land to measure against.
    /// The saturation value has to survive the transform intact, because the
    /// caller turns it into "open water, far from any bank" — an infinity here
    /// would come back as a NaN and take a whole scanline with it.
    #[test]
    fn a_seedless_grid_saturates_rather_than_going_infinite() {
        let size = 8;
        let unreached = (2 * size) as f32;
        let mut grid = vec![unreached; size * size];
        squared_distance_transform(&mut grid, size);
        assert!(grid.iter().all(|d| *d == unreached * unreached), "{grid:?}");
    }

    fn point_in_rings(px: f32, pz: f32, rings: &[Vec<[f32; 2]>]) -> bool {
        let mut inside = false;
        for r in rings {
            for w in r.windows(2) {
                let (a, b) = (w[0], w[1]);
                if (a[1] > pz) != (b[1] > pz) {
                    let t = (pz - a[1]) / (b[1] - a[1]);
                    if px < a[0] + t * (b[0] - a[0]) {
                        inside = !inside;
                    }
                }
            }
        }
        inside
    }

    #[test]
    fn a_ring_that_leaves_the_tile_and_returns_stays_inside_its_own_outline() {
        // A canal running out through the west edge and back — a thin U whose two
        // banks both lie outside the tile. Clamping folds those banks onto x = 0,
        // the ring self-intersects, and earcut answers with bow-ties: finite-area
        // triangles *outside* the polygon. On screen those are the dark water
        // wedges lying across a lawn or a carriageway well away from any water.
        let ring = geo_types::LineString::from(vec![
            (-200.0f32, 1000.0),
            (600.0, 1000.0),
            (600.0, 1100.0),
            (-200.0, 1100.0),
            (-200.0, 2000.0),
            (600.0, 2000.0),
            (600.0, 2100.0),
            (-200.0, 2100.0),
        ]);
        let poly = geo_types::Polygon::new(ring, vec![]);
        let (scale, extent) = (1000.0 / 4096.0, 4096.0);

        let mut positions = Vec::new();
        let mut indices = Vec::new();
        tessellate_polygon(&poly, scale, extent, &mut positions, &mut indices);
        assert!(!indices.is_empty(), "the clipped polygon still has area");

        let rings = clipped_rings(&poly, scale, extent);
        for t in indices.chunks_exact(3) {
            let (a, b, c) = (
                positions[t[0] as usize],
                positions[t[1] as usize],
                positions[t[2] as usize],
            );
            let area =
                (((b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0])) * 0.5).abs();
            if area <= 1.0 {
                continue; // a zero-width connector along the seam — invisible
            }
            let (cx, cz) = ((a[0] + b[0] + c[0]) / 3.0, (a[2] + b[2] + c[2]) / 3.0);
            assert!(
                point_in_rings(cx, cz, &rings),
                "triangle of {area:.0} m² at ({cx:.0}, {cz:.0}) lies outside the polygon"
            );
        }
    }

    #[test]
    fn clipping_cuts_the_overhang_rather_than_folding_it() {
        // The clip keeps what is inside, inserts the two border crossings, and
        // hands back an open, duplicate-free ring.
        let out = clip_ring(
            &[[-10.0, 10.0], [50.0, 10.0], [50.0, 50.0], [-10.0, 50.0]],
            100.0,
        );
        assert_eq!(out.len(), 4, "{out:?}");
        assert!(out.iter().all(|p| p[0] >= 0.0 && p[0] <= 100.0));
        assert_eq!(out.iter().filter(|p| p[0] == 0.0).count(), 2);
        // Entirely outside the tile: nothing survives.
        assert!(clip_ring(&[[-50.0, -50.0], [-10.0, -50.0], [-10.0, -10.0]], 100.0).is_empty());
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

    /// The cut order has to stay a **total** order, and the band is the term
    /// that is easiest to get wrong: an asymmetric rule — the kerb beats the
    /// carriageway that beats the kerb — cuts both out of the ground they share
    /// and opens a hole in the road.
    #[test]
    fn the_cut_bands_order_a_kerb_under_every_carriageway() {
        use RoadClass::{Carriageway, Major, Minor, Motorway, Path, Rail};
        for kerbed in [Motorway, Carriageway, Major, Minor] {
            for other in [Motorway, Carriageway, Major, Minor] {
                assert!(
                    cut_band(SurfaceGroup::Road(kerbed))
                        > cut_band(SurfaceGroup::RoadCasing(other)),
                    "{kerbed:?}'s carriageway must break {other:?}'s kerb",
                );
            }
            // Rails are laid *over* a road, not cut by it.
            assert!(cut_band(SurfaceGroup::Road(Rail)) > cut_band(SurfaceGroup::Road(kerbed)));
            // A footway is not a carriageway and does not break a kerb; inside
            // one band `road_rung` settles it exactly as it did before.
            assert_eq!(
                cut_band(SurfaceGroup::Road(Path)),
                cut_band(SurfaceGroup::RoadCasing(kerbed)),
            );
            assert!(
                road_rung(kerbed, 6.0, false, 0) > road_casing_rung(kerbed, false),
                "a road's own kerb still loses to its own tarmac",
            );
        }
    }

    /// **A side street's opening has to be open.** A kerb is the edge between a
    /// carriageway and what is not one, so where another carriageway arrives
    /// there is no kerb.
    ///
    /// Until `cut_band` the cut settled that on `road_rung` alone, which puts a
    /// whole class above a whole class, kerb band included — so a residential
    /// street tee-ing into a primary road lost to that road's *kerb*: its tarmac
    /// stopped at the far side of the band and the kerb line ran on unbroken
    /// across the opening. Measured on the fixture over the junctions where two
    /// kerbed classes meet at a clear angle, the point on the side street's
    /// centre line halfway through the main road's kerb band was drawn as:
    ///
    /// | | before | after |
    /// |---|---|---|
    /// | the side street's carriageway | 0 | 129 |
    /// | the main road's kerb | 129 | 0 |
    ///
    /// A picture cannot settle this — a kerb crossing a junction opening is a
    /// hairline at any altitude a whole junction fits in — which is why it is a
    /// geometric assertion and not a screenshot.
    #[test]
    fn a_side_street_reaches_the_road_it_joins() {
        let tile_m = 1000.0f32;
        let parsed = TileData::parse(GRONINGEN_Z14).expect("parse");
        let meta = parsed.layer("streets").expect("a streets layer");
        let features = parsed.features(meta);
        let scale = tile_m / TileData::extent(meta);
        let nodes = street_nodes(&features, scale);
        let meshes = tessellate_roads(GRONINGEN_Z14, tile_m, &[]).expect("tessellate");

        let covers = |group: &dyn Fn(SurfaceGroup) -> bool, p: [f32; 2]| -> bool {
            meshes.iter().filter(|m| group(m.group)).any(|m| {
                m.indices.chunks_exact(3).any(|t| {
                    let q: Vec<[f32; 2]> = t
                        .iter()
                        .map(|i| [m.positions[*i as usize][0], m.positions[*i as usize][2]])
                        .collect();
                    let s = |a: &[f32; 2], b: &[f32; 2]| {
                        (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
                    };
                    let (d0, d1, d2) = (s(&q[0], &q[1]), s(&q[1], &q[2]), s(&q[2], &q[0]));
                    (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0) || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
                })
            })
        };
        let kerbed = |c: RoadClass| {
            matches!(
                c,
                RoadClass::Motorway | RoadClass::Carriageway | RoadClass::Major | RoadClass::Minor
            )
        };

        let (mut open, mut walled, mut bare) = (0usize, 0usize, 0usize);
        let (mut ranked, mut angled) = (0usize, 0usize);
        for (key, arms) in &nodes {
            for side in arms {
                // A street that *ends* here…
                let Some((leaves, _)) = side.leaves else {
                    continue;
                };
                if !kerbed(side.class) {
                    continue;
                }
                for main in arms {
                    // …onto a road of a higher class, which is exactly the pair
                    // the old ordering got wrong: the kerb band goes in at the
                    // foot of its own class's block, so a whole class's kerb sat
                    // above a whole lower class's tarmac.
                    if main.feature == side.feature
                        || main.leaves.is_some()
                        || !kerbed(main.class)
                        || road_class_rank(main.class) <= road_class_rank(side.class)
                    {
                        continue;
                    }
                    ranked += 1;
                    // Shallow crossings are excluded: they stretch the band the
                    // sample has to land in along the side street until the
                    // street's own first segment is too short to hold it, and
                    // below `MERGE_SIN` the two are a continuation rather than a
                    // junction at all.
                    let sin = (leaves[0] * main.axis[1] - leaves[1] * main.axis[0]).abs();
                    if sin < 0.45 {
                        continue;
                    }
                    // The sample sits on the side street's own centre line, in
                    // the main road's kerb band **on the approach side** — the
                    // node is on the main road's centre line, so the band the
                    // side street has to break through is the one between it and
                    // the carriageway. It is `1 / sin` further out along the side
                    // street than it is across the main road, which is what makes
                    // the crossing angle matter here at all. Placed 40 % into the
                    // 0.75 m band, which leaves room for the node key's rounding
                    // to whole tile units (±0.12 m at z14).
                    let d = (main.half + ROAD_CASING_M * 0.4) / sin;
                    // …and only where the side street actually runs that far in a
                    // straight line, or the sample is off its centre line.
                    if side.leaves.is_some_and(|(_, seg)| seg < d) {
                        continue;
                    }
                    angled += 1;
                    let p = [
                        key.0 as f32 * scale + leaves[0] * d,
                        key.1 as f32 * scale + leaves[1] * d,
                    ];
                    if covers(&|g| matches!(g, SurfaceGroup::Road(_)), p) {
                        open += 1;
                    } else if covers(&|g| matches!(g, SurfaceGroup::RoadCasing(_)), p) {
                        walled += 1;
                    } else {
                        bare += 1;
                    }
                }
            }
        }
        println!(
            "cross-class junction ends {ranked}, of those at a usable angle {angled}; \
             openings: {open} open, {walled} walled off, {bare} bare"
        );
        // The population is small because most of a city centre's streets are
        // one Shortbread class (`Minor` covers tertiary down to service), and it
        // is only *across* classes that a kerb ever outranked a carriageway.
        assert!(open + walled + bare >= 12, "too few junctions to conclude");
        assert_eq!(
            walled,
            0,
            "{walled} of {} openings are sealed by the kerb of the road they join",
            open + walled + bare,
        );
    }

    /// **A drawn street is a curve, not a chain of boards.**
    ///
    /// Shortbread generalises its geometry for a map read from above, where a
    /// 20° kink in a street is a pixel. Extruded into a ribbon and seen from the
    /// pavement, that kink is a corner in the kerb and in the painted edge line,
    /// and the street reads as a row of straight planks. Measured on the fixture
    /// over every drawn centre line's interior vertices:
    ///
    /// | turn | raw | rounded |
    /// |---|---|---|
    /// | under 10° | 648 (38 %) | 4616 (94 %) |
    /// | 10–20° | 378 | 2 |
    /// | 20–70° | 447 | 70 |
    /// | over 70° — a corner the map asserts | 216 | 216 |
    ///
    /// The last row is the one that must **not** move: a street grid's right
    /// angles are real, and `round_corners` keeps them. What is left in 20–70° is
    /// the corners the fold rule and the one-metre allowance refused, which is
    /// the guard working rather than a miss.
    #[test]
    fn a_drawn_street_is_a_curve_not_a_chain_of_boards() {
        let tile_m = 1000.0f32;
        let parsed = TileData::parse(GRONINGEN_Z14).expect("parse");
        let meta = parsed.layer("streets").expect("streets");
        let features = parsed.features(meta);
        let scale = tile_m / TileData::extent(meta);

        let tally = |rounded: bool| -> [usize; 4] {
            let mut bins = [0usize; 4];
            for f in features.iter() {
                let Some((_, half)) = drawn_street(f) else {
                    continue;
                };
                let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
                match &f.geometry {
                    geo_types::Geometry::LineString(ls) => lines.push(ls),
                    geo_types::Geometry::MultiLineString(ml) => lines.extend(ml.iter()),
                    _ => continue,
                }
                for ls in lines {
                    let raw: Vec<[f32; 2]> =
                        ls.0.iter().map(|c| [c.x * scale, c.y * scale]).collect();
                    let p = if rounded {
                        round_corners(&raw, half)
                    } else {
                        raw
                    };
                    for i in 1..p.len().saturating_sub(1) {
                        let a = [p[i][0] - p[i - 1][0], p[i][1] - p[i - 1][1]];
                        let b = [p[i + 1][0] - p[i][0], p[i + 1][1] - p[i][1]];
                        let (la, lb) = (a[0].hypot(a[1]), b[0].hypot(b[1]));
                        if la < 1e-4 || lb < 1e-4 {
                            continue;
                        }
                        let cos = ((a[0] * b[0] + a[1] * b[1]) / (la * lb)).clamp(-1.0, 1.0);
                        let deg = cos.acos().to_degrees();
                        bins[match deg {
                            d if d < 10.0 => 0,
                            d if d < 20.0 => 1,
                            d if d < 70.0 => 2,
                            _ => 3,
                        }] += 1;
                    }
                }
            }
            bins
        };

        let raw = tally(false);
        let round = tally(true);
        println!(
            "raw     <10 {} 10-20 {} 20-70 {} >70 {}",
            raw[0], raw[1], raw[2], raw[3]
        );
        println!(
            "rounded <10 {} 10-20 {} 20-70 {} >70 {}",
            round[0], round[1], round[2], round[3]
        );

        let visible = |b: [usize; 4]| b[1] + b[2];
        assert!(
            visible(raw) > 700,
            "the fixture should be faceted to begin with: {raw:?}"
        );
        assert!(
            visible(round) * 8 < visible(raw),
            "rounding left {} visible kinks of {}",
            visible(round),
            visible(raw),
        );
        assert_eq!(
            raw[3], round[3],
            "a corner the map asserts must survive rounding untouched",
        );
    }

    /// The three properties `round_corners` is allowed to have, and the one it is
    /// not. Each is load-bearing somewhere else in this module, so each is
    /// asserted here rather than left to the fixture.
    #[test]
    fn a_fillet_keeps_the_ends_the_bearings_and_the_corner() {
        // A gentle bend on long segments: rounded.
        let bend = [[-60.0f32, 0.0], [0.0, 0.0], [50.0, 30.0]];
        let out = round_corners(&bend, 3.5);
        assert!(out.len() > bend.len(), "a 31° bend should be filleted");
        assert_eq!(
            out[0], bend[0],
            "the head is a junction node and may not move"
        );
        assert_eq!(out[out.len() - 1], bend[2], "and so is the tail",);
        // The first segment keeps its bearing — `junction_extent` and
        // `continuation` both read it off the raw line.
        let bearing = |a: [f32; 2], b: [f32; 2]| (b[1] - a[1]).atan2(b[0] - a[0]);
        assert!(
            (bearing(out[0], out[1]) - bearing(bend[0], bend[1])).abs() < 1e-4,
            "the fillet turned the first segment",
        );
        // …and it never wanders further from the recorded corner than it is
        // allowed to.
        let worst = out
            .iter()
            .map(|p| (p[0] - bend[1][0]).hypot(p[1] - bend[1][1]))
            .fold(f32::MAX, f32::min);
        assert!(
            worst <= ROUND_MAX_OFFSET_M + 1e-3,
            "the fillet cut {worst:.2} m off a corner it may cut {ROUND_MAX_OFFSET_M} m off",
        );

        // A right angle is a corner the map asserts, and survives.
        let square = [[-60.0f32, 0.0], [0.0, 0.0], [0.0, 60.0]];
        assert_eq!(round_corners(&square, 3.5), square.to_vec());

        // And a bend too tight for the road's own width is refused rather than
        // folded: a 22 m carriageway cannot be bent round a 3 m fillet.
        let tight = [[-8.0f32, 0.0], [0.0, 0.0], [7.0, 4.0]];
        assert_eq!(
            round_corners(&tight, 11.0),
            tight.to_vec(),
            "a fillet tighter than the half-width would fold the inner edge",
        );
        assert!(
            round_corners(&tight, 1.5).len() > tight.len(),
            "…while the same bend on an alley is fine",
        );
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

    /// Which of two overlapping ribbons wins has to be a property of the *roads*,
    /// not of the order the tile happened to store them in — otherwise the same
    /// junction resolves one way in this tile and the other way in its neighbour.
    #[test]
    fn the_higher_class_and_the_bridge_win_the_overlap() {
        let path = road_rung(RoadClass::Path, 2.0, false, 0);
        let street = road_rung(RoadClass::Minor, 7.0, false, 0);
        let main = road_rung(RoadClass::Major, 14.0, false, 0);
        let motorway = road_rung(RoadClass::Motorway, 22.0, false, 0);
        assert!(
            path < street && street < main && main < motorway,
            "the higher road class must be on top"
        );
        // Width still settles two roads of one class…
        assert!(
            road_rung(RoadClass::Minor, 4.0, false, 0) < road_rung(RoadClass::Minor, 9.0, false, 0),
            "the wider road of a class must be on top"
        );
        // …but it may never lift a road out of its class block. This is the
        // inversion the class-major layout exists to remove: a wide residential
        // used to outrank a narrow tertiary in the geometry while the depth bias
        // said the opposite, so which won depended on camera distance.
        assert!(
            road_rung(RoadClass::Minor, 20.0, false, 3)
                < road_rung(RoadClass::Major, 2.0, false, 0),
            "the widest Minor must still lie under the narrowest Major"
        );
        // A flyover and the road under it are both flat sheets on this plane; the
        // deck is the only one that can be on top, whatever it flies over.
        assert!(
            road_rung(RoadClass::Path, 4.0, true, 0) > motorway,
            "a bridge deck must be on top"
        );
        // …and every rung the ladder can produce has to be on the ladder, which is
        // what `road_rung_ceiling` sizes the road band against.
        for class in ROAD_CLASSES {
            for width in [1.5f32, 4.0, 7.0, 12.0, 22.0] {
                for bridge in [false, true] {
                    for seed in 0u64..40 {
                        let rung = road_rung(class, width, bridge, seed);
                        assert!(rung < ROAD_LAYERS, "rung {rung} is off the ladder");
                    }
                    let c = road_casing_rung(class, bridge);
                    assert!(c < ROAD_LAYERS, "casing rung {c} is off the ladder");
                }
            }
        }
    }

    /// Every road class in hierarchy order, for the ladder tests.
    const ROAD_CLASSES: [RoadClass; 6] = [
        RoadClass::Path,
        RoadClass::Minor,
        RoadClass::Major,
        RoadClass::Carriageway,
        RoadClass::Motorway,
        RoadClass::Rail,
    ];

    /// **There is exactly one ladder, and every road group agrees to defer to
    /// it.** This is the successor to
    /// `the_geometric_ladder_never_contradicts_the_depth_bias`, which swept every
    /// class pair looking for a place where the geometry and the depth bias
    /// disagreed. That test is gone because the contradiction it hunted is no
    /// longer expressible: the classes are ordered *by the rung*, and the bias
    /// they all share cannot rank them differently from each other because it
    /// does not rank them at all.
    ///
    /// What still has to hold is that nothing sneaks a second opinion back in.
    #[test]
    fn the_road_hierarchy_lives_in_the_rung_alone() {
        for a in ROAD_CLASSES {
            for b in ROAD_CLASSES {
                assert_eq!(
                    SurfaceGroup::Road(a).depth_bias(),
                    SurfaceGroup::Road(b).depth_bias(),
                    "{a:?} and {b:?} disagree in the material bias — the class \
                     order belongs to `road_rung`, and a bias that also ranks \
                     them is the second ladder all over again"
                );
                assert_eq!(
                    SurfaceGroup::RoadCasing(a).depth_bias(),
                    SurfaceGroup::Road(b).depth_bias(),
                    "a casing must share the road band's foot too"
                );
            }
        }
    }

    /// The hierarchy the rung has to express, now that it is the only thing
    /// expressing it: class order, casings under their own tarmac and over the
    /// narrower classes they cross, and a bridge above everything.
    #[test]
    fn the_rung_orders_the_whole_road_hierarchy() {
        let widths = [1.5f32, 3.0, 5.0, 7.0, 9.0, 12.0, 16.0, 24.0];
        let mut checked = 0usize;
        for (ai, a) in ROAD_CLASSES.into_iter().enumerate() {
            for b in ROAD_CLASSES.into_iter().skip(ai + 1) {
                for wa in widths {
                    for wb in widths {
                        for seed in 0u64..ROAD_TIE_LAYERS as u64 {
                            assert!(
                                road_rung(a, wa, false, seed) < road_rung(b, wb, false, 0),
                                "{a:?} at {wa} m must lie under {b:?} at {wb} m"
                            );
                            checked += 1;
                        }
                    }
                }
                // The higher class's *casing* rides over the lower class's
                // tarmac, so a major road's kerb band covers a path it crosses…
                assert!(
                    road_casing_rung(b, false) > road_rung(a, 24.0, false, 3),
                    "{b:?}'s kerb band must cover the widest {a:?} it crosses"
                );
                // …while still lying under its own, so it reads as an edge and
                // never over-paints its own markings.
                assert!(
                    road_casing_rung(b, false) < road_rung(b, 1.5, false, 0),
                    "{b:?}'s kerb band must stay under its own tarmac"
                );
            }
        }
        assert!(checked > 0, "the sweep must actually compare something");

        // A bridge is above every class, and its own casing is under its own
        // tarmac — the same two rules the classes follow, applied to the block
        // reserved at the top of the ladder. The second used to fail one time in
        // `ROAD_TIE_LAYERS`: the fills started on the casing's rung rather than
        // above it, so a deck's kerb band z-fought its own surface.
        for class in ROAD_CLASSES {
            for seed in 0u64..ROAD_TIE_LAYERS as u64 {
                let deck = road_rung(class, 8.0, true, seed);
                assert!(
                    deck > road_rung(RoadClass::Rail, 24.0, false, 3),
                    "a bridge must be on top of every ground road"
                );
                assert!(
                    deck > road_casing_rung(class, true),
                    "a deck's kerb band must stay under its own tarmac"
                );
                assert!(deck < ROAD_LAYERS, "bridge rung {deck} is off the ladder");
            }
        }
    }

    /// **The separation between two rungs is the same at every camera distance.**
    ///
    /// This is the property the whole redesign exists for, so it is worth
    /// asserting directly rather than inferring from the constants. A rung is
    /// `ROAD_RUNG_ULP` depth-buffer ULPs, and an ULP is proportional to the
    /// fragment's own depth — so the apparent world separation grows as `z` and
    /// the separation *measured in the units the depth test uses* is constant.
    ///
    /// The ladder it replaced was a fixed 24 µm, and the table below is why that
    /// could not work: the same two ribbons that were comfortably ordered in a
    /// street view had nothing between them from a rooftop.
    #[test]
    fn the_ladder_does_not_decay_with_range() {
        // One rung's separation, in depth-buffer ULPs, at a given camera range.
        //
        // The old ladder lifted a ribbon 24 µm and let the depth buffer convert
        // that to ULPs, which divides by the quantum `1.19e-7 · z`. The new one is
        // *already* ULPs, so `z` does not appear.
        let old_ulps = |z: f32| 2.4e-5 / (1.19e-7 * z);
        let new_ulps = |_z: f32| ROAD_RUNG_ULP;

        // Two ribbons one rung apart have to stay apart by enough to beat the
        // rasteriser's own interpolation rounding. One ULP does not; the ladder
        // is effectively gone below that.
        const RESOLVED: f32 = 2.0;

        assert!(
            old_ulps(100.0) >= RESOLVED,
            "sanity: the old ladder did work at street range"
        );
        // …and this is the failure it was rewritten to remove. A junction seen
        // from 300 m — an ordinary rooftop view, well inside the city tier — had
        // less than one ULP holding its ribbons apart.
        assert!(
            old_ulps(300.0) < 1.0,
            "the old ladder should be under one ULP at 300 m"
        );

        for z in [10.0f32, 100.0, 300.0, 1_000.0, 10_000.0] {
            assert!(
                new_ulps(z) >= RESOLVED,
                "at {z} m one rung is {} ULP, inside the rasteriser's noise",
                new_ulps(z),
            );
            // The property in one line: the separation does not depend on range.
            assert_eq!(
                new_ulps(z),
                new_ulps(10.0),
                "the ladder must not vary with distance"
            );
        }
        // And past the range where the old one had dissolved, the new one is
        // still worth the same as it was in the street.
        assert!(new_ulps(1_000.0) > old_ulps(1_000.0) * 10.0);
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

    /// One arm of a junction, as `street_nodes` would have recorded it.
    fn arm(feature: usize, half: f32, class: RoadClass, leaves: [f32; 2]) -> NodeArm {
        NodeArm {
            feature,
            half,
            class,
            axis: leaves,
            leaves: Some((leaves, 50.0)),
        }
    }

    /// A street that Shortbread split in two is not a junction with itself, and
    /// this is the common case: 58 % of the fixture's junction ends are joins
    /// like that. The old rule *floored* the crossing sine and divided by it, so
    /// two collinear features each fired the longest stub the cap allowed —
    /// twelve metres of blank tarmac over its neighbour, from both sides. A real
    /// crossing still gets its fill.
    #[test]
    fn a_street_that_continues_asks_for_no_stub() {
        let east = [1.0, 0.0];
        let mut nodes: HashMap<(i32, i32), Vec<NodeArm>> = HashMap::new();
        // Feature 0 runs west→east and ends here; feature 1 carries it on east.
        nodes.insert(
            (0, 0),
            vec![
                arm(0, 3.5, RoadClass::Minor, [-1.0, 0.0]),
                arm(1, 3.5, RoadClass::Minor, east),
            ],
        );
        assert_eq!(
            junction_extent(&nodes, [0.0, 0.0], 0, east),
            0.0,
            "a collinear continuation is not a notch to fill"
        );
        assert!(
            continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_some(),
            "the road carrying on east must be found as the continuation"
        );

        // The same node with a genuine side road crossing at 90°: the stub is
        // back, and reaches that road's far kerb.
        nodes.get_mut(&(0, 0)).unwrap()[1] = arm(1, 5.0, RoadClass::Major, [0.0, 1.0]);
        assert!(
            (junction_extent(&nodes, [0.0, 0.0], 0, east) - 5.0).abs() < 1e-4,
            "a perpendicular crossing still needs its junction filled"
        );
        assert!(
            continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none(),
            "a road crossing at 90 degrees is not a continuation"
        );

        // Nor is a road of another class, or one of a very different width,
        // however well aligned: grafting a service road's cap onto a motorway's
        // is not a join.
        nodes.get_mut(&(0, 0)).unwrap()[1] = arm(1, 11.0, RoadClass::Motorway, east);
        assert!(continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none());
        nodes.get_mut(&(0, 0)).unwrap()[1] = arm(1, 8.0, RoadClass::Minor, east);
        assert!(continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none());

        // A road that merely *passes through* the node is being crossed, not
        // joined, even when it is aligned — it has no end here to meet.
        let mut through = arm(1, 3.5, RoadClass::Minor, east);
        through.leaves = None;
        nodes.get_mut(&(0, 0)).unwrap()[1] = through;
        assert!(continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none());
    }

    /// The point of mitring a continuation rather than approximating it: the two
    /// halves must put their end vertices on the *same two points*, so the join
    /// is watertight by construction. Two square butt caps meeting at a 30° bend
    /// leave a wedge of bare ground on the outside and a doubled overlap on the
    /// inside — measured up to 1.28 m across on the fixture's 7 m streets.
    #[test]
    fn two_halves_of_one_street_meet_on_the_same_two_points() {
        let turn = 30.0f32.to_radians();
        // A comes in from the west and stops at the origin; B carries on, bent
        // by `turn`. Each is told the direction the other leaves the node in.
        let b_leaves = [turn.cos(), turn.sin()];
        let a_leaves = [-1.0, 0.0];
        let lay = |pts: &[[f32; 2]], ends: [EndJoin; 2]| {
            let (mut p, mut u, mut i) = (Vec::new(), Vec::new(), Vec::new());
            ribbon(pts, 3.5, 0.0, 12.0, ends, false, &mut p, &mut u, &mut i);
            p
        };
        let seg = 40.0;
        let a = lay(
            &[[-seg, 0.0], [0.0, 0.0]],
            [
                EndJoin::default(),
                EndJoin {
                    extend: 0.0,
                    cont: Some((b_leaves, seg)),
                },
            ],
        );
        let b = lay(
            &[[0.0, 0.0], [b_leaves[0] * seg, b_leaves[1] * seg]],
            [
                EndJoin {
                    extend: 0.0,
                    cont: Some((a_leaves, seg)),
                },
                EndJoin::default(),
            ],
        );
        // The two vertices each ribbon puts *at* the node.
        let at_node = |v: &[[f32; 3]]| -> Vec<[f32; 2]> {
            let mut out: Vec<[f32; 2]> = v
                .iter()
                .filter(|p| p[0].hypot(p[2]) < 3.5 * MITER_LIMIT + 1e-3)
                .map(|p| [p[0], p[2]])
                .collect();
            out.sort_by(|x, y| x[1].partial_cmp(&y[1]).unwrap());
            out.dedup_by(|x, y| (x[0] - y[0]).hypot(x[1] - y[1]) < 1e-4);
            out
        };
        let (ea, eb) = (at_node(&a), at_node(&b));
        assert_eq!(ea.len(), 2, "an end is two vertices, got {ea:?}");
        assert_eq!(eb.len(), 2, "an end is two vertices, got {eb:?}");
        for (x, y) in ea.iter().zip(&eb) {
            assert!(
                (x[0] - y[0]).hypot(x[1] - y[1]) < 1e-4,
                "the two halves end on different points: {x:?} against {y:?}"
            );
        }
        // And the cap really is mitred, not square: a butt end would sit on the
        // segment normal, i.e. exactly on x = 0.
        assert!(
            ea.iter().any(|p| p[0].abs() > 0.1),
            "the cap is still square: {ea:?}"
        );
    }

    /// A roundabout's centre line closes on itself, so its "two ends" are one
    /// node the road runs *through*. Butt-capping there bit a notch out of every
    /// roundabout in the city; closing the ribbon puts both ends on one miter.
    #[test]
    fn a_ring_closes_on_itself_instead_of_butting() {
        let n = 12;
        let ring: Vec<[f32; 2]> = (0..=n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                [20.0 * a.cos(), 20.0 * a.sin()]
            })
            .collect();
        let lay = |ring_flag: bool| {
            let (mut p, mut u, mut i) = (Vec::new(), Vec::new(), Vec::new());
            ribbon(
                &ring,
                3.5,
                0.0,
                12.0,
                [EndJoin::default(); 2],
                ring_flag,
                &mut p,
                &mut u,
                &mut i,
            );
            p
        };
        let closed = lay(true);
        let butted = lay(false);
        // The seam's first and last vertex pairs are the same two points when the
        // ribbon closes, and different ones when it butt-caps.
        let seam = |p: &[[f32; 3]]| {
            let last = p.len() - 2;
            (
                (p[0][0] - p[last][0]).hypot(p[0][2] - p[last][2]),
                (p[1][0] - p[last + 1][0]).hypot(p[1][2] - p[last + 1][2]),
            )
        };
        let (d0, d1) = seam(&closed);
        assert!(
            d0 < 1e-4 && d1 < 1e-4,
            "the ring does not close: seam gaps {d0} and {d1} m"
        );
        let (b0, b1) = seam(&butted);
        assert!(
            b0.max(b1) > 0.2,
            "the butt-capped control should show the notch this test is about"
        );
        // Every vertex still sits on the carriageway: the closing miter must not
        // spike, and the ring must not neck in.
        for p in &closed {
            let r = p[0].hypot(p[2]);
            assert!(
                (16.0..=24.5).contains(&r),
                "ring vertex at radius {r} is off the carriageway"
            );
        }
    }

    /// A straight run of road must come out exactly its class's width — this is
    /// the number that decides whether a street looks like a street from the
    /// pavement, and the miter maths is the easiest place to lose it.
    #[test]
    fn a_straight_ribbon_is_the_road_width_across() {
        let mut positions = Vec::new();
        let mut uvs = Vec::new();
        let mut indices = Vec::new();
        // 100 m due east, 7 m wide (a residential street), joining a 10 m road at
        // its far end and nothing at all at its near one.
        ribbon(
            &[[0.0, 0.0], [100.0, 0.0]],
            3.5,
            0.0,
            12.0,
            [
                EndJoin::default(),
                EndJoin {
                    extend: 5.0,
                    cont: None,
                },
            ],
            false,
            &mut positions,
            &mut uvs,
            &mut indices,
        );
        assert!(!indices.is_empty());
        for p in &positions {
            assert!(
                (p[2].abs() - 3.5).abs() < 1e-3,
                "edge at {:?} is not 3.5 m from the centre line",
                p
            );
        }
        // The junction stub reaches the far kerb of the road it joins, and the
        // dead end stops where the centre line does.
        let x_max = positions.iter().fold(f32::MIN, |a, p| a.max(p[0]));
        let x_min = positions.iter().fold(f32::MAX, |a, p| a.min(p[0]));
        assert!((x_max - 105.0).abs() < 1e-3, "tail at {x_max}");
        assert!(x_min.abs() < 1e-3, "head at {x_min}");
        // `v` counts metres travelled over the class's texture length.
        let v_max = uvs.iter().fold(0.0f32, |a, uv| a.max(uv[1]));
        assert!((v_max - 105.0 / 12.0).abs() < 1e-2, "v_max {v_max}");
    }

    /// **A roundabout's ribbon may not spike into its own island.**
    ///
    /// A ring is the shape this geometry is worst at: every vertex turns the same
    /// way, so every *inner* join runs toward one point, and the miter length
    /// there is `half / sin(theta/2)` — which grows without bound as a segment
    /// gets short. Groningen has roundabouts whose island is a few metres across
    /// and whose ring the encoder generalises to a dozen uneven segments, and one
    /// of those drew a tooth of tarmac across the middle of its own island
    /// (seen 2026-09-20).
    ///
    /// The invariant is radial and needs no tuning: on a closed ring of radius
    /// `r` drawn at half-width `h`, every vertex belongs in the annulus between
    /// `r - h` and `r + h`, give or take what a miter legitimately adds at a
    /// corner. A vertex nearer the centre than that has folded through the inner
    /// edge, and one at the centre or beyond it is the tooth.
    fn ring_annulus(radius: f32, segs: usize, jitter: f32, half: f32) -> (f32, f32) {
        ring_annulus_as(radius, segs, jitter, half, true)
    }

    fn ring_annulus_as(radius: f32, segs: usize, jitter: f32, half: f32, ring: bool) -> (f32, f32) {
        ring_annulus_ends(radius, segs, jitter, half, ring, 0.0)
    }

    fn ring_annulus_ends(
        radius: f32,
        segs: usize,
        jitter: f32,
        half: f32,
        ring: bool,
        extend: f32,
    ) -> (f32, f32) {
        let mut centre: Vec<[f32; 2]> = (0..segs)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32 / segs as f32 * std::f32::consts::TAU;
                // Uneven segments, which is what a generalised ring really is.
                #[allow(clippy::cast_precision_loss)]
                let r = radius * (1.0 + jitter * ((i * 7) % 5) as f32 / 5.0);
                [r * t.cos(), r * t.sin()]
            })
            .collect();
        centre.push(centre[0]);
        let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
        ribbon(
            &centre,
            half,
            0.0,
            12.0,
            [EndJoin { extend, cont: None }; 2],
            ring,
            &mut positions,
            &mut uvs,
            &mut indices,
        );
        assert!(!indices.is_empty(), "the ring built nothing");
        positions.iter().fold((f32::MAX, 0.0f32), |(lo, hi), p| {
            let d = p[0].hypot(p[2]);
            (lo.min(d), hi.max(d))
        })
    }

    /// The same ring handed over as an **open** polyline whose ends coincide —
    /// which is what a roundabout is if anything upstream fails to notice it
    /// closes. The two ends then get their own joins at the same place.
    #[test]
    fn a_ring_drawn_open_is_what_spikes() {
        let (half, radius) = (3.5, 7.5);
        let (closed_lo, _) = ring_annulus_as(radius, 12, 0.25, half, true);
        let (open_lo, _) = ring_annulus_as(radius, 12, 0.25, half, false);
        assert!(
            closed_lo > 1.0,
            "a ring drawn as a ring should keep its island: {closed_lo} m"
        );
        assert!(
            open_lo >= closed_lo - 0.01,
            "drawn open, the ring reaches {open_lo} m from its centre against \
             {closed_lo} m closed — the ends' joins are the spike"
        );
    }

    /// **An open ring whose ends are extended is the spike.**
    ///
    /// `junction_extent` runs a road's end on into whatever it meets, which is
    /// right at a tee and wrong on a circle: the stub leaves along the *tangent*,
    /// and a tangent from a small ring crosses the island. If a roundabout ever
    /// reaches `ribbon` as an open polyline with stubs — because nothing upstream
    /// noticed it closes — this is the tooth of tarmac it draws across its own
    /// middle.
    #[test]
    fn an_extended_open_ring_is_the_spike() {
        let (half, radius) = (3.5, 7.5);
        let (no_stub, _) = ring_annulus_ends(radius, 12, 0.25, half, false, 0.0);
        let (stubbed, _) = ring_annulus_ends(radius, 12, 0.25, half, false, half * 2.0);
        assert!(
            stubbed >= no_stub - 0.01,
            "stubs pull the ring to {stubbed} m from its centre against {no_stub} m \
             without them — that is a tangent crossing the island"
        );
    }

    /// **An arc of a roundabout, stubbed at both ends.**
    ///
    /// This is what a real roundabout is: OSM closes the way, the encoder splits
    /// it at each exit, and what reaches `ribbon` is three or four arcs, each an
    /// open polyline whose ends are junctions. `junction_extent` then runs each
    /// end on along its **tangent** — and the tangent of a small circle cuts the
    /// chord, which is to say it crosses the island.
    ///
    /// The full-circle cases above pass because a whole ring closes on itself and
    /// never grows a stub. Splitting it is what exposes this.
    #[test]
    fn an_arc_of_a_roundabout_does_not_cross_its_island() {
        let (half, radius) = (2.9, 6.9);
        for arc_deg in [120.0f32, 90.0, 72.0] {
            let segs = 5;
            let centre: Vec<[f32; 2]> = (0..=segs)
                .map(|i| {
                    #[allow(clippy::cast_precision_loss)]
                    let t = (i as f32 / segs as f32) * arc_deg.to_radians();
                    [radius * t.cos(), radius * t.sin()]
                })
                .collect();
            let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
            ribbon(
                &centre,
                half,
                0.0,
                12.0,
                [EndJoin {
                    extend: half * 2.0,
                    cont: None,
                }; 2],
                false,
                &mut positions,
                &mut uvs,
                &mut indices,
            );
            let nearest = positions
                .iter()
                .fold(f32::MAX, |a, p| a.min(p[0].hypot(p[2])));
            assert!(
                nearest > radius - half - 0.6,
                "a {arc_deg} deg arc reaches {nearest} m from the centre, inside the \
                 {} m its inner edge may reach — the end stubs are crossing the island",
                radius - half
            );
        }
    }

    /// **A kink in a ring must not throw its inner edge across the island.**
    ///
    /// This is the real Groningen roundabout, reduced: a generalised ring with one
    /// vertex sharper than its neighbours. The *outer* side of a sharp corner is
    /// bevelled once the miter passes [`MITER_LIMIT`]; the inner side is clamped
    /// only in how far it runs back **along** the two segments, and nothing bounds
    /// how far it moves **across** them. At a sharp kink the bisector points over
    /// the carriageway, so the inner vertex lands well inside the ring — a tooth
    /// of tarmac across the middle of the island, which is what the map drew
    /// (seen 2026-09-20, in the carriageway and in its kerb band at the same
    /// vertex, because both come off one centre line).
    #[test]
    fn a_kink_in_a_ring_does_not_cross_the_island() {
        let (half, radius) = (2.9, 6.9);
        let segs = 12;
        let mut centre: Vec<[f32; 2]> = (0..segs)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32 / segs as f32 * std::f32::consts::TAU;
                [radius * t.cos(), radius * t.sin()]
            })
            .collect();
        // One vertex pulled in hard — a generalised ring's short segment.
        centre[3] = [centre[3][0] * 0.35, centre[3][1] * 0.35];
        centre.push(centre[0]);

        let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
        ribbon(
            &centre,
            half,
            0.0,
            12.0,
            [EndJoin::default(); 2],
            true,
            &mut positions,
            &mut uvs,
            &mut indices,
        );
        let nearest = positions
            .iter()
            .fold(f32::MAX, |a, p| a.min(p[0].hypot(p[2])));
        // The kinked vertex itself sits at 0.35 r, so its own edges legitimately
        // reach `0.35 r - half`. Past the centre is never legitimate.
        assert!(
            nearest > 0.0,
            "the ribbon reached {nearest} m from the centre — through the middle \
             of the island and out the other side"
        );
    }

    /// **A road joining a roundabout must not stub across the island**, which is
    /// the tooth, and these are the tile's own coordinates for it.
    ///
    /// `--dump-lines` over `14/8490/5321`: the roundabout is the closing tail of a
    /// `residential` way, and a `service` road begins **on one of its vertices**
    /// and leaves to the south-east. `junction_extent` runs that end on into
    /// whatever it meets, because at a tee the two have to overlap for the cut to
    /// resolve them. A roundabout is not a tee: the road it meets curves away, so
    /// a straight stub cuts the chord — and on a ring this small the chord is the
    /// island.
    ///
    /// The five synthetic rings above all pass because they test the *ring's* own
    /// ends. The tooth belongs to the road joining it.
    /// **Known fault, reproduced, not yet fixed** — `#[ignore]` so it states the
    /// bug without failing the suite. Run it with
    /// `cargo test -p cartopolis_geo -- --ignored stub_across`.
    ///
    /// Measured: the stub reaches **2.35 m** of the roundabout's centre where the
    /// island begins at 3.05 m. The fix is not a smaller cap — the reach is
    /// already bounded at `2 x half` and the overshoot is 0.7 m, so any constant
    /// that fixes this one is a threshold waiting for a tighter roundabout. It
    /// has to come from the joined road's **curvature at the node**: `half / sin`
    /// is the far kerb of a road that runs straight through, and a road that
    /// turns there has its far kerb nearer. `NodeArm` carries one axis per arm and
    /// a ring contributes two, so the turn is available; spending it correctly is
    /// the work.
    #[test]
    #[ignore = "reproduces a known fault: a stub crosses a small roundabout's island"]
    fn a_road_joining_a_roundabout_does_not_stub_across_the_island() {
        // The ring, verbatim, and the centre it turns about.
        let ring: [[f32; 2]; 10] = [
            [-1.97, -6.10],
            [-6.26, -3.24],
            [-7.33, 1.77],
            [-4.83, 6.06],
            [-0.54, 7.85],
            [1.97, 7.49],
            [5.54, 4.99],
            [6.97, 0.34],
            [6.26, -2.17],
            [2.68, -5.74],
        ];
        #[allow(clippy::cast_precision_loss)]
        let hub = ring.iter().fold([0.0f32; 2], |a, p| {
            [
                a[0] + p[0] / ring.len() as f32,
                a[1] + p[1] / ring.len() as f32,
            ]
        });
        let inner = ring
            .iter()
            .fold(f32::MAX, |a, p| a.min((p[0] - hub[0]).hypot(p[1] - hub[1])))
            - 3.5;

        // The service road, verbatim, stubbed back into the ring at its head.
        let spur = [[5.54, 4.99], [17.70, 39.68]];
        let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
        ribbon(
            &spur,
            2.0,
            0.0,
            12.0,
            [
                EndJoin {
                    extend: 7.0,
                    cont: None,
                },
                EndJoin::default(),
            ],
            false,
            &mut positions,
            &mut uvs,
            &mut indices,
        );
        let nearest = positions
            .iter()
            .fold(f32::MAX, |a, p| a.min((p[0] - hub[0]).hypot(p[2] - hub[1])));
        assert!(
            nearest >= inner,
            "the joining road reaches {nearest} m of the roundabout's centre, \
             inside the {inner} m where the island is — that is the tooth"
        );
    }

    #[test]
    fn a_roundabout_does_not_spike_into_its_island() {
        // A real small roundabout: ~7.5 m centre-line radius, a 7 m carriageway.
        for (segs, jitter) in [(12, 0.0), (12, 0.25), (9, 0.3), (8, 0.35)] {
            let (half, radius) = (3.5, 7.5);
            let (lo, hi) = ring_annulus(radius, segs, jitter, half);
            let floor = (radius * (1.0 - jitter)) - half * 1.6;
            assert!(
                lo > floor.max(0.0),
                "{segs} segments, jitter {jitter}: a vertex {lo} m from the centre, \
                 inside the {floor} m the inner edge may reach — the ribbon has \
                 folded through its own island"
            );
            assert!(hi < radius * (1.0 + jitter) + half * 1.6, "outer {hi}");
        }
    }

    /// Build one corner and hand back the ribbon's vertices in the XZ plane, each
    /// tagged with its `u` (which side of the road it is), plus the centre line
    /// the ribbon was actually built around.
    ///
    /// Built with no junction stubs (`extend = [0, 0]`): this is about what the
    /// join does to the two segments meeting at the corner, and a stub only ever
    /// lengthens the outer *ends*, which both tests filter away anyway.
    fn corner(turn_deg: f32, seg_m: f32, half: f32) -> (Vec<([f32; 2], f32)>, Vec<[f32; 2]>) {
        let t = turn_deg.to_radians();
        let centre = vec![
            [-seg_m, 0.0],
            [0.0, 0.0],
            [seg_m * t.cos(), seg_m * t.sin()],
        ];
        let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
        ribbon(
            &centre,
            half,
            0.0,
            12.0,
            [EndJoin::default(); 2],
            false,
            &mut positions,
            &mut uvs,
            &mut indices,
        );
        assert_eq!(uvs.len(), positions.len());
        assert!(!indices.is_empty());
        (
            positions
                .iter()
                .zip(&uvs)
                .map(|(p, uv)| ([p[0], p[2]], uv[0]))
                .collect(),
            // The line the ribbon is actually built on, which is the rounded one
            // wherever `corner_cut` allowed a fillet. Handing back the *input*
            // here would measure every one of these tests against a centre line
            // the road does not follow.
            round_corners(&centre, half),
        )
    }

    /// The **outer** edge of a bend keeps the full carriageway, however sharp.
    ///
    /// A single miter clamped by `MITER_LIMIT` pulled both edges in together, so a
    /// hairpin necked the road down instead of blunting its corner: a 22 m
    /// motorway measured 14.2 m across through a 150° turn and 4.8 m through a
    /// 170° one. Past the limit the corner is bevelled — two square ends at the
    /// full half-width — so the outer edge stays a genuine offset of the
    /// carriageway it belongs to.
    ///
    /// Measured against the segment *lines*, not the nearest point on the centre
    /// line: on the outside of a corner the nearest centre-line point is the
    /// corner vertex itself, which reports `half × miter` and so cannot tell a
    /// stretched join from a necked one.
    #[test]
    fn a_sharp_bend_keeps_the_full_carriageway_width() {
        let half = 11.0; // a 22 m motorway, the widest class and the worst case
        for turn in [5.0f32, 60.0, 120.0, 150.0, 170.0] {
            let t = turn.to_radians();
            let (verts, _) = corner(turn, 60.0, half);
            // Both segments run through the corner at the origin, so a vertex's
            // distance from each carriageway is just the cross product.
            let perp = |p: [f32; 2], d: [f32; 2]| (p[0] * d[1] - p[1] * d[0]).abs();
            // A positive turn puts the inside of the corner on `+m`, which is
            // `u = 1`; the outer edge is therefore the `u = 0` one. The segments
            // are 60 m long so that the corner's own vertices are the only ones
            // anywhere near the origin — every other vertex is a plain
            // perpendicular offset with nothing to say about the join.
            let at_corner = verts
                .iter()
                .filter(|(_, u)| *u == 0.0)
                .filter(|(p, _)| p[0].hypot(p[1]) < 45.0);
            let mut seen = 0;
            for (p, _) in at_corner {
                // Every outer vertex serves one of the two segments; it must sit
                // the full half-width off of that one.
                let reach = perp(*p, [1.0, 0.0]).max(perp(*p, [t.cos(), t.sin()]));
                assert!(
                    reach >= half - 1e-2,
                    "{turn}° turn necks the carriageway to {reach:.2} m of {half} m"
                );
                assert!(
                    reach <= half * MITER_LIMIT + 1e-2,
                    "{turn}° turn spikes the outer corner to {reach:.2} m"
                );
                seen += 1;
            }
            assert!(seen > 0, "{turn}° turn: no outer vertex at the corner");
        }
    }

    /// The **inner** edge must not fold back through the ribbon.
    ///
    /// The inner miter backtracks `half / tan(θ/2)` along both neighbours, and
    /// nothing bounded that against how long they actually are: on a 22 m
    /// motorway a 60° turn overran a 6 m segment — and z14 Shortbread geometry is
    /// full of sub-10 m segments. The fold is a self-intersecting quad, whose two
    /// layers carry different `v`, i.e. the centre line fighting its own
    /// reflection along the bend.
    ///
    /// A fold is the inner rail *reversing* along the road, so that is what this
    /// measures. It is not about quads overlapping each other: a 22 m carriageway
    /// around a 3 m corner has to double back over itself, and the union of that
    /// overlap is still the right piece of road.
    #[test]
    fn a_tight_corner_does_not_fold_the_ribbon() {
        for turn in [30.0f32, 60.0, 90.0, 120.0, 150.0, 170.0] {
            for seg in [3.0f32, 6.0, 15.0, 60.0] {
                let (verts, centre) = corner(turn, seg, 11.0);
                // A bevel splits the *outer* vertex in two and shares the inner
                // one, so the inner rail is the one with a vertex per centre-line
                // point — exactly the rail a fold shows up on, and the pairing
                // that lets each vertex be measured against its own segment
                // rather than whichever happens to be nearest (with a 22 m road
                // on 3 m segments, that is routinely the wrong one).
                let inner: Vec<[f32; 2]> = verts
                    .iter()
                    .filter(|(_, u)| *u == 1.0)
                    .map(|(p, _)| *p)
                    .collect();
                assert_eq!(inner.len(), centre.len(), "one inner vertex per point");
                for k in 0..centre.len() - 1 {
                    let (a, b) = (centre[k], centre[k + 1]);
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    let len = dx.hypot(dy);
                    let (ux, uy) = (dx / len, dy / len);
                    // How far each end of this quad's inner rail has advanced
                    // along the segment it belongs to.
                    let along = |p: [f32; 2]| (p[0] - a[0]) * ux + (p[1] - a[1]) * uy;
                    let (from, to) = (along(inner[k]), along(inner[k + 1]));
                    assert!(
                        to >= from - 1e-3,
                        "{turn}° turn on {seg} m segments folds the ribbon: over segment \
                         {k} the inner edge runs backwards, {from:.2} m to {to:.2} m"
                    );
                }
            }
        }
    }

    /// A join may not throw a spike into the field.
    ///
    /// [`MITER_LIMIT`] bounds how far the outer corner may stand from the centre
    /// line, and the bevel is what enforces it. This measures the geometry rather
    /// than the constant: every vertex of the built ribbon, against the line it
    /// was built on.
    #[test]
    fn a_reversal_is_bevelled_rather_than_drawn_as_a_spike() {
        let seg_dist = |p: [f32; 2], a: [f32; 2], b: [f32; 2]| -> f32 {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 1e-9 {
                (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (p[0] - (a[0] + dx * t)).hypot(p[1] - (a[1] + dy * t))
        };
        for turn in [30.0f32, 60.0, 90.0, 100.0, 128.2, 150.0, 170.0] {
            for half in [2.0f32, 4.5, 11.0] {
                let (verts, centre) = corner(turn, 6.0, half);
                let worst = verts
                    .iter()
                    .map(|(p, _)| {
                        centre
                            .windows(2)
                            .map(|w| seg_dist(*p, w[0], w[1]))
                            .fold(f32::MAX, f32::min)
                    })
                    .fold(0.0f32, f32::max);
                assert!(
                    worst <= half * MITER_LIMIT + 1e-3,
                    "a {turn}° turn on a {:.0} m road reaches {worst:.2} m from its centre \
                     line, past the {:.2} m the miter limit allows",
                    half * 2.0,
                    half * MITER_LIMIT,
                );
            }
        }

        // …and the limit is where it is for a reason, which is the half of this
        // that would otherwise drift. A right angle mitres at `1 / cos 45°` and
        // **keeps its point** — a street grid's right angles are corners the map
        // asserts, the same licence `ROUND_MAX_DEG` draws. A reversal does not:
        // the Groningen fixture's feature 69 is a `tertiary` turning head that
        // reverses through 128.2°, and its miter of `1 / cos 64.1° = 2.29` slipped
        // under the old limit of 2.5 and drew a ten-metre tongue of tarmac out
        // into the field past the end of the road.
        assert!(
            MITER_LIMIT >= 1.0 / std::f32::consts::FRAC_PI_4.cos(),
            "a right angle must keep its point",
        );
        assert!(
            MITER_LIMIT < 1.0 / 64.1f32.to_radians().cos(),
            "a 128° reversal must bevel rather than mitre",
        );
    }

    /// Clipping, not clamping: geometry in the MVT buffer zone must be *cut* at
    /// the tile border, or both neighbours draw the same overlapping tarmac.
    #[test]
    fn polylines_are_clipped_at_the_tile_border() {
        // Enters at x = 0, leaves at x = 100, having started and ended outside.
        let runs = clip_polyline(&[[-50.0, 10.0], [150.0, 10.0]], 0.0, 100.0);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].first().unwrap()[0], 0.0);
        assert_eq!(runs[0].last().unwrap()[0], 100.0);

        // Out, in, out again → one run, and it is the middle stretch only.
        let runs = clip_polyline(&[[-10.0, 50.0], [50.0, 50.0], [110.0, 50.0]], 0.0, 100.0);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len(), 3);

        // A line that leaves and re-enters breaks into two runs rather than
        // cutting the corner across the tile.
        let runs = clip_polyline(
            &[[50.0, 50.0], [150.0, 50.0], [150.0, 80.0], [50.0, 80.0]],
            0.0,
            100.0,
        );
        assert_eq!(runs.len(), 2, "re-entry must start a new run");

        // Entirely outside: nothing at all.
        assert!(clip_polyline(&[[-10.0, -10.0], [-5.0, -5.0]], 0.0, 100.0).is_empty());
    }

    #[test]
    fn raster_sniffing() {
        assert!(looks_like_raster(b"\x89PNG\r\n\x1a\n____"));
        assert!(looks_like_raster(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(!looks_like_raster(GRONINGEN_Z14));
    }
}
