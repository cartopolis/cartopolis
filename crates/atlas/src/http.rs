//! Atlas's HTTP surface.
//!
//! Public, read-only data is served here with long cache lifetimes and open CORS,
//! because the browser build of the client fetches it cross-origin and a CDN or
//! Caddy may cache it. Nothing on this surface is on the simulator's tick path, and
//! nothing here trusts the network it arrives from (`docs/notes/atlas.md`).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::terrain::{Answer, TerrainError, TerrainService};

/// The media type of a terrain tile body (`cartopolis_geo::terrain_tile`).
pub const TERRAIN_MEDIA_TYPE: &str = "application/vnd.cartopolis.terrain";

#[derive(Clone)]
pub struct AtlasState {
    pub terrain: Arc<TerrainService>,
    /// `/height`.
    pub heights: Arc<crate::terrain::HeightPatches>,
    /// The pipeline surface the web UI drives, `/api/pipeline/*`.
    pub api: crate::api::ApiState,
    /// `/satellites/active.tle`, the mirrored satellite catalogue.
    pub satellites: Arc<crate::satellites::SatelliteMirror>,
    /// `/plans/groningen.json`, the notices and council business on the map.
    pub plans: Arc<crate::plans::PlansMirror>,
}

/// Only the paths Caddy sends here. The site root — the front page, the
/// operator's pages and the players' — is the web UI (`crates/atlas/web`),
/// static files Caddy serves itself.
pub fn router(state: AtlasState) -> Router {
    let api = crate::api::router(state.api.clone());
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/terrain/{z}/{x}/{y}", get(terrain_tile))
        .route("/height", get(height_patch))
        .route(
            "/satellites/active.tle",
            get(crate::satellites::serve).with_state(state.satellites.clone()),
        )
        .route(
            "/plans/groningen.json",
            get(crate::plans::serve).with_state(state.plans.clone()),
        )
        .with_state(state)
        .nest("/api/pipeline", api)
}

/// `/height?coverage=dtm_05m&bbox=x0,y0,x1,y1&grid=n`: PDOK's `GetCoverage`
/// question in the client's own words — RD metres, pixel-edge box, north row
/// first — answered from AHN6 where it is published (`crate::ahn6`).
#[derive(serde::Deserialize)]
struct HeightQuery {
    coverage: String,
    bbox: String,
    grid: usize,
}

async fn height_patch(
    State(state): State<AtlasState>,
    axum::extract::Query(q): axum::extract::Query<HeightQuery>,
) -> Response {
    let cors = || (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
    let bad = |why: &'static str| (StatusCode::BAD_REQUEST, [cors()], why).into_response();
    let Some(model) = crate::terrain::Model::from_coverage(&q.coverage) else {
        return bad("coverage is dtm_05m or dsm_05m");
    };
    let b: Vec<f64> = q
        .bbox
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    let [x0, y0, x1, y1] = b[..] else {
        return bad("bbox is x0,y0,x1,y1 in RD metres");
    };
    match state.heights.patch(model, [x0, y0], [x1, y1], q.grid).await {
        Ok(samples) => match crate::terrain::encode_float_tiff(&samples, q.grid) {
            Ok(tiff) => (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "image/tiff"),
                    // A day: the answer changes only when a new sheet is published.
                    (header::CACHE_CONTROL, "public, max-age=86400"),
                    cors(),
                ],
                tiff,
            )
                .into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, [cors()], e).into_response(),
        },
        Err(None) => bad("grid 2..=4096 over a box up to 10 km a side"),
        Err(Some(e)) => {
            tracing::warn!(error = %e, "height patch failed");
            (
                StatusCode::BAD_GATEWAY,
                [cors()],
                "the height source did not answer",
            )
                .into_response()
        }
    }
}

/// `?grid=513` asks for the dense cut the client's terrain field is assembled from
/// (`cartopolis_geo::terrain_tile::DENSE_GRID`); the default is the tile tree's.
/// `&graded=1` asks for the graded tile (`pipeline::terrain_grade`) and is answered
/// only from what the pipeline wrote. The client adds `&v=` with the grading run's
/// stamp from the coverage manifest, so a re-grade is a new URL to every cache.
#[derive(serde::Deserialize)]
struct Grid {
    grid: Option<usize>,
    graded: Option<u8>,
}

async fn terrain_tile(
    State(state): State<AtlasState>,
    Path((z, x, y)): Path<(u32, i64, i64)>,
    axum::extract::Query(q): axum::extract::Query<Grid>,
) -> Response {
    // A week: a cut tile never changes for a given survey, and a new AHN release
    // is a new cache directory, not an edit.
    let cache = (header::CACHE_CONTROL, "public, max-age=604800");
    let cors = (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
    let grid = q.grid.unwrap_or(cartopolis_geo::terrain_tile::GRID);
    if q.graded == Some(1) {
        return match state.terrain.graded(z, x, y, grid).await {
            Some(Answer::Tile(bytes)) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, TERRAIN_MEDIA_TYPE), cache, cors],
                bytes,
            )
                .into_response(),
            // Not cached: the pipeline may grade it later.
            Some(Answer::Nothing) | None => {
                (StatusCode::NOT_FOUND, [cors], "not graded here").into_response()
            }
        };
    }
    match state.terrain.tile_at(z, x, y, grid).await {
        Ok(Answer::Tile(bytes)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, TERRAIN_MEDIA_TYPE), cache, cors],
            bytes,
        )
            .into_response(),
        // Remembered as firmly as a tile is, so it may be cached as long.
        Ok(Answer::Nothing) => {
            (StatusCode::NOT_FOUND, [cache, cors], "no terrain here").into_response()
        }
        Err(TerrainError::OutOfRange) => (
            StatusCode::BAD_REQUEST,
            [cors],
            "tile outside the served zooms",
        )
            .into_response(),
        // Not cacheable: the next request may succeed.
        Err(TerrainError::Upstream(_)) => (
            StatusCode::BAD_GATEWAY,
            [cors],
            "the height service did not answer",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::HeightSource;

    struct Flat;
    impl HeightSource for Flat {
        fn raster(&self, _: [f64; 2], _: [f64; 2], n: usize) -> Result<Vec<f32>, String> {
            Ok(vec![1.5; n * n])
        }
    }

    struct Down;
    impl HeightSource for Down {
        fn raster(&self, _: [f64; 2], _: [f64; 2], _: usize) -> Result<Vec<f32>, String> {
            Err("503".to_string())
        }
    }

    async fn serve(source: Arc<dyn HeightSource>) -> (String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        struct Nobody;
        impl crate::auth::Authority for Nobody {
            fn admin(&self, _: &str) -> Result<Option<crate::auth::Principal>, String> {
                Ok(None)
            }
        }
        let state = AtlasState {
            terrain: Arc::new(TerrainService::new(source.clone(), dir.path(), 2)),
            heights: Arc::new(crate::terrain::HeightPatches::new(source, 2)),
            api: crate::api::ApiState {
                auth: Arc::new(crate::auth::Auth::new(Box::new(Nobody))),
                runner: crate::jobs::Runner::new(dir.path(), crate::jobs::Registry::new()),
                out_root: dir.path().to_path_buf(),
                designs: std::sync::Arc::new(crate::designs::Designs::new(dir.path(), dir.path())),
                overrides: std::sync::Arc::new(crate::overrides::Overrides::new(
                    dir.path(),
                    dir.path(),
                )),
            },
            satellites: Arc::new(crate::satellites::SatelliteMirror::new(dir.path(), "")),
            plans: Arc::new(crate::plans::PlansMirror::new(dir.path(), false)),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(state)).await });
        (format!("http://{addr}"), dir)
    }

    /// Status, media type and body of a GET, on a blocking thread (`ureq`).
    async fn get(url: String) -> (u16, String, Vec<u8>) {
        tokio::task::spawn_blocking(move || {
            let r = match ureq::get(&url).call() {
                Ok(r) | Err(ureq::Error::Status(_, r)) => r,
                Err(e) => panic!("{e}"),
            };
            let status = r.status();
            let media = r.content_type().to_string();
            let mut body = Vec::new();
            std::io::Read::read_to_end(&mut r.into_reader(), &mut body).unwrap();
            (status, media, body)
        })
        .await
        .unwrap()
    }

    /// Before the first copy has been fetched the satellite route says so, rather
    /// than answering an empty body a client would parse as no satellites.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_satellite_route_is_unavailable_before_a_copy() {
        let (base, _dir) = serve(Arc::new(Flat)).await;
        assert_eq!(get(format!("{base}/satellites/active.tle")).await.0, 503);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_terrain_route_answers_each_case() {
        let (base, _dir) = serve(Arc::new(Flat)).await;

        let (status, media, body) = get(format!("{base}/terrain/14/8490/5318")).await;
        assert_eq!(status, 200);
        assert_eq!(media, TERRAIN_MEDIA_TYPE);
        let tile = cartopolis_geo::terrain_tile::TerrainTile::decode(&body).unwrap();
        assert!(tile.heights.iter().all(|h| (h - 1.5).abs() < 0.01));

        let (status, _, body) = get(format!("{base}/terrain/14/8490/5318?grid=513")).await;
        assert_eq!(status, 200);
        let dense = cartopolis_geo::terrain_tile::TerrainTile::decode(&body).unwrap();
        assert_eq!(dense.grid, cartopolis_geo::terrain_tile::DENSE_GRID);
        assert_eq!(
            get(format!("{base}/terrain/14/8490/5318?grid=7")).await.0,
            400
        );

        let (x, y) = cartopolis_core::lat_lng_to_tile(48.8566, 2.3522, 14);
        assert_eq!(get(format!("{base}/terrain/14/{x}/{y}")).await.0, 404);
        assert_eq!(get(format!("{base}/terrain/25/0/0")).await.0, 400);
        assert_eq!(get(format!("{base}/health")).await.0, 200);
    }

    /// A graded tile is served at the grid the pipeline wrote and, thinned, at the
    /// tile tree's — the handset's — and nowhere it was not graded.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_graded_tile_is_served_dense_and_thinned() {
        use cartopolis_geo::terrain_tile::{TerrainTile, DENSE_GRID, GRID};
        let (base, dir) = serve(Arc::new(Flat)).await;
        let dense = TerrainTile {
            grid: DENSE_GRID,
            heights: (0..DENSE_GRID * DENSE_GRID)
                .map(|i| (i % DENSE_GRID) as f32 * 0.01)
                .collect(),
        };
        crate::terrain::write_stored(
            &crate::terrain::graded_dir(dir.path()),
            14,
            8490,
            5318,
            DENSE_GRID,
            &Answer::Tile(dense.encode()),
        );

        let (status, _, body) = get(format!("{base}/terrain/14/8490/5318?grid=513&graded=1")).await;
        assert_eq!(status, 200);
        assert_eq!(TerrainTile::decode(&body).unwrap().grid, DENSE_GRID);

        let (status, _, body) = get(format!("{base}/terrain/14/8490/5318?graded=1")).await;
        assert_eq!(status, 200);
        let thin = TerrainTile::decode(&body).unwrap();
        assert_eq!(thin.grid, GRID);
        assert!((thin.at(GRID - 1, 0) - dense.at(DENSE_GRID - 1, 0)).abs() < 0.01);

        assert_eq!(
            get(format!("{base}/terrain/14/8491/5318?graded=1")).await.0,
            404
        );
    }

    /// `/height` answers in the shape PDOK's WCS did, so the client's decoder
    /// reads it unchanged.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_height_route_answers_what_the_client_decodes() {
        let (base, _dir) = serve(Arc::new(Flat)).await;
        let q = "bbox=234000,580000,234100,580100&grid=16";
        let (status, media, body) = get(format!("{base}/height?coverage=dtm_05m&{q}")).await;
        assert_eq!(status, 200);
        assert_eq!(media, "image/tiff");
        let samples = cartopolis_geo::ahn::decode_float_tiff(&body, 16).unwrap();
        assert!(samples.iter().all(|h| (h - 1.5).abs() < 1e-6));
        // A source with no surface model says so as a failure, not as ground.
        assert_eq!(
            get(format!("{base}/height?coverage=dsm_05m&{q}")).await.0,
            502
        );
        assert_eq!(get(format!("{base}/height?coverage=ahn&{q}")).await.0, 400);
        assert_eq!(
            get(format!("{base}/height?coverage=dtm_05m&bbox=1,2,3&grid=16"))
                .await
                .0,
            400
        );
        assert_eq!(
            get(format!(
                "{base}/height?coverage=dtm_05m&bbox=0,0,50000,50000&grid=16"
            ))
            .await
            .0,
            400
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failing_upstream_is_a_502() {
        let (base, _dir) = serve(Arc::new(Down)).await;
        assert_eq!(get(format!("{base}/terrain/14/8490/5318")).await.0, 502);
    }
}
