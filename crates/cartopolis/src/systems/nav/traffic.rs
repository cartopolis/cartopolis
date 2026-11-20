//! Ambient traffic: cars, people and cyclists on the streets the map already draws.
//!
//! The pure half — which centrelines are drivable, which way and how fast — is
//! [`cartopolis_geo::traffic`]. This is the ECS half: where the network is kept,
//! the pools of cars, people and cyclists that travel it, and the models they share.
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
//!   object is the obvious way to reintroduce it. So the models — four car
//!   bodies (`cartopolis_geo::agent_models`) and the crowd's people
//!   (`systems::crowd`), a handful of meshes in all — are built once and shared
//!   by the whole pool; an agent is an entity with a `Transform` and a few child
//!   parts, and moving it writes those transforms (and a braking car's
//!   tail-lamp material handle, a walker's `MeshTag` and level of detail) and
//!   nothing else. Once the crowd is made this layer uploads nothing, ever.
//!   `systems::grass` reached the same
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
//! * **A car is not a physics body.** It follows its line, easing its speed
//!   toward the least of the road, the bend ahead, the car in front and a red
//!   light, and hands off to a connected line at the end; its body pitch and
//!   roll are read off that motion rather than simulated.
//!
//! ## What this deliberately is not
//!
//! No collision beyond following the car ahead, no give-way, no car that
//! notices an avatar, and people walk through each other and ignore the
//! lights. (Walkers do notice avatars — they make way, look and wave back,
//! `systems::crowd_react` — because a walker passing through *you* is the one
//! overlap that is never invisible.) Every
//! one of those is a simulation problem whose cost lands on the frame, and none
//! of them is what the layer is for: the question it answers is "is this street
//! alive", settled at a glance from a distance where two figures overlapping for
//! a moment is invisible. Cars *do* stop at the drawn signals
//! (`systems::street_detail`), because a car driving through a red light it is
//! standing next to is not invisible at all — see [`signal_speed`].

use crate::components::AgentKind;
use bevy::prelude::*;
use bevy::render::mesh::MeshTag;
use big_space::prelude::{BigSpace, Grid, GridCell};
use std::collections::HashMap;

#[cfg(test)]
use cartopolis_geo::traffic::Pavements;
use cartopolis_geo::traffic::{DriveLine, Flow, Uses, PAVEMENT_CLEARANCE_M};

use crate::components::{FreeCamera, HideInSpace};
use crate::grid::{anchored_xz, AnchoredCamera};
use crate::systems::give_way;
use crate::systems::traffic_physics::{BodyKind, Size, TrafficPhysics};

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
/// continuation. A turn sharper than [`TURN_BACK_COS`] is only taken when
/// nothing else is on offer.
///
/// **And it is a draw, not the straightest.** From 2026-09-10 to 2026-11-17
/// the road nearest straight on always won, so traffic never turned: a car
/// ran down one street until it was recycled, and a crossroads had cars only
/// going straight through it. Now each way on is weighted by
/// [`STRAIGHT_ON_WEIGHT`] if it carries on within ~25° and by 1 if it turns,
/// times how much traffic a road of its sort draws
/// ([`AgentKind::importance`]) — so a main road keeps most of its traffic
/// past each side street (four in five at a residential one), a crossroads
/// of equals sends about half straight on, and `r`, a unit random, says
/// which.
fn hand_off(
    net: &DriveNetwork,
    candidates: &[LineKey],
    from: LineKey,
    at: Vec2,
    travel: Vec2,
    kind: AgentKind,
    r: f32,
) -> Option<(LineKey, f32)> {
    // Every way on that does not turn back, weighted; at most a handful meet
    // at a junction.
    let mut ways: Vec<(f32, LineKey, f32)> = Vec::new();
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
                (k, _) if k.ignores_oneway() => true,
                (_, Flow::Both) => true,
                (_, Flow::Forward) => dir > 0.0,
                (_, Flow::Backward) => dir < 0.0,
            };
            if !allowed {
                continue;
            }
            // The direction the agent would be travelling once on it.
            let (_, heading) = l.at(if dir > 0.0 { 0.0 } else { l.length() });
            let cos = travel.dot(heading * dir);
            if cos >= TURN_BACK_COS {
                let straight = if cos >= STRAIGHT_ON_COS {
                    STRAIGHT_ON_WEIGHT
                } else {
                    1.0
                };
                ways.push((straight * kind.importance(l), key, dir));
            } else if fallback.is_none_or(|(c, ..)| cos > c) {
                fallback = Some((cos, key, dir));
            }
        }
    }
    let total: f32 = ways.iter().map(|w| w.0).sum();
    let mut left = r.clamp(0.0, 0.999_999) * total;
    for &(weight, key, dir) in &ways {
        if left < weight {
            return Some((key, dir));
        }
        left -= weight;
    }
    ways.last()
        .or(fallback.as_ref())
        .map(|&(_, key, dir)| (key, dir))
}

/// How far from the camera an agent past its pool's quota for the hour
/// (`traffic_hours`) must be before it leaves the road at the end of a line,
/// metres. Nearer, it carries on: an agent vanishing at a junction in view
/// is a pop, and a thinning street is not worth one.
const OFF_DUTY_M: f32 = 80.0;

/// A way on within this of straight ahead (cosine; about 25°) counts as
/// carrying on rather than turning.
const STRAIGHT_ON_COS: f32 = 0.9;
/// How much likelier carrying straight on is than each turn, at a junction
/// of roads of one sort.
const STRAIGHT_ON_WEIGHT: f32 = 2.5;

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

/// How one vehicle follows another: the Intelligent Driver Model (Treiber,
/// Hennecke & Helbing 2000), the car-following law most microscopic traffic
/// simulators use or offer. `s0` is the gap kept standing (bumper to
/// bumper), `t` the time gap kept moving, `a` the comfortable acceleration and
/// `b` the comfortable braking. The car values are the urban ones the
/// literature calibrates at signalised junctions (a shorter time gap and a
/// quicker start than on a motorway); a bike's are a bike's; a reckless driver
/// follows closer.
#[derive(Clone, Copy, Debug)]
struct Idm {
    s0: f32,
    t: f32,
    a: f32,
    b: f32,
}

impl Idm {
    fn of(kind: AgentKind, reckless: bool) -> Self {
        match kind {
            AgentKind::Car if reckless => Self {
                s0: 1.0,
                t: 0.6,
                a: 2.5,
                b: 3.0,
            },
            AgentKind::Car => Self {
                s0: 2.0,
                t: 1.2,
                a: 1.5,
                b: 2.0,
            },
            AgentKind::Cyclist => Self {
                s0: 1.0,
                t: 0.8,
                a: 1.0,
                b: 1.5,
            },
            AgentKind::Fatbike => Self {
                s0: 0.8,
                t: 0.6,
                a: 2.0,
                b: 2.5,
            },
            AgentKind::Walker => Self {
                s0: 0.5,
                t: 0.5,
                a: 1.0,
                b: 1.4,
            },
        }
    }

    /// The acceleration at `v` m/s toward `v0`, `gap` metres behind a leader
    /// going `dv` m/s slower (bumper to bumper; `None` for a free road).
    fn accel(self, v: f32, v0: f32, leader: Option<(f32, f32)>) -> f32 {
        let free = 1.0 - (v / v0.max(0.1)).powi(4);
        let Some((gap, dv)) = leader else {
            return self.a * free;
        };
        let want = self.s0 + (v * self.t + v * dv / (2.0 * (self.a * self.b).sqrt())).max(0.0);
        self.a * (free - (want / gap.max(0.05)).powi(2))
    }
}

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

pub use crate::systems::drive_network::WorldDriveLine;

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

pub use crate::systems::drive_network::DriveNetwork;

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
                let bounds = points.iter().fold(
                    (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)),
                    |(lo, hi), p| (lo.min(*p), hi.max(*p)),
                );
                (run > 1.0).then_some(WorldDriveLine {
                    bounds,
                    points,
                    flow: l.flow,
                    speed: l.speed,
                    half_width: l.half_width,
                    uses: l.uses,
                    kind_drives: l.uses.drive,
                    kind_cycles: l.uses.cycle,
                    cumulative,
                    deck,
                    on_deck: l.on_deck,
                    pavements: l.pavements,
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

    /// Every drivable line within `reach` of `at`, as `(speed in m/s, distance
    /// to its nearest vertex in m)` — what the street ambience is mixed from
    /// (`systems::ambience`).
    ///
    /// The road hum that scanned every vertex of the ring every frame was
    /// removed for that cost (`ambient-traffic.md`). This rejects a line on its
    /// bounding box before reading a point, and is called by a sampler a few
    /// times a second, not per frame. The nearest vertex rather than segment: a
    /// simplified line has one every few metres, finer than the ear places a
    /// road.
    pub fn drivable_near(&self, at: Vec2, reach: f32) -> impl Iterator<Item = (f32, f32)> + '_ {
        let r2 = reach * reach;
        self.cells
            .values()
            .flatten()
            .filter(|l| l.uses.drive)
            .filter(move |l| {
                let (lo, hi) = l.bounds;
                at.x >= lo.x - reach
                    && at.x <= hi.x + reach
                    && at.y >= lo.y - reach
                    && at.y <= hi.y + reach
            })
            .filter_map(move |l| {
                let d2 = l
                    .points
                    .iter()
                    .map(|p| p.distance_squared(at))
                    .fold(f32::MAX, f32::min);
                (d2 <= r2).then(|| (l.speed, d2.sqrt()))
            })
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

/// Take the cyclists off a street that has a cycle track of its own beside
/// it: a Dutch rider takes the track, and riders on both drew two streams of
/// bikes side by side, one of them among the cars.
///
/// A street (a line cars and bikes may both use) is beside a track when at
/// least [`TRACK_SHARE`] of the points along it, every [`TRACK_SAMPLE_M`],
/// have a cycle-only line's segment running the same way (within ~25°)
/// within its half width and [`TRACK_BESIDE_M`]. Recomputed from the tile's
/// answer (`kind_cycles`) each time, so a street whose track streams out
/// carries bikes again. Fatbikes ride it either way.
pub(crate) fn ride_the_tracks(net: &mut DriveNetwork) {
    let tracks: Vec<((Vec2, Vec2), Vec<Vec2>)> = net
        .cells
        .values()
        .flatten()
        .filter(|l| l.kind_cycles && !l.kind_drives)
        .map(|l| (l.bounds, l.points.clone()))
        .collect();
    for line in net.cells.values_mut().flatten() {
        line.uses.cycle = line.kind_cycles;
        if !(line.kind_drives && line.kind_cycles) || tracks.is_empty() {
            continue;
        }
        let reach = line.half_width + TRACK_BESIDE_M;
        let (lo, hi) = line.bounds;
        let near: Vec<&[Vec2]> = tracks
            .iter()
            .filter(|((tlo, thi), _)| {
                tlo.x <= hi.x + reach
                    && thi.x >= lo.x - reach
                    && tlo.y <= hi.y + reach
                    && thi.y >= lo.y - reach
            })
            .map(|(_, p)| p.as_slice())
            .collect();
        if near.is_empty() {
            continue;
        }
        let length = line.length();
        let samples = ((length / TRACK_SAMPLE_M).floor() as usize).max(1);
        let beside = (0..samples)
            .filter(|k| {
                let (p, heading) = line.at((*k as f32 + 0.5) * length / samples as f32);
                near.iter().any(|track| {
                    track.windows(2).any(|w| {
                        let d = w[1] - w[0];
                        let Some(dir) = d.try_normalize() else {
                            return false;
                        };
                        let t = ((p - w[0]).dot(d) / d.length_squared()).clamp(0.0, 1.0);
                        dir.dot(heading).abs() >= STRAIGHT_ON_COS
                            && p.distance(w[0] + d * t) <= reach
                    })
                })
            })
            .count();
        if beside as f32 >= TRACK_SHARE * samples as f32 {
            line.uses.cycle = false;
        }
    }
}

/// How far beyond a street's carriageway its cycle track may run and still
/// be its own, metres: a verge, a row of trees, a parking strip.
const TRACK_BESIDE_M: f32 = 8.0;
/// How often along a street a track beside it is looked for, metres.
const TRACK_SAMPLE_M: f32 = 8.0;
/// The share of a street that must have a track beside it.
const TRACK_SHARE: f32 = 0.6;

/// Keep cars off the streets closed to them.
///
/// The tile calls the Vismarkt a `residential` street like any other — its
/// `motor_vehicle=no` reaches the client only through the street-detail Overpass
/// query (`cartopolis_geo::traffic::cars_barred`). The two arrive on their own
/// schedules, so this re-matches the loaded ring whenever either changes rather
/// than when a cell is inserted, and recomputes `drive` from the tile's answer
/// each time, so a closure that streams out reopens its street. Walkers keep the
/// line, and with `drive` off they walk the middle of it, as people do on a
/// pedestrianised street.
pub fn close_car_free_lines(
    mut net: ResMut<DriveNetwork>,
    detail: Res<crate::systems::street_detail::StreetDetail>,
    mut seen: Local<Option<(u32, u32)>>,
) {
    let now = (net.revision, detail.car_free_revision);
    if *seen == Some(now) {
        return;
    }
    // The tracks beside the streets change only with the lines themselves.
    if seen.is_none_or(|(revision, _)| revision != now.0) {
        ride_the_tracks(&mut net);
    }
    *seen = Some(now);
    let ways: Vec<((Vec2, Vec2), &[[f32; 2]])> = detail
        .car_free_ways()
        .map(|w| {
            let bounds = w.iter().fold(
                (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)),
                |(lo, hi), p| (lo.min(Vec2::from(*p)), hi.max(Vec2::from(*p))),
            );
            (bounds, w.as_slice())
        })
        .collect();
    let net = &mut *net;
    for line in net.cells.values_mut().flatten() {
        let (lo, hi) = line.bounds;
        let near: Vec<&[[f32; 2]]> = ways
            .iter()
            .filter(|((wlo, whi), _)| {
                wlo.x <= hi.x + CAR_FREE_MARGIN_M
                    && whi.x >= lo.x - CAR_FREE_MARGIN_M
                    && wlo.y <= hi.y + CAR_FREE_MARGIN_M
                    && whi.y >= lo.y - CAR_FREE_MARGIN_M
            })
            .map(|(_, w)| *w)
            .collect();
        let closed = line.kind_drives
            && !near.is_empty()
            && cartopolis_geo::traffic::runs_along_closed(
                &line.points.iter().map(|p| p.to_array()).collect::<Vec<_>>(),
                &near,
            );
        line.uses.drive = line.kind_drives && !closed;
    }
}

/// How far outside a line's bounding box a closed way is still considered, m —
/// at least the match distance in `runs_along_closed`.
const CAR_FREE_MARGIN_M: f32 = 6.0;

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

impl AgentKind {
    /// Which of the per-kind tables in [`drive_traffic`] this kind uses.
    fn index(self) -> usize {
        match self {
            Self::Car => 0,
            Self::Walker => 1,
            Self::Cyclist => 2,
            Self::Fatbike => 3,
        }
    }

    fn may_use(self, uses: Uses) -> bool {
        match self {
            Self::Car => uses.drive,
            Self::Walker => uses.walk,
            Self::Cyclist => uses.cycle,
            Self::Fatbike => uses.cycle || uses.walk,
        }
    }

    /// Pace in m/s. A car takes its road's speed; a walker and a cyclist keep
    /// their own, because a pavement has no speed limit and nobody pedals
    /// faster down a main road. A fatbike is quickest where there is room,
    /// and not much slower where there is none.
    fn speed(self, line: &WorldDriveLine) -> f32 {
        match self {
            Self::Car => line.speed,
            Self::Walker => WALK_SPEED,
            Self::Cyclist => CYCLE_SPEED,
            Self::Fatbike if line.uses.cycle => FATBIKE_SPEED,
            Self::Fatbike => FATBIKE_FOOTWAY_SPEED,
        }
    }

    /// How fast this kind may gain and shed speed, m/s². A person's is one
    /// number in both directions; a car's brakes are stronger than its engine.
    fn accel(self) -> (f32, f32) {
        match self {
            Self::Car => (CAR_ACCEL, CAR_BRAKE),
            Self::Walker => (WALK_ACCEL, WALK_ACCEL),
            Self::Cyclist => (CYCLE_ACCEL, CYCLE_BRAKE),
            Self::Fatbike => (FATBIKE_ACCEL, FATBIKE_BRAKE),
        }
    }

    /// Whether this kind slows for the bend ahead, for whatever of its own kind
    /// is in front of it, and for a red light. All three are about vehicles on
    /// a carriageway or a cycle track; a person on a pavement neither corners
    /// nor queues. (A fatbike that runs reds says so in [`Car::defiant`].)
    fn drives(self) -> bool {
        matches!(self, Self::Car | Self::Cyclist | Self::Fatbike)
    }

    /// Whether this kind may travel a line against its one-way flow. A
    /// pavement is two-way whatever the carriageway beside it does — and a
    /// fatbike does not care.
    fn ignores_oneway(self) -> bool {
        matches!(self, Self::Walker | Self::Fatbike)
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
    /// fast. A cycle track carries more bikes than a street does: it is where
    /// the bikes are sent.
    fn importance(self, line: &WorldDriveLine) -> f32 {
        match self {
            Self::Car => (line.speed / RESIDENTIAL_SPEED).max(0.2).powf(0.7),
            Self::Walker => 1.0,
            Self::Cyclist if !line.uses.drive => CYCLE_TRACK_SHARE,
            Self::Cyclist => 1.0,
            Self::Fatbike if line.uses.cycle && !line.uses.drive => CYCLE_TRACK_SHARE,
            Self::Fatbike => 1.0,
        }
    }

    /// How much road this kind needs to itself when it is recycled, in metres.
    fn spawn_gap(self) -> f32 {
        match self {
            Self::Car => SPAWN_GAP_M,
            Self::Walker => SPAWN_GAP_M * 0.2,
            Self::Cyclist | Self::Fatbike => SPAWN_GAP_M * 0.75,
        }
    }

    /// How far out from the camera this kind is placed, in metres.
    ///
    /// A person is a third of a car's size and so legible at roughly a third of
    /// the range; spreading the walker pool over the car's radius spends most of
    /// it where nobody can resolve a walker anyway, and leaves the pavement in
    /// front of you empty. Measured 2026-08-26: at one radius for both, 220
    /// walkers over a city-centre ring put none on the street in view, while the
    /// metrics happily reported all 220 placed. A bike and its rider are between
    /// the two.
    fn place_radius(self) -> f32 {
        match self {
            Self::Car => PLACE_RADIUS_M,
            Self::Walker => PLACE_RADIUS_M * 0.45,
            Self::Cyclist | Self::Fatbike => PLACE_RADIUS_M * 0.6,
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
    ///
    /// A cyclist on a street keeps to the edge of the carriageway, where a
    /// Dutch street paints its cycle lane, with the cars between it and the
    /// middle; on a cycle track, to the right-hand half of it, which is two-way
    /// unless the track is one-way. A fatbike in the road rides where a
    /// cyclist does; on a footway, to its right of the middle. (On a
    /// pavement it is where a walker is: [`Car::offset`].)
    fn offset(self, line: &WorldDriveLine) -> f32 {
        match self {
            Self::Car => line.half_width * LANE_FRACTION,
            Self::Walker => line.half_width + PAVEMENT_CLEARANCE_M,
            Self::Cyclist | Self::Fatbike if line.uses.drive => {
                (line.half_width - CYCLE_KERB_M).max(line.half_width * LANE_FRACTION + 0.6)
            }
            Self::Cyclist => line.half_width * 0.4,
            Self::Fatbike if line.uses.cycle => line.half_width * 0.4,
            Self::Fatbike => line.half_width * 0.3,
        }
    }
}

/// A fatbike's pace where it has room, m/s: 26 km/h, past the 25 an e-bike's
/// motor is meant to stop helping at (each rider ±15 %, so some well past).
const FATBIKE_SPEED: f32 = 7.2;
/// On a footpath, or a pavement among the walkers, m/s: still three times
/// their pace.
const FATBIKE_FOOTWAY_SPEED: f32 = 5.0;
/// A motor pulls away harder than legs; the brakes are a bike's.
const FATBIKE_ACCEL: f32 = 2.2;
const FATBIKE_BRAKE: f32 = 3.5;
/// The share of fatbikes that ride the pavement of a street that has one,
/// rather than the cycle lane.
const FATBIKE_PAVEMENT_SHARE: f32 = 0.55;
/// The share of fatbike riders that run red lights and do not stop at
/// zebras. They all still stop for somebody right in their path.
const FATBIKE_DEFIANT_SHARE: f32 = 0.7;
/// The share of ordinary cyclists who do the same: through red, past the
/// zebra, across whoever has the right of way.
const CYCLIST_DEFIANT_SHARE: f32 = 0.15;
/// The share of drivers who drive too fast and give way to nobody on foot:
/// through red and zebras, past people crossing, braking only for what is
/// in front of them that is not a person — an avatar excepted. They are
/// the ones who knock walkers down (`collide_traffic`).
const RECKLESS_SHARE: f32 = 0.03;
/// How much faster than the road a reckless driver goes.
const RECKLESS_SPEED: f32 = 1.35;
/// How far ahead a fatbike starts weaving round somebody in its way, and
/// how fast it swerves sideways (m/s): a second and a half at its pace.
const FATBIKE_WEAVE_AHEAD_M: f32 = 11.0;
const FATBIKE_SWERVE_RATE: f32 = 1.8;

/// A person's pace, m/s. A stroll rather than the avatar's purposeful walk —
/// ambient people are not going anywhere in particular.
const WALK_SPEED: f32 = 1.25;

/// A city bike's pace, m/s: 16.5 km/h, what a Dutch cyclist rides at in town
/// (each rider ±15 % by its `speed_scale`).
const CYCLE_SPEED: f32 = 4.6;

/// How a cyclist pulls away and slows, m/s².
const CYCLE_ACCEL: f32 = 1.0;
const CYCLE_BRAKE: f32 = 2.5;

/// How far in from the edge of the carriageway a cyclist on a street rides —
/// the middle of a cycle lane, metres.
const CYCLE_KERB_M: f32 = 0.9;

/// A cycle track's share of the riders relative to a street of the same
/// length, in [`AgentKind::importance`].
const CYCLE_TRACK_SHARE: f32 = 1.8;

/// How far a rider leans into a turn: the angle whose tangent is the lateral
/// acceleration over gravity, capped where a city bike's pedal would touch the
/// road. Eased like a car body's tilt.
const MAX_BIKE_LEAN: f32 = 0.35;

/// How far either way of a pavement's middle a walker may keep, metres. A
/// pavement is two or three metres wide; more than this and the outer ones
/// stand in the front gardens.
const WALK_SPREAD_M: f32 = 0.4;

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
    /// Which pavement a walker keeps to: `+1` the one right of its travel,
    /// `-1` the one left of it. A pavement is two-way, so people going both
    /// ways use both — with everybody on their own right, each pavement of a
    /// street carried a one-way crowd. Always `+1` for a car.
    side: f32,
    /// A walker's distance out from the pavement's own line, so a pavement's
    /// people are not threaded on one string. Zero for a car.
    lateral: f32,
    /// A walker's gait phase, radians; one cycle per stride of its crowd
    /// body's walk (`systems::crowd`), walked.
    phase: f32,
    /// A car's body pitch and roll, radians, eased — see [`body_tilt`]; a
    /// cyclist's lean into a turn is the roll ([`bike_lean`]).
    tilt: Vec2,
    /// How far a bike's wheels have turned, radians.
    roll: f32,
    /// Metres of road over which signals are ignored after driving through
    /// one, so a car that crossed on green does not stop in the middle of the
    /// junction for the opposite approach's stop line.
    signal_grace: f32,
    /// Whether the brake lamps are lit, as last written.
    braking: bool,
    /// A car's tail-lamp child (whose material braking swaps); a walker's
    /// body is on the agent itself (`crowd::CrowdFigure`).
    parts: AgentParts,
    /// A walker's answer to the avatars near it: the step aside, the look,
    /// the wave back (`systems::crowd_react`). Untouched for a car.
    react: crate::systems::crowd_react::Reaction,
    /// An offset right of its travel that `CARTO_CROWD_HERE` set to aim it
    /// at the avatar, until its next line.
    pinned: Option<f32>,
    /// A fatbike riding this street's pavement rather than its cycle lane,
    /// chosen per line ([`Car::choose_way`]); the pavement is on `side`.
    pavement: bool,
    /// A fatbike rider who runs red lights and does not stop at zebras
    /// ([`FATBIKE_DEFIANT_SHARE`]). Fixed for the agent's life.
    defiant: bool,
    /// A walker crossing its street, or waiting at the kerb to.
    crossing: Option<Crossing>,
    /// Where it last crossed, so it does not turn round at the far kerb and
    /// cross the same stripes back.
    last_cross: Option<Vec2>,
    /// How far a rider held still has put its foot down, 0 … 1
    /// ([`foot_down`]).
    foot_down: f32,
    /// Its place in its own pool, from 0: the first so many of a pool are
    /// out at an hour (`traffic_hours`), the rest stay home.
    ordinal: u16,
    /// Its Rapier body's box, from its model; `None` for its kind's.
    size: Option<Size>,
    /// Its Rapier body (`traffic_physics`), made the first frame it drives.
    body: Option<rapier3d::prelude::RigidBodyHandle>,
    /// In a collision, or getting going again after one ([`Crash`]).
    crash: Option<Crash>,
    /// Seconds it has been held back by somebody it gave way to at a
    /// crossing (`give_way_to`); past [`CROSSING_PATIENCE_S`] it goes.
    waited: f32,
    /// Seconds since it was put on the road. A body put down on top of
    /// another has not collided with it ([`SETTLE_IN_S`]).
    driven: f32,
    /// Where it was drawn last frame, and the step that took it there (the
    /// smoothness count of `CARTO_TRAFFIC_OVERLAPS`).
    drawn: Vec2,
    drawn_step: Vec2,
    /// What held it back most this frame, and the gap to it if a sweep found
    /// it; and how long it has stood near the camera (the stuck report).
    why: (&'static str, f32),
    stuck: f32,
}

/// How long a newly placed agent cannot collide: placement keeps clear of
/// its own kind on its own line only, and one dropped across another's
/// path drives out of it rather than crashing into it.
const SETTLE_IN_S: f32 = 1.0;
/// How far a body may fall behind where its way has it before the way
/// waits for it, metres (`traffic_physics::deviation`).
const HELD_BACK_M: f32 = 0.5;
/// How far Rapier may push a vehicle sideways off its lane, and a walker off
/// its line, metres (`TrafficPhysics::place`): a car stays on its side of
/// the road, somebody on foot steps aside further.
const LANE_SLACK_M: f32 = 0.3;
const WALKER_SLACK_M: f32 = 1.0;
/// How many agents may be put on the road in one frame. Each is checked
/// against the bodies Rapier has stepped, so the first frame of a fresh
/// ring — every pool empty — would otherwise put hundreds down blind, on
/// top of each other; at this rate the ring fills in a few frames.
const PLACE_PER_FRAME: usize = 24;
/// How deep two agents must be inside each other, not in a collision, for
/// the later to be put down again elsewhere, metres (`collide_traffic`).
const RETIRE_DEPTH_M: f32 = 0.6;

/// An agent in a collision (`traffic_physics::TrafficPhysics::knock`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Crash {
    /// Seconds since it began, or since it began getting going again.
    t: f32,
    /// Getting going again: easing from where the collision left it, `at`
    /// facing `facing`, back onto its line.
    recovering: bool,
    at: Vec2,
    facing: Vec2,
    /// A rider knocked off its bike: the side it falls to, `+1` its right,
    /// `-1` its left; 0 for anybody who stays on their feet or wheels.
    fall: f32,
}

/// How far over a rider in `crash` lies, signed by the side it fell to:
/// eased down over [`FALL_S`], held, and eased back up over [`RISE_S`] as it
/// gets going again. 0 for one that did not fall.
fn fallen(crash: Option<Crash>) -> f32 {
    let Some(c) = crash.filter(|c| c.fall != 0.0) else {
        return 0.0;
    };
    let ease = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    };
    let over = if c.recovering {
        1.0 - ease(c.t / RISE_S)
    } else {
        ease(c.t / FALL_S)
    };
    over * c.fall
}

/// How long a collision holds the two still before they get going: the
/// shove, the slide to a stop, a moment.
const CRASH_HOLD_S: f32 = 3.0;
/// How long a rider who fell lies there before getting up, seconds.
const FALLEN_HOLD_S: f32 = 5.0;
/// Seconds to go down, and to get up and back on.
const FALL_S: f32 = 0.45;
const RISE_S: f32 = 1.2;
/// How far a fallen bike and rider lie over, radians: on its side, the
/// pedal and the bar holding it a little off flat.
const FALL_ANGLE: f32 = 1.35;
/// The closing speed at which a rider goes down whatever it hit, m/s.
const FALL_CLOSING: f32 = 5.0;
/// How far over a person knocked down lies, radians: flat.
const PERSON_FALL_ANGLE: f32 = 1.5;
/// How long an agent takes to ease back onto its line after one.
const CRASH_RECOVER_S: f32 = 1.5;
/// The closing speed two bodies must meet at to collide, m/s: below it they
/// only brush, as a car does a cyclist it is passing too close.
const CRASH_CLOSING: f32 = 2.5;
/// How long a vehicle gives way at a crossing before it goes anyway: two
/// that give way to each other would otherwise wait for ever.
const CROSSING_PATIENCE_S: f32 = 6.0;
/// The same for the one with the right of way: it goes first.
const PRIORITY_PATIENCE_S: f32 = 2.0;
/// How far apart across their way two oncoming bodies must be to pass
/// rather than meet head on, metres: a little under two car half widths.
const ONCOMING_BRUSH_M: f32 = 1.2;

/// A walker crossing the street it walks beside, at a zebra or at a light:
/// it stops at the kerb, faces the road, looks, and walks straight across to
/// the other pavement, then on the way it was going (`Car::crossing`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Crossing {
    /// The zebra or the stop line, world XZ.
    at: Vec2,
    /// The phase group of the traffic it crosses, at a light; `None` at a
    /// zebra, where the cars stop for it.
    signal: Option<u8>,
    /// Seconds at the kerb so far.
    waited: f32,
    /// How far across, 0 at the kerb it left … 1 at the other; `None` while
    /// it waits.
    progress: Option<f32>,
}

/// The share of walkers passing a zebra or a crossing at a light who cross
/// there, when the far side has a pavement.
const CROSS_SHARE: f32 = 0.45;
/// The least a walker stands at the kerb, looking, before it steps out.
const CROSS_LOOK_S: f32 = 0.8;
/// Past this it gives up waiting and walks on.
const CROSS_GIVE_UP_S: f32 = 30.0;
/// Its pace across, as a share of its walking pace: people step out briskly.
const CROSS_PACE: f32 = 1.15;
/// How far ahead of a walker a crossing is taken up, metres: it slows to a
/// stop at about the stripes.
const CROSS_AHEAD_M: (f32, f32) = (0.3, 2.2);
/// The hardest a walker trusts a vehicle to brake for it, m/s²: one that
/// would need more is let by first.
const CROSS_TRUST_BRAKE: f32 = 2.5;

/// The child entities an agent drives besides its own transform.
#[derive(Clone, Copy)]
enum AgentParts {
    Car {
        tail: Entity,
    },
    Walker,
    /// The rider is on the agent itself, as a walker is; the bike is a child
    /// scaled to the rider's body, and its cranks and wheels turn.
    Cyclist {
        bike: Entity,
        crank: Entity,
        wheels: [Entity; 2],
    },
}

/// A bike's parts that the drive turns or scales (`AgentParts::Cyclist`).
#[derive(Component)]
pub struct BikePart;

/// The two tail-lamp materials, swapped per car as it brakes.
#[derive(Resource)]
pub struct TailLampMaterials {
    off: Handle<StandardMaterial>,
    on: Handle<StandardMaterial>,
}

impl Car {
    /// How far right of the centre line this agent travels, in metres.
    ///
    /// A walker on a path walks **on** it, keeping a little to its own side; on
    /// a street it walks the pavement beyond the kerb. It used to stand beside a
    /// path the way it stands beside a street, which put every walker in a
    /// park on the lawn either side of the path.
    fn offset(&self, kind: AgentKind, line: &WorldDriveLine) -> f32 {
        if let Some(pinned) = self.pinned {
            return pinned;
        }
        if let Some(Crossing {
            progress: Some(p), ..
        }) = self.crossing
        {
            // Straight across, from its own pavement to the other.
            return (kind.offset(line) + self.lateral) * self.side * (1.0 - 2.0 * p);
        }
        if kind == AgentKind::Walker && !line.uses.drive {
            let keep = (self.lateral + WALK_SPREAD_M) / (2.0 * WALK_SPREAD_M);
            return self.side * line.half_width * 0.5 * keep.clamp(0.0, 1.0);
        }
        if kind == AgentKind::Fatbike && self.pavement && line.uses.drive {
            // Among the walkers, a little nearer the kerb than they keep.
            return self.side * (line.half_width + PAVEMENT_CLEARANCE_M - 0.3);
        }
        (kind.offset(line) + self.lateral) * self.side
    }

    /// Where a fatbike about to ride `line` in direction `dir` rides it: the
    /// pavement, on whichever side has one, [`FATBIKE_PAVEMENT_SHARE`] of
    /// the time on a street that has a pavement at all — else the cycle lane
    /// on its own right. `r` is a unit random for the choice, `r_side` for
    /// which pavement when there are two. Anything else is left alone.
    fn choose_way(
        &mut self,
        kind: AgentKind,
        line: &WorldDriveLine,
        dir: f32,
        r: f32,
        r_side: f32,
    ) {
        if kind != AgentKind::Fatbike {
            return;
        }
        self.pavement = false;
        self.side = 1.0;
        if !line.uses.drive || r >= FATBIKE_PAVEMENT_SHARE {
            return;
        }
        // `side` is right (+1) or left (−1) of its travel; the pavements are
        // right and left of the line's own order.
        let mine = line.pavements.on(dir);
        let other = line.pavements.on(-dir);
        self.side = match (mine, other) {
            (true, true) if r_side < 0.3 => -1.0,
            (true, _) => 1.0,
            (false, true) => -1.0,
            (false, false) => return,
        };
        self.pavement = true;
    }

    /// Put a walker about to travel `line` in direction `dir` on a side of it
    /// that has a pavement — its own if it can, else the other one, which at a
    /// junction is crossing the street. A car, and a path, are left alone.
    fn keep_to_pavement(&mut self, kind: AgentKind, line: &WorldDriveLine, dir: f32) {
        if kind != AgentKind::Walker || !line.uses.drive {
            return;
        }
        if !line.pavements.on(self.side * dir) && line.pavements.on(-self.side * dir) {
            self.side = -self.side;
        }
    }
}

/// The zebra or light a walker at `p`, going `travel` beside a street
/// `half_width` either side of its centre line, has just ahead of it, across
/// that street: where it is, and the phase group of the traffic it holds
/// (`None` for a zebra). Each signal is (stop line, group, held by a
/// button), as `drive_traffic` lists them.
fn crossing_ahead(
    p: Vec2,
    travel: Vec2,
    half_width: f32,
    zebras: &[crate::systems::street_detail::ZebraPoint],
    signals: &[(Vec2, u8, bool)],
) -> Option<(Vec2, Option<u8>)> {
    let reach = half_width + PAVEMENT_CLEARANCE_M + 2.0;
    let near = |at: Vec2| {
        let rel = at - p;
        let ahead = rel.dot(travel);
        let lateral = (rel.x * travel.y - rel.y * travel.x).abs();
        (CROSS_AHEAD_M.0..=CROSS_AHEAD_M.1).contains(&ahead) && lateral <= reach
    };
    if let Some(z) = zebras
        .iter()
        .find(|z| z.road.dot(travel).abs() > 0.8 && near(z.at))
    {
        return Some((z.at, None));
    }
    let group = crate::systems::street_detail::signal_group(travel);
    signals
        .iter()
        .find(|(at, g, _)| *g == group && near(*at))
        .map(|&(at, g, _)| (at, Some(g)))
}

/// Whether a walker may step out at `at`: no vehicle is on the crossing, and
/// every one coming can stop short of it at a braking a walker would trust
/// ([`CROSS_TRUST_BRAKE`]). Vehicles are (where, which way, how fast);
/// `reach` is how far either side of a vehicle's path the crossing counts.
fn safe_to_cross(at: Vec2, vehicles: &[(Vec2, Vec2, f32)], reach: f32) -> bool {
    vehicles.iter().all(|&(p, travel, speed)| {
        let rel = at - p;
        let ahead = rel.dot(travel);
        let lateral = (rel.x * travel.y - rel.y * travel.x).abs();
        if lateral > reach || !(-3.0..=45.0).contains(&ahead) {
            return true;
        }
        if ahead.abs() < 3.0 {
            // On the crossing, or its nose over it.
            return false;
        }
        speed < 0.8 || speed * speed / (2.0 * CROSS_TRUST_BRAKE) < ahead - 3.0
    })
}

/// What a vehicle does about the body its sweep ahead ran into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    /// Carry on: the other will pass clear of it.
    Go,
    /// Stop short of it: somebody on foot, or a vehicle going its way.
    Wait,
    /// Stop short of a vehicle crossing its path, for a while: `true` when
    /// this one has the right of way and so loses patience first
    /// ([`CROSSING_PATIENCE_S`]).
    Cross(bool),
}

/// Who gives way when a vehicle of `kind` at `at` going `travel` would run
/// into `other`: an avatar (`None`), or an agent of its kind where it is and
/// which way it goes.
///
/// - Somebody on foot is waited for — except by a fatbike, which weaves
///   round them (`make_way_ahead`).
/// - A vehicle going the same way is followed; one coming head-on is waited
///   for only if it is in this one's lane (within half a metre across):
///   on a narrow street two oncoming cars brush past, they do not stop nose
///   to nose for ever.
/// - A vehicle across its path is waited for whoever has the right of way:
///   nobody drives into a car already in the junction. **Right before
///   left**, the Dutch rule where there are no signs, decides who goes first
///   when two wait for each other: the one with the other on its left (its
///   right of way) runs out of patience sooner. A defiant fatbike gives way
///   to nobody.
fn gives_way(
    kind: AgentKind,
    defiant: bool,
    at: Vec2,
    travel: Vec2,
    other: Option<(AgentKind, Vec2, Vec2)>,
) -> Way {
    let Some((other_kind, other_at, heading)) = other else {
        return if kind == AgentKind::Fatbike {
            Way::Go
        } else {
            Way::Wait
        };
    };
    if other_kind == AgentKind::Walker {
        return if kind == AgentKind::Fatbike || defiant {
            Way::Go
        } else {
            Way::Wait
        };
    }
    let rel = other_at - at;
    let along = heading.dot(travel);
    if along > 0.7 {
        return Way::Wait;
    }
    if along < -0.7 {
        let across = (rel.x * travel.y - rel.y * travel.x).abs();
        return if across < 0.5 { Way::Wait } else { Way::Go };
    }
    if defiant {
        return Way::Go;
    }
    Way::Cross(rel.dot(right_of(travel)) <= 0.0)
}

/// The fastest a vehicle may go to stop [`LOOK_AHEAD_GAP_M`] short of a body
/// `gap` metres ahead of its own footprint, braking at the give-way rate.
fn stop_within(gap: f32) -> f32 {
    (2.0 * give_way::YIELD_BRAKE * (gap - LOOK_AHEAD_GAP_M).max(0.0)).sqrt()
}

/// How far ahead of its stopping distance a vehicle sweeps its footprint,
/// and the most it sweeps, metres: a straight sweep runs off the road at a
/// bend, so it is kept short of where that would find the pavement.
const LOOK_AHEAD_MARGIN_M: f32 = 3.0;
const LOOK_AHEAD_MAX_M: f32 = 35.0;
/// The room a vehicle leaves before whoever it stops for, metres.
const LOOK_AHEAD_GAP_M: f32 = 1.0;
/// The least room a vehicle keeps to whoever it stops for, however late it
/// saw them: its step is cut short there.
const LOOK_AHEAD_CLEAR_M: f32 = 0.2;

/// How fast a car moves over to pass a cyclist, and back, m/s sideways.
const CAR_SWERVE_RATE: f32 = 1.0;

/// How far behind a car's centre a cyclist still counts as being passed, and
/// how far ahead one is seen to be caught up with, metres.
const PASS_BEHIND_M: f32 = 4.0;
const PASS_AHEAD_M: f32 = 18.0;
/// The room a driver leaves a cyclist, side to side, beyond the two bodies.
const PASS_CLEARANCE_M: f32 = 0.6;
/// How far over the centre line of a two-way street a car will go to pass,
/// as a share of the half width.
const PASS_INTO_ONCOMING: f32 = 0.25;
/// A car following a cyclist it cannot pass keeps this far back, centre to
/// centre, before it slows to nothing; it settles about 8 m behind one at a
/// cyclist's pace.
const PASS_FOLLOW_M: f32 = 4.5;

/// How a car on `l` gets past a cyclist `gap` metres ahead of its centre in
/// the same lane (negative: beside it): how far to move over, metres right
/// of its lane (so negative, toward the middle), and the speed it may keep
/// of `want`.
///
/// Until 2026-11-17 the two kept their lane offsets and a car drove
/// *through* a rider wherever the street was too narrow for both side by
/// side, which is most Dutch residential streets. Now a car moves over when
/// it can do so without taking more than [`PASS_INTO_ONCOMING`] of the other
/// lane (or, on a one-way street, all but 0.3 m of it), and otherwise stays
/// behind the bike until the street changes.
fn pass_cyclist(l: &WorldDriveLine, gap: f32, want: f32) -> (f32, f32) {
    use crate::systems::give_way::{BIKE_BODY, CAR_BODY};
    let lane = AgentKind::Car.offset(l);
    let apart = AgentKind::Cyclist.offset(l) - lane;
    let need = CAR_BODY.half_width + BIKE_BODY.half_width + PASS_CLEARANCE_M - apart;
    if need <= 0.0 {
        return (0.0, want);
    }
    let furthest = if l.flow == Flow::Both {
        -l.half_width * PASS_INTO_ONCOMING
    } else {
        -l.half_width + 0.3
    };
    if lane - need - CAR_BODY.half_width >= furthest {
        return (-need, want);
    }
    if gap <= 0.0 {
        return (0.0, want);
    }
    (0.0, want.min(((gap - PASS_FOLLOW_M) * 1.2).max(0.0)))
}

/// The side of the squares the walkers are binned in to find each other.
/// Larger than a walker's lookahead (`crowd_react::LOOKAHEAD_M` is 6 m), so
/// the square it is in and the eight round it hold everybody it could meet.
const WALK_CELL_M: f32 = 6.0;

fn walk_cell(p: Vec2) -> (i32, i32) {
    (
        (p.x / WALK_CELL_M).floor() as i32,
        (p.y / WALK_CELL_M).floor() as i32,
    )
}

/// The other walkers that walker `me`, at `p` moving at `vel`, is closing
/// on, appended to `out`: those in its square and the eight round it whose
/// own velocity leaves it gaining on them along its way by more than a
/// quarter of a metre a second — so it steps round somebody coming the other
/// way or dawdling ahead, and not round the one walking beside it at its
/// own pace.
fn closing_walkers(
    grid: &HashMap<(i32, i32), Vec<(usize, Vec2, Vec2)>>,
    me: usize,
    p: Vec2,
    vel: Vec2,
    out: &mut Vec<Vec2>,
) {
    let Some(travel) = vel.try_normalize() else {
        return;
    };
    let (cx, cz) = walk_cell(p);
    for dx in -1..=1 {
        for dz in -1..=1 {
            let Some(cell) = grid.get(&(cx + dx, cz + dz)) else {
                continue;
            };
            for &(i, at, v) in cell {
                if i != me && (vel - v).dot(travel) > 0.25 {
                    out.push(at);
                }
            }
        }
    }
}

/// Deterministic unit random from three integers. The same shape as the geo
/// crate's, kept local so this file does not reach across for four lines.
pub(crate) fn rand01(seed: u64, a: u64, b: u64) -> f32 {
    let mut h =
        seed ^ a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    (h >> 40) as f32 / 16_777_216.0
}

/// Build the shared models and paints, and spawn the pool.
///
/// Runs once. The cars start with no line and are placed by
/// [`drive_traffic`] on the first frame there is a network to place them on —
/// which is not the first frame of the app, since the roads stream in.
///
/// A car is three meshes (body, trim, tail lamps); see
/// `cartopolis_geo::agent_models` for why each part is its own. Every car mesh
/// is built here, once, and shared by the whole pool: four car bodies, **eight
/// car meshes in all**. After this nothing is uploaded — the parts are child
/// entities, and driving writes their transforms and, for a braking car, which
/// of two materials its tail lamps hold. A walker is spawned bare, with the
/// crowd body it is to wear (`crowd::CrowdFigure`); `crowd::dress_walkers` puts
/// it on once that body is made, and until then the walker is not drawn.
pub fn setup_traffic(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    roots: Query<Entity, With<BigSpace>>,
) {
    use cartopolis_geo::agent_models::CarBody;
    let Ok(root) = roots.single() else {
        // Ordered `.after(setup_camera)` in `nav::plugin`, which is what spawns the
        // root — so this is unreachable, and it says so rather than returning in
        // silence. Silence is exactly how this went unnoticed the first time.
        tracing::warn!("no BigSpace root at traffic setup; ambient traffic is off for this run");
        return;
    };
    let mut colour = |rgb: u32, roughness: f32, reflectance: f32| {
        let [r, g, b] = crate::systems::scatter::srgb_to_linear_rgb(rgb);
        materials.add(StandardMaterial {
            base_color: Color::linear_rgb(r, g, b),
            perceptual_roughness: roughness,
            reflectance,
            ..default()
        })
    };
    // Car paint is glossier than the furniture it used to share a material
    // shape with: the sheen along a body side is most of what says "car".
    let paints: Vec<Handle<StandardMaterial>> = cartopolis_geo::parking::CAR_PAINTS
        .iter()
        .map(|&rgb| colour(rgb, 0.32, 0.6))
        .collect();
    let white_paint = colour(VAN_WHITE, 0.32, 0.6);
    // The trim's colours are per vertex; the material only says "fairly glossy"
    // so the glass catches the sky.
    let trim = materials.add(StandardMaterial {
        perceptual_roughness: 0.3,
        reflectance: 0.5,
        ..default()
    });
    // Lamps are unlit — a lamp is its own colour whatever the sun does — and a
    // braking car's are pushed past 1.0 so the bloom picks them up where it runs.
    let tail_off = materials.add(StandardMaterial {
        base_color: Color::linear_rgb(0.7, 0.7, 0.7),
        unlit: true,
        ..default()
    });
    let tail_on = materials.add(StandardMaterial {
        base_color: Color::linear_rgb(4.0, 4.0, 4.0),
        unlit: true,
        ..default()
    });
    commands.insert_resource(TailLampMaterials {
        off: tail_off.clone(),
        on: tail_on,
    });

    // Each body with the box its Rapier body is: the drawn body's bounds.
    let bodies: Vec<(CarBody, [Handle<Mesh>; 3], Size)> = CarBody::ALL
        .iter()
        .map(|&b| {
            let m = b.build();
            (
                b,
                [
                    meshes.add(model_mesh_to_bevy(&m.body)),
                    meshes.add(model_mesh_to_bevy(&m.trim)),
                    meshes.add(model_mesh_to_bevy(&m.tail)),
                ],
                model_size(&m.body),
            )
        })
        .collect();

    // `CARTO_RECKLESS_SHARE=<0..1>`: more reckless drivers than [`RECKLESS_SHARE`],
    // so a capture can show one hitting somebody without waiting an hour.
    let reckless = std::env::var("CARTO_RECKLESS_SHARE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(RECKLESS_SHARE);
    let agent = |kind: AgentKind, i: usize, seed: u64, parts: AgentParts| {
        let walker = kind == AgentKind::Walker;
        let r = |stream: u64| rand01(seed, i as u64, stream);
        Car {
            line: None,
            s: 0.0,
            dir: 1.0,
            speed_scale: (0.85 + r(7) * 0.3)
                * if kind == AgentKind::Car && r(15) < reckless {
                    RECKLESS_SPEED
                } else {
                    1.0
                },
            speed: 0.0,
            heading: Vec2::ZERO,
            turn: None,
            side: if walker && r(11) < 0.5 { -1.0 } else { 1.0 },
            lateral: if walker {
                (r(12) * 2.0 - 1.0) * WALK_SPREAD_M
            } else {
                0.0
            },
            phase: r(13) * std::f32::consts::TAU,
            tilt: Vec2::ZERO,
            roll: 0.0,
            signal_grace: 0.0,
            braking: false,
            parts,
            react: Default::default(),
            pinned: None,
            pavement: false,
            defiant: r(15)
                < match kind {
                    AgentKind::Fatbike => FATBIKE_DEFIANT_SHARE,
                    AgentKind::Cyclist => CYCLIST_DEFIANT_SHARE,
                    AgentKind::Car => reckless,
                    AgentKind::Walker => 0.0,
                },
            crossing: None,
            last_cross: None,
            foot_down: 0.0,
            ordinal: i as u16,
            size: None,
            body: None,
            crash: None,
            waited: 0.0,
            driven: 0.0,
            drawn: Vec2::ZERO,
            drawn_step: Vec2::ZERO,
            why: ("free", f32::NAN),
            stuck: 0.0,
        }
    };

    for i in 0..MAX_CARS {
        let (body, [body_mesh, trim_mesh, tail_mesh], size) = {
            let b = CarBody::for_index(i);
            let (_, m, size) = bodies
                .iter()
                .find(|(k, ..)| *k == b)
                .expect("every body is built");
            (b, m.clone(), *size)
        };
        let paint = if body.fleet_white() && rand01(0x0CA2, i as u64, 3) < 0.7 {
            white_paint.clone()
        } else {
            paints[i % paints.len()].clone()
        };
        let trim_part = commands
            .spawn((
                Mesh3d(trim_mesh),
                MeshMaterial3d(trim.clone()),
                Transform::default(),
            ))
            .id();
        let tail = commands
            .spawn((
                Mesh3d(tail_mesh),
                MeshMaterial3d(tail_off.clone()),
                Transform::default(),
            ))
            .id();
        let car = commands
            .spawn((
                Mesh3d(body_mesh),
                MeshMaterial3d(paint),
                // Off-screen until placed. An agent with no line must not be
                // visible at the origin, which is where the avatar stands.
                Visibility::Hidden,
                Transform::default(),
                GridCell::default(),
                ChildOf(root),
                HideInSpace,
                AgentKind::Car,
                Car {
                    size: Some(size),
                    ..agent(AgentKind::Car, i, 0x0CA2, AgentParts::Car { tail })
                },
            ))
            .id();
        commands.entity(trim_part).insert(ChildOf(car));
        commands.entity(tail).insert(ChildOf(car));
    }

    for i in 0..MAX_WALKERS {
        use crate::systems::crowd::{CrowdFigure, COAT_COLOURS, TROUSER_COLOURS};
        // The crowd's bodies already differ in height as their people do;
        // ±4 % more keeps two walkers wearing one body from matching.
        let height = 0.96 + rand01(0x0FEE, i as u64, 14) * 0.08;
        commands.spawn((
            Visibility::Hidden,
            Transform::from_scale(Vec3::splat(height)),
            GridCell::default(),
            ChildOf(root),
            HideInSpace,
            AgentKind::Walker,
            agent(AgentKind::Walker, i, 0x0FEE, AgentParts::Walker),
            CrowdFigure {
                seed: i as u32,
                top: (i % COAT_COLOURS.len()) as u8,
                bottom: ((i * 7 + 3) % TROUSER_COLOURS.len()) as u8,
                near: false,
                riding: false,
            },
        ));
    }

    // The riders: a crowd person on the agent, as a walker is, and a bike under
    // them — the frame in its paint, the trim, the cranks and two wheels, the
    // last three turning about their axles. Two frames, the diamond and the
    // step-through, five meshes each counting the shared wheel; nothing more
    // is uploaded.
    use cartopolis_geo::agent_models::{bicycle, BIKE_FIT};
    let frames: Vec<[Handle<Mesh>; 4]> = [false, true]
        .iter()
        .map(|&step_through| {
            let b = bicycle(step_through);
            [
                meshes.add(model_mesh_to_bevy(&b.frame)),
                meshes.add(model_mesh_to_bevy(&b.trim)),
                meshes.add(model_mesh_to_bevy(&b.crank)),
                meshes.add(model_mesh_to_bevy(&b.wheel)),
            ]
        })
        .collect();
    let bike_paints: Vec<Handle<StandardMaterial>> = BIKE_PAINTS
        .iter()
        .map(|&rgb| {
            let [r, g, b] = crate::systems::scatter::srgb_to_linear_rgb(rgb);
            materials.add(StandardMaterial {
                base_color: Color::linear_rgb(r, g, b),
                perceptual_roughness: 0.45,
                reflectance: 0.5,
                ..default()
            })
        })
        .collect();
    for i in 0..MAX_CYCLISTS {
        use crate::systems::crowd::{CrowdFigure, COAT_COLOURS, TROUSER_COLOURS};
        let r = |stream: u64| rand01(0xB1CE, i as u64, stream);
        let [frame, bike_trim, crank_mesh, wheel] = frames[usize::from(r(1) < 0.5)].clone();
        let part = |commands: &mut Commands, mesh: Handle<Mesh>, at: [f32; 3]| {
            commands
                .spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(trim.clone()),
                    Transform::from_translation(Vec3::from_array(at)),
                    BikePart,
                ))
                .id()
        };
        let crank = part(&mut commands, crank_mesh, BIKE_FIT.bottom_bracket);
        let wheels = [
            part(&mut commands, wheel.clone(), BIKE_FIT.rear_axle),
            part(&mut commands, wheel, BIKE_FIT.front_axle),
        ];
        let fittings = part(&mut commands, bike_trim, [0.0; 3]);
        let bike = commands
            .spawn((
                Mesh3d(frame),
                MeshMaterial3d(
                    bike_paints[(r(2) * bike_paints.len() as f32) as usize % bike_paints.len()]
                        .clone(),
                ),
                Transform::default(),
                BikePart,
            ))
            .add_children(&[crank, wheels[0], wheels[1], fittings])
            .id();
        let height = 0.96 + r(14) * 0.08;
        let rider = commands
            .spawn((
                Visibility::Hidden,
                Transform::from_scale(Vec3::splat(height)),
                GridCell::default(),
                ChildOf(root),
                HideInSpace,
                AgentKind::Cyclist,
                agent(
                    AgentKind::Cyclist,
                    i,
                    0xB1CE,
                    AgentParts::Cyclist {
                        bike,
                        crank,
                        wheels,
                    },
                ),
                CrowdFigure {
                    // Past the walkers' seeds, so a street's riders and walkers
                    // are dealt different people.
                    seed: (MAX_WALKERS + i) as u32,
                    top: ((i * 5 + 2) % COAT_COLOURS.len()) as u8,
                    bottom: ((i * 3 + 1) % TROUSER_COLOURS.len()) as u8,
                    near: false,
                    riding: true,
                },
            ))
            .id();
        commands.entity(bike).insert(ChildOf(rider));
    }

    // The fatbikes: the same rider on the same fit, on the fatbike's meshes —
    // four more, uploaded here once like the rest — in their own paints.
    use cartopolis_geo::agent_models::{fatbike, FATBIKE_FRONT_AXLE, FATBIKE_REAR_AXLE};
    let fat = fatbike();
    let [frame, bike_trim, crank_mesh, wheel] = [
        meshes.add(model_mesh_to_bevy(&fat.frame)),
        meshes.add(model_mesh_to_bevy(&fat.trim)),
        meshes.add(model_mesh_to_bevy(&fat.crank)),
        meshes.add(model_mesh_to_bevy(&fat.wheel)),
    ];
    let fat_paints: Vec<Handle<StandardMaterial>> = FATBIKE_PAINTS
        .iter()
        .map(|&rgb| {
            let [r, g, b] = crate::systems::scatter::srgb_to_linear_rgb(rgb);
            materials.add(StandardMaterial {
                base_color: Color::linear_rgb(r, g, b),
                // Matte, as most of them are sold.
                perceptual_roughness: 0.75,
                reflectance: 0.35,
                ..default()
            })
        })
        .collect();
    for i in 0..MAX_FATBIKES {
        use crate::systems::crowd::{CrowdFigure, COAT_COLOURS, TROUSER_COLOURS};
        let r = |stream: u64| rand01(0xFA7B, i as u64, stream);
        let part = |commands: &mut Commands, mesh: Handle<Mesh>, at: [f32; 3]| {
            commands
                .spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(trim.clone()),
                    Transform::from_translation(Vec3::from_array(at)),
                    BikePart,
                ))
                .id()
        };
        let crank = part(&mut commands, crank_mesh.clone(), BIKE_FIT.bottom_bracket);
        let wheels = [
            part(&mut commands, wheel.clone(), FATBIKE_REAR_AXLE),
            part(&mut commands, wheel.clone(), FATBIKE_FRONT_AXLE),
        ];
        let fittings = part(&mut commands, bike_trim.clone(), [0.0; 3]);
        let bike = commands
            .spawn((
                Mesh3d(frame.clone()),
                MeshMaterial3d(fat_paints[i % fat_paints.len()].clone()),
                Transform::default(),
                BikePart,
            ))
            .add_children(&[crank, wheels[0], wheels[1], fittings])
            .id();
        let height = 0.96 + r(14) * 0.08;
        let rider = commands
            .spawn((
                Visibility::Hidden,
                Transform::from_scale(Vec3::splat(height)),
                GridCell::default(),
                ChildOf(root),
                HideInSpace,
                AgentKind::Fatbike,
                agent(
                    AgentKind::Fatbike,
                    i,
                    0xFA7B,
                    AgentParts::Cyclist {
                        bike,
                        crank,
                        wheels,
                    },
                ),
                CrowdFigure {
                    // Past the cyclists' seeds.
                    seed: (MAX_WALKERS + MAX_CYCLISTS + i) as u32,
                    top: ((i * 3 + 4) % COAT_COLOURS.len()) as u8,
                    bottom: ((i * 5 + 2) % TROUSER_COLOURS.len()) as u8,
                    near: false,
                    riding: true,
                },
            ))
            .id();
        commands.entity(bike).insert(ChildOf(rider));
    }
}

/// How many fatbikes exist. A handful against the cyclists' seventy: a few
/// in view at a time is how they are met in a Dutch town, and each is loud.
pub const MAX_FATBIKES: usize = 12;

/// Fatbike frames: matte black for most, then the greys, the army green and
/// the sand they are sold in, and one white.
const FATBIKE_PAINTS: &[u32] = &[
    0x1A_1A_1B, 0x1A_1A_1B, 0x1A_1A_1B, 0x4A_4D_51, 0x4B_53_3A, 0xB5_A4_80, 0x1A_1A_1B, 0xE4_E4_E2,
];

/// How many cyclists exist, on the same fixed-pool terms as the cars.
///
/// Fewer than the cars because a ring holds far less cycle track than road:
/// at 120 (2026-11-15) the cycle track by Groningen's northern railway carried
/// the pool as one unbroken column, a rider every few metres.
pub const MAX_CYCLISTS: usize = 70;

/// Bike frames: mostly black, as a Dutch street's are, then the dark blues,
/// greys and greens, and one cream for the old *omafiets*. Muted for the reason
/// the cars' paints are: a primary-coloured pool is the loudest thing on a
/// pale map.
const BIKE_PAINTS: &[u32] = &[
    0x1F_1F_21, 0x1F_1F_21, 0x1F_1F_21, 0x26_2E_3F, 0x5A_5E_63, 0x2F_45_3A, 0xD2_C9_B2, 0x6E_2E_2A,
];

/// What the walkers react to (`systems::crowd_react`), and the state of the
/// `CARTO_CROWD_HERE` capture hook. One parameter, because `drive_traffic`
/// sits near Bevy's sixteen.
#[derive(bevy::ecs::system::SystemParam)]
pub struct CrowdReact<'w, 's> {
    people: Res<'w, crate::systems::crowd_react::CrowdPeople>,
    here: Local<'s, CrowdHere>,
    /// The signal buttons pressed (`systems::give_way`), and the
    /// `CARTO_TRAFFIC_HERE` hook's state.
    buttons: Res<'w, crate::systems::give_way::SignalButtons>,
    traffic_here: Local<'s, TrafficHere>,
    /// The scene's hour (the sun's clock) and the day of the week, for how
    /// much of each pool is out (`systems::traffic_hours`).
    dev: Option<Res<'w, crate::resources::DevPanelState>>,
    /// The traffic's Rapier bodies (`systems::traffic_physics`).
    physics: ResMut<'w, crate::systems::traffic_physics::TrafficPhysics>,
    /// The smoothness count (`Jitter`).
    jitter: Local<'s, Jitter>,
    clock: Option<Res<'w, crate::systems::transit_live::WorldClock>>,
}

/// `CARTO_TRAFFIC_HERE=1`: cars sent at the avatar, so a capture can show
/// them giving way. Every [`TRAFFIC_HERE_EVERY_S`] of the moving world's
/// clock the first three cars of the pool are put on the drivable line
/// nearest the avatar (within 25 m), 45, 60 and 75 m before it in a
/// direction the line may be driven, and once a second the nearest of them
/// is logged: its distance to the avatar and its speed. A capture's hook,
/// not something a session runs.
#[derive(Default)]
pub struct TrafficHere {
    on: Option<bool>,
    next: f32,
    plan: Vec<(LineKey, f32, f32)>,
    /// The pool indices sent this round.
    sent: Vec<usize>,
    reported: f32,
}

const TRAFFIC_HERE_EVERY_S: f32 = 25.0;

/// Placements for [`TrafficHere`] round `avatar`: (line, direction, distance
/// run along it), or none if no car line is within 25 m.
fn traffic_here_plan(net: &DriveNetwork, avatar: Vec2) -> Vec<(LineKey, f32, f32)> {
    let best = net
        .near(avatar, 25.0, AgentKind::Car)
        .into_iter()
        .filter_map(|k| {
            let l = net.get(k)?;
            let along = l.nearest_along(avatar);
            Some((k, along, l.at(along).0.distance(avatar)))
        })
        .min_by(|a, b| a.2.total_cmp(&b.2));
    let Some((key, along, _)) = best else {
        return Vec::new();
    };
    let l = net.get(key).expect("near only offers lines it holds");
    let dirs: &[f32] = match l.flow {
        Flow::Forward => &[1.0],
        Flow::Backward => &[-1.0],
        Flow::Both => &[1.0, -1.0],
    };
    // The direction with the most road before the avatar.
    let dir = dirs
        .iter()
        .copied()
        .max_by(|a, b| {
            let room = |d: f32| if d > 0.0 { along } else { l.length() - along };
            room(*a).total_cmp(&room(*b))
        })
        .unwrap_or(1.0);
    [45.0, 60.0, 75.0]
        .iter()
        .filter_map(|back| {
            let start = along - dir * back;
            let start = start.clamp(0.0, l.length());
            if (start - along).abs() < 15.0 {
                return None;
            }
            let s = if dir > 0.0 { start } else { l.length() - start };
            Some((key, dir, s))
        })
        .collect()
}

/// Where `person` is relative to an agent's path: metres ahead of it along
/// the path and metres off the middle of its lane. Along the line it is on
/// where the person projects onto that line, so a bend does not put the
/// pavement in its way; straight ahead in a junction or past the line's end.
fn on_path(
    car: &Car,
    kind: AgentKind,
    l: &WorldDriveLine,
    p: Vec2,
    travel: Vec2,
    person: Vec2,
) -> (f32, f32) {
    let straight = || {
        let rel = person - p;
        (rel.dot(travel), rel.dot(right_of(travel)))
    };
    if car.turn.is_some() {
        return straight();
    }
    let at = l.nearest_along(person);
    if at <= 0.01 || at >= l.length() - 0.01 {
        return straight();
    }
    let along = if car.dir > 0.0 {
        car.s
    } else {
        l.length() - car.s
    };
    let (centre, heading) = l.at(at);
    let lane_dir = heading * car.dir;
    let lane = centre + right_of(lane_dir) * car.offset(kind, l);
    (
        (at - along) * car.dir,
        (person - lane).dot(right_of(lane_dir)),
    )
}

/// `CARTO_CROWD_HERE=1`: walkers sent at the avatar, so a capture can show
/// them making way. Every [`CROWD_HERE_EVERY_S`] of the moving world's clock
/// the first eight dressed walkers are put on the walking line nearest the
/// avatar, four coming from each side 5–14 m off, on courses through it
/// (±0.25 m), held until each reaches its next line. It is a
/// capture's hook, not something a session runs.
#[derive(Default)]
pub struct CrowdHere {
    on: Option<bool>,
    next: f32,
    /// This round's placements, walker by walker; empty between rounds.
    plan: Vec<(LineKey, f32, f32, f32)>,
    /// What it reports once a second: the nearest any walker came to the
    /// avatar, the widest step aside, the most heads turned and walkers
    /// waving.
    closest: f32,
    widest: f32,
    looking: usize,
    waving: usize,
    reported: f32,
}

const CROWD_HERE_EVERY_S: f32 = 30.0;

/// Placements for [`CrowdHere`] round `avatar`, or none if no walking line
/// is within 25 m: (line, direction, distance along, offset right of travel).
fn crowd_here_plan(net: &DriveNetwork, avatar: Vec2) -> Vec<(LineKey, f32, f32, f32)> {
    let best = net
        .near(avatar, 25.0, AgentKind::Walker)
        .into_iter()
        .filter_map(|k| {
            let l = net.get(k)?;
            let along = l.nearest_along(avatar);
            let (c, _) = l.at(along);
            Some((k, along, c.distance(avatar)))
        })
        .min_by(|a, b| a.2.total_cmp(&b.2));
    let Some((key, along, _)) = best else {
        return Vec::new();
    };
    let l = net.get(key).expect("near only offers lines it holds");
    let (centre, heading) = l.at(along);
    // Where the avatar is across the line, metres right of its own order.
    let q = (avatar - centre).dot(right_of(heading));
    (0..8)
        .filter_map(|k| {
            let dir = if k % 2 == 0 { 1.0 } else { -1.0 };
            let start = along - dir * (5.0 + 3.0 * (k / 2) as f32);
            if !(0.0..=l.length()).contains(&start) {
                return None;
            }
            let s = if dir > 0.0 { start } else { l.length() - start };
            let jitter = ((k * 7) % 5) as f32 * 0.125 - 0.25;
            // Right of its own travel, which runs `dir` along the line.
            Some((key, dir, s, (q + jitter) * dir))
        })
        .collect()
}

/// Most vans are white; see [`CarBody::fleet_white`](cartopolis_geo::agent_models::CarBody::fleet_white).
const VAN_WHITE: u32 = 0xEC_EC_EA;

/// How many people exist, on the same fixed-pool terms as the cars.
///
/// More than the cars because a city has more people than vehicles in view and
/// each is a fraction of a car's geometry.
pub const MAX_WALKERS: usize = 220;

/// Advance every car, and recycle the ones that have run out of road.
#[allow(clippy::too_many_arguments)]
pub fn drive_traffic(
    // The moving world's clock, not the loaders': frozen between the frames of a
    // `--video-fps` movie (`systems::anim_clock`), the virtual clock otherwise.
    time: Res<crate::systems::anim_clock::AnimClock>,
    net: Res<DriveNetwork>,
    layers: Res<crate::systems::layer_state::DataLayers>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    decks: Res<crate::systems::map_geometry::DeckPlates>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: AnchoredCamera,
    grids: Query<&Grid, With<BigSpace>>,
    mut cars: Query<
        (
            &mut Car,
            &AgentKind,
            // Its Rapier body's tag (`traffic_physics`).
            Entity,
            &mut Transform,
            &mut GridCell,
            // A walker's body, once `crowd::dress_walkers` has put it on.
            Option<(
                &mut crate::systems::crowd::CrowdFigure,
                &mut Mesh3d,
                &mut MeshMaterial3d<crate::systems::crowd::CrowdMaterial>,
                &mut MeshTag,
            )>,
            &mut Visibility,
        ),
        Without<FreeCamera>,
    >,
    detail: Res<crate::systems::street_detail::StreetDetail>,
    tail_materials: Option<Res<TailLampMaterials>>,
    mut tails: Query<&mut MeshMaterial3d<StandardMaterial>, Without<Car>>,
    crowd: Res<crate::systems::crowd::Crowd>,
    // A bike's frame, cranks and wheels (`AgentParts::Cyclist`).
    mut bike_parts: Query<&mut Transform, (With<BikePart>, Without<Car>, Without<FreeCamera>)>,
    react: CrowdReact,
    mut frame: Local<u64>,
) {
    // One toggle per pool (Layers ▸ Life ▸ Traffic / People / Cyclists). A pool
    // that is off is hidden and then skipped entirely — it holds its line and
    // its distance along it, so switching it back on resumes rather than
    // re-places, and a hidden agent costs neither the scan nor the transform
    // write.
    let show_cars = layers.cars_visible();
    let show_people = layers.people_visible();
    let show_cyclists = layers.cyclists_visible();
    if (!show_cars && !show_people && !show_cyclists) || net.is_empty() {
        for (.., mut vis) in &mut cars {
            vis.set_if_neq(Visibility::Hidden);
        }
        return;
    }
    let showing = |kind: AgentKind| match kind {
        AgentKind::Car => show_cars,
        AgentKind::Walker => show_people,
        AgentKind::Cyclist | AgentKind::Fatbike => show_cyclists,
    };
    let (Some((cam_cell, cam_tf)), Ok(grid)) = (camera.get(), grids.single()) else {
        return;
    };
    // The camera's absolute world XZ, which is the frame the network is in.
    let cam = anchored_xz(cam_cell, cam_tf.translation);
    // Which way the camera looks on the ground; zero looking straight down.
    let cam_ahead = {
        let f = cam_tf.forward();
        Vec2::new(f.x, f.z).normalize_or_zero()
    };
    // The altitude, not `cam_tf.translation.y`: that is the height inside the
    // camera's 10 km grid cell, which wraps, so above the first cell the layer
    // came back on — and stayed on to the space tier, simulated and drawn.
    let too_high = globe.altitude_m(cam_cell, cam_tf) as f32 > TRAFFIC_HIDE_ALT;
    if too_high {
        for (.., mut vis) in &mut cars {
            vis.set_if_neq(Visibility::Hidden);
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
    let lanes = if show_cyclists {
        net.near(cam, AgentKind::Cyclist.place_radius(), AgentKind::Cyclist)
    } else {
        Vec::new()
    };
    // Everything a fatbike will ride: the cyclists' lines and the walkers'.
    let rides = if show_cyclists {
        net.near(cam, AgentKind::Fatbike.place_radius(), AgentKind::Fatbike)
    } else {
        Vec::new()
    };
    if roads.is_empty() && paths.is_empty() && lanes.is_empty() && rides.is_empty() {
        return;
    }
    // How much of the pool each candidate deserves — see `weigh`. Built once per
    // frame beside the candidate lists themselves, for the same reason.
    let road_weights = weigh(&net, &roads, AgentKind::Car);
    let path_weights = weigh(&net, &paths, AgentKind::Walker);
    let lane_weights = weigh(&net, &lanes, AgentKind::Cyclist);
    let ride_weights = weigh(&net, &rides, AgentKind::Fatbike);

    // Where every placed agent is, keyed by its line **and** the direction it is
    // travelling, as the distance it has run along that line. Two things read
    // it: a car looks up whatever is in front of it (`headway_speed`), and a
    // recycled agent avoids landing on top of one. Both used to be missing, and
    // both are visible: a fast car overtook a slow one *through* it, and two of
    // the pool on one street a metre apart drew one car twice.
    //
    // **One table per kind** (`AgentKind::index`): a car queues behind a car, a
    // cyclist behind a cyclist. Kept in one table, a car on a street followed
    // the people on its pavement at walking pace, and would have followed every
    // bike in the cycle lane beside it — which a driver overtakes.
    //
    // It is a read-only pass over the same query — one hash insert per placed
    // agent, no allocation per frame beyond the maps themselves.
    let mut occupied: [HashMap<(LineKey, i8), Vec<f32>>; 4] = Default::default();
    // The same, with each one's speed: whom a vehicle follows, and how fast
    // it goes (`follow_speed`).
    let mut leaders: [HashMap<(LineKey, i8), Vec<(f32, f32)>>; 4] = Default::default();
    // How many of each pool are on the road.
    let mut placed_count = [0usize; 4];
    // Where the walkers and the riders are, for the fatbikes weaving through
    // them; and where the fatbikes are, for the walkers making way. Only
    // while there are both, from the same read-only pass.
    //
    // The same pass notes where the walkers are, for each other and for the
    // fatbikes; who is crossing a street or waiting at a zebra, for the
    // traffic to stop for; and where every vehicle is and how fast, for a
    // walker deciding whether to step out.
    let weaving = show_cyclists && show_people;
    let mut walkers_at: Vec<Vec2> = Vec::new();
    // Walkers by 5 m square: (pool index, where, velocity).
    let mut walker_grid: HashMap<(i32, i32), Vec<(usize, Vec2, Vec2)>> = HashMap::new();
    let mut riders_at: Vec<Vec2> = Vec::new();
    let mut fatbikes_at: Vec<Vec2> = Vec::new();
    let mut vehicles: Vec<(Vec2, Vec2, f32)> = Vec::new();
    let mut crossers: Vec<Vec2> = Vec::new();
    let mut kerb_waiters: Vec<(Vec2, Vec2)> = Vec::new();
    // Every agent placed, by its Rapier tag: what it is, where, which way —
    // for deciding who gives way to the body a shape cast runs into.
    let mut seen: HashMap<u64, (AgentKind, Vec2, Vec2)> = HashMap::new();
    // Every vehicle, and every walker crossing a street, by 20 m square:
    // (tag, kind, where, velocity) — for whoever might cross its path
    // (`path_conflict`).
    let mut movers: HashMap<(i32, i32), Vec<(u64, AgentKind, Vec2, Vec2)>> = HashMap::new();
    for (i, (car, &kind, entity, ..)) in cars.iter().enumerate() {
        if !showing(kind) {
            continue;
        }
        let Some(key) = car.line else {
            continue;
        };
        occupied[kind.index()]
            .entry((key, lane_key(car.dir)))
            .or_default()
            .push(car.s);
        leaders[kind.index()]
            .entry((key, lane_key(car.dir)))
            .or_default()
            .push((car.s, car.speed));
        placed_count[kind.index()] += 1;
        let Some((p, travel, _)) = pose_of(car, net.get(key), kind) else {
            continue;
        };
        let p = p + right_of(travel) * car.react.shift;
        seen.insert(entity.to_bits(), (kind, p, travel));
        if kind != AgentKind::Walker || car.crossing.is_some() {
            movers.entry(mover_cell(p)).or_default().push((
                entity.to_bits(),
                kind,
                p,
                travel * car.speed,
            ));
        }
        match kind {
            AgentKind::Walker => {
                walkers_at.push(p);
                walker_grid
                    .entry(walk_cell(p))
                    .or_default()
                    .push((i, p, travel * car.speed));
                match car.crossing {
                    Some(Crossing {
                        progress: Some(_), ..
                    }) => crossers.push(p),
                    Some(Crossing { signal: None, .. }) => {
                        kerb_waiters.push((p, right_of(travel) * -car.side));
                    }
                    _ => {}
                }
            }
            AgentKind::Cyclist => {
                riders_at.push(p);
                vehicles.push((p, travel, car.speed));
            }
            AgentKind::Fatbike => {
                if weaving {
                    fatbikes_at.push(p);
                }
                vehicles.push((p, travel, car.speed));
            }
            AgentKind::Car => vehicles.push((p, travel, car.speed)),
        }
    }

    let dt = time.delta_secs();
    let elapsed = time.elapsed_secs();

    // ── Who the walkers notice ───────────────────────────────────────────────
    //
    // Every avatar about, as one short list (`crowd_react::gather_people`),
    // and the walkers that answer a wave begun this frame — chosen here, in a
    // read-only pass that only runs on a frame somebody starts waving.
    use crate::systems::crowd_react as react_to;
    let CrowdReact {
        people,
        mut here,
        buttons,
        mut traffic_here,
        dev,
        clock,
        mut physics,
        mut jitter,
    } = react;
    if !jitter.checked {
        jitter.checked = true;
        jitter.on = crate::debug_switches::env_flag("CARTO_TRAFFIC_OVERLAPS");
    }
    if jitter.on && elapsed >= jitter.reported + 1.0 {
        let line = [
            AgentKind::Car,
            AgentKind::Walker,
            AgentKind::Cyclist,
            AgentKind::Fatbike,
        ]
        .iter()
        .map(|k| {
            let i = k.index();
            let mean = jitter.sum[i] / jitter.n[i].max(1) as f32;
            format!(
                "{k:?} mean {mean:.2} max {:.1} (n {})",
                jitter.max[i], jitter.n[i]
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
        info!("traffic jitter m/s²: {line}");
        jitter.sum = [0.0; 4];
        jitter.max = [0.0; 4];
        jitter.n = [0; 4];
        jitter.reported = elapsed;
    }
    // The avatars stand in the traffic's world as bodies it can run into.
    let avatar_feet: Vec<Vec3> = people
        .at
        .iter()
        .map(|a| Vec3::new(a.x, terrain.ground_at(a.x, a.y), a.y))
        .collect();
    physics.anchor(cam);
    physics.set_avatars(&avatar_feet);
    // How many of each pool are out at this hour (`traffic_hours`): an agent
    // past its pool's quota is let go when it next runs out of road out of
    // the camera's sight, and not placed again until the hour fills.
    let quota: [usize; 4] = {
        use crate::systems::traffic_hours::{out_of, share, weekday};
        let hour = dev.as_ref().map_or(12.0, |d| d.time_of_day);
        let day = clock
            .as_ref()
            .map_or(2, |c| weekday(c.now_unix, c.local_offset_secs.unwrap_or(0)));
        [
            (AgentKind::Car, MAX_CARS),
            (AgentKind::Walker, MAX_WALKERS),
            (AgentKind::Cyclist, MAX_CYCLISTS),
            (AgentKind::Fatbike, MAX_FATBIKES),
        ]
        .map(|(kind, size)| out_of(size, share(kind, hour, day)))
    };
    // What a walker makes way for and looks at: every avatar, and every
    // fatbike — which it sees too late to do much about.
    let walker_sees: Vec<Vec2> = if show_people {
        people.at.iter().chain(&fatbikes_at).copied().collect()
    } else {
        Vec::new()
    };
    let people_at: &[Vec2] = &walker_sees;
    // What a fatbike weaves round: the walkers on its pavement or footpath,
    // the riders in its lane, and the avatars.
    let fatbike_dodges: Vec<Vec2> = if show_cyclists {
        people
            .at
            .iter()
            .chain(&walkers_at)
            .chain(&riders_at)
            .copied()
            .collect()
    } else {
        Vec::new()
    };
    let mut answering: [Option<(usize, f32, Vec2)>; 4] = [None; 4];
    if show_people {
        let first_wave = people.waves_seen.wrapping_sub(people.waves.len() as u32);
        for (w, &at) in people.waves.iter().enumerate().take(2) {
            let candidates = cars.iter().enumerate().filter_map(|(i, (car, &kind, ..))| {
                if kind != AgentKind::Walker || car.react.wave.is_some() {
                    return None;
                }
                let l = car.line.and_then(|k| net.get(k))?;
                let (p, travel, _) = pose_of(car, Some(l), kind)?;
                let facing = if car.heading == Vec2::ZERO {
                    travel
                } else {
                    car.heading
                };
                let p = p + right_of(travel) * car.react.shift;
                react_to::wave_candidate(p, facing, at).map(|s| (i, s))
            });
            let picked =
                react_to::pick_wave_backs(candidates, first_wave.wrapping_add(w as u32 + 1));
            for (k, pick) in picked.into_iter().enumerate() {
                if let Some((i, delay)) = pick {
                    answering[w * react_to::MAX_WAVE_BACKS + k] = Some((i, delay, at));
                }
            }
            info!(
                "a wave at ({:.1}, {:.1}): {} walker(s) answer",
                at.x,
                at.y,
                picked.iter().flatten().count()
            );
        }
    }

    // `CARTO_CROWD_HERE`: a round of walkers sent at the avatar, once the
    // crowd's bodies are all made.
    if *here
        .on
        .get_or_insert_with(|| crate::debug_switches::env_flag("CARTO_CROWD_HERE"))
        && show_people
        && here.plan.is_empty()
        && elapsed >= here.next
        && !crowd.is_empty()
        && crowd.bodies.iter().all(Option::is_some)
    {
        if let Some(&avatar) = people.at.first() {
            here.plan = crowd_here_plan(&net, avatar);
            if !here.plan.is_empty() {
                info!(
                    "CARTO_CROWD_HERE: {} walkers sent at the avatar at {elapsed:.1} s",
                    here.plan.len()
                );
                here.next = elapsed + CROWD_HERE_EVERY_S;
                here.closest = f32::MAX;
                here.reported = elapsed;
            }
        }
    }
    let mut here_plan = std::mem::take(&mut here.plan).into_iter();
    let here_avatar = here
        .on
        .unwrap_or(false)
        .then(|| people.at.first().copied())
        .flatten();
    let (mut here_looking, mut here_waving) = (0, 0);
    // The drawn signals a car could reach before it is recycled. Empty with the
    // street-detail layer off, which is right: a car stops only at a light that
    // is on the screen to be red.
    //
    // Each with whether a pressed button holds it red (`give_way`). The
    // walkers cross at the same lights.
    let any_pool = show_cars || show_cyclists || show_people;
    let signals: Vec<(Vec2, u8, bool)> = if any_pool {
        detail
            .signals_near(cam, AgentKind::Car.recycle_radius() + SIGNAL_LOOKAHEAD_M)
            .map(|s| (s.at, s.group, buttons.holds(s.at, s.group, elapsed)))
            .collect()
    } else {
        Vec::new()
    };
    // The zebras, and the avatars with the way each faces, for the cars and
    // cyclists that give way (`give_way`) — every avatar, whether or not the
    // people layer is on — and the walkers waiting at one or on the road.
    let zebras: Vec<crate::systems::street_detail::ZebraPoint> = if any_pool {
        detail
            .zebras_near(cam, AgentKind::Car.recycle_radius() + SIGNAL_LOOKAHEAD_M)
            .collect()
    } else {
        Vec::new()
    };
    // Whom a vehicle gives way to on its path: the avatars, and the walkers
    // crossing in front of it.
    let avatars: Vec<Vec2> = people.at.iter().chain(&crossers).copied().collect();
    let avatars: &[Vec2] = &avatars;
    let avatars_facing: Vec<(Vec2, Vec2)> = if zebras.is_empty() {
        Vec::new()
    } else {
        people
            .at
            .iter()
            .copied()
            .zip(people.facing.iter().copied())
            .chain(kerb_waiters.iter().copied())
            // On the stripes, the way a walker faces does not matter.
            .chain(crossers.iter().map(|&p| (p, Vec2::X)))
            .collect()
    };

    // `CARTO_TRAFFIC_HERE`: a round of cars sent at the avatar.
    if *traffic_here
        .on
        .get_or_insert_with(|| crate::debug_switches::env_flag("CARTO_TRAFFIC_HERE"))
        && show_cars
        && traffic_here.plan.is_empty()
        && elapsed >= traffic_here.next
    {
        if let Some(&avatar) = people.at.first() {
            traffic_here.plan = traffic_here_plan(&net, avatar);
            traffic_here.sent.clear();
            if !traffic_here.plan.is_empty() {
                info!(
                    "CARTO_TRAFFIC_HERE: {} cars sent at the avatar at {elapsed:.1} s",
                    traffic_here.plan.len()
                );
                traffic_here.next = elapsed + TRAFFIC_HERE_EVERY_S;
            }
        }
    }
    let mut traffic_plan = std::mem::take(&mut traffic_here.plan).into_iter();
    let report_traffic = traffic_here.on == Some(true)
        && !traffic_here.sent.is_empty()
        && elapsed - traffic_here.reported >= 1.0;
    // (distance to the avatar, speed) of the nearest car sent, this frame.
    let mut nearest_sent: Option<(f32, f32)> = None;
    // Who a walker makes way for this frame; one buffer, refilled per walker.
    let mut near: Vec<Vec2> = Vec::new();
    // Agents put on the road this frame.
    let mut placed_now = 0usize;

    for (i, (mut car, &kind, entity, mut tf, mut cell, dressed, mut vis)) in
        cars.iter_mut().enumerate()
    {
        if !showing(kind) {
            *vis = Visibility::Hidden;
            continue;
        }
        let (candidates, weights): (&[LineKey], &[f32]) = match kind {
            AgentKind::Car => (&roads, &road_weights),
            AgentKind::Walker => (&paths, &path_weights),
            AgentKind::Cyclist => (&lanes, &lane_weights),
            AgentKind::Fatbike => (&rides, &ride_weights),
        };
        // A car with a cyclist close ahead in its lane: how far ahead, from
        // the cyclists' own table (`AgentKind::index`), read before this
        // kind's table is borrowed to write.
        let bike_ahead = if kind == AgentKind::Car {
            car.line
                .and_then(|k| occupied[AgentKind::Cyclist.index()].get(&(k, lane_key(car.dir))))
                .and_then(|riders| {
                    riders
                        .iter()
                        .map(|o| o - car.s)
                        .filter(|d| (-PASS_BEHIND_M..PASS_AHEAD_M).contains(d))
                        .min_by(f32::total_cmp)
                })
        } else {
            None
        };
        let occupied = &mut occupied[kind.index()];
        if candidates.is_empty() {
            *vis = Visibility::Hidden;
            continue;
        }
        let walker = kind == AgentKind::Walker;
        let on_duty = usize::from(car.ordinal) < quota[kind.index()];
        // How long this walker's body waves for; 0 for one that cannot.
        let wave_secs = dressed
            .as_ref()
            .and_then(|(figure, ..)| crowd.body(figure.body(&crowd)))
            .map_or(0.0, |b| b.wave_secs);
        if walker && dressed.is_some() {
            if let Some((k, dir, s, offset)) = here_plan.next() {
                car.line = Some(k);
                car.dir = dir;
                car.s = s;
                car.pinned = Some(offset);
                car.crossing = None;
                car.turn = None;
                car.heading = Vec2::ZERO;
                car.speed = WALK_SPEED * car.speed_scale;
                car.react.reset();
            }
        }
        if kind == AgentKind::Car {
            if let Some((k, dir, s)) = traffic_plan.next() {
                if let Some(l) = net.get(k) {
                    car.line = Some(k);
                    car.dir = dir;
                    car.s = s;
                    car.turn = None;
                    car.heading = Vec2::ZERO;
                    car.speed = kind.speed(l) * car.speed_scale;
                    car.signal_grace = 0.0;
                    traffic_here.sent.push(i);
                }
            }
        }
        if walker && wave_secs > 0.0 {
            for (j, at) in answering
                .iter()
                .flatten()
                .filter(|a| a.0 == i)
                .map(|a| (a.1, a.2))
            {
                car.react.wave = Some(react_to::WaveBack {
                    at,
                    t: 0.0,
                    delay: j,
                    waving: None,
                });
            }
        }
        let mut key = car.line;
        // A line closed since the agent joined it (`close_car_free_lines`) is
        // left at once: the closure is what the street is, not news to it. A
        // cyclist on a street a track has just been found beside
        // (`ride_the_tracks`) rides it to its end instead: the bikes may use
        // it, and vanishing would be a pop.
        let mut line = key
            .and_then(|k| net.get(k))
            .filter(|l| kind.may_use(l.uses) || (kind == AgentKind::Cyclist && l.kind_cycles));
        // Its Rapier body, and whether a collision has it (`collide_traffic`):
        // then Rapier moves it, not its line.
        let size = car.size;
        let handle = *car
            .body
            .get_or_insert_with(|| physics.agent_body(entity.to_bits(), kind, size));
        let knocked = car.crash.is_some_and(|c| !c.recovering);
        car.driven += time.delta_secs();
        if knocked {
            car.speed = 0.0;
        }
        // What Rapier made of it last step: how far it was pushed aside or
        // held back from where its way had it (`traffic_physics::deviation`).
        // Held back, its way waits for it — the path does not run on ahead of
        // a body that something stands in front of.
        let push = if knocked {
            Vec2::ZERO
        } else {
            physics.deviation(handle).unwrap_or(Vec2::ZERO)
        };
        let held_back = -push.dot(car.heading) - HELD_BACK_M;
        if held_back > 0.0 {
            match car.turn.as_mut() {
                Some(t) => t.t = (t.t - held_back / t.len.max(0.5)).max(0.0),
                None => car.s = (car.s - held_back).max(0.0),
            }
            if let Some((_, _, moving)) = physics.pose(handle) {
                car.speed = car.speed.min(moving);
            }
        }
        // What this frame's advance did, for the body's lean, the brake lamps
        // and the stride.
        let mut accel_now = 0.0;
        let mut held = false;
        let mut stepped = 0.0;
        // Where a walker means to step to, metres right of its path.
        let mut shift_to = 0.0;
        // A walker's pace across the street it is crossing, m/s; its along
        // speed is zero meanwhile.
        let mut cross_speed = 0.0;
        // A vehicle giving way to somebody close in front: brake hard, and
        // the most its centre may move this frame (`give_way::Yield`).
        let mut hard_brake = false;
        let mut room = f32::MAX;
        // What answering a wave has it doing.
        let wave_step = car.react.wave.map(|w| w.step(wave_secs));

        // ── Advance ──────────────────────────────────────────────────────────
        //
        // Speed is *eased*, never assigned. What it is eased toward is the least
        // of what the road allows, what the bend ahead allows and what the car
        // in front allows — so a car pulls away from a junction, slows into a
        // corner and sits behind a slower one instead of stepping between three
        // constants as it changes road.
        if let (Some(k), Some(l), false) = (key, line, knocked) {
            let along = if car.dir > 0.0 {
                car.s
            } else {
                l.length() - car.s
            };
            let mut want = kind.speed(l) * car.speed_scale;
            // What holds it back most, for `CARTO_TRAFFIC_OVERLAPS`'s stuck report.
            let mut why = ("free", f32::NAN);
            let mut last = want;
            let mut note =
                |want: f32, name: &'static str, gap: f32, why: &mut (&'static str, f32)| {
                    if want < last - 1e-4 {
                        *why = (name, gap);
                    }
                    last = want;
                };
            if kind.drives() {
                want = want.min(corner_speed(l.min_turn_radius(
                    along,
                    CORNER_LOOKAHEAD_M,
                    car.dir,
                )));
                note(want, "bend", f32::NAN, &mut why);
                if let Some(t) = &car.turn {
                    want = want.min(t.speed_limit());
                }
                note(want, "turn", f32::NAN, &mut why);
                let (follow, hard) = follow_speed(
                    &leaders[kind.index()],
                    k,
                    car.dir,
                    car.s,
                    car.speed,
                    want,
                    2.0 * reach_of(kind),
                    Idm::of(kind, car.defiant),
                    dt,
                );
                want = want.min(follow);
                hard_brake |= hard;
                note(want, "headway", f32::NAN, &mut why);
                if let Some(gap) = bike_ahead {
                    let (shift, keep_to) = pass_cyclist(l, gap, want);
                    shift_to = shift;
                    want = keep_to;
                    note(want, "cyclist ahead", gap, &mut why);
                }
                if let Some((p, travel, _)) = pose_of(&car, Some(l), kind) {
                    let reach = l.half_width + 1.0;
                    // A defiant fatbike rides through red, and past a zebra.
                    if !car.defiant {
                        let mut grace = car.signal_grace;
                        want = want.min(signal_speed(
                            &signals, elapsed, p, travel, car.speed, reach, &mut grace,
                        ));
                        car.signal_grace = grace;
                        note(want, "signal", f32::NAN, &mut why);
                    }
                    // Give way to the avatars: on its path, and at a zebra
                    // (`give_way`).
                    let body = if kind == AgentKind::Car {
                        give_way::CAR_BODY
                    } else {
                        give_way::BIKE_BODY
                    };
                    if kind == AgentKind::Fatbike {
                        // A fatbike does not wait for anybody: it swerves
                        // round them, and slows only when it cannot.
                        let (to, keep) = react_to::make_way_ahead(
                            p,
                            travel,
                            car.react.shift,
                            &fatbike_dodges,
                            FATBIKE_WEAVE_AHEAD_M,
                        );
                        shift_to = to;
                        want *= keep;
                        if car.pavement {
                            want = want.min(FATBIKE_FOOTWAY_SPEED * car.speed_scale);
                        }
                    } else {
                        let near = give_way::YIELD_LOOKAHEAD_M + body.nose + 2.0;
                        // A reckless driver gives way to nobody crossing; it
                        // still brakes for an avatar.
                        let people_in_way: &[Vec2] = if car.defiant { &people.at } else { avatars };
                        let y = give_way::yield_to(
                            people_in_way
                                .iter()
                                .filter(|a| a.distance_squared(p) < near * near)
                                .map(|&a| on_path(&car, kind, l, p, travel, a)),
                            car.speed,
                            body,
                        );
                        want = want.min(y.limit);
                        note(want, "person in path", f32::NAN, &mut why);
                        hard_brake |= y.hard;
                        room = y.room;
                    }
                    if !zebras.is_empty() && !car.defiant {
                        want = want.min(give_way::zebra_speed(
                            &zebras,
                            &avatars_facing,
                            p,
                            travel,
                            car.speed,
                            reach,
                            body,
                        ));
                        note(want, "zebra", f32::NAN, &mut why);
                    }
                    // Whoever its own footprint would run into, of any kind,
                    // on any line: Rapier sweeps it ahead along its way.
                    let ahead_m = (car.speed * car.speed / (2.0 * give_way::YIELD_BRAKE)
                        + LOOK_AHEAD_MARGIN_M)
                        .min(LOOK_AHEAD_MAX_M);
                    let feet = Vec3::new(
                        p.x,
                        physics
                            .height(handle)
                            .unwrap_or_else(|| terrain.ground_at(p.x, p.y)),
                        p.y,
                    );
                    let mut crossing = false;
                    // Through a junction, toward where the turn comes out:
                    // straight along the curve's tangent it sweeps the
                    // corner and misses whoever waits round it.
                    let sweep = car
                        .turn
                        .and_then(|t| (t.to + t.to_dir * 4.0 - p).try_normalize())
                        .unwrap_or(travel);
                    if let Some((gap, tag)) = physics.look_ahead(handle, kind, feet, sweep, ahead_m)
                    {
                        let other = if TrafficPhysics::is_avatar(tag) {
                            Some(None)
                        } else {
                            seen.get(&(tag as u64))
                                .map(|&(k, at, heading)| Some((k, at, heading)))
                        };
                        if let Some(other) = other {
                            let stop = match gives_way(kind, car.defiant, p, travel, other) {
                                Way::Go => false,
                                Way::Wait => true,
                                Way::Cross(first) => {
                                    crossing = true;
                                    let patience = if first {
                                        PRIORITY_PATIENCE_S
                                    } else {
                                        CROSSING_PATIENCE_S
                                    };
                                    car.waited < patience
                                }
                            };
                            if stop {
                                want = want.min(stop_within(gap));
                                note(want, other_name(other), gap, &mut why);
                                // Seen late — it pulled out, or came round a
                                // bend — it brakes as hard as it must, and
                                // never drives on into it: a collision is the
                                // other one moving into its way, not this one
                                // failing to stop.
                                if car.speed * car.speed / (2.0 * give_way::YIELD_BRAKE)
                                    > gap - LOOK_AHEAD_GAP_M
                                {
                                    hard_brake = true;
                                }
                                room = room.min((gap - LOOK_AHEAD_CLEAR_M).max(0.0));
                            }
                        }
                    }
                    // Whoever is coming toward the point where their paths
                    // cross: the one who would get there second waits short
                    // of it, the one with the other on its right if they
                    // would get there together, and always for somebody on
                    // foot. The sweep sees only what is already in its way;
                    // this sees the cyclist about to cut across it.
                    if !car.defiant {
                        let mine = (p, travel * car.speed.max(0.5), reach_of(kind));
                        let me_tag = entity.to_bits();
                        let (cx, cz) = mover_cell(p);
                        for dx in -1..=1 {
                            for dz in -1..=1 {
                                let Some(cell) = movers.get(&(cx + dx, cz + dz)) else {
                                    continue;
                                };
                                for &(tag, other_kind, at, vel) in cell {
                                    if tag == me_tag {
                                        continue;
                                    }
                                    let theirs = (at, vel, reach_of(other_kind));
                                    if let Some(short) =
                                        path_conflict(mine, theirs, other_kind == AgentKind::Walker)
                                    {
                                        want = want.min(stop_within(short));
                                        note(want, "crossing path", short, &mut why);
                                    }
                                }
                            }
                        }
                    }
                    car.waited = if crossing { car.waited + dt } else { 0.0 };
                }
            } else if walker {
                if let Some(mut c) = car.crossing {
                    // At the kerb, or on the road: it does not walk on.
                    want = 0.0;
                    match c.progress {
                        None => {
                            c.waited += dt;
                            let lights = c.signal.is_none_or(|g| {
                                crate::systems::street_detail::signal_lets_walk(elapsed, g)
                                    || signals.iter().any(|s| s.0 == c.at && s.2)
                            });
                            let go = c.waited >= CROSS_LOOK_S
                                && lights
                                && safe_to_cross(c.at, &vehicles, l.half_width + 1.0);
                            if go {
                                c.progress = Some(0.0);
                            }
                            car.crossing = (go || c.waited < CROSS_GIVE_UP_S).then_some(c);
                        }
                        Some(p) => {
                            let width = 2.0 * (kind.offset(l) + car.lateral).max(1.0);
                            cross_speed = WALK_SPEED * CROSS_PACE * car.speed_scale;
                            let p = p + cross_speed * dt / width;
                            if p >= 1.0 {
                                car.side = -car.side;
                                car.crossing = None;
                                car.last_cross = Some(c.at);
                            } else {
                                c.progress = Some(p);
                                car.crossing = Some(c);
                            }
                        }
                    }
                } else if let Some((p, travel, _)) = pose_of(&car, Some(l), kind) {
                    // Make way for whoever is on its line — the avatars, the
                    // fatbikes, and the walkers it is closing on — slowing if
                    // it must.
                    near.clear();
                    near.extend_from_slice(people_at);
                    closing_walkers(&walker_grid, i, p, travel * car.speed, &mut near);
                    let (to, keep) = react_to::make_way(p, travel, car.react.shift, &near);
                    shift_to = to;
                    want *= keep;
                    // A zebra or a light just ahead, across its street, with
                    // a pavement on the far side: some cross there.
                    let crossable = l.uses.drive
                        && car.pinned.is_none()
                        && car.react.wave.is_none()
                        && l.pavements.on(-car.side * car.dir);
                    if crossable {
                        if let Some((at, signal)) =
                            crossing_ahead(p, travel, l.half_width, &zebras, &signals)
                        {
                            let fresh = car.last_cross.is_none_or(|q| q.distance(at) > 8.0);
                            let spot =
                                ((at.x * 4.0) as i64 as u64) ^ ((at.y * 4.0) as i64 as u64) << 20;
                            if fresh && rand01(0xC205, i as u64, spot) < CROSS_SHARE {
                                car.crossing = Some(Crossing {
                                    at,
                                    signal,
                                    waited: 0.0,
                                    progress: None,
                                });
                                want = 0.0;
                            }
                        }
                    }
                }
                // Stop to answer a wave.
                if matches!(
                    wave_step,
                    Some(react_to::WaveStep::Stopping | react_to::WaveStep::Waving { .. })
                ) {
                    want = 0.0;
                }
            }
            car.why = why;
            let (accel, brake) = kind.accel();
            let rate = if want > car.speed {
                accel
            } else if hard_brake {
                brake.max(give_way::EMERGENCY_BRAKE)
            } else {
                brake
            };
            let before = car.speed;
            car.speed = (car.speed + (want - car.speed).clamp(-rate * dt, rate * dt)).max(0.0);
            // Never into somebody, whatever the brakes managed.
            if dt > 0.0 && car.speed * dt > room {
                car.speed = room / dt;
            }
            if dt > 0.0 {
                accel_now = (car.speed - before) / dt;
            }
            held = car.speed < 0.4 && want < 0.4;

            // Inside a junction the progress is the turn's, not the line's.
            let step = car.speed * dt;
            // The stride counts the walk across a street too.
            stepped = step + cross_speed * dt;
            car.signal_grace = (car.signal_grace - step).max(0.0);
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
                // Past its pool's quota for the hour, and out of the way of
                // the camera: it goes home here rather than on.
                let off_duty = !on_duty && p.distance(cam) > OFF_DUTY_M;
                if car.s > l.length() && off_duty {
                    key = None;
                    line = None;
                } else if car.s > l.length() {
                    let r = rand01(0x7A2E, i as u64, *frame);
                    match hand_off(&net, candidates, k, p, travel, kind, r) {
                        Some((next, next_dir)) => {
                            let nl = net.get(next).expect("hand_off only offers lines it read");
                            let entry = if next_dir > 0.0 { 0.0 } else { nl.length() };
                            let (centre, heading, to_y) = nl.at_with_deck(entry);
                            let to_dir = heading * next_dir;
                            car.pinned = None;
                            car.keep_to_pavement(kind, nl, next_dir);
                            car.choose_way(
                                kind,
                                nl,
                                next_dir,
                                rand01(0xFA7E, i as u64, *frame),
                                rand01(0xFA7F, i as u64, *frame),
                            );
                            let to = centre + right_of(to_dir) * car.offset(kind, nl);
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
        // Past this frame's placements, it waits for the next: an agent put
        // down is checked against the bodies Rapier knows, which are only
        // those of earlier frames (`PLACE_PER_FRAME`).
        if line.is_none() && placed_now >= PLACE_PER_FRAME {
            car.line = None;
            *vis = Visibility::Hidden;
            continue;
        }
        if line.is_none() && !on_duty {
            // Not out at this hour: it waits, unseen, until the hour fills.
            car.line = None;
            *vis = Visibility::Hidden;
            continue;
        }
        if line.is_none() {
            // A spot is free when the agent's body stood there would be
            // inside nobody else's (`traffic_physics::is_clear`).
            let size = car.size;
            // And, once the pool has filled, out of sight: behind the camera or
            // far off (`unseen_m`), so nobody appears from nowhere in view.
            let filling = placed_count[kind.index()] * 2 < quota[kind.index()];
            let free = |l: &WorldDriveLine, along: f32, dir: f32| {
                let (centre, heading) = l.at(along);
                let travel = heading * dir;
                let p = centre + right_of(travel) * kind.offset(l);
                let rel = p - cam;
                let near = rel.length() < unseen_m(kind);
                if !filling
                    && near
                    && (cam_ahead == Vec2::ZERO || rel.dot(cam_ahead) > -0.3 * rel.length())
                {
                    return false;
                }
                physics.is_clear(
                    kind,
                    size,
                    Vec3::new(p.x, terrain.ground_at(p.x, p.y), p.y),
                    travel,
                )
            };
            match place_agent(
                &net, candidates, weights, cam, kind, occupied, i as u64, *frame, &free,
            ) {
                Some((k, dir, s)) => {
                    let l = net.get(k).expect("place_agent only offers lines it read");
                    car.pinned = None;
                    car.keep_to_pavement(kind, l, dir);
                    car.choose_way(
                        kind,
                        l,
                        dir,
                        rand01(0xFA7E, i as u64, *frame),
                        rand01(0xFA7F, i as u64, *frame),
                    );
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
                    car.tilt = Vec2::ZERO;
                    car.signal_grace = 0.0;
                    car.react.reset();
                    car.crossing = None;
                    car.last_cross = None;
                    car.foot_down = 0.0;
                    placed_now += 1;
                    car.drawn = Vec2::ZERO;
                    car.waited = 0.0;
                    car.driven = 0.0;
                    if car.crash.take().is_some() {
                        physics.settle(handle);
                    }
                    shift_to = 0.0;
                    accel_now = 0.0;
                    held = false;
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

        let (Some(_), Some((mut pos, travel, deck))) = (key, pose) else {
            *vis = Visibility::Hidden;
            continue;
        };
        let on_deck = line.is_some_and(|l| l.on_deck);
        if report_traffic && traffic_here.sent.contains(&i) {
            if let Some(&a) = people.at.first() {
                let d = pos.distance(a);
                if nearest_sent.is_none_or(|(n, _)| d < n) {
                    nearest_sent = Some((d, car.speed));
                }
            }
        }

        // ── A walker's reactions ─────────────────────────────────────────────
        //
        // Stepped aside of its path by `Reaction::shift`, eased toward what
        // `make_way` asked for, and never inside an avatar (`keep_clear`). It
        // faces the way it is going — sideways too, so the step reads as a
        // step — or, answering a wave, the waver.
        let mut aim = travel;
        let mut facing_rate = None;
        if walker {
            let (eased, sideways) = react_to::ease_shift(car.react.shift, shift_to, dt);
            let shift = react_to::keep_clear(pos, travel, eased, people_at);
            car.react.shift = shift;
            car.react.shift_speed = sideways;
            pos += right_of(travel) * shift;
            let ahead = travel * car.speed.max(0.6) + right_of(travel) * sideways;
            aim = ahead.try_normalize().unwrap_or(travel);
            if let (
                Some(w),
                Some(react_to::WaveStep::Stopping | react_to::WaveStep::Waving { .. }),
            ) = (car.react.wave, wave_step)
            {
                if let Some(to) = (w.at - pos).try_normalize() {
                    aim = to;
                    facing_rate = Some(3.0);
                }
            }
            // Waiting at the kerb or on the way over: facing across, toward
            // the far pavement.
            if car.crossing.is_some() {
                aim = right_of(travel) * -car.side;
                facing_rate = Some(3.0);
            }
        } else if kind == AgentKind::Car {
            // Moved over to pass a cyclist, and back.
            let (eased, _) =
                react_to::ease_shift_at(car.react.shift, shift_to, dt, CAR_SWERVE_RATE);
            car.react.shift = eased;
            pos += right_of(travel) * eased;
        } else if kind == AgentKind::Fatbike {
            // The swerve, quicker than a walker's step, and never through
            // anybody.
            let (eased, sideways) =
                react_to::ease_shift_at(car.react.shift, shift_to, dt, FATBIKE_SWERVE_RATE);
            let shift = react_to::keep_clear(pos, travel, eased, &fatbike_dodges);
            car.react.shift = shift;
            pos += right_of(travel) * shift;
            let ahead = travel * car.speed.max(1.0) + right_of(travel) * sideways;
            aim = ahead.try_normalize().unwrap_or(travel);
        }

        // ── A collision ──────────────────────────────────────────────────────
        //
        // Knocked loose, it is where Rapier has it, facing the way the shove
        // turned it, until it has slid to a stop and stood a moment; then it
        // eases back onto its line from there.
        if let Some(mut c) = car.crash {
            facing_rate = Some(1.0e3);
            if c.recovering {
                c.t += dt;
                let w = (c.t / CRASH_RECOVER_S).clamp(0.0, 1.0);
                let w = w * w * (3.0 - 2.0 * w);
                pos = c.at.lerp(pos, w);
                aim = c.facing.lerp(aim, w).try_normalize().unwrap_or(aim);
            } else {
                physics.keep(handle);
                held = true;
                if let Some((at, facing, speed)) = physics.pose(handle) {
                    pos = at;
                    aim = facing;
                    c.at = at;
                    c.facing = facing;
                    c.t += dt;
                    let hold = if c.fall != 0.0 {
                        FALLEN_HOLD_S
                    } else {
                        CRASH_HOLD_S
                    };
                    // At rest — or pinned against something that keeps it
                    // sliding — it gets going again.
                    if c.t >= hold && (speed < 0.3 || c.t >= hold + 2.0) {
                        physics.settle(handle);
                        c.recovering = true;
                        c.t = 0.0;
                    }
                }
            }
            car.crash = (!c.recovering || c.t < CRASH_RECOVER_S).then_some(c);
        }

        // ── Draw ─────────────────────────────────────────────────────────────
        //
        // The drawn heading is not the path's tangent but a rate-limited chase
        // of it: a simplified centre line changes direction in steps, and a
        // vehicle cannot. See `TURN_RATE_PER_MS`.
        let was = car.heading;
        car.heading = if car.heading == Vec2::ZERO {
            aim
        } else {
            let rate =
                facing_rate.unwrap_or_else(|| (TURN_RATE_PER_MS * car.speed).max(TURN_RATE_FLOOR));
            rotate_toward(car.heading, aim, rate * dt)
        };
        let facing = car.heading;
        if walker {
            // Answering a wave: once stood and facing the waver, it waves;
            // when it is done (or gives up, or the waver is gone) it walks on.
            if let Some(mut w) = car.react.wave {
                w.t += dt;
                match w.waving.as_mut() {
                    Some(t) => *t += dt,
                    None => {
                        let facing_them = (w.at - pos)
                            .try_normalize()
                            .is_some_and(|to| facing.dot(to) > 0.94);
                        if w.t >= w.delay && car.speed < 0.12 && facing_them {
                            w.waving = Some(0.0);
                        }
                    }
                }
                let gone = w.at.distance(pos) > react_to::WAVE_REACH_M * 1.6;
                car.react.wave =
                    (!gone && w.step(wave_secs) != react_to::WaveStep::Done).then_some(w);
            }
            car.react.look(i as u64, pos, facing, people_at, dt);
            if let Some(avatar) = here_avatar {
                here.closest = here.closest.min(pos.distance(avatar));
                here.widest = here.widest.max(car.react.shift.abs());
                here_looking += usize::from(car.react.head.abs() > 0.1);
                here_waving += usize::from(car.react.wave.is_some_and(|w| w.waving.is_some()));
            }
        }
        // Signed yaw rate, rad/s, positive toward `right_of`.
        let yaw_rate = if was == Vec2::ZERO || dt <= 0.0 {
            0.0
        } else {
            (was.x * facing.y - was.y * facing.x)
                .clamp(-1.0, 1.0)
                .asin()
                / dt
        };

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
        // Its body driven toward where its way puts it — unless a collision
        // has it loose — and drawn where Rapier let it get to: pushed aside
        // by somebody, or held back.
        if !car.crash.is_some_and(|c| !c.recovering) {
            // Where the last step left it: a body moves continuously, so this
            // is drawn without the shake that its way plus a push had, and is
            // never somewhere the solver would not let it be.
            let body_at = physics.pose(handle).map(|p| p.0);
            let put = physics.place(
                handle,
                Vec3::new(pos.x, y, pos.y),
                facing,
                facing * car.speed,
                if walker { WALKER_SLACK_M } else { LANE_SLACK_M },
                dt,
            );
            if let (false, Some(at)) = (put, body_at) {
                pos = at;
            }
        }
        // `CARTO_TRAFFIC_OVERLAPS`'s smoothness count: how much the drawn
        // step changes frame to frame, for whoever is near the camera.
        if jitter.on && dt > 0.0 && pos.distance(cam) < 60.0 && car.drawn != Vec2::ZERO {
            let step = pos - car.drawn;
            if step.length() < 3.0 {
                let j = (step - car.drawn_step).length() / dt;
                let k = kind.index();
                jitter.sum[k] += j;
                jitter.max[k] = jitter.max[k].max(j);
                jitter.n[k] += 1;
                if j > 30.0 {
                    info!(
                        "traffic jump: {kind:?} {j:.0} m/s² dt {dt:.3} step {:.2} was {:.2} speed {:.1} turn {} crash {:?} push {:.2} held {held}",
                        step.length(),
                        car.drawn_step.length(),
                        car.speed,
                        car.turn.is_some(),
                        car.crash.map(|c| (c.recovering, c.t)),
                        push.length()
                    );
                }
            }
            // A step of 3 m or more is a teleport, not motion.
            car.drawn_step = if step.length() < 3.0 {
                step
            } else {
                Vec2::ZERO
            };
        }
        car.drawn = pos;
        // The stuck report: a vehicle standing near the camera, and why.
        if jitter.on && kind.drives() && pos.distance(cam) < 60.0 {
            if car.speed < 0.2 {
                car.stuck += dt;
                let s = car.stuck;
                if s >= 3.0 && (s - dt) % 3.0 > s % 3.0 {
                    info!(
                        "traffic stuck: {kind:?} {s:.0} s, held by {} (gap {:.1}), turn {}, crash {}, waited {:.1}",
                        car.why.0,
                        car.why.1,
                        car.turn.is_some(),
                        car.crash.is_some(),
                        car.waited
                    );
                }
            } else {
                car.stuck = 0.0;
            }
        }
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
        let parts = car.parts;
        match parts {
            AgentParts::Car { tail } => {
                car.tilt = body_tilt(car.tilt, accel_now, car.speed * yaw_rate, dt);
                tf.rotation = tf.rotation
                    * Quat::from_rotation_x(car.tilt.x)
                    * Quat::from_rotation_z(car.tilt.y);
                let braking = brake_lamps(car.braking, accel_now, held);
                if braking != car.braking {
                    car.braking = braking;
                    if let (Some(mats), Ok(mut m)) = (tail_materials.as_ref(), tails.get_mut(tail))
                    {
                        m.0 = if braking {
                            mats.on.clone()
                        } else {
                            mats.off.clone()
                        };
                    }
                }
            }
            AgentParts::Walker => {
                // Knocked down: over onto its side about its feet, a little
                // further than a bike, which its pedal and bar hold up.
                let down = fallen(car.crash);
                if down != 0.0 {
                    tf.rotation *= Quat::from_rotation_z(down * PERSON_FALL_ANGLE);
                }
                use crate::systems::crowd;
                // Its body is not made yet: it walks, unseen.
                let Some((mut figure, mut mesh, mut material, mut tag)) = dressed else {
                    vis.set_if_neq(Visibility::Hidden);
                    continue;
                };
                let Some(body) = crowd.body(figure.body(&crowd)) else {
                    vis.set_if_neq(Visibility::Hidden);
                    continue;
                };
                // The stride is walked, not timed: the phase advances by distance,
                // so the feet keep pace with the ground at every speed. A taller
                // figure takes longer steps.
                let stride = body.stride * tf.scale.y;
                car.phase =
                    (car.phase + stepped / stride * std::f32::consts::TAU) % std::f32::consts::TAU;
                let near = crowd::wants_near(&figure, (pos - cam).length());
                if near != figure.near {
                    figure.near = near;
                    (mesh.0, material.0) = body.wear(if near { 0 } else { usize::MAX }, &figure);
                }
                // Slowing to a stop eases from the stride into standing.
                let standing =
                    1.0 - ((car.speed + cross_speed) / (WALK_SPEED * 0.5)).clamp(0.0, 1.0);
                let pose = match car.react.wave.map(|w| w.step(wave_secs)) {
                    Some(react_to::WaveStep::Waving { progress, blend }) => {
                        crowd::BodyPose::Wave { progress, blend }
                    }
                    _ => crowd::BodyPose::Walk {
                        phase: car.phase,
                        standing,
                    },
                };
                let t = crowd::tag(pose, car.react.head, &figure);
                if tag.value != t {
                    tag.value = t;
                }
            }
            AgentParts::Cyclist {
                bike,
                crank,
                wheels,
            } => {
                use crate::systems::crowd;
                // Into the turn, as far as the turn asks — the opposite way to a
                // car body's roll.
                car.tilt.y = bike_lean(car.tilt.y, car.speed * yaw_rate, dt);
                let body = dressed
                    .as_ref()
                    .and_then(|(figure, ..)| crowd.body(figure.body(&crowd)));
                // Held at a light or in a queue, the rider puts a foot down and
                // the bike leans onto it; moving off, the foot comes back up.
                let stop_lean = body.and_then(|b| b.stop_lean);
                car.foot_down = foot_down(car.foot_down, held && stop_lean.is_some(), dt);
                let leaning = stop_lean.unwrap_or(0.0) * car.foot_down;
                // Knocked off: bike and rider go over onto their side about
                // the line the wheels stand on, lie there, and get back up.
                let down = fallen(car.crash);
                if down != 0.0 {
                    car.tilt.y = 0.0;
                }
                tf.rotation *= Quat::from_rotation_z(car.tilt.y - leaning + down * FALL_ANGLE);
                let Some((mut figure, mut mesh, mut material, mut tag)) = dressed else {
                    vis.set_if_neq(Visibility::Hidden);
                    continue;
                };
                let Some(body) = body else {
                    vis.set_if_neq(Visibility::Hidden);
                    continue;
                };
                // The bike is the rider's size: its every length scaled to the
                // legs the rider was posed onto it with.
                let scale = body.bike_scale;
                let fit = cartopolis_geo::agent_models::BIKE_FIT;
                // Pedalled by distance, a turn of the cranks per gear's
                // development, so the feet keep pace with the road; slowing for a
                // corner or a light, the rider stops pedalling and coasts.
                let ride_scale = scale * tf.scale.y;
                let fat = kind == AgentKind::Fatbike;
                if car.foot_down > 0.0 {
                    // The right pedal back up and forward, where the stopped
                    // pose has the foot, ready to push off.
                    car.phase = toward_angle(car.phase, STOP_CRANK, 4.0 * dt);
                } else if fat {
                    // The motor does the work: the feet rest on level cranks.
                    car.phase = std::f32::consts::FRAC_PI_2;
                } else if accel_now > -0.3 && !held {
                    car.phase = (car.phase
                        + stepped / (fit.development * ride_scale) * std::f32::consts::TAU)
                        % std::f32::consts::TAU;
                }
                let wheel_r = if fat {
                    cartopolis_geo::agent_models::FATBIKE_WHEEL_R
                } else {
                    fit.wheel_r
                };
                car.roll = (car.roll + stepped / (wheel_r * ride_scale)) % std::f32::consts::TAU;
                if let Ok(mut t) = bike_parts.get_mut(bike) {
                    if t.scale.x != scale {
                        t.scale = Vec3::splat(scale);
                    }
                }
                if let Ok(mut t) = bike_parts.get_mut(crank) {
                    t.rotation = Quat::from_rotation_x(car.phase);
                }
                for wheel in wheels {
                    if let Ok(mut t) = bike_parts.get_mut(wheel) {
                        t.rotation = Quat::from_rotation_x(car.roll);
                    }
                }
                let near = crowd::wants_near(&figure, (pos - cam).length());
                if near != figure.near {
                    figure.near = near;
                    (mesh.0, material.0) = body.wear(if near { 0 } else { usize::MAX }, &figure);
                }
                // The foot goes down by blending into the riders' "standing"
                // pose, which is the stopped one (`crowd::upload`).
                let f = car.foot_down;
                let t = crowd::tag(
                    crowd::BodyPose::Walk {
                        phase: car.phase,
                        standing: f * f * (3.0 - 2.0 * f),
                    },
                    0.0,
                    &figure,
                );
                if tag.value != t {
                    tag.value = t;
                }
            }
        }
        *vis = Visibility::Inherited;
    }

    // `CARTO_CROWD_HERE`'s report, once a second of the moving world's clock:
    // what a capture can only show, measured.
    if let Some(avatar) = here_avatar {
        here.looking = here.looking.max(here_looking);
        here.waving = here.waving.max(here_waving);
        if elapsed >= here.reported + 1.0 {
            info!(
                "CARTO_CROWD_HERE: t {elapsed:.1} s, avatar at ({:.1}, {:.1}): nearest walker \
                 {:.2} m, widest step aside {:.2} m, up to {} looking, {} waving",
                avatar.x, avatar.y, here.closest, here.widest, here.looking, here.waving
            );
            here.reported = elapsed;
            here.closest = f32::MAX;
            here.widest = 0.0;
            here.looking = 0;
            here.waving = 0;
        }
    }
    // `CARTO_TRAFFIC_HERE`'s report: how near the sent cars came, and how fast
    // the nearest was going.
    if report_traffic {
        traffic_here.reported = elapsed;
        match nearest_sent {
            Some((d, v)) => info!(
                "CARTO_TRAFFIC_HERE: t {elapsed:.1} s: nearest sent car {d:.2} m from the \
                 avatar, at {v:.2} m/s"
            ),
            None => info!("CARTO_TRAFFIC_HERE: t {elapsed:.1} s: no sent car on the road"),
        }
    }
}

/// Step the traffic's Rapier world, and make a collision of every pair that
/// began touching at more than [`CRASH_CLOSING`] — two people on foot only
/// ever bump. Both are knocked loose with the velocities they had, and
/// Rapier shoves them apart by their masses: a car barely checks, the bike
/// it hit is thrown aside. An avatar takes part but is not moved: where it
/// is, the server says.
pub fn collide_traffic(
    time: Res<crate::systems::anim_clock::AnimClock>,
    mut physics: ResMut<TrafficPhysics>,
    mut cars: Query<(&mut Car, &AgentKind)>,
    mut report: Local<(Option<bool>, f32)>,
    people: Res<crate::systems::crowd_react::CrowdPeople>,
    mut knock_every: Local<Option<f32>>,
    mut knock_next: Local<f32>,
) {
    let touches = physics.step(time.delta_secs());
    // `CARTO_KNOCK_RIDER=<s>`: every that many seconds of the moving world's
    // clock, the rider nearest the avatar (within 30 m) is knocked off as a
    // car would knock it, so a capture can show a fall without waiting for
    // one. A capture's hook, not something a session runs.
    let every = *knock_every.get_or_insert_with(|| {
        std::env::var("CARTO_KNOCK_RIDER")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .filter(|s| *s > 0.0)
            .unwrap_or(0.0)
    });
    if every > 0.0 && time.elapsed_secs() >= *knock_next {
        if let Some(&avatar) = people.at.first() {
            let nearest = cars
                .iter()
                .filter(|(c, k)| {
                    matches!(k, AgentKind::Cyclist | AgentKind::Fatbike) && c.crash.is_none()
                })
                .filter_map(|(c, _)| {
                    let h = c.body?;
                    let (at, ..) = physics.pose(h)?;
                    Some((h, at.distance(avatar)))
                })
                .filter(|(_, d)| *d < 30.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(h, _)| h);
            if let Some(h) = nearest {
                if let Some((mut car, &kind)) = cars.iter_mut().find(|(c, _)| c.body == Some(h)) {
                    let push = right_of(car.heading) * 2.0 + car.heading * car.speed * 0.5;
                    physics.knock(h, BodyKind::Agent(kind), push);
                    car.speed = 0.0;
                    car.foot_down = 0.0;
                    car.crash = Some(Crash {
                        t: 0.0,
                        recovering: false,
                        at: Vec2::ZERO,
                        facing: car.heading,
                        fall: 1.0,
                    });
                    info!("CARTO_KNOCK_RIDER: a {kind:?} knocked off");
                    *knock_next = time.elapsed_secs() + every;
                }
            }
        }
    }
    // Two deep inside each other that did not collide were put down so —
    // two of a pool dropped on one spot, or handed onto one line at once —
    // and their ways keep them there, so Rapier's push never wins. The later
    // of the two is taken off the road and put down again where it is clear.
    for (ta, tb, _) in physics.overlaps(RETIRE_DEPTH_M) {
        if TrafficPhysics::is_avatar(ta) || TrafficPhysics::is_avatar(tb) {
            continue;
        }
        let Some(later) = Entity::try_from_bits(ta.max(tb) as u64) else {
            continue;
        };
        let Some(other) = Entity::try_from_bits(ta.min(tb) as u64) else {
            continue;
        };
        let walkers = [later, other]
            .iter()
            .all(|e| cars.get(*e).is_ok_and(|(_, k)| *k == AgentKind::Walker));
        let crashed = [later, other]
            .iter()
            .any(|e| cars.get(*e).is_ok_and(|(c, _)| c.crash.is_some()));
        if walkers || crashed {
            continue;
        }
        if let Ok((mut car, _)) = cars.get_mut(later) {
            car.line = None;
            car.turn = None;
        }
    }
    // `CARTO_TRAFFIC_OVERLAPS=1`: once a second of the moving world's clock,
    // how many pairs are drawn inside each other (deeper than 0.2 m, not two
    // walkers, not in a collision), and how the first few stand: a capture's
    // measure of whatever still passes through something.
    let elapsed = time.elapsed_secs();
    if *report
        .0
        .get_or_insert_with(|| crate::debug_switches::env_flag("CARTO_TRAFFIC_OVERLAPS"))
        && elapsed >= report.1 + 1.0
    {
        report.1 = elapsed;
        let found = |tag: u128| {
            (!TrafficPhysics::is_avatar(tag))
                .then(|| Entity::try_from_bits(tag as u64))
                .flatten()
                .and_then(|e| cars.get(e).ok())
        };
        let mut n = 0;
        let mut seen = Vec::new();
        for (ta, tb, depth) in physics.overlaps(0.2) {
            let (Some((a, ka)), Some((b, kb))) = (found(ta), found(tb)) else {
                continue;
            };
            if (*ka == AgentKind::Walker && *kb == AgentKind::Walker)
                || a.crash.is_some()
                || b.crash.is_some()
            {
                continue;
            }
            n += 1;
            if seen.len() < 6 {
                let pa = a.body.and_then(|h| physics.pose(h));
                let pb = b.body.and_then(|h| physics.pose(h));
                let (along, across) = match (pa, pb) {
                    (Some(pa), Some(pb)) => {
                        let rel = pb.0 - pa.0;
                        (rel.dot(pa.1), rel.dot(right_of(pa.1)))
                    }
                    _ => (f32::NAN, f32::NAN),
                };
                seen.push(format!(
                    "{ka:?}@{:.1}{} / {kb:?}@{:.1}{} {depth:.2} m deep, headings {:.2}, \
                     b {along:.1} m ahead {across:.1} m right, same line {}",
                    a.speed,
                    if a.turn.is_some() { " turning" } else { "" },
                    b.speed,
                    if b.turn.is_some() { " turning" } else { "" },
                    a.heading.dot(b.heading),
                    a.line.is_some() && a.line == b.line,
                ));
            }
        }
        info!("traffic overlaps: {n} at {elapsed:.1} s");
        for s in seen {
            info!("traffic overlap: {s}");
        }
    }
    for t in touches {
        let entity = |tag: u128| {
            (!TrafficPhysics::is_avatar(tag))
                .then(|| Entity::try_from_bits(tag as u64))
                .flatten()
        };
        let (ea, eb) = (entity(t.a), entity(t.b));
        // Velocity, what it is, and whether a collision already has it.
        let state = |e: Option<Entity>| match e.and_then(|e| cars.get(e).ok()) {
            Some((car, &kind)) => (
                car.heading * car.speed,
                BodyKind::Agent(kind),
                car.crash.is_some(),
                car.driven < SETTLE_IN_S,
                car.body.and_then(|h| physics.pose(h)).map(|p| p.0),
            ),
            None => (Vec2::ZERO, BodyKind::Avatar, false, false, None),
        };
        let (a, b) = (state(ea), state(eb));
        // Two on foot only bump; one just put down, or still in a collision
        // or getting going after one, is not hit again.
        if (a.1.on_foot() && b.1.on_foot()) || a.2 || b.2 || a.3 || b.3 {
            continue;
        }
        // Two going opposite ways and more than [`ONCOMING_BRUSH_M`] apart
        // across their way pass each other: on a narrow street their boxes
        // graze at the corners, which is not a head-on collision.
        if let (Some(pa), Some(pb), Some(dir)) = (a.4, b.4, a.0.try_normalize()) {
            let oncoming = b.0.try_normalize().is_some_and(|d| d.dot(dir) < -0.7);
            let rel = pb - pa;
            if oncoming && (rel.x * dir.y - rel.y * dir.x).abs() > ONCOMING_BRUSH_M {
                continue;
            }
        }
        let rel = a.0 - b.0;
        let closing = if t.normal == Vec2::ZERO {
            rel.length()
        } else {
            rel.dot(t.normal).abs()
        };
        if closing < CRASH_CLOSING {
            continue;
        }
        info!(
            "traffic collision: {:?} and {:?} at {closing:.1} m/s (headings {:.2}, normal across the first {:.2})",
            a.1,
            b.1,
            a.0.normalize_or_zero().dot(b.0.normalize_or_zero()),
            t.normal.dot(a.0.normalize_or_zero().perp()).abs()
        );
        for (e, (velocity, kind, already, _, at), (_, other, _, _, other_at)) in
            [(ea, a, b), (eb, b, a)]
        {
            let Some(e) = e else { continue };
            if already {
                continue;
            }
            let Ok((mut car, _)) = cars.get_mut(e) else {
                continue;
            };
            let Some(handle) = car.body else { continue };
            physics.knock(handle, kind, velocity);
            car.speed = 0.0;
            // A rider hit by a car, or hitting anything hard, goes down,
            // away from what hit it.
            let rider = matches!(
                kind,
                BodyKind::Agent(AgentKind::Cyclist | AgentKind::Fatbike)
            );
            // Somebody on foot hit by anything on wheels goes down too.
            let walker_hit = kind == BodyKind::Agent(AgentKind::Walker) && !other.on_foot();
            let fall = if walker_hit
                || (rider && (other == BodyKind::Agent(AgentKind::Car) || closing > FALL_CLOSING))
            {
                let side = match (at, other_at) {
                    (Some(me), Some(them)) => (them - me).dot(right_of(car.heading)),
                    _ => 0.0,
                };
                if side > 0.0 {
                    -1.0
                } else {
                    1.0
                }
            } else {
                0.0
            };
            car.foot_down = 0.0;
            car.crash = Some(Crash {
                t: 0.0,
                recovering: false,
                at: Vec2::ZERO,
                facing: car.heading,
                fall,
            });
        }
    }
}

/// How far ahead a car looks for a signal, metres.
const SIGNAL_LOOKAHEAD_M: f32 = 45.0;

/// How far short of a signal's node a waiting car's centre stops. The node is
/// the stop line, and the nose is half a car in front of the centre.
const STOP_SHORT_M: f32 = 4.0;

/// The deceleration a car plans a stop at a light with, m/s² — gentler than
/// [`CAR_BRAKE`], which is the most the easing allows, so the stop is a glide
/// rather than a lurch and the easing never falls behind the plan.
const SIGNAL_BRAKE: f32 = 3.0;

/// The hardest a driver will brake for a light. A car that would need more is
/// committed and drives through on the amber, as a real one caught by it does —
/// stopping it anyway puts it in the middle of the junction.
const SIGNAL_HARD_BRAKE: f32 = 7.0;

/// Road over which a car ignores signals after driving through one. OSM puts a
/// stop line on each approach, so a car that has crossed on green has the
/// *opposite* approach's line right ahead of it, in its own phase group.
const SIGNAL_GRACE_M: f32 = 30.0;

/// The fastest a car may go given the signals ahead of it: a braking curve
/// that ends at the stop line of a light showing amber or red, or no limit.
///
/// Only signals in the car's own phase group (`street_detail::signal_group` of
/// its heading) and within `reach` of its line count — that is what keeps a
/// cross street's lights, ten metres to the side at the same junction, from
/// stopping it. `grace` is [`Car::signal_grace`]: set here when the car is
/// found just past a signal. Each signal is (stop line, phase group, held
/// red by a pressed button — `give_way::SignalButtons`).
fn signal_speed(
    signals: &[(Vec2, u8, bool)],
    elapsed: f32,
    pos: Vec2,
    travel: Vec2,
    speed: f32,
    reach: f32,
    grace: &mut f32,
) -> f32 {
    use crate::systems::street_detail::{signal_group, signal_says_stop};
    let group = signal_group(travel);
    let mut limit = f32::MAX;
    for &(at, g, pressed) in signals {
        if g != group {
            continue;
        }
        let rel = at - pos;
        let ahead = rel.dot(travel);
        let lateral = (rel.x * travel.y - rel.y * travel.x).abs();
        if lateral > reach || ahead > SIGNAL_LOOKAHEAD_M {
            continue;
        }
        if ahead <= 0.0 {
            if ahead > -3.0 {
                *grace = SIGNAL_GRACE_M;
            }
            continue;
        }
        if ahead < *grace || !(pressed || signal_says_stop(elapsed, g)) {
            continue;
        }
        let gap = ahead - STOP_SHORT_M;
        if gap < -1.5 {
            // Its nose is over the line: it goes.
            continue;
        }
        let gap = gap.max(0.0);
        if speed * speed / (2.0 * SIGNAL_HARD_BRAKE) > gap + 0.5 {
            continue;
        }
        limit = limit.min((2.0 * SIGNAL_BRAKE * gap).sqrt());
    }
    limit
}

/// Pitch per m/s² of acceleration, and roll per m/s² of cornering, radians.
/// About a degree and a half at a firm stop and two in a brisk corner — what a
/// family car's suspension gives, and enough to read from across a street.
const PITCH_PER_ACCEL: f32 = 0.005;
const ROLL_PER_LAT_ACCEL: f32 = 0.012;
const MAX_TILT: f32 = 0.05;
/// How quickly the body settles to its lean, seconds.
const TILT_TAU: f32 = 0.25;

/// Ease a car body's (pitch, roll) toward what its acceleration and cornering
/// ask for. The nose dips under braking and lifts pulling away; the body leans
/// out of a turn. `lateral` is speed × yaw rate, positive turning toward
/// [`right_of`].
///
/// The signs are the model's: +z is the nose, so a positive rotation about x
/// dips it; +x is the car's left, so a turn to the right (outside on the left)
/// rolls the roof toward +x, which is a negative rotation about z.
fn body_tilt(tilt: Vec2, accel: f32, lateral: f32, dt: f32) -> Vec2 {
    let target = Vec2::new(-accel * PITCH_PER_ACCEL, -lateral * ROLL_PER_LAT_ACCEL)
        .clamp(Vec2::splat(-MAX_TILT), Vec2::splat(MAX_TILT));
    if dt <= 0.0 {
        return tilt;
    }
    tilt + (target - tilt) * (1.0 - (-dt / TILT_TAU).exp())
}

/// Ease a rider's lean toward what the turn asks: the angle whose tangent is
/// the lateral acceleration over gravity, so a bike and rider stay balanced
/// round the bend. `lateral` is speed × yaw rate, positive turning toward
/// [`right_of`]; turning right the bike leans its top toward `-x`, the rider's
/// right, which is a positive turn about `z` — the other way to a car body's
/// roll ([`body_tilt`]).
fn bike_lean(lean: f32, lateral: f32, dt: f32) -> f32 {
    let target = (lateral / 9.81).atan().clamp(-MAX_BIKE_LEAN, MAX_BIKE_LEAN);
    if dt <= 0.0 {
        return lean;
    }
    lean + (target - lean) * (1.0 - (-dt / TILT_TAU).exp())
}

/// Where the right crank points in a stopped rider's pose, radians from the
/// top, forward: an eighth of a turn, where `cartopolis_human::crowd` bakes
/// the stopped pose's right foot.
const STOP_CRANK: f32 = std::f32::consts::FRAC_PI_4;
/// Seconds for a held rider's foot to go down, and to come back up.
const FOOT_DOWN_S: f32 = 0.7;
const FOOT_UP_S: f32 = 0.35;

/// Ease how far a rider's foot is down (0 riding … 1 on the road) toward
/// down while `held`, up otherwise: slowly down, so a moment's check at a
/// corner does not put it out, and quickly up as the bike moves off.
fn foot_down(now: f32, held: bool, dt: f32) -> f32 {
    if held {
        (now + dt / FOOT_DOWN_S).min(1.0)
    } else {
        (now - dt / FOOT_UP_S).max(0.0)
    }
}

/// Turn the angle `from` toward `to` by at most `step`, the short way round,
/// in `0..TAU`.
fn toward_angle(from: f32, to: f32, step: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let d = (to - from + PI).rem_euclid(TAU) - PI;
    (from + d.clamp(-step, step)).rem_euclid(TAU)
}

/// Whether a car's brake lamps are lit: while it slows harder than it would
/// rolling, and while it waits at a stop. The two thresholds differ so a car
/// easing off does not flicker its lamps every frame.
fn brake_lamps(lit: bool, accel: f32, held: bool) -> bool {
    if held {
        return true;
    }
    if lit {
        accel < -0.3
    } else {
        accel < -0.9
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
    Some((
        centre + right_of(travel) * car.offset(kind, l),
        travel,
        deck,
    ))
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

/// The speed the vehicle in front leaves this one this frame, by the IDM
/// ([`Idm`]): the nearest of its kind ahead on its line going its way, from
/// `leaders` (each: distance along, speed). `v` is its speed, `v0` the most
/// it would do, `length` the two bodies' half lengths together; and whether
/// that is harder braking than its own.
#[allow(clippy::too_many_arguments)]
fn follow_speed(
    leaders: &HashMap<(LineKey, i8), Vec<(f32, f32)>>,
    key: LineKey,
    dir: f32,
    s: f32,
    v: f32,
    v0: f32,
    length: f32,
    idm: Idm,
    dt: f32,
) -> (f32, bool) {
    let lead = leaders.get(&(key, lane_key(dir))).and_then(|others| {
        others
            .iter()
            .filter(|(o, _)| *o > s + 1e-3)
            .min_by(|a, b| a.0.total_cmp(&b.0))
    });
    let leader = lead.map(|&(o, lv)| (o - s - length, v - lv));
    let a = idm.accel(v, v0, leader);
    if a >= 0.0 {
        return (v0, false);
    }
    ((v + a * dt.max(1.0 / 60.0)).max(0.0), a < -idm.b * 1.5)
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
    free: &dyn Fn(&WorldDriveLine, f32, f32) -> bool,
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
        // A pavement is two-way whatever the carriageway beside it does —
        // which is what somebody walking up a one-way street is doing.
        let flow = if kind.ignores_oneway() {
            Flow::Both
        } else {
            line.flow
        };
        let dir = match flow {
            Flow::Both => {
                if rand01(0xB1D1, agent, seed) < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            Flow::Forward => 1.0,
            Flow::Backward => -1.0,
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
        // Never inside another body, of any kind: two put down on one spot
        // are drawn through each other until one drives off.
        let free = free(line, along, dir);
        if clear && free {
            return Some((key, dir, s));
        }
        if free {
            fallback.get_or_insert((key, dir, s));
        }
    }
    // Every draw was near one of its own kind. Close behind another beats
    // nowhere: an agent with no line is hidden, and a hidden agent is one the
    // pool has lost. Inside another does not: it waits for the next frame.
    fallback
}

/// How far the car body's origin sits above the road surface.
///
/// The models (`cartopolis_geo::agent_models`) have their origin on the ground
/// between the wheels, so this is only the clearance that keeps them
/// off a coplanar fight with the road it is standing on. The road ribbons win
/// their own depth fight through `systems::road_depth`; a car is ordinary
/// geometry and just needs to be above them.
const CAR_RIDE_HEIGHT: f32 = 0.02;

/// One of `cartopolis_geo::agent_models`' meshes as a bevy `Mesh`.
///
/// Local rather than reaching into `map_geometry::build_mesh`, which also folds in
/// the terrain refinement a cell's ground needs and an agent's model does not.
pub(crate) fn model_mesh_to_bevy(m: &cartopolis_geo::agent_models::ModelMesh) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::render::mesh::{Indices, PrimitiveTopology};
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, m.positions.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, m.colors.clone());
    mesh.insert_indices(Indices::U32(m.indices.clone()));
    mesh
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod crossing_tests {
    use super::*;
    use crate::systems::street_detail::ZebraPoint;

    fn street(half_width: f32, flow: Flow) -> DriveNetwork {
        let mut net = DriveNetwork::default();
        net.insert_cell(
            (0, 0),
            0.0,
            0.0,
            vec![DriveLine {
                points: vec![[0.0, 0.0], [100.0, 0.0]],
                flow,
                speed: 8.0,
                half_width,
                uses: Uses {
                    drive: true,
                    walk: true,
                    cycle: true,
                },
                deck: vec![f32::NAN; 2],
                on_deck: false,
                pavements: Pavements::BOTH,
            }],
        );
        net
    }

    const KEY: LineKey = LineKey {
        cell: (0, 0),
        index: 0,
    };

    /// A walker on the pavement takes up the zebra just ahead across its own
    /// street, and not one behind it, nor one across the side street.
    #[test]
    fn a_walker_finds_the_crossing_just_ahead() {
        let hw = 3.0;
        // On the right-hand pavement going east.
        let p = Vec2::new(40.0, hw + PAVEMENT_CLEARANCE_M);
        let across = |x: f32| ZebraPoint {
            at: Vec2::new(x, 0.0),
            road: Vec2::X,
            half_width: hw,
        };
        assert_eq!(
            crossing_ahead(p, Vec2::X, hw, &[across(41.5)], &[]),
            Some((Vec2::new(41.5, 0.0), None))
        );
        assert_eq!(
            crossing_ahead(p, Vec2::X, hw, &[across(38.0)], &[]),
            None,
            "behind"
        );
        assert_eq!(
            crossing_ahead(p, Vec2::X, hw, &[across(50.0)], &[]),
            None,
            "not yet"
        );
        let side_street = ZebraPoint {
            at: Vec2::new(41.0, 12.0),
            road: Vec2::Y,
            half_width: hw,
        };
        assert_eq!(crossing_ahead(p, Vec2::X, hw, &[side_street], &[]), None);
        // A light on this street's traffic (east–west is group 1), not the
        // cross street's.
        let lights = [
            (Vec2::new(41.0, 0.0), 0, false),
            (Vec2::new(41.2, 0.5), 1, false),
        ];
        assert_eq!(
            crossing_ahead(p, Vec2::X, hw, &[], &lights),
            Some((Vec2::new(41.2, 0.5), Some(1)))
        );
    }

    /// A walker steps out only when nothing is on the crossing and every
    /// vehicle coming could stop for it.
    #[test]
    fn a_walker_waits_for_a_gap() {
        let at = Vec2::new(50.0, 0.0);
        let east = Vec2::X;
        assert!(safe_to_cross(at, &[], 4.0));
        // 40 m off at 8 m/s stops in under 13 m: it will stop.
        assert!(safe_to_cross(at, &[(Vec2::new(10.0, 1.3), east, 8.0)], 4.0));
        // 15 m off at 12 m/s would need 29 m.
        assert!(!safe_to_cross(
            at,
            &[(Vec2::new(35.0, 1.3), east, 12.0)],
            4.0
        ));
        // Waiting on the stripes, or with its nose over them.
        assert!(!safe_to_cross(
            at,
            &[(Vec2::new(49.0, 1.3), east, 0.0)],
            4.0
        ));
        // Past it, going away; or on another road altogether.
        assert!(safe_to_cross(
            at,
            &[(Vec2::new(60.0, 1.3), east, 12.0)],
            4.0
        ));
        assert!(safe_to_cross(
            at,
            &[(Vec2::new(40.0, 30.0), east, 12.0)],
            4.0
        ));
        // A queue held at the zebra, standing.
        assert!(safe_to_cross(at, &[(Vec2::new(44.0, 1.3), east, 0.0)], 4.0));
    }

    /// Across, a walker goes straight over from its own pavement to the
    /// other, through the middle of the road.
    #[test]
    fn a_crossing_walks_from_kerb_to_kerb() {
        let net = street(3.0, Flow::Both);
        let l = net.get(KEY).unwrap();
        let mut w = tests_walker();
        let start = w.offset(AgentKind::Walker, l);
        assert!(start > l.half_width, "on the pavement: {start}");
        for (p, want) in [(0.0, start), (0.5, 0.0), (1.0, -start)] {
            w.crossing = Some(Crossing {
                at: Vec2::new(50.0, 0.0),
                signal: None,
                waited: 1.0,
                progress: Some(p),
            });
            let got = w.offset(AgentKind::Walker, l);
            assert!((got - want).abs() < 1e-4, "at {p}: {got} not {want}");
        }
        // Waiting, it is still on its pavement.
        w.crossing = Some(Crossing {
            at: Vec2::new(50.0, 0.0),
            signal: None,
            waited: 0.0,
            progress: None,
        });
        assert_eq!(w.offset(AgentKind::Walker, l), start);
    }

    fn tests_walker() -> Car {
        Car {
            line: Some(KEY),
            s: 50.0,
            dir: 1.0,
            speed_scale: 1.0,
            speed: 1.0,
            heading: Vec2::ZERO,
            turn: None,
            side: 1.0,
            lateral: 0.0,
            phase: 0.0,
            tilt: Vec2::ZERO,
            roll: 0.0,
            signal_grace: 0.0,
            braking: false,
            parts: AgentParts::Walker,
            react: Default::default(),
            pinned: None,
            pavement: false,
            defiant: false,
            crossing: None,
            last_cross: None,
            foot_down: 0.0,
            ordinal: 0,
            size: None,
            body: None,
            crash: None,
            waited: 0.0,
            driven: 0.0,
            drawn: Vec2::ZERO,
            drawn_step: Vec2::ZERO,
            why: ("free", f32::NAN),
            stuck: 0.0,
        }
    }

    /// A car passes a cyclist where it can without taking the oncoming lane,
    /// and stays behind it where it cannot.
    #[test]
    fn a_car_passes_a_cyclist_only_with_room() {
        // A narrow two-way residential street: it follows.
        let narrow = street(2.75, Flow::Both);
        let l = narrow.get(KEY).unwrap();
        let (shift, keep) = pass_cyclist(l, 10.0, 8.0);
        assert_eq!(shift, 0.0);
        assert!(keep < 8.0 && keep > CYCLE_SPEED, "closing gently: {keep}");
        let (_, keep) = pass_cyclist(l, 8.3, 8.0);
        assert!(
            (keep - CYCLE_SPEED).abs() < 0.1,
            "settles at the bike's pace: {keep}"
        );
        assert_eq!(pass_cyclist(l, 4.0, 8.0).1, 0.0, "never into it");
        // The same width one-way: it moves over and keeps its speed.
        let oneway = street(2.75, Flow::Forward);
        let l = oneway.get(KEY).unwrap();
        let (shift, keep) = pass_cyclist(l, 10.0, 8.0);
        assert!(shift < 0.0 && keep == 8.0, "{shift} {keep}");
        let lane = AgentKind::Car.offset(l) + shift;
        let bike = AgentKind::Cyclist.offset(l);
        assert!(bike - lane >= 0.95 + 0.35 + PASS_CLEARANCE_M - 1e-4);
        // A wide two-way street: room enough in its own lane, or nearly.
        let wide = street(5.0, Flow::Both);
        let l = wide.get(KEY).unwrap();
        let (shift, keep) = pass_cyclist(l, 10.0, 12.0);
        assert!(shift <= 0.0 && keep == 12.0);
    }

    /// A walker steps round the one coming the other way and the one it is
    /// catching up, not round the one beside it at its pace, nor itself.
    #[test]
    fn walkers_make_way_for_the_walkers_they_meet() {
        let mut grid: HashMap<(i32, i32), Vec<(usize, Vec2, Vec2)>> = HashMap::new();
        let me = Vec2::new(10.0, 10.0);
        let v = Vec2::new(1.25, 0.0);
        for (i, at, vel) in [
            (0, me, v),
            (1, Vec2::new(14.0, 10.2), -v),
            (2, Vec2::new(13.0, 10.0), v * 0.5),
            (3, Vec2::new(10.5, 10.6), v),
        ] {
            grid.entry(walk_cell(at)).or_default().push((i, at, vel));
        }
        let mut out = Vec::new();
        closing_walkers(&grid, 0, me, v, &mut out);
        out.sort_by(|a, b| a.x.total_cmp(&b.x));
        assert_eq!(out, vec![Vec2::new(13.0, 10.0), Vec2::new(14.0, 10.2)]);
        // Standing, it makes way for nobody: the others go round it.
        out.clear();
        closing_walkers(&grid, 0, me, Vec2::ZERO, &mut out);
        assert!(out.is_empty());
    }
}

#[cfg(test)]
mod rider_tests {
    use super::*;

    /// A held rider's foot goes down over most of a second and comes back
    /// up in about a third of one; a moment's hold barely moves it.
    #[test]
    fn a_held_rider_puts_a_foot_down_and_picks_it_up() {
        let mut f = 0.0;
        for _ in 0..10 {
            f = foot_down(f, true, 0.02);
        }
        assert!(f < 0.35, "a fifth of a second held: {f}");
        for _ in 0..40 {
            f = foot_down(f, true, 0.02);
        }
        assert_eq!(f, 1.0);
        for _ in 0..18 {
            f = foot_down(f, false, 0.02);
        }
        assert_eq!(f, 0.0);
    }

    /// The cranks come round to the stopped pose's pedal the short way.
    #[test]
    fn the_cranks_come_round_the_short_way() {
        use std::f32::consts::TAU;
        let a = toward_angle(TAU - 0.1, 0.2, 0.05);
        assert!((a - (TAU - 0.05)).abs() < 1e-5, "{a}");
        let mut a = 3.0;
        for _ in 0..100 {
            a = toward_angle(a, STOP_CRANK, 0.1);
        }
        assert!((a - STOP_CRANK).abs() < 1e-5);
    }
}

#[cfg(test)]
mod track_tests {
    use super::*;

    fn way(points: &[[f32; 2]], drive: bool, cycle: bool) -> DriveLine {
        DriveLine {
            points: points.to_vec(),
            flow: Flow::Both,
            speed: if drive { 13.9 } else { 0.0 },
            half_width: if drive { 4.0 } else { 1.5 },
            uses: Uses {
                drive,
                walk: true,
                cycle,
            },
            deck: vec![f32::NAN; points.len()],
            on_deck: false,
            pavements: Pavements::BOTH,
        }
    }

    /// A street with a cycle track along it loses its cyclists to the track;
    /// one with a track only crossing it, or only along a short stretch, or
    /// none, keeps them — and the street gets them back when the track goes.
    #[test]
    fn cyclists_take_the_track_beside_the_street() {
        let mut net = DriveNetwork::default();
        net.insert_cell(
            (0, 0),
            0.0,
            0.0,
            vec![
                // A main road with a track 9 m off its centre line.
                way(&[[0.0, 0.0], [200.0, 0.0]], true, true),
                way(&[[0.0, 9.0], [200.0, 9.0]], false, true),
                // A street 100 m away, crossed by a track, and one with a
                // track beside a fifth of it.
                way(&[[0.0, 100.0], [200.0, 100.0]], true, true),
                way(&[[100.0, 50.0], [100.0, 150.0]], false, true),
                way(&[[0.0, 300.0], [200.0, 300.0]], true, true),
                way(&[[0.0, 307.0], [40.0, 307.0]], false, true),
            ],
        );
        ride_the_tracks(&mut net);
        fn cycles(net: &DriveNetwork, index: usize) -> bool {
            net.get(LineKey {
                cell: (0, 0),
                index,
            })
            .unwrap()
            .uses
            .cycle
        }
        assert!(!cycles(&net, 0), "the main road's bikes are on its track");
        assert!(cycles(&net, 1), "the track itself");
        assert!(cycles(&net, 2), "a crossing track is not beside it");
        assert!(
            cycles(&net, 4),
            "a short stretch of track is not the street's"
        );
        // The track streams out: the street carries bikes again.
        let mut lines = vec![way(&[[0.0, 0.0], [200.0, 0.0]], true, true)];
        lines.push(way(&[[0.0, 100.0], [200.0, 100.0]], true, true));
        net.insert_cell((0, 0), 0.0, 0.0, lines);
        ride_the_tracks(&mut net);
        assert!(cycles(&net, 0));
    }
}

#[cfg(test)]
mod way_tests {
    use super::*;

    const NORTH: Vec2 = Vec2::new(0.0, -1.0);

    /// A vehicle across the way is waited for, the one from the left (right
    /// before left) with less patience; people are waited for, except by a
    /// fatbike; the one ahead is followed; an oncoming one in the next lane
    /// is passed.
    #[test]
    fn who_gives_way() {
        let at = Vec2::ZERO;
        let car = AgentKind::Car;
        // Going north, `right_of` is east (+x).
        let from_right = Some((car, Vec2::new(8.0, -8.0), Vec2::new(-1.0, 0.0)));
        let from_left = Some((car, Vec2::new(-8.0, -8.0), Vec2::new(1.0, 0.0)));
        assert_eq!(
            gives_way(car, false, at, NORTH, from_right),
            Way::Cross(false)
        );
        assert_eq!(
            gives_way(car, false, at, NORTH, from_left),
            Way::Cross(true)
        );
        assert_eq!(
            gives_way(AgentKind::Fatbike, true, at, NORTH, from_right),
            Way::Go
        );
        let walker = Some((AgentKind::Walker, Vec2::new(0.0, -5.0), Vec2::X));
        assert_eq!(gives_way(car, false, at, NORTH, walker), Way::Wait);
        assert_eq!(
            gives_way(car, false, at, NORTH, None),
            Way::Wait,
            "an avatar"
        );
        assert_eq!(
            gives_way(AgentKind::Fatbike, false, at, NORTH, walker),
            Way::Go
        );
        let ahead = Some((AgentKind::Cyclist, Vec2::new(0.0, -6.0), NORTH));
        assert_eq!(gives_way(car, false, at, NORTH, ahead), Way::Wait);
        let oncoming = Some((car, Vec2::new(-1.6, -10.0), -NORTH));
        assert_eq!(gives_way(car, false, at, NORTH, oncoming), Way::Go);
        let head_on = Some((car, Vec2::new(0.2, -10.0), -NORTH));
        assert_eq!(gives_way(car, false, at, NORTH, head_on), Way::Wait);
    }

    #[test]
    fn a_stop_ends_short_of_the_other() {
        assert_eq!(stop_within(LOOK_AHEAD_GAP_M), 0.0);
        assert_eq!(stop_within(0.0), 0.0);
        let v = stop_within(LOOK_AHEAD_GAP_M + 10.0);
        assert!((v * v / (2.0 * give_way::YIELD_BRAKE) - 10.0).abs() < 1e-3);
    }
}

#[cfg(test)]
mod fall_tests {
    use super::*;

    /// A rider knocked off goes over to its side, lies there, and comes
    /// back up as it gets going; one that stays up never tips.
    #[test]
    fn a_knocked_rider_goes_down_and_gets_up() {
        let crash = |t, recovering, fall| {
            Some(Crash {
                t,
                recovering,
                at: Vec2::ZERO,
                facing: Vec2::Y,
                fall,
            })
        };
        assert_eq!(fallen(None), 0.0);
        assert_eq!(fallen(crash(1.0, false, 0.0)), 0.0);
        assert_eq!(fallen(crash(0.0, false, 1.0)), 0.0);
        assert!(
            (fallen(crash(FALL_S, false, -1.0)) + 1.0).abs() < 1e-6,
            "down on its left"
        );
        assert_eq!(fallen(crash(FALLEN_HOLD_S, false, 1.0)), 1.0);
        let rising = fallen(crash(RISE_S * 0.5, true, 1.0));
        assert!(rising > 0.0 && rising < 1.0, "{rising}");
        assert_eq!(fallen(crash(RISE_S, true, 1.0)), 0.0);
    }
}

/// `CARTO_TRAFFIC_OVERLAPS=1`'s smoothness count: per kind, the sum, the most
/// and the number of frame-to-frame changes in the drawn step of agents near
/// the camera, m/s² — a smooth mover's is its acceleration, a stutter's is
/// far more. Logged and cleared once a second.
#[derive(Default)]
pub struct Jitter {
    on: bool,
    checked: bool,
    sum: [f32; 4],
    max: [f32; 4],
    n: [u32; 4],
    reported: f32,
}

/// The Rapier box for an agent drawn as `mesh`: its bounds across, up and
/// along, and how far their middle is ahead of the model's origin.
fn model_size(mesh: &cartopolis_geo::agent_models::ModelMesh) -> Size {
    let (lo, hi) = mesh.positions.iter().fold(
        (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
        |(lo, hi), p| {
            let p = Vec3::from_array(*p);
            (lo.min(p), hi.max(p))
        },
    );
    Size {
        half: Vec3::new(
            lo.x.abs().max(hi.x.abs()),
            (hi.y * 0.5).max(0.3),
            (hi.z - lo.z) * 0.5,
        ),
        centre_z: (hi.z + lo.z) * 0.5,
    }
}

/// A name for what a vehicle's sweep ahead found, for the stuck report.
fn other_name(other: Option<(AgentKind, Vec2, Vec2)>) -> &'static str {
    match other.map(|o| o.0) {
        None => "avatar ahead",
        Some(AgentKind::Car) => "car ahead",
        Some(AgentKind::Walker) => "walker ahead",
        Some(AgentKind::Cyclist) => "cyclist ahead",
        Some(AgentKind::Fatbike) => "fatbike ahead",
    }
}

/// The side of the squares the vehicles are binned in to find who might
/// cross their path, metres: a car at town speed covers it in two seconds.
const MOVER_CELL_M: f32 = 20.0;

fn mover_cell(p: Vec2) -> (i32, i32) {
    (
        (p.x / MOVER_CELL_M).floor() as i32,
        (p.y / MOVER_CELL_M).floor() as i32,
    )
}

/// How far from its centre a kind takes up room on its path, metres: half a
/// car's length, a bike's, a person's shoulders.
fn reach_of(kind: AgentKind) -> f32 {
    match kind {
        AgentKind::Car => 2.4,
        AgentKind::Cyclist | AgentKind::Fatbike => 1.0,
        AgentKind::Walker => 0.4,
    }
}

/// How long ahead two paths are looked at for meeting, seconds.
const CONFLICT_HORIZON_S: f32 = 4.0;
/// Within this of each other, two arriving at a crossing point arrive
/// together, seconds: the right-before-left rule decides.
const CONFLICT_TIE_S: f32 = 0.6;

/// Whether `me` (where, velocity, reach) must wait for `them` where their
/// straight paths cross, and if so how far it may still go: the distance to
/// the crossing point less both reaches and a metre.
///
/// They conflict when both would be at the crossing point within
/// [`CONFLICT_HORIZON_S`], at times closer than it takes the two to clear
/// it. The later one waits; arriving together, the one with the other on
/// its right; and a vehicle always waits for somebody on foot (`on_foot`).
/// Paths that run alongside (within ~25°) never conflict here — the sweep
/// ahead and the headway see to those.
fn path_conflict(me: (Vec2, Vec2, f32), them: (Vec2, Vec2, f32), on_foot: bool) -> Option<f32> {
    let (p, v, r) = me;
    let (q, w, s) = them;
    let (speed, their_speed) = (v.length(), w.length());
    if speed < 0.3 || their_speed < 0.3 {
        return None;
    }
    let (d, e) = (v / speed, w / their_speed);
    let cross = d.x * e.y - d.y * e.x;
    if cross.abs() < 0.42 {
        return None;
    }
    // Where the two straight paths meet: p + d·a = q + e·b.
    let rel = q - p;
    let a = (rel.x * e.y - rel.y * e.x) / cross;
    let b = (rel.x * d.y - rel.y * d.x) / cross;
    // Past it, or already on it: it clears the point rather than stopping
    // in the middle.
    if a < r + s || b < -s {
        return None;
    }
    let (t_me, t_them) = (a / speed, b / their_speed);
    if t_me > CONFLICT_HORIZON_S || t_them > CONFLICT_HORIZON_S {
        return None;
    }
    // Each takes this long to clear the point once it gets there.
    let clear = (r + s + 1.0) / speed.min(their_speed);
    if (t_me - t_them).abs() > clear {
        return None;
    }
    let second = if on_foot {
        true
    } else if (t_me - t_them).abs() < CONFLICT_TIE_S {
        rel.dot(Vec2::new(-d.y, d.x)) > 0.0
    } else {
        t_me > t_them
    };
    second.then(|| (a - r - s - 1.0).max(0.0))
}

#[cfg(test)]
mod conflict_tests {
    use super::*;

    const NORTH: Vec2 = Vec2::new(0.0, -1.0);
    const WEST: Vec2 = Vec2::new(-1.0, 0.0);

    /// Two coming to one crossing point: the later waits; together, the one
    /// with the other on its right; a vehicle always waits for somebody on
    /// foot; paths alongside never conflict here.
    #[test]
    fn the_second_to_a_crossing_waits() {
        // A car 30 m south of the point going north at 10 m/s, a cyclist 12 m
        // east of it going west at 4 m/s: both there in 3 s.
        let car = (Vec2::new(0.0, 30.0), NORTH * 10.0, reach_of(AgentKind::Car));
        let bike = (
            Vec2::new(12.0, 0.0),
            WEST * 4.0,
            reach_of(AgentKind::Cyclist),
        );
        let short = path_conflict(car, bike, false).expect("the bike is on its right");
        assert!((short - (30.0 - 2.4 - 1.0 - 1.0)).abs() < 1e-3, "{short}");
        assert_eq!(
            path_conflict(bike, car, false),
            None,
            "the car is on its left"
        );
        // The bike a second ahead of the car, not long enough to be clear of
        // the point: the car waits, the bike goes.
        let early = (
            Vec2::new(4.0, 0.0),
            WEST * 4.0,
            reach_of(AgentKind::Cyclist),
        );
        let late = (Vec2::new(0.0, 20.0), NORTH * 10.0, reach_of(AgentKind::Car));
        assert!(path_conflict(late, early, false).is_some());
        assert_eq!(path_conflict(early, late, false), None);
        // Far apart in time: no conflict.
        let far = (Vec2::new(0.0, 80.0), NORTH * 10.0, reach_of(AgentKind::Car));
        assert_eq!(path_conflict(far, bike, false), None);
        // Somebody on foot, even from the left.
        let walker = (
            Vec2::new(-4.0, 0.0),
            -WEST * 1.3,
            reach_of(AgentKind::Walker),
        );
        let slow = (Vec2::new(0.0, 9.0), NORTH * 3.0, reach_of(AgentKind::Car));
        assert!(path_conflict(slow, walker, true).is_some());
        // Alongside.
        let beside = (
            Vec2::new(2.0, 20.0),
            NORTH * 4.0,
            reach_of(AgentKind::Cyclist),
        );
        assert_eq!(path_conflict(car, beside, false), None);
    }
}

/// Within this of the camera, an agent of `kind` is put on the road only
/// behind it (once the pool has filled), metres: the ring's own edge is
/// further out, and appearing there, small and far, is not seen as appearing.
fn unseen_m(kind: AgentKind) -> f32 {
    kind.place_radius() * 0.8
}
