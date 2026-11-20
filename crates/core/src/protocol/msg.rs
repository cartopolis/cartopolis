//! The client-to-server messages' fields, one struct each.
//!
//! The variant names the message and the struct carries it, so the server's
//! `ClientRequest` holds the same struct instead of copying the field list.
//! postcard writes a newtype variant around a struct exactly as it wrote the
//! struct variant (the variant's index, then the fields in order), so moving
//! a message here changes no byte on the wire.

use glam::Vec3;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::*;
use crate::particles::ParticleEffect;

/// The fields of [`NetMessage::AnswerContact`](super::NetMessage::AnswerContact).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnswerContact {
    pub request_id: u32,
    pub username: String,
    pub accept: bool,
}

/// The fields of [`NetMessage::CallAnswer`](super::NetMessage::CallAnswer).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallAnswer {
    pub group: Vec<u8>,
    pub sdp: String,
}

/// The fields of [`NetMessage::CallControl`](super::NetMessage::CallControl).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallControl {
    pub group: Vec<u8>,
    pub action: CallAction,
}

/// The fields of [`NetMessage::CallDecline`](super::NetMessage::CallDecline).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallDecline {
    pub group: Vec<u8>,
}

/// The fields of [`NetMessage::CallInvite`](super::NetMessage::CallInvite).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallInvite {
    pub group: Vec<u8>,
}

/// The fields of [`NetMessage::CallJoin`](super::NetMessage::CallJoin).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallJoin {
    pub request_id: u32,
    pub group: Vec<u8>,
    pub offer: String,
    /// The media id in `offer` that carries a shared screen (protocol 41).
    /// Offered from the start, like the camera, so sharing later needs no
    /// renegotiation; `None` from a device that cannot share.
    pub screen: Option<String>,
}

/// The fields of [`NetMessage::CallKeyDrop`](super::NetMessage::CallKeyDrop).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallKeyDrop {
    pub group: Vec<u8>,
    pub participant: u64,
    pub sealed: Vec<u8>,
}

/// The fields of [`NetMessage::CallLeave`](super::NetMessage::CallLeave).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallLeave {
    pub group: Vec<u8>,
}

/// The fields of [`NetMessage::CallReact`](super::NetMessage::CallReact).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallReact {
    pub group: Vec<u8>,
    pub epoch: u64,
    pub sealed: Vec<u8>,
}

/// The fields of [`NetMessage::CallWatch`](super::NetMessage::CallWatch).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallWatch {
    pub group: Vec<u8>,
    pub cameras: Option<Vec<u64>>,
}

/// The fields of [`NetMessage::ChatSend`](super::NetMessage::ChatSend).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatSend {
    pub request_id: u32,
    pub scope: ChatScope,
    pub body: String,
}

/// The fields of [`NetMessage::ClientIntent`](super::NetMessage::ClientIntent).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClientIntent {
    pub request_id: u32,
    pub move_x: f32,
    pub move_z: f32,
    /// World yaw replicated to others (travel facing — the direction the avatar
    /// is turned toward). Server applies this instead of `atan2(velocity)`.
    pub display_yaw: f32,
    /// Avatar flight, from protocol 12 until it was removed on 2026-11-14.
    ///
    /// **Ignored by the sim, and clients send `false`.** The avatar walks,
    /// runs and jumps; an old client that still sets this is simply fallen
    /// by the sim like everyone else, and its flight-speed wish clamps at
    /// `motion::MAX_SPEED`. Kept on the wire because removing a field is a
    /// protocol revision, and these three cost three bytes.
    pub flying: bool,
    /// Climb and descend for `flying`; ignored by the sim, `false` from
    /// clients, kept for the same reason.
    pub fly_up: bool,
    pub fly_down: bool,
    /// One-shot: the client asked to jump this frame. The sim grants it only
    /// from the ground, so this is a *request*, not an impulse — a client that
    /// holds the bit down does not climb.
    pub jump: bool,
    /// One-shot: an emote the avatar started this frame, replicated to
    /// everyone in range through [`AvatarStateDto::emote`]. `None` on almost
    /// every intent.
    pub emote: Option<Emote>,
    /// One-shot: an expression the face started this frame (protocol 44),
    /// replicated as [`AvatarStateDto::expression`]; like `emote`.
    pub expression: Option<Expression>,
    /// Where the avatar is sitting, as metres from its own position — see
    /// [`AvatarStateDto::seated`] for why it is an offset. A *level*, not an
    /// edge: it is set for as long as the sit lasts, so an avatar that comes
    /// into range mid-sit is drawn sitting. The sim clears it the moment the
    /// same intent asks to move.
    pub seated: Option<[f32; 2]>,
    /// Where this client believes its avatar stands on Earth (protocol v20).
    ///
    /// Since protocol 47 the sim holds the avatar's place itself
    /// ([`AvatarStateDto::geo`]) and moves it by the intent's velocity, so
    /// this is read for one thing: **placing an avatar that has no place
    /// yet.** A connection's avatar is nowhere until its client says where
    /// it is — the first intent carrying a `geo`, or a `Teleport` — and until
    /// then it is in nobody's snapshot and in earshot of nobody, which is how
    /// a web UI that only chats stays out of the world. On a placed avatar
    /// the field is ignored: the authority is the sim's, and a client wanting
    /// to be elsewhere teleports.
    ///
    /// `None` from a client that does not know yet (before the map has
    /// anchored).
    pub geo: Option<GeoAnchor>,
}

/// The fields of [`NetMessage::CreatePrim`](super::NetMessage::CreatePrim).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreatePrim {
    pub request_id: u32,
    pub name: String,
    pub position: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
    pub color: [f32; 3],
    pub texture_id: Option<String>,
    pub shape: String,
    #[serde(default)]
    pub surface: PrimSurface,
    #[serde(default)]
    pub geometry: PrimGeometry,
    /// Absolute geo placement (ADR-029).
    pub geo: GeoAnchor,
    /// A particle emitter to author onto the prim (protocol v20).
    #[serde(default)]
    pub particles: Option<ParticleEffect>,
    /// A stored model asset to draw instead of `shape` (protocol v22).
    #[serde(default)]
    pub model: Option<String>,
    /// Make it a draft only its maker sees (protocol 51): what the design
    /// room builds, until it is placed.
    #[serde(default)]
    pub draft: bool,
}

/// The fields of [`NetMessage::DeleteAccount`](super::NetMessage::DeleteAccount).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeleteAccount {
    pub request_id: u32,
    pub password: String,
}

/// The fields of [`NetMessage::DeletePrim`](super::NetMessage::DeletePrim).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeletePrim {
    pub request_id: u32,
    pub prim_id: i64,
}

/// The fields of [`NetMessage::E2eeClaimKeyPackages`](super::NetMessage::E2eeClaimKeyPackages).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeClaimKeyPackages {
    pub request_id: u32,
    pub address: String,
}

/// The fields of [`NetMessage::E2eeCreateGroup`](super::NetMessage::E2eeCreateGroup).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeCreateGroup {
    pub request_id: u32,
    pub group: Vec<u8>,
    pub direct_with: Option<String>,
}

/// The fields of [`NetMessage::E2eeRegisterDevice`](super::NetMessage::E2eeRegisterDevice).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeRegisterDevice {
    pub request_id: u32,
    pub device: Uuid,
    pub key_packages: Vec<Vec<u8>>,
}

/// The fields of [`NetMessage::E2eeSend`](super::NetMessage::E2eeSend).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeSend {
    pub request_id: u32,
    pub group: Vec<u8>,
    pub epoch: u64,
    pub commit: bool,
    pub message: Vec<u8>,
    /// For a commit that adds people: the invitation and whom it is for.
    pub welcome: Option<E2eeWelcome>,
}

/// The fields of [`NetMessage::E2eeSync`](super::NetMessage::E2eeSync).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct E2eeSync {
    pub request_id: u32,
    pub cursors: Vec<E2eeCursor>,
}

/// The fields of [`NetMessage::FollowPlans`](super::NetMessage::FollowPlans).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FollowPlans {
    pub places: Vec<[f64; 2]>,
}

/// The fields of [`NetMessage::GetNearbyPeople`](super::NetMessage::GetNearbyPeople).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GetNearbyPeople {
    pub request_id: u32,
}

/// The fields of [`NetMessage::GetPrimScript`](super::NetMessage::GetPrimScript).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GetPrimScript {
    pub request_id: u32,
    pub prim_id: i64,
}

/// The fields of [`NetMessage::GrabPrim`](super::NetMessage::GrabPrim).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GrabPrim {
    pub request_id: u32,
    pub prim_id: i64,
}

/// The fields of [`NetMessage::LookupPerson`](super::NetMessage::LookupPerson).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LookupPerson {
    pub request_id: u32,
    pub username: Option<String>,
    pub avatar_id: Option<u64>,
}

/// The fields of [`NetMessage::OfferGreeting`](super::NetMessage::OfferGreeting).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OfferGreeting {
    pub request_id: u32,
    pub to: u64,
    pub greeting: crate::greet::Greeting,
}

/// The fields of [`NetMessage::RemoveContact`](super::NetMessage::RemoveContact).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoveContact {
    pub request_id: u32,
    pub username: String,
}

/// The fields of [`NetMessage::ReportUser`](super::NetMessage::ReportUser).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportUser {
    pub request_id: u32,
    pub username: String,
    pub reason: ReportReason,
    /// The reporter's own words. May be empty.
    pub note: String,
    /// The reported message, if the report was raised from a chat line.
    pub message: Option<String>,
    /// The model asset the report is about, if it was raised from an object
    /// rather than from a person or a chat line (protocol v22).
    ///
    /// The subject stays a username — the uploader is still who an operator
    /// may need to act on — and this rides alongside so that acting on one
    /// report reaches every prim wearing that file, rather than the single one
    /// the reporter happened to be standing in front of.
    #[serde(default)]
    pub asset: Option<String>,
}

/// The fields of [`NetMessage::RequestContact`](super::NetMessage::RequestContact).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestContact {
    pub request_id: u32,
    pub username: String,
    pub link_code: Option<String>,
}

/// The fields of [`NetMessage::SetAvatarLook`](super::NetMessage::SetAvatarLook).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetAvatarLook {
    pub look: AvatarLook,
}

/// The fields of [`NetMessage::SetBlocked`](super::NetMessage::SetBlocked).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetBlocked {
    pub request_id: u32,
    pub username: String,
    pub blocked: bool,
}

/// The fields of [`NetMessage::SetContactSettings`](super::NetMessage::SetContactSettings).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetContactSettings {
    pub request_id: u32,
    pub policy: RequestPolicy,
    pub show_online: bool,
    pub notify_requests: bool,
    pub separate_new_accounts: bool,
    pub reset_link: bool,
}

/// The fields of [`NetMessage::SetFaceFrame`](super::NetMessage::SetFaceFrame).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetFaceFrame {
    pub weights: Vec<u8>,
}

/// The fields of [`NetMessage::SetPresence`](super::NetMessage::SetPresence).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetPresence {
    pub request_id: u32,
    pub active: bool,
}

/// The fields of [`NetMessage::SetPrimPhysical`](super::NetMessage::SetPrimPhysical).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetPrimPhysical {
    pub request_id: u32,
    pub prim_id: i64,
    pub physical: bool,
}

/// The fields of [`NetMessage::SetPrimScript`](super::NetMessage::SetPrimScript).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetPrimScript {
    pub request_id: u32,
    pub prim_id: i64,
    pub source: String,
}

/// The fields of [`NetMessage::SetPrimVideo`](super::NetMessage::SetPrimVideo).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetPrimVideo {
    pub request_id: u32,
    pub prim_id: i64,
    pub video: Option<SetVideo>,
}

/// The fields of [`NetMessage::SetPushEndpoint`](super::NetMessage::SetPushEndpoint).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetPushEndpoint {
    pub endpoint: Option<String>,
}

/// The fields of [`NetMessage::Teleport`](super::NetMessage::Teleport).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Teleport {
    pub request_id: u32,
    pub geo: GeoAnchor,
    pub alt_m: f32,
}

/// The fields of [`NetMessage::TextureRequest`](super::NetMessage::TextureRequest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextureRequest {
    pub request_id: u32,
    pub texture_id: String,
}

/// The fields of [`NetMessage::TouchPrim`](super::NetMessage::TouchPrim).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TouchPrim {
    pub request_id: u32,
    pub prim_id: i64,
}

/// The fields of [`NetMessage::UpdatePrim`](super::NetMessage::UpdatePrim).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpdatePrim {
    pub request_id: u32,
    pub prim_id: i64,
    pub position: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
    pub color: [f32; 3],
    pub texture_id: Option<String>,
    pub name: String,
    /// Texture surface params (ADR-017 / protocol v8).
    #[serde(default)]
    pub surface: PrimSurface,
    /// Geometry deformation params (ADR-018 / protocol v8).
    #[serde(default)]
    pub geometry: PrimGeometry,
    /// Absolute geo placement (ADR-029).
    pub geo: GeoAnchor,
    /// The prim's particle emitter, or `None` to take it off (protocol v20).
    #[serde(default)]
    pub particles: Option<ParticleEffect>,
    /// The prim's model asset, or `None` to go back to the procedural shape
    /// (protocol v22).
    #[serde(default)]
    pub model: Option<String>,
}

/// The fields of [`NetMessage::VoiceFrame`](super::NetMessage::VoiceFrame).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceFrame {
    pub scope: ChatScope,
    pub seq: u32,
    pub payload: Vec<u8>,
}

/// The fields of [`NetMessage::WherePeople`](super::NetMessage::WherePeople).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WherePeople {
    pub request_id: u32,
    pub usernames: Vec<String>,
}

/// The fields of [`NetMessage::ReleasePrim`](super::NetMessage::ReleasePrim).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleasePrim {
    pub request_id: u32,
    pub throw: bool,
}

/// The fields of [`NetMessage::AcceptGreeting`](super::NetMessage::AcceptGreeting).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcceptGreeting {
    pub request_id: u32,
    pub from: u64,
}
