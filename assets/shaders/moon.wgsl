// Moon disc with a radiant halo. The material supplies the sun direction in the
// quad's local frame (xyz) and an overall brightness (w). The disc fills the
// inner DISC_R of the quad: the fragment builds a sphere normal from the quad's
// [-1,1]² UV and lights it, so the lunar phase and the terminator's orientation
// fall out of the real sun–moon geometry — no phase texture or mask. The rest
// of the quad carries a soft glow that scales with how much of the lit face is
// turned toward the viewer. Alpha-blended: the disc (even its dark side) covers
// the stars behind it, and the halo gently veils them.
#import bevy_pbr::forward_io::VertexOutput

@group(3) @binding(0) var<uniform> sun_local: vec4<f32>;

const DISC_R: f32 = 0.42;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let brightness = clamp(sun_local.w, 0.0, 1.0);
    let sun = normalize(sun_local.xyz);
    // Fraction of the lit hemisphere facing the viewer (drives the halo).
    let facing = clamp(sun.z, 0.0, 1.0);

    // Sphere-shaded disc in the inner portion of the quad.
    let p = in.uv / DISC_R;
    let d2 = dot(p, p);
    let inside = 1.0 - smoothstep(0.92, 1.0, d2);
    var rgb = vec3<f32>(0.0);
    if d2 < 1.0 {
        let n = vec3<f32>(p.x, p.y, sqrt(max(1.0 - d2, 0.0)));
        let lit = clamp(dot(n, sun), 0.0, 1.0);
        // Pushed a little past 1.0 so the lit face stays radiant-white through
        // the tonemapper, plus a whisper of earthshine on the dark limb.
        rgb = vec3<f32>(0.92, 0.90, 0.84) * (0.05 + 1.25 * pow(lit, 0.8));
    }

    // Radiant halo past the limb: cubic falloff that reaches exactly zero well
    // inside the quad. (An exponential tail never reaches zero, so the quad's
    // square boundary showed as a diamond against the black sky.)
    let r = length(in.uv);
    let t = clamp(1.0 - (r - DISC_R) / (0.95 - DISC_R), 0.0, 1.0);
    let halo = 0.22 * facing * t * t * t;

    let a = max(inside, halo) * brightness;
    rgb = mix(vec3<f32>(0.80, 0.83, 0.92), rgb, inside);
    return vec4<f32>(rgb, a);
}
