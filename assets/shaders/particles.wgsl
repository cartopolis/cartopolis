// A prim's particle emitter, generated in the vertex stage.
//
// The mesh this runs over holds no particles. It is `particles::quad_pool` — a
// fixed run of `MAX_PARTICLES` quads, four vertices each, carrying only which
// particle they belong to (`uv.x`) and which corner they are (`position.xy`,
// ±0.5). Every emitter in the world draws that one shared handle; what differs is
// the twenty floats below. So an emitter uploads nothing after startup, and a
// hundred fires cost a hundred small uniform buffers rather than a hundred vertex
// buffers rewritten every frame.
//
// That is the same trade `grass.wgsl` makes, for the same reason: a CPU-simulated
// system would rewrite its vertex buffer every frame, and mass per-frame uploads
// are what loses the device on the machines this has to run on (CLAUDE.md's
// "Mass mesh/texture churn hangs the Intel iGPU", `docs/notes/upload-bursts.md`).
//
// The consequence, and it is not hidden: **a particle here is a closed-form
// function of time**, so nothing collides, nothing is individually addressable,
// and two clients agree only because they evaluate the same formula. For fire and
// for an explosion that is the whole of what is wanted.
//
// Both stages are ours, so the varyings are this file's own struct rather than
// `forward_io::VertexOutput` — every field of that one is behind a shader-def
// keyed to a mesh attribute, and this mesh deliberately carries only two.

#import bevy_pbr::{
    mesh_functions,
    mesh_view_bindings::{view, globals},
    view_transformations::position_world_to_clip,
}

const TAU: f32 = 6.283185307;

// x: particle count. y: lifetime, seconds. z: source sphere radius, metres.
// w: turbulence, metres of wander at one second.
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> emitter: vec4<f32>;

// x: initial speed, m/s. y: cone half-angle, radians. z: acceleration on world up,
// m/s² (positive is up — a flame rises). w: the time a one-shot burst started, or
// a negative number for a continuous emitter.
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var<uniform> motion: vec4<f32>;

// x: size at birth, metres. y: size at death. z: overall opacity. w: spare.
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var<uniform> sizing: vec4<f32>;

// rgb: linear colour at birth / at death. These are emissive and routinely above
// 1 — the core of a flame is meant to clip the tonemapper.
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var<uniform> hot: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var<uniform> cool: vec4<f32>;

struct ParticleVertex {
    @builtin(instance_index) instance_index: u32,
    // The quad corner, ±0.5 in x and y.
    @location(0) position: vec3<f32>,
    // x: which particle. y: unused, and kept so the attribute is a `vec2` — the
    // mesh's UV_0 is what puts this at @location(2) at all.
    @location(2) uv: vec2<f32>,
};

struct ParticleOut {
    @builtin(position) position: vec4<f32>,
    // The corner in 0..1, for the fragment's radial falloff.
    @location(0) uv: vec2<f32>,
    // rgb: this particle's colour. a: its opacity, envelope already applied.
    @location(1) tint: vec4<f32>,
};

// Four decorrelated randoms for one particle. A hash rather than a texture or a
// buffer: a particle's identity is its index, and everything about it has to be
// re-derivable from that alone for the "nothing is stored" property to hold.
fn hash4(n: f32) -> vec4<f32> {
    let p = vec4<f32>(n, n * 1.7, n * 2.9, n * 4.3);
    return fract(sin(p * vec4<f32>(12.9898, 78.233, 37.719, 4.898)) * 43758.5453);
}

@vertex
fn vertex(v: ParticleVertex) -> ParticleOut {
    var out: ParticleOut;

    let count = max(emitter.x, 1.0);
    let life = max(emitter.y, 1e-3);
    let burst_start = motion.w;
    let idx = v.uv.x;

    // Where the emitter is. Column 3 of the model matrix is the translation, and
    // *only* the translation — which is the point: the emitter entity is a child
    // of the prim, so the matrix also carries the prim's rotation and scale, and a
    // flame should not stretch because the thing burning is a flattened box.
    // Every offset below is built in world metres from the uniforms instead.
    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let origin = world_from_local[3].xyz;

    var age: f32;
    var alive: f32;
    if burst_start >= 0.0 {
        // One shot: every particle is born at the same instant.
        //
        // `globals.time` wraps (bevy's default period is an hour), so a burst
        // armed either side of a wrap reads as a negative age and is skipped
        // rather than drawn wrongly. That is one missed explosion an hour in the
        // worst case, against a per-frame uniform write for every emitter in the
        // world to avoid it.
        age = globals.time - burst_start;
        alive = f32(age >= 0.0 && age <= life);
        age = clamp(age, 0.0, life);
    } else {
        // Continuous: the particles are spread evenly over one lifetime, so the
        // stream is steady and `count / life` is the emission rate. No separate
        // rate uniform, because two numbers that must agree are one number
        // written twice.
        age = fract(globals.time / life + idx / count) * life;
        alive = 1.0;
    }
    let t = age / life;

    let h = hash4(idx + 0.5);

    // A direction inside a cone about world up. Sampled in the cosine, not in the
    // angle: sampling the angle uniformly crowds the particles towards the axis
    // and a wide spread then reads as a narrow one with a halo.
    let cos_min = cos(clamp(motion.y, 0.0, 3.14159265));
    let cz = mix(1.0, cos_min, h.x);
    let sz = sqrt(max(0.0, 1.0 - cz * cz));
    let phi = h.y * TAU;
    let dir = vec3<f32>(sz * cos(phi), cz, sz * sin(phi));

    // Born somewhere in a sphere, not all at one point — a point source reads as
    // a jet. The cube root is what makes the sphere uniformly filled rather than
    // clustered at the middle.
    let birth = normalize(h.xyz - 0.5 + vec3<f32>(1e-4)) * emitter.z * pow(h.w, 0.3333);

    // Speed varies per particle, or the front of the stream is a hard shell.
    let speed = motion.x * (0.6 + 0.8 * h.z);

    // The wander. Three out-of-phase sines rather than a noise lookup: it is a
    // vertex stage, this runs per corner, and what it has to buy is "the flame
    // curls" rather than a physically defensible turbulence. Growing with age
    // (not with age²) is what makes it a drift rather than a second gravity.
    let swirl = vec3<f32>(
        sin(age * 3.1 + h.x * TAU),
        sin(age * 2.2 + h.y * TAU) * 0.35,
        cos(age * 2.7 + h.z * TAU),
    );

    let centre = origin
        + birth
        + dir * (speed * age)
        + vec3<f32>(0.0, 0.5 * motion.z * age * age, 0.0)
        + swirl * (emitter.w * age);

    // In fast, out slow. Not configurable, because every other shape of this
    // curve — most of all a linear fade — reads as a bug rather than as a choice.
    let envelope = smoothstep(0.0, 0.1, t) * (1.0 - smoothstep(0.5, 1.0, t));
    let size = mix(sizing.x, sizing.y, t) * alive;

    // Camera-facing. The first two columns of `world_from_view` are the camera's
    // right and up in world space, which is the whole of a billboard.
    let cam_right = normalize(view.world_from_view[0].xyz);
    let cam_up = normalize(view.world_from_view[1].xyz);
    let world = centre + cam_right * (v.position.x * size) + cam_up * (v.position.y * size);

    out.position = position_world_to_clip(world);
    out.uv = v.position.xy + 0.5;
    out.tint = vec4<f32>(mix(hot.rgb, cool.rgb, t), sizing.z * envelope * alive);
    return out;
}

@fragment
fn fragment(in: ParticleOut) -> @location(0) vec4<f32> {
    // A soft round blob out of a square quad. Squared, so the core is bright and
    // the edge is genuinely gone — a linear falloff leaves a visible disc rim
    // once a few hundred of these overlap.
    let r = length(in.uv * 2.0 - 1.0);
    let core = 1.0 - smoothstep(0.0, 1.0, r);
    let a = core * core * in.tint.a;
    if a <= 0.003 {
        // Cheaper than blending a transparent fragment, and — since the material
        // is `AlphaMode::Add` and writes no depth — it costs nothing else.
        discard;
    }
    // Premultiplied with an alpha of zero, which under bevy's `AlphaMode::Add`
    // (`src·1 + dst·(1 − srcAlpha)`) is exactly `src + dst`: pure additive. Fire
    // adds light; it does not cover what is behind it.
    return vec4<f32>(in.tint.rgb * a, 0.0);
}
