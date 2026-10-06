// Windows on the 3DBAG buildings — a pattern in the wall, not quads on it.
//
// `osm_buildings` builds real window quads and has since long before this. It
// cannot help here, because `osm_buildings` *despawns* its own detail patch over
// every cell 3DBAG covers (`lod22_covers`) — so over the Netherlands, which is
// the whole point of that dataset, the buildings you actually see had no windows
// at all, by day or by night.
//
// Quads were the wrong answer at this scale and the arithmetic is not close. A
// z14 LoD2.2 cell is thousands of buildings and ~240k triangles; four floors of
// windows on each is tens of megabytes of extra mesh per cell, on the lane that
// already owns the largest assets in the app and the documented device-loss
// churn (`docs/notes/upload-bursts.md`). So nothing is stored: the windows are
// computed per fragment from the wall itself, which costs no vertex, no upload
// and no residency, and reaches every building rather than a patch around the
// camera. `systems::grass` made the same call for the same reason ("generated,
// not scattered"), as did `globe_ocean` ("a BRDF split, not water").
//
// Three things are load-bearing:
//
// * **A wall is found by its normal, not by a flag.** The source tags each
//   triangle roof/wall, but that tag is not in the mesh and putting it there
//   would cost a vertex attribute on the biggest geometry lane we have. A
//   near-horizontal normal is a roof and a near-vertical one is a wall, which is
//   what the tag means anyway.
// * **The lattice must survive the floating origin.** `world_position` under
//   big_space jumps by a whole `WORLD_CELL_EDGE` (10,000 m) as the camera crosses
//   a grid cell — the trap `water.wgsl` avoids with UV0 and `grass.wgsl` with
//   `MeshTag`. Neither is available here (a shared vertex belongs to two walls
//   with different tangents, so no per-vertex coordinate can serve both), so the
//   lattice is chosen to *divide* 10,000 instead: `lattice.x` and `lattice.y`
//   both do, and the per-window hash is taken modulo `HASH_PERIOD`, which divides
//   the resulting index step. The pattern is then bit-identical either side of a
//   jump. Change either pitch without checking that and every window in the city
//   changes brightness when you fly 10 km.
// * **The horizontal coordinate comes from the wall's own tangent.** Two
//   coplanar triangles of one wall share a normal, so they share a tangent and
//   the columns line up across the seam between them. Adjacent walls of a
//   building meet at a corner where nothing needs to line up.
//
// Known limit: floors sit on a global height lattice rather than on each
// building's own base, because the shader cannot see the base. With the terrain
// layer off every building stands at y = 0 and it is exact; with it on, Dutch
// relief moves a base by a metre or two and a ground-floor band can sit low on
// its own wall. Real streets do share floor levels, so this reads as a minor
// misalignment rather than as a mistake.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types::{PbrInput, STANDARD_MATERIAL_FLAGS_UNLIT_BIT},
}

// x: column pitch (m). y: floor pitch (m). z: window width (m). w: window height (m).
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> lattice: vec4<f32>;
// x: night factor, 0 by day → 1 after dusk. y: emissive gain at full night.
// z: how much a window darkens the facade by day. w: lowest wall height (m) that
// carries windows at all — below it a fragment is a garden wall or a shed.
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var<uniform> glow: vec4<f32>;
// rgb: the lit colour, linear. a unused.
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> lit: vec4<f32>;
// x: occupancy block size (m). y: blocks per hash period. z, w: the fraction of
// windows lit in the deadest and the liveliest block.
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var<uniform> block: vec4<f32>;

// ─── The ghosted building ────────────────────────────────────────────────────
//
// One building's walls are taken out of this pass, so you can see into it —
// which is what happens when the avatar walks inside one. `systems::interiors`
// has already built the room behind the wall, and `building_select` draws a
// translucent shell in the wall's place; this is the half that has to *stop*
// drawing, because a merged cell mesh has no per-building entity to hide.
//
// It is a polygon test on a uniform rather than a per-vertex key, and the reason
// is the one the module note gives for the windows: a vertex attribute on this
// lane costs four bytes on the largest geometry in the app, permanently, to serve
// a state that is off almost always. The ring is rewritten each frame in render
// space, so the floating origin is somebody else's problem.
//
// x: how many ring points are live (0 = nothing ghosted, and then this whole
// block is two instructions). y: 1 when the roof goes as well as the walls — the
// cutaway (`systems::cutaway`). z, w unused.
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var<uniform> ghost: vec4<f32>;
// The ring's bounding box in render-space XZ: min x, min z, max x, max z. The
// broad phase — without it every wall fragment in the city walks the polygon.
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var<uniform> ghost_bounds: vec4<f32>;
// The ring, two points per element (xy then zw), in render-space XZ.
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var<uniform> ghost_ring: array<vec4<f32>, 16>;

/// Ring points this shader can hold. A BAG outline simplified at 10 cm is about
/// nine points; anything longer is simplified again on the CPU before it is sent,
/// which moves a tint boundary by centimetres and nothing else.
const GHOST_MAX_POINTS: i32 = 32;

/// A normal this far off horizontal is a wall. Roofs — including the steep
/// pitched ones 3DBAG actually carries — stay above it and get nothing.
const WALL_NORMAL_Y: f32 = 0.42;

/// The window hash repeats on this many columns and floors. It has to divide the
/// index step a floating-origin jump produces (10,000 / pitch), which is what
/// keeps the pattern stable — see the module note. 32 divides both 4,000 and
/// 3,200, and 32 windows is ~80 m, wider than almost any single facade.
const HASH_PERIOD: f32 = 32.0;

/// Roughness of a pane, against the wall's own (0.95 — as matte as the parameter
/// goes).
///
/// Not near zero. A perfectly smooth pane mirrors the sky, and with no
/// screen-space reflection behind it that arrives as a flat block of sky colour
/// with a hard edge on every window in the city. At this value the pane takes a
/// long specular highlight and a sheen of the environment map, which is what
/// glass does at the distances anybody looks at these buildings from.
const GLASS_ROUGHNESS: f32 = 0.14;

/// Reflectance of a pane, against bevy's 0.5 dielectric default (~4% at normal
/// incidence). Glass sits nearer 8%.
const GLASS_REFLECTANCE: f32 = 0.72;

/// Peak-to-peak spread of the wall's own roughness between lattice cells, around
/// whatever the material asks for (0.95 on both building lanes).
///
/// Small on purpose. At this end of the parameter the specular lobe is broad
/// either way, so ±0.05 is a change in how a wall *catches* a low sun and not a
/// change in what it is made of — which is the whole ask. Widening it to where
/// you can see it per cell turns a facade into a chequerboard, because the cell
/// is a hard-edged lattice and nothing here blurs across one.
const WALL_FINISH_RANGE: f32 = 0.10;

/// Cheap 2D value hash. Not for cryptography and not for a large domain — it is
/// asked for one number per window per frame.
fn hash2(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

// ─── Roofs ───────────────────────────────────────────────────────────────────
//
// The roof's *colour* is measured (atlas's `roofs` dataset, baked into the
// vertex colour by `lod22`); what it is made of is drawn here, the same way the
// windows are: from the fragment's own position and normal, nothing stored. A
// pitched roof is courses of tiles, a flat one is a finish chosen by its
// measured tone. Every pattern is normalised to a mean of 1 and converges on 1
// below a pixel, so from a distance a roof is exactly the colour atlas measured
// and up close it has the texture that colour is the average of.
//
// Known limit, shared with the windows: a pitched roof's course and column
// lattice runs along the roof's own slope, which is not an axis, so a
// floating-origin jump (10 km of flight) shifts the pattern's phase on it. The
// tiles are then drawn one course over from where they were — a one-frame change
// in which tile is dark, at a distance where nobody can count tiles.

/// A normal closer to vertical than this is a flat roof — `lod22`'s
/// `FLAT_ROOF_NORMAL_Y`, which picks the measured flat colour by the same test.
const FLAT_ROOF_Y: f32 = 0.90;
/// Tile courses: the exposed length of one course down the slope, and the
/// width of one tile across it. A Dutch pantile shows ~0.30 × 0.21 m.
const TILE_COURSE_M: f32 = 0.30;
const TILE_WIDTH_M: f32 = 0.22;
/// The mean of the tile pattern below. The measured colour is the lit face of
/// a tile — atlas drops the darkest quarter of a roof's pixels, which is where
/// the laps' shadows went — so the pattern is not renormalised: up close the
/// faces are the measured colour and the laps darker, and at range the roof
/// converges on this share of it, which is what the photo's own average is.
const TILE_MEAN: f32 = 0.88;
/// Width of one bitumen roll.
const ROLL_M: f32 = 1.0;
/// The scale of a gravel ballast's grain, and of the weathering, repairs and
/// ponding that make one flat roof blotchy at a distance. Both divide the
/// floating origin's 10,000 m jump into a multiple of the 256 `hash_wrapped`
/// repeats on (124,928 and 2,048 steps), so a flat roof — whose lattices run
/// along the world axes — is bit-identical either side of one.
const GRAIN_M: f32 = 0.0800461;
const PATCH_M: f32 = 4.8828125;

// ─── Walls ───────────────────────────────────────────────────────────────────
//
// The rest of a building, on the terms the roofs above are drawn on: what the
// shader is told about it arrives packed in the vertex colour's alpha, and what
// a wall is made of is read off its colour — the era palette already chose red
// brick for a 1900 terrace and pale render for a 2005 block, and a measured
// colour says the same thing. Every pattern converges on the wall's own colour
// below a pixel.

/// A shop's ground floor, and the storey everything else is divided into.
const SHOP_FLOOR_M: f32 = 4.2;
const STOREY_M: f32 = 3.0;
/// With the eaves known, the ground floor's windows start at a real sill.
const GROUND_SILL_M: f32 = 0.7;
/// A shop window, with the fascia over it.
const SHOP_PANE_M: vec2<f32> = vec2<f32>(2.3, 2.9);
/// A Dutch waal-format brick laid with its joint: 210 + 10 mm long, 50 + 12.5 mm
/// a course. Below a pixel from a few tens of metres, which the blur handles.
const BRICK_LENGTH_M: f32 = 0.22;
const BRICK_COURSE_M: f32 = 0.0625;
/// The share of a brick wall that is mortar, for keeping its mean the wall's
/// colour: bed joints are 0.2 of a course, head joints 0.045 of a brick.
const MORTAR_SHARE: f32 = 0.24;
const MORTAR: vec3<f32> = vec3<f32>(0.42, 0.41, 0.38);
/// The shadow a gutter and the eaves' overhang throw down the wall.
const GUTTER_M: f32 = 0.35;

/// Window size in metres for an era (`building_windows::era`): tall narrow
/// sashes before 1920, squarer between the wars and after, long horizontal
/// bands in the system-built decades, large panes since.
fn pane_for_era(era: u32, fallback: vec2<f32>) -> vec2<f32> {
    switch era {
        case 1u: { return vec2<f32>(0.95, 1.85); }
        case 2u: { return vec2<f32>(1.4, 1.5); }
        case 3u: { return vec2<f32>(2.35, 1.2); }
        case 4u: { return vec2<f32>(1.9, 2.1); }
        default: { return fallback; }
    }
}

/// What the wall is made of, drawn over its colour `c` (linear): brick where it
/// is a saturated warm colour, concrete panels where it is a mid grey, render
/// where it is pale. `brick` is the brick lattice, `floor_f` the continuous
/// floor coordinate, `brick_px`/`px` how much of each lattice a pixel spans.
fn wall_finish(
    c: vec3<f32>,
    u: f32,
    v: f32,
    floor_f: f32,
    brick: vec2<f32>,
    brick_px: f32,
    px: f32,
) -> vec3<f32> {
    let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let hi = max(c.r, max(c.g, c.b));
    let sat = (hi - min(c.r, min(c.g, c.b))) / max(hi, 1e-4);
    let brick_w = smoothstep(0.18, 0.32, sat) * (1.0 - smoothstep(0.3, 0.45, luma)) * f32(c.r >= c.b);
    let pale_w = smoothstep(0.28, 0.42, luma) * (1.0 - brick_w);
    let concrete_w = (1.0 - brick_w) * (1.0 - pale_w);

    // Brick: stretcher bond, each course half a brick over, every brick its own
    // firing. The face is darkened by just enough that face and mortar together
    // average to `c`, so the colour a wall was given is the colour it reads as.
    let course = floor(brick.y);
    let along = brick.x + 0.5 * course;
    let fb = vec2<f32>(fract(along), fract(brick.y));
    let mortar = max(f32(fb.y < 0.2), f32(fb.x < 0.045));
    let fired = 0.88 + 0.24 * hash_wrapped(vec2<f32>(floor(along), course) + vec2<f32>(5.0, 3.0));
    let face = max((c - MORTAR_SHARE * MORTAR) / (1.0 - MORTAR_SHARE), c * 0.3);
    // Faded out earlier than the windows are: a course is 6 cm, and at a few
    // pixels a course the bond beats against the pixel grid into moiré rings
    // across the whole wall (seen on the Deck at 20–40 m, 2026-09-30).
    let brick_blur = clamp(brick_px * 3.0 - 0.3, 0.0, 1.0);
    let bricked = mix(mix(face * fired, MORTAR, mortar), c, brick_blur);

    // Concrete: panel joints a window bay apart and at every floor, and the
    // streaks rain leaves running down from each sill.
    let jx = abs(fract(u / 2.5 + 0.5) - 0.5) * 2.5;
    let jy = abs(fract(floor_f + 0.5) - 0.5) * STOREY_M;
    let joint = (1.0 - smoothstep(0.0, 0.03, min(jx, jy))) * (1.0 - clamp(px * 1.6 - 0.35, 0.0, 1.0));
    let streak = value_noise(vec2<f32>(u / 0.7, v / 5.0)) - 0.5;
    let panelled = c * (1.0 + 0.18 * streak) * mix(1.0, 0.55, joint);

    // Render: a plaster finish, faintly uneven.
    let rendered = c * (1.0 + 0.1 * (value_noise(vec2<f32>(u, v) / 1.3) - 0.5));

    return bricked * brick_w + panelled * concrete_w + rendered * pale_w;
}

/// Height of a facade's plinth — the darker, harder-wearing course it stands on.
const PLINTH_M: f32 = 0.5;

/// A plinth at the foot of any building whose eaves are known, with a thin
/// ledge along its top; and on a building from before 1920 a stone cornice
/// under the eaves. `dv` is how many metres of wall one pixel spans.
fn base_and_cornice(c: vec3<f32>, v: f32, eave: f32, era: u32, dv: f32) -> vec3<f32> {
    if eave <= 0.0 {
        return c;
    }
    let crisp = 1.0 - clamp(dv * 12.0 - 0.5, 0.0, 1.0);
    let plinth = f32(v < PLINTH_M - 0.04);
    let ledge = f32(v >= PLINTH_M - 0.04 && v < PLINTH_M);
    var out = c * mix(1.0, 0.72, plinth) * mix(1.0, 1.3, ledge * crisp);
    if era == 1u && eave > 6.0 {
        let cornice = f32(v > eave - 0.7 && v < eave - 0.32);
        let shadow = f32(v > eave - 0.8 && v <= eave - 0.7);
        out = mix(out, SILL_COLOUR * 1.25, cornice * crisp);
        out = out * mix(1.0, 0.6, shadow * crisp);
    }
    return out;
}

/// The eaves' shadow: a dark band under the gutter, fading out below a pixel.
fn gutter_shade(v: f32, eave: f32, gutter_px: f32) -> f32 {
    if eave <= 0.0 {
        return 1.0;
    }
    let below = eave - v;
    let band = step(-0.05, below) * (1.0 - smoothstep(0.6 * GUTTER_M, GUTTER_M, below));
    return mix(1.0, 0.5, band * (1.0 - clamp(gutter_px * 1.6 - 0.35, 0.0, 1.0)));
}

// ─── Doors ───────────────────────────────────────────────────────────────────

/// `building_windows::DOOR_STEP_M` and `DOOR_CLEAR_M`, and `lod22`'s leaf size.
const DOOR_STEP_M: f32 = 0.2;
const DOOR_CLEAR_M: f32 = 1.0;
const DOOR_W: f32 = 1.1;
const DOOR_H: f32 = 2.1;

/// A doorway part (`lod22::push_doorway`). The surround, reveals and step have
/// UV0.y = -0.5 and keep their plain colour; the leaf has UV0 = (x + 10 × style,
/// -1 - y) and is drawn here: panels, glass and a handle. Style 3 is a window's
/// pane in a designed facade (`lod22_facade`): all glass, no handle.
fn door_surface(pbr_input: ptr<function, PbrInput>, uv: vec2<f32>) {
    if uv.y > -0.75 {
        // UV0.x = 1 is a scaffold's galvanised tube (`scaffolding`).
        let metal = f32(uv.x > 0.5 && uv.x < 1.5);
        (*pbr_input).material.perceptual_roughness = mix(0.7, 0.45, metal);
        (*pbr_input).material.metallic = 0.7 * metal;
        return;
    }
    let style = floor(uv.x / 10.0);
    let x = uv.x - 10.0 * style;
    let y = -uv.y - 1.0;
    let rim = 0.11;
    var glass = 0.0;
    var tone = 1.0;
    if style < 0.5 {
        // Panelled, with a glazed transom: two raised panels below a lock rail,
        // split by a muntin, each with a darker bevel round it.
        let lower = x > rim && x < DOOR_W - rim && y > 0.25 && y < 1.2
            && abs(x - DOOR_W * 0.5) > 0.05;
        if lower {
            let ex = min(x - rim, DOOR_W - rim - x);
            let ey = min(y - 0.25, 1.2 - y);
            let edge = min(min(ex, ey), abs(x - DOOR_W * 0.5) - 0.05);
            tone = select(1.1, 0.72, edge < 0.035);
        }
        glass = f32(x > rim && x < DOOR_W - rim && y > 1.35 && y < DOOR_H - 0.13);
    } else if style < 1.5 {
        // A glass door in a slim frame.
        glass = f32(x > 0.07 && x < DOOR_W - 0.07 && y > 0.07 && y < DOOR_H - 0.07);
    } else if style < 2.5 {
        // Flush, with a tall glass strip on the lock side.
        glass = f32(x > DOOR_W * 0.6 && x < DOOR_W - 0.13 && y > 0.3 && y < DOOR_H - 0.3);
    } else {
        // A window's pane: glass edge to edge; its frame is geometry.
        glass = 1.0;
    }
    let c = (*pbr_input).material.base_color.rgb;
    var col = mix(c * tone, vec3<f32>(0.02, 0.025, 0.03), glass);
    var metal = 0.0;
    // The handle, on the lock side at hand height.
    if style < 2.5 && length(vec2<f32>(x - (DOOR_W - 0.13), y - 1.05)) < 0.035 {
        col = vec3<f32>(0.55, 0.4, 0.15);
        metal = 1.0;
    }
    (*pbr_input).material.base_color = vec4<f32>(col, (*pbr_input).material.base_color.a);
    (*pbr_input).material.metallic = metal;
    (*pbr_input).material.perceptual_roughness = mix(mix(0.5, GLASS_ROUGHNESS, glass), 0.3, metal);
    (*pbr_input).material.reflectance = mix(
        (*pbr_input).material.reflectance,
        vec3<f32>(GLASS_REFLECTANCE),
        glass,
    );
}

// ─── Windows, up close ───────────────────────────────────────────────────────

/// A room seen through a window by day: nearly black.
const GLASS_BASE: vec3<f32> = vec3<f32>(0.03, 0.034, 0.04);

/// What is in the window of a room, drawn from the window's own hash `r`: net
/// curtains or drapes at the sides of about a third of them, a blind part-way
/// down in one in five, and the rest nothing but the dark room.
fn room_through_glass(pm: vec2<f32>, hs: vec2<f32>, r: f32) -> vec3<f32> {
    let pick = fract(r * 13.7);
    let fabric_pick = fract(r * 29.3);
    var fabric = vec3<f32>(0.34, 0.3, 0.24);
    if fabric_pick > 0.6 {
        fabric = vec3<f32>(0.2, 0.05, 0.04);
    } else if fabric_pick > 0.35 {
        fabric = vec3<f32>(0.3, 0.3, 0.3);
    }
    if pick < 0.35 {
        let curtain = f32(abs(pm.x) > hs.x * 0.6);
        return mix(GLASS_BASE, fabric * 0.5, curtain);
    }
    if pick < 0.55 {
        let drop = mix(0.25, 0.7, fract(r * 7.1));
        let blind = f32(pm.y > hs.y * (1.0 - 2.0 * drop));
        let slat = 0.8 + 0.2 * f32(fract(pm.y / 0.045) > 0.3);
        return mix(GLASS_BASE, vec3<f32>(0.42, 0.4, 0.36) * 0.55 * slat, blind);
    }
    return GLASS_BASE;
}

/// A shop by day, through its window: lit, and busy — racks and displays at
/// no particular spacing, brighter under the ceiling lights.
fn shop_through_glass(pm: vec2<f32>, hs: vec2<f32>, along: f32) -> vec3<f32> {
    let racks = 0.7 + 0.6 * value_noise(vec2<f32>(along / 0.55, 3.0));
    let lights = mix(0.9, 1.5, smoothstep(0.4, 0.95, pm.y / max(hs.y, 0.1)));
    return vec3<f32>(0.1, 0.09, 0.075) * racks * lights;
}

// ─── Scaffold netting ────────────────────────────────────────────────────────

/// The netting's mesh pitch, metres.
const NET_MESH_M: f32 = 0.06;
/// Width of one roll of netting; the rolls overlap at a darker seam.
const NET_ROLL_M: f32 = 2.5;

/// Debris netting on a scaffold: a woven sheet, its rolls lapped at seams and
/// sagging between the ties at each lift. The holes show the shadow behind, so
/// up close it is a fabric and from across the street a pale sheet over the
/// facade — which is the whole point of drawing it.
fn netting(pbr_input: ptr<function, PbrInput>, u: f32, v: f32, weave: vec2<f32>, weave_px: f32) -> f32 {
    let g = abs(fract(weave) - 0.5);
    let line = min(0.5 - g.x, 0.5 - g.y);
    let strand = 1.0 - smoothstep(0.1, 0.16, line);
    let near = 1.0 - clamp(weave_px * 1.6 - 0.35, 0.0, 1.0);
    let seam_d = abs(fract(u / NET_ROLL_M + 0.5) - 0.5) * NET_ROLL_M;
    let seam = 1.0 - smoothstep(0.03, 0.07, seam_d);
    // Each lift's sheet sags a little away from its tie line.
    let sag = 0.9 + 0.1 * smoothstep(0.0, 0.5, abs(fract(v / 2.0) - 0.5));
    let c = (*pbr_input).material.base_color;
    (*pbr_input).material.base_color = vec4<f32>(c.rgb * sag * mix(1.0, 0.8, seam), c.a);
    (*pbr_input).material.perceptual_roughness = 0.95;
    (*pbr_input).material.metallic = 0.0;
    // How much of the surface the net covers: its strands up close, its mean
    // at range, and solid where two rolls overlap.
    return max(mix(NET_COVER, strand, near), seam);
}

/// The share of a scaffold's netting that is net rather than hole.
const NET_COVER: f32 = 0.4;
/// The share of a site fence's mesh panel that is wire, at range.
const FENCE_COVER: f32 = 0.18;

/// A site fence's welded mesh (`scaffolding::push_fence`, UV0.x = 3): wires
/// every 7.5 cm across and 22.5 cm up. Returns the wire's coverage.
fn fence_mesh(pbr_input: ptr<function, PbrInput>, wire: vec2<f32>, wire_px: f32) -> f32 {
    let g = abs(fract(wire) - 0.5);
    let d = 0.5 - g;
    let on_wire = max(1.0 - smoothstep(0.03, 0.06, d.x), 1.0 - smoothstep(0.01, 0.025, d.y));
    let near = 1.0 - clamp(wire_px * 1.6 - 0.35, 0.0, 1.0);
    (*pbr_input).material.perceptual_roughness = 0.5;
    (*pbr_input).material.metallic = 0.7;
    return mix(FENCE_COVER, on_wire, near);
}

/// Screen-door transparency: whether a fragment of a surface with this
/// coverage is a hole here, on a 4×4 ordered dither. The building material is
/// opaque and there is no depth prepass (`render_features`), so a discarded
/// fragment simply shows whatever lies behind it.
fn see_through(coverage: f32, frag: vec2<f32>) -> bool {
    let p = vec2<u32>(frag) % vec2<u32>(4u, 4u);
    var bayer = array<f32, 16>(
        0.0, 8.0, 2.0, 10.0,
        12.0, 4.0, 14.0, 6.0,
        3.0, 11.0, 1.0, 9.0,
        15.0, 7.0, 13.0, 5.0,
    );
    return coverage <= (bayer[p.y * 4u + p.x] + 0.5) / 16.0;
}

// ─── Neglect ─────────────────────────────────────────────────────────────────
//
// A building the register lists as out of use, or as waiting for demolition
// (`building_windows::Condition`, carried in the occupancy's unused top). It is
// dark at night, its ground floor is boarded, some of the windows above it too,
// a shop's window is whitewashed from the inside the way an empty shop in the
// Netherlands says so, and the walls carry the grime nobody is washing off. The
// register's word only: no other building is aged by this.

/// Weathered plywood over an opening, in pane-local metres: 1.22 m sheets with
/// a darker seam between them and a grain running up them.
fn boarded(pm: vec2<f32>, hs: vec2<f32>, r: f32) -> vec3<f32> {
    // Pale yellow-brown plywood, a little greyed by the rain — never as grey as
    // glass, or a boarded window reads as a dark pane from the street.
    let age = fract(r * 5.3) * 0.35;
    let ply = mix(vec3<f32>(0.46, 0.33, 0.16), vec3<f32>(0.3, 0.27, 0.22), age);
    let grain = 0.9 + 0.1 * value_noise(vec2<f32>(pm.x / 0.04, pm.y / 0.9));
    let seam_y = abs(fract((pm.y + hs.y) / 1.22 + 0.5) - 0.5) * 1.22;
    let seam = mix(0.6, 1.0, smoothstep(0.004, 0.012, seam_y));
    return ply * grain * seam;
}

/// What boarded windows average to at range.
const BOARD_MEAN: vec3<f32> = vec3<f32>(0.4, 0.3, 0.17);

/// A shop window whitewashed from the inside: swirls of chalk on the glass.
fn whitewashed(pm: vec2<f32>, along: f32) -> vec3<f32> {
    let swirl = value_noise(vec2<f32>(along / 0.3, pm.y / 0.3))
        + 0.5 * value_noise(vec2<f32>(along / 0.11, pm.y / 0.11));
    return mix(vec3<f32>(0.6, 0.6, 0.56), vec3<f32>(0.4, 0.4, 0.37), swirl * 0.5);
}

/// A neglected wall's grime: damp rising from the foot in a ragged line, and
/// dirt streaked down it by the rain. `neglect` is 1 vacant, 2 condemned;
/// `fade` takes the streaks out below a pixel.
fn grimed(c: vec3<f32>, u: f32, v: f32, neglect: f32, fade: f32) -> vec3<f32> {
    let rise = (0.5 + 0.5 * value_noise(vec2<f32>(u / 1.3, 0.5))) * mix(0.8, 1.5, neglect - 1.0);
    let damp = 1.0 - smoothstep(rise * 0.5, rise, v);
    var out = mix(c, c * vec3<f32>(0.5, 0.56, 0.46), damp * 0.75);
    let streak = smoothstep(0.55, 0.9, value_noise(vec2<f32>(u / 0.45, v / 7.0)));
    out = out * (1.0 - (0.2 + 0.1 * neglect) * streak * fade);
    // …and the whole wall a little duller than a kept one.
    return out * mix(0.92, 0.85, neglect - 1.0);
}

/// How far to tilt the normal on a window frame's bevel, as (along the wall,
/// up): towards the glass on the inner 40 % of the frame.
fn frame_bevel(pm: vec2<f32>, hs: vec2<f32>, era: u32, shop: bool) -> vec2<f32> {
    let fw = select(0.065, 0.05, era >= 3u || shop);
    let inner = hs - vec2<f32>(fw, fw);
    let a = abs(pm);
    let into = a - inner;
    let bx = f32(into.x > 0.0 && into.x < fw * 0.4 && a.y < hs.y);
    let by = f32(into.y > 0.0 && into.y < fw * 0.4 && a.x < hs.x);
    return vec2<f32>(-sign(pm.x) * bx, -sign(pm.y) * by) * 0.6;
}

/// Stone sills, and the lintel tone over an old window.
const SILL_COLOUR: vec3<f32> = vec3<f32>(0.42, 0.41, 0.38);

/// A window's frame: white-painted timber on an old building, brown on a
/// system-built one, dark aluminium on a new one.
fn frame_colour(era: u32) -> vec3<f32> {
    switch era {
        case 1u, 2u: { return vec3<f32>(0.66, 0.64, 0.58); }
        case 3u: { return vec3<f32>(0.07, 0.05, 0.035); }
        default: { return vec3<f32>(0.04, 0.042, 0.045); }
    }
}

fn lintel_tone(era: u32) -> f32 {
    return select(0.78, 1.35, era == 1u);
}

/// A band over the opening: a stone lintel on an old house, a soldier course
/// of darker brick between the wars, none after.
fn lintel(pm: vec2<f32>, hs: vec2<f32>, era: u32) -> f32 {
    if era != 1u && era != 2u {
        return 0.0;
    }
    return f32(pm.y > hs.y && pm.y < hs.y + 0.17 && abs(pm.x) < hs.x + 0.08);
}

/// What a point of a window is, given its offset `pm` from the pane's centre and
/// the pane's half size `hs`, both in metres. x: unused. y: frame or glazing
/// bar (1) versus glass (0). z: the glass's shading from the reveal — the
/// opening is set back in the wall, so its top and one side lie in the shadow
/// of the wall around it. w: the sill below it.
fn window_look(pm: vec2<f32>, hs: vec2<f32>, era: u32, shop: bool) -> vec4<f32> {
    let a = abs(pm);
    let fw = select(0.065, 0.05, era >= 3u || shop);
    var bar = f32(a.x > hs.x - fw || a.y > hs.y - fw);
    let bw = 0.028;
    // Glazing bars by era: a sash window's meeting rail and the bars of its
    // upper sash; a transom with a top light between the wars; a transom and
    // a mullion every 0.9 m in a system-built band; one mullion in a wide new
    // pane; a shop window's transom high up.
    switch era {
        case 1u: {
            let rail = abs(pm.y - hs.y * 0.05) < 0.035;
            let upper = pm.y > hs.y * 0.05;
            bar = max(bar, f32(rail || (upper && (abs(pm.x) < bw || abs(pm.y - hs.y * 0.55) < bw))));
        }
        case 2u: {
            let transom = abs(pm.y - hs.y * 0.42) < 0.04;
            bar = max(bar, f32(transom || (pm.y > hs.y * 0.42 && abs(pm.x) < bw)));
        }
        case 3u: {
            let transom = abs(pm.y - hs.y * 0.5) < 0.035;
            let mullion = abs(fract((pm.x + hs.x) / 0.9 + 0.5) - 0.5) * 0.9 < 0.025;
            bar = max(bar, f32(transom || mullion));
        }
        default: {
            bar = max(bar, f32(hs.x > 0.7 && abs(pm.x) < 0.035));
        }
    }
    if shop {
        bar = max(bar, f32(abs(pm.y - hs.y * 0.62) < 0.04));
    }
    let inner = hs - vec2<f32>(fw, fw);
    let reveal_top = smoothstep(inner.y - 0.14, inner.y, pm.y);
    let reveal_side = smoothstep(inner.x - 0.1, inner.x, -pm.x);
    let shade = 1.0 - 0.45 * max(reveal_top, 0.6 * reveal_side);
    let sill = f32(pm.y < -hs.y && pm.y > -hs.y - 0.07 && a.x < hs.x + 0.06 && !shop);
    return vec4<f32>(0.0, bar, shade, sill);
}

/// `hash2` over a lattice index wrapped into a small domain first: `sin` of
/// the index of a 6 cm grain ten kilometres out has no fractional bits left.
fn hash_wrapped(i: vec2<f32>) -> f32 {
    return hash2(i - floor(i / 256.0) * 256.0);
}

/// Smooth value noise in [0, 1].
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let w = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(hash_wrapped(i), hash_wrapped(i + vec2<f32>(1.0, 0.0)), w.x),
        mix(hash_wrapped(i + vec2<f32>(0.0, 1.0)), hash_wrapped(i + vec2<f32>(1.0, 1.0)), w.x),
        w.y,
    );
}

/// One ring point, out of the packed pairs.
fn ghost_point(i: i32) -> vec2<f32> {
    let v = ghost_ring[i / 2];
    return select(v.xy, v.zw, (i & 1) == 1);
}

/// Is this fragment on the wall of the ghosted building?
///
/// Even-odd, the same test `collision::Footprint::contains` runs on the CPU over
/// the same ring — so what you can see through is what you can walk through.
fn in_ghost(p: vec2<f32>) -> bool {
    let n = min(i32(ghost.x), GHOST_MAX_POINTS);
    if n < 3 {
        return false;
    }
    if p.x < ghost_bounds.x || p.x > ghost_bounds.z
        || p.y < ghost_bounds.y || p.y > ghost_bounds.w {
        return false;
    }
    var inside = false;
    var j = n - 1;
    for (var i = 0; i < n; i = i + 1) {
        let a = ghost_point(i);
        let b = ghost_point(j);
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    return inside;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let n = normalize(in.world_normal);
    // `abs`, because a wall's normal may point either way and back faces are
    // culled by the material rather than by this test.
    //
    // The second half is defensive rather than a fix for anything observed. A
    // triangle soup generalised from real survey data does contain degenerate
    // triangles; their normal is zero, `normalize` of that is NaN, and every
    // comparison against a NaN is false — so such a fragment would slip past this
    // test into a garbage tangent. It was *suspected* of drawing the thin vertical
    // streaks on the gable walls in the 2026-08-26 night captures and it is not
    // the cause of those (see `blur` below); the guard is kept because the failure
    // mode is real and silent, not because it fixed that.
    let horiz_len = length(vec2<f32>(n.x, n.z));
    let is_wall = abs(n.y) < WALL_NORMAL_Y && horiz_len > 1e-3;

    // The ghosted building loses its walls, and in a cutaway (y = 1, which is
    // every ghost since `systems::cutaway`) its roof as well. Keeping the roof
    // was right while the inside was one empty room with a glass box round it —
    // without the roof that was a hole in the street with a floor in it. Now the
    // inside is every storey up to the one being looked at, with its walls, and
    // a follow camera above and behind the avatar can only see into it from
    // above: with the roof on it was looking at the underside of the roof.
    // `is_wall` is already the right question and is already answered.
    if (is_wall || ghost.y > 0.5) && in_ghost(in.world_position.xz) {
        discard;
    }

    // Computed **outside** the branch on purpose: `fwidth` below is a derivative,
    // and a derivative taken in non-uniform control flow is undefined — naga
    // rejects some of those outright and the rest read neighbouring fragments
    // that took the other path. The cost is a few ALU ops on roof fragments.
    //
    // `max` rather than the raw length, so a degenerate normal divides by the
    // floor instead of producing an infinity that then propagates into `fwidth`.
    let horiz = vec2<f32>(n.x, n.z) / max(horiz_len, 1e-3);
    let tangent = vec2<f32>(-horiz.y, horiz.x);
    let u = dot(in.world_position.xz, tangent);
    let v = in.world_position.y;

    // What the vertex colour's alpha says about this building — its night
    // occupancy, a shop, its era and its eaves (`building_windows::Facade`
    // packs it; the tests there pin the layout). Alpha 1.0 is code 0, which is
    // "nothing known", and every branch below falls back to the old look on it.
    // The 1/32 is the interpolation margin `building_windows::Facade` describes.
    let packed = pbr_input.material.base_color.a;
    let code_f = max(floor((packed + 1.0 / 32.0) / 4.0), 0.0);
    let occupancy_packed = max(packed - 4.0 * code_f, 0.0);
    // 3 is vacant and 3.5 condemned, past anything a kept building is lit by.
    let neglect = select(0.0, select(1.0, 2.0, occupancy_packed > 3.25), occupancy_packed > 2.75);
    let neglected = neglect > 0.5;
    let occupancy_raw = select(occupancy_packed, 0.0, neglected);
    let code = u32(code_f);
    let eave = f32(code >> 4u) * 0.2;
    let era = (code >> 1u) & 7u;
    // A shop front needs a ground floor to stand in.
    let shop = (code & 1u) == 1u && eave > SHOP_FLOOR_M + 2.0;

    // The floors. Known eaves are divided into whole storeys of about three
    // metres — the rule `interiors` builds its storeys by — and a shop's ground
    // floor is its own taller storey under them. Unknown keeps the one pitch
    // every building used to share.
    var ground_h = lattice.y;
    var storey_h = lattice.y;
    if eave > 0.0 {
        let rest = select(eave, eave - SHOP_FLOOR_M, shop);
        storey_h = rest / max(1.0, round(rest / STOREY_M));
        ground_h = select(storey_h, SHOP_FLOOR_M, shop);
    }
    // A continuous floor coordinate: 0–1 is the ground floor, then one a storey.
    // Measured from y = 0, which is the building's base with the terrain layer
    // off — the limit the module note describes.
    let floor_f = select(v / ground_h, 1.0 + (v - ground_h) / storey_h, v >= ground_h);

    // Index of the window this fragment is in, and where inside it we are.
    // The wall face's own layout, where `lod22` gave one (`WallFaces`): UV0.x
    // metres along the face from its corner, UV0.y an integer carrying the bay
    // width that fits the face and where its door is. Negative UV0.y is a
    // doorway part, 0 is no layout (the OSM lane) and keeps the world lattice.
#ifdef VERTEX_UVS_A
    let face_uv = in.uv;
#else
    let face_uv = vec2<f32>(0.0, 0.0);
#endif
    let door_part = face_uv.y < -0.25;
    let laid_out = face_uv.y > 0.5;
    let face_code = u32(round(max(face_uv.y, 0.0)));
    var bay = lattice.x;
    var along = u;
    var door_u = -1000.0;
    if laid_out {
        bay = f32(face_code % 512u) / 100.0;
        along = face_uv.x;
        let door_step = face_code / 512u;
        if door_step > 0u {
            door_u = f32(door_step) * DOOR_STEP_M;
        }
    }
    // A face too short for a window says so with a 1 cm bay.
    let windowless = laid_out && bay < 0.5;
    let cell = vec2<f32>(along / max(bay, 0.5), floor_f);
    // How much of the lattice one pixel spans. Where that reaches a whole window
    // the grid is below the sampling rate and point-sampling it aliases — into
    // vertical streaks on a wall seen edge-on, and into a crawling sparkle at
    // distance.
    let px = max(fwidth(cell.x), fwidth(cell.y));

    // The roofs' lattices, and how much of each one a pixel spans — outside the
    // branch for the same reason as `px`. `horiz` points down a pitched roof's
    // slope (its normal leans that way); dividing by the normal's vertical part
    // turns a horizontal distance into one measured along the roof.
    let down = dot(in.world_position.xz, horiz) / max(abs(n.y), 0.3);
    let tile = vec2<f32>(u / TILE_WIDTH_M, down / TILE_COURSE_M);
    let tile_px = max(fwidth(tile.x), fwidth(tile.y));
    let grain = in.world_position.xz / GRAIN_M;
    let grain_px = max(fwidth(grain.x), fwidth(grain.y));
    let blotch_uv = in.world_position.xz / PATCH_M;
    let blotch_px = max(fwidth(blotch_uv.x), fwidth(blotch_uv.y));
    let brick = vec2<f32>(u / BRICK_LENGTH_M, v / BRICK_COURSE_M);
    let brick_px = max(fwidth(brick.x), fwidth(brick.y));
    let gutter_px = fwidth(v) / GUTTER_M;
    let weave = vec2<f32>(u, v) / NET_MESH_M;
    let weave_px = max(fwidth(weave.x), fwidth(weave.y));
    let wire = vec2<f32>(u / 0.075, v / 0.225);
    let wire_px = max(fwidth(wire.x), fwidth(wire.y));

    if door_part {
        door_surface(&pbr_input, face_uv);
        // A scaffold's debris netting (`scaffolding`, UV0.x = 2) and a site
        // fence's mesh panel (UV0.x = 3): both are see-through.
        if face_uv.y > -0.75 && face_uv.x > 1.5 {
            var cover = 1.0;
            if face_uv.x > 2.5 {
                cover = fence_mesh(&pbr_input, wire, wire_px);
            } else {
                cover = netting(&pbr_input, u, v, weave, weave_px);
            }
            if see_through(cover, in.position.xy) {
                discard;
            }
        }
    } else if is_wall {
        let idx = floor(cell);
        let f = cell - idx;

        // The wall itself first: what it is built of, then the eaves' shadow,
        // then — below — the windows cut into it. The glass is drawn over the
        // colour as it was, not over the brick: a pane is not bricked.
        let plain = pbr_input.material.base_color.rgb;
        pbr_input.material.base_color = vec4<f32>(
            wall_finish(pbr_input.material.base_color.rgb, u, v, floor_f, brick, brick_px, px)
                * gutter_shade(v, eave, gutter_px),
            pbr_input.material.base_color.a,
        );
        pbr_input.material.base_color = vec4<f32>(
            base_and_cornice(pbr_input.material.base_color.rgb, v, eave, era, gutter_px * GUTTER_M),
            pbr_input.material.base_color.a,
        );
        if neglected {
            pbr_input.material.base_color = vec4<f32>(
                grimed(pbr_input.material.base_color.rgb, along, v, neglect, 1.0 - clamp(px * 0.8, 0.0, 1.0)),
                pbr_input.material.base_color.a,
            );
        }

        // The pane, sized by the building's era and centred in its cell. Written
        // as a half-width test either side of the middle so the window keeps its
        // real size as the pitch changes.
        let on_ground = idx.y < 0.5;
        let floor_h = select(storey_h, ground_h, on_ground);
        var pane_m = pane_for_era(era, lattice.zw);
        var centre = vec2<f32>(0.5, 0.5);
        if shop && on_ground {
            pane_m = SHOP_PANE_M;
            centre.y = 0.42;
            // The fascia over the shop window: a dark band for the sign.
            let fascia = f32(f.y > 0.82 && f.y < 0.95) * (1.0 - clamp(px * 1.6 - 0.35, 0.0, 1.0));
            pbr_input.material.base_color = vec4<f32>(
                pbr_input.material.base_color.rgb * mix(1.0, 0.45, fascia),
                pbr_input.material.base_color.a,
            );
        }
        let half = min(vec2<f32>(pane_m.x / max(bay, 0.5), pane_m.y / floor_h) * 0.5, vec2<f32>(0.47, 0.45));
        // No window where the door is: the ground-floor bays whose pane would
        // come within DOOR_CLEAR_M of the door's centre.
        let clear_of_door = !(on_ground
            && abs((idx.x + 0.5) * bay - door_u) < pane_m.x * 0.5 + DOOR_CLEAR_M);
        let d = abs(f - centre);
        let inside = f32(d.x < half.x && d.y < half.y);

        // Modulo *before* hashing, so a floating-origin jump — which moves `idx`
        // by an exact multiple of HASH_PERIOD — cannot change the answer.
        let key = idx - floor(idx / HASH_PERIOD) * HASH_PERIOD;
        let r = hash2(key);

        // How lively this block is. A second, coarser hash on roughly one
        // building's footprint — without it every facade in the city carries the
        // same fraction of lit windows, which is what made the first night
        // capture read as decoration rather than as a city. Real streets have
        // whole dark buildings standing between the lit ones.
        //
        // Taken on the horizontal position only, so all four walls of a building
        // agree, and moduloed on its own period for the floating-origin reason
        // the window lattice is.
        let bidx = floor(in.world_position.xz / block.x);
        let bkey = bidx - floor(bidx / block.y) * block.y;
        let liveliness = hash2(bkey + vec2<f32>(37.0, 91.0));

        // …scaled by what the building is actually *for*. The vertex colour's
        // alpha carries `osm_buildings::night_lit_scale` of the BAG's
        // `gebruiksdoel` for this building — free, because the material is opaque
        // and that channel was a constant 1.0 (the same spare-channel trick
        // `globe_ocean` plays with its sea mask, and read here for the same
        // reason: before `alpha_discard`, which flattens alpha on an opaque
        // material). 1.0 means "nothing known", so a building outside the BAG
        // coverage — or a whole city outside the Netherlands — keeps the purely
        // hashed liveliness above and looks exactly as it did.
        //
        // Clamped rather than trusted: the scale is authored above 1.0 for homes,
        // and a lit fraction past 1 would light every window on a facade.
        let occupancy = clamp(occupancy_raw, 0.0, 2.0);
        let lit_fraction = clamp(mix(block.z, block.w, liveliness) * occupancy, 0.0, 1.0);

        // Lit if this window's own draw came in under the block's fraction, then
        // ramped so the ones that are on are not all on equally.
        var brightness = 0.0;
        if r < lit_fraction {
            brightness = mix(0.45, 1.25, r / max(lit_fraction, 1e-4));
        }

        // Nothing below the first sill: a fragment that low is a plinth, a garden
        // wall or a shed side, and lighting those reads as a mistake rather than
        // as a window.
        //
        // Where the eaves are known the building is known to be a building, and
        // its ground floor has windows like any other; a shop's is the window.
        let sill = select(glow.w, GROUND_SILL_M, eave >= 4.0);
        let tall_enough = select(f32(v > sill), 1.0, shop && on_ground)
            * f32(clear_of_door && !windowless);

        // Below a pixel, converge on the pattern's own **mean** rather than fading
        // it out. Fading out is the usual answer and it is the wrong one here: the
        // windows that are sub-pixel are the whole lit horizon, and removing them
        // leaves the far half of the city black. Converging keeps the light and
        // loses only the grid, which is what a real city does at that distance.
        //
        // It is worth being clear what this does *not* do, because two captures
        // were spent on it. The thin vertical streaks on a gable wall seen nearly
        // edge-on are **not** aliasing and this does not remove them: at 40 m a
        // window is ~30 px tall, and at ~10° incidence its 1.35 m width projects
        // to ~4 px, so a 4×30 px sliver is the correct projection of a real
        // window and not an artefact. This exists for genuine sub-pixel range,
        // where it stops the grid crawling as the camera moves — which no still
        // capture can show.
        let blur = clamp(px * 1.6 - 0.35, 0.0, 1.0);
        // Fraction of the wall the panes cover — the mean of `inside`.
        let pane_area = 4.0 * half.x * half.y;
        let mean_bright = lit_fraction * 0.85;

        let pane = mix(inside, pane_area, blur) * tall_enough;
        let emissive = mix(inside * brightness, pane_area * mean_bright, blur) * tall_enough;

        // By day a window is dark glass in the wall around it — the wall's own
        // colour, darkened, rather than its brick darkened, which read as a
        // bricked-up opening once the walls had a texture. The glass roughness
        // and reflectance below are what put the sky in it.
        // The window itself, up close: frame, glazing bars, the shadow of the
        // reveal, a sill under it and a lintel over it. Faded out with the
        // same `blur`, so at range only the glass coverage above is left.
        let metres = vec2<f32>(max(bay, 0.5), floor_h);
        let pm = (f - centre) * metres;
        let hs = half * metres;
        let look = window_look(pm, hs, era, shop && on_ground);
        let detail = tall_enough * (1.0 - blur);
        let frame_col = frame_colour(era);
        let shop_front = shop && on_ground;
        // Behind the glass. Not the wall's colour darkened, which tinted every
        // pane in a brick street brick-red: a dark neutral room, seen through
        // glass, with what hangs in the window — curtains, a blind — or, in a
        // shop, the lit shop. The sky it reflects is the lighting's job
        // (`GLASS_REFLECTANCE`), and it only shows over something this dark.
        var interior = select(
            room_through_glass(pm, hs, r),
            shop_through_glass(pm, hs, along),
            shop_front,
        ) * look.z;
        // A neglected building: an empty room behind the glass, a whitewashed
        // shop window, and boards over the ground floor and over half the windows
        // above it — all of them once it is waiting to come down.
        let boards = neglected && !shop_front
            && (on_ground || fract(r * 17.3) < select(0.5, 1.0, neglect > 1.5));
        let board_k = f32(boards);
        if neglected {
            interior = select(GLASS_BASE * 0.7, whitewashed(pm, along), shop_front) * look.z;
        }
        var wall_col = pbr_input.material.base_color.rgb;
        // Sill and lintel sit on the wall around the opening.
        wall_col = mix(wall_col, SILL_COLOUR, look.w * detail);
        wall_col = mix(wall_col, wall_col * lintel_tone(era), lintel(pm, hs, era) * detail);
        var glazed = mix(interior, frame_col, look.y);
        // At range the pane is only its coverage, the way it always was.
        glazed = mix(glazed, mix(GLASS_BASE, plain * glow.z, 0.3), blur);
        // Boards cover the frame and the glass alike.
        glazed = mix(glazed, mix(boarded(pm, hs, r), BOARD_MEAN, blur), board_k);
        // The glass's share of the opening: none where it is boarded.
        let glass = pane * (1.0 - board_k);
        // A frame's inner edge is bevelled down to the glass: tilt the normal
        // there so it catches the light like a moulding rather than a stripe.
        let bevel = frame_bevel(pm, hs, era, shop_front) * detail * glass;
        let t3 = vec3<f32>(tangent.x, 0.0, tangent.y);
        pbr_input.N = normalize(pbr_input.N + t3 * bevel.x + vec3<f32>(0.0, bevel.y, 0.0));
        pbr_input.material.base_color = vec4<f32>(
            mix(wall_col, glazed, pane),
            pbr_input.material.base_color.a,
        );

        // …and at night it is lit from inside. The emissive is added rather than
        // replacing the surface, so the wall keeps taking the moon and the street.
        pbr_input.material.emissive += vec4<f32>(
            lit.rgb * (emissive * glow.x * glow.y),
            0.0,
        );

        // Glass is not plaster, and until this every square metre of every
        // building in the country carried one roughness. That is the flatness you
        // see on a facade at any hour: a window took the sun exactly as dully as
        // the wall around it, so a street of windows had no highlights in it at
        // all and the whole elevation read as one printed sheet.
        //
        // `pane` and not `inside`, deliberately — `pane` is the same
        // distance-blurred coverage the colour uses, so at range the windows
        // converge on the wall's own finish instead of leaving a grid of glints
        // sparkling across the far half of the city. That is the identical
        // argument the `blur` comment above makes for the emissive, and getting
        // it wrong here would undo it there.
        //
        // Reflectance rises with it: glass reflects appreciably more at normal
        // incidence than a rendered or brick wall, and it is that — not the
        // roughness alone — that puts the sky into the upper floors of a building
        // seen from across a square.
        // The *wall* between the windows is not one finish either, and until this
        // it was: one `perceptual_roughness: 0.95` on every square metre of every
        // building in the country, so a whole street took the sun identically and
        // the elevations read as printed card. Brick, render, stone panels and
        // painted plaster are not the same material and do not need to be told
        // apart by name to stop looking like one sheet — they need the highlight
        // to break up at facade scale.
        //
        // It reuses `r`, the lattice cell's own draw, rather than hashing again:
        // this is the largest geometry lane in the app and the question the whole
        // module answers is "what can a facade have for free". One multiply and a
        // `fract` decorrelate it from the lit-window decision it shares a hash
        // with, which is enough for a roughness nobody is meant to read as a
        // pattern. The cell is roughly a window bay, which is about the scale real
        // facades change finish at — a panel, a bay, a stone course.
        //
        // Faded out by `blur` for the same reason the emissive and the glass
        // roughness are: a per-cell value that survives below a pixel is a
        // crawling sparkle across the far half of the city.
        let finish = fract(r * 7.13) - 0.5;
        pbr_input.material.perceptual_roughness = clamp(
            pbr_input.material.perceptual_roughness + WALL_FINISH_RANGE * finish * (1.0 - blur),
            0.0,
            1.0,
        );

        pbr_input.material.perceptual_roughness = mix(
            pbr_input.material.perceptual_roughness,
            GLASS_ROUGHNESS,
            glass,
        );
        pbr_input.material.reflectance = mix(
            pbr_input.material.reflectance,
            vec3<f32>(GLASS_REFLECTANCE),
            glass,
        );
    } else if n.y > 0.0 && !door_part {
        var tone = 1.0;
        if n.y < FLAT_ROOF_Y {
            // A pitched roof: courses of tiles, each course half a tile over
            // from the one above. `f` runs down the slope through one course,
            // so f ≈ 0 is where the course above laps over it and throws its
            // shadow; `g` runs across one tile, and its edges are the gaps.
            let course = floor(tile.y);
            let across = tile.x + 0.5 * course;
            let col = floor(across);
            let f = fract(tile.y);
            let g = fract(across);
            let lap = mix(0.3, 1.0, smoothstep(0.0, 0.25, f));
            let gap = mix(0.75, 1.0, smoothstep(0.0, 0.08, min(g, 1.0 - g)));
            // Every tile is its own firing: a few percent either way.
            let fired = 0.9 + 0.2 * hash_wrapped(vec2<f32>(col, course) + vec2<f32>(13.0, 7.0));
            let blur = clamp(tile_px * 2.5 - 0.3, 0.0, 1.0);
            tone = mix(lap * gap * fired, TILE_MEAN, blur);
            // …and the courses stand proud of the plane: a tile's face is a
            // little flatter than the roof it lies on, so it tilts the normal
            // back up the slope, most at the course's lower edge.
            let down3 = normalize(vec3<f32>(horiz.x, 0.0, horiz.y));
            let tilt = 0.35 * (smoothstep(0.0, 0.25, f) - 0.5) * (1.0 - blur);
            pbr_input.N = normalize(pbr_input.N - down3 * tilt);
        } else {
            // A flat roof: its finish follows from its measured tone, which is
            // what atlas's material classes are cut from too — dark is bitumen,
            // mid is gravel ballast, pale is membrane, and green is sedum.
            let c = pbr_input.material.base_color.rgb;
            let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
            let blur = clamp(grain_px * 1.6 - 0.35, 0.0, 1.0);
            let fine = value_noise(grain) - 0.5;
            let coarse = value_noise(grain * 0.07) - 0.5;
            // Blotches a few metres across: the one scale of a flat roof that
            // still reads from a few hundred metres up, so it keeps its own blur.
            let blotch = (value_noise(blotch_uv) - 0.5 + 0.5 * (value_noise(blotch_uv * 2.0) - 0.5))
                * (1.0 - clamp(blotch_px * 1.6 - 0.35, 0.0, 1.0));
            // The seams between rolls, along whichever world axis — a flat roof
            // records no direction for them. A thin raised bead, blurred out
            // with the grain.
            let seam_d = abs(fract(in.world_position.x / ROLL_M + 0.5) - 0.5) * ROLL_M;
            let seam = 1.0 - smoothstep(0.0, 0.03, seam_d);
            let dark = 1.0 - smoothstep(0.035, 0.07, luma);
            let pale = smoothstep(0.25, 0.4, luma);
            let green = smoothstep(1.05, 1.25, c.g / max(max(c.r, c.b), 1e-4));
            let gravel = (1.0 - dark) * (1.0 - pale) * (1.0 - green);
            let finish = gravel * 0.45 * fine
                + green * (0.6 * fine + 0.3 * coarse)
                + dark * (0.12 * seam + 0.1 * coarse)
                + pale * (0.08 * seam - 0.1 * coarse);
            tone = 1.0 + finish * (1.0 - blur)
                + blotch * (0.35 * gravel + 0.3 * dark - 0.25 * pale + 0.3 * green);
            // A membrane and wet-looking bitumen carry a sheen; ballast does not.
            pbr_input.material.perceptual_roughness = mix(
                pbr_input.material.perceptual_roughness,
                0.6,
                max(dark, pale) * 0.5,
            );
        }
        pbr_input.material.base_color = vec4<f32>(
            pbr_input.material.base_color.rgb * tone,
            pbr_input.material.base_color.a,
        );
    }

    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
