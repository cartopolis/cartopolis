//! Driving a car (asked for 2026-11-25, `docs/notes/driving.md`).
//!
//! The avatar walks up to a parked car, or calls its own, gets in, and drives
//! it: the handling is `cartopolis_core::car` (tyres that slip, an engine with
//! gears), the walls are the same `BuildingColliders` the walk slides along,
//! and the body sits at the wheel in the sit clip, displaced onto the driver's
//! seat the way a bench seats it (`resources::Seated`).
//!
//! **The sim is told a velocity, as for a walk.** It has no map and so no walls
//! to stop a car; it integrates the car's velocity the way it integrates a
//! walk, capped at `motion::MAX_DRIVE_SPEED` while the intent says it is driving
//! (protocol 52). Everyone else draws the car under the avatar from
//! `AvatarStateDto::driving`.
//!
//! Getting in and out is a short walk, not a jump: from where the avatar stands
//! to the car's middle, and from there out beside the driver's door. A walk is
//! what the sim already moves an avatar by, so no teleport (with its re-anchor
//! and its arrival) is spent on a metre and a half.

use std::collections::HashMap;

use bevy::prelude::*;
use cartopolis_core::car::{self, CarInput, CarParams, CarState};
use cartopolis_core::protocol::{CarKind, Driving};
use cartopolis_geo::agent_models::CarBody;

use crate::components::{Avatar, RemoteAvatar};
use crate::grid::WORLD_CELL_EDGE;
use crate::resources::{AvatarState, CameraMode, CameraState, Seated};

pub use crate::resources::CarSpot;

impl CarSpot {
    /// The parked car `systems::world_places` holds, as one to get into.
    #[must_use]
    pub fn parked(car: &crate::systems::world_places::ParkedCar) -> Self {
        let [r, g, b] = [
            (car.paint >> 16) as u8,
            (car.paint >> 8) as u8,
            car.paint as u8,
        ];
        Self {
            pos: car.pos,
            yaw: car.yaw,
            kind: kind_of(car.body),
            paint: [r, g, b],
            from_lot: true,
        }
    }
}

/// The wire's name for a body.
#[must_use]
pub fn kind_of(body: CarBody) -> CarKind {
    match body {
        CarBody::Hatchback => CarKind::Hatchback,
        CarBody::Saloon => CarKind::Saloon,
        CarBody::Estate => CarKind::Estate,
        CarBody::Van => CarKind::Van,
    }
}

/// The body for a wire name.
#[must_use]
pub fn body_of(kind: CarKind) -> CarBody {
    match kind {
        CarKind::Hatchback => CarBody::Hatchback,
        CarKind::Saloon => CarBody::Saloon,
        CarKind::Estate => CarBody::Estate,
        CarKind::Van => CarBody::Van,
    }
}

/// How each body drives.
#[must_use]
pub fn params_for(kind: CarKind) -> CarParams {
    match kind {
        CarKind::Hatchback => CarParams::hatchback(),
        CarKind::Saloon => CarParams::saloon(),
        CarKind::Estate => CarParams::estate(),
        CarKind::Van => CarParams::van(),
    }
}

/// Where in getting in, driving and getting out a car is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    /// Walking up to it: the car stands where it was, and the avatar walks to
    /// its middle. Seconds so far, so a walk that cannot arrive gives up.
    Boarding(f32),
    Driving,
    /// Out of it and walking to the spot beside the door; the car is already
    /// [`Garage::left`].
    Alighting {
        to: Vec2,
        secs: f32,
    },
}

/// The car this person is in, or getting into or out of.
#[derive(Clone, Debug)]
pub struct Car {
    pub spot: CarSpot,
    pub params: CarParams,
    pub state: CarState,
    pub phase: Phase,
    /// Asked to get out at speed: brake to a stop first.
    pub stopping: bool,
}

/// Everything about this person's cars.
#[derive(Resource, Debug)]
pub struct Garage {
    pub car: Option<Car>,
    /// The car last driven, where it was left: drawn and solid until it is
    /// driven again or another is taken. One at a time — a person has one car
    /// at the kerb, not a trail of them.
    pub left: Option<CarSpot>,
    /// What this person's own car looks like, for "Call my car".
    pub own: (CarKind, [u8; 3]),
    /// The map anchor the spots above were measured in: a re-anchor moves
    /// the frame under them, and they are dropped.
    anchor: Option<(f64, f64, u32)>,
}

impl Default for Garage {
    fn default() -> Self {
        Self {
            car: None,
            left: None,
            own: (CarKind::Hatchback, [178, 34, 34]),
            anchor: None,
        }
    }
}

/// The walk up to a car and away from one, m/s: a brisk walk.
const BOARD_SPEED: f32 = 2.2;
/// A walk up to a car that has not arrived by now does not: it is in at once.
const BOARD_GIVE_UP_S: f32 = 3.0;
/// Close enough to the car's middle to be in it, m.
const BOARD_ARRIVED_M: f32 = 0.12;
/// Slow enough to get out, m/s.
const GET_OUT_BELOW: f32 = 0.6;
/// How far beside the body the door step is, m.
const DOOR_STEP_M: f32 = 0.55;
/// Within this a car one can get into is offered, m.
pub const CAR_REACH_M: f32 = 3.5;

/// Where the driver sits, in the car's own frame (+z forward, +x to the left):
/// the left front seat, as in a Dutch car.
const DRIVER_SEAT: Vec3 = Vec3::new(0.36, 0.0, -0.15);
/// How far the body is raised onto the driver's seat over the sit clip's own
/// stool height, m.
pub const DRIVER_LIFT: f32 = -0.04;

/// Restitution against a wall: a car does not bounce much.
const WALL_BOUNCE: f32 = 0.15;
/// What a scrape along a wall leaves of the speed along it.
const WALL_SCRAPE: f32 = 0.85;

/// The driver's seat as a seat offset: world XZ from the car's middle.
#[must_use]
pub fn driver_seat_offset(yaw: f32) -> Vec2 {
    let s = Quat::from_rotation_y(yaw) * DRIVER_SEAT;
    Vec2::new(s.x, s.z)
}

/// The seat the driving body is held in (`resources::Seated`), keeping the
/// blend of the one it already has so getting in is a slide, not a cut.
fn driver_seat(yaw: f32, current: Option<Seated>) -> Seated {
    Seated {
        offset: driver_seat_offset(yaw),
        yaw,
        blend: current.filter(|s| !s.rising).map_or(0.0, |s| s.blend),
        rising: false,
        lift: DRIVER_LIFT,
        dangle: false,
        edge: false,
    }
}

/// Points round a car's outline, in its own frame: the four corners, the
/// middle of each side, and the two ends. What touches a wall first.
fn outline(params: &CarParams) -> [Vec2; 8] {
    let (l, w) = (params.length * 0.5, params.width * 0.5);
    [
        Vec2::new(w, l),
        Vec2::new(-w, l),
        Vec2::new(w, -l),
        Vec2::new(-w, -l),
        Vec2::new(w, 0.0),
        Vec2::new(-w, 0.0),
        Vec2::new(0.0, l),
        Vec2::new(0.0, -l),
    ]
}

/// A point of the car's own frame in world XZ.
fn to_world(pos: Vec2, yaw: f32, local: Vec2) -> Vec2 {
    let (s, c) = yaw.sin_cos();
    // forward = (sin, cos), left = (cos, −sin)
    pos + Vec2::new(local.y * s + local.x * c, local.y * c - local.x * s)
}

/// What a car runs into, asked of the map: walls, props and water.
pub trait Obstacles {
    /// Whether the move `from → to` is refused, and if so which way is out.
    fn refuses(&self, from: Vec2, to: Vec2) -> Option<Vec2>;
}

/// Move a car from `pos` by its velocity for `dt`, stopped and turned by what
/// it runs into. Returns where it got to; the velocity and the turn rate in
/// `state` lose what the contact took.
pub fn collide(
    obstacles: &impl Obstacles,
    pos: Vec2,
    params: &CarParams,
    state: &mut CarState,
    dt: f32,
) -> Vec2 {
    let v = Vec2::from(state.world_velocity());
    let to = pos + v * dt;
    let pts = outline(params);
    let mut normal = Vec2::ZERO;
    let mut contact = Vec2::ZERO;
    let mut hits = 0.0;
    for p in pts {
        let a = to_world(pos, state.yaw, p);
        let b = to_world(to, state.yaw, p);
        if let Some(n) = obstacles.refuses(a, b) {
            normal += n;
            contact += a - pos;
            hits += 1.0;
        }
    }
    if hits == 0.0 {
        return to;
    }
    let n = normal.normalize_or_zero();
    let n = if n == Vec2::ZERO {
        -v.normalize_or_zero()
    } else {
        n
    };
    let r = contact / hits;
    let into = v.dot(n);
    let mut out = v;
    if into < 0.0 {
        let along = v - n * into;
        out = along * WALL_SCRAPE - n * into * WALL_BOUNCE;
        // The blow turns the car about its middle: a corner caught spins it.
        let dv = out - v;
        let mass_over_inertia = params.mass / params.inertia;
        state.yaw_rate += 0.5 * mass_over_inertia * (r.y * dv.x - r.x * dv.y);
    }
    state.set_world_velocity(out.into());
    // Try the move again with what is left; if that is refused too, stay.
    let again = pos + out * dt;
    let clear = pts.iter().all(|&p| {
        obstacles
            .refuses(to_world(pos, state.yaw, p), to_world(again, state.yaw, p))
            .is_none()
    });
    if clear {
        again
    } else {
        pos
    }
}

/// The map's obstacles for a car standing on `ground`.
struct MapObstacles<'a> {
    colliders: &'a crate::systems::collision::BuildingColliders,
    terrain: &'a crate::systems::terrain::TerrainField,
    /// From the colliders' frame to the terrain's.
    base: Vec2,
    buildings: bool,
    ground: f32,
}

impl Obstacles for MapObstacles<'_> {
    fn refuses(&self, from: Vec2, to: Vec2) -> Option<Vec2> {
        // The quay is the kerb a car stops at: nobody drives into a canal.
        let (t, f) = (to - self.base, from - self.base);
        if self.terrain.swim_surface_at(t.x, t.y).is_some()
            && self.terrain.swim_surface_at(f.x, f.y).is_none()
        {
            return Some((from - to).normalize_or_zero());
        }
        if !self.buildings {
            return None;
        }
        let body = crate::systems::collision::Body::new(self.ground, self.ground);
        if !self.colliders.refuses(from, to, body) {
            return None;
        }
        Some(
            self.colliders
                .eject_dir(to, body)
                .unwrap_or_else(|| (from - to).normalize_or_zero()),
        )
    }
}

/// What the driver asks for this frame, from the keys or the on-screen stick.
fn read_input(
    keyboard: &ButtonInput<KeyCode>,
    osd: &crate::systems::map_osd::OsdInput,
    blocked: bool,
) -> CarInput {
    if blocked {
        return CarInput::default();
    }
    let held = |a: KeyCode, b: KeyCode| keyboard.pressed(a) || keyboard.pressed(b);
    let mut input = CarInput {
        throttle: f32::from(u8::from(held(KeyCode::KeyW, KeyCode::ArrowUp))),
        brake: f32::from(u8::from(held(KeyCode::KeyS, KeyCode::ArrowDown))),
        steer: f32::from(u8::from(held(KeyCode::KeyA, KeyCode::ArrowLeft)))
            - f32::from(u8::from(held(KeyCode::KeyD, KeyCode::ArrowRight))),
        handbrake: keyboard.pressed(KeyCode::Space),
    };
    // The stick: up is the accelerator, down the brake, sideways the wheel.
    // Its `x` is to the right, and a positive steer is to the left.
    if input == CarInput::default() && osd.walk.length_squared() > 1e-6 {
        input.throttle = osd.walk.y.max(0.0).min(1.0);
        input.brake = (-osd.walk.y).max(0.0).min(1.0);
        input.steer = (-osd.walk.x).clamp(-1.0, 1.0);
    }
    input
}

/// Where the avatar is drawn, in the colliders' frame (anchored world XZ),
/// and the offset from that frame to the terrain's: `climb::avatar_frame`'s
/// pair. Online the drawn place, not the sim's: a car meets the wall where it
/// is seen to, not a snapshot's latency behind.
fn avatar_xz(
    online: bool,
    state: &AvatarState,
    tf: &Transform,
    cell: &big_space::prelude::GridCell,
) -> (Vec2, Vec2) {
    let at = if online {
        state.display_position
    } else {
        tf.translation
    };
    crate::systems::climb::avatar_frame(online, at, Some(cell))
}

/// The map this system reads, bundled under Bevy's parameter cap.
#[derive(bevy::ecs::system::SystemParam)]
pub struct DriveWorld<'w> {
    colliders: Res<'w, crate::systems::collision::BuildingColliders>,
    layers: Res<'w, crate::systems::layer_state::DataLayers>,
    terrain: Res<'w, crate::systems::terrain::TerrainField>,
    anchor: Res<'w, crate::resources::MapAnchor>,
    places: ResMut<'w, crate::systems::world_places::WorldPlaces>,
}

/// Get in, drive, get out: everything that moves the avatar while a car is
/// involved. Runs before the walk, which stands aside while
/// [`AvatarState::car_control`] is set.
#[allow(clippy::too_many_arguments)]
pub fn drive(
    time: Res<Time>,
    keyboard: Res<ButtonInput<KeyCode>>,
    osd: Res<crate::systems::map_osd::OsdInput>,
    camera_state: Res<CameraState>,
    ui_input: Res<crate::systems::ui_input::UiInput>,
    edit_dialog: Res<crate::resources::EditDialogState>,
    online: Option<Res<crate::resources::OnlineSession>>,
    mut avatar_state: ResMut<AvatarState>,
    mut garage: ResMut<Garage>,
    mut world: DriveWorld,
    mut avatar_q: Query<(&mut Transform, &big_space::prelude::GridCell), With<Avatar>>,
) {
    let garage = &mut *garage;
    let dt = time.delta_secs();
    let online = online.is_some();

    // A re-anchor moves the frame every spot was measured in.
    if garage.anchor != world.anchor.geo {
        if garage.anchor.is_some() {
            garage.left = None;
            world.places.clear_taken();
            if let Some(car) = &mut garage.car {
                car.spot.pos = Vec2::NAN;
            }
        }
        garage.anchor = world.anchor.geo;
    }

    let Ok((mut tf, cell)) = avatar_q.single_mut() else {
        return;
    };
    let (here, base) = avatar_xz(online, &avatar_state, &tf, cell);

    // ── Requests ─────────────────────────────────────────────────────────────
    if std::mem::take(&mut avatar_state.wants_call_car) && garage.car.is_none() {
        // The own car pulls up beside the avatar, facing the way it faces, on
        // whichever side has room.
        let yaw = avatar_state.facing_yaw;
        let (kind, paint) = garage.own;
        let params = params_for(kind);
        let side = params.width * 0.5 + 1.0;
        let obstacles = MapObstacles {
            colliders: &world.colliders,
            terrain: &world.terrain,
            base,
            buildings: world.layers.buildings_enabled,
            ground: world.terrain.ground_at(here.x - base.x, here.y - base.y),
        };
        let fits = |pos: Vec2| {
            outline(&params)
                .iter()
                .all(|&p| obstacles.refuses(pos, to_world(pos, yaw, p)).is_none())
        };
        let right = to_world(here, yaw, Vec2::new(-side, 0.0));
        let left = to_world(here, yaw, Vec2::new(side, 0.0));
        let ahead = to_world(here, yaw, Vec2::new(0.0, params.length * 0.5 + 1.2));
        let pos = [right, left, ahead]
            .into_iter()
            .find(|p| fits(*p))
            .unwrap_or(right);
        garage.left = Some(CarSpot {
            pos,
            yaw,
            kind,
            paint,
            from_lot: false,
        });
    }
    if let Some(spot) = avatar_state.wants_car.take() {
        if garage.car.is_none() && camera_state.mode == CameraMode::Avatar {
            if spot.from_lot {
                world.places.take_car(spot.pos);
            } else if garage.left.is_some_and(|l| l.pos.distance(spot.pos) < 0.5) {
                garage.left = None;
            }
            avatar_state.stand_up();
            let params = params_for(spot.kind);
            garage.car = Some(Car {
                state: CarState::parked(spot.yaw, &params),
                params,
                spot,
                phase: Phase::Boarding(0.0),
                stopping: false,
            });
        }
    }
    if std::mem::take(&mut avatar_state.wants_out) {
        if let Some(car) = &mut garage.car {
            if car.phase == Phase::Driving {
                car.stopping = true;
            }
        }
    }
    // The car left at the kerb is solid.
    let extra = garage.left.map_or_else(Vec::new, |l| {
        let params = params_for(l.kind);
        let reach = params.length * 0.5 - params.width * 0.5;
        let along = Vec2::new(l.yaw.sin(), l.yaw.cos());
        vec![
            (l.pos + along * reach, params.width * 0.5),
            (l.pos - along * reach, params.width * 0.5),
        ]
    });
    world.places.set_extra_props(extra);

    let Some(car) = &mut garage.car else {
        avatar_state.car_control = false;
        avatar_state.driving = None;
        return;
    };
    avatar_state.car_control = true;
    if !car.spot.pos.is_finite() {
        // The map moved under a car still being walked up to: it is gone.
        garage.car = None;
        avatar_state.car_control = false;
        avatar_state.driving = None;
        return;
    }

    // A walk to a point, as the sim is told any walk: a direction and a pace.
    let walk_to = |avatar_state: &mut AvatarState, tf: &mut Transform, to: Vec2| -> bool {
        let d = to - here;
        let dist = d.length();
        if dist < BOARD_ARRIVED_M {
            avatar_state.ground_speed = 0.0;
            avatar_state.is_walking = false;
            return true;
        }
        let dir = d / dist;
        let speed = BOARD_SPEED.min(dist / dt.max(1e-3));
        avatar_state.move_dir = Vec3::new(dir.x, 0.0, dir.y);
        avatar_state.ground_speed = speed;
        avatar_state.is_walking = true;
        avatar_state.gait = crate::resources::Gait::Walk;
        avatar_state.facing_yaw = dir.x.atan2(dir.y);
        if !online {
            tf.translation.x += dir.x * speed * dt;
            tf.translation.z += dir.y * speed * dt;
        }
        false
    };

    let mut out = false;
    match car.phase {
        Phase::Boarding(secs) => {
            avatar_state.driving = None;
            let arrived = walk_to(&mut avatar_state, &mut tf, car.spot.pos);
            if arrived || secs + dt > BOARD_GIVE_UP_S {
                car.phase = Phase::Driving;
                if !online {
                    let d = car.spot.pos - here;
                    tf.translation.x += d.x;
                    tf.translation.z += d.y;
                }
            } else {
                car.phase = Phase::Boarding(secs + dt);
            }
        }
        Phase::Driving => {
            let blocked = camera_state.mode != CameraMode::Avatar
                || ui_input.ui_owns_keyboard(edit_dialog.visible, &keyboard);
            let mut input = read_input(&keyboard, &osd, blocked);
            if car.stopping || camera_state.mode != CameraMode::Avatar {
                // Out of the driver's view, or on the way out: stop.
                input = CarInput {
                    brake: 1.0,
                    ..CarInput::default()
                };
            }
            car::step(&mut car.state, &car.params, input, 1.0, dt);
            let ground = world.terrain.ground_at(here.x - base.x, here.y - base.y);
            let obstacles = MapObstacles {
                colliders: &world.colliders,
                terrain: &world.terrain,
                base,
                buildings: world.layers.buildings_enabled,
                ground,
            };
            let to = collide(&obstacles, here, &car.params, &mut car.state, dt);
            let moved = if dt > 0.0 {
                (to - here) / dt
            } else {
                Vec2::ZERO
            };
            let speed = moved.length();
            avatar_state.facing_yaw = car.state.yaw;
            avatar_state.move_dir = if speed > 1e-3 {
                Vec3::new(moved.x / speed, 0.0, moved.y / speed)
            } else {
                Vec3::new(car.state.yaw.sin(), 0.0, car.state.yaw.cos())
            };
            avatar_state.ground_speed = speed;
            avatar_state.is_walking = false;
            avatar_state.wants_jump = false;
            avatar_state.seated = Some(driver_seat(car.state.yaw, avatar_state.seated));
            avatar_state.driving = Some(Driving {
                kind: car.spot.kind,
                paint: car.spot.paint,
                steer: Driving::steer_of(car.state.steer),
                braking: car.state.braking,
            });
            if !online {
                tf.translation.x += to.x - here.x;
                tf.translation.z += to.y - here.y;
                tf.translation.y = crate::systems::avatar::GROUND_HEIGHT
                    + world.terrain.ground_at(to.x - base.x, to.y - base.y);
                avatar_state.vertical_velocity = 0.0;
                avatar_state.grounded = true;
            }
            if car.stopping && car.state.speed() < GET_OUT_BELOW {
                // Out by the driver's door, or the other one if a wall is
                // against it; the car stays where it stopped.
                let side = car.params.width * 0.5 + DOOR_STEP_M;
                let doors = [Vec2::new(side, -0.1), Vec2::new(-side, -0.1)];
                let to = doors
                    .iter()
                    .map(|d| to_world(here, car.state.yaw, *d))
                    .find(|d| obstacles.refuses(here, *d).is_none())
                    .unwrap_or_else(|| to_world(here, car.state.yaw, doors[0]));
                garage.left = Some(CarSpot {
                    pos: here,
                    yaw: car.state.yaw,
                    ..car.spot
                });
                car.phase = Phase::Alighting { to, secs: 0.0 };
                avatar_state.driving = None;
                avatar_state.stand_up();
            }
        }
        Phase::Alighting { to, secs } => {
            avatar_state.driving = None;
            let arrived = walk_to(&mut avatar_state, &mut tf, to);
            if arrived || secs + dt > BOARD_GIVE_UP_S {
                out = true;
            } else {
                car.phase = Phase::Alighting {
                    to,
                    secs: secs + dt,
                };
            }
        }
    }

    if out {
        garage.car = None;
        avatar_state.car_control = false;
        avatar_state.ground_speed = 0.0;
        avatar_state.move_dir = Vec3::ZERO;
    }
    let rotation = Quat::from_rotation_y(avatar_state.facing_yaw);
    if tf.rotation != rotation {
        tf.rotation = rotation;
    }
    if !online {
        avatar_state.position = tf.translation;
        avatar_state.display_position = tf.translation;
        avatar_state.velocity = avatar_state.move_dir * avatar_state.ground_speed;
    }
}

// ── Drawing ──────────────────────────────────────────────────────────────────

/// The meshes every drawn car shares, and its materials.
#[derive(Resource)]
pub struct CarAssets {
    meshes: HashMap<CarKind, [Handle<Mesh>; 3]>,
    trim: Handle<StandardMaterial>,
    tail_off: Handle<StandardMaterial>,
    tail_on: Handle<StandardMaterial>,
    paints: HashMap<[u8; 3], Handle<StandardMaterial>>,
}

/// One drawn car: the entity that places it and the lamp part.
#[derive(Clone, Copy, Debug)]
pub struct Drawn {
    root: Entity,
    tail: Entity,
    kind: CarKind,
    paint: [u8; 3],
    braking: bool,
}

/// Which car a drawing is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Which {
    /// The car this person drives, or is walking up to.
    Mine,
    /// The one left at the kerb.
    Left,
    /// Another avatar's.
    Remote(Entity),
}

/// Build the shared meshes once.
pub fn setup_car_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut by_kind = HashMap::new();
    for kind in [
        CarKind::Hatchback,
        CarKind::Saloon,
        CarKind::Estate,
        CarKind::Van,
    ] {
        let m = body_of(kind).build();
        by_kind.insert(
            kind,
            [
                meshes.add(crate::systems::traffic::model_mesh_to_bevy(&m.body)),
                meshes.add(crate::systems::traffic::model_mesh_to_bevy(&m.trim)),
                meshes.add(crate::systems::traffic::model_mesh_to_bevy(&m.tail)),
            ],
        );
    }
    commands.insert_resource(CarAssets {
        meshes: by_kind,
        trim: materials.add(StandardMaterial {
            perceptual_roughness: 0.3,
            reflectance: 0.5,
            ..default()
        }),
        tail_off: materials.add(StandardMaterial {
            base_color: Color::linear_rgb(0.7, 0.7, 0.7),
            unlit: true,
            ..default()
        }),
        tail_on: materials.add(StandardMaterial {
            base_color: Color::linear_rgb(4.0, 4.0, 4.0),
            unlit: true,
            ..default()
        }),
        paints: HashMap::new(),
    });
}

/// Spawn one car's drawing under `parent`.
fn spawn_car(
    commands: &mut Commands,
    assets: &mut CarAssets,
    materials: &mut Assets<StandardMaterial>,
    parent: Entity,
    kind: CarKind,
    paint: [u8; 3],
) -> Drawn {
    let paint_mat = assets
        .paints
        .entry(paint)
        .or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: Color::srgb_u8(paint[0], paint[1], paint[2]),
                perceptual_roughness: 0.32,
                reflectance: 0.6,
                ..default()
            })
        })
        .clone();
    let [body, trim, tail] = assets.meshes[&kind].clone();
    let root = commands
        .spawn((Transform::default(), Visibility::default(), ChildOf(parent)))
        .id();
    commands.spawn((Mesh3d(body), MeshMaterial3d(paint_mat), ChildOf(root)));
    commands.spawn((
        Mesh3d(trim),
        MeshMaterial3d(assets.trim.clone()),
        ChildOf(root),
    ));
    let tail = commands
        .spawn((
            Mesh3d(tail),
            MeshMaterial3d(assets.tail_off.clone()),
            ChildOf(root),
            bevy::light::NotShadowCaster,
        ))
        .id();
    Drawn {
        root,
        tail,
        kind,
        paint,
        braking: false,
    }
}

/// The pitch and roll of a car standing at `pos` on the ground.
fn lie_on_ground(
    terrain: &crate::systems::terrain::TerrainField,
    base: Vec2,
    pos: Vec2,
    yaw: f32,
    params: &CarParams,
) -> (f32, Quat) {
    let h = |local: Vec2| {
        let p = to_world(pos, yaw, local) - base;
        terrain.ground_at(p.x, p.y)
    };
    let (a, b, w) = (params.cg_to_front, params.cg_to_rear, params.width * 0.5);
    let front = h(Vec2::new(0.0, a));
    let rear = h(Vec2::new(0.0, -b));
    let left = h(Vec2::new(w, 0.0));
    let right = h(Vec2::new(-w, 0.0));
    let pitch = ((front - rear) / (a + b)).atan();
    let roll = ((left - right) / (2.0 * w)).atan();
    let middle = (front + rear) * 0.5;
    // Nose up is a turn about −x for a body facing +z; left up about −z.
    (
        middle,
        Quat::from_rotation_x(-pitch) * Quat::from_rotation_z(-roll),
    )
}

/// Draw every car: this person's (being driven, walked up to, or left at the
/// kerb) and everyone else's, under their avatars.
///
/// The cars that are not under a driving avatar hang off the local avatar too,
/// placed by their offset from it, so none of them needs a grid cell of its own:
/// they are never more than a street away from the avatar that left them.
#[allow(clippy::too_many_arguments)]
pub fn draw_cars(
    mut commands: Commands,
    assets: Option<ResMut<CarAssets>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    garage: Res<Garage>,
    avatar_state: Res<AvatarState>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    online: Option<Res<crate::resources::OnlineSession>>,
    avatar: Query<(Entity, &Transform, &big_space::prelude::GridCell), With<Avatar>>,
    remotes: Query<(Entity, &RemoteAvatar)>,
    mut parts: Query<&mut Transform, Without<Avatar>>,
    mut tails: Query<&mut MeshMaterial3d<StandardMaterial>>,
    mut drawn: Local<HashMap<Which, Drawn>>,
) {
    let Some(mut assets) = assets else {
        return;
    };
    let mut want: Vec<(Which, Entity, CarKind, [u8; 3], bool, Transform)> = Vec::new();
    if let Ok((me, tf, cell)) = avatar.single() {
        let (at, base) = avatar_xz(online.is_some(), &avatar_state, tf, cell);
        // A car standing still, as a child of the avatar: its offset turned
        // into the avatar's own frame.
        let standing = |spot: &CarSpot| {
            let params = params_for(spot.kind);
            let (ground, lie) = lie_on_ground(&terrain, base, spot.pos, spot.yaw, &params);
            let d = spot.pos - at;
            let world = Vec3::new(
                d.x,
                ground + crate::systems::avatar::GROUND_HEIGHT - tf.translation.y,
                d.y,
            );
            let inv = tf.rotation.inverse();
            Transform::from_translation(inv * world)
                .with_rotation(inv * Quat::from_rotation_y(spot.yaw) * lie)
        };
        if let Some(car) = &garage.car {
            match car.phase {
                Phase::Boarding(_) => {
                    want.push((
                        Which::Mine,
                        me,
                        car.spot.kind,
                        car.spot.paint,
                        false,
                        standing(&car.spot),
                    ));
                }
                Phase::Driving => {
                    let (_, lie) = lie_on_ground(&terrain, base, at, car.state.yaw, &car.params);
                    want.push((
                        Which::Mine,
                        me,
                        car.spot.kind,
                        car.spot.paint,
                        car.state.braking,
                        Transform::from_rotation(lie),
                    ));
                }
                Phase::Alighting { .. } => {}
            }
        }
        if let Some(left) = &garage.left {
            want.push((
                Which::Left,
                me,
                left.kind,
                left.paint,
                false,
                standing(left),
            ));
        }
    }
    for (e, r) in &remotes {
        if let Some(d) = r.driving {
            want.push((
                Which::Remote(e),
                e,
                d.kind,
                d.paint,
                d.braking,
                Transform::default(),
            ));
        }
    }

    // Retire what is no longer wanted, or no longer the same car.
    drawn.retain(|which, d| {
        let keep = want
            .iter()
            .any(|(w, _, kind, paint, ..)| w == which && *kind == d.kind && *paint == d.paint);
        if !keep {
            commands.entity(d.root).try_despawn();
        }
        keep
    });
    for (which, parent, kind, paint, braking, place) in want {
        let d = match drawn.get_mut(&which) {
            Some(d) => d,
            None => {
                let d = spawn_car(
                    &mut commands,
                    &mut assets,
                    &mut materials,
                    parent,
                    kind,
                    paint,
                );
                drawn.insert(which, d);
                drawn.get_mut(&which).expect("just inserted")
            }
        };
        if let Ok(mut tf) = parts.get_mut(d.root) {
            if *tf != place {
                *tf = place;
            }
        }
        if d.braking != braking {
            if let Ok(mut m) = tails.get_mut(d.tail) {
                m.0 = if braking {
                    assets.tail_on.clone()
                } else {
                    assets.tail_off.clone()
                };
                d.braking = braking;
            }
        }
    }
}

/// Fold a taken parked car out of its cell's street-furniture mesh: the
/// vertices inside its footprint are pulled to one point, so its triangles
/// draw nothing. The car parks are one mesh per cell (`map_geometry`), and
/// rebuilding a cell for one car would cost a worker round trip and a whole
/// re-upload; this is one mesh edit, once per car taken, and again when its
/// cell streams back in.
#[allow(clippy::type_complexity)]
pub fn fold_taken_cars(
    places: Res<crate::systems::world_places::WorldPlaces>,
    surfaces: Query<
        (Entity, &Mesh3d, &Transform, &big_space::prelude::GridCell),
        With<crate::components::FurnitureSurface>,
    >,
    mut meshes: ResMut<Assets<Mesh>>,
    mut folded: Local<HashMap<Entity, usize>>,
) {
    let taken = places.taken();
    folded.retain(|e, _| surfaces.contains(*e));
    if taken.is_empty() {
        folded.clear();
        return;
    }
    for (e, mesh, tf, cell) in &surfaces {
        if folded.get(&e) == Some(&taken.len()) {
            continue;
        }
        folded.insert(e, taken.len());
        let Some(mut m) = meshes.get_mut(&mesh.0) else {
            continue;
        };
        let base = Vec2::new(
            cell.x as f32 * WORLD_CELL_EDGE + tf.translation.x,
            cell.z as f32 * WORLD_CELL_EDGE + tf.translation.z,
        );
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(positions)) =
            m.attribute_mut(Mesh::ATTRIBUTE_POSITION)
        else {
            continue;
        };
        for p in positions.iter_mut() {
            let w = base + Vec2::new(p[0], p[2]);
            if let Some(t) = taken.iter().find(|t| t.distance_squared(w) < 2.6 * 2.6) {
                let local = *t - base;
                *p = [local.x, p[1].min(0.0), local.y];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wall along x = `at`, refusing any move that crosses it.
    struct Wall {
        at: f32,
    }

    impl Obstacles for Wall {
        fn refuses(&self, from: Vec2, to: Vec2) -> Option<Vec2> {
            (from.x < self.at && to.x >= self.at).then_some(Vec2::new(-1.0, 0.0))
        }
    }

    struct Open;

    impl Obstacles for Open {
        fn refuses(&self, _: Vec2, _: Vec2) -> Option<Vec2> {
            None
        }
    }

    #[test]
    fn the_outline_is_the_cars_own_shape_turned_with_it() {
        let p = CarParams::hatchback();
        // Facing +x (yaw π/2), the nose is at +x and the left side at −z.
        let nose = to_world(
            Vec2::ZERO,
            std::f32::consts::FRAC_PI_2,
            Vec2::new(0.0, p.length * 0.5),
        );
        assert!((nose.x - p.length * 0.5).abs() < 1e-4 && nose.y.abs() < 1e-4);
        let left = to_world(Vec2::ZERO, std::f32::consts::FRAC_PI_2, Vec2::new(1.0, 0.0));
        assert!(left.x.abs() < 1e-4 && (left.y + 1.0).abs() < 1e-4);
    }

    #[test]
    fn an_open_road_lets_the_car_go_where_it_was_going() {
        let p = CarParams::hatchback();
        let mut s = CarState::parked(0.0, &p);
        s.forward = 10.0;
        let to = collide(&Open, Vec2::ZERO, &p, &mut s, 0.1);
        assert!((to.y - 1.0).abs() < 1e-4 && to.x.abs() < 1e-4);
        assert_eq!(s.forward, 10.0);
    }

    #[test]
    fn a_wall_stops_a_car_driven_at_it_and_lets_it_scrape_along() {
        let p = CarParams::hatchback();
        // Facing +x, 0.1 m short of a wall at x = length/2 + 0.1.
        let mut s = CarState::parked(std::f32::consts::FRAC_PI_2, &p);
        s.forward = 15.0;
        let wall = Wall {
            at: p.length * 0.5 + 0.1,
        };
        let to = collide(&wall, Vec2::ZERO, &p, &mut s, 1.0 / 30.0);
        assert!(
            to.x + p.length * 0.5 < wall.at,
            "through the wall to {to:?}"
        );
        let v = s.world_velocity();
        assert!(v[0] <= 0.0, "still driving into it at {v:?}");
        assert!(v[0] > -15.0 * 0.5, "bounced off like a ball at {v:?}");

        // At an angle, the car keeps most of its speed along the wall.
        let mut s = CarState::parked(std::f32::consts::FRAC_PI_4, &p);
        s.forward = 15.0;
        let _ = collide(&wall, Vec2::new(0.0, 0.0), &p, &mut s, 1.0 / 30.0);
        let v = s.world_velocity();
        assert!(v[1] > 7.0, "lost the speed along the wall: {v:?}");
        assert!(s.yaw_rate != 0.0, "a corner caught does not turn the car");
    }

    #[test]
    fn the_driver_sits_on_the_left_front_seat() {
        // Facing +z, the left is +x.
        let seat = driver_seat_offset(0.0);
        assert!(seat.x > 0.2 && seat.y < 0.0);
        // Facing +x, the left is −z.
        let seat = driver_seat_offset(std::f32::consts::FRAC_PI_2);
        assert!(seat.y < -0.2);
    }

    #[test]
    fn a_parked_car_keeps_its_colour() {
        let parked = crate::systems::world_places::ParkedCar {
            pos: Vec2::ZERO,
            yaw: 0.0,
            body: CarBody::Van,
            paint: 0x12_34_56,
        };
        let spot = CarSpot::parked(&parked);
        assert_eq!(spot.paint, [0x12, 0x34, 0x56]);
        assert_eq!(spot.kind, CarKind::Van);
        assert!(spot.from_lot);
    }
}
