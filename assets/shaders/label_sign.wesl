// A street name on the road: the sign's slot of the shared atlas, composed from
// two coverage channels — ink over a halo — in the colours the material carries.
// Unlit (`StandardMaterial { unlit: true }`), so the lighting branch is dead
// code kept for the material's own flag; fog and tonemapping still apply.
//
// The slot and the name's extent within it come from the sign's `MeshTag`
// (`cartopolis_materials::label_sign::sign_tag`): bits 0–7 the slot, 8–19 the
// width fraction, 20–31 the height fraction, each in 1/4095.

import bevy_pbr::render::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_functions,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
};

struct LabelSign {
    // x, y: one slot cell as a fraction of the atlas. z: cells per row.
    cell: vec4<f32>,
    ink: vec4<f32>,
    // rgb: the halo's colour. w: its opacity.
    halo: vec4<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> sign: LabelSign;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var atlas: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var atlas_sampler: sampler;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let tag = mesh_functions::get_tag(in.instance_index);
    let slot = tag & 0xFFu;
    let fu = f32((tag >> 8u) & 0xFFFu) / 4095.0;
    let fv = f32((tag >> 20u) & 0xFFFu) / 4095.0;
    let cols = max(u32(sign.cell.z), 1u);
    let cell = vec2<f32>(f32(slot % cols), f32(slot / cols));
    // The quad's u runs along the name and v across it, 0..1 over the whole
    // sign; the name fills `fu` of its cell's width and `fv` of its height.
    let uv = (cell + in.uv * vec2<f32>(fu, fv)) * sign.cell.xy;
    let t = textureSample(atlas, atlas_sampler, uv).rg;

    // Ink over halo, source-over, as the rasteriser composed it before.
    let ink_a = t.r;
    let halo_a = t.g * sign.halo.w;
    let a = ink_a + halo_a * (1.0 - ink_a);
    var rgb = vec3<f32>(0.0);
    if a > 0.0 {
        rgb = (sign.ink.rgb * ink_a + sign.halo.rgb * halo_a * (1.0 - ink_a)) / a;
    }
    pbr_input.material.base_color = vec4<f32>(rgb, a);

    var out: FragmentOutput;
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
