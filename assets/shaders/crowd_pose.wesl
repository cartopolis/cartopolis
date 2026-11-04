// A street walker's pose, read from its body's baked poses: what both the
// colour pass (`crowd.wesl`) and the depth and shadow passes
// (`crowd_prepass.wesl`) draw it in, so it casts the shadow of the step it is
// taking.
//
// The body is a MakeHuman person cut down for a crowd
// (`cartopolis_human::crowd`). Its walk is not a skeleton: every vertex's
// position and normal were baked at each of `counts.y` poses through one walk
// cycle, then once standing, into `poses`. A walker's `MeshTag` says where it
// is in its cycle and how far it has slowed to a stand, so every walker
// wearing a body shares one mesh and one draw and still steps on its own.
//
// The tag, from bit 0:
//   0–11   the phase through the cycle, 0 … 4095
//   12–17  standing, 0 (walking) … 63 (stood still)
//   18–21  the top's colour, an index into `crowd.tops`
//   22–25  the bottom's colour, into `crowd.bottoms`

import bevy_pbr::render::{mesh_bindings::mesh, mesh_functions};

struct Crowd {
    // x: vertices. y: poses in the walk cycle. z: the pose texture's width.
    counts: vec4<u32>,
    tops: array<vec4<f32>, 16>,
    bottoms: array<vec4<f32>, 16>,
}
@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> crowd: Crowd;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var poses: texture_2d<f32>;

struct Posed {
    position: vec3<f32>,
    normal: vec3<f32>,
}

fn texel(pose: u32, v: u32, k: u32) -> vec3<f32> {
    let i = (pose * crowd.counts.x + v) * 2u + k;
    let w = crowd.counts.z;
    return textureLoad(poses, vec2<u32>(i % w, i / w), 0).xyz;
}

// The vertex's index in its own mesh. `vertex_index` counts from the start of
// the shared buffer the mesh allocator put this mesh in.
fn local_vertex(vertex_index: u32, instance_index: u32) -> u32 {
    return vertex_index - mesh[instance_index].first_vertex_index;
}

fn posed(v: u32, instance_index: u32) -> Posed {
    let tag = mesh_functions::get_tag(instance_index);
    let n = max(crowd.counts.y, 1u);
    let phase = f32(tag & 0xFFFu) / 4096.0 * f32(n);
    let a = u32(floor(phase)) % n;
    let b = (a + 1u) % n;
    let k = fract(phase);
    let stand = f32((tag >> 12u) & 0x3Fu) / 63.0;
    let walk_p = mix(texel(a, v, 0u), texel(b, v, 0u), k);
    let walk_n = mix(texel(a, v, 1u), texel(b, v, 1u), k);
    var out: Posed;
    out.position = mix(walk_p, texel(n, v, 0u), stand);
    out.normal = normalize(mix(walk_n, texel(n, v, 1u), stand));
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
