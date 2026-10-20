//! Composed shaders, kept between runs.
//!
//! bevy composes every WESL shader variant to WGSL the first time a pipeline
//! needs it. On the web that runs on the page's one thread, and over a load it
//! was ~2 s of main-thread time, some of it mid-drive as new kinds of mesh came
//! into view (`docs/notes/wasm-performance.md`). The vendored `bevy_shader`
//! (`crates/bevy_shader`) asks a [`CompiledShaderStore`] before composing and
//! offers it everything it composes. This is that store: one blob in
//! [`platform::storage`]'s cache, read once at startup and written back a few
//! seconds after anything new was composed.
//!
//! **A stale entry cannot be used.** `bevy_shader` keys an entry by the main
//! module's source, defs and constants, and only takes it if every module it was
//! composed from is still registered with the same source. A deploy that changes a
//! shader therefore misses and recomposes. The blob keeps the entries used this
//! run first and drops the oldest past [`MAX_BYTES`], so it does not grow
//! deploy after deploy.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy::shader::{CompiledShaderStore, StoredShader};
use bevy::tasks::IoTaskPool;

use crate::platform::storage::{self, Namespace};
use crate::platform::sync::{LazyLock, Mutex};

/// The storage key. The `v1` is this file's encoding. `bevy_shader::stored::FORMAT`
/// covers the composition, inside every entry's key.
const KEY: &str = "shaders/wgsl-v1.bin";

/// The blob's ceiling, in bytes of WGSL. A full Groningen session composes ~60
/// variants, a few MB.
const MAX_BYTES: usize = 24 << 20;

/// How long after the last new shader the blob is written, in seconds. Shaders
/// arrive in bursts while a scene loads, and each write is the whole blob.
const SAVE_AFTER_S: f64 = 5.0;

const MAGIC: &[u8; 8] = b"CWGSL\x01\0\0";

#[derive(Default)]
struct State {
    entries: HashMap<u64, StoredShader>,
    /// Entries looked up or composed this run, kept first when the blob is cut.
    used: HashSet<u64>,
    /// Something was composed that the saved blob does not have.
    dirty: bool,
    /// The startup read has finished, found or not. Nothing is written before
    /// it, or a quick write would replace the saved blob with this run's few.
    loaded: bool,
    /// Seconds since startup at the last `put`.
    last_put: f64,
    /// The main module of each shader composed since the last write, for its log
    /// line: on a second run this should be empty, and what is in it is a key
    /// that does not hold still.
    composed: Vec<String>,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(Default::default);

fn state() -> crate::platform::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Seconds since the store was installed, from any thread.
fn now_s() -> f64 {
    static START: LazyLock<bevy::platform::time::Instant> =
        LazyLock::new(bevy::platform::time::Instant::now);
    START.elapsed().as_secs_f64()
}

struct Store;

impl CompiledShaderStore for Store {
    fn get(&self, key: u64) -> Option<StoredShader> {
        let mut state = state();
        let hit = state.entries.get(&key).cloned();
        if hit.is_some() {
            state.used.insert(key);
        }
        hit
    }

    fn put(&self, key: u64, shader: StoredShader) {
        let mut state = state();
        state.used.insert(key);
        state.dirty = true;
        state.last_put = now_s();
        let main = shader
            .modules
            .first()
            .map_or_else(String::new, |(path, _)| path.clone());
        state.composed.push(main);
        state.entries.insert(key, shader);
    }
}

pub struct ShaderStorePlugin;

impl Plugin for ShaderStorePlugin {
    fn build(&self, app: &mut App) {
        // `CARTO_NO_SHADER_STORE` (`?no_shader_store`): compose everything, read
        // and write nothing — the bisecting switch for "is it the stored shaders".
        if crate::debug_switches::env_flag("CARTO_NO_SHADER_STORE") {
            return;
        }
        // Installed here, before any pipeline exists, so the first compose asks.
        // A second app in the same process (a test) finds it installed already.
        bevy::shader::set_compiled_shader_store(Box::new(Store));
        let _ = now_s();
        load_saved();
        app.add_systems(Last, save_when_settled);
    }
}

/// Read the saved blob and merge it under anything composed in the meantime.
///
/// Started from `build`, not a `Startup` system, and decoded in the future
/// rather than on a worker: the first pipelines are composed in the first
/// rendered frames, and every one composed before this lands is composed again.
/// On the web the workers are queued with map tiles by then, and the decode is a
/// few milliseconds of copying.
fn load_saved() {
    let Some(pool) = IoTaskPool::try_get() else {
        return;
    };
    pool.spawn(async {
        let entries = storage::get(Namespace::Cache, KEY)
            .await
            .and_then(|bytes| decode(&bytes))
            .unwrap_or_default();
        let n = entries.len();
        let mut state = state();
        for (key, shader) in entries {
            state.entries.entry(key).or_insert(shader);
        }
        state.loaded = true;
        tracing::info!(entries = n, "composed shaders read back");
    })
    .detach();
}

/// Write the blob once a burst of composes has gone quiet.
fn save_when_settled() {
    let snapshot = {
        let mut state = state();
        if !state.loaded || !state.dirty || now_s() - state.last_put < SAVE_AFTER_S {
            return;
        }
        state.dirty = false;
        let composed = std::mem::take(&mut state.composed);
        // The first few by name: a first run lists dozens, a later one should list
        // none, and the names it does list are the keys that do not hold still.
        let named = &composed[..composed.len().min(8)];
        tracing::info!(n = composed.len(), first = ?named, "composed shaders not in the store");
        let mut keys: Vec<u64> = state.entries.keys().copied().collect();
        // Used this run first, then the rest; within each, by key for a stable blob.
        keys.sort_by_key(|k| (!state.used.contains(k), *k));
        let mut total = 0usize;
        let mut kept = Vec::new();
        for key in keys {
            let shader = &state.entries[&key];
            total += shader.wgsl.len();
            if total > MAX_BYTES {
                break;
            }
            kept.push((key, shader.clone()));
        }
        kept
    };
    IoTaskPool::get()
        .spawn(async move {
            let n = snapshot.len();
            let Some(bytes) =
                crate::platform::workers::compute("shader store", move || encode(&snapshot)).await
            else {
                return;
            };
            storage::put(Namespace::Cache, KEY, &bytes).await;
            tracing::debug!(entries = n, bytes = bytes.len(), "composed shaders saved");
        })
        .detach();
}

fn encode(entries: &[(u64, StoredShader)]) -> Vec<u8> {
    fn bytes(out: &mut Vec<u8>, b: &[u8]) {
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(b);
    }
    let mut out = Vec::with_capacity(entries.iter().map(|(_, s)| s.wgsl.len() + 256).sum());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (key, shader) in entries {
        out.extend_from_slice(&key.to_le_bytes());
        out.extend_from_slice(&(shader.modules.len() as u32).to_le_bytes());
        for (path, hash) in &shader.modules {
            bytes(&mut out, path.as_bytes());
            out.extend_from_slice(&hash.to_le_bytes());
        }
        bytes(&mut out, shader.wgsl.as_bytes());
    }
    out
}

/// `None` for anything that is not a whole blob of this version: a cache entry
/// is only ever a speed-up, so a bad one is dropped, never trusted in part.
fn decode(data: &[u8]) -> Option<Vec<(u64, StoredShader)>> {
    struct Reader<'a>(&'a [u8]);
    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            let (head, tail) = (self.0.get(..n)?, self.0.get(n..)?);
            self.0 = tail;
            Some(head)
        }
        fn u32(&mut self) -> Option<u32> {
            Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
        }
        fn u64(&mut self) -> Option<u64> {
            Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
        }
        fn string(&mut self) -> Option<String> {
            let n = self.u32()? as usize;
            String::from_utf8(self.take(n)?.to_vec()).ok()
        }
    }
    let mut r = Reader(data);
    if r.take(MAGIC.len())? != MAGIC {
        return None;
    }
    let count = r.u32()?;
    let mut entries = Vec::new();
    for _ in 0..count {
        let key = r.u64()?;
        let n = r.u32()?;
        let mut modules = Vec::new();
        for _ in 0..n {
            let path = r.string()?;
            modules.push((path, r.u64()?));
        }
        let wgsl = r.string()?;
        entries.push((key, StoredShader { wgsl, modules }));
    }
    r.0.is_empty().then_some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<(u64, StoredShader)> {
        vec![
            (
                7,
                StoredShader {
                    wgsl: "fn main() {}".into(),
                    modules: vec![
                        ("shaders/water.wesl".into(), 1),
                        ("embedded://x.wesl".into(), 2),
                    ],
                },
            ),
            (
                u64::MAX,
                StoredShader {
                    wgsl: String::new(),
                    modules: Vec::new(),
                },
            ),
        ]
    }

    #[test]
    fn a_blob_reads_back_as_written() {
        assert_eq!(decode(&encode(&sample())), Some(sample()));
    }

    /// A truncated write (a tab closed mid-`put`) or trailing garbage is no blob.
    #[test]
    fn a_damaged_blob_is_dropped_whole() {
        let blob = encode(&sample());
        for cut in [0, 7, 12, blob.len() - 1] {
            assert_eq!(decode(&blob[..cut]), None, "cut at {cut}");
        }
        let mut longer = blob.clone();
        longer.push(0);
        assert_eq!(decode(&longer), None);
        let mut other = blob;
        other[5] = 2;
        assert_eq!(decode(&other), None);
    }
}
