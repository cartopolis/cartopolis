//! Ambient traffic: cars driving the streets the map already draws.
//!
//! The pure half — which centrelines are drivable, which way and how fast — is
//! [`cartopolis_geo::traffic`]. This is the ECS half: where the network is kept,
//! the pool of cars that drives it, and the one mesh they all share.
//!
//! It is the first thing in this codebase that *moves on its own*. Everything
//! else streamed from the map is placed and then stands still — buildings,
//! trees, benches, parked cars — and a city of stationary objects is most of what
//! makes a streamed map read as a model rather than a place. Live transit
//! vehicles (`systems::transit_vehicles`) move, but they are a real feed of real
//! buses and only exist where OVapi has one.
//!
//! Four things shape the design, and three of them are the same constraint:
//!
//! * **Nothing may be built per frame.** The documented device-loss failure in
//!   this tree is mass mesh churn (`docs/notes/upload-bursts.md`), and a moving
//!   object is the obvious way to reintroduce it. So there is exactly **one** car
//!   mesh, built once at startup and shared by every car; a car is an entity with
//!   a `Transform`, and driving it writes that transform and nothing else. After
//!   startup this layer uploads nothing, ever. `systems::grass` reached the same
//!   shape from the same constraint — a fixed pool that slides rather than a
//!   stream that rebuilds.
//! * **The pool is fixed and the cars are recycled.** [`MAX_CARS`] entities are
//!   spawned once. A car that drives out of range is not despawned; it is *given
//!   a new road* near the camera. Spawning and despawning on the move is the
//!   churn again, one level up.
//! * **Colour needs no per-car material.** A shared mesh cannot carry a
//!   per-instance tint, so the paints are a handful of shared materials and each
//!   car holds one of them — the same small palette the parked cars use
//!   (`cartopolis_geo::parking`), because a moving car and a parked one on the
//!   same street must not be from different worlds.
//! * **A car is not a physics body.** It follows its line at its road's speed,
//!   turns to face along it, and hands off to a connected line at the end. It
//!   does not accelerate, avoid, queue or stop — see the module's list of what it
//!   deliberately is not, below.
//!
//! ## What this deliberately is not
//!
//! No collision between cars, no traffic-light obedience, no give-way, no
//! queueing, no avatar interaction. Every one of those is a simulation problem
//! whose cost lands on the frame, and none of them is what the layer is for: the
//! question it answers is "is this street alive", which is settled at a glance
//! from a distance where two cars overlapping for a moment is invisible. The
//! signals already cycle (`systems::street_detail`) and cars ignore them; that is
//! a known limit, recorded rather than hidden.

use bevy::prelude::*;
use big_space::prelude::{BigSpace, Grid, GridCell};
use std::collections::HashMap;

use cartopolis_geo::traffic::{DriveLine, Flow, Uses};

use crate::components::{FreeCamera, HideInSpace};
use crate::grid::WORLD_CELL_EDGE;

/// How many cars exist. Fixed for the life of the process.
///
/// Sized for "the street I am looking at is alive", not for a traffic census. The
/// cars are recycled onto roads near the camera, so this is a budget for the
/// *visible* neighbourhood rather than for the city — raising it does not widen
/// the area covered, it thickens the traffic in the same area.
///
/// **Raised from 96 on 2026-08-26** after a capture over the Groningen centre put
/// exactly one car in view: a city-centre ring holds a lot of streets, and spread
/// evenly over all of them a hundred cars is one every few blocks. The count is
/// cheap in draw calls because every car shares one mesh and one of a handful of
/// materials, so bevy batches them — it is entity and transform work, not draws.
pub const MAX_CARS: usize = 180;

/// Cars are only ever placed on a road within this distance of the camera, and
/// are recycled once they get half again as far.
///
/// The two are separate numbers on purpose: one radius for both would recycle a
/// car the instant it crossed the line and then place it just inside, so a car at
/// the boundary would flicker between the two states.
const PLACE_RADIUS_M: f32 = 260.0;
/// Where a car is taken off the road and given a new one. See [`PLACE_RADIUS_M`].
const RECYCLE_RADIUS_M: f32 = 390.0;

/// Altitude at which traffic stops being drawn.
///
/// Above this a car is well under a pixel and the whole layer is a shimmer. It is
/// higher than the street furniture's fade
/// ([`FURNITURE_FADE_END`](cartopolis_geo::furniture::FURNITURE_FADE_END), 450 m)
/// because a moving object is picked out by the eye at a range a stationary one
/// of the same size is not — motion is the signal, and it survives being small.
const TRAFFIC_HIDE_ALT: f32 = 700.0;

/// Find a road leading on from `at`, and which way to drive it.
///
/// Returns the line and the direction (`+1` along its own order, `-1` against).
/// A car joining a road joins it at an end, so where along it to start from is
/// not a choice — it is zero or the line's length, and the caller derives it.
///
/// The road just left is excluded, or a car reaching a dead end turns round and
/// drives back down it forever, which reads as a fault rather than as traffic.
///
/// **The choice is by heading, not by distance.** It used to take whichever line
/// end was nearest, which at a crossroads is decided by a metre of survey
/// accuracy: a car running down a main road would turn off it at random every
/// time a side street happened to be noded a few centimetres closer than the
/// continuation. Traffic mostly goes *straight on*, so the score is the cosine
/// of the turn the car would be making, with the nearness of the join as a
/// tie-break, and a turn sharper than [`TURN_BACK_COS`] is only taken when
/// nothing else is on offer.
fn hand_off(
    net: &DriveNetwork,
    candidates: &[LineKey],
    from: LineKey,
    at: Vec2,
    travel: Vec2,
    kind: AgentKind,
) -> Option<(LineKey, f32)> {
    let mut best: Option<(f32, LineKey, f32)> = None;
    let mut fallback: Option<(f32, LineKey, f32)> = None;
    for &key in candidates {
        if key == from {
            continue;
        }
        let Some(l) = net.get(key) else {
            continue;
        };
        let (start, end) = (l.points[0], *l.points.last()?);
        // Joining at the start means driving the line forwards from zero; joining
        // at the end means driving it backwards, which this represents as `dir`
        // −1 and `s` counted from the same zero (see how `along` is computed).
        for (join, dir) in [(start, 1.0f32), (end, -1.0f32)] {
            let d = join.distance(at);
            if d > HANDOFF_M {
                continue;
            }
            // A one-way road may only be joined at the end traffic enters by —
            // and only by traffic it applies to. A pavement is two-way whatever
            // the carriageway beside it does, which is what somebody walking the
            // wrong way up a one-way street is doing every day.
            let allowed = match (kind, l.flow) {
                (AgentKind::Walker, _) | (_, Flow::Both) => true,
                (_, Flow::Forward) => dir > 0.0,
                (_, Flow::Backward) => dir < 0.0,
            };
            if !allowed {
                continue;
            }
            // The direction the agent would be travelling once on it.
            let (_, heading) = l.at(if dir > 0.0 { 0.0 } else { l.length() });
            let cos = travel.dot(heading * dir);
            // Nearness only separates two roads that leave at the same angle, so
            // it is worth a fraction of a cosine and no more.
            let score = cos - d / HANDOFF_M * 0.05;
            let slot = if cos >= TURN_BACK_COS {
                &mut best
            } else {
                &mut fallback
            };
            if slot.is_none_or(|(s, ..)| score > s) {
                *slot = Some((score, key, dir));
            }
        }
    }
    best.or(fallback).map(|(_, key, dir)| (key, dir))
}

/// How close two line ends must be for a car to drive from one onto the other.
///
/// Tiles at a zoom share one integer lattice, so a road split across two cells
/// yields the *same* coordinate in both and a hand-off needs no tolerance at all
/// (`systems::routing` documents this). The tolerance is for the other case: two
/// different roads meeting at a junction, where the ends are a carriageway's
/// width apart rather than coincident.
const HANDOFF_M: f32 = 14.0;

/// Metres a car keeps to the right of its road's centre line.
///
/// A fraction of the carriageway rather than a fixed offset, so a car on a
/// motorway is not driving up the middle and a car in an alley is not in the
/// gutter. Right-hand traffic: the Netherlands, and every country this map has
/// data for.
const LANE_FRACTION: f32 = 0.45;

/// How hard a car pulls away, in m/s².
///
/// Well under a real car's, and that is the point: ambient traffic is watched
/// from the pavement, where a vehicle that reaches the speed limit in its own
/// length reads as a game object. Before this the speed was *assigned* — a car
/// handed from a residential street onto a trunk road stepped from 7.5 m/s to
/// 22 m/s between two frames, and every recycled car appeared already at speed.
const CAR_ACCEL: f32 = 2.2;

/// How hard a car slows, in m/s². Larger than [`CAR_ACCEL`], as a car's brakes
/// are stronger than its engine — and it has to be, or a car cannot shed enough
/// speed inside [`CORNER_LOOKAHEAD_M`] to take the corner it is looking at.
const CAR_BRAKE: f32 = 5.5;

/// A walker's, m/s². Small in both directions: nobody accelerates.
const WALK_ACCEL: f32 = 1.4;

/// How far ahead a car looks for the bend it must slow for.
///
/// Deliberately short of what a real driver uses: `v²/2a` at a trunk road's
/// speed is 44 m, and braking from off screen for a bend nobody can see yet
/// reads as traffic slowing for nothing. A car that arrives at a bend still too
/// fast simply takes it a little wide.
const CORNER_LOOKAHEAD_M: f32 = 30.0;

/// Lateral acceleration a driver accepts through a bend, m/s². About a quarter
/// of gravity — brisk but well short of the tyre limit, and it is what sets how
/// fast a corner is taken: `v = √(a·r)`.
const CORNER_LAT_ACCEL: f32 = 2.8;

/// How fast an agent may swing its nose, in radians per second **per m/s** of
/// its own speed.
///
/// A simplified centre line is a chain of straight segments, so its heading is a
/// *step* function: at every vertex the tangent changes by the whole bend at
/// once, and a car reading it directly snapped its yaw by the full angle between
/// two frames — several times down any curving street. The drawn heading eases
/// toward the path's instead, rate-limited. It is per m/s rather than a flat
/// rate because a vehicle turns through a *radius*, and `v / r` is what a yaw
/// rate is: this allows about an eight-metre one.
const TURN_RATE_PER_MS: f32 = 0.12;

/// …and the floor under it, in radians per second.
///
/// A rate proportional to speed alone leaves anything slow turning in slow
/// motion — a walker at 1.25 m/s would need fifteen seconds to round a corner,
/// which is a much worse artefact than the snap this replaces. In practice the
/// floor governs city speeds and the rate above it governs fast roads.
const TURN_RATE_FLOOR: f32 = 1.4;

/// The gap a car keeps to the one in front of it, and the gap at which it has
/// stopped entirely.
///
/// This is **not** the queueing simulation the module note rules out: there is
/// still no give-way, no signal obedience and no collision test. It is one
/// lookup in the occupancy map [`drive_traffic`] already builds for placement,
/// against the nearest agent *on the same line travelling the same way* — the
/// only pair that can visibly drive through each other, since two cars on one
/// street at different speeds converge and then overlap for as long as it takes
/// one of them to be recycled. Walkers are exempt: at 1.25 m/s two of them
/// passing through each other is not what anybody is looking at, and a pavement
/// where nobody can overtake is worse than one where they do.
const HEADWAY_M: f32 = 24.0;
/// See [`HEADWAY_M`]. A car's own length and a little more.
const MIN_HEADWAY_M: f32 = 7.0;

/// How much road an agent needs to itself when it is recycled.
///
/// Placement drew a line and then a distance along it with nothing looking at
/// where the other cars already were, so two of the pool landing on the same
/// street within a metre of each other was an ordinary outcome — one car drawn
/// twice, in the same paint, which reads as a rendering fault rather than as
/// traffic. A handful of redraws is enough: the map has far more road in range
/// than the pool has agents.
const SPAWN_GAP_M: f32 = 16.0;

/// A U-turn at a junction is a last resort. The cosine of the sharpest turn an
/// agent will take while anything straighter is on offer — 90°, so it will turn
/// a corner but will not double back unless the corner is all there is.
const TURN_BACK_COS: f32 = 0.0;

/// One agent's state while it crosses a junction.
///
/// Before this a hand-off was a **cut**: the line, the direction and the lateral
/// offset were all replaced between two frames, so at every junction the car's
/// nose snapped through the turn angle and its body jumped across the road by
/// the two lane offsets — and by up to [`HANDOFF_M`] more where the two lines do
/// not share a node. Roads meet at junctions constantly, so that was most of
/// what the motion looked like.
///
/// So the junction is *driven*. The exit pose of the old road and the entry pose
/// of the new one are the two ends of a cubic Hermite curve whose tangents are
/// the two travel directions, and the car follows it at a speed derived from how
/// tight it is. The curve is exact at both ends by construction — it starts
/// where the car is, pointing where it is pointing, and finishes on the new
/// line's own lane at its own heading — so there is nothing to pop at either
/// end. It is also the only place the layer models something a straight read of
/// the centre lines cannot express: the corner itself, which is not in the data.
#[derive(Clone, Copy)]
struct Turn {
    /// Where the agent leaves the old road, and the direction it leaves in.
    from: Vec2,
    from_dir: Vec2,
    /// Where it joins the new road, and the direction it joins in.
    to: Vec2,
    to_dir: Vec2,
    /// Tangent scale, and the length progress is measured against. Never smaller
    /// than the junction is wide, so two lines sharing a node still get a rounded
    /// corner rather than a pivot on the spot.
    len: f32,
    /// 0 at `from`, 1 at `to`.
    t: f32,
    /// Carriageway height at each end, so a turn off a tunnel ramp onto a
    /// surface street rises with the road instead of stepping at the node.
    from_y: f32,
    to_y: f32,
}

impl Turn {
    /// The carriageway height at the current `t`.
    ///
    /// Linear where the position is cubic, deliberately: the two roads meet at
    /// one node, so the height at each end is the same number both lines report
    /// and there is nothing between them to curve through.
    fn deck(&self) -> f32 {
        let t = self.t.clamp(0.0, 1.0);
        self.from_y + (self.to_y - self.from_y) * t
    }

    /// Position and unit heading at the current `t`.
    fn pose(&self) -> (Vec2, Vec2) {
        let (t, l) = (self.t.clamp(0.0, 1.0), self.len);
        let (t2, t3) = (t * t, t * t * t);
        let pos = self.from * (2.0 * t3 - 3.0 * t2 + 1.0)
            + self.from_dir * (l * (t3 - 2.0 * t2 + t))
            + self.to * (-2.0 * t3 + 3.0 * t2)
            + self.to_dir * (l * (t3 - t2));
        let tangent = self.from * (6.0 * t2 - 6.0 * t)
            + self.from_dir * (l * (3.0 * t2 - 4.0 * t + 1.0))
            + self.to * (-6.0 * t2 + 6.0 * t)
            + self.to_dir * (l * (3.0 * t2 - 2.0 * t));
        // A degenerate curve has no tangent to normalise; the two travel
        // directions are then the honest answer.
        let heading = tangent
            .try_normalize()
            .unwrap_or_else(|| self.from_dir.lerp(self.to_dir, t).normalize_or_zero());
        (pos, heading)
    }

    /// The speed this turn may be taken at, m/s.
    fn speed_limit(&self) -> f32 {
        corner_speed(corner_radius(self.from_dir, self.to_dir, self.len * 0.5))
    }
}

// ─── The network ─────────────────────────────────────────────────────────────

/// One drivable line placed in the anchored world-XZ frame.
pub struct WorldDriveLine {
    pub points: Vec<Vec2>,
    pub flow: Flow,
    pub speed: f32,
    pub half_width: f32,
    pub uses: Uses,
    /// Cumulative length at each point, so a car can be advanced without walking
    /// the line from the start every frame.
    pub cumulative: Vec<f32>,
    /// Carriageway height at each point through a structure below the ground,
    /// `NaN` on the ground — see [`cartopolis_geo::traffic::DriveLine::deck`].
    /// Parallel to `points`, and every operation on one must keep it so.
    pub deck: Vec<f32>,
    /// This way is flagged `bridge`, so its height comes from `DeckPlates` —
    /// which is measured, and so arrives some frames after the cell does.
    pub on_deck: bool,
}

impl WorldDriveLine {
    fn length(&self) -> f32 {
        self.cumulative.last().copied().unwrap_or(0.0)
    }

    /// Position and unit heading at `s` metres along the line.
    ///
    /// Clamped at both ends rather than wrapping: a car past the end is about to
    /// be handed off, and it should sit at the end until it is rather than
    /// teleporting to the start.
    fn at(&self, s: f32) -> (Vec2, Vec2) {
        let (p, d, _) = self.at_with_deck(s);
        (p, d)
    }

    /// [`at`](Self::at), and the carriageway height there.
    ///
    /// One walk rather than two: the interpolation that places the car is the
    /// same one that places its road, and doing them apart is how a deck and the
    /// thing standing on it come to disagree.
    fn at_with_deck(&self, s: f32) -> (Vec2, Vec2, f32) {
        let s = s.clamp(0.0, self.length());
        // Linear scan from the start. The lines are simplified to ~8 m steps and
        // capped in length, so this is a handful of comparisons; a binary search
        // would be more code for a loop that is already short.
        let mut i = 0;
        while i + 2 < self.points.len() && self.cumulative[i + 1] < s {
            i += 1;
        }
        let (a, b) = (self.points[i], self.points[i + 1]);
        let seg = (b - a).length().max(1e-4);
        let t = ((s - self.cumulative[i]) / seg).clamp(0.0, 1.0);
        // `NaN` off a structure, and so wherever either end is: a car on the
        // last stretch out of a ramp stands on the ground it is coming up to.
        let deck = match (self.deck.get(i), self.deck.get(i + 1)) {
            (Some(y0), Some(y1)) => y0 + (y1 - y0) * t,
            _ => f32::NAN,
        };
        (a.lerp(b, t), (b - a) / seg, deck)
    }

    /// Arc length of the point on this line closest to `p`.
    ///
    /// Used when an agent is recycled, so it is dropped onto the stretch of road
    /// that is actually in front of the camera rather than anywhere along a line
    /// that merely *reaches* the camera. Exact (a projection onto every segment)
    /// rather than nearest-vertex, because the difference is half a segment and
    /// the whole point of the call is to land inside a radius.
    fn nearest_along(&self, p: Vec2) -> f32 {
        let mut best = (f32::MAX, 0.0f32);
        for i in 0..self.points.len() - 1 {
            let (a, b) = (self.points[i], self.points[i + 1]);
            let ab = b - a;
            let len2 = ab.length_squared().max(1e-8);
            let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
            let d2 = (a + ab * t).distance_squared(p);
            if d2 < best.0 {
                best = (d2, self.cumulative[i] + t * len2.sqrt());
            }
        }
        best.1
    }

    /// The tightest bend within `ahead_m` of `along`, as the radius of the arc
    /// that would round it — [`f32::MAX`] on a stretch with no bend in it.
    ///
    /// A simplified centre line has no curvature, only kinks: two straight
    /// segments meeting at an angle. So the radius is the one an arc tangent to
    /// both would have if it were allowed to eat half of the shorter of them,
    /// `T / tan(θ/2)`. That is what turns "this street bends" into a number a
    /// speed can be derived from, which is what stops a car taking a right-angle
    /// corner at the speed limit.
    ///
    /// `dir` is the travel direction, because a car only has to slow for the
    /// bends it is about to reach.
    fn min_turn_radius(&self, along: f32, ahead_m: f32, dir: f32) -> f32 {
        let (lo, hi) = if dir > 0.0 {
            (along, along + ahead_m)
        } else {
            (along - ahead_m, along)
        };
        let mut best = f32::MAX;
        for i in 1..self.points.len().saturating_sub(1) {
            let c = self.cumulative[i];
            if c < lo || c > hi {
                continue;
            }
            let d_in = (self.points[i] - self.points[i - 1]).normalize_or_zero();
            let d_out = (self.points[i + 1] - self.points[i]).normalize_or_zero();
            let seg = (self.cumulative[i] - self.cumulative[i - 1])
                .min(self.cumulative[i + 1] - self.cumulative[i]);
            best = best.min(corner_radius(d_in, d_out, seg * 0.5));
        }
        best
    }
}

/// The radius of an arc tangent to two directions meeting at a point, given how
/// far back along each it may reach.
///
/// Shared by the in-line bends ([`WorldDriveLine::min_turn_radius`]) and the
/// junction turns ([`Turn`]) so both derive a cornering speed the same way — a
/// car that slows for a bend in a street and then takes the right-angle turn at
/// its end flat out would be worse than one that does neither.
fn corner_radius(d_in: Vec2, d_out: Vec2, tangent_m: f32) -> f32 {
    // The deflection: 0 straight on, π doubling back.
    let cos = d_in.dot(d_out).clamp(-1.0, 1.0);
    let theta = cos.acos();
    if theta < 1e-3 {
        return f32::MAX;
    }
    (tangent_m / (theta * 0.5).tan()).max(0.5)
}

/// The speed a bend of this radius may be taken at, m/s.
///
/// `v = √(a·r)` for the lateral acceleration a driver will accept. Uncapped at
/// the top — the road's own speed is the other bound and the caller takes the
/// smaller.
fn corner_speed(radius: f32) -> f32 {
    (CORNER_LAT_ACCEL * radius.min(1.0e4)).sqrt()
}

/// Every drivable line in the streamed world, keyed by the cell that supplied it
/// so a retired cell takes its roads with it — the same contract
/// `WorldPlaces` and `StreetLabelStore` hold, and for the same reason.
#[derive(Resource, Default)]
pub struct DriveNetwork {
    cells: HashMap<(i64, i64), Vec<WorldDriveLine>>,
    /// Bumped whenever the set of lines changes, so a car holding an index into
    /// a retired cell can notice rather than dereference it.
    pub revision: u32,
}

impl DriveNetwork {
    /// Place a finished cell's tile-local lines at the cell's world origin.
    pub fn insert_cell(&mut self, cell: (i64, i64), wx: f32, wz: f32, lines: Vec<DriveLine>) {
        let placed: Vec<WorldDriveLine> = lines
            .into_iter()
            .filter_map(|l| {
                let points: Vec<Vec2> = l
                    .points
                    .iter()
                    .map(|p| Vec2::new(wx + p[0], wz + p[1]))
                    .collect();
                if points.len() < 2 {
                    return None;
                }
                let mut cumulative = Vec::with_capacity(points.len());
                let mut run = 0.0;
                cumulative.push(0.0);
                for w in points.windows(2) {
                    run += (w[1] - w[0]).length();
                    cumulative.push(run);
                }
                // A line whose profile did not survive is flat rather than
                // wrong: a mismatched length here would index past the end on
                // whichever car reached that point.
                let deck = if l.deck.len() == points.len() {
                    l.deck
                } else {
                    vec![f32::NAN; points.len()]
                };
                (run > 1.0).then_some(WorldDriveLine {
                    points,
                    flow: l.flow,
                    speed: l.speed,
                    half_width: l.half_width,
                    uses: l.uses,
                    cumulative,
                    deck,
                    on_deck: l.on_deck,
                })
            })
            .collect();
        self.cells.insert(cell, placed);
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn remove_cell(&mut self, cell: &(i64, i64)) {
        if self.cells.remove(cell).is_some() {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn clear(&mut self) {
        if !self.cells.is_empty() {
            self.cells.clear();
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn get(&self, key: LineKey) -> Option<&WorldDriveLine> {
        self.cells.get(&key.cell)?.get(key.index)
    }

    fn is_empty(&self) -> bool {
        self.cells.values().all(Vec::is_empty)
    }

    /// Every line with a point within `radius` of `near`, as keys.
    fn near(&self, near: Vec2, radius: f32, kind: AgentKind) -> Vec<LineKey> {
        let r2 = radius * radius;
        let mut out = Vec::new();
        for (cell, lines) in &self.cells {
            for (index, l) in lines.iter().enumerate() {
                if !kind.may_use(l.uses) {
                    continue;
                }
                if l.points.iter().any(|p| p.distance_squared(near) <= r2) {
                    out.push(LineKey { cell: *cell, index });
                }
            }
        }
        out
    }
}

/// Which line a car is on. Not a reference: the cell it names can be retired
/// under it, and [`DriveNetwork::get`] answering `None` is how the car finds out.
///
/// `Hash` because the per-frame occupancy map (see [`drive_traffic`]) is keyed by
/// the line an agent is on: that is what lets a car see the one in front of it
/// and a recycled car avoid being dropped on top of one.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct LineKey {
    cell: (i64, i64),
    index: usize,
}

// ─── The cars ────────────────────────────────────────────────────────────────

/// What an agent is. The steering is identical for both — what differs is which
/// lines they may use, how fast they go, and where across the road they sit.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Car,
    /// A person on foot. Also stands in for a cyclist on a cycleway: at the range
    /// this is seen from the difference is a bicycle-sized silhouette, and
    /// inventing one would claim the tile knows which it is.
    Walker,
}

impl AgentKind {
    fn may_use(self, uses: Uses) -> bool {
        match self {
            Self::Car => uses.drive,
            Self::Walker => uses.walk,
        }
    }

    /// Pace in m/s. A car takes its road's speed; a walker keeps its own, because
    /// a pavement has no speed limit and nobody walks faster down a main road.
    fn speed(self, line: &WorldDriveLine) -> f32 {
        match self {
            Self::Car => line.speed,
            Self::Walker => WALK_SPEED,
        }
    }

    /// How fast this kind may gain and shed speed, m/s². A person's is one
    /// number in both directions; a car's brakes are stronger than its engine.
    fn accel(self) -> (f32, f32) {
        match self {
            Self::Car => (CAR_ACCEL, CAR_BRAKE),
            Self::Walker => (WALK_ACCEL, WALK_ACCEL),
        }
    }

    /// Whether this kind slows for the bend ahead and for whatever is in front
    /// of it. Both are about vehicles sharing a carriageway; a person on a
    /// pavement neither corners nor queues.
    fn drives(self) -> bool {
        matches!(self, Self::Car)
    }

    /// How much traffic of this kind a line of this sort carries, relative to a
    /// residential street. One of the two terms in [`weigh`].
    ///
    /// `drive_speed` is already this codebase's encoding of how important a road
    /// is, so a car's share follows it — but *softened*, because the raw ratio
    /// between a motorway and a service road is nearly six and a city where
    /// every side street is empty is as wrong as one where the main road is.
    /// People are the other way round: a pavement is a pavement, and a busy
    /// street is busy with pedestrians because it is long, not because it is
    /// fast.
    fn importance(self, line: &WorldDriveLine) -> f32 {
        match self {
            Self::Car => (line.speed / RESIDENTIAL_SPEED).max(0.2).powf(0.7),
            Self::Walker => 1.0,
        }
    }

    /// How much road this kind needs to itself when it is recycled, in metres.
    fn spawn_gap(self) -> f32 {
        match self {
            Self::Car => SPAWN_GAP_M,
            Self::Walker => SPAWN_GAP_M * 0.2,
        }
    }

    /// How far out from the camera this kind is placed, in metres.
    ///
    /// A person is a third of a car's size and so legible at roughly a third of
    /// the range; spreading the walker pool over the car's radius spends most of
    /// it where nobody can resolve a walker anyway, and leaves the pavement in
    /// front of you empty. Measured 2026-08-26: at one radius for both, 220
    /// walkers over a city-centre ring put none on the street in view, while the
    /// metrics happily reported all 220 placed.
    fn place_radius(self) -> f32 {
        match self {
            Self::Car => PLACE_RADIUS_M,
            Self::Walker => PLACE_RADIUS_M * 0.45,
        }
    }

    /// Where this kind is taken off its line and given a new one. Outside
    /// [`Self::place_radius`] by the same margin for both, so neither flickers at
    /// the boundary — see [`PLACE_RADIUS_M`].
    fn recycle_radius(self) -> f32 {
        self.place_radius() * (RECYCLE_RADIUS_M / PLACE_RADIUS_M)
    }

    /// How far right of the centre line, in metres.
    ///
    /// A car sits in its lane. A walker sits **outside** the carriageway, which is
    /// where the pavement is: the tile does not model pavements separately, so
    /// this offset is the whole of what puts a person beside a street rather than
    /// down the middle of it.
    fn offset(self, line: &WorldDriveLine) -> f32 {
        match self {
            Self::Car => line.half_width * LANE_FRACTION,
            Self::Walker => line.half_width + PAVEMENT_CLEARANCE_M,
        }
    }
}

/// A person's pace, m/s. A stroll rather than the avatar's purposeful walk —
/// ambient people are not going anywhere in particular.
const WALK_SPEED: f32 = 1.25;

/// How far beyond the kerb a walker's line sits.
const PAVEMENT_CLEARANCE_M: f32 = 1.6;

/// One agent in a pool. The entity exists for the life of the process; every
/// field here is rewritten when it is recycled onto a new line.
#[derive(Component)]
pub struct Car {
    line: Option<LineKey>,
    /// Metres travelled along the line, always increasing.
    s: f32,
    /// `+1` if driving the line in its own order, `-1` against it.
    dir: f32,
    /// Stable per-car jitter, so a street's cars are not all doing exactly the
    /// speed limit in lockstep.
    speed_scale: f32,
    /// Metres per second **now**, eased toward whatever the road, the bend ahead
    /// and the car in front allow. The road's speed used to be written straight
    /// into the step, which is why a car crossing from a side street onto a
    /// trunk road changed speed by a factor of three in one frame.
    speed: f32,
    /// The drawn heading — the path's, rate-limited. See [`TURN_RATE_PER_MS`].
    /// Zero until the agent is first placed, which is how the first frame after
    /// a placement knows to adopt the path's heading outright rather than
    /// swinging round to it from due east.
    heading: Vec2,
    /// Set while the agent is crossing a junction. See [`Turn`].
    turn: Option<Turn>,
}

/// Deterministic unit random from three integers. The same shape as the geo
/// crate's, kept local so this file does not reach across for four lines.
fn rand01(seed: u64, a: u64, b: u64) -> f32 {
    let mut h =
        seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    (h >> 40) as f32 / 16_777_216.0
}

/// Build the one car mesh and the shared paints, and spawn the pool.
///
/// Runs once. The cars start with no line and are placed by
/// [`drive_traffic`] on the first frame there is a network to place them on —
/// which is not the first frame of the app, since the roads stream in.
pub fn setup_traffic(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    roots: Query<Entity, With<BigSpace>>,
) {
    let Ok(root) = roots.single() else {
        // Ordered `.after(setup_camera)` in `nav::plugin`, which is what spawns the
        // root — so this is unreachable, and it says so rather than returning in
        // silence. Silence is exactly how this went unnoticed the first time.
        tracing::warn!("no BigSpace root at traffic setup; ambient traffic is off for this run");
        return;
    };
    let mesh = meshes.add(car_mesh());
    let paints: Vec<Handle<StandardMaterial>> = cartopolis_geo::parking::CAR_PAINTS
        .iter()
        .map(|&rgb| {
            let [r, g, b] = crate::systems::scatter::srgb_to_linear_rgb(rgb);
            materials.add(StandardMaterial {
                base_color: Color::linear_rgb(r, g, b),
                perceptual_roughness: 0.45,
                ..default()
            })
        })
        .collect();

    let walker_mesh = meshes.add(walker_mesh());
    let coats: Vec<Handle<StandardMaterial>> = COAT_COLOURS
        .iter()
        .map(|&rgb| {
            let [r, g, b] = crate::systems::scatter::srgb_to_linear_rgb(rgb);
            materials.add(StandardMaterial {
                base_color: Color::linear_rgb(r, g, b),
                perceptual_roughness: 0.85,
                ..default()
            })
        })
        .collect();

    let mut spawn_pool = |count: usize, kind: AgentKind, seed: u64| {
        for i in 0..count {
            let (m, mat) = match kind {
                AgentKind::Car => (mesh.clone(), paints[i % paints.len()].clone()),
                AgentKind::Walker => (walker_mesh.clone(), coats[i % coats.len()].clone()),
            };
            commands.spawn((
                Mesh3d(m),
                MeshMaterial3d(mat),
                // Off-screen until placed. An agent with no line must not be
                // visible at the origin, which is where the avatar stands.
                Visibility::Hidden,
                Transform::default(),
                GridCell::default(),
                ChildOf(root),
                HideInSpace,
                kind,
                Car {
                    line: None,
                    s: 0.0,
                    dir: 1.0,
                    speed_scale: 0.85 + rand01(seed, i as u64, 7) * 0.3,
                    speed: 0.0,
                    heading: Vec2::ZERO,
                    turn: None,
                },
            ));
        }
    };
    spawn_pool(MAX_CARS, AgentKind::Car, 0x0CA2);
    spawn_pool(MAX_WALKERS, AgentKind::Walker, 0x0FEE);
}

/// Muted outerwear. The same judgement the parked cars' palette makes: a crowd in
/// primary colours is the loudest thing on a pale cartographic street.
const COAT_COLOURS: &[u32] = &[
    0x4A_4F_57, 0x6B_5E_52, 0x3E_4A_52, 0x7A_6E_63, 0x5A_4E_58, 0x86_7C_6E,
];

/// How many people exist, on the same fixed-pool terms as the cars.
///
/// More than the cars because a city has more people than vehicles in view and
/// they are a fifth of the geometry each — three boxes against seven.
pub const MAX_WALKERS: usize = 220;

/// The one mesh every walker draws: a body, a head and a pair of legs.
///
/// Three boxes and no animation. A figure this size at the range the layer is
/// seen from is a silhouette in motion, and motion is the whole signal — the
/// avatar's own locomotion work (`docs/notes/avatar-game-feel.md`) is about a
/// model you stand behind, which this is not. What it must not do is read as a
/// bollard, which is why the head and the leg gap are there at all.
fn walker_mesh() -> Mesh {
    use cartopolis_geo::furniture::{FurnitureInstance, FurnitureKind};
    let items = [FurnitureInstance {
        x: 0.0,
        z: 0.0,
        yaw: 0.0,
        kind: FurnitureKind::Person,
        luma: 1.0,
        paint: cartopolis_geo::vector_tiles::palette::FURNITURE,
        over_water: false,
    }];
    let meshes = cartopolis_geo::furniture::build_furniture_meshes(&items, None);
    let sm = meshes
        .into_iter()
        .next()
        .expect("the person form has parts");
    surface_mesh_to_bevy(&sm)
}

/// Advance every car, and recycle the ones that have run out of road.
#[allow(clippy::too_many_arguments)]
pub fn drive_traffic(
    time: Res<Time>,
    net: Res<DriveNetwork>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    decks: Res<crate::systems::map_geometry::DeckPlates>,
    camera: Query<(&GridCell, &Transform), With<FreeCamera>>,
    grids: Query<&Grid, With<BigSpace>>,
    mut cars: Query<
        (
            &mut Car,
            &AgentKind,
            &mut Transform,
            &mut GridCell,
            &mut Visibility,
        ),
        Without<FreeCamera>,
    >,
    mut frame: Local<u64>,
) {
    // One toggle per pool (Layers ▸ Life ▸ Traffic / People). A pool that is off
    // is hidden and then skipped entirely — it holds its line and its distance
    // along it, so switching it back on resumes rather than re-places, and a
    // hidden agent costs neither the scan nor the transform write.
    let show_cars = layers.cars_visible();
    let show_people = layers.people_visible();
    if (!show_cars && !show_people) || net.is_empty() {
        for (.., mut vis) in &mut cars {
            *vis = Visibility::Hidden;
        }
        return;
    }
    let showing = |kind: AgentKind| match kind {
        AgentKind::Car => show_cars,
        AgentKind::Walker => show_people,
    };
    let (Ok((cam_cell, cam_tf)), Ok(grid)) = (camera.single(), grids.single()) else {
        return;
    };
    // The camera's absolute world XZ, which is the frame the network is in.
    let cam = Vec2::new(
        cam_cell.x as f32 * WORLD_CELL_EDGE + cam_tf.translation.x,
        cam_cell.z as f32 * WORLD_CELL_EDGE + cam_tf.translation.z,
    );
    let too_high = cam_tf.translation.y > TRAFFIC_HIDE_ALT;
    if too_high {
        for (.., mut vis) in &mut cars {
            *vis = Visibility::Hidden;
        }
        return;
    }

    *frame = frame.wrapping_add(1);
    // Candidate roads are gathered once per frame, not once per car: the scan is
    // over the loaded ring and doing it ninety-six times would be the whole
    // budget for this layer.
    // One list per pool: a walker offered a motorway would reject it on every
    // recycle, and the two want different answers out of the same ring.
    // Only for a pool that is on: this is a scan of every line in the loaded
    // ring, and running it for a hidden pool is the whole per-frame cost of a
    // layer that was switched off.
    let roads = if show_cars {
        net.near(cam, AgentKind::Car.place_radius(), AgentKind::Car)
    } else {
        Vec::new()
    };
    let paths = if show_people {
        net.near(cam, AgentKind::Walker.place_radius(), AgentKind::Walker)
    } else {
        Vec::new()
    };
    if roads.is_empty() && paths.is_empty() {
        return;
    }
    // How much of the pool each candidate deserves — see `weigh`. Built once per
    // frame beside the candidate lists themselves, for the same reason.
    let road_weights = weigh(&net, &roads, AgentKind::Car);
    let path_weights = weigh(&net, &paths, AgentKind::Walker);

    // Where every placed agent is, keyed by its line **and** the direction it is
    // travelling, as the distance it has run along that line. Two things read
    // it: a car looks up whatever is in front of it (`headway_speed`), and a
    // recycled agent avoids landing on top of one. Both used to be missing, and
    // both are visible: a fast car overtook a slow one *through* it, and two of
    // the pool on one street a metre apart drew one car twice.
    //
    // It is a read-only pass over the same query — one hash insert per placed
    // agent, no allocation per frame beyond the map itself.
    let mut occupied: HashMap<(LineKey, i8), Vec<f32>> = HashMap::new();
    for (car, &kind, ..) in cars.iter() {
        if !showing(kind) {
            continue;
        }
        if let Some(key) = car.line {
            occupied
                .entry((key, lane_key(car.dir)))
                .or_default()
                .push(car.s);
        }
    }

    let dt = time.delta_secs();

    for (i, (mut car, &kind, mut tf, mut cell, mut vis)) in cars.iter_mut().enumerate() {
        if !showing(kind) {
            *vis = Visibility::Hidden;
            continue;
        }
        let (candidates, weights): (&[LineKey], &[f32]) = match kind {
            AgentKind::Car => (&roads, &road_weights),
            AgentKind::Walker => (&paths, &path_weights),
        };
        if candidates.is_empty() {
            *vis = Visibility::Hidden;
            continue;
        }
        let mut key = car.line;
        let mut line = key.and_then(|k| net.get(k));

        // ── Advance ──────────────────────────────────────────────────────────
        //
        // Speed is *eased*, never assigned. What it is eased toward is the least
        // of what the road allows, what the bend ahead allows and what the car
        // in front allows — so a car pulls away from a junction, slows into a
        // corner and sits behind a slower one instead of stepping between three
        // constants as it changes road.
        if let (Some(k), Some(l)) = (key, line) {
            let along = if car.dir > 0.0 {
                car.s
            } else {
                l.length() - car.s
            };
            let mut want = kind.speed(l) * car.speed_scale;
            if kind.drives() {
                want = want.min(corner_speed(l.min_turn_radius(
                    along,
                    CORNER_LOOKAHEAD_M,
                    car.dir,
                )));
                if let Some(t) = &car.turn {
                    want = want.min(t.speed_limit());
                }
                want = want.min(headway_speed(&occupied, k, car.dir, car.s, want));
            }
            let (accel, brake) = kind.accel();
            let rate = if want > car.speed { accel } else { brake };
            car.speed = (car.speed + (want - car.speed).clamp(-rate * dt, rate * dt)).max(0.0);

            // Inside a junction the progress is the turn's, not the line's.
            let step = car.speed * dt;
            let mut done = false;
            if let Some(t) = car.turn.as_mut() {
                t.t += step / t.len.max(0.5);
                done = t.t >= 1.0;
            } else {
                car.s += step;
            }
            if done {
                car.turn = None;
            }
        }

        // ── Where that leaves it ─────────────────────────────────────────────
        let mut pose = pose_of(&car, line, kind);
        if pose.is_some_and(|(p, _, _)| p.distance(cam) > kind.recycle_radius()) {
            key = None;
            line = None;
            car.turn = None;
        }

        // ── The end of the road ──────────────────────────────────────────────
        //
        // Drive **onto a connected one** rather than vanishing: without this an
        // agent pops out of existence at every junction and reappears elsewhere,
        // which is more visible than the traffic itself. The crossing itself is
        // a `Turn` — see its note for why the hand-off cannot simply be a swap.
        if car.turn.is_none() {
            if let (Some(k), Some(l), Some((p, travel, from_y))) = (key, line, pose) {
                if car.s > l.length() {
                    match hand_off(&net, candidates, k, p, travel, kind) {
                        Some((next, next_dir)) => {
                            let nl = net.get(next).expect("hand_off only offers lines it read");
                            let entry = if next_dir > 0.0 { 0.0 } else { nl.length() };
                            let (centre, heading, to_y) = nl.at_with_deck(entry);
                            let to_dir = heading * next_dir;
                            let to = centre + right_of(to_dir) * kind.offset(nl);
                            // The corner is not in the data: two centre lines
                            // meet at a *point*, and how far the vehicle swings
                            // getting from one to the other is set by how wide
                            // the two roads are. Where the lines do not share a
                            // node the gap between them is wider still, and then
                            // that is the arc.
                            let span =
                                (l.half_width + nl.half_width + 2.0).max(p.distance(to) * 1.1);
                            car.turn = Some(Turn {
                                from: p,
                                from_dir: travel,
                                to,
                                to_dir,
                                len: span,
                                t: 0.0,
                                from_y,
                                to_y,
                            });
                            car.line = Some(next);
                            car.dir = next_dir;
                            car.s = 0.0;
                            key = Some(next);
                            line = Some(nl);
                        }
                        None => {
                            key = None;
                            line = None;
                        }
                    }
                }
            }
        }

        // ── Recycling ────────────────────────────────────────────────────────
        if line.is_none() {
            match place_agent(
                &net, candidates, weights, cam, kind, &occupied, i as u64, *frame,
            ) {
                Some((k, dir, s)) => {
                    let l = net.get(k).expect("place_agent only offers lines it read");
                    car.line = Some(k);
                    car.dir = dir;
                    car.s = s;
                    car.turn = None;
                    // A recycled agent is somewhere else entirely; it must adopt
                    // its new heading outright rather than swinging round to it
                    // from the one it had on the far side of the city.
                    car.heading = Vec2::ZERO;
                    // Not from rest — a car dropped into moving traffic is
                    // already moving — but not at the limit either, so it is
                    // seen to pick up rather than to appear at speed.
                    car.speed = kind.speed(l) * car.speed_scale * 0.7;
                    // Record it, so the next agent recycled on this same frame
                    // does not choose the spot that has just been taken.
                    occupied.entry((k, lane_key(dir))).or_default().push(s);
                    key = Some(k);
                    line = Some(l);
                }
                None => {
                    *vis = Visibility::Hidden;
                    continue;
                }
            }
            pose = pose_of(&car, line, kind);
        }

        let (Some(_), Some((pos, travel, deck))) = (key, pose) else {
            *vis = Visibility::Hidden;
            continue;
        };
        let on_deck = line.is_some_and(|l| l.on_deck);

        // ── Draw ─────────────────────────────────────────────────────────────
        //
        // The drawn heading is not the path's tangent but a rate-limited chase
        // of it: a simplified centre line changes direction in steps, and a
        // vehicle cannot. See `TURN_RATE_PER_MS`.
        car.heading = if car.heading == Vec2::ZERO {
            travel
        } else {
            let rate = (TURN_RATE_PER_MS * car.speed).max(TURN_RATE_FLOOR);
            rotate_toward(car.heading, travel, rate * dt)
        };
        let facing = car.heading;

        // **Under the ground where the road is under the ground.** `deck` is
        // this line's own carriageway height, resolved per feature, so a street
        // crossing over the ring keeps its traffic on the bridge rather than
        // sampling the tunnel beneath it — and it is the height the structure is
        // drawn at (`tunnels::SunkenWay`), so a car on it needs nothing added.
        let ground = terrain.ground_at(pos.x, pos.y);
        // …and **over** it where the road is over it. A deck's height is
        // measured rather than tessellated, so it cannot travel on the line the
        // way the tunnel depth does; what travels is `on_deck`, and that is the
        // half that matters — `lift_at` answers by position, and a viaduct and
        // the street under it are at the same position. Asking only for a line
        // the tile flagged `bridge` is what keeps the underpass underneath.
        let lift = if on_deck { decks.lift_at(pos) } else { 0.0 };
        // On a deck, the deck **as drawn**: each is now placed at its own
        // measured top, and ground + clearance put cars in or over a deck by the
        // difference between the two (bridge research, 2026-09-23).
        let y = match on_deck.then(|| decks.top_at(pos)).flatten() {
            Some(top) => top + CAR_RIDE_HEIGHT,
            None if deck.is_finite() => deck + CAR_RIDE_HEIGHT,
            None => ground + lift + CAR_RIDE_HEIGHT,
        };
        let (grid_cell, local) = grid.translation_to_grid(Vec3::new(pos.x, y, pos.y).as_dvec3());
        *cell = grid_cell;
        tf.translation = local;
        // The model's +z is its nose (`cartopolis_geo::furniture`'s CAR), so the
        // yaw that points +z along the travel direction is measured from +z.
        let yaw = Quat::from_rotation_y(facing.x.atan2(facing.y));
        // A vehicle lies **on** the road, and over relief the road is not level:
        // a yaw-only car keeps its wheels in the horizontal plane, so it sinks
        // into every rise and hangs off every crest. The height was already
        // sampled from the same field; the slope is one more read of it
        // (`TerrainSnapshot::normal_at` is `Vec3::Y` wherever the field is off or
        // absent, so this costs nothing outside coverage). People stay upright.
        tf.rotation = if kind.drives() && terrain.is_ready() {
            Quat::from_rotation_arc(Vec3::Y, terrain.normal_at(pos.x, pos.y)) * yaw
        } else {
            yaw
        };
        *vis = Visibility::Inherited;
    }
}

/// Right of a travel direction, in the XZ plane where +x is east and +z is
/// south: rotating the heading a quarter turn one way gives the kerb side.
/// Right-hand traffic — see [`LANE_FRACTION`].
fn right_of(travel: Vec2) -> Vec2 {
    Vec2::new(-travel.y, travel.x)
}

/// Which side of a line an agent's occupancy entry belongs to. Two agents can
/// only be in each other's way when they are going the same way.
fn lane_key(dir: f32) -> i8 {
    if dir > 0.0 {
        1
    } else {
        -1
    }
}

/// Where an agent is and which way it is pointing: on its line, or on the curve
/// across the junction it is in the middle of.
fn pose_of(car: &Car, line: Option<&WorldDriveLine>, kind: AgentKind) -> Option<(Vec2, Vec2, f32)> {
    let l = line?;
    if let Some(t) = &car.turn {
        let (p, h) = t.pose();
        return Some((p, h, t.deck()));
    }
    let along = if car.dir > 0.0 {
        car.s
    } else {
        l.length() - car.s
    };
    let (centre, heading, deck) = l.at_with_deck(along);
    let travel = heading * car.dir;
    Some((centre + right_of(travel) * kind.offset(l), travel, deck))
}

/// Turn `from` toward `to` by at most `max_rad`.
fn rotate_toward(from: Vec2, to: Vec2, max_rad: f32) -> Vec2 {
    let cross = from.x * to.y - from.y * to.x;
    let angle = cross.atan2(from.dot(to)).clamp(-max_rad, max_rad);
    let (s, c) = angle.sin_cos();
    Vec2::new(from.x * c - from.y * s, from.x * s + from.y * c)
        .try_normalize()
        .unwrap_or(to)
}

/// What speed the agent in front leaves this one. See [`HEADWAY_M`].
fn headway_speed(
    occupied: &HashMap<(LineKey, i8), Vec<f32>>,
    key: LineKey,
    dir: f32,
    s: f32,
    want: f32,
) -> f32 {
    let Some(others) = occupied.get(&(key, lane_key(dir))) else {
        return want;
    };
    let gap = others
        .iter()
        .copied()
        .filter(|o| *o > s + 1e-3)
        .fold(f32::MAX, f32::min)
        - s;
    if gap >= HEADWAY_M {
        return want;
    }
    want * ((gap - MIN_HEADWAY_M) / (HEADWAY_M - MIN_HEADWAY_M)).clamp(0.0, 1.0)
}

/// The road a residential street runs at, m/s — the unit
/// [`AgentKind::importance`] measures the others against.
const RESIDENTIAL_SPEED: f32 = 7.5;

/// Cumulative placement weights for a pool's candidate lines, one entry per
/// candidate.
///
/// The old rule was **three uniform draws, keep the fastest**, and it is worth
/// saying what was wrong with it because both halves were: a uniform draw makes
/// a 30 m service stub as likely as a 400 m avenue, so most of the pool ended up
/// on back lanes; and keeping the maximum of three draws over a speed range of
/// 4.5 to 26 m/s is very nearly *deterministic*, so once a trunk road was in
/// range almost every recycled car went onto it and the ordinary streets emptied
/// out. Neither is what "spread over the traffic a city carries" means.
///
/// So a line's share is how much road it contributes — its length, capped at the
/// diameter of the placement disc, since nothing past that is in range — times
/// how much traffic a road of its class carries. That is a *distribution* rather
/// than an argmax: a motorway takes several times a back lane's share and does
/// not take all of it.
fn weigh(net: &DriveNetwork, keys: &[LineKey], kind: AgentKind) -> Vec<f32> {
    let cap = kind.place_radius() * 2.0;
    let mut cum = Vec::with_capacity(keys.len());
    let mut run = 0.0;
    for k in keys {
        run += net
            .get(*k)
            .map_or(0.0, |l| l.length().min(cap) * kind.importance(l));
        cum.push(run);
    }
    cum
}

/// Draw one line from `keys` in proportion to [`weigh`]'s weights.
fn draw_line(keys: &[LineKey], cum: &[f32], r: f32) -> Option<LineKey> {
    let total = *cum.last()?;
    if !(total > 0.0) {
        return keys.first().copied();
    }
    let target = r.clamp(0.0, 0.999_999) * total;
    let i = cum.partition_point(|c| *c <= target).min(keys.len() - 1);
    keys.get(i).copied()
}

/// How many times placement will redraw to find a spot with nobody already on
/// it. A handful is enough: the map holds far more road in range than the pool
/// holds agents, so a rejection is rare and a second rejection rarer.
const PLACE_ATTEMPTS: u64 = 4;

/// Choose a line, a direction and a distance along it for a recycled agent.
///
/// Returns `(line, dir, s)` where `s` is measured the way [`Car::s`] is — from
/// the end the agent enters by.
///
/// Two things it does that the old placement did not. It lands the agent on the
/// stretch of the chosen road that is **in front of the camera**: the candidate
/// list holds every line with *any* point in range, so a 900 m avenue clipping
/// the edge of the disc used to receive cars anywhere along its whole length,
/// most of them outside the recycle radius and thrown away again on the next
/// frame. And it refuses a spot another agent is already occupying, which is
/// what stops two of one pool being drawn a metre apart in the same paint.
#[allow(clippy::too_many_arguments)]
fn place_agent(
    net: &DriveNetwork,
    keys: &[LineKey],
    cum: &[f32],
    cam: Vec2,
    kind: AgentKind,
    occupied: &HashMap<(LineKey, i8), Vec<f32>>,
    agent: u64,
    frame: u64,
) -> Option<(LineKey, f32, f32)> {
    let mut fallback = None;
    for attempt in 0..PLACE_ATTEMPTS {
        // The `frame` term is what stops every agent that runs out on the same
        // frame picking the same road.
        let seed = frame.wrapping_mul(PLACE_ATTEMPTS).wrapping_add(attempt);
        let key = draw_line(keys, cum, rand01(0xD1CE, agent, seed))?;
        let Some(line) = net.get(key) else {
            continue;
        };
        let dir = match (kind, line.flow) {
            // A pavement is two-way whatever the carriageway beside it does —
            // which is what somebody walking up a one-way street is doing.
            (AgentKind::Walker, _) | (_, Flow::Both) => {
                if rand01(0xB1D1, agent, seed) < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            (_, Flow::Forward) => 1.0,
            (_, Flow::Backward) => -1.0,
        };
        // Around the point on this line nearest the camera, so the agent lands
        // in the neighbourhood the pool is sized for rather than anywhere along
        // a line that merely reaches into it.
        let focus = line.nearest_along(cam);
        let spread = kind.place_radius() * 0.7;
        let mut along =
            (focus + (rand01(0x5EED, agent, seed) - 0.5) * 2.0 * spread).clamp(0.0, line.length());
        if line.at(along).0.distance(cam) > kind.place_radius() {
            // A line qualifies for the candidate list by having *any* point in
            // range, so a road running away from the camera has stretches that
            // do not — and one of those would be recycled on the next frame
            // without ever being seen. The nearest point is in range by
            // construction; take that instead.
            along = focus;
        }
        let s = if dir > 0.0 {
            along
        } else {
            line.length() - along
        };
        let clear = occupied
            .get(&(key, lane_key(dir)))
            .is_none_or(|v| v.iter().all(|o| (o - s).abs() > kind.spawn_gap()));
        if clear {
            return Some((key, dir, s));
        }
        fallback.get_or_insert((key, dir, s));
    }
    // Every draw was taken. Somewhere occupied beats nowhere: an agent with no
    // line is hidden, and a hidden agent is one the pool has lost.
    fallback
}

/// How far the car body's origin sits above the road surface.
///
/// The shared mesh is built in the same frame `furniture::form::CAR` is — origin
/// on the ground between the wheels — so this is only the clearance that keeps it
/// off a coplanar fight with the road it is standing on. The road ribbons win
/// their own depth fight through `systems::road_depth`; a car is ordinary
/// geometry and just needs to be above them.
const CAR_RIDE_HEIGHT: f32 = 0.02;

/// The one mesh every car draws.
///
/// Built from the *same* `furniture::form::CAR` parts the parked cars use, so a
/// car waiting at a kerb and a car driving past it are the same object. The paint
/// is the material here rather than a vertex colour, because a shared mesh cannot
/// carry a per-instance tint — which is the one difference between the two.
fn car_mesh() -> Mesh {
    use cartopolis_geo::furniture::{parts, FurnitureKind};
    let items = [cartopolis_geo::furniture::FurnitureInstance {
        x: 0.0,
        z: 0.0,
        yaw: 0.0,
        kind: FurnitureKind::Car,
        luma: 1.0,
        paint: cartopolis_geo::vector_tiles::palette::FURNITURE,
        over_water: false,
    }];
    debug_assert!(!parts(FurnitureKind::Car).is_empty());
    let meshes = cartopolis_geo::furniture::build_furniture_meshes(&items, None);
    let sm = meshes.into_iter().next().expect("the car form has parts");
    surface_mesh_to_bevy(&sm)
}

/// One `SurfaceMesh` as a bevy `Mesh`.
///
/// Local rather than reaching into `map_geometry::build_mesh`, which also folds in
/// the terrain refinement a cell's ground needs and an agent's model does not.
///
/// The form's own vertex colours carry the parts that are *not* the body — glass,
/// tyres, a walker's head — apart from it. The body colour is the material, which
/// is the one thing a shared mesh cannot vary per instance.
fn surface_mesh_to_bevy(sm: &cartopolis_geo::vector_tiles::SurfaceMesh) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::mesh::{Indices, PrimitiveTopology};
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, sm.positions.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, sm.normals.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, sm.colors.clone());
    mesh.insert_indices(Indices::U32(sm.indices.clone()));
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(points: &[[f32; 2]], flow: Flow) -> DriveLine {
        DriveLine {
            points: points.to_vec(),
            flow,
            speed: 10.0,
            half_width: 3.0,
            uses: Uses {
                drive: true,
                walk: true,
            },
            deck: vec![f32::NAN; points.len()],
            on_deck: false,
        }
    }

    fn net_with(points: &[[f32; 2]]) -> DriveNetwork {
        let mut net = DriveNetwork::default();
        net.insert_cell((0, 0), 0.0, 0.0, vec![line(points, Flow::Both)]);
        net
    }

    /// A cell's lines land at the cell's own origin, and carry a running length —
    /// which is what lets a car be advanced without walking the line each frame.
    #[test]
    fn a_cell_places_its_roads_and_measures_them() {
        let mut net = DriveNetwork::default();
        net.insert_cell(
            (1, 2),
            1000.0,
            -500.0,
            vec![line(&[[0.0, 0.0], [30.0, 0.0], [30.0, 40.0]], Flow::Both)],
        );
        let l = net
            .get(LineKey {
                cell: (1, 2),
                index: 0,
            })
            .expect("the road is where the cell put it");
        assert_eq!(l.points[0], Vec2::new(1000.0, -500.0));
        assert!((l.length() - 70.0).abs() < 1e-3, "length {}", l.length());
    }

    /// Walking a line must give a position that advances monotonically and a
    /// heading that is a unit vector — a car steers by both.
    #[test]
    fn walking_a_line_advances_and_keeps_a_unit_heading() {
        let net = net_with(&[[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]);
        let l = net
            .get(LineKey {
                cell: (0, 0),
                index: 0,
            })
            .expect("one road");
        let mut last = f32::MIN;
        for step in 0..=20 {
            let s = step as f32 * 10.0;
            let (pos, dir) = l.at(s);
            assert!((dir.length() - 1.0).abs() < 1e-3, "heading not unit: {dir}");
            let travelled = pos.x + pos.y;
            assert!(travelled >= last - 1e-3, "went backwards at {s}");
            last = travelled;
        }
    }

    /// Past the end the walk clamps rather than wrapping. A car about to be handed
    /// off should sit at the end of its road, not teleport to the start of it.
    #[test]
    fn walking_past_the_end_clamps() {
        let net = net_with(&[[0.0, 0.0], [50.0, 0.0]]);
        let l = net
            .get(LineKey {
                cell: (0, 0),
                index: 0,
            })
            .expect("one road");
        let (end, _) = l.at(l.length());
        let (past, _) = l.at(l.length() * 4.0);
        assert_eq!(end, past);
        let (start, _) = l.at(-20.0);
        assert_eq!(start, l.points[0]);
    }

    /// Retiring a cell takes its roads with it, and a car holding a key into it
    /// gets `None` rather than a stale road — which is the whole reason a car
    /// stores a key and not a reference.
    #[test]
    fn retiring_a_cell_invalidates_its_keys() {
        let mut net = net_with(&[[0.0, 0.0], [80.0, 0.0]]);
        let key = LineKey {
            cell: (0, 0),
            index: 0,
        };
        assert!(net.get(key).is_some());
        let before = net.revision;
        net.remove_cell(&(0, 0));
        assert!(net.get(key).is_none(), "a retired road must not answer");
        assert_ne!(net.revision, before, "consumers must be told");
    }

    /// `near` has to find roads by any of their points, not only their start — a
    /// long road whose far end is beside the camera is a road beside the camera.
    #[test]
    fn near_finds_a_road_by_its_far_end() {
        let net = net_with(&[[0.0, 0.0], [500.0, 0.0]]);
        assert_eq!(
            net.near(Vec2::new(495.0, 0.0), 30.0, AgentKind::Car).len(),
            1
        );
        assert_eq!(
            net.near(Vec2::new(250.0, 400.0), 30.0, AgentKind::Car)
                .len(),
            0
        );
    }

    /// The two pools must not see each other's network. A footway is not a road
    /// and a motorway is not a pavement; `near` filters by use so a recycled agent
    /// is never offered a line it may not travel.
    #[test]
    fn each_pool_only_sees_its_own_network() {
        let mut net = DriveNetwork::default();
        let mut road = line(&[[0.0, 0.0], [100.0, 0.0]], Flow::Both);
        road.uses = Uses {
            drive: true,
            walk: false,
        };
        let mut path = line(&[[0.0, 50.0], [100.0, 50.0]], Flow::Both);
        path.uses = Uses {
            drive: false,
            walk: true,
        };
        net.insert_cell((0, 0), 0.0, 0.0, vec![road, path]);

        let here = Vec2::new(50.0, 25.0);
        assert_eq!(net.near(here, 60.0, AgentKind::Car).len(), 1, "one road");
        assert_eq!(net.near(here, 60.0, AgentKind::Walker).len(), 1, "one path");
        // …and they are not the same line.
        assert_ne!(
            net.near(here, 60.0, AgentKind::Car)[0].index,
            net.near(here, 60.0, AgentKind::Walker)[0].index
        );
    }

    /// A walker keeps to the pavement, which is *outside* the carriageway — the
    /// offset is the whole of what stops a crowd walking down the middle of the
    /// road, since the tile does not model pavements at all.
    #[test]
    fn a_walker_stands_outside_the_carriageway_and_a_car_inside_it() {
        let net = net_with(&[[0.0, 0.0], [100.0, 0.0]]);
        let l = net
            .get(LineKey {
                cell: (0, 0),
                index: 0,
            })
            .expect("one line");
        let car = AgentKind::Car.offset(l);
        let walker = AgentKind::Walker.offset(l);
        assert!(car < l.half_width, "a car drives inside its own kerb");
        assert!(walker > l.half_width, "a walker stands beyond it");
    }

    /// A person walks; a car drives at the road's speed. The pace is not the same
    /// number and a walker must not inherit the carriageway's.
    #[test]
    fn a_walker_keeps_its_own_pace() {
        let net = net_with(&[[0.0, 0.0], [100.0, 0.0]]);
        let l = net
            .get(LineKey {
                cell: (0, 0),
                index: 0,
            })
            .expect("one line");
        assert_eq!(AgentKind::Car.speed(l), l.speed);
        assert_eq!(AgentKind::Walker.speed(l), WALK_SPEED);
        assert!(WALK_SPEED < l.speed, "walking is slower than driving");
    }

    /// The place and recycle radii must not be the same number, or a car sitting
    /// on the boundary is recycled and immediately re-placed on the wrong side of
    /// it, every frame.
    #[test]
    fn the_recycle_radius_is_outside_the_place_radius() {
        assert!(RECYCLE_RADIUS_M > PLACE_RADIUS_M * 1.2);
    }

    // ── Trajectory ───────────────────────────────────────────────────────────

    /// A tight corner has to be taken slower than a gentle one, and a straight
    /// road must impose no limit at all — otherwise the whole cornering model is
    /// a constant slow-down applied everywhere.
    #[test]
    fn a_tighter_corner_is_a_slower_one() {
        let straight = corner_radius(Vec2::X, Vec2::X, 5.0);
        let gentle = corner_radius(Vec2::X, Vec2::new(0.95, 0.31).normalize(), 5.0);
        let square = corner_radius(Vec2::X, Vec2::Y, 5.0);
        assert_eq!(straight, f32::MAX, "a straight road bends nowhere");
        assert!(gentle > square, "{gentle} should be wider than {square}");
        assert!(
            corner_speed(square) < corner_speed(gentle),
            "a right angle must be slower than a bend"
        );
        // …and a motorway's speed must survive a motorway's curves: a 300 m
        // radius may not be a reason to brake.
        assert!(corner_speed(300.0) > 26.0);
    }

    /// The bend a car slows for is the one **in front of it**. A car reads its
    /// line in whichever direction it drives it, so the same vertex is a reason
    /// to brake for one of them and already behind the other.
    #[test]
    fn only_the_bend_ahead_is_a_reason_to_slow() {
        // Straight for 100 m, then a right angle.
        let net = net_with(&[[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]);
        let l = net
            .get(LineKey {
                cell: (0, 0),
                index: 0,
            })
            .expect("one road");
        // 10 m in, driving forwards: the corner is 90 m away and out of sight.
        assert_eq!(l.min_turn_radius(10.0, 30.0, 1.0), f32::MAX);
        // 80 m in: it is 20 m away and must be seen.
        assert!(l.min_turn_radius(80.0, 30.0, 1.0) < 60.0);
        // 80 m in going the other way: it is behind.
        assert_eq!(l.min_turn_radius(80.0, 30.0, -1.0), f32::MAX);
    }

    /// The junction curve has to begin exactly where the agent is, pointing
    /// where it is pointing, and finish exactly on the new road's lane at its
    /// heading. That is the whole reason it exists: anything approximate at
    /// either end is the pop it was built to remove.
    #[test]
    fn a_junction_turn_is_exact_at_both_ends() {
        let mut turn = Turn {
            from: Vec2::new(10.0, 0.0),
            from_dir: Vec2::X,
            to: Vec2::new(14.0, 4.0),
            to_dir: Vec2::Y,
            len: 8.0,
            t: 0.0,
            from_y: 0.0,
            to_y: 0.0,
        };
        let (p0, d0) = turn.pose();
        assert!(p0.distance(turn.from) < 1e-3, "{p0} != {}", turn.from);
        assert!(d0.distance(Vec2::X) < 1e-3, "leaves along the old road");
        turn.t = 1.0;
        let (p1, d1) = turn.pose();
        assert!(p1.distance(turn.to) < 1e-3, "{p1} != {}", turn.to);
        assert!(d1.distance(Vec2::Y) < 1e-3, "joins along the new road");
        // And it stays a curve in between: every sample is a unit heading and
        // the path does not double back on itself.
        let mut last = p0;
        for step in 1..=20 {
            turn.t = step as f32 / 20.0;
            let (p, d) = turn.pose();
            assert!((d.length() - 1.0).abs() < 1e-3, "heading not unit at {p}");
            assert!(p.distance(last) > 0.0, "the curve stalled");
            last = p;
        }
    }

    /// A right-angle turn must be taken slower than a straight-through
    /// continuation, or the corner curve is decoration.
    #[test]
    fn a_sharp_junction_turn_is_taken_slowly() {
        let corner = Turn {
            from: Vec2::ZERO,
            from_dir: Vec2::X,
            to: Vec2::new(6.0, 6.0),
            to_dir: Vec2::Y,
            len: 9.0,
            t: 0.0,
            from_y: 0.0,
            to_y: 0.0,
        };
        let ahead = Turn {
            to: Vec2::new(9.0, 0.0),
            to_dir: Vec2::X,
            ..corner
        };
        assert!(corner.speed_limit() < 8.0, "{}", corner.speed_limit());
        assert!(
            ahead.speed_limit() > corner.speed_limit() * 4.0,
            "driving straight on through a node is not cornering"
        );
    }

    /// The drawn heading chases the path's and never jumps to it. The rate limit
    /// is the whole mechanism, so both halves are asserted: it moves, and it
    /// moves by no more than it is allowed to.
    #[test]
    fn a_heading_turns_at_a_bounded_rate() {
        let mut h = Vec2::X;
        let target = Vec2::new(-1.0, 0.0);
        let mut steps = 0;
        while h.distance(target) > 1e-2 && steps < 1000 {
            let next = rotate_toward(h, target, 0.1);
            let turned = (h.x * next.y - h.y * next.x).atan2(h.dot(next)).abs();
            assert!(turned <= 0.1 + 1e-4, "turned {turned} rad in one step");
            h = next;
            steps += 1;
        }
        assert!(steps > 20, "a half turn at 0.1 rad a step cannot be quick");
        assert!(h.distance(target) < 1e-2, "never got there");
    }

    // ── Spread ───────────────────────────────────────────────────────────────

    /// Placement is a distribution, not an argmax. A long fast road takes the
    /// larger share and a short slow one still gets picked — the old
    /// three-draws-keep-the-fastest rule failed both halves of that.
    #[test]
    fn placement_favours_the_bigger_road_without_starving_the_smaller() {
        let mut net = DriveNetwork::default();
        let mut fast = line(&[[0.0, 0.0], [400.0, 0.0]], Flow::Both);
        fast.speed = 22.0;
        let mut alley = line(&[[0.0, 20.0], [40.0, 20.0]], Flow::Both);
        alley.speed = 4.5;
        net.insert_cell((0, 0), 0.0, 0.0, vec![fast, alley]);
        let keys = [
            LineKey {
                cell: (0, 0),
                index: 0,
            },
            LineKey {
                cell: (0, 0),
                index: 1,
            },
        ];
        let cum = weigh(&net, &keys, AgentKind::Car);
        let mut tally = [0usize; 2];
        for i in 0..1000 {
            let r = i as f32 / 1000.0;
            let k = draw_line(&keys, &cum, r).expect("a line");
            tally[k.index] += 1;
        }
        assert!(tally[0] > tally[1] * 4, "{tally:?} — the avenue should win");
        assert!(tally[1] > 20, "{tally:?} — but the alley is not starved");
    }

    /// An agent is dropped on the part of a road that is in front of the camera.
    /// The candidate list holds every line with *any* point in range, so without
    /// this a long road clipping the edge of the disc receives agents anywhere
    /// along its whole length — most of them outside the recycle radius and
    /// thrown away on the next frame.
    #[test]
    fn a_recycled_agent_lands_near_the_camera() {
        // 2 km of road; the camera sits beside one end of it.
        let pts: Vec<[f32; 2]> = (0..=200).map(|i| [i as f32 * 10.0, 0.0]).collect();
        let net = net_with(&pts);
        let keys = [LineKey {
            cell: (0, 0),
            index: 0,
        }];
        let cum = weigh(&net, &keys, AgentKind::Car);
        let cam = Vec2::new(1500.0, 30.0);
        let occupied = HashMap::new();
        let l = net.get(keys[0]).expect("one road");
        for agent in 0..50u64 {
            let (key, dir, s) =
                place_agent(&net, &keys, &cum, cam, AgentKind::Car, &occupied, agent, 3)
                    .expect("a spot");
            let along = if dir > 0.0 { s } else { l.length() - s };
            let (p, _) = net.get(key).expect("placed line").at(along);
            assert!(
                p.distance(cam) <= AgentKind::Car.place_radius() + 1.0,
                "placed {:.0} m out, outside the disc the pool is sized for",
                p.distance(cam)
            );
        }
    }

    /// Two agents may not be dropped on the same spot. One car drawn twice a
    /// metre apart in the same paint reads as a rendering fault, not as traffic.
    #[test]
    fn placement_avoids_a_spot_that_is_taken() {
        let pts: Vec<[f32; 2]> = (0..=40).map(|i| [i as f32 * 10.0, 0.0]).collect();
        let net = net_with(&pts);
        let key = LineKey {
            cell: (0, 0),
            index: 0,
        };
        let keys = [key];
        let cum = weigh(&net, &keys, AgentKind::Car);
        let cam = Vec2::new(200.0, 0.0);
        let mut occupied: HashMap<(LineKey, i8), Vec<f32>> = HashMap::new();
        let mut placed: Vec<(i8, f32)> = Vec::new();
        for agent in 0..12u64 {
            let Some((k, dir, s)) =
                place_agent(&net, &keys, &cum, cam, AgentKind::Car, &occupied, agent, 11)
            else {
                continue;
            };
            for (d, o) in &placed {
                if *d == lane_key(dir) {
                    assert!(
                        (o - s).abs() > 1.0,
                        "two agents {:.2} m apart on one lane",
                        (o - s).abs()
                    );
                }
            }
            placed.push((lane_key(dir), s));
            occupied.entry((k, lane_key(dir))).or_default().push(s);
        }
        assert!(placed.len() > 6, "hardly anything was placed");
    }

    /// A car keeps its distance from the one in front. Both ends matter: with
    /// nobody ahead nothing changes at all, and at contact the speed is zero
    /// rather than merely reduced.
    #[test]
    fn a_car_slows_for_the_one_in_front() {
        let key = LineKey {
            cell: (0, 0),
            index: 0,
        };
        let mut occupied: HashMap<(LineKey, i8), Vec<f32>> = HashMap::new();
        // The car itself, at 100 m, and one 10 m in front of it.
        occupied.insert((key, 1), vec![100.0, 110.0]);
        assert!(
            headway_speed(&occupied, key, 1.0, 100.0, 14.0) < 14.0,
            "a car 10 m behind another is not doing the speed limit"
        );
        assert_eq!(
            headway_speed(&occupied, key, 1.0, 110.0, 14.0),
            14.0,
            "the leader is not held up by the car behind it"
        );
        // Bumper to bumper.
        occupied.insert((key, 1), vec![100.0, 100.0 + MIN_HEADWAY_M]);
        assert_eq!(headway_speed(&occupied, key, 1.0, 100.0, 14.0), 0.0);
        // Clear road.
        occupied.insert((key, 1), vec![100.0, 100.0 + HEADWAY_M + 1.0]);
        assert_eq!(headway_speed(&occupied, key, 1.0, 100.0, 14.0), 14.0);
        // And a car coming the other way on the same line is not in the way.
        occupied.insert((key, -1), vec![100.5]);
        assert_eq!(headway_speed(&occupied, key, 1.0, 100.0, 14.0), 14.0);
    }

    /// At a junction a car goes **straight on** unless it is choosing to turn.
    /// Taking whichever line end is nearest — which is what this used to do —
    /// makes the choice on a metre of survey accuracy, so a car running down a
    /// main road turned off it whenever a side street happened to be noded a
    /// fraction closer than the continuation.
    #[test]
    fn a_hand_off_prefers_the_road_straight_ahead() {
        let mut net = DriveNetwork::default();
        // Arriving eastbound at (100, 0). The continuation runs on east; the
        // side street turns south and its end is nearer.
        let straight = line(&[[100.4, 0.0], [300.0, 0.0]], Flow::Both);
        let side = line(&[[100.1, 0.0], [100.1, 200.0]], Flow::Both);
        net.insert_cell((0, 0), 0.0, 0.0, vec![straight, side]);
        let keys: Vec<LineKey> = (0..2)
            .map(|index| LineKey {
                cell: (0, 0),
                index,
            })
            .collect();
        let from = LineKey {
            cell: (9, 9),
            index: 0,
        };
        let (key, dir) = hand_off(
            &net,
            &keys,
            from,
            Vec2::new(100.0, 0.0),
            Vec2::X,
            AgentKind::Car,
        )
        .expect("a road to go on to");
        assert_eq!(key.index, 0, "took the side street over the continuation");
        assert_eq!(dir, 1.0);
    }

    /// …but a dead end is still a dead end. Where the only road on offer doubles
    /// back, the car takes it rather than vanishing.
    #[test]
    fn a_hand_off_doubles_back_only_when_it_must() {
        let mut net = DriveNetwork::default();
        net.insert_cell(
            (0, 0),
            0.0,
            0.0,
            vec![line(&[[100.0, 0.0], [100.0, -80.0]], Flow::Both)],
        );
        let keys = [LineKey {
            cell: (0, 0),
            index: 0,
        }];
        let from = LineKey {
            cell: (9, 9),
            index: 0,
        };
        let got = hand_off(
            &net,
            &keys,
            from,
            Vec2::new(100.0, 0.0),
            Vec2::new(0.0, 1.0),
            AgentKind::Car,
        );
        assert!(got.is_some(), "a car at a dead end has to go somewhere");
    }

    /// The projection has to be onto the segments, not onto the vertices: a
    /// simplified line has a vertex every few metres and the placement radius is
    /// what the answer is compared against.
    #[test]
    fn nearest_along_projects_onto_the_line() {
        let net = net_with(&[[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]);
        let l = net
            .get(LineKey {
                cell: (0, 0),
                index: 0,
            })
            .expect("one road");
        assert!((l.nearest_along(Vec2::new(37.0, 12.0)) - 37.0).abs() < 1e-2);
        assert!((l.nearest_along(Vec2::new(140.0, 60.0)) - 160.0).abs() < 1e-2);
        // Past either end it clamps to the end.
        assert!(l.nearest_along(Vec2::new(-50.0, 0.0)).abs() < 1e-2);
    }
}
