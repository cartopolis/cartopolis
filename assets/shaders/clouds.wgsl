// Procedural cloud ceiling. A camera-centred shell (like starfield.wgsl /
// horizonhaze.wgsl) whose fragment shader projects each above-the-horizon view
// ray onto a flat cloud layer a fixed height above the camera, samples an FBM
// noise field there for coverage, and shades it with a cheap toward-the-sun
// self-shadow tap. No geometry churn — one static shell mesh — so it stays well
// inside the iGPU's command-buffer budget (see CLAUDE.md device-loss notes).
//
// The clouds are camera-relative: because the client is a big_space floating
// origin, `view.world_position` sits at ~0 at any real altitude, so the layer
// reads as an infinite ceiling that follows you horizontally. Wind (params1.xy ×
// time) gives it motion. Sampling in absolute world coords would need the grid
// cell offset; that (and looking *down* on cloud tops from above the layer) are
// follow-ups — this shell is a ceiling seen from below.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

// x: coverage 0..1 (fraction of sky filled). y: edge softness. z: time seconds
// (drives wind). w: layer height above the camera, in metres.
@group(3) @binding(0) var<uniform> params0: vec4<f32>;
// xy: wind velocity in UV units/sec. z: space fade (1 = fully hidden in orbit).
// w: altitude fade (1 = full clouds, 0 = faded out at high flight). unused beyond.
@group(3) @binding(1) var<uniform> params1: vec4<f32>;
// xyz: world direction TO the sun (for the self-shadow offset). w unused.
@group(3) @binding(2) var<uniform> sun_dir: vec4<f32>;
// rgb: sunlit cloud-top colour. a: overall cloud opacity (before the fades).
@group(3) @binding(3) var<uniform> lit_color: vec4<f32>;
// rgb: shadowed underside colour. a unused.
@group(3) @binding(4) var<uniform> shadow_color: vec4<f32>;

// Base noise frequency: metres⁻¹ in the projected cloud-plane coordinates.
const FREQ: f32 = 0.00028;

fn hash2(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453);
}

// Value noise with smooth (quintic) interpolation.
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = hash2(i);
    let b = hash2(i + vec2<f32>(1.0, 0.0));
    let c = hash2(i + vec2<f32>(0.0, 1.0));
    let d = hash2(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// 4-octave fractal noise, normalised to ~0..1.
fn fbm(p: vec2<f32>) -> f32 {
    var v = 0.0;
    var amp = 0.5;
    var q = p;
    for (var i = 0; i < 4; i = i + 1) {
        v += amp * value_noise(q);
        q = q * 2.02 + vec2<f32>(37.1, 17.3);
        amp *= 0.5;
    }
    return v; // sum of amps = 0.9375 ≈ 1
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let dir = normalize(in.world_position.xyz - view.world_position.xyz);

    // Only rays that climb toward the ceiling carry cloud; grazing rays stretch
    // the layer to the horizon and are faded out below.
    if dir.y <= 0.02 {
        discard;
    }

    // Project the ray onto the cloud plane `params0.w` metres overhead.
    let t = params0.w / dir.y;
    let sample_pos = dir.xz * t;
    let uv = sample_pos * FREQ + params0.z * params1.xy;

    let density = fbm(uv);
    let cover = params0.x;
    // Higher coverage lowers the threshold, filling more of the sky.
    let thresh = 1.0 - cover;
    let soft = max(params0.y, 1e-3);
    var a = smoothstep(thresh, thresh + soft, density);

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
    let toward_sun = fbm(uv + sun2d * (60.0 * FREQ * 400.0));
    let lit = clamp((density - toward_sun) + 0.55, 0.0, 1.0);
    let col = mix(shadow_color.rgb, lit_color.rgb, lit);

    return vec4<f32>(col, clamp(a, 0.0, 1.0));
}
