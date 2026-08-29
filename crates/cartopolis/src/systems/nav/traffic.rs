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
/// Returns the line, the direction (`+1` along its own order, `-1` against) and
/// the distance along it to start from — which is zero or the line's length,
/// because a car joining a road joins it at an end.
///
/// The road just left is excluded, or a car reaching a dead end turns round and
/// drives back down it forever, which reads as a fault rather than as traffic.
fn hand_off(
    net: &DriveNetwork,
    candidates: &[LineKey],
    from: LineKey,
    at: Vec2,
    kind: AgentKind,
) -> Option<(LineKey, f32, f32)> {
    let mut best: Option<(f32, LineKey, f32, f32)> = None;
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
            if best.is_none_or(|(bd, ..)| d < bd) {
                best = Some((d, key, dir, 0.0));
            }
        }
    }
    best.map(|(_, key, dir, s)| (key, dir, s))
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
        (a.lerp(b, t), (b - a) / seg)
    }
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
                (run > 1.0).then_some(WorldDriveLine {
                    points,
                    flow: l.flow,
                    speed: l.speed,
                    half_width: l.half_width,
                    uses: l.uses,
                    cumulative,
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

    /// The loudest road within earshot of `at`, 0..1
    /// (`soundscape::road_loudness`).
    ///
    /// A **maximum**, not a sum — summing makes a dense grid of back streets
    /// louder than a motorway, which is backwards. Public because the soundscape
    /// asks it: the city's hum is a function of the map, and this resource is
    /// where the map's roads already live, so it costs no new data and one scan
    /// of the loaded ring.
    #[must_use]
    pub fn loudest_road(&self, at: Vec2) -> f32 {
        let mut best = 0.0f32;
        for lines in self.cells.values() {
            for l in lines {
                if !l.uses.drive {
                    continue;
                }
                // Distance to the nearest *vertex*, not to the nearest segment: a
                // simplified line has a vertex every few metres, so the error is
                // smaller than the ear can place a sound, and the exact version
                // would cost a projection per segment per frame.
                let d2 = l
                    .points
                    .iter()
                    .map(|p| p.distance_squared(at))
                    .fold(f32::MAX, f32::min);
                best = best.max(crate::systems::soundscape::road_loudness(
                    l.speed,
                    d2.sqrt(),
                ));
            }
        }
        best
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
#[derive(Clone, Copy, PartialEq, Eq)]
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
        // Ordered `.after(setup_camera)` in `lib.rs`, which is what spawns the
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
    let dt = time.delta_secs();

    for (i, (mut car, &kind, mut tf, mut cell, mut vis)) in cars.iter_mut().enumerate() {
        if !showing(kind) {
            *vis = Visibility::Hidden;
            continue;
        }
        let candidates: &[LineKey] = match kind {
            AgentKind::Car => &roads,
            AgentKind::Walker => &paths,
        };
        if candidates.is_empty() {
            *vis = Visibility::Hidden;
            continue;
        }
        // Advance, then decide whether the car still has a road.
        let mut placed = car.line.and_then(|k| net.get(k).map(|l| (k, l)));
        if let Some((key, line)) = placed {
            car.s += kind.speed(line) * car.speed_scale * dt;
            let (pos, _) = line.at(if car.dir > 0.0 {
                car.s
            } else {
                line.length() - car.s
            });
            if pos.distance(cam) > kind.recycle_radius() {
                placed = None;
            } else if car.s > line.length() {
                // At the end of its road, drive **onto a connected one** rather
                // than vanishing. Without this a car pops out of existence at
                // every junction and reappears elsewhere, which is more visible
                // than the traffic itself.
                placed = hand_off(&net, candidates, key, pos, kind).and_then(
                    |(next_key, next_dir, next_s)| {
                        let l = net.get(next_key)?;
                        car.line = Some(next_key);
                        car.dir = next_dir;
                        car.s = next_s;
                        Some((next_key, l))
                    },
                );
            }
        }

        if placed.is_none() {
            // Recycled onto a road near the camera. The `frame` term is what stops
            // every car that runs out on the same frame picking the same road.
            //
            // Three draws, keep the fastest road: a cheap bias toward the streets
            // that would actually carry traffic. Picking uniformly spreads the
            // pool evenly over every service alley and back lane in the ring,
            // which is how the first capture ended up with one car on a main
            // street. The road's speed stands in for its importance because that
            // is exactly what `drive_speed` encodes.
            let key = (0..3)
                .map(|t| {
                    let r = rand01(0xD1CE, i as u64, frame.wrapping_add(t));
                    candidates[((r * candidates.len() as f32) as usize).min(candidates.len() - 1)]
                })
                .max_by(|a, b| {
                    let sp = |k: &LineKey| net.get(*k).map_or(0.0, |l| l.speed);
                    sp(a).total_cmp(&sp(b))
                })
                .expect("three draws");
            let Some(line) = net.get(key) else {
                continue;
            };
            car.line = Some(key);
            let coin = if rand01(0xB1D1, i as u64, *frame) < 0.5 {
                1.0
            } else {
                -1.0
            };
            car.dir = match (kind, line.flow) {
                // A pavement is two-way whatever the carriageway beside it does —
                // which is what somebody walking up a one-way street is doing.
                (AgentKind::Walker, _) | (_, Flow::Both) => coin,
                (_, Flow::Forward) => 1.0,
                (_, Flow::Backward) => -1.0,
            };
            // Start somewhere along the road rather than at its end, or every
            // recycled car appears at a junction.
            car.s = rand01(0x5EED, i as u64, *frame) * line.length() * 0.8;
            placed = Some((key, line));
        }

        let Some((_, line)) = placed else {
            *vis = Visibility::Hidden;
            continue;
        };

        // Position along the line, in the direction this car drives it.
        let along = if car.dir > 0.0 {
            car.s
        } else {
            line.length() - car.s
        };
        let (centre, heading) = line.at(along);
        let travel = heading * car.dir;
        // Right of the travel direction, in the XZ plane where +x is east and +z
        // is south: rotating the heading a quarter turn one way gives the kerb
        // side. Right-hand traffic — see `LANE_FRACTION`.
        let right = Vec2::new(-travel.y, travel.x);
        let pos = centre + right * kind.offset(line);

        let y = terrain.at(pos.x, pos.y) + CAR_RIDE_HEIGHT;
        let (grid_cell, local) = grid.translation_to_grid(Vec3::new(pos.x, y, pos.y).as_dvec3());
        *cell = grid_cell;
        tf.translation = local;
        // The model's +z is its nose (`cartopolis_geo::furniture`'s CAR), so the
        // yaw that points +z along the travel direction is measured from +z.
        tf.rotation = Quat::from_rotation_y(travel.x.atan2(travel.y));
        *vis = Visibility::Inherited;
    }
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
}
