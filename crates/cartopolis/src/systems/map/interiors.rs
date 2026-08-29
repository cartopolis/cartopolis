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

/// Ceiling height, metres. A nominal Dutch storey.
const CEILING_M: f32 = 2.9;

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
    facts: Res<BuildingFacts>,
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
    let mut rooms = 0usize;
    let (lat, lng) = world_to_lat_lng(geo, f64::from(here.x), f64::from(here.y));
    for building in facts.near(lat, lng, f64::from(INTERIOR_RADIUS)) {
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
        if build_room(&mut buf, &ring, &doors) {
            rooms += 1;
        }
    }
    interiors.rooms = rooms;

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

/// Build one building's ground-floor room into `buf`. `false` if the outline was
/// too small to stand in.
fn build_room(buf: &mut RoomMesh, ring: &[Vec2], doors: &[Vec2]) -> bool {
    let inset = inset_ring(ring, WALL_INSET_M);
    if inset.len() < 3 || ring_area(&inset) < MIN_ROOM_M2 {
        return false;
    }
    // Walls, inward-facing, with the doorways left out.
    //
    // Which way is "inward" is **probed, not assumed**. The register does not
    // promise a winding, and the identical assumption in `lod22`'s door placement
    // was wrong in this frame — every door in the city was drawn inside its own
    // wall and the count said they existed. A wall facing the wrong way fails the
    // same silent way: the room is built, the metric counts it, and standing in it
    // you see straight out through the walls.
    for i in 0..inset.len() {
        let (a, b) = (inset[i], inset[(i + 1) % inset.len()]);
        let Some(inward) = inward_normal(&inset, a, b) else {
            continue;
        };
        for (from, to) in segments_between_doors(a, b, doors) {
            buf.wall(from, to, inward);
        }
    }
    buf.slab(&inset, FLOOR_Y, FLOOR_COLOR, true);
    buf.slab(&inset, CEILING_M, CEILING_COLOR, false);
    true
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
    /// One wall panel, facing the way it is told to.
    fn wall(&mut self, a: Vec2, b: Vec2, inward: Vec2) {
        let normal = [inward.x, 0.0, inward.y];
        let base = self.positions.len() as u32;
        for (p, y) in [(a, FLOOR_Y), (b, FLOOR_Y), (b, CEILING_M), (a, CEILING_M)] {
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

    /// The floor or the ceiling: the ring triangulated as a fan about its
    /// centroid.
    ///
    /// A fan rather than earcut because a room's outline is a building footprint —
    /// eight or nine points and very nearly convex — and a fan on one of those is
    /// correct. The concave case is a slightly wrong floor in a room whose ceiling
    /// is 2.9 m over your head, which is not worth a dependency here.
    fn slab(&mut self, ring: &[Vec2], y: f32, color: [f32; 4], up: bool) {
        let centre = ring.iter().copied().fold(Vec2::ZERO, |a, b| a + b) / ring.len() as f32;
        let normal = if up {
            [0.0, 1.0, 0.0]
        } else {
            [0.0, -1.0, 0.0]
        };
        let base = self.positions.len() as u32;
        self.positions.push([centre.x, y, centre.y]);
        self.normals.push(normal);
        self.colors.push(color);
        for p in ring {
            self.positions.push([p.x, y, p.y]);
            self.normals.push(normal);
            self.colors.push(color);
        }
        for i in 0..ring.len() as u32 {
            let (b, c) = (base + 1 + i, base + 1 + (i + 1) % ring.len() as u32);
            // Wind so the face points the way its normal says. A floor lit from
            // beneath is the giveaway that this was guessed rather than derived.
            let (p, q, r) = (
                Vec2::new(centre.x, centre.y),
                ring[i as usize],
                ring[((i + 1) as usize) % ring.len()],
            );
            let cross = (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x);
            if (cross < 0.0) == up {
                self.indices.extend([base, b, c]);
            } else {
                self.indices.extend([base, c, b]);
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

    #[test]
    fn a_room_builds_a_floor_a_ceiling_and_walls() {
        let mut buf = RoomMesh::default();
        assert!(build_room(&mut buf, &square(10.0), &[Vec2::new(5.0, 0.0)]));
        let up = buf.normals.iter().filter(|n| n[1] > 0.9).count();
        let down = buf.normals.iter().filter(|n| n[1] < -0.9).count();
        let side = buf.normals.iter().filter(|n| n[1].abs() < 0.1).count();
        assert!(up > 0 && down > 0 && side > 0, "floor, ceiling and walls");
    }

    /// A cupboard is not a room.
    #[test]
    fn a_footprint_too_small_to_stand_in_gets_nothing() {
        let mut buf = RoomMesh::default();
        assert!(!build_room(&mut buf, &square(1.0), &[Vec2::new(0.5, 0.0)]));
        assert!(buf.indices.is_empty());
    }
}
