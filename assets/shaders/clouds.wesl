// Procedural cloud ceiling: two camera-centred decks, projected onto flat layers
// above the camera and shaded with a *scattering* model rather than a surface one.
//
// The geometry is unchanged — one static shell mesh, no churn, so it stays clear
// of the iGPU command-buffer budget (see `docs/notes/upload-bursts.md`) — and each
// above-the-horizon view ray is still projected onto a cloud plane and sampled for
// coverage there.
// What changed is the lighting, which is the whole reason a projected plane can
// read as cloud at all while a Lambert-shaded noise field reads as painted
// geometry (`docs/notes/cloud-lighting.md` has the derivation and the sources):
//
//   * light is scattered, not reflected — a Henyey-Greenstein dual lobe makes the
//     sun-side rim glow (the silver lining) and adds a weaker backscatter lobe;
//   * the sun is occluded by a short march *through the deck toward the sun*, not
//     by a fake surface normal built from the density gradient;
//   * a Beer-Powder term darkens thin edges when the sun is behind the camera,
//     which is what makes a cloud read as thick rather than as fog;
//   * three scattering octaves keep thick bases luminous grey instead of black;
//   * light that has scattered many times diffuses through the column — the sun's
//     and the blue sky's above it — so a base is bright, smooth and sky-tinted
//     rather than a dark grey step behind a lit rim;
//   * and the deck recedes into the horizon haze by aerial perspective instead of
//     fading its alpha to nothing — the sky used to visibly *end* at the horizon.
//
// The two decks sit at different heights, so translating the camera slides them
// past each other. That parallax is the one cue a single field can never have.
//
// The field itself — the noise, the shape, the churn, the phase functions and the
// sunlight — lives in `cloud_field.wgsl`, because water reflects this deck and
// must find the same clouds in the same places.
//
// World-anchoring: `params2.xy` is the camera's horizontal displacement from the
// map anchor in **metres**, which keeps the clouds fixed over the ground so you
// fly under them. The wind's drift arrives already integrated (`params1.xy`,
// metres): the live wind changes speed and direction, and a velocity times the
// clock would jump the whole field every time it did.
import bevy_pbr::render::forward_io::VertexOutput;
import bevy_pbr::render::mesh_view_bindings::view;
import super::cloud_field as cf;

// x: coverage 0..1 (fraction of sky filled). y: edge softness. z: the sky's clock
// in seconds (drives the churn). w: low deck height above the camera, in metres.
// → `deck.shape`, read as `params0` in `fragment`.
// xy: the wind's accumulated drift in metres. z: space fade (1 = fully hidden in
// orbit). w: altitude fade (1 = full clouds, 0 = faded out at high flight).
// → `deck.motion`, read as `params1` in `fragment`.
// xyz: world direction TO the sun. w: deck thickness in metres (the vertical
// extent the sun march traverses).
// → `deck.sun_dir`, read as `sun_dir` in `fragment`.
// rgb: sunlight reaching the deck's altitude by `utils::sun`'s model — the
// stand-in for a view with no bevy atmosphere (see `cf::deck_sunlight`). a: overall
// cloud opacity, before the coverage/altitude/space fades.
// → `deck.sun_color`, read as `sun_color` in `fragment`.
// rgb: ambient sky light falling on the deck (what fills the shadowed side).
// a: opacity scale for the high deck, so the cirrus layer can be tuned alone.
// → `deck.sky_color`, read as `sky_color` in `fragment`.
// xy: world-anchor offset in metres. z: the camera fog's per-metre density, for
// aerial perspective. w: high deck height above the camera, in metres.
// → `deck.place`, read as `params2` in `fragment`.
// rgb: the colour the deck recedes into with distance — the same fog colour the
// horizon-haze shell paints, so cloud and sky reach the horizon together. It is
// also what `sky_color` is now taken from, which is what stops an overcast deck
// from meeting the horizon in a step (see `update_clouds`).
// a: the fraction of `sky_color` an opaque core still receives — low for scattered
// cumulus (a dark base against a bright sky), high for overcast (the deck *is*
// the sky). Rust decides it from the coverage; see `cloud_sky_floor`.
// → `deck.haze_color`, read as `haze_color` in `fragment`.
// x: the sunlight's scale apart from its colour — the sun disc's occlusion through
// sunrise and sunset times the deck's gain — for the atmosphere path of
// `cf::deck_sunlight`. y: 1 on the cheap tier, for `cf::mirrored`; this shader
// reads `CLOUDS_LOW` instead. z: the strength of the clear sky above the deck,
// as light on its top (`cf::SKY_ABOVE_TINT` is its colour). w: unused.
// → `deck.light`, read as `params3` in `fragment`.
//
// One uniform rather than eight: WebGPU allows 12 uniform buffers per shader
// stage, bevy's view bindings already spend some, and eight more made this
// pipeline invalid in every browser — the renderer halted the moment the deck
// was first drawn. `cf::Deck` is the layout `CloudMaterial` writes
// (`CloudFieldGpu`), the same one the water reads its mirrored deck from.
@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> deck: cf::Deck;

// ---- quality tier -----------------------------------------------------------
// `CLOUDS_LOW` is set by `CloudMaterial::specialize` on `quality::Tier::Phone`.
//
// This shader was the most expensive fragment program the phone ran and the only
// one with no tier knob at all: 14 FBM octaves before the alpha bail — ~56 integer
// hashes per pixel over the whole upper half of the screen — and the march past it
// adds nine more. Its one existing quality control is the *distance* LOD below,
// which is about aliasing rather than cost.
//
// What the low tier gives up is structure, not weather: half the octaves in each
// deck, no domain warp (the curl that makes a billow lean), a single-tap sun march
// and one fewer multiple-scattering octave. It reads as a flatter, softer sky,
// which on a 5.7" panel is most of what a 60 km cloud deck reads as anyway.
//
// The low deck's octave counts are `cf::low_octaves` / `cf::low_billow`, picked
// by `CHEAP`: the water mirroring this deck needs the same numbers without the
// shader def, so they live in the shared module.
@if(CLOUDS_LOW)
const CHEAP: bool = true;
@else
const CHEAP: bool = false;
@if(CLOUDS_LOW)
const OCT_HIGH: i32 = 2;
@else
const OCT_HIGH: i32 = 4;
// Taps in the sun march (`cf::sun_optical_depth`). Three is enough because the
// field it marches is low-frequency; each tap costs a whole (small) FBM.
@if(CLOUDS_LOW)
const SUN_STEPS: i32 = 1;
@else
const SUN_STEPS: i32 = 3;
@if(CLOUDS_LOW)
const MS_OCTAVES: i32 = 2;
@else
const MS_OCTAVES: i32 = 3;
// Steps the view ray takes climbing through the low deck's relief, and the
// octaves of the field each one reads. Each step is a coarse fbm, so this is the
// relief's whole cost; the march stops at the first cloud it enters.
@if(CLOUDS_LOW)
const RELIEF_STEPS: i32 = 4;
@else
const RELIEF_STEPS: i32 = 8;
@if(CLOUDS_LOW)
const MARCH_OCTAVES: i32 = 2;
@else
const MARCH_OCTAVES: i32 = cf::RELIEF_MARCH_OCTAVES;
// Secant steps refining where the ray crosses into a cloud (see the march).
@if(CLOUDS_LOW)
const SECANT_STEPS: i32 = 1;
@else
const SECANT_STEPS: i32 = 2;
// How far into a cloud, as a fraction of the deck's thickness, its opacity and
// thickness are read: one march step, so a ray that only clips a top is past
// it there.
// Optical depth, along the view ray, of a column the deck's whole thickness
// tall seen straight up (see the opacity). 4 makes a column a tenth of the deck
// (90 m) about 30 % opaque overhead and one a third of it (300 m) 70 %.
const VIEW_DEPTH_LOW: f32 = 4.0;
@if(CLOUDS_LOW)
const READ_DEPTH: f32 = 0.25;
@else
const READ_DEPTH: f32 = 0.125;

// How far the coarse field stands above the relief's surface at height `h`
// (0..1) up the deck along the view ray: positive inside a cloud.
fn relief_excess(
    dir: vec3<f32>,
    h: f32,
    t_base: f32,
    t_top: f32,
    anchor_m: vec2<f32>,
    drift_m: vec2<f32>,
    warp_off: vec2<f32>,
    thresh: f32,
    span: f32,
    octaves: i32,
    coarse: f32,
    evolve: vec2<f32>,
) -> f32 {
    let uv = cf::low_uv(dir.xz * mix(t_base, t_top, h), anchor_m, drift_m) + warp_off;
    return cf::relief_field(uv, octaves, coarse, evolve, 0.0)
        - cf::relief_floor(h, thresh, span);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // The eight vectors under the names the comments above give them.
    let params0 = deck.shape;
    let params1 = deck.motion;
    let sun_dir = deck.sun_dir;
    let sun_color = deck.sun_color;
    let sky_color = deck.sky_color;
    let params2 = deck.place;
    let haze_color = deck.haze_color;
    let params3 = deck.light;

    let dir = normalize(in.world_position.xyz - view.world_position.xyz);

    // Only rays that climb toward the ceiling carry cloud.
    if dir.y <= cf::GRAZE_MIN {
        discard;
    }

    let cover = params0.x;
    let soft = max(params0.y, 1e-3);
    let thresh = cf::low_threshold(cover, CHEAP);
    let clock = params0.z;
    let evolve = cf::EVOLVE_DETAIL * clock;
    // The sample point is absolute world position: camera-relative projection plus
    // the camera's own displacement from the anchor, both in metres, and the wind's
    // drift on top.
    let anchor_m = params2.xy;
    let drift_m = params1.xy;

    // ---- low deck, in relief ----------------------------------------------
    // Climb the view ray through the slab from the deck's base to its top, on
    // the coarse field, until it is inside a column (`cf::relief_column`): at the
    // first step that is a base, after it a side. The crossing is placed between
    // the last two steps by linear interpolation, as parallax occlusion mapping
    // places it, so a surface is smooth rather than terraced by the step count.
    let octaves = cf::low_octaves(CHEAP);
    let span = cf::relief_span(cover);
    let thick_m = sun_dir.w;
    let t_base = params0.w / dir.y;
    let t_top = (params0.w + thick_m) / dir.y;
    let detail_low = cf::detail_at(t_base);
    let coarse = cf::detail_keeping(MARCH_OCTAVES, octaves, detail_low);
    // The warp is low-frequency — a cloud's lean, not its edge — so it is taken
    // once, where the ray meets the base, and moves every step alike. Per step it
    // would cost two more fbms each.
    let uv_base = cf::low_uv(dir.xz * t_base, anchor_m, drift_m);
    let warp_off = cf::low_warped(uv_base, clock, detail_low) - uv_base;
    var h_hit = -1.0;
    var prev_h = 0.0;
    var prev_e = -1.0;
    for (var i = 0; i <= RELIEF_STEPS; i = i + 1) {
        let h = f32(i) / f32(RELIEF_STEPS);
        let e = relief_excess(dir, h, t_base, t_top, anchor_m, drift_m, warp_off, thresh, span, octaves, coarse, evolve);
        if e > 0.0 {
            if i == 0 {
                h_hit = 0.0;
            } else {
                // Two secant steps on the crossing. Which step first lands inside
                // changes from one pixel to the next across a cloud's side, and a
                // single interpolation is off by a different amount on each
                // side of that change, which drew the side as contour lines.
                var lo_h = prev_h;
                var lo_e = prev_e;
                var hi_h = h;
                var hi_e = e;
                for (var k = 0; k < SECANT_STEPS; k = k + 1) {
                    let mid_h = lo_h + (hi_h - lo_h) * lo_e / (lo_e - hi_e);
                    let mid_e = relief_excess(dir, mid_h, t_base, t_top, anchor_m, drift_m, warp_off, thresh, span, octaves, coarse, evolve);
                    if mid_e > 0.0 {
                        hi_h = mid_h;
                        hi_e = mid_e;
                    } else {
                        lo_h = mid_h;
                        lo_e = mid_e;
                    }
                }
                h_hit = lo_h + (hi_h - lo_h) * lo_e / (lo_e - hi_e);
            }
            break;
        }
        prev_h = h;
        prev_e = e;
    }
    let h_low = max(h_hit, 0.0);
    let t_low = mix(t_base, t_top, h_low);
    let uv_low = cf::low_uv(dir.xz * t_low, anchor_m, drift_m) + warp_off;
    // The fine octaves, once, a little inside the surface — at the crossing the
    // coarse field is exactly at the threshold, half in and half out by
    // construction. They fray the edge the coarse field put there (a ray that
    // only clips a column's top comes out of it again, and reads thin) and set
    // the column's thickness. A ray that met no cloud skips them.
    var shape_low = 0.0;
    var column_low = 0.0;
    if h_hit >= 0.0 {
        let h_read = min(h_low + READ_DEPTH, 1.0);
        let uv_read = cf::low_uv(dir.xz * mix(t_base, t_top, h_read), anchor_m, drift_m) + warp_off;
        let v_low = cf::relief_field(uv_read, octaves, detail_low, evolve, 0.0);
        shape_low = smoothstep(-cf::RELIEF_SOFT, cf::RELIEF_SOFT, v_low - cf::relief_floor(h_read, thresh, span));
        column_low = cf::relief_column(v_low, thresh, span, cover);
    }

    // ---- high deck --------------------------------------------------------
    let t_high = params2.w / dir.y;
    let uv_high = (dir.xz * t_high + anchor_m + drift_m * cf::HIGH_WIND_SCALE) * cf::FREQ_HIGH;
    let detail_high = cf::detail_at(t_high);
    let thresh_high = 1.0 - cover * cf::HIGH_COVER_SCALE;
    // Cirrus streaks churn too, a little slower than the deck below them.
    let shape_high = smoothstep(
        thresh_high,
        thresh_high + soft,
        cf::fbm(uv_high, OCT_HIGH, detail_high, evolve * 0.7),
    );

    // ---- opacity ----------------------------------------------------------
    // The low deck's opacity is Beer's law on the cloud the ray crosses: the
    // column's thickness where it is read, lengthened by the slant of the ray.
    // A thick core hides the sun completely, which the sheet's capped opacity
    // never did; a column only a few tens of metres thick — a cloud's fringe,
    // a small young puff — lets the sky through. Before this every cloud was
    // solid to its last pixel. The edge itself still fades over `RELIEF_SOFT`.
    // The high deck keeps its own Beer's law: cirrus is thin.
    let od_high = shape_high * cf::OPTICAL_DEPTH_HIGH;

    let graze = smoothstep(cf::GRAZE_MIN, cf::GRAZE_MAX, dir.y);
    let fade = sun_color.a * params1.w * (1.0 - params1.z) * graze;
    let crossed = column_low / max(dir.y, 0.25);
    // A closed deck is stratocumulus, optically deep everywhere: its thinnest
    // places must still hide the sun disc.
    let view_depth = VIEW_DEPTH_LOW * mix(1.0, 5.0, smoothstep(0.75, 1.0, cover));
    let a_low = shape_low * (1.0 - exp(-crossed * view_depth)) * fade;
    let a_high = (1.0 - exp(-od_high)) * sky_color.a * fade;
    let alpha = a_low + a_high * (1.0 - a_low);
    // Bail before the march: on a part-covered sky this is most of the screen, and
    // everything below here is the expensive half of the shader.
    if alpha <= 0.002 {
        discard;
    }

    // ---- lighting ---------------------------------------------------------
    let sun = normalize(sun_dir.xyz);
    let cos_theta = dot(dir, sun);
    // Each deck is lit by the sun as it reaches *that* deck: the high one sees
    // less air above it, so it stays whiter longer as the sun goes down.
    let sun_low_rgb = cf::deck_sunlight(sun, params0.w, sun_color.rgb, params3.x);
    let sun_high_rgb = cf::deck_sunlight(sun, params2.w, sun_color.rgb, params3.x);

    let sun_od = cf::relief_sun_od(
        uv_low,
        h_low,
        sun,
        thick_m,
        thresh,
        span,
        coarse,
        evolve,
        octaves,
        SUN_STEPS,
    );
    let low_rgb = cf::low_deck_light(
        column_low,
        max(column_low - h_low, 0.0),
        sun_od,
        cos_theta,
        sun,
        sun_low_rgb,
        cf::SKY_ABOVE_TINT * params3.z,
        sky_color.rgb,
        haze_color.a,
        MS_OCTAVES,
    );

    // The high deck is thin enough that self-shadowing is not a thing it does:
    // phase function and ambient only, no march.
    let high_rgb = sun_high_rgb * cf::dual_hg(cos_theta, 1.0) * exp(-od_high)
        + sky_color.rgb * (1.0 - 0.5 * shape_high);

    // Composite low over high in premultiplied alpha, then divide back out.
    var rgb = (low_rgb * a_low + high_rgb * a_high * (1.0 - a_low)) / max(alpha, 1e-4);

    // ---- aerial perspective ----------------------------------------------
    // Bevy composites its own aerial perspective over opaque geometry only, so an
    // alpha-blended shell never receives any. Without this the deck kept full
    // contrast right up to the last visible pixel and then had its alpha faded
    // out, which reads as the sky *ending* rather than as cloud receding into air.
    // Extinction toward the same fog colour the horizon shell paints puts the far
    // deck behind the same air as the far ground. The curve is squared to match
    // `FogFalloff::ExponentialSquared`, which is what `free_camera::distance_fog`
    // builds and therefore what every other surface in the scene is wearing; the
    // path is the ray's traverse of the haze layer, not the whole trip to the deck.
    let haze_path = min(t_low, cf::HAZE_LAYER_M / dir.y);
    let x = haze_path * params2.z;
    rgb = mix(haze_color.rgb, rgb, exp(-x * x));

    return vec4<f32>(rgb, clamp(alpha, 0.0, 1.0));
}
