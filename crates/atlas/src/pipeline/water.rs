//! Water surfaces at the height they actually stand at (`water_levels.rs` in the
//! client reads them). Ported from the earlier TypeScript pipeline (`water.ts`).
//!
//! The ground under water is no-data in AHN and the banks are dilated inward to
//! fill it, so without this a canal is drawn level with the quay beside it. The 3D
//! basisvoorziening's `Waterdeel_vlak` objects carry an AHN-draped z, one flat
//! level per body — a polygon carrying a level.
//!
//! **The boundary must be walked with directed edges.** Each body arrives as a
//! triangulated `MultiSurface`, so the outline is recovered, not read: an edge is
//! on the boundary exactly when its reverse is absent. Counting edges undirected
//! loses the winding, and where two parts of one body touch at a point the walk
//! chains through it into a self-crossing ring the client's even-odd test answers
//! nonsense for.
//!
//! Data: Kadaster 3D basisvoorziening — CC BY 4.0.

use std::collections::{BTreeMap, HashMap, HashSet};

use cartopolis_geo::formats::coverage::Manifest;
use cartopolis_geo::formats::water as wire;
use serde_json::{json, Value};

use super::bv3d::{for_each_block, CityJson};
use super::geom::{ring_area, round, simplify, Pt};
use super::rd::rd_to_lat_lng;
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{Count, Ctx, DatasetResult, Outcome};

const ATTRIBUTION: &str = "Water levels © Kadaster 3D basisvoorziening (CC BY 4.0)";
pub(super) const WATER_TABLE: &str = "Waterdeel_vlak";
/// Metres, in RD before projecting: the client uses these for a point-in-polygon
/// test and to carve the ground, neither of which can see a decimetre.
const SIMPLIFY_M: f64 = 0.25;
/// Below this a ring is a sliver between two parts of one body, not a pond.
const MIN_RING_M2: f64 = 4.0;

/// Recover the outlines of a triangulated surface soup, as open rings of vertex
/// indices.
///
/// The edge set keeps **first-insertion order**, and so does each vertex's list
/// of outgoing boundary edges: that order decides which way the walk turns where
/// two rings touch, and it is the order the TypeScript's `Map` kept.
#[must_use]
pub fn boundary_rings(boundaries: &Value) -> Vec<Vec<u64>> {
    let mut order: Vec<(u64, u64)> = Vec::new();
    let mut edges: HashSet<(u64, u64)> = HashSet::new();

    fn walk(node: &Value, order: &mut Vec<(u64, u64)>, edges: &mut HashSet<(u64, u64)>) {
        let Some(items) = node.as_array() else {
            return;
        };
        if items.iter().all(Value::is_number) {
            // A ring. Only whole-number indices form one; anything else is skipped
            // whole, as the TypeScript's `typeof v !== 'number'` guard did.
            let pts: Vec<u64> = items.iter().filter_map(Value::as_u64).collect();
            if pts.len() != items.len() {
                return;
            }
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                if a != b && edges.insert((a, b)) {
                    order.push((a, b));
                }
            }
            return;
        }
        for child in items {
            walk(child, order, edges);
        }
    }
    walk(boundaries, &mut order, &mut edges);

    // Boundary = an edge whose reverse is absent.
    let mut outgoing: BTreeMap<u64, std::collections::VecDeque<u64>> = BTreeMap::new();
    for &(a, b) in &order {
        if !edges.contains(&(b, a)) {
            outgoing.entry(a).or_default().push_back(b);
        }
    }
    let starts: Vec<u64> = outgoing.keys().copied().collect();
    let mut rings = Vec::new();
    for start in starts {
        while outgoing.get(&start).is_some_and(|l| !l.is_empty()) {
            let mut ring = Vec::new();
            let mut at = start;
            // Bounded by the edge count: every step consumes one edge.
            while let Some(next) = outgoing.get_mut(&at).and_then(|l| l.pop_front()) {
                ring.push(at);
                at = next;
                if at == start {
                    break;
                }
            }
            if ring.len() >= 3 {
                rings.push(ring);
            }
        }
    }
    rings
}

/// Every water ring in one block, as `(level NAP m, [(lat, lng), …])`.
#[must_use]
pub fn water_rings(cj: &CityJson) -> Vec<(f64, Vec<(f64, f64)>)> {
    let (scale, translate) = (cj.transform.scale, cj.transform.translate);
    let vertex = |i: u64| cj.vertices.get(i as usize);
    let mut out = Vec::new();
    for object in cj.city_objects.values() {
        if object
            .attributes
            .as_ref()
            .and_then(|a| a.tabelnaam.as_deref())
            != Some(WATER_TABLE)
        {
            continue;
        }
        for geometry in &object.geometry {
            let rings = boundary_rings(&geometry.boundaries);
            if rings.is_empty() {
                continue;
            }
            // One level per body, the median: a drape puts a handful of vertices
            // on the bank it was clipped against, and a mean drags the surface up.
            let mut zs: Vec<f64> = rings
                .iter()
                .flatten()
                .filter_map(|&i| vertex(i).map(|v| v[2] * scale[2] + translate[2]))
                .collect();
            if zs.is_empty() {
                continue;
            }
            zs.sort_by(f64::total_cmp);
            let level = zs[zs.len() / 2];
            for ring in &rings {
                // Simplified in RD, where the units are metres, then projected.
                let rd: Vec<Pt> = ring
                    .iter()
                    .filter_map(|&i| {
                        vertex(i).map(|v| {
                            [
                                v[0] * scale[0] + translate[0],
                                v[1] * scale[1] + translate[1],
                            ]
                        })
                    })
                    .collect();
                if rd.len() < 3 {
                    continue;
                }
                let kept = simplify(&rd, SIMPLIFY_M);
                if kept.len() < 3 || ring_area(&kept) < MIN_RING_M2 {
                    continue;
                }
                out.push((
                    level,
                    kept.iter().map(|&[x, y]| rd_to_lat_lng(x, y)).collect(),
                ));
            }
        }
    }
    out
}

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    let mut cells: HashMap<Cell, Vec<wire::Ring>> = HashMap::new();
    let (mut n_rings, mut n_placements) = (0usize, 0usize);

    for_each_block(ctx, "waterpeilen", water_rings, |rings| {
        for (level, ring) in rings {
            n_rings += 1;
            // Into **every** cell the ring's extent touches: the client tests each
            // sample against the rings it holds, so a canal filed under one end is
            // level with the quay for everybody standing at the other.
            let (mut min_lat, mut min_lng, mut max_lat, mut max_lng) = (
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            );
            for &(lat, lng) in &ring {
                min_lat = min_lat.min(lat);
                max_lat = max_lat.max(lat);
                min_lng = min_lng.min(lng);
                max_lng = max_lng.max(lng);
            }
            let (x0, y0) = lat_lng_to_cell(max_lat, min_lng);
            let (x1, y1) = lat_lng_to_cell(min_lat, max_lng);
            let flat: Vec<f64> = ring
                .iter()
                .flat_map(|&(lat, lng)| [round(lat, 7), round(lng, 7)])
                .collect();
            let entry: wire::Ring = (round(level, 2), flat);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    cells.entry((x, y)).or_default().push(entry.clone());
                    n_placements += 1;
                }
            }
        }
    })?;

    let written: Vec<(Cell, Vec<wire::Ring>)> =
        cells.into_iter().filter(|(_, r)| !r.is_empty()).collect();
    let covered = sorted_cells(written.iter().map(|(c, _)| *c));
    let n_written = written.len();
    for (cell, rings) in written {
        ctx.write_doc(&cell_path("water", cell), &wire::Cell { rings })?;
    }
    ctx.write_coverage(
        "water/coverage.json",
        &Manifest::new(CACHE_ZOOM, covered)
            .described(
                ctx.name.as_str(),
                ATTRIBUTION,
                "Kadaster 3D basisvoorziening (Waterdeel_vlak)",
                ctx.bbox,
            )
            .with("ring_count", n_rings),
    )?;
    ctx.log(format!(
        "{n_rings} waterringen, {n_placements} plaatsingen over {} cellen",
        n_written
    ));
    Ok(DatasetResult {
        cells: n_written,
        counts: vec![Count::new("water rings", n_rings)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two triangles sharing an edge are one square outline; the shared edge is
    /// interior because its reverse is present.
    #[test]
    fn a_triangulated_square_has_one_outline() {
        let soup = json!([[[0, 1, 2]], [[0, 2, 3]]]);
        let rings = boundary_rings(&soup);
        assert_eq!(rings.len(), 1);
        let mut r = rings[0].clone();
        r.sort_unstable();
        assert_eq!(r, vec![0, 1, 2, 3]);
    }

    /// Two squares touching at a single vertex stay two rings rather than one
    /// self-crossing ring through the shared point.
    #[test]
    fn bodies_touching_at_a_point_stay_separate() {
        // Square A: 0 1 2 3; square B: 2 4 5 6, touching A at vertex 2.
        let soup = json!([[[0, 1, 2]], [[0, 2, 3]], [[2, 4, 5]], [[2, 5, 6]]]);
        let rings = boundary_rings(&soup);
        let total: usize = rings.iter().map(Vec::len).sum();
        assert_eq!(total, 8, "{rings:?}");
        assert!(
            rings.iter().all(|r| r.len() == 4 || r.len() == 8),
            "{rings:?}"
        );
    }
}
