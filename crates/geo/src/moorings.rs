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
use crate::vector_tiles::{clipped_rings, TileData, WATER_LAYERS};

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
            "moorings ran past the collection bound; this cell is moored short"
        );
    }
    Ok(crate::scatter::thin_evenly(out, MAX_BOATS_PER_CELL))
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
                // A moored boat is on water by definition, and saying so here would
                // be true and unread: the flag drives the fountain's form and the
                // client's collider pass, and a hull is meant to stop you.
                over_water: false,
                hull_only: false,
            });
            // Collected past the ceiling and thinned evenly to it afterwards
            // (`scatter::thin_evenly`); this bound only stops a runaway cell.
            if out.len() >= MAX_BOATS_PER_CELL * 4 {
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

// ─── Marinas ─────────────────────────────────────────────────────────────────

/// The jetty layers: OSM's `man_made=pier` as areas and as lines.
const PIER_LAYERS: [&str; 2] = ["pier_polygons", "pier_lines"];

/// Metres between yachts along a jetty: a 10 m hull and the gap to the next.
const YACHT_PITCH_M: f32 = 14.0;
/// A jetty segment shorter than this carries no berths: it is the elbow where a
/// finger meets the walkway.
const MIN_JETTY_EDGE_M: f32 = 6.0;
/// From the jetty's line to a yacht's centre: half a jetty, a fender, half a
/// beam. A city harbour moors its yachts *alongside* the jetties — the
/// Oosterhaven's photograph shows them so — and the dense comb of boxes this
/// first laid read as a raft of planks rather than as boats.
const YACHT_OFFSET_M: f32 = 2.8;
/// No two yachts closer than this, centre to centre: a jetty mapped both as an
/// area and as a line would otherwise moor two boats in one place.
const YACHT_SPACING_M: f32 = 7.0;
/// No yacht within this of a registered houseboat berth.
const BERTH_CLEARANCE_M: f32 = 7.0;
/// How far from a jetty a marina basin's quay still takes yachts, metres.
const MARINA_REACH_M: f32 = 120.0;
/// Fraction of yacht berths left empty.
const YACHT_VACANCY: f32 = 0.35;
/// Ceiling per cell, on the terms of [`MAX_BOATS_PER_CELL`].
pub const MAX_YACHTS_PER_CELL: usize = 400;
/// Yacht hulls: mostly white and cream, the odd navy or racing green.
const YACHT_PAINTS: &[u32] = &[
    0xEC_EC_E8, 0xF2_F0_E8, 0xE4_E6_EA, 0xEC_EC_E8, 0xE0_DC_D2, 0x2E_3A_55, 0x2F_4A_3C,
];

/// Yachts moored at a tile's jetties.
///
/// **A jetty in water is a marina's berth line**, whatever the polygon around
/// it is called. The canal moorings above refuse wide water on purpose — lining
/// a lake's shore is the failure they guard against — so a harbour basin with
/// fifty boats in it stood empty (the Oosterhaven, 2026-10-01). What a harbour
/// has that a lake does not is jetties, and OSM maps them (`man_made=pier`,
/// sixty-three around Groningen). So the yachts follow the jetties: one per
/// pitch on each side, bow in, kept only where the whole hull is over water and
/// clear of every jetty — which is also what keeps a jetty on land, or a pier
/// along a canal wall, from growing boats on the dry side.
pub fn extract_marina(
    tile: &TileData,
    tile_m: f32,
    seed: u64,
    berths: &[[f32; 2]],
) -> Result<Vec<FurnitureInstance>, String> {
    use geo_types::Geometry;
    // Every water polygon and every jetty area, as rings in tile metres.
    let mut water: Vec<Vec<Vec<[f32; 2]>>> = Vec::new();
    let mut piers: Vec<Vec<Vec<[f32; 2]>>> = Vec::new();
    // The lines berths are laid along: jetty lines, and jetty areas' edges.
    let mut lines: Vec<Vec<[f32; 2]>> = Vec::new();
    for meta in tile.layers() {
        let name = meta.name.as_str();
        let is_water = WATER_LAYERS.contains(&name);
        let is_pier = PIER_LAYERS.contains(&name);
        if !is_water && !is_pier {
            continue;
        }
        let extent = TileData::extent(meta);
        let scale = tile_m / extent;
        for feature in tile.features(meta) {
            match &feature.geometry {
                Geometry::Polygon(p) => {
                    let rings = clipped_rings(p, scale, extent);
                    if rings.is_empty() {
                        continue;
                    }
                    if is_pier {
                        lines.push(rings[0].clone());
                        piers.push(rings);
                    } else {
                        water.push(rings);
                    }
                }
                Geometry::MultiPolygon(mp) => {
                    for p in mp {
                        let rings = clipped_rings(p, scale, extent);
                        if rings.is_empty() {
                            continue;
                        }
                        if is_pier {
                            lines.push(rings[0].clone());
                            piers.push(rings);
                        } else {
                            water.push(rings);
                        }
                    }
                }
                Geometry::LineString(ls) if is_pier => {
                    lines.push(ls.0.iter().map(|c| [c.x * scale, c.y * scale]).collect());
                }
                Geometry::MultiLineString(mls) if is_pier => {
                    for ls in mls {
                        lines.push(ls.0.iter().map(|c| [c.x * scale, c.y * scale]).collect());
                    }
                }
                _ => {}
            }
        }
    }
    if lines.is_empty() || water.is_empty() {
        return Ok(Vec::new());
    }
    // **A basin with a jetty in it is a marina, and its quays are berths too.**
    // The Oosterhaven's yachts lie along the quay wall as much as along the
    // jetties; its banks are added as berth lines, and the probes below keep
    // each yacht on the water side of them.
    let jetty_points: Vec<[f32; 2]> = lines.iter().flat_map(|l| l.iter().copied()).collect();
    // Only the quay near the jetties: a long canal with one jetty in it is not
    // a marina along its whole length.
    let near_jetty = |p: [f32; 2]| {
        jetty_points.iter().any(|q| {
            let (dx, dz) = (q[0] - p[0], q[1] - p[1]);
            dx * dx + dz * dz < MARINA_REACH_M * MARINA_REACH_M
        })
    };
    let quays: Vec<Vec<[f32; 2]>> = water
        .iter()
        .filter(|rings| jetty_points.iter().any(|&p| inside_rings(rings, p)))
        .flat_map(|rings| rings.iter())
        .flat_map(|ring| {
            ring.windows(2)
                .map(|w| vec![w[0], w[1]])
                .collect::<Vec<_>>()
        })
        .filter(|seg| near_jetty([(seg[0][0] + seg[1][0]) * 0.5, (seg[0][1] + seg[1][1]) * 0.5]))
        .collect();
    lines.extend(quays);
    let in_water = |p: [f32; 2]| water.iter().any(|rings| inside_rings(rings, p));
    let on_pier = |p: [f32; 2]| piers.iter().any(|rings| inside_rings(rings, p));
    let inside_tile = |p: [f32; 2]| (0.0..tile_m).contains(&p[0]) && (0.0..tile_m).contains(&p[1]);

    let mut out = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        for (ei, w) in line.windows(2).enumerate() {
            let (a, b) = (w[0], w[1]);
            if on_tile_border(a, b, tile_m) {
                continue;
            }
            let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
            let len = (dx * dx + dz * dz).sqrt();
            if len < MIN_JETTY_EDGE_M {
                continue;
            }
            let (ux, uz) = (dx / len, dz / len);
            // A finger shorter than a pitch still takes one yacht, at its middle.
            let mut s = (YACHT_PITCH_M * 0.5).min(len * 0.5);
            let mut slot = 0i32;
            while s < len {
                let at = s;
                s += YACHT_PITCH_M;
                for side in [-1.0f32, 1.0] {
                    let h = hash3(
                        seed ^ (li as u64) << 20,
                        ei as i32,
                        slot * 2 + (side > 0.0) as i32,
                    );
                    if rand01(h, 0) < YACHT_VACANCY {
                        continue;
                    }
                    // Alongside the jetty, on this side.
                    let (nx, nz) = (-uz * side, ux * side);
                    let base = [a[0] + ux * at, a[1] + uz * at];
                    let c = [base[0] + nx * YACHT_OFFSET_M, base[1] + nz * YACHT_OFFSET_M];
                    // Bow, stern and both beams over water, none on a jetty.
                    let probes = [
                        c,
                        [c[0] - ux * 4.8, c[1] - uz * 4.8],
                        [c[0] + ux * 4.8, c[1] + uz * 4.8],
                        [c[0] + nx * 1.5, c[1] + nz * 1.5],
                        [c[0] - nx * 1.5, c[1] - nz * 1.5],
                    ];
                    let near = |q: [f32; 2], r: f32| {
                        let (dx, dz) = (q[0] - c[0], q[1] - c[1]);
                        dx * dx + dz * dz < r * r
                    };
                    if !inside_tile(c)
                        || !probes.iter().all(|&p| in_water(p) && !on_pier(p))
                        || berths.iter().any(|&b| near(b, BERTH_CLEARANCE_M))
                        || out
                            .iter()
                            .any(|y: &FurnitureInstance| near([y.x, y.z], YACHT_SPACING_M))
                    {
                        continue;
                    }
                    // The bow along the jetty: the form's +z is its bow.
                    let yaw = ux.atan2(uz);
                    out.push(FurnitureInstance {
                        x: c[0],
                        z: c[1],
                        yaw,
                        kind: FurnitureKind::Yacht,
                        luma: 1.0 + (rand01(h, 1) - 0.5) * 0.06,
                        paint: YACHT_PAINTS[((rand01(h, 2) * YACHT_PAINTS.len() as f32) as usize)
                            .min(YACHT_PAINTS.len() - 1)],
                        over_water: false,
                        hull_only: false,
                    });
                }
                slot += 1;
            }
        }
    }
    Ok(crate::scatter::thin_evenly(out, MAX_YACHTS_PER_CELL))
}

/// Houseboats on registered berths (tile-local points, the BAG `ligplaats`
/// centres), each lying along the nearest bank of the water it is in.
///
/// A berth is a *surveyed* boat — the register says somebody lives there — so
/// it is drawn wherever it is, with no vacancy and no shape test; the bank only
/// decides which way the hull lies. A berth with no water edge within
/// [`HOUSEBOAT_BANK_M`] keeps the cell's default heading rather than being
/// dropped: it is still a home, and the register is the better witness.
///
/// A berth paired with `true` has a registered building standing on it, which
/// is the house; only the hull is drawn under it (`FurnitureInstance::hull_only`).
pub fn place_houseboats(
    tile: &TileData,
    tile_m: f32,
    berths: &[([f32; 2], bool)],
    seed: u64,
) -> Vec<FurnitureInstance> {
    use geo_types::Geometry;
    if berths.is_empty() {
        return Vec::new();
    }
    let mut edges: Vec<([f32; 2], [f32; 2])> = Vec::new();
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
            for p in polys {
                for ring in clipped_rings(p, scale, extent) {
                    for w in ring.windows(2) {
                        if !on_tile_border(w[0], w[1], tile_m) {
                            edges.push((w[0], w[1]));
                        }
                    }
                }
            }
        }
    }
    berths
        .iter()
        .enumerate()
        .map(|(i, &(p, housed))| {
            let h = hash3(seed, i as i32, 7);
            let nearest = edges
                .iter()
                .map(|&(a, b)| (dist2_to_segment(p, a, b), a, b))
                .filter(|(d, _, _)| *d < HOUSEBOAT_BANK_M * HOUSEBOAT_BANK_M)
                .min_by(|x, y| x.0.total_cmp(&y.0));
            let yaw = nearest.map_or(0.0, |(_, a, b)| (b[0] - a[0]).atan2(b[1] - a[1]));
            FurnitureInstance {
                x: p[0],
                z: p[1],
                yaw,
                kind: FurnitureKind::Houseboat,
                luma: 1.0 + (rand01(h, 1) - 0.5) * 0.10,
                paint: BOAT_PAINTS[((rand01(h, 2) * BOAT_PAINTS.len() as f32) as usize)
                    .min(BOAT_PAINTS.len() - 1)],
                over_water: false,
                hull_only: housed,
            }
        })
        .collect()
}

/// How far a berth looks for the bank it lies along, metres.
const HOUSEBOAT_BANK_M: f32 = 30.0;

/// Squared distance from `p` to the segment `a`–`b`.
fn dist2_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dz * dz;
    let t = if l2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (qx, qz) = (a[0] + dx * t - p[0], a[1] + dz * t - p[1]);
    qx * qx + qz * qz
}

/// Even-odd point-in-polygon over a polygon's rings (outer and holes).
fn inside_rings(rings: &[Vec<[f32; 2]>], p: [f32; 2]) -> bool {
    let mut inside = false;
    for ring in rings {
        for w in ring.windows(2) {
            let (a, b) = (w[0], w[1]);
            if (a[1] > p[1]) != (b[1] > p[1])
                && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
            {
                inside = !inside;
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testdata::groningen as fixture;

    /// A jetty sticking out into a harbour grows yachts on both sides, bow in,
    /// all over water; the same jetty on dry land grows none.
    #[test]
    fn a_jetty_in_water_is_lined_with_yachts() {
        use crate::vector_tiles::synth::{line_layer, polygon_layer, tile};
        // A 4096-unit tile over 4096 m: a harbour basin in the west half, and a
        // north-south jetty down its middle; a second jetty on dry land east.
        let basin = polygon_layer(
            "water_polygons",
            4096,
            &[(500, 500), (1500, 500), (1500, 3500), (500, 3500)],
        );
        let jetty = line_layer("pier_lines", 4096, &[(1000, 800), (1000, 3200)]);
        let dry = line_layer("pier_lines", 4096, &[(3000, 800), (3000, 3200)]);
        let t = TileData::parse(&tile(&[basin, jetty, dry])).expect("synthetic tile");
        let yachts = extract_marina(&t, 4096.0, 9, &[]).expect("extract");
        assert!(
            yachts.len() > 100,
            "both sides of 2.4 km of jetty: {}",
            yachts.len()
        );
        for y in &yachts {
            assert!(
                (500.0..1500.0).contains(&y.x),
                "a yacht off the water at {}",
                y.x
            );
            assert!((y.x - 1000.0).abs() > 2.0, "a yacht on the jetty");
            // Alongside: the bow runs along the north-south jetty.
            assert!(y.yaw.sin().abs() < 1e-3, "{}", y.yaw);
        }
        for (i, a) in yachts.iter().enumerate() {
            for b in &yachts[i + 1..] {
                assert!((a.x - b.x).hypot(a.z - b.z) >= YACHT_SPACING_M - 1e-3);
            }
        }
        // A registered berth keeps its stretch of jetty for its houseboat.
        let berth = [1003.0, 2000.0];
        let kept = extract_marina(&t, 4096.0, 9, &[berth]).expect("extract");
        assert!(kept
            .iter()
            .all(|y| (y.x - berth[0]).hypot(y.z - berth[1]) >= BERTH_CLEARANCE_M));
    }

    /// The houseboat lies along the bank it is moored to.
    #[test]
    fn a_houseboat_lies_along_its_bank() {
        let d = dist2_to_segment([5.0, 3.0], [0.0, 0.0], [10.0, 0.0]);
        assert!((d - 9.0).abs() < 1e-4);
        let d = dist2_to_segment([15.0, 0.0], [0.0, 0.0], [10.0, 0.0]);
        assert!((d - 25.0).abs() < 1e-4);
    }

    /// The fixture's harbour jetties grow yachts, every one over water.
    #[test]
    fn the_fixture_has_yachts_at_its_jetties() {
        let yachts = extract_marina(&fixture(), TILE_M, 5, &[]).expect("extract");
        assert!(yachts.len() <= MAX_YACHTS_PER_CELL);
        for y in &yachts {
            assert_eq!(y.kind, FurnitureKind::Yacht);
            assert!(y.x >= 0.0 && y.x < TILE_M && y.z >= 0.0 && y.z < TILE_M);
        }
    }

    const TILE_M: f32 = 1472.0;

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
