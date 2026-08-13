//! Headless simulation server binary — a thin wrapper over the `simulator` lib
//! (ADR-007). Loads layered config (ADR-014), serves, and runs until Ctrl-C.

use cartopolis_simulator::{cli::SimCli, db, ensure_admin_account, serve, SimConfig};
use clap::Parser;
use std::sync::Arc;

/// Environment variable carrying the password for `--create-admin`.
const ADMIN_PASSWORD_ENV: &str = "CARTO_ADMIN_PASSWORD";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let sim_cli = SimCli::parse();
    let mut config = SimConfig::load()?;
    config.apply_cli(&sim_cli);

    // Bootstrap, not a mode: create-or-repair the admin and exit, without ever
    // opening a listener. Run against the same database the server will use, so
    // the deploy script's ordering is "provision, then start" rather than "start,
    // then race the server for the file".
    if let Some(username) = sim_cli.create_admin.as_deref() {
        let password = std::env::var(ADMIN_PASSWORD_ENV).map_err(|_| {
            anyhow::anyhow!("--create-admin needs a password in ${ADMIN_PASSWORD_ENV}")
        })?;
        let conn = db::open_and_migrate(&config.database_path)?;
        let id = ensure_admin_account(&conn, username, &password)?;
        tracing::info!(%id, username, db = %config.database_path, "admin account ready");
        return Ok(());
    }

    let config = Arc::new(config);
    tracing::info!(
        listen = %config.listen,
        http_listen = config.http_listen.as_deref().unwrap_or("(off)"),
        db = %config.database_path,
        tick_hz = config.tick_hz,
        aoi = config.aoi_radius,
        tile_template = %config.osm_tile_url_template,
        texture_dir = %config.texture_dir,
        "simulator"
    );

    let handle = serve(config).await?;
    tracing::info!(addr = %handle.local_addr, "simulator ready");

    // Hold the handle so the server keeps running until Ctrl-C; on exit it drops
    // and the accept/tick tasks are aborted.
    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    Ok(())
}
