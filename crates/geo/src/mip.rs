//! Mipmap chain construction for tile imagery.
//!
//! Pure: packs successive halvings of an RGBA image into one buffer. Turning
//! that buffer into a GPU texture is Bevy's job and lives in the client's
//! `systems::tile_loader`.
//!
//! The halvings happen in **linear light**. Tile imagery is sRGB-encoded, and
//! averaging the encoded bytes systematically darkens every level that holds
//! contrast — the encoded midpoint of black and white is 128 where the correct
//! answer is 188 — which on a map full of dark roads on pale paper read as
//! "the distant tiles are muddier than the near ones". RGB is decoded through
//! the sRGB curve into 16-bit linear, filtered there level to level (so the
//! error of re-encoding never compounds down the chain), and re-encoded per
//! level for the `Rgba8UnormSrgb` texture. **Alpha stays linear**: it is
//! coverage, not colour — the globe's ocean mask rides in it.

use std::sync::OnceLock;

/// sRGB byte → 16-bit linear.
fn srgb_to_linear_lut() -> &'static [u16; 256] {
    static LUT: OnceLock<[u16; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        std::array::from_fn(|i| {
            let s = i as f32 / 255.0;
            let l = if s <= 0.040_45 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            };
            (l * 65535.0).round() as u16
        })
    })
}

/// Nearest sRGB byte for a 16-bit linear value — the exact inverse of the
/// decode LUT under round-to-nearest, found by binary search over its
/// monotonic entries rather than by three `powf`s per texel.
fn linear_to_srgb(l: u16) -> u8 {
    let lut = srgb_to_linear_lut();
    let i = lut.partition_point(|&v| v < l);
    if i == 0 {
        return 0;
    }
    if i >= lut.len() {
        return 255;
    }
    if l - lut[i - 1] <= lut[i] - l {
        (i - 1) as u8
    } else {
        i as u8
    }
}

/// Build a full RGBA8 mipmap chain from a mip-0 level, returning the packed data
/// (all levels, smallest-last) and the level count. Pure CPU work — safe to run on
/// a background thread. Mips let the GPU sample a size-appropriate level, so
/// minified textures filter smoothly instead of aliasing/shimmering, and distant
/// draws read far less texture bandwidth.
pub fn build_mip_chain(width: u32, height: u32, mip0: Vec<u8>) -> (Vec<u8>, u32) {
    let mut data = Vec::with_capacity(mip0.len() * 2);
    data.extend_from_slice(&mip0);
    if mip0.len() != width as usize * height as usize * 4 {
        return (data, 1);
    }

    // Decode once; the whole chain is filtered in this 16-bit linear buffer.
    let lut = srgb_to_linear_lut();
    let mut lin: Vec<u16> = mip0
        .chunks_exact(4)
        .flat_map(|px| {
            [
                lut[px[0] as usize],
                lut[px[1] as usize],
                lut[px[2] as usize],
                u16::from(px[3]) << 8 | u16::from(px[3]),
            ]
        })
        .collect();
    let (mut w, mut h) = (width as usize, height as usize);
    let mut count = 1u32;
    while w > 1 || h > 1 {
        let (next, nw, nh) = halve_linear(&lin, w, h);
        data.extend(next.chunks_exact(4).flat_map(|px| {
            [
                linear_to_srgb(px[0]),
                linear_to_srgb(px[1]),
                linear_to_srgb(px[2]),
                (px[3] >> 8) as u8,
            ]
        }));
        (lin, w, h) = (next, nw, nh);
        count += 1;
    }
    (data, count)
}

/// Halve one linear-light level. Even dimensions ≥ 4 take the fixed 4-tap tent
/// (`[1, 3, 3, 1] / 8` per axis — what a `Triangle` filter degenerates to at
/// exactly 0.5 scale, as shifts and adds instead of a normalised float kernel
/// recomputed per output column); the odd/tiny tail of a chain falls back to
/// the general resampler over the same 16-bit linear texels, so both paths
/// filter in the same space.
///
/// Edges follow `image`: a tap that falls outside the image is *dropped and
/// the remaining weights renormalised* (not clamped to the edge texel), which
/// is why the first and last output along each axis divide by 7 instead of 8.
/// Carrying the two denominators separately keeps the whole thing in integers —
/// the horizontal pass stores unnormalised sums, and the vertical pass divides
/// once by the product.
fn halve_linear(src: &[u16], w: usize, h: usize) -> (Vec<u16>, usize, usize) {
    let (dw, dh) = ((w / 2).max(1), (h / 2).max(1));
    if w < 4 || h < 4 || w % 2 != 0 || h % 2 != 0 {
        let img = image::ImageBuffer::<image::Rgba<u16>, Vec<u16>>::from_raw(
            w as u32,
            h as u32,
            src.to_vec(),
        )
        .expect("level buffer matches its dimensions");
        let out = image::imageops::resize(
            &img,
            dw as u32,
            dh as u32,
            image::imageops::FilterType::Triangle,
        );
        return (out.into_raw(), dw, dh);
    }

    // Horizontal pass: 4 taps into an unnormalised u32 sum (max 65535 · 8).
    // First/last column drop their outside tap, so they carry denominator 7.
    let mut tmp = vec![0u32; dw * h * 4];
    for y in 0..h {
        let row = &src[y * w * 4..(y + 1) * w * 4];
        let out = &mut tmp[y * dw * 4..(y + 1) * dw * 4];
        for k in 0..4 {
            out[k] = 3 * u32::from(row[k]) + 3 * u32::from(row[4 + k]) + u32::from(row[8 + k]);
        }
        for x in 1..dw - 1 {
            let c = (x * 2) * 4;
            for k in 0..4 {
                out[x * 4 + k] = u32::from(row[c - 4 + k])
                    + 3 * u32::from(row[c + k])
                    + 3 * u32::from(row[c + 4 + k])
                    + u32::from(row[c + 8 + k]);
            }
        }
        let c = (dw - 1) * 2 * 4;
        for k in 0..4 {
            out[(dw - 1) * 4 + k] = u32::from(row[c - 4 + k])
                + 3 * u32::from(row[c + k])
                + 3 * u32::from(row[c + 4 + k]);
        }
    }

    // Vertical pass: same tent down the columns, then one rounded divide by the
    // product of the two axes' denominators. A zero-weight tap stands in for the
    // dropped one on the first and last row so every row runs the same shape.
    let stride = dw * 4;
    let mut dst = vec![0u16; stride * dh];
    for y in 0..dh {
        let c = y * 2;
        let (taps, vden): ([(usize, u32); 4], u32) = if y == 0 {
            ([(0, 3), (1, 3), (2, 1), (0, 0)], 7)
        } else if y == dh - 1 {
            ([(c - 1, 1), (c, 3), (c + 1, 3), (0, 0)], 7)
        } else {
            ([(c - 1, 1), (c, 3), (c + 1, 3), (c + 2, 1)], 8)
        };
        let out = &mut dst[y * stride..(y + 1) * stride];
        let tent = |i: usize| -> u32 {
            taps.iter()
                .map(|&(sy, wgt)| wgt * tmp[sy * stride + i])
                .sum()
        };
        // Interior: denominator 64, so the normalisation is a shift and the loop
        // stays free of integer division. This is all but two pixels of the row.
        if vden == 8 {
            for (i, o) in out.iter_mut().enumerate().take(stride - 4).skip(4) {
                *o = ((tent(i) + 32) >> 6) as u16;
            }
        } else {
            for (i, o) in out.iter_mut().enumerate().take(stride - 4).skip(4) {
                *o = ((2 * tent(i) + 56) / 112) as u16;
            }
        }
        // First and last pixel of the row: their horizontal tap was dropped too,
        // so they normalise by 7·vden. Round half away from zero.
        let den = vden * 7;
        for i in (0..4).chain(stride - 4..stride) {
            out[i] = ((2 * u64::from(tent(i)) + u64::from(den)) / (2 * u64::from(den))) as u16;
        }
    }

    (dst, dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 50/50 black-and-white checkerboard is the discriminating input: its
    /// average in encoded bytes is 128, its average in linear light re-encodes
    /// to 188 — a 60-count gap no rounding can blur.
    fn checkerboard(w: u32, h: u32) -> Vec<u8> {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let on = (x + y) % 2 == 0;
                let v = if on { 255 } else { 0 };
                // Alpha alternates too, to pin that *it* averages linearly —
                // alpha is coverage (the ocean mask), not colour.
                px.extend_from_slice(&[v, v, v, v]);
            }
        }
        px
    }

    #[test]
    fn mip_averaging_happens_in_linear_light() {
        let (data, count) = build_mip_chain(64, 64, checkerboard(64, 64));
        assert_eq!(count, 7);
        let one = &data[data.len() - 4..];
        assert!(
            (i32::from(one[0]) - 188).abs() <= 10,
            "1x1 level is {} — 188 is the linear-light mean of a checkerboard, \
             128 is the gamma-naive one",
            one[0]
        );
        assert!(
            (i32::from(one[3]) - 128).abs() <= 10,
            "alpha is coverage and must average linearly, got {}",
            one[3]
        );
    }

    /// A flat image must pass through every level unchanged — the round trip
    /// through the linear decode and back must be exact for a constant, or
    /// every uniform surface would drift a count per level.
    #[test]
    fn a_flat_colour_survives_the_chain_exactly() {
        for v in [0u8, 1, 127, 128, 200, 254, 255] {
            let mip0 = vec![v; 16 * 16 * 4];
            let (data, count) = build_mip_chain(16, 16, mip0);
            assert_eq!(count, 5);
            assert!(
                data.iter().all(|b| *b == v),
                "flat {v} drifted somewhere in the chain"
            );
        }
    }

    /// The chain still has to cover every level down to 1x1, whatever the path —
    /// including the odd-size fallback in the tail.
    #[test]
    fn mip_chain_is_complete_and_correctly_sized() {
        let (data, count) = build_mip_chain(64, 16, checkerboard(64, 16));
        // 64x16, 32x8, 16x4, 8x2, 4x1, 2x1, 1x1
        assert_eq!(count, 7);
        let expected: usize = [(64, 16), (32, 8), (16, 4), (8, 2), (4, 1), (2, 1), (1, 1)]
            .iter()
            .map(|(w, h): &(usize, usize)| w * h * 4)
            .sum();
        assert_eq!(data.len(), expected);
    }

    /// Odd and tiny sizes must still produce a whole chain via the fallback.
    #[test]
    fn odd_sizes_still_chain_to_one_pixel() {
        let (_, count) = build_mip_chain(6, 10, vec![90u8; 6 * 10 * 4]);
        // 6x10, 3x5, 1x2, 1x1
        assert_eq!(count, 4);
    }
}
