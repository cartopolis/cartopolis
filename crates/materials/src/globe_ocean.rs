//! The globe's surface material: the ocean split. Why the ocean at globe scale is
//! a BRDF difference and not water is `cartopolis::systems::globe_ocean`'s module
//! note.

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
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

/// [`GlobeOcean`]'s two vectors, as the one buffer the shader reads them from
/// (binding 100; in bindless mode an element of the array at 110).
///
/// **Bindless is what this shape is for**, as it is for
/// `ground_detail::GroundUniform`. Every globe tile is a material of its own — it
/// has its own imagery — so with two plain uniforms every tile was a bind group
/// of its own, and with it a multidraw batch set of its own. Bevy rewrites three
/// GPU buffers per batch set per frame through `Queue::write_buffer`, and on Metal
/// each of those is a buffer created and later destroyed through the kernel:
/// over Groningen from 3 km the ~320 globe tiles on the horizon were ~330 of the
/// main pass's 373 batch sets, and creating and freeing their staging buffers was
/// ~8 ms of a 12 ms frame (M5 Pro, 2026-10-16). Bindless, they share a handful.
/// Where the platform cannot do bindless the same declaration falls back to an
/// ordinary uniform at 100.
#[derive(Clone, Copy, Debug, Default, ShaderType)]
pub struct GlobeOceanUniform {
    /// [`GlobeOcean::ocean`].
    pub ocean: Vec4,
    /// [`GlobeOcean::tint`].
    pub tint: Vec4,
    /// [`GlobeOcean::night`].
    pub night: Vec4,
    /// [`GlobeOcean::lights`].
    pub lights: Vec4,
    /// [`GlobeOcean::tile`].
    pub tile: Vec4,
}

impl From<&GlobeOcean> for GlobeOceanUniform {
    fn from(e: &GlobeOcean) -> Self {
        Self {
            ocean: e.ocean,
            tint: e.tint,
            night: e.night,
            lights: e.lights,
            tile: e.tile,
        }
    }
}

/// Ocean uniforms for `shaders/globe_ocean.wesl`.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug, Default)]
#[data(100, GlobeOceanUniform, binding_array(110))]
#[bindless(index_table(range(100..103), binding(109)))]
pub struct GlobeOcean {
    /// x: effect strength, ramped with `space_factor` (0 = the surface behaves
    /// exactly as it did before this material existed).
    /// y: 1 when the alpha channel actually carries a mask, 0 when it does not —
    /// the backstop sphere has no imagery at all, and an unbound texture reads as
    /// opaque white, which would otherwise turn the whole planet into ocean.
    /// z: ocean roughness. w: ocean reflectance.
    pub ocean: Vec4,
    /// rgb: the water tint. a unused.
    pub tint: Vec4,
    /// The night side (`docs/mockups/macro-view/`, v3): xyz the direction
    /// towards the sun in render space, from the real sun's position whatever
    /// the light is doing; w how dark the night side goes, 0 = off. The shader
    /// ramps it with the effect strength in `ocean.x`, so it arrives with the
    /// globe and not at the tier boundary.
    pub night: Vec4,
    /// City lights on the night side: x their strength (0 = off), y 1 when
    /// [`lights_map`](Self::lights_map) is bound.
    pub lights: Vec4,
    /// The tile's key for the optional hex grid (`hex_grid.wesl`): x, y, `2^z`,
    /// and w the layer's strength, 0 = off. Zero on the backstop sphere.
    pub tile: Vec4,
    /// Where the built-up land is on this tile, one channel, read off its own
    /// painted imagery (`globe::lights_mask`). `None` on the backstop sphere.
    #[texture(101)]
    #[sampler(102)]
    pub lights_map: Option<Handle<Image>>,
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
            ..Self::default()
        }
    }

    /// Ocean parameters for a surface with no mask (the backstop sphere): inert.
    #[must_use]
    pub fn unmasked() -> Self {
        Self {
            ocean: Vec4::new(0.0, 0.0, LAND_ROUGHNESS, 0.0),
            tint: Vec3::ONE.extend(1.0),
            ..Self::default()
        }
    }
}

impl MaterialExtension for GlobeOcean {
    fn fragment_shader() -> ShaderRef {
        "shaders/globe_ocean.wesl".into()
    }
}
