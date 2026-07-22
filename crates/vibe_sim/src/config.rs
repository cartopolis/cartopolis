use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Server configuration (ADR-014).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SimConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_db")]
    pub database_path: String,
    #[serde(default = "default_tick_hz")]
    pub tick_hz: f32,
    #[serde(default = "default_aoi")]
    pub aoi_radius: f32,
    /// Placeholders `{z}`, `{x}`, `{y}` for client tile fetch (ADR-004 / ADR-014).
    #[serde(default = "default_osm_tile_url_template")]
    pub osm_tile_url_template: String,
    /// Directory from which *.png texture files are served to clients.
    #[serde(default = "default_texture_dir")]
    pub texture_dir: String,
}

fn default_listen() -> String {
    "127.0.0.1:4747".into()
}

fn default_db() -> String {
    "data/world.db".into()
}

fn default_tick_hz() -> f32 {
    20.0
}

fn default_aoi() -> f32 {
    500.0
}

fn default_osm_tile_url_template() -> String {
    "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into()
}

fn default_texture_dir() -> String {
    "data/textures".into()
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            database_path: default_db(),
            tick_hz: default_tick_hz(),
            aoi_radius: default_aoi(),
            osm_tile_url_template: default_osm_tile_url_template(),
            texture_dir: default_texture_dir(),
        }
    }
}

impl SimConfig {
    /// `vibe.toml` (optional) + env `VIBE_*` (e.g. `VIBE_listen`, `VIBE_database_path`).
    pub fn load() -> anyhow::Result<Self> {
        let mut figment = Figment::new().merge(Serialized::defaults(SimConfig::default()));
        if Path::new("vibe.toml").exists() {
            figment = figment.merge(Toml::file("vibe.toml"));
        }
        figment = figment.merge(Env::prefixed("VIBE_"));
        Ok(figment.extract()?)
    }

    /// CLI flags override file/env (ADR-014).
    pub fn apply_cli(&mut self, cli: &super::cli::SimCli) {
        if let Some(ref v) = cli.listen {
            self.listen.clone_from(v);
        }
        if let Some(ref v) = cli.database_path {
            self.database_path.clone_from(v);
        }
        if let Some(v) = cli.tick_hz {
            self.tick_hz = v;
        }
        if let Some(v) = cli.aoi_radius {
            self.aoi_radius = v;
        }
        if let Some(ref v) = cli.osm_tile_url_template {
            self.osm_tile_url_template.clone_from(v);
        }
        if let Some(ref v) = cli.texture_dir {
            self.texture_dir.clone_from(v);
        }
    }
}
