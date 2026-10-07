//! Land cover: the WorldCover classes painted under every raster tile, in the same tones as
//! the OSM land.

use super::*;

/// Deepest zoom the land-cover tile set is published at. A deeper source tile
/// reads its ancestor at this zoom ([`landcover_fills`]).
pub const LANDCOVER_MAX_ZOOM: u32 = 10;

/// The tone a land-cover class is painted in, or `None` for one this map does
/// not draw (open water, which the map's own water layers own).
///
/// The land cover is VersaTiles' `landcover-vectors`, ESA WorldCover 2021
/// classified into Shortbread-style `kind`s. It exists because the OSM tiles
/// carry **no `land` layer below z7–z8**: from orbit every continent was the
/// bare [`palette::PAPER`], and no palette could make the globe look like Earth.
/// It reads the same [`LAND_KINDS`] table as the OSM land, so the two are one
/// map style; only built-up land takes the far band's tone, since the land
/// cover is only ever the far picture.
#[must_use]
pub fn landcover_rgb(kind: &str) -> Option<u32> {
    match kind {
        "water" => None,
        "residential" => Some(palette::RESIDENTIAL_FAR),
        k => LAND_KINDS
            .iter()
            .find(|(name, _)| *name == k)
            .map(|(_, rgb)| *rgb),
    }
}

/// A land-cover tile's classes as [`OverlayFill`]s in a source tile's unit
/// square, for [`render_mvt_layered`]'s underlay.
///
/// `shift` is how many zooms the source tile sits below the land-cover tile and
/// `(ox, oy)` is its index within it — (0, 0, 0) when they are the same tile.
/// Rings that land wholly off the source tile are dropped, since a z14 tile reads
/// a z10 ancestor and would otherwise paint 256 tiles' worth of polygons to keep
/// one.
#[must_use]
pub fn landcover_fills(tile: &TileData, shift: u32, ox: u32, oy: u32) -> Vec<OverlayFill> {
    let k = (1u32 << shift.min(16)) as f32;
    let mut out = Vec::new();
    for layer_name in ["land", "water_polygons"] {
        let Some(meta) = tile.layer(layer_name) else {
            continue;
        };
        let extent = TileData::extent(meta);
        let to_unit = 1.0 / extent;
        for feature in tile.features(meta) {
            let Some(rgb) = kind_of(feature).and_then(landcover_rgb) else {
                continue;
            };
            let polys: Vec<&geo_types::Polygon<f32>> = match &feature.geometry {
                Geometry::Polygon(p) => vec![p],
                Geometry::MultiPolygon(mp) => mp.iter().collect(),
                _ => continue,
            };
            for poly in polys {
                let rings: Vec<Vec<[f32; 2]>> = clipped_rings(poly, to_unit, extent)
                    .into_iter()
                    .map(|r| {
                        r.into_iter()
                            .map(|p| [p[0] * k - ox as f32, p[1] * k - oy as f32])
                            .collect()
                    })
                    .collect();
                let Some(outer) = rings.first() else {
                    continue;
                };
                let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
                for p in outer {
                    for a in 0..2 {
                        lo[a] = lo[a].min(p[a]);
                        hi[a] = hi[a].max(p[a]);
                    }
                }
                if hi[0] < 0.0 || hi[1] < 0.0 || lo[0] > 1.0 || lo[1] > 1.0 {
                    continue;
                }
                out.push(OverlayFill { rings, rgb });
            }
        }
    }
    out
}
