//! What the signed-in account is asked to do: the tool arguments the server
//! deserialises, the [`AgentRequest`] it sends down the channel, and the
//! [`AgentReply`] that comes back. The client's `mcp::agent` is what answers
//! them from the world.

use rmcp::schemars;
use serde_json::Value;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadChatArgs {
    #[schemars(
        description = "Return lines after this sequence number. Omit to get everything \
                       you have not read yet."
    )]
    pub after: Option<u64>,
    #[schemars(description = "At most this many lines, newest kept. Default 50.")]
    pub limit: Option<usize>,
    #[schemars(
        description = "If nobody you can hear has said anything new, wait up to this many \
                       seconds (at most 600) and answer the moment somebody does. This is how \
                       to stay in a conversation: call it again with a wait after every reply, \
                       and it costs nothing while it waits."
    )]
    pub wait_s: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InboxArgs {
    #[schemars(
        description = "Wait up to this many seconds (at most 600) for somebody you can hear \
                       to say something. Default 600."
    )]
    pub wait_s: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SayArgs {
    #[schemars(description = "What to say. One line, at most 500 characters.")]
    pub text: String,
    #[schemars(
        description = "Send it privately, end-to-end encrypted, to this username. Must be \
                       somebody you are allowed to hear. Omit to speak out loud."
    )]
    pub to: Option<String>,
    #[schemars(
        description = "Out loud only: 'local' (default — people within 50 m), 'shout' \
                       (300 m) or 'global' (everyone signed in)."
    )]
    pub scope: Option<String>,
}

/// `say` as the tool takes it: [`SayArgs`], or a group chat or community
/// topic by name. Split from `SayArgs` so the requests that already build one
/// (the voice agent's line to whoever asked) need not know about channels.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SayToolArgs {
    #[schemars(description = "What to say. One line, at most 500 characters.")]
    pub text: String,
    #[schemars(
        description = "Send it privately, end-to-end encrypted, to this username. Must be \
                       somebody you are allowed to hear. Omit to speak out loud."
    )]
    pub to: Option<String>,
    #[schemars(
        description = "Send it into this group chat or community topic instead, by the name \
                       `read_chat` gives it ('group:…', or 'community#topic'). Only into a \
                       chat where you are allowed to hear everybody in it."
    )]
    pub channel: Option<String>,
    #[schemars(
        description = "Out loud only: 'local' (default — people within 50 m), 'shout' \
                       (300 m) or 'global' (everyone signed in)."
    )]
    pub scope: Option<String>,
}

impl SayToolArgs {
    /// The request it is: a line into a named chat, or an ordinary `say`. A
    /// channel with `to` or `scope` as well is refused, in words: which of the
    /// two was meant cannot be guessed, and guessing wrong is speech.
    pub fn into_request(self) -> Result<AgentRequest, String> {
        match self.channel.filter(|c| !c.trim().is_empty()) {
            Some(_) if self.to.is_some() || self.scope.is_some() => Err(
                "Give `channel` on its own, or `to` / `scope` — not both: a line goes to one \
                 place."
                    .into(),
            ),
            Some(channel) => Ok(AgentRequest::SayIn(ChannelLine {
                channel,
                text: self.text,
            })),
            None => Ok(AgentRequest::Say(SayArgs {
                text: self.text,
                to: self.to,
                scope: self.scope,
            })),
        }
    }
}

/// A line for a group chat or a community topic, named as `read_chat` names it.
#[derive(Debug)]
pub struct ChannelLine {
    pub channel: String,
    pub text: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WalkArgs {
    #[schemars(description = "Latitude to walk to.")]
    pub lat: Option<f64>,
    #[schemars(description = "Longitude to walk to.")]
    pub lng: Option<f64>,
    #[schemars(description = "A place you saved with `save_place`, by its name.")]
    pub place: Option<String>,
    #[schemars(
        description = "Walk up to this person instead of to a place. They must be somebody \
                       you are allowed to hear, and they must have said something in \
                       public chat since you arrived — that is how their avatar is known."
    )]
    pub person: Option<String>,
    #[schemars(description = "'walk' (default, 1.8 m/s), 'run' (6 m/s) or 'stroll'.")]
    pub pace: Option<String>,
    #[schemars(
        description = "Wait up to this many seconds (at most 600) for the walk to end, and \
                       answer with how it ended — arrived, or stuck and where. Use this rather \
                       than calling `me` over and over while walking."
    )]
    pub wait_s: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PlaceArgs {
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    #[schemars(description = "A place you saved with `save_place`, by its name, instead.")]
    pub place: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LookAroundArgs {
    #[schemars(description = "How far to look, in metres. Default 60, at most 300.")]
    pub radius_m: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindPlaceArgs {
    #[schemars(
        description = "What to look for: a name ('Grote Markt Groningen'), an address \
                       ('Vismarkt 12, Groningen') or a kind of place with a town."
    )]
    pub query: String,
    #[schemars(description = "At most this many answers. Default 5.")]
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SavePlaceArgs {
    #[schemars(description = "What to call it: 'home', 'where Jeroen likes to meet'.")]
    pub name: String,
    #[schemars(description = "Where. Omit both to save where your avatar is standing.")]
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    #[schemars(description = "Anything worth remembering about it.")]
    pub note: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NameArgs {
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WaitArgs {
    #[schemars(
        description = "What to wait for, any of: 'chat' (somebody you can hear says \
                       something), 'arrival' (somebody comes near), 'call' (a call rings), \
                       'contact' (somebody asks to be your contact), 'script' (one of your \
                       objects prints something), 'walk' (your walk ends), 'location' (somebody \
                       starts sharing where they are with you), 'object' (one of the objects in \
                       `ids` is moved, turned, resized, recoloured, renamed, prints something \
                       or is removed). Default: all of them, and 'object' when `ids` is given."
    )]
    pub events: Option<Vec<String>>,
    #[schemars(description = "Give up after this many seconds, at most 600. Default 300.")]
    pub timeout_s: Option<u32>,
    #[schemars(description = "How near counts as an arrival, in metres. Default 20.")]
    pub near_m: Option<f32>,
    #[schemars(
        description = "For 'object': the objects to watch, by id. Any object in view, not only \
                       yours; what one prints is shown (`script_output`) only for your own."
    )]
    pub ids: Option<Vec<i64>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FollowArgs {
    #[schemars(
        description = "Who to follow. They must be somebody you can hear who has said \
                       something in public chat since you arrived."
    )]
    pub person: String,
    #[schemars(description = "How close to keep, in metres. Default 2.5.")]
    pub distance_m: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NearArgs {
    #[schemars(
        description = "Part of its name, e.g. 'bench', 'Vismarkt', or an object's name. Omit \
                       for the nearest one."
    )]
    pub name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FaceArgs {
    #[schemars(description = "Turn to face this person.")]
    pub person: Option<String>,
    #[schemars(description = "Turn to face this object, by id.")]
    pub object: Option<i64>,
    #[schemars(description = "Turn to face this place.")]
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    #[schemars(description = "Turn to this compass bearing, degrees (0 north, 90 east).")]
    pub bearing_deg: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EditArgs {
    #[schemars(description = "The object's id, as `build` or `my_builds` reported it.")]
    pub id: i64,
    #[schemars(description = "Move it here (both, or neither).")]
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    #[schemars(description = "Or nudge it this many metres east / north of where it is.")]
    pub east_m: Option<f64>,
    pub north_m: Option<f64>,
    #[schemars(description = "Metres above the ground to its centre.")]
    pub altitude: Option<f32>,
    #[schemars(description = "Rotation [x, y, z] in degrees.")]
    pub rotation: Option<[f32; 3]>,
    #[schemars(description = "Size [x, y, z] in metres.")]
    pub scale: Option<[f32; 3]>,
    #[schemars(description = "Colour [r, g, b] from 0 to 1.")]
    pub color: Option<[f32; 3]>,
    pub name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RememberArgs {
    #[schemars(description = "The note, at most 2000 characters.")]
    pub text: String,
    #[schemars(description = "Who or what it is about: a person's name, a place, a build.")]
    pub about: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RecallArgs {
    #[schemars(description = "Only notes about this.")]
    pub about: Option<String>,
    #[schemars(description = "Only notes containing this word.")]
    pub search: Option<String>,
    #[schemars(description = "At most this many, newest first. Default 30.")]
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NoteIdArgs {
    pub id: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SeePictureArgs {
    #[schemars(description = "The picture line's number, as `read_chat` or `wait` gave it.")]
    pub seq: u64,
}

/// A picture to send privately: built by the `send_picture` tool from a file
/// or from a fresh `look`.
pub struct PictureSend {
    pub to: String,
    pub bytes: Vec<u8>,
    pub caption: String,
}

impl std::fmt::Debug for PictureSend {
    // Not the bytes: a request is logged, and a megabyte of numbers is not a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PictureSend")
            .field("to", &self.to)
            .field("bytes", &self.bytes.len())
            .field("caption", &self.caption)
            .finish()
    }
}

/// Where in an [`AgentReply`]'s data an image rides, base64, for the tool to
/// lift out into an image block (`server::Tools::act`). The reply type stays
/// text and data; the one request that answers with a picture says so here.
pub const IMAGE_KEY: &str = "jpeg_base64";

/// Where `look` turns the avatar before it photographs: the same choices as
/// [`FaceArgs`], or none to look where it already faces.
#[derive(Debug, Default)]
pub struct LookPrep {
    pub face: Option<FaceArgs>,
    /// Degrees, negative looking down, as `view` takes it.
    pub pitch_deg: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EmoteArgs {
    #[schemars(description = "One of: wave, interact, roll, punch, kick, faint.")]
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BuildArgs {
    #[schemars(
        description = "The objects, each a JSON object. Place each one with either \
                       `latitude` + `longitude`, or `east_m` + `north_m` (metres from the \
                       build's origin). Other fields: `name`, `shape` (box, sphere, \
                       cylinder, cone, torus), `altitude` (metres above the ground to the \
                       object's centre), `scale` [x, y, z] in metres, `rotation` [x, y, z] \
                       in degrees, `color` [r, g, b] from 0 to 1, `script` (Luau source \
                       — see the scripting_guide tool), and `model`: a model's asset hash from \
                       `find_models`, drawn instead of the shape at its own size (`scale` then \
                       multiplies it)."
    )]
    pub prims: Vec<Value>,
    #[schemars(
        description = "Origin for `east_m`/`north_m`. Defaults to where your avatar is \
                       standing."
    )]
    pub lat: Option<f64>,
    pub lng: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScriptArgs {
    #[schemars(description = "The object's id, as `build` or `my_builds` reported it.")]
    pub id: i64,
    #[schemars(description = "Luau source. An empty string removes the script.")]
    pub source: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct VideoArgs {
    #[schemars(description = "The object's id, as `build` or `my_builds` reported it.")]
    pub id: i64,
    #[schemars(
        description = "A WebM file (VP8 picture, Opus sound if any) on the machine this client runs \
                       on, uploaded and then played. Omit, with no `asset`, to stop the video."
    )]
    pub file: Option<String>,
    #[schemars(
        description = "A clip already in the server's store, by the hash an upload answered."
    )]
    pub asset: Option<String>,
    #[schemars(description = "Start again at the end. Default true.")]
    pub looped: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IdsArgs {
    pub ids: Vec<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ContactArgs {
    #[schemars(description = "Their username.")]
    pub username: String,
    #[schemars(
        description = "A first message, sent into a private chat with them once the request \
                       is on record. Optional."
    )]
    pub message: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AnswerContactArgs {
    #[schemars(description = "The username whose request to answer.")]
    pub username: String,
    #[schemars(description = "true to accept, false to decline (a decline is silent).")]
    pub accept: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CallArgs {
    #[schemars(
        description = "Who to call, by username. Must be somebody you are allowed to hear."
    )]
    pub to: String,
    #[schemars(
        description = "Send a picture too. This client has no camera or microphone, so what \
                       it sends is a moving test pattern and a tone. Default true."
    )]
    pub video: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct VoiceCallArgs {
    #[schemars(
        description = "Who to call, by username. Anybody: they need not be on the list \
                               of people you hear."
    )]
    pub to: String,
    #[schemars(
        description = "Why you are calling, in the words of whoever asked: what to say first, \
                       what to find out or settle. The agent on the call opens with it."
    )]
    pub purpose: String,
    #[schemars(
        description = "Who asked for the call, by username: one of the people you hear. They \
                       get one line in their private chat with you when the call ends."
    )]
    pub asked_by: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IdArgs {
    pub id: i64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct OutputArgs {
    #[schemars(description = "Only this object. Omit for all of yours.")]
    pub id: Option<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReplaceBuildingArgs {
    #[schemars(description = "The building's BAG number: its 16-digit pand identificatie.")]
    pub pand: String,
    #[schemars(
        description = "The model, a GLB (binary glTF: +Y up, the front toward −Z, one unit a \
                       metre), base64-encoded. One flat colour per part; textures are dropped."
    )]
    pub glb_base64: String,
    #[schemars(description = "What the building is called. Omit to keep the current name.")]
    pub name: Option<String>,
    #[schemars(
        description = "Where the model's footprint centre stands. Give both or neither; \
                       neither puts it on the centre of the building's outline."
    )]
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    #[schemars(description = "Degrees clockwise from north the model's front faces. Default 0.")]
    pub heading_deg: Option<f64>,
    #[schemars(
        description = "What stops a walker: `walls` (parts named wall, column or pillar; the \
                       default), `parts` (every part) or `none`."
    )]
    pub collision: Option<String>,
    #[schemars(
        description = "`none` (the model is the whole building; the default) or `design` \
                       (keep the building's rooms inside it)."
    )]
    pub interior: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PandArgs {
    #[schemars(description = "The building's BAG number: its 16-digit pand identificatie.")]
    pub pand: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindModelsArgs {
    #[schemars(
        description = "Words in the model's name or tags: 'bench', 'lamp post', 'tree'. Omit to \
                       list the whole library."
    )]
    pub query: Option<String>,
    #[schemars(description = "Only this category, as an earlier answer named it.")]
    pub category: Option<String>,
    #[schemars(
        description = "Also list the models in your own inventory: the ones this account \
                       uploaded or took from the library. Default false."
    )]
    pub mine: Option<bool>,
    #[schemars(description = "At most this many from the library. Default 20.")]
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CommunitiesArgs {
    #[schemars(
        description = "Also search the server's directory of communities for this: a name, a \
                       word, a town. Omit to list only the ones you are in."
    )]
    pub query: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CommunityArgs {
    #[schemars(description = "The community, by its name or id as `communities` gave it.")]
    pub community: String,
    #[schemars(
        description = "To join one a moderator confirms: a line to the moderators. Optional."
    )]
    pub message: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadPostsArgs {
    #[schemars(description = "The community, by its name or id as `communities` gave it.")]
    pub community: String,
    #[schemars(
        description = "Only posts written before this time, Unix milliseconds: the `created_ms` \
                       of the oldest post you have, for the page before it. Omit for the newest."
    )]
    pub before: Option<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PostIdArgs {
    #[schemars(description = "The post's id, as `read_posts` gave it.")]
    pub id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CalendarArgs {
    #[schemars(description = "How many days ahead, from now. Default 7, at most 60.")]
    pub days: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AnswerMeetingArgs {
    #[schemars(description = "The meeting's plan id, as `calendar` gave it.")]
    pub plan: String,
    #[schemars(description = "'going', 'maybe' or 'no'.")]
    pub answer: String,
    #[schemars(
        description = "For a repeating meeting: which date, as the `start_ms` `calendar` gave \
                       it. Default the next one."
    )]
    pub date_ms: Option<u64>,
    #[schemars(description = "For a repeating meeting: answer for every date. Default false.")]
    pub every: Option<bool>,
}

#[derive(Debug)]
pub enum AgentRequest {
    Me,
    ReadChat(ReadChatArgs),
    Say(SayArgs),
    Walk(WalkArgs),
    Stop,
    Teleport(PlaceArgs),
    Emote(EmoteArgs),
    Build(BuildArgs),
    SetScript(ScriptArgs),
    PlayVideo(VideoArgs),
    /// Replace a building's outside with a model, through the operator API.
    ReplaceBuilding(ReplaceBuildingArgs),
    /// Delete that replacement: the survey's building comes back.
    RestoreBuilding(PandArgs),
    Delete(IdsArgs),
    Touch(IdArgs),
    ScriptOutput(OutputArgs),
    MyBuilds,
    SignOut,
    SignIn,
    Call(CallArgs),
    AnswerCall,
    DeclineCall,
    HangUp,
    CallStatus,
    VoiceCall(VoiceCallArgs),
    RequestContact(ContactArgs),
    AnswerContact(AnswerContactArgs),
    Contacts,
    LookAround(LookAroundArgs),
    FindPlace(FindPlaceArgs),
    SavePlace(SavePlaceArgs),
    MyPlaces,
    ForgetPlace(NameArgs),
    Wait(WaitArgs),
    Follow(FollowArgs),
    Sit(NearArgs),
    Stand,
    Interact(NearArgs),
    Face(FaceArgs),
    /// The first half of `look`: turn, and put the camera in the avatar's eyes.
    PrepareLook(LookPrep),
    /// The last half: take the camera back out of the head.
    EndLook,
    Edit(EditArgs),
    Remember(RememberArgs),
    Recall(RecallArgs),
    Forget(NoteIdArgs),
    SendPicture(PictureSend),
    SeePicture(SeePictureArgs),
    /// One object in full, its script when the account may see it.
    Inspect(IdArgs),
    /// A line into a group chat or a community topic.
    SayIn(ChannelLine),
    /// Search the shared model library, and the account's own inventory.
    FindModels(FindModelsArgs),
    /// The account's communities, and a directory search.
    Communities(CommunitiesArgs),
    JoinCommunity(CommunityArgs),
    /// A community's public posts, through the HTTP API.
    ReadPosts(ReadPostsArgs),
    ReadPost(PostIdArgs),
    Calendar(CalendarArgs),
    AnswerMeeting(AnswerMeetingArgs),
    /// Take back the agent's last build, edit or delete.
    Undo,
}

impl AgentRequest {
    /// The activity overlay's label for it.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Me => "me",
            Self::ReadChat(_) => "read chat",
            Self::Say(_) => "say",
            Self::Walk(_) => "walk",
            Self::Stop => "stop",
            Self::Teleport(_) => "teleport",
            Self::Emote(_) => "emote",
            Self::Build(_) => "build",
            Self::SetScript(_) => "script",
            Self::PlayVideo(_) => "play video",
            Self::ReplaceBuilding(_) => "replace building",
            Self::RestoreBuilding(_) => "restore building",
            Self::Delete(_) => "delete",
            Self::Touch(_) => "touch",
            Self::ScriptOutput(_) => "script output",
            Self::MyBuilds => "my builds",
            Self::SignOut => "sign out",
            Self::SignIn => "sign in",
            Self::Call(_) => "call",
            Self::AnswerCall => "answer call",
            Self::DeclineCall => "decline call",
            Self::HangUp => "hang up",
            Self::CallStatus => "call status",
            Self::VoiceCall(_) => "voice call",
            Self::RequestContact(_) => "request contact",
            Self::AnswerContact(_) => "answer contact",
            Self::Contacts => "contacts",
            Self::LookAround(_) => "look around",
            Self::FindPlace(_) => "find place",
            Self::SavePlace(_) => "save place",
            Self::MyPlaces => "my places",
            Self::ForgetPlace(_) => "forget place",
            Self::Wait(_) => "wait",
            Self::Follow(_) => "follow",
            Self::Sit(_) => "sit",
            Self::Stand => "stand",
            Self::Interact(_) => "interact",
            Self::Face(_) => "face",
            Self::PrepareLook(_) | Self::EndLook => "look",
            Self::Edit(_) => "edit object",
            Self::Remember(_) => "remember",
            Self::Recall(_) => "recall",
            Self::Forget(_) => "forget",
            Self::SendPicture(_) => "send picture",
            Self::SeePicture(_) => "see picture",
            Self::Inspect(_) => "inspect object",
            Self::SayIn(_) => "say",
            Self::FindModels(_) => "find models",
            Self::Communities(_) => "communities",
            Self::JoinCommunity(_) => "join community",
            Self::ReadPosts(_) => "read posts",
            Self::ReadPost(_) => "read post",
            Self::Calendar(_) => "calendar",
            Self::AnswerMeeting(_) => "answer meeting",
            Self::Undo => "undo",
        }
    }
}

/// An answer: a sentence for the model to read, the same facts as data, and
/// whether the thing asked for did not happen.
pub struct AgentReply {
    pub text: String,
    pub data: Value,
    pub error: bool,
}

impl AgentReply {
    pub fn ok(text: impl Into<String>, data: Value) -> Self {
        Self {
            text: text.into(),
            data,
            error: false,
        }
    }

    pub fn refuse(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            data: Value::Null,
            error: true,
        }
    }
}
