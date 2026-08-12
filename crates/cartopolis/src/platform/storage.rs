//! Keyed blob cache (ADR-033 KvStore seam).
//!
//! A tiny async get/put/remove over `/`-separated string keys. Native maps the key
//! to a path under [`cache_root`](crate::utils::cache::cache_root) and uses
//! `std::fs`; the wasm build (behind `cfg`) will use IndexedDB. Callers run these
//! inside `IoTaskPool` tasks — on native the fs work blocks a pool thread (fine),
//! on wasm the storage calls genuinely `await`. The `async` signature is what lets
//! one call site serve both.
//!
//! Keys are relative paths, e.g. `"buildings/lod22/14/134/86.json"` or
//! `"tiles/vector/14/8/5.mvt"` — the same layout the old per-file cache paths built
//! by hand under `cache_root()`.

/// Read a cached blob, or `None` if absent (or unreadable).
pub async fn get(key: &str) -> Option<Vec<u8>> {
    imp::get(key).await
}

/// Read a cached blob and decode it as UTF-8.
pub async fn get_string(key: &str) -> Option<String> {
    get(key).await.and_then(|b| String::from_utf8(b).ok())
}

/// Write a blob, creating parent directories as needed. Best-effort: a failed
/// write just means a future miss, never an error to the caller.
pub async fn put(key: &str, bytes: &[u8]) {
    imp::put(key, bytes).await;
}

/// Remove a cached entry (e.g. to invalidate a corrupt one).
pub async fn remove(key: &str) {
    imp::remove(key).await;
}

/// Seconds since the entry was last written, if it exists — for TTL freshness
/// checks (e.g. the transit line list).
#[allow(dead_code)] // used by the fs-coupled fetchers still to be migrated.
pub async fn age_secs(key: &str) -> Option<f64> {
    imp::age_secs(key).await
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::path::PathBuf;

    /// Resolve a `/`-separated key to a path under the shared cache root. Splitting
    /// and joining per segment keeps it correct on every OS (rather than trusting
    /// `/` inside a single `join`).
    fn path(key: &str) -> PathBuf {
        key.split('/')
            .filter(|s| !s.is_empty())
            .fold(crate::utils::cache::cache_root(), |p, seg| p.join(seg))
    }

    pub async fn get(key: &str) -> Option<Vec<u8>> {
        std::fs::read(path(key)).ok()
    }

    pub async fn put(key: &str, bytes: &[u8]) {
        let p = path(key);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Written through a temp file and a rename rather than straight into place:
        // a truncating write leaves the entry short for as long as it takes to fill,
        // and on Android that window is not theoretical — the system kills
        // backgrounded processes as a matter of routine, so being interrupted
        // mid-write is the ordinary case. The cost of landing in it is permanent,
        // because the streamers cache the *bytes* and re-read them forever: a torn
        // `.mvt` is a tile that fails to decode on every launch until something
        // happens to call `remove` on it, and a torn `.png` is a texture that never
        // appears. See `utils::cache::write_atomic` for why the flush matters too.
        let _ = crate::utils::cache::write_atomic(&p, bytes);
    }

    pub async fn remove(key: &str) {
        let _ = std::fs::remove_file(path(key));
    }

    pub async fn age_secs(key: &str) -> Option<f64> {
        let modified = std::fs::metadata(path(key)).ok()?.modified().ok()?;
        modified.elapsed().ok().map(|d| d.as_secs_f64())
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    //! IndexedDB-backed cache. A no-op stub until the wasm storage impl lands
    //! (ADR-033) — a wasm build then simply re-fetches instead of hitting a cache,
    //! which is correct, just unoptimised. The seam's shape is identical to native
    //! so every call site compiles unchanged.
    pub async fn get(_key: &str) -> Option<Vec<u8>> {
        None
    }
    pub async fn put(_key: &str, _bytes: &[u8]) {}
    pub async fn remove(_key: &str) {}
    pub async fn age_secs(_key: &str) -> Option<f64> {
        None
    }
}
