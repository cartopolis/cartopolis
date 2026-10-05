//! ADR-029: geo-anchored prim placement.
//!
//! A prim's authoritative horizontal placement is an absolute geographic
//! coordinate ([`PrimGeo`]), so it survives an OSM re-anchor. The render-space
//! position — a big_space `GridCell` + a cell-local `Transform` — is *derived*
//! here from the current anchor (`MapStream::geo_anchor()`) and re-derived
//! whenever that anchor moves.

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
pub(crate) fn world_position(cell: Option<&GridCell>, local: &Transform) -> DVec3 {
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
/// geo prim (no `GridCell` yet), one whose [`PrimGeo`] changed, or — when the
/// anchor moved — every geo prim. The altitude (`position.y`) is preserved
/// across the re-projection; only the horizontal placement follows the anchor.
///
/// **The `Changed` arm is what lets a prim move at all.** Before it, a prim was
/// placed once and then only re-placed on a re-anchor, which was right while the
/// only way a prim's geo changed was somebody dragging it (and the drag moves the
/// `Transform` directly). A script writing `set_geo` twenty times a second
/// updates `PrimGeo` and nothing else: the prim would have sat exactly where it
/// was first drawn, while the server, the database and every reconnecting client
/// agreed it had moved.
pub fn place_geo_prims(
    mut commands: Commands,
    bigspace: Query<Entity, With<BigSpace>>,
    // A physical prim in motion is placed by `smooth_physical_prims` instead,
    // and two systems writing one `GridCell` in a frame is a race on whose
    // command lands last.
    mut prims: Query<
        (Entity, Ref<PrimGeo>, &mut Transform, Option<&GridCell>),
        (With<Prim>, Without<crate::components::PhysicalMotion>),
    >,
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
        // Only work when there's work: a new prim (no cell), a prim whose anchor
        // was rewritten, or a move of the map anchor everything is projected
        // against.
        if !anchor_changed && cell.is_some() && !geo.is_changed() {
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

/// How far past the last pose a physical prim is carried along its velocity
/// before it waits for the next one — four ticks. Further than that and a lost
/// packet draws a ball sailing on through the goal it actually hit.
const MOTION_EXTRAPOLATE_S: f64 = 0.2;
/// How quickly what is drawn closes on where the body should be, per second.
/// High, because the target is already extrapolated: this only hides the step
/// a correction makes, it is not what makes the motion smooth.
const MOTION_EASE_RATE: f32 = 18.0;
/// A correction this large is a teleport (a `set_geo`, an edit) and is drawn
/// as one rather than as a streak across the pitch.
const MOTION_SNAP_M: f64 = 4.0;

/// Draw each physical prim where the simulator says it is going
/// (`docs/notes/physical-prims.md`).
///
/// Runs after [`place_geo_prims`] and overrides it for these prims: their
/// `PrimGeo` is still kept current (other systems match prims by it), but the
/// placement here is the extrapolated, eased one.
pub fn smooth_physical_prims(
    mut commands: Commands,
    time: Res<Time>,
    map_anchor: Res<MapAnchor>,
    mut prims: Query<(
        Entity,
        &mut crate::components::PhysicalMotion,
        &mut Transform,
        Option<&GridCell>,
    )>,
) {
    let Some(anchor) = map_anchor.geo else {
        return;
    };
    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    let now = time.elapsed_secs_f64();
    let dt = time.delta_secs();
    let blend = 1.0 - (-MOTION_EASE_RATE * dt).exp();
    for (entity, mut motion, mut transform, cell) in &mut prims {
        let (wx, wz) = lat_lng_to_world(anchor, motion.geo.latitude, motion.geo.longitude);
        let mut target = DVec3::new(wx, f64::from(motion.altitude), wz);
        if !motion.resting {
            let ahead = (now - motion.received).clamp(0.0, MOTION_EXTRAPOLATE_S);
            target += motion.velocity.as_dvec3() * ahead;
        }
        let shown = match motion.shown {
            Some(s) if s.distance(target) <= MOTION_SNAP_M => s + (target - s) * f64::from(blend),
            _ => target,
        };
        motion.shown = Some(shown);
        let (new_cell, local) = grid.translation_to_grid(shown);
        transform.translation = local;
        transform.rotation = if motion.resting {
            motion.rotation
        } else {
            transform.rotation.slerp(motion.rotation, blend)
        };
        if cell != Some(&new_cell) {
            commands.entity(entity).try_insert(new_cell);
        }
    }
}
