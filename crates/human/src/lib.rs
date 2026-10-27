//! Realistic people, built at runtime from MakeHuman's CC0 data.
//!
//! A [`HumanPack`] holds everything a body is made from: the MakeHuman base mesh
//! (every vertex, the helper geometry included, because the joints and the
//! clothes are placed from it), the shape targets the macro sliders blend, the
//! `game_engine` rig, its skin weights, the clothes and hair as fitting rules
//! rather than meshes, and motion-capture clips as motion relative to their
//! takes' own T-pose (`mocap`). [`build`] turns a pack and a [`Look`] into a
//! [`HumanModel`]: meshes, a skeleton and clips retargeted onto *this* body.
//!
//! Why the fitting rules and not fitted meshes: a garment is stored as, per
//! vertex, three base-mesh vertices, their weights and an offset
//! ([`ProxyRef`]), so it follows any body the sliders make. The garment `.obj`
//! files are not even in their fitted positions (they were authored on other
//! bodies), so only their faces and UVs are kept.
//!
//! Frames: the pack is in MakeHuman's OBJ frame — decimetres, +Y up, facing
//! +Z, the same handedness as Bevy. [`build`] scales to metres and stands the
//! body on y = 0. Rolls and the T-pose are Blender's, so the rig maths is done
//! in Blender's frame (Z up) and converted once ([`rig`]).

pub mod build;
pub mod face;
pub mod fit;
pub mod hands;
pub mod macros;
#[cfg(feature = "pack")]
pub mod mocap;
#[cfg(feature = "pack")]
pub mod pack;
pub mod reach;
pub mod rig;
#[cfg(feature = "pack")]
pub mod textures;

pub use build::{build, HumanModel, Joint, Look, Part, PartKind, RetargetedClip};
pub use macros::Phenotype;

use serde::{Deserialize, Serialize};

/// Bumped whenever [`HumanPack`]'s layout changes: postcard is not
/// self-describing, so an old pack read by new code is garbage, not an error.
pub const PACK_VERSION: u32 = 8;

/// Base-mesh vertices `0..BODY_VERTICES` are the skin; the rest are helpers
/// (joint cubes, eyes, teeth, tights, hair…) that are shaped with it but never
/// drawn.
pub const BODY_VERTICES: usize = 13_380;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct HumanPack {
    pub version: u32,
    /// Every base-mesh vertex, OBJ frame (decimetres).
    pub vertices: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// The skin's faces (group `body`).
    pub body_faces: Vec<Face>,
    pub targets: Vec<Target>,
    /// The `game_engine` rig, parents before children.
    pub bones: Vec<Bone>,
    /// Per base vertex, every `(bone, weight)` it has; [`build`] keeps four.
    pub weights: Vec<Vec<(u16, f32)>>,
    pub proxies: Vec<Proxy>,
    pub skins: Vec<Skin>,
    pub eye_colours: Vec<EyeColour>,
    /// MakeHuman's face controls, each a pair of target sets.
    pub modifiers: Vec<Modifier>,
    pub clips: Vec<SourceClip>,
    /// The face's 52 ARKit units, in [`face::UNITS`]' order, or none: shape
    /// targets on the base mesh that the client blends live on the GPU rather
    /// than baking in ([`face`]). Last, so a pack of version 7 is this one
    /// with the field empty (`pack::decode_upgrading`).
    pub face_units: Vec<Target>,
}

/// A triangle or a quad: `v` indexes the mesh's vertices, `uv` its UVs. A
/// triangle repeats nothing; its fourth slot is [`Face::NONE`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Face {
    pub v: [u32; 4],
    pub uv: [u32; 4],
}

impl Face {
    pub const NONE: u32 = u32::MAX;

    /// The face as triangles of corner indices (0..4), counter-clockwise.
    pub fn triangles(&self) -> impl Iterator<Item = [usize; 3]> {
        let quad = self.v[3] != Self::NONE;
        std::iter::once([0, 1, 2]).chain(quad.then_some([0, 2, 3]))
    }
}

/// A shape target: sparse offsets, OBJ frame, added at the slider's weight.
///
/// Stored small, because the sliders need hundreds of them (786 in the shipped
/// pack, 4.5 million offsets): vertex numbers as gaps from the previous one,
/// mostly 1 and so a byte each, and offsets as whole steps of half a millimetre,
/// which postcard writes in one or two bytes and deflate shrinks further
/// ([`TARGET_STEP`]). (A step scaled to each target's largest offset was tried first:
/// every value then filled its 16 bits, and the adult and teenage targets
/// came to 18.7 MB.)
///
/// **The offsets are three columns, each value stored as its difference from
/// the one before.** Neighbouring vertices of a whole-body target move almost
/// alike, so the differences are mostly a step or two, and one axis's run of
/// small numbers is what deflate packs best. Measured 2026-10-18 on the
/// shipped pack: the 786 targets took 10.26 MB deflated as `[x, y, z]` rows,
/// 6.59 MB as plain columns and 5.13 MB as differenced columns — the same
/// numbers, losslessly. (Halving the resolution to 1 mm as well gave 4.21 MB;
/// not taken.) The differences wrap at 16 bits, so they hold in an `i16`
/// whatever the jump.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Target {
    /// Path under MakeHuman's `targets/`, without `.target`, e.g.
    /// `macrodetails/universal-male-young-averagemuscle-averageweight`.
    pub name: String,
    pub gaps: Vec<u16>,
    /// x, y and z, each in steps, each value less the one before it
    /// (wrapping).
    pub columns: [Vec<i16>; 3],
    /// Decimetres per unit of the offsets.
    pub step: f32,
}

/// A target offset's resolution, decimetres: half a millimetre, below anything
/// visible on a body two metres from the camera, and coarse enough that most
/// offsets fit a single byte.
pub const TARGET_STEP: f32 = 0.005;

impl Target {
    /// Pack `(vertex, offset)` pairs; their order does not matter.
    #[must_use]
    pub fn new(name: &str, offsets: Vec<(u32, [f32; 3])>) -> Self {
        Self::with_step(name, offsets, TARGET_STEP)
    }

    /// [`Target::new`] at a finer resolution than [`TARGET_STEP`], for
    /// targets whose whole movement is a few millimetres (an eye turning).
    #[must_use]
    pub fn with_step(name: &str, mut offsets: Vec<(u32, [f32; 3])>, finest: f32) -> Self {
        offsets.sort_by_key(|(v, _)| *v);
        offsets.dedup_by_key(|(v, _)| *v);
        let largest = offsets
            .iter()
            .flat_map(|(_, d)| d.iter().map(|x| x.abs()))
            .fold(0.0f32, f32::max);
        // `finest` (0.5 mm for a body target), unless a target moves a vertex
        // more than 16 m (none does; the largest, a baby's, is 1.07 m).
        let step = (largest / 32767.0).max(finest);
        let mut prev = 0u32;
        let mut last = [0i16; 3];
        let mut gaps = Vec::with_capacity(offsets.len());
        let mut columns: [Vec<i16>; 3] = Default::default();
        for (v, d) in offsets {
            let q = d.map(|x| (x / step).round().clamp(-32767.0, 32767.0) as i16);
            // Rounds to nothing: not worth its bytes.
            if q == [0; 3] {
                continue;
            }
            gaps.push((v - prev) as u16);
            prev = v;
            for (a, column) in columns.iter_mut().enumerate() {
                column.push(q[a].wrapping_sub(last[a]));
            }
            last = q;
        }
        Self {
            name: name.to_string(),
            gaps,
            columns,
            step,
        }
    }

    /// Every `(vertex, offset)` the target moves, in decimetres.
    pub fn offsets(&self) -> impl Iterator<Item = (usize, glam::Vec3)> + '_ {
        let mut v = 0usize;
        let mut q = [0i16; 3];
        let [x, y, z] = &self.columns;
        self.gaps
            .iter()
            .zip(x.iter().zip(y).zip(z))
            .map(move |(&g, ((&dx, &dy), &dz))| {
                v += g as usize;
                q = [
                    q[0].wrapping_add(dx),
                    q[1].wrapping_add(dy),
                    q[2].wrapping_add(dz),
                ];
                (
                    v,
                    glam::Vec3::new(q[0] as f32, q[1] as f32, q[2] as f32) * self.step,
                )
            })
    }

    /// How many vertices the target moves.
    #[must_use]
    pub fn len(&self) -> usize {
        self.gaps.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.gaps.is_empty()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Bone {
    pub name: String,
    pub parent: Option<u16>,
    /// Base-mesh vertices whose mean is the bone's head (a joint cube, one
    /// vertex, or a list), and the same for its tail.
    pub head: Vec<u32>,
    pub tail: Vec<u32>,
    /// Blender's bone roll, radians.
    pub roll: f32,
    /// MakeHuman's T-pose for this rig: Blender Euler XYZ, bone space. What a
    /// capture's rest pose is matched against ([`rig::retarget`]).
    pub tpose: [f32; 3],
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyKind {
    Eyes,
    Eyebrows,
    Eyelashes,
    Hair,
    /// A whole outfit: MakeHuman's garments are suits, top and bottom in one
    /// mesh, which the pack splits into tint regions ([`Proxy::face_regions`]).
    Outfit,
    Shoes,
    Hat,
}

/// Something worn: eyes, brows, lashes, hair, a garment.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Proxy {
    pub name: String,
    pub kind: ProxyKind,
    /// One per proxy vertex: where it sits on the base mesh.
    pub refs: Vec<ProxyRef>,
    /// `x_scale`/`y_scale`/`z_scale`: two base vertices and the distance
    /// between them on the body the garment was authored on. The offsets scale
    /// with the ratio, so a garment grows with the body.
    pub scale: [Option<(u32, u32, f32)>; 3],
    pub uvs: Vec<[f32; 2]>,
    pub faces: Vec<Face>,
    /// Body vertices this proxy covers; a body face all of whose corners are
    /// listed is not drawn, so skin cannot poke through.
    pub delete_verts: Vec<u32>,
    /// Texture path relative to the pack's texture directory.
    pub texture: Option<String>,
    /// Cut out by alpha (hair, brows, lashes) rather than opaque.
    pub alpha: bool,
    /// Per face, its tint region: an outfit's top (0) and bottom (1). Empty
    /// for one region.
    pub face_regions: Vec<u8>,
    /// Per region, the colour that gives the garment back as it was authored.
    /// Non-empty means the texture is a *neutral* one — its colour taken out
    /// and its light kept — so a tint can make it any colour, lighter ones
    /// included. Empty means the texture is the original, shown as it is.
    pub region_colours: Vec<[u8; 3]>,
}

/// One proxy vertex: `Σ w·B[v] + offset·scale`, over the morphed base mesh B.
///
/// Stored in whole steps: weights in 1/16384ths and the offset in
/// [`TARGET_STEP`]s, the half millimetre the shape targets already use. As
/// three `f32` each they were 2.85 MB of the pack, deflate getting nothing out
/// of their mantissas. The weights are `i32` because MakeHuman's go past 2
/// (every pair of shoes, some suits and hair): as `i16` they clipped, and
/// the clipped vertices stood out of the garment as spikes. postcard writes
/// either in the same three bytes for a weight near 1.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct ProxyRef {
    pub v: [u32; 3],
    w: [i32; 3],
    offset: [i16; 3],
}

/// One unit of a [`ProxyRef`] weight.
const PROXY_WEIGHT_STEP: f32 = 1.0 / 16384.0;

impl ProxyRef {
    #[must_use]
    pub fn new(v: [u32; 3], w: [f32; 3], offset: [f32; 3]) -> Self {
        Self {
            v,
            w: w.map(|x| (x / PROXY_WEIGHT_STEP).round() as i32),
            offset: offset.map(|x| (x / TARGET_STEP).round().clamp(-32767.0, 32767.0) as i16),
        }
    }

    /// The three base vertices' weights.
    #[must_use]
    pub fn w(&self) -> [f32; 3] {
        self.w.map(|x| x as f32 * PROXY_WEIGHT_STEP)
    }

    /// The offset from the weighted base vertices, in decimetres before scaling.
    #[must_use]
    pub fn offset(&self) -> glam::Vec3 {
        glam::Vec3::from_array(self.offset.map(|x| f32::from(x) * TARGET_STEP))
    }
}

/// A skin texture, and whom MakeHuman painted it for: a look picks its skin
/// by `family`, and the body's age and shape pick among the family's six.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Skin {
    pub name: String,
    pub texture: String,
    /// MakeHuman's `african`, `asian`, `caucasian`.
    pub family: String,
    pub female: bool,
    pub age: SkinAge,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkinAge {
    Young,
    Middle,
    Old,
}

/// A MakeHuman modifier: a slider from -1 to 1 that blends `negative` below
/// zero and `positive` above, each at the slider's size. A one-sided control
/// (`head/head-oval`) has no negative; a left-and-right one lists a target
/// per side, moved together.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Modifier {
    /// `group/name`, e.g. `nose/nose-scale-horiz-decr-incr`.
    pub name: String,
    pub negative: Vec<String>,
    pub positive: Vec<String>,
}

/// An iris texture for the eyes.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EyeColour {
    pub name: String,
    pub texture: String,
}

/// A clip of motion capture, as motion relative to the take's own
/// T-pose, sampled at a fixed rate — independent of any body, so one pack
/// animates every body the sliders make.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SourceClip {
    pub name: String,
    pub fps: f32,
    pub frames: u32,
    /// Per rig bone (indexing [`HumanPack::bones`]): its world rotation at each
    /// frame times the inverse of its world rotation in the take's T-pose, in
    /// our frame (x, y, z, w). `None` for a bone the take has no joint for.
    pub rotations: Vec<Option<Vec<[f32; 4]>>>,
    /// The pelvis's displacement at each frame, in hip heights (the pelvis
    /// above the ankles at rest), so it scales to any body's legs. Its height is
    /// only relative: the retarget stands the clip on the ground itself.
    pub hips: Vec<[f32; 3]>,
    /// How fast the take travelled, in hip heights a second; 0 for a clip
    /// played on the spot. A locomotion clip plays at ground speed over this.
    pub speed: f32,
    /// The last frame is the first, so the clip repeats without a seam.
    pub looped: bool,
    /// Per rig bone: which way the take's bone points in its rest pose, in our
    /// frame — towards the joint of `child` (a rig bone), or towards the
    /// joint's End Site when `child` is `None`. The retarget turns the body's
    /// own T-pose bone onto it, so a spine the capture holds dead straight at
    /// rest is not leant forward twice ([`rig::retarget`]).
    pub rest_dirs: Vec<Option<RestDir>>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct RestDir {
    pub dir: [f32; 3],
    pub child: Option<u16>,
}

impl HumanPack {
    /// Decode a pack, refusing one written for another layout.
    /// A pack is postcard, deflated.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        use std::io::Read;
        let mut raw = Vec::new();
        flate2::read::DeflateDecoder::new(bytes)
            .read_to_end(&mut raw)
            .map_err(|e| format!("not a human pack: {e}"))?;
        let pack: Self = postcard::from_bytes(&raw).map_err(|e| e.to_string())?;
        if pack.version != PACK_VERSION {
            return Err(format!(
                "pack version {} is not {PACK_VERSION}; rebuild it with human-pack",
                pack.version
            ));
        }
        Ok(pack)
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        use std::io::Write;
        let raw = postcard::to_allocvec(self).map_err(|e| e.to_string())?;
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        z.write_all(&raw).map_err(|e| e.to_string())?;
        z.finish().map_err(|e| e.to_string())
    }

    pub fn bone(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|b| b.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The differenced columns give back exactly the steps that went in, a
    /// jump across most of an `i16`'s range included, and a vertex that
    /// rounds to nothing is left out.
    #[test]
    fn a_target_gives_back_its_offsets() {
        let step = TARGET_STEP;
        let input = vec![
            (3, [0.0, 1.0, -2.0]),
            (4, [150.0, -150.0, 0.5]),
            (9, [-150.0, 150.0, 0.0]),
            (10, [0.0001, 0.0, 0.0]),
            (12, [0.01, 0.02, -0.03]),
        ];
        let t = Target::new("t", input.clone());
        let out: Vec<(usize, glam::Vec3)> = t.offsets().collect();
        assert_eq!(out.len(), 4, "the vertex that rounds to nothing is dropped");
        for ((v, got), (want_v, want)) in out.iter().zip(input.iter().filter(|(v, _)| *v != 10)) {
            assert_eq!(*v, *want_v as usize);
            let want = glam::Vec3::from_array(*want);
            assert!(
                (*got - want).abs().max_element() <= step * 0.5 + 1.0e-6,
                "{got} vs {want}"
            );
        }
    }

    /// A clothing reference keeps its weights and offset to within half a step.
    #[test]
    fn a_proxy_ref_keeps_its_numbers() {
        let r = ProxyRef::new([1, 2, 3], [0.7, 0.25, 0.05], [0.123, -0.456, 0.0]);
        let w = r.w();
        for (a, b) in w.iter().zip([0.7, 0.25, 0.05]) {
            assert!((a - b).abs() <= PROXY_WEIGHT_STEP);
        }
        let o = r.offset();
        assert!(
            (o - glam::Vec3::new(0.123, -0.456, 0.0))
                .abs()
                .max_element()
                <= TARGET_STEP
        );
    }
}
