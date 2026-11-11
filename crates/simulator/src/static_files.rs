//! Files this listener serves itself, when it is the one address of a server
//! (`cartopolis-server`): the web UI at the root and atlas's map-data tree
//! under `/data/`.
//!
//! On the hosted server Caddy serves both and neither setting is given, so
//! nothing here applies; an install that has no Caddy names the two directories
//! and has a whole site on one port. The map data is read with its brotli and
//! gzip copies beside it (atlas writes a `.br` for every file), so a cell goes
//! out compressed without being compressed per request.

use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

use crate::SimConfig;

/// Where the map-data tree is mounted: the server's `map_data_url` is
/// `{public_base_url}/data` when it serves the tree itself.
pub const MAP_DATA_PATH: &str = "/data";

/// `router` with the configured directories mounted. The web UI is the
/// fallback, after every route, and answers any path it has no file for with
/// its `index.html`: it is a single-page app that routes in the browser.
pub fn mount<S: Clone + Send + Sync + 'static>(router: Router<S>, config: &SimConfig) -> Router<S> {
    let mut router = router;
    if let Some(dir) = config.map_data_dir.as_deref().filter(|d| !d.is_empty()) {
        router = router.nest_service(
            MAP_DATA_PATH,
            ServeDir::new(dir).precompressed_br().precompressed_gzip(),
        );
    }
    if let Some(dir) = config.web_root.as_deref().filter(|d| !d.is_empty()) {
        let index = std::path::Path::new(dir).join("index.html");
        router = router.fallback_service(
            ServeDir::new(dir)
                .precompressed_br()
                .precompressed_gzip()
                .fallback(ServeFile::new(index)),
        );
    }
    router
}
