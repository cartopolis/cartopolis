//! Each building's roof colour as the aerial photograph measured it
//! (`roofs/14/{x}/{y}.json`, written by atlas's `pipeline/roofs.rs`).
//!
//! Like the BAG facts, this rides inside `lod22`'s mesh job rather than
//! arriving through a resource on its own schedule: the colour is baked into the
//! roof's vertices, so it has to be known on the worker that writes them. Where
//! the host serves no roof file the generated palette stands, exactly as before
//! — the layer is additive. `docs/notes/roof-colours-from-aerial-imagery.md`
//! says why it is a measured colour and never the photograph itself.

use cartopolis_geo::formats::roofs::{self, Cell as RoofCell, Surface};

use crate::platform::cell_source::CellSource;
use crate::systems::coverage::CoverageSlot;

const ROOFS_ZOOM: u32 = cartopolis_geo::formats::CACHE_ZOOM;

static COVERAGE: CoverageSlot = CoverageSlot::new("roofs", "/roofs/coverage.json", ROOFS_ZOOM);

/// Keep the coverage manifest fetched. Called from `update_lod22`, the only
/// consumer.
pub(crate) fn prefetch_coverage(now: f64) {
    COVERAGE.ensure(now);
}

/// 0 while the coverage question is open, 1 once answered — folded into
/// `lod22`'s rebuild revision, since a cell built before the answer drew its
/// roofs in the generated palette and nothing else would revisit it.
pub(crate) fn coverage_revision() -> u32 {
    COVERAGE.revision()
}

pub fn roofs_attribution() -> Option<String> {
    COVERAGE.attribution()
}

fn cell_cache_key(cx: i64, cy: i64) -> String {
    format!("buildings/roofs/{ROOFS_ZOOM}/{cx}/{cy}.json")
}

const ROOFS: CellSource<RoofCell> = CellSource {
    label: "roof colours",
    gate: Some(|cx, cy| COVERAGE.covers(cx, cy)),
    key: cell_cache_key,
    url: Some(|cx, cy| {
        format!(
            "{}/roofs/{ROOFS_ZOOM}/{cx}/{cy}.json",
            crate::systems::osm_buildings::tdbag_url()
        )
    }),
    breaker: None,
    timeout: None,
    parse: |text| roofs::parse(text).ok(),
};

/// One cell's roofs; empty for an uncovered or unreachable cell.
pub(crate) async fn load_roof_cell(cx: i64, cy: i64) -> RoofCell {
    ROOFS.load(cx, cy).await
}

/// A measured surface's colour as a linear vertex colour, keeping the
/// night-occupancy alpha the window shader reads.
#[must_use]
pub(crate) fn vertex_colour(s: &Surface, alpha: f32) -> [f32; 4] {
    linear(s.c, alpha)
}

/// An sRGB colour from the roofs file as a linear vertex colour with `alpha`.
#[must_use]
pub(crate) fn linear(c: [u8; 3], alpha: f32) -> [f32; 4] {
    let c = crate::systems::scatter::srgb_to_linear_f32(c.map(|v| f32::from(v) / 255.0));
    [c[0], c[1], c[2], alpha]
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_geo::formats::roofs::Material;

    #[test]
    fn a_measured_colour_is_linearised_and_keeps_its_alpha() {
        let s = Surface {
            c: [255, 128, 0],
            m: Material::Tile,
            n: 100,
        };
        let v = vertex_colour(&s, 0.4);
        assert!((v[0] - 1.0).abs() < 1e-4);
        assert!((v[1] - 0.2158).abs() < 1e-3, "{v:?}");
        assert_eq!((v[2], v[3]), (0.0, 0.4));
    }
}
