//! The painted far style: a tile seen from far enough that it is read as a map
//! rather than as the ground's texture, drawn as one (`docs/mockups/macro-view/`,
//! v3 and v4, decided 2026-10-30).
//!
//! What it draws differently from the near style, and why:
//!
//! * **Land use told apart.** Housing warm grey, industry blue-grey, shops and
//!   offices mauve-grey, parks a darker green than pasture, farmland khaki
//!   ([`palette::painted`]). Up close those sit within a few counts of the grass
//!   on purpose (streets and buildings carry the figure there); from the air
//!   they are all that is left, and the old far style made a province one sheet.
//! * **Roads by rank.** Motorways gold with a dark edge, main roads cream with a
//!   dark edge, local streets fine and pale (and only on the nearest far tiles),
//!   railways dashed. The near style paints the carriageway's own tarmac, which
//!   is right at street level and invisible from 10 km.
//! * **Buildings with a shadow from their height.** Shortbread carries
//!   footprints only at z14, so a far tile is handed its descendants' footprints
//!   by the client ([`FarDetail`], `tile_loader::far_detail`). Each casts a
//!   shadow `height / tan(sun elevation)` long, away from the sun. Where there
//!   are no footprints, built-up land is lifted off the ground by a soft edge
//!   instead, so each town still has one.
//!
//! The decision of which tiles are painted is [`painted_style`]; the tile tree
//! picks coarser tiles further from the camera, so it follows distance from the
//! camera, not height, and what is near keeps today's style.

use super::*;

/// One building as the painted map needs it: its outline in the **far tile's**
/// unit square (0..1 across, y down) and its height. Courtyards are dropped: at
/// the zooms this is drawn at they are under a pixel.
#[derive(Debug, Clone, PartialEq)]
pub struct FarBuilding {
    pub outline: Vec<[f32; 2]>,
    pub height_m: f32,
}

/// What a painted tile is given beyond its own body.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FarDetail {
    /// Footprints from the tile's z14 descendants ([`far_buildings`]).
    pub buildings: Vec<FarBuilding>,
    /// A building's shadow per metre of its height, in the tile's unit square
    /// (x east, y south) — [`shadow_step`]. `None` draws no shadows.
    pub shadow: Option<[f32; 2]>,
    /// Draw the tile painted whatever its size: the twin a tile in the band
    /// where the two styles meet carries, for the cross-fade on distance
    /// (`tile_loader::PAINTED_TWIN_ZOOM`).
    pub painted: bool,
}

/// The empty detail: what every caller that has none passes.
pub static NO_FAR_DETAIL: FarDetail = FarDetail {
    buildings: Vec::new(),
    shadow: None,
    painted: false,
};

/// Whether a raster of `source_m` ground metres, magnified `factor` times, is
/// drawn in the painted style.
#[must_use]
pub fn painted_style(source_m: f32, factor: u32) -> bool {
    source_m / factor.max(1) as f32 > FAR_STYLE_TILE_M
}

/// Ground edge under which a painted tile still draws the local streets: the
/// nearest painted band (z12 at Dutch latitudes, ~5.9 km), where the mockup
/// keeps them to ~20 km.
const LOCAL_STREETS_TILE_M: f32 = 8_000.0;

/// Ground edge over which a painted tile draws no streets at all: from about
/// z7 (~200 km a tile) the network is continental noise, and the map there is
/// land cover, water and the names on it.
const NO_STREETS_TILE_M: f32 = 150_000.0;

/// Ground edge over which only motorways, trunk and primary roads are drawn.
const MAJOR_ONLY_TILE_M: f32 = 60_000.0;

/// The sun's elevation is held to at least this when casting shadows: a low
/// sun's shadows would cover whole streets, and at night there is no shadow to
/// speak of but the ground is dark anyway.
const MIN_SUN_ELEVATION_DEG: f32 = 15.0;

/// A building's shadow per metre of height for a tile `source_m` across, with
/// the sun at `azimuth_deg` (clockwise from north) and `elevation_deg`.
///
/// Quantised — 15° of azimuth, 5° of elevation — because the result is part of
/// a rendered tile's cache key: a sun that moved a fraction of a degree must not
/// make every stored tile a miss.
#[must_use]
pub fn shadow_step(azimuth_deg: f32, elevation_deg: f32, source_m: f32) -> [f32; 2] {
    let az = ((azimuth_deg / 15.0).round() * 15.0).to_radians();
    let el = ((elevation_deg.max(MIN_SUN_ELEVATION_DEG) / 5.0).round() * 5.0).to_radians();
    let len = 1.0 / el.tan() / source_m.max(1.0);
    // Away from the sun: the sun stands at (sin az, cos az) east/north, so the
    // shadow runs west/south of that; y is down (south) in the unit square.
    [-az.sin() * len, az.cos() * len]
}

/// The footprints of a z14 tile, placed in the unit square of its ancestor
/// `shift` zooms up, at index `(ox, oy)` within it.
///
/// Vertices closer than `min_step` (in the ancestor's unit square) are merged:
/// a z14 outline carries detail a far tile's pixel cannot show, and every
/// vertex is kept in memory for as long as the tile is.
#[must_use]
pub fn far_buildings(
    tile: &TileData,
    shift: u32,
    ox: u32,
    oy: u32,
    min_step: f32,
) -> Vec<FarBuilding> {
    let Some(meta) = tile.layer("buildings") else {
        return Vec::new();
    };
    let k = (1u32 << shift.min(16)) as f32;
    let extent = TileData::extent(meta);
    let to = |c: &geo_types::Coord<f32>| {
        [
            (c.x / extent + ox as f32) / k,
            (c.y / extent + oy as f32) / k,
        ]
    };
    let mut out = Vec::new();
    for feature in tile.features(meta) {
        let height_m = num_of(feature, "height").unwrap_or(6.0).clamp(3.0, 300.0);
        let polys: Vec<&geo_types::Polygon<f32>> = match &feature.geometry {
            Geometry::Polygon(p) => vec![p],
            Geometry::MultiPolygon(mp) => mp.iter().collect(),
            _ => continue,
        };
        for poly in polys {
            let mut outline: Vec<[f32; 2]> = Vec::with_capacity(poly.exterior().0.len());
            for c in poly.exterior().coords() {
                if !c.x.is_finite() || !c.y.is_finite() {
                    continue;
                }
                let p = to(c);
                if outline.last().is_some_and(|q: &[f32; 2]| {
                    (q[0] - p[0]).abs() < min_step && (q[1] - p[1]).abs() < min_step
                }) {
                    continue;
                }
                outline.push(p);
            }
            if outline.len() >= 3 {
                out.push(FarBuilding { outline, height_m });
            }
        }
    }
    out
}

/// A numeric property, whatever its encoding.
fn num_of(feature: &Feature, key: &str) -> Option<f32> {
    match feature.properties.as_ref()?.get(key)? {
        Value::Int(v) | Value::SInt(v) => Some(*v as f32),
        Value::UInt(v) => Some(*v as f32),
        Value::Float(v) => Some(*v),
        Value::Double(v) => Some(*v as f32),
        _ => None,
    }
}

/// The painted tone of a `land` kind, from OSM's land layer or the land cover.
#[must_use]
pub fn painted_land(kind: &str) -> Option<u32> {
    use palette::painted as p;
    Some(match kind {
        "grass" | "grassland" | "meadow" | "village_green" | "greenfield" | "recreation_ground"
        | "playground" | "golf_course" | "miniature_golf" => p::GRASS,
        "park" | "garden" | "allotments" | "cemetery" | "grave_yard" => p::PARK,
        "forest" | "wood" => p::FOREST,
        "scrub" | "heath" => p::SCRUB,
        "farmland"
        | "orchard"
        | "plant_nursery"
        | "vineyard"
        | "farmyard"
        | "greenhouse_horticulture" => p::FARMLAND,
        "marsh" | "wet_meadow" | "swamp" | "bog" => p::MARSH,
        "sand" | "beach" | "scree" | "bare_rock" | "shingle" | "quarry" => p::SAND,
        "glacier" => p::ICE,
        "residential" | "garages" => p::RESIDENTIAL,
        "industrial" | "railway" | "brownfield" | "landfill" | "construction" | "port" => {
            p::INDUSTRIAL
        }
        "commercial" | "retail" => p::COMMERCIAL,
        "water" => return None,
        _ => return None,
    })
}

fn is_built(rgb: u32) -> bool {
    use palette::painted as p;
    matches!(rgb, p::RESIDENTIAL | p::INDUSTRIAL | p::COMMERCIAL)
}

/// A solid shader with alpha.
fn tint(rgb: u32, alpha: f32) -> Shader<'static> {
    Shader::SolidColor(tiny_skia::Color::from_rgba8(
        ((rgb >> 16) & 0xFF) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    ))
}

/// Everything [`paint`] needs to know about the raster it draws into.
pub(super) struct Canvas {
    /// The whole tile's edge in pixels (a band is a slice of it).
    pub px: f32,
    /// The overzoom factor (1 for every painted tile in practice).
    pub f: f32,
    /// The band's first row.
    pub y0: u32,
    /// The stroke-width multiplier against the authored [`TILE_PX`] widths.
    pub wf: f32,
    /// The source tile's ground edge in metres.
    pub source_m: f32,
    /// The requested sub-tile.
    pub sub: SubTile,
}

impl Canvas {
    /// Tile-extent units → this band's pixels.
    fn layer_transform(&self, extent: f32) -> Transform {
        let s = self.px * self.f / extent;
        Transform::from_row(
            s,
            0.0,
            0.0,
            s,
            -(self.sub.x as f32) * self.px,
            -(self.sub.y as f32) * self.px - self.y0 as f32,
        )
    }

    /// The source tile's unit square → this band's pixels.
    fn unit_transform(&self) -> Transform {
        self.layer_transform(1.0)
    }
}

/// Paint `tile` in the painted style into `pixmap`, which already holds the
/// paper. The land cover `underlay` was built in painted colours by the caller
/// (`landcover_fills` with `painted`).
pub(super) fn paint(
    pixmap: &mut Pixmap,
    tile: &TileData,
    c: &Canvas,
    underlay: &[OverlayFill],
    far: &FarDetail,
) {
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint_overlay(pixmap, &mut paint, underlay, &c.unit_transform());

    // Land: built first, then what grows on it, as the near style stacks it.
    let mut built_mask: Option<Pixmap> = None;
    if let Some(meta) = tile.layer("land") {
        let t = c.layer_transform(TileData::extent(meta));
        let mut feats: Vec<(&Feature, u32)> = tile
            .features(meta)
            .iter()
            .filter_map(|f| Some((f, kind_of(f).and_then(painted_land)?)))
            .collect();
        feats.sort_by_key(|(_, rgb)| !is_built(*rgb));
        fill_runs(pixmap, &mut paint, &feats, &t, 1.0);
        // The lifted edge under built-up land, where no footprints are drawn.
        if far.buildings.is_empty() {
            let built: Vec<(&Feature, u32)> = feats
                .iter()
                .copied()
                .filter(|(_, rgb)| is_built(*rgb))
                .collect();
            if !built.is_empty() {
                let mut mask = Pixmap::new(pixmap.width(), pixmap.height());
                if let Some(m) = mask.as_mut() {
                    let white: Vec<(&Feature, u32)> =
                        built.iter().map(|(f, _)| (*f, 0xFF_FF_FF)).collect();
                    fill_runs(m, &mut paint, &white, &t, 1.0);
                }
                built_mask = mask;
            }
        }
    }
    if let Some(mask) = built_mask {
        // Lit from the sun's side: the edge falls away from it, 2 px.
        let dir = far
            .shadow
            .map(|s| {
                let l = (s[0] * s[0] + s[1] * s[1]).sqrt().max(1e-9);
                [s[0] / l, s[1] / l]
            })
            .unwrap_or([0.0, -1.0]);
        drop_edge(pixmap, &mask, [dir[0] * 2.0, dir[1] * 2.0], 0.6);
    }

    // Water: open water, and the rivers and canals as lines. Ditches thinner
    // than a pixel are left out — at this scale they only grey the polder.
    for layer in ["ocean", "water_polygons"] {
        let Some(meta) = tile.layer(layer) else {
            continue;
        };
        let extent = TileData::extent(meta);
        let t = c.layer_transform(extent);
        let px_per_unit = c.px * c.f / extent;
        let min_area = 6.0 / (px_per_unit * px_per_unit);
        let feats: Vec<(&Feature, u32)> = tile
            .features(meta)
            .iter()
            .filter(|f| layer == "ocean" || area_of(&f.geometry) >= min_area)
            .map(|f| (f, palette::painted::WATER))
            .collect();
        fill_runs(pixmap, &mut paint, &feats, &t, 1.0);
    }
    if let Some(meta) = tile.layer("water_lines") {
        let t = c.layer_transform(TileData::extent(meta));
        let w = if c.source_m < LOCAL_STREETS_TILE_M {
            2.0
        } else {
            1.2
        };
        let feats: Vec<&Feature> = tile
            .features(meta)
            .iter()
            .filter(|f| matches!(kind_of(f), Some("river" | "canal")))
            .collect();
        stroke(
            pixmap,
            &mut paint,
            &feats,
            &t,
            palette::painted::WATER,
            1.0,
            w * c.wf,
            None,
        );
    }

    if !far.buildings.is_empty() {
        paint_buildings(pixmap, c, far);
    }

    paint_streets(pixmap, &mut paint, tile, c);

    if let Some(meta) = tile.layer("boundaries") {
        let t = c.layer_transform(TileData::extent(meta));
        let feats: Vec<&Feature> = tile.features(meta).iter().collect();
        stroke(
            pixmap,
            &mut paint,
            &feats,
            &t,
            palette::BOUNDARY,
            0.7,
            1.2 * c.wf,
            None,
        );
    }
}

/// Fill features in runs of one colour, the way the near style batches.
fn fill_runs(
    pixmap: &mut Pixmap,
    paint: &mut Paint<'static>,
    feats: &[(&Feature, u32)],
    t: &Transform,
    alpha: f32,
) {
    let mut pb = PathBuilder::new();
    let mut run: Option<u32> = None;
    let mut points = 0usize;
    let mut flush = |pb: &mut PathBuilder, rgb: Option<u32>, pixmap: &mut Pixmap| {
        let Some(rgb) = rgb else { return };
        if let Some(path) = std::mem::replace(pb, PathBuilder::new()).finish() {
            paint.shader = tint(rgb, alpha);
            pixmap.fill_path(&path, paint, FillRule::Winding, *t, None);
        }
    };
    for (f, rgb) in feats {
        if run != Some(*rgb) || points >= 4096 {
            flush(&mut pb, run, pixmap);
            run = Some(*rgb);
            points = 0;
        }
        points += append_area(&mut pb, &f.geometry);
    }
    flush(&mut pb, run, pixmap);
}

/// Stroke features as one batch; `dash` is `[on, off]` in pixels.
#[allow(clippy::too_many_arguments)]
fn stroke(
    pixmap: &mut Pixmap,
    paint: &mut Paint<'static>,
    feats: &[&Feature],
    t: &Transform,
    rgb: u32,
    alpha: f32,
    width_px: f32,
    dash: Option<[f32; 2]>,
) {
    if feats.is_empty() || width_px <= 0.0 {
        return;
    }
    let mut pb = PathBuilder::new();
    for f in feats {
        append_lines(&mut pb, &f.geometry);
    }
    let Some(path) = pb.finish() else {
        return;
    };
    paint.shader = tint(rgb, alpha);
    // Widths and dashes are in path units, which are tile-extent units here
    // (`flush_batch` has the story of the stroke that came out a hairline).
    let s = t.sx.max(1e-9);
    let stroke = Stroke {
        width: width_px / s,
        line_cap: if dash.is_some() {
            LineCap::Butt
        } else {
            LineCap::Round
        },
        line_join: LineJoin::Round,
        dash: dash.and_then(|[on, off]| tiny_skia::StrokeDash::new(vec![on / s, off / s], 0.0)),
        ..Stroke::default()
    };
    pixmap.stroke_path(&path, paint, &stroke, *t, None);
}

/// Streets by rank, bottom first: local, tertiary, main roads, motorways, rail.
fn paint_streets(pixmap: &mut Pixmap, paint: &mut Paint<'static>, tile: &TileData, c: &Canvas) {
    use palette::painted as p;
    if c.source_m > NO_STREETS_TILE_M {
        return;
    }
    let Some(meta) = tile.layer("streets") else {
        return;
    };
    let t = c.layer_transform(TileData::extent(meta));
    let near = c.source_m < LOCAL_STREETS_TILE_M;
    let major_only = c.source_m > MAJOR_ONLY_TILE_M;
    let mut local = Vec::new();
    let mut tertiary = Vec::new();
    let mut main = Vec::new();
    let mut motorway = Vec::new();
    let mut rail = Vec::new();
    for f in tile.features(meta) {
        if crate::tunnels::underground(f) {
            continue;
        }
        match kind_of(f).unwrap_or("") {
            "motorway" | "trunk" => motorway.push(f),
            "primary" => main.push(f),
            "secondary" if !major_only => main.push(f),
            "tertiary" if !major_only => tertiary.push(f),
            "residential" | "unclassified" | "living_street" if near => local.push(f),
            "rail" if !major_only || c.source_m < 2.0 * MAJOR_ONLY_TILE_M => rail.push(f),
            _ => {}
        }
    }
    let w = c.wf;
    // Two width sets, the mockup's at 10 km and at 40 km and further.
    let (tw, mc, mf, xc, xf) = if near {
        (1.6, 4.4, 2.6, 7.0, 4.0)
    } else if major_only {
        (0.9, 2.6, 1.3, 4.0, 2.2)
    } else {
        (0.9, 3.0, 1.6, 5.5, 3.0)
    };
    stroke(
        pixmap,
        paint,
        &local,
        &t,
        p::LOCAL_STREET,
        0.55,
        0.8 * w,
        None,
    );
    stroke(
        pixmap,
        paint,
        &tertiary,
        &t,
        p::TERTIARY,
        0.85,
        tw * w,
        None,
    );
    stroke(
        pixmap,
        paint,
        &main,
        &t,
        p::MAIN_ROAD_CASING,
        0.45,
        mc * w,
        None,
    );
    stroke(pixmap, paint, &main, &t, p::MAIN_ROAD, 1.0, mf * w, None);
    stroke(
        pixmap,
        paint,
        &motorway,
        &t,
        p::MOTORWAY_CASING,
        0.6,
        xc * w,
        None,
    );
    stroke(pixmap, paint, &motorway, &t, p::MOTORWAY, 1.0, xf * w, None);
    stroke(
        pixmap,
        paint,
        &rail,
        &t,
        p::RAIL_CASING,
        0.55,
        2.4 * w,
        None,
    );
    stroke(
        pixmap,
        paint,
        &rail,
        &t,
        p::RAIL,
        1.0,
        1.0 * w,
        Some([5.0 * w, 5.0 * w]),
    );
}

/// Footprints with a roof tone each and a shadow from their height.
///
/// Drawn into two masks and composited by hand rather than painted straight on:
/// a shadow is translucent, and the swept copies of one building (and the
/// shadows of neighbours) overlap, so painting them would darken every overlap
/// twice. Into a mask they union.
fn paint_buildings(pixmap: &mut Pixmap, c: &Canvas, far: &FarDetail) {
    let (w, h) = (pixmap.width(), pixmap.height());
    let (Some(mut shadow), Some(mut roofs)) = (Pixmap::new(w, h), Pixmap::new(w, h)) else {
        return;
    };
    let t = c.unit_transform();
    let unit_px = c.px * c.f;
    // The band's window in the unit square, with room for the longest shadow.
    let reach = far
        .shadow
        .map(|s| (s[0].abs().max(s[1].abs())) * 300.0)
        .unwrap_or(0.0);
    let win = (
        c.sub.x as f32 / c.f - reach,
        (c.sub.y as f32 * c.px + c.y0 as f32) / unit_px - reach,
        (c.sub.x as f32 + 1.0) / c.f + reach,
        (c.sub.y as f32 * c.px + c.y0 as f32 + h as f32) / unit_px + reach,
    );
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    let mut shadow_pb = PathBuilder::new();
    let mut shadow_points = 0usize;
    let mut tone_pb: Vec<PathBuilder> = (0..5).map(|_| PathBuilder::new()).collect();
    let mut tone_points = [0usize; 5];
    let flush =
        |pb: &mut PathBuilder, target: &mut Pixmap, rgb: u32, paint: &mut Paint<'static>| {
            if let Some(path) = std::mem::replace(pb, PathBuilder::new()).finish() {
                paint.shader = shader(rgb);
                target.fill_path(&path, paint, FillRule::Winding, t, None);
            }
        };
    let tones: [u32; 5] = [
        palette::painted::ROOFS[0],
        palette::painted::ROOFS[1],
        palette::painted::ROOFS[2],
        palette::painted::ROOFS[3],
        palette::painted::ROOF_TALL,
    ];
    for b in &far.buildings {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in &b.outline {
            x0 = x0.min(p[0]);
            y0 = y0.min(p[1]);
            x1 = x1.max(p[0]);
            y1 = y1.max(p[1]);
        }
        if x1 < win.0 || x0 > win.2 || y1 < win.1 || y0 > win.3 {
            continue;
        }
        if let Some(s) = far.shadow {
            let len_px = (s[0] * s[0] + s[1] * s[1]).sqrt() * b.height_m * unit_px;
            let steps = (len_px.ceil() as usize).clamp(1, 24);
            for k in 1..=steps {
                let d = b.height_m * k as f32 / steps as f32;
                let (dx, dy) = (s[0] * d, s[1] * d);
                shadow_points += push_ring(&mut shadow_pb, &b.outline, dx, dy);
            }
            if shadow_points >= 4096 {
                flush(&mut shadow_pb, &mut shadow, 0xFF_FF_FF, &mut paint);
                shadow_points = 0;
            }
        }
        // A tone from the outline's first vertex: stable for a building
        // whichever tile it is drawn in, and no id needed.
        let tone = if b.height_m > 15.0 {
            4
        } else {
            let q = b.outline[0];
            let hv = ((q[0] * 7_340_033.0) as i64 ^ (q[1] * 3_145_739.0) as i64).unsigned_abs();
            (hv % 4) as usize
        };
        tone_points[tone] += push_ring(&mut tone_pb[tone], &b.outline, 0.0, 0.0);
        if tone_points[tone] >= 4096 {
            flush(&mut tone_pb[tone], &mut roofs, tones[tone], &mut paint);
            tone_points[tone] = 0;
        }
    }
    flush(&mut shadow_pb, &mut shadow, 0xFF_FF_FF, &mut paint);
    for (i, pb) in tone_pb.iter_mut().enumerate() {
        flush(pb, &mut roofs, tones[i], &mut paint);
    }

    // Composite: the shadow where no roof is, then the roofs over it.
    let sh = palette::painted::SHADOW;
    let shc = [
        ((sh >> 16) & 0xFF) as f32,
        ((sh >> 8) & 0xFF) as f32,
        (sh & 0xFF) as f32,
    ];
    let out = pixmap.data_mut();
    let s = shadow.data();
    let r = roofs.data();
    for i in (0..out.len()).step_by(4) {
        let ra = r[i + 3] as f32 / 255.0;
        let sa = s[i + 3] as f32 / 255.0 * 0.45 * (1.0 - ra);
        for ch in 0..3 {
            let base = out[i + ch] as f32;
            let shaded = base + (shc[ch] - base) * sa;
            // `roofs` is premultiplied, as tiny-skia keeps every pixmap.
            out[i + ch] = (r[i + ch] as f32 + shaded * (1.0 - ra))
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
}

/// Push one closed ring, offset by `(dx, dy)` in the unit square.
fn push_ring(pb: &mut PathBuilder, ring: &[[f32; 2]], dx: f32, dy: f32) -> usize {
    let mut it = ring.iter();
    let Some(first) = it.next() else {
        return 0;
    };
    pb.move_to(first[0] + dx, first[1] + dy);
    for p in it {
        pb.line_to(p[0] + dx, p[1] + dy);
    }
    pb.close();
    ring.len()
}

/// The soft edge under built-up land: darken where `mask` shifted by `d` pixels
/// covers and `mask` itself does not.
fn drop_edge(pixmap: &mut Pixmap, mask: &Pixmap, d: [f32; 2], strength: f32) {
    let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
    let m = mask.data();
    let at = |x: i32, y: i32| -> f32 {
        if x < 0 || y < 0 || x >= w || y >= h {
            0.0
        } else {
            m[((y * w + x) * 4 + 3) as usize] as f32 / 255.0
        }
    };
    // Bilinear, so a 1.5 px offset is a soft edge rather than a hard step.
    let sample = |x: f32, y: f32| -> f32 {
        let (fx, fy) = (x.floor(), y.floor());
        let (tx, ty) = (x - fx, y - fy);
        let (ix, iy) = (fx as i32, fy as i32);
        let a = at(ix, iy) * (1.0 - tx) + at(ix + 1, iy) * tx;
        let b = at(ix, iy + 1) * (1.0 - tx) + at(ix + 1, iy + 1) * tx;
        a * (1.0 - ty) + b * ty
    };
    let sh = palette::painted::SHADOW;
    let shc = [
        ((sh >> 16) & 0xFF) as f32,
        ((sh >> 8) & 0xFF) as f32,
        (sh & 0xFF) as f32,
    ];
    let out = pixmap.data_mut();
    for y in 0..h {
        for x in 0..w {
            let own = at(x, y);
            if own >= 1.0 {
                continue;
            }
            // Average two taps along the offset for a little blur.
            let s = 0.5
                * (sample(x as f32 - d[0], y as f32 - d[1])
                    + sample(x as f32 - d[0] * 0.5, y as f32 - d[1] * 0.5));
            let a = s * (1.0 - own) * strength;
            if a <= 0.0 {
                continue;
            }
            let i = ((y * w + x) * 4) as usize;
            for ch in 0..3 {
                let base = out[i + ch] as f32;
                out[i + ch] = (base + (shc[ch] - base) * a).round() as u8;
            }
        }
    }
}

/// Unsigned area of a geometry's polygons in its own units, holes subtracted.
fn area_of(geom: &Geometry<f32>) -> f32 {
    fn ring(ls: &geo_types::LineString<f32>) -> f32 {
        let c = &ls.0;
        if c.len() < 3 {
            return 0.0;
        }
        let mut a = 0.0;
        for i in 0..c.len() {
            let (p, q) = (c[i], c[(i + 1) % c.len()]);
            a += p.x * q.y - q.x * p.y;
        }
        (a * 0.5).abs()
    }
    let poly = |p: &geo_types::Polygon<f32>| {
        (ring(p.exterior()) - p.interiors().iter().map(ring).sum::<f32>()).max(0.0)
    };
    match geom {
        Geometry::Polygon(p) => poly(p),
        Geometry::MultiPolygon(mp) => mp.iter().map(poly).sum(),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadows_fall_away_from_the_sun() {
        // A sun in the south casts shadows north (y is down, so negative y).
        let s = shadow_step(180.0, 30.0, 5_000.0);
        assert!(s[1] < 0.0 && s[0].abs() < 1e-6, "{s:?}");
        // From the west, they fall east.
        let s = shadow_step(270.0, 30.0, 5_000.0);
        assert!(s[0] > 0.0, "{s:?}");
        // A 10 m building under a 45° sun casts 10 m.
        let s = shadow_step(180.0, 45.0, 5_000.0);
        assert!(((-s[1]) * 5_000.0 * 10.0 - 10.0).abs() < 0.01, "{s:?}");
    }

    #[test]
    fn a_sun_moving_a_little_keeps_the_same_shadow() {
        assert_eq!(
            shadow_step(181.0, 31.0, 5_000.0),
            shadow_step(183.0, 29.0, 5_000.0)
        );
    }

    #[test]
    fn built_land_kinds_are_told_apart() {
        let r = painted_land("residential");
        let i = painted_land("industrial");
        let c = painted_land("retail");
        assert!(r != i && i != c && r != c);
        assert_ne!(painted_land("park"), painted_land("meadow"));
        assert_ne!(painted_land("farmland"), painted_land("grassland"));
    }
}
