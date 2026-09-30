//! Which messages a client may send.
//!
//! [`NetMessage`] is one enum for both directions, because both ends share the
//! frame codec and one `message_kind` table. That is right for the wire and wrong
//! for a dispatcher: a server that matches on `NetMessage` has to say something
//! about `WorldSnapshot` and `ServerHelloAck`, and every new server-to-client
//! message lands in the connection loop's catch-all without anyone deciding
//! anything about it.
//!
//! [`ClientRequest`] is the other half: only the messages a client may send, and
//! the conversion from [`NetMessage`] is the **one** place that decides which
//! those are. The connection loop matches it exhaustively, so adding a
//! client-to-server message to the protocol fails to compile here until somebody
//! writes the `TryFrom` arm — and after that, fails to compile in the dispatcher
//! until somebody writes a handler. A message that arrives from a client and is
//! not one of these is the `TryFrom` error, which the loop logs and ignores,
//! exactly as the old catch-all arm did.
//!
//! This is a server-side type and changes nothing on the wire.

use cartopolis_core::{
    ChatScope, E2eeCursor, E2eeWelcome, Emote, GeoAnchor, NetMessage, ParticleEffect, PrimGeometry,
    PrimSurface, ReportReason, RequestPolicy,
};
use glam::Vec3;
use uuid::Uuid;

/// A message a client may send, with the fields the server reads.
///
/// Fields the server ignores (the `request_id` of a message that is never
/// answered) are dropped in the conversion rather than carried.
#[derive(Debug)]
pub enum ClientRequest {
    /// A second `ClientHello` on an authenticated connection.
    Hello,
    Intent {
        move_x: f32,
        move_z: f32,
        display_yaw: f32,
        flying: bool,
        fly_up: bool,
        fly_down: bool,
        jump: bool,
        emote: Option<Emote>,
        seated: Option<[f32; 2]>,
        geo: Option<GeoAnchor>,
    },
    Teleport {
        position: Vec3,
    },
    SetPresence {
        active: bool,
    },
    CreatePrim {
        request_id: u32,
        name: String,
        position: Vec3,
        rotation: Vec3,
        scale: Vec3,
        color: [f32; 3],
        texture_id: Option<String>,
        shape: String,
        surface: PrimSurface,
        geometry: PrimGeometry,
        geo: GeoAnchor,
        particles: Option<ParticleEffect>,
        model: Option<String>,
    },
    UpdatePrim {
        request_id: u32,
        prim_id: i64,
        position: Vec3,
        rotation: Vec3,
        scale: Vec3,
        color: [f32; 3],
        texture_id: Option<String>,
        name: String,
        surface: PrimSurface,
        geometry: PrimGeometry,
        geo: GeoAnchor,
        particles: Option<ParticleEffect>,
        model: Option<String>,
    },
    DeletePrim {
        request_id: u32,
        prim_id: i64,
    },
    SetPrimVideo {
        request_id: u32,
        prim_id: i64,
        video: Option<cartopolis_core::SetVideo>,
    },
    TextureCatalogRequest,
    TextureRequest {
        request_id: u32,
        texture_id: String,
    },
    ChatSend {
        scope: ChatScope,
        body: String,
    },
    ChatSubscribe {
        room: String,
        join: bool,
    },
    GetPrimScript {
        request_id: u32,
        prim_id: i64,
    },
    SetPrimScript {
        request_id: u32,
        prim_id: i64,
        source: String,
    },
    TouchPrim {
        request_id: u32,
        prim_id: i64,
    },
    VoiceFrame {
        scope: ChatScope,
        seq: u32,
        payload: Vec<u8>,
    },
    /// No `request_id`: the answer is the whole `BlockedList`, not an ack, so
    /// there is nothing to correlate.
    SetBlocked {
        username: String,
        blocked: bool,
    },
    ReportUser {
        request_id: u32,
        username: String,
        reason: ReportReason,
        note: String,
        message: Option<String>,
        asset: Option<String>,
    },
    DeleteAccount {
        request_id: u32,
        password: String,
    },
    E2eeRegisterDevice {
        request_id: u32,
        device: Uuid,
        key_packages: Vec<Vec<u8>>,
    },
    E2eeClaimKeyPackages {
        request_id: u32,
        username: String,
    },
    E2eeCreateGroup {
        request_id: u32,
        group: Vec<u8>,
        direct_with: Option<String>,
    },
    E2eeSend {
        request_id: u32,
        group: Vec<u8>,
        epoch: u64,
        commit: bool,
        message: Vec<u8>,
        welcome: Option<E2eeWelcome>,
    },
    E2eeSync {
        request_id: u32,
        cursors: Vec<E2eeCursor>,
    },
    CallJoin {
        request_id: u32,
        group: Vec<u8>,
        offer: String,
    },
    CallAnswer {
        group: Vec<u8>,
        sdp: String,
    },
    CallLeave {
        group: Vec<u8>,
    },
    RequestContact {
        request_id: u32,
        username: String,
        link_code: Option<String>,
    },
    AnswerContact {
        request_id: u32,
        username: String,
        accept: bool,
    },
    /// No `request_id`: the answer is the whole new contact state.
    RemoveContact {
        username: String,
    },
    /// No `request_id`, for the same reason as `RemoveContact`.
    SetContactSettings {
        policy: RequestPolicy,
        show_online: bool,
        notify_requests: bool,
        separate_new_accounts: bool,
        reset_link: bool,
    },
    LookupPerson {
        request_id: u32,
        username: Option<String>,
        avatar_id: Option<u64>,
    },
    GetNearbyPeople {
        request_id: u32,
    },
}

impl TryFrom<NetMessage> for ClientRequest {
    /// The message, handed back: it is one only a server sends.
    type Error = NetMessage;

    fn try_from(msg: NetMessage) -> Result<Self, NetMessage> {
        Ok(match msg {
            NetMessage::ClientHello { .. } => Self::Hello,
            NetMessage::ClientIntent {
                move_x,
                move_z,
                display_yaw,
                flying,
                fly_up,
                fly_down,
                jump,
                emote,
                seated,
                geo,
                ..
            } => Self::Intent {
                move_x,
                move_z,
                display_yaw,
                flying,
                fly_up,
                fly_down,
                jump,
                emote,
                seated,
                geo,
            },
            NetMessage::Teleport { position, .. } => Self::Teleport { position },
            NetMessage::SetPresence { active, .. } => Self::SetPresence { active },
            NetMessage::CreatePrim {
                request_id,
                name,
                position,
                rotation,
                scale,
                color,
                texture_id,
                shape,
                surface,
                geometry,
                geo,
                particles,
                model,
            } => Self::CreatePrim {
                request_id,
                name,
                position,
                rotation,
                scale,
                color,
                texture_id,
                shape,
                surface,
                geometry,
                geo,
                particles,
                model,
            },
            NetMessage::UpdatePrim {
                request_id,
                prim_id,
                position,
                rotation,
                scale,
                color,
                texture_id,
                name,
                surface,
                geometry,
                geo,
                particles,
                model,
            } => Self::UpdatePrim {
                request_id,
                prim_id,
                position,
                rotation,
                scale,
                color,
                texture_id,
                name,
                surface,
                geometry,
                geo,
                particles,
                model,
            },
            NetMessage::DeletePrim {
                request_id,
                prim_id,
            } => Self::DeletePrim {
                request_id,
                prim_id,
            },
            NetMessage::SetPrimVideo {
                request_id,
                prim_id,
                video,
            } => Self::SetPrimVideo {
                request_id,
                prim_id,
                video,
            },
            NetMessage::TextureCatalogRequest => Self::TextureCatalogRequest,
            NetMessage::TextureRequest {
                request_id,
                texture_id,
            } => Self::TextureRequest {
                request_id,
                texture_id,
            },
            NetMessage::ChatSend { scope, body, .. } => Self::ChatSend { scope, body },
            NetMessage::ChatSubscribe { room, join } => Self::ChatSubscribe { room, join },
            NetMessage::GetPrimScript {
                request_id,
                prim_id,
            } => Self::GetPrimScript {
                request_id,
                prim_id,
            },
            NetMessage::SetPrimScript {
                request_id,
                prim_id,
                source,
            } => Self::SetPrimScript {
                request_id,
                prim_id,
                source,
            },
            NetMessage::TouchPrim {
                request_id,
                prim_id,
            } => Self::TouchPrim {
                request_id,
                prim_id,
            },
            NetMessage::VoiceFrame {
                scope,
                seq,
                payload,
            } => Self::VoiceFrame {
                scope,
                seq,
                payload,
            },
            NetMessage::SetBlocked {
                username, blocked, ..
            } => Self::SetBlocked { username, blocked },
            NetMessage::ReportUser {
                request_id,
                username,
                reason,
                note,
                message,
                asset,
            } => Self::ReportUser {
                request_id,
                username,
                reason,
                note,
                message,
                asset,
            },
            NetMessage::DeleteAccount {
                request_id,
                password,
            } => Self::DeleteAccount {
                request_id,
                password,
            },
            NetMessage::E2eeRegisterDevice {
                request_id,
                device,
                key_packages,
            } => Self::E2eeRegisterDevice {
                request_id,
                device,
                key_packages,
            },
            NetMessage::E2eeClaimKeyPackages {
                request_id,
                username,
            } => Self::E2eeClaimKeyPackages {
                request_id,
                username,
            },
            NetMessage::E2eeCreateGroup {
                request_id,
                group,
                direct_with,
            } => Self::E2eeCreateGroup {
                request_id,
                group,
                direct_with,
            },
            NetMessage::E2eeSend {
                request_id,
                group,
                epoch,
                commit,
                message,
                welcome,
            } => Self::E2eeSend {
                request_id,
                group,
                epoch,
                commit,
                message,
                welcome,
            },
            NetMessage::E2eeSync {
                request_id,
                cursors,
            } => Self::E2eeSync {
                request_id,
                cursors,
            },
            NetMessage::CallJoin {
                request_id,
                group,
                offer,
            } => Self::CallJoin {
                request_id,
                group,
                offer,
            },
            NetMessage::CallAnswer { group, sdp } => Self::CallAnswer { group, sdp },
            NetMessage::CallLeave { group } => Self::CallLeave { group },
            NetMessage::RequestContact {
                request_id,
                username,
                link_code,
            } => Self::RequestContact {
                request_id,
                username,
                link_code,
            },
            NetMessage::AnswerContact {
                request_id,
                username,
                accept,
            } => Self::AnswerContact {
                request_id,
                username,
                accept,
            },
            NetMessage::RemoveContact { username, .. } => Self::RemoveContact { username },
            NetMessage::SetContactSettings {
                policy,
                show_online,
                notify_requests,
                separate_new_accounts,
                reset_link,
                ..
            } => Self::SetContactSettings {
                policy,
                show_online,
                notify_requests,
                separate_new_accounts,
                reset_link,
            },
            NetMessage::LookupPerson {
                request_id,
                username,
                avatar_id,
            } => Self::LookupPerson {
                request_id,
                username,
                avatar_id,
            },
            NetMessage::GetNearbyPeople { request_id } => Self::GetNearbyPeople { request_id },

            // Everything below is server-to-client. Listed rather than folded into
            // a `_` arm, so a new message has to be put on one side or the other
            // here before this compiles.
            NetMessage::ServerHelloAck { .. }
            | NetMessage::ServerError { .. }
            | NetMessage::WorldSnapshot { .. }
            | NetMessage::PrimRemoved { .. }
            | NetMessage::PrimUpsert { .. }
            | NetMessage::TextureCatalog { .. }
            | NetMessage::TextureData { .. }
            | NetMessage::TextureNotFound { .. }
            | NetMessage::ChatMessage { .. }
            | NetMessage::PrimScript { .. }
            | NetMessage::ScriptOutput { .. }
            | NetMessage::WorldCapacity { .. }
            | NetMessage::VoiceData { .. }
            | NetMessage::BlockedList { .. }
            | NetMessage::ReportAck { .. }
            | NetMessage::AccountDeleted
            | NetMessage::E2eeDeviceStatus { .. }
            | NetMessage::E2eeKeyPackages { .. }
            | NetMessage::E2eeGroupCreated { .. }
            | NetMessage::E2eeSent { .. }
            | NetMessage::E2eeSynced { .. }
            | NetMessage::E2eeDeliver { .. }
            | NetMessage::E2eeInvite { .. }
            | NetMessage::CallJoined { .. }
            | NetMessage::CallOffer { .. }
            | NetMessage::CallRoster { .. }
            | NetMessage::ContactsState { .. }
            | NetMessage::PersonInfo { .. }
            | NetMessage::NearbyPeople { .. } => return Err(msg),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The direction is decided in one place, and a server-only message coming
    /// from a client is handed back rather than acted on.
    #[test]
    fn only_client_messages_become_requests() {
        let touch = NetMessage::TouchPrim {
            request_id: 7,
            prim_id: 3,
        };
        assert!(matches!(
            ClientRequest::try_from(touch),
            Ok(ClientRequest::TouchPrim {
                request_id: 7,
                prim_id: 3
            })
        ));
        let echo = NetMessage::PrimRemoved { id: 3 };
        assert!(matches!(
            ClientRequest::try_from(echo),
            Err(NetMessage::PrimRemoved { id: 3 })
        ));
        assert!(matches!(
            ClientRequest::try_from(NetMessage::AccountDeleted),
            Err(NetMessage::AccountDeleted)
        ));
    }
}
