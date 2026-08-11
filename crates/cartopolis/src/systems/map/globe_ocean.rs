//! The ocean at globe scale (ADR-025 tier, the counterpart to `globe_weather`).
//!
//! `systems::water` gives the *local* tier real water: a wave field, a shoreline,
//! shallows. None of that transfers to orbit — from 3,000 km a wave is many
//! orders of magnitude below a pixel, and a normal map at that scale is wasted
//! bandwidth. What actually distinguishes an ocean from a continent in every
//! photograph ever taken from space is far simpler and entirely a **BRDF**
//! difference: the sea is dark and glossy, the land is bright and matte, and the
//! sun draws a broad glint across the water somewhere near the sub-solar point.
//!
//! So this is not a wave shader. It is one number per texel — "is this water?" —
//! driving roughness, specular and tint on the globe's own tile materials. The
//! mask rides in the **alpha channel of the tile imagery** the globe already
//! uploads (`tile_loader::load_globe_tile_rgba`), so it costs no extra texture,
//! binding or upload; the shader reads it back out of the sampled base colour
//! before `alpha_discard` flattens it.
//!
//! Everything ramps on `GlobeState::space_factor`, like the rest of the tier
//! (see the CLAUDE.md note): the flat map tiles just below the boundary have no
//! gloss at all, so switching it on at the threshold would put a hard cut in the
//! middle of a climb — the exact failure the albedo ramp exists to avoid.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

/// Roughness of open ocean in deep space.
///
/// Not a mirror. A mirror-smooth sphere concentrates the sun into a handful of
/// pixels that alias into a crawling sparkle as the globe turns, which is both
/// wrong and ugly; the real glint is *broad* because it is the statistical sum of
/// a whole sea's worth of wave facets — the same sub-pixel detail the local
/// shader pays back as roughness, one scale further out.
const OCEAN_ROUGHNESS: f32 = 0.34;

/// Land keeps the matte response the globe imagery has always had.
const LAND_ROUGHNESS: f32 = 1.0;

/// Specular reflectance of water, remapped the way `StandardMaterial` does it
/// (0.5 ⇒ F0 = 0.04). Water's F0 is ~0.02, so 0.25 — the same value the local
/// water material uses, for the same reason.
const OCEAN_REFLECTANCE: f32 = 0.25;

/// Colour multiplier applied to the imagery where it is water, at full space
/// factor. The basemap paints a *pale cartographic* blue; a real ocean is much
/// darker than the land beside it, and that contrast is most of what makes the
/// continents read from orbit. Darkening the sea also buys back the headroom the
/// glint needs — a pale sea plus a specular highlight just clips to white.
const OCEAN_TINT: Vec3 = Vec3::new(0.42, 0.55, 0.72);

/// The globe's surface material: the same `StandardMaterial` as before, plus the
/// ocean split. Used by **both** the tile quadtree and the backstop sphere, so
/// `update_globe_albedo` still has one material type to walk.
pub type GlobeSurfaceMaterial = ExtendedMaterial<StandardMaterial, GlobeOcean>;

/// Ocean uniforms for `shaders/globe_ocean.wgsl`.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug, Default)]
pub struct GlobeOcean {
    /// x: effect strength, ramped with `space_factor` (0 = the surface behaves
    /// exactly as it did before this material existed).
    /// y: 1 when the alpha channel actually carries a mask, 0 when it does not —
    /// the backstop sphere has no imagery at all, and an unbound texture reads as
    /// opaque white, which would otherwise turn the whole planet into ocean.
    /// z: ocean roughness. w: ocean reflectance.
    #[uniform(100)]
    pub ocean: Vec4,
    /// rgb: the water tint. a unused.
    #[uniform(101)]
    pub tint: Vec4,
}

/// `CARTO_NO_OCEAN=1` strips the ocean split, leaving the matte globe this
/// module replaced — one more layer to remove when bisecting a render problem,
/// alongside `CARTO_NO_ATMOSPHERE` / `NO_HAZE` / `NO_ENVMAP`. Read once: this
/// is consulted on every tile spawn.
fn disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DISABLED.get_or_init(|| std::env::var("CARTO_NO_OCEAN").is_ok())
}

impl GlobeOcean {
    /// Ocean parameters for a surface that carries an imagery mask.
    #[must_use]
    pub fn masked(strength: f32) -> Self {
        if disabled() {
            return Self::unmasked();
        }
        Self {
            ocean: Vec4::new(strength, 1.0, OCEAN_ROUGHNESS, OCEAN_REFLECTANCE),
            tint: OCEAN_TINT.extend(1.0),
        }
    }

    /// Ocean parameters for a surface with no mask (the backstop sphere): inert.
    #[must_use]
    pub fn unmasked() -> Self {
        Self {
            ocean: Vec4::new(0.0, 0.0, LAND_ROUGHNESS, 0.0),
            tint: Vec3::ONE.extend(1.0),
        }
    }
}

impl MaterialExtension for GlobeOcean {
    fn fragment_shader() -> ShaderRef {
        "shaders/globe_ocean.wgsl".into()
    }
}

/// Current ocean strength, so a tile streaming in mid-climb comes up matching the
/// tiles around it instead of popping glossy — the same contract
/// `GlobeSurfaceAlbedo` has for the albedo ramp, and written by the same system.
#[derive(Resource, Default)]
pub struct GlobeOceanGloss(pub f32);
