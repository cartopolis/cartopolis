//! Atlas's public paths, passed through to it, when the simulator is the one
//! listener in front of both (`cartopolis-server`, `crates/server`).
//!
//! Atlas runs as a process of its own beside the simulator — its imports peak at
//! gigabytes and must not cost the tick or take the world down with them
//! (`docs/notes/atlas.md`) — but a server somebody installs should have one
//! address, not two ports and a reverse proxy to write by hand. So with
//! [`SimConfig::atlas_url`] set, the simulator answers atlas's paths by asking
//! atlas: the same request, streamed both ways. Unset (the hosted server, where
//! Caddy routes these itself), there are no such routes.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

use crate::SimConfig;

/// Every path atlas answers for the public (`crates/atlas/src/http.rs`), in
/// axum's syntax. The rest of atlas's surface is the operator's, under
/// `/api/pipeline`, and goes the same way.
pub const ATLAS_PATHS: &[&str] = &[
    "/health",
    "/terrain/{*rest}",
    "/height",
    "/geocode/{*rest}",
    "/satellites/{*rest}",
    "/plans/{*rest}",
    "/api/pipeline",
    "/api/pipeline/{*rest}",
];

struct Proxy {
    base: String,
    client: Client<HttpConnector, Body>,
}

/// The pass-through routes, or none when no atlas is configured. Generic over
/// the state of the router they are merged into: they carry their own.
pub fn routes<S: Clone + Send + Sync + 'static>(config: &SimConfig) -> Router<S> {
    let Some(base) = config.atlas_url.as_deref().filter(|u| !u.is_empty()) else {
        return Router::new();
    };
    let proxy = Arc::new(Proxy {
        base: base.trim_end_matches('/').to_string(),
        client: Client::builder(TokioExecutor::new()).build_http(),
    });
    ATLAS_PATHS
        .iter()
        .fold(Router::new(), |r, path| r.route(path, any(forward)))
        .with_state(proxy)
}

async fn forward(State(proxy): State<Arc<Proxy>>, request: Request) -> Response {
    let (mut parts, body) = request.into_parts();
    let path = parts.uri.path_and_query().map_or("/", |p| p.as_str());
    let Ok(uri) = format!("{}{path}", proxy.base).parse::<Uri>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    parts.uri = uri;
    // The client names atlas's host itself; the caller's would be ours.
    parts.headers.remove(header::HOST);
    match proxy.client.request(Request::from_parts(parts, body)).await {
        Ok(response) => response.map(Body::new).into_response(),
        Err(e) => {
            tracing::warn!("atlas did not answer: {e}");
            (StatusCode::BAD_GATEWAY, "atlas is not answering").into_response()
        }
    }
}
