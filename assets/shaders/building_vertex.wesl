// The LoD2.2 buildings' vertex stage: bevy's own `mesh.wesl` vertex, minus the
// skinning and morph paths a building never has, plus one attribute.
//
// It exists for the vertex format, not for anything it computes. A building's
// colour is stored as four bytes (`Unorm8x4`) instead of four floats, and the
// facade code the fragment shader reads out of the colour's alpha — occupancy,
// era, shop and eaves, packed into one float well past 1.0
// (`building_windows::Facade`) — arrives as its own `f32` at location 8. This is
// where the two are put back together, so `building_windows.wesl` reads
// `base_color.a` exactly as it did when the colour was four floats.
// `osm_buildings::MeshData::with_facade_code` has the byte arithmetic.
//
// Main pass only. The shadow pass is bevy's prepass pipeline, whose vertex
// inputs sit at different locations; `BuildingWindows::specialize` leaves it
// alone, and it reads the position alone.

import bevy_pbr::render::{
    mesh_functions,
    view_transformations::position_world_to_clip,
    forward_io::VertexOutput,
};

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    // `Snorm8x4` on the wire; the fourth component is padding.
    @location(1) normal: vec3<f32>,
    @if(VERTEX_UVS_A)
    @location(2) uv: vec2<f32>,
    // `Unorm8x4` when the facade code is split out (its alpha byte is then
    // unused), four floats on a mesh built the old way.
    @if(VERTEX_COLORS)
    @location(5) color: vec4<f32>,
    @if(BUILDING_FACADE_CODE)
    @location(8) facade: f32,
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
    @if(VERTEX_UVS_A)
    out.uv = vertex.uv;
    // A mesh built without the split (`specialize` found no facade attribute)
    // carries its code in the alpha as it always did.
    @if(VERTEX_COLORS && BUILDING_FACADE_CODE)
    out.color = vec4<f32>(vertex.color.rgb, vertex.facade);
    @elif(VERTEX_COLORS)
    out.color = vertex.color;
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = vertex.instance_index;
    @if(VISIBILITY_RANGE_DITHER)
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        vertex.instance_index,
        world_from_local[3],
    );
    return out;
}
