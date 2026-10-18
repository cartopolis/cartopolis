//! The pack's textures: resized for a phone, and — for what a look recolours —
//! made *neutral*.
//!
//! A tint multiplies a texture, so over an authored brown it can only make
//! darker browns: no blond hair, no white shirt from a blue one. A neutral
//! texture keeps the authored light (folds, strands, weave) and drops the
//! colour: each pixel's luminance over the texture's bright end, as grey. The
//! garment's own colour comes back as a tint, recorded per region
//! ([`crate::Proxy::region_colours`]), so the default look is the authored one
//! and every other colour, lighter ones included, is a tint away.
//!
//! The multiplication happens in linear light on the GPU and this works in
//! sRGB; the two agree because sRGB's curve is close to a power, and a power of
//! a product is the product of the powers.

use std::path::Path;

use image::{DynamicImage, GenericImageView, ImageBuffer, Rgba, RgbaImage};

pub type Result<T> = std::result::Result<T, String>;

/// Longest side of a texture in the pack, by what it covers. MakeHuman's are
/// 2048; at the size an avatar is seen, 1024 loses nothing visible on a skin
/// or a garment, hair and brows hold up at 512, and an iris is a few pixels
/// across even close up.
pub const MAX_SIDE: u32 = 1024;
pub const HAIR_SIDE: u32 = 512;
pub const EYE_SIDE: u32 = 256;

fn open(src: &Path) -> Result<DynamicImage> {
    image::open(src).map_err(|e| format!("{}: {e}", src.display()))
}

fn shrink(img: DynamicImage, max: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= max {
        return img;
    }
    img.resize(max, max, image::imageops::FilterType::Lanczos3)
}

/// Luminance of an sRGB pixel, 0–1.
fn luma(p: &Rgba<u8>) -> f32 {
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
}

/// Write `img` beside the pack: JPEG when it is opaque (a skin is ~250 KB that
/// way and a megabyte as PNG), PNG when its alpha is a cut-out. Returns the
/// file name.
pub fn write(img: &DynamicImage, out_dir: &Path, stem: &str, alpha: bool) -> Result<String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let name = format!("{stem}.{}", if alpha { "png" } else { "jpg" });
    let path = out_dir.join(&name);
    let written = if alpha {
        img.to_rgba8().save(&path).map_err(|e| e.to_string())
    } else {
        let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 88)
            .encode_image(&DynamicImage::ImageRgb8(img.to_rgb8()))
            .map_err(|e| e.to_string())
    };
    written.map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(name)
}

/// Whether an image really cuts anything out. MakeHuman marks most garments
/// transparent whether or not their alpha holds anything, which made every
/// outfit a megabyte-and-a-half PNG instead of a JPEG a tenth the size.
fn cuts_out(img: &DynamicImage) -> bool {
    let Some(rgba) = img.as_rgba8() else {
        return img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p[3] < 128);
    };
    let clear = rgba.pixels().filter(|p| p[3] < 128).count();
    clear * 200 > rgba.pixels().len()
}

/// A texture as authored, no larger than `max` on a side. Returns its file
/// name and whether it cuts out.
pub fn plain(
    src: &Path,
    out_dir: &Path,
    stem: &str,
    alpha: bool,
    max: u32,
) -> Result<(String, bool)> {
    let img = shrink(open(src)?, max);
    let alpha = alpha && cuts_out(&img);
    Ok((write(&img, out_dir, stem, alpha)?, alpha))
}

/// The neutral version of `src` (no larger than `max` on a side), whether it
/// cuts out, and per region the tint that gives the authored colour back. `regions` are, per region, the UVs (OBJ convention,
/// v up) of the faces' corners in it, where its colour is sampled.
pub fn neutral(
    src: &Path,
    out_dir: &Path,
    stem: &str,
    alpha: bool,
    regions: &[Vec<[f32; 2]>],
    max: u32,
) -> Result<(String, bool, Vec<[u8; 3]>)> {
    let img = shrink(open(src)?, max);
    let alpha = alpha && cuts_out(&img);
    let img = img.to_rgba8();
    let (w, h) = img.dimensions();
    // The bright end: the 95th percentile of the luminance of what is drawn
    // (alpha over half), so a few specular highlights do not set it.
    let mut lumas: Vec<f32> = img
        .pixels()
        .filter(|p| !alpha || p[3] > 127)
        .map(luma)
        .collect();
    if lumas.is_empty() {
        return Err(format!("{}: nothing drawn", src.display()));
    }
    lumas.sort_by(f32::total_cmp);
    let bright = lumas[lumas.len() * 95 / 100].max(1.0e-3);

    let colours = regions
        .iter()
        .map(|uvs| {
            let (mut sum, mut l, mut n) = ([0.0f32; 3], 0.0f32, 0u32);
            for uv in uvs {
                let x = ((uv[0].rem_euclid(1.0)) * (w - 1) as f32).round() as u32;
                let y = (((1.0 - uv[1]).rem_euclid(1.0)) * (h - 1) as f32).round() as u32;
                let p = img.get_pixel(x.min(w - 1), y.min(h - 1));
                if alpha && p[3] <= 127 {
                    continue;
                }
                for k in 0..3 {
                    sum[k] += p[k] as f32;
                }
                l += luma(p);
                n += 1;
            }
            if n == 0 {
                return [255; 3];
            }
            let mean_l = (l / n as f32).max(1.0e-3);
            // The tint that turns this region's average grey back into its
            // average colour.
            let gain = bright / mean_l;
            sum.map(|s| ((s / n as f32) * gain).round().clamp(0.0, 255.0) as u8)
        })
        .collect();

    let out: RgbaImage = ImageBuffer::from_fn(w, h, |x, y| {
        let p = img.get_pixel(x, y);
        let g = ((luma(p) / bright).min(1.0) * 255.0).round() as u8;
        Rgba([g, g, g, p[3]])
    });
    let name = write(&DynamicImage::ImageRgba8(out), out_dir, stem, alpha)?;
    Ok((name, alpha, colours))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat blue garment becomes a flat light grey, and its tint brings the
    /// blue back.
    #[test]
    fn a_neutral_texture_and_its_tint_give_back_the_original() {
        let dir = std::env::temp_dir().join(format!("human-tex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("blue.png");
        RgbaImage::from_pixel(8, 8, Rgba([40, 80, 160, 255]))
            .save(&src)
            .unwrap();
        let (name, _, tints) =
            neutral(&src, &dir, "n", false, &[vec![[0.5, 0.5]]], MAX_SIDE).unwrap();
        let grey = image::open(dir.join(&name)).unwrap().to_rgb8();
        let g = grey.get_pixel(4, 4);
        assert!(g[0] > 240 && g[0] == g[1] && g[1] == g[2], "{g:?}");
        let t = tints[0];
        let back = [
            t[0] as f32 * g[0] as f32 / 255.0,
            t[2] as f32 * g[2] as f32 / 255.0,
        ];
        assert!(
            (back[0] - 40.0).abs() < 4.0 && (back[1] - 160.0).abs() < 4.0,
            "{t:?} {g:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
