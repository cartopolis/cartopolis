//! `carto-pipeline` — the map preprocessing that should happen once, on a server.
//!
//! The client currently derives the whole ground from raw sources every time
//! somebody moves: it fetches the vector tile, cuts the landuse fills against each
//! other, builds the road ribbons, cuts the surveyed pavements out from under them,
//! fetches a 12 MB height survey per map anchor, bakes it into a field, and bends
//! every sheet onto it. Measured on one capture over Groningen, the ground
//! tessellation alone was **45.9 seconds** of worker time against 4.5 for the
//! height field — 58 % of all the map work the client did.
//!
//! All of it is the same answer for every device and every session, and none of it
//! depends on who is looking.
//!
//! # Why the pipeline is Rust, in this repository
//!
//! Not for speed. The manifest has the argument in full; the short form is that
//! the consumers of every one of these formats are already here, so a pipeline
//! written here calls the client's own code — `cartopolis_geo` — rather than a
//! second implementation of it in another language. A ground surface built by this
//! binary and a ground surface built by the client are built by the *same
//! function*. They cannot drift, and there is no format to keep in step by hand.
//!
//! That property is worth most for the thing this crate was started for. Where the
//! ground is, and how a road has flattened it, is a calculation rather than a shape
//! of data: if the server and the app work it out separately they disagree by
//! metres over a canal bank, and every sheet drawn on that ground tears through
//! every other one.
//!
//! # What cartopy does instead
//!
//! Everything it is actually for: showing an operator which cities are built and
//! how stale, starting a run, streaming its log, and recording coverage. It runs
//! this binary as a command and reads [`Progress`] off its standard output, one
//! JSON object per line.
//!
//! `docs/guides/pipeline.md` is how to run it.

mod ground;
mod terrain;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;

use cartopolis_core::world::{format_osm_tile_url, TileKey};

/// One line of machine-readable progress, for cartopy's console.
///
/// A line per unit of work rather than a percentage: a console wants to show what
/// is being built and what it produced, and a stage that can only report a
/// fraction cannot say which cell went wrong.
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Progress {
    /// What the run is about to do, before it starts doing it. An operator
    /// watching a console needs the denominator.
    Start {
        stage: &'static str,
        cells: usize,
        jobs: usize,
    },
    /// One map cell built and written.
    Cell {
        z: u32,
        x: u32,
        y: u32,
        /// Ground meshes produced, by surface group.
        groups: Vec<(String, usize)>,
        triangles: usize,
        bytes: usize,
        /// Fraction of the cell its own surfaces cover. A run whose cities are
        /// whole sits a little under 1.0; a dip means a block of ground nothing
        /// drew, which is otherwise only ever found by pointing a camera at it.
        covered: f32,
        /// `(latitude, longitude)` of the deepest point of each tunnel in the cell.
        /// A count says a cell has one somewhere in 1.5 km of ground; this says
        /// where to stand.
        tunnels: Vec<(f64, f64)>,
        millis: u128,
    },
    /// A cell already built by this format version, left alone. `--force` rebuilds
    /// it.
    Skipped { z: u32, x: u32, y: u32 },
    /// A cell that could not be built. **Not fatal**: a city is thousands of cells
    /// and one bad tile must not cost the run, so this is reported and the pass
    /// continues. The exit status still reflects that something failed.
    Failed {
        z: u32,
        x: u32,
        y: u32,
        error: String,
    },
    /// One map cell's ground heights, built and written.
    Terrain {
        z: u32,
        x: u32,
        y: u32,
        grid: u32,
        /// Metres between the cell's lowest and highest measured ground.
        relief_m: f32,
        /// Samples inside a road corridor, margin included.
        road_samples: usize,
        /// Samples the flattening actually moved. Reported beside `road_samples`
        /// because "there were roads" and "the grading did something" are
        /// different claims, and a stage that silently did nothing looks exactly
        /// like a stage with nothing to do.
        flattened: usize,
        /// Samples the survey has no reading for — over water it has none at all,
        /// so a canal city is expected to carry a few per cent.
        no_data: usize,
        bytes: usize,
        millis: u128,
    },
    /// The stage finished.
    Done {
        stage: &'static str,
        cells: usize,
        failed: usize,
        millis: u128,
    },
}

impl Progress {
    /// Write one line and flush it, so a console tailing the pipe sees a cell land
    /// when it lands rather than when the operating system's buffer happens to
    /// fill.
    pub fn emit(&self) {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        if let Ok(line) = serde_json::to_string(self) {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}

/// Where a stage gets its vector tiles: a URL template, or a directory of files
/// recorded earlier.
///
/// The directory form is not a convenience. It is what lets a run be reproduced
/// exactly — same bytes in, same bytes out — and what lets the stage be exercised
/// against the recorded fixtures with no server at all.
pub enum Tiles {
    Url {
        template: String,
        agent: ureq::Agent,
    },
    Dir(std::path::PathBuf),
}

impl Tiles {
    pub fn fetch(&self, key: &TileKey) -> Result<Vec<u8>> {
        match self {
            Self::Url { template, agent } => {
                let url = format_osm_tile_url(template, key);
                let resp = agent
                    .get(&url)
                    .call()
                    .with_context(|| format!("fetching {url}"))?;
                let mut body = Vec::new();
                std::io::copy(&mut resp.into_reader(), &mut body)
                    .with_context(|| format!("reading {url}"))?;
                Ok(body)
            }
            Self::Dir(root) => {
                let path = root.join(format!("{}/{}/{}.mvt", key.z, key.x, key.y));
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))
            }
        }
    }
}

#[derive(Parser)]
#[command(
    name = "carto-pipeline",
    about = "Build the map's derived data once, so no client has to.",
    long_about = "Prepares map data on a server that every client would otherwise \
                  derive for itself. Writes one JSON object per line to standard \
                  output for an operator console to read; logs go to standard error."
)]
struct Cli {
    #[command(subcommand)]
    stage: Stage,
}

#[derive(Subcommand)]
enum Stage {
    /// Build the ground surfaces: the landuse fills, the road ribbons and their
    /// kerbs, cut into a partition so no two of them cover the same ground.
    ///
    /// This is the stage that carries the most client work, and the cut is why it
    /// is worth carrying: two sheets over one piece of ground can only be held
    /// apart by ordering them, and ordering stops working the moment the ground
    /// has a gradient (`docs/notes/the-ground-stack.md`).
    Ground(GroundArgs),

    /// Build the ground heights: the national height survey, resampled onto each
    /// cell and flattened along its roads.
    ///
    /// The flattening is the point. Every surface the map draws is bent onto this
    /// ground against its own triangles, so where the ground curves between one
    /// surface's corners and another's they disagree about where it is — by up to
    /// 2.8 m on a handset, against the ten millimetres the depth order has to work
    /// in. Removing the curvature under a road removes the disagreement, including
    /// for the pairs no cut can reach.
    Terrain(TerrainArgs),
}

#[derive(clap::Args)]
struct TerrainArgs {
    /// The area to build, as `west,south,east,north` in degrees.
    #[arg(long, value_name = "W,S,E,N", value_parser = parse_bbox)]
    bbox: [f64; 4],

    /// Which zoom's cells to build. Must match the `ground` stage's, or the
    /// heights and the surfaces describe different pieces of ground.
    #[arg(long, default_value_t = 14)]
    zoom: u32,

    /// Samples per side of each written cell. The default is ~2.9 m ground at
    /// zoom 14, a little finer than the client's own field.
    #[arg(long, default_value_t = terrain::DEFAULT_GRID)]
    grid: u32,

    /// Vector tile source, for the road shapes the flattening follows.
    #[arg(
        long,
        value_name = "TEMPLATE",
        conflicts_with = "tile_dir",
        required_unless_present = "tile_dir"
    )]
    tile_url: Option<String>,

    /// Vector tiles already on disk, as `{z}/{x}/{y}.mvt` under this directory.
    #[arg(long, value_name = "DIR")]
    tile_dir: Option<std::path::PathBuf>,

    /// Where the prepared heights go: `{z}/{x}/{y}.ctrn` under here.
    #[arg(long, value_name = "DIR")]
    out: std::path::PathBuf,

    /// How many cells to build at once.
    ///
    /// **Lower than the ground stage's default on purpose**: every cell is a
    /// request to a shared public service, and being a good guest on it is worth
    /// more than finishing a city a few minutes sooner.
    #[arg(long, default_value_t = 3)]
    jobs: usize,

    /// Rebuild cells that are already present and readable.
    #[arg(long)]
    force: bool,
}

#[derive(clap::Args)]
struct GroundArgs {
    /// The area to build, as `west,south,east,north` in degrees.
    #[arg(long, value_name = "W,S,E,N", value_parser = parse_bbox)]
    bbox: [f64; 4],

    /// Which zoom's cells to build. 14 is what the client streams its ground from.
    #[arg(long, default_value_t = 14)]
    zoom: u32,

    /// Vector tile source, `{z}/{x}/{y}` template. Exclusive with `--tile-dir`.
    #[arg(
        long,
        value_name = "TEMPLATE",
        conflicts_with = "tile_dir",
        required_unless_present = "tile_dir"
    )]
    tile_url: Option<String>,

    /// Vector tiles already on disk, as `{z}/{x}/{y}.mvt` under this directory.
    #[arg(long, value_name = "DIR")]
    tile_dir: Option<std::path::PathBuf>,

    /// Where the prepared cells go: `{z}/{x}/{y}.cgnd` under here.
    #[arg(long, value_name = "DIR")]
    out: std::path::PathBuf,

    /// How many cells to build at once. Defaults to the machine's parallelism.
    ///
    /// Each job holds one tile and one cell's meshes, so memory scales with this;
    /// a dense cell's paving is measured in megabytes.
    #[arg(long)]
    jobs: Option<usize>,

    /// Rebuild cells that are already present and readable.
    ///
    /// Without it a run resumes: a cell whose file this build can read is left
    /// alone, so an interrupted city costs only what it had not reached.
    #[arg(long)]
    force: bool,
}

/// `west,south,east,north`, degrees.
fn parse_bbox(s: &str) -> Result<[f64; 4], String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 4 {
        return Err(format!("{s:?} is not west,south,east,north"));
    }
    let mut out = [0.0; 4];
    for (slot, text) in out.iter_mut().zip(parts) {
        *slot = text
            .trim()
            .parse()
            .map_err(|_| format!("{text:?} is not a number"))?;
    }
    let [west, south, east, north] = out;
    if !(-180.0..=180.0).contains(&west) || !(-180.0..=180.0).contains(&east) {
        return Err("longitude is outside -180..180".into());
    }
    // The client's own tile maths is Web Mercator, which stops here.
    if !(-85.05..=85.05).contains(&south) || !(-85.05..=85.05).contains(&north) {
        return Err("latitude is outside Web Mercator's -85.05..85.05".into());
    }
    if west > east || south > north {
        return Err("the box is inside out: expected west,south,east,north".into());
    }
    Ok(out)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        // The console reads standard output and nothing else, so logs go to
        // standard error or they corrupt the event stream.
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.stage {
        Stage::Ground(args) => run_ground(&args),
        Stage::Terrain(args) => run_terrain(&args),
    }
}

/// Where a stage gets its vector tiles, from the flags both stages share.
fn tiles_from(tile_url: Option<&String>, tile_dir: Option<&std::path::PathBuf>) -> Result<Tiles> {
    match (tile_url, tile_dir) {
        (Some(template), _) => Ok(Tiles::Url {
            template: template.clone(),
            agent: ureq::AgentBuilder::new()
                // A stage is a batch job and a hung socket would stall a whole city
                // behind one cell.
                .timeout_connect(std::time::Duration::from_secs(10))
                .timeout_read(std::time::Duration::from_secs(60))
                .build(),
        }),
        (None, Some(dir)) => Ok(Tiles::Dir(dir.clone())),
        (None, None) => anyhow::bail!("give either --tile-url or --tile-dir"),
    }
}

fn run_terrain(args: &TerrainArgs) -> Result<()> {
    let cells = ground::cells_in(args.bbox, args.zoom);
    anyhow::ensure!(
        !cells.is_empty(),
        "that box contains no cells at zoom {}",
        args.zoom
    );
    anyhow::ensure!(
        (2..=cartopolis_geo::terrain_file::MAX_GRID).contains(&args.grid),
        "--grid must be between 2 and {}",
        cartopolis_geo::terrain_file::MAX_GRID
    );
    let tiles = tiles_from(args.tile_url.as_ref(), args.tile_dir.as_ref())?;

    let started = std::time::Instant::now();
    Progress::Start {
        stage: "terrain",
        cells: cells.len(),
        jobs: args.jobs,
    }
    .emit();

    let (done, failed) = terrain::run(&cells, &tiles, &args.out, args.grid, args.jobs, args.force)?;

    Progress::Done {
        stage: "terrain",
        cells: done,
        failed,
        millis: started.elapsed().as_millis(),
    }
    .emit();
    anyhow::ensure!(failed == 0, "{failed} of {} cells failed", cells.len());
    Ok(())
}

fn run_ground(args: &GroundArgs) -> Result<()> {
    let cells = ground::cells_in(args.bbox, args.zoom);
    anyhow::ensure!(
        !cells.is_empty(),
        "that box contains no cells at zoom {}",
        args.zoom
    );

    let jobs = args.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get)
    });

    let tiles = tiles_from(args.tile_url.as_ref(), args.tile_dir.as_ref())?;

    let started = std::time::Instant::now();
    Progress::Start {
        stage: "ground",
        cells: cells.len(),
        jobs,
    }
    .emit();

    let (done, failed) = ground::run(&cells, &tiles, &args.out, jobs, args.force)?;

    Progress::Done {
        stage: "ground",
        cells: done,
        failed,
        millis: started.elapsed().as_millis(),
    }
    .emit();
    anyhow::ensure!(failed == 0, "{failed} of {} cells failed", cells.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bounding_box_is_checked_rather_than_trusted() {
        assert!(parse_bbox("6.5,53.2,6.6,53.3").is_ok());
        assert!(parse_bbox("6.5,53.2,6.6").is_err(), "three numbers");
        assert!(parse_bbox("6.6,53.2,6.5,53.3").is_err(), "east of west");
        assert!(parse_bbox("6.5,53.3,6.6,53.2").is_err(), "north of south");
        assert!(parse_bbox("6.5,-89.0,6.6,53.3").is_err(), "past Mercator");
        assert!(parse_bbox("x,53.2,6.6,53.3").is_err(), "not a number");
    }

    /// A box strictly inside one tile is still one cell of work, not zero — an
    /// empty answer would report a successful run that built nothing.
    #[test]
    fn a_box_inside_a_single_tile_is_one_cell() {
        let cells = ground::cells_in([6.5449, 53.2104, 6.5451, 53.2106], 14);
        assert_eq!(cells.len(), 1);
        assert_eq!((cells[0].z, cells[0].x, cells[0].y), (14, 8489, 5321));
    }

    /// Tile width in metres is a function of latitude, so a tall box must not use
    /// one number for all of it: at 53° a degree of longitude is 0.6 of what it is
    /// at the equator, and a wrong `tile_m` scales every surface in the cell.
    #[test]
    fn each_row_takes_its_own_ground_width() {
        let cells = ground::cells_in([6.0, 50.0, 6.2, 53.5], 14);
        let north = cells.first().expect("cells").tile_m;
        let south = cells.last().expect("cells").tile_m;
        assert!(
            south > north * 1.02,
            "a 3.5° tall box should widen southward: {north} m to {south} m"
        );
    }

    /// The output path is the same `{z}/{x}/{y}` shape every other streamed layer
    /// uses, so a static host needs no rule of its own.
    #[test]
    fn a_cell_writes_where_a_client_would_look() {
        let cell = ground::Cell {
            z: 14,
            x: 8489,
            y: 5321,
            tile_m: 1464.8,
        };
        let path = ground::path_for(std::path::Path::new("/srv/ground"), &cell);
        assert_eq!(path, std::path::Path::new("/srv/ground/14/8489/5321.cgnd"));
    }
}
