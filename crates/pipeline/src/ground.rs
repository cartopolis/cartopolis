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

/// Grid the cell's coverage is sampled on. 512 over a ~1.5 km cell is ~2.9 m, fine
/// enough to distinguish a missing block from the sliver a kerb leaves and coarse
/// enough to cost nothing beside the tessellation it follows.
const COVERAGE_SAMPLES: usize = 512;

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
                            covered: report.covered,
                            tunnels: report.tunnels,
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

/// Tile-local metres back to degrees, for a point inside `cell`.
///
/// **A framing coordinate, and the reason it exists is today.** A count of tunnels
/// says a cell has one somewhere in 1.5 km of ground and vouches for nothing you
/// can point a camera at; `docs/shots.toml`'s tunnel section says the same thing
/// about doors, and calls a framing metric the first thing to add. Twenty minutes
/// went on flying around looking for a tunnel that was three cells away.
fn cell_point_to_lat_lng(cell: &Cell, p: [f32; 2]) -> (f64, f64) {
    let n = f64::from(1u32 << cell.z);
    let fx = f64::from(cell.x) + f64::from(p[0]) / f64::from(cell.tile_m);
    let fy = f64::from(cell.y) + f64::from(p[1]) / f64::from(cell.tile_m);
    let lng = fx / n * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * fy / n))
        .sinh()
        .atan()
        .to_degrees();
    (lat, lng)
}

struct Report {
    groups: Vec<(String, usize)>,
    triangles: usize,
    bytes: usize,
    /// What fraction of the cell the surfaces cover. Never quite 1.0 — see
    /// `vector_tiles::surface_coverage` — but a *block* of missing ground shows up
    /// here as a number rather than as something somebody has to notice.
    covered: f32,
    /// Where each tunnel in this cell actually is, so a viewpoint can be aimed at
    /// one instead of hunted for.
    tunnels: Vec<(f64, f64)>,
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
    let GroundBuild {
        meshes,
        tunnels: tunnel_places,
    } = build_meshes(&tile, cell.tile_m)?;

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
        covered: vector_tiles::surface_coverage(&meshes, cell.tile_m, COVERAGE_SAMPLES),
        tunnels: tunnel_places
            .iter()
            .map(|p| cell_point_to_lat_lng(cell, *p))
            .collect(),
    })
}

/// The ground, through the client's own tessellation — the same functions
/// `systems::map_geometry` calls, in the same order and for the same reasons.
/// A cell's ground, and where its tunnels are.
///
/// The two travel together because only the pass that *builds* the tunnels knows
/// where they ended up, and a caller that wants to photograph one needs the
/// second half as much as the first.
pub struct GroundBuild {
    pub meshes: Vec<cartopolis_geo::vector_tiles::SurfaceMesh>,
    /// Tile-local metres, at the deepest point of each bore.
    pub tunnels: Vec<[f32; 2]>,
}

pub fn build_meshes(tile: &TileData, tile_m: f32) -> Result<GroundBuild> {
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

    // **The roads that are not on the road sheet, because they run under it.** A
    // tunnel is a deck, two walls and a roof at its own depth, so it is built apart
    // from the coplanar ground stack rather than given a rung in it.
    //
    // The openings are not optional and not cosmetic: an approach ramp that is
    // still roofed by the ground sheet is a tunnel nothing can drive into, so the
    // ground has to be cut away over every open stretch. `build_tunnels` hands them
    // back precisely because only the caller can do that.
    let tunnels = cartopolis_geo::tunnels::build_tunnels(tile, tile_m, None);
    // Length along the centre line, per tunnel. **A count is not enough**: 41
    // tunnels that are each one segment long is a builder that ran and produced
    // nothing usable, and it looks identical to 41 working tunnels in any
    // summary that only counts them.
    for t in &tunnels.tunnels {
        let len: f32 = t
            .centre
            .windows(2)
            .map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt())
            .sum();
        tracing::debug!(
            vertices = t.centre.len(),
            length_m = len,
            half_w = t.half_w,
            "tunnel"
        );
    }
    let tunnel_places: Vec<[f32; 2]> = tunnels
        .tunnels
        .iter()
        .filter_map(|t| {
            // The deepest point of the centre line, which is the middle of the bore
            // rather than either ramp — the place worth photographing.
            let (i, _) = t
                .road_y
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.total_cmp(b.1))?;
            t.centre.get(i).copied()
        })
        .collect();
    if !tunnels.openings.is_empty() {
        cartopolis_geo::tunnels::cut_ground(&mut meshes, &tunnels.openings);
    }
    for m in &tunnels.meshes {
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in &m.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        tracing::debug!(
            group = ?m.group,
            verts = m.positions.len(),
            tris = m.indices.len() / 3,
            extent_x_m = hi[0] - lo[0],
            extent_y_m = hi[1] - lo[1],
            extent_z_m = hi[2] - lo[2],
            "tunnel mesh"
        );
    }
    meshes.extend(tunnels.meshes);
    Ok(GroundBuild {
        meshes,
        tunnels: tunnel_places,
    })
}
