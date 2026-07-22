//! Solo/dev mode (ADR-016): run an in-process `vibe_sim` on loopback and connect
//! to it as a normal authenticated client. Feature-gated (`solo`, default on).
//!
//! There is deliberately no in-memory bypass — the embedded server binds a real
//! loopback TCP socket and the client goes through the same `connect` + auth path
//! `--connect` uses, so external connect stays first-class and always-tested.

use std::sync::mpsc;
use std::sync::Arc;

/// Dev account solo mode provisions and the client logs in as (matches
/// `network::connect_credentials` defaults).
const DEV_USER: &str = "dev";
const DEV_PASSWORD: &str = "dev";

/// Start an in-process sim on an ephemeral loopback port, ensure the dev account
/// exists, and return the bound `host:port` to point the client at. The server
/// and its tokio runtime live on a detached thread for the process lifetime;
/// process exit tears them down.
pub fn start() -> anyhow::Result<String> {
    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    std::thread::Builder::new()
        .name("solo-sim".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Err(format!("solo runtime: {e}")));
                    return;
                }
            };
            rt.block_on(async move {
                let mut config = vibe_sim::SimConfig::default();
                config.listen = "127.0.0.1:0".into(); // ephemeral — no clash with a manual sim
                let config = Arc::new(config);

                if let Err(e) = ensure_dev_account(&config.database_path) {
                    let _ = tx.send(Err(format!("solo dev account: {e:#}")));
                    return;
                }
                match vibe_sim::serve(config).await {
                    Ok(handle) => {
                        let _ = tx.send(Ok(handle.local_addr.to_string()));
                        // Hold the handle (and this runtime) alive for the process
                        // lifetime; dropping it would abort the server.
                        std::future::pending::<()>().await;
                        drop(handle);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("solo serve: {e:#}")));
                    }
                }
            });
        })?;

    rx.recv()
        .map_err(|_| anyhow::anyhow!("solo sim thread exited before binding"))?
        .map_err(|e| anyhow::anyhow!(e))
}

/// Provision the dev account before serving (idempotent). Opens its own short
/// connection; `serve` opens the same DB file afterward.
fn ensure_dev_account(db_path: &str) -> anyhow::Result<()> {
    let conn = vibe_sim::db::open_and_migrate(db_path)?;
    vibe_sim::ensure_account(&conn, DEV_USER, DEV_PASSWORD)?;
    Ok(())
}
