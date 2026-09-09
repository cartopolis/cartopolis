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
//! cartopy's `lod22.ts` now carries. Without a shared key, swapping a mass for a model
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

use std::collections::HashSet;

use crate::platform::cell_source::CellSource;
use crate::systems::coverage::CoverageSlot;

/// Fixed cache/streaming zoom — the same cells as every other building tier.
const LOD3_ZOOM: u32 = 14;

/// Which cells the host serves imported models for.
static COVERAGE: CoverageSlot = CoverageSlot::new("lod3", "/lod3/coverage.json", LOD3_ZOOM);

/// Keep the coverage manifest fetched. Called from `update_lod22`, which is the
/// only consumer and already has the clock.
pub(crate) fn prefetch_coverage(now: f64) {
    COVERAGE.ensure(now);
}

/// 0 while the coverage question is open, 1 once answered. `lod22` folds this
/// into its rebuild revision: a cell built before the manifest arrived drew the
/// shell of a building that has a model, and nothing would revisit it.
pub(crate) fn coverage_revision() -> u32 {
    COVERAGE.revision()
}

/// The credit line for the imported models, if any are served. Unlike the BAG and
/// the BGT this is not one national dataset with one licence — a model belongs to
/// whoever surveyed the building — so the extractor takes the line as an argument
/// and this passes it through.
pub fn lod3_attribution() -> Option<String> {
    COVERAGE.attribution()
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

fn cell_cache_key(cx: i64, cy: i64) -> String {
    format!("buildings/lod3/{LOD3_ZOOM}/{cx}/{cy}.json")
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
    gate: Some(|cx, cy| COVERAGE.covers(cx, cy)),
    key: cell_cache_key,
    url: Some(|cx, cy| {
        format!(
            "{}/lod3/{LOD3_ZOOM}/{cx}/{cy}.json",
            crate::systems::osm_buildings::tdbag_url()
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
