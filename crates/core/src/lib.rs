//! Shared types for the Cartopolis sim and client (ADR-006, ADR-009, ADR-015).

pub mod address;
pub mod error;
pub mod field_check;
pub mod geo;
pub mod lens;
pub mod meeting;
pub mod model_limits;
pub mod moderation;
pub mod motion;
pub mod particles;
pub mod permissions;
pub mod picture_limits;
pub mod placeholders;
pub mod point;
pub mod posts;
pub mod protocol;
pub mod script_api;
pub mod shot_env;
pub mod sounds;
pub mod tone;
pub mod ts_mirror;
pub mod video_limits;
pub mod world;
pub mod ws_address;
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
pub use permissions::{Permission, PermissionSet};
pub use protocol::{
    decode_app_frame, decode_message, encode_app_frame, encode_message, message_kind,
    message_request_id, topic_name, AuthCredential, AvatarLook, AvatarStateDto, CallAction,
    CallEndReason, CallMember, CallRole, CallRooms, CallSettings, CallTrack, CallTrackKind,
    CallWaiter, ChatScope, CommunityDto, CommunityListing, CommunityMemberDto, CommunityPlace,
    CommunityReportDto, CommunityRole, CommunitySettings, ContactDto, ContactRequestDto,
    ContactSettings, DirectoryOrder, E2eeCursor, E2eeKeyPackage, E2eeNewDevice, E2eeWelcome, Emote,
    EmoteState, GeoAnchor, JoinRequestDto, JoinRule, ListingState, MessageKind, MetDto,
    NearbyPersonDto, NetMessage, PersonDto, PersonWhereDto, PrimBody, PrimDto, PrimGeometry,
    PrimSurface, PrimVideo, ProtocolRevision, Relation, ReportReason, RequestPolicy, ScriptLevel,
    SetVideo, TextureEntry, TopicDto, CALL_CONTROL_REQUEST, CALL_GUEST_HOURS,
    COMMUNITY_INVITE_DAYS, EMOTE_REPLICATE_SECS, MAX_REPORT_MESSAGE_LEN, MAX_REPORT_NOTE_LEN,
    MAX_SEAT_OFFSET_M, PROTOCOL_HISTORY, PROTOCOL_VERSION,
};
pub use script_api::{api_in_group, ScriptFn, ScriptGroup, SCRIPT_API, SCRIPT_EVENTS};
pub use shot_env::ShotEnv;
pub use sounds::{
    PrimSound, SoundName, DEFAULT_SOUND_RANGE_M, MAX_SOUND_RANGE_M, MIN_SOUND_RANGE_M,
};
pub use tone::{HandPose, ToneSpec};
pub use world::{
    find_optimal_zoom, format_osm_tile_url, lat_lng_to_tile, lat_lng_to_tile_frac,
    tile_source_kind, tile_to_lat_lng, tile_to_meters, TileKey, TileSourceKind, DEFAULT_TILE_ZOOM,
};
pub use yaw::{snap_yaw_continuation, wrap_angle_pi};
