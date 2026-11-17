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

use bevy::prelude::*;
use bevy::render::mesh::MeshTag;
use big_space::prelude::{BigSpace, Grid, GridCell};
use std::collections::HashMap;

use cartopolis_geo::traffic::{DriveLine, Flow, Pavements, Uses, PAVEMENT_CLEARANCE_M};

use crate::components::{FreeCamera, HideInSpace};
use crate::grid::{anchored_xz, AnchoredCamera};
use crate::systems::give_way;

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
    /// Who may use it — `drive` is the tile's answer ([`Self::kind_drives`]) less
    /// the streets closed to cars ([`close_car_free_lines`]).
    pub uses: Uses,
    /// Whether the tile's road kind takes cars, before any closure. Kept so a
    /// closure that streams out again reopens the line.
    pub kind_drives: bool,
    /// Whether the tile's road kind takes bikes, before a cycle track beside
    /// it takes them off it ([`ride_the_tracks`]). `uses.cycle` is the answer.
    pub kind_cycles: bool,
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
    /// Which sides have a pavement (`cartopolis_geo::traffic::keep_walkers_off`).
    pub pavements: Pavements,
    /// The corners of the line's bounding box, so a proximity query can skip
    /// a line without reading its points.
    pub bounds: (Vec2, Vec2),
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

/// What an agent is. The steering is the same for all three — what differs is
/// which lines they may use, how fast they go, and where across the road they
/// sit.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Car,
    /// A person on foot.
    Walker,
    /// A person on a bike: the crowd's people again, posed on a city bike
    /// (`cartopolis_human::crowd::Ride`), on the cycle tracks and the streets
    /// bikes share with cars (`cartopolis_geo::traffic::cycle_ok`).
    Cyclist,
    /// An electric fatbike, ridden fast and not by the rules: on the cycle
    /// tracks and the streets, and on the pavements and footpaths too, either
    /// way up a one-way street, weaving through whoever is in the way, and
    /// most of them through a red light. Shown with the cyclists.
    Fatbike,
}

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
}

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

    let bodies: Vec<(CarBody, [Handle<Mesh>; 3])> = CarBody::ALL
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
            )
        })
        .collect();

    let agent = |kind: AgentKind, i: usize, seed: u64, parts: AgentParts| {
        let walker = kind == AgentKind::Walker;
        let r = |stream: u64| rand01(seed, i as u64, stream);
        Car {
            line: None,
            s: 0.0,
            dir: 1.0,
            speed_scale: 0.85 + r(7) * 0.3,
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
            defiant: kind == AgentKind::Fatbike && r(15) < FATBIKE_DEFIANT_SHARE,
            crossing: None,
            last_cross: None,
            foot_down: 0.0,
            ordinal: i as u16,
        }
    };

    for i in 0..MAX_CARS {
        let (body, [body_mesh, trim_mesh, tail_mesh]) = {
            let b = CarBody::for_index(i);
            let (_, m) = bodies
                .iter()
                .find(|(k, _)| *k == b)
                .expect("every body is built");
            (b, m.clone())
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
                agent(AgentKind::Car, i, 0x0CA2, AgentParts::Car { tail }),
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
    layers: Res<crate::systems::data_layers::DataLayers>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    decks: Res<crate::systems::map_geometry::DeckPlates>,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: AnchoredCamera,
    grids: Query<&Grid, With<BigSpace>>,
    mut cars: Query<
        (
            &mut Car,
            &AgentKind,
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
    for (i, (car, &kind, ..)) in cars.iter().enumerate() {
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
        let wanted = match kind {
            AgentKind::Walker => show_people,
            AgentKind::Car => show_people,
            AgentKind::Cyclist | AgentKind::Fatbike => true,
        };
        if !wanted {
            continue;
        }
        let Some((p, travel, _)) = pose_of(car, net.get(key), kind) else {
            continue;
        };
        let p = p + right_of(travel) * car.react.shift;
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
    } = react;
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

    for (i, (mut car, &kind, mut tf, mut cell, dressed, mut vis)) in cars.iter_mut().enumerate() {
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
                want = want.min(headway_speed(occupied, k, car.dir, car.s, want));
                if let Some(gap) = bike_ahead {
                    let (shift, keep_to) = pass_cyclist(l, gap, want);
                    shift_to = shift;
                    want = keep_to;
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
                        let y = give_way::yield_to(
                            avatars
                                .iter()
                                .filter(|a| a.distance_squared(p) < near * near)
                                .map(|&a| on_path(&car, kind, l, p, travel, a)),
                            car.speed,
                            body,
                        );
                        want = want.min(y.limit);
                        hard_brake = y.hard;
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
                    }
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
        if line.is_none() && !on_duty {
            // Not out at this hour: it waits, unseen, until the hour fills.
            car.line = None;
            *vis = Visibility::Hidden;
            continue;
        }
        if line.is_none() {
            match place_agent(
                &net, candidates, weights, cam, kind, occupied, i as u64, *frame,
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
                tf.rotation *= Quat::from_rotation_z(car.tilt.y - leaning);
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
                cycle: true,
            },
            deck: vec![f32::NAN; points.len()],
            on_deck: false,
            pavements: Pavements::BOTH,
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
            cycle: false,
        };
        let mut path = line(&[[0.0, 50.0], [100.0, 50.0]], Flow::Both);
        path.uses = Uses {
            drive: false,
            walk: true,
            cycle: true,
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
        let cyclist = AgentKind::Cyclist.offset(l);
        assert!(
            cyclist > car && cyclist < l.half_width,
            "a cyclist rides between the car and the kerb: {cyclist} (car {car}, kerb {})",
            l.half_width
        );
    }

    /// A cyclist rides the cycle track and the street, and nobody else's
    /// network: not the footpath, not the motorway. A cycle track draws more
    /// riders than a street of its length.
    #[test]
    fn cyclists_ride_their_own_network() {
        let mut net = DriveNetwork::default();
        let mut motorway = line(&[[0.0, 0.0], [100.0, 0.0]], Flow::Both);
        motorway.uses = Uses {
            drive: true,
            walk: false,
            cycle: false,
        };
        let mut footway = line(&[[0.0, 20.0], [100.0, 20.0]], Flow::Both);
        footway.uses = Uses {
            drive: false,
            walk: true,
            cycle: false,
        };
        let mut track = line(&[[0.0, 40.0], [100.0, 40.0]], Flow::Both);
        track.uses = Uses {
            drive: false,
            walk: true,
            cycle: true,
        };
        let street = line(&[[0.0, 60.0], [100.0, 60.0]], Flow::Both);
        net.insert_cell((0, 0), 0.0, 0.0, vec![motorway, footway, track, street]);
        let mut rides: Vec<usize> = net
            .near(Vec2::new(50.0, 30.0), 80.0, AgentKind::Cyclist)
            .iter()
            .map(|k| k.index)
            .collect();
        rides.sort_unstable();
        assert_eq!(rides, vec![2, 3], "the track and the street");
        let key = |index| LineKey {
            cell: (0, 0),
            index,
        };
        let (track, street) = (net.get(key(2)).unwrap(), net.get(key(3)).unwrap());
        assert!(AgentKind::Cyclist.importance(track) > AgentKind::Cyclist.importance(street));
        assert_eq!(AgentKind::Cyclist.speed(street), CYCLE_SPEED);
        assert!(
            AgentKind::Cyclist.offset(track) < track.half_width,
            "on a track, on it"
        );
        // A fatbike rides all of that and the footpath too — never the
        // motorway — and is quicker than a cyclist everywhere.
        let mut fat: Vec<usize> = net
            .near(Vec2::new(50.0, 30.0), 80.0, AgentKind::Fatbike)
            .iter()
            .map(|k| k.index)
            .collect();
        fat.sort_unstable();
        assert_eq!(fat, vec![1, 2, 3], "the footway, the track and the street");
        let footway = net.get(key(1)).unwrap();
        assert!(AgentKind::Fatbike.speed(street) > CYCLE_SPEED);
        assert!(AgentKind::Fatbike.speed(footway) > CYCLE_SPEED);
        assert!(AgentKind::Fatbike.speed(footway) < AgentKind::Fatbike.speed(street));
    }

    /// A fatbike rides a street's pavement or its cycle lane; on the pavement
    /// it is out among the walkers, beyond the kerb, on whichever side has
    /// one. It may ride a one-way street either way.
    #[test]
    fn a_fatbike_takes_the_pavement_or_the_lane() {
        let mut net = DriveNetwork::default();
        let mut street = line(&[[0.0, 0.0], [100.0, 0.0]], Flow::Forward);
        street.pavements = Pavements {
            right: false,
            left: true,
        };
        net.insert_cell((0, 0), 0.0, 0.0, vec![street]);
        let key = LineKey {
            cell: (0, 0),
            index: 0,
        };
        let l = net.get(key).unwrap();
        let mut bike = walker_on(key, 1.0, 0.0);
        // Riding the line's own way, its right is the line's right: no
        // pavement there, so the pavement it takes is on its left.
        bike.choose_way(AgentKind::Fatbike, l, 1.0, 0.0, 0.9);
        assert!(bike.pavement);
        assert_eq!(bike.side, -1.0);
        let out = bike.offset(AgentKind::Fatbike, l);
        assert!(out < -l.half_width, "beyond the left kerb: {out}");
        // Against the one-way flow, the line's left is its right.
        bike.choose_way(AgentKind::Fatbike, l, -1.0, 0.0, 0.9);
        assert!(bike.pavement);
        assert_eq!(bike.side, 1.0);
        // Most of the rest of the time, the lane.
        bike.choose_way(AgentKind::Fatbike, l, 1.0, 0.9, 0.0);
        assert!(!bike.pavement);
        let lane = bike.offset(AgentKind::Fatbike, l);
        assert!(lane > 0.0 && lane < l.half_width, "in the lane: {lane}");
        // A cyclist is never moved.
        let mut cyclist = walker_on(key, 1.0, 0.0);
        cyclist.choose_way(AgentKind::Cyclist, l, 1.0, 0.0, 0.0);
        assert!(!cyclist.pavement);
        // Off the end of a one-way street's exit, the fatbike may ride it
        // back; a cyclist may not.
        let mut other = DriveNetwork::default();
        other.insert_cell(
            (0, 0),
            0.0,
            0.0,
            vec![
                line(&[[0.0, 0.0], [50.0, 0.0]], Flow::Both),
                line(&[[100.0, 0.0], [50.0, 0.0]], Flow::Forward),
            ],
        );
        let keys = other.near(Vec2::new(50.0, 0.0), 80.0, AgentKind::Fatbike);
        let first = LineKey {
            cell: (0, 0),
            index: 0,
        };
        let at = Vec2::new(50.0, 0.0);
        let east = Vec2::X;
        assert!(hand_off(&other, &keys, first, at, east, AgentKind::Fatbike, 0.5).is_some());
        assert!(hand_off(&other, &keys, first, at, east, AgentKind::Cyclist, 0.5).is_none());
    }

    /// A rider leans into a turn, the other way to a car body, by the angle the
    /// turn's lateral acceleration makes with gravity, and no further than a
    /// pedal clears the road.
    #[test]
    fn a_rider_leans_into_the_turn() {
        let mut lean = 0.0;
        for _ in 0..200 {
            lean = bike_lean(lean, 2.0, 0.02);
        }
        assert!((lean - (2.0f32 / 9.81).atan()).abs() < 1e-3, "{lean}");
        let car = body_tilt(Vec2::ZERO, 0.0, 2.0, 1.0).y;
        assert!(
            lean > 0.0 && car < 0.0,
            "the bike leans in, the car body out"
        );
        let mut hard = 0.0;
        for _ in 0..200 {
            hard = bike_lean(hard, -40.0, 0.02);
        }
        assert!((hard + MAX_BIKE_LEAN).abs() < 1e-3);
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
        // Over the whole range of the draw: mostly straight on, whichever end
        // is nearer — and sometimes the turn.
        let mut straight = 0;
        for k in 0..100 {
            let (key, dir) = hand_off(
                &net,
                &keys,
                from,
                Vec2::new(100.0, 0.0),
                Vec2::X,
                AgentKind::Car,
                k as f32 / 100.0,
            )
            .expect("a road to go on to");
            if key.index == 0 {
                straight += 1;
                assert_eq!(dir, 1.0);
            }
        }
        assert!(
            (65..=80).contains(&straight),
            "{straight} in 100 straight on"
        );
    }

    /// A main road keeps most of its traffic past a residential side street.
    #[test]
    fn a_main_road_keeps_its_traffic() {
        let mut net = DriveNetwork::default();
        let mut main = line(&[[100.0, 0.0], [300.0, 0.0]], Flow::Both);
        main.speed = 13.9;
        let mut side = line(&[[100.0, 0.0], [100.0, 200.0]], Flow::Both);
        side.speed = RESIDENTIAL_SPEED;
        net.insert_cell((0, 0), 0.0, 0.0, vec![main, side]);
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
        let on = (0..100)
            .filter(|k| {
                let r = *k as f32 / 100.0;
                hand_off(
                    &net,
                    &keys,
                    from,
                    Vec2::new(100.0, 0.0),
                    Vec2::X,
                    AgentKind::Car,
                    r,
                )
                .is_some_and(|(key, _)| key.index == 0)
            })
            .count();
        assert!((75..=85).contains(&on), "{on} in 100 stay on the main road");
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
            0.5,
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

    /// A time when group 0 shows stop and one when it shows go.
    fn red_and_green() -> (f32, f32) {
        use crate::systems::street_detail::signal_says_stop;
        let ts = || (0..480).map(|t| t as f32 * 0.1);
        (
            ts().find(|&t| signal_says_stop(t, 0)).unwrap(),
            ts().find(|&t| !signal_says_stop(t, 0)).unwrap(),
        )
    }

    /// Heading north (−z), which is phase group 0, with a light 30 m ahead.
    const NORTH: Vec2 = Vec2::new(0.0, -1.0);

    #[test]
    fn a_car_stops_at_red_and_drives_on_green() {
        let (red, green) = red_and_green();
        let light = [(Vec2::new(0.0, -30.0), 0u8, false)];
        let mut grace = 0.0;
        let at_red = signal_speed(&light, red, Vec2::ZERO, NORTH, 8.0, 4.0, &mut grace);
        // A braking curve ending at the stop line, not an instant stop.
        let curve = (2.0 * SIGNAL_BRAKE * (30.0 - STOP_SHORT_M)).sqrt();
        assert!(
            (at_red - curve).abs() < 1e-3,
            "a red light 30 m ahead slows the car: {at_red}"
        );
        let at_green = signal_speed(&light, green, Vec2::ZERO, NORTH, 8.0, 4.0, &mut grace);
        assert_eq!(at_green, f32::MAX);
        // Waiting at the line it stays put until the light changes.
        let line = Vec2::new(0.0, -30.0 + STOP_SHORT_M);
        assert_eq!(
            signal_speed(&light, red, line, NORTH, 0.0, 4.0, &mut grace),
            0.0
        );
        assert_eq!(
            signal_speed(&light, green, line, NORTH, 0.0, 4.0, &mut grace),
            f32::MAX
        );
    }

    /// A light a pressed button holds stops the car on its green too, and
    /// lets it go once the press is over.
    #[test]
    fn a_pressed_button_holds_a_car_at_its_green() {
        let (_, green) = red_and_green();
        let held = [(Vec2::new(0.0, -30.0), 0u8, true)];
        let mut grace = 0.0;
        let line = Vec2::new(0.0, -30.0 + STOP_SHORT_M);
        assert_eq!(
            signal_speed(&held, green, line, NORTH, 0.0, 4.0, &mut grace),
            0.0
        );
        let released = [(Vec2::new(0.0, -30.0), 0u8, false)];
        assert_eq!(
            signal_speed(&released, green, line, NORTH, 0.0, 4.0, &mut grace),
            f32::MAX
        );
    }

    /// The cross street's lights stand at the same junction, beside this road
    /// and in the other phase group; neither may stop a car.
    #[test]
    fn a_cross_streets_light_does_not_stop_a_car() {
        let (red, _) = red_and_green();
        let mut grace = 0.0;
        let beside = [(Vec2::new(10.0, -30.0), 0u8, false)];
        assert_eq!(
            signal_speed(&beside, red, Vec2::ZERO, NORTH, 8.0, 4.0, &mut grace),
            f32::MAX
        );
        let other_group = [(Vec2::new(0.0, -30.0), 1u8, false)];
        assert_eq!(
            signal_speed(&other_group, red, Vec2::ZERO, NORTH, 8.0, 4.0, &mut grace),
            f32::MAX
        );
    }

    /// Too close to stop and it goes, as a driver caught by the amber does —
    /// a stop forced anyway would leave it in the junction.
    #[test]
    fn a_car_too_close_to_stop_drives_through() {
        let (red, _) = red_and_green();
        let light = [(Vec2::new(0.0, -10.0), 0u8, false)];
        let mut grace = 0.0;
        assert_eq!(
            signal_speed(&light, red, Vec2::ZERO, NORTH, 14.0, 4.0, &mut grace),
            f32::MAX
        );
    }

    /// Having crossed one stop line, the opposite approach's line beyond the
    /// junction — same road, same phase group — is not this car's.
    #[test]
    fn a_car_through_a_junction_ignores_the_far_stop_line() {
        let (red, _) = red_and_green();
        let near_and_far = [
            (Vec2::new(0.0, 1.0), 0u8, false),
            (Vec2::new(0.0, -15.0), 0u8, false),
        ];
        let mut grace = 0.0;
        let v = signal_speed(&near_and_far, red, Vec2::ZERO, NORTH, 6.0, 4.0, &mut grace);
        assert_eq!(v, f32::MAX);
        assert_eq!(grace, SIGNAL_GRACE_M);
    }

    #[test]
    fn the_body_dips_braking_and_leans_out_of_a_turn() {
        let mut t = Vec2::ZERO;
        for _ in 0..60 {
            t = body_tilt(t, -5.0, 0.0, 1.0 / 60.0);
        }
        assert!(t.x > 0.02, "the nose dips under braking: {t:?}");
        let mut t = Vec2::ZERO;
        for _ in 0..60 {
            // Turning right at speed: the outside of the turn is the car's left,
            // which is the model's +x — a negative roll about z.
            t = body_tilt(t, 0.0, 2.5, 1.0 / 60.0);
        }
        assert!(t.y < -0.02, "the roof leans out of the turn: {t:?}");
        assert!(
            body_tilt(Vec2::ZERO, -100.0, 100.0, 10.0)
                .abs()
                .max_element()
                <= MAX_TILT + 1e-6
        );
    }

    #[test]
    fn brake_lamps_hold_at_a_stop_and_do_not_flicker() {
        assert!(brake_lamps(false, 0.0, true), "lit while waiting");
        assert!(brake_lamps(false, -2.0, false), "lit braking");
        assert!(!brake_lamps(false, -0.5, false), "not lit easing off");
        assert!(
            brake_lamps(true, -0.5, false),
            "stays lit easing off a stop"
        );
        assert!(!brake_lamps(true, 1.0, false), "off pulling away");
    }

    /// A pavement is two-way, and people going each way use both of them.
    #[test]
    fn walkers_keep_to_either_pavement() {
        let net = net_with(&[[0.0, 0.0], [100.0, 0.0]]);
        let key = LineKey {
            cell: (0, 0),
            index: 0,
        };
        let l = net.get(key);
        let walker = |side: f32| Car {
            line: Some(key),
            s: 50.0,
            dir: 1.0,
            speed_scale: 1.0,
            speed: 1.0,
            heading: Vec2::ZERO,
            turn: None,
            side,
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
        };
        let (right, ..) = pose_of(&walker(1.0), l, AgentKind::Walker).unwrap();
        let (left, ..) = pose_of(&walker(-1.0), l, AgentKind::Walker).unwrap();
        assert!(right.y > 3.0 && left.y < -3.0, "{right} {left}");
        assert!((right.y + left.y).abs() < 1e-3, "the two pavements mirror");
    }

    fn walker_on(key: LineKey, side: f32, lateral: f32) -> Car {
        Car {
            line: Some(key),
            s: 50.0,
            dir: 1.0,
            speed_scale: 1.0,
            speed: 1.0,
            heading: Vec2::ZERO,
            turn: None,
            side,
            lateral,
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
        }
    }

    /// A street with a pavement on its right only: a walker meant for the left
    /// crosses to the right, in either direction of travel.
    #[test]
    fn a_walker_crosses_to_the_side_with_a_pavement() {
        let mut one_sided = line(&[[0.0, 0.0], [100.0, 0.0]], Flow::Both);
        one_sided.pavements = Pavements {
            right: true,
            left: false,
        };
        let mut net = DriveNetwork::default();
        net.insert_cell((0, 0), 0.0, 0.0, vec![one_sided]);
        let key = LineKey {
            cell: (0, 0),
            index: 0,
        };
        let l = net.get(key).unwrap();
        for dir in [1.0f32, -1.0] {
            for side in [1.0f32, -1.0] {
                let mut w = walker_on(key, side, 0.0);
                w.keep_to_pavement(AgentKind::Walker, l, dir);
                assert_eq!(w.side * dir, 1.0, "dir {dir} side {side}");
            }
        }
    }

    /// On a path a walker is on the path, not on the grass beside it.
    #[test]
    fn a_walker_walks_on_a_path() {
        let mut path = line(&[[0.0, 0.0], [100.0, 0.0]], Flow::Both);
        path.uses.drive = false;
        path.half_width = 1.2;
        let mut net = DriveNetwork::default();
        net.insert_cell((0, 0), 0.0, 0.0, vec![path]);
        let key = LineKey {
            cell: (0, 0),
            index: 0,
        };
        let l = net.get(key);
        for side in [1.0, -1.0] {
            for lateral in [-WALK_SPREAD_M, 0.0, WALK_SPREAD_M] {
                let (p, ..) =
                    pose_of(&walker_on(key, side, lateral), l, AgentKind::Walker).unwrap();
                assert!(p.y.abs() <= 1.2, "off the path at {p}");
            }
        }
    }
}

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
