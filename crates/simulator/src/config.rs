use anyhow::Context;
use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Server configuration (ADR-014).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SimConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    /// HTTP listener for the browser transport (`/ws`) and the account admin API
    /// (`/api/admin/*`), or `None` for none.
    ///
    /// Off by default, so solo mode and every existing test get exactly the TCP
    /// listener they had. A deployment turns it on; the listener it wants is a
    /// loopback or docker-gateway address, because it speaks plain HTTP and the
    /// TLS in front of it is Caddy's.
    #[serde(default)]
    pub http_listen: Option<String>,
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
    /// Words masked in chat (`cartopolis_core::moderation`).
    ///
    /// Defaults to the built-in seed list. An operator replaces it wholesale —
    /// there is no "add to the defaults" mode, because the words a community
    /// needs blocked are not a superset of anyone else's and a half-inherited
    /// list is impossible to reason about.
    #[serde(default = "default_moderation_words")]
    pub moderation_words: Vec<String>,
    /// A file of words, one per line, `#` for a comment. Read at startup into
    /// [`SimConfig::moderation_words`], replacing whatever was there.
    ///
    /// A path rather than a long TOML array because this is the one setting an
    /// operator edits under pressure, mid-incident, and a text file is what a
    /// text editor and `>>` are good at.
    #[serde(default)]
    pub moderation_words_file: Option<String>,
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
    cartopolis_core::world::DEFAULT_TILE_URL_TEMPLATE.into()
}

fn default_texture_dir() -> String {
    "data/textures".into()
}

fn default_moderation_words() -> Vec<String> {
    cartopolis_core::moderation::DEFAULT_WORDS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            http_listen: None,
            database_path: default_db(),
            tick_hz: default_tick_hz(),
            aoi_radius: default_aoi(),
            osm_tile_url_template: default_osm_tile_url_template(),
            texture_dir: default_texture_dir(),
            moderation_words: default_moderation_words(),
            moderation_words_file: None,
        }
    }
}

impl SimConfig {
    /// `cartopolis.toml` (optional) + env `CARTO_*` (e.g. `CARTO_listen`, `CARTO_database_path`).
    pub fn load() -> anyhow::Result<Self> {
        let mut figment = Figment::new().merge(Serialized::defaults(SimConfig::default()));
        if Path::new("cartopolis.toml").exists() {
            figment = figment.merge(Toml::file("cartopolis.toml"));
        }
        figment = figment.merge(Env::prefixed("CARTO_"));
        let mut config: Self = figment.extract()?;
        config.load_moderation_words()?;
        Ok(config)
    }

    /// Resolve [`SimConfig::moderation_words_file`] into the in-memory list.
    ///
    /// A missing or unreadable file is an **error**, not a fallback to the seed
    /// list. Silently running with different filtering than the operator
    /// configured is the failure this whole surface exists to avoid, and it would
    /// only be noticed by whoever it failed to protect.
    pub fn load_moderation_words(&mut self) -> anyhow::Result<()> {
        let Some(ref path) = self.moderation_words_file else {
            return Ok(());
        };
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read moderation_words_file {path}"))?;
        self.moderation_words = text.lines().map(str::to_string).collect();
        Ok(())
    }

    /// The compiled chat filter. Built per caller — it is a few dozen short
    /// strings, and a connection builds one once.
    #[must_use]
    pub fn moderation_filter(&self) -> cartopolis_core::moderation::Filter {
        cartopolis_core::moderation::Filter::from_words(
            self.moderation_words.iter().map(String::as_str),
        )
    }

    /// CLI flags override file/env (ADR-014).
    pub fn apply_cli(&mut self, cli: &super::cli::SimCli) {
        if let Some(ref v) = cli.listen {
            self.listen.clone_from(v);
        }
        if let Some(ref v) = cli.http_listen {
            self.http_listen = Some(v.clone());
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
