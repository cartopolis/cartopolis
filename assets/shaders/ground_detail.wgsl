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

//
// # The map's water is lit as water, not painted
//
// Past the streamed ring the only water is the clipmap tile's own pixels, and the
// painted colour there used to be matched to the lit surface inside the ring by
// hand (`WATER_ALBEDO`, `palette::WATER`) — at one time of day, and drifting at
// every other. The tile now carries **which texels are water** in its alpha
// (`tile_loader::TilePayload::decode`, 255 = land, so an image source with no mask
// reads as land and is untouched), and those texels take the surface's deep body
// colour, its far-field roughness and its reflectance from `water_body` /
// `water_sea`, then the same fresnel and stand-in sky out of `water_common`. Past
// the ring the surface has already retired every wave it can resolve, so a flat
// sheet with that roughness *is* what it would draw there.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings as view_bindings,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
}
#import cartopolis::water_common::{water_reflected, water_transmitted, sky_fallback}

// x, z: the detail box's centre in render-space XZ. z: 1 / its half-extent, so
// `(world_position.xz - centre) * inv_half` is −1..1 across it. w: strength, 0 when
// there is no tier to read (the layer off, outside the Netherlands, still fetching),
// which is what makes this shader identical to the plain one at no cost.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> detail: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var detail_slope: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var detail_sampler: sampler;

// The clipmap level's ring, for the cross-fade below: xy its centre in render-space
// XZ, z 1 / its half-extent, w the mip bias to reach at its edge. Zero on every
// ground surface that is not a clipmap tile, which is the off switch.
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var<uniform> lod: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var lod_basemap: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var lod_basemap_sampler: sampler;

// The water surface's far field, for a clipmap tile's water texels
// (`water::WaterSeaState::far_water`). rgb: the deep body colour — the surface's
// albedo less what depth takes out of it, linear. w: its `reflectance`.
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var<uniform> water_body: vec4<f32>;
// x: perceptual roughness with every wave retired. y: how clear the sky is (1 =
// cloudless), for the stand-in sky. z: 1 on a clipmap tile, 0 everywhere else —
// the off switch, since the vector fills and the roads share this material and
// carry no mask. w unused.
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var<uniform> water_sea: vec4<f32>;

// Metres of rise per metre of run a fully-saturated texel stands for. Must equal
// `terrain::DETAIL_SLOPE_RANGE`.
const SLOPE_RANGE: f32 = 1.0;

// Where the box starts fading out, as a fraction of its half-extent. The tier's own
// rim blend (`TerrainSnapshot::rim_weight`) does the same job for point queries; a
// hard edge here would draw a square of texture around the camera.
const RIM_START: f32 = 0.80;

// Where the level-to-level cross-fade starts, as a fraction of the ring's
// half-extent. 0.65 is a little over the outermost ring and a half of tiles at the
// finest level's radius of 4 — wide enough that the softening reads as distance
// rather than as a second edge just inside the first, narrow enough that most of the
// ring keeps the sharpness it was rasterised at.
const LOD_BLEND_START: f32 = 0.65;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    // How much of this texel is water, read out of the tile's alpha **before**
    // `alpha_discard` flattens it to 1 for an opaque material. 255 is land, so a
    // mask-less image and every surface that is not a tile answer 0.
    var water = 0.0;
    if water_sea.z > 0.0 {
        water = 1.0 - pbr_input.material.base_color.a;
    }
    pbr_input.material.base_color =
        alpha_discard(pbr_input.material, pbr_input.material.base_color);

    // A clipmap level meets the coarser one beneath it along the edge of its own
    // ring, and across that edge the texel density halves in one step — a hard square
    // line of sharpness drawn across the ground, which no amount of filtering removes
    // because it is a difference in the *source*, not in the sampling rate.
    //
    // The levels above `vector_tiles::MAX_SOURCE_ZOOM` are magnified renders of the
    // same z14 body, so a child's own mip 1 is, texel for texel, its parent's
    // density. Ramping a mip bias to 1.0 across the outer band of the ring therefore
    // lands the child's edge on exactly the sharpness the parent carries just beyond
    // it, and the step becomes a gradient. Nothing is blended between two textures and
    // nothing is made transparent: the tile stays one opaque quad, sampled softer
    // towards its rim.
    //
    // The branch is on a uniform-buffer value, so the sample below stays in uniform
    // control flow — and every ground surface that is not a clipmap tile carries
    // `lod.w == 0` and pays one comparison.
    //
    // **`VERTEX_UVS_A` is not optional here.** `VertexOutput::uv` exists only when
    // the mesh carries UV0, and several groups that share this material do not —
    // `tessellate_surfaces` leaves `uvs` empty for the flat colour fills. Reading
    // `in.uv` unguarded does not fail on the tiles, which have it; it fails to
    // compile the *specialisation* the fills need, which is a pipeline that never
    // builds and a shader error at the first draw of a grass polygon.
#ifdef VERTEX_UVS_A
    if lod.w > 0.0 {
        let ring = (in.world_position.xz - lod.xy) * lod.z;
        // Chebyshev, because the ring is a square of tiles and not a disc: this is
        // exactly 1.0 along the whole outer boundary, which is where the parent
        // takes over.
        let edge = max(abs(ring.x), abs(ring.y));
        let t = smoothstep(LOD_BLEND_START, 1.0, edge);
        let soft = textureSampleBias(lod_basemap, lod_basemap_sampler, in.uv, lod.w * t);
        // `mix`, not an assignment: at `t = 0` the inner ring must be bit-for-bit
        // what it was, and this fetch and bevy's own are not obliged to agree in the
        // last bit. Only the colour is taken — `alpha_discard` has already run
        // against the alpha above, and re-deciding it here from a softer sample
        // would move a cutout that has already been made.
        pbr_input.material.base_color = vec4<f32>(
            mix(pbr_input.material.base_color.rgb, soft.rgb, t),
            pbr_input.material.base_color.a,
        );
    }
#endif

    if detail.w > 0.0 {
        let d = (in.world_position.xz - detail.xy) * detail.z;
        let rim = max(abs(d.x), abs(d.y));
        // Water is level; the ground's kerbs and ruts are not in it.
        let w = detail.w * (1.0 - smoothstep(RIM_START, 1.0, rim)) * (1.0 - water);
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

    // The water texels become the surface's far field: its deep body colour
    // behind the same fresnel, its roughness with every wave retired, its
    // reflectance. Blended by the mask, so a coastline's mip-softened edge is a
    // blend rather than a stair.
    var reflected = 0.0;
    if water > 0.0 {
        // **Water is level, whatever the tile under it is doing.** A relief tile's
        // normal follows the terrain field at its own quad size, and beside a dike
        // that is a slope spread over tens of metres of open water. Matte paint
        // never showed it; a glossy surface reflecting the sky turned it into a
        // dark band on one side of the Afsluitdijk and a bright one on the other
        // (Deck, 2026-09-26). The mask-weighted blend keeps a coastline's land
        // half on the ground's own normal.
        let up = normalize(mix(pbr_input.N, vec3<f32>(0.0, 1.0, 0.0), water));
        pbr_input.N = up;
        pbr_input.world_normal = up;
        reflected = water_reflected(dot(pbr_input.N, pbr_input.V));
        pbr_input.material.base_color = vec4<f32>(
            mix(
                pbr_input.material.base_color.rgb,
                water_body.rgb * water_transmitted(reflected),
                water,
            ),
            pbr_input.material.base_color.a,
        );
        pbr_input.material.perceptual_roughness =
            mix(pbr_input.material.perceptual_roughness, water_sea.x, water);
        pbr_input.material.reflectance =
            mix(pbr_input.material.reflectance, vec3<f32>(water_body.w), water);
    }

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);

#ifndef ENVIRONMENT_MAP
#ifdef DISTANCE_FOG
    // The same stand-in sky the surface reflects where there is no environment
    // map, weighted by the mask; the land part of a coastal texel keeps all of
    // bevy's ambient.
    if water > 0.0 {
        out.color = vec4<f32>(
            sky_fallback(
                out.color.rgb,
                pbr_input,
                view_bindings::fog.base_color.rgb,
                water_sea.y,
                reflect(-pbr_input.V, pbr_input.N).y,
                reflected,
                water,
                1.0 - water,
            ),
            out.color.a,
        );
    }
#endif
#endif

    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
