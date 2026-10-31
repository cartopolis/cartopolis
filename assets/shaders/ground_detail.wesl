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

import bevy_pbr::render::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
};
import bevy_pbr::render::mesh_view_bindings as view_bindings;
import super::water_common::{
    water_reflected, water_transmitted, sky_fallback, with_clouds, cloud_mirror_fade,
};
import super::cloud_field as cf;

import bevy_pbr::render::mesh_bindings::mesh;
@if(BINDLESS)
import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d};

// `ground_clouds` below is a `binding_array`; bevy's own bindless modules
// enable the extension the same way.
@if(BINDLESS)
enable wgpu_binding_array;

// The material's four vectors, one buffer (`ground_detail::GroundUniform`):
//
// detail — x, z: the detail box's centre in render-space XZ. z: 1 / its
//   half-extent, so `(world_position.xz - centre) * inv_half` is −1..1 across it.
//   w: strength, 0 when there is no tier to read (the layer off, outside the
//   Netherlands, still fetching), which is what makes this shader identical to the
//   plain one at no cost.
// lod — the clipmap level's ring, for the cross-fade below: xy its centre in
//   render-space XZ, z 1 / its half-extent, w the mip bias to reach at its edge.
//   Zero on every ground surface that is not a clipmap tile, which is the off
//   switch.
// water_body — the water surface's far field, for a clipmap tile's water texels
//   (`water::WaterSeaState::far_water`). rgb: the deep body colour — the surface's
//   albedo less what depth takes out of it, linear. w: its `reflectance`.
// water_sea — x: perceptual roughness with every wave retired. y: how clear the
//   sky is (1 = cloudless), for the stand-in sky. z: 1 on a clipmap tile, 0
//   everywhere else — the off switch, since the vector fills and the roads share
//   this material and carry no mask. w: 1 on a road ribbon (`RoadDepthExtension`
//   sets it), 0 on every ground surface.
// far — the cross-fade to the painted map: x the camera distance it starts at,
//   y where it is complete, metres; z 1 when the painted twin (106) is bound.
struct GroundUniform {
    detail: vec4<f32>,
    lod: vec4<f32>,
    water_body: vec4<f32>,
    water_sea: vec4<f32>,
    far: vec4<f32>,
}

// Bindless where the platform has it, which is what lets every clipmap tile —
// each a material of its own, with its own basemap — share one bind group and be
// batched into one draw (`ground_detail::GroundUniform`). The same resources
// either way: the uniforms, the slope tier (`detail_slope`), the tile's own
// basemap bound a second time for the cross-fade (`lod_basemap`), and the ground
// cloud deck (`sky::CLOUD_FIELD_BUFFER`) the water texels mirror exactly as the
// streamed surface inside the ring does.
// One entry per material, field i naming bindless index 100 + i.
@if(BINDLESS)
struct GroundIndices {
    material: u32,        // 100
    slope: u32,           // 101
    slope_sampler: u32,   // 102
    basemap: u32,         // 103
    basemap_sampler: u32, // 104
    clouds: u32,          // 105
    painted: u32,         // 106
    painted_sampler: u32, // 107
}
@if(BINDLESS) {
@group(constants::MATERIAL_BIND_GROUP) @binding(109) var<storage> ground_indices: array<GroundIndices>;
@group(constants::MATERIAL_BIND_GROUP) @binding(110) var<storage> ground_array: array<GroundUniform>;
@group(constants::MATERIAL_BIND_GROUP) @binding(111) var<storage> ground_clouds: binding_array<cf::Deck>;
}
@else {
@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> ground_single: GroundUniform;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var detail_slope: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var detail_sampler: sampler;
@group(constants::MATERIAL_BIND_GROUP) @binding(103) var lod_basemap: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(104) var lod_basemap_sampler: sampler;
@group(constants::MATERIAL_BIND_GROUP) @binding(105) var<storage, read> clouds_single: cf::Deck;
@group(constants::MATERIAL_BIND_GROUP) @binding(106) var painted_basemap: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(107) var painted_sampler: sampler;
}

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
    /// How much of this point is the joint and the worn edges either side of
    /// it, where the dirt collects; faded with the stones, 0 with no stones.
    joint: f32,
    /// 1 − the stones' blur, and 0 on a pattern with no stones — which is what
    /// says `lived_in` applies.
    crisp: f32,
    /// What fills the joints, as a colour of its own (rgb, linear) laid over the
    /// stone by `a` — sand on red brick is sand-coloured, not a darker red.
    fill: vec4<f32>,
}

/// How strongly asphalt's aggregate tilts the normal, for the surveyed asphalt
/// and the road ribbons alike, so the two read as one material where they meet.
const ASPHALT_RELIEF: f32 = 0.6;

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

/// A loose or bound aggregate in relief: the slope of the same grain noise the
/// tone draws at `unit` metres, as a tilt — so a low sun picks the stones out
/// of a carriageway rather than only their colour. Gone below a pixel with the
/// grain itself, since a slope that survives there is a sparkle.
fn grain_tilt(p: vec2<f32>, unit: f32, strength: f32, metres_per_px: f32) -> vec2<f32> {
    let k = 1.0 - pav_blur(unit, metres_per_px);
    if k <= 0.0 {
        return vec2<f32>(0.0, 0.0);
    }
    let q = p / unit;
    let h0 = pav_noise(q);
    let slope = vec2<f32>(pav_noise(q + vec2<f32>(0.25, 0.0)) - h0, pav_noise(q + vec2<f32>(0.0, 0.25)) - h0);
    return -slope * strength * k;
}

/// How much of a stone's edge bevel one point is on: 0 on the flat and in the
/// joint, rising to 1 halfway down the chamfer — a parabola over `[a, a + w]`
/// metres from the joint's centre, so the tilt has no step at either end.
fn chamfer(d: f32, a: f32, w: f32) -> f32 {
    let t = clamp((d - a) / w, 0.0, 1.0);
    return 4.0 * t * (1.0 - t);
}

/// Bevelled edges are what a laid stone is up close: the face is flat and its
/// edges roll down into the joint, so under a low sun every stone carries a lit
/// edge and a dark one. Each axis tilts away from its nearer side, on a chamfer
/// `BEVEL_M` wide starting at the joint's edge.
const BEVEL_M: f32 = 0.012;

/// A stone's face: darker in its joints, its own tint, a faintly domed top and
/// bevelled edges — `f` is where in the stone this is (0–1 both ways), `size`
/// the stone in metres along those two axes, `edge` how far from its nearest
/// joint as a share of the stone, `joint` the joint's share.
fn stone(id: f32, f: vec2<f32>, size: vec2<f32>, edge: f32, joint: f32, spread: f32, blur: f32, mean: f32) -> PavingLook {
    let crisp = 1.0 - blur;
    let in_joint = 1.0 - smoothstep(joint * 0.5, joint, edge);
    let tint = 1.0 + spread * (id - 0.5);
    let tone = mix(mix(tint, 0.62, in_joint), mean, blur);
    var look: PavingLook;
    look.tone = vec3<f32>(tone);
    // The bevel is a centimetre, so it goes well before the stone does: gone
    // once a stone is ~6 px, where it would only be a crawling edge.
    let bevel_k = clamp(1.0 - blur * 6.0, 0.0, 1.0);
    let half_joint = joint * 0.75 * min(size.x, size.y);
    let d = min(f, vec2<f32>(1.0, 1.0) - f) * size;
    let away = select(vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, -1.0), f < vec2<f32>(0.5, 0.5));
    let bevel = away * vec2<f32>(chamfer(d.x, half_joint, BEVEL_M), chamfer(d.y, half_joint, BEVEL_M));
    // No two stones were laid alike: each sits a little off level, rocked by
    // the bed settling under it, and each has its edges worn by a different
    // amount — a crisp new one beside a rounded old one.
    let rock = (vec2<f32>(fract(id * 7.31), fract(id * 3.17)) - vec2<f32>(0.5, 0.5)) * 0.1;
    let worn = mix(0.35, 1.2, fract(id * 5.71));
    look.tilt = ((f - vec2<f32>(0.5, 0.5)) * 0.12 + rock) * crisp * (1.0 - in_joint)
        + bevel * 0.8 * worn * bevel_k;
    look.roughness = 0.85;
    look.joint = (1.0 - smoothstep(joint * 0.5, joint * 2.5, edge)) * crisp;
    look.crisp = crisp;
    return look;
}

/// What a street does to its stones once they are laid — they are not as clean
/// as the pattern draws them. Stains a metre across that darken more than they
/// lighten, joints filled with sand and grit (and moss where nobody walks) that
/// take the colour of what is in them and soften the edges they fill, and on a
/// footpath the odd flattened disc of chewing gum.
///
/// Every lattice here divides the floating origin's 10,000 m jump into a
/// multiple of `pav_hash`'s 80 (1.0, 0.625, 1.25, 0.3125 and 0.25 m all do), so
/// the dirt stays where it was across one. Each term fades on its own scale.
/// Joint sand and the moss that takes over a quiet joint, linear.
const SAND: vec3<f32> = vec3<f32>(0.30, 0.26, 0.19);
const MOSS: vec3<f32> = vec3<f32>(0.10, 0.14, 0.05);

fn lived_in(look_in: PavingLook, pattern: u32, p: vec2<f32>, metres_per_px: f32, fresh: f32) -> PavingLook {
    var look = look_in;
    // A relaid patch has not had the years to collect it yet.
    let stain_fade = (1.0 - pav_blur(1.0, metres_per_px)) * (1.0 - 0.75 * fresh);
    let blotch = smoothstep(0.55, 0.85, pav_noise(p + vec2<f32>(31.0, 17.0)));
    let mottle = pav_noise(p / 0.3125) - 0.5;
    // 0.15 is about the blotch's mean, so the stains darken a street by a few
    // percent before they fade into its measured colour. Dirt is grey-green.
    // Half what it was once the stones stopped fading three metres out: seen
    // across a whole square, a metre blotch at 35 % read as spilt oil.
    let grime = (0.15 * mottle - 0.18 * (blotch - 0.15)) * stain_fade;
    look.tone *= (1.0 + grime) * mix(vec3<f32>(1.0), vec3<f32>(0.94, 0.96, 0.92), blotch * stain_fade);

    let fill = pav_noise(p / 0.3125 + vec2<f32>(5.0, 71.0));
    let moss = smoothstep(0.6, 0.8, pav_noise(p / 1.25 + vec2<f32>(13.0, 3.0)));
    let grit = mix(SAND, MOSS, moss) * (0.8 + 0.4 * fill);
    look.fill = vec4<f32>(grit, look.joint * mix(0.35, 0.85, fill) * (1.0 - 0.6 * fresh));
    look.tilt *= mix(1.0, 0.35, smoothstep(0.45, 0.8, fill));

    if pattern == 4u || pattern == 5u {
        let cell = floor(p / 0.25);
        let h = pav_hash(cell + vec2<f32>(91.0, 47.0));
        if h > 0.96 {
            let centre = (cell + vec2<f32>(0.2, 0.2) + 0.6 * vec2<f32>(fract(h * 13.7), fract(h * 31.1))) * 0.25;
            let r = 0.008 + 0.01 * fract(h * 7.9);
            let gum = 1.0 - smoothstep(r * 0.7, r, length(p - centre));
            look.tone *= 1.0 - 0.45 * gum * (1.0 - pav_blur(0.02, metres_per_px));
        }
    }
    return look;
}

/// The street at the scale it is walked rather than the scale of its stones.
/// Every stone above is varied and every stain is a metre across, so a square
/// a hundred metres wide was still one carpet: nothing in it changed over the
/// ten metres between one lamppost and the next. A real one is lighter here and
/// duller there, and has been dug up and relaid in places — a trench, a cable,
/// a drain — where the stones are of another batch and have not yet taken on
/// the street's dirt, or, in asphalt, a darker, smoother patch with a seam of
/// sealant round it.
///
/// All of it averages out — the clouds are zero-mean, the patches a fifth of
/// the cells and a few per cent of the ground — so where `FLAT_GROUND_OFF_M`
/// hands the ground to the basemap, which paints each pattern's mean
/// (`paving::pattern_mean`), the colour does not step, only the mottling goes.
/// It fades on a pixel's *sharp* side, between one and three metres, because a
/// blotch five metres wide is still five metres across a grazing view. The lattices (12.5, 6.25, 5 and
/// 3.125 m) divide 125, so with `pav_hash`'s 80 they divide the floating
/// origin's 10,000 m jump like the stones' do.
struct StreetScale {
    /// Multiplies the surface's colour.
    tone: vec3<f32>,
    /// Multiplies its roughness.
    roughness: f32,
    /// 1 inside a relaid patch, 0 elsewhere and on patterns that are not laid.
    fresh: f32,
}

/// The street-scale variation at `p`; `laid` is 0 for no patches (loose
/// ground, a road ribbon), 1 for laid stones, 2 for asphalt.
fn street_scale(p: vec2<f32>, laid: u32, metres_per_px: f32, sharpest_px: f32) -> StreetScale {
    var s: StreetScale;
    let k = 1.0 - smoothstep(1.0, 3.0, sharpest_px);
    let broad = pav_noise(p / 12.5 + vec2<f32>(3.0, 61.0)) - 0.5;
    let near = pav_noise(p / 5.0 + vec2<f32>(47.0, 11.0)) - 0.5;
    let warm = pav_noise(p / 12.5 + vec2<f32>(29.0, 5.0)) - 0.5;
    let v = (0.45 * broad + 0.22 * near) * k;
    s.tone = vec3<f32>(1.0 + v) * (vec3<f32>(1.0) + vec3<f32>(0.08, 0.0, -0.08) * warm * k);
    s.roughness = 1.0 + 0.16 * (pav_noise(p / 3.125 + vec2<f32>(7.0, 41.0)) - 0.5) * k;
    s.fresh = 0.0;
    if laid == 0u || k <= 0.0 {
        return s;
    }
    let cell = floor(p / 6.25);
    let h = pav_hash(cell + vec2<f32>(53.0, 29.0));
    if h > 0.8 {
        let f = fract(p / 6.25);
        let lo = vec2<f32>(0.05 + 0.3 * fract(h * 11.3), 0.05 + 0.3 * fract(h * 17.1));
        let hi = vec2<f32>(0.6 + 0.35 * fract(h * 23.9), 0.6 + 0.35 * fract(h * 5.3));
        // Metres inside the patch's nearest edge: negative outside it.
        let d = min(f - lo, hi - f) * 6.25;
        let inside_m = min(d.x, d.y);
        let aa = max(metres_per_px, 0.002);
        let fresh = smoothstep(-aa, aa, inside_m) * k;
        s.fresh = fresh;
        let batch = fract(h * 41.7) - 0.5;
        if laid == 1u {
            // Another batch of stone: a shade off in hue, and a little lighter
            // for being clean.
            s.tone *= mix(vec3<f32>(1.0), vec3<f32>(1.14 + 0.25 * batch, 1.14 + 0.08 * batch, 1.14 - 0.2 * batch), fresh);
        } else {
            // Fresh asphalt is darker and closer-textured than the worn road
            // round it, and the joint is sealed with a strip of bitumen.
            let seam = (1.0 - smoothstep(0.03, 0.03 + aa, abs(inside_m))) * k;
            s.tone *= mix(vec3<f32>(1.0), vec3<f32>(0.8), fresh) * (1.0 - 0.45 * seam);
            s.roughness *= mix(1.0, 0.85, fresh);
        }
    }
    return s;
}

/// `paving::pattern`: 1 asphalt, 2 concrete slabs, 3 brick in herringbone,
/// 4 concrete pavers in a stretcher bond, 5 pavement tiles, 6 stone setts,
/// 7 grass pavers, 8 gravel and sand.
fn paving_look(pattern: u32, p: vec2<f32>, metres_per_px: f32, sharpest_px: f32) -> PavingLook {
    var look: PavingLook;
    look.tone = vec3<f32>(1.0);
    look.tilt = vec2<f32>(0.0);
    look.roughness = 0.9;
    let laid = select(select(0u, 1u, pattern >= 2u && pattern <= 6u), 2u, pattern == 1u);
    let street = street_scale(p, laid, metres_per_px, sharpest_px);
    switch pattern {
        case 1u: {
            // Asphalt: a fine aggregate, and the patches of a road resurfaced
            // piece by piece.
            let grain = (pav_noise(p / 0.025) - 0.5) * (1.0 - pav_blur(0.025, metres_per_px));
            let patches = pav_noise(p / 2.44) - 0.5;
            look.tone = vec3<f32>(1.0 + 0.16 * grain + 0.12 * patches);
            look.tilt = grain_tilt(p, 0.025, ASPHALT_RELIEF, metres_per_px);
        }
        case 2u: {
            // Poured concrete in 2.5 m slabs with sawn joints.
            let q = p / 2.5;
            let f = fract(q);
            let edge = min(min(f.x, 1.0 - f.x), min(f.y, 1.0 - f.y));
            look = stone(pav_hash(floor(q)), f, vec2<f32>(2.5, 2.5), edge, 0.006, 0.1, pav_blur(2.5, metres_per_px), 0.99);
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
            look = stone(pav_hash(id), g, select(vec2<f32>(0.1, 0.2), vec2<f32>(0.2, 0.1), d < 2u), edge, 0.08, 0.32, pav_blur(0.1, metres_per_px), 0.96);
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
            look = stone(pav_hash(floor(q)), f, vec2<f32>(0.2, 0.1), edge, 0.08, 0.14, pav_blur(0.1, metres_per_px), 0.97);
        }
        case 5u: {
            // Pavement tiles, 30 × 30 cm, butted close.
            let q = p / 0.3125;
            let f = fract(q);
            let edge = min(min(f.x, 1.0 - f.x), min(f.y, 1.0 - f.y));
            look = stone(pav_hash(floor(q)), f, vec2<f32>(0.3125, 0.3125), edge, 0.05, 0.2, pav_blur(0.3125, metres_per_px), 0.97);
            // Pavement tiles weather: a faint stain here and there, larger than a tile.
            look.tone *= 1.0 + 0.1 * (pav_noise(p / 1.7) - 0.5) * (1.0 - pav_blur(1.7, metres_per_px));
            look.tilt *= 0.5;
        }
        case 6u: {
            // Natural stone setts, 12.5 cm, each row shifted by its own amount,
            // wide sandy joints and a strongly varied face.
            let row = floor(p.y / 0.125);
            let shift = pav_hash(vec2<f32>(row, 7.0));
            let q = vec2<f32>(p.x / 0.125 + shift, p.y / 0.125);
            let f = fract(q);
            let edge = min(min(f.x, 1.0 - f.x), min(f.y, 1.0 - f.y));
            look = stone(pav_hash(floor(q)), f, vec2<f32>(0.125, 0.125), edge, 0.14, 0.4, pav_blur(0.125, metres_per_px), 0.93);
            look.roughness = 0.75;
            // A sett is hand-dressed stone, rounder than a pressed paver.
            look.tilt += (f - vec2<f32>(0.5, 0.5)) * 0.25 * (1.0 - pav_blur(0.125, metres_per_px));
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
            look.tilt = grain_tilt(p, 0.03, 0.8, metres_per_px);
        }
        default: {}
    }
    if look.crisp > 0.0 {
        look = lived_in(look, pattern, p, metres_per_px, street.fresh);
    }
    look.tone *= street.tone;
    look.roughness *= street.roughness;
    return look;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    @if(BINDLESS)
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
    @if(BINDLESS)
    let ground = ground_array[ground_indices[slot].material];
    @else
    let ground = ground_single;
    let detail = ground.detail;
    let lod = ground.lod;
    let water_body = ground.water_body;
    let water_sea = ground.water_sea;
    let far = ground.far;
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
    //
    // A grazing view smears the ground along the line of sight and not across
    // it: at eye height ten metres off, a pixel is five times longer than it is
    // wide. Fading the stones on the long side alone took them out of the
    // picture three metres from the feet (Grote Markt, 2026-10-29), so they
    // fade on the side's geometric mean — the pixel's area — which keeps them
    // twice as far and still goes before they are small enough to crawl.
    let along_px = max(length(dpdx(in.world_position.xz)), length(dpdy(in.world_position.xz)));
    let sharpest_px = min(length(dpdx(in.world_position.xz)), length(dpdy(in.world_position.xz)));
    let metres_per_px = sqrt(along_px * sharpest_px);
    pbr_input.material.base_color =
        alpha_discard(pbr_input.material.flags, pbr_input.material.alpha_cutoff, pbr_input.material.base_color);

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
    @if(VERTEX_UVS_A)
    if lod.w > 0.0 {
        let ring = (in.world_position.xz - lod.xy) * lod.z;
        // Chebyshev, because the ring is a square of tiles and not a disc: this is
        // exactly 1.0 along the whole outer boundary, which is where the parent
        // takes over.
        let edge = max(abs(ring.x), abs(ring.y));
        let t = smoothstep(LOD_BLEND_START, 1.0, edge);
        @if(BINDLESS)
        let soft = textureSampleBias(
            bindless_textures_2d[ground_indices[slot].basemap],
            bindless_samplers_filtering[ground_indices[slot].basemap_sampler],
            in.uv,
            lod.w * t,
        );
        @else
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

    // From today's ground to the painted map (`vector_tiles::painted`), on distance
    // from the camera and not on its height, so what is near keeps today's style in
    // a tilted view too. A tile in the band where the two meet carries itself drawn
    // a second time in the painted style; every other surface carries `far.z == 0`.
    // Water keeps its mask from the tile's own alpha, read above.
    @if(VERTEX_UVS_A)
    if far.z > 0.0 {
        let d = length(in.world_position.xyz - view_bindings::view.world_position.xyz);
        let t = smoothstep(far.x, far.y, d);
        @if(BINDLESS)
        let painted = textureSample(
            bindless_textures_2d[ground_indices[slot].painted],
            bindless_samplers_filtering[ground_indices[slot].painted_sampler],
            in.uv,
        );
        @else
        let painted = textureSample(painted_basemap, painted_sampler, in.uv);
        pbr_input.material.base_color = vec4<f32>(
            mix(pbr_input.material.base_color.rgb, painted.rgb, t),
            pbr_input.material.base_color.a,
        );
    }

    if paving_code > 0.5 {
        let look = paving_look(u32(round(paving_code)), in.world_position.xz, metres_per_px, sharpest_px);
        pbr_input.material.base_color = vec4<f32>(
            mix(pbr_input.material.base_color.rgb * look.tone, look.fill.rgb, look.fill.a),
            pbr_input.material.base_color.a,
        );
        let n = normalize(pbr_input.N + vec3<f32>(look.tilt.x, 0.0, look.tilt.y));
        pbr_input.N = n;
        pbr_input.world_normal = n;
        pbr_input.material.perceptual_roughness = look.roughness;
    } else if water_sea.w > 0.5 {
        // A road ribbon: its texture carries the markings, the wheel paths and
        // a grain coarser than the stones, so the aggregate's relief is added
        // here exactly as the surveyed asphalt beside it gets it.
        let t = grain_tilt(in.world_position.xz, 0.025, ASPHALT_RELIEF, metres_per_px);
        let n = normalize(pbr_input.N + vec3<f32>(t.x, 0.0, t.y));
        pbr_input.N = n;
        pbr_input.world_normal = n;
        // The same broad variation as the surveyed asphalt beside it, so the two
        // do not change character at the seam; no patches, which would cut
        // across the markings at angles no repair crew would.
        let street = street_scale(in.world_position.xz, 0u, metres_per_px, sharpest_px);
        pbr_input.material.base_color = vec4<f32>(
            pbr_input.material.base_color.rgb * street.tone,
            pbr_input.material.base_color.a,
        );
        pbr_input.material.perceptual_roughness *= street.roughness;
    }

    if detail.w > 0.0 {
        let d = (in.world_position.xz - detail.xy) * detail.z;
        let rim = max(abs(d.x), abs(d.y));
        // Water is level; the ground's kerbs and ruts are not in it.
        let w = detail.w * (1.0 - smoothstep(RIM_START, 1.0, rim)) * (1.0 - water);
        if w > 0.0 {
            let uv = d * 0.5 + vec2<f32>(0.5, 0.5);
            @if(BINDLESS)
            let slope = textureSampleLevel(
                bindless_textures_2d[ground_indices[slot].slope],
                bindless_samplers_filtering[ground_indices[slot].slope_sampler],
                uv,
                0.0,
            );
            @else
            let slope = textureSampleLevel(detail_slope, detail_sampler, uv, 0.0);
            let s = slope.xy * SLOPE_RANGE * w;
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

    // The same stand-in sky the surface reflects where there is no environment
    // map, weighted by the mask; the land part of a coastal texel keeps all of
    // bevy's ambient.
    @if(!ENVIRONMENT_MAP && DISTANCE_FOG)
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

    // And the clouds in it, as the surface inside the ring mirrors them
    // (`water.wgsl`): without this a lake's clouds would stop at the ring's edge.
    // The pixel's footprint on the deck is taken out here, for every ground pixel,
    // because a derivative is not allowed inside the water-only branch — the mask
    // varies from texel to texel. It is a handful of arithmetic and one load.
    // Field by field, never `let clouds = ground_clouds[i]` whole (`cf::Deck`):
    // naga's Metal writer turns a whole load out of a `binding_array` of buffers
    // into a pointer-to-struct conversion Metal refuses, the opaque pipeline fails
    // to build, and the renderer halts on a black window (macOS, 2026-10-15).
    @if(BINDLESS)
    let cloud_slot = ground_indices[slot].clouds;
    @if(BINDLESS)
    let deck_shape = ground_clouds[cloud_slot].shape;
    @else
    let deck_shape = clouds_single.shape;
    let offset = in.world_position.xyz - view_bindings::view.world_position.xyz;
    let mirror_ray = reflect(-pbr_input.V, pbr_input.N);
    let spread = length(fwidth(cf::mirror_hit(deck_shape.w, offset, mirror_ray)));
    // The same cut-off the surface applies, to the same weight, so it cannot
    // draw a line along the ring's edge.
    let cloud_fade = cloud_mirror_fade(reflected * water);
    if cloud_fade > 0.0 {
        @if(BINDLESS)
        let deck = cf::Deck(
            deck_shape,
            ground_clouds[cloud_slot].motion,
            ground_clouds[cloud_slot].sun_dir,
            ground_clouds[cloud_slot].sun_color,
            ground_clouds[cloud_slot].sky_color,
            ground_clouds[cloud_slot].place,
            ground_clouds[cloud_slot].haze_color,
            ground_clouds[cloud_slot].light,
        );
        @else
        let deck = cf::Deck(
            deck_shape,
            clouds_single.motion,
            clouds_single.sun_dir,
            clouds_single.sun_color,
            clouds_single.sky_color,
            clouds_single.place,
            clouds_single.haze_color,
            clouds_single.light,
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
    @if(GROUND_SEE_THROUGH) {
        let horizontal = length(in.world_position.xz - view_bindings::view.world_position.xz);
        let open = SEE_THROUGH * (1.0 - smoothstep(SEE_THROUGH_NEAR_M, SEE_THROUGH_FAR_M, horizontal));
        let cell = vec2<u32>(in.position.xy) % vec2<u32>(4u);
        if bayer4(cell.x, cell.y) < open {
            discard;
        }
    }
    return out;
}

// How much of the ground opens near the camera. Over half, because the
// pipes are the point; under all of it, because a street you cannot see is
// a street you cannot place them in.
@if(GROUND_SEE_THROUGH)
const SEE_THROUGH: f32 = 0.6;
// Horizontal metres from the camera over which the ground closes again, so
// the far city reads as a city and the opening reads as looking down.
@if(GROUND_SEE_THROUGH)
const SEE_THROUGH_NEAR_M: f32 = 250.0;
@if(GROUND_SEE_THROUGH)
const SEE_THROUGH_FAR_M: f32 = 450.0;

// A 4×4 ordered-dither threshold in [0, 1): the pattern with the most even
// spread of holes at every density.
@if(GROUND_SEE_THROUGH)
fn bayer4(x: u32, y: u32) -> f32 {
    var m = array<f32, 16>(
        0.0, 8.0, 2.0, 10.0,
        12.0, 4.0, 14.0, 6.0,
        3.0, 11.0, 1.0, 9.0,
        15.0, 7.0, 13.0, 5.0,
    );
    return (m[y * 4u + x] + 0.5) / 16.0;
}
