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
// The one thing it does compute is a wall's height above its own base, out of
// two bytes that layout left spare — see `height_code` below.
//
// Main pass only. The shadow pass is bevy's prepass pipeline, whose vertex
// inputs sit at different locations; `BuildingWindows::specialize` leaves it
// alone, and it reads the position alone.

import bevy_pbr::render::{
    mesh_functions,
    view_transformations::position_world_to_clip,
    forward_io::VertexOutput,
};

// `osm_buildings::GROUND_FLOOR_M` and `GROUND_STEP_M`.
const GROUND_FLOOR_M: f32 = -128.0;
const GROUND_STEP_M: f32 = 0.01;

/// A height above the building's base as a normal's length: 4 at the base,
/// one more every 64 m, so any height from 128 m below the base up is at least
/// 2 — and a mesh that carries none keeps a unit normal, which reads as
/// "unknown". `building_windows.wesl::height_above_base` inverts it.
fn height_code(h: f32) -> f32 {
    return 4.0 + max(h, -128.0) / 64.0;
}

/// The base `osm_buildings::pack_ground` wrote: the high byte as the normal's
/// fourth snorm component, the low byte as the colour's unorm alpha.
fn packed_ground(normal_w: f32, color_a: f32) -> f32 {
    let hi = round(normal_w * 127.0) + 127.0;
    let lo = round(color_a * 255.0);
    return (hi * 256.0 + lo) * GROUND_STEP_M + GROUND_FLOOR_M;
}

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    // `Snorm8x4` on the wire. On a facade-coded mesh the fourth byte is the
    // high byte of the building's base (`osm_buildings::pack_ground`); on any
    // other it is padding, and the layout may be three floats, which a `vec4`
    // could not be declared over.
    @if(BUILDING_FACADE_CODE)
    @location(1) normal: vec4<f32>,
    @else
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
        vertex.normal.xyz,
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
    // The building's base, out of the two spare bytes, and this vertex's height
    // above it handed to the fragment as the world normal's *length*
    // (`height_code`) — the one place a `VertexOutput` has room, since bevy
    // reads every other field. A wall's three corners share one normal, so the
    // length interpolates exactly as the height does; `building_windows.wesl`
    // takes it back off and renormalises before anything lights with it.
    @if(VERTEX_COLORS && BUILDING_FACADE_CODE)
    out.world_normal = normalize(out.world_normal)
        * height_code(vertex.position.y - packed_ground(vertex.normal.w, vertex.color.a));
    return out;
}
