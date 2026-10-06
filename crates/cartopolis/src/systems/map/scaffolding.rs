//! Scaffolding on the buildings the register says are being worked on.
//!
//! The BAG carries a status per building, and two of its values describe a
//! building with work going on at it: `renovation` (*verbouwing pand*) and
//! `building` (*bouw gestart*). Over Groningen's four central cells that is 87
//! buildings in about twenty-five thousand, and until this they were only a line
//! on the building card. A street with a scaffolded house in it is a street
//! somebody is doing something to, and it is the register saying so, not a
//! decoration — so this draws only where the register says, and nowhere else.
//!
//! **Geometry, in the shell's own mesh.** A scaffold is tubes and boards, the
//! one thing on a facade the window shader cannot paint: it stands off the
//! wall, throws a shadow onto it and is seen past from the street. Every part
//! carries the doorway parts' negative UV0.y, which `building_windows.wgsl`
//! reads as "not a wall", so no window lattice is painted on a standard. It is
//! built on a `Near` cell only, as the doors are, and it is a few hundred
//! triangles a face on a few dozen buildings a city.
//!
//! **Which walls.** A pand's solid includes its party walls, and a scaffold on
//! one of those stands inside the neighbour. So a face is only a candidate if a
//! step off it lands outside *every* building in the cell — a measurement on
//! the outlines, the same rule `lod22::nearest_edge_point` uses for doors rather
//! than a guess about winding. Of those, the street side (the faces carrying a
//! door) is scaffolded, or the longest one when the register places no door.

use bevy::math::{Vec2, Vec3};

use crate::systems::building_facts::BuildingStatus;
use crate::systems::building_windows::WallFace;
use crate::systems::osm_buildings::MeshData;

/// Whether the register's status means work is going on at the building.
#[must_use]
pub fn wants_scaffold(status: BuildingStatus) -> bool {
    matches!(
        status,
        BuildingStatus::Renovation | BuildingStatus::UnderConstruction
    )
}

/// Galvanised tube, linear RGB. UV0.x = 1 asks the shader for metal.
const TUBE: [f32; 4] = [0.30, 0.31, 0.32, 1.0];
const TUBE_UV: [f32; 2] = [1.0, -0.5];
/// Scaffold boards: weathered softwood.
const BOARD: [f32; 4] = [0.26, 0.17, 0.085, 1.0];
const BOARD_UV: [f32; 2] = [0.0, -0.5];

/// The inner and outer standards' distance off the wall, metres. Between them
/// is one deck of boards, 0.7 m — a standard access scaffold.
const INNER_M: f32 = 0.3;
const OUTER_M: f32 = 1.0;
/// Bay length the standards are spaced to, and the height of one lift.
const BAY_M: f32 = 2.5;
const LIFT_M: f32 = 2.0;
/// Tube section (a 48.3 mm scaffold tube, drawn square).
const TUBE_W: f32 = 0.05;
/// A face shorter or lower than this takes no scaffold.
const MIN_FACE_M: f32 = 3.0;
/// The highest the drawn scaffold reaches above the ground it stands on. A
/// tower under renovation is not wrapped to its top.
const MAX_HEIGHT_M: f32 = 40.0;
/// At most this many faces of one building.
const MAX_FACES: usize = 3;
/// How far off a face its exterior probe steps.
const PROBE_M: f32 = 1.2;

/// The cell's building outlines, for "does a step off this wall land inside a
/// building?". Pushed once per drawn building.
#[derive(Default)]
pub struct Outlines {
    rings: Vec<(Vec2, Vec2, Vec<Vec2>)>,
}

impl Outlines {
    pub fn push(&mut self, ring: &[Vec2]) {
        if ring.len() < 3 {
            return;
        }
        let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for &p in ring {
            lo = lo.min(p);
            hi = hi.max(p);
        }
        self.rings.push((lo, hi, ring.to_vec()));
    }

    fn inside_any(&self, p: Vec2) -> bool {
        self.rings.iter().any(|(lo, hi, ring)| {
            p.x >= lo.x && p.y >= lo.y && p.x <= hi.x && p.y <= hi.y && inside(ring, p)
        })
    }
}

/// Even-odd point in polygon.
fn inside(ring: &[Vec2], p: Vec2) -> bool {
    let mut odd = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            odd = !odd;
        }
        j = i;
    }
    odd
}

/// A building waiting for its scaffold until the whole cell's outlines are
/// known: its candidate faces (in the shell's world-XZ frame, heights above
/// its base) and the ground level it stands on.
pub struct Pending {
    pub faces: Vec<WallFace>,
    pub base: f32,
}

impl Pending {
    /// The faces worth considering: standing on the ground, big enough to be
    /// worth a scaffold.
    #[must_use]
    pub fn new(faces: &[WallFace], base: f32) -> Self {
        Self {
            faces: faces
                .iter()
                .filter(|f| f.low < 0.8 && f.len >= MIN_FACE_M && f.high - f.low >= MIN_FACE_M)
                .cloned()
                .collect(),
            base,
        }
    }
}

/// Which of a building's candidate faces get a scaffold: the exterior ones
/// carrying a door, else the longest exterior one.
#[must_use]
pub fn choose(faces: &[WallFace], outlines: &Outlines) -> Vec<usize> {
    let exterior: Vec<usize> = (0..faces.len())
        .filter(|&i| {
            let f = &faces[i];
            let t = f.tangent();
            [0.2, 0.5, 0.8].iter().all(|s| {
                let p = t * (f.u0 + f.len * s) + f.outward * (f.d + PROBE_M);
                !outlines.inside_any(p)
            })
        })
        .collect();
    let mut chosen: Vec<usize> = exterior
        .iter()
        .copied()
        .filter(|&i| faces[i].door.is_some())
        .collect();
    if chosen.is_empty() {
        chosen.extend(
            exterior
                .iter()
                .copied()
                .max_by(|&a, &b| faces[a].len.total_cmp(&faces[b].len)),
        );
    }
    chosen.truncate(MAX_FACES);
    chosen
}

/// A box from `a` to `b` with half-extents `side` and `up` across it, each
/// face wound outward.
fn bar(buf: &mut MeshData, a: Vec3, b: Vec3, side: Vec3, up: Vec3, colour: [f32; 4], uv: [f32; 2]) {
    let axis = (b - a).normalize_or_zero();
    let c = |e: Vec3, s: f32, u: f32| e + side * s + up * u;
    let faces = [
        (side, [(1.0, -1.0), (1.0, 1.0)]),
        (-side, [(-1.0, -1.0), (-1.0, 1.0)]),
        (up, [(-1.0, 1.0), (1.0, 1.0)]),
        (-up, [(-1.0, -1.0), (1.0, -1.0)]),
    ];
    for (n, [(s0, u0), (s1, u1)]) in faces {
        buf.quad_facing_uv(
            [c(a, s0, u0), c(b, s0, u0), c(b, s1, u1), c(a, s1, u1)],
            colour,
            n.normalize_or_zero(),
            [uv; 4],
        );
    }
    for (e, n) in [(a, -axis), (b, axis)] {
        buf.quad_facing_uv(
            [
                c(e, -1.0, -1.0),
                c(e, 1.0, -1.0),
                c(e, 1.0, 1.0),
                c(e, -1.0, 1.0),
            ],
            colour,
            n,
            [uv; 4],
        );
    }
}

/// A tube from `a` to `b`.
fn tube(buf: &mut MeshData, a: Vec3, b: Vec3) {
    let axis = (b - a).normalize_or_zero();
    let side = if axis.y.abs() > 0.9 {
        Vec3::X
    } else {
        axis.cross(Vec3::Y).normalize_or_zero()
    };
    let up = side.cross(axis).normalize_or_zero();
    bar(
        buf,
        a,
        b,
        side * TUBE_W * 0.5,
        up * TUBE_W * 0.5,
        TUBE,
        TUBE_UV,
    );
}

/// Build one face's scaffold. `face` is in world XZ with heights above the
/// building's base; `base` is the ground level and `origin` the mesh's
/// big_space cell offset. Returns whether anything was drawn.
pub fn push(buf: &mut MeshData, face: &WallFace, base: f32, origin: Vec2) -> bool {
    let t = face.tangent();
    let n = face.outward;
    let n3 = Vec3::new(n.x, 0.0, n.y);
    let p = |u: f32, y: f32, out: f32| {
        let q = t * (face.u0 + u) + n * (face.d + out);
        Vec3::new(q.x - origin.x, base + face.low + y, q.y - origin.y)
    };
    // Kept a little inside the face's ends, so it does not poke into a
    // neighbour round the corner.
    let (u_lo, u_hi) = (0.15, face.len - 0.15);
    let height = (face.high - face.low).min(MAX_HEIGHT_M);
    let lifts = ((height - 0.5) / LIFT_M).floor() as i32;
    if lifts < 1 || u_hi - u_lo < 1.0 {
        return false;
    }
    let bays = ((u_hi - u_lo) / BAY_M).ceil().max(1.0) as i32;
    let step = (u_hi - u_lo) / bays as f32;
    let us: Vec<f32> = (0..=bays).map(|i| u_lo + step * i as f32).collect();
    let top = lifts as f32 * LIFT_M + 1.0;

    // Standards, inner and outer.
    for &u in &us {
        for out in [INNER_M, OUTER_M] {
            tube(buf, p(u, 0.0, out), p(u, top, out));
        }
    }
    // Sole ledgers just off the ground.
    for out in [INNER_M, OUTER_M] {
        tube(buf, p(u_lo, 0.2, out), p(u_hi, 0.2, out));
    }
    let board_half = (OUTER_M - INNER_M) * 0.5 - 0.03;
    for k in 1..=lifts {
        let y = k as f32 * LIFT_M;
        // Ledgers along the face, transoms across it under the boards.
        for out in [INNER_M, OUTER_M] {
            tube(buf, p(u_lo, y, out), p(u_hi, y, out));
        }
        for &u in &us {
            tube(buf, p(u, y, INNER_M - 0.1), p(u, y, OUTER_M + 0.1));
        }
        // The deck: one run of boards on the transoms.
        let mid = (INNER_M + OUTER_M) * 0.5;
        let deck_y = y + TUBE_W * 0.5 + 0.025;
        bar(
            buf,
            p(u_lo, deck_y, mid),
            p(u_hi, deck_y, mid),
            n3 * board_half,
            Vec3::Y * 0.025,
            BOARD,
            BOARD_UV,
        );
        // The toe board on the outer edge.
        bar(
            buf,
            p(u_lo, deck_y + 0.1, OUTER_M - 0.04),
            p(u_hi, deck_y + 0.1, OUTER_M - 0.04),
            n3 * 0.015,
            Vec3::Y * 0.075,
            BOARD,
            BOARD_UV,
        );
        // Guard rail and mid rail on the outside, and across both ends.
        for rail in [0.5, 1.0] {
            tube(buf, p(u_lo, y + rail, OUTER_M), p(u_hi, y + rail, OUTER_M));
        }
        for u in [u_lo, u_hi] {
            tube(buf, p(u, y + 1.0, INNER_M), p(u, y + 1.0, OUTER_M));
        }
        // A face brace across every third bay of each lift, alternating its
        // direction so the bracing zig-zags up the scaffold.
        for (j, w) in us.windows(2).enumerate() {
            if j % 3 != 0 {
                continue;
            }
            let (a, b) = if k % 2 == 0 {
                (w[0], w[1])
            } else {
                (w[1], w[0])
            };
            tube(
                buf,
                p(a, y - LIFT_M + 0.2, OUTER_M + TUBE_W),
                p(b, y, OUTER_M + TUBE_W),
            );
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::building_windows::WallFaces;

    /// A 10 × 8 m box, 9 m high: its pts and its triangles (walls only).
    fn box_shell(x0: f32, z0: f32) -> (Vec<Vec3>, Vec<(usize, usize, usize, u8)>) {
        let (x1, z1, h) = (x0 + 10.0, z0 + 8.0, 9.0);
        let corners = [
            Vec2::new(x0, z0),
            Vec2::new(x1, z0),
            Vec2::new(x1, z1),
            Vec2::new(x0, z1),
        ];
        let mut pts = Vec::new();
        let mut tris = Vec::new();
        for i in 0..4 {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            let k = pts.len();
            pts.extend([
                Vec3::new(a.x, 0.0, a.y),
                Vec3::new(b.x, 0.0, b.y),
                Vec3::new(b.x, h, b.y),
                Vec3::new(a.x, h, a.y),
            ]);
            tris.push((k, k + 1, k + 2, 0));
            tris.push((k, k + 2, k + 3, 0));
        }
        (pts, tris)
    }

    fn ring(x0: f32, z0: f32) -> Vec<Vec2> {
        vec![
            Vec2::new(x0, z0),
            Vec2::new(x0 + 10.0, z0),
            Vec2::new(x0 + 10.0, z0 + 8.0),
            Vec2::new(x0, z0 + 8.0),
        ]
    }

    /// A party wall takes no scaffold, whichever way the shell is wound: the
    /// face shared with the neighbour is the one whose probe lands in it.
    #[test]
    fn a_party_wall_is_never_chosen() {
        for flip in [false, true] {
            let (pts, mut tris) = box_shell(0.0, 0.0);
            if flip {
                for t in &mut tris {
                    std::mem::swap(&mut t.1, &mut t.2);
                }
            }
            let faces = WallFaces::new(&pts, &tris, 0);
            let mut outlines = Outlines::default();
            outlines.push(&ring(0.0, 0.0));
            // The neighbour to the east, sharing the wall at x = 10.
            outlines.push(&ring(10.0, 0.0));
            let pending = Pending::new(&faces.faces, 0.0);
            for i in choose(&pending.faces, &outlines) {
                let f = &pending.faces[i];
                let mid = f.tangent() * (f.u0 + f.len * 0.5) + f.outward * f.d;
                assert!(
                    (mid.x - 10.0).abs() > 0.5,
                    "flip={flip}: scaffolded the party wall at {mid}"
                );
            }
        }
    }

    /// With no door, the longest exterior face is scaffolded, and on the
    /// outside of it.
    #[test]
    fn scaffolds_the_longest_outside_face() {
        // Wound outward, as a shell is (it is back-face culled).
        let (pts, mut tris) = box_shell(0.0, 0.0);
        for t in &mut tris {
            std::mem::swap(&mut t.1, &mut t.2);
        }
        let faces = WallFaces::new(&pts, &tris, 0);
        let mut outlines = Outlines::default();
        outlines.push(&ring(0.0, 0.0));
        let pending = Pending::new(&faces.faces, 0.0);
        let chosen = choose(&pending.faces, &outlines);
        assert_eq!(chosen.len(), 1);
        let f = &pending.faces[chosen[0]];
        assert!((f.len - 10.0).abs() < 0.01, "chose a {} m face", f.len);

        let mut buf = MeshData::with_dedup().with_uvs();
        assert!(push(&mut buf, f, 0.0, Vec2::ZERO));
        let pos = buf.positions();
        assert!(!pos.is_empty());
        let mut top = f32::MIN;
        for v in pos {
            // Every vertex stands off the wall, on its outside: the transoms
            // reach in to 0.2 m, nothing nearer.
            let out = f.outward.dot(Vec2::new(v[0], v[2])) - f.d;
            assert!(out > 0.15, "a vertex {out} m off the wall");
            top = top.max(v[1]);
        }
        assert!(top > 6.0 && top <= 9.5, "top at {top}");
    }

    #[test]
    fn only_works_in_progress_are_scaffolded() {
        assert!(wants_scaffold(BuildingStatus::Renovation));
        assert!(wants_scaffold(BuildingStatus::UnderConstruction));
        assert!(!wants_scaffold(BuildingStatus::InUse));
        assert!(!wants_scaffold(BuildingStatus::OutOfUse));
    }
}
