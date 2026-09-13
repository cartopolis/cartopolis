// Sub-metre ground: the half-metre tier's slope, applied to the normal.
//
// `systems::terrain`'s field is 3.125 m and the meshes that stand on it carry a
// vertex every 6 m. That is what gives a street a *gradient*; it is not what gives
// it a kerb. AHN measures the ground at half a metre and `terrain`'s fine tier
// already fetches it, but half-metre geometry over a z14 cell is nine million quads
// and is never going to be drawn.
//
// So the sub-metre half of the ground lives here, in the normal, exactly as
// `water.wgsl`'s waves do and for the same reason: a 12 cm kerb, the camber of a
// carriageway, the lip of a quay and a rut across a verge are all *shading*, not
// silhouette. Nothing is displaced, nothing is subdivided, and the cost is one
// texture fetch per ground fragment.
//
// # What the texture holds, and why it is a residual
//
// `terrain::encode_slope` writes the tier's slope **minus a box blur of itself at
// `MAX_GROUND_EDGE_M`** — the slope the mesh underneath is already drawn with. Add
// the raw slope instead and every gradient is applied twice, once in the vertices
// and once here, and a hillside shades as if it were half again as steep.
//
// # It is in render space, and that is a moving target
//
// `detail.xy` is the box's centre in **render** space, not the anchored world frame:
// under `big_space` the origin jumps a whole `WORLD_CELL_EDGE` as the camera crosses
// a grid cell. `ground_detail::refresh_ground_detail` rewrites it whenever the box
// re-centres or the origin moves — which is rare, twice a minute on foot at worst —
// rather than every frame, because unlike `building_windows` there are hundreds of
// ground materials and not one.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
}

// x, z: the detail box's centre in render-space XZ. z: 1 / its half-extent, so
// `(world_position.xz - centre) * inv_half` is −1..1 across it. w: strength, 0 when
// there is no tier to read (the layer off, outside the Netherlands, still fetching),
// which is what makes this shader identical to the plain one at no cost.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> detail: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var detail_slope: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var detail_sampler: sampler;

// Metres of rise per metre of run a fully-saturated texel stands for. Must equal
// `terrain::DETAIL_SLOPE_RANGE`.
const SLOPE_RANGE: f32 = 1.0;

// Where the box starts fading out, as a fraction of its half-extent. The tier's own
// rim blend (`TerrainSnapshot::rim_weight`) does the same job for point queries; a
// hard edge here would draw a square of texture around the camera.
const RIM_START: f32 = 0.80;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color =
        alpha_discard(pbr_input.material, pbr_input.material.base_color);

    if detail.w > 0.0 {
        let d = (in.world_position.xz - detail.xy) * detail.z;
        let rim = max(abs(d.x), abs(d.y));
        let w = detail.w * (1.0 - smoothstep(RIM_START, 1.0, rim));
        if w > 0.0 {
            let uv = d * 0.5 + vec2<f32>(0.5, 0.5);
            let s = textureSampleLevel(detail_slope, detail_sampler, uv, 0.0).xy
                * SLOPE_RANGE
                * w;
            // The ground's own normal already carries the coarse slope, and the
            // texture is the residual, so this is an addition rather than a
            // replacement — `water.wgsl` replaces because its surface is flat by
            // construction and this one is not.
            let n = normalize(pbr_input.N + vec3<f32>(-s.x, 0.0, -s.y));
            pbr_input.N = n;
            pbr_input.world_normal = n;
        }
    }

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
