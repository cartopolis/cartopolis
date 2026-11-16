// A splash's sheet of water (`cartopolis_materials::splash`): the crown a body
// throws up, the column up its middle, and the foam left on the surface.
//
// The mesh is a fixed grid, `splashes::sheet_mesh`: `position` is
// (cos θ, v, sin θ) and `uv` is (θ / 2π, v), v running 0..1 up the sheet (or
// out from the middle of the foam). Everything else — how high, how wide, how
// ragged, how far through its life — is computed here from the clock and two
// vec4s, so a splash uploads one small uniform and no vertices, the trade
// `particles.wesl` makes for the same reason (`docs/notes/upload-bursts.md`).
//
// The vertex stage gives only the broad shape: a shell leaning out, taller on
// some sides than others. Everything small — the fingers of the rim, the
// beads they break into, the white streaks up the sheet, the holes as it
// falls apart, the foam's lace and its edge — is cut per pixel, because a
// rim made of vertices came out as straight-edged shards (2026-11-16).
//
// Blended, not added: white water covers what is behind it, and the clear
// water between the streaks tints it. Lit by the scene's directional lights
// and ambient like a rough white surface that light also passes through, so
// it follows the hour instead of glowing at night.

import bevy_pbr::render::{
    mesh_functions,
    mesh_view_bindings::{view, globals, lights},
    view_transformations::position_world_to_clip,
};

const TAU: f32 = 6.283185307;
const PI: f32 = 3.14159265;

// x: start time, y: rise s, z: fall s, w: shape (0 crown, 1 column, 2 foam).
@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> timing: vec4<f32>;
// x: radius at the start, y: radius gained, z: height, w: seed.
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var<uniform> shape: vec4<f32>;
// The foam's reach (m) in eight directions an eighth of a turn apart, from +x
// towards +z (`splashes::foam_reach`): where the drawn water leaves its level.
@group(constants::MATERIAL_BIND_GROUP) @binding(2) var<uniform> reach_a: vec4<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(3) var<uniform> reach_b: vec4<f32>;

struct SheetVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

struct SheetOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // x: life fraction 0..1 (≥ 1 gone), y: how far into the fall 0..1,
    // z: the sheet's height here (m).
    @location(1) life: vec3<f32>,
    @location(2) normal: vec3<f32>,
    // Metres from the entry point.
    @location(3) local: vec3<f32>,
    @location(4) world: vec3<f32>,
};

fn hash31(q: vec3<f32>) -> f32 {
    var p = fract(q * 0.1031);
    p += dot(p, p.zyx + 31.32);
    return fract((p.x + p.y) * p.z);
}

fn noise3(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = mix(
        mix(hash31(i), hash31(i + vec3<f32>(1.0, 0.0, 0.0)), u.x),
        mix(hash31(i + vec3<f32>(0.0, 1.0, 0.0)), hash31(i + vec3<f32>(1.0, 1.0, 0.0)), u.x),
        u.y,
    );
    let b = mix(
        mix(hash31(i + vec3<f32>(0.0, 0.0, 1.0)), hash31(i + vec3<f32>(1.0, 0.0, 1.0)), u.x),
        mix(hash31(i + vec3<f32>(0.0, 1.0, 1.0)), hash31(i + vec3<f32>(1.0, 1.0, 1.0)), u.x),
        u.y,
    );
    return mix(a, b, u.z);
}

fn fbm3(p: vec3<f32>) -> f32 {
    return 0.55 * noise3(p) + 0.3 * noise3(p * 2.13 + 7.1) + 0.15 * noise3(p * 4.37 + 3.3);
}

// A point on the circle at angle θ, scaled: noise sampled there runs all the
// way round with no seam.
fn ring(theta: f32, f: f32) -> vec2<f32> {
    return vec2<f32>(cos(theta), sin(theta)) * f;
}

fn reach_of(i: i32) -> f32 {
    let j = ((i % 8) + 8) % 8;
    if j < 4 {
        return reach_a[j];
    }
    return reach_b[j - 4];
}

// The foam's reach towards `q` (metres, xz), between the eight measured.
fn reach_at(q: vec2<f32>) -> f32 {
    let s = fract(atan2(q.y, q.x) / TAU) * 8.0;
    let i = i32(floor(s));
    return mix(reach_of(i), reach_of(i + 1), fract(s));
}

@vertex
fn vertex(v: SheetVertex) -> SheetOut {
    var out: SheetOut;
    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let origin = world_from_local[3].xyz;

    let rise = timing.y;
    let fall = timing.z;
    let age = globals.time - timing.x;
    let life = rise + fall;
    let alive = f32(age >= 0.0 && age <= life);
    let a = clamp(age, 0.0, life);
    let up = clamp(a / rise, 0.0, 1.0);
    let down = clamp((a - rise) / fall, 0.0, 1.0);

    let theta = v.uv.x * TAU;
    let dir = vec2<f32>(v.position.x, v.position.z);
    let k = v.uv.y;
    let seed = shape.w * 7.0;
    // Out over the whole life, fast at first: water thrown sideways slows.
    let radius = shape.x + shape.y * sqrt(a / life);

    var p: vec3<f32>;
    var n: vec3<f32>;
    var h = 0.0;
    if timing.w > 1.5 {
        // Foam: flat, k out from the middle; its edge is cut per pixel.
        let rr = radius * k;
        p = vec3<f32>(dir.x * rr, 0.0, dir.y * rr);
        n = vec3<f32>(0.0, 1.0, 0.0);
    } else {
        // Thrown up fast and slowing (eased out), then dropping (eased in).
        let lift = (1.0 - (1.0 - up) * (1.0 - up)) * (1.0 - 0.85 * down * down);
        // Taller on some sides than others: a crown is never level.
        let lobes = 0.45 + 0.55 * noise3(vec3<f32>(ring(theta, 1.7), seed));
        h = shape.z * lift * lobes;
        // Bulging in and out a little, so it is not turned on a lathe.
        let wob = noise3(vec3<f32>(ring(theta, 2.6), k * 1.5 + seed + a)) - 0.5;
        var rr: f32;
        var slope: f32;
        if timing.w > 0.5 {
            // The column: thick at its foot, thinning up.
            rr = radius * (1.0 - 0.55 * k) * (1.0 + 0.3 * wob);
            slope = -0.55 * radius / max(h, 0.05);
        } else {
            // The crown leans out, and further as it falls: its top folds
            // over outward and comes down beyond its foot.
            let lean = 0.40 + 0.40 * down;
            rr = radius * (1.0 + 0.25 * wob) + h * (lean * k + 0.6 * down * k * k);
            slope = lean + 1.2 * down * k;
        }
        p = vec3<f32>(dir.x * rr, h * k, dir.y * rr);
        n = normalize(vec3<f32>(dir.x, -slope, dir.y));
    }

    let world = origin + p * alive;
    out.position = position_world_to_clip(world);
    out.uv = v.uv;
    out.life = vec3<f32>(a / life, down, h);
    out.normal = n;
    out.local = p;
    out.world = world;
    return out;
}

struct Light {
    // Sun and sky on a rough surface that light also comes through.
    diffuse: vec3<f32>,
    // The sun's glint off a facet facing it.
    glint: vec3<f32>,
};

fn light_at(n: vec3<f32>, v: vec3<f32>, facet: vec3<f32>) -> Light {
    var out: Light;
    out.diffuse = lights.ambient_color.rgb;
    out.glint = vec3<f32>(0.0);
    for (var i: u32 = 0u; i < lights.n_directional_lights; i = i + 1u) {
        let l = lights.directional_lights[i];
        let to = l.direction_to_light.xyz;
        let ndl = dot(n, to);
        // Wrapped and two-sided: a thin sheet is lit through.
        let wrap = 0.35 + 0.65 * abs(ndl);
        let day = smoothstep(-0.05, 0.1, to.y);
        out.diffuse += l.color.rgb * wrap * day / PI;
        let r = reflect(-v, facet);
        out.glint += l.color.rgb * day * pow(max(dot(r, to), 0.0), 120.0) * 0.06;
    }
    out.diffuse *= view.exposure;
    out.glint *= view.exposure;
    return out;
}

@fragment
fn fragment(in: SheetOut, @builtin(front_facing) front: bool) -> @location(0) vec4<f32> {
    let t = in.life.x;
    let down = in.life.y;
    let k = in.uv.y;
    let theta = in.uv.x * TAU;
    let seed = shape.w * 11.0;
    var n = normalize(in.normal);
    if !front {
        n = -n;
    }
    let v = normalize(view.world_position.xyz - in.world);

    var alpha: f32;
    var white: f32;
    var grain: vec3<f32>;
    if timing.w > 1.5 {
        // Foam: lace and blotches of white with dark water between, an edge
        // that wanders in and out, and holes that grow until nothing is left.
        let q = in.local.xz;
        let r = length(q) / max(shape.x + shape.y * sqrt(t), 0.01);
        let edge = 0.45 + 0.5 * fbm3(vec3<f32>(q * 1.4, seed));
        let reach = reach_at(q);
        let inside = (1.0 - smoothstep(edge - 0.25, edge, r))
            * (1.0 - smoothstep(0.6 * reach, reach, length(q)));
        let blot = fbm3(vec3<f32>(q * 3.2, seed + 0.4 * t));
        let vein = fbm3(vec3<f32>(q * 5.5 + 3.0, seed + 1.7));
        // Lace in strands that break off, not a closed net.
        let lace = (1.0 - smoothstep(0.015, 0.05 + 0.04 * (1.0 - t), abs(vein - 0.5)))
            * smoothstep(0.35, 0.6, fbm3(vec3<f32>(q * 2.0, seed + 5.0)));
        // A churned white mass where the body went through, wider than the
        // crown's foot and ragged, so that foot never shows as a clean curve.
        let churn = fbm3(vec3<f32>(q * 4.5, seed + 2.0 * t));
        let core = (1.0 - smoothstep(0.35, 0.75, r + 0.35 * (churn - 0.5)))
            * (1.0 - smoothstep(0.15, 0.45, t)) * smoothstep(0.3, 0.5, churn);
        let patches = smoothstep(0.38 + 0.45 * t, 0.55 + 0.45 * t, blot);
        let fade = (1.0 - smoothstep(0.55, 1.0, t)) * smoothstep(0.0, 0.03, t);
        white = clamp(max(max(patches, core), lace * 0.7), 0.0, 1.0);
        alpha = white * inside * fade * 0.85;
        grain = vec3<f32>(q * 30.0, seed);
    } else {
        let column = timing.w > 0.5;
        let c = vec2<f32>(cos(theta), sin(theta));
        let h = max(in.life.z, 0.05);
        // The rim: fingers of every height, narrow, between low saddles, and
        // lower as the sheet falls apart. The column ends in a ragged point.
        let fing = noise3(vec3<f32>(c * 6.0, seed));
        let fine = noise3(vec3<f32>(c * 15.0, seed + 3.0));
        var rim = 0.30 + 0.40 * fing * fing + 0.55 * fine * fine * fine;
        if column {
            rim = 0.75 + 0.25 * fine;
        }
        // Always under the mesh's top row: where the rim passed it, the
        // straight top edge of the grid showed as shards (2026-11-16).
        rim = min(rim, 0.93) * (1.0 - 0.35 * down);
        let solid = 1.0 - smoothstep(rim - 0.015, rim + 0.015, k);
        // Towards the rim the sheet is already beads, smaller at the tip.
        let tip = smoothstep(rim - 0.32, rim, k);
        let bead = noise3(vec3<f32>(c * 38.0, k * h * 11.0 + seed - t * 4.0));
        let beads = mix(1.0, smoothstep(0.3 + 0.3 * tip, 0.42 + 0.3 * tip, bead), tip);

        // Fine streaks running up the sheet (thin bright ridges), aerated
        // white churn at its foot where it tears off the surface.
        // Streaks of two widths, each only in places, so the sheet is not
        // combed: clear water shows between them.
        let streak = noise3(vec3<f32>(c * 34.0, k * 1.1 + seed));
        let thin = noise3(vec3<f32>(c * 80.0, k * 1.6 + seed + 9.0));
        let lace = smoothstep(0.78, 0.95, 1.0 - abs(2.0 * streak - 1.0))
            + 0.6 * smoothstep(0.85, 0.97, 1.0 - abs(2.0 * thin - 1.0));
        let churn = fbm3(vec3<f32>(c * 12.0, k * h * 4.0 + seed + t * 1.5));
        let patchy = smoothstep(0.35, 0.65, fbm3(vec3<f32>(c * 5.0, k * h * 2.0 + seed + 4.0)));
        // Aerated white at the foot, up to a height that wanders.
        let foot = 1.0 - smoothstep(0.0, 0.25 + 0.35 * noise3(vec3<f32>(c * 4.0, seed + 6.0)), k);
        white = clamp(
            0.8 * foot * smoothstep(0.45, 0.65, churn) + 0.85 * lace * patchy
                + 0.25 * patchy * smoothstep(0.45, 0.7, churn)
                + 0.7 * tip * smoothstep(0.4, 0.7, bead),
            0.0,
            1.0,
        );
        // Holes open as it falls apart.
        let gone = smoothstep(-0.05 + 0.85 * down, 0.10 + 0.85 * down, churn);
        let ends = smoothstep(0.0, 0.04, t) * (1.0 - smoothstep(0.65, 1.0, t));
        // The foot is torn, not a clean curve on the water: it breaks into
        // churn up to a height that wanders round the crown.
        let torn = 0.04 + 0.16 * noise3(vec3<f32>(c * 9.0, seed + 8.0));
        let base = smoothstep(0.0, 0.02, k)
            * max(smoothstep(torn * 0.5, torn, k), smoothstep(0.45, 0.6, churn));
        alpha = mix(0.22, 0.92, white) * solid * beads * gone * ends * base;
        grain = vec3<f32>(c * 90.0, k * h * 40.0 + seed);
    }
    if alpha <= 0.01 || t >= 1.0 {
        discard;
    }
    // Clear water tints what is behind it with the sky's blue-grey; white
    // water is nearly white. A facet here and there catches the sun.
    let facet = normalize(n + 0.7 * (vec3<f32>(noise3(grain), noise3(grain + 17.0), noise3(grain + 31.0)) - 0.5));
    let light = light_at(n, v, facet);
    let albedo = mix(vec3<f32>(0.30, 0.40, 0.45), vec3<f32>(0.80, 0.85, 0.88), white);
    let colour = albedo * light.diffuse + light.glint * (0.3 + 0.7 * white);
    return vec4<f32>(colour, alpha);
}
