//! Which world you are in, and the machinery for changing your mind about it.
//!
//! There are exactly two modes: **offline** (no sim, no auth, a streamed basemap
//! and a local avatar) and **online** (a simulator reached over the network).
//! There used to be a third — *solo*, an in-process sim on loopback behind a
//! `solo` cargo feature — and it was removed on 2026-08-21 because three modes
//! were an architecture quiz nobody needed to sit: solo was reached through the
//! same authenticated connect path as any server, so it was "online to yourself"
//! wearing a mode of its own, plus a bundled SQLite that neither wasm nor
//! Android could compile and a feature flag every build lane had to remember.
//! A developer runs `cargo run -p cartopolis_simulator` and signs in to
//! `127.0.0.1:4747` like any other server.
//!
//! The sim is not a different application: it is offline plus an authority for
//! the avatar, the prims and the other people in it. Everything online is
//! already `Option<Res<OnlineSession>>`-guarded, so "offline" is precisely "that
//! resource is absent" — a state the app can enter and leave at runtime rather
//! than a launch-time fork. This module is that state machine, and
//! `systems::login_screen` is its face.
//!
//! **The request is a resource field, not an event**, because the sign-in screen
//! and startup both write it and neither can observe when the driver reads it;
//! `take()` in one place is easier to reason about than a message that may be read
//! twice or dropped. Requests are edge-triggered and idempotent: re-entering the
//! mode you are already in tears the session down and rebuilds it, which is what
//! "Reconnect" means.

use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::{Grid, GridCell};
use cartopolis_core::AuthCredential;
use serde::{Deserialize, Serialize};

use crate::components::{Avatar, Prim, RemoteAvatar};
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::resources::{
    AuthIdentity, AvatarState, ConnectAddr, ConnectAuth, GameState, LocalAvatarSimId, MapAnchor,
    NetworkMailbox, NetworkSyncState, OnlineSession, OsmTileUrlTemplate,
};
use crate::systems::geo_nav;
use crate::systems::network::{self, LinkState, LinkStatus};
use crate::systems::user_store::UserStore;

/// The persisted half of a session: enough to offer it again next launch, and
/// never the password (that is what the session token in `UserStore` is for).
///
/// Lives here rather than in `user_store` so the mode enum has one definition;
/// `UserData::session` holds it behind `#[serde(default)]`.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct SessionPrefs {
    pub kind: SessionKind,
    /// `host:port` of the last external sim, remembered so the field is prefilled.
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub username: String,
    /// Enter this mode on the next launch without asking. Default `true` against
    /// a default `kind` of [`SessionKind::Offline`], which is what makes a fresh
    /// install open straight onto the map: offline is where everybody starts, so
    /// there is nothing to ask about until somebody asks to sign in.
    pub auto_enter: bool,
}

impl Default for SessionPrefs {
    fn default() -> Self {
        Self {
            kind: SessionKind::Offline,
            server: String::new(),
            username: String::new(),
            auto_enter: true,
        }
    }
}

/// A mode without its address — the serialisable, comparable part of
/// [`SessionMode`].
///
/// The sign-in card offers neither of these as a *mode*: offline is where the
/// app starts and signing in is the one verb, so the card is a form for
/// [`Self::Remote`] and nothing else. It used to be a three-row picker, which
/// asked everybody to make an architecture decision before they had seen the
/// map.
///
/// `#[serde(other)]` on `Offline` is a file-format obligation, not a
/// convenience: a `user.json` written before 2026-08-21 may say `"solo"` (the
/// removed in-process-sim mode), and `UserData::session`'s `#[serde(default)]`
/// only covers a *missing* field — an unparseable one fails the whole document,
/// which quarantines the user's bookmarks over a mode that no longer exists.
/// An unknown kind is read as offline, which is also what a remembered solo
/// session should now mean.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// A simulator reached over the network.
    Remote,
    /// No sim at all: the streamed map and a local avatar, single user.
    /// Last, because serde requires the `other` fallback to be the last variant.
    #[default]
    #[serde(other)]
    Offline,
}

/// The mode the client is actually in.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum SessionMode {
    #[default]
    Offline,
    Remote {
        server: String,
    },
}

impl SessionMode {
    /// Where the session lives, for a status line.
    pub fn where_(&self) -> String {
        match self {
            Self::Offline => "no simulator".into(),
            Self::Remote { server } => server.clone(),
        }
    }
}

/// How far the current mode has got. `Offline` is a resting state, not a failure.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum SessionStatus {
    #[default]
    Offline,
    Connecting,
    Authenticating,
    Online,
}

impl SessionStatus {
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Connecting | Self::Authenticating)
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Offline => "Offline",
            Self::Connecting => "Connecting…",
            Self::Authenticating => "Signing in…",
            Self::Online => "Online",
        }
    }
}

/// What the sign-in screen (or startup) is asking for.
#[derive(Clone, Debug)]
pub enum SessionRequest {
    Offline,
    Remote {
        server: String,
        credential: AuthCredential,
        /// Keep the issued session token so this server does not ask again.
        persist_token: bool,
    },
}

#[derive(Resource, Default)]
pub struct SessionState {
    pub mode: SessionMode,
    pub status: SessionStatus,
    /// Why the last attempt ended, if it ended badly. Cleared when a new request
    /// is taken, so the sign-in screen never shows a stale reason beside a
    /// spinner.
    pub error: Option<String>,
    /// Written by the UI or startup, taken by [`drive_session`].
    pub request: Option<SessionRequest>,
    link: Option<LinkStatus>,
    /// A session that was lost rather than left, and is being got back.
    reconnect: Option<Reconnect>,
    /// Set on the frame a lost session comes back, and taken by
    /// [`keep_avatar_across_reconnect`] once it has put the avatar back.
    resumed: bool,
    /// Set when the app comes back to the foreground; [`drive_session`] turns it
    /// into a [`LinkProbe`] on its next run.
    probe_wanted: bool,
    probe: Option<LinkProbe>,
    /// `NetworkMailbox::received` and when it last moved, while online.
    heard: Option<(u64, f64)>,
    /// The app is in the background (`lifecycle`): the sim sends no snapshots
    /// then, so silence means nothing.
    backgrounded: bool,
}

/// A link being watched for silence after the app came back to the screen.
#[derive(Clone, Copy)]
struct LinkProbe {
    /// `Time<Real>` seconds at which silence counts as a dead link.
    deadline: f64,
    /// `NetworkMailbox::received` when the watch began.
    seen: u64,
}

/// How long a resumed link may say nothing before it is treated as lost. The sim
/// stops snapshots while the app is away and starts them again the moment
/// presence returns, at 20 Hz — so a live link is heard from within a tick or
/// two, and this is generous for a slow mobile round trip.
const RESUME_SILENCE_S: f64 = 6.0;

/// How long a link on screen may say nothing before it is treated as lost. The
/// sim sends a snapshot every tick to a client that is being looked at, so six
/// seconds is a hundred and twenty missed; without this a link that died in
/// the foreground (Wi-Fi to mobile data on the move) froze the client until the
/// TCP stack gave up, which can be minutes. The counter is the network
/// thread's, so a long frame here is not mistaken for silence there.
const FOREGROUND_SILENCE_S: f64 = 6.0;

/// Seconds to wait before each attempt to get a lost session back, counted from
/// the failure before it. The first is immediate because the common loss is the
/// phone's: Android takes the socket away while the app is in the background and
/// the client only finds out on the first frame back, when the network is
/// already up again. The rest cover a server restarting under a deploy. About
/// half a minute in all, and then the card's error line says why.
const RECONNECT_DELAYS_S: [f64; 6] = [0.0, 1.0, 2.0, 4.0, 8.0, 15.0];

/// Where a lost session was and how to sign in to it again.
#[derive(Clone)]
struct Reconnect {
    server: String,
    /// The credential the session was opened with. A remembered token is looked
    /// up again by [`connect`], exactly as for a fresh sign-in; a password typed
    /// this run is held here for as long as `ConnectAuth` already held it.
    auth: ConnectAuth,
    /// Attempts made since the session was lost.
    attempts: usize,
    /// When the next attempt is due, in `Time<Real>` seconds. `None` once they
    /// have run out — kept rather than dropped so [`SessionState::retry_now`] can
    /// start again when the app comes back to the foreground.
    due: Option<f64>,
}

impl SessionState {
    pub fn is_online(&self) -> bool {
        self.status == SessionStatus::Online
    }

    /// The app is back on the screen: check the connection still answers.
    ///
    /// A suspended app carries no traffic, and Android can take the network away
    /// without the socket ever reporting it — the link stays "online", nothing
    /// arrives and nothing is retried, which reads as a frozen client. A live
    /// link is heard from within a tick of presence returning; one that is not
    /// is dropped and re-established by the ordinary reconnect path.
    pub fn check_link_after_resume(&mut self) {
        if self.is_online() && self.link.is_some() {
            self.probe_wanted = true;
        }
    }

    /// Whether the app is off the screen, where the sim goes quiet on purpose.
    /// Coming back starts the silence count afresh; the resume probe covers
    /// the first seconds.
    pub fn set_backgrounded(&mut self, backgrounded: bool) {
        self.backgrounded = backgrounded;
        self.heard = None;
    }

    /// A session that was lost rather than left, and is being got back — the gap
    /// in which the client is offline only because the phone took the socket.
    pub fn is_reconnecting(&self) -> bool {
        self.reconnect.as_ref().is_some_and(|r| r.due.is_some())
    }

    /// Put the state a lost session leaves behind, for tests of what reads it.
    #[cfg(test)]
    pub fn lose_session_for_test(&mut self) {
        self.reconnect = Some(Reconnect {
            server: "127.0.0.1:1".into(),
            auth: ConnectAuth {
                credential: AuthCredential::Password {
                    username: "alice".into(),
                    password: String::new(),
                },
                persist_token: true,
            },
            attempts: 0,
            due: Some(0.0),
        });
        self.status = SessionStatus::Connecting;
    }

    /// The lost session came back: what `drive_session` leaves on that frame.
    #[cfg(test)]
    pub fn resume_for_test(&mut self) {
        self.reconnect = None;
        self.resumed = true;
        self.status = SessionStatus::Online;
    }

    /// Nothing lost and nothing being got back: a fresh sign-in is a fresh start.
    pub fn is_settled_offline(&self) -> bool {
        self.status == SessionStatus::Offline && self.reconnect.is_none()
    }

    /// Try to get a lost session back now rather than when its back-off says.
    ///
    /// Called when the app returns to the foreground: a phone that spent the
    /// back-off in somebody else's app may have used up every attempt against a
    /// network that was down, and coming back is exactly when to try again. A
    /// no-op with no lost session, or with an attempt already on the wire.
    pub fn retry_now(&mut self) {
        if self.link.is_some() {
            return;
        }
        if let Some(reconnect) = &mut self.reconnect {
            reconnect.attempts = 0;
            reconnect.due = Some(f64::NEG_INFINITY);
            self.mode = SessionMode::Remote {
                server: reconnect.server.clone(),
            };
            self.status = SessionStatus::Connecting;
            self.error = None;
        }
    }

    /// Ask for a mode. Safe to call every frame from a button — the driver takes
    /// the request on its next run.
    pub fn request(&mut self, request: SessionRequest) {
        self.request = Some(request);
    }
}

/// True when this build can reach a simulator at all.
///
/// Every build can, now. It was false on the web while that build had no
/// transport (ADR-031/033) — the sign-in screen's "Connect to a server" row was
/// greyed out with that as the reason. The browser has a WebSocket carrier since
/// this became true (`systems::ws_transport`), so the row is live everywhere.
///
/// Kept as a function rather than deleted along with its call sites: what it
/// answers is "can this build reach a sim", and the honest answer today is yes
/// everywhere. If a target ever appears where that stops being true, this is
/// where it says so — and the two call sites already read correctly.
pub const fn can_simulate() -> bool {
    true
}

/// Leave the world and go back to the offline map — the app menu's **Sign out**,
/// which is the only place it is offered. The sign-in card had one too, for the
/// case where a failure raised that card over a session that was still live; the
/// card is opened from this same menu now and closes the moment a session comes
/// up, so that case is gone and leaving is one entry with one outcome.
///
/// Three things happen together and it is the *together* that matters, which is
/// why they are here rather than at each call site. The remembered token for this
/// server is dropped (a log-out that leaves a token behind signs you straight back
/// in on the next launch, which is not a log-out); the remembered mode becomes
/// offline for the same reason; and only then is the session torn down. Written
/// as a free function rather than a method because it spans the two resources —
/// `SessionState` knows the server, `UserStore` holds the token.
pub fn sign_out(session: &mut SessionState, store: &mut UserStore) {
    if let SessionMode::Remote { server } = session.mode.clone() {
        store.clear_session_token(&server);
    }
    let prefs = SessionPrefs {
        kind: SessionKind::Offline,
        ..store.data.session.clone()
    };
    store.set_session_prefs(prefs);
    session.request(SessionRequest::Offline);
}

/// Apply session requests and watch a live link.
///
/// One system rather than two because both mutate the same state machine
/// and the ordering between them matters within a single frame: a request torn
/// down here must not be seen as a *dropped* link by the watcher below.
///
/// **A session that is lost rather than left comes back by itself.** Android
/// takes an app's socket away while it is in the background, so switching to
/// another app and back used to land the user offline with "Software caused
/// connection abort" on the card and the password field empty — a log-out
/// nobody asked for. A link that fails *after* it was online is therefore retried
/// on [`RECONNECT_DELAYS_S`] with the credential it was opened with, which rides
/// the remembered token like a launch does. Three things stop it: any request
/// (a log-out, or a sign-in somewhere else, wins), a server that refuses the
/// credential ([`LinkState::Rejected`] — asking again gets the same answer), and
/// running out of attempts. A first sign-in that fails is never retried; its
/// reason belongs on the card in front of the person who pressed the button.
#[allow(clippy::too_many_arguments)]
pub fn drive_session(
    mut commands: Commands,
    mut session: ResMut<SessionState>,
    store: Res<UserStore>,
    tile_url: Res<OsmTileUrlTemplate>,
    time: Res<Time<Real>>,
    auth: Option<Res<ConnectAuth>>,
    mailbox: Option<Res<NetworkMailbox>>,
    mut game_state: ResMut<GameState>,
    mut local_sim_id: ResMut<LocalAvatarSimId>,
    mut identity: ResMut<AuthIdentity>,
    mut chat: ResMut<crate::systems::chat::ChatState>,
    prims: Query<Entity, With<Prim>>,
    remotes: Query<Entity, With<RemoteAvatar>>,
    avatars: Query<Entity, With<Avatar>>,
) {
    if let Some(request) = session.request.take() {
        session.reconnect = None;
        session.resumed = false;
        session.probe = None;
        session.probe_wanted = false;
        session.heard = None;
        teardown(
            &mut commands,
            &mut session,
            &mut game_state,
            &mut local_sim_id,
            &mut identity,
            &mut chat,
            &prims,
            &remotes,
            &avatars,
        );
        session.error = None;
        match request {
            SessionRequest::Offline => {
                session.mode = SessionMode::Offline;
                session.status = SessionStatus::Offline;
                info!("session: offline");
            }
            SessionRequest::Remote {
                server,
                credential,
                persist_token,
            } => {
                let auth = ConnectAuth {
                    credential,
                    persist_token,
                };
                session.mode = SessionMode::Remote {
                    server: server.clone(),
                };
                connect(
                    &mut commands,
                    &mut session,
                    &store,
                    &tile_url,
                    &server,
                    auth,
                );
            }
        }
        return;
    }

    // Watch the live link. A drop is the same event as a failed sign-in as far as
    // the app is concerned: the authority is gone, so fall back to offline rather
    // than leaving a half-online client whose avatar no longer answers — and then,
    // if it was a session that got lost, go and get it back.
    let now = time.elapsed_secs_f64();
    let Some(link) = session.link.clone() else {
        // Between attempts: fire the next one when it is due.
        let due = |r: &mut Reconnect| r.due.is_some_and(|at| now >= at);
        if let Some(mut reconnect) = session.reconnect.take_if(due) {
            reconnect.attempts += 1;
            info!(
                server = %reconnect.server,
                attempt = reconnect.attempts,
                "session: reconnecting"
            );
            session.mode = SessionMode::Remote {
                server: reconnect.server.clone(),
            };
            let server = reconnect.server.clone();
            connect(
                &mut commands,
                &mut session,
                &store,
                &tile_url,
                &server,
                reconnect.auth.clone(),
            );
            session.reconnect = Some(reconnect);
        }
        return;
    };
    let ended = match link.get() {
        LinkState::Connecting => {
            session.status = SessionStatus::Connecting;
            return;
        }
        LinkState::Authenticating => {
            session.status = SessionStatus::Authenticating;
            return;
        }
        LinkState::Online => {
            session.status = SessionStatus::Online;
            if session.reconnect.take().is_some() {
                info!(server = %session.mode.where_(), "session: reconnected");
                session.resumed = true;
            }
            let received = mailbox
                .as_ref()
                .map_or(0, |m| m.received.load(std::sync::atomic::Ordering::Relaxed));
            match session.heard {
                Some((seen, _)) if seen == received => {}
                _ => session.heard = Some((received, now)),
            }
            // On screen, the sim is heard from every tick; the resume probe has
            // the first seconds after coming back.
            let silent_on_screen = !session.backgrounded
                && session.probe.is_none()
                && !session.probe_wanted
                && session
                    .heard
                    .is_some_and(|(_, since)| now - since >= FOREGROUND_SILENCE_S);
            if silent_on_screen {
                warn!(server = %session.mode.where_(), "session: silent on screen, reconnecting");
            } else {
                if std::mem::take(&mut session.probe_wanted) {
                    session.probe = Some(LinkProbe {
                        deadline: now + RESUME_SILENCE_S,
                        seen: received,
                    });
                }
                match session.probe {
                    Some(p) if received != p.seen => {
                        session.probe = None;
                        return;
                    }
                    Some(p) if now >= p.deadline => {
                        warn!(server = %session.mode.where_(), "session: silent since resume, reconnecting");
                        session.probe = None;
                    }
                    _ => return,
                }
            }
            session.heard = None;
            Some("connection went silent".to_string())
        }
        LinkState::Rejected(message) => {
            // Refused: the token has expired or the account is gone, and trying
            // again gets the same answer. Offline, with the server's reason.
            warn!(%message, "session refused");
            session.reconnect = None;
            Some(message)
        }
        LinkState::Failed(message) => {
            warn!(%message, "session failed");
            Some(message)
        }
        LinkState::Closed => None,
    };

    // Lost — a session that was up, or an attempt to bring one back — as opposed
    // to a sign-in that never got there.
    let lost = session.status == SessionStatus::Online || session.reconnect.is_some();
    let was = session.mode.where_();
    let server = match &session.mode {
        SessionMode::Remote { server } => Some(server.clone()),
        SessionMode::Offline => None,
    };
    let resume_with = auth
        .as_deref()
        .cloned()
        .or_else(|| session.reconnect.as_ref().map(|r| r.auth.clone()));
    // The conversation is kept across a reconnect: the same people on the same
    // server, and losing it to the phone having switched apps would be the same
    // surprise as the log-out this avoids. `teardown` still clears it when the
    // session is not coming back.
    let kept_chat = std::mem::take(&mut *chat);
    teardown(
        &mut commands,
        &mut session,
        &mut game_state,
        &mut local_sim_id,
        &mut identity,
        &mut chat,
        &prims,
        &remotes,
        &avatars,
    );
    if lost && schedule_reconnect(&mut session, server, resume_with, now) {
        *chat = kept_chat;
        return;
    }
    session.error = match ended {
        Some(message) => Some(format!("{was}: {message}")),
        // Only worth reporting if there was a session to lose; a link closed
        // before it came up already failed for a better-stated reason.
        None if lost => Some(format!("Disconnected from {was}")),
        None => None,
    };
}

/// Arrange the next attempt to get a lost session back, or record that they have
/// run out. Returns whether one is coming.
///
/// The client reads as *connecting* while it waits rather than offline, because
/// it is: the status line and the splash say "Connecting…" instead of flashing
/// "Offline" between attempts, and a sign-in card opened meanwhile holds its
/// button until the outcome is known.
fn schedule_reconnect(
    session: &mut SessionState,
    server: Option<String>,
    auth: Option<ConnectAuth>,
    now: f64,
) -> bool {
    // Rejected clears `reconnect`, so an attempt count here always belongs to a
    // lost session, never to a refused one.
    let (Some(server), Some(auth)) = (server, auth) else {
        session.reconnect = None;
        return false;
    };
    let attempts = session.reconnect.as_ref().map_or(0, |r| r.attempts);
    // ±25 %, so the clients a sim restart dropped together do not all knock
    // again on the same second. The spread comes from the clock's sub-millisecond
    // digits, which differ per client; nothing here needs real randomness.
    let jitter = 0.75 + 0.5 * (now * 1000.0).fract();
    let due = RECONNECT_DELAYS_S
        .get(attempts)
        .map(|delay| now + delay * jitter);
    session.reconnect = Some(Reconnect {
        server: server.clone(),
        auth,
        attempts,
        due,
    });
    if due.is_none() {
        warn!(%server, attempts, "session: giving up on reconnecting");
        return false;
    }
    session.mode = SessionMode::Remote { server };
    session.status = SessionStatus::Connecting;
    true
}

/// **A reconnect puts the avatar back where it was.** A lost session is torn down
/// like any other, so the avatar is despawned with it, and the sim that takes the
/// reconnect spawns a *new* avatar at the origin of its flat frame — which is the
/// map anchor, i.e. wherever the last long jump or pan at altitude left it.
/// Switching to another app on the phone and back therefore dropped the avatar
/// at that spot rather than where it had walked to.
///
/// So while a session has an avatar this remembers where it stands — as a
/// latitude and longitude, because the map may re-anchor while the client waits
/// to reconnect and a position in the old frame would then be the wrong place —
/// and on the frame a lost session comes back it places the avatar there and
/// tells the sim, through the same [`geo_nav::place_avatar`] a teleport uses.
/// The camera is left alone: the view the person switched away from is still
/// the view they come back to.
///
/// **Every other sign-in puts it where the server says it was left**
/// (`ServerHelloAck::last_place`, kept per account), so signing out and in
/// again, opening the app tomorrow or signing in on another device all find the
/// avatar where it was, not on whatever the map is anchored to. A server with no
/// place for the account falls back to the launch's `user.json` (`last_avatar`).
/// Within [`geo_nav::SHORT_JUMP_M`] of the anchor the avatar is placed in the
/// frame and the camera stays; further, it goes there by a silent [`Teleport`],
/// which brings the camera and re-anchors the map, because an avatar kilometres
/// out of its frame is the one thing the flat frame cannot hold
/// (`docs/notes/device-location.md`).
///
/// [`geo_nav::place_avatar`]: crate::systems::geo_nav::place_avatar
/// [`Teleport`]: crate::systems::geo_nav::Teleport
#[allow(clippy::too_many_arguments)]
pub fn keep_avatar_across_reconnect(
    mut session: ResMut<SessionState>,
    anchor: Res<MapAnchor>,
    online: Option<Res<OnlineSession>>,
    mut store: ResMut<UserStore>,
    mut identity: ResMut<AuthIdentity>,
    mut nav: ResMut<geo_nav::GeoNavState>,
    mut avatar_state: ResMut<AvatarState>,
    mut avatars: Query<(&mut GridCell, &mut Transform), With<Avatar>>,
    mut last: Local<Option<(f64, f64, f64)>>,
    mut launch_restore_done: Local<bool>,
) {
    let (Some(anchor), Ok((mut cell, mut tf))) = (anchor.geo, avatars.single_mut()) else {
        // No avatar: nothing to watch, and a resume waits for the one
        // `sync_local_avatar` is about to spawn.
        return;
    };
    let edge = f64::from(WORLD_CELL_EDGE);
    // What this sign-in should come back to, if anything: the server's word
    // first, then (once a launch) the device's own.
    let arrival = if session.resumed || !session.is_online() {
        None
    } else if let Some(g) = identity.arrival.take() {
        *launch_restore_done = true;
        Some((g.latitude, g.longitude, 0.0))
    } else if !*launch_restore_done {
        *launch_restore_done = true;
        store
            .data
            .last_avatar
            .map(|a| (a.lat, a.lng, if a.y.is_finite() { a.y } else { 0.0 }))
    } else {
        None
    };
    if let (Some((lat, lng, y)), Some(sess)) = (arrival, online.as_deref()) {
        if lat.is_finite() && lng.is_finite() && lat.abs() <= 90.0 && lng.abs() <= 180.0 {
            let (wx, wz) = crate::grid::lat_lng_to_world(anchor, lat, lng);
            if wx.hypot(wz) < geo_nav::SHORT_JUMP_M {
                let (to_cell, to_local) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
                    .translation_to_grid(DVec3::new(wx, y.clamp(0.0, 10_000.0), wz));
                geo_nav::place_avatar(
                    &mut cell,
                    &mut tf,
                    &mut avatar_state,
                    Some(sess),
                    anchor,
                    to_cell,
                    to_local,
                );
            } else {
                nav.pending_teleport = Some(geo_nav::Teleport::geo(lat, lng, None).silently());
            }
            info!(lat, lng, "session: avatar put back where it was left");
        }
    }
    if session.resumed {
        session.resumed = false;
        *launch_restore_done = true;
        // The reconnect's own handshake names the place the lost session was
        // left, which is this one at best and a stale save at worst.
        identity.arrival = None;
        if let (Some((lat, lng, y)), Some(sess)) = (*last, online.as_deref()) {
            let (wx, wz) = crate::grid::lat_lng_to_world(anchor, lat, lng);
            let (to_cell, to_local) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
                .translation_to_grid(DVec3::new(wx, y, wz));
            geo_nav::place_avatar(
                &mut cell,
                &mut tf,
                &mut avatar_state,
                Some(sess),
                anchor,
                to_cell,
                to_local,
            );
            info!(lat, lng, "session: avatar put back where it was");
        }
    }
    if !session.is_online() {
        return;
    }
    // `AvatarState::position` is relative to the avatar's cell, which is what
    // `place_avatar` writes; the cell is added back so a position past the first
    // cell's edge is not remembered 10 km out.
    let p = avatar_state.position;
    let wx = cell.x as f64 * edge + f64::from(p.x);
    let wz = cell.z as f64 * edge + f64::from(p.z);
    let (lat, lng) = crate::grid::world_to_lat_lng(anchor, wx, wz);
    *last = Some((lat, lng, f64::from(p.y)));
    // Compared first so a standing avatar does not mark the store changed every
    // frame; the autosave writes it with everything else.
    let here = crate::systems::user_store::SavedAvatar {
        lat,
        lng,
        y: f64::from(p.y),
    };
    if store.data.last_avatar != Some(here) {
        store.data.last_avatar = Some(here);
    }
}

/// Open the link and install the resources that make the rest of the client
/// online. Everything here is the inverse of [`teardown`]; keep them in step.
fn connect(
    commands: &mut Commands,
    session: &mut SessionState,
    store: &UserStore,
    tile_url: &OsmTileUrlTemplate,
    addr: &str,
    auth: ConnectAuth,
) {
    let token = auth
        .persist_token
        .then(|| store.session_token(addr))
        .flatten();
    let link = network::start_link(addr, &auth, token, tile_url.0.clone());
    session.status = SessionStatus::Connecting;
    session.link = Some(link.status);
    commands.insert_resource(ConnectAddr(addr.to_string()));
    commands.insert_resource(auth);
    commands.insert_resource(link.mailbox);
    commands.insert_resource(link.session);
    commands.insert_resource(NetworkSyncState::default());
}

/// Return the client to the offline world: drop the link, forget who we were, and
/// despawn everything the sim owned.
///
/// Dropping [`OnlineSession`] is what actually closes the socket — the network
/// thread's `select!` sees the intent channel end and leaves its loop — so it must
/// go even when the link already failed, or a reconnect would leave the old thread
/// holding a socket.
#[allow(clippy::too_many_arguments)]
fn teardown(
    commands: &mut Commands,
    session: &mut SessionState,
    game_state: &mut GameState,
    local_sim_id: &mut LocalAvatarSimId,
    identity: &mut AuthIdentity,
    chat: &mut crate::systems::chat::ChatState,
    prims: &Query<Entity, With<Prim>>,
    remotes: &Query<Entity, With<RemoteAvatar>>,
    avatars: &Query<Entity, With<Avatar>>,
) {
    session.link = None;
    session.probe = None;
    session.probe_wanted = false;
    session.heard = None;
    session.mode = SessionMode::Offline;
    session.status = SessionStatus::Offline;

    // Paired with the `set_endpoint` in the handshake, exactly as this function is
    // paired with `connect`: a token left pointing at a server nobody is signed in
    // to is a fetch that answers 401 forever, and the models it would fetch belong
    // to a world that is no longer on screen.
    crate::systems::model_assets::clear_endpoint();
    crate::services::clear();
    // And the panel's copy of the lists, for the same reason: an inventory
    // belongs to an account on a server, and showing the last server's after
    // signing in to a different one is worse than showing nothing.
    commands.insert_resource(crate::systems::model_library::ModelLibraryState::default());

    commands.remove_resource::<OnlineSession>();
    commands.remove_resource::<NetworkMailbox>();
    commands.remove_resource::<NetworkSyncState>();
    commands.remove_resource::<ConnectAddr>();
    commands.remove_resource::<ConnectAuth>();
    // Reset rather than removed: `avatar::smooth_online_avatar_display` takes it
    // as a plain `ResMut` on every run mode, and the figures describe one
    // connection — carrying a stall count into the next session would blame the
    // new server for the old one's link.
    commands.insert_resource(crate::resources::NetPredictionHealth::default());

    *identity = AuthIdentity::default();
    *local_sim_id = LocalAvatarSimId(None);
    // The chat log belongs to the world it was said in; carrying it into the next
    // one would attribute strangers' lines to a server they never spoke on.
    *chat = crate::systems::chat::ChatState::default();
    // Other avatars' looks are keyed by that server's avatar ids, which the
    // next session hands out afresh.
    commands.insert_resource(crate::systems::human_avatar::RemoteLooks::default());
    game_state.prims_loaded = false;
    game_state.selected_prim_id = None;

    // **The local avatar goes too.** It used to be spawned at startup and never
    // despawned, so leaving a session left you standing on the map as a body with
    // no sim behind it: nothing owned its position, nothing else could see it,
    // and it was the one thing on screen still claiming there was a world to be
    // in. Offline is the map and the camera. `local_avatar::sync_local_avatar` is the
    // counterpart that brings it back when a session does, and it is also what a
    // scripted capture asks for when it is photographing the avatar.
    //
    // `try_despawn`: `network::apply_network_snapshot` retires the same entities
    // from a snapshot backlog, and nothing orders the two systems, so on the frame
    // a session drops they can both queue a despawn for one entity.
    for e in prims.iter().chain(remotes.iter()).chain(avatars.iter()) {
        commands.entity(e).try_despawn();
    }
}

#[cfg(test)]
mod tests;
