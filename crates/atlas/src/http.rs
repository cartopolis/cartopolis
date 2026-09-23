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
}

pub fn router(state: AtlasState) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/terrain/{z}/{x}/{y}", get(terrain_tile))
        .with_state(state)
}

async fn terrain_tile(
    State(state): State<AtlasState>,
    Path((z, x, y)): Path<(u32, i64, i64)>,
) -> Response {
    // A week: a cut tile never changes for a given survey, and a new AHN release
    // is a new cache directory, not an edit.
    let cache = (header::CACHE_CONTROL, "public, max-age=604800");
    let cors = (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
    match state.terrain.tile(z, x, y).await {
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
        let state = AtlasState {
            terrain: Arc::new(TerrainService::new(source, dir.path(), 2)),
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

        let (x, y) = cartopolis_core::lat_lng_to_tile(48.8566, 2.3522, 14);
        assert_eq!(get(format!("{base}/terrain/14/{x}/{y}")).await.0, 404);
        assert_eq!(get(format!("{base}/terrain/25/0/0")).await.0, 400);
        assert_eq!(get(format!("{base}/health")).await.0, 200);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failing_upstream_is_a_502() {
        let (base, _dir) = serve(Arc::new(Down)).await;
        assert_eq!(get(format!("{base}/terrain/14/8490/5318")).await.0, 502);
    }
}
