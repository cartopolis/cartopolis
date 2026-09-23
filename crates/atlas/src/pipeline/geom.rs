//! Geometry more than one extractor uses. Ported from cartopy's
//! `shared/geom.ts`, with its behaviour — including its edge cases — kept.

use serde_json::Value;

pub type Pt = [f64; 2];

/// Metres per degree of latitude, for the local-metre conversions.
pub const M_PER_DEG: f64 = 111_320.0;

/// Douglas-Peucker over a ring already in metres. **Iterative, not recursive**: a
/// BGT ring runs to ten thousand points, and the recursion depth that implies is a
/// stack overflow on a worst-case (already simple) ring.
#[must_use]
pub fn simplify(ring: &[Pt], eps: f64) -> Vec<Pt> {
    let n = ring.len();
    if n < 3 {
        return ring.to_vec();
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut stack = vec![(0usize, n - 1)];
    while let Some((i, j)) = stack.pop() {
        if j <= i + 1 {
            continue;
        }
        let [ax, ay] = ring[i];
        let [bx, by] = ring[j];
        let (dx, dy) = (bx - ax, by - ay);
        let seg = dx.hypot(dy);
        let mut best = -1.0;
        let mut at = 0;
        for (k, &[px, py]) in ring.iter().enumerate().take(j).skip(i + 1) {
            let d = if seg < 1e-12 {
                (px - ax).hypot(py - ay)
            } else {
                (dx * (ay - py) - (ax - px) * dy).abs() / seg
            };
            if d > best {
                best = d;
                at = k;
            }
        }
        if best > eps {
            keep[at] = true;
            stack.push((i, at));
            stack.push((at, j));
        }
    }
    ring.iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(p, _)| *p)
        .collect()
}

/// Shoelace area of a ring, unsigned, in the ring's own units.
#[must_use]
pub fn ring_area(pts: &[Pt]) -> f64 {
    let n = pts.len();
    let mut a = 0.0;
    for i in 0..n {
        let [x1, y1] = pts[i];
        let [x2, y2] = pts[(i + 1) % n];
        a += x1 * y2 - x2 * y1;
    }
    a.abs() * 0.5
}

fn pt(v: &Value) -> Option<Pt> {
    let a = v.as_array()?;
    Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])
}

fn ring(v: &Value) -> Vec<Pt> {
    v.as_array()
        .map(|r| r.iter().filter_map(pt).collect())
        .unwrap_or_default()
}

/// Every ring of a (Multi)Polygon as `[x, y]` pairs — outer rings *and* holes,
/// because [`point_in_rings`] needs both.
#[must_use]
pub fn rings_of(geom: &Value) -> Vec<Vec<Pt>> {
    let coords = &geom["coordinates"];
    match geom["type"].as_str() {
        Some("Polygon") => coords
            .as_array()
            .map(|p| p.iter().map(ring).collect())
            .unwrap_or_default(),
        Some("MultiPolygon") => coords
            .as_array()
            .map(|ps| {
                ps.iter()
                    .flat_map(|p| p.as_array().cloned().unwrap_or_default())
                    .map(|r| ring(&r))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// A (Multi)Polygon's rings grouped per polygon, coordinates as given.
#[must_use]
pub fn polygon_rings(geom: &Value) -> Vec<Vec<Vec<Pt>>> {
    let coords = &geom["coordinates"];
    let poly = |p: &Value| -> Vec<Vec<Pt>> {
        p.as_array()
            .map(|rs| rs.iter().map(ring).collect())
            .unwrap_or_default()
    };
    match geom["type"].as_str() {
        Some("Polygon") => vec![poly(coords)],
        Some("MultiPolygon") => coords
            .as_array()
            .map(|ps| ps.iter().map(poly).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Crossing-number test. The ring may be closed or open; the wrap handles both.
#[must_use]
pub fn point_in_ring(x: f64, y: f64, ring: &[Pt]) -> bool {
    let mut inside = false;
    let n = ring.len();
    for i in 0..n {
        let [x1, y1] = ring[i];
        let [x2, y2] = ring[(i + 1) % n];
        if (y1 > y) != (y2 > y) {
            let xc = x1 + (y - y1) / (y2 - y1) * (x2 - x1);
            if x < xc {
                inside = !inside;
            }
        }
    }
    inside
}

/// Inside the outer ring and outside every hole — by parity, so the rings need
/// not say which is which.
#[must_use]
pub fn point_in_rings(x: f64, y: f64, rings: &[Vec<Pt>]) -> bool {
    rings
        .iter()
        .filter(|r| r.len() >= 3 && point_in_ring(x, y, r))
        .count()
        % 2
        == 1
}

/// Every `[x, y]` in a GeoJSON geometry, at any nesting depth.
#[must_use]
pub fn geom_coords(geom: &Value) -> Vec<Pt> {
    fn walk(node: &Value, out: &mut Vec<Pt>) {
        if let Some(a) = node.as_array() {
            if a.len() >= 2 && a[0].is_number() && a[1].is_number() {
                if let Some(p) = pt(node) {
                    out.push(p);
                }
                return;
            }
            for child in a {
                walk(child, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(&geom["coordinates"], &mut out);
    out
}

/// Round to `dp` decimals on the double's exact decimal value, as the
/// TypeScript's `toFixed` and the Python's `round` both did. The short form
/// `(v * 10^dp).round() / 10^dp` rounds twice and disagrees with both at the last
/// digit for a good fraction of inputs.
#[must_use]
pub fn round(value: f64, dp: usize) -> f64 {
    format!("{value:.dp$}").parse().unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn simplify_keeps_corners_and_drops_collinear_filler() {
        let ring = [
            [0.0, 0.0],
            [1.0, 0.0],
            [2.0, 0.0],
            [2.0, 1.0],
            [2.0, 2.0],
            [0.0, 2.0],
        ];
        assert_eq!(
            simplify(&ring, 0.1),
            vec![[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]
        );
    }

    #[test]
    fn a_hole_is_outside() {
        let geom = json!({"type": "Polygon", "coordinates": [
            [[0, 0], [10, 0], [10, 10], [0, 10], [0, 0]],
            [[4, 4], [6, 4], [6, 6], [4, 6], [4, 4]]
        ]});
        let rings = rings_of(&geom);
        assert!(point_in_rings(2.0, 2.0, &rings));
        assert!(!point_in_rings(5.0, 5.0, &rings));
        assert!((ring_area(&rings[0]) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn rounding_is_on_the_decimal_value() {
        assert_eq!(round(1.005, 2), 1.0); // 1.005 is 1.00499999999999989… as a double
        assert_eq!(round(6.566_512_345_67, 7), 6.566_512_3);
        // -5e-8 is a hair under -0.00000005 as a double, so both JavaScript and
        // this round it to (negative) zero rather than to -1e-7.
        assert_eq!(round(-0.000_000_05, 7), 0.0);
    }
}
