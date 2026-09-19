//! The `ground` stage: a city's finished surfaces, one file per map cell.
//!
//! This is where the client's map time goes. Measured on one capture over
//! Groningen, cutting the landuse fills, the road ribbons and the surveyed
//! pavements into a partition was **45.9 s** of the client's 78 s of map work,
//! and it is the same answer for every device and every session.
//!
//! The cut is also why it is worth carrying rather than merely expensive: two
//! sheets over one piece of ground can only be held apart by *ordering* them, and
//! ordering stops working the moment the ground has a gradient
//! (`docs/notes/the-ground-stack.md`). Doing it once, on a server, means every
//! device gets the same partition rather than each deriving its own.

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicUsize, Ordering};

use cartopolis_core::world::{self, TileKey};
use cartopolis_geo::ground_file;
use cartopolis_geo::vector_tiles::{self, TileData};

use crate::{Progress, Tiles};

/// One cell's worth of work: where to get it and where to put it.
pub struct Cell {
    pub z: u32,
    pub x: u32,
    pub y: u32,
    /// Ground metres across the tile — a function of latitude, so it cannot be
    /// derived from the zoom alone.
    pub tile_m: f32,
}

/// Every cell of `zoom` whose tile overlaps the box, west/south/east/north in
/// degrees.
///
/// Latitude is per *row*: a tile is a constant number of degrees wide and a
/// shrinking number of metres, so one `tile_m` for a whole city would be wrong by
/// the height of the box. Taken at each row's own centre.
#[must_use]
pub fn cells_in(bbox: [f64; 4], zoom: u32) -> Vec<Cell> {
    let [west, south, east, north] = bbox;
    // y runs the other way from latitude, so north is the low row.
    let (x0, y0) = world::lat_lng_to_tile(north, west, zoom);
    let (x1, y1) = world::lat_lng_to_tile(south, east, zoom);
    let mut out = Vec::new();
    for y in y0.min(y1)..=y0.max(y1) {
        let (top, _) = world::tile_to_lat_lng(x0, y, zoom);
        let (bottom, _) = world::tile_to_lat_lng(x0, y + 1, zoom);
        #[allow(clippy::cast_possible_truncation)]
        let tile_m = world::tile_to_meters(zoom, (top + bottom) * 0.5) as f32;
        for x in x0.min(x1)..=x0.max(x1) {
            out.push(Cell {
                z: zoom,
                #[allow(clippy::cast_sign_loss)]
                x: x.max(0) as u32,
                #[allow(clippy::cast_sign_loss)]
                y: y.max(0) as u32,
                tile_m,
            });
        }
    }
    out
}

/// Where a cell's prepared ground is written, under `out`.
///
/// `{z}/{x}/{y}.cgnd`, the same shape every other streamed layer uses, so a static
/// host serves it with no rules of its own.
#[must_use]
pub fn path_for(out: &std::path::Path, cell: &Cell) -> std::path::PathBuf {
    out.join(cell.z.to_string())
        .join(cell.x.to_string())
        .join(format!("{}.cgnd", cell.y))
}

/// Run the stage over `cells`, `jobs` at a time.
///
/// **A failed cell is reported and the pass continues.** A city is thousands of
/// them and one unreachable tile must not cost the run; the exit status still says
/// something failed, so a scheduled build cannot go green on a half-built city.
pub fn run(
    cells: &[Cell],
    tiles: &Tiles,
    out: &std::path::Path,
    jobs: usize,
    force: bool,
) -> Result<(usize, usize)> {
    let next = AtomicUsize::new(0);
    let built = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(cell) = cells.get(i) else { return };
                let at = std::time::Instant::now();
                let dest = path_for(out, cell);

                if !force && is_current(&dest) {
                    skipped.fetch_add(1, Ordering::Relaxed);
                    Progress::Skipped {
                        z: cell.z,
                        x: cell.x,
                        y: cell.y,
                    }
                    .emit();
                    continue;
                }

                match build_cell(cell, tiles, &dest) {
                    Ok(report) => {
                        built.fetch_add(1, Ordering::Relaxed);
                        Progress::Cell {
                            z: cell.z,
                            x: cell.x,
                            y: cell.y,
                            groups: report.groups,
                            triangles: report.triangles,
                            bytes: report.bytes,
                            millis: at.elapsed().as_millis(),
                        }
                        .emit();
                    }
                    Err(e) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        Progress::Failed {
                            z: cell.z,
                            x: cell.x,
                            y: cell.y,
                            error: format!("{e:#}"),
                        }
                        .emit();
                    }
                }
            });
        }
    });

    Ok((
        built.load(Ordering::Relaxed) + skipped.load(Ordering::Relaxed),
        failed.load(Ordering::Relaxed),
    ))
}

/// Whether an existing file was written by *this* reader's format.
///
/// Reading the header rather than trusting the name: a half-written file from an
/// interrupted run and a file from an older format both look identical on disk,
/// and both would be served to a client that then falls back to building the
/// ground itself — silently undoing the entire point of the run.
fn is_current(path: &std::path::Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    ground_file::decode(&bytes).is_ok()
}

struct Report {
    groups: Vec<(String, usize)>,
    triangles: usize,
    bytes: usize,
}

/// One cell, from tile bytes to a file on disk.
///
/// **One parse feeding every consumer**, which is the rule `TileData` exists to
/// enforce: the surfaces, the bridge decks and the roads each used to open their
/// own reader over the same bytes.
fn build_cell(cell: &Cell, tiles: &Tiles, dest: &std::path::Path) -> Result<Report> {
    let key = TileKey::new(i64::from(cell.x), i64::from(cell.y), cell.z);
    let bytes = tiles.fetch(&key)?;
    let tile = TileData::parse(&bytes).map_err(anyhow::Error::msg)?;
    let meshes = build_meshes(&tile, cell.tile_m)?;

    let encoded = ground_file::encode(cell.z, cell.x, cell.y, &meshes);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    // Write beside it and rename, so an interrupted run leaves either the old file
    // or the new one and never half of one. A reader has no way to tell a truncated
    // file from a cell that genuinely has less in it.
    let tmp = dest.with_extension("cgnd.part");
    std::fs::write(&tmp, &encoded).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, dest).with_context(|| format!("renaming into {}", dest.display()))?;

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
    Ok(Report {
        groups,
        triangles,
        bytes: encoded.len(),
    })
}

/// The ground, through the client's own tessellation — the same functions
/// `systems::map_geometry` calls, in the same order and for the same reasons.
pub fn build_meshes(
    tile: &TileData,
    tile_m: f32,
) -> Result<Vec<cartopolis_geo::vector_tiles::SurfaceMesh>> {
    let mut meshes = vector_tiles::tessellate_surfaces(tile, tile_m, None)
        .map_err(anyhow::Error::msg)
        .context("ground fills")?;

    // The decks first: a bridge-flagged street rides the deck that carries it
    // rather than the ground plane, and cuts itself out of that deck, so the slabs
    // come back from `tessellate_roads` changed.
    let mut decks = vector_tiles::tessellate_bridges(tile, tile_m)
        .map_err(anyhow::Error::msg)
        .context("bridge decks")?;

    // `false`: the surveyed paving is not part of this stage yet, so footways and
    // pedestrian streets are still drawn as constant-width ribbons. When the
    // surveyed layer joins, this argument becomes what it is in the client —
    // whether this cell has real measured shapes to draw instead.
    let roads = vector_tiles::tessellate_roads(tile, tile_m, &mut decks, None, false)
        .map_err(anyhow::Error::msg)
        .context("road ribbons")?;

    // And the fills come out from under the carriageways. On the Groningen fixture
    // that is 67 % of the road sheet's area covered by built land — three quarters
    // of every carriageway with a second opaque surface beneath it, which a depth
    // order can only hide while the ground is flat.
    cartopolis_geo::road_stitch::cut_under_roads(&mut meshes, &roads.ground);
    meshes.extend(roads.ground);
    Ok(meshes)
}
