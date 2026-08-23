// The road stack's depth ladder — the one thing that decides which of two
// overlapping road ribbons is drawn on top.
//
// Road ribbons **overlap by construction**: a junction stub crosses whatever it
// tees into, Shortbread's merged ways cross each other freely, and a flyover is
// drawn on the same ground plane as the road it flies over. Every one of those
// overlaps is two opaque coplanar triangles, whose depth values agree to the last
// few bits — so which one wins is decided by rasteriser rounding, and it re-rolls
// every frame the view matrix moves. That is the shimmer along a street and the
// bleeding at its junctions.
//
// `cartopolis_geo::vector_tiles::road_rung` settles it by giving every ribbon an
// explicit place in an ordered ladder — class, then casing, then width band, then
// a tie colouring. This file is the half that applies it.
//
// ## Why the rung is applied here and not as height
//
// It used to be height: each rung lifted a ribbon 24 µm. That works up close and
// **dissolves with distance**, because a geometric gap's significance in the depth
// buffer decays as 1/z — 24 µm is 2.0 ULP at 100 m, 1.0 at 200 m and nothing
// beyond. Past a couple of hundred metres the whole ladder was below the buffer's
// resolution and every same-class overlap went back to a per-pixel coin flip. No
// amount of cleverness in *assigning* rungs could reach that, because the rungs
// had stopped meaning anything.
//
// A depth offset proportional to the primitive's own depth cannot decay, which is
// the entire point: scaling clip-space `z` by `1 + k` shifts the surface toward
// the viewer by `k · z` in world terms, so the separation tracks the depth
// buffer's own quantum at every range. It is the same denomination
// `StandardMaterial::depth_bias` uses (`1.19e-7` per ULP) — the difference is that
// a material constant is one value for every ribbon sharing the material, and the
// fight this settles is *between* two such ribbons. Hence a vertex attribute.
//
// Two consequences worth keeping in mind. The geometry is left exactly flat, so
// the ladder costs no height and can be as tall as it likes — which is what let
// the width bands and the tie set go back to full resolution. And because nothing
// moves in world space, shadow casting and the collider footprints are untouched:
// only `prepass_vertex_shader` is left at bevy's default, deliberately, so the
// shadow map sees honest geometry.

#import bevy_pbr::{
    forward_io::VertexOutput,
    mesh_functions,
    view_transformations::position_world_to_clip,
}

// One depth-buffer ULP as a fraction of the depth value, for `Depth32Float`:
// a bias unit at depth `D` is `2^(e-23) ~= D * 1.19e-7`. Scaling clip `z` by this
// times the rung count reproduces exactly what `depth_bias` would have done, had
// it been able to differ per vertex.
const ULP: f32 = 1.19e-7;

// How many ULPs one rung is worth. Must match
// `cartopolis_geo::vector_tiles::ROAD_RUNG_ULP` — `road_depth`'s
// `the_shader_and_the_ladder_agree_on_the_step` pins the pair.
const RUNG_ULP: f32 = 4.0;

struct RoadVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // `SurfaceMesh::rungs`, one per vertex — the whole road ordering as a single
    // number. Constant across a ribbon, so this interpolates to itself.
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
    // Unguarded, as `grass.wgsl` does: `specialize` requires `ATTRIBUTE_UV_0` in
    // the layout, so `VERTEX_UVS_A` is always defined for this pipeline — and a
    // road without its UVs could not register its markings anyway.
    out.uv = v.uv;
    // This one *is* optional, and writing it unconditionally is a WGSL compile
    // error rather than a warning. Naga then rejects the module, the pipeline
    // fails to build, and bevy answers by skipping the pass — i.e. the roads
    // simply would not be drawn, with healthy mesh counts and nothing in the log
    // but `systems::pipeline_health`.
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = v.instance_index;
#endif

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
