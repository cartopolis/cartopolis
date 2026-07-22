// Planet-wrapping cloud layer for the globe / space tier. A sphere a few km above
// the globe surface, centred on Earth's render-space centre (fed each frame like
// the planet atmosphere entity — see globe::update_atmosphere_origin). Its
// fragment shader samples a 3D FBM field on the surface direction for coverage
// and shades it by the sun terminator, so from orbit you see scattered cloud over
// the day side fading into the night. Fades in with `params.y` (space_factor) so
// it's absent on the ground, where the camera-relative ceiling (clouds.wgsl)
// takes over. One static sphere mesh — fixed cost, no churn.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

// xyz: sphere centre in render space. w: sphere radius (m).
@group(3) @binding(0) var<uniform> centre: vec4<f32>;
// xyz: world direction TO the sun (local anchor frame). w unused.
@group(3) @binding(1) var<uniform> sun_dir: vec4<f32>;
// x: coverage 0..1. y: opacity (space fade). z: time. w: noise frequency.
@group(3) @binding(2) var<uniform> params: vec4<f32>;
// rgb: sunlit cloud colour. a: overall opacity.
@group(3) @binding(3) var<uniform> lit_color: vec4<f32>;
// rgb: night-side cloud colour. a unused.
@group(3) @binding(4) var<uniform> shadow_color: vec4<f32>;

fn hash3(p: vec3<i32>) -> f32 {
    var n = u32(p.x) * 1597334677u + u32(p.y) * 3812015801u + u32(p.z) * 2798796415u;
    n = (n ^ (n >> 16u)) * 2246822519u;
    n = n ^ (n >> 13u);
    return f32(n & 0x00FFFFFFu) / f32(0x01000000u);
}

fn vnoise3(p: vec3<f32>) -> f32 {
    let ip = floor(p);
    let f = fract(p);
    let i = vec3<i32>(ip);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let c000 = hash3(i);
    let c100 = hash3(i + vec3<i32>(1, 0, 0));
    let c010 = hash3(i + vec3<i32>(0, 1, 0));
    let c110 = hash3(i + vec3<i32>(1, 1, 0));
    let c001 = hash3(i + vec3<i32>(0, 0, 1));
    let c101 = hash3(i + vec3<i32>(1, 0, 1));
    let c011 = hash3(i + vec3<i32>(0, 1, 1));
    let c111 = hash3(i + vec3<i32>(1, 1, 1));
    let x00 = mix(c000, c100, u.x);
    let x10 = mix(c010, c110, u.x);
    let x01 = mix(c001, c101, u.x);
    let x11 = mix(c011, c111, u.x);
    let y0 = mix(x00, x10, u.y);
    let y1 = mix(x01, x11, u.y);
    return mix(y0, y1, u.z);
}

fn fbm3(p: vec3<f32>) -> f32 {
    var v = 0.0;
    var amp = 0.5;
    var norm = 0.0;
    var q = p;
    for (var i = 0; i < 5; i = i + 1) {
        v += amp * vnoise3(q);
        norm += amp;
        q = q * 2.02 + vec3<f32>(19.3, 7.1, 13.7);
        amp *= 0.55;
    }
    return v / norm;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Surface direction from Earth's centre — frame-independent of the render
    // translation, so it matches the sun direction's local frame.
    let normal = normalize(in.world_position.xyz - centre.xyz);

    let drift = vec3<f32>(params.z * 0.004, 0.0, 0.0);
    let base = normal * params.w + drift;
    // Domain warping: offset the sample by a lower-frequency noise vector so the
    // field forms swirls, bands and fronts instead of an even field of blobs.
    let warp = vec3<f32>(
        fbm3(base * 0.5 + vec3<f32>(11.5, 3.2, 7.7)),
        fbm3(base * 0.5 + vec3<f32>(27.3, 19.1, 2.4)),
        fbm3(base * 0.5 + vec3<f32>(43.1, 8.6, 31.9)),
    ) - 0.5;
    let density = fbm3(base + warp * 2.2);
    let cover = params.x;
    // Bias the threshold so coverage 0.5 leaves clear gaps; a wide smoothstep and
    // a power give soft, wispy cloud edges rather than hard-cut spots.
    let thresh = 1.0 - 0.75 * cover;
    var a = smoothstep(thresh, thresh + 0.28, density);
    a = pow(a, 1.3);
    a *= lit_color.a * params.y;
    if a <= 0.003 {
        discard;
    }

    // Day/night terminator: clouds on the lit hemisphere are bright, the night
    // side falls to a dim blue.
    let ndl = dot(normal, sun_dir.xyz);
    let day = smoothstep(-0.18, 0.22, ndl);
    let col = mix(shadow_color.rgb, lit_color.rgb, day);

    return vec4<f32>(col, clamp(a, 0.0, 1.0));
}
