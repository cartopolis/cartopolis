//! The outside of a designed building: the surveyed walls redrawn with the doors
//! and windows its owner drew (`docs/notes/building-designs.md`).
//!
//! A 3DBAG shell is a solid with painted windows: `building_windows.wesl` lays a
//! lattice of panes over every wall, guessing their size from the building's
//! era. That is right for a building nobody has described and wrong for one
//! somebody has — the panes would be in different places from the windows they
//! drew, so from inside you would look out through a window the street cannot
//! see. So for a building with a stored design, `lod22` drops the shell's
//! **walls** (its roof stays: it is surveyed and nobody drew one) and this draws
//! them again:
//!
//! * **one wall per edge of the register's outline**, the same ring the collider
//!   and the rooms are built from, up to the height the survey measured along that
//!   edge — a profile, not a box, so a gable keeps its point and meets the roof;
//! * **the design's openings cut into it** — every outer wall's doors and windows,
//!   on every storey, projected onto the outline edge they stand behind — as real
//!   holes, with reveals as deep as the wall is thick so a window reads as an
//!   opening in brickwork and not as a sticker on it;
//! * **a pane, a frame or a door leaf** set back in each opening.
//!
//! The wall keeps the facade's brick and its era colour: its UV code asks the
//! window shader for a face with no windows of its own (a 1 cm bay), so the
//! shader paints the brick and nothing else. The panes and leaves use the doorway
//! path of the same shader, style 3 being a pane. The inside sees the same
//! openings, because `interiors` cuts them out of the inset walls from the same
//! design — so the window you look out of is the window the street sees.

use bevy::prelude::*;

use crate::systems::osm_buildings::MeshData;

/// How deep an opening's reveal goes, metres: to the plane the room's outer wall
/// stands on (`building_design::WALL_INSET_M`), so no gap shows between the two.
const REVEAL_M: f32 = cartopolis_geo::building_design::WALL_INSET_M;

/// Where a window's pane sits back from the face, metres.
const PANE_SET_BACK_M: f32 = 0.10;

/// Where a door's leaf sits back from the face, metres.
const LEAF_SET_BACK_M: f32 = 0.06;

/// The width of a window frame round its pane, metres.
const FRAME_M: f32 = 0.06;

/// How far an opening's centre may be from an outline edge and still be cut into
/// it, metres. The design's outer walls stand `WALL_INSET_M` inside the outline,
/// so this is that and some slack for a hand-drawn wall.
const SNAP_M: f32 = 0.8;

/// An opening's surround and reveals.
const SURROUND: [f32; 4] = [0.58, 0.56, 0.5, 1.0];
/// A window pane before the shader makes it glass.
const PANE: [f32; 4] = [0.03, 0.034, 0.04, 1.0];
/// A front door's leaf.
const LEAF: [f32; 4] = [0.02, 0.03, 0.085, 1.0];

/// UV0 of the plain doorway parts (surround, reveals): kept their own colour.
const PLAIN_UV: [f32; 2] = [0.0, -0.5];

/// UV0.y of a wall face with no painted windows: a 1 cm bay, which the shader
/// reads as "too short for a window" and leaves as brick. The same marker the
/// imported models carry ([`crate::systems::building_windows::WINDOWLESS_UV`]).
const WINDOWLESS_UV_Y: f32 = crate::systems::building_windows::WINDOWLESS_UV[1];

/// One opening in the facade, in the building's world frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FacadeOpening {
    /// Centre, world XZ.
    pub at: Vec2,
    pub width: f32,
    /// Bottom and top, metres above the building's base.
    pub bottom: f32,
    pub top: f32,
    pub door: bool,
}

/// The openings of a design's outer walls, in the world frame `to_local` maps
/// lat/lng into.
pub(crate) fn openings_of(
    design: &cartopolis_geo::building_design::BuildingDesign,
    to_local: impl Fn(f64, f64) -> Vec2,
) -> Vec<FacadeOpening> {
    use cartopolis_geo::building_design::{to_lat_lng, OpeningKind, FLOOR_Y};
    let mut out = Vec::new();
    for s in &design.storeys {
        for w in s.walls.iter().filter(|w| w.exterior) {
            let len = (w.b[0] - w.a[0]).hypot(w.b[1] - w.a[1]);
            if len < 1e-3 {
                continue;
            }
            let d = [(w.b[0] - w.a[0]) / len, (w.b[1] - w.a[1]) / len];
            for o in &w.openings {
                let c = [w.a[0] + d[0] * o.at, w.a[1] + d[1] * o.at];
                let (lat, lng) = to_lat_lng(design.origin, c);
                let bottom = s.elevation + FLOOR_Y + o.sill;
                out.push(FacadeOpening {
                    at: to_local(lat, lng),
                    width: o.width,
                    bottom,
                    top: bottom + o.height,
                    door: o.kind == OpeningKind::Door,
                });
            }
        }
    }
    out
}

/// Draw a designed building's walls into `buf`.
///
/// `outline` is the register's ring and `shell` the surveyed wall vertices as
/// `(x, height above base, z)`, both in the cell's world frame; `origin` is the
/// cell's offset, subtracted from every vertex as `lod22` does. `fallback_top`
/// is the height of an edge the survey has no vertex along.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    buf: &mut MeshData,
    outline: &[Vec2],
    base: f32,
    shell: &[Vec3],
    fallback_top: f32,
    openings: &[FacadeOpening],
    wall: [f32; 4],
    origin: Vec2,
) {
    let n = outline.len();
    if n < 3 {
        return;
    }
    for i in 0..n {
        let (a, b) = (outline[i], outline[(i + 1) % n]);
        let len = a.distance(b);
        if len < 0.3 {
            continue;
        }
        let t = (b - a) / len;
        let perp = Vec2::new(-t.y, t.x);
        let mid = (a + b) * 0.5;
        // Outward by asking, never by winding (`docs/notes/doors-and-rooms.md`).
        let out = if point_in_ring(outline, mid + perp * 0.1) {
            -perp
        } else {
            perp
        };
        let profile = top_profile(shell, a, t, len, out, fallback_top);
        // Each opening this edge carries, as a rectangle in the wall's own
        // (along, height) frame, with whether it is a door.
        let holes: Vec<([f32; 4], bool)> = openings
            .iter()
            .filter_map(|o| {
                let rel = o.at - a;
                let along = rel.dot(t);
                if rel.dot(out).abs() > SNAP_M || along < 0.0 || along > len {
                    return None;
                }
                let u0 = (along - o.width * 0.5).max(0.08);
                let u1 = (along + o.width * 0.5).min(len - 0.08);
                let y0 = o.bottom.max(0.03);
                // Strictly under the wall's top across the whole opening, or the
                // hole would cut the outline of the wall it is a hole in.
                let ceiling = profile_min(&profile, u0, u1) - 0.08;
                let y1 = o.top.min(ceiling);
                (u1 - u0 > 0.2 && y1 - y0 > 0.2).then_some(([u0, y0, u1, y1], o.door))
            })
            .collect();
        let at = |u: f32, y: f32, depth: f32| {
            let q = a + t * u - out * depth;
            Vec3::new(q.x - origin.x, base + y, q.y - origin.y)
        };
        let n3 = Vec3::new(out.x, 0.0, out.y);
        let t3 = Vec3::new(t.x, 0.0, t.y);

        // The wall: its profile as the outer ring, the openings as holes.
        let mut flat: Vec<f64> = Vec::new();
        let mut pts: Vec<(f32, f32)> = Vec::new();
        let push = |u: f32, y: f32, flat: &mut Vec<f64>, pts: &mut Vec<(f32, f32)>| {
            flat.extend([f64::from(u), f64::from(y)]);
            pts.push((u, y));
        };
        push(0.0, 0.0, &mut flat, &mut pts);
        push(len, 0.0, &mut flat, &mut pts);
        for &(u, h) in profile.iter().rev() {
            push(u, h, &mut flat, &mut pts);
        }
        let mut hole_starts = Vec::new();
        for (h, _) in &holes {
            hole_starts.push(pts.len());
            for (u, y) in [(h[0], h[1]), (h[2], h[1]), (h[2], h[3]), (h[0], h[3])] {
                push(u, y, &mut flat, &mut pts);
            }
        }
        let Ok(tris) = earcutr::earcut(&flat, &hole_starts, 2) else {
            continue;
        };
        let uv = |u: f32| [u, WINDOWLESS_UV_Y];
        for tri in tris.chunks_exact(3) {
            let p = [pts[tri[0]], pts[tri[1]], pts[tri[2]]];
            let v = p.map(|(u, y)| at(u, y, 0.0));
            let (v, p) = if (v[1] - v[0]).cross(v[2] - v[0]).dot(n3) < 0.0 {
                ([v[0], v[2], v[1]], [p[0], p[2], p[1]])
            } else {
                (v, p)
            };
            let tone = |y: f32| {
                let d = crate::systems::osm_buildings::wall_shade(fallback_top.max(3.0), y);
                [wall[0] * d, wall[1] * d, wall[2] * d, wall[3]]
            };
            buf.tri_graded_uv(
                v,
                [tone(p[0].1), tone(p[1].1), tone(p[2].1)],
                [uv(p[0].0), uv(p[1].0), uv(p[2].0)],
            );
        }

        // Each opening: reveals to the depth of the wall, then what fills it.
        for &(h, door) in &holes {
            let [u0, y0, u1, y1] = h;
            // Jambs, sill and head, each facing into the opening.
            buf.quad_facing_uv(
                [
                    at(u0, y0, 0.0),
                    at(u0, y0, REVEAL_M),
                    at(u0, y1, REVEAL_M),
                    at(u0, y1, 0.0),
                ],
                SURROUND,
                t3,
                [PLAIN_UV; 4],
            );
            buf.quad_facing_uv(
                [
                    at(u1, y0, 0.0),
                    at(u1, y0, REVEAL_M),
                    at(u1, y1, REVEAL_M),
                    at(u1, y1, 0.0),
                ],
                SURROUND,
                -t3,
                [PLAIN_UV; 4],
            );
            buf.quad_facing_uv(
                [
                    at(u0, y0, 0.0),
                    at(u1, y0, 0.0),
                    at(u1, y0, REVEAL_M),
                    at(u0, y0, REVEAL_M),
                ],
                SURROUND,
                Vec3::Y,
                [PLAIN_UV; 4],
            );
            buf.quad_facing_uv(
                [
                    at(u0, y1, 0.0),
                    at(u1, y1, 0.0),
                    at(u1, y1, REVEAL_M),
                    at(u0, y1, REVEAL_M),
                ],
                SURROUND,
                -Vec3::Y,
                [PLAIN_UV; 4],
            );
            if door {
                // The leaf: panelled, with a glazed transom (style 0).
                let d = LEAF_SET_BACK_M;
                buf.quad_facing_uv(
                    [at(u0, y0, d), at(u1, y0, d), at(u1, y1, d), at(u0, y1, d)],
                    LEAF,
                    n3,
                    [
                        [0.0, -1.0],
                        [u1 - u0, -1.0],
                        [u1 - u0, -1.0 - (y1 - y0)],
                        [0.0, -1.0 - (y1 - y0)],
                    ],
                );
            } else {
                // The frame, then the pane inside it (style 3: all glass).
                let d = PANE_SET_BACK_M;
                let f = FRAME_M.min((u1 - u0) * 0.2).min((y1 - y0) * 0.2);
                for [a0, b0, a1, b1] in [
                    [u0, y0, u1, y0 + f],
                    [u0, y1 - f, u1, y1],
                    [u0, y0 + f, u0 + f, y1 - f],
                    [u1 - f, y0 + f, u1, y1 - f],
                ] {
                    buf.quad_facing_uv(
                        [at(a0, b0, d), at(a1, b0, d), at(a1, b1, d), at(a0, b1, d)],
                        SURROUND,
                        n3,
                        [PLAIN_UV; 4],
                    );
                }
                let (p0, q0, p1, q1) = (u0 + f, y0 + f, u1 - f, y1 - f);
                buf.quad_facing_uv(
                    [at(p0, q0, d), at(p1, q0, d), at(p1, q1, d), at(p0, q1, d)],
                    PANE,
                    n3,
                    [
                        [30.0, -1.0],
                        [30.0 + (p1 - p0), -1.0],
                        [30.0 + (p1 - p0), -1.0 - (q1 - q0)],
                        [30.0, -1.0 - (q1 - q0)],
                    ],
                );
            }
        }
    }
}

/// The height the survey measured along one outline edge, as `(along, height)`
/// from 0 to `len`: the upper envelope of the shell's wall vertices that stand on
/// the edge, so a gable keeps its point. An edge the survey has nothing along —
/// the register's ring and 3DBAG's soup are not the same numbers — gets
/// `fallback` across.
fn top_profile(
    shell: &[Vec3],
    a: Vec2,
    t: Vec2,
    len: f32,
    out: Vec2,
    fallback: f32,
) -> Vec<(f32, f32)> {
    const BIN_M: f32 = 0.25;
    let mut bins: Vec<(f32, f32)> = Vec::new();
    for v in shell {
        if v.y < 1.0 {
            continue; // the foot of the wall, not its top
        }
        let rel = Vec2::new(v.x, v.z) - a;
        let along = rel.dot(t);
        if rel.dot(out).abs() > 0.6 || along < -0.3 || along > len + 0.3 {
            continue;
        }
        let u = along.clamp(0.0, len);
        match bins.iter_mut().find(|(b, _)| (b - u).abs() < BIN_M) {
            Some(bin) => bin.1 = bin.1.max(v.y),
            None => bins.push((u, v.y)),
        }
    }
    if bins.is_empty() {
        return vec![(0.0, fallback), (len, fallback)];
    }
    bins.sort_by(|x, y| x.0.total_cmp(&y.0));
    if bins[0].0 > 0.01 {
        bins.insert(0, (0.0, bins[0].1));
    } else {
        bins[0].0 = 0.0;
    }
    let last = bins.len() - 1;
    if bins[last].0 < len - 0.01 {
        bins.push((len, bins[last].1));
    } else {
        bins[last].0 = len;
    }
    bins
}

/// The lowest the profile goes over `u0..u1`.
fn profile_min(profile: &[(f32, f32)], u0: f32, u1: f32) -> f32 {
    let height = |u: f32| {
        for w in profile.windows(2) {
            if u >= w[0].0 && u <= w[1].0 {
                let span = (w[1].0 - w[0].0).max(1e-4);
                return w[0].1 + (w[1].1 - w[0].1) * (u - w[0].0) / span;
            }
        }
        profile.last().map_or(0.0, |p| p.1)
    };
    let mut m = height(u0).min(height(u1));
    for &(u, h) in profile {
        if u > u0 && u < u1 {
            m = m.min(h);
        }
    }
    m
}

fn point_in_ring(ring: &[Vec2], p: Vec2) -> bool {
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A 6 × 12 m house with a gable on its short walls: eaves at 6 m, ridge at
    /// 9 m over the middle of the short walls.
    fn gabled() -> (Vec<Vec2>, Vec<Vec3>) {
        let outline = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(6.0, 0.0),
            Vec2::new(6.0, 12.0),
            Vec2::new(0.0, 12.0),
        ];
        let mut shell = Vec::new();
        for &(x, z) in &[(0.0, 0.0), (6.0, 0.0), (6.0, 12.0), (0.0, 12.0)] {
            shell.push(Vec3::new(x, 0.0, z));
            shell.push(Vec3::new(x, 6.0, z));
        }
        shell.push(Vec3::new(3.0, 9.0, 0.0));
        shell.push(Vec3::new(3.0, 9.0, 12.0));
        (outline, shell)
    }

    #[test]
    fn a_gable_keeps_its_point() {
        let (outline, shell) = gabled();
        let t = (outline[1] - outline[0]).normalize();
        let p = top_profile(&shell, outline[0], t, 6.0, Vec2::new(0.0, -1.0), 5.0);
        assert_eq!(p.first(), Some(&(0.0, 6.0)));
        assert_eq!(p.last(), Some(&(6.0, 6.0)));
        assert!(
            p.iter().any(|&(u, h)| (u - 3.0).abs() < 0.01 && h == 9.0),
            "{p:?}"
        );
        // Under the gable a window may go higher than under the eaves.
        assert!(profile_min(&p, 2.5, 3.5) > 8.0);
        assert!((profile_min(&p, 0.0, 1.0) - 6.0).abs() < 1e-4);
    }

    #[test]
    fn an_edge_with_no_survey_gets_the_fallback() {
        let p = top_profile(&[], Vec2::ZERO, Vec2::X, 4.0, Vec2::NEG_Y, 7.5);
        assert_eq!(p, vec![(0.0, 7.5), (4.0, 7.5)]);
    }

    /// A window in the south wall: the wall gets a hole with a pane in it, and no
    /// wall triangle covers the hole's middle.
    #[test]
    fn a_window_is_a_hole_with_a_pane_in_it() {
        let (outline, shell) = gabled();
        let opening = FacadeOpening {
            at: Vec2::new(3.0, 0.22),
            width: 1.2,
            bottom: 1.0,
            top: 2.3,
            door: false,
        };
        let mut buf = MeshData::with_dedup().with_uvs();
        draw(
            &mut buf,
            &outline,
            0.0,
            &shell,
            6.0,
            &[opening],
            [0.5, 0.4, 0.3, 1.0],
            Vec2::ZERO,
        );
        let centre = Vec3::new(3.0, 1.65, 0.0);
        // Nothing drawn exactly in the wall's plane at the hole's centre…
        let covered = buf.test_triangles().any(|[a, b, c]| {
            a.z.abs() < 1e-4 && b.z.abs() < 1e-4 && c.z.abs() < 1e-4 && in_tri(centre, a, b, c)
        });
        assert!(!covered, "the wall was drawn across its own window");
        // …and a pane set back behind it.
        assert!(buf
            .test_triangles()
            .any(|[a, b, c]| (a.z - PANE_SET_BACK_M).abs() < 1e-4
                && in_tri(Vec3::new(3.0, 1.65, PANE_SET_BACK_M), a, b, c)));
    }

    fn in_tri(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> bool {
        let (p, a, b, c) = (p.xy(), a.xy(), b.xy(), c.xy());
        let s = |u: Vec2, v: Vec2, w: Vec2| (v - u).perp_dot(w - u);
        let (d1, d2, d3) = (s(a, b, p), s(b, c, p), s(c, a, p));
        !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
    }
}
