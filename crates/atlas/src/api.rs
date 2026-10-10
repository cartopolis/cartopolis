//! `/api/pipeline/*`: what the web UI drives the map data pipelines
//! through. Ported from the earlier TypeScript server (`index.ts`), route for route and refusal
//! for refusal, so the web UI needed no change to talk to atlas instead.
//!
//! Every route but `/health` asks the simulator whether the caller is an admin
//! ([`crate::auth`]). Every refusal is `{"error": "…"}`, the shape the web UI's
//! `lib/api.ts` unwraps.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::areas::{area_km2, validate_bbox, AREAS};
use crate::auth::{Auth, Principal};
use crate::coverage::coverage;
use crate::jobs::Runner;
use crate::pipeline::DATASETS;

#[derive(Clone)]
pub struct ApiState {
    pub auth: Arc<Auth>,
    pub runner: Runner,
    /// The tree the pipelines write and Caddy serves.
    pub out_root: PathBuf,
    /// The autopilot's control file.
    pub autopilot: PathBuf,
    /// Building designs (`crate::designs`).
    pub designs: Arc<crate::designs::Designs>,
    /// Building overrides (`crate::overrides`).
    pub overrides: Arc<crate::overrides::Overrides>,
}

fn fail(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

/// The signed-in operator behind a request, if their roles let them run the
/// pipelines (`pipeline_run`; an admin always may).
async fn admin(state: &ApiState, headers: &HeaderMap) -> Result<Principal, Response> {
    let who = state.auth.admin(headers).await.ok_or_else(|| {
        fail(
            StatusCode::UNAUTHORIZED,
            "That session is not an operator session",
        )
    })?;
    if !who.may(cartopolis_core::permissions::Permission::PipelineRun) {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "That account may not run the map data pipelines",
        ));
    }
    Ok(who)
}

/// A blocking read of the served tree, off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("a blocking pipeline read panicked")
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/config", get(config))
        .route("/coverage", get(get_coverage))
        .route("/autopilot", get(get_autopilot).patch(patch_autopilot))
        .route("/jobs", get(list_jobs).post(start_job))
        .route("/jobs/{id}/log", get(job_log))
        .route("/jobs/{id}/cancel", post(cancel_job))
        .route("/backdrop.svg", get(get_backdrop))
        .route("/buildings/{pand}", get(get_building))
        .route(
            "/buildings/{pand}/design",
            axum::routing::put(put_design).delete(delete_design),
        )
        // Building overrides (`crate::overrides`, `docs/notes/building-models.md`).
        .route("/overrides", get(list_overrides))
        .route(
            "/overrides/{pand}",
            get(get_override).put(put_override).delete(delete_override),
        )
        .route(
            "/overrides/{pand}/model",
            // Raised on this route alone, to the model store's own ceiling;
            // axum's two-megabyte default refuses nearly every real model.
            post(upload_override_model).layer(axum::extract::DefaultBodyLimit::max(
                cartopolis_core::model_limits::MAX_ASSET_BYTES + 1024,
            )),
        )
        .fallback(|| async { fail(StatusCode::NOT_FOUND, "No such endpoint") })
        .with_state(state)
}

#[derive(Deserialize)]
struct BackdropQuery {
    /// `south,west,north,east` in degrees; Groningen's centre when absent.
    bbox: Option<String>,
    /// `streets` (the default) or `pulses`.
    layer: Option<String>,
}

/// A city's streets as the web UI's backdrop (`pipeline::backdrop`). Operator
/// only, like the rest of this surface: every call is a query on an Overpass
/// other people share. The site serves a committed copy; this is how it is made.
async fn get_backdrop(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<BackdropQuery>,
) -> Response {
    use crate::pipeline::backdrop::{self, Layer};
    if let Err(r) = admin(&state, &headers).await {
        return r;
    }
    let layer = match q.layer.as_deref().map(Layer::parse) {
        None => Layer::Streets,
        Some(Some(l)) => l,
        Some(None) => return fail(StatusCode::BAD_REQUEST, "The layer is streets or pulses."),
    };
    let bbox = match q.bbox.as_deref() {
        None => backdrop::GRONINGEN,
        Some(text) => {
            let numbers: Result<Vec<f64>, _> = text.split(',').map(|v| v.trim().parse()).collect();
            match numbers
                .map_err(|_| "A bbox is four numbers.".to_string())
                .and_then(|v| backdrop::validate(&v))
            {
                Ok(b) => b,
                Err(e) => return fail(StatusCode::BAD_REQUEST, &e),
            }
        }
    };
    match blocking(move || backdrop::fetch(bbox, layer)).await {
        Ok(svg) => ([("content-type", "image/svg+xml")], svg).into_response(),
        Err(e) => fail(StatusCode::BAD_GATEWAY, &format!("Overpass: {e}")),
    }
}

/// Unauthenticated and deliberately says nothing: what a supervisor polls.
async fn health(State(state): State<ApiState>) -> Json<Value> {
    Json(json!({"ok": true, "out": state.out_root}))
}

async fn config(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if let Err(r) = admin(&state, &headers).await {
        return r;
    }
    let areas: Vec<Value> = AREAS
        .iter()
        .map(|a| {
            json!({"bbox": a.bbox, "id": a.id, "label": a.label,
                   "km2": area_km2(a.bbox).round()})
        })
        .collect();
    let datasets: Vec<Value> = DATASETS
        .iter()
        .map(|d| {
            json!({"description": d.description, "id": d.id, "label": d.label,
                   "typicalMinutes": d.typical_minutes})
        })
        .collect();
    Json(json!({"areas": areas, "datasets": datasets, "out": state.out_root})).into_response()
}

async fn get_coverage(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if let Err(r) = admin(&state, &headers).await {
        return r;
    }
    let out = state.out_root.clone();
    Json(json!({"coverage": blocking(move || coverage(&out)).await})).into_response()
}

const NO_AUTOPILOT: &str = "The autopilot does not run beside this server";

/// On a host without the lane the file is absent and both verbs say so, so the
/// web UI explains itself instead of drawing dead sliders.
async fn get_autopilot(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if let Err(r) = admin(&state, &headers).await {
        return r;
    }
    let path = state.autopilot.clone();
    match blocking(move || crate::autopilot::read(&path)).await {
        Ok(config) => Json(json!({"config": config})).into_response(),
        Err(_) => fail(StatusCode::SERVICE_UNAVAILABLE, NO_AUTOPILOT),
    }
}

async fn patch_autopilot(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let who = match admin(&state, &headers).await {
        Ok(w) => w,
        Err(r) => return r,
    };
    let patch = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(m)) => m,
        _ => return fail(StatusCode::BAD_REQUEST, "That request body is not JSON"),
    };
    let path = state.autopilot.clone();
    match blocking(move || crate::autopilot::write(&path, &patch)).await {
        Ok(config) => {
            tracing::info!(
                "autopilot config set by {}: {}",
                who.username,
                serde_json::to_string(&config).unwrap_or_default()
            );
            Json(json!({"config": config})).into_response()
        }
        Err(_) => fail(StatusCode::SERVICE_UNAVAILABLE, NO_AUTOPILOT),
    }
}

async fn list_jobs(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if let Err(r) = admin(&state, &headers).await {
        return r;
    }
    Json(json!({"jobs": state.runner.list()})).into_response()
}

#[derive(Deserialize)]
struct Since {
    since: Option<String>,
}

async fn job_log(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<Since>,
) -> Response {
    if let Err(r) = admin(&state, &headers).await {
        return r;
    }
    let Some(job) = state.runner.get(&id) else {
        return fail(StatusCode::NOT_FOUND, "No such job");
    };
    let since = q.since.and_then(|s| s.parse().ok()).unwrap_or(0);
    let (lines, total) = state.runner.log(&id, since).unwrap_or_default();
    Json(json!({"job": job, "lines": lines, "total": total})).into_response()
}

async fn cancel_job(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = admin(&state, &headers).await {
        return r;
    }
    if !state.runner.cancel(&id) {
        return fail(StatusCode::CONFLICT, "That run has already finished");
    }
    Json(json!({"job": state.runner.get(&id)})).into_response()
}

// ─── Building designs ───────────────────────────────────────────────────────
//
// Under `/api/pipeline` because that is the prefix Caddy already sends here; a
// design is map data atlas publishes, which is what this surface is about. These
// routes take any signed-in player rather than only an admin, and ask the
// simulator about the one building named.

#[derive(Deserialize)]
struct Near {
    lat: Option<f64>,
    lng: Option<f64>,
}

async fn building_access(
    state: &ApiState,
    headers: &HeaderMap,
    pand: &str,
) -> Result<crate::auth::Access, Response> {
    if !crate::designs::valid_pand(pand) {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "That is not a BAG building number",
        ));
    }
    state.auth.access(headers, pand).await.ok_or_else(|| {
        fail(
            StatusCode::UNAUTHORIZED,
            "Sign in to open the building editor",
        )
    })
}

/// Everything the editor opens with: what the registers say about the building
/// (outline, entrances, height, the neighbours either side), its stored design if
/// it has one, the design it would have if it had not, and what the caller may do.
async fn get_building(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(pand): Path<String>,
    Query(near): Query<Near>,
) -> Response {
    let access = match building_access(&state, &headers, &pand).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let designs = state.designs.clone();
    let p = pand.clone();
    let (stored, facts, generated) = blocking(move || {
        let stored = designs.stored(&p);
        let near = match (near.lat, near.lng, &stored) {
            (Some(lat), Some(lng), _) => Some((lat, lng)),
            (_, _, Some(d)) => Some((d.origin[0], d.origin[1])),
            _ => None,
        };
        let facts = near.and_then(|n| designs.facts(&p, n));
        let generated = facts.as_ref().and_then(|f| designs.generated(f));
        (stored, facts, generated)
    })
    .await;
    if stored.is_none() && facts.is_none() {
        return fail(
            StatusCode::NOT_FOUND,
            "That building is not in this server's map data",
        );
    }
    Json(json!({
        "pand": pand,
        "access": access,
        "facts": facts,
        "design": stored.clone().or_else(|| generated.clone()),
        "stored": stored.is_some(),
        "generated": generated,
    }))
    .into_response()
}

async fn put_design(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(pand): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let access = match building_access(&state, &headers, &pand).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    if !access.can_edit {
        return fail(
            StatusCode::FORBIDDEN,
            "Only the building's owner can change its design",
        );
    }
    if body.len() > 2 * 1024 * 1024 {
        return fail(StatusCode::PAYLOAD_TOO_LARGE, "That design is over 2 MB");
    }
    let design = match std::str::from_utf8(&body)
        .map_err(|e| e.to_string())
        .and_then(cartopolis_geo::building_design::BuildingDesign::from_json)
    {
        Ok(d) => d,
        Err(e) => return fail(StatusCode::BAD_REQUEST, &e),
    };
    if design.pand != pand {
        return fail(
            StatusCode::BAD_REQUEST,
            "That design is for a different building",
        );
    }
    let designs = state.designs.clone();
    match blocking(move || designs.save(design)).await {
        Ok(saved) => {
            tracing::info!(
                pand = %pand,
                by = %access.username,
                revision = saved.revision,
                storeys = saved.storeys.len(),
                "building design saved"
            );
            Json(json!({"design": saved})).into_response()
        }
        Err(e) => fail(StatusCode::BAD_REQUEST, &e),
    }
}

async fn delete_design(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(pand): Path<String>,
) -> Response {
    let access = match building_access(&state, &headers, &pand).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    if !access.can_edit {
        return fail(
            StatusCode::FORBIDDEN,
            "Only the building's owner can change its design",
        );
    }
    let designs = state.designs.clone();
    let p = pand.clone();
    match blocking(move || designs.remove(&p)).await {
        Ok(true) => {
            tracing::info!(pand = %pand, by = %access.username, "building design reset");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => fail(StatusCode::NOT_FOUND, "That building has no saved design"),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

// ─── Building overrides ─────────────────────────────────────────────────────

/// The signed-in operator, if their roles let them replace a building's outside
/// (`building_override`; an admin always may).
async fn override_operator(state: &ApiState, headers: &HeaderMap) -> Result<Principal, Response> {
    let who = state.auth.admin(headers).await.ok_or_else(|| {
        fail(
            StatusCode::UNAUTHORIZED,
            "That session is not an operator session",
        )
    })?;
    if !who.may(cartopolis_core::permissions::Permission::BuildingOverride) {
        return Err(fail(
            StatusCode::FORBIDDEN,
            "That account may not replace buildings",
        ));
    }
    Ok(who)
}

async fn list_overrides(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if let Err(r) = override_operator(&state, &headers).await {
        return r;
    }
    let store = state.overrides.clone();
    let all = blocking(move || store.list()).await;
    Json(json!({ "overrides": all })).into_response()
}

async fn get_override(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(pand): Path<String>,
) -> Response {
    if let Err(r) = override_operator(&state, &headers).await {
        return r;
    }
    let store = state.overrides.clone();
    match blocking(move || store.get(&pand)).await {
        Some(o) => Json(json!({ "override": o })).into_response(),
        None => fail(StatusCode::NOT_FOUND, "That building has no override"),
    }
}

/// Save an edit, and with it a recompile and a republish. Slow on purpose when
/// the source is a stadium: it measures the bowl from AHN again.
async fn put_override(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(pand): Path<String>,
    Json(edit): Json<crate::overrides::Edit>,
) -> Response {
    let who = match override_operator(&state, &headers).await {
        Ok(w) => w,
        Err(r) => return r,
    };
    let store = state.overrides.clone();
    let p = pand.clone();
    let by = who.username.clone();
    match blocking(move || store.save(&p, edit, None, &by)).await {
        Ok((o, summary)) => {
            tracing::info!(pand = %pand, by = %who.username, ?summary, "building override saved");
            Json(json!({ "override": o, "summary": summary })).into_response()
        }
        Err(e) => fail(StatusCode::BAD_REQUEST, &e),
    }
}

/// Upload a GLB for a building: creates the override, or replaces its model.
async fn upload_override_model(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(pand): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let who = match override_operator(&state, &headers).await {
        Ok(w) => w,
        Err(r) => return r,
    };
    let store = state.overrides.clone();
    let p = pand.clone();
    let by = who.username.clone();
    let bytes = body.to_vec();
    match blocking(move || store.save(&p, crate::overrides::Edit::default(), Some(bytes), &by))
        .await
    {
        Ok((o, summary)) => {
            tracing::info!(pand = %pand, by = %who.username, ?summary, "building model uploaded");
            Json(json!({ "override": o, "summary": summary })).into_response()
        }
        Err(e) => fail(StatusCode::BAD_REQUEST, &e),
    }
}

/// Delete an override: the building goes back to what the survey drew.
async fn delete_override(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(pand): Path<String>,
) -> Response {
    let who = match override_operator(&state, &headers).await {
        Ok(w) => w,
        Err(r) => return r,
    };
    let store = state.overrides.clone();
    let p = pand.clone();
    match blocking(move || store.remove(&p)).await {
        Ok(true) => {
            tracing::info!(pand = %pand, by = %who.username, "building override deleted");
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => fail(StatusCode::NOT_FOUND, "That building has no override"),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, &e),
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 40
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

async fn start_job(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let who = match admin(&state, &headers).await {
        Ok(w) => w,
        Err(r) => return r,
    };
    let Ok(Value::Object(body)) = serde_json::from_slice::<Value>(&body) else {
        return fail(StatusCode::BAD_REQUEST, "That request body is not JSON");
    };
    let datasets: Vec<String> = body
        .get("datasets")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if datasets.is_empty() {
        return fail(
            StatusCode::BAD_REQUEST,
            "Pick at least one dataset to extract",
        );
    }
    let unknown: Vec<&str> = datasets
        .iter()
        .map(String::as_str)
        .filter(|id| crate::pipeline::dataset(id).is_none())
        .collect();
    if !unknown.is_empty() {
        return fail(
            StatusCode::BAD_REQUEST,
            &format!("No such dataset: {}", unknown.join(", ")),
        );
    }

    // A named area or a typed bbox, and the name travels either way: it is
    // stamped into every coverage manifest.
    let bbox_given = body.get("bbox").is_some_and(|b| !b.is_null());
    let (bbox, name) = match body.get("area").and_then(Value::as_str) {
        Some(id) if !bbox_given => match crate::areas::area(id) {
            Some(a) => (a.bbox, a.id.to_string()),
            None => return fail(StatusCode::BAD_REQUEST, &format!("No such area: {id}")),
        },
        _ => {
            let raw: Vec<f64> = match body.get("bbox").and_then(Value::as_array) {
                Some(a) if a.iter().all(Value::is_number) => {
                    a.iter().filter_map(Value::as_f64).collect()
                }
                _ => Vec::new(),
            };
            let bbox = match validate_bbox(&raw) {
                Ok(b) => b,
                Err(e) => return fail(StatusCode::BAD_REQUEST, &e),
            };
            let name = body
                .get("area")
                .map_or(String::new(), |v| {
                    v.as_str().map_or_else(|| v.to_string(), str::to_string)
                })
                .trim()
                .to_string();
            if !valid_name(&name) {
                return fail(
                    StatusCode::BAD_REQUEST,
                    "A custom area needs a name: lower case, digits and hyphens.",
                );
            }
            (bbox, name)
        }
    };

    // The extractors rewrite the manifest for the *whole* dataset, so a second
    // area lands on top of the first rather than beside it. Saying so now beats
    // finding out twenty minutes later that Groningen stopped being covered.
    let out = state.out_root.clone();
    let replaces: Vec<Value> = blocking(move || coverage(&out))
        .await
        .into_iter()
        .filter(|e| datasets.contains(&e.dataset))
        .filter_map(|e| match e.name {
            Some(n) if n != name => Some(json!({"dataset": e.dataset, "name": n})),
            _ => None,
        })
        .collect();

    // `"fresh": true` asks every source anew instead of the month's cache.
    let fresh = body.get("fresh").and_then(Value::as_bool).unwrap_or(false);
    let job = state
        .runner
        .submit_with(&name, bbox, &datasets, &who.username, fresh);
    Json(json!({"job": job, "replaces": replaces})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Authority;
    use crate::jobs::{Registry, Runnable};
    use crate::pipeline::DatasetResult;

    struct Tokens;
    impl Authority for Tokens {
        fn admin(&self, token: &str) -> Result<Option<Principal>, String> {
            Ok((token == "admin").then(|| Principal {
                principal: json!(1),
                username: "op".into(),
                admin: true,
                permissions: Vec::new(),
            }))
        }

        /// `owner` owns the eastern house; `visitor` owns nothing.
        fn access(&self, token: &str, pand: &str) -> Result<Option<crate::auth::Access>, String> {
            let owner = token == "owner" && pand == crate::designs::EAST;
            Ok(
                matches!(token, "owner" | "visitor").then(|| crate::auth::Access {
                    username: token.into(),
                    owner,
                    can_edit: owner,
                    ..Default::default()
                }),
            )
        }
    }

    async fn serve() -> (String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let registry: Registry = DATASETS
            .iter()
            .map(|d| {
                (
                    d.id.to_string(),
                    Runnable {
                        label: d.label.into(),
                        run: Arc::new(|_| Ok(DatasetResult::default())),
                    },
                )
            })
            .collect();
        let site = dir.path().join("site");
        std::fs::create_dir_all(site.join("bgt")).unwrap();
        crate::designs::two_houses(&site);
        std::fs::write(
            site.join("bgt/coverage.json"),
            r#"{"name":"groningen","cells":[[1,2]],"tree_count":3}"#,
        )
        .unwrap();
        let state = ApiState {
            auth: Arc::new(Auth::new(Box::new(Tokens))),
            runner: Runner::new(&site, registry),
            out_root: site.clone(),
            autopilot: dir.path().join("autopilot.json"),
            designs: Arc::new(crate::designs::Designs::new(
                &dir.path().join("data"),
                &site,
            )),
            overrides: Arc::new(crate::overrides::Overrides::new(
                &dir.path().join("data"),
                &site,
            )),
        };
        let app = Router::new().nest("/api/pipeline", router(state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}/api/pipeline"), dir)
    }

    async fn call(
        method: &'static str,
        url: String,
        token: &'static str,
        body: Option<Value>,
    ) -> (u16, Value) {
        tokio::task::spawn_blocking(move || {
            let req = ureq::request(method, &url).set("authorization", &format!("Bearer {token}"));
            let r = match body {
                Some(b) => req.send_string(&b.to_string()),
                None => req.call(),
            };
            let r = match r {
                Ok(r) | Err(ureq::Error::Status(_, r)) => r,
                Err(e) => panic!("{e}"),
            };
            let status = r.status();
            let text = r.into_string().unwrap();
            let body = if text.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&text).unwrap()
            };
            (status, body)
        })
        .await
        .unwrap()
    }

    /// Who may open the editor, who may save, and what a save publishes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_the_owner_saves_a_design_and_a_save_is_published() {
        let (base, dir) = serve().await;
        let east = crate::designs::EAST;
        let at = "lat=53.2194&lng=6.5665";

        // No session, no editor.
        let (s, _) = call("GET", format!("{base}/buildings/{east}?{at}"), "", None).await;
        assert_eq!(s, 401);
        // A visitor opens it, and is told they may not change it.
        let (s, v) = call(
            "GET",
            format!("{base}/buildings/{east}?{at}"),
            "visitor",
            None,
        )
        .await;
        assert_eq!(s, 200);
        assert_eq!(v["access"]["can_edit"], false);
        assert_eq!(v["stored"], false);
        assert_eq!(v["facts"]["outline"].as_array().unwrap().len(), 4);
        let design = v["design"].clone();
        assert_eq!(design["source"], "generated");

        let (s, v) = call(
            "PUT",
            format!("{base}/buildings/{east}/design"),
            "visitor",
            Some(design.clone()),
        )
        .await;
        assert_eq!(s, 403, "{v}");
        // A design for another building is refused even from its owner's hand.
        let (s, _) = call(
            "PUT",
            format!("{base}/buildings/0014100000000001/design"),
            "owner",
            Some(design.clone()),
        )
        .await;
        assert_eq!(s, 403);

        let (s, v) = call(
            "PUT",
            format!("{base}/buildings/{east}/design"),
            "owner",
            Some(design),
        )
        .await;
        assert_eq!(s, 200, "{v}");
        assert_eq!(v["design"]["source"], "editor");
        assert!(dir.path().join("site/designs/coverage.json").exists());

        let (s, v) = call("GET", format!("{base}/buildings/{east}"), "visitor", None).await;
        assert_eq!(s, 200, "a stored design is found without a hint");
        assert_eq!(v["stored"], true);

        let (s, _) = call(
            "DELETE",
            format!("{base}/buildings/{east}/design"),
            "visitor",
            None,
        )
        .await;
        assert_eq!(s, 403);
        let (s, _) = call(
            "DELETE",
            format!("{base}/buildings/{east}/design"),
            "owner",
            None,
        )
        .await;
        assert_eq!(s, 204);
        let (s, _) = call("GET", format!("{base}/buildings/not-a-pand"), "owner", None).await;
        assert_eq!(s, 400);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_pipeline_surface_answers_as_before() {
        let (base, dir) = serve().await;

        assert_eq!(call("GET", format!("{base}/health"), "", None).await.0, 200);
        let (s, v) = call("GET", format!("{base}/config"), "nobody", None).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (401, Some("That session is not an operator session"))
        );

        let (s, v) = call("GET", format!("{base}/config"), "admin", None).await;
        assert_eq!(s, 200);
        assert_eq!(v["areas"][0]["km2"], 81.0);
        assert_eq!(v["datasets"].as_array().unwrap().len(), DATASETS.len());

        let (_, v) = call("GET", format!("{base}/coverage"), "admin", None).await;
        let bgt = v["coverage"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["dataset"] == "bgt")
            .unwrap()
            .clone();
        assert_eq!(bgt["counts"][0], json!({"label": "trees", "n": 3}));

        assert_eq!(
            call("GET", format!("{base}/autopilot"), "admin", None)
                .await
                .0,
            503
        );
        std::fs::write(dir.path().join("autopilot.json"), "{}").unwrap();
        let (s, v) = call(
            "PATCH",
            format!("{base}/autopilot"),
            "admin",
            Some(json!({"qaPerDay": 3})),
        )
        .await;
        assert_eq!((s, v["config"]["qaPerDay"].as_i64()), (200, Some(3)));

        let (s, v) = call(
            "POST",
            format!("{base}/jobs"),
            "admin",
            Some(json!({"area": "groningen"})),
        )
        .await;
        assert_eq!(
            (s, v["error"].as_str()),
            (400, Some("Pick at least one dataset to extract"))
        );
        let (s, _) = call("POST", format!("{base}/jobs"), "admin",
            Some(json!({"area": "Bad Name", "bbox": [229000, 578000, 230000, 579000], "datasets": ["bgt"]}))).await;
        assert_eq!(s, 400);
        let (s, v) = call("POST", format!("{base}/jobs"), "admin",
            Some(json!({"area": "centrum", "bbox": [229000, 578000, 230000, 579000], "datasets": ["bgt"]}))).await;
        assert_eq!(s, 200);
        assert_eq!(
            v["replaces"],
            json!([{"dataset": "bgt", "name": "groningen"}])
        );
        assert_eq!(v["job"]["by"], "op");
        let id = v["job"]["id"].as_str().unwrap().to_string();

        let (s, v) = call(
            "GET",
            format!("{base}/jobs/{id}/log?since=0"),
            "admin",
            None,
        )
        .await;
        assert_eq!(s, 200);
        assert!(v["total"].is_u64());
        assert_eq!(
            call("GET", format!("{base}/jobs/99/log"), "admin", None)
                .await
                .0,
            404
        );
        let (_, v) = call("GET", format!("{base}/jobs"), "admin", None).await;
        assert_eq!(v["jobs"].as_array().unwrap().len(), 1);
        assert_eq!(
            call("GET", format!("{base}/nope"), "admin", None).await.0,
            404
        );
    }
}
