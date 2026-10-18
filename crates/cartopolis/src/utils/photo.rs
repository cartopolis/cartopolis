//! Turning whatever a camera or a file picker hands over into the two images a
//! saved place keeps: one full-size photo and one small square thumbnail.
//!
//! Everything here is pure — bytes in, bytes out, no filesystem and no ECS — so
//! it compiles and is tested on every target, including the web build that has
//! nowhere to write the result. The storage half is [`crate::utils::photo_store`]
//! and the capture half is [`crate::platform::photo`].
//!
//! Three decisions are worth stating, because none of them is reversible once a
//! photo has been written:
//!
//! **The source is re-encoded, never stored as it arrived.** A phone camera
//! produces 3–8 MB per frame at a resolution no panel will ever show; a place
//! that costs eight megabytes to remember is a place nobody attaches a second
//! photo to. [`FULL_MAX_EDGE`] is the compromise — large enough to fill a laptop
//! screen in the viewer, roughly a twentieth of the bytes.
//!
//! **EXIF orientation is applied to the pixels, not carried in the file.** Phone
//! cameras almost always write the sensor's own landscape frame plus a rotation
//! tag, so a portrait photo is a landscape image with "turn me" attached. Every
//! later consumer (egui's texture upload, the thumbnail crop) would have to
//! honour it independently, so the rotation is baked in once and the rest of
//! the client treats a stored photo as a plain upright image. **The rest of the
//! EXIF and XMP is kept** (since 2026-10-18, `utils::jpeg_meta`), with its
//! orientation set to upright: a picture keeps where it was taken.
//!
//! **The thumbnail is a square centre-crop, not a scaled-down copy.** The Places
//! row is a list: a thumbnail whose width depends on the photo's aspect ratio
//! makes every name in the list start at a different x, which reads as a broken
//! layout rather than as variety.

use image::{DynamicImage, ImageDecoder as _, ImageReader};
use std::io::Cursor;

/// Longest edge of the stored photo, in pixels.
pub const FULL_MAX_EDGE: u32 = 1600;

/// Edge of the square thumbnail. Drawn at [`THUMB_POINTS`] in the row, so this is
/// comfortably past 2× for a high-density phone screen and stays sharp when the
/// viewer is opened on a hidpi laptop.
pub const THUMB_EDGE: u32 = 128;

/// Side of the thumbnail tile in the Places row, in egui points.
pub const THUMB_POINTS: f32 = 28.0;

/// JPEG quality for the stored photo and its thumbnail. 82 is the knee: the file
/// halves again below it, and so does the first thing anyone notices, which is
/// blocking in the sky of a photo taken outdoors.
const FULL_QUALITY: u8 = 82;
const THUMB_QUALITY: u8 = 78;

/// Cap on decode allocation. A saved place's photo comes from the user's own
/// device, so this is not a hostile-input defence — it is the difference between
/// "that file was too big" in the panel and the process being killed by the phone
/// for taking 2 GB while decoding a panorama someone picked by accident.
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;

/// The two encoded images a place stores.
pub struct Prepared {
    /// Full-size JPEG, at most [`FULL_MAX_EDGE`] on its longest edge.
    pub full: Vec<u8>,
    /// Square JPEG, [`THUMB_EDGE`] on a side.
    pub thumb: Vec<u8>,
}

/// Decode `bytes` (JPEG, PNG, WebP — whatever `image` recognises), straighten it
/// and re-encode both sizes.
///
/// The error is a sentence for the panel to show, not a type to match on: every
/// failure here ends the same way, with the capture discarded and the user told
/// why.
pub fn prepare(bytes: &[u8]) -> Result<Prepared, String> {
    let img = decode_upright(bytes)?;

    let (fw, fh) = fit_within(img.width(), img.height(), FULL_MAX_EDGE);
    // `resize_to_fill` would crop; `resize` alone would letterbox nothing, which is
    // what we want for the full image — the aspect ratio is the photographer's.
    let full_img = if (fw, fh) == (img.width(), img.height()) {
        img.clone()
    } else {
        img.resize(fw, fh, image::imageops::FilterType::CatmullRom)
    };
    // `resize_to_fill` centre-crops to the target aspect and scales in one step,
    // which is exactly the square tile, and it samples from the *original* rather
    // than from the already-downscaled copy.
    let thumb_img = img.resize_to_fill(
        THUMB_EDGE,
        THUMB_EDGE,
        image::imageops::FilterType::CatmullRom,
    );

    // The full image keeps where it was taken: the original's EXIF and XMP go
    // into the copy, marked upright because its pixels now are. The thumbnail
    // is a list's tile and carries none.
    let mut meta = crate::utils::jpeg_meta::read(bytes);
    if let Some(tiff) = meta.exif.as_mut() {
        crate::utils::jpeg_meta::set_upright(tiff);
    }
    Ok(Prepared {
        full: crate::utils::jpeg_meta::write(&encode_jpeg(&full_img, FULL_QUALITY)?, &meta),
        thumb: encode_jpeg(&thumb_img, THUMB_QUALITY)?,
    })
}

/// Edge of a person's picture (`systems::faces`). Larger than [`THUMB_EDGE`]
/// because the You page draws it 90 points across, which is 270 pixels on a
/// phone; a list row's 48 points are the same file scaled down.
pub const FACE_EDGE: u32 = 256;
const FACE_QUALITY: u8 = 82;

/// A person's picture from whatever was picked: a [`FACE_EDGE`] square
/// centre-crop, upright, as a JPEG plus the pixels it decodes to.
pub fn face(bytes: &[u8]) -> Result<(Vec<u8>, (Vec<u8>, [usize; 2])), String> {
    face_of(decode_upright(bytes)?)
}

/// [`face`] from raw RGBA, as a snapshot arrives.
pub fn face_from_rgba(
    rgba: Vec<u8>,
    size: [usize; 2],
) -> Result<(Vec<u8>, (Vec<u8>, [usize; 2])), String> {
    let img = image::RgbaImage::from_raw(size[0] as u32, size[1] as u32, rgba)
        .ok_or_else(|| "the snapshot was the wrong size".to_string())?;
    face_of(DynamicImage::ImageRgba8(img))
}

fn face_of(img: DynamicImage) -> Result<(Vec<u8>, (Vec<u8>, [usize; 2])), String> {
    let square = img.resize_to_fill(
        FACE_EDGE,
        FACE_EDGE,
        image::imageops::FilterType::CatmullRom,
    );
    let jpeg = encode_jpeg(&square, FACE_QUALITY)?;
    let rgba = square.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok((jpeg, (rgba.into_raw(), size)))
}

/// Decode to RGBA8 for egui, which wants straight bytes and a size.
///
/// Stored photos are already upright (see the module note), but this runs the
/// same orientation step anyway: it is a no-op on a file we wrote, and it is the
/// difference between right and sideways if it is ever pointed at a file we did
/// not.
pub fn decode_rgba(bytes: &[u8]) -> Result<(Vec<u8>, [usize; 2]), String> {
    let img = decode_upright(bytes)?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok((rgba.into_raw(), size))
}

/// Decode with the format guessed from the bytes and the EXIF rotation applied.
fn decode_upright(bytes: &[u8]) -> Result<DynamicImage, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("could not read the image: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);

    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("unsupported image format: {e}"))?;
    // An absent or unreadable EXIF block is the common case for a PNG and for a
    // screenshot, and it means "no rotation" rather than a failure.
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img =
        DynamicImage::from_decoder(decoder).map_err(|e| format!("could not decode: {e}"))?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// Encode as JPEG. Alpha is dropped (`to_rgb8`) because JPEG has none — a PNG
/// with transparency composites onto black rather than failing to encode.
fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&img.to_rgb8())
        .map_err(|e| format!("could not encode the photo: {e}"))?;
    Ok(out)
}

/// The largest size with the same aspect ratio that fits in a `max`-edged box —
/// and the original size when it already does, so a small photo is never scaled
/// *up* into a blurrier version of itself.
///
/// Rounds to at least 1 in each axis: the ratio of a 4000×1 panorama would
/// otherwise floor the short edge to zero, and an image with a zero dimension
/// fails to encode.
fn fit_within(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w, h);
    }
    let scale = max as f64 / w.max(h) as f64;
    (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny synthetic photo: a red/blue split so a rotation is visible in the
    /// pixels rather than only in the dimensions.
    fn sample(w: u32, h: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(w, h);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            *px = if x < w / 2 {
                image::Rgb([200, 30, 30])
            } else {
                image::Rgb([30, 30, 200])
            };
        }
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode_image(&img)
            .unwrap();
        out
    }

    #[test]
    fn fit_within_leaves_a_small_image_alone() {
        assert_eq!(fit_within(800, 600, FULL_MAX_EDGE), (800, 600));
        assert_eq!(fit_within(FULL_MAX_EDGE, 10, FULL_MAX_EDGE), (1600, 10));
    }

    #[test]
    fn fit_within_scales_the_long_edge_to_the_cap() {
        // Landscape and portrait both cap their *longest* edge, not their width.
        assert_eq!(fit_within(4000, 3000, 1600), (1600, 1200));
        assert_eq!(fit_within(3000, 4000, 1600), (1200, 1600));
    }

    #[test]
    fn fit_within_never_returns_a_zero_edge() {
        // A 4000×1 slice would floor to a height of 0 and fail to encode.
        let (w, h) = fit_within(4000, 1, 1600);
        assert!(w >= 1 && h >= 1, "got {w}×{h}");
    }

    #[test]
    fn prepare_caps_the_full_image_and_squares_the_thumbnail() {
        let prepared = prepare(&sample(2400, 1200)).expect("prepare");

        let full = image::load_from_memory(&prepared.full).expect("full decodes");
        assert_eq!(full.width().max(full.height()), FULL_MAX_EDGE);
        assert_eq!((full.width(), full.height()), (1600, 800));

        let thumb = image::load_from_memory(&prepared.thumb).expect("thumb decodes");
        assert_eq!((thumb.width(), thumb.height()), (THUMB_EDGE, THUMB_EDGE));
    }

    #[test]
    fn prepare_keeps_where_a_photo_was_taken() {
        let meta = crate::utils::jpeg_meta::Meta {
            exif: Some(b"II*\0\x08\0\0\0\0\0\0\0\0\0".to_vec()),
            xmp: Some(b"<x:xmpmeta/>".to_vec()),
        };
        let original = crate::utils::jpeg_meta::write(&sample(2400, 1200), &meta);
        let prepared = prepare(&original).expect("prepare");
        assert_eq!(crate::utils::jpeg_meta::read(&prepared.full), meta);
        assert!(crate::utils::jpeg_meta::read(&prepared.thumb).is_empty());
    }

    #[test]
    fn prepare_does_not_upscale_a_small_photo() {
        let prepared = prepare(&sample(320, 240)).expect("prepare");
        let full = image::load_from_memory(&prepared.full).expect("full decodes");
        assert_eq!((full.width(), full.height()), (320, 240));
    }

    #[test]
    fn prepare_rejects_bytes_that_are_not_an_image() {
        // The failure path matters: this is what a cancelled capture that returned
        // an empty file, or a `.heic` no decoder here understands, arrives as.
        let err = prepare(b"not an image at all").err().expect("not an image");
        assert!(!err.is_empty());
    }

    #[test]
    fn decode_rgba_returns_four_bytes_per_pixel() {
        let (pixels, [w, h]) = decode_rgba(&sample(64, 48)).expect("decode");
        assert_eq!((w, h), (64, 48));
        assert_eq!(pixels.len(), w * h * 4);
    }
}
