//! Crows: flocks working the sky over the camera, and birds on the open ground
//! pecking, strutting, hopping and looking round — until something walks up and
//! they go.
//!
//! The ambient traffic's terms, for the same reasons (`systems::traffic`,
//! `docs/notes/ambient-traffic.md`): **nothing is built per frame**. Six meshes
//! — a body, a head and two jointed wings (`cartopolis_geo::agent_models::crow`)
//! — and one material are made at startup and shared by a fixed pool, and a crow
//! moving is its parts' transforms being written. A group that wanders out of
//! range is placed again near the camera, never despawned.
//!
//! # What makes it read as birds rather than as dots
//!
//! * **They come in groups, and the group decides.** [`GROUP_SIZES`] runs from
//!   a pair to a flock of eleven — crows are mostly seen in pairs and loose
//!   handfuls, with the odd larger crowd. A group roams between waypoints, comes
//!   down together on open ground, feeds for a while and lifts off together; its
//!   members only steer.
//! * **Flocking, loosely.** Airborne members seek the group's waypoint, keep
//!   to the group's centre and heading, and keep a few metres apart, with a
//!   limited turn rate — so a flock arcs round a waypoint rather than snapping
//!   to the next, and banks into the turn. Crows are not starlings: the spacing
//!   is wide and the flock is ragged on purpose.
//! * **And one flock high up.** [`HIGH_FLOCK`] birds cross the sky 60–120 m up
//!   in loops up to 300 m out, and never come down: the crowd going over the
//!   town, against which the low groups are the birds working the street.
//! * **Flap, flap, glide.** Each bird has its own bouts of beating and gliding,
//!   glides when it is coming down and beats when it climbs; the wing is two
//!   segments whose hand lags the arm ([`wingbeat`]).
//! * **On the ground, small things in sequence**: a bout of pecks, a few
//!   strutting steps with the head bobbing, a hop with a flick of the wings, a
//!   look round. Each crow chooses its own, so a group is never in step.
//! * **They spook.** The avatar, a low camera or one of the ambient walkers
//!   coming within a few metres flushes the nearest bird, and the rest follow a
//!   beat later — the cascade is what a real flock taking off looks like.
//! * **And they are heard, a little.** Every 7–20 s a crow within [`HEAR_M`]
//!   of the camera gives one to three caws, quieter with distance, and a
//!   flushed group an alarm as it goes (`play_crow_calls`, recordings).
//!
//! # Where they land, and what that is not
//!
//! Crows come down only where the paving survey says there is open ground a crow
//! can be seen on — a square, a wide path, a car park, a yard, or a lawn the
//! grass layer has not grown its knee-high blades on
//! ([`CrowGround::feeding_ground`]) — and only where the last 30 m of the glide
//! in clears the walls. That is a *texture* over real data, as the soundscape's
//! birdsong is: it places nothing and claims nothing about a particular bird.
//! Like the walkers, the birds themselves are invented.
//!
//! # Deliberately not
//!
//! No roosting on roofs or wires, no flying round buildings (the flocks cruise
//! above the roofs of a Dutch city, which is most of them; a tower is flown
//! through), no night roost.

use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use big_space::prelude::{BigSpace, Grid, GridCell};
use std::f32::consts::{PI, TAU};

use cartopolis_geo::agent_models::{self as models, wingbeat, WingPose, CROW_ARM_M, CROW_NECK};
use cartopolis_geo::paving::{PavingKind, PavingMaterial};

use crate::components::{FreeCamera, HideInSpace};
use crate::grid::{anchored_position, AnchoredCamera};

/// The groups, by size: pairs and handfuls, and one larger flock. 34 birds.
pub const GROUP_SIZES: [usize; 7] = [11, 7, 5, 4, 3, 2, 2];

/// The high flock: a crowd of 44 crossing the sky 60–120 m up in wide loops,
/// never landing — the flock seen going over a Dutch town on a winter
/// afternoon, where the low groups are the birds working the street. It is the
/// last group, after [`GROUP_SIZES`].
pub const HIGH_FLOCK: usize = 44;
/// The high flock's waypoints are drawn this far out, so it crosses the whole
/// sky and is sometimes far off rather than always overhead.
const HIGH_ROAM_M: f32 = 300.0;
const HIGH_RECYCLE_M: f32 = 650.0;
/// How far apart the high flock's birds keep: wider than a low group's, so at
/// 100 m it reads as a loose spray of birds and not one blot.
const HIGH_SPACING_M: f32 = 5.0;

/// Above this the layer is hidden: a 46 cm bird is under a pixel well before.
const HIDE_ALT: f32 = 450.0;
/// Waypoints are drawn within this of the camera.
const ROAM_M: f32 = 170.0;
/// A group whose centre is this far from the camera is placed again near it.
/// Well outside [`ROAM_M`] so a flock on the far side of a waypoint is not
/// caught by it.
const RECYCLE_M: f32 = 330.0;
/// A crow's cruising speed, m/s. Carrion crows fly at 9–12.
const CRUISE: f32 = 9.5;
/// The fastest a flying crow may turn its heading, rad/s.
const TURN_RATE: f32 = 1.3;
/// Wingbeats per second in level flight; take-off and the landing flare beat
/// faster.
const FLAP_HZ: f32 = 4.0;
/// How close something may come to a feeding crow before it goes, metres.
const STARTLE_M: f32 = 6.0;
/// The camera's own radius: a camera hovering for a close look should not
/// clear the square as a person walking up would.
const CAMERA_STARTLE_M: f32 = 2.5;
/// A walker's: they pass along the pavement, and crows on a verge tolerate
/// people on the far side of it.
const WALKER_STARTLE_M: f32 = 3.5;
/// How far down the approach to a landing site must be free of walls.
const APPROACH_M: f32 = 30.0;

/// One crow's root: its body. The head and wings are children ([`CrowParts`]).
#[derive(Component)]
pub struct Crow {
    group: usize,
    mode: Mode,
    /// Anchored world position of the feet, absolute height in `y`.
    pos: Vec3,
    vel: Vec3,
    yaw: f32,
    /// Body pitch, positive nose-down, and roll, positive raising the `+x`
    /// (left) wing.
    pitch: f32,
    roll: f32,
    flap_phase: f32,
    beat: f32,
    spread: f32,
    /// Seconds left in the current flapping or gliding bout.
    bout: f32,
    gliding: bool,
    /// This bird's place in its group: an offset on the landing site, and a
    /// height offset in the air.
    slot: Vec2,
    lift: f32,
    act: Act,
    /// Seconds left in `act`, and seconds into it.
    act_left: f32,
    act_age: f32,
    /// Seconds before a take-off leaves the ground: a flock goes one by one.
    delay: f32,
    head_yaw: f32,
    head_pitch: f32,
    head_target: f32,
    head_bob: f32,
    step_phase: f32,
    turn: f32,
    hop_from: Vec2,
    hop_to: Vec2,
    /// The group placement this bird last followed ([`Flock::seq`]).
    seq: u32,
    speed_scale: f32,
    rng: Rng,
    parts: CrowParts,
}

#[derive(Clone, Copy)]
struct CrowParts {
    head: Entity,
    arm: [Entity; 2],
    hand: [Entity; 2],
}

/// The head and wings, for the `Without` that keeps their transforms apart from
/// the bodies'.
#[derive(Component)]
pub struct CrowPart;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Hidden,
    Air,
    Landing,
    Ground,
    TakeOff,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    Stand,
    Peck,
    Walk,
    Hop,
    Look,
    Turn,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FlockMode {
    /// Not anywhere yet, or recycled: waiting to be placed near the camera.
    Unplaced,
    Roam,
    Land,
    Ground,
}

struct Flock {
    mode: FlockMode,
    /// The waypoint, anchored world XZ, and the height over the ground the
    /// group cruises at.
    goal: Vec2,
    height: f32,
    /// Seconds until the group next considers landing (roaming), or leaves
    /// (on the ground).
    timer: f32,
    site: Vec2,
    /// Where a flush came from, so the flock leaves away from it.
    flushed_from: Option<Vec2>,
    /// Bumped on every placement; a member that has not seen it places itself.
    seq: u32,
    spawn_on_ground: bool,
    tries: u32,
    rng: Rng,
    size: usize,
    /// The high flock: roams wide, cruises high, never lands ([`HIGH_FLOCK`]).
    high: bool,
}

impl Flock {
    fn roam_m(&self) -> f32 {
        if self.high {
            HIGH_ROAM_M
        } else {
            ROAM_M
        }
    }

    fn recycle_m(&self) -> f32 {
        if self.high {
            HIGH_RECYCLE_M
        } else {
            RECYCLE_M
        }
    }
}

/// The groups' shared state. Per-bird state is on [`Crow`].
#[derive(Resource, Default)]
pub struct Crows {
    flocks: Vec<Flock>,
    /// Bumped each time a feeding group is flushed, with where: the calls
    /// (`play_crow_calls`) answer it with an alarm.
    flushes: u32,
    flush_at: Vec3,
}

/// A small deterministic generator, one per bird and per group, so the pool
/// behaves the same run to run and a capture can be repeated.
#[derive(Clone, Copy)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }
    fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 40) as f32 / 16_777_216.0
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
    fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
    /// A point in the disc of radius `r`, uniform by area.
    fn disc(&mut self, r: f32) -> Vec2 {
        Vec2::from_angle(self.range(0.0, TAU)) * r * self.unit().sqrt()
    }
}

/// The map's answers about the ground, bundled.
#[derive(bevy::ecs::system::SystemParam)]
pub struct CrowGround<'w> {
    terrain: Res<'w, crate::systems::terrain::TerrainField>,
    grass: Res<'w, crate::systems::grass::GrassAreas>,
    places: Res<'w, crate::systems::world_places::WorldPlaces>,
    walls: Res<'w, crate::systems::collision::BuildingColliders>,
}

impl CrowGround<'_> {
    fn height(&self, p: Vec2) -> f32 {
        self.terrain.ground_at(p.x, p.y)
    }

    /// Open ground a crow would feed on *and still be seen on*: a square, a
    /// wide path, a car park or a yard, or a lawn with no blades drawn on it.
    ///
    /// **Not the grass layer's lawns.** Its blades are wild grass, 23–53 cm
    /// (`grass::BLADE_HEIGHT_M` and the shader's jitter), and a feeding crow is
    /// about 25 cm tall: the first captures put a group of seven on a lawn in
    /// the Noorderplantsoen and showed one dark speck. Where the layer is off,
    /// or has not planted a lawn, the survey's grass is flat and a crow on it
    /// reads.
    fn feeding_ground(&self, p: Vec2) -> bool {
        if self.grass.grass_at(p) || self.terrain.water_surface_at(p.x, p.y).is_some() {
            return false;
        }
        matches!(
            self.places.paving_at(p),
            Some((_, Some(PavingMaterial::Grass | PavingMaterial::Planting)))
                | Some((
                    PavingKind::Verge
                        | PavingKind::Garden
                        | PavingKind::Paved
                        | PavingKind::Footway
                        | PavingKind::Parking
                        | PavingKind::LivingStreet
                        | PavingKind::Yard,
                    _
                ))
        )
    }

    /// A landing site: feeding ground round about, and the last of the glide in from
    /// `from` clear of the walls.
    fn site(&self, p: Vec2, from: Vec2) -> bool {
        if !self.feeding_ground(p) {
            return false;
        }
        let around =
            (0..4).all(|k| self.feeding_ground(p + Vec2::from_angle(k as f32 * PI * 0.5) * 1.5));
        if !around {
            return false;
        }
        let dir = (from - p).normalize_or(Vec2::X);
        let g = self.height(p);
        self.walls
            .segment_fraction(p, g + 0.5, p + dir * APPROACH_M, g + APPROACH_M * 0.4)
            .is_none()
    }

    /// Look for a site within `r` of `around`, approached from `from`, and not
    /// beside anything that would flush the birds the moment they were down.
    fn find_site(
        &self,
        rng: &mut Rng,
        around: Vec2,
        r: f32,
        from: Vec2,
        avoid: &[(Vec3, f32)],
    ) -> Option<Vec2> {
        (0..14).map(|_| around + rng.disc(r)).find(|&p| {
            avoid
                .iter()
                .all(|(t, r)| t.xz().distance(p) > r * 2.0 + 4.0)
                && self.site(p, from)
        })
    }
}

/// Build the shared model and spawn the pool, hidden until placed.
pub fn setup_crows(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    roots: Query<Entity, With<BigSpace>>,
) {
    let Ok(root) = roots.single() else {
        // `.after(setup_camera)`, as the traffic is; see `setup_traffic`.
        tracing::warn!("no BigSpace root at crow setup; crows are off for this run");
        return;
    };
    let model = models::crow();
    let mut mesh =
        |m: &models::ModelMesh| meshes.add(crate::systems::traffic::model_mesh_to_bevy(m));
    let body = mesh(&model.body);
    let head = mesh(&model.head);
    let arm = [mesh(&model.arm[0]), mesh(&model.arm[1])];
    let hand = [mesh(&model.hand[0]), mesh(&model.hand[1])];
    // The colour is in the vertices; the material says "feathers": matte, with
    // the faint sheen a crow's back has in the sun.
    let feathers = materials.add(StandardMaterial {
        perceptual_roughness: 0.6,
        reflectance: 0.25,
        ..default()
    });
    let part = |m: &Handle<Mesh>| {
        (
            Mesh3d(m.clone()),
            MeshMaterial3d(feathers.clone()),
            Transform::default(),
            CrowPart,
        )
    };

    let mut flocks = Vec::new();
    let mut i = 0u64;
    let sizes = GROUP_SIZES
        .iter()
        .map(|&s| (s, false))
        .chain([(HIGH_FLOCK, true)]);
    for (g, (size, high)) in sizes.enumerate() {
        let mut rng = Rng::new(0xC0_0C + g as u64);
        flocks.push(Flock {
            mode: FlockMode::Unplaced,
            goal: Vec2::ZERO,
            height: if high {
                rng.range(70.0, 110.0)
            } else {
                rng.range(20.0, 42.0)
            },
            timer: 0.0,
            site: Vec2::ZERO,
            flushed_from: None,
            seq: 0,
            // About half the groups start out feeding, so a street has birds on
            // its squares from the start rather than only once a flock comes down.
            spawn_on_ground: !high && (g % 2 == 1 || size <= 2),
            tries: 0,
            rng,
            size,
            high,
        });
        for _ in 0..size {
            let mut rng = Rng::new(0xC2_0F_F1 + i * 7919);
            i += 1;
            let parts = CrowParts {
                head: commands.spawn(part(&head)).id(),
                arm: [
                    commands.spawn(part(&arm[0])).id(),
                    commands.spawn(part(&arm[1])).id(),
                ],
                hand: [
                    commands.spawn(part(&hand[0])).id(),
                    commands.spawn(part(&hand[1])).id(),
                ],
            };
            // Crows vary a little in size, and a pair of identical birds is a
            // copy rather than a pair.
            let size_scale = rng.range(0.92, 1.06);
            let crow = Crow {
                group: g,
                mode: Mode::Hidden,
                pos: Vec3::ZERO,
                vel: Vec3::ZERO,
                yaw: rng.range(0.0, TAU),
                pitch: 0.0,
                roll: 0.0,
                flap_phase: rng.range(0.0, TAU),
                beat: 0.0,
                spread: 0.0,
                bout: rng.range(0.5, 3.0),
                gliding: false,
                slot: Vec2::ZERO,
                lift: rng.range(-3.0, 3.0),
                act: Act::Stand,
                act_left: rng.range(0.2, 1.5),
                act_age: 0.0,
                delay: 0.0,
                head_yaw: 0.0,
                head_pitch: 0.0,
                head_target: 0.0,
                head_bob: 0.0,
                step_phase: 0.0,
                turn: 0.0,
                hop_from: Vec2::ZERO,
                hop_to: Vec2::ZERO,
                seq: 0,
                speed_scale: rng.range(0.9, 1.1),
                rng,
                parts,
            };
            let id = commands
                .spawn((
                    Mesh3d(body.clone()),
                    MeshMaterial3d(feathers.clone()),
                    Visibility::Hidden,
                    Transform::from_scale(Vec3::splat(size_scale)),
                    GridCell::default(),
                    ChildOf(root),
                    HideInSpace,
                    crow,
                ))
                .id();
            for p in [parts.head, parts.arm[0], parts.arm[1]] {
                commands.entity(p).insert(ChildOf(id));
            }
            // A bird 150 m up casts a smudge no bigger than itself somewhere far off
            // across the town; not worth a place in every shadow cascade.
            if high {
                for e in [
                    id,
                    parts.head,
                    parts.arm[0],
                    parts.arm[1],
                    parts.hand[0],
                    parts.hand[1],
                ] {
                    commands.entity(e).insert(NotShadowCaster);
                }
            }
            // The hands hang from the arms' tips.
            for s in 0..2 {
                commands.entity(parts.hand[s]).insert(ChildOf(parts.arm[s]));
            }
        }
    }
    commands.insert_resource(Crows {
        flocks,
        flushes: 0,
        flush_at: Vec3::ZERO,
    });
}

/// What the members of one group are doing this frame, summed before any of
/// them moves.
#[derive(Default, Clone, Copy)]
struct GroupStats {
    air: usize,
    air_centre: Vec3,
    air_vel: Vec3,
    ground: usize,
    hidden: usize,
    centre: Vec2,
    members: usize,
}

/// Advance every group and every crow, and write their transforms.
#[allow(clippy::too_many_arguments)]
pub fn drive_crows(
    time: Res<crate::systems::anim_clock::AnimClock>,
    layers: Res<crate::systems::data_layers::DataLayers>,
    ground: CrowGround,
    globe: Res<crate::systems::globe::GlobeState>,
    camera: AnchoredCamera,
    grids: Query<&Grid, With<BigSpace>>,
    avatar: Res<crate::resources::AvatarState>,
    session: Res<crate::systems::session::SessionState>,
    state: Option<ResMut<Crows>>,
    mut crows: Query<
        (&mut Crow, &mut Transform, &mut GridCell, &mut Visibility),
        (Without<FreeCamera>, Without<CrowPart>),
    >,
    mut parts: Query<&mut Transform, (With<CrowPart>, Without<Crow>, Without<FreeCamera>)>,
    mut here: Local<Option<bool>>,
    walkers: Query<
        (
            &GridCell,
            &Transform,
            &crate::systems::traffic::AgentKind,
            &Visibility,
        ),
        (Without<Crow>, Without<CrowPart>, Without<FreeCamera>),
    >,
) {
    let Some(mut state) = state else {
        return;
    };
    let (Some((cam_cell, cam_tf)), Ok(grid)) = (camera.get(), grids.single()) else {
        return;
    };
    // Off, or too high to see: hidden, and held where they are, so switching
    // back on resumes rather than re-places.
    if !layers.birds_visible() || globe.altitude_m(cam_cell, cam_tf) as f32 > HIDE_ALT {
        for (.., mut vis) in &mut crows {
            vis.set_if_neq(Visibility::Hidden);
        }
        return;
    }
    let cam = anchored_position(cam_cell, cam_tf.translation);
    let cam_xz = cam.xz();
    let dt = time.delta_secs().min(0.1);

    // What may flush a feeding crow, and how close it may come.
    let mut threats: Vec<(Vec3, f32)> = vec![(cam, CAMERA_STARTLE_M)];
    if session.is_online() {
        threats.push((avatar.display_position, STARTLE_M));
    }
    for (cell, tf, kind, vis) in &walkers {
        if *kind == crate::systems::traffic::AgentKind::Walker && *vis != Visibility::Hidden {
            threats.push((anchored_position(cell, tf.translation), WALKER_STARTLE_M));
        }
    }

    // ── The groups ───────────────────────────────────────────────────────────
    let mut stats = vec![GroupStats::default(); state.flocks.len()];
    let snapshot: Vec<(usize, Mode, Vec3, Vec3)> = crows
        .iter()
        .map(|(c, ..)| (c.group, c.mode, c.pos, c.vel))
        .collect();
    for &(g, mode, pos, vel) in &snapshot {
        let s = &mut stats[g];
        s.members += 1;
        s.centre += pos.xz();
        match mode {
            Mode::Air | Mode::TakeOff => {
                s.air += 1;
                s.air_centre += pos;
                s.air_vel += vel;
            }
            Mode::Landing => {}
            Mode::Ground => s.ground += 1,
            Mode::Hidden => s.hidden += 1,
        }
    }
    for s in &mut stats {
        if s.air > 0 {
            s.air_centre /= s.air as f32;
            s.air_vel /= s.air as f32;
        }
        if s.members > 0 {
            s.centre /= s.members as f32;
        }
    }

    // `CARTO_CROWS_HERE=1`: a group feeding 7 m in front of the camera, the
    // big flock circling just past it and the high flock further out, so a
    // capture can be aimed at birds instead of hoping one is in frame. Read
    // once; a dev switch, not a mode.
    let here = *here.get_or_insert_with(|| crate::debug_switches::env_flag("CARTO_CROWS_HERE"));
    let ahead = {
        let f = cam_tf.forward().as_vec3().xz().normalize_or(Vec2::NEG_Y);
        cam_xz + f * 7.0
    };

    for (g, flock) in state.flocks.iter_mut().enumerate() {
        let s = stats[g];
        flock.timer -= dt;
        if here && g == 1 {
            if flock.mode != FlockMode::Ground || flock.site.distance(ahead) > 3.0 {
                flock.mode = FlockMode::Ground;
                flock.site = ahead;
                flock.seq = flock.seq.wrapping_add(1);
            }
            flock.timer = f32::MAX;
            continue;
        }
        if here && flock.high {
            // ...and the high flock wheeling 120 m out in front, 70 m up.
            let out = cam_xz + (ahead - cam_xz).normalize_or(Vec2::NEG_Y) * 120.0;
            flock.height = 70.0;
            flock.timer = f32::MAX;
            if flock.mode != FlockMode::Roam {
                flock.mode = FlockMode::Roam;
                flock.goal = out;
                flock.seq = flock.seq.wrapping_add(1);
            }
            let at = if s.air > 0 {
                s.air_centre.xz()
            } else {
                s.centre
            };
            if at.distance(flock.goal) < 15.0 || flock.goal.distance(out) > 60.0 {
                flock.goal = out + flock.rng.disc(50.0);
            }
            continue;
        }
        if here && g == 0 {
            flock.height = 9.0;
            if flock.mode != FlockMode::Roam {
                flock.mode = FlockMode::Roam;
                flock.seq = flock.seq.wrapping_add(1);
            }
            flock.timer = f32::MAX;
            let at = if s.air > 0 {
                s.air_centre.xz()
            } else {
                s.centre
            };
            if at.distance(flock.goal) < 12.0 || flock.goal.distance(ahead) > 40.0 {
                flock.goal = ahead + flock.rng.disc(30.0);
            }
            continue;
        }
        let far = s.members > s.hidden && s.centre.distance(cam_xz) > flock.recycle_m();
        if far && flock.mode != FlockMode::Unplaced {
            flock.mode = FlockMode::Unplaced;
            flock.tries = 0;
            flock.timer = 0.0;
        }
        match flock.mode {
            FlockMode::Unplaced => {
                if flock.timer > 0.0 {
                    continue;
                }
                flock.timer = 1.0;
                flock.tries += 1;
                let mut rng = flock.rng;
                let site = if flock.spawn_on_ground {
                    let around = cam_xz + rng.disc(55.0);
                    ground.find_site(&mut rng, around, 25.0, cam_xz, &threats)
                } else {
                    None
                };
                flock.rng = rng;
                match site {
                    Some(site) => {
                        flock.site = site;
                        tracing::debug!(
                            group = g,
                            east_m = site.x - cam_xz.x,
                            south_m = site.y - cam_xz.y,
                            "crows placed on open ground"
                        );
                        flock.mode = FlockMode::Ground;
                        flock.timer = flock.rng.range(30.0, 90.0);
                    }
                    // The map may still be loading: give a ground group a few
                    // seconds to find its ground before it takes to the air.
                    None if flock.spawn_on_ground && flock.tries < 12 => continue,
                    None => {
                        // The high flock starts well inside its range, so it is in
                        // the sky from the start rather than arriving from 450 m off.
                        let r = if flock.high {
                            HIGH_ROAM_M * 0.4
                        } else {
                            ROAM_M
                        };
                        flock.goal = cam_xz + Vec2::from_angle(flock.rng.range(0.0, TAU)) * r;
                        flock.site = flock.goal;
                        flock.mode = FlockMode::Roam;
                        flock.timer = flock.rng.range(15.0, 45.0);
                    }
                }
                flock.flushed_from = None;
                flock.seq = flock.seq.wrapping_add(1);
            }
            FlockMode::Roam => {
                // A new waypoint once the flock is there, and the next one well
                // away from it, so the flock crosses the view rather than milling.
                let at = if s.air > 0 {
                    s.air_centre.xz()
                } else {
                    s.centre
                };
                let (arrive, leg) = if flock.high {
                    (40.0, 160.0)
                } else {
                    (25.0, 70.0)
                };
                if at.distance(flock.goal) < arrive {
                    flock.flushed_from = None;
                    let roam = flock.roam_m();
                    let mut next = cam_xz + flock.rng.disc(roam);
                    for _ in 0..4 {
                        if next.distance(at) > leg {
                            break;
                        }
                        next = cam_xz + flock.rng.disc(roam);
                    }
                    flock.goal = next;
                    if flock.high {
                        tracing::debug!(
                            east_m = at.x - cam_xz.x,
                            south_m = at.y - cam_xz.y,
                            up_m = s.air_centre.y - cam.y,
                            "high flock at a waypoint"
                        );
                    }
                    flock.height = if flock.high {
                        (flock.height + flock.rng.range(-15.0, 15.0)).clamp(60.0, 120.0)
                    } else {
                        (flock.height + flock.rng.range(-8.0, 8.0)).clamp(18.0, 45.0)
                    };
                }
                if flock.timer <= 0.0 && s.air == s.members && !flock.high {
                    // Ahead, and far enough ahead to glide down to rather than
                    // drop on: a site straight under the flock is none.
                    let ahead = at + s.air_vel.xz().normalize_or_zero() * 70.0;
                    let mut rng = flock.rng;
                    let site = if ahead.distance(cam_xz) < ROAM_M {
                        ground.find_site(&mut rng, ahead, 35.0, at, &threats)
                    } else {
                        None
                    };
                    flock.rng = rng;
                    match site {
                        Some(site) => {
                            flock.site = site;
                            tracing::debug!(
                                group = g,
                                east_m = site.x - cam_xz.x,
                                south_m = site.y - cam_xz.y,
                                "crows coming down on open ground"
                            );
                            flock.mode = FlockMode::Land;
                            flock.timer = 30.0;
                        }
                        None => flock.timer = flock.rng.range(4.0, 10.0),
                    }
                }
            }
            FlockMode::Land => {
                // The timer is a backstop: a straggler still coming in lands on
                // its own and joins whatever the group is doing by then.
                if s.ground == s.members || flock.timer <= 0.0 {
                    flock.mode = FlockMode::Ground;
                    flock.timer = flock.rng.range(25.0, 80.0);
                }
            }
            FlockMode::Ground => {
                if flock.timer <= 0.0 {
                    flock.mode = FlockMode::Roam;
                    flock.goal = flock.site + flock.rng.disc(ROAM_M);
                    flock.timer = flock.rng.range(20.0, 60.0);
                }
            }
        }
    }

    // ── The birds ────────────────────────────────────────────────────────────
    let mut flushed: Option<Vec3> = None;
    for (i, (mut crow, mut tf, mut cell, mut vis)) in crows.iter_mut().enumerate() {
        let crow = &mut *crow;
        let g = crow.group;
        let flock = &mut state.flocks[g];

        if flock.mode == FlockMode::Unplaced {
            crow.mode = Mode::Hidden;
            vis.set_if_neq(Visibility::Hidden);
            continue;
        }
        if crow.seq != flock.seq {
            crow.seq = flock.seq;
            place(crow, flock, flock.size, cam_xz, &ground);
        }
        // The group's decisions reach its members.
        match (flock.mode, crow.mode) {
            (FlockMode::Land, Mode::Air) => {
                crow.mode = Mode::Landing;
                crow.slot = flock_slot(&mut crow.rng, flock.size);
            }
            (FlockMode::Roam, Mode::Ground) => {
                crow.mode = Mode::TakeOff;
                crow.delay = if flock.flushed_from.is_some() {
                    crow.rng.range(0.08, 0.6)
                } else {
                    crow.rng.range(0.0, 2.5)
                };
                crow.act_age = 0.0;
                if let Some(t) = flock.flushed_from {
                    crow.yaw = heading_of(crow.pos.xz() - t) + crow.rng.range(-0.6, 0.6);
                }
            }
            _ => {}
        }

        match crow.mode {
            Mode::Hidden => {
                vis.set_if_neq(Visibility::Hidden);
                continue;
            }
            Mode::Air => {
                let s = stats[g];
                fly(crow, flock, s, &snapshot, i, &ground, dt);
            }
            Mode::Landing => land(crow, flock.site, flock.height, &ground, dt),
            Mode::TakeOff => take_off(crow, &ground, dt),
            Mode::Ground => {
                let leash = flock.site;
                walk_about(crow, leash, flock.size, &snapshot, i, &ground, dt);
                // Flushed: this bird goes now and tells the group.
                let p = crow.pos;
                if let Some(&(t, _)) = threats
                    .iter()
                    .find(|(t, r)| (t.y - p.y).abs() < 5.0 && t.xz().distance(p.xz()) < *r)
                {
                    crow.mode = Mode::TakeOff;
                    crow.delay = 0.0;
                    crow.act_age = 0.0;
                    crow.yaw = heading_of(p.xz() - t.xz()) + crow.rng.range(-0.5, 0.5);
                    if flock.mode == FlockMode::Ground || flock.mode == FlockMode::Land {
                        flock.mode = FlockMode::Roam;
                        flock.flushed_from = Some(t.xz());
                        flock.goal = p.xz() + (p.xz() - t.xz()).normalize_or(Vec2::X) * 80.0;
                        flock.timer = flock.rng.range(25.0, 60.0);
                        // Once per group: the first bird up raises the alarm.
                        flushed = Some(p);
                    }
                }
            }
        }

        draw(crow, &mut tf, &mut cell, grid, &mut parts);
        vis.set_if_neq(Visibility::Inherited);
    }
    if let Some(p) = flushed {
        state.flushes = state.flushes.wrapping_add(1);
        state.flush_at = p;
    }
}

impl Crow {
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Anchored world position of the feet.
    #[must_use]
    pub fn position(&self) -> Vec3 {
        self.pos
    }

    /// In the air: cruising, coming in to land, or climbing out.
    #[must_use]
    pub fn flying(&self) -> bool {
        matches!(self.mode, Mode::Air | Mode::Landing | Mode::TakeOff)
    }
}

/// Put a bird where its group has just been placed.
fn place(crow: &mut Crow, flock: &Flock, size: usize, cam: Vec2, ground: &CrowGround) {
    crow.vel = Vec3::ZERO;
    crow.pitch = 0.0;
    crow.roll = 0.0;
    crow.delay = 0.0;
    match flock.mode {
        FlockMode::Ground => {
            crow.slot = flock_slot(&mut crow.rng, size);
            // The slot as drawn, even off the ground just checked: falling back to
            // the site itself stacked a group on one spot when it was placed
            // before the paving cells had arrived.
            let p = flock.site + crow.slot;
            crow.pos = Vec3::new(p.x, ground.height(p), p.y);
            crow.mode = Mode::Ground;
            crow.spread = 0.0;
            crow.beat = 0.0;
            crow.yaw = crow.rng.range(0.0, TAU);
            begin_act(crow, flock.site, size);
        }
        _ => {
            let off = crow.rng.disc(8.0 + size as f32);
            let p = flock.goal + off;
            // Flying in, across the camera's ground rather than at it.
            let dir = Vec2::from_angle(crow.rng.range(-0.6, 0.6))
                .rotate((cam - flock.goal).normalize_or(Vec2::X));
            crow.pos = Vec3::new(p.x, ground.height(p) + flock.height + crow.lift, p.y);
            crow.vel = Vec3::new(dir.x, 0.0, dir.y) * CRUISE;
            crow.yaw = heading_of(dir);
            crow.mode = Mode::Air;
            crow.spread = 1.0;
            crow.beat = 1.0;
        }
    }
}

/// Where in a group of `size` a bird comes down: a loose patch a metre or two
/// across, wider for a larger group.
fn flock_slot(rng: &mut Rng, size: usize) -> Vec2 {
    rng.disc(0.8 * (size as f32).sqrt() + 0.5)
}

/// The yaw that points the model's `+z` along `d` (as the traffic measures it).
fn heading_of(d: Vec2) -> f32 {
    d.x.atan2(d.y)
}

/// Shortest signed angle from `a` to `b`.
fn angle_diff(a: f32, b: f32) -> f32 {
    (b - a + PI).rem_euclid(TAU) - PI
}

/// Ease `v` toward `to` at `rate` per second, frame-rate independently.
fn ease(v: f32, to: f32, rate: f32, dt: f32) -> f32 {
    v + (to - v) * (1.0 - (-rate * dt).exp())
}

/// Turn the horizontal velocity toward `want` at no more than `turn` rad/s, and
/// its speed toward `want`'s at `accel` m/s².
fn steer(vel: Vec3, want: Vec2, turn: f32, accel: f32, dt: f32) -> Vec2 {
    let h = vel.xz();
    let speed = h.length();
    let want_speed = want.length();
    let new_speed = speed + (want_speed - speed).clamp(-accel * dt, accel * dt);
    let dir = if speed < 0.05 {
        want.normalize_or(Vec2::Y)
    } else {
        let a = heading_of(h);
        let b = if want_speed < 1e-3 {
            a
        } else {
            heading_of(want)
        };
        let yaw = a + angle_diff(a, b).clamp(-turn * dt, turn * dt);
        Vec2::new(yaw.sin(), yaw.cos())
    };
    dir * new_speed
}

/// Flocking toward the group's waypoint.
fn fly(
    crow: &mut Crow,
    flock: &Flock,
    s: GroupStats,
    snapshot: &[(usize, Mode, Vec3, Vec3)],
    me: usize,
    ground: &CrowGround,
    dt: f32,
) {
    let p = crow.pos;
    let spacing = if flock.high { HIGH_SPACING_M } else { 4.0 };
    let to_goal = flock.goal - p.xz();
    let mut want = to_goal.normalize_or_zero() * CRUISE * crow.speed_scale;
    if s.air > 1 {
        want += (s.air_centre.xz() - p.xz()) * 0.10 + (s.air_vel.xz() - crow.vel.xz()) * 0.35;
    }
    for (j, &(g, mode, q, _)) in snapshot.iter().enumerate() {
        if j == me || g != crow.group || !matches!(mode, Mode::Air | Mode::TakeOff) {
            continue;
        }
        let d = p - q;
        let l = d.length();
        if l > 1e-3 && l < spacing {
            want += d.xz() / l * (spacing - l) * 1.6;
        }
    }
    let floor = ground.height(p.xz());
    // The high flock is deeper as well as wider: a crowd at 150 m with every
    // bird at one height reads as a flat sheet.
    let lift = if flock.high {
        crow.lift * 4.0
    } else {
        crow.lift
    };
    let target_y = (ground.height(flock.goal) + flock.height + lift).max(floor + 12.0);
    let climb = ((target_y - p.y) * 0.35).clamp(-2.5, 2.5);
    // A crow cannot hover: whatever the sum, it keeps flying forward.
    let want = if want.length_squared() < 1e-4 {
        crow.vel.xz()
    } else {
        want
    };
    let h = steer(crow.vel, want.clamp_length(5.5, 13.0), TURN_RATE, 2.5, dt);
    crow.vel = Vec3::new(h.x, ease(crow.vel.y, climb, 1.5, dt), h.y);
    crow.pos += crow.vel * dt;
    crow.pos.y = crow.pos.y.max(floor + 2.0);

    // Bouts of beating and gliding; beat to climb, glide to come down.
    crow.bout -= dt;
    if crow.bout <= 0.0 {
        crow.gliding = !crow.gliding;
        crow.bout = if crow.gliding {
            crow.rng.range(0.6, 2.2)
        } else {
            crow.rng.range(1.2, 4.0)
        };
    }
    let glide = (crow.gliding && climb < 0.6) || climb < -1.2;
    crow.beat = ease(crow.beat, if glide { 0.0 } else { 0.95 }, 4.0, dt);
    crow.spread = ease(crow.spread, 1.0, 6.0, dt);
    crow.flap_phase = (crow.flap_phase + FLAP_HZ * crow.speed_scale * TAU * dt) % TAU;
    flight_attitude(crow, dt);
}

/// Body attitude from the velocity: yaw along it, the nose following the climb,
/// and a bank into the turn.
fn flight_attitude(crow: &mut Crow, dt: f32) {
    let h = crow.vel.xz();
    let speed = h.length();
    if speed > 0.3 {
        let yaw = heading_of(h);
        let rate = if dt > 0.0 {
            angle_diff(crow.yaw, yaw) / dt
        } else {
            0.0
        };
        crow.yaw = yaw;
        // Turning toward +x (yaw rising) is turning left; the left wing dips.
        crow.roll = ease(
            crow.roll,
            (-rate * speed * 0.07).clamp(-0.75, 0.75),
            4.0,
            dt,
        );
    }
    let climb = crow.vel.y.atan2(speed.max(0.5));
    crow.pitch = ease(crow.pitch, (-climb * 0.8).clamp(-0.7, 0.6), 4.0, dt);
    crow.head_pitch = ease(crow.head_pitch, -crow.pitch * 0.7, 8.0, dt);
    crow.head_yaw = ease(crow.head_yaw, 0.0, 6.0, dt);
    crow.head_bob = 0.0;
}

/// Glide down the approach to this bird's patch of the site, flare, and stand.
fn land(crow: &mut Crow, site: Vec2, height: f32, ground: &CrowGround, dt: f32) {
    let target = site + crow.slot;
    let gy = ground.height(target);
    let d = target.distance(crow.pos.xz());
    // A three-to-one glide path, steeper than the approach check's slope so the
    // bird is inside what was checked.
    let path_y = gy + (d * 0.33).min(height + crow.lift);
    let want_speed = (d * 0.55).clamp(0.5, CRUISE);
    let dir = (target - crow.pos.xz()).normalize_or_zero();
    let turn = if d < 6.0 { 6.0 } else { 2.2 };
    let h = steer(crow.vel, dir * want_speed, turn, 4.0, dt);
    let sink = ((path_y - crow.pos.y) * 1.2).clamp(-4.0, 1.5);
    crow.vel = Vec3::new(h.x, ease(crow.vel.y, sink, 3.0, dt), h.y);
    crow.pos += crow.vel * dt;
    crow.pos.y = crow.pos.y.max(gy);

    let flare = d < 4.0;
    crow.beat = ease(crow.beat, if flare { 1.0 } else { 0.1 }, 5.0, dt);
    crow.spread = ease(crow.spread, 1.0, 6.0, dt);
    let hz = if flare { FLAP_HZ * 1.4 } else { FLAP_HZ };
    crow.flap_phase = (crow.flap_phase + hz * TAU * dt) % TAU;
    flight_attitude(crow, dt);
    if flare {
        // Nose up into the flare, feet first.
        crow.pitch = ease(crow.pitch, -0.75, 6.0, dt);
    }
    if d < 0.4 && crow.pos.y - gy < 0.3 {
        crow.mode = Mode::Ground;
        crow.pos = Vec3::new(target.x, gy, target.y);
        crow.vel = Vec3::ZERO;
        crow.act = Act::Stand;
        crow.act_left = crow.rng.range(0.4, 1.2);
        crow.act_age = 0.0;
    }
}

/// A few steps of run and hard beats, climbing out, then back to the flock.
fn take_off(crow: &mut Crow, ground: &CrowGround, dt: f32) {
    if crow.delay > 0.0 {
        // Alert: head up, ready.
        crow.delay -= dt;
        crow.head_pitch = ease(crow.head_pitch, -0.2, 12.0, dt);
        crow.pitch = ease(crow.pitch, 0.0, 8.0, dt);
        crow.pos.y = ground.height(crow.pos.xz());
        return;
    }
    crow.act_age += dt;
    let t = crow.act_age;
    let fwd = Vec2::new(crow.yaw.sin(), crow.yaw.cos());
    let h = fwd * (2.5 + t * 4.0).min(CRUISE);
    crow.vel = Vec3::new(h.x, (3.4 - t * 1.2).max(1.0), h.y);
    crow.pos += crow.vel * dt;
    crow.beat = 1.0;
    crow.spread = ease(crow.spread, 1.0, 14.0, dt);
    crow.flap_phase = (crow.flap_phase + FLAP_HZ * 1.5 * TAU * dt) % TAU;
    crow.pitch = ease(crow.pitch, -0.55, 8.0, dt);
    crow.roll = ease(crow.roll, 0.0, 6.0, dt);
    crow.head_pitch = ease(crow.head_pitch, 0.3, 8.0, dt);
    crow.head_yaw = ease(crow.head_yaw, 0.0, 8.0, dt);
    if t > 1.4 {
        crow.mode = Mode::Air;
        crow.gliding = false;
        crow.bout = crow.rng.range(1.5, 3.5);
    }
}

/// Choose this bird's next thing to do on the ground.
fn begin_act(crow: &mut Crow, site: Vec2, size: usize) {
    let r = &mut crow.rng;
    let leash = 0.8 * (size as f32).sqrt() + 3.0;
    let away = crow.pos.xz().distance(site) > leash;
    let roll = r.unit();
    crow.act = if away {
        Act::Turn
    } else if roll < 0.38 {
        Act::Peck
    } else if roll < 0.62 {
        Act::Walk
    } else if roll < 0.80 {
        Act::Look
    } else if roll < 0.88 {
        Act::Hop
    } else if roll < 0.95 {
        Act::Turn
    } else {
        Act::Stand
    };
    crow.act_age = 0.0;
    crow.act_left = match crow.act {
        Act::Peck => PECK_S * r.range(2.0, 6.0).floor(),
        Act::Walk => r.range(1.2, 3.5),
        Act::Look => r.range(1.2, 3.0),
        Act::Hop => HOP_S,
        Act::Turn => 0.8,
        Act::Stand => r.range(0.4, 1.6),
    };
    match crow.act {
        Act::Walk => crow.turn = r.range(-0.7, 0.7),
        Act::Turn => {
            crow.turn = if away {
                heading_of(site - crow.pos.xz())
            } else {
                crow.yaw + r.range(0.6, 2.2) * if r.chance(0.5) { 1.0 } else { -1.0 }
            };
        }
        Act::Hop => {
            let yaw = crow.yaw + r.range(-0.5, 0.5);
            crow.yaw = yaw;
            crow.hop_from = crow.pos.xz();
            crow.hop_to = crow.hop_from + Vec2::new(yaw.sin(), yaw.cos()) * r.range(0.3, 0.6);
        }
        Act::Look => crow.head_target = r.range(-0.9, 0.9),
        _ => {}
    }
}

/// One peck: down, and back up. A bout is a whole number of them.
const PECK_S: f32 = 0.42;
const HOP_S: f32 = 0.36;
/// A strutting step, metres. Crows walk; they hop only to cover ground fast.
const STEP_M: f32 = 0.13;

/// Feeding: the current act, then the next.
fn walk_about(
    crow: &mut Crow,
    site: Vec2,
    size: usize,
    snapshot: &[(usize, Mode, Vec3, Vec3)],
    me: usize,
    ground: &CrowGround,
    dt: f32,
) {
    crow.act_age += dt;
    crow.act_left -= dt;
    crow.vel = Vec3::ZERO;
    let mut body_pitch = 0.05;
    let mut head_pitch = 0.1;
    let mut head_yaw = 0.0;
    let mut spread = 0.0;
    let mut lift = 0.0;
    crow.head_bob = 0.0;
    crow.roll = ease(crow.roll, 0.0, 8.0, dt);
    let fwd = Vec2::new(crow.yaw.sin(), crow.yaw.cos());
    let mut p = crow.pos.xz();

    match crow.act {
        Act::Peck => {
            let u = (crow.act_age % PECK_S) / PECK_S;
            let dip = if u < 0.45 { (u / 0.45 * PI).sin() } else { 0.0 };
            // Tipped forward over the feet, tail up, so the beak reaches the grass.
            body_pitch = 0.42 + dip * 0.25;
            head_pitch = 0.35 + dip;
        }
        Act::Walk => {
            let speed = 0.42 * crow.speed_scale;
            let next = p + fwd * speed * dt;
            if ground.feeding_ground(next) {
                p = next;
                crow.yaw += crow.turn * dt;
                crow.step_phase = (crow.step_phase + speed * dt / STEP_M * PI) % TAU;
                // The head is thrust forward and held while the body catches up.
                crow.head_bob = 0.018 * crow.step_phase.sin();
                crow.roll = 0.05 * crow.step_phase.sin();
                lift = 0.006 * (2.0 * crow.step_phase).sin().abs();
            } else {
                // The open ground ends here: stop, and choose again.
                crow.act_left = 0.0;
            }
            body_pitch = 0.14;
            head_pitch = 0.3;
        }
        Act::Hop => {
            let u = (crow.act_age / HOP_S).min(1.0);
            let to = crow.hop_from.lerp(crow.hop_to, u);
            if ground.feeding_ground(to) {
                p = to;
            }
            lift = 0.12 * (u * PI).sin();
            spread = 0.55 * (u * PI).sin();
            body_pitch = 0.1;
            crow.beat = 1.0;
            crow.flap_phase = (crow.flap_phase + 7.0 * TAU * dt) % TAU;
        }
        Act::Look => {
            // Snap to a side, hold, snap to the other.
            if crow.act_age % 0.8 < dt {
                crow.head_target = if crow.head_target > 0.0 {
                    crow.rng.range(-0.95, -0.3)
                } else {
                    crow.rng.range(0.3, 0.95)
                };
            }
            head_yaw = crow.head_target;
            head_pitch = -0.15;
            body_pitch = -0.02;
        }
        Act::Turn => {
            let d = angle_diff(crow.yaw, crow.turn);
            let step = d.clamp(-3.0 * dt, 3.0 * dt);
            crow.yaw += step;
            crow.step_phase = (crow.step_phase + step.abs() * 8.0) % TAU;
            lift = 0.004 * (2.0 * crow.step_phase).sin().abs();
            if d.abs() < 0.05 {
                crow.act_left = 0.0;
            }
        }
        Act::Stand => {
            head_yaw = crow.head_target * 0.4;
        }
    }

    // Two birds do not stand in one place.
    for (j, &(g, mode, q, _)) in snapshot.iter().enumerate() {
        if j != me && g == crow.group && mode == Mode::Ground {
            let d = p - q.xz();
            let l = d.length();
            if l > 1e-3 && l < 0.4 {
                p += d / l * (0.4 - l).min(0.3 * dt);
            }
        }
    }
    crow.pos = Vec3::new(p.x, ground.height(p) + lift, p.y);
    crow.pitch = ease(crow.pitch, body_pitch, 14.0, dt);
    crow.head_pitch = ease(crow.head_pitch, head_pitch, 22.0, dt);
    crow.head_yaw = ease(crow.head_yaw, head_yaw, 16.0, dt);
    crow.spread = ease(crow.spread, spread, 12.0, dt);
    if crow.act != Act::Hop {
        crow.beat = ease(crow.beat, 0.0, 8.0, dt);
    }
    if crow.act_left <= 0.0 {
        begin_act(crow, site, size);
    }
}

/// Write the body's and the parts' transforms.
fn draw(
    crow: &Crow,
    tf: &mut Transform,
    cell: &mut GridCell,
    grid: &Grid,
    parts: &mut Query<&mut Transform, (With<CrowPart>, Without<Crow>, Without<FreeCamera>)>,
) {
    let mut p = crow.pos;
    // The body rises on the downstroke.
    if crow.mode != Mode::Ground {
        p.y += 0.025 * crow.beat * crow.spread * -crow.flap_phase.cos();
    }
    let (c, local) = grid.translation_to_grid(p.as_dvec3());
    *cell = c;
    tf.translation = local;
    tf.rotation = Quat::from_rotation_y(crow.yaw)
        * Quat::from_rotation_x(crow.pitch)
        * Quat::from_rotation_z(crow.roll);

    if let Ok(mut head) = parts.get_mut(crow.parts.head) {
        head.translation = Vec3::from(CROW_NECK) + Vec3::Z * crow.head_bob;
        head.rotation =
            Quat::from_rotation_y(crow.head_yaw) * Quat::from_rotation_x(crow.head_pitch);
    }
    let pose = WingPose::FOLDED.lerp(wingbeat(crow.flap_phase, crow.beat), crow.spread);
    for (s, sign) in [(0usize, 1.0f32), (1, -1.0)] {
        let [x, y, z] = pose.shoulder();
        if let Ok(mut arm) = parts.get_mut(crow.parts.arm[s]) {
            arm.translation = Vec3::new(x * sign, y, z);
            // Mirroring across x flips the turns about y and z; the twist about
            // the span itself is the same on both sides.
            arm.rotation = Quat::from_rotation_y(pose.sweep * sign)
                * Quat::from_rotation_z(pose.raise * sign)
                * Quat::from_rotation_x(pose.twist);
        }
        if let Ok(mut hand) = parts.get_mut(crow.parts.hand[s]) {
            hand.translation = Vec3::new(CROW_ARM_M * sign, 0.0, 0.0);
            hand.rotation = Quat::from_rotation_y(pose.hand_sweep * sign)
                * Quat::from_rotation_z(pose.hand_raise * sign);
            hand.scale = Vec3::new(pose.hand_span, 1.0, 1.0);
        }
    }
}

/// Beyond this a crow is not heard, metres from the camera. Short on purpose:
/// the calls are a detail of the birds you can see, not a layer of their own.
pub const HEAR_M: f32 = 55.0;
/// Within this a caw plays at full level.
const NEAR_M: f32 = 6.0;

/// How loud a caw `d` metres away plays, 0–1: full within [`NEAR_M`], falling
/// off on a square to nothing at [`HEAR_M`]. Not the inverse square of free
/// field — a city absorbs and the mix is quiet anyway — but its shape: most of
/// the level goes in the first tens of metres.
#[must_use]
pub fn caw_level(d: f32) -> f32 {
    let k = ((HEAR_M - d) / (HEAR_M - NEAR_M)).clamp(0.0, 1.0);
    k * k
}

#[cfg(feature = "audio")]
pub use calls::*;

/// Now and then a crow near the camera calls: a series of one to three caws.
/// A flushed group gives a quicker, louder alarm as it goes.
///
/// The takes are single caws cut from CC0 recordings of carrion crows
/// (`assets/sounds/LICENSE.md`): `caw-1`–`4` one bird calling in Germany,
/// `caw-5`–`11` crows mobbing a fox in an English garden. A series keeps to
/// one recording, so a call does not change voice halfway through.
#[cfg(feature = "audio")]
mod calls {
    use super::*;
    use bevy::audio::{AudioPlayer, AudioSource, PlaybackMode, PlaybackSettings, Volume};

    /// The takes on disk, `assets/sounds/crows/caw-<n>.ogg`, and where the
    /// second recording's start.
    const TAKES: usize = 11;
    const SECOND_BIRD: usize = 4;
    /// Overall level: a detail under the city's beds, not over them.
    const CALL_GAIN: f32 = 0.35;
    /// A flushed group's alarm, relative to an ordinary call.
    const ALARM_GAIN: f32 = 1.3;

    #[derive(Resource)]
    pub struct CrowCalls {
        takes: Vec<Handle<AudioSource>>,
        /// Caws waiting for their moment: `(seconds left, take, volume, speed)`.
        queue: Vec<(f32, usize, f32, f32)>,
        next_in: f32,
        flushes_seen: u32,
        rng: Rng,
    }

    pub fn setup_crow_calls(mut commands: Commands, assets: Res<AssetServer>) {
        let takes = (1..=TAKES)
            .map(|n| assets.load(format!("sounds/crows/caw-{n}.ogg")))
            .collect();
        commands.insert_resource(CrowCalls {
            takes,
            queue: Vec::new(),
            next_in: 4.0,
            flushes_seen: 0,
            rng: Rng::new(0xCA_77),
        });
    }

    impl CrowCalls {
        /// Queue a series of `n` caws from one recording, `gap` seconds apart.
        fn series(&mut self, n: usize, gap: (f32, f32), volume: f32) {
            let (lo, hi) = if self.rng.chance(0.5) {
                (0, SECOND_BIRD)
            } else {
                (SECOND_BIRD, TAKES)
            };
            // One bird, one voice: the pitch is set for the series.
            let speed = self.rng.range(0.94, 1.06);
            let mut at = 0.05;
            let mut take = lo + (self.rng.unit() * (hi - lo) as f32) as usize;
            for _ in 0..n {
                self.queue.push((
                    at,
                    take.min(hi - 1),
                    volume * self.rng.range(0.85, 1.0),
                    speed,
                ));
                at += self.rng.range(gap.0, gap.1);
                take = lo + (take + 1 - lo) % (hi - lo);
            }
        }
    }

    /// Choose and play the calls. After [`drive_crows`], so it hears this
    /// frame's flush.
    #[allow(clippy::too_many_arguments)]
    pub fn play_crow_calls(
        mut commands: Commands,
        time: Res<Time>,
        layers: Res<crate::systems::data_layers::DataLayers>,
        prefs: Res<crate::resources::AppPrefs>,
        camera: AnchoredCamera,
        state: Option<Res<Crows>>,
        calls: Option<ResMut<CrowCalls>>,
        crows: Query<&Crow>,
        sources: Res<Assets<AudioSource>>,
    ) {
        let (Some(state), Some(mut calls)) = (state, calls) else {
            return;
        };
        let calls = &mut *calls;
        let dt = time.delta_secs();
        let gain = CALL_GAIN * prefs.effects_gain();

        // What is due.
        let mut due = Vec::new();
        calls.queue.retain_mut(|(left, take, volume, speed)| {
            *left -= dt;
            if *left > 0.0 {
                return true;
            }
            due.push((*take, *volume, *speed));
            false
        });
        for (take, volume, speed) in due {
            let Some(h) = calls.takes.get(take).filter(|h| sources.contains(*h)) else {
                continue;
            };
            commands.spawn((
                AudioPlayer(h.clone()),
                PlaybackSettings {
                    mode: PlaybackMode::Despawn,
                    volume: Volume::Linear(volume),
                    speed,
                    ..default()
                },
            ));
        }

        let Some((cell, tf)) = camera.get() else {
            return;
        };
        if !layers.birds_visible() {
            calls.flushes_seen = state.flushes;
            return;
        }
        let ear = anchored_position(cell, tf.translation);

        // A flush near enough to hear: the alarm, now.
        if state.flushes != calls.flushes_seen {
            calls.flushes_seen = state.flushes;
            let level = caw_level(state.flush_at.distance(ear));
            if level > 0.0 {
                let n = 2 + (calls.rng.unit() * 3.0) as usize;
                calls.series(n, (0.22, 0.4), level * gain * ALARM_GAIN);
                calls.next_in = calls.next_in.max(5.0);
                return;
            }
        }

        calls.next_in -= dt;
        if calls.next_in > 0.0 || !calls.queue.is_empty() {
            return;
        }
        let near: Vec<f32> = crows
            .iter()
            .filter(|c| c.mode() != Mode::Hidden)
            .map(|c| c.position().distance(ear))
            .filter(|d| *d < HEAR_M)
            .collect();
        if near.is_empty() {
            calls.next_in = 3.0;
            return;
        }
        let d = near[(calls.rng.unit() * near.len() as f32) as usize % near.len()];
        let n = 1 + (calls.rng.unit() * 3.0) as usize;
        calls.series(n, (0.5, 0.85), caw_level(d) * gain);
        calls.next_in = calls.rng.range(7.0, 20.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caw_is_loud_close_and_gone_out_of_earshot() {
        assert_eq!(caw_level(0.0), 1.0);
        assert_eq!(caw_level(NEAR_M), 1.0);
        assert_eq!(caw_level(HEAR_M), 0.0);
        assert_eq!(caw_level(HEAR_M * 2.0), 0.0);
        let mut last = 1.0;
        for d in 0..60 {
            let l = caw_level(d as f32);
            assert!(l <= last, "louder further off at {d} m");
            last = l;
        }
        assert!(
            caw_level(30.0) < 0.3,
            "half way out is quiet: {}",
            caw_level(30.0)
        );
    }

    #[test]
    fn steering_turns_at_a_bounded_rate_and_keeps_its_speed_bounded() {
        let vel = Vec3::new(0.0, 0.0, 10.0);
        // Asked to reverse, it turns by at most the rate in one step.
        let h = steer(vel, Vec2::new(0.0, -10.0), 1.0, 2.0, 0.1);
        let turned = angle_diff(heading_of(vel.xz()), heading_of(h)).abs();
        assert!((turned - 0.1).abs() < 1e-4, "{turned}");
        assert!((h.length() - 10.0).abs() < 1e-4);
        // Asked to stop, it slows by the acceleration only.
        let h = steer(vel, Vec2::ZERO, 1.0, 2.0, 0.5);
        assert!((h.length() - 9.0).abs() < 1e-4);
    }

    #[test]
    fn angles_wrap_the_short_way() {
        assert!((angle_diff(3.0, -3.0) - (TAU - 6.0)).abs() < 1e-5);
        assert!((angle_diff(-3.0, 3.0) + (TAU - 6.0)).abs() < 1e-5);
        assert!(angle_diff(0.2, 0.5) > 0.0);
    }

    #[test]
    fn a_group_lands_in_a_patch_it_can_stand_in() {
        let mut rng = Rng::new(1);
        for &size in &GROUP_SIZES {
            let r = 0.8 * (size as f32).sqrt() + 0.5;
            for _ in 0..200 {
                assert!(flock_slot(&mut rng, size).length() <= r + 1e-4);
            }
        }
    }

    #[test]
    fn the_generator_is_uniform_enough_and_repeatable() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        let xs: Vec<f32> = (0..4000).map(|_| a.unit()).collect();
        assert!(xs.iter().all(|x| (0.0..1.0).contains(x)));
        let mean = xs.iter().sum::<f32>() / xs.len() as f32;
        assert!((mean - 0.5).abs() < 0.03, "{mean}");
        assert_eq!(xs[17], (0..18).map(|_| b.unit()).last().unwrap());
    }
}
