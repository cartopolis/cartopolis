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
#import cartopolis::water_common::{
    water_reflected, water_transmitted, sky_fallback, with_clouds, cloud_mirror_fade,
}
#import cartopolis::cloud_field as cf

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
// The ground cloud deck (`sky::CLOUD_FIELD_BUFFER`), which the water texels mirror
// exactly as the streamed surface inside the ring does. Bound on every ground
// material and read only under a water mask; a field at a time (see `cf::Deck`).
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var<storage, read> clouds: cf::Deck;

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

// ─── Paving ──────────────────────────────────────────────────────────────────
//
// What a surveyed surface is made of, drawn at the scale of its stones: the
// pattern number arrives in the vertex alpha (`paving::pattern`), the stones
// are laid on a lattice in world metres, and every pattern converges on its own
// mean colour below a pixel. Nothing is stored. The lattices divide the
// floating origin's 10,000 m jump (0.1, 0.125, 0.3125, 2.5 m all do), and the
// per-stone hash repeats every 80 stones, which divides each jump's index step
// — so the pavement is the same pavement either side of one.

struct PavingLook {
    tone: vec3<f32>,
    tilt: vec2<f32>,
    roughness: f32,
}

fn pav_hash(i: vec2<f32>) -> f32 {
    let w = i - floor(i / 80.0) * 80.0;
    return fract(sin(dot(w, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

fn pav_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(pav_hash(i), pav_hash(i + vec2<f32>(1.0, 0.0)), u.x),
        mix(pav_hash(i + vec2<f32>(0.0, 1.0)), pav_hash(i + vec2<f32>(1.0, 1.0)), u.x),
        u.y,
    );
}

/// How far below a pixel a lattice of `unit` metres is, as 0 (crisp) to 1 (gone).
fn pav_blur(unit: f32, metres_per_px: f32) -> f32 {
    return clamp(metres_per_px / unit * 3.0 - 0.3, 0.0, 1.0);
}

/// A stone's face: darker in its joints, its own tint, and a slightly domed top
/// — `f` is where in the stone this is (0–1 both ways), `edge` how far from
/// its nearest joint as a share of the stone, `joint` the joint's share.
fn stone(id: f32, f: vec2<f32>, edge: f32, joint: f32, spread: f32, blur: f32, mean: f32) -> PavingLook {
    let crisp = 1.0 - blur;
    let in_joint = 1.0 - smoothstep(joint * 0.5, joint, edge);
    let tint = 1.0 + spread * (id - 0.5);
    let tone = mix(mix(tint, 0.62, in_joint), mean, blur);
    var look: PavingLook;
    look.tone = vec3<f32>(tone);
    look.tilt = (f - vec2<f32>(0.5, 0.5)) * 0.35 * crisp * (1.0 - in_joint);
    look.roughness = 0.85;
    return look;
}

/// `paving::pattern`: 1 asphalt, 2 concrete slabs, 3 brick in herringbone,
/// 4 concrete pavers in a stretcher bond, 5 pavement tiles, 6 stone setts,
/// 7 grass pavers, 8 gravel and sand.
fn paving_look(pattern: u32, p: vec2<f32>, metres_per_px: f32) -> PavingLook {
    var look: PavingLook;
    look.tone = vec3<f32>(1.0);
    look.tilt = vec2<f32>(0.0);
    look.roughness = 0.9;
    switch pattern {
        case 1u: {
            // Asphalt: a fine aggregate, and the patches of a road resurfaced
            // piece by piece.
            let grain = (pav_noise(p / 0.025) - 0.5) * (1.0 - pav_blur(0.025, metres_per_px));
            let patches = pav_noise(p / 2.44) - 0.5;
            look.tone = vec3<f32>(1.0 + 0.16 * grain + 0.12 * patches);
        }
        case 2u: {
            // Poured concrete in 2.5 m slabs with sawn joints.
            let q = p / 2.5;
            let f = fract(q);
            let edge = min(min(f.x, 1.0 - f.x), min(f.y, 1.0 - f.y));
            look = stone(pav_hash(floor(q)), f, edge, 0.006, 0.1, pav_blur(2.5, metres_per_px), 0.99);
            look.tilt = vec2<f32>(0.0);
            look.tone *= 1.0 + 0.06 * (pav_noise(p / 0.7) - 0.5);
        }
        case 3u: {
            // Clay brick in a 90° herringbone, 20 × 10 cm. Each 10 cm square is
            // half a brick: with d = (x + y) mod 4, d = 0/1 are the left/right of
            // a brick lying along x, d = 2/3 the bottom/top of one along z —
            // which tiles the plane, and is the bond most Dutch brick streets
            // are laid in.
            let q = p / 0.1;
            let c = floor(q);
            let f = fract(q);
            let d = u32(c.x + c.y - 4.0 * floor((c.x + c.y) / 4.0));
            var id = c;
            var edge = 1.0;
            var g = f;
            switch d {
                case 0u: { edge = min(f.x, min(f.y, 1.0 - f.y)); g = vec2<f32>(f.x * 0.5, f.y); }
                case 1u: { id = c - vec2<f32>(1.0, 0.0); edge = min(1.0 - f.x, min(f.y, 1.0 - f.y)); g = vec2<f32>(0.5 + f.x * 0.5, f.y); }
                case 2u: { edge = min(f.y, min(f.x, 1.0 - f.x)); g = vec2<f32>(f.x, f.y * 0.5); }
                default: { id = c - vec2<f32>(0.0, 1.0); edge = min(1.0 - f.y, min(f.x, 1.0 - f.x)); g = vec2<f32>(f.x, 0.5 + f.y * 0.5); }
            }
            look = stone(pav_hash(id), g, edge, 0.08, 0.32, pav_blur(0.1, metres_per_px), 0.96);
            // Clay is not one colour: some bricks fire browner, some purpler.
            let hue = pav_hash(id + vec2<f32>(17.0, 3.0)) - 0.5;
            look.tone *= vec3<f32>(1.0 + 0.1 * hue, 1.0, 1.0 - 0.12 * hue);
        }
        case 4u: {
            // Concrete pavers, 20 × 10 cm, each course half a stone over.
            let course = floor(p.y / 0.1);
            let q = vec2<f32>(p.x / 0.2 + 0.5 * course, p.y / 0.1);
            let f = fract(q);
            let edge = min(min(f.x, 1.0 - f.x) * 2.0, min(f.y, 1.0 - f.y));
            look = stone(pav_hash(floor(q)), f, edge, 0.08, 0.14, pav_blur(0.1, metres_per_px), 0.97);
        }
        case 5u: {
            // Pavement tiles, 30 × 30 cm, butted close.
            let q = p / 0.3125;
            let f = fract(q);
            let edge = min(min(f.x, 1.0 - f.x), min(f.y, 1.0 - f.y));
            look = stone(pav_hash(floor(q)), f, edge, 0.05, 0.2, pav_blur(0.3125, metres_per_px), 0.97);
            // Pavement tiles weather: a faint stain here and there, larger than a tile.
            look.tone *= 1.0 + 0.1 * (pav_noise(p / 1.7) - 0.5) * (1.0 - pav_blur(1.7, metres_per_px));
            look.tilt *= 0.3;
        }
        case 6u: {
            // Natural stone setts, 12.5 cm, each row shifted by its own amount,
            // wide sandy joints and a strongly varied face.
            let row = floor(p.y / 0.125);
            let shift = pav_hash(vec2<f32>(row, 7.0));
            let q = vec2<f32>(p.x / 0.125 + shift, p.y / 0.125);
            let f = fract(q);
            let edge = min(min(f.x, 1.0 - f.x), min(f.y, 1.0 - f.y));
            look = stone(pav_hash(floor(q)), f, edge, 0.14, 0.4, pav_blur(0.125, metres_per_px), 0.93);
            look.roughness = 0.75;
        }
        case 7u: {
            // Grass pavers: a 40 cm concrete grid with grass in its cells.
            let f = fract(p / 0.4);
            let hole = f32(abs(f.x - 0.5) < 0.32 && abs(f.y - 0.5) < 0.32);
            let blur = pav_blur(0.4, metres_per_px);
            let grass = vec3<f32>(0.45, 0.85, 0.35);
            look.tone = mix(mix(vec3<f32>(1.05), grass, hole), mix(vec3<f32>(1.05), grass, 0.4), blur);
        }
        case 8u: {
            // Gravel, shell and sand: loose grains.
            let grain = (pav_noise(p / 0.03) - 0.5) * (1.0 - pav_blur(0.03, metres_per_px));
            look.tone = vec3<f32>(1.0 + 0.3 * grain + 0.08 * (pav_noise(p / 1.5) - 0.5));
        }
        default: {}
    }
    return look;
}

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
    // …and on a surveyed surface the same alpha carries what it is paved in
    // (`paving::pattern`, as `1 + pattern`): 1.0, which every other surface
    // carries, is none. Read before `alpha_discard` for the same reason.
    let paving_code = select(0.0, pbr_input.material.base_color.a - 1.0, water_sea.z <= 0.0);
    // How many metres one pixel spans here — taken outside any branch, since a
    // derivative in non-uniform control flow is undefined.
    let metres_per_px = max(length(dpdx(in.world_position.xz)), length(dpdy(in.world_position.xz)));
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

    if paving_code > 0.5 {
        let look = paving_look(u32(round(paving_code)), in.world_position.xz, metres_per_px);
        pbr_input.material.base_color = vec4<f32>(
            pbr_input.material.base_color.rgb * look.tone,
            pbr_input.material.base_color.a,
        );
        let n = normalize(pbr_input.N + vec3<f32>(look.tilt.x, 0.0, look.tilt.y));
        pbr_input.N = n;
        pbr_input.world_normal = n;
        pbr_input.material.perceptual_roughness = look.roughness;
    }

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

    // And the clouds in it, as the surface inside the ring mirrors them
    // (`water.wgsl`): without this a lake's clouds would stop at the ring's edge.
    // The pixel's footprint on the deck is taken out here, for every ground pixel,
    // because a derivative is not allowed inside the water-only branch — the mask
    // varies from texel to texel. It is a handful of arithmetic and one load.
    let offset = in.world_position.xyz - view_bindings::view.world_position.xyz;
    let mirror_ray = reflect(-pbr_input.V, pbr_input.N);
    let spread = length(fwidth(cf::mirror_hit(clouds.shape.w, offset, mirror_ray)));
    // The same cut-off the surface applies, to the same weight, so it cannot
    // draw a line along the ring's edge.
    let cloud_fade = cloud_mirror_fade(reflected * water);
    if cloud_fade > 0.0 {
        let deck = cf::Deck(
            clouds.shape,
            clouds.motion,
            clouds.sun_dir,
            clouds.sun_color,
            clouds.sky_color,
            clouds.place,
            clouds.haze_color,
            clouds.light,
        );
        var cloud = cf::mirrored(
            deck,
            offset,
            mirror_ray,
            length(offset),
            pbr_input.material.perceptual_roughness,
            spread,
        );
        cloud.a *= cloud_fade;
        out.color = vec4<f32>(
            with_clouds(out.color.rgb, pbr_input, cloud, water_sea.y, reflected, water),
            out.color.a,
        );
    }

    out.color = main_pass_post_lighting_processing(pbr_input, out.color);

    // The underground layer looks through the ground (`systems::sewers`). A
    // screen door, not a blend: the ground stays opaque and keeps writing depth,
    // so the ground stack's depth ladder is untouched and nothing has to be
    // sorted — a pixel is either ground or a hole. Every sheet of the stack draws
    // the same pattern at the same pixels, so a hole goes through all of them at
    // once rather than opening onto the clipmap level underneath.
    //
    // At the very end, after every derivative and texture sample, because a
    // `discard` before them would put them in non-uniform control flow. Only in
    // this variant (`ground_detail::GroundKey`): the statement's mere presence
    // costs early depth testing.
#ifdef GROUND_SEE_THROUGH
    let horizontal = length(in.world_position.xz - view_bindings::view.world_position.xz);
    let open = SEE_THROUGH * (1.0 - smoothstep(SEE_THROUGH_NEAR_M, SEE_THROUGH_FAR_M, horizontal));
    let cell = vec2<u32>(in.position.xy) % vec2<u32>(4u);
    if bayer4(cell.x, cell.y) < open {
        discard;
    }
#endif
    return out;
}

#ifdef GROUND_SEE_THROUGH
// How much of the ground opens near the camera. Over half, because the
// pipes are the point; under all of it, because a street you cannot see is
// a street you cannot place them in.
const SEE_THROUGH: f32 = 0.6;
// Horizontal metres from the camera over which the ground closes again, so
// the far city reads as a city and the opening reads as looking down.
const SEE_THROUGH_NEAR_M: f32 = 250.0;
const SEE_THROUGH_FAR_M: f32 = 450.0;

// A 4×4 ordered-dither threshold in [0, 1): the pattern with the most even
// spread of holes at every density.
fn bayer4(x: u32, y: u32) -> f32 {
    var m = array<f32, 16>(
        0.0, 8.0, 2.0, 10.0,
        12.0, 4.0, 14.0, 6.0,
        3.0, 11.0, 1.0, 9.0,
        15.0, 7.0, 13.0, 5.0,
    );
    return (m[y * 4u + x] + 0.5) / 16.0;
}
#endif
