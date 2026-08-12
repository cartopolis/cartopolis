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

use std::cell::OnceCell;

use geo_types::Geometry;
use mvt_reader::feature::{Feature, Value};
use mvt_reader::layer::Layer;
use mvt_reader::Reader;
use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Shader, Stroke, Transform,
};

use std::collections::HashMap;

use crate::road_texture::{road_class, road_width_m, RoadClass, StreetFlags};

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
/// decoded twice each. `OnceCell` (not `RefCell`) because a decoded layer is never
/// invalidated; the interior mutability is initialise-once, so `&TileData` stays
/// the shape every consumer wants.
///
/// Deliberately **not** cached across tasks: a parsed reader holds `geo_types`
/// f32 geometry for every feature in the tile and nothing here measures how much
/// that is, whereas `tile_source`'s byte cache holds things whose size is exactly
/// known. Collapsing the ~179 repeat parses a fresh anchor makes is the obvious
/// follow-up, once someone has a number for the memory.
pub struct TileData {
    reader: Reader,
    layers: Vec<Layer>,
    /// Decoded features per `Layer::layer_index`. Empty for a layer that failed
    /// to decode — the byte-taking callers all treated that as "skip the layer",
    /// and folding it in here keeps that behaviour in one place.
    features: Vec<OnceCell<Vec<Feature>>>,
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
            features: (0..slots).map(|_| OnceCell::new()).collect(),
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

/// Depth-buffer units between adjacent rungs of the coplanar ground stack, and of
/// everything else drawn flat on it (the route line, the driver arrow).
///
/// Every bias in the scene is a small integer rung times this, so scaling it moves
/// the whole stack together and cannot reorder it. It exists because the rungs used
/// to *be* the bias: neighbours one unit apart, which is one ULP of the depth
/// buffer and below the per-frame rounding noise. See [`SurfaceGroup::depth_bias`]
/// for why the millimetre `y_offset`s stop covering for that as you climb.
///
/// 16 was not enough on its own — lakes still took the occasional triangle
/// through the surface at the oblique angles a low camera looks at the middle
/// distance with, where the depth gradient across a pixel is at its steepest.
/// This is the blunt lever for that; the sharp one is `bias.slope_scale`, which
/// is the textbook remedy for grazing angles and is *proportional* to that
/// gradient, but `StandardMaterial` exposes only `constant` and reaching the
/// other field means wrapping every surface group in an `ExtendedMaterial` to
/// get at `MaterialExtension::specialize`. If a lake ever flickers again, that
/// is the fix — not another doubling here.
pub const DEPTH_BIAS_STEP: f32 = 64.0;

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
        if z <= MAX_SOURCE_ZOOM {
            return ((z, x, y), Self::WHOLE);
        }
        let shift = z - MAX_SOURCE_ZOOM;
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
    /// it read as a bright cut straight across every lake. Sampled from a render,
    /// the shaded surface sits around `#36495C`–`#415462`; this is picked to land
    /// there once the sun has been on it.
    ///
    /// So this is not a free colour choice: move `water::WATER_ALBEDO` and this
    /// has to move with it.
    ///
    /// It is pulled the other way too, and that one is silent. On a *raster*
    /// basemap the globe has no geometry to ask, so it decides what is ocean by
    /// how much bluer than red a texel is (`tile_loader::WATER_BLUE_BIAS`): this
    /// has to stay at least 40 bluer than it is red or the planet quietly loses
    /// its oceans. Matching the shaded tone wants it greyer, that wants it blue,
    /// and this value is where the two settle — the residual blue cast at the ring
    /// edge is the price. `tile_loader`'s
    /// `the_basemaps_water_paint_reads_as_fully_water` holds the line.
    pub const WATER: u32 = 0x3E_5A_70;
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
    pub const RESIDENTIAL: u32 = 0xE6_E1_D8;
    pub const INDUSTRIAL: u32 = 0xE0_DC_D8;
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

/// Road class → (width in px, fill colour, casing colour). Widths are at the source
/// tile's scale and are multiplied by the overzoom factor, so a street keeps its
/// real-world width as you magnify past z14 instead of thinning to a hair.
fn street_style(kind: &str) -> (f32, u32, u32) {
    match kind {
        "motorway" | "trunk" => (5.0, palette::MOTORWAY_FILL, palette::MOTORWAY_CASING),
        "primary" => (4.0, palette::MOTORWAY_FILL, palette::MOTORWAY_CASING),
        "secondary" => (3.5, palette::ROAD_FILL, palette::ROAD_CASING),
        "tertiary" => (3.0, palette::ROAD_FILL, palette::ROAD_CASING),
        "residential" | "unclassified" | "living_street" => {
            (2.5, palette::ROAD_FILL, palette::ROAD_CASING)
        }
        "service" => (1.6, palette::ROAD_FILL, palette::ROAD_CASING),
        "rail" | "light_rail" | "subway" | "tram" => (1.4, palette::RAIL, palette::RAIL),
        "pedestrian" | "footway" | "path" | "track" | "cycleway" | "steps" => {
            (1.0, palette::PATH, palette::PATH)
        }
        _ => (2.0, palette::ROAD_FILL, palette::ROAD_CASING),
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

/// Render an MVT tile body to a `TILE_PX × TILE_PX` RGBA8 buffer.
///
/// `sub` selects the magnified sub-rectangle for overzoomed requests; pass
/// [`SubTile::WHOLE`] to draw the source tile as-is.
pub fn render_mvt(tile: &TileData, sub: SubTile) -> Result<Vec<u8>, String> {
    render_mvt_sized(tile, sub, TILE_PX)
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
pub fn render_mvt_sized(tile: &TileData, sub: SubTile, size: u32) -> Result<Vec<u8>, String> {
    // Cull only when overzooming: at `factor == 1` the window is the whole tile,
    // so every feature passes and the bounds scan is pure overhead.
    render_mvt_culled(tile, sub, size, sub.factor > 1)
}

/// [`render_mvt_sized`] with the off-screen-feature cull switchable, so a test can
/// assert the two produce identical pixels.
fn render_mvt_culled(
    tile: &TileData,
    sub: SubTile,
    size: u32,
    cull: bool,
) -> Result<Vec<u8>, String> {
    let mut pixmap = Pixmap::new(size, size).ok_or_else(|| "pixmap alloc failed".to_string())?;
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

    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };

    for (layer_name, draw) in PAINT_ORDER {
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
            -(sub.y as f32) * px,
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

        for feature in features {
            let kind = kind_of(feature);
            // Bounds once per feature, reused by whichever arm draws it. tiny-skia
            // has no early-out of its own: `fill_path` clones and transforms the
            // whole path before any bounds test (its `// TODO: ignore paths
            // outside the pixmap` is still there), and `stroke_path` builds the
            // full stroke outline first. So an off-screen feature costs a
            // `PathBuilder`, a `Path`, a transform pass and — for a non-hairline
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
            match draw {
                Draw::Fill(rgb) => {
                    if !visible(0.0) {
                        continue;
                    }
                    paint.shader = shader(*rgb);
                    fill_geometry(&mut pixmap, &paint, &feature.geometry, &transform);
                }
                Draw::FillByKind { default, kinds } => {
                    if !visible(0.0) {
                        continue;
                    }
                    let rgb = kind
                        .and_then(|k| kinds.iter().find(|(n, _)| *n == k).map(|(_, c)| *c))
                        .unwrap_or(*default);
                    paint.shader = shader(rgb);
                    fill_geometry(&mut pixmap, &paint, &feature.geometry, &transform);
                }
                Draw::Line { color, width } => {
                    if !visible(width * wf * 0.5) {
                        continue;
                    }
                    paint.shader = shader(*color);
                    stroke_geometry(
                        &mut pixmap,
                        &paint,
                        &feature.geometry,
                        &transform,
                        width * wf,
                    );
                }
                Draw::Outline { color, width } => {
                    if !visible(width * wf * 0.5) {
                        continue;
                    }
                    paint.shader = shader(*color);
                    stroke_geometry(
                        &mut pixmap,
                        &paint,
                        &feature.geometry,
                        &transform,
                        width * wf,
                    );
                }
                Draw::Streets { casing } => {
                    let (w, fill, case) = street_style(kind.unwrap_or(""));
                    let (color, width) = if *casing {
                        (case, (w + 1.2) * wf)
                    } else {
                        (fill, w * wf)
                    };
                    if !visible(width * 0.5) {
                        continue;
                    }
                    paint.shader = shader(color);
                    stroke_geometry(&mut pixmap, &paint, &feature.geometry, &transform, width);
                }
            }
        }
    }

    // Fully opaque throughout (opaque background, opaque paints), so tiny-skia's
    // premultiplied buffer is already straight RGBA.
    Ok(pixmap.take())
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
fn push_line(pb: &mut PathBuilder, coords: impl Iterator<Item = (f32, f32)>, close: bool) -> bool {
    let mut started = false;
    for (x, y) in coords {
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        if started {
            pb.line_to(x, y);
        } else {
            pb.move_to(x, y);
            started = true;
        }
    }
    if started && close {
        pb.close();
    }
    started
}

/// Build a path covering a geometry's *areas* (polygon rings), or `None`.
fn area_path(geom: &Geometry<f32>) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let mut any = false;
    let add_poly = |p: &geo_types::Polygon<f32>, pb: &mut PathBuilder| {
        let mut drew = push_line(pb, p.exterior().coords().map(|c| (c.x, c.y)), true);
        for hole in p.interiors() {
            drew |= push_line(pb, hole.coords().map(|c| (c.x, c.y)), true);
        }
        drew
    };
    match geom {
        Geometry::Polygon(p) => any |= add_poly(p, &mut pb),
        Geometry::MultiPolygon(mp) => {
            for p in mp {
                any |= add_poly(p, &mut pb);
            }
        }
        _ => return None,
    }
    if !any {
        return None;
    }
    pb.finish()
}

/// Build a path covering a geometry's *lines* — polygon rings count, so the same
/// helper serves `Draw::Outline`.
fn line_path(geom: &Geometry<f32>) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let mut any = false;
    match geom {
        Geometry::LineString(ls) => {
            any |= push_line(&mut pb, ls.coords().map(|c| (c.x, c.y)), false);
        }
        Geometry::MultiLineString(mls) => {
            for ls in mls {
                any |= push_line(&mut pb, ls.coords().map(|c| (c.x, c.y)), false);
            }
        }
        Geometry::Polygon(_) | Geometry::MultiPolygon(_) => return area_path(geom),
        _ => return None,
    }
    if !any {
        return None;
    }
    pb.finish()
}

fn fill_geometry(pixmap: &mut Pixmap, paint: &Paint, geom: &Geometry<f32>, t: &Transform) {
    if let Some(path) = area_path(geom) {
        pixmap.fill_path(&path, paint, FillRule::EvenOdd, *t, None);
    }
}

fn stroke_geometry(
    pixmap: &mut Pixmap,
    paint: &Paint,
    geom: &Geometry<f32>,
    t: &Transform,
    width: f32,
) {
    if width <= 0.0 {
        return;
    }
    if let Some(path) = line_path(geom) {
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, paint, &stroke, *t, None);
    }
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
}

impl SurfaceGroup {
    /// Base colour for the group's material. Lit by the scene's own sun, unlike the
    /// raster path's baked-in shading — this is the point of ADR-035 option B.
    #[must_use]
    pub fn base_color(self) -> [f32; 3] {
        let rgb = match self {
            // Not where water's colour comes from, and never reached: `map_geometry`
            // builds a `WaterMaterial` for any cell holding a water mesh and takes
            // that branch for every one of them, shading toggle or not, so this
            // function only ever sees the other groups. Kept because the match is
            // exhaustive and this is the honest fallback anyway — `palette::WATER`
            // is *tuned* to the tone `water::WATER_ALBEDO` shades to, so a water
            // sheet that somehow arrived here would still land the right colour
            // rather than panic in the middle of a tile stream.
            Self::Water => palette::WATER,
            Self::Forest => palette::FOREST,
            Self::Grass => palette::GRASS,
            Self::Sand => palette::SAND,
            Self::Built => palette::RESIDENTIAL,
            // Roads carry their surface in a texture; tinting it would only fight
            // the paint values chosen there, so the material multiplies by white.
            Self::Road(_) => 0xFF_FF_FF,
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

    /// Height in metres, relative to the ground plane at y = 0.
    ///
    /// Surfaces sit in the narrow window **between the map tile's top face and the
    /// buildings' base**: `map_stream` spawns each tile as a 0.04 m cuboid centred
    /// at −0.03, so the finest tier's top face is at −0.01, and every building
    /// (3DBAG and Overpass alike) stands on y = 0. Drawing surfaces *above* zero —
    /// the first attempt — pushed grass and water up through the bottom courses of
    /// the buildings, which is the green bleeding out from under the grey.
    ///
    /// The window is only 10 mm. That is enough to settle the stack at street
    /// range and nowhere near enough from the air: measured in depth-buffer units
    /// a geometric gap shrinks as 1/z, so these offsets fade out exactly as you
    /// climb. [`Self::depth_bias`] is what has to hold at altitude; these fix the
    /// ordering intent and carry it up close.
    ///
    /// The millimetres inside the window are **not** spread evenly, because the
    /// fights are not equally visible. Land against land is pale green against
    /// pale grey and can lose without anyone noticing; land against water is the
    /// one that shows, dark against pale, and it is the one that came back as
    /// triangles flickering up through a lake. So the land groups stay packed at
    /// 1 mm apart and water takes the widest gap in the stack — 4 mm of clear air
    /// under it, 2 mm over it to the roads. Spend the window where the contrast
    /// is.
    #[must_use]
    pub fn y_offset(self) -> f32 {
        match self {
            Self::Built => -0.0095,
            Self::Grass => -0.0085,
            Self::Forest | Self::Sand => -0.0075,
            Self::Water => -0.0035,
            // Roads go on top of every natural surface — a street crosses a park,
            // never the other way round.
            Self::Road(_) => -0.0015,
            // Trees are real 3D geometry standing *on* the ground, not another
            // sheet in the coplanar stack, so they take the ground plane itself.
            // A bridge deck is the same kind of thing: it carries its own height,
            // which is a measurement and not a place in this millimetre ladder.
            Self::BridgeDeck | Self::Canopy | Self::Trunk | Self::Furniture => 0.0,
        }
    }

    /// Depth-buffer bias, which is what actually stops the flicker.
    ///
    /// The rung ladder below is multiplied by [`DEPTH_BIAS_STEP`], and the spacing
    /// is the whole point. `StandardMaterial::depth_bias` reaches the hardware
    /// depth bias — Bevy's `StandardMaterial::specialize` writes it to
    /// `DepthStencilState::bias.constant`, and `ExtendedMaterial` chains to the
    /// base's `specialize`, so the water material gets it too. But that field is
    /// denominated in **depth-buffer ULPs**, not world units, so a ladder in steps
    /// of 1.0 buys exactly one ULP between neighbours — less than the rasteriser's
    /// own rounding noise, which re-rolls every frame as the view matrix moves.
    ///
    /// The `y_offset` millimetres do not make up the difference at range. The depth
    /// buffer is reversed-z `Depth32Float`, so for a primitive at depth `D` one
    /// bias unit is `r = 2^(e-23) ≈ D·1.19e-7`; expressing a geometric gap `Δy` in
    /// those units gives `Δy / (1.19e-7 · z)`, which **decays as 1/z**. Water sits
    /// 2 mm above Forest: ~3.4 ULP of margin at 5 km, ~1.1 ULP at 15 km. Climbing
    /// collapses that term until only the ladder is left, which is why the flicker
    /// appears when the camera moves in *height* and freezes when it stops.
    ///
    /// Widening the ladder is safe at every range precisely because the unit is
    /// proportional to the primitive's own depth: the apparent world-space shift is
    /// `1.19e-7 · bias · z`, so water's 64 units read as ~4 cm at 5 km and ~40 µm
    /// at 10 m. It cannot punch through a building, which is separated by metres of
    /// honest depth.
    ///
    /// `bias.slope_scale` stays 0 — `StandardMaterial` exposes only `constant`, and
    /// reaching the other field would mean wrapping every group in an
    /// `ExtendedMaterial` just to get at `MaterialExtension::specialize`.
    #[must_use]
    pub fn depth_bias(self) -> f32 {
        self.depth_rung() * DEPTH_BIAS_STEP
    }

    /// The group's rung in the coplanar ground stack, low to high. Ordering intent
    /// only; [`Self::depth_bias`] turns it into depth-buffer units.
    #[must_use]
    pub fn depth_rung(self) -> f32 {
        match self {
            Self::Built => 1.0,
            Self::Grass => 2.0,
            Self::Forest | Self::Sand => 3.0,
            Self::Water => 4.0,
            // Road classes are separated from each other as well as from the
            // ground: ribbons genuinely overlap at every junction, and equal
            // biases there would z-fight in exactly the places you look at. The
            // order is the road hierarchy, so a motorway crosses a service road.
            Self::Road(RoadClass::Path) => 5.0,
            Self::Road(RoadClass::Minor) => 6.0,
            Self::Road(RoadClass::Major) => 7.0,
            // A carriageway is a motorway's half or a ramp: above the ordinary
            // roads it flies over, below the motorway it feeds — a slip road
            // merging into a carriageway must not paint its kerb line across it.
            Self::Road(RoadClass::Carriageway) => 8.0,
            Self::Road(RoadClass::Motorway) => 9.0,
            // Rails sit above their own crossings, so a level crossing shows the
            // track rather than the tarmac.
            Self::Road(RoadClass::Rail) => 10.0,
            // A deck is above the tracks, which are above the tarmac — and that
            // ordering is the *whole* point of the group. A flyover measured at
            // ~0 m (a Dutch canal bridge sits at street level, with the water
            // below) has no honest depth to separate it from the road it carries,
            // so this rung is what stops the rails painting over the viaduct.
            Self::BridgeDeck => 11.0,
            // Above the whole coplanar ground stack. Trees resolve against it by
            // honest depth — they stand a metre or ten above the plane — but the
            // ground sheets are biased *forward* to settle their own z-fight, and
            // a grass sheet at 2.0 would otherwise punch through a trunk base.
            // Street furniture stands on the same plane and wants the same rung —
            // a bench's legs are exactly the thin thing a biased sheet swallows.
            Self::Canopy | Self::Trunk | Self::Furniture => 12.0,
        }
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
fn land_group(kind: &str) -> Option<SurfaceGroup> {
    Some(match kind {
        "forest" | "wood" => SurfaceGroup::Forest,
        "grass" | "meadow" | "park" | "garden" | "farmland" | "orchard" | "scrub" | "heath"
        | "allotments" | "village_green" | "recreation_ground" => SurfaceGroup::Grass,
        "sand" | "beach" | "bare_rock" | "scree" => SurfaceGroup::Sand,
        "residential" | "industrial" | "commercial" | "retail" | "quarry" => SurfaceGroup::Built,
        _ => return None,
    })
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
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(ring.len());
    for p in ring {
        if out
            .last()
            .is_none_or(|q: &[f32; 2]| (p[0] - q[0]).abs() > 1e-5 || (p[1] - q[1]).abs() > 1e-5)
        {
            out.push(p);
        }
    }
    while out.len() >= 2 && out[0] == out[out.len() - 1] {
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
#[derive(Debug, Clone)]
pub struct SurfaceMesh {
    pub group: SurfaceGroup,
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
}

/// Tessellate a tile's area layers into one mesh per [`SurfaceGroup`].
///
/// `tile_m` is the tile's ground size in metres, so the output is directly in world
/// units. Only water and landuse are handled here — buildings deliberately are not,
/// because 3DBAG (ADR-027) and the Overpass detail patches (ADR-022) already supply
/// them in 3D, and they are ~75% of a tile's triangles.
pub fn tessellate_surfaces(tile: &TileData, tile_m: f32) -> Result<Vec<SurfaceMesh>, String> {
    let mut builders: Vec<(SurfaceGroup, Vec<[f32; 3]>, Vec<u32>)> = Vec::new();

    for meta in tile.layers() {
        // (layer, fixed group) — `land` resolves its group per feature instead.
        let fixed = match meta.name.as_str() {
            "ocean" | "water_polygons" => Some(SurfaceGroup::Water),
            "sites" => Some(SurfaceGroup::Built),
            "land" => None,
            _ => continue,
        };
        let extent = TileData::extent(meta);
        let scale = tile_m / extent;

        for feature in tile.features(meta) {
            let group = match fixed {
                Some(g) => g,
                None => match kind_of(feature).and_then(land_group) {
                    Some(g) => g,
                    None => continue,
                },
            };
            let slot = match builders.iter().position(|(g, _, _)| *g == group) {
                Some(i) => i,
                None => {
                    builders.push((group, Vec::new(), Vec::new()));
                    builders.len() - 1
                }
            };
            let (_, positions, indices) = &mut builders[slot];
            match &feature.geometry {
                Geometry::Polygon(p) => tessellate_polygon(p, scale, extent, positions, indices),
                Geometry::MultiPolygon(mp) => {
                    for p in mp {
                        tessellate_polygon(p, scale, extent, positions, indices);
                    }
                }
                _ => {}
            }
        }
    }

    Ok(builders
        .into_iter()
        .filter(|(_, p, i)| !p.is_empty() && !i.is_empty())
        .map(|(group, positions, indices)| SurfaceMesh {
            group,
            positions,
            indices,
            uvs: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
        })
        .collect())
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
pub fn tessellate_bridges(tile: &TileData, tile_m: f32) -> Result<Vec<BridgeDeck>, String> {
    let Some(meta) = tile.layer("bridges") else {
        return Ok(Vec::new());
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
    Ok(decks)
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

/// Vertical spacing, in metres, between successive road features within one tile.
///
/// Ribbons **overlap by construction** — a junction stub crosses whatever it tees
/// into, Shortbread's merged ways cross each other freely, and a flyover is drawn
/// on the same ground plane as the road it flies over. Two coplanar overlapping
/// triangles have depth values that agree only to the last few bits, so which one
/// wins flips per pixel as the camera moves: the flicker along a junction.
/// `depth_bias` cannot settle it, being a per-material constant while the fight is
/// between two features sharing a material.
///
/// Stepping each feature up by a fraction of a millimetre makes the winner
/// deterministic. The step has to clear the depth buffer's resolution at ground
/// range (~12 µm at 100 m, ~120 µm at 1 km) without leaving the 10 mm window
/// `SurfaceGroup::y_offset` documents — at [`ROAD_LAYERS`] rungs the ladder is
/// under 1.5 mm tall, so roads stay below the buildings' base at y = 0.
const ROAD_LAYER_STEP: f32 = 6.0e-5;

/// How many rungs the [`ROAD_LAYER_STEP`] ladder has. The ladder is *ordered*, not
/// arbitrary (see [`road_rung`]), so which of two overlapping features wins is a
/// property of the roads rather than of the order the tile happened to store them
/// in — and the same two roads resolve the same way in the neighbouring tile.
const ROAD_LAYERS: u32 = 20;

/// Rungs reserved at the top of the ladder for bridge decks.
const BRIDGE_LAYERS: u32 = 4;

/// Tie-breaking rungs within one width band.
const ROAD_TIE_LAYERS: u32 = 4;

/// Which rung of the [`ROAD_LAYER_STEP`] ladder a feature draws on.
///
/// Two rules, in order:
///
/// * A **bridge** takes the reserved top of the ladder. A flyover and the road
///   under it are both flat sheets on the same ground plane here, so the only
///   thing that can say which is on top is this; without it an interchange
///   resolves per-pixel and the deck and the road beneath interleave.
/// * Otherwise the **wider road wins**, which is the same rule
///   [`SurfaceGroup::depth_rung`] applies between classes, continued inside one:
///   the through road keeps an unbroken surface and the side road disappears
///   under it, rather than slicing its kerb lines across the junction.
///
/// Features that tie on both fall back to their index in the tile, which is only a
/// deterministic shuffle — but two roads of the same width that also overlap are
/// two halves of the same junction, where either winner reads the same.
fn road_rung(width_m: f32, bridge: bool, feature_index: usize) -> u32 {
    let tie = feature_index as u32 % ROAD_TIE_LAYERS;
    if bridge {
        return ROAD_LAYERS - BRIDGE_LAYERS + tie;
    }
    // 0: paths and service roads, 1: streets, 2: main roads, 3: motorways.
    let band = ((width_m / 6.0) as u32).min((ROAD_LAYERS - BRIDGE_LAYERS) / ROAD_TIE_LAYERS - 1);
    band * ROAD_TIE_LAYERS + tie
}

/// Miter limit: how far a join may push the **outer** corner out relative to the
/// half-width before it is bevelled off instead. Unbounded miters spike to
/// infinity at a hairpin, which OSM has plenty of (motorway sliproads,
/// switchbacks).
///
/// Past this the join emits two outer vertices and a fill triangle rather than a
/// single stretched one — see [`ribbon`] on why clamping the miter *in place* is
/// not the same thing.
const MITER_LIMIT: f32 = 2.5;

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
/// `decks` are the tile's [`BridgeDeck`]s. Where one of them carries a
/// bridge-flagged street, that street's ribbon is **left out**: the deck is the
/// road surface up there, and drawing both would put a carriageway at ground level
/// under a structure standing several metres above it. Only `bridge` features are
/// dropped, so the road *under* a viaduct keeps every metre of its own ribbon.
pub fn tessellate_roads(
    tile: &TileData,
    tile_m: f32,
    decks: &[BridgeDeck],
) -> Result<Vec<SurfaceMesh>, String> {
    let Some(meta) = tile.layer("streets") else {
        return Ok(Vec::new());
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    let nodes = street_nodes(&features);

    let mut builders: Vec<(RoadClass, Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<u32>)> = Vec::new();

    for (fi, feature) in features.iter().enumerate() {
        let Some((class, half)) = drawn_street(feature) else {
            continue;
        };
        let bridge = flag_of(feature, "bridge");
        // This feature's rung on the anti-z-fight ladder (see `ROAD_LAYER_STEP`).
        let y = road_rung(half * 2.0, bridge, fi) as f32 * ROAD_LAYER_STEP;

        let slot = match builders.iter().position(|(c, _, _, _)| *c == class) {
            Some(i) => i,
            None => {
                builders.push((class, Vec::new(), Vec::new(), Vec::new()));
                builders.len() - 1
            }
        };
        let (_, positions, uvs, indices) = &mut builders[slot];

        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            // The `streets` layer is lines; a stray polygon belongs to
            // `street_polygons`, which the raster path fills.
            _ => continue,
        }

        for ls in lines {
            let pts: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x, c.y])
                    .collect();
            let (Some(head), Some(tail)) = (pts.first().copied(), pts.last().copied()) else {
                continue;
            };
            for run in clip_polyline(&pts, 0.0, extent) {
                // Only the run holding an *original* end of the line can be at a
                // junction; the rest begin and end at the tile border, where the
                // neighbour's copy of the same street abuts this one exactly and
                // an extension would draw the same tarmac twice.
                let ext_head = (run.first() == Some(&head))
                    .then(|| junction_extent(&nodes, head, fi))
                    .unwrap_or(0.0);
                let ext_tail = (run.last() == Some(&tail))
                    .then(|| junction_extent(&nodes, tail, fi))
                    .unwrap_or(0.0);
                // Tile units → metres, once, before any geometry is built.
                let metres: Vec<[f32; 2]> =
                    run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                // A bridge's own span belongs to the deck that carries it.
                let parts = if bridge && !decks.is_empty() {
                    off_deck_runs(&metres, decks)
                } else {
                    vec![metres]
                };
                // A run that lost part of itself to a deck now ends *at* the deck,
                // which is not a junction — extending there would push tarmac out
                // from under the structure.
                let whole = parts.len() == 1 && parts[0].len() == run.len();
                for part in parts {
                    let extend = if whole {
                        [ext_head, ext_tail]
                    } else {
                        [0.0, 0.0]
                    };
                    ribbon(
                        &part,
                        half,
                        y,
                        class.texture_length_m(),
                        extend,
                        positions,
                        uvs,
                        indices,
                    );
                }
            }
        }
    }

    Ok(builders
        .into_iter()
        .filter(|(_, p, _, i)| !p.is_empty() && !i.is_empty())
        .map(|(class, positions, uvs, indices)| SurfaceMesh {
            group: SurfaceGroup::Road(class),
            positions,
            uvs,
            indices,
            normals: Vec::new(),
            colors: Vec::new(),
        })
        .collect())
}

/// Split a centre line into the stretches that are **not** on any deck.
///
/// Per vertex rather than per feature: Shortbread merges ways by attribute, so one
/// `bridge` feature can be a chain of spans with ordinary road between them, and
/// dropping the whole feature would erase the road as well as the bridge. The cut
/// lands on the vertex either side of the structure, so the ribbon stops within a
/// segment of the deck edge and the deck takes over from there.
fn off_deck_runs(points: &[[f32; 2]], decks: &[BridgeDeck]) -> Vec<Vec<[f32; 2]>> {
    let mut runs: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    for p in points {
        if decks.iter().any(|d| d.contains(*p)) {
            if cur.len() >= 2 {
                runs.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        } else {
            cur.push(*p);
        }
    }
    if cur.len() >= 2 {
        runs.push(cur);
    }
    runs
}

/// Read a boolean Shortbread property (`bridge`, `tunnel`, `oneway`, `link`),
/// treating a missing or non-boolean value as `false`.
fn flag_of(feature: &Feature, name: &str) -> bool {
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
    if flag_of(feature, "tunnel") {
        return None;
    }
    let kind = kind_of(feature).unwrap_or("");
    let flags = StreetFlags {
        oneway: flag_of(feature, "oneway") || flag_of(feature, "oneway_reverse"),
        link: flag_of(feature, "link"),
    };
    Some((road_class(kind, flags)?, road_width_m(kind, flags) * 0.5))
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
///
/// Keys are tile units; the half-widths are metres, which is what the ribbon
/// builder works in.
fn street_nodes(features: &[Feature]) -> HashMap<(i32, i32), Vec<(usize, f32)>> {
    let mut nodes: HashMap<(i32, i32), Vec<(usize, f32)>> = HashMap::new();
    for (fi, feature) in features.iter().enumerate() {
        let Some((_, half)) = drawn_street(feature) else {
            continue;
        };
        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            _ => continue,
        }
        for c in lines.iter().flat_map(|ls| ls.0.iter()) {
            if !c.x.is_finite() || !c.y.is_finite() {
                continue;
            }
            let at = nodes.entry(node_key([c.x, c.y])).or_default();
            // One entry per feature: a way that doubles back through its own node
            // is not a junction with itself.
            if !at.iter().any(|(o, _)| *o == fi) {
                at.push((fi, half));
            }
        }
    }
    nodes
}

fn node_key(p: [f32; 2]) -> (i32, i32) {
    (p[0].round() as i32, p[1].round() as i32)
}

/// How far past `end` the feature's ribbon has to run to fill the junction there,
/// in metres: the half-width of the **widest other road** meeting at that node, so
/// the stub reaches its far kerb and stops. `0.0` where nothing else meets — a
/// dead end keeps a square end on the ground instead of a flap of tarmac reaching
/// into the field beyond it.
fn junction_extent(
    nodes: &HashMap<(i32, i32), Vec<(usize, f32)>>,
    end: [f32; 2],
    feature_index: usize,
) -> f32 {
    nodes
        .get(&node_key(end))
        .map(|at| {
            at.iter()
                .filter(|(fi, _)| *fi != feature_index)
                .map(|(_, half)| *half)
                .fold(0.0, f32::max)
        })
        .unwrap_or(0.0)
        .min(JUNCTION_EXTEND_MAX)
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
/// `y` is the feature's rung on the [`ROAD_LAYER_STEP`] ladder — sub-millimetre,
/// and the only thing that keeps two overlapping ribbons from flickering against
/// each other at a junction.
///
/// `extend` is `[head, tail]` metres of stub past each end, from
/// [`junction_extent`]: how far this road has to run *into* whatever it meets
/// there. Zero at a dead end and at a tile border.
fn ribbon(
    points: &[[f32; 2]],
    half: f32,
    y: f32,
    tex_len_m: f32,
    extend: [f32; 2],
    positions: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
) {
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
        return;
    }

    // Stubs past each end, to fill the notch at a junction.
    let dir = |a: [f32; 2], b: [f32; 2]| -> [f32; 2] {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy).max(1e-6);
        [dx / len, dy / len]
    };
    if extend[0] > 0.0 {
        let head = dir(pts[1], pts[0]);
        let first = pts[0];
        pts.insert(
            0,
            [
                first[0] + head[0] * extend[0],
                first[1] + head[1] * extend[0],
            ],
        );
    }
    if extend[1] > 0.0 {
        let tail = dir(pts[pts.len() - 2], pts[pts.len() - 1]);
        let last = pts[pts.len() - 1];
        pts.push([last[0] + tail[0] * extend[1], last[1] + tail[1] * extend[1]]);
    }

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
        let d_in = if i > 0 {
            dir(pts[i - 1], pts[i])
        } else {
            dir(pts[0], pts[1])
        };
        let d_out = if i + 1 < pts.len() {
            dir(pts[i], pts[i + 1])
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
        let neighbour = match (i > 0, i + 1 < pts.len()) {
            (true, true) => seg_len(i - 1).min(seg_len(i)),
            (true, false) => seg_len(i - 1),
            (false, true) => seg_len(i),
            (false, false) => 0.0,
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
fn clip_polyline(points: &[[f32; 2]], lo: f32, hi: f32) -> Vec<Vec<[f32; 2]>> {
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

/// A tile's distance-to-shore field: `SHORE_FIELD_PX²` bytes, 0 at the bank and
/// 255 at [`SHORE_MAX_DIST_M`] or beyond, in the same tile-local frame as
/// [`tessellate_surfaces`]' output (origin north-west, +x east, +y south).
#[derive(Debug, Clone)]
pub struct ShoreField {
    pub size: u32,
    pub data: Vec<u8>,
}

/// Rasterise the tile's water polygons and turn them into a distance-to-shore
/// field, or `None` when the tile holds no water at all.
///
/// This is the piece of information the geometry alone cannot give the water
/// shader. Earcut emits nothing but the polygon's own boundary vertices, so
/// every triangle corner sits *on* the shore and a per-vertex distance would be
/// identically zero; the answer has to come from the interior of the shape,
/// which means rasterising it. With it, the shader can shelter a canal from the
/// swell, colour the shallows and lap a foam line against the bank — the three
/// things that separate "a blue surface with waves on it" from water in a place.
///
/// Distances are measured only against land **inside this tile**. That is not an
/// approximation of the truth, it *is* the truth for the interior: in MVT, land
/// is the absence of a water polygon. It only overestimates within a tile's
/// width of the border, where the nearest bank may sit in the neighbouring tile
/// — which is why nothing is padded around the raster. Treating the border as
/// land instead (the obvious padding) would draw a shoreline straight down the
/// middle of every river that crosses a tile edge.
pub fn water_shore_field(tile: &TileData, tile_m: f32) -> Result<Option<ShoreField>, String> {
    let size = SHORE_FIELD_PX;
    let mut pixmap = Pixmap::new(size, size).ok_or_else(|| "pixmap alloc failed".to_string())?;
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
        let transform = Transform::from_scale(size as f32 / extent, size as f32 / extent);
        for feature in tile.features(meta) {
            if area_path(&feature.geometry).is_some() {
                any_water = true;
            }
            fill_geometry(&mut pixmap, &paint, &feature.geometry, &transform);
        }
    }
    if !any_water {
        return Ok(None);
    }

    // Seed the transform from every *land* texel, then measure each water texel's
    // distance to the nearest one.
    // "Unreached" is a large *finite* distance, not infinity: the envelope below
    // subtracts one sample's value from another's, and ∞ − ∞ is a NaN that
    // quietly poisons a whole scanline. Two texel-grids is past any real answer
    // (nothing inside the field can be further than `size` from a seed) while
    // staying exactly representable in f32.
    let unreached = (2 * size) as f32;
    let n = (size * size) as usize;
    let mut sq = vec![0.0f32; n];
    for (d, px) in sq.iter_mut().zip(pixmap.pixels()) {
        *d = if px.alpha() > 127 { unreached } else { 0.0 };
    }
    squared_distance_transform(&mut sq, size as usize);

    let m_per_texel = tile_m / size as f32;
    let data = sq
        .iter()
        .map(|d2| {
            let m = d2.sqrt() * m_per_texel;
            (m / SHORE_MAX_DIST_M).clamp(0.0, 1.0).mul_add(255.0, 0.5) as u8
        })
        .collect();
    Ok(Some(ShoreField { size, data }))
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
        super::tessellate_surfaces(&TileData::parse(bytes)?, tile_m)
    }

    pub fn tessellate_roads(
        bytes: &[u8],
        tile_m: f32,
        decks: &[BridgeDeck],
    ) -> Result<Vec<SurfaceMesh>, String> {
        super::tessellate_roads(&TileData::parse(bytes)?, tile_m, decks)
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
    use super::bytes::{
        extract_street_labels, ocean_mask, render_mvt, tessellate_bridges, tessellate_roads,
        tessellate_surfaces, water_shore_field,
    };
    use super::*;

    /// A real z14 tile over Groningen, from the VersaTiles Shortbread source
    /// (now self-hosted at tiles.cartopolis.org; the archive is identical).
    const GRONINGEN_Z14: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/groningen_z14.mvt"
    ));

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
            &super::tessellate_surfaces(&tile, tile_m).unwrap(),
            &tessellate_surfaces(GRONINGEN_Z14, tile_m).unwrap(),
        );
        same(
            &super::tessellate_roads(&tile, tile_m, &[]).unwrap(),
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
        for (name, body) in [
            (
                "10/526/334",
                &include_bytes!("../tests/fixtures/lowzoom_10_526_334.mvt")[..],
            ),
            (
                "10/534/336",
                &include_bytes!("../tests/fixtures/lowzoom_10_534_336.mvt")[..],
            ),
        ] {
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
            let rgba = render_mvt_sized(&tile, SubTile::WHOLE, size).expect("render");
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
            let culled = render_mvt_culled(&tile, sub, TILE_PX, true).expect("culled render");
            let full = render_mvt_culled(&tile, sub, TILE_PX, false).expect("unculled render");
            assert_eq!(
                culled.len(),
                full.len(),
                "factor {factor} ({x},{y}): raster size changed"
            );
            let differing = culled
                .iter()
                .zip(full.iter())
                .filter(|(a, b)| a != b)
                .count();
            assert_eq!(
                differing, 0,
                "factor {factor} ({x},{y}): culling changed {differing} bytes"
            );
        }
    }

    /// Not a test — a measurement hatch for the overzoom cull, since its whole
    /// justification is a number. `cargo test -p cartopolis_geo bench_overzoom_cull
    /// -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_overzoom_cull() {
        let tile = TileData::parse(GRONINGEN_Z14).expect("parse fixture");
        for factor in [2u32, 4, 8] {
            let subs: Vec<SubTile> = (0..factor)
                .flat_map(|x| (0..factor).map(move |y| SubTile { factor, x, y }))
                .collect();
            for (label, cull) in [("cull ", true), ("full ", false)] {
                let t0 = std::time::Instant::now();
                for &sub in &subs {
                    render_mvt_culled(&tile, sub, TILE_PX, cull).expect("render");
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

    /// Not a test — an eyeball hatch. `cargo test -p cartopolis_geo
    /// dump_png -- --ignored --nocapture` writes the fixture at z14 and at an
    /// overzoomed z17 so the styling can actually be looked at.
    #[test]
    #[ignore]
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
    #[ignore]
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
    #[ignore]
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

    #[test]
    fn overlapping_road_features_are_separated_in_depth() {
        // Ribbons overlap at every junction and tile seam; equal heights there
        // z-fight, which is the flicker along a joint. Successive features must
        // land on different rungs, and the whole ladder must stay inside the
        // millimetre window `SurfaceGroup::y_offset` leaves under the buildings.
        let meshes = tessellate_roads(GRONINGEN_Z14, 1000.0, &[]).expect("tessellate");
        let heights: std::collections::BTreeSet<u32> = meshes
            .iter()
            .flat_map(|m| {
                m.positions
                    .iter()
                    .map(|p| (p[1] / ROAD_LAYER_STEP).round() as u32)
            })
            .collect();
        assert!(
            heights.len() > 1,
            "every road at one height — overlaps would z-fight"
        );
        let top = ROAD_LAYERS as f32 * ROAD_LAYER_STEP;
        assert!(
            SurfaceGroup::Road(RoadClass::Minor).y_offset() + top < 0.0,
            "the ladder pushes roads through the buildings' base"
        );
    }

    /// Eyeball hatch for junction geometry: rasterise the tessellated ribbons
    /// top-down and colour each pixel by how many ribbons cover it, so stubs,
    /// gaps and overlaps are visible without a GPU.
    ///
    /// `cargo test -p cartopolis_geo dump_road_ribbons -- --ignored
    /// --nocapture`; `CARTO_DUMP_WIN="x,z,size"` (tile metres) zooms to a junction.
    #[test]
    #[ignore]
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

        // Decks first, exactly as the streamer does it: a bridge's own span is
        // drawn by the deck, so the ribbons this dumps are the ones that really
        // reach the ground.
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

    /// A bridge's own span is drawn by the deck; the road *under* it keeps every
    /// metre of its ribbon. Getting that backwards would either double the
    /// carriageway or erase the road beneath a flyover.
    #[test]
    fn a_deck_takes_over_from_the_street_it_carries() {
        let decks = tessellate_bridges(GRONINGEN_Z14, 1000.0).expect("bridges");
        let bare = tessellate_roads(GRONINGEN_Z14, 1000.0, &[]).expect("roads");
        let with_decks = tessellate_roads(GRONINGEN_Z14, 1000.0, &decks).expect("roads");
        let count = |ms: &[SurfaceMesh]| ms.iter().map(|m| m.indices.len()).sum::<usize>();
        assert!(
            count(&with_decks) < count(&bare),
            "the decks suppressed no ribbon at all"
        );
        // …but only a little of it: the tile's bridges are a handful of spans, not
        // a fifth of its streets. A big drop here means ordinary roads are being
        // eaten by a deck they merely pass under.
        let dropped = 1.0 - count(&with_decks) as f32 / count(&bare) as f32;
        assert!(
            dropped < 0.1,
            "decks removed {:.0}% of the road mesh",
            dropped * 100.0
        );
    }

    /// Which of two overlapping ribbons wins has to be a property of the *roads*,
    /// not of the order the tile happened to store them in — otherwise the same
    /// junction resolves one way in this tile and the other way in its neighbour.
    #[test]
    fn the_wider_road_and_the_bridge_win_the_overlap() {
        let street = road_rung(7.0, false, 0);
        let main = road_rung(14.0, false, 0);
        let motorway = road_rung(22.0, false, 0);
        assert!(
            street < main && main < motorway,
            "the wider road must be on top"
        );
        // A flyover and the road under it are both flat sheets on this plane; the
        // deck is the only one that can be on top, whatever it flies over.
        assert!(
            road_rung(4.0, true, 0) > motorway,
            "a bridge deck must be on top"
        );
        // …and the whole ladder has to stay inside the millimetre window under the
        // buildings' base that `SurfaceGroup::y_offset` documents.
        for width in [1.5f32, 4.0, 7.0, 12.0, 22.0] {
            for bridge in [false, true] {
                for fi in 0..40 {
                    let rung = road_rung(width, bridge, fi);
                    assert!(rung < ROAD_LAYERS, "rung {rung} is off the ladder");
                }
            }
        }
        assert!(
            SurfaceGroup::Road(RoadClass::Minor).y_offset() + ROAD_LAYERS as f32 * ROAD_LAYER_STEP
                < 0.0,
            "the ladder pushes roads through the buildings' base"
        );
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
            // Tile-local metres, inside the tile (clamped) and flat on the ground.
            for p in &m.positions {
                assert!(
                    (0.0..=tile_m).contains(&p[0]),
                    "{:?}: x {} out of tile",
                    m.group,
                    p[0]
                );
                assert!(
                    (0.0..=tile_m).contains(&p[2]),
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

    #[test]
    fn surfaces_sit_between_the_map_tile_and_the_buildings() {
        // `map_stream` spawns the finest tier as a 0.04 m cuboid centred at -0.03,
        // so its top face is at -0.01; buildings stand on y = 0. A surface outside
        // that window either sinks under the map or cuts into the buildings — the
        // latter is what made grass bleed through the grey.
        const TILE_TOP: f32 = -0.01;
        const BUILDING_BASE: f32 = 0.0;
        for group in [
            SurfaceGroup::Water,
            SurfaceGroup::Forest,
            SurfaceGroup::Grass,
            SurfaceGroup::Sand,
            SurfaceGroup::Built,
        ] {
            let y = group.y_offset();
            assert!(
                y > TILE_TOP && y < BUILDING_BASE,
                "{group:?} at y={y} escapes the ground window"
            );
            assert!(
                group.depth_bias() > 0.0,
                "{group:?} must outrank the map tile"
            );
        }
        // Water draws over land, and its bias must agree with its height.
        assert!(SurfaceGroup::Water.y_offset() > SurfaceGroup::Grass.y_offset());
        assert!(SurfaceGroup::Water.depth_bias() > SurfaceGroup::Grass.depth_bias());

        // Water gets the widest gap in the stack, because dark-on-pale is the
        // fight that shows when it is lost. Whoever next rebalances this window
        // has to take the room from somewhere other than under the water.
        let water_gap = SurfaceGroup::Water.y_offset() - SurfaceGroup::Forest.y_offset();
        let land_gap = SurfaceGroup::Forest.y_offset() - SurfaceGroup::Grass.y_offset();
        assert!(
            water_gap >= 3.0 * land_gap,
            "water sits {water_gap} m above the land sheets but they are {land_gap} m apart; \
             the contrasty fight should get the widest gap"
        );
    }

    #[test]
    fn depth_bias_rungs_clear_the_rounding_noise() {
        // A bias is denominated in depth-buffer ULPs, and the interpolated depth
        // carries a few ULPs of per-frame rounding noise. Neighbouring rungs one
        // unit apart therefore cannot hold on their own, and the millimetre
        // `y_offset`s that used to cover for them decay as 1/z — which is why the
        // water surface flickered against the land when the camera changed
        // altitude. Keep real distance between adjacent rungs.
        const MIN_SEPARATION: f32 = 8.0;
        let mut ladder: Vec<f32> = [
            SurfaceGroup::Built,
            SurfaceGroup::Grass,
            SurfaceGroup::Forest,
            SurfaceGroup::Sand,
            SurfaceGroup::Water,
            SurfaceGroup::Road(RoadClass::Path),
            SurfaceGroup::Road(RoadClass::Minor),
            SurfaceGroup::Road(RoadClass::Major),
            SurfaceGroup::Road(RoadClass::Motorway),
            SurfaceGroup::Road(RoadClass::Rail),
            SurfaceGroup::Canopy,
        ]
        .into_iter()
        .map(SurfaceGroup::depth_bias)
        .collect();
        ladder.sort_by(f32::total_cmp);
        ladder.dedup();
        for pair in ladder.windows(2) {
            assert!(
                pair[1] - pair[0] >= MIN_SEPARATION,
                "rungs {} and {} are only {} depth units apart",
                pair[0],
                pair[1],
                pair[1] - pair[0]
            );
        }

        // A road crosses a canal on a bridge, never the other way round, and the
        // canopy stands above the whole sheet stack.
        assert!(
            SurfaceGroup::Road(RoadClass::Path).depth_bias() > SurfaceGroup::Water.depth_bias()
        );
        assert!(
            SurfaceGroup::Canopy.depth_bias() > SurfaceGroup::Road(RoadClass::Rail).depth_bias()
        );
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
        for m in &meshes {
            assert!(matches!(m.group, SurfaceGroup::Road(_)), "{:?}", m.group);
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
            // Ground geometry: flat to within the anti-z-fight ladder, and inside
            // the tile plus the junction stubs.
            let ladder = ROAD_LAYERS as f32 * ROAD_LAYER_STEP;
            assert!(m.positions.iter().all(|p| p[1] >= 0.0 && p[1] < ladder));
            assert!(m
                .positions
                .iter()
                .all(|p| p[0] > -40.0 && p[0] < 1040.0 && p[2] > -40.0 && p[2] < 1040.0));
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
            [0.0, 5.0],
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
            [0.0, 0.0],
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
            centre,
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
