use std::path::PathBuf;

use clap::Parser;

/// Cartopolis atlas: map data and terrain tiles (`docs/notes/atlas.md`).
#[derive(Parser)]
struct Cli {
    /// Configuration file. Optional; defaults apply where it is missing.
    #[arg(long, default_value = "atlas.toml")]
    config: PathBuf,
    /// Override the listen address from the configuration.
    #[arg(long)]
    listen: Option<std::net::SocketAddr>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let mut config = cartopolis_atlas::config::AtlasConfig::load(&cli.config)?;
    if let Some(listen) = cli.listen {
        config.listen = listen;
    }
    cartopolis_atlas::run(config).await
}
