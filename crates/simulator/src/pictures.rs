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

use crate::http::{internal, require_session, ApiError, ApiResult, HttpState};

/// `POST /api/pictures` (an encrypted body) and `GET /api/pictures/{hash}`.
pub fn routes() -> Router<HttpState> {
    Router::new()
        .route(
            "/api/pictures",
            post(upload).layer(axum::extract::DefaultBodyLimit::max(MAX_PICTURE_BYTES)),
        )
        .route("/api/pictures/{hash}", get(download))
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
