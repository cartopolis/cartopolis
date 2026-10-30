//! `cartopolis_atlas`: everything around the world that is not the simulation.
//!
//! `docs/notes/atlas.md` has the design and the rules it keeps. It serves the
//! terrain tiles the screen-driven tile tree stands on
//! (`docs/notes/level-of-detail-tile-tree.md`) and runs the map data pipelines
//! (`pipeline`, driven through `api`); the accounts arrive here later.

pub mod ahn6;
pub mod api;
pub mod areas;
pub mod auth;
pub mod config;
pub mod coverage;
pub mod designs;
pub mod http;
pub mod jobs;
pub mod overrides;
pub mod pipeline;
pub mod plans;
pub mod satellites;
pub mod stamped;
pub mod terrain;

use std::sync::Arc;

/// Serve until the process is told to stop.
pub async fn run(config: config::AtlasConfig) -> anyhow::Result<()> {
    // AHN6 where it has been published, AHN4 from PDOK everywhere else — for the
    // terrain tiles and the `/height` patches alike, so the ground and the decks
    // standing on it are measured by the same flight.
    let heights: Arc<dyn terrain::HeightSource> = Arc::new(ahn6::Ahn6::new(
        &config.data_dir,
        Arc::new(terrain::Pdok::new()),
    ));
    let terrain = terrain::TerrainService::new(
        heights.clone(),
        &config.data_dir,
        config.upstream_concurrency,
    );
    let api = api::ApiState {
        auth: Arc::new(auth::Auth::new(Box::new(auth::Simulator {
            api: config.admin_api.clone(),
        }))),
        runner: jobs::Runner::new(&config.site_dir, jobs::default_registry())
            .with_data_dir(&config.data_dir)
            .with_upstream(config.upstream_concurrency),
        out_root: config.site_dir.clone(),
        designs: Arc::new(designs::Designs::new(&config.data_dir, &config.site_dir)),
        overrides: Arc::new(overrides::Overrides::new(
            &config.data_dir,
            &config.site_dir,
        )),
    };
    let state = http::AtlasState {
        terrain: Arc::new(terrain),
        heights: Arc::new(terrain::HeightPatches::new(
            heights,
            config.upstream_concurrency,
        )),
        api,
        satellites: {
            let mirror = Arc::new(satellites::SatelliteMirror::new(
                &config.data_dir,
                &config.satellites_source,
            ));
            mirror.spawn_refresh();
            mirror
        },
        plans: {
            let mirror = Arc::new(plans::PlansMirror::new(
                &config.data_dir,
                config.plans_refresh,
            ));
            mirror.spawn_refresh();
            mirror
        },
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
