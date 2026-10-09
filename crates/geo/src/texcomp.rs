//! Block compression of tile imagery, done on the worker that built the mips.
//!
//! Pure: turns the packed RGBA8 chain [`crate::mip::build_mip_chain`] produces
//! into GPU block-compressed data. Choosing the format for the adapter and
//! creating the texture is the client's job.
//!
//! The point is memory, not bandwidth. An RGBA8 texel is 4 bytes and a block
//! format spends 16 bytes on a 4×4 block — 1 byte a texel, a quarter — so a
//! 512² tile with its chain falls from 1.4 MB to 350 kB resident. That is the
//! difference between a phone affording 256 px tiles and affording 512 px ones,
//! since what kills a handset here is the resident total
//! (`docs/notes/upload-bursts.md`), not the rate.
//!
//! Two formats, because no GPU family reads both: **BC3** (`Bc3RgbaUnormSrgb`)
//! on desktop, **ETC2 RGBA8** (`Etc2Rgba8UnormSrgb`) on phones. Both are 8 bytes
//! of alpha followed by 8 bytes of colour, and both keep alpha in its own block
//! with eight levels per block — which is what matters here, because **alpha is
//! the globe's ocean mask** (`globe_ocean`), a hard 0/255 coastline that must
//! not smear. A mask block of only 0 and 255 decodes exactly in both; the tests
//! pin it.
//!
//! The encoders are deliberately the fast, simple kind: this runs once per tile
//! on a phone CPU, ahead of the upload, and a slow encoder would only move the
//! cost from GPU memory to the time a tile takes to appear.
//!
//! - **BC1 colour** (inside BC3): endpoints from the principal axis of the
//!   block's colours (four rounds of power iteration on the covariance), the
//!   extreme pixels along it as endpoints, nearest-of-four indices, then one
//!   least-squares refit of both endpoints to those indices, kept only if it
//!   measures better. Always four-colour mode (`color0 > color1`); when both
//!   endpoints quantise to one 565 value every index is 0, which decodes the
//!   same under either mode. A flat block takes the single-colour tables
//!   (`stb_dxt`'s trick: the pair of 565 endpoints whose ⅔ interpolant lands
//!   nearest the colour), which hold a flat colour to within ~1 level rather
//!   than 565's ±4.
//! - **BC4 alpha** (inside BC3): min/max as endpoints in the eight-value mode,
//!   a least-squares refit, and — when the block contains 0 or 255 beside other
//!   values, i.e. an anti-aliased coastline — the six-value mode with its
//!   explicit 0 and 255 is tried too and the better one kept.
//! - **ETC2 colour**: every mode but ETC2's punch-through — the
//!   ETC1-compatible individual and differential modes, planar, T and H —
//!   keeping whichever measures the lowest error.
//!   - *Individual / differential*: the sub-block average as base and each
//!     pixel's best modifier. Differential only when both bases are real 5-bit
//!     values within the 3-bit delta; a delta that pushed the second base
//!     outside 0..31 would be read as T, H or planar instead. Two prunings make
//!     the search affordable: only the flip orientation whose halves spread
//!     less about their means is fitted (`ETC_FLIP_SLACK`), and of the eight
//!     tables only the three a closed-form estimate ranks best are measured
//!     exactly (`ETC_SHORTLIST`) — together 0.8 dB for half the time.
//!   - *Planar*: a least-squares plane per channel, RGB 6:7:6, tried on every
//!     non-flat block, its ±1 code refinement only once it has won. The only
//!     mode that follows a chroma change inside a block: a smooth gradient goes
//!     from 40.0 dB to 47.4 dB with it.
//!   - *T and H*: two paint colours — a road across landuse, the commonest
//!     block on a map, which one base per half moving only along grey cannot
//!     draw. The block is split in two by 2-means seeded along its principal
//!     axis; T paints one cluster flat and the other ± a distance, H both ± a
//!     distance; three distances around the clusters' measured spread are
//!     tried rather than all eight. Only for blocks still worse than an RMS of
//!     6 levels after the modes above (`ETC_TH_THRESHOLD`). Lines over a
//!     chroma gradient go from 28.4 dB to 40.4 dB (BC3: 39.2 dB).
//!
//!   Each of the three ETC2 modes is signalled by a "differential" block whose
//!   red (T), green (H) or blue (planar) delta overflows; bits that carry no
//!   colour are set to make exactly that overflow happen, and a test walks
//!   every value of the fields that decide it.
//! - **EAC alpha** (inside ETC2 RGBA8): all sixteen modifier tables with a
//!   multiplier and base fitted to the block's range, then a ±1 search on the
//!   multiplier and base of the best. Multiplier 0 is never written.
//!
//! All of it works on the sRGB-encoded bytes as stored — the formats are
//! sampled as sRGB, so the error that matters is the error in encoded space.
//! No SIMD intrinsics: plain integer loops over 16 texels, which the compiler
//! vectorises well enough on both x86-64 and aarch64.
//!
//! Each level is cut into whole 4×4 blocks, the edge texel repeated to fill the
//! ones that overhang it, including the levels smaller than a block — the GPU
//! reads a 2×1 level out of one full block.
//!
//! Measured 2026-10-09 with `timing_of_a_512_chain` (ignored; `cargo test -p
//! cartopolis_geo --release timing_of_a_512_chain -- --ignored --nocapture`),
//! one thread on the VPS's x86-64 with `target-cpu=x86-64-v3` (what
//! `.cargo/config.toml` sets), the fastest of ten runs of a whole 512² chain:
//! on the synthetic map-like image **BC3 6.3 ms, ETC2 14.4 ms**; on the worst
//! case, an image that is all aerial grain with no flat blocks, BC3 13.7 ms and
//! ETC2 21.9 ms. The box was loaded (load average 9 on 8 cores) and is shared,
//! so treat these as ±15 %. Of the ETC2 figure, planar is ~3 ms and T/H with
//! its 2-means ~2.5 ms. Nothing has been timed on a phone. Quality on the 256²
//! map-like image, level 0: BC3 33.7 dB RGB, ETC2 37.8 dB (34.4 before T and
//! H), alpha ~59 dB in both — the image is built to be hard (hairlines and
//! per-pixel grain), see `a_map_tile_compresses_with_good_quality`.

use std::sync::OnceLock;

/// The GPU block format a chain is compressed into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockFormat {
    /// BC3 / DXT5: a BC4 alpha block, then a BC1 colour block. wgpu's
    /// `Bc3RgbaUnormSrgb`, the desktop format.
    Bc3,
    /// ETC2 RGBA8: an EAC alpha block, then an ETC2 colour block. wgpu's
    /// `Etc2Rgba8UnormSrgb`, the mobile format.
    Etc2Rgba8,
}

impl BlockFormat {
    /// Bytes one 4×4 block occupies.
    pub const fn block_bytes(self) -> usize {
        match self {
            Self::Bc3 | Self::Etc2Rgba8 => 16,
        }
    }
}

/// Dimensions of each level of a chain, level 0 first — the same halving
/// `build_mip_chain` does (`(w / 2).max(1)` per step).
fn levels(width: u32, height: u32, mip_count: u32) -> impl Iterator<Item = (usize, usize)> {
    (0..mip_count).map(move |l| {
        let w = width.checked_shr(l).unwrap_or(0).max(1);
        let h = height.checked_shr(l).unwrap_or(0).max(1);
        (w as usize, h as usize)
    })
}

/// Bytes the compressed form of a packed chain occupies (each level padded to
/// whole 4×4 blocks).
pub fn compressed_chain_len(format: BlockFormat, width: u32, height: u32, mip_count: u32) -> usize {
    levels(width, height, mip_count)
        .map(|(w, h)| w.div_ceil(4) * h.div_ceil(4) * format.block_bytes())
        .sum()
}

/// Compress a packed RGBA8 mip chain as produced by
/// [`crate::mip::build_mip_chain`]. Each level is padded to whole 4×4 blocks by
/// clamping to the edge texel (levels smaller than 4×4 included — wgpu expects
/// a full block for them). Output: the levels' blocks concatenated in level
/// order, blocks row-major within a level.
///
/// Panics if `chain` is shorter than the levels `width`, `height` and
/// `mip_count` describe.
pub fn compress_chain(
    format: BlockFormat,
    width: u32,
    height: u32,
    mip_count: u32,
    chain: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(compressed_chain_len(format, width, height, mip_count));
    // Map tiles are dominated by large areas of one colour — paper, water,
    // fields — so the last flat block's encoding is very often the next one's.
    let mut flat = FlatCache::default();
    let mut offset = 0;
    for (w, h) in levels(width, height, mip_count) {
        let len = w * h * 4;
        assert!(
            chain.len() >= offset + len,
            "mip chain is {} bytes, its levels need at least {}",
            chain.len(),
            offset + len
        );
        let level = &chain[offset..offset + len];
        offset += len;
        for by in 0..h.div_ceil(4) {
            for bx in 0..w.div_ceil(4) {
                let px = gather(level, w, h, bx * 4, by * 4);
                let block = match format {
                    BlockFormat::Bc3 => encode_bc3(&px, &mut flat),
                    BlockFormat::Etc2Rgba8 => encode_etc2_rgba8(&px, &mut flat),
                };
                out.extend_from_slice(&block);
            }
        }
    }
    out
}

/// One 4×4 block of RGBA texels, row-major (`y * 4 + x`).
type Block = [[u8; 4]; 16];

/// Read the block whose top-left texel is `(x0, y0)`, repeating the last
/// column/row for texels past the level's edge.
fn gather(level: &[u8], w: usize, h: usize, x0: usize, y0: usize) -> Block {
    let mut px = [[0u8; 4]; 16];
    for y in 0..4 {
        let sy = (y0 + y).min(h - 1);
        for x in 0..4 {
            let sx = (x0 + x).min(w - 1);
            let s = (sy * w + sx) * 4;
            px[y * 4 + x] = [level[s], level[s + 1], level[s + 2], level[s + 3]];
        }
    }
    px
}

/// The encoded colour half of the last flat block, keyed by its colour.
#[derive(Default)]
struct FlatCache {
    colour: Option<([u8; 3], [u8; 8])>,
}

impl FlatCache {
    fn get_or(&mut self, rgb: [u8; 3], encode: impl FnOnce([u8; 3]) -> [u8; 8]) -> [u8; 8] {
        if let Some((key, block)) = self.colour {
            if key == rgb {
                return block;
            }
        }
        let block = encode(rgb);
        self.colour = Some((rgb, block));
        block
    }
}

/// The block's colour if every texel has the same RGB.
fn flat_rgb(px: &Block) -> Option<[u8; 3]> {
    let first = [px[0][0], px[0][1], px[0][2]];
    px.iter()
        .all(|p| p[0] == first[0] && p[1] == first[1] && p[2] == first[2])
        .then_some(first)
}

fn rgb_i32(p: [u8; 4]) -> [i32; 3] {
    [i32::from(p[0]), i32::from(p[1]), i32::from(p[2])]
}

fn dist3(a: [i32; 3], b: [i32; 3]) -> u32 {
    let (dr, dg, db) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    (dr * dr + dg * dg + db * db) as u32
}

// ---------------------------------------------------------------------------
// BC3 = BC4 alpha + BC1 colour
// ---------------------------------------------------------------------------

fn encode_bc3(px: &Block, flat: &mut FlatCache) -> [u8; 16] {
    let alpha: [u8; 16] = std::array::from_fn(|i| px[i][3]);
    let a = encode_bc4(&alpha);
    let c = match flat_rgb(px) {
        Some(rgb) => flat.get_or(rgb, bc1_flat),
        None => encode_bc1(px),
    };
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&a);
    out[8..].copy_from_slice(&c);
    out
}

/// A BC4 candidate: endpoints, per-texel 3-bit indices (row-major) and its
/// squared error.
struct Bc4Fit {
    err: u32,
    a0: u8,
    a1: u8,
    idx: [u8; 16],
}

/// The eight values a BC4 block's indices select. `a0 > a1` is the mode with
/// six interpolants; otherwise four interpolants plus explicit 0 and 255.
fn bc4_palette(a0: u8, a1: u8) -> [i32; 8] {
    let (e0, e1) = (i32::from(a0), i32::from(a1));
    let mut p = [0i32; 8];
    p[0] = e0;
    p[1] = e1;
    if a0 > a1 {
        for (i, v) in p.iter_mut().enumerate().skip(2) {
            let i = i as i32;
            *v = ((8 - i) * e0 + (i - 1) * e1 + 3) / 7;
        }
    } else {
        for (i, v) in p.iter_mut().enumerate().take(6).skip(2) {
            let i = i as i32;
            *v = ((6 - i) * e0 + (i - 1) * e1 + 2) / 5;
        }
        p[6] = 0;
        p[7] = 255;
    }
    p
}

fn bc4_fit(a: &[u8; 16], a0: u8, a1: u8) -> Bc4Fit {
    let pal = bc4_palette(a0, a1);
    let mut idx = [0u8; 16];
    let mut err = 0u32;
    for (o, &v) in idx.iter_mut().zip(a) {
        let v = i32::from(v);
        let (mut bd, mut bs) = (i32::MAX, 0);
        for (s, &p) in pal.iter().enumerate() {
            let d = (p - v) * (p - v);
            if d < bd {
                bd = d;
                bs = s;
            }
        }
        *o = bs as u8;
        err += bd as u32;
    }
    Bc4Fit { err, a0, a1, idx }
}

/// Least-squares endpoints for the eight-value mode given a fit's indices.
fn bc4_refit(a: &[u8; 16], fit: &Bc4Fit) -> Option<(u8, u8)> {
    // Weight of a0 per index, in sevenths: index 0 is a0, 1 is a1, i ≥ 2 is
    // ((8 − i)·a0 + (i − 1)·a1) / 7.
    const W0: [f32; 8] = [7.0, 0.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0];
    let (mut saa, mut sab, mut sbb, mut sav, mut sbv) = (0f32, 0f32, 0f32, 0f32, 0f32);
    for (&s, &v) in fit.idx.iter().zip(a) {
        let al = W0[s as usize] / 7.0;
        let be = 1.0 - al;
        let v = f32::from(v);
        saa += al * al;
        sab += al * be;
        sbb += be * be;
        sav += al * v;
        sbv += be * v;
    }
    let det = saa * sbb - sab * sab;
    if det.abs() < 1e-6 {
        return None;
    }
    let e0 = ((sav * sbb - sbv * sab) / det).round().clamp(0.0, 255.0) as u8;
    let e1 = ((sbv * saa - sav * sab) / det).round().clamp(0.0, 255.0) as u8;
    Some((e0, e1))
}

fn encode_bc4(a: &[u8; 16]) -> [u8; 8] {
    let (mut lo, mut hi) = (255u8, 0u8);
    for &v in a {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if lo == hi {
        // Index 0 is a0 in either mode.
        return pack_bc4(hi, hi, &[0; 16]);
    }
    let mut best = bc4_fit(a, hi, lo);
    if let Some((e0, e1)) = bc4_refit(a, &best) {
        if e0 > e1 && (e0, e1) != (best.a0, best.a1) {
            let c = bc4_fit(a, e0, e1);
            if c.err < best.err {
                best = c;
            }
        }
    }
    // An anti-aliased coastline: 0 or 255 beside values in between. The
    // six-value mode spends its interpolants on the in-between values and gets
    // the extremes for free.
    if best.err > 0 && (lo == 0 || hi == 255) {
        let (mut ilo, mut ihi) = (255u8, 0u8);
        for &v in a {
            if v != 0 && v != 255 {
                ilo = ilo.min(v);
                ihi = ihi.max(v);
            }
        }
        if ilo <= ihi {
            let c = bc4_fit(a, ilo, ihi);
            if c.err < best.err {
                best = c;
            }
        }
    }
    pack_bc4(best.a0, best.a1, &best.idx)
}

/// Two endpoint bytes, then 48 bits of 3-bit indices, little-endian, texel
/// `y * 4 + x` at bit `3 · (y * 4 + x)`.
fn pack_bc4(a0: u8, a1: u8, idx: &[u8; 16]) -> [u8; 8] {
    let mut bits = u64::from(a0) | u64::from(a1) << 8;
    for (i, &s) in idx.iter().enumerate() {
        bits |= u64::from(s) << (16 + 3 * i);
    }
    bits.to_le_bytes()
}

fn rgb565(r: u32, g: u32, b: u32) -> u16 {
    (r << 11 | g << 5 | b) as u16
}

/// Nearest 565 value to an 8-bit colour.
fn quant565(c: [f32; 3]) -> u16 {
    let q = |v: f32, max: f32| (v * max / 255.0).round().clamp(0.0, max) as u32;
    rgb565(q(c[0], 31.0), q(c[1], 63.0), q(c[2], 31.0))
}

fn expand565(c: u16) -> [i32; 3] {
    let c = i32::from(c);
    let (r, g, b) = (c >> 11 & 31, c >> 5 & 63, c & 31);
    [r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2]
}

/// The four-colour-mode palette of two 565 endpoints.
fn bc1_palette(c0: u16, c1: u16) -> [[i32; 3]; 4] {
    let (e0, e1) = (expand565(c0), expand565(c1));
    let mix = |a: [i32; 3], b: [i32; 3]| std::array::from_fn(|k| (2 * a[k] + b[k] + 1) / 3);
    [e0, e1, mix(e0, e1), mix(e1, e0)]
}

/// Nearest palette entry per texel, and the total squared error.
fn bc1_assign(px: &Block, c0: u16, c1: u16) -> (u32, [u8; 16]) {
    let pal = bc1_palette(c0, c1);
    let mut idx = [0u8; 16];
    let mut err = 0u32;
    for (o, p) in idx.iter_mut().zip(px) {
        let p = rgb_i32(*p);
        let (mut bd, mut bs) = (u32::MAX, 0);
        for (s, &c) in pal.iter().enumerate() {
            let d = dist3(p, c);
            if d < bd {
                bd = d;
                bs = s;
            }
        }
        *o = bs as u8;
        err += bd;
    }
    (err, idx)
}

/// The principal axis of a block's colours: four rounds of power iteration on
/// their covariance. Not normalised; only its direction is used.
fn principal_axis(px: &Block) -> [f32; 3] {
    let mut mean = [0f32; 3];
    for p in px {
        for k in 0..3 {
            mean[k] += f32::from(p[k]);
        }
    }
    for m in &mut mean {
        *m /= 16.0;
    }
    let mut cov = [[0f32; 3]; 3];
    for p in px {
        let d: [f32; 3] = std::array::from_fn(|k| f32::from(p[k]) - mean[k]);
        for (i, row) in cov.iter_mut().enumerate() {
            for (j, c) in row.iter_mut().enumerate() {
                *c += d[i] * d[j];
            }
        }
    }
    // Power iteration from the row of the channel with the most variance, which
    // cannot be orthogonal to the principal axis the way a fixed start can.
    let k = (0..3)
        .max_by(|&a, &b| cov[a][a].total_cmp(&cov[b][b]))
        .unwrap_or(0);
    let mut axis = cov[k];
    for _ in 0..4 {
        let next: [f32; 3] = std::array::from_fn(|i| {
            cov[i][0] * axis[0] + cov[i][1] * axis[1] + cov[i][2] * axis[2]
        });
        let m = next.iter().fold(0f32, |m, v| m.max(v.abs()));
        if m < 1e-12 {
            break;
        }
        axis = next.map(|v| v / m);
    }

    axis
}

fn encode_bc1(px: &Block) -> [u8; 8] {
    let axis = principal_axis(px);

    // The pixels furthest apart along it are the starting endpoints.
    let (mut imin, mut imax, mut dmin, mut dmax) = (0, 0, f32::MAX, f32::MIN);
    for (i, p) in px.iter().enumerate() {
        let d = f32::from(p[0]) * axis[0] + f32::from(p[1]) * axis[1] + f32::from(p[2]) * axis[2];
        if d < dmin {
            dmin = d;
            imin = i;
        }
        if d > dmax {
            dmax = d;
            imax = i;
        }
    }
    let as_f32 = |p: [u8; 4]| [f32::from(p[0]), f32::from(p[1]), f32::from(p[2])];
    let (mut c0, mut c1) = (quant565(as_f32(px[imax])), quant565(as_f32(px[imin])));
    let (err, mut idx) = bc1_assign(px, c0, c1);

    // One least-squares refit of both endpoints to the chosen indices. The
    // weight of c0 per index, in thirds: 0 → 3, 1 → 0, 2 → 2, 3 → 1.
    if err > 0 {
        const W0: [i32; 4] = [3, 0, 2, 1];
        let (mut sxx, mut sxy, mut syy) = (0i32, 0i32, 0i32);
        let (mut sxp, mut syp) = ([0i32; 3], [0i32; 3]);
        for (&s, p) in idx.iter().zip(px) {
            let x = W0[s as usize];
            let y = 3 - x;
            sxx += x * x;
            sxy += x * y;
            syy += y * y;
            for k in 0..3 {
                sxp[k] += x * i32::from(p[k]);
                syp[k] += y * i32::from(p[k]);
            }
        }
        let det = sxx * syy - sxy * sxy;
        if det != 0 {
            let det = det as f32;
            let e0: [f32; 3] =
                std::array::from_fn(|k| 3.0 * (sxp[k] * syy - syp[k] * sxy) as f32 / det);
            let e1: [f32; 3] =
                std::array::from_fn(|k| 3.0 * (syp[k] * sxx - sxp[k] * sxy) as f32 / det);
            let (r0, r1) = (quant565(e0), quant565(e1));
            if (r0, r1) != (c0, c1) {
                let (e, i) = bc1_assign(px, r0, r1);
                if e < err {
                    (c0, c1, idx) = (r0, r1, i);
                }
            }
        }
    }
    pack_bc1(c0, c1, idx)
}

/// Two 565 endpoints little-endian, then 32 bits of 2-bit indices, texel
/// `y * 4 + x` at bit `2 · (y * 4 + x)`. Forces four-colour mode: swaps the
/// endpoints (and relabels the indices) when `c0 < c1`, and zeroes the indices
/// when they are equal — index 0 is `c0` in both modes.
fn pack_bc1(mut c0: u16, mut c1: u16, mut idx: [u8; 16]) -> [u8; 8] {
    if c0 < c1 {
        std::mem::swap(&mut c0, &mut c1);
        // 0 ↔ 1 and ⅔ ↔ ⅓: the same colours with the ends exchanged.
        for s in &mut idx {
            *s ^= 1;
        }
    }
    if c0 == c1 {
        idx = [0; 16];
    }
    let mut bits = 0u32;
    for (i, &s) in idx.iter().enumerate() {
        bits |= u32::from(s) << (2 * i);
    }
    let mut out = [0u8; 8];
    out[..2].copy_from_slice(&c0.to_le_bytes());
    out[2..4].copy_from_slice(&c1.to_le_bytes());
    out[4..].copy_from_slice(&bits.to_le_bytes());
    out
}

/// For every 8-bit value, the (c0, c1) channel codes whose ⅔ interpolant
/// `(2·c0 + c1) / 3` lands nearest it — 5-bit and 6-bit tables. Ties go to
/// the closer pair: D3D lets a decoder's interpolant be off by 3 % of the
/// endpoint distance, so the score charges that much.
fn bc1_single_tables() -> &'static ([[u8; 2]; 256], [[u8; 2]; 256]) {
    static TABLES: OnceLock<([[u8; 2]; 256], [[u8; 2]; 256])> = OnceLock::new();
    TABLES.get_or_init(|| {
        let build = |bits: u32| -> [[u8; 2]; 256] {
            let n = 1i32 << bits;
            let expand = |c: i32| {
                if bits == 5 {
                    c << 3 | c >> 2
                } else {
                    c << 2 | c >> 4
                }
            };
            std::array::from_fn(|v| {
                let v = v as f32;
                let mut best = (f32::MAX, [0u8; 2]);
                for a in 0..n {
                    for b in 0..n {
                        let (ea, eb) = (expand(a), expand(b));
                        let interp = (2 * ea + eb) as f32 / 3.0;
                        let score = (interp - v).abs() + 0.03 * (ea - eb).abs() as f32;
                        if score < best.0 {
                            best = (score, [a as u8, b as u8]);
                        }
                    }
                }
                best.1
            })
        };
        (build(5), build(6))
    })
}

fn bc1_flat(rgb: [u8; 3]) -> [u8; 8] {
    let (t5, t6) = bc1_single_tables();
    let [ra, rb] = t5[rgb[0] as usize];
    let [ga, gb] = t6[rgb[1] as usize];
    let [ba, bb] = t5[rgb[2] as usize];
    let c0 = rgb565(ra.into(), ga.into(), ba.into());
    let c1 = rgb565(rb.into(), gb.into(), bb.into());
    pack_bc1(c0, c1, [2; 16])
}

// ---------------------------------------------------------------------------
// ETC2 RGBA8 = EAC alpha + ETC2 colour
// ---------------------------------------------------------------------------

/// ETC1/ETC2 intensity modifier tables: `[a, b]`, selected per pixel as
/// `+a, +b, −a, −b` by the pixel index `msb·2 + lsb`.
const ETC1_MODS: [[i32; 2]; 8] = [
    [2, 8],
    [5, 17],
    [9, 29],
    [13, 42],
    [18, 60],
    [24, 80],
    [33, 106],
    [47, 183],
];

/// EAC alpha modifier tables, indexed by the 3-bit pixel index.
const EAC_MODS: [[i32; 8]; 16] = [
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

/// The two sub-blocks of each flip orientation, as row-major texel indices.
/// Flip 0 splits left | right (2×4 each), flip 1 top / bottom (4×2 each).
const ETC_SUB: [[[usize; 8]; 2]; 2] = [
    [[0, 1, 4, 5, 8, 9, 12, 13], [2, 3, 6, 7, 10, 11, 14, 15]],
    [[0, 1, 2, 3, 4, 5, 6, 7], [8, 9, 10, 11, 12, 13, 14, 15]],
];

fn expand4(c: i32) -> i32 {
    c << 4 | c
}

fn expand5(c: i32) -> i32 {
    c << 3 | c >> 2
}

fn encode_etc2_rgba8(px: &Block, flat: &mut FlatCache) -> [u8; 16] {
    let alpha: [u8; 16] = std::array::from_fn(|i| px[i][3]);
    let a = encode_eac(&alpha);
    let c = match flat_rgb(px) {
        Some(rgb) => flat.get_or(rgb, etc_flat),
        None => encode_etc_colour(px),
    };
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&a);
    out[8..].copy_from_slice(&c);
    out
}

/// One sub-block's best table and per-pixel selectors for a given base.
struct SubFit {
    err: u32,
    table: u8,
    sel: [u8; 8],
}

/// How many of the eight tables `etc_fit_sub` measures exactly.
const ETC_SHORTLIST: usize = 3;

/// Pick a sub-block's table and per-pixel modifiers for a given base.
///
/// A modifier `m` moves all three channels together, so while nothing clamps,
/// the error of pixel `p` against `base + m` is `|p − base|² − 2m·Σ(p − base)
/// + 3m²` — one number per pixel decides every table, and ranking all eight
/// costs a few integer ops a pixel. That ranking ignores clamping, which
/// matters on a map (pale paper sits near 255, where the large modifiers
/// clamp and fit better than the formula says), so it only shortlists: the
/// best `ETC_SHORTLIST` tables by the formula are then measured exactly, with
/// the clamp, and the least squared error wins.
fn etc_fit_sub(px: &Block, members: &[usize; 8], base: [i32; 3]) -> SubFit {
    let mut ch = [[0i32; 8]; 3];
    let mut ds = [0i32; 8];
    for (k, &i) in members.iter().enumerate() {
        for (c, lane) in ch.iter_mut().enumerate() {
            lane[k] = i32::from(px[i][c]);
            ds[k] += lane[k] - base[c];
        }
    }
    let mut rank: [(i32, usize); 8] = std::array::from_fn(|t| {
        let [a, b] = ETC1_MODS[t];
        let cost = ds
            .iter()
            .map(|d| {
                let d = d.abs();
                (3 * a * a - 2 * a * d).min(3 * b * b - 2 * b * d)
            })
            .sum::<i32>();
        (cost, t)
    });
    rank.sort_unstable();

    let mut best = SubFit {
        err: u32::MAX,
        table: 0,
        sel: [0; 8],
    };
    for &(_, t) in &rank[..ETC_SHORTLIST] {
        let [a, b] = ETC1_MODS[t];
        // Error and selector in one key, `err << 2 | selector` (the error is
        // at most 3 · 255², so it fits), so the select is a plain min.
        let mut key = [i32::MAX; 8];
        for (s, m) in [a, b, -a, -b].into_iter().enumerate() {
            let c = base.map(|v| (v + m).clamp(0, 255));
            for k in 0..8 {
                let (dr, dg, db) = (ch[0][k] - c[0], ch[1][k] - c[1], ch[2][k] - c[2]);
                key[k] = key[k].min((dr * dr + dg * dg + db * db) << 2 | s as i32);
            }
        }
        let err = key.iter().map(|k| k >> 2).sum::<i32>() as u32;
        if err < best.err {
            best = SubFit {
                err,
                table: t as u8,
                sel: key.map(|k| (k & 3) as u8),
            };
        }
    }
    best
}

/// A flip orientation is fitted only if its sub-blocks' spread about their
/// own means is within this factor of the tighter orientation's. The spread
/// is what the sub-block's single base cannot follow, so the looser split is
/// rarely the better one, and fitting it is half the encoder's time.
const ETC_FLIP_SLACK: f32 = 1.0;

/// The colour half of an ETC2 block that is not flat: the best ETC1-style
/// encoding, planar where planar measures better, and T or H where the block
/// is still badly served after both.
fn encode_etc_colour(px: &Block) -> [u8; 8] {
    let (mut err, mut bits) = etc1_colour(px);
    if err > 0 {
        let (planar_err, _) = etc_planar(px, false);
        if planar_err < err {
            (err, bits) = etc_planar(px, true);
        }
    }
    if err > ETC_TH_THRESHOLD {
        let colours = two_colours(px);
        for (e, b) in [etc_t_mode(px, &colours), etc_h_mode(px, &colours)] {
            if e < err {
                (err, bits) = (e, b);
            }
        }
    }
    bits.to_be_bytes()
}

/// The best ETC1-compatible (individual or differential) encoding of a
/// non-flat block, and its squared error.
fn etc1_colour(px: &Block) -> (u32, u64) {
    // Per orientation and sub-block: channel sums, and the spread about the mean.
    let mut sums = [[[0u32; 3]; 2]; 2];
    let mut spread = [0f32; 2];
    for (flip, subs) in ETC_SUB.iter().enumerate() {
        for (sub, members) in subs.iter().enumerate() {
            let mut sq = [0u32; 3];
            for &i in members {
                for k in 0..3 {
                    let v = u32::from(px[i][k]);
                    sums[flip][sub][k] += v;
                    sq[k] += v * v;
                }
            }
            for k in 0..3 {
                let s = sums[flip][sub][k] as f32;
                spread[flip] += sq[k] as f32 - s * s / 8.0;
            }
        }
    }
    let tight = spread[0].min(spread[1]);

    let mut best: Option<(u32, u64)> = None;
    for (flip, subs) in ETC_SUB.iter().enumerate() {
        if spread[flip] > tight * ETC_FLIP_SLACK + 1.0 {
            continue;
        }
        let avg = sums[flip].map(|c| c.map(|v| v as f32 / 8.0));
        let q5 = avg.map(|c| c.map(|v| (v * 31.0 / 255.0).round() as i32));
        // Differential only while both bases are real 5-bit values within the
        // delta's −4..=3: anything else an ETC2 decoder reads as T/H/planar.
        let diff = (0..3).all(|k| (-4..=3).contains(&(q5[1][k] - q5[0][k])));
        let (codes, bases) = if diff {
            (q5, q5.map(|c| c.map(expand5)))
        } else {
            let q4 = avg.map(|c| c.map(|v| (v * 15.0 / 255.0).round() as i32));
            (q4, q4.map(|c| c.map(expand4)))
        };
        let f0 = etc_fit_sub(px, &subs[0], bases[0]);
        let f1 = etc_fit_sub(px, &subs[1], bases[1]);
        if best.is_some_and(|b| b.0 <= f0.err + f1.err) {
            continue;
        }
        let bits = pack_etc_colour(
            diff,
            flip == 1,
            codes,
            [f0.table, f1.table],
            [f0.sel, f1.sel],
            subs,
        );
        best = Some((f0.err + f1.err, bits));
    }
    // The tighter orientation always passes the slack test.
    best.unwrap_or((u32::MAX, 0))
}

/// Column and row of each texel of a block, row-major.
const X: [i32; 16] = [0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3];
const Y: [i32; 16] = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3];

/// ETC2's planar mode: three colours, O at the block's top-left texel, H four
/// texels to its right and V four below, in RGB 6:7:6, and every texel the
/// plane through them — `(x·(H − O) + y·(V − O) + 4·O + 2) >> 2` per channel.
/// It is the one mode that can follow a *chroma* change inside a block: the
/// ETC1 modes give each half a single base colour and move it only along grey.
/// On the 256² red-with-x, green-with-y gradient in the tests the ETC1 modes
/// alone measure 40.0 dB and with planar 47.4 dB (BC3: 39.0 dB); a steeper
/// gradient widens the gap (8 levels a texel: 33.4 dB against 45.0).
///
/// The fit is the least-squares plane per channel, quantised; its error is
/// measured with the decoder's own rounding so it competes fairly with the
/// ETC1 modes. `refine` then nudges each of the nine codes by ±1 where that
/// measures better — worth 2.4 dB on a smooth gradient, and as much time
/// again as the rest of the encoder if done for every block, so the caller
/// asks for it only once planar has already won.
fn etc_planar(px: &Block, refine: bool) -> (u32, u64) {
    // Per channel: Σv, Σ(2x − 3)·v, Σ(2y − 3)·v — the plane's moments about
    // the block centre, in integers.
    let (mut sum, mut sx, mut sy) = ([0i32; 3], [0i32; 3], [0i32; 3]);
    let mut ch = [[0i32; 16]; 3];
    for (i, p) in px.iter().enumerate() {
        for k in 0..3 {
            let v = i32::from(p[k]);
            ch[k][i] = v;
            sum[k] += v;
            sx[k] += (2 * X[i] - 3) * v;
            sy[k] += (2 * Y[i] - 3) * v;
        }
    }
    // [O, H, V][channel] codes.
    let mut codes = [[0i32; 3]; 3];
    let mut err = 0u32;
    for k in 0..3 {
        // Σ(2x − 3)² over the block is 80, so the slope per texel is sx / 40.
        let (gx, gy) = (sx[k] as f32 / 40.0, sy[k] as f32 / 40.0);
        let o = sum[k] as f32 / 16.0 - 1.5 * (gx + gy);
        let max = if k == 1 { 127 } else { 63 };
        let q = |v: f32| (v * max as f32 / 255.0).round().clamp(0.0, max as f32) as i32;
        let mut c = [q(o), q(o + 4.0 * gx), q(o + 4.0 * gy)];
        let mut e = planar_channel_error(&ch[k], c, k == 1);
        if refine {
            for j in 0..3 {
                for step in [-1, 1] {
                    let mut t = c;
                    t[j] = (t[j] + step).clamp(0, max);
                    let te = planar_channel_error(&ch[k], t, k == 1);
                    if te < e {
                        (c, e) = (t, te);
                    }
                }
            }
        }
        err += e as u32;
        for (slot, code) in codes.iter_mut().zip(c) {
            slot[k] = code;
        }
    }
    (err, pack_planar(codes))
}

/// Squared error of one channel of a planar block against its 16 texels.
fn planar_channel_error(texels: &[i32; 16], codes: [i32; 3], green: bool) -> i32 {
    let [eo, eh, ev] = codes.map(|v| if green { expand7(v) } else { expand6(v) });
    let (dx, dy, base) = (eh - eo, ev - eo, 4 * eo + 2);
    let mut e = 0;
    for i in 0..16 {
        let d = ((X[i] * dx + Y[i] * dy + base) >> 2).clamp(0, 255) - texels[i];
        e += d * d;
    }
    e
}

fn expand6(c: i32) -> i32 {
    c << 2 | c >> 4
}

fn expand7(c: i32) -> i32 {
    c << 1 | c >> 6
}

/// The planar layout (Khronos Data Format spec, ETC2 "planar mode"):
/// RO 62..57; GO 56 and 54..49; BO 48, 44..43 and 41..39; RH 38..34 and 32;
/// GH 31..25; BH 24..19; RV 18..13; GV 12..6; BV 5..0; the differential bit
/// (33) set. What makes a decoder take it as planar is the differential
/// reading of the same bits: red must *not* overflow (else T mode), green must
/// not (else H mode) and blue must. Bits 63, 55, 47..45 and 42 carry no colour
/// and are set to make that so.
fn pack_planar(codes: [[i32; 3]; 3]) -> u64 {
    let [o, h, v] = codes.map(|c| c.map(|x| x as u64));
    let mut w = 1u64 << 33;
    w |= (o[0] & 63) << 57;
    w |= (o[1] >> 6 & 1) << 56 | (o[1] & 63) << 49;
    w |= (o[2] >> 5 & 1) << 48 | (o[2] >> 3 & 3) << 43 | (o[2] & 7) << 39;
    w |= (h[0] >> 1 & 31) << 34 | (h[0] & 1) << 32;
    w |= (h[1] & 127) << 25;
    w |= (h[2] & 63) << 19;
    w |= (v[0] & 63) << 13;
    w |= (v[1] & 127) << 6;
    w |= v[2] & 63;

    // The 3-bit two's-complement delta at `lo` added to the 5-bit base above it.
    let sum = |w: u64, lo: u32| {
        let base = (w >> (lo + 3) & 31) as i32;
        let delta = (w >> lo & 7) as i32;
        base + if delta >= 4 { delta - 8 } else { delta }
    };
    // Red and green: with the free top bit clear the base is at most 15, so
    // only an underflow is possible, and setting it lifts the base to 16..=19.
    if sum(w, 56) < 0 {
        w |= 1 << 63;
    }
    if sum(w, 48) < 0 {
        w |= 1 << 55;
    }
    // Blue must overflow. With u = bits 44..43 and v = bits 41..40, clearing
    // 47..45 and setting 42 gives u + v − 4, below 0 when u + v < 4; setting
    // 47..45 and clearing 42 gives 28 + u + v, above 31 otherwise.
    let (u, v) = (w >> 43 & 3, w >> 40 & 3);
    if u + v < 4 {
        w |= 1 << 42;
    } else {
        w |= 7 << 45;
    }
    w
}

/// T and H modes are tried only on a block whose best ETC1/planar encoding
/// is still worse than this squared error — 16 texels × 3 channels at an RMS
/// of 6 levels. Below it a block is flat or smooth enough that two colours
/// rarely win by much. Measured on the test images against an RMS of 4: 0.06
/// dB lost on both, 2.3 ms saved on the 512² chain; at 8 the lines image
/// starts to lose (0.3 dB) and at 12 it loses 2.2 dB.
const ETC_TH_THRESHOLD: u32 = 16 * 3 * 6 * 6;

/// The distance table of ETC2's T and H modes.
const ETC_TH_DIST: [i32; 8] = [3, 6, 11, 16, 23, 32, 41, 64];

/// Split a block into two colours: halfway along the principal axis between
/// its extremes (not at the mean, which a one-texel line barely moves), then
/// two rounds of 2-means in RGB.
fn two_colours(px: &Block) -> [[f32; 3]; 2] {
    let axis = principal_axis(px);
    let p: [[f32; 3]; 16] = std::array::from_fn(|i| {
        let [r, g, b, _] = px[i].map(f32::from);
        [r, g, b]
    });
    let proj: [f32; 16] =
        std::array::from_fn(|i| p[i][0] * axis[0] + p[i][1] * axis[1] + p[i][2] * axis[2]);
    let (lo, hi) = proj
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let mid = (lo + hi) / 2.0;
    let mut side: [bool; 16] = std::array::from_fn(|i| proj[i] > mid);
    let mut centres = [[0f32; 3]; 2];
    for round in 0..3 {
        let mut sum = [[0f32; 3]; 2];
        let mut n = [0f32; 2];
        for (i, c) in p.iter().enumerate() {
            let s = usize::from(side[i]);
            n[s] += 1.0;
            for k in 0..3 {
                sum[s][k] += c[k];
            }
        }
        for s in 0..2 {
            if n[s] > 0.0 {
                centres[s] = sum[s].map(|v| v / n[s]);
            } else {
                // An empty side keeps the other's centre; T and H then
                // degrade to one colour, which the caller's comparison rejects.
                centres[s] = sum[1 - s].map(|v| v / n[1 - s]);
            }
        }
        if round == 2 {
            break;
        }
        let d2 = |a: [f32; 3], b: [f32; 3]| (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f32>();
        side = std::array::from_fn(|i| d2(p[i], centres[1]) < d2(p[i], centres[0]));
    }
    centres
}

/// The distance indices worth measuring for paint colours `base ± d`: the
/// one nearest the mean grey offset of the texels nearer `base` than
/// `other`, and its neighbours. The full table is eight; the error is close
/// to unimodal in the distance, so three are nearly always enough.
fn th_distances(px: &Block, base: [i32; 3], other: [i32; 3]) -> std::ops::RangeInclusive<usize> {
    let (mut sum, mut n) = (0i32, 0i32);
    for p in px {
        let p = rgb_i32(*p);
        if dist3(p, base) <= dist3(p, other) {
            sum += ((p[0] - base[0]) + (p[1] - base[1]) + (p[2] - base[2])).abs();
            n += 1;
        }
    }
    let mean = if n > 0 { sum / (3 * n) } else { 0 };
    let nearest = (0..8)
        .min_by_key(|&d| (ETC_TH_DIST[d] - mean).abs())
        .unwrap_or(0);
    nearest.saturating_sub(1)..=(nearest + 1).min(7)
}

fn quant4(c: [f32; 3]) -> [i32; 3] {
    c.map(|v| (v * 15.0 / 255.0).round().clamp(0.0, 15.0) as i32)
}

/// Each texel's nearest of four paint colours, as row-major selectors, and the
/// total squared error. Laid out like `etc_fit_sub` — channels in arrays,
/// error and selector in one `err << 2 | selector` key — so each paint colour
/// is one pass of lane-wise arithmetic and a min.
fn assign_paint(px: &Block, paint: &[[i32; 3]; 4]) -> (u32, [u8; 16]) {
    let ch: [[i32; 16]; 3] = std::array::from_fn(|k| std::array::from_fn(|i| i32::from(px[i][k])));
    let mut key = [i32::MAX; 16];
    for (s, c) in paint.iter().enumerate() {
        for i in 0..16 {
            let (dr, dg, db) = (ch[0][i] - c[0], ch[1][i] - c[1], ch[2][i] - c[2]);
            key[i] = key[i].min((dr * dr + dg * dg + db * db) << 2 | s as i32);
        }
    }
    let err = key.iter().map(|k| (k >> 2) as u32).sum();
    (err, key.map(|k| (k & 3) as u8))
}

/// The pixel-index half shared by every ETC colour mode: selector LSB at bit
/// `j`, MSB at `j + 16`, `j = x * 4 + y`.
fn pack_selectors(sel: &[u8; 16]) -> u64 {
    let mut bits = 0u64;
    for (i, &s) in sel.iter().enumerate() {
        let j = (i % 4) * 4 + i / 4;
        bits |= u64::from(s & 1) << j | u64::from(s >> 1 & 1) << (j + 16);
    }
    bits
}

/// Make a differential-looking 5-bit base plus 3-bit delta overflow, using
/// the field's three free top bits and the delta's free top bit. `u` is the
/// base's two low (colour) bits, `v` the delta's two low bits: clearing the
/// top bits and setting the delta's sign gives `u + v − 4`, below 0 when
/// `u + v < 4`; setting the three top bits gives `28 + u + v`, above 31
/// otherwise.
fn force_overflow(w: u64, base_lo: u32, delta_lo: u32) -> u64 {
    let (u, v) = (w >> base_lo & 3, w >> delta_lo & 3);
    if u + v < 4 {
        w | 1 << (delta_lo + 2)
    } else {
        w | 7 << (base_lo + 2)
    }
}

/// ETC2's T mode: one paint colour is base 1, the other three are base 2 and
/// base 2 ± a distance — a line or a dot (base 1) across a field whose own
/// variation the ± distance follows. Both clusters are tried as base 1.
fn etc_t_mode(px: &Block, colours: &[[f32; 3]; 2]) -> (u32, u64) {
    let mut best = (u32::MAX, 0u64);
    for single in 0..2 {
        let (a, b) = (quant4(colours[single]), quant4(colours[1 - single]));
        let (ea, eb) = (a.map(expand4), b.map(expand4));
        for d in th_distances(px, eb, ea) {
            let dist = ETC_TH_DIST[d];
            let paint = [
                ea,
                eb.map(|v| (v + dist).clamp(0, 255)),
                eb,
                eb.map(|v| (v - dist).clamp(0, 255)),
            ];
            let (err, sel) = assign_paint(px, &paint);
            if err < best.0 {
                best = (err, pack_t(a, b, d as u64, &sel));
            }
        }
    }
    best
}

/// T mode layout (Khronos Data Format spec, ETC2 "T mode"): R1 at 60..59 and
/// 57..56, G1 55..52, B1 51..48, R2 47..44, G2 43..40, B2 39..36, distance
/// index 35..34 and 32, the differential bit (33) set, selectors below. A
/// decoder takes it as T because the red field (63..59 plus delta 58..56)
/// overflows, which the free bits 63..61 and 58 are set to guarantee.
fn pack_t(a: [i32; 3], b: [i32; 3], d: u64, sel: &[u8; 16]) -> u64 {
    let [a, b] = [a, b].map(|c| c.map(|v| v as u64 & 15));
    let mut w = 1u64 << 33;
    w |= (a[0] >> 2) << 59 | (a[0] & 3) << 56;
    w |= a[1] << 52 | a[2] << 48;
    w |= b[0] << 44 | b[1] << 40 | b[2] << 36;
    w |= (d >> 1 & 3) << 34 | (d & 1) << 32;
    w |= pack_selectors(sel);
    force_overflow(w, 59, 56)
}

/// ETC2's H mode: four paint colours, base 1 ± a distance and base 2 ± the
/// same distance — two colours each with a little variation, e.g. a road and
/// the landuse beside it. The distance index's lowest bit is not stored: it is
/// 1 exactly when base 1 ≥ base 2 as 12-bit `RGB444` numbers, so a distance
/// whose parity disagrees with the bases' order is reached by swapping them,
/// and with equal bases only odd distances exist.
fn etc_h_mode(px: &Block, colours: &[[f32; 3]; 2]) -> (u32, u64) {
    let pack12 = |c: [i32; 3]| c[0] << 8 | c[1] << 4 | c[2];
    let mut best = (u32::MAX, 0u64);
    let (qa, qb) = (quant4(colours[0]), quant4(colours[1]));
    let (lo, hi) = {
        let (ra, rb) = (
            th_distances(px, qa.map(expand4), qb.map(expand4)),
            th_distances(px, qb.map(expand4), qa.map(expand4)),
        );
        (*ra.start().min(rb.start()), *ra.end().max(rb.end()))
    };
    for d in lo..=hi {
        let dist = ETC_TH_DIST[d];
        let (mut a, mut b) = (qa, qb);
        let odd = d & 1 == 1;
        if pack12(a) == pack12(b) && !odd {
            continue;
        }
        if (pack12(a) >= pack12(b)) != odd {
            std::mem::swap(&mut a, &mut b);
        }
        let (ea, eb) = (a.map(expand4), b.map(expand4));
        let paint = [
            ea.map(|v| (v + dist).clamp(0, 255)),
            ea.map(|v| (v - dist).clamp(0, 255)),
            eb.map(|v| (v + dist).clamp(0, 255)),
            eb.map(|v| (v - dist).clamp(0, 255)),
        ];
        let (err, sel) = assign_paint(px, &paint);
        if err < best.0 {
            best = (err, pack_h(a, b, d as u64, &sel));
        }
    }
    best
}

/// H mode layout (Khronos Data Format spec, ETC2 "H mode"): R1 62..59, G1 at
/// 58..56 and 52, B1 at 51 and 49..47, R2 46..43, G2 42..39, B2 38..35, the
/// distance index's two high bits at 34 and 32, the differential bit (33)
/// set, selectors below. A decoder takes it as H because the red field (63..59
/// plus delta 58..56) does *not* overflow and the green one (55..51 plus
/// 50..48) does: bit 63 is set if red would underflow without it, and the free
/// bits 55..53 and 50 force green over.
fn pack_h(a: [i32; 3], b: [i32; 3], d: u64, sel: &[u8; 16]) -> u64 {
    let [a, b] = [a, b].map(|c| c.map(|v| v as u64 & 15));
    let mut w = 1u64 << 33;
    w |= a[0] << 59;
    w |= (a[1] >> 1) << 56 | (a[1] & 1) << 52;
    w |= (a[2] >> 3) << 51 | (a[2] & 7) << 47;
    w |= b[0] << 43 | b[1] << 39 | b[2] << 35;
    w |= (d >> 2 & 1) << 34 | (d >> 1 & 1) << 32;
    w |= pack_selectors(sel);
    let red = (w >> 59 & 31) as i32 + {
        let delta = (w >> 56 & 7) as i32;
        if delta >= 4 {
            delta - 8
        } else {
            delta
        }
    };
    if red < 0 {
        w |= 1 << 63;
    }
    force_overflow(w, 51, 48)
}

/// For every 8-bit value, every precision (0 = 5-bit differential, 1 = 4-bit
/// individual) and every one of the 32 modifiers (`table · 4 + selector`): the
/// base code whose decoded value lands nearest the value, and that squared
/// error. A flat block's channels are independent once the modifier is fixed,
/// so its best encoding is a sum of three lookups per modifier.
fn etc_flat_table() -> &'static [[[(u8, u16); 32]; 256]; 2] {
    static TABLE: OnceLock<Box<[[[(u8, u16); 32]; 256]; 2]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = Box::new([[[(0u8, 0u16); 32]; 256]; 2]);
        for (prec, (max, expand)) in [
            (31, expand5 as fn(i32) -> i32),
            (15, expand4 as fn(i32) -> i32),
        ]
        .into_iter()
        .enumerate()
        {
            for v in 0..256i32 {
                for (t, &[a, b]) in ETC1_MODS.iter().enumerate() {
                    for (s, m) in [a, b, -a, -b].into_iter().enumerate() {
                        let mut best = (0u8, u16::MAX);
                        for c in 0..=max {
                            let d = (expand(c) + m).clamp(0, 255) - v;
                            if ((d * d) as u16) < best.1 {
                                best = (c as u8, (d * d) as u16);
                            }
                        }
                        table[prec][v as usize][t * 4 + s] = best;
                    }
                }
            }
        }
        table
    })
}

/// A flat colour: the modifier and base, at either precision, whose decoded
/// value is nearest it. The shared modifier is one offset on all three
/// channels, so the base's per-channel rounding error is what is left over:
/// ETC1 cannot hold an arbitrary flat colour exactly.
fn etc_flat(rgb: [u8; 3]) -> [u8; 8] {
    let table = etc_flat_table();
    // (err, diff, codes, table, selector)
    let mut best = (u32::MAX, true, [0i32; 3], 0u8, 0u8);
    for (prec, per_value) in table.iter().enumerate() {
        let [r, g, b] = rgb.map(|v| &per_value[v as usize]);
        for i in 0..32 {
            let err = u32::from(r[i].1) + u32::from(g[i].1) + u32::from(b[i].1);
            if err < best.0 {
                let codes = [r[i].0, g[i].0, b[i].0].map(i32::from);
                best = (err, prec == 0, codes, (i / 4) as u8, (i % 4) as u8);
            }
        }
    }
    let (_, diff, codes, table, sel) = best;
    pack_etc_colour(
        diff,
        false,
        [codes, codes],
        [table, table],
        [[sel; 8], [sel; 8]],
        &ETC_SUB[0],
    )
    .to_be_bytes()
}

/// Pack an individual (`diff == false`, 4-bit codes) or differential
/// (`diff == true`, 5-bit codes, second within −4..=3 of the first) block.
/// The 64-bit word is stored big-endian. Pixel indices are column-major
/// (`j = x * 4 + y`): the index's LSB at bit `j`, its MSB at bit `j + 16`.
fn pack_etc_colour(
    diff: bool,
    flip: bool,
    codes: [[i32; 3]; 2],
    tables: [u8; 2],
    sel: [[u8; 8]; 2],
    members: &[[usize; 8]; 2],
) -> u64 {
    let mut bits = 0u64;
    for k in 0..3 {
        let shift = 8 * k as u32;
        if diff {
            let delta = codes[1][k] - codes[0][k];
            debug_assert!((-4..=3).contains(&delta));
            bits |= (codes[0][k] as u64 & 31) << (59 - shift);
            bits |= (delta as u64 & 7) << (56 - shift);
        } else {
            bits |= (codes[0][k] as u64 & 15) << (60 - shift);
            bits |= (codes[1][k] as u64 & 15) << (56 - shift);
        }
    }
    bits |= u64::from(tables[0] & 7) << 37;
    bits |= u64::from(tables[1] & 7) << 34;
    bits |= u64::from(diff) << 33;
    bits |= u64::from(flip) << 32;
    for (sub, sels) in members.iter().zip(sel) {
        for (&i, s) in sub.iter().zip(sels) {
            let j = (i % 4) * 4 + i / 4;
            bits |= u64::from(s & 1) << j;
            bits |= u64::from(s >> 1 & 1) << (j + 16);
        }
    }
    bits
}

/// An EAC candidate and its squared error.
struct EacFit {
    err: u32,
    base: u8,
    mult: u8,
    table: u8,
    sel: [u8; 16],
}

/// Evaluate one base / multiplier / table, giving up once the error reaches
/// `bound`.
fn eac_eval(a: &[u8; 16], base: i32, mult: i32, table: usize, bound: u32) -> Option<EacFit> {
    let pal = EAC_MODS[table].map(|m| (base + m * mult).clamp(0, 255));
    let mut sel = [0u8; 16];
    let mut err = 0u32;
    for (o, &v) in sel.iter_mut().zip(a) {
        let v = i32::from(v);
        let (mut bd, mut bs) = (i32::MAX, 0);
        for (s, &p) in pal.iter().enumerate() {
            let d = (p - v) * (p - v);
            if d < bd {
                bd = d;
                bs = s;
            }
        }
        *o = bs as u8;
        err += bd as u32;
        if err >= bound {
            return None;
        }
    }
    Some(EacFit {
        err,
        base: base as u8,
        mult: mult as u8,
        table: table as u8,
        sel,
    })
}

fn encode_eac(a: &[u8; 16]) -> [u8; 8] {
    let (mut lo, mut hi) = (255i32, 0i32);
    for &v in a {
        lo = lo.min(i32::from(v));
        hi = hi.max(i32::from(v));
    }
    if lo == hi {
        // Table 13 holds a 0 modifier (index 4): exact, with a legal multiplier.
        return pack_eac(lo as u8, 1, 13, &[4; 16]);
    }
    let mut best: Option<EacFit> = None;
    for (t, m) in EAC_MODS.iter().enumerate() {
        // Stretch the table's range over the block's, centred on it.
        let (mlo, mhi) = (m[3], m[7]);
        let mult = ((hi - lo) as f32 / (mhi - mlo) as f32)
            .round()
            .clamp(1.0, 15.0) as i32;
        let base = ((hi + lo) as f32 / 2.0 - (mhi + mlo) as f32 * mult as f32 / 2.0)
            .round()
            .clamp(0.0, 255.0) as i32;
        let bound = best.as_ref().map_or(u32::MAX, |b| b.err);
        if let Some(f) = eac_eval(a, base, mult, t, bound) {
            best = Some(f);
        }
    }
    // The first table always completes: its bound is u32::MAX.
    let Some(mut best) = best else {
        return pack_eac(lo as u8, 1, 13, &[4; 16]);
    };
    if best.err > 0 {
        let (b0, m0, t) = (
            i32::from(best.base),
            i32::from(best.mult),
            best.table as usize,
        );
        for dm in -1..=1 {
            for db in -1..=1 {
                let (mult, base) = (m0 + dm, b0 + db);
                if (dm, db) == (0, 0) || !(1..=15).contains(&mult) || !(0..=255).contains(&base) {
                    continue;
                }
                if let Some(f) = eac_eval(a, base, mult, t, best.err) {
                    best = f;
                }
            }
        }
    }
    pack_eac(best.base, best.mult, best.table, &best.sel)
}

/// Base codeword in bits 63..56, multiplier 55..52, table 51..48, then sixteen
/// 3-bit indices MSB-first in column-major order (`j = x * 4 + y`, pixel `j`
/// at bits `47 − 3j ..= 45 − 3j`); the word is stored big-endian.
fn pack_eac(base: u8, mult: u8, table: u8, sel: &[u8; 16]) -> [u8; 8] {
    let mut bits = u64::from(base) << 56 | u64::from(mult & 15) << 52 | u64::from(table & 15) << 48;
    for (i, &s) in sel.iter().enumerate() {
        let j = (i % 4) * 4 + i / 4;
        bits |= u64::from(s & 7) << (45 - 3 * j);
    }
    bits.to_be_bytes()
}

#[cfg(test)]
mod tests {
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
                            BlockFormat::Etc2Rgba8 => {
                                (decode_eac(&b[..8]), decode_etc2_rgb(&b[8..]))
                            }
                        };
                        for y in 0..4 {
                            for x in 0..4 {
                                let (px, py) = (bx * 4 + x, by * 4 + y);
                                if px < w && py < h {
                                    let o = (py * w + px) * 4;
                                    let i = y * 4 + x;
                                    img[o..o + 4]
                                        .copy_from_slice(&[c[i][0], c[i][1], c[i][2], a[i]]);
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
        eprintln!("lines over gradient: ETC1+planar {before:.2} dB, with T/H {after:.2} dB, BC3 {bc3:.2} dB");
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
}
