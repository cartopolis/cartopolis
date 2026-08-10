//! Resolve the on-disk cache root once, independent of the current directory.
//!
//! Every streamer (tiles, buildings, data layers, transit) caches under a shared
//! `cache/` tree. The path used to be a bare cwd-relative `PathBuf::from("cache")`,
//! so launching the client with its working directory set to a crate dir (an IDE
//! run config, or `cargo run` invoked inside `crates/viberfox/`) scattered a second
//! `crates/viberfox/cache/` next to the repo-root one. `cache_root()` anchors the
//! tree to one stable location:
//!
//! 1. `VIBE_CACHE_DIR`, if set — an explicit override (also lets a packaged build
//!    point at a writable data dir).
//! 2. otherwise the workspace root, found by walking up from the current dir for the
//!    `Cargo.toml` that declares `[workspace]` (so a dev run lands in repo-root
//!    `cache/` no matter which crate dir it started in).
//! 3. otherwise `cache/` relative to the cwd — the original behaviour, which is what
//!    a distributed binary run outside any checkout should use.
//!
//! [`app_data_root`] answers the *other* half of the same question — where the
//! things the user would be upset to lose live (`user.json`) — and it deliberately
//! resolves differently, because a cache is disposable and a profile is not.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The shared cache root, resolved once per process. Streamers `.join(...)` their
/// own namespace onto it (`cache_root().join("tiles")`, `.join("buildings")`, …).
pub fn cache_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(resolve_cache_root).clone()
}

fn resolve_cache_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("VIBE_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    // Android has no cwd worth resolving against — a NativeActivity process starts
    // in `/`, which is read-only, and there is no checkout to walk up to. Both
    // fallbacks below would therefore hand back an unwritable `cache/`, and because
    // every write here is best-effort the failure is *silent*: the app runs, and
    // re-fetches every tile it has already fetched, forever. The app-private dir
    // the framework hands us is the only writable place we can reach without JNI.
    //
    // **It is not the directory "Clear cache" clears**, whatever an earlier version
    // of this comment claimed. `AndroidApp::internal_data_path` returns the value
    // the framework passed in `ANativeActivity::internalDataPath`, which is
    // `getFilesDir()` — app *data*, counted as such in Settings, kept until the user
    // clears app data or uninstalls (verified against
    // `android-activity-0.6.0/src/native_activity/mod.rs:447`). `getCacheDir()` is a
    // sibling the framework will evict under storage pressure, and it is the correct
    // home for this tree — but reaching it needs a JNI call through the activity's
    // `Context`, which this crate does not make today. **This is the spot**: when the
    // JNI seam lands, resolve `getCacheDir()` here and fall back to the line below.
    // Until then [`sweep_cache_root`] is what keeps the directory from growing
    // without bound, because nothing else will reclaim it.
    #[cfg(target_os = "android")]
    if let Some(app) = bevy::android::ANDROID_APP.get() {
        if let Some(path) = app.internal_data_path() {
            return path.join("cache");
        }
    }
    if let Some(root) = workspace_root() {
        return root.join("cache");
    }
    PathBuf::from("cache")
}

/// Where the user's own data lives — today just `user.json` (bookmarks, Home, the
/// remembered session, layer prefs, avatar appearance). Resolved once per process,
/// the same shape as [`cache_root`] and for a sharper version of the same reason.
///
/// The path used to be a bare cwd-relative `data/`, and on Android that is fatal
/// rather than untidy: a NativeActivity process starts in `/`, so `create_dir_all`
/// and the write behind it fail on every autosave, the failure is a `warn!` nobody
/// reads on a phone, and **nothing the user does in the app survives switching away
/// from it**. On desktop the same line is merely surprising — launching from a
/// different directory silently hands you a different profile, which reads as "my
/// bookmarks are gone".
///
/// The order below is deliberate:
///
/// 1. `VIBE_DATA_DIR`, the explicit override (`VIBE_CACHE_DIR`'s counterpart).
/// 2. on Android, the app-private data dir — the only writable answer, and the one
///    the system keeps until the app's data is cleared. Note it is *data*, not
///    cache: the profile must outlive a "Clear cache" (see `resolve_cache_root`).
/// 3. an **existing** `data/` in the checkout, so a dev box keeps using the profile
///    it already has instead of appearing to lose it on the first run after this
///    change. Anchored to the workspace root rather than the cwd, which also fixes
///    the "different directory, different profile" surprise.
/// 4. an **existing** `data/` beside the cwd, which is what a packaged build laid
///    out that way already uses.
/// 5. otherwise the XDG user-data dir. This is the only branch a fresh install
///    reaches, and it is the only one that is writable no matter where the binary
///    was started from.
pub fn app_data_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(resolve_app_data_root).clone()
}

fn resolve_app_data_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("VIBE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(target_os = "android")]
    if let Some(app) = bevy::android::ANDROID_APP.get() {
        if let Some(path) = app.internal_data_path() {
            return path.join("data");
        }
    }
    if let Some(dir) = workspace_root()
        .map(|r| r.join("data"))
        .filter(|d| d.is_dir())
    {
        return dir;
    }
    let cwd_data = PathBuf::from("data");
    if cwd_data.is_dir() {
        return cwd_data;
    }
    user_data_dir().unwrap_or(cwd_data)
}

/// `$XDG_DATA_HOME/viberfox`, or `$HOME/.local/share/viberfox` — the freedesktop
/// spelling, hand-rolled because the tree has no `dirs`-style dependency and this is
/// the only place that wants one. `None` when neither variable is set (a Windows
/// build, or a service account with no home), which leaves the caller on its
/// relative fallback.
fn user_data_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("viberfox"));
    }
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("viberfox"),
    )
}

/// Walk up from the current directory for the workspace manifest; fall back to the
/// nearest ancestor holding a `.git` dir if no `[workspace]` manifest is found.
fn workspace_root() -> Option<PathBuf> {
    let start = std::env::current_dir().ok()?;
    let mut git_fallback = None;
    for dir in start.ancestors() {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() && manifest_declares_workspace(&manifest) {
            return Some(dir.to_path_buf());
        }
        if git_fallback.is_none() && dir.join(".git").exists() {
            git_fallback = Some(dir.to_path_buf());
        }
    }
    git_fallback
}

fn manifest_declares_workspace(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s.lines().any(|l| l.trim_start().starts_with("[workspace]")))
        .unwrap_or(false)
}

/// Write `bytes` to `path` so that a reader only ever sees the old file or the new
/// one — never half of either. Used by the blob cache (`platform::storage::put`) and
/// by the user store's save.
///
/// A plain `fs::write` truncates first and then fills, so *every* moment between
/// those two is a window in which the file on disk is short. On a desktop that
/// window closes in microseconds and only a crash can catch it; on Android the OS
/// kills backgrounded processes as a matter of routine, so landing in that window is
/// ordinary rather than exceptional. The consequences differ by file and both are
/// bad: a torn `.mvt`/`.png` is a cache entry that decodes to garbage forever (the
/// streamers cache the *bytes*, not the verdict), and a torn `user.json` is
/// unparseable, which used to mean every bookmark gone.
///
/// `sync_all` before the rename is the part that is easy to leave out and matters
/// most: the rename is atomic with respect to *readers*, but without the flush a
/// power loss can leave the metadata operation committed ahead of the data it
/// points at, i.e. an atomically-renamed file full of zeroes.
///
/// The temp name carries the pid and a counter so two writers of the same key —
/// several tile workers, or a second client sharing the directory — cannot scribble
/// over each other's half-written file and then rename the result into place.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut tmp = path.to_path_buf().into_os_string();
    tmp.push(tmp_suffix());
    let tmp = PathBuf::from(tmp);

    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        // Leaving the temp behind would be a leak the sweep below has to clean up;
        // it is a cache file's worth of bytes, but the same helper writes the user
        // store, where a stray sibling is just confusing.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// A suffix no other in-flight write can be using: the process id (writers in other
/// processes) plus a counter (writers in this one).
fn tmp_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        ".{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Ceiling on the on-disk cache, in bytes.
///
/// Nothing bounded this before: `platform::storage::remove` has two callers (both
/// invalidating a single corrupt entry) and nothing ever walked the tree, so the
/// cache grew for as long as the app was used. A few hours of flying over a city is
/// tens of thousands of tiles, buildings and detail patches — on a desktop that is
/// untidy, on a phone it is the app quietly eating the user's storage with no way to
/// get it back short of clearing app data (see `resolve_cache_root` on why "Clear
/// cache" does not reach it yet).
///
/// 512 MiB, chosen rather than measured: a z14 Shortbread body is ~500 KB and the
/// in-memory byte cache holds 32 MiB of them (`tile_source::MAX_CACHE_BYTES`), so
/// this is roughly a thousand tiles' worth of *disk* per source — a couple of
/// cities' worth of revisitable map, which is what the cache is for, and small
/// enough that a phone owner would not notice it. It is one const so a measurement
/// can move it.
pub const MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;

/// Trim the cache to [`MAX_CACHE_BYTES`], oldest file first. Call once at startup.
///
/// Deliberately not a Bevy system and deliberately off the calling thread: walking a
/// tree of tens of thousands of files is tens of milliseconds of `stat` calls at
/// best and seconds on a cold phone filesystem, and startup is the one moment where
/// that is most visible. Everything about it is best-effort — an unreadable
/// directory, a file that vanished under us, a root that does not exist yet on a
/// first run are all "nothing to do", never an error the caller has to handle.
///
/// Startup is also the *right* moment: it is the only time the streamers are
/// guaranteed not to be reading the files we are about to delete, and even if one
/// were, deleting a cache entry can only ever cost a re-fetch.
///
/// There is no wasm arm because there is no wasm module: `utils/mod.rs` compiles
/// this whole file out for the web build, where the browser's own quota is what
/// bounds the store.
pub fn sweep_cache_root() {
    let root = cache_root();
    // A detached thread rather than `platform::workers::submit`: that pool *is*
    // `AsyncComputeTaskPool`, whose threads the tile decodes and Bevy's screenshot
    // readback already contend for (see `platform::workers::set_paused`), and a
    // blocking directory walk is exactly the kind of job that starves it.
    // The handle is dropped, which detaches the thread: nothing waits for it, and a
    // sweep that is still running when the app exits has simply not run.
    let _ = std::thread::Builder::new()
        .name("vibe-cache-sweep".into())
        .spawn(move || {
            let freed = sweep(&root, MAX_CACHE_BYTES);
            if freed > 0 {
                tracing::info!(
                    freed_mb = freed / (1024 * 1024),
                    cap_mb = MAX_CACHE_BYTES / (1024 * 1024),
                    "trimmed the disk cache"
                );
            }
        });
}

/// Delete the oldest files under `root` until the tree fits in `cap` bytes; returns
/// how many bytes were freed. Split out from [`sweep_cache_root`] so it is testable
/// against a directory that is not the real cache.
fn sweep(root: &Path, cap: u64) -> u64 {
    let mut files = cache_files(root);
    let total: u64 = files.iter().map(|(_, _, len)| len).sum();
    if total <= cap {
        return 0;
    }
    // Oldest first — a least-recently-*written* approximation of LRU. Reads do not
    // touch mtime and asking for atime is a mount option we do not control, so this
    // is the only ordering the filesystem offers for free. It is the right one
    // anyway: a tile that has not been re-fetched in weeks is one nobody is flying
    // over. Files whose mtime is unreadable sort as ancient and go first.
    files.sort_by_key(|(_, modified, _)| *modified);

    let mut freed = 0;
    for (path, _, len) in files {
        if total.saturating_sub(freed) <= cap {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            freed += len;
        }
    }
    freed
}

/// Every file under `root`, with its mtime and size. Iterative rather than
/// recursive: the tree is `tiles/<z>/<x>/<y>` deep today, but a cache root that
/// someone points at the wrong directory should cost a big `Vec`, not a blown stack.
fn cache_files(root: &Path) -> Vec<(PathBuf, std::time::SystemTime, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue; // unreadable or gone: nothing to reclaim here
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                out.push((entry.path(), modified, meta.len()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viberfox-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tiles").join("14")).expect("scratch dir");
        dir
    }

    fn total_bytes(root: &Path) -> u64 {
        cache_files(root).iter().map(|(_, _, len)| len).sum()
    }

    /// The sweep is a bound, not a policy: under the cap it must not touch a single
    /// file (a startup that deleted a working cache would be worse than no sweep).
    #[test]
    fn a_cache_under_the_cap_is_left_alone() {
        let root = scratch("sweep-under");
        for i in 0..8 {
            std::fs::write(
                root.join("tiles").join("14").join(format!("{i}.mvt")),
                [0u8; 64],
            )
            .unwrap();
        }
        assert_eq!(sweep(&root, MAX_CACHE_BYTES), 0);
        assert_eq!(cache_files(&root).len(), 8);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Over the cap it deletes until the tree fits — and stops there, rather than
    /// emptying the directory.
    #[test]
    fn a_cache_over_the_cap_is_trimmed_to_it() {
        let root = scratch("sweep-over");
        for i in 0..10 {
            std::fs::write(
                root.join("tiles").join("14").join(format!("{i}.mvt")),
                [7u8; 100],
            )
            .unwrap();
        }
        let freed = sweep(&root, 450);
        assert!(freed >= 550, "freed {freed} bytes");
        assert!(total_bytes(&root) <= 450, "still over the cap");
        assert!(!cache_files(&root).is_empty(), "the whole cache was wiped");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An atomic write leaves exactly one file behind — no `.tmp` sibling for the
    /// sweep (or a confused user) to find.
    #[test]
    fn atomic_write_leaves_no_temp_file() {
        let root = scratch("atomic");
        let path = root.join("user.json");
        write_atomic(&path, b"{}").unwrap();
        write_atomic(&path, br#"{"version":1}"#).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"version":1}"#);
        assert_eq!(
            std::fs::read_dir(&root).unwrap().count(),
            2,
            "expected only user.json and the tiles/ dir"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
