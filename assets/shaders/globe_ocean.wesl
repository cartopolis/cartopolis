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
import bevy_pbr::render::mesh_bindings::mesh;
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};

// The material's two vectors, one buffer (`globe_ocean::GlobeOceanUniform`):
//
// ocean — x: effect strength (space_factor ramp). y: 1 when alpha carries a real
//   mask. z: ocean roughness. w: ocean reflectance (StandardMaterial's 0..1 remap).
// tint — rgb: water tint applied to the imagery. a unused.
// night — xyz: towards the sun, render space. w: how dark the night side goes.
// lights — x: city-light strength. y: 1 when the lights map (101) is bound.
struct GlobeOceanUniform {
    ocean: vec4<f32>,
    tint: vec4<f32>,
    night: vec4<f32>,
    lights: vec4<f32>,
}

// Bindless where the platform has it, so the globe's tiles — each a material of
// its own, with its own imagery — share a bind group and a batch set rather than
// costing one each (`globe_ocean::GlobeOceanUniform` says what that cost).
// One entry per material, naming bindless index 100.
@if(BINDLESS)
struct GlobeOceanIndices {
    material: u32, // 100
    lights_map: u32, // 101
    lights_sampler: u32, // 102
}
@if(BINDLESS) {
@group(constants::MATERIAL_BIND_GROUP) @binding(109) var<storage> globe_ocean_indices: array<GlobeOceanIndices>;
@group(constants::MATERIAL_BIND_GROUP) @binding(110) var<storage> globe_ocean_array: array<GlobeOceanUniform>;
}
@else {
@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> globe_ocean_single: GlobeOceanUniform;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var lights_map: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var lights_sampler: sampler;
}

// How much of the ground's own tone the night side keeps away from: the space
// ambient still lights it, and 0.78 matches the mockup's night wash.
const NIGHT_DARKEN: f32 = 0.78;

// A built-up texel's light at night, linear, before exposure: sodium-warm, and
// bright enough to read at the daylight exposure space holds.
const CITY_LIGHT: vec3<f32> = vec3<f32>(160.0, 115.0, 60.0);

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    @if(BINDLESS)
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    @if(BINDLESS)
    let params = globe_ocean_array[globe_ocean_indices[slot].material];
    @else
    let params = globe_ocean_single;
    let ocean = params.ocean;
    let tint = params.tint;
    let night = params.night;
    let lights = params.lights;

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

    // The night side, from the real sun: a twilight band about 12° wide round the
    // terminator, then the ground darkened towards the space ambient's floor and
    // the built-up land lit (`docs/mockups/macro-view/`, v3). Ramped with
    // `ocean.x`, which is `space_factor`, so it comes with the globe.
    let into_night = smoothstep(0.06, -0.14, dot(normalize(pbr_input.N), night.xyz))
        * night.w
        * clamp(ocean.x, 0.0, 1.0);
    if into_night > 0.0 {
        pbr_input.material.base_color = vec4<f32>(
            pbr_input.material.base_color.rgb * (1.0 - NIGHT_DARKEN * into_night),
            pbr_input.material.base_color.a,
        );
        var built = 0.0;
        @if(VERTEX_UVS_A)
        if lights.y > 0.5 {
            @if(BINDLESS)
            let l = textureSample(
                bindless_textures_2d[globe_ocean_indices[slot].lights_map],
                bindless_samplers_filtering[globe_ocean_indices[slot].lights_sampler],
                in.uv,
            );
            @else
            let l = textureSample(lights_map, lights_sampler, in.uv);
            built = l.r;
        }
        pbr_input.material.emissive = vec4<f32>(
            pbr_input.material.emissive.rgb + CITY_LIGHT * built * into_night * lights.x,
            pbr_input.material.emissive.a,
        );
    }

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
