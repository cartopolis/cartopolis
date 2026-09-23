//! `/api/pipeline/*`: what the admin console drives the map data pipelines
//! through. Ported from cartopy's `server/index.ts`, route for route and refusal
//! for refusal, so the console needed no change to talk to atlas instead.
//!
//! Every route but `/health` asks the simulator whether the caller is an admin
//! ([`crate::auth`]). Every refusal is `{"error": "…"}`, the shape the console's
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
pub struct ConsoleState {
    pub auth: Arc<Auth>,
    pub runner: Runner,
    /// The tree the pipelines write and Caddy serves.
    pub out_root: PathBuf,
    /// The autopilot's control file.
    pub autopilot: PathBuf,
}

fn fail(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

async fn admin(state: &ConsoleState, headers: &HeaderMap) -> Result<Principal, Response> {
    state.auth.admin(headers).await.ok_or_else(|| {
        fail(
            StatusCode::UNAUTHORIZED,
            "That session is not an admin session",
        )
    })
}

/// A blocking read of the served tree, off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("a blocking pipeline read panicked")
}

pub fn router(state: ConsoleState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/config", get(config))
        .route("/coverage", get(get_coverage))
        .route("/autopilot", get(get_autopilot).patch(patch_autopilot))
        .route("/jobs", get(list_jobs).post(start_job))
        .route("/jobs/{id}/log", get(job_log))
        .route("/jobs/{id}/cancel", post(cancel_job))
        .fallback(|| async { fail(StatusCode::NOT_FOUND, "No such endpoint") })
        .with_state(state)
}

/// Unauthenticated and deliberately says nothing: what a supervisor polls.
async fn health(State(state): State<ConsoleState>) -> Json<Value> {
    Json(json!({"ok": true, "out": state.out_root}))
}

async fn config(State(state): State<ConsoleState>, headers: HeaderMap) -> Response {
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

async fn get_coverage(State(state): State<ConsoleState>, headers: HeaderMap) -> Response {
    if let Err(r) = admin(&state, &headers).await {
        return r;
    }
    let out = state.out_root.clone();
    Json(json!({"coverage": blocking(move || coverage(&out)).await})).into_response()
}

const NO_AUTOPILOT: &str = "The autopilot does not run beside this server";

/// On a host without the lane the file is absent and both verbs say so, so the
/// console explains itself instead of drawing dead sliders.
async fn get_autopilot(State(state): State<ConsoleState>, headers: HeaderMap) -> Response {
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
    State(state): State<ConsoleState>,
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

async fn list_jobs(State(state): State<ConsoleState>, headers: HeaderMap) -> Response {
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
    State(state): State<ConsoleState>,
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
    State(state): State<ConsoleState>,
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

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 40
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

async fn start_job(
    State(state): State<ConsoleState>,
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

    let job = state.runner.submit(&name, bbox, &datasets, &who.username);
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
            }))
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
        std::fs::write(
            site.join("bgt/coverage.json"),
            r#"{"name":"groningen","cells":[[1,2]],"tree_count":3}"#,
        )
        .unwrap();
        let state = ConsoleState {
            auth: Arc::new(Auth::new(Box::new(Tokens))),
            runner: Runner::new(&site, registry),
            out_root: site,
            autopilot: dir.path().join("autopilot.json"),
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
            (
                r.status(),
                serde_json::from_reader(r.into_reader()).unwrap(),
            )
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_console_surface_answers_as_cartopy_did() {
        let (base, dir) = serve().await;

        assert_eq!(call("GET", format!("{base}/health"), "", None).await.0, 200);
        let (s, v) = call("GET", format!("{base}/config"), "nobody", None).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (401, Some("That session is not an admin session"))
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
