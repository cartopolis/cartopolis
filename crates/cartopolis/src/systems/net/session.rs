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

use bevy::prelude::*;
use cartopolis_core::AuthCredential;
use serde::{Deserialize, Serialize};

use crate::components::{Avatar, Prim, RemoteAvatar};
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
    prims: Query<Entity, With<Prim>>,
    remotes: Query<Entity, With<RemoteAvatar>>,
    avatars: Query<Entity, With<Avatar>>,
) {
    if let Some(request) = session.request.take() {
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
                &prims,
                &remotes,
                &avatars,
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
                &prims,
                &remotes,
                &avatars,
            );
            // Only worth reporting if there was a session to lose; a link closed
            // before it came up already failed for a better-stated reason.
            if online {
                session.error = Some(format!("Disconnected from {was}"));
            }
        }
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
            .init_resource::<GameState>()
            .init_resource::<LocalAvatarSimId>()
            .init_resource::<AuthIdentity>()
            .init_resource::<crate::systems::chat::ChatState>()
            .init_resource::<OsmTileUrlTemplate>()
            .insert_resource(UserStore::load())
            .add_systems(Update, drive_session);
        app
    }

    /// Pretend a session is up: the resources a live link installs, plus the
    /// local avatar and one of somebody else's.
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
        app.world_mut().spawn(Avatar);
        app.world_mut().spawn(RemoteAvatar {
            sim_id: 7,
            net_position: Vec3::ZERO,
            net_yaw: 0.0,
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

    /// **"Stay signed in", end to end, across a viewer restart.** A real sim on
    /// loopback, the real `drive_session` + `apply_network_snapshot` systems, a
    /// real `UserStore` — sign in with a password once, then rebuild the whole
    /// client world (a restart) carrying only what `user.json` would carry, and
    /// require the remembered session to come up without the password. This is
    /// the exact loop a user reported broken on 2026-08-21, and every unit here
    /// read as correct on paper; only the assembled path can say so.
    #[test]
    fn stay_signed_in_survives_a_viewer_restart() {
        use crate::resources::{AvatarState, PrimTextureCache, TextureLibrary};
        use bevy::asset::Assets;
        use bevy::image::Image;

        /// The client world as `lib.rs` assembles it, reduced to the systems and
        /// resources the session + snapshot path touches. Fresh per "launch".
        fn client_app(store: UserStore) -> App {
            let mut app = App::new();
            app.init_resource::<SessionState>()
                .init_resource::<GameState>()
                .init_resource::<LocalAvatarSimId>()
                .init_resource::<AuthIdentity>()
                .init_resource::<AvatarState>()
                .init_resource::<TextureLibrary>()
                .init_resource::<PrimTextureCache>()
                .init_resource::<crate::systems::chat::ChatState>()
                .init_resource::<crate::systems::script_editor::ScriptEditorState>()
                .init_resource::<crate::systems::voice::VoiceState>()
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
        /// with whatever the session has to say for itself.
        fn pump(app: &mut App, what: &str, done: impl Fn(&World) -> bool) {
            for _ in 0..500 {
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

        // A real simulator with a known account, exactly as the voice loopback
        // test boots one.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("world.db").to_str().unwrap().to_string();
        {
            let conn = cartopolis_simulator::db::open_and_migrate(&db).unwrap();
            cartopolis_simulator::ensure_account(&conn, "alice", "hunter22").unwrap();
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        let handle = rt
            .block_on(cartopolis_simulator::serve(std::sync::Arc::new(
                cartopolis_simulator::SimConfig {
                    database_path: db,
                    listen: "127.0.0.1:0".into(),
                    ..cartopolis_simulator::SimConfig::default()
                },
            )))
            .unwrap();
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
}
