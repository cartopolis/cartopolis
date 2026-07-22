// Procedural cloud ceiling. A camera-centred shell (like starfield.wgsl /
// horizonhaze.wgsl) whose fragment shader projects each above-the-horizon view
// ray onto a flat cloud layer a fixed height above the camera, samples an FBM
// noise field there for coverage, and shades it with a cheap toward-the-sun
// self-shadow tap. No geometry churn — one static shell mesh — so it stays well
// inside the iGPU's command-buffer budget (see CLAUDE.md device-loss notes).
//
// The deck is world-anchored: `params2.xy` carries the camera's horizontal
// displacement from the map anchor (metres), added to the projected sample so
// the clouds stay put over the ground and you fly *under* them. Wind (params1.xy
// × time) drifts the whole field on top of that. The integer hash keeps the
// noise stable at the large sample coordinates world-anchoring produces (the old
// sin() hash banded and shimmered once the offset grew).
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

// x: coverage 0..1 (fraction of sky filled). y: edge softness. z: time seconds
// (drives wind). w: layer height above the camera, in metres.
@group(3) @binding(0) var<uniform> params0: vec4<f32>;
// xy: wind velocity in UV units/sec. z: space fade (1 = fully hidden in orbit).
// w: altitude fade (1 = full clouds, 0 = faded out at high flight).
@group(3) @binding(1) var<uniform> params1: vec4<f32>;
// xyz: world direction TO the sun (for the self-shadow offset). w unused.
@group(3) @binding(2) var<uniform> sun_dir: vec4<f32>;
// rgb: sunlit cloud-top colour. a: overall cloud opacity (before the fades).
@group(3) @binding(3) var<uniform> lit_color: vec4<f32>;
// rgb: shadowed underside colour. a unused.
@group(3) @binding(4) var<uniform> shadow_color: vec4<f32>;
// xy: world-anchor offset in UV units (camera displacement from the anchor ×
// FREQ). z, w unused.
@group(3) @binding(5) var<uniform> params2: vec4<f32>;

// Base noise frequency: metres⁻¹ in the projected cloud-plane coordinates.
const FREQ: f32 = 0.00034;

// Integer hash → [0,1). Stable at large coordinates (unlike a sin() hash), so a
// world-anchored deck that has drifted kilometres from the origin doesn't band.
fn hash_i(p: vec2<i32>) -> f32 {
    var n = u32(p.x) * 1597334677u + u32(p.y) * 3812015801u;
    n = (n ^ (n >> 16u)) * 2246822519u;
    n = n ^ (n >> 13u);
    return f32(n & 0x00FFFFFFu) / f32(0x01000000u);
}

// Value noise with smooth (quintic) interpolation.
fn value_noise(p: vec2<f32>) -> f32 {
    let ip = floor(p);
    let f = fract(p);
    let i = vec2<i32>(ip);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = hash_i(i);
    let b = hash_i(i + vec2<i32>(1, 0));
    let c = hash_i(i + vec2<i32>(0, 1));
    let d = hash_i(i + vec2<i32>(1, 1));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// 6-octave fractal noise, normalised to ~0..1. More octaves than the v0 give the
// deck fine billow detail (it reads less blurry near the zenith, where the small
// projected footprint magnifies the field).
fn fbm(p: vec2<f32>) -> f32 {
    var v = 0.0;
    var amp = 0.5;
    var norm = 0.0;
    var q = p;
    for (var i = 0; i < 6; i = i + 1) {
        v += amp * value_noise(q);
        norm += amp;
        q = q * 2.03 + vec2<f32>(37.1, 17.3);
        amp *= 0.55;
    }
    return v / norm;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let dir = normalize(in.world_position.xyz - view.world_position.xyz);

    // Only rays that climb toward the ceiling carry cloud; grazing rays stretch
    // the layer to the horizon and are faded out below.
    if dir.y <= 0.02 {
        discard;
    }

    // Project the ray onto the cloud plane `params0.w` metres overhead, then
    // world-anchor the sample so the deck stays fixed over the ground.
    let t = params0.w / dir.y;
    let sample_pos = dir.xz * t;
    let uv = sample_pos * FREQ + params2.xy + params0.z * params1.xy;

    // Coverage: higher coverage lowers the threshold, filling more of the sky.
    // A gentle power puffs the cores and thins the wispy edges.
    let density = fbm(uv);
    let cover = params0.x;
    let thresh = 1.0 - cover;
    let soft = max(params0.y, 1e-3);
    var a = smoothstep(thresh, thresh + soft, density);
    a = pow(a, 0.75);

    // Fade the grazing-angle stretch into the horizon, and apply the CPU-side
    // altitude / space fades so a high or orbital view sees clear sky.
    a *= smoothstep(0.02, 0.20, dir.y);
    a *= lit_color.a * params1.w * (1.0 - params1.z);
    if a <= 0.002 {
        discard;
    }

    // Cheap self-shadow: sample the field a step toward the sun; where that side
    // is denser, this pixel sits under a taller column, so it reads shadowed.
    let sun2d = normalize(sun_dir.xz + vec2<f32>(1e-4, 0.0));
    let toward_sun = fbm(uv + sun2d * 0.09);
    let lit = clamp((density - toward_sun) + 0.55, 0.0, 1.0);
    // A touch darker toward the base of thick cloud for depth.
    let col = mix(shadow_color.rgb, lit_color.rgb, lit * mix(0.7, 1.0, density));

    return vec4<f32>(col, clamp(a, 0.0, 1.0));
}
