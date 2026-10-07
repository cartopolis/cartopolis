//! **The bridges as the client stands them**, off the same published data:
//! atlas's graded ground (`--atlas`), and the decks atlas measured and the BGT's
//! piers (`--site`).
//!
//! Without these the harness lifts every deck a flat `--deck-height` over draped
//! ground, which answers "is the geometry continuous" and nothing about whether
//! a deck meets its approach. With them it draws what the client draws — the
//! ground atlas graded, each deck along its measured profile, a pier under it —
//! in seconds, where the client's own capture on a software renderer takes most
//! of an hour to settle a viewpoint.

use std::io::Read;

use cartopolis_geo::deck_profile::{DeckMeasure, DeckProfile};
use cartopolis_geo::terrain_tile::TerrainTile;
use cartopolis_geo::vector_tiles::Approach;

use crate::{Cell, Field, Loaded};

/// The deck slab's thickness, as the client draws it (`map_geometry`).
pub const DECK_THICKNESS_M: f32 = 0.9;
/// How far a pier is sunk below the ground at its foot.
const PIER_FOOTING_M: f32 = 0.3;
/// How far past a deck's edge an approach is probed, as `DeckPlates::approach_steps`.
const APPROACH_PROBE_M: f32 = 3.0;

fn get(url: &str) -> Option<Vec<u8>> {
    let r = ureq::get(url).call().ok()?;
    let mut v = Vec::new();
    r.into_reader().read_to_end(&mut v).ok()?;
    Some(v)
}

/// The ground from atlas's graded z14 tiles over the loaded cells — the tiles the
/// client samples where atlas has graded (`terrain_tiles`), so the approaches are
/// already ramped to the decks' measured tops. Heights are relative to their mean
/// (`Field::datum` carries it); a cell atlas did not grade is left at the datum.
pub fn graded_field(loaded: &Loaded, atlas: &str) -> Option<Field> {
    let first = loaded.cells.first()?;
    let (x0, x1) = (
        loaded.cells.iter().map(|c| c.x).min()?,
        loaded.cells.iter().map(|c| c.x).max()?,
    );
    let (y0, y1) = (
        loaded.cells.iter().map(|c| c.y).min()?,
        loaded.cells.iter().map(|c| c.y).max()?,
    );
    let grid = cartopolis_geo::terrain_tile::DENSE_GRID;
    let per = grid - 1;
    let n = (x1 - x0 + 1).max(y1 - y0 + 1) as usize * per + 1;
    let mut samples = vec![f32::NAN; n * n];
    let mut got = 0;
    for c in &loaded.cells {
        let url = format!(
            "{atlas}/terrain/{}/{}/{}?grid={grid}&graded=1",
            c.z, c.x, c.y
        );
        let Some(tile) = get(&url).and_then(|b| TerrainTile::decode(&b).ok()) else {
            eprintln!("atlas: no graded tile for {}/{}/{}", c.z, c.x, c.y);
            continue;
        };
        got += 1;
        let (ox, oz) = ((c.x - x0) as usize * per, (c.y - y0) as usize * per);
        for row in 0..grid {
            for col in 0..grid {
                samples[(oz + row) * n + ox + col] = tile.at(col, row);
            }
        }
    }
    if got == 0 {
        return None;
    }
    let finite: Vec<f32> = samples.iter().copied().filter(|v| v.is_finite()).collect();
    #[allow(clippy::cast_precision_loss)]
    let datum = finite.iter().sum::<f32>() / finite.len().max(1) as f32;
    for v in &mut samples {
        *v = if v.is_finite() { *v - datum } else { 0.0 };
    }
    #[allow(clippy::cast_precision_loss)]
    let origin = [
        (x0 - loaded.origin.0) as f32 * first.tile_m,
        (y0 - loaded.origin.1) as f32 * first.tile_m,
    ];
    #[allow(clippy::cast_precision_loss)]
    let step_m = first.tile_m / per as f32;
    eprintln!("atlas: {got} graded tiles, datum {datum:.2} m NAP, {n}² at {step_m:.2} m");
    Some(Field {
        samples,
        step_m,
        n,
        origin,
        datum,
    })
}

/// Atlas's graded tiles by cell, for reading the ground at a point **in that
/// cell's own frame**. The field is one grid at the first cell's metres while
/// the cells are placed at their own, which differ by the cosine of a
/// kilometre of latitude — about 1.4 m a cell row over Groningen, eight metres
/// six columns out. A probe read off the field there landed beside the
/// embankment it was measuring (a false 3.9 m on the eastern ring,
/// 2026-09-24).
#[derive(Default)]
pub struct GradedTiles(std::collections::HashMap<(i64, i64), TerrainTile>);

impl GradedTiles {
    pub fn fetch(loaded: &Loaded, atlas: &str) -> Self {
        let grid = cartopolis_geo::terrain_tile::DENSE_GRID;
        let mut out = std::collections::HashMap::new();
        for c in &loaded.cells {
            let url = format!(
                "{atlas}/terrain/{}/{}/{}?grid={grid}&graded=1",
                c.z, c.x, c.y
            );
            if let Some(t) = get(&url).and_then(|b| TerrainTile::decode(&b).ok()) {
                out.insert((c.x, c.y), t);
            }
        }
        Self(out)
    }

    /// Ground (NAP) at a tile-local point of `cell`, bilinear; a point past the
    /// cell's edge is read from the neighbour it falls in.
    pub fn at(&self, cell: &Cell, local: [f32; 2]) -> Option<f32> {
        let (fx, fy) = (local[0] / cell.tile_m, local[1] / cell.tile_m);
        let (dx, dy) = (fx.floor() as i64, fy.floor() as i64);
        let t = self.0.get(&(cell.x + dx, cell.y + dy))?;
        let last = (t.grid - 1) as f32;
        let (u, v) = ((fx - dx as f32) * last, (fy - dy as f32) * last);
        let (c0, r0) = (u.floor() as usize, v.floor() as usize);
        let (c1, r1) = ((c0 + 1).min(t.grid - 1), (r0 + 1).min(t.grid - 1));
        let (tu, tv) = (u - c0 as f32, v - r0 as f32);
        let top = t.at(c0, r0) + (t.at(c1, r0) - t.at(c0, r0)) * tu;
        let bottom = t.at(c0, r1) + (t.at(c1, r1) - t.at(c0, r1)) * tu;
        let h = top + (bottom - top) * tv;
        h.is_finite().then_some(h)
    }
}

/// What the client knows about one cell's bridges.
#[derive(Default)]
pub struct CellBridges {
    /// By the cell's deck index; `None` where atlas has no match — the client
    /// would measure that deck itself.
    pub decks: Vec<Option<DeckMeasure>>,
    /// Pier outer rings, tile-local.
    pub piers: Vec<Vec<[f32; 2]>>,
}

/// Atlas's decks (`decks/14/x/y.json`) matched to the cell's own by centre, as
/// `terrain_tiles::match_atlas_decks` does, and the BGT's piers
/// (`bgt/14/x/y.json`).
pub fn cell_bridges(site: &str, cell: &Cell) -> CellBridges {
    let tile_m = cell.tile_m;
    let mut out = CellBridges {
        decks: vec![None; cell.decks.len()],
        piers: Vec::new(),
    };
    let json = |path: String| -> Option<serde_json::Value> {
        serde_json::from_slice(&get(&format!("{site}/{path}"))?).ok()
    };
    if let Some(published) = get(&format!(
        "{site}/decks/{}/{}/{}.json",
        cell.z, cell.x, cell.y
    ))
    .and_then(|b| cartopolis_geo::formats::decks::parse_slice(&b).ok())
    .map(|c| c.decks)
    {
        let tol = 1.0 / tile_m;
        for (i, deck) in cell.decks.iter().enumerate() {
            let c = [
                (deck.min[0] + deck.max[0]) * 0.5 / tile_m,
                (deck.min[1] + deck.max[1]) * 0.5 / tile_m,
            ];
            out.decks[i] = published
                .iter()
                .find(|d| {
                    (d.at[0] as f32 - c[0]).abs() < tol && (d.at[1] as f32 - c[1]).abs() < tol
                })
                .map(|d| DeckMeasure {
                    height: d.height as f32,
                    top: d.top.map(|t| t as f32),
                    profile: d
                        .profile
                        .as_deref()
                        .and_then(|w| DeckProfile::from_wire(w, f64::from(tile_m))),
                });
        }
    }
    if let Some(v) = json(format!("bgt/{}/{}/{}.json", cell.z, cell.x, cell.y)) {
        for b in v["bridges"].as_array().into_iter().flatten() {
            // `[part, level, movable, [ring, …]]`, part 1 a pier.
            if b.get(0).and_then(serde_json::Value::as_u64) != Some(1) {
                continue;
            }
            let Some(ring) = b.get(3).and_then(|r| r.get(0)).and_then(|r| r.as_array()) else {
                continue;
            };
            let pts: Vec<[f32; 2]> = ring
                .iter()
                .filter_map(|p| {
                    let (lat, lng) = (p.get(0)?.as_f64()?, p.get(1)?.as_f64()?);
                    let (fx, fy) = cartopolis_core::world::lat_lng_to_tile_frac(lat, lng, cell.z);
                    Some([
                        ((fx - cell.x as f64) * f64::from(tile_m)) as f32,
                        ((fy - cell.y as f64) * f64::from(tile_m)) as f32,
                    ])
                })
                .collect();
            if pts.len() >= 3 {
                out.piers.push(pts);
            }
        }
    }
    out
}

/// A measured deck's top (NAP) over a tile-local point, where it has one.
pub fn top_at(m: &DeckMeasure, local: [f32; 2]) -> Option<f32> {
    if m.height <= 0.0 {
        return None;
    }
    match &m.profile {
        Some(p) => Some(p.at([f64::from(local[0]), f64::from(local[1])])),
        None => m.top,
    }
}

/// The point `d` metres along a polyline, clamped to its ends.
fn point_along(run: &[[f32; 2]], mut d: f32) -> [f32; 2] {
    for w in run.windows(2) {
        let len = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
        if d <= len && len > 0.0 {
            let t = d / len;
            return [
                w[0][0] + (w[1][0] - w[0][0]) * t,
                w[0][1] + (w[1][1] - w[0][1]) * t,
            ];
        }
        d -= len;
    }
    run.last().copied().unwrap_or([0.0, 0.0])
}

/// How well the measured decks meet their approaches — the client's
/// `approach_steps`: for each approach of a lifted deck, the deck's top where the
/// approach leaves it against the ground three metres further out. Returns
/// `(decks lifted, decks profiled, steps, max, mean)` and prints each step.
pub fn report(
    loaded: &Loaded,
    bridges: &[CellBridges],
    field: &Field,
    graded: &GradedTiles,
) -> (usize, usize, usize, f32, f32) {
    let (mut lifted, mut profiled, mut steps, mut max, mut sum) = (0, 0, 0, 0.0f32, 0.0f32);
    for (cell, b) in loaded.cells.iter().zip(bridges) {
        #[allow(clippy::cast_precision_loss)]
        let origin = [
            (cell.x - loaded.origin.0) as f32 * cell.tile_m,
            (cell.y - loaded.origin.1) as f32 * cell.tile_m,
        ];
        for (i, deck) in cell.decks.iter().enumerate() {
            let Some(m) = b.decks.get(i).and_then(Option::as_ref) else {
                continue;
            };
            if m.height <= 0.0 {
                continue;
            }
            lifted += 1;
            profiled += usize::from(m.profile.is_some());
            let runs = cell
                .approaches
                .iter()
                .filter(|a: &&Approach| a.deck as usize == i);
            for a in runs {
                let len: f32 = a
                    .points
                    .windows(2)
                    .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
                    .sum();
                if len < 1.0 {
                    continue;
                }
                let mut clear = 0.0f32;
                while clear < len && deck.contains(point_along(&a.points, clear)) {
                    clear += 0.5;
                }
                // A run that never leaves the deck — a slip road inside a wide
                // viaduct's outline, cut by the tile's edge — has no ground end
                // to measure: probed at its end it reads the ground *under* the
                // deck (a false 6.8 m at Laan 1940–1945, 2026-09-24).
                let out = point_along(&a.points, clear + APPROACH_PROBE_M);
                if clear >= len || deck.contains(out) {
                    continue;
                }
                let edge = point_along(&a.points, clear);
                let Some(top) = top_at(m, edge) else {
                    continue;
                };
                // The surface drawn there: another deck's top where the approach
                // runs straight onto one — an interchange's ramps chain decks —
                // and the ground otherwise. The ground *under* the next deck is
                // not where the road is, and reads as a cliff that is not there.
                // In any loaded cell: a deck is cut at its tile's edge, so the
                // structure an approach runs onto is as often the neighbour's.
                let world = [origin[0] + out[0], origin[1] + out[1]];
                let onto = loaded
                    .cells
                    .iter()
                    .zip(bridges)
                    .flat_map(|(oc, ob)| {
                        #[allow(clippy::cast_precision_loss)]
                        let local = [
                            world[0] - (oc.x - loaded.origin.0) as f32 * oc.tile_m,
                            world[1] - (oc.y - loaded.origin.1) as f32 * oc.tile_m,
                        ];
                        let same = std::ptr::eq(oc, cell);
                        oc.decks
                            .iter()
                            .zip(&ob.decks)
                            .enumerate()
                            .filter(move |(j, (d, _))| !(same && *j == i) && d.contains(local))
                            .filter_map(move |(_, (_, m))| top_at(m.as_ref()?, local))
                    })
                    .min_by(|a, b| (a - top).abs().total_cmp(&(b - top).abs()));
                let ground = onto.unwrap_or_else(|| {
                    graded.at(cell, out).unwrap_or_else(|| {
                        field.at(origin[0] + out[0], origin[1] + out[1]) + field.datum
                    })
                });
                let step = (top - ground).abs();
                let (lat, lng) = cartopolis_core::world::tile_frac_to_lat_lng(
                    cell.x as f64 + f64::from(out[0] / cell.tile_m),
                    cell.y as f64 + f64::from(out[1] / cell.tile_m),
                    cell.z,
                );
                println!(
                    "  deck {}/{} #{i}: top at approach {top:.2} m NAP, ground 3 m out {ground:.2}, \
                     step {step:.2} m{} — probe at {lat:.6},{lng:.6}",
                    cell.x,
                    cell.y,
                    if m.profile.is_some() { " (profiled)" } else { "" }
                );
                steps += 1;
                max = max.max(step);
                sum += step;
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let mean = if steps > 0 { sum / steps as f32 } else { 0.0 };
    (lifted, profiled, steps, max, mean)
}

/// Pier walls from the ground to the underside of the lowest measured deck over
/// each pier's centre, in world `y` relative to the field's datum — the client's
/// `add_piers`. Positions are tile-local; returns `(positions, normals, indices,
/// piers stood)`.
pub fn pier_mesh(
    cell: &Cell,
    b: &CellBridges,
    field: &Field,
    origin: [f32; 2],
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>, usize) {
    let (mut pos, mut nor, mut idx, mut stood) = (Vec::new(), Vec::new(), Vec::new(), 0);
    for ring in &b.piers {
        #[allow(clippy::cast_precision_loss)]
        let n = ring.len() as f32;
        let c = [
            ring.iter().map(|p| p[0]).sum::<f32>() / n,
            ring.iter().map(|p| p[1]).sum::<f32>() / n,
        ];
        let top = cell
            .decks
            .iter()
            .zip(&b.decks)
            .filter(|(d, _)| d.contains(c))
            .filter_map(|(_, m)| top_at(m.as_ref()?, c))
            .reduce(f32::min);
        let Some(top) = top else {
            continue;
        };
        let y1 = top - field.datum - DECK_THICKNESS_M;
        let y0 = ring
            .iter()
            .map(|p| field.at(origin[0] + p[0], origin[1] + p[1]))
            .fold(f32::INFINITY, f32::min)
            - PIER_FOOTING_M;
        if y1 - y0 < 0.5 {
            continue;
        }
        for k in 0..ring.len() {
            let (a, bb) = (ring[k], ring[(k + 1) % ring.len()]);
            let (dx, dz) = (bb[0] - a[0], bb[1] - a[1]);
            let len = dx.hypot(dz);
            if len < 1e-3 {
                continue;
            }
            let (mut nx, mut nz) = (-dz / len, dx / len);
            let mid = [(a[0] + bb[0]) * 0.5 - c[0], (a[1] + bb[1]) * 0.5 - c[1]];
            if nx * mid[0] + nz * mid[1] < 0.0 {
                (nx, nz) = (-nx, -nz);
            }
            #[allow(clippy::cast_possible_truncation)]
            let base = pos.len() as u32;
            for (p, y) in [(a, y1), (bb, y1), (bb, y0), (a, y0)] {
                pos.push([p[0], y, p[1]]);
                nor.push([nx, 0.0, nz]);
            }
            idx.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        stood += 1;
    }
    (pos, nor, idx, stood)
}

/// Stand a deck mesh (or a carriageway riding one) on its measured profile:
/// subdivided so a long deck's slope has vertices to follow, each vertex at the
/// top over it relative to the field's datum. `lift` is the flat lift the
/// tessellation already added, taken back off.
pub fn stand_on_deck(
    positions: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
    uvs: &mut Vec<[f32; 2]>,
    m: &DeckMeasure,
    datum: f32,
    lift: f32,
) {
    let (mut normals, mut colors, mut rungs) = (Vec::new(), Vec::new(), Vec::new());
    cartopolis_geo::terrain_refine::refine_for_terrain(
        positions,
        indices,
        uvs,
        &mut normals,
        &mut colors,
        &mut rungs,
        3.0,
        None,
    );
    for p in positions.iter_mut() {
        let top = top_at(m, [p[0], p[2]]).unwrap_or(datum);
        p[1] += top - datum - lift;
    }
}

/// Decks the harness knows nothing about beyond the tile: whether this deck
/// index has a measurement worth standing on.
pub fn measured(b: &CellBridges, deck: u32) -> Option<&DeckMeasure> {
    b.decks
        .get(deck as usize)
        .and_then(Option::as_ref)
        .filter(|m| m.height > 0.0)
}
