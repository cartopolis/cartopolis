//! Grade the pre-cut terrain tiles so the client only has to sample them
//! (`docs/notes/level-of-detail-tile-tree.md`, "Atlas grades").
//!
//! The client's terrain bake used to do four things to the heights it fetched:
//! carve the registered water down to its level, fill the holes AHN leaves under
//! buildings, flatten the ground under every road, and ramp the approaches up to
//! every lifted bridge deck. Each device did that for every place it visited, and
//! could not start until every ground cell around the camera had been meshed —
//! the road shapes come out of that meshing — and every deck had been measured
//! against PDOK. This does it once, here, with the same code
//! (`cartopolis_geo::terrain_grade`), and writes the result beside the raw tile.
//!
//! **Heights only.** The roads, decks and water are read to decide where the
//! ground is flattened; nothing about them is served. The client still streams
//! the vector map and keeps every road, bridge and building as its own object.
//!
//! # The window
//!
//! Every step is a neighbourhood operation — the fill reaches 150 m, the ramp up
//! to a deck about as far, the relaxation along a road a few dozen metres — so a
//! tile graded from its own samples alone would put a step at every tile edge a
//! road crosses. Each tile is therefore graded over itself plus [`MARGIN_M`] of its
//! neighbours on every side, on its own sample lattice (the samples of a z14 dense
//! tile continue exactly into its neighbour's), and only its own 513² are kept. Two
//! neighbours grade their shared edge from nearly the same window, so they agree
//! to well inside the format's centimetre.
//!
//! # What is and is not the client's
//!
//! One knowing difference: the client suppresses footpath ribbons in cells where
//! the BGT's surveyed paving is drawn instead (`tessellate_roads`'
//! `surveyed_paving`), and so does not flatten under them. This grades every
//! path. A footpath is at grade anyway; the effect is a slightly smoother verge.

use std::collections::HashMap;

use cartopolis_core::world::{lat_lng_to_tile_frac, tile_frac_to_lat_lng, tile_to_meters};
use cartopolis_geo::terrain_grade::{self, DeckPlate, Lattice};
use cartopolis_geo::terrain_tile::{TerrainTile, DENSE_GRID};
use cartopolis_geo::vector_tiles::{self, BridgeDeck, TileData};

use super::{Ctx, Outcome};
use crate::terrain::{graded_dir, read_stored, store_dir, write_stored, Answer, HeightSource};

/// How far past its own edge a tile is graded. Past the fill's 150 m reach and
/// the approach ramps', with room for the relaxation.
pub const MARGIN_M: f64 = 350.0;

/// The vector tiles the road shapes come from — the same ones the client draws.
pub const DEFAULT_VECTOR_TILES: &str = "https://tiles.cartopolis.org/tiles/osm/{z}/{x}/{y}";

/// Samples per metre a deck's surface is measured at. The client's
/// `DECK_SAMPLE_M` is one metre; a viaduct longer than the raster ceiling is
/// measured more coarsely rather than not at all.
const DECK_SAMPLE_M: f64 = 1.0;
const DECK_RASTER_MAX: usize = 1024;

/// Where a tile's road shapes come from.
pub trait VectorSource {
    /// The z14 MVT body, `None` where the server has no tile.
    fn mvt(&self, x: i64, y: i64) -> Result<Option<Vec<u8>>, String>;
}

pub struct HttpTiles {
    agent: ureq::Agent,
    template: String,
}

impl HttpTiles {
    #[must_use]
    pub fn new(template: &str) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("cartopolis-atlas (+https://cartopolis.org)")
                .build(),
            template: template.to_string(),
        }
    }

    /// `ATLAS_VECTOR_TILES`, or the public tile server.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("ATLAS_VECTOR_TILES").unwrap_or_else(|_| DEFAULT_VECTOR_TILES.into()),
        )
    }
}

impl VectorSource for HttpTiles {
    fn mvt(&self, x: i64, y: i64) -> Result<Option<Vec<u8>>, String> {
        use std::io::Read;
        let url = self
            .template
            .replace("{z}", "14")
            .replace("{x}", &x.to_string())
            .replace("{y}", &y.to_string());
        match self.agent.get(&url).call() {
            Ok(r) => {
                let mut bytes = Vec::new();
                r.into_reader()
                    .take(32 << 20)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                Ok(Some(bytes))
            }
            Err(ureq::Error::Status(404 | 204, _)) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// One lifted deck of a cell, in that cell's tile-local metres.
struct Deck {
    deck: BridgeDeck,
    height: f32,
    approaches: Vec<Vec<[f32; 2]>>,
}

/// What a z14 cell contributes to every window that covers it.
struct Cell {
    /// The metres its tile-local coordinates were built at.
    tile_m: f32,
    roads: Vec<[[f32; 2]; 3]>,
    decks: Vec<Deck>,
}

pub struct Grader<'a> {
    ctx: &'a Ctx,
    vector: &'a dyn VectorSource,
    surface: &'a dyn HeightSource,
    cells: HashMap<(i64, i64), Cell>,
    raw: HashMap<(i64, i64), Option<TerrainTile>>,
    water: HashMap<(i64, i64), Vec<(f32, Vec<(f64, f64)>)>>,
    /// Every deck measured, per cell: its centre in tile fractions and its
    /// clearance (0 for a flat one), for `decks/14/x/y.json`.
    pub deck_heights: HashMap<(i64, i64), Vec<[f32; 3]>>,
    /// Decks measured, and how many were lifted.
    pub decks_measured: usize,
    pub decks_lifted: usize,
}

impl<'a> Grader<'a> {
    pub fn new(ctx: &'a Ctx, vector: &'a dyn VectorSource, surface: &'a dyn HeightSource) -> Self {
        Self {
            ctx,
            vector,
            surface,
            cells: HashMap::new(),
            raw: HashMap::new(),
            water: HashMap::new(),
            deck_heights: HashMap::new(),
            decks_measured: 0,
            decks_lifted: 0,
        }
    }

    /// The raw (unfilled) tile atlas cut, if there is one.
    fn raw(&mut self, x: i64, y: i64) -> Option<&TerrainTile> {
        let dir = self.ctx.data_dir().ok().map(store_dir);
        self.raw
            .entry((x, y))
            .or_insert_with(
                || match dir.and_then(|d| read_stored(&d, 14, x, y, DENSE_GRID)) {
                    Some(Answer::Tile(bytes)) => TerrainTile::decode(&bytes).ok(),
                    _ => None,
                },
            )
            .as_ref()
    }

    /// Bare ground at a point, from the raw tiles — nearest sample.
    fn dtm_at(&mut self, lat: f64, lng: f64) -> Option<f32> {
        let (fx, fy) = lat_lng_to_tile_frac(lat, lng, 14);
        let (tx, ty) = (fx.floor() as i64, fy.floor() as i64);
        let tile = self.raw(tx, ty)?;
        let last = (tile.grid - 1) as f64;
        let col = ((fx - tx as f64) * last).round() as usize;
        let row = ((fy - ty as f64) * last).round() as usize;
        let v = tile.at(col.min(tile.grid - 1), row.min(tile.grid - 1));
        v.is_finite().then_some(v)
    }

    fn water(&mut self, x: i64, y: i64) -> &[(f32, Vec<(f64, f64)>)] {
        let ctx = self.ctx;
        self.water.entry((x, y)).or_insert_with(|| {
            let Some(bytes) = ctx.read_site(&format!("water/14/{x}/{y}.json")) else {
                return Vec::new();
            };
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                return Vec::new();
            };
            v["rings"]
                .as_array()
                .map(|rings| {
                    rings
                        .iter()
                        .filter_map(|r| {
                            let level = r.get(0)?.as_f64()? as f32;
                            let flat: Vec<f64> = r
                                .get(1)?
                                .as_array()?
                                .iter()
                                .filter_map(|v| v.as_f64())
                                .collect();
                            (flat.len() >= 6 && flat.len() % 2 == 0).then(|| {
                                (level, flat.chunks_exact(2).map(|p| (p[0], p[1])).collect())
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// Tessellate a cell's roads and bridges and measure its decks, once.
    fn cell(&mut self, x: i64, y: i64) -> Result<(), String> {
        if self.cells.contains_key(&(x, y)) {
            return Ok(());
        }
        let (lat, _) = tile_frac_to_lat_lng(x as f64 + 0.5, y as f64 + 0.5, 14);
        let tile_m = tile_to_meters(14, lat) as f32;
        let mut cell = Cell {
            tile_m,
            roads: Vec::new(),
            decks: Vec::new(),
        };
        if let Some(bytes) = self.vector.mvt(x, y)? {
            let tile = TileData::parse(&bytes)?;
            let mut decks = vector_tiles::tessellate_bridges(&tile, tile_m)?;
            let roads = vector_tiles::tessellate_roads(&tile, tile_m, &mut decks, None, false)?;
            cell.roads = terrain_grade::road_triangles(&roads.ground);
            self.deck_heights.entry((x, y)).or_default();
            for (i, deck) in decks.into_iter().enumerate() {
                let height = self.measure_deck(x, y, tile_m, &deck)?;
                self.decks_measured += 1;
                // Recorded whether lifted or not: a flat deck is an answer too, and
                // the client asks PDOK for any deck it cannot find here.
                self.deck_heights.entry((x, y)).or_default().push([
                    (deck.min[0] + deck.max[0]) * 0.5 / tile_m,
                    (deck.min[1] + deck.max[1]) * 0.5 / tile_m,
                    height,
                ]);
                if height <= 0.0 {
                    continue;
                }
                self.decks_lifted += 1;
                let approaches = roads
                    .approaches
                    .iter()
                    .filter(|a| a.deck as usize == i)
                    .map(|a| a.points.clone())
                    .collect();
                cell.decks.push(Deck {
                    deck,
                    height,
                    approaches,
                });
            }
        }
        self.cells.insert((x, y), cell);
        Ok(())
    }

    /// How high a deck stands above the ground beside it: the median surface over
    /// its outline less the median bare ground in a box [`GROUND_RING_M`] wider —
    /// the client's `deck_height`, with the ground read from the raw tiles rather
    /// than a second request.
    ///
    /// [`GROUND_RING_M`]: terrain_grade::GROUND_RING_M
    fn measure_deck(
        &mut self,
        cx: i64,
        cy: i64,
        tile_m: f32,
        deck: &BridgeDeck,
    ) -> Result<f32, String> {
        let to_ll = |p: [f32; 2]| {
            tile_frac_to_lat_lng(
                cx as f64 + f64::from(p[0] / tile_m),
                cy as f64 + f64::from(p[1] / tile_m),
                14,
            )
        };
        let rings_rd: Vec<Vec<[f64; 2]>> = deck
            .rings
            .iter()
            .map(|r| {
                r.iter()
                    .map(|&p| {
                        let (lat, lng) = to_ll(p);
                        cartopolis_geo::ahn::wgs84_to_rd(lat, lng)
                    })
                    .collect()
            })
            .collect();
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        for p in rings_rd.iter().flatten() {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
        if lo[0] > hi[0] {
            return Ok(0.0);
        }
        let span = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1.0);
        let n = ((span / DECK_SAMPLE_M).ceil() as usize + 1).clamp(2, DECK_RASTER_MAX);
        let max = [lo[0] + span, lo[1] + span];
        let px = span / n as f64;
        let surface = self.surface.raster(lo, max, n)?;
        let mut tops: Vec<f32> = Vec::new();
        for r in 0..n {
            for c in 0..n {
                let v = surface[r * n + c];
                // North row first.
                let p = [
                    lo[0] + (c as f64 + 0.5) * px,
                    max[1] - (r as f64 + 0.5) * px,
                ];
                if v.is_finite() && inside_rings(&rings_rd, p) {
                    tops.push(v);
                }
            }
        }
        // The ground: every bare-ground sample in the padded box. AHN's DTM has the
        // structure cut out of it already, so what is left is the ground beside it.
        let ring = terrain_grade::GROUND_RING_M;
        let (glo, ghi) = ([lo[0] - ring, lo[1] - ring], [hi[0] + ring, hi[1] + ring]);
        let (mut a, mut b) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in deck.rings.iter().flatten() {
            a = [a[0].min(p[0]), a[1].min(p[1])];
            b = [b[0].max(p[0]), b[1].max(p[1])];
        }
        let pad = (ring as f32) * 1.5;
        let mut grounds: Vec<f32> = Vec::new();
        let step = tile_m / (DENSE_GRID - 1) as f32;
        let mut z = a[1] - pad;
        while z <= b[1] + pad {
            let mut x = a[0] - pad;
            while x <= b[0] + pad {
                let (lat, lng) = to_ll([x, z]);
                let rd = cartopolis_geo::ahn::wgs84_to_rd(lat, lng);
                if (glo[0]..=ghi[0]).contains(&rd[0]) && (glo[1]..=ghi[1]).contains(&rd[1]) {
                    if let Some(v) = self.dtm_at(lat, lng) {
                        grounds.push(v);
                    }
                }
                x += step;
            }
            z += step;
        }
        Ok(match (median(&mut tops), median(&mut grounds)) {
            (Some(top), Some(ground)) => terrain_grade::deck_clearance(top, ground),
            _ => 0.0,
        })
    }

    /// Grade one tile. `None` when atlas has no raw tile for it.
    pub fn grade_tile(&mut self, tx: i64, ty: i64) -> Result<Option<TerrainTile>, String> {
        if self.raw(tx, ty).is_none() {
            return Ok(None);
        }
        let (lat, _) = tile_frac_to_lat_lng(tx as f64 + 0.5, ty as f64 + 0.5, 14);
        let tile_m = tile_to_meters(14, lat) as f32;
        let last = (DENSE_GRID - 1) as i64;
        let step = tile_m / last as f32;
        let k = (MARGIN_M / f64::from(step)).ceil() as i64;
        let n = DENSE_GRID + 2 * k as usize;
        let lattice = Lattice {
            n,
            origin: [-(k as f32) * step, -(k as f32) * step],
            step,
        };

        // The raw heights over the window, off each tile's own lattice.
        let mut raw = vec![f32::NAN; n * n];
        for j in 0..n {
            let gj = j as i64 - k;
            let (dy, row) = (gj.div_euclid(last), gj.rem_euclid(last));
            for i in 0..n {
                let gi = i as i64 - k;
                let (dx, col) = (gi.div_euclid(last), gi.rem_euclid(last));
                if let Some(t) = self.raw(tx + dx, ty + dy) {
                    raw[j * n + i] = t.at(col as usize, row as usize);
                }
            }
        }

        // Roads and decks from every cell the window touches, scaled into this
        // tile's metres (a neighbour a row north is a few parts in ten thousand
        // wider).
        let reach = (k + last - 1) / last;
        let mut road = vec![false; n * n];
        let mut scaled: Vec<(BridgeDeck, [f32; 2], f32, Vec<Vec<[f32; 2]>>)> = Vec::new();
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                self.cell(tx + dx, ty + dy)?;
                let cell = &self.cells[&(tx + dx, ty + dy)];
                let s = tile_m / cell.tile_m;
                let off = [dx as f32 * tile_m, dy as f32 * tile_m];
                let tris: Vec<[[f32; 2]; 3]> = cell
                    .roads
                    .iter()
                    .map(|t| t.map(|p| [off[0] + p[0] * s, off[1] + p[1] * s]))
                    .collect();
                cartopolis_geo::road_grade::mark_roads(
                    &mut road,
                    n,
                    (lattice.origin[0], lattice.origin[1]),
                    step,
                    &tris,
                );
                for d in &cell.decks {
                    let sc = |p: [f32; 2]| [p[0] * s, p[1] * s];
                    let deck = BridgeDeck {
                        positions: Vec::new(),
                        indices: Vec::new(),
                        rings: d
                            .deck
                            .rings
                            .iter()
                            .map(|r| r.iter().map(|&p| sc(p)).collect())
                            .collect(),
                        min: sc(d.deck.min),
                        max: sc(d.deck.max),
                    };
                    let approaches = d
                        .approaches
                        .iter()
                        .map(|r| r.iter().map(|&p| sc(p)).collect())
                        .collect();
                    scaled.push((deck, off, d.height, approaches));
                }
            }
        }
        let plates: Vec<DeckPlate<'_>> = scaled
            .iter()
            .map(|(deck, world, height, approaches)| DeckPlate {
                deck,
                world: *world,
                height: *height,
                approaches,
            })
            .collect();
        let (ramp, lift) = terrain_grade::deck_masks(&plates, lattice);

        // The water, from the water dataset's cells under the window.
        let mut rings: Vec<(f32, Vec<[f32; 2]>)> = Vec::new();
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                for (level, pts) in self.water(tx + dx, ty + dy).to_vec() {
                    let ring = pts
                        .iter()
                        .map(|&(la, ln)| {
                            let (fx, fy) = lat_lng_to_tile_frac(la, ln, 14);
                            [
                                ((fx - tx as f64) * f64::from(tile_m)) as f32,
                                ((fy - ty as f64) * f64::from(tile_m)) as f32,
                            ]
                        })
                        .collect();
                    rings.push((level, ring));
                }
            }
        }
        let levels = terrain_grade::water_level_grid(&rings, lattice);
        let level_at = |x: f32, z: f32| {
            let (i, j) = lattice.index_of([x, z])?;
            let v = levels[j * n + i];
            v.is_finite().then_some(v)
        };

        terrain_grade::grade(&mut raw, lattice, &level_at, road, ramp, &lift);

        let k = k as usize;
        let mut heights = Vec::with_capacity(DENSE_GRID * DENSE_GRID);
        for j in 0..DENSE_GRID {
            let row = (j + k) * n + k;
            heights.extend_from_slice(&raw[row..row + DENSE_GRID]);
        }
        let tile = TerrainTile {
            grid: DENSE_GRID,
            heights,
        };
        Ok(tile.has_data().then_some(tile))
    }
}

/// Grade every tile in `tiles` into the graded store. Returns the tiles written.
pub fn grade_area(
    ctx: &Ctx,
    tiles: &[(i64, i64)],
    vector: &dyn VectorSource,
    surface: &dyn HeightSource,
) -> Outcome<(Vec<[i64; 2]>, usize, usize)> {
    let dir = graded_dir(ctx.data_dir()?);
    let mut grader = Grader::new(ctx, vector, surface);
    let mut graded = Vec::new();
    for (i, &(x, y)) in tiles.iter().enumerate() {
        ctx.check()?;
        if i % 10 == 0 {
            ctx.progress(super::Progress {
                phase: Some("egaliseren".into()),
                done: Some(i as u64),
                total: Some(Some(tiles.len() as u64)),
            });
        }
        match grader.grade_tile(x, y) {
            Ok(Some(tile)) => {
                write_stored(&dir, 14, x, y, DENSE_GRID, &Answer::Tile(tile.encode()));
                graded.push([x, y]);
            }
            Ok(None) => {}
            Err(e) => ctx.log(format!("tegel 14/{x}/{y} niet geëgaliseerd: {e}")),
        }
    }
    ctx.log(format!(
        "{} tegels geëgaliseerd; {} bruggen gemeten, {} verhoogd",
        graded.len(),
        grader.decks_measured,
        grader.decks_lifted
    ));
    write_decks(ctx, &grader.deck_heights)?;
    Ok((graded, grader.decks_measured, grader.decks_lifted))
}

/// The deck heights measured while grading, per z14 cell, for the client to
/// place its bridge decks with instead of asking PDOK itself: `{"decks": [[fx, fz,
/// clearance], …]}`, the deck's bounding-box centre in fractions of its tile and
/// its clearance in metres, 0 for a deck laid flat. Every cell the grading
/// tessellated is written, including ones with no decks, so the client can tell
/// "no decks here" from "not measured".
fn write_decks(ctx: &Ctx, decks: &HashMap<(i64, i64), Vec<[f32; 3]>>) -> Outcome<()> {
    let round = |v: f32| (f64::from(v) * 1e5).round() / 1e5;
    let mut cells = Vec::new();
    for (&(x, y), list) in decks {
        let rows: Vec<serde_json::Value> = list
            .iter()
            .map(|d| serde_json::json!([round(d[0]), round(d[1]), round(d[2])]))
            .collect();
        ctx.write_json(
            &format!("decks/14/{x}/{y}.json"),
            &serde_json::json!({ "decks": rows }),
        )?;
        cells.push([x, y]);
    }
    cells.sort_unstable();
    ctx.write_json(
        "decks/coverage.json",
        &serde_json::json!({
            "attribution": "AHN (Rijkswaterstaat / Het Waterschapshuis), CC0",
            "cells": cells,
            "name": ctx.name,
            "zoom": 14,
        }),
    )
}

fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    let mid = v.len() / 2;
    v.select_nth_unstable_by(mid, f32::total_cmp);
    Some(v[mid])
}

/// Even-odd point-in-polygon over RD rings.
fn inside_rings(rings: &[Vec<[f64; 2]>], p: [f64; 2]) -> bool {
    let mut inside = false;
    for ring in rings {
        for k in 0..ring.len() {
            let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
            if (a[1] > p[1]) != (b[1] > p[1]) {
                let t = (p[1] - a[1]) / (b[1] - a[1]);
                if p[0] < a[0] + t * (b[0] - a[0]) {
                    inside = !inside;
                }
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A report on a graded store, printed rather than asserted: how far the
    /// grading moved the ground, and whether neighbours agree along the edge they
    /// share. Point it at a data directory a run wrote:
    ///
    /// `ATLAS_GRADED_CHECK=/path/to/data cargo test -p cartopolis_atlas graded_store_report -- --ignored --nocapture`
    #[test]
    #[ignore = "reads a real run's output"]
    fn graded_store_report() {
        let data = std::path::PathBuf::from(std::env::var("ATLAS_GRADED_CHECK").unwrap());
        let (raw_dir, graded) = (store_dir(&data), graded_dir(&data));
        let load =
            |dir: &std::path::Path, x: i64, y: i64| match read_stored(dir, 14, x, y, DENSE_GRID) {
                Some(Answer::Tile(b)) => TerrainTile::decode(&b).ok(),
                _ => None,
            };
        let mut tiles = Vec::new();
        for entry in std::fs::read_dir(graded.join("14")).unwrap() {
            let x: i64 = entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .parse()
                .unwrap();
            for f in std::fs::read_dir(entry.unwrap().path()).unwrap() {
                let name = f.unwrap().file_name().to_string_lossy().to_string();
                let y: i64 = name.split('.').next().unwrap().parse().unwrap();
                tiles.push((x, y));
            }
        }
        tiles.sort_unstable();
        let g = DENSE_GRID;
        for &(x, y) in &tiles {
            let (Some(t), Some(r)) = (load(&graded, x, y), load(&raw_dir, x, y)) else {
                continue;
            };
            let finite = t.heights.iter().filter(|v| v.is_finite()).count();
            let (mut moved, mut n, mut worst) = (0.0f64, 0usize, 0.0f32);
            for (a, b) in t.heights.iter().zip(&r.heights) {
                if a.is_finite() && b.is_finite() {
                    let d = (a - b).abs();
                    moved += f64::from(d);
                    n += 1;
                    worst = worst.max(d);
                }
            }
            let mut seam = 0.0f32;
            if let Some(east) = load(&graded, x + 1, y) {
                for row in 0..g {
                    let (a, b) = (t.at(g - 1, row), east.at(0, row));
                    if a.is_finite() && b.is_finite() {
                        seam = seam.max((a - b).abs());
                    }
                }
            }
            if let Some(south) = load(&graded, x, y + 1) {
                for col in 0..g {
                    let (a, b) = (t.at(col, g - 1), south.at(col, 0));
                    if a.is_finite() && b.is_finite() {
                        seam = seam.max((a - b).abs());
                    }
                }
            }
            println!(
                "14/{x}/{y}: finite {:.1} %, moved mean {:.2} m worst {worst:.2} m, seam max {seam:.3} m",
                100.0 * finite as f64 / (g * g) as f64,
                moved / n.max(1) as f64,
            );
        }
    }
}
