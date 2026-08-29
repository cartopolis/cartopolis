//! The **aerial basemap**: PDOK's Luchtfoto RGB, streamed straight into the
//! clipmap in place of the drawn one.
//!
//! Everything the flat tier paints is a *render* of vector geometry, and it stops
//! being one at z14: Shortbread's deepest source zoom
//! ([`MAX_SOURCE_ZOOM`](crate::systems::vector_tiles::MAX_SOURCE_ZOOM)) is four
//! levels short of the clipmap's anchor zoom, so the ground under the camera is a
//! magnified z14 render — 179 of a fresh anchor's 424 tiles
//! (`docs/notes/tile-raster-cost.md`). PDOK serves those zooms natively, in true
//! colour, down to z21.
//!
//! **There is no extractor and there is deliberately not going to be one.** The
//! five surveyed layers have one each because their sources are WFS/GML needing
//! RD→WGS84 reprojection and tessellation. Imagery needs none of that: it is
//! already tiled, already Web Mercator, already CDN-cached and already
//! CORS-open, so the only honest shape is the one [`crate::systems::height`]
//! already uses — reach the service from the client, behind a Layers toggle that
//! is off by default. Mirroring it is not affordable either: the Netherlands at
//! z20 is ~78 million tiles at ~20 KB, i.e. ~1.5 TB.
//! `docs/notes/aerial-imagery.md` has the probe measurements and the licence.
//!
//! **Coverage is an envelope, not a status code.** PDOK answers a tile outside
//! the Netherlands with `200 OK` and a ~1.6 kB near-uniform JPEG rather than a
//! 404 (measured, see the note), so a client that decided coverage from the
//! response would paper the whole planet in grey. Nor is the service's own
//! `WGS84BoundingBox` usable: it describes the *tile matrix set*'s extent
//! (−1.66°..12.43° E, 48.04°..56.11° N — most of the Low Countries plus
//! Germany and half of France), not where imagery exists. So the envelope here is
//! AHN's, which is the same country and is already the one this client trusts.

use std::sync::OnceLock;

use cartopolis_core::world::tile_to_lat_lng;
use cartopolis_core::TileKey;

/// PDOK's public service host — the same one [`crate::systems::height`] reaches
/// for AHN. Unlike AHN this needs no same-origin relay on the web: the WMTS
/// answers `Access-Control-Allow-Origin: *`.
pub const AERIAL_ORIGIN: &str = "https://service.pdok.nl";

/// The Luchtfoto RGB WMTS entry point.
const AERIAL_PATH: &str = "/hwh/luchtfotorgb/wmts/v1_0";

/// The tile matrix set to request. `EPSG:3857` is Web Mercator with unpadded zoom
/// identifiers, which is exactly the `{z}/{x}/{y}` lattice the whole client
/// already speaks (`OGC:1.0:GoogleMapsCompatible` is the same grid under another
/// name; both are offered).
const AERIAL_MATRIX_SET: &str = "EPSG:3857";

/// The layer to draw, unless `CARTO_AERIAL_LAYER` says otherwise.
///
/// **25 cm, not the 8 cm `Actueel_orthoHR`**, for two reasons that are worth
/// stating so they are not re-litigated. The clipmap tops out at
/// `map_stream`'s anchor zoom of 17, whose ground scale at 53° N is ~0.36 m/px
/// at `tile_px = 512` and ~0.71 m/px at 256 — so 25 cm source is *already* finer
/// than anything this renderer can show, and orthoHR would be oversampled 4–5×
/// for the same bytes per tile. And ortho25 is the summer flight, which matches
/// the foliage `systems::vegetation` and `systems::grass` draw on top of it;
/// orthoHR is flown leaf-off in winter, so trees would stand over bare branches.
pub const DEFAULT_AERIAL_LAYER: &str = "Actueel_ortho25";

/// AHN's coverage envelope, out of its own `GetCapabilities` (`lonLatEnvelope`,
/// CRS84) — see [`crate::systems::height`], which states the same two constants
/// for the same service and the same country.
const AERIAL_LON: (f64, f64) = (3.20, 7.28);
const AERIAL_LAT: (f64, f64) = (50.72, 53.56);

/// The layer identifier this run draws.
///
/// `CARTO_AERIAL_LAYER` overrides it — the archive goes back to `2016_ortho25`
/// and a workstation comparing ortho25 against orthoHR needs no rebuild to do it.
/// The value is **validated rather than trusted**: it is interpolated into a URL
/// path, so anything outside `[A-Za-z0-9_-]` is refused and the default stands.
pub fn layer() -> &'static str {
    static LAYER: OnceLock<String> = OnceLock::new();
    LAYER
        .get_or_init(|| {
            let Ok(name) = std::env::var("CARTO_AERIAL_LAYER") else {
                return DEFAULT_AERIAL_LAYER.to_string();
            };
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                tracing::warn!(
                    requested = %name,
                    using = DEFAULT_AERIAL_LAYER,
                    "CARTO_AERIAL_LAYER is not a WMTS layer identifier; ignoring it"
                );
                return DEFAULT_AERIAL_LAYER.to_string();
            }
            name
        })
        .as_str()
}

/// The XYZ template for this run's layer.
///
/// A `{z}/{x}/{y}.jpeg` tail, which is all the rest of the client needs to know:
/// [`cartopolis_core::format_osm_tile_url`] substitutes the placeholders verbatim
/// and [`cartopolis_core::tile_source_kind`] classifies the `.jpeg` as raster, so
/// `tile_loader::fetch_tile_payload` takes the same decode path a PNG basemap
/// takes and nothing downstream branches on which source it came from.
pub fn template() -> &'static str {
    static TEMPLATE: OnceLock<String> = OnceLock::new();
    TEMPLATE
        .get_or_init(|| {
            format!(
                "{AERIAL_ORIGIN}{AERIAL_PATH}/{}/{AERIAL_MATRIX_SET}/{{z}}/{{x}}/{{y}}.jpeg",
                layer()
            )
        })
        .as_str()
}

/// Is this point inside the imagery envelope?
#[must_use]
pub fn covers(lat: f64, lng: f64) -> bool {
    lng >= AERIAL_LON.0 && lng <= AERIAL_LON.1 && lat >= AERIAL_LAT.0 && lat <= AERIAL_LAT.1
}

/// Is this tile drawn from the aerial source?
///
/// The **centre**, so one tile has one source and a tile is never half photograph
/// and half drawing. What it costs is the border: a coarse clipmap level's tile is
/// tens of kilometres across, so one whose centre is inside can still reach over
/// Germany or the North Sea, where PDOK renders blank. That is a look question
/// rather than a correctness one, and it is recorded as such in
/// `docs/notes/aerial-imagery.md`.
#[must_use]
pub fn covers_tile(key: &TileKey) -> bool {
    let (n_lat, w_lng) = tile_to_lat_lng(key.x, key.y, key.z);
    let (s_lat, e_lng) = tile_to_lat_lng(key.x + 1, key.y + 1, key.z);
    covers((n_lat + s_lat) / 2.0, (w_lng + e_lng) / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The template has to survive the two functions the whole tile path is built
    /// on: the placeholder substitution and the raster/vector classification. A
    /// vector classification here would send the JPEG through the overzoom
    /// resolver and hand it to the MVT reader.
    #[test]
    fn the_template_is_an_xyz_raster_source() {
        let url = cartopolis_core::format_osm_tile_url(template(), &TileKey::new(8425, 5405, 14));
        // Built from `layer()` rather than written out, so a developer with
        // `CARTO_AERIAL_LAYER` set still tests the shape and not their env.
        assert_eq!(
            url,
            format!(
                "https://service.pdok.nl/hwh/luchtfotorgb/wmts/v1_0/{}/\
EPSG:3857/14/8425/5405.jpeg",
                layer()
            ),
            "the probed URL shape changed"
        );
        assert_eq!(
            cartopolis_core::tile_source_kind(template()),
            cartopolis_core::TileSourceKind::Raster
        );
    }

    /// The default layer is a decision, not a detail — see the module note on why
    /// 25 cm summer beats 8 cm winter here — so it is pinned rather than left to
    /// whatever a later edit finds convenient.
    #[test]
    fn the_default_layer_is_the_summer_25cm_flight() {
        assert_eq!(DEFAULT_AERIAL_LAYER, "Actueel_ortho25");
    }

    /// Groningen is inside, Berlin is not — and Berlin is the case that matters,
    /// because PDOK answers it `200` with a blank tile rather than refusing.
    #[test]
    fn the_envelope_is_the_netherlands() {
        assert!(covers(53.2194, 6.5665), "Groningen");
        assert!(covers(51.9225, 4.4792), "Rotterdam");
        assert!(!covers(52.5200, 13.4050), "Berlin");
        assert!(!covers(48.8566, 2.3522), "Paris");
    }

    /// The tile test is the point test at the tile's centre, at every clipmap
    /// zoom — including the coarsest, where a tile is tens of kilometres wide.
    #[test]
    fn a_tile_is_classified_by_its_centre() {
        for z in 10..=17u32 {
            let (x, y) = cartopolis_core::world::lat_lng_to_tile(53.2194, 6.5665, z);
            assert!(covers_tile(&TileKey::new(x, y, z)), "Groningen at z{z}");
            let (bx, by) = cartopolis_core::world::lat_lng_to_tile(52.5200, 13.4050, z);
            assert!(!covers_tile(&TileKey::new(bx, by, z)), "Berlin at z{z}");
        }
    }
}
