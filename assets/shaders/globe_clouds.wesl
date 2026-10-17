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
import bevy_pbr::render::forward_io::VertexOutput;
import bevy_pbr::render::mesh_view_bindings::view;

// One uniform rather than seven: WebGPU allows 12 uniform buffers per shader
// stage and bevy's view bindings already spend five, so seven left this
// pipeline at exactly the limit. `GlobeCloudGpu` in `sky.rs` is the Rust side;
// `fragment` reads the fields back under their old names.
struct GlobeClouds {
    // xyz: sphere centre in render space. w: sphere radius (m).
    centre: vec4<f32>,
    // xyz: world direction TO the sun (local anchor frame). w unused.
    sun_dir: vec4<f32>,
    // x: coverage 0..1. y: opacity (space fade). z: time. w: noise frequency.
    params: vec4<f32>,
    // rgb: sunlit cloud colour. a: overall opacity.
    lit_color: vec4<f32>,
    // rgb: night-side cloud colour. a unused.
    shadow_color: vec4<f32>,
    // xyz: Earth's spin axis in the local frame (for latitude banding + drift).
    north_axis: vec4<f32>,
    // xyz: local-frame direction to geographic (0°,0°) — the prime-meridian/
    // equator reference used to recover longitude for the data lookup. w: data
    // weight (0 = pure procedural fallback, 1 = live GFS/TCDC coverage — ADR-032).
    frame_ref: vec4<f32>,
}
@group(3) @binding(0) var<uniform> globe: GlobeClouds;
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
// see — the density, the ridge crests and the relief sample — keep their detail.
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
// Octaves past `DENSITY_OCTAVES` that only the visible density reads
// (`fbm3_detail`).
const DETAIL_OCTAVES: i32 = 2;
// Half-width of a cloud's edge, in density units: crisp, as satellite cloud is.
const EDGE: f32 = 0.025;
// The most of a grid cell the data may cover, so the cloudiest regions keep
// broken gaps.
const COVER_CAP: f32 = 0.9;
// The first octave of `fbm3_detail` folded into puffs (~220 km cells, down to
// ~28 km). The coarser ones stay plain: they are weather systems, not cells.
const PUFF_FROM: i32 = 4;

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

// The density field to `octaves`: y is its value, x its value at
// `DENSITY_OCTAVES` when it runs past them. Six octaves at `GLOBE_CLOUD_FREQ`
// stop at ~110 km — 18 px from 4,000 km up at 960 px across, which is why the
// clouds read as soft blobs; two more reach ~28 km. Only the density is read
// this fine (two noise samples more per fragment); the relief compares its
// six-octave value against a six-octave sample.
fn fbm3_detail(p: vec3<f32>, octaves: i32) -> vec2<f32> {
    var v = 0.0;
    var amp = 0.5;
    var norm = 0.0;
    var q = p;
    var coarse = 0.0;
    for (var i = 0; i < octaves; i = i + 1) {
        if i == DENSITY_OCTAVES {
            coarse = v / norm;
        }
        let raw = vnoise3(q);
        // From `PUFF_FROM` on, folded into puffs — round cells with creases
        // between, the popcorn of a convective cloud field seen from orbit.
        v += amp * select(raw, abs(2.0 * raw - 1.0), i >= PUFF_FROM);
        norm += amp;
        q = M3 * q * 2.0;
        amp *= 0.5;
    }
    return vec2<f32>(select(v / norm, coarse, octaves > DENSITY_OCTAVES), v / norm);
}

// The density above which a fraction `cover` of the sphere is cloud, at covers
// 0, 0.05, … 1: the field's quantiles, from a TypeScript mirror of
// `fbm3_detail` (puffs and all) and the ridge mix over 300,000 points
// (2026-10-17). The warp
// moves the field but not its distribution.
fn cover_threshold(cover: f32) -> f32 {
    var t = array<f32, 21>(
        1.000, 0.763, 0.718, 0.686, 0.660, 0.638, 0.617, 0.597, 0.579, 0.561, 0.544,
        0.526, 0.508, 0.490, 0.471, 0.451, 0.429, 0.404, 0.373, 0.330, 0.000,
    );
    let x = clamp(cover, 0.0, 1.0) * 20.0;
    let i = min(i32(x), 19);
    return mix(t[i], t[i + 1], x - f32(i));
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

// Relief shading (see the fragment): how far toward the sun the field is read
// again, in noise units (at `GLOBE_CLOUD_FREQ` 1.8 one unit is ~3,500 km, so
// this is ~50 km — a cloud tower's shadow at a low sun), how strongly a
// height difference shades, and the clamp on the result.
const RELIEF_STEP: f32 = 0.015;
const RELIEF_GAIN: f32 = 2.5;
const RELIEF_MIN: f32 = 0.35;
const RELIEF_MAX: f32 = 1.3;
// Zenith Rayleigh optical depth per channel — `utils::sun::RAYLEIGH`.
const RAYLEIGH_DEPTH: vec3<f32> = vec3<f32>(0.041, 0.100, 0.250);

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let centre = globe.centre;
    let sun_dir = globe.sun_dir;
    let params = globe.params;
    let lit_color = globe.lit_color;
    let shadow_color = globe.shadow_color;
    let north_axis = globe.north_axis;
    let frame_ref = globe.frame_ref;
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
    // Eight octaves for what is seen, the first six of them for the relief
    // (`fbm3_detail`).
    let densities = fbm3_detail(base + 2.2 * rw, DENSITY_OCTAVES + DETAIL_OCTAVES);
    let base_density = densities.y;

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

    // Coverage envelope from real TCDC (or the procedural fallback). It is the
    // fraction of a grid cell under cloud, so it picks the field's quantile
    // (`cover_threshold`) and the edge there is crisp: the field breaks a 100 km
    // cell into the cloud it says, with structure down to the finest octave.
    // It used to be `1 − 0.72 × cover` with an edge a quarter of the field wide,
    // which drew the coarse grid itself as soft blobs. Held back from saturation
    // (`COVER_CAP`) so even the cloudiest regions keep broken gaps.
    let cover = clamp(mix(proc_cover, tcdc, frame_ref.w), 0.0, 1.0);
    let thresh = cover_threshold(cover * COVER_CAP);

    // Opacity varies with cloud THICKNESS instead of snapping to one flat value:
    //   * `presence` is the cloud's edge. It used to be 0.28 of the field wide,
    //     which blurred every system into a soft veil; satellite cloud has edges
    //     you can draw a line along, so it is narrow now;
    //   * `thickness` keeps ramping past the edge, so most cloud stays a
    //     translucent veil the surface reads through, with only the densest cores
    //     going bright/opaque — the opacity spread of real satellite cloud.
    let presence = smoothstep(thresh - EDGE, thresh + EDGE, density);
    let thickness = smoothstep(thresh, thresh + 0.12, density);
    let opacity = mix(0.3, 1.0, thickness);
    var a = presence * clamp(opacity, 0.0, 1.0) * lit_color.a * params.y;
    if a <= 0.003 {
        discard;
    }

    // ---- light --------------------------------------------------------------
    // Use the un-rotated normal so the lighting is fixed to the sun, not the
    // drifting field.
    let ndl = dot(normal, sun_dir.xyz);

    // Relief: the field read as cloud-top height, compared with the same field
    // a step toward the sun. Where the cloud rises toward the sun this point is
    // in its shadow; where it falls away, this is a sunlit face. The step and
    // the field are in noise space, so the sun's along-surface direction is spun
    // with the bulk drift as the field was. It is what a satellite image near the
    // terminator is made of — long shadows behind every tower — and at noon,
    // with the sun overhead, it fades to nothing, as it does there. It replaced a
    // 3-octave "wisp" opacity sample, so the shader got one sample of 6 octaves
    // and lost one of 3.
    let sun_along = sun_dir.xyz - normal * ndl;
    let sun_drift = rotate_about(sun_along, axis, params.z * BULK_DRIFT);
    let sun_step = normalize(sun_drift + vec3<f32>(1e-6)) * RELIEF_STEP;
    let toward_sun = fbm3_detail(base + 2.2 * rw + sun_step, DENSITY_OCTAVES).y;
    // Weighted by the shadow's length, tan of the sun's zenith angle: none
    // under an overhead sun, long near the terminator.
    let shadow_len = length(sun_along) / max(ndl, 0.12);
    let relief = clamp(
        1.0 + (densities.x - toward_sun) * RELIEF_GAIN * shadow_len,
        RELIEF_MIN,
        RELIEF_MAX,
    );

    // The sun's colour at this cloud, not at the map anchor: reddened by the
    // air it crosses, which grows as it nears the terminator. Kasten–Young's
    // air mass, roughly, through a quarter of the column (the tops sit high, and
// half of it tinted every mid-latitude noon cream), on the
    // same Rayleigh depths `utils::sun` uses. This is the pink and gold band
    // along the terminator.
    let air_mass = min(1.0 / (max(ndl, -0.06) + 0.12), 14.0);
    let sun_tint = exp(-RAYLEIGH_DEPTH * 0.25 * air_mass);
    // Clouds are lit across the terminator a little (their tops are high), and
    // dim toward it as the sun lowers rather than holding full white to the
    // edge.
    let day = smoothstep(-0.10, 0.12, ndl);
    let lum = 0.35 + 0.65 * sqrt(clamp(ndl, 0.0, 1.0));
    let lit = lit_color.rgb * sun_tint * lum * relief;
    let col = mix(shadow_color.rgb, lit, day);

    return vec4<f32>(col, clamp(a, 0.0, 1.0));
}
