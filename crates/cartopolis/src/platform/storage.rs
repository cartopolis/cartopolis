//! Keyed blob store (ADR-030/033 KvStore seam) — **all** of this client's
//! persistence, on every target.
//!
//! A get/put/remove over `/`-separated string keys, in two namespaces. Native
//! maps a key to a path under [`cache_root`](crate::utils::cache::cache_root) or
//! [`app_data_root`](crate::utils::cache::app_data_root) and uses `std::fs`; the
//! web build uses **IndexedDB**, one object store per namespace.
//!
//! Keys are relative paths, e.g. `"tiles/vector/14/8/5.mvt"`, `"user.json"` or
//! `"photos/place-3-1.thumb.jpg"` — the same layout the per-file cache paths used
//! to build by hand.
//!
//! ## The two namespaces are a durability statement, not a directory
//!
//! Natively the split is where the bytes land. In a browser it is what the user
//! keeps: storage is evictable, and a browser under pressure (or Safari after
//! seven idle days) discards an origin's data without asking. [`Namespace::Data`]
//! is the half where that would read as "my saved places are gone", so the first
//! write to it asks for `navigator.storage.persist()`. [`Namespace::Cache`] is
//! genuinely re-fetchable and asks for nothing.
//!
//! ## Async is the real API; the sync façade is a cache in front of it
//!
//! IndexedDB has **no synchronous read**, and two callers need one:
//! `UserStore::load` runs while the `App` is still being built, and
//! `photo_store::read_thumb` runs inside an egui draw. Neither can `await`.
//!
//! So on the web, [`preload`] pulls a prefix of the Data namespace into memory
//! *before the app is constructed* ([`crate::run`]), and [`get_now`] answers out
//! of that. Everything downstream — the store, the restore systems, their
//! ordering — is then identical to native, which is the whole point: the
//! alternative (start with defaults, fold the real values in a few hundred
//! milliseconds later) would have moved `restore_layer_prefs`,
//! `restore_start_location` and the session's startup decision onto a hydration
//! event, i.e. a behaviour change on the platform hardest to test.
//!
//! What cannot be preloaded — a full-size photo is megabytes and only wanted when
//! someone opens it — goes through [`request`], which answers `Loading` on the
//! first ask and `Ready` once the fetch lands. Natively that state never occurs,
//! and call sites still read the same on both.

/// Which half of the store a key lives in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Namespace {
    /// Re-fetchable from the network. Evictable, swept by size natively
    /// (`utils::cache::sweep_cache_root`) and by the browser on the web.
    Cache,
    /// The user's own: `user.json`, the photos on their saved places. Survives a
    /// cache clear natively, and is what `navigator.storage.persist()` is asked
    /// about on the web.
    Data,
}

/// What a synchronous ask found.
///
/// `Loading` exists only on the web and only for keys that were not preloaded:
/// it is the difference between "there is no photo" and "the read has not come
/// back yet", which a plain `Option` cannot express and which a caller that
/// caches its misses gets wrong in a way that never recovers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Cached {
    Ready(Vec<u8>),
    /// A fetch is in flight; ask again next frame.
    ///
    /// `allow`: constructed only by the web arm. Natively a read either finds the
    /// file or does not, in the same call — which is exactly why this variant has
    /// to exist for the shared call sites to be correct on both.
    #[allow(dead_code)]
    Loading,
    /// Looked, and there is nothing under that key.
    Missing,
}

impl Cached {
    /// The bytes, if they are in hand. `Loading` and `Missing` both answer `None`
    /// — for a caller that cannot wait either way, which is most of them.
    pub fn ready(self) -> Option<Vec<u8>> {
        match self {
            Self::Ready(bytes) => Some(bytes),
            Self::Loading | Self::Missing => None,
        }
    }
}

/// Read a blob, or `None` if absent (or unreadable).
pub async fn get(ns: Namespace, key: &str) -> Option<Vec<u8>> {
    let found = imp::get(ns, key).await;
    if ns == Namespace::Cache {
        stats::record(key, found.is_some());
    }
    found
}

/// Cache reads counted by the key's first segment (`tiles`, `rendered`,
/// `terrain`, …): hits and misses, cumulative for the process. What says
/// whether a warm start was actually warm, per kind of thing, without reading a
/// cache directory by hand.
pub mod stats {
    use crate::platform::sync::Mutex;
    use std::collections::BTreeMap;

    static READS: Mutex<BTreeMap<String, (u64, u64)>> = Mutex::new(BTreeMap::new());

    pub(super) fn record(key: &str, hit: bool) {
        let kind = key.split('/').find(|s| !s.is_empty()).unwrap_or("");
        let mut reads = match READS.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let e = reads.entry(kind.to_string()).or_default();
        if hit {
            e.0 += 1;
        } else {
            e.1 += 1;
        }
    }

    /// `(kind, hits, misses)` for every kind read so far.
    #[must_use]
    pub fn reads() -> Vec<(String, u64, u64)> {
        let reads = match READS.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        reads
            .iter()
            .map(|(k, (h, m))| (k.clone(), *h, *m))
            .collect()
    }
}

/// Read a blob and decode it as UTF-8.
pub async fn get_string(ns: Namespace, key: &str) -> Option<String> {
    get(ns, key).await.and_then(|b| String::from_utf8(b).ok())
}

/// Write a blob, creating parent directories as needed. Best-effort: a failed
/// write just means a future miss, never an error to the caller.
pub async fn put(ns: Namespace, key: &str, bytes: &[u8]) {
    imp::put(ns, key, bytes).await;
}

/// Remove an entry (e.g. to invalidate a corrupt one).
pub async fn remove(ns: Namespace, key: &str) {
    imp::remove(ns, key).await;
}

/// Seconds since the entry was last written, if it exists — for TTL freshness
/// checks (e.g. the transit line list).
pub async fn age_secs(ns: Namespace, key: &str) -> Option<f64> {
    imp::age_secs(ns, key).await
}

/// Read from a synchronous context — an egui pass, or `App` construction.
///
/// Natively this is the file read, and answers `Ready` or `Missing` in the same
/// call. On the web it answers out of the in-memory view, and a key that is in
/// neither the view nor in flight starts an IndexedDB read and answers `Loading`;
/// ask again next frame. A caller that cannot wait treats `Loading` as a miss
/// ([`Cached::ready`]) and gets the value on a later frame instead of never.
pub fn read_now(ns: Namespace, key: &str) -> Cached {
    let found = imp::read_now(ns, key);
    if ns == Namespace::Cache && !matches!(found, Cached::Loading) {
        stats::record(key, matches!(found, Cached::Ready(_)));
    }
    found
}

/// Write from a synchronous context.
///
/// Native writes the file before returning (atomically — see
/// [`crate::utils::cache::write_atomic`]), which is what lets the autosave's exit
/// flush mean something. The web updates the synchronous view immediately and
/// queues the IndexedDB write, because there is no way to block on one; a tab
/// closed in the same frame as the write can lose it, and no browser API would
/// have prevented that.
pub fn write_now(ns: Namespace, key: &str, bytes: &[u8]) -> Result<(), String> {
    imp::write_now(ns, key, bytes)
}

/// Remove an entry from a synchronous context, from both the in-memory view and
/// the store behind it.
pub fn remove_now(ns: Namespace, key: &str) {
    imp::remove_now(ns, key);
}

/// How a [`clear`] is getting on. Shared with whatever is doing the deleting.
///
/// It exists because neither platform can answer "cleared" in the call: natively
/// the work is a directory walk on another thread, and on the web it is a chain of
/// IndexedDB requests. A caller that wanted a return value would have to block an
/// egui frame on a filesystem.
#[derive(Default)]
pub struct ClearProgress {
    done: std::sync::atomic::AtomicBool,
    freed_bytes: std::sync::atomic::AtomicU64,
}

impl ClearProgress {
    /// Has the store finished emptying?
    pub fn finished(&self) -> bool {
        self.done.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Bytes reclaimed, once [`Self::finished`]. **Zero is not "nothing"** on the
    /// web: IndexedDB will not say how big a record was, and walking every value
    /// to add them up would read the whole cache into memory to throw it away. So
    /// the browser arm reports 0 and the UI has to phrase itself accordingly.
    pub fn freed_bytes(&self) -> u64 {
        self.freed_bytes.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Called by the worker when the store is empty. `Release`/`Acquire` in that
    /// order and not the other way round: a reader that sees `done` must also see
    /// the byte count that was written before it.
    pub fn finish(&self, freed_bytes: u64) {
        self.freed_bytes
            .store(freed_bytes, std::sync::atomic::Ordering::Release);
        self.done.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Throw away everything in a namespace.
///
/// Only ever called for [`Namespace::Cache`] today, and the signature takes the
/// namespace anyway because the alternative — a `clear_cache()` that closes over
/// which half it means — is how a future "delete my data" ends up as a second
/// function that does 90% of the same thing. It is safe on the cache for the
/// reason `utils::cache`'s sweep gives: an entry deleted under a running streamer
/// costs a re-fetch and nothing else. It would **not** be safe on
/// [`Namespace::Data`] without more thought, because `user.json` is written back
/// from memory on a timer and would simply reappear.
pub fn clear(ns: Namespace) -> std::sync::Arc<ClearProgress> {
    let progress = std::sync::Arc::new(ClearProgress::default());
    imp::clear(ns, std::sync::Arc::clone(&progress));
    progress
}

/// Pull every Data-namespace key that begins with `prefix` and ends with
/// `suffix` into the synchronous view, so [`get_now`] can answer for them.
///
/// Both ends matter: photos are `photos/<stem>.jpg` and `photos/<stem>.thumb.jpg`
/// under one prefix, and preloading the first of those would pull every full-size
/// photo the user has ever taken into memory at startup, to draw thumbnails.
///
/// A no-op natively (the filesystem is already synchronous). On the web this is
/// what has to finish before the `App` is built — see the module note.
#[cfg_attr(
    not(target_arch = "wasm32"),
    expect(dead_code, reason = "the web build's startup is its only caller")
)]
pub async fn preload(prefix: &str, suffix: &str) {
    imp::preload(prefix, suffix).await;
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::{Cached, Namespace};
    use std::path::PathBuf;

    fn root(ns: Namespace) -> PathBuf {
        match ns {
            Namespace::Cache => crate::utils::cache::cache_root(),
            Namespace::Data => crate::utils::cache::app_data_root(),
        }
    }

    /// Resolve a `/`-separated key to a path under its namespace's root. Splitting
    /// and joining per segment keeps it correct on every OS (rather than trusting
    /// `/` inside a single `join`).
    fn path(ns: Namespace, key: &str) -> PathBuf {
        key.split('/')
            .filter(|s| !s.is_empty())
            .fold(root(ns), |p, seg| p.join(seg))
    }

    pub async fn get(ns: Namespace, key: &str) -> Option<Vec<u8>> {
        std::fs::read(path(ns, key)).ok()
    }

    pub async fn put(ns: Namespace, key: &str, bytes: &[u8]) {
        let _ = write_now(ns, key, bytes);
    }

    pub async fn remove(ns: Namespace, key: &str) {
        remove_now(ns, key);
    }

    pub async fn age_secs(ns: Namespace, key: &str) -> Option<f64> {
        let modified = std::fs::metadata(path(ns, key)).ok()?.modified().ok()?;
        modified.elapsed().ok().map(|d| d.as_secs_f64())
    }

    pub fn read_now(ns: Namespace, key: &str) -> Cached {
        match std::fs::read(path(ns, key)) {
            Ok(bytes) => Cached::Ready(bytes),
            Err(_) => Cached::Missing,
        }
    }

    pub fn write_now(ns: Namespace, key: &str, bytes: &[u8]) -> Result<(), String> {
        let p = path(ns, key);
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
        crate::utils::cache::write_atomic(&p, bytes).map_err(|e| e.to_string())
    }

    pub fn remove_now(ns: Namespace, key: &str) {
        let _ = std::fs::remove_file(path(ns, key));
    }

    pub fn clear(ns: Namespace, progress: std::sync::Arc<super::ClearProgress>) {
        match ns {
            // Delegated rather than a `remove_dir_all` here: the sweep already
            // owns the tree walk, the detached thread and the byte count, and a
            // second implementation of "empty this directory" is a second place
            // for the Android root question to be answered differently.
            Namespace::Cache => crate::utils::cache::clear_cache_root(progress),
            // Deliberately unimplemented: see [`super::clear`]. Reporting it
            // finished with nothing freed is the honest answer — nothing was.
            Namespace::Data => progress.finish(0),
        }
    }

    pub async fn preload(_prefix: &str, _suffix: &str) {}
}

#[cfg(target_arch = "wasm32")]
mod imp {
    //! IndexedDB, plus the in-memory view [`super::get_now`] answers out of.
    //!
    //! One database, `cartopolis`, with an object store per namespace. A record is
    //! `{b: Uint8Array, t: epoch_ms}` — the timestamp is what `age_secs` reads,
    //! and it has to be stored because IndexedDB keeps no metadata of its own.

    use super::{Cached, Namespace};
    use crate::platform::sync::{Mutex, OnceLock};
    use js_sys::{Object, Reflect, Uint8Array};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use wasm_bindgen::prelude::*;
    use web_sys::{IdbDatabase, IdbOpenDbRequest, IdbRequest, IdbTransactionMode};

    const DB_NAME: &str = "cartopolis";
    const DB_VERSION: u32 = 1;
    const CACHE_STORE: &str = "cache";
    const DATA_STORE: &str = "data";
    const BYTES_FIELD: &str = "b";
    const TIME_FIELD: &str = "t";

    fn store_name(ns: Namespace) -> &'static str {
        match ns {
            Namespace::Cache => CACHE_STORE,
            Namespace::Data => DATA_STORE,
        }
    }

    /// The synchronous view. A `Mutex` rather than a `thread_local` because this
    /// build has real threads (`platform::workers` runs Web Workers) and `Vec<u8>`
    /// crosses them happily — unlike the database handle below, which is a
    /// `JsValue` and cannot.
    fn warm() -> &'static Mutex<HashMap<String, Cached>> {
        static WARM: OnceLock<Mutex<HashMap<String, Cached>>> = OnceLock::new();
        WARM.get_or_init(Default::default)
    }

    fn warm_key(ns: Namespace, key: &str) -> String {
        format!("{}/{key}", store_name(ns))
    }

    thread_local! {
        /// Opened once per thread that touches storage — in practice the main one.
        static DB: RefCell<Option<IdbDatabase>> = const { RefCell::new(None) };
        /// Whether the quota has already refused a write. IndexedDB answers a full
        /// quota with an exception per operation, and the tile streamers would
        /// otherwise take that cost on every tile for the rest of the session.
        static CACHE_FULL: RefCell<bool> = const { RefCell::new(false) };
    }

    /// Resolve when an `IDBRequest` settles. `Some` on success.
    ///
    /// A request is not a promise, so there is no `JsFuture` to await: the two
    /// event handlers are wired to a oneshot instead, and — this is the part that
    /// is easy to get wrong — the `Closure`s are held in scope *across* the await,
    /// because dropping one before its event fires unregisters it and the future
    /// never completes.
    async fn settle(request: &IdbRequest) -> Option<JsValue> {
        let (tx, rx) = futures_channel::oneshot::channel::<bool>();
        let tx = std::rc::Rc::new(RefCell::new(Some(tx)));
        let send =
            |value: bool,
             tx: std::rc::Rc<RefCell<Option<futures_channel::oneshot::Sender<bool>>>>| {
                Closure::<dyn FnMut()>::new(move || {
                    if let Some(tx) = tx.borrow_mut().take() {
                        let _ = tx.send(value);
                    }
                })
            };
        let on_success = send(true, tx.clone());
        let on_error = send(false, tx.clone());
        request.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));
        request.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let ok = rx.await.unwrap_or(false);
        request.set_onsuccess(None);
        request.set_onerror(None);
        drop(on_success);
        drop(on_error);
        ok.then(|| request.result().ok()).flatten()
    }

    async fn db() -> Option<IdbDatabase> {
        if let Some(open) = DB.with(|slot| slot.borrow().clone()) {
            return Some(open);
        }
        let factory = web_sys::window()?.indexed_db().ok()??;
        let open: IdbOpenDbRequest = factory.open_with_u32(DB_NAME, DB_VERSION).ok()?;
        // Both stores are created in the one upgrade, so a namespace added later
        // is a version bump rather than a migration.
        let upgrade = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            let Some(request) = event
                .target()
                .and_then(|t| t.dyn_into::<IdbOpenDbRequest>().ok())
            else {
                return;
            };
            let Some(db) = request
                .result()
                .ok()
                .and_then(|v| v.dyn_into::<IdbDatabase>().ok())
            else {
                return;
            };
            let existing = db.object_store_names();
            for name in [CACHE_STORE, DATA_STORE] {
                if existing.contains(name) {
                    continue;
                }
                if db.create_object_store(name).is_err() {
                    tracing::warn!(store = name, "could not create the object store");
                }
            }
        });
        open.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
        let result = settle(open.as_ref()).await;
        open.set_onupgradeneeded(None);
        drop(upgrade);

        let database: IdbDatabase = result?.dyn_into().ok()?;
        DB.with(|slot| *slot.borrow_mut() = Some(database.clone()));
        Some(database)
    }

    /// Ask the browser to stop treating this origin's data as disposable.
    ///
    /// Once per session, on the first Data write. Chrome decides silently from
    /// engagement; Firefox may prompt; Safari grants it on a user gesture. A
    /// refusal is not an error — it means the saved places are best-effort, which
    /// is the status quo and worth a line in the log rather than a dialog.
    async fn request_persistence() {
        let Some(manager) = web_sys::window().map(|w| w.navigator().storage()) else {
            return;
        };
        if let Ok(already) = manager.persisted() {
            if wasm_bindgen_futures::JsFuture::from(already)
                .await
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return;
            }
        }
        let Ok(promise) = manager.persist() else {
            return;
        };
        let granted = wasm_bindgen_futures::JsFuture::from(promise)
            .await
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        tracing::info!(granted, "asked the browser to keep this origin's data");
    }

    fn ensure_persistence(ns: Namespace) {
        if ns != Namespace::Data {
            return;
        }
        thread_local! {
            static ASKED: RefCell<bool> = const { RefCell::new(false) };
        }
        let first = ASKED.with(|asked| {
            let mut asked = asked.borrow_mut();
            let first = !*asked;
            *asked = true;
            first
        });
        if first {
            wasm_bindgen_futures::spawn_local(request_persistence());
        }
    }

    /// Read one record, as `(bytes, written_epoch_ms)`.
    async fn read_record(ns: Namespace, key: &str) -> Option<(Vec<u8>, f64)> {
        let db = db().await?;
        let name = store_name(ns);
        let tx = db
            .transaction_with_str_and_mode(name, IdbTransactionMode::Readonly)
            .ok()?;
        let request = tx
            .object_store(name)
            .ok()?
            .get(&JsValue::from_str(key))
            .ok()?;
        let value = settle(&request).await?;
        if value.is_undefined() || value.is_null() {
            return None;
        }
        let bytes = Reflect::get(&value, &JsValue::from_str(BYTES_FIELD))
            .ok()?
            .dyn_into::<Uint8Array>()
            .ok()?
            .to_vec();
        let written = Reflect::get(&value, &JsValue::from_str(TIME_FIELD))
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        Some((bytes, written))
    }

    async fn write_record(ns: Namespace, key: &str, bytes: &[u8]) -> bool {
        if ns == Namespace::Cache && CACHE_FULL.with(|full| *full.borrow()) {
            return false;
        }
        let Some(db) = db().await else { return false };
        let name = store_name(ns);
        let record = Object::new();
        // `Uint8Array::from` copies into the JS heap, which is what the structured
        // clone would do anyway — a view onto wasm memory cannot be stored.
        let payload = Uint8Array::from(bytes);
        let ok = Reflect::set(&record, &JsValue::from_str(BYTES_FIELD), &payload).is_ok()
            && Reflect::set(
                &record,
                &JsValue::from_str(TIME_FIELD),
                &JsValue::from_f64(js_sys::Date::now()),
            )
            .is_ok();
        if !ok {
            return false;
        }
        let Ok(tx) = db.transaction_with_str_and_mode(name, IdbTransactionMode::Readwrite) else {
            return false;
        };
        let stored = match tx
            .object_store(name)
            .and_then(|store| store.put_with_key(&record, &JsValue::from_str(key)))
        {
            Ok(request) => settle(&request).await.is_some(),
            // A synchronous throw here is the quota refusing the write outright.
            Err(_) => false,
        };
        if !stored && ns == Namespace::Cache {
            CACHE_FULL.with(|full| *full.borrow_mut() = true);
            tracing::warn!(
                "the browser refused a cache write (quota); not writing more this session"
            );
        }
        stored
    }

    pub async fn get(ns: Namespace, key: &str) -> Option<Vec<u8>> {
        if let Cached::Ready(bytes) = peek(ns, key) {
            return Some(bytes);
        }
        let bytes = read_record(ns, key).await.map(|(bytes, _)| bytes);
        remember(ns, key, bytes.clone());
        bytes
    }

    pub async fn put(ns: Namespace, key: &str, bytes: &[u8]) {
        ensure_persistence(ns);
        remember(ns, key, Some(bytes.to_vec()));
        write_record(ns, key, bytes).await;
    }

    pub async fn remove(ns: Namespace, key: &str) {
        remove_now(ns, key);
    }

    pub async fn age_secs(ns: Namespace, key: &str) -> Option<f64> {
        let (_, written) = read_record(ns, key).await?;
        Some(((js_sys::Date::now() - written) / 1000.0).max(0.0))
    }

    /// Record what a read found — including that it found nothing, which is what
    /// stops a missing key being re-fetched on every frame.
    fn remember(ns: Namespace, key: &str, bytes: Option<Vec<u8>>) {
        if let Ok(mut warm) = warm().lock() {
            warm.insert(
                warm_key(ns, key),
                match bytes {
                    Some(bytes) => Cached::Ready(bytes),
                    None => Cached::Missing,
                },
            );
        }
    }

    /// What the in-memory view already knows, without starting anything.
    fn peek(ns: Namespace, key: &str) -> Cached {
        warm()
            .lock()
            .ok()
            .and_then(|warm| warm.get(&warm_key(ns, key)).cloned())
            .unwrap_or(Cached::Missing)
    }

    pub fn read_now(ns: Namespace, key: &str) -> Cached {
        let known = warm()
            .lock()
            .ok()
            .and_then(|warm| warm.get(&warm_key(ns, key)).cloned());
        if let Some(state) = known {
            return state;
        }
        // Mark it in flight *before* spawning, so a caller asking again next frame
        // does not start a second read of the same key.
        if let Ok(mut warm) = warm().lock() {
            warm.insert(warm_key(ns, key), Cached::Loading);
        }
        let key = key.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let bytes = read_record(ns, &key).await.map(|(bytes, _)| bytes);
            remember(ns, &key, bytes);
        });
        Cached::Loading
    }

    pub fn write_now(ns: Namespace, key: &str, bytes: &[u8]) -> Result<(), String> {
        ensure_persistence(ns);
        // The synchronous view is updated first and unconditionally: a photo taken
        // this session has to be visible in the row on the next frame, whatever
        // IndexedDB is doing about it.
        remember(ns, key, Some(bytes.to_vec()));
        let (ns, key, bytes) = (ns, key.to_string(), bytes.to_vec());
        wasm_bindgen_futures::spawn_local(async move {
            write_record(ns, &key, &bytes).await;
        });
        Ok(())
    }

    pub fn remove_now(ns: Namespace, key: &str) {
        if let Ok(mut warm) = warm().lock() {
            warm.insert(warm_key(ns, key), Cached::Missing);
        }
        let (ns, key) = (ns, key.to_string());
        wasm_bindgen_futures::spawn_local(async move {
            let Some(db) = db().await else { return };
            let name = store_name(ns);
            let Ok(tx) = db.transaction_with_str_and_mode(name, IdbTransactionMode::Readwrite)
            else {
                return;
            };
            if let Ok(request) = tx
                .object_store(name)
                .and_then(|store| store.delete(&JsValue::from_str(&key)))
            {
                settle(&request).await;
            }
        });
    }

    pub fn clear(ns: Namespace, progress: std::sync::Arc<super::ClearProgress>) {
        // The synchronous view first and outside the task: a read on the very next
        // frame has to miss, or the streamers keep being handed the bytes we are
        // in the middle of deleting and nothing appears to have happened.
        //
        // Every key, not just the `Ready` ones — a remembered `Missing` is a
        // negative cache entry, and leaving those behind is how a "reset" comes to
        // mean "forget what you found and keep what you didn't".
        if let Ok(mut warm) = warm().lock() {
            let prefix = format!("{}/", store_name(ns));
            warm.retain(|key, _| !key.starts_with(&prefix));
        }
        // A refused write is a fact about a full quota, and the quota is what we
        // just emptied. Only this thread's flag: it is a `thread_local`, and the
        // worker threads' copies will clear themselves the next time a write to a
        // now-roomy store succeeds.
        if ns == Namespace::Cache {
            CACHE_FULL.with(|full| *full.borrow_mut() = false);
        }
        wasm_bindgen_futures::spawn_local(async move {
            let name = store_name(ns);
            let cleared = async {
                let db = db().await?;
                let tx = db
                    .transaction_with_str_and_mode(name, IdbTransactionMode::Readwrite)
                    .ok()?;
                // `clear()` on the store rather than a key walk: one request, and
                // it does not care how many records there are.
                let request = tx.object_store(name).and_then(|store| store.clear()).ok()?;
                settle(&request).await
            }
            .await;
            if cleared.is_none() {
                tracing::warn!("the browser refused to empty the {name} store");
            }
            // Finished either way. A reset that failed still has to stop saying
            // "clearing…", and the warning above is where the truth is.
            //
            // Zero bytes: IndexedDB does not report a record's size and adding
            // them up means reading the whole cache in to throw it away.
            progress.finish(0);
        });
    }

    pub async fn preload(prefix: &str, suffix: &str) {
        let Some(db) = db().await else { return };
        let Ok(tx) = db.transaction_with_str_and_mode(DATA_STORE, IdbTransactionMode::Readonly)
        else {
            return;
        };
        let Ok(store) = tx.object_store(DATA_STORE) else {
            return;
        };
        // Keys first, then one read each: `get_all` would pull every photo in the
        // namespace into memory to find the thumbnails among them.
        let Ok(request) = store.get_all_keys() else {
            return;
        };
        let Some(keys) = settle(&request).await else {
            return;
        };
        let keys = js_sys::Array::from(&keys);
        let mut loaded = 0usize;
        for key in keys.iter() {
            let Some(key) = key.as_string() else { continue };
            if !key.starts_with(prefix) || !key.ends_with(suffix) {
                continue;
            }
            let bytes = read_record(Namespace::Data, &key).await.map(|(b, _)| b);
            if bytes.is_some() {
                loaded += 1;
            }
            remember(Namespace::Data, &key, bytes);
        }
        tracing::info!(prefix, loaded, "preloaded stored data");
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// A key nothing else uses, removed by the test that made it.
    fn scratch(name: &str) -> String {
        format!("storage-selftest/{name}")
    }

    /// The round trip every caller depends on, through the real roots.
    #[test]
    fn a_written_key_reads_back_and_a_removed_one_does_not() {
        let key = scratch("round-trip");
        write_now(Namespace::Data, &key, b"hello").expect("write");
        assert_eq!(
            read_now(Namespace::Data, &key).ready().as_deref(),
            Some(&b"hello"[..])
        );

        remove_now(Namespace::Data, &key);
        assert_eq!(read_now(Namespace::Data, &key), Cached::Missing);
    }

    /// The namespaces are two places, not two prefixes in one.
    ///
    /// This is the property the whole durability story rests on: natively it is
    /// the cache root against the data root (so clearing tiles cannot take a
    /// user's photos), and on the web it is the object store that
    /// `navigator.storage.persist()` was asked about against the one that was not.
    /// A key written to one must be invisible in the other.
    #[test]
    fn the_two_namespaces_do_not_see_each_other() {
        let key = scratch("namespaces");
        write_now(Namespace::Cache, &key, b"cached").expect("write");
        assert_eq!(read_now(Namespace::Data, &key), Cached::Missing);

        write_now(Namespace::Data, &key, b"kept").expect("write");
        assert_eq!(
            read_now(Namespace::Cache, &key).ready().as_deref(),
            Some(&b"cached"[..])
        );
        assert_eq!(
            read_now(Namespace::Data, &key).ready().as_deref(),
            Some(&b"kept"[..])
        );

        remove_now(Namespace::Cache, &key);
        remove_now(Namespace::Data, &key);
    }

    /// `Loading` is not a value a caller can use, and must not read as data.
    #[test]
    fn loading_is_not_ready() {
        assert_eq!(Cached::Loading.ready(), None);
        assert_eq!(Cached::Missing.ready(), None);
        assert_eq!(Cached::Ready(vec![7]).ready(), Some(vec![7]));
    }
}
