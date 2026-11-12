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
    /// Run datasets from a shell, with no web UI and no simulator — the same code
    /// path the web UI drives, with the log on stderr. For a first extraction on
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
    /// Write official-area sets for scenario layers (`official_areas`) into
    /// the served tree, where the client fetches them.
    Areas {
        /// `cbs-2024` (buurt to country, with figures), `ses-2023` (status
        /// scores 2014–2023), `pbl-2050` (the forecast to 2050), `wlo-2025`
        /// (the WLO futures to 2060), `ne-admin1-europe` or `ne-admin0-europe`.
        set: String,
        /// For the `ne-` sets: a GeoJSON file or URL instead of Natural
        /// Earth's; for `wlo-2025`, the workbook instead of PBL's.
        #[arg(long)]
        from: Option<String>,
        /// The directory the area files go in: the served site tree's `areas/`.
        #[arg(long, default_value = "site/areas")]
        out: PathBuf,
    },
    /// The sea floor's depth over the world, as the grey Web-Mercator image
    /// the client embeds (`ocean_depth`); written once and committed.
    OceanDepth {
        #[arg(long, default_value = "assets/globe/ocean_depth.png")]
        out: PathBuf,
        #[arg(long, default_value_t = cartopolis_atlas::ocean_depth::DEFAULT_ZOOM)]
        zoom: u32,
    },
    /// Rebuild a landmark from measurements and photographs
    /// (`landmark`, `docs/notes/landmark-kit.md`).
    Landmark {
        #[command(subcommand)]
        step: LandmarkStep,
    },
}

#[derive(Subcommand)]
enum LandmarkStep {
    /// Gather a building's outline, laser heights, aerial photograph and
    /// nearby photographs, and a first kit, into a folder.
    Gather {
        /// The BAG identificatie.
        pand: String,
        /// Words a photograph's title must hold one of (`Martinitoren`).
        #[arg(long = "name", required = true)]
        names: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Build a kit into model.glb, placement.json and score.md.
    Build {
        kit: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Cut through the laser's surface (and the kit's, given one): heights
    /// along a line in metres, its steps and levels, and a drawing to scale.
    Section {
        /// A kit file, or a BAG identificatie (in the frame `gather` uses).
        target: String,
        /// The line's compass heading in the local frame (0 along +y).
        #[arg(long, default_value_t = 0.0)]
        heading: f64,
        /// A local point the line passes through, `x,y`. Default the origin.
        #[arg(long)]
        through: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// The aerial photograph in the local frame at a stated scale, with a
    /// metre grid, the outline, the laser's contours and the kit's plan.
    Aerial {
        /// A kit file, or a BAG identificatie.
        target: String,
        #[arg(long, default_value_t = 5.0)]
        cm_per_px: f64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Solve where a photograph was taken from, from four or more points
    /// whose pixel and local position you know; writes pose.json and pose.md.
    PhotoPose {
        kit: PathBuf,
        #[arg(long)]
        photo: PathBuf,
        /// `u,v=x,y,z`: a pixel and the local point it shows (z above the
        /// ground). Repeat for every point; four at least.
        #[arg(long = "point", required = true)]
        points: Vec<String>,
        /// Where the photograph says it was taken, `lat,lng` (its Commons
        /// coordinate): a start for the search, not a constraint.
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// A magnified crop of a photograph round a pixel, with a pixel grid, to
    /// place a pose's points to the pixel; writes zoom.png.
    PhotoZoom {
        #[arg(long)]
        photo: PathBuf,
        /// The pixel to centre on, `u,v`.
        #[arg(long)]
        at: String,
        /// Source pixels either side.
        #[arg(long, default_value_t = 40)]
        half: u32,
        /// How many times larger.
        #[arg(long, default_value_t = 8)]
        times: u32,
        #[arg(long)]
        out: PathBuf,
    },
    /// Measure on a wall in a solved photograph: each pixel's distance along
    /// the wall and height, and the distance between consecutive ones.
    PhotoMeasure {
        #[arg(long)]
        pose: PathBuf,
        /// The wall: the vertical plane through two local points, `x1,y1:x2,y2`.
        #[arg(long)]
        wall: String,
        /// `u,v`; repeat.
        #[arg(long = "px", required = true)]
        pixels: Vec<String>,
    },
    /// A wall straightened out of a solved photograph: an elevation at a
    /// stated scale, with a metre grid; writes elevation.png and elevation.md.
    PhotoRectify {
        #[arg(long)]
        pose: PathBuf,
        #[arg(long)]
        photo: PathBuf,
        #[arg(long)]
        wall: String,
        #[arg(long, default_value_t = 0.0)]
        bottom: f64,
        #[arg(long)]
        top: f64,
        #[arg(long, default_value_t = 2.0)]
        cm_per_px: f64,
        #[arg(long)]
        out: PathBuf,
    },
    /// The kit drawn over a solved photograph, its visible edges in red, and
    /// how many of the photograph's edges it lands on; writes overlay.png.
    PhotoOverlay {
        kit: PathBuf,
        #[arg(long)]
        pose: PathBuf,
        #[arg(long)]
        photo: PathBuf,
        #[arg(long)]
        out: PathBuf,
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
    config: &cartopolis_atlas::config::AtlasConfig,
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
    .with_data_dir(&config.data_dir)
    .with_upstream(config.upstream_concurrency)
    .with_fresh(fresh);
    cartopolis_atlas::pipeline::http::sweep_source_cache(&config.data_dir);
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
    if let Some(Command::Landmark { step }) = &cli.command {
        let said = match step {
            LandmarkStep::Gather { pand, names, out } => {
                cartopolis_atlas::landmark::gather::gather(pand, names, out)
            }
            LandmarkStep::Build { kit, out } => cartopolis_atlas::landmark::build(kit, out),
            LandmarkStep::Section {
                target,
                heading,
                through,
                out,
            } => cartopolis_atlas::landmark::measure::section_cmd(
                target,
                through.as_deref(),
                *heading,
                out,
            ),
            LandmarkStep::Aerial {
                target,
                cm_per_px,
                out,
            } => cartopolis_atlas::landmark::aerial::aerial_cmd(target, *cm_per_px, out),
            LandmarkStep::PhotoPose {
                kit,
                photo,
                points,
                from,
                out,
            } => match from
                .as_deref()
                .map(cartopolis_atlas::landmark::measure::parse_xy)
                .transpose()
            {
                Ok(from) => cartopolis_atlas::landmark::photo::pose_cmd(
                    kit,
                    photo,
                    points,
                    from.map(|p| (p[0], p[1])),
                    out,
                ),
                Err(e) => Err(format!("--from wants lat,lng: {e}")),
            },
            LandmarkStep::PhotoZoom {
                photo,
                at,
                half,
                times,
                out,
            } => cartopolis_atlas::landmark::photo::zoom_cmd(photo, at, *half, *times, out),
            LandmarkStep::PhotoMeasure { pose, wall, pixels } => {
                cartopolis_atlas::landmark::photo::measure_cmd(pose, wall, pixels)
            }
            LandmarkStep::PhotoRectify {
                pose,
                photo,
                wall,
                bottom,
                top,
                cm_per_px,
                out,
            } => cartopolis_atlas::landmark::photo::rectify_cmd(
                pose, photo, wall, *bottom, *top, *cm_per_px, out,
            ),
            LandmarkStep::PhotoOverlay {
                kit,
                pose,
                photo,
                out,
            } => cartopolis_atlas::landmark::photo::overlay_cmd(kit, pose, photo, out),
        };
        println!("{}", said.map_err(anyhow::Error::msg)?);
        return Ok(());
    }
    if let Some(Command::OceanDepth { out, zoom }) = &cli.command {
        eprintln!(
            "{}",
            cartopolis_atlas::ocean_depth::write(*zoom, out).map_err(anyhow::Error::msg)?
        );
        return Ok(());
    }
    if let Some(Command::Areas { set, from, out }) = &cli.command {
        for (name, n, bytes) in cartopolis_atlas::official_areas::write(set, from.as_deref(), out)?
        {
            eprintln!("{name}: {n} areas, {bytes} bytes");
        }
        return Ok(());
    }
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
        return run_datasets(out, area, name, bbox, datasets, &config, fresh);
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
