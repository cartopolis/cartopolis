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

use cartopolis_geo::traffic::{DriveLine, Flow};

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
            // A one-way road may only be joined at the end traffic enters by.
            let allowed = match l.flow {
                Flow::Both => true,
                Flow::Forward => dir > 0.0,
                Flow::Backward => dir < 0.0,
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

    fn is_empty(&self) -> bool {
        self.cells.values().all(Vec::is_empty)
    }

    /// Every line with a point within `radius` of `near`, as keys.
    fn near(&self, near: Vec2, radius: f32) -> Vec<LineKey> {
        let r2 = radius * radius;
        let mut out = Vec::new();
        for (cell, lines) in &self.cells {
            for (index, l) in lines.iter().enumerate() {
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

/// One car in the pool. The entity exists for the life of the process; every
/// field here is rewritten when it is recycled onto a new road.
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

    for i in 0..MAX_CARS {
        commands.spawn((
            Mesh3d(mesh.clone()),
            MeshMaterial3d(paints[i % paints.len()].clone()),
            // Off-screen until placed. A car with no road must not be visible at
            // the origin, which is where the avatar is standing.
            Visibility::Hidden,
            Transform::default(),
            GridCell::default(),
            ChildOf(root),
            HideInSpace,
            Car {
                line: None,
                s: 0.0,
                dir: 1.0,
                speed_scale: 0.85 + rand01(0x0CA2, i as u64, 7) * 0.3,
            },
        ));
    }
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
        (&mut Car, &mut Transform, &mut GridCell, &mut Visibility),
        Without<FreeCamera>,
    >,
    mut frame: Local<u64>,
) {
    let show = layers.traffic_enabled;
    if !show || net.is_empty() {
        for (_, _, _, mut vis) in &mut cars {
            *vis = Visibility::Hidden;
        }
        return;
    }
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
        for (_, _, _, mut vis) in &mut cars {
            *vis = Visibility::Hidden;
        }
        return;
    }

    *frame = frame.wrapping_add(1);
    // Candidate roads are gathered once per frame, not once per car: the scan is
    // over the loaded ring and doing it ninety-six times would be the whole
    // budget for this layer.
    let candidates = net.near(cam, PLACE_RADIUS_M);
    if candidates.is_empty() {
        return;
    }
    let dt = time.delta_secs();

    for (i, (mut car, mut tf, mut cell, mut vis)) in cars.iter_mut().enumerate() {
        // Advance, then decide whether the car still has a road.
        let mut placed = car.line.and_then(|k| net.get(k).map(|l| (k, l)));
        if let Some((key, line)) = placed {
            car.s += line.speed * car.speed_scale * dt;
            let (pos, _) = line.at(if car.dir > 0.0 {
                car.s
            } else {
                line.length() - car.s
            });
            if pos.distance(cam) > RECYCLE_RADIUS_M {
                placed = None;
            } else if car.s > line.length() {
                // At the end of its road, drive **onto a connected one** rather
                // than vanishing. Without this a car pops out of existence at
                // every junction and reappears elsewhere, which is more visible
                // than the traffic itself.
                placed = hand_off(&net, &candidates, key, pos).and_then(
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
            car.dir = match line.flow {
                Flow::Forward => 1.0,
                Flow::Backward => -1.0,
                // A two-way street gets traffic both ways, decided per car.
                Flow::Both => {
                    if rand01(0xB1D1, i as u64, *frame) < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
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
        let pos = centre + right * (line.half_width * LANE_FRACTION);

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

    use bevy::asset::RenderAssetUsages;
    use bevy::render::mesh::{Indices, PrimitiveTopology};
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, sm.positions.clone());
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, sm.normals.clone());
    // The form's own vertex colours carry the glass and the tyres apart from the
    // body; the *body* colour is the material, which is the one thing a shared
    // mesh cannot vary per car.
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
        assert_eq!(net.near(Vec2::new(495.0, 0.0), 30.0).len(), 1);
        assert_eq!(net.near(Vec2::new(250.0, 400.0), 30.0).len(), 0);
    }

    /// The place and recycle radii must not be the same number, or a car sitting
    /// on the boundary is recycled and immediately re-placed on the wrong side of
    /// it, every frame.
    #[test]
    fn the_recycle_radius_is_outside_the_place_radius() {
        assert!(RECYCLE_RADIUS_M > PLACE_RADIUS_M * 1.2);
    }
}
