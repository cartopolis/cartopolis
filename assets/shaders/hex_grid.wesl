// The optional hex grid over the land, seen from far up (`docs/mockups/macro-view/`,
// decided 2026-10-30: a layer, off by default — Layers ▸ Map ▸ From above).
//
// One grid for the whole world, laid in Web Mercator's unit square, so the flat
// clipmap tiles and the globe's tiles draw the same lines and meet without a step:
// each tile hands over its own key (`tile` below), and its UV is its place inside
// it. Mercator stretches the cells towards the poles, as every web map does; at the
// Netherlands a cell is ~9 km across, the mockup's size.

// A hex's circumradius in Mercator units (the world is 1 across): 3.6e-4 of the
// equator is ~14.4 km there and ~8.9 km at 52°N.
const HEX_R: f32 = 3.6e-4;

// Camera distances, metres, between which the grid shows: in from 60–120 km, out
// from 3,000–4,500 km, where the cells are too small to be worth drawing.
const SHOW_FROM: vec2<f32> = vec2<f32>(60000.0, 120000.0);
const SHOW_UNTIL: vec2<f32> = vec2<f32>(3000000.0, 4500000.0);

// The line: a pale cream, faint.
const LINE: vec3<f32> = vec3<f32>(1.0, 0.973, 0.878);
const OPACITY: f32 = 0.22;

// Distance from `p` to the nearest hex edge, in units of the circumradius, on a
// pointy-topped grid of unit cells.
fn edge_distance(p: vec2<f32>) -> f32 {
    let r = vec2<f32>(1.7320508, 3.0);
    let h = r * 0.5;
    let a = p - r * floor(p / r) - h;
    let b = p - h - r * floor((p - h) / r) - h;
    let g = select(b, a, dot(a, a) < dot(b, b));
    let q = abs(g);
    // The cell is the lattice's Voronoi cell: its sides are normal to the six
    // neighbours, at 0° (the vertical sides) and ±60°.
    let d = max(q.x, dot(q, vec2<f32>(0.5, 0.8660254)));
    return 0.8660254 - d;
}

// How much of the grid's line to mix over a land texel: 0 off the line, off the
// layer, over water or out of the distance band. `tile` is the tile's key as
// (x, y, 2^z, strength); `uv` its place in the tile; `water` 0..1.
fn coverage(tile: vec4<f32>, uv: vec2<f32>, distance: f32, water: f32) -> f32 {
    // A surface with no key (the backstop sphere) has `z == 0`, and draws none.
    let has_key = step(0.5, tile.z);
    let m = (tile.xy + uv) / max(tile.z, 1.0);
    let p = m / HEX_R;
    let e = edge_distance(p);
    // About a pixel and a half wide, whatever the distance.
    let w = max(fwidth(e) * 1.5, 1e-6);
    let line = 1.0 - smoothstep(0.0, w, e);
    let shown = smoothstep(SHOW_FROM.x, SHOW_FROM.y, distance)
        * (1.0 - smoothstep(SHOW_UNTIL.x, SHOW_UNTIL.y, distance));
    return line * shown * (1.0 - water) * tile.w * has_key * OPACITY;
}
