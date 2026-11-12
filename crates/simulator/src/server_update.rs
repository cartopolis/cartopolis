//! The Admin page's view of a server that can replace itself with a newer
//! release (`cartopolis-server`, `crates/server/src/update.rs`).
//!
//! The simulator does not update anything: it only gives the program that can
//! a place on the admin API, behind the admin check. A server without one (the
//! hosted simulator, every test) answers these routes with 404, so the Admin
//! page shows nothing about updates there.
//!
//! - `GET  /api/admin/update`          what is installed, what is available, what happened
//! - `POST /api/admin/update/check`    look for a release now
//! - `POST /api/admin/update/install`  install the available release (asks the password again)

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;

use crate::http::{is_access_admin, prove_again, require_access, ApiError, ApiResult, HttpState};

/// What `cartopolis-server` offers the Admin page. Every method returns at
/// once: the work runs on the updater's own thread, and [`Self::status`] says
/// how far it is.
pub trait ServerUpdates: Send + Sync {
    /// Installed, available, in progress and the last result, as the Admin
    /// page shows them.
    fn status(&self) -> serde_json::Value;
    /// Look for a release now rather than at the next scheduled check.
    fn check_now(&self);
    /// Start installing the release that is downloaded and verified; refused
    /// when there is none or an install is already running.
    fn install(&self, by: &str) -> Result<(), String>;
}

pub type Updates = Option<Arc<dyn ServerUpdates>>;

/// Only an admin, with a credential that carries an admin's permissions:
/// replacing the program is more than any role may do.
fn admin(
    state: &HttpState,
    headers: &HeaderMap,
) -> ApiResult<(Arc<dyn ServerUpdates>, crate::auth::Access)> {
    let updates = state.updates.clone().ok_or(ApiError(
        StatusCode::NOT_FOUND,
        "this server does not update itself".into(),
    ))?;
    let access = require_access(state, headers)?;
    if !is_access_admin(state, &access)? {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "only an admin may update this server".into(),
        ));
    }
    Ok((updates, access))
}

pub(crate) async fn status(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (updates, _) = admin(&state, &headers)?;
    Ok(Json(updates.status()))
}

pub(crate) async fn check(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let (updates, _) = admin(&state, &headers)?;
    updates.check_now();
    Ok(Json(updates.status()))
}

#[derive(Deserialize, Default)]
pub(crate) struct InstallRequest {
    password: Option<String>,
}

pub(crate) async fn install(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: Option<Json<InstallRequest>>,
) -> ApiResult<Json<serde_json::Value>> {
    let (updates, access) = admin(&state, &headers)?;
    // A stolen session alone must not be enough to restart the server on other
    // code, even code you signed.
    let password = body.and_then(|Json(b)| b.password);
    prove_again(&state, &access, password).await?;
    updates
        .install(&access.username)
        .map_err(|e| ApiError(StatusCode::CONFLICT, e))?;
    tracing::info!(admin = %access.username, "server update: install started");
    Ok(Json(updates.status()))
}
