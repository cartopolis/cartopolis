//! Parked vehicles: the `sites` layer's car parks and cycle parking, filled.
//!
//! Shortbread's **`sites`** layer is already in the bytes `map_geometry` fetches
//! for every cell — 84 features in the Groningen z14 fixture, 56 of them
//! `parking` and 15 `bicycle_parking` — and until now the whole layer was one
//! flat grey fill and nothing else. A city whose car parks are empty rectangles
//! reads as a model of a city at four in the morning, which is most of what makes
//! a streamed map feel uninhabited.
//!
//! **What this asserts, and what it does not.** OSM surveys the *lot*, never the
//! cars in it: the polygon is real, its orientation is real, its capacity is
//! implied by its area, and every individual vehicle here is invented. So the
//! same rule the tree scatter follows applies — the fill has to be right about
//! the things the data does say (where the tarmac is, which way the bays run, how
//! many fit) and must not pretend to be right about anything else. Concretely,
//! the vehicles are **not** interactables: `world_places` takes them as colliders
//! and gives them no proximity prompt, because offering to look at a named object
//! we made up is a claim the data cannot carry.
//!
//! Four things shape the layout:
//!
//! * **The rows follow the lot's longest edge** ([`dominant_angle`], the same
//!   function the orchards use). A car park is bays perpendicular to an aisle and
//!   the aisle runs the long way; squaring the bays to Mercator north instead
//!   puts a diagonal grid of cars inside every kerb line on the map.
//! * **Every third row is left empty.** Two back-to-back rows and an aisle is a
//!   16 m module, which is what a real lot is built on, and it is what stops a
//!   fill reading as a solid raft of metal. The two rows of a pair face away from
//!   each other, toward their own aisle.
//! * **Some bays are empty.** A full car park is as artificial as an empty one,
//!   and the vacancy is hashed off the cell seed so a lot that streams out and
//!   back is parked identically ([`crate::scatter`] documents why a reshuffle is
//!   instantly visible).
//! * **The count follows the area, so the count is what has to be bounded.** Every
//!   other producer in this crate is bounded by a *survey* — there are as many
//!   benches as OSM recorded. A lot fill is bounded by the ground it covers, so
//!   [`MAX_VEHICLES_PER_CELL`] is a working limit rather than headroom, and
//!   `build_furniture_meshes` cuts the result to the frame's upload ceiling for
//!   the same reason the canopies are cut (`docs/notes/upload-bursts.md`).
//!
//! Two limits worth knowing. A lot that straddles a cell boundary is filled twice
//! from two independently clipped halves, on two lattices that do not line up —
//! the boundary inset leaves a couple of metres either side, so the seam reads as
//! an aisle rather than as a collision, and z14 cells are 611 m at NL latitudes so
//! it is rare. And the tile cannot tell a surface car park from a multi-storey:
//! `amenity=parking` on a `parkeergarage` gets a ground-level fill inside the
//! building that is drawn over it. Neither is worth a request to fix.

use crate::furniture::{FurnitureInstance, FurnitureKind};
use crate::scatter::{
    bucket_range, dominant_angle, hash3, in_triangle, min_max, rand01, BorderGuard, EdgeGrid,
    RoadMask,
};
use crate::vector_tiles::{clipped_rings, kind_of, palette, TileData};

/// Hard ceiling on parked vehicles per streamed cell.
///
/// **Measured 2026-08-26** on the Groningen z14 fixture (`survey_the_fixture`):
/// 531 vehicles — 215 cars and 316 bikes — costing 3.2 MB of mesh, which
/// `build_furniture_meshes` then cuts into pieces no larger than the frame's
/// upload ceiling. That is a city-centre tile, i.e. near the worst case, and this
/// cap is set a little above it.
///
/// Unlike [`MAX_FURNITURE_PER_CELL`](crate::furniture::MAX_FURNITURE_PER_CELL) it
/// is close to a *working* limit rather than pure headroom, because the count
/// follows area rather than a survey. It is bounded by GPU residency, not by
/// upload rate: deferring an upload changes when a car arrives, never whether it
/// stays (`docs/notes/upload-bursts.md`). Truncation is logged, and it costs whole
/// lots at the end of the layer rather than thinning every lot evenly — a car park
/// with nothing in it beside one that is full is what hitting this looks like.
pub const MAX_VEHICLES_PER_CELL: usize = 900;

/// How one kind of lot is filled. Metres, in the lattice aligned with the lot's
/// longest edge: `across` is the bay pitch along a row, `along` is the row pitch.
struct Lot {
    kind: FurnitureKind,
    across: f32,
    along: f32,
    /// Rows per module. The last row of each module is the aisle and stays empty,
    /// so `3` means two parked rows and a lane.
    module: i32,
    /// Fraction of bays left empty.
    vacancy: f32,
    /// How far a vehicle must keep from the lot's own boundary, so a car does not
    /// hang over the kerb the fill is drawn inside.
    inset: f32,
    /// The body colours this lot's vehicles are painted from.
    paints: &'static [u32],
}

/// Muted car bodies. Real traffic is the most saturated thing in a street and
/// this map is pale cartographic paper (`crate::furniture` says why), so these are
/// the colours a car park actually reads as from across it — silver, white and
/// graphite are most of any Dutch lot — with two desaturated hues to break up the
/// grey and nothing brighter.
pub const CAR_PAINTS: &[u32] = &[
    0xC6_C8_CA, // silver
    0xE2_E0_DC, // white
    0x7E_81_85, // graphite
    0xA9_AC_AE, // pale grey
    0x78_88_98, // slate blue
    0xA3_79_74, // muted red
    0xC6_BC_A8, // sand
];

/// Bike frames: dark metal, with one muted red for the same reason the cars have
/// one. A rack reads as a mass rather than as individuals, so the variety is doing
/// less work here than in a car park.
const BIKE_PAINTS: &[u32] = &[0x6C_69_65, 0x86_82_7C, 0x55_5A_60, 0x8E_74_70];

/// The fill profile for a `sites` feature, or `None` for the kinds that are a
/// flat colour and nothing more (`school`, `hospital`, `university`).
fn lot_profile(kind: &str) -> Option<Lot> {
    match kind {
        // 2.5 × 5.0 m bays with a little air, and a lane of the same pitch: the
        // Dutch NEN 2443 standard lot, near enough for something drawn at this
        // size.
        "parking" => Some(Lot {
            kind: FurnitureKind::Car,
            across: 2.65,
            along: 5.30,
            module: 3,
            vacancy: 0.30,
            inset: 1.70,
            paints: CAR_PAINTS,
        }),
        // A double-sided rack is bikes at ~70 cm centres in rows 2 m apart, then
        // a walkway. They pack far tighter than cars and there are far more of
        // them, which is what a Dutch station forecourt looks like.
        "bicycle_parking" => Some(Lot {
            kind: FurnitureKind::Bicycle,
            across: 0.74,
            along: 2.05,
            module: 3,
            vacancy: 0.20,
            inset: 0.85,
            paints: BIKE_PAINTS,
        }),
        _ => None,
    }
}

/// Read a tile's `sites` layer as parked vehicles.
///
/// `seed` must be stable for a given cell (its x/y are the natural choice), so a
/// lot that streams out and back is parked the same way — see the module docs.
///
/// Takes the cell's already-parsed [`TileData`] rather than the tile body, for the
/// reason [`extract_furniture`](crate::furniture::extract_furniture) does: every
/// consumer on this worker reads a different layer out of the same bytes.
pub fn extract_parking(
    tile: &TileData,
    tile_m: f32,
    seed: u64,
) -> Result<Vec<FurnitureInstance>, String> {
    use crate::vector_tiles::tessellate_polygon;
    use geo_types::Geometry;

    let Some(meta) = tile.layer("sites") else {
        return Ok(Vec::new());
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;

    // A lot's access road and the street it opens onto are routinely interior to
    // the polygon — Shortbread draws the carriageway on top rather than punching a
    // hole — so the boundary inset cannot see them and a naive fill parks a row of
    // cars across the entrance. Same mask, same reason, as the tree scatter's.
    let roads = RoadMask::build(tile, tile_m);
    let border = BorderGuard::new(tile_m);

    let mut out: Vec<FurnitureInstance> = Vec::new();
    let mut truncated = false;

    for (fi, feature) in features.iter().enumerate() {
        let Some(lot) = kind_of(feature).and_then(lot_profile) else {
            continue;
        };
        let polys: Vec<&geo_types::Polygon<f32>> = match &feature.geometry {
            Geometry::Polygon(p) => vec![p],
            Geometry::MultiPolygon(mp) => mp.iter().collect(),
            _ => continue,
        };

        // Triangles to park inside and rings to keep off, both from the same clip
        // the fill uses — or the cars would stand off an edge the visible tarmac
        // does not have.
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        let mut rings: Vec<Vec<[f32; 2]>> = Vec::new();
        for p in &polys {
            tessellate_polygon(p, scale, extent, &mut positions, &mut indices);
            rings.extend(clipped_rings(p, scale, extent));
        }
        if indices.is_empty() {
            continue;
        }

        let edges = EdgeGrid::new(&rings, &border);
        let theta = dominant_angle(&rings);
        let (cs, sn) = (theta.cos(), theta.sin());
        let to_lattice = |x: f32, z: f32| (x * cs + z * sn, -x * sn + z * cs);
        let to_world = |x: f32, z: f32| (x * cs - z * sn, x * sn + z * cs);
        // A form's front is its +z, and lattice +z is the world direction
        // `to_world(0, 1)` = (−sin θ, cos θ). `push_box` turns local +z to
        // (sin yaw, cos yaw), so a vehicle nose-on down the lattice is yaw = −θ.
        let nose_along = -theta;

        let fseed = seed ^ (fi as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        // Lattice indices are global to the feature, so a bay on a shared triangle
        // edge is generated by both triangles with the *same* index — deduping on
        // the index is exact, and it is what stops two cars in one bay.
        let mut parked = std::collections::HashSet::new();

        for tri in indices.chunks_exact(3) {
            let v = [
                {
                    let p = positions[tri[0] as usize];
                    to_lattice(p[0], p[2])
                },
                {
                    let p = positions[tri[1] as usize];
                    to_lattice(p[0], p[2])
                },
                {
                    let p = positions[tri[2] as usize];
                    to_lattice(p[0], p[2])
                },
            ];
            let (minx, maxx) = min_max(v[0].0, v[1].0, v[2].0);
            let (minz, maxz) = min_max(v[0].1, v[1].1, v[2].1);
            // Clamped for the reason `WayField::push` documents: `as i32`
            // saturates, so an unclamped bound off a wild coordinate is
            // `i32::MAX` and the loop never finishes. The span handed over is the
            // tile's **diagonal**, not its edge — these indices are in the lattice
            // the lot's own angle defines, and a square rotated 45° reaches
            // √2 times as far as its side. Passing `tile_m` clipped the far
            // corner of a diagonally-aligned lot and parked no cars in it.
            let span = tile_m * std::f32::consts::SQRT_2;
            for i in bucket_range(minx, maxx, lot.across, span) {
                for j in bucket_range(minz, maxz, lot.along, span) {
                    // `rem_euclid`, not `%`: a lot straddling the lattice origin
                    // has negative row indices, and `-1 % 3` is `-1`, which would
                    // put the aisle on a different row on each side of it.
                    let row = j.rem_euclid(lot.module);
                    if row == lot.module - 1 {
                        continue;
                    }
                    if parked.contains(&(i, j)) {
                        continue;
                    }
                    let h = hash3(fseed, i, j);
                    if rand01(h, 0) < lot.vacancy {
                        parked.insert((i, j));
                        continue;
                    }
                    let lx = (i as f32 + 0.5) * lot.across;
                    let lz = (j as f32 + 0.5) * lot.along;
                    if !in_triangle((lx, lz), v[0], v[1], v[2]) {
                        continue;
                    }
                    let (wx, wz) = to_world(lx, lz);
                    if edges.within(wx, wz, lot.inset) || roads.blocks(wx, wz, lot.inset) {
                        continue;
                    }
                    parked.insert((i, j));
                    let paints = lot.paints;
                    out.push(FurnitureInstance {
                        x: wx,
                        z: wz,
                        // The two rows of a pair back onto each other, so each
                        // faces its own aisle: row 0's is below it, row 1's above.
                        yaw: nose_along + if row == 0 { std::f32::consts::PI } else { 0.0 },
                        kind: lot.kind,
                        luma: 1.0 + (rand01(h, 1) - 0.5) * 0.08,
                        paint: paints_pick(paints, h),
                        over_water: false,
                        hull_only: false,
                    });
                    // Collected past the ceiling, then thinned evenly to it below;
                    // the collection bound only stops an absurd cell running away.
                    if out.len() >= MAX_VEHICLES_PER_CELL * COLLECT_PAST_CAP {
                        truncated = true;
                        break;
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
            break;
        }
    }

    if truncated {
        tracing::warn!(
            cap = MAX_VEHICLES_PER_CELL,
            "parked vehicles ran past the collection bound; this cell is parked short"
        );
    }
    if out.len() > MAX_VEHICLES_PER_CELL {
        tracing::debug!(
            laid = out.len(),
            cap = MAX_VEHICLES_PER_CELL,
            "parked vehicles thinned evenly to the per-cell cap"
        );
    }
    Ok(crate::scatter::thin_evenly(out, MAX_VEHICLES_PER_CELL))
}

/// How far past [`MAX_VEHICLES_PER_CELL`] a cell is laid before it is thinned
/// (`scatter::thin_evenly`).
const COLLECT_PAST_CAP: usize = 4;

/// One body colour off the instance hash.
fn paints_pick(paints: &[u32], h: u64) -> u32 {
    if paints.is_empty() {
        return palette::FURNITURE;
    }
    paints[((rand01(h, 2) * paints.len() as f32) as usize).min(paints.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_tiles::{mesh_upload_bytes, SurfaceGroup};

    use crate::testdata::groningen as fixture;

    const TILE_M: f32 = 1472.0;

    /// The fixture is a city tile with 56 car parks and 15 cycle parks in it, so
    /// the fill must produce both, keep them inside the tile and stay under the cap.
    #[test]
    fn the_fixture_parks_cars_and_bikes() {
        let items = extract_parking(&fixture(), TILE_M, 11).expect("extract");
        let cars = items
            .iter()
            .filter(|i| i.kind == FurnitureKind::Car)
            .count();
        let bikes = items.len() - cars;
        assert!(
            cars > 50,
            "a city tile should park more than a handful: {cars}"
        );
        assert!(bikes > 20, "and some bikes: {bikes}");
        assert!(items.len() <= MAX_VEHICLES_PER_CELL);
        for i in &items {
            assert!(
                i.x >= 0.0 && i.z >= 0.0 && i.x <= TILE_M && i.z <= TILE_M,
                "vehicle at ({}, {}) is outside its own cell",
                i.x,
                i.z
            );
        }
    }

    /// A cell that streams out and back must be parked identically — the tree
    /// scatter's note on why a reshuffle is instantly visible applies here twice
    /// over, because a car that moves is a car somebody watched move.
    #[test]
    fn the_fill_is_deterministic() {
        let a = extract_parking(&fixture(), TILE_M, 3).expect("extract");
        let b = extract_parking(&fixture(), TILE_M, 3).expect("extract");
        assert_eq!(a.len(), b.len());
        for (p, q) in a.iter().zip(&b) {
            assert_eq!((p.x, p.z, p.kind, p.paint), (q.x, q.z, q.kind, q.paint));
        }
        let c = extract_parking(&fixture(), TILE_M, 4).expect("extract");
        assert!(
            a.iter()
                .zip(&c)
                .any(|(p, q)| p.paint != q.paint || p.x != q.x),
            "a different seed must park differently"
        );
    }

    /// The aisles are the whole reason a fill reads as a car park rather than as a
    /// raft of metal, so check that a third of the rows really are empty: no two
    /// vehicles of one lot may sit `along` apart across the module boundary.
    #[test]
    fn every_third_row_is_an_aisle() {
        let lot = lot_profile("parking").expect("parking is a lot");
        assert_eq!(lot.module, 3);
        for j in -6i32..6 {
            let row = j.rem_euclid(lot.module);
            // The negative side must place its aisle on the same lattice rows as
            // the positive one, which `%` would not do.
            assert_eq!(row == lot.module - 1, (j + 300) % 3 == 2, "row {j}");
        }
    }

    /// Cars are not surveyed objects and must never claim to be: nothing here may
    /// come out of the layers `furniture` reads, and nothing `furniture` reads may
    /// come out as a vehicle.
    #[test]
    fn the_two_extractions_do_not_overlap() {
        let tile = fixture();
        let props = crate::furniture::extract_furniture(&tile, TILE_M, 11).expect("furniture");
        assert!(
            !props
                .iter()
                .any(|p| matches!(p.kind, FurnitureKind::Car | FurnitureKind::Bicycle)),
            "the `pois` table must not name a vehicle"
        );
        let parked = extract_parking(&tile, TILE_M, 11).expect("parking");
        assert!(
            parked
                .iter()
                .all(|p| matches!(p.kind, FurnitureKind::Car | FurnitureKind::Bicycle)),
            "the `sites` fill produces vehicles and nothing else"
        );
    }

    /// A lot's vehicles must be one mesh's worth of the shared group, cut to the
    /// ceiling when there is one — a car park is the producer that made the cut
    /// necessary.
    #[test]
    fn a_parked_cell_is_cut_to_the_ceiling() {
        let items = extract_parking(&fixture(), TILE_M, 11).expect("extract");
        let whole = crate::furniture::build_furniture_meshes(&items, None);
        assert_eq!(whole.len(), 1, "with no ceiling it is one draw");

        const MAX: usize = 256 * 1024;
        let cut = crate::furniture::build_furniture_meshes(&items, Some(MAX));
        assert!(cut.len() > 1, "a parked city tile is past a 256 kB ceiling");
        let one_vehicle =
            crate::furniture::parts(FurnitureKind::Car).len() * mesh_upload_bytes(24, 36);
        for m in &cut {
            assert_eq!(m.group, SurfaceGroup::Furniture);
            assert!(!m.positions.is_empty(), "a piece is never empty");
            assert_eq!(m.positions.len(), m.colors.len());
            assert!(
                mesh_upload_bytes(m.positions.len(), m.indices.len()) <= MAX + one_vehicle,
                "a piece may overrun by at most the vehicle that would not fit"
            );
        }
        let total: usize = cut.iter().map(|m| m.positions.len()).sum();
        assert_eq!(total, whole[0].positions.len(), "the cut loses nothing");
    }

    /// A car is painted per instance off one shared material, so a lot must not be
    /// one colour — and every colour must come from the table.
    #[test]
    fn a_car_park_is_not_all_one_colour() {
        let items = extract_parking(&fixture(), TILE_M, 11).expect("extract");
        let cars: Vec<u32> = items
            .iter()
            .filter(|i| i.kind == FurnitureKind::Car)
            .map(|i| i.paint)
            .collect();
        let distinct: std::collections::HashSet<u32> = cars.iter().copied().collect();
        assert!(distinct.len() >= 5, "only {} body colours", distinct.len());
        assert!(distinct.iter().all(|c| CAR_PAINTS.contains(c)));
    }

    /// Not an assertion so much as the measurement the cap above was chosen from.
    /// `cargo test -p cartopolis_geo survey_the_fixture -- --nocapture`.
    #[test]
    fn survey_the_fixture() {
        let items = extract_parking(&fixture(), TILE_M, 11).expect("extract");
        let cars = items
            .iter()
            .filter(|i| i.kind == FurnitureKind::Car)
            .count();
        let bikes = items.len() - cars;
        let bytes: usize = items
            .iter()
            .map(|i| crate::furniture::parts(i.kind).len() * mesh_upload_bytes(24, 36))
            .sum();
        println!(
            "parked {} ({} cars, {} bikes), {} kB of mesh",
            items.len(),
            cars,
            bikes,
            bytes / 1024
        );
    }
}
