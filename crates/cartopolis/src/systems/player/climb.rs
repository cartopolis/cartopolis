//! Climbing onto low walls, and sitting on their edges and on quay edges.
//!
//! Asked for by the owner as "klimmen en zitten" (2026-11-15).
//! `docs/notes/climbing-and-edges.md` has what is climbable and why, and what
//! lost. In short:
//!
//! - **What.** A street-detail wall whose top can be stood on
//!   (`collision::Footprint::as_ledge`: a wall, a city wall, a retaining wall)
//!   and is [`CLIMB_MIN_M`]..=[`CLIMB_MAX_M`] over the soles. Walking up to one
//!   offers "Climb up" (E, or the prompt), and Space while walking into it
//!   climbs instead of jumping.
//! - **How.** The avatar plays the `climb` clip (a step up onto a stool, CMU
//!   15_02) while it is carried onto the top ([`Climb`]); the model is lifted
//!   the rest of the way the wall is higher than the clip's own step. When it
//!   ends the top is the avatar's floor (`interiors::Footing`, through
//!   `BuildingColliders::ledge_under`), so it walks along it and walking off
//!   the edge is a fall, as off a roof.
//! - **Sitting.** On a wall from the ground, on the edge of a wall top, or on a
//!   quay over the water ([`find_edge_seat`]): the bench's seat (an offset the
//!   wire carries, protocol 19) with a lift, and the `dangle` clip where the
//!   feet do not reach the ground. Every viewer measures the edge under the
//!   seat for itself ([`seat_on_edge`]), so nothing new goes on the wire.
//!
//! The sim is not told: like the water and the terrain, the walls are every
//! viewer's own map, and the sim holds none.

use bevy::prelude::*;

use crate::systems::collision::{BuildingColliders, Ledge, CLIMB_MAX_M, CLIMB_MIN_M};

/// The climb clip's length, seconds (`cartopolis_human`'s
/// `the_climb_steps_up_onto_the_top`; change them together).
pub const CLIMB_SECS: f32 = 1.49;
/// How far up the clip itself steps the default man, metres: the rest of a
/// wall's height is the model's lift ([`Climb::holder_lift`]).
pub const CLIMB_RISE_M: f32 = 0.365;
/// How high the `sit` clip's seat is over its ground, metres, and the
/// `dangle` clip's (`the_seats_are_where_an_edge_takes_them`).
pub const SIT_SEAT_M: f32 = 0.30;
pub const DANGLE_SEAT_M: f32 = 0.45;
/// How far ahead a wall is offered, metres: where walking into one stops
/// (`collision::COLLIDE_PROBE_M`, 0.6 m) and a little more.
pub const CLIMB_REACH_M: f32 = 1.0;
/// The walls sat on from the ground, by height: below, a step; above, a
/// climb first.
pub const SIT_ON_MIN_M: f32 = 0.35;
pub const SIT_ON_MAX_M: f32 = 0.9;
/// How far below a seat the feet must hang for it to be an edge rather than
/// ground, metres.
pub const EDGE_DROP_M: f32 = 0.35;
/// How far behind an edge the hips sit, metres.
const SEAT_BACK_M: f32 = 0.1;
/// Where the feet hang, out from the seat, metres.
const FEET_OUT_M: f32 = 0.4;
/// How far ahead the water below a quay is looked for, metres.
const WATER_REACH_M: f32 = 1.2;
/// How finely a line is walked for the water's edge, metres.
const PROBE_M: f32 = 0.05;
/// A seat hangs the legs (`dangle`) once the drop below it is within this of
/// the clip's own seat: from there down the feet are off the ground.
const DANGLE_SLACK_M: f32 = 0.03;

/// A climb under way: the avatar carried from where it stood to its landing
/// on the top, over [`CLIMB_SECS`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Climb {
    /// The way it goes, unit world XZ.
    pub dir: Vec2,
    /// How fast it is carried, m/s.
    pub speed: f32,
    /// The top, in the colliders' frame.
    pub top: f32,
    /// Where the soles were when it started, same frame.
    pub ground: f32,
    pub elapsed: f32,
}

impl Climb {
    /// A climb from `pos` (world XZ), soles at `soles`, onto `ledge`.
    #[must_use]
    pub fn onto(pos: Vec2, soles: f32, ledge: &Ledge) -> Self {
        let to = ledge.landing - pos;
        let dist = to.length();
        Self {
            dir: to.normalize_or_zero(),
            speed: dist / CLIMB_SECS,
            top: ledge.top,
            ground: soles,
            elapsed: 0.0,
        }
    }

    /// How high the top is over where it started.
    #[must_use]
    pub fn height(&self) -> f32 {
        self.top - self.ground
    }

    /// How far through, 0..1.
    #[must_use]
    pub fn progress(&self) -> f32 {
        (self.elapsed / CLIMB_SECS).clamp(0.0, 1.0)
    }

    /// Advance by `dt`; whether it has finished.
    pub fn step(&mut self, dt: f32) -> bool {
        self.elapsed += dt;
        self.elapsed >= CLIMB_SECS
    }

    /// The model's lift on top of the clip's own step at this point: the rest
    /// of the wall's height (or less, for a wall lower than the step), eased
    /// in over the climb so the body rises with the legs.
    #[must_use]
    pub fn holder_lift(&self) -> f32 {
        climb_lift(self.height(), self.progress())
    }

    /// The facing the climb holds.
    #[must_use]
    pub fn yaw(&self) -> f32 {
        self.dir.x.atan2(self.dir.y)
    }
}

/// The model's lift `progress` (0..1) of the way up a wall `height` high:
/// what the clip's own [`CLIMB_RISE_M`] does not cover, smoothstepped.
#[must_use]
pub fn climb_lift(height: f32, progress: f32) -> f32 {
    let t = progress.clamp(0.0, 1.0);
    (height - CLIMB_RISE_M) * t * t * (3.0 - 2.0 * t)
}

/// Whether a wall `height` over the soles is climbed rather than stepped over
/// or walked round.
#[must_use]
pub fn climbable(height: f32) -> bool {
    (CLIMB_MIN_M..=CLIMB_MAX_M).contains(&height)
}

/// A place to sit on an edge: a wall top, or the ground at a quay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeSeat {
    /// Where the hips go, world XZ (the colliders' frame).
    pub seat: Vec2,
    /// The way the sitter faces, out over the edge, unit.
    pub out: Vec2,
    /// The seat's height, the colliders' frame.
    pub surface: f32,
    /// How far below the seat the feet hang: the ground, the wall's foot or
    /// the water.
    pub drop: f32,
}

impl EdgeSeat {
    /// Whether the legs hang free (the `dangle` clip) rather than reach the
    /// ground (`sit`).
    #[must_use]
    pub fn dangle(&self) -> bool {
        self.drop >= DANGLE_SEAT_M - DANGLE_SLACK_M
    }

    /// The seat height of the clip it plays.
    #[must_use]
    pub fn clip_seat(&self) -> f32 {
        if self.dangle() {
            DANGLE_SEAT_M
        } else {
            SIT_SEAT_M
        }
    }

    /// How far the model is raised to put the clip's seat on this one, for
    /// an avatar drawn standing at height `root`.
    #[must_use]
    pub fn lift(&self, root: f32) -> f32 {
        self.surface - self.clip_seat() - root
    }

    /// The world yaw a sitter turns to.
    #[must_use]
    pub fn yaw(&self) -> f32 {
        self.out.x.atan2(self.out.y)
    }
}

/// The map under a point, as this module asks it: the colliders, the ground
/// and the water. Coordinates are the colliders' world XZ; heights their
/// frame.
pub struct Surroundings<'a> {
    pub colliders: &'a BuildingColliders,
    pub ground: &'a dyn Fn(Vec2) -> f32,
    pub water: &'a dyn Fn(Vec2) -> Option<f32>,
}

impl Surroundings<'_> {
    /// Where feet hanging at `p` would come down: the water, or the ground.
    fn below(&self, p: Vec2) -> f32 {
        (self.water)(p).unwrap_or_else(|| (self.ground)(p))
    }
}

/// The edge seat at `seat`, facing `out`, as any viewer measures it — the
/// local avatar when it sits, and every other viewer from the seat offset the
/// wire carries. `None` where it is no edge (a bench, whose clip already sits
/// at its slat): not on a wall top, and nothing [`EDGE_DROP_M`] below the
/// feet.
#[must_use]
pub fn seat_on_edge(map: &Surroundings, seat: Vec2, out: Vec2) -> Option<EdgeSeat> {
    let out = out.normalize_or_zero();
    if out == Vec2::ZERO || (map.water)(seat).is_some() {
        return None;
    }
    let ledge = map.colliders.ledge_under(seat, f32::INFINITY);
    let surface = ledge.unwrap_or_else(|| (map.ground)(seat));
    let drop = surface - map.below(seat + out * FEET_OUT_M);
    (ledge.is_some() || drop >= EDGE_DROP_M).then_some(EdgeSeat {
        seat,
        out,
        surface,
        drop,
    })
}

/// Somewhere to sit on an edge for an avatar at `pos`, soles at `soles`,
/// facing `facing` — what the prompt offers as "Sit":
///
/// 1. standing on a wall top, its nearest edge with a drop beyond it,
///    trying the way it faces, then either side, then behind;
/// 2. on the ground facing a wall [`SIT_ON_MIN_M`]..=[`SIT_ON_MAX_M`] high
///    within [`CLIMB_REACH_M`], its near edge, facing back out;
/// 3. on the ground with water [`EDGE_DROP_M`] or more below within
///    [`WATER_REACH_M`] ahead, the quay's edge, facing the water.
#[must_use]
pub fn find_edge_seat(map: &Surroundings, pos: Vec2, facing: Vec2, soles: f32) -> Option<EdgeSeat> {
    let f = facing.normalize_or_zero();
    if f == Vec2::ZERO {
        return None;
    }
    // On a wall top: the soles are at its height.
    let on_top = map
        .colliders
        .ledge_under(pos, soles + 0.1)
        .is_some_and(|top| top > soles - 0.1);
    if on_top {
        let side = Vec2::new(f.y, -f.x);
        for d in [f, side, -side, -f] {
            let Some(edge) = map.colliders.ledge_edge(pos, d, 0.8) else {
                continue;
            };
            let seat = edge - d * SEAT_BACK_M.min(edge.distance(pos));
            if let Some(s) = seat_on_edge(map, seat, d).filter(|s| s.drop >= EDGE_DROP_M) {
                return Some(s);
            }
        }
        return None;
    }
    if let Some(ledge) = map.colliders.ledge_ahead(pos, f, soles, CLIMB_REACH_M) {
        if (SIT_ON_MIN_M..=SIT_ON_MAX_M).contains(&ledge.height) {
            return seat_on_edge(map, ledge.entry + f * SEAT_BACK_M, -f);
        }
        return None;
    }
    let here = (map.ground)(pos);
    let mut last = pos;
    let mut along = PROBE_M;
    while along <= WATER_REACH_M {
        let q = pos + f * along;
        if let Some(surface) = (map.water)(q) {
            if here - surface < EDGE_DROP_M {
                return None;
            }
            return seat_on_edge(map, last - f * PROBE_M, f);
        }
        last = q;
        along += PROBE_M;
    }
    None
}

/// What the prompt can offer at the wall or edge in front of the avatar,
/// refreshed every frame in Follow by [`track_edges`].
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq)]
pub struct Edges {
    /// A wall to climb onto.
    pub climb: Option<Ledge>,
    /// An edge to sit on.
    pub sit: Option<EdgeSeat>,
    /// Where the avatar was when these were found, world XZ, and the height
    /// it was drawn standing at: what a seat's offset and lift are measured
    /// from.
    pub at: Vec2,
    pub soles: f32,
}

impl Edges {
    #[must_use]
    pub fn any(&self) -> bool {
        self.climb.is_some() || self.sit.is_some()
    }

    /// Take what is offered first — the climb where there is one, else the
    /// seat — as E and the prompt's first button do.
    pub fn act(&self, avatar: &mut crate::resources::AvatarState) {
        if self.climb.is_some() {
            avatar.wants_climb = true;
        } else {
            self.sit(avatar);
        }
    }

    /// Sit on the edge offered, if one is.
    pub fn sit(&self, avatar: &mut crate::resources::AvatarState) {
        if let Some(seat) = &self.sit {
            sit_on_edge(avatar, self.at, self.soles, seat);
        }
    }
}

/// Where the local avatar is in the colliders' frame and the terrain's:
/// `(world XZ, the offset from that to the terrain's coordinates)`. Online
/// the two are one frame (`avatar::smooth_online_avatar_display` asks both
/// with the same numbers); offline the terrain is asked in the avatar's
/// cell-local frame and the colliders in absolute metres
/// (`avatar::handle_avatar_movement`).
#[must_use]
pub fn avatar_frame(
    online: bool,
    position: Vec3,
    cell: Option<&big_space::prelude::GridCell>,
) -> (Vec2, Vec2) {
    use crate::grid::WORLD_CELL_EDGE;
    let base = match (online, cell) {
        (false, Some(c)) => Vec2::new(c.x as f32 * WORLD_CELL_EDGE, c.z as f32 * WORLD_CELL_EDGE),
        _ => Vec2::ZERO,
    };
    (base + Vec2::new(position.x, position.z), base)
}

/// Find the wall to climb and the edge to sit on in front of the local
/// avatar, for the prompt. Only standing in Follow on the ground or a wall
/// top — not sitting, climbing, swimming, carrying or in the air.
pub fn track_edges(
    camera_state: Res<crate::resources::CameraState>,
    mut avatar_state: ResMut<crate::resources::AvatarState>,
    online: Option<Res<crate::resources::OnlineSession>>,
    colliders: Res<BuildingColliders>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    avatars: Query<&big_space::prelude::GridCell, With<crate::components::Avatar>>,
    mut edges: ResMut<Edges>,
) {
    // An edge seat's lift, against where the avatar is drawn now.
    if let Some(seated) = avatar_state.seated.filter(|s| s.edge) {
        let (pos, base) = avatar_frame(
            online.is_some(),
            avatar_state.display_position,
            avatars.single().ok(),
        );
        let ground = |p: Vec2| terrain.ground_at(p.x - base.x, p.y - base.y);
        let water = |p: Vec2| terrain.swim_surface_at(p.x - base.x, p.y - base.y);
        let map = Surroundings {
            colliders: &colliders,
            ground: &ground,
            water: &water,
        };
        let root = avatar_state.display_position.y;
        if let Some(e) = remeasure(&map, pos, &seated) {
            let lift = e.lift(root);
            if (lift - seated.lift).abs() > 0.01 || e.dangle() != seated.dangle {
                let at = pos + seated.offset;
                tracing::info!(
                    target: "climb",
                    root,
                    surface = e.surface,
                    drop = e.drop,
                    ground_under_seat = ground(at),
                    ledge_at_seat = ?colliders.ledge_at(at),
                    was = drawn_seat(root, seated.lift, seated.dangle),
                    now = drawn_seat(root, lift, e.dangle()),
                    "edge seat re-measured: drawn seat height was / is"
                );
                if let Some(s) = &mut avatar_state.seated {
                    s.lift = lift;
                    s.dangle = e.dangle();
                }
            }
        }
    }
    let idle = camera_state.mode != crate::resources::CameraMode::Avatar
        || avatar_state.seated.is_some()
        || avatar_state.climb.is_some()
        || avatar_state.swimming
        || avatar_state.holding.is_some()
        || !avatar_state.grounded;
    let found = if idle {
        Edges::default()
    } else {
        let (pos, base) = avatar_frame(
            online.is_some(),
            avatar_state.display_position,
            avatars.single().ok(),
        );
        let ground = |p: Vec2| terrain.ground_at(p.x - base.x, p.y - base.y);
        let water = |p: Vec2| terrain.swim_surface_at(p.x - base.x, p.y - base.y);
        let map = Surroundings {
            colliders: &colliders,
            ground: &ground,
            water: &water,
        };
        let yaw = avatar_state.facing_yaw;
        let facing = Vec2::new(yaw.sin(), yaw.cos());
        let soles = avatar_state.display_position.y;
        let climb = colliders
            .ledge_ahead(pos, facing, soles, CLIMB_REACH_M)
            .filter(|l| climbable(l.height));
        let sit = find_edge_seat(&map, pos, facing, soles);
        if climb.is_none() && sit.is_none() {
            Edges::default()
        } else {
            Edges {
                climb,
                sit,
                at: pos,
                soles,
            }
        }
    };
    if *edges != found {
        *edges = found;
    }
}

/// Sit the local avatar on `seat`: the bench's seat, with the edge's lift and
/// clip. `root` is the height it is drawn standing at.
pub fn sit_on_edge(
    avatar: &mut crate::resources::AvatarState,
    pos: Vec2,
    root: f32,
    seat: &EdgeSeat,
) {
    avatar.seated = Some(crate::resources::Seated {
        offset: seat.seat - pos,
        yaw: seat.yaw(),
        blend: 0.0,
        rising: false,
        lift: seat.lift(root),
        dangle: seat.dangle(),
        edge: true,
    });
}

/// Re-measure an edge seat for a sitter at `pos` (world XZ): the edge under
/// the seat point, whose `lift` from the root the sitter is drawn at now puts
/// the clip's seat on it. `None` when there is no edge there
/// any more (the wall's cell went), and the seat keeps what it had.
///
/// The lift is a difference between two heights measured at different
/// times unless it is measured again: the root moves after the avatar sits
/// (a capture seats it the frame it lands, before the ground under it has
/// streamed in), and the edge moves when its cell is rebuilt on new terrain.
#[must_use]
pub fn remeasure(
    map: &Surroundings,
    pos: Vec2,
    seated: &crate::resources::Seated,
) -> Option<EdgeSeat> {
    let out = Vec2::new(seated.yaw.sin(), seated.yaw.cos());
    seat_on_edge(map, pos + seated.offset, out)
}

/// Where a seated clip's seat is drawn: the root, the lift and the clip's
/// own seat height.
#[must_use]
pub fn drawn_seat(root: f32, lift: f32, dangle: bool) -> f32 {
    root + lift + if dangle { DANGLE_SEAT_M } else { SIT_SEAT_M }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::collision::Footprint;

    /// A wall `height` m high on flat ground at 0, running east–west along
    /// z = 2, 0.3 m thick and 6 m long.
    fn wall(height: f32) -> BuildingColliders {
        let mut c = BuildingColliders::default();
        let f = Footprint::new(vec![
            Vec2::new(-3.0, 1.85),
            Vec2::new(3.0, 1.85),
            Vec2::new(3.0, 2.15),
            Vec2::new(-3.0, 2.15),
        ])
        .unwrap()
        .with_extent(-0.5, height)
        .as_ledge();
        c.set_feature_cell((0, 0), vec![f]);
        c
    }

    fn flat() -> impl Fn(Vec2) -> f32 {
        |_| 0.0
    }

    fn dry() -> impl Fn(Vec2) -> Option<f32> {
        |_| None
    }

    /// Facing +z (south) from the origin, toward the wall.
    const SOUTH: Vec2 = Vec2::new(0.0, 1.0);

    #[test]
    fn a_waist_high_wall_is_climbed_and_lands_on_top() {
        let c = wall(0.8);
        let l = c.ledge_ahead(Vec2::new(0.0, 1.2), SOUTH, 0.0, CLIMB_REACH_M);
        let l = l.expect("a 0.8 m wall 0.65 m ahead is offered");
        assert!(climbable(l.height));
        assert!((l.top - 0.8).abs() < 1e-5);
        assert!(
            (1.85..=2.15).contains(&l.landing.y),
            "lands on the top: {l:?}"
        );
        let mut climb = Climb::onto(Vec2::new(0.0, 1.2), 0.0, &l);
        assert!(climb.yaw().abs() < 1e-5, "faces the wall");
        assert!(climb.holder_lift().abs() < 1e-6, "starts on the ground");
        while !climb.step(0.1) {}
        assert!(
            (climb.holder_lift() + CLIMB_RISE_M - 0.8).abs() < 1e-5,
            "the clip's step and the lift reach the top"
        );
        assert!(
            (climb.speed * CLIMB_SECS - l.landing.distance(Vec2::new(0.0, 1.2))).abs() < 1e-4,
            "carried exactly onto the landing"
        );
    }

    #[test]
    fn only_walls_of_a_climbable_height_are_offered() {
        for (h, offered) in [
            (0.3, false),
            (0.5, true),
            (1.2, true),
            (1.5, false),
            (2.0, false),
        ] {
            let c = wall(h);
            let got = c
                .ledge_ahead(Vec2::new(0.0, 1.2), SOUTH, 0.0, CLIMB_REACH_M)
                .is_some_and(|l| climbable(l.height));
            assert_eq!(got, offered, "a {h} m wall");
        }
        // Out of reach, or facing away.
        let c = wall(0.8);
        assert!(c
            .ledge_ahead(Vec2::new(0.0, 0.0), SOUTH, 0.0, CLIMB_REACH_M)
            .is_none());
        assert!(c
            .ledge_ahead(Vec2::new(0.0, 1.2), -SOUTH, 0.0, CLIMB_REACH_M)
            .is_none());
    }

    /// A wall that is not a ledge — a hedge, a fence, a building — is not
    /// climbed, and neither is a ledge behind one.
    #[test]
    fn a_hedge_is_not_climbed() {
        let mut c = BuildingColliders::default();
        let hedge = Footprint::new(vec![
            Vec2::new(-3.0, 1.6),
            Vec2::new(3.0, 1.6),
            Vec2::new(3.0, 1.8),
            Vec2::new(-3.0, 1.8),
        ])
        .unwrap()
        .with_extent(-0.5, 0.8);
        let wall = Footprint::new(vec![
            Vec2::new(-3.0, 1.85),
            Vec2::new(3.0, 1.85),
            Vec2::new(3.0, 2.15),
            Vec2::new(-3.0, 2.15),
        ])
        .unwrap()
        .with_extent(-0.5, 0.8)
        .as_ledge();
        c.set_feature_cell((0, 0), vec![hedge, wall]);
        assert!(c
            .ledge_ahead(Vec2::new(0.0, 1.2), SOUTH, 0.0, CLIMB_REACH_M)
            .is_none());
    }

    /// On top the wall lets the body through and holds it up; off the edge it
    /// holds nothing, and from the ground it is a wall.
    #[test]
    fn on_top_a_wall_is_ground_and_lets_the_body_through() {
        let c = wall(0.8);
        let along = (Vec2::new(-2.0, 2.0), Vec2::new(2.0, 2.0));
        let on_top = crate::systems::collision::Body::new(0.8, 0.8);
        assert_eq!(
            c.slide(along.0, along.1, on_top),
            along.1,
            "walks along the top"
        );
        let below = crate::systems::collision::Body::new(0.0, 0.0);
        let into = (Vec2::new(0.0, 1.2), Vec2::new(0.0, 2.0));
        assert_ne!(
            c.slide(into.0, into.1, below),
            into.1,
            "a wall from the ground"
        );
        assert_eq!(c.ledge_under(Vec2::new(0.0, 2.0), 0.8 + 0.35), Some(0.8));
        assert_eq!(
            c.ledge_under(Vec2::new(0.0, 2.2), 1.15),
            Some(0.8),
            "a foot on the edge still stands on it"
        );
        assert_eq!(
            c.ledge_under(Vec2::new(0.0, 2.6), 1.15),
            None,
            "off the edge"
        );
        assert_eq!(
            c.ledge_under(Vec2::new(0.0, 2.0), 0.35),
            None,
            "not from below"
        );
        assert_eq!(
            c.ledge_at(Vec2::new(0.0, 2.0)),
            Some(0.8),
            "a remote inside is on it"
        );
        assert_eq!(c.ledge_at(Vec2::new(0.0, 1.0)), None);
    }

    #[test]
    fn a_low_wall_is_sat_on_from_the_ground_facing_out() {
        let (ground, water) = (flat(), dry());
        let c = wall(0.4);
        let map = Surroundings {
            colliders: &c,
            ground: &ground,
            water: &water,
        };
        let s = find_edge_seat(&map, Vec2::new(0.0, 1.2), SOUTH, 0.0).expect("a seat");
        assert!((1.85..=2.15).contains(&s.seat.y), "on the wall: {s:?}");
        assert!((s.out - -SOUTH).length() < 1e-5, "facing back out");
        assert!(!s.dangle(), "0.4 m: the feet reach the ground");
        assert!((s.lift(0.0) - (0.4 - SIT_SEAT_M)).abs() < 1e-5);
        // A remote viewer measures the same seat from the offset alone.
        assert_eq!(seat_on_edge(&map, s.seat, s.out), Some(s));
        // A little higher and the legs hang, the feet just off the ground.
        let c = wall(0.7);
        let map = Surroundings {
            colliders: &c,
            ground: &ground,
            water: &water,
        };
        let s = find_edge_seat(&map, Vec2::new(0.0, 1.2), SOUTH, 0.0).expect("a seat");
        assert!(s.dangle(), "0.7 m: the legs hang");
        assert!((s.lift(0.0) - (0.7 - DANGLE_SEAT_M)).abs() < 1e-5);
        // Too high to sit on from the ground.
        let c = wall(1.2);
        let map = Surroundings {
            colliders: &c,
            ground: &ground,
            water: &water,
        };
        assert!(find_edge_seat(&map, Vec2::new(0.0, 1.2), SOUTH, 0.0).is_none());
    }

    #[test]
    fn on_a_wall_top_the_legs_hang_over_the_edge() {
        let (ground, water) = (flat(), dry());
        let c = wall(1.2);
        let map = Surroundings {
            colliders: &c,
            ground: &ground,
            water: &water,
        };
        // Standing on the top facing along it (east): the seat is on a long side.
        let s = find_edge_seat(&map, Vec2::new(0.0, 2.0), Vec2::X, 1.2).expect("a seat");
        assert!(s.out.x.abs() < 1e-5, "out over a long side: {s:?}");
        assert!(s.dangle(), "1.2 m: the legs hang");
        assert!(
            (s.lift(1.2) + DANGLE_SEAT_M).abs() < 1e-5,
            "the clip sinks to put its seat on the top"
        );
    }

    /// Wherever the sitter turns out to stand once the ground is in, the seat
    /// is drawn on the wall: the seat point lies on it, and the clip's seat
    /// under the pelvis is at its top. Measured once, against the root the
    /// avatar had the frame it sat (a capture seats it as it lands, 1.2 m up
    /// before the ground streams in), the seat floated a metre over the grass
    /// beside the wall (2026-11-15); re-measured, it does not.
    #[test]
    fn an_edge_seat_is_drawn_on_the_wall_whatever_the_root_was() {
        let (ground, water) = (flat(), dry());
        for (h, from, soles) in [
            (0.7, Vec2::new(0.0, 1.2), 0.0),
            (1.2, Vec2::new(0.0, 2.0), 1.2),
        ] {
            let c = wall(h);
            let map = Surroundings {
                colliders: &c,
                ground: &ground,
                water: &water,
            };
            let facing = if soles > 0.0 { Vec2::X } else { SOUTH };
            let seat = find_edge_seat(&map, from, facing, soles).expect("a seat");
            assert_eq!(
                c.ledge_at(seat.seat),
                Some(h),
                "the seat point is on the wall: {seat:?}"
            );
            let mut avatar = crate::resources::AvatarState::default();
            // Sat while it still stood 1.2 m off its ground.
            sit_on_edge(&mut avatar, from, soles + 1.2, &seat);
            let seated = avatar.seated.unwrap();
            let stale = drawn_seat(soles, seated.lift, seated.dangle);
            assert!((stale - h).abs() > 1.0, "measured once it floats: {stale}");
            let e = remeasure(&map, from, &seated).expect("still an edge");
            let drawn = drawn_seat(soles, e.lift(soles), e.dangle());
            assert!(
                (drawn - h).abs() < 0.03,
                "a {h} m wall: the seat drawn at {drawn}"
            );
        }
    }

    #[test]
    fn a_quay_over_the_water_is_an_edge_and_a_bench_is_not() {
        let ground = flat();
        // Water from z = 1 on, its surface 1.4 m below the street.
        let water = |p: Vec2| (p.y > 1.0).then_some(-1.4);
        let c = BuildingColliders::default();
        let map = Surroundings {
            colliders: &c,
            ground: &ground,
            water: &water,
        };
        let s = find_edge_seat(&map, Vec2::ZERO, SOUTH, 0.0).expect("the quay");
        assert!(s.seat.y < 1.0 && s.seat.y > 0.8, "at the edge: {s:?}");
        assert!(s.dangle() && (s.drop - 1.4).abs() < 1e-5);
        // Facing away from the water there is no edge.
        assert!(find_edge_seat(&map, Vec2::ZERO, -SOUTH, 0.0).is_none());
        // A bench in a square: its seat is no edge, and keeps the bench's sit.
        let dry = dry();
        let square = Surroundings {
            colliders: &c,
            ground: &ground,
            water: &dry,
        };
        assert_eq!(seat_on_edge(&square, Vec2::ZERO, SOUTH), None);
    }
}
