//! `simulator` as a library: the headless simulation server, usable standalone via
//! the `simulator` binary or embedded in-process (cartopolis solo/dev mode, ADR-016).
//!
//! Embedding runs the *real* server on a background tokio runtime and connects to
//! it over loopback TCP — the same wire protocol and (once ADR-016 lands) the same
//! auth handshake an external sim uses. There is deliberately no in-memory bypass:
//! solo mode and `--connect` share one client path (see ADR-016 "Solo/dev mode").

pub mod auth;
pub mod chat;
pub mod cli;
pub mod config;
pub mod db;
pub mod http;
pub mod net;
pub mod scripting;
pub mod state;
pub mod textures;

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
    let (regions, prims) = db::load_world(&conn)?;
    let caps = db::load_region_caps(&conn)?;
    let scripts = db::load_prim_scripts(&conn)?;
    let conn = Arc::new(std::sync::Mutex::new(conn));

    let world = Arc::new(RwLock::new(state::SimWorld::new(
        regions,
        prims,
        config.aoi_radius,
        conn.clone(),
        caps,
        scripts.iter().map(|(id, _)| *id).collect(),
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

    let (tx_snap, _) = broadcast::channel::<Vec<u8>>(256);
    let script_handle = scripts_host.handle();
    // Each task returns a label so `task_ended` can say *which* one died.
    let mut tasks = tokio::task::JoinSet::new();
    {
        let world = world.clone();
        let config = config.clone();
        let tx_snap = tx_snap.clone();
        let chat = chat.clone();
        tasks.spawn(async move {
            net::tick_loop(world, config, tx_snap, scripts_host, chat).await;
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
    if let Some(ref addr) = config.http_listen {
        let state = http::HttpState {
            world: world.clone(),
            config: config.clone(),
            tx_snap: tx_snap.clone(),
            conn: conn.clone(),
            sessions: sessions.clone(),
            chat: chat.clone(),
            scripts: script_handle.clone(),
        };
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
        )
        .await;
        "accept loop"
    });

    Ok(SimHandle {
        local_addr,
        http_addr,
        tasks,
    })
}

/// Accept connections until the listener task is aborted (server shutdown). Unlike
/// the old inline loop, a transient `accept()` error is logged and skipped rather
/// than terminating the server.
async fn accept_loop(
    listener: TcpListener,
    world: Arc<RwLock<state::SimWorld>>,
    config: Arc<SimConfig>,
    tx_snap: broadcast::Sender<Vec<u8>>,
    conn: Arc<std::sync::Mutex<rusqlite::Connection>>,
    sessions: Arc<auth::Sessions>,
    chat: Arc<chat::ChatHub>,
    scripts: scripting::ScriptHandle,
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
        let tx_c = tx_snap.clone();
        let conn_c = conn.clone();
        let sessions_c = sessions.clone();
        let chat_c = chat.clone();
        let scripts_c = scripts.clone();
        tokio::spawn(async move {
            if let Err(e) = net::handle_connection(
                net::tcp_frames(stream),
                world_c,
                cfg_c,
                rx,
                tx_c,
                conn_c,
                sessions_c,
                chat_c,
                scripts_c,
            )
            .await
            {
                tracing::warn!(%addr, "client ended: {e:#}");
            }
        });
    }
}
