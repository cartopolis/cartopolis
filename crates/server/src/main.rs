//! `cartopolis-server`: the simulator and atlas as one program to install.
//!
//! One binary, one settings file and one HTTP address. The simulator's settings
//! sit at the top of `cartopolis.toml` (`SimConfig`), atlas's under `[atlas]`
//! (`AtlasConfig`). Run with no subcommand, this process runs the simulator and
//! starts **itself again** as `cartopolis-server atlas`: atlas is never in the
//! simulator's runtime, because its imports peak at gigabytes and must neither
//! cost the tick nor take the world down when they fail (`docs/notes/atlas.md`).
//! The child is restarted when it ends, with a growing pause, and ends itself
//! when this process goes (it holds the child's stdin; the end of it is the
//! signal). The simulator's HTTP listener answers atlas's paths by passing them
//! to it (`cartopolis_simulator::atlas_proxy`), so a reverse proxy in front needs
//! one upstream, not two.
//!
//! What is left to the operator: TLS in front of the HTTP listener, and the
//! static files (the web UI, the map-data tree), for now (`docs/notes/the-server-stack.md`).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cartopolis_atlas::config::AtlasConfig;
use cartopolis_simulator::SimConfig;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "cartopolis-server",
    about = "The Cartopolis simulator and atlas, as one program"
)]
struct Cli {
    /// The settings: the simulator's at the top, atlas's under `[atlas]`.
    #[arg(long, default_value = "cartopolis.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run atlas alone: what this program starts beside the simulator.
    Atlas,
}

/// The simulator's HTTP listener when the file names none: loopback, so an
/// install never publishes plain HTTP by accident; TLS goes in front of it.
const DEFAULT_HTTP_LISTEN: &str = "127.0.0.1:4748";

/// Longest pause before atlas is started again; a run that lasted longer than
/// this resets the pause.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    match cli.command {
        Some(Cmd::Atlas) => {
            end_with_parent();
            runtime.block_on(cartopolis_atlas::run(atlas_config(&cli.config)?))
        }
        None => runtime.block_on(run_both(&cli.config)),
    }
}

/// The simulator's settings, with what one program implies filled in: an HTTP
/// listener, and atlas behind it.
fn sim_config(file: &Path) -> anyhow::Result<SimConfig> {
    let mut sim = SimConfig::load_from(file)?;
    if sim.http_listen.is_none() {
        sim.http_listen = Some(DEFAULT_HTTP_LISTEN.into());
    }
    if sim.atlas_url.is_none() {
        let atlas = AtlasConfig::load_table(file, "atlas")?;
        sim.atlas_url = Some(format!("http://{}", atlas.listen));
    }
    Ok(sim)
}

/// Atlas's settings, asking the simulator beside it who is an operator unless
/// the file says otherwise.
fn atlas_config(file: &Path) -> anyhow::Result<AtlasConfig> {
    let mut atlas = AtlasConfig::load_table(file, "atlas")?;
    if atlas.admin_api == AtlasConfig::default().admin_api {
        let sim = sim_config(file)?;
        if let Some(listen) = sim.http_listen {
            atlas.admin_api = format!("http://{listen}");
        }
    }
    Ok(atlas)
}

async fn run_both(file: &Path) -> anyhow::Result<()> {
    let sim = sim_config(file)?;
    tracing::info!(
        listen = %sim.listen,
        http_listen = sim.http_listen.as_deref().unwrap_or_default(),
        atlas = sim.atlas_url.as_deref().unwrap_or_default(),
        db = %sim.database_path,
        "cartopolis-server"
    );
    let file = file.to_path_buf();
    std::thread::Builder::new()
        .name("atlas-supervisor".into())
        .spawn(move || supervise_atlas(&file))?;
    let mut handle = cartopolis_simulator::serve(Arc::new(sim)).await?;
    tracing::info!(addr = %handle.local_addr, "simulator ready");
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

/// Run `cartopolis-server atlas` for as long as this process lives, starting it
/// again whenever it ends: at once after a long run, after a doubling pause
/// after a short one, so a crash on start does not spin.
fn supervise_atlas(file: &Path) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = Instant::now();
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                tracing::error!("cannot find this program to start atlas: {e}");
                return;
            }
        };
        let status = Command::new(exe)
            .arg("--config")
            .arg(file)
            .arg("atlas")
            .stdin(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                // Held, never written: the child ends when this end closes.
                let _stdin = child.stdin.take();
                child.wait()
            });
        match status {
            Ok(s) => tracing::warn!("atlas ended ({s}); starting it again"),
            Err(e) => tracing::error!("atlas could not be started: {e}"),
        }
        if started.elapsed() > MAX_BACKOFF {
            backoff = Duration::from_secs(1);
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// In the child: end when the parent does, which closes our stdin.
fn end_with_parent() {
    std::thread::spawn(|| {
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
        tracing::info!("cartopolis-server went away; atlas ends too");
        std::process::exit(0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_file_gives_both_their_settings_and_ties_them_together() {
        let dir = std::env::temp_dir().join(format!("cartopolis-server-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cartopolis.toml");
        std::fs::write(
            &file,
            "http_listen = \"127.0.0.1:5748\"\npublic_base_url = \"https://maps.example.org\"\n\
             [atlas]\nlisten = \"127.0.0.1:5749\"\ndata_dir = \"/srv/atlas\"\n",
        )
        .unwrap();
        let sim = sim_config(&file).unwrap();
        assert_eq!(sim.atlas_url.as_deref(), Some("http://127.0.0.1:5749"));
        assert_eq!(sim.public_base_url, "https://maps.example.org");
        let atlas = atlas_config(&file).unwrap();
        assert_eq!(atlas.admin_api, "http://127.0.0.1:5748");
        assert_eq!(atlas.data_dir, PathBuf::from("/srv/atlas"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_empty_file_still_makes_a_server() {
        let file = std::env::temp_dir().join("cartopolis-server-none.toml");
        let sim = sim_config(&file).unwrap();
        assert_eq!(sim.http_listen.as_deref(), Some(DEFAULT_HTTP_LISTEN));
        assert_eq!(sim.atlas_url.as_deref(), Some("http://127.0.0.1:4749"));
    }
}
