//! Mipmap chain construction for tile imagery.
//!
//! Pure: packs successive halvings of an RGBA image into one buffer. Turning
//! that buffer into a GPU texture is Bevy's job and lives in the client's
//! `systems::tile_loader`.

/// Build a full RGBA8 mipmap chain from a mip-0 level, returning the packed data
/// (all levels, smallest-last) and the level count. Pure CPU work — safe to run on
/// a background thread. Mips let the GPU sample a size-appropriate level, so
/// minified textures filter smoothly instead of aliasing/shimmering, and distant
/// draws read far less texture bandwidth.
pub fn build_mip_chain(width: u32, height: u32, mip0: Vec<u8>) -> (Vec<u8>, u32) {
    let mut data = Vec::with_capacity(mip0.len() * 2);
    data.extend_from_slice(&mip0);
    let Some(mut level) = image::RgbaImage::from_raw(width, height, mip0) else {
        return (data, 1);
    };
    let mut count = 1u32;
    while level.width() > 1 || level.height() > 1 {
        let w = (level.width() / 2).max(1);
        let h = (level.height() / 2).max(1);
        level = match halve_triangle(&level) {
            Some(next) => next,
            // Odd dimension somewhere in the chain: fall back to the general
            // resampler for this level and every level after it. Real textures
            // are power-of-two, so this is the tail of an unusual size, not a
            // hot path.
            None => image::imageops::resize(&level, w, h, image::imageops::FilterType::Triangle),
        };
        data.extend_from_slice(level.as_raw());
        count += 1;
    }
    (data, count)
}

/// Halve an RGBA image with the same filter `image::imageops::resize` applies at
/// exactly 0.5 scale, byte for byte, but without its per-pixel weight machinery.
///
/// Every level of every mip chain goes through here — each streamed tile, each
/// prim texture — and the generic resampler is the wrong tool for the one ratio
/// we ever ask it for. At 0.5 scale a `Triangle` filter degenerates to a fixed
/// 4-tap tent, `[1, 3, 3, 1] / 8` per axis, which is a shift and an add rather
/// than a normalised float kernel recomputed per output column. Fixing the
/// weights also fixes the loop shape, so it autovectorises. Measured ~2x faster
/// than `resize`.
///
/// Output matches the resampler's except where the exact result lands on a `.5`
/// tie: the weights here are integers, so a tie rounds half-up exactly, while
/// `resize` accumulates in `f32` and can land a hair under. Those pixels come out
/// one LSB apart, on the order of four bytes in a megabyte (see
/// `mips_match_the_image_crate`), and it is this version that is exact.
///
/// Returns `None` unless both dimensions are even and at least 4, which is where
/// "halving" is a true 2x reduction *and* the tent's outer taps still land on
/// real texels. The 2x2 and 1xN tail of a chain falls back.
///
/// Edges follow `image`: a tap that falls outside the image is *dropped and the
/// remaining weights renormalised* (not clamped to the edge texel), which is why
/// the first and last output along each axis divide by 7 instead of 8. Carrying
/// the two denominators separately keeps the whole thing in integers — the
/// horizontal pass stores unnormalised sums, and the vertical pass divides once
/// by the product.
fn halve_triangle(src: &image::RgbaImage) -> Option<image::RgbaImage> {
    let (w, h) = (src.width() as usize, src.height() as usize);
    if w < 4 || h < 4 || w % 2 != 0 || h % 2 != 0 {
        return None;
    }
    let (dw, dh) = (w / 2, h / 2);
    let src = src.as_raw();

    // Horizontal pass: 4 taps into an unnormalised u16 sum (max 255*8 = 2040).
    // First/last column drop their outside tap, so they carry denominator 7.
    let mut tmp = vec![0u16; dw * h * 4];
    for y in 0..h {
        let row = &src[y * w * 4..(y + 1) * w * 4];
        let out = &mut tmp[y * dw * 4..(y + 1) * dw * 4];
        for k in 0..4 {
            out[k] = 3 * u16::from(row[k]) + 3 * u16::from(row[4 + k]) + u16::from(row[8 + k]);
        }
        for x in 1..dw - 1 {
            let c = (x * 2) * 4;
            for k in 0..4 {
                out[x * 4 + k] = u16::from(row[c - 4 + k])
                    + 3 * u16::from(row[c + k])
                    + 3 * u16::from(row[c + 4 + k])
                    + u16::from(row[c + 8 + k]);
            }
        }
        let c = (dw - 1) * 2 * 4;
        for k in 0..4 {
            out[(dw - 1) * 4 + k] = u16::from(row[c - 4 + k])
                + 3 * u16::from(row[c + k])
                + 3 * u16::from(row[c + 4 + k]);
        }
    }

    // Vertical pass: same tent down the columns, then one rounded divide by the
    // product of the two axes' denominators. A zero-weight tap stands in for the
    // dropped one on the first and last row so every row runs the same shape.
    let stride = dw * 4;
    let mut dst = vec![0u8; stride * dh];
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
                .map(|&(sy, wgt)| wgt * u32::from(tmp[sy * stride + i]))
                .sum()
        };
        // Interior: denominator 64, so the normalisation is a shift and the loop
        // stays free of integer division. This is all but two pixels of the row.
        if vden == 8 {
            for (i, o) in out.iter_mut().enumerate().take(stride - 4).skip(4) {
                *o = ((tent(i) + 32) >> 6) as u8;
            }
        } else {
            for (i, o) in out.iter_mut().enumerate().take(stride - 4).skip(4) {
                *o = ((2 * tent(i) + 56) / 112) as u8;
            }
        }
        // First and last pixel of the row: their horizontal tap was dropped too,
        // so they normalise by 7·vden. Round half away from zero, matching the
        // resampler's `FloatNearest`.
        let den = vden * 7;
        for i in (0..4).chain(stride - 4..stride) {
            out[i] = ((2 * tent(i) + den) / (2 * den)) as u8;
        }
    }

    image::RgbaImage::from_raw(dw as u32, dh as u32, dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise: a gradient would pass against *any* symmetric
    /// normalised kernel, so it cannot tell a tent from a box. Noise can.
    fn noise(w: u32, h: u32) -> image::RgbaImage {
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        image::RgbaImage::from_fn(w, h, |_, _| {
            let mut b = [0u8; 4];
            for c in &mut b {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                *c = (s >> 33) as u8;
            }
            image::Rgba(b)
        })
    }


/// [`halve_triangle`] exists only because it is faster than the resampler it
    /// replaces — the moment it stops agreeing with it, every mip in the game
    /// silently changes. Agreement means the same tent and the same renormalised
    /// edges, to within the `.5`-tie rounding noted on the function: never more
    /// than one LSB, and never on more than a handful of bytes.
    #[test]
    fn mips_match_the_image_crate() {
        for (w, h) in [(512, 512), (256, 64), (128, 512), (64, 64), (4, 4), (6, 10)] {
            let src = noise(w, h);
            let want =
                image::imageops::resize(&src, w / 2, h / 2, image::imageops::FilterType::Triangle);
            let got = halve_triangle(&src).expect("even dimensions >= 4 take the fast path");
            assert_eq!(got.dimensions(), want.dimensions(), "{w}x{h}");

            let deltas: Vec<i32> = got
                .as_raw()
                .iter()
                .zip(want.as_raw())
                .map(|(a, b)| i32::from(*a) - i32::from(*b))
                .filter(|d| *d != 0)
                .collect();
            let worst = deltas.iter().map(|d| d.abs()).max().unwrap_or(0);
            assert!(worst <= 1, "{w}x{h}: max |delta| {worst} — that is a different filter, not a tie");
            let budget = (got.as_raw().len() / 10_000).max(8);
            assert!(
                deltas.len() <= budget,
                "{w}x{h}: {} of {} bytes differ — too many for tie-breaking alone",
                deltas.len(),
                got.as_raw().len(),
            );
        }
    }

    /// Sizes the tent cannot handle must fall back, not produce wrong pixels.
    #[test]
    fn odd_and_tiny_sizes_fall_back() {
        for (w, h) in [(2, 2), (1, 8), (7, 8), (8, 7), (3, 3)] {
            assert!(halve_triangle(&noise(w, h)).is_none(), "{w}x{h} must fall back");
        }
    }

    /// The chain still has to cover every level down to 1x1, whatever the path.
    #[test]
    fn mip_chain_is_complete_and_correctly_sized() {
        let (data, count) = build_mip_chain(64, 16, noise(64, 16).into_raw());
        // 64x16, 32x8, 16x4, 8x2, 4x1, 2x1, 1x1
        assert_eq!(count, 7);
        let expected: usize = [(64, 16), (32, 8), (16, 4), (8, 2), (4, 1), (2, 1), (1, 1)]
            .iter()
            .map(|(w, h): &(usize, usize)| w * h * 4)
            .sum();
        assert_eq!(data.len(), expected);
    }
}
