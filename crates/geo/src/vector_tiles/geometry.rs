//! Plane geometry shared by every tessellator here: clipping rings and polylines to the tile
//! square, earcut triangulation, and upward-facing triangle winding.

/// Clip one ring to the square `[0, extent]²` (Sutherland–Hodgman), returning it
/// **open** — no repeated last point — with consecutive duplicates dropped.
///
/// This exists instead of a per-vertex `clamp` because clamping is not a clip: it
/// projects every outside vertex onto the border and throws away the order in
/// which the ring left and re-entered. A canal that runs out through the tile's
/// west edge and back comes home as a *fold* — the outbound and return banks both
/// collapse onto `x = 0` and cross each other — and earcut on a self-intersecting
/// ring emits bow-ties: finite-area triangles outside the polygon. On the ground
/// those are the dark water wedges lying across a lawn or a carriageway a hundred
/// metres from the nearest canal, and the pale spikes over the water beside them.
/// Measured over the cached Groningen z14 tiles, the clamp produced 145 such
/// triangles (23,000 m² of misplaced water) across 4,970 water polygons; the clip
/// produces none.
///
/// Sutherland–Hodgman inserts the true border crossings and walks the border
/// between an exit and the next entry, so a concave shape comes back with
/// zero-width connectors along the seam rather than a fold. Those degenerate into
/// zero-area triangles, which cost a little raster time and are invisible — the
/// trade the clamp was reaching for.
pub(super) fn clip_ring(points: &[[f32; 2]], extent: f32) -> Vec<[f32; 2]> {
    // Each pass clips against one half-plane of the square, feeding the next.
    let mut ring: Vec<[f32; 2]> = points.to_vec();
    // Rings arrive closed; the walk below re-closes implicitly via the modulo.
    if ring.len() >= 2 && ring[0] == ring[ring.len() - 1] {
        ring.pop();
    }
    for edge in 0..4 {
        if ring.len() < 3 {
            return Vec::new();
        }
        let inside = |p: &[f32; 2]| match edge {
            0 => p[0] >= 0.0,
            1 => p[0] <= extent,
            2 => p[1] >= 0.0,
            _ => p[1] <= extent,
        };
        // Where segment a→b crosses this edge. Only called when the two ends sit
        // on opposite sides, so the denominator cannot be zero.
        let crossing = |a: &[f32; 2], b: &[f32; 2]| -> [f32; 2] {
            match edge {
                0 | 1 => {
                    let at = if edge == 0 { 0.0 } else { extent };
                    let t = (at - a[0]) / (b[0] - a[0]);
                    [at, a[1] + t * (b[1] - a[1])]
                }
                _ => {
                    let at = if edge == 2 { 0.0 } else { extent };
                    let t = (at - a[1]) / (b[1] - a[1]);
                    [a[0] + t * (b[0] - a[0]), at]
                }
            }
        };
        let src = std::mem::take(&mut ring);
        for i in 0..src.len() {
            let prev = src[(i + src.len() - 1) % src.len()];
            let cur = src[i];
            match (inside(&prev), inside(&cur)) {
                (true, true) => ring.push(cur),
                (true, false) => ring.push(crossing(&prev, &cur)),
                (false, true) => {
                    ring.push(crossing(&prev, &cur));
                    ring.push(cur);
                }
                (false, false) => {}
            }
        }
    }

    // The clip lands successive vertices on the same border point whenever the
    // ring grazes a corner; earcut turns a duplicate into a degenerate ear.
    /// Two clipped vertices close enough that earcut would see one.
    fn near(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() <= 1e-5 && (a[1] - b[1]).abs() <= 1e-5
    }
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(ring.len());
    for p in ring {
        if out.last().is_none_or(|q: &[f32; 2]| !near(p, *q)) {
            out.push(p);
        }
    }
    // Closing vertex, on the same tolerance the interior dedup above uses. An exact
    // `==` here left a ring whose ends differ by less than 1e-5 — but not by
    // nothing — carrying a near-duplicate seam vertex into earcut, which is exactly
    // the degenerate ear this function exists to prevent, arrived at from the one
    // direction the loop above cannot see.
    while out.len() >= 2 && near(out[0], out[out.len() - 1]) {
        out.pop();
    }
    if out.len() < 3 {
        return Vec::new();
    }
    out
}

/// A polygon's rings in tile-local metres, clipped to the tile exactly the way
/// [`tessellate_polygon`] clips them, and **closed** (last point repeats the
/// first) so `windows(2)` walks every edge.
///
/// This is the boundary the vegetation scatter insets from. It has to come from
/// the same clip as the fill, or the trees would stand off an edge the visible
/// green does not have.
pub(crate) fn clipped_rings(
    poly: &geo_types::Polygon<f32>,
    scale: f32,
    extent: f32,
) -> Vec<Vec<[f32; 2]>> {
    let ring = |r: &geo_types::LineString<f32>| -> Vec<[f32; 2]> {
        let raw: Vec<[f32; 2]> = r.0.iter().map(|c| [c.x, c.y]).collect();
        let mut pts: Vec<[f32; 2]> = clip_ring(&raw, extent)
            .into_iter()
            .map(|p| [p[0] * scale, p[1] * scale])
            .collect();
        if pts.len() >= 3 {
            pts.push(pts[0]);
        }
        pts
    };
    std::iter::once(poly.exterior())
        .chain(poly.interiors())
        .map(ring)
        .filter(|r| r.len() >= 4)
        .collect()
}

/// Twice the signed area of a closed ring — positive counter-clockwise in the
/// tile's (x east, y south) frame. Only the sign and the ordering of magnitudes
/// are used, so the factor of two is left in.
pub(super) fn ring_area(ring: &[[f32; 2]]) -> f32 {
    let mut acc = 0.0f32;
    for i in 0..ring.len() {
        let a = ring[i];
        let b = ring[(i + 1) % ring.len()];
        acc += a[0] * b[1] - b[0] * a[1];
    }
    acc
}

/// Append a triangle wound so its face points **+Y**, whatever order the caller
/// supplies. Ground geometry is single-sided and every surface here faces up, so
/// the winding is derived rather than reasoned about at each call site (the same
/// rule [`tessellate_polygon`] applies to its earcut output).
pub(super) fn push_triangle_up(
    positions: &[[f32; 3]],
    indices: &mut Vec<u32>,
    a: u32,
    b: u32,
    c: u32,
) {
    let (pa, pb, pc) = (
        positions[a as usize],
        positions[b as usize],
        positions[c as usize],
    );
    let cross = (pb[0] - pa[0]) * (pc[2] - pa[2]) - (pb[2] - pa[2]) * (pc[0] - pa[0]);
    if cross.abs() < f32::EPSILON {
        return; // degenerate — a zero-area triangle only costs raster time
    }
    if cross < 0.0 {
        indices.extend([a, b, c]);
    } else {
        indices.extend([a, c, b]);
    }
}

/// Clip a polyline to the square `[lo, hi]²`, returning the runs that survive.
///
/// MVT geometry runs past the tile edge into a buffer zone so that a stroked line
/// has something to join to; kept as-is, both neighbours would draw the same
/// stretch of road and the overlap z-fights. Clipping cuts each crossing at the
/// border, so the two tiles' ribbons abut. A run breaks whenever a segment leaves
/// and re-enters, which is why the result is a list rather than one line.
pub(crate) fn clip_polyline(points: &[[f32; 2]], lo: f32, hi: f32) -> Vec<Vec<[f32; 2]>> {
    let mut runs: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    let flush = |cur: &mut Vec<[f32; 2]>, runs: &mut Vec<Vec<[f32; 2]>>| {
        if cur.len() >= 2 {
            runs.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };

    for seg in points.windows(2) {
        match clip_segment(seg[0], seg[1], lo, hi) {
            None => flush(&mut cur, &mut runs),
            Some((a, b)) => {
                let continues = cur
                    .last()
                    .is_some_and(|q| (q[0] - a[0]).hypot(q[1] - a[1]) <= 1e-4);
                if !continues {
                    flush(&mut cur, &mut runs);
                    cur.push(a);
                }
                cur.push(b);
            }
        }
    }
    flush(&mut cur, &mut runs);
    runs
}

/// Liang–Barsky segment clip against the square `[lo, hi]²`. `None` if the
/// segment misses the square entirely.
fn clip_segment(a: [f32; 2], b: [f32; 2], lo: f32, hi: f32) -> Option<([f32; 2], [f32; 2])> {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [
        (-dx, a[0] - lo),
        (dx, hi - a[0]),
        (-dy, a[1] - lo),
        (dy, hi - a[1]),
    ] {
        if p == 0.0 {
            // Parallel to this edge: inside or entirely out, no clipping to do.
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let r = q / p;
        if p < 0.0 {
            if r > t1 {
                return None;
            }
            t0 = t0.max(r);
        } else {
            if r < t0 {
                return None;
            }
            t1 = t1.min(r);
        }
    }
    if t1 <= t0 {
        return None;
    }
    Some((
        [a[0] + dx * t0, a[1] + dy * t0],
        [a[0] + dx * t1, a[1] + dy * t1],
    ))
}

/// Earcut one polygon (exterior + holes) into an existing position/index buffer.
pub(crate) fn tessellate_polygon(
    poly: &geo_types::Polygon<f32>,
    scale: f32,
    extent: f32,
    positions: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
) {
    // MVT geometry runs past the tile edge into a buffer zone, so neighbouring
    // tiles would overlap — and coplanar overlapping ground z-fights. [`clip_ring`]
    // cuts the overhang off against the border, which is invisible because the
    // adjacent tile continues the same surface. It returns the ring open (earcut
    // wants it open; a repeated vertex there produces degenerate triangles) and
    // deduplicated.
    let ring_coords = |ring: &geo_types::LineString<f32>| -> Vec<f64> {
        let raw: Vec<[f32; 2]> = ring.0.iter().map(|c| [c.x, c.y]).collect();
        clip_ring(&raw, extent)
            .iter()
            .flat_map(|p| [f64::from(p[0]), f64::from(p[1])])
            .collect()
    };

    let mut flat = ring_coords(poly.exterior());
    if flat.len() < 6 {
        return; // fewer than 3 points
    }
    let mut holes = Vec::new();
    for interior in poly.interiors() {
        let h = ring_coords(interior);
        if h.len() < 6 {
            continue;
        }
        holes.push(flat.len() / 2);
        flat.extend(h);
    }

    let Ok(tris) = earcutr::earcut(&flat, &holes, 2) else {
        return;
    };

    let base = positions.len() as u32;
    positions.extend(
        flat.chunks_exact(2)
            .map(|c| [c[0] as f32 * scale, 0.0, c[1] as f32 * scale]),
    );
    // Earcut keeps the input ring's orientation, which MVT does not guarantee to be
    // the one we want; a ground surface always faces up, so wind for +Y.
    for t in tris.chunks_exact(3) {
        let p = |i: usize| -> [f32; 3] {
            [
                flat[i * 2] as f32 * scale,
                0.0,
                flat[i * 2 + 1] as f32 * scale,
            ]
        };
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        // Cross product's Y component decides the winding (all y are 0 here).
        let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
        if cross < 0.0 {
            indices.extend([base + t[0] as u32, base + t[1] as u32, base + t[2] as u32]);
        } else {
            indices.extend([base + t[0] as u32, base + t[2] as u32, base + t[1] as u32]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point_in_rings(px: f32, pz: f32, rings: &[Vec<[f32; 2]>]) -> bool {
        let mut inside = false;
        for r in rings {
            for w in r.windows(2) {
                let (a, b) = (w[0], w[1]);
                if (a[1] > pz) != (b[1] > pz) {
                    let t = (pz - a[1]) / (b[1] - a[1]);
                    if px < a[0] + t * (b[0] - a[0]) {
                        inside = !inside;
                    }
                }
            }
        }
        inside
    }

    #[test]
    fn a_ring_that_leaves_the_tile_and_returns_stays_inside_its_own_outline() {
        // A canal running out through the west edge and back — a thin U whose two
        // banks both lie outside the tile. Clamping folds those banks onto x = 0,
        // the ring self-intersects, and earcut answers with bow-ties: finite-area
        // triangles *outside* the polygon. On screen those are the dark water
        // wedges lying across a lawn or a carriageway well away from any water.
        let ring = geo_types::LineString::from(vec![
            (-200.0f32, 1000.0),
            (600.0, 1000.0),
            (600.0, 1100.0),
            (-200.0, 1100.0),
            (-200.0, 2000.0),
            (600.0, 2000.0),
            (600.0, 2100.0),
            (-200.0, 2100.0),
        ]);
        let poly = geo_types::Polygon::new(ring, vec![]);
        let (scale, extent) = (1000.0 / 4096.0, 4096.0);

        let mut positions = Vec::new();
        let mut indices = Vec::new();
        tessellate_polygon(&poly, scale, extent, &mut positions, &mut indices);
        assert!(!indices.is_empty(), "the clipped polygon still has area");

        let rings = clipped_rings(&poly, scale, extent);
        for t in indices.chunks_exact(3) {
            let (a, b, c) = (
                positions[t[0] as usize],
                positions[t[1] as usize],
                positions[t[2] as usize],
            );
            let area =
                (((b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0])) * 0.5).abs();
            if area <= 1.0 {
                continue; // a zero-width connector along the seam — invisible
            }
            let (cx, cz) = ((a[0] + b[0] + c[0]) / 3.0, (a[2] + b[2] + c[2]) / 3.0);
            assert!(
                point_in_rings(cx, cz, &rings),
                "triangle of {area:.0} m² at ({cx:.0}, {cz:.0}) lies outside the polygon"
            );
        }
    }

    #[test]
    fn clipping_cuts_the_overhang_rather_than_folding_it() {
        // The clip keeps what is inside, inserts the two border crossings, and
        // hands back an open, duplicate-free ring.
        let out = clip_ring(
            &[[-10.0, 10.0], [50.0, 10.0], [50.0, 50.0], [-10.0, 50.0]],
            100.0,
        );
        assert_eq!(out.len(), 4, "{out:?}");
        assert!(out.iter().all(|p| p[0] >= 0.0 && p[0] <= 100.0));
        assert_eq!(out.iter().filter(|p| p[0] == 0.0).count(), 2);
        // Entirely outside the tile: nothing survives.
        assert!(clip_ring(&[[-50.0, -50.0], [-10.0, -50.0], [-10.0, -10.0]], 100.0).is_empty());
    }

    /// Clipping, not clamping: geometry in the MVT buffer zone must be *cut* at
    /// the tile border, or both neighbours draw the same overlapping tarmac.
    #[test]
    fn polylines_are_clipped_at_the_tile_border() {
        // Enters at x = 0, leaves at x = 100, having started and ended outside.
        let runs = clip_polyline(&[[-50.0, 10.0], [150.0, 10.0]], 0.0, 100.0);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].first().unwrap()[0], 0.0);
        assert_eq!(runs[0].last().unwrap()[0], 100.0);

        // Out, in, out again → one run, and it is the middle stretch only.
        let runs = clip_polyline(&[[-10.0, 50.0], [50.0, 50.0], [110.0, 50.0]], 0.0, 100.0);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len(), 3);

        // A line that leaves and re-enters breaks into two runs rather than
        // cutting the corner across the tile.
        let runs = clip_polyline(
            &[[50.0, 50.0], [150.0, 50.0], [150.0, 80.0], [50.0, 80.0]],
            0.0,
            100.0,
        );
        assert_eq!(runs.len(), 2, "re-entry must start a new run");

        // Entirely outside: nothing at all.
        assert!(clip_polyline(&[[-10.0, -10.0], [-5.0, -5.0]], 0.0, 100.0).is_empty());
    }
}
