// The ocean at globe scale — a BRDF split, not a wave field.
//
// `water.wgsl` gives the local tier real water. This is its counterpart from
// orbit, and it deliberately shares none of its machinery: at 3,000 km a wave is
// far below a pixel, so there is nothing to perturb. What separates sea from
// land in every photograph taken from space is that water is dark and glossy
// where land is bright and matte, and that the sun lays a broad glint across it.
// All of that is roughness, specular and tint — one mask bit per texel.
//
// The mask arrives in the **alpha channel of the globe's own imagery**
// (`tile_loader::load_globe_tile_rgba`), which is free: the tiles are opaque, so
// alpha was unused, and it needs no second texture or binding. It has to be read
// out of `pbr_input.material.base_color.a` *before* `alpha_discard`, which is
// what flattens alpha to 1 for an opaque material.
//
// `ocean.x` ramps the whole effect with `space_factor`, so the transition from
// the flat map tiles below the tier boundary carries no step.

import bevy_pbr::render::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
};

// x: effect strength (space_factor ramp). y: 1 when alpha carries a real mask.
// z: ocean roughness. w: ocean reflectance (StandardMaterial's 0..1 remap).
@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> ocean: vec4<f32>;
// rgb: water tint applied to the imagery. a unused.
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var<uniform> tint: vec4<f32>;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // Water coverage, 0..1. The imagery's mip chain averages the mask with the
    // colour, so a coastline softens with distance rather than aliasing — which
    // is exactly right, since a coastline *is* sub-pixel from orbit.
    let mask = pbr_input.material.base_color.a * ocean.y * clamp(ocean.x, 0.0, 1.0);

    // Sea darker than the land beside it. The basemap paints a pale cartographic
    // blue; left alone it has no headroom for a highlight and the glint clips to
    // a white blob instead of reading as sun on water.
    pbr_input.material.base_color = vec4<f32>(
        pbr_input.material.base_color.rgb * mix(vec3<f32>(1.0), tint.rgb, mask),
        1.0,
    );

    // The glint itself: a smooth dielectric where the matte land keeps its
    // roughness of 1. Broad on purpose (see `OCEAN_ROUGHNESS`) — a near-mirror
    // sphere concentrates the sun into a few pixels that crawl and alias as the
    // globe turns, where the real thing is the sum of a whole sea of facets.
    pbr_input.material.perceptual_roughness = mix(
        pbr_input.material.perceptual_roughness,
        ocean.z,
        mask,
    );
    pbr_input.material.reflectance = mix(
        pbr_input.material.reflectance,
        vec3<f32>(ocean.w),
        mask,
    );

    pbr_input.material.base_color = alpha_discard(
        pbr_input.material.flags,
        pbr_input.material.alpha_cutoff,
        pbr_input.material.base_color,
    );

    var out: FragmentOutput;
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
