//! Pre-cut the AHN height tiles over an area into atlas's terrain store
//! (`crate::terrain`), so the client's ground over a covered city is never cut
//! from PDOK on a player's first visit (`docs/notes/level-of-detail-tile-tree.md`,
//! step 2a).
//!
//! The HTTP route cuts on demand and keeps the result; this fills the same store
//! ahead of time. It does not write per-cell files into the site tree like the
//! other datasets, because atlas serves these tiles itself — only the coverage
//! manifest goes there, so the web UI can say what is covered.
//!
//! **Three tiles at a time** (`Ctx::upstream`), visited in order. It
//! was one, when a tile was one WCS call of a second or two; since AHN6 a tile is
//! also the download of the ~9 MB sheets under it, static files off a download
//! server, and one at a time was most of the 43 s Groningen's 49 tiles took
//! (2026-09-24). Two tiles wanting the same sheet share one download
//! (`crate::ahn6`).

use cartopolis_geo::formats::coverage::{CellRef, Manifest};
use cartopolis_geo::terrain_tile::DENSE_GRID;

use super::rd::wgs84_bbox;
use super::{in_order, Count, Ctx, DatasetResult, Outcome, PipelineError, Progress};
use crate::terrain::{precut, HeightSource, Pdok, Precut};

/// The tiles the client assembles its field from: z14 at the dense grid, ~1.47 km
/// a tile at 53°N with its 513 samples ~2.9 m apart — the 3.125 m the field is
/// sampled at, in ~25 requests a field rather than ~360 z16 ones.
pub const ZOOM: u32 = 14;

const ATTRIBUTION: &str = "AHN (Rijkswaterstaat / Het Waterschapshuis), CC0";

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let vector = super::terrain_grade::HttpTiles::from_env();
    // The same source the HTTP route cuts from — AHN6 where it is published —
    // for the ground and the decks alike, or a pre-cut tile and one cut on
    // demand would be two different flights.
    let source: std::sync::Arc<dyn HeightSource> = std::sync::Arc::new(crate::ahn6::Ahn6::new(
        ctx.data_dir()?,
        std::sync::Arc::new(Pdok::new()),
    ));
    let surface = crate::terrain::SurfaceOf(source.clone());
    run_graded(ctx, source.as_ref(), Some((&vector, &surface)))
}

/// Every z14 tile an RD bbox touches, nearest the centre first — so a cancelled
/// run has done the tiles a player standing in the middle would load first.
#[must_use]
pub fn tiles_for(bbox: [f64; 4]) -> Vec<(i64, i64)> {
    let [min_lng, min_lat, max_lng, max_lat] = wgs84_bbox(bbox);
    let (x0, y0) = cartopolis_core::world::lat_lng_to_tile(max_lat, min_lng, ZOOM);
    let (x1, y1) = cartopolis_core::world::lat_lng_to_tile(min_lat, max_lng, ZOOM);
    let (cx, cy) = ((x0 + x1) as f64 / 2.0, (y0 + y1) as f64 / 2.0);
    let mut tiles: Vec<(i64, i64)> = (y0..=y1)
        .flat_map(|y| (x0..=x1).map(move |x| (x, y)))
        .collect();
    tiles.sort_by(|a, b| {
        let d = |t: &(i64, i64)| (t.0 as f64 - cx).powi(2) + (t.1 as f64 - cy).powi(2);
        d(a).total_cmp(&d(b)).then(a.cmp(b))
    });
    tiles
}

/// Cut only, no grading — what the tests drive, with no vector tiles to read.
pub fn run_with(ctx: &Ctx, source: &dyn HeightSource) -> Outcome<DatasetResult> {
    run_graded(ctx, source, None)
}

/// Cut every tile, then grade them (`terrain_grade`) when given the road shapes
/// and the surface model to grade with.
pub fn run_graded(
    ctx: &Ctx,
    source: &dyn HeightSource,
    grading: Option<(&dyn super::terrain_grade::VectorSource, &dyn HeightSource)>,
) -> Outcome<DatasetResult> {
    let data_dir = ctx.data_dir()?.to_path_buf();
    let tiles = tiles_for(ctx.bbox);
    ctx.log(format!("{} tegels op z{ZOOM}…", tiles.len()));
    let (mut stored, mut cut, mut nothing, mut failed) = (0usize, 0usize, 0usize, 0usize);
    let mut measured = Vec::new();
    let mut i = 0usize;
    in_order(
        ctx,
        &tiles,
        ctx.upstream(),
        |&(x, y)| precut(source, &data_dir, ZOOM, x, y, DENSE_GRID),
        |&(x, y), answer| {
            if i % 25 == 0 {
                ctx.progress(Progress {
                    phase: Some("tegels".into()),
                    done: Some(i as u64),
                    total: Some(Some(tiles.len() as u64)),
                });
            }
            i += 1;
            match answer {
                Ok(Precut::Stored) => {
                    stored += 1;
                    measured.push([x, y]);
                }
                Ok(Precut::Cut) => {
                    cut += 1;
                    measured.push([x, y]);
                }
                Ok(Precut::Nothing) => nothing += 1,
                // One tile PDOK would not answer is a tile cut on demand later, not
                // a failed run; a run where most fail is PDOK being down.
                Err(e) => {
                    failed += 1;
                    ctx.log(format!("tegel {ZOOM}/{x}/{y} overgeslagen: {e}"));
                    if failed > 20 && failed * 2 > i {
                        return Err(PipelineError::Failed(format!(
                            "PDOK answered {failed} of {i} tiles with an error; stopping"
                        )));
                    }
                }
            }
            if i % 100 == 0 {
                ctx.log(format!(
                    "{i} / {} tegels ({cut} gesneden, {stored} al aanwezig)",
                    tiles.len()
                ));
            }
            Ok(())
        },
    )?;
    ctx.log(format!(
        "{cut} gesneden, {stored} al aanwezig, {nothing} zonder meting, {failed} mislukt"
    ));
    // Graded after every tile is cut, because each is graded over a window
    // reaching into its neighbours.
    let (graded, decks_lifted) = match grading {
        Some((vector, surface)) => {
            let tiles: Vec<(i64, i64)> = measured.iter().map(|c| (c[0], c[1])).collect();
            let (graded, _, lifted) =
                super::terrain_grade::grade_area(ctx, &tiles, vector, surface)?;
            (graded, lifted)
        }
        None => (Vec::new(), 0),
    };
    // The stamp names this grading run: the client puts it in the tile URL and its
    // cache key, so a re-grade is never answered from an old copy.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let n_measured = measured.len();
    ctx.write_coverage(
        "terrain/coverage.json",
        &Manifest {
            graded: Some(graded.iter().copied().map(CellRef::from).collect()),
            graded_stamp: Some(stamp),
            ..Manifest::new(ZOOM, measured.iter().copied()).described(
                ctx.name.as_str(),
                ATTRIBUTION,
                "PDOK AHN WCS dtm_05m, cut to z14 tiles at 513 samples",
                ctx.bbox,
            )
        }
        .with("tile_count", n_measured),
    )?;
    Ok(DatasetResult {
        cells: measured.len(),
        counts: vec![
            Count::new("tiles cut", cut),
            Count::new("tiles already stored", stored),
            Count::new("tiles with no data", nothing),
            Count::new("tiles graded", graded.len()),
            Count::new("bridge decks lifted", decks_lifted),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{Progress as P, Sink};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Quiet;
    impl Sink for Quiet {
        fn log(&self, _: String) {}
        fn progress(&self, _: P) {}
    }

    struct Flat(AtomicUsize);
    impl HeightSource for Flat {
        fn raster(&self, _: [f64; 2], _: [f64; 2], n: usize) -> Result<Vec<f32>, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(vec![1.0; n * n])
        }
    }

    #[test]
    fn the_tiles_cover_the_box_and_start_in_the_middle() {
        // Groningen's 9 km box: ~6–8 z14 tiles a side.
        let bbox = [229_000.0, 578_000.0, 238_000.0, 587_000.0];
        let tiles = tiles_for(bbox);
        assert!((25..=81).contains(&tiles.len()), "{}", tiles.len());
        let xs: Vec<i64> = tiles.iter().map(|t| t.0).collect();
        let ys: Vec<i64> = tiles.iter().map(|t| t.1).collect();
        let mid = (
            (xs.iter().min().unwrap() + xs.iter().max().unwrap()) as f64 / 2.0,
            (ys.iter().min().unwrap() + ys.iter().max().unwrap()) as f64 / 2.0,
        );
        let d = |t: (i64, i64)| (t.0 as f64 - mid.0).abs() + (t.1 as f64 - mid.1).abs();
        assert!(d(tiles[0]) <= d(*tiles.last().unwrap()));
    }

    #[test]
    fn a_second_run_cuts_nothing_and_needs_the_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site");
        let data = dir.path().join("data");
        let bbox = [233_000.0, 581_000.0, 233_500.0, 581_500.0];
        let ctx = |with: bool| {
            let c = Ctx::new(
                bbox,
                "t",
                &site,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Quiet),
            );
            if with {
                c.with_data_dir(&data)
            } else {
                c
            }
        };
        assert!(run_with(&ctx(false), &Flat(AtomicUsize::new(0))).is_err());

        let source = Flat(AtomicUsize::new(0));
        let first = run_with(&ctx(true), &source).unwrap();
        let asked = source.0.load(Ordering::SeqCst);
        assert!(asked > 0);
        assert_eq!(first.cells, asked);
        let second = run_with(&ctx(true), &source).unwrap();
        assert_eq!(source.0.load(Ordering::SeqCst), asked, "nothing re-cut");
        assert_eq!(second.counts[1].n as usize, asked);
        assert!(site.join("terrain/coverage.json").exists());
    }
}
