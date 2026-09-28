//! The aerial photograph of what a capture looks at, written beside it.
//!
//! **A render of a real place can be plausible and wrong**, and nothing in the
//! render says which. On 2026-09-28 the unified tunnels walled a cycle path across
//! the park beside Groningen's ring for a hundred metres: every number in the
//! capture's metrics was self-consistent, the picture looked like a sunken path,
//! and only an aerial photograph of the spot — fetched by hand, three tiles at a
//! time — showed a path at grade across the grass. So the shot harness can write
//! that photograph itself (`--aerial-reference`, or `aerial_reference = true` in
//! a manifest): `<shot>.aerial.jpg`, PDOK's Luchtfoto RGB over the ground from
//! the camera to the point it is aimed at, with the camera drawn as a red dot, the
//! line of sight as a red line and the aim as a yellow cross.
//!
//! Top-down, deliberately: an oblique photograph would need a second camera model
//! to line up with the render, and what it has to answer — is there a wall here,
//! a roof, a road, water — reads from above. The imagery is the same layer the
//! aerial basemap draws (`systems::aerial`), with the same envelope; outside it
//! nothing is written, and a failure to fetch is logged rather than failing the
//! capture it would only have been a reference for.
//!
//! Native only, and off the frame: the tiles are fetched on a thread of its own,
//! so a capture never waits on PDOK.

use std::path::PathBuf;

use image::{Rgb, RgbImage};

/// Tile edge in pixels.
const TILE_PX: f64 = 256.0;

/// The finest zoom the reference is drawn at: about 18 cm a pixel at Groningen's
/// latitude, well past what the 25 cm layer resolves.
const MAX_ZOOM: u32 = 19;

/// The coarsest it steps down to when the camera is far from its aim.
const MIN_ZOOM: u32 = 14;

/// The widest the reference may be, pixels, before it drops a zoom level.
const MAX_SIDE_PX: f64 = 1400.0;

/// How much ground to show around the camera and the aim, metres.
const MARGIN_M: f64 = 50.0;

/// What a reference is drawn around: where the camera is and where it looks, as
/// latitude and longitude.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub camera: (f64, f64),
    pub aim: (f64, f64),
}

/// Write the reference for `view` to `path` on a thread of its own. Nothing is
/// written outside the imagery's envelope.
pub fn spawn_write(path: PathBuf, view: View) {
    if !crate::systems::aerial::covers(view.aim.0, view.aim.1) {
        tracing::info!(path = %path.display(), "aerial reference: outside the imagery, none written");
        return;
    }
    std::thread::spawn(move || match render(view) {
        Ok(img) => match img.save(&path) {
            Ok(()) => tracing::info!(path = %path.display(), "aerial reference: saved"),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "aerial reference: not saved")
            }
        },
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "aerial reference: not fetched")
        }
    });
}

/// A point in world pixels at zoom `z` (Web Mercator, north-west origin).
fn world_px(lat: f64, lng: f64, z: u32) -> (f64, f64) {
    let scale = TILE_PX * f64::from(1u32 << z);
    let x = (lng + 180.0) / 360.0 * scale;
    let phi = lat.to_radians();
    let y = (1.0 - (phi.tan() + 1.0 / phi.cos()).ln() / std::f64::consts::PI) / 2.0 * scale;
    (x, y)
}

/// Metres per pixel at zoom `z` and latitude `lat`.
fn metres_per_px(lat: f64, z: u32) -> f64 {
    40_075_016.686 * lat.to_radians().cos() / (TILE_PX * f64::from(1u32 << z))
}

/// The zoom, and the pixel box at it, that shows both ends of `view` with
/// [`MARGIN_M`] around them in no more than [`MAX_SIDE_PX`] a side.
fn frame(view: View) -> (u32, [f64; 4]) {
    let mut z = MAX_ZOOM;
    loop {
        let (a, b) = (
            world_px(view.camera.0, view.camera.1, z),
            world_px(view.aim.0, view.aim.1, z),
        );
        let pad = MARGIN_M / metres_per_px(view.aim.0, z);
        let (x0, y0) = (a.0.min(b.0) - pad, a.1.min(b.1) - pad);
        let (x1, y1) = (a.0.max(b.0) + pad, a.1.max(b.1) + pad);
        if (x1 - x0).max(y1 - y0) <= MAX_SIDE_PX || z == MIN_ZOOM {
            return (z, [x0, y0, x1, y1]);
        }
        z -= 1;
    }
}

/// Fetch, stitch, crop and mark the reference for `view`.
fn render(view: View) -> Result<RgbImage, String> {
    let (z, [x0, y0, x1, y1]) = frame(view);
    let (tx0, ty0) = ((x0 / TILE_PX).floor() as u32, (y0 / TILE_PX).floor() as u32);
    let (tx1, ty1) = ((x1 / TILE_PX).floor() as u32, (y1 / TILE_PX).floor() as u32);
    let (w, h) = ((x1 - x0).ceil() as u32, (y1 - y0).ceil() as u32);
    let mut out = RgbImage::from_pixel(w, h, Rgb([40, 40, 40]));
    let template = crate::systems::aerial::template();
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let url = template
                .replace("{z}", &z.to_string())
                .replace("{x}", &tx.to_string())
                .replace("{y}", &ty.to_string());
            let bytes = futures_lite::future::block_on(crate::platform::http::fetch(
                crate::platform::http::Request::get(url),
            ))?;
            let tile = image::load_from_memory(&bytes)
                .map_err(|e| format!("tile {z}/{tx}/{ty}: {e}"))?
                .to_rgb8();
            let (ox, oy) = (f64::from(tx) * TILE_PX - x0, f64::from(ty) * TILE_PX - y0);
            for (px, py, c) in tile.enumerate_pixels() {
                let (dx, dy) = (ox + f64::from(px), oy + f64::from(py));
                if dx >= 0.0 && dy >= 0.0 && (dx as u32) < w && (dy as u32) < h {
                    out.put_pixel(dx as u32, dy as u32, *c);
                }
            }
        }
    }
    let at = |ll: (f64, f64)| {
        let p = world_px(ll.0, ll.1, z);
        (p.0 - x0, p.1 - y0)
    };
    mark(&mut out, at(view.camera), at(view.aim));
    Ok(out)
}

const RED: Rgb<u8> = Rgb([230, 40, 30]);
const YELLOW: Rgb<u8> = Rgb([255, 220, 0]);

/// The line of sight in red from the camera, a red dot at the camera and a
/// yellow cross at the aim.
fn mark(img: &mut RgbImage, camera: (f64, f64), aim: (f64, f64)) {
    let mut dot = |x: f64, y: f64, r: f64, c: Rgb<u8>| {
        let (w, h) = (img.width() as i64, img.height() as i64);
        let ri = r.ceil() as i64;
        for dy in -ri..=ri {
            for dx in -ri..=ri {
                if ((dx * dx + dy * dy) as f64) > r * r {
                    continue;
                }
                let (px, py) = (x as i64 + dx, y as i64 + dy);
                if px >= 0 && py >= 0 && px < w && py < h {
                    img.put_pixel(px as u32, py as u32, c);
                }
            }
        }
    };
    let len = (aim.0 - camera.0).hypot(aim.1 - camera.1).max(1.0);
    let steps = len.ceil() as usize;
    for k in 0..=steps {
        let t = k as f64 / steps as f64;
        dot(
            camera.0 + (aim.0 - camera.0) * t,
            camera.1 + (aim.1 - camera.1) * t,
            1.2,
            RED,
        );
    }
    for k in -10..=10 {
        let k = f64::from(k);
        dot(aim.0 + k, aim.1 + k, 1.2, YELLOW);
        dot(aim.0 + k, aim.1 - k, 1.2, YELLOW);
    }
    dot(camera.0, camera.1, 6.0, RED);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both ends in frame with room around them, at the finest zoom that fits.
    #[test]
    fn a_close_view_is_framed_at_full_detail() {
        let view = View {
            camera: (53.204932, 6.572974),
            aim: (53.20513, 6.573829),
        };
        let (z, [x0, y0, x1, y1]) = frame(view);
        assert_eq!(z, MAX_ZOOM);
        for (lat, lng) in [view.camera, view.aim] {
            let (x, y) = world_px(lat, lng, z);
            assert!(x > x0 && x < x1 && y > y0 && y < y1);
        }
        assert!((x1 - x0).max(y1 - y0) <= MAX_SIDE_PX);
    }

    /// A camera a few kilometres from its aim drops zoom rather than asking for
    /// a hundred tiles.
    #[test]
    fn a_far_view_drops_a_zoom() {
        let view = View {
            camera: (53.18, 6.52),
            aim: (53.2053, 6.575),
        };
        let (z, [x0, y0, x1, y1]) = frame(view);
        assert!(z < MAX_ZOOM);
        assert!((x1 - x0).max(y1 - y0) <= MAX_SIDE_PX || z == MIN_ZOOM);
    }

    /// The camera is marked red and the aim yellow, where they are.
    #[test]
    fn the_camera_and_the_aim_are_marked() {
        let mut img = RgbImage::from_pixel(200, 200, Rgb([0, 0, 0]));
        mark(&mut img, (40.0, 40.0), (160.0, 160.0));
        assert_eq!(*img.get_pixel(40, 40), RED);
        assert_eq!(*img.get_pixel(100, 100), RED, "the line of sight is drawn");
        assert_eq!(*img.get_pixel(168, 152), YELLOW, "the aim is crossed");
    }
}
