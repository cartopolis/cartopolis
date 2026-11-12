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

import bevy_pbr::render::mesh_view_bindings::{globals, lights, view};

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

// The sheet through that curve is a poster (asked 2026-11-12, "te fel"):
// wanted semi-realistic, keeping what makes it a strategy map — towns, roads,
// land use told apart. So the albedo is greyed by `GRADE_SAT` and darkened, from
// the map light's own band on: by `NEAR_DIM` there, deepening to `ORBIT_DIM`
// over `ORBIT` metres of camera distance, where satellite true colour (Blue
// Marble, Google Earth's globe) has the forests nearly black-green and the
// deserts a warm tan. On distance, so the two tiers grade alike where they meet.
const GRADE_SAT: f32 = 0.8;
const NEAR_DIM: f32 = 0.9;
const ORBIT: vec2<f32> = vec2<f32>(300000.0, 2000000.0);
const ORBIT_DIM: f32 = 0.75;
// The air between orbit and the ground adds blue (`globe::SPACE_ATMOSPHERE_DENSITY`)
// that a grey cannot take back: the Congo came out teal. So from orbit the
// albedo is warmed against it, by taking blue out — except snow and ice, which
// the warming turned cream: it fades out over the brightest albedos.
const ORBIT_WARM: vec3<f32> = vec3<f32>(1.0, 0.97, 0.8);

// `albedo` graded for a camera `distance` metres away.
fn grade(albedo: vec3<f32>, distance: f32) -> vec3<f32> {
    let k = smoothstep(FROM.x, FROM.y, distance);
    let o = smoothstep(ORBIT.x, ORBIT.y, distance);
    let y = dot(albedo, vec3<f32>(0.2126, 0.7152, 0.0722));
    let warm = mix(vec3<f32>(1.0), mix(ORBIT_WARM, vec3<f32>(1.0), smoothstep(0.45, 0.75, y)), o);
    let grey = mix(albedo, mix(vec3<f32>(y), albedo, GRADE_SAT), k);
    return grey * mix(1.0, mix(NEAR_DIM, ORBIT_DIM, o), k) * warm;
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
    let flat = (pow(grade(albedo, distance), vec3<f32>(CURVE)) * light * GAIN / 3.14159265 + emissive) * view.exposure;
    return vec4<f32>(mix(lit.rgb, flat, k), lit.a);
}

// The sun on the sea, which the map light above takes away with the angle: the
// glint is nothing but angle. Painted, as the land is, rather than a specular
// highlight: a GGX one, added to the flat sea, was a white hotspot with a
// smooth falloff and read as plastic or metal (asked 2026-11-03). In MODIS's
// true colour (Terra, 2026-07-15, the western Mediterranean) the glint is a
// broad warm-grey sheen about as bright as the land beside it, with no white in
// it, mottled where the wind is calmer or rougher. So the sea is
// painted towards `SHEEN` through the same map light as the land, over the
// shape of a GGX lobe, and the lobe's width wanders over the sea.

// The sheen's colour, as a painted-map albedo. It was that crop's warm grey
// (sRGB 136,130,113), painted over the sea at 0.85: on the globe it replaced
// the blue with a beige patch and read as another material, sand or metal
// (asked 2026-11-12, "alsof het een ander materiaal is"). The crop's warmth is
// the haze over the Mediterranean, not the water. So the sheen is a cool
// silver the sea goes part of the way towards, keeping its own blue in it.
const SHEEN: vec3<f32> = vec3<f32>(0.26, 0.30, 0.35);
// How far the sea goes towards it at the lobe's peak.
const SHEEN_PEAK: f32 = 0.55;
const SHEEN_ALPHA: f32 = 0.25;
// The mottle: the lobe's width wanders ±`MOTTLE_SPREAD` of itself and its
// strength ±`MOTTLE_GLOW` over a round, three-octave field whose coarsest
// patches are `MOTTLE_SCALE` Mercator cells across (the world is 1): 1/120 of
// the equator is ~330 km. It was a single octave stretched three to one along a
// wind, and from orbit that read as big stripes laid over the sea, not as a sea
// (asked 2026-11-12); a real glint is a soft oval, mottled where the wind is
// calmer or rougher.
const MOTTLE_SPREAD: f32 = 0.06;
const MOTTLE_GLOW: f32 = 0.1;
const MOTTLE_SCALE: f32 = 120.0;

// The rest of what keeps the sea from reading as a glass bauble (asked
// 2026-11-12, "dynamischer"): its colour is by depth in the tile
// (`cartopolis_geo::ocean_depth`), and here three things on top of it.
//
// The mottle drifts downwind, `DRIFT` Mercator units a second: its finest
// patches (~50 km) in ~90 s, far faster than real weather and slow enough to
// read as the sea moving rather than the map. `globals.time` wraps each hour,
// which jumps the field once.
const DRIFT: f32 = 1.0 / (MOTTLE_SCALE * 6.7 * 90.0);
// Wind-rows over the open sea: its tone wanders ±`ROWS_TONE` over patches
// `ROWS_SCALE` cells across (~400 km), as roughness and plankton do in any
// true-colour mosaic, so no two stretches of open ocean are the same blue.
const ROWS_SCALE: f32 = 100.0;
const ROWS_TONE: f32 = 0.09;
// The sky in the water: towards the limb a sea reflects more of the sky than it
// lets out of its depths (Fresnel), so the globe's oceans lighten to a hazy
// blue at the edge rather than staying one dark ball. An albedo through the map
// light, as the sheen is; `SKY_PEAK` of the way at grazing.
const SKY: vec3<f32> = vec3<f32>(0.20, 0.30, 0.42);
const SKY_PEAK: f32 = 0.55;

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

// The mottle field, about -1..1, at Mercator `merc`; `px` is one pixel there,
// in Mercator units, so an octave finer than ~3 pixels fades out instead of
// crawling as the globe turns.
fn mottle(merc: vec2<f32>, px: f32) -> f32 {
    // At most ~800 cells across the world, where an f32 still holds 1/10000 of one.
    let p = merc * MOTTLE_SCALE;
    var sum = (value_noise(p) - 0.5) * 1.1;
    let f1 = 1.0 - smoothstep(0.15, 0.4, px * MOTTLE_SCALE * 2.6);
    sum += (value_noise(p * 2.6 + 17.0) - 0.5) * 0.6 * f1;
    let f2 = 1.0 - smoothstep(0.15, 0.4, px * MOTTLE_SCALE * 6.7);
    sum += (value_noise(p * 6.7 + 41.0) - 0.5) * 0.3 * f2;
    return sum * 1.6;
}

// `lit` (as `apply` returned it) with the sea painted towards the sheen, for
// `water` (0..1) of the texel, at the strength the map light has taken over: so
// below `FROM` the material's own specular is the glint and past it this is,
// with nothing doubled between. `merc` is the texel's place in Web Mercator's
// unit square (`hex_grid`'s tile key and UV), (-1, -1) where the surface has
// none, which draws the sheen unmottled; `px` is `length(fwidth(merc))`,
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
    let has_merc = merc.x >= 0.0;
    // Downwind, from the WSW.
    let drifted = merc - vec2<f32>(0.94, -0.34) * (globals.time * DRIFT);
    let wander = select(0.0, mottle(drifted, px), has_merc);
    let rows = select(0.0, value_noise(drifted * ROWS_SCALE) * 2.0 - 1.0, has_merc);
    let k = smoothstep(FROM.x, FROM.y, distance) * water;
    if k <= 0.0 {
        return lit;
    }
    var sea = lit.rgb * (1.0 + ROWS_TONE * rows * k);
    let alpha = SHEEN_ALPHA * (1.0 + MOTTLE_SPREAD * wander);
    let a2 = alpha * alpha;
    var paint = vec3<f32>(0.0);
    var sky = vec3<f32>(0.0);
    var cover = 0.0;
    for (var i: u32 = 0u; i < lights.n_directional_lights; i = i + 1u) {
        let li = lights.directional_lights[i];
        let l = li.direction_to_light.xyz;
        // The same twilight band `apply` puts the light behind.
        let up = smoothstep(-0.05, 0.15, dot(n, l));
        if up <= 0.0 {
            continue;
        }
        sky += li.color.rgb * up;
        let h = normalize(l + v);
        let noh = max(dot(n, h), 0.0);
        // GGX's shape, 1 at its peak: a broad core and a long soft skirt.
        let d = (noh * noh) * (a2 - 1.0) + 1.0;
        let lobe = (a2 * a2) / (d * d) * up;
        paint += li.color.rgb * lobe;
        cover = max(cover, lobe);
    }
    // The sky in the water, lit as the land is, so the night side stays dark.
    let grazing = pow(1.0 - clamp(dot(n, v), 0.0, 1.0), 4.0);
    let sky_lit = pow(SKY, vec3<f32>(CURVE)) * sky * GAIN / 3.14159265 * view.exposure;
    sea = mix(sea, sky_lit, grazing * SKY_PEAK * k);
    if cover <= 0.0 {
        return vec4<f32>(sea, lit.a);
    }
    // The sheen lit as the map lights the land, in the lights' own colours, so
    // a low red sun lays a red sheen.
    let sheen = pow(SHEEN, vec3<f32>(CURVE)) * (paint / cover) * GAIN / 3.14159265 * view.exposure;
    let glow = clamp(cover * SHEEN_PEAK * (1.0 + MOTTLE_GLOW * wander), 0.0, 1.0);
    return vec4<f32>(mix(sea, sheen, glow * k), lit.a);
}
