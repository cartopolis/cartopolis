// The strategy map's light (`docs/mockups/macro-view/`, v1 and v3): seen from
// far, the painted map shows its own colours, whatever the season or the hour.
//
// Lit as ground, the map took the real sun's angle: on 3 November at noon the sun
// stands ~20° over the Netherlands, and the map came out about a fifth darker and
// greyer than the sheet (mac-1, 350 km; farmland 111,113,88 against the sheet's
// 143,141,95). So from `FROM` metres of camera distance on, the ground is lit by
// the same lights at the same strength and colour but as if each stood overhead
// wherever it is above the horizon. The night side stays night, a red sunset stays
// red, and only the angle is taken away.

import bevy_pbr::render::mesh_view_bindings::{lights, view};

// Camera distances, metres, over which the map light takes over: the band the
// design starts the strategy map in (v2: the painted ground comes in from 15 km
// and the city is gone by 50 km).
const FROM: vec2<f32> = vec2<f32>(20000.0, 60000.0);

// The camera's tonemapping (TonyMcMapface) lifts dark tones and pales them: lit
// flat, the map came out pastel, the sea 74,119,151 for the sheet's 40,74,104.
// So the colour goes in through a curve that deepens it (`CURVE`), at a strength
// (`GAIN`) that keeps farmland at the sheet's 143,141,95. Fitted by eye and by
// the mean colour of three patches on mac-1 at 350 km
// (`docs/notes/painted-far-map.md`); the sea stays bluer than the sheet's.
const CURVE: f32 = 1.5;
const GAIN: f32 = 1.5;

// From orbit the painted sheet read as a poster (asked 2026-11-11, "te vel",
// wanted more like Google Earth): a cream Sahara, yellow steppe, bright green
// woods and a pale royal-blue sea. Satellite true colour (Blue Marble, Google
// Earth's globe) is darker and greyer: the forests nearly black-green, the
// deserts a warm tan, the open sea a deep navy. So over `ORBIT` metres of
// camera distance the albedo is greyed by `ORBIT_SAT` and darkened by
// `ORBIT_DIM` before it goes through the curve. On distance, as the map light
// itself is, so the two tiers grade alike where they meet.
const ORBIT: vec2<f32> = vec2<f32>(300000.0, 2000000.0);
const ORBIT_SAT: f32 = 0.85;
const ORBIT_DIM: f32 = 0.75;
// The air between orbit and the ground adds blue (`globe::SPACE_ATMOSPHERE_DENSITY`)
// that a grey cannot take back: the Congo came out teal. So the graded albedo
// is warmed against it, by taking blue out — except snow and ice, which the
// warming turned cream: it fades out over the brightest albedos.
const ORBIT_WARM: vec3<f32> = vec3<f32>(1.0, 0.97, 0.8);

// `albedo` as orbit sees it, `distance` metres from the camera.
fn orbit_grade(albedo: vec3<f32>, distance: f32) -> vec3<f32> {
    let o = smoothstep(ORBIT.x, ORBIT.y, distance);
    let y = dot(albedo, vec3<f32>(0.2126, 0.7152, 0.0722));
    let warm = mix(ORBIT_WARM, vec3<f32>(1.0), smoothstep(0.45, 0.75, y));
    let graded = mix(vec3<f32>(y), albedo, ORBIT_SAT) * ORBIT_DIM * warm;
    return mix(albedo, graded, o);
}

// `lit` (as `apply_pbr_lighting` returned it) with the map light mixed in, for a
// ground of colour `albedo` and normal `n`, `distance` metres from the camera.
// `emissive` is the material's own light (the globe's cities at night), kept.
fn apply(
    lit: vec4<f32>,
    albedo: vec3<f32>,
    emissive: vec3<f32>,
    n: vec3<f32>,
    distance: f32,
) -> vec4<f32> {
    let k = smoothstep(FROM.x, FROM.y, distance);
    if k <= 0.0 {
        return lit;
    }
    var light = vec3<f32>(0.0);
    for (var i: u32 = 0u; i < lights.n_directional_lights; i = i + 1u) {
        let l = lights.directional_lights[i];
        // Above the horizon, with the twilight band the globe's night side uses.
        let up = smoothstep(-0.05, 0.15, dot(n, l.direction_to_light.xyz));
        light += l.color.rgb * up;
    }
    let flat = (pow(orbit_grade(albedo, distance), vec3<f32>(CURVE)) * light * GAIN / 3.14159265 + emissive) * view.exposure;
    return vec4<f32>(mix(lit.rgb, flat, k), lit.a);
}

// The sun on the sea, which the map light above takes away with the angle: the
// glint is nothing but angle. Painted, as the land is, rather than a specular
// highlight: a GGX one, added to the flat sea, was a white hotspot with a
// smooth falloff and read as plastic or metal (asked 2026-11-03). In MODIS's
// true colour (Terra, 2026-07-15, the western Mediterranean) the glint is a
// broad warm-grey sheen about as bright as the land beside it, with no white in
// it, broken into streaks where the wind is calmer or rougher. So the sea is
// painted towards `SHEEN` through the same map light as the land, over the
// shape of a GGX lobe, and the lobe's width wanders over the sea.

// The sheen's colour, as a painted-map albedo: that crop's mean is sRGB
// 136,130,113, and the map light turns an albedo into its own sheet colour.
const SHEEN: vec3<f32> = vec3<f32>(0.243, 0.223, 0.171);
// How far the sea goes towards it at the lobe's peak.
const SHEEN_PEAK: f32 = 0.85;
// The lobe's GGX width: Cox & Munk's `0.003 + 0.00512·U` at a 12 m/s wind is a
// mean-square slope of 0.064, α ≈ 0.25. A sea state of its own, not either
// surface's live one, so the globe and the flat tier paint the same sheen where
// it crosses from one to the other (`docs/notes/globe-ocean-is-a-brdf-split.md`).
const SHEEN_ALPHA: f32 = 0.25;
// The streaks: the width wanders ±`STREAK_SPREAD` of itself over patches
// `STREAK_SCALE` Mercator cells across (the world is 1): 1/600 of the equator
// is ~67 km, stretched three to one along a fixed wind.
const STREAK_SPREAD: f32 = 0.35;
const STREAK_SCALE: f32 = 600.0;

fn hash2(p: vec2<f32>) -> f32 {
    let q = fract(p * vec2<f32>(0.1031, 0.1030));
    let r = q + dot(q, q.yx + 33.33);
    return fract((r.x + r.y) * r.x);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(hash2(i), hash2(i + vec2<f32>(1.0, 0.0)), u.x),
        mix(hash2(i + vec2<f32>(0.0, 1.0)), hash2(i + vec2<f32>(1.0, 1.0)), u.x),
        u.y,
    );
}

// The streak field, -1..1, at Mercator `merc`; `px` is one pixel there, in
// Mercator units, so an octave finer than ~3 pixels fades out instead of
// crawling as the globe turns.
fn streaks(merc: vec2<f32>, px: f32) -> f32 {
    // Stretched along a wind from the WSW: a rotation, then 4:1.
    let r = vec2<f32>(0.94 * merc.x + 0.34 * merc.y, -0.34 * merc.x + 0.94 * merc.y);
    let p = r * STREAK_SCALE * vec2<f32>(1.0, 3.0);
    // At most ~6,000 across the world, where an f32 still holds 1/2000 of a cell.
    let a = value_noise(p);
    let b = value_noise(p * 2.7 + 17.0);
    let fine = 1.0 - smoothstep(0.15, 0.4, px * STREAK_SCALE * 3.0 * 2.7);
    return (a - 0.5) * 1.4 + (b - 0.5) * 0.4 * fine;
}

// `lit` (as `apply` returned it) with the sea painted towards the sheen, for
// `water` (0..1) of the texel, at the strength the map light has taken over: so
// below `FROM` the material's own specular is the glint and past it this is,
// with nothing doubled between. `merc` is the texel's place in Web Mercator's
// unit square (`hex_grid`'s tile key and UV), (-1, -1) where the surface has
// none, which draws the sheen without streaks; `px` is `length(fwidth(merc))`,
// taken by the caller outside any branch, since it is a derivative.
fn sea_sheen(
    lit: vec4<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    water: f32,
    distance: f32,
    merc: vec2<f32>,
    px: f32,
) -> vec4<f32> {
    let wander = select(streaks(merc, px), 0.0, merc.x < 0.0);
    let k = smoothstep(FROM.x, FROM.y, distance) * water;
    if k <= 0.0 {
        return lit;
    }
    let alpha = SHEEN_ALPHA * (1.0 + STREAK_SPREAD * wander);
    let a2 = alpha * alpha;
    var paint = vec3<f32>(0.0);
    var cover = 0.0;
    for (var i: u32 = 0u; i < lights.n_directional_lights; i = i + 1u) {
        let li = lights.directional_lights[i];
        let l = li.direction_to_light.xyz;
        // The same twilight band `apply` puts the light behind.
        let up = smoothstep(-0.05, 0.15, dot(n, l));
        if up <= 0.0 {
            continue;
        }
        let h = normalize(l + v);
        let noh = max(dot(n, h), 0.0);
        // GGX's shape, 1 at its peak: a broad core and a long soft skirt.
        let d = (noh * noh) * (a2 - 1.0) + 1.0;
        let lobe = (a2 * a2) / (d * d) * up;
        paint += li.color.rgb * lobe;
        cover = max(cover, lobe);
    }
    if cover <= 0.0 {
        return lit;
    }
    // The sheen lit as the map lights the land, in the lights' own colours, so
    // a low red sun lays a red sheen.
    let sheen = pow(SHEEN, vec3<f32>(CURVE)) * (paint / cover) * GAIN / 3.14159265 * view.exposure;
    return vec4<f32>(mix(lit.rgb, sheen, cover * SHEEN_PEAK * k), lit.a);
}
