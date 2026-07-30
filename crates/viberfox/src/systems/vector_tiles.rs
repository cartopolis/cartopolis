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

use geo_types::Geometry;
use mvt_reader::feature::{Feature, Value};
use mvt_reader::Reader;
use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Shader, Stroke, Transform,
};

use crate::systems::road_texture::{road_class, road_width_m, RoadClass};

/// Edge length in pixels of a rendered tile. Larger than the 256 px raster tiles
/// we used to fetch: the texture is stretched across a whole region on the ground,
/// and re-rendering costs us CPU rather than bandwidth, so we may as well take the
/// resolution.
pub const TILE_PX: u32 = 512;

/// Highest zoom the Shortbread source actually contains. Anything above this is
/// rendered by magnifying the z14 ancestor.
pub const MAX_SOURCE_ZOOM: u32 = 14;

/// Depth-buffer units between adjacent rungs of the coplanar ground stack, and of
/// everything else drawn flat on it (the route line, the driver arrow).
///
/// Every bias in the scene is a small integer rung times this, so scaling it moves
/// the whole stack together and cannot reorder it. It exists because the rungs used
/// to *be* the bias: neighbours one unit apart, which is one ULP of the depth
/// buffer and below the per-frame rounding noise. See [`SurfaceGroup::depth_bias`]
/// for why the millimetre `y_offset`s stop covering for that as you climb.
pub const DEPTH_BIAS_STEP: f32 = 16.0;

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
    pub const WATER: u32 = 0xA8_C4_DA;
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
    pub const PATH: u32 = 0xC5_C0_B0;
    pub const BOUNDARY: u32 = 0xB0_A6_B4;
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
pub fn render_mvt(bytes: &[u8], sub: SubTile) -> Result<Vec<u8>, String> {
    let reader = Reader::new(bytes.to_vec()).map_err(|e| format!("mvt parse: {e:?}"))?;
    let layers = reader
        .get_layer_metadata()
        .map_err(|e| format!("mvt layers: {e:?}"))?;

    let mut pixmap =
        Pixmap::new(TILE_PX, TILE_PX).ok_or_else(|| "pixmap alloc failed".to_string())?;
    pixmap.fill(tiny_skia::Color::from_rgba8(
        ((palette::PAPER >> 16) & 0xFF) as u8,
        ((palette::PAPER >> 8) & 0xFF) as u8,
        (palette::PAPER & 0xFF) as u8,
        255,
    ));

    // Tile-extent units → pixels, including the overzoom magnification and the
    // shift that brings the requested sub-tile to the origin.
    let f = sub.factor as f32;
    let px = TILE_PX as f32;

    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };

    for (layer_name, draw) in PAINT_ORDER {
        let Some(meta) = layers.iter().find(|l| l.name == *layer_name) else {
            continue;
        };
        let Ok(features) = reader.get_features(meta.layer_index) else {
            continue;
        };
        if features.is_empty() {
            continue;
        }
        let extent = if meta.extent == 0 { 4096 } else { meta.extent } as f32;
        let scale = px * f / extent;
        let transform = Transform::from_row(
            scale,
            0.0,
            0.0,
            scale,
            -(sub.x as f32) * px,
            -(sub.y as f32) * px,
        );

        for feature in &features {
            let kind = kind_of(feature);
            match draw {
                Draw::Fill(rgb) => {
                    paint.shader = shader(*rgb);
                    fill_geometry(&mut pixmap, &paint, &feature.geometry, &transform);
                }
                Draw::FillByKind { default, kinds } => {
                    let rgb = kind
                        .and_then(|k| kinds.iter().find(|(n, _)| *n == k).map(|(_, c)| *c))
                        .unwrap_or(*default);
                    paint.shader = shader(rgb);
                    fill_geometry(&mut pixmap, &paint, &feature.geometry, &transform);
                }
                Draw::Line { color, width } => {
                    paint.shader = shader(*color);
                    stroke_geometry(&mut pixmap, &paint, &feature.geometry, &transform, width * f);
                }
                Draw::Outline { color, width } => {
                    paint.shader = shader(*color);
                    stroke_geometry(&mut pixmap, &paint, &feature.geometry, &transform, width * f);
                }
                Draw::Streets { casing } => {
                    let (w, fill, case) = street_style(kind.unwrap_or(""));
                    let (color, width) = if *casing {
                        (case, (w + 1.2) * f)
                    } else {
                        (fill, w * f)
                    };
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
    /// [`road_texture`](crate::systems::road_texture).
    Road(RoadClass),
    /// Merged tree canopies (`systems::vegetation`). Per-instance tone and luma
    /// variation ride in the mesh's vertex colours, so this stays **one** group,
    /// one material and one mesh per cell like every other group here.
    Canopy,
    /// Merged tree trunks.
    Trunk,
}

impl SurfaceGroup {
    /// Base colour for the group's material. Lit by the scene's own sun, unlike the
    /// raster path's baked-in shading — this is the point of ADR-035 option B.
    #[must_use]
    pub fn base_color(self) -> [f32; 3] {
        let rgb = match self {
            Self::Water => palette::WATER,
            Self::Forest => palette::FOREST,
            Self::Grass => palette::GRASS,
            Self::Sand => palette::SAND,
            Self::Built => palette::RESIDENTIAL,
            // Roads carry their surface in a texture; tinting it would only fight
            // the paint values chosen there, so the material multiplies by white.
            Self::Road(_) => 0xFF_FF_FF,
            // Vertex colours multiply this, and they are centred on 1.0 — so the
            // palette entry stays the one place the canopy tone is written down.
            Self::Canopy => palette::CANOPY,
            Self::Trunk => palette::TRUNK,
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
    #[must_use]
    pub fn y_offset(self) -> f32 {
        match self {
            Self::Built => -0.008,
            Self::Grass => -0.007,
            Self::Forest | Self::Sand => -0.006,
            Self::Water => -0.004,
            // Roads go on top of every natural surface — a street crosses a park,
            // never the other way round.
            Self::Road(_) => -0.003,
            // Trees are real 3D geometry standing *on* the ground, not another
            // sheet in the coplanar stack, so they take the ground plane itself.
            Self::Canopy | Self::Trunk => 0.0,
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
            Self::Road(RoadClass::Motorway) => 8.0,
            // Rails sit above their own crossings, so a level crossing shows the
            // track rather than the tarmac.
            Self::Road(RoadClass::Rail) => 9.0,
            // Above the whole coplanar ground stack. Trees resolve against it by
            // honest depth — they stand a metre or ten above the plane — but the
            // ground sheets are biased *forward* to settle their own z-fight, and
            // a grass sheet at 2.0 would otherwise punch through a trunk base.
            Self::Canopy | Self::Trunk => 10.0,
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
        "grass" | "meadow" | "park" | "garden" | "farmland" | "orchard" | "scrub"
        | "heath" | "allotments" | "village_green" | "recreation_ground" => SurfaceGroup::Grass,
        "sand" | "beach" | "bare_rock" | "scree" => SurfaceGroup::Sand,
        "residential" | "industrial" | "commercial" | "retail" | "quarry" => SurfaceGroup::Built,
        _ => return None,
    })
}

/// Open an MVT body. Shared with `systems::vegetation`, which reads the same
/// `land` layer for a different purpose and should not re-derive the error text.
pub(crate) fn mvt_reader(bytes: &[u8]) -> Result<Reader, String> {
    Reader::new(bytes.to_vec()).map_err(|e| format!("mvt parse: {e:?}"))
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
    /// [`RoadClass::texture_length_m`](crate::systems::road_texture::RoadClass::texture_length_m).
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
pub fn tessellate_surfaces(bytes: &[u8], tile_m: f32) -> Result<Vec<SurfaceMesh>, String> {
    let reader = Reader::new(bytes.to_vec()).map_err(|e| format!("mvt parse: {e:?}"))?;
    let layers = reader
        .get_layer_metadata()
        .map_err(|e| format!("mvt layers: {e:?}"))?;

    let mut builders: Vec<(SurfaceGroup, Vec<[f32; 3]>, Vec<u32>)> = Vec::new();

    for meta in &layers {
        // (layer, fixed group) — `land` resolves its group per feature instead.
        let fixed = match meta.name.as_str() {
            "ocean" | "water_polygons" => Some(SurfaceGroup::Water),
            "sites" => Some(SurfaceGroup::Built),
            "land" => None,
            _ => continue,
        };
        let Ok(features) = reader.get_features(meta.layer_index) else {
            continue;
        };
        let extent = if meta.extent == 0 { 4096 } else { meta.extent } as f32;
        let scale = tile_m / extent;

        for feature in &features {
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

/// Fraction of its own half-width that a ribbon is extended past each end of its
/// centre line. Streets arrive as separate features that merely *share an
/// endpoint*, so without this every junction is a notch of missing tarmac.
///
/// A full half-width (`1.0`) is what a square cap needs to fill a right-angled
/// corner exactly: the stub reaches the far edge of the road it tees into. The
/// earlier `0.5` left the outer half of the corner bare, which is the little dark
/// notch on the outside of a path junction.
const JUNCTION_EXTEND: f32 = 1.0;

/// Vertical spacing, in metres, between successive road features within one tile.
///
/// Ribbons **overlap by construction** — [`JUNCTION_EXTEND`] pushes a stub across
/// whatever it tees into, a clipped run's stub crosses the tile seam into the
/// neighbour's copy of the same street, and Shortbread's merged ways cross each
/// other freely. Two coplanar overlapping triangles have depth values that agree
/// only to the last few bits, so which one wins flips per pixel as the camera
/// moves: the flicker along a junction. `depth_bias` cannot settle it, being a
/// per-material constant while the fight is between two features sharing a
/// material.
///
/// Stepping each feature up by a fraction of a millimetre makes the winner
/// deterministic. The step has to clear the depth buffer's resolution at ground
/// range (~12 µm at 100 m, ~120 µm at 1 km) without leaving the 10 mm window
/// `SurfaceGroup::y_offset` documents — at [`ROAD_LAYERS`] rungs the ladder is
/// under 1 mm tall, so roads stay below the buildings' base at y = 0.
const ROAD_LAYER_STEP: f32 = 6.0e-5;

/// How many rungs the [`ROAD_LAYER_STEP`] ladder has before it wraps. Two features
/// that land on the same rung z-fight again, but they have to be the 16th apart in
/// one tile *and* overlap, which the ordering of a Shortbread `streets` layer
/// (grouped by kind) makes rare.
const ROAD_LAYERS: u32 = 16;

/// Miter limit: how far a join may push a vertex out relative to the half-width,
/// before it is allowed to pinch instead. Unbounded miters spike to infinity at a
/// hairpin, which OSM has plenty of (motorway sliproads, switchbacks).
const MITER_LIMIT: f32 = 2.5;

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
pub fn tessellate_roads(bytes: &[u8], tile_m: f32) -> Result<Vec<SurfaceMesh>, String> {
    let reader = Reader::new(bytes.to_vec()).map_err(|e| format!("mvt parse: {e:?}"))?;
    let layers = reader
        .get_layer_metadata()
        .map_err(|e| format!("mvt layers: {e:?}"))?;

    let Some(meta) = layers.iter().find(|l| l.name == "streets") else {
        return Ok(Vec::new());
    };
    let features = reader
        .get_features(meta.layer_index)
        .map_err(|e| format!("mvt features: {e:?}"))?;
    let extent = if meta.extent == 0 { 4096 } else { meta.extent } as f32;
    let scale = tile_m / extent;

    let mut builders: Vec<(RoadClass, Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<u32>)> = Vec::new();

    for (fi, feature) in features.iter().enumerate() {
        let kind = kind_of(feature).unwrap_or("");
        let Some(class) = road_class(kind) else {
            continue;
        };
        let half = road_width_m(kind) * 0.5;
        // This feature's rung on the anti-z-fight ladder (see `ROAD_LAYER_STEP`).
        let y = (fi as u32 % ROAD_LAYERS) as f32 * ROAD_LAYER_STEP;

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
            let pts: Vec<[f32; 2]> = ls
                .0
                .iter()
                .filter(|c| c.x.is_finite() && c.y.is_finite())
                .map(|c| [c.x, c.y])
                .collect();
            for run in clip_polyline(&pts, 0.0, extent) {
                // Tile units → metres, once, before any geometry is built.
                let metres: Vec<[f32; 2]> =
                    run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                ribbon(
                    &metres,
                    half,
                    y,
                    class.texture_length_m(),
                    positions,
                    uvs,
                    indices,
                );
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

/// Extrude a centre line into a flat ribbon of `2 × half` metres at height `y`,
/// writing positions, UVs and indices into shared buffers.
///
/// Joins are mitered from the average of the two adjacent segment normals, which
/// keeps the carriageway a constant width around a bend — the naive
/// quad-per-segment alternative leaves a wedge of gap on the outside of every
/// corner, and OSM road geometry is nothing but corners.
///
/// `y` is the feature's rung on the [`ROAD_LAYER_STEP`] ladder — sub-millimetre,
/// and the only thing that keeps two overlapping ribbons from flickering against
/// each other at a junction.
fn ribbon(
    points: &[[f32; 2]],
    half: f32,
    y: f32,
    tex_len_m: f32,
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
    let extend = half * JUNCTION_EXTEND;
    let dir = |a: [f32; 2], b: [f32; 2]| -> [f32; 2] {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy).max(1e-6);
        [dx / len, dy / len]
    };
    let head = dir(pts[1], pts[0]);
    let first = pts[0];
    pts.insert(0, [first[0] + head[0] * extend, first[1] + head[1] * extend]);
    let tail = dir(pts[pts.len() - 2], pts[pts.len() - 1]);
    let last = pts[pts.len() - 1];
    pts.push([last[0] + tail[0] * extend, last[1] + tail[1] * extend]);

    let base = positions.len() as u32;
    let mut travelled = 0.0f32;

    for i in 0..pts.len() {
        if i > 0 {
            travelled += (pts[i][0] - pts[i - 1][0]).hypot(pts[i][1] - pts[i - 1][1]);
        }
        let d_in = if i > 0 { dir(pts[i - 1], pts[i]) } else { dir(pts[0], pts[1]) };
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
        // through the join, capped so a hairpin pinches instead of spiking.
        let cos_half = (m[0] * n_out[0] + m[1] * n_out[1]).abs().max(1e-3);
        let ext = half * (1.0 / cos_half).min(MITER_LIMIT);

        let v = travelled / tex_len_m;
        positions.push([pts[i][0] - m[0] * ext, y, pts[i][1] - m[1] * ext]);
        uvs.push([0.0, v]);
        positions.push([pts[i][0] + m[0] * ext, y, pts[i][1] + m[1] * ext]);
        uvs.push([1.0, v]);

        if i > 0 {
            let (a, b) = (base + (i as u32 - 1) * 2, base + i as u32 * 2);
            push_triangle_up(positions, indices, a, b, b + 1);
            push_triangle_up(positions, indices, a, b + 1, a + 1);
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
pub fn ocean_mask(bytes: &[u8], sub: SubTile) -> Result<Vec<u8>, String> {
    let reader = Reader::new(bytes.to_vec()).map_err(|e| format!("mvt parse: {e:?}"))?;
    let layers = reader
        .get_layer_metadata()
        .map_err(|e| format!("mvt layers: {e:?}"))?;

    let mut pixmap =
        Pixmap::new(TILE_PX, TILE_PX).ok_or_else(|| "pixmap alloc failed".to_string())?;
    let paint = Paint {
        anti_alias: true,
        shader: Shader::SolidColor(tiny_skia::Color::WHITE),
        ..Paint::default()
    };

    let f = sub.factor as f32;
    let px = TILE_PX as f32;
    for meta in &layers {
        if !WATER_LAYERS.contains(&meta.name.as_str()) {
            continue;
        }
        let Ok(features) = reader.get_features(meta.layer_index) else {
            continue;
        };
        let extent = if meta.extent == 0 { 4096 } else { meta.extent } as f32;
        let scale = px * f / extent;
        let transform = Transform::from_row(
            scale,
            0.0,
            0.0,
            scale,
            -(sub.x as f32) * px,
            -(sub.y as f32) * px,
        );
        for feature in &features {
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
pub fn water_shore_field(bytes: &[u8], tile_m: f32) -> Result<Option<ShoreField>, String> {
    let reader = Reader::new(bytes.to_vec()).map_err(|e| format!("mvt parse: {e:?}"))?;
    let layers = reader
        .get_layer_metadata()
        .map_err(|e| format!("mvt layers: {e:?}"))?;

    let size = SHORE_FIELD_PX;
    let mut pixmap = Pixmap::new(size, size).ok_or_else(|| "pixmap alloc failed".to_string())?;
    let paint = Paint {
        anti_alias: false,
        shader: Shader::SolidColor(tiny_skia::Color::WHITE),
        ..Paint::default()
    };

    let mut any_water = false;
    for meta in &layers {
        if !WATER_LAYERS.contains(&meta.name.as_str()) {
            continue;
        }
        let Ok(features) = reader.get_features(meta.layer_index) else {
            continue;
        };
        let extent = if meta.extent == 0 { 4096 } else { meta.extent } as f32;
        let transform = Transform::from_scale(size as f32 / extent, size as f32 / extent);
        for feature in &features {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A real z14 tile over Groningen, from the VersaTiles Shortbread source
    /// (now self-hosted at tiles.garage44.eu; the archive is identical).
    const GRONINGEN_Z14: &[u8] = include_bytes!("../../tests/fixtures/groningen_z14.mvt");

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
        assert!(rgba.chunks_exact(4).all(|p| p[3] == 255), "alpha must be opaque");
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

    /// Not a test — an eyeball hatch. `cargo test -p viberfox --bin viberfox
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
        let reader = Reader::new(GRONINGEN_Z14.to_vec()).unwrap();
        let meta = reader.get_layer_metadata().unwrap();
        println!("tile bytes: {}", GRONINGEN_Z14.len());
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
                kv.sort_by_key(|(k, _)| k.clone());
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
                Geometry::LineString(ls) => {
                    (ls.0.len(), 0, ls.0.len().saturating_sub(1) * 2)
                }
                Geometry::MultiLineString(mls) => mls
                    .iter()
                    .map(|ls| count(&Geometry::LineString(ls.clone())))
                    .fold((0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2)),
                _ => (0, 0, 0),
            }
        }

        println!("{:<24} {:>8} {:>9} {:>10}", "layer", "features", "vertices", "triangles");
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
                println!("{:<24} {:>8} {:>9} {:>10}", l.name, l.feature_count, v, tris);
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
                Geometry::LineString(ls) => ls
                    .0
                    .iter()
                    .any(|c| c.x <= 0.0 || c.y <= 0.0 || c.x >= 4096.0 || c.y >= 4096.0),
                Geometry::MultiLineString(mls) => mls.iter().any(|ls| {
                    ls.0.iter()
                        .any(|c| c.x <= 0.0 || c.y <= 0.0 || c.x >= 4096.0 || c.y >= 4096.0)
                }),
                _ => false,
            })
            .count();
        println!(
            "\nstreets with a feature id : {with_id}/{}",
            streets.len()
        );
        println!("streets clipped at a seam : {touches_edge}/{}", streets.len());

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
                        grid[y * size + x], want,
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
            let (cx, cz) = (
                (a[0] + b[0] + c[0]) / 3.0,
                (a[2] + b[2] + c[2]) / 3.0,
            );
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
        let meshes = tessellate_roads(GRONINGEN_Z14, 1000.0).expect("tessellate");
        let heights: std::collections::BTreeSet<u32> = meshes
            .iter()
            .flat_map(|m| m.positions.iter().map(|p| (p[1] / ROAD_LAYER_STEP).round() as u32))
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
        assert_eq!(water.indices.len() % 3, 0, "indices must be whole triangles");

        for m in &meshes {
            let n = m.positions.len() as u32;
            assert!(
                m.indices.iter().all(|&i| i < n),
                "{:?}: index out of range",
                m.group
            );
            // Tile-local metres, inside the tile (clamped) and flat on the ground.
            for p in &m.positions {
                assert!((0.0..=tile_m).contains(&p[0]), "{:?}: x {} out of tile", m.group, p[0]);
                assert!((0.0..=tile_m).contains(&p[2]), "{:?}: z {} out of tile", m.group, p[2]);
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
            assert!(group.depth_bias() > 0.0, "{group:?} must outrank the map tile");
        }
        // Water draws over land, and its bias must agree with its height.
        assert!(SurfaceGroup::Water.y_offset() > SurfaceGroup::Grass.y_offset());
        assert!(SurfaceGroup::Water.depth_bias() > SurfaceGroup::Grass.depth_bias());
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
        assert!(SurfaceGroup::Canopy.depth_bias() > SurfaceGroup::Road(RoadClass::Rail).depth_bias());
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
        let meshes = tessellate_roads(GRONINGEN_Z14, 1000.0).expect("tessellate");
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
        // 100 m due east, 7 m wide (a residential street).
        ribbon(
            &[[0.0, 0.0], [100.0, 0.0]],
            3.5,
            0.0,
            12.0,
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
        // `v` counts metres travelled over the class's texture length, so the far
        // end of a 100 m street (plus a half-width junction stub at each end) is
        // ~8.9 repeats in.
        let v_max = uvs.iter().fold(0.0f32, |a, uv| a.max(uv[1]));
        assert!((v_max - 107.0 / 12.0).abs() < 1e-2, "v_max {v_max}");
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
        let runs = clip_polyline(
            &[[-10.0, 50.0], [50.0, 50.0], [110.0, 50.0]],
            0.0,
            100.0,
        );
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

