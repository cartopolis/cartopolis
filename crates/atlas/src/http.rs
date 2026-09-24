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
    /// The admin console's pipeline surface, `/api/pipeline/*`.
    pub console: crate::console::ConsoleState,
}

/// `cartopolis.org` itself. Atlas owns the site root; the browser build of the
/// client lives at `/viewer/`, and Caddy routes that, `/ws` and the other client
/// paths before anything reaches here. *Sign in* is the console at `/console/`
/// (`crates/atlas/console`), which Caddy serves as static files: the operator's
/// pages and the players' registration, verification and house claim.
const FRONT_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Cartopolis</title>
<style>
  body { margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center;
         background: #1B2830; color: #F2EFEA; font-family: system-ui, sans-serif; }
  main { text-align: center; padding: 2rem; }
  h1 { font-weight: 600; letter-spacing: 0.02em; margin: 0 0 0.5rem; }
  p { opacity: 0.8; margin: 0 0 2rem; }
  a.button { display: inline-block; margin: 0.4rem; padding: 0.8rem 1.4rem; border-radius: 0.5rem;
             background: #FD6012; color: #F2EFEA; text-decoration: none; font-weight: 600; }
  a.quiet { background: #2e424e; }
</style>
</head>
<body>
<main>
  <h1>Cartopolis</h1>
  <p>A shared 3D world on the real map.</p>
  <a class="button" href="/viewer/">Open the map</a>
  <a class="button quiet" href="/apk/">Android app</a>
  <a class="button quiet" href="/console/">Sign in</a>
</main>
</body>
</html>
"#;

pub fn router(state: AtlasState) -> Router {
    let console = crate::console::router(state.console.clone());
    Router::new()
        .route("/", get(|| async { axum::response::Html(FRONT_PAGE) }))
        .route("/health", get(|| async { "ok" }))
        .route("/terrain/{z}/{x}/{y}", get(terrain_tile))
        .with_state(state)
        .nest("/api/pipeline", console)
}

/// `?grid=513` asks for the dense cut the client's terrain field is assembled from
/// (`cartopolis_geo::terrain_tile::DENSE_GRID`); the default is the tile tree's.
#[derive(serde::Deserialize)]
struct Grid {
    grid: Option<usize>,
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
            terrain: Arc::new(TerrainService::new(source, dir.path(), 2)),
            console: crate::console::ConsoleState {
                auth: Arc::new(crate::auth::Auth::new(Box::new(Nobody))),
                runner: crate::jobs::Runner::new(dir.path(), crate::jobs::Registry::new()),
                out_root: dir.path().to_path_buf(),
                autopilot: dir.path().join("autopilot.json"),
            },
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
        let (status, media, body) = get(format!("{base}/")).await;
        assert_eq!(status, 200);
        assert!(media.starts_with("text/html"));
        let page = String::from_utf8_lossy(&body);
        assert!(page.contains("href=\"/viewer/\"") && page.contains("href=\"/console/\""));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failing_upstream_is_a_502() {
        let (base, _dir) = serve(Arc::new(Down)).await;
        assert_eq!(get(format!("{base}/terrain/14/8490/5318")).await.0, 502);
    }
}
