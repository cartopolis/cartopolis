use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use clap::{Parser, Subcommand};

/// Cartopolis atlas: map data and terrain tiles (`docs/notes/atlas.md`).
#[derive(Parser)]
struct Cli {
    /// Configuration file. Optional; defaults apply where it is missing.
    #[arg(long, default_value = "atlas.toml")]
    config: PathBuf,
    /// Override the listen address from the configuration.
    #[arg(long)]
    listen: Option<std::net::SocketAddr>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run datasets from a shell, with no console and no simulator — the same code
    /// path the console drives, with the log on stderr. For a first extraction on
    /// a fresh box, a cron job, and diffing a port against what it replaced.
    Run {
        /// Output root: the tree Caddy serves as 3dbag.cartopolis.org.
        #[arg(long, default_value = "site")]
        out: PathBuf,
        /// A named area (`groningen`, `amsterdam`, …) instead of a bbox.
        #[arg(long)]
        area: Option<String>,
        /// The coverage name stamped into every manifest.
        #[arg(long)]
        name: Option<String>,
        /// RD New bbox: xmin ymin xmax ymax.
        #[arg(long, num_args = 4, allow_negative_numbers = true)]
        bbox: Option<Vec<f64>>,
        /// Comma-separated dataset ids; every dataset when omitted.
        #[arg(long, value_delimiter = ',')]
        datasets: Vec<String>,
        /// Fetch every source anew instead of reusing what was fetched within the
        /// last month (`pipeline::http::SOURCE_CACHE_TTL`).
        #[arg(long)]
        fresh: bool,
    },
}

struct Stderr;

impl cartopolis_atlas::pipeline::Sink for Stderr {
    fn log(&self, line: String) {
        eprintln!("  {line}");
    }
    fn progress(&self, _update: cartopolis_atlas::pipeline::Progress) {
        // The log lines carry the running totals; a progress bar on a pipe is noise.
    }
}

fn run_datasets(
    out: PathBuf,
    area: Option<String>,
    name: Option<String>,
    bbox: Option<Vec<f64>>,
    datasets: Vec<String>,
    data_dir: &std::path::Path,
    fresh: bool,
) -> anyhow::Result<()> {
    use cartopolis_atlas::pipeline::{dataset, Ctx, DATASETS};
    let area = match area {
        Some(id) => Some(cartopolis_atlas::areas::area(&id).ok_or_else(|| {
            anyhow::anyhow!(
                "no such area '{id}' — one of: {}",
                cartopolis_atlas::areas::AREAS
                    .iter()
                    .map(|a| a.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?),
        None => None,
    };
    let bbox: [f64; 4] = match (area, bbox) {
        (Some(a), _) => a.bbox,
        (None, Some(b)) => [b[0], b[1], b[2], b[3]],
        (None, None) => cartopolis_atlas::areas::AREAS[0].bbox,
    };
    let name = name.unwrap_or_else(|| area.map_or("groningen", |a| a.id).to_string());
    let wanted: Vec<&str> = if datasets.is_empty() {
        DATASETS.iter().map(|d| d.id).collect()
    } else {
        datasets.iter().map(String::as_str).collect()
    };
    for id in &wanted {
        anyhow::ensure!(
            dataset(id).is_some(),
            "no such dataset '{id}' — one of: {}",
            DATASETS.iter().map(|d| d.id).collect::<Vec<_>>().join(", ")
        );
    }
    let ctx = Ctx::new(
        bbox,
        &name,
        &out,
        Arc::new(AtomicBool::new(false)),
        Arc::new(Stderr),
    )
    .with_data_dir(data_dir)
    .with_fresh(fresh);
    cartopolis_atlas::pipeline::http::sweep_source_cache(data_dir);
    for id in wanted {
        let d = dataset(id).expect("checked above");
        eprintln!(
            "{} '{name}' bbox(RD)={bbox:?} -> {}",
            d.label,
            out.display()
        );
        let started = std::time::Instant::now();
        let result = (d.run)(&ctx).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
        eprintln!(
            "{id}: {} across {} cells in {}s\n",
            result
                .counts
                .iter()
                .map(|c| format!("{} {}", c.n, c.label))
                .collect::<Vec<_>>()
                .join(", "),
            result.cells,
            started.elapsed().as_secs()
        );
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    if let Some(Command::Run {
        out,
        area,
        name,
        bbox,
        datasets,
        fresh,
    }) = cli.command
    {
        // The configuration names atlas's store, which the `terrain` dataset fills.
        let config = cartopolis_atlas::config::AtlasConfig::load(&cli.config)?;
        return run_datasets(out, area, name, bbox, datasets, &config.data_dir, fresh);
    }
    let mut config = cartopolis_atlas::config::AtlasConfig::load(&cli.config)?;
    if let Some(listen) = cli.listen {
        config.listen = listen;
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(cartopolis_atlas::run(config))
}
