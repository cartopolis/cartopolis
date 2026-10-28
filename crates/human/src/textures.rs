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

/// The inner extension every pack texture carries (`skin.avatar.jpg`). The
/// client claims `avatar.jpg` and `avatar.png` with a loader of its own, which
/// mipmaps and block-compresses them for its GPU (`human_avatar`); a plain
/// `.jpg` would go to Bevy's image loader and up as uncompressed RGBA.
pub const TEXTURE_EXTENSION: &str = "avatar";

/// Write `img` beside the pack: JPEG when it is opaque (a skin is ~250 KB that
/// way and a megabyte as PNG), PNG when its alpha is a cut-out. Returns the
/// file name.
pub fn write(img: &DynamicImage, out_dir: &Path, stem: &str, alpha: bool) -> Result<String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let name = format!(
        "{stem}.{TEXTURE_EXTENSION}.{}",
        if alpha { "png" } else { "jpg" }
    );
    let path = out_dir.join(&name);
    // A neutral texture is grey, and three equal channels are two more than
    // it needs: one channel (and its alpha) is what goes in the file. The
    // client widens it back to RGBA on decode.
    let grey = img
        .to_rgba8()
        .pixels()
        .all(|p| p[0] == p[1] && p[1] == p[2]);
    let written = if alpha {
        if grey {
            img.to_luma_alpha8().save(&path)
        } else {
            img.to_rgba8().save(&path)
        }
        .map_err(|e| e.to_string())
    } else {
        let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        let pixels = if grey {
            DynamicImage::ImageLuma8(img.to_luma8())
        } else {
            DynamicImage::ImageRgb8(img.to_rgb8())
        };
        image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 88)
            .encode_image(&pixels)
            .map_err(|e| e.to_string())
    };
    written.map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(name)
}

/// The inner extension of a normal map (`suit.nrm.avatar.jpg`): its texels are
/// directions, not colours, so the client loads it linear rather than sRGB,
/// and the full extension is what tells it so.
pub const NORMAL_EXTENSION: &str = "nrm.avatar";

/// Normal maps are 512 on a side: the folds a garment's carries are broad, and
/// at 1024 a skin's would double the GPU memory of a body for detail a follow
/// camera never resolves.
pub const NORMAL_SIDE: u32 = 512;

fn write_normal(img: &DynamicImage, out_dir: &Path, stem: &str) -> Result<String> {
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let name = format!("{stem}.{NORMAL_EXTENSION}.jpg");
    let path = out_dir.join(&name);
    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 92)
        .encode_image(&DynamicImage::ImageRgb8(img.to_rgb8()))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(name)
}

/// A normal map MakeHuman ships, no larger than [`NORMAL_SIDE`].
pub fn normal_map(src: &Path, out_dir: &Path, stem: &str) -> Result<String> {
    write_normal(&shrink(open(src)?, NORMAL_SIDE), out_dir, stem)
}

/// A tangent-space normal map baked from a painted texture's fine detail, for
/// a skin, which MakeHuman ships without one. The painting's luminance with
/// its broad shading taken out (a blur subtracted) is read as height — a pore
/// or a crease is painted darker than the skin round it — and its slope as the
/// normal. `strength` is how steep a full-range step reads.
pub fn normal_from_detail(src: &Path, out_dir: &Path, stem: &str, strength: f32) -> Result<String> {
    let img = shrink(open(src)?, NORMAL_SIDE).to_rgba8();
    let (w, h) = img.dimensions();
    let lum: Vec<f32> = img.pixels().map(luma).collect();
    let blurred = box_blur(&lum, w as usize, h as usize, 6);
    let height: Vec<f32> = lum.iter().zip(&blurred).map(|(l, b)| l - b).collect();
    let at = |x: i64, y: i64| {
        let x = x.clamp(0, i64::from(w) - 1) as usize;
        let y = y.clamp(0, i64::from(h) - 1) as usize;
        height[y * w as usize + x]
    };
    let out: RgbaImage = ImageBuffer::from_fn(w, h, |x, y| {
        let (x, y) = (i64::from(x), i64::from(y));
        let dx = (at(x + 1, y) - at(x - 1, y)) * strength;
        // Image rows run down and UV v runs up: a rise down the image is a
        // fall along v.
        let dy = (at(x, y - 1) - at(x, y + 1)) * strength;
        let n = glam::Vec3::new(-dx, -dy, 1.0).normalize();
        let c = |v: f32| ((v * 0.5 + 0.5) * 255.0).round() as u8;
        Rgba([c(n.x), c(n.y), c(n.z), 255])
    });
    write_normal(&DynamicImage::ImageRgba8(out), out_dir, stem)
}

/// A box blur of radius `r`, rows then columns, clamped at the edges.
fn box_blur(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let pass = |src: &[f32], len: usize, lines: usize, at: &dyn Fn(usize, usize) -> usize| {
        let mut out = vec![0.0; src.len()];
        for line in 0..lines {
            for i in 0..len {
                let (lo, hi) = (i.saturating_sub(r), (i + r).min(len - 1));
                let sum: f32 = (lo..=hi).map(|k| src[at(line, k)]).sum();
                out[at(line, i)] = sum / (hi - lo + 1) as f32;
            }
        }
        out
    };
    let rows = pass(v, w, h, &|y, x| y * w + x);
    pass(&rows, h, w, &|x, y| y * w + x)
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
