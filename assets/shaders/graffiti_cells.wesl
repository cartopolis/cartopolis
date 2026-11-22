// Written by tools/graffiti/gen.ts — do not edit; run it again.
//
// The cells of assets/textures/graffiti.png, 4 × 4, row by row: x, y the
// share of the cell the cut-out fills (centred in it), z its width on a wall in
// metres, w 1 for a tag and 0 for a piece. The pieces come first.

const GRID: f32 = 4.0;
const PIECES: u32 = 11u;
const TAGS: u32 = 4u;
const CELLS = array<vec4<f32>, 15>(
    vec4<f32>(0.9844, 0.9570, 1.90, 0.0),
    vec4<f32>(0.9844, 0.7461, 2.30, 0.0),
    vec4<f32>(0.8887, 0.9844, 1.80, 0.0),
    vec4<f32>(0.9844, 0.5605, 2.80, 0.0),
    vec4<f32>(0.9844, 0.5215, 1.80, 0.0),
    vec4<f32>(0.9844, 0.1563, 3.20, 0.0),
    vec4<f32>(0.9844, 0.4277, 3.00, 0.0),
    vec4<f32>(0.9844, 0.5313, 2.60, 0.0),
    vec4<f32>(0.9844, 0.3438, 3.00, 0.0),
    vec4<f32>(0.9844, 0.3340, 3.00, 0.0),
    vec4<f32>(0.9844, 0.3047, 1.40, 0.0),
    vec4<f32>(0.5176, 0.9844, 0.80, 1.0),
    vec4<f32>(0.9844, 0.6055, 1.10, 1.0),
    vec4<f32>(0.7773, 0.9844, 0.90, 1.0),
    vec4<f32>(0.9844, 0.5742, 0.60, 1.0),
);
