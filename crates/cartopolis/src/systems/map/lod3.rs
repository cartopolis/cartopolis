//! The rung above the national shell: an imported **building model**.
//!
//! `lod22` streams what a survey produced — every roof plane, correct to a few
//! centimetres, and solid. A building has an inside, and a survey flown from an
//! aeroplane never will. Where somebody holds an IFC model of a particular
//! building, `tools/ifc_lod3.py` converts it into the *same cell format* and this
//! module streams it in place of that building's shell.
//!
//! **It is a thin module on purpose.** The format is the LoD2.2 one with two
//! fields added, so `lod22` parses it, meshes it and spawns it through the code
//! that already existed; what is here is the coverage manifest and the fetch. The
//! whole tier is worth having because of one field: the BAG identificatie that
//! atlas's `pipeline/lod22.rs` now carries. Without a shared key, swapping a mass for a model
//! is a geometry match against a building that has moved, been extended or been
//! re-surveyed since. With one, it is a set membership test.
//!
//! Two things follow from a model having an interior, and they are the reason this
//! is not simply written into the `lod22` tree:
//!
//! * **Its surfaces are drawn double-sided.** Backface culling is right for a
//!   shell — half the triangles face away and nothing can ever be behind them. It
//!   is wrong for a building you can be inside, where the wall in front of you is
//!   showing you its back. `lod22` keeps its culling; these pieces get their own
//!   material.
//! * **Collision comes from the walls, not from the footprint.** A shell's
//!   collider is the convex hull of the whole building, which is what keeps the
//!   avatar outside it. Hulling a model the same way would seal it — including
//!   sealing the doorway the model actually has. So each wall-like element gets
//!   its own hull and everything else gets none, which leaves the openings open.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{LazyLock, RwLock};

use bevy::tasks::IoTaskPool;

use crate::platform::cell_source::CellSource;
use crate::platform::http;
use crate::systems::osm_buildings::tdbag_url;

/// Fixed cache/streaming zoom — the same cells as every other building tier.
const LOD3_ZOOM: u32 = 14;

/// How often the manifest is asked again, seconds — the designs' rate, for the
/// designs' reason: an override is an operator's edit, made a minute ago, and
/// they are looking at the building to see it (`docs/notes/building-models.md`).
const POLL_SECS: f64 = 30.0;

/// The published manifest (`lod3/coverage.json`): which cells hold models, each
/// with the stamp it was last written under, and which buildings get no
/// interior at all.
#[derive(Default, PartialEq)]
struct Manifest {
    cells: HashMap<(i64, i64), u64>,
    no_interior: HashSet<String>,
    attribution: Option<String>,
}

static MANIFEST: LazyLock<RwLock<Manifest>> = LazyLock::new(Default::default);
/// 0 until the first manifest (or the host saying it has none), 1 after.
static ANSWERED: AtomicU32 = AtomicU32::new(0);
/// Bumped whenever the manifest's content changes.
static MANIFEST_REVISION: AtomicU64 = AtomicU64::new(0);
static POLLING: AtomicBool = AtomicBool::new(false);
static LAST_POLL: AtomicU64 = AtomicU64::new(0);

fn manifest() -> std::sync::RwLockReadGuard<'static, Manifest> {
    MANIFEST
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Keep the manifest fresh. Called from `update_lod22`, which is the only
/// consumer that has the clock; cheap between polls.
pub(crate) fn prefetch_coverage(now: f64) {
    let last = f64::from_bits(LAST_POLL.load(Ordering::Relaxed));
    if (last > 0.0 && now - last < POLL_SECS) || POLLING.swap(true, Ordering::Relaxed) {
        return;
    }
    LAST_POLL.store(now.to_bits(), Ordering::Relaxed);
    IoTaskPool::get()
        .spawn(async {
            if let Some(m) = fetch_manifest().await {
                let mut slot = MANIFEST
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *slot != m {
                    tracing::info!(cells = m.cells.len(), "building models manifest");
                    *slot = m;
                    MANIFEST_REVISION.fetch_add(1, Ordering::Relaxed);
                }
                ANSWERED.store(1, Ordering::Relaxed);
            }
            POLLING.store(false, Ordering::Relaxed);
        })
        .detach();
}

/// `None` is "could not ask"; an empty manifest is the host saying it has none.
async fn fetch_manifest() -> Option<Manifest> {
    let base = tdbag_url();
    if base.is_empty() {
        return Some(Manifest::default());
    }
    // Bucketed by the poll period, for the caches between here and atlas: the
    // manifest is the one file in this tree that must not be served stale.
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let url = format!("{base}/lod3/coverage.json?t={}", now / POLL_SECS as u64);
    match http::fetch_optional(http::Request::get(url)).await {
        Ok(Some(bytes)) => Some(parse_manifest(&bytes)),
        Ok(None) => Some(Manifest::default()),
        Err(e) => {
            tracing::debug!("building models manifest: {e}");
            None
        }
    }
}

/// Read a manifest. A cell is `[x, y, stamp]`, or `[x, y]` from a host older
/// than stamps, which reads as stamp 0 — cached for ever, as it always was.
fn parse_manifest(bytes: &[u8]) -> Manifest {
    match cartopolis_geo::formats::coverage::parse_slice(bytes) {
        Ok(m) if m.zoom == Some(LOD3_ZOOM) => Manifest {
            cells: m
                .cells
                .iter()
                .map(|c| (c.xy(), c.stamp.unwrap_or(0)))
                .collect(),
            // The overrides' own key (`atlas::overrides::manifest_extra`).
            no_interior: m
                .extra
                .get("no_interior")
                .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                .unwrap_or_default()
                .into_iter()
                .collect(),
            attribution: m.attribution,
        },
        Ok(_) => Manifest::default(),
        Err(e) => {
            tracing::warn!("building models manifest unreadable: {e}");
            Manifest::default()
        }
    }
}

/// 0 while the manifest question is open, 1 once answered. `lod22` folds this
/// into its rebuild revision: a cell built before the manifest arrived drew the
/// shell of a building that has a model, and nothing would revisit it. After
/// that a change rebuilds only the cell it is in ([`cell_stamp`]).
pub(crate) fn coverage_revision() -> u32 {
    ANSWERED.load(Ordering::Relaxed)
}

/// Bumped whenever the published manifest changes: what `lod22` compares before
/// asking which of its cells moved.
pub(crate) fn manifest_revision() -> u64 {
    MANIFEST_REVISION.load(Ordering::Relaxed)
}

/// The stamp a cell's models were last published under; 0 for a cell with none.
/// An override saved or deleted is a new stamp on its cell and nowhere else.
pub(crate) fn cell_stamp(cx: i64, cy: i64) -> u64 {
    manifest().cells.get(&(cx, cy)).copied().unwrap_or(0)
}

/// Whether this building's override says it has no inside (`interior: none`) —
/// a stadium has a pitch where generated rooms would go.
pub(crate) fn no_interior(pand: &str) -> bool {
    manifest().no_interior.contains(pand)
}

/// The credit line for the models, if any are served. Unlike the BAG and the BGT
/// this is not one national dataset with one licence — a model belongs to
/// whoever made it — so atlas writes the line and this passes it through.
pub fn lod3_attribution() -> Option<String> {
    manifest().attribution.clone()
}

/// Element classes whose footprint should stop an avatar.
///
/// Walls and columns, and nothing else. A floor slab's footprint is the whole
/// storey, so hulling one would fill the building with an invisible block — the
/// exact failure the shell's own collider is *supposed* to have and a model is
/// supposed to lose. Doors and windows are openings and must never be solid;
/// stairs and railings are things to walk on and past.
pub(crate) fn is_solid_class(class: &str) -> bool {
    matches!(
        class,
        "IfcWall" | "IfcWallStandardCase" | "IfcCurtainWall" | "IfcColumn"
    )
}

/// The stamp is in the key, as the designs' is: a cached cell is good for ever
/// under its stamp, and an edit is a new stamp, a new key and one fetch.
fn cell_cache_key(cx: i64, cy: i64) -> String {
    format!(
        "buildings/lod3/v2/{LOD3_ZOOM}/{cx}/{cy}.r{}.json",
        cell_stamp(cx, cy)
    )
}

/// One cell's imported models, as text, plus nothing else — the parse belongs to
/// `lod22`, which owns the format.
///
/// `None` for an uncovered or unreachable cell, which every caller reads as "this
/// cell has no models" and falls back to the shells it always drew.
/// The cell's text, not a parsed cell: `lod22` owns the parser, because the
/// format is the LoD2.2 one with two fields added and re-implementing it here is
/// how the two would come to disagree.
const MODELS: CellSource<String> = CellSource {
    label: "lod3 models",
    gate: Some(|cx, cy| manifest().cells.contains_key(&(cx, cy))),
    key: cell_cache_key,
    url: Some(|cx, cy| {
        format!(
            "{}/lod3/{LOD3_ZOOM}/{cx}/{cy}.json?r={}",
            tdbag_url(),
            cell_stamp(cx, cy)
        )
    }),
    breaker: None,
    timeout: None,
    parse: |text| Some(text.to_string()),
};

pub(crate) async fn load_lod3_text(cx: i64, cy: i64) -> Option<String> {
    MODELS.load_text(cx, cy).await
}

/// The buildings an imported cell replaces, so the shells can be skipped without
/// inspecting every element.
pub(crate) fn replaced_pands(pands: &[String]) -> HashSet<&str> {
    pands.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_carries_stamps_and_reads_an_old_one_as_stamp_zero() {
        let m = parse_manifest(
            br#"{"zoom":14,"cells":[[8492,5321,7],[1,2]],
                 "no_interior":["0014100010953806"],"attribution":"x"}"#,
        );
        assert_eq!(m.cells.get(&(8492, 5321)), Some(&7));
        assert_eq!(m.cells.get(&(1, 2)), Some(&0));
        assert!(m.no_interior.contains("0014100010953806"));
        assert_eq!(m.attribution.as_deref(), Some("x"));
        // Another zoom is no manifest rather than the wrong cells.
        assert!(parse_manifest(br#"{"zoom":13,"cells":[[1,2,3]]}"#)
            .cells
            .is_empty());
    }

    #[test]
    fn only_walls_and_columns_are_solid() {
        assert!(is_solid_class("IfcWall"));
        assert!(is_solid_class("IfcWallStandardCase"));
        assert!(is_solid_class("IfcColumn"));
        // The one that matters: a slab's footprint is the whole floor.
        assert!(!is_solid_class("IfcSlab"));
        assert!(!is_solid_class("IfcRoof"));
        // …and the openings, which must stay walkable.
        assert!(!is_solid_class("IfcDoor"));
        assert!(!is_solid_class("IfcWindow"));
        // A shell element carries no class at all.
        assert!(!is_solid_class(""));
    }
}
