#define_import_path cartopolis::water_common

// What a sheet of water looks like with its waves taken away: the fresnel split
// between the body colour and the reflection, and the stand-in sky it reflects on
// a view with no environment map.
//
// Two shaders draw water and both import this. `water.wgsl` is the streamed
// surface around the camera, with its waves, shore field and foam; past the edge
// of that ring the ground is the clipmap's flat tiles (`ground_detail.wgsl`),
// whose water texels were once a *painted* colour matched to the surface by hand.
// Now they are lit here too, from the same sea state — so the two sides of the
// ring agree by construction, and a change to the water reaches the horizon
// without anything to re-match. See `docs/notes/water-surface.md`.

#import bevy_pbr::{
    mesh_view_bindings::{view, lights},
    // The module as well, for the bindings that exist only under a shader def
    // (the fog, the environment map) — the reason `water.wgsl` gives.
    mesh_view_bindings as view_bindings,
    pbr_functions::calculate_F0,
    pbr_types::PbrInput,
    lighting::{EnvBRDFApprox, F_AB, perceptualRoughnessToRoughness},
}

/// Water's reflectance at normal incidence, from its refractive index:
/// `((1.333 - 1) / (1.333 + 1))²`. The one number the whole surface turns on —
/// nearly everything you see looking at water is this times the sky.
const WATER_F0: f32 = 0.0204;

/// The clear sky overhead as a multiple of the haze colour at the horizon, for
/// the reflection a view without an environment map stands in. Linear. The
/// haze's day colour `#B5BFD4` becomes a zenith of roughly `#779BDB`: ~60 % of the
/// horizon's luminance and clearly blue, which is the shape of a real clear sky —
/// darker and more saturated overhead, pale and bright along the horizon.
const SKY_ZENITH_TINT: vec3<f32> = vec3<f32>(0.40, 0.63, 1.08);

/// Where a fully rough surface lands between horizon (0) and zenith (1): the
/// cosine-weighted mean of `sqrt(elevation sine)` over the upper hemisphere,
/// 2·∫₀¹ √y · y dy = 0.8.
const SKY_MEAN_T: f32 = 0.8;

/// Schlick against water's own F0: the fraction of the light reaching the eye
/// that is reflection rather than body. Written out as multiplies: the exponent
/// is a constant 5, and `pow` is `exp2(y·log2(x))` on every adapter there is.
fn water_reflected(n_dot_v: f32) -> f32 {
    let grazing = 1.0 - clamp(n_dot_v, 0.0, 1.0);
    let grazing2 = grazing * grazing;
    return WATER_F0 + (1.0 - WATER_F0) * grazing2 * grazing2 * grazing;
}

/// **Light that was not reflected is light that went in**, so the body colour is
/// worth `1 - F` and nothing else — scaled so the head-on tone is unchanged,
/// which is the tone the deep water's colour was chosen at.
fn water_transmitted(reflected: f32) -> f32 {
    return (1.0 - reflected) / (1.0 - WATER_F0);
}

/// Where the reflected ray lands on the stand-in sky, 0 = horizon, 1 = zenith. A
/// rough surface sums the sky over a wide lobe, so it drifts toward the sky's
/// mean rather than picking out one elevation.
fn sky_t(ray_y: f32, perceptual_roughness: f32) -> f32 {
    let r = perceptual_roughness;
    return mix(sqrt(clamp(ray_y, 0.0, 1.0)), SKY_MEAN_T, r * r);
}

/// The stand-in sky at `t`: the haze colour at the horizon, `SKY_ZENITH_TINT` of
/// it overhead, greyed back to the haze by cloud (`clear` 0 = overcast).
fn sky_colour(horizon: vec3<f32>, clear: f32, t: f32) -> vec3<f32> {
    let zenith = mix(horizon, horizon * SKY_ZENITH_TINT, clamp(clear, 0.0, 1.0));
    return mix(horizon, zenith, t);
}

/// The weight bevy gives this surface's image-based specular — its split-sum
/// `F0·A + B` and the "pre-baked specular occlusion" it multiplies in below an
/// F0 of 0.02 — recomputed with bevy's own functions on bevy's own inputs. Both
/// the ambient and the environment map reach the surface through it.
fn specular_weight(pbr_input: PbrInput) -> vec3<f32> {
    let f0 = calculate_F0(
        pbr_input.material.base_color.rgb,
        pbr_input.material.metallic,
        pbr_input.material.reflectance,
    );
    let n_dot_v = max(dot(pbr_input.N, pbr_input.V), 0.0001);
    return EnvBRDFApprox(f0, F_AB(pbr_input.material.perceptual_roughness, n_dot_v))
        * saturate(dot(f0, vec3(50.0 * 0.33)));
}

/// Exactly the specular half of what bevy's `ambient.wgsl` adds for this
/// surface, recomputed with bevy's own functions on bevy's own inputs so that
/// taking it back out is exact rather than a tuned discount. Pre-exposed, like
/// the lit colour it is subtracted from.
fn ambient_specular(pbr_input: PbrInput) -> vec3<f32> {
    return specular_weight(pbr_input)
        * lights.ambient_color.rgb
        * pbr_input.diffuse_occlusion
        * view.exposure;
}

#ifdef ENVIRONMENT_MAP
/// The environment map's radiance along this surface's reflection, sampled
/// exactly as bevy's `environment_map::compute_radiances` samples it — the same
/// bent direction, the same mip for the roughness, the same intensity — and
/// pre-exposed like the lit colour. Zero when the view turns out to have no map,
/// which is bevy's own bail-out.
fn environment_radiance(pbr_input: PbrInput) -> vec3<f32> {
    let probes = view_bindings::light_probes.view_cubemap_index;
    if probes < 0 {
        return vec3<f32>(0.0);
    }
    let perceptual_roughness = pbr_input.material.perceptual_roughness;
    let roughness = perceptualRoughnessToRoughness(perceptual_roughness);
    let n = pbr_input.N;
    // Frostbite's bent reflection vector, as `radiance_sample_direction`.
    let smoothness = saturate(1.0 - roughness);
    let bend = smoothness * (sqrt(smoothness) + roughness);
    var dir = mix(n, reflect(-pbr_input.V, n), bend);
    // The map's own rotation (`quat_rotate`), then the cube map's left-handed z.
    let q = view_bindings::light_probes.view_rotation;
    let t = 2.0 * cross(q.xyz, dir);
    dir = dir + q.w * t + cross(q.xyz, t);
    dir.z = -dir.z;
    let level = perceptual_roughness
        * f32(view_bindings::light_probes.smallest_specular_mip_level_for_view);
#ifdef MULTIPLE_LIGHT_PROBES_IN_ARRAY
    let radiance = textureSampleLevel(
        view_bindings::specular_environment_maps[probes],
        view_bindings::environment_map_sampler,
        dir,
        level,
    ).rgb;
#else
    let radiance = textureSampleLevel(
        view_bindings::specular_environment_map,
        view_bindings::environment_map_sampler,
        dir,
        level,
    ).rgb;
#endif
    return radiance * view_bindings::light_probes.intensity_for_view * view.exposure;
}
#endif

/// Reflected fraction below which the clouds are not looked up at all; they are
/// at full strength from twice it. Water's own F0 is 0.0204, so this spares water
/// seen from well above — where the reflection is a few per cent of the sky and a
/// cloud in it a level or two of grey — and a quay's shadow in the mirror, and
/// foam. The streamed surface and the flat map past its ring apply it to the same
/// weight, so the cut-off cannot draw a line along the ring's edge.
const MIRROR_CLOUD_MIN: f32 = 0.03;

/// How much of the mirrored deck to keep at reflection weight `weight`: 0 means
/// do not look it up. A band rather than a cut, so no line marks where it stops.
fn cloud_mirror_fade(weight: f32) -> f32 {
    return smoothstep(MIRROR_CLOUD_MIN, 2.0 * MIRROR_CLOUD_MIN, weight);
}

/// Puts the cloud deck into the reflection. `cloud` is the deck along the
/// reflected ray (`cloud_field::mirrored`): where it stands in front of the sky,
/// the sky the reflection already holds gives way to it, at the weight that sky
/// went in with — so where there is no cloud the colour is left exactly as it was.
///
/// With an environment map the sky is the map's own radiance and the weight is
/// bevy's split-sum ([`specular_weight`], and the occlusion it is applied under).
/// Without one it is `sky_fallback`'s stand-in at `reflected × weight` plus the
/// ambient specular it kept over the open water, so the caller passes the same
/// `clear`, `reflected` and `weight` it passed there. With
/// neither, the surface reflects no sky, and a cloud in it would be the only
/// thing the mirror held.
///
/// Clamped at zero because the sky estimate is recomputed rather than read back:
/// a cloud darker than the sky it covers may not take out more than was put in.
fn with_clouds(
    lit: vec3<f32>,
    pbr_input: PbrInput,
    cloud: vec4<f32>,
    clear: f32,
    reflected: f32,
    weight: f32,
) -> vec3<f32> {
    if cloud.a <= 0.0 {
        return lit;
    }
#ifdef ENVIRONMENT_MAP
    let sky = environment_radiance(pbr_input);
    let w = specular_weight(pbr_input) * pbr_input.specular_occlusion;
#else ifdef DISTANCE_FOG
    // The sky this reflection holds is the stand-in *and* the part of bevy's
    // ambient specular `sky_fallback` left in place, and a cloud stands in front
    // of both. Taking out only the stand-in left the ambient as a flat bright
    // sheen over every cloud — and under overcast, where the ambient is raised
    // and barely gives way, that sheen was nearly all of the reflection: a lake
    // under a dark grey deck came out white-grey with no clouds in it (Deck,
    // `balanced`, 2026-09-29).
    let ray_y = reflect(-pbr_input.V, pbr_input.N).y;
    let t = sky_t(ray_y, pbr_input.material.perceptual_roughness);
    let w = reflected * weight;
    let kept_ambient = ambient_specular(pbr_input) * (1.0 - t * clamp(clear, 0.0, 1.0)) * weight;
    let sky = sky_colour(view_bindings::fog.base_color.rgb, clear, t) * w + kept_ambient;
    return max(lit + cloud.a * (cloud.rgb * w - sky), vec3<f32>(0.0));
#else
    let sky = vec3<f32>(0.0);
    let w = vec3<f32>(0.0);
#endif
    return max(lit + w * cloud.a * (cloud.rgb - sky), vec3<f32>(0.0));
}

/// What a view with **no environment map** adds for the sky the water reflects,
/// on top of the lit colour, and what it takes back out of bevy's ambient.
///
/// The haze colour reflected at every angle made the reflection independent of
/// the wave normal, and a phone's lake read as grey corduroy under a blue sky;
/// the two-stop sky fixes that. Bevy's ambient specular — one flat colour from
/// every direction, raised on these tiers for exactly this sheen — gives way to
/// it in proportion to `t × clear`: under cloud the ambient *is* the sky, and at
/// the horizon it is the brightness the haze colour alone is short of, and both
/// were measured wrong when it was removed everywhere (overcast went navy, dusk
/// went darker than its horizon). `weight` is how much of this fragment is open
/// water — the reflection's own `reflected × mirror × (1 − foam)` on the surface,
/// the tile's water mask on the flat map.
fn sky_fallback(
    lit: vec3<f32>,
    pbr_input: PbrInput,
    horizon: vec3<f32>,
    clear: f32,
    ray_y: f32,
    reflected: f32,
    weight: f32,
    keep_ambient: f32,
) -> vec3<f32> {
    let t = sky_t(ray_y, pbr_input.material.perceptual_roughness);
    let give_way = t * clamp(clear, 0.0, 1.0) * (1.0 - keep_ambient);
    return max(lit - ambient_specular(pbr_input) * give_way, vec3<f32>(0.0))
        + sky_colour(horizon, clear, t) * reflected * weight;
}
