// Near-field grass: the vertex stage that turns a blade *slot* into a blade.
//
// The mesh this runs over holds no blades. It is `grass::blade_lattice` — a
// patch's worth of slots on a regular grid, five vertices each, carrying only
// which slot (`position.xz`) and which of the five corners (`uv.x`) they are.
// Every grass patch in the world draws that one shared handle, so walking across
// a park uploads nothing: what moves is the patch entities' transforms, and what
// varies is what this file computes.
//
// Everything a blade has — where it really stands, which way it faces, how tall
// it is, how it leans, what shade of green it is — is hashed out of the slot's
// position in the **anchor-metre frame**, which is reconstructed from the patch's
// `MeshTag`. That frame is the one thing that must not come from
// `world_position`: under `big_space` the render origin jumps a whole grid cell
// as the camera moves, and hashing against it would reshuffle every blade in
// sight at the moment it jumped (the same trap `water.wgsl` documents for its
// wave phase). A tag-derived frame changes only on a re-anchor, which rebuilds
// the layer anyway.
//
// Two things are sampled rather than hashed, because no hash can know them: the
// **mask** says whether this spot is grass at all (red) and how high the ground
// under it is (green). It is a small moving raster around the camera, rebuilt by
// `systems::grass` when the camera walks out of it.
//
// Only the vertex stage is ours. The fragment is `StandardMaterial`'s, unchanged,
// which is the whole reason this is a `MaterialExtension`: the sun, the shadow
// cascades, the fog and the atmosphere's environment map already reach every
// standard material, so a lawn is lit by *this* scene's sky and follows the
// day/night cycle with nothing here to say about it. A standalone `Material` —
// which is what the off-the-shelf Bevy grass plugins are — would have to
// re-derive all of it, and would light grass differently from the ground it
// stands on.

#import bevy_pbr::{
    forward_io::VertexOutput,
    mesh_functions,
    mesh_view_bindings::{view, globals},
    view_transformations::position_world_to_clip,
}

// xy: the mask's minimum corner in anchor metres. z: the ground size of the
// whole mask, in metres. w: the ground plane the blades stand on (the vector
// surfaces' own y).
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> field: vec4<f32>;

// x: blade height in metres (before the per-blade jitter). y: blade width.
// z: how far a blade may be jittered off its lattice slot, in metres — the
// slot spacing with `JITTER_FRAC` already folded in, so it is applied once and
// on the side that documents it. w: patch edge length in metres, the unit the
// `MeshTag` counts in.
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var<uniform> blade: vec4<f32>;

// xy: unit wind direction. z: sway amplitude as a fraction of blade height.
// w: sway speed.
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> wind: vec4<f32>;

// x: distance (m) at which blades start retiring. y: distance by which they all
// have. z: the ground height the mask's green `0` stands for. w: the height
// range a full byte of green covers.
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var<uniform> range: vec4<f32>;

// rgb: the tip colour, linear. a: how much darker the root is (0 = no gradient).
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var<uniform> tint: vec4<f32>;

// x: clump size in metres. y,z: the density the thinnest and thickest patches
// keep, 0–1. w: how far a blade's facing may swing off its clump's, in radians.
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var<uniform> clump: vec4<f32>;

// xy: the distance band over which this tier's blades *arrive*, staggered per
// blade. The near tier passes a band below zero, which any real distance has
// already crossed; the far ring passes the near tier's retire band, so its
// blades stand up exactly where the dense ones sink. zw: spare.
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var<uniform> range2: vec4<f32>;

// r: grass coverage. g: ground height, encoded over `range.zw`.
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var mask_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var mask_sampler: sampler;

// The lattice mesh's vertices. Declared here rather than imported from
// `forward_io` because that struct's fields are behind shader-defs and this one
// is exact: the lattice always carries these four attributes, and it carries
// them *because* their presence is what defines `VERTEX_NORMALS` and
// `VERTEX_COLORS` and so gives us somewhere to write the normal and the tone.
struct GrassVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(5) color: vec4<f32>,
};

// A blade's whole identity, from its slot's anchor-metre position. One hash, read
// as eight independent streams — cheaper than eight hashes and, more to the
// point, stable: the input is a *world* position, so the same blade comes back
// identically every frame regardless of which patch entity happens to be over it
// and which way the camera is facing.
fn hash_slot(p: vec2<f32>) -> vec4<f32> {
    // Quantised to millimetres first. The slot is arithmetic on floats
    // (`patch_origin + lattice offset`), so the same slot reached from two
    // different patch origins can differ in the last bit — and an unquantised
    // hash would turn that last bit into a different blade, i.e. a visible seam
    // along every patch boundary.
    let q = floor(p * 1000.0 + 0.5);
    var h = fract(sin(vec4<f32>(
        dot(q, vec2<f32>(127.1, 311.7)),
        dot(q, vec2<f32>(269.5, 183.3)),
        dot(q, vec2<f32>(419.2, 371.9)),
        dot(q, vec2<f32>(113.5, 271.9)),
    )) * 43758.5453);
    return h;
}

fn hash_slot2(p: vec2<f32>) -> vec4<f32> {
    let q = floor(p * 1000.0 + 0.5);
    return fract(sin(vec4<f32>(
        dot(q, vec2<f32>(521.7, 149.3)),
        dot(q, vec2<f32>(197.3, 443.1)),
        dot(q, vec2<f32>(311.9, 97.7)),
        dot(q, vec2<f32>(73.1, 389.3)),
    )) * 24634.6345);
}

// Scalar hash on a lattice point, for the value noise below.
fn hash21(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

// Smooth 2-D value noise in metres. The thing that makes a lawn stop looking
// laid: real grass clumps because the soil under it varies, so density has to
// vary on a scale that has nothing to do with the lattice the blades sit on.
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    // Smoothstep the interpolant, or the lattice shows as diamonds.
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

/// The two ends of the per-clump hue spread, as multipliers on the tip colour.
///
/// Their midpoint is **exactly 1.0 on every channel**, and that is the contract:
/// averaged over a lawn this leaves the material's `tint.rgb` untouched, so the
/// blades still hand over to the flat `palette::GRASS` fill at the edge of the
/// pool and above the altitude fade without a coloured disc appearing around the
/// camera. Change one end and change the other.
const GRASS_WARM: vec3<f32> = vec3<f32>(1.20, 1.02, 0.80);
const GRASS_COOL: vec3<f32> = vec3<f32>(0.80, 0.98, 1.20);

/// What the top of a blade is multiplied by, relative to its body.
///
/// Brighter *and* warmer: the tip is the part of a blade that its neighbours do
/// not shade, so it is both better lit and closer to the sun's own colour. It is
/// the other half of the root darkening — one gives the mat a floor, this gives
/// it a ceiling — and between them a dense lawn has a gradient to read instead of
/// being one flat sheet.
const TIP_SUN: vec3<f32> = vec3<f32>(1.18, 1.12, 0.86);

// Which of the five vertices this is, as (height up the blade, offset across it).
// The strip narrows on the way up — that taper is what stops a blade reading as
// a shard when it leans, and it is why the tip is one vertex and not two.
fn blade_corner(role: u32) -> vec2<f32> {
    switch role {
        case 0u: { return vec2<f32>(0.0, -0.5); }
        case 1u: { return vec2<f32>(0.0, 0.5); }
        case 2u: { return vec2<f32>(0.55, -0.32); }
        case 3u: { return vec2<f32>(0.55, 0.32); }
        default: { return vec2<f32>(1.0, 0.0); }
    }
}

@vertex
fn vertex(v: GrassVertex) -> VertexOutput {
    var out: VertexOutput;

    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);

    // The patch's own corner in anchor metres, packed by `systems::grass` as two
    // signed 16-bit patch indices. Two i16s rather than a float pair because a
    // `MeshTag` is one u32 — and it is enough: at the patch sizes here the range
    // is a couple of hundred kilometres from the map anchor, and the map
    // re-anchors within a few kilometres.
    let tag = i32(mesh_functions::get_tag(v.instance_index));
    // Arithmetic shifts, so both halves come back signed — a patch west or north
    // of the map anchor has a negative index and must not read as 65535.
    // Not `patch` — that is a WGSL *reserved* keyword (reserved for a future
    // extension, so it parses nowhere and names nothing). Naga rejects the whole
    // module, `pbr_opaque_mesh_pipeline` fails to build, and bevy answers by
    // skipping the pass: the lawn is simply absent, with a healthy patch count
    // and a healthy coverage mask. `systems::pipeline_health` is the only thing
    // in the tree that says so out loud.
    let patch_ij = vec2<f32>(f32(tag >> 16u), f32((tag << 16u) >> 16u));
    let slot = patch_ij * blade.w + v.position.xz;

    let h = hash_slot(slot);
    let h2 = hash_slot2(slot);

    // Off the lattice, or the lawn is visibly a grid. Kept inside half a spacing
    // so blades never cross and the density stays what the lattice says it is.
    let stand = slot + (h.xy - 0.5) * blade.z;

    // The mask. `textureSampleLevel`, not `textureSample`: there are no
    // derivatives in a vertex stage. Clamped to the field's own edge and then
    // *forced* to zero outside it, because clamp-to-edge would otherwise smear
    // the field's border row outwards over the whole world.
    let uv = (stand - field.xy) / field.z;
    let inside = f32(all(uv > vec2<f32>(0.0)) && all(uv < vec2<f32>(1.0)));
    let m = textureSampleLevel(mask_texture, mask_sampler, uv, 0.0);
    let cover = m.r * inside;
    let ground = range.z + m.g * range.w;

    // Distance is taken in render space — both ends of it are, so the frame
    // cancels and no anchor-metre camera position is needed.
    let base_render = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(v.position, 1.0),
    ).xyz;
    let dist = distance(base_render.xz, view.world_position.xz);

    // Retire blades at *staggered* distances rather than shrinking the whole
    // field together: each blade draws its own cut-off out of the ramp, so the
    // lawn thins out towards its edge instead of sinking into the ground as one
    // sheet. Ghost of Tsushima drops three blades in four at its LOD switch; this
    // is the same idea made continuous, which costs nothing here because there is
    // no switch to hide.
    //
    // Arrival is the same stagger run the other way, and it draws from the SAME
    // hash lane (`h.z`) on purpose: the far ring's arrive band equals the near
    // tier's retire band, so at any distance through the handover a blade that
    // has retired from one lattice has, in expectation, stood up on the other —
    // the total blade count ramps smoothly from the near density to the far one
    // and no circle exists for the eye to find.
    let cut = mix(range.x, range.y, h.z);
    let acut = mix(range2.x, range2.y, h.z);
    let reach = smoothstep(acut - 2.0, acut, dist) * (1.0 - smoothstep(cut - 2.0, cut, dist));

    // The clump this blade belongs to. Square cells rather than a true Voronoi:
    // the blades are jittered off their slots and the density noise below is
    // continuous, so the cell edges never surface, and a Voronoi would cost nine
    // hashes per vertex to hide something already hidden.
    let clump_cell = floor(stand / clump.x);
    let ch = hash_slot(clump_cell * clump.x);

    // Two octaves, and they are two different phenomena: a metre-scale one for
    // wear, shade and damp ground, and a tuft-scale one for the clumping itself.
    let wear = value_noise(stand * 0.28);
    let tuft = value_noise(stand * 1.6);
    let density = clamp(
        mix(clump.y, clump.z, 0.55 * wear + 0.45 * tuft),
        0.0,
        1.0,
    );

    // One probabilistic test does three jobs, which is why it is a *test* and not
    // a multiply: it thins the clumps, and it feathers the edge of the lawn —
    // where `cover` falls off, proportionally fewer blades survive, so a lawn
    // meets a path in a scatter rather than on a line. A blade either stands or
    // it does not; there is no half-height blade in nature and a smoothstep here
    // produced a visible mown ring around every path.
    let survives = f32(h.z < density * cover);
    let live = reach * survives;

    // Blades in a clump broadly agree on which way they lean, and fan out from
    // there. A fully independent yaw per blade averages out to a uniform carpet
    // at any distance — the variation has to have a *scale* to be visible.
    let yaw = ch.x * 6.2831853 + (h2.x - 0.5) * clump.w;
    let facing = vec3<f32>(-sin(yaw), 0.0, cos(yaw));
    let side = vec3<f32>(cos(yaw), 0.0, sin(yaw));

    // Thin ground grows short grass, and a clump has its own vigour. Both ride
    // the same noise the density does, which is what keeps them correlated —
    // a sparse patch of tall blades reads as damage, not as a lawn.
    //
    // The spreads are wide on purpose: this is wild grass, and what separates a
    // verge from a lawn is less the mean height than the unevenness — 0.6–1.4×
    // per blade and 0.65–1.35× per clump means neighbouring stands genuinely
    // differ, where the original mown ranges read as a cut surface.
    let height = blade.x
        * (0.6 + 0.8 * h.w)
        * mix(0.65, 1.35, ch.y)
        * mix(0.72, 1.08, density)
        * live;

    let corner = blade_corner(u32(v.uv.x + 0.5));
    let t = corner.x;

    // Two bends, and they are different things. The static one is the blade's own
    // droop, in its own random direction, which is what keeps a still lawn from
    // looking like a bed of nails. The wind one is downwind for every blade at
    // once — that shared direction is what makes a gust read as a gust rather
    // than as noise.
    //
    // The per-blade phase offset is *small* (a fraction of a radian, not a full
    // turn). A uniform random phase was the first version and it is subtly
    // wrong: at any instant half the blades lean upwind and half downwind, so
    // the field shimmers in place instead of being pushed. The travelling term
    // is what carries a gust across the lawn, and it only reads if neighbours
    // broadly agree.
    let drift = value_noise(stand * 0.12 - wind.xy * (globals.time * 0.35));
    let phase = dot(stand, wind.xy) * 0.45 + globals.time * wind.w + h2.y * 0.9;
    let gust = (sin(phase) + 0.35 * sin(phase * 2.3 + 1.7)) * (0.55 + 0.75 * drift);
    // A blade of grass is not a spike. At 0.18–0.40 the tip moved 3–6 cm on a
    // 15 cm blade — about 15° off vertical — and a lawn of those reads as a
    // brush. Real grass folds over under its own weight, so the arc has to be
    // the dominant feature of the silhouette, not a lean on a straight line.
    // Per *clump*, so a tuft folds together. The range is heavier than the
    // lawn's original 0.38–0.88 because wild grass at shin height cannot stand
    // straight: the tallest clumps must visibly bow.
    let lean = 0.45 + 0.65 * ch.z;
    // t², because a blade is stiff at the root and loose at the tip.
    let curve = t * t;

    // A blade edge-on to the camera is a sub-pixel sliver, and a field of those
    // aliases into crawling static under any camera motion. Widening it towards
    // the viewer as it turns away is the standard answer and costs two lines.
    let to_cam = normalize(view.world_position - base_render);
    let edge_on = 1.0 - abs(dot(to_cam, facing));
    // …and a blade that is *not there* has to collapse to a point rather than to
    // a zero-height strip. `live` already takes the height to zero, but without
    // this factor the five vertices still spread along `side`, so a dead slot
    // ends up a flat hairline lying in the ground plane. A hairline has no area
    // to rasterise in theory and catches the occasional pixel centre in
    // practice — and a frame runs ~1.5 million slots against a couple of dozen
    // live blades, so "occasional" is a field of single-pixel specks over the
    // whole near view. They show only where what is behind them is darker than
    // grass, which is why this arrived as "speckles on the canals" and not as
    // anything wrong with the lawns: over pale paving they are invisible, and
    // the slots sit at `ground + field.w`, above the water's rung in the
    // coplanar stack, so a canal cannot occlude them. And they crawl, because
    // which degenerate triangle lands on a pixel centre is a property of the
    // projection and re-rolls on every camera move.
    let width = blade.y * (0.8 + 0.4 * h2.w) * (1.0 + edge_on * 0.6) * step(1e-6, live);

    var world = vec3<f32>(stand.x, ground + field.w, stand.y);
    world += side * (corner.y * width);
    world += vec3<f32>(0.0, height * t, 0.0);
    world += facing * (lean * curve * height);
    world += vec3<f32>(wind.x, 0.0, wind.y) * (wind.z * gust * curve * height);
    // Bending a blade over does not stretch it: what leaves the vertical has to
    // come off the top, or a gale makes the whole lawn visibly taller.
    let bend = lean * curve + abs(wind.z * gust * curve);
    world.y -= min(bend * bend * 0.5, 1.0) * height * t;

    // The blade is a flat strip, so its true normal is `facing` — which is
    // horizontal, and a horizontal normal under an overhead sun renders a lawn
    // black. Rounding it towards up is what every grass shader does and what
    // makes a field read as a lit surface rather than as a wall of edges.
    let own_normal = normalize(facing * 0.5 + vec3<f32>(0.0, 1.0, 0.0));
    // …and with distance, roll it towards the clump's shared normal. Once a
    // blade is under a pixel, per-blade normals are pure specular noise that
    // crawls under any camera motion; agreeing with the neighbours trades that
    // for a broad sheen and keeps the shape of the field. Ghost of Tsushima's
    // talk is where this comes from.
    let clump_normal = normalize(vec3<f32>(0.0, 1.0, 0.0) + facing * 0.12);
    out.world_normal = normalize(mix(
        own_normal,
        clump_normal,
        smoothstep(range.x * 0.4, range.y, dist),
    ));

    out.world_position = vec4<f32>(world, 1.0);
    out.position = position_world_to_clip(world);
    out.uv = v.uv;

    // Darker at the root: the light that reaches the bottom of a blade has come
    // through the ones around it. This is the cheapest ambient occlusion there
    // is and it is most of what makes a lawn read as having depth.
    // Per *clump* first, per blade second. Per-blade-only colour noise averages
    // back to a flat green a few metres out; a patch of slightly different green
    // is still legible there, and is most of what a real lawn's surface is.
    let tone = (0.86 + 0.28 * ch.w) * (0.94 + 0.12 * h2.z) * mix(0.9, 1.06, density);
    // Per-clump *hue*, not only per-clump brightness. Until this the whole lawn
    // was one green with a scalar on it, and one hue reads as a painted surface
    // however much its value varies — what makes real turf worth looking at from
    // standing height is that neighbouring stands are different greens, some
    // yellowed and some blue-deep.
    //
    // Driven off `tuft` rather than off `ch.w`, which already carries the tone:
    // hue and brightness varying together is one variation wearing two hats, and
    // half the variety for the same instructions. `tuft` is the ~0.6 m noise the
    // clumping itself rides on, so a hue change lands on a stand of grass rather
    // than on a lattice cell.
    let hue = mix(GRASS_COOL, GRASS_WARM, tuft);
    let root = 1.0 - tint.a;
    // `t²` so it is the top third that lifts rather than the whole blade — a
    // linear ramp here just brightens everything and loses the gradient.
    let sun_tip = mix(vec3<f32>(1.0), TIP_SUN, t * t);
    out.color = vec4<f32>(tint.rgb * hue * sun_tip * tone * mix(root, 1.0, t), 1.0);

#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    // Not optional: the standard fragment reads its material out of a bindless
    // array indexed by this. Dropping it renders the lawn in some other
    // material's colours, or in none.
    out.instance_index = v.instance_index;
#endif

    return out;
}
