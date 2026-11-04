// A street walker in the colour pass: posed from its baked walk
// (`crowd_pose.wesl`), then lit by `StandardMaterial`'s own fragment stage, as
// everything else in the street is.

import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_functions,
    view_transformations::position_world_to_clip,
};
import super::crowd_pose::{local_vertex, posed, tinted};

// Exactly the attributes the crowd's meshes carry: no UVs, and the index the
// pose is looked up by.
struct CrowdVertex {
    @builtin(instance_index) instance_index: u32,
    @builtin(vertex_index) vertex_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(5) color: vec4<f32>,
};

@vertex
fn vertex(in: CrowdVertex) -> VertexOutput {
    var out: VertexOutput;
    let p = posed(local_vertex(in.vertex_index, in.instance_index), in.instance_index);
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    out.world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(p.position, 1.0),
    );
    out.position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(p.normal, in.instance_index);
@if(VERTEX_COLORS)
    out.color = tinted(in.color, in.instance_index);
@if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = in.instance_index;
@if(VISIBILITY_RANGE_DITHER)
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        in.instance_index, world_from_local[3]);
    return out;
}
