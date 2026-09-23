//! Atlas's configuration: `atlas.toml`, then `CARTO_ATLAS_*` from the environment,
//! then the command line.
//!
//! **Unknown keys are an error.** A misspelt key that silently does nothing is how
//! a service ends up running on defaults nobody chose — OpenSimulator's `.ini`
//! sprawl is the cautionary tale `docs/notes/atlas.md` cites, and the shot
//! manifest here already refuses unknown keys for the same reason.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AtlasConfig {
    /// Where the HTTP surface listens. Loopback by default: a deployment puts
    /// Caddy in front of it, as it does the simulator's HTTP listener.
    pub listen: SocketAddr,
    /// Where built and cached data lives (`terrain/{z}/{x}/{y}.ctt`, …).
    pub data_dir: PathBuf,
    /// Requests to PDOK allowed in flight at once. PDOK is a shared public
    /// service and we are a guest on it; the client's own height layer uses 3.
    pub upstream_concurrency: usize,
}

impl Default for AtlasConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 4749)),
            data_dir: PathBuf::from("data/atlas"),
            upstream_concurrency: 3,
        }
    }
}

impl AtlasConfig {
    /// Defaults, overlaid by the TOML file (if it exists), overlaid by the
    /// environment.
    pub fn load(file: &Path) -> anyhow::Result<Self> {
        Self::from_figment(
            Figment::from(Serialized::defaults(Self::default()))
                .merge(Toml::file(file))
                .merge(Env::prefixed("CARTO_ATLAS_")),
        )
    }

    fn from_figment(figment: Figment) -> anyhow::Result<Self> {
        let config: Self = figment.extract()?;
        anyhow::ensure!(
            config.upstream_concurrency >= 1,
            "upstream_concurrency must be at least 1"
        );
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_toml(text: &str) -> anyhow::Result<AtlasConfig> {
        AtlasConfig::from_figment(
            Figment::from(Serialized::defaults(AtlasConfig::default())).merge(Toml::string(text)),
        )
    }

    #[test]
    fn a_misspelt_key_is_refused() {
        let err = from_toml("lisen = \"0.0.0.0:80\"").unwrap_err().to_string();
        assert!(
            err.contains("lisen"),
            "the error did not name the key: {err}"
        );
    }

    #[test]
    fn a_file_overrides_only_what_it_names() {
        let c = from_toml("upstream_concurrency = 2").unwrap();
        assert_eq!(c.upstream_concurrency, 2);
        assert_eq!(c.listen, AtlasConfig::default().listen);
    }

    #[test]
    fn zero_upstream_slots_is_refused() {
        assert!(from_toml("upstream_concurrency = 0").is_err());
    }
}
