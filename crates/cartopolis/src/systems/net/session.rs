//! Which world you are in, and the machinery for changing your mind about it.
//!
//! The client has always had three run modes (ADR-016): offline (no sim, no auth,
//! a streamed basemap and a local avatar), solo (an in-process sim on loopback)
//! and split (`--connect` to an external sim). What it did not have was a way to
//! *move between* them — the mode was decided in `lib.rs` before the app was
//! built, from flags, and changing it meant quitting.
//!
//! That was the wrong shape, because the sim is not a different application: it is
//! offline plus an authority for the avatar, the prims and the other people in it.
//! Everything online is already `Option<Res<OnlineSession>>`-guarded, so "offline"
//! is precisely "that resource is absent" — a state the app can enter and leave at
//! runtime rather than a launch-time fork. This module is that state machine, and
//! `systems::login_screen` is its face.
//!
//! **The request is a resource field, not an event**, because the sign-in screen
//! and startup both write it and neither can observe when the driver reads it;
//! `take()` in one place is easier to reason about than a message that may be read
//! twice or dropped. Requests are edge-triggered and idempotent: re-entering the
//! mode you are already in tears the session down and rebuilds it, which is what
//! "Reconnect" means.

use bevy::prelude::*;
use cartopolis_core::AuthCredential;
use serde::{Deserialize, Serialize};

use crate::components::{Prim, Region, RemoteAvatar};
use crate::resources::{
    AuthIdentity, ConnectAddr, ConnectAuth, GameState, LocalAvatarSimId, NetworkMailbox,
    NetworkSyncState, OnlineSession, OsmTileUrlTemplate,
};
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
/// **Only two of these are anything a person chooses**, and the sign-in card
/// offers neither as a *mode*: offline is where the app starts and signing in is
/// the one verb, so the card is a form for [`Self::Remote`] and nothing else. It
/// used to be a three-row picker, which asked everybody to make an architecture
/// decision — "solo simulator" against "connect to a server" — before they had
/// seen the map. [`Self::Solo`] survives as a developer path (`--login` with no
/// server, `crates/cartopolis/src/solo.rs`) and as a mode a `user.json` written
/// by an older build may still name; nothing in the UI can enter it.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// No sim at all: the streamed map and a local avatar, single user.
    #[default]
    Offline,
    /// An in-process sim on loopback (the `solo` feature).
    Solo,
    /// An external sim over TCP.
    Remote,
}

/// The mode the client is actually in.
///
/// Only `Offline` is reachable on the web build (`can_simulate`), hence the
/// allow — same reason as `network::LinkState`.
///
/// The `solo` arm of that allow is the more interesting one, and it is here
/// because leaving it out cost this project every Android release it never had.
/// `Solo` is constructed only where the `solo` feature compiles, and the APK is
/// built `--no-default-features` (nothing cross-compiles `rusqlite` for Android)
/// — so on that target the variant is dead, `[workspace.lints.rust] unused =
/// "deny"` turns dead code into a hard error, and the whole client crate fails to
/// build. Gating on the *platform* would have been the third instance of the trap
/// CLAUDE.md names: this tree's seams were written as native-vs-wasm and Android
/// answers "native" to all of them. Gate on the feature that is actually absent.
#[cfg_attr(any(target_arch = "wasm32", not(feature = "solo")), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum SessionMode {
    #[default]
    Offline,
    Solo,
    Remote {
        server: String,
    },
}

impl SessionMode {
    /// Where the session lives, for a status line. Solo's ephemeral loopback port
    /// is noise to a user, so it reads as the machine instead.
    pub fn where_(&self) -> String {
        match self {
            Self::Offline => "no simulator".into(),
            Self::Solo => "this machine".into(),
            Self::Remote { server } => server.clone(),
        }
    }
}

/// How far the current mode has got. `Offline` is a resting state, not a failure.
///
/// Same two-part allow as [`SessionMode`]: `Starting` means "booting the
/// in-process sim", which is unreachable both on the web and in any build without
/// the `solo` feature — the Android APK being the one that matters.
#[cfg_attr(any(target_arch = "wasm32", not(feature = "solo")), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum SessionStatus {
    #[default]
    Offline,
    /// Booting the in-process sim; no socket yet.
    Starting,
    Connecting,
    Authenticating,
    Online,
}

impl SessionStatus {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Connecting | Self::Authenticating
        )
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Offline => "Offline",
            Self::Starting => "Starting the local simulator…",
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
    Solo {
        /// Log in as somebody other than the provisioned `dev` account — what a
        /// `--login` with no server means. `None` is the sign-in screen's answer
        /// and the ordinary one.
        credential: Option<AuthCredential>,
    },
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
    /// The credential to present once a pending solo sim finishes booting.
    pending_auth: Option<ConnectAuth>,
    #[cfg(feature = "solo")]
    pending_solo: Option<crate::solo::SoloPending>,
    /// Keeps the in-process sim alive; dropped on teardown, which stops it.
    #[cfg(feature = "solo")]
    solo: Option<crate::solo::SoloSim>,
}

impl SessionState {
    pub fn is_online(&self) -> bool {
        self.status == SessionStatus::Online
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

pub const fn can_solo() -> bool {
    cfg!(feature = "solo") && !cfg!(target_arch = "wasm32")
}

/// Leave the world and go back to the offline map — the app menu's **Log out**,
/// and the sign-in card's Sign out.
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

/// Apply session requests, poll a booting solo sim, and watch a live link.
///
/// One system rather than three because all of them mutate the same state machine
/// and the ordering between them matters within a single frame: a request torn
/// down here must not be seen as a *dropped* link by the watcher below.
#[allow(clippy::too_many_arguments)]
pub fn drive_session(
    mut commands: Commands,
    mut session: ResMut<SessionState>,
    store: Res<UserStore>,
    tile_url: Res<OsmTileUrlTemplate>,
    mut game_state: ResMut<GameState>,
    mut local_sim_id: ResMut<LocalAvatarSimId>,
    mut identity: ResMut<AuthIdentity>,
    mut chat: ResMut<crate::systems::chat::ChatState>,
    regions: Query<Entity, With<Region>>,
    prims: Query<Entity, With<Prim>>,
    remotes: Query<Entity, With<RemoteAvatar>>,
) {
    if let Some(request) = session.request.take() {
        teardown(
            &mut commands,
            &mut session,
            &mut game_state,
            &mut local_sim_id,
            &mut identity,
            &mut chat,
            &regions,
            &prims,
            &remotes,
        );
        session.error = None;
        match request {
            SessionRequest::Offline => {
                session.mode = SessionMode::Offline;
                session.status = SessionStatus::Offline;
                info!("session: offline");
            }
            SessionRequest::Solo { credential } => start_solo(&mut session, credential),
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

    // A solo sim that was still binding when the request came through.
    #[cfg(feature = "solo")]
    if let Some(mut pending) = session.pending_solo.take() {
        match pending.poll() {
            None => session.pending_solo = Some(pending),
            Some(Ok((addr, sim))) => {
                info!(%addr, "solo mode: in-process sim");
                session.solo = Some(sim);
                let auth = session.pending_auth.take().unwrap_or_else(solo_auth);
                connect(&mut commands, &mut session, &store, &tile_url, &addr, auth);
            }
            Some(Err(e)) => {
                error!("solo sim failed to start: {e}");
                session.error = Some(e);
                session.mode = SessionMode::Offline;
                session.status = SessionStatus::Offline;
            }
        }
    }

    // Watch the live link. A drop is the same event as a failed sign-in as far as
    // the app is concerned: the authority is gone, so fall back to offline rather
    // than leaving a half-online client whose avatar no longer answers.
    let Some(link) = session.link.clone() else {
        return;
    };
    match link.get() {
        LinkState::Connecting => session.status = SessionStatus::Connecting,
        LinkState::Authenticating => session.status = SessionStatus::Authenticating,
        LinkState::Online => session.status = SessionStatus::Online,
        LinkState::Failed(message) => {
            warn!(%message, "session failed");
            let was = session.mode.where_();
            teardown(
                &mut commands,
                &mut session,
                &mut game_state,
                &mut local_sim_id,
                &mut identity,
                &mut chat,
                &regions,
                &prims,
                &remotes,
            );
            session.error = Some(format!("{was}: {message}"));
        }
        LinkState::Closed => {
            let online = session.status == SessionStatus::Online;
            let was = session.mode.where_();
            teardown(
                &mut commands,
                &mut session,
                &mut game_state,
                &mut local_sim_id,
                &mut identity,
                &mut chat,
                &regions,
                &prims,
                &remotes,
            );
            // Only worth reporting if there was a session to lose; a link closed
            // before it came up already failed for a better-stated reason.
            if online {
                session.error = Some(format!("Disconnected from {was}"));
            }
        }
    }
}

#[cfg(feature = "solo")]
fn solo_auth() -> ConnectAuth {
    ConnectAuth {
        credential: AuthCredential::Password {
            username: crate::solo::DEV_USER.to_string(),
            password: crate::solo::DEV_PASSWORD.to_string(),
        },
        // Solo's loopback port is ephemeral, so a token keyed on it is useless.
        persist_token: false,
    }
}

#[cfg(feature = "solo")]
fn start_solo(session: &mut SessionState, credential: Option<AuthCredential>) {
    session.mode = SessionMode::Solo;
    session.status = SessionStatus::Starting;
    session.pending_auth = Some(match credential {
        Some(credential) => ConnectAuth {
            credential,
            persist_token: false,
        },
        None => solo_auth(),
    });
    session.pending_solo = Some(crate::solo::start());
}

#[cfg(not(feature = "solo"))]
fn start_solo(session: &mut SessionState, _credential: Option<AuthCredential>) {
    session.mode = SessionMode::Offline;
    session.status = SessionStatus::Offline;
    session.error = Some("This build has no local simulator — connect to a server".into());
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
    regions: &Query<Entity, With<Region>>,
    prims: &Query<Entity, With<Prim>>,
    remotes: &Query<Entity, With<RemoteAvatar>>,
) {
    session.link = None;
    session.pending_auth = None;
    #[cfg(feature = "solo")]
    {
        session.pending_solo = None;
        // Drops the shutdown sender → the sim thread wakes, aborts its tasks and
        // frees the port. Switching solo → remote otherwise leaks a runtime and a
        // SQLite handle per switch.
        session.solo = None;
    }
    session.mode = SessionMode::Offline;
    session.status = SessionStatus::Offline;

    commands.remove_resource::<OnlineSession>();
    commands.remove_resource::<NetworkMailbox>();
    commands.remove_resource::<NetworkSyncState>();
    commands.remove_resource::<ConnectAddr>();
    commands.remove_resource::<ConnectAuth>();

    *identity = AuthIdentity::default();
    *local_sim_id = LocalAvatarSimId(None);
    // The chat log belongs to the world it was said in; carrying it into the next
    // one would attribute strangers' lines to a server they never spoke on.
    *chat = crate::systems::chat::ChatState::default();
    game_state.regions_loaded = false;
    game_state.prims_loaded = false;
    game_state.selected_prim_id = None;

    // The local avatar entity stays — it is spawned at startup and its offline
    // movement path takes over the moment `OnlineSession` is gone. Only the
    // server's own objects go.
    for e in regions.iter().chain(prims.iter()).chain(remotes.iter()) {
        commands.entity(e).despawn();
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
            .init_resource::<GameState>()
            .init_resource::<LocalAvatarSimId>()
            .init_resource::<AuthIdentity>()
            .init_resource::<crate::systems::chat::ChatState>()
            .init_resource::<OsmTileUrlTemplate>()
            .insert_resource(UserStore::load())
            .add_systems(Update, drive_session);
        app
    }

    /// Pretend a session is up: the resources a live link installs, plus two of
    /// the server's own entities.
    fn go_online(app: &mut App) {
        let (intent_tx, _rx) = futures_channel::mpsc::unbounded();
        let (_tx, out_rx) = std::sync::mpsc::channel();
        app.insert_resource(OnlineSession { intent_tx });
        app.insert_resource(NetworkMailbox {
            rx: std::sync::Mutex::new(out_rx),
        });
        app.insert_resource(NetworkSyncState {
            received_initial_world: true,
        });
        app.insert_resource(ConnectAddr("sim.example:4747".into()));
        app.world_mut().resource_mut::<AuthIdentity>().username = Some("alice".into());
        app.world_mut().spawn(Region {
            id: 1,
            name: "Region".into(),
            latitude: 0.0,
            longitude: 0.0,
            tile_x: 0,
            tile_y: 0,
            tile_z: 0,
            sim_origin: None,
        });
        app.world_mut().spawn(RemoteAvatar {
            sim_id: 7,
            net_position: Vec3::ZERO,
            net_yaw: 0.0,
            pending_emote: None,
            last_emote_sequence: None,
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
            world.query::<&Region>().iter(world).count(),
            0,
            "the sim's regions outlived the session"
        );
        assert_eq!(
            world.query::<&RemoteAvatar>().iter(world).count(),
            0,
            "other people outlived the session"
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
            (SessionKind::Solo, "\"solo\""),
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
        assert!(SessionStatus::Starting.is_busy());
        assert!(SessionStatus::Connecting.is_busy());
        assert!(SessionStatus::Authenticating.is_busy());
        assert!(!SessionStatus::Online.is_busy());
        assert!(!SessionStatus::Offline.is_busy());
    }
}
