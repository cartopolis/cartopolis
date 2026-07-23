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

/// Edge length in pixels of a rendered tile. Larger than the 256 px raster tiles
/// we used to fetch: the texture is stretched across a whole region on the ground,
/// and re-rendering costs us CPU rather than bandwidth, so we may as well take the
/// resolution.
pub const TILE_PX: u32 = 512;

/// Highest zoom the Shortbread source actually contains. Anything above this is
/// rendered by magnifying the z14 ancestor.
pub const MAX_SOURCE_ZOOM: u32 = 14;

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
mod palette {
    pub const PAPER: u32 = 0xEE_EA_E2;
    pub const WATER: u32 = 0xA8_C4_DA;
    pub const FOREST: u32 = 0xC3_D3_BB;
    pub const GRASS: u32 = 0xD2_DE_C6;
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
fn kind_of(feature: &Feature) -> Option<&str> {
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
    /// The window is only 10 mm, far too little to separate anything by geometry
    /// alone against a 200 km far plane, so the actual z-fight is settled by
    /// [`Self::depth_bias`]; these offsets only fix the ordering intent.
    #[must_use]
    pub fn y_offset(self) -> f32 {
        match self {
            Self::Built => -0.008,
            Self::Grass => -0.007,
            Self::Forest | Self::Sand => -0.006,
            Self::Water => -0.004,
        }
    }

    /// Depth-buffer bias, which is what actually stops the flicker.
    ///
    /// The camera runs `near = 0`, `far = 200 km` (`free_camera::camera_controls`
    /// widens it for the globe tier), so depth precision at ground range is far too
    /// coarse to resolve a millimetre of physical separation — coplanar ground
    /// surfaces then flicker as the camera moves, which is exactly the symptom.
    /// `StandardMaterial::depth_bias` biases the comparison itself, so the ordering
    /// holds at any distance and does not depend on the far plane.
    #[must_use]
    pub fn depth_bias(self) -> f32 {
        match self {
            Self::Built => 1.0,
            Self::Grass => 2.0,
            Self::Forest | Self::Sand => 3.0,
            Self::Water => 4.0,
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

/// A tessellated surface ready to become a Bevy mesh. Positions are **tile-local
/// metres**: origin at the tile's north-west corner, +x east, +z south — which is
/// the same convention `map_stream` places its ground quads in, so the caller only
/// has to position the tile's corner.
#[derive(Debug, Clone)]
pub struct SurfaceMesh {
    pub group: SurfaceGroup,
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
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
        })
        .collect())
}

/// Earcut one polygon (exterior + holes) into an existing position/index buffer.
fn tessellate_polygon(
    poly: &geo_types::Polygon<f32>,
    scale: f32,
    extent: f32,
    positions: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
) {
    // MVT geometry runs past the tile edge into a buffer zone, so neighbouring
    // tiles would overlap — and coplanar overlapping ground z-fights. Clamping to
    // the tile bounds is an approximation of a real polygon clip: it flattens the
    // overhang against the border, which is invisible because the adjacent tile
    // continues the same surface. A true Sutherland–Hodgman clip is the refinement
    // if a shape ever visibly kinks at a seam.
    let clamp = |v: f32| v.clamp(0.0, extent);

    // Rings arrive closed (last point repeats the first); earcut wants it open, and
    // a duplicated vertex there produces degenerate triangles.
    let ring_coords = |ring: &geo_types::LineString<f32>| -> Vec<f64> {
        let pts = ring.0.as_slice();
        let n = if pts.len() >= 2 && pts[0] == pts[pts.len() - 1] {
            pts.len() - 1
        } else {
            pts.len()
        };
        pts[..n]
            .iter()
            .flat_map(|c| [f64::from(clamp(c.x)), f64::from(clamp(c.y))])
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

    /// A real z14 tile over Groningen, fetched from tiles.versatiles.org.
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

    #[test]
    fn raster_sniffing() {
        assert!(looks_like_raster(b"\x89PNG\r\n\x1a\n____"));
        assert!(looks_like_raster(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(!looks_like_raster(GRONINGEN_Z14));
    }
}
