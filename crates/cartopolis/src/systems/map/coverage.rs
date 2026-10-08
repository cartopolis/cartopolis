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

use crate::platform::{http, storage};
use crate::systems::osm_buildings::tdbag_url;

/// Attempts before an unreachable manifest latches as "no coverage here".
const MAX_ATTEMPTS: u8 = 3;
/// Spacing between those attempts.
const RETRY_SECS: f64 = 30.0;

/// One manifest's answer.
#[derive(PartialEq)]
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
    /// How many different answers this slot has held: what [`Self::revision`]
    /// reports. 0 until the first.
    generation: AtomicU32,
    /// Whether the copy kept on the device has been looked for yet.
    stored_read: AtomicBool,
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
            generation: AtomicU32::new(0),
            stored_read: AtomicBool::new(false),
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

    /// Hold `manifest` as the answer. The revision moves only when it differs
    /// from the one already held, so the network confirming the stored copy —
    /// the ordinary launch — rebuilds nothing.
    fn set(&self, manifest: Manifest, epoch: u32) {
        let mut answer = self
            .answer
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if answer.as_deref() != Some(&manifest) {
            *answer = Some(Arc::new(manifest));
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
        self.epoch.store(epoch, Ordering::Relaxed);
    }

    fn stored_key(&self) -> String {
        stored_key(self.sub)
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
                // **The copy kept from the last launch answers first.** Every
                // consumer holds its first cells for this answer (the basemap and
                // the ground cells for up to three seconds each), and a manifest
                // changes when atlas re-runs a city — days apart — so waiting for
                // the network every launch was a round trip, often several in a
                // row, spent re-learning yesterday's answer before anything could
                // be drawn. The network is still asked, right after; an answer that
                // differs replaces this one and moves the revision, which rebuilds
                // exactly what a late manifest always rebuilt.
                let key = self.stored_key();
                let mut stored = None;
                if !self.stored_read.swap(true, Ordering::Relaxed) && self.manifest().is_none() {
                    stored = storage::get_string(storage::Namespace::Cache, &key).await;
                    if let Some(text) = &stored {
                        let url = format!("{}{}", tdbag_url(), self.sub);
                        self.set(parse(&url, text, self.zoom), epoch);
                    }
                }
                match fetch(self.sub, self.zoom).await {
                    Some((m, text)) => {
                        if let Some(text) = text.filter(|t| stored.as_ref() != Some(t)) {
                            storage::put(storage::Namespace::Cache, &key, text.as_bytes()).await;
                        }
                        self.set(m, epoch);
                    }
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

    /// 0 while the question is open; otherwise how many different answers the
    /// slot has held, so it moves exactly when the answer does.
    ///
    /// A streamer that bakes this tree's data into geometry reads it as a
    /// revision and rebuilds when it changes, the way the terrain field is
    /// handled — otherwise whatever was built during the gap keeps the
    /// no-coverage look for ever. It used to change once per process; with the
    /// stored copy answering first it changes again if the network then says
    /// something different, and a [`refresh_all`] that finds a new answer moves
    /// it too. It never passes back through 0.
    /// The name it was made with — the dataset it covers. Only a test asks.
    #[cfg(test)]
    #[must_use]
    pub fn label(&self) -> &'static str {
        self.label
    }

    pub fn revision(&self) -> u32 {
        self.generation.load(Ordering::Relaxed)
    }

    /// The tree's attribution line, once it has loaded and has any cells.
    pub fn attribution(&self) -> Option<String> {
        self.manifest()
            .filter(|m| !m.cells.is_empty() && !m.attribution.is_empty())
            .map(|m| m.attribution.clone())
    }
}

/// Where the manifest at `sub` is kept on the device, per data host: a QA run
/// against a loopback host must not read the production host's answer. Shared
/// with `osm_buildings`' two 3DBAG manifests, which keep theirs the same way.
pub(crate) fn stored_key(sub: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tdbag_url().hash(&mut h);
    format!("coverage/v1/{:016x}{sub}", h.finish())
}

/// What a host that does not serve a tree is remembered as on the device: no
/// cells, which [`parse`] reads back as the empty answer the 404 gave.
const NOT_SERVED: &str = "{}";

/// `None` means "could not ask" — a transient failure worth retrying. `Some` is
/// an answer, empty or not, and latches; with it, the text worth keeping on the
/// device for the next launch (`None` for an answer that should not be kept).
async fn fetch(sub: &str, zoom: u32) -> Option<(Manifest, Option<String>)> {
    let base = tdbag_url();
    if base.is_empty() {
        // Configured off: a definitive "no", and nothing to remember.
        return Some((Manifest::empty(zoom), None));
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
            return Some((Manifest::empty(zoom), Some(NOT_SERVED.to_string())));
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
            return Some((Manifest::empty(zoom), None));
        }
    };
    let manifest = parse(&url, &text, zoom);
    tracing::info!(%url, cells = manifest.cells.len(), graded = manifest.graded.len(),
                   zoom = manifest.zoom, "coverage loaded");
    Some((manifest, Some(text)))
}

/// Read a manifest's text — from the host, or as kept on the device. Anything
/// that is not one reads as the empty answer, which latches: the host gave it,
/// and asking again would get the same.
fn parse(url: &str, text: &str, zoom: u32) -> Manifest {
    let json = match cartopolis_geo::formats::coverage::parse(text) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(%url, error = %e, "coverage is not a manifest");
            return Manifest::empty(zoom);
        }
    };
    Manifest {
        zoom: json.zoom.unwrap_or(zoom),
        cells: json.cell_set().collect(),
        attribution: json.attribution.unwrap_or_default(),
        graded: json
            .graded
            .unwrap_or_default()
            .iter()
            .map(cartopolis_geo::formats::coverage::CellRef::xy)
            .collect(),
        graded_stamp: json.graded_stamp.unwrap_or(0),
        stamp: json.stamp.unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{"zoom":14,"cells":[[8490,5319],[8491,5319]],"attribution":"x"}"#;

    #[test]
    fn the_same_answer_twice_is_one_revision() {
        static SLOT: CoverageSlot = CoverageSlot::new("test", "/test/coverage.json", 14);
        assert_eq!(SLOT.revision(), 0, "unanswered");
        // The copy kept from the last launch answers first…
        SLOT.set(parse("kept", MANIFEST, 14), 0);
        assert_eq!(SLOT.revision(), 1);
        assert!(SLOT.covers(8490, 5319));
        // …and the host saying the same thing rebuilds nothing.
        SLOT.set(parse("host", MANIFEST, 14), 0);
        assert_eq!(SLOT.revision(), 1, "a confirmed answer moved the revision");
        // A host that disagrees does move it, once.
        SLOT.set(parse("host", r#"{"cells":[[1,2]]}"#, 14), 0);
        assert_eq!(SLOT.revision(), 2);
        assert!(!SLOT.covers(8490, 5319) && SLOT.covers(1, 2));
    }

    #[test]
    fn a_kept_not_served_reads_back_as_the_404_answer() {
        assert!(parse("kept", NOT_SERVED, 14) == Manifest::empty(14));
    }
}
