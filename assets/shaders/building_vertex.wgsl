// The LoD2.2 buildings' vertex stage: bevy's own `mesh.wgsl` vertex, minus the
// skinning and morph paths a building never has, plus one attribute.
//
// It exists for the vertex format, not for anything it computes. A building's
// colour is stored as four bytes (`Unorm8x4`) instead of four floats, and the
// facade code the fragment shader reads out of the colour's alpha — occupancy,
// era, shop and eaves, packed into one float well past 1.0
// (`building_windows::Facade`) — arrives as its own `f32` at location 8. This is
// where the two are put back together, so `building_windows.wgsl` reads
// `base_color.a` exactly as it did when the colour was four floats.
// `osm_buildings::MeshData::with_facade_code` has the byte arithmetic.
//
// Main pass only. The shadow pass is bevy's prepass pipeline, whose vertex
// inputs sit at different locations; `BuildingWindows::specialize` leaves it
// alone, and it reads the position alone.

#import bevy_pbr::{
    mesh_functions,
    view_transformations::position_world_to_clip,
    forward_io::VertexOutput,
}

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    // `Snorm8x4` on the wire; the fourth component is padding.
    @location(1) normal: vec3<f32>,
#ifdef VERTEX_UVS_A
    @location(2) uv: vec2<f32>,
#endif
    // `Unorm8x4` when the facade code is split out (its alpha byte is then
    // unused), four floats on a mesh built the old way.
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
#ifdef BUILDING_FACADE_CODE
    @location(8) facade: f32,
#endif
};

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        vertex.instance_index,
    );
    out.world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(vertex.position, 1.0),
    );
    out.position = position_world_to_clip(out.world_position.xyz);
#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_COLORS
#ifdef BUILDING_FACADE_CODE
    out.color = vec4<f32>(vertex.color.rgb, vertex.facade);
#else
    // A mesh built without the split (`specialize` found no facade attribute)
    // carries its code in the alpha as it always did.
    out.color = vertex.color;
#endif
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif
#ifdef VISIBILITY_RANGE_DITHER
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        vertex.instance_index,
        world_from_local[3],
    );
#endif
    return out;
}
