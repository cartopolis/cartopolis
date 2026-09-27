//! The streaming half of the crop parcels: the coverage manifest, the per-cell
//! file, and the projection into the frame the meshes are built in.
//!
//! The pure half — what a parcel is, what colour a crop is, and how a cell's
//! parcels become byte-bounded meshes — is `cartopolis_geo::crops`, and
//! `docs/notes/crop-parcels.md` has the endpoint, the licence and the
//! measurements behind both.
//!
//! There is no streamer here and no resource, for the same reason
//! `systems::surveyed` has none: a parcel ends up merged into its cell's meshes,
//! so it has to be in hand on the worker that builds them rather than arriving
//! later on a schedule of its own. `map_geometry` awaits it beside the surveyed
//! paving.
//!
//! The extractor half is atlas's `pipeline/brp.rs`. This side went first — the
//! same order the BGT's second round of collections used: a client that can
//! name a form the host never sends is harmless; an extractor that sends one no
//! client knows is silently dropped content.

use cartopolis_geo::crops::{CropCategory, CropParcel};

use crate::platform::cell_source::CellSource;
use crate::systems::coverage::CoverageSlot;
use crate::systems::osm_buildings::tdbag_url;
use crate::systems::surveyed::cell_local_unclamped;

/// Fixed cache/streaming zoom — must match `map_geometry::GEOMETRY_ZOOM` and the
/// zoom the extractor bins into.
const BRP_ZOOM: u32 = 14;

/// Which cells the host serves crop parcels for.
static COVERAGE: CoverageSlot = CoverageSlot::new("brp", "/brp/coverage.json", BRP_ZOOM);

/// Keep the coverage manifest fetched. Registered beside
/// `surveyed::prefetch_bgt_coverage`, and a system of its own for the reason that
/// one gives: the natural caller is `map_geometry::update_map_surfaces`, which
/// sits on Bevy's parameter ceiling and holds no clock.
pub fn prefetch_brp_coverage(time: bevy::prelude::Res<bevy::prelude::Time>) {
    COVERAGE.ensure(time.elapsed_secs_f64());
}

/// 0 while the coverage question is open, 1 once answered — read by
/// `map_geometry` as a rebuild revision, since parcels are baked into its merged
/// meshes. Can only change once per process.
pub(crate) fn coverage_revision() -> u32 {
    COVERAGE.revision()
}

/// The BRP attribution line, once coverage has loaded and is non-empty.
pub fn brp_attribution() -> Option<String> {
    COVERAGE.attribution()
}

/// The categories the extractor can name, in wire order. The BRP extractor
/// (`CATEGORIES` in atlas's `pipeline/brp.rs`) mirrors this list — **append,
/// never renumber**.
///
/// Position 5 is the register's `Landschapselement` and is deliberately **not
/// drawable**. Sixty per cent of the features in the register are landscape
/// elements and most of those are ditches, which the Shortbread water layers
/// already draw — filling one with land puts ground where the register says
/// water. The extractor drops them; the position is claimed here so nothing can
/// ever quietly re-use it for a form that *is* drawn, and a record that arrives
/// in it is dropped rather than drawn as the category next to it.
const CATEGORIES: [Option<CropCategory>; 6] = [
    Some(CropCategory::Grassland),
    Some(CropCategory::Arable),
    Some(CropCategory::Nature),
    Some(CropCategory::Fallow),
    Some(CropCategory::Other),
    None,
];

/// One parcel on the wire: its category, its `gewascode`, and its rings as
/// lat/lng — outer first, holes after.
#[derive(serde::Deserialize)]
struct WireParcel(usize, u32, Vec<Vec<(f64, f64)>>);

/// One cell's crop parcels, still in lat/lng as the host sent them.
#[derive(Clone, Debug, Default)]
pub struct CropCell {
    pub parcels: Vec<(CropCategory, u32, Vec<Vec<(f64, f64)>>)>,
}

/// Parse a cell's crop file: `{"parcels":[[category,code,[ring,…]],…]}`.
pub fn parse_crop_cell(text: &str) -> Result<CropCell, String> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        parcels: Vec<WireParcel>,
    }
    let wire: Wire = serde_json::from_str(text).map_err(|e| e.to_string())?;
    // Every parcel shares one rung of the depth ladder, so two copies of one
    // polygon are a per-pixel z-fight that re-rolls with the view matrix. The
    // register serves one year with no superseded versions in it — unlike the
    // BGT, which is where this fold was first needed (`parse_surveyed_paving`) —
    // but a cached cell outlives the extractor that wrote it, and the floor costs
    // one hash set.
    let mut seen = std::collections::HashSet::new();
    Ok(CropCell {
        parcels: wire
            .parcels
            .into_iter()
            // An outer ring under three points is not a parcel. The extractor
            // drops those; a truncated cache entry can produce one.
            .filter(|p| p.2.first().is_some_and(|r| r.len() >= 3))
            .filter(|WireParcel(c, code, rings)| {
                seen.insert((
                    *c,
                    *code,
                    rings
                        .iter()
                        .map(|r| {
                            r.iter()
                                .map(|&(lat, lng)| (lat.to_bits(), lng.to_bits()))
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>(),
                ))
            })
            .filter_map(|WireParcel(c, code, rings)| {
                // A category this build has no variant for — the reserved
                // landscape-element position, or a position from an extractor
                // newer than this client — drops the parcel. Clamping to the last
                // known category would paint a ditch as farmland.
                let category = (*CATEGORIES.get(c)?)?;
                Some((category, code, rings))
            })
            .collect(),
    })
}

/// This cell's parcels in the local metre frame, ready to triangulate.
///
/// **Projected unclamped.** Where the surveyed paving drops any surface whose
/// outer ring leaves its cell, the extractor cuts a field crossing a cell edge
/// along it (atlas's `brp::pieces`) — 22 % of a Groningen farmland cell's
/// parcels cross one, 24 % over Flevoland, measured 2026-08-30 — so each cell
/// holds exactly the ground it covers and cuts its own grass against all of it
/// (`crops::cut_into_ground`). A cell written by an older extractor, which binned
/// whole parcels on their centroid, still draws: its overhang lands over the
/// neighbour, as it always did.
#[must_use]
pub fn crop_parcels(cell: &CropCell, cx: i64, cy: i64, tile_m: f32) -> Vec<CropParcel> {
    let to_local = |pts: &Vec<(f64, f64)>| -> Option<Vec<[f32; 2]>> {
        (pts.len() >= 3).then(|| {
            pts.iter()
                .map(|&(lat, lng)| {
                    let (x, z) = cell_local_unclamped(lat, lng, cx, cy, BRP_ZOOM, tile_m);
                    [x, z]
                })
                .collect()
        })
    };
    cell.parcels
        .iter()
        .filter_map(|(category, code, rings)| {
            let outer = to_local(rings.first()?)?;
            let mut local = vec![outer];
            local.extend(rings[1..].iter().filter_map(to_local));
            Some(CropParcel {
                category: *category,
                code: *code,
                seed: seed_of(rings),
                rings: local,
            })
        })
        .collect()
}

/// Fetch and parse one cell's parcels, through the storage cache.
///
/// An uncovered cell asks nothing and answers empty; so does an unreachable one,
/// because the fallback is the generic green the map already draws.
fn cache_key(cx: i64, cy: i64) -> String {
    format!("brp/v1/{BRP_ZOOM}/{cx}/{cy}.json")
}

const PARCELS: CellSource<CropCell> = CellSource {
    label: "brp parcels",
    gate: Some(|cx, cy| COVERAGE.covers(cx, cy)),
    key: cache_key,
    url: Some(|cx, cy| format!("{}/brp/{BRP_ZOOM}/{cx}/{cy}.json", tdbag_url())),
    breaker: None,
    timeout: None,
    parse: |text| parse_crop_cell(text).ok(),
};

pub(crate) async fn load_crop_cell(cx: i64, cy: i64) -> CropCell {
    PARCELS.load(cx, cy).await
}

/// Coarsest source zoom the basemap paints fields into. A z12 tile is sixteen
/// parcel cells, ~6 km across, where a field is still a few pixels of a 512 px
/// raster; one level up is 64 cells for a mosaic of one-pixel specks, and those
/// tiles are only drawn from high enough that the countryside reads as one tone
/// anyway.
const RASTER_MIN_ZOOM: u32 = 12;

/// How long the basemap holds its first tiles for the coverage answer. A tile
/// drawn before it is drawn without fields and keeps that picture — on disk as
/// well — until it next leaves the ring, so the wait is the same one
/// `map_geometry` gives its cells (`COVERAGE_WAIT_S`), for the same reason.
const RASTER_WAIT_S: f32 = 3.0;

/// Whether the basemap may start drawing: the coverage has answered, or it has
/// been waited on long enough. Only ever holds at start-up.
pub(crate) fn raster_ready() -> bool {
    static SINCE: std::sync::OnceLock<bevy::platform::time::Instant> = std::sync::OnceLock::new();
    COVERAGE.revision() != 0
        || SINCE
            .get_or_init(bevy::platform::time::Instant::now)
            .elapsed()
            .as_secs_f32()
            > RASTER_WAIT_S
}

/// The parcels over one basemap source tile, in that tile's unit square and in
/// each crop's tone — what `tile_loader` hands the rasteriser to paint over the
/// land (`vector_tiles::render_mvt_overlaid`), so the fields keep their colour
/// above the altitude where the ground stops being meshed.
///
/// Empty below [`RASTER_MIN_ZOOM`], and for every cell the host does not cover.
pub(crate) async fn raster_overlay(
    source: cartopolis_core::TileKey,
) -> Vec<cartopolis_geo::vector_tiles::OverlayFill> {
    if source.z < RASTER_MIN_ZOOM || source.z > BRP_ZOOM {
        return Vec::new();
    }
    let shift = BRP_ZOOM - source.z;
    let (x0, y0) = (source.x << shift, source.y << shift);
    let span = 1i64 << shift;
    let mut out = Vec::new();
    for cx in x0..x0 + span {
        for cy in y0..y0 + span {
            if !COVERAGE.covers(cx, cy) {
                continue;
            }
            let cell = load_crop_cell(cx, cy).await;
            out.extend(overlay_fills(&cell, source));
        }
    }
    out
}

/// One cell's parcels as fills in `source`'s unit square.
fn overlay_fills(
    cell: &CropCell,
    source: cartopolis_core::TileKey,
) -> impl Iterator<Item = cartopolis_geo::vector_tiles::OverlayFill> + '_ {
    let to_unit = move |&(lat, lng): &(f64, f64)| {
        let (fx, fy) = cartopolis_core::world::lat_lng_to_tile_frac(lat, lng, source.z);
        [(fx - source.x as f64) as f32, (fy - source.y as f64) as f32]
    };
    cell.parcels.iter().map(move |(category, code, rings)| {
        cartopolis_geo::vector_tiles::OverlayFill {
            rings: rings
                .iter()
                .map(|r| r.iter().map(to_unit).collect())
                .collect(),
            rgb: cartopolis_geo::crops::field_rgb(*category, *code, seed_of(rings)),
        }
    })
}

/// A piece's [`cartopolis_geo::crops::field_seed`], off its first outer vertex
/// as the host sent it — the one input the mesh and the basemap share.
fn seed_of(rings: &[Vec<(f64, f64)>]) -> u32 {
    rings
        .first()
        .and_then(|r| r.first())
        .map_or(0, |&(lat, lng)| cartopolis_geo::crops::field_seed(lat, lng))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::world::tile_to_lat_lng;

    /// A ring around the middle of a cell, `d` degrees on a side.
    fn ring(lat: f64, lng: f64, d: f64) -> String {
        format!(
            "[[{lat},{lng}],[{lat},{}],[{},{}],[{},{lng}]]",
            lng + d,
            lat + d,
            lng + d,
            lat + d
        )
    }

    fn middle_of(cx: i64, cy: i64) -> (f64, f64) {
        let (north, west) = tile_to_lat_lng(cx, cy, BRP_ZOOM);
        let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, BRP_ZOOM);
        ((north + south) / 2.0, (west + east) / 2.0)
    }

    #[test]
    fn parses_a_cell_and_maps_its_categories() {
        let (lat, lng) = middle_of(8449, 5374);
        let cell = parse_crop_cell(&format!(
            r#"{{"parcels":[[1,2014,[{}]],[0,265,[{}]]]}}"#,
            ring(lat, lng, 1e-4),
            ring(lat + 0.001, lng, 1e-4)
        ))
        .expect("valid cell");
        assert_eq!(cell.parcels[0].0, CropCategory::Arable);
        assert_eq!(cell.parcels[0].1, 2014);
        assert_eq!(cell.parcels[1].0, CropCategory::Grassland);
    }

    /// A ditch is water, and the water layers already draw it. The reserved
    /// landscape-element position must produce no parcel at all — drawing one
    /// puts land where the register says there is none.
    #[test]
    fn a_landscape_element_produces_no_parcel() {
        let (lat, lng) = middle_of(8449, 5374);
        let cell = parse_crop_cell(&format!(
            r#"{{"parcels":[[5,0,[{}]],[1,2014,[{}]]]}}"#,
            ring(lat, lng, 1e-4),
            ring(lat + 0.001, lng, 1e-4)
        ))
        .expect("valid cell");
        assert_eq!(cell.parcels.len(), 1);
        assert_eq!(cell.parcels[0].0, CropCategory::Arable);
    }

    /// Same rule one position further out: a host running a newer extractor sends
    /// a category this build has no tone for, and dropping it beats drawing it as
    /// whatever happens to sit at the end of the table.
    #[test]
    fn an_unknown_category_is_dropped_not_clamped() {
        let (lat, lng) = middle_of(8449, 5374);
        let cell = parse_crop_cell(&format!(
            r#"{{"parcels":[[99,2014,[{}]]]}}"#,
            ring(lat, lng, 1e-4)
        ))
        .expect("valid cell");
        assert!(cell.parcels.is_empty());
    }

    #[test]
    fn a_degenerate_parcel_is_dropped() {
        let cell =
            parse_crop_cell(r#"{"parcels":[[1,2014,[[[53.2,6.5],[53.2,6.6]]]],[0,265,[[]]]]}"#)
                .expect("valid cell");
        assert!(cell.parcels.is_empty());
    }

    #[test]
    fn a_repeated_parcel_is_kept_once() {
        let (lat, lng) = middle_of(8449, 5374);
        let r = ring(lat, lng, 1e-4);
        let other = ring(lat + 0.001, lng, 1e-4);
        let cell = parse_crop_cell(&format!(
            r#"{{"parcels":[[1,2014,[{r}]],[1,2014,[{r}]],[1,2014,[{other}]]]}}"#
        ))
        .expect("valid cell");
        assert_eq!(cell.parcels.len(), 2, "the repeat collapses");
    }

    /// The same geometry under a *different* crop is not a repeat — a field can
    /// genuinely be re-registered — so only exact copies fold.
    #[test]
    fn the_same_ground_under_a_different_crop_is_not_a_repeat() {
        let (lat, lng) = middle_of(8449, 5374);
        let r = ring(lat, lng, 1e-4);
        let cell =
            parse_crop_cell(&format!(r#"{{"parcels":[[1,2014,[{r}]],[1,233,[{r}]]]}}"#)).unwrap();
        assert_eq!(cell.parcels.len(), 2);
    }

    #[test]
    fn an_empty_cell_is_empty_rather_than_an_error() {
        assert!(parse_crop_cell("{}").expect("valid").parcels.is_empty());
    }

    /// The local frame has its origin at the cell's **north-west** corner with +z
    /// running south, the same frame the tessellator hands the mesh builders. Get
    /// z the wrong way round and every field in the country is mirrored about the
    /// middle of its cell.
    #[test]
    fn cell_local_puts_north_west_at_the_origin() {
        let (cx, cy) = (8449, 5374);
        let (north, west) = tile_to_lat_lng(cx, cy, BRP_ZOOM);
        let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, BRP_ZOOM);
        let tile_m = 1500.0;
        let nw = cell_local_unclamped(north, west, cx, cy, BRP_ZOOM, tile_m);
        assert!(nw.0.abs() < 1.0 && nw.1.abs() < 1.0, "{nw:?}");
        let se = cell_local_unclamped(south, east, cx, cy, BRP_ZOOM, tile_m);
        assert!(se.0 > tile_m - 1.0 && se.1 > tile_m - 1.0, "{se:?}");
    }

    /// The measurement this whole layer's binning rests on: a third of Flevoland's
    /// fields cross a cell boundary, so a parcel that leaves its cell is drawn
    /// whole — every vertex kept, none clipped and none clamped to the edge.
    #[test]
    fn a_parcel_straddling_the_boundary_survives_with_all_its_vertices() {
        let (cx, cy) = (8449, 5374);
        let (north, west) = tile_to_lat_lng(cx, cy, BRP_ZOOM);
        let (south, east) = tile_to_lat_lng(cx + 1, cy + 1, BRP_ZOOM);
        let (mid_lat, mid_lng) = ((north + south) / 2.0, (west + east) / 2.0);
        let tile_m = 1500.0;
        // Centroid inside the cell, north edge well outside it.
        let straddler = vec![
            (mid_lat, mid_lng),
            (mid_lat, mid_lng + 0.002),
            (north + 0.004, mid_lng + 0.002),
            (north + 0.004, mid_lng),
        ];
        let cell = CropCell {
            parcels: vec![(CropCategory::Arable, 2014, vec![straddler.clone()])],
        };
        let built = crop_parcels(&cell, cx, cy, tile_m);
        assert_eq!(built.len(), 1, "the straddling parcel was dropped");
        assert_eq!(
            built[0].rings[0].len(),
            straddler.len(),
            "vertices were lost at the boundary"
        );
        // …and the outside ones really are outside, i.e. nothing clamped them.
        assert!(
            built[0].rings[0].iter().any(|p| p[1] < 0.0),
            "the northern edge should project to a negative z"
        );
        // The east/west extent is unchanged, so the mesh is the field's own shape.
        assert!(built[0].rings[0]
            .iter()
            .all(|p| p[0] > 0.0 && p[0] < tile_m));
        assert!(!cartopolis_geo::crops::build_crop_meshes(&built, None).is_empty());
    }
}
