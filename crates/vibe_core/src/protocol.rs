//! Wire messages (ADR-009). TCP framing is length-delimited (ADR-008); each frame body is an **app frame**:
//! `protocol_version` (u16 LE) + `message_kind` (u16 LE) + `request_id` (u32 LE) + postcard(`NetMessage`).

use glam::Vec3;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ProtocolError;

/// Bump when the app-frame layout or postcard schema changes incompatibly.
/// v8: prim texture surface params (alpha/glow/full-bright/repeats/rotation/offset).
/// v9: authenticated handshake — `ClientHello.auth`, `ServerHelloAck.principal_id`
///     + `session_token` (ADR-016).
/// v10: geo-anchored prims — `PrimDto.geo`, `CreatePrim`/`UpdatePrim` carry a
///      mandatory `GeoAnchor`; prim horizontal placement is lat/lng (ADR-029).
pub const PROTOCOL_VERSION: u16 = 10;

const APP_HEADER_LEN: usize = 8;

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    ClientHello = 1,
    ServerHelloAck = 2,
    ServerError = 3,
    ClientIntent = 4,
    ObserverUpdate = 5,
    WorldSnapshot = 6,
    PrimRemoved = 7,
    CreatePrim = 8,
    UpdatePrim = 9,
    DeletePrim = 10,
    PrimUpsert = 11,
    TextureCatalogRequest = 12,
    TextureCatalog = 13,
    TextureRequest = 14,
    TextureData = 15,
    TextureNotFound = 16,
}

impl MessageKind {
    #[must_use]
    pub fn from_wire(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::ClientHello),
            2 => Some(Self::ServerHelloAck),
            3 => Some(Self::ServerError),
            4 => Some(Self::ClientIntent),
            5 => Some(Self::ObserverUpdate),
            6 => Some(Self::WorldSnapshot),
            7 => Some(Self::PrimRemoved),
            8 => Some(Self::CreatePrim),
            9 => Some(Self::UpdatePrim),
            10 => Some(Self::DeletePrim),
            11 => Some(Self::PrimUpsert),
            12 => Some(Self::TextureCatalogRequest),
            13 => Some(Self::TextureCatalog),
            14 => Some(Self::TextureRequest),
            15 => Some(Self::TextureData),
            16 => Some(Self::TextureNotFound),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextureEntry {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegionDto {
    pub id: i64,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub tile_x: i64,
    pub tile_y: i64,
    pub tile_z: i64,
    /// Sim-space origin for this region’s tile (matches `vibe_sim` grid layout).
    pub sim_x: f32,
    pub sim_y: f32,
    pub sim_z: f32,
}

/// Per-prim texture surface parameters (Texture tab). Mirrors a subset of the
/// Second Life texture tab: overall transparency, glow, full-bright, and the
/// texture UV transform (repeats per face, flip, rotation, offset).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PrimSurface {
    /// Overall opacity, 0.0 (fully transparent) – 1.0 (opaque). SL "Transparency %" = (1 − alpha) · 100.
    pub alpha: f32,
    /// Surface glow, 0.0 – 1.0. Drives emissive (separate from scene lighting).
    pub glow: f32,
    /// Render fully lit regardless of the day/night sun position (SL "Full Bright").
    pub full_bright: bool,
    /// Texture repeats per face along U / V (tiling count). Default 1.0.
    pub repeat_u: f32,
    pub repeat_v: f32,
    /// Mirror the texture along U / V.
    pub flip_u: bool,
    pub flip_v: bool,
    /// Texture rotation in degrees.
    pub rotation: f32,
    /// Texture offset along U / V (fraction of a repeat).
    pub offset_u: f32,
    pub offset_v: f32,
}

impl Default for PrimSurface {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            glow: 0.0,
            full_bright: false,
            repeat_u: 1.0,
            repeat_v: 1.0,
            flip_u: false,
            flip_v: false,
            rotation: 0.0,
            offset_u: 0.0,
            offset_v: 0.0,
        }
    }
}

/// Per-prim geometry deformation parameters (Object tab): path cut, hollow, and the
/// twist / taper / top-shear / slice "warp" set. Grouped so the online `UpdatePrim`
/// write path can carry them without a 20-field message variant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PrimGeometry {
    pub path_cut_begin: f32,
    pub path_cut_end: f32,
    pub hollow: f32,
    pub twist_begin: f32,
    pub twist_end: f32,
    pub taper_x: f32,
    pub taper_y: f32,
    pub top_shear_x: f32,
    pub top_shear_y: f32,
    pub slice_begin: f32,
    pub slice_end: f32,
}

impl Default for PrimGeometry {
    fn default() -> Self {
        Self {
            path_cut_begin: 0.0,
            path_cut_end: 1.0,
            hollow: 0.0,
            twist_begin: 0.0,
            twist_end: 0.0,
            taper_x: 0.0,
            taper_y: 0.0,
            top_shear_x: 0.0,
            top_shear_y: 0.0,
            slice_begin: 0.0,
            slice_end: 1.0,
        }
    }
}

/// Absolute geographic placement of a prim's horizontal position (ADR-029).
///
/// This is the anchor-independent ground truth: the client derives the
/// render-space `GridCell` + local `Transform` from it via the *current* OSM
/// anchor (`lat_lng_to_world`), so a prim keeps its real-world location across a
/// re-anchor. Altitude is carried separately in `PrimDto::position.y`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct GeoAnchor {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrimDto {
    pub id: i64,
    pub region_id: i64,
    pub name: String,
    pub shape: String,
    /// Sim-space position (metres). `position.y` is the authoritative altitude;
    /// horizontal placement (x/z) is derived from `geo` (ADR-029), so x/z here are
    /// only a transient echo of the last render frame.
    pub position: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
    pub color: [f32; 3],
    /// Optional texture key from standard library (ADR-017).
    #[serde(default)]
    pub texture_id: Option<String>,
    #[serde(default)]
    pub path_cut_begin: f32,
    #[serde(default = "default_path_cut_end")]
    pub path_cut_end: f32,
    #[serde(default)]
    pub hollow: f32,
    #[serde(default)]
    pub twist_begin: f32,
    #[serde(default)]
    pub twist_end: f32,
    #[serde(default)]
    pub taper_x: f32,
    #[serde(default)]
    pub taper_y: f32,
    #[serde(default)]
    pub top_shear_x: f32,
    #[serde(default)]
    pub top_shear_y: f32,
    #[serde(default)]
    pub slice_begin: f32,
    #[serde(default = "default_slice_end")]
    pub slice_end: f32,
    /// Texture surface params (ADR-017 / protocol v8).
    #[serde(default)]
    pub surface: PrimSurface,
    /// Absolute geographic placement (ADR-029 / protocol v10). Every prim is
    /// geo-anchored — there is no legacy fixed-origin placement.
    pub geo: GeoAnchor,
}

fn default_path_cut_end() -> f32 {
    1.0
}

fn default_slice_end() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AvatarStateDto {
    pub id: u64,
    pub position: Vec3,
    pub yaw: f32,
}

/// Credential presented in the [`NetMessage::ClientHello`] handshake (ADR-016).
/// The server verifies it against its local account store and issues a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AuthCredential {
    /// A previously issued session token (silent reconnect).
    Token(String),
    /// Username + password. Verified server-side with argon2; never stored
    /// client-side (persist the returned session token instead).
    Password { username: String, password: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NetMessage {
    ClientHello {
        protocol_version: u16,
        /// ADR-016: credential the server authenticates before any world traffic.
        auth: AuthCredential,
    },
    ServerHelloAck {
        session_id: Uuid,
        tick_hz: f32,
        your_avatar_id: u64,
        /// ADR-016: stable account id bound to this connection.
        principal_id: Uuid,
        /// ADR-016: authoritative account name for this principal (display/UI).
        username: String,
        /// ADR-016: opaque session token to persist for silent reconnect.
        session_token: String,
        /// ADR-014: operator tile URL; `{z}/{x}/{y}` placeholders. Empty = client default.
        #[serde(default)]
        osm_tile_url_template: String,
    },
    ServerError {
        request_id: u32,
        code: u32,
        message: String,
    },
    ClientIntent {
        request_id: u32,
        move_x: f32,
        move_z: f32,
        /// World yaw replicated to others (fox / travel facing). Server applies this when moving instead of `atan2(velocity)`.
        display_yaw: f32,
        fly_up: bool,
        fly_down: bool,
    },
    ObserverUpdate {
        position: Vec3,
    },
    WorldSnapshot {
        tick: u64,
        regions: Vec<RegionDto>,
        prims: Vec<PrimDto>,
        avatars: Vec<AvatarStateDto>,
    },
    /// ADR-011 delta v0: remove prim by stable id (server-assigned).
    PrimRemoved {
        id: i64,
    },
    /// ADR-017 / ADR-018: client requests creation of a new prim with full initial state.
    CreatePrim {
        request_id: u32,
        region_id: i64,
        name: String,
        position: Vec3,
        rotation: Vec3,
        scale: Vec3,
        color: [f32; 3],
        texture_id: Option<String>,
        shape: String,
        #[serde(default)]
        surface: PrimSurface,
        #[serde(default)]
        geometry: PrimGeometry,
        /// Absolute geo placement (ADR-029).
        geo: GeoAnchor,
    },
    /// ADR-017: client sends updated prim state (position, rotation, scale, color, texture, name).
    UpdatePrim {
        request_id: u32,
        prim_id: i64,
        position: Vec3,
        rotation: Vec3,
        scale: Vec3,
        color: [f32; 3],
        texture_id: Option<String>,
        name: String,
        /// Texture surface params (ADR-017 / protocol v8).
        #[serde(default)]
        surface: PrimSurface,
        /// Geometry deformation params (ADR-018 / protocol v8).
        #[serde(default)]
        geometry: PrimGeometry,
        /// Absolute geo placement (ADR-029).
        geo: GeoAnchor,
    },
    /// ADR-017: client requests deletion of a prim by id.
    DeletePrim {
        request_id: u32,
        prim_id: i64,
    },
    /// ADR-017: server broadcasts the full authoritative prim state after a create or update.
    PrimUpsert {
        prim: PrimDto,
    },
    /// Client requests the full texture catalog from the sim.
    TextureCatalogRequest,
    /// Sim responds with the list of available textures.
    TextureCatalog {
        textures: Vec<TextureEntry>,
    },
    /// Client requests raw PNG bytes for a specific texture.
    TextureRequest {
        request_id: u32,
        texture_id: String,
    },
    /// Sim sends raw PNG bytes for the requested texture.
    TextureData {
        request_id: u32,
        texture_id: String,
        png_bytes: Vec<u8>,
    },
    /// Sim signals that the requested texture id does not exist.
    TextureNotFound {
        request_id: u32,
        texture_id: String,
    },
}

#[must_use]
pub fn message_kind(msg: &NetMessage) -> MessageKind {
    match msg {
        NetMessage::ClientHello { .. } => MessageKind::ClientHello,
        NetMessage::ServerHelloAck { .. } => MessageKind::ServerHelloAck,
        NetMessage::ServerError { .. } => MessageKind::ServerError,
        NetMessage::ClientIntent { .. } => MessageKind::ClientIntent,
        NetMessage::ObserverUpdate { .. } => MessageKind::ObserverUpdate,
        NetMessage::WorldSnapshot { .. } => MessageKind::WorldSnapshot,
        NetMessage::PrimRemoved { .. } => MessageKind::PrimRemoved,
        NetMessage::CreatePrim { .. } => MessageKind::CreatePrim,
        NetMessage::UpdatePrim { .. } => MessageKind::UpdatePrim,
        NetMessage::DeletePrim { .. } => MessageKind::DeletePrim,
        NetMessage::PrimUpsert { .. } => MessageKind::PrimUpsert,
        NetMessage::TextureCatalogRequest => MessageKind::TextureCatalogRequest,
        NetMessage::TextureCatalog { .. } => MessageKind::TextureCatalog,
        NetMessage::TextureRequest { .. } => MessageKind::TextureRequest,
        NetMessage::TextureData { .. } => MessageKind::TextureData,
        NetMessage::TextureNotFound { .. } => MessageKind::TextureNotFound,
    }
}

#[must_use]
pub fn message_request_id(msg: &NetMessage) -> u32 {
    match msg {
        NetMessage::ClientIntent { request_id, .. } => *request_id,
        NetMessage::ServerError { request_id, .. } => *request_id,
        NetMessage::CreatePrim { request_id, .. } => *request_id,
        NetMessage::UpdatePrim { request_id, .. } => *request_id,
        NetMessage::DeletePrim { request_id, .. } => *request_id,
        NetMessage::TextureRequest { request_id, .. } => *request_id,
        NetMessage::TextureData { request_id, .. } => *request_id,
        NetMessage::TextureNotFound { request_id, .. } => *request_id,
        _ => 0,
    }
}

/// Raw postcard body (no app header). Prefer [`encode_app_frame`] / [`decode_app_frame`] on the wire.
#[must_use]
pub fn encode_message(msg: &NetMessage) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(msg)
}

#[must_use]
pub fn decode_message(bytes: &[u8]) -> Result<NetMessage, postcard::Error> {
    postcard::from_bytes(bytes)
}

/// Full ADR-009 app frame: version + kind + request_id + postcard payload.
#[must_use]
pub fn encode_app_frame(msg: &NetMessage) -> Result<Vec<u8>, postcard::Error> {
    let body = encode_message(msg)?;
    let kind = message_kind(msg) as u16;
    let rid = message_request_id(msg);
    let mut out = Vec::with_capacity(APP_HEADER_LEN + body.len());
    out.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&rid.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

#[must_use]
pub fn decode_app_frame(bytes: &[u8]) -> Result<NetMessage, ProtocolError> {
    if bytes.len() < APP_HEADER_LEN {
        return Err(ProtocolError::FrameTooShort(bytes.len()));
    }
    let ver = u16::from_le_bytes(bytes[0..2].try_into().unwrap());
    if ver != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(ver));
    }
    let kind_wire = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
    let Some(kind) = MessageKind::from_wire(kind_wire) else {
        return Err(ProtocolError::UnknownMessageKind(kind_wire));
    };
    let _request_id = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let msg = decode_message(&bytes[APP_HEADER_LEN..])?;
    if message_kind(&msg) != kind {
        return Err(ProtocolError::KindMismatch {
            header: kind_wire,
            body: message_kind(&msg) as u16,
        });
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_hello_app_frame() {
        let m = NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: AuthCredential::Password {
                username: "dev".into(),
                password: "dev".into(),
            },
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_hello_ack_app_frame() {
        let m = NetMessage::ServerHelloAck {
            session_id: Uuid::nil(),
            tick_hz: 20.0,
            your_avatar_id: 7,
            principal_id: Uuid::nil(),
            username: "dev".into(),
            session_token: "deadbeef".into(),
            osm_tile_url_template: String::new(),
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_snapshot_app_frame() {
        let m = NetMessage::WorldSnapshot {
            tick: 42,
            regions: vec![],
            prims: vec![],
            avatars: vec![],
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_create_prim_app_frame() {
        let m = NetMessage::CreatePrim {
            request_id: 123,
            region_id: 1,
            name: "New Prim".into(),
            position: Vec3::new(10.0, 5.0, 20.0),
            rotation: Vec3::ZERO,
            scale: Vec3::ONE,
            color: [0.5, 0.5, 0.5],
            texture_id: None,
            shape: "box".into(),
            surface: PrimSurface::default(),
            geometry: PrimGeometry::default(),
            geo: GeoAnchor { latitude: 53.2194, longitude: 6.5665 },
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_update_prim_app_frame() {
        let m = NetMessage::UpdatePrim {
            request_id: 124,
            prim_id: 42,
            position: Vec3::new(10.0, 5.0, 20.0),
            rotation: Vec3::new(0.0, 90.0, 0.0),
            scale: Vec3::new(2.0, 2.0, 2.0),
            color: [1.0, 0.0, 0.0],
            texture_id: Some("brick".into()),
            name: "My Prim".into(),
            surface: PrimSurface::default(),
            geometry: PrimGeometry::default(),
            geo: GeoAnchor { latitude: 53.2194, longitude: 6.5665 },
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_delete_prim_app_frame() {
        let m = NetMessage::DeletePrim {
            request_id: 125,
            prim_id: 42,
        };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn roundtrip_prim_upsert_app_frame() {
        let prim = PrimDto {
            id: 42,
            region_id: 1,
            name: "Updated Prim".into(),
            shape: "sphere".into(),
            position: Vec3::new(15.0, 8.0, 25.0),
            rotation: Vec3::new(45.0, 45.0, 0.0),
            scale: Vec3::new(1.5, 1.5, 1.5),
            color: [0.0, 1.0, 0.0],
            texture_id: Some("grass".into()),
            path_cut_begin: 0.0,
            path_cut_end: 1.0,
            hollow: 0.0,
            twist_begin: 0.0,
            twist_end: 0.0,
            taper_x: 0.0,
            taper_y: 0.0,
            top_shear_x: 0.0,
            top_shear_y: 0.0,
            slice_begin: 0.0,
            slice_end: 1.0,
            surface: PrimSurface::default(),
            geo: GeoAnchor { latitude: 53.2194, longitude: 6.5665 },
        };
        let m = NetMessage::PrimUpsert { prim: prim.clone() };
        let b = encode_app_frame(&m).unwrap();
        let m2 = decode_app_frame(&b).unwrap();
        assert_eq!(m, m2);
    }
}
