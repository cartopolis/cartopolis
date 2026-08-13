// Screen-space rain. Drawn on a small camera-centred shell (like the horizon-haze
// shell) whose interior fills the frame; the pattern is purely in screen space —
// derived from the framebuffer coordinate and the viewport size — so the mesh's
// exact size and orientation don't matter, only that it covers the view. Fixed
// per-pixel cost, no geometry churn: safe on the iGPU (see CLAUDE.md notes).
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

// x: intensity 0..1. y: time seconds. z: wind slant. w: unused.
@group(3) @binding(0) var<uniform> params: vec4<f32>;
// rgb: streak colour. a: overall opacity scale.
@group(3) @binding(1) var<uniform> tint: vec4<f32>;

fn hash11(p: f32) -> f32 {
    return fract(sin(p * 127.1) * 43758.5453);
}
fn hash21(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

// One sheet of falling streaks. `cols` sets the column count (depth cue: fewer =
// nearer/bigger), `speed` the fall rate, `slant` the wind shear, `thin` the
// streak width.
fn rain_layer(uv: vec2<f32>, t: f32, cols: f32, speed: f32, slant: f32, thin: f32) -> f32 {
    var p = uv;
    p.x += p.y * slant;
    p.x *= cols;
    let id = floor(p.x);
    let rnd = hash11(id);
    // Per-column phase and a little speed variation so the sheet isn't a grid.
    // NOTE the sign: framebuffer Y runs top→bottom, so subtracting the time term
    // scrolls the streaks *downward* (a `+` here rained upward).
    let y = p.y * (cols * 0.55) - t * speed * (0.6 + 0.8 * rnd) + rnd * 17.0;
    let fx = fract(p.x) - 0.5;
    let fy = fract(y);
    // Thin across, a short bright segment along, brightest at the head.
    let across = smoothstep(thin, 0.0, abs(fx));
    let along = smoothstep(0.0, 0.12, fy) * smoothstep(0.6, 0.12, fy);
    // Only some columns carry a drop in any given fall cycle.
    let on = step(0.55, hash21(vec2<f32>(id, floor(y))));
    return across * along * on;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Screen UV in 0..1, then aspect-correct x so streaks stay vertical/round.
    //
    // `main_pass_viewport`, not `viewport`: the phone tier renders the 3D passes
    // below the target's resolution (`systems::render_scale`), and Bevy's rule for
    // that is that a shader wanting the size it is *actually* rasterising into must
    // read this field. `in.position.xy` is a fragment coordinate in the reduced
    // space, so dividing it by the full target size would push the whole rain
    // field into one corner of the screen. The two are identical on every tier
    // that does not scale.
    let uv = in.position.xy / view.main_pass_viewport.zw;
    let aspect = view.main_pass_viewport.z / max(view.main_pass_viewport.w, 1.0);
    let suv = vec2<f32>(uv.x * aspect, uv.y);

    // Fall speed in cells/sec: ~2–3 screen heights per second reads as real rain
    // (the old ~0.1 crept). Three sheets at different rates give depth parallax.
    let t = params.y;
    let slant = params.z;
    var r = 0.0;
    r += rain_layer(suv, t, 26.0, 22.0, slant, 0.05) * 0.55;
    r += rain_layer(suv, t, 46.0, 32.0, slant * 0.8, 0.035) * 0.45;
    r += rain_layer(suv, t, 16.0, 16.0, slant * 1.25, 0.07) * 0.40;
    r *= params.x;

    let a = clamp(r, 0.0, 1.0) * tint.a;
    if a <= 0.002 {
        discard;
    }
    return vec4<f32>(tint.rgb, a);
}
