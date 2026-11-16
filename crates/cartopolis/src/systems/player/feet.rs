//! The feet stood on the ground they are over (`docs/notes/foot-placement.md`).
//!
//! The clips are captured on a flat floor, and an avatar is stood on the one
//! ground height under its origin (`avatar::ground_or_water`), so on a slope
//! the foot ahead goes into the ground and the one behind floats. Each frame,
//! after the animation has written the joints and before they are propagated
//! ([`place_feet`]), each leg is solved so its foot lands on the ground under
//! it (`cartopolis_human::feet`): the pelvis comes down for the lower foot,
//! the thigh and calf turn, and a foot near the ground is tilted to its slope.
//!
//! Only on the ground, upright and on its own feet — not swimming, sitting,
//! climbing, in an emote or in the air, and not on a floor, a wall top or a
//! roof, which the ground field does not hold (`feet::Footing`). Eased in and
//! out over `feet::BLEND_S`. Every viewer does it on its own map, for its own
//! avatar and for the others within [`RANGE_M`] of its camera; nothing is on
//! the wire.
//!
//! `CARTO_NO_FOOT_IK=1` leaves the clips' feet alone (the "before" of a
//! capture); `CARTO_FOOT_LOG=1` logs the local avatar's feet against the
//! ground under them, before and after, every frame.

use std::sync::OnceLock;

use bevy::prelude::*;
use cartopolis_human::feet::{self, Footing, Leg};

use crate::components::{Avatar, RemoteAvatar};
use crate::resources::AvatarState;
use crate::systems::avatar::{AvatarHolder, ClipName, EmotePlayback, PinnedAnimation};
use crate::systems::terrain::TerrainField;

/// How far from the camera another avatar's feet are still placed, metres:
/// past it a foot's few centimetres are under a pixel.
pub const RANGE_M: f32 = 40.0;

/// How far either side of a foot the ground is sampled for its slope, metres:
/// about a foot's length.
const SLOPE_SPAN_M: f32 = 0.15;

/// How fast the pelvis follows the drop the feet ask for (1/s): a kerb-sized
/// step in the ground under one foot lowers the body over a few frames, not
/// in one.
const DROP_RATE: f32 = 20.0;

/// A foot this far over the clip's own floor (its ankle over where it stands
/// at rest) is lifted in a stride, metres: it bears no weight, is not tilted
/// to the ground and does not bring the pelvis down. Below [`STANCE_FULL_M`]
/// it is planted. The idle stands its ankles 3–4 cm over rest (the shoes'
/// soles; `build::plant_feet`), and 3 cm called a standing foot nine tenths
/// planted, leaving it 3 mm short of lower ground.
const STANCE_NONE_M: f32 = 0.10;
const STANCE_FULL_M: f32 = 0.045;

/// One leg's joints on a body, and its ankle's height over the soles at rest.
#[derive(Clone, Copy, Debug)]
pub struct LegJoints {
    pub thigh: Entity,
    pub calf: Entity,
    pub foot: Entity,
    pub rest_ankle: f32,
}

/// A body's legs, by the `game_engine` rig's names, kept on its holder. Made
/// with the body (`human_avatar::spawn_human`).
#[derive(Component, Clone, Copy, Debug)]
pub struct HumanLegs {
    pub pelvis: Entity,
    /// Left first.
    pub legs: [LegJoints; 2],
}

impl HumanLegs {
    /// Bind a body's joints by name: `joint` answers a name with the joint's
    /// entity and its rest position in the holder's frame.
    #[must_use]
    pub fn new(joint: impl Fn(&str) -> Option<(Entity, Vec3)>) -> Option<Self> {
        let leg = |s: &str| {
            let (thigh, _) = joint(&format!("thigh_{s}"))?;
            let (calf, _) = joint(&format!("calf_{s}"))?;
            let (foot, ankle) = joint(&format!("foot_{s}"))?;
            Some(LegJoints {
                thigh,
                calf,
                foot,
                rest_ankle: ankle.y,
            })
        };
        Some(Self {
            pelvis: joint("pelvis")?.0,
            legs: [leg("l")?, leg("r")?],
        })
    }
}

/// How far in the placement is on one body, and the pelvis's drop as eased.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct FootPlacement {
    pub weight: f32,
    pub drop: f32,
}

#[derive(Clone, Copy)]
struct Switches {
    off: bool,
    log: bool,
}

fn switches() -> Switches {
    static S: OnceLock<Switches> = OnceLock::new();
    *S.get_or_init(|| Switches {
        off: crate::debug_switches::env_flag("CARTO_NO_FOOT_IK"),
        log: crate::debug_switches::env_flag("CARTO_FOOT_LOG"),
    })
}

/// A joint's pose in its holder's frame, from the transforms as the animation
/// left them this frame.
fn in_holder(entity: Entity, holder: Entity, joints: &Joints) -> Option<(Vec3, Quat)> {
    let (mut at, mut rotation) = (Vec3::ZERO, Quat::IDENTITY);
    let mut e = entity;
    while e != holder {
        let (t, parent) = joints.get(e).ok()?;
        at = t.translation + t.rotation * at;
        rotation = t.rotation * rotation;
        e = parent.parent();
    }
    Some((at, rotation))
}

type Joints<'w, 's> = Query<
    'w,
    's,
    (&'static mut Transform, &'static ChildOf),
    (
        Without<AvatarHolder>,
        Without<Avatar>,
        Without<RemoteAvatar>,
    ),
>;

/// Place every near body's feet on the ground under them, over the clip.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn place_feet(
    time: Res<Time>,
    terrain: Res<TerrainField>,
    state: Res<AvatarState>,
    pinned: Option<Res<PinnedAnimation>>,
    locals: Query<(&Transform, Has<EmotePlayback>), With<Avatar>>,
    remotes: Query<(&Transform, &RemoteAvatar, Has<EmotePlayback>), Without<Avatar>>,
    camera: Query<&GlobalTransform, With<crate::components::FreeCamera>>,
    mut holders: Query<
        (
            Entity,
            &ChildOf,
            &Transform,
            &GlobalTransform,
            &HumanLegs,
            &mut FootPlacement,
        ),
        With<AvatarHolder>,
    >,
    mut joints: Joints,
) {
    let sw = switches();
    if sw.off && !sw.log {
        return;
    }
    let dt = time.delta_secs();
    let eye = camera.single().ok().map(GlobalTransform::translation);
    let ground = |x: f32, z: f32| terrain.ground_at(x, z);
    for (holder, parent, holder_tf, holder_at, legs, mut placement) in &mut holders {
        let owner = parent.parent();
        // Whose body, where it stands, and whether its feet are placed.
        let (body, footing, local, near) = if let Ok((tf, emoting)) = locals.get(owner) {
            let pinned_other = pinned.as_ref().is_some_and(|p| {
                !matches!(p.clip, ClipName::Idle | ClipName::Walk | ClipName::Run)
            });
            let footing = Footing {
                swimming: state.swimming,
                seated: state.seated.is_some(),
                climbing: state.climb.is_some(),
                emoting: emoting || pinned_other,
                airborne: !state.grounded,
                over_ground: tf.translation.y - ground(tf.translation.x, tf.translation.z),
            };
            (*tf, footing, true, true)
        } else if let Ok((tf, remote, emoting)) = remotes.get(owner) {
            let near = eye.is_none_or(|e| e.distance(holder_at.translation()) <= RANGE_M);
            // A jump or a fall shows as height over the ground: the sim
            // does not say whether a remote is grounded.
            let footing = Footing {
                swimming: remote.swimming || remote.swim_blend > 0.0,
                seated: remote.seated.is_some() || remote.seat_blend > 0.0,
                climbing: remote.climb.is_some(),
                emoting,
                airborne: false,
                over_ground: tf.translation.y - ground(tf.translation.x, tf.translation.z),
            };
            (*tf, footing, false, near)
        } else {
            continue;
        };
        let logging = sw.log && local;
        if sw.off && !logging {
            continue;
        }
        let on = footing.places_feet() && near && !sw.off;
        placement.weight = feet::blend(placement.weight, on, dt);
        if placement.weight <= 0.0 && !logging {
            placement.drop = 0.0;
            continue;
        }
        let w = placement.weight;
        let pose = |e| in_holder(e, holder, &joints);
        let Ok((_, pelvis_parent)) = joints.get(legs.pelvis) else {
            continue;
        };
        let Some((_, root_rot)) = pose(pelvis_parent.parent()) else {
            continue;
        };
        // Each leg as the clip has it, the ground under its foot, and the
        // slope there in the holder's frame.
        let to_world = body * *holder_tf;
        let to_holder = to_world.rotation.inverse();
        // The ground as drawn (`TerrainField::ground_at`, `systems::drawn_ground`),
        // which is also what the body stands on.
        let drawn = ground;
        let here = drawn(body.translation.x, body.translation.z);
        // Where the body stands over it (logged): a teleport arriving, a
        // water lift easing, and nothing once standing.
        let body_off = body.translation.y - here;
        let mut clip = [None; 2];
        let mut rise = [0.0f32; 2];
        let mut normal = [Vec3::Y; 2];
        let mut stance = [0.0f32; 2];
        let mut gap = [0.0f32; 2];
        for (i, lj) in legs.legs.iter().enumerate() {
            let (Some(thigh), Some(calf), Some(foot)) =
                (pose(lj.thigh), pose(lj.calf), pose(lj.foot))
            else {
                continue;
            };
            let Ok((_, thigh_parent)) = joints.get(lj.thigh) else {
                continue;
            };
            let Some((_, above)) = pose(thigh_parent.parent()) else {
                continue;
            };
            let at = to_world.transform_point(foot.0);
            let under = drawn(at.x, at.z);
            let lifted = foot.0.y - lj.rest_ankle;
            // How far the foot is over (or in) the drawn ground under it
            // beyond what the clip has on its own flat floor, less where the
            // body stands: zero is the clip's contact, whatever phase of the
            // stride.
            gap[i] = (at.y - under) - foot.0.y - body_off;
            rise[i] = feet::step(under - here) * w;
            let n = feet::ground_normal(
                [under, drawn(at.x + 2.0 * SLOPE_SPAN_M, at.z)],
                [under, drawn(at.x, at.z + 2.0 * SLOPE_SPAN_M)],
                SLOPE_SPAN_M,
            );
            normal[i] = to_holder * n;
            stance[i] =
                ((STANCE_NONE_M - lifted) / (STANCE_NONE_M - STANCE_FULL_M)).clamp(0.0, 1.0);
            clip[i] = Some((thigh, calf, foot, above));
        }
        let as_leg = |c: Option<((Vec3, Quat), (Vec3, Quat), (Vec3, Quat), Quat)>| {
            c.map(|(thigh, calf, foot, _)| Leg {
                hip: thigh.0,
                knee: calf.0,
                ankle: foot.0,
            })
        };
        let (Some(left), Some(right)) = (as_leg(clip[0]), as_leg(clip[1])) else {
            continue;
        };
        let want = feet::pelvis_drop(&[left, right], rise, stance);
        placement.drop += (want - placement.drop) * (1.0 - (-DROP_RATE * dt).exp());
        let drop = placement.drop;

        // The pelvis down, in its parent's frame.
        let write = w > 0.0;
        if write {
            if let Ok((mut t, _)) = joints.get_mut(legs.pelvis) {
                t.translation += root_rot.inverse() * (Vec3::NEG_Y * drop);
            }
        }
        let mut after = [0.0f32; 2];
        let mut reached = [true; 2];
        for (i, lj) in legs.legs.iter().enumerate() {
            let Some((thigh, calf, foot, above)) = clip[i] else {
                continue;
            };
            let leg = [left, right][i];
            let turn = feet::plant_leg(&leg, drop, rise[i], Vec3::Z);
            let thigh_rot = turn.thigh * thigh.1;
            let calf_rot = turn.calf * turn.thigh * calf.1;
            let tilt = feet::foot_tilt(normal[i], w * stance[i]);
            let foot_rot = tilt * turn.calf * turn.thigh * foot.1;
            if write {
                for (e, local) in [
                    (lj.thigh, above.inverse() * thigh_rot),
                    (lj.calf, thigh_rot.inverse() * calf_rot),
                    (lj.foot, calf_rot.inverse() * foot_rot),
                ] {
                    if let Ok((mut t, _)) = joints.get_mut(e) {
                        t.rotation = local.normalize();
                    }
                }
            }
            reached[i] = turn.reached;
            if logging {
                let down = Vec3::Y * drop;
                let knee = leg.hip - down + turn.thigh * (leg.knee - leg.hip);
                let ankle = knee + turn.calf * turn.thigh * (leg.ankle - leg.knee);
                after[i] = gap[i] + (ankle.y - leg.ankle.y);
            }
        }
        if logging {
            tracing::info!(
                left_gap_cm = gap[0] * 100.0,
                right_gap_cm = gap[1] * 100.0,
                left_after_cm = after[0] * 100.0,
                right_after_cm = after[1] * 100.0,
                drop_cm = drop * 100.0,
                want_drop_cm = want * 100.0,
                over_ground_cm = footing.over_ground * 100.0,
                body_over_drawn_cm = body_off * 100.0,
                left_reached = u8::from(reached[0]),
                right_reached = u8::from(reached[1]),
                weight = w,
                left_stance = stance[0],
                right_stance = stance[1],
                "feet: soles over the ground under them"
            );
        }
    }
}
