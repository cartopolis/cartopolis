//! Raster rendering: a tile painted into RGBA pixels in PAINT_ORDER, with overlays, the
//! survey, off-screen culling and the banded parallel paint.

use super::*;

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
pub(super) const LAND_KINDS: &[(&str, u32)] = &[
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

pub(super) fn shader(rgb: u32) -> Shader<'static> {
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
pub(super) fn fill_geometry(
    pixmap: &mut Pixmap,
    paint: &Paint,
    geom: &Geometry<f32>,
    t: &Transform,
) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_tiles::bytes::render_mvt;

    // A real z14 tile over Groningen, from the VersaTiles Shortbread source
    // (now self-hosted at tiles.cartopolis.org; the archive is identical).
    use crate::testdata::GRONINGEN_Z14;

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
        // Each window on its own thread: they share nothing, and one after
        // another they were 1.4 s of renders.
        let tile = &tile;
        std::thread::scope(|s| {
            for (factor, x, y) in [(1, 0, 0), (4, 2, 1), (8, 3, 5)] {
                s.spawn(move || banding_changes_no_geometry_in(tile, SubTile { factor, x, y }));
            }
        });
    }

    /// One window of [`banding_the_raster_changes_no_geometry`].
    fn banding_changes_no_geometry_in(tile: &TileData, sub: SubTile) {
        let factor = sub.factor;
        let whole = render_mvt_banded(tile, sub, TILE_PX, NOMINAL_Z14_TILE_M, false, 1, &[], &[])
            .expect("whole render");
        // 7 is in the list on purpose: it does not divide TILE_PX, so the
        // last band is short and the row arithmetic has to survive it.
        for bands in [2u32, 3, 4, 7] {
            let split = render_mvt_banded(
                tile,
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

/// Peak memory of rendering real tiles the way the globe does, for finding a tile
/// that runs the web build's heap out (`docs/notes/wasm-performance.md`). A
/// diagnostic, not a gate: point `CARTO_PEAK_TILES` at a directory of
/// `z_x_y.mvt` bodies and run it with `--ignored --nocapture`.
#[cfg(test)]
mod render_peak {
    use super::*;

    #[test]
    #[ignore = "diagnostic: CARTO_PEAK_TILES=<dir of z_x_y.mvt>"]
    fn peak_memory_of_each_tile_in_a_directory() {
        let Ok(dir) = std::env::var("CARTO_PEAK_TILES") else {
            return;
        };
        let mut rows = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("CARTO_PEAK_TILES is a directory") {
            let path = entry.unwrap().path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let parts: Vec<i64> = stem.split('_').filter_map(|p| p.parse().ok()).collect();
            let [z, x, y] = parts[..] else { continue };
            let bytes = std::fs::read(&path).unwrap();
            let Ok(tile) = TileData::parse(&bytes) else {
                println!("{stem}: does not parse");
                continue;
            };
            let (_, sub) = SubTile::resolve(z as u32, x, y);
            let (north, _) = cartopolis_core::world::tile_to_lat_lng(x, y, z as u32);
            let (south, _) = cartopolis_core::world::tile_to_lat_lng(x, y + 1, z as u32);
            let source_m =
                cartopolis_core::world::tile_to_meters(z as u32, (north + south) / 2.0) as f32;
            // Decoding alone, on a fresh parse: what the memo keeps per layer.
            let fresh = TileData::parse(&bytes).unwrap();
            let (per_layer, decode) = crate::perf::Cost::measure(|| {
                fresh
                    .layers()
                    .iter()
                    .map(|l| (l.name.clone(), fresh.features(l).len()))
                    .collect::<Vec<_>>()
            });
            let busiest = per_layer.iter().max_by_key(|(_, n)| *n).cloned();
            println!(
                "{z}/{x}/{y}: decoding every layer keeps {} MB in {} allocations; {} features, most in {:?}",
                decode.retained_bytes >> 20,
                decode.allocs,
                per_layer.iter().map(|(_, n)| n).sum::<usize>(),
                busiest,
            );
            let started = std::time::Instant::now();
            let (out, cost) = crate::perf::Cost::measure(|| {
                // One band, as on the web: the counter sees only this thread.
                render_mvt_banded(&tile, sub, 256, source_m, sub.factor > 1, 1, &[], &[])
                    .map(|p| p.len())
            });
            rows.push((
                cost.peak_bytes,
                format!(
                "{z}/{x}/{y}: {} kB body, peak {} MB, largest {} MB, {} allocations, {} ms, {:?}",
                bytes.len() / 1024,
                cost.peak_bytes >> 20,
                cost.largest_alloc >> 20,
                cost.allocs,
                started.elapsed().as_millis(),
                out.map(|_| "ok"),
            ),
            ));
        }
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, row) in rows {
            println!("{row}");
        }
    }
}
