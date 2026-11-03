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
    let flat = (pow(albedo, vec3<f32>(CURVE)) * light * GAIN / 3.14159265 + emissive) * view.exposure;
    return vec4<f32>(mix(lit.rgb, flat, k), lit.a);
}
