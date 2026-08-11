//! `simulator` as a library: the headless simulation server, usable standalone via
//! the `simulator` binary or embedded in-process (viberfox solo/dev mode, ADR-016).
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
pub mod net;
pub mod scripting;
pub mod state;
pub mod textures;

pub use auth::{ensure_account, Sessions};
pub use chat::ChatHub;
pub use config::SimConfig;

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
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl SimHandle {
    /// Stop the server now. Equivalent to dropping the handle.
    pub fn shutdown(self) {
        // Drop runs the abort.
    }
}

impl Drop for SimHandle {
    fn drop(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
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

    let sessions = Arc::new(auth::Sessions::new());
    let chat = Arc::new(chat::ChatHub::new());

    let (tx_snap, _) = broadcast::channel::<Vec<u8>>(256);
    let script_handle = scripts_host.handle();
    let tick_task = tokio::spawn(net::tick_loop(
        world.clone(),
        config.clone(),
        tx_snap.clone(),
        scripts_host,
        chat.clone(),
    ));

    let listener = TcpListener::bind(&config.listen).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(%local_addr, "listening");

    let accept_task = tokio::spawn(accept_loop(
        listener,
        world,
        config,
        tx_snap,
        conn,
        sessions,
        chat,
        script_handle,
    ));

    Ok(SimHandle {
        local_addr,
        tasks: vec![tick_task, accept_task],
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
                stream, world_c, cfg_c, rx, tx_c, conn_c, sessions_c, chat_c, scripts_c,
            )
            .await
            {
                tracing::warn!(%addr, "client ended: {e:#}");
            }
        });
    }
}
