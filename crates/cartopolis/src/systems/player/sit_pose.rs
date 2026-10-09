//! The seated pose — a clip this client authors, because the character set has
//! none.
//!
//! The Quaternius "Ultimate Modular" set embeds the same 24 clips in every
//! character file (`Idle`, `Walk`, `Run`, six the emotes use, and a dozen weapon
//! motions), and **not one of them is a sit**. So "Sit at Bench" had nothing to
//! play and did what every other prompt does: fired the one-shot `Interact`
//! gesture, left a line of text on screen, and left the avatar standing.
//!
//! Two ways out were considered and one of them is not really available. Shipping
//! a sit animation means an asset — a `.gltf` under `.gitattributes`, i.e. an LFS
//! object with the documented silent-failure mode, authored against *two* rigs
//! (the men and the women differ, see [`Rig`]) that nothing in this tree can
//! author against. The other is what the soundscape already does with its WAVs:
//! **synthesise it**. A static pose is a one-frame clip, a one-frame clip is a
//! handful of bone rotations, and the rotations follow from the rig's own
//! measurements plus the seat height the bench is meshed at — so this module
//! computes it rather than storing it.
//!
//! ## What is computed, and what is measured
//!
//! Measured, from the character glTFs and restated in [`RIG_MAN`] / [`RIG_WOMAN`]:
//! the bind-pose positions of the hip, knee and ankle joints, the two bone
//! lengths, and each leg bone's bind-pose orientation. Those are properties of the
//! asset and cannot be derived.
//!
//! Computed here: where the joints go. The hips are put at [`HIP_HEIGHT`] above
//! the ground — a seat plus the height of the femur head above it — the ankles are
//! put on the ground one thigh-length in front, and the knee between them is the
//! two-link solution ([`solve_knee`]). That is the whole pose: thighs level, shins
//! vertical, feet flat. Doing it this way rather than with two tables of
//! quaternions is what makes the *same* code fit a rig whose legs are five
//! centimetres longer, and what lets a reader check the result against a bench.
//!
//! ## What it does not do
//!
//! Only the legs and the pelvis are keyed. Everything above `Body` — spine, arms,
//! head — carries no curve, so it holds whatever the outgoing clip left, which is
//! the idle's own relaxed upper body. That is deliberate: the pelvis keeps the
//! rig's built-in 27° yaw ([`BODY_YAW`]) precisely so the spine above it lands
//! where the idle put it, and a pose that re-authored the shoulders would have to
//! re-author the whole chain to keep them square.

use bevy::animation::animation_curves::AnimatableCurve;
use bevy::animation::{animated_field, AnimationTargetId};
use bevy::curve::{ConstantCurve, Interval};
use bevy::prelude::*;

use crate::systems::user_store::AvatarSex;

/// Height of the hip joint above the ground when seated.
///
/// [`cartopolis_geo::furniture::seat`] puts every slat this client can sit on at
/// 0.48 m; the femur head rides ~7 cm above the surface it rests on. Keeping the
/// sum here rather than passing the seat height in is a deliberate simplification
/// and the only reason it holds: the furniture table has exactly one seat height,
/// and `a_seat_is_the_top_of_a_slat_that_is_really_meshed` is what keeps it that
/// way.
const HIP_HEIGHT: f32 = 0.55;

/// The pelvis yaw the rig carries in every clip.
///
/// `Body` sits at this rotation in the bind file and in all 24 clips, and the
/// `Torso` above it carries the opposite twist, so the two cancel into a
/// square-shouldered character. Setting the pelvis to identity instead — the
/// obvious thing, since the bind pose's *global* pelvis rotation is identity —
/// leaves that counter-twist uncancelled and turns the torso 27° off the way the
/// avatar is walking.
const BODY_YAW: Quat = Quat::from_xyzw(0.0, 0.235_53, 0.0, 0.971_87);

/// A pose, expressed the way an [`AnimationClip`] wants it: one bone's local
/// transform, named by the path `bevy_gltf` hashed its `AnimationTargetId` from.
struct Keyed {
    path: &'static [&'static str],
    translation: Option<Vec3>,
    rotation: Quat,
}

/// The measurements one character rig contributes. Read out of the bind pose
/// (`skins[0].inverseBindMatrices`) of a representative character file; every
/// file for a sex agrees, which is what makes two of these enough for twenty
/// characters.
struct Rig {
    /// `UpperLeg.{L,R}`'s translation in `Body`'s frame — i.e. where the hip
    /// joints hang off the pelvis.
    hip_offset: [Vec3; 2],
    /// `LowerLeg`'s translation in `UpperLeg`'s frame is `(0, thigh, 0)`.
    thigh: f32,
    /// Knee to ankle. Not a local translation — the feet hang off `Root`, not off
    /// the shin (see [`FOOT_PATHS`]) — so it is the bind-pose distance.
    shin: f32,
    /// Bind-pose height of the ankle joint. The models are authored with the sole
    /// a couple of centimetres under it, so this *is* "foot flat on the ground".
    ankle_y: f32,
    /// Bind-pose orientation of `UpperLeg.{L,R}` / `LowerLeg.{L,R}`. Needed
    /// because a bone's roll about its own axis is not implied by where it points,
    /// and getting it wrong twists the thigh mesh inside out.
    upper_leg_bind: [Quat; 2],
    lower_leg_bind: [Quat; 2],
    /// Bind-pose orientation of `Foot.{L,R}`: flat, toes toward +z. Identical for
    /// both feet and both rigs, but kept beside the rest of the measurements.
    foot_bind: Quat,
}

/// `Root`'s own offset from the armature. The feet are keyed in this frame.
const ROOT_Z: f32 = 0.0723;

/// The men's rig (`Man_*.gltf`).
const RIG_MAN: Rig = Rig {
    hip_offset: [
        Vec3::new(0.120_14, 0.098_75, 0.018_38),
        Vec3::new(-0.121_04, 0.098_75, 0.018_38),
    ],
    thigh: 0.432_98,
    shin: 0.511_12,
    ankle_y: 0.022_8,
    upper_leg_bind: [
        Quat::from_xyzw(-0.706_258, 0.034_608, 0.706_261, -0.034_609),
        Quat::from_xyzw(0.706_258, 0.034_608, 0.706_261, 0.034_609),
    ],
    lower_leg_bind: [
        Quat::from_xyzw(0.999_693, 0.0, 0.0, -0.024_758),
        Quat::from_xyzw(0.999_693, 0.0, 0.0, -0.024_758),
    ],
    foot_bind: Quat::from_xyzw(0.705_214, 0.0, 0.0, 0.708_995),
};

/// The women's rig (`Woman_*.gltf`). Same height overall, but the legs are ~5 cm
/// longer and the torso correspondingly shorter — which is exactly why the pose
/// is solved rather than tabulated.
const RIG_WOMAN: Rig = Rig {
    hip_offset: [
        Vec3::new(0.096_41, 0.063_34, -0.023_23),
        Vec3::new(-0.097_31, 0.063_34, -0.023_23),
    ],
    thigh: 0.481_82,
    shin: 0.534_83,
    ankle_y: 0.022_8,
    upper_leg_bind: [
        Quat::from_xyzw(0.999_033, 0.0, 0.0, 0.043_973),
        Quat::from_xyzw(0.999_033, 0.0, 0.0, 0.043_973),
    ],
    lower_leg_bind: [
        Quat::from_xyzw(0.999_720, 0.0, 0.0, -0.023_660),
        Quat::from_xyzw(0.999_720, 0.0, 0.0, -0.023_660),
    ],
    foot_bind: Quat::from_xyzw(0.705_214, 0.0, 0.0, 0.708_995),
};

fn rig(sex: AvatarSex) -> &'static Rig {
    match sex {
        AvatarSex::Man => &RIG_MAN,
        AvatarSex::Woman => &RIG_WOMAN,
    }
}

// Bone name-paths, rooted at the scene's single root node exactly as
// `bevy_gltf::loader::collect_path` walks it — that is what the embedded clips'
// target ids were hashed from, so ours have to match them character for
// character.
const BODY_PATH: &[&str] = &["CharacterArmature", "Root", "Body"];
const UPPER_LEG_PATHS: [&[&str]; 2] = [
    &["CharacterArmature", "Root", "Body", "UpperLeg.L"],
    &["CharacterArmature", "Root", "Body", "UpperLeg.R"],
];
const LOWER_LEG_PATHS: [&[&str]; 2] = [
    &[
        "CharacterArmature",
        "Root",
        "Body",
        "UpperLeg.L",
        "LowerLeg.L",
    ],
    &[
        "CharacterArmature",
        "Root",
        "Body",
        "UpperLeg.R",
        "LowerLeg.R",
    ],
];
/// The feet hang off `Root`, not off the shins — the rig drives them directly
/// rather than through an ankle. So bending the knee does not carry the foot with
/// it and the pose has to place both.
const FOOT_PATHS: [&[&str]; 2] = [
    &["CharacterArmature", "Root", "Foot.L"],
    &["CharacterArmature", "Root", "Foot.R"],
];

/// Where the knee goes, given the two joints it is strung between.
///
/// Plane geometry: the knee is on the circle of radius `thigh` about the hip and
/// the circle of radius `shin` about the ankle, and of the two intersections it is
/// the one *in front* — a knee that bends the other way is a leg on backwards.
/// `forward` is the direction "in front" points in, and the answer is clamped to
/// the reachable case so an unreachable seat straightens the leg rather than
/// returning a NaN.
fn solve_knee(hip: Vec3, ankle: Vec3, thigh: f32, shin: f32, forward: Vec3) -> Vec3 {
    let span = ankle - hip;
    let d = span.length();
    if d < 1e-4 {
        return hip + forward * thigh;
    }
    let along = span / d;
    // Distance from the hip to the knee's projection on the hip→ankle line, and
    // the knee's offset off that line. `clamp` covers the two degenerate cases
    // (the joints further apart than the legs are long, or nested inside each
    // other) by straightening rather than by producing an imaginary triangle.
    let base = ((d * d + thigh * thigh - shin * shin) / (2.0 * d)).clamp(-thigh, thigh);
    let bulge = (thigh * thigh - base * base).max(0.0).sqrt();
    // The bulge direction: `forward` with everything along the leg removed, so the
    // knee leads without the answer sliding up or down the leg.
    let out = (forward - along * forward.dot(along)).normalize_or_zero();
    hip + along * base + out * bulge
}

/// The keyed bones of the seated pose for one rig.
fn seated_bones(rig: &Rig) -> Vec<Keyed> {
    // The pelvis, placed so the hips land at seat height. Its rotation is the
    // rig's own (`BODY_YAW`), so everything above it is untouched — which also
    // means the two hips are staggered a few centimetres front to back, exactly as
    // they are when the avatar stands.
    let hip_mid = BODY_YAW * ((rig.hip_offset[0] + rig.hip_offset[1]) * 0.5);
    let body = Vec3::new(0.0, HIP_HEIGHT, 0.0) - hip_mid;
    let mut keyed = vec![Keyed {
        path: BODY_PATH,
        translation: Some(body - Vec3::Z * ROOT_Z),
        rotation: BODY_YAW,
    }];

    for side in 0..2 {
        let hip = body + BODY_YAW * rig.hip_offset[side];
        // Each foot one thigh-length in front of its own hip. Measuring it per
        // side is what makes the shin vertical: the pelvis yaw staggers the two
        // hips a few centimetres front to back, and a shared foot line would rake
        // one shin forward and the other back by that much.
        let ankle = Vec3::new(hip.x, rig.ankle_y, hip.z + rig.thigh);
        let knee = solve_knee(hip, ankle, rig.thigh, rig.shin, Vec3::Z);

        // A bone's own axis is +y, so its orientation is "the bind orientation,
        // turned by the shortest arc that takes the bind direction onto this one".
        // Going through the arc rather than building a rotation from scratch is
        // what preserves the roll the mesh is skinned around.
        let upper_bind = rig.upper_leg_bind[side];
        let lower_bind = rig.lower_leg_bind[side];
        let upper =
            Quat::from_rotation_arc(upper_bind * Vec3::Y, (knee - hip).normalize()) * upper_bind;
        let lower =
            Quat::from_rotation_arc(lower_bind * Vec3::Y, (ankle - knee).normalize()) * lower_bind;

        keyed.push(Keyed {
            path: UPPER_LEG_PATHS[side],
            // Not keyed: the hip offset is a bone length, and the clips leave it
            // alone too.
            translation: None,
            // `Body` is the parent, and it is the only rotation between here and
            // the armature root.
            rotation: BODY_YAW.inverse() * upper,
        });
        keyed.push(Keyed {
            path: LOWER_LEG_PATHS[side],
            translation: None,
            rotation: upper.inverse() * lower,
        });
        keyed.push(Keyed {
            path: FOOT_PATHS[side],
            translation: Some(ankle - Vec3::Z * ROOT_Z),
            rotation: rig.foot_bind,
        });
    }
    keyed
}

/// How long the authored clip claims to be.
///
/// A pose has no duration, but a zero-length clip is a divide by its own cycle as
/// soon as anything asks it to repeat, so it gets a nominal second.
const SIT_CLIP_SECS: f32 = 1.0;

/// Build the seated clip for a rig. One constant curve per keyed bone.
#[must_use]
pub fn sit_clip(sex: AvatarSex) -> AnimationClip {
    let mut clip = AnimationClip::default();
    let domain = Interval::new(0.0, SIT_CLIP_SECS).expect("a positive clip length");
    for bone in seated_bones(rig(sex)) {
        let target = bone.path.iter().copied().collect::<AnimationTargetId>();
        clip.add_curve_to_target(
            target,
            AnimatableCurve::new(
                animated_field!(Transform::rotation),
                ConstantCurve::new(domain, bone.rotation),
            ),
        );
        if let Some(t) = bone.translation {
            clip.add_curve_to_target(
                target,
                AnimatableCurve::new(
                    animated_field!(Transform::translation),
                    ConstantCurve::new(domain, t),
                ),
            );
        }
    }
    clip
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forward kinematics over the keyed bones, so the tests can ask where a joint
    /// actually ended up rather than trusting the quaternions. Only the two
    /// rotations between the armature and a leg bone matter, and both are keyed.
    fn joints(rig: &Rig) -> Vec<(Vec3, Vec3, Vec3)> {
        let bones = seated_bones(rig);
        let body_t = bones[0].translation.unwrap() + Vec3::Z * ROOT_Z;
        let body_r = bones[0].rotation;
        (0..2)
            .map(|side| {
                let upper = body_r * bones[1 + side * 3].rotation;
                let lower = upper * bones[2 + side * 3].rotation;
                let foot_t = bones[3 + side * 3].translation.unwrap() + Vec3::Z * ROOT_Z;
                let hip = body_t + body_r * rig.hip_offset[side];
                let knee = hip + upper * Vec3::Y * rig.thigh;
                // Where the shin's own rotation says the ankle is. The foot is
                // keyed separately (it hangs off `Root`), so this has to agree
                // with `foot_t` or the leg ends somewhere the shoe is not.
                let shin_end = knee + lower * Vec3::Y * rig.shin;
                assert!(
                    shin_end.distance(foot_t) < 1e-3,
                    "the shin ends at {shin_end:?} but the foot is at {foot_t:?}"
                );
                (hip, knee, foot_t)
            })
            .collect()
    }

    /// The pose has to land on the furniture it was built for: hips at the seat,
    /// feet on the pavement. Both rigs, because their legs are different lengths
    /// and a tabulated pose would only have fitted one.
    #[test]
    fn a_seated_avatar_sits_on_the_bench_and_stands_on_the_ground() {
        for sex in [AvatarSex::Man, AvatarSex::Woman] {
            let r = rig(sex);
            for (hip, _knee, ankle) in joints(r) {
                assert!(
                    (hip.y - HIP_HEIGHT).abs() < 1e-3,
                    "{sex:?} hip at {} not {HIP_HEIGHT}",
                    hip.y
                );
                assert!(
                    (ankle.y - r.ankle_y).abs() < 1e-3,
                    "{sex:?} foot at {} not on the ground",
                    ankle.y
                );
            }
        }
    }

    /// The shape that makes it read as sitting rather than as squatting or
    /// kneeling: the knee is out in front of the hip, and the shin drops from it
    /// to the foot instead of raking forward or back.
    #[test]
    fn the_thigh_goes_forward_and_the_shin_goes_down() {
        for sex in [AvatarSex::Man, AvatarSex::Woman] {
            for (hip, knee, ankle) in joints(rig(sex)) {
                assert!(
                    knee.z - hip.z > 0.3,
                    "{sex:?} knee only {} m in front of the hip",
                    knee.z - hip.z
                );
                assert!(
                    (knee.y - hip.y).abs() < 0.05,
                    "{sex:?} thigh is not level: {} m of drop",
                    knee.y - hip.y
                );
                let shin = ankle - knee;
                assert!(
                    shin.y < -0.4 && shin.z.abs() < 0.05,
                    "{sex:?} shin runs {shin:?}, which is not straight down"
                );
            }
        }
    }

    /// The legs must not swap the joint they bend at. Solved from the front, the
    /// knee leads; solved from behind it trails, which is the same triangle and a
    /// broken leg.
    #[test]
    fn the_knee_bends_the_way_it_is_pointed() {
        let hip = Vec3::new(0.0, 0.55, 0.0);
        let ankle = Vec3::new(0.0, 0.02, 0.43);
        let front = solve_knee(hip, ankle, 0.433, 0.511, Vec3::Z);
        let back = solve_knee(hip, ankle, 0.433, 0.511, -Vec3::Z);
        assert!(front.z > ankle.z, "the knee should lead the ankle");
        assert!(back.z < hip.z, "reversed, the knee should trail the hip");
        // Both are real solutions of the same two-link problem.
        for knee in [front, back] {
            assert!((knee.distance(hip) - 0.433).abs() < 1e-3);
            assert!((knee.distance(ankle) - 0.511).abs() < 1e-3);
        }
    }

    /// An unreachable target straightens the leg instead of returning a NaN. The
    /// seat is fixed and the rigs are known, so this cannot happen today — but it
    /// is one re-measured bench away, and a NaN in a bone transform takes the
    /// whole skeleton with it.
    #[test]
    fn an_unreachable_foot_straightens_the_leg() {
        let hip = Vec3::new(0.0, 2.0, 0.0);
        let ankle = Vec3::new(0.0, 0.0, 0.0);
        let knee = solve_knee(hip, ankle, 0.433, 0.511, Vec3::Z);
        assert!(knee.is_finite());
        assert!((knee.distance(hip) - 0.433).abs() < 1e-3);
    }

    /// Every keyed bone names a real path in the rig, and names it only once —
    /// `AnimationTargetId` is a hash, so a typo binds to nothing at all and the
    /// bone simply keeps the pose the outgoing clip left it in. That failure looks
    /// exactly like a badly authored pose.
    #[test]
    fn the_keyed_bones_are_distinct_and_rooted_at_the_armature() {
        let mut seen: Vec<AnimationTargetId> = Vec::new();
        for bone in seated_bones(&RIG_MAN) {
            assert_eq!(bone.path[0], "CharacterArmature");
            assert_eq!(bone.path[1], "Root");
            assert!(bone.rotation.is_normalized(), "{:?}", bone.path);
            let id = bone.path.iter().copied().collect::<AnimationTargetId>();
            assert!(!seen.contains(&id), "{:?} keyed twice", bone.path);
            seen.push(id);
        }
        assert_eq!(seen.len(), 7, "pelvis, two legs of three bones each");
    }
}
