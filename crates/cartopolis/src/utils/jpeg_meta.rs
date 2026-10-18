//! A JPEG's own metadata — its EXIF and XMP segments — kept through a
//! re-encode, and written into a picture the app makes.
//!
//! **Why this exists.** Every picture the client handles is decoded and encoded
//! again (`utils::photo::prepare`: smaller, upright), and the `image` crate's
//! encoder writes no segment it was not handed. Until 2026-10-18 that meant a
//! photo lost where it was taken the moment it was sent or posted. The camera
//! (`systems::camera_screen`) now puts its whole pose in every picture
//! (`cartopolis_core::photo_pose`), so the pose has to survive the same trip:
//! the segments are lifted out of the original and put back into the copy.
//!
//! Pure bytes, every target.

/// The APP1 marker both segments use, and what opens each one's payload.
const APP1: u8 = 0xE1;
const EXIF_HEADER: &[u8] = b"Exif\0\0";
const XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// The EXIF (as a TIFF block, without its `Exif\0\0`) and the XMP packet of a
/// JPEG, where it has them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Meta {
    pub exif: Option<Vec<u8>>,
    pub xmp: Option<Vec<u8>>,
}

impl Meta {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exif.is_none() && self.xmp.is_none()
    }
}

/// Lift the EXIF and XMP out of `jpeg`. Anything that is not a JPEG, or whose
/// segments do not parse, has none. Reads only up to the first scan.
#[must_use]
pub fn read(jpeg: &[u8]) -> Meta {
    let mut meta = Meta::default();
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return meta;
    }
    let mut at = 2;
    while at + 4 <= jpeg.len() {
        if jpeg[at] != 0xFF {
            break;
        }
        let marker = jpeg[at + 1];
        // Start of scan, end of image: nothing after it is metadata.
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = usize::from(u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]));
        if len < 2 || at + 2 + len > jpeg.len() {
            break;
        }
        let body = &jpeg[at + 4..at + 2 + len];
        if marker == APP1 {
            if let Some(tiff) = body.strip_prefix(EXIF_HEADER) {
                meta.exif.get_or_insert_with(|| tiff.to_vec());
            } else if let Some(xmp) = body.strip_prefix(XMP_HEADER) {
                meta.xmp.get_or_insert_with(|| xmp.to_vec());
            }
        }
        at += 2 + len;
    }
    meta
}

/// `jpeg` with `meta`'s segments put in right after its start (and after a
/// JFIF header, if it opens with one). A segment too long for a JPEG segment
/// is left out rather than written broken.
#[must_use]
pub fn write(jpeg: &[u8], meta: &Meta) -> Vec<u8> {
    if meta.is_empty() || jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return jpeg.to_vec();
    }
    // After SOI, and after an APP0 (JFIF) if one is first: EXIF readers expect
    // the APP1 early, and JFIF readers expect APP0 first.
    let mut insert_at = 2;
    if jpeg.len() > 6 && jpeg[2] == 0xFF && jpeg[3] == 0xE0 {
        let len = usize::from(u16::from_be_bytes([jpeg[4], jpeg[5]]));
        if 2 + 2 + len <= jpeg.len() {
            insert_at = 2 + 2 + len;
        }
    }
    let mut out = Vec::with_capacity(jpeg.len() + 4096);
    out.extend_from_slice(&jpeg[..insert_at]);
    for (header, payload) in [
        (EXIF_HEADER, meta.exif.as_deref()),
        (XMP_HEADER, meta.xmp.as_deref()),
    ] {
        let Some(payload) = payload else {
            continue;
        };
        let len = 2 + header.len() + payload.len();
        let Ok(len16) = u16::try_from(len) else {
            tracing::warn!(
                bytes = len,
                "a metadata segment too long for a JPEG was left out"
            );
            continue;
        };
        out.extend_from_slice(&[0xFF, APP1]);
        out.extend_from_slice(&len16.to_be_bytes());
        out.extend_from_slice(header);
        out.extend_from_slice(payload);
    }
    out.extend_from_slice(&jpeg[insert_at..]);
    out
}

/// Set a TIFF block's Orientation (tag 0x0112 in its first IFD) to 1, "as
/// stored": for pixels that have already been turned upright, which is what
/// `photo::prepare` does with every picture. A block without the tag is left
/// as it is, which reads as upright anyway.
pub fn set_upright(tiff: &mut [u8]) {
    if tiff.len() < 8 {
        return;
    }
    let little = match &tiff[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return,
    };
    let u16_at = |b: &[u8], o: usize| -> Option<u16> {
        let v = [*b.get(o)?, *b.get(o + 1)?];
        Some(if little {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        })
    };
    let u32_at = |b: &[u8], o: usize| -> Option<u32> {
        let v = [*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?];
        Some(if little {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        })
    };
    let Some(ifd) = u32_at(tiff, 4).map(|v| v as usize) else {
        return;
    };
    let Some(n) = u16_at(tiff, ifd) else {
        return;
    };
    for k in 0..usize::from(n) {
        let entry = ifd + 2 + 12 * k;
        if u16_at(tiff, entry) == Some(0x0112) && u16_at(tiff, entry + 2) == Some(3) {
            let one = if little {
                1u16.to_le_bytes()
            } else {
                1u16.to_be_bytes()
            };
            if let Some(slot) = tiff.get_mut(entry + 8..entry + 10) {
                slot.copy_from_slice(&one);
            }
            return;
        }
    }
}

/// Where a JPEG says it was taken: its EXIF GPS latitude and longitude, as
/// signed degrees. `None` without them, or for anything that does not parse.
#[must_use]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // for the camera, native-only
pub fn gps(jpeg: &[u8]) -> Option<(f64, f64)> {
    let tiff = read(jpeg).exif?;
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let v = [*tiff.get(o)?, *tiff.get(o + 1)?];
        Some(if little {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let v = [
            *tiff.get(o)?,
            *tiff.get(o + 1)?,
            *tiff.get(o + 2)?,
            *tiff.get(o + 3)?,
        ];
        Some(if little {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        })
    };
    // The entry for `tag` in the IFD at `ifd`: (type, count, value offset).
    let entry = |ifd: usize, tag: u16| -> Option<(u16, u32, usize)> {
        let n = usize::from(u16_at(ifd)?);
        (0..n).map(|k| ifd + 2 + 12 * k).find_map(|e| {
            (u16_at(e)? == tag).then_some(())?;
            Some((u16_at(e + 2)?, u32_at(e + 4)?, e + 8))
        })
    };
    let ifd0 = u32_at(4)? as usize;
    let (_, _, at) = entry(ifd0, 0x8825)?;
    let gps = u32_at(at)? as usize;
    let degrees = |tag: u16| -> Option<f64> {
        let (kind, count, at) = entry(gps, tag)?;
        if kind != 5 || count != 3 {
            return None;
        }
        let values = u32_at(at)? as usize;
        let mut out = 0.0;
        for (i, scale) in [1.0, 60.0, 3600.0].into_iter().enumerate() {
            let num = f64::from(u32_at(values + 8 * i)?);
            let den = f64::from(u32_at(values + 8 * i + 4)?);
            if den == 0.0 {
                return None;
            }
            out += num / den / scale;
        }
        Some(out)
    };
    let sign = |tag: u16, negative: u8| -> Option<f64> {
        let (_, _, at) = entry(gps, tag)?;
        Some(if *tiff.get(at)? == negative {
            -1.0
        } else {
            1.0
        })
    };
    let lat = degrees(2)? * sign(1, b'S')?;
    let lng = degrees(4)? * sign(3, b'W')?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lng)).then_some((lat, lng))
}

/// Encode RGBA pixels as a JPEG at `quality` with `meta` in it.
pub fn encode_rgba(
    rgba: &[u8],
    [w, h]: [u32; 2],
    quality: u8,
    meta: &Meta,
) -> Result<Vec<u8>, String> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())
        .ok_or_else(|| "the picture's size does not match its pixels".to_string())?;
    // A capture's alpha carries HDR brightness, not transparency.
    let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&rgb)
        .map_err(|e| format!("JPEG: {e}"))?;
    Ok(write(&out, meta))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Vec<u8> {
        let img = image::RgbImage::from_pixel(8, 6, image::Rgb([90, 120, 30]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
            .encode_image(&img)
            .unwrap();
        out
    }

    #[test]
    fn what_is_written_reads_back_and_the_picture_still_decodes() {
        let meta = Meta {
            exif: Some(b"II*\0\x08\0\0\0\0\0\0\0\0\0".to_vec()),
            xmp: Some(b"<x:xmpmeta/>".to_vec()),
        };
        let jpeg = write(&plain(), &meta);
        assert_eq!(read(&jpeg), meta);
        let back = image::load_from_memory(&jpeg).expect("still a JPEG");
        assert_eq!((back.width(), back.height()), (8, 6));
        // Nothing to write is the picture unchanged.
        assert_eq!(write(&plain(), &Meta::default()), plain());
        assert!(read(b"not a jpeg").is_empty());
    }

    #[test]
    fn a_photo_says_where_it_was_taken() {
        let pose: cartopolis_core::photo_pose::PhotoPose = serde_json::from_str(
            r#"{"lat":-33.8568,"lng":151.2153,"height_m":1.5,"heading_deg":9,
                "pitch_deg":0,"roll_deg":0,"fov_y_deg":60,"camera":"rear",
                "taken_ms":0,"source":"sensors","client":"x"}"#,
        )
        .unwrap();
        let meta = Meta {
            exif: Some(pose.exif()),
            xmp: None,
        };
        let (lat, lng) = gps(&write(&plain(), &meta)).expect("a position");
        assert!((lat + 33.8568).abs() < 1e-6, "{lat}");
        assert!((lng - 151.2153).abs() < 1e-6, "{lng}");
        assert_eq!(gps(&plain()), None);
    }

    #[test]
    fn a_turned_photo_is_marked_upright() {
        // Big-endian, one IFD entry: Orientation = 6 (turn 90°).
        let mut tiff = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0".to_vec();
        set_upright(&mut tiff);
        assert_eq!(&tiff[18..20], &[0, 1]);
        let mut le = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x08\0\0\0\0\0\0\0".to_vec();
        set_upright(&mut le);
        assert_eq!(&le[18..20], &[1, 0]);
    }
}
