//! Realistic people, built at runtime from MakeHuman's CC0 data.
//!
//! A [`HumanPack`] holds everything a body is made from: the MakeHuman base mesh
//! (every vertex, the helper geometry included, because the joints and the
//! clothes are placed from it), the shape targets the macro sliders blend, the
//! `game_engine` rig, its skin weights, the clothes and hair as fitting rules
//! rather than meshes, and the Quaternius animation clips as motion relative to
//! their own T-pose. [`build`] turns a pack and a [`Look`] into a
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
pub mod pack;
pub mod rig;

pub use build::{build, HumanModel, Joint, Look, Part, PartKind, RetargetedClip};
pub use macros::Phenotype;

use serde::{Deserialize, Serialize};

/// Bumped whenever [`HumanPack`]'s layout changes: postcard is not
/// self-describing, so an old pack read by new code is garbage, not an error.
pub const PACK_VERSION: u32 = 1;

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
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Target {
    /// Path under MakeHuman's `targets/`, without `.target`, e.g.
    /// `macrodetails/universal-male-young-averagemuscle-averageweight`.
    pub name: String,
    pub indices: Vec<u32>,
    pub deltas: Vec<[f32; 3]>,
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
    /// MakeHuman's T-pose for this rig: Blender Euler XYZ, bone space. What the
    /// animation library's own rest pose is matched against ([`rig::retarget`]).
    pub tpose: [f32; 3],
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyKind {
    Eyes,
    Eyebrows,
    Eyelashes,
    Hair,
    Clothes,
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
}

/// One proxy vertex: `Σ w·B[v] + offset·scale`, over the morphed base mesh B.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct ProxyRef {
    pub v: [u32; 3],
    pub w: [f32; 3],
    pub offset: [f32; 3],
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Skin {
    pub name: String,
    pub texture: String,
}

/// A clip from the animation library, as motion relative to the library's own
/// T-pose, sampled at a fixed rate — independent of any body, so one pack
/// animates every body the sliders make.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SourceClip {
    pub name: String,
    pub fps: f32,
    pub frames: u32,
    /// Per rig bone (indexing [`HumanPack::bones`]): its world rotation at each
    /// frame times the inverse of its world rotation in the library's T-pose,
    /// in our frame (x, y, z, w). `None` for a bone the library does not move.
    pub rotations: Vec<Option<Vec<[f32; 4]>>>,
    /// The pelvis's world displacement from its rest position at each frame,
    /// over the library's hip height, so it scales to any body's legs.
    pub hips: Vec<[f32; 3]>,
}

impl HumanPack {
    /// Decode a pack, refusing one written for another layout.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let pack: Self = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
        if pack.version != PACK_VERSION {
            return Err(format!(
                "pack version {} is not {PACK_VERSION}; rebuild it with human-pack",
                pack.version
            ));
        }
        Ok(pack)
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        postcard::to_allocvec(self).map_err(|e| e.to_string())
    }

    pub fn bone(&self, name: &str) -> Option<usize> {
        self.bones.iter().position(|b| b.name == name)
    }
}
