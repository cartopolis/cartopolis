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

/// Leave the world and go back to the offline map — the app menu's **Log out**,
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
    let due = RECONNECT_DELAYS_S.get(attempts).map(|delay| now + delay);
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
/// map anchor, i.e. wherever the last teleport landed. Switching to another app
/// on the phone and back therefore dropped the avatar at that spot rather than
/// where it had walked to.
///
/// So while a session has an avatar this remembers where it stands — as a
/// latitude and longitude, because the map may re-anchor while the client waits
/// to reconnect and a position in the old frame would then be the wrong place —
/// and on the frame a lost session comes back it places the avatar there and
/// tells the sim, through the same [`geo_nav::place_avatar`] a teleport uses.
/// The camera is left alone: the view the person switched away from is still
/// the view they come back to.
///
/// **The launch's first sign-in does the same from `user.json`** (`last_avatar`),
/// so closing the app and opening it again finds the avatar where it was left
/// rather than on the anchor's tile centre. Only while that spot is within
/// [`geo_nav::SHORT_JUMP_M`] of the anchor: the restored camera re-anchored the
/// map around it, and anything further means this launch deliberately started
/// somewhere else (a place link, or a jump made before the session came up) —
/// the avatar then starts there, where the camera is. A sign-in later in the
/// same run starts where the sim puts them, as it always has.
///
/// [`geo_nav::place_avatar`]: crate::systems::geo_nav::place_avatar
pub fn keep_avatar_across_reconnect(
    mut session: ResMut<SessionState>,
    anchor: Res<MapAnchor>,
    online: Option<Res<OnlineSession>>,
    mut store: ResMut<UserStore>,
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
    if !session.resumed && !*launch_restore_done && session.is_online() {
        *launch_restore_done = true;
        let saved = store.data.last_avatar.filter(|a| {
            let (wx, wz) = geo_nav::lat_lng_to_world(anchor, a.lat, a.lng);
            a.lat.is_finite() && a.lng.is_finite() && wx.hypot(wz) < geo_nav::SHORT_JUMP_M
        });
        if let (Some(a), Some(sess)) = (saved, online.as_deref()) {
            let y = if a.y.is_finite() {
                a.y.clamp(0.0, 10_000.0)
            } else {
                0.0
            };
            let (wx, wz) = geo_nav::lat_lng_to_world(anchor, a.lat, a.lng);
            let (to_cell, to_local) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
                .translation_to_grid(DVec3::new(wx, y, wz));
            geo_nav::place_avatar(
                &mut cell,
                &mut tf,
                &mut avatar_state,
                Some(sess),
                to_cell,
                to_local,
            );
            info!(
                lat = a.lat,
                lng = a.lng,
                "session: avatar put back where it was last launch"
            );
        }
    }
    if session.resumed {
        session.resumed = false;
        *launch_restore_done = true;
        if let (Some((lat, lng, y)), Some(sess)) = (*last, online.as_deref()) {
            let (wx, wz) = geo_nav::lat_lng_to_world(anchor, lat, lng);
            let (to_cell, to_local) = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
                .translation_to_grid(DVec3::new(wx, y, wz));
            geo_nav::place_avatar(
                &mut cell,
                &mut tf,
                &mut avatar_state,
                Some(sess),
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
    let (lat, lng) = geo_nav::world_to_lat_lng(anchor, wx, wz);
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
    session.mode = SessionMode::Offline;
    session.status = SessionStatus::Offline;

    // Paired with the `set_endpoint` in the handshake, exactly as this function is
    // paired with `connect`: a token left pointing at a server nobody is signed in
    // to is a fetch that answers 401 forever, and the models it would fetch belong
    // to a world that is no longer on screen.
    crate::systems::model_assets::clear_endpoint();
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
    game_state.prims_loaded = false;
    game_state.selected_prim_id = None;

    // **The local avatar goes too.** It used to be spawned at startup and never
    // despawned, so leaving a session left you standing on the map as a body with
    // no sim behind it: nothing owned its position, nothing else could see it,
    // and it was the one thing on screen still claiming there was a world to be
    // in. Offline is the map and the camera. `lib::sync_local_avatar` is the
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
mod tests {
    use super::*;

    /// An app carrying only what [`drive_session`] touches. Enough to exercise the
    /// state machine without a renderer, a window or a sim.
    fn test_app() -> App {
        let mut app = App::new();
        app.init_resource::<SessionState>()
            .init_resource::<Time<Real>>()
            .init_resource::<GameState>()
            .init_resource::<LocalAvatarSimId>()
            .init_resource::<AuthIdentity>()
            .init_resource::<crate::systems::chat::ChatState>()
            .init_resource::<OsmTileUrlTemplate>()
            .insert_resource(UserStore::load())
            .add_systems(Update, drive_session);
        app
    }

    /// A session that is up, over a link this test controls instead of a
    /// network thread. Returns the link so the test can end it.
    fn online_over_fake_link(app: &mut App, server: &str) -> LinkStatus {
        let link = LinkStatus::new();
        link.set(LinkState::Online);
        app.insert_resource(ConnectAuth {
            credential: AuthCredential::Password {
                username: "alice".into(),
                password: String::new(),
            },
            persist_token: true,
        });
        let mut session = app.world_mut().resource_mut::<SessionState>();
        session.mode = SessionMode::Remote {
            server: server.into(),
        };
        session.status = SessionStatus::Online;
        session.link = Some(link.clone());
        link
    }

    /// **The bug this was written for**: Android takes the socket away while the
    /// app is in the background, and the first frame back sees the link fail.
    /// That used to be the end of the session — offline, the error on the card,
    /// the password field empty. It must read as a reconnect instead, with
    /// nothing put in front of the user.
    #[test]
    fn a_lost_session_is_got_back_rather_than_dropped() {
        let mut app = test_app();
        let link = online_over_fake_link(&mut app, "127.0.0.1:1");
        link.set(LinkState::Failed(
            "IO error: Software caused connection abort (os error 103)".into(),
        ));
        app.update();

        let session = app.world().resource::<SessionState>();
        assert_eq!(
            session.error, None,
            "a reconnect is not a failure to report"
        );
        assert_eq!(session.status, SessionStatus::Connecting);
        assert_eq!(
            session.mode,
            SessionMode::Remote {
                server: "127.0.0.1:1".into()
            }
        );
        let reconnect = session.reconnect.as_ref().expect("no reconnect scheduled");
        assert_eq!(reconnect.attempts, 0);

        // The first attempt is immediate: the network is back by the time the
        // client notices it was gone.
        app.update();
        let session = app.world().resource::<SessionState>();
        assert!(session.link.is_some(), "the first attempt did not go out");
        assert_eq!(session.reconnect.as_ref().map(|r| r.attempts), Some(1));
    }

    /// A link that closes cleanly under a live session — a server restarting
    /// for a deploy — is a loss too, not a log-out.
    #[test]
    fn a_closed_link_is_got_back_too() {
        let mut app = test_app();
        let link = online_over_fake_link(&mut app, "127.0.0.1:1");
        link.set(LinkState::Closed);
        app.update();
        let session = app.world().resource::<SessionState>();
        assert!(session.reconnect.is_some());
        assert_eq!(session.error, None);
    }

    /// A first sign-in that fails is reported, never retried: the reason
    /// belongs on the card in front of the person who pressed the button.
    #[test]
    fn a_failed_first_sign_in_is_not_retried() {
        let mut app = test_app();
        let link = LinkStatus::new();
        {
            let mut session = app.world_mut().resource_mut::<SessionState>();
            session.mode = SessionMode::Remote {
                server: "sim.example".into(),
            };
            session.status = SessionStatus::Connecting;
            session.link = Some(link.clone());
        }
        link.set(LinkState::Failed("No simulator is listening there".into()));
        app.update();
        let session = app.world().resource::<SessionState>();
        assert!(session.reconnect.is_none());
        assert_eq!(session.status, SessionStatus::Offline);
        assert_eq!(
            session.error.as_deref(),
            Some("sim.example: No simulator is listening there")
        );
    }

    /// A refused credential ends the reconnect: asking again gets the same
    /// answer, and the reason is what the user needs to see.
    #[test]
    fn a_refused_credential_stops_the_reconnect() {
        let mut app = test_app();
        let link = online_over_fake_link(&mut app, "127.0.0.1:1");
        link.set(LinkState::Failed("connection reset".into()));
        app.update();
        app.update(); // the first attempt goes out
                      // Swapped for a fake: the real one is a thread dialling a closed port,
                      // which would race the refusal this test sets.
        let attempt = LinkStatus::new();
        app.world_mut().resource_mut::<SessionState>().link = Some(attempt.clone());
        attempt.set(LinkState::Rejected("session expired".into()));
        app.update();

        let session = app.world().resource::<SessionState>();
        assert!(session.reconnect.is_none(), "a refusal was retried");
        assert_eq!(session.status, SessionStatus::Offline);
        assert_eq!(
            session.error.as_deref(),
            Some("127.0.0.1:1: session expired")
        );
    }

    /// The back-off runs out, the reason is shown — and coming back to the app
    /// starts it again, because a phone may have spent every attempt against a
    /// network that was down while it was in the background.
    #[test]
    fn a_reconnect_gives_up_and_the_foreground_rearms_it() {
        let mut app = test_app();
        let link = online_over_fake_link(&mut app, "127.0.0.1:1");
        link.set(LinkState::Failed("connection reset".into()));
        app.update();
        // Pretend every attempt has already been spent.
        let spent = RECONNECT_DELAYS_S.len();
        let attempt = LinkStatus::new();
        {
            let mut session = app.world_mut().resource_mut::<SessionState>();
            session.reconnect.as_mut().unwrap().attempts = spent;
            session.link = Some(attempt.clone());
        }
        attempt.set(LinkState::Failed("network unreachable".into()));
        app.update();

        let session = app.world().resource::<SessionState>();
        assert_eq!(session.status, SessionStatus::Offline);
        assert_eq!(
            session.error.as_deref(),
            Some("127.0.0.1:1: network unreachable")
        );
        assert_eq!(session.reconnect.as_ref().and_then(|r| r.due), None);

        app.world_mut().resource_mut::<SessionState>().retry_now();
        app.update();
        let session = app.world().resource::<SessionState>();
        assert!(session.link.is_some(), "the foreground did not try again");
        assert_eq!(session.error, None);
        assert_eq!(session.reconnect.as_ref().map(|r| r.attempts), Some(1));
    }

    /// Logging out in the middle of a reconnect ends it: a request always wins.
    #[test]
    fn a_log_out_cancels_a_reconnect() {
        let mut app = test_app();
        let link = online_over_fake_link(&mut app, "127.0.0.1:1");
        link.set(LinkState::Closed);
        app.update();
        app.world_mut()
            .resource_mut::<SessionState>()
            .request(SessionRequest::Offline);
        app.update();
        app.update();
        let session = app.world().resource::<SessionState>();
        assert!(session.reconnect.is_none());
        assert!(session.link.is_none());
        assert_eq!(session.status, SessionStatus::Offline);
    }

    /// Pretend a session is up: the resources a live link installs, plus the
    /// local avatar and one of somebody else's.
    fn go_online(app: &mut App) {
        let (intent_tx, _rx) = futures_channel::mpsc::unbounded();
        let (_tx, out_rx) = std::sync::mpsc::channel();
        app.insert_resource(OnlineSession { intent_tx });
        app.insert_resource(NetworkMailbox::new(out_rx));
        app.insert_resource(NetworkSyncState {
            received_initial_world: true,
        });
        app.insert_resource(ConnectAddr("sim.example:4747".into()));
        app.world_mut().resource_mut::<AuthIdentity>().username = Some("alice".into());
        app.world_mut().spawn(Avatar);
        app.world_mut().spawn(RemoteAvatar {
            sim_id: 7,
            net_position: Vec3::ZERO,
            net_yaw: 0.0,
            net_velocity: Vec3::ZERO,
            net_age: 0.0,
            pending_emote: None,
            last_emote_sequence: None,
            seated: None,
            seat_blend: 0.0,
        });
    }

    /// Going offline has to leave *no* trace of the sim: the online resources are
    /// what every guarded system keys off, and a stray `OnlineSession` would leave
    /// the avatar taking orders from a socket that is gone.
    #[test]
    fn going_offline_removes_every_trace_of_the_session() {
        let mut app = test_app();
        go_online(&mut app);
        app.world_mut()
            .resource_mut::<SessionState>()
            .request(SessionRequest::Offline);
        app.update();

        let world = app.world_mut();
        assert!(world.get_resource::<OnlineSession>().is_none());
        assert!(world.get_resource::<NetworkMailbox>().is_none());
        assert!(world.get_resource::<NetworkSyncState>().is_none());
        assert!(world.get_resource::<ConnectAddr>().is_none());
        assert_eq!(world.resource::<AuthIdentity>().username, None);
        assert_eq!(
            world.query::<&RemoteAvatar>().iter(world).count(),
            0,
            "other people outlived the session"
        );
        assert_eq!(
            world.query::<&Avatar>().iter(world).count(),
            0,
            "the local avatar outlived the session it belonged to"
        );
        let session = world.resource::<SessionState>();
        assert_eq!(session.mode, SessionMode::Offline);
        assert_eq!(session.status, SessionStatus::Offline);
        assert!(session.request.is_none(), "the request was consumed");
    }

    /// The request is edge-triggered: one apply, not one per frame.
    #[test]
    fn a_request_is_taken_once() {
        let mut app = test_app();
        app.world_mut()
            .resource_mut::<SessionState>()
            .request(SessionRequest::Offline);
        app.update();
        app.update();
        assert!(app.world().resource::<SessionState>().request.is_none());
    }

    /// **A fresh install opens onto the map and asks nothing.** It used to boot
    /// into solo — the behaviour the client had before there was a sign-in screen
    /// — which meant a first launch silently started a database-backed simulator
    /// for somebody who had come to look at a map. `auto_enter` stays true, and on
    /// an offline kind that means "enter offline without asking", which is the
    /// only reading of it that leaves the sign-in card closed.
    #[test]
    fn default_prefs_boot_offline_without_asking() {
        let prefs = SessionPrefs::default();
        assert_eq!(prefs.kind, SessionKind::Offline);
        assert!(prefs.auto_enter);
    }

    /// The kind is stored in `user.json`, so its spelling is a file format.
    #[test]
    fn session_kind_round_trips_as_snake_case() {
        for (kind, text) in [
            (SessionKind::Offline, "\"offline\""),
            (SessionKind::Remote, "\"remote\""),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), text);
            assert_eq!(
                serde_json::from_str::<SessionKind>(text).unwrap(),
                kind,
                "{text} did not round-trip"
            );
        }
    }

    /// A `user.json` written before 2026-08-21 may remember the removed solo
    /// mode. That must read as offline — an unparseable session field fails the
    /// whole document, and the document is the user's bookmarks.
    #[test]
    fn a_remembered_solo_session_degrades_to_offline() {
        assert_eq!(
            serde_json::from_str::<SessionKind>("\"solo\"").unwrap(),
            SessionKind::Offline
        );
    }

    #[test]
    fn mode_reports_where_it_is() {
        let remote = SessionMode::Remote {
            server: "sim.example:4747".into(),
        };
        assert_eq!(remote.where_(), "sim.example:4747");
        assert_eq!(SessionMode::Offline.where_(), "no simulator");
    }

    /// **Logging out has to drop the token as well as the session**, or the next
    /// launch signs itself straight back in with it — which is not a log-out, it
    /// is a reconnect with an extra step.
    #[test]
    fn logging_out_forgets_the_session_it_was_in() {
        let mut store = UserStore::default();
        store.set_session_prefs(SessionPrefs {
            kind: SessionKind::Remote,
            server: "sim.example".into(),
            username: "alice".into(),
            auto_enter: true,
        });
        store.set_session_token("sim.example", "a-token");
        let mut session = SessionState {
            mode: SessionMode::Remote {
                server: "sim.example".into(),
            },
            status: SessionStatus::Online,
            ..SessionState::default()
        };

        sign_out(&mut session, &mut store);

        assert!(matches!(session.request, Some(SessionRequest::Offline)));
        assert_eq!(store.data.session.kind, SessionKind::Offline);
        assert_eq!(store.session_token("sim.example"), None);
        // The address and the name stay: the sign-in card comes back prefilled,
        // which is the whole reason they are remembered separately from the token.
        assert_eq!(store.data.session.server, "sim.example");
        assert_eq!(store.data.session.username, "alice");
    }

    #[test]
    fn busy_statuses_are_the_ones_with_a_spinner() {
        assert!(SessionStatus::Connecting.is_busy());
        assert!(SessionStatus::Authenticating.is_busy());
        assert!(!SessionStatus::Online.is_busy());
        assert!(!SessionStatus::Offline.is_busy());
    }

    /// The client world as `lib.rs` assembles it, reduced to the systems and
    /// resources the session + snapshot path touches. Fresh per "launch".
    fn client_app(store: UserStore) -> App {
        use crate::resources::{AvatarState, PrimTextureCache, TextureLibrary};
        use bevy::asset::Assets;
        use bevy::image::Image;

        let mut app = App::new();
        app.init_resource::<SessionState>()
            .init_resource::<Time<Real>>()
            .init_resource::<GameState>()
            .init_resource::<LocalAvatarSimId>()
            .init_resource::<AuthIdentity>()
            .init_resource::<AvatarState>()
            .init_resource::<TextureLibrary>()
            .init_resource::<PrimTextureCache>()
            .init_resource::<crate::systems::chat::ChatState>()
            .init_resource::<crate::systems::script_editor::ScriptEditorState>()
            .init_resource::<crate::systems::voice::VoiceState>()
            .init_resource::<crate::systems::private_chat::PrivateInbox>()
            .init_resource::<crate::systems::calls::CallInbox>()
            .init_resource::<OsmTileUrlTemplate>()
            .insert_resource(Assets::<Image>::default())
            .insert_resource(store)
            .add_systems(
                Update,
                (
                    drive_session,
                    crate::systems::network::apply_network_snapshot,
                )
                    .chain(),
            );
        app
    }

    /// Pump frames (the link runs on its own thread) until `done`, or fail
    /// with whatever the session has to say for itself. The real clock is
    /// advanced by hand, since there is no `TimePlugin` to do it.
    fn pump(app: &mut App, what: &str, done: impl Fn(&World) -> bool) {
        for _ in 0..1000 {
            app.world_mut().resource_mut::<Time<Real>>().update();
            app.update();
            if done(app.world()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let session = app.world().resource::<SessionState>();
        panic!(
            "timed out waiting for {what}: status {:?}, error {:?}",
            session.status, session.error
        );
    }

    /// A real simulator on loopback with one account, `alice` / `hunter22`, on
    /// `listen` (`127.0.0.1:0` for any port). The runtime is returned because
    /// dropping it is what takes the server down, connections and all.
    fn boot_sim(
        db: &str,
        listen: &str,
    ) -> (tokio::runtime::Runtime, cartopolis_simulator::SimHandle) {
        {
            let conn = cartopolis_simulator::db::open_and_migrate(db).unwrap();
            cartopolis_simulator::ensure_account(&conn, "alice", "hunter22").unwrap();
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        let handle = rt
            .block_on(cartopolis_simulator::serve(std::sync::Arc::new(
                cartopolis_simulator::SimConfig {
                    database_path: db.to_string(),
                    listen: listen.to_string(),
                    ..cartopolis_simulator::SimConfig::default()
                },
            )))
            .unwrap();
        (rt, handle)
    }

    /// **"Stay signed in", end to end, across a viewer restart.** A real sim on
    /// loopback, the real `drive_session` + `apply_network_snapshot` systems, a
    /// real `UserStore` — sign in with a password once, then rebuild the whole
    /// client world (a restart) carrying only what `user.json` would carry, and
    /// require the remembered session to come up without the password. This is
    /// the exact loop a user reported broken on 2026-08-21, and every unit here
    /// read as correct on paper; only the assembled path can say so.
    #[test]
    fn stay_signed_in_survives_a_viewer_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("world.db").to_str().unwrap().to_string();
        let (_rt, handle) = boot_sim(&db, "127.0.0.1:0");
        let addr = handle.local_addr.to_string();

        // Launch one: the sign-in card's submit, "stay signed in" ticked.
        let mut app = client_app(UserStore::default());
        app.world_mut()
            .resource_mut::<SessionState>()
            .request(SessionRequest::Remote {
                server: addr.clone(),
                credential: AuthCredential::Password {
                    username: "alice".into(),
                    password: "hunter22".into(),
                },
                persist_token: true,
            });
        pump(&mut app, "the password sign-in", |w| {
            w.resource::<SessionState>().is_online()
        });
        // The ack's token has to land in the store under the address the next
        // launch will look it up by.
        pump(&mut app, "the token to be persisted", |w| {
            w.resource::<UserStore>().session_token(&addr).is_some()
        });

        // The restart: a new world, carrying only what user.json holds. The
        // request is verbatim what `lib.rs` builds from a remembered session —
        // the username with no password, riding the persisted token.
        let mut store = UserStore::default();
        store.data.session_tokens = app
            .world()
            .resource::<UserStore>()
            .data
            .session_tokens
            .clone();
        drop(app);
        let mut relaunch = client_app(store);
        relaunch
            .world_mut()
            .resource_mut::<SessionState>()
            .request(SessionRequest::Remote {
                server: addr.clone(),
                credential: AuthCredential::Password {
                    username: "alice".into(),
                    password: String::new(),
                },
                persist_token: true,
            });
        pump(&mut relaunch, "the silent reconnect", |w| {
            w.resource::<SessionState>().is_online()
        });
        assert_eq!(
            relaunch
                .world()
                .resource::<AuthIdentity>()
                .username
                .as_deref(),
            Some("alice"),
            "the token signed in as somebody else"
        );
    }

    /// **A dropped link comes back by itself, end to end.** A real sim takes
    /// every connection down with it and comes back on the same port — the
    /// shape of a deploy, and on the client's side the same event as Android
    /// aborting the socket of a backgrounded app. The client must be online
    /// again with nobody having asked it to sign in.
    #[test]
    fn a_dropped_link_signs_itself_back_in() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("world.db").to_str().unwrap().to_string();
        let (rt, handle) = boot_sim(&db, "127.0.0.1:0");
        let addr = handle.local_addr.to_string();

        let mut app = client_app(UserStore::default());
        app.world_mut()
            .resource_mut::<SessionState>()
            .request(SessionRequest::Remote {
                server: addr.clone(),
                credential: AuthCredential::Password {
                    username: "alice".into(),
                    password: "hunter22".into(),
                },
                persist_token: true,
            });
        pump(&mut app, "the password sign-in", |w| {
            w.resource::<SessionState>().is_online()
        });

        // The server goes, and every socket with it.
        drop(handle);
        rt.shutdown_timeout(std::time::Duration::from_secs(2));
        pump(&mut app, "the loss to be noticed", |w| {
            w.resource::<SessionState>().reconnect.is_some()
        });

        // And comes back where it was.
        let (_rt, _handle) = boot_sim(&db, &addr);
        pump(&mut app, "the session to come back", |w| {
            w.resource::<SessionState>().is_online()
        });
        let session = app.world().resource::<SessionState>();
        assert!(session.reconnect.is_none(), "a live session still retrying");
        assert_eq!(session.error, None);
        assert_eq!(
            app.world().resource::<AuthIdentity>().username.as_deref(),
            Some("alice")
        );
    }

    fn sign_in_alice(app: &mut App, addr: &str) {
        app.world_mut()
            .resource_mut::<SessionState>()
            .request(SessionRequest::Remote {
                server: addr.to_string(),
                credential: AuthCredential::Password {
                    username: "alice".into(),
                    password: "hunter22".into(),
                },
                persist_token: true,
            });
        pump(app, "the password sign-in", |w| {
            w.resource::<SessionState>().is_online()
        });
    }

    /// Android can take the network away while the app is suspended without the
    /// socket ever reporting it. Nothing arrives after the app comes back, and
    /// the client must notice that itself and sign back in.
    #[test]
    fn a_link_that_stays_silent_after_resume_is_reconnected() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("world.db").to_str().unwrap().to_string();
        let (_rt, handle) = boot_sim(&db, "127.0.0.1:0");

        // No `apply_network_snapshot`: nothing is ever read off the link, which is
        // what a half-open socket looks like from this side.
        let mut app = App::new();
        app.init_resource::<SessionState>()
            .init_resource::<Time<Real>>()
            .init_resource::<GameState>()
            .init_resource::<LocalAvatarSimId>()
            .init_resource::<AuthIdentity>()
            .init_resource::<crate::systems::chat::ChatState>()
            .init_resource::<OsmTileUrlTemplate>()
            .insert_resource(UserStore::default())
            .add_systems(Update, drive_session);
        sign_in_alice(&mut app, &handle.local_addr.to_string());

        app.world_mut()
            .resource_mut::<SessionState>()
            .check_link_after_resume();
        app.update();
        assert!(app.world().resource::<SessionState>().is_online());

        let later = bevy::platform::time::Instant::now()
            + std::time::Duration::from_secs_f64(RESUME_SILENCE_S + 1.0);
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .update_with_instant(later);
        app.update();
        assert!(
            app.world().resource::<SessionState>().is_reconnecting(),
            "a link silent for {RESUME_SILENCE_S} s after resume was left standing"
        );
    }

    /// The other half: a link that answers is left alone.
    #[test]
    fn a_link_that_answers_after_resume_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("world.db").to_str().unwrap().to_string();
        let (_rt, handle) = boot_sim(&db, "127.0.0.1:0");

        let mut app = client_app(UserStore::default());
        sign_in_alice(&mut app, &handle.local_addr.to_string());
        pump(&mut app, "the first snapshot", |w| {
            w.resource::<NetworkMailbox>()
                .received
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0
        });

        app.world_mut()
            .resource_mut::<SessionState>()
            .check_link_after_resume();
        let received_before = app
            .world()
            .resource::<NetworkMailbox>()
            .received
            .load(std::sync::atomic::Ordering::Relaxed);
        pump(&mut app, "traffic after resume", |w| {
            w.resource::<NetworkMailbox>()
                .received
                .load(std::sync::atomic::Ordering::Relaxed)
                > received_before + 1
        });

        let later = bevy::platform::time::Instant::now()
            + std::time::Duration::from_secs_f64(RESUME_SILENCE_S + 1.0);
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .update_with_instant(later);
        app.update();
        let session = app.world().resource::<SessionState>();
        assert!(session.is_online() && !session.is_reconnecting());
    }

    /// Switching apps on the phone drops the socket, and the sim that takes the
    /// reconnect spawns a new avatar at the map anchor. The avatar must come back
    /// where it had walked to — and the sim must be told, or the next snapshot
    /// pulls it straight back to the anchor.
    #[test]
    fn a_reconnect_puts_the_avatar_back_where_it_was() {
        let mut app = App::new();
        app.init_resource::<SessionState>()
            .init_resource::<UserStore>()
            .init_resource::<AvatarState>()
            .insert_resource(MapAnchor {
                geo: Some((53.2194, 6.5665, 14)),
            })
            .add_systems(Update, keep_avatar_across_reconnect);
        let (intent_tx, mut rx) = futures_channel::mpsc::unbounded();
        app.insert_resource(OnlineSession { intent_tx });
        app.world_mut().resource_mut::<SessionState>().status = SessionStatus::Online;

        let walked = Vec3::new(120.0, 0.0, -80.0);
        app.world_mut().resource_mut::<AvatarState>().position = walked;
        let avatar = app
            .world_mut()
            .spawn((
                Avatar,
                GridCell::default(),
                Transform::from_translation(walked),
            ))
            .id();
        app.update();
        assert!(
            rx.try_recv().is_err(),
            "a live session placed its own avatar"
        );

        // Lost and got back: a new avatar, at the origin the sim spawns it on.
        app.world_mut().despawn(avatar);
        app.world_mut().resource_mut::<AvatarState>().position = Vec3::ZERO;
        let avatar = app
            .world_mut()
            .spawn((Avatar, GridCell::default(), Transform::default()))
            .id();
        app.world_mut().resource_mut::<SessionState>().resumed = true;
        app.update();

        let at = app.world().resource::<AvatarState>().position;
        assert!(
            at.distance(walked) < 0.01,
            "avatar at {at}, had walked to {walked}"
        );
        let drawn = app.world().get::<Transform>(avatar).unwrap().translation;
        assert!(drawn.distance(walked) < 0.01, "avatar drawn at {drawn}");
        match rx.try_recv() {
            Ok(cartopolis_core::NetMessage::Teleport { position, .. }) => {
                assert!(position.distance(walked) < 0.01, "sim told {position}");
            }
            _ => panic!("the sim was not told where the avatar is"),
        }
        assert!(!app.world().resource::<SessionState>().resumed);
    }

    /// Closing the app and opening it again finds the avatar where it was left —
    /// the first sign-in of a launch reads `last_avatar` the way a reconnect reads
    /// its own memory — unless that spot is far from where this launch opened.
    #[test]
    fn the_first_sign_in_puts_the_avatar_back_where_it_was_left() {
        use crate::systems::user_store::SavedAvatar;
        let anchor = (53.2194, 6.5665, 14);
        let near = geo_nav::world_to_lat_lng(anchor, 200.0, -150.0);
        let far = geo_nav::world_to_lat_lng(anchor, 50_000.0, 0.0);
        for (saved, expect_moved) in [(near, true), (far, false)] {
            let mut app = App::new();
            app.init_resource::<SessionState>()
                .init_resource::<AvatarState>()
                .init_resource::<UserStore>()
                .insert_resource(MapAnchor { geo: Some(anchor) })
                .add_systems(Update, keep_avatar_across_reconnect);
            let (intent_tx, mut rx) = futures_channel::mpsc::unbounded();
            app.insert_resource(OnlineSession { intent_tx });
            app.world_mut().resource_mut::<UserStore>().data.last_avatar = Some(SavedAvatar {
                lat: saved.0,
                lng: saved.1,
                y: 0.0,
            });
            app.world_mut().resource_mut::<SessionState>().status = SessionStatus::Online;
            app.world_mut()
                .spawn((Avatar, GridCell::default(), Transform::default()));
            app.update();

            let at = app.world().resource::<AvatarState>().position;
            let sent = matches!(
                rx.try_recv(),
                Ok(cartopolis_core::NetMessage::Teleport { .. })
            );
            if expect_moved {
                assert!(
                    at.distance(Vec3::new(200.0, 0.0, -150.0)) < 0.5,
                    "avatar at {at}"
                );
                assert!(sent, "the sim was not told where the avatar is");
            } else {
                assert!(
                    at.length() < 0.01,
                    "a far-off spot moved the avatar to {at}"
                );
                assert!(!sent);
            }
            // …and from then on the store follows the avatar.
            let kept = app
                .world()
                .resource::<UserStore>()
                .data
                .last_avatar
                .unwrap();
            let (wx, wz) = geo_nav::lat_lng_to_world(anchor, kept.lat, kept.lng);
            assert!((Vec3::new(wx as f32, 0.0, wz as f32) - at).length() < 0.5);
        }
    }
}
