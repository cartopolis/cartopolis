//! Prims on the wire: their state, surface, geometry, motion and scripts' output.

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::particles::ParticleEffect;
use crate::sounds::PrimSound;
use crate::tone::ToneSpec;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextureEntry {
    pub id: String,
    pub name: String,
}

/// Per-prim texture surface parameters (Texture tab). Mirrors a subset of the
/// Second Life texture tab: overall transparency, glow, full-bright, and the
/// texture UV transform (repeats per face, flip, rotation, offset).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PrimSurface {
    /// Overall opacity, 0.0 (fully transparent) – 1.0 (opaque). SL "Transparency %" = (1 − alpha) · 100.
    pub alpha: f32,
    /// Surface glow, 0.0 – 1.0. Drives emissive (separate from scene lighting).
    pub glow: f32,
    /// Render fully lit regardless of the day/night sun position (SL "Full Bright").
    pub full_bright: bool,
    /// Texture repeats per face along U / V (tiling count). Default 1.0.
    pub repeat_u: f32,
    pub repeat_v: f32,
    /// Mirror the texture along U / V.
    pub flip_u: bool,
    pub flip_v: bool,
    /// Texture rotation in degrees.
    pub rotation: f32,
    /// Texture offset along U / V (fraction of a repeat).
    pub offset_u: f32,
    pub offset_v: f32,
}

impl Default for PrimSurface {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            glow: 0.0,
            full_bright: false,
            repeat_u: 1.0,
            repeat_v: 1.0,
            flip_u: false,
            flip_v: false,
            rotation: 0.0,
            offset_u: 0.0,
            offset_v: 0.0,
        }
    }
}

/// Per-prim geometry deformation parameters (Object tab): path cut, hollow, and the
/// twist / taper / top-shear / slice "warp" set. Grouped so the online `UpdatePrim`
/// write path can carry them without a 20-field message variant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PrimGeometry {
    pub path_cut_begin: f32,
    pub path_cut_end: f32,
    pub hollow: f32,
    pub twist_begin: f32,
    pub twist_end: f32,
    pub taper_x: f32,
    pub taper_y: f32,
    pub top_shear_x: f32,
    pub top_shear_y: f32,
    pub slice_begin: f32,
    pub slice_end: f32,
}

impl Default for PrimGeometry {
    fn default() -> Self {
        Self {
            path_cut_begin: 0.0,
            path_cut_end: 1.0,
            hollow: 0.0,
            twist_begin: 0.0,
            twist_end: 0.0,
            taper_x: 0.0,
            taper_y: 0.0,
            top_shear_x: 0.0,
            top_shear_y: 0.0,
            slice_begin: 0.0,
            slice_end: 1.0,
        }
    }
}

/// Absolute geographic placement of a prim's horizontal position (ADR-029).
///
/// This is the anchor-independent ground truth: the client derives the
/// render-space `GridCell` + local `Transform` from it via the *current* OSM
/// anchor (`lat_lng_to_world`), so a prim keeps its real-world location across a
/// re-anchor. Altitude is carried separately in `PrimDto::position.y`.
/// `Default` is null island, and it means "not placed yet" rather than a place —
/// the one thing that reads it is a script's view of a prim before the first tick
/// has filled it in, where every sensor answer is "nobody" and that is correct.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct GeoAnchor {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrimDto {
    pub id: i64,
    pub name: String,
    pub shape: String,
    /// Sim-space position (metres). `position.y` is the authoritative altitude;
    /// horizontal placement (x/z) is derived from `geo` (ADR-029), so x/z here are
    /// only a transient echo of the last render frame.
    pub position: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
    pub color: [f32; 3],
    /// Optional texture key from standard library (ADR-017).
    #[serde(default)]
    pub texture_id: Option<String>,
    /// A stored model asset that stands in for [`Self::shape`] (protocol v22).
    ///
    /// The value is an asset hash — the name the simulator stores the glTF
    /// binary under, and the whole of the address: the client fetches
    /// `/api/assets/{model}.glb` and, because a file named by its own content
    /// can never change, caches it forever.
    ///
    /// **A hash and not an inventory item id.** An item is a person's *named
    /// reference* to a file and it can be renamed, moved to a folder or thrown
    /// away; the prim in the street must not go blank when it is. Two prims made
    /// from two people's copies of the same bench also carry the same value here,
    /// which is what lets the client hold one mesh for both.
    ///
    /// Every other field keeps its meaning when this is set — the scale, the
    /// rotation, the geo anchor, the colour tint, the script and the particles
    /// are all about the object, not about how its surface is generated. The
    /// deformation set (hollow, twist, taper) is the exception: it describes the
    /// procedural shape and is ignored.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub path_cut_begin: f32,
    #[serde(default = "default_path_cut_end")]
    pub path_cut_end: f32,
    #[serde(default)]
    pub hollow: f32,
    #[serde(default)]
    pub twist_begin: f32,
    #[serde(default)]
    pub twist_end: f32,
    #[serde(default)]
    pub taper_x: f32,
    #[serde(default)]
    pub taper_y: f32,
    #[serde(default)]
    pub top_shear_x: f32,
    #[serde(default)]
    pub top_shear_y: f32,
    #[serde(default)]
    pub slice_begin: f32,
    #[serde(default = "default_slice_end")]
    pub slice_end: f32,
    /// Texture surface params (ADR-017 / protocol v8).
    #[serde(default)]
    pub surface: PrimSurface,
    /// Absolute geographic placement (ADR-029 / protocol v10). Every prim is
    /// geo-anchored — there is no legacy fixed-origin placement.
    pub geo: GeoAnchor,
    /// A particle emitter on this prim, if it has one (protocol v20). `None` is
    /// the overwhelmingly common case and costs one byte.
    #[serde(default)]
    pub particles: Option<ParticleEffect>,
    /// Whether this prim carries a script (protocol v21).
    ///
    /// **The client cannot work this out.** Script source is deliberately kept
    /// out of `WorldSnapshot` — it is an order of magnitude larger than the rest
    /// of a prim and of interest to one person — so "does pressing this do
    /// anything?" has no answer on the client without the server saying. It is
    /// what `systems::interact` gates its prompt on: offering to press every
    /// crate in the city would be noise, and offering to press nothing would mean
    /// a button nobody can find.
    ///
    /// It says a script is *stored*, not that it is running: a source that failed
    /// to compile is stored and faulted, and pressing it does nothing. Reporting
    /// "stored" is the honest half — the client has no business knowing about a
    /// compile error on somebody else's object.
    #[serde(default)]
    pub scripted: bool,
    /// A video playing on the prim's surface (protocol 28,
    /// `docs/notes/video-on-a-prim.md`), or `None`. Set only by
    /// [`NetMessage::SetPrimVideo`] — never by `UpdatePrim`, which is a
    /// full-state edit every tool sends, so that a colour change cannot stop a
    /// video by forgetting to carry it.
    #[serde(default)]
    pub video: Option<PrimVideo>,
    /// The sound this prim is looping (`loop_sound`, protocol 33), or `None`.
    /// A level rather than an event, so somebody walking up to a fountain hears
    /// it. Set only by the prim's script; held in the simulator's memory and
    /// **not persisted** — a restart runs every script's `on_start` again, which
    /// is where a standing loop is started.
    #[serde(default)]
    pub sound_loop: Option<PrimSound>,
    /// The simulator moves this prim: it falls, collides with other prims and
    /// with avatars, and can be pushed and kicked (protocol 34,
    /// `docs/notes/physical-prims.md`). Second Life's *Physical* checkbox. Set
    /// only by [`NetMessage::SetPrimPhysical`] and by a script, never by
    /// `UpdatePrim`; while it is set the prim's pose arrives in
    /// [`NetMessage::PrimMotion`].
    #[serde(default)]
    pub physical: bool,
    /// The prim's script wants the hands within this many metres
    /// (`sense_hands`, protocol 36); 0 when it wants none. Clients read it to
    /// know which prims to report a hand to. Set only by the script and, like
    /// `sound_loop`, not persisted.
    #[serde(default)]
    pub hand_range: f32,
    /// The tone the prim's script is sounding (`set_tone`), or `None`. Here
    /// for a client arriving while it sounds; changes arrive as
    /// [`NetMessage::PrimTone`].
    #[serde(default)]
    pub tone: Option<ToneSpec>,
    /// A draft from the design room (protocol 51, `docs/notes/design-room.md`):
    /// sent only to its owner's connections, touched, carried and collided
    /// with by nobody else, its script not run, until it is placed.
    #[serde(default)]
    pub draft: bool,
    /// The groups this prim is in, outermost first (protocol 51): "Market
    /// stall", then "Frame". Empty for a prim in no group. Each prim carries its
    /// whole chain, so a client can draw the tree from the prims alone; a group
    /// holds only its owner's prims.
    #[serde(default)]
    pub groups: Vec<GroupLevel>,
}

/// One group a prim is in: an id the server gave it, unique on this server,
/// and the name its owner gave it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupLevel {
    pub id: i64,
    pub name: String,
}

/// A video on a prim: which clip, and when it started, so everybody near it
/// sees the same moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrimVideo {
    /// The clip's content hash in the simulator's video store (`/api/videos`).
    pub asset: String,
    /// When it started, in Unix milliseconds by the simulator's clock. A
    /// client plays from `now - started_at_ms`, so sync is only as good as the
    /// two clocks' agreement — seconds at worst on devices that keep time.
    pub started_at_ms: u64,
    /// Start again at the end, rather than stop on the last picture.
    pub looped: bool,
}

fn default_path_cut_end() -> f32 {
    1.0
}

fn default_slice_end() -> f32 {
    1.0
}

/// One physical prim's pose and velocity ([`NetMessage::PrimMotion`]).
///
/// The pose is the same pair a prim is always placed by — the geo anchor for
/// the horizontal and an altitude for the vertical — so a client applies it
/// exactly as it applies a `PrimUpsert`, with no frame of the server's own to
/// translate from. The velocity is in the client world frame's axes (x east,
/// y up, z south: every client's frame shares them, only the origin differs),
/// which is what lets a receiver carry a body forward between ticks.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PrimBody {
    pub id: i64,
    pub geo: GeoAnchor,
    /// Metres above the ground, as `PrimDto::position.y`.
    pub altitude: f32,
    /// Orientation as a quaternion `[x, y, z, w]` — not the Euler angles
    /// `PrimDto::rotation` stores, because a tumbling body is interpolated, and
    /// interpolating Euler angles is how a ball spins the long way round.
    pub rotation: [f32; 4],
    /// Metres per second.
    pub velocity: Vec3,
    /// The body has come to rest: place it exactly here and stop carrying it
    /// forward.
    pub resting: bool,
}

/// What [`NetMessage::SetPrimVideo`] asks for; the server adds the start time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetVideo {
    pub asset: String,
    pub looped: bool,
}

/// Severity of a [`NetMessage::ScriptOutput`] line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptLevel {
    /// `log` from a running script.
    Info,
    /// The host has something to say about the script's lifecycle (compiled,
    /// stopped, out of fuel).
    Notice,
    /// A compile error or a runtime fault. The script is not running.
    Error,
}
