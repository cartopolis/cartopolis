// Separable Gaussian for the chrome's backdrop blur (`systems::glass`).
//
// Run twice over a quarter-size copy of the frame: once across, once down. The
// downsample itself does most of the blurring — a bilinear tap at quarter size
// already averages a 4x4 block — so the kernel below only has to hide the
// structure that leaves behind.
//
// Not a Bevy `Material` and not loaded through the asset server: this is a raw
// wgpu pipeline in the render world, built once from `include_str!`, so it needs
// no asset root, no LFS object and no handle to be alive before the first frame.

struct Params {
    // xy: how much of the source texture actually holds the picture. The phone
    // tier renders into the top-left corner of a full-size texture
    // (`systems::render_scale`), so this is `viewport / texture` and is 1.0 only
    // when the two agree. Sampling past it reads unrendered margin and smears it
    // across the edge of every panel.
    //
    // zw: one step along the axis this pass blurs, in the source's UV space. The
    // other component is zero, which is what makes one shader do both passes.
    span: vec4<f32>,
};

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> params: Params;

struct Vertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// The usual fullscreen triangle: three vertices, no vertex buffer, no index
// buffer. A quad would need four and would seam along the diagonal.
@vertex
fn vertex(@builtin(vertex_index) index: u32) -> Vertex {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: Vertex;
    out.uv = uv;
    out.position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

// A nine-tap Gaussian folded into five samples: each off-centre tap sits between
// two texels so the hardware's linear filter adds the pair for free. The offsets
// are the standard sigma-2 pair and the weights are their combined kernel
// weights, which is why they are not whole numbers.
const OFFSET_1: f32 = 1.3846153846;
const OFFSET_2: f32 = 3.2307692308;
const WEIGHT_0: f32 = 0.2270270270;
const WEIGHT_1: f32 = 0.3162162162;
const WEIGHT_2: f32 = 0.0702702703;

@fragment
fn fragment(in: Vertex) -> @location(0) vec4<f32> {
    let extent = params.span.xy;
    let step = params.span.zw;
    let centre = in.uv * extent;
    // Clamp every tap into the rendered region rather than relying on the
    // sampler's clamp-to-edge, which would clamp to the edge of the *texture* —
    // i.e. into the unrendered margin whenever the render scale is below one.
    let low = vec2<f32>(0.0, 0.0);
    let high = extent;

    var colour = textureSampleLevel(source, source_sampler, centre, 0.0).rgb * WEIGHT_0;
    let step_1 = step * OFFSET_1;
    let step_2 = step * OFFSET_2;
    colour += textureSampleLevel(source, source_sampler, clamp(centre + step_1, low, high), 0.0).rgb * WEIGHT_1;
    colour += textureSampleLevel(source, source_sampler, clamp(centre - step_1, low, high), 0.0).rgb * WEIGHT_1;
    colour += textureSampleLevel(source, source_sampler, clamp(centre + step_2, low, high), 0.0).rgb * WEIGHT_2;
    colour += textureSampleLevel(source, source_sampler, clamp(centre - step_2, low, high), 0.0).rgb * WEIGHT_2;

    // Opaque: the panel's own fill is what makes the chrome translucent, and an
    // alpha here would let the *unblurred* frame show through the blur.
    return vec4<f32>(colour, 1.0);
}
