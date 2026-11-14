//! Files this listener serves itself, when it is the one address of a server
//! (`cartopolis-server`): the web UI at the root and atlas's map-data tree
//! under `/data/`.
//!
//! On the hosted server Caddy serves both and neither setting is given, so
//! nothing here applies; an install that has no Caddy names the two directories
//! and has a whole site on one port. The map data is read with its brotli and
//! gzip copies beside it (atlas writes a `.br` for every file), so a cell goes
//! out compressed without being compressed per request.

use std::sync::Arc;

use axum::extract::Request;
use axum::response::IntoResponse;
use axum::Router;
use tower::ServiceExt as _;
use tower_http::services::{ServeDir, ServeFile};

use crate::pages::PageService;
use crate::SimConfig;

/// Where the map-data tree is mounted: the server's `map_data_url` is
/// `{public_base_url}/data` when it serves the tree itself.
pub const MAP_DATA_PATH: &str = "/data";

/// `router` with the configured directories mounted. The web UI is the
/// fallback, after every route, and answers any path it has no file for with
/// its `index.html`: it is a single-page app that routes in the browser.
///
/// Except a content page (`/{lang}/{slug}`, `/p/{slug}`), which is asked first:
/// its shell is the web UI's `pages.html` with the page's own head filled in
/// (`pages::http::shell`). Asked here rather than as a route, because a route
/// `/{lang}/{slug}` would also catch `/fonts/…` and `/c/{handle}`.
pub fn mount<S: Clone + Send + Sync + 'static>(
    router: Router<S>,
    config: &SimConfig,
    pages: &PageService,
) -> Router<S> {
    let mut router = router;
    if let Some(dir) = config.map_data_dir.as_deref().filter(|d| !d.is_empty()) {
        router = router.nest_service(
            MAP_DATA_PATH,
            ServeDir::new(dir).precompressed_br().precompressed_gzip(),
        );
    }
    if let Some(dir) = config.web_root.as_deref().filter(|d| !d.is_empty()) {
        let index = std::path::Path::new(dir).join("index.html");
        let files = ServeDir::new(dir)
            .precompressed_br()
            .precompressed_gzip()
            .fallback(ServeFile::new(index));
        let shell = Arc::new(std::path::Path::new(dir).join("pages.html"));
        let base = Arc::new(config.public_base_url.clone());
        let pages = pages.clone();
        router = router.fallback(move |req: Request| {
            let (files, shell, base, pages) =
                (files.clone(), shell.clone(), base.clone(), pages.clone());
            async move {
                if let Some(page) =
                    crate::pages::http::shell(&pages, &shell, &base, req.uri(), req.headers()).await
                {
                    return page;
                }
                match files.oneshot(req).await {
                    Ok(r) => r.into_response(),
                    Err(never) => match never {},
                }
            }
        });
    }
    router
}
