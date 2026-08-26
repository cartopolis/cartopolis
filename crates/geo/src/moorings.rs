//! Boats moored along the canal banks, from the water polygons already parsed.
//!
//! A Dutch city canal is lined with boats, and a canal with none reads as a
//! drainage ditch. This is the water's equivalent of `crate::parking`: the
//! polygon is surveyed, the boats on it are inferred, and the same rule applies —
//! be right about what the data says and claim nothing else.
//!
//! It costs no request and no extra decode. `map_geometry` already parses the
//! water polygons to tessellate the surface; this walks their **boundary** and
//! puts a boat every so often just inside it.
//!
//! Three things shape it:
//!
//! * **A canal is told from open water by its shape, not by a tag.** The tile has
//!   no width and no `waterway` value on a polygon; what it has is geometry, and
//!   a long thin shape has a small area for its perimeter while a lake or an
//!   estuary has a large one. `area / perimeter` is the mean half-width, and
//!   [`MAX_CANAL_HALF_WIDTH_M`] is where a canal stops being one. Mooring boats
//!   along the shore of the Wadden Sea would be the obvious failure here.
//! * **Which way is *into* the water depends on the ring's winding**, which MVT
//!   does not guarantee — so the winding is measured (signed area) rather than
//!   assumed. Get it backwards and every boat is moored on the pavement.
//! * **The tile border is not a bank.** A canal crossing a cell edge has a
//!   straight ring edge along the seam that is an artefact of the clip; boats
//!   laid along it would stand in a line down the middle of the water. Same guard
//!   the vegetation inset uses, and for the same reason.

use crate::furniture::{FurnitureInstance, FurnitureKind};
use crate::scatter::{hash3, rand01};
use crate::vector_tiles::{clipped_rings, palette, TileData, WATER_LAYERS};

/// Above this mean half-width a water polygon is open water, not a canal, and
/// gets no boats.
///
/// Measured as `area / perimeter`, which for a long thin shape *is* the half
/// width. A Groningen city canal runs 6–15 m across; a harbour basin or a lake is
/// far wider and would otherwise get a ring of boats round its whole shore.
const MAX_CANAL_HALF_WIDTH_M: f32 = 14.0;

/// Metres between moorings along a bank.
const MOORING_PITCH_M: f32 = 16.0;

/// How far inside the bank a hull sits, centre to bank.
const MOORING_INSET_M: f32 = 2.6;

/// Fraction of mooring slots left empty. A bank with a boat at every pitch is a
/// fence, not a canal.
const MOORING_VACANCY: f32 = 0.55;

/// A bank edge shorter than this carries nothing — it is a corner, not a quay.
const MIN_BANK_EDGE_M: f32 = 10.0;

/// Hard ceiling per streamed cell, on the same terms as
/// [`MAX_VEHICLES_PER_CELL`](crate::parking::MAX_VEHICLES_PER_CELL): the count
/// follows the length of bank in the cell rather than a survey, so it is the
/// thing that has to be bounded. Truncation is logged rather than silent.
pub const MAX_BOATS_PER_CELL: usize = 220;

/// Hull colours. Dutch canal boats are dark — near-black, bottle green, deep
/// blue, oxblood, varnished timber — which suits the palette here anyway: the
/// map is pale cartographic paper and a bright hull would be the loudest thing
/// on the water.
const BOAT_PAINTS: &[u32] = &[
    0x3E_45_47, // near-black slate
    0x3A_4C_43, // bottle green
    0x3B_49_5C, // deep blue
    0x5A_42_3C, // oxblood
    0x6B_5A_45, // varnished timber
    0x53_57_58, // grey
];

/// Read a tile's water polygons as moored boats.
///
/// `seed` must be stable for the cell, so a canal that streams out and back is
/// moored identically — a boat that moves when you look away is worse than no
/// boat, and `crate::scatter` documents why a reshuffle is instantly visible.
pub fn extract_moorings(
    tile: &TileData,
    tile_m: f32,
    seed: u64,
) -> Result<Vec<FurnitureInstance>, String> {
    use geo_types::Geometry;

    let mut out: Vec<FurnitureInstance> = Vec::new();
    let mut truncated = false;
    let mut feature_index = 0u64;

    for meta in tile.layers() {
        if !WATER_LAYERS.contains(&meta.name.as_str()) {
            continue;
        }
        let extent = TileData::extent(meta);
        let scale = tile_m / extent;

        for feature in tile.features(meta) {
            let polys: Vec<&geo_types::Polygon<f32>> = match &feature.geometry {
                Geometry::Polygon(p) => vec![p],
                Geometry::MultiPolygon(mp) => mp.iter().collect(),
                _ => continue,
            };
            for poly in polys {
                feature_index += 1;
                let rings = clipped_rings(poly, scale, extent);
                if rings.is_empty() {
                    continue;
                }
                // Open water gets nothing. `area / perimeter` is the mean half
                // width of a long thin shape, which is what a canal is and a
                // harbour basin is not.
                let (area, perimeter) = ring_measure(&rings);
                if perimeter <= 0.0 || area / perimeter > MAX_CANAL_HALF_WIDTH_M {
                    continue;
                }

                let fseed = seed ^ feature_index.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                for (ri, ring) in rings.iter().enumerate() {
                    // Which way is into the water. MVT does not guarantee a
                    // winding, so it is measured: a positive signed area means
                    // the ring runs one way and the interior is to one side of
                    // each edge, negative means the other.
                    let inward = if signed_area(ring) > 0.0 { 1.0 } else { -1.0 };
                    moor_along_ring(
                        ring,
                        inward,
                        tile_m,
                        fseed ^ (ri as u64) << 32,
                        &mut out,
                        &mut truncated,
                    );
                    if truncated {
                        break;
                    }
                }
            }
            if truncated {
                break;
            }
        }
        if truncated {
            break;
        }
    }

    if truncated {
        tracing::warn!(
            cap = MAX_BOATS_PER_CELL,
            "moorings hit the per-cell cap; this cell is moored short"
        );
    }
    Ok(out)
}

/// Lay boats along one ring's edges.
fn moor_along_ring(
    ring: &[[f32; 2]],
    inward: f32,
    tile_m: f32,
    seed: u64,
    out: &mut Vec<FurnitureInstance>,
    truncated: &mut bool,
) {
    for (ei, w) in ring.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        // The tile border is a clip artefact, not a bank. Boats laid along it
        // stand in a line down the middle of a canal that crosses a cell edge.
        if on_tile_border(a, b, tile_m) {
            continue;
        }
        let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dz * dz).sqrt();
        if len < MIN_BANK_EDGE_M {
            continue;
        }
        let (ux, uz) = (dx / len, dz / len);
        // Into the water, perpendicular to the bank, signed by the winding.
        //
        // For a ring whose signed area is positive the interior lies to the
        // **left** of each edge, and left of `(ux, uz)` is `(-uz, ux)`. This had
        // the sign the other way round first and moored every boat on the quay —
        // which is why `boats_moor_inside_the_water_whichever_way_the_ring_winds`
        // runs the same ring in both directions rather than trusting one.
        let (nx, nz) = (-uz * inward, ux * inward);

        // Start half a pitch in, so a boat is never exactly on a corner.
        let mut s = MOORING_PITCH_M * 0.5;
        let mut slot = 0i32;
        while s < len {
            let h = hash3(seed, ei as i32, slot);
            slot += 1;
            let at = s;
            s += MOORING_PITCH_M;
            if rand01(h, 0) < MOORING_VACANCY {
                continue;
            }
            let x = a[0] + ux * at + nx * MOORING_INSET_M;
            let z = a[1] + uz * at + nz * MOORING_INSET_M;
            // A hull lies *along* the bank, and the form's +z is its bow, so the
            // yaw is measured from +z the same way `push_box` reads it.
            let yaw = ux.atan2(uz);
            out.push(FurnitureInstance {
                x,
                z,
                yaw,
                kind: FurnitureKind::Boat,
                luma: 1.0 + (rand01(h, 1) - 0.5) * 0.10,
                paint: BOAT_PAINTS[((rand01(h, 2) * BOAT_PAINTS.len() as f32) as usize)
                    .min(BOAT_PAINTS.len() - 1)],
            });
            if out.len() >= MAX_BOATS_PER_CELL {
                *truncated = true;
                return;
            }
        }
    }
}

/// Signed area of a closed ring. Sign is the winding; magnitude is the area.
fn signed_area(ring: &[[f32; 2]]) -> f32 {
    ring.windows(2)
        .map(|w| w[0][0] * w[1][1] - w[1][0] * w[0][1])
        .sum::<f32>()
        * 0.5
}

/// Total unsigned area and perimeter of a polygon's rings.
///
/// Interiors subtract from the area (they are holes) and add to the perimeter,
/// which is what makes the ratio the shape's real mean half-width rather than its
/// outline's.
fn ring_measure(rings: &[Vec<[f32; 2]>]) -> (f32, f32) {
    let mut area = 0.0;
    let mut perimeter = 0.0;
    for (i, ring) in rings.iter().enumerate() {
        let a = signed_area(ring).abs();
        area += if i == 0 { a } else { -a };
        perimeter += ring
            .windows(2)
            .map(|w| {
                let (dx, dz) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
                (dx * dx + dz * dz).sqrt()
            })
            .sum::<f32>();
    }
    (area.max(0.0), perimeter)
}

/// True if the whole edge lies along one of the four tile edges.
///
/// The same test [`BorderGuard`](crate::scatter) makes, restated here rather than
/// shared because that one is private to the scatter's inset and this one is
/// about a bank rather than about a canopy.
fn on_tile_border(a: [f32; 2], b: [f32; 2], tile_m: f32) -> bool {
    const EPS: f32 = 0.5;
    let on = |v: f32, at: f32| (v - at).abs() < EPS;
    (on(a[0], 0.0) && on(b[0], 0.0))
        || (on(a[0], tile_m) && on(b[0], tile_m))
        || (on(a[1], 0.0) && on(b[1], 0.0))
        || (on(a[1], tile_m) && on(b[1], tile_m))
}

/// The no-tint value, for a form with no `Paint` part. Kept so the module reads
/// the same as its siblings.
#[allow(dead_code)]
const NO_PAINT: u32 = palette::FURNITURE;

#[cfg(test)]
mod tests {
    use super::*;

    const GRONINGEN_Z14: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../geo/tests/fixtures/groningen_z14.mvt"
    ));
    const TILE_M: f32 = 1472.0;

    fn fixture() -> TileData {
        TileData::parse(GRONINGEN_Z14).expect("parse fixture")
    }

    /// Groningen is a canal city, so the fixture must moor boats — inside its own
    /// cell, under the cap, and every one of them a boat.
    #[test]
    fn the_fixture_moors_boats_on_its_canals() {
        let boats = extract_moorings(&fixture(), TILE_M, 5).expect("extract");
        assert!(!boats.is_empty(), "a canal city should have boats");
        assert!(boats.len() <= MAX_BOATS_PER_CELL);
        for b in &boats {
            assert_eq!(b.kind, FurnitureKind::Boat);
            assert!(BOAT_PAINTS.contains(&b.paint));
        }
    }

    /// The whole point of the shape test: a wide body of water is not a canal and
    /// must not be lined with boats.
    #[test]
    fn open_water_gets_no_boats() {
        // A 400 m square lake: area/perimeter = 100 m, far past the threshold.
        let lake = vec![vec![
            [0.0, 0.0],
            [400.0, 0.0],
            [400.0, 400.0],
            [0.0, 400.0],
            [0.0, 0.0],
        ]];
        let (area, perimeter) = ring_measure(&lake);
        assert!(
            area / perimeter > MAX_CANAL_HALF_WIDTH_M,
            "a lake must read as open water: {}",
            area / perimeter
        );

        // A 400 × 12 m canal: area/perimeter ≈ 5.8 m, comfortably a canal.
        let canal = vec![vec![
            [0.0, 0.0],
            [400.0, 0.0],
            [400.0, 12.0],
            [0.0, 12.0],
            [0.0, 0.0],
        ]];
        let (area, perimeter) = ring_measure(&canal);
        assert!(
            area / perimeter < MAX_CANAL_HALF_WIDTH_M,
            "a canal must read as a canal: {}",
            area / perimeter
        );
    }

    /// Boats moor *in* the water, on whichever side of the bank that is. The
    /// winding is measured rather than assumed, so a ring given either way round
    /// must put its boats inside — get this backwards and they sit on the quay.
    #[test]
    fn boats_moor_inside_the_water_whichever_way_the_ring_winds() {
        let ccw = vec![
            [0.0, 0.0],
            [200.0, 0.0],
            [200.0, 16.0],
            [0.0, 16.0],
            [0.0, 0.0],
        ];
        let cw: Vec<[f32; 2]> = ccw.iter().rev().copied().collect();

        for ring in [ccw, cw] {
            let inward = if signed_area(&ring) > 0.0 { 1.0 } else { -1.0 };
            let mut out = Vec::new();
            let mut trunc = false;
            // A tile far larger than the ring, so nothing is taken for a border.
            moor_along_ring(&ring, inward, 5000.0, 99, &mut out, &mut trunc);
            assert!(!out.is_empty(), "no boats laid at all");
            for b in &out {
                assert!(
                    b.z > 0.0 && b.z < 16.0 && b.x > 0.0 && b.x < 200.0,
                    "boat at ({}, {}) is outside the water",
                    b.x,
                    b.z
                );
            }
        }
    }

    /// A canal crossing a cell edge has a straight ring edge along the seam that
    /// is a clip artefact. Boats laid along it run down the middle of the water.
    #[test]
    fn the_tile_seam_is_not_a_bank() {
        let tile_m = 100.0;
        // An edge lying exactly on the cell's east border.
        assert!(on_tile_border([100.0, 10.0], [100.0, 90.0], tile_m));
        assert!(on_tile_border([10.0, 0.0], [90.0, 0.0], tile_m));
        // A real bank a few metres inside it is not.
        assert!(!on_tile_border([96.0, 10.0], [96.0, 90.0], tile_m));
    }

    /// Same determinism contract every placed layer here holds.
    #[test]
    fn mooring_is_deterministic() {
        let a = extract_moorings(&fixture(), TILE_M, 3).expect("extract");
        let b = extract_moorings(&fixture(), TILE_M, 3).expect("extract");
        assert_eq!(a.len(), b.len());
        for (p, q) in a.iter().zip(&b) {
            assert_eq!((p.x, p.z, p.paint), (q.x, q.z, q.paint));
        }
    }
}
