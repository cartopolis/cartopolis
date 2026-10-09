// Catalog star field: each star is a tiny sphere-tangent quad (built in
// setup_stars from the embedded HYG bright-star catalog) whose vertex color
// carries the star's temperature tint × magnitude luminance, with a twinkle
// seed in the alpha channel. Drawn additively: black adds nothing, so only the
// gaussian point cores register.
import bevy_pbr::render::forward_io::VertexOutput;
import bevy_pbr::render::mesh_view_bindings::view;

// x: brightness (night factor × fog veil), y: twinkle clock in seconds,
// z: 1 in the space tier (above the atmosphere), 0 on the ground (w unused).
@group(3) @binding(0) var<uniform> params: vec4<f32>;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Soft point profile: gaussian core over the quad's [-1,1]² footprint,
    // offset to reach exactly zero at the quad edge (no square halos).
    let d2 = dot(in.uv, in.uv);
    let core = max(exp(-4.0 * d2) - 0.0183, 0.0);

    let dir = normalize(in.world_position.xyz - view.world_position.xyz);
    // Everything below is the view from *inside* the atmosphere, in the anchor's
    // local frame where +Y is up: a horizon to cut the sky at, air to twinkle and
    // redden through. In space none of it applies — and the horizon cut in
    // particular hid the entire star field, because looking down at the globe
    // every star on screen sits at `dir.y < 0`.
    let space = params.z;
    // How close to the horizon this star sits (1 = at it, 0 = well above).
    let horizon = 1.0 - smoothstep(0.02, 0.35, dir.y);

    // Scintillation: two incommensurate sines, phase-shifted per star by the
    // seed. Twinkle grows toward the horizon — starlight there crosses ~38×
    // more air, which is also why low stars are dimmed by extinction below.
    let phase = in.color.a * 41.0;
    let tw = 0.5 * sin(params.y * 5.3 + phase) + 0.5 * sin(params.y * 13.7 + phase * 1.7);
    let flicker = mix(1.0 + (0.08 + 0.45 * horizon) * tw, 1.0, space);
    let extinction = mix(1.0 - 0.75 * horizon, 1.0, space);

    // Hard fade just below the horizon: the ground normally occludes these, but
    // a high camera can see past the ground disc's rim.
    let above = mix(smoothstep(-0.03, 0.0, dir.y), 1.0, space);

    let rgb = in.color.rgb * core * flicker * extinction * params.x * above;
    return vec4<f32>(rgb, 1.0);
}
