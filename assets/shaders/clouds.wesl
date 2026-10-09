// Procedural cloud ceiling: two camera-centred decks, projected onto flat layers
// above the camera and shaded with a *scattering* model rather than a surface one.
//
// The geometry is unchanged — one static shell mesh, no churn, so it stays clear
// of the iGPU command-buffer budget (see CLAUDE.md) — and each above-the-horizon
// view ray is still projected onto a cloud plane and sampled for coverage there.
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
// reads `CLOUDS_LOW` instead. zw: unused.
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
    let thresh = 1.0 - cover;
    let clock = params0.z;
    let evolve = cf::EVOLVE_DETAIL * clock;
    // The sample point is absolute world position: camera-relative projection plus
    // the camera's own displacement from the anchor, both in metres, and the wind's
    // drift on top.
    let anchor_m = params2.xy;
    let drift_m = params1.xy;

    // ---- low deck ---------------------------------------------------------
    let t_low = params0.w / dir.y;
    let detail_low = cf::detail_at(t_low);
    let uv_low = cf::low_warped(cf::low_uv(dir.xz * t_low, anchor_m, drift_m), clock, detail_low);
    let shape_low = cf::low_shape(
        uv_low,
        clock,
        cover,
        soft,
        detail_low,
        cf::low_octaves(CHEAP),
        cf::low_billow(CHEAP),
    );

    // ---- high deck --------------------------------------------------------
    let t_high = params2.w / dir.y;
    let uv_high = (dir.xz * t_high + anchor_m + drift_m * cf::HIGH_WIND_SCALE) * cf::FREQ_HIGH;
    let detail_high = cf::detail_at(t_high);
    let thresh_high = 1.0 - cover * cf::HIGH_COVER_SCALE;
    // Cirrus streaks churn too, a little slower than the deck below them.
    let shape_high = smoothstep(
        thresh_high,
        thresh_high + soft,
        cf::fbm(uv_high, OCT_HIGH, 0, detail_high, evolve * 0.7),
    );

    // ---- opacity ----------------------------------------------------------
    // Beer's law on the view ray, so a core saturates smoothly rather than being
    // clipped by the `pow()` curve this used to shape the alpha with.
    let od_low = shape_low * cf::OPTICAL_DEPTH_LOW;
    let od_high = shape_high * cf::OPTICAL_DEPTH_HIGH;

    let graze = smoothstep(cf::GRAZE_MIN, cf::GRAZE_MAX, dir.y);
    let fade = sun_color.a * params1.w * (1.0 - params1.z) * graze;
    let a_low = (1.0 - exp(-od_low)) * fade;
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

    let sun_od = cf::sun_optical_depth(
        uv_low,
        sun,
        sun_dir.w,
        thresh,
        soft,
        detail_low,
        evolve,
        SUN_STEPS,
    );
    let ms = cf::multi_scatter(sun_od, cos_theta, MS_OCTAVES);
    // Beer-Powder (Schneider, Nubis / Horizon Zero Dawn). The powder factor
    // darkens thin edges — the reason a cumulus reads as solid rather than as
    // fog — but only when the sun is behind the camera. Looking *into* the sun
    // those same edges are the brightest thing in the sky, and applying powder
    // there would cancel exactly the silver lining the phase function just built.
    let powder = 1.0 - exp(-2.0 * od_low);
    let powder_w = clamp(0.5 - 0.5 * cos_theta, 0.0, 1.0);
    // Cloud bases go dark, but never black — the ground bounces light back up at
    // them, and an overcast deck is the sky's own light source rather than
    // something standing in front of it. How dark is `haze_color.a`, above.
    let sky_floor = haze_color.a;
    let low_rgb = sun_low_rgb * ms * mix(1.0, powder, powder_w)
        + sky_color.rgb * (sky_floor + (1.0 - sky_floor) * (1.0 - shape_low));

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
