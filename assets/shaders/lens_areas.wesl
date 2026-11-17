// A lens's areas over the map (`systems::lens_areas`): fills and edges of one
// board, each vertex carrying the texel of its area's colour in UV0, so a year
// played or a figure switched rewrites a texel an area and nothing else.
//
// Drawn with the depth test off, over the map as the painted overlay was; what
// would show through the planet is dropped here instead. A point is kept while
// the camera stands on its side of the ground's tangent plane there — the same
// horizon test `place_pins::place_delta` makes, `params.y` metres of margin
// included — so the far side of the globe, and ground past the horizon on the
// flat map, are not drawn over the near.

import bevy_pbr::render::{
    mesh_functions,
    mesh_view_bindings::view,
    view_transformations::position_world_to_clip,
};

// x: the board's opacity. y: the horizon margin, metres.
@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> params: vec4<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var colours: texture_2d<f32>;

struct LensVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    // The ground's up at the point: away from the Earth's centre.
    @location(1) normal: vec3<f32>,
    // The area's colour texel, whole numbers.
    @location(2) uv: vec2<f32>,
};

struct LensOut {
    @builtin(position) position: vec4<f32>,
    @location(0) colour: vec4<f32>,
    // How far the camera stands over the ground's tangent plane here: < 0 is
    // past the horizon.
    @location(1) facing: f32,
};

@vertex
fn vertex(v: LensVertex) -> LensOut {
    var out: LensOut;
    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let world = (world_from_local * vec4<f32>(v.position, 1.0)).xyz;
    let up = normalize((world_from_local * vec4<f32>(v.normal, 0.0)).xyz);
    out.position = position_world_to_clip(world);
    out.colour = textureLoad(colours, vec2<i32>(v.uv), 0);
    out.facing = dot(view.world_position - world, up) + params.y;
    return out;
}

@fragment
fn fragment(in: LensOut) -> @location(0) vec4<f32> {
    if in.facing < 0.0 {
        discard;
    }
    let a = in.colour.a * params.x;
    if a <= 0.002 {
        discard;
    }
    // A custom material's output is not scaled by the camera's exposure, and
    // the neutral tonemapper leaves colours this dark as they are, so the
    // map shows the key's colours.
    return vec4<f32>(in.colour.rgb, a);
}
