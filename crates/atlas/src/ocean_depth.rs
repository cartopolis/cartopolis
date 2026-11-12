//! The sea floor's depth over the whole world, as one small Web-Mercator image
//! the client embeds (`cartopolis_geo::ocean_depth`), so the painted map can
//! colour the sea by how deep it is: light over the shelves, dark over the
//! abyss, the ridges and trenches between — the sea as every satellite mosaic
//! shows it, rather than one flat blue.
//!
//! The source is the open terrain tiles on AWS (Mapzen's "Terrarium" encoding,
//! `elevation-tiles-prod`), whose sea floor is ETOPO1 and GEBCO. Written once
//! and committed; rerun only to change the resolution or the encoding.

use std::path::Path;
use std::time::Duration;

/// Terrarium tiles, `{z}/{x}/{y}`.
const TERRARIUM: &str = "https://s3.amazonaws.com/elevation-tiles-prod/terrarium";

/// Tiles are 256 px, so zoom 3 is a 2048 px world: ~20 km a pixel at the
/// equator, 10 km at 60°.
pub const DEFAULT_ZOOM: u32 = 3;

/// The deepest depth the encoding tells apart, metres. Below it is all one
/// colour; the abyssal plains lie at 4–6 km.
pub const FULL_DEPTH_M: f32 = 8000.0;

/// Depth, metres, as one byte: `255·√(d / FULL_DEPTH_M)`, so the shelves get
/// most of the resolution (100 m is 28, 200 m is 40). Land and lakes above sea
/// level are 0. `cartopolis_geo::ocean_depth` decodes the same curve.
#[must_use]
pub fn encode(elevation_m: f32) -> u8 {
    let d = (-elevation_m).clamp(0.0, FULL_DEPTH_M);
    (255.0 * (d / FULL_DEPTH_M).sqrt()).round() as u8
}

/// A Terrarium pixel's elevation in metres.
fn terrarium(px: [u8; 3]) -> f32 {
    f32::from(px[0]) * 256.0 + f32::from(px[1]) + f32::from(px[2]) / 256.0 - 32768.0
}

/// Fetch the world at `zoom` and write it to `out` as an 8-bit grey PNG.
pub fn write(zoom: u32, out: &Path) -> Result<String, String> {
    let n = 1u32 << zoom;
    let edge = 256 * n;
    let mut img = image::GrayImage::new(edge, edge);
    for ty in 0..n {
        for tx in 0..n {
            let url = format!("{TERRARIUM}/{zoom}/{tx}/{ty}.png");
            let resp = ureq::get(&url)
                .timeout(Duration::from_secs(60))
                .call()
                .map_err(|e| format!("{url}: {e}"))?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut resp.into_reader(), &mut bytes)
                .map_err(|e| format!("{url}: {e}"))?;
            let tile = image::load_from_memory(&bytes)
                .map_err(|e| format!("{url}: {e}"))?
                .to_rgb8();
            for (x, y, px) in tile.enumerate_pixels() {
                img.put_pixel(
                    tx * 256 + x,
                    ty * 256 + y,
                    image::Luma([encode(terrarium(px.0))]),
                );
            }
        }
    }
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    img.save(out).map_err(|e| e.to_string())?;
    Ok(format!("{} ({edge}×{edge}, zoom {zoom})", out.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn land_is_zero_and_the_shelf_gets_the_resolution() {
        assert_eq!(encode(12.0), 0);
        assert_eq!(encode(0.0), 0);
        assert_eq!(encode(-200.0), 40);
        assert_eq!(encode(-FULL_DEPTH_M * 2.0), 255);
        // Terrarium: 32768 is sea level.
        assert_eq!(terrarium([128, 0, 0]), 0.0);
        assert_eq!(terrarium([127, 56, 0]), -200.0);
    }
}
