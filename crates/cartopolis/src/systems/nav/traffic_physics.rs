//! The ambient traffic's bodies in Rapier: what each car, rider and walker
//! sees ahead of it, and what happens when two of them meet anyway.
//!
//! Every agent of `systems::traffic` has a rigid body here — a box for a car
//! or a bike, a capsule for a person — and so does every avatar. While an
//! agent drives its body is **kinematic**: the traffic's own steering puts it
//! where it is each frame, and Rapier only answers questions about it: a
//! shape cast along its way (`look_ahead`) is how it finds whoever it would
//! run into, of any kind, on any line. When two bodies touch at more than a
//! walking closing speed, both are made **dynamic** with the velocity they
//! had (`knock`) and Rapier resolves the collision — they shove each other
//! apart by their masses and slide to a stop — before the traffic takes them
//! back (`settle`).
//!
//! The world is in metres from an origin near the camera, with the client's
//! axes (x east, y up, z south) and no gravity: the bodies stand at the
//! heights the traffic puts them at (so a car on a bridge does not meet a
//! walker underneath) and a dynamic one is held to its height and upright.
//! Nothing of the map is in it — buildings, terrain and furniture do not
//! collide — for the reason the simulator's prims do not collide with them:
//! this is about the traffic meeting itself (`docs/notes/ambient-traffic.md`).
//!
//! The traffic is invented on each client, so this is per viewer and nothing
//! of it goes on the wire. The simulator's own Rapier world
//! (`docs/notes/physical-prims.md`) moves prims and knows nothing of this.

use std::sync::Mutex;

use bevy::prelude::{Quat, Resource, Vec2, Vec3};
use rapier3d::math::{Pose, Rotation as RQuat, Vector as RVec};
use rapier3d::prelude::{
    ActiveCollisionTypes, ActiveEvents, Capsule, ColliderBuilder, ColliderSet, CollisionEvent,
    ContactPair, Cuboid, EventHandler, LockedAxes, PhysicsWorld, QueryFilter, RigidBodyBuilder,
    RigidBodyHandle, RigidBodySet, RigidBodyType, Shape,
};

use rapier3d::dynamics::{SoftBodySet, SoftBodyTearEvent};
use rapier3d::parry::query::details::ShapeCastOptions;

use super::traffic::AgentKind;

/// What a body is, for its shape, its mass and how it behaves knocked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyKind {
    Agent(AgentKind),
    Avatar,
}

impl BodyKind {
    /// Whether this is somebody on foot.
    #[must_use]
    pub fn on_foot(self) -> bool {
        matches!(self, Self::Avatar | Self::Agent(AgentKind::Walker))
    }

    /// Half the footprint across and along, and half the height, metres.
    /// A car is a little narrower than the drawn bodies (1.7–1.9 m), or two
    /// passing on a narrow street would graze every time; a person is a
    /// capsule of their shoulders.
    #[must_use]
    pub fn half(self) -> Vec3 {
        match self {
            Self::Agent(AgentKind::Car) => Vec3::new(0.82, 0.7, 2.2),
            Self::Agent(AgentKind::Cyclist | AgentKind::Fatbike) => Vec3::new(0.3, 0.6, 0.9),
            Self::Agent(AgentKind::Walker) => Vec3::new(0.28, 0.88, 0.28),
            Self::Avatar => Vec3::new(0.35, 0.9, 0.35),
        }
    }

    /// Mass, kilograms: what a collision shares the shove out by.
    #[must_use]
    pub fn mass(self) -> f32 {
        match self {
            Self::Agent(AgentKind::Car) => 1200.0,
            Self::Agent(AgentKind::Cyclist) => 95.0,
            Self::Agent(AgentKind::Fatbike) => 110.0,
            Self::Agent(AgentKind::Walker) | Self::Avatar => 75.0,
        }
    }

    fn shape(self) -> Box<dyn Shape> {
        let h = self.half();
        if self.on_foot() {
            Box::new(Capsule::new_y(h.y - h.x, h.x))
        } else {
            Box::new(Cuboid::new(RVec::new(h.x, h.y, h.z)))
        }
    }
}

/// The marker on an avatar body's user data; an agent's is its entity bits.
const AVATAR_TAG: u128 = 1 << 127;

/// How far the origin may fall behind the camera before the frame is moved,
/// metres: `f32` keeps millimetres well beyond it.
const REORIGIN_M: f32 = 4_000.0;

/// The furthest an agent moves in one frame by driving, metres; further is a
/// teleport (`place`). A car at 30 m/s covers 3 m in a tenth of a second.
const TELEPORT_M: f32 = 3.0;

/// How fast a knocked body loses its speed, per second: a car skids to a
/// stop in a couple of seconds, a person stumbles to one sooner.
const VEHICLE_DAMPING: f32 = 1.4;
const PERSON_DAMPING: f32 = 3.5;

/// Two bodies that began touching, by their user data, with the contact
/// normal on the ground (from the first toward the second, or zero).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Touch {
    pub a: u128,
    pub b: u128,
    pub normal: Vec2,
}

/// The collision events of one step, gathered behind a lock because Rapier
/// hands them to a shared reference.
#[derive(Default)]
struct Touches(Mutex<Vec<Touch>>);

impl EventHandler for Touches {
    fn handle_collision_event(
        &self,
        _bodies: &RigidBodySet,
        colliders: &ColliderSet,
        event: CollisionEvent,
        contact_pair: Option<&ContactPair>,
    ) {
        let CollisionEvent::Started(c1, c2, _) = event else {
            return;
        };
        let (Some(a), Some(b)) = (colliders.get(c1), colliders.get(c2)) else {
            return;
        };
        let normal = contact_pair
            .and_then(|p| p.find_deepest_contact())
            .map_or(Vec2::ZERO, |(m, _)| {
                Vec2::new(m.data.normal.x, m.data.normal.z).normalize_or_zero()
            });
        if let Ok(mut t) = self.0.lock() {
            t.push(Touch {
                a: a.user_data,
                b: b.user_data,
                normal,
            });
        }
    }

    fn handle_contact_force_event(
        &self,
        _dt: f32,
        _bodies: &RigidBodySet,
        _colliders: &ColliderSet,
        _contact_pair: &ContactPair,
        _total_force_magnitude: f32,
    ) {
    }

    fn handle_soft_body_tear_event(&self, _soft_bodies: &SoftBodySet, _event: &SoftBodyTearEvent) {}
}

/// The traffic's Rapier world. See the module note.
#[derive(Resource)]
pub struct TrafficPhysics {
    world: PhysicsWorld,
    /// World XZ of the frame's origin; set by the first body placed.
    origin: Option<Vec2>,
    /// Every agent body, and whether it was placed since the last step:
    /// one that was not belongs to a hidden agent and is switched off.
    agents: Vec<(RigidBodyHandle, bool)>,
    avatars: Vec<RigidBodyHandle>,
    touches: Touches,
}

impl Default for TrafficPhysics {
    fn default() -> Self {
        let mut world = PhysicsWorld::new();
        world.gravity = RVec::ZERO;
        Self {
            world,
            origin: None,
            agents: Vec::new(),
            avatars: Vec::new(),
            touches: Touches::default(),
        }
    }
}

fn rv(v: Vec3) -> RVec {
    RVec::new(v.x, v.y, v.z)
}

fn rq(q: Quat) -> RQuat {
    RQuat::from_xyzw(q.x, q.y, q.z, q.w)
}

/// The rotation that points a body's `+z` (a car's nose) along `facing`.
fn yaw(facing: Vec2) -> Quat {
    Quat::from_rotation_y(facing.x.atan2(facing.y))
}

impl TrafficPhysics {
    /// A new body for `kind`, kinematic and switched off until placed, its
    /// user data `tag`.
    fn insert(&mut self, kind: BodyKind, tag: u128) -> RigidBodyHandle {
        let h = kind.half();
        let body = RigidBodyBuilder::kinematic_position_based()
            .enabled(false)
            .user_data(tag)
            .build();
        let handle = self.world.insert_body(body);
        let collider = if kind.on_foot() {
            ColliderBuilder::capsule_y(h.y - h.x, h.x)
        } else {
            ColliderBuilder::cuboid(h.x, h.y, h.z)
        }
        // Half its height up, so the body stands on the point it is put at.
        .translation(RVec::new(0.0, h.y, 0.0))
        .mass(kind.mass())
        .friction(0.6)
        .restitution(0.15)
        .user_data(tag)
        .active_events(ActiveEvents::COLLISION_EVENTS)
        .active_collision_types(ActiveCollisionTypes::all())
        .build();
        self.world.insert_collider(collider, Some(handle));
        handle
    }

    /// A body for the agent `entity_bits` of `kind`.
    pub fn agent_body(&mut self, entity_bits: u64, kind: AgentKind) -> RigidBodyHandle {
        let h = self.insert(BodyKind::Agent(kind), u128::from(entity_bits));
        self.agents.push((h, false));
        h
    }

    /// Whether `tag` (a body's user data) is an avatar's.
    #[must_use]
    pub fn is_avatar(tag: u128) -> bool {
        tag & AVATAR_TAG != 0
    }

    fn local(&mut self, at: Vec3) -> Vec3 {
        let origin = *self.origin.get_or_insert(Vec2::new(at.x, at.z));
        if Vec2::new(at.x, at.z).distance(origin) > REORIGIN_M {
            self.reorigin(Vec2::new(at.x, at.z));
            return self.local(at);
        }
        at - Vec3::new(origin.x, 0.0, origin.y)
    }

    /// Move the frame's origin to `to`, and every body with it.
    fn reorigin(&mut self, to: Vec2) {
        let Some(from) = self.origin else {
            self.origin = Some(to);
            return;
        };
        let shift = rv(Vec3::new(from.x - to.x, 0.0, from.y - to.y));
        for (_, rb) in self.world.bodies.iter_mut() {
            let mut p = *rb.position();
            p.translation += shift;
            rb.set_position(p, false);
        }
        self.origin = Some(to);
    }

    /// Put an agent's kinematic body where the traffic drew it this frame:
    /// `at` its world point on the ground, `facing` its heading on it.
    pub fn place(&mut self, body: RigidBodyHandle, at: Vec3, facing: Vec2) {
        let local = self.local(at);
        if let Some(e) = self.agents.iter_mut().find(|e| e.0 == body) {
            e.1 = true;
        }
        let Some(rb) = self.world.bodies.get_mut(body) else {
            return;
        };
        let pose = Pose::from_parts(rv(local), rq(yaw(facing)));
        let jumped = (rb.translation() - rv(local)).length() > TELEPORT_M;
        if !rb.is_enabled() || jumped {
            // Put there, not driven there: a kinematic body moved to its
            // next pose sweeps everything between, and an agent recycled
            // across the ring would collide with half the town on the way.
            rb.set_enabled(true);
            rb.set_position(pose, true);
            rb.set_linvel(RVec::ZERO, false);
        } else if rb.is_kinematic() {
            rb.set_next_kinematic_position(pose);
        }
    }

    /// Keep a knocked agent's body on through the next step: the traffic
    /// does not place it while Rapier moves it.
    pub fn keep(&mut self, body: RigidBodyHandle) {
        if let Some(e) = self.agents.iter_mut().find(|e| e.0 == body) {
            e.1 = true;
        }
    }

    /// Where a body is: world XZ, its heading, and its speed on the ground.
    #[must_use]
    pub fn pose(&self, body: RigidBodyHandle) -> Option<(Vec2, Vec2, f32)> {
        let rb = self.world.bodies.get(body)?;
        let origin = self.origin?;
        let t = rb.translation();
        let r = rb.rotation();
        let nose = Quat::from_xyzw(r.x, r.y, r.z, r.w) * Vec3::Z;
        let v = rb.linvel();
        Some((
            Vec2::new(t.x, t.z) + origin,
            Vec2::new(nose.x, nose.z).normalize_or(Vec2::Y),
            Vec2::new(v.x, v.z).length(),
        ))
    }

    /// A body's height, world metres: where it was last put.
    #[must_use]
    pub fn height(&self, body: RigidBodyHandle) -> Option<f32> {
        self.origin?;
        let rb = self.world.bodies.get(body)?;
        rb.is_enabled().then(|| rb.translation().y)
    }

    /// Knock an agent's body loose: dynamic, moving at `velocity` on the
    /// ground, held to its height and upright — a person turned by nothing,
    /// a vehicle free to slew round — and slowed by friction with the road.
    pub fn knock(&mut self, body: RigidBodyHandle, kind: BodyKind, velocity: Vec2) {
        let Some(rb) = self.world.bodies.get_mut(body) else {
            return;
        };
        rb.set_body_type(RigidBodyType::Dynamic, true);
        let locked = if kind.on_foot() {
            LockedAxes::TRANSLATION_LOCKED_Y | LockedAxes::ROTATION_LOCKED
        } else {
            LockedAxes::TRANSLATION_LOCKED_Y
                | LockedAxes::ROTATION_LOCKED_X
                | LockedAxes::ROTATION_LOCKED_Z
        };
        rb.set_locked_axes(locked, true);
        rb.set_linear_damping(if kind.on_foot() {
            PERSON_DAMPING
        } else {
            VEHICLE_DAMPING
        });
        rb.set_angular_damping(2.0);
        rb.set_linvel(RVec::new(velocity.x, 0.0, velocity.y), true);
        rb.set_angvel(RVec::ZERO, true);
    }

    /// Hand a knocked body back to the traffic: kinematic again, still.
    pub fn settle(&mut self, body: RigidBodyHandle) {
        if let Some(rb) = self.world.bodies.get_mut(body) {
            rb.set_body_type(RigidBodyType::KinematicPositionBased, true);
            rb.set_linvel(RVec::ZERO, false);
            rb.set_angvel(RVec::ZERO, false);
        }
    }

    /// Stand a body at each avatar: `at` their world points on the ground.
    /// They push and are never pushed — where an avatar is, the server says.
    pub fn set_avatars(&mut self, at: &[Vec3]) {
        while self.avatars.len() < at.len() {
            let tag = AVATAR_TAG | self.avatars.len() as u128;
            let h = self.insert(BodyKind::Avatar, tag);
            self.avatars.push(h);
        }
        for i in 0..self.avatars.len() {
            let h = self.avatars[i];
            match at.get(i) {
                Some(&p) => {
                    let local = self.local(p);
                    if let Some(rb) = self.world.bodies.get_mut(h) {
                        let pose = Pose::from_parts(rv(local), RQuat::IDENTITY);
                        if rb.is_enabled() {
                            rb.set_next_kinematic_position(pose);
                        } else {
                            rb.set_enabled(true);
                            rb.set_position(pose, true);
                        }
                    }
                }
                None => {
                    if let Some(rb) = self.world.bodies.get_mut(h) {
                        rb.set_enabled(false);
                    }
                }
            }
        }
    }

    /// The first body an agent's footprint would run into going `dist`
    /// metres from `at` along `travel`: how far it can go before touching,
    /// and the other's user data. Its own body is left out.
    #[must_use]
    pub fn look_ahead(
        &self,
        body: RigidBodyHandle,
        kind: AgentKind,
        at: Vec3,
        travel: Vec2,
        dist: f32,
    ) -> Option<(f32, u128)> {
        let origin = self.origin?;
        let local = at - Vec3::new(origin.x, 0.0, origin.y);
        let me = BodyKind::Agent(kind);
        let pose = Pose::from_parts(rv(local + Vec3::Y * me.half().y), rq(yaw(travel)));
        let shape = me.shape();
        let options = ShapeCastOptions {
            max_time_of_impact: dist,
            target_distance: 0.0,
            stop_at_penetration: false,
            compute_impact_geometry_on_penetration: false,
        };
        let filter = QueryFilter::default().exclude_rigid_body(body);
        let (collider, hit) = self.world.cast_shape(
            &pose,
            RVec::new(travel.x, 0.0, travel.y),
            shape.as_ref(),
            options,
            filter,
        )?;
        let tag = self.world.colliders.get(collider)?.user_data;
        Some((hit.time_of_impact, tag))
    }

    /// Advance `dt` seconds: knocked bodies move, kinematic ones reach where
    /// they were placed, and every agent body nobody placed since the last
    /// step is switched off. The pairs that began touching.
    pub fn step(&mut self, dt: f32) -> Vec<Touch> {
        for (h, placed) in &mut self.agents {
            if !*placed {
                if let Some(rb) = self.world.bodies.get_mut(*h) {
                    if rb.is_enabled() {
                        rb.set_enabled(false);
                    }
                }
            }
            *placed = false;
        }
        if dt <= 0.0 {
            return Vec::new();
        }
        self.world.integration_parameters.dt = dt.min(1.0 / 20.0);
        self.world.step_with_events(&(), &self.touches);
        self.touches
            .0
            .lock()
            .map(|mut t| std::mem::take(&mut *t))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, z: f32) -> Vec3 {
        Vec3::new(x, 0.0, z)
    }

    /// A car sees the car crossing in front of it, and how far it has to go
    /// before it would touch; not one beside its path, and not itself.
    #[test]
    fn a_car_sees_what_it_would_run_into() {
        let mut p = TrafficPhysics::default();
        let me = p.agent_body(1, AgentKind::Car);
        let crossing = p.agent_body(2, AgentKind::Car);
        let beside = p.agent_body(3, AgentKind::Car);
        p.place(me, at(0.0, 0.0), Vec2::Y);
        p.place(crossing, at(0.0, 20.0), Vec2::X);
        p.place(beside, at(4.0, 8.0), Vec2::Y);
        p.step(1.0 / 60.0);
        let (gap, tag) = p
            .look_ahead(me, AgentKind::Car, at(0.0, 0.0), Vec2::Y, 30.0)
            .expect("the car across its way");
        assert_eq!(tag, 2);
        // Nose at 2.2, the other's side at 20 − 0.82.
        assert!((gap - (20.0 - 0.82 - 2.2)).abs() < 0.05, "{gap}");
        assert!(p
            .look_ahead(me, AgentKind::Car, at(0.0, 0.0), Vec2::Y, 10.0)
            .is_none());
    }

    /// Two cars driven into each other touch; knocked loose with their
    /// speeds, the heavy one shoves the light one, and both come to rest.
    #[test]
    fn a_collision_shoves_by_mass_and_comes_to_rest() {
        let mut p = TrafficPhysics::default();
        let car = p.agent_body(1, AgentKind::Car);
        let bike = p.agent_body(2, AgentKind::Cyclist);
        let (mut zc, zb) = (0.0f32, 4.0f32);
        p.place(car, at(0.0, zc), Vec2::Y);
        p.place(bike, at(0.0, zb), Vec2::X);
        p.step(1.0 / 60.0);
        let mut touched = None;
        for _ in 0..120 {
            zc += 8.0 / 60.0;
            p.place(car, at(0.0, zc), Vec2::Y);
            p.place(bike, at(0.0, zb), Vec2::X);
            let t = p.step(1.0 / 60.0);
            if let Some(t) = t.first() {
                touched = Some(*t);
                break;
            }
        }
        let t = touched.expect("they touch");
        assert!([t.a, t.b].contains(&1) && [t.a, t.b].contains(&2));
        assert!(t.normal.y.abs() > 0.9, "head on, along z: {:?}", t.normal);
        p.knock(car, BodyKind::Agent(AgentKind::Car), Vec2::new(0.0, 8.0));
        p.knock(bike, BodyKind::Agent(AgentKind::Cyclist), Vec2::ZERO);
        for _ in 0..30 {
            p.keep(car);
            p.keep(bike);
            p.step(1.0 / 60.0);
        }
        let (_, _, bike_speed) = p.pose(bike).unwrap();
        assert!(bike_speed > 3.0, "the bike is shoved: {bike_speed}");
        for _ in 0..600 {
            p.keep(car);
            p.keep(bike);
            p.step(1.0 / 60.0);
        }
        assert!(p.pose(car).unwrap().2 < 0.1 && p.pose(bike).unwrap().2 < 0.1);
        // Upright and on the ground still.
        let rb = p.world.bodies.get(bike).unwrap();
        assert!(rb.translation().y.abs() < 1e-3);
        p.settle(bike);
        assert!(!p.world.bodies.get(bike).unwrap().is_dynamic());
    }

    /// A body nobody placed is switched off and is in nobody's way.
    #[test]
    fn a_hidden_agent_is_in_nobodys_way() {
        let mut p = TrafficPhysics::default();
        let me = p.agent_body(1, AgentKind::Car);
        let gone = p.agent_body(2, AgentKind::Car);
        p.place(me, at(0.0, 0.0), Vec2::Y);
        p.place(gone, at(0.0, 10.0), Vec2::Y);
        p.step(1.0 / 60.0);
        p.place(me, at(0.0, 0.0), Vec2::Y);
        p.step(1.0 / 60.0);
        assert!(p
            .look_ahead(me, AgentKind::Car, at(0.0, 0.0), Vec2::Y, 20.0)
            .is_none());
    }

    /// An avatar is a body the traffic sees, tagged as one.
    #[test]
    fn an_avatar_is_in_the_way() {
        let mut p = TrafficPhysics::default();
        let me = p.agent_body(1, AgentKind::Cyclist);
        p.place(me, at(0.0, 0.0), Vec2::Y);
        p.set_avatars(&[at(0.2, 6.0)]);
        p.step(1.0 / 60.0);
        let (_, tag) = p
            .look_ahead(me, AgentKind::Cyclist, at(0.0, 0.0), Vec2::Y, 10.0)
            .expect("the avatar");
        assert!(TrafficPhysics::is_avatar(tag));
    }

    /// The frame follows a camera that travels far, bodies and all.
    #[test]
    fn the_frame_moves_with_the_camera() {
        let mut p = TrafficPhysics::default();
        let a = p.agent_body(1, AgentKind::Car);
        p.place(a, at(0.0, 0.0), Vec2::Y);
        p.step(1.0 / 60.0);
        let b = p.agent_body(2, AgentKind::Car);
        p.place(a, at(9_000.0, 0.0), Vec2::Y);
        p.place(b, at(9_000.0, 10.0), Vec2::Y);
        p.step(1.0 / 60.0);
        let (pos, ..) = p.pose(a).unwrap();
        assert!((pos - Vec2::new(9_000.0, 0.0)).length() < 0.01, "{pos}");
        assert!(p
            .look_ahead(a, AgentKind::Car, at(9_000.0, 0.0), Vec2::Y, 20.0)
            .is_some());
    }
}
