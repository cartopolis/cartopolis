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

use cartopolis_core::msg;
use cartopolis_core::NetMessage;

/// A message a client may send, with the fields the server reads.
///
/// Fields the server ignores (the `request_id` of a message that is never
/// answered) are dropped in the conversion rather than carried.
#[derive(Debug)]
pub enum ClientRequest {
    /// A second `ClientHello` on an authenticated connection.
    Hello,
    Intent(msg::ClientIntent),
    Teleport(msg::Teleport),
    SetPresence(msg::SetPresence),
    SetAvatarLook(msg::SetAvatarLook),
    /// A face its camera tracks, to hand on (`look::on_face`).
    SetFaceFrame(msg::SetFaceFrame),
    CreatePrim(msg::CreatePrim),
    UpdatePrim(msg::UpdatePrim),
    DeletePrim(msg::DeletePrim),
    SetPrimVideo(msg::SetPrimVideo),
    SetPrimPhysical(msg::SetPrimPhysical),
    /// Pick up a physical prim (`carry::on_grab`).
    GrabPrim(msg::GrabPrim),
    /// Throw or put down what the avatar carries (`carry::on_release`).
    ReleasePrim(msg::ReleasePrim),
    /// Offer a handshake or a high five to another avatar (`greet::on_offer`).
    OfferGreeting(msg::OfferGreeting),
    /// Take a greeting another avatar offered (`greet::on_accept`).
    AcceptGreeting(msg::AcceptGreeting),
    SetPushEndpoint(msg::SetPushEndpoint),
    /// The spots this account follows for new plans (`plans_watch`).
    FollowPlans(msg::FollowPlans),
    TextureCatalogRequest,
    TextureRequest(msg::TextureRequest),
    ChatSend(msg::ChatSend),
    GetPrimScript(msg::GetPrimScript),
    SetPrimScript(msg::SetPrimScript),
    TouchPrim(msg::TouchPrim),
    HandPose {
        pose: cartopolis_core::HandPose,
    },
    VoiceFrame(msg::VoiceFrame),
    /// No `request_id`: the answer is the whole `BlockedList`, not an ack, so
    /// there is nothing to correlate.
    SetBlocked(msg::SetBlocked),
    ReportUser(msg::ReportUser),
    DeleteAccount(msg::DeleteAccount),
    E2eeRegisterDevice(msg::E2eeRegisterDevice),
    E2eeClaimKeyPackages(msg::E2eeClaimKeyPackages),
    E2eeCreateGroup(msg::E2eeCreateGroup),
    E2eeSend(msg::E2eeSend),
    E2eeSync(msg::E2eeSync),
    CallJoin(msg::CallJoin),
    CallControl(msg::CallControl),
    CallReact(msg::CallReact),
    CallWatch(msg::CallWatch),
    CallAnswer(msg::CallAnswer),
    CallLeave(msg::CallLeave),
    CallDecline(msg::CallDecline),
    CallInvite(msg::CallInvite),
    CallKeyDrop(msg::CallKeyDrop),
    RequestContact(msg::RequestContact),
    AnswerContact(msg::AnswerContact),
    /// No `request_id`: the answer is the whole new contact state.
    RemoveContact(msg::RemoveContact),
    /// No `request_id`, for the same reason as `RemoveContact`.
    SetContactSettings(msg::SetContactSettings),
    LookupPerson(msg::LookupPerson),
    GetNearbyPeople(msg::GetNearbyPeople),
    WherePeople(msg::WherePeople),
    /// One of the community messages (protocols 39 and 40), kept whole: there
    /// are twenty, and `net::communities::dispatch` matches them exhaustively
    /// against this same list.
    Community(NetMessage),
}

impl ClientRequest {
    /// Whether a call's guest may make this request: taking part in the call
    /// it was let into, and registering the device a call needs. Exhaustive,
    /// so a new request is refused to guests until somebody decides otherwise.
    #[must_use]
    pub fn guest_may(&self) -> bool {
        match self {
            Self::Hello
            // Only ever to say "send me no world": a guest's page draws none.
            | Self::SetPresence(msg::SetPresence { .. })
            | Self::E2eeRegisterDevice(msg::E2eeRegisterDevice { .. })
            // Its own mailbox, which nothing is sent to: the chat engine
            // syncs on every connect, and a refusal would stall it.
            | Self::E2eeSync(msg::E2eeSync { .. })
            | Self::CallJoin(msg::CallJoin { .. })
            | Self::CallControl(msg::CallControl { .. })
            | Self::CallReact(msg::CallReact { .. })
            | Self::CallWatch(msg::CallWatch { .. })
            | Self::CallAnswer(msg::CallAnswer { .. })
            | Self::CallLeave(msg::CallLeave { .. }) => true,
            Self::Intent(msg::ClientIntent { .. })
            | Self::Teleport(msg::Teleport { .. })
            | Self::CreatePrim(msg::CreatePrim { .. })
            | Self::UpdatePrim(msg::UpdatePrim { .. })
            | Self::DeletePrim(msg::DeletePrim { .. })
            | Self::SetPrimVideo(msg::SetPrimVideo { .. })
            | Self::SetPrimPhysical(msg::SetPrimPhysical { .. })
            // A guest has no avatar in the world to carry anything with.
            | Self::GrabPrim(msg::GrabPrim { .. })
            | Self::ReleasePrim(msg::ReleasePrim { .. })
            // Nor anybody to greet with.
            | Self::OfferGreeting(msg::OfferGreeting { .. })
            | Self::AcceptGreeting(msg::AcceptGreeting { .. })
            | Self::SetPushEndpoint(msg::SetPushEndpoint { .. })
            | Self::FollowPlans(msg::FollowPlans { .. })
            | Self::TextureCatalogRequest
            | Self::TextureRequest(msg::TextureRequest { .. })
            | Self::ChatSend(msg::ChatSend { .. })
            | Self::GetPrimScript(msg::GetPrimScript { .. })
            | Self::SetPrimScript(msg::SetPrimScript { .. })
            | Self::TouchPrim(msg::TouchPrim { .. })
            | Self::HandPose { .. }
            | Self::VoiceFrame(msg::VoiceFrame { .. })
            | Self::SetBlocked(msg::SetBlocked { .. })
            | Self::ReportUser(msg::ReportUser { .. })
            | Self::DeleteAccount(msg::DeleteAccount { .. })
            | Self::E2eeClaimKeyPackages(msg::E2eeClaimKeyPackages { .. })
            | Self::E2eeCreateGroup(msg::E2eeCreateGroup { .. })
            | Self::E2eeSend(msg::E2eeSend { .. })
            | Self::CallDecline(msg::CallDecline { .. })
            | Self::CallInvite(msg::CallInvite { .. })
            | Self::CallKeyDrop(msg::CallKeyDrop { .. })
            // A guest has no avatar in the world to wear one.
            | Self::SetAvatarLook(msg::SetAvatarLook { .. })
            | Self::SetFaceFrame(msg::SetFaceFrame { .. })
            | Self::RequestContact(msg::RequestContact { .. })
            | Self::AnswerContact(msg::AnswerContact { .. })
            | Self::RemoveContact(msg::RemoveContact { .. })
            | Self::SetContactSettings(msg::SetContactSettings { .. })
            | Self::LookupPerson(msg::LookupPerson { .. })
            | Self::GetNearbyPeople(msg::GetNearbyPeople { .. })
            | Self::WherePeople(msg::WherePeople { .. })
            | Self::Community(_) => false,
        }
    }
}

impl TryFrom<NetMessage> for ClientRequest {
    /// The message, handed back: it is one only a server sends.
    type Error = NetMessage;

    fn try_from(msg: NetMessage) -> Result<Self, NetMessage> {
        Ok(match msg {
            NetMessage::ClientHello { .. } => Self::Hello,
            NetMessage::ClientIntent(m) => Self::Intent(m),
            NetMessage::Teleport(m) => Self::Teleport(m),
            NetMessage::SetPresence(m) => Self::SetPresence(m),
            NetMessage::SetAvatarLook(m) => Self::SetAvatarLook(m),
            NetMessage::SetFaceFrame(m) => Self::SetFaceFrame(m),
            NetMessage::CreatePrim(m) => Self::CreatePrim(m),
            NetMessage::UpdatePrim(m) => Self::UpdatePrim(m),
            NetMessage::DeletePrim(m) => Self::DeletePrim(m),
            NetMessage::SetPrimVideo(m) => Self::SetPrimVideo(m),
            NetMessage::SetPrimPhysical(m) => Self::SetPrimPhysical(m),
            NetMessage::GrabPrim(m) => Self::GrabPrim(m),
            NetMessage::ReleasePrim(m) => Self::ReleasePrim(m),
            NetMessage::OfferGreeting(m) => Self::OfferGreeting(m),
            NetMessage::AcceptGreeting(m) => Self::AcceptGreeting(m),
            NetMessage::SetPushEndpoint(m) => Self::SetPushEndpoint(m),
            NetMessage::FollowPlans(m) => Self::FollowPlans(m),
            NetMessage::TextureCatalogRequest => Self::TextureCatalogRequest,
            NetMessage::TextureRequest(m) => Self::TextureRequest(m),
            NetMessage::ChatSend(m) => Self::ChatSend(m),
            NetMessage::GetPrimScript(m) => Self::GetPrimScript(m),
            NetMessage::SetPrimScript(m) => Self::SetPrimScript(m),
            NetMessage::TouchPrim(m) => Self::TouchPrim(m),
            NetMessage::HandPose { pose } => Self::HandPose { pose },
            NetMessage::VoiceFrame(m) => Self::VoiceFrame(m),
            NetMessage::SetBlocked(m) => Self::SetBlocked(m),
            NetMessage::ReportUser(m) => Self::ReportUser(m),
            NetMessage::DeleteAccount(m) => Self::DeleteAccount(m),
            NetMessage::E2eeRegisterDevice(m) => Self::E2eeRegisterDevice(m),
            NetMessage::E2eeClaimKeyPackages(m) => Self::E2eeClaimKeyPackages(m),
            NetMessage::E2eeCreateGroup(m) => Self::E2eeCreateGroup(m),
            NetMessage::E2eeSend(m) => Self::E2eeSend(m),
            NetMessage::E2eeSync(m) => Self::E2eeSync(m),
            NetMessage::CallJoin(m) => Self::CallJoin(m),
            NetMessage::CallControl(m) => Self::CallControl(m),
            NetMessage::CallReact(m) => Self::CallReact(m),
            NetMessage::CallWatch(m) => Self::CallWatch(m),
            NetMessage::CallAnswer(m) => Self::CallAnswer(m),
            NetMessage::CallLeave(m) => Self::CallLeave(m),
            NetMessage::CallDecline(m) => Self::CallDecline(m),
            NetMessage::CallInvite(m) => Self::CallInvite(m),
            NetMessage::CallKeyDrop(m) => Self::CallKeyDrop(m),
            NetMessage::RequestContact(m) => Self::RequestContact(m),
            NetMessage::AnswerContact(m) => Self::AnswerContact(m),
            NetMessage::RemoveContact(m) => Self::RemoveContact(m),
            NetMessage::SetContactSettings(m) => Self::SetContactSettings(m),
            NetMessage::LookupPerson(m) => Self::LookupPerson(m),
            NetMessage::GetNearbyPeople(m) => Self::GetNearbyPeople(m),
            NetMessage::WherePeople(m) => Self::WherePeople(m),
            NetMessage::CommunityList { .. }
            | NetMessage::CommunityCreate { .. }
            | NetMessage::CommunityEdit { .. }
            | NetMessage::CommunityDirectory { .. }
            | NetMessage::CommunityJoin { .. }
            | NetMessage::CommunityLeave { .. }
            | NetMessage::CommunityDecide { .. }
            | NetMessage::CommunityAddMember { .. }
            | NetMessage::CommunitySetRole { .. }
            | NetMessage::CommunityRemove { .. }
            | NetMessage::CommunityDelete { .. }
            | NetMessage::TopicCreate { .. }
            | NetMessage::TopicEdit { .. }
            | NetMessage::TopicRemoved { .. }
            | NetMessage::ModerateDelete { .. }
            | NetMessage::CommunityReport { .. }
            | NetMessage::CommunityReportResolve { .. }
            | NetMessage::CommunityInviteCreate { .. }
            | NetMessage::CommunityInviteLookup { .. }
            | NetMessage::CommunityJoinInvite { .. } => Self::Community(msg),

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
            | NetMessage::CallDeclined { .. }
            | NetMessage::CallEnded { .. }
            | NetMessage::CallReacted { .. }
            | NetMessage::CallWaiting { .. }
            | NetMessage::CallInvited { .. }
            | NetMessage::CallKeyDropped { .. }
            | NetMessage::PrimSound { .. }
            | NetMessage::PrimTone { .. }
            | NetMessage::PrimMotion { .. }
            | NetMessage::Permissions { .. }
            | NetMessage::ContactsState { .. }
            | NetMessage::PersonInfo { .. }
            | NetMessage::NearbyPeople { .. }
            | NetMessage::PeopleWhere { .. }
            | NetMessage::E2eeAddDevices { .. }
            | NetMessage::Communities { .. }
            | NetMessage::CommunityCreated { .. }
            | NetMessage::CommunityListings { .. }
            | NetMessage::CommunityInvite { .. }
            | NetMessage::TopicAddPeople { .. }
            | NetMessage::TopicRemovePeople { .. }
            | NetMessage::TopicLeft { .. }
            | NetMessage::E2eeRedacted { .. }
            | NetMessage::AvatarLookOf { .. }
            | NetMessage::FaceFrameOf { .. }
            | NetMessage::GreetingOffered { .. }
            | NetMessage::GreetingStarted { .. }
            | NetMessage::PrimOutOfReach { .. } => return Err(msg),
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
        let touch = NetMessage::TouchPrim(msg::TouchPrim {
            request_id: 7,
            prim_id: 3,
        });
        assert!(matches!(
            ClientRequest::try_from(touch),
            Ok(ClientRequest::TouchPrim(msg::TouchPrim {
                request_id: 7,
                prim_id: 3
            }))
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

    /// Greetings are a client's to send, and a guest's not; the sim's own
    /// answers are not requests at all.
    #[test]
    fn greetings_are_client_requests_guests_may_not_make() {
        let offer = ClientRequest::try_from(NetMessage::OfferGreeting(msg::OfferGreeting {
            request_id: 1,
            to: 8,
            greeting: cartopolis_core::Greeting::Handshake,
        }))
        .expect("a client request");
        assert!(matches!(
            offer,
            ClientRequest::OfferGreeting(msg::OfferGreeting { to: 8, .. })
        ));
        assert!(!offer.guest_may());
        let accept = ClientRequest::try_from(NetMessage::AcceptGreeting(msg::AcceptGreeting {
            request_id: 2,
            from: 5,
        }))
        .expect("a client request");
        assert!(matches!(
            accept,
            ClientRequest::AcceptGreeting(msg::AcceptGreeting { from: 5, .. })
        ));
        assert!(!accept.guest_may());
        assert!(ClientRequest::try_from(NetMessage::GreetingStarted {
            greeting: cartopolis_core::Greeting::HighFive,
            partner: 1,
            yaw: 0.0,
        })
        .is_err());
    }

    /// Picking up and letting go are a client's to send, and a guest's not.
    #[test]
    fn grab_and_release_are_client_requests_guests_may_not_make() {
        let grab = ClientRequest::try_from(NetMessage::GrabPrim(msg::GrabPrim {
            request_id: 4,
            prim_id: 9,
        }))
        .expect("a client request");
        assert!(matches!(
            grab,
            ClientRequest::GrabPrim(msg::GrabPrim {
                request_id: 4,
                prim_id: 9
            })
        ));
        assert!(!grab.guest_may());
        let release = ClientRequest::try_from(NetMessage::ReleasePrim(msg::ReleasePrim {
            request_id: 5,
            throw: true,
        }))
        .expect("a client request");
        assert!(matches!(
            release,
            ClientRequest::ReleasePrim(msg::ReleasePrim {
                request_id: 5,
                throw: true
            })
        ));
        assert!(!release.guest_may());
    }
}
