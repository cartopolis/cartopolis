// Water surface for the vector-tile geometry path (ADR-035, option B).
//
// The water polygons `vector_tiles::tessellate_surfaces` produces are *flat* —
// a canal is often two triangles, a lake a couple of dozen — so there is nothing
// to displace: every wave here lives in the **normal**, not in the vertex
// positions. That is also why this is an `ExtendedMaterial` over
// `StandardMaterial` rather than a standalone `Material`: the sun, the shadow
// cascades, the atmosphere's environment map and the fog are exactly what make
// water read as water, and all of them come for free from the standard PBR
// path. We replace the geometric normal, the roughness and a depth tint before
// handing the `PbrInput` back to Bevy, and add two terms Bevy has no notion of
// (crest scattering and foam) on top of the lit colour.
//
// The wave field is a sum of sharpened directional sines (a cheap Gerstner
// stand-in) evaluated in **map metres**, taken from UV0 rather than from
// `world_position`: under `big_space` the world origin jumps a whole grid cell
// as the camera moves, which would snap the phase of the entire sea at once.
// UV0 is baked per cell from the map anchor (`map_geometry::build_mesh`), so it
// only changes on a re-anchor, which rebuilds the meshes anyway.
//
// # What the octave fade is for, and why it now pays for itself
//
// Sub-pixel ripples are the classic specular-aliasing source: unfaded, a 20 cm
// wave a kilometre out flickers as white noise under any camera motion. Every
// octave therefore carries a distance fade that retires it once its wavelength
// drops below roughly a pixel.
//
// What the retired detail becomes is the part that changed. It used to be a
// hand-tuned lump of extra roughness (`surface.y * (1 - detail)`), which is the
// right *idea* — a surface whose wiggles are smaller than a pixel is a rougher
// surface — but the wrong number, because it was not derived from the wiggles
// that were actually dropped. It is now the **slope variance** of exactly the
// octaves the fade retired, folded into the GGX width the way LEAN/Toksvig
// mapping does it (`α² += σ²`). That has three consequences worth knowing:
//
// * The sun's glitter path — the long shimmering streak that stretches toward
//   the horizon on any real water — falls out of it, instead of being a hard
//   dot near the camera and noise beyond it.
// * The octave sum can be normalised so that `wave.x` is the field's **RMS
//   slope**, a physical quantity, which is why `water::sea_state` can take it
//   straight off the Cox–Munk wind relation rather than off taste.
// * A steeper sea is now safe. The old numbers were held down by aliasing; with
//   the aliasing answered by roughness rather than by flattening, the sea can be
//   as steep as the measurement says it is.
//
// The loop also **breaks** at the first fully-faded octave rather than running
// every one with the body switched off. Wavenumber only rises, so once one
// octave is past the cut every finer one is too, and the slope variance they
// would have contributed is a geometric tail that closes in one line. That is
// what pays for the extra fine octaves — but only past the crossover: near water
// costs up to 60% more octaves than it used to and far water costs less, with
// the break-even at a couple of hundred metres in a calm. `water-surface.md` has
// the table, and `Budget::water_octaves` is the knob if the trade is wrong.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::{view, lights, globals},
    // The module alias as well as the three items above: `fog` only exists when
    // the view has one, so it can only be named behind `#ifdef DISTANCE_FOG`, and
    // a conditional *item* import is a shape naga_oil is not asked for anywhere
    // else in bevy. Reaching it through the module is what bevy's own
    // `pbr_functions` does.
    mesh_view_bindings as view_bindings,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
}
#import cartopolis::water_common::{
    water_reflected, water_transmitted, sky_fallback, with_clouds, cloud_mirror_fade,
}
#import cartopolis::cloud_field as cf

// x: RMS slope of the wave field (see `water::sea_state`). y: base wavelength in
// metres. z: wave speed scalar (metres/second at the base wavelength).
// w: detail cut-off — larger retires the fine octaves closer to the camera.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> wave: vec4<f32>;

// x: base (micro) roughness, perceptual — what is left when every wave the field
// models has been resolved. y: crest sub-surface scattering strength.
// z: top-down depth darkening (0 = none). w: foam amount on the wave crests.
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var<uniform> surface: vec4<f32>;

// xy: unit wind direction the waves travel along. z: chop (0 = rolling swell,
// 1 = sharp wind chop — raises the crest exponent). w: how clear the sky is,
// 1 = cloudless, 0 = overcast (read only by the no-environment-map reflection).
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> wind: vec4<f32>;

// This cell's shore field placement. xy: the cell's north-west corner in the
// same anchor-metre frame as UV0. z: the cell's ground size in metres.
// w: metres a fully-saturated texel stands for.
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var<uniform> field: vec4<f32>;

// Distance from the nearest bank, rasterised per cell (`water_shore_field`).
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var shore_field: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var shore_sampler: sampler;

// The octave sum's shape, all of it derived on the CPU in `water::octave_norms`
// so the fragment pays no series arithmetic and the maths is unit-testable.
// x: how many octaves to run (the quality knob — see `Budget::water_octaves`).
// y: gradient normaliser, chosen so the summed slope has RMS `wave.x`.
// z: 1 / Σ amplitude — turns the surviving-amplitude tally into a 0..1 "how much
// detail is left" fraction. w: 1 / Σ amplitude over the short octaves only.
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var<uniform> octaves: vec4<f32>;

// Distance from the nearest *building*, in the same frame and at the same metre
// scale, a quarter of the resolution (`ShoreField::bank`). Only the
// bank-in-the-mirror term below reads it, and only to ask whether anything is
// actually standing where it is about to darken the water.
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var bank_field: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var bank_sampler: sampler;

// The ground cloud deck this frame (`sky::CLOUD_FIELD_BUFFER`) — one buffer every
// water material and every ground material shares, rewritten in place as the
// deck drifts. Read a field at a time, never whole (see `cf::Deck`).
@group(#{MATERIAL_BIND_GROUP}) @binding(109) var<storage, read> clouds: cf::Deck;

/// Distance from the bank over which a body of water is treated as sheltered.
/// A 12 m canal never gets more than 6 m from a wall and so keeps only a fine
/// ripple; the middle of a harbour basin gets the full wind sea. Without this
/// every canal in the city ran the same swell as the open IJ, which is the
/// single most artificial thing flat water can do.
const SHELTER_M: f32 = 30.0;

/// Steepness a fully sheltered strip keeps, as a fraction of the open-water sea.
/// Not zero: sheltered water is calm, not a mirror.
const SHELTER_FLOOR: f32 = 0.22;

/// Cells either side of the camera's cell that `map_geometry` keeps live — its
/// `CELL_RADIUS`, which a test there holds this to. The ring the surface fades
/// out toward is this many cells out from the camera's cell, on every side.
const RING_RADIUS_CELLS: f32 = 1.0;

/// Width of the band inside the ring's edge over which the surface's waves, and
/// its shore terms, give way to the flat far field the clipmap draws past it.
/// A third of a z14 cell: the camera's cell is always whole inside the ring, so
/// the band starts at least a kilometre out, where a ripple is a few pixels at
/// most, and it is wide enough that the retirement reads as distance rather than
/// as a line.
const RING_FADE_M: f32 = 500.0;

/// How fast the bed falls away from the bank: metres of depth per metre of
/// distance from it. There is no bathymetry in a vector tile, so distance from
/// the bank stands in for depth — which is also what the eye reads at the edge
/// of water. 0.12 puts a metre of water eight metres out, a gently shelving
/// lake shore; a quay-walled canal is steeper than that, and reads a little
/// clearer than it is.
const BED_SLOPE: f32 = 0.12;

/// Deepest the stand-in bed goes. Past it the bed is out of sight in every
/// channel (`WATER_EXTINCTION` × the shortest path takes it under 1 %), so the
/// water is exactly its deep colour and the painted-tile seam `WATER_ALBEDO` is
/// matched against is untouched.
const BED_MAX_DEPTH_M: f32 = 6.0;

/// Colour of the bed where it shows through: silt over sand, linear albedo.
/// Close to the pale bank beside it on purpose, so the water's edge reads as the
/// ground running on under the surface rather than as the end of a blue
/// polygon.
const BED_ALBEDO: vec3<f32> = vec3<f32>(0.20, 0.165, 0.10);

/// Extinction per metre of path, red / green / blue, for **inland** water.
///
/// Clear ocean runs about 0.45 / 0.08 / 0.03, which is why a tropical shallow
/// goes turquoise. Dutch lakes, rivers and canals are nothing like it: algae and
/// dissolved peat take the blue out almost as fast as the red, visibility is a
/// metre or two, and the colour that survives a little depth is green-brown —
/// so a bank here shelves from sand through olive into the deep water's own
/// colour, not through turquoise.
const WATER_EXTINCTION: vec3<f32> = vec3<f32>(0.80, 0.42, 0.62);

/// `1 / 1.333²`, for the refracted view angle under the surface.
const INV_IOR_SQ: f32 = 0.5628;

/// Height of the thing the bank field marks — a building, so two or three
/// storeys. It sets *how far along the mirror* the surface looks for one: the
/// reflected ray clears something this tall after `height · cot(elevation)`.
///
/// It is no longer a guess about what a bank has on it. It used to be, and that
/// was the whole defect: the probe asked the *shore* field, which can only say
/// "this is land", so every bank in the world was treated as carrying a storey
/// of quay. Over a city canal that is true and over a lake with a mown bank it
/// is not, and what it drew there was a rim of shadow following the shoreline —
/// most visible from a few hundred metres up, where the camera direction barely
/// changes across the frame and so the rim lands on the same side of every water
/// body at once and reads as a cast shadow from a raised bank.
const BANK_HEIGHT_M: f32 = 9.0;

/// Furthest the bank probe will look.
///
/// The reach is `BANK_HEIGHT_M · cot(elevation)`, so it grows without bound as the
/// view flattens: 31 m at 16° above the water, 129 m at 4°, half a kilometre at
/// 1°. This cap is what stops that, and it is set by the shallowest angle these
/// views actually use rather than by taste — at 45 m the grazing lake frames were
/// truncated to a third of their real reach and the far shore fell out of the
/// mirror exactly where it matters most.
///
/// A probe leaving the cell reads a clamped edge texel, so the effect switches
/// itself off out there; at 130 m against a ~1.5 km cell that is a rare edge.
const BANK_REACH_MAX_M: f32 = 130.0;

/// Metres of *building* distance over which "a bank is in the mirror" softens
/// back to open sky, so the band has an edge rather than a cut. A quay a dozen
/// metres behind where the ray comes down is half in the reflection.
const BANK_SOFT_M: f32 = 12.0;

/// What the mirror is worth where it is full of bank rather than sky. Not zero: a
/// sunlit brick quay is dark against the sky, not black.
const BANK_MIRROR: f32 = 0.28;

/// Half-width of the foam line against the bank, in metres, at full sea.
/// Must stay comfortably above one shore-field texel (~1.4 m at z14): the field
/// is the only thing that can place the line, and a band narrower than its own
/// sampling grid simply never shows (measured: no bright band at all at 2.6 m).
const SHORE_FOAM_M: f32 = 4.0;

const TAU: f32 = 6.2831853;

/// Per-octave amplitude roll-off, and its square (the roll-off of the octave's
/// slope *variance*, which is what the early-out's geometric tail sums).
///
/// A steeper roll-off (0.62 and below) lets the fundamental dominate, and one
/// dominant direction is exactly what reads as corduroy; 0.72 keeps the higher
/// octaves in the mix without letting the finest one turn the normal into noise.
const AMP_FALLOFF: f32 = 0.72;
const AMP_SQ_FALLOFF: f32 = 0.5184;

/// Wavelength ratio between neighbouring octaves.
const K_STEP: f32 = 1.83;

/// Golden angle between octave directions, so they never line up into a visible
/// grid — the giveaway of a stacked-sine ocean. Its sine and cosine are written
/// out because they are loop-invariant constants that a shader compiler is not
/// obliged to fold, and folding them by hand takes two transcendentals per
/// octave out of the inner loop.
const OCTAVE_TURN_COS: f32 = -0.7373667;
const OCTAVE_TURN_SIN: f32 = 0.6754927;

/// `1 / sqrt(K_STEP)`, which advances the deep-water phase speed from one octave
/// to the next by a multiply instead of a reciprocal square root.
const INV_SQRT_K_STEP: f32 = 0.7392213;

/// Phase each octave is offset by, per octave index.
///
/// Without it every octave — and every crest wobble — passes through zero phase
/// at `p == 0`, which is the map anchor, which is where the camera is. Eight
/// waves in phase at one point is a focus, and a 2026-09-10 capture over open
/// water has the standing rings to prove it: a bullseye of interference lobes
/// sitting in the middle of the lake and travelling with the anchor. An offset
/// that is not a rational multiple of anything else here scatters the zero
/// crossings instead. It costs one multiply-add.
const OCTAVE_PHASE: f32 = 11.937;

/// Octaves below this index are "the big crests"; the rest are the chop riding on
/// them. Foam asks for both (see the crest-and-chop mask in `fragment`).
const LO_OCTAVES: i32 = 2;

/// First octave counted into the fine-detail signal that breaks foam up. By here
/// the golden-angle turn has taken the octaves far enough out of alignment that
/// their sum reads as blotches rather than as stripes.
const FINE_FIRST: i32 = 3;

/// Mean square of one octave's shaped derivative, as a quadratic in the chop.
///
/// The crest sharpening used to be `pow(s, mix(2, 5, chop))`, which is two `pow`s
/// per octave — four transcendentals, more than the sines the wave itself is made
/// of — for a shape that is a blend of `s²` and `s⁵`. It is now literally that
/// blend, and both powers fall out of three multiplies off `s²`.
///
/// The catch is that the mean square of a blend is not the blend of the mean
/// squares: a straight lerp between the two endpoints is up to **8.5 % high** in
/// the middle of the range, which is a 4 % error in the RMS slope the surface
/// renders. So the curve is fitted instead — exact at both ends and within 1e-4
/// across, integrated numerically over `d/dφ mix(s², s⁵, b)`. This is what
/// converts octave amplitudes into a real slope variance, and therefore what
/// makes `wave.x` mean RMS slope; `water::octave_norms` carries the same three
/// coefficients on the CPU side.
const SLOPE_MS_A: f32 = 0.1563;
const SLOPE_MS_B: f32 = 0.0097;
const SLOPE_MS_C: f32 = 0.0658;

fn slope_mean_square(chop: f32) -> f32 {
    return SLOPE_MS_A + chop * (SLOPE_MS_B + chop * SLOPE_MS_C);
}

/// Colour of light that goes *into* a wave and comes back out toward the viewer,
/// normalised so its largest component is 1. Water absorbs the red end within
/// centimetres, so what emerges from a crest a few tens of centimetres thick is
/// green-blue. This is the term that makes a backlit wave look lit from within
/// rather than painted.
///
/// **It is applied as a fraction of the light already there, not as light added
/// to it**, and that is a correctness decision rather than a stylistic one. As an
/// additive radiance it was scaled by the sun's `color · illuminance · exposure`,
/// and at a low sun this scene lifts exposure faster than the beam is
/// extinguished — so the term grew as the sun set, which is backwards, and put a
/// lattice of blown-out cyan crests across the Paterswoldsemeer. Twice: the
/// second time at an eighth of the first strength, because the view that was
/// meant to prove it tamed was aimed 100° away from the sun and never fired it.
///
/// A multiplier cannot do that. It says "a crest is brighter and greener than the
/// water beside it, by at most `SCATTER_MAX`", which is true in any exposure and
/// bounded in all of them.
const SCATTER_TINT: vec3<f32> = vec3<f32>(0.14, 1.0, 0.81);

/// Ceiling on the crest lift, as a fraction of the water's own lit colour.
const SCATTER_MAX: f32 = 0.30;

/// Turns `surface.y`'s few per cent into that fraction.
const SCATTER_GAIN: f32 = 8.0;

/// How tightly the scattering hugs the direction straight away from the sun.
const SCATTER_POWER: f32 = 4.0;

/// Sun elevation, as a sine, at which crest scattering is strongest.
///
/// Light has to cross the *thickness* of a crest to come out of the far side, so
/// this is a low-sun effect and needs to say so: the first version gated only on
/// "the sun is up" and therefore ran at full strength at noon, when a wave is lit
/// from above and is opaque.
const SCATTER_PEAK_ELEVATION: f32 = 0.13;

/// Perceptual roughness of foam. Foam is a mat of bubbles: it has no specular
/// lobe worth the name and it scatters in every direction.
const FOAM_ROUGHNESS: f32 = 0.92;

struct Waves {
    /// ∂h/∂x, ∂h/∂z of the wave height field, already scaled to RMS `wave.x`.
    grad: vec2<f32>,
    /// The same, for the two long octaves alone — what steers the clouds in the
    /// mirror (see where `fragment` reflects them).
    grad_lo: vec2<f32>,
    /// Mean square of the slope the short octaves still carry — the part of the
    /// surface the cloud reflection takes as blur rather than as direction.
    slope_var_hi: f32,
    /// Normalised height of the two long octaves — the big crests.
    height_lo: f32,
    /// Normalised height of the short octaves — the chop riding on them.
    height_hi: f32,
    /// How much of the fine detail survived the distance fade, 0..1.
    detail: f32,
    /// Mean square of the slope the fade retired, as a 2D vector magnitude —
    /// the variance that has to come back as GGX width.
    slope_var: f32,
    /// Signed fine-scale texture, roughly −1..1, for breaking foam up. Zero once
    /// the fine octaves have faded out, which is also when foam is retired.
    fine: f32,
}

/// Evaluate the wave field at `p` (map metres) and time `t` (seconds).
///
/// `cut` is the per-metre frequency budget: an octave of angular frequency `k`
/// fades out as `k * cut` approaches 1. The caller folds view distance into it,
/// which is what keeps the far water from aliasing.
fn waves_at(p: vec2<f32>, t: f32, cut: f32, shelter: f32) -> Waves {
    var grad = vec2<f32>(0.0);
    var grad_lo = vec2<f32>(0.0);
    // Σ (surviving amplitude)² over the short octaves, the shape of their slope
    // variance before the RMS scale is applied — as `retired_sq` is for the fade.
    var short_sq = 0.0;
    // Long and short octaves are summed apart so foam can ask for *both* — see
    // the crest-and-chop mask in `fragment`.
    var height_lo = 0.0;
    var height_hi = 0.0;
    var visible = 0.0;
    // Σ (amplitude × the part of it the fade threw away)², i.e. the shape of the
    // retired slope variance before the RMS scale is applied.
    var retired_sq = 0.0;
    var fine = 0.0;
    var fine_weight = 0.0;

    var amp = 1.0;
    var k = TAU / max(wave.y, 0.5);
    // Advanced by a multiply each octave rather than a reciprocal square root.
    var inv_sqrt_k = inverseSqrt(k);
    let chop = clamp(wind.z, 0.0, 1.0);
    var dir = normalize(select(vec2<f32>(1.0, 0.0), wind.xy, length(wind.xy) > 1e-4));
    let count = i32(octaves.x);

    for (var i = 0; i < count; i += 1) {
        let fade = clamp(1.0 - k * cut, 0.0, 1.0);
        if fade <= 0.0 {
            // Wavenumber only rises, so every remaining octave is past the cut
            // too. Their variance is a geometric series in `AMP_SQ_FALLOFF`;
            // summing it here is what lets the loop stop instead of spinning
            // through a switched-off body for the far half of the screen.
            retired_sq += amp * amp
                * (1.0 - pow(AMP_SQ_FALLOFF, f32(count - i)))
                / (1.0 - AMP_SQ_FALLOFF);
            break;
        }
        // Deep-water dispersion: long waves outrun short ones (c = √(g/k)),
        // which is most of why a stack of sines reads as moving water at all
        // rather than as a rippling sheet.
        let speed = wave.z * inv_sqrt_k;
        // Crest wobble. A pure directional sine has crests that run dead
        // straight to the horizon, and a stack of them reads as corduroy —
        // the giveaway of a sine ocean. Bending each crest along its own
        // length with a long, slow wave costs one extra sin() and turns the
        // stripes into an interlocking cell pattern. The derivative of the
        // wobble is deliberately dropped: it is small next to the wave's own
        // gradient, and omitting it keeps this to one transcendental.
        let perp = vec2<f32>(-dir.y, dir.x);
        let along = dot(p, perp);
        // Per-octave wobble ratio, plus a second incommensurate wobble on
        // the fundamental only. One wobble at one ratio is itself periodic:
        // in the far field, where the distance fade has already retired the
        // short octaves, the fundamental's arcs line up into a visible
        // repeating scallop. The second term (worth one extra sin per pixel,
        // not per octave) beats against the first and breaks the tiling.
        //
        // The amplitudes here are deliberately under a radian. Phase modulation
        // this strong is a lens: at ±1.35 rad a crest wanders by a fifth of its
        // own wavelength, and where two octaves' wanderings agree the sum focuses
        // into the ellipse-shaped caustics the same capture shows. Enough bend to
        // break a straight crest, not enough to fold one over another.
        let offset = OCTAVE_PHASE * f32(i);
        var wobble = sin(along * k * (0.31 + 0.11 * f32(i)) + t * 0.27 + offset) * 0.7;
        if i == 0 {
            wobble += sin(along * k * 0.13 - t * 0.19) * 0.45;
        }
        let phase = dot(p, dir) * k + t * speed * k + wobble + offset;
        // 0..1 sine, sharpened into crests: a blend of s² (a rolling swell) and
        // s⁵ (a sharp wind chop). Three multiplies off s² give both powers and
        // both derivatives, where the `pow(s, mix(2, 5, chop))` this replaces cost
        // two `pow`s — four transcendentals — for the same family of shapes.
        // `d/dφ sⁿ = n·s^(n-1)·½cos φ`, and a blend differentiates term by term.
        let s = 0.5 + 0.5 * sin(phase);
        let s2 = s * s;
        let s4 = s2 * s2;
        let shaped = mix(s2, s4 * s, chop);
        let d_shaped = mix(2.0 * s, 5.0 * s4, chop) * 0.5 * cos(phase);
        let w = amp * fade;
        if i < LO_OCTAVES {
            height_lo += w * shaped;
        } else {
            height_hi += w * shaped;
        }
        if i >= FINE_FIRST {
            // A signed, blotchy field that rides along with the waves. Free —
            // `s` is already in hand — and it exists so foam breaks into patches
            // instead of painting the crest mask as a smooth ramp.
            fine += w * (s - 0.5) * 2.0;
            fine_weight += w;
        }
        // No `* k` here: `wave.x` is a *slope* (each octave's real-world
        // amplitude is that slope over k, so the k cancels out of the gradient).
        // With the k left in, the rendered slope was steepness/wavelength —
        // and `sea_state` raises steepness and wavelength together, so dead
        // calm to a gale changed the visible water by barely 2× and the sea
        // measured as a flat sheet (open-water luminance σ < 1/255).
        grad += dir * (w * d_shaped);
        if i < LO_OCTAVES {
            grad_lo += dir * (w * d_shaped);
        } else {
            short_sq += w * w;
        }
        visible += w;
        let lost = amp * (1.0 - fade);
        retired_sq += lost * lost;

        amp *= AMP_FALLOFF;
        k *= K_STEP;
        inv_sqrt_k *= INV_SQRT_K_STEP;
        dir = vec2<f32>(
            dir.x * OCTAVE_TURN_COS - dir.y * OCTAVE_TURN_SIN,
            dir.x * OCTAVE_TURN_SIN + dir.y * OCTAVE_TURN_COS,
        );
    }

    // One scale carries the whole field: `octaves.y` is 1/√(mean-square of a
    // single octave's derivative × Σ amplitude²), so the sum below comes out
    // with RMS magnitude `wave.x` — a real slope, in the units Cox–Munk reports.
    let slope_ms = slope_mean_square(chop);
    let scale = wave.x * shelter * octaves.y;

    var out: Waves;
    out.grad = grad * scale;
    out.grad_lo = grad_lo * scale;
    out.slope_var_hi = scale * scale * slope_ms * short_sq;
    // The long band's weight is two fixed octaves, so its normaliser is a plain
    // constant; the short band's depends on how many octaves are running, which
    // is why that one is supplied.
    out.height_lo = height_lo / (1.0 + AMP_FALLOFF);
    out.height_hi = height_hi * octaves.w;
    out.detail = visible * octaves.z;
    // Same scale as the gradient, squared: the variance the fade dropped, in the
    // same slope units the surviving gradient is in.
    out.slope_var = scale * scale * slope_ms * retired_sq;
    out.fine = fine / max(fine_weight, 1e-4);
    return out;
}

struct KeyLight {
    /// Direction *toward* the light.
    dir: vec3<f32>,
    /// Its colour times illuminance times exposure — the same pre-tonemap space
    /// every other term in this shader is in.
    rgb: vec3<f32>,
}

/// The brightest directional light and the direction toward it.
///
/// The scene runs two (sun and moon) and their order in the array is bevy's
/// extraction order, not ours, so "the sun" is "whichever is brighter" — which
/// is also the right answer for a moonlit sea. Capped at two because that is how
/// many this scene has; the array is fixed-size and the rest are dark.
fn key_light() -> KeyLight {
    var out: KeyLight;
    out.rgb = vec3<f32>(0.0);
    out.dir = vec3<f32>(0.0, 1.0, 0.0);
    var best = 0.0;
    let n = min(lights.n_directional_lights, 2u);
    for (var i = 0u; i < n; i += 1u) {
        let l = lights.directional_lights[i];
        let lum = dot(l.color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        if lum > best {
            best = lum;
            out.rgb = l.color.rgb;
            out.dir = l.direction_to_light;
        }
    }
    return out;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let to_view = view.world_position.xyz - in.world_position.xyz;
    let dist = length(to_view);
    let v = to_view / max(dist, 1e-4);

#ifdef VERTEX_UVS_A
    // Map metres from the anchor — stable while the floating origin moves.
    let p = in.uv;
#else
    let p = in.world_position.xz;
#endif

    // Distance to the nearest bank, in metres. UV0 is anchor metres, so the cell's
    // own corner and size turn it into this cell's 0..1 texture frame.
    let shore_uv = (p - field.xy) / max(field.z, 1.0);
    let shore_m = textureSample(shore_field, shore_sampler, shore_uv).r * field.w;

    // A canal is not a small sea: waves need fetch, and a strip of water with a
    // wall 6 m away has none. Damping the whole wave field by distance from the
    // bank is what makes the same shader serve a canal, a harbour basin and the
    // open river without three sets of numbers.
    // **Past the ring the flat map draws the water, so the surface has to arrive
    // there looking like it.** The clipmap tiles light their water as this
    // surface's far field — no waves, every octave's variance returned as
    // roughness (`ground_detail.wgsl`) — so the colour matches across the ring's
    // edge by construction. What cannot match is texture: at low altitude this
    // surface's ripples are still resolved at the edge, and the tile beyond it is
    // a smooth sheet — a hard square line of *texture* across open water (Deck,
    // 2026-09-26, 150 m over the IJsselmeer).
    //
    // So over the last `RING_FADE_M` inside the ring the wave detail is retired
    // early: the distance cut is divided by the fade, and at the edge every
    // octave is past it. That is the octave fade's own mechanism, so what it
    // leaves behind is exactly what the tile draws, not an approximation of it.
    // The shore terms go with it (shelter, the bed, the bank in the mirror),
    // because the tile has no shore field either.
    //
    // The ring is `map_geometry`'s: the `RING_RADIUS_CELLS` cells either side of
    // the camera's cell. It is found here rather than sent — this cell's corner
    // and size give the cell grid in the anchor-metre frame, and the offset
    // between that frame and render space at this fragment puts the camera in it.
    let cam = view.world_position.xz + (p - in.world_position.xz);
    let cell_m = max(field.z, 1.0);
    let cam_cell = field.xy + floor((cam - field.xy) / cell_m) * cell_m;
    let ring_lo = cam_cell - RING_RADIUS_CELLS * cell_m;
    let ring_hi = cam_cell + (RING_RADIUS_CELLS + 1.0) * cell_m;
    let inside = min(p - ring_lo, ring_hi - p);
    let ring = smoothstep(0.0, RING_FADE_M, min(inside.x, inside.y));

    let shelter = mix(
        1.0,
        mix(SHELTER_FLOOR, 1.0, smoothstep(0.0, SHELTER_M, shore_m)),
        ring,
    );

    // Still water runs no octaves at all. `wave.x` is zero whenever Map ▸ Water
    // shading is off — which is the *default* on the tier with the least fill to
    // spare, since the toggle follows `Budget::detail_layers` — and without this
    // the loop still summed eight sines per fragment to arrive at a gradient of
    // exactly zero. A flat sheet is the state of the surface, not an absence of
    // detail, so `detail` is 1: the shore's wet edge stays drawn and only the
    // waves that are not there stop being computed.
    var w: Waves;
    if wave.x > 0.0 {
        w = waves_at(p, globals.time, dist * wave.w / max(ring, 1e-4), shelter);
    } else {
        w.grad = vec2<f32>(0.0);
        w.grad_lo = vec2<f32>(0.0);
        w.slope_var_hi = 0.0;
        w.height_lo = 0.0;
        w.height_hi = 0.0;
        w.detail = 1.0;
        w.slope_var = 0.0;
        w.fine = 0.0;
    }

    // The surface is flat ground, so the geometric normal is +Y and the wave
    // normal can be built directly in world space — no tangent frame needed.
    let n = normalize(vec3<f32>(-w.grad.x, 1.0, -w.grad.y));
    pbr_input.N = n;
    pbr_input.world_normal = n;

    // The waves too small to resolve are not gone, they are *rough*. Fold their
    // slope variance into the GGX width the way LEAN/Toksvig mapping does
    // (α² += σ²) rather than adding a tuned constant: this is what turns the
    // sun's reflection into a glitter path that stretches to the horizon instead
    // of a hard dot surrounded by crawling noise. `surface.x` is what remains at
    // zero distance — the micro-roughness of water itself.
    let alpha_base = surface.x * surface.x;
    let alpha = sqrt(alpha_base * alpha_base + w.slope_var);
    pbr_input.material.perceptual_roughness = clamp(sqrt(alpha), 0.02, 1.0);

    // Schlick against water's own F0 (`water_common`, shared with the flat map's
    // water past the ring so the two cannot disagree about it).
    let n_dot_v = clamp(dot(n, v), 0.0, 1.0);
    let reflected = water_reflected(n_dot_v);

    // **Light that was not reflected is light that went in**, so the body colour
    // is worth `1 - F` and nothing else. This used to be the other way round —
    // the albedo was *brightened* toward full at grazing angles — and that is a
    // flat, chalky wash laid over exactly the part of the surface where a real
    // sheet of water stops having a colour of its own and becomes a picture of
    // the sky. It is the single most "computer graphics" thing the old surface
    // did, and it was hiding the reflection underneath it.
    //
    // Scaled so the **head-on tone is unchanged to the last bit**: `surface.z` is
    // still what the depth takes out of the albedo looking straight down, which
    // is the number `vector_tiles::palette::WATER` was matched against (see
    // `WATER_ALBEDO`). Only the grazing end moves, and it moves onto the sky.
    let transmitted = water_transmitted(reflected);
    let deep = pbr_input.material.base_color.rgb * (1.0 - clamp(surface.z, 0.0, 1.0));

    // **Transparency without a transparent pass.** In the shallows the bed shows
    // through: light goes down to it, is reflected by it, and comes back up,
    // losing each channel to Beer–Lambert on the way. Blending the surface for
    // real would move water into the transmissive pass (the coplanar-ground
    // ordering this tree keeps being bitten by) and still need a depth prepass
    // it no longer has to know how thick the water is; the shore field already
    // says how far out the bank is, and that is the depth proxy.
    //
    // The path is down at roughly the sun's steepness and up along the
    // *refracted* view ray, which is why a grazing look into shallow water sees
    // less of the bed than one straight down. Refraction bounds it: under water
    // the view ray is never flatter than ~41°, so the path cannot run away.
    let depth_m = min(shore_m * BED_SLOPE, BED_MAX_DEPTH_M);
    let cos_under = sqrt(1.0 - (1.0 - n_dot_v * n_dot_v) * INV_IOR_SQ);
    let bed = exp(-WATER_EXTINCTION * (depth_m * (1.0 + 1.0 / cos_under)));
    // Faded with the ring, like the waves: the tile past it has no bed to show.
    let body_rgb = mix(deep, BED_ALBEDO, bed * ring) * transmitted;

    // **What a canal actually looks like is the far bank upside down in it.**
    // There are no scene reflections here and there cannot cheaply be — but the
    // dominant term is not the detail of the reflection, it is that the mirror is
    // *dark* where it is full of quay and bright where it is full of sky, and the
    // bank field already knows where the buildings are.
    //
    // The reflected ray leaves horizontally away from the camera and climbs at the
    // angle the view descends, so it clears a bank-side building after
    // `height · cot(elevation)`. Probe the **bank** field that far along it: it
    // seeds buildings at zero, so a near-zero reading there means the mirror is
    // looking at a wall.
    //
    // It has to be the bank field and not the shore field, and that is the whole
    // of this fix. The shore field can only answer "land", which made the term
    // assert that a storey of quay stands on every bank there is — right over a
    // canal, wrong over a lake, and what it drew over the lake was a rim of
    // shadow hugging the shoreline. See `BANK_HEIGHT_M`.
    //
    // This is view-dependent for free, which is half of why it works. Water by the
    // *near* bank probes across the water and sees sky; water by the far bank
    // probes onto whatever is behind it. Looking straight down the reach goes to
    // zero and the effect switches itself off, which is correct — straight down
    // the mirror holds the zenith, not the quay.
    let rise = max(n_dot_v, 0.02);
    let horiz = sqrt(max(1.0 - rise * rise, 0.0));
    let reach = min(BANK_HEIGHT_M * horiz / rise, BANK_REACH_MAX_M);
    let away = normalize(select(vec2<f32>(1.0, 0.0), -v.xz, length(v.xz) > 1e-4));
    let bank_uv = (p + away * reach - field.xy) / max(field.z, 1.0);
    // Outside this cell the sampler clamps to an edge texel, which is an answer
    // about the wrong place and would draw a seam along every cell boundary. Off
    // the edge the effect simply does not apply.
    var mirror = 1.0;
    if all(bank_uv > vec2<f32>(0.0)) && all(bank_uv < vec2<f32>(1.0)) {
        let ahead = textureSample(bank_field, bank_sampler, bank_uv).r * field.w;
        mirror = mix(BANK_MIRROR, 1.0, smoothstep(0.0, BANK_SOFT_M, ahead));
    }
    // And the bank in the mirror, which the tile past the ring has no field for.
    mirror = mix(1.0, mirror, ring);
    // `F0 = 0.16 · reflectance²`, so a mirror worth `m` is a reflectance scaled by
    // its square root. Dimming F0 rather than the lit colour is what keeps the sun
    // glint out of a reflection that is looking at a wall — a building blocks the
    // sun's image as surely as it blocks the sky's.
    pbr_input.material.reflectance *= sqrt(mirror);

    pbr_input.material.base_color = vec4<f32>(
        body_rgb,
        pbr_input.material.base_color.a,
    );

    // Whitecaps break where the chop is riding the top of a big crest, so the
    // mask is the product of the two bands. A threshold on the summed height
    // alone paints continuous ribbons down every crest line — foam as a stripe,
    // not as spray. The bands sit well below 1 on purpose: each is a mean of
    // sharpened sines, so its peaks land nearer 0.7 than unity.
    //
    // `w.fine` then tears the result up. Foam is a mat of bubbles with holes in
    // it, and a smoothstep of a smooth field is a smooth blob — which is what
    // makes painted-on foam read as painted on. The breakup rides the same wave
    // field, so it moves with the crest instead of crawling over it.
    // Clamped at 1: the breakup may tear holes in foam, never brighten it. An
    // earlier version allowed 1.4 and so quietly raised the foam ceiling above
    // what `sea_state` asked for, which is not a thing a *breakup* should do.
    let breakup = clamp(0.55 + 0.75 * w.fine, 0.0, 1.0);
    let crest_foam = surface.w
        * w.detail
        * breakup
        * smoothstep(0.34, 0.62, w.height_lo)
        * smoothstep(0.40, 0.80, w.height_hi);

    // The waterline. A hard edge between water and bank is the tell that a river
    // is a blue polygon; real water leaves a bright, restless line where it meets
    // the land. The band's width breathes with the local wave height, so it laps
    // in and out along the bank instead of tracing a constant offset, and it
    // survives a dead calm (a wet edge is always there) while growing with the
    // sea. `w.detail` retires it with the rest of the fine detail at distance.
    // Clamped to two shore-field texels: below that the band falls between the
    // samples of the very field that draws it, and no wind setting can help.
    let texel_m = field.z / f32(textureDimensions(shore_field).x);
    let lap = max(
        SHORE_FOAM_M * (0.55 + 0.45 * wind.z) * (0.55 + 0.9 * w.height_hi),
        2.0 * texel_m,
    );
    let shore_foam = w.detail
        * (0.30 + 0.55 * wind.z)
        * clamp(0.75 + 0.4 * w.fine, 0.0, 1.0)
        * (1.0 - smoothstep(0.0, lap, shore_m));

    let foam = clamp(max(crest_foam, shore_foam), 0.0, 1.0);
    pbr_input.material.base_color = vec4<f32>(
        mix(pbr_input.material.base_color.rgb, vec3<f32>(1.0), foam),
        pbr_input.material.base_color.a,
    );
    pbr_input.material.perceptual_roughness = mix(
        pbr_input.material.perceptual_roughness,
        FOAM_ROUGHNESS,
        foam,
    );

    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }

    let key = key_light();

#ifndef ENVIRONMENT_MAP
#ifdef DISTANCE_FOG
    // No environment map on this view, so the standard path has *no* reflection
    // to give the surface at all — which is what Android's `balanced` rung renders
    // in (`sky_ibl` off), as do the laptop and any run with sky lighting switched
    // off. Water with nothing in it is the one thing water can never look like, so
    // `water_common::sky_fallback` stands in with a two-stop sky built on the haze
    // colour the ground already fades into, and gives it the ambient's reflection
    // where the sky is open. It carries the argument and the measurements; the
    // flat map's water past the ring (`ground_detail.wgsl`) calls the same thing.
    //
    // Deliberately not compiled at all when there *is* an environment map: this
    // would be a second reflection stacked on the real one. Weighted by the *same*
    // `reflected` the body colour was taken out of, so the two halves of the
    // surface still add up to one; foam keeps the ambient, being a rough white mat
    // and not a mirror. Only the cloud cover is sent (`wind.w`), because an
    // overcast sky has no blue in it.
    out.color = vec4<f32>(
        sky_fallback(
            out.color.rgb,
            pbr_input,
            view_bindings::fog.base_color.rgb,
            wind.w,
            reflect(-v, n).y,
            reflected,
            mirror * (1.0 - foam),
            foam,
        ),
        out.color.a,
    );
#endif
#endif

    // **The clouds, in the mirror.** Neither the environment map nor the stand-in
    // sky above has any: the deck is a shell drawn over the sky after both, so a
    // lake under a cloudy sky reflected a clear one. The deck is found again along
    // the reflected ray — the same field, the same light, `cf::mirrored` — and put
    // in front of whichever sky the reflection holds, at the weight that sky went
    // in with (`water_common::with_clouds`). Where the bank is in the mirror the
    // cloud is behind the quay, so `mirror` dims it with the sky.
    //
    // Not faded toward the ring's edge the way the shore terms are: the clipmap's
    // water past it mirrors the same deck (`ground_detail.wgsl`), at the same
    // far-field roughness, so the clouds run on across the edge.
    //
    // **Only the long waves steer the clouds; the short ones blur them.** A
    // ripple tilts the reflected ray by a tenth of a radian, which kilometres
    // up is a different cloud — so the full wave normal sampled the deck at
    // random from one pixel to the next and drew a breeze-blown lake as grain
    // (Deck, 2026-09-29). The sky's own reflection gets away with the same
    // normal because the sky barely changes over a tenth of a radian; a cloud
    // edge changes completely. So the cloud ray bends on the two long octaves
    // alone — a big swell still wobbles the reflection, as it does on real
    // water — and the short octaves' slope variance joins the roughness the
    // deck is blurred by. The footprint on the deck is then taken of that
    // smooth ray, here in uniform control flow (see `cf::mirrored`).
    let cloud_normal = normalize(vec3<f32>(-w.grad_lo.x, 1.0, -w.grad_lo.y));
    let cloud_roughness = sqrt(sqrt(alpha * alpha + w.slope_var_hi));
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
    let offset = in.world_position.xyz - view.world_position.xyz;
    let mirror_ray = reflect(-v, cloud_normal);
    let spread = length(fwidth(cf::mirror_hit(deck.shape.w, offset, mirror_ray)));
    // Not looked up at all where the water reflects too little of it to show
    // (`water_common::cloud_mirror_fade`).
    let cloud_fade = cloud_mirror_fade(reflected * mirror * (1.0 - foam));
    var cloud = vec4<f32>(0.0);
    if cloud_fade > 0.0 {
        cloud = cf::mirrored(deck, offset, mirror_ray, dist, cloud_roughness, spread);
        cloud.a *= cloud_fade;
    }
    out.color = vec4<f32>(
        with_clouds(out.color.rgb, pbr_input, cloud, wind.w, reflected, mirror * (1.0 - foam)),
        out.color.a,
    );

    // Light that goes into the back of a crest, scatters, and comes out the front
    // toward the viewer — the translucent green that makes a backlit wave look
    // lit from inside. Three gates, all cheap: it needs a crest to shine through
    // (`height_lo`), the viewer roughly opposite the sun, and the sun above the
    // horizon. Foam kills it, being opaque.
    let toward_sun = clamp(dot(v, -key.dir), 0.0, 1.0);
    // Up over the first few degrees of elevation, down again as the sun climbs:
    // a crest is translucent when the light crosses it and opaque when the light
    // comes from over the top of it.
    let elevation = clamp(key.dir.y, 0.0, 1.0);
    let sun_low = smoothstep(0.0, SCATTER_PEAK_ELEVATION, elevation)
        * (1.0 - smoothstep(SCATTER_PEAK_ELEVATION, 0.75, elevation));
    let scatter = surface.y
        * w.detail
        * pow(toward_sun, SCATTER_POWER)
        // Weighted toward the tops of the big crests, but as a ramp rather than a
        // band: a narrow threshold on a smooth field draws the wave field's own
        // lattice, which is a diamond grid and reads as neon rather than as water.
        * smoothstep(0.30, 1.00, w.height_lo)
        * sun_low
        * (1.0 - foam);
    let glow = clamp(scatter * SCATTER_GAIN, 0.0, SCATTER_MAX);
    out.color = vec4<f32>(
        out.color.rgb * (vec3<f32>(1.0) + SCATTER_TINT * glow),
        out.color.a,
    );

    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
