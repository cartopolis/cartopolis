//! `ground`: every cell's flat ground — fills, paving, crops, roads and the gap
//! fill — cut into one partition, so the client does not have to.
//!
//! The cut is `cartopolis_geo::ground_sheets::build_sheets`, the function the
//! client calls, run on the inputs the client reads: the vector tile from the same
//! tile server, and this site's own `bgt` paving, `roofs` measurements, `brp`
//! parcels and `terrain` decks. Each file carries a fingerprint of those inputs
//! and the build's `GROUND_REVISION`; a client whose inputs or build differ
//! ignores the file and cuts the cell itself, so a stale or partial run costs a
//! cell its speed-up and never its ground (`docs/notes/near-ground-layers-at-altitude.md`).
//!
//! Run it after `bgt`, `roofs`, `brp` and `terrain`: it reads what they wrote,
//! and a cell built before one of them ran is fingerprinted on the older inputs
//! and will not match what the client fetches afterwards.

use super::terrain_grade::{HttpTiles, VectorSource};
use super::{Count, Ctx, DatasetResult, Outcome, PipelineError};
use cartopolis_core::world::{tile_frac_to_lat_lng, tile_to_meters};
use cartopolis_geo::formats::roofs;
use cartopolis_geo::ground_sheets;
use cartopolis_geo::surveyed_ground::{self, CropCell, SurveyedPaving};

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let tiles = super::terrain::tiles_for(ctx.bbox);
    let source = HttpTiles::from_env();
    let started = std::time::Instant::now();
    let mut cells = Vec::new();
    let (mut bytes, mut triangles) = (0usize, 0usize);
    ctx.log(format!("{} cellen", tiles.len()));
    for (i, &(x, y)) in tiles.iter().enumerate() {
        ctx.check()?;
        let Some(body) = source
            .mvt(x, y)
            .map_err(|e| PipelineError::Failed(format!("tile {x}/{y}: {e}")))?
        else {
            continue;
        };
        let Some((file, n)) = cell(ctx, x, y, &body)? else {
            continue;
        };
        bytes += file.len();
        triangles += n;
        ctx.write_bytes_gzipped(&format!("ground/14/{x}/{y}.bin"), &file)?;
        cells.push([x, y]);
        if (i + 1) % 10 == 0 {
            ctx.log(format!(
                "{}/{} cellen, {:.0} s",
                i + 1,
                tiles.len(),
                started.elapsed().as_secs_f64()
            ));
        }
    }
    cells.sort_unstable();
    // The run, so the client's cache key moves with it (`CoverageSlot::stamp`).
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    ctx.write_coverage(
        "ground/coverage.json",
        &cartopolis_geo::formats::coverage::Manifest {
            attribution: Some(
                "© OpenStreetMap contributors; BGT (Kadaster), CC0; BRP (RVO), CC0".into(),
            ),
            name: Some(ctx.name.clone()),
            stamp: Some(stamp),
            ..cartopolis_geo::formats::coverage::Manifest::new(14, cells.clone())
        },
    )?;
    ctx.log(format!(
        "{} cellen, {} driehoeken, {:.1} MB, {:.0} s",
        cells.len(),
        triangles,
        bytes as f64 / 1e6,
        started.elapsed().as_secs_f64()
    ));
    Ok(DatasetResult {
        cells: cells.len(),
        counts: vec![
            Count::new("cells", cells.len()),
            Count::new("triangles", triangles),
        ],
    })
}

/// One cell's file and its triangle count, or `None` where its surfaces could not
/// be tessellated.
fn cell(ctx: &Ctx, x: i64, y: i64, body: &[u8]) -> Outcome<Option<(Vec<u8>, usize)>> {
    let text = |path: String| ctx.read_site(&path).and_then(|b| String::from_utf8(b).ok());
    // Each read exactly as the client reads it, and anything missing exactly as
    // the client sees it missing: empty, or no decks at all.
    let paving: SurveyedPaving = text(format!("bgt/14/{x}/{y}.surfaces.json"))
        .and_then(|t| surveyed_ground::parse_surveyed_paving(&t).ok())
        .unwrap_or_default();
    let measured: roofs::Cell = text(format!("roofs/14/{x}/{y}.json"))
        .and_then(|t| roofs::parse(&t).ok())
        .unwrap_or_default();
    let crops: CropCell = text(format!("brp/14/{x}/{y}.json"))
        .and_then(|t| surveyed_ground::parse_crop_cell(&t).ok())
        .unwrap_or_default();
    let decks = text(format!("decks/14/{x}/{y}.json"))
        .and_then(|t| cartopolis_geo::formats::decks::parse(&t).ok())
        .map(surveyed_ground::atlas_decks);
    let tile = match cartopolis_geo::vector_tiles::TileData::parse(body) {
        Ok(t) => t,
        Err(e) => {
            ctx.log(format!("tegel {x}/{y}: {e}"));
            return Ok(None);
        }
    };
    let (lat, _) = tile_frac_to_lat_lng(x as f64 + 0.5, y as f64 + 0.5, 14);
    let tile_m = tile_to_meters(14, lat) as f32;
    let paved = surveyed_ground::surveyed_paving(&paving, &measured, x, y, tile_m);
    let parcels = surveyed_ground::crop_parcels(&crops, x, y, tile_m);
    let meshes = match ground_sheets::build_sheets(
        &tile,
        tile_m,
        None,
        &paved,
        &parcels,
        decks.as_deref(),
    ) {
        Ok(m) => m,
        Err(e) => {
            ctx.log(format!("cel {x}/{y}: {e}"));
            return Ok(None);
        }
    };
    let fingerprint =
        ground_sheets::fingerprint(body, &paving, &measured, &crops, decks.as_deref());
    let triangles = meshes.iter().map(|m| m.indices.len() / 3).sum();
    Ok(Some((
        ground_sheets::encode(ground_sheets::GROUND_REVISION, fingerprint, tile_m, &meshes),
        triangles,
    )))
}
