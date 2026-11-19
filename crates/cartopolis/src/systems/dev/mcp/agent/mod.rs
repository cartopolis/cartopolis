//! **The client as somebody's avatar**: what an agent driving it over MCP may
//! hear, what it may say, where it may walk and what it may build.
//!
//! The camera half of this server (`state`, `view`) photographs the world. This
//! half *lives* in it — an account signed in to a simulator, standing on a
//! street, talking to people. Every action goes out through the same channel a
//! person's does: a walk is a travel wish into `avatar::handle_avatar_movement`,
//! a line is a `ChatSend` or a sealed private message, a build is the owner-scoped
//! `/api/content/import` any signed-in account may call. So an agent can do
//! nothing here that the account could not do with a keyboard, and the server's
//! checks — ownership, rate limits, the world budget — are the ones that apply.
//!
//! # What it may hear: the allow-list
//!
//! **The risk this module is built around is text, not actions.** An agent reads
//! what arrives, and anything it reads can try to instruct it. So chat reaches it
//! only from the accounts on [`Audience`]; everybody else's lines are *counted*
//! and never shown, names included (a username is text somebody chose). The list
//! is empty by default, which means an agent hears nobody until its owner says
//! whom.
//!
//! Two things make a name trustworthy enough to filter on, and both were holes:
//!
//! - **A scripted object speaks under its own name.** `say` arrives as local
//!   chat with `sender_id == 0` and the prim's name as the sender, and anybody who
//!   builds can name a prim anything — including the one username on the list.
//!   [`ChatLine::sender_is_account`](crate::systems::chat::ChatLine::sender_is_account)
//!   is the test.
//! - **A private line's name is its sender's own claim** until it is checked
//!   against the account the server took it from, which
//!   `private_chat::Engine::on_deliver` now does for everyone, not just here.
//!
//! The filter lives in the client, not the server, on purpose. A person and their
//! agent can share one account: the person still sees every line on screen, and
//! only what the *agent* reads is narrowed. The threat is to the owner's own
//! model, and the owner's own client is on their side.
//!
//! What the filter cannot reach is a picture. A capture shows the chat overlay,
//! and the chat overlay shows everybody, so `view` hides the chrome whenever the
//! list is narrower than everyone (see `mcp::pump`). What is left in a picture —
//! a sign somebody built, a name somebody gave a prim — is the residual, and it
//! is why the session driving this should hold no other keys.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use futures_channel::oneshot;
use serde_json::{json, Value};

use super::surroundings::{self as around, Note, SavedPlace};
use super::{McpBridge, Outcome, Reply};
use crate::components::{Avatar, Prim, PrimGeo, RemoteAvatar};
use crate::resources::{
    AuthIdentity, AvatarState, CameraMode, CameraState, ConnectAddr, MapAnchor, OnlineSession,
    OsmTileUrlTemplate,
};
use crate::systems::avatar::AgentWalk;
use crate::systems::building_facts::BuildingFacts;
use crate::systems::chat::{ChatLine, ChatState};
use crate::systems::geo_nav::{self, GeoNavState, Teleport};
use crate::systems::interact::INTERACT_RADIUS;
use crate::systems::locomotion::Gait;
use crate::systems::private_chat::{self, PrivateChats};
use crate::systems::script_editor::ScriptEditorState;
use crate::systems::session::SessionState;
use crate::systems::world_places::Interactable;
use cartopolis_core::meeting::{AnswerFor, MeetingObject, Rsvp};
use cartopolis_core::permissions::Permission;
use cartopolis_core::point::Point;
use cartopolis_core::posts::{PostDto, PostThread};
use cartopolis_core::{ChatScope, CommunityListing, Emote, NetMessage};

/// Furthest an agent may *walk* in one call, in metres.
///
/// Past this a walk is minutes of an avatar trudging in a straight line through
/// whatever is in the way, and the map frame it is measured in stops being the
/// right one (`geo_nav::SHORT_JUMP_M` re-anchors at 3 km). Teleport, then walk.
const MAX_WALK_M: f64 = 2_000.0;

/// How close counts as arrived at a place, and at a person — who is not a point
/// and should not be walked into.
const ARRIVE_PLACE_M: f32 = 1.2;
const ARRIVE_PERSON_M: f32 = 2.5;

/// A walk that has not moved a metre for this long is stuck: something is in
/// the way that the route (or the straight line) does not get round.
const STUCK_AFTER_S: f64 = 5.0;

/// Sidesteps a wedged walk tries before it gives up, and how long each lasts.
const UNSTICK_TRIES: u8 = 3;
const SIDESTEP_S: f64 = 1.5;

/// How long to wait for a walking route before going straight at the goal.
const PLAN_WAIT_S: f64 = 30.0;

/// How near a route's corner counts as reached. Wider than a step, because the
/// route runs down the middle of a footpath and the avatar slides along walls.
const WAYPOINT_REACHED_M: f32 = 3.0;

/// How far outside a wall a walk to a spot inside a building ends: past the
/// avatar's collision probe (0.6 m, `docs/notes/doors-and-rooms.md`), so it is
/// a spot the avatar can actually stand on.
const CLEAR_OF_WALL_M: f32 = 0.8;

/// How far off a wall to probe for its outside. Asked of the ring, never worked
/// out from its winding: +z runs south, so winding rules come out inside-out
/// (`doors-and-rooms.md`).
const WALL_PROBE_M: f32 = 0.05;

/// Buildings within this of a walk's goal are the ones a spot outside a wall
/// must not land inside — the neighbour behind a party wall.
const NEIGHBOURS_M: f32 = 150.0;

/// A route whose footpaths end further than this from the goal says so: the
/// last stretch is a straight line across whatever is in between.
const LEAD_OUT_NOTE_M: f32 = 15.0;

/// How long to wait for a private chat to open before saying it would not.
const OPEN_CHAT_WAIT_S: f64 = 20.0;

/// How long a script save waits for the server's compile verdict.
const SCRIPT_WAIT_S: f64 = 4.0;

/// How long a build waits for its objects to be announced back. One that has
/// not arrived by then was refused (the world budget, a bad value) — the server
/// answers a refused create to the sender only, and not in a form tied to it.
const BUILD_WAIT_S: f64 = 10.0;

/// How long `request_contact` waits for the server to confirm the request.
const CONTACT_WAIT_S: f64 = 8.0;

/// How long `play_video` waits for an upload and the server's answer: a clip is
/// up to 64 MB.
const VIDEO_WAIT_S: f64 = 300.0;

/// An upload in flight: the hash, or why not.
type VideoUpload =
    crate::platform::sync::Arc<crate::platform::sync::Mutex<Option<Result<String, String>>>>;

/// How long `replace_building` waits for atlas: the upload over a phone's
/// uplink, then a compile and a republish of one cell.
const BUILDING_WAIT_S: f64 = 180.0;

const PAND_FORMAT: &str = "A BAG building number is 16 digits.";

fn valid_pand(pand: &str) -> bool {
    pand.len() == 16 && pand.bytes().all(|b| b.is_ascii_digit())
}

/// What a refused operator request means, in words: `http::fetch` keeps the
/// status and drops atlas's body.
fn operator_refusal(why: &str) -> String {
    if why.ends_with("status code 401") {
        "The server did not take this session as an operator's. Sign in again.".into()
    } else if why.ends_with("status code 403") {
        "This account may not replace buildings (it needs building_override).".into()
    } else if why.ends_with("status code 404") {
        "That building has no replacement.".into()
    } else if why.ends_with("status code 400") {
        format!("Atlas refused it ({why}): the model or a setting is not acceptable.")
    } else {
        format!("The request failed: {why}")
    }
}

/// How long `sign_in` waits for the session to come up before answering.
const SIGN_IN_WAIT_S: f64 = 30.0;

/// Longest `read_chat` may wait for a line. Ten minutes: long enough that an
/// idle conversation costs a model a handful of turns an hour, short enough that
/// no connection in between gives up on the request first.
const MAX_CHAT_WAIT_S: u32 = 600;

/// Longest line the server keeps (`simulator::net::MAX_CHAT_LEN`).
const MAX_LINE_CHARS: usize = 500;

/// Most prims one `build` call may create. The server's own ceiling is higher;
/// this one is about an agent's mistakes, which are cheaper in batches of this.
const MAX_BUILD: usize = 64;

/// Shapes the server accepts (`simulator::authoring::SHAPES`), checked here so a
/// typo is refused before anything is created rather than halfway through.
const SHAPES: &[&str] = &["box", "sphere", "cylinder", "cone", "torus"];

/// How far `sit` and `interact` look for the thing named, and walk to it.
/// Further than this is a `walk_to` the agent should choose to make.
const REACH_FOR_M: f32 = 40.0;

/// How long `find_place` waits for the geocoder.
const GEOCODE_WAIT_S: f64 = 10.0;

/// How long `edit_object` waits for the server to announce the change back.
const EDIT_WAIT_S: f64 = 6.0;

/// How long `inspect_object` waits for the server to send a script's source.
const INSPECT_WAIT_S: f64 = 4.0;

/// Following: the gap kept by default, and how far past it the person must get
/// before the avatar sets off again — without the margin it twitches after
/// every step they take.
const FOLLOW_GAP_M: f32 = 2.5;
const FOLLOW_SLACK_M: f32 = 1.5;
/// Past this the follower runs to catch up.
const FOLLOW_RUN_M: f32 = 12.0;

/// How near counts as "arrived" for `wait`'s arrival event, unless asked.
const ARRIVAL_M: f32 = 20.0;

/// How long a directory search waits for its answer. The answer is not tied
/// to the question on the wire (`Communities::directory` is simply replaced),
/// so this is also how long a search that changed nothing takes to say so.
const DIRECTORY_WAIT_S: f64 = 8.0;

/// How long a request to the HTTP API — the model library, a community's
/// posts — waits before saying the server did not answer.
const HTTP_WAIT_S: f64 = 20.0;

/// How many changes `undo` remembers in memory (edits and deletes), and how
/// many builds it keeps on disk to undo after a restart.
const MAX_UNDO: usize = 20;
const MAX_BATCHES: usize = 50;

// ---------------------------------------------------------------------------
// The allow-list
// ---------------------------------------------------------------------------

/// Whose words an agent may read. See the module note.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct Audience {
    pub names: Vec<String>,
    /// `*`: everyone. Offered because it is sometimes what somebody wants — a
    /// greeter bot in a public square — and refused by nothing, but it is the
    /// one setting under which a stranger can put words in front of the model.
    pub everyone: bool,
    /// Set by `--mcp-allow`: the flag is an instruction for this launch, and
    /// Settings shows the list without being able to change it.
    pub pinned: bool,
    /// Set by `--mcp-private-only`: the agent speaks in private chats and
    /// nowhere else. `say` without `to` is refused, and so is a script that
    /// could make one of its objects speak ([`public_speech_in`]). Here, below
    /// the agent, because an instruction in the agent's brief is only advice.
    pub private_only: bool,
}

/// The script functions that put words in public chat
/// (`cartopolis_core::script_api`, the Chat group but `log`, which prints to
/// the script console).
const SPEAKING_FNS: &[&str] = &["whisper", "say", "shout"];

/// Ways a script could reach a call by a computed name (`globalThis["s" + "ay"]`),
/// which are the only routes round naming a speaking function outright. The
/// script host hands the calls to a script as local bindings taken from one
/// object (`__api`), and leaves it no `globalThis`, `eval` or working
/// `Function`; the words are refused here anyway, so this rule does not rest
/// on the host staying that way.
const GLOBAL_ESCAPES: &[&str] = &["globalThis", "eval", "Function", "__api"];

/// The word in `source` that could make an object speak in public chat, for a
/// client that talks only privately: a speaking function, or a global escape.
/// Words in comments and strings count too: a false refusal costs a rewrite,
/// a miss costs the rule.
#[must_use]
pub fn public_speech_in(source: &str) -> Option<String> {
    source
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .find(|w| SPEAKING_FNS.contains(w) || GLOBAL_ESCAPES.contains(w))
        .map(str::to_string)
}

/// The refusal for a script [`public_speech_in`] caught.
fn speech_refusal(word: &str) -> AgentReply {
    AgentReply::refuse(format!(
        "This client speaks only in private chats (--mcp-private-only), and a script using \
         `{word}` could make the object speak in public, so it was not sent. Write it without \
         chat (whisper, say, shout) and without reaching globals by name."
    ))
}

impl Audience {
    /// `"jeroen, alice"`, or `"*"`. Separators are commas and whitespace, names
    /// are compared without case (as the server compares them), and a leading
    /// `@` is dropped because people type one.
    #[must_use]
    pub fn parse(list: &str) -> Self {
        let mut out = Self::default();
        for part in list
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(|p| p.trim().trim_start_matches('@'))
            .filter(|p| !p.is_empty())
        {
            if part == "*" {
                out.everyone = true;
            } else if !out.names.iter().any(|n| n.eq_ignore_ascii_case(part)) {
                out.names.push(part.to_string());
            }
        }
        out
    }

    #[must_use]
    pub fn hears(&self, name: &str) -> bool {
        self.everyone || self.names.iter().any(|n| n.eq_ignore_ascii_case(name))
    }

    /// The list as [`Self::parse`] reads it back.
    #[must_use]
    pub fn to_list(&self) -> String {
        if self.everyone {
            "*".into()
        } else {
            self.names.join(", ")
        }
    }

    /// One sentence for an agent: whom it can hear.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.everyone {
            "everyone (the owner allowed all accounts)".into()
        } else if self.names.is_empty() {
            "nobody — the owner has not allowed anyone yet, so every line is withheld".into()
        } else {
            self.names.join(", ")
        }
    }
}

// ---------------------------------------------------------------------------
// The requests
// ---------------------------------------------------------------------------

// The tool arguments, the request enum and the reply type live in the MCP
// server crate (`cartopolis_mcp::args`), because the server deserialises and
// builds them and may not depend on this one. Re-exported (the ones this side
// names) so everything here still names them as `agent::…`.
pub use cartopolis_mcp::args::{
    AgentReply, AgentRequest, BuildArgs, CallArgs, DesignRoomArgs, EditArgs, EmoteArgs, FaceArgs,
    FollowArgs, LookAroundArgs, OutputArgs, PlaceArgs, ReadChatArgs, RememberArgs, SavePlaceArgs,
    SayArgs, VoiceCallArgs, WalkArgs, IMAGE_KEY,
};
use cartopolis_mcp::args::{
    AnswerMeetingArgs, CalendarArgs, ChannelLine, CommunitiesArgs, CommunityArgs,
};

mod communities;
mod dispatch;
mod objects;
mod settle;
mod talk;
mod walking;
mod world;

use communities::*;
use dispatch::*;
use objects::*;
use settle::*;
use talk::*;
use walking::*;
use world::*;

/// How long `send_picture` waits for the picture to be encrypted, stored and
/// announced.
const PICTURE_SEND_WAIT_S: f64 = 120.0;

/// How long `see_picture` waits for a picture to download and open.
const PICTURE_SEE_WAIT_S: f64 = 60.0;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// What the agent is in the middle of.
#[derive(Resource, Default)]
pub struct AgentState {
    /// The last chat line `read_chat` handed over.
    cursor: u64,
    walk: Option<Walk>,
    /// How the last walk ended, for `me`.
    walk_ended: Option<String>,
    pending: Vec<Pending>,
    /// Objects this account built through here, as far as this installation
    /// knows — the server's export drops ids by design, so this is the only
    /// list of *which* prims an agent can still edit. Persisted per server and
    /// account in the Data namespace.
    built: Vec<i64>,
    /// Where [`Self::built`] was loaded from; reloaded when that changes.
    built_key: Option<String>,
    /// Each finished `build` call's objects, oldest first: what `undo` takes
    /// back. Persisted beside [`Self::built`] (`mcp/batches/…`), because a
    /// build is the change most worth undoing and the one most often made in
    /// an earlier conversation.
    batches: Vec<Vec<i64>>,
    /// Changes `undo` can take back, newest last. In memory only: an edit or a
    /// delete is undone in the conversation that made it, and the prims it
    /// would restore may have moved on since a restart.
    undo: Vec<Undo>,
    /// How this run signed in at launch, so `sign_in` can do it again after a
    /// `sign_out`. `None` for a run that was not told a server, which then has
    /// nothing to sign back in to.
    sign_in: Option<crate::systems::session::SessionRequest>,
    /// Places the agent named and notes it wrote, for whoever is signed in
    /// now, and the account they were loaded for ([`sync_keeps`]).
    places: Vec<SavedPlace>,
    notes: Vec<Note>,
    keeps_key: Option<String>,
    /// What a walk that has just ended was for, still to be done.
    due: Option<Then>,
}

impl AgentState {
    /// Whether a walk is under way, and how the last one ended: what a voice
    /// agent is told when a walk it started finishes (`mcp::voice`).
    #[must_use]
    pub fn walk_status(&self) -> (bool, Option<&str>) {
        (self.walk.is_some(), self.walk_ended.as_deref())
    }

    /// State for a run that signed in at launch with `request`.
    #[must_use]
    pub fn signing_in_with(request: crate::systems::session::SessionRequest) -> Self {
        Self {
            sign_in: Some(request),
            ..Self::default()
        }
    }

    /// Is a call still being carried out over several frames — a walk, or a
    /// line or a build waiting to be confirmed? A headless client idles only
    /// when this is false (`mcp::pace_idle`).
    ///
    /// **A call that is only listening is not work.** `read_chat` and `wait`
    /// with a timeout are how a bot sits in a conversation, for minutes at a
    /// time; counting them would keep it at the full frame rate for exactly the
    /// stretch the idle rate exists for. Half a second's delay on noticing a
    /// line is nothing beside a model's turn.
    pub fn is_working(&self) -> bool {
        self.walk.is_some()
            || self
                .pending
                .iter()
                .any(|p| !matches!(p, Pending::Chat { .. } | Pending::Wait { .. }))
    }
}

/// What a walk does when it gets there.
#[derive(Clone)]
enum Then {
    /// Sit on this.
    Sit(Interactable),
    /// Use this, as the E key does.
    Use(Target),
}

/// Something `interact` can act on: a streamed place or prop, or a prim.
#[derive(Clone)]
enum Target {
    Thing(Interactable),
    Prim { id: i64, name: String, at: Vec2 },
}

impl Target {
    fn at(&self) -> Vec2 {
        match self {
            Self::Thing(i) => i.pos,
            Self::Prim { at, .. } => *at,
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Thing(i) => &i.name,
            Self::Prim { name, .. } => name,
        }
    }
}

struct Walk {
    goal: Goal,
    gait: Gait,
    /// A walking route being solved (`navigation::solve_detached`). The avatar
    /// stands still until it lands; a place in a city is almost never reachable
    /// in a straight line, which was the first version and walked into the
    /// first building on the way.
    planning: Option<crate::systems::navigation::DetachedSolve>,
    /// The route, as `[lat, lng]`, and the point being walked towards. Empty
    /// means straight at the goal: a person, or a place no route was found to.
    path: Vec<[f64; 2]>,
    next: usize,
    /// What the planner said, for `me`.
    plan: Option<String>,
    started: f64,
    /// Where the avatar last moved a metre from, and when — the stuck test.
    /// Movement rather than closeness, because a route bends away from its
    /// goal as often as towards it.
    moved_from: Vec2,
    moved_at: f64,
    /// Sidesteps spent getting unstuck since the last corner reached, and the
    /// one in progress: which way, and until when.
    unstick: u8,
    sidestep: Option<(Vec2, f64)>,
    /// Following rather than walking up: the gap to keep. The walk does not
    /// end on arrival; it waits for the person to move off again.
    follow: Option<f32>,
    /// Following, and standing by them right now.
    holding: bool,
    /// What to do on getting there.
    then: Option<Then>,
    /// Where the walk started, on Earth: a goal inside the building the avatar
    /// is already in is left where it is.
    from: Option<(f64, f64)>,
    /// Whether the goal has been checked for being inside a building
    /// ([`walkable_goal`]). Not yet when the register around it had not loaded
    /// at the start; the route landing checks again.
    checked: bool,
    /// How arriving is worded when the goal was moved off an unwalkable spot:
    /// "at the building's door".
    arrive_as: Option<String>,
}

enum Goal {
    Place { lat: f64, lng: f64 },
    Person { name: String, sim_id: u64 },
}

enum Pending {
    /// A private line waiting for its chat to open.
    Private {
        to: String,
        text: String,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// Objects asked for over the wire, waiting to be announced back.
    Build {
        items: Vec<BuildItem>,
        /// Prims that already existed when the build was sent, so an identical
        /// object somebody made earlier is never mistaken for this one.
        before: std::collections::HashSet<i64>,
        /// Deleted objects being made again by `undo`, rather than a `build`:
        /// worded as such, and not a batch of their own for the next undo.
        /// How many of the deleted could not be copied, to say so.
        restoring: Option<usize>,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A request to the server's HTTP API (the model library, a community's
    /// posts), until it answers.
    Http {
        label: &'static str,
        /// What the answer is about, for its wording: the search, the
        /// community's name.
        about: String,
        task: bevy::tasks::Task<HttpDone>,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A directory search, until `Communities::directory` changes. The
    /// account's own communities are already written into `answer`.
    Directory {
        query: String,
        before: Vec<CommunityListing>,
        answer: AgentReply,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A `walk_to` with a wait, until the walk ends one way or another.
    Walking {
        started: f64,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A `sign_in`, until the session is up or has failed.
    SigningIn {
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A `read_chat` with a wait, until somebody on the list says something.
    Chat {
        args: ReadChatArgs,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A `call`, until this device is in the call (it then rings for the
    /// other person) or it could not start.
    Calling {
        to: String,
        /// Whether the join has been asked for — false while the direct chat
        /// is still being opened.
        asked: bool,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A `request_contact`, until the server has the request on record (or
    /// refused it).
    Contact {
        to: String,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A `play_video`: the upload, if there is one, then the server's
    /// announcement of the prim with the clip on it (or without, to stop).
    Video {
        id: i64,
        upload: Option<VideoUpload>,
        asset: Option<String>,
        looped: bool,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A building replaced or restored through the operator API, waiting for
    /// the answer.
    Building {
        pand: String,
        restore: bool,
        bytes: usize,
        slot: VideoUpload,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A script saved, waiting for the compile verdict.
    Script {
        id: i64,
        seen: usize,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A `find_place`, until the geocoder answers.
    Geocode {
        query: String,
        limit: usize,
        slot: GeocodeSlot,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A `wait`, until one of the things it names happens.
    Wait {
        events: Vec<Event>,
        base: WaitBase,
        near_m: f32,
        started: f64,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// An `edit_object`, until the server announces the object as asked.
    Edit {
        id: i64,
        want: EditWant,
        /// The object as it stood, as the `UpdatePrim` that puts it back:
        /// what `undo` sends once the edit is confirmed.
        before: NetMessage,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A `send_picture`: waiting for the private chat to open (`channel` is
    /// `None` and `bytes` still held), then for the picture's line to land in
    /// it, which `private_chat` adds only once it is stored and announced.
    Picture {
        to: String,
        channel: Option<String>,
        bytes: Option<Vec<u8>>,
        caption: String,
        /// Picture lines already in the chat when it was queued.
        before: usize,
        /// The chat's notice when it was queued, so a new one can be told apart.
        notice_before: Option<String>,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// A `see_picture`, until the picture has downloaded and opened.
    Seeing {
        picture: crate::systems::chat_pictures::PictureRef,
        what: String,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
    /// An `inspect_object` whose script was asked for, until the source
    /// arrives. The answer is written; only the script is missing.
    Inspect {
        id: i64,
        mine: bool,
        answer: AgentReply,
        until: f64,
        reply: oneshot::Sender<Reply>,
    },
}

/// A geocoder answer in flight.
type GeocodeSlot = std::sync::Arc<
    crate::platform::sync::Mutex<
        Option<Result<Vec<crate::systems::geo_nav::GeoSuggestion>, String>>,
    >,
>;

/// What `wait` can wake for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Chat,
    Arrival,
    Call,
    Contact,
    Script,
    Walk,
    Location,
    /// One of the objects named in `ids` changed or went.
    Object,
}

impl Event {
    /// What a `wait` naming nothing waits for. Not [`Self::Object`], which
    /// needs the ids it is about and is added when they are given.
    const ALL: [Self; 7] = [
        Self::Chat,
        Self::Arrival,
        Self::Call,
        Self::Contact,
        Self::Script,
        Self::Walk,
        Self::Location,
    ];

    fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "chat" => Self::Chat,
            "arrival" | "arrivals" => Self::Arrival,
            "call" | "calls" => Self::Call,
            "contact" | "contacts" => Self::Contact,
            "script" | "scripts" => Self::Script,
            "walk" => Self::Walk,
            "location" | "locations" => Self::Location,
            "object" | "objects" => Self::Object,
            _ => return None,
        })
    }
}

/// One object as `wait` watches it: everything a change to it can touch.
///
/// Its place is its geo anchor and its height the absolute world height, never
/// the `Transform`'s translation, which is `big_space` cell-local and jumps
/// whenever the render origin does (`prim-scripting.md`).
#[derive(Clone, PartialEq)]
struct ObjectLook {
    lat: f64,
    lng: f64,
    height: f64,
    rotation: Quat,
    scale: Vec3,
    color: [f32; 3],
    name: String,
    /// Console lines it has printed so far.
    printed: usize,
}

impl ObjectLook {
    /// What is different in `now`, in words, if anything that matters is.
    /// Tolerant of the last decimals: the same value round a JSON trip and a
    /// float conversion should not wake anybody.
    fn changes(&self, now: &Self) -> Vec<&'static str> {
        let mut out = Vec::new();
        let (per_lat, per_lng) = cartopolis_core::degree_scale_m(self.lat);
        let moved_m = ((now.lat - self.lat) * per_lat).hypot((now.lng - self.lng) * per_lng);
        if moved_m > 0.05 || (now.height - self.height).abs() > 0.05 {
            out.push("moved");
        }
        if self.rotation.angle_between(now.rotation) > 0.5_f32.to_radians() {
            out.push("turned");
        }
        if (now.scale - self.scale).abs().max_element() > 1e-3 {
            out.push("resized");
        }
        if self
            .color
            .iter()
            .zip(now.color)
            .any(|(a, b)| (a - b).abs() > 1e-3)
        {
            out.push("recoloured");
        }
        if now.name != self.name {
            out.push("renamed");
        }
        if now.printed > self.printed {
            out.push("printed something");
        }
        out
    }
}

/// How things stood when a `wait` began, so it wakes for what is *new*.
struct WaitBase {
    /// The objects watched for [`Event::Object`], as they were; `None` for one
    /// not in view when the wait began.
    objects: Vec<(i64, Option<ObjectLook>)>,
    /// Avatars already near.
    near: Vec<u64>,
    /// Contact requests already waiting.
    incoming: Vec<String>,
    /// Console lines from the agent's own objects already printed.
    script_lines: usize,
    /// Whether it was walking.
    walking: bool,
    /// Who was already sharing their location.
    sharing: Vec<String>,
}

/// What an edit asked for, to recognise it when the server echoes it.
struct EditWant {
    lat: f64,
    lng: f64,
    name: String,
    scale: Vec3,
}

/// One object of a build: what was asked for, and the id once it is known.
///
/// **Matched by name and place, because nothing else comes back.** The wire's
/// `CreatePrim` carries a request id, but the server announces the result as an
/// ordinary `PrimUpsert` to every client, which does not. The name and the
/// coordinates are the agent's own and arrive back exactly as sent, so together
/// with [`Pending::Build::before`] they identify the object.
#[derive(Debug)]
struct BuildItem {
    name: String,
    lat: f64,
    lng: f64,
    script: Option<String>,
    id: Option<i64>,
}

/// One change an agent made that `undo` can take back.
enum Undo {
    /// A `build` call: the objects it made.
    Build(Vec<i64>),
    /// An `edit_object`: the `UpdatePrim` that puts the object back as it was.
    Edit { id: i64, before: NetMessage },
    /// A `delete_objects`: each object's id, the `CreatePrim` that makes it
    /// again and what to recognise that by, and the ids that were not in view
    /// to copy. The server keeps nothing of a deleted prim, so this is the
    /// only copy.
    Delete {
        made: Vec<(i64, NetMessage, BuildItem)>,
        missing: Vec<i64>,
    },
}

/// What a request to the HTTP API came back with.
enum HttpDone {
    /// The library's matches, and the inventory when it was asked for.
    Models(
        Result<Vec<crate::systems::model_library::LibraryEntry>, String>,
        Option<Result<Vec<crate::systems::model_library::InventoryItem>, String>>,
    ),
    Feed(Result<Vec<PostDto>, String>),
    Thread(Result<PostThread, String>),
    /// The design room's requests (group, ungroup, place): the body, or why
    /// not, and the sentence to answer with when it worked.
    Room(Result<String, String>, String),
}

/// A JSON answer from the API, read as `T`.
fn json_of<T: serde::de::DeserializeOwned>(body: Result<String, String>) -> Result<T, String> {
    serde_json::from_str(&body?).map_err(|e| format!("unreadable answer: {e}"))
}

/// Everything the requests read and write, as one system parameter.
#[derive(SystemParam)]
pub struct AgentWorld<'w, 's> {
    chat: ResMut<'w, ChatState>,
    private: Res<'w, PrivateChats>,
    identity: Res<'w, AuthIdentity>,
    session: ResMut<'w, SessionState>,
    addr: Option<Res<'w, ConnectAddr>>,
    online: Option<Res<'w, OnlineSession>>,
    anchor: Res<'w, MapAnchor>,
    tile_url: Res<'w, OsmTileUrlTemplate>,
    avatar: ResMut<'w, AvatarState>,
    camera: ResMut<'w, CameraState>,
    nav: ResMut<'w, GeoNavState>,
    /// Mutable only so `inspect_object` can borrow the idle editor's buffer to
    /// receive a script's source: `PrimScript` lands nowhere else.
    script: ResMut<'w, ScriptEditorState>,
    remotes: Query<'w, 's, &'static RemoteAvatar>,
    bodies: Query<'w, 's, (), With<Avatar>>,
    prims: Query<'w, 's, (&'static Prim, Option<&'static PrimGeo>)>,
    calls: ResMut<'w, crate::systems::calls::Calls>,
    videos: Res<'w, crate::systems::prim_video::PrimVideos>,
    contacts: ResMut<'w, crate::systems::contacts::Contacts>,
    around: Around<'w, 's>,
    /// Calls queued for the voice agent; absent without `--voice-agent`.
    voice: Option<ResMut<'w, super::voice::VoiceCalls>>,
}

/// What the agent reads about its surroundings, as one parameter: the walk-up
/// prompt's places, the building register, the weather, each prim's
/// transform (for editing one, which needs its whole state), and the
/// communities and calendar the account's chats make up.
#[derive(SystemParam)]
pub struct Around<'w, 's> {
    places: Res<'w, crate::systems::world_places::WorldPlaces>,
    facts: Res<'w, crate::systems::building_facts::BuildingFacts>,
    weather: Option<Res<'w, crate::systems::weather::WeatherState>>,
    pictures: ResMut<'w, crate::systems::chat_pictures::ChatPictures>,
    /// Mutable for its `ops`, which the private-chat engine drains: a
    /// directory search, a join.
    communities: ResMut<'w, crate::systems::communities::Communities>,
    calendar: Res<'w, crate::systems::calendar::Calendar>,
    /// The design room: opened, closed and placed by `design_room`, and
    /// whether the person has paused the agent from it.
    room: ResMut<'w, crate::systems::design_room::DesignRoom>,
    prim_tf: Query<
        'w,
        's,
        (
            &'static Prim,
            Option<&'static big_space::prelude::GridCell>,
            &'static Transform,
        ),
    >,
}

impl AgentWorld<'_, '_> {
    /// Anchored world XZ of a lat/lng, if the map has an anchor.
    fn to_world(&self, lat: f64, lng: f64) -> Option<Vec2> {
        let a = self.anchor.geo?;
        let (x, z) = crate::grid::lat_lng_to_world(a, lat, lng);
        Some(Vec2::new(x as f32, z as f32))
    }

    /// The avatar's anchored world XZ.
    fn here(&self) -> Vec2 {
        Vec2::new(self.avatar.position.x, self.avatar.position.z)
    }

    /// Who this installation keeps something under, by kind: builds, places,
    /// notes.
    fn keep_key(&self, kind: &str) -> Option<String> {
        let user = self.identity.username.as_deref()?;
        let server = self.addr.as_ref().map(|a| a.0.as_str()).unwrap_or("");
        Some(format!(
            "mcp/{kind}/{}/{}.json",
            clean(server),
            clean(&user.to_lowercase())
        ))
    }

    /// Where the avatar stands, on Earth, if there is one and a map to measure
    /// it on.
    fn avatar_geo(&self) -> Option<(f64, f64)> {
        if self.bodies.is_empty() {
            return None;
        }
        let anchor = self.anchor.geo?;
        let p = self.avatar.position;
        Some(crate::grid::world_to_lat_lng(
            anchor,
            f64::from(p.x),
            f64::from(p.z),
        ))
    }

    /// The avatar id a username last spoke with in public, if it did. Avatars
    /// carry no names on the wire, so a chat line is the only thing that ties
    /// one to a person — and only a line the server vouches for.
    fn avatar_of(&self, name: &str) -> Option<u64> {
        self.chat
            .channels
            .iter()
            .filter(|c| !ChatState::is_private(&c.key))
            .flat_map(|c| c.lines.iter())
            .filter(|l| l.sender_id != 0 && l.sender.eq_ignore_ascii_case(name))
            .max_by_key(|l| l.seq)
            .map(|l| l.sender_id)
    }

    /// Where an avatar is now, in the same frame as `AvatarState::position`.
    fn remote_position(&self, sim_id: u64) -> Option<Vec3> {
        self.remotes
            .iter()
            .find(|r| r.sim_id == sim_id)
            .map(|r| r.net_position + r.net_velocity * r.net_age)
    }

    /// Who this installation's builds are filed under.
    fn builds_key(&self) -> Option<String> {
        let user = self.identity.username.as_deref()?;
        let server = self.addr.as_ref().map(|a| a.0.as_str()).unwrap_or("");
        Some(format!(
            "mcp/builds/{}/{}.json",
            clean(server),
            clean(&user.to_lowercase())
        ))
    }

    /// How a channel is named to an agent, and the name `say` takes back.
    ///
    /// - A community topic is `community#topic`. A topic's name is only ever
    ///   letters, digits, `-` and `_` (`cartopolis_core::topic_name`); the
    ///   community's is its owner's choice, cut short, and it is the name the
    ///   agent itself picked the community by.
    /// - A group chat is `group:` and its label **only when the agent hears
    ///   everybody in it**, because the label is whoever started it's choice of
    ///   words; otherwise `group:` and the start of its id, which nobody chose.
    fn channel_name(&self, audience: &Audience, key: &str) -> String {
        match key {
            "local" => "local".into(),
            "geo:" => "global".into(),
            k if ChatState::is_private(k) => {
                if let Some((c, t)) = self.around.communities.topic(k) {
                    let community: String = c.settings.name.trim().chars().take(40).collect();
                    return format!("{community}#{}", t.name);
                }
                match private_chat::group_of(k).and_then(|g| self.private.describe(&g)) {
                    Some((false, peer)) => format!("private:{peer}"),
                    Some((true, label))
                        if !label.trim().is_empty()
                            && everyone_heard(audience, self, k).is_ok() =>
                    {
                        format!("group:{}", label.trim())
                    }
                    _ => format!("group:{}", &k[3..k.len().min(11)]),
                }
            }
            k => k.to_string(),
        }
    }

    /// The private chat `read_chat` calls `name`, if this device is in one: a
    /// direct chat, a group or a community topic.
    fn find_channel(&self, audience: &Audience, name: &str) -> Option<String> {
        let name = name.trim();
        let mut keys: Vec<String> = self
            .chat
            .channels
            .iter()
            .map(|c| c.key.clone())
            .filter(|k| ChatState::is_private(k))
            .collect();
        // A topic nobody has written in yet may have no channel to draw.
        for c in &self.around.communities.list {
            for t in c.topics.iter().filter(|t| t.joined) {
                let key = private_chat::channel_key(&t.group);
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        keys.into_iter()
            .find(|k| self.channel_name(audience, k).eq_ignore_ascii_case(name))
    }
}

/// Whether the agent may put words into a private chat: it hears the person
/// in a direct one, and **everybody** in a group or a community topic.
///
/// The allow-list is about what the agent reads, but a line into a chat is
/// read by everyone in it, and `say` already refuses a direct message to
/// somebody off the list. So a group counts only when nobody in it is a
/// stranger. In a community topic that is almost never true unless the owner
/// allowed everyone (`*`), and that is the safe reading: a topic's members
/// change without anybody here being asked, and a bot that may talk to a
/// hundred people because one of them is on its list is not what the list
/// says. Names of the people it does not hear are counted, never listed.
fn everyone_heard(audience: &Audience, w: &AgentWorld, key: &str) -> Result<(), String> {
    if audience.everyone {
        return Ok(());
    }
    let me = w.identity.username.as_deref();
    match private_chat::group_of(key).and_then(|g| w.private.describe(&g)) {
        None => Err("That chat is not open on this device.".into()),
        Some((false, peer)) if audience.hears(&peer) => Ok(()),
        Some((false, _)) => Err(format!(
            "The person in that chat is not on the list of people this client lets you hear, \
             so you may not write to them either. You can hear: {}.",
            audience.describe()
        )),
        Some((true, _)) => {
            let others: Vec<String> = w
                .private
                .people_in(key)
                .into_iter()
                .filter(|p| !me.is_some_and(|m| p.eq_ignore_ascii_case(m)))
                .collect();
            let strangers = others.iter().filter(|p| !audience.hears(p)).count();
            if others.is_empty() {
                Err(
                    "This device cannot tell who is in that chat yet, so nothing was sent to it."
                        .into(),
                )
            } else if strangers > 0 {
                Err(format!(
                    "{strangers} of the {} other people in that chat are not on the list of \
                     people this client lets you hear, and everything said there reaches all of \
                     them, so it was not sent. You can hear: {}.",
                    others.len(),
                    audience.describe()
                ))
            } else {
                Ok(())
            }
        }
    }
}

/// Letters, digits and `-`; the same rule `private_chat::state_key` uses, for
/// the same reason (a `..` must not become a path segment).
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The systems
// ---------------------------------------------------------------------------

/// Answer every agent request that arrived this frame, and every one that was
/// waiting on something that has now happened.
pub fn serve(
    mut bridge: ResMut<McpBridge>,
    mut state: ResMut<AgentState>,
    audience: Res<Audience>,
    mut world: AgentWorld,
    real: Res<Time<Real>>,
) {
    let now = real.elapsed_secs_f64();
    sync_builds(&mut state, &world);
    sync_keeps(&mut state, &world);
    arrive_if_due(&mut state, &mut world);
    let jobs = std::mem::take(&mut bridge.agent);
    for (request, reply) in jobs {
        let label = request.label();
        let answered = handle(request, &mut state, &audience, &mut world, now, reply);
        if let Some((reply, answer)) = answered {
            bridge.note(
                label.into(),
                first_line(&answer.text),
                if answer.error {
                    Outcome::Failed
                } else {
                    Outcome::Done
                },
                now,
            );
            let _ = reply.send(Reply::Agent(Box::new(answer)));
        } else {
            bridge.note(label.into(), "working…".into(), Outcome::Working, now);
        }
    }
    settle(&mut bridge, &mut state, &audience, &mut world, now);
}

/// Steer a walk in progress: wait for its route, aim [`AgentWalk`] at the next
/// corner of it (or at the goal), keep the camera on the avatar (walking only
/// happens in Follow), and end the walk on arrival, on losing the goal, or when
/// the avatar stops moving.
#[allow(clippy::too_many_arguments)]
pub fn steer(
    mut state: ResMut<AgentState>,
    mut walk: ResMut<AgentWalk>,
    avatar: Res<AvatarState>,
    anchor: Res<MapAnchor>,
    remotes: Query<&RemoteAvatar>,
    bodies: Query<(), With<Avatar>>,
    mut camera: ResMut<CameraState>,
    bridge: Res<McpBridge>,
    facts: Res<BuildingFacts>,
    real: Res<Time<Real>>,
) {
    let now = real.elapsed_secs_f64();
    let Some(w) = state.walk.as_mut() else {
        if walk.dir != Vec3::ZERO {
            *walk = AgentWalk::default();
        }
        return;
    };
    let here = Vec2::new(avatar.position.x, avatar.position.z);
    let (target, radius) = match (&w.goal, anchor.geo) {
        (_, None) => (None, 0.0),
        (Goal::Place { lat, lng }, Some(a)) => {
            let (x, z) = crate::grid::lat_lng_to_world(a, *lat, *lng);
            (Some(Vec2::new(x as f32, z as f32)), ARRIVE_PLACE_M)
        }
        (Goal::Person { sim_id, .. }, Some(_)) => (
            remotes
                .iter()
                .find(|r| r.sim_id == *sim_id)
                .map(|r| r.net_position + r.net_velocity * r.net_age)
                .map(|p| Vec2::new(p.x, p.z)),
            ARRIVE_PERSON_M,
        ),
    };
    // A route still being solved: stand, and do not count the wait as stuck.
    if let Some(solve) = &w.planning {
        let landed = solve.lock().ok().and_then(|mut g| g.take());
        match landed {
            Some(Ok(route)) => {
                let mut plan = format!("following a {:.0} m walking route", route.distance_m);
                let mut points = route.points;
                // The register around the goal may have arrived since the walk
                // began. A goal inside a building moves to its door now, and
                // the route's lead-out (its last point is the goal itself, the
                // one before it the footpath it snapped to) goes there instead.
                if !w.checked {
                    if let Goal::Place { lat, lng } = &mut w.goal {
                        if let Some(r) = walkable_goal(&facts, (*lat, *lng), w.from) {
                            (*lat, *lng) = (r.lat, r.lng);
                            if let Some(last) = points.last_mut() {
                                *last = [r.lat, r.lng];
                            }
                            plan.push_str(&format!(
                                "; the spot is inside a building, so it ends {}",
                                r.words()
                            ));
                            w.arrive_as = Some(r.arrival().into());
                        }
                    }
                    w.checked = true;
                }
                if let [.., end, goal] = points.as_slice() {
                    let off = around::geo_step((end[0], end[1]), (goal[0], goal[1])).length();
                    if off > LEAD_OUT_NOTE_M {
                        plan.push_str(&format!(
                            "; the footpaths end {off:.0} m from the spot, and the last \
                             stretch is straight across"
                        ));
                    }
                }
                w.plan = Some(plan);
                w.path = points;
                w.next = 0;
                w.planning = None;
            }
            Some(Err(e)) => {
                w.plan = Some(format!("no walking route ({e}), so straight at it"));
                w.planning = None;
            }
            None if now - w.started > PLAN_WAIT_S => {
                w.plan = Some("the route took too long to plan, so straight at it".into());
                w.planning = None;
            }
            None => {}
        }
        w.moved_at = now;
        w.moved_from = here;
        if w.planning.is_some() {
            walk.dir = Vec3::ZERO;
            return;
        }
    }
    // The point walked towards: the next corner of the route, skipping any
    // already within reach, or the goal itself.
    let to_world = |p: &[f64; 2]| {
        anchor.geo.map(|a| {
            let (x, z) = crate::grid::lat_lng_to_world(a, p[0], p[1]);
            Vec2::new(x as f32, z as f32)
        })
    };
    while w.next + 1 < w.path.len()
        && to_world(&w.path[w.next]).is_some_and(|p| p.distance(here) < WAYPOINT_REACHED_M)
    {
        w.next += 1;
        // Progress along the route earns the sidesteps back.
        w.unstick = 0;
    }
    let aim = match w.path.get(w.next) {
        Some(p) if w.next + 1 < w.path.len() => to_world(p),
        _ => target,
    };
    let ended = match (target, aim) {
        _ if bodies.is_empty() => {
            Some("stopped: there is no avatar any more (signed out?)".to_string())
        }
        (None, _) | (_, None) => Some(match &w.goal {
            Goal::Person { name, .. } => format!("stopped: lost sight of {name}"),
            Goal::Place { .. } => "stopped: the map lost its anchor".to_string(),
        }),
        (Some(t), Some(aim)) => {
            let d = here.distance(t);
            if let Some(gap) = w.follow {
                // Following: stand by them, and set off again only once they
                // are properly away — and run if they have got far ahead.
                let hold_until = if w.holding { gap + FOLLOW_SLACK_M } else { gap };
                if d <= hold_until {
                    w.holding = true;
                    w.moved_from = here;
                    w.moved_at = now;
                    if walk.dir != Vec3::ZERO {
                        *walk = AgentWalk::default();
                    }
                    return;
                }
                w.holding = false;
                w.gait = if d > FOLLOW_RUN_M {
                    Gait::Run
                } else {
                    Gait::Walk
                };
            }
            // The step the next frame will take: the sim keeps moving the
            // avatar on the last wish until the next intent arrives, so a goal
            // closer than that is passed before this system runs again. A
            // fraction of a centimetre at a normal frame rate; metres on a
            // software renderer, which walked a bot clean past what it was
            // walking to.
            let step = w.gait.speed() * real.delta_secs().min(1.0);
            if w.follow.is_none() && d <= radius.max(step) {
                Some(match (&w.goal, &w.arrive_as) {
                    (Goal::Person { name, .. }, _) => format!("arrived next to {name}"),
                    (Goal::Place { .. }, Some(at)) => format!("arrived {at}"),
                    (Goal::Place { .. }, None) => "arrived".to_string(),
                })
            } else {
                // A capture borrows the camera, and walking only happens in
                // Follow — so the walk pauses under a `view`, and the pause is
                // not "stuck".
                if bridge.busy() || here.distance(w.moved_from) > 1.0 {
                    w.moved_from = here;
                    w.moved_at = now;
                }
                let last_leg = w.next + 1 >= w.path.len();
                let stuck = now - w.moved_at > STUCK_AFTER_S;
                if stuck && w.unstick < UNSTICK_TRIES && !(last_leg && d < 30.0) {
                    // Wedged mid-route — the second bot lost two walks this way
                    // a few metres from the start, on a hedge or a fence corner
                    // the footpath runs past. Step aside, left then right, then
                    // back and aside, and try again from there; on the second
                    // try also give up on the corner it was aiming at.
                    w.unstick += 1;
                    let ahead = (aim - here).normalize_or_zero();
                    let left = Vec2::new(ahead.y, -ahead.x);
                    let dir = match w.unstick {
                        1 => left,
                        2 => -left,
                        _ => (left - ahead).normalize_or_zero(),
                    };
                    if w.unstick == 2 && w.next + 1 < w.path.len() {
                        w.next += 1;
                    }
                    w.sidestep = Some((dir, now + SIDESTEP_S));
                    w.moved_from = here;
                    w.moved_at = now;
                }
                if let Some((dir, until)) = w.sidestep {
                    if now < until {
                        walk.dir = Vec3::new(dir.x, 0.0, dir.y);
                        walk.gait = w.gait;
                        return;
                    }
                    w.sidestep = None;
                }
                if now - w.moved_at > STUCK_AFTER_S {
                    Some(if last_leg && d < 30.0 {
                        // The route got there and only the last few metres, off
                        // the path, are blocked: the spot is inside a building
                        // or behind a bench, a tree or somebody's object.
                        format!(
                            "stopped {d:.0} m from the spot: it is inside or behind something. \
                             This is as near as it goes"
                        )
                    } else {
                        format!(
                            "stuck {d:.0} m short: something is in the way. Walk to a point \
                             beside it first, or teleport"
                        )
                    })
                } else {
                    if !bridge.busy() && camera.mode != CameraMode::Avatar {
                        camera.mode = CameraMode::Avatar;
                    }
                    let dir = (aim - here).normalize_or_zero();
                    walk.dir = Vec3::new(dir.x, 0.0, dir.y);
                    // The last few metres at a stroll, so arriving is a stop
                    // rather than a run-on.
                    walk.gait = if w.follow.is_none() && d < 4.0 {
                        Gait::Stroll
                    } else {
                        w.gait
                    };
                    None
                }
            }
        }
    };
    if let Some(how) = ended {
        // What it was walking there to do is done by `serve`, which can reach
        // the session and the seat; this system steers and nothing else.
        state.due = state.walk.take().and_then(|w| w.then);
        state.walk_ended = Some(how);
        *walk = AgentWalk::default();
    }
}

/// Do what a finished walk was for — sit, or use the thing — if it got near
/// enough. Stopped short against a bench is the ordinary way to arrive at one,
/// so it is the distance that decides, not how the walk ended.
fn arrive_if_due(state: &mut AgentState, w: &mut AgentWorld) {
    let Some(then) = state.due.take() else {
        return;
    };
    let (at, what) = match &then {
        Then::Sit(i) => (i.pos, format!("sit on the {}", i.name.to_lowercase())),
        Then::Use(t) => (t.at(), format!("use {}", t.name())),
    };
    let how = state.walk_ended.take().unwrap_or_default();
    let d = w.here().distance(at);
    state.walk_ended = Some(if d <= INTERACT_RADIUS + 0.5 {
        format!("got there and {}", arrive(&then, w))
    } else {
        format!("{how} — {d:.0} m short, too far to {what}")
    });
}

/// Load the list of built objects for whoever is signed in now.
fn sync_builds(state: &mut AgentState, world: &AgentWorld) {
    let key = world.builds_key();
    if key == state.built_key {
        return;
    }
    let load = |k: &str| {
        crate::platform::storage::read_now(crate::platform::storage::Namespace::Data, k).ready()
    };
    state.built = key
        .as_deref()
        .and_then(load)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    state.batches = key
        .as_deref()
        .map(batches_key)
        .as_deref()
        .and_then(load)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    // Another account's edits and deletes are not this one's to undo.
    state.undo.clear();
    state.built_key = key;
}

/// Where the build batches are kept: the build list's sibling, same account
/// and server, `batches` for `builds`.
fn batches_key(builds_key: &str) -> String {
    builds_key.replacen("mcp/builds/", "mcp/batches/", 1)
}

fn save_batches(state: &AgentState) {
    let Some(key) = &state.built_key else {
        return;
    };
    let bytes = serde_json::to_vec(&state.batches).unwrap_or_default();
    if let Err(e) = crate::platform::storage::write_now(
        crate::platform::storage::Namespace::Data,
        &batches_key(key),
        &bytes,
    ) {
        tracing::warn!(error = %e, "mcp: could not save the list of builds to undo");
    }
}

/// Remember a change for `undo`, forgetting the oldest past [`MAX_UNDO`].
fn push_undo(state: &mut AgentState, change: Undo) {
    state.undo.push(change);
    if state.undo.len() > MAX_UNDO {
        state.undo.remove(0);
    }
}

/// Load the saved places and notes for whoever is signed in now. Keyed by the
/// places file; the notes file sits beside it under the same account.
fn sync_keeps(state: &mut AgentState, world: &AgentWorld) {
    let key = world.keep_key("places");
    if key == state.keeps_key {
        return;
    }
    state.places = key.as_deref().map(around::load_list).unwrap_or_default();
    state.notes = world
        .keep_key("notes")
        .as_deref()
        .map(around::load_list)
        .unwrap_or_default();
    state.keeps_key = key;
}

fn save_places(state: &AgentState) {
    if let Some(key) = &state.keeps_key {
        around::save_list(key, &state.places);
    }
}

fn save_notes(state: &AgentState) {
    if let Some(key) = &state.keeps_key {
        // The notes file is the places file's sibling: same account, same
        // server, `notes` for `places`.
        around::save_list(&key.replacen("mcp/places/", "mcp/notes/", 1), &state.notes);
    }
}

fn save_builds(state: &AgentState) {
    let Some(key) = &state.built_key else {
        return;
    };
    let bytes = serde_json::to_vec(&state.built).unwrap_or_default();
    if let Err(e) =
        crate::platform::storage::write_now(crate::platform::storage::Namespace::Data, key, &bytes)
    {
        tracing::warn!(error = %e, "mcp: could not save the list of built objects");
    }
}

#[cfg(test)]
mod tests;
