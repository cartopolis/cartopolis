// Analytic single-scattering sky, for the GPUs that cannot run bevy_pbr's
// atmosphere (`systems::sky_dome` has the whole argument — the short version is
// that bevy's `ScatteringMedium` keeps its LUTs in Rgba32Float and samples them
// with a filtering sampler, which a great many mobile adapters do not allow, so
// on those the sky is switched off and the dome below stands in for it).
//
// It is one shell around the camera and one fragment of arithmetic per pixel:
// no compute pass, no lookup tables, no float32 textures, nothing that a GLES-era
// mobile adapter can refuse. What it gives up against the real thing is
// multiple scattering (so it cannot know its own indirect light — see the
// twilight floor the CPU side supplies), aerial perspective on geometry (the
// camera's `DistanceFog` keeps doing that), and any dependence on the observer's
// position over a curved planet: this is a flat-earth sky, which is exactly
// right for the tier it draws in and is why it is hidden before the globe
// appears.
//
// Sampled by per-pixel view direction, like starfield.wgsl and horizonhaze.wgsl,
// so the mesh is only a cheap shell — its shape does not enter the shading.
import bevy_pbr::render::forward_io::VertexOutput;
import bevy_pbr::render::mesh_view_bindings::view;

// xyz: world direction to the sun. w: sun-disc brightness, as a multiplier on
// `source` (0 once the disc has set).
@group(3) @binding(0) var<uniform> sun: vec4<f32>;
// xyz: Rayleigh scattering optical depth at the zenith, per channel, already
// scaled by the barometric column above the camera. w: Mie asymmetry g.
@group(3) @binding(1) var<uniform> rayleigh: vec4<f32>;
// xyz: the same for the aerosol (Mie) term. w: cosine of the sun disc's angular
// radius.
@group(3) @binding(2) var<uniform> mie: vec4<f32>;
// xyz: the light being scattered — sunlight at the scattering altitude, as a
// radiance (lux/π), pre-exposure. w unused.
@group(3) @binding(3) var<uniform> source: vec4<f32>;
// xyz: what the sky is worth when the sun cannot account for it — the twilight
// and night floor, also pre-exposure. w unused.
@group(3) @binding(4) var<uniform> night: vec4<f32>;

/// Relative optical air mass (Kasten–Young 1989): 1 overhead, ~38 at the horizon.
/// The same curve `utils::sun::air_mass` uses for the sun beam, so the sky and
/// the sunlight redden on one model rather than two.
fn air_mass(sin_elevation: f32) -> f32 {
    let s = max(sin_elevation, 0.0);
    let h_deg = degrees(asin(s));
    return 1.0 / (s + 0.50572 * pow(h_deg + 6.07995, -1.6364));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let dir = normalize(in.world_position.xyz - view.world_position.xyz);
    let mu = dot(dir, sun.xyz);

    // Below the horizon the air mass is held at its horizon value (`max` inside
    // air_mass), so the thin band of dome that shows under the true horizon —
    // past the rim of the ground disc — carries the horizon's colour instead of
    // an edge. Real ground covers all of it that is closer than the shell.
    let air = air_mass(dir.y);

    // Phase functions, each multiplied by 4π so that an isotropic scatterer would
    // be exactly 1 and the ratio below stays normalised.
    let phase_rayleigh = 0.75 * (1.0 + mu * mu);
    let g = rayleigh.w;
    let denom = max(1.0 + g * g - 2.0 * g * mu, 1e-4);
    let phase_mie = (1.0 - g * g) / pow(denom, 1.5);

    // Single scattering through a uniform slab of `air` zenith-columns: the
    // scattered fraction saturates at 1 as the path gets long, per channel. That
    // per-channel saturation is the whole colour story — blue scatters out first,
    // so a short (zenith) path is blue and a long (horizon) one has run out of
    // blue to scatter and goes pale, then red as the source itself reddens.
    let beta_r = rayleigh.xyz;
    let beta_m = mie.xyz;
    let extinction = max(beta_r + beta_m, vec3<f32>(1e-6));
    let scattered = (beta_r * phase_rayleigh + beta_m * phase_mie) / extinction;
    let inscatter = scattered * (1.0 - exp(-extinction * air));

    var rgb = inscatter * source.xyz + night.xyz;

    // The sun itself. bevy's procedural `SunDisk` is drawn by the atmosphere's
    // sky pass, so on this path there is no sun in the sky unless the dome draws
    // one. Softened over the outer eighth of its radius (a quarter of the way in
    // *cosine*, which is what `mix` is interpolating) — at the disc's ~0.7°
    // angular radius a hard edge aliases into a visible polygon as the camera
    // turns.
    let cos_r = mie.w;
    let disc = smoothstep(cos_r, mix(cos_r, 1.0, 0.25), mu);
    rgb += source.xyz * sun.w * disc;

    // Same convention as horizonhaze.wgsl: the uniforms above are photometric,
    // and the camera's exposure is what brings them into the tonemapper's range.
    // Going through `view.exposure` rather than baking a constant in is what
    // keeps the sky on the day/night cycle's eye adaptation.
    return vec4<f32>(rgb * view.exposure, 1.0);
}
