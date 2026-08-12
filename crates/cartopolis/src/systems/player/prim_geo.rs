//! ADR-029: geo-anchored prim placement.
//!
//! A prim's authoritative horizontal placement is an absolute geographic
//! coordinate ([`PrimGeo`]), so it survives an OSM re-anchor. The render-space
//! position — a big_space `GridCell` + a cell-local `Transform` — is *derived*
//! here from the current anchor (`MapStream::geo_anchor()`) and re-derived
//! whenever that anchor moves. This is the prim-side mirror of how
//! `rendering::spawn_regions` rebases regions into the grid.

use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::resources::MapAnchor;
use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::{BigSpace, Grid, GridCell};

use crate::components::{Prim, PrimGeo};
use crate::systems::geo_nav::{lat_lng_to_world, world_to_lat_lng};
use cartopolis_core::GeoAnchor;

/// Reconstruct a prim's absolute world position from its `GridCell` + local
/// `Transform`. A missing cell means the prim still sits at the big_space root
/// (cell `0,0,0`), so the local translation *is* the world position.
fn world_position(cell: Option<&GridCell>, local: &Transform) -> DVec3 {
    let edge = WORLD_CELL_EDGE as f64;
    let base = cell.map_or(DVec3::ZERO, |c| {
        DVec3::new(c.x as f64 * edge, c.y as f64 * edge, c.z as f64 * edge)
    });
    base + local.translation.as_dvec3()
}

/// Convert a prim's world position (cell + local) to the anchor-independent
/// geo coordinate to persist. Used by the authoring send path so the server
/// stores lat/lng rather than a frame-relative offset. `None` if the world has
/// no anchor yet (nothing streamed).
pub fn geo_for(
    cell: Option<&GridCell>,
    local: &Transform,
    map_anchor: &MapAnchor,
) -> Option<GeoAnchor> {
    let anchor = map_anchor.geo?;
    let world = world_position(cell, local);
    let (latitude, longitude) = world_to_lat_lng(anchor, world.x, world.z);
    Some(GeoAnchor {
        latitude,
        longitude,
    })
}

/// Convert an absolute world XZ position (metres, anchored frame) to a geo
/// coordinate. For authoring a brand-new prim whose entity/`GridCell` doesn't
/// exist yet — the authored position is taken as world-space (near the origin).
pub fn geo_from_world(world: Vec3, map_anchor: &MapAnchor) -> Option<GeoAnchor> {
    let anchor = map_anchor.geo?;
    let (latitude, longitude) = world_to_lat_lng(anchor, world.x as f64, world.z as f64);
    Some(GeoAnchor {
        latitude,
        longitude,
    })
}

/// Place / re-project every geo-anchored prim into the big_space grid.
///
/// Runs each frame but only touches a prim when it needs it: a freshly spawned
/// geo prim (no `GridCell` yet) or — when the anchor moved — every geo prim. The
/// altitude (`position.y`) is preserved across the re-projection; only the
/// horizontal placement follows the anchor.
pub fn place_geo_prims(
    mut commands: Commands,
    bigspace: Query<Entity, With<BigSpace>>,
    mut prims: Query<(Entity, &PrimGeo, &mut Transform, Option<&GridCell>), With<Prim>>,
    mut last_anchor: Local<Option<(f64, f64, u32)>>,
    map_anchor: Res<MapAnchor>,
) {
    let Some(anchor) = map_anchor.geo else {
        return; // world not anchored yet
    };
    let Ok(root) = bigspace.single() else {
        return; // BigSpace not spawned yet (camera setup runs first)
    };

    let anchor_changed = *last_anchor != Some(anchor);
    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);

    for (entity, geo, mut transform, cell) in prims.iter_mut() {
        // Only work when there's work: a new prim (no cell) or an anchor move.
        if !anchor_changed && cell.is_some() {
            continue;
        }
        // Keep altitude; recompute the horizontal placement from the anchor.
        let world_y = world_position(cell, &transform).y;
        let (wx, wz) = lat_lng_to_world(anchor, geo.latitude, geo.longitude);
        let (new_cell, local) = grid.translation_to_grid(DVec3::new(wx, world_y, wz));
        transform.translation = local;
        commands.entity(entity).insert((new_cell, ChildOf(root)));
    }

    *last_anchor = Some(anchor);
}
