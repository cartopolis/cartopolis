use bevy::prelude::*;
use vibe_core::PrimSurface;

#[derive(Component, Debug, Clone)]
#[allow(dead_code)] // latitude/longitude kept for future geo display
pub struct Region {
    pub id: i64,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub tile_x: i64,
    pub tile_y: i64,
    pub tile_z: i64,
    /// When set (network snapshot), region mesh uses this sim position; otherwise legacy grid layout.
    pub sim_origin: Option<Vec3>,
}

#[derive(Component, Debug, Clone)]
#[allow(dead_code)] // region_id/name kept for future UI labelling
pub struct Prim {
    pub id: i64,
    pub region_id: i64,
    pub name: String,
    pub shape: PrimShape,
    pub color: Color,
    pub texture_id: Option<String>,
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
}

/// Horizontal speed (m/s) from visual motion, used for remote run vs idle animation.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct RemoteAvatarMotionHint {
    pub last_translation: Vec3,
    pub horizontal_speed: f32,
    pub initialized: bool,
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
