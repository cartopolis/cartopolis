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
    /// Each cell's surveyed road surfaces, as outer rings of (lat, lng).
    paving: HashMap<(i64, i64), Vec<Vec<(f64, f64)>>>,
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
    /// Every tunnel mouth the graded tiles build, as published — see
    /// [`write_mouths`].
    pub mouths: Vec<serde_json::Value>,
    /// Every tunnel the graded tiles have that no structure was built for.
    pub unbuilt: Vec<serde_json::Value>,
    /// Every flaw `tunnels::audit` finds in the structures the graded tiles
    /// build — see [`write_mouths`].
    pub flaws: Vec<serde_json::Value>,
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
            paving: HashMap::new(),
            deck_heights: HashMap::new(),
            road_heights: HashMap::new(),
            decks_measured: 0,
            decks_lifted: 0,
            mouths: Vec::new(),
            unbuilt: Vec::new(),
            flaws: Vec::new(),
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

    /// A cell's surveyed road surfaces from the `bgt` dataset's output, outer
    /// rings only; empty where that dataset has not run. Verges and traffic
    /// islands are left out: nothing drives down them into a trench.
    fn paving(&mut self, x: i64, y: i64) -> &[Vec<(f64, f64)>] {
        let ctx = self.ctx;
        self.paving.entry((x, y)).or_insert_with(|| {
            let Some(bytes) = ctx.read_site(&format!("bgt/14/{x}/{y}.surfaces.json")) else {
                return Vec::new();
            };
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                return Vec::new();
            };
            v["surfaces"]
                .as_array()
                .map(|all| {
                    all.iter()
                        .filter(|s| {
                            !matches!(s.get(0).and_then(serde_json::Value::as_u64), Some(4 | 5))
                        })
                        .filter_map(|s| {
                            let ring: Vec<(f64, f64)> = s
                                .get(1)?
                                .get(0)?
                                .as_array()?
                                .iter()
                                .filter_map(|p| Some((p.get(0)?.as_f64()?, p.get(1)?.as_f64()?)))
                                .collect();
                            (ring.len() >= 3).then_some(ring)
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
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

        // **The surveyed paving under the window** (the BGT's road surfaces,
        // `bgt`), for how wide a sunken road's trench is — see [`Paving`].
        let mut paving = Paving::default();
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                for ring in self.paving(tx + dx, ty + dy).to_vec() {
                    paving.push(
                        ring.iter()
                            .map(|&(la, ln)| {
                                let (fx, fy) = lat_lng_to_tile_frac(la, ln, 14);
                                [
                                    ((fx - tx as f64) * f64::from(tile_m)) as f32,
                                    ((fy - ty as f64) * f64::from(tile_m)) as f32,
                                ]
                            })
                            .collect(),
                    );
                }
            }
        }
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
        // **Under a lifted deck a road is under a span, not under the ground.**
        // OSM tags a path beneath a bridge as a tunnel as readily as one beneath
        // an embankment, and read as a bore it was given a lid at the level of
        // the land around: under the ring's bridge by Kardinge that filled the
        // space beneath the deck with ten metres of grass, where the photograph
        // shows the path at grade between the abutments (2026-09-28). Such a
        // point is measured like any road: its height from the survey, its land
        // from its banks, its walls from the survey — and a point atlas measured
        // walls for is not roofed by the client (`tunnels::plan`).
        let spanned = |p: [f32; 2]| {
            plates.iter().any(|plate| {
                plate
                    .deck
                    .contains([p[0] - plate.world[0], p[1] - plate.world[1]])
            })
        };
        // **Over a tunnel the survey may be stale.** Groningen's ring was flown as
        // an open trench and has a lid on it now, so a street crossing the lid
        // would read the trench floor nine metres down, and a path across the
        // park on top of it two to four. A road is measured on the ground as it
        // is put back (`fill_to_land`): the lid over a tunnel's corridor, blending
        // back to the survey over the same taper — but never lowered by it, and
        // not by a tunnel the road itself runs into, whose ramp the survey
        // measured truly.
        let mut bores: Vec<(Vec<[f32; 2]>, f32, [f32; 4])> = Vec::new();
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let Some(cell) = self.cells.get(&(tx + dx, ty + dy)) else {
                    continue;
                };
                let s = tile_m / cell.tile_m;
                let off = [dx as f32 * tile_m, dy as f32 * tile_m];
                for run in cell.runs.iter().filter(|r| r.level < 0) {
                    let pts: Vec<[f32; 2]> = run
                        .points
                        .iter()
                        .map(|p| [off[0] + p[0] * s, off[1] + p[1] * s])
                        .collect();
                    if pts.iter().all(|&p| spanned(p)) {
                        continue;
                    }
                    let pad = run.half_w * s + LID_REACH_M + cartopolis_geo::tunnels::DATUM_M;
                    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
                    for p in &pts {
                        b = [
                            b[0].min(p[0]),
                            b[1].min(p[1]),
                            b[2].max(p[0]),
                            b[3].max(p[1]),
                        ];
                    }
                    bores.push((
                        pts,
                        run.half_w * s,
                        [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad],
                    ));
                }
            }
        }
        // How much a point stands on a lid rather than on the survey: 1 within
        // `LID_REACH_M` of a tunnel's carriageway, falling smoothly to 0 over the
        // fill's taper, and 0 for the tunnels in `skip`.
        let lid_weight = |p: [f32; 2], skip: &[usize]| -> f32 {
            let mut best = 0.0f32;
            for (bi, (pts, half, b)) in bores.iter().enumerate() {
                if skip.contains(&bi) || p[0] < b[0] || p[1] < b[1] || p[0] > b[2] || p[1] > b[3] {
                    continue;
                }
                for w in pts.windows(2) {
                    let (a, c) = (w[0], w[1]);
                    let (dx, dz) = (c[0] - a[0], c[1] - a[1]);
                    let len2 = dx * dx + dz * dz;
                    let t = if len2 > 1e-9 {
                        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let d = (p[0] - a[0] - dx * t).hypot(p[1] - a[1] - dz * t) - half - LID_REACH_M;
                    let wt = if d <= 0.0 {
                        1.0
                    } else if d >= cartopolis_geo::tunnels::DATUM_M {
                        0.0
                    } else {
                        let u = 1.0 - d / cartopolis_geo::tunnels::DATUM_M;
                        u * u * (3.0 - 2.0 * u)
                    };
                    best = best.max(wt);
                }
                if best >= 1.0 {
                    break;
                }
            }
            best
        };
        // The tunnels a line runs into: any whose line passes within a join's
        // reach of either of its ends.
        let runs_into = |points: &[[f32; 2]]| -> Vec<usize> {
            let ends = [points[0], points[points.len() - 1]];
            bores
                .iter()
                .enumerate()
                .filter(|(_, (pts, ..))| {
                    pts.iter().any(|q| {
                        ends.iter()
                            .any(|e| (q[0] - e[0]).hypot(q[1] - e[1]) <= road_levels::TEE_M)
                    })
                })
                .map(|(bi, _)| bi)
                .collect()
        };
        let mut lines: Vec<road_levels::Line> = Vec::new();
        // Per line, which of its points are a tunnel's under a lifted deck.
        let mut span: Vec<Vec<bool>> = Vec::new();
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
                    let own_bores = if run.level == 0 {
                        runs_into(&points)
                    } else {
                        Vec::new()
                    };
                    let heights = points
                        .iter()
                        .map(|&p| {
                            if run.level < 0 && !spanned(p) {
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
                            let here = sample(p);
                            let w = lid_weight(p, &own_bores);
                            let v = if w > 0.0 {
                                let lid = lid_level(&sample, p);
                                match (here.is_finite(), lid.is_finite()) {
                                    (true, true) => here + w * (lid - here).max(0.0),
                                    (false, true) => lid,
                                    _ => here,
                                }
                            } else {
                                here
                            };
                            if v.is_finite() {
                                Height::Measured { y: v, weight: 1.0 }
                            } else {
                                Height::Unknown
                            }
                        })
                        .collect();
                    span.push(
                        points
                            .iter()
                            .map(|&p| run.level < 0 && spanned(p))
                            .collect(),
                    );
                    lines.push(road_levels::Line {
                        points,
                        heights,
                        level: run.level,
                        max_grade: road_levels::max_grade(run.class),
                        cut: run.cut,
                    });
                    owners.push((
                        dx == 0 && dy == 0,
                        run.key,
                        run.half_w,
                        run.level,
                        run.class,
                    ));
                }
            }
        }
        let mut solved = road_levels::solve(&lines);

        // **The land each road runs through**, for deciding which stretches are
        // structures below the ground and how high their walls reach. Over a
        // tunnel it is the lid — the level of the land around (`lid_level`), the
        // same the ceilings are measured against. Beside any other road it is the
        // **bank**: the higher of the ground either side just past where a wall
        // would stand (`bank_level`), which is what a retaining wall holds back.
        // Not the lid level there: a ring of samples 30 and 60 m out reaches the
        // lids and embankments near a cutting, and read a cycle path across a
        // park sloping down from the ring as nearly three metres under its land
        // — it was walled for a hundred metres where the photograph shows it at
        // grade (2026-09-28). Only below a bridge's level — a deck's structure is
        // its own. A point the survey has nothing beside takes its neighbour's
        // along the line, and a tunnel with none at all is a level's depth under
        // its road.
        let derive = |lines: &[road_levels::Line], heights: &[Vec<f32>]| {
            let mut land: Vec<Vec<f32>> = lines
                .iter()
                .zip(&owners)
                .zip(heights)
                .zip(&span)
                .map(|(((l, o), ys), sp)| {
                    if l.level > 0 {
                        return vec![f32::NAN; l.points.len()];
                    }
                    let mut g: Vec<f32> = (0..l.points.len())
                        .map(|i| {
                            if l.level < 0 && !sp[i] {
                                lid_level(&sample, l.points[i])
                            } else {
                                bank_level(&sample, &l.points, i, paving.from(&l.points, i, o.2))
                            }
                        })
                        .collect();
                    fill_along(&mut g);
                    // **A street the survey has no bank for at all** — the ramp into
                    // the bike park under Groningen's station square, which AHN masks
                    // with the garage like a building — takes the level of the land
                    // around, as a lid does. Left without land it was never a
                    // structure, and the ramp the photograph shows was not there.
                    if l.level == 0 && g.iter().all(|v| !v.is_finite()) {
                        g = l.points.iter().map(|&p| lid_level(&sample, p)).collect();
                    }
                    if g.iter().all(|v| !v.is_finite()) && l.level < 0 && !sp.contains(&true) {
                        g = ys
                            .iter()
                            .map(|y| y + cartopolis_geo::tunnels::level_depth(o.4))
                            .collect();
                    }
                    g
                })
                .collect();
            let mut inside = road_levels::structures(lines, heights, &land);
            in_the_same_trench(lines, &owners, heights, &mut land, &mut inside);
            for (g, m) in land.iter_mut().zip(&inside) {
                smooth_land(g, m);
            }
            // **Where the walls stand, and whether there are any**, from the survey
            // either side of every open stretch — see `wall_side`. A stretch sloped on
            // both sides is no structure at all: the survey's own slope is its
            // ground, and the road is laid in it as any road is.
            let mut walls: Vec<Vec<[f32; 2]>> = lines
                .iter()
                .map(|l| vec![[f32::NAN; 2]; l.points.len()])
                .collect();
            for (li, l) in lines.iter().enumerate() {
                if l.level > 0 || !inside[li].iter().any(|&b| b) {
                    continue;
                }
                for vi in 0..l.points.len() {
                    if inside[li][vi] && (l.level == 0 || span[li][vi]) {
                        walls[li][vi] = wall_sides(
                            &sample,
                            &l.points,
                            vi,
                            paving.from(&l.points, vi, owners[li].2),
                            heights[li][vi],
                            land[li][vi],
                        );
                    }
                }
                smooth_walls(&mut walls[li], &inside[li]);
                for vi in 0..l.points.len() {
                    if inside[li][vi] && walls[li][vi] == [0.0, 0.0] {
                        inside[li][vi] = false;
                        land[li][vi] = f32::NAN;
                        walls[li][vi] = [f32::NAN; 2];
                    }
                }
            }
            (land, inside, walls)
        };
        let (mut land, mut inside, mut walls) = derive(&lines, &solved.heights);
        // **A street on a structure's surface is on the structure.** OSM maps
        // the footway and the cycle path down one ramp as two ways and tags
        // only one; the survey has a hole across the ramp, so the other read at
        // grade and the ramp's opening cut through it — eleven of the eighteen
        // streets the audit found cut (`tunnels::audit`, 2026-09-29). Such a
        // street is pinned to the structure's road where the two overlap, and
        // the network is solved again, so its grades carry it into the ramp
        // and back out rather than stepping.
        let pins = on_the_same_surface(&lines, &owners, &solved.heights, &land, &inside, &walls);
        if !pins.is_empty() {
            for &(li, vi, y) in &pins {
                lines[li].heights[vi] = Height::Measured { y, weight: 1.0 };
            }
            solved = road_levels::solve(&lines);
            (land, inside, walls) = derive(&lines, &solved.heights);
        }
        let own = self.road_heights.entry((tx, ty)).or_default();
        for ((((mine, key, ..), ys), (g, m)), w) in owners
            .iter()
            .zip(&solved.heights)
            .zip(land.iter().zip(&inside))
            .zip(&walls)
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
                    own.walls.insert(
                        *key,
                        w.iter()
                            .zip(m)
                            .map(|(&v, &b)| if b { v } else { [f32::NAN; 2] })
                            .collect(),
                    );
                }
            }
        }

        // **Every tunnel mouth this tile builds, and every tunnel it does not**,
        // from the same plans the client draws — for the gallery that photographs
        // each of them, and for the one number that says a tunnel went missing.
        {
            let own_runs = &self.cells[&(tx, ty)].runs;
            let profiles: Vec<Option<cartopolis_geo::tunnels::Profile>> = own_runs
                .iter()
                .map(|r| {
                    Some(cartopolis_geo::tunnels::Profile {
                        road: own.runs.get(&r.key)?.clone(),
                        land: own.land.get(&r.key)?.clone(),
                        walls: own.walls.get(&r.key).cloned().unwrap_or_default(),
                    })
                })
                .collect();
            let ll = |p: [f32; 2]| {
                tile_frac_to_lat_lng(
                    tx as f64 + f64::from(p[0] / tile_m),
                    ty as f64 + f64::from(p[1] / tile_m),
                    14,
                )
            };
            let round = |v: f64| (v * 1e6).round() / 1e6;
            for f in cartopolis_geo::tunnels::audit(own_runs, &profiles) {
                let (lat, lng) = ll(f.at);
                self.flaws.push(serde_json::json!({
                    "kind": f.flaw.slug(),
                    "note": f.note,
                    "lat": round(lat),
                    "lng": round(lng),
                    "tile": [tx, ty],
                }));
            }
            for m in cartopolis_geo::tunnels::mouths(own_runs, &profiles) {
                let (lat, lng) = ll(m.at);
                // +z runs south, so north is -z.
                let heading = f64::from(m.out[0].atan2(-m.out[1]))
                    .to_degrees()
                    .rem_euclid(360.0);
                self.mouths.push(serde_json::json!({
                    "lat": round(lat),
                    "lng": round(lng),
                    "out_deg": heading.round(),
                    "depth": (f64::from(m.land_y - m.road_y) * 100.0).round() / 100.0,
                    "tile": [tx, ty],
                }));
            }
            // A tunnel that runs under a lifted deck is a road under a span
            // there, measured like any road, and may rightly build nothing.
            let under_a_span: std::collections::HashSet<u64> = owners
                .iter()
                .zip(&span)
                .filter(|((mine, ..), sp)| *mine && sp.contains(&true))
                .map(|((_, key, ..), _)| *key)
                .collect();
            // And one that never runs below its land is at grade — a passage
            // through a block, tagged a tunnel — and rightly builds nothing
            // (`road_levels::structures`). Only a tunnel below the ground that
            // got no structure is a tunnel gone missing.
            let below: std::collections::HashSet<u64> = owners
                .iter()
                .enumerate()
                .filter(|(li, (mine, ..))| {
                    *mine
                        && solved.heights[*li]
                            .iter()
                            .zip(&land[*li])
                            .any(|(y, g)| !g.is_finite() || *y < g - road_levels::SUNK_M)
                })
                .map(|(_, (_, key, ..))| *key)
                .collect();
            for (r, p) in own_runs.iter().zip(&profiles) {
                if r.level < 0
                    && below.contains(&r.key)
                    && !under_a_span.contains(&r.key)
                    && p.as_ref()
                        .is_none_or(|p| p.land.iter().all(|g| !g.is_finite()))
                {
                    let (lat, lng) = ll(r.points[r.points.len() / 2]);
                    self.unbuilt.push(serde_json::json!({
                        "lat": round(lat),
                        "lng": round(lng),
                        "tile": [tx, ty],
                    }));
                }
            }
        }

        // The structures' stretches, each with its land.
        let mut stretches: Vec<(Vec<[f32; 2]>, Vec<f32>, f32, Vec<[f32; 2]>)> = Vec::new();
        for (((l, o), (g, m)), w) in lines
            .iter()
            .zip(&owners)
            .zip(land.iter().zip(&inside))
            .zip(&walls)
        {
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
                    stretches.push((
                        l.points[start..i].to_vec(),
                        g[start..i].to_vec(),
                        o.2,
                        w[start..i].to_vec(),
                    ));
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
            .map(|(pts, g, half, w)| terrain_grade::FillLine {
                points: pts,
                land: g,
                half_w: *half,
                walls: w,
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
        // Out to the wall on a walled side; on a side that is a bank the ground
        // is laid to the road like any road's.
        let in_a_structure = |p: [f32; 2]| {
            fills.iter().any(|line| {
                let last = line.points.len().saturating_sub(2);
                line.points.windows(2).enumerate().any(|(s, w)| {
                    let (a, b) = (w[0], w[1]);
                    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
                    let len2 = dx * dx + dz * dz;
                    let t = if len2 > 1e-9 {
                        ((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2
                    } else {
                        0.0
                    };
                    // Not past either end, as for the fill.
                    if (s == 0 && t < 0.0) || (s == last && t > 1.0) {
                        return false;
                    }
                    let t = t.clamp(0.0, 1.0);
                    line.reach(s, t, line.side_of(s, p)).is_some_and(|r| {
                        (p[0] - a[0] - dx * t).hypot(p[1] - a[1] - dz * t)
                            <= r + cartopolis_geo::tunnels::WALL_REACH_M
                    })
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
    write_mouths(ctx, &grader.mouths, &grader.unbuilt, &grader.flaws)?;
    Ok((graded, grader.decks_measured, grader.decks_lifted))
}

/// How far past a tunnel's carriageway a road crossing over it stands on the
/// lid rather than on the survey, metres: the tunnel's walls and a margin.
const LID_REACH_M: f32 = 4.0;

/// The level of the land around `p` — what a lid over a tunnel is filled back to:
/// the 80th percentile of sixteen samples, eight at 30 m and eight at 60 m, the
/// client's `TerrainSnapshot::lid_level` on the raw survey (missing samples
/// skipped). NaN where none of them was measured.
/// How far past a carriageway's edge its wall is looked for, metres.
const WALL_SEARCH_M: f32 = 15.0;

/// The widest a rise from the road to the land may be and still be a wall,
/// metres. The survey is read on its own lattice, about three metres at z14, so a
/// vertical wall between two samples reads as a rise over one of them; a bank at
/// one in two over a two-metre cut takes four.
const WALL_RISE_M: f32 = 3.0;

/// Where the walls beside point `i` of a line stand, as distances from its centre
/// line, left then right: 0 where that side has none, because the ground rises
/// there as a slope.
///
/// **The survey says whether there is a wall.** A wall takes the ground from the
/// road to its land within a metre or two — and the bare-ground model has usually
/// cut the wall out, leaving a hole that is skipped — while a bank rises over
/// several metres. The wall stands just past the last ground still at the
/// road's level, which is also how wide the trench is: a cycle underpass's ramp
/// was drawn as a slot the width of the path's line in the middle of paving at
/// street level, where the aerial photograph shows the whole ramp going down,
/// five or six metres wide, between grass slopes (2026-09-28).
fn wall_sides(
    sample: &dyn Fn([f32; 2]) -> f32,
    points: &[[f32; 2]],
    i: usize,
    from: [f32; 2],
    road: f32,
    land: f32,
) -> [f32; 2] {
    let depth = land - road;
    if !(depth > road_levels::SUNK_M) {
        return [f32::NAN; 2];
    }
    let (a, b) = (
        points[i.saturating_sub(1)],
        points[(i + 1).min(points.len() - 1)],
    );
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dz);
    if len <= 1e-6 {
        return [f32::NAN; 2];
    }
    let n = [-dz / len, dx / len];
    let p = points[i];
    let (low, high) = (road + 0.5 * depth, road + 0.8 * depth);
    let side = |s: f32, half_w: f32| {
        let mut last_low = half_w;
        let mut d = half_w + 0.5;
        let mut seen = false;
        // Whether anything was measured part way up since the last ground at
        // the road's level. The survey is sampled bilinearly, so a bank always
        // leaves some; a wall leaves none where the survey cut it out, however
        // wide the hole — the ramp into the bike park under the station square
        // is a hole the whole width of the ramp.
        let mut part_way = false;
        while d <= half_w + WALL_SEARCH_M {
            let v = sample([p[0] + n[0] * s * d, p[1] + n[1] * s * d]);
            if v.is_finite() {
                seen = true;
                if v >= high {
                    return if !part_way || d - last_low <= WALL_RISE_M {
                        last_low + 0.5
                    } else {
                        0.0
                    };
                }
                if v < low {
                    last_low = d;
                    part_way = false;
                } else {
                    part_way = true;
                }
            }
            d += 0.5;
        }
        // Nothing measured on this side at all is not a bank: it is not known,
        // and the wall stands at the carriageway.
        if seen {
            0.0
        } else {
            f32::NAN
        }
    };
    [side(1.0, from[0]), side(-1.0, from[1])]
}

/// Walls along a line with the single-point misreads taken out: on each side,
/// whether there is a wall by majority over [`LAND_SMOOTH`] points either side,
/// and where it stands by the median of the walled ones.
fn smooth_walls(walls: &mut [[f32; 2]], inside: &[bool]) {
    let src = walls.to_vec();
    for i in 0..walls.len() {
        if !inside[i] {
            continue;
        }
        let (lo, hi) = (
            i.saturating_sub(LAND_SMOOTH),
            (i + LAND_SMOOTH + 1).min(walls.len()),
        );
        for s in 0..2 {
            let known: Vec<f32> = (lo..hi)
                .filter(|&k| inside[k] && src[k][s].is_finite())
                .map(|k| src[k][s])
                .collect();
            if known.is_empty() {
                continue;
            }
            let mut walled: Vec<f32> = known.iter().copied().filter(|&v| v > 0.0).collect();
            walls[i][s] = if walled.len() * 2 > known.len() {
                walled.sort_by(f32::total_cmp);
                walled[walled.len() / 2]
            } else {
                0.0
            };
        }
    }
}

/// Points either side a structure's land is smoothed over along its line.
const LAND_SMOOTH: usize = 3;

/// A structure's land with the single-point misreads taken out: the median over
/// [`LAND_SMOOTH`] points either side along the line, among the points that are
/// part of the structure. A bank read into the survey's hole beside a wall, or
/// onto the lane beside, is one point of road-level land in a run of land, and
/// would put a notch the depth of the cutting into the top of its wall.
fn smooth_land(land: &mut [f32], inside: &[bool]) {
    let src = land.to_vec();
    for i in 0..land.len() {
        if !inside[i] {
            continue;
        }
        let (lo, hi) = (
            i.saturating_sub(LAND_SMOOTH),
            (i + LAND_SMOOTH + 1).min(land.len()),
        );
        let mut v: Vec<f32> = (lo..hi)
            .filter(|&k| inside[k] && src[k].is_finite())
            .map(|k| src[k])
            .collect();
        if v.is_empty() {
            continue;
        }
        v.sort_by(f32::total_cmp);
        land[i] = v[v.len() / 2];
    }
}

/// Every point of a street at grade that lies on an open structure's surface —
/// a ramp's or a cutting's: between its walls (its own half-width where none
/// was measured standing back), with its own half-width and half a metre, and
/// running within 35° of its heading, where that structure is below its land. With the height
/// to pin it to — the structure's road there.
fn on_the_same_surface(
    lines: &[road_levels::Line],
    owners: &[(bool, u64, f32, i8, cartopolis_geo::road_texture::RoadClass)],
    road: &[Vec<f32>],
    land: &[Vec<f32>],
    inside: &[Vec<bool>],
    walls: &[Vec<[f32; 2]>],
) -> Vec<(usize, usize, f32)> {
    const CELL_M: f32 = 16.0;
    const PARALLEL_COS: f32 = 0.819;
    let cell = |p: [f32; 2]| {
        (
            (p[0] / CELL_M).floor() as i64,
            (p[1] / CELL_M).floor() as i64,
        )
    };
    let heading = |pts: &[[f32; 2]], i: usize| {
        let (a, b) = (pts[i.saturating_sub(1)], pts[(i + 1).min(pts.len() - 1)]);
        let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dz);
        (len > 1e-6).then(|| [dx / len, dz / len])
    };
    // Only the open structures — the ramps and cuttings, at level 0 — since a
    // street along the top of a tunnel's lid runs parallel to the tunnel and
    // over it, and must never be pulled down onto its road.
    let mut near: HashMap<(i64, i64), Vec<(usize, usize)>> = HashMap::new();
    for (li, l) in lines.iter().enumerate() {
        if l.level != 0 {
            continue;
        }
        for vi in 0..l.points.len() {
            if inside[li][vi]
                && land[li][vi].is_finite()
                && road[li][vi] < land[li][vi] - road_levels::SUNK_M
            {
                near.entry(cell(l.points[vi])).or_default().push((li, vi));
            }
        }
    }
    let mut out = Vec::new();
    for (li, l) in lines.iter().enumerate() {
        if l.level != 0 {
            continue;
        }
        for (vi, &p) in l.points.iter().enumerate() {
            if inside[li][vi] {
                continue;
            }
            let Some(h) = heading(&l.points, vi) else {
                continue;
            };
            let (cx, cz) = cell(p);
            let mut best: Option<(f32, f32)> = None;
            for dx in -1..=1 {
                for dz in -1..=1 {
                    for &(o, oi) in near.get(&(cx + dx, cz + dz)).into_iter().flatten() {
                        if o == li {
                            continue;
                        }
                        let q = lines[o].points[oi];
                        let d = (q[0] - p[0]).hypot(q[1] - p[1]);
                        let Some(oh) = heading(&lines[o].points, oi) else {
                            continue;
                        };
                        // Between the structure's walls: out to the wall on
                        // the side the street is on, where one was measured
                        // standing back; the two half-widths otherwise.
                        let across = (p[0] - q[0]) * -oh[1] + (p[1] - q[1]) * oh[0];
                        let wall = walls[o][oi][usize::from(across < 0.0)];
                        let reach = if wall > 0.0 { wall } else { owners[o].2 }.max(owners[o].2)
                            + owners[li].2
                            + 0.5;
                        if d > reach + 3.0 || across.abs() > reach {
                            continue;
                        }
                        if (h[0] * oh[0] + h[1] * oh[1]).abs() < PARALLEL_COS {
                            continue;
                        }
                        if best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, road[o][oi]));
                        }
                    }
                }
            }
            if let Some((_, y)) = best {
                out.push((li, vi, y));
            }
        }
    }
    out
}

/// How far beside a structure another road may run and still be in its trench,
/// metres between their centre lines.
const TRENCH_WIDTH_M: f32 = 20.0;

/// **A road beside a structure, at its depth, is in its trench.** The bank a
/// road is measured against (`bank_level`) is the ground just past its own
/// edge, and in a trench carrying a dual carriageway and a slip road, the lane
/// beside one of them is another lane: over Groningen's ring one carriageway of a
/// cutting was walled and its twin fifteen metres off was not, for eighty metres
/// where a slip road ran between it and the wall. So a ground road that is not
/// part of a structure, and runs within [`TRENCH_WIDTH_M`] of one at the same
/// depth to within `SUNK_M` while well under that structure's land, joins it
/// and takes its land. A path across the park beside a cutting is metres higher
/// than the road in it, and stays out.
fn in_the_same_trench(
    lines: &[road_levels::Line],
    owners: &[(bool, u64, f32, i8, cartopolis_geo::road_texture::RoadClass)],
    road: &[Vec<f32>],
    land: &mut [Vec<f32>],
    inside: &mut [Vec<bool>],
) {
    // Every structure point, bucketed: (line, x, z, road, land).
    let cell = |p: [f32; 2]| {
        (
            (p[0] / TRENCH_WIDTH_M).floor() as i64,
            (p[1] / TRENCH_WIDTH_M).floor() as i64,
        )
    };
    let mut near: HashMap<(i64, i64), Vec<(usize, [f32; 2], f32, f32)>> = HashMap::new();
    for (li, l) in lines.iter().enumerate() {
        for (vi, &p) in l.points.iter().enumerate() {
            if inside[li][vi] && land[li][vi].is_finite() {
                near.entry(cell(p))
                    .or_default()
                    .push((li, p, road[li][vi], land[li][vi]));
            }
        }
    }
    let sunk = road_levels::SUNK_M;
    let mut add: Vec<(usize, usize, f32)> = Vec::new();
    for (li, l) in lines.iter().enumerate() {
        if l.level != 0 || owners[li].3 != 0 {
            continue;
        }
        for (vi, &p) in l.points.iter().enumerate() {
            if inside[li][vi] {
                continue;
            }
            let y = road[li][vi];
            let (cx, cz) = cell(p);
            let mut best: Option<(f32, f32)> = None;
            for dx in -1..=1 {
                for dz in -1..=1 {
                    for &(o, q, oy, og) in near.get(&(cx + dx, cz + dz)).into_iter().flatten() {
                        let d = (q[0] - p[0]).hypot(q[1] - p[1]);
                        if o != li
                            && d <= TRENCH_WIDTH_M
                            && (oy - y).abs() <= sunk
                            && y < og - sunk
                            && best.is_none_or(|(bd, _)| d < bd)
                        {
                            best = Some((d, og));
                        }
                    }
                }
            }
            if let Some((_, g)) = best {
                add.push((li, vi, g));
            }
        }
    }
    for (li, vi, g) in add {
        inside[li][vi] = true;
        land[li][vi] = g;
    }
}

/// Where a carriageway's bank is looked for, metres past its edge: from past a
/// wall's own width, across whatever hole the survey left beside it.
const BANK_M: [f32; 2] = [2.0, 20.0];

/// How far past the first measured ground the bank is read, metres.
const BANK_READ_M: f32 = 4.0;

/// The ground a wall beside point `i` of a line would hold back.
///
/// On each side, the first measured ground past the carriageway (skipping the
/// hole the survey's bare-ground model leaves where it cut a wall out), and the
/// highest of it over the next [`BANK_READ_M`]; then the higher of the two
/// sides — a dual carriageway's median lies in the trench with it, and the outer
/// side is the land.
///
/// **Close, because a wall is abrupt and a slope is not.** A cutting's wall takes
/// the ground from the road to the land within a metre or two; a path across a
/// park that falls gently towards the ring is two and a half metres under the
/// ground sixteen metres off and at grade beside itself, and read that far out it
/// was walled for a hundred metres where the photograph shows none (2026-09-28).
/// NaN where neither side was measured.
fn bank_level(
    sample: &dyn Fn([f32; 2]) -> f32,
    points: &[[f32; 2]],
    i: usize,
    from: [f32; 2],
) -> f32 {
    let (a, b) = (
        points[i.saturating_sub(1)],
        points[(i + 1).min(points.len() - 1)],
    );
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dz);
    if len <= 1e-6 {
        return f32::NAN;
    }
    let n = [-dz / len, dx / len];
    let p = points[i];
    let at = |s: f32, d: f32| {
        let r = s * (if s > 0.0 { from[0] } else { from[1] } + d);
        sample([p[0] + n[0] * r, p[1] + n[1] * r])
    };
    let side = |s: f32| {
        let mut d = BANK_M[0];
        while d <= BANK_M[1] && !at(s, d).is_finite() {
            d += 1.0;
        }
        let mut best = f32::NAN;
        let end = d + BANK_READ_M;
        while d <= end {
            let v = at(s, d);
            if v.is_finite() && !(best >= v) {
                best = v;
            }
            d += 1.0;
        }
        best
    };
    let (l, r) = (side(1.0), side(-1.0));
    match (l.is_finite(), r.is_finite()) {
        (true, true) => l.max(r),
        (true, false) => l,
        (false, true) => r,
        _ => f32::NAN,
    }
}

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

/// `roads/mouths.json`: every tunnel mouth the graded tiles build — where the
/// roof stops, the compass bearing out of the tunnel and how far the road there
/// is under its land — and every tunnel that was built as no structure at all.
/// The second list should be empty; a tunnel in it is one the map tags and the
/// client will not draw. The first is what the shot gallery photographs
/// (`tools/structure_shots.ts`), one camera per mouth, placed from the data
/// rather than by hand.
///
/// And every flaw `tunnels::audit` finds, by kind: a street the opening of a
/// structure cuts through, a roof lower than any underpass is, an open end that
/// drops into nothing. Each was found once by eye and fixed by a rule, so the
/// count per kind is what says a later change broke one again — logged with
/// every run, and published so the gallery photographs those places too.
fn write_mouths(
    ctx: &Ctx,
    mouths: &[serde_json::Value],
    unbuilt: &[serde_json::Value],
    flaws: &[serde_json::Value],
) -> Outcome<()> {
    let mut by_kind: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for f in flaws {
        *by_kind
            .entry(f["kind"].as_str().unwrap_or("?"))
            .or_default() += 1;
    }
    ctx.log(format!(
        "{} tunnelmonden, {} tunnels zonder bouwwerk, gebreken: {}",
        mouths.len(),
        unbuilt.len(),
        if by_kind.is_empty() {
            "geen".to_string()
        } else {
            by_kind
                .iter()
                .map(|(k, n)| format!("{n} {k}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    ctx.write_json(
        "roads/mouths.json",
        &serde_json::json!({ "name": ctx.name, "mouths": mouths, "unbuilt": unbuilt, "flaws": flaws }),
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

/// The surveyed road surfaces under a grading window, in its tile-local metres,
/// bucketed for point lookups.
///
/// **A sunken road's trench is at least as wide as the surface it carries.**
/// OSM draws a cycle path as a line two metres wide; the ramp it runs down into
/// the bike park under Groningen's station square is ten, and the survey has a
/// hole across all of it. Read from the edge of the line, the bank landed on
/// the ramp itself and said the ramp was at grade, so it was never built
/// (2026-09-29). The BGT surveys that surface to about 20 cm, so the bank and
/// the walls are read from its edge outward.
#[derive(Default)]
struct Paving {
    rings: Vec<([f32; 4], Vec<[f32; 2]>)>,
    grid: HashMap<(i32, i32), Vec<usize>>,
}

impl Paving {
    const BUCKET_M: f32 = 32.0;

    fn push(&mut self, ring: Vec<[f32; 2]>) {
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for p in &ring {
            b = [
                b[0].min(p[0]),
                b[1].min(p[1]),
                b[2].max(p[0]),
                b[3].max(p[1]),
            ];
        }
        let i = self.rings.len();
        #[allow(clippy::cast_possible_truncation)]
        let cell = |v: f32| (v / Self::BUCKET_M).floor() as i32;
        for gx in cell(b[0])..=cell(b[2]) {
            for gz in cell(b[1])..=cell(b[3]) {
                self.grid.entry((gx, gz)).or_default().push(i);
            }
        }
        self.rings.push((b, ring));
    }

    /// How far out from point `i` of a line its trench reaches before the bank
    /// or the wall is read, left then right: the edge of the surveyed surface it
    /// runs on, where that is past its own half-width and within
    /// [`WALL_SEARCH_M`]; its half-width otherwise.
    fn from(&self, points: &[[f32; 2]], i: usize, half_w: f32) -> [f32; 2] {
        let (a, b) = (
            points[i.saturating_sub(1)],
            points[(i + 1).min(points.len() - 1)],
        );
        let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dz);
        if len <= 1e-6 || self.rings.is_empty() {
            return [half_w; 2];
        }
        let n = [-dz / len, dx / len];
        let p = points[i];
        #[allow(clippy::cast_possible_truncation)]
        let key = (
            (p[0] / Self::BUCKET_M).floor() as i32,
            (p[1] / Self::BUCKET_M).floor() as i32,
        );
        let mut out = [half_w; 2];
        for &ri in self.grid.get(&key).into_iter().flatten() {
            let (bb, ring) = &self.rings[ri];
            if p[0] < bb[0] || p[1] < bb[1] || p[0] > bb[2] || p[1] > bb[3] || !contains(ring, p) {
                continue;
            }
            for (k, s) in [1.0f32, -1.0].into_iter().enumerate() {
                let dir = [n[0] * s, n[1] * s];
                if let Some(t) = ray_exit(ring, p, dir) {
                    if t <= half_w + WALL_SEARCH_M {
                        out[k] = out[k].max(t);
                    }
                }
            }
        }
        out
    }
}

/// Even-odd point in polygon.
fn contains(ring: &[[f32; 2]], p: [f32; 2]) -> bool {
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[j]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// The distance along `dir` (a unit vector) from `p` to the nearest edge of
/// `ring` it crosses.
fn ray_exit(ring: &[[f32; 2]], p: [f32; 2], dir: [f32; 2]) -> Option<f32> {
    let mut best: Option<f32> = None;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (a, b) = (ring[j], ring[i]);
        let e = [b[0] - a[0], b[1] - a[1]];
        let den = dir[0] * e[1] - dir[1] * e[0];
        if den.abs() > 1e-9 {
            let w = [a[0] - p[0], a[1] - p[1]];
            let t = (w[0] * e[1] - w[1] * e[0]) / den;
            let u = (w[0] * dir[1] - w[1] * dir[0]) / den;
            if t > 0.0 && (0.0..=1.0).contains(&u) && best.is_none_or(|bt| t < bt) {
                best = Some(t);
            }
        }
        j = i;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A trench reaches to the edge of the surface it carries.** A path drawn
    /// two metres wide down the middle of a ramp surveyed as ten, lying 3 m to
    /// its left and 7 m to its right: the bank and the walls are read from those
    /// edges, and off the paving from the path's own.
    #[test]
    fn a_trench_reaches_the_edge_of_its_paving() {
        let mut paving = Paving::default();
        paving.push(vec![[0.0, -7.0], [40.0, -7.0], [40.0, 3.0], [0.0, 3.0]]);
        let line = [[10.0, 0.0], [20.0, 0.0], [30.0, 0.0], [50.0, 0.0]];
        // Heading east, left is +z.
        let [l, r] = paving.from(&line, 1, 1.0);
        assert!((l - 3.0).abs() < 1e-4 && (r - 7.0).abs() < 1e-4, "{l} {r}");
        assert_eq!(paving.from(&line, 3, 1.0), [1.0, 1.0], "off the paving");
    }

    /// **The survey tells a wall from a bank, and says when it cannot.** A road
    /// three metres down along x, read across on its left (+z): a wall between
    /// two samples, a wall the survey cut out as a hole, a bank rising over ten
    /// metres, and a side with no survey at all.
    #[test]
    fn a_wall_and_a_bank_read_off_the_survey() {
        let points = [[0.0, 0.0], [10.0, 0.0]];
        let read = |ground: &dyn Fn(f32) -> f32| {
            let sample = |p: [f32; 2]| if p[1] > 0.0 { ground(p[1]) } else { f32::NAN };
            wall_sides(&sample, &points, 0, [1.0, 1.0], -3.0, 0.0)[0]
        };
        // Floor to 4 m, the land from 5 m: a wall at about 4.5 m.
        let wall = read(&|z| {
            if z < 4.2 {
                -3.0
            } else if z < 5.0 {
                -1.5 + (z - 4.2)
            } else {
                0.0
            }
        });
        assert!((4.0..=5.5).contains(&wall), "a wall read as {wall}");
        // Floor to 4 m, nothing measured to 9 m, then the land: a wall.
        let cut = read(&|z| {
            if z < 4.0 {
                -3.0
            } else if z < 9.0 {
                f32::NAN
            } else {
                0.0
            }
        });
        assert!(cut > 0.0, "a wall the survey cut out read as {cut}");
        // Rising steadily from 2 m to 12 m: a bank.
        let bank = read(&|z| (-3.0 + (z - 2.0) * 0.3).clamp(-3.0, 0.0));
        assert!(bank == 0.0, "a bank read as a wall at {bank}");
        // Nothing at all: not known.
        assert!(
            read(&|_| f32::NAN).is_nan(),
            "an unmeasured side read as known"
        );
    }

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
