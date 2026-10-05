//! The building designs atlas publishes (`cartopolis_geo::building_design`,
//! `docs/notes/building-designs.md`): which cells have any, fetching them, and
//! the by-building index the two consumers read.
//!
//! **Two consumers, and they want different things.** `lod22` bakes a building's
//! entrances into its collider and its facade, so it fetches the designs for its
//! cell *inside its mesh job* ([`load_around`]) and rebuilds a cell when the
//! manifest says a design around it changed ([`neighbourhood_revision`]).
//! `interiors` lays out the rooms near the avatar, so it reads the index
//! ([`get`]) and rebuilds when anything new has landed ([`revision`]).
//!
//! **Designs change while the app runs, which no other building tree does.** The
//! BAG and 3DBAG trees are rewritten by an extraction run, and a client that sees
//! an old cell is merely a day behind. A design is somebody's work, saved a
//! minute ago, and they are standing in the building to look at it. So the
//! manifest is asked again every [`POLL_SECS`], and each cell's stamp is part of
//! its cache key: a cached cell is good for ever *under its stamp*, and an edit is
//! a new stamp, a new key and one fetch.
//!
//! Everything here is a static rather than a resource because the readers include
//! a worker task (`lod22`'s job), which has no `World`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use bevy::tasks::IoTaskPool;
use cartopolis_geo::building_design::{BuildingDesign, DESIGN_ZOOM};

use crate::platform::{http, storage};
use crate::systems::osm_buildings::tdbag_url;

/// How often the manifest is asked again, seconds. Short enough that a person
/// who saves in the editor and walks into the building sees it; long enough to be
/// a rounding error against everything else this host serves.
const POLL_SECS: f64 = 30.0;

#[derive(Default)]
struct Store {
    /// cell → stamp, from the last manifest. Empty until one lands.
    manifest: HashMap<(i64, i64), u64>,
    /// The cells parsed, with the stamp each was parsed under.
    cells: HashMap<(i64, i64), (u64, Arc<Vec<Arc<BuildingDesign>>>)>,
    /// Every design in a parsed cell, by building.
    by_pand: HashMap<String, Arc<BuildingDesign>>,
}

static STORE: LazyLock<RwLock<Store>> = LazyLock::new(Default::default);
/// Bumped whenever the index changes — a cell parsed, a manifest that differs.
static REVISION: AtomicU64 = AtomicU64::new(0);
/// Bumped only when the manifest's content changes: what `lod22` compares.
static MANIFEST_REVISION: AtomicU64 = AtomicU64::new(0);
static POLLING: AtomicBool = AtomicBool::new(false);
static LAST_POLL: AtomicU64 = AtomicU64::new(0);
/// Cell fetches in flight, so two readers asking at once share one.
static IN_FLIGHT: LazyLock<Mutex<HashSet<((i64, i64), u64)>>> = LazyLock::new(Default::default);

fn read() -> std::sync::RwLockReadGuard<'static, Store> {
    STORE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write() -> std::sync::RwLockWriteGuard<'static, Store> {
    STORE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Keep the manifest fresh. Cheap; call it every frame from a consumer.
pub fn poll(now: f64) {
    let last = f64::from_bits(LAST_POLL.load(Ordering::Relaxed));
    if (last > 0.0 && now - last < POLL_SECS) || POLLING.swap(true, Ordering::Relaxed) {
        return;
    }
    LAST_POLL.store(now.to_bits(), Ordering::Relaxed);
    IoTaskPool::get()
        .spawn(async {
            if let Some(manifest) = fetch_manifest().await {
                set_manifest(manifest);
            }
            POLLING.store(false, Ordering::Relaxed);
        })
        .detach();
}

/// `None` is "could not ask"; an empty map is the host saying it has none.
async fn fetch_manifest() -> Option<HashMap<(i64, i64), u64>> {
    let base = tdbag_url();
    if base.is_empty() {
        return Some(HashMap::new());
    }
    // The stamp in the query is for the caches between here and atlas: the
    // manifest is the one file in this tree that must never be served stale.
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let url = format!("{base}/designs/coverage.json?t={}", now / POLL_SECS as u64);
    match http::fetch_optional(http::Request::get(url)).await {
        Ok(Some(bytes)) => Some(parse_manifest(&bytes)),
        Ok(None) => Some(HashMap::new()),
        Err(e) => {
            tracing::debug!("building designs manifest: {e}");
            None
        }
    }
}

fn parse_manifest(bytes: &[u8]) -> HashMap<(i64, i64), u64> {
    #[derive(serde::Deserialize)]
    struct Manifest {
        zoom: u32,
        cells: Vec<(i64, i64, u64)>,
    }
    match serde_json::from_slice::<Manifest>(bytes) {
        Ok(m) if m.zoom == DESIGN_ZOOM => {
            m.cells.into_iter().map(|(x, y, r)| ((x, y), r)).collect()
        }
        Ok(_) => HashMap::new(),
        Err(e) => {
            tracing::warn!("building designs manifest unreadable: {e}");
            HashMap::new()
        }
    }
}

fn set_manifest(manifest: HashMap<(i64, i64), u64>) {
    let mut store = write();
    if store.manifest == manifest {
        return;
    }
    // A cell that left the manifest, or whose stamp moved, takes its designs out
    // of the index now: a removed design must stop being drawn even if nobody
    // fetches the cell again.
    let stale: Vec<(i64, i64)> = store
        .cells
        .iter()
        .filter(|(c, (rev, _))| manifest.get(c) != Some(rev))
        .map(|(c, _)| *c)
        .collect();
    for c in stale {
        if let Some((_, designs)) = store.cells.remove(&c) {
            for d in designs.iter() {
                store.by_pand.remove(&d.pand);
            }
        }
    }
    tracing::info!(cells = manifest.len(), "building designs manifest");
    store.manifest = manifest;
    drop(store);
    MANIFEST_REVISION.fetch_add(1, Ordering::Relaxed);
    REVISION.fetch_add(1, Ordering::Relaxed);
}

/// Bumped whenever the set of designs this client holds changes.
#[must_use]
pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

/// Bumped whenever the published manifest changes.
#[must_use]
pub fn manifest_revision() -> u64 {
    MANIFEST_REVISION.load(Ordering::Relaxed)
}

/// A digest of the stamps of a cell and the eight around it: a building's design
/// is filed by its origin, which need not be in the cell `lod22` files the
/// building under, so a change in any of them can move a door in this one.
#[must_use]
pub fn neighbourhood_revision(cx: i64, cy: i64) -> u64 {
    let store = read();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for dx in -1..=1 {
        for dy in -1..=1 {
            let r = store
                .manifest
                .get(&(cx + dx, cy + dy))
                .copied()
                .unwrap_or(0);
            h = (h ^ r).wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// The stored design of a building, if one has been fetched.
#[must_use]
pub fn get(pand: &str) -> Option<Arc<BuildingDesign>> {
    read().by_pand.get(pand).cloned()
}

/// Fetch the designs of the cells around `(cx, cy)` that have any, and keep
/// them in the index. Every cell without designs costs nothing — the manifest
/// already said so.
pub async fn load_around(cx: i64, cy: i64) -> Vec<Arc<BuildingDesign>> {
    let wanted: Vec<((i64, i64), u64)> = {
        let store = read();
        (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dy| (cx + dx, cy + dy)))
            .filter_map(|c| store.manifest.get(&c).map(|&r| (c, r)))
            .collect()
    };
    let mut out = Vec::new();
    for (cell, rev) in wanted {
        out.extend(load_cell(cell, rev).await.iter().cloned());
    }
    out
}

/// Ask for the cells around a point without waiting — for `interiors`, which
/// runs on the main thread and reads the index on a later frame.
pub fn prefetch_around(cx: i64, cy: i64) {
    let wanted: Vec<((i64, i64), u64)> = {
        let store = read();
        (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dy| (cx + dx, cy + dy)))
            .filter_map(|c| {
                let r = *store.manifest.get(&c)?;
                (store.cells.get(&c).map(|e| e.0) != Some(r)).then_some((c, r))
            })
            .collect()
    };
    for (cell, rev) in wanted {
        if !IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((cell, rev))
        {
            continue;
        }
        IoTaskPool::get()
            .spawn(async move {
                load_cell(cell, rev).await;
                IN_FLIGHT
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&(cell, rev));
            })
            .detach();
    }
}

async fn load_cell(cell: (i64, i64), rev: u64) -> Arc<Vec<Arc<BuildingDesign>>> {
    if let Some((r, designs)) = read().cells.get(&cell) {
        if *r == rev {
            return designs.clone();
        }
    }
    let key = format!(
        "buildings/designs/v1/{DESIGN_ZOOM}/{}/{}.r{rev}.json",
        cell.0, cell.1
    );
    let text = match storage::get_string(storage::Namespace::Cache, &key).await {
        Some(t) => Some(t),
        None => {
            let url = format!(
                "{}/designs/{DESIGN_ZOOM}/{}/{}.json?r={rev}",
                tdbag_url(),
                cell.0,
                cell.1
            );
            match http::fetch_optional(http::Request::get(url)).await {
                Ok(Some(bytes)) => {
                    storage::put(storage::Namespace::Cache, &key, &bytes).await;
                    String::from_utf8(bytes).ok()
                }
                Ok(None) => None,
                Err(e) => {
                    tracing::debug!(?cell, "building designs: {e}");
                    return Arc::default();
                }
            }
        }
    };
    let designs: Arc<Vec<Arc<BuildingDesign>>> =
        Arc::new(text.map(|t| parse_cell(&t)).unwrap_or_default());
    let mut store = write();
    // Only if the manifest still says this stamp: a cell fetched under a stamp
    // that has since moved would put a superseded design back in the index.
    if store.manifest.get(&cell) == Some(&rev) {
        if let Some((_, old)) = store.cells.insert(cell, (rev, designs.clone())) {
            for d in old.iter() {
                store.by_pand.remove(&d.pand);
            }
        }
        for d in designs.iter() {
            store.by_pand.insert(d.pand.clone(), d.clone());
        }
        drop(store);
        REVISION.fetch_add(1, Ordering::Relaxed);
    }
    designs
}

/// Parse a served cell, dropping (and saying so) any design that does not
/// validate: one bad document must not take its neighbours with it.
fn parse_cell(text: &str) -> Vec<Arc<BuildingDesign>> {
    #[derive(serde::Deserialize)]
    struct Cell {
        designs: Vec<serde_json::Value>,
    }
    let Ok(cell) = serde_json::from_str::<Cell>(text) else {
        tracing::warn!("building designs cell unreadable");
        return Vec::new();
    };
    cell.designs
        .into_iter()
        .filter_map(|v| {
            let d: BuildingDesign = serde_json::from_value(v).ok()?;
            match d.validate() {
                Ok(()) => Some(Arc::new(d)),
                Err(e) => {
                    tracing::warn!(pand = %d.pand, "building design refused: {e}");
                    None
                }
            }
        })
        .collect()
}

/// `facts` with every designed building's entrances replaced by its design's —
/// so the facade door, the collider's opening and the room's doorway all move
/// together when an owner moves a door. The same `Arc` back when nothing in the
/// cell is designed, which is nearly always.
#[must_use]
pub fn with_design_doors(
    facts: Arc<crate::systems::building_facts::BagCell>,
    designs: &[Arc<BuildingDesign>],
) -> Arc<crate::systems::building_facts::BagCell> {
    if !designs
        .iter()
        .any(|d| facts.buildings.contains_key(d.pand.as_str()))
    {
        return facts;
    }
    let mut cell = (*facts).clone();
    for d in designs {
        if let Some(b) = cell.buildings.get_mut(d.pand.as_str()) {
            b.doors = d
                .entrances()
                .into_iter()
                .map(|p| cartopolis_geo::building_design::to_lat_lng(d.origin, p))
                .collect();
        }
    }
    Arc::new(cell)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_is_read_and_a_wrong_zoom_is_no_manifest() {
        let m = parse_manifest(br#"{"zoom":14,"cells":[[8490,5320,17],[8491,5320,3]]}"#);
        assert_eq!(m.get(&(8490, 5320)), Some(&17));
        assert_eq!(m.len(), 2);
        assert!(parse_manifest(br#"{"zoom":15,"cells":[[1,2,3]]}"#).is_empty());
        assert!(parse_manifest(b"not json").is_empty());
    }

    #[test]
    fn a_bad_design_does_not_take_its_neighbours_with_it() {
        let good = cartopolis_geo::building_design::generate(
            "0014100010938796",
            [53.2, 6.56],
            &[[0.0, 0.0], [6.0, 0.0], [6.0, 12.0], [0.0, 12.0]],
            None,
            &[[3.0, 0.0]],
        )
        .unwrap();
        let mut bad = good.clone();
        bad.pand = "0014100010938797".into();
        bad.storeys.clear();
        let text = serde_json::json!({ "designs": [good, bad] }).to_string();
        let parsed = parse_cell(&text);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].pand, "0014100010938796");
    }
}
