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
    #[arg(long, help = "SQLite path (overrides CARTO_database_path)")]
    pub database_path: Option<String>,
    #[arg(long, help = "Simulation tick rate (Hz)")]
    pub tick_hz: Option<f32>,
    #[arg(long, help = "AOI radius in sim units (ADR-012)")]
    pub aoi_radius: Option<f32>,
    #[arg(
        long,
        help = "OSM tile URL template with {z}/{x}/{y} (sent to clients at handshake)"
    )]
    pub osm_tile_url_template: Option<String>,
    #[arg(long, help = "Directory serving texture PNG files to clients")]
    pub texture_dir: Option<String>,
}
