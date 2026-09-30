//! The video store: clips a prim can play (protocol 28,
//! `docs/notes/video-on-a-prim.md`).
//!
//! Beside the model store rather than inside it. The model store's tables and
//! routes are about glTF — triangle counts, licences, a library — and a clip is
//! none of that; what the two share is the idea, which is borrowed: a file is
//! named by the hash of its bytes (`assets::content_hash`), written to a
//! temporary name and renamed, and served cached forever because a hash never
//! changes.
//!
//! What is checked at upload is the container, not the pictures: WebM, one VP8
//! video track within `video_limits`, at most one Opus audio track, and
//! nothing else. The server never decodes a frame — decoding is every viewer's
//! job, and each viewer's decoder has its own pixel ceiling
//! (`cartopolis_calls::video::MAX_PIXELS`) for a file that lies in its header.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use cartopolis_core::video_limits::{
    MAX_VIDEO_BYTES, MAX_VIDEO_HEIGHT, MAX_VIDEO_MS, MAX_VIDEO_WIDTH,
};
use matroska_demuxer::{Frame, MatroskaFile, TrackType};

use crate::assets;
use crate::db::VideoRow;

fn video_path(dir: &str, hash: &str) -> PathBuf {
    Path::new(dir).join("videos").join(format!("{hash}.webm"))
}

/// Measure a clip, or say in words a person can act on why it is refused.
pub fn inspect(bytes: &[u8]) -> Result<VideoRow, String> {
    if bytes.len() > MAX_VIDEO_BYTES {
        return Err(format!(
            "that clip is {} MB; the limit is {} MB",
            bytes.len() / (1024 * 1024),
            MAX_VIDEO_BYTES / (1024 * 1024)
        ));
    }
    let mut mkv =
        MatroskaFile::open(Cursor::new(bytes)).map_err(|e| format!("not a WebM file ({e})"))?;
    let mut video = None;
    let mut has_audio = false;
    for t in mkv.tracks() {
        match t.track_type() {
            TrackType::Video => {
                if video.is_some() {
                    return Err("a clip may have one video track".into());
                }
                if t.codec_id() != "V_VP8" {
                    return Err(format!(
                        "the video is {}; only VP8 is played (re-encode with -c:v libvpx)",
                        t.codec_id()
                    ));
                }
                let v = t.video().ok_or("the video track has no picture size")?;
                let (w, h) = (v.pixel_width().get() as u32, v.pixel_height().get() as u32);
                if w > MAX_VIDEO_WIDTH || h > MAX_VIDEO_HEIGHT {
                    return Err(format!(
                        "the picture is {w}×{h}; the limit is {MAX_VIDEO_WIDTH}×{MAX_VIDEO_HEIGHT}"
                    ));
                }
                video = Some((t.track_number().get(), w, h));
            }
            TrackType::Audio => {
                if has_audio {
                    return Err("a clip may have one sound track".into());
                }
                if t.codec_id() != "A_OPUS" {
                    return Err(format!(
                        "the sound is {}; only Opus is played (re-encode with -c:a libopus)",
                        t.codec_id()
                    ));
                }
                has_audio = true;
            }
            _ => {}
        }
    }
    let (track, width, height) = video.ok_or("there is no video track")?;

    // The duration, from the last video frame rather than the header's claim:
    // it is a bound this server enforces, and a header can say anything.
    let scale = mkv.info().timestamp_scale().get();
    let mut frame = Frame::default();
    let mut last = 0u64;
    let mut pictures = 0u64;
    while mkv
        .next_frame(&mut frame)
        .map_err(|e| format!("the file is damaged ({e})"))?
    {
        if frame.track == track {
            last = last.max(frame.timestamp);
            pictures += 1;
        }
    }
    if pictures == 0 {
        return Err("the video track has no pictures".into());
    }
    let duration_ms = last.saturating_mul(scale) / 1_000_000;
    if duration_ms > MAX_VIDEO_MS {
        return Err(format!(
            "the clip is {} minutes; the limit is {}",
            duration_ms / 60_000,
            MAX_VIDEO_MS / 60_000
        ));
    }
    Ok(VideoRow {
        hash: assets::content_hash(bytes),
        byte_size: bytes.len() as u64,
        width,
        height,
        duration_ms,
        has_audio,
    })
}

/// A clip's bytes, if the store has them.
#[must_use]
pub fn load(dir: &str, hash: &str) -> Option<Vec<u8>> {
    if !assets::is_hash(hash) {
        return None;
    }
    fs::read(video_path(dir, hash)).ok()
}

/// Store a clip under its hash: a temporary name, then a rename, so a reader
/// never sees half a file. Storing what is already there does nothing.
pub fn store(dir: &str, hash: &str, bytes: &[u8]) -> std::io::Result<()> {
    let final_path = video_path(dir, hash);
    if final_path.exists() {
        return Ok(());
    }
    let videos = Path::new(dir).join("videos");
    fs::create_dir_all(&videos)?;
    let temp = videos.join(format!(".{hash}.part"));
    fs::write(&temp, bytes)?;
    fs::rename(&temp, &final_path)
}

// ── HTTP ────────────────────────────────────────────────────────────────────

use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;

use crate::http::{internal, require_session, with_db, ApiError, ApiResult, HttpState};

/// `POST /api/videos` (a WebM body) and `GET /api/videos/{hash}.webm`.
pub fn routes() -> Router<HttpState> {
    Router::new()
        .route(
            "/api/videos",
            // The body limit is raised on this route alone, to the clip limit.
            post(upload).layer(axum::extract::DefaultBodyLimit::max(MAX_VIDEO_BYTES)),
        )
        .route("/api/videos/{name}", get(download))
}

/// Anyone signed in may upload a clip; what they may *play* it on is decided
/// by who may edit the prim (`PrimService::set_video`). The answer is the
/// clip's measurements, its hash included — what `SetPrimVideo` names.
async fn upload(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let (principal, username) = require_session(&state, &headers)?;
    let bytes = body.to_vec();
    // Walking a 64 MB container is CPU and the runtime is shared with every
    // connection, so it and the write go off the async workers.
    let (row, bytes) = tokio::task::spawn_blocking(move || inspect(&bytes).map(|r| (r, bytes)))
        .await
        .map_err(|e| internal(anyhow::anyhow!("video inspection panicked: {e}")))?
        .map_err(|msg| ApiError(StatusCode::BAD_REQUEST, msg))?;
    {
        let dir = state.config.asset_dir.clone();
        let hash = row.hash.clone();
        tokio::task::spawn_blocking(move || store(&dir, &hash, &bytes))
            .await
            .map_err(|e| internal(anyhow::anyhow!("video store panicked: {e}")))?
            .map_err(|e| internal(anyhow::anyhow!("could not store the video: {e}")))?;
    }
    let owner = principal.to_string();
    let saved = row.clone();
    with_db(&state, move |c| crate::db::insert_video(c, &saved, &owner))?;
    tracing::info!(hash = %row.hash, %username, bytes = row.byte_size, ms = row.duration_ms, "video uploaded");
    Ok((StatusCode::CREATED, Json(row)).into_response())
}

/// The clip's bytes, signed in, cached forever (the name is the content's hash).
async fn download(
    State(state): State<HttpState>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<Response> {
    require_session(&state, &headers)?;
    let hash = name.strip_suffix(".webm").unwrap_or(&name).to_string();
    let dir = state.config.asset_dir.clone();
    let loaded = tokio::task::spawn_blocking(move || load(&dir, &hash))
        .await
        .map_err(|e| internal(anyhow::anyhow!("video read panicked: {e}")))?;
    let Some(bytes) = loaded else {
        return Err(ApiError(StatusCode::NOT_FOUND, "no such video".into()));
    };
    Ok((
        [
            ("content-type", "video/webm"),
            ("cache-control", "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WebM this module did not write: the smallest valid-looking clip is
    /// easier to get wrong by hand than to take from a real muxer, so the
    /// refusals are tested on what is not a clip, and a real clip is exercised
    /// by the integration suite.
    #[test]
    fn what_is_not_a_clip_is_refused_in_words() {
        assert!(inspect(b"not a video").unwrap_err().contains("not a WebM"));
        let big = vec![0u8; MAX_VIDEO_BYTES + 1];
        assert!(inspect(&big).unwrap_err().contains("limit is 64 MB"));
    }

    #[test]
    fn a_clip_is_stored_under_its_hash_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_str().unwrap();
        let hash = assets::content_hash(b"clip");
        store(d, &hash, b"clip").unwrap();
        assert_eq!(load(d, &hash).as_deref(), Some(&b"clip"[..]));
        assert_eq!(load(d, "../etc/passwd"), None);
    }
}
