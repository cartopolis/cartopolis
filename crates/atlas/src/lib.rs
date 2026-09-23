//! `cartopolis_atlas`: everything around the world that is not the simulation.
//!
//! `docs/notes/atlas.md` has the design and the rules it keeps. So far it serves
//! one thing, the terrain tiles the screen-driven tile tree stands on
//! (`docs/notes/level-of-detail-tile-tree.md`); the map data pipelines from
//! `jeroen/cartopy` and the accounts arrive here later, one at a time.

pub mod areas;
pub mod auth;
pub mod autopilot;
pub mod config;
pub mod console;
pub mod coverage;
pub mod http;
pub mod jobs;
pub mod pipeline;
pub mod terrain;

use std::sync::Arc;

/// Serve until the process is told to stop.
pub async fn run(config: config::AtlasConfig) -> anyhow::Result<()> {
    let terrain = terrain::TerrainService::new(
        Arc::new(terrain::Pdok::new()),
        &config.data_dir,
        config.upstream_concurrency,
    );
    let console = console::ConsoleState {
        auth: Arc::new(auth::Auth::new(Box::new(auth::Simulator {
            api: config.admin_api.clone(),
        }))),
        runner: jobs::Runner::new(&config.site_dir, jobs::default_registry()),
        out_root: config.site_dir.clone(),
        autopilot: config.autopilot_config.clone(),
    };
    let state = http::AtlasState {
        terrain: Arc::new(terrain),
        console,
    };
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(listen = %config.listen, data_dir = %config.data_dir.display(), "atlas serving");
    axum::serve(listener, http::router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
