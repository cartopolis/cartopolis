//! CLI overrides (ADR-014).

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "simulator", about = "Headless Cartopolis simulation server")]
pub struct SimCli {
    #[arg(
        long,
        help = "TCP listen address (overrides cartopolis.toml / CARTO_listen)"
    )]
    pub listen: Option<String>,
    #[arg(
        long,
        help = "HTTP listen address for the browser transport (/ws) and the admin API"
    )]
    pub http_listen: Option<String>,
    /// Create-or-repair an admin account, then exit without serving.
    ///
    /// Takes the password from `CARTO_ADMIN_PASSWORD` rather than a flag: an
    /// argument is in the process table and in the operator's shell history, and
    /// this is the one credential that can grant itself everything else.
    #[arg(
        long,
        value_name = "USERNAME",
        help = "Create/repair an admin account (password from CARTO_ADMIN_PASSWORD) and exit"
    )]
    pub create_admin: Option<String>,
    #[arg(long, help = "SQLite path (overrides CARTO_database_path)")]
    pub database_path: Option<String>,
    #[arg(long, help = "Simulation tick rate (Hz)")]
    pub tick_hz: Option<f32>,
    #[arg(long, help = "Most prims this world may hold at once")]
    pub max_prims: Option<u32>,
    #[arg(long, help = "Most scripted prims this world may hold at once")]
    pub max_scripts: Option<u32>,
    #[arg(
        long,
        help = "OSM tile URL template with {z}/{x}/{y} (sent to clients at handshake)"
    )]
    pub osm_tile_url_template: Option<String>,
    #[arg(long, help = "Directory serving texture PNG files to clients")]
    pub texture_dir: Option<String>,
}
