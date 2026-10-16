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

/// How far (m) a prim may have moved since the lift went on and still have it
/// taken off at the top of the next frame. Covers `big_space` re-gridding a
/// prim between the lift and the strip (same world point, new cell, float
/// rounding); anything further means something rewrote the `Transform` after
/// the lift went on, with a flat value, and there is nothing to take off.
const LIFT_STRIP_TOLERANCE_M: f64 = 0.01;

/// How much to take off a prim's local `y` at the top of a frame, given where
/// it is now (cell + local).
#[must_use]
pub(crate) fn lift_to_strip(lift: &crate::components::PrimLift, world_now: DVec3) -> f32 {
    match lift.shown {
        Some(shown) if shown.distance(world_now) <= LIFT_STRIP_TOLERANCE_M => lift.lift,
        _ => 0.0,
    }
}

/// Put the lift for `ground` on a prim whose flat world position is `flat`.
/// Returns whether the drawn position moved since the lift last went on — i.e.
/// whether transform propagation has to hear about it.
pub(crate) fn put_lift_on(
    lift: &mut crate::components::PrimLift,
    flat: DVec3,
    ground: f32,
) -> bool {
    let moved = ground != lift.lift;
    lift.lift = ground;
    lift.shown = (ground != 0.0).then(|| flat + DVec3::Y * f64::from(ground));
    moved
}

/// Take last frame's terrain lift back off every prim, so everything in
/// `Update` sees a prim's `Transform` in the sim's flat frame — the frame the
/// gizmo, the edit dialog, the MCP agent and `place_geo_prims` read it in and
/// send it back to the server in. See [`crate::components::PrimLift`].
///
/// Bypasses change detection: the drawn position has not moved, only the frame
/// the `Transform` is written in, and [`lift_prims_onto_terrain`] flags the
/// prims whose drawn position does move.
pub fn strip_prim_lift(
    mut prims: Query<(
        &mut crate::components::PrimLift,
        &mut Transform,
        Option<&GridCell>,
    )>,
) {
    for (mut lift, mut transform, cell) in &mut prims {
        if lift.shown.is_none() {
            continue;
        }
        let take = lift_to_strip(&lift, world_position(cell, &transform));
        lift.bypass_change_detection().shown = None;
        if take != 0.0 {
            transform.bypass_change_detection().translation.y -= take;
        }
    }
}

/// Stand every prim on the measured ground under it, for drawing only.
///
/// The sim is flat — `y = 0` is the ground everywhere, since it holds no map
/// data — so a prim's `y` is its height above the ground, and each client adds
/// the ground it measured, exactly as it does for its own avatar
/// (`AvatarState::terrain_lift`) and for other people's
/// (`RemoteAvatar::terrain_lift`). Before this only the local avatar was
/// lifted: a build a kilometre from the anchor, where the ground is lower than
/// the anchor's datum, floated above it on every screen.
///
/// **One height per prim**, sampled under its anchor — a prim is a rigid
/// object, and draping its vertices would rack it out of shape (the rule the
/// buildings and the water follow). Sampled every frame rather than on
/// `TerrainField::revision`: the field lands a second or two after the anchor
/// and refines after that, a scripted prim moves, and the lookup is a bilinear
/// sample per prim. Only a prim whose lift *changed* is flagged for
/// propagation; the rest are written around change detection, since their
/// `GlobalTransform` already shows exactly this.
///
/// Runs in `PostUpdate` before propagation, and [`strip_prim_lift`] takes it
/// off in `First`, so the lift lives only in the `GlobalTransform` — which is
/// what picking, the gizmo handles, the hand sensor and the renderer read.
pub fn lift_prims_onto_terrain(
    terrain: Res<crate::systems::terrain::TerrainField>,
    mut prims: Query<(
        &mut crate::components::PrimLift,
        &mut Transform,
        Option<&GridCell>,
    )>,
) {
    for (mut lift, mut transform, cell) in &mut prims {
        let flat = world_position(cell, &transform);
        let ground = terrain.ground_at(flat.x as f32, flat.z as f32);
        if ground == 0.0 && lift.lift == 0.0 {
            continue;
        }
        let lift = lift.bypass_change_detection();
        if put_lift_on(lift, flat, ground) {
            transform.translation.y += ground;
        } else {
            transform.bypass_change_detection().translation.y += ground;
        }
    }
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

/// The camera-attachment owner name for objects (`systems::camera_attach`); the
/// id is the prim's.
pub const PRIM_ATTACH_SOURCE: &str = "prim";

/// The attachment key for prim `id`.
#[must_use]
pub fn prim_attach_key(id: i64) -> crate::systems::camera_attach::AttachKey {
    crate::systems::camera_attach::AttachKey {
        source: PRIM_ATTACH_SOURCE,
        id: id as u64,
    }
}

/// Where the camera sits round a followed object: a few metres off, as a
/// person would stand to look at it.
pub const PRIM_ATTACH_RANGE: crate::systems::camera_attach::AttachRange =
    crate::systems::camera_attach::AttachRange {
        start_m: 12.0,
        min_m: 2.0,
        max_m: 5_000.0,
    };

/// While the camera follows an object, tell it where the object is. A prim a
/// script moves is the case this is for; one that is deleted lets the camera go.
pub fn report_attached_prim(
    prims: Query<(
        &Prim,
        Option<&GridCell>,
        &Transform,
        &crate::components::PrimLift,
    )>,
    mut attach: ResMut<crate::systems::camera_attach::CameraAttach>,
) {
    let Some(key) = attach.key().filter(|k| k.source == PRIM_ATTACH_SOURCE) else {
        return;
    };
    match prims.iter().find(|(p, ..)| p.id as u64 == key.id) {
        // Where the object is *drawn*: the `Transform` is flat during `Update`
        // and the terrain lift goes on in `PostUpdate` (`lift_prims_onto_terrain`).
        Some((_, cell, tf, lift)) => attach.report(
            key,
            world_position(cell, tf) + DVec3::Y * f64::from(lift.lift),
            DVec3::Y,
        ),
        None => attach.detach_if(key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::PrimLift;

    /// One frame of the lift's life: the strip at the top of the frame, what
    /// `Update` sees, then the lift going on again. Returns (`y` as `Update`
    /// saw it, drawn `y`, whether propagation was told).
    fn frame(lift: &mut PrimLift, local_y: &mut f32, ground: f32) -> (f32, f32, bool) {
        let at = |y: f32| DVec3::new(250.0, f64::from(y), -40.0);
        *local_y -= lift_to_strip(lift, at(*local_y));
        lift.shown = None;
        let seen = *local_y;
        let flagged = put_lift_on(lift, at(*local_y), ground);
        *local_y += ground;
        (seen, *local_y, flagged)
    }

    #[test]
    fn a_prim_is_drawn_on_the_ground_and_edited_in_the_sims_frame() {
        // A theremin rod 0.5 m above the sim's ground, where the measured ground
        // is three metres below the anchor's datum.
        let (mut lift, mut y) = (PrimLift::default(), 0.5_f32);
        for i in 0..10 {
            let (seen, drawn, flagged) = frame(&mut lift, &mut y, -3.0);
            assert_eq!(seen, 0.5, "frame {i}: Update saw a lifted prim");
            assert_eq!(drawn, -2.5, "frame {i}: not drawn on the ground");
            // Only the first frame moves what is drawn.
            assert_eq!(flagged, i == 0, "frame {i}");
        }
    }

    #[test]
    fn a_new_field_moves_the_drawing_and_nothing_else() {
        let (mut lift, mut y) = (PrimLift::default(), 1.0_f32);
        frame(&mut lift, &mut y, 0.0);
        // The field lands a second after the anchor…
        assert_eq!(frame(&mut lift, &mut y, -2.0), (1.0, -1.0, true));
        // …and refines after that.
        assert_eq!(frame(&mut lift, &mut y, -2.25), (1.0, -1.25, true));
        // Going away puts the prim back on the flat plane.
        assert_eq!(frame(&mut lift, &mut y, 0.0), (1.0, 1.0, true));
        assert!(lift.shown.is_none());
    }

    #[test]
    fn a_transform_rewritten_after_the_lift_is_not_stripped_twice() {
        let mut lift = PrimLift::default();
        let flat = DVec3::new(10.0, 0.5, 10.0);
        put_lift_on(&mut lift, flat, -3.0);
        // Something wrote the sim's position back over the lifted one.
        assert_eq!(lift_to_strip(&lift, flat), 0.0);
        // A prim still where the lift left it has it taken off…
        assert_eq!(lift_to_strip(&lift, flat - DVec3::Y * 3.0), -3.0);
        // …including when big_space re-gridded it on the way (float rounding).
        assert_eq!(
            lift_to_strip(&lift, flat - DVec3::Y * 3.0 + DVec3::X * 0.001),
            -3.0
        );
        // A prim with no lift on has nothing to strip.
        assert_eq!(lift_to_strip(&PrimLift::default(), flat), 0.0);
    }
}
