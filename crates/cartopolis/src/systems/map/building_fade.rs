//! **The city used to stop at a square line, and this is how it stops doing that.**
//!
//! Three tiers draw the buildings and the outermost one just ended. `lod22` draws
//! real LoD2.2 shells out to `lod22::lod22_ring()`, `osm_buildings` draws a
//! simplified extrusion from there to `far_cell_radius()` — about 5 km — and
//! beyond that the ground is the flat drawn map with the buildings *printed on
//! it*. Those two do not look alike, and the boundary between them was a cell
//! edge: a straight line across the view, a city on one side and a map on the
//! other. Photographed over Groningen at 600 m on 2026-09-21, it was the most
//! visible LOD discontinuity in the client.
//!
//! # Why height, and not transparency
//!
//! **The endpoint of a height ramp is already drawn.** A building taken down to
//! zero height *is* its own printed footprint, which is exactly what takes over
//! beyond the ring — so the outer ring does not fade into nothing, it fades into
//! the thing that replaces it. Nothing else available here has that property, and
//! it is what makes this the shape to reach for rather than a compromise.
//!
//! Transparency was never a candidate. `systems::grass` sets the rule for this
//! tree and both of its retires are height ramps for the same reason: an
//! alpha-blended building would leave the opaque pass, sort against the ground it
//! stands on and the sky behind it, and pay a per-pixel depth decision at the one
//! distance where there are thousands of them.
//!
//! # Chebyshev, because the thing being hidden is a square
//!
//! The ramp is measured in **cells, Chebyshev**, not in metres radially. The ring
//! it has to reach zero at is a square of cells, so a radial ramp would finish
//! early along the axes and leave the four corners standing at full height against
//! the ring boundary — the original hard line, kept in the places it reads worst.
//! Chebyshev distance is constant along that boundary, which is why
//! `map_stream::refresh_tile_lod_blend` measures the clipmap the same way.
//!
//! # A vertex has to know its own base
//!
//! Scaling a building's height is `base + (y - base) * f`, and `base` is the
//! ground under *that* building. The far tier computes it per building
//! (`MeshData::set_base`) and then folds it into the vertex Y, where it cannot be
//! recovered; [`ATTRIBUTE_BUILDING_BASE`] carries it through instead. It is opt-in
//! per buffer (`MeshData::with_bases`), because the detail patch and the LoD2.2
//! shells never fade and four bytes a vertex is ~2.9 MB of graphics memory on one
//! 720k-vertex cell, on the tier with the least of it.
//!
//! # This only works because there is no prepass, and that is not an accident
//!
//! A material that moves vertices in its main-pass vertex stage and leaves
//! `prepass_vertex_shader` at bevy's default has **two pipelines drawing one
//! building in two places**: depth written for the full height, geometry drawn at
//! the short one. Everything behind the building's missing upper half would be
//! depth-rejected against a silhouette that is no longer drawn.
//!
//! This client has no prepass — ambient occlusion was its only user and was
//! removed on 2026-09-21 (`systems::render_features`). **Anything that
//! reintroduces one owes this module a `prepass_vertex_shader` applying the
//! identical transform**, which means a copy of bevy's prepass vertex stage
//! carrying its `#ifdef`s for motion vectors, normals, deferred, skinning and
//! morph targets — a file that then has to be diffed against upstream on every
//! bevy bump, the way the vendored `big_space` already is. That is the price, and
//! it is why the guard in
//! [`BuildingFade::specialize`](cartopolis_materials::building_fade::BuildingFade)
//! is there to be found.

use bevy::prelude::*;

use crate::systems::ground_detail::GridCellOrigin;

// The far tier's material and its vertex attribute live in
// `cartopolis_materials`, so that Bevy's per-material plugin code is generated
// there rather than in this crate.
pub use cartopolis_materials::building_fade::{FarBuildingMaterial, ATTRIBUTE_BUILDING_BASE};

/// How many cells wide the ramp is, ending at the ring boundary.
///
/// One, so it occupies exactly the outermost ring of cells. A wider ramp would
/// start shortening buildings inside the ring the LoD2.2 shells hand over to, and
/// the two tiers overlap there — the far volume is built to sit *under* its
/// detailed twin (`detail_eaves`), so shrinking it would pull it out from under a
/// shell that is still drawn at full height.
const FADE_CELLS: f32 = 1.0;

/// Whether the ramp is on at all.
///
/// `CARTO_NO_BUILDING_FADE=1` strips it, on the same rule as
/// `CARTO_NO_GROUND_DETAIL` and `CARTO_NO_LOD_BLEND`: it changes what is drawn
/// where, and the only honest A/B of one is the same scene with the term removed.
/// `docs/notes/headless-shots-software-renderer.md` has why a *difference image*
/// of two runs is not that A/B here.
#[must_use]
pub fn enabled() -> bool {
    !matches!(
        std::env::var("CARTO_NO_BUILDING_FADE").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// The ring a far building is measured against, in render space.
///
/// Split out so the arithmetic can be tested without a camera: `centre` is the
/// camera's ground position in the anchored world frame, `origin` is where render
/// space currently sits in it, and `cell_m` is the cell edge.
#[must_use]
pub fn ring_uniform(centre: Vec2, origin: Vec2, cell_m: f32, radius: i64, on: bool) -> Vec4 {
    if !on || cell_m <= 0.0 {
        return Vec4::ZERO;
    }
    Vec4::new(
        centre.x - origin.x,
        centre.y - origin.y,
        1.0 / (cell_m * FADE_CELLS),
        // The outer edge of the outermost cell, which is where the ring — and with
        // it every far building — actually stops, expressed in ramp widths.
        (radius as f32 + 0.5) / FADE_CELLS,
    )
}

/// Keep the ramp centred on the camera.
///
/// Every frame, and unlike `ground_detail::refresh_ground_detail` that is
/// affordable for the reason `building_windows` gives: there are **five**
/// materials here — one for the walls and four window buckets — not one per cell.
/// A quantised centre would be cheaper and is wrong: the ramp is a continuous
/// function of distance, and stepping its centre every N metres would step every
/// building's height at once.
pub fn update_building_fade(
    mut materials: ResMut<Assets<FarBuildingMaterial>>,
    camera: crate::grid::AnchoredCamera,
    origin: Res<GridCellOrigin>,
    stream: Res<crate::systems::map_stream::MapStream>,
) {
    if materials.is_empty() {
        return;
    }
    let Some(centre) = camera.world_xz() else {
        return;
    };
    let Some((clat, _, _)) = stream.geo_anchor() else {
        return;
    };
    let cell_m =
        cartopolis_core::world::tile_to_meters(crate::systems::osm_buildings::CACHE_ZOOM, clat)
            as f32;
    let ring = ring_uniform(
        centre,
        origin.0,
        cell_m,
        crate::systems::osm_buildings::far_cell_radius(),
        enabled(),
    );
    let ids: Vec<AssetId<FarBuildingMaterial>> = materials.ids().collect();
    for id in ids {
        let Some(mut material) = materials.get_mut(id) else {
            continue;
        };
        // Compared before it is written: `get_mut` marks the asset changed, and an
        // unconditional write would re-prepare five bind groups every frame.
        if material.extension.ring != ring {
            material.extension.ring = ring;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A building at the camera keeps its height; one at the ring's outer edge has
    /// none left. The shader's ramp is `1 - smoothstep(w - FADE_CELLS, w, cells)`,
    /// so this checks the uniform those two ends are computed from.
    fn factor(ring: Vec4, at: Vec2) -> f32 {
        let d = (at - ring.xy()).abs() * ring.z;
        let reach = d.x.max(d.y);
        let t = (reach - (ring.w - 1.0)).clamp(0.0, 1.0);
        1.0 - (t * t * (3.0 - 2.0 * t))
    }

    #[test]
    fn a_building_is_full_height_at_the_camera_and_flat_at_the_rings_edge() {
        const CELL: f32 = 1471.0;
        let ring = ring_uniform(Vec2::ZERO, Vec2::ZERO, CELL, 3, true);
        assert!((factor(ring, Vec2::ZERO) - 1.0).abs() < 1e-6);
        // The ring's outer boundary: 3.5 cells out, on an axis and on the diagonal.
        let edge = 3.5 * CELL;
        assert!(factor(ring, Vec2::new(edge, 0.0)).abs() < 1e-6);
        assert!(
            factor(ring, Vec2::new(edge, edge)).abs() < 1e-6,
            "the corner of a square ring must reach zero too — a radial ramp is \
             what this is not"
        );
        // …and the corner is not reached *early*, which a radial ramp would do.
        assert!(
            factor(ring, Vec2::new(2.6 * CELL, 2.6 * CELL)) > 0.0,
            "the ramp is Chebyshev, so the diagonal is not further out than the axis"
        );
    }

    /// The ramp must not reach into the ring the LoD2.2 shells still draw at full
    /// height — the far volume sits *under* its detailed twin there.
    #[test]
    fn the_ramp_stays_out_of_the_shells_ring() {
        const CELL: f32 = 1471.0;
        let ring = ring_uniform(Vec2::ZERO, Vec2::ZERO, CELL, 3, true);
        // A cell at ring distance 2 is the outermost the shells cover.
        assert!((factor(ring, Vec2::new(2.5 * CELL, 0.0)) - 1.0).abs() < 1e-6);
    }

    /// Render space moves under the world frame; the centre has to move with it or
    /// the ramp sits a whole grid cell away from the camera.
    #[test]
    fn the_centre_is_in_render_space() {
        let origin = Vec2::new(10_000.0, -20_000.0);
        let ring = ring_uniform(Vec2::new(10_500.0, -19_500.0), origin, 1471.0, 3, true);
        assert!((ring.x - 500.0).abs() < 1e-3);
        assert!((ring.y - 500.0).abs() < 1e-3);
    }

    /// Off is off, at the uniform rather than in the shader.
    #[test]
    fn the_switch_zeroes_the_ring() {
        assert_eq!(
            ring_uniform(Vec2::ZERO, Vec2::ZERO, 1471.0, 3, false),
            Vec4::ZERO
        );
        // …and so does a degenerate cell size, which would otherwise divide by zero
        // and hand the shader an infinity.
        assert_eq!(
            ring_uniform(Vec2::ZERO, Vec2::ZERO, 0.0, 3, true),
            Vec4::ZERO
        );
    }
}
