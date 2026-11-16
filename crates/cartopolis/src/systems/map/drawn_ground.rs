//! The ground as it is drawn, for whatever stands on it.
//!
//! The ground sheets are the measured field meshed at up to 12.5 m edges,
//! refined where the field creases, laid on the streets' solved levels, on
//! the landings off bridge ends and on the lower quays
//! (`map_geometry::stand_on_terrain`). No formula over the field reproduces
//! that surface: between its vertices a sheet is a chord, and on the
//! Noorderplantsoen rampart the chord stood 21 cm off the field it was cut
//! from (2026-11-16). So each cell's worker, once it has stood the sheets,
//! samples the triangles it is about to hand to the GPU onto a one-metre
//! raster ([`DrawnGround::rasterize`]), and [`TerrainField::ground_at`] reads
//! that before anything else: the avatar, its feet, the collision, the cars
//! and every pin stand on exactly what is drawn (`docs/notes/foot-placement.md`).
//!
//! [`TerrainField::ground_at`]: crate::systems::terrain::TerrainField::ground_at

use std::collections::HashMap;

use bevy::math::Vec2;
use cartopolis_geo::vector_tiles::{SurfaceGroup, SurfaceMesh};

/// Raster spacing, metres. A sheet is a chord between vertices at least
/// 1.56 m apart (`MIN_GROUND_EDGE_M`), so a metre lands every triangle that
/// matters and keeps a 1.5 km cell to at most 4.3 MB.
pub const STEP_M: f32 = 1.0;

/// Samples along a stored tile's edge: tiles with no sheet over them are
/// not stored (under buildings, over water).
const TILE: usize = 64;

/// A sample with no sheet over it.
const EMPTY: i16 = i16::MIN;

/// Heights are stored in centimetres.
const CM: f32 = 100.0;

/// The groups a body stands on: the flat ground sheets. Not the water (a
/// body in it swims), the decks (`walk_deck_top`), the tunnels' own roads,
/// and nothing with volume.
#[must_use]
pub fn stood_on(group: SurfaceGroup) -> bool {
    matches!(
        group,
        SurfaceGroup::Forest
            | SurfaceGroup::Grass
            | SurfaceGroup::Sand
            | SurfaceGroup::Built
            | SurfaceGroup::Road(_)
            | SurfaceGroup::RoadCasing(_)
            | SurfaceGroup::Paving
            | SurfaceGroup::Crop
    )
}

/// One cell's drawn ground on a [`STEP_M`] raster, in the cell's own frame
/// (sample `(i, j)` at `(i, j) * STEP_M` from its corner), heights in the
/// field's frame as the sheets were drawn.
#[derive(Clone, Debug, Default)]
pub struct DrawnGround {
    /// Samples per side of the cell.
    n: usize,
    tiles: HashMap<(u16, u16), Box<[i16]>>,
}

impl DrawnGround {
    /// Sample the stood-on sheets of a cell `size` metres across, positions
    /// in the cell's frame with heights already stood on the ground. Where
    /// sheets overlap the higher one is the ground.
    #[must_use]
    pub fn rasterize(meshes: &[SurfaceMesh], size: f32) -> Self {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = ((size / STEP_M).ceil() as usize + 1).min(usize::from(u16::MAX));
        let mut out = Self {
            n,
            tiles: HashMap::new(),
        };
        for m in meshes.iter().filter(|m| stood_on(m.group)) {
            for t in m.indices.chunks_exact(3) {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.positions[i as usize]);
                out.triangle(a, b, c);
            }
        }
        out
    }

    fn triangle(&mut self, a: [f32; 3], b: [f32; 3], c: [f32; 3]) {
        let det = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
        if det.abs() < 1e-9 {
            return;
        }
        let lo = |k: usize| a[k].min(b[k]).min(c[k]) / STEP_M;
        let hi = |k: usize| a[k].max(b[k]).max(c[k]) / STEP_M;
        #[allow(clippy::cast_possible_truncation)]
        let (i0, i1, j0, j1) = (
            lo(0).ceil().max(0.0) as i64,
            hi(0).floor() as i64,
            lo(2).ceil().max(0.0) as i64,
            hi(2).floor() as i64,
        );
        #[allow(clippy::cast_possible_wrap)]
        let last = self.n as i64 - 1;
        for j in j0..=j1.min(last) {
            for i in i0..=i1.min(last) {
                #[allow(clippy::cast_precision_loss)]
                let (x, z) = (i as f32 * STEP_M, j as f32 * STEP_M);
                let l1 = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / det;
                let l2 = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / det;
                let l3 = 1.0 - l1 - l2;
                if l1 < -1e-5 || l2 < -1e-5 || l3 < -1e-5 {
                    continue;
                }
                let y = l1 * a[1] + l2 * b[1] + l3 * c[1];
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                self.raise(i as usize, j as usize, y);
            }
        }
    }

    fn raise(&mut self, i: usize, j: usize, y: f32) {
        #[allow(clippy::cast_possible_truncation)]
        let v = (y * CM)
            .round()
            .clamp(f32::from(i16::MIN + 1), f32::from(i16::MAX)) as i16;
        #[allow(clippy::cast_possible_truncation)]
        let key = ((i / TILE) as u16, (j / TILE) as u16);
        let tile = self
            .tiles
            .entry(key)
            .or_insert_with(|| vec![EMPTY; TILE * TILE].into_boxed_slice());
        let s = &mut tile[(j % TILE) * TILE + i % TILE];
        if *s == EMPTY || v > *s {
            *s = v;
        }
    }

    fn sample(&self, i: usize, j: usize) -> Option<f32> {
        if i >= self.n || j >= self.n {
            return None;
        }
        #[allow(clippy::cast_possible_truncation)]
        let tile = self.tiles.get(&((i / TILE) as u16, (j / TILE) as u16))?;
        let v = tile[(j % TILE) * TILE + i % TILE];
        (v != EMPTY).then(|| f32::from(v) / CM)
    }

    /// The drawn ground at a point in the cell's frame: bilinear between the
    /// four samples round it, over those a sheet covers; `None` where none
    /// does.
    #[must_use]
    pub fn height_at(&self, x: f32, z: f32) -> Option<f32> {
        let (fx, fz) = (x / STEP_M, z / STEP_M);
        if fx < 0.0 || fz < 0.0 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (i, j) = (fx.floor() as usize, fz.floor() as usize);
        #[allow(clippy::cast_precision_loss)]
        let (tx, tz) = (fx - i as f32, fz - j as f32);
        let (mut sum, mut weight) = (0.0f32, 0.0f32);
        for (di, dj, w) in [
            (0, 0, (1.0 - tx) * (1.0 - tz)),
            (1, 0, tx * (1.0 - tz)),
            (0, 1, (1.0 - tx) * tz),
            (1, 1, tx * tz),
        ] {
            if let Some(y) = self.sample(i + di, j + dj) {
                sum += y * w;
                weight += w;
            }
        }
        (weight > 1e-4).then(|| sum / weight)
    }

    /// Bytes held, for the memory ledger and the note.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.tiles.len() * TILE * TILE * std::mem::size_of::<i16>()
    }
}

type Cell = (i64, i64);

/// Every built cell's drawn ground, with the cell's world origin. Set by the
/// cell's worker with the sheets it stood (`map_geometry`), dropped with the
/// cell, as the walkable decks and landings beside it are.
static WALK_SURFACE: std::sync::RwLock<Option<HashMap<Cell, (Vec2, DrawnGround)>>> =
    std::sync::RwLock::new(None);

/// Bumped whenever a cell's drawn ground comes or goes, for whatever
/// reports `ground_at` (`ground_probe`).
static GENERATION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// How many times the drawn ground has changed.
#[must_use]
pub fn generation() -> u32 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// Replace one cell's drawn ground, or drop it with `None`.
pub fn set_cell(cell: Cell, ground: Option<(Vec2, DrawnGround)>) {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut w) = WALK_SURFACE.write() {
        let map = w.get_or_insert_with(HashMap::new);
        match ground {
            Some(g) => map.insert(cell, g),
            None => map.remove(&cell),
        };
    }
}

/// Drop every cell's (a re-anchor: the heights are in the old datum).
pub fn clear() {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut w) = WALK_SURFACE.write() {
        *w = None;
    }
}

/// The drawn ground at a world point, if a built cell has a sheet there.
#[must_use]
pub fn height_at(wx: f32, wz: f32) -> Option<f32> {
    let w = WALK_SURFACE.read().ok()?;
    w.as_ref()?.values().find_map(|(origin, g)| {
        #[allow(clippy::cast_precision_loss)]
        let size = (g.n.saturating_sub(1)) as f32 * STEP_M;
        let (x, z) = (wx - origin.x, wz - origin.y);
        ((0.0..=size).contains(&x) && (0.0..=size).contains(&z))
            .then(|| g.height_at(x, z))
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sheet over a rampart: flat at 0 for 10 m, a 34 % slope for 12 m,
    /// flat on top, cut into triangles the way the refinement leaves them —
    /// long chords where the field is straight, short ones at the creases —
    /// and stood at the field's height at each vertex only.
    /// The creases fall between raster samples, as they do on real ground.
    fn rampart(x: f32) -> f32 {
        (0.34 * (x - 10.4)).clamp(0.0, 4.08)
    }

    fn sheet() -> SurfaceMesh {
        let xs = [
            0.0, 6.25, 9.0, 10.4, 11.96, 16.0, 21.0, 22.4, 23.96, 30.0, 40.0,
        ];
        let zs = [0.0, 12.5, 25.0, 37.5, 40.0];
        let mut positions = Vec::new();
        for &z in &zs {
            for &x in &xs {
                positions.push([x, rampart(x), z]);
            }
        }
        let w = xs.len() as u32;
        let mut indices = Vec::new();
        for j in 0..zs.len() as u32 - 1 {
            for i in 0..w - 1 {
                let (a, b, c, d) = (
                    j * w + i,
                    j * w + i + 1,
                    (j + 1) * w + i,
                    (j + 1) * w + i + 1,
                );
                indices.extend([a, c, b, b, c, d]);
            }
        }
        SurfaceMesh {
            group: SurfaceGroup::Grass,
            positions,
            indices,
            uvs: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            rungs: Vec::new(),
        }
    }

    #[test]
    fn the_raster_is_the_drawn_sheet_within_two_centimetres() {
        let sheet = sheet();
        let g = DrawnGround::rasterize(std::slice::from_ref(&sheet), 40.0);
        let (mut worst, mut worst_crease) = (0.0f32, 0.0f32);
        for k in 0..1600 {
            // Points off the raster's own lattice, all over the sheet.
            #[allow(clippy::cast_precision_loss)]
            let (x, z) = (
                0.37 + (k % 40) as f32 * 0.977,
                0.61 + (k / 40) as f32 * 0.963,
            );
            let drawn =
                crate::systems::ground_probe::height_at(&sheet.positions, &sheet.indices, x, z)
                    .expect("on the sheet");
            let off = (g.height_at(x, z).expect("covered") - drawn).abs();
            // Within a sample of a crease the raster cuts the corner: at most
            // the change of slope times a quarter of a sample.
            if (x - 10.4).abs() < STEP_M || (x - 22.4).abs() < STEP_M {
                worst_crease = worst_crease.max(off);
            } else {
                worst = worst.max(off);
            }
        }
        assert!(worst <= 0.02, "worst {worst} m");
        assert!(
            worst_crease <= 0.34 * STEP_M / 4.0 + 0.005,
            "{worst_crease} m"
        );
    }

    #[test]
    fn nothing_is_stood_on_where_no_sheet_is_drawn_or_on_water() {
        let mut water = sheet();
        water.group = SurfaceGroup::Water;
        let g = DrawnGround::rasterize(&[water], 40.0);
        assert_eq!(g.height_at(5.0, 5.0), None);
        assert_eq!(g.bytes(), 0);
        let g = DrawnGround::rasterize(&[sheet()], 80.0);
        assert!(g.height_at(5.0, 5.0).is_some());
        assert_eq!(g.height_at(60.0, 60.0), None, "past the sheet");
    }

    #[test]
    fn the_higher_of_two_sheets_is_the_ground() {
        let low = sheet();
        let mut high = sheet();
        high.group = SurfaceGroup::Paving;
        for p in &mut high.positions {
            p[1] += 0.3;
        }
        let g = DrawnGround::rasterize(&[low, high], 40.0);
        let y = g.height_at(3.3, 3.3).unwrap();
        assert!((y - 0.3).abs() < 0.01, "{y}");
    }

    #[test]
    fn what_stands_on_the_ground_stands_on_the_drawn_sheet() {
        // `TerrainField::ground_at` — the avatar's, its feet's, the
        // collision's — over a built cell is the sheet itself, chords and
        // all, not the field it was cut from.
        let sheet = sheet();
        let origin = Vec2::new(-3000.0, 7000.0);
        set_cell(
            (900_002, 9),
            Some((
                origin,
                DrawnGround::rasterize(std::slice::from_ref(&sheet), 40.0),
            )),
        );
        let terrain = crate::systems::terrain::TerrainField::default();
        for (x, z) in [(3.1, 5.2), (14.7, 20.3), (17.2, 33.9), (31.5, 8.8)] {
            let drawn =
                crate::systems::ground_probe::height_at(&sheet.positions, &sheet.indices, x, z)
                    .unwrap();
            let stood = terrain.ground_at(origin.x + x, origin.y + z);
            assert!((stood - drawn).abs() <= 0.02, "{x},{z}: {stood} vs {drawn}");
        }
        set_cell((900_002, 9), None);
    }

    #[test]
    fn a_world_point_finds_its_cell() {
        let g = DrawnGround::rasterize(&[sheet()], 40.0);
        set_cell((900_001, 3), Some((Vec2::new(1000.0, -500.0), g)));
        let y = height_at(1000.0 + 16.0, -500.0 + 5.0).unwrap();
        assert!((y - rampart(16.0)).abs() < 0.02, "{y}");
        assert_eq!(height_at(5000.0, 5000.0), None);
        set_cell((900_001, 3), None);
        assert_eq!(height_at(1016.0, -495.0), None);
    }
}
