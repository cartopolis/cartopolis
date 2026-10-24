//! `simulator` as a library: the headless simulation server, usable standalone via
//! the `simulator` binary or embedded in-process (cartopolis solo/dev mode, ADR-016).
//!
//! Embedding runs the *real* server on a background tokio runtime and connects to
//! it over loopback TCP — the same wire protocol and (once ADR-016 lands) the same
//! auth handshake an external sim uses. There is deliberately no in-memory bypass:
//! solo mode and `--connect` share one client path (see ADR-016 "Solo/dev mode").

pub mod assets;
pub mod auth;
pub mod authoring;
pub mod buildings;
pub mod calls;
pub mod chat;
pub mod cli;
pub mod communities;
pub mod config;
pub mod contacts;
pub mod content;
pub mod db;
pub mod e2ee;
pub mod fediverse;
pub mod http;
pub mod library;
pub mod looks;
pub mod mail;
pub mod net;
pub mod physics;
pub mod pictures;
pub mod prims;
pub mod push;
pub mod registration;
pub mod roles;
pub mod scripting;
pub mod signin;
pub mod state;
pub mod stats;
pub mod textures;
pub mod vapid;
pub mod videos;

pub use auth::{ensure_account, ensure_admin_account, Sessions};
pub use chat::ChatHub;
pub use config::SimConfig;

use anyhow::Context;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, RwLock};

/// A running server. Dropping it aborts the accept + tick tasks, which is how
/// in-process (solo) teardown works when the client exits.
pub struct SimHandle {
    /// The actually-bound address. Resolves an ephemeral `:0` port to a concrete
    /// one, which solo mode reads back to point the client at.
    pub local_addr: SocketAddr,
    /// Where the HTTP surface (`/ws` + the admin API) is bound, when
    /// `SimConfig::http_listen` asked for one. `None` is the default and is what
    /// solo mode and the tests get.
    pub http_addr: Option<SocketAddr>,
    /// Where the call forwarder's UDP socket is bound, when
    /// `SimConfig::calls_listen` asked for one.
    pub calls_addr: Option<SocketAddr>,
    /// The usage counters, so a test can write them out without waiting for
    /// the minute (`stats::Stats::flush`).
    pub stats: Arc<stats::Stats>,
    tasks: tokio::task::JoinSet<&'static str>,
}

impl SimHandle {
    /// Stop the server now. Equivalent to dropping the handle (a `JoinSet`
    /// aborts its tasks on drop).
    pub fn shutdown(self) {}

    /// Resolve when any background task ends — with what it was.
    ///
    /// Short of shutdown these tasks run for the life of the process, so an
    /// ending task is a failure even when it is not a panic: a tick loop that
    /// returned leaves a server that still accepts connections, authenticates
    /// and answers admin HTTP while the world silently never advances and no
    /// snapshot goes out. The binary selects on this against Ctrl-C and exits
    /// non-zero, so a half-dead server becomes a systemd restart instead of a
    /// mystery.
    pub async fn task_ended(&mut self) -> anyhow::Error {
        match self.tasks.join_next().await {
            Some(Ok(label)) => anyhow::anyhow!("{label} ended unexpectedly"),
            // JoinError carries the panic message when there was one.
            Some(Err(e)) => anyhow::anyhow!("background task died: {e}"),
            None => anyhow::anyhow!("no background tasks"),
        }
    }
}

/// Open the DB, load the world, start the tick loop, and bind + serve the TCP
/// listener. Returns once the listener is bound — so `SimHandle::local_addr` is
/// valid and the server is ready to accept connections.
pub async fn serve(config: Arc<SimConfig>) -> anyhow::Result<SimHandle> {
    let conn = db::open_and_migrate(&config.database_path)?;
    let prims = db::load_world(&conn)?;
    let scripts = db::load_prim_scripts(&conn)?;
    let owners = db::load_prim_owners(&conn)?;
    let conn = Arc::new(std::sync::Mutex::new(conn));
    // Usage counts, in their own file beside the world's (`stats`). Sampled and
    // written once a minute; what the last minute counted is lost on a stop.
    let stats = stats::Stats::open_beside(&config.database_path)?;

    let world = Arc::new(RwLock::new(state::SimWorld::new(
        prims,
        config.world_caps(),
        scripts.iter().map(|(id, _)| *id).collect(),
        owners,
    )));

    // Compile the world's stored scripts before the first tick, so a prim that was
    // moving when the server stopped is moving again when it comes back rather
    // than waiting for someone to open the editor.
    let scripts_host = scripting::ScriptHost::spawn();
    for (prim_id, source) in scripts {
        scripts_host.load(prim_id, source);
    }
    tracing::info!(count = scripts_host.requested(), "scripts loaded");

    // Backed by the `sessions` table, which is what keeps "stay signed in"
    // true across a redeploy — the deployed sim restarts on every publish.
    let sessions = Arc::new(auth::Sessions::persistent(conn.clone()));
    let chat = Arc::new(chat::ChatHub::new());

    // Bound here, before anything is accepted, for the reason `http_listen` is
    // below: a bad address should fail `serve`, not a background task later.
    let mut calls_addr = None;
    let mut calls_task = None;
    // The key browsers subscribe to pushes with (`vapid`), kept beside the
    // database. Only when calls are on: a push rings a call, and a server
    // without calls has nothing to ring a browser for.
    let vapid = match config.calls_listen {
        Some(_) => Some(Arc::new(load_vapid(&config)?)),
        None => None,
    };
    let calls = match config.calls_listen {
        Some(ref listen) => {
            let public = config
                .calls_public_addr
                .as_deref()
                .map(str::parse::<SocketAddr>)
                .transpose()
                .context("calls_public_addr must be IP:port")?;
            // Wakes the phones of a chat's members when a call starts in it.
            let ring = push::spawn(conn.clone(), vapid.clone());
            let (handle, bound, task) = calls::spawn(
                listen,
                public,
                chat.clone(),
                Some(ring),
                Some(stats.clone()),
            )
            .await
            .with_context(|| format!("bind calls_listen {listen}"))?;
            tracing::info!(%bound, "calls listening (udp)");
            calls_addr = Some(bound);
            calls_task = Some(task);
            Some(handle)
        }
        None => None,
    };

    let (tx_snap, _) = broadcast::channel::<bytes::Bytes>(256);
    let script_handle = scripts_host.handle();
    // The one way prims are made, changed and deleted, shared by the wire, the
    // authoring API and the tick loop's flush (`prims`'s module note).
    let prim_service = prims::PrimService::new(
        world.clone(),
        conn.clone(),
        tx_snap.clone(),
        script_handle.clone(),
    );
    // Each task returns a label so `task_ended` can say *which* one died.
    let mut tasks = tokio::task::JoinSet::new();
    {
        let (stats, world) = (stats.clone(), world.clone());
        tasks.spawn(async move {
            let _ = stats::spawn(stats, world).await;
            "stats"
        });
    }
    // The web visitors' countries: a table beside the database, fetched monthly
    // when `geoip_download` is on (`stats::countries`).
    if config.database_path != ":memory:" {
        stats::countries::keep_fresh(
            stats.countries(),
            stats::countries::path_beside(&config.database_path),
            config.geoip_download,
        );
    }
    // APK downloads, from the lines Caddy streams (`stats::apk`). Bound here so
    // a bad address fails the boot rather than a task later.
    if let Some(ref addr) = config.apk_log_listen {
        let (bound, task) = stats::apk::listen(addr, stats.clone())
            .await
            .with_context(|| format!("bind apk_log_listen {addr}"))?;
        tracing::info!(%bound, "apk download lines listening");
        tasks.spawn(async move {
            let _ = task.await;
            "apk download lines"
        });
    }
    if let Some(task) = calls_task {
        tasks.spawn(async move {
            let _ = task.await;
            "calls"
        });
    }
    {
        let world = world.clone();
        let config = config.clone();
        let tx_snap = tx_snap.clone();
        let chat = chat.clone();
        let prims = prim_service.clone();
        tasks.spawn(async move {
            net::tick_loop(world, config, tx_snap, scripts_host, chat, prims).await;
            "tick loop"
        });
    }

    let listener = TcpListener::bind(&config.listen).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(%local_addr, "listening");

    // Bound before the accept loop takes ownership of everything, so a bad
    // `http_listen` fails `serve` rather than killing a background task minutes
    // later and leaving a server that looks healthy and answers nothing.
    let mut http_addr = None;
    let fedi = fediverse::Federation::from_config(&config);
    if let Some(ref addr) = config.http_listen {
        // Built before the listener binds, so a mistyped relay or sender address
        // fails the boot rather than the first registration.
        let mailer = Arc::new(mail::Mailer::from_config(&config.mail())?);
        if config.registration_open && !mailer.delivers() {
            tracing::warn!(
                "registration is open with no smtp_url configured — verification links will be \
                 written to this log instead of sent"
            );
        }
        // Frees the usernames of registrations nobody confirmed, and drops the
        // tokens that have expired. Also runs per registration; here so a server
        // that never gets another one still converges.
        if let Ok(guard) = conn.lock() {
            if let Err(e) = registration::sweep(&guard) {
                tracing::warn!("registration sweep: {e:#}");
            }
            if let Err(e) = auth::sweep_guests(&guard, auth::unix_now_s()) {
                tracing::warn!("guest sweep: {e:#}");
            }
        }
        let state = http::HttpState {
            world: world.clone(),
            config: config.clone(),
            tx_snap: tx_snap.clone(),
            conn: conn.clone(),
            sessions: sessions.clone(),
            chat: chat.clone(),
            scripts: script_handle.clone(),
            calls: calls.clone(),
            vapid: vapid.clone(),
            prims: prim_service.clone(),
            mailer,
            registration_throttle: Arc::new(http::RegistrationLimits::default()),
            fedi: fedi.clone(),
            stats: stats.clone(),
            stats_throttle: Arc::new(registration::Throttle::new(
                600,
                std::time::Duration::from_secs(3600),
            )),
            signin: Arc::new(signin::SignIn::new(
                load_signin_key(&config)?,
                config.fediverse_allow_private,
                config
                    .signin_test_base
                    .clone()
                    .filter(|_| config.fediverse_allow_private),
            )),
        };
        // Public posts reach other servers through a queue, and the servers
        // nobody here talks to come from a shared list; both only once this
        // server is on the fediverse at all.
        if fedi.enabled {
            tracing::info!(server = %fedi.server, "fediverse: on");
            let (db, fed) = (conn.clone(), fedi.clone());
            tasks.spawn(async move {
                fediverse::delivery::run(db, fed).await;
                "fediverse delivery"
            });
            let lists = config.fediverse_blocklists.clone();
            tokio::spawn(fediverse::refresh_blocklists(
                conn.clone(),
                fedi.clone(),
                lists,
            ));
        }
        let http_listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("bind http_listen {addr}"))?;
        let bound = http_listener.local_addr()?;
        tracing::info!(%bound, "http listening (/ws + /api/admin)");
        http_addr = Some(bound);
        // `into_make_service_with_connect_info` and not `into_make_service`: the
        // WS handler logs the peer, and without the connect info that extractor
        // is a 500 on every upgrade rather than a compile error.
        let app = http::router(state).into_make_service_with_connect_info::<SocketAddr>();
        // The same Nagle disable the raw TCP accept loop does, and it matters more
        // here, not less: this is the listener every phone and browser arrives on
        // (a bare hostname resolves to `wss://host/ws`), so the deployed client's
        // snapshots all come through it. `tap_io` is axum's hook for reaching the
        // socket before the connection is served.
        let http_listener = {
            use axum::serve::ListenerExt;
            http_listener.tap_io(|tcp| {
                if let Err(e) = tcp.set_nodelay(true) {
                    tracing::warn!("set_nodelay failed on a /ws connection: {e:#}");
                }
            })
        };
        tasks.spawn(async move {
            if let Err(e) = axum::serve(http_listener, app).await {
                tracing::error!("http server ended: {e:#}");
            }
            "http server"
        });
    }

    let accept_stats = stats.clone();
    tasks.spawn(async move {
        accept_loop(
            listener,
            world,
            config,
            tx_snap,
            conn,
            sessions,
            chat,
            script_handle,
            calls,
            prim_service,
            accept_stats,
        )
        .await;
        "accept loop"
    });

    Ok(SimHandle {
        local_addr,
        http_addr,
        calls_addr,
        stats,
        tasks,
    })
}

/// This server's web push key: `vapid.key` beside the database, made the first
/// time. An in-memory database has nowhere to keep one, so it gets a key for
/// the life of the process.
fn load_vapid(config: &SimConfig) -> anyhow::Result<vapid::Vapid> {
    let contact = &config.public_base_url;
    if config.database_path == ":memory:" {
        return vapid::Vapid::ephemeral(contact);
    }
    let path = std::path::Path::new(&config.database_path).with_file_name("vapid.key");
    vapid::Vapid::load_or_create(&path, contact)
        .with_context(|| format!("web push key at {}", path.display()))
}

/// The key provider secrets are sealed with in the database (`signin::keys`):
/// `CARTO_SIGNIN_KEY` (64 hex digits) when set, else `signin.key` beside the
/// database, made the first time. Read from the environment directly rather
/// than through [`SimConfig`], which is `Debug` and would print it. An
/// in-memory database gets a key for the life of the process.
fn load_signin_key(config: &SimConfig) -> anyhow::Result<signin::keys::SealKey> {
    if let Ok(hex) = std::env::var("CARTO_SIGNIN_KEY") {
        return signin::keys::SealKey::from_hex(hex.trim()).context("CARTO_SIGNIN_KEY");
    }
    if config.database_path == ":memory:" {
        return signin::keys::SealKey::ephemeral();
    }
    let path = std::path::Path::new(&config.database_path).with_file_name("signin.key");
    signin::keys::SealKey::load_or_create(&path)
        .with_context(|| format!("sign-in key at {}", path.display()))
}

/// Accept connections until the listener task is aborted (server shutdown). Unlike
/// the old inline loop, a transient `accept()` error is logged and skipped rather
/// than terminating the server.
async fn accept_loop(
    listener: TcpListener,
    world: Arc<RwLock<state::SimWorld>>,
    config: Arc<SimConfig>,
    tx_snap: broadcast::Sender<bytes::Bytes>,
    conn: Arc<std::sync::Mutex<rusqlite::Connection>>,
    sessions: Arc<auth::Sessions>,
    chat: Arc<chat::ChatHub>,
    scripts: scripting::ScriptHandle,
    calls: Option<calls::CallsHandle>,
    prims: prims::PrimService,
    stats: Arc<stats::Stats>,
) {
    // Consecutive failures, for the backoff below. Reset by any success.
    let mut failures: u32 = 0;
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                // Back off, because the interesting `accept` errors are the
                // *persistent* ones. `EMFILE` (out of file descriptors) does not
                // clear because we looped; it clears when a connection closes, and
                // until then this loop is a core burnt at millions of iterations a
                // second, each writing a warn line — a log that fills the disk
                // while the server does nothing. Doubling from 50 ms to a second
                // costs a transient error almost nothing and gives a real one room
                // to resolve.
                failures = failures.saturating_add(1);
                let backoff = std::time::Duration::from_millis(50)
                    .saturating_mul(1 << failures.min(5))
                    .min(std::time::Duration::from_secs(1));
                tracing::warn!(failures, ?backoff, "accept failed: {e:#}");
                tokio::time::sleep(backoff).await;
                continue;
            }
        };
        failures = 0;
        // Nagle's algorithm holds a small write back until the previous one is
        // acknowledged, so on a link with any real round-trip time — a phone on
        // mobile data, which is the case this server is deployed for — the 20 Hz
        // snapshots do not leave at 20 Hz. Two or three of them coalesce into one
        // segment and arrive together, which the client's prediction can only read
        // as a stalled stream followed by a jump. There is nothing to gain from
        // the coalescing either: every frame here is already a complete message
        // that the peer wants as soon as it exists.
        //
        // Best-effort: a socket that refuses the option is still a usable socket,
        // and refusing the connection over it would be the worse trade.
        if let Err(e) = stream.set_nodelay(true) {
            tracing::warn!(%addr, "set_nodelay failed, connection will be Nagle-delayed: {e:#}");
        }
        tracing::info!(%addr, "accepted");
        let world_c = world.clone();
        let cfg_c = config.clone();
        let rx = tx_snap.subscribe();
        let conn_c = conn.clone();
        let sessions_c = sessions.clone();
        let chat_c = chat.clone();
        let scripts_c = scripts.clone();
        let calls_c = calls.clone();
        let prims_c = prims.clone();
        // Plain TCP is local tooling and dev servers: no address an app could
        // name itself on, so its sessions are `unknown`.
        let arrival = stats::Arrival {
            stats: stats.clone(),
            client: stats::client_tag(None),
            build: String::new(),
            ip: addr.ip().to_string(),
            user_agent: String::new(),
        };
        tokio::spawn(async move {
            if let Err(e) = net::handle_connection(
                net::tcp_frames(stream),
                world_c,
                cfg_c,
                rx,
                conn_c,
                sessions_c,
                chat_c,
                scripts_c,
                calls_c,
                prims_c,
                arrival,
            )
            .await
            {
                tracing::warn!(%addr, "client ended: {e:#}");
            }
        });
    }
}
