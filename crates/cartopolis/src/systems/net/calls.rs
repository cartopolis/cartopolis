//! Calls in private chats: the client half of protocol 26 (`docs/notes/calls.md`).
//!
//! ```text
//! AudioInput ─→ resample 16 kHz ─→ 20 ms frames ─→ Session thread ─→ UDP ─→ forwarder
//!                                                  (Opus, SFrame, str0m)
//! forwarder ─→ UDP ─→ Session thread ─→ PCM per person ─→ ring ─→ VoiceStream ─→ the mix
//! ```
//!
//! Everything between the microphone and the network is
//! `cartopolis_calls::session`, on a thread of its own; this module is the
//! Bevy side of it: carrying its signalling over the simulator connection,
//! handing it the chat's keys, feeding it the microphone and playing what it
//! hears through the output proximity voice already uses
//! ([`crate::systems::voice::VoiceStream`]).
//!
//! **Who is in a call is the server's `CallRoster`**, sent to every connected
//! device of the chat whenever it changes. A roster with people in it and not
//! this account is a call ringing; an empty one is a call over. The UI reads
//! [`Calls::ringing`] and [`Calls::active_group`] and writes
//! [`Calls::request`], so no panel needs a handle on a thread or a socket.
//!
//! Proximity voice is switched off for the length of a call: both would open
//! the microphone, and nobody wants to be heard by passers-by while on a call.
//!
//! **In the browser the media is the browser's own** (`platform::web_call`):
//! its WebRTC, microphone, camera and codecs, with every frame sealed and
//! opened in an encoded-transform worker by the same SFrame as here. This module
//! carries its signalling and keys the same way and tells it about mute and
//! the camera rather than feeding it samples.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use cartopolis_core::{
    CallAction, CallEndReason, CallMember, CallRooms, CallSettings, CallWaiter, NetMessage,
    CALL_CONTROL_REQUEST,
};

#[cfg(not(target_arch = "wasm32"))]
use crate::platform::audio::AudioInput;
#[cfg(not(target_arch = "wasm32"))]
use crate::platform::camera::Camera;
use crate::resources::{AppPrefs, AuthIdentity, ConnectAddr, OnlineSession};
use crate::systems::private_chat::PrivateChats;
#[cfg(not(target_arch = "wasm32"))]
use crate::systems::voice::Resampler;
use crate::systems::voice::{self, VoiceMode, VoiceState};

/// Request ids for call traffic, so a refusal (`ServerError`) finds its way
/// here rather than to the prim editor, which deselects on a 403.
const REQUEST_BASE: u32 = 0xCA11_0000;
const JOIN_REQUEST: u32 = REQUEST_BASE | 1;

/// Whether a request id is a call's.
#[must_use]
pub fn is_call_request(request_id: u32) -> bool {
    request_id & 0xFFFF_0000 == REQUEST_BASE
}

/// The PCM rate a call runs at on this client: proximity voice's, so the same
/// resampler and the same playback stream serve both.
#[cfg(not(target_arch = "wasm32"))]
const RATE: u32 = voice::VOICE_RATE;

/// The key [`Calls::pictures_mut`] files this device's own camera under: no
/// participant number is this large.
pub const SELF_TILE: u64 = u64::MAX;

/// Call messages the network system collected this frame, for [`drive_calls`].
#[derive(Resource, Default)]
pub struct CallInbox(pub Vec<NetMessage>);

impl CallInbox {
    /// Whether a message belongs here rather than to the network system.
    #[must_use]
    pub fn wants(msg: &NetMessage) -> bool {
        match msg {
            NetMessage::CallJoined { .. }
            | NetMessage::CallOffer { .. }
            | NetMessage::CallRoster { .. }
            | NetMessage::CallDeclined { .. }
            | NetMessage::CallEnded { .. }
            | NetMessage::CallReacted { .. }
            | NetMessage::CallWaiting { .. }
            | NetMessage::CallInvited { .. } => true,
            NetMessage::ServerError { request_id, .. } => is_call_request(*request_id),
            _ => false,
        }
    }
}

/// What the UI asks of the call system this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallRequest {
    /// Start, or join, the call of the chat with this group id.
    Join(Vec<u8>),
    /// Hang up.
    Leave,
    /// Stop showing this chat's ringing call.
    Dismiss(Vec<u8>),
    /// A hand, a shared screen, or a host's action in this device's call
    /// (protocol 41).
    Control(CallAction),
    /// React with this emoji, for everyone in the call.
    React(String),
    /// A host moves the agenda of the plan with this id to an item, or stops
    /// it (`None`); sent sealed to everyone, as the web does.
    Agenda(String, Option<usize>),
    /// Share this device's screen, or stop (`platform::screen`).
    Share(bool),
    /// A host makes a guest link to this call (protocol 41): the server makes
    /// the code, this device the secret after the `#`.
    InviteGuest,
}

/// A guest link's address: the web page a guest opens, which handles `meet`.
/// Not the `/go` path, which the app claims on Android: a guest has no app.
#[must_use]
pub fn guest_link(code: &str, secret: &[u8]) -> String {
    format!(
        "{}/?meet={code}#{}",
        crate::systems::place_link::WEB_ORIGIN,
        b64url(secret)
    )
}

/// A guest link's code and secret, from a link or a chat line that holds one
/// (the web's `readGuestLink`).
#[must_use]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub fn read_guest_link(text: &str) -> Option<(String, Vec<u8>)> {
    let at = text.find("meet=")? + 5;
    let code: String = text[at..].chars().take(10).collect();
    if code.len() != 10
        || !code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return None;
    }
    let rest = text[at + 10..].strip_prefix('#')?;
    let secret: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (secret.len() == 22)
        .then(|| unb64url(&secret))
        .flatten()
        .map(|s| (code, s))
}

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Base64url without padding: what a URL's fragment carries, and the browser's
/// `btoa` with `+/` swapped and `=` dropped.
fn b64url(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, b| (n << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(B64URL[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn unb64url(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = B64URL.iter().position(|b| *b == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Where a meeting is on its plan's agenda, as a host's device last said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgendaAt {
    pub plan: String,
    pub index: usize,
    /// When the item started, Unix ms.
    pub started_ms: u64,
}

/// A meeting this device should write the record of
/// (`cartopolis_core::meeting::Record`): handed to `private_chat`, which
/// holds the chat's lines the record is gathered from.
#[derive(Debug, Clone)]
pub struct RecordDue {
    pub group: Vec<u8>,
    pub start_ms: u64,
    pub end_ms: u64,
    pub people: Vec<String>,
    pub transcript: Vec<(String, String)>,
}

/// Tell everybody in the call where the agenda is, sealed like a reaction:
/// `{"g": item, "p": plan, "t": started}`, or `g: -1` for stopped — the web's
/// shape (`call.ts`). Only a host's is believed.
fn send_agenda(
    calls: &Calls,
    private: &PrivateChats,
    online: &OnlineSession,
    plan: &str,
    at: Option<&AgendaAt>,
) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(group) = calls.active_group().map(<[u8]>::to_vec) {
        if let Some(e) = private.call_epoch(&group) {
            let text = match at {
                Some(at) => serde_json::json!({"g": at.index, "p": at.plan, "t": at.started_ms}),
                None => serde_json::json!({"g": -1, "p": plan, "t": 0}),
            }
            .to_string();
            let sealed = cartopolis_calls::events::seal(&e.base_key, e.epoch, text.as_bytes());
            online.send(
                "call agenda",
                NetMessage::CallReact {
                    group,
                    epoch: e.epoch,
                    sealed,
                },
            );
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (calls, private, online, plan, at);
}

/// Unix milliseconds now.
fn unix_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The rest of a call's meeting state, from its roster (protocol 41).
#[derive(Debug, Clone, Default)]
pub struct MeetingExtras {
    pub waiting: Vec<CallWaiter>,
    pub spotlight: Option<u64>,
    pub rooms: Option<CallRooms>,
}

/// Somebody's reaction, shown over their tile for [`REACTION_SECS`].
#[derive(Debug, Clone)]
pub struct Reaction {
    /// Who, or [`SELF_TILE`].
    pub participant: u64,
    pub emoji: String,
    pub at: web_time::Instant,
}

/// How long a reaction floats over its tile.
pub const REACTION_SECS: f32 = 4.0;

/// The call this device is in.
struct Active {
    group: Vec<u8>,
    /// Our number in the call, once the server has answered the join.
    participant: Option<u64>,
    #[cfg(not(target_arch = "wasm32"))]
    session: cartopolis_calls::session::Session,
    /// The browser's own call (`platform::web_call`): its WebRTC, microphone,
    /// camera and codecs, with our encryption on every frame.
    #[cfg(target_arch = "wasm32")]
    web: crate::platform::web_call::WebSession,
    /// What the browser's camera was last told.
    #[cfg(target_arch = "wasm32")]
    web_camera: bool,
    connected: bool,
    /// The connection stopped answering and has not come back yet.
    reconnecting: bool,
    /// The chat epoch last handed to the session.
    epoch: Option<u64>,
    started: web_time::Instant,
    /// When somebody else was first in the call with this device connected:
    /// what the timer counts from. Not `started`, or a call that rang for
    /// twenty seconds would show 00:20 the moment it was answered.
    answered_at: Option<web_time::Instant>,
    /// The same moment as Unix milliseconds, for the meeting record.
    answered_ms: Option<u64>,
    /// Everybody who was in the call while this device was, for the record.
    seen_people: HashSet<String>,
    #[cfg(not(target_arch = "wasm32"))]
    mic: Option<AudioInput>,
    #[cfg(not(target_arch = "wasm32"))]
    resampler: Resampler,
    /// Noise suppression on the microphone (`cartopolis_calls::denoise`).
    #[cfg(not(target_arch = "wasm32"))]
    denoiser: cartopolis_calls::denoise::Denoiser,
    #[cfg(not(target_arch = "wasm32"))]
    capture: Vec<i16>,
    #[cfg(not(target_arch = "wasm32"))]
    pending: Vec<i16>,
    /// Who is heard, by participant: the ring their audio goes into.
    speakers: HashMap<u64, Speaker>,
    /// The camera, while it is on.
    #[cfg(not(target_arch = "wasm32"))]
    camera: Option<Camera>,
    /// The screen this device shares, while it does (`platform::screen`).
    #[cfg(not(target_arch = "wasm32"))]
    screen: Option<crate::platform::screen::ScreenCapture>,
    /// The roster has said this device shares: a later roster saying it
    /// does not is a host stopping it.
    sharing_seen: bool,
    /// The newest picture from each person seen, by participant.
    pictures: HashMap<u64, Picture>,
    /// The newest picture of each shared screen, by participant.
    screens: HashMap<u64, Picture>,
    /// Whether this device's sound was held by a host at the last roster.
    held: bool,
    /// What the call was last told about this device's microphone.
    told_muted: Option<bool>,
    /// Whether the forwarder was last told this device draws cameras: none
    /// while the call is folded to its bar (`CallWatch`).
    told_looking: Option<bool>,
    /// Waiting for a host to let it in (`CallWaiting`).
    waiting: bool,
    /// The stand-in microphone's frame count (`CARTO_FAKE_MIC`).
    #[cfg(not(target_arch = "wasm32"))]
    fake_mic_sent: u64,
    /// Whether anybody else has been in the call yet: until they have, it is
    /// still ringing on the other side.
    others_seen: bool,
    /// Received so far, per participant: (audio frames, pictures). Logged
    /// every few seconds, which is what a headless test reads.
    received: HashMap<u64, (u64, u64)>,
    last_report: web_time::Instant,
}

/// The newest picture from somebody in the call, ready to draw.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Arrived since the UI last uploaded it.
    pub fresh: bool,
    /// When it arrived: a person whose pictures stop (they turned their
    /// camera off) goes back to their initial rather than freezing.
    pub at: web_time::Instant,
}

/// How long somebody else's last picture stays up with nothing after it. Not
/// shorter: a slow sender (a software renderer manages about one picture a
/// second) would flicker between picture and initial.
const PICTURE_STALE: std::time::Duration = std::time::Duration::from_secs(3);

/// One person heard in the call.
struct Speaker {
    ring: voice::Ring,
    entity: Option<Entity>,
    /// When they were last heard above [`SPEAKING_RMS`]: what lights their tile.
    heard_at: Option<web_time::Instant>,
}

/// A frame this loud (RMS, full scale 1) counts as speaking.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const SPEAKING_RMS: f32 = 0.02;
/// How long a tile stays lit after the last loud frame, so it does not
/// flicker between words.
const SPEAKING_HOLD_MS: u128 = 400;

/// How long a call you placed rings before it gives up, as a phone's does.
const NO_ANSWER: std::time::Duration = std::time::Duration::from_secs(60);

/// Every call this device knows about, and the one it is in.
#[derive(Resource, Default)]
pub struct Calls {
    /// Who is in each chat's call, by group id, as the server last said.
    rosters: HashMap<Vec<u8>, Vec<CallMember>>,
    /// What each call's hosts allow, from the same rosters.
    settings: HashMap<Vec<u8>, CallSettings>,
    /// Who waits, who is in the spotlight and which rooms are open, the same way.
    extras: HashMap<Vec<u8>, MeetingExtras>,
    /// Reactions in this device's call, newest last; dropped after
    /// [`REACTION_SECS`].
    pub reactions: Vec<Reaction>,
    /// The newest line of captions per person, and when it came: made on
    /// each speaker's own device and sent sealed (protocol 41).
    pub captions: HashMap<u64, (String, web_time::Instant)>,
    /// Finished lines of captions, (who, what), for the meeting record.
    pub transcript: Vec<(String, String)>,
    /// Where this device's meeting is on its agenda, if a host started it.
    pub agenda: Option<AgendaAt>,
    /// Notes taken in the call screen, to go into the call's chat: drained
    /// by `private_chat`, which seals and sends them.
    pub meeting_out: Vec<(Vec<u8>, cartopolis_core::meeting::MeetingObject)>,
    /// A meeting's record to write, drained by `private_chat`.
    pub record_due: Option<RecordDue>,
    /// The guest link asked for and not answered yet: the call and its secret.
    invite_secret: Option<(Vec<u8>, Vec<u8>)>,
    /// The newest guest link made here, to show and copy.
    pub invited: Option<String>,
    /// A guest link to post into its chat, sealed, so every member's device
    /// can hand the guest the key: drained by `private_chat`.
    pub link_to_post: Option<(Vec<u8>, String)>,
    /// Guest links' secrets by chat: made here, or read from the chat.
    pub guest_links: HashMap<Vec<u8>, Vec<u8>>,
    /// Hosting: the epoch each guest in the call was last handed, by
    /// participant. Cleared with the call.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub keys_dropped: HashMap<u64, u64>,
    active: Option<Active>,
    /// Set by the UI; taken by [`drive_calls`].
    pub request: Option<CallRequest>,
    /// Whether the microphone is muted. Kept across calls: somebody who
    /// muted last time is not surprised by an open microphone this time.
    pub muted: bool,
    /// Something to tell the person, once.
    pub notice: Option<String>,
    /// Ringing calls the person dismissed; forgotten when the call ends.
    dismissed: HashSet<Vec<u8>>,
    /// The microphone's level while in a call, for the in-call bar's meter.
    pub level: f32,
    /// Whether this device sends its camera. Off until asked for: a call
    /// starts as a voice call, and a camera switching itself on is the one
    /// surprise a person does not forgive.
    pub camera_on: bool,
    /// Send the stand-in camera and microphone instead of the devices: for a
    /// client nobody sits at, such as an agent placing a call over MCP. The
    /// environment switches (`CARTO_FAKE_CAMERA`, `CARTO_FAKE_MIC`) do the same
    /// for a whole run.
    pub fake_media: bool,
    /// A voice agent speaks for this device (`mcp::voice`): no microphone and
    /// no stand-in tone, since the agent sends its own frames from its own
    /// thread ([`Calls::agent_audio`]), and no camera.
    pub agent_voice: bool,
    /// The call screen is folded down to the bar at the top (Back, or its own
    /// arrow), so the chats and the world can be used during a call. Unfolded
    /// again by a new call ringing or by joining one.
    pub minimized: bool,
    /// Noise suppression is off for this call: the microphone goes out as it
    /// hears (music, a choir). On is the default, so this is the exception.
    pub keep_noise: bool,
    /// Whether the full-screen call was drawn last frame, for the Back/Escape
    /// ladder (`dismiss::Rung::CallScreen`), which cannot work out ringing
    /// without the account name.
    pub screen_shown: bool,
    /// A scripted capture's stand-in call (`--ui call…`, [`Calls::seed_demo`]):
    /// kept while offline, where a real call is dropped.
    pub demo: bool,
}

/// What the UI shows about a chat's call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ringing {
    pub group: Vec<u8>,
    /// Who is in it, by name.
    pub who: Vec<String>,
}

impl Calls {
    /// Whether the full-screen call is up: see [`Calls::screen_shown`].
    #[must_use]
    pub fn screen_up(&self) -> bool {
        self.screen_shown
    }

    /// The chat whose call this device is in.
    #[must_use]
    pub fn active_group(&self) -> Option<&[u8]> {
        self.active.as_ref().map(|a| a.group.as_slice())
    }

    /// Whether this device's call is carrying audio yet.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.active.as_ref().is_some_and(|a| a.connected)
    }

    /// Whether somebody else has been in this device's call with media
    /// flowing: it was answered, and is no longer ringing on the other side.
    #[must_use]
    pub fn answered(&self) -> bool {
        self.active.as_ref().is_some_and(|a| a.answered_at.is_some())
    }

    /// A way to speak into this device's call from another thread, and to
    /// hear everybody in it there: what a voice agent talks through
    /// (`mcp::voice`). Each call hands out a fresh tap.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn agent_audio(
        &self,
    ) -> Option<(
        cartopolis_calls::session::PcmSink,
        std::sync::mpsc::Receiver<(u64, Vec<f32>)>,
    )> {
        let a = self.active.as_ref()?;
        Some((a.session.pcm_sink(), a.session.tap_audio()))
    }

    /// How long this device's call has been answered: zero while it rings.
    #[must_use]
    pub fn elapsed_secs(&self) -> u64 {
        self.active
            .as_ref()
            .and_then(|a| a.answered_at)
            .map_or(0, |t| t.elapsed().as_secs())
    }

    /// What to say when this device's call ends: how long it lasted, as a
    /// phone does, once it was ever answered.
    fn ended_notice(&self) -> String {
        match self.active.as_ref().and_then(|a| a.answered_at) {
            Some(_) => format!("Call ended · {}", call_clock(self.elapsed_secs())),
            None => "Call ended".into(),
        }
    }

    /// Whether the call's connection dropped and is being waited for.
    #[must_use]
    pub fn reconnecting(&self) -> bool {
        self.active.as_ref().is_some_and(|a| a.reconnecting)
    }

    /// Who is in a chat's call, by name.
    #[must_use]
    pub fn in_call(&self, group: &[u8]) -> Vec<String> {
        self.rosters
            .get(group)
            .map(|r| r.iter().map(|m| m.username.clone()).collect())
            .unwrap_or_default()
    }

    /// Calls ringing for `me`: people are in them, `me` is not, and they were
    /// not dismissed. The one this device is in never rings.
    #[must_use]
    pub fn ringing(&self, me: &str) -> Vec<Ringing> {
        let mut out: Vec<Ringing> = self
            .rosters
            .iter()
            .filter(|(g, r)| {
                !r.is_empty()
                    && self.active_group() != Some(g.as_slice())
                    && !self.dismissed.contains(*g)
                    && !r.iter().any(|m| m.username.eq_ignore_ascii_case(me))
            })
            .map(|(g, r)| Ringing {
                group: g.clone(),
                who: r.iter().map(|m| m.username.clone()).collect(),
            })
            .collect();
        out.sort_by(|a, b| a.group.cmp(&b.group));
        out
    }

    /// Who a participant in this device's call is, by name.
    /// Whether `participant` is speaking now — or this device, for
    /// [`SELF_TILE`]. Always false in the browser, where the audio never
    /// passes through here.
    #[must_use]
    pub fn speaking(&self, participant: u64) -> bool {
        let Some(a) = self.active.as_ref() else {
            return false;
        };
        if participant == SELF_TILE {
            return !self.muted && self.level > 0.08;
        }
        a.speakers
            .get(&participant)
            .and_then(|s| s.heard_at)
            .is_some_and(|t| t.elapsed().as_millis() < SPEAKING_HOLD_MS)
    }

    /// Everybody else in this device's call, by participant: the call bar's
    /// tiles, one each whether their camera is on or not.
    #[must_use]
    pub fn others(&self) -> Vec<(u64, String)> {
        let Some(a) = self.active.as_ref() else {
            return Vec::new();
        };
        self.rosters
            .get(&a.group)
            .map(|r| {
                r.iter()
                    .filter(|m| Some(m.participant) != a.participant)
                    .map(|m| (m.participant, m.username.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Everybody in this device's call, itself included, in join order, as
    /// the server last said (protocol 41: with their role, hand, hold and
    /// screen).
    #[must_use]
    pub fn members(&self) -> &[CallMember] {
        self.active
            .as_ref()
            .and_then(|a| self.rosters.get(&a.group))
            .map_or(&[], Vec::as_slice)
    }

    /// One person in this device's call.
    #[must_use]
    pub fn member(&self, participant: u64) -> Option<&CallMember> {
        self.members().iter().find(|m| m.participant == participant)
    }

    /// This device, as its call's roster has it.
    #[must_use]
    pub fn me(&self) -> Option<&CallMember> {
        let me = self.active.as_ref()?.participant?;
        self.member(me)
    }

    /// Whether this device may take a host's actions in its call.
    #[must_use]
    pub fn hosting(&self) -> bool {
        self.me().is_some_and(|m| m.role.hosts())
    }

    /// The record this device owes when its call ends: a host's, of a call
    /// somebody answered.
    fn record_of_ending(&self) -> Option<RecordDue> {
        if !self.hosting() {
            return None;
        }
        let a = self.active.as_ref()?;
        Some(RecordDue {
            group: a.group.clone(),
            start_ms: a.answered_ms?,
            end_ms: unix_ms(),
            people: a.seen_people.iter().cloned().collect(),
            transcript: self.transcript.clone(),
        })
    }

    /// Whether this device is sharing its screen (or asking to: on Android the
    /// system's dialog may still be up).
    #[must_use]
    pub fn sharing_screen(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.active.as_ref().is_some_and(|a| a.screen.is_some());
        #[cfg(target_arch = "wasm32")]
        false
    }

    /// Sharing was asked for and no picture has come yet: the phone's dialog
    /// is up, or a desktop is asking for its permission.
    #[must_use]
    pub fn screen_starting(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self
            .active
            .as_ref()
            .and_then(|a| a.screen.as_ref())
            .is_some_and(|s| !s.running());
        #[cfg(target_arch = "wasm32")]
        false
    }

    /// A host posts the record now, as the meeting stands.
    pub fn post_record_now(&mut self) {
        self.record_due = self.record_of_ending();
    }

    /// When this device's call started, Unix ms, once somebody answered.
    #[must_use]
    pub fn started_ms(&self) -> Option<u64> {
        self.active.as_ref().and_then(|a| a.answered_ms)
    }

    /// What the hosts of this device's call allow.
    #[must_use]
    pub fn call_settings(&self) -> CallSettings {
        self.active
            .as_ref()
            .and_then(|a| self.settings.get(&a.group).copied())
            .unwrap_or_default()
    }

    /// The person whose shared screen is showing, if anybody's is: the most
    /// recent sharer the roster names whose picture has arrived.
    #[must_use]
    pub fn sharer(&self) -> Option<u64> {
        let a = self.active.as_ref()?;
        self.members()
            .iter()
            .rev()
            .filter(|m| m.sharing && Some(m.participant) != a.participant)
            .map(|m| m.participant)
            .find(|p| a.screens.contains_key(p))
    }

    /// The newest picture of each shared screen in this device's call.
    pub fn screens_mut(&mut self) -> impl Iterator<Item = (u64, &mut Picture)> {
        self.active
            .iter_mut()
            .flat_map(|a| a.screens.iter_mut().map(|(k, v)| (*k, v)))
    }

    /// Whether this device waits for a host to let it into its call.
    #[must_use]
    pub fn waiting_to_join(&self) -> bool {
        self.active.as_ref().is_some_and(|a| a.waiting)
    }

    /// Who waits, the spotlight and the open rooms of this device's call.
    #[must_use]
    pub fn extras(&self) -> MeetingExtras {
        self.active
            .as_ref()
            .and_then(|a| self.extras.get(&a.group).cloned())
            .unwrap_or_default()
    }

    /// The captions to show now: whoever spoke last, within five seconds.
    #[must_use]
    pub fn caption_now(&self) -> Option<(String, String)> {
        let (p, (text, _)) = self
            .captions
            .iter()
            .filter(|(_, (_, at))| at.elapsed().as_secs() < 5)
            .max_by_key(|(_, (_, at))| *at)?;
        Some((
            self.name_of(*p).unwrap_or("Somebody").to_string(),
            text.clone(),
        ))
    }

    /// The reaction showing over a tile now, if any: the newest.
    #[must_use]
    pub fn reaction_of(&self, participant: u64) -> Option<&Reaction> {
        self.reactions
            .iter()
            .rev()
            .find(|r| r.participant == participant && r.at.elapsed().as_secs_f32() < REACTION_SECS)
    }

    // The call tools (`dev::mcp`) name people by it; the call bar uses `others`.
    #[cfg_attr(not(mcp), allow(dead_code))]
    pub fn name_of(&self, participant: u64) -> Option<&str> {
        if participant == SELF_TILE {
            return Some("You");
        }
        self.rosters
            .get(self.active_group()?)?
            .iter()
            .find(|m| m.participant == participant)
            .map(|m| m.username.as_str())
    }

    /// The newest picture from each person in this device's call.
    pub fn pictures_mut(&mut self) -> impl Iterator<Item = (u64, &mut Picture)> {
        self.active
            .iter_mut()
            .flat_map(|a| a.pictures.iter_mut().map(|(k, v)| (*k, v)))
    }

    /// What this device's call has received so far, per participant:
    /// (participant, audio frames, pictures). For `mcp`'s `call_status`.
    #[cfg(mcp)]
    #[must_use]
    pub fn received(&self) -> Vec<(u64, u64, u64)> {
        let mut v: Vec<_> = self
            .active
            .iter()
            .flat_map(|a| a.received.iter().map(|(p, (au, pi))| (*p, *au, *pi)))
            .collect();
        v.sort_unstable();
        v
    }

    /// Stand in a call for a scripted capture of the call screen, or take the
    /// stand-in away (`None`). `screen` is the `--ui` name: `callincoming`,
    /// `call` (voice), `callvideo`, `callgroup` or `callfolded`. The pictures
    /// are the stand-in camera's test pattern; the session is a real one
    /// pointed at nothing, so the screen reads exactly the state a call has.
    pub fn seed_demo(&mut self, screen: Option<&str>) {
        let group = vec![0xDE; 16];
        if self.demo {
            self.active = None;
            self.rosters.clear();
            self.minimized = false;
            self.camera_on = false;
            self.demo = false;
        }
        let Some(screen) = screen else {
            return;
        };
        self.demo = true;
        let member = |name: &str, participant: u64| CallMember {
            participant,
            username: name.into(),
            device: uuid::Uuid::from_u128(u128::from(participant)),
            role: if participant == 1 {
                cartopolis_core::CallRole::Host
            } else {
                cartopolis_core::CallRole::Member
            },
            hand: (screen == "callgroup" && participant == 3).then_some(1),
            held: false,
            sharing: false,
            muted: false,
            recording: false,
            guest: false,
            room: 0,
        };
        if screen == "callincoming" {
            self.rosters.insert(group, vec![member("Jeroen", 2)]);
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut members = vec![member("You", 1), member("Jeroen", 2)];
            if screen == "callgroup" {
                members.push(member("Annemiek", 3));
                members.push(member("Sven", 4));
            }
            self.rosters.insert(group.clone(), members);
            let Some(mut a) = start(&group, None) else {
                return;
            };
            a.participant = Some(1);
            a.connected = true;
            a.others_seen = true;
            a.started = web_time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(83))
                .unwrap_or_else(web_time::Instant::now);
            a.answered_at = Some(a.started);
            a.answered_ms = Some(unix_ms().saturating_sub(83_000));
            // Not the stand-in camera's test card: its bottom third is black by
            // design, which in a capture reads as a picture that stops short of
            // the screen's edge. A portrait frame (what an upright phone sends)
            // with a gradient to its edges and a white border shows exactly
            // where the picture ends.
            let picture = |n: u64| {
                let (width, height) = (240u32, 320u32);
                let mut rgba = Vec::with_capacity((width * height * 4) as usize);
                let hue = (n % 6) as f32 / 6.0;
                for y in 0..height {
                    for x in 0..width {
                        let border = x < 4 || y < 4 || x >= width - 4 || y >= height - 4;
                        let t = y as f32 / height as f32;
                        let (r, g, b) = if border {
                            (255, 255, 255)
                        } else {
                            (
                                (60.0 + 160.0 * (1.0 - t) * hue) as u8,
                                (90.0 + 100.0 * t) as u8,
                                (140.0 + 90.0 * (1.0 - hue)) as u8,
                            )
                        };
                        rgba.extend_from_slice(&[r, g, b, 255]);
                    }
                }
                Picture {
                    width,
                    height,
                    rgba,
                    fresh: true,
                    at: web_time::Instant::now(),
                }
            };
            if matches!(screen, "callvideo" | "callgroup") {
                a.pictures.insert(2, picture(3));
                a.pictures.insert(SELF_TILE, picture(9));
                self.camera_on = true;
            }
            if screen == "callgroup" {
                a.pictures.insert(4, picture(5));
            }
            self.minimized = screen == "callfolded";
            self.active = Some(a);
        }
    }

    /// Whether this build can place a call at all.
    #[must_use]
    pub fn available() -> bool {
        #[cfg(target_arch = "wasm32")]
        return crate::platform::web_call::WebSession::supported();
        #[cfg(not(target_arch = "wasm32"))]
        true
    }
}

/// The host a call's UDP goes to: the simulator this client is connected to,
/// whatever form the address was typed in.
#[must_use]
pub fn host_of(addr: &str) -> String {
    let rest = addr
        .split_once("://")
        .map_or(addr, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or("");
    if let Some(v6) = rest.strip_prefix('[') {
        return v6.split(']').next().unwrap_or("").to_string();
    }
    match rest.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host.to_string(),
        _ => rest.to_string(),
    }
}

/// Carry a call's signalling, feed it the microphone and the chat's keys, and
/// collect what it heard. Once a frame.
#[allow(clippy::too_many_arguments)]
pub fn drive_calls(
    online: Option<Res<OnlineSession>>,
    addr: Option<Res<ConnectAddr>>,
    mut inbox: ResMut<CallInbox>,
    mut calls: ResMut<Calls>,
    private: Res<PrivateChats>,
    mut voice: ResMut<VoiceState>,
    prefs: Res<AppPrefs>,
    identity: Res<AuthIdentity>,
) {
    let me = identity.username.clone().unwrap_or_default();
    let Some(online) = online else {
        if calls.demo {
            inbox.0.clear();
            return;
        }
        // No connection, no call: the forwarder has already dropped us.
        calls.active = None;
        calls.rosters.clear();
        inbox.0.clear();
        return;
    };
    for msg in inbox.0.drain(..) {
        match msg {
            NetMessage::CallRoster {
                group,
                members,
                settings,
                waiting,
                spotlight,
                rooms,
            } => {
                // Somebody who left stops being drawn: the forwarder does not
                // renegotiate the last person's receiving connection down to
                // nothing (`sfu::negotiate`), so no offer says they went.
                let mut held_now = None;
                if let Some(a) = calls.active.as_mut().filter(|a| a.group == group) {
                    let here: HashSet<u64> = members.iter().map(|m| m.participant).collect();
                    if !a.waiting {
                        a.seen_people
                            .extend(members.iter().map(|m| m.username.clone()));
                    }
                    a.pictures
                        .retain(|p, _| *p == SELF_TILE || here.contains(p));
                    // A screen whose sharer stopped goes at once, not when it ages.
                    let sharing: HashSet<u64> = members
                        .iter()
                        .filter(|m| m.sharing)
                        .map(|m| m.participant)
                        .collect();
                    a.screens.retain(|p, _| sharing.contains(p));
                    // This device's own screen: once the roster has said it is
                    // shared, a roster saying it is not is a host stopping it.
                    let mine = a.participant.is_some_and(|p| sharing.contains(&p));
                    if mine {
                        a.sharing_seen = true;
                    } else if a.sharing_seen {
                        a.sharing_seen = false;
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            a.screen = None;
                        }
                    }
                    let held = members
                        .iter()
                        .any(|m| Some(m.participant) == a.participant && m.held);
                    if held != a.held {
                        a.held = held;
                        held_now = Some(held);
                    }
                }
                // Held by a host: muted here too, and it stays muted when the
                // hold lifts, until the person unmutes themselves.
                match held_now {
                    Some(true) => {
                        calls.muted = true;
                        calls.notice =
                            Some("A host muted you. Raise your hand to ask to talk.".into());
                    }
                    Some(false) => {
                        calls.notice = Some("You may talk now. Unmute when ready.".into())
                    }
                    None => {}
                }
                calls.settings.insert(group.clone(), settings);
                calls.extras.insert(
                    group.clone(),
                    MeetingExtras {
                        waiting,
                        spotlight,
                        rooms,
                    },
                );
                if members.is_empty() {
                    // A call that rang here and stopped before anybody here
                    // answered or declined it: a missed call, said once.
                    if let Some(old) = calls.rosters.get(&group) {
                        let rang_here = !old.iter().any(|m| m.username.eq_ignore_ascii_case(&me));
                        if rang_here
                            && !calls.dismissed.contains(&group)
                            && calls.active_group() != Some(group.as_slice())
                        {
                            let who: Vec<String> = old.iter().map(|m| m.username.clone()).collect();
                            calls.notice = Some(format!("Missed call from {}", who.join(", ")));
                        }
                    }
                    calls.dismissed.remove(&group);
                    calls.rosters.remove(&group);
                    calls.settings.remove(&group);
                    calls.extras.remove(&group);
                } else {
                    // Somebody new hears where the agenda is from a host.
                    let newcomer = calls.active_group() == Some(group.as_slice())
                        && calls.rosters.get(&group).is_some_and(|old| {
                            members
                                .iter()
                                .any(|m| !old.iter().any(|o| o.participant == m.participant))
                        });
                    calls.rosters.insert(group, members);
                    if newcomer && calls.hosting() {
                        if let Some(at) = calls.agenda.clone() {
                            send_agenda(&calls, &private, &online, &at.plan, Some(&at));
                        }
                    }
                }
            }
            NetMessage::CallDeclined { group, username } => {
                if username.eq_ignore_ascii_case(&me) {
                    // Declined on another of this account's devices: stop
                    // ringing here too, and it is not a missed call.
                    calls.dismissed.insert(group);
                } else if calls.active_group() == Some(group.as_slice())
                    && calls.others().is_empty()
                {
                    // The call this device is ringing out. In a one-to-one chat
                    // that is the end of it; in a group the others may still pick
                    // up, so it is only said.
                    let is_group = private.describe(&group).is_some_and(|(g, _)| g);
                    calls.notice = Some(format!("{username} declined"));
                    if !is_group {
                        online.send("call leave", NetMessage::CallLeave { group });
                        calls.active = None;
                    }
                }
            }
            NetMessage::CallJoined {
                group,
                participant,
                answer,
                ..
            } => {
                if let Some(a) = calls.active.as_mut().filter(|a| a.group == group) {
                    a.participant = Some(participant);
                    a.waiting = false;
                    #[cfg(not(target_arch = "wasm32"))]
                    a.session.joined(answer);
                    #[cfg(target_arch = "wasm32")]
                    a.web.joined(&answer);
                }
            }
            NetMessage::CallOffer { group, sdp, tracks } => {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(a) = calls.active.as_ref().filter(|a| a.group == group) {
                    let tracks = tracks
                        .into_iter()
                        .map(|t| cartopolis_calls::media::TrackInfo {
                            mid: t.mid,
                            from: cartopolis_calls::media::ParticipantId(t.participant),
                            kind: match t.kind {
                                cartopolis_core::CallTrackKind::Audio => {
                                    cartopolis_calls::media::Kind::Audio
                                }
                                cartopolis_core::CallTrackKind::Camera => {
                                    cartopolis_calls::media::Kind::Video
                                }
                                cartopolis_core::CallTrackKind::Screen => {
                                    cartopolis_calls::media::Kind::Screen
                                }
                            },
                        })
                        .collect();
                    a.session.offered(sdp, tracks);
                }
                #[cfg(target_arch = "wasm32")]
                if let Some(a) = calls.active.as_ref().filter(|a| a.group == group) {
                    a.web.offered(&sdp, &tracks);
                }
            }
            NetMessage::CallWaiting {
                group, participant, ..
            } => {
                if let Some(a) = calls.active.as_mut().filter(|a| a.group == group) {
                    a.participant = Some(participant);
                    a.waiting = true;
                }
            }
            // A refused host action: said, and the call goes on. A screen the
            // server would not let this device share stops here too.
            NetMessage::ServerError {
                message,
                request_id: CALL_CONTROL_REQUEST,
                ..
            } => {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(a) = calls.active.as_mut().filter(|a| !a.sharing_seen) {
                    a.screen = None;
                }
                calls.notice = Some(message);
            }
            NetMessage::CallEnded { group, reason } => {
                if calls.active_group() == Some(group.as_slice()) {
                    calls.active = None;
                    calls.reactions.clear();
                    calls.captions.clear();
                    calls.notice = Some(match reason {
                        CallEndReason::Removed { by } => format!("{by} removed you from the call"),
                        CallEndReason::Ended { by } => format!("{by} ended the call for everyone"),
                    });
                }
            }
            // The code for the guest link asked for: the link is the code and
            // this device's secret, posted in the chat and shown to copy.
            NetMessage::CallInvited { group, code, .. } if !code.is_empty() => {
                if let Some((asked, secret)) = calls.invite_secret.take() {
                    if asked == group {
                        let link = guest_link(&code, &secret);
                        calls.guest_links.insert(group.clone(), secret);
                        calls.invited = Some(link.clone());
                        calls.link_to_post = Some((group, link));
                        calls.notice = Some("Guest link made: it is in the chat too".into());
                    }
                }
            }
            NetMessage::CallReacted {
                group,
                participant,
                epoch,
                sealed,
            } => {
                // Opened with the chat's key for that epoch; one that does not
                // open (an epoch this device has moved past) is dropped.
                #[cfg(not(target_arch = "wasm32"))]
                if calls.active_group() == Some(group.as_slice()) {
                    let opened = private
                        .call_epoch(&group)
                        .filter(|e| e.epoch == epoch)
                        .and_then(|e| cartopolis_calls::events::open(&e.base_key, epoch, &sealed))
                        .and_then(|b| String::from_utf8(b).ok());
                    match opened {
                        // A line of captions (`{"c": text, "f": final}`), made on
                        // the speaker's own device.
                        Some(text) if text.starts_with('{') => {
                            #[derive(serde::Deserialize)]
                            struct Line {
                                c: String,
                                #[serde(default)]
                                f: bool,
                            }
                            if let Ok(line) = serde_json::from_str::<Line>(&text) {
                                let text: String = line.c.chars().take(300).collect();
                                if line.f {
                                    let who = calls
                                        .name_of(participant)
                                        .unwrap_or("Somebody")
                                        .to_string();
                                    calls.transcript.push((who, text.clone()));
                                }
                                calls
                                    .captions
                                    .insert(participant, (text, web_time::Instant::now()));
                            }
                            // Where the agenda is (`send_agenda`): believed
                            // only from a host.
                            #[derive(serde::Deserialize)]
                            struct Agenda {
                                g: i64,
                                p: String,
                                t: u64,
                            }
                            let from_host = calls
                                .members()
                                .iter()
                                .any(|m| m.participant == participant && m.role.hosts());
                            if let (true, Ok(a)) =
                                (from_host, serde_json::from_str::<Agenda>(&text))
                            {
                                calls.agenda = usize::try_from(a.g).ok().map(|index| AgendaAt {
                                    plan: a.p,
                                    index,
                                    started_ms: a.t,
                                });
                            }
                        }
                        Some(emoji) if emoji.chars().count() <= 16 => {
                            calls.reactions.push(Reaction {
                                participant,
                                // The web sends ❤️ with its emoji-style selector,
                                // which the reaction font has no glyph for.
                                emoji: emoji.replace('\u{fe0f}', ""),
                                at: web_time::Instant::now(),
                            });
                        }
                        _ => {}
                    }
                }
                #[cfg(target_arch = "wasm32")]
                let _ = (group, participant, epoch, sealed);
            }
            NetMessage::ServerError { message, .. } => {
                tracing::warn!(%message, "call refused by the server");
                calls.notice = Some(format!("Call ended: {message}"));
                calls.active = None;
            }
            _ => {}
        }
    }

    match calls.request.take() {
        Some(CallRequest::Join(group)) => {
            if calls.active_group() == Some(group.as_slice()) {
                // Already in it.
            } else if !Calls::available() {
                calls.notice = Some(
                    "This browser cannot make an encrypted call. A recent Chrome, Firefox or \
                     Safari can, and so can the app."
                        .into(),
                );
            } else if private.call_epoch(&group).is_none() {
                calls.notice = Some("This chat is not ready for a call yet.".into());
            } else {
                if let Some(old) = calls.active.take() {
                    online.send("call leave", NetMessage::CallLeave { group: old.group });
                }
                // Both would open the microphone, and a call is not something
                // to broadcast to whoever is standing nearby.
                voice.set_mode(VoiceMode::Off);
                calls.dismissed.remove(&group);
                calls.active = start(&group, addr.as_deref());
                calls.minimized = false;
            }
        }
        Some(CallRequest::Leave) => {
            let ended = calls.ended_notice();
            if let Some(a) = calls.active.take() {
                calls.notice = Some(ended);
                online.send("call leave", NetMessage::CallLeave { group: a.group });
            }
        }
        Some(CallRequest::Dismiss(group)) => {
            // Declining, as a phone does: the caller hears at once, and this
            // account's other devices stop ringing (protocol 31).
            online.send(
                "call decline",
                NetMessage::CallDecline {
                    group: group.clone(),
                },
            );
            calls.dismissed.insert(group);
        }
        Some(CallRequest::Control(action)) => {
            // Ending it for everybody, the host writes the record.
            if action == CallAction::EndForAll {
                calls.record_due = calls.record_of_ending();
            }
            if let Some(group) = calls.active_group().map(<[u8]>::to_vec) {
                online.send("call control", NetMessage::CallControl { group, action });
            }
        }
        Some(CallRequest::React(emoji)) => {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(group) = calls.active_group().map(<[u8]>::to_vec) {
                if let Some(e) = private.call_epoch(&group) {
                    let sealed =
                        cartopolis_calls::events::seal(&e.base_key, e.epoch, emoji.as_bytes());
                    online.send(
                        "call react",
                        NetMessage::CallReact {
                            group,
                            epoch: e.epoch,
                            sealed,
                        },
                    );
                    calls.reactions.push(Reaction {
                        participant: SELF_TILE,
                        emoji,
                        at: web_time::Instant::now(),
                    });
                }
            }
            #[cfg(target_arch = "wasm32")]
            let _ = emoji;
        }
        Some(CallRequest::Share(on)) => {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(a) = calls.active.as_mut() {
                a.screen = on.then(crate::platform::screen::ScreenCapture::start);
                a.sharing_seen = false;
                online.send(
                    "call sharing",
                    NetMessage::CallControl {
                        group: a.group.clone(),
                        action: CallAction::Sharing(on),
                    },
                );
            }
            #[cfg(target_arch = "wasm32")]
            if on {
                calls.notice = Some("Share your screen from the chat page in this browser.".into());
            }
        }
        Some(CallRequest::InviteGuest) => {
            if let Some(group) = calls.active_group().map(<[u8]>::to_vec) {
                // The secret: a v4 uuid's 16 bytes, 122 of their bits random.
                let secret = uuid::Uuid::new_v4().into_bytes().to_vec();
                calls.invite_secret = Some((group.clone(), secret));
                online.send("call invite", NetMessage::CallInvite { group });
            }
        }
        Some(CallRequest::Agenda(plan, index)) => {
            let at = index.map(|index| AgendaAt {
                plan: plan.clone(),
                index,
                started_ms: unix_ms(),
            });
            send_agenda(&calls, &private, &online, &plan, at.as_ref());
            calls.agenda = at;
        }
        None => {}
    }
    calls
        .reactions
        .retain(|r| r.at.elapsed().as_secs_f32() < REACTION_SECS);

    // The phone rules. A call you placed that nobody answers within
    // [`NO_ANSWER`] ends ("No answer"); and a call ends for you when the last
    // other person in it hangs up, as it does on a phone.
    if let Some(group) = calls.active_group().map(<[u8]>::to_vec) {
        let others = calls.others().len();
        let (seen, since) = match calls.active.as_mut() {
            Some(a) => {
                a.others_seen |= others > 0;
                if others > 0 && a.connected && a.answered_at.is_none() {
                    a.answered_at = Some(web_time::Instant::now());
                    a.answered_ms = Some(unix_ms());
                }
                (a.others_seen, a.started.elapsed())
            }
            None => (true, std::time::Duration::ZERO),
        };
        let end = if seen && others == 0 {
            Some(calls.ended_notice())
        } else if !seen && since > NO_ANSWER {
            Some("No answer".into())
        } else {
            None
        };
        if let Some(why) = end {
            // The host left last writes what the meeting came to.
            if seen {
                calls.record_due = calls.record_of_ending();
            }
            online.send("call leave", NetMessage::CallLeave { group });
            calls.active = None;
            calls.agenda = None;
            calls.notice = Some(why);
        }
    }

    let Calls {
        active,
        notice,
        muted,
        level,
        camera_on,
        fake_media,
        agent_voice,
        minimized,
        keep_noise,
        demo,
        ..
    } = &mut *calls;
    // Folded down to the bar, the call shows no pictures, so none are fetched
    // or decoded; the newest of each waits for the screen to come back.
    let looking = !*minimized;
    let Some(a) = active.as_mut() else {
        *level = 0.0;
        return;
    };

    // The others see who is muted (protocol 41): said once in, and on change.
    if a.participant.is_some() && a.told_muted != Some(*muted) {
        a.told_muted = Some(*muted);
        online.send(
            "call muted",
            NetMessage::CallControl {
                group: a.group.clone(),
                action: CallAction::Muted(*muted),
            },
        );
    }
    // Folded, nothing draws a camera: the forwarder stops sending them, and
    // sends them all again when the screen comes back. Screens always come.
    if a.participant.is_some() && !a.waiting && a.told_looking != Some(looking) {
        a.told_looking = Some(looking);
        online.send(
            "call watch",
            NetMessage::CallWatch {
                group: a.group.clone(),
                cameras: if looking { None } else { Some(Vec::new()) },
            },
        );
    }

    // The chat's keys, whenever its epoch moves (and once at the start).
    if let Some(e) = private.call_epoch(&a.group) {
        if a.epoch != Some(e.epoch) {
            a.epoch = Some(e.epoch);
            #[cfg(not(target_arch = "wasm32"))]
            a.session.epoch(
                e.epoch,
                e.base_key,
                e.own_index,
                e.members.iter().map(|m| m.0).collect(),
            );
            #[cfg(target_arch = "wasm32")]
            a.web.epoch(e.epoch, &e.base_key, e.own_index);
        }
    }

    #[cfg(target_arch = "wasm32")]
    {
        use crate::platform::web_call::Event;
        let mut failed = None;
        for ev in a.web.events() {
            match ev {
                Event::Offer(offer) => {
                    online.send(
                        "call join",
                        NetMessage::CallJoin {
                            request_id: JOIN_REQUEST,
                            group: a.group.clone(),
                            offer,
                            screen: None,
                        },
                    );
                }
                Event::Answer(sdp) => {
                    online.send(
                        "call answer",
                        NetMessage::CallAnswer {
                            group: a.group.clone(),
                            sdp,
                        },
                    );
                }
                Event::Connected => a.connected = true,
                Event::Notice(text) => *notice = Some(text),
                Event::CameraOff => {
                    *camera_on = false;
                    a.web_camera = false;
                }
                Event::Failed(why) => failed = Some(why),
            }
        }
        for p in looking.then(|| a.web.pictures()).unwrap_or_default() {
            a.received.entry(p.participant).or_default().1 += 1;
            a.pictures.insert(
                p.participant,
                Picture {
                    width: p.width,
                    height: p.height,
                    rgba: p.rgba,
                    fresh: true,
                    at: web_time::Instant::now(),
                },
            );
        }
        if let Some(why) = failed {
            tracing::warn!(%why, "call ended");
            *notice = Some(format!("Call ended: {why}"));
            online.send(
                "call leave",
                NetMessage::CallLeave {
                    group: a.group.clone(),
                },
            );
            *active = None;
            *level = 0.0;
            return;
        }
        // The browser owns the microphone and the camera; they are told, not fed.
        a.web.set_muted(*muted);
        if a.web_camera != *camera_on {
            a.web_camera = *camera_on;
            a.web.set_camera(*camera_on);
        }
        *level = 0.0;
        let _ = (fake_media, agent_voice, &prefs, keep_noise);
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        use cartopolis_calls::session::Event;
        let mut failed = None;
        for ev in a.session.events() {
            match ev {
                Event::Offer(offer) => {
                    online.send(
                        "call join",
                        NetMessage::CallJoin {
                            request_id: JOIN_REQUEST,
                            group: a.group.clone(),
                            offer,
                            screen: a.session.screen_mid(),
                        },
                    );
                }
                Event::Answer(sdp) => {
                    online.send(
                        "call answer",
                        NetMessage::CallAnswer {
                            group: a.group.clone(),
                            sdp,
                        },
                    );
                }
                Event::Connected => a.connected = true,
                Event::Audio { from, pcm, .. } => {
                    a.received.entry(from.0).or_default().0 += 1;
                    let speaker = a.speakers.entry(from.0).or_insert_with(|| Speaker {
                        ring: voice::Ring::default(),
                        entity: None,
                        heard_at: None,
                    });
                    let loud = pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len().max(1) as f32;
                    if loud.sqrt() > SPEAKING_RMS {
                        speaker.heard_at = Some(web_time::Instant::now());
                    }
                    if let Ok(mut ring) = speaker.ring.lock() {
                        ring.extend(pcm);
                        let cap = (voice::MAX_RING_SECS * RATE as f32) as usize;
                        while ring.len() > cap {
                            ring.pop_front();
                        }
                    }
                }
                Event::Reconnecting => a.reconnecting = true,
                Event::Reconnected => a.reconnecting = false,
                Event::Failed(why) => failed = Some(why),
            }
        }
        a.session.set_video_wanted(looking);
        if looking {
            for ((from, kind), p) in a.session.take_pictures() {
                let screen = kind == cartopolis_calls::media::Kind::Screen;
                if !screen {
                    a.received.entry(from.0).or_default().1 = p.decoded;
                }
                if let Some(n) = p.test_number {
                    tracing::debug!(from = from.0, n, "call picture");
                }
                let into = if screen {
                    &mut a.screens
                } else {
                    &mut a.pictures
                };
                into.insert(
                    from.0,
                    Picture {
                        width: p.width,
                        height: p.height,
                        rgba: p.rgba,
                        fresh: true,
                        at: web_time::Instant::now(),
                    },
                );
            }
        }
        if let Some(why) = failed {
            tracing::warn!(%why, "call ended");
            *notice = Some(format!("Call ended: {why}"));
            online.send(
                "call leave",
                NetMessage::CallLeave {
                    group: a.group.clone(),
                },
            );
            *active = None;
            *level = 0.0;
            return;
        }
    }

    // A camera turned off on the other end leaves its last picture behind;
    // nothing says it went, so its age does. Not while folded, when nothing
    // is fetched and every picture ages.
    if looking && !*demo {
        a.pictures
            .retain(|id, p| *id == SELF_TILE || p.at.elapsed() < PICTURE_STALE);
    }

    if a.last_report.elapsed().as_secs() >= 5 {
        a.last_report = web_time::Instant::now();
        for (from, (audio, pictures)) in &a.received {
            tracing::info!(from, audio, pictures, "call: received");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        if *agent_voice {
            *level = 0.0;
        } else {
            capture(a, *muted, !*keep_noise, *fake_media, &prefs, level, notice);
            send_camera(a, camera_on, *fake_media, looking, notice);
        }
        send_screen(a, notice, &online);
    }
}

/// A call's length as a phone shows it: `04:12`, or `1:04:12` past an hour.
#[must_use]
pub fn call_clock(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    }
}

/// Whether this device can send a camera in a call.
#[must_use]
pub fn camera_available() -> bool {
    #[cfg(target_arch = "wasm32")]
    return true;
    #[cfg(not(target_arch = "wasm32"))]
    Camera::available()
}

/// The call's session thread. On a phone its video goes through the system's
/// codecs (`platform::hw_video`), with the pure-Rust encoder behind them.
#[cfg(not(target_arch = "wasm32"))]
fn start_session(host: String) -> cartopolis_calls::session::Session {
    #[cfg(target_os = "android")]
    return cartopolis_calls::session::Session::start_with_encoder(
        host,
        RATE,
        true,
        crate::platform::hw_video::encoder_factory(),
    );
    #[cfg(not(target_os = "android"))]
    cartopolis_calls::session::Session::start(host, RATE, true)
}

/// Start a call's session towards the simulator this client is connected to.
fn start(group: &[u8], addr: Option<&ConnectAddr>) -> Option<Active> {
    let host = addr.map_or_else(|| "127.0.0.1".to_string(), |a| host_of(&a.0));
    #[cfg(target_arch = "wasm32")]
    let _ = host;
    Some(Active {
        group: group.to_vec(),
        participant: None,
        #[cfg(not(target_arch = "wasm32"))]
        // Always offer a camera track, whether the camera is on or not: turning
        // it on mid-call must not need a renegotiation, and a track with no
        // frames on it costs nothing.
        session: start_session(host),
        #[cfg(target_arch = "wasm32")]
        web: crate::platform::web_call::WebSession::start(),
        #[cfg(target_arch = "wasm32")]
        web_camera: false,
        connected: false,
        reconnecting: false,
        epoch: None,
        started: web_time::Instant::now(),
        answered_at: None,
        answered_ms: None,
        seen_people: HashSet::new(),
        #[cfg(not(target_arch = "wasm32"))]
        mic: None,
        #[cfg(not(target_arch = "wasm32"))]
        resampler: Resampler::default(),
        #[cfg(not(target_arch = "wasm32"))]
        denoiser: cartopolis_calls::denoise::Denoiser::new(),
        #[cfg(not(target_arch = "wasm32"))]
        capture: Vec::new(),
        #[cfg(not(target_arch = "wasm32"))]
        pending: Vec::new(),
        speakers: HashMap::new(),
        #[cfg(not(target_arch = "wasm32"))]
        camera: None,
        #[cfg(not(target_arch = "wasm32"))]
        screen: None,
        sharing_seen: false,
        pictures: HashMap::new(),
        screens: HashMap::new(),
        held: false,
        told_muted: None,
        told_looking: None,
        waiting: false,
        #[cfg(not(target_arch = "wasm32"))]
        fake_mic_sent: 0,
        received: HashMap::new(),
        others_seen: false,
        last_report: web_time::Instant::now(),
    })
}

/// The shared screen to the session, a picture at a time as the capture
/// takes them. A capture that stopped (refused, or stopped from the phone's
/// own controls) says why, and the call is told this device no longer shares.
#[cfg(not(target_arch = "wasm32"))]
fn send_screen(a: &mut Active, notice: &mut Option<String>, online: &OnlineSession) {
    let Some(screen) = a.screen.as_ref() else {
        return;
    };
    if let Some(why) = screen.error() {
        *notice = Some(why);
        a.screen = None;
        a.sharing_seen = false;
        online.send(
            "call sharing",
            NetMessage::CallControl {
                group: a.group.clone(),
                action: CallAction::Sharing(false),
            },
        );
        return;
    }
    if let Some(picture) = screen.take() {
        a.session.send_screen(picture);
    }
}

/// Camera to the session, at the call's frame rate, while it is on. The
/// stand-in camera (`CARTO_FAKE_CAMERA`) is on from the start, since a
/// headless test has nobody to press the button.
#[cfg(not(target_arch = "wasm32"))]
fn send_camera(
    a: &mut Active,
    on: &mut bool,
    fake_media: bool,
    looking: bool,
    notice: &mut Option<String>,
) {
    let fake = fake_media || crate::env_flag("CARTO_FAKE_CAMERA");
    if fake && !fake_media {
        *on = true;
    }
    if !*on {
        a.camera = None;
        a.pictures.remove(&SELF_TILE);
        return;
    }
    if a.camera.is_none() {
        let camera = if fake { Camera::fake() } else { Camera::open() };
        // The camera thread sends straight to the session: video at the
        // camera's pace, not at the app's frame rate.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let sink = a.session.picture_sink();
            camera.set_sink(Box::new(move |p| sink.send(p)));
        }
        a.camera = Some(camera);
    }
    // The encoder decides the size (a hardware one sends more, and a failed
    // one hands back to software mid-call), so the camera is told each frame.
    if let Some(camera) = a.camera.as_ref() {
        camera.set_send_size(a.session.send_size());
    }
    if let Some(err) = a.camera.as_ref().and_then(Camera::error) {
        *notice = Some(err);
        *on = false;
        a.camera = None;
        a.pictures.remove(&SELF_TILE);
        return;
    }
    // What the camera sees, for the person's own tile: mirrored, and small.
    // Not built while folded, where there is no tile to put it in.
    if let Some(p) = a
        .camera
        .as_ref()
        .filter(|_| looking)
        .and_then(Camera::take_preview)
    {
        let (width, height, rgba) = crate::platform::camera::self_view(&p, 320);
        a.pictures.insert(
            SELF_TILE,
            Picture {
                width,
                height,
                rgba,
                fresh: true,
                at: web_time::Instant::now(),
            },
        );
    }
}

/// Microphone to the session, in 20 ms frames at [`RATE`]. The device stays
/// open while muted, so unmuting does not cost the first word to a device
/// opening; the audio is simply not sent.
#[cfg(not(target_arch = "wasm32"))]
fn capture(
    a: &mut Active,
    muted: bool,
    denoise: bool,
    fake_media: bool,
    prefs: &AppPrefs,
    level: &mut f32,
    notice: &mut Option<String>,
) {
    // The stand-in microphone (`CARTO_FAKE_MIC`): a tone, 20 ms at a time, as
    // many frames as wall-clock time says are due.
    if fake_media || crate::env_flag("CARTO_FAKE_MIC") {
        *level = if muted { 0.0 } else { 0.3 };
        let due = a.started.elapsed().as_millis() as u64 / 20;
        while a.fake_mic_sent < due {
            let pcm = cartopolis_calls::video::test_tone(voice::VOICE_RATE, a.fake_mic_sent);
            a.fake_mic_sent += 1;
            if !muted && a.connected {
                #[cfg(not(target_arch = "wasm32"))]
                a.session.send_pcm(pcm);
                #[cfg(target_arch = "wasm32")]
                let _ = pcm;
            }
        }
        return;
    }
    let mic = a.mic.get_or_insert_with(AudioInput::start_for_call);
    if let Some(err) = mic.error() {
        *notice = Some(format!("Microphone: {err}"));
        a.mic = None;
        *level = 0.0;
        return;
    }
    let Some(src_rate) = mic.sample_rate() else {
        return;
    };
    a.capture.clear();
    mic.drain(&mut a.capture, src_rate as usize);
    // Before the gain and the resampling: the network wants the microphone's
    // own 48 kHz, and the level meter then shows what the others hear.
    if denoise {
        a.denoiser.process(&mut a.capture, src_rate);
    }
    voice::apply_gain(&mut a.capture, prefs.capture_gain());
    *level = if muted { 0.0 } else { voice::rms(&a.capture) };
    a.resampler.push(&a.capture, src_rate, &mut a.pending);
    if muted || !a.connected {
        a.pending.clear();
        return;
    }
    let frame = voice::FRAME_SAMPLES;
    // Behind by more than a few frames (a stall): drop the oldest rather than
    // talking a second late for the rest of the call.
    if a.pending.len() > frame * 5 {
        let excess = a.pending.len() - frame * 5;
        a.pending.drain(..excess);
    }
    while a.pending.len() >= frame {
        let pcm: Vec<f32> = a
            .pending
            .drain(..frame)
            .map(|s| f32::from(s) / f32::from(i16::MAX))
            .collect();
        #[cfg(not(target_arch = "wasm32"))]
        a.session.send_pcm(pcm);
        #[cfg(target_arch = "wasm32")]
        let _ = pcm;
    }
}

/// Play the people heard in the call, and stop playing them when it ends.
#[cfg(feature = "audio")]
pub fn play_calls(
    mut commands: Commands,
    mut calls: ResMut<Calls>,
    mut streams: ResMut<Assets<voice::VoiceStream>>,
    prefs: Res<AppPrefs>,
    sinks: Query<(Entity, &CallSink)>,
) {
    let gain = prefs.voice_gain();
    let mut live: HashSet<Entity> = HashSet::new();
    if let Some(a) = calls.active.as_mut() {
        for speaker in a.speakers.values_mut() {
            let entity = *speaker.entity.get_or_insert_with(|| {
                let handle = streams.add(voice::VoiceStream::from_ring(speaker.ring.clone()));
                commands
                    .spawn((
                        bevy::audio::AudioPlayer(handle),
                        bevy::audio::PlaybackSettings {
                            mode: bevy::audio::PlaybackMode::Once,
                            volume: bevy::audio::Volume::Linear(gain),
                            ..default()
                        },
                        CallSink,
                    ))
                    .id()
            });
            live.insert(entity);
        }
    }
    for (entity, _) in &sinks {
        if !live.contains(&entity) {
            commands.entity(entity).try_despawn();
        }
    }
}

/// Marks an entity playing a call participant.
#[derive(Component)]
pub struct CallSink;

/// Test switches for placing a call with nobody at the keyboard:
///
/// - `CARTO_CALL=<username>`: start a direct chat with that person if there is
///   none, then call them.
/// - `CARTO_ANSWER=1`: join any call that rings.
///
/// With `CARTO_FAKE_CAMERA` and `CARTO_FAKE_MIC`, two headless clients signed
/// in to one simulator place a real call — every step after the camera and the
/// microphone on the real path — and `calls: received` in each log says what
/// arrived. On the web the same switches are read from the page address
/// (`?call=bob`, `?answer`, and `?camera` to send the camera once in the
/// call) — **only for a page served from this machine**:
/// on the public site a link carrying `?answer` would open somebody's
/// microphone to whoever rang, so there they do nothing.
#[cfg(not(target_os = "android"))]
#[allow(clippy::too_many_arguments)]
pub fn drive_call_harness(
    mut calls: ResMut<Calls>,
    private: Res<PrivateChats>,
    mut chat: ResMut<crate::systems::chat::ChatState>,
    identity: Res<AuthIdentity>,
    online: Option<Res<OnlineSession>>,
    mut asked: Local<bool>,
    mut last_try: Local<Option<web_time::Instant>>,
) {
    #[cfg(target_arch = "wasm32")]
    if !page_is_local() {
        return;
    }
    // `?camera` on the web: send the browser's camera once in a call (a
    // desktop run has `CARTO_FAKE_CAMERA`, which does this itself).
    #[cfg(target_arch = "wasm32")]
    if calls.active_group().is_some() && crate::env_flag("CARTO_CAMERA") && !calls.camera_on {
        calls.camera_on = true;
    }
    if online.is_none() || calls.request.is_some() || calls.active_group().is_some() {
        return;
    }
    // Not every frame: a join that fails ("not ready yet") is retried.
    if last_try.is_some_and(|t| t.elapsed().as_secs_f32() < 2.0) {
        return;
    }
    if let Some(peer) = crate::env_value("CARTO_CALL") {
        match private.direct_chat_with(&peer) {
            Some(group) => {
                calls.notice = None;
                calls.request = Some(CallRequest::Join(group));
                *last_try = Some(web_time::Instant::now());
            }
            None if !*asked => {
                chat.private_start.push(peer);
                *asked = true;
            }
            None => {}
        }
    }
    if crate::env_flag("CARTO_ANSWER") {
        let me = identity.username.clone().unwrap_or_default();
        if let Some(r) = calls.ringing(&me).into_iter().next() {
            calls.notice = None;
            calls.request = Some(CallRequest::Join(r.group));
            *last_try = Some(web_time::Instant::now());
        }
    }
}

/// Whether this page was served from this machine: the only place the call
/// harness's address switches are honoured.
#[cfg(target_arch = "wasm32")]
fn page_is_local() -> bool {
    let host = web_sys::window()
        .and_then(|w| w.location().hostname().ok())
        .unwrap_or_default();
    matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same link the browser makes (`call.ts`: base64url of the secret,
    /// no padding) reads back here, from the link or the chat line it is in.
    #[test]
    fn a_guest_link_carries_its_code_and_secret() {
        let secret: Vec<u8> = (0u8..16).collect();
        let link = guest_link("k7q2m9x4ab", &secret);
        assert!(
            link.ends_with("/?meet=k7q2m9x4ab#AAECAwQFBgcICQoLDA0ODw"),
            "{link}"
        );
        let line = format!("Guest link for the call, good for a day: {link}");
        assert_eq!(read_guest_link(&line), Some(("k7q2m9x4ab".into(), secret)));
        assert_eq!(read_guest_link("meet=short#AAECAwQFBgcICQoLDA0ODw"), None);
        assert_eq!(read_guest_link("no link here"), None);
    }

    #[test]
    fn the_call_goes_to_the_host_the_client_is_connected_to() {
        assert_eq!(host_of("127.0.0.1:4747"), "127.0.0.1");
        assert_eq!(host_of("wss://cartopolis.org/ws"), "cartopolis.org");
        assert_eq!(host_of("wss://cartopolis.org:8443/ws"), "cartopolis.org");
        assert_eq!(host_of("tcp://10.0.0.2:4747"), "10.0.0.2");
        assert_eq!(host_of("[::1]:4747"), "::1");
        assert_eq!(host_of("cartopolis.org"), "cartopolis.org");
    }

    fn member(name: &str, participant: u64) -> CallMember {
        CallMember {
            participant,
            username: name.into(),
            device: uuid::Uuid::from_u128(u128::from(participant)),
            role: cartopolis_core::CallRole::Member,
            hand: None,
            held: false,
            sharing: false,
            muted: false,
            recording: false,
            guest: false,
            room: 0,
        }
    }

    #[test]
    fn a_call_rings_for_everyone_not_in_it_until_dismissed_or_over() {
        let mut calls = Calls::default();
        let g = vec![1u8; 16];
        calls.rosters.insert(g.clone(), vec![member("alice", 1)]);
        assert_eq!(calls.ringing("bob").len(), 1);
        assert_eq!(calls.ringing("bob")[0].who, vec!["alice".to_string()]);
        assert!(
            calls.ringing("alice").is_empty(),
            "your own call does not ring"
        );
        assert!(
            calls.ringing("ALICE").is_empty(),
            "names are not case-sensitive"
        );

        calls.dismissed.insert(g.clone());
        assert!(calls.ringing("bob").is_empty());
        // The call ends; the next one rings again.
        calls.dismissed.remove(&g);
        calls.rosters.remove(&g);
        assert!(calls.ringing("bob").is_empty());
        calls.rosters.insert(g, vec![member("alice", 2)]);
        assert_eq!(calls.ringing("bob").len(), 1);
    }

    #[test]
    fn only_call_traffic_is_taken_from_the_mailbox() {
        assert!(CallInbox::wants(&NetMessage::CallRoster {
            group: vec![],
            members: vec![],
            settings: CallSettings::default(),
            waiting: vec![],
            spotlight: None,
            rooms: None,
        }));
        assert!(CallInbox::wants(&NetMessage::ServerError {
            request_id: JOIN_REQUEST,
            code: 403,
            message: String::new()
        }));
        assert!(!CallInbox::wants(&NetMessage::ServerError {
            request_id: 7,
            code: 403,
            message: String::new()
        }));
    }
}
