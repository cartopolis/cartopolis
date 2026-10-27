//! The face's live shapes: 52 units, blended on the GPU at any weight.
//!
//! Everything else about a body is baked into its vertices when it is built
//! ([`crate::build`]); a face that smiles, blinks and follows its owner's own
//! face cannot be, because it changes every frame. So the pack carries the
//! face's units as ordinary shape targets on the base mesh
//! ([`HumanPack::face_units`](crate::HumanPack::face_units)), `build` turns
//! them into per-vertex differences on the parts they move, and the client
//! hands those to the engine as morph targets and sets their weights.
//!
//! The units are Apple's ARKit blend shapes, the vocabulary face trackers
//! speak (MediaPipe's Face Landmarker gives the same names), as MakeHuman's
//! community made them for its base mesh: the `faceunits01` asset pack, CC0,
//! by Mika Suominen. Left and right are the face's own, as in ARKit.

/// The units, in the order the pack stores them and the weights are given:
/// ARKit's names, sorted. MediaPipe's blend-shape output is the same list
/// after its leading `_neutral`, less `tongueOut`.
pub const UNITS: [&str; 52] = [
    "browDownLeft",
    "browDownRight",
    "browInnerUp",
    "browOuterUpLeft",
    "browOuterUpRight",
    "cheekPuff",
    "cheekSquintLeft",
    "cheekSquintRight",
    "eyeBlinkLeft",
    "eyeBlinkRight",
    "eyeLookDownLeft",
    "eyeLookDownRight",
    "eyeLookInLeft",
    "eyeLookInRight",
    "eyeLookOutLeft",
    "eyeLookOutRight",
    "eyeLookUpLeft",
    "eyeLookUpRight",
    "eyeSquintLeft",
    "eyeSquintRight",
    "eyeWideLeft",
    "eyeWideRight",
    "jawForward",
    "jawLeft",
    "jawOpen",
    "jawRight",
    "mouthClose",
    "mouthDimpleLeft",
    "mouthDimpleRight",
    "mouthFrownLeft",
    "mouthFrownRight",
    "mouthFunnel",
    "mouthLeft",
    "mouthLowerDownLeft",
    "mouthLowerDownRight",
    "mouthPressLeft",
    "mouthPressRight",
    "mouthPucker",
    "mouthRight",
    "mouthRollLower",
    "mouthRollUpper",
    "mouthShrugLower",
    "mouthShrugUpper",
    "mouthSmileLeft",
    "mouthSmileRight",
    "mouthStretchLeft",
    "mouthStretchRight",
    "mouthUpperUpLeft",
    "mouthUpperUpRight",
    "noseSneerLeft",
    "noseSneerRight",
    "tongueOut",
];

/// A face unit's resolution in the pack, decimetres: 0.05 mm. The units move
/// a few millimetres (an eye turning, a lid closing), so the body targets'
/// half millimetre would leave a turned eye in steps.
pub const FACE_UNIT_STEP: f32 = 0.0005;

/// A unit's index in [`UNITS`].
#[must_use]
pub fn unit(name: &str) -> Option<usize> {
    UNITS.iter().position(|u| *u == name)
}

/// The units' weights for a named expression, by unit name; `None` for a
/// name with no recipe. Composed by hand from what each expression does to a
/// face (Ekman's descriptions, as FACS action units mapped onto ARKit's), and
/// held under full strength so a face never looks stretched.
#[must_use]
pub fn recipe(name: &str) -> Option<&'static [(&'static str, f32)]> {
    Some(match name {
        "neutral" => &[],
        "happy" => &[
            ("mouthSmileLeft", 0.75),
            ("mouthSmileRight", 0.75),
            ("cheekSquintLeft", 0.45),
            ("cheekSquintRight", 0.45),
            ("eyeSquintLeft", 0.25),
            ("eyeSquintRight", 0.25),
            ("mouthDimpleLeft", 0.2),
            ("mouthDimpleRight", 0.2),
        ],
        "laugh" => &[
            ("mouthSmileLeft", 0.95),
            ("mouthSmileRight", 0.95),
            ("jawOpen", 0.4),
            ("cheekSquintLeft", 0.7),
            ("cheekSquintRight", 0.7),
            ("eyeSquintLeft", 0.55),
            ("eyeSquintRight", 0.55),
            ("mouthUpperUpLeft", 0.3),
            ("mouthUpperUpRight", 0.3),
            ("browInnerUp", 0.2),
        ],
        "sad" => &[
            ("browInnerUp", 0.8),
            ("browDownLeft", 0.15),
            ("browDownRight", 0.15),
            ("mouthFrownLeft", 0.7),
            ("mouthFrownRight", 0.7),
            ("mouthShrugLower", 0.35),
            ("mouthPressLeft", 0.2),
            ("mouthPressRight", 0.2),
            ("eyeLookDownLeft", 0.3),
            ("eyeLookDownRight", 0.3),
            ("eyeSquintLeft", 0.15),
            ("eyeSquintRight", 0.15),
        ],
        "angry" => &[
            ("browDownLeft", 0.9),
            ("browDownRight", 0.9),
            ("eyeSquintLeft", 0.4),
            ("eyeSquintRight", 0.4),
            ("eyeWideLeft", 0.15),
            ("eyeWideRight", 0.15),
            ("noseSneerLeft", 0.45),
            ("noseSneerRight", 0.45),
            ("mouthPressLeft", 0.5),
            ("mouthPressRight", 0.5),
            ("mouthFrownLeft", 0.35),
            ("mouthFrownRight", 0.35),
            ("mouthRollLower", 0.2),
        ],
        "surprised" => &[
            ("browInnerUp", 0.9),
            ("browOuterUpLeft", 0.85),
            ("browOuterUpRight", 0.85),
            ("eyeWideLeft", 0.75),
            ("eyeWideRight", 0.75),
            ("jawOpen", 0.45),
            ("mouthFunnel", 0.2),
        ],
        "afraid" => &[
            ("browInnerUp", 0.9),
            ("browOuterUpLeft", 0.35),
            ("browOuterUpRight", 0.35),
            ("browDownLeft", 0.2),
            ("browDownRight", 0.2),
            ("eyeWideLeft", 0.85),
            ("eyeWideRight", 0.85),
            ("mouthStretchLeft", 0.6),
            ("mouthStretchRight", 0.6),
            ("jawOpen", 0.2),
        ],
        "disgusted" => &[
            ("noseSneerLeft", 0.85),
            ("noseSneerRight", 0.85),
            ("mouthUpperUpLeft", 0.6),
            ("mouthUpperUpRight", 0.6),
            ("browDownLeft", 0.5),
            ("browDownRight", 0.5),
            ("eyeSquintLeft", 0.4),
            ("eyeSquintRight", 0.4),
            ("cheekSquintLeft", 0.3),
            ("cheekSquintRight", 0.3),
            ("mouthFrownLeft", 0.3),
            ("mouthFrownRight", 0.3),
        ],
        "wink" => &[
            ("eyeBlinkLeft", 1.0),
            ("cheekSquintLeft", 0.4),
            ("mouthSmileLeft", 0.55),
            ("mouthSmileRight", 0.2),
        ],
        _ => return None,
    })
}

/// Every unit's weight for a recipe, in [`UNITS`]' order.
#[must_use]
pub fn weights(recipe: &[(&str, f32)]) -> [f32; 52] {
    let mut out = [0.0; 52];
    for (name, w) in recipe {
        if let Some(i) = unit(name) {
            out[i] = *w;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The list is sorted and has no repeats, so it is ARKit's order.
    #[test]
    fn the_units_are_arkits_sorted() {
        let mut sorted = UNITS;
        sorted.sort_unstable();
        assert_eq!(sorted, UNITS);
        assert!(UNITS.windows(2).all(|w| w[0] != w[1]));
    }

    /// Every recipe names real units, at a weight from 0 to 1.
    #[test]
    fn every_recipe_names_real_units() {
        for name in [
            "neutral",
            "happy",
            "laugh",
            "sad",
            "angry",
            "surprised",
            "afraid",
            "disgusted",
            "wink",
        ] {
            let r = recipe(name).expect(name);
            for (u, w) in r {
                assert!(unit(u).is_some(), "{name}: no unit {u}");
                assert!((0.0..=1.0).contains(w), "{name}: {u} at {w}");
            }
        }
    }
}
