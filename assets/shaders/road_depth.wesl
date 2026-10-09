// The road stack's depth ladder — two rungs, ground and bridge.
//
// Road ribbons used to overlap by construction, and this file used to be what
// decided which of two coplanar surfaces was drawn: 399 rungs of class, casing,
// width band and tie colouring, each worth four depth-buffer units. Four was not
// enough — the sibling ladder that orders grass against water against road needs
// sixty-four, and sixteen had already been measured as too little at the shallow
// angles a low camera looks at the middle distance with — so junctions kept
// flickering however carefully the rungs were assigned.
//
// `cartopolis_geo::road_stitch` removes the overlap instead of ordering it. The
// ground roads are cut against each other into a partition, so nothing is left
// for a tie-break, and the only overlap that remains is the one that is real: a
// flyover and the road beneath it, both drawn on the ground plane. One rung of
// sixty-four units holds those apart.
//
// The rung is a depth offset, not a height, and that has not changed. A geometric
// gap's significance decays as 1/z — 24 µm is 2.0 units at 100 m, 1.0 at 200 m and
// nothing beyond — while scaling clip-space `z` shifts a surface by a fixed number
// of units of its own depth, which is what a hardware depth-bias unit is. The
// geometry is left exactly flat, so shadow casting and the collider footprints are
// untouched, and `prepass_vertex_shader` stays at bevy's default deliberately: the
// shadow map sees honest geometry.

import bevy_pbr::render::{
    forward_io::VertexOutput,
    mesh_functions,
    view_transformations::position_world_to_clip,
};

// One depth-buffer ULP as a fraction of the depth value, for `Depth32Float`:
// a bias unit at depth `D` is `2^(e-23) ~= D * 1.19e-7`. Scaling clip `z` by this
// times the rung count reproduces exactly what `depth_bias` would have done, had
// it been able to differ per vertex.
const ULP: f32 = 1.19e-7;

// How many ULPs one rung is worth. Must match
// `cartopolis_geo::vector_tiles::ROAD_RUNG_ULP` — `road_depth`'s
// `the_shader_and_the_ladder_agree_on_the_step` pins the pair.
const RUNG_ULP: f32 = 64.0;

struct RoadVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // `SurfaceMesh::rungs`, one per vertex: 0 on the ground, 1 on a bridge.
    // Constant across a ribbon, so this interpolates to itself.
    @location(8) rung: f32,
};

@vertex
fn vertex(v: RoadVertex) -> VertexOutput {
    var out: VertexOutput;

    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    out.world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(v.position, 1.0),
    );
    out.world_normal = mesh_functions::mesh_normal_local_to_world(v.normal, v.instance_index);
    // Unguarded, as `grass.wesl` does: `specialize` requires `ATTRIBUTE_UV_0` in
    // the layout, so `VERTEX_UVS_A` is always defined for this pipeline — and a
    // road without its UVs could not register its markings anyway.
    out.uv = v.uv;
    // This one *is* optional, and writing it unconditionally is a WGSL compile
    // error rather than a warning. Naga then rejects the module, the pipeline
    // fails to build, and bevy answers by skipping the pass — i.e. the roads
    // simply would not be drawn, with healthy mesh counts and nothing in the log
    // but `systems::pipeline_health`.
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = v.instance_index;

    out.position = position_world_to_clip(out.world_position.xyz);
    // Reversed-Z: nearer is *larger*, so a higher rung scales up and wins the
    // depth test.
    //
    // Scaled, not offset, and the projection is why. Bevy builds the camera
    // matrix with `perspective_infinite_reverse_rh`, so `clip.z` is the constant
    // near plane and `clip.w` is the view depth — `ndc_z = near / depth`. Scaling
    // `clip.z` therefore scales `ndc_z` by the same fraction, which is a fixed
    // number of ULPs of this fragment's *own* depth: precisely what a hardware
    // `depth_bias` unit is, and the reason the separation neither decays nor runs
    // away with range. Adding a constant to NDC instead would grow as z² and pull
    // a distant road metres in front of itself.
    out.position.z *= 1.0 + v.rung * RUNG_ULP * ULP;

    return out;
}
