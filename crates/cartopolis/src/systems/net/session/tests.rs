use super::*;
use cartopolis_core::msg;

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
        pending_expression: None,
        last_expression_sequence: None,
        seated: None,
        seat_blend: 0.0,
        terrain_lift: 0.0,
        swimming: false,
        swim_blend: 0.0,
        ledge_lift: 0.0,
        climb: None,
        seat_lift: 0.0,
        seat_dangle: false,
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
        .init_resource::<crate::systems::contacts::ContactsInbox>()
        .init_resource::<crate::systems::greet::GreetInbox>()
        .init_resource::<crate::systems::prim_sound::PrimSoundInbox>()
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
fn boot_sim(db: &str, listen: &str) -> (tokio::runtime::Runtime, cartopolis_simulator::SimHandle) {
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
        .init_resource::<AuthIdentity>()
        .init_resource::<geo_nav::GeoNavState>()
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
        Ok(cartopolis_core::NetMessage::Teleport(msg::Teleport { geo, alt_m, .. })) => {
            // A place on Earth (protocol 47), which under the test's anchor
            // is the frame position the avatar had walked to.
            let (x, z) =
                crate::grid::lat_lng_to_world((53.2194, 6.5665, 14), geo.latitude, geo.longitude);
            let told = Vec3::new(x as f32, alt_m, z as f32);
            assert!(told.distance(walked) < 0.01, "sim told {told}");
        }
        _ => panic!("the sim was not told where the avatar is"),
    }
    assert!(!app.world().resource::<SessionState>().resumed);
}

/// Signing out and in again in the same run used to leave the avatar on the
/// map anchor, wherever that was. The server's word on where it was left
/// wins, on every sign-in, over the device's own memory.
#[test]
fn every_sign_in_goes_where_the_server_says_it_was_left() {
    use crate::systems::user_store::SavedAvatar;
    let anchor = (53.2194, 6.5665, 14);
    let mut app = App::new();
    app.init_resource::<SessionState>()
        .init_resource::<AvatarState>()
        .init_resource::<UserStore>()
        .init_resource::<AuthIdentity>()
        .init_resource::<geo_nav::GeoNavState>()
        .insert_resource(MapAnchor { geo: Some(anchor) })
        .add_systems(Update, keep_avatar_across_reconnect);
    let (intent_tx, mut rx) = futures_channel::mpsc::unbounded();
    app.insert_resource(OnlineSession { intent_tx });
    let device = crate::grid::world_to_lat_lng(anchor, -300.0, 40.0);
    app.world_mut().resource_mut::<UserStore>().data.last_avatar = Some(SavedAvatar {
        lat: device.0,
        lng: device.1,
        y: 0.0,
    });
    app.world_mut().resource_mut::<SessionState>().status = SessionStatus::Online;
    app.world_mut()
        .spawn((Avatar, GridCell::default(), Transform::default()));

    for (x, z) in [(200.0, -150.0), (-900.0, 600.0)] {
        // A sign-in: the handshake says where, and the avatar is new.
        let (lat, lng) = crate::grid::world_to_lat_lng(anchor, x, z);
        app.world_mut().resource_mut::<AuthIdentity>().arrival = Some(cartopolis_core::GeoAnchor {
            latitude: lat,
            longitude: lng,
        });
        app.update();
        let p = app.world().resource::<AvatarState>().position;
        let at = Vec3::new(p.x, 0.0, p.z);
        let want = Vec3::new(x as f32, 0.0, z as f32);
        assert!(at.distance(want) < 0.5, "avatar at {at}, left at {want}");
        assert!(
            matches!(
                rx.try_recv(),
                Ok(cartopolis_core::NetMessage::Teleport(msg::Teleport { .. }))
            ),
            "the sim was not told where the avatar is"
        );
        assert!(app.world().resource::<AuthIdentity>().arrival.is_none());
    }
}

/// Closing the app and opening it again finds the avatar where it was left —
/// the first sign-in of a launch reads `last_avatar` the way a reconnect reads
/// its own memory, when the server has no place for the account — and a spot
/// far from where this launch opened is jumped to rather than written.
#[test]
fn the_first_sign_in_puts_the_avatar_back_where_it_was_left() {
    use crate::systems::user_store::SavedAvatar;
    let anchor = (53.2194, 6.5665, 14);
    let near = crate::grid::world_to_lat_lng(anchor, 200.0, -150.0);
    let far = crate::grid::world_to_lat_lng(anchor, 50_000.0, 0.0);
    for (saved, expect_moved) in [(near, true), (far, false)] {
        let mut app = App::new();
        app.init_resource::<SessionState>()
            .init_resource::<AvatarState>()
            .init_resource::<UserStore>()
            .init_resource::<AuthIdentity>()
            .init_resource::<geo_nav::GeoNavState>()
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

        let p = app.world().resource::<AvatarState>().position;
        // Horizontal only: a fresh `AvatarState` stands at its own default height.
        let at = Vec3::new(p.x, 0.0, p.z);
        let sent = matches!(
            rx.try_recv(),
            Ok(cartopolis_core::NetMessage::Teleport(msg::Teleport { .. }))
        );
        if expect_moved {
            assert!(
                at.distance(Vec3::new(200.0, 0.0, -150.0)) < 0.5,
                "avatar at {at}"
            );
            assert!(sent, "the sim was not told where the avatar is");
        } else {
            // Far: not written into this frame, but jumped to.
            assert!(
                at.length() < 0.01,
                "a far-off spot was written into the frame at {at}"
            );
            assert!(!sent);
            let jump = app
                .world_mut()
                .resource_mut::<geo_nav::GeoNavState>()
                .pending_teleport
                .take()
                .expect("a far-off spot was not teleported to");
            assert!((jump.lat - saved.0).abs() < 1e-9 && (jump.lng - saved.1).abs() < 1e-9);
            assert!(jump.silent);
            continue;
        }
        // …and from then on the store follows the avatar.
        let kept = app
            .world()
            .resource::<UserStore>()
            .data
            .last_avatar
            .unwrap();
        let (wx, wz) = crate::grid::lat_lng_to_world(anchor, kept.lat, kept.lng);
        assert!((Vec3::new(wx as f32, 0.0, wz as f32) - at).length() < 0.5);
    }
}
