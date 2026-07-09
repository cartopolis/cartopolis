// Fog-colored horizon band. Bridges the fogged ground and the atmosphere sky:
// the Nishita sky is single-scattering, so horizon-grazing view rays accumulate
// extinction without the multiple-scatter glow that keeps real horizons bright,
// leaving a dark band right where the distance fog needs to meet the sky. This
// shell paints that band with the fog color — including the same directional
// sun-glow term Bevy's DistanceFog applies to fogged geometry (see apply_fog in
// bevy_pbr's pbr_functions.wgsl), so at full density the shell is
// indistinguishable from fog at infinite distance and no line forms at the
// horizon. Sampled by per-pixel view direction (like starsky.wgsl), so the mesh
// shape is irrelevant — it's just a cheap shell around the camera.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::{view, lights}

@group(2) @binding(0) var<uniform> color: vec4<f32>;
// params.x: falloff scale — elevation (view-dir y) over which the haze decays by 1/e.
// params.y: peak alpha at/below the horizon.
// params.z: directional-light exponent (DistanceFog::directional_light_exponent).
// params.w: zenith floor (0–1) — fraction of peak alpha covering the whole sky,
//           so dense fog veils the entire dome like a real foggy day.
@group(2) @binding(1) var<uniform> params: vec4<f32>;
// DistanceFog::directional_light_color — rgb tint, alpha = strength.
@group(2) @binding(2) var<uniform> dir_light_color: vec4<f32>;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let dir = normalize(in.world_position.xyz - view.world_position.xyz);

    // Same directional in-scattering as Bevy's DistanceFog.
    var scattering = vec3<f32>(0.0);
    if dir_light_color.a > 0.0 {
        let n = lights.n_directional_lights;
        for (var i: u32 = 0u; i < n; i = i + 1u) {
            let light = lights.directional_lights[i];
            scattering += pow(max(dot(dir, light.direction_to_light.xyz), 0.0), params.z)
                * light.color.rgb * view.exposure;
        }
    }
    let rgb = color.rgb + scattering * dir_light_color.rgb * dir_light_color.a;

    // Full haze at and below the horizon, decaying exponentially with elevation
    // toward the zenith floor. The asymptotic tail (unlike a smoothstep's hard
    // end) leaves no visible edge where the haze hands over to the sky, and the
    // floor veils the whole dome when the fog is dense.
    let fade = params.w + (1.0 - params.w) * exp(-max(dir.y, 0.0) / params.x);
    return vec4<f32>(rgb, color.a * params.y * fade);
}
