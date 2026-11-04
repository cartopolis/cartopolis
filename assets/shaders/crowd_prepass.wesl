// A street walker in the depth, normal and shadow passes: the same pose as
// `crowd.wesl` draws, so the shadow steps with the body. Bevy's prepass
// fragment stage (where one runs at all) is left as it is.

import bevy_pbr::{
    prepass::io::VertexOutput,
    render::{mesh_functions, view_transformations::position_world_to_clip},
};
import super::crowd_pose::{local_vertex, posed, tinted};

// The prepass numbers its attributes differently from the colour pass
// (`prepass::io`): position 0, and the colour at 7. The normal is read from
// the poses, so the mesh's own is not needed here.
struct CrowdVertex {
    @builtin(instance_index) instance_index: u32,
    @builtin(vertex_index) vertex_index: u32,
    @location(0) position: vec3<f32>,
@if(VERTEX_COLORS)
    @location(7) color: vec4<f32>,
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
@if(UNCLIPPED_DEPTH_ORTHO_EMULATION) {
    out.unclipped_depth = out.position.z;
    out.position.z = min(out.position.z, 1.0);
}
@if(NORMAL_PREPASS_OR_DEFERRED_PREPASS)
    out.world_normal = mesh_functions::mesh_normal_local_to_world(p.normal, in.instance_index);
@if(VERTEX_COLORS)
    out.color = tinted(in.color, in.instance_index);
@if(MOTION_VECTOR_PREPASS)
    // The pose a frame ago is not kept; the body's own motion is what moves.
    out.previous_world_position = mesh_functions::mesh_position_local_to_world(
        mesh_functions::get_previous_world_from_local(in.instance_index),
        vec4<f32>(p.position, 1.0),
    );
@if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = in.instance_index;
@if(VISIBILITY_RANGE_DITHER)
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        in.instance_index, world_from_local[3]);
    return out;
}
