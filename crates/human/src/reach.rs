//! An arm reaching for a point: the upper arm and the forearm turned so the
//! wrist lands on it, the elbow bent toward a side the caller picks.
//!
//! What it is for: the phone in a hand (`docs/notes/hands-and-tones.md`). In
//! the high-precision mode the phone's pose is measured to the centimetre, and
//! the avatar's hand is to be where the person's hand is — while the rest of
//! the body plays its clip. No capture can say where that hand is, so the arm
//! is solved per frame from the clip's own pose, the way `hands` makes the
//! fingers no capture records.
//!
//! Two bones have a closed form: the triangle shoulder–elbow–wrist has known
//! sides, so the elbow's angle follows from the distance to reach, and the one
//! free choice — how far round the shoulder–target line the elbow sits — is
//! the caller's `elbow_towards`. Everything is in one frame of the caller's
//! (the model's, usually), and the answer is two turns about the joints in
//! that frame, so it works on any parent chain.

use glam::{Quat, Vec3};

/// An arm's three joints, where the clip has them this frame.
#[derive(Clone, Copy, Debug)]
pub struct Arm {
    pub shoulder: Vec3,
    pub elbow: Vec3,
    pub wrist: Vec3,
}

/// The two turns that make an [`Arm`] reach, in the frame its joints were
/// given in: `upper` about the shoulder, then `lower` about the moved elbow.
/// A bone's new rotation in that frame is the turn times its old one —
/// `upper * shoulder`, and `lower * upper * elbow` for the forearm.
#[derive(Clone, Copy, Debug)]
pub struct Reach {
    pub upper: Quat,
    pub lower: Quat,
    /// Whether the wrist is on the target: false when it was out of reach
    /// (the arm points at it) or inside the closest the elbow folds.
    pub reached: bool,
}

/// How straight the arm may get, as a share of its full length. A dead
/// straight elbow has no bend direction, and an arm that flicks between
/// almost straight and straight flicks its elbow round with it.
const STRAIGHTEST: f32 = 0.995;

/// The nearest the wrist comes to the shoulder, as a share of the longer
/// bone: a hand cannot be folded through the upper arm.
const FOLDED: f32 = 0.3;

/// Turn `arm` so its wrist reaches `target`, its elbow bending toward
/// `elbow_towards` (a direction: for a hand held in front of the chest, down
/// and out to the side). Out of reach, the arm points at the target.
#[must_use]
pub fn reach(arm: &Arm, target: Vec3, elbow_towards: Vec3) -> Reach {
    let upper_len = arm.elbow.distance(arm.shoulder);
    let lower_len = arm.wrist.distance(arm.elbow);
    let to = target - arm.shoulder;
    let (Some(dir), true) = (to.try_normalize(), upper_len > 0.0 && lower_len > 0.0) else {
        return Reach {
            upper: Quat::IDENTITY,
            lower: Quat::IDENTITY,
            reached: false,
        };
    };
    let longest = (upper_len + lower_len) * STRAIGHTEST;
    let nearest = ((upper_len - lower_len).abs()).max(FOLDED * upper_len.max(lower_len));
    let wanted = to.length();
    let d = wanted.clamp(nearest, longest);

    // The bend: `elbow_towards` less its part along the reach. A hint along
    // the reach itself says nothing, so the elbow keeps the side it was on.
    let side = |v: Vec3| (v - dir * v.dot(dir)).try_normalize();
    let bend = side(elbow_towards)
        .or_else(|| side(arm.elbow - arm.shoulder))
        .unwrap_or_else(|| dir.any_orthonormal_vector());

    // The shoulder's angle in the triangle of sides upper, lower and d.
    let cos = ((upper_len * upper_len + d * d - lower_len * lower_len) / (2.0 * upper_len * d))
        .clamp(-1.0, 1.0);
    let sin = (1.0 - cos * cos).sqrt();
    let elbow = arm.shoulder + upper_len * (cos * dir + sin * bend);

    let upper = Quat::from_rotation_arc(
        (arm.elbow - arm.shoulder) / upper_len,
        (elbow - arm.shoulder) / upper_len,
    );
    let wrist_turned = arm.shoulder + upper * (arm.wrist - arm.shoulder);
    let lower = Quat::from_rotation_arc(
        (wrist_turned - elbow) / lower_len,
        (arm.shoulder + dir * d - elbow) / lower_len,
    );
    Reach {
        upper,
        lower,
        reached: (wanted - d).abs() < 1e-4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A right arm held out sideways: shoulder at the origin, the elbow 30 cm
    /// along −x, the wrist 26 cm beyond it.
    fn arm() -> Arm {
        Arm {
            shoulder: Vec3::ZERO,
            elbow: Vec3::new(-0.30, 0.0, 0.0),
            wrist: Vec3::new(-0.56, 0.0, 0.0),
        }
    }

    /// Where the joints are after `r`, as a caller applying it would put them.
    fn apply(a: &Arm, r: &Reach) -> Arm {
        let elbow = a.shoulder + r.upper * (a.elbow - a.shoulder);
        let wrist = elbow + r.lower * r.upper * (a.wrist - a.elbow);
        Arm {
            shoulder: a.shoulder,
            elbow,
            wrist,
        }
    }

    #[test]
    fn the_wrist_lands_on_a_point_in_reach_and_the_bones_keep_their_length() {
        let a = arm();
        // A phone held in front of the chest: forward (+z), a little down and
        // in toward the middle.
        let target = Vec3::new(-0.12, -0.2, 0.35);
        let r = reach(&a, target, Vec3::new(-1.0, -1.0, 0.0));
        assert!(r.reached);
        let b = apply(&a, &r);
        assert!(b.wrist.distance(target) < 1e-4, "{}", b.wrist);
        assert!((b.elbow.distance(b.shoulder) - 0.30).abs() < 1e-5);
        assert!((b.wrist.distance(b.elbow) - 0.26).abs() < 1e-5);
        // The elbow bent the way it was told: down and out, not up and in.
        assert!(b.elbow.y < -0.05 && b.elbow.x < -0.1, "{}", b.elbow);
    }

    #[test]
    fn out_of_reach_the_arm_points_at_the_target() {
        let a = arm();
        let target = Vec3::new(0.0, 0.0, 2.0);
        let r = reach(&a, target, Vec3::NEG_Y);
        assert!(!r.reached);
        let b = apply(&a, &r);
        let along = b.wrist.normalize();
        assert!(along.dot(Vec3::Z) > 0.9999, "{along}");
        assert!((b.wrist.length() - 0.56 * STRAIGHTEST).abs() < 1e-4);
    }

    #[test]
    fn a_target_at_the_shoulder_folds_the_arm_as_far_as_it_goes() {
        let a = arm();
        let r = reach(&a, Vec3::new(0.0, 0.01, 0.02), Vec3::NEG_Y);
        assert!(!r.reached);
        let b = apply(&a, &r);
        assert!(
            (b.wrist.length() - FOLDED * 0.30).abs() < 1e-3,
            "{}",
            b.wrist.length()
        );
        assert!(b.wrist.is_finite() && b.elbow.is_finite());
    }

    #[test]
    fn a_bend_hint_along_the_reach_keeps_the_elbow_where_it_was() {
        // The elbow starts below the line; a hint pointing straight at the
        // target gives no side, so it stays below.
        let a = Arm {
            shoulder: Vec3::ZERO,
            elbow: Vec3::new(-0.2, -0.2, 0.0),
            wrist: Vec3::new(-0.3, -0.1, 0.2),
        };
        let target = Vec3::new(-0.3, 0.0, 0.3);
        let r = reach(&a, target, target);
        let b = apply(&a, &r);
        assert!(r.reached);
        assert!(b.wrist.distance(target) < 1e-4);
        assert!(b.elbow.y < 0.0, "{}", b.elbow);
    }
}
