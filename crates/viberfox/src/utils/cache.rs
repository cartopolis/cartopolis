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
    if let Some(root) = workspace_root() {
        return root.join("cache");
    }
    PathBuf::from("cache")
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
