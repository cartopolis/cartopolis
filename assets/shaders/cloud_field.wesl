// The ground cloud deck as a *field*: where the cloud is, how it is lit, and how
// it churns. Three shaders draw it and all import this, so they cannot disagree:
// `clouds.wgsl` paints the deck overhead, and `water.wgsl` and `ground_detail.wgsl`
// (the flat map's water past the streamed ring) find the same deck along a
// reflected ray — `mirrored`, at the bottom — so a lake mirrors the clouds that
// are actually there. `docs/notes/cloud-lighting.md` has the lighting derivation.

import bevy_pbr::render::mesh_view_bindings as view_bindings;

// The deck as the shared buffer lays it out (`sky::CloudFieldGpu`): the shell
// material's eight uniforms, in binding order. Each field means what the
// `clouds.wgsl` binding in the same place means — `shape` is its `params0`,
// `motion` its `params1`, `place` its `params2`, `light` its `params3`, and the
// colours and the sun keep their names. Renamed rather than kept, because naga's
// writer renames a member that ends in a digit (`params0_`), and naga_oil refuses
// an imported struct whose names would not survive that. All `vec4`, so it has
// one layout on every driver.
//
// A consumer binds the buffer itself and builds one of these out of its fields
// one at a time — `Deck(clouds.shape, clouds.motion, …)` — rather than by
// loading the storage struct whole. The Adreno 840 reads a by-value copy of a
// bound struct wrong (`docs/notes/adreno-atmosphere-struct-copy.md`); an all-vec4
// struct probably escapes that, but a field load certainly does.
struct Deck {
    shape: vec4<f32>,
    motion: vec4<f32>,
    sun_dir: vec4<f32>,
    sun_color: vec4<f32>,
    sky_color: vec4<f32>,
    place: vec4<f32>,
    haze_color: vec4<f32>,
    light: vec4<f32>,
}

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

// Frequency step between octaves of every fbm here.
const LACUNARITY: f32 = 2.03;

// Octaves of the low deck on the full and the cheap tier (see `relief_field`
// for which are puffs). The shell picks its count by shader def (`CLOUDS_LOW`,
// set on the phone tier); the mirror is compiled into the water's and the
// ground's pipelines, which have no such def, and picks the same count by the
// flag the deck carries (`light.y`) — so a lake finds the shapes the sky has.
const OCT_LOW_FULL: i32 = 6;
const OCT_LOW_CHEAP: i32 = 3;

fn low_octaves(cheap: bool) -> i32 {
    return select(OCT_LOW_FULL, OCT_LOW_CHEAP, cheap);
}

// Grazing cut. Rays this shallow project to a degenerate, near-infinite footprint
// and carry no usable signal. It is deliberately narrow — the aerial perspective,
// not an alpha fade, is what makes the deck recede.
const GRAZE_MIN: f32 = 0.006;
const GRAZE_MAX: f32 = 0.030;

// Peak optical depth of a fully-covered column through the low deck. This sets
// how opaque a core gets (alpha = 1 − exp(−d), Beer's law, so cores saturate
// smoothly instead of clipping) and how hard the deck self-shadows.
const OPTICAL_DEPTH_LOW: f32 = 7.0;
// The high deck is thin by construction — cirrus you can see the sun through.
const OPTICAL_DEPTH_HIGH: f32 = 1.6;

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
const MS_ATTENUATION: f32 = 0.5;
const MS_CONTRIBUTION: f32 = 0.5;
const MS_ECCENTRICITY: f32 = 0.5;

// Domain-warp strength, in UV units of the low deck's field.
const WARP: f32 = 0.12;

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
// Both run on the scene clock in real seconds: an edge changes over ~15 s, a
// whole cloud over a minute or two. The drift underneath them is the live wind,
// sped up by `sky::CLOUD_TIME_LAPSE` and integrated on the CPU.
const EVOLVE_WARP: vec2<f32> = vec2<f32>(-0.0031, 0.0052);
const EVOLVE_DETAIL: vec2<f32> = vec2<f32>(0.011, -0.007);

// Detail LOD range, in metres of slant range. A pixel's footprint on the cloud
// plane grows without bound toward the horizon, so the high octaves alias into
// shimmer there. Fade them out with range instead.
const DETAIL_NEAR_M: f32 = 5000.0;
const DETAIL_FAR_M: f32 = 60000.0;

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

// Fractal value noise: the warp's field and the cirrus. The low deck has its
// own, `relief_field`, which says why it is not this.
//
// `detail` in 0..1 progressively silences the high octaves. A muted octave
// contributes its *mean* (0.5) rather than dropping out, so the field's average —
// and therefore the coverage threshold, and therefore how much sky is filled —
// does not shift with range. Octave 0 is never muted.
//
// `evolve` is the per-octave slide (see EVOLVE_DETAIL): octave i samples its noise
// `evolve × i` away from where the wind put it.
//
// A fully muted octave skips its noise altogether. That is exact, not an
// approximation — it contributes its mean whatever the noise says — and it is
// what lets a caller ask for the full octave count at low `detail` and pay only
// for the octaves it keeps. The mirror does exactly that: it must run the shell's
// octave count, or the normalisation differs and the same field lands on
// different shapes, but a rough mirror kilometres from the deck keeps only one
// or two of them. `relief_field` mutes the same way.
fn fbm(p: vec2<f32>, octaves: i32, detail: f32, evolve: vec2<f32>) -> f32 {
    var v = 0.0;
    var amp = 0.5;
    var norm = 0.0;
    var q = p;
    let n = f32(octaves);
    for (var i = 0; i < octaves; i = i + 1) {
        let w = clamp((detail - f32(i) / n) * n + 1.0, 0.0, 1.0);
        var s = 0.5;
        if w > 0.0 {
            s = value_noise(q + evolve * f32(i));
        }
        v += amp * mix(0.5, s, w);
        norm += amp;
        q = q * LACUNARITY + vec2<f32>(37.1, 17.3);
        amp *= 0.55;
    }
    return v / norm;
}

// The low deck's field, as the relief stands it up and the mirror reflects it.
//
// A cumulus is a heap of rounded puffs with sharp creases between them — a
// cauliflower. `|2v − 1|` is exactly that shape in a noise octave: it folds
// every zero crossing into a cusped valley and leaves the extremes as round
// humps. The billow fold `fbm` uses (`1 − |2v − 1|`) is its upside-down twin,
// round valleys under knife-edge ridges, and stood up as a height map those
// ridges came out as paper folds; rounding them off (`fbm_blurred`) left smooth
// blobs that read as liquid. So here the masses (the first `PUFF_FROM` octaves)
// are plain noise, the middle octaves are puffs, and the last is plain grain.
//
// `detail` mutes octaves as `fbm`'s does. `blur` rounds the puffs' creases as
// though the field had been averaged over `blur` base octaves — the mirror's
// rough water, which cannot show a crease it cannot resolve.
fn relief_field(p: vec2<f32>, octaves: i32, detail: f32, evolve: vec2<f32>, blur: f32) -> f32 {
    var v = 0.0;
    var amp = 0.5;
    var norm = 0.0;
    var q = p;
    var crease = blur;
    let n = f32(octaves);
    let puff_from = select(PUFF_FROM_FULL, PUFF_FROM_CHEAP, octaves <= OCT_LOW_CHEAP);
    for (var i = 0; i < octaves; i = i + 1) {
        let w = clamp((detail - f32(i) / n) * n + 1.0, 0.0, 1.0);
        var s = 0.5;
        if w > 0.0 {
            let raw = value_noise(q + evolve * f32(i));
            s = raw;
            if i >= puff_from && i < octaves - 1 {
                let x = 2.0 * raw - 1.0;
                let c = min(crease, 1.0);
                // `|x|`, with its cusp rounded to `√(x² + c²)` and put back on 0..1.
                s = (sqrt(x * x + c * c) - c) / (sqrt(1.0 + c * c) - c);
            }
        }
        v += amp * mix(0.5, s, w);
        norm += amp;
        q = q * LACUNARITY + vec2<f32>(37.1, 17.3);
        amp *= 0.55;
        crease *= LACUNARITY;
    }
    return v / norm;
}

// The first octave of the relief field that is a puff, on each tier: below it
// the cloud masses, from it the cauliflower. On the cheap tier's three octaves
// that is the middle one alone.
const PUFF_FROM_FULL: i32 = 2;
const PUFF_FROM_CHEAP: i32 = 1;

// How much of a pixel's high-octave detail survives at this slant range.
fn detail_at(range_m: f32) -> f32 {
    return clamp(1.0 - (range_m - DETAIL_NEAR_M) / (DETAIL_FAR_M - DETAIL_NEAR_M), 0.0, 1.0);
}

// The low deck's field coordinate for the point `offset_m` (metres, horizontal)
// from the camera: the camera's own place over the ground (`anchor_m`) and the
// wind's accumulated drift (`drift_m`) on top. Metres in, UV out.
fn low_uv(offset_m: vec2<f32>, anchor_m: vec2<f32>, drift_m: vec2<f32>) -> vec2<f32> {
    return (offset_m + anchor_m + drift_m) * FREQ_LOW;
}

// The warped sample point of the low deck. Domain warp displaces the sample by a
// low-frequency field of its own, so the billows lean and curl instead of sitting
// square on the noise lattice — and that field moves on its own across the wind,
// which is what makes a cloud reshape rather than slide. Two more FBMs, and the
// first thing the cheap tier drops.
fn warp(uv_base: vec2<f32>, time_s: f32, detail: f32) -> vec2<f32> {
    let evolve = EVOLVE_DETAIL * time_s;
    let warp_uv = uv_base * 0.45 + EVOLVE_WARP * time_s;
    let w0 = fbm(warp_uv, 2, detail, evolve) - 0.5;
    let w1 = fbm(warp_uv + vec2<f32>(5.2, 1.3), 2, detail, evolve) - 0.5;
    return uv_base + vec2<f32>(w0, w1) * WARP;
}

// `warp` on the full tier, and nothing on the cheap one — for the shell, which
// knows its tier by shader def.
fn low_warped(uv_base: vec2<f32>, time_s: f32, detail: f32) -> vec2<f32> {
    @if(CLOUDS_LOW) {
        return uv_base;
    } @else {
        return warp(uv_base, time_s, detail);
    }
}

// Optical depth from a point on the deck toward the sun, by marching the coverage
// field along the sun's horizontal displacement per unit rise (sun.xz / sun.y)
// over the deck's thickness, in `steps` taps.
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
    thresh: f32,
    soft: f32,
    detail: f32,
    evolve: vec2<f32>,
    steps: i32,
) -> f32 {
    let run_m = thickness_m * sun.xz / max(sun.y, 0.20);
    let step = run_m * FREQ_LOW / f32(steps);
    var sum = 0.0;
    for (var i = 1; i <= steps; i = i + 1) {
        // The same churn as the visible field, or the self-shadow would stay put
        // while the cloud casting it reshaped.
        let s = relief_field(uv + step * f32(i), low_octaves(true), detail, evolve, 0.0);
        sum += smoothstep(thresh, thresh + soft, s);
    }
    // Mean coverage along the path × the deck's depth × the obliquity of the path
    // itself (a low sun travels farther through the same slab of cloud).
    let slant = clamp(1.0 / max(sun.y, 0.15), 1.0, 4.0);
    return (sum / f32(steps)) * OPTICAL_DEPTH_LOW * slant;
}

// ---- the low deck in relief -------------------------------------------------
//
// The shell used to draw the low deck as a sheet: each view ray met one plane
// and read the field there. A sheet has no sides, so toward the horizon every
// cloud foreshortened to a sliver and the far deck was a stack of stripes, and
// nothing had a top for the sun to light. In relief the field is a height map:
// a column stands from the deck's flat base up to `relief_column` of its
// thickness, so a cloud has a flat base, a rounded top (the field's maxima) and
// sides, and a ray climbing through the slab meets whichever comes first — a
// base overhead, a side toward the horizon. The mirror keeps the sheet: what it
// reflects is blurred far past the difference.

// Field excess over the threshold at which a column reaches the deck's top, for
// scattered cloud and for a closed deck.
const RELIEF_SPAN_SCATTERED: f32 = 0.3;
const RELIEF_SPAN_OVERCAST: f32 = 0.35;
// The least field value that raises a column at all, whatever the cover. Under a
// closed deck the threshold is 0 and every point is cloud; measuring thickness
// from here rather than from 0 is what leaves thin, bright places between the
// thick, dark cells of an overcast instead of one even lid.
const RELIEF_COLUMN_FLOOR: f32 = 0.30;
// Softness of a cloud's surface, in field units: how far outside a column a ray
// may pass and still pick some of it up. This is what an edge looks like; the
// sheet's was six times wider, which is why its clouds read as smears.
const RELIEF_SOFT: f32 = 0.05;
// Octaves of the field the march climbs through. The coarse ones decide where
// the clouds stand; the fine ones are paid for once, at the hit.
const RELIEF_MARCH_OCTAVES: i32 = 4;
// The field value above which the low deck is cloud, at covers 0, 0.05, … 1 —
// so that `cover` really is the fraction of the sky under cloud. Computed, not
// tuned: a TypeScript mirror of `relief_field` run over 600,000 points
// (2026-10-17), counting what the relief draws — a column wherever the march's
// coarse field clears the threshold, faded by the full field's edge. The field
// is narrow (80 % of it within ±0.17 of its median), so `1 − cover`, which the
// sheet used, was far off: with crisp edges it put four fifths of the sky under
// a "45 %" deck; the sheet's wide soft edges had hidden that by leaving most of
// its cloud half transparent. Change the field (octaves, puffs,
// `RELIEF_MARCH_OCTAVES`, `RELIEF_SOFT`) and these move.
fn low_threshold(cover: f32, cheap: bool) -> f32 {
    var full = array<f32, 21>(
        1.000, 0.669, 0.627, 0.597, 0.572, 0.551, 0.531, 0.512, 0.495, 0.477, 0.460,
        0.443, 0.426, 0.408, 0.389, 0.369, 0.348, 0.323, 0.294, 0.252, 0.000,
    );
    var low = array<f32, 21>(
        1.000, 0.686, 0.637, 0.602, 0.574, 0.550, 0.527, 0.506, 0.485, 0.465, 0.445,
        0.426, 0.406, 0.385, 0.364, 0.341, 0.317, 0.289, 0.255, 0.210, 0.000,
    );
    let x = clamp(cover, 0.0, 1.0) * 20.0;
    let i = min(i32(x), 19);
    let f = x - f32(i);
    if cheap {
        return mix(low[i], low[i + 1], f);
    }
    return mix(full[i], full[i + 1], f);
}

fn relief_span(cover: f32) -> f32 {
    return mix(RELIEF_SPAN_SCATTERED, RELIEF_SPAN_OVERCAST, smoothstep(0.6, 1.0, cover));
}

// The field value a point `h` (0..1) up the deck needs to be inside a cloud.
// Quadratic in `h`, so a column's top is a dome: its sides climb steeply off the
// flat base and round over at the top, as a cumulus does, where a linear profile
// stood every cloud up as a cone.
fn relief_floor(h: f32, thresh: f32, span: f32) -> f32 {
    return thresh + span * h * h;
}

// Height of the column (0..1 of the deck) a field value raises — the `h` at
// which `relief_floor` reaches it, measured from `RELIEF_COLUMN_FLOOR` up.
//
// Toward a closed deck it eases from that square root to a straight line from
// `RELIEF_OVERCAST_MIN`. There every point is cloud and the column is only how
// thick it is, which the light reads. The root's vertical start at the floor,
// and a column of nothing under a deck with no gaps, drew every thin place as a
// hard-edged dark blotch: the powder term darkens what it takes for a thin edge.
fn relief_column(v: f32, thresh: f32, span: f32, cover: f32) -> f32 {
    let x = clamp((v - max(thresh, RELIEF_COLUMN_FLOOR)) / span, 0.0, 1.0);
    let closed = RELIEF_OVERCAST_MIN + (1.0 - RELIEF_OVERCAST_MIN) * x;
    return mix(sqrt(x), closed, smoothstep(0.6, 1.0, cover));
}

// The thinnest a closed deck gets, as a fraction of its thickness.
const RELIEF_OVERCAST_MIN: f32 = 0.3;

// The `detail` that keeps only the first `keep` of `octaves` octaves of `fbm`
// (see its muting), never more than `detail` itself would.
fn detail_keeping(keep: i32, octaves: i32, detail: f32) -> f32 {
    return min(detail, f32(keep - 1) / f32(octaves));
}

// Optical depth toward the sun from a point `h` (0..1) up the deck, by marching
// the relief: each tap climbs toward the sun and counts as cloud only where a
// column stands taller than the tap. A point near a top sees open sky after one
// tap, a point on a base the whole cloud — which is what lights a cumulus's
// crown and leaves its base in shade.
fn relief_sun_od(
    uv: vec2<f32>,
    h: f32,
    sun: vec3<f32>,
    thickness_m: f32,
    thresh: f32,
    span: f32,
    detail: f32,
    evolve: vec2<f32>,
    octaves: i32,
    steps: i32,
) -> f32 {
    let rise = 1.0 - h;
    let run_uv = thickness_m * rise * sun.xz / max(sun.y, 0.20) * FREQ_LOW;
    var sum = 0.0;
    for (var i = 1; i <= steps; i = i + 1) {
        let f = f32(i) / f32(steps);
        let v = relief_field(uv + run_uv * f, octaves, detail, evolve, 0.0);
        sum += smoothstep(-RELIEF_SOFT, RELIEF_SOFT, v - relief_floor(h + rise * f, thresh, span));
    }
    let slant = clamp(1.0 / max(sun.y, 0.15), 1.0, 4.0);
    return (sum / f32(steps)) * OPTICAL_DEPTH_LOW * rise * slant;
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
fn multi_scatter(sun_od: f32, cos_theta: f32, octaves: i32) -> f32 {
    var att = 1.0;
    var contrib = 1.0;
    var ecc = 1.0;
    var total = 0.0;
    var norm = 0.0;
    for (var i = 0; i < octaves; i = i + 1) {
        total += contrib * dual_hg(cos_theta, ecc) * exp(-sun_od * att);
        norm += contrib;
        att *= MS_ATTENUATION;
        contrib *= MS_CONTRIBUTION;
        ecc *= MS_ECCENTRICITY;
    }
    return total / norm;
}

// ---- light that has scattered many times ----------------------------------
//
// The octaves above are the first few bounces, and every one of them decays as
// exp(−depth): a thick core gets almost nothing from them, and its base fell to
// the ambient floor alone — dark, with a hard step from lit edge to grey core
// that reads as metal rather than as vapour. A real cloud does not darken that
// way. Light that has bounced dozens of times diffuses through it, and diffusion
// falls off as 1/(1 + (1−g)τ), not exponentially: an overcast deck of optical
// depth 20 still lets through a third of the sun, from every direction at once.
// (Frozen Fractal's *A pretty sky*, 2026, found the same thing by Monte Carlo:
// with real multiple scattering, cloud bottoms come out brighter and smoother.)
//
// The two-stream (Eddington) transmittance of a conservatively scattering slab
// is that curve in closed form. What is added here is its *diffuse* part only:
// everything except what still travels near the incoming direction, which the
// phase-function octaves already draw with their silver lining.

// Mean cosine of a water droplet's scatter.
const G_DIFFUSE: f32 = 0.85;
// Optical depth, for diffusion, of a fully covered column of the low deck. A
// cumulus is 30–100 deep; `OPTICAL_DEPTH_LOW` is far less because it is tuned as
// the *view* ray's opacity and the sun march's shadow, not as a column.
const DIFFUSE_DEPTH_LOW: f32 = 30.0;

// The clear sky above the deck, as light falling on its top: Rayleigh blue,
// scaled to luminance 1 (`light.z` is its strength). It reaches the base the same
// way the sun's diffuse part does, which is what tints a cloud's underside blue
// by day — and, under a red sun, pink: a red beam and a blue sky diffusing out
// of the same base. Frozen Fractal got it by bending the base's normals up into
// the sky; a plane has no normals, so it goes through the slab instead.
const SKY_ABOVE_TINT: vec3<f32> = vec3<f32>(0.59, 1.02, 1.97);
// The mean cosine of skylight's incidence on the deck's top: a whole hemisphere.
const SKY_ABOVE_MU: f32 = 0.5;

// The diffuse part of what crosses a slab of optical depth `tau`, lit at the
// cosine `mu0`: 1 − R of the Eddington slab, less what is still in the forward
// peak (the depth there is scaled by 1 − g, as delta-Eddington scales it).
fn diffuse_through(tau: f32, mu0: f32) -> f32 {
    let mu = max(mu0, 0.05);
    let total = ((2.0 / 3.0 + mu) + (2.0 / 3.0 - mu) * exp(-tau / mu))
        / (4.0 / 3.0 + (1.0 - G_DIFFUSE) * tau);
    return max(total - exp(-(1.0 - G_DIFFUSE) * tau / mu), 0.0);
}

// The low deck's light at a point, for the shell and the mirror alike.
//
// `shape` is the column's coverage (0..1), `sun_od` the march's optical depth
// toward the sun, `sun_rgb` the sunlight at the deck, `sky_above` the skylight on
// its top, and `ambient` the fill from the horizon and the ground below, of which
// an opaque core keeps `sky_floor`.
//
// `column` is how thick the cloud is there (0..1 of the deck) and `above` how
// much of it lies over the point seen: the same number for a flat deck, but in
// relief a point near a cloud's top has little above it however thick the
// column, so its top is lit and its base is not.
fn low_deck_light(
    column: f32,
    above: f32,
    sun_od: f32,
    cos_theta: f32,
    sun: vec3<f32>,
    sun_rgb: vec3<f32>,
    sky_above: vec3<f32>,
    ambient: vec3<f32>,
    sky_floor: f32,
    ms_octaves: i32,
) -> vec3<f32> {
    let ms = multi_scatter(sun_od, cos_theta, ms_octaves);
    // Beer-Powder (Schneider, Nubis / Horizon Zero Dawn). The powder factor
    // darkens thin edges — the reason a cumulus reads as solid rather than as
    // fog — but only when the sun is behind the camera. Looking *into* the sun
    // those same edges are the brightest thing in the sky, and applying powder
    // there would cancel exactly the silver lining the phase function just built.
    let powder = 1.0 - exp(-2.0 * column * OPTICAL_DEPTH_LOW);
    let powder_w = clamp(0.5 - 0.5 * cos_theta, 0.0, 1.0);
    let tau = above * DIFFUSE_DEPTH_LOW;
    // The diffused part is weighted by `sun.y`: a low sun spreads the same beam
    // over more of the deck's top, so less of it comes out of each patch of
    // base. Without it a closed deck under a 16 h sun glowed orange.
    let from_above = ms * mix(1.0, powder, powder_w) + diffuse_through(tau, sun.y) * max(sun.y, 0.0);
    // Just after sunset the deck still has the sun — its horizon is lower than
    // the street's (`utils::sun::sun_disc_at`) — but now *under* it, so a base is
    // the lit face, and what a viewer below sees is what the column reflects
    // (1 − T of the same slab), not what crosses it: the after-sunset glow.
    let mu_below = max(-sun.y, 0.05);
    let tau_column = column * DIFFUSE_DEPTH_LOW;
    let reflected = 1.0 - ((2.0 / 3.0 + mu_below) + (2.0 / 3.0 - mu_below) * exp(-tau_column / mu_below))
        / (4.0 / 3.0 + (1.0 - G_DIFFUSE) * tau_column);
    let sun_term = mix(from_above, reflected, 1.0 - smoothstep(-0.01, 0.0, sun.y));
    // Cloud bases go dark, but never black — the ground bounces light back up at
    // them, and an overcast deck is the sky's own light source rather than
    // something standing in front of it. How dark is `sky_floor`.
    return sun_rgb * sun_term
        + sky_above * diffuse_through(tau, SKY_ABOVE_MU)
        + ambient * (sky_floor + (1.0 - sky_floor) * (1.0 - above));
}

// The sunlight reaching a deck `above_camera_m` over the camera.
//
// With bevy's atmosphere running, this is *its* transmittance — the lookup table
// the sky itself is drawn from — so a sunset deck takes exactly the orange the sky
// around it has, and the fog/sun model's own guess (`fallback`, `utils::sun`) is
// only the stand-in for a view without that atmosphere (the dome sky). `scale` is
// what the fallback carries besides the colour: the sun disc's own occlusion at
// sunrise and sunset, and the deck's brightness gain.
fn deck_sunlight(sun: vec3<f32>, above_camera_m: f32, fallback: vec3<f32>, scale: f32) -> vec3<f32> {
    @if(ATMOSPHERE) {
        return scale * atmosphere_transmittance(sun, above_camera_m);
    } @else {
        return fallback;
    }
}

@if(ATMOSPHERE)
// bevy's transmittance lookup (`bruneton_functions::transmittance_lut_r_mu_to_uv`),
// for a point `above_camera_m` over the camera looking toward `sun`. The radii are
// read through the binding one field at a time and never as a whole struct: the
// Adreno 840 reads a by-value copy of `Atmosphere` wrong
// (`docs/notes/adreno-atmosphere-struct-copy.md`).
fn atmosphere_transmittance(sun: vec3<f32>, above_camera_m: f32) -> vec3<f32> {
    let inner = view_bindings::atmosphere.inner_radius;
    let outer = view_bindings::atmosphere.outer_radius;
    let cam = (view_bindings::atmosphere.world_to_atmosphere
        * vec4<f32>(view_bindings::view.world_position, 1.0)).xyz;
    // Never below the surface, as bevy's own `clamp_to_surface`.
    let r = max(length(cam), inner + 1.0) + above_camera_m;
    let mu = dot(sun, normalize(cam));
    let h = sqrt(outer * outer - inner * inner);
    let rho = sqrt(max(r * r - inner * inner, 0.0));
    let disc = max(r * r * (mu * mu - 1.0) + outer * outer, 0.0);
    let d = max(-r * mu + sqrt(disc), 0.0);
    let d_min = outer - r;
    let d_max = rho + h;
    let uv = vec2<f32>((d - d_min) / (d_max - d_min), rho / h);
    return textureSampleLevel(
        view_bindings::atmosphere_transmittance_texture,
        view_bindings::atmosphere_transmittance_sampler,
        uv,
        0.0,
    ).rgb;
}

// ---- the deck in a mirror ---------------------------------------------------

// How far the mirror's blur may widen the coverage threshold, in fbm units, once
// the patch it gathers spans a whole base octave. Half the field's useful range:
// enough that a rough lake's clouds are soft blotches rather than cut-outs.
const MIRROR_BLUR: f32 = 0.5;

// **The mirror is a cheaper deck than the sky, on purpose, and measured.** Run
// at the shell's full precision — six octaves, the domain warp, a sun march — it
// cost the Steam Deck 2.4–3.5 ms a frame over the Paterswoldsemeer at 1600×900
// (2026-09-29), for detail the fresnel dims and the waves blur. So it keeps the
// four coarsest octaves of the shell's count (muted, not dropped, so the fbm's
// normalisation — and the shapes — stay the shell's), no warp (which moves a
// cloud by a couple of hundred metres at most: a fraction of one), a one-tap sun
// march, and two scattering octaves.
const MIRROR_MAX_OCTAVES: i32 = 4;
const MIRROR_MS_OCTAVES: i32 = 2;

// Where a ray from `offset` (relative to the camera) along `dir` meets the deck's
// plane, `height` above the camera: horizontal metres from the camera. A ray
// flatter than the grazing cut is held at it, so the answer stays finite — every
// pixel's, including the ones `mirrored` then turns away, feeds the derivative a
// caller takes of it, and one infinity poisons its whole 2×2 quad.
fn mirror_hit(height: f32, offset: vec3<f32>, dir: vec3<f32>) -> vec2<f32> {
    let t = (height - offset.y) / max(dir.y, GRAZE_MIN);
    return offset.xz + dir.xz * t;
}

// The low deck as the shell would draw it along a reflected ray: rgb in the
// shell's own units (post-exposure, like everything the sky is drawn in), a its
// opacity. Zero where there is no cloud, or no deck.
//
// `offset` is the reflecting point relative to the camera in render space — the
// deck is a plane `shape.w` above the *camera*, so only the offset matters.
// `dir` is the reflected ray, `range_m` the camera's distance to the point, and
// `perceptual_roughness` the surface's.
//
// `spread_m` is how far the ray's landing point on the deck moves from one pixel
// to the next (`length(fwidth(mirror_hit(…)))`, taken by the caller): the
// pixel's footprint up there, which grows toward the horizon and wherever a swell
// bends neighbouring rays apart. The patch is at least that wide, the way a
// texture's mip is chosen. It arrives as an argument because a derivative cannot
// be taken past this function's early outs. What a caller must not hand in is a
// ray bent by every ripple — see where `water.wgsl` reflects the clouds.
//
// Only the low deck: the cirrus above it is thin enough that its reflection, at
// water's few per cent, is below anything a wave does to the picture.
fn mirrored(
    deck: Deck,
    offset: vec3<f32>,
    dir: vec3<f32>,
    range_m: f32,
    perceptual_roughness: f32,
    spread_m: f32,
) -> vec4<f32> {
    let cover = deck.shape.x;
    let fade = deck.sun_color.a * deck.motion.w * (1.0 - deck.motion.z)
        * smoothstep(GRAZE_MIN, GRAZE_MAX, dir.y);
    if cover <= 0.0 || fade <= 0.0 {
        return vec4<f32>(0.0);
    }
    let cheap = deck.light.y > 0.5;
    let soft = max(deck.shape.y, 1e-3);
    let thresh = low_threshold(cover, cheap);
    let clock = deck.shape.z;
    let evolve = EVOLVE_DETAIL * clock;

    // Up from the water to the deck's plane, and across to where the ray meets it.
    let t = (deck.shape.w - offset.y) / dir.y;
    let hit = mirror_hit(deck.shape.w, offset, dir);

    // **A rough mirror reflects a cone, not a ray**, and over a path kilometres
    // long a cone gathers whole clouds. GGX's α is roughly the lobe's half-width
    // in radians, so the patch is `2·t·α` across — or the pixel's own footprint
    // on the deck, where that is wider (`spread_m`, above). Octaves finer than the
    // patch are averaged away — muted, which is what `detail` does, counting how
    // many octaves still resolve — and once the patch is a base octave wide the
    // edges go too: the threshold ramp widens about its middle, so a blurred deck
    // fades to the cover it averages to instead of losing it.
    let alpha_ggx = perceptual_roughness * perceptual_roughness;
    let patch_m = max(2.0 * t * alpha_ggx, spread_m);
    let octaves = low_octaves(cheap);
    let resolved = log2(max(1.0 / max(patch_m * FREQ_LOW, 1e-6), 1.0)) / log2(LACUNARITY);
    // `detail · n` octaves past the first survive, so this keeps the coarsest
    // `MIRROR_MAX_OCTAVES` at most (see there).
    let kept = f32(MIRROR_MAX_OCTAVES - 1) / f32(octaves);
    let detail = min(min(detail_at(range_m + t), resolved / f32(octaves)), kept);
    let widen = clamp(patch_m * FREQ_LOW, 0.0, 1.0) * MIRROR_BLUR;

    let uv = low_uv(hit, deck.place.xy, deck.motion.xy);
    let v = relief_field(uv, octaves, detail, evolve, patch_m * FREQ_LOW);
    // Muting octaves cannot blur the base octave itself, which is still sampled at
    // one point — and a patch several base octaves wide lands that point on a
    // different cloud from each pixel to the next. Past one base octave the shape
    // settles onto the cover it would average to, as a wide enough lens would.
    //
    // What it settles onto is the patch's mean *opacity*, not its mean shape. The
    // two differ by the exponential between them: a patch 70 % covered is 70 %
    // opaque cloud and 30 % gap, while a shape of 0.7 is an optical depth of ~5,
    // i.e. 99 % opaque — which is how a blurred lake used to show a solid lid with
    // every gap in the deck closed over (Deck, 2026-09-29).
    let settle = smoothstep(1.0, 3.0, patch_m * FREQ_LOW);
    // Centred on the threshold with the relief's edge, as the shell draws it.
    let sharp = smoothstep(thresh - RELIEF_SOFT - 0.5 * widen, thresh + RELIEF_SOFT + 0.5 * widen, v);
    let shape = mix(sharp, cover, settle);
    let opacity = mix(1.0 - exp(-sharp * OPTICAL_DEPTH_LOW), cover, settle);
    let alpha = opacity * fade;
    if alpha <= 0.002 {
        return vec4<f32>(0.0);
    }

    // Lit as the shell lights its low deck, with a one-tap march. The tap is not
    // optional: taking the column's own depth instead charged every cloud its
    // full thickness toward the sun, so no cloud had a lit side, and the
    // reflected deck came out as dark as the sky around it — the reflection
    // measured cheaper and was simply gone (Deck, 2026-09-29).
    let sun = normalize(deck.sun_dir.xyz);
    let cos_theta = dot(dir, sun);
    let sun_rgb = deck_sunlight(sun, deck.shape.w, deck.sun_color.rgb, deck.light.x);
    let sun_od = sun_optical_depth(uv, sun, deck.sun_dir.w, thresh, soft, detail, evolve, 1);
    var rgb = low_deck_light(
        shape,
        shape,
        sun_od,
        cos_theta,
        sun,
        sun_rgb,
        SKY_ABOVE_TINT * deck.light.z,
        deck.sky_color.rgb,
        deck.haze_color.a,
        MIRROR_MS_OCTAVES,
    );

    // The haze between the water and the deck. The leg from the camera to the
    // water is the surface's own, and bevy fogs it after this returns.
    let haze_path = min(t, HAZE_LAYER_M / dir.y);
    let x = haze_path * deck.place.z;
    rgb = mix(deck.haze_color.rgb, rgb, exp(-x * x));
    return vec4<f32>(rgb, clamp(alpha, 0.0, 1.0));
}
