//! Canonical coordinate + OSM tile mapping (ADR-006).

use serde::{Deserialize, Serialize};

/// OSM slippy-map tile key (z/x/y).
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileKey {
    pub x: i64,
    pub y: i64,
    pub z: u32,
}

impl TileKey {
    #[must_use]
    pub fn new(x: i64, y: i64, z: u32) -> Self {
        Self { x, y, z }
    }

    /// Path segment for `.../{z}/{x}/{y}.png`
    #[must_use]
    pub fn to_path(&self) -> String {
        format!("{}/{}/{}", self.z, self.x, self.y)
    }
}

const EARTH_RADIUS: f64 = 6_378_137.0;
const EARTH_CIRCUMFERENCE: f64 = 2.0 * std::f64::consts::PI * EARTH_RADIUS;

/// Fixed zoom for region ground tiles (ADR-004 / ADR-006).
pub const REGION_ZOOM_LEVEL: u32 = 17;
pub const REGION_SIZE_METERS: f64 = 256.0;

/// Default map tile source: our **self-hosted** VersaTiles **vector** (Shortbread
/// MVT) server, the client-side tessellated-and-lit basemap of ADR-035. This is a
/// garage44 mirror of `download.versatiles.org`'s planet archive (z0–14), served by
/// the `versatiles` server behind Caddy — same schema and URL shape as the public
/// `tiles.versatiles.org`, so it is a drop-in origin swap off their hosting.
/// Shared so the sim's advertised default (`vibe_sim::config`) and the client's
/// offline/`--shot` fallback (`viberfox::systems::tile_loader`) cannot drift apart —
/// a mismatch there is how solo mode silently renders the old raster tiles instead
/// of the vector map.
///
/// A `{z}/{x}/{y}` template with **no extension**, which `tile_source_kind`
/// classifies as vector. Override per-run with `VIBE_TILE_URL` (client) or the
/// sim's `osm_tile_url_template` config; a raster template (e.g. one ending
/// `.png`) still works and falls back to the pre-rendered path.
pub const DEFAULT_TILE_URL_TEMPLATE: &str = "https://tiles.garage44.eu/tiles/osm/{z}/{x}/{y}";

/// WGS84 → OSM tile index (Web Mercator).
#[must_use]
pub fn lat_lng_to_tile(lat: f64, lng: f64, zoom: u32) -> (i64, i64) {
    let n = 2.0_f64.powi(zoom as i32);
    let x = ((lng + 180.0) / 360.0 * n).floor() as i64;
    let lat_rad = lat.to_radians();
    let y = ((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n)
        .floor() as i64;
    (x, y)
}

/// Tile corner (north-west) as WGS84.
#[must_use]
pub fn tile_to_lat_lng(x: i64, y: i64, zoom: u32) -> (f64, f64) {
    let n = 2.0_f64.powi(zoom as i32);
    let lng = (x as f64 / n) * 360.0 - 180.0;
    let lat_rad = ((1.0 - (2.0 * y as f64) / n) * std::f64::consts::PI).sinh().atan();
    let lat = lat_rad.to_degrees();
    (lat, lng)
}

#[must_use]
pub fn tile_to_meters(zoom: u32, lat: f64) -> f64 {
    let lat_rad = lat.to_radians();
    let meters_per_pixel =
        (EARTH_CIRCUMFERENCE * lat_rad.cos()) / (256.0 * 2.0_f64.powi(zoom as i32));
    meters_per_pixel * 256.0
}

#[must_use]
pub fn find_optimal_zoom(target_meters: f64, lat: f64) -> u32 {
    let mut best_zoom = 0;
    let mut best_diff = f64::INFINITY;
    for zoom in 0..=19 {
        let meters = tile_to_meters(zoom, lat);
        let diff = (meters - target_meters).abs();
        if diff < best_diff {
            best_diff = diff;
            best_zoom = zoom;
        }
    }
    best_zoom
}

/// Derive default tile key for a region anchor at [`REGION_ZOOM_LEVEL`].
#[must_use]
pub fn tile_key_from_lat_lng(lat: f64, lng: f64) -> TileKey {
    let (x, y) = lat_lng_to_tile(lat, lng, REGION_ZOOM_LEVEL);
    TileKey::new(x, y, REGION_ZOOM_LEVEL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    #[test]
    fn groningen_tile_roundtrip_rough() {
        let lat = 53.2194_f64;
        let lng = 6.5665_f64;
        let (x, y) = lat_lng_to_tile(lat, lng, REGION_ZOOM_LEVEL);
        let (lat2, lng2) = tile_to_lat_lng(x, y, REGION_ZOOM_LEVEL);
        assert_relative_eq!(lat, lat2, epsilon = 0.05);
        assert_relative_eq!(lng, lng2, epsilon = 0.05);
    }
}
