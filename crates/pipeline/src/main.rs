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

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;

use cartopolis_geo::vector_tiles::{self, TileData};

/// One line of machine-readable progress, for cartopy's console.
///
/// A line per unit of work rather than a percentage: a console wants to show what
/// is being built and what it produced, and a stage that can only report a
/// fraction cannot say which cell went wrong.
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Progress {
    /// One map cell finished.
    Cell {
        z: u32,
        x: u32,
        y: u32,
        /// Ground meshes produced, by surface group.
        groups: Vec<(String, usize)>,
        /// Triangles in all of them.
        triangles: usize,
        millis: u128,
    },
    /// A cell that could not be built. **Not fatal**: a city is thousands of cells
    /// and one bad tile must not cost the run, so this is reported and the pass
    /// continues. The exit status still reflects that something failed.
    Failed {
        z: u32,
        x: u32,
        y: u32,
        error: String,
    },
    /// The stage finished.
    Done {
        cells: usize,
        failed: usize,
        millis: u128,
    },
}

impl Progress {
    /// Write one line and flush it, so a console tailing the pipe sees a cell land
    /// when it lands rather than when the operating system's buffer happens to
    /// fill.
    fn emit(&self) {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        if let Ok(line) = serde_json::to_string(self) {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}

#[derive(Parser)]
#[command(
    name = "carto-pipeline",
    about = "Build the map's derived data once, so no client has to."
)]
struct Cli {
    #[command(subcommand)]
    stage: Stage,
}

#[derive(Subcommand)]
enum Stage {
    /// Build the ground surfaces for one or more map cells: the landuse fills, the
    /// road ribbons and the surveyed pavements, cut into a partition so no two of
    /// them cover the same ground.
    ///
    /// This is the stage that carries the most client work, and the cut is why it
    /// is worth carrying: two sheets over one piece of ground can only be held
    /// apart by ordering them, and ordering stops working the moment the ground
    /// has a gradient (`docs/notes/the-ground-stack.md`).
    Ground(GroundArgs),
}

#[derive(clap::Args)]
struct GroundArgs {
    /// A vector tile on disk, for one cell. Repeatable.
    ///
    /// The network form — a tile URL template and a bounding box — is the next
    /// thing this grows; a local file is what makes the stage testable against the
    /// recorded fixtures without a server.
    #[arg(long, value_name = "PATH", required = true)]
    tile: Vec<std::path::PathBuf>,

    /// The cell the tile is, as `z/x/y`. Repeatable, and positionally paired with
    /// `--tile`; a tile carries no coordinate of its own.
    #[arg(long, value_name = "Z/X/Y", required = true)]
    cell: Vec<String>,

    /// Ground metres across the tile. Depends on latitude, so it cannot be derived
    /// from the zoom alone.
    #[arg(long, default_value_t = 1000.0)]
    tile_m: f32,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        // The console reads stdout and nothing else, so logs go to stderr or they
        // corrupt the event stream.
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.stage {
        Stage::Ground(args) => ground(&args),
    }
}

/// `z/x/y` — the only coordinate a tile file does not carry itself.
fn parse_cell(s: &str) -> Result<(u32, u32, u32)> {
    let mut parts = s.split('/');
    let mut next = |what: &str| -> Result<u32> {
        parts
            .next()
            .with_context(|| format!("cell {s:?} has no {what}"))?
            .parse()
            .with_context(|| format!("cell {s:?}: {what} is not a number"))
    };
    let (z, x, y) = (next("zoom")?, next("x")?, next("y")?);
    anyhow::ensure!(parts.next().is_none(), "cell {s:?} has more than z/x/y");
    Ok((z, x, y))
}

fn ground(args: &GroundArgs) -> Result<()> {
    anyhow::ensure!(
        args.tile.len() == args.cell.len(),
        "{} tiles against {} cells — they pair by position",
        args.tile.len(),
        args.cell.len()
    );
    let started = std::time::Instant::now();
    let mut failed = 0usize;

    for (path, cell) in args.tile.iter().zip(&args.cell) {
        let (z, x, y) = parse_cell(cell)?;
        let at = std::time::Instant::now();
        match build_cell(path, args.tile_m) {
            Ok((groups, triangles)) => Progress::Cell {
                z,
                x,
                y,
                groups,
                triangles,
                millis: at.elapsed().as_millis(),
            }
            .emit(),
            Err(e) => {
                failed += 1;
                Progress::Failed {
                    z,
                    x,
                    y,
                    error: format!("{e:#}"),
                }
                .emit();
            }
        }
    }

    Progress::Done {
        cells: args.tile.len(),
        failed,
        millis: started.elapsed().as_millis(),
    }
    .emit();
    anyhow::ensure!(failed == 0, "{failed} cells failed");
    Ok(())
}

/// One cell's ground, through the client's own tessellation.
///
/// **One parse feeding every consumer**, which is the rule `TileData` exists to
/// enforce: the surfaces, the bridge decks and the roads each used to open their
/// own reader over the same bytes.
fn build_cell(path: &std::path::Path, tile_m: f32) -> Result<(Vec<(String, usize)>, usize)> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let tile = TileData::parse(&bytes).map_err(anyhow::Error::msg)?;

    let mut meshes = vector_tiles::tessellate_surfaces(&tile, tile_m, None)
        .map_err(anyhow::Error::msg)
        .context("ground fills")?;

    // The decks first: a bridge-flagged street rides the deck that carries it
    // rather than the ground plane, and cuts itself out of that deck, so the slabs
    // come back from `tessellate_roads` changed.
    let mut decks = vector_tiles::tessellate_bridges(&tile, tile_m)
        .map_err(anyhow::Error::msg)
        .context("bridge decks")?;

    // `false`: no surveyed paving is being supplied here yet, so the footways and
    // pedestrian streets are still drawn as constant-width ribbons. When the
    // surveyed layer joins this stage that argument becomes what it is in the
    // client — whether this cell has real shapes to draw instead.
    let roads = vector_tiles::tessellate_roads(&tile, tile_m, &mut decks, None, false)
        .map_err(anyhow::Error::msg)
        .context("road ribbons")?;

    // And the fills come out from under the carriageways. On the Groningen fixture
    // that is 67 % of the road sheet's area covered by built land, 7 % by grass and
    // 2 % by woodland — three quarters of every carriageway with a second opaque
    // surface beneath it, which a depth order can only hide while the ground is
    // flat.
    cartopolis_geo::road_stitch::cut_under_roads(&mut meshes, &roads.ground);
    meshes.extend(roads.ground);

    let mut groups: Vec<(String, usize)> = Vec::new();
    let mut triangles = 0usize;
    for m in &meshes {
        triangles += m.indices.len() / 3;
        let name = format!("{:?}", m.group);
        match groups.iter_mut().find(|(g, _)| *g == name) {
            Some((_, n)) => *n += 1,
            None => groups.push((name, 1)),
        }
    }
    groups.sort();
    Ok((groups, triangles))
}
