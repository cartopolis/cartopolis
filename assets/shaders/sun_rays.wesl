// Sunbeams as a pass over the finished frame (`systems::sun_rays` has the why).
//
// Each pixel walks toward the sun's position on screen and gathers what is bright
// along the way. The sun and the sky showing through a gap are the brightest
// things near the sun; a cloud's base and a building are darker. So a pixel whose
// line to the sun crosses a gap gathers light and one whose line crosses a cloud
// does not, and the frame's own occluders cut the glow into beams.
//
// Runs in HDR before tonemapping, on pre-exposed colour, and measures what counts
// as bright against the sky round the sun rather than against a fixed number.
import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput;

// `systems::sun_rays::SunRays`, field for field.
struct SunRays {
    sun_uv: vec2<f32>,
    strength: f32,
    aspect: f32,
    tint: vec4<f32>,
}

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var screen_sampler: sampler;
@group(0) @binding(2) var<uniform> rays: SunRays;

// Taps per pixel. Jittered per pixel, so fewer read as grain rather than steps.
const SAMPLES: i32 = 24;
// How much of the way to the sun the walk covers. Under 1, so every pixel's walk
// ends short of the disc and the disc alone does not wash the whole frame.
const REACH: f32 = 0.9;
// Each tap counts for this much less than the one before it, nearer the sun: the
// beam belongs to the gap it came through, not to the disc behind it.
const DECAY: f32 = 0.96;
// Brightness at which the frame starts to count as light to spread, and at which
// it counts in full — as fractions of how bright the sky round the sun is, not
// as absolute values, so a dusk sky casts beams as a noon one does. Below the
// first is cloud base, ground and building; a gap near the sun is above the
// second.
const MASK_LOW: f32 = 0.8;
const MASK_HIGH: f32 = 1.3;
// Brightest a tap may count as, in the same units, so the sun's disc — orders of
// magnitude over the sky — is one bright source among the gaps rather than the
// whole effect.
const MASK_CAP: f32 = 2.5;
// Radius, in screen heights, of the region round the sun whose brightness
// casts beams. A bright facade across the frame is not the sun.
const SOURCE_RADIUS: f32 = 0.38;
// Radius, in screen heights, of the ring the sky's brightness is read from.
// Outside the disc and its bloom, inside the region that casts.
const REFERENCE_RADIUS: f32 = 0.12;

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// How bright the frame is round the sun: the mean of eight taps on a ring. The
// same for every pixel, so it costs eight taps a pixel and buys thresholds that
// follow the sky rather than a number tuned for one hour of one day.
fn sky_reference() -> f32 {
    var sum = 0.0;
    for (var i = 0; i < 8; i = i + 1) {
        let a = f32(i) * 0.7853982;
        let offset = vec2<f32>(cos(a) / rays.aspect, sin(a)) * REFERENCE_RADIUS;
        let lum = luminance(textureSampleLevel(screen, screen_sampler, rays.sun_uv + offset, 0.0).rgb);
        sum += log(max(lum, 1e-4));
    }
    // The geometric mean, not the plain one. A crisp cloud edge lit by the
    // forward lobe is many times the sky, and with the plain mean one tap
    // crossing it scaled every beam on the screen at once: the frame flashed
    // white as the deck drifted past the sun (2026-10-17).
    return exp(sum / 8.0);
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let base = textureSample(screen, screen_sampler, in.uv);
    if rays.strength <= 0.0 {
        return base;
    }

    let reference = sky_reference();
    let step = (rays.sun_uv - in.uv) * (REACH / f32(SAMPLES));
    // Interleaved gradient noise, per pixel.
    let jitter = fract(52.9829189 * fract(dot(in.position.xy, vec2<f32>(0.06711056, 0.00583715))));
    var p = in.uv + step * jitter;
    var weight = 1.0;
    var gathered = 0.0;
    for (var i = 0; i < SAMPLES; i = i + 1) {
        let lum = luminance(textureSampleLevel(screen, screen_sampler, p, 0.0).rgb) / reference;
        let light = smoothstep(MASK_LOW, MASK_HIGH, lum) * min(lum, MASK_CAP);
        let d = (p - rays.sun_uv) * vec2<f32>(rays.aspect, 1.0);
        let source = exp(-dot(d, d) / (SOURCE_RADIUS * SOURCE_RADIUS));
        gathered += light * source * weight;
        weight *= DECAY;
        p += step;
    }
    // Back into the frame's units: the beams are as bright as the sky they are
    // made of, which is what keeps them the same strength from noon to dusk.
    let beams = rays.tint.rgb * (gathered * reference * rays.strength / f32(SAMPLES));
    return vec4<f32>(base.rgb + beams, base.a);
}
