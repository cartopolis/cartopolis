// Planet-wrapping cloud layer for the globe / space tier — a global *weather*
// system, not a uniform noise field. A sphere a few km above the globe surface,
// centred on Earth's render-space centre (fed each frame like the planet
// atmosphere entity — see globe::update_atmosphere_origin). Its fragment shader
// builds an Earth-like cloud pattern:
//   * Latitude banding — a cloudy equatorial ITCZ, clear subtropical highs
//     (~25°), and cloudy mid-latitude storm tracks (~55°), the signature stripes
//     of the planet from space.
//   * Zonal stretch — features are elongated east–west into bands and fronts
//     (real cloud systems are stretched by the zonal winds), eased off in the
//     mid-latitude storm belts so cyclones there stay round.
//   * Domain-warped 3D FBM with a Coriolis-signed curl twist — cloud masses wind
//     into cyclonic spirals (counterclockwise north, clockwise south) and ridged
//     detail carves fibrous fronts, instead of round blobs.
//   * Motion obeys one invariant: only latitude-INDEPENDENT terms may grow with
//     time (a latitude-dependent term × unbounded time aliases into thin latitude
//     stripes — the "taffy" smearing). So bulk motion is a slow UNIFORM spin, the
//     weather evolves via a UNIFORM warp scroll (features form and dissolve around
//     an anchored base), and the hemispheric swirl is a time-static Coriolis curl.
//     Real per-region weather variation is meant to come from live GFS/TCDC data
//     (ADR-032), not from procedural differential drift, which is unstable here.
// Shaded by the sun terminator and faded in with `params.y` (space_factor) so
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
// xyz: Earth's spin axis in the local frame (for latitude banding + drift).
@group(3) @binding(5) var<uniform> north_axis: vec4<f32>;
// xyz: local-frame direction to geographic (0°,0°) — the prime-meridian/equator
// reference used to recover longitude for the data lookup. w: data weight
// (0 = pure procedural fallback, 1 = live GFS/TCDC coverage — see ADR-032).
@group(3) @binding(6) var<uniform> frame_ref: vec4<f32>;
// Live Total Cloud Cover: a coarse equirectangular grid (GFS TCDC), r = cloud
// fraction 0..1. Row 0 = +90° lat, column 0 = 0° lng, columns increase eastward.
@group(3) @binding(7) var cloud_field: texture_2d<f32>;
@group(3) @binding(8) var cloud_samp: sampler;

const PI: f32 = 3.14159265;

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

// A rotation-and-scale applied between octaves. Value/gradient noise lives on an
// axis-aligned lattice; adding octaves along the *same* axes reinforces that grid
// (the hexagonal/blocky blobs). Rotating each octave by this non-axis-aligned
// matrix decorrelates the lattices, so the sum reads as smooth fractal cloud.
const M3: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(0.00, 0.80, 0.60),
    vec3<f32>(-0.80, 0.36, -0.48),
    vec3<f32>(-0.60, -0.48, 0.64),
);

// Octave counts, and why they are not all the same.
//
// **This shader is the most expensive thing the client draws.** Measured on a
// Steam Deck (RADV) 2026-09-09: the cloud shell is the whole transparent pass, and
// at 1280x720 that is 71% of the GPU frame — 52 fps against 85 with the shell off.
// It is not overdraw (exactly 1.00x, one fragment per pixel); it is what one
// fragment costs. So the octave count is a performance decision, not a taste one.
//
// The nine `fbm3` calls below are not equal. Six of them (`q` and `r`) are *warps*:
// their output is multiplied by 2.2 and added to a sample coordinate, so their
// high octaves move the sample by a fraction of a noise cell — a displacement of a
// displacement, invisible in the result. The three that shape what you actually
// see — the density, the ridge crests and the opacity wisp — keep their detail.
//
// `WARP_OCTAVES` at 2 rather than 6 removes 4 octaves from 6 of the 9 fields:
// 54 `vnoise3` calls per fragment become 27, a 50% cut in the noise work that
// dominates this shader.
//
// **Two is the floor, and one is not merely worse — it is broken.** Measured as a
// ladder on the Deck at 1280x720 (cloud pass, whole-Earth capture with live GFS):
//
//     warp 3  5.90 ms   reference
//     warp 2  5.05 ms   -14%   mean difference 1.10/255
//     warp 1  4.02 ms   -32%   mean 2.77/255, and visibly blocky
//
// At one octave the warp is too coarse to decorrelate the noise lattice, and the
// axis-aligned grid `M3` exists to hide shows straight through as rectangular,
// stair-stepped patches — exactly the "hexagonal/blocky blobs" the comment above
// `M3` describes. The extra 1.0 ms is not worth a visible artefact; that is the
// trade, and it is why this stops at 2.
const DENSITY_OCTAVES: i32 = 6;
const WARP_OCTAVES: i32 = 2;
/// The wisp is documented below as "gentle, LARGE-scale opacity variation… low
/// frequency and shallow", so its top octaves are contributing detail the effect
/// explicitly does not want.
const WISP_OCTAVES: i32 = 3;

fn fbm3_n(p: vec3<f32>, octaves: i32) -> f32 {
    var v = 0.0;
    var amp = 0.5;
    var norm = 0.0;
    var q = p;
    for (var i = 0; i < octaves; i = i + 1) {
        v += amp * vnoise3(q);
        norm += amp;
        q = M3 * q * 2.0;
        amp *= 0.5;
    }
    return v / norm;
}

fn fbm3(p: vec3<f32>) -> f32 {
    return fbm3_n(p, DENSITY_OCTAVES);
}

// Rotate `v` about a unit `axis` by `ang` radians (Rodrigues).
fn rotate_about(v: vec3<f32>, axis: vec3<f32>, ang: f32) -> vec3<f32> {
    let c = cos(ang);
    let s = sin(ang);
    return v * c + cross(axis, v) * s + axis * dot(axis, v) * (1.0 - c);
}

// Unit-area Gaussian bump. Used with a manual `x*x` (not `pow(x, 2.0)`, which is
// undefined for x<0 in WGSL) so latitude bands centred off the equator are safe.
fn gauss(x: f32) -> f32 {
    return exp(-x * x);
}

// THE INVARIANT: only latitude-INDEPENDENT terms may grow with time. Any
// latitude-dependent phase multiplied by unbounded `t` develops an ever-steeper
// latitudinal gradient and aliases into thin latitude-parallel stripes (the
// "taffy" smearing). So bulk motion is a uniform spin, evolution is a uniform
// warp scroll, and every latitude-dependent term (curl, banding) is time-static.

// Bulk planet spin: a slow UNIFORM rotation about the axis (same at every
// latitude → rigid → zero shear). Gentle, since it spins the clouds relative to
// the fixed map beneath.
const BULK_DRIFT: f32 = 0.002;
// Uniform temporal evolution: a fixed, latitude-independent translation of the
// warp SOURCE through the noise, so the field regenerates (features form and
// dissolve) without introducing any latitude-varying phase. This is the safe
// substitute for differential drift — the "weather evolves" motion.
const EVO_RATE: f32 = 0.020;
// Coriolis-signed curl twist on the warp: cyclones wind counterclockwise in the
// north, clockwise in the south, none on the equator. Latitude-signed but NOT
// time-growing, so it's bounded and alias-free.
const SWIRL_STRENGTH: f32 = 1.10;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Surface direction from Earth's centre — frame-independent of the render
    // translation, so it matches the sun direction's local frame.
    let normal = normalize(in.world_position.xyz - centre.xyz);
    let axis = normalize(north_axis.xyz);

    // Latitude (−π/2..π/2) from the spin axis. Drift below rotates about that
    // axis, which preserves latitude, so this is the same before and after.
    let up = dot(normal, axis);
    let lat = asin(clamp(up, -1.0, 1.0));

    // Bulk uniform spin only (grows with time but never shears — same angle at
    // every latitude). The large-scale cloud positions are anchored to this
    // rigidly-rotated frame; all evolution rides the bounded warp below.
    let dn = rotate_about(normal, axis, params.z * BULK_DRIFT);
    let radial = normalize(dn);

    // Anisotropic sample frame: a mild east–west stretch shapes zonal bands
    // (eased off in the mid-latitude storm belts so cyclones stay round). Kept low
    // — a strong stretch pre-smears features into the long streaks we're avoiding.
    let eqv = dn - axis * up;
    let stretch = mix(1.35, 1.05, gauss((abs(lat) - 0.90) / 0.35));
    let sample_n = eqv + axis * up * stretch;
    let base = sample_n * params.w;

    // Uniform temporal evolution: a fixed, latitude-INDEPENDENT translation of the
    // warp source through the noise. Identical for every fragment, so it adds no
    // latitude-varying phase (no aliasing however large `t` grows); it just sweeps
    // the warp, which — since the final density is sampled at `base + 2.2 * rw`
    // with `rw` bounded — makes clouds form and dissolve around the anchored
    // `base` without ever sliding or winding away.
    let evo = vec3<f32>(0.61, -0.42, 0.67) * (params.z * EVO_RATE);

    // Iterative flow warping (iq's technique) plus a Coriolis-signed curl
    // (`rotate_about` the local vertical `radial`) that twists the advection into
    // spirals — counterclockwise north, clockwise south, none on the equator (the
    // ITCZ convects, it doesn't rotate).
    let coriolis = sin(lat);
    let q = vec3<f32>(
        fbm3_n(base + evo, WARP_OCTAVES),
        fbm3_n(base + evo + vec3<f32>(5.2, 1.3, 7.8), WARP_OCTAVES),
        fbm3_n(base + evo + vec3<f32>(2.7, 9.1, 3.4), WARP_OCTAVES),
    ) - 0.5;
    let qw = rotate_about(q, radial, coriolis * SWIRL_STRENGTH);
    let r = vec3<f32>(
        fbm3_n(base + evo + 2.2 * qw + vec3<f32>(1.7, 8.2, 4.1), WARP_OCTAVES),
        fbm3_n(base + evo + 2.2 * qw + vec3<f32>(6.3, 2.9, 9.6), WARP_OCTAVES),
        fbm3_n(base + evo + 2.2 * qw + vec3<f32>(3.8, 5.5, 1.2), WARP_OCTAVES),
    ) - 0.5;
    let rw = rotate_about(r, radial, coriolis * SWIRL_STRENGTH * 0.6);
    let base_density = fbm3(base + 2.2 * rw);

    // Ridged high-frequency detail carves fronts and fibrous filaments into the
    // cloud edges (sharp crests) instead of leaving soft, round halos.
    let ridge = 1.0 - abs(2.0 * fbm3(base * 2.6 + 4.0 * rw) - 1.0);
    let density = mix(base_density, base_density * (0.55 + 0.9 * ridge), 0.4);

    // Latitude banding — the planet's signature stripes. Cloudy equatorial ITCZ,
    // thinner (not bare) subtropical highs (~26°), cloudy mid-latitude storm
    // tracks (~54°). A high baseline keeps every band populated so it reads as a
    // cloudy planet with structure, not scattered dots.
    let l = abs(lat);
    let itcz = 0.55 * exp(-pow(l / 0.15, 2.0));
    let dry = -0.35 * exp(-pow((l - 0.46) / 0.18, 2.0));
    let storm = 0.45 * exp(-pow((l - 0.95) / 0.32, 2.0));
    let band = clamp(0.85 + itcz + dry + storm, 0.2, 1.6);
    let proc_cover = params.x * band;

    // Live coverage from GFS Total Cloud Cover. Recover geographic (lng, lat) for
    // this fragment — latitude from the spin axis (reuse `lat`), longitude from the
    // prime-meridian reference — and look up the coarse equirectangular grid. Row 0
    // is +90°, column 0 is 0° lng increasing east, so v = 0.5 − lat/π and
    // u = lng/2π (wrapped). Real TCDC already carries the ITCZ/storm-track
    // structure, so it *replaces* the hand-authored bands (weighted by frame_ref.w,
    // which is 0 until data lands → the procedural fallback below).
    let east0 = normalize(cross(axis, normalize(frame_ref.xyz)));
    let lng = atan2(dot(normal, east0), dot(normal, normalize(frame_ref.xyz)));
    // Perturb the lookup by the flow warp so the coarse 1° grid's blocky cell
    // boundaries dissolve into fractal cloud edges instead of reading as stair-steps.
    let uv = vec2<f32>(fract(lng / (2.0 * PI)), 0.5 - lat / PI) + vec2<f32>(rw.x, rw.z) * 0.02;
    let tcdc = textureSampleLevel(cloud_field, cloud_samp, uv, 0.0).r;

    // Coverage envelope from real TCDC (or the procedural fallback). Held back from
    // saturation (0.72, not 0.95) so even the cloudiest regions keep broken gaps —
    // the surface should read *through* the weather, not vanish under a solid sheet.
    let cover = clamp(mix(proc_cover, tcdc, frame_ref.w), 0.0, 1.0);
    let thresh = 1.0 - 0.72 * cover;

    // Opacity varies with cloud THICKNESS instead of snapping to one flat value:
    //   * `presence` is a soft, wide-edged mask (gentle borders, not a hard cut);
    //   * `thickness` keeps ramping past the edge, and is squared so most cloud
    //     stays a translucent veil the surface reads through, with only the densest
    //     cores going bright/opaque — the opacity spread of real satellite cloud;
    //   * `wisp` breaks each cloud into filaments/cells in OPACITY only (finer,
    //     warped noise), adding texture without enlarging the cloud's footprint.
    let presence = smoothstep(thresh, thresh + 0.28, density);
    let thickness = smoothstep(thresh, thresh + 0.60, density);
    // Gentle, LARGE-scale opacity variation (thin veils vs solid cores). Low
    // frequency and shallow, so it grades the cloud rather than shattering it into
    // cells — the earlier fine, deep wisp is what fragmented the masses.
    let wisp = 0.72 + 0.36 * fbm3_n(base * 1.3 + 3.0 * rw, WISP_OCTAVES);
    let opacity = mix(0.16, 1.0, thickness) * wisp;
    var a = presence * clamp(opacity, 0.0, 1.0) * lit_color.a * params.y;
    if a <= 0.003 {
        discard;
    }

    // Day/night terminator: clouds on the lit hemisphere are bright, the night
    // side falls to a dim blue. Use the un-rotated normal so lighting is fixed.
    let ndl = dot(normal, sun_dir.xyz);
    let day = smoothstep(-0.18, 0.22, ndl);
    let col = mix(shadow_color.rgb, lit_color.rgb, day);

    return vec4<f32>(col, clamp(a, 0.0, 1.0));
}
