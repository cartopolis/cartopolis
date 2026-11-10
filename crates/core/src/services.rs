//! Where a server's geo services are: the answer to `GET /api/services`.
//!
//! The client used to carry these addresses compiled in, so a second server could
//! not point its players at its own search, map data or terrain: every client
//! asked cartopolis.org whatever it had signed in to. Now the server says, the
//! way it already says where its tiles are (`ServerHelloAck::osm_tile_url_template`).
//!
//! **An HTTP document, not a handshake field.** Asked once after sign-in, it moves
//! no protocol version, an older server answers 404 and the client keeps its
//! defaults, and an older client never asks. The defaults below are those
//! compiled-in addresses, so a server that configures nothing tells its clients
//! what they would have used anyway.
//!
//! Each address is used as the client always used it, so pointing one elsewhere
//! needs a service that answers the same requests: a Nominatim-shaped geocoder,
//! an Overpass interpreter, atlas's cell tree, atlas's terrain and height. An
//! empty `map_data` or `terrain` turns that layer off, as their `CARTO_*`
//! overrides do; an empty `height` makes the client ask PDOK itself; an empty
//! `osm_query` says the server runs no Overpass, and the client asks the public
//! one, politely, for whatever the map data does not cover.

use serde::{Deserialize, Serialize};

/// The geocoder: Nominatim's `/search` and `/reverse`, `jsonv2`.
pub const DEFAULT_GEOCODER_URL: &str = "https://nominatim.cartopolis.org";
/// The OSM query endpoint: an Overpass interpreter, queried by POST `data=`.
pub const DEFAULT_OSM_QUERY_URL: &str = "https://overpass.cartopolis.org/api/interpreter";
/// The cell tree atlas builds (`<dataset>/14/x/y.json`, `coverage.json`).
pub const DEFAULT_MAP_DATA_URL: &str = "https://3dbag.cartopolis.org";
/// Terrain tiles, a `{z}/{x}/{y}` template, served by atlas.
pub const DEFAULT_TERRAIN_URL: &str = "https://cartopolis.org/terrain/{z}/{x}/{y}?grid=513&ahn=6";
/// Height patches, served by atlas.
pub const DEFAULT_HEIGHT_URL: &str = "https://cartopolis.org/height";

/// The path a server answers its [`ServiceDirectory`] on, below its API base.
pub const SERVICES_PATH: &str = "/api/services";

/// Where each geo service is. Every field is a full URL (or template).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceDirectory {
    pub geocoder: String,
    pub osm_query: String,
    pub map_data: String,
    pub terrain: String,
    pub height: String,
}

impl Default for ServiceDirectory {
    fn default() -> Self {
        Self {
            geocoder: DEFAULT_GEOCODER_URL.into(),
            osm_query: DEFAULT_OSM_QUERY_URL.into(),
            map_data: DEFAULT_MAP_DATA_URL.into(),
            terrain: DEFAULT_TERRAIN_URL.into(),
            height: DEFAULT_HEIGHT_URL.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_document_keeps_the_defaults_it_does_not_name() {
        let d: ServiceDirectory =
            serde_json::from_str(r#"{"geocoder":"https://geo.example.org"}"#).unwrap();
        assert_eq!(d.geocoder, "https://geo.example.org");
        assert_eq!(d.osm_query, DEFAULT_OSM_QUERY_URL);
        assert_eq!(d.terrain, DEFAULT_TERRAIN_URL);
    }

    #[test]
    fn an_empty_field_survives_as_empty() {
        let d: ServiceDirectory = serde_json::from_str(r#"{"map_data":""}"#).unwrap();
        assert!(d.map_data.is_empty());
    }
}
