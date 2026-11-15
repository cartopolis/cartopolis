//! What an avatar can pick up, and how far it reaches to do it (protocol 48,
//! `docs/notes/physical-prims.md`).
//!
//! One rule for both ends: the client offers "Pick up" only for what the sim
//! would grant, and the sim decides. Mass is the physics step's own — a prim
//! weighs what it collides as, at [`DENSITY_KG_M3`] — so a thing the kick
//! barely moves is also a thing nobody lifts.

use glam::Vec3;

/// Kilograms per cubic metre every physical prim is made of. A football (22 cm
/// across) comes out at 0.56 kg, near the real 0.43; a one-metre crate at
/// 100 kg.
pub const DENSITY_KG_M3: f32 = 100.0;

/// Heaviest thing one avatar carries, in kilograms: a full crate of beer, a
/// 0.5 m box of this world's material. A football is 0.56 kg; a 1 m crate,
/// 100 kg, stays on the ground.
pub const MAX_CARRY_KG: f32 = 15.0;

/// Longest side of anything carried, in metres. Light but long — a 3 m plank,
/// a goal's crossbar made of thin prims — is not held in front of the chest
/// either.
pub const MAX_CARRY_SIZE_M: f32 = 1.2;

/// How far from where the sim holds the avatar a thing may be picked up from,
/// measured flat from the feet to the prim's centre. Two short paces: the
/// client's prompt appears inside it, and a stale client a tick behind is
/// still inside it.
pub const GRAB_REACH_M: f32 = 2.5;

/// How far above the feet (and below them) the centre of a thing may be and
/// still be reached: from the ground to just over the head.
pub const GRAB_ABOVE_M: f32 = 2.3;
pub const GRAB_BELOW_M: f32 = 0.5;

/// A prim's size as the physics step uses it: never quite zero.
#[must_use]
pub fn prim_size(scale: Vec3) -> Vec3 {
    scale.abs().max(Vec3::splat(0.01))
}

/// What a prim weighs at [`DENSITY_KG_M3`], from the shape it collides as. A
/// model collides as its bounding box, and so weighs as one.
#[must_use]
pub fn prim_mass_kg(shape: &str, scale: Vec3, is_model: bool) -> f32 {
    let s = prim_size(scale);
    let box_volume = s.x * s.y * s.z;
    let volume = if is_model {
        box_volume
    } else {
        match shape.to_ascii_lowercase().as_str() {
            "sphere" => box_volume * std::f32::consts::PI / 6.0,
            "cylinder" => box_volume * std::f32::consts::PI / 4.0,
            "cone" => box_volume * std::f32::consts::PI / 12.0,
            _ => box_volume,
        }
    };
    volume * DENSITY_KG_M3
}

/// Whether a prim of this shape and scale is small and light enough to carry.
/// Says nothing of whether it is physical, near, or free — the sim asks those.
#[must_use]
pub fn can_carry(shape: &str, scale: Vec3, is_model: bool) -> bool {
    prim_size(scale).max_element() <= MAX_CARRY_SIZE_M
        && prim_mass_kg(shape, scale, is_model) <= MAX_CARRY_KG
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The football is the reason this exists, and the goals beside it must
    /// stay where they are.
    #[test]
    fn a_ball_is_carried_and_a_goal_is_not() {
        assert!(can_carry("sphere", Vec3::splat(0.22), false));
        // A goal post: thin, but 2.4 m tall.
        assert!(!can_carry("box", Vec3::new(0.12, 2.44, 0.12), false));
        // A 1 m crate is 100 kg.
        assert!(!can_carry("box", Vec3::ONE, false));
        // Half a metre of crate is 12.5 kg: heavy, but carried.
        assert!(can_carry("box", Vec3::splat(0.5), false));
        // The same half-metre as a model is its box, and so the same.
        assert!(can_carry("anything", Vec3::splat(0.5), true));
        // A sphere 0.65 m across is 14.4 kg; 0.7 m is 18 kg.
        assert!(can_carry("sphere", Vec3::splat(0.65), false));
        assert!(!can_carry("sphere", Vec3::splat(0.7), false));
    }

    #[test]
    fn a_mirrored_scale_weighs_the_same() {
        let a = prim_mass_kg("box", Vec3::new(0.3, 0.3, 0.3), false);
        let b = prim_mass_kg("box", Vec3::new(-0.3, 0.3, 0.3), false);
        assert!((a - b).abs() < 1e-6);
        assert!((a - 2.7).abs() < 1e-3, "{a}");
    }
}
