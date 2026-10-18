//! Headless simulation server binary — a thin wrapper over the `simulator` lib
//! (ADR-007). Loads layered config (ADR-014), serves, and runs until Ctrl-C.

use cartopolis_simulator::{cli::SimCli, db, ensure_admin_account, serve, SimConfig};
use clap::Parser;
use std::sync::Arc;

/// Environment variable carrying the password for `--create-admin`.
const ADMIN_PASSWORD_ENV: &str = "CARTO_ADMIN_PASSWORD";

/// Where the HTTP surface goes when nobody said. Loopback, so a development
/// server never publishes an unencrypted API to the network by accident.
const DEV_HTTP_LISTEN: &str = "127.0.0.1:4748";

/// Where the call forwarder goes when nobody said: loopback, next to the HTTP
/// surface, so the documented dev loop can place a call between two clients
/// on one machine.
const DEV_CALLS_LISTEN: &str = "127.0.0.1:4749";

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
        // The deploy smoke-tests migrations by running this against a copy of
        // the live files: opening `stats.db` here makes a broken stats
        // migration fail the deploy instead of the server's next start.
        cartopolis_simulator::stats::Stats::open_beside(&config.database_path)?;
        let id = ensure_admin_account(&conn, username, &password)?;
        tracing::info!(%id, username, db = %config.database_path, "admin account ready");
        return Ok(());
    }

    // The dev loop is `cargo run -p cartopolis_simulator`, and with no HTTP
    // listener that server has no model store, no registration and nothing a
    // browser can connect to — the Models panel in the client is inert and says
    // "sign in to a server" to somebody who is signed in. So the *binary*
    // defaults one on loopback. `SimConfig::default` deliberately still says
    // `None`: the tests and every embedded use of `serve` get exactly the TCP
    // listener they had.
    //
    // A port already in use is not fatal here, unlike an `--http-listen` that was
    // asked for: a second dev sim on the same machine must still boot.
    if config.http_listen.is_none() {
        match std::net::TcpListener::bind(DEV_HTTP_LISTEN) {
            Ok(probe) => {
                drop(probe);
                config.http_listen = Some(DEV_HTTP_LISTEN.to_string());
            }
            Err(e) => tracing::warn!(
                "no --http-listen given and {DEV_HTTP_LISTEN} is taken ({e}); \
                 running without an HTTP surface, so this server has no model store"
            ),
        }
    }

    // The same default for calls, and the same leniency about a taken port.
    if config.calls_listen.is_none() {
        match std::net::UdpSocket::bind(DEV_CALLS_LISTEN) {
            Ok(probe) => {
                drop(probe);
                config.calls_listen = Some(DEV_CALLS_LISTEN.to_string());
            }
            Err(e) => tracing::warn!(
                "no --calls-listen given and {DEV_CALLS_LISTEN} is taken ({e}); \
                 running without calls"
            ),
        }
    }

    let config = Arc::new(config);
    tracing::info!(
        listen = %config.listen,
        http_listen = config.http_listen.as_deref().unwrap_or("(off)"),
        calls_listen = config.calls_listen.as_deref().unwrap_or("(off)"),
        db = %config.database_path,
        tick_hz = config.tick_hz,
        max_prims = config.max_prims,
        max_scripts = config.max_scripts,
        tile_template = %config.osm_tile_url_template,
        texture_dir = %config.texture_dir,
        asset_dir = %config.asset_dir,
        // What clients are told about the model API, which is the one thing a
        // silent failure here looks like from the other side.
        api_base = %{
            let base = config.resolved_api_base_url();
            if base.is_empty() { "(client derives it)".to_string() } else { base }
        },
        "simulator"
    );

    let mut handle = serve(config).await?;
    tracing::info!(addr = %handle.local_addr, "simulator ready");

    // Hold the handle so the server keeps running until Ctrl-C; on exit it drops
    // and the accept/tick tasks are aborted. Supervised, not merely held: a
    // background task ending (tick loop most of all) is a server that looks
    // healthy and serves a frozen world, so it becomes a non-zero exit and a
    // systemd restart rather than a mystery.
    tokio::select! {
        r = tokio::signal::ctrl_c() => {
            r?;
            tracing::info!("shutting down");
            Ok(())
        }
        e = handle.task_ended() => {
            tracing::error!("{e:#}");
            Err(e)
        }
    }
}
