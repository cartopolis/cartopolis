//! The bores this client streams, and the floor you stand on inside one.
//!
//! The geometry is [`cartopolis_geo::tunnels`]; this is the half that needs Bevy.
//! It holds what a cell's tunnels are *as places* rather than as triangles — the
//! carriageway down there, and the stretch of ground that has to be missing over
//! the portal — keyed by cell and dropped with it, the same contract
//! `world_places`, `street_labels` and the drive network keep.
//!
//! # A tunnel floor is a negative storey
//!
//! `interiors::InteriorFloors` already answers "how high is the ground here" for
//! a building's upper storeys, the avatar carries the answer as
//! [`AvatarState::floor_lift`](crate::resources::AvatarState::floor_lift), and
//! both movement paths add it to the terrain the way they add the terrain to
//! zero. A tunnel is that mechanism with the sign flipped, and it needs no new
//! channel: the deck is a floor seven metres *down*.
//!
//! What keeps the two apart is the rule that was already there. A surface more
//! than `interiors::STEP_UP_M` above you cannot pick you up, so from the pavement
//! over a bore the deck is unreachable and you walk over the tunnel; from inside
//! it, the ground over your head is unreachable for the same reason and you walk
//! along it. Neither case needs to know the other exists. The only way between
//! them is the ramp, which rises continuously out of the deck — exactly as the
//! only way to an upper storey is the flight.
//!
//! # Why the walls have to be collision and cannot be a lie
//!
//! Without them, walking into a tunnel wall leaves the carriageway, finds no deck
//! under the next step, and the floor returns to ground level — which reads as
//! being fired seven metres into the air through the road above. So a bore is a
//! **hollow** collider ([`collision::Footprint`]) while you are inside it, and no
//! collider at all while you are not: on the surface it is not there to be walked
//! into, and underground it is the only thing that is.

use std::collections::HashMap;

use bevy::prelude::*;
use cartopolis_geo::tunnels::Bore;

/// One cell's tunnels, as the worker built them: tile-local metres.
#[derive(Default)]
pub struct CellTunnels {
    pub bores: Vec<Bore>,
    /// The plan corridors the worker took the ground away over
    /// (`cartopolis_geo::tunnels::cut_ground`). Kept on this side as well as cut
    /// there, because a hole is a fact about the *floor* too: over one, the
    /// ground is not something to stand on, which is the whole of how the avatar
    /// gets down the ramp instead of walking over it.
    pub mouths: Vec<Vec<[f32; 2]>>,
}

/// One cell's tunnels in the anchored world-XZ frame.
struct Cell {
    bores: Vec<Bore>,
    mouths: Vec<Mouth>,
}

/// One opening, with the two things every frame asks of it precomputed.
///
/// **Both are a per-frame cost otherwise.** `map_stream` tests every clipmap tile
/// against every opening on every frame — 424 tiles against the openings of a
/// whole city — so deriving a ring's bounding box by walking its points there
/// would be millions of comparisons a frame to answer a question whose answer
/// never changes after the cell lands. The hash is precomputed for the same
/// reason: a tile's digest is then the sum of the hashes of the openings that
/// reach it, and reaches nothing else.
struct Mouth {
    /// `[min x, min z, max x, max z]`.
    bbox: [f32; 4],
    ring: Vec<[f32; 2]>,
    hash: u64,
}

impl Mouth {
    fn new(ring: Vec<[f32; 2]>) -> Self {
        let mut bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for p in &ring {
            bbox[0] = bbox[0].min(p[0]);
            bbox[1] = bbox[1].min(p[1]);
            bbox[2] = bbox[2].max(p[0]);
            bbox[3] = bbox[3].max(p[1]);
            for v in p {
                hash ^= u64::from(v.to_bits());
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        }
        Self { bbox, ring, hash }
    }
}

/// Every bore standing, keyed by the cell that streamed it.
///
/// The frame is anchored world-XZ — the same one `collision::BuildingColliders`
/// and the avatar's own position are in, so a query needs no conversion at the
/// call site. The tile-local coordinates the tessellator works in are translated
/// once, here, at insert.
#[derive(Resource, Default)]
pub struct TunnelBores {
    cells: HashMap<(i64, i64), Cell>,
}

impl TunnelBores {
    /// Install (or replace) one cell's tunnels. `world` is the cell's north-west
    /// corner in world metres, which is what the tile-local coordinates are
    /// relative to.
    pub fn insert_cell(&mut self, cell: (i64, i64), world: Vec2, tunnels: CellTunnels) {
        if tunnels.bores.is_empty() && tunnels.mouths.is_empty() {
            // Not merely an optimisation: an empty entry is indistinguishable
            // from a cell with tunnels when counting, and `bore_count` is what
            // `--dump-state` reports.
            self.cells.remove(&cell);
            return;
        }
        let shift = |p: [f32; 2]| [p[0] + world.x, p[1] + world.y];
        self.cells.insert(
            cell,
            Cell {
                bores: tunnels
                    .bores
                    .into_iter()
                    .map(|b| Bore {
                        centre: b.centre.iter().copied().map(shift).collect(),
                        ..b
                    })
                    .collect(),
                mouths: tunnels
                    .mouths
                    .into_iter()
                    .map(|ring| Mouth::new(ring.iter().copied().map(shift).collect()))
                    .collect(),
            },
        );
    }

    /// Drop one cell's tunnels (the cell was retired or the map re-anchored).
    pub fn drop_cell(&mut self, cell: (i64, i64)) {
        self.cells.remove(&cell);
    }

    /// Drop everything (re-anchor: every stored coordinate is in the old frame).
    pub fn clear(&mut self) {
        self.cells.clear();
    }

    /// Every opening overlapping the square of `half` metres about `centre`, moved
    /// into that square's own frame.
    ///
    /// The frame change is the point: a clipmap tile's vertices are metres from its
    /// own centre, and the rings are kept in anchored world metres. A ring is
    /// returned whole even when only a corner of it reaches the tile — clipping it
    /// to the square would put an edge down the middle of the opening, and the cut
    /// itself is happy to be handed geometry that misses.
    #[must_use]
    pub fn mouths_near(&self, centre: Vec2, half: f32) -> Vec<Vec<[f32; 2]>> {
        let mut out = Vec::new();
        self.each_mouth_near(centre, half, |m| {
            out.push(
                m.ring
                    .iter()
                    .map(|p| [p[0] - centre.x, p[1] - centre.y])
                    .collect(),
            );
        });
        out
    }

    /// What a clipmap tile stores to know its hole is still the right hole.
    ///
    /// **Not a global revision, and the difference is a burst.** Every cell that
    /// lands changes the set of openings, and a single counter would rebuild every
    /// cut tile in the pyramid each time one arrived — the coarse levels cover the
    /// whole city, so they would re-mesh on every vector cell of a fill. A digest
    /// of the rings that reach *this* tile is quiet unless this tile's own hole
    /// moved, and it costs a bounding-box test per ring with nothing allocated.
    #[must_use]
    pub fn mouth_digest(&self, centre: Vec2, half: f32) -> u64 {
        // Order-independent: the cells are a hash map and iterate in whatever
        // order they please, so the fold has to commute or the digest changes for
        // reasons that are not about geometry. Addition of a per-ring hash does.
        let mut sum: u64 = 0;
        self.each_mouth_near(centre, half, |m| sum = sum.wrapping_add(m.hash));
        sum
    }

    fn each_mouth_near(&self, centre: Vec2, half: f32, mut f: impl FnMut(&Mouth)) {
        let (lo, hi) = (centre - half, centre + half);
        for m in self.cells.values().flat_map(|c| c.mouths.iter()) {
            let b = m.bbox;
            if b[0] <= hi.x && lo.x <= b[2] && b[1] <= hi.y && lo.y <= b[3] {
                f(m);
            }
        }
    }

    /// The deepest carriageway among the streamed bores, metres below the ground
    /// sheet (so zero or negative), for `--dump-state`.
    ///
    /// **A picture cannot report this and that is the point.** A cut seen from
    /// above shows a road through a hole whether the road is one metre down or
    /// seven; the ramp and the full-depth stretch look identical from anywhere
    /// except a low oblique. So the depth is a number, and a capture can assert
    /// it — which is what stops "I cannot see the depth" being answerable only by
    /// another camera angle.
    #[must_use]
    pub fn deepest_deck(&self) -> f32 {
        self.cells
            .values()
            .flat_map(|c| c.bores.iter())
            .flat_map(|b| b.deck_y.iter().copied())
            .fold(0.0, f32::min)
    }

    /// Bores standing, for `--dump-state`.
    #[must_use]
    pub fn bore_count(&self) -> usize {
        self.cells.values().map(|c| c.bores.len()).sum()
    }

    /// The deck level under `p`, metres relative to the ground sheet (so always
    /// zero or below), or `None` where no bore's carriageway is under it.
    ///
    /// The **deepest** answer wins where two bores overlap in plan — a tunnel
    /// under a tunnel is `layer=-2` under `layer=-1`, and the one you can be
    /// standing on is the lower.
    #[must_use]
    pub fn deck_under(&self, p: Vec2) -> Option<f32> {
        let at = [p.x, p.y];
        self.cells
            .values()
            .flat_map(|c| c.bores.iter())
            .filter_map(|b| b.deck_under(at))
            .min_by(f32::total_cmp)
    }

    /// True where the ground has been taken away — the open approach at a portal.
    ///
    /// The one thing that makes a tunnel enterable on foot. Everywhere else the
    /// ground is a floor and the deck under it is unreachable, which is what keeps
    /// the pavement over a bore walkable; here the ground is a hole, so the floor
    /// is whatever is at the bottom of it.
    #[must_use]
    pub fn over_opening(&self, p: Vec2) -> bool {
        let at = [p.x, p.y];
        self.cells
            .values()
            .flat_map(|c| c.mouths.iter())
            .any(|m| point_in_ring(at, &m.ring))
    }

    /// Every bore's carriageway outline, for the collider mirror.
    pub fn corridors(&self) -> impl Iterator<Item = Vec<Vec2>> + '_ {
        self.cells
            .values()
            .flat_map(|c| c.bores.iter())
            .map(|b| b.corridor().into_iter().map(Vec2::from).collect())
    }
}

/// How far below the ground sheet counts as "in a tunnel" for the rules that need
/// a yes or a no — the collision gate, and the building footprints that stop
/// applying once you are under them.
///
/// Not zero: the deck *reaches* zero at the portal, and at that moment the avatar
/// is standing on the road outside as much as inside. Half a metre down is past
/// the kerb line and inside the cut.
pub const INSIDE_M: f32 = -0.5;

/// True when the avatar's floor puts it inside a bore rather than on the surface.
#[must_use]
pub fn inside(floor_lift: f32) -> bool {
    floor_lift <= INSIDE_M
}

/// Even-odd point in ring. The rings are closed (first point repeated), which
/// costs one degenerate edge and no correctness.
fn point_in_ring(p: [f32; 2], ring: &[[f32; 2]]) -> bool {
    if ring.len() < 3 {
        return false;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_geo::tunnels::Bore;

    /// A cell holding one 20 m-wide opening running north from the cell corner.
    fn one_mouth(bores: &mut TunnelBores, cell: (i64, i64), world: Vec2) {
        bores.insert_cell(
            cell,
            world,
            CellTunnels {
                bores: vec![Bore {
                    centre: vec![[0.0, 0.0], [0.0, 100.0]],
                    deck_y: vec![0.0, -7.0],
                    half_w: 10.0,
                    clearance: 4.8,
                }],
                mouths: vec![vec![
                    [-10.0, 0.0],
                    [10.0, 0.0],
                    [10.0, 100.0],
                    [-10.0, 100.0],
                    [-10.0, 0.0],
                ]],
            },
        );
    }

    /// **The digest is what stops a fill re-meshing the whole pyramid.**
    ///
    /// Every vector cell that lands changes the set of openings somewhere. A tile
    /// the change does not reach has to see the same number afterwards, or the
    /// coarse levels — which cover the whole city — rebuild once per cell of a
    /// fill, which is the upload burst this codebase keeps measuring.
    #[test]
    fn a_tile_the_opening_misses_sees_no_change() {
        let mut bores = TunnelBores::default();
        one_mouth(&mut bores, (0, 0), Vec2::new(0.0, 0.0));
        let near = Vec2::new(0.0, 50.0);
        let far = Vec2::new(5000.0, 5000.0);
        let (over, away) = (
            bores.mouth_digest(near, 92.0),
            bores.mouth_digest(far, 92.0),
        );
        assert_ne!(over, 0, "the tile the opening is in must carry a digest");
        assert_eq!(away, 0, "a tile a kilometre away must carry none");

        // A second cell arriving elsewhere leaves the first tile's digest alone.
        one_mouth(&mut bores, (1, 0), Vec2::new(3000.0, 0.0));
        assert_eq!(bores.mouth_digest(near, 92.0), over);
        assert_eq!(bores.mouth_digest(far, 92.0), away);

        // …and retiring the cell the tile's opening came from does move it.
        bores.drop_cell((0, 0));
        assert_eq!(bores.mouth_digest(near, 92.0), 0);
    }

    /// The rings come back in the tile's own frame, which is what the cut is given.
    ///
    /// Getting this wrong is invisible in a count and total in a picture: a hole
    /// cut at anchored-world coordinates lands wherever the tile happens to be
    /// relative to the map anchor, so the opening appears in a field somewhere and
    /// the tunnel stays roofed.
    #[test]
    fn the_openings_arrive_in_the_tiles_own_frame() {
        let mut bores = TunnelBores::default();
        one_mouth(&mut bores, (0, 0), Vec2::new(1000.0, 2000.0));
        let centre = Vec2::new(1000.0, 2050.0);
        let rings = bores.mouths_near(centre, 92.0);
        assert_eq!(rings.len(), 1);
        let ring = &rings[0];
        // The mouth spans x −10..10 and z 0..100 about the cell corner, so about
        // the tile centre it spans x −10..10 and z −50..50.
        let xs: Vec<f32> = ring.iter().map(|p| p[0]).collect();
        let zs: Vec<f32> = ring.iter().map(|p| p[1]).collect();
        assert!((xs.iter().copied().fold(f32::MAX, f32::min) + 10.0).abs() < 1e-3);
        assert!((zs.iter().copied().fold(f32::MAX, f32::min) + 50.0).abs() < 1e-3);
        assert!((zs.iter().copied().fold(f32::MIN, f32::max) - 50.0).abs() < 1e-3);
    }
}
