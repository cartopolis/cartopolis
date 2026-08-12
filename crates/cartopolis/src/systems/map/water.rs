//! Water material for the vector-tile surface meshes (ADR-035, option B).
//!
//! `map_geometry` spawns one batched mesh per [`SurfaceGroup`]; every group but
//! water gets a plain `StandardMaterial`. Water gets this one: a
//! [`MaterialExtension`] over `StandardMaterial` whose fragment shader
//! (`assets/shaders/water.wgsl`) replaces the flat +Y normal with an animated
//! wave field, trades distance-faded ripple detail for roughness, and tints the
//! albedo by fresnel so the surface darkens looking down and brightens at
//! grazing angles.
//!
//! Extending rather than replacing `StandardMaterial` is the whole trick: the
//! sun, the shadow cascades, the fog and — most of all — the atmosphere's
//! environment map already reach every standard material, so the sea reflects
//! *this scene's* sky and follows the day/night cycle without a single extra
//! uniform. A standalone `Material` would have to re-derive all of it.
//!
//! The wave state is driven by live weather: [`update_water_waves`] maps
//! `WeatherState`'s wind speed onto steepness, wavelength, chop and foam, so a
//! gale actually whips up the canals. It writes the uniforms only when the wind
//! has moved meaningfully — the animation clock itself is `globals.time` inside
//! the shader, so a still frame costs no CPU work at all.
//!
//! Where the water *is* matters as much as what the wind is doing. Each cell
//! carries a rasterised distance-to-shore field ([`vector_tiles::water_shore_field`]),
//! and that one number per fragment buys the three things a wave field alone
//! cannot give: a canal is sheltered from the swell that belongs on the open
//! river, the shallows read lighter and greener than the deep, and a foam line
//! laps against the bank instead of the water ending in a hard polygon edge.
//! It is also why water is the one surface group with a material *per cell*.
//!
//! [`SurfaceGroup`]: crate::systems::vector_tiles::SurfaceGroup

use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

use crate::systems::vector_tiles::{self, ShoreField};
use crate::systems::weather::WeatherState;
use crate::CLOUD_WIND;

/// Albedo of the water body itself — dark, because nearly everything you see
/// looking at water is *reflected*, not scattered back out of it.
///
/// Being an albedo, this is not what the surface *renders* as: the shader takes
/// another ~40% out of it for depth (`sea_state`'s `surface.z`) except at grazing
/// angles, and the sun, sky and environment map put light back. Shaded, it lands
/// around `#36495C`–`#415462`.
///
/// That rendered tone is the number `vector_tiles::palette::WATER` is matched to,
/// because the vector surfaces only stream in a ring around the camera and the
/// paint has to take over at its edge without a step. Changing this means
/// re-sampling a render and moving the palette entry to wherever it lands.
const WATER_ALBEDO: Color = Color::srgb(0.06, 0.16, 0.21);

/// Wind speed (m/s) at which the wave shaping is fully "blown up" — beyond this
/// the sea state stops growing. Roughly Beaufort 6.
const FULL_SEA_WIND: f32 = 12.0;

/// Wind speed (m/s) where whitecaps start. Below it the crests stay clean.
const FOAM_ONSET_WIND: f32 = 7.0;

/// Wind assumed until a weather sample lands (or when the fetch fails outright,
/// as it does with no network). A light breeze, not dead calm: glass-flat water
/// is a real state but a rare one, and it is the one that reads as "the shader
/// isn't running".
const UNKNOWN_WIND: f32 = 3.0;

/// Wind change (m/s) needed before the uniforms are rewritten. The shader
/// animates itself from `globals.time`, so this system only exists to track
/// *weather*, and weather moves on the scale of minutes.
const WIND_EPSILON: f32 = 0.25;

/// The material `map_geometry` uses for [`SurfaceGroup::Water`].
///
/// [`SurfaceGroup::Water`]: crate::systems::vector_tiles::SurfaceGroup::Water
pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterExtension>;

/// Wave/surface uniforms for `shaders/water.wgsl`.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
pub struct WaterExtension {
    /// x: wave steepness (normal strength). y: base wavelength (m).
    /// z: wave speed scalar (m/s). w: detail cut-off (per metre of view range).
    #[uniform(100)]
    pub wave: Vec4,
    /// x: near roughness. y: extra roughness folded in with distance.
    /// z: top-down depth darkening. w: crest foam amount.
    #[uniform(101)]
    pub surface: Vec4,
    /// xy: unit wind direction. z: chop 0–1 (swell → wind chop). w unused.
    #[uniform(102)]
    pub wind: Vec4,
    /// Where this cell's [shore field] sits: xy = the cell's north-west corner in
    /// the same anchor-metre frame the mesh's UV0 uses, z = the cell's ground
    /// size in metres, w = the metre value a fully-saturated texel stands for.
    ///
    /// [shore field]: crate::systems::vector_tiles::water_shore_field
    #[uniform(103)]
    pub field: Vec4,
    /// Distance from the nearest bank, in this cell's tile-local frame.
    #[texture(104)]
    #[sampler(105)]
    pub shore: Handle<Image>,
}

impl MaterialExtension for WaterExtension {
    fn fragment_shader() -> ShaderRef {
        "shaders/water.wgsl".into()
    }
}

/// The sea state the whole scene's water shares, plus the pieces every cell's
/// material is built from.
///
/// Water needs a material *per cell*, because the shore field is a per-cell
/// texture — so unlike the other surface groups it cannot ride a single shared
/// handle. What is shared instead is this: the wind-driven wave parameters, so a
/// cell built three seconds after the weather changed comes up in the same sea as
/// its neighbours instead of popping (the same rule `globe::GlobeSurfaceAlbedo`
/// follows for newly-streamed globe tiles).
#[derive(Resource)]
pub struct WaterSeaState {
    /// Current wind-driven uniforms (`wave`, `surface`, `wind`).
    wave: Vec4,
    surface: Vec4,
    wind: Vec4,
    /// 1×1 "open water everywhere" field, for a cell whose shore field failed to
    /// build. Deep water with no bank is a far better failure than a shoreline in
    /// the wrong place.
    open_water: Handle<Image>,
    /// Mirror of `DataLayers::water_shading`, for the same reason
    /// `VegetationLod::enabled` exists — so the per-cell worker can read the
    /// toggle without `update_map_surfaces` taking another system parameter (it is
    /// at Bevy's ceiling of 16).
    ///
    /// What it gates is not the shader but the **shore field**: a 1024² rasterise
    /// plus an exact distance transform per water cell, ~30 ms on a workstation and
    /// ~9 MiB of texture across a ring. With shading off there are no waves to
    /// shelter and no crest foam to place, so the field buys only a shallow tint,
    /// and `WaterSeaState::material` already has a documented fallback for a cell
    /// that has no field at all.
    pub shading: bool,
}

impl FromWorld for WaterSeaState {
    fn from_world(world: &mut World) -> Self {
        let open_water =
            world
                .resource_mut::<Assets<Image>>()
                .add(shore_image(&[u8::MAX], 1, "open water"));
        let s = sea_state(UNKNOWN_WIND);
        Self {
            wave: s.0,
            surface: s.1,
            wind: s.2,
            open_water,
            // Matches `DataLayers::water_shading`'s own default, which is the
            // tier's `detail_layers`. `update_water_waves` corrects it on the first
            // frame either way; starting from the tier means a phone does not
            // build one ring of shore fields before that happens.
            shading: crate::quality::budget().detail_layers,
        }
    }
}

impl WaterSeaState {
    /// The material for one streamed cell: the shared sea state, plus this cell's
    /// own shore field and the frame to sample it in.
    pub fn material(
        &self,
        shore: Option<Handle<Image>>,
        origin: Vec2,
        tile_m: f32,
    ) -> WaterMaterial {
        WaterMaterial {
            base: StandardMaterial {
                base_color: WATER_ALBEDO,
                // Overwritten per fragment by the shader; this is just the
                // fallback for the prepass, which runs the standard pipeline.
                perceptual_roughness: 0.06,
                metallic: 0.0,
                // Water's F0 is ~0.02, half the dielectric default — the bright
                // part of a water surface is the grazing fresnel, not the head-on
                // reflection, and overstating F0 gives it a plastic sheen.
                reflectance: 0.25,
                // Same coplanar-ground ordering the other surface groups use; see
                // `SurfaceGroup::depth_bias`.
                depth_bias: crate::systems::vector_tiles::SurfaceGroup::Water.depth_bias(),
                ..default()
            },
            extension: WaterExtension {
                wave: self.wave,
                surface: self.surface,
                wind: self.wind,
                field: Vec4::new(origin.x, origin.y, tile_m, vector_tiles::SHORE_MAX_DIST_M),
                shore: shore.unwrap_or_else(|| self.open_water.clone()),
            },
        }
    }
}

/// Upload a shore field as a single-channel texture.
///
/// `R8Unorm` (not sRGB — this is a distance, not a colour), linearly filtered so
/// the ~1.5 m texel grid does not show up as stair-stepping along a bank, and
/// clamped at the edges so a cell's last row of texels extends outwards rather
/// than wrapping a far shore in from the opposite side.
pub fn shore_field_image(field: &ShoreField) -> Image {
    shore_image(&field.data, field.size, "shore field")
}

fn shore_image(data: &[u8], size: u32, label: &str) -> Image {
    use bevy::asset::RenderAssetUsages;
    use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
    use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

    let mut image = Image::new(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data.to_vec(),
        TextureFormat::R8Unorm,
        // `RENDER_WORLD` only, exactly as `tile_loader::TILE_TEXTURE_USAGE` argues:
        // a shore field is rasterised on a worker, uploaded, and thereafter only
        // sampled by `water.wgsl` — nothing reads a texel back. The default keeps
        // the decoded bytes in `Assets<Image>` as well, and this is the one texture
        // there is **one of per water cell** (1024² R8 = 1 MiB each), so over a
        // canal city the duplicate is the largest image cost in the app.
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        label: Some(label.into()),
        ..ImageSamplerDescriptor::linear()
    });
    image
}

/// The sea state a given wind speed produces, as (`wave`, `surface`, `wind`).
///
/// Wind does four things at once, and they have to move together or the result
/// reads wrong: it lengthens the dominant wave, steepens it, speeds it up, and
/// past [`FOAM_ONSET_WIND`] starts tearing the crests into whitecaps. A calm
/// canal keeps a fine, almost flat ripple; a gale gets long, sharp, fast swell.
fn sea_state(wind_ms: f32) -> (Vec4, Vec4, Vec4) {
    let sea = (wind_ms / FULL_SEA_WIND).clamp(0.0, 1.0);
    let foam = ((wind_ms - FOAM_ONSET_WIND) / (FULL_SEA_WIND - FOAM_ONSET_WIND)).clamp(0.0, 1.0);
    let dir = CLOUD_WIND.normalize_or(Vec2::X);
    (
        Vec4::new(
            // Steepness: a flat sheen when still, a properly rippled surface in wind.
            0.06_f32.lerp(0.55, sea),
            // Dominant wavelength grows with fetch — short chop → long swell.
            6.0_f32.lerp(26.0, sea),
            0.55_f32.lerp(2.2, sea),
            // Detail cut-off is a constant: it is about pixel size, not weather.
            3.0e-4,
        ),
        Vec4::new(
            0.04,
            0.30,
            0.40,
            // Whitecaps only, not a foamy soup — this rides on top of the crest
            // mask in the shader, which is already narrow.
            0.45 * foam,
        ),
        Vec4::new(dir.x, dir.y, sea, 0.0),
    )
}

/// Drive the sea state from the live weather's wind speed.
///
/// Every live cell's material is rewritten, not one shared handle — but only when
/// the wind has actually moved, which is a handful of times an hour.
pub fn update_water_waves(
    weather: Res<WeatherState>,
    dl: Res<crate::systems::data_layers::DataLayers>,
    mut state: ResMut<WaterSeaState>,
    mut materials: ResMut<Assets<WaterMaterial>>,
    mut applied: Local<Option<(f32, bool)>>,
) {
    // The "Water shading" toggle (`DataLayers::water_shading`) flattens the sea to a
    // still sheet: hold the wind at calm and zero the wave steepness + foam. New
    // cells read `state.wave`, so they come up flat too.
    let shading = dl.water_shading;
    let wind_ms = if shading {
        weather.latest.map_or(UNKNOWN_WIND, |o| o.wind_ms).max(0.0)
    } else {
        0.0
    };
    if applied.is_some_and(|(prev_wind, prev_shading)| {
        (prev_wind - wind_ms).abs() < WIND_EPSILON && prev_shading == shading
    }) {
        return;
    }
    let (mut wave, mut surface, wind) = sea_state(wind_ms);
    if !shading {
        wave.x = 0.0; // steepness → no ripples in the normal
        surface.w = 0.0; // no crest foam
    }
    state.wave = wave;
    state.surface = surface;
    state.wind = wind;
    // Read by `map_geometry`'s per-cell worker to decide whether to build a shore
    // field at all; `MapSurfaces` retears the ring when it changes.
    state.shading = shading;

    let ids: Vec<_> = materials.ids().collect();
    for id in ids {
        if let Some(mut material) = materials.get_mut(id) {
            material.extension.wave = wave;
            material.extension.surface = surface;
            material.extension.wind = wind;
        }
    }
    *applied = Some((wind_ms, shading));
    tracing::debug!(wind_ms, shading, "water sea state updated");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wind_direction_is_a_unit_vector() {
        // The shader takes `wind.xy` as a direction and does not normalise it —
        // a stray length would scale every octave's frequency with it.
        let (_, _, wind) = sea_state(UNKNOWN_WIND);
        assert!((Vec2::new(wind.x, wind.y).length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn wind_raises_steepness_wavelength_and_speed_together() {
        // The wind-driven knobs must move in the same direction; a longer wave
        // at a lower steepness would read as a flat sheet, not a sea.
        let (calm_wave, calm_surface, calm_wind) = sea_state(0.0);
        let (gale_wave, gale_surface, gale_wind) = sea_state(FULL_SEA_WIND);
        assert!(gale_wave.x > calm_wave.x, "steepness");
        assert!(gale_wave.y > calm_wave.y, "wavelength");
        assert!(gale_wave.z > calm_wave.z, "speed");
        assert!(gale_surface.w > calm_surface.w, "foam");
        assert!(gale_wind.z > calm_wind.z, "chop");
    }

    #[test]
    fn foam_only_appears_above_the_onset_wind() {
        assert_eq!(sea_state(FOAM_ONSET_WIND - 1.0).1.w, 0.0);
        assert!(sea_state(FOAM_ONSET_WIND + 1.0).1.w > 0.0);
    }

    #[test]
    fn a_storm_does_not_run_off_the_end_of_the_ramps() {
        // Wind speeds well past `FULL_SEA_WIND` happen; the shaping must
        // saturate rather than extrapolate into an unrenderable sea.
        let gale = sea_state(FULL_SEA_WIND);
        let storm = sea_state(FULL_SEA_WIND * 4.0);
        assert_eq!(storm.0, gale.0);
        assert_eq!(storm.1, gale.1);
    }

    #[test]
    fn a_shore_field_image_is_single_channel_and_unfiltered_by_srgb() {
        use bevy::render::render_resource::TextureFormat;

        let field = ShoreField {
            size: 2,
            data: vec![0, 64, 128, 255],
        };
        let image = shore_field_image(&field);
        // R8Unorm, not R8UnormSrgb: the texels are metres, and an sRGB decode
        // would bend the distance scale into a curve.
        assert_eq!(image.texture_descriptor.format, TextureFormat::R8Unorm);
        assert_eq!(image.texture_descriptor.size.width, 2);
    }
}
