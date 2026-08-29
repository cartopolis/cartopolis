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

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;

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
}

impl Manifest {
    fn empty(zoom: u32) -> Self {
        Self {
            zoom,
            cells: HashSet::new(),
            attribution: String::new(),
        }
    }
}

/// A lazily-fetched coverage manifest for one data tree.
///
/// Declared as a `static` beside the streamer that uses it. Every method is safe
/// to call every frame from any system; the fetch happens at most once.
pub struct CoverageSlot {
    /// Path under the host root, e.g. `/bag/coverage.json`.
    sub: &'static str,
    /// The zoom the tree is binned at, and the value a manifest without one
    /// defaults to.
    zoom: u32,
    /// A short name for the log line, so two slots' messages are tellable apart.
    label: &'static str,
    answer: OnceLock<Manifest>,
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
            answer: OnceLock::new(),
            fetching: AtomicBool::new(false),
            last_attempt: AtomicU64::new(0),
            attempts: AtomicU8::new(0),
        }
    }

    /// Start the fetch if it has not been answered and nothing is in flight.
    /// Idempotent and cheap; call it from whatever system needs the answer.
    pub fn ensure(&'static self, now: f64) {
        if self.answer.get().is_some() || self.fetching.load(Ordering::Relaxed) {
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
                    Some(m) => {
                        let _ = self.answer.set(m);
                    }
                    None => {
                        let n = self.attempts.fetch_add(1, Ordering::Relaxed) + 1;
                        if n >= MAX_ATTEMPTS {
                            tracing::info!(
                                tree = self.label,
                                "no coverage on this host; the layer stays as it was"
                            );
                            let _ = self.answer.set(Manifest::empty(self.zoom));
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
        self.answer
            .get()
            .is_some_and(|m| m.zoom == self.zoom && m.cells.contains(&(cx, cy)))
    }

    /// 0 while the question is open, 1 once it has an answer of any kind.
    ///
    /// A streamer that bakes this tree's data into geometry reads it as a
    /// revision and rebuilds when it changes, the way the terrain field is
    /// handled — otherwise whatever was built during the gap keeps the
    /// no-coverage look for ever. It can only change once per process.
    pub fn revision(&self) -> u32 {
        u32::from(self.answer.get().is_some())
    }

    /// The tree's attribution line, once it has loaded and has any cells.
    pub fn attribution(&self) -> Option<String> {
        self.answer
            .get()
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
    let text = match http::fetch_string(http::Request::get(url.clone())).await {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!(%url, error = %e, "coverage unreachable");
            return None;
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
    let mut cells = HashSet::new();
    if let Some(arr) = json.get("cells").and_then(serde_json::Value::as_array) {
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
    tracing::info!(%url, cells = cells.len(), zoom, "coverage loaded");
    Some(Manifest {
        zoom,
        cells,
        attribution,
    })
}
