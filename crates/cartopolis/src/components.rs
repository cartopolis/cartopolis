use bevy::prelude::*;
use cartopolis_core::{GeoAnchor, PrimSurface};

#[derive(Component, Debug, Clone)]
#[allow(dead_code)] // name kept for future UI labelling
pub struct Prim {
    pub id: i64,
    pub name: String,
    pub shape: PrimShape,
    pub color: Color,
    pub texture_id: Option<String>,
    /// A stored model asset that stands in for [`Self::shape`] (protocol v22).
    ///
    /// The value is the asset hash the simulator serves the file under, which is
    /// also the whole of the address — `systems::model_assets` turns it into a
    /// `sim://` asset path. `None` is every prim before this existed and every
    /// prim built from the shape menu since.
    pub model: Option<String>,
    /// Fraction of the path to start at (0.0–1.0, default 0.0). Maps to angular sweep for cylinder/cone.
    pub path_cut_begin: f32,
    /// Fraction of the path to end at (0.0–1.0, default 1.0).
    pub path_cut_end: f32,
    /// Hollow ratio (0.0–0.95, default 0.0). Inner radius = outer * hollow.
    pub hollow: f32,
    /// Twist (degrees) of the cross-section at the path start/end. Default 0/0.
    pub twist_begin: f32,
    pub twist_end: f32,
    /// Taper of the top cross-section per axis (−1.0–1.0, default 0). +X shrinks
    /// the top, −X shrinks the bottom. `taper_y` maps to the depth (Z) axis.
    pub taper_x: f32,
    pub taper_y: f32,
    /// Top shear: lateral offset of the top relative to the bottom (−0.5–0.5,
    /// default 0). `top_shear_y` maps to the depth (Z) axis.
    pub top_shear_x: f32,
    pub top_shear_y: f32,
    /// Slice: trims the path to the fraction [begin, end] (default 0.0/1.0).
    pub slice_begin: f32,
    pub slice_end: f32,
    /// Texture surface params (transparency, glow, full-bright, repeats, rotation, offset).
    pub surface: PrimSurface,
    /// A particle emitter carried on this prim (protocol v20), or `None` for the
    /// overwhelming majority that have none. `systems::particles` turns it into a
    /// child entity; nothing else reads it.
    pub particles: Option<cartopolis_core::ParticleEffect>,
    /// Whether the server holds a script for this prim (protocol v21). The client
    /// cannot derive it — source never travels in a snapshot — and
    /// `systems::interact` is what reads it, to decide whether walking up to this
    /// thing offers to press it.
    pub scripted: bool,
    /// A clip playing on the prim (protocol 28), drawn by `systems::prim_video`.
    pub video: Option<cartopolis_core::PrimVideo>,
    /// The sound the prim's script is looping (protocol 33), played by
    /// `systems::prim_sound`.
    pub sound_loop: Option<cartopolis_core::PrimSound>,
    /// The simulator moves this prim (protocol 34); its pose arrives as
    /// [`PhysicalMotion`] rather than through `PrimUpsert`.
    pub physical: bool,
}

/// The last pose the simulator sent for a physical prim
/// (`NetMessage::PrimMotion`), and where it is being drawn on the way there.
///
/// Twenty poses a second is a stutter if each is drawn as it lands, so
/// `prim_geo::smooth_physical_prims` carries the body on along its velocity
/// between them and eases what is drawn toward that — the way a remote avatar is
/// drawn, and for the same reason.
#[derive(Component, Debug, Clone)]
pub struct PhysicalMotion {
    pub geo: GeoAnchor,
    pub altitude: f32,
    pub rotation: Quat,
    pub velocity: Vec3,
    pub resting: bool,
    /// `Time::elapsed_secs_f64` when this pose arrived.
    pub received: f64,
    /// Absolute world position currently drawn; `None` until the first frame.
    pub shown: Option<bevy::math::DVec3>,
}

/// ADR-029: a prim's absolute geographic placement. Present iff the prim was
/// authored with geo (`PrimDto::geo`). `prim_geo::place_geo_prims` derives the
/// big_space `GridCell` + local `Transform` from this and the current OSM anchor,
/// re-projecting on re-anchor so the prim keeps its real-world location.
#[derive(Component, Debug, Clone, Copy)]
pub struct PrimGeo {
    pub latitude: f64,
    pub longitude: f64,
}

/// Marker: prim's mesh needs to be rebuilt (shape/path-cut/hollow changed).
#[derive(Component, Debug, Clone, Copy)]
pub struct NeedsMeshRebuild;

/// Marker: prim's material needs its `base_color_texture` swapped from the cache.
#[derive(Component, Debug, Clone, Copy)]
pub struct NeedsTextureRefresh;

#[derive(Component, Debug, Clone)]
pub struct Avatar;

/// Marker component for a prim that is currently selected (for editing).
#[derive(Component, Debug, Clone, Copy)]
pub struct Selected;

/// Another client's avatar (sim id from `WorldSnapshot`); same fox mesh as [`Avatar`].
/// `net_*` is authoritative each tick; [`crate::systems::avatar::smooth_remote_avatars`] blends `Transform`.
#[derive(Component, Debug, Clone, Copy)]
pub struct RemoteAvatar {
    pub sim_id: u64,
    pub net_position: Vec3,
    pub net_yaw: f32,
    /// Replicated world velocity (`AvatarStateDto::velocity`) at the snapshot
    /// that set `net_position`. Both the extrapolation and the gait read it.
    pub net_velocity: Vec3,
    /// Seconds since that snapshot arrived: reset by the snapshot apply,
    /// advanced by `avatar::smooth_remote_avatars`, and what turns
    /// `net_position` into where the avatar is *now*.
    pub net_age: f32,
    /// An emote this avatar started that we have not played yet — set for one
    /// frame by the snapshot apply and consumed by
    /// `avatar::update_remote_avatar_animation`.
    pub pending_emote: Option<cartopolis_core::Emote>,
    /// Sequence of the last emote played for this avatar. The sim holds an emote
    /// on the wire for a couple of seconds (so a client arriving mid-wave sees
    /// it), which means the same emote arrives in many snapshots; the sequence is
    /// what turns that level back into an edge, and what distinguishes "waved
    /// twice" from "still waving".
    pub last_emote_sequence: Option<u32>,
    /// Replicated seat (`AvatarStateDto::seated`): metres in world XZ from this
    /// avatar's position to where its body is drawn. `None` when it is standing.
    pub seated: Option<Vec2>,
    /// How much of that offset is applied, 0..1. **Not replicated** — the wire
    /// carries the seat, and each viewer eases onto it for itself, so a remote
    /// sits down over the same fraction of a second the sitter does without
    /// twenty offsets a second on the wire to say so.
    pub seat_blend: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimShape {
    Box,
    Sphere,
    Cylinder,
    Cone,
    Torus,
}

impl PrimShape {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "box" => PrimShape::Box,
            "sphere" => PrimShape::Sphere,
            "cylinder" => PrimShape::Cylinder,
            "cone" => PrimShape::Cone,
            "torus" => PrimShape::Torus,
            _ => PrimShape::Box,
        }
    }
}

// ─── Markers lifted out of `systems/` ─────────────────────────────────────────
//
// Zero-sized identity tags: no fields, no behaviour, nothing to depend on. They
// were declared next to whichever system spawned them, which is what made
// `free_camera` import `rendering` and `globe` import `rendering` — a marker is
// the cheapest possible thing to share and was creating cycles by being filed
// rather than by being used.

#[derive(Component)]
pub struct AvatarModelLoaded;

/// Marker for in-world departure-board quads.
#[derive(Component)]
pub struct BoardQuad;

/// Marker component for spawned data-layer cell meshes.
#[derive(Component)]
pub struct DataLayerMarkers;

/// The position marker drawn while driving.
#[derive(Component)]
pub struct DriverArrow;

#[derive(Component)]
pub struct FreeCamera;

/// Marker on the furniture entities, so the ramp can hide them wholesale.
#[derive(Component)]
pub struct FurnitureSurface;

/// Marker for globe imagery entities (base sphere + quadtree tiles).
#[derive(Component)]
pub struct GlobeMesh;

/// Local-scene ground props (the horizon disc, region quads, streamed map tiles)
/// that should be hidden once the globe is in view — from space they read as
/// bright specks/discs floating over the anchor. Attached where each is spawned.
#[derive(Component)]
pub struct HideInSpace;

/// Marker on the **coarsest** clipmap level (`MIN_ZOOM`) — the one level allowed to
/// show its untextured placeholder.
///
/// Every finer tile stays hidden until its imagery lands, so what shows through the
/// gap is the coarser level already stacked beneath it: the map arrives blurry and
/// sharpens, instead of arriving as grey squares. That backstop-from-below is what
/// the pyramid was built for, and an opaque placeholder quad was defeating it — the
/// finer tile spawned *on top* of the coarse imagery it was supposed to be refining.
/// `MIN_ZOOM` has nothing beneath it, so it keeps a visible placeholder or the
/// horizon would be a hole; it is painted the basemap's own paper colour, which
/// reads as unsurveyed ground rather than as a slab of concrete.
#[derive(Component)]
pub struct MapBackstop;

/// Marker for the paper-toned skirt that carries the ground past the last
/// streamed tile (`free_camera::GROUND_DISC_RADIUS`).
///
/// It carries [`HideInSpace`] as well, but it is the one prop that must go at
/// the *globe's* draw threshold rather than at the space tier: the globe is the
/// same surface done properly, so the two may never be on screen together. Its
/// own marker because `globe::update_globe_visibility` needs a query it can hold
/// disjoint from the `HideInSpace` sweep next to it.
#[derive(Component)]
pub struct GroundSkirt;

/// Marker for a spawned surface mesh, so the whole layer can be found and cleared.
#[derive(Component)]
pub struct MapSurface;

/// Marker for a tunnel's own geometry — the carriageway down there, its walls
/// and its roof (`SurfaceGroup::TunnelRoad` / `TunnelStructure`).
///
/// **Only so a capture can say whether the thing is on screen.** A tunnel is the
/// one feature here that is invisible when it is working *and* invisible when it
/// is broken: from anywhere but a low oblique over an open cut you see ground
/// either way. `Tunnels::tunnel_count` counts the floor-and-collision
/// description rather than triangles, so five captures in a row came back with
/// "41 tunnels" and nothing in frame, which is exactly the gap
/// `RouteVisual`'s pair of numbers exists to close for the route line.
#[derive(Component)]
pub struct TunnelSurface;

/// Marker for spawned building meshes.
#[derive(Component)]
pub struct OsmBuildingMesh;

/// Marker: this prim owns a private material copied out of the shared pool (it
/// was selected / edited / re-textured), so mutating it is safe. See
/// [`owned_material`].
#[derive(Component)]
pub struct OwnedMaterial;

/// Marker for the planet-centred entity that carries the built-in atmosphere's
/// [`Atmosphere`] component (spawned in `free_camera::setup_camera`), keeping a
/// copy of that component. `update_atmosphere_origin` reads it to keep the
/// atmosphere centred on Earth in render coords.
///
/// NOTE: do NOT add/remove the `Atmosphere` component (or the camera's
/// `AtmosphereEnvironmentMapLight`) at runtime as an on/off switch — that
/// teardown frees GPU resources still in flight and hangs the device on this
/// stack (confirmed via kernel `GPU HANG` logs). Both are chosen once at startup
/// (`CARTO_NO_ATMOSPHERE` / `CARTO_NO_ENVMAP`); the only safe runtime
/// change is the in-place render-*mode* switch below.
#[derive(Component)]
pub struct PlanetAtmosphere;

#[derive(Component)]
pub struct PrimMesh;

/// Marker for every entity making up the drawn route, so the whole lot can be
/// cleared in one query.
#[derive(Component)]
pub struct RouteVisual;

/// A radiating ground ring at a POI (subtle visibility aid; the signs
/// themselves are real-world sized and easy to miss from a distance).
#[derive(Component)]
pub struct StopRing;

/// Marker: the fetch attempt budget is spent and this tile will never get imagery.
///
/// Kept distinct from [`StreamTileReady`] because the two want opposite treatment
/// on screen: a textured tile is revealed, a permanently failed one stays hidden so
/// the coarser level beneath keeps covering that ground. Both stop reprocessing.
#[derive(Component)]
pub struct StreamTileFailed;

/// Marker: imagery applied — don't reprocess, and safe to show.
#[derive(Component)]
pub struct StreamTileReady;

/// Marker for a pooled label quad.
#[derive(Component)]
pub struct StreetLabelSign;

/// Marker on the canopy/trunk entities, so the ramp can hide them wholesale.
#[derive(Component)]
pub struct VegetationSurface;

/// Marker for the in-world vehicle boxes.
#[derive(Component)]
pub struct VehicleBox;

/// Marker for the billboarded line-number labels above the boxes.
#[derive(Component)]
pub struct VehicleLabel;
