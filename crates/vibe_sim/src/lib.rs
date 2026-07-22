//! `vibe_sim` as a library: the headless simulation server, usable standalone via
//! the `vibe_sim` binary or embedded in-process (viberfox solo/dev mode, ADR-016).
//!
//! Embedding runs the *real* server on a background tokio runtime and connects to
//! it over loopback TCP — the same wire protocol and (once ADR-016 lands) the same
//! auth handshake an external sim uses. There is deliberately no in-memory bypass:
//! solo mode and `--connect` share one client path (see ADR-016 "Solo/dev mode").

pub mod cli;
pub mod config;
pub mod db;
pub mod net;
pub mod state;
pub mod textures;

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
    let conn = Arc::new(std::sync::Mutex::new(conn));

    let world = Arc::new(RwLock::new(state::SimWorld::new(
        regions,
        prims,
        config.aoi_radius,
        conn.clone(),
    )));

    let (tx_snap, _) = broadcast::channel::<Vec<u8>>(256);
    let tick_task = tokio::spawn(net::tick_loop(world.clone(), config.clone(), tx_snap.clone()));

    let listener = TcpListener::bind(&config.listen).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(%local_addr, "listening");

    let accept_task = tokio::spawn(accept_loop(listener, world, config, tx_snap, conn));

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
) {
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("accept failed: {e:#}");
                continue;
            }
        };
        tracing::info!(%addr, "accepted");
        let world_c = world.clone();
        let cfg_c = config.clone();
        let rx = tx_snap.subscribe();
        let tx_c = tx_snap.clone();
        let conn_c = conn.clone();
        tokio::spawn(async move {
            if let Err(e) = net::handle_connection(stream, world_c, cfg_c, rx, tx_c, conn_c).await {
                tracing::warn!(%addr, "client ended: {e:#}");
            }
        });
    }
}
