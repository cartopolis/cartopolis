//! What is on the other side of a door: a ground-floor room for each building
//! near the avatar.
//!
//! `lod22` draws a door on every entrance the register lists and
//! `collision::Footprint::with_doors` opens the collider there, so the avatar can
//! walk in. Walking into a *shell* is worse than being stopped by one: the
//! exterior walls are back-face culled, so from inside they are not there at all
//! and the building reads as a hole in the world with the street visible through
//! it. This is the other half of the door.
//!
//! **It is a near-camera layer, and that is what makes it affordable.** A room is
//! only ever seen from inside it or through its own doorway, so there is no
//! reason to hold one for a city: this keeps rooms for the buildings within
//! [`INTERIOR_RADIUS`] and rebuilds when the avatar has moved
//! [`REBUILD_STEP_M`]. Around fifty buildings at a few dozen triangles each is
//! ~1,500 triangles standing at any moment, against the ~240,000 a single LoD2.2
//! cell carries — which is why this can be one mesh, rebuilt outright, without
//! going anywhere near the churn that loses a mobile device.
//!
//! **A room is one storey and no more.** The register says how many addressable
//! units a building holds, never where its floors are; 3DBAG's storey count is not
//! in the tree the client streams. So a room is the ground floor at a nominal
//! ceiling height, and what is above that ceiling is the shell, unchanged. That
//! is honest — it is a room you can stand in — where a full-height empty volume
//! would be a sixteen-metre shaft with a front door, which is not a thing.
//!
//! Three details are load-bearing:
//!
//! * **The walls are inset** from the register's outline, so they stand *inside*
//!   the shell's own walls rather than in the same plane, where the two would
//!   z-fight the length of every room.
//! * **The doorway is cut out of the room's wall too**, at the same points the
//!   collider opens at. A room whose wall crossed its own door would seal the
//!   avatar in behind a surface they had just walked through.
//! * **Nothing here is double-sided.** The walls face inward, the floor up, the
//!   ceiling down — so from outside a room is invisible and costs no fill, which
//!   is the state it is in for all but a few seconds of a session.

use std::sync::Arc;

use bevy::prelude::*;
use big_space::prelude::BigSpace;

use crate::components::HideInSpace;
use crate::grid::{WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD};
use crate::resources::{AvatarState, MapAnchor};
use crate::systems::building_facts::BuildingFacts;
use crate::systems::collision::DOOR_HALF_WIDTH;
use crate::systems::geo_nav::{lat_lng_to_world, world_to_lat_lng};
use crate::systems::map_stream::MapStream;

/// How far from the avatar a building still gets a room, metres.
///
/// Generous enough that a room is always standing before its door is close enough
/// to walk through, and small enough that the whole layer is a rounding error.
const INTERIOR_RADIUS: f32 = 70.0;

/// How far the avatar must move before the set is rebuilt, metres. Well under
/// [`INTERIOR_RADIUS`] so a room is never wanted before it exists.
const REBUILD_STEP_M: f32 = 15.0;

/// Floor-to-ceiling height, metres. A nominal Dutch storey.
const CEILING_M: f32 = 2.9;

/// Floor-to-floor height, metres — the ceiling plus the slab and joists over it.
/// This is the rise one flight climbs and the pitch the storeys stack at.
const STOREY_M: f32 = 3.2;

/// Most storeys stacked in one building. Nothing in Groningen's centre cell
/// reaches it (the tallest shell there divides into seven), but the datum comes
/// from a survey and a survey can carry a chimney or a mast on a building's
/// record; a tower block of rooms nobody will enter is not worth the triangles.
const MAX_STOREYS: usize = 8;

/// Horizontal length of one flight, metres. With [`STOREY_M`] over it that is a
/// 43° pitch — steep, and correct: a Dutch staircase is steep because the houses
/// are narrow, and a shallower flight simply does not fit in the rooms this layer
/// is mostly built over.
const STAIR_RUN_M: f32 = 3.4;

/// Width of a flight, metres.
const STAIR_WIDTH_M: f32 = 0.95;

/// How much of a wall must be left over at each end of a flight, metres. Without
/// it a staircase can be laid corner to corner along a wall exactly its own
/// length, which puts the top landing in the corner of the room.
const STAIR_END_CLEAR_M: f32 = 0.4;

/// Treads in one flight. [`STAIR_RUN_M`] and [`STOREY_M`] over this are a
/// 21 cm going and a 20 cm rise.
const STAIR_STEPS: usize = 16;

/// How far above the level the avatar is standing on a surface may be and still
/// be stepped onto, metres.
///
/// This is what makes a stack of floors behave like floors: the ceiling over your
/// head is a surface directly above you and the storey above it is another, and
/// neither may pick you up. Only the flight rises continuously out of the floor
/// you are on, so only the flight can take you up — which is the whole point of
/// there being one.
const STEP_UP_M: f32 = 0.3;

/// How far the room's walls stand inside the register's outline, metres —
/// roughly a real wall's thickness, and enough that the inner surface can never
/// land in the same plane as the shell's outer one.
const WALL_INSET_M: f32 = 0.22;

/// The floor's height above the ground plane, metres. Clear of the whole coplanar
/// ground stack (paving, roads, fills), which lives inside a centimetre of zero.
const FLOOR_Y: f32 = 0.06;

/// Linear RGB for the three surfaces. Pale plaster, a warmer floor, a ceiling
/// lighter than the walls — the ordinary way a room reads, and enough separation
/// to tell the three apart under one flat interior light.
const WALL_COLOR: [f32; 4] = [0.62, 0.60, 0.57, 1.0];
const FLOOR_COLOR: [f32; 4] = [0.40, 0.33, 0.27, 1.0];
const CEILING_COLOR: [f32; 4] = [0.72, 0.71, 0.69, 1.0];

/// Marker for the merged interior mesh.
#[derive(Component)]
pub struct InteriorMesh;

/// The standing set of rooms.
#[derive(Resource, Default)]
pub struct Interiors {
    entity: Option<Entity>,
    /// Where the avatar was when the set was last built, in anchored world XZ.
    built_at: Option<Vec2>,
    /// The `MapAnchor` generation the positions were computed against.
    anchor_generation: u32,
    /// Facts revision the set was built from.
    facts_len: usize,
    /// Rooms standing, for `--dump-state`.
    rooms: usize,
    material: Option<Handle<StandardMaterial>>,
}

impl Interiors {
    /// How many ground-floor rooms are standing.
    pub fn rooms(&self) -> usize {
        self.rooms
    }
}

/// Keep a room standing for every building near the avatar.
pub fn update_interiors(
    mut commands: Commands,
    mut interiors: ResMut<Interiors>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut floors: ResMut<InteriorFloors>,
    facts: Res<BuildingFacts>,
    lod22: Res<crate::systems::lod22::Lod22Buildings>,
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

    // Rebuild on a real move, on a re-anchor, or when the facts have grown. The
    // last is what makes the first frames right: the streamer lands its cells over
    // several seconds, and a set built from an empty index would otherwise stand
    // until the avatar walked fifteen metres.
    let moved = interiors
        .built_at
        .is_none_or(|was| was.distance(here) > REBUILD_STEP_M);
    let restreamed = interiors.facts_len != facts.len();
    let reanchored = interiors.anchor_generation != stream.anchor_generation;
    if !(moved || restreamed || reanchored) {
        return;
    }
    interiors.built_at = Some(here);
    interiors.facts_len = facts.len();
    interiors.anchor_generation = stream.anchor_generation;

    let mut buf = RoomMesh::default();
    let mut built: Vec<Room> = Vec::new();
    let (lat, lng) = world_to_lat_lng(geo, f64::from(here.x), f64::from(here.y));
    for (id, building) in facts.near(lat, lng, f64::from(INTERIOR_RADIUS)) {
        if building.footprint.len() < 3 || building.doors.is_empty() {
            continue;
        }
        let ring: Vec<Vec2> = building
            .footprint
            .iter()
            .map(|&(la, lo)| {
                let (x, z) = lat_lng_to_world(geo, la, lo);
                Vec2::new(x as f32, z as f32)
            })
            .collect();
        let doors: Vec<Vec2> = building
            .doors
            .iter()
            .map(|&(la, lo)| {
                let (x, z) = lat_lng_to_world(geo, la, lo);
                Vec2::new(x as f32, z as f32)
            })
            .collect();
        // How tall the shell is, from the only lane that ever knows: the height
        // is on the LoD2.2 vertices and nowhere else. `None` while that cell is
        // still in flight, which reads as one storey — so a building gains its
        // upper floors when its shell lands, on the rebuild the growing facts
        // index triggers anyway.
        let Some(room) = plan_room(Arc::from(id), &ring, lod22.shell_height(id)) else {
            continue;
        };
        build_room(&mut buf, &room, &doors);
        built.push(room);
    }
    interiors.rooms = built.len();
    // Published before the mesh is: the support field and the geometry are the
    // same rooms and must never be a frame apart, or the avatar stands on a floor
    // that has just been rebuilt somewhere else.
    floors.rooms = built;

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

/// A building's interior, as the support field and the mesh builder both see it.
///
/// Held in the same frame `update_interiors` builds in — anchored world XZ,
/// exactly the value `AvatarState::position` carries — so the floor you stand on
/// and the floor you can see are derived from one ring and cannot disagree.
pub struct Room {
    /// The BAG identificatie of the building this room is inside.
    ///
    /// Carried so [`InteriorFloors::building_at`] can *name* the building the
    /// avatar is standing in, which is what `building_select` draws its glass
    /// around. A ring cannot name itself.
    id: Arc<str>,
    /// The building's own outline, uninset — the same ring
    /// `collision::Footprint` was built from.
    ///
    /// The room's walls stand [`WALL_INSET_M`] inside it, so "am I in this room"
    /// and "am I in this building" disagree over a 22 cm band. `building_at` asks
    /// the second one, so an avatar standing in a doorway — which is exactly that
    /// band — is already counted as inside, and the glass goes up as it crosses
    /// the wall rather than a step later.
    outline: Vec<Vec2>,
    /// The inset outline: where the room's walls stand.
    ring: Vec<Vec2>,
    /// How many storeys are stacked in it, at least one.
    storeys: usize,
    /// The flight that reaches them, absent in a building too small to hold one
    /// (in which case `storeys` is 1 — a floor nobody can reach is not a floor).
    stair: Option<Stair>,
}

/// One straight flight, repeated identically on every storey it serves.
///
/// Straight, and against a wall, because those two together are what let the same
/// footprint stack: a flight that wandered would need its own stairwell on each
/// floor and its own answer for where the one above starts.
#[derive(Clone, Copy)]
struct Stair {
    /// The foot of the flight, on the wall line, at the edge of its width.
    foot: Vec2,
    /// Unit vector of ascent, along the wall.
    dir: Vec2,
    /// Unit vector across the flight, pointing into the room.
    side: Vec2,
    /// Horizontal length of the flight, metres.
    run: f32,
}

impl Stair {
    /// Where a point lies on the flight: `(fraction of the run, metres across)`,
    /// or `None` if it is not over the flight at all.
    fn local(&self, p: Vec2) -> Option<(f32, f32)> {
        let rel = p - self.foot;
        let along = rel.dot(self.dir);
        let across = rel.dot(self.side);
        (along >= 0.0 && along <= self.run && across >= 0.0 && across <= STAIR_WIDTH_M)
            .then(|| (along / self.run, across))
    }

    /// The four corners of the flight's footprint, anticlockwise from the foot at
    /// the wall. This is both the stairwell cut out of every floor above and the
    /// area the support field ramps over, which is the point of there being one.
    fn rect(&self) -> [Vec2; 4] {
        let w = self.side * STAIR_WIDTH_M;
        let r = self.dir * self.run;
        [self.foot, self.foot + r, self.foot + r + w, self.foot + w]
    }
}

/// Every room standing, and the height of the surface under any point in one.
///
/// Written by [`update_interiors`] and read by the avatar's two movement paths —
/// which is what makes an upper storey a place rather than a picture. It is
/// deliberately not a set of colliders: `collision::BuildingColliders` answers
/// "may I move here", in two dimensions, and this answers "how high is the
/// ground", which is the question a stack of floors actually poses.
#[derive(Resource, Default)]
pub struct InteriorFloors {
    rooms: Vec<Room>,
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
        for room in &self.rooms {
            if !point_in_ring(&room.ring, p) {
                continue;
            }
            let on_stair = room.stair.and_then(|s| s.local(p));
            for k in 0..room.storeys {
                let level = storey_floor_y(k);
                // A floor with the stairwell cut out of it is not a floor where
                // the hole is — that hole is how you came up, and standing on it
                // would be standing on the gap.
                if k > 0 && on_stair.is_some() {
                    continue;
                }
                if level <= from + STEP_UP_M && level > best {
                    best = level;
                }
            }
            // The flight, as a ramp rather than as sixteen steps. The treads are
            // drawn because a ramp reads as a ramp, but walking a staircase one
            // riser at a time is a bob at every step and a foot through every
            // tread; the surface the avatar stands on is the line they lie on.
            if let Some((t, _)) = on_stair {
                for k in 0..room.storeys.saturating_sub(1) {
                    let level = storey_floor_y(k) + t * STOREY_M;
                    if level <= from + STEP_UP_M && level > best {
                        best = level;
                    }
                }
            }
        }
        best
    }

    /// Storeys standing across the set, for `--dump-state`. One per room is the
    /// old behaviour, so a reading equal to the room count means nothing was
    /// found tall enough to divide.
    pub fn storeys(&self) -> usize {
        self.rooms.iter().map(|r| r.storeys).sum()
    }

    /// Which building `p` (anchored world XZ) is standing in, if any.
    ///
    /// **Tested against the building's outline, not the room's inset walls**, so
    /// the answer agrees with `collision::Footprint`, which is built from the same
    /// ring — and so the doorway itself, a 22 cm band between the two, counts as
    /// inside rather than as neither.
    ///
    /// It answers only for a building that has a *room* in it, which is what makes
    /// it safe to key a see-through shell off: a sealed building is never reported,
    /// so its walls are never dissolved to show an interior it does not have.
    pub fn building_at(&self, p: Vec2) -> Option<&Arc<str>> {
        self.rooms
            .iter()
            .find(|r| point_in_ring(&r.outline, p))
            .map(|r| &r.id)
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

/// The floor level of storey `k`, metres above the ground plane.
fn storey_floor_y(k: usize) -> f32 {
    k as f32 * STOREY_M + FLOOR_Y
}

/// How many storeys fit inside a shell `height_m` metres tall.
///
/// `floor`, not `round`, and against the eaves datum rather than the ridge
/// (`lod22::shell_datum_height`): the whole stack has to fit *under* the roof,
/// because a room whose ceiling pokes out through the tiles is worse than a
/// building with one room in it. The top ceiling lands at `n·STOREY_M − 0.3`,
/// which is under the datum by construction whatever the building is.
///
/// `None` — a shell whose LoD2.2 cell has not landed — is one storey, which is
/// what this layer built before it could ask.
fn storeys_for(height_m: Option<f32>) -> usize {
    let Some(h) = height_m else {
        return 1;
    };
    ((h / STOREY_M).floor() as usize).clamp(1, MAX_STOREYS)
}

/// The ring a room's walls would stand on inside `outline`, or `None` when the
/// building is too small to hold a room at all.
///
/// Public because the *collider* is decided by it: a building with a room in it
/// is hollow, and one without is a solid mass the avatar is kept out of
/// (`collision::Footprint`). Both answers have to come from this one function or
/// the door opens onto a shell — which is a hole you see the street through, and
/// is worse than a door that does not open.
pub fn room_ring(outline: &[Vec2]) -> Option<Vec<Vec2>> {
    let inset = inset_ring(outline, WALL_INSET_M);
    (inset.len() >= 3 && ring_area(&inset) >= MIN_ROOM_M2).then_some(inset)
}

/// Whether this outline has a room behind its door — see [`room_ring`].
pub fn can_hold_a_room(outline: &[Vec2]) -> bool {
    room_ring(outline).is_some()
}

/// Lay out one building's interior: inset the outline, and find somewhere to put
/// the stairs. `None` if the outline is too small to stand in.
fn plan_room(id: Arc<str>, ring: &[Vec2], height_m: Option<f32>) -> Option<Room> {
    let inset = room_ring(ring)?;
    let wanted = storeys_for(height_m);
    let stair = (wanted > 1).then(|| find_stair(&inset)).flatten();
    Some(Room {
        id,
        outline: ring.to_vec(),
        // A building with nowhere to put a flight gets its ground floor and
        // nothing else. The alternative — stacking the storeys anyway — is rooms
        // that can only be reached by flying, which is not what a floor is.
        storeys: if stair.is_some() { wanted } else { 1 },
        stair,
        ring: inset,
    })
}

/// Put a straight flight against the longest wall of a room, or `None` if none of
/// them can hold one.
///
/// The longest wall because that is the one most likely to have the run free, and
/// against a wall because a flight in the middle of a room needs a balustrade on
/// both sides and a reason. Which end it starts from is fixed (centred on the
/// wall) rather than chosen, so the answer is the same every rebuild — a
/// staircase that moved when you walked fifteen metres would be a poltergeist.
fn find_stair(ring: &[Vec2]) -> Option<Stair> {
    let n = ring.len();
    let mut best: Option<(f32, Stair)> = None;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let len = (b - a).length();
        if len < STAIR_RUN_M + 2.0 * STAIR_END_CLEAR_M {
            continue;
        }
        let dir = (b - a) / len;
        let Some(side) = inward_normal(ring, a, b) else {
            continue;
        };
        let foot = a + dir * ((len - STAIR_RUN_M) * 0.5);
        let stair = Stair {
            foot,
            dir,
            side,
            run: STAIR_RUN_M,
        };
        // The room has to be deep enough here to take the flight, not merely long
        // enough along the wall: a corridor five metres long and one wide has a
        // wall that passes the length test and no room for a staircase at all.
        if !stair.rect().iter().all(|&c| point_in_ring(ring, c)) {
            continue;
        }
        if best.is_none_or(|(l, _)| len > l) {
            best = Some((len, stair));
        }
    }
    best.map(|(_, s)| s)
}

/// Build one room's storeys into `buf`.
fn build_room(buf: &mut RoomMesh, room: &Room, doors: &[Vec2]) {
    let ring = &room.ring;
    // The floor of every storey above the ground has the stairwell cut out of it,
    // and so does the ceiling under it — a stairwell capped by the ceiling of the
    // room it rises out of is a flight into a lid.
    let notched = room
        .stair
        .filter(|_| room.storeys > 1)
        .and_then(|s| notch_ring(ring, &s));
    for k in 0..room.storeys {
        let floor_y = storey_floor_y(k);
        for i in 0..ring.len() {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            let Some(inward) = inward_normal(ring, a, b) else {
                continue;
            };
            // Only the ground floor is cut for a door. The register's entrances
            // are at street level and nothing above it opens onto anything.
            let pieces = if k == 0 {
                segments_between_doors(a, b, doors)
            } else {
                vec![(a, b)]
            };
            for (from, to) in pieces {
                buf.wall(from, to, inward, floor_y, floor_y + CEILING_M);
            }
        }
        let floor_ring = if k == 0 {
            ring.as_slice()
        } else {
            notched.as_deref().unwrap_or(ring)
        };
        let ceiling_ring = if k + 1 == room.storeys {
            ring.as_slice()
        } else {
            notched.as_deref().unwrap_or(ring)
        };
        buf.slab(floor_ring, floor_y, FLOOR_COLOR, true);
        buf.slab(ceiling_ring, floor_y + CEILING_M, CEILING_COLOR, false);
    }
    if let Some(stair) = room.stair {
        for k in 0..room.storeys.saturating_sub(1) {
            buf.flight(&stair, storey_floor_y(k));
        }
    }
}

/// The ring with the stairwell taken out of it.
///
/// The flight stands against a wall, so the well it needs is a **bite out of the
/// boundary** rather than an island in the middle of the slab: the ring is walked
/// as before until it reaches the wall the stairs are on, then detours around
/// them and carries on. That keeps the result a simple polygon, which is the
/// whole reason for putting the flight against a wall in the first place — an
/// interior hole would have to be a separate ring, strictly inside, and a
/// stairwell that stops five centimetres short of the wall leaves a nib of floor
/// hanging over it.
///
/// `None` when the flight is not on any of this ring's edges, which cannot happen
/// for a stair this ring produced and is the honest answer if one ever arrives
/// from somewhere else.
fn notch_ring(ring: &[Vec2], stair: &Stair) -> Option<Vec<Vec2>> {
    let n = ring.len();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let len = (b - a).length();
        if len <= f32::EPSILON {
            continue;
        }
        let dir = (b - a) / len;
        // The flight's own wall: the one it points along and sits on.
        if dir.dot(stair.dir) < 0.999 || (stair.foot - a).perp_dot(dir).abs() > 0.01 {
            continue;
        }
        let t0 = (stair.foot - a).dot(dir);
        let corners = stair.rect();
        let mut out = Vec::with_capacity(n + 4);
        for (j, &p) in ring.iter().enumerate() {
            out.push(p);
            if j == i {
                // Anticlockwise around the well, from the near corner on the wall
                // to the far one, so the detour keeps the ring's own direction.
                out.push(a + dir * t0);
                out.push(corners[3]);
                out.push(corners[2]);
                out.push(a + dir * (t0 + stair.run));
            }
        }
        return Some(out);
    }
    None
}
/// Smallest room worth building, square metres — below this the outline is a
/// porch or a bin store and the avatar could not stand in it anyway.
const MIN_ROOM_M2: f32 = 4.0;

/// Split a wall segment around any doorway that falls on it.
///
/// The gaps are the same points the collider opens at, so the hole in the wall
/// and the hole in the collision are the same hole. They have to be: a room whose
/// wall crossed its own door would seal the avatar in behind a surface they had
/// just walked through.
fn segments_between_doors(a: Vec2, b: Vec2, doors: &[Vec2]) -> Vec<(Vec2, Vec2)> {
    let ab = b - a;
    let len = ab.length();
    if len <= f32::EPSILON {
        return Vec::new();
    }
    let dir = ab / len;
    // Each door that lies near this segment becomes a (start, end) gap along it.
    let mut gaps: Vec<(f32, f32)> = Vec::new();
    for &d in doors {
        let t = (d - a).dot(dir);
        let along = (a + dir * t.clamp(0.0, len)).distance(d);
        // Only a door actually against this wall cuts it. A door on the far side
        // of the building projects onto this segment too, and cutting on that
        // would open a hole in a wall nobody walks through.
        if along > DOOR_HALF_WIDTH * 2.0 {
            continue;
        }
        let (from, to) = (t - DOOR_HALF_WIDTH, t + DOOR_HALF_WIDTH);
        if to > 0.0 && from < len {
            gaps.push((from.max(0.0), to.min(len)));
        }
    }
    gaps.sort_by(|x, y| x.0.total_cmp(&y.0));

    let mut out = Vec::new();
    let mut at = 0.0f32;
    for (from, to) in gaps {
        if from > at {
            out.push((a + dir * at, a + dir * from));
        }
        at = at.max(to);
    }
    if at < len {
        out.push((a + dir * at, b));
    }
    out
}

/// Move every edge of a ring inward by `by` metres.
///
/// Edge offset with a corner join, not a vertex push: pushing each vertex along
/// its own bisector collapses a thin building into a bow tie, and these are Dutch
/// canal houses. Where the offset edges no longer intersect the corner is simply
/// dropped, which turns a sliver into a shorter wall rather than into a knot.
fn inset_ring(ring: &[Vec2], by: f32) -> Vec<Vec2> {
    let n = ring.len();
    let mut area = 0.0;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        area += a.x * b.y - b.x * a.y;
    }
    // Inward is the side the ring's own winding says it is.
    let sign = if area > 0.0 { 1.0 } else { -1.0 };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = ring[(i + n - 1) % n];
        let cur = ring[i];
        let next = ring[(i + 1) % n];
        let n0 = edge_normal(prev, cur) * sign * by;
        let n1 = edge_normal(cur, next) * sign * by;
        // The corner of the two offset edges. Their bisector is the average of
        // the two normals, scaled by how sharp the corner is.
        let bis = n0 + n1;
        if bis.length_squared() <= f32::EPSILON {
            continue;
        }
        let half = bis.normalize();
        let cos = half.dot(n0.normalize_or_zero());
        if cos.abs() < 0.2 {
            continue; // a spike; dropping it shortens the wall instead of knotting it
        }
        out.push(cur + half * (by / cos));
    }
    out
}

/// The normal of an edge that points into the room, by asking.
///
/// A step off the wall either lands inside the ring or outside it, which is true
/// on any winding and any shape — where a rule derived from the signed area is
/// only true on the winding it was reasoned about.
fn inward_normal(ring: &[Vec2], a: Vec2, b: Vec2) -> Option<Vec2> {
    /// How far off the wall the probe is taken. Under the wall inset, so it
    /// cannot step through the far side of a thin room.
    const PROBE_M: f32 = 0.05;
    let n = edge_normal(a, b);
    if n == Vec2::ZERO {
        return None;
    }
    let mid = (a + b) * 0.5;
    Some(if point_in_ring(ring, mid + n * PROBE_M) {
        n
    } else {
        -n
    })
}

/// Even-odd ray cast against a bare ring.
fn point_in_ring(ring: &[Vec2], p: Vec2) -> bool {
    let mut inside = false;
    let n = ring.len();
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

/// The left-hand normal of an edge, unit length.
fn edge_normal(a: Vec2, b: Vec2) -> Vec2 {
    let d = b - a;
    let len = d.length();
    if len <= f32::EPSILON {
        return Vec2::ZERO;
    }
    Vec2::new(-d.y, d.x) / len
}

fn ring_area(ring: &[Vec2]) -> f32 {
    let mut a = 0.0;
    for i in 0..ring.len() {
        let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
        a += p.x * q.y - q.x * p.y;
    }
    a.abs() * 0.5
}

/// The merged mesh for every room in the set.
#[derive(Default)]
struct RoomMesh {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl RoomMesh {
    /// One wall panel between two heights, facing the way it is told to.
    fn wall(&mut self, a: Vec2, b: Vec2, inward: Vec2, bottom: f32, top: f32) {
        let normal = [inward.x, 0.0, inward.y];
        let base = self.positions.len() as u32;
        for (p, y) in [(a, bottom), (b, bottom), (b, top), (a, top)] {
            self.positions.push([p.x, y, p.y]);
            self.normals.push(normal);
            self.colors.push(WALL_COLOR);
        }
        // Wound to agree with the normal it was given, or the panel is lit from
        // the room and culled from it at the same time.
        let facing = (b - a).perp_dot(inward) < 0.0;
        if facing {
            self.indices
                .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        } else {
            self.indices
                .extend([base, base + 2, base + 1, base, base + 3, base + 2]);
        }
    }

    /// The floor or the ceiling.
    ///
    /// **Earcut, not the fan this used to be.** A fan about the centroid is
    /// correct on the near-convex outline a building footprint usually is, which
    /// is why it was good enough while a room was one storey with no hole in it.
    /// A slab with a stairwell bitten out of its edge is exactly the shape a fan
    /// gets wrong, and it gets it wrong by *filling the well in* — a floor drawn
    /// over the hole the flight comes up through, in a room where the metric,
    /// the collider and the stairs all still say the hole is there.
    fn slab(&mut self, ring: &[Vec2], y: f32, color: [f32; 4], up: bool) {
        if ring.len() < 3 {
            return;
        }
        let flat: Vec<f64> = ring
            .iter()
            .flat_map(|p| [f64::from(p.x), f64::from(p.y)])
            .collect();
        let Ok(tris) = earcutr::earcut(&flat, &[], 2) else {
            return;
        };
        let normal = if up {
            [0.0, 1.0, 0.0]
        } else {
            [0.0, -1.0, 0.0]
        };
        let base = self.positions.len() as u32;
        for p in ring {
            self.positions.push([p.x, y, p.y]);
            self.normals.push(normal);
            self.colors.push(color);
        }
        for t in tris.chunks_exact(3) {
            let (a, b, c) = (ring[t[0]], ring[t[1]], ring[t[2]]);
            // Earcut keeps the input ring's winding and no register promises one,
            // so each triangle is wound to agree with the normal it was given. A
            // floor lit from beneath is the giveaway that this was guessed.
            let cross = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
            let (i, j, k) = (t[0] as u32, t[1] as u32, t[2] as u32);
            if (cross < 0.0) == up {
                self.indices.extend([base + i, base + j, base + k]);
            } else {
                self.indices.extend([base + i, base + k, base + j]);
            }
        }
    }

    /// One flight of stairs, rising `STOREY_M` from `bottom`.
    ///
    /// Treads and risers, plus a closed ceiling and one open side. Both of those
    /// are load-bearing rather than decoration: nothing in this layer is
    /// double-sided, so a flight of bare treads is invisible from underneath and
    /// the storey above is visible straight through it — and standing under the
    /// stairs is somewhere the avatar routinely ends up, because the support
    /// field will not lift it onto a flight more than a step above its head.
    fn flight(&mut self, stair: &Stair, bottom: f32) {
        let going = stair.run / STAIR_STEPS as f32;
        let rise = STOREY_M / STAIR_STEPS as f32;
        let across = stair.side * STAIR_WIDTH_M;
        let up = [0.0, 1.0, 0.0];
        for s in 0..STAIR_STEPS {
            let y = bottom + (s + 1) as f32 * rise;
            let front = stair.foot + stair.dir * (s as f32 * going);
            let back = front + stair.dir * going;
            // The tread, walked on from above.
            self.quad(
                [front, back, back + across, front + across],
                [y, y, y, y],
                up,
                WALL_COLOR,
            );
            // The riser, seen from the foot of the flight.
            let n = [-stair.dir.x, 0.0, -stair.dir.y];
            self.quad(
                [front, front + across, front + across, front],
                [y - rise, y - rise, y, y],
                n,
                FLOOR_COLOR,
            );
        }
        // The ceiling: one sloped quad under the whole flight, facing down.
        //
        // Its normal is **perpendicular to the ramp and pointing away from the
        // treads**, which is not the same as "roughly downward" and is easy to
        // write as neither. `slope` runs along the flight; turning it a quarter
        // turn in the vertical plane the flight lies in gives the underside, and
        // the sign is what makes it the *under*side.
        let top = stair.foot + stair.dir * stair.run;
        let slope = Vec2::new(stair.run, STOREY_M).normalize();
        let down = Vec3::new(stair.dir.x * slope.y, -slope.x, stair.dir.y * slope.y);
        self.quad(
            [stair.foot, stair.foot + across, top + across, top],
            [bottom, bottom, bottom + STOREY_M, bottom + STOREY_M],
            [down.x, down.y, down.z],
            FLOOR_COLOR,
        );
        // The open side, as the right triangle the flight's profile is. The other
        // side is the wall the flight stands against and needs nothing.
        let side_n = [stair.side.x, 0.0, stair.side.y];
        self.quad(
            [
                stair.foot + across,
                top + across,
                top + across,
                stair.foot + across,
            ],
            [bottom, bottom, bottom + STOREY_M, bottom],
            side_n,
            FLOOR_COLOR,
        );
    }

    /// Four corners at four heights, wound to face `normal`. A degenerate corner
    /// pair (a triangle written as a quad) costs one empty triangle and saves the
    /// callers above a second entry point.
    fn quad(&mut self, p: [Vec2; 4], y: [f32; 4], normal: [f32; 3], color: [f32; 4]) {
        let base = self.positions.len() as u32;
        for i in 0..4 {
            self.positions.push([p[i].x, y[i], p[i].y]);
            self.normals.push(normal);
            self.colors.push(color);
        }
        // Wound against the normal it was given rather than assumed from the
        // corner order, which is the same rule `wall` follows and for the same
        // reason: the caller derives its corners from a ring whose winding is not
        // promised.
        let n = Vec3::new(normal[0], normal[1], normal[2]);
        let e1 = Vec3::new(p[1].x - p[0].x, y[1] - y[0], p[1].y - p[0].y);
        let e2 = Vec3::new(p[2].x - p[0].x, y[2] - y[0], p[2].y - p[0].y);
        if e1.cross(e2).dot(n) >= 0.0 {
            self.indices
                .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        } else {
            self.indices
                .extend([base, base + 2, base + 1, base, base + 3, base + 2]);
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

    fn square(size: f32) -> Vec<Vec2> {
        vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(size, 0.0),
            Vec2::new(size, size),
            Vec2::new(0.0, size),
        ]
    }

    #[test]
    fn a_room_is_smaller_than_the_building_it_is_in() {
        let outer = square(10.0);
        let inner = inset_ring(&outer, WALL_INSET_M);
        assert_eq!(inner.len(), 4);
        assert!(
            ring_area(&inner) < ring_area(&outer),
            "the inset ring must be inside the outline, not outside it"
        );
        // A 10 m square inset by 0.22 m is 9.56 m on a side.
        assert!((ring_area(&inner) - 9.56 * 9.56).abs() < 0.1);
    }

    /// The winding a footprint arrives in is not promised, and inset must go the
    /// same way — inward — either way round. Getting this wrong builds a room
    /// *bigger* than its building, poking out through the facade.
    #[test]
    fn inset_goes_inward_whichever_way_the_ring_is_wound() {
        let mut reversed = square(10.0);
        reversed.reverse();
        let inner = inset_ring(&reversed, WALL_INSET_M);
        assert!(ring_area(&inner) < ring_area(&reversed));
    }

    #[test]
    fn a_wall_is_cut_where_its_door_is() {
        let (a, b) = (Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0));
        let whole = segments_between_doors(a, b, &[]);
        assert_eq!(whole.len(), 1, "no door leaves the wall whole");

        let cut = segments_between_doors(a, b, &[Vec2::new(5.0, 0.0)]);
        assert_eq!(cut.len(), 2, "a door in the middle leaves two pieces");
        let gap = cut[1].0.x - cut[0].1.x;
        assert!(
            (gap - 2.0 * DOOR_HALF_WIDTH).abs() < 0.01,
            "the gap should be a doorway wide, got {gap}"
        );
    }

    /// A door on the far side of the building projects onto this wall too. Cutting
    /// on that would open a hole in a wall nobody walks through.
    #[test]
    fn a_door_on_another_wall_does_not_cut_this_one() {
        let (a, b) = (Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0));
        let far = segments_between_doors(a, b, &[Vec2::new(5.0, 9.0)]);
        assert_eq!(far.len(), 1);
    }

    /// A door at the very end of a wall must not leave a sliver behind it.
    #[test]
    fn a_door_at_a_corner_does_not_leave_a_sliver() {
        let (a, b) = (Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0));
        let cut = segments_between_doors(a, b, &[Vec2::new(0.0, 0.0)]);
        assert_eq!(cut.len(), 1, "the gap runs off the end, leaving one piece");
        assert!(cut[0].0.x >= DOOR_HALF_WIDTH - 0.01);
    }

    /// Every wall must face into the room it encloses, on either winding — the
    /// same failure `lod22`'s doors had, and the same reason it is a test.
    #[test]
    fn every_wall_faces_into_the_room_whichever_way_the_ring_is_wound() {
        let mut ring = square(10.0);
        for _ in 0..2 {
            let centre = Vec2::new(5.0, 5.0);
            for i in 0..ring.len() {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                let n = inward_normal(&ring, a, b).expect("a square has edges");
                let toward_centre = (centre - (a + b) * 0.5).normalize();
                assert!(
                    n.dot(toward_centre) > 0.5,
                    "wall {a:?}->{b:?} faces {n:?}, away from the room"
                );
            }
            ring.reverse();
        }
    }

    /// A stand-in BAG identificatie. Nothing under test reads it — it exists so
    /// `InteriorFloors::building_at` can name the building a room is in.
    fn test_id() -> Arc<str> {
        Arc::from("0014100010903891")
    }

    /// A room big enough to hold a staircase, for the storey tests below. 12 m
    /// square: comfortably over `STAIR_RUN_M` plus its end clearances on a side.
    fn tall_room(height: Option<f32>) -> Room {
        plan_room(test_id(), &square(12.0), height).expect("a 12 m square is a room")
    }

    #[test]
    fn a_room_builds_a_floor_a_ceiling_and_walls() {
        let mut buf = RoomMesh::default();
        build_room(
            &mut buf,
            &plan_room(test_id(), &square(10.0), None).expect("a 10 m square is a room"),
            &[Vec2::new(5.0, 0.0)],
        );
        let up = buf.normals.iter().filter(|n| n[1] > 0.9).count();
        let down = buf.normals.iter().filter(|n| n[1] < -0.9).count();
        let side = buf.normals.iter().filter(|n| n[1].abs() < 0.1).count();
        assert!(up > 0 && down > 0 && side > 0, "floor, ceiling and walls");
    }

    /// A cupboard is not a room.
    #[test]
    fn a_footprint_too_small_to_stand_in_gets_nothing() {
        assert!(plan_room(test_id(), &square(1.0), Some(12.0)).is_none());
    }

    /// The whole stack has to fit *under* the roof. A ceiling poking out through
    /// the tiles is worse than a building with one room in it, and it is the
    /// failure a `round` here would produce on every second building.
    #[test]
    fn no_storey_ever_reaches_above_the_shell_that_holds_it() {
        for tenths in 30..300u32 {
            let h = tenths as f32 / 10.0;
            let n = storeys_for(Some(h));
            let top_ceiling = storey_floor_y(n - 1) + CEILING_M;
            assert!(
                n == 1 || top_ceiling <= h,
                "{n} storeys in a {h} m shell puts a ceiling at {top_ceiling} m"
            );
        }
    }

    /// A shell whose LoD2.2 cell has not landed reads as one storey — the answer
    /// this layer gave everywhere before it could ask, so a building gains its
    /// floors when its geometry arrives rather than being wrong until then.
    #[test]
    fn an_unmeasured_shell_is_one_storey() {
        assert_eq!(storeys_for(None), 1);
        assert_eq!(storeys_for(Some(2.0)), 1, "shorter than a storey");
        assert_eq!(storeys_for(Some(9.3)), 2, "the median Groningen shell");
        assert_eq!(storeys_for(Some(1000.0)), MAX_STOREYS);
    }

    /// A floor nobody can reach is not a floor. A building with no wall long
    /// enough for a flight keeps its ground floor whatever the register says
    /// about its height.
    #[test]
    fn a_building_with_nowhere_for_stairs_stays_one_storey() {
        // Big enough to stand in (`MIN_ROOM_M2`), and no wall on it reaches
        // `STAIR_RUN_M` plus its end clearances.
        let small = square(2.8);
        let room = plan_room(test_id(), &small, Some(20.0)).expect("still a room");
        assert!(room.stair.is_none());
        assert_eq!(room.storeys, 1);
    }

    /// The stair must lie inside the room on *both* axes. A corridor has a wall
    /// long enough for the run and no depth to put the flight in, and the length
    /// test alone passes it.
    #[test]
    fn a_corridor_is_long_enough_for_a_flight_and_too_narrow_for_one() {
        let corridor = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(9.0, 0.0),
            Vec2::new(9.0, 1.2),
            Vec2::new(0.0, 1.2),
        ];
        let room = plan_room(test_id(), &corridor, Some(20.0)).expect("still a room");
        assert!(
            room.stair.is_none(),
            "a 1.2 m corridor cannot hold a {STAIR_WIDTH_M} m flight"
        );
    }

    /// The ceiling over your head and the storey above it are both surfaces
    /// directly above you, and neither may pick you up. Only the flight rises out
    /// of the floor you are standing on — which is what makes the stairs the way
    /// up rather than a decoration over a lift.
    #[test]
    fn standing_on_the_ground_floor_is_not_lifted_to_the_one_above() {
        let room = tall_room(Some(20.0));
        assert!(room.storeys >= 3, "a 20 m shell should hold storeys");
        let floors = InteriorFloors { rooms: vec![room] };
        // A spot in the middle of the room, clear of the stairs.
        let middle = Vec2::new(6.0, 6.0);
        assert!(
            floors.support(middle, 0.0) < STOREY_M,
            "the floor above is not a step up"
        );
        assert!(
            (floors.support(middle, 0.0) - FLOOR_Y).abs() < 1e-4,
            "the ground floor is"
        );
    }

    /// Walking the flight from the bottom has to arrive at the floor above, and
    /// arrive *at* it — a ramp that stops short leaves you standing in the well.
    #[test]
    fn the_flight_reaches_the_storey_above_and_carries_you_onto_it() {
        let room = tall_room(Some(20.0));
        let stair = room.stair.expect("a 12 m square holds a flight");
        let floors = InteriorFloors { rooms: vec![room] };
        let mut at = FLOOR_Y;
        // Sixty samples up the run, each reached from the last — the walk, not a
        // teleport to the top.
        for i in 0..=60 {
            let t = i as f32 / 60.0;
            let p = stair.foot + stair.dir * (t * stair.run) + stair.side * (STAIR_WIDTH_M * 0.5);
            let next = floors.support(p, at);
            assert!(
                next >= at - 1e-3,
                "the flight dropped from {at} to {next} at t={t}"
            );
            at = next;
        }
        let expected = storey_floor_y(1);
        assert!(
            (at - expected).abs() < 1e-3,
            "the top of the flight is {at}, the floor above is {expected}"
        );
    }

    /// The hole the flight comes up through is not somewhere to stand. Without
    /// this the well is floored over and the stairs arrive under a lid.
    #[test]
    fn the_stairwell_is_not_a_floor_on_the_storey_it_opens_into() {
        let room = tall_room(Some(20.0));
        let stair = room.stair.expect("a flight");
        let floors = InteriorFloors { rooms: vec![room] };
        // Standing on the first floor, over the middle of the well.
        let over_well =
            stair.foot + stair.dir * (stair.run * 0.5) + stair.side * (STAIR_WIDTH_M * 0.5);
        let from = storey_floor_y(1);
        let held = floors.support(over_well, from);
        assert!(
            held < from,
            "the well held the avatar up at {held} instead of letting it down"
        );
    }

    /// The well is cut out of the slab the flight rises into, and it is cut by
    /// *removing* area — a fan triangulation fills it back in, which is what this
    /// asserts against.
    #[test]
    fn the_upper_floor_has_the_stairwell_taken_out_of_it() {
        let room = tall_room(Some(20.0));
        let stair = room.stair.expect("a flight");
        let notched = notch_ring(&room.ring, &stair).expect("the flight is on a wall");
        let whole = ring_area(&room.ring);
        let cut = ring_area(&notched);
        let well = STAIR_RUN_M * STAIR_WIDTH_M;
        assert!(
            (whole - cut - well).abs() < 0.05,
            "cut {:.2} m² out of {whole:.2} m², expected {well:.2} m²",
            whole - cut
        );
    }

    /// The ceiling closes the flight from underneath, and "roughly downward" is
    /// not the same as perpendicular to it. Written the obvious way the normal
    /// came out 0.998 *along* the ramp and pointing up, which faces the surface
    /// the wrong way — so the underside is open, and since nothing here is
    /// double-sided the storey above shows straight through the stairs. The
    /// picture that would have caught it is one taken from under a staircase.
    #[test]
    fn the_underside_of_a_flight_faces_down_and_across_it() {
        let room = tall_room(Some(20.0));
        let stair = room.stair.expect("a flight");
        let mut buf = RoomMesh::default();
        buf.flight(&stair, FLOOR_Y);
        // The ramp's own direction: along the flight and up.
        let along =
            Vec3::new(stair.dir.x * stair.run, STOREY_M, stair.dir.y * stair.run).normalize();
        let ceiling = buf
            .normals
            .iter()
            .map(|n| Vec3::new(n[0], n[1], n[2]))
            .find(|n| n.y < -0.1 && n.y > -0.99)
            .expect("no downward-and-sloped face under the flight");
        assert!(
            along.dot(ceiling).abs() < 1e-3,
            "the ceiling normal {ceiling:?} is not perpendicular to the ramp"
        );
        assert!(ceiling.y < 0.0, "the ceiling faces up: {ceiling:?}");
    }

    /// Nothing a room is built from may land outside the building holding it, on
    /// any of the awkward outlines a register actually ships.
    ///
    /// This is the check a picture cannot make. An interior is drawn single-sided
    /// and facing in, so a slab or a flight that escapes its own walls is
    /// invisible from every angle you would think to photograph it from, and
    /// shows up only as something odd hanging over the street from one particular
    /// spot. Concave rings are where it would come from: the inset of an L or a U
    /// is where an offset can fold, and earcut on a folded ring emits triangles
    /// outside it.
    #[test]
    fn no_part_of_a_room_lands_outside_the_building() {
        let shapes: Vec<(&str, Vec<Vec2>)> = vec![
            ("square", square(14.0)),
            (
                "L",
                vec![
                    Vec2::new(0.0, 0.0),
                    Vec2::new(14.0, 0.0),
                    Vec2::new(14.0, 6.0),
                    Vec2::new(6.0, 6.0),
                    Vec2::new(6.0, 14.0),
                    Vec2::new(0.0, 14.0),
                ],
            ),
            (
                "U",
                vec![
                    Vec2::new(0.0, 0.0),
                    Vec2::new(16.0, 0.0),
                    Vec2::new(16.0, 12.0),
                    Vec2::new(11.0, 12.0),
                    Vec2::new(11.0, 5.0),
                    Vec2::new(5.0, 5.0),
                    Vec2::new(5.0, 12.0),
                    Vec2::new(0.0, 12.0),
                ],
            ),
            (
                "canal house, deep and narrow",
                vec![
                    Vec2::new(0.0, 0.0),
                    Vec2::new(5.2, 0.0),
                    Vec2::new(5.2, 13.0),
                    Vec2::new(0.0, 13.0),
                ],
            ),
        ];
        for (name, outline) in shapes {
            let Some(room) = plan_room(test_id(), &outline, Some(20.0)) else {
                continue;
            };
            let mut buf = RoomMesh::default();
            build_room(&mut buf, &room, &[]);
            let top = storey_floor_y(room.storeys - 1) + CEILING_M;
            for v in &buf.positions {
                // A shade of tolerance: the inset ring is inside the outline by
                // `WALL_INSET_M`, so anything at all outside is a fold, not a
                // rounding error.
                assert!(
                    point_in_ring(&outline, Vec2::new(v[0], v[2])),
                    "{name}: a vertex at ({}, {}) is outside the building",
                    v[0],
                    v[2]
                );
                assert!(
                    v[1] >= -1e-3 && v[1] <= top + 1e-3,
                    "{name}: a vertex at {} m is outside 0..{top} m",
                    v[1]
                );
            }
        }
    }

    /// Nothing outside a building is on any floor, whatever is stacked inside it.
    #[test]
    fn outdoors_has_no_interior_floor() {
        let floors = InteriorFloors {
            rooms: vec![tall_room(Some(20.0))],
        };
        assert_eq!(floors.support(Vec2::new(-50.0, -50.0), 0.0), 0.0);
    }

    /// Every storey is built, not just counted — the metric and the geometry are
    /// the same rooms, and a tally that outran the mesh would be a floor you fall
    /// through.
    #[test]
    fn a_stack_of_storeys_builds_a_slab_for_each_of_them() {
        let room = tall_room(Some(20.0));
        let storeys = room.storeys;
        let mut buf = RoomMesh::default();
        build_room(&mut buf, &room, &[]);
        let mut levels: Vec<f32> = buf
            .positions
            .iter()
            .zip(&buf.normals)
            .filter(|(_, n)| n[1] > 0.9)
            .map(|(p, _)| p[1])
            .collect();
        levels.sort_by(f32::total_cmp);
        levels.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
        for k in 0..storeys {
            let want = storey_floor_y(k);
            assert!(
                levels.iter().any(|&y| (y - want).abs() < 1e-3),
                "no floor at {want} m; up-facing levels are {levels:?}"
            );
        }
    }
}
