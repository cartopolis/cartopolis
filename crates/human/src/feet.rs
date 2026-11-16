//! The feet stood on the ground they are over: two-bone leg IK on top of a
//! clip (`docs/notes/foot-placement.md`).
//!
//! Every clip is captured on a flat floor and stood on its soles
//! (`build::plant_feet`), and the avatar is stood on one ground height, the
//! one under its origin. On a slope that puts the foot ahead in the ground
//! and the one behind over it, by half the stride times the gradient. Here
//! each foot is moved by the difference between the ground under it and the
//! ground the body stands on: the pelvis comes down by what the lower foot
//! needs (a leg cannot reach further than its length), each leg is solved so
//! its ankle lands where its foot belongs, and the knee keeps the side the
//! clip bent it to.
//!
//! Everything is in the body's holder frame — metres, +y up, the soles at
//! y = 0 on flat ground, +z ahead — which is the world's axes turned about
//! the vertical, so a height difference in the world is one here.

use glam::{Quat, Vec3};

use crate::reach::{reach_from_pose, Arm};

/// The lowest the pelvis is brought down for a foot below the body, metres:
/// a deep step down with the other foot still up. Beyond it the lower foot
/// stops short of its ground rather than folding the standing leg.
pub const MAX_DROP_M: f32 = 0.3;

/// The furthest a foot is moved off the clip's floor, either way, metres. A
/// difference larger than this under one foot is not a slope but an edge (a
/// quay wall, a wall the field half-resolves), and is left to the clip.
pub const MAX_STEP_M: f32 = 0.35;

/// The steepest the foot is tilted to the ground under it, radians (~25°):
/// an ankle bends that far on a dyke's grass and no further.
pub const MAX_TILT: f32 = 0.44;

/// Below this the ground under a foot counts as the floor the clip was
/// captured on, metres: nothing is solved, so on flat ground the pose is the
/// clip's exactly.
pub const FLAT_M: f32 = 0.001;

/// One leg as the clip has it this frame, in the holder's frame.
#[derive(Clone, Copy, Debug)]
pub struct Leg {
    pub hip: Vec3,
    pub knee: Vec3,
    pub ankle: Vec3,
}

/// The two turns that put a leg's ankle on its ground, in the holder's
/// frame: `thigh` about the hip, then `calf` about the moved knee — as
/// [`crate::reach::Reach`], whose solution it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LegTurn {
    pub thigh: Quat,
    pub calf: Quat,
    /// The ankle reached its target (false: out of reach, the leg points at it).
    pub reached: bool,
}

impl LegTurn {
    pub const NONE: Self = Self {
        thigh: Quat::IDENTITY,
        calf: Quat::IDENTITY,
        reached: true,
    };
}

/// A frame's answer for both legs: how far the pelvis comes down, and each
/// leg's turns (left first).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeetPose {
    pub drop: f32,
    pub legs: [LegTurn; 2],
}

impl FeetPose {
    pub const NONE: Self = Self {
        drop: 0.0,
        legs: [LegTurn::NONE; 2],
    };
}

/// How far the pelvis comes down so each leg reaches its foot's ground:
/// `rise` is how far each ankle goes over where the clip put it, `stance`
/// how much each foot bears weight (1 planted, 0 in the air).
///
/// A planted foot keeps the bend the clip gave its leg — the pelvis comes
/// down until that leg is no longer from hip to ankle than the clip had it —
/// and a foot in the air asks for nothing: it stops short of lower ground
/// rather than pulling the body down to it. Taking the lower foot's whole
/// difference instead dropped the body 20 cm on a dyke whenever the
/// swinging foot behind was over ground lower down the slope: a lunge at
/// every stride (2026-11-16). Never up.
#[must_use]
pub fn pelvis_drop(legs: &[Leg; 2], rise: [f32; 2], stance: [f32; 2]) -> f32 {
    let mut drop = 0.0f32;
    for ((leg, rise), stance) in legs.iter().zip(rise).zip(stance) {
        let target = leg.ankle + Vec3::Y * rise;
        let reach = leg.hip.distance(leg.ankle);
        let across = (target - leg.hip).with_y(0.0).length();
        let below = (reach * reach - across * across).max(0.0).sqrt();
        drop = drop.max((leg.hip.y - target.y - below) * stance.clamp(0.0, 1.0));
    }
    // Rounding on flat ground is not a drop.
    if drop < FLAT_M {
        return 0.0;
    }
    drop.min(MAX_DROP_M)
}

/// A ground difference clamped to what a foot follows.
#[must_use]
pub fn step(ground: f32) -> f32 {
    ground.clamp(-MAX_STEP_M, MAX_STEP_M)
}

/// Solve one leg, already lowered with the pelvis by `drop`, so its ankle
/// lands `rise` over where the clip put it. `forward` is the body's ahead
/// (the knee's side when the clip holds the leg straight).
#[must_use]
pub fn plant_leg(leg: &Leg, drop: f32, rise: f32, forward: Vec3) -> LegTurn {
    if drop.abs() < FLAT_M && rise.abs() < FLAT_M {
        return LegTurn::NONE;
    }
    let down = Vec3::Y * drop;
    let arm = Arm {
        shoulder: leg.hip - down,
        elbow: leg.knee - down,
        wrist: leg.ankle - down,
    };
    let target = leg.ankle + Vec3::Y * rise;
    let r = reach_from_pose(&arm, target, knee_side(leg, forward));
    LegTurn {
        thigh: r.upper,
        calf: r.lower,
        reached: r.reached,
    }
}

/// Which way the knee bends: the side of the hip–ankle line the clip has it
/// on, or ahead where the clip holds the leg straight or bent backwards — a
/// knee never bends back.
#[must_use]
pub fn knee_side(leg: &Leg, forward: Vec3) -> Vec3 {
    let line = leg.ankle - leg.hip;
    let off = match line.try_normalize() {
        Some(dir) => {
            let k = leg.knee - leg.hip;
            k - dir * k.dot(dir)
        }
        None => Vec3::ZERO,
    };
    if off.length() < 0.01 || off.dot(forward) < 0.0 {
        forward
    } else {
        off
    }
}

/// Both legs for one frame: `legs` as the clip has them, `ground` the ground
/// under each foot less the ground the body stands on, `stance` how much
/// each bears weight, `weight` how far in the placement is (0 the clip, 1
/// fully placed).
#[must_use]
pub fn solve_feet(
    legs: &[Leg; 2],
    ground: [f32; 2],
    stance: [f32; 2],
    weight: f32,
    forward: Vec3,
) -> FeetPose {
    let w = weight.clamp(0.0, 1.0);
    let g = [step(ground[0]) * w, step(ground[1]) * w];
    let drop = pelvis_drop(legs, g, stance);
    FeetPose {
        drop,
        legs: [
            plant_leg(&legs[0], drop, g[0], forward),
            plant_leg(&legs[1], drop, g[1], forward),
        ],
    }
}

/// The turn that lays a foot flat on ground whose up is `normal` (holder
/// frame), at most [`MAX_TILT`], scaled by `weight`.
#[must_use]
pub fn foot_tilt(normal: Vec3, weight: f32) -> Quat {
    let Some(n) = normal.try_normalize() else {
        return Quat::IDENTITY;
    };
    let angle = n.dot(Vec3::Y).clamp(-1.0, 1.0).acos();
    if angle < 1e-4 {
        return Quat::IDENTITY;
    }
    let Some(axis) = Vec3::Y.cross(n).try_normalize() else {
        return Quat::IDENTITY;
    };
    Quat::from_axis_angle(axis, angle.min(MAX_TILT) * weight.clamp(0.0, 1.0))
}

/// The ground's up from heights sampled `half` metres either side of a point
/// along x and along z.
#[must_use]
pub fn ground_normal(x: [f32; 2], z: [f32; 2], half: f32) -> Vec3 {
    let span = 2.0 * half;
    Vec3::new(-(x[1] - x[0]) / span, 1.0, -(z[1] - z[0]) / span).normalize()
}

/// When the feet are placed at all: on the ground, upright and on its feet.
/// Everything else is a pose the clip owns — a swim, a seat, a climb, an
/// emote, the air — or a floor the ground field knows nothing of.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Footing {
    pub swimming: bool,
    pub seated: bool,
    pub climbing: bool,
    pub emoting: bool,
    /// Off the ground: a jump, a fall.
    pub airborne: bool,
    /// The soles' height over the ground field at the origin, metres: a
    /// floor, a wall top or a roof shows as a difference here.
    pub over_ground: f32,
}

/// How far over (or under) the ground field the soles may be and still be
/// standing on it, metres.
pub const ON_GROUND_M: f32 = 0.05;

impl Footing {
    /// Whether the feet are stood on the ground under each of them.
    #[must_use]
    pub fn places_feet(&self) -> bool {
        !(self.swimming || self.seated || self.climbing || self.emoting || self.airborne)
            && self.over_ground.abs() <= ON_GROUND_M
    }
}

/// Seconds for the placement to come in or go out.
pub const BLEND_S: f32 = 0.2;

/// One frame of the placement's weight toward `on`.
#[must_use]
pub fn blend(weight: f32, on: bool, dt: f32) -> f32 {
    let step = dt / BLEND_S;
    if on {
        (weight + step).min(1.0)
    } else {
        (weight - step).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A left leg standing a little bent: hip 0.9 m up, the knee 4 cm ahead
    /// of the line, the ankle 8 cm over the soles. Thigh 0.43 m, shin 0.41.
    fn leg() -> Leg {
        Leg {
            hip: Vec3::new(0.1, 0.9, 0.0),
            knee: Vec3::new(0.1, 0.48, 0.04),
            ankle: Vec3::new(0.1, 0.08, 0.0),
        }
    }

    const PLANTED: [f32; 2] = [1.0, 1.0];

    fn apply(l: &Leg, drop: f32, t: &LegTurn) -> Leg {
        let down = Vec3::Y * drop;
        let hip = l.hip - down;
        let knee = hip + t.thigh * (l.knee - l.hip);
        let ankle = knee + t.calf * t.thigh * (l.ankle - l.knee);
        Leg { hip, knee, ankle }
    }

    #[test]
    fn flat_ground_leaves_the_clip_alone() {
        let legs = [leg(), leg()];
        let p = solve_feet(&legs, [0.0, 0.0005], PLANTED, 1.0, Vec3::Z);
        assert_eq!(p, FeetPose::NONE);
        // Just past the threshold the ankle goes exactly where it is asked.
        let p = solve_feet(&legs, [0.002, 0.0], PLANTED, 1.0, Vec3::Z);
        let a = apply(&legs[0], p.drop, &p.legs[0]);
        let want = leg().ankle + Vec3::Y * 0.002;
        assert!(a.ankle.distance(want) < 1e-4, "{}", a.ankle);
        // The knee comes forward for it (a nearly straight leg amplifies), and only that.
        assert!(
            a.knee.distance(leg().knee) < 0.015 && a.knee.z >= leg().knee.z,
            "{}",
            a.knee
        );
    }

    #[test]
    fn a_straight_leg_on_flat_ground_is_not_pulled_in() {
        let straight = Leg {
            hip: Vec3::new(0.1, 0.92, 0.0),
            knee: Vec3::new(0.1, 0.5, 0.0005),
            ankle: Vec3::new(0.1, 0.08, 0.0),
        };
        let t = plant_leg(&straight, 0.0, 0.002, Vec3::Z);
        let a = apply(&straight, 0.0, &t);
        assert!((a.ankle.y - 0.082).abs() < 0.001, "{}", a.ankle);
        assert!(a.knee.z > 0.0, "the knee bends forward: {}", a.knee);
    }

    #[test]
    fn each_foot_lands_on_its_own_ground_and_the_pelvis_comes_down_for_the_lower() {
        // On a slope: the left foot ahead 6 cm up, the right behind 6 cm down.
        let mut ahead = leg();
        ahead.ankle.z = 0.2;
        ahead.knee.z = 0.15;
        let mut behind = leg();
        behind.hip.x = -0.1;
        behind.knee = Vec3::new(-0.1, 0.48, -0.05);
        behind.ankle = Vec3::new(-0.1, 0.08, -0.2);
        let legs = [ahead, behind];
        let p = solve_feet(&legs, [0.06, -0.06], PLANTED, 1.0, Vec3::Z);
        assert!((p.drop - 0.06).abs() < 1e-6);
        for (i, rise) in [0.06, -0.06].into_iter().enumerate() {
            let a = apply(&legs[i], p.drop, &p.legs[i]);
            let want = legs[i].ankle + Vec3::Y * rise;
            assert!(p.legs[i].reached);
            assert!(
                a.ankle.distance(want) < 1e-3,
                "leg {i}: {} vs {want}",
                a.ankle
            );
            // Bones keep their length.
            assert!((a.knee.distance(a.hip) - legs[i].knee.distance(legs[i].hip)).abs() < 1e-4);
            assert!((a.ankle.distance(a.knee) - legs[i].ankle.distance(legs[i].knee)).abs() < 1e-4);
        }
    }

    #[test]
    fn the_knee_bends_forward() {
        let l = leg();
        // A foot 20 cm up: a deep bend, and the knee goes ahead of the line.
        let t = plant_leg(&l, 0.0, 0.2, Vec3::Z);
        let a = apply(&l, 0.0, &t);
        assert!(t.reached);
        assert!(a.knee.z > 0.1, "{}", a.knee);
        // A clip knee bent backwards is solved forward all the same.
        let mut back = l;
        back.knee.z = -0.03;
        let t = plant_leg(&back, 0.0, 0.2, Vec3::Z);
        assert!(apply(&back, 0.0, &t).knee.z > 0.1);
    }

    #[test]
    fn a_target_out_of_reach_is_pointed_at_and_the_drop_is_bounded() {
        let l = leg();
        // A foot asked 0.6 m down: clamped to the step, the pelvis to its drop.
        let p = solve_feet(&[l, l], [-0.6, 0.0], PLANTED, 1.0, Vec3::Z);
        assert!((p.drop - MAX_DROP_M).abs() < 1e-6);
        let a = apply(&l, p.drop, &p.legs[0]);
        assert!(!p.legs[0].reached);
        // The straightened leg points at its target and stops short of it.
        let want = l.ankle - Vec3::Y * MAX_STEP_M;
        assert!(a.ankle.y > want.y && a.ankle.is_finite());
        assert!((a.ankle.x - want.x).abs() < 1e-3 && (a.ankle.z - want.z).abs() < 1e-3);
        // And a foot asked through the hip folds as far as it goes, finite.
        let t = plant_leg(&l, 0.0, 0.79, Vec3::Z);
        assert!(!t.reached);
        assert!(apply(&l, 0.0, &t).ankle.is_finite());
    }

    #[test]
    fn half_weight_is_half_the_placement() {
        let l = leg();
        let p = solve_feet(&[l, l], [-0.1, 0.0], PLANTED, 0.5, Vec3::Z);
        assert!((p.drop - 0.05).abs() < 1e-6);
        assert_eq!(
            solve_feet(&[l, l], [-0.1, 0.1], PLANTED, 0.0, Vec3::Z),
            FeetPose::NONE
        );
    }

    #[test]
    fn a_foot_in_the_air_over_lower_ground_does_not_bring_the_body_down() {
        // Mid-stride up a slope: the planted foot under the body, the one
        // behind lifted and bent, 15 cm over ground lower down.
        let planted = leg();
        let swinging = Leg {
            hip: Vec3::new(-0.1, 0.9, 0.0),
            knee: Vec3::new(-0.1, 0.5, -0.12),
            ankle: Vec3::new(-0.1, 0.2, -0.35),
        };
        let p = solve_feet(&[planted, swinging], [0.0, -0.15], [1.0, 0.0], 1.0, Vec3::Z);
        assert_eq!(p.drop, 0.0, "the swinging leg does not pull the body down");
        // It reaches toward its ground as far as it goes.
        let a = apply(&swinging, p.drop, &p.legs[1]);
        assert!(a.ankle.y < swinging.ankle.y - 0.02, "{}", a.ankle);
        // Planted on that lower ground instead, it does.
        let p = solve_feet(&[planted, swinging], [0.0, -0.15], [1.0, 1.0], 1.0, Vec3::Z);
        assert!(p.drop > 0.1, "{}", p.drop);
    }

    #[test]
    fn the_foot_tilts_to_the_slope_and_no_further() {
        assert_eq!(foot_tilt(Vec3::Y, 1.0), Quat::IDENTITY);
        // A 1:3 slope rising ahead (+z): the toe comes up.
        let n = ground_normal([0.0, 0.0], [-0.1, 0.1], 0.3);
        let t = foot_tilt(n, 1.0);
        let toe = t * Vec3::Z;
        assert!((toe.y - (1.0f32 / 3.0).atan().sin()).abs() < 1e-4, "{toe}");
        // A wall-steep one stops at MAX_TILT.
        let steep = foot_tilt(Vec3::new(0.0, 1.0, -5.0), 1.0);
        assert!(((steep * Vec3::Z).y.asin() - MAX_TILT).abs() < 1e-4);
    }

    #[test]
    fn feet_are_placed_only_standing_or_walking_on_the_ground() {
        let on = Footing::default();
        assert!(on.places_feet());
        assert!(Footing {
            over_ground: 0.04,
            ..on
        }
        .places_feet());
        for off in [
            Footing {
                swimming: true,
                ..on
            },
            Footing { seated: true, ..on },
            Footing {
                climbing: true,
                ..on
            },
            Footing {
                emoting: true,
                ..on
            },
            Footing {
                airborne: true,
                ..on
            },
            // On a floor or a wall top, which the ground field does not hold.
            Footing {
                over_ground: 0.4,
                ..on
            },
            Footing {
                over_ground: -0.2,
                ..on
            },
        ] {
            assert!(!off.places_feet(), "{off:?}");
        }
    }

    #[test]
    fn the_weight_eases_in_and_out() {
        let mut w = 0.0;
        for _ in 0..5 {
            w = blend(w, true, 0.02);
        }
        assert!((w - 0.5).abs() < 1e-5);
        for _ in 0..20 {
            w = blend(w, true, 0.02);
        }
        assert_eq!(w, 1.0);
        assert_eq!(blend(0.05, false, 0.1), 0.0);
    }
}
