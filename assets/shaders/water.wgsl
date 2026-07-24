// Water surface for the vector-tile geometry path (ADR-035, option B).
//
// The water polygons `vector_tiles::tessellate_surfaces` produces are *flat* —
// a canal is often two triangles, a lake a couple of dozen — so there is nothing
// to displace: every wave here lives in the **normal**, not in the vertex
// positions. That is also why this is an `ExtendedMaterial` over
// `StandardMaterial` rather than a standalone `Material`: the sun, the shadow
// cascades, the atmosphere's environment map and the fog are exactly what make
// water read as water, and all of them come for free from the standard PBR
// path. We only replace the geometric normal, the roughness and a depth tint
// before handing the `PbrInput` back to Bevy.
//
// The wave field is a sum of sharpened directional sines (a cheap Gerstner
// stand-in) evaluated in **map metres**, taken from UV0 rather than from
// `world_position`: under `big_space` the world origin jumps a whole grid cell
// as the camera moves, which would snap the phase of the entire sea at once.
// UV0 is baked per cell from the map anchor (`map_geometry::build_mesh`), so it
// only changes on a re-anchor, which rebuilds the meshes anyway.
//
// Every octave carries its own distance fade. Sub-pixel ripples are the classic
// specular-aliasing source: unfaded, a 20 cm wave a kilometre out flickers as
// white noise under any camera motion. The fade retires each octave once its
// wavelength drops below roughly a pixel and rolls the lost detail into
// roughness instead, so distant water turns into a broad sheen rather than
// crawling glitter.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::{view, globals},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
}

// x: wave steepness (normal strength). y: base wavelength in metres.
// z: wave speed scalar (metres/second at the base wavelength).
// w: detail cut-off — larger retires the fine octaves closer to the camera.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> wave: vec4<f32>;

// x: near roughness. y: extra roughness folded in at distance (the sub-pixel
// waves the octave fade dropped). z: top-down depth darkening (0 = none).
// w: foam amount on the wave crests.
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var<uniform> surface: vec4<f32>;

// xy: unit wind direction the waves travel along. z: chop (0 = rolling swell,
// 1 = sharp wind chop — raises the crest exponent). w unused.
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> wind: vec4<f32>;

// This cell's shore field placement. xy: the cell's north-west corner in the
// same anchor-metre frame as UV0. z: the cell's ground size in metres.
// w: metres a fully-saturated texel stands for.
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var<uniform> field: vec4<f32>;

// Distance from the nearest bank, rasterised per cell (`water_shore_field`).
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var shore_field: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var shore_sampler: sampler;

/// Distance from the bank over which a body of water is treated as sheltered.
/// A 12 m canal never gets more than 6 m from a wall and so keeps only a fine
/// ripple; the middle of a harbour basin gets the full wind sea. Without this
/// every canal in the city ran the same swell as the open IJ, which is the
/// single most artificial thing flat water can do.
const SHELTER_M: f32 = 30.0;

/// Steepness a fully sheltered strip keeps, as a fraction of the open-water sea.
/// Not zero: sheltered water is calm, not a mirror.
const SHELTER_FLOOR: f32 = 0.22;

/// Distance over which the shallow tint fades out into open water.
const SHALLOW_M: f32 = 22.0;

/// Colour multiplier for the shallows. Shallow water is *lighter and greener*:
/// less of the column to absorb the red end, and the bottom scatters some light
/// back. Applied as a tint on the albedo so it rides the scene's own lighting.
const SHALLOW_TINT: vec3<f32> = vec3<f32>(1.5, 2.1, 1.7);

/// Half-width of the foam line against the bank, in metres, at full sea.
const SHORE_FOAM_M: f32 = 2.6;

const TAU: f32 = 6.2831853;

/// Octave count. Five spans ~40 m swell down to ~2 m ripples at the default base
/// wavelength; the distance fade means only the nearest water pays for all five.
const OCTAVES: i32 = 5;

/// Golden angle between octave directions, so they never line up into a visible
/// grid — the giveaway of a stacked-sine ocean.
const OCTAVE_TURN: f32 = 2.39996;

struct Waves {
    /// ∂h/∂x, ∂h/∂z of the wave height field, already scaled by steepness.
    grad: vec2<f32>,
    /// Normalised height of the two long octaves — the big crests.
    height_lo: f32,
    /// Normalised height of the short octaves — the chop riding on them.
    height_hi: f32,
    /// How much of the fine detail survived the distance fade, 0..1 — the
    /// complement is what gets paid back as roughness.
    detail: f32,
}

/// Evaluate the wave field at `p` (map metres) and time `t` (seconds).
///
/// `cut` is the per-metre frequency budget: an octave of angular frequency `k`
/// fades out as `k * cut` approaches 1. The caller folds view distance into it,
/// which is what keeps the far water from aliasing.
fn waves_at(p: vec2<f32>, t: f32, cut: f32, shelter: f32) -> Waves {
    var grad = vec2<f32>(0.0);
    // Long and short octaves are summed apart so foam can ask for *both* — see
    // the crest-and-chop mask in `fragment`.
    var height_lo = 0.0;
    var height_hi = 0.0;
    var weight_lo = 0.0;
    var weight_hi = 0.0;
    var weight = 0.0;
    var visible = 0.0;

    var amp = 1.0;
    var k = TAU / max(wave.y, 0.5);
    // Sharper crests in a wind chop, rounder in a swell.
    let crest_exp = mix(2.0, 5.0, clamp(wind.z, 0.0, 1.0));
    var dir = normalize(select(vec2<f32>(1.0, 0.0), wind.xy, length(wind.xy) > 1e-4));

    for (var i = 0; i < OCTAVES; i += 1) {
        let fade = clamp(1.0 - k * cut, 0.0, 1.0);
        weight += amp;
        let is_lo = i < 2;
        if is_lo {
            weight_lo += amp;
        } else {
            weight_hi += amp;
        }
        if fade > 0.0 {
            // Deep-water dispersion: long waves outrun short ones (c = √(g/k)),
            // which is most of why a stack of sines reads as moving water at all
            // rather than as a rippling sheet.
            let speed = wave.z * inverseSqrt(k);
            // Crest wobble. A pure directional sine has crests that run dead
            // straight to the horizon, and a stack of them reads as corduroy —
            // the giveaway of a sine ocean. Bending each crest along its own
            // length with a long, slow wave costs one extra sin() and turns the
            // stripes into an interlocking cell pattern. The derivative of the
            // wobble is deliberately dropped: it is small next to the wave's own
            // gradient, and omitting it keeps this to one transcendental.
            let perp = vec2<f32>(-dir.y, dir.x);
            let along = dot(p, perp);
            // Per-octave wobble ratio, plus a second incommensurate wobble on
            // the fundamental only. One wobble at one ratio is itself periodic:
            // in the far field, where the distance fade has already retired the
            // short octaves, the fundamental's arcs line up into a visible
            // repeating scallop. The second term (worth one extra sin per pixel,
            // not per octave) beats against the first and breaks the tiling.
            var wobble = sin(along * k * (0.31 + 0.11 * f32(i)) + t * 0.27) * 1.35;
            if i == 0 {
                wobble += sin(along * k * 0.13 - t * 0.19) * 0.9;
            }
            let phase = dot(p, dir) * k + t * speed * k + wobble;
            // 0..1 sine, sharpened into crests. d/dphase of pow(s, n) is
            // n·s^(n-1)·½cos(phase).
            let s = 0.5 + 0.5 * sin(phase);
            let shaped = pow(s, crest_exp);
            let d_shaped = crest_exp * pow(s, crest_exp - 1.0) * 0.5 * cos(phase);
            let w = amp * fade;
            if is_lo {
                height_lo += w * shaped;
            } else {
                height_hi += w * shaped;
            }
            grad += dir * (w * d_shaped * k);
            visible += w;
        }
        // A steeper amplitude roll-off (0.62 and below) lets the fundamental
        // dominate, and one dominant direction is exactly what reads as
        // corduroy; 0.72 keeps the higher octaves in the mix.
        amp *= 0.72;
        k *= 1.83;
        dir = vec2<f32>(
            dir.x * cos(OCTAVE_TURN) - dir.y * sin(OCTAVE_TURN),
            dir.x * sin(OCTAVE_TURN) + dir.y * cos(OCTAVE_TURN),
        );
    }

    var out: Waves;
    out.grad = grad * (wave.x * shelter / max(weight, 1e-4));
    out.height_lo = height_lo / max(weight_lo, 1e-4);
    out.height_hi = height_hi / max(weight_hi, 1e-4);
    out.detail = visible / max(weight, 1e-4);
    return out;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let to_view = view.world_position.xyz - in.world_position.xyz;
    let dist = length(to_view);
    let v = to_view / max(dist, 1e-4);

#ifdef VERTEX_UVS_A
    // Map metres from the anchor — stable while the floating origin moves.
    let p = in.uv;
#else
    let p = in.world_position.xz;
#endif

    // Distance to the nearest bank, in metres. UV0 is anchor metres, so the cell's
    // own corner and size turn it into this cell's 0..1 texture frame.
    let shore_uv = (p - field.xy) / max(field.z, 1.0);
    let shore_m = textureSample(shore_field, shore_sampler, shore_uv).r * field.w;

    // A canal is not a small sea: waves need fetch, and a strip of water with a
    // wall 6 m away has none. Damping the whole wave field by distance from the
    // bank is what makes the same shader serve a canal, a harbour basin and the
    // open river without three sets of numbers.
    let shelter = mix(SHELTER_FLOOR, 1.0, smoothstep(0.0, SHELTER_M, shore_m));

    let w = waves_at(p, globals.time, dist * wave.w, shelter);

    // The surface is flat ground, so the geometric normal is +Y and the wave
    // normal can be built directly in world space — no tangent frame needed.
    let n = normalize(vec3<f32>(-w.grad.x, 1.0, -w.grad.y));
    pbr_input.N = n;
    pbr_input.world_normal = n;

    // Detail lost to the distance fade comes back as roughness, so far water is
    // a wide soft sheen instead of aliasing sparkle.
    pbr_input.material.perceptual_roughness = clamp(
        surface.x + surface.y * (1.0 - w.detail),
        0.02,
        1.0,
    );

    // Water is dark looking down (you see into it) and mirror-bright at grazing
    // angles. Schlick's fresnel drives the tint; the reflection itself is the
    // standard path's environment map, which is the atmosphere — so the sea
    // takes the sky's colour through dawn, noon and dusk on its own.
    let fresnel = pow(1.0 - clamp(dot(n, v), 0.0, 1.0), 5.0);
    let depth_tint = mix(1.0 - clamp(surface.z, 0.0, 1.0), 1.0, fresnel);

    // Shallows read lighter and greener: a short water column absorbs less of the
    // red end and the bottom scatters some light back. There is no bathymetry in
    // the tile, but distance from the bank is a good proxy for it — and it is the
    // right proxy visually, because what the eye actually reads is "this is the
    // edge of the water", which is exactly what the field measures.
    let shallow = 1.0 - smoothstep(0.0, SHALLOW_M, shore_m);
    let body_tint = mix(vec3<f32>(1.0), SHALLOW_TINT, shallow) * depth_tint;
    pbr_input.material.base_color = vec4<f32>(
        pbr_input.material.base_color.rgb * body_tint,
        pbr_input.material.base_color.a,
    );

    // Whitecaps break where the chop is riding the top of a big crest, so the
    // mask is the product of the two bands. A threshold on the summed height
    // alone paints continuous ribbons down every crest line — foam as a stripe,
    // not as spray. The bands sit well below 1 on purpose: each is a mean of
    // sharpened sines, so its peaks land nearer 0.7 than unity.
    let crest_foam = surface.w
        * w.detail
        * smoothstep(0.34, 0.62, w.height_lo)
        * smoothstep(0.40, 0.80, w.height_hi);

    // The waterline. A hard edge between water and bank is the tell that a river
    // is a blue polygon; real water leaves a bright, restless line where it meets
    // the land. The band's width breathes with the local wave height, so it laps
    // in and out along the bank instead of tracing a constant offset, and it
    // survives a dead calm (a wet edge is always there) while growing with the
    // sea. `w.detail` retires it with the rest of the fine detail at distance.
    let lap = SHORE_FOAM_M * (0.35 + 0.65 * wind.z) * (0.55 + 0.9 * w.height_hi);
    let shore_foam = w.detail * (0.30 + 0.55 * wind.z) * (1.0 - smoothstep(0.0, lap, shore_m));

    let foam = clamp(max(crest_foam, shore_foam), 0.0, 1.0);
    pbr_input.material.base_color = vec4<f32>(
        mix(pbr_input.material.base_color.rgb, vec3<f32>(1.0), foam),
        pbr_input.material.base_color.a,
    );
    pbr_input.material.perceptual_roughness = mix(
        pbr_input.material.perceptual_roughness,
        0.9,
        foam,
    );

    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
