// The design room's floor (`systems::design_room`): a technical grid, drawn
// per pixel rather than as line meshes.
//
// Line meshes were thin quads, and a quad narrower than a pixel breaks into
// dashes and grain as it recedes — the "ugly grid" of the first room. Here each
// pixel measures its distance to the nearest line in its own footprint
// (`fwidth`), so a line is the same crisp width in pixels near and far, and a
// set of lines leaves before its spacing falls under a few pixels, which is
// where the grey moiré band at the horizon came from.
//
// UV0 is metres from the spot (x east, y south), the mesh's own: keyed to
// anchor metres, never `world_position`, which `big_space` jumps a whole cell
// at a time (`water-surface.md`).

import bevy_pbr::render::forward_io::VertexOutput;

// x: the room's white, linear (above 1.0: the tonemapper takes 1.0 to a light
// grey). y, z: the lines fade out between these distances from the spot,
// metres. w: spare.
@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> params: vec4<f32>;

// Coverage of the lines every `spacing` metres, `width` pixels wide.
fn lines(p: vec2<f32>, spacing: f32, width: f32) -> f32 {
    let q = p / spacing;
    let footprint = max(fwidth(q), vec2<f32>(1e-5));
    let d = abs(fract(q - 0.5) - 0.5) / footprint;
    return 1.0 - min(min(d.x, d.y) / width, 1.0);
}

// Coverage of the one line where `c` is zero, `width` pixels wide.
fn axis(c: f32, width: f32) -> f32 {
    let d = abs(c) / max(fwidth(c), 1e-5);
    return 1.0 - min(d / width, 1.0);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let p = in.uv;
    let white = params.x;
    let fade = 1.0 - smoothstep(params.y, params.z, length(p));
    // Pixels per metre here: the metre lines go before they are 6 px apart,
    // the five-metre lines before they are 2 px apart.
    let per_m = 1.0 / max(max(fwidth(p.x), fwidth(p.y)), 1e-5);
    let minor = lines(p, 1.0, 1.0) * smoothstep(6.0, 14.0, per_m) * 0.40;
    let major = lines(p, 5.0, 1.4) * smoothstep(2.0, 5.0, per_m) * 0.55;
    // A drawing office's grey, a touch cool.
    let ink = vec3<f32>(0.50, 0.54, 0.60) * white;
    var rgb = mix(vec3<f32>(white), ink, max(minor, major) * fade);
    // Blender's axes: red X (east), blue Z (south here).
    rgb = mix(rgb, vec3<f32>(0.80, 0.16, 0.19), axis(p.y, 1.8) * fade);
    rgb = mix(rgb, vec3<f32>(0.18, 0.40, 0.86), axis(p.x, 1.8) * fade);
    return vec4<f32>(rgb, 1.0);
}
