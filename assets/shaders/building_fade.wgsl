// The outermost ring of buildings goes down into the map rather than stopping at
// a line. `systems::building_fade` has the why; this is the half that moves.
//
// A building's height is scaled about its own base, which arrives as a vertex
// attribute because the far tier folds the ground height into the vertex Y where
// it cannot be recovered. Scaling about the world's zero instead would push a
// building through the ground wherever the terrain is above the datum and lift it
// off wherever it is below.
//
// The distance is Chebyshev in cells, not metres radially: what this has to reach
// zero at is a square ring of cells, and a radial ramp would finish early along
// the axes and leave the four corners standing at full height against the ring
// boundary — the hard line this exists to remove, kept where it reads worst.
//
// **There is no prepass in this client**, which is what lets this exist as a
// main-pass vertex stage alone. `systems::building_fade` says what a prepass would
// owe it.

#import bevy_pbr::{
    mesh_functions,
    view_transformations::position_world_to_clip,
    forward_io::VertexOutput,
}

// xy: the camera's ground position in render-space XZ. z: the scale turning a
// distance in metres into *ramp widths*. w: the distance, in those units, at
// which no height is left — and `0` is the off switch, which is what a material
// carries before the camera has been read and what `CARTO_NO_BUILDING_FADE=1`
// pins it to.
//
// Ramp widths, so the ramp's own size lives in `building_fade::FADE_CELLS` and
// nowhere else: the `1.0` below is structural, not a number to keep in step.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> ring: vec4<f32>;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(5) color: vec4<f32>,
    // The ground this vertex's building stands on
    // (`building_fade::ATTRIBUTE_BUILDING_BASE`).
    @location(8) base: f32,
};

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;

    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    var local = vertex.position;

    if ring.w > 0.0 {
        // Measured where the building *stands*, so every vertex of one building
        // takes the same factor and the box stays a box. Using each vertex's own
        // position would shear a roof away from the walls under it.
        let stands = mesh_functions::mesh_position_local_to_world(
            world_from_local,
            vec4<f32>(vertex.position.x, vertex.base, vertex.position.z, 1.0),
        );
        let d = abs(stands.xz - ring.xy) * ring.z;
        let reach = max(d.x, d.y);
        let f = 1.0 - smoothstep(ring.w - 1.0, ring.w, reach);
        local.y = vertex.base + (vertex.position.y - vertex.base) * f;
    }

    let world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(local, 1.0),
    );
    out.world_position = world_position;
    out.position = position_world_to_clip(world_position.xyz);
    // The normal is left alone. A uniform vertical squash does tilt a roof pitch,
    // but the ramp only runs where a building is a couple of pixels tall, and
    // re-deriving normals would cost a cross product per vertex to correct shading
    // nobody can resolve at that distance.
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        vertex.instance_index,
    );
#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif
    out.instance_index = vertex.instance_index;
    return out;
}
