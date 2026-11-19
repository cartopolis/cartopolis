use super::*;

// -- Reference decoders, written from the Khronos Data Format spec and the
// -- D3D block-compression rules, sharing no code with the encoders.

/// BC1 with the general mode rule (`c0 <= c1` → three colours + black), so
/// an encoder that ever left four-colour mode by accident decodes wrong.
fn decode_bc1(b: &[u8]) -> [[u8; 3]; 16] {
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let bits = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    let unpack = |c: u16| -> [u32; 3] {
        let r = u32::from(c >> 11);
        let g = u32::from(c >> 5 & 0x3f);
        let bl = u32::from(c & 0x1f);
        [
            (r << 3) | (r >> 2),
            (g << 2) | (g >> 4),
            (bl << 3) | (bl >> 2),
        ]
    };
    let (p0, p1) = (unpack(c0), unpack(c1));
    let lerp = |w0: u32, w1: u32, d: u32| -> [u32; 3] {
        [0, 1, 2].map(|k| (w0 * p0[k] + w1 * p1[k] + d / 2) / d)
    };
    let pal = if c0 > c1 {
        [p0, p1, lerp(2, 1, 3), lerp(1, 2, 3)]
    } else {
        [p0, p1, lerp(1, 1, 2), [0, 0, 0]]
    };
    std::array::from_fn(|i| pal[(bits >> (2 * i) & 3) as usize].map(|v| v as u8))
}

fn decode_bc4(b: &[u8]) -> [u8; 16] {
    let (a0, a1) = (u32::from(b[0]), u32::from(b[1]));
    let mut bits = 0u64;
    for (k, &byte) in b[2..8].iter().enumerate() {
        bits |= u64::from(byte) << (8 * k);
    }
    let mut pal = [a0, a1, 0, 0, 0, 0, 0, 0];
    if a0 > a1 {
        for i in 1..=6u32 {
            pal[i as usize + 1] = ((7 - i) * a0 + i * a1 + 3) / 7;
        }
    } else {
        for i in 1..=4u32 {
            pal[i as usize + 1] = ((5 - i) * a0 + i * a1 + 2) / 5;
        }
        pal[6] = 0;
        pal[7] = 255;
    }
    std::array::from_fn(|i| pal[(bits >> (3 * i) & 7) as usize] as u8)
}

/// ETC1-mode ETC2 colour decode. Panics on a differential block whose
/// second base leaves 0..=31, which an ETC2 decoder would take for the T
/// (red), H (green) or planar (blue) mode.
fn decode_etc2_rgb(b: &[u8]) -> [[u8; 3]; 16] {
    const TABLE: [[i32; 4]; 8] = [
        [2, 8, -2, -8],
        [5, 17, -5, -17],
        [9, 29, -9, -29],
        [13, 42, -13, -42],
        [18, 60, -18, -60],
        [24, 80, -24, -80],
        [33, 106, -33, -106],
        [47, 183, -47, -183],
    ];
    let w = u64::from_be_bytes(b[..8].try_into().unwrap());
    let field = |hi: u32, len: u32| ((w >> (hi + 1 - len)) & ((1 << len) - 1)) as i32;
    let diff = field(33, 1) == 1;
    let flip = field(32, 1) == 1;
    let (b1, b2): ([i32; 3], [i32; 3]) = if diff {
        let mut b1 = [0; 3];
        let mut b2 = [0; 3];
        for k in 0..3u32 {
            let base = field(63 - 8 * k, 5);
            let mut d = field(58 - 8 * k, 3);
            if d >= 4 {
                d -= 8;
            }
            let second = base + d;
            if !(0..=31).contains(&second) {
                // The first channel to overflow names the mode.
                return match k {
                    0 => decode_t(b),
                    1 => decode_h(b),
                    _ => decode_planar(b),
                };
            }
            b1[k as usize] = (base << 3) | (base >> 2);
            b2[k as usize] = (second << 3) | (second >> 2);
        }
        (b1, b2)
    } else {
        let b1 = [0, 1, 2].map(|k| field(63 - 8 * k, 4) * 17);
        let b2 = [0, 1, 2].map(|k| field(59 - 8 * k, 4) * 17);
        (b1, b2)
    };
    let (t1, t2) = (field(39, 3) as usize, field(36, 3) as usize);
    let mut out = [[0u8; 3]; 16];
    for x in 0..4 {
        for y in 0..4 {
            let j = x * 4 + y;
            let msb = (w >> (16 + j)) & 1;
            let lsb = (w >> j) & 1;
            let second = if flip { y >= 2 } else { x >= 2 };
            let (base, t) = if second { (b2, t2) } else { (b1, t1) };
            let m = TABLE[t][(msb * 2 + lsb) as usize];
            out[y * 4 + x] = base.map(|c| (c + m).clamp(0, 255) as u8);
        }
    }
    out
}

/// Which mode an ETC2 colour block is, by the spec's rule: differential bit
/// clear → individual; set → red overflowing is T, else green is H, else
/// blue is planar, else differential.
fn etc2_mode(b: &[u8]) -> &'static str {
    if b[3] & 2 == 0 {
        return "individual";
    }
    for (k, name) in ["T", "H", "planar"].into_iter().enumerate() {
        let base = i32::from(b[k] >> 3);
        let delta = i32::from(b[k] & 7);
        let delta = if delta >= 4 { delta - 8 } else { delta };
        if !(0..=31).contains(&(base + delta)) {
            return name;
        }
    }
    "differential"
}

/// The 2-bit selector of texel (x, y): LSB from the low 16 bits, MSB from
/// the next 16, both indexed column-major.
fn etc_selector(b: &[u8], x: usize, y: usize) -> usize {
    let low = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
    let j = x * 4 + y;
    (((low >> (16 + j)) & 1) << 1 | ((low >> j) & 1)) as usize
}

const TH_DIST: [i32; 8] = [3, 6, 11, 16, 23, 32, 41, 64];

fn paint_block(b: &[u8], paint: [[i32; 3]; 4]) -> [[u8; 3]; 16] {
    let mut out = [[0u8; 3]; 16];
    for y in 0..4 {
        for x in 0..4 {
            out[y * 4 + x] = paint[etc_selector(b, x, y)].map(|v| v.clamp(0, 255) as u8);
        }
    }
    out
}

/// T mode's codes, read byte by byte: `(base1, base2, distance index)`.
fn t_codes(b: &[u8]) -> ([i32; 3], [i32; 3], usize) {
    let b: [i32; 8] = std::array::from_fn(|i| i32::from(b[i]));
    let c1 = [(((b[0] >> 3) & 3) << 2) | (b[0] & 3), b[1] >> 4, b[1] & 15];
    let c2 = [b[2] >> 4, b[2] & 15, b[3] >> 4];
    let d = ((((b[3] >> 2) & 3) << 1) | (b[3] & 1)) as usize;
    (c1, c2, d)
}

fn decode_t(b: &[u8]) -> [[u8; 3]; 16] {
    let (c1, c2, d) = t_codes(b);
    let (c1, c2, dist) = (c1.map(|v| v * 17), c2.map(|v| v * 17), TH_DIST[d]);
    paint_block(b, [c1, c2.map(|v| v + dist), c2, c2.map(|v| v - dist)])
}

/// H mode's codes, read byte by byte: `(base1, base2, distance index)`,
/// the index's lowest bit derived from the order of the two bases.
fn h_codes(b: &[u8]) -> ([i32; 3], [i32; 3], usize) {
    let b: [i32; 8] = std::array::from_fn(|i| i32::from(b[i]));
    let c1 = [
        (b[0] >> 3) & 15,
        ((b[0] & 7) << 1) | ((b[1] >> 4) & 1),
        (((b[1] >> 3) & 1) << 3) | ((b[1] & 3) << 1) | (b[2] >> 7),
    ];
    let c2 = [
        (b[2] >> 3) & 15,
        ((b[2] & 7) << 1) | (b[3] >> 7),
        (b[3] >> 3) & 15,
    ];
    let order = |c: [i32; 3]| c[0] * 256 + c[1] * 16 + c[2];
    let d = (b[3] & 4) | ((b[3] & 1) << 1) | i32::from(order(c1) >= order(c2));
    (c1, c2, d as usize)
}

fn decode_h(b: &[u8]) -> [[u8; 3]; 16] {
    let (c1, c2, d) = h_codes(b);
    let (c1, c2, dist) = (c1.map(|v| v * 17), c2.map(|v| v * 17), TH_DIST[d]);
    paint_block(
        b,
        [
            c1.map(|v| v + dist),
            c1.map(|v| v - dist),
            c2.map(|v| v + dist),
            c2.map(|v| v - dist),
        ],
    )
}

/// The planar block's nine colour codes, read byte by byte the way the
/// spec's bit table falls across the big-endian bytes (independent of the
/// encoder's 64-bit shifts): `[[RO, GO, BO], [RH, GH, BH], [RV, GV, BV]]`.
fn planar_codes(b: &[u8]) -> [[i32; 3]; 3] {
    let b: [i32; 8] = std::array::from_fn(|i| i32::from(b[i]));
    let ro = (b[0] >> 1) & 0x3f;
    let go = ((b[0] & 1) << 6) | ((b[1] >> 1) & 0x3f);
    let bo = ((b[1] & 1) << 5) | (b[2] & 0x18) | ((b[2] & 3) << 1) | (b[3] >> 7);
    let rh = (((b[3] >> 2) & 0x1f) << 1) | (b[3] & 1);
    let gh = b[4] >> 1;
    let bh = ((b[4] & 1) << 5) | (b[5] >> 3);
    let rv = ((b[5] & 7) << 3) | (b[6] >> 5);
    let gv = ((b[6] & 0x1f) << 2) | (b[7] >> 6);
    let bv = b[7] & 0x3f;
    [[ro, go, bo], [rh, gh, bh], [rv, gv, bv]]
}

fn decode_planar(b: &[u8]) -> [[u8; 3]; 16] {
    let c = planar_codes(b);
    // 6-bit red and blue, 7-bit green, widened by bit replication.
    let wide = |v: i32, k: usize| {
        if k == 1 {
            (v << 1) | (v >> 6)
        } else {
            (v << 2) | (v >> 4)
        }
    };
    let mut out = [[0u8; 3]; 16];
    for y in 0..4i32 {
        for x in 0..4i32 {
            for k in 0..3 {
                let (o, h, v) = (wide(c[0][k], k), wide(c[1][k], k), wide(c[2][k], k));
                let val = (x * (h - o) + y * (v - o) + 4 * o + 2) >> 2;
                out[(y * 4 + x) as usize][k] = val.clamp(0, 255) as u8;
            }
        }
    }
    out
}

fn decode_eac(b: &[u8]) -> [u8; 16] {
    const TABLE: [[i32; 8]; 16] = [
        [-3, -6, -9, -15, 2, 5, 8, 14],
        [-3, -7, -10, -13, 2, 6, 9, 12],
        [-2, -5, -8, -13, 1, 4, 7, 12],
        [-2, -4, -6, -13, 1, 3, 5, 12],
        [-3, -6, -8, -12, 2, 5, 7, 11],
        [-3, -7, -9, -11, 2, 6, 8, 10],
        [-4, -7, -8, -11, 3, 6, 7, 10],
        [-3, -5, -8, -11, 2, 4, 7, 10],
        [-2, -6, -8, -10, 1, 5, 7, 9],
        [-2, -5, -8, -10, 1, 4, 7, 9],
        [-2, -4, -8, -10, 1, 3, 7, 9],
        [-2, -5, -7, -10, 1, 4, 6, 9],
        [-3, -4, -7, -10, 2, 3, 6, 9],
        [-1, -2, -3, -10, 0, 1, 2, 9],
        [-4, -6, -8, -9, 3, 5, 7, 8],
        [-3, -5, -7, -9, 2, 4, 6, 8],
    ];
    let w = u64::from_be_bytes(b[..8].try_into().unwrap());
    let base = (w >> 56) as i32;
    let mult = (w >> 52 & 15) as i32;
    let table = (w >> 48 & 15) as usize;
    assert_ne!(mult, 0, "multiplier 0 written to an EAC alpha block");
    let mut out = [0u8; 16];
    for j in 0..16 {
        let idx = (w >> (45 - 3 * j) & 7) as usize;
        let (x, y) = (j / 4, j % 4);
        out[y * 4 + x] = (base + TABLE[table][idx] * mult).clamp(0, 255) as u8;
    }
    out
}

/// Decode a whole compressed chain back to RGBA8 levels, cropped to each
/// level's real size.
fn decode_chain(
    format: BlockFormat,
    width: u32,
    height: u32,
    mip_count: u32,
    data: &[u8],
) -> Vec<Vec<u8>> {
    let mut blocks = data.chunks_exact(16);
    levels(width, height, mip_count)
        .map(|(w, h)| {
            let mut img = vec![0u8; w * h * 4];
            for by in 0..h.div_ceil(4) {
                for bx in 0..w.div_ceil(4) {
                    let b = blocks.next().expect("chain ran out of blocks");
                    let (a, c) = match format {
                        BlockFormat::Bc3 => (decode_bc4(&b[..8]), decode_bc1(&b[8..])),
                        BlockFormat::Etc2Rgba8 => (decode_eac(&b[..8]), decode_etc2_rgb(&b[8..])),
                    };
                    for y in 0..4 {
                        for x in 0..4 {
                            let (px, py) = (bx * 4 + x, by * 4 + y);
                            if px < w && py < h {
                                let o = (py * w + px) * 4;
                                let i = y * 4 + x;
                                img[o..o + 4].copy_from_slice(&[c[i][0], c[i][1], c[i][2], a[i]]);
                            }
                        }
                    }
                }
            }
            img
        })
        .collect()
}

/// A box-filtered chain in the same packing as `build_mip_chain`. The
/// filter does not matter here, only the layout.
fn box_chain(width: u32, height: u32, mip0: Vec<u8>) -> (Vec<u8>, u32) {
    let mut data = mip0.clone();
    let (mut w, mut h, mut cur) = (width as usize, height as usize, mip0);
    let mut count = 1;
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0u8; nw * nh * 4];
        for y in 0..nh {
            for x in 0..nw {
                for k in 0..4 {
                    let mut s = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(w - 1);
                        let sy = (y * 2 + dy).min(h - 1);
                        s += u32::from(cur[(sy * w + sx) * 4 + k]);
                    }
                    next[(y * nw + x) * 4 + k] = ((s + 2) / 4) as u8;
                }
            }
        }
        data.extend_from_slice(&next);
        (w, h, cur) = (nw, nh, next);
        count += 1;
    }
    (data, count)
}

fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

/// What a tile looks like: pale paper with a gradient, roads of several
/// widths with dark casings, hairline "text", a coloured area, a smooth
/// noisy aerial-photo quadrant, and a water body whose alpha is 0 behind a
/// hard coastline (anti-aliased along one stretch).
fn map_like(w: usize, h: usize) -> Vec<u8> {
    let mut img = vec![0u8; w * h * 4];
    let smooth = |x: usize, y: usize, cell: usize, seed: u32| -> f32 {
        let (gx, gy) = (x / cell, y / cell);
        let (fx, fy) = (
            (x % cell) as f32 / cell as f32,
            (y % cell) as f32 / cell as f32,
        );
        let at = |i: usize, j: usize| {
            (hash((i as u32) * 7919 + (j as u32) * 104_729 + seed) & 255) as f32
        };
        let top = at(gx, gy) * (1.0 - fx) + at(gx + 1, gy) * fx;
        let bot = at(gx, gy + 1) * (1.0 - fx) + at(gx + 1, gy + 1) * fx;
        top * (1.0 - fy) + bot * fy
    };
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            // Paper.
            let mut c = [
                (240.0 - 10.0 * fy) as i32,
                (236.0 - 6.0 * fx) as i32,
                (226.0 + 8.0 * fx * fy) as i32,
            ];
            let mut a = 255i32;
            // Aerial quadrant: smooth noise plus grain.
            if x >= w / 2 && y >= h / 2 {
                let n = smooth(x, y, 16, 1) * 0.6 + smooth(x, y, 5, 2) * 0.4;
                let grain = (hash((y * w + x) as u32) & 31) as f32 - 16.0;
                c = [
                    (40.0 + n * 0.35 + grain) as i32,
                    (60.0 + n * 0.45 + grain) as i32,
                    (30.0 + n * 0.25 + grain) as i32,
                ];
            }
            // A park.
            if x < w / 3 && y > h * 2 / 3 {
                c = [200, 228, 190];
            }
            // Roads: a horizontal, a vertical and a diagonal, with casings.
            let road = |d: f32, half: f32, fill: [i32; 3], c: &mut [i32; 3]| {
                if d <= half + 1.0 {
                    *c = [90, 90, 100];
                }
                if d <= half {
                    *c = fill;
                }
            };
            road(
                (y as f32 - h as f32 * 0.3).abs(),
                3.0,
                [252, 214, 90],
                &mut c,
            );
            road(
                (x as f32 - w as f32 * 0.6).abs(),
                1.5,
                [255, 255, 255],
                &mut c,
            );
            road(
                ((x as f32 - y as f32) / 1.414).abs(),
                2.0,
                [255, 255, 255],
                &mut c,
            );
            // Hairline text-like strokes.
            if (w / 10..w / 3).contains(&x)
                && (h / 10..h / 6).contains(&y)
                && (x % 5 == 0 || y % 7 == 0)
            {
                c = [70, 60, 80];
            }
            // Water: a disc with alpha 0, hard edge except one AA stretch.
            let (dx, dy) = (x as f32 - w as f32 * 0.25, y as f32 - h as f32 * 0.55);
            let r = (dx * dx + dy * dy).sqrt();
            let rad = w as f32 * 0.17;
            if r < rad {
                c = [170, 211, 223];
                a = 0;
            } else if dx > 0.0 && dy > 0.0 && r < rad + 1.5 {
                a = (((r - rad) / 1.5) * 255.0) as i32;
            }
            let o = (y * w + x) * 4;
            img[o..o + 4].copy_from_slice(&[
                c[0].clamp(0, 255) as u8,
                c[1].clamp(0, 255) as u8,
                c[2].clamp(0, 255) as u8,
                a as u8,
            ]);
        }
    }
    img
}

/// (RGB PSNR, alpha PSNR) in dB between two RGBA8 images.
fn psnr(a: &[u8], b: &[u8]) -> (f64, f64) {
    let (mut rgb, mut alpha) = (0f64, 0f64);
    for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for k in 0..3 {
            rgb += (f64::from(p[k]) - f64::from(q[k])).powi(2);
        }
        alpha += (f64::from(p[3]) - f64::from(q[3])).powi(2);
    }
    let n = (a.len() / 4) as f64;
    let db = |mse: f64| {
        if mse == 0.0 {
            f64::INFINITY
        } else {
            10.0 * (255.0f64 * 255.0 / mse).log10()
        }
    };
    (db(rgb / (3.0 * n)), db(alpha / n))
}

const FORMATS: [BlockFormat; 2] = [BlockFormat::Bc3, BlockFormat::Etc2Rgba8];

/// Every flat colour comes back close and every flat alpha exactly. BC1's
/// single-colour tables hold within 2 levels (the ⅔ interpolant's own
/// rounding plus the decoder's); ETC1 applies one modifier to all three
/// channels, so the 5-bit base's per-channel rounding survives — ±4 levels
/// is the format's floor, not the encoder's.
#[test]
fn flat_blocks_round_trip() {
    let values = [0u8, 1, 17, 64, 100, 127, 128, 200, 254, 255];
    for format in FORMATS {
        let tol = match format {
            BlockFormat::Bc3 => 2,
            BlockFormat::Etc2Rgba8 => 4,
        };
        let mut worst = 0;
        for &r in &values {
            for &g in &values {
                for &b in &values {
                    for a in [0u8, 1, 128, 254, 255] {
                        let img: Vec<u8> = [r, g, b, a].repeat(16);
                        let data = compress_chain(format, 4, 4, 1, &img);
                        let out = &decode_chain(format, 4, 4, 1, &data)[0];
                        for (p, q) in out.chunks_exact(4).zip(img.chunks_exact(4)) {
                            for k in 0..3 {
                                worst = worst.max((i32::from(p[k]) - i32::from(q[k])).abs());
                            }
                            assert_eq!(
                                p[3], q[3],
                                "{format:?}: flat alpha {a} decoded as {}",
                                p[3]
                            );
                        }
                    }
                }
            }
        }
        assert!(
            worst <= tol,
            "{format:?}: a flat colour came back {worst} levels off"
        );
    }
}

/// The ocean mask: a hard 0/255 coastline, at an offset that cuts through
/// blocks, decodes to exactly 0 and 255 on either side.
#[test]
fn a_hard_mask_edge_stays_exact() {
    let (w, h) = (16usize, 16usize);
    let mut img = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let land = x * 2 + y > 13 && (hash((y * w + x) as u32) % 7 != 0 || x > 10);
            let o = (y * w + x) * 4;
            img[o..o + 4].copy_from_slice(&[
                (x * 15) as u8,
                (y * 15) as u8,
                if land { 60 } else { 200 },
                if land { 255 } else { 0 },
            ]);
        }
    }
    for format in FORMATS {
        let data = compress_chain(format, w as u32, h as u32, 1, &img);
        let out = &decode_chain(format, w as u32, h as u32, 1, &data)[0];
        for (i, (p, q)) in out.chunks_exact(4).zip(img.chunks_exact(4)).enumerate() {
            assert_eq!(
                p[3], q[3],
                "{format:?}: mask texel {i} is {} not {}",
                p[3], q[3]
            );
        }
    }
}

/// A map-like tile and its whole chain. Thresholds: the image is built to
/// be hard — hairlines, cased roads and ±16 levels of per-pixel grain over
/// the aerial quadrant, which no 4×4 block format follows — and measures
/// 33.7 dB (BC3) and 34.3 dB (ETC2) at level 0, so 32 dB holds a margin
/// while still failing on a real regression (a broken refit or a wrong
/// table costs several dB). Alpha measures ~59 dB; 40 dB is a mask that is
/// all but exact, only its anti-aliased stretch of coastline approximated.
#[test]
fn a_map_tile_compresses_with_good_quality() {
    let (w, h) = (256u32, 256u32);
    let (chain, count) = box_chain(w, h, map_like(w as usize, h as usize));
    for format in FORMATS {
        let data = compress_chain(format, w, h, count, &chain);
        let decoded = decode_chain(format, w, h, count, &data);
        let (rgb, alpha) = psnr(&decoded[0], &chain[..(w * h * 4) as usize]);
        eprintln!("{format:?}: level 0 RGB {rgb:.2} dB, alpha {alpha:.2} dB");
        assert!(rgb > 32.0, "{format:?}: RGB PSNR {rgb:.2} dB");
        assert!(alpha > 40.0, "{format:?}: alpha PSNR {alpha:.2} dB");
        // The reduced levels down to 16², each a box-filtered mix of every
        // feature above and so busier per block than level 0 (measured:
        // 30.3 dB worst, ETC2 at 32²). Below that a level is one to four
        // blocks whose PSNR says little — a 2×2 level is four unrelated
        // colours in one block, which ETC's one base per half cannot hold.
        let mut offset = 0;
        for ((lw, lh), img) in levels(w, h, count).zip(&decoded) {
            let len = lw * lh * 4;
            let (rgb, _) = psnr(img, &chain[offset..offset + len]);
            if lw.min(lh) >= 16 {
                assert!(
                    rgb > 29.0,
                    "{format:?}: level {lw}x{lh} RGB PSNR {rgb:.2} dB"
                );
            }
            offset += len;
        }
    }
}

#[test]
fn compressed_length_matches_the_output() {
    for (w, h) in [
        (512u32, 512u32),
        (256, 256),
        (6, 10),
        (1, 1),
        (64, 16),
        (3, 2),
    ] {
        let mip0: Vec<u8> = (0..w * h * 4).map(|i| hash(i) as u8).collect();
        let (chain, count) = box_chain(w, h, mip0);
        for format in FORMATS {
            let data = compress_chain(format, w, h, count, &chain);
            assert_eq!(
                data.len(),
                compressed_chain_len(format, w, h, count),
                "{format:?} {w}x{h}"
            );
        }
    }
    // 512²: 128² + 64² + … + 1 + 1 + 1 blocks (the last three levels are
    // 2×2 and 1×1, one padded block each).
    assert_eq!(
        compressed_chain_len(BlockFormat::Bc3, 512, 512, 10),
        (16384 + 4096 + 1024 + 256 + 64 + 16 + 4 + 1 + 1 + 1) * 16
    );
    assert_eq!(compressed_chain_len(BlockFormat::Etc2Rgba8, 1, 1, 1), 16);
    // 6×10 → 3×5 → 1×2 → 1×1: 2·3 + 1·2 + 1 + 1 blocks.
    assert_eq!(compressed_chain_len(BlockFormat::Bc3, 6, 10, 4), 10 * 16);
}

/// Two very different halves cannot be differential — the second base would
/// leave 0..=31 and an ETC2 decoder would read T/H/planar. The reference
/// decoder panics on a red or green overflow (T, H) and decodes a blue one
/// as planar, so a misplaced differential block shows up either as a panic
/// or as colours far off; this also walks the delta's two legal extremes,
/// −4 and +3, which must stay differential and decode right.
#[test]
fn differential_mode_never_overflows() {
    let split = |left: [u8; 3], right: [u8; 3], vertical: bool| -> Vec<u8> {
        (0..16)
            .flat_map(|i| {
                let (x, y) = (i % 4, i / 4);
                let second = if vertical { x >= 2 } else { y >= 2 };
                let c = if second { right } else { left };
                [c[0], c[1], c[2], 255]
            })
            .collect()
    };
    let e5 = |c: u8| (c << 3) | (c >> 2);
    let cases = [
        ([0, 0, 0], [255, 255, 255], 40),
        ([255, 0, 255], [0, 255, 0], 40),
        ([10, 250, 128], [250, 10, 30], 40),
        ([e5(10), e5(20), e5(5)], [e5(13), e5(16), e5(8)], 10),
        ([e5(31), e5(0), e5(4)], [e5(27), e5(3), e5(0)], 10),
    ];
    for (left, right, tol) in cases {
        for vertical in [true, false] {
            let img = split(left, right, vertical);
            let data = compress_chain(BlockFormat::Etc2Rgba8, 4, 4, 1, &img);
            let rgb = decode_etc2_rgb(&data[8..]);
            for (i, p) in rgb.iter().enumerate() {
                for k in 0..3 {
                    let d = (i32::from(p[k]) - i32::from(img[i * 4 + k])).abs();
                    assert!(
                        d <= tol,
                        "{left:?}|{right:?}: texel {i} channel {k} off by {d}"
                    );
                }
            }
        }
    }
}

/// The planar layout, exhaustively over every O colour (which is where all
/// the bits that decide the mode live): the reference decoder must take
/// every block as planar — not T or H, which it panics on — and read back
/// the nine codes that were packed.
#[test]
fn planar_blocks_always_decode_as_planar() {
    for ro in 0..64 {
        for go in 0..128 {
            for bo in 0..64 {
                let n = hash(ro * 1_000_003 + go * 1009 + bo);
                let codes = [
                    [ro as i32, go as i32, bo as i32],
                    [
                        (n & 63) as i32,
                        (n >> 6 & 127) as i32,
                        (n >> 13 & 63) as i32,
                    ],
                    [
                        (n >> 19 & 63) as i32,
                        (n >> 25 & 127) as i32,
                        (hash(n) & 63) as i32,
                    ],
                ];
                let bytes = pack_planar(codes).to_be_bytes();
                assert_eq!(planar_codes(&bytes), codes);
                let _ = decode_etc2_rgb(&bytes);
            }
        }
    }
}

/// Squared RGB error of an ETC2 colour block against the source block.
fn etc_block_error(bytes: &[u8], px: &Block) -> u32 {
    decode_etc2_rgb(bytes)
        .iter()
        .zip(px)
        .map(|(d, p)| {
            (0..3)
                .map(|k| (i32::from(d[k]) - i32::from(p[k])).pow(2) as u32)
                .sum::<u32>()
        })
        .sum()
}

/// A chroma change inside one block — red rising to the right, green
/// downward, blue falling diagonally — is what the ETC1 modes cannot
/// follow and planar can.
#[test]
fn planar_follows_a_chroma_ramp_inside_a_block() {
    let px: Block = std::array::from_fn(|i| {
        let (x, y) = ((i % 4) as i32, (i / 4) as i32);
        [
            (40 + 20 * x) as u8,
            (60 + 15 * y) as u8,
            (100 + 6 * x - 6 * y) as u8,
            255,
        ]
    });
    let etc1 = etc1_colour(&px).1.to_be_bytes();
    let chosen = encode_etc_colour(&px);
    let (before, after) = (etc_block_error(&etc1, &px), etc_block_error(&chosen, &px));
    assert!(after < before / 4, "planar {after} vs ETC1 {before}");
    for (d, p) in decode_etc2_rgb(&chosen).iter().zip(&px) {
        for k in 0..3 {
            assert!(
                (i32::from(d[k]) - i32::from(p[k])).abs() <= 3,
                "{d:?} vs {p:?}"
            );
        }
    }
}

/// Level 0 of an image encoded as ETC2 with the colour modes limited to
/// ETC1's (and planar, if `planar`) — the encoder as it was before the
/// later modes, for before/after comparisons.
fn etc2_restricted(img: &[u8], w: usize, h: usize, planar: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for by in 0..h.div_ceil(4) {
        for bx in 0..w.div_ceil(4) {
            let px = gather(img, w, h, bx * 4, by * 4);
            out.extend_from_slice(&encode_eac(&std::array::from_fn(|i| px[i][3])));
            let bits = match flat_rgb(&px) {
                Some(rgb) => etc_flat(rgb),
                None => {
                    let (err, bits) = etc1_colour(&px);
                    let (pe, pb) = etc_planar(&px, true);
                    if planar && pe < err { pb } else { bits }.to_be_bytes()
                }
            };
            out.extend_from_slice(&bits);
        }
    }
    out
}

/// Every T and H block decodes as that mode and reads back the codes it was
/// packed from — exhaustively over every value of the colour fields the
/// mode-deciding bits are taken from (T: R1; H: all of base 1) and every
/// distance, the remaining fields varied.
#[test]
fn t_and_h_blocks_always_decode_as_their_mode() {
    let sel: [u8; 16] = std::array::from_fn(|i| (hash(i as u32) & 3) as u8);
    for c1 in 0..4096u32 {
        for d in 0..8u64 {
            let n = hash(c1 * 8 + d as u32);
            let a = [(c1 >> 8) as i32, (c1 >> 4 & 15) as i32, (c1 & 15) as i32];
            let b = [(n & 15) as i32, (n >> 4 & 15) as i32, (n >> 8 & 15) as i32];
            let t = pack_t(a, b, d, &sel).to_be_bytes();
            assert_eq!(etc2_mode(&t), "T", "{a:?} {b:?} {d}");
            assert_eq!(t_codes(&t), (a, b, d as usize));

            // H stores the distance's low bit as the bases' order.
            let order = |c: [i32; 3]| c[0] * 256 + c[1] * 16 + c[2];
            if order(a) == order(b) && d & 1 == 0 {
                continue;
            }
            let (ha, hb) = if (order(a) >= order(b)) == (d & 1 == 1) {
                (a, b)
            } else {
                (b, a)
            };
            let hblock = pack_h(ha, hb, d, &sel).to_be_bytes();
            assert_eq!(etc2_mode(&hblock), "H", "{ha:?} {hb:?} {d}");
            assert_eq!(h_codes(&hblock), (ha, hb, d as usize));
        }
    }
}

/// A one-texel dark line across a coloured field — a road across landuse,
/// the commonest block on a map. One base per half moving only along grey
/// cannot draw it; two paint colours can. Measured: a dark line over
/// orange 22,196 → 696 squared error (H), a white line over green 4,588 →
/// 204 (T); the test asks for a tenth.
#[test]
fn a_line_across_colour_takes_t_or_h_mode() {
    for (line, field) in [
        ([60u8, 60, 60], [180u8, 120, 60]),
        ([255, 255, 255], [120, 170, 90]),
    ] {
        let px: Block = std::array::from_fn(|i| {
            let (x, y) = (i % 4, i / 4);
            let c = if x == y { line } else { field };
            [c[0], c[1], c[2], 255]
        });
        let before = etc_block_error(&etc1_colour(&px).1.to_be_bytes(), &px);
        let chosen = encode_etc_colour(&px);
        let after = etc_block_error(&chosen, &px);
        let mode = etc2_mode(&chosen);
        eprintln!("line {line:?} over {field:?}: ETC1 error {before}, {mode} {after}");
        assert!(matches!(mode, "T" | "H"), "chose {mode}");
        assert!(after * 10 < before, "{mode} {after} vs ETC1 {before}");
    }
}

/// Dark grey 1-texel lines every 37 texels across and 23 down, over a
/// two-dimensional chroma gradient: roads over landuse. Measured: 28.4 dB
/// with ETC1 and planar only, 40.4 dB with T and H, BC3 39.2 dB. 36 dB
/// sits well clear of the first, so it fails if T/H stops being chosen,
/// and leaves 4 dB for the encoder to get cheaper.
#[test]
fn lines_over_a_chroma_gradient() {
    let (w, h) = (256usize, 256usize);
    let img: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            if x % 37 == 0 || y % 23 == 0 {
                [64, 64, 64, 255]
            } else {
                [x as u8, y as u8, 128, 255]
            }
        })
        .collect();
    let decode = |data: &[u8], f| decode_chain(f, w as u32, h as u32, 1, data).remove(0);
    let level0 = |f| compress_chain(f, w as u32, h as u32, 1, &img);
    let (bc3, _) = psnr(&decode(&level0(BlockFormat::Bc3), BlockFormat::Bc3), &img);
    let (after, _) = psnr(
        &decode(&level0(BlockFormat::Etc2Rgba8), BlockFormat::Etc2Rgba8),
        &img,
    );
    let (before, _) = psnr(
        &decode(&etc2_restricted(&img, w, h, true), BlockFormat::Etc2Rgba8),
        &img,
    );
    eprintln!(
        "lines over gradient: ETC1+planar {before:.2} dB, with T/H {after:.2} dB, BC3 {bc3:.2} dB"
    );
    assert!(
        after > 36.0,
        "with T/H {after:.2} dB (before {before:.2}, BC3 {bc3:.2})"
    );
}

/// A smooth two-dimensional chroma gradient over a whole tile: red with
/// x, green with y. Measured: ETC1 modes alone 40.0 dB, with planar 47.4
/// dB (BC3 39.0 dB).
#[test]
fn a_chroma_gradient_compresses_well_in_etc2() {
    let (w, h) = (256usize, 256usize);
    let img: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i % w) as u8, (i / w) as u8, 128, 255])
        .collect();
    let data = compress_chain(BlockFormat::Etc2Rgba8, w as u32, h as u32, 1, &img);
    let (after, _) = psnr(
        &decode_chain(BlockFormat::Etc2Rgba8, w as u32, h as u32, 1, &data)[0],
        &img,
    );
    let etc1 = etc2_restricted(&img, w, h, false);
    let (before, _) = psnr(
        &decode_chain(BlockFormat::Etc2Rgba8, w as u32, h as u32, 1, &etc1)[0],
        &img,
    );
    eprintln!("chroma gradient: ETC1 modes {before:.2} dB, with planar {after:.2} dB");
    assert!(
        after > 40.0,
        "with planar {after:.2} dB (ETC1 modes alone {before:.2} dB)"
    );
}

/// Every BC1 block in a busy image is in four-colour mode, or has all its
/// indices at 0 (which decodes the same in either mode).
#[test]
fn bc1_blocks_are_always_four_colour() {
    let (w, h) = (64u32, 64u32);
    let mut img = map_like(w as usize, h as usize);
    // Plus some blocks of near-identical colours that quantise together.
    for (i, px) in img.chunks_exact_mut(4).enumerate().take(64 * 8) {
        px[0] = 100 + (hash(i as u32) & 1) as u8;
        px[1] = 100;
        px[2] = 100 + (hash(i as u32 + 9) & 3) as u8;
    }
    let data = compress_chain(BlockFormat::Bc3, w, h, 1, &img);
    for b in data.chunks_exact(16) {
        let c0 = u16::from_le_bytes([b[8], b[9]]);
        let c1 = u16::from_le_bytes([b[10], b[11]]);
        let bits = u32::from_le_bytes([b[12], b[13], b[14], b[15]]);
        assert!(
            c0 > c1 || bits == 0,
            "three-colour BC1 block: {c0:#06x} {c1:#06x} {bits:#010x}"
        );
    }
}

#[test]
#[ignore = "timing, run with --release --nocapture"]
fn timing_of_a_512_chain() {
    let (w, h) = (512u32, 512u32);
    // The map-like image, and the aerial quadrant of a 1024² one — grain
    // and two roads everywhere, the worst case: almost no flat blocks.
    let big = map_like(1024, 1024);
    let aerial: Vec<u8> = (512..1024)
        .flat_map(|y| big[(y * 1024 + 512) * 4..(y * 1024 + 1024) * 4].to_vec())
        .collect();
    for (name, mip0) in [
        ("map-like", map_like(w as usize, h as usize)),
        ("aerial", aerial),
    ] {
        let (chain, count) = box_chain(w, h, mip0);
        for format in FORMATS {
            // Warm the single-colour tables outside the measurement.
            let _ = compress_chain(format, 4, 4, 1, &[0u8; 64]);
            // The fastest of ten: this box is shared, and the minimum is
            // the run nothing else interrupted.
            let mut ms = f64::MAX;
            for _ in 0..10 {
                let start = std::time::Instant::now();
                std::hint::black_box(compress_chain(format, w, h, count, &chain));
                ms = ms.min(start.elapsed().as_secs_f64() * 1000.0);
            }
            eprintln!("{name} {format:?}: 512² chain in {ms:.2} ms");
        }
    }
}
