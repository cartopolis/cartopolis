//! Shared types for the Viberfox sim and client (ADR-006, ADR-009, ADR-015).

pub mod error;
pub mod geo;
pub mod protocol;
pub mod world;
pub mod yaw;

pub use error::ProtocolError;
pub use protocol::{
    decode_app_frame, decode_message, encode_app_frame, encode_message, message_kind,
    message_request_id, AuthCredential, AvatarStateDto, ChatScope, GeoAnchor, MessageKind,
    NetMessage, PrimDto, PrimGeometry, PrimSurface, ProtocolRevision, RegionDto, TextureEntry,
    PROTOCOL_HISTORY, PROTOCOL_VERSION,
};
pub use world::{
    find_optimal_zoom, format_osm_tile_url, lat_lng_to_tile, lat_lng_to_tile_frac,
    tile_source_kind, tile_to_lat_lng, tile_to_meters, TileKey, TileSourceKind,
    REGION_SIZE_METERS, REGION_ZOOM_LEVEL,
};
pub use geo::{
    ecef_to_geodetic, geodetic_to_ecef, region_index, region_origin, regions_adjacent,
    TangentFrame, REGION_SIZE,
};
pub use yaw::{snap_yaw_continuation, wrap_angle_pi};
