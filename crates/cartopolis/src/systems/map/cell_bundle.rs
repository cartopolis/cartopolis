//! One request for everything our own host holds about a cell.
//!
//! # The chain this exists to shorten
//!
//! `map_geometry::spawn_geometry_task` cannot mesh a cell until it has the
//! cell's tile *and* the datasets that are baked into the same meshes — the
//! BGT's surveyed street objects, the BGT's paving, the crop parcels. Each was
//! its own `GET`, awaited one after another, and everything the cell is made of
//! (ground fills, roads, water, bridges, trees, labels, footprints) sits behind
//! the last of them.
//!
//! Natively they really are sequential: `http::fetch` is a blocking `ureq` call
//! with no await point in it, so the `zip`s and `join_all`s further down that
//! function are presentation rather than overlap — `platform::http`'s module note
//! and `docs/notes/optional-layers-on-the-critical-path.md` both say so. Three
//! host round trips per cell, nine cells in the opening ring, and the task holds
//! one of at most eight `IoTaskPool` threads for the whole chain.
//!
//! # It is a cache prefill, not a new data path
//!
//! The obvious design — a bundle endpoint that every consumer reads instead of
//! its own file — would mean rewriting three loaders, three parsers and three
//! coverage gates, and would leave the client unable to talk to a host that has
//! not been redeployed. So the bundle writes each part into the storage cache
//! **under the key that part's own loader already looks for**, and then does
//! nothing else. The loaders are untouched; they simply hit the cache instead of
//! the network.
//!
//! Three consequences, all of them the point:
//!
//! * **A host that does not serve `/cell/…` costs one extra 404 per cell until
//!   [`SUPPORT`] latches**, after which this is a branch that returns
//!   immediately and the client behaves exactly as it did before.
//! * **A part the host omits is not cached at all** — deliberately, because an
//!   empty document written under a real key is never re-fetched, and "no trees
//!   in this cell" would then be permanent. The part's own loader asks for it as
//!   before.
//! * **Nothing downstream knows this happened.** The parts are stored verbatim,
//!   so an existing cache entry and a bundled one are the same bytes.
//!
//! # The document
//!
//! `GET {host}/cell/14/{x}/{y}.json`
//!
//! ```json
//! { "z": 14, "x": 8458, "y": 5422,
//!   "parts": { "bgt": { … }, "bgt.surfaces": { … }, "brp": { … } } }
//! ```
//!
//! Each part's value is **the same document that part's own endpoint serves**,
//! verbatim as JSON — `/bgt/14/x/y.json`, `/bgt/14/x/y.surfaces.json`,
//! `/brp/14/x/y.json`. A part the host has nothing for is simply left out. The
//! `z`/`x`/`y` fields are not read by this client; they are there so a stored or
//! logged document says which cell it is.
//!
//! # The part names are the client's, not the server's
//!
//! The document names its parts `bgt`, `bgt.surfaces`, `brp` — short stable
//! labels — and [`PARTS`] maps each to the cache key. The server therefore never
//! has to know how this client versions its cache keys, which is what lets
//! `surveyed`'s hand-bumped `v2` change without a redeploy.

use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use crate::platform::{http, storage};
use crate::systems::osm_buildings::tdbag_url;

/// The zoom the bundle is cut at — `map_geometry::GEOMETRY_ZOOM`, and the zoom
/// every part below is binned at.
const BUNDLE_ZOOM: u32 = 14;

/// Bumped when the *document* changes shape, which invalidates the markers and
/// so re-bundles every cell. The parts inside carry their own versions.
const MARKER_GENERATION: &str = "v1";

/// The parts a bundle may carry, and where each one belongs in the cache.
///
/// Only the three on `map_geometry`'s critical path. The LoD2.2 shells, the BAG
/// attributes and the LoD3 models are deliberately absent: each of those has a
/// streamer of its own with its own ring, its own concurrency bound and its own
/// retry policy, so none of them is holding up the ground. Adding them would
/// couple three independent schedules to one request for no latency anybody is
/// waiting on.
const PARTS: &[(&str, fn(i64, i64) -> String)] = &[
    ("bgt", crate::systems::surveyed::objects_cache_key),
    ("bgt.surfaces", crate::systems::surveyed::paving_cache_key),
    ("brp", crate::systems::crops::cache_key),
];

/// Whether this host serves bundles: unknown, yes, or no.
mod support {
    pub const UNKNOWN: u8 = 0;
    pub const YES: u8 = 1;
    pub const NO: u8 = 2;
}

static SUPPORT: AtomicU8 = AtomicU8::new(support::UNKNOWN);

/// Failed probes so far. The opening ring asks for nine cells at once, so the
/// answer cannot be latched off the first one without serialising them behind
/// it — the cheaper trade is to let the ring probe and latch once it is clear.
static PROBE_FAILURES: AtomicU32 = AtomicU32::new(0);

/// Failures before this host is written off as not serving bundles. One ring's
/// worth, so a single unlucky request does not cost the session the feature.
const MAX_PROBE_FAILURES: u32 = 9;

/// Fill this cell's cache entries from one request, if the host serves them.
///
/// Awaited by `map_geometry::spawn_geometry_task` **before** the per-part loads,
/// so those find what they need on disk. Cheap and safe to call for any cell: it
/// returns without a request when the host has no bundles, when this cell has
/// been bundled already, or when nothing here is configured.
pub(crate) async fn prefill(cx: i64, cy: i64) {
    if SUPPORT.load(Ordering::Relaxed) == support::NO {
        return;
    }
    let base = tdbag_url();
    if base.is_empty() {
        return;
    }
    // **Coverage first, before anything else.** Every part of a bundle is BGT or
    // BRP, so a cell the BGT manifest does not cover has nothing to bundle. Left
    // out, a session outside the coverage would ask once per cell and be told no
    // for ever — [`note_failure`] deliberately stops latching once a bundle has
    // been served, so one Dutch cell earlier in the session would keep the
    // feature switched on across the whole planet.
    if !crate::systems::surveyed::covers(cx, cy) {
        return;
    }
    // The marker is written only after a bundle has actually been stored, so its
    // presence means "these keys were filled from one" — not "we tried once".
    // One small storage read to save three network ones.
    let marker = marker_key(cx, cy);
    if storage::get_string(storage::Namespace::Cache, &marker)
        .await
        .is_some()
    {
        return;
    }

    let url = format!("{base}/cell/{BUNDLE_ZOOM}/{cx}/{cy}.json");
    // The same bound the other optional per-cell layers carry, and for the same
    // reason: this sits between the cell's tile and its tessellation worker, so a
    // slow answer reads as no ground rather than as no decoration. A failure
    // costs three seconds and then the per-part path runs as it always did.
    let request = http::Request::get(url).timeout(http::OPTIONAL_LAYER_TIMEOUT);
    let Ok(text) = http::fetch_string(request).await else {
        note_failure();
        return;
    };
    let Ok(document) = serde_json::from_str::<serde_json::Value>(&text) else {
        note_failure();
        return;
    };
    let stored = store_parts(cx, cy, &document).await;
    if stored == 0 {
        // A well-formed document with nothing in it is not a working bundle
        // endpoint; treat it as a failure so a host serving an empty stub does
        // not cost a request per cell for ever.
        note_failure();
        return;
    }
    SUPPORT.store(support::YES, Ordering::Relaxed);
    storage::put(storage::Namespace::Cache, &marker, b"1").await;
    tracing::debug!(cell = ?(cx, cy), parts = stored, "cell bundle prefilled the cache");
}

/// Write every part the document carries under its own loader's key. Returns how
/// many were written.
async fn store_parts(cx: i64, cy: i64, document: &serde_json::Value) -> usize {
    let Some(parts) = document.get("parts").and_then(serde_json::Value::as_object) else {
        return 0;
    };
    let mut stored = 0;
    for (name, key_of) in PARTS {
        let Some(part) = parts.get(*name) else {
            // Absent means the host has none for this cell. Nothing is written:
            // an empty document under a real key is never re-fetched, so it would
            // make "no data" permanent rather than merely current.
            continue;
        };
        // Re-serialised compactly rather than passed through as text: the part is
        // a JSON value inside the bundle, and the loaders parse JSON, so the
        // bytes need only be a document these parsers accept. `to_string` is
        // compact, which is also what the per-part endpoints serve.
        let body = part.to_string();
        storage::put(storage::Namespace::Cache, &key_of(cx, cy), body.as_bytes()).await;
        stored += 1;
    }
    stored
}

/// Count a failed probe, and latch the feature off once enough have failed.
fn note_failure() {
    if SUPPORT.load(Ordering::Relaxed) == support::YES {
        // A host that has served one bundle is not written off for a later
        // failure: that is an outage, and the per-part path already covers it.
        return;
    }
    let failures = PROBE_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
    if failures >= MAX_PROBE_FAILURES {
        SUPPORT.store(support::NO, Ordering::Relaxed);
        tracing::info!("this host serves no cell bundles; asking per part instead");
    }
}

fn marker_key(cx: i64, cy: i64) -> String {
    format!("cell-bundle/{MARKER_GENERATION}/{BUNDLE_ZOOM}/{cx}/{cy}")
}

#[cfg(test)]
mod tests {
    use super::{marker_key, PARTS};

    /// Every part must name a key that belongs to the loader it is standing in
    /// for. A typo here is invisible: the bundle would be fetched, the part
    /// written under a key nobody reads, and the per-part request made anyway —
    /// slower than not having the feature, with nothing to say so.
    #[test]
    fn every_part_writes_the_key_its_own_loader_reads() {
        let by_name: std::collections::HashMap<&str, String> =
            PARTS.iter().map(|(n, k)| (*n, k(8458, 5422))).collect();
        assert_eq!(
            by_name["bgt"],
            crate::systems::surveyed::objects_cache_key(8458, 5422)
        );
        assert_eq!(
            by_name["bgt.surfaces"],
            crate::systems::surveyed::paving_cache_key(8458, 5422)
        );
        assert_eq!(by_name["brp"], crate::systems::crops::cache_key(8458, 5422));
    }

    /// The two BGT parts differ — they are two files, and a key function copied
    /// and not edited would silently make the objects overwrite the paving.
    #[test]
    fn the_parts_do_not_share_a_key() {
        let keys: std::collections::HashSet<String> = PARTS.iter().map(|(_, k)| k(1, 2)).collect();
        assert_eq!(
            keys.len(),
            PARTS.len(),
            "two parts write the same cache key"
        );
    }

    #[test]
    fn the_marker_is_per_cell_and_versioned() {
        assert_eq!(marker_key(8458, 5422), "cell-bundle/v1/14/8458/5422");
        assert_ne!(marker_key(1, 2), marker_key(2, 1));
    }
}
