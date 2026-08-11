//! Headless simulation server binary — a thin wrapper over the `simulator` lib
//! (ADR-007). Loads layered config (ADR-014), serves, and runs until Ctrl-C.

use cartopolis_simulator::{cli::SimCli, serve, SimConfig};
use clap::Parser;
use std::sync::Arc;

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
    let config = Arc::new(config);
    tracing::info!(
        listen = %config.listen,
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
