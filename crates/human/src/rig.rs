//! The skeleton: where each bone sits in a shaped body, how it is turned, and
//! how a clip authored on another skeleton is played on it.
//!
//! **Placement.** A bone's head and tail are means of base-mesh vertices (the
//! joint cubes MakeHuman hides inside the body), taken *after* the body is
//! shaped — so a taller body has a taller skeleton, and the skin weights still
//! line up.
//!
//! **Orientation** is Blender's: the bone's Y axis runs head to tail, and the
//! roll turns it about that axis from the minimal rotation that takes Y onto
//! the bone (`vec_roll_to_mat3`). The rig's rolls and T-pose are authored in
//! Blender's Z-up frame, so the frame is built there and converted once by
//! [`BLENDER_TO_OURS`]; a bone's *local* frame is the same in both, which is what
//! lets the T-pose's bone-space Euler angles be applied unchanged.
//!
//! **Retargeting** goes through a T-pose both skeletons can stand in. The
//! library's clips are stored as each bone's world rotation relative to the
//! library's T-pose ([`crate::SourceClip`]); on this skeleton the same bone's
//! world rotation is that delta applied to *its* T-pose world rotation
//! ([`retarget`]), and the local rotations follow from the parents'. Neither the
//! bone rolls nor the rest poses (MakeHuman's is an A-pose, the library's a
//! T-pose) have to agree; only the two T-poses do.

use glam::{Mat3, Quat, Vec3};

use crate::{HumanPack, SourceClip};

/// Blender's frame (x right, y back-to-front reversed, z up) to ours (+Y up,
/// facing +Z): `(x, y, z) → (x, z, -y)`.
pub const BLENDER_TO_OURS: Mat3 = Mat3::from_cols(
    Vec3::new(1.0, 0.0, 0.0),
    Vec3::new(0.0, 0.0, -1.0),
    Vec3::new(0.0, 1.0, 0.0),
);

/// Ours to Blender's: the inverse of [`BLENDER_TO_OURS`].
#[must_use]
pub fn ours_to_blender(v: Vec3) -> Vec3 {
    Vec3::new(v.x, -v.z, v.y)
}

/// Blender's `vec_roll_to_mat3`: the frame of a bone along `dir` (unit) rolled
/// by `roll` radians, in whatever frame `dir` is given.
#[must_use]
pub fn bone_frame(dir: Vec3, roll: f32) -> Mat3 {
    let (x, y, z) = (dir.x, dir.y, dir.z);
    let theta = 1.0 + y;
    // The minimal rotation taking +Y onto `dir` (Rodrigues about Y × dir), or a
    // half turn about Z when `dir` is all but -Y.
    let align = if theta > 1.0e-5 || x * x + z * z > 1.0e-10 {
        let theta = if theta > 1.0e-5 {
            theta
        } else {
            let t = x * x + z * z;
            t * 0.5 + t * t * 0.125
        };
        Mat3::from_cols(
            Vec3::new(1.0 - x * x / theta, -x, -x * z / theta),
            Vec3::new(x, y, z),
            Vec3::new(-x * z / theta, -z, 1.0 - z * z / theta),
        )
    } else {
        Mat3::from_diagonal(Vec3::new(-1.0, -1.0, 1.0))
    };
    Mat3::from_axis_angle(dir, roll) * align
}

/// Blender's Euler XYZ (X applied first), as a quaternion.
#[must_use]
pub fn euler_xyz(e: [f32; 3]) -> Quat {
    Quat::from_rotation_z(e[2]) * Quat::from_rotation_y(e[1]) * Quat::from_rotation_x(e[0])
}

/// One bone of a placed skeleton, in our frame and metres.
#[derive(Clone, Copy, Debug)]
pub struct RestBone {
    pub head: Vec3,
    pub tail: Vec3,
    pub rotation: Quat,
}

/// The skeleton's rest pose for a shaped body: `positions` are the base-mesh
/// vertices already in our frame and metres (see [`crate::build`]).
#[must_use]
pub fn rest_pose(pack: &HumanPack, positions: &[Vec3]) -> Vec<RestBone> {
    let mean = |ix: &[u32]| -> Vec3 {
        let sum: Vec3 = ix.iter().map(|&i| positions[i as usize]).sum();
        sum / ix.len().max(1) as f32
    };
    pack.bones
        .iter()
        .map(|b| {
            let (head, tail) = (mean(&b.head), mean(&b.tail));
            let dir = ours_to_blender(tail - head).normalize_or(Vec3::Z);
            let frame = BLENDER_TO_OURS * bone_frame(dir, b.roll);
            RestBone {
                head,
                tail,
                rotation: Quat::from_mat3(&frame).normalize(),
            }
        })
        .collect()
}

/// Each bone's world rotation with the skeleton in MakeHuman's T-pose.
#[must_use]
pub fn tpose_world(pack: &HumanPack, rest: &[RestBone]) -> Vec<Quat> {
    let mut world: Vec<Quat> = Vec::with_capacity(rest.len());
    for (i, b) in pack.bones.iter().enumerate() {
        let pose = euler_xyz(b.tpose);
        let w = match b.parent {
            // parent_T · (parent_rest⁻¹ · rest) · pose
            Some(p) => {
                let p = p as usize;
                world[p] * (rest[p].rotation.inverse() * rest[i].rotation) * pose
            }
            None => rest[i].rotation * pose,
        };
        world.push(w.normalize());
    }
    world
}

/// A clip on this skeleton: per frame, every bone's local rotation, and the
/// pelvis's local translation.
#[derive(Clone, Debug)]
pub struct Retargeted {
    pub local: Vec<Vec<Quat>>,
    pub pelvis: Vec<Vec3>,
}

/// Play `clip` on the skeleton `rest`, whose T-pose is `tpose`. `hip_height` is
/// this body's pelvis above the ground, which scales the library's hip motion.
#[must_use]
pub fn retarget(
    pack: &HumanPack,
    rest: &[RestBone],
    tpose: &[Quat],
    clip: &SourceClip,
    hip_height: f32,
) -> Retargeted {
    let n = pack.bones.len();
    let tpose = &aligned_tpose(pack, rest, tpose, clip);
    let pelvis = pack.bone("pelvis");
    // The fingers no capture moves (`hands`): a turn per joint per frame on
    // top of the rest hand.
    let grip = crate::hands::grip_for(&clip.name);
    let mut fingers: Vec<Option<crate::hands::FingerJoint>> = vec![None; n];
    for j in crate::hands::finger_joints(pack, rest) {
        fingers[j.bone] = Some(j);
    }
    let mut local = vec![Vec::with_capacity(clip.frames as usize); n];
    let mut hips = Vec::with_capacity(clip.frames as usize);
    let mut world = vec![Quat::IDENTITY; n];
    for f in 0..clip.frames as usize {
        for (i, b) in pack.bones.iter().enumerate() {
            let parent_world = b.parent.map(|p| world[p as usize]);
            let rest_local = match b.parent {
                Some(p) => rest[p as usize].rotation.inverse() * rest[i].rotation,
                None => rest[i].rotation,
            };
            let w = match clip
                .rotations
                .get(i)
                .and_then(|r| r.as_ref())
                .and_then(|r| r.get(f))
            {
                Some(d) => (Quat::from_array(*d) * tpose[i]).normalize(),
                // Not moved by the library: keep the rest pose under its
                // parent, a finger curled as the clip's grip says.
                None => {
                    let turn = fingers[i].map_or(Quat::IDENTITY, |j| {
                        let phase = f as f32 / clip.frames.max(1) as f32;
                        let curl = crate::hands::curl_at(
                            grip,
                            phase,
                            0.04 * j.finger as f32,
                            j.side as f32,
                        );
                        crate::hands::joint_turn(&j, curl)
                    });
                    parent_world.unwrap_or(Quat::IDENTITY) * rest_local * turn
                }
            };
            world[i] = w;
            local[i].push(match parent_world {
                Some(pw) => (pw.inverse() * w).normalize(),
                None => w,
            });
        }
        if let Some(p) = pelvis {
            let offset =
                Vec3::from_array(clip.hips.get(f).copied().unwrap_or_default()) * hip_height;
            let at = rest[p].head + offset;
            let local_at = match pack.bones[p].parent {
                Some(pp) => {
                    let pp = pp as usize;
                    // The parent's pose at this frame, not its rest: its
                    // rotation is the one just computed.
                    world[pp].inverse() * (at - rest[pp].head)
                }
                None => at,
            };
            hips.push(local_at);
        }
    }
    level_gaze(pack, rest, &mut local);
    ground(pack, rest, &local, &mut hips);
    Retargeted {
        local,
        pelvis: hips,
    }
}

/// The T-pose with each bone turned to point the way the take's bone points
/// at rest ([`crate::SourceClip::rest_dirs`]).
///
/// Two T-poses agree on the arms and little else. A capture skeleton's spine,
/// neck and head stand dead straight at rest, while MakeHuman's lean forward as
/// a real one does; the take's own forward lean of the neck was therefore added
/// to a neck that already had it, and every first retarget looked at its feet.
/// CMU's legs splay twenty degrees at rest, which would bow-leg every clip the
/// same way. Turning by the smallest rotation keeps the body's own twist.
fn aligned_tpose(
    pack: &HumanPack,
    rest: &[RestBone],
    tpose: &[Quat],
    clip: &SourceClip,
) -> Vec<Quat> {
    let n = pack.bones.len();
    let mut at = vec![Vec3::ZERO; n];
    for (i, b) in pack.bones.iter().enumerate() {
        at[i] = match b.parent {
            Some(p) => {
                let p = p as usize;
                at[p] + tpose[p] * (rest[p].rotation.inverse() * (rest[i].head - rest[p].head))
            }
            None => rest[i].head,
        };
    }
    (0..n)
        .map(|i| {
            let Some(d) = clip.rest_dirs.get(i).copied().flatten() else {
                return tpose[i];
            };
            let points = match d.child {
                Some(c) => at[c as usize] - at[i],
                None => tpose[i] * (rest[i].rotation.inverse() * (rest[i].tail - rest[i].head)),
            };
            let want = Vec3::from_array(d.dir);
            if points.length() < 1.0e-5 || want.length() < 1.0e-5 {
                return tpose[i];
            }
            (Quat::from_rotation_arc(points.normalize(), want.normalize()) * tpose[i]).normalize()
        })
        .collect()
}

/// Where an upright clip's face points on average: a few degrees below the
/// horizon, the ground ahead of someone walking.
const GAZE_PITCH: f32 = -6.0_f32.to_radians();
/// A clip whose head pitches more than this over its length is tumbling (the
/// roll, the fall), and its gaze is left as captured.
const GAZE_UPRIGHT_RANGE: f32 = 40.0_f32.to_radians();

/// Turn the head so an upright clip's face looks, on average, at
/// [`GAZE_PITCH`]; the motion about that average is the capture's.
///
/// Retargeted as they came, the clips held the face 20–25° down at idle and
/// in the gestures and 42° down in the walk, the run level (measured
/// 2026-10-18 on the default man): a figure that studies the pavement. How
/// much of that the subjects did and how much the retarget adds (a capture's
/// neck and head are straight bones, MakeHuman's a curve) was not settled;
/// either way a person in a street looks ahead.
///
/// **The neck is made, not captured: halfway between the chest and the
/// head.** CMU's skeleton has two neck joints and the rig one, and played
/// through the lower of the two the gestures held the rig's neck bent ~55°
/// back with the head bent forward to make up for it — a kink that showed as
/// a head dropped onto the shoulder. Halfway is roughly what a neck does, and
/// it carries half of the gaze's lift with it. A tumbling clip keeps its
/// captured gaze but gets the same neck.
fn level_gaze(pack: &HumanPack, rest: &[RestBone], local: &mut [Vec<Quat>]) {
    let (Some(neck), Some(head)) = (pack.bone("neck_01"), pack.bone("head")) else {
        return;
    };
    let frames = local.first().map_or(0, Vec::len);
    if frames == 0 {
        return;
    }
    let world_at = |local: &[Vec<Quat>], f: usize| -> Vec<Quat> {
        let mut w = vec![Quat::IDENTITY; pack.bones.len()];
        for (i, b) in pack.bones.iter().enumerate() {
            w[i] = match b.parent {
                Some(p) => w[p as usize] * local[i][f],
                None => local[i][f],
            };
        }
        w
    };
    // The body faces +z at rest, so the face's direction is the head's turn
    // from rest applied to +z.
    let face = |w: &[Quat]| w[head] * rest[head].rotation.inverse() * Vec3::Z;
    let pitches: Vec<f32> = (0..frames)
        .map(|f| face(&world_at(local, f)).y.clamp(-1.0, 1.0).asin())
        .collect();
    let (lo, hi) = pitches
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &p| (lo.min(p), hi.max(p)));
    let mean = pitches.iter().sum::<f32>() / frames as f32;
    let lift = if hi - lo > GAZE_UPRIGHT_RANGE {
        0.0
    } else {
        GAZE_PITCH - mean
    };
    let Some(neck_parent) = pack.bones[neck].parent.map(|p| p as usize) else {
        return;
    };
    for f in 0..frames {
        let w = world_at(local, f);
        // Raise about the axis across the face, whichever way it turns.
        let Some(across) = face(&w).cross(Vec3::Y).try_normalize() else {
            continue;
        };
        let head_w = Quat::from_axis_angle(across, lift) * w[head];
        // The neck halfway between the chest and the head, each as its turn
        // from rest, so the rest pose's curve is kept.
        let turn = |b: usize, w: Quat| w * rest[b].rotation.inverse();
        let neck_turn = turn(neck_parent, w[neck_parent]).slerp(turn(head, head_w), 0.5);
        let neck_w = neck_turn * rest[neck].rotation;
        local[neck][f] = (w[neck_parent].inverse() * neck_w).normalize();
        local[head][f] = (neck_w.inverse() * head_w).normalize();
    }
}

/// Stand a clip on the ground: shift the pelvis so the lowest an ankle gets
/// over the whole clip is where the ankle stands at rest.
///
/// A take's floor is wherever its capture put it, and its hips' height is in
/// its own units; rather than trust either, the clip is placed by its feet. One
/// shift for the whole clip, so a jump still leaves the ground.
fn ground(pack: &HumanPack, rest: &[RestBone], local: &[Vec<Quat>], pelvis_at: &mut [Vec3]) {
    let (Some(pelvis), Some(feet)) = (
        pack.bone("pelvis"),
        pack.bone("foot_l").zip(pack.bone("foot_r")),
    ) else {
        return;
    };
    let frames = pelvis_at.len();
    let rest_ankle = rest[feet.0].head.y.min(rest[feet.1].head.y);
    let mut lowest = f32::INFINITY;
    let mut world: Vec<(Vec3, Quat)> = vec![(Vec3::ZERO, Quat::IDENTITY); pack.bones.len()];
    for f in 0..frames {
        for (i, b) in pack.bones.iter().enumerate() {
            let r = local[i].get(f).copied().unwrap_or(rest[i].rotation);
            world[i] = match b.parent {
                Some(p) => {
                    let p = p as usize;
                    let t = if i == pelvis {
                        pelvis_at[f]
                    } else {
                        rest[p].rotation.inverse() * (rest[i].head - rest[p].head)
                    };
                    (world[p].0 + world[p].1 * t, world[p].1 * r)
                }
                None => (rest[i].head, r),
            };
        }
        lowest = lowest.min(world[feet.0].0.y).min(world[feet.1].0.y);
    }
    if !lowest.is_finite() {
        return;
    }
    // In the pelvis's parent's frame, which is the root's: turned at most
    // about the vertical, so a world lift is a lift along its local up.
    let lift = rest_ankle - lowest;
    let up = pack.bones[pelvis]
        .parent
        .map_or(Vec3::Y, |p| rest[p as usize].rotation.inverse() * Vec3::Y);
    for at in pelvis_at.iter_mut() {
        *at += up * lift;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1.0e-5
    }

    /// The bone's Y axis is the bone, whatever the roll; the roll turns X and
    /// Z about it.
    #[test]
    fn a_bone_frame_points_its_y_axis_along_the_bone() {
        for dir in [
            Vec3::X,
            Vec3::Y,
            -Vec3::Y,
            Vec3::Z,
            Vec3::new(0.3, -0.8, 0.52).normalize(),
        ] {
            for roll in [0.0, 0.7, -2.4] {
                let m = bone_frame(dir, roll);
                assert!(close(m.y_axis, dir), "{dir} {roll}: {:?}", m.y_axis);
                assert!((m.determinant() - 1.0).abs() < 1.0e-4, "a rotation");
            }
        }
        // Blender: a bone straight up (+Z) with no roll has X along +X.
        let up = bone_frame(Vec3::Z, 0.0);
        assert!(close(up.x_axis, Vec3::X), "{:?}", up.x_axis);
    }

    #[test]
    fn the_frame_change_is_a_rotation_and_round_trips() {
        assert!((BLENDER_TO_OURS.determinant() - 1.0).abs() < 1.0e-6);
        let v = Vec3::new(0.2, -1.3, 0.7);
        assert!(close(BLENDER_TO_OURS * ours_to_blender(v), v));
        // Blender's up is ours.
        assert!(close(BLENDER_TO_OURS * Vec3::Z, Vec3::Y));
    }

    #[test]
    fn euler_xyz_applies_x_first() {
        let q = euler_xyz([
            std::f32::consts::FRAC_PI_2,
            0.0,
            std::f32::consts::FRAC_PI_2,
        ]);
        // X first takes Y to Z; Z (a quarter turn) then leaves Z where it is.
        assert!(close(q * Vec3::Y, Vec3::Z), "{:?}", q * Vec3::Y);
    }
}
