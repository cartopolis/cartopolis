//! The fingers, which no capture here records.
//!
//! 100STYLE's skeleton stops at the wrist and CMU's has one stub per hand, so
//! every clip leaves the rig's fifteen finger joints a hand where they were at
//! rest: the same half-open hand through a walk, a run and a punch. A hand's
//! shape mostly says what the arm is doing — loose while walking, a loose fist
//! while running, a fist to punch, open to wave — so it is made here from the
//! clip's name and its phase, the part of animation code can usefully make
//! (`docs/notes/makehuman-avatars.md`).
//!
//! **Which way a finger bends is read off the rest pose.** MakeHuman's rest
//! hand is already relaxed — each finger's three segments bend a little toward
//! the palm — so the plane of that bend is the plane a fist closes in, without
//! knowing anything about the bones' rolls. A finger too straight to say takes
//! the hand's other fingers' axis.

use glam::{Quat, Vec3};

use crate::rig::RestBone;
use crate::HumanPack;

/// The four fingers and the thumb, as the rig names them.
const FINGERS: [&str; 5] = ["thumb", "index", "middle", "ring", "pinky"];

/// How far each segment of a finger turns from its rest bend to a closed
/// fist, in degrees: knuckle, middle joint, tip. The thumb folds less, and
/// mostly at its last two joints.
const FIST_DEG: [[f32; 3]; 5] = [
    [10.0, 25.0, 35.0],
    [65.0, 80.0, 50.0],
    [65.0, 80.0, 50.0],
    [65.0, 80.0, 50.0],
    [65.0, 80.0, 50.0],
];

/// A relaxed hand curls more toward the little finger; so does a loose fist.
const FINGER_CURL: [f32; 5] = [1.0, 0.85, 1.0, 1.1, 1.2];

/// How a clip holds its hands: 0 is the rest pose's relaxed hand, 1 a fist,
/// below 0 opening toward flat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grip {
    /// The curl the hand holds.
    pub base: f32,
    /// How much it breathes about that, once per `cycles` of the clip.
    pub swing: f32,
    pub cycles: f32,
    /// A grasp: the curl rises by `.0` around phase `.1`.
    pub grasp: Option<(f32, f32)>,
}

impl Grip {
    const fn held(base: f32, swing: f32) -> Self {
        Self {
            base,
            swing,
            cycles: 1.0,
            grasp: None,
        }
    }
}

/// The grip for a clip, by its name in the pack (`human-pack`'s cuts).
#[must_use]
pub fn grip_for(clip: &str) -> Grip {
    match clip {
        // Loose, moving a little with the arm's swing.
        "idle" => Grip::held(0.2, 0.04),
        "walk" => Grip::held(0.3, 0.1),
        "run" => Grip::held(0.6, 0.08),
        "punch" => Grip::held(1.0, 0.0),
        "kick" | "roll" => Grip::held(0.55, 0.05),
        "faint" => Grip::held(0.1, 0.0),
        "sit" => Grip::held(0.3, 0.03),
        // An open hand, fingers working a little.
        "wave" => Grip {
            base: -0.2,
            swing: 0.08,
            cycles: 2.0,
            grasp: None,
        },
        // The reach opens the hand and closes it on what it reaches for.
        "interact" => Grip {
            base: -0.1,
            swing: 0.0,
            cycles: 1.0,
            grasp: Some((0.7, 0.55)),
        },
        _ => Grip::held(0.25, 0.0),
    }
}

/// The curl at `phase` (0..1 through the clip) for a finger `lag` behind the
/// first — a wave of a hand runs across its fingers, not through all at once —
/// on the side `side` (0 left, 1 right; they swing in opposition, as the arms
/// do).
#[must_use]
pub fn curl_at(grip: Grip, phase: f32, lag: f32, side: f32) -> f32 {
    use std::f32::consts::TAU;
    let t = phase - lag;
    let swing = grip.swing * (TAU * (grip.cycles * t + 0.5 * side)).sin();
    let grasp = grip.grasp.map_or(0.0, |(amount, at)| {
        // A smooth bump about a fifth of the clip wide.
        let d = (t - at) / 0.18;
        amount * (-d * d).exp()
    });
    grip.base + swing + grasp
}

/// One finger joint the hand pose turns: its bone, the axis it bends about in
/// its own frame, and its turn at a full fist (radians).
#[derive(Clone, Copy, Debug)]
pub struct FingerJoint {
    pub bone: usize,
    pub axis: Vec3,
    pub fist: f32,
    /// Which finger, thumb first ([`FINGERS`]), and which hand (0 left).
    pub finger: usize,
    pub side: usize,
}

/// Every finger joint of `pack`'s rig the hand pose drives, with the axis it
/// bends about read off `rest`.
#[must_use]
pub fn finger_joints(pack: &HumanPack, rest: &[RestBone]) -> Vec<FingerJoint> {
    let mut out = Vec::new();
    for (side, s) in ["l", "r"].iter().enumerate() {
        let bones = |finger: &str| -> Option<[usize; 3]> {
            let b = |k: u32| pack.bone(&format!("{finger}_0{k}_{s}"));
            Some([b(1)?, b(2)?, b(3)?])
        };
        // Each finger's own bend plane, where it has one.
        let mut axes: [Option<Vec3>; 5] = [None; 5];
        for (f, name) in FINGERS.iter().enumerate() {
            let Some(chain) = bones(name) else { continue };
            let seg = chain.map(|i| (rest[i].tail - rest[i].head).normalize_or_zero());
            let bend = seg[0].cross(seg[1]) + seg[1].cross(seg[2]);
            // A few degrees of bend is a plane; less is noise.
            if bend.length() > 0.05 {
                axes[f] = Some(bend.normalize());
            }
        }
        // A straight finger bends the way the others do (the thumb excepted:
        // it opposes them, so it neither lends nor borrows).
        let shared = axes[1..]
            .iter()
            .flatten()
            .copied()
            .reduce(|a, b| a + b)
            .and_then(|v| v.try_normalize());
        for (f, name) in FINGERS.iter().enumerate() {
            let Some(chain) = bones(name) else { continue };
            let Some(axis) = axes[f].or(if f == 0 { None } else { shared }) else {
                continue;
            };
            for (k, &bone) in chain.iter().enumerate() {
                out.push(FingerJoint {
                    bone,
                    axis: (rest[bone].rotation.inverse() * axis).normalize(),
                    fist: FIST_DEG[f][k].to_radians(),
                    finger: f,
                    side,
                });
            }
        }
    }
    out
}

/// The turn a finger joint takes, on top of its rest, for `curl`.
#[must_use]
pub fn joint_turn(joint: &FingerJoint, curl: f32) -> Quat {
    let c = (curl * FINGER_CURL[joint.finger]).clamp(-0.25, 1.05);
    Quat::from_axis_angle(joint.axis, c * joint.fist)
}

/// How far a hand holding a phone is closed: the fingers round its far edge,
/// short of a fist. At 0.5 they stood out past the edge.
pub const PHONE_CURL: f32 = 0.75;

/// A hand holding a phone, as the client poses it over a clip (`reach`).
#[derive(Clone, Debug, PartialEq)]
pub struct PhoneGrip {
    /// The hand bone.
    pub hand: usize,
    /// Each finger joint's local rotation, closed round the phone.
    pub fingers: Vec<(usize, Quat)>,
    /// From the wrist toward the middle knuckle, in the hand bone's frame.
    pub towards_fingers: Vec3,
    /// The middle knuckle from the wrist, metres, in the hand bone's frame:
    /// what places the hand on the phone.
    pub to_knuckle: Vec3,
    /// The way the palm faces, square to `towards_fingers`, in the hand
    /// bone's frame. Read off which way the fingers curl, like the bend axes,
    /// so no bone roll had to be known.
    pub palm: Vec3,
}

/// The phone grip of each hand, left first; `None` for a hand the rig has no
/// middle finger on.
#[must_use]
pub fn phone_grip(pack: &HumanPack, rest: &[RestBone]) -> [Option<PhoneGrip>; 2] {
    let joints = finger_joints(pack, rest);
    ["l", "r"]
        .iter()
        .enumerate()
        .map(|(side, s)| {
            let hand = pack.bone(&format!("hand_{s}"))?;
            let knuckle = pack.bone(&format!("middle_01_{s}"))?;
            let bend = joints.iter().find(|j| j.bone == knuckle)?;
            let frame = rest[hand].rotation.inverse();
            let towards = (rest[knuckle].head - rest[hand].head).try_normalize()?;
            // Where the middle finger's tip goes as it closes is the palm side.
            let seg = rest[knuckle].tail - rest[knuckle].head;
            let axis = rest[knuckle].rotation * bend.axis;
            let closed = Quat::from_axis_angle(axis, bend.fist) * seg;
            let palm = closed - seg;
            let palm = (palm - towards * palm.dot(towards)).try_normalize()?;
            let fingers = joints
                .iter()
                .filter(|j| j.side == side)
                .map(|j| {
                    let rest_local = match pack.bones[j.bone].parent {
                        Some(p) => rest[usize::from(p)].rotation.inverse() * rest[j.bone].rotation,
                        None => rest[j.bone].rotation,
                    };
                    (j.bone, (rest_local * joint_turn(j, PHONE_CURL)).normalize())
                })
                .collect();
            Some(PhoneGrip {
                hand,
                fingers,
                towards_fingers: frame * towards,
                to_knuckle: frame * (rest[knuckle].head - rest[hand].head),
                palm: frame * palm,
            })
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap_or([None, None])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grip_holds_its_base_and_a_grasp_closes_mid_reach() {
        let walk = grip_for("walk");
        let mean: f32 = (0..100)
            .map(|i| curl_at(walk, i as f32 / 100.0, 0.0, 0.0))
            .sum::<f32>()
            / 100.0;
        assert!((mean - walk.base).abs() < 0.01, "{mean}");
        // The two hands swing in opposition.
        assert!((curl_at(walk, 0.25, 0.0, 0.0) - curl_at(walk, 0.25, 0.0, 1.0)).abs() > 0.15);
        let reach = grip_for("interact");
        assert!(curl_at(reach, 0.55, 0.0, 0.0) > curl_at(reach, 0.1, 0.0, 0.0) + 0.5);
        assert_eq!(grip_for("punch").base, 1.0);
    }
}
