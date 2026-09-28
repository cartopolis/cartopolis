// Procedural cloud ceiling: two camera-centred decks, projected onto flat layers
// above the camera and shaded with a *scattering* model rather than a surface one.
//
// The geometry is unchanged — one static shell mesh, no churn, so it stays clear
// of the iGPU command-buffer budget (see CLAUDE.md) — and each above-the-horizon
// view ray is still projected onto a cloud plane and sampled for coverage there.
// What changed is the lighting, which is the whole reason a projected plane can
// read as cloud at all while a Lambert-shaded noise field reads as painted
// geometry (`docs/notes/cloud-lighting.md` has the derivation and the sources):
//
//   * light is scattered, not reflected — a Henyey-Greenstein dual lobe makes the
//     sun-side rim glow (the silver lining) and adds a weaker backscatter lobe;
//   * the sun is occluded by a short march *through the deck toward the sun*, not
//     by a fake surface normal built from the density gradient;
//   * a Beer-Powder term darkens thin edges when the sun is behind the camera,
//     which is what makes a cloud read as thick rather than as fog;
//   * three scattering octaves keep thick bases luminous grey instead of black;
//   * and the deck recedes into the horizon haze by aerial perspective instead of
//     fading its alpha to nothing — the sky used to visibly *end* at the horizon.
//
// The two decks sit at different heights, so translating the camera slides them
// past each other. That parallax is the one cue a single field can never have.
//
// World-anchoring: `params2.xy` is the camera's horizontal displacement from the
// map anchor in **metres** (each deck applies its own frequency, so the shared
// FREQ constant this file used to duplicate on the Rust side is gone), which
// keeps the clouds fixed over the ground so you fly under them. The integer hash
// stays stable at the large coordinates that produces — the old sin() hash banded
// and shimmered once the offset grew.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

// x: coverage 0..1 (fraction of sky filled). y: edge softness. z: time seconds
// (drives wind). w: low deck height above the camera, in metres.
@group(3) @binding(0) var<uniform> params0: vec4<f32>;
// xy: wind velocity in metres/sec. z: space fade (1 = fully hidden in orbit).
// w: altitude fade (1 = full clouds, 0 = faded out at high flight).
@group(3) @binding(1) var<uniform> params1: vec4<f32>;
// xyz: world direction TO the sun. w: deck thickness in metres (the vertical
// extent the sun march traverses).
@group(3) @binding(2) var<uniform> sun_dir: vec4<f32>;
// rgb: sunlight reaching the deck's altitude — already extincted along the beam's
// path down to that height, which is *less* air than the ground sees. a: overall
// cloud opacity, before the coverage/altitude/space fades.
@group(3) @binding(3) var<uniform> sun_color: vec4<f32>;
// rgb: ambient sky light falling on the deck (what fills the shadowed side).
// a: opacity scale for the high deck, so the cirrus layer can be tuned alone.
@group(3) @binding(4) var<uniform> sky_color: vec4<f32>;
// xy: world-anchor offset in metres. z: the camera fog's per-metre density, for
// aerial perspective. w: high deck height above the camera, in metres.
@group(3) @binding(5) var<uniform> params2: vec4<f32>;
// rgb: the colour the deck recedes into with distance — the same fog colour the
// horizon-haze shell paints, so cloud and sky reach the horizon together. It is
// also what `sky_color` is now taken from, which is what stops an overcast deck
// from meeting the horizon in a step (see `update_clouds`).
// a: the fraction of `sky_color` an opaque core still receives — low for scattered
// cumulus (a dark base against a bright sky), high for overcast (the deck *is*
// the sky). Rust decides it from the coverage; see `cloud_sky_floor`.
@group(3) @binding(6) var<uniform> haze_color: vec4<f32>;

// Base noise frequency of the low deck: metres⁻¹ in its projected plane.
const FREQ_LOW: f32 = 0.00034;
// The high deck is finer-grained in world terms — cirrus streaks, not cumulus
// billows — and it is farther away, so both effects push the same way on screen.
const FREQ_HIGH: f32 = 0.00070;
// The upper deck rides a faster wind than the lower one, which is both true and
// the second half of the parallax (height alone only separates them when the
// *camera* moves; differing wind separates them when nothing moves at all).
const HIGH_WIND_SCALE: f32 = 1.45;
// Cirrus is patchier than the deck below it at the same reported cover.
const HIGH_COVER_SCALE: f32 = 0.75;

// Peak optical depth of a fully-covered column through the low deck. This sets
// how opaque a core gets (alpha = 1 − exp(−d), Beer's law, so cores saturate
// smoothly instead of clipping) and how hard the deck self-shadows.
const OPTICAL_DEPTH_LOW: f32 = 7.0;
// The high deck is thin by construction — cirrus you can see the sun through.
const OPTICAL_DEPTH_HIGH: f32 = 1.6;

// ---- quality tier -----------------------------------------------------------
// `CLOUDS_LOW` is set by `CloudMaterial::specialize` on `quality::Tier::Phone`.
//
// This shader was the most expensive fragment program the phone ran and the only
// one with no tier knob at all: 14 FBM octaves before the alpha bail — ~56 integer
// hashes per pixel over the whole upper half of the screen — and the march past it
// adds nine more. Its one existing quality control is the *distance* LOD below,
// which is about aliasing rather than cost.
//
// What the low tier gives up is structure, not weather: half the octaves in each
// deck, no domain warp (the curl that makes a billow lean), a single-tap sun march
// and one fewer multiple-scattering octave. It reads as a flatter, softer sky,
// which on a 5.7" panel is most of what a 60 km cloud deck reads as anyway.
#ifdef CLOUDS_LOW
const OCT_LOW: i32 = 3;
const OCT_LOW_BILLOW: i32 = 1;
const OCT_HIGH: i32 = 2;
const SUN_STEPS: i32 = 1;
const MS_OCTAVES: i32 = 2;
#else
const OCT_LOW: i32 = 6;
const OCT_LOW_BILLOW: i32 = 2;
const OCT_HIGH: i32 = 4;
// Taps in the sun march. Three is enough because the field it marches is
// low-frequency; each tap costs a whole (small) FBM.
const SUN_STEPS: i32 = 3;
const MS_OCTAVES: i32 = 3;
#endif

// Henyey-Greenstein lobes. The forward peak is the silver lining; the weaker
// backward lobe is the bright side you see with the sun behind you.
const HG_FORWARD: f32 = 0.80;
const HG_BACKWARD: f32 = -0.30;
const HG_MIX: f32 = 0.40;
// The forward lobe runs to ~45× isotropic at g = 0.8, which is a physically real
// spike that we are not equipped to spend: nothing here conserves energy, so it
// would simply blow out the pixels nearest the sun. Cap it. This is an art
// constant standing in for the normalisation we don't do.
const MAX_PHASE: f32 = 8.0;

// Multiple-scattering approximation (Wrenninge et al. 2013 / Hillaire 2016):
// evaluate single scattering several times, each octave seeing a fraction of the
// extinction, contributing a fraction of the energy and carrying a fraction of
// the anisotropy. It costs three multiplies and no extra samples, and it is what
// keeps a thick cloud base a luminous grey rather than collapsing to black.
// (Octave count is tiered, above.)
const MS_ATTENUATION: f32 = 0.5;
const MS_CONTRIBUTION: f32 = 0.5;
const MS_ECCENTRICITY: f32 = 0.5;

// Domain-warp strength, in UV units of the low deck's field.
const WARP: f32 = 0.28;

// Evolution: the field churns as well as drifting. The wind alone carries the
// whole pattern rigidly, which reads as a decal sliding across the sky; a real
// deck reshapes as it goes. Both motions below are relative to the wind-carried
// frame and both are free — an offset per sample, not another sample:
//
//   * the domain warp's own field moves across the wind at EVOLVE_WARP (warp-field
//     units per second, one unit ≈ 6.5 km), so every billow's lean and curl
//     slowly changes;
//   * octave i of every fbm slides EVOLVE_DETAIL × i further than the base (in
//     that octave's own units per second), so the fine edges roil while the mass
//     they sit on holds. Octave 0 does not slide: the shape of a cloud is set by
//     the wind, only its surface churns.
//
// Scaled to the wind's own time-lapse (CLOUD_WIND crosses the sky in under a
// minute): an edge changes over ~15 s, a whole cloud over a minute or two.
const EVOLVE_WARP: vec2<f32> = vec2<f32>(-0.0031, 0.0052);
const EVOLVE_DETAIL: vec2<f32> = vec2<f32>(0.011, -0.007);

// Detail LOD range, in metres of slant range. A pixel's footprint on the cloud
// plane grows without bound toward the horizon, so the high octaves alias into
// shimmer there. Fade them out with range instead.
const DETAIL_NEAR_M: f32 = 5000.0;
const DETAIL_FAR_M: f32 = 60000.0;

// Grazing cut. Rays this shallow project to a degenerate, near-infinite footprint
// and carry no usable signal. It is deliberately narrow — the aerial perspective
// below, not an alpha fade, is what makes the deck recede.
const GRAZE_MIN: f32 = 0.006;
const GRAZE_MAX: f32 = 0.030;

// Effective thickness of the haze layer, in metres. `DistanceFog` is a *uniform*
// medium, which is right along the ground and badly wrong looking up: at the
// default density its 7 km visibility would put ~30 % of the fog colour on a
// cloud straight overhead, because it charges the full 2.5 km of the vertical
// path at ground density. Real haze sits in the boundary layer, so a ray leaves
// it after climbing a kilometre or so and the rest of the trip is clear. Bounding
// the *path* rather than the density is what keeps the zenith clean while still
// burying the horizon, where the same slab is a 30 km traverse.
const HAZE_LAYER_M: f32 = 1000.0;

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

// Fractal noise.
//
// `billow_octaves` of the low octaves are folded (`1 − |2v − 1|`), which puts a
// crease at every zero crossing and reads as the cauliflower edge of a cumulus;
// plain value-noise FBM alone is characteristically smoky. Perlin-Worley is the
// textbook answer here (it is what Nubis uses) and looks better still, but an F1
// cellular lookup is nine hashes per octave and this shader is fill-bound.
//
// `detail` in 0..1 progressively silences the high octaves. A muted octave
// contributes its *mean* (0.5) rather than dropping out, so the field's average —
// and therefore the coverage threshold, and therefore how much sky is filled —
// does not shift with range. Octave 0 is never muted.
//
// `evolve` is the per-octave slide (see EVOLVE_DETAIL): octave i samples its noise
// `evolve × i` away from where the wind put it.
fn fbm(p: vec2<f32>, octaves: i32, billow_octaves: i32, detail: f32, evolve: vec2<f32>) -> f32 {
    var v = 0.0;
    var amp = 0.5;
    var norm = 0.0;
    var q = p;
    let n = f32(octaves);
    for (var i = 0; i < octaves; i = i + 1) {
        let w = clamp((detail - f32(i) / n) * n + 1.0, 0.0, 1.0);
        let raw = value_noise(q + evolve * f32(i));
        var s = raw;
        if i < billow_octaves {
            s = 1.0 - abs(2.0 * raw - 1.0);
        }
        v += amp * mix(0.5, s, w);
        norm += amp;
        q = q * 2.03 + vec2<f32>(37.1, 17.3);
        amp *= 0.55;
    }
    return v / norm;
}

// Henyey-Greenstein, normalised so isotropic (g = 0) returns 1 — this is a
// shading weight, not a probability density.
fn hg(cos_theta: f32, g: f32) -> f32 {
    let g2 = g * g;
    let d = max(1.0 + g2 - 2.0 * g * cos_theta, 1e-4);
    return (1.0 - g2) / (d * sqrt(d));
}

// Dual lobe. `scale` shrinks both eccentricities for the multiple-scattering
// octaves — light that has bounced several times has forgotten which way it came.
fn dual_hg(cos_theta: f32, scale: f32) -> f32 {
    let p = mix(
        hg(cos_theta, HG_FORWARD * scale),
        hg(cos_theta, HG_BACKWARD * scale),
        HG_MIX,
    );
    return min(p, MAX_PHASE);
}

// Sum of single-scattering evaluations with geometrically decaying extinction,
// energy and anisotropy — each octave stands in for one more scattering event.
// Normalised by the total contribution so the isotropic case stays ~1.
fn multi_scatter(sun_od: f32, cos_theta: f32) -> f32 {
    var att = 1.0;
    var contrib = 1.0;
    var ecc = 1.0;
    var total = 0.0;
    var norm = 0.0;
    for (var i = 0; i < MS_OCTAVES; i = i + 1) {
        total += contrib * dual_hg(cos_theta, ecc) * exp(-sun_od * att);
        norm += contrib;
        att *= MS_ATTENUATION;
        contrib *= MS_CONTRIBUTION;
        ecc *= MS_ECCENTRICITY;
    }
    return total / norm;
}

// Optical depth from a point on the deck toward the sun, by marching the coverage
// field along the sun's horizontal displacement per unit rise (sun.xz / sun.y)
// over the deck's thickness.
//
// The classic cheap version of this offsets a single shadow tap by the sun's
// horizontal direction alone, which has a trap: the offset — and with it all
// self-shadowing — vanishes as the sun approaches the zenith. Dividing by sun.y
// is what makes the shadow *lengthen* as the sun drops, which is the behaviour
// that sells a low sun; the clamp keeps a sun on the horizon from marching to
// infinity.
fn sun_optical_depth(
    uv: vec2<f32>,
    sun: vec3<f32>,
    thickness_m: f32,
    freq: f32,
    thresh: f32,
    soft: f32,
    detail: f32,
    evolve: vec2<f32>,
) -> f32 {
    let run_m = thickness_m * sun.xz / max(sun.y, 0.20);
    let step = run_m * freq / f32(SUN_STEPS);
    var sum = 0.0;
    for (var i = 1; i <= SUN_STEPS; i = i + 1) {
        // The same churn as the visible field, or the self-shadow would stay put
        // while the cloud casting it reshaped.
        let s = fbm(uv + step * f32(i), 3, 2, detail, evolve);
        sum += smoothstep(thresh, thresh + soft, s);
    }
    // Mean coverage along the path × the deck's depth × the obliquity of the path
    // itself (a low sun travels farther through the same slab of cloud).
    let slant = clamp(1.0 / max(sun.y, 0.15), 1.0, 4.0);
    return (sum / f32(SUN_STEPS)) * OPTICAL_DEPTH_LOW * slant;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let dir = normalize(in.world_position.xyz - view.world_position.xyz);

    // Only rays that climb toward the ceiling carry cloud.
    if dir.y <= GRAZE_MIN {
        discard;
    }

    let cover = params0.x;
    let soft = max(params0.y, 1e-3);
    let thresh = 1.0 - cover;
    // The sample point is absolute world position: camera-relative projection plus
    // the camera's own displacement from the anchor, both in metres. Wind drifts
    // the whole field on top of that.
    let anchor_m = params2.xy;
    let wind_m = params1.xy * params0.z;
    // The churn on top of the wind (see EVOLVE_WARP / EVOLVE_DETAIL).
    let evolve = EVOLVE_DETAIL * params0.z;

    // ---- low deck ---------------------------------------------------------
    let t_low = params0.w / dir.y;
    let uv_low_base = (dir.xz * t_low + anchor_m + wind_m) * FREQ_LOW;
    let detail_low = clamp(
        1.0 - (t_low - DETAIL_NEAR_M) / (DETAIL_FAR_M - DETAIL_NEAR_M),
        0.0,
        1.0,
    );
    // Domain warp: displace the sample by a low-frequency field of its own, so the
    // billows lean and curl instead of sitting square on the noise lattice. Two
    // more FBMs, and the first thing the low tier drops — it is the most expensive
    // thing here that is pure shape.
#ifdef CLOUDS_LOW
    let uv_low = uv_low_base;
#else
    // The warp field moves on its own across the wind, which is what makes a
    // cloud reshape rather than slide.
    let warp_uv = uv_low_base * 0.45 + EVOLVE_WARP * params0.z;
    let w0 = fbm(warp_uv, 2, 0, detail_low, evolve) - 0.5;
    let w1 = fbm(warp_uv + vec2<f32>(5.2, 1.3), 2, 0, detail_low, evolve) - 0.5;
    let uv_low = uv_low_base + vec2<f32>(w0, w1) * WARP;
#endif

    let shape_low = smoothstep(
        thresh,
        thresh + soft,
        fbm(uv_low, OCT_LOW, OCT_LOW_BILLOW, detail_low, evolve),
    );

    // ---- high deck --------------------------------------------------------
    let t_high = params2.w / dir.y;
    let uv_high = (dir.xz * t_high + anchor_m + wind_m * HIGH_WIND_SCALE) * FREQ_HIGH;
    let detail_high = clamp(
        1.0 - (t_high - DETAIL_NEAR_M) / (DETAIL_FAR_M - DETAIL_NEAR_M),
        0.0,
        1.0,
    );
    let thresh_high = 1.0 - cover * HIGH_COVER_SCALE;
    // Cirrus streaks churn too, a little slower than the deck below them.
    let shape_high = smoothstep(
        thresh_high,
        thresh_high + soft,
        fbm(uv_high, OCT_HIGH, 0, detail_high, evolve * 0.7),
    );

    // ---- opacity ----------------------------------------------------------
    // Beer's law on the view ray, so a core saturates smoothly rather than being
    // clipped by the `pow()` curve this used to shape the alpha with.
    let od_low = shape_low * OPTICAL_DEPTH_LOW;
    let od_high = shape_high * OPTICAL_DEPTH_HIGH;

    let graze = smoothstep(GRAZE_MIN, GRAZE_MAX, dir.y);
    let fade = sun_color.a * params1.w * (1.0 - params1.z) * graze;
    let a_low = (1.0 - exp(-od_low)) * fade;
    let a_high = (1.0 - exp(-od_high)) * sky_color.a * fade;
    let alpha = a_low + a_high * (1.0 - a_low);
    // Bail before the march: on a part-covered sky this is most of the screen, and
    // everything below here is the expensive half of the shader.
    if alpha <= 0.002 {
        discard;
    }

    // ---- lighting ---------------------------------------------------------
    let sun = normalize(sun_dir.xyz);
    let cos_theta = dot(dir, sun);

    let sun_od = sun_optical_depth(uv_low, sun, sun_dir.w, FREQ_LOW, thresh, soft, detail_low, evolve);
    let ms = multi_scatter(sun_od, cos_theta);
    // Beer-Powder (Schneider, Nubis / Horizon Zero Dawn). The powder factor
    // darkens thin edges — the reason a cumulus reads as solid rather than as
    // fog — but only when the sun is behind the camera. Looking *into* the sun
    // those same edges are the brightest thing in the sky, and applying powder
    // there would cancel exactly the silver lining the phase function just built.
    let powder = 1.0 - exp(-2.0 * od_low);
    let powder_w = clamp(0.5 - 0.5 * cos_theta, 0.0, 1.0);
    // Cloud bases go dark, but never black — the ground bounces light back up at
    // them, and an overcast deck is the sky's own light source rather than
    // something standing in front of it. How dark is `haze_color.a`, above.
    let sky_floor = haze_color.a;
    let low_rgb = sun_color.rgb * ms * mix(1.0, powder, powder_w)
        + sky_color.rgb * (sky_floor + (1.0 - sky_floor) * (1.0 - shape_low));

    // The high deck is thin enough that self-shadowing is not a thing it does:
    // phase function and ambient only, no march.
    let high_rgb = sun_color.rgb * dual_hg(cos_theta, 1.0) * exp(-od_high)
        + sky_color.rgb * (1.0 - 0.5 * shape_high);

    // Composite low over high in premultiplied alpha, then divide back out.
    var rgb = (low_rgb * a_low + high_rgb * a_high * (1.0 - a_low)) / max(alpha, 1e-4);

    // ---- aerial perspective ----------------------------------------------
    // Bevy composites its own aerial perspective over opaque geometry only, so an
    // alpha-blended shell never receives any. Without this the deck kept full
    // contrast right up to the last visible pixel and then had its alpha faded
    // out, which reads as the sky *ending* rather than as cloud receding into air.
    // Extinction toward the same fog colour the horizon shell paints puts the far
    // deck behind the same air as the far ground. The curve is squared to match
    // `FogFalloff::ExponentialSquared`, which is what `free_camera::distance_fog`
    // builds and therefore what every other surface in the scene is wearing; the
    // path is the ray's traverse of the haze layer, not the whole trip to the deck.
    let haze_path = min(t_low, HAZE_LAYER_M / dir.y);
    let x = haze_path * params2.z;
    rgb = mix(haze_color.rgb, rgb, exp(-x * x));

    return vec4<f32>(rgb, clamp(alpha, 0.0, 1.0));
}
