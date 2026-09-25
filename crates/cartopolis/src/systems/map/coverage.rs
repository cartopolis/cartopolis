//! "Which cells does our host actually serve?", once per additive data tree.
//!
//! Three trees hang off the same host and each answers the same question the same
//! way: fetch a `coverage.json`, remember the cell set, and until it lands report
//! no coverage so every consumer takes the path it took before the tree existed.
//! `osm_buildings` wrote that logic for the 3DBAG manifests and keeps its own copy
//! — it has extra behaviour bolted on (a Layers-panel toggle, a resolution gate
//! the building streamers block on) that the additive trees have no use for. This
//! is the plain version, shared by the BAG attributes and the BGT street objects.
//!
//! **The retry policy differs from `osm_buildings`' on purpose.** Those manifests
//! retry an unreachable host for ever, because they decide between real buildings
//! and guessed ones and a bad minute at startup must not cost the session. These
//! trees are *additive*: a host that has never run the extractor answers 404 for
//! ever, so retrying without end is a request every thirty seconds for the life of
//! the process to re-learn one permanent fact. A few attempts covers the transient
//! case, then it latches empty and says so once.
//!
//! **The ladder is for not being able to ask, never for having been told no.** A
//! 404 is the host answering, and it answers in about 70 ms; until 2026-09-21 that
//! answer went down the same ladder as a dead socket and was not believed for a
//! minute. What made that expensive rather than merely untidy is [`Self::revision`]:
//! `map_geometry` and `lod22` rebuild their whole ring when it moves, so a manifest
//! believed at +60 s re-tessellated nine surface cells and twenty-five building
//! cells a minute into the fill — measured, in `docs/notes/one-pass-cells.md`.
//! `http::fetch_optional` is what tells the two apart.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use bevy::tasks::IoTaskPool;

use crate::platform::http;
use crate::systems::osm_buildings::tdbag_url;

/// Attempts before an unreachable manifest latches as "no coverage here".
const MAX_ATTEMPTS: u8 = 3;
/// Spacing between those attempts.
const RETRY_SECS: f64 = 30.0;

/// One manifest's answer.
struct Manifest {
    zoom: u32,
    cells: HashSet<(i64, i64)>,
    attribution: String,
    /// The cells a tree has also *processed further*, and which run did it — only
    /// the terrain manifest has these: the tiles atlas has graded, and the stamp
    /// that goes into their URL and cache key (`terrain_tiles`).
    graded: HashSet<(i64, i64)>,
    graded_stamp: u64,
    /// When the tree was last extracted, for a consumer that keys its cache on
    /// the run (`dikes`). 0 for a manifest that does not say.
    stamp: u64,
}

impl Manifest {
    fn empty(zoom: u32) -> Self {
        Self {
            zoom,
            cells: HashSet::new(),
            attribution: String::new(),
            graded: HashSet::new(),
            graded_stamp: 0,
            stamp: 0,
        }
    }
}

/// Bumped by [`refresh_all`]: a slot whose answer is from an older epoch asks
/// again. See that function for why a manifest is ever asked twice.
static REFRESH_EPOCH: AtomicU32 = AtomicU32::new(0);

/// Every slot that has been asked anything, so [`refresh_all`] can reach the ones
/// whose consumer is not running this frame (a layer switched off still has a
/// manifest, and a stale one would be read the moment it is switched back on).
static SLOTS: Mutex<Vec<&'static CoverageSlot>> = Mutex::new(Vec::new());

/// The epoch a refresh is waiting for.
#[must_use]
pub fn refresh_epoch() -> u32 {
    REFRESH_EPOCH.load(Ordering::Relaxed)
}

/// Ask every manifest again, keeping the old answer until the new one lands.
///
/// **Once per process was a rule about the host, and the cache reset broke it.**
/// A manifest was latched for the life of the process on the grounds that the
/// answer never changes while the client runs. It does: atlas re-grades the
/// terrain under a new stamp, and a tree gains cells, while a phone runs for days
/// without being restarted. "Clear downloaded map data" deleted every file and
/// then rebuilt the map from manifests fetched at launch — graded ground asked for
/// under the old stamp, new cells not asked for at all — so the button did the
/// expensive half of a clean start and skipped the half that decides what to ask
/// for. `systems::cache_reset` calls this and waits on [`all_refreshed`] before it
/// rebuilds.
///
/// The old answer keeps serving meanwhile, and that is deliberate: `revision()`
/// never drops back to 0, so nothing retears to the no-coverage look for the
/// round trip and then again when the answer lands.
pub fn refresh_all(now: f64) {
    REFRESH_EPOCH.fetch_add(1, Ordering::Relaxed);
    for slot in registered() {
        slot.attempts.store(0, Ordering::Relaxed);
        slot.last_attempt.store(0, Ordering::Relaxed);
    }
    poke_all(now);
}

/// [`CoverageSlot::ensure`] every slot, for a caller waiting on
/// [`all_refreshed`]: a fetch already in flight when the refresh began answers
/// for the old epoch, and the slot's own consumer may not be asking this frame.
pub fn poke_all(now: f64) {
    for slot in registered() {
        slot.ensure(now);
    }
}

fn registered() -> Vec<&'static CoverageSlot> {
    SLOTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Whether every manifest has answered since the last [`refresh_all`] — or given
/// up trying, in which case it keeps the answer it had.
#[must_use]
pub fn all_refreshed() -> bool {
    let epoch = refresh_epoch();
    SLOTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .all(|s| s.epoch.load(Ordering::Relaxed) == epoch)
}

/// A lazily-fetched coverage manifest for one data tree.
///
/// Declared as a `static` beside the streamer that uses it. Every method is safe
/// to call every frame from any system; the fetch happens once, and again after
/// each [`refresh_all`].
pub struct CoverageSlot {
    /// Path under the host root, e.g. `/bag/coverage.json`.
    sub: &'static str,
    /// The zoom the tree is binned at, and the value a manifest without one
    /// defaults to.
    zoom: u32,
    /// A short name for the log line, so two slots' messages are tellable apart.
    label: &'static str,
    answer: RwLock<Option<Arc<Manifest>>>,
    /// The [`REFRESH_EPOCH`] the answer belongs to.
    epoch: AtomicU32,
    registered: AtomicBool,
    fetching: AtomicBool,
    /// Clock reading of the last attempt, as `f64` bits; 0 = never.
    last_attempt: AtomicU64,
    attempts: AtomicU8,
}

impl CoverageSlot {
    pub const fn new(label: &'static str, sub: &'static str, zoom: u32) -> Self {
        Self {
            sub,
            zoom,
            label,
            answer: RwLock::new(None),
            epoch: AtomicU32::new(0),
            registered: AtomicBool::new(false),
            fetching: AtomicBool::new(false),
            last_attempt: AtomicU64::new(0),
            attempts: AtomicU8::new(0),
        }
    }

    fn manifest(&self) -> Option<Arc<Manifest>> {
        self.answer
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set(&self, manifest: Manifest, epoch: u32) {
        *self
            .answer
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(manifest));
        self.epoch.store(epoch, Ordering::Relaxed);
    }

    /// Start the fetch if it has not been answered and nothing is in flight.
    /// Idempotent and cheap; call it from whatever system needs the answer.
    pub fn ensure(&'static self, now: f64) {
        if !self.registered.swap(true, Ordering::Relaxed) {
            SLOTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(self);
        }
        let epoch = refresh_epoch();
        let answered = self.answer.read().is_ok_and(|a| a.is_some());
        let current = self.epoch.load(Ordering::Relaxed) == epoch;
        if (answered && current) || self.fetching.load(Ordering::Relaxed) {
            return;
        }
        if self.attempts.load(Ordering::Relaxed) >= MAX_ATTEMPTS {
            return;
        }
        let last = f64::from_bits(self.last_attempt.load(Ordering::Relaxed));
        if last > 0.0 && now - last < RETRY_SECS {
            return;
        }
        self.fetching.store(true, Ordering::Relaxed);
        self.last_attempt.store(now.to_bits(), Ordering::Relaxed);
        IoTaskPool::get()
            .spawn(async move {
                match fetch(self.sub, self.zoom).await {
                    Some(m) => self.set(m, epoch),
                    None => {
                        let n = self.attempts.fetch_add(1, Ordering::Relaxed) + 1;
                        if n >= MAX_ATTEMPTS {
                            if self.manifest().is_some() {
                                // A refresh that could not reach the host keeps the
                                // answer it had, rather than forgetting a tree the
                                // host still serves.
                                self.epoch.store(epoch, Ordering::Relaxed);
                            } else {
                                tracing::info!(
                                    tree = self.label,
                                    "no coverage on this host; the layer stays as it was"
                                );
                                self.set(Manifest::empty(self.zoom), epoch);
                            }
                        }
                    }
                }
                self.fetching.store(false, Ordering::Relaxed);
            })
            .detach();
    }

    /// Whether the host serves this cell. `false` until the manifest lands, so a
    /// consumer degrades to "no data" rather than waiting for one.
    pub fn covers(&self, cx: i64, cy: i64) -> bool {
        self.manifest()
            .is_some_and(|m| m.zoom == self.zoom && m.cells.contains(&(cx, cy)))
    }

    /// The stamp of the run that graded this cell, if one did (only the terrain
    /// manifest carries any). A stamp of 0 is an old manifest's, and means none.
    pub fn graded(&self, cx: i64, cy: i64) -> Option<u64> {
        self.manifest()
            .filter(|m| m.zoom == self.zoom && m.graded_stamp > 0 && m.graded.contains(&(cx, cy)))
            .map(|m| m.graded_stamp)
    }

    /// The manifest's grading stamp, whatever the cell: 0 until it lands, and for a
    /// tree that was never graded.
    pub fn graded_stamp(&self) -> u64 {
        self.manifest().map_or(0, |m| m.graded_stamp)
    }

    /// The extraction run's stamp (`Manifest::stamp`): 0 until the manifest lands
    /// and for one that does not carry it.
    pub fn stamp(&self) -> u64 {
        self.manifest().map_or(0, |m| m.stamp)
    }

    /// 0 while the question is open, 1 once it has an answer of any kind.
    ///
    /// A streamer that bakes this tree's data into geometry reads it as a
    /// revision and rebuilds when it changes, the way the terrain field is
    /// handled — otherwise whatever was built during the gap keeps the
    /// no-coverage look for ever. It changes once per process: a [`refresh_all`]
    /// replaces the answer without passing back through 0.
    pub fn revision(&self) -> u32 {
        u32::from(self.manifest().is_some())
    }

    /// The tree's attribution line, once it has loaded and has any cells.
    pub fn attribution(&self) -> Option<String> {
        self.manifest()
            .filter(|m| !m.cells.is_empty() && !m.attribution.is_empty())
            .map(|m| m.attribution.clone())
    }
}

/// `None` means "could not ask" — a transient failure worth retrying. `Some` is
/// an answer, empty or not, and latches.
async fn fetch(sub: &str, zoom: u32) -> Option<Manifest> {
    let base = tdbag_url();
    if base.is_empty() {
        return Some(Manifest::empty(zoom)); // configured off: a definitive "no"
    }
    let url = format!("{base}{sub}");
    // **A 404 here is the answer, not a failure to get one**, and the difference was
    // worth 60 seconds of every cold start. The host answers "I do not serve crops"
    // in about 70 ms; treated as transient it cost `MAX_ATTEMPTS` tries at
    // `RETRY_SECS` apart, so the answer latched at ~+60 s — and since `revision()`
    // is what `map_geometry` and `lod22` rebuild on, *that* is when the whole ring
    // was torn up and re-tessellated, a minute into the fill. Measured cold over
    // Groningen on 2026-09-21: the crops manifest retore 9 surface cells at +67 s
    // and the LoD3 manifest 25 building cells at +70 s, both of them 404s.
    let bytes = match http::fetch_optional(http::Request::get(url.clone())).await {
        Ok(Some(b)) => b,
        Ok(None) => {
            // The host was reached and does not serve this tree. It will not start
            // while the client runs, so this latches now rather than after a ladder
            // of retries — and every consumer sees "no coverage" before it builds
            // anything, which is the whole point.
            tracing::info!(%url, "no coverage on this host; the layer stays as it was");
            return Some(Manifest::empty(zoom));
        }
        Err(e) => {
            tracing::debug!(%url, error = %e, "coverage unreachable");
            return None;
        }
    };
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => {
            // Reached, answered, unreadable — an answer, and the same one next time.
            tracing::warn!(%url, error = %e, "coverage is not UTF-8");
            return Some(Manifest::empty(zoom));
        }
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        // The host answered with something unreadable. Asking again would get the
        // same answer, so this one latches.
        tracing::warn!(%url, "coverage is not JSON");
        return Some(Manifest::empty(zoom));
    };
    let zoom = json
        .get("zoom")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(u64::from(zoom)) as u32;
    let attribution = json
        .get("attribution")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let cell_set = |key: &str| {
        let mut cells = HashSet::new();
        if let Some(arr) = json.get(key).and_then(serde_json::Value::as_array) {
            for cell in arr {
                if let Some(pair) = cell.as_array() {
                    if let (Some(x), Some(y)) = (
                        pair.first().and_then(serde_json::Value::as_i64),
                        pair.get(1).and_then(serde_json::Value::as_i64),
                    ) {
                        cells.insert((x, y));
                    }
                }
            }
        }
        cells
    };
    let cells = cell_set("cells");
    let graded = cell_set("graded");
    let graded_stamp = json
        .get("graded_stamp")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let stamp = json
        .get("stamp")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    tracing::info!(%url, cells = cells.len(), graded = graded.len(), zoom, "coverage loaded");
    Some(Manifest {
        zoom,
        cells,
        attribution,
        graded,
        graded_stamp,
        stamp,
    })
}
