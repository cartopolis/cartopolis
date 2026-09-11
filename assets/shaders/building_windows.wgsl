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
    pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
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
// block is two instructions). y, z, w unused.
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

    // The ghosted building loses its **walls** and keeps its roof. That is not a
    // saving, it is the picture: a building whose roof went too would be a hole in
    // the street with a floor in it, and the roof standing over a glass shell is
    // what a cutaway looks like. `is_wall` is already the right question and is
    // already answered.
    if is_wall && in_ghost(in.world_position.xz) {
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

    // Index of the window this fragment is in, and where inside it we are.
    let cell = vec2<f32>(u / lattice.x, v / lattice.y);
    // How much of the lattice one pixel spans. Where that reaches a whole window
    // the grid is below the sampling rate and point-sampling it aliases — into
    // vertical streaks on a wall seen edge-on, and into a crawling sparkle at
    // distance.
    let px = max(fwidth(cell.x), fwidth(cell.y));

    if is_wall {
        let idx = floor(cell);
        let f = cell - idx;

        // The pane, centred in its cell. Written as a half-width test either side
        // of the middle so the window keeps its real size as the pitch changes.
        let half = vec2<f32>(lattice.z / lattice.x, lattice.w / lattice.y) * 0.5;
        let d = abs(f - vec2<f32>(0.5, 0.5));
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
        let occupancy = clamp(pbr_input.material.base_color.a, 0.0, 2.0);
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
        let tall_enough = f32(v > glow.w);

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

        // By day a window is darker glass than the wall around it. This is the
        // half that shows in every daylight capture, and it is deliberately
        // slight — the facades are pale cartographic tones and a black grid on
        // them would be the loudest thing on the street.
        pbr_input.material.base_color = vec4<f32>(
            pbr_input.material.base_color.rgb * mix(1.0, glow.z, pane),
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
            pane,
        );
        pbr_input.material.reflectance = mix(
            pbr_input.material.reflectance,
            vec3<f32>(GLASS_REFLECTANCE),
            pane,
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
