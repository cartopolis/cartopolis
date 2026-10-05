//! Physical prims: the rigid-body step (`docs/notes/physical-prims.md`).
//!
//! A prim with [`PrimDto::physical`] set is moved by this module instead of
//! standing where it was put — it falls, bounces off other prims, rolls, and is
//! pushed by avatars walking into it. Second Life's *Physical* checkbox, and the
//! reason it exists is a football that a pitch full of people can kick.
//!
//! # What collides with what
//!
//! Physical prims collide with each other, with every non-physical prim near
//! them, with the ground and with avatars. **Not with buildings, terrain or
//! anything else on the map**, and that is a decision rather than a gap: the
//! simulator holds no map data, and a world built out of prims (a pitch, its
//! goals, a ramp) is what this is for. The ground is the flat plane at
//! [`motion::GROUND_Y`] the sim already stands every avatar on.
//!
//! # Frames
//!
//! Prims are placed by a geo anchor and an altitude (ADR-029), so there is no
//! sim-wide metre frame to simulate in, and Rapier works in `f32`. The world is
//! cut into **zones** of [`ZONE_DEG`] (about a kilometre), each a Rapier world of
//! its own in metres from the zone's centre, with the client's axes (x east,
//! y up, z south) so velocities and an avatar's yaw mean the same thing on both
//! sides. A body that rolls out of its zone moves to the next one, velocity and
//! all. Zones exist only while a physical prim is in them.
//!
//! # Who decides
//!
//! The server, alone. Each tick the bodies that moved are written back into the
//! prims (so a script's `get_pos` and the periodic flush see where they are) and
//! sent to clients as one [`NetMessage::PrimMotion`]. An edit or a script that
//! moves a physical prim is noticed by comparing the prim with what this module
//! last wrote into it, and teleports the body — after an edit it is held still
//! for [`HOLD_AFTER_EDIT_S`] so a builder can drag a ball up and let go.
//!
//! [`NetMessage::PrimMotion`]: cartopolis_core::NetMessage::PrimMotion

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use cartopolis_core::{motion, GeoAnchor, PrimBody, PrimDto};
use glam::{EulerRot, Quat, Vec3};
use rapier3d::math::{Pose, Rotation as RQuat, Vector as RVec};
use rapier3d::prelude::{
    ColliderBuilder, ColliderHandle, PhysicsWorld, RigidBodyBuilder, RigidBodyHandle, RigidBodyType,
};

/// Edge of a zone, in degrees of latitude and of longitude. 0.01° is 1.1 km of
/// latitude and ~670 m of longitude at Groningen — far beyond any pitch, and
/// small enough that `f32` keeps millimetres at its edge.
pub const ZONE_DEG: f64 = 0.01;
/// How far past its own edge a zone looks for the static prims and avatars a
/// body might touch. Prims are at most 64 m on a side, so 0.002° (~130 m of
/// longitude) cannot miss one that overlaps the zone.
const ZONE_MARGIN_DEG: f64 = 0.002;

/// The rigid-body step rate. The tick loop runs at 20 Hz; a ball at shot speed
/// covers 0.75 m in a tick, which is more than a goal post is wide, so the step
/// runs three times inside one.
pub const STEP_HZ: f32 = 60.0;
/// Most steps one tick may run. A stalled tick catches up this far and then
/// drops the rest of the time, rather than freezing the loop to replay it.
const MAX_STEPS_PER_TICK: u32 = 6;

/// Most physical prims the world may hold at once. A bound on the step's cost
/// that a builder cannot talk past: refused at the point a prim is switched on.
pub const MAX_PHYSICAL_PRIMS: usize = 64;

/// Kilograms per cubic metre. A football (22 cm across) comes out at 0.56 kg,
/// near the real 0.43; a one-metre crate at 100 kg, which a kick barely moves.
pub const DENSITY_KG_M3: f32 = 100.0;

/// Fastest a script may send a prim (`set_vel`), in metres per second.
pub const MAX_SCRIPT_SPEED: f32 = 60.0;
/// Largest single shove a script may give, in kilogram-metres per second.
pub const MAX_SCRIPT_IMPULSE: f32 = 5_000.0;

/// An avatar's body, for pushing: a capsule 1.8 m tall standing on its feet.
const AVATAR_RADIUS: f32 = 0.3;
const AVATAR_HALF_HEIGHT: f32 = 0.6;
/// An avatar that moved further than this in one tick was placed, not walked:
/// its capsule is moved with it rather than swept through everything between.
const AVATAR_TELEPORT_M: f32 = 3.0;

/// How far in front of an avatar's feet a kick reaches, and how high.
const KICK_REACH_M: f32 = 1.5;
const KICK_HEIGHT_M: f32 = 1.2;
/// What a kick does to a light object: this much speed along the avatar's
/// facing, and a little lift so it leaves the ground.
const KICK_SPEED: f32 = 14.0;
const KICK_LIFT: f32 = 3.0;
/// Above this mass a kick moves things proportionally less — the same boot, a
/// heavier object.
const KICK_MASS_REF_KG: f32 = 2.0;

/// How long a physical prim stays where an edit put it before it falls again.
pub const HOLD_AFTER_EDIT_S: f32 = 1.0;

/// Somebody kicked. Taken from an intent carrying `Emote::Kick`.
#[derive(Debug, Clone, Copy)]
pub struct Kick {
    pub geo: GeoAnchor,
    pub feet_y: f32,
    /// The avatar's facing, as `ClientIntent::display_yaw`: 0 faces +z (south).
    pub yaw: f32,
}

/// An avatar this step should push things with.
#[derive(Debug, Clone, Copy)]
pub struct AvatarProbe {
    pub id: u64,
    pub geo: GeoAnchor,
    pub feet_y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ZoneKey(i32, i32);

impl ZoneKey {
    fn of(g: GeoAnchor) -> Self {
        Self(
            (g.latitude / ZONE_DEG).floor() as i32,
            (g.longitude / ZONE_DEG).floor() as i32,
        )
    }

    fn origin(self) -> GeoAnchor {
        GeoAnchor {
            latitude: (f64::from(self.0) + 0.5) * ZONE_DEG,
            longitude: (f64::from(self.1) + 0.5) * ZONE_DEG,
        }
    }

    /// Whether `g` is in this zone or within [`ZONE_MARGIN_DEG`] of it.
    fn reaches(self, g: GeoAnchor) -> bool {
        let o = self.origin();
        let half = ZONE_DEG * 0.5 + ZONE_MARGIN_DEG;
        (g.latitude - o.latitude).abs() <= half && (g.longitude - o.longitude).abs() <= half
    }
}

/// A zone's metre frame: x east, y up, z south, from the zone's centre.
#[derive(Debug, Clone, Copy)]
struct Frame {
    origin: GeoAnchor,
    m_per_lat: f64,
    m_per_lng: f64,
}

impl Frame {
    fn new(origin: GeoAnchor) -> Self {
        let (m_per_lat, m_per_lng) = cartopolis_core::degree_scale_m(origin.latitude);
        Self {
            origin,
            m_per_lat,
            m_per_lng,
        }
    }

    fn to_local(&self, g: GeoAnchor, y: f32) -> Vec3 {
        Vec3::new(
            ((g.longitude - self.origin.longitude) * self.m_per_lng) as f32,
            y,
            (-(g.latitude - self.origin.latitude) * self.m_per_lat) as f32,
        )
    }

    fn to_geo(&self, p: Vec3) -> (GeoAnchor, f32) {
        (
            GeoAnchor {
                latitude: self.origin.latitude - f64::from(p.z) / self.m_per_lat,
                longitude: self.origin.longitude + f64::from(p.x) / self.m_per_lng,
            },
            p.y,
        )
    }
}

fn rv(v: Vec3) -> RVec {
    RVec::new(v.x, v.y, v.z)
}

fn gv(v: RVec) -> Vec3 {
    Vec3::new(v.x, v.y, v.z)
}

fn rq(q: Quat) -> RQuat {
    RQuat::from_xyzw(q.x, q.y, q.z, q.w)
}

fn gq(q: RQuat) -> Quat {
    Quat::from_xyzw(q.x, q.y, q.z, q.w)
}

/// A prim's orientation as the client builds it (`transform_from_dto`).
fn prim_quat(p: &PrimDto) -> Quat {
    Quat::from_euler(EulerRot::XYZ, p.rotation.x, p.rotation.y, p.rotation.z)
}

/// What a prim's collision shape depends on. A change rebuilds the collider.
#[derive(Debug, Clone, PartialEq)]
struct ShapeKey {
    shape: String,
    scale: [u32; 3],
    model: bool,
}

impl ShapeKey {
    fn of(p: &PrimDto) -> Self {
        Self {
            shape: p.shape.to_ascii_lowercase(),
            scale: [
                p.scale.x.to_bits(),
                p.scale.y.to_bits(),
                p.scale.z.to_bits(),
            ],
            model: p.model.is_some(),
        }
    }
}

/// Where this module last put a prim, so an edit can be told from its own
/// writes. Compared bit for bit: these are the exact values it wrote.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placed {
    geo: GeoAnchor,
    y: f32,
    rotation: Vec3,
}

impl Placed {
    fn of(p: &PrimDto) -> Self {
        Self {
            geo: p.geo,
            y: p.position.y,
            rotation: p.rotation,
        }
    }
}

fn size(p: &PrimDto) -> Vec3 {
    p.scale.abs().max(Vec3::splat(0.01))
}

/// The collision shape for a prim, at its scale, before it is placed.
///
/// Procedural shapes keep their kind; the deformations (hollow, path cut,
/// twist, taper) are not modelled, and a torus or a model collides as its box.
fn collider_for(p: &PrimDto) -> ColliderBuilder {
    let s = size(p);
    let h = s * 0.5;
    if p.model.is_some() {
        return ColliderBuilder::cuboid(h.x, h.y, h.z);
    }
    match p.shape.to_ascii_lowercase().as_str() {
        "sphere" => {
            let (lo, hi) = (h.min_element(), h.max_element());
            if hi - lo <= hi * 0.05 {
                ColliderBuilder::ball((h.x + h.y + h.z) / 3.0)
            } else {
                // An ellipsoid: the hull of points on it is close enough for
                // something that is going to roll.
                let mut pts = Vec::with_capacity(8 * 16 + 2);
                pts.push(RVec::new(0.0, h.y, 0.0));
                pts.push(RVec::new(0.0, -h.y, 0.0));
                for i in 1..8 {
                    let lat = std::f32::consts::PI * (i as f32 / 8.0 - 0.5);
                    for j in 0..16 {
                        let lng = std::f32::consts::TAU * j as f32 / 16.0;
                        pts.push(RVec::new(
                            h.x * lat.cos() * lng.cos(),
                            h.y * lat.sin(),
                            h.z * lat.cos() * lng.sin(),
                        ));
                    }
                }
                ColliderBuilder::convex_hull(&pts)
                    .unwrap_or_else(|| ColliderBuilder::cuboid(h.x, h.y, h.z))
            }
        }
        "cylinder" => ColliderBuilder::cylinder(h.y, (h.x + h.z) * 0.5),
        "cone" => ColliderBuilder::cone(h.y, (h.x + h.z) * 0.5),
        _ => ColliderBuilder::cuboid(h.x, h.y, h.z),
    }
}

/// What a prim weighs at [`DENSITY_KG_M3`], from the same shape it collides as.
#[must_use]
pub fn mass_of(p: &PrimDto) -> f32 {
    let s = size(p);
    let box_volume = s.x * s.y * s.z;
    let volume = if p.model.is_some() {
        box_volume
    } else {
        match p.shape.to_ascii_lowercase().as_str() {
            "sphere" => box_volume * std::f32::consts::PI / 6.0,
            "cylinder" => box_volume * std::f32::consts::PI / 4.0,
            "cone" => box_volume * std::f32::consts::PI / 12.0,
            _ => box_volume,
        }
    };
    volume * DENSITY_KG_M3
}

/// Bounciness and grip. A ball bounces; a crate lands.
fn material(c: ColliderBuilder, p: &PrimDto) -> ColliderBuilder {
    let bouncy = p.model.is_none() && p.shape.eq_ignore_ascii_case("sphere");
    c.density(DENSITY_KG_M3)
        .friction(0.6)
        .restitution(if bouncy { 0.6 } else { 0.15 })
}

struct Body {
    handle: RigidBodyHandle,
    collider: ColliderHandle,
    zone: ZoneKey,
    shape: ShapeKey,
    placed: Placed,
    /// Seconds left of the post-edit hold; the body is kinematic while > 0.
    held: f32,
    /// The last pose sent said it was resting, so nothing more is sent until it
    /// moves.
    resting_sent: bool,
}

struct AvatarBody {
    handle: RigidBodyHandle,
    from: Vec3,
    to: Vec3,
}

struct Zone {
    frame: Frame,
    world: PhysicsWorld,
    statics: Vec<ColliderHandle>,
    statics_hash: Option<u64>,
    avatars: HashMap<u64, AvatarBody>,
}

impl Zone {
    fn new(key: ZoneKey) -> Self {
        let mut world = PhysicsWorld::new();
        world.gravity = RVec::new(0.0, -9.81, 0.0);
        world.integration_parameters.dt = 1.0 / STEP_HZ;
        // The ground: a slab whose top is the sim's ground plane, wider than the
        // zone and its margin together.
        world.insert_collider(
            ColliderBuilder::cuboid(2_000.0, 1.0, 2_000.0)
                .translation(RVec::new(0.0, motion::GROUND_Y - 1.0, 0.0))
                .friction(0.8)
                .restitution(0.5),
            None,
        );
        Self {
            frame: Frame::new(key.origin()),
            world,
            statics: Vec::new(),
            statics_hash: None,
            avatars: HashMap::new(),
        }
    }
}

/// Every physics zone and every body in them. Lives inside `SimWorld`, under the
/// same lock, and is stepped from `SimWorld::step`.
#[derive(Default)]
pub struct Physics {
    zones: HashMap<ZoneKey, Zone>,
    bodies: HashMap<i64, Body>,
    kicks: Vec<Kick>,
    impulses: Vec<(i64, Vec3)>,
    velocities: Vec<(i64, Vec3)>,
    /// Simulated time not yet stepped, in seconds.
    clock: f32,
}

impl Physics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a kick for the next step.
    pub fn kick(&mut self, kick: Kick) {
        if !self.bodies.is_empty() {
            self.kicks.push(kick);
        }
    }

    /// Queue a shove (`apply_impulse`), kilogram-metres per second.
    pub fn apply_impulse(&mut self, prim_id: i64, impulse: Vec3) {
        if impulse.is_finite() {
            self.impulses
                .push((prim_id, impulse.clamp_length_max(MAX_SCRIPT_IMPULSE)));
        }
    }

    /// Queue a velocity (`set_vel`), metres per second.
    pub fn set_velocity(&mut self, prim_id: i64, velocity: Vec3) {
        if velocity.is_finite() {
            self.velocities
                .push((prim_id, velocity.clamp_length_max(MAX_SCRIPT_SPEED)));
        }
    }

    /// How fast a physical prim is moving, or zero.
    #[must_use]
    pub fn velocity(&self, prim_id: i64) -> Vec3 {
        self.bodies
            .get(&prim_id)
            .and_then(|b| {
                let zone = self.zones.get(&b.zone)?;
                zone.world.bodies.get(b.handle).map(|rb| gv(rb.linvel()))
            })
            .unwrap_or(Vec3::ZERO)
    }

    /// Bodies being simulated.
    #[must_use]
    pub fn body_count(&self) -> usize {
        self.bodies.len()
    }

    /// Advance by `dt` seconds: bring the bodies in line with `prims`, step, and
    /// write the result back into `prims`.
    ///
    /// `prims` must be sorted by id (`SimWorld` keeps it so). Returns the bodies
    /// that moved, which is the whole of what clients are told.
    pub fn step(
        &mut self,
        dt: f32,
        prims: &mut [PrimDto],
        avatars: &[AvatarProbe],
    ) -> Vec<PrimBody> {
        let any_physical = prims.iter().any(|p| p.physical);
        if !any_physical && self.bodies.is_empty() {
            self.zones.clear();
            self.kicks.clear();
            self.impulses.clear();
            self.velocities.clear();
            self.clock = 0.0;
            return Vec::new();
        }

        self.sync_bodies(prims, dt);
        self.sync_statics(prims);
        self.sync_avatars(avatars);
        self.apply_pushes(prims);

        self.clock += dt.max(0.0);
        let mut steps = (self.clock * STEP_HZ).floor() as u32;
        self.clock -= steps as f32 / STEP_HZ;
        if steps > MAX_STEPS_PER_TICK {
            steps = MAX_STEPS_PER_TICK;
            self.clock = 0.0;
        }
        if steps == 0 {
            return Vec::new();
        }
        for zone in self.zones.values_mut() {
            for k in 1..=steps {
                let t = k as f32 / steps as f32;
                for a in zone.avatars.values() {
                    if let Some(rb) = zone.world.bodies.get_mut(a.handle) {
                        rb.set_next_kinematic_translation(rv(a.from.lerp(a.to, t)));
                    }
                }
                zone.world.step();
            }
            for a in zone.avatars.values_mut() {
                a.from = a.to;
            }
        }
        self.read_back(prims)
    }

    fn sync_bodies(&mut self, prims: &[PrimDto], dt: f32) {
        let mut live: HashSet<i64> = HashSet::new();
        for p in prims.iter().filter(|p| p.physical) {
            live.insert(p.id);
            let key = ZoneKey::of(p.geo);
            self.zones.entry(key).or_insert_with(|| Zone::new(key));
            let existing = self
                .bodies
                .get(&p.id)
                .map(|b| (b.zone, b.shape.clone(), b.placed));
            match existing {
                None => {
                    let body = self.insert_body(key, p, Vec3::ZERO, Vec3::ZERO);
                    self.bodies.insert(p.id, body);
                }
                Some((zone, shape, placed)) => {
                    let edited = placed != Placed::of(p);
                    if zone != key || shape != ShapeKey::of(p) {
                        // Out of its zone or reshaped: a fresh body where the prim
                        // is, keeping the motion unless an edit put it there.
                        let (lin, ang) = if edited {
                            (Vec3::ZERO, Vec3::ZERO)
                        } else {
                            self.motion_of(p.id)
                        };
                        let held = self.bodies.get(&p.id).map_or(0.0, |b| b.held);
                        self.remove_body(p.id);
                        let mut body = self.insert_body(key, p, lin, ang);
                        if edited {
                            body.held = HOLD_AFTER_EDIT_S;
                        } else {
                            body.held = held;
                        }
                        self.bodies.insert(p.id, body);
                    } else if edited {
                        let b = self.bodies.get_mut(&p.id).expect("checked above");
                        let zone = self.zones.get_mut(&b.zone).expect("body's zone exists");
                        if let Some(rb) = zone.world.bodies.get_mut(b.handle) {
                            rb.set_body_type(RigidBodyType::KinematicPositionBased, true);
                            rb.set_translation(rv(zone.frame.to_local(p.geo, p.position.y)), true);
                            rb.set_rotation(rq(prim_quat(p)), true);
                            rb.set_linvel(RVec::ZERO, false);
                            rb.set_angvel(RVec::ZERO, false);
                        }
                        b.placed = Placed::of(p);
                        b.held = HOLD_AFTER_EDIT_S;
                        b.resting_sent = false;
                    }
                }
            }
        }
        let gone: Vec<i64> = self
            .bodies
            .keys()
            .filter(|id| !live.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.remove_body(id);
        }
        // Release holds that have run out.
        for b in self.bodies.values_mut() {
            if b.held > 0.0 {
                b.held -= dt;
                if b.held <= 0.0 {
                    b.held = 0.0;
                    if let Some(rb) = self
                        .zones
                        .get_mut(&b.zone)
                        .and_then(|z| z.world.bodies.get_mut(b.handle))
                    {
                        rb.set_body_type(RigidBodyType::Dynamic, true);
                    }
                }
            }
        }
        let used: HashSet<ZoneKey> = self.bodies.values().map(|b| b.zone).collect();
        self.zones.retain(|k, _| used.contains(k));
    }

    fn motion_of(&self, id: i64) -> (Vec3, Vec3) {
        self.bodies
            .get(&id)
            .and_then(|b| self.zones.get(&b.zone)?.world.bodies.get(b.handle))
            .map_or((Vec3::ZERO, Vec3::ZERO), |rb| {
                (gv(rb.linvel()), gv(rb.angvel()))
            })
    }

    fn insert_body(&mut self, key: ZoneKey, p: &PrimDto, lin: Vec3, ang: Vec3) -> Body {
        let zone = self
            .zones
            .get_mut(&key)
            .expect("zone created by the caller");
        let at = zone.frame.to_local(p.geo, p.position.y);
        let rb = RigidBodyBuilder::dynamic()
            .pose(Pose::from_parts(rv(at), rq(prim_quat(p))))
            .linvel(rv(lin))
            .angvel(rv(ang))
            .linear_damping(0.2)
            .angular_damping(0.8)
            .ccd_enabled(true)
            .user_data(p.id as u128);
        let (handle, collider) = zone.world.insert(rb, material(collider_for(p), p));
        Body {
            handle,
            collider,
            zone: key,
            shape: ShapeKey::of(p),
            placed: Placed::of(p),
            held: 0.0,
            resting_sent: false,
        }
    }

    fn remove_body(&mut self, id: i64) {
        if let Some(b) = self.bodies.remove(&id) {
            if let Some(zone) = self.zones.get_mut(&b.zone) {
                zone.world.remove_body(b.handle);
            }
            let _ = b.collider;
        }
    }

    /// The non-physical prims a zone's bodies can hit, rebuilt when any of them
    /// changes — detected by hashing what a collider depends on, so no edit path
    /// anywhere has to remember to tell this module.
    fn sync_statics(&mut self, prims: &[PrimDto]) {
        for (key, zone) in &mut self.zones {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            let near: Vec<&PrimDto> = prims
                .iter()
                .filter(|p| !p.physical && key.reaches(p.geo))
                .collect();
            for p in &near {
                p.id.hash(&mut hasher);
                p.geo.latitude.to_bits().hash(&mut hasher);
                p.geo.longitude.to_bits().hash(&mut hasher);
                p.position.y.to_bits().hash(&mut hasher);
                for v in [p.rotation, p.scale] {
                    v.x.to_bits().hash(&mut hasher);
                    v.y.to_bits().hash(&mut hasher);
                    v.z.to_bits().hash(&mut hasher);
                }
                p.shape.hash(&mut hasher);
                p.model.is_some().hash(&mut hasher);
            }
            let hash = hasher.finish();
            if zone.statics_hash == Some(hash) {
                continue;
            }
            for c in zone.statics.drain(..) {
                zone.world.remove_collider(c);
            }
            for p in near {
                let at = zone.frame.to_local(p.geo, p.position.y);
                let c = material(collider_for(p), p)
                    .position(Pose::from_parts(rv(at), rq(prim_quat(p))));
                zone.statics.push(zone.world.insert_collider(c, None));
            }
            zone.statics_hash = Some(hash);
            // Something under a resting body may have gone.
            zone.world.wake_up_all(true);
        }
    }

    fn sync_avatars(&mut self, avatars: &[AvatarProbe]) {
        for (key, zone) in &mut self.zones {
            let mut seen = HashSet::new();
            for a in avatars.iter().filter(|a| key.reaches(a.geo)) {
                seen.insert(a.id);
                let centre = zone.frame.to_local(a.geo, a.feet_y)
                    + Vec3::Y * (AVATAR_HALF_HEIGHT + AVATAR_RADIUS);
                match zone.avatars.get_mut(&a.id) {
                    Some(body) => {
                        if centre.distance(body.to) > AVATAR_TELEPORT_M {
                            if let Some(rb) = zone.world.bodies.get_mut(body.handle) {
                                rb.set_translation(rv(centre), true);
                            }
                            body.from = centre;
                        } else {
                            body.from = body.to;
                        }
                        body.to = centre;
                    }
                    None => {
                        let (handle, _) = zone.world.insert(
                            RigidBodyBuilder::kinematic_position_based().translation(rv(centre)),
                            ColliderBuilder::capsule_y(AVATAR_HALF_HEIGHT, AVATAR_RADIUS)
                                .friction(0.4),
                        );
                        zone.avatars.insert(
                            a.id,
                            AvatarBody {
                                handle,
                                from: centre,
                                to: centre,
                            },
                        );
                    }
                }
            }
            let gone: Vec<u64> = zone
                .avatars
                .keys()
                .filter(|id| !seen.contains(id))
                .copied()
                .collect();
            for id in gone {
                if let Some(a) = zone.avatars.remove(&id) {
                    zone.world.remove_body(a.handle);
                }
            }
        }
    }

    fn apply_pushes(&mut self, prims: &[PrimDto]) {
        let mass = |id: i64| {
            prims
                .binary_search_by_key(&id, |p| p.id)
                .map_or(1.0, |i| mass_of(&prims[i]))
        };
        for (id, v) in std::mem::take(&mut self.velocities) {
            if let Some((zone, h)) = self.locate(id) {
                if let Some(rb) = self
                    .zones
                    .get_mut(&zone)
                    .and_then(|z| z.world.bodies.get_mut(h))
                {
                    rb.set_linvel(rv(v), true);
                }
            }
        }
        for (id, j) in std::mem::take(&mut self.impulses) {
            if let Some((zone, h)) = self.locate(id) {
                if let Some(rb) = self
                    .zones
                    .get_mut(&zone)
                    .and_then(|z| z.world.bodies.get_mut(h))
                {
                    // Applied as a velocity change from the shape's own mass:
                    // Rapier fills a body's mass in at its first step, so a prim
                    // made physical this tick would otherwise read as weightless.
                    let dv = j / mass(id).max(1e-3);
                    let v = gv(rb.linvel()) + dv;
                    rb.set_linvel(rv(v.clamp_length_max(MAX_SCRIPT_SPEED)), true);
                }
            }
        }
        for kick in std::mem::take(&mut self.kicks) {
            let facing = Vec3::new(kick.yaw.sin(), 0.0, kick.yaw.cos());
            for (id, b) in &self.bodies {
                if b.held > 0.0 || !b.zone.reaches(kick.geo) {
                    continue;
                }
                let Some(zone) = self.zones.get_mut(&b.zone) else {
                    continue;
                };
                let feet = zone.frame.to_local(kick.geo, kick.feet_y);
                let Some(rb) = zone.world.bodies.get_mut(b.handle) else {
                    continue;
                };
                let at = gv(rb.translation());
                let off = at - feet;
                let flat = Vec3::new(off.x, 0.0, off.z);
                let dist = flat.length();
                if dist > KICK_REACH_M || off.y > KICK_HEIGHT_M || off.y < -0.5 {
                    continue;
                }
                // In front, or so close that the boot is on it anyway.
                if dist > 0.4 && flat.normalize().dot(facing) < 0.3 {
                    continue;
                }
                let scale = (KICK_MASS_REF_KG / mass(*id).max(1e-3)).min(1.0);
                let dv = (facing * KICK_SPEED + Vec3::Y * KICK_LIFT) * scale;
                let v = gv(rb.linvel()) + dv;
                rb.set_linvel(rv(v.clamp_length_max(MAX_SCRIPT_SPEED)), true);
            }
        }
    }

    fn locate(&self, id: i64) -> Option<(ZoneKey, RigidBodyHandle)> {
        self.bodies
            .get(&id)
            .filter(|b| b.held <= 0.0)
            .map(|b| (b.zone, b.handle))
    }

    fn read_back(&mut self, prims: &mut [PrimDto]) -> Vec<PrimBody> {
        let mut moved = Vec::new();
        for (id, b) in &mut self.bodies {
            if b.held > 0.0 {
                continue;
            }
            let Some(zone) = self.zones.get(&b.zone) else {
                continue;
            };
            let Some(rb) = zone.world.bodies.get(b.handle) else {
                continue;
            };
            let resting = rb.is_sleeping();
            if resting && b.resting_sent {
                continue;
            }
            let Ok(i) = prims.binary_search_by_key(id, |p| p.id) else {
                continue;
            };
            let (geo, y) = zone.frame.to_geo(gv(rb.translation()));
            let q = gq(*rb.rotation()).normalize();
            let (rx, ry, rz) = q.to_euler(EulerRot::XYZ);
            let p = &mut prims[i];
            p.geo = geo;
            p.position.y = y;
            p.rotation = Vec3::new(rx, ry, rz);
            b.placed = Placed::of(p);
            b.resting_sent = resting;
            moved.push(PrimBody {
                id: *id,
                geo,
                altitude: y,
                rotation: q.to_array(),
                velocity: if resting { Vec3::ZERO } else { gv(rb.linvel()) },
                resting,
            });
        }
        moved.sort_unstable_by_key(|b| b.id);
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartopolis_core::PrimSurface;

    const PITCH: GeoAnchor = GeoAnchor {
        latitude: 53.20662,
        longitude: 6.59188,
    };

    fn prim(id: i64, shape: &str, scale: Vec3, y: f32, geo: GeoAnchor, physical: bool) -> PrimDto {
        PrimDto {
            id,
            name: format!("p{id}"),
            shape: shape.into(),
            position: Vec3::new(0.0, y, 0.0),
            rotation: Vec3::ZERO,
            scale,
            color: [1.0; 3],
            texture_id: None,
            model: None,
            path_cut_begin: 0.0,
            path_cut_end: 1.0,
            hollow: 0.0,
            twist_begin: 0.0,
            twist_end: 0.0,
            taper_x: 0.0,
            taper_y: 0.0,
            top_shear_x: 0.0,
            top_shear_y: 0.0,
            slice_begin: 0.0,
            slice_end: 1.0,
            surface: PrimSurface::default(),
            geo,
            particles: None,
            scripted: false,
            video: None,
            sound_loop: None,
            physical,
        }
    }

    fn ball(id: i64, y: f32) -> PrimDto {
        prim(id, "sphere", Vec3::splat(0.22), y, PITCH, true)
    }

    fn run(phys: &mut Physics, prims: &mut [PrimDto], avatars: &[AvatarProbe], secs: f32) {
        let ticks = (secs * 20.0).round() as usize;
        for _ in 0..ticks {
            phys.step(0.05, prims, avatars);
        }
    }

    /// Metres from `a` to `b`, east and south.
    fn offset(a: GeoAnchor, b: GeoAnchor) -> Vec3 {
        Frame::new(a).to_local(b, 0.0)
    }

    #[test]
    fn frame_round_trips() {
        let f = Frame::new(ZoneKey::of(PITCH).origin());
        let p = f.to_local(PITCH, 3.0);
        let (g, y) = f.to_geo(p);
        assert!((g.latitude - PITCH.latitude).abs() < 1e-9);
        assert!((g.longitude - PITCH.longitude).abs() < 1e-9);
        assert_eq!(y, 3.0);
    }

    #[test]
    fn a_dropped_ball_lands_on_the_ground_and_rests() {
        let mut phys = Physics::new();
        let mut prims = vec![ball(1, 5.0)];
        run(&mut phys, &mut prims, &[], 6.0);
        let y = prims[0].position.y;
        assert!((y - 0.11).abs() < 0.02, "ball centre at {y}, radius 0.11");
        // At rest it stops being sent.
        let sent = phys.step(0.05, &mut prims, &[]);
        assert!(
            sent.is_empty(),
            "a resting ball is still being sent: {sent:?}"
        );
    }

    #[test]
    fn a_ball_lands_on_a_static_prim() {
        let mut phys = Physics::new();
        let table = prim(1, "box", Vec3::new(2.0, 1.0, 2.0), 0.5, PITCH, false);
        let mut prims = vec![table, ball(2, 4.0)];
        run(&mut phys, &mut prims, &[], 6.0);
        let y = prims[1].position.y;
        assert!(
            (y - 1.11).abs() < 0.03,
            "ball on a 1 m table should sit at 1.11, is at {y}"
        );
    }

    #[test]
    fn walking_into_a_ball_pushes_it() {
        let mut phys = Physics::new();
        let mut prims = vec![ball(1, 0.11)];
        run(&mut phys, &mut prims, &[], 1.0);
        let start = prims[0].geo;
        let (m_lat, _) = cartopolis_core::degree_scale_m(PITCH.latitude);
        // An avatar two metres north of the ball walks south through it at 3 m/s.
        for tick in 0..40 {
            let north = 2.0 - 3.0 * 0.05 * tick as f64;
            let avatar = AvatarProbe {
                id: 7,
                geo: GeoAnchor {
                    latitude: start.latitude + north / m_lat,
                    longitude: start.longitude,
                },
                feet_y: 0.0,
            };
            phys.step(0.05, &mut prims, &[avatar]);
        }
        let moved = offset(start, prims[0].geo);
        assert!(
            moved.z > 1.0,
            "the ball should have gone south, moved {moved:?}"
        );
    }

    #[test]
    fn a_kick_sends_the_ball_where_the_avatar_faces() {
        let mut phys = Physics::new();
        let mut prims = vec![ball(1, 0.11)];
        run(&mut phys, &mut prims, &[], 1.0);
        let start = prims[0].geo;
        let (_, m_lng) = cartopolis_core::degree_scale_m(PITCH.latitude);
        // Standing half a metre west of the ball, facing east (yaw +90°).
        phys.kick(Kick {
            geo: GeoAnchor {
                latitude: start.latitude,
                longitude: start.longitude - 0.5 / m_lng,
            },
            feet_y: 0.0,
            yaw: std::f32::consts::FRAC_PI_2,
        });
        run(&mut phys, &mut prims, &[], 1.0);
        let moved = offset(start, prims[0].geo);
        assert!(
            moved.x > 5.0,
            "a kick east should carry the ball metres east: {moved:?}"
        );
        assert!(moved.z.abs() < 1.0, "and not sideways: {moved:?}");
    }

    #[test]
    fn a_kick_behind_the_avatar_does_nothing() {
        let mut phys = Physics::new();
        let mut prims = vec![ball(1, 0.11)];
        run(&mut phys, &mut prims, &[], 1.0);
        let start = prims[0].geo;
        let (_, m_lng) = cartopolis_core::degree_scale_m(PITCH.latitude);
        // West of the ball, facing west: the ball is behind.
        phys.kick(Kick {
            geo: GeoAnchor {
                latitude: start.latitude,
                longitude: start.longitude - 1.0 / m_lng,
            },
            feet_y: 0.0,
            yaw: -std::f32::consts::FRAC_PI_2,
        });
        run(&mut phys, &mut prims, &[], 1.0);
        assert!(offset(start, prims[0].geo).length() < 0.05);
    }

    #[test]
    fn a_heavy_crate_barely_moves_when_kicked() {
        let mut phys = Physics::new();
        let mut prims = vec![prim(1, "box", Vec3::ONE, 0.5, PITCH, true)];
        run(&mut phys, &mut prims, &[], 1.0);
        let start = prims[0].geo;
        let (_, m_lng) = cartopolis_core::degree_scale_m(PITCH.latitude);
        phys.kick(Kick {
            geo: GeoAnchor {
                latitude: start.latitude,
                longitude: start.longitude - 1.0 / m_lng,
            },
            feet_y: 0.0,
            yaw: std::f32::consts::FRAC_PI_2,
        });
        run(&mut phys, &mut prims, &[], 2.0);
        assert!(offset(start, prims[0].geo).length() < 0.5);
    }

    #[test]
    fn an_edit_teleports_and_holds_the_body() {
        let mut phys = Physics::new();
        let mut prims = vec![ball(1, 0.11)];
        run(&mut phys, &mut prims, &[], 1.0);
        // A builder lifts it to 3 m.
        prims[0].position.y = 3.0;
        run(&mut phys, &mut prims, &[], HOLD_AFTER_EDIT_S * 0.5);
        assert_eq!(prims[0].position.y, 3.0, "held where the edit put it");
        run(&mut phys, &mut prims, &[], 4.0);
        assert!(prims[0].position.y < 0.2, "and then it falls");
    }

    #[test]
    fn switching_physics_off_leaves_the_prim_where_it_is() {
        let mut phys = Physics::new();
        let mut prims = vec![ball(1, 2.0)];
        run(&mut phys, &mut prims, &[], 0.25);
        prims[0].physical = false;
        let y = prims[0].position.y;
        run(&mut phys, &mut prims, &[], 1.0);
        assert_eq!(prims[0].position.y, y);
        assert_eq!(phys.body_count(), 0);
    }

    #[test]
    fn a_shot_does_not_tunnel_through_a_post() {
        let mut phys = Physics::new();
        let (_, m_lng) = cartopolis_core::degree_scale_m(PITCH.latitude);
        // A 12 cm post four metres east of the ball.
        let post_geo = GeoAnchor {
            latitude: PITCH.latitude,
            longitude: PITCH.longitude + 4.0 / m_lng,
        };
        let post = prim(1, "box", Vec3::new(0.12, 2.4, 0.12), 1.2, post_geo, false);
        let mut prims = vec![post, ball(2, 0.5)];
        run(&mut phys, &mut prims, &[], 0.2);
        phys.set_velocity(2, Vec3::new(40.0, 0.0, 0.0));
        run(&mut phys, &mut prims, &[], 1.0);
        let east = offset(PITCH, prims[1].geo).x;
        assert!(east < 4.0, "the ball went through the post: {east} m east");
    }

    #[test]
    fn a_ball_rolling_out_of_its_zone_keeps_going() {
        let mut phys = Physics::new();
        let (_, m_lng) = cartopolis_core::degree_scale_m(PITCH.latitude);
        // Two metres west of a zone boundary, rolling east.
        let edge = ((PITCH.longitude / ZONE_DEG).floor() + 1.0) * ZONE_DEG;
        let geo = GeoAnchor {
            latitude: PITCH.latitude,
            longitude: edge - 2.0 / m_lng,
        };
        let mut prims = vec![prim(1, "sphere", Vec3::splat(0.22), 0.11, geo, true)];
        run(&mut phys, &mut prims, &[], 0.2);
        phys.set_velocity(1, Vec3::new(8.0, 0.0, 0.0));
        run(&mut phys, &mut prims, &[], 1.0);
        let east = offset(geo, prims[0].geo).x;
        assert!(prims[0].geo.longitude > edge, "still west of the boundary");
        assert!(east > 4.0, "lost its speed crossing the boundary: {east} m");
    }

    #[test]
    fn mass_follows_shape() {
        let b = ball(1, 0.0);
        assert!((mass_of(&b) - 0.557).abs() < 0.01);
        let crate_ = prim(2, "box", Vec3::ONE, 0.5, PITCH, true);
        assert_eq!(mass_of(&crate_), DENSITY_KG_M3);
    }
}
