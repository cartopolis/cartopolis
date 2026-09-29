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
//! **The browser has no call yet.** Its WebRTC is the browser's own and the
//! frame encryption needs an encoded-transform worker; until that exists the
//! web build says so rather than offering a button that does nothing.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use cartopolis_core::{CallMember, NetMessage};

use crate::platform::audio::AudioInput;
use crate::platform::camera::Camera;
use crate::resources::{AppPrefs, AuthIdentity, ConnectAddr, OnlineSession};
use crate::systems::private_chat::{channel_key, PrivateChats};
use crate::systems::voice::{self, Resampler, VoiceMode, VoiceState};

/// Request ids for call traffic, so a refusal (`ServerError`) finds its way
/// here rather than to the prim editor, which deselects on a 403.
const REQUEST_BASE: u32 = 0xCA11_0000;
#[cfg(any(not(target_arch = "wasm32"), test))]
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
            | NetMessage::CallRoster { .. } => true,
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
}

/// The call this device is in.
struct Active {
    group: Vec<u8>,
    /// Our number in the call, once the server has answered the join.
    participant: Option<u64>,
    #[cfg(not(target_arch = "wasm32"))]
    session: cartopolis_calls::session::Session,
    connected: bool,
    /// The chat epoch last handed to the session.
    epoch: Option<u64>,
    started: web_time::Instant,
    mic: Option<AudioInput>,
    resampler: Resampler,
    capture: Vec<i16>,
    pending: Vec<i16>,
    /// Who is heard, by participant: the ring their audio goes into.
    speakers: HashMap<u64, Speaker>,
    /// The camera, while it is on.
    camera: Option<Camera>,
    last_picture: web_time::Instant,
    /// The newest picture from each person seen, by participant.
    pictures: HashMap<u64, Picture>,
    /// The stand-in microphone's frame count (`CARTO_FAKE_MIC`).
    fake_mic_sent: u64,
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
}

/// One person heard in the call.
struct Speaker {
    ring: voice::Ring,
    entity: Option<Entity>,
}

/// Every call this device knows about, and the one it is in.
#[derive(Resource, Default)]
pub struct Calls {
    /// Who is in each chat's call, by group id, as the server last said.
    rosters: HashMap<Vec<u8>, Vec<CallMember>>,
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
}

/// What the UI shows about a chat's call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ringing {
    pub group: Vec<u8>,
    /// Who is in it, by name.
    pub who: Vec<String>,
}

impl Calls {
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

    /// How long this device has been in its call.
    #[must_use]
    pub fn elapsed_secs(&self) -> u64 {
        self.active
            .as_ref()
            .map_or(0, |a| a.started.elapsed().as_secs())
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
    #[must_use]
    pub fn name_of(&self, participant: u64) -> Option<&str> {
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
    /// (participant, audio frames, pictures).
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

    /// Whether this build can place a call at all.
    #[must_use]
    pub fn available() -> bool {
        cfg!(not(target_arch = "wasm32"))
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
) {
    let Some(online) = online else {
        // No connection, no call: the forwarder has already dropped us.
        calls.active = None;
        calls.rosters.clear();
        inbox.0.clear();
        return;
    };
    for msg in inbox.0.drain(..) {
        match msg {
            NetMessage::CallRoster { group, members } => {
                if members.is_empty() {
                    calls.dismissed.remove(&group);
                    calls.rosters.remove(&group);
                } else {
                    calls.rosters.insert(group, members);
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
                    #[cfg(not(target_arch = "wasm32"))]
                    a.session.joined(answer);
                    #[cfg(target_arch = "wasm32")]
                    let _ = answer;
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
                            kind: if t.video {
                                cartopolis_calls::media::Kind::Video
                            } else {
                                cartopolis_calls::media::Kind::Audio
                            },
                        })
                        .collect();
                    a.session.offered(sdp, tracks);
                }
                #[cfg(target_arch = "wasm32")]
                let _ = (group, sdp, tracks);
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
                calls.notice = Some("Calls need the app for now; the browser comes later.".into());
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
            }
        }
        Some(CallRequest::Leave) => {
            if let Some(a) = calls.active.take() {
                online.send("call leave", NetMessage::CallLeave { group: a.group });
            }
        }
        Some(CallRequest::Dismiss(group)) => {
            calls.dismissed.insert(group);
        }
        None => {}
    }

    let Calls {
        active,
        notice,
        muted,
        level,
        camera_on,
        fake_media,
        ..
    } = &mut *calls;
    let Some(a) = active.as_mut() else {
        *level = 0.0;
        return;
    };

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
        }
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
                    });
                    if let Ok(mut ring) = speaker.ring.lock() {
                        ring.extend(pcm);
                        let cap = (voice::MAX_RING_SECS * RATE as f32) as usize;
                        while ring.len() > cap {
                            ring.pop_front();
                        }
                    }
                }
                Event::Video { from, frame } => {
                    a.received.entry(from.0).or_default().1 += 1;
                    if let Some(n) = cartopolis_calls::video::test_pattern_number(&frame) {
                        tracing::debug!(from = from.0, n, "call picture");
                    }
                    a.pictures.insert(
                        from.0,
                        Picture {
                            width: frame.width,
                            height: frame.height,
                            rgba: frame.to_rgba(),
                            fresh: true,
                        },
                    );
                }
                Event::Failed(why) => failed = Some(why),
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

    if a.last_report.elapsed().as_secs() >= 5 {
        a.last_report = web_time::Instant::now();
        for (from, (audio, pictures)) in &a.received {
            tracing::info!(from, audio, pictures, "call: received");
        }
    }

    capture(a, *muted, *fake_media, &prefs, level, notice);
    send_camera(a, camera_on, *fake_media, notice);
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
        session: cartopolis_calls::session::Session::start(host, RATE, true),
        connected: false,
        epoch: None,
        started: web_time::Instant::now(),
        mic: None,
        resampler: Resampler::default(),
        capture: Vec::new(),
        pending: Vec::new(),
        speakers: HashMap::new(),
        camera: None,
        last_picture: web_time::Instant::now(),
        pictures: HashMap::new(),
        fake_mic_sent: 0,
        received: HashMap::new(),
        last_report: web_time::Instant::now(),
    })
}

/// Camera to the session, at the call's frame rate, while it is on. The
/// stand-in camera (`CARTO_FAKE_CAMERA`) is on from the start, since a
/// headless test has nobody to press the button.
fn send_camera(a: &mut Active, on: &mut bool, fake_media: bool, notice: &mut Option<String>) {
    let fake = fake_media || crate::env_flag("CARTO_FAKE_CAMERA");
    if fake && !fake_media {
        *on = true;
    }
    if !*on {
        a.camera = None;
        return;
    }
    let camera = a
        .camera
        .get_or_insert_with(|| if fake { Camera::fake() } else { Camera::open() });
    if let Some(err) = camera.error() {
        *notice = Some(err);
        *on = false;
        a.camera = None;
        return;
    }
    let period =
        std::time::Duration::from_micros(1_000_000 / u64::from(cartopolis_calls::video::FPS));
    if !a.connected || a.last_picture.elapsed() < period {
        return;
    }
    if let Some(picture) = camera.take() {
        a.last_picture = web_time::Instant::now();
        #[cfg(not(target_arch = "wasm32"))]
        a.session.send_picture(picture);
        #[cfg(target_arch = "wasm32")]
        let _ = picture;
    }
}

/// Microphone to the session, in 20 ms frames at [`RATE`]. The device stays
/// open while muted, so unmuting does not cost the first word to a device
/// opening; the audio is simply not sent.
fn capture(
    a: &mut Active,
    muted: bool,
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

/// The bar at the top of the screen: a call ringing, or the call you are in.
///
/// An `Area` of its own at the top centre, below the location-sharing notice
/// when that is up (which must never be covered), rather than part of the chat
/// panel — a call rings whatever panel is open, and a call in progress has to
/// be hang-up-able from anywhere.
pub fn render_call_bar(
    mut egui: ResMut<crate::systems::egui_manager::EguiManager>,
    mut calls: ResMut<Calls>,
    identity: Res<AuthIdentity>,
    chat: Res<crate::systems::chat::ChatState>,
    online: Option<Res<OnlineSession>>,
    mut tiles: Local<HashMap<u64, egui::TextureHandle>>,
) {
    use crate::systems::theme;
    if online.is_none() {
        return;
    }
    let me = identity.username.clone().unwrap_or_default();
    let ringing = calls.ringing(&me);
    let active = calls.active_group().map(<[u8]>::to_vec);
    let notice = calls.notice.clone();
    if ringing.is_empty() && active.is_none() && notice.is_none() {
        return;
    }
    let label_of = |group: &[u8]| {
        let key = channel_key(group);
        chat.channels
            .iter()
            .find(|c| c.key == key)
            .map(|c| crate::systems::private_chat::peer_of_label(&c.label).to_string())
            .unwrap_or_else(|| "a chat".into())
    };
    let top = if chat.sharing.is_empty() { 8.0 } else { 52.0 };
    let ctx = egui.ctx_mut().clone();

    // Each person's newest picture, uploaded only when it changed: a texture
    // per person, reused, so a call is one small upload per picture received
    // rather than a texture allocated per frame (`docs/notes/upload-bursts.md`).
    let mut present = HashSet::new();
    for (id, p) in calls.pictures_mut() {
        present.insert(id);
        if !p.fresh {
            continue;
        }
        p.fresh = false;
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [p.width as usize, p.height as usize],
            &p.rgba,
        );
        match tiles.get_mut(&id) {
            Some(t) => t.set(image, egui::TextureOptions::LINEAR),
            None => {
                let t = ctx.load_texture(
                    format!("call-video-{id}"),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                tiles.insert(id, t);
            }
        }
    }
    tiles.retain(|id, _| present.contains(id));
    let mut faces: Vec<(String, egui::TextureId)> = tiles
        .iter()
        .map(|(id, t)| (calls.name_of(*id).unwrap_or("someone").to_string(), t.id()))
        .collect();
    faces.sort_by(|a, b| a.0.cmp(&b.0));
    let mut request = None;
    let mut clear_notice = false;
    egui::Area::new(egui::Id::new("call_bar"))
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, top))
        .order(egui::Order::Foreground)
        .show(&ctx, |ui| {
            egui::Frame::new()
                .fill(theme::SURFACE)
                .stroke(egui::Stroke::new(1.0, theme::ACCENT))
                .corner_radius(12.0)
                .inner_margin(egui::Margin::symmetric(12, 6))
                .show(ui, |ui| {
                    if let Some(group) = &active {
                        let secs = calls.elapsed_secs();
                        let status = if calls.connected() {
                            format!("{:02}:{:02}", secs / 60, secs % 60)
                        } else {
                            "connecting…".into()
                        };
                        let others: Vec<String> = calls
                            .in_call(group)
                            .into_iter()
                            .filter(|n| !n.eq_ignore_ascii_case(&me))
                            .collect();
                        let with = if others.is_empty() {
                            format!("Calling {}", label_of(group))
                        } else {
                            format!("In call with {}", others.join(", "))
                        };
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} {with} · {status}",
                                    crate::systems::icons::LOCK
                                ))
                                .color(theme::CREAM),
                            );
                            let mute = if calls.muted { "Unmute" } else { "Mute" };
                            if theme::action_button(ui, mute).clicked() {
                                calls.muted = !calls.muted;
                            }
                            if Camera::available() {
                                let cam = if calls.camera_on {
                                    "Camera off"
                                } else {
                                    "Camera"
                                };
                                if theme::action_button(ui, cam).clicked() {
                                    calls.camera_on = !calls.camera_on;
                                }
                            }
                            if theme::action_button(ui, "Hang up").clicked() {
                                request = Some(CallRequest::Leave);
                            }
                        });
                        // The people in the call who have their camera on.
                        if !faces.is_empty() {
                            ui.horizontal(|ui| {
                                for (name, texture) in &faces {
                                    ui.vertical(|ui| {
                                        ui.image((*texture, egui::vec2(160.0, 120.0)));
                                        ui.label(
                                            egui::RichText::new(name.as_str())
                                                .size(12.0)
                                                .color(theme::TEXT_DIM),
                                        );
                                    });
                                }
                            });
                        }
                    }
                    for r in &ringing {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} is calling · {}",
                                    r.who.join(", "),
                                    label_of(&r.group)
                                ))
                                .color(theme::CREAM),
                            );
                            if theme::action_button(ui, "Join").clicked() {
                                request = Some(CallRequest::Join(r.group.clone()));
                            }
                            if theme::action_button(ui, "Dismiss").clicked() {
                                request = Some(CallRequest::Dismiss(r.group.clone()));
                            }
                        });
                    }
                    if let Some(text) = &notice {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(text).color(theme::TEXT_DIM));
                            if theme::action_button(ui, "OK").clicked() {
                                clear_notice = true;
                            }
                        });
                    }
                });
        });
    if request.is_some() {
        calls.request = request;
    }
    if clear_notice {
        calls.notice = None;
    }
}

/// Test switches for placing a call with nobody at the keyboard:
///
/// - `CARTO_CALL=<username>`: start a direct chat with that person if there is
///   none, then call them.
/// - `CARTO_ANSWER=1`: join any call that rings.
///
/// With `CARTO_FAKE_CAMERA` and `CARTO_FAKE_MIC`, two headless clients signed
/// in to one simulator place a real call — every step after the camera and the
/// microphone on the real path — and `calls: received` in each log says what
/// arrived. Desktop only: an environment is what a harness sets.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
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
    if online.is_none() || calls.request.is_some() || calls.active_group().is_some() {
        return;
    }
    // Not every frame: a join that fails ("not ready yet") is retried.
    if last_try.is_some_and(|t| t.elapsed().as_secs_f32() < 2.0) {
        return;
    }
    if let Ok(peer) = std::env::var("CARTO_CALL") {
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

#[cfg(test)]
mod tests {
    use super::*;

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
            members: vec![]
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
