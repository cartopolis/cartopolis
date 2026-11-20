//! Each building's wall colour and material as a municipality's textured city
//! model shows them (`facades/14/{x}/{y}.json`, written by atlas's
//! `pipeline/facades.rs`; only Groningen publishes one).
//!
//! Rides inside `lod22`'s mesh job beside the roof colours and on the same
//! terms (`roof_colours`): the colour is baked into the wall's vertices and the
//! material into the facade code, so both have to be known on the worker that
//! writes them. Where the host serves no facade file the era palette stands,
//! exactly as before — the layer is additive.

use cartopolis_geo::formats::facades::{self, Cell as FacadeCell};

use crate::platform::cell_source::CellSource;
use crate::systems::coverage::CoverageSlot;

const FACADES_ZOOM: u32 = cartopolis_geo::formats::CACHE_ZOOM;

static COVERAGE: CoverageSlot =
    CoverageSlot::new("facades", "/facades/coverage.json", FACADES_ZOOM);

/// Keep the coverage manifest fetched. Called from `update_lod22`, the only
/// consumer.
pub(crate) fn prefetch_coverage(now: f64) {
    COVERAGE.ensure(now);
}

/// 0 while the coverage question is open, 1 once answered — folded into
/// `lod22`'s rebuild revision, since a cell built before the answer drew its
/// walls in the era palette and nothing else would revisit it.
pub(crate) fn coverage_revision() -> u32 {
    COVERAGE.revision()
}

pub fn facades_attribution() -> Option<String> {
    COVERAGE.attribution()
}

/// A cached cell is never re-fetched, so the version is how a rewritten file
/// reaches a device that already has the old one.
fn cell_cache_key(cx: i64, cy: i64) -> String {
    format!("buildings/facades/v1/{FACADES_ZOOM}/{cx}/{cy}.json")
}

const FACADES: CellSource<FacadeCell> = CellSource {
    label: "facade colours",
    gate: Some(|cx, cy| COVERAGE.covers(cx, cy)),
    key: cell_cache_key,
    url: Some(|cx, cy| {
        format!(
            "{}/facades/{FACADES_ZOOM}/{cx}/{cy}.json",
            crate::services::tdbag_url()
        )
    }),
    breaker: None,
    timeout: None,
    parse: |text| facades::parse(text).ok(),
};

/// One cell's facades, with the corrections around it laid over them; empty
/// for an uncovered or unreachable cell with none.
pub(crate) async fn load_facade_cell(cx: i64, cy: i64) -> FacadeCell {
    let mut cell = FACADES.load(cx, cy).await;
    cell.facades.extend(load_edits_around(cx, cy).await);
    cell
}

// ─── Corrections ─────────────────────────────────────────────────────────────
//
// A facade somebody said the city model got wrong, measured again from street
// photographs (the building card's facade buttons): atlas's `facade_edits/`
// tree, laid over the cell above a building at a time. Stamped and polled like
// the building designs (`building_designs`), because the person who asked is
// standing in front of the building waiting to see it change.

use crate::platform::sync::{LazyLock, RwLock};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// How often the corrections manifest is asked again, seconds.
const EDITS_POLL_SECS: f64 = 30.0;

static EDITS: LazyLock<RwLock<HashMap<(i64, i64), u64>>> = LazyLock::new(Default::default);
static EDITS_REVISION: AtomicU64 = AtomicU64::new(0);
static EDITS_POLLING: AtomicBool = AtomicBool::new(false);
static EDITS_LAST_POLL: AtomicU64 = AtomicU64::new(0);

/// Keep the corrections manifest fresh. Cheap; `lod22` calls it every frame.
pub(crate) fn poll_edits(now: f64) {
    let last = f64::from_bits(EDITS_LAST_POLL.load(Ordering::Relaxed));
    if (last > 0.0 && now - last < EDITS_POLL_SECS) || EDITS_POLLING.swap(true, Ordering::Relaxed) {
        return;
    }
    EDITS_LAST_POLL.store(now.to_bits(), Ordering::Relaxed);
    bevy::tasks::IoTaskPool::get()
        .spawn(async move {
            if let Some(manifest) = fetch_edits_manifest(now).await {
                let mut slot = EDITS
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *slot != manifest {
                    tracing::info!(cells = manifest.len(), "facade corrections manifest");
                    *slot = manifest;
                    EDITS_REVISION.fetch_add(1, Ordering::Relaxed);
                }
            }
            EDITS_POLLING.store(false, Ordering::Relaxed);
        })
        .detach();
}

/// Ask for the manifest at the next poll rather than up to half a minute on:
/// this device just sent a correction.
pub(crate) fn poll_edits_soon() {
    EDITS_LAST_POLL.store(0, Ordering::Relaxed);
}

async fn fetch_edits_manifest(now: f64) -> Option<HashMap<(i64, i64), u64>> {
    let base = crate::services::tdbag_url();
    if base.is_empty() {
        return Some(HashMap::new());
    }
    // The query is for the caches between here and atlas: this manifest must
    // never be served stale.
    let url = format!(
        "{base}/facade_edits/coverage.json?t={}",
        (now / EDITS_POLL_SECS) as u64
    );
    match crate::platform::http::fetch_optional(crate::platform::http::Request::get(url)).await {
        Ok(Some(bytes)) => Some(
            cartopolis_geo::formats::coverage::parse_slice(&bytes)
                .ok()
                .filter(|m| m.zoom == Some(FACADES_ZOOM))
                .map(|m| {
                    m.cells
                        .into_iter()
                        .map(|c| (c.xy(), c.stamp.unwrap_or(0)))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        Ok(None) => Some(HashMap::new()),
        Err(e) => {
            tracing::debug!("facade corrections manifest: {e}");
            None
        }
    }
}

/// Bumped whenever the corrections manifest changes: `lod22` then asks
/// [`edits_stamp`] which of its cells it touches.
pub(crate) fn edits_revision() -> u64 {
    EDITS_REVISION.load(Ordering::Relaxed)
}

/// Whether this host has published any facade correction.
pub(crate) fn edits_present() -> bool {
    !EDITS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_empty()
}

/// A digest of the correction stamps of a cell and the eight around it: a
/// building's correction is filed under its BAG cell, which need not be the
/// cell `lod22` draws it in.
pub(crate) fn edits_stamp(cx: i64, cy: i64) -> u64 {
    let edits = EDITS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut h: u64 = 0x8422_2325_cbf2_9ce4;
    for dx in -1..=1 {
        for dy in -1..=1 {
            let r = edits.get(&(cx + dx, cy + dy)).copied().unwrap_or(0);
            h = (h ^ r).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// The corrections in the cells around `(cx, cy)`, by building. A cell is
/// cached under its stamp, so a correction is one fetch and the rest none.
pub(crate) async fn load_edits_around(cx: i64, cy: i64) -> BTreeMap<String, facades::Facade> {
    use crate::platform::{http, storage};
    let wanted: Vec<((i64, i64), u64)> = {
        let edits = EDITS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dy| (cx + dx, cy + dy)))
            .filter_map(|c| edits.get(&c).map(|&r| (c, r)))
            .collect()
    };
    let mut out = BTreeMap::new();
    for ((x, y), rev) in wanted {
        let key = format!("buildings/facade_edits/v1/{FACADES_ZOOM}/{x}/{y}.r{rev}.json");
        let text = match storage::get_string(storage::Namespace::Cache, &key).await {
            Some(t) => Some(t),
            None => {
                let url = format!(
                    "{}/facade_edits/{FACADES_ZOOM}/{x}/{y}.json?r={rev}",
                    crate::services::tdbag_url()
                );
                match http::fetch_optional(http::Request::get(url)).await {
                    Ok(Some(bytes)) => {
                        storage::put(storage::Namespace::Cache, &key, &bytes).await;
                        String::from_utf8(bytes).ok()
                    }
                    Ok(None) => None,
                    Err(e) => {
                        tracing::debug!(cell = ?(x, y), "facade corrections: {e}");
                        None
                    }
                }
            }
        };
        if let Some(cell) = text.and_then(|t| facades::parse(&t).ok()) {
            out.extend(cell.facades);
        }
    }
    out
}
