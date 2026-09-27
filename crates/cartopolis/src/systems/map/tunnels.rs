//! The tunnels this client streams, and the floor you stand on inside one.
//!
//! The geometry is [`cartopolis_geo::tunnels`]; this is the half that needs Bevy.
//! It holds what a cell's tunnels are *as places* rather than as triangles — the
//! carriageway down there, and the stretch of ground that has to be missing over
//! the entrance — keyed by cell and dropped with it, the same contract
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
//! over a tunnel the deck is unreachable and you walk over the tunnel; from inside
//! it, the ground over your head is unreachable for the same reason and you walk
//! along it. Neither case needs to know the other exists. The only way between
//! them is the ramp, which rises continuously out of the deck — exactly as the
//! only way to an upper storey is the flight.
//!
//! # Why the walls have to be collision and cannot be a lie
//!
//! Without them, walking into a tunnel wall leaves the carriageway, finds no deck
//! under the next step, and the floor returns to ground level — which reads as
//! being fired seven metres into the air through the road above. So a tunnel is a
//! **hollow** collider ([`collision::Footprint`]) while you are inside it, and no
//! collider at all while you are not: on the surface it is not there to be walked
//! into, and underground it is the only thing that is.

use std::collections::HashMap;

use bevy::prelude::*;
use cartopolis_geo::tunnels::Tunnel;

/// How far back up a tunnel is lifted at one place — see
/// [`cartopolis_geo::tunnels::lift_for`], which is where it lives — as
/// [`lift_for_road`], which takes no lift for a corridor already measured.
///
/// One function because four places computed it — the drawn mesh, the walkable
/// floor, the nearest-tunnel metric and the shot harness — and three of them had
/// their own idea of the cap. The drawn deck and the floor under it disagreeing is
/// not a cosmetic difference; it is the avatar walking through the road. It sits
/// in the shared crate rather than here for the fifth caller: the harness draws
/// the same tunnels from the same tiles and cannot depend on this crate, so a copy
/// there is a copy that drifts (`docs/notes/harness-and-client-agree.md`).
pub use cartopolis_geo::tunnels::lift_for_road;

/// One cell's tunnels, as the worker built them: tile-local metres.
#[derive(Default)]
pub struct CellTunnels {
    pub tunnels: Vec<Tunnel>,
    /// The plan corridors the worker took the ground away over
    /// (`cartopolis_geo::tunnels::cut_ground`). Kept on this side as well as cut
    /// there, because a hole is a fact about the *floor* too: over one, the
    /// ground is not something to stand on, which is the whole of how the avatar
    /// gets down the ramp instead of walking over it.
    pub openings: Vec<Vec<[f32; 2]>>,
}

/// How far outside a covered corridor [`Tunnels::cover_over`] still holds the
/// ground above the roof: half the widest median a dual carriageway is mapped
/// with, so the strip between two lids is covered from both sides.
const COVER_REACH_M: f32 = 6.0;

/// Covered tunnels and openings copied out of [`Tunnels`] for a worker job, in the
/// anchored world-XZ frame.
#[derive(Clone, Default)]
pub struct LidSnapshot {
    tunnels: Vec<Tunnel>,
    openings: Vec<Vec<[f32; 2]>>,
}

impl LidSnapshot {
    /// The ground at a point as the vector ground is drawn: the survey, filled
    /// back over a lid or beside a deep cutting (`tunnels::Lidded` in
    /// `map_geometry`). For things placed on the ground on a worker — trees, the
    /// grass mask — which stood on the survey's old trench floor, under the lid,
    /// until 2026-09-23.
    #[must_use]
    pub fn ground_at(
        &self,
        x: f32,
        z: f32,
        ground: &crate::systems::terrain::TerrainSnapshot,
    ) -> f32 {
        let here = ground.at(x, z);
        if self.tunnels.is_empty() {
            return here;
        }
        let w = self
            .tunnels
            .iter()
            .map(|t| t.fill_weight([x, z], cartopolis_geo::tunnels::DATUM_M))
            .fold(0.0f32, f32::max);
        if w <= 0.0 {
            return here;
        }
        here + w * (ground.lid_level(x, z, cartopolis_geo::tunnels::DATUM_M) - here)
    }

    /// Whether a building with this footprint (world XZ) stands in a covered
    /// tunnel's old trench and so is not to be drawn —
    /// [`cartopolis_geo::tunnels::stands_in_a_covered_trench`].
    #[must_use]
    pub fn holds(
        &self,
        footprint: &[Vec2],
        ground: &crate::systems::terrain::TerrainSnapshot,
    ) -> bool {
        if (self.tunnels.is_empty() && self.openings.is_empty()) || footprint.is_empty() {
            return false;
        }
        let ring: Vec<[f32; 2]> = footprint.iter().map(|p| [p.x, p.y]).collect();
        cartopolis_geo::tunnels::footprint_in_a_covered_trench(
            &self.tunnels,
            &self.openings,
            &ring,
            &|p| {
                ground.surrounding(p[0], p[1], cartopolis_geo::tunnels::DATUM_M)
                    - ground.at(p[0], p[1])
            },
        )
    }
}

/// The measured terrain and the streamed tunnels together, for a system at
/// Bevy's parameter ceiling that needs the filled ground
/// ([`LidSnapshot::ground_at`]).
#[derive(bevy::ecs::system::SystemParam)]
pub struct FilledTerrain<'w> {
    pub terrain: Res<'w, crate::systems::terrain::TerrainField>,
    pub tunnels: Res<'w, Tunnels>,
}

/// One cell's tunnels in the anchored world-XZ frame.
struct Cell {
    tunnels: Vec<Tunnel>,
    /// Each tunnel's filled span ([`Tunnel::fill_bounds`]: covered, or sunk
    /// deep enough that the ground beside it is filled) as a bounding box, by the
    /// same index as `tunnels`; `None` where it has neither.
    ///
    /// Precomputed because every ground vertex in the city asks `fill_weight`
    /// about it, and a vertex nowhere near a tunnel — the common case — would
    /// otherwise walk every segment of every tunnel to find that out. The union
    /// makes a cell with no such stretch one comparison.
    filled: Vec<Option<[f32; 4]>>,
    filled_bbox: Option<[f32; 4]>,
    openings: Vec<Mouth>,
}

/// Is `p` inside `b` grown by `pad`?
fn within(p: [f32; 2], b: [f32; 4], pad: f32) -> bool {
    p[0] >= b[0] - pad && p[0] <= b[2] + pad && p[1] >= b[1] - pad && p[1] <= b[3] + pad
}

/// The smallest box containing both.
fn union(a: Option<[f32; 4]>, b: [f32; 4]) -> Option<[f32; 4]> {
    Some(match a {
        None => b,
        Some(v) => [
            v[0].min(b[0]),
            v[1].min(b[1]),
            v[2].max(b[2]),
            v[3].max(b[3]),
        ],
    })
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

/// Every tunnel standing, keyed by the cell that streamed it.
///
/// The frame is anchored world-XZ — the same one `collision::BuildingColliders`
/// and the avatar's own position are in, so a query needs no conversion at the
/// call site. The tile-local coordinates the tessellator works in are translated
/// once, here, at insert.
#[derive(Resource, Default)]
pub struct Tunnels {
    cells: HashMap<(i64, i64), Cell>,
}

impl Tunnels {
    /// Install (or replace) one cell's tunnels. `world` is the cell's north-west
    /// corner in world metres, which is what the tile-local coordinates are
    /// relative to.
    pub fn insert_cell(&mut self, cell: (i64, i64), world: Vec2, tunnels: CellTunnels) {
        if tunnels.tunnels.is_empty() && tunnels.openings.is_empty() {
            // Not merely an optimisation: an empty entry is indistinguishable
            // from a cell with tunnels when counting, and `tunnel_count` is what
            // `--dump-state` reports.
            self.cells.remove(&cell);
            return;
        }
        let shift = |p: [f32; 2]| [p[0] + world.x, p[1] + world.y];
        let moved: Vec<Tunnel> = tunnels
            .tunnels
            .into_iter()
            .map(|b| Tunnel {
                centre: b.centre.iter().copied().map(shift).collect(),
                ..b
            })
            .collect();
        // Once, here, while the cell is being installed — see `Cell::filled`.
        let filled: Vec<Option<[f32; 4]>> = moved.iter().map(Tunnel::fill_bounds).collect();
        let filled_bbox = filled.iter().flatten().fold(None, |a, b| union(a, *b));
        self.cells.insert(
            cell,
            Cell {
                tunnels: moved,
                filled,
                filled_bbox,
                openings: tunnels
                    .openings
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
    pub fn openings_near(&self, centre: Vec2, half: f32) -> Vec<Vec<[f32; 2]>> {
        let mut out = Vec::new();
        self.each_opening_near(centre, half, |m| {
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
    pub fn opening_digest(&self, centre: Vec2, half: f32) -> u64 {
        // Order-independent: the cells are a hash map and iterate in whatever
        // order they please, so the fold has to commute or the digest changes for
        // reasons that are not about geometry. Addition of a per-ring hash does.
        let mut sum: u64 = 0;
        self.each_opening_near(centre, half, |m| sum = sum.wrapping_add(m.hash));
        sum
    }

    /// What a clipmap tile stores to know its lid fill is still the right fill:
    /// a digest of the covered stretches within `half` of `centre`, the same
    /// per-tile shape as [`Self::opening_digest`] and for the same reason.
    #[must_use]
    pub fn lid_digest(&self, centre: Vec2, half: f32) -> u64 {
        use std::hash::{Hash, Hasher};
        let (lo, hi) = (centre - half, centre + half);
        let mut sum: u64 = 0;
        for c in self.cells.values() {
            // The filled bounds, a superset of the covered ones: the basemap's
            // fill reads `fill_weight`, and the buildings rule reads covered
            // corridors inside them.
            for b in c.filled.iter().flatten() {
                if b[0] <= hi.x && lo.x <= b[2] && b[1] <= hi.y && lo.y <= b[3] {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    b.map(f32::to_bits).hash(&mut h);
                    sum = sum.wrapping_add(h.finish());
                }
            }
        }
        sum
    }

    /// The union of every filled stretch's box that reaches the square, grown by
    /// `pad`, or `None` where none does — so a caller that has to visit a grid
    /// can visit only the part a fill could touch.
    #[must_use]
    pub fn filled_box_near(&self, centre: Vec2, half: f32, pad: f32) -> Option<[f32; 4]> {
        let (lo, hi) = (centre - half, centre + half);
        let mut out: Option<[f32; 4]> = None;
        for c in self.cells.values() {
            for b in c.filled.iter().flatten() {
                let g = [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad];
                if g[0] <= hi.x && lo.x <= g[2] && g[1] <= hi.y && lo.y <= g[3] {
                    out = union(out, g);
                }
            }
        }
        out
    }

    /// The lids and openings within `half` of `centre`, copied so a worker job
    /// can ask [`LidSnapshot::holds`] of its buildings without the resource.
    #[must_use]
    pub fn lids_near(&self, centre: Vec2, half: f32) -> LidSnapshot {
        let (lo, hi) = (centre - half, centre + half);
        let mut out = LidSnapshot::default();
        for c in self.cells.values() {
            // The filled bounds, a superset of the covered: `holds` asks each
            // tunnel about its covered stretches, and `ground_at` about all of it.
            for (t, b) in c.tunnels.iter().zip(&c.filled) {
                if let Some(b) = b {
                    if b[0] <= hi.x && lo.x <= b[2] && b[1] <= hi.y && lo.y <= b[3] {
                        out.tunnels.push(t.clone());
                    }
                }
            }
        }
        self.each_opening_near(centre, half, |m| out.openings.push(m.ring.clone()));
        out
    }

    fn each_opening_near(&self, centre: Vec2, half: f32, mut f: impl FnMut(&Mouth)) {
        let (lo, hi) = (centre - half, centre + half);
        for m in self.cells.values().flat_map(|c| c.openings.iter()) {
            let b = m.bbox;
            if b[0] <= hi.x && lo.x <= b[2] && b[1] <= hi.y && lo.y <= b[3] {
                f(m);
            }
        }
    }

    /// The deepest carriageway among the streamed tunnels, metres below the ground
    /// sheet (so zero or negative), for `--dump-state`.
    ///
    /// **A picture cannot report this and that is the point.** A cut seen from
    /// above shows a road through a hole whether the road is one metre down or
    /// seven; the ramp and the full-depth stretch look identical from anywhere
    /// except a low oblique. So the depth is a number, and a capture can assert
    /// it — which is what stops "I cannot see the depth" being answerable only by
    /// another camera angle.
    #[must_use]
    pub fn deepest_road(&self) -> f32 {
        self.cells
            .values()
            .flat_map(|c| c.tunnels.iter())
            .flat_map(|b| b.road_y.iter().copied())
            .fold(0.0, f32::min)
    }

    /// Bores standing, for `--dump-state`.
    #[must_use]
    pub fn tunnel_count(&self) -> usize {
        self.cells.values().map(|c| c.tunnels.len()).sum()
    }

    /// The deck level under `p`, metres relative to the ground sheet, corrected
    /// for the depth the terrain field has *already* dug. `None` where no tunnel's
    /// carriageway is under it.
    ///
    /// The **deepest** answer wins where two tunnels overlap in plan — a tunnel
    /// under a tunnel is `layer=-2` under `layer=-1`, and the one you can be
    /// standing on is the lower.
    ///
    /// **The floor has to move with the road, and the road moves.** A tunnel's deck
    /// is metres below the ground sheet, and over an open cut the measured ground
    /// sheet is the trench floor — so `map_geometry::stand_on_terrain` lifts the
    /// drawn tunnel back up by what AHN accounts for. A collision floor that did not
    /// do the same arithmetic would leave the avatar walking nine metres under the
    /// carriageway it can see, which is the kind of disagreement that reads as the
    /// tunnel being broken rather than as two places computing one number.
    ///
    /// `dug` answers "how far below the land around it is the measured ground
    /// here", and is a closure because this crate must not know about
    /// `TerrainField` — the cap is a property of the tunnel, so it is applied in
    /// here where the tunnel is known rather than by the caller.
    #[must_use]
    pub fn road_under_measured(&self, p: Vec2, dug: impl Fn(Vec2) -> f32) -> Option<f32> {
        let at = [p.x, p.y];
        self.cells
            .values()
            .flat_map(|c| c.tunnels.iter())
            .filter_map(|b| {
                b.road_under(at)
                    .map(|y| y + lift_for_road(b.measured, -y, dug(p)))
            })
            .min_by(f32::total_cmp)
    }

    /// The least height the ground at `p` may have over a lid, relative to the
    /// survey's ground there (`here`, the frame `dug` is measured in): the drawn
    /// roof plus a slab. `None` where `p` is over no covered stretch.
    ///
    /// **The fill's target is a guess about the land and the roof is not**, so the
    /// roof wins. The fill goes to the middle of a ring of survey samples 30 m out,
    /// and at a mouth part of that ring lies in the open trench beyond it; the
    /// target came out below the roof, so the lid's ground sagged into the mouth
    /// under the headwall and the roof's edges showed through the street and the
    /// canal as thin lines (Deck captures of the ring's east portal, 2026-09-23).
    #[must_use]
    pub fn cover_over(&self, p: Vec2, dug: f32) -> Option<f32> {
        let at = [p.x, p.y];
        self.cells
            .values()
            .filter(|c| c.filled_bbox.is_some_and(|b| within(at, b, COVER_REACH_M)))
            .flat_map(|c| c.tunnels.iter())
            // **Beside the lid as well as on it**: a dual carriageway is two
            // corridors with a strip between them, and over that strip the fill
            // could still come out below the inner walls' tops, which ran out of
            // the ground the length of the ring's east lid as a wedge toward the
            // canal (2026-09-23).
            .filter(|t| t.covered_weight(at, COVER_REACH_M) > 0.0)
            .filter_map(|t| {
                t.road_near(at, COVER_REACH_M).map(|y| {
                    y + lift_for_road(t.measured, -y, dug) + t.clearance
                        - cartopolis_geo::tunnels::CEILING_MAX_Y
                })
            })
            .reduce(f32::max)
    }

    /// How deep the tessellator put a carriageway under `p`, as a positive number
    /// of metres, before the height survey is allowed to account for any of it.
    /// `None` where no tunnel's corridor covers `p`.
    ///
    /// **The cap on the terrain lift is a property of the place, not of the mesh.**
    /// `stand_on_terrain` used to take it from the deepest vertex in whichever
    /// merged mesh it happened to be displacing and apply that one number to every
    /// vertex in it, which over-lifts everything shallower — and almost all of a
    /// tunnel is shallower, because [`depth_at`](cartopolis_geo::tunnels) spends
    /// `LEVEL_DEPTH_M / MAX_GRADE` ≈ 117 m ramping at each end. Forty metres into
    /// Groningen's ring, where the tessellator wants −2.4 m and the survey has
    /// already dug 6.4, the deck was lifted by the full 7 and drawn four metres
    /// *above* the land beside it. The runs there arrive 172–242 m long, so nearly
    /// all of one is ramp.
    ///
    /// The deepest answer wins where two tunnels overlap in plan, matching
    /// [`road_under_measured`](Self::road_under_measured): the one you could be
    /// standing on is the lower.
    #[must_use]
    pub fn convention_depth(&self, p: Vec2) -> Option<f32> {
        let at = [p.x, p.y];
        self.cells
            .values()
            .flat_map(|c| c.tunnels.iter())
            // A measured corridor is placed by the whole of what the survey dug
            // (`lift_for_road`): as deep as anything, so the cap never bites.
            .filter_map(|b| {
                b.road_under(at)
                    .map(|y| if b.measured { f32::NEG_INFINITY } else { y })
            })
            .min_by(f32::total_cmp)
            .map(|y| -y)
    }

    /// The nearest tunnel to `p`: how far its carriageway is in plan, and how deep
    /// the deck runs there. `None` when nothing is streamed.
    ///
    /// **`deepest_road` is a global minimum and cannot answer "is *that* road
    /// down there?"** It reports the deepest carriageway anywhere in the streamed
    /// cells, so it reads −7.0 over a city with one tunnel in it while the road
    /// in the frame sits on the ground — which is how a capture came to be
    /// described as showing a trench when it showed a surface street beside one.
    /// This answers for a place.
    #[must_use]
    pub fn nearest_tunnel(&self, p: Vec2, dug: impl Fn(Vec2) -> f32) -> Option<(f32, f32)> {
        let at = [p.x, p.y];
        self.cells
            .values()
            .flat_map(|c| c.tunnels.iter())
            .map(|b| {
                let (d, y) = b.distance_to(at);
                // The *drawn* depth, not the tessellator's convention. Reporting
                // the raw `road_y` was a metric that disagreed with the picture
                // beside it — it read −7.00 over a cut the measured ground puts
                // nine metres down — and a number that contradicts the capture it
                // is attached to is worse than no number.
                (d, y + lift_for_road(b.measured, -y, dug(p)))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
    }

    /// The nearest point where a tunnel goes under, and the compass heading the
    /// road is travelling there. Anchored world XZ, degrees clockwise from north.
    ///
    /// **This exists because aiming a camera at a tunnel by hand does not work.**
    /// Over two days, cameras placed by converting the tessellator's tile-local
    /// metres into a latitude and longitude landed 28, 42, 56, 62, 64, 103, 111,
    /// 122, 136, 140 and 178 metres from the nearest tunnel — a park, a rail yard
    /// and the side of an office block, each costing minutes of software
    /// rendering to discover. Two hand-written conversions disagreed with each
    /// other and both disagreed with the client. The client has never been
    /// confused about where these are: they are in the same frame as the avatar.
    /// So it answers, and `geo_nav::world_to_lat_lng` — the conversion everything
    /// else already trusts — turns it into something to type into `--at`.
    ///
    /// The entrance is the first vertex deep enough to take a roof, not the middle
    /// of an opening: an opening is the stretch that has *not* reached depth, so
    /// its centre is a road at lawn level, which is what four overhead captures
    /// faithfully photographed.
    #[must_use]
    pub fn nearest_entrance(&self, from: Vec2) -> Option<(Vec2, f32)> {
        let mut best: Option<(f32, Vec2, f32)> = None;
        for b in self.cells.values().flat_map(|c| c.tunnels.iter()) {
            let deepest = b.road_y.iter().copied().fold(0.0_f32, f32::min);
            if deepest > -1.0 {
                continue;
            }
            let full = deepest + cartopolis_geo::tunnels::ROOF_AT_DEPTH_M;
            let Some(k) = b.road_y.iter().position(|&y| y <= full) else {
                continue;
            };
            if k == 0 || k >= b.centre.len() {
                continue;
            }
            let (a, c) = (b.centre[k - 1], b.centre[k]);
            let at = Vec2::new(c[0], c[1]);
            // **The deepest tunnel, not the closest one.** Nearest picked a 41 m
            // service tunnel in a terraced street and reported it as *the*
            // entrance, which is true and useless: what a capture wants is the
            // road tunnel, and depth is what tells a road tunnel from a duct. Ties
            // go to the nearer of two, so this is still stable and still local.
            let d = deepest * 1000.0 + at.distance(from);
            if best.is_none_or(|(bd, _, _)| d < bd) {
                // +z is south, so north is −dz.
                let heading = (c[0] - a[0])
                    .atan2(a[1] - c[1])
                    .to_degrees()
                    .rem_euclid(360.0);
                best = Some((d, at, heading));
            }
        }
        best.map(|(_, at, h)| (at, h))
    }

    /// How much the ground at `p` is put back to the level of the land around it
    /// — over a lid, or beside a deep cutting — from 1 on the corridor to 0 past
    /// `taper_m`. See [`cartopolis_geo::tunnels::Tunnel::fill_weight`].
    #[must_use]
    pub fn fill_weight(&self, p: Vec2, taper_m: f32) -> f32 {
        let at = [p.x, p.y];
        let mut best = 0.0f32;
        for c in self.cells.values() {
            match c.filled_bbox {
                Some(b) if within(at, b, taper_m) => {}
                _ => continue,
            }
            for (t, bounds) in c.tunnels.iter().zip(&c.filled) {
                let Some(b) = bounds else { continue };
                if !within(at, *b, taper_m) {
                    continue;
                }
                best = best.max(t.fill_weight(at, taper_m));
                if best >= 1.0 {
                    return best;
                }
            }
        }
        best
    }

    /// True where the ground has been taken away — the open approach at a entrance.
    ///
    /// The one thing that makes a tunnel enterable on foot. Everywhere else the
    /// ground is a floor and the deck under it is unreachable, which is what keeps
    /// the pavement over a tunnel walkable; here the ground is a hole, so the floor
    /// is whatever is at the bottom of it.
    #[must_use]
    pub fn over_opening(&self, p: Vec2) -> bool {
        let at = [p.x, p.y];
        self.cells
            .values()
            .flat_map(|c| c.openings.iter())
            .any(|m| point_in_ring(at, &m.ring))
    }

    /// Every tunnel's carriageway outline, for the collider mirror.
    pub fn corridors(&self) -> impl Iterator<Item = Vec<Vec2>> + '_ {
        self.cells
            .values()
            .flat_map(|c| c.tunnels.iter())
            .map(|b| b.corridor().into_iter().map(Vec2::from).collect())
    }
}

/// How far below the ground sheet counts as "in a tunnel" for the rules that need
/// a yes or a no — the collision gate, and the building footprints that stop
/// applying once you are under them.
///
/// Not zero: the deck *reaches* zero at the entrance, and at that moment the avatar
/// is standing on the road outside as much as inside. Half a metre down is past
/// the kerb line and inside the cut.
pub const INSIDE_M: f32 = -0.5;

/// True when the avatar's floor puts it inside a tunnel rather than on the surface.
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
    use cartopolis_geo::tunnels::Tunnel;

    /// A cell holding one 20 m-wide opening running north from the cell corner.
    fn one_mouth(tunnels: &mut Tunnels, cell: (i64, i64), world: Vec2) {
        tunnels.insert_cell(
            cell,
            world,
            CellTunnels {
                tunnels: vec![Tunnel {
                    centre: vec![[0.0, 0.0], [0.0, 100.0]],
                    road_y: vec![0.0, -7.0],
                    half_w: 10.0,
                    clearance: 4.8,
                    covered: vec![true; 2],
                    measured: false,
                }],
                openings: vec![vec![
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
        let mut tunnels = Tunnels::default();
        one_mouth(&mut tunnels, (0, 0), Vec2::new(0.0, 0.0));
        let near = Vec2::new(0.0, 50.0);
        let far = Vec2::new(5000.0, 5000.0);
        let (over, away) = (
            tunnels.opening_digest(near, 92.0),
            tunnels.opening_digest(far, 92.0),
        );
        assert_ne!(over, 0, "the tile the opening is in must carry a digest");
        assert_eq!(away, 0, "a tile a kilometre away must carry none");

        // A second cell arriving elsewhere leaves the first tile's digest alone.
        one_mouth(&mut tunnels, (1, 0), Vec2::new(3000.0, 0.0));
        assert_eq!(tunnels.opening_digest(near, 92.0), over);
        assert_eq!(tunnels.opening_digest(far, 92.0), away);

        // …and retiring the cell the tile's opening came from does move it.
        tunnels.drop_cell((0, 0));
        assert_eq!(tunnels.opening_digest(near, 92.0), 0);
    }

    /// The rings come back in the tile's own frame, which is what the cut is given.
    ///
    /// Getting this wrong is invisible in a count and total in a picture: a hole
    /// cut at anchored-world coordinates lands wherever the tile happens to be
    /// relative to the map anchor, so the opening appears in a field somewhere and
    /// the tunnel stays roofed.
    #[test]
    fn the_openings_arrive_in_the_tiles_own_frame() {
        let mut tunnels = Tunnels::default();
        one_mouth(&mut tunnels, (0, 0), Vec2::new(1000.0, 2000.0));
        let centre = Vec2::new(1000.0, 2050.0);
        let rings = tunnels.openings_near(centre, 92.0);
        assert_eq!(rings.len(), 1);
        let ring = &rings[0];
        // The opening spans x −10..10 and z 0..100 about the cell corner, so about
        // the tile centre it spans x −10..10 and z −50..50.
        let xs: Vec<f32> = ring.iter().map(|p| p[0]).collect();
        let zs: Vec<f32> = ring.iter().map(|p| p[1]).collect();
        assert!((xs.iter().copied().fold(f32::MAX, f32::min) + 10.0).abs() < 1e-3);
        assert!((zs.iter().copied().fold(f32::MAX, f32::min) + 50.0).abs() < 1e-3);
        assert!((zs.iter().copied().fold(f32::MIN, f32::max) - 50.0).abs() < 1e-3);
    }
}
