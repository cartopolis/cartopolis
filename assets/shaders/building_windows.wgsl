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

/// A normal this far off horizontal is a wall. Roofs — including the steep
/// pitched ones 3DBAG actually carries — stay above it and get nothing.
const WALL_NORMAL_Y: f32 = 0.42;

/// The window hash repeats on this many columns and floors. It has to divide the
/// index step a floating-origin jump produces (10,000 / pitch), which is what
/// keeps the pattern stable — see the module note. 32 divides both 4,000 and
/// 3,200, and 32 windows is ~80 m, wider than almost any single facade.
const HASH_PERIOD: f32 = 32.0;

/// Cheap 2D value hash. Not for cryptography and not for a large domain — it is
/// asked for one number per window per frame.
fn hash2(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
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
