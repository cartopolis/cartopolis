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
//! It also serves the web UI (`web_root`) and atlas's map-data tree under
//! `/data/` (`static_files`), and points every service the file leaves at
//! cartopolis.org's address at its own `public_base_url` instead — tiles at the
//! public VersaTiles server. What is left to the operator is TLS in front.

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
    /// Make an admin account, or reset its password, then exit. Asks for the
    /// password unless `CARTO_ADMIN_PASSWORD` holds it.
    CreateAdmin {
        /// The account's name; asked for when left out.
        username: Option<String>,
    },
}

/// Environment variable that answers `create-admin`'s password question, for a
/// script; the simulator's `--create-admin` reads the same one.
const ADMIN_PASSWORD_ENV: &str = "CARTO_ADMIN_PASSWORD";

/// The built web UI, looked for beside this program when the file names no
/// `web_root`: where the server package puts it.
const WEB_DIR: &str = "web";

/// The simulator's HTTP listener when the file names none: loopback, so an
/// install never publishes plain HTTP by accident; TLS goes in front of it.
const DEFAULT_HTTP_LISTEN: &str = "127.0.0.1:4748";

/// Longest pause before atlas is started again; a run that lasted longer than
/// this resets the pause.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // `create-admin` is a question asked at a terminal: only what went wrong is
    // said there, not every migration the database runs on its way.
    let level = match cli.command {
        Some(Cmd::CreateAdmin { .. }) => "warn",
        _ => "info",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level)),
        )
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    match cli.command {
        Some(Cmd::Atlas) => {
            end_with_parent();
            runtime.block_on(cartopolis_atlas::run(atlas_config(&cli.config)?))
        }
        Some(Cmd::CreateAdmin { username }) => create_admin(&cli.config, username),
        None => runtime.block_on(run_both(&cli.config)),
    }
}

/// Create or repair an admin account in the database the server uses, without
/// starting it: a first install has no other way to an admin, and a forgotten
/// password is mended by running it again.
fn create_admin(file: &Path, username: Option<String>) -> anyhow::Result<()> {
    use std::io::Write;
    let sim = sim_config(file)?;
    let username = match username {
        Some(name) => name,
        None => {
            print!("Admin account name: ");
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim().to_string()
        }
    };
    anyhow::ensure!(!username.is_empty(), "an admin account needs a name");
    let password = match std::env::var(ADMIN_PASSWORD_ENV) {
        Ok(password) => password,
        Err(_) => {
            let password = rpassword::prompt_password(format!("Password for {username}: "))?;
            let again = rpassword::prompt_password("The same password again: ")?;
            anyhow::ensure!(
                password == again,
                "the two passwords differ; nothing changed"
            );
            password
        }
    };
    anyhow::ensure!(!password.is_empty(), "an admin account needs a password");
    if let Some(dir) = Path::new(&sim.database_path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = cartopolis_simulator::db::open_and_migrate(&sim.database_path)?;
    cartopolis_simulator::ensure_admin_account(&conn, &username, &password)?;
    println!(
        "{username} is an admin of {}; sign in there with it.",
        sim.public_base_url
    );
    Ok(())
}

/// The simulator's settings, with what one program implies filled in: an HTTP
/// listener, and atlas behind it.
fn sim_config(file: &Path) -> anyhow::Result<SimConfig> {
    let mut sim = SimConfig::load_from(file)?;
    if sim.http_listen.is_none() {
        sim.http_listen = Some(DEFAULT_HTTP_LISTEN.into());
    }
    let atlas = AtlasConfig::load_table(file, "atlas")?;
    if sim.atlas_url.is_none() {
        sim.atlas_url = Some(format!("http://{}", atlas.listen));
    }
    if sim.map_data_dir.is_none() {
        sim.map_data_dir = Some(atlas.site_dir.display().to_string());
    }
    if sim.web_root.is_none() {
        sim.web_root = web_beside_program();
    }
    own_services(&mut sim);
    Ok(sim)
}

/// The package's `web/`, when this program sits beside one.
fn web_beside_program() -> Option<String> {
    let dir = std::env::current_exe().ok()?.parent()?.join(WEB_DIR);
    dir.join("index.html")
        .is_file()
        .then(|| dir.display().to_string())
}

/// The public tile server with the schema and URL shape the client reads
/// (`cartopolis_core::world::DEFAULT_TILE_URL_TEMPLATE` is cartopolis.org's
/// mirror of it), for an install that hosts no tiles of its own.
const PUBLIC_TILES: &str = "https://tiles.versatiles.org/tiles/osm/{z}/{x}/{y}";

/// Every service the file leaves at cartopolis.org's address, pointed at this
/// server instead: it answers search, terrain and height through atlas and the
/// map data from its own tree. Tiles go to the public server unless named.
fn own_services(sim: &mut SimConfig) {
    use cartopolis_core::services as d;
    let base = sim.public_base_url.trim_end_matches('/').to_string();
    let own = |value: &mut String, default: &str, path: &str| {
        if value == default {
            *value = format!("{base}{path}");
        }
    };
    own(&mut sim.geocoder_url, d::DEFAULT_GEOCODER_URL, "/geocode");
    own(
        &mut sim.terrain_url,
        d::DEFAULT_TERRAIN_URL,
        "/terrain/{z}/{x}/{y}?grid=513&ahn=6",
    );
    own(&mut sim.height_url, d::DEFAULT_HEIGHT_URL, "/height");
    own(
        &mut sim.map_data_url,
        d::DEFAULT_MAP_DATA_URL,
        cartopolis_simulator::static_files::MAP_DATA_PATH,
    );
    if sim.osm_tile_url_template == cartopolis_core::world::DEFAULT_TILE_URL_TEMPLATE {
        sim.osm_tile_url_template = PUBLIC_TILES.into();
    }
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
    if sim.public_base_url == SimConfig::default().public_base_url {
        tracing::warn!(
            "public_base_url is not set in {}: clients will be sent to {} for search and map data; \
             set it to this server's https address",
            file.display(),
            sim.public_base_url
        );
    }
    if sim.web_root.is_none() {
        tracing::warn!(
            "no web UI: set web_root, or put the built one in {WEB_DIR}/ beside this program"
        );
    }
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
        assert_eq!(sim.geocoder_url, "https://maps.example.org/geocode");
        assert_eq!(sim.map_data_url, "https://maps.example.org/data");
        assert_eq!(sim.osm_tile_url_template, PUBLIC_TILES);
        assert_eq!(sim.map_data_dir.as_deref(), Some("data/atlas/site"));
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
