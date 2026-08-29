//! Street objects that were **surveyed** rather than guessed: the BGT's trees
//! and its street lighting.
//!
//! `crates/geo/src/scatter.rs` states the gap in its own module note — Shortbread
//! carries canopy *areas*, so "the trees here are scattered, not surveyed" — and
//! `furniture.rs` reads a `pois` layer that has no street lighting in it at all.
//! The BGT registers both, object by object, for the whole country, and
//! `tools/bgt_objects.py` bins them into the same z14 cells everything else here
//! is streamed in.
//!
//! **Added to the generated layers, never replacing them.** Two reasons, and the
//! second is the one that decides it. The scatter plants inside park, wood and
//! garden polygons while a surveyed street tree stands on the pavement, so the
//! two hardly overlap by construction. And the BGT's tree points are *optional*
//! content whose coverage varies by municipality — 2,340 trees over Groningen,
//! which is a street layer and not a forest — so a cell that has none must keep
//! its scattered canopies or the map would lose its parks in exchange for a
//! handful of limes.
//!
//! There is no streamer here and no resource. The data is consumed inside
//! `map_geometry`'s existing per-cell worker, beside the scatter it joins, for
//! the reason `building_facts` gives about `lod22`: this ends up baked into a
//! merged mesh, so it has to be known on the worker that builds it rather than
//! arriving later on a schedule of its own.

use crate::platform::{http, storage};
use crate::systems::coverage::CoverageSlot;
use crate::systems::furniture::{FurnitureInstance, FurnitureKind, LineKind, SurveyedLine};
use crate::systems::osm_buildings::tdbag_url;
use crate::systems::paving::{PavedArea, PavingKind};
use crate::systems::scatter::TreeInstance;
use cartopolis_core::world::tile_to_lat_lng;

/// Fixed cache/streaming zoom — must match `map_geometry::GEOMETRY_ZOOM` and the
/// `CACHE_ZOOM` the extractor bins into.
const BGT_ZOOM: u32 = 14;

/// Which cells the host serves surveyed objects for.
static COVERAGE: CoverageSlot = CoverageSlot::new("bgt", "/bgt/coverage.json", BGT_ZOOM);

/// Keep the coverage manifest fetched.
///
/// A system of its own, with one parameter, because the natural caller cannot
/// have it: `map_geometry::update_map_surfaces` consumes the answer but sits on
/// Bevy's 16-parameter ceiling and deliberately holds no `Res<Time>` — it passes
/// a literal `0.0` where a clock would go, which is exactly the value that would
/// defeat the retry spacing here and re-ask on every frame until the host
/// answered. Hanging the fetch off `building_facts`, which does have a clock, was
/// the alternative and would have made two unrelated data trees share a system.
pub fn prefetch_bgt_coverage(time: bevy::prelude::Res<bevy::prelude::Time>) {
    COVERAGE.ensure(time.elapsed_secs_f64());
}

/// 0 while the coverage question is open, 1 once answered — read by
/// `map_geometry` as a rebuild revision, since surveyed objects are baked into
/// its merged meshes. Can only change once per process.
pub(crate) fn coverage_revision() -> u32 {
    COVERAGE.revision()
}

/// The BGT attribution line, once coverage has loaded and is non-empty.
pub fn bgt_attribution() -> Option<String> {
    COVERAGE.attribution()
}

/// The forms the extractor can name, in wire order. Mirrors `PROP_KINDS` in
/// `tools/bgt_objects.py` — **append, never renumber**.
///
/// An index past the end is dropped rather than clamped: a host running a newer
/// extractor than this client should lose the objects it has no form for, not
/// draw them as whatever happens to sit at the end of this list.
const PROP_KINDS: [FurnitureKind; 7] = [
    FurnitureKind::Lamp,
    FurnitureKind::Bench,
    FurnitureKind::Shelter,
    FurnitureKind::PicnicTable,
    FurnitureKind::Artwork,
    FurnitureKind::Swing,
    FurnitureKind::Cabinet,
];

/// The linear forms, in wire order. Mirrors `LINE_KINDS` in
/// `tools/bgt_objects.py` — **append, never renumber**.
const LINE_KINDS: [LineKind; 3] = [LineKind::Fence, LineKind::Wall, LineKind::Hedge];

/// The paved forms, in wire order. Mirrors `SURFACE_KINDS` in
/// `tools/bgt_objects.py` — **append, never renumber**.
const SURFACE_KINDS: [PavingKind; 8] = [
    PavingKind::Footway,
    PavingKind::Cycleway,
    PavingKind::Parking,
    PavingKind::Drive,
    PavingKind::Verge,
    PavingKind::Island,
    PavingKind::Paved,
    PavingKind::Steps,
];

/// One cell's surveyed paving, as rings of lat/lng: outer ring first, holes after.
#[derive(Clone, Debug, Default)]
pub struct SurveyedPaving {
    pub areas: Vec<(PavingKind, Vec<Vec<(f64, f64)>>)>,
}

/// Parse a cell's paving file: `{"surfaces":[[kind,[ring,…]],…]}`.
pub fn parse_surveyed_paving(text: &str) -> Result<SurveyedPaving, String> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        surfaces: Vec<(usize, Vec<Vec<(f64, f64)>>)>,
    }
    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(SurveyedPaving {
        areas: wire
            .surfaces
            .into_iter()
            // An outer ring under three points is not a surface. The extractor
            // drops those; a truncated cache entry can produce one.
            .filter(|(_, rings)| rings.first().is_some_and(|r| r.len() >= 3))
            .filter_map(|(k, rings)| SURFACE_KINDS.get(k).map(|&kind| (kind, rings)))
            .collect(),
    })
}

/// This cell's paving in the local metre frame, ready to triangulate.
///
/// A ring is kept whole or not at all, and a surface whose outer ring leaves the
/// cell is dropped: the cell that owns the surface's first vertex draws it whole,
/// and clipping it here would leave a straight edge across the middle of a car
/// park. Holes that fall outside are dropped individually — a hole that is not
/// cut is a filled patch, which is wrong but local, where a dropped outer ring
/// loses the whole surface.
#[must_use]
pub fn surveyed_paving(paving: &SurveyedPaving, cx: i64, cy: i64, tile_m: f32) -> Vec<PavedArea> {
    let to_local = |pts: &Vec<(f64, f64)>| -> Option<Vec<[f32; 2]>> {
        let out: Vec<[f32; 2]> = pts
            .iter()
            .filter_map(|&(lat, lng)| to_cell_local(lat, lng, cx, cy, tile_m).map(|(x, z)| [x, z]))
            .collect();
        (out.len() == pts.len() && out.len() >= 3).then_some(out)
    };
    paving
        .areas
        .iter()
        .filter_map(|(kind, rings)| {
            let outer = to_local(rings.first()?)?;
            let mut local = vec![outer];
            local.extend(rings[1..].iter().filter_map(to_local));
            Some(PavedArea {
                kind: *kind,
                rings: local,
            })
        })
        .collect()
}

/// One cell's surveyed objects, still in lat/lng as the host sent them.
#[derive(Clone, Debug, Default)]
pub struct SurveyedCell {
    /// Tree positions.
    pub trees: Vec<(f64, f64)>,
    /// Street objects: position and form.
    pub props: Vec<(f64, f64, FurnitureKind)>,
    /// Boundaries — fences, walls, hedges — as the polylines they were surveyed
    /// as, with their form.
    pub lines: Vec<(LineKind, Vec<(f64, f64)>)>,
}

/// Parse a cell file: `{"trees":[[lat,lng],…],"props":[[lat,lng,kind],…]}`.
pub fn parse_surveyed_cell(text: &str) -> Result<SurveyedCell, String> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        trees: Vec<(f64, f64)>,
        #[serde(default)]
        props: Vec<(f64, f64, usize)>,
        #[serde(default)]
        lines: Vec<(usize, Vec<(f64, f64)>)>,
    }
    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(SurveyedCell {
        trees: wire.trees,
        props: wire
            .props
            .into_iter()
            .filter_map(|(lat, lng, k)| PROP_KINDS.get(k).map(|&kind| (lat, lng, kind)))
            .collect(),
        lines: wire
            .lines
            .into_iter()
            // A one-point line is not a line. The extractor does not emit them,
            // and a truncated cache entry can.
            .filter(|(_, pts)| pts.len() >= 2)
            .filter_map(|(k, pts)| LINE_KINDS.get(k).map(|&kind| (kind, pts)))
            .collect(),
    })
}

fn cell_cache_key(cx: i64, cy: i64) -> String {
    format!("bgt/{BGT_ZOOM}/{cx}/{cy}.json")
}

/// Fetch one cell's **paving**, through the storage cache.
///
/// Its own file because it is by far the heaviest part — 1.5 MB against 60 kB of
/// objects for the densest Groningen cell — and the same split, for the same
/// reason, as the BAG's buildings and places.
pub(crate) async fn load_surveyed_paving(cx: i64, cy: i64) -> SurveyedPaving {
    if !COVERAGE.covers(cx, cy) {
        return SurveyedPaving::default();
    }
    let key = format!("bgt/{BGT_ZOOM}/{cx}/{cy}.surfaces.json");
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        return parse_surveyed_paving(&cached).unwrap_or_default();
    }
    let url = format!(
        "{}/bgt/{}/{}/{}.surfaces.json",
        tdbag_url(),
        BGT_ZOOM,
        cx,
        cy
    );
    let Ok(bytes) = http::fetch(http::Request::get(url)).await else {
        return SurveyedPaving::default();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return SurveyedPaving::default();
    };
    storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
    parse_surveyed_paving(&text).unwrap_or_default()
}

/// Fetch and parse one cell, through the storage cache.
///
/// An uncovered cell asks nothing and answers empty; so does an unreachable one,
/// because every consumer's fallback is the generated layer it already had.
pub(crate) async fn load_surveyed_cell(cx: i64, cy: i64) -> SurveyedCell {
    if !COVERAGE.covers(cx, cy) {
        return SurveyedCell::default();
    }
    let key = cell_cache_key(cx, cy);
    if let Some(cached) = storage::get_string(storage::Namespace::Cache, &key).await {
        return parse_surveyed_cell(&cached).unwrap_or_default();
    }
    let url = format!("{}/bgt/{}/{}/{}.json", tdbag_url(), BGT_ZOOM, cx, cy);
    let Ok(bytes) = http::fetch(http::Request::get(url)).await else {
        return SurveyedCell::default();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return SurveyedCell::default();
    };
    storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
    parse_surveyed_cell(&text).unwrap_or_default()
}

/// Convert a lat/lng to the tile-local metre frame a cell is meshed in: metres
/// from the cell's **north-west corner**, +x east, +z south.
///
/// That is the frame `furniture.rs` and `scatter.rs` work in — they get it as
/// `tile_coordinate * (tile_m / extent)` off the MVT — and matching it exactly is
/// what lets surveyed objects be handed to the same builders as generated ones.
///
/// Points outside `[0, 1)` of the cell are the caller's to drop. They happen: the
/// extractor bins on the same rule, but a cell edge in floating point is not
/// bit-identical between Python and Rust, and a lamppost on the boundary would
/// otherwise land a whole cell away.
fn to_cell_local(lat: f64, lng: f64, cx: i64, cy: i64, tile_m: f32) -> Option<(f32, f32)> {
    let (north, west) = tile_to_lat_lng(cx, cy, BGT_ZOOM);
    let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, BGT_ZOOM);
    if !(west..east).contains(&lng) || !(south..north).contains(&lat) {
        return None;
    }
    // Longitude is linear in the projection; latitude is not, so the north-south
    // fraction is taken in Mercator y rather than in degrees. Over one z14 cell
    // the difference is centimetres — but it is free to be right, and this is the
    // function every surveyed object's position passes through.
    let merc = |deg: f64| {
        let r = deg.to_radians();
        (r.tan() + 1.0 / r.cos()).ln()
    };
    let (my_n, my_s, my) = (merc(north), merc(south), merc(lat));
    let fx = (lng - west) / (east - west);
    let fy = (my_n - my) / (my_n - my_s);
    Some((fx as f32 * tile_m, fy as f32 * tile_m))
}

/// This cell's surveyed trees as instances, ready to join the scattered ones.
#[must_use]
pub fn surveyed_trees(
    cell: &SurveyedCell,
    cx: i64,
    cy: i64,
    tile_m: f32,
    seed: u64,
) -> Vec<TreeInstance> {
    let points: Vec<(f32, f32)> = cell
        .trees
        .iter()
        .filter_map(|&(lat, lng)| to_cell_local(lat, lng, cx, cy, tile_m))
        .collect();
    crate::systems::scatter::surveyed_trees(&points, seed)
}

/// This cell's surveyed boundaries, in the cell's local metre frame.
///
/// A line is kept only if **every** vertex falls inside the cell. The alternative
/// — dropping the outside ones and joining what is left — would run a fence
/// straight across whatever lies between them, and a boundary that crosses a cell
/// edge is already drawn whole by the cell that owns its first point.
#[must_use]
pub fn surveyed_lines(cell: &SurveyedCell, cx: i64, cy: i64, tile_m: f32) -> Vec<SurveyedLine> {
    cell.lines
        .iter()
        .filter_map(|(kind, pts)| {
            let local: Vec<[f32; 2]> = pts
                .iter()
                .filter_map(|&(lat, lng)| {
                    to_cell_local(lat, lng, cx, cy, tile_m).map(|(x, z)| [x, z])
                })
                .collect();
            (local.len() == pts.len() && local.len() >= 2).then_some(SurveyedLine {
                points: local,
                kind: *kind,
            })
        })
        .collect()
}

/// This cell's surveyed street objects, oriented against the tile's own streets.
#[must_use]
pub fn surveyed_props(
    cell: &SurveyedCell,
    tile: &crate::systems::vector_tiles::TileData,
    cx: i64,
    cy: i64,
    tile_m: f32,
    seed: u64,
) -> Vec<FurnitureInstance> {
    let points: Vec<(f32, f32, FurnitureKind)> = cell
        .props
        .iter()
        .filter_map(|&(lat, lng, kind)| {
            to_cell_local(lat, lng, cx, cy, tile_m).map(|(x, z)| (x, z, kind))
        })
        .collect();
    crate::systems::furniture::place_surveyed(tile, tile_m, seed, &points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_cell_and_maps_its_forms() {
        let cell = parse_surveyed_cell(
            r#"{"trees":[[53.2194,6.5665],[53.2195,6.5666]],
                "props":[[53.2194,6.5665,0],[53.2196,6.5667,1]]}"#,
        )
        .expect("valid cell");
        assert_eq!(cell.trees.len(), 2);
        assert_eq!(cell.props[0].2, FurnitureKind::Lamp);
        assert_eq!(cell.props[1].2, FurnitureKind::Bench);
    }

    /// A host running a newer extractor sends a form this build has no shape for.
    /// Dropping it is the whole point — clamping to the last known kind would put
    /// a cabinet wherever the next form is meant to stand.
    #[test]
    fn parses_boundary_lines_and_maps_their_forms() {
        let cell = parse_surveyed_cell(
            r#"{"lines":[[0,[[53.2194,6.5665],[53.2195,6.5666]]],
                         [2,[[53.2196,6.5667],[53.2197,6.5668],[53.2198,6.5669]]]]}"#,
        )
        .expect("valid cell");
        assert_eq!(cell.lines[0].0, LineKind::Fence);
        assert_eq!(cell.lines[1].0, LineKind::Hedge);
        assert_eq!(cell.lines[1].1.len(), 3);
    }

    /// A line with one point is not a line — the extractor never writes one, and a
    /// truncated cache entry can.
    #[test]
    fn a_degenerate_line_is_dropped() {
        let cell =
            parse_surveyed_cell(r#"{"lines":[[0,[[53.2,6.5]]],[1,[]]]}"#).expect("valid cell");
        assert!(cell.lines.is_empty());
    }

    /// A boundary is kept only if every vertex is inside the cell. Joining the
    /// survivors of one that leaves would run a fence straight across whatever
    /// lies between them.
    #[test]
    fn a_line_leaving_the_cell_is_dropped_whole() {
        let (cx, cy) = (8490, 5320);
        let (north, west) = tile_to_lat_lng(cx, cy, BGT_ZOOM);
        let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, BGT_ZOOM);
        let inside = ((north + south) / 2.0, (west + east) / 2.0);
        let cell = SurveyedCell {
            lines: vec![
                (LineKind::Wall, vec![inside, (inside.0 + 1e-5, inside.1)]),
                (LineKind::Wall, vec![inside, (north + 0.01, inside.1)]),
            ],
            ..SurveyedCell::default()
        };
        let built = surveyed_lines(&cell, cx, cy, 1500.0);
        assert_eq!(built.len(), 1, "only the wholly-inside line survives");
        assert_eq!(built[0].points.len(), 2);
    }

    #[test]
    fn parses_paving_and_maps_its_forms() {
        let paving = parse_surveyed_paving(
            r#"{"surfaces":[[0,[[[53.2194,6.5665],[53.2195,6.5665],[53.2195,6.5666]]]],
                            [4,[[[53.2196,6.5667],[53.2197,6.5667],[53.2197,6.5668]]]]]}"#,
        )
        .expect("valid cell");
        assert_eq!(paving.areas[0].0, PavingKind::Footway);
        assert_eq!(paving.areas[1].0, PavingKind::Verge);
    }

    /// An outer ring under three points is not a surface.
    #[test]
    fn a_degenerate_surface_is_dropped() {
        let paving =
            parse_surveyed_paving(r#"{"surfaces":[[0,[[[53.2,6.5],[53.2,6.6]]]],[1,[[]]]]}"#)
                .expect("valid cell");
        assert!(paving.areas.is_empty());
    }

    /// A surface whose outer ring leaves the cell is dropped whole: the cell that
    /// owns its first vertex draws it, and clipping here would cut a straight
    /// edge across the middle of a car park.
    #[test]
    fn paving_that_leaves_the_cell_is_dropped_whole() {
        let (cx, cy) = (8490, 5320);
        let (north, west) = tile_to_lat_lng(cx, cy, BGT_ZOOM);
        let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, BGT_ZOOM);
        let (mid_lat, mid_lng) = ((north + south) / 2.0, (west + east) / 2.0);
        let inside = vec![
            (mid_lat, mid_lng),
            (mid_lat + 1e-5, mid_lng),
            (mid_lat + 1e-5, mid_lng + 1e-5),
        ];
        let leaves = vec![
            (mid_lat, mid_lng),
            (north + 0.01, mid_lng),
            (mid_lat, mid_lng + 1e-5),
        ];
        let paving = SurveyedPaving {
            areas: vec![
                (PavingKind::Footway, vec![inside]),
                (PavingKind::Footway, vec![leaves]),
            ],
        };
        let built = surveyed_paving(&paving, cx, cy, 1500.0);
        assert_eq!(built.len(), 1, "only the wholly-inside surface survives");
    }

    #[test]
    fn an_unknown_form_is_dropped_not_clamped() {
        let cell = parse_surveyed_cell(r#"{"props":[[53.2,6.5,0],[53.2,6.5,99]]}"#).expect("valid");
        assert_eq!(cell.props.len(), 1);
        assert_eq!(cell.props[0].2, FurnitureKind::Lamp);
    }

    #[test]
    fn an_empty_cell_is_empty_rather_than_an_error() {
        let cell = parse_surveyed_cell("{}").expect("valid");
        assert!(cell.trees.is_empty() && cell.props.is_empty());
    }

    /// The local frame has its origin at the cell's **north-west** corner with +z
    /// running south — the same frame the tessellator hands the mesh builders. Get
    /// the z direction wrong and every surveyed object in the country is mirrored
    /// about the middle of its cell, which reads as "the trees are in the road".
    #[test]
    fn cell_local_puts_north_west_at_the_origin() {
        let (cx, cy) = (8490, 5320);
        let (north, west) = tile_to_lat_lng(cx, cy, BGT_ZOOM);
        let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, BGT_ZOOM);
        let tile_m = 1500.0;

        let nw = to_cell_local(north - 1e-9, west + 1e-9, cx, cy, tile_m).expect("inside");
        assert!(
            nw.0 < 1.0 && nw.1 < 1.0,
            "north-west should be near (0,0): {nw:?}"
        );

        let se = to_cell_local(south + 1e-9, east - 1e-9, cx, cy, tile_m).expect("inside");
        assert!(
            se.0 > tile_m - 1.0 && se.1 > tile_m - 1.0,
            "south-east should be near (tile_m, tile_m): {se:?}"
        );
    }

    #[test]
    fn a_point_outside_the_cell_is_dropped() {
        let (cx, cy) = (8490, 5320);
        let (north, west) = tile_to_lat_lng(cx, cy, BGT_ZOOM);
        assert!(to_cell_local(north + 0.01, west + 0.001, cx, cy, 1500.0).is_none());
        assert!(to_cell_local(north - 0.001, west - 0.01, cx, cy, 1500.0).is_none());
    }
}
