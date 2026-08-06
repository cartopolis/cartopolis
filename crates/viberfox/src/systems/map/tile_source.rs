//! The one place tile bytes come from: an in-memory byte cache and a
//! single-flight table in front of the [`storage`] and [`http`] seams.
//!
//! Nine loaders want the same source tiles. Before this module each of them
//! de-duplicated only against *its own* in-flight set, so two consumers asking
//! for the same tile in the same frame both missed and both fetched — and the
//! overzoom fan-out makes that large. A fresh anchor at
//! `map_stream::DEFAULT_ANCHOR_ZOOM` asks for a z10–z17 clipmap, 424 tiles, of
//! which the 179 at z15–z17 all resolve (via [`SubTile::resolve`]) to a few dozen
//! distinct z14 bodies — bodies the z14 ring of the same pass is already
//! requesting, and which `map_geometry`, the globe quadtree and the router then
//! ask for again. Every one of those was an independent ~500 KB `std::fs::read`
//! natively and an independent **network fetch** on the web build, where the wasm
//! `storage` impl is still a `None`-returning stub.
//!
//! Two mechanisms, in this order:
//!
//! * **A byte cache**, bounded in bytes (not entries — a dense tile must not be
//!   able to blow the budget) and LRU. Cheap to reason about because the size of
//!   the thing cached is exactly known; see the module-level note on why parsed
//!   readers are *not* cached here.
//! * **Single flight.** The first caller for a key fetches; everyone else who
//!   asks while that is in the air waits on the same answer instead of starting
//!   their own. This is what collapses the fan-out, since the duplicate requests
//!   arrive in the same frame — well inside one fetch's latency.
//!
//! State lives in a process-global rather than a Bevy `Resource` because every
//! caller is a detached `IoTaskPool` task that holds no `World` access.
//!
//! [`SubTile::resolve`]: crate::systems::vector_tiles::SubTile::resolve

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use futures_channel::oneshot;

use crate::platform::{http, storage};
use viberfox_core::TileKey;
use viberfox_core::{format_osm_tile_url, tile_source_kind, TileSourceKind};

/// On-disk cache path for a tile, under the app's `cache/` folder (persists across
/// runs so revisits/LOD bursts don't re-hit the tile server and trip rate limits).
/// The extension tracks the source kind so a raster and a vector cache can coexist
/// (and so switching sources doesn't serve MVT bytes to the PNG decoder).
pub(crate) fn tile_cache_key(key: &TileKey, kind: TileSourceKind) -> String {
    let ext = match kind {
        TileSourceKind::Raster => "png",
        TileSourceKind::Vector => "mvt",
    };
    format!("tiles/{}/{}/{}.{ext}", key.z, key.x, key.y)
}

/// Ceiling on the in-memory byte cache.
///
/// A z14 Shortbread body is ~500 KB, so this holds ~65 of them — comfortably the
/// live z14 neighbourhood of every consumer at once (`map_geometry` streams 3×3,
/// the overzoom pyramid resolves onto a 7×7 z14 ring, a route corridor is capped
/// at `routing::MAX_ROUTE_TILES` = 96 but is fetched once and parsed straight
/// away). Nobody has measured the true working set yet — this const is placed so
/// one edit moves it when someone does.
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;

/// What identifies a cached body. Carries the [`TileSourceKind`] for the same
/// reason [`tile_cache_key`] does: a source switch must not hand MVT
/// bytes to the PNG decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub key: TileKey,
    pub kind: TileSourceKind,
}

/// One cached-and-coalesced byte source.
///
/// A struct rather than bare statics so a test can own an instance: the global
/// one below is shared by every task in the process, and a test that asserted
/// against it would see whatever the previous test left behind.
#[derive(Default)]
pub struct TileSource {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Cached bodies, plus the recency order the eviction walks. A `Vec` for the
    /// order is deliberate: at ~500 KB/tile the cache holds tens of entries, and
    /// even a globe quadtree's thousand tiny z2–z8 bodies make a linear scan of
    /// 20-byte keys cheaper than the bookkeeping an intrusive list would need.
    cache: HashMap<CacheKey, Arc<Vec<u8>>>,
    order: Vec<CacheKey>,
    bytes: usize,
    /// Keys with a fetch in the air, and who is waiting on it.
    in_flight: HashMap<CacheKey, Vec<oneshot::Sender<Result<Arc<Vec<u8>>, String>>>>,
}

/// Where the leader looked for a key's bytes — reported so a caller can log the
/// difference between a cache hit and a fetch without timing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Memory,
    /// Fetched by us (through `storage`/`http`, or a test's stub).
    Fetched,
    /// Another task was already fetching it; we waited on that result.
    Coalesced,
}

impl TileSource {
    /// Bytes for `ck`, fetching with `fetch` only if nobody has them and nobody
    /// is already getting them.
    ///
    /// `fetch` is a closure so the two mechanisms can be tested against a stub
    /// source, and so the real one stays out of this file's arithmetic.
    pub async fn bytes<F, Fut>(
        &self,
        ck: CacheKey,
        fetch: F,
    ) -> (Result<Arc<Vec<u8>>, String>, Origin)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
    {
        // Look, then either claim the fetch or join the queue. The guard is
        // dropped before any `await` — a `std::sync::Mutex` held across one
        // would deadlock the single-threaded wasm executor outright.
        let waiter = {
            let mut st = self.lock();
            if let Some(bytes) = st.touch(&ck) {
                return (Ok(bytes), Origin::Memory);
            }
            match st.in_flight.get_mut(&ck) {
                Some(waiters) => {
                    let (tx, rx) = oneshot::channel();
                    waiters.push(tx);
                    Some(rx)
                }
                None => {
                    st.in_flight.insert(ck, Vec::new());
                    None
                }
            }
        };

        if let Some(rx) = waiter {
            // The leader dropping its sender (a panicking task) cancels this;
            // report it rather than hanging the follower forever.
            let got = rx
                .await
                .unwrap_or_else(|_| Err("tile fetch was cancelled".to_string()));
            return (got, Origin::Coalesced);
        }

        let result = fetch().await.map(Arc::new);

        // Publish to the cache and to everyone who queued up behind us. Doing
        // both under one lock is what makes "in flight" and "cached" a single
        // transition: a follower can never look between them and miss twice.
        {
            let mut st = self.lock();
            let waiters = st.in_flight.remove(&ck).unwrap_or_default();
            if let Ok(bytes) = &result {
                st.insert(ck, bytes.clone());
            }
            for tx in waiters {
                let _ = tx.send(result.clone());
            }
        }
        (result, Origin::Fetched)
    }

    /// Bytes already in memory, if any — a peek that does not fetch.
    #[cfg(test)]
    fn cached(&self, ck: &CacheKey) -> Option<Arc<Vec<u8>>> {
        self.lock().cache.get(ck).cloned()
    }

    #[cfg(test)]
    fn cached_bytes(&self) -> usize {
        self.lock().bytes
    }

    /// A poisoned lock here means some task panicked mid-publish. The invariants
    /// the state holds are "a map and a running total", not anything a torn
    /// update can corrupt into unsoundness, so recovering beats taking the whole
    /// tile pipeline down with it.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl State {
    /// A cache hit, moved to the most-recent end.
    fn touch(&mut self, ck: &CacheKey) -> Option<Arc<Vec<u8>>> {
        let bytes = self.cache.get(ck)?.clone();
        if let Some(i) = self.order.iter().position(|k| k == ck) {
            let k = self.order.remove(i);
            self.order.push(k);
        }
        Some(bytes)
    }

    fn insert(&mut self, ck: CacheKey, bytes: Arc<Vec<u8>>) {
        // A body larger than the whole budget would evict everything and then
        // itself; keep it out rather than thrashing the cache empty for it.
        if bytes.len() > MAX_CACHE_BYTES {
            return;
        }
        if let Some(old) = self.cache.insert(ck, bytes.clone()) {
            self.bytes -= old.len();
            if let Some(i) = self.order.iter().position(|k| *k == ck) {
                self.order.remove(i);
            }
        }
        self.bytes += bytes.len();
        self.order.push(ck);
        while self.bytes > MAX_CACHE_BYTES {
            let Some(oldest) = (!self.order.is_empty()).then(|| self.order.remove(0)) else {
                break;
            };
            if let Some(dropped) = self.cache.remove(&oldest) {
                self.bytes -= dropped.len();
            }
        }
    }
}

/// The shared source every loader reaches bytes through.
fn shared() -> &'static TileSource {
    static SOURCE: OnceLock<TileSource> = OnceLock::new();
    SOURCE.get_or_init(TileSource::default)
}

/// Fetch a single tile's raw bytes: in-memory cache → in-flight coalesce →
/// on-disk blob cache → network (write-through).
///
/// **The** entry point for tile bytes. `tile_loader::load_tile_image` is a thin
/// wrapper over it, which is how `map_geometry`, `map_stream`, the globe
/// quadtree, `navigation::solve` and the router all inherit the cache without a
/// call-site change.
pub async fn tile_bytes(key: &TileKey, template: &str) -> Result<Arc<Vec<u8>>, String> {
    let kind = tile_source_kind(template);
    let ck = CacheKey { key: *key, kind };
    let (result, origin) = shared()
        .bytes(ck, || fetch_uncached(*key, kind, template.to_string()))
        .await;
    if origin != Origin::Fetched {
        tracing::trace!(tile = %key.to_path(), ?origin, "tile bytes served without a fetch");
    }
    result
}

/// The uncoalesced path: the on-disk blob cache first (persists across runs, so
/// revisits and LOD bursts don't re-hit the tile server), then the network, which
/// is written back through. **The only place in the client that fetches a tile
/// body** — everything else goes through [`tile_bytes`].
async fn fetch_uncached(
    key: TileKey,
    kind: TileSourceKind,
    template: String,
) -> Result<Vec<u8>, String> {
    let cache_key = tile_cache_key(&key, kind);
    if let Some(bytes) = storage::get(&cache_key).await {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }

    let url = format_osm_tile_url(&template, &key);
    let bytes = http::fetch(http::Request::get(url)).await?;
    // Best-effort write-through to the on-disk cache.
    storage::put(&cache_key, &bytes).await;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn key(x: i64) -> CacheKey {
        CacheKey {
            key: TileKey::new(x, 0, 14),
            kind: TileSourceKind::Vector,
        }
    }

    /// Drive a future to completion on this thread. The tile source's own
    /// awaits are the oneshot and the caller's fetch; nothing here needs a
    /// reactor, so `block_on` is the whole executor a test wants.
    fn run<T>(f: impl std::future::Future<Output = T>) -> T {
        futures_lite::future::block_on(f)
    }

    /// The point of the module: N callers for one key, one underlying fetch, and
    /// every caller gets the same bytes. The 179 overzoomed z15–z17 requests a
    /// fresh anchor makes all land on a handful of z14 bodies in the same frame,
    /// which is exactly this shape.
    #[test]
    fn concurrent_callers_share_one_fetch() {
        let source = TileSource::default();
        let fetches = AtomicUsize::new(0);
        let ck = key(1);

        // The leader parks *inside* its fetch until released, so the follower is
        // guaranteed to arrive while the flight is still open. Sequencing it any
        // other way would test the cache instead of the coalescing.
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let leader = source.bytes(ck, || async {
            fetches.fetch_add(1, Ordering::SeqCst);
            let _ = release_rx.await;
            Ok(b"body".to_vec())
        });
        let follower = source.bytes(ck, || async {
            fetches.fetch_add(1, Ordering::SeqCst);
            Ok(b"a different body".to_vec())
        });
        // A third task releases the leader once both have been polled and parked.
        let release = async {
            for _ in 0..8 {
                futures_lite::future::yield_now().await;
            }
            let _ = release_tx.send(());
        };

        let ((a, b), ()) = run(futures_lite::future::zip(
            futures_lite::future::zip(leader, follower),
            release,
        ));

        assert_eq!(
            fetches.load(Ordering::SeqCst),
            1,
            "the fetch was duplicated"
        );
        assert_eq!(a.1, Origin::Fetched);
        assert_eq!(b.1, Origin::Coalesced);
        assert_eq!(a.0.unwrap().as_slice(), b"body");
        assert_eq!(b.0.unwrap().as_slice(), b"body", "follower got other bytes");
    }

    /// A second call after the first has landed is served from memory and never
    /// reaches the fetcher at all — the disk read (or, on the web build, the
    /// network round trip) the duplicate used to pay.
    #[test]
    fn a_repeat_call_is_served_from_memory() {
        let source = TileSource::default();
        let fetches = AtomicUsize::new(0);
        let ck = key(2);
        let count = || {
            fetches.fetch_add(1, Ordering::SeqCst);
            async { Ok(b"body".to_vec()) }
        };

        assert_eq!(run(source.bytes(ck, count)).1, Origin::Fetched);
        assert_eq!(run(source.bytes(ck, count)).1, Origin::Memory);
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    /// A failed fetch must not be cached, or one 503 poisons the tile until the
    /// process restarts. The waiters still hear about it.
    #[test]
    fn a_failure_is_reported_and_not_cached() {
        let source = TileSource::default();
        let ck = key(3);
        let (r, _) = run(source.bytes(ck, || async { Err("503".to_string()) }));
        assert_eq!(r.unwrap_err(), "503");
        assert!(source.cached(&ck).is_none());
        // …and the key is no longer in flight, so the next caller retries.
        let (r, origin) = run(source.bytes(ck, || async { Ok(b"ok".to_vec()) }));
        assert_eq!(origin, Origin::Fetched);
        assert_eq!(r.unwrap().as_slice(), b"ok");
    }

    /// Byte-bounded, not entry-bounded, and least-recently-*used* is what goes.
    #[test]
    fn the_cache_evicts_by_bytes_in_lru_order() {
        let source = TileSource::default();
        // Four bodies of a third of the budget each: the fourth cannot fit
        // alongside the other three, so exactly one must be evicted.
        let third = MAX_CACHE_BYTES / 3;
        for i in 0..3u8 {
            let _ = run(source.bytes(key(10 + i64::from(i)), || async move { Ok(vec![i; third]) }));
        }
        assert!(source.cached_bytes() <= MAX_CACHE_BYTES);
        // Re-read the oldest so the *middle* one becomes least-recently-used.
        let (_, origin) = run(source.bytes(key(10), || async {
            Err("must not be fetched again".to_string())
        }));
        assert_eq!(origin, Origin::Memory);
        let _ = run(source.bytes(key(13), || async move { Ok(vec![3u8; third]) }));

        assert!(
            source.cached_bytes() <= MAX_CACHE_BYTES,
            "cache overshot its ceiling: {} > {MAX_CACHE_BYTES}",
            source.cached_bytes()
        );
        assert!(source.cached(&key(11)).is_none(), "LRU entry survived");
        assert!(
            source.cached(&key(10)).is_some(),
            "touched entry was evicted"
        );
        assert!(source.cached(&key(12)).is_some());
        assert!(source.cached(&key(13)).is_some());
    }

    /// The kind is part of the identity, for the same reason the on-disk cache
    /// path carries an extension: switching sources must not serve MVT bytes to
    /// the PNG decoder.
    #[test]
    fn cache_keys_separate_by_source_kind() {
        let source = TileSource::default();
        let k = TileKey::new(8490, 5319, 14);
        let vector = CacheKey {
            key: k,
            kind: TileSourceKind::Vector,
        };
        let raster = CacheKey {
            key: k,
            kind: TileSourceKind::Raster,
        };
        let _ = run(source.bytes(vector, || async { Ok(b"mvt".to_vec()) }));
        let (r, origin) = run(source.bytes(raster, || async { Ok(b"png".to_vec()) }));
        assert_eq!(origin, Origin::Fetched, "a kind switch reused cached bytes");
        assert_eq!(r.unwrap().as_slice(), b"png");
    }
}
