// The sunlight the street throws back up: the light that fills a wall in shade.
//
// Bevy lights a surface with the sun and the sky (the atmosphere's environment
// map) and nothing that bounced off anything else. Out of the sun that leaves the
// sky alone, which is blue and comes from above, so a facade in shade went to a
// cold dark blue, and a sunlit one had only the sun: the city read as flat,
// coloured blocks under a blue cast (mac-2, 2026-11-09, the Grote Markt at
// 13:30 in June: ground in shade 17 times darker than ground in sun and twice
// as blue as red, where a street in sun is about six times its shade and the
// shade a neutral grey). In a real street most of a wall's fill is the sunlit
// pavement in front of it, warm, and from below.
//
// So: the ground taken as an endless plane lit by the sun, of albedo `GROUND`,
// seen by a surface of normal `n` through the lower half of its view, (1 − n.y)/2
// of it. Lambertian both ways, in the same units `apply_pbr_lighting` returns
// (multiplied by the view's exposure), so it is added to its result.

import bevy_pbr::render::mesh_view_bindings::{lights, view};

// The street's albedo times the share of it in sun: brick paving and asphalt
// are 0.1–0.25, and in a street a good part is in the buildings' shadow.
const GROUND: f32 = 0.13;
// The sky's light on the ground beside the sun's, as a share of it: a clear sky
// gives a sixth to a fifth of the sun's on a horizontal plane at a high sun.
const SKY_SHARE: f32 = 0.15;

// What the air leaves of the sun: a clear-sky optical depth over the air mass.
// The patched lighting applies the atmosphere's own table to the direct light;
// this needs only the right size, and it is what takes the bounce away at dusk.
fn transmittance(sun_y: f32) -> f32 {
    return exp(-0.17 / max(sun_y, 0.035));
}

// Radiance to add to `apply_pbr_lighting`'s result, for a diffuse colour
// `albedo` (already without metal and glass) at normal `n`.
fn radiance(albedo: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let below = clamp(0.5 * (1.0 - n.y), 0.0, 1.0);
    if below <= 0.0 {
        return vec3<f32>(0.0);
    }
    var on_ground = vec3<f32>(0.0);
    for (var i: u32 = 0u; i < lights.n_directional_lights; i = i + 1u) {
        let l = lights.directional_lights[i];
        let y = l.direction_to_light.y;
        if y > 0.0 {
            on_ground += l.color.rgb * y * transmittance(y);
        }
    }
    let irradiance = on_ground * (1.0 + SKY_SHARE) * GROUND * below;
    return albedo * irradiance / 3.14159265 * view.exposure;
}
