//! What is on the other side of a door: the inside of every building near the
//! avatar, built from that building's design (`cartopolis_geo::building_design`,
//! `docs/notes/building-designs.md`).
//!
//! `lod22` draws a door on every entrance and `collision::Footprint::with_doors`
//! opens the collider there, so the avatar can walk in. Walking into a *shell* is
//! worse than being stopped by one: the exterior walls are back-face culled, so
//! from inside they are not there at all and the building reads as a hole in the
//! world with the street visible through it. This is the other half of the door.
//!
//! **Every building is built from a design, and most designs are generated.** A
//! building somebody has drawn in the atlas editor has its stored design
//! (`systems::building_designs`); every other one gets the design
//! `building_design::generate` lays out from the register — an inset room per
//! storey and one straight flight against the longest wall — which is exactly the
//! interior this module used to build by hand. One compiler for both is the point:
//! a drawn building and a generated one cannot come to behave differently.
//!
//! **It is a near-camera layer, and that is what makes it affordable.** A room is
//! only ever seen from inside it or through its own doorway, so this keeps
//! interiors for the buildings within [`INTERIOR_RADIUS`] and rebuilds when the
//! avatar has moved [`REBUILD_STEP_M`]. A few dozen buildings at a few dozen
//! triangles each, against the ~240,000 a single LoD2.2 cell carries — which is
//! why this can be one mesh, rebuilt outright, without going anywhere near the
//! churn that loses a mobile device. A drawn building is more, and is still a
//! rounding error.
//!
//! Three details are load-bearing, and all three live in the compiler now:
//!
//! * **The outer walls are inset** from the register's outline, so they stand
//!   *inside* the shell's own walls rather than in the same plane.
//! * **The doorway is cut out of the room's wall at the entrance**, and `lod22`
//!   opens the collider at the same points — for a drawn building because it
//!   takes its doors from the same design (`building_designs::with_design_doors`).
//! * **Nothing is double-sided.** Walls face into the room, floors up, ceilings
//!   down — so from outside a room is invisible and costs no fill. An inner wall
//!   has two faces because it has two sides.
//!
//! **The design's frame is not the world's.** A design is metres east and north
//! of its own origin; the world here runs +z *south*. [`DesignToWorld`] carries a
//! design across, and since it is a mirror it reverses every triangle on the way —
//! the alternative is a building whose floors are lit from beneath.

use std::sync::Arc;

use bevy::prelude::*;
use big_space::prelude::BigSpace;
use cartopolis_geo::building_design::{self as bd, BuildingDesign, Compiled};

use crate::components::HideInSpace;
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::resources::{AvatarState, MapAnchor};
use crate::systems::building_facts::BuildingFacts;
use crate::systems::collision::{BuildingColliders, Footprint};
use crate::systems::geo_nav::{lat_lng_to_world, world_to_lat_lng};
use crate::systems::map_stream::MapStream;

/// How far from the avatar a building still gets an interior, metres.
///
/// Generous enough that a room is always standing before its door is close enough
/// to walk through, and small enough that the whole layer is a rounding error.
const INTERIOR_RADIUS: f32 = 70.0;

/// How far the avatar must move before the set is rebuilt, metres. Well under
/// [`INTERIOR_RADIUS`] so a room is never wanted before it exists.
const REBUILD_STEP_M: f32 = 15.0;

/// How far above the level the avatar is standing on a surface may be and still
/// be stepped onto, metres.
///
/// This is what makes a stack of floors behave like floors: the ceiling over your
/// head is a surface directly above you and the storey above it is another, and
/// neither may pick you up. Only the flight rises continuously out of the floor
/// you are on, so only the flight can take you up — which is the whole point of
/// there being one.
pub const STEP_UP_M: f32 = 0.3;

/// Marker for the merged interior mesh.
#[derive(Component)]
pub struct InteriorMesh;

/// The standing set of interiors.
#[derive(Resource, Default)]
pub struct Interiors {
    entity: Option<Entity>,
    /// Where the avatar was when the set was last built, in anchored world XZ.
    built_at: Option<Vec2>,
    /// The `MapAnchor` generation the positions were computed against.
    anchor_generation: u32,
    /// Facts revision the set was built from.
    facts_len: usize,
    /// `building_designs::revision` the set was built from.
    designs_revision: u64,
    /// Interiors standing, for `--dump-state`.
    rooms: usize,
    /// Of which drawn by somebody rather than generated.
    designed: usize,
    material: Option<Handle<StandardMaterial>>,
}

impl Interiors {
    /// How many buildings have an interior standing.
    pub fn rooms(&self) -> usize {
        self.rooms
    }

    /// How many of them are from a stored design.
    pub fn designed(&self) -> usize {
        self.designed
    }
}

/// Keep an interior standing for every building near the avatar.
#[allow(clippy::too_many_arguments)]
pub fn update_interiors(
    mut commands: Commands,
    mut interiors: ResMut<Interiors>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut floors: ResMut<InteriorFloors>,
    mut colliders: ResMut<BuildingColliders>,
    facts: Res<BuildingFacts>,
    lod22: Res<crate::systems::lod22::Lod22Buildings>,
    terrain: Res<crate::systems::terrain::TerrainField>,
    anchor: Res<MapAnchor>,
    stream: Res<MapStream>,
    avatar: Res<AvatarState>,
    bigspace: Query<Entity, With<BigSpace>>,
) {
    let Some(geo) = anchor.geo else {
        return;
    };
    let Ok(root) = bigspace.single() else {
        return;
    };
    let here = Vec2::new(avatar.position.x, avatar.position.z);
    let (lat, lng) = world_to_lat_lng(geo, f64::from(here.x), f64::from(here.y));
    let (cx, cy) = cartopolis_core::world::lat_lng_to_tile(lat, lng, bd::DESIGN_ZOOM);
    crate::systems::building_designs::prefetch_around(cx, cy);

    // Rebuild on a real move, on a re-anchor, when the facts have grown, or when
    // a design has landed or changed. The facts are what make the first frames
    // right: the streamer lands its cells over several seconds, and a set built
    // from an empty index would otherwise stand until the avatar walked fifteen
    // metres. The designs are what make an edit show without walking away.
    let designs_revision = crate::systems::building_designs::revision();
    let moved = interiors
        .built_at
        .is_none_or(|was| was.distance(here) > REBUILD_STEP_M);
    let restreamed = interiors.facts_len != facts.len();
    let reanchored = interiors.anchor_generation != stream.anchor_generation;
    let redesigned = interiors.designs_revision != designs_revision;
    if !(moved || restreamed || reanchored || redesigned) {
        return;
    }
    interiors.built_at = Some(here);
    interiors.facts_len = facts.len();
    interiors.anchor_generation = stream.anchor_generation;
    interiors.designs_revision = designs_revision;

    let mut buf = WorldMesh::default();
    let mut built: Vec<Placed> = Vec::new();
    let mut walls: Vec<Footprint> = Vec::new();
    let mut designed = 0usize;
    for (id, building) in facts.near(lat, lng, f64::from(INTERIOR_RADIUS)) {
        if building.footprint.len() < 3 {
            continue;
        }
        let outline: Vec<Vec2> = building
            .footprint
            .iter()
            .map(|&(la, lo)| {
                let (x, z) = lat_lng_to_world(geo, la, lo);
                Vec2::new(x as f32, z as f32)
            })
            .collect();
        let stored = crate::systems::building_designs::get(id);
        let design = match &stored {
            Some(d) => Some(d.clone()),
            // A building with no entrance stays a solid mass (`lod22` keeps its
            // collider shut), so a room behind no door would be a room nobody
            // can reach.
            None if building.doors.is_empty() => None,
            // How tall the shell is, from the only lane that ever knows: the
            // height is on the LoD2.2 vertices and nowhere else. `None` while
            // that cell is still in flight, which reads as one storey — so a
            // building gains its upper floors when its shell lands, on the
            // rebuild the growing facts index triggers anyway.
            None => generated(
                id,
                &building.footprint,
                &building.doors,
                lod22.shell_height(id),
            ),
        };
        let Some(design) = design else { continue };
        if stored.is_some() && design.entrances().is_empty() {
            continue;
        }
        let to_world = DesignToWorld::new(geo, design.origin);
        let compiled = design.compile();
        // The wall colliders stand on the ground under the building, in the frame
        // the avatar's feet are measured in (terrain plus storey).
        let base = {
            let c = outline.iter().copied().sum::<Vec2>() / outline.len() as f32;
            terrain.at(c.x, c.y)
        };
        for seg in &compiled.walls {
            if let Some(fp) = Footprint::wall(to_world.point(seg.a), to_world.point(seg.b)) {
                walls.push(fp.with_extent(base + seg.bottom, base + seg.top));
            }
        }
        buf.append(&compiled, &to_world);
        if stored.is_some() {
            designed += 1;
        }
        built.push(Placed::new(
            Arc::from(id),
            outline,
            &compiled,
            &to_world,
            design.storeys.len(),
        ));
    }
    interiors.rooms = built.len();
    interiors.designed = designed;
    // Published before the mesh is: the support field and the geometry are the
    // same interiors and must never be a frame apart, or the avatar stands on a
    // floor that has just been rebuilt somewhere else.
    floors.rooms = built;
    colliders.set_rooms(walls);

    if let Some(old) = interiors.entity.take() {
        // `try_despawn`: the whole set is one entity and this system is the only
        // thing that spawns it, but a re-anchor teardown can retire it first and a
        // command outlives the entity it names (CLAUDE.md).
        commands.entity(old).try_despawn();
    }
    let Some(mesh) = buf.finish() else {
        return;
    };
    let material = interiors
        .material
        .get_or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: Color::WHITE,
                perceptual_roughness: 0.95,
                ..default()
            })
        })
        .clone();
    // Anchored to the BigSpace cell the avatar is in, like every other
    // world-placed mesh: the set is small and near, so one cell holds all of it.
    let (grid_cell, _) = big_space::prelude::Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD)
        .translation_to_grid(Vec3::new(here.x, 0.0, here.y).as_dvec3());
    let ox = grid_cell.x as f32 * WORLD_CELL_EDGE;
    let oz = grid_cell.z as f32 * WORLD_CELL_EDGE;
    let id = commands
        .spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material),
            Transform::from_translation(Vec3::new(-ox, 0.0, -oz)),
            grid_cell,
            ChildOf(root),
            InteriorMesh,
            HideInSpace,
        ))
        .id();
    interiors.entity = Some(id);
}

/// The design a building with no stored one gets: laid out from its outline, its
/// entrances and its shell's height, in a frame whose origin is the outline's
/// first corner.
fn generated(
    id: &str,
    footprint: &[(f64, f64)],
    doors: &[(f64, f64)],
    height: Option<f32>,
) -> Option<Arc<BuildingDesign>> {
    let origin = [footprint[0].0, footprint[0].1];
    let outline: Vec<[f32; 2]> = footprint
        .iter()
        .map(|&(la, lo)| bd::to_local(origin, la, lo))
        .collect();
    let doors: Vec<[f32; 2]> = doors
        .iter()
        .map(|&(la, lo)| bd::to_local(origin, la, lo))
        .collect();
    bd::generate(id, origin, &outline, height, &doors).map(Arc::new)
}

/// Carries a design's frame (metres east/north of its origin) into anchored world
/// XZ (+z south). Affine, measured at the origin: over one building that is exact
/// to well under a millimetre, and it is what lets a mesh of thousands of
/// vertices be carried across with a multiply-add each.
#[derive(Clone, Copy, Debug)]
pub struct DesignToWorld {
    origin: Vec2,
    east: Vec2,
    north: Vec2,
    /// The map reverses handedness (it does: north is −z), so every triangle's
    /// winding has to be reversed with it.
    mirrored: bool,
}

impl DesignToWorld {
    pub fn new(anchor: (f64, f64, u32), origin: [f64; 2]) -> Self {
        let at = |p: [f32; 2]| {
            let (lat, lng) = bd::to_lat_lng(origin, p);
            let (x, z) = lat_lng_to_world(anchor, lat, lng);
            Vec2::new(x as f32, z as f32)
        };
        let o = at([0.0, 0.0]);
        // Measured over a hundred metres and divided back, so the f32 the world
        // frame is kept in contributes nothing that matters.
        let east = (at([100.0, 0.0]) - o) / 100.0;
        let north = (at([0.0, 100.0]) - o) / 100.0;
        Self {
            origin: o,
            east,
            north,
            mirrored: east.perp_dot(north) < 0.0,
        }
    }

    pub fn point(&self, p: [f32; 2]) -> Vec2 {
        self.origin + self.east * p[0] + self.north * p[1]
    }

    /// A direction, renormalised.
    fn direction(&self, d: [f32; 2]) -> Vec2 {
        (self.east * d[0] + self.north * d[1]).normalize_or_zero()
    }
}

/// One building's interior, as the support field and `building_select` see it.
///
/// Held in the frame `update_interiors` builds in — anchored world XZ, exactly the
/// value `AvatarState::position` carries — so the floor you stand on and the
/// floor you can see are derived from one design and cannot disagree.
pub struct Placed {
    /// The BAG identificatie of the building this is the inside of.
    ///
    /// Carried so [`InteriorFloors::building_at`] can *name* the building the
    /// avatar is standing in, which is what `building_select` draws its glass
    /// around. A ring cannot name itself.
    id: Arc<str>,
    /// The building's own outline, uninset — the same ring
    /// `collision::Footprint` was built from.
    ///
    /// The design's outer walls stand inside it, so "am I in this room" and "am I
    /// in this building" disagree over a band the width of the wall.
    /// `building_at` asks the second one, so an avatar standing in a doorway —
    /// which is exactly that band — is already counted as inside, and the glass
    /// goes up as it crosses the wall rather than a step later.
    outline: Vec<Vec2>,
    floors: Vec<WorldFloor>,
    ramps: Vec<WorldRamp>,
    storeys: usize,
}

struct WorldFloor {
    y: f32,
    /// Outer ring then holes, per polygon.
    polygons: Vec<Vec<Vec<Vec2>>>,
}

struct WorldRamp {
    foot: Vec2,
    dir: Vec2,
    left: Vec2,
    run: f32,
    width: f32,
    y0: f32,
    rise: f32,
}

impl WorldRamp {
    fn height_at(&self, p: Vec2) -> Option<f32> {
        let rel = p - self.foot;
        let along = rel.dot(self.dir);
        let across = rel.dot(self.left);
        (along >= 0.0 && along <= self.run && across >= 0.0 && across <= self.width)
            .then(|| self.y0 + along / self.run * self.rise)
    }
}

impl Placed {
    fn new(
        id: Arc<str>,
        outline: Vec<Vec2>,
        compiled: &Compiled,
        to_world: &DesignToWorld,
        storeys: usize,
    ) -> Self {
        let ring =
            |r: &Vec<[f32; 2]>| -> Vec<Vec2> { r.iter().map(|&p| to_world.point(p)).collect() };
        let floors = compiled
            .floors
            .iter()
            .map(|f| WorldFloor {
                y: f.y,
                polygons: f
                    .polygons
                    .iter()
                    .map(|poly| poly.iter().map(ring).collect())
                    .collect(),
            })
            .collect();
        let ramps = compiled
            .ramps
            .iter()
            .filter_map(|r| {
                let rect = r.stair.rect()?;
                let foot = to_world.point(rect[0]);
                let along = to_world.point(rect[1]) - foot;
                let across = to_world.point(rect[3]) - foot;
                Some(WorldRamp {
                    foot,
                    dir: along.normalize_or_zero(),
                    left: across.normalize_or_zero(),
                    run: along.length(),
                    width: across.length(),
                    y0: r.y0,
                    rise: r.rise,
                })
            })
            .collect();
        Self {
            id,
            outline,
            floors,
            ramps,
            storeys,
        }
    }
}

/// Every interior standing, and the height of the surface under any point in one.
///
/// Written by [`update_interiors`] and read by the avatar's two movement paths —
/// which is what makes an upper storey a place rather than a picture. It is
/// deliberately not a set of colliders: `collision::BuildingColliders` answers
/// "may I move here", in two dimensions, and this answers "how high is the
/// ground", which is the question a stack of floors actually poses.
#[derive(Resource, Default)]
pub struct InteriorFloors {
    rooms: Vec<Placed>,
}

impl InteriorFloors {
    /// The highest interior surface under `p` that can be reached from `from`,
    /// metres above the ground plane; 0 when there is nothing there, which is the
    /// ordinary outdoors and the answer this whole layer used to give everywhere.
    ///
    /// `from` is the level the avatar is already standing at, and it is what makes
    /// this a floor rather than a lift: a surface more than [`STEP_UP_M`] above
    /// you is something to walk under, not something to be raised onto. Reaching
    /// the storey above is therefore only ever possible up the flight, whose ramp
    /// rises continuously out of the floor you are on.
    pub fn support(&self, p: Vec2, from: f32) -> f32 {
        let mut best = 0.0f32;
        let mut take = |level: f32| {
            if level <= from + STEP_UP_M && level > best {
                best = level;
            }
        };
        for room in &self.rooms {
            if !point_in_ring(&room.outline, p) {
                continue;
            }
            for f in &room.floors {
                if f.polygons.iter().any(|poly| {
                    poly.first().is_some_and(|outer| point_in_ring(outer, p))
                        && !poly[1..].iter().any(|h| point_in_ring(h, p))
                }) {
                    take(f.y);
                }
            }
            // The flight, as a ramp rather than as sixteen steps. The treads are
            // drawn because a ramp reads as a ramp, but walking a staircase one
            // riser at a time is a bob at every step and a foot through every
            // tread; the surface the avatar stands on is the line they lie on.
            for r in &room.ramps {
                if let Some(h) = r.height_at(p) {
                    take(h);
                }
            }
        }
        best
    }

    /// Storeys standing across the set, for `--dump-state`. One per room means
    /// nothing was found tall enough to divide.
    pub fn storeys(&self) -> usize {
        self.rooms.iter().map(|r| r.storeys).sum()
    }

    /// Which building `p` (anchored world XZ) is standing in, if any.
    ///
    /// **Tested against the building's outline, not the inset walls**, so the
    /// answer agrees with `collision::Footprint`, which is built from the same
    /// ring — and so the doorway itself counts as inside rather than as neither.
    ///
    /// It answers only for a building that has an interior, which is what makes
    /// it safe to key a see-through shell off: a sealed building is never
    /// reported, so its walls are never dissolved to show an inside it does not
    /// have.
    pub fn building_at(&self, p: Vec2) -> Option<&Arc<str>> {
        self.rooms
            .iter()
            .find(|r| point_in_ring(&r.outline, p))
            .map(|r| &r.id)
    }
}

/// The two things that decide how high the ground is, in one parameter.
///
/// Both movement systems were at Bevy's 16-parameter ceiling, and the floor is
/// one question with two answers in it: the storey you are standing on and the
/// tunnel you are standing in. Reading them together also makes it impossible to
/// consult one without the other, which is the bug this would otherwise invite —
/// a path that knows about upper floors and walks over an open tunnel.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Footing<'w> {
    pub floors: Res<'w, InteriorFloors>,
    pub tunnels: Res<'w, crate::systems::tunnels::Tunnels>,
    /// **Here so the floor agrees with the road that is drawn.** A tunnel's deck is
    /// metres below the ground sheet, and over an open cut the measured ground
    /// sheet already *is* the trench floor, so `map_geometry` lifts the drawn tunnel
    /// back by what AHN accounts for. Without the same correction here the avatar
    /// walks that far under the carriageway it can see.
    pub terrain: Res<'w, crate::systems::terrain::TerrainField>,
}

impl Footing<'_> {
    /// Advance the avatar's floor one frame. See [`step_floor_lift`].
    pub fn step(&self, avatar: &mut AvatarState, at: Vec2, dt: f32) -> f32 {
        step_floor_lift(&self.floors, &self.tunnels, &self.terrain, avatar, at, dt)
    }
}

/// Advance the avatar's interior floor level one frame, and answer where it now
/// is (metres above the ground plane).
///
/// Called from **both** movement paths with the frame each one already works in —
/// offline from the transform, online from the reconciled sim position — because
/// that is how the collider frame is chosen there too, and a floor derived in a
/// different frame from the walls around it would be a storey you fall through in
/// one run mode and hover over in the other.
///
/// Rising is immediate: a step up is a step, and the [`STEP_UP_M`] cap in
/// [`InteriorFloors::support`] has already refused anything that is not one.
/// Falling is not, because stepping off a landing is a fall — the avatar's own
/// `vertical_velocity` cannot express it, since in the sim's flat frame it is
/// standing on the ground the whole way down, so it is the ground that drops.
pub fn step_floor_lift(
    floors: &InteriorFloors,
    tunnels: &crate::systems::tunnels::Tunnels,
    terrain: &crate::systems::terrain::TerrainField,
    avatar: &mut AvatarState,
    at: Vec2,
    dt: f32,
) -> f32 {
    let target = support_level(floors, tunnels, terrain, at, avatar.floor_lift);
    if target >= avatar.floor_lift {
        avatar.floor_lift = target;
        avatar.floor_drop_speed = 0.0;
    } else {
        avatar.floor_drop_speed += cartopolis_core::motion::GRAVITY * dt;
        avatar.floor_lift = (avatar.floor_lift - avatar.floor_drop_speed * dt).max(target);
        if avatar.floor_lift <= target {
            avatar.floor_drop_speed = 0.0;
        }
    }
    avatar.floor_lift
}

/// The ground level under `at`, given the level the avatar is already standing
/// at: a building's storey above, a tunnel's carriageway below, or the ground.
///
/// **The ground wins wherever there is any.** That is what keeps the pavement over
/// a tunnel walkable, and it is not the step rule doing the work — a deck seven
/// metres down passes "no more than a step above you" trivially, so preferring a
/// deck whenever one is under the point drops the avatar through the road. What
/// separates the two cases is the *hole*: the approach at a entrance is a stretch
/// the ground has genuinely been taken away over
/// (`cartopolis_geo::tunnels::cut_ground`), and only there is the floor whatever
/// lies at the bottom of the cut. Walking down the ramp works because the ramp is
/// inside that hole; walking over the covered tunnel works because it is not.
///
/// Going back up is the rule that was already here. From the deck, the ground over
/// your head is more than [`STEP_UP_M`] above you and is refused exactly as an
/// upper storey is refused from the floor below it, so the only way out is the
/// ramp — which rises continuously, as the flight does.
fn support_level(
    floors: &InteriorFloors,
    tunnels: &crate::systems::tunnels::Tunnels,
    terrain: &crate::systems::terrain::TerrainField,
    at: Vec2,
    from: f32,
) -> f32 {
    // The deck as drawn, over the ground field it is measured against here.
    let deck = tunnels.road_under(at).map(|y| y - terrain.at(at.x, at.y));
    if !tunnels.over_opening(at) {
        let indoors = floors.support(at, from);
        if indoors <= from + STEP_UP_M {
            return indoors;
        }
    }
    // Over the opening, or under a roof that cannot be climbed back onto: the
    // bottom of the cut is the only floor there is. `from` where there is not
    // even that — the avatar is against a wall it cannot be moved through, and
    // holding its level beats dropping it through the world.
    deck.unwrap_or(from)
}

/// The ring a room's outer walls would stand on inside `outline`, or `None` when
/// the building is too small to hold a room at all.
///
/// Public because the *collider* is decided by it: a building with a room in it
/// is hollow, and one without is a solid mass the avatar is kept out of
/// (`collision::Footprint`). Both answers have to come from this one function or
/// the door opens onto a shell — which is a hole you see the street through, and
/// is worse than a door that does not open. Frame-agnostic, so it is asked in the
/// world frame directly.
pub fn room_ring(outline: &[Vec2]) -> Option<Vec<Vec2>> {
    let ring: Vec<[f32; 2]> = outline.iter().map(|p| [p.x, p.y]).collect();
    bd::room_ring(&ring).map(|r| r.into_iter().map(|p| Vec2::new(p[0], p[1])).collect())
}

/// Whether this outline has a room behind its door — see [`room_ring`].
pub fn can_hold_a_room(outline: &[Vec2]) -> bool {
    room_ring(outline).is_some()
}

/// Even-odd ray cast against a bare ring.
fn point_in_ring(ring: &[Vec2], p: Vec2) -> bool {
    let mut inside = false;
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (u, v) = (ring[i], ring[j]);
        if (u.y > p.y) != (v.y > p.y) {
            let x = u.x + (p.y - u.y) / (v.y - u.y) * (v.x - u.x);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// The merged mesh for every interior in the set, in anchored world XZ.
#[derive(Default)]
struct WorldMesh {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl WorldMesh {
    /// Carry one compiled design across into the world frame.
    fn append(&mut self, compiled: &Compiled, to_world: &DesignToWorld) {
        let m = &compiled.mesh;
        let base = self.positions.len() as u32;
        for p in &m.positions {
            let w = to_world.point([p[0], p[2]]);
            self.positions.push([w.x, p[1], w.y]);
        }
        for n in &m.normals {
            let h = to_world.direction([n[0], n[2]]) * Vec2::new(n[0], n[2]).length();
            self.normals.push([h.x, n[1], h.y]);
        }
        self.colors.extend_from_slice(&m.colors);
        for t in m.indices.chunks_exact(3) {
            if to_world.mirrored {
                self.indices.extend([base + t[0], base + t[2], base + t[1]]);
            } else {
                self.indices.extend([base + t[0], base + t[1], base + t[2]]);
            }
        }
    }

    fn finish(self) -> Option<Mesh> {
        if self.indices.is_empty() {
            return None;
        }
        let mut mesh = Mesh::new(
            bevy::render::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        mesh.insert_indices(bevy::render::mesh::Indices::U32(self.indices));
        Some(mesh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::terrain::TerrainField;
    use cartopolis_geo::building_design::{FLOOR_Y, STOREY_M};

    /// One straight tunnel 7 m down, 12 m wide, running east from the origin, with
    /// its last 30 m open to the sky — the shape of a entrance approach.
    fn one_bore() -> crate::systems::tunnels::Tunnels {
        use cartopolis_geo::tunnels::Tunnel;
        let mut tunnels = crate::systems::tunnels::Tunnels::default();
        let tunnel = Tunnel {
            centre: vec![[0.0, 0.0], [200.0, 0.0]],
            road_y: vec![0.0, -7.0],
            land_y: vec![0.0, 0.0],
            half_w: 6.0,
            clearance: 4.8,
            covered: vec![true; 2],
        };
        // The opening: a corridor over the first 30 m, where the rising deck
        // leaves no room for a roof.
        let opening = vec![
            [0.0, -6.5],
            [30.0, -6.5],
            [30.0, 6.5],
            [0.0, 6.5],
            [0.0, -6.5],
        ];
        tunnels.insert_cell(
            (0, 0),
            Vec2::ZERO,
            crate::systems::tunnels::CellTunnels {
                tunnels: vec![tunnel],
                openings: vec![opening],
            },
        );
        tunnels
    }

    /// The case that matters most, because getting it wrong drops people through
    /// the road: a tunnel under your feet is not a reason to leave the pavement.
    #[test]
    fn the_ground_over_a_covered_bore_is_still_the_floor() {
        let floors = InteriorFloors::default();
        let tunnels = one_bore();
        // 100 m along, the deck is 3.5 m down and roofed. Standing on the surface.
        let level = support_level(
            &floors,
            &tunnels,
            &TerrainField::default(),
            Vec2::new(100.0, 0.0),
            0.0,
        );
        assert_eq!(level, 0.0, "the pavement over a tunnel gave way");
    }

    /// …and the mirror of it, which is what makes the tunnel a place: from the
    /// deck, the ground overhead is not something you can be lifted onto.
    #[test]
    fn the_ground_over_your_head_cannot_pick_you_up() {
        let floors = InteriorFloors::default();
        let tunnels = one_bore();
        let level = support_level(
            &floors,
            &tunnels,
            &TerrainField::default(),
            Vec2::new(100.0, 0.0),
            -3.5,
        );
        assert!(
            level < -3.0,
            "standing in the tunnel, the floor became the road above: {level}"
        );
    }

    /// Over the opening the ground is a hole, so the floor is the bottom of the
    /// cut — which is how the avatar gets down the ramp at all.
    #[test]
    fn over_the_opening_the_floor_is_the_deck() {
        let floors = InteriorFloors::default();
        let tunnels = one_bore();
        let level = support_level(
            &floors,
            &tunnels,
            &TerrainField::default(),
            Vec2::new(20.0, 0.0),
            0.0,
        );
        assert!(
            (level - -0.7).abs() < 0.1,
            "the ramp should be 0.7 m down 20 m in, got {level}"
        );
    }

    /// Beside the carriageway, on ordinary ground, nothing has changed.
    #[test]
    fn beside_a_bore_the_floor_is_the_ground() {
        let floors = InteriorFloors::default();
        let tunnels = one_bore();
        assert_eq!(
            support_level(
                &floors,
                &tunnels,
                &TerrainField::default(),
                Vec2::new(100.0, 40.0),
                0.0
            ),
            0.0
        );
    }

    /// The anchor the frame tests place a building under: Groningen.
    const ANCHOR: (f64, f64, u32) = (53.2194, 6.5665, 14);

    /// A 6 × 12 m house stood at the anchor, three storeys tall, door on the
    /// south wall — placed through the same path `update_interiors` takes.
    fn house() -> (Placed, WorldMesh, DesignToWorld) {
        let origin = [53.2194, 6.5665];
        let outline = [[0.0, 0.0], [6.0, 0.0], [6.0, 12.0], [0.0, 12.0]];
        let design = bd::generate("p", origin, &outline, Some(9.8), &[[3.0, 0.0]]).unwrap();
        let to_world = DesignToWorld::new(ANCHOR, design.origin);
        let compiled = design.compile();
        let world_outline: Vec<Vec2> = outline.iter().map(|&p| to_world.point(p)).collect();
        let mut buf = WorldMesh::default();
        buf.append(&compiled, &to_world);
        (
            Placed::new(
                Arc::from("p"),
                world_outline,
                &compiled,
                &to_world,
                design.storeys.len(),
            ),
            buf,
            to_world,
        )
    }

    /// North in a design is −z in the world, and a metre is a metre.
    #[test]
    fn a_design_lands_the_right_way_up_and_the_right_size() {
        let to_world = DesignToWorld::new(ANCHOR, [53.2194, 6.5665]);
        let o = to_world.point([0.0, 0.0]);
        let n = to_world.point([0.0, 10.0]);
        let e = to_world.point([10.0, 0.0]);
        assert!(n.y < o.y, "north went south");
        assert!(e.x > o.x, "east went west");
        assert!((n.distance(o) - 10.0).abs() < 0.2, "{}", n.distance(o));
        assert!((e.distance(o) - 10.0).abs() < 0.2, "{}", e.distance(o));
        assert!(to_world.mirrored);
    }

    /// The mirror reverses handedness, so every triangle must have been reversed
    /// with it: a floor whose triangles wind the wrong way is culled from above
    /// and lit from beneath.
    #[test]
    fn every_triangle_still_faces_its_normal_after_the_mirror() {
        let (_, buf, _) = house();
        assert!(!buf.indices.is_empty());
        for t in buf.indices.chunks_exact(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| Vec3::from(buf.positions[i as usize]));
            let n = Vec3::from(buf.normals[t[0] as usize]);
            let face = (b - a).cross(c - a);
            if face.length() < 1e-6 {
                continue;
            }
            assert!(
                face.dot(n) > 0.0,
                "a triangle faces away from its normal {n:?} (face {face:?})"
            );
        }
    }

    /// The ceiling over your head and the storey above it are both surfaces
    /// directly above you, and neither may pick you up.
    #[test]
    fn standing_on_the_ground_floor_is_not_lifted_to_the_one_above() {
        let (room, _, to_world) = house();
        assert_eq!(room.storeys, 3);
        let floors = InteriorFloors { rooms: vec![room] };
        // In the middle of the room, clear of the stairs on the long wall.
        let middle = to_world.point([3.0, 6.0]);
        assert!((floors.support(middle, 0.0) - FLOOR_Y).abs() < 1e-4);
        assert!(
            floors.support(Vec2::new(-500.0, -500.0), 0.0) == 0.0,
            "outdoors"
        );
    }

    /// Walking the flight from the bottom arrives *at* the floor above, and the
    /// well it comes up through is not somewhere to stand.
    #[test]
    fn the_flight_carries_you_onto_the_floor_above() {
        let (room, _, _) = house();
        let ramp = &room.ramps[0];
        let (foot, dir, left, run, width) = (ramp.foot, ramp.dir, ramp.left, ramp.run, ramp.width);
        let floors = InteriorFloors { rooms: vec![room] };
        let mut at = FLOOR_Y;
        for i in 0..=60 {
            let t = i as f32 / 60.0;
            let p = foot + dir * (t * run) + left * (width * 0.5);
            let next = floors.support(p, at);
            assert!(
                next >= at - 1e-3,
                "the flight dropped from {at} to {next} at t={t}"
            );
            at = next;
        }
        let first = STOREY_M + FLOOR_Y;
        assert!(
            (at - first).abs() < 1e-3,
            "the top of the flight is {at}, not {first}"
        );
        // Standing on the first floor over the middle of the well, nothing holds
        // the avatar up there.
        let over_well = foot + dir * (run * 0.5) + left * (width * 0.5);
        assert!(floors.support(over_well, first) < first);
    }

    /// The doorway is inside the building as far as the glass is concerned.
    #[test]
    fn the_building_is_named_from_its_outline() {
        let (room, _, to_world) = house();
        let floors = InteriorFloors { rooms: vec![room] };
        assert_eq!(
            floors.building_at(to_world.point([3.0, 0.1])).map(|s| &**s),
            Some("p")
        );
        assert!(floors.building_at(to_world.point([3.0, -1.0])).is_none());
    }

    /// A drawn inner wall reaches the collider, in the world frame.
    #[test]
    fn a_drawn_wall_becomes_a_world_wall() {
        let origin = [53.2194, 6.5665];
        let outline = [[0.0, 0.0], [6.0, 0.0], [6.0, 12.0], [0.0, 12.0]];
        let mut design = bd::generate("p", origin, &outline, None, &[[3.0, 0.0]]).unwrap();
        design.storeys[0].walls.push(bd::Wall {
            id: "w".into(),
            a: [0.22, 6.0],
            b: [5.78, 6.0],
            thickness: 0.1,
            exterior: false,
            openings: Vec::new(),
        });
        let to_world = DesignToWorld::new(ANCHOR, design.origin);
        let compiled = design.compile();
        let seg = compiled.walls[0];
        let mut c = BuildingColliders::default();
        c.set_rooms(vec![Footprint::wall(
            to_world.point(seg.a),
            to_world.point(seg.b),
        )
        .unwrap()
        .with_extent(seg.bottom, seg.top)]);
        let south = to_world.point([3.0, 5.5]);
        let north = to_world.point([3.0, 6.5]);
        let body = crate::systems::collision::Body::new(FLOOR_Y, FLOOR_Y);
        assert_eq!(
            c.slide(south, north, body),
            south,
            "walked through the wall"
        );
    }
}
