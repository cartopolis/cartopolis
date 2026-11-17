// A street walker's pose, read from its body's baked poses: what both the
// colour pass (`crowd.wesl`) and the depth and shadow passes
// (`crowd_prepass.wesl`) draw it in, so it casts the shadow of the step it is
// taking.
//
// The body is a MakeHuman person cut down for a crowd
// (`cartopolis_human::crowd`). Its walk is not a skeleton: every vertex's
// position and normal were baked at each of `counts.y` poses through one walk
// cycle, then once standing, then (riders) a turn of the cranks and once
// stopped with a foot down, then (finest level only) at `counts.w` poses
// through a wave, into `poses`. A walker's `MeshTag` says where it is in its
// cycle and how far it has slowed to a stand, so every walker wearing a body
// shares one mesh and one draw and still steps on its own.
//
// A cyclist wears the same body and texture through a material of its own,
// whose `cycle.x` starts its cycle at the riding poses — a turn of the cranks,
// baked after the standing pose — so its phase is the cranks', and whose
// `cycle.y` is the stopped pose: a rider "stands" by putting a foot down.
//
// The tag, from bit 0:
//   0–11   the phase through the walk cycle, 0 … 4095; waving, through the wave
//   12–17  standing, 0 (walking) … 63 (stood still); waving, how far from
//          standing into the wave, 0 … 63
//   18–21  the top's colour, an index into `crowd.tops`
//   22–25  the bottom's colour, into `crowd.bottoms`
//   26     waving
//   27–31  the head's turn, signed (−16 … 15) steps of `HEAD_STEP` radians
//          about the body's up axis, positive toward its +x
//
// The head turn is not a pose: a vertex's share of it (1 on the head, ½ on the
// neck) is the `w` of its position texel, and it turns about the head joint as
// it is in that pose (`crowd.pivots`, one a pose).

import bevy_pbr::render::{mesh_bindings::mesh, mesh_functions};

// Keep in step with `systems::crowd::HEAD_STEP`.
const HEAD_STEP: f32 = 0.0785398; // 4.5°

struct Crowd {
    // x: vertices. y: poses in the cycle played. z: the pose texture's width.
    // w: poses in the wave (0 where none is baked or played).
    counts: vec4<u32>,
    // x: the cycle's first pose. y: the standing pose. z: the wave's first.
    cycle: vec4<u32>,
    tops: array<vec4<f32>, 16>,
    bottoms: array<vec4<f32>, 16>,
    // Per pose, the head joint in the body's frame.
    pivots: array<vec4<f32>, 40>,
}
@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> crowd: Crowd;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var poses: texture_2d<f32>;

struct Posed {
    position: vec3<f32>,
    normal: vec3<f32>,
}

fn texel(pose: u32, v: u32, k: u32) -> vec4<f32> {
    let i = (pose * crowd.counts.x + v) * 2u + k;
    let w = crowd.counts.z;
    return textureLoad(poses, vec2<u32>(i % w, i / w), 0);
}

// The vertex's index in its own mesh. `vertex_index` counts from the start of
// the shared buffer the mesh allocator put this mesh in.
fn local_vertex(vertex_index: u32, instance_index: u32) -> u32 {
    return vertex_index - mesh[instance_index].first_vertex_index;
}

fn posed(v: u32, instance_index: u32) -> Posed {
    let tag = mesh_functions::get_tag(instance_index);
    let n = max(crowd.counts.y, 1u);
    let m = crowd.counts.w;
    // The cycle played starts at `cycle.x` (the walk's 0, or the ride's);
    // standing is `cycle.y`, the wave from `cycle.z`.
    let first = crowd.cycle.x;
    let st = crowd.cycle.y;
    let along = f32(tag & 0xFFFu) / 4096.0;
    let amount = f32((tag >> 12u) & 0x3Fu) / 63.0;
    let stand_p = texel(st, v, 0u);
    let stand_n = texel(st, v, 1u).xyz;
    var p: vec4<f32>;
    var nrm: vec3<f32>;
    var pivot: vec3<f32>;
    if ((tag >> 26u) & 1u) == 1u {
        // Waving: from standing into the wave by `amount`. A level with no
        // wave baked stands.
        if m > 1u {
            let x = along * 4096.0 / 4095.0 * f32(m - 1u);
            let a = min(u32(floor(x)), m - 2u);
            let k = clamp(x - f32(a), 0.0, 1.0);
            let pa = crowd.cycle.z + a;
            let wave_p = mix(texel(pa, v, 0u), texel(pa + 1u, v, 0u), k);
            let wave_n = mix(texel(pa, v, 1u).xyz, texel(pa + 1u, v, 1u).xyz, k);
            let wave_pivot = mix(crowd.pivots[pa].xyz, crowd.pivots[pa + 1u].xyz, k);
            p = mix(stand_p, wave_p, amount);
            nrm = mix(stand_n, wave_n, amount);
            pivot = mix(crowd.pivots[st].xyz, wave_pivot, amount);
        } else {
            p = stand_p;
            nrm = stand_n;
            pivot = crowd.pivots[st].xyz;
        }
    } else {
        let phase = along * f32(n);
        let a = first + u32(floor(phase)) % n;
        let b = first + (u32(floor(phase)) + 1u) % n;
        let k = fract(phase);
        let walk_p = mix(texel(a, v, 0u), texel(b, v, 0u), k);
        let walk_n = mix(texel(a, v, 1u).xyz, texel(b, v, 1u).xyz, k);
        let walk_pivot = mix(crowd.pivots[a].xyz, crowd.pivots[b].xyz, k);
        p = mix(walk_p, stand_p, amount);
        nrm = mix(walk_n, stand_n, amount);
        pivot = mix(walk_pivot, crowd.pivots[st].xyz, amount);
    }
    var out: Posed;
    out.position = p.xyz;
    out.normal = nrm;
    // The head turn, sign-extended from its five bits.
    let steps = bitcast<i32>(tag) >> 27u;
    if steps != 0 && p.w > 0.0 {
        let yaw = f32(steps) * HEAD_STEP * p.w;
        let c = cos(yaw);
        let s = sin(yaw);
        let d = out.position - pivot;
        out.position = pivot + vec3<f32>(c * d.x + s * d.z, d.y, -s * d.x + c * d.z);
        out.normal = vec3<f32>(c * nrm.x + s * nrm.z, nrm.y, -s * nrm.x + c * nrm.z);
    }
    out.normal = normalize(out.normal);
    return out;
}

// The vertex colour, with a grey top or bottom (alpha 0.5 or 1) multiplied by
// the walker's own colour for it; opaque.
fn tinted(color: vec4<f32>, instance_index: u32) -> vec4<f32> {
    let tag = mesh_functions::get_tag(instance_index);
    var tint = vec3<f32>(1.0);
    if color.a > 0.75 {
        tint = crowd.bottoms[(tag >> 22u) & 15u].rgb;
    } else if color.a > 0.25 {
        tint = crowd.tops[(tag >> 18u) & 15u].rgb;
    }
    return vec4<f32>(color.rgb * tint, 1.0);
}
