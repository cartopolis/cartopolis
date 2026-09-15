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
    mouths: Vec<Vec<[f32; 2]>>,
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
                    .map(|ring| ring.iter().copied().map(shift).collect())
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
            .any(|ring| point_in_ring(at, ring))
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
