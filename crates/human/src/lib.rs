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
pub mod fit;
pub mod macros;
#[cfg(feature = "pack")]
pub mod mocap;
#[cfg(feature = "pack")]
pub mod pack;
pub mod rig;
#[cfg(feature = "pack")]
pub mod textures;

pub use build::{build, HumanModel, Joint, Look, Part, PartKind, RetargetedClip};
pub use macros::Phenotype;

use serde::{Deserialize, Serialize};

/// Bumped whenever [`HumanPack`]'s layout changes: postcard is not
/// self-describing, so an old pack read by new code is garbage, not an error.
pub const PACK_VERSION: u32 = 6;

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
/// Stored small, because the sliders need hundreds of them (348 for adult
/// bodies, 2.9 million offsets): vertex numbers as gaps from the previous one,
/// mostly 1 and so a byte each, and offsets as whole steps of half a millimetre,
/// which postcard writes in one or two bytes and deflate shrinks further
/// ([`TARGET_STEP`]). (A step scaled to each target's largest offset was tried first:
/// every value then filled its 16 bits, and the adult and teenage targets
/// came to 18.7 MB.)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Target {
    /// Path under MakeHuman's `targets/`, without `.target`, e.g.
    /// `macrodetails/universal-male-young-averagemuscle-averageweight`.
    pub name: String,
    pub gaps: Vec<u16>,
    pub deltas: Vec<[i16; 3]>,
    /// Decimetres per unit of `deltas`.
    pub step: f32,
}

/// A target offset's resolution, decimetres: half a millimetre, below anything
/// visible on a body two metres from the camera, and coarse enough that most
/// offsets fit a single byte.
pub const TARGET_STEP: f32 = 0.005;

impl Target {
    /// Pack `(vertex, offset)` pairs; their order does not matter.
    #[must_use]
    pub fn new(name: &str, mut offsets: Vec<(u32, [f32; 3])>) -> Self {
        offsets.sort_by_key(|(v, _)| *v);
        offsets.dedup_by_key(|(v, _)| *v);
        let largest = offsets
            .iter()
            .flat_map(|(_, d)| d.iter().map(|x| x.abs()))
            .fold(0.0f32, f32::max);
        // 0.5 mm, unless a target moves a vertex more than 16 m (none does;
        // the largest, a baby's, is 1.07 m).
        let step = (largest / 32767.0).max(TARGET_STEP);
        let mut prev = 0u32;
        let mut gaps = Vec::with_capacity(offsets.len());
        let mut deltas = Vec::with_capacity(offsets.len());
        for (v, d) in offsets {
            let q = d.map(|x| (x / step).round().clamp(-32767.0, 32767.0) as i16);
            // Rounds to nothing: not worth its bytes.
            if q == [0; 3] {
                continue;
            }
            gaps.push((v - prev) as u16);
            prev = v;
            deltas.push(q);
        }
        Self {
            name: name.to_string(),
            gaps,
            deltas,
            step,
        }
    }

    /// Every `(vertex, offset)` the target moves, in decimetres.
    pub fn offsets(&self) -> impl Iterator<Item = (usize, glam::Vec3)> + '_ {
        let mut v = 0usize;
        self.gaps.iter().zip(&self.deltas).map(move |(&g, d)| {
            v += g as usize;
            (
                v,
                glam::Vec3::new(d[0] as f32, d[1] as f32, d[2] as f32) * self.step,
            )
        })
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
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct ProxyRef {
    pub v: [u32; 3],
    pub w: [f32; 3],
    pub offset: [f32; 3],
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
