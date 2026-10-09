// Satellites in orbit, one screen-facing point each (`systems::satellites`).
//
// The mesh is a snapshot taken on a worker every few seconds: per satellite,
// four corners that all carry its position (`position`) and velocity (`normal`)
// at one instant, in the anchor's frame. This stage moves each point forward
// from that instant by `motion.w` seconds under the Earth's gravity — a second-
// order step, which over the rebuild interval is metres off the full SGP4
// answer at the speed of the space station, i.e. far below a pixel at any
// altitude this layer is drawn from. So a satellite glides rather than jumping
// once per rebuild, and nothing is uploaded between rebuilds but the uniform.
//
// The point keeps its size on screen whatever its distance: a satellite is far
// below a pixel at its true size from anywhere this is shown, and a world-sized
// marker would vanish from orbit and swamp the view from close by.

import bevy_pbr::render::{
    mesh_functions,
    mesh_view_bindings::view,
    view_transformations::position_world_to_clip,
};

// xyz: the Earth's centre in the mesh's frame, metres. w: seconds since the
// snapshot.
@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> motion: vec4<f32>;

// x: GM, m³/s². y: brightness. z: point-size multiplier. w: spare.
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var<uniform> params: vec4<f32>;

struct SatVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    // The satellite's velocity, m/s. In the normal slot because it is a vec3 per
    // vertex and the mesh has no other use for one.
    @location(1) normal: vec3<f32>,
    // The quad corner, ±1.
    @location(2) uv: vec2<f32>,
    // rgb: the kind's colour. a: the point's diameter in pixels.
    @location(5) color: vec4<f32>,
};

struct SatOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec3<f32>,
};

@vertex
fn vertex(v: SatVertex) -> SatOut {
    var out: SatOut;

    let dt = motion.w;
    let rel = v.position - motion.xyz;
    let r = max(length(rel), 1.0);
    let accel = -params.x * rel / (r * r * r);
    let local = v.position + v.normal * dt + 0.5 * accel * dt * dt;

    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let world = (world_from_local * vec4<f32>(local, 1.0)).xyz;
    var clip = position_world_to_clip(world);

    // Corner offset in pixels, carried into clip space. Multiplying by w undoes
    // the perspective divide that will follow, which is what keeps the size
    // constant on screen.
    let px = v.color.a * params.z;
    clip = vec4<f32>(clip.xy + v.uv * px / view.viewport.zw * clip.w, clip.zw);

    out.position = clip;
    out.uv = v.uv;
    out.tint = v.color.rgb;
    return out;
}

@fragment
fn fragment(in: SatOut) -> @location(0) vec4<f32> {
    // A bright core with a soft rim, so a point a pixel or two across still
    // reads as round rather than as a square.
    let d = length(in.uv);
    let core = 1.0 - smoothstep(0.35, 1.0, d);
    if core <= 0.003 {
        discard;
    }
    // Premultiplied with an alpha of zero: under `AlphaMode::Add` that is pure
    // `src + dst`. A custom material's output is not scaled by the camera's
    // exposure, so this is a display value directly.
    return vec4<f32>(in.tint * core * params.y, 0.0);
}
