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
use cartopolis_geo::deck_profile::{DeckMeasure, DeckProfile};
use cartopolis_geo::road_levels::{self, Height, RoadHeights, StreetRun};
use cartopolis_geo::terrain_grade::{self, ConformRoad, DeckPlate, Lattice};
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
    top: Option<f32>,
    profile: Option<cartopolis_geo::deck_profile::DeckProfile>,
    approaches: Vec<Vec<[f32; 2]>>,
}

/// What a z14 cell contributes to every window that covers it.
struct Cell {
    /// The metres its tile-local coordinates were built at.
    tile_m: f32,
    /// Its streets as the client draws them, for the road network the heights
    /// are solved on (`road_levels`).
    runs: Vec<StreetRun>,
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
    /// Every deck measured, per cell, as its published record — see
    /// [`write_decks`].
    pub deck_heights: HashMap<(i64, i64), Vec<serde_json::Value>>,
    /// Every graded tile's own street runs with their solved heights — see
    /// [`write_road_heights`].
    pub road_heights: HashMap<(i64, i64), RoadHeights>,
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
            road_heights: HashMap::new(),
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
            runs: Vec::new(),
            decks: Vec::new(),
        };
        if let Some(bytes) = self.vector.mvt(x, y)? {
            let tile = TileData::parse(&bytes)?;
            let mut decks = vector_tiles::tessellate_bridges(&tile, tile_m)?;
            let roads = vector_tiles::tessellate_roads(&tile, tile_m, &mut decks, None, false)?;
            cell.runs = road_levels::street_runs(&tile, tile_m);
            self.deck_heights.entry((x, y)).or_default();
            for (i, deck) in decks.into_iter().enumerate() {
                let measured = self.measure_deck(x, y, tile_m, &deck)?;
                let height = measured.height;
                self.decks_measured += 1;
                // Recorded whether lifted or not: a flat deck is an answer too, and
                // the client asks PDOK for any deck it cannot find here. The top
                // and the profile trail, so a client from before them still reads
                // the height.
                let round = |v: f64| (v * 1e5).round() / 1e5;
                self.deck_heights
                    .entry((x, y))
                    .or_default()
                    .push(serde_json::json!([
                        round(f64::from((deck.min[0] + deck.max[0]) * 0.5 / tile_m)),
                        round(f64::from((deck.min[1] + deck.max[1]) * 0.5 / tile_m)),
                        round(f64::from(height)),
                        measured.top.map(|t| round(f64::from(t))),
                        measured.profile.as_ref().map(|p| p
                            .to_wire(f64::from(tile_m))
                            .into_iter()
                            .map(round)
                            .collect::<Vec<_>>()),
                    ]));
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
                    top: measured.top,
                    profile: measured.profile,
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
    ) -> Result<DeckMeasure, String> {
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
        let flat = DeckMeasure {
            height: 0.0,
            top: None,
            profile: None,
        };
        if lo[0] > hi[0] {
            return Ok(flat);
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
        let (Some(top), Some(ground)) = (median(&mut tops), median(&mut grounds)) else {
            return Ok(flat);
        };
        let height = terrain_grade::deck_clearance(top, ground);
        if height <= 0.0 {
            return Ok(flat);
        }
        // The profile from the same raster, read at the deck's own tile-local
        // points as the client reads it (`DeckProfile::fit_over`).
        let at = |p: [f64; 2]| -> Option<f32> {
            let c = ((p[0] - lo[0]) / px).floor();
            let r = ((max[1] - p[1]) / px).floor();
            if c < 0.0 || r < 0.0 || c >= n as f64 || r >= n as f64 {
                return None;
            }
            let v = surface[r as usize * n + c as usize];
            v.is_finite().then_some(v)
        };
        let profile = DeckProfile::fit_over(deck, |local| {
            let (lat, lng) = to_ll(local);
            at(cartopolis_geo::ahn::wgs84_to_rd(lat, lng))
        });
        Ok(DeckMeasure {
            height,
            top: Some(top),
            profile,
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
        #[allow(clippy::type_complexity)]
        let mut scaled: Vec<(
            BridgeDeck,
            [f32; 2],
            f32,
            Option<f32>,
            Option<DeckProfile>,
            Vec<Vec<[f32; 2]>>,
        )> = Vec::new();
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                self.cell(tx + dx, ty + dy)?;
                let cell = &self.cells[&(tx + dx, ty + dy)];
                let s = tile_m / cell.tile_m;
                let off = [dx as f32 * tile_m, dy as f32 * tile_m];
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
                    scaled.push((
                        deck,
                        off,
                        d.height,
                        d.top,
                        d.profile.as_ref().map(|p| p.scaled(f64::from(s))),
                        approaches,
                    ));
                }
            }
        }
        let plates: Vec<DeckPlate<'_>> = scaled
            .iter()
            .map(
                |(deck, world, height, top, profile, approaches)| DeckPlate {
                    deck,
                    world: *world,
                    height: *height,
                    top: *top,
                    profile: profile.as_ref(),
                    approaches,
                },
            )
            .collect();

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

        // **The roads' own heights, solved on the network** (`road_levels`) from
        // the raw survey — before anything is filled or carved, so a hole is read
        // as a hole: the road over or under a structure the survey cut out, which
        // the network bridges from both sides.
        let sample = |p: [f32; 2]| -> f32 {
            let fx = (p[0] - lattice.origin[0]) / step;
            let fz = (p[1] - lattice.origin[1]) / step;
            let (x0, z0) = (fx.floor(), fz.floor());
            let (tx_, tz_) = (fx - x0, fz - z0);
            let (mut sum, mut w) = (0.0f32, 0.0f32);
            for (ox, oz, wt) in [
                (0, 0, (1.0 - tx_) * (1.0 - tz_)),
                (1, 0, tx_ * (1.0 - tz_)),
                (0, 1, (1.0 - tx_) * tz_),
                (1, 1, tx_ * tz_),
            ] {
                let (x, z) = (x0 as i64 + ox, z0 as i64 + oz);
                if x < 0 || z < 0 || x >= n as i64 || z >= n as i64 {
                    continue;
                }
                let v = raw[z as usize * n + x as usize];
                if v.is_finite() && wt > 0.0 {
                    sum += v * wt;
                    w += wt;
                }
            }
            if w > 1e-6 {
                sum / w
            } else {
                f32::NAN
            }
        };
        // **Over a tunnel the survey may be stale.** Groningen's ring was flown as
        // an open trench and has a lid on it now, so a street crossing the lid
        // would read the trench floor nine metres down. Where a road runs over a
        // tunnel's corridor its ground is the lid — the land around — as the
        // client's own fill (`tunnels::Lidded`) has always put it back.
        let mut bores: Vec<(Vec<[f32; 2]>, f32)> = Vec::new();
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let Some(cell) = self.cells.get(&(tx + dx, ty + dy)) else {
                    continue;
                };
                let s = tile_m / cell.tile_m;
                let off = [dx as f32 * tile_m, dy as f32 * tile_m];
                for run in cell.runs.iter().filter(|r| r.level < 0) {
                    bores.push((
                        run.points
                            .iter()
                            .map(|p| [off[0] + p[0] * s, off[1] + p[1] * s])
                            .collect(),
                        run.half_w * s,
                    ));
                }
            }
        }
        let over_a_bore = |p: [f32; 2]| {
            bores.iter().any(|(pts, half)| {
                pts.windows(2).any(|w| {
                    let (a, b) = (w[0], w[1]);
                    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
                    let len2 = dx * dx + dz * dz;
                    let t = if len2 > 1e-9 {
                        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    (p[0] - a[0] - dx * t).hypot(p[1] - a[1] - dz * t) <= half + LID_REACH_M
                })
            })
        };
        let mut lines: Vec<road_levels::Line> = Vec::new();
        // (this tile's own run, its key, its half-width, its level, its class)
        // per line.
        let mut owners: Vec<(bool, u64, f32, i8, cartopolis_geo::road_texture::RoadClass)> =
            Vec::new();
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let Some(cell) = self.cells.get(&(tx + dx, ty + dy)) else {
                    continue;
                };
                let s = tile_m / cell.tile_m;
                let off = [dx as f32 * tile_m, dy as f32 * tile_m];
                for run in &cell.runs {
                    let points: Vec<[f32; 2]> = run
                        .points
                        .iter()
                        .map(|p| [off[0] + p[0] * s, off[1] + p[1] * s])
                        .collect();
                    // **A span rides its deck end to end.** The deck a street's
                    // own span was built from ends exactly where the span does,
                    // so its last point sits on the deck's edge and may test
                    // either side of it; asked point by point, the joint lost its
                    // pin. Any point on a lifted deck puts the whole run on it,
                    // at the deck's surveyed top under each point.
                    let carried = (run.level > 0)
                        .then(|| {
                            plates.iter().find(|plate| {
                                plate.top_at([0.0, 0.0]).is_some()
                                    && points.iter().any(|&p| {
                                        plate.deck.contains([
                                            p[0] - plate.world[0],
                                            p[1] - plate.world[1],
                                        ])
                                    })
                            })
                        })
                        .flatten();
                    // **Under a lid the road only has a ceiling**: the land around
                    // less a level's depth, as `tunnels` has always put it — and
                    // where the survey saw down into an open trench, that too,
                    // since then it measured the road.
                    let depth = cartopolis_geo::tunnels::level_depth(run.class);
                    let heights = points
                        .iter()
                        .map(|&p| {
                            if run.level < 0 {
                                let lid = lid_level(&sample, p);
                                let here = sample(p);
                                if !lid.is_finite() {
                                    return Height::Unknown;
                                }
                                return Height::AtMost {
                                    ceiling: lid - depth,
                                    measured: (here.is_finite() && here < lid - 2.0)
                                        .then_some(here),
                                };
                            }
                            if let Some(plate) = carried {
                                let local = [p[0] - plate.world[0], p[1] - plate.world[1]];
                                if let Some(t) = plate.top_at(local) {
                                    return Height::Fixed(t);
                                }
                            }
                            let v = if over_a_bore(p) {
                                lid_level(&sample, p)
                            } else {
                                sample(p)
                            };
                            if v.is_finite() {
                                Height::Measured { y: v, weight: 1.0 }
                            } else {
                                Height::Unknown
                            }
                        })
                        .collect();
                    lines.push(road_levels::Line {
                        points,
                        heights,
                        level: run.level,
                        max_grade: road_levels::max_grade(run.class),
                        cut: run.cut,
                    });
                    owners.push((dx == 0 && dy == 0, run.key, run.half_w, run.level, run.class));
                }
            }
        }
        let solved = road_levels::solve(&lines);

        // **The land each road runs through**, for deciding which stretches are
        // structures below the ground and how high their walls reach: the level
        // of the land around (`lid_level`), the same the lids are measured
        // against. Only below a bridge's level — a deck's structure is its own.
        // A point the survey has nothing around takes its neighbour's along the
        // line, and a tunnel with none at all is a level's depth under its road.
        let land: Vec<Vec<f32>> = lines
            .iter()
            .zip(&owners)
            .zip(&solved.heights)
            .map(|((l, o), ys)| {
                if l.level > 0 {
                    return vec![f32::NAN; l.points.len()];
                }
                let mut g: Vec<f32> = l.points.iter().map(|&p| lid_level(&sample, p)).collect();
                fill_along(&mut g);
                if g.iter().all(|v| !v.is_finite()) && l.level < 0 {
                    g = ys
                        .iter()
                        .map(|y| y + cartopolis_geo::tunnels::level_depth(o.4))
                        .collect();
                }
                g
            })
            .collect();
        let inside = road_levels::structures(&lines, &solved.heights, &land);

        let own = self.road_heights.entry((tx, ty)).or_default();
        for (((mine, key, ..), ys), (g, m)) in owners
            .iter()
            .zip(&solved.heights)
            .zip(land.iter().zip(&inside))
        {
            if *mine {
                own.runs.insert(*key, ys.clone());
                if m.iter().any(|&b| b) {
                    own.land.insert(
                        *key,
                        g.iter()
                            .zip(m)
                            .map(|(&v, &b)| if b { v } else { f32::NAN })
                            .collect(),
                    );
                }
            }
        }

        // The structures' stretches, each with its land.
        let mut stretches: Vec<(Vec<[f32; 2]>, Vec<f32>, f32)> = Vec::new();
        for ((l, o), (g, m)) in lines.iter().zip(&owners).zip(land.iter().zip(&inside)) {
            let mut i = 0;
            while i < m.len() {
                if !m[i] {
                    i += 1;
                    continue;
                }
                let start = i;
                while i < m.len() && m[i] {
                    i += 1;
                }
                if i - start >= 2 {
                    stretches.push((l.points[start..i].to_vec(), g[start..i].to_vec(), o.2));
                }
            }
        }

        // Water carved and holes filled as before; the roads are not flattened
        // into the ground any more — the ground is laid to them.
        terrain_grade::grade(
            &mut raw,
            lattice,
            &level_at,
            Vec::new(),
            Vec::new(),
            &[],
            &[],
        );
        // **Below the land the ground is the land**: put back over every lid and
        // beside every wall, where the survey still shows the trench it was flown
        // over — but not in the water, whose bed was carved.
        let fills: Vec<terrain_grade::FillLine<'_>> = stretches
            .iter()
            .map(|(pts, g, half)| terrain_grade::FillLine {
                points: pts,
                land: g,
                half_w: *half,
            })
            .collect();
        terrain_grade::fill_to_land(
            &mut raw,
            lattice,
            &fills,
            cartopolis_geo::tunnels::DATUM_M,
            &|p| level_at(p[0], p[1]).is_some(),
        );
        // **At grade the ground is laid to the road.** Every stretch of every
        // street at level 0 that is not a structure: the embankments up to the
        // decks and the banks down to a shallow dip. Not inside a structure's
        // corridor, which the client opens and walls itself.
        let conform: Vec<ConformRoad<'_>> = lines
            .iter()
            .zip(&owners)
            .zip(&solved.heights)
            .filter(|((_, o), _)| o.3 == 0)
            .map(|((l, o), ys)| ConformRoad {
                points: &l.points,
                ys,
                half_w: o.2,
            })
            .collect();
        let in_a_structure = |p: [f32; 2]| {
            stretches.iter().any(|(pts, _, half)| {
                pts.windows(2).any(|w| {
                    let (a, b) = (w[0], w[1]);
                    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
                    let len2 = dx * dx + dz * dz;
                    let t = if len2 > 1e-9 {
                        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    (p[0] - a[0] - dx * t).hypot(p[1] - a[1] - dz * t)
                        <= *half + cartopolis_geo::tunnels::WALL_REACH_M
                })
            })
        };
        terrain_grade::conform_to_roads(&mut raw, lattice, &conform, &in_a_structure);

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
    write_road_heights(ctx, &grader.road_heights)?;
    Ok((graded, grader.decks_measured, grader.decks_lifted))
}

/// How far past a tunnel's carriageway a road crossing over it stands on the
/// lid rather than on the survey, metres: the tunnel's walls and a margin.
const LID_REACH_M: f32 = 4.0;

/// The level of the land around `p` — what a lid over a tunnel is filled back to:
/// the 80th percentile of sixteen samples, eight at 30 m and eight at 60 m, the
/// client's `TerrainSnapshot::lid_level` on the raw survey (missing samples
/// skipped). NaN where none of them was measured.
/// Give each `NaN` in a line's values the nearest finite one along it. A line
/// with none keeps its `NaN`s.
fn fill_along(v: &mut [f32]) {
    let mut last = f32::NAN;
    for x in v.iter_mut() {
        if x.is_finite() {
            last = *x;
        } else if last.is_finite() {
            *x = last;
        }
    }
    let mut next = f32::NAN;
    for x in v.iter_mut().rev() {
        if x.is_finite() {
            next = *x;
        } else if next.is_finite() {
            *x = next;
        }
    }
}

fn lid_level(sample: &dyn Fn([f32; 2]) -> f32, p: [f32; 2]) -> f32 {
    let radius = cartopolis_geo::tunnels::DATUM_M;
    let mut ring: Vec<f32> = (0..16)
        .map(|k| {
            let a = std::f32::consts::TAU * (k % 8) as f32 / 8.0
                + if k >= 8 {
                    std::f32::consts::PI / 8.0
                } else {
                    0.0
                };
            let r = if k >= 8 { radius * 2.0 } else { radius };
            sample([p[0] + r * a.cos(), p[1] + r * a.sin()])
        })
        .filter(|v| v.is_finite())
        .collect();
    if ring.is_empty() {
        return f32::NAN;
    }
    ring.sort_by(f32::total_cmp);
    ring[(ring.len() * 12) / 16]
}

/// Every graded tile's street heights, solved on the network
/// (`road_levels`): `roads/14/x/y.bin` in [`RoadHeights`]' format, keyed by
/// the run names the client derives from the same tile, plus
/// `roads/coverage.json`.
fn write_road_heights(ctx: &Ctx, roads: &HashMap<(i64, i64), RoadHeights>) -> Outcome<()> {
    let mut cells = Vec::new();
    for (&(x, y), heights) in roads {
        ctx.write_bytes_gzipped(&format!("roads/14/{x}/{y}.bin"), &heights.encode())?;
        cells.push([x, y]);
    }
    cells.sort_unstable();
    ctx.write_json(
        "roads/coverage.json",
        &serde_json::json!({
            "attribution": "AHN (Rijkswaterstaat / Het Waterschapshuis), CC0; © OpenStreetMap contributors",
            "cells": cells,
            "name": ctx.name,
            "zoom": 14,
        }),
    )
}

/// The deck heights measured while grading, per z14 cell, for the client to
/// place its bridge decks with instead of asking PDOK itself: `{"decks": [[fx, fz,
/// clearance], …]}`, the deck's bounding-box centre in fractions of its tile and
/// its clearance in metres, 0 for a deck laid flat. Every cell the grading
/// tessellated is written, including ones with no decks, so the client can tell
/// "no decks here" from "not measured".
/// `decks/14/x/y.json`: `{"decks": [[x, y, height, top, profile], …]}` — the
/// centre in tile fractions, the clearance, the median top in NAP (`null` for a
/// flat deck) and the profile (`DeckProfile::to_wire`, `null` without one).
fn write_decks(ctx: &Ctx, decks: &HashMap<(i64, i64), Vec<serde_json::Value>>) -> Outcome<()> {
    let mut cells = Vec::new();
    for (&(x, y), rows) in decks {
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
