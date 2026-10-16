//! The picture store: what a private chat's pictures are kept in
//! (`docs/notes/pictures-in-private-chats.md`).
//!
//! **Every file in it is ciphertext, and the server cannot open one.** A
//! private chat is end to end encrypted, and its log refuses anything over
//! `e2ee::MAX_MESSAGE_BYTES` so that it does not become a file store: a
//! picture is encrypted on the sending device under a key made for it alone,
//! uploaded here, and the *key* travels inside the sealed message with the
//! hash. Only the chat's members can read the message, so only they can read
//! the picture. That is the reason nothing here checks a format — there is
//! nothing to check but a size.
//!
//! Borrowed from the video store: a file is named by the hash of its bytes
//! (`assets::content_hash`), written to a temporary name and renamed, and
//! served cached forever. What it does not borrow is the database row: a
//! picture has no measurements worth keeping and belongs to a conversation the
//! server cannot see into, so there is nothing to put in one.
//!
//! **Fetching one needs a session, and the hash.** A hash is 256 bits nobody
//! can guess, so in practice only a chat member, who read it out of the sealed
//! message, can name a picture — and what that buys anybody else is ciphertext.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use cartopolis_core::picture_limits::{MAX_PICTURES_PER_HOUR, MAX_PICTURE_BYTES};

use crate::assets;

fn picture_path(dir: &str, hash: &str) -> PathBuf {
    Path::new(dir).join("pictures").join(format!("{hash}.bin"))
}

/// A picture's bytes, if the store has them.
#[must_use]
pub fn load(dir: &str, hash: &str) -> Option<Vec<u8>> {
    if !assets::is_hash(hash) {
        return None;
    }
    fs::read(picture_path(dir, hash)).ok()
}

/// Store a picture under its hash: a temporary name, then a rename, so a
/// reader never sees half a file. Storing what is already there does nothing.
pub fn store(dir: &str, hash: &str, bytes: &[u8]) -> std::io::Result<()> {
    let final_path = picture_path(dir, hash);
    if final_path.exists() {
        return Ok(());
    }
    let pictures = Path::new(dir).join("pictures");
    fs::create_dir_all(&pictures)?;
    let temp = pictures.join(format!(".{hash}.part"));
    fs::write(&temp, bytes)?;
    fs::rename(&temp, &final_path)
}

/// Uploads per account in the last hour. In memory: a restart forgets it, which
/// costs at most one hour's allowance and keeps a counter out of the database.
static RECENT: LazyLock<Mutex<HashMap<String, Vec<Instant>>>> = LazyLock::new(Default::default);

/// Whether `who` may store another picture now, counting this one if so.
fn take_allowance(who: &str, now: Instant) -> bool {
    let Ok(mut recent) = RECENT.lock() else {
        return true;
    };
    let hour = Duration::from_secs(3600);
    let times = recent.entry(who.to_string()).or_default();
    times.retain(|t| now.duration_since(*t) < hour);
    if times.len() >= MAX_PICTURES_PER_HOUR {
        return false;
    }
    times.push(now);
    true
}

// ── HTTP ────────────────────────────────────────────────────────────────────

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;

use rusqlite::OptionalExtension as _;

use crate::http::{internal, require_session, ApiError, ApiResult, HttpState};

/// `POST /api/pictures` (an encrypted body) and `GET /api/pictures/{hash}`;
/// and a person's own picture, `PUT /api/pictures/mine`, with everybody's
/// looked up by name at `GET /api/pictures/people?names=a,b`.
///
/// The people routes live under this prefix rather than a new one because the
/// public host routes `/api/pictures/*` to the simulator already, and every
/// other `/api` path there is listed by hand.
pub fn routes() -> Router<HttpState> {
    Router::new()
        .route(
            "/api/pictures",
            post(upload).layer(axum::extract::DefaultBodyLimit::max(MAX_PICTURE_BYTES)),
        )
        .route("/api/pictures/mine", axum::routing::put(set_mine))
        .route("/api/pictures/people", get(people))
        .route("/api/pictures/{hash}", get(download))
}

/// Most names one lookup answers: a screen's worth of discs, with room over.
pub const MAX_PEOPLE_PER_LOOKUP: usize = 100;

#[derive(serde::Deserialize)]
struct SetPicture {
    /// A hash in this store, or `null` to go back to the initial.
    picture: Option<String>,
}

/// Set the caller's own picture. The picture is uploaded first, as any other
/// (`POST /api/pictures`), and this names it: a hash the store does not hold
/// is refused, so a disc can never point at nothing.
async fn set_mine(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<SetPicture>,
) -> ApiResult<Json<serde_json::Value>> {
    let (principal, username) = require_session(&state, &headers)?;
    if let Some(hash) = &body.picture {
        let dir = state.config.asset_dir.clone();
        let h = hash.clone();
        let held = tokio::task::spawn_blocking(move || {
            assets::is_hash(&h) && picture_path(&dir, &h).is_file()
        })
        .await
        .map_err(|e| internal(anyhow::anyhow!("picture check panicked: {e}")))?;
        if !held {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "upload the picture first; that one is not in the store".into(),
            ));
        }
    }
    let picture = body.picture.clone();
    crate::http::with_db(&state, move |c| {
        c.execute(
            "UPDATE accounts SET picture = ?1 WHERE id = ?2",
            rusqlite::params![picture, principal.to_string()],
        )?;
        Ok(())
    })?;
    tracing::info!(%username, picture = ?body.picture, "profile picture set");
    Ok(Json(serde_json::json!({ "picture": body.picture })))
}

#[derive(serde::Deserialize)]
struct PeopleQuery {
    /// Usernames, comma-separated.
    #[serde(default)]
    names: String,
}

/// Each named person's picture: a hash, or `null` for none (and for nobody by
/// that name, which a caller cannot tell apart — this is not a way to find out
/// who has an account). Keyed by the name as asked, lowercased.
async fn people(
    State(state): State<HttpState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<PeopleQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    require_session(&state, &headers)?;
    let names: Vec<String> = q
        .names
        .split(',')
        .map(|n| n.trim().to_lowercase())
        .filter(|n| !n.is_empty())
        .take(MAX_PEOPLE_PER_LOOKUP)
        .collect();
    let found = crate::http::with_db(&state, move |c| {
        let mut stmt = c.prepare_cached(
            "SELECT picture FROM accounts WHERE username = ?1 COLLATE NOCASE AND disabled = 0",
        )?;
        let mut out = serde_json::Map::new();
        for name in names {
            let picture: Option<String> =
                stmt.query_row([&name], |r| r.get(0)).optional()?.flatten();
            out.insert(name, picture.map_or(serde_json::Value::Null, Into::into));
        }
        Ok(out)
    })?;
    Ok(Json(serde_json::json!({ "pictures": found })))
}

/// Anyone signed in may store a picture. The answer is its hash, which is what
/// the sealed message names.
async fn upload(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let (principal, username) = require_session(&state, &headers)?;
    if body.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "an empty picture".into()));
    }
    if !take_allowance(&principal.to_string(), Instant::now()) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            format!("at most {MAX_PICTURES_PER_HOUR} pictures an hour; try again later"),
        ));
    }
    let hash = assets::content_hash(&body);
    let size = body.len();
    {
        let dir = state.config.asset_dir.clone();
        let hash = hash.clone();
        tokio::task::spawn_blocking(move || store(&dir, &hash, &body))
            .await
            .map_err(|e| internal(anyhow::anyhow!("picture store panicked: {e}")))?
            .map_err(|e| internal(anyhow::anyhow!("could not store the picture: {e}")))?;
    }
    tracing::info!(%hash, %username, bytes = size, "picture stored");
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "hash": hash, "bytes": size })),
    )
        .into_response())
}

/// The picture's (encrypted) bytes, signed in, cached forever.
async fn download(
    State(state): State<HttpState>,
    headers: HeaderMap,
    UrlPath(hash): UrlPath<String>,
) -> ApiResult<Response> {
    require_session(&state, &headers)?;
    let dir = state.config.asset_dir.clone();
    let loaded = tokio::task::spawn_blocking(move || load(&dir, &hash))
        .await
        .map_err(|e| internal(anyhow::anyhow!("picture read panicked: {e}")))?;
    let Some(bytes) = loaded else {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such picture".into()));
    };
    Ok((
        [
            ("content-type", "application/octet-stream"),
            ("cache-control", "private, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_picture_reads_back_and_a_bad_name_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().to_str().unwrap();
        let bytes = b"not really a picture";
        let hash = assets::content_hash(bytes);
        store(dir, &hash, bytes).unwrap();
        store(dir, &hash, bytes).unwrap();
        assert_eq!(load(dir, &hash).as_deref(), Some(&bytes[..]));
        assert_eq!(load(dir, "../../etc/passwd"), None);
    }

    #[test]
    fn an_account_runs_out_of_pictures_for_the_hour() {
        let now = Instant::now();
        let who = "allowance-test-account";
        for _ in 0..MAX_PICTURES_PER_HOUR {
            assert!(take_allowance(who, now));
        }
        assert!(!take_allowance(who, now));
        assert!(take_allowance("somebody-else", now));
        assert!(take_allowance(who, now + Duration::from_secs(3601)));
    }
}
