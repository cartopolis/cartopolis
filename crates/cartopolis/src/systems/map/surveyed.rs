//! Street objects that were **surveyed** rather than guessed: the BGT's trees
//! and its street lighting.
//!
//! `crates/geo/src/scatter.rs` states the gap in its own module note — Shortbread
//! carries canopy *areas*, so "the trees here are scattered, not surveyed" — and
//! `furniture.rs` reads a `pois` layer that has no street lighting in it at all.
//! The BGT registers both, object by object, for the whole country, and
//! atlas's `pipeline/bgt.rs` bins them into the same z14 cells everything else here
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
//! **A city's own register supersedes the trees here.** The BGT's point
//! collections are optional content and the hole they leave is in the middle of
//! a city, not at its edges — over the cell covering the study quay in Groningen
//! this register holds no trees at all and the city's holds 3,018. So where
//! `systems::municipal` has an entry, that entry is the source for trees and the
//! `trees` field below is not read; the props and lines still are, since no city
//! register carries those. `docs/notes/municipal-registers.md` has the counts.
//!
//! There is no streamer here and no resource. The data is consumed inside
//! `map_geometry`'s existing per-cell worker, beside the scatter it joins, for
//! the reason `building_facts` gives about `lod22`: this ends up baked into a
//! merged mesh, so it has to be known on the worker that builds it rather than
//! arriving later on a schedule of its own.

use crate::platform::cell_source::CellSource;
use crate::systems::coverage::CoverageSlot;
use crate::systems::furniture::{FurnitureInstance, FurnitureKind, LineKind, SurveyedLine};
use crate::systems::osm_buildings::tdbag_url;
use crate::systems::paving::{PavingKind, PavingMaterial};
use crate::systems::scatter::TreeInstance;

/// Fixed cache/streaming zoom — the one the extractor bins into. Must also match
/// `map_geometry::GEOMETRY_ZOOM`.
const BGT_ZOOM: u32 = cartopolis_geo::formats::CACHE_ZOOM;

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
/// atlas's `pipeline/bgt.rs` —**append, never renumber**.
///
/// An index past the end is dropped rather than clamped: a host running a newer
/// extractor than this client should lose the objects it has no form for, not
/// draw them as whatever happens to sit at the end of this list.
///
/// Positions 7–9 are claimed by the second round of collections
/// (`docs/notes/surveyed-street-objects.md`, "The other forty-two"): the `bord`,
/// `kast` and `bak` registers. `Cabinet` was already here for `straatmeubilair`,
/// so `kast` needs no position of its own. Positions 10 `BikeRack` and 11
/// `Toilet` are the `fietsenrek` and `openbaar toilet` of `straatmeubilair`.
/// `bord` and `kast` are still not read, so nothing emits 7 — and a client that
/// can name a form the host never sends is the harmless direction of this seam. The reverse is not: an extractor that
/// sends a position no client has is silently dropped content, so the client goes
/// first.
const PROP_KINDS: [FurnitureKind; 12] = [
    FurnitureKind::Lamp,
    FurnitureKind::Bench,
    FurnitureKind::Shelter,
    FurnitureKind::PicnicTable,
    FurnitureKind::Artwork,
    FurnitureKind::Swing,
    FurnitureKind::Cabinet,
    FurnitureKind::Board,
    FurnitureKind::Bin,
    FurnitureKind::Recycling,
    FurnitureKind::BikeRack,
    FurnitureKind::Toilet,
];

/// The linear forms, in wire order. Mirrors `LINE_KINDS` in
/// atlas's `pipeline/bgt.rs` — **append, never renumber**.
const LINE_KINDS: [LineKind; 4] = [
    LineKind::Fence,
    LineKind::Wall,
    LineKind::Hedge,
    LineKind::Quay,
];

// The paving's parse and its local frame live in `cartopolis_geo`, where atlas
// builds a cell's ground sheets with them too (`ground_sheets`).
pub use cartopolis_geo::surveyed_ground::{
    cell_local, cell_local_unclamped, parse_surveyed_paving, surveyed_paving, SurveyedPaving,
};

/// [`cell_local`] at the surveyed layers' zoom.
fn to_cell_local(lat: f64, lng: f64, cx: i64, cy: i64, tile_m: f32) -> Option<(f32, f32)> {
    cell_local(lat, lng, cx, cy, BGT_ZOOM, tile_m)
}

/// One cell's surveyed objects, still in lat/lng as the host sent them.
#[derive(Clone, Debug, Default)]
pub struct SurveyedCell {
    /// Tree positions.
    pub trees: Vec<(f64, f64)>,
    /// Street objects: position and form.
    pub props: Vec<(f64, f64, FurnitureKind)>,
    /// Boundaries — fences, walls, hedges — as the polylines they were surveyed
    /// as, with their form and, where the register measured one, their height in
    /// metres (see [`cartopolis_geo::furniture::SurveyedLine::height_m`]).
    pub lines: Vec<(LineKind, Option<f32>, Vec<(f64, f64)>)>,
    /// The parts bridges are surveyed as — decks, piers, abutments — with their
    /// outlines, outer ring first.
    pub bridges: Vec<SurveyedBridgePart>,
}

/// One part of a bridge as the BGT surveys it (`overbruggingsdeel`).
#[derive(Clone, Debug, PartialEq)]
pub struct SurveyedBridgePart {
    pub part: BridgePart,
    /// The BGT's relative level: 1 for a deck over something, 0 for what stands
    /// on the ground.
    pub level: i8,
    /// Whether the span opens — a bascule, swing or lift bridge.
    pub movable: bool,
    /// `(lat, lng)` rings, outer first.
    pub rings: Vec<Vec<(f64, f64)>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgePart {
    Deck,
    Pier,
    Abutment,
}

/// Wire order of the bridge parts — atlas's `BRIDGE_PARTS`; **append, never
/// renumber**.
const BRIDGE_PARTS: [BridgePart; 3] = [BridgePart::Deck, BridgePart::Pier, BridgePart::Abutment];

/// Parse a cell file: `{"trees":[[lat,lng],…],"props":[[lat,lng,kind],…]}`.
pub fn parse_surveyed_cell(text: &str) -> Result<SurveyedCell, String> {
    // One boundary on the wire: its form, its points, and — only where the 3D
    // product has one — its measured height in metres.
    //
    // **Trailing**, on exactly the same terms as `WireSurface`'s material, and
    // for the same two reasons: most boundaries have no measurement (the 3D file
    // carries `Scheiding_vlak` and no `Scheiding_lijn` at all, so every surveyed
    // *line* is unmeasured), and every cell already cached must keep parsing.
    use cartopolis_geo::formats::bgt::Line as WireLine;
    let wire = cartopolis_geo::formats::bgt::parse_objects(text)?;
    // Exact duplicates are ghost *versions* of one object, same as the paving
    // (see `parse_surveyed_paving`): a doubled tree or fence segment is two
    // identical meshes in one place, z-fighting their own surfaces.
    let bits = |lat: f64, lng: f64| (lat.to_bits(), lng.to_bits());
    let mut seen_tree = std::collections::HashSet::new();
    let mut seen_prop = std::collections::HashSet::new();
    let mut seen_line = std::collections::HashSet::new();
    Ok(SurveyedCell {
        trees: wire
            .trees
            .into_iter()
            .filter(|&(lat, lng)| seen_tree.insert(bits(lat, lng)))
            .collect(),
        props: wire
            .props
            .into_iter()
            .filter(|&(lat, lng, k)| seen_prop.insert((bits(lat, lng), k)))
            .filter_map(|(lat, lng, k)| PROP_KINDS.get(k).map(|&kind| (lat, lng, kind)))
            .collect(),
        lines: wire
            .lines
            .into_iter()
            // A one-point line is not a line. The extractor does not emit them,
            // and a truncated cache entry can.
            .filter(|WireLine(_, pts, _)| pts.len() >= 2)
            .filter(|WireLine(k, pts, _)| {
                // The height is deliberately not in the key: two records of one
                // boundary that differ only in a measured height are still the same
                // ghost pair, and keeping both would draw one wall twice.
                seen_line.insert((
                    *k,
                    pts.iter()
                        .map(|&(lat, lng)| bits(lat, lng))
                        .collect::<Vec<_>>(),
                ))
            })
            .filter_map(|WireLine(k, pts, h)| {
                // An unknown form drops the line; a height that is not a finite
                // positive number drops only the height, leaving the boundary drawn
                // the way it was drawn before heights existed.
                let kind = *LINE_KINDS.get(k)?;
                let height = h.map(|m| m as f32).filter(|m| m.is_finite() && *m > 0.0);
                Some((kind, height, pts))
            })
            .collect(),
        bridges: wire
            .bridges
            .into_iter()
            .filter(|(_, _, _, rings)| rings.first().is_some_and(|r| r.len() >= 3))
            .filter_map(|(k, level, movable, rings)| {
                Some(SurveyedBridgePart {
                    part: *BRIDGE_PARTS.get(k)?,
                    level,
                    movable,
                    rings,
                })
            })
            .collect(),
    })
}

/// Cache keys carry a `v2` the server path does not: nothing ever revalidates a
/// cached cell (the sweep is by size, not age), so when the deployed data was
/// regenerated without the ghost versions the only way an existing install would
/// ever see the fix was a new key. The `v1` entries age out through the ordinary
/// sweep.
fn objects_cache_key(cx: i64, cy: i64) -> String {
    // `v3`: the objects file gained the bridge parts, and a cell cached before
    // would otherwise never show a pier.
    format!("bgt/v3/{BGT_ZOOM}/{cx}/{cy}.json")
}

/// The paving half's key.
fn paving_cache_key(cx: i64, cy: i64) -> String {
    format!("bgt/v2/{BGT_ZOOM}/{cx}/{cy}.surfaces.json")
}

/// Fetch one cell's **paving**, through the storage cache.
///
/// Its own file because it is by far the heaviest part — 1.5 MB against 60 kB of
/// objects for the densest Groningen cell — and the same split, for the same
/// reason, as the BAG's buildings and places.
/// The paving half of a cell, in a file of its own because it is by far the
/// heavier — 1.5 MB against 60 kB of objects for the densest Groningen cell.
const PAVING: CellSource<SurveyedPaving> = CellSource {
    label: "bgt paving",
    gate: Some(|cx, cy| COVERAGE.covers(cx, cy)),
    // `v2` for the reason `objects_cache_key` gives.
    key: paving_cache_key,
    url: Some(|cx, cy| format!("{}/bgt/{BGT_ZOOM}/{cx}/{cy}.surfaces.json", tdbag_url())),
    // Our own host: a failure here is our problem, not a third party's, and the
    // coverage manifest already answers "does this host serve it at all".
    breaker: None,
    timeout: None,
    parse: |text| parse_surveyed_paving(text).ok(),
};

pub(crate) async fn load_surveyed_paving(cx: i64, cy: i64) -> SurveyedPaving {
    PAVING.load(cx, cy).await
}

/// Fetch and parse one cell, through the storage cache.
///
/// An uncovered cell asks nothing and answers empty; so does an unreachable one,
/// because every consumer's fallback is the generated layer it already had.
const OBJECTS: CellSource<SurveyedCell> = CellSource {
    label: "bgt objects",
    gate: Some(|cx, cy| COVERAGE.covers(cx, cy)),
    key: objects_cache_key,
    url: Some(|cx, cy| format!("{}/bgt/{BGT_ZOOM}/{cx}/{cy}.json", tdbag_url())),
    breaker: None,
    timeout: None,
    parse: |text| parse_surveyed_cell(text).ok(),
};

/// This cell's surveyed piers as outer rings in the tile-local metre frame.
///
/// Kept whole even where a ring leaves the cell — a pier is a few metres across
/// and the cell whose first vertex it is draws it.
#[must_use]
pub fn surveyed_piers(cell: &SurveyedCell, cx: i64, cy: i64, tile_m: f32) -> Vec<Vec<[f32; 2]>> {
    cell.bridges
        .iter()
        .filter(|b| b.part == BridgePart::Pier)
        .filter_map(|b| {
            let ring = b.rings.first()?;
            Some(
                ring.iter()
                    .map(|&(lat, lng)| {
                        let (x, z) = cell_local_unclamped(lat, lng, cx, cy, BGT_ZOOM, tile_m);
                        [x, z]
                    })
                    .collect(),
            )
        })
        .collect()
}

pub(crate) async fn load_surveyed_cell(cx: i64, cy: i64) -> SurveyedCell {
    OBJECTS.load(cx, cy).await
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
    // No measured height: the BGT registers a tree as a point and says nothing
    // about how big it is. A city's own register does, which is what
    // `systems::municipal` carries the third element for.
    let points: Vec<(f32, f32, Option<f32>)> = cell
        .trees
        .iter()
        .filter_map(|&(lat, lng)| {
            to_cell_local(lat, lng, cx, cy, tile_m).map(|(x, z)| (x, z, None))
        })
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
        .filter_map(|(kind, height_m, pts)| {
            let local: Vec<[f32; 2]> = pts
                .iter()
                .filter_map(|&(lat, lng)| {
                    to_cell_local(lat, lng, cx, cy, tile_m).map(|(x, z)| [x, z])
                })
                .collect();
            (local.len() == pts.len() && local.len() >= 2).then_some(SurveyedLine {
                points: local,
                kind: *kind,
                height_m: *height_m,
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
    // No paint: the BGT registers a street object's position and form and says
    // nothing about its colour. A city's own register sometimes does — a bottle
    // bank's fraction — which is what `systems::municipal` fills the fourth
    // element for.
    let points: Vec<(f32, f32, FurnitureKind, Option<u32>)> = cell
        .props
        .iter()
        .filter_map(|&(lat, lng, kind)| {
            to_cell_local(lat, lng, cx, cy, tile_m).map(|(x, z)| (x, z, kind, None))
        })
        .collect();
    crate::systems::furniture::place_surveyed(tile, tile_m, seed, &points)
}

/// Shallowest basemap source the survey is painted into. A z12 source tile
/// spans sixteen cells of multi-megabyte paving for a raster on which a street is
/// a pixel; it is drawn from high enough that the paving is one tone anyway, and
/// the meshes hand over at z13–z14 distances.
const RASTER_MIN_ZOOM: u32 = 13;

/// How long the basemap holds its first tiles for the BGT's coverage answer —
/// the crops' wait, for the crops' reason (`crops::RASTER_WAIT_S`): a tile drawn
/// before it is drawn without the survey and kept that way, on disk too.
const RASTER_WAIT_S: f32 = 3.0;

/// Whether the basemap may start drawing as far as the survey is concerned.
pub(crate) fn raster_ready() -> bool {
    static SINCE: std::sync::OnceLock<bevy::platform::time::Instant> = std::sync::OnceLock::new();
    (COVERAGE.revision() != 0 && crate::systems::roof_colours::coverage_answered())
        || SINCE
            .get_or_init(bevy::platform::time::Instant::now)
            .elapsed()
            .as_secs_f32()
            > RASTER_WAIT_S
}

/// Recently built surveys, by source tile. Up to 64 overzoomed tiles share one
/// source, and each would otherwise re-read and re-parse every paving file
/// under it — the crops can afford that and a city's paving cannot.
static RASTER_SURVEYS: crate::platform::sync::Mutex<
    Vec<(
        cartopolis_core::TileKey,
        std::sync::Arc<cartopolis_geo::vector_tiles::Survey>,
    )>,
> = crate::platform::sync::Mutex::new(Vec::new());

/// Surveys kept in [`RASTER_SURVEYS`].
const RASTER_SURVEYS_KEPT: usize = 24;

/// The surveyed ground over one basemap source tile, as the basemap paints it
/// (`vector_tiles::Survey`): every paved surface in the colour the meshes give it
/// from the air (`paving::far_rgb`), and for each covered cell which OSM ribbons
/// the survey replaces — the same per-cell decision `map_geometry` takes.
///
/// **This is what keeps the ground one picture across the hand-over.** Above
/// `FLAT_GROUND_OFF_M` the meshes go and this image is the ground; painted from
/// OSM alone it put a white square where the Grote Markt's brick had been.
///
/// Empty outside z13–z14 and for every cell the host does not cover.
pub(crate) async fn raster_survey(
    source: cartopolis_core::TileKey,
) -> std::sync::Arc<cartopolis_geo::vector_tiles::Survey> {
    use cartopolis_geo::road_texture::SurveyedCover;
    use cartopolis_geo::vector_tiles::{CoveredCell, OverlayFill, Survey};
    if source.z < RASTER_MIN_ZOOM || source.z > BGT_ZOOM {
        return std::sync::Arc::default();
    }
    let lock = || {
        RASTER_SURVEYS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if let Some((_, s)) = lock().iter().find(|(k, _)| *k == source) {
        return s.clone();
    }
    let shift = BGT_ZOOM - source.z;
    let (x0, y0) = (source.x << shift, source.y << shift);
    let span = 1i64 << shift;
    let mut survey = Survey::default();
    for cx in x0..x0 + span {
        for cy in y0..y0 + span {
            if !COVERAGE.covers(cx, cy) {
                continue;
            }
            let paving = load_surveyed_paving(cx, cy).await;
            if paving.areas.is_empty() {
                continue;
            }
            let measured = crate::systems::roof_colours::load_roof_cell(cx, cy).await;
            // The rest is arithmetic over every vertex of the cell — a projection
            // each, a formatted key per area — and freeing both parsed files, which
            // are megabytes of maps. On a worker (`workers::compute`): on the web
            // this future runs on the thread that draws the frame, and doing it here
            // was most of a basemap tile's main-thread time (2026-10-16).
            let s = span as f32;
            let (u, v) = ((cx - x0) as f32 / s, (cy - y0) as f32 / s);
            let built = crate::platform::workers::compute("survey", move || {
                let to_unit = |&(lat, lng): &(f64, f64)| {
                    let (fx, fy) = cartopolis_core::world::lat_lng_to_tile_frac(lat, lng, source.z);
                    [(fx - source.x as f64) as f32, (fy - source.y as f64) as f32]
                };
                // `map_geometry`'s choice, made on the same file.
                let cover = if paving
                    .areas
                    .iter()
                    .any(|(k, _, _)| matches!(k, PavingKind::Street | PavingKind::LivingStreet))
                {
                    SurveyedCover::Streets
                } else {
                    SurveyedCover::Paths
                };
                let cell = CoveredCell {
                    bounds: [u, v, u + 1.0 / s, v + 1.0 / s],
                    cover,
                };
                let mut fills = Vec::new();
                for (kind, material, rings) in &paving.areas {
                    let Some(&(lat, lng)) = rings.first().and_then(|r| r.first()) else {
                        continue;
                    };
                    let key = cartopolis_geo::formats::roofs::paving_key(lat, lng);
                    // A split garden is painted as its two halves here too, or the
                    // distant image would show every garden as paving while the
                    // meshes up close show it planted.
                    if *kind == PavingKind::Garden {
                        if let Some(split) = measured.gardens.get(&key) {
                            for (polys, material) in [
                                (&split.planted, Some(PavingMaterial::Grass)),
                                (&split.paved, None),
                            ] {
                                for poly in polys {
                                    fills.push(OverlayFill {
                                        rings: poly
                                            .iter()
                                            .map(|r| r.iter().map(to_unit).collect())
                                            .collect(),
                                        rgb: cartopolis_geo::paving::far_rgb(*kind, material, None),
                                    });
                                }
                            }
                            continue;
                        }
                    }
                    let measured = measured.paving.get(&key).map(|m| {
                        u32::from(m.c[0]) << 16 | u32::from(m.c[1]) << 8 | u32::from(m.c[2])
                    });
                    fills.push(OverlayFill {
                        rings: rings
                            .iter()
                            .map(|r| r.iter().map(to_unit).collect())
                            .collect(),
                        rgb: cartopolis_geo::paving::far_rgb(*kind, *material, measured),
                    });
                }
                (cell, fills)
            })
            .await;
            if let Some((cell, fills)) = built {
                survey.cells.push(cell);
                survey.paving.extend(fills);
            }
        }
    }
    let survey = std::sync::Arc::new(survey);
    let mut kept = lock();
    if kept.len() >= RASTER_SURVEYS_KEPT {
        kept.remove(0);
    }
    kept.push((source, survey.clone()));
    survey
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::world::tile_to_lat_lng;

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
        assert_eq!(cell.lines[1].2.len(), 3);
        assert!(
            cell.lines.iter().all(|l| l.1.is_none()),
            "a two-element line says nothing about height"
        );
    }

    /// The measured height is a **trailing** element, so both shapes have to read:
    /// every BGT cell already cached was written before heights existed, and the 3D
    /// product covers only some of the boundaries in the ones written after.
    #[test]
    fn a_boundary_may_carry_a_measured_height() {
        let cell = parse_surveyed_cell(
            r#"{"lines":[[1,[[53.2194,6.5665],[53.2195,6.5666]],2.06],
                         [1,[[53.2196,6.5667],[53.2197,6.5668]]],
                         [1,[[53.2198,6.5669],[53.2199,6.5670]],0.0]]}"#,
        )
        .expect("valid cell");
        assert_eq!(cell.lines[0].1, Some(2.06), "the measurement is carried");
        assert_eq!(
            cell.lines[1].1, None,
            "the old two-element form still reads"
        );
        assert_eq!(
            cell.lines[2].1, None,
            "a non-positive height drops the height, not the boundary"
        );
        assert_eq!(cell.lines.len(), 3, "and never the boundary itself");
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
    /// Bridge parts read back in the extractor's order; an unknown part is
    /// dropped, and a cell file from before bridges still parses.
    #[test]
    fn bridge_parts_are_read_and_unknown_ones_dropped() {
        let ring = "[[53.2,6.5],[53.2,6.5001],[53.2001,6.5001],[53.2,6.5]]";
        let text = format!(
            r#"{{"trees":[],"bridges":[[1,0,false,[{ring}]],[0,1,true,[{ring}]],[9,0,false,[{ring}]]]}}"#
        );
        let cell = parse_surveyed_cell(&text).unwrap();
        assert_eq!(cell.bridges.len(), 2);
        assert_eq!(cell.bridges[0].part, BridgePart::Pier);
        assert_eq!(cell.bridges[0].level, 0);
        assert!(cell.bridges[1].movable && cell.bridges[1].part == BridgePart::Deck);
        assert!(parse_surveyed_cell(r#"{"trees":[]}"#)
            .unwrap()
            .bridges
            .is_empty());
    }

    #[test]
    fn a_line_leaving_the_cell_is_dropped_whole() {
        let (cx, cy) = (8490, 5320);
        let (north, west) = tile_to_lat_lng(cx, cy, BGT_ZOOM);
        let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, BGT_ZOOM);
        let inside = ((north + south) / 2.0, (west + east) / 2.0);
        let cell = SurveyedCell {
            lines: vec![
                (
                    LineKind::Wall,
                    None,
                    vec![inside, (inside.0 + 1e-5, inside.1)],
                ),
                (LineKind::Wall, None, vec![inside, (north + 0.01, inside.1)]),
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

    /// A repeated record is a ghost *version* of the same object — the BGT API
    /// hands out every superseded version of a surface and the extractor once
    /// kept them all — and every copy past the first is a coplanar z-fight on
    /// the pavement it duplicates. The parse drops exact repeats, because a
    /// cell cached with the ghosts in it never gets refetched on its own.
    #[test]
    fn a_repeated_surface_is_kept_once() {
        let ring = r#"[[[53.2194,6.5665],[53.2195,6.5665],[53.2195,6.5666]]]"#;
        let other = r#"[[[53.2196,6.5667],[53.2197,6.5667],[53.2197,6.5668]]]"#;
        let paving = parse_surveyed_paving(&format!(
            r#"{{"surfaces":[[0,{ring},2],[0,{ring},2],[0,{ring},2],[0,{other},2]]}}"#
        ))
        .expect("valid cell");
        assert_eq!(paving.areas.len(), 2, "three copies collapse to one");
    }

    /// Same kind and geometry with a *different* material is not a repeat — the
    /// register can genuinely re-describe a surface — so only exact copies fold.
    #[test]
    fn a_repeated_ring_with_a_different_material_is_not_a_repeat() {
        let ring = r#"[[[53.2194,6.5665],[53.2195,6.5665],[53.2195,6.5666]]]"#;
        let paving =
            parse_surveyed_paving(&format!(r#"{{"surfaces":[[0,{ring},2],[0,{ring},1]]}}"#))
                .expect("valid cell");
        assert_eq!(paving.areas.len(), 2);
    }

    /// The cell file has the same ghost-version problem: a doubled tree is two
    /// identical canopies z-fighting, a doubled fence line two coplanar boxes.
    #[test]
    fn repeated_trees_props_and_lines_are_kept_once() {
        let cell = parse_surveyed_cell(
            r#"{"trees":[[53.2,6.5],[53.2,6.5],[53.3,6.5]],
                "props":[[53.2,6.5,0],[53.2,6.5,0]],
                "lines":[[0,[[53.2,6.5],[53.2,6.6]]],[0,[[53.2,6.5],[53.2,6.6]]]]}"#,
        )
        .expect("valid cell");
        assert_eq!(cell.trees.len(), 2);
        assert_eq!(cell.props.len(), 1);
        assert_eq!(cell.lines.len(), 1);
    }

    /// The two wire forms have to coexist: the register names a substance for
    /// only about a third of surfaces, and a cell cached before materials existed
    /// carries none at all. Both must parse in the same file.
    #[test]
    fn a_surface_with_and_without_a_material_both_parse() {
        let ring = r#"[[[53.2194,6.5665],[53.2195,6.5665],[53.2195,6.5666]]]"#;
        let paving = parse_surveyed_paving(&format!(
            r#"{{"surfaces":[[0,{ring},2],[0,{ring}],[4,{ring},7]]}}"#
        ))
        .expect("valid cell");
        assert_eq!(paving.areas.len(), 3);
        assert_eq!(paving.areas[0].1, Some(PavingMaterial::Brick));
        assert_eq!(paving.areas[1].1, None, "the short form means unknown");
        assert_eq!(paving.areas[2].1, Some(PavingMaterial::Grass));
    }

    /// An unknown *form* costs the surface; an unknown *material* must cost only
    /// the material. A host running a newer extractor should still pave the
    /// street, in the tone this build already knew.
    #[test]
    fn an_unknown_material_keeps_the_surface() {
        let ring = r#"[[[53.2194,6.5665],[53.2195,6.5665],[53.2195,6.5666]]]"#;
        let paving = parse_surveyed_paving(&format!(r#"{{"surfaces":[[0,{ring},99]]}}"#))
            .expect("valid cell");
        assert_eq!(paving.areas.len(), 1);
        assert_eq!(paving.areas[0].0, PavingKind::Footway);
        assert_eq!(paving.areas[0].1, None);
    }

    /// The material has to survive the trip into the frame the mesh is built in —
    /// a field that parsed and was then dropped on the way to the worker is a
    /// field that changes nothing.
    #[test]
    fn the_material_reaches_the_built_area() {
        let (cx, cy) = (8490, 5320);
        let (north, west) = tile_to_lat_lng(cx, cy, BGT_ZOOM);
        let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, BGT_ZOOM);
        let (mid_lat, mid_lng) = ((north + south) / 2.0, (west + east) / 2.0);
        let paving = SurveyedPaving {
            areas: vec![(
                PavingKind::Footway,
                Some(PavingMaterial::Brick),
                vec![vec![
                    (mid_lat, mid_lng),
                    (mid_lat + 1e-5, mid_lng),
                    (mid_lat + 1e-5, mid_lng + 1e-5),
                ]],
            )],
        };
        let built = surveyed_paving(&paving, &Default::default(), cx, cy, 1500.0);
        assert_eq!(built[0].material, Some(PavingMaterial::Brick));
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
                (PavingKind::Footway, None, vec![inside]),
                (PavingKind::Footway, None, vec![leaves]),
            ],
        };
        let built = surveyed_paving(&paving, &Default::default(), cx, cy, 1500.0);
        assert_eq!(built.len(), 1, "only the wholly-inside surface survives");
    }

    /// The prop table is a **wire order**, shared with an extractor in another
    /// repository that nothing but a person keeps in step. Nobody notices a
    /// renumber by reading the diff — a swapped pair compiles, parses and puts
    /// benches where the lampposts are — so the positions are written out here
    /// once, and moving one has to be done deliberately, twice.
    #[test]
    fn the_prop_wire_positions_are_pinned() {
        assert_eq!(
            PROP_KINDS.to_vec(),
            vec![
                FurnitureKind::Lamp,
                FurnitureKind::Bench,
                FurnitureKind::Shelter,
                FurnitureKind::PicnicTable,
                FurnitureKind::Artwork,
                FurnitureKind::Swing,
                FurnitureKind::Cabinet,
                FurnitureKind::Board,
                FurnitureKind::Bin,
                FurnitureKind::Recycling,
                FurnitureKind::BikeRack,
                FurnitureKind::Toilet,
            ]
        );
        assert_eq!(
            LINE_KINDS.to_vec(),
            vec![
                LineKind::Fence,
                LineKind::Wall,
                LineKind::Hedge,
                LineKind::Quay
            ]
        );
    }

    /// Both directions of the seam, in one file. A cell written before positions
    /// 7–9 existed must read exactly as it did (the old indices cannot have
    /// moved), and a cell written by an extractor newer than this client must
    /// lose only the forms this build has no shape for.
    #[test]
    fn old_and_new_cell_files_both_parse() {
        let old = parse_surveyed_cell(r#"{"props":[[53.2,6.5,0],[53.2,6.5,6]]}"#).expect("valid");
        assert_eq!(old.props[0].2, FurnitureKind::Lamp);
        assert_eq!(old.props[1].2, FurnitureKind::Cabinet);

        let new = parse_surveyed_cell(
            r#"{"props":[[53.2,6.5,7],[53.2,6.5,8],[53.2,6.5,9],
                                                   [53.2,6.5,10],[53.2,6.5,11],
                                                   [53.2,6.5,12]]}"#,
        )
        .expect("valid");
        assert_eq!(
            new.props.len(),
            5,
            "position 12 has no form here and is dropped"
        );
        assert_eq!(new.props[0].2, FurnitureKind::Board);
        assert_eq!(new.props[1].2, FurnitureKind::Bin);
        assert_eq!(new.props[2].2, FurnitureKind::Recycling);
        assert_eq!(new.props[3].2, FurnitureKind::BikeRack);
        assert_eq!(new.props[4].2, FurnitureKind::Toilet);
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
