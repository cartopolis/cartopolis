//! The ground nothing else covers, as one plain sheet.
//!
//! Near the camera the ground is the vector sheets — landuse fills, surveyed
//! paving, roads, water, crops — and under them the basemap's relief tiles. The
//! two were never meant to be seen together: the tiles stood in wherever the
//! vector ground had a hole, at almost the same height. Once both were bent
//! onto the measured terrain, each against its own triangles, they disagreed
//! about where the ground is by more than the 3 cm between them, and the
//! basemap stood through the paving in bands that followed the terrain
//! (`CARTO_PROBE`, Vismarkt, 2026-10-01: 0.8 cm of clearance where the band
//! was). The fix is to stop drawing the basemap where a cell is built — and for
//! that the cell's own ground has to be complete, which is this: a grid over the
//! cell, cut out from under every other sheet, so what is left is exactly the
//! holes and overlaps nothing.

use crate::vector_tiles::{SurfaceGroup, SurfaceMesh};

/// Quad size of the grid the holes are cut out of. Small enough that one quad
/// overlaps a bounded number of blockers — the cut is per triangle — and large
/// enough that an empty cell is a few thousand triangles, not millions.
pub const GAP_QUAD_M: f32 = 24.0;

/// A plain sheet over the rectangle `min..max` (cell-local XZ) minus every flat
/// sheet in `sheets`, in the sRGB tone `rgb`. `None` when nothing is left.
#[must_use]
pub fn gap_fill(
    sheets: &[SurfaceMesh],
    min: [f32; 2],
    max: [f32; 2],
    rgb: u32,
) -> Option<SurfaceMesh> {
    let tint = crate::flat_fill::srgb_to_linear(rgb);
    let nx = ((max[0] - min[0]) / GAP_QUAD_M).ceil().max(1.0) as usize;
    let nz = ((max[1] - min[1]) / GAP_QUAD_M).ceil().max(1.0) as usize;
    let (sx, sz) = ((max[0] - min[0]) / nx as f32, (max[1] - min[1]) / nz as f32);
    let mut grid = SurfaceMesh {
        group: SurfaceGroup::Built,
        positions: Vec::with_capacity((nx + 1) * (nz + 1)),
        indices: Vec::with_capacity(nx * nz * 6),
        uvs: Vec::new(),
        normals: Vec::new(),
        colors: Vec::new(),
        rungs: Vec::new(),
    };
    for j in 0..=nz {
        for i in 0..=nx {
            grid.positions
                .push([min[0] + i as f32 * sx, 0.0, min[1] + j as f32 * sz]);
            grid.normals.push([0.0, 1.0, 0.0]);
            grid.colors.push(tint);
        }
    }
    let w = (nx + 1) as u32;
    for j in 0..nz as u32 {
        for i in 0..nx as u32 {
            let (a, b, c, d) = (
                j * w + i,
                j * w + i + 1,
                (j + 1) * w + i,
                (j + 1) * w + i + 1,
            );
            // Wound to face up, as every ground sheet is.
            grid.indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    let blockers: Vec<SurfaceMesh> = sheets
        .iter()
        .filter(|m| m.group.layer().is_sheet())
        .cloned()
        .collect();
    let mut out = vec![grid];
    crate::road_stitch::cut_under_roads(&mut out, &blockers);
    out.pop().filter(|m| !m.indices.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(m: &SurfaceMesh) -> f64 {
        m.indices
            .chunks_exact(3)
            .map(|t| {
                let p = |i: u32| m.positions[i as usize];
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                f64::from(((b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0])).abs())
                    / 2.0
            })
            .sum()
    }

    /// Faces up, and covers exactly the square less what the sheet covers.
    #[test]
    fn the_fill_is_the_square_less_the_sheets() {
        let sheet = crate::paving::build_paving_meshes(
            &[crate::paving::PavedArea {
                kind: crate::paving::PavingKind::Footway,
                material: None,
                rings: vec![vec![[10.0, 10.0], [60.0, 10.0], [60.0, 40.0], [10.0, 40.0]]],
                measured: None,
            }],
            None,
        );
        let fill = gap_fill(&sheet, [0.0, 0.0], [100.0, 100.0], 0xFF_FF_FF).unwrap();
        assert!(
            (area(&fill) - (10_000.0 - 1_500.0)).abs() < 1.0,
            "{}",
            area(&fill)
        );
        for t in fill.indices.chunks_exact(3) {
            let p = |i: u32| fill.positions[i as usize];
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
            assert!(cross < 0.0, "a fill triangle faces down");
        }
    }

    /// What a real cell costs: Groningen's centre, every surface the tile
    /// draws. `--ignored --nocapture` prints it.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn print_gap_fill_cost() {
        let tile = crate::testdata::groningen();
        let tile_m = 1464.5;
        let sheets = crate::vector_tiles::tessellate_surfaces(&tile, tile_m, None).unwrap();
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for m in &sheets {
            for p in &m.positions {
                lo = [lo[0].min(p[0]), lo[1].min(p[2])];
                hi = [hi[0].max(p[0]), hi[1].max(p[2])];
            }
        }
        let tris: usize = sheets.iter().map(|m| m.indices.len() / 3).sum();
        let t = std::time::Instant::now();
        let fill = gap_fill(&sheets, [0.0, 0.0], [tile_m, tile_m], 0xFF_FF_FF);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let (fa, ft) = fill
            .as_ref()
            .map_or((0.0, 0), |f| (area(f), f.indices.len() / 3));
        eprintln!(
            "sheets {} ({tris} tris), extent {lo:?}..{hi:?}; gap fill {ms:.1} ms, {ft} tris, {:.1} % of the cell",
            sheets.len(),
            fa / f64::from(tile_m * tile_m) * 100.0
        );
    }
}
