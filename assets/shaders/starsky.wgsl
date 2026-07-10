// Equirectangular star-map skybox. Samples the texture by the per-pixel view direction
// (not by mesh UVs), so there are no UV-sphere poles or longitude seam — the mapping is
// uniform in every direction. Drawn additively; `brightness` fades it with the night.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

@group(2) @binding(0) var<uniform> brightness: f32;
@group(2) @binding(1) var star_texture: texture_2d<f32>;
@group(2) @binding(2) var star_sampler: sampler;

const PI: f32 = 3.141592653589793;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Direction from the camera to this fragment on the surrounding sphere.
    let dir = normalize(in.world_position.xyz - view.world_position.xyz);
    // Equirectangular projection: longitude → U, latitude → V.
    let u = atan2(dir.z, dir.x) * (0.5 / PI) + 0.5;
    let v = acos(clamp(dir.y, -1.0, 1.0)) / PI;
    let stars = textureSample(star_texture, star_sampler, vec2<f32>(u, v)).rgb;
    // The source is a long-exposure photo: its lifted sky glow and saturated-blue
    // Milky Way haze read as wallpaper, not a night sky. Desaturate slightly and
    // apply a power curve — the haze drops toward black while bright star cores
    // keep their punch, which is how the naked eye actually sees it.
    let lum = dot(stars, vec3<f32>(0.2126, 0.7152, 0.0722));
    let graded = pow(mix(vec3<f32>(lum), stars, 0.75), vec3<f32>(1.8));
    return vec4<f32>(graded * brightness, 1.0);
}
