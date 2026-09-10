//! Water material for the vector-tile surface meshes (ADR-035, option B).
//!
//! `map_geometry` spawns one batched mesh per [`SurfaceGroup`]; every group but
//! water gets a plain `StandardMaterial`. Water gets this one: a
//! [`MaterialExtension`] over `StandardMaterial` whose fragment shader
//! (`assets/shaders/water.wgsl`) replaces the flat +Y normal with an animated
//! wave field, trades distance-faded ripple detail for roughness, tints the
//! albedo by fresnel so the surface darkens looking down and brightens at
//! grazing angles, and adds the two things the standard path has no notion of —
//! light scattering through a crest, and foam.
//!
//! # The wave field is a measurement now, not a taste
//!
//! `wave.x` used to be a "steepness": a number with no units, picked by looking
//! at the result, and constrained less by what water does than by how much of it
//! the renderer could show before the sub-pixel ripples started to sparkle. It is
//! the field's **RMS slope** now, taken straight off [`SLOPE_VAR_CALM`]'s
//! Cox–Munk relation, and the shader's octave sum is normalised by
//! [`octave_norms`] so a slope asked for is the slope rendered.
//!
//! Two changes on the shader side had to come first, and the notes over there
//! carry the argument: the detail the distance fade retires comes back as GGX
//! roughness derived from the retired octaves' own variance (so a steep sea no
//! longer has to be flattened to stop it aliasing), and the octave loop stops at
//! the first fully-faded octave (so the finer octaves the steeper sea wants are
//! paid for only within the few tens of metres where they can be seen).
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

use crate::systems::sky::CLOUD_WIND;
use crate::systems::vector_tiles::{self, ShoreField};
use crate::systems::weather::WeatherState;

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
///
/// **Sample the paint from the same frame, not from its constant.** The palette
/// entry is an albedo too — it is painted into a tile texture and then lit — so
/// comparing this surface's *render* against that constant's *value* compares two
/// different spaces and lets a step open up that neither number admits to. That
/// is exactly what had happened by 2026-08-20; see the palette entry for the
/// measurement.
///
/// # Neither end of this can close the gap alone, and that is measured
///
/// The reconciliation has to be split across both constants, because each one is
/// a weak lever on the thing being matched and a strong lever on the thing it
/// exists for. Sampled at 53.07,5.34 from 12 km, ring water against the paint
/// beside it in the same frame:
///
/// | change | rendered step (R,G,B levels) |
/// |---|---|
/// | as it stood | 23, 24, 16 |
/// | paint `#3E5A70` → `#2C4862` | 12, 14, 12 |
/// | + this `0.06,0.16,0.21` → `0.12,0.24,0.30` | **8, 9, 9** |
///
/// **Doubling this constant moved the render by four levels.** That is not a
/// disappointment, it is the design working: the paragraph above says most of
/// what you see is reflected, and a surface dominated by its reflection barely
/// responds to its albedo. Reaching the paint from this side alone would need a
/// pale-pond albedo that the fresnel would then blow out at grazing angles.
/// Reaching it from the paint's side alone is worse and is a hard stop, not a
/// judgement: the required ink is near-black, and `tile_loader::WATER_BLUE_BIAS`
/// needs the paint at least 40 bluer than it is red or the globe's raster oceans
/// classify as land. So both moved, modestly, and both stayed inside what they
/// are for.
///
/// The residual 8–9 levels are not closable by constants at all — the surface
/// reflects *this* sky and the paint does not, so any single pair of numbers is
/// matched at one time of day and drifts at every other. Closing it properly
/// means the ring's edge stops being a hard line (fading the water surface out
/// over its last cell), which is a change to the surface build and the shader,
/// not to a colour.
const WATER_ALBEDO: Color = Color::srgb(0.12, 0.24, 0.30);

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

/// Cox & Munk's sun-glitter measurement of sea-surface slope, 1954: the variance
/// of the surface slope over a clean sea is `0.003 + 0.00512·U`, with `U` the wind
/// speed in m/s. It is the classic result and it is still what ocean-optics work
/// uses, because it was taken the only way this quantity can be — by photographing
/// the sun's glitter path from an aircraft and inverting the width of it.
///
/// The shader's octave sum is normalised so `wave.x` *is* the RMS slope, which is
/// what lets this replace the three hand-picked steepness numbers that used to
/// live here. Two things had to be true first, and now are. The sum has a real
/// normaliser (`octave_norms`), so a slope asked for is a slope rendered; and the
/// detail the distance fade retires comes back as roughness rather than as a
/// flattened sea, so a steep surface no longer has to be held down to stop it
/// aliasing (`water.wgsl` has that argument in full).
///
/// The numbers this yields, against what stood here before: dead calm 0.055 where
/// the old constant rendered ≈0.012, and Beaufort 6 0.254 against ≈0.11. The old
/// sea was roughly half as steep as the measured one everywhere, and four times
/// too flat when the wind dropped — which is exactly the reading of "the water
/// looks cheap", since calm water is most of the water in a canal city.
const SLOPE_VAR_CALM: f32 = 0.003;
const SLOPE_VAR_PER_WIND: f32 = 0.00512;

/// Per-octave amplitude roll-off. Must match `AMP_FALLOFF` in `water.wgsl`; the
/// normalisers below are the series that shape it, and the shader walks the same
/// series one octave at a time.
const AMP_FALLOFF: f32 = 0.72;

/// Octaves the shader counts as "the long ones" (`LO_OCTAVES` over there). The
/// short band starts after them and is what `octave_norms`' `w` normalises.
const LO_OCTAVES: u32 = 2;

/// Mean square of one octave's shaped derivative, as a quadratic in the chop —
/// the same three coefficients `water.wgsl` evaluates, integrated numerically
/// over `d/dφ mix(s², s⁵, chop)`. It is the conversion factor between the octave
/// amplitudes and a real slope variance, so it belongs on both sides: here to
/// build the normaliser, there to turn the retired octaves back into roughness.
///
/// A quadratic rather than a lerp between the endpoints because the mean square
/// of a blend is not the blend of the mean squares: a straight lerp is 8.5 % high
/// in the middle of the range, which is a 4 % error in the rendered slope.
const SLOPE_MS_A: f32 = 0.1563;
const SLOPE_MS_B: f32 = 0.0097;
const SLOPE_MS_C: f32 = 0.0658;

/// Mean square of one octave's shaped derivative at the given chop.
fn slope_mean_square(chop: f32) -> f32 {
    let c = chop.clamp(0.0, 1.0);
    SLOPE_MS_A + c * (SLOPE_MS_B + c * SLOPE_MS_C)
}

/// Share of the measured slope variance this octave stack puts in the **normal**.
/// The rest is not discarded — it becomes base roughness, which is the same split
/// the distance fade already makes and the reason it can be made at all.
///
/// Two things force a split. Cox & Munk measured the *whole* surface, down to the
/// capillary ripples that carry most of a sea's slope variance and that no normal
/// map at this scale can hold; these octaves span roughly 0.4 m to 26 m. And a sum
/// of eight sines simply cannot carry a full sea's slope: handed the whole 0.254
/// of Beaufort 6, the open water in a 2026-09-10 capture over the
/// Paterswoldsemeer read as corduroy crossed by a standing moiré, which is a
/// worse artefact than the flat sheet it replaced.
///
/// So the number is chosen, not measured — but it is bounded above by a
/// measurement, and what it excludes is accounted for rather than dropped. At a
/// quarter of the variance the normal field carries an RMS slope of 0.5·σ, which
/// at the top of the range lands near the old hand-tuned steepness: the old
/// number was a reasonable guess at the *resolvable* band. What it was missing is
/// the other three quarters, which used to be nothing at all and is now the width
/// of the specular lobe.
const RESOLVED_SLOPE_VARIANCE: f32 = 0.25;

/// GGX width of water's own surface, with every wave removed — the roughness the
/// [`RESOLVED_SLOPE_VARIANCE`] split is added on top of.
const MICRO_ALPHA: f32 = 0.0016;

/// The RMS slope of the whole sea at a given wind speed, per [`SLOPE_VAR_CALM`].
///
/// Saturating at [`FULL_SEA_WIND`] rather than running on: past Beaufort 6 the
/// thing that changes is the wave *height*, which this surface has none of, and
/// extrapolating the slope instead would only make the normal noisier.
pub fn rms_slope(wind_ms: f32) -> f32 {
    let u = wind_ms.clamp(0.0, FULL_SEA_WIND);
    (SLOPE_VAR_CALM + SLOPE_VAR_PER_WIND * u).sqrt()
}

/// The two halves of [`rms_slope`]: the RMS slope the wave field renders as a
/// normal, and the perceptual roughness the rest of it comes back as.
///
/// Adding a variance to `α²` is exactly what the shader does with the octaves the
/// distance fade retires (LEAN/Toksvig), so the unresolved band and the
/// out-of-range band arrive by the same route and cannot double-count.
pub fn slope_split(wind_ms: f32) -> (f32, f32) {
    let var = rms_slope(wind_ms).powi(2);
    let resolved = (var * RESOLVED_SLOPE_VARIANCE).sqrt();
    let alpha = (MICRO_ALPHA * MICRO_ALPHA + var * (1.0 - RESOLVED_SLOPE_VARIANCE)).sqrt();
    // bevy's `roughness` is `perceptual_roughness²`, so this is the inverse.
    (resolved, alpha.sqrt())
}

/// The four numbers that describe the shape of an `n`-octave sum to the shader.
///
/// * `x` — the octave count, which the fragment loop reads as its bound.
/// * `y` — the gradient normaliser. Each octave contributes a slope whose mean
///   square is `slope_ms · amp²`, and the octaves point in different enough
///   directions to add as variances, so the sum's RMS magnitude is
///   `√(slope_ms · Σ amp²)`. Dividing by it makes `wave.x` mean RMS slope, in the
///   units [`rms_slope`] answers in.
/// * `z` — `1 / Σ amp`, which turns the tally of octaves that survived the
///   distance fade into a 0..1 "how much detail is left".
/// * `w` — the same for the short octaves alone, which is what the chop band of
///   the foam mask is measured against.
///
/// Closed forms rather than a loop, and on the CPU rather than in the fragment,
/// for the reason the shader's early-out needs: the *whole* series has to be known
/// even when the loop stops after two octaves, or the normalisation would change
/// with view distance and put a ring of brightness around the camera.
pub fn octave_norms(count: u32, chop: f32) -> Vec4 {
    let n = count.max(1);
    let r = AMP_FALLOFF;
    let sum = |ratio: f32, terms: u32| (1.0 - ratio.powi(terms as i32)) / (1.0 - ratio);
    let sum_amp = sum(r, n);
    let sum_sq = sum(r * r, n);
    // The long band is the first `LO_OCTAVES`; everything after it is the chop.
    let sum_hi = sum_amp - sum(r, n.min(LO_OCTAVES));
    let slope_ms = slope_mean_square(chop);
    Vec4::new(
        n as f32,
        1.0 / (slope_ms * sum_sq).sqrt(),
        1.0 / sum_amp,
        1.0 / sum_hi.max(1e-4),
    )
}

/// The material `map_geometry` uses for [`SurfaceGroup::Water`].
///
/// [`SurfaceGroup::Water`]: crate::systems::vector_tiles::SurfaceGroup::Water
pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterExtension>;

/// Wave/surface uniforms for `shaders/water.wgsl`.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
pub struct WaterExtension {
    /// x: RMS slope of the wave field (see [`rms_slope`]). y: base wavelength (m).
    /// z: wave speed scalar (m/s). w: detail cut-off (per metre of view range).
    #[uniform(100)]
    pub wave: Vec4,
    /// x: base (micro) roughness, perceptual. y: crest sub-surface scattering.
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
    /// The shape of the octave sum, all of it from [`octave_norms`]: x = how many
    /// octaves to run, y = the gradient normaliser that makes `wave.x` an RMS
    /// slope, z = 1/Σ amplitude, w = 1/Σ amplitude over the short octaves.
    ///
    /// It is a uniform rather than four shader constants because the octave count
    /// is a quality rung ([`crate::quality::Budget::water_octaves`]) and because
    /// the series arithmetic behind y and z is worth having on the CPU, where a
    /// test can read it.
    #[uniform(106)]
    pub octaves: Vec4,
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
    /// Current wind-driven uniforms (`wave`, `surface`, `wind`, `octaves`).
    wave: Vec4,
    surface: Vec4,
    wind: Vec4,
    octaves: Vec4,
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
        let s = sea_state(UNKNOWN_WIND, crate::quality::budget().water_octaves);
        Self {
            wave: s.0,
            surface: s.1,
            wind: s.2,
            octaves: s.3,
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
                // Bevy's `reflectance` is not F0 — `F0 = 0.16 · reflectance²`.
                // Water's F0 is `((n-1)/(n+1))²` at n = 1.333, i.e. 0.0204, so
                // the reflectance that produces it is √(0.0204 / 0.16). The 0.25
                // that stood here was aimed at 0.02 and landed on **0.01**, which
                // is half of water's reflectance — and the reflection is very
                // nearly all of what a water surface is. `water.wgsl` uses the
                // same 0.0204 for the fresnel it does itself, so the body colour
                // and the reflection cannot disagree about where the light went.
                reflectance: 0.3571,
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
                octaves: self.octaves,
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

/// The sea state a given wind speed produces, as (`wave`, `surface`, `wind`,
/// `octaves`).
///
/// Wind does four things at once, and they have to move together or the result
/// reads wrong: it lengthens the dominant wave, steepens it, speeds it up, and
/// past [`FOAM_ONSET_WIND`] starts tearing the crests into whitecaps. A calm
/// canal keeps a fine, almost flat ripple; a gale gets long, sharp, fast swell.
fn sea_state(wind_ms: f32, octave_count: u32) -> (Vec4, Vec4, Vec4, Vec4) {
    let sea = (wind_ms / FULL_SEA_WIND).clamp(0.0, 1.0);
    let foam = ((wind_ms - FOAM_ONSET_WIND) / (FULL_SEA_WIND - FOAM_ONSET_WIND)).clamp(0.0, 1.0);
    let dir = CLOUD_WIND.normalize_or(Vec2::X);
    let (slope, base_roughness) = slope_split(wind_ms);
    (
        Vec4::new(
            // RMS slope of the band the octaves can resolve — the shader's sum is
            // normalised so this number is rendered as written. It used to be a
            // hand-picked "steepness"; the notes on [`SLOPE_VAR_CALM`] and
            // [`RESOLVED_SLOPE_VARIANCE`] have the measurement and the split.
            slope,
            // Dominant wavelength grows with fetch — short chop → long swell.
            6.0_f32.lerp(26.0, sea),
            0.55_f32.lerp(2.2, sea),
            // Detail cut-off is a constant: it is about pixel size, not weather.
            3.0e-4,
        ),
        Vec4::new(
            // Water's own micro-roughness *plus* the slope variance the octaves
            // cannot hold — the other side of the split above. This is what makes
            // a calm surface a soft sheen instead of a mirror, and a mirror is
            // most of what reads as computer-generated water.
            base_roughness,
            // Crest scattering, as an effective albedo. It rides the wind because
            // it needs a wave with a thin sunlit edge to shine through, and never
            // quite reaches zero because a low sun finds the smallest swell.
            //
            // **Deliberately set low enough to err toward invisible.** The shader
            // adds this *after* `apply_pbr_lighting`, so nothing downstream will
            // hold it in range, and its magnitude is the one number here that a
            // software rasteriser cannot settle — colour is not evidence on this
            // container. Two captures over the Paterswoldsemeer bounded it from
            // above: at 0.55 it was a lattice of blown-out cyan crests across the
            // lake, and at 0.09 it still pulled the brightest water 18 levels
            // toward green. An effect that is too faint reads as the term not
            // being there; one that is too strong reads as a bug. **Worth a
            // workstation's eye before it is raised.**
            0.012_f32.lerp(0.045, sea),
            0.40,
            // Whitecaps only, not a foamy soup — this rides on top of the crest
            // mask in the shader, which is already narrow.
            0.45 * foam,
        ),
        Vec4::new(dir.x, dir.y, sea, 0.0),
        // Chop sharpens the crests, which changes how much slope one octave is
        // worth — so the normaliser is part of the sea state, not a constant.
        octave_norms(octave_count, sea),
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
    let (mut wave, mut surface, wind, octaves) =
        sea_state(wind_ms, crate::quality::budget().water_octaves);
    if !shading {
        wave.x = 0.0; // RMS slope → no ripples in the normal
        surface.w = 0.0; // no crest foam
        surface.y = 0.0; // and no crest to scatter light through
    }
    state.wave = wave;
    state.surface = surface;
    state.wind = wind;
    state.octaves = octaves;
    // Read by `map_geometry`'s per-cell worker to decide whether to build a shore
    // field at all; `MapSurfaces` retears the ring when it changes.
    state.shading = shading;

    let ids: Vec<_> = materials.ids().collect();
    for id in ids {
        if let Some(mut material) = materials.get_mut(id) {
            material.extension.wave = wave;
            material.extension.surface = surface;
            material.extension.wind = wind;
            material.extension.octaves = octaves;
        }
    }
    *applied = Some((wind_ms, shading));
    tracing::debug!(wind_ms, shading, "water sea state updated");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Octave count used by the tests that only care about the wind ramps.
    const OCTAVES: u32 = 8;

    #[test]
    fn the_wind_direction_is_a_unit_vector() {
        // The shader takes `wind.xy` as a direction and does not normalise it —
        // a stray length would scale every octave's frequency with it.
        let (_, _, wind, _) = sea_state(UNKNOWN_WIND, OCTAVES);
        assert!((Vec2::new(wind.x, wind.y).length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn wind_raises_slope_wavelength_and_speed_together() {
        // The wind-driven knobs must move in the same direction; a longer wave
        // at a lower slope would read as a flat sheet, not a sea.
        let (calm_wave, calm_surface, calm_wind, _) = sea_state(0.0, OCTAVES);
        let (gale_wave, gale_surface, gale_wind, _) = sea_state(FULL_SEA_WIND, OCTAVES);
        assert!(gale_wave.x > calm_wave.x, "slope");
        assert!(gale_wave.y > calm_wave.y, "wavelength");
        assert!(gale_wave.z > calm_wave.z, "speed");
        assert!(gale_surface.w > calm_surface.w, "foam");
        assert!(gale_surface.y > calm_surface.y, "crest scattering");
        assert!(gale_wind.z > calm_wind.z, "chop");
    }

    #[test]
    fn the_shaped_slope_curve_is_exact_at_both_ends() {
        // The two endpoints are integrals, not fits: `d/dφ s²` and `d/dφ s⁵`
        // have mean squares 0.1563 and 0.2318. The quadratic exists only to
        // carry the blend between them, so if it has drifted off either end the
        // rendered slope no longer matches `rms_slope` at the wind speeds that
        // matter most — dead calm and a full gale.
        assert!((slope_mean_square(0.0) - 0.1563).abs() < 1e-6, "swell");
        assert!((slope_mean_square(1.0) - 0.2318).abs() < 1e-6, "chop");
        // And it must stay between them: a blend of two shapes cannot be
        // steeper than the steeper one.
        for i in 0..=10 {
            let c = i as f32 / 10.0;
            let m = slope_mean_square(c);
            assert!((0.1563..=0.2318).contains(&m), "chop {c} gave {m}");
        }
    }

    #[test]
    fn the_reflectance_produces_waters_own_f0() {
        // Bevy computes `F0 = 0.16 · reflectance²`, and `water.wgsl` does its own
        // Schlick against a hard-coded 0.0204 — so if these two ever disagree the
        // body colour is taken out at one reflectance and put back at another,
        // and the surface stops conserving light without saying so.
        let material = WaterSeaState {
            wave: Vec4::ZERO,
            surface: Vec4::ZERO,
            wind: Vec4::ZERO,
            octaves: Vec4::ZERO,
            open_water: Handle::default(),
            shading: true,
        }
        .material(None, Vec2::ZERO, 1.0);
        let f0 = 0.16 * material.base.reflectance * material.base.reflectance;
        assert!(
            (f0 - 0.0204).abs() < 1e-4,
            "reflectance {} gives F0 {f0}, water is 0.0204",
            material.base.reflectance
        );
    }

    #[test]
    fn the_split_conserves_the_measured_variance() {
        // Nothing is allowed to vanish between the measurement and the surface:
        // whatever the normal field does not carry has to arrive as roughness, or
        // the sea is quietly flatter than Cox & Munk says it is — which is the
        // bug the whole split exists to avoid. Recombining is the shader's own
        // arithmetic run backwards: perceptual² is α, and α² is a variance.
        for wind in [0.0, 3.0, 7.0, FULL_SEA_WIND] {
            let (slope, perceptual) = slope_split(wind);
            let alpha = perceptual * perceptual;
            let recovered = slope * slope + alpha * alpha - MICRO_ALPHA * MICRO_ALPHA;
            let measured = rms_slope(wind).powi(2);
            assert!(
                (recovered - measured).abs() < 1e-6,
                "wind {wind}: {recovered} recovered from a measured {measured}"
            );
        }
    }

    #[test]
    fn calm_water_is_a_sheen_and_not_a_mirror() {
        // The old constant was a flat 0.04 perceptual at every wind speed, which
        // is a near-mirror — and a mirror is most of what reads as computer
        // graphics rather than as water. Dead calm must still be visibly rougher
        // than that, and a gale rougher again.
        let (_, calm) = slope_split(0.0);
        let (_, gale) = slope_split(FULL_SEA_WIND);
        assert!(calm > 0.15, "dead calm perceptual roughness {calm}");
        assert!(gale > calm, "{calm} → {gale}");
        assert!(gale < 0.8, "a gale is still water, not concrete: {gale}");
    }

    #[test]
    fn the_sea_slope_is_the_measured_one() {
        // `wave.x` is the rendered RMS slope directly (`water.wgsl` normalises
        // its octave sum to it), so this is the one place the Cox–Munk relation
        // can be checked against what the shader will actually be handed.
        // Guarding the numbers, not just the direction: an earlier version of
        // this file kept a sibling test that only asserted "gale steeper than
        // calm", and it stayed green while an extra wavenumber in the shader
        // cancelled the ramp and the sea rendered as a flat sheet.
        assert!((rms_slope(0.0) - 0.0548).abs() < 1e-3, "dead calm");
        assert!(
            (rms_slope(UNKNOWN_WIND) - 0.1357).abs() < 1e-3,
            "light breeze"
        );
        assert!(
            (rms_slope(FULL_SEA_WIND) - 0.2542).abs() < 1e-3,
            "Beaufort 6"
        );
        // And what reaches the shader is the resolvable half of it.
        let calm = sea_state(0.0, OCTAVES).0.x;
        let gale = sea_state(FULL_SEA_WIND, OCTAVES).0.x;
        assert!(
            (calm - 0.0274).abs() < 1e-3,
            "dead calm rendered slope {calm}"
        );
        assert!(
            (gale - 0.1271).abs() < 1e-3,
            "Beaufort 6 rendered slope {gale}"
        );
        assert!(
            gale / calm >= 4.0,
            "calm {calm} → gale {gale} must stay a real ramp"
        );
    }

    #[test]
    fn foam_only_appears_above_the_onset_wind() {
        assert_eq!(sea_state(FOAM_ONSET_WIND - 1.0, OCTAVES).1.w, 0.0);
        assert!(sea_state(FOAM_ONSET_WIND + 1.0, OCTAVES).1.w > 0.0);
    }

    #[test]
    fn a_storm_does_not_run_off_the_end_of_the_ramps() {
        // Wind speeds well past `FULL_SEA_WIND` happen; the shaping must
        // saturate rather than extrapolate into an unrenderable sea.
        let gale = sea_state(FULL_SEA_WIND, OCTAVES);
        let storm = sea_state(FULL_SEA_WIND * 4.0, OCTAVES);
        assert_eq!(storm.0, gale.0);
        assert_eq!(storm.1, gale.1);
    }

    /// Sum the octave series the way the fragment shader walks it, and report the
    /// RMS magnitude of the gradient it would produce for `wave.x == 1`.
    ///
    /// This is the shader's loop with the trigonometry replaced by its own mean
    /// square: each octave adds `amp²·slope_ms` of slope variance, and they add as
    /// variances because the golden-angle turn keeps their directions apart.
    fn rendered_rms_slope(count: u32, chop: f32) -> f32 {
        let norms = octave_norms(count, chop);
        let slope_ms = slope_mean_square(chop);
        let mut amp = 1.0_f32;
        let mut var = 0.0_f32;
        for _ in 0..count {
            var += amp * amp * slope_ms;
            amp *= AMP_FALLOFF;
        }
        var.sqrt() * norms.y
    }

    #[test]
    fn the_octave_sum_renders_the_slope_it_was_asked_for() {
        // The whole point of `octave_norms`: `wave.x` is a physical RMS slope
        // only if the normaliser actually cancels the series. Checked at both
        // ends of the chop range, because the per-octave slope depends on how
        // sharp the crests are.
        for chop in [0.0, 0.5, 1.0] {
            for count in [4, 6, 8] {
                let rendered = rendered_rms_slope(count, chop);
                assert!(
                    (rendered - 1.0).abs() < 1e-4,
                    "{count} octaves at chop {chop} rendered {rendered}× the asked-for slope"
                );
            }
        }
    }

    #[test]
    fn changing_the_octave_count_does_not_change_the_sea() {
        // The quality rung picks the octave count, and a rung must buy detail —
        // not a different sea. If the normaliser did not depend on the count, a
        // phone would render a visibly flatter (or steeper) surface than a
        // workstation looking at the same weather, which is a bug that only ever
        // shows up on the hardware nobody is testing on.
        let four = rendered_rms_slope(4, 0.5);
        let eight = rendered_rms_slope(8, 0.5);
        assert!((four - eight).abs() < 1e-4, "{four} vs {eight}");
    }

    #[test]
    fn the_detail_fractions_reach_exactly_one() {
        // `octaves.z` and `.w` are what the shader divides its surviving-amplitude
        // tallies by, so with nothing faded they must come out at 1.0 — a
        // normaliser that is merely close puts a permanent haze of "detail is
        // missing" over water right in front of the camera.
        for count in [4_u32, 6, 8] {
            let norms = octave_norms(count, 0.5);
            let mut amp = 1.0_f32;
            let (mut all, mut hi) = (0.0_f32, 0.0_f32);
            for i in 0..count {
                all += amp;
                if i >= LO_OCTAVES {
                    hi += amp;
                }
                amp *= AMP_FALLOFF;
            }
            assert!((all * norms.z - 1.0).abs() < 1e-5, "all: {count} octaves");
            assert!((hi * norms.w - 1.0).abs() < 1e-5, "short: {count} octaves");
        }
    }

    #[test]
    fn a_single_octave_is_still_normalisable() {
        // `octave_norms` is handed a quality rung's number, and the lowest rung is
        // free to move. One octave leaves the short band empty, and a division by
        // that empty sum would arrive in the shader as an inf.
        let norms = octave_norms(1, 0.0);
        assert!(norms.y.is_finite() && norms.y > 0.0);
        assert!(norms.z.is_finite() && norms.w.is_finite());
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
