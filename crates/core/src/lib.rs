//! Shared types for the Cartopolis sim and client (ADR-006, ADR-009, ADR-015).

pub mod error;
pub mod geo;
pub mod moderation;
pub mod motion;
pub mod particles;
pub mod placeholders;
pub mod protocol;
pub mod script_api;
pub mod world;
pub mod yaw;

pub use error::ProtocolError;
pub use geo::{
    degree_scale_m, ecef_to_geodetic, geodetic_to_ecef, ground_distance_m, TangentFrame,
};
pub use moderation::Filter;
pub use particles::{
    ParticleEffect, MAX_PARTICLES, MAX_PARTICLE_GRAVITY, MAX_PARTICLE_LIFE_S, MAX_PARTICLE_SIZE,
    MAX_PARTICLE_SPEED,
};
pub use protocol::{
    decode_app_frame, decode_message, encode_app_frame, encode_message, message_kind,
    message_request_id, AuthCredential, AvatarStateDto, ChatScope, E2eeCursor, E2eeKeyPackage,
    E2eeWelcome, Emote, EmoteState, GeoAnchor, MessageKind, NetMessage, PrimDto, PrimGeometry,
    PrimSurface, ProtocolRevision, ReportReason, ScriptLevel, TextureEntry, EMOTE_REPLICATE_SECS,
    MAX_REPORT_MESSAGE_LEN, MAX_REPORT_NOTE_LEN, MAX_ROOM_LEN, MAX_SEAT_OFFSET_M, PROTOCOL_HISTORY,
    PROTOCOL_VERSION,
};
pub use script_api::{api_in_group, ScriptFn, ScriptGroup, SCRIPT_API, SCRIPT_EVENTS};
pub use world::{
    find_optimal_zoom, format_osm_tile_url, lat_lng_to_tile, lat_lng_to_tile_frac,
    tile_source_kind, tile_to_lat_lng, tile_to_meters, TileKey, TileSourceKind, DEFAULT_TILE_ZOOM,
};
pub use yaw::{snap_yaw_continuation, wrap_angle_pi};
