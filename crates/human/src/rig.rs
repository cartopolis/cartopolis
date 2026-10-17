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
    let pelvis = pack.bone("pelvis");
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
                // Not moved by the library: keep the rest pose under its parent.
                None => parent_world.unwrap_or(Quat::IDENTITY) * rest_local,
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
    Retargeted {
        local,
        pelvis: hips,
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
