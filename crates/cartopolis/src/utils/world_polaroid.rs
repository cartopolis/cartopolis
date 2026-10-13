//! A snapshot of the world, framed as a polaroid, for a public post
//! (`systems::community_posts`, `docs/notes/fediverse.md`).
//!
//! The white frame and the caption are drawn **into the picture**, not around
//! it by whoever shows it. A post goes to Mastodon and Lemmy as an ordinary
//! image attachment; drawn in, it reads as a polaroid there too. The tilt and
//! the shadow are left to whoever shows it (the app and the web page), since a
//! JPEG has no transparent corners to cast a shadow from.
//!
//! Pure pixels, on every target: a capture's RGBA in, a JPEG out, the caption
//! set with `fontdue` over the brand typeface as the map's street names are.

use fontdue::{Font, FontSettings};

/// The frame: even on three sides, deep at the bottom, as a polaroid's is.
const SIDE: u32 = 40;
const BOTTOM: u32 = 150;
/// The picture inside it: nearly square, as a polaroid's is.
const PHOTO_W: u32 = 960;
const PHOTO_H: u32 = 900;

/// The whole picture, in pixels.
pub const WIDTH: u32 = PHOTO_W + 2 * SIDE;
pub const HEIGHT: u32 = PHOTO_H + SIDE + BOTTOM;

/// The frame's white: a touch warm, as the card of a print is.
const FRAME: [u8; 3] = [0xFB, 0xFA, 0xF6];

/// Frame a `width`×`height` capture of the view as a polaroid, with `place`
/// and `date` written in the deep bottom edge. Answers a JPEG.
pub fn compose(
    rgba: &[u8],
    width: u32,
    height: u32,
    place: &str,
    date: &str,
) -> Result<Vec<u8>, String> {
    let shot = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or("the snapshot is not the size it says")?;
    // A capture's alpha carries HDR brightness, not transparency.
    let shot = image::DynamicImage::ImageRgba8(shot).to_rgb8();
    let photo = image::DynamicImage::ImageRgb8(shot)
        .resize_to_fill(PHOTO_W, PHOTO_H, image::imageops::FilterType::Triangle)
        .to_rgb8();

    let mut card = image::RgbImage::from_pixel(WIDTH, HEIGHT, image::Rgb(FRAME));
    // A little of a print's look: blacks lifted a shade and the light warmed,
    // and a soft darkening towards the corners.
    let (cx, cy) = (PHOTO_W as f32 / 2.0, PHOTO_H as f32 / 2.0);
    let reach = (cx * cx + cy * cy).sqrt();
    for (x, y, p) in photo.enumerate_pixels() {
        let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / reach;
        let shade = 1.0 - 0.18 * d.powi(2);
        let tone = |v: u8, warm: f32| -> u8 {
            let v = f32::from(v) / 255.0;
            let lifted = 0.035 + v * 0.95;
            ((lifted * warm * shade).clamp(0.0, 1.0) * 255.0).round() as u8
        };
        card.put_pixel(
            SIDE + x,
            SIDE + y,
            image::Rgb([tone(p[0], 1.03), tone(p[1], 1.0), tone(p[2], 0.95)]),
        );
    }
    // The faint line where the print meets its frame.
    for x in SIDE..SIDE + PHOTO_W {
        for y in [SIDE, SIDE + PHOTO_H - 1] {
            darken(&mut card, x, y, 0.85);
        }
    }
    for y in SIDE..SIDE + PHOTO_H {
        for x in [SIDE, SIDE + PHOTO_W - 1] {
            darken(&mut card, x, y, 0.85);
        }
    }

    let font = Font::from_bytes(crate::systems::theme::BRAND_TTF, FontSettings::default())
        .map_err(|e| format!("the brand font would not load: {e}"))?;
    let ink = {
        let c = crate::systems::theme::INK;
        [c.r(), c.g(), c.b()]
    };
    let name = if place.trim().is_empty() {
        "Cartopolis"
    } else {
        place.trim()
    };
    let room = (PHOTO_W - 40) as f32;
    let size = fit(&font, name, 46.0, 30.0, room);
    let top = SIDE + PHOTO_H;
    text(
        &mut card,
        &font,
        name,
        SIDE as f32 + 8.0,
        (top + 70) as f32,
        size,
        ink,
        0.85,
        room,
    );
    text(
        &mut card,
        &font,
        date,
        SIDE as f32 + 8.0,
        (top + 112) as f32,
        24.0,
        ink,
        0.45,
        room,
    );

    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
        .encode_image(&card)
        .map_err(|e| format!("could not encode the polaroid: {e}"))?;
    Ok(out)
}

fn darken(card: &mut image::RgbImage, x: u32, y: u32, by: f32) {
    let p = card.get_pixel_mut(x, y);
    for c in &mut p.0 {
        *c = (f32::from(*c) * by) as u8;
    }
}

/// The largest size from `max` down to `min` at which `s` fits `width`.
fn fit(font: &Font, s: &str, max: f32, min: f32, width: f32) -> f32 {
    let mut size = max;
    while size > min && measure(font, s, size) > width {
        size -= 2.0;
    }
    size
}

fn measure(font: &Font, s: &str, size: f32) -> f32 {
    s.chars().map(|c| font.metrics(c, size).advance_width).sum()
}

/// Write `s` with its baseline at (`x`, `baseline`), cut with an ellipsis
/// past `width`.
#[allow(clippy::too_many_arguments)]
fn text(
    card: &mut image::RgbImage,
    font: &Font,
    s: &str,
    x: f32,
    baseline: f32,
    size: f32,
    rgb: [u8; 3],
    alpha: f32,
    width: f32,
) {
    let ellipsis = font.metrics('…', size).advance_width;
    let mut chars: Vec<char> = s.chars().collect();
    if measure(font, s, size) > width {
        while !chars.is_empty()
            && measure(font, &chars.iter().collect::<String>(), size) + ellipsis > width
        {
            chars.pop();
        }
        chars.push('…');
    }
    let (w, h) = (card.width() as i32, card.height() as i32);
    let mut pen = x;
    for c in chars {
        let (m, bitmap) = font.rasterize(c, size);
        let gx = (pen + m.xmin as f32).round() as i32;
        let gy = (baseline - m.height as f32 - m.ymin as f32).round() as i32;
        for row in 0..m.height as i32 {
            for col in 0..m.width as i32 {
                let (px, py) = (gx + col, gy + row);
                if px < 0 || py < 0 || px >= w || py >= h {
                    continue;
                }
                let cov = f32::from(bitmap[(row * m.width as i32 + col) as usize]) / 255.0 * alpha;
                if cov <= 0.0 {
                    continue;
                }
                let p = card.get_pixel_mut(px as u32, py as u32);
                for (d, s) in p.0.iter_mut().zip(rgb) {
                    *d = (f32::from(s) * cov + f32::from(*d) * (1.0 - cov)).round() as u8;
                }
            }
        }
        pen += m.advance_width;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_becomes_a_framed_print() {
        let (w, h) = (320u32, 200u32);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i % 255) as u8, 90, 160, 255])
            .collect();
        let jpeg = compose(&rgba, w, h, "Oosterpoort, Groningen", "13 October 2026").unwrap();
        let back = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!((back.width(), back.height()), (WIDTH, HEIGHT));
        // The frame is white on every side, deepest at the bottom.
        for (x, y) in [
            (10, 10),
            (WIDTH - 10, HEIGHT / 2),
            (WIDTH - 10, HEIGHT - 10),
        ] {
            let p = back.get_pixel(x, y).0;
            assert!(
                p.iter().zip(FRAME).all(|(a, b)| a.abs_diff(b) < 10),
                "{x},{y}: {p:?}"
            );
        }
        assert!(BOTTOM > 3 * SIDE);
    }

    #[test]
    fn a_snapshot_that_lies_about_its_size_is_refused() {
        assert!(compose(&[0; 16], 100, 100, "x", "y").is_err());
    }

    #[test]
    fn a_long_name_is_set_smaller_before_it_is_cut() {
        let font =
            Font::from_bytes(crate::systems::theme::BRAND_TTF, FontSettings::default()).unwrap();
        let room = (PHOTO_W - 40) as f32;
        assert_eq!(fit(&font, "Vismarkt", 46.0, 30.0, room), 46.0);
        assert!(fit(&font, &"Oosterhaven ".repeat(6), 46.0, 30.0, room) < 46.0);
    }

    /// Frame a real capture, to look at:
    /// `CARTO_POLAROID_IN=shot.png CARTO_POLAROID_OUT=print.jpg cargo test -p
    /// cartopolis world_polaroid -- --ignored`.
    #[test]
    #[ignore = "writes a file to look at; needs CARTO_POLAROID_IN and _OUT"]
    fn draw_one_to_look_at() {
        let (Ok(input), Ok(out)) = (
            std::env::var("CARTO_POLAROID_IN"),
            std::env::var("CARTO_POLAROID_OUT"),
        ) else {
            return;
        };
        let shot = image::open(input).unwrap().to_rgba8();
        let (w, h) = shot.dimensions();
        let jpeg = compose(
            shot.as_raw(),
            w,
            h,
            "Oosterpoort, Groningen",
            "13 October 2026",
        )
        .unwrap();
        std::fs::write(out, jpeg).unwrap();
    }
}
