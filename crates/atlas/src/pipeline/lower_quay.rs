//! A **lower quay** — the walk at the foot of a retaining wall, along the water —
//! found among the BGT's paving by measuring it against AHN.
//!
//! The register surveys the walk as paving like any other and says nothing of
//! its height, so the client stood it on the field — and the field, 2.9–3.1 m
//! between samples, cannot hold a walk two metres wide between the water and a
//! wall (`docs/notes/quay-walls.md`): it drew the walk at the level of the street
//! the wall holds up. At Gorechtkade by the Linnaeusplein in Groningen
//! (`wegdeel` `G0014.404f925c4b580015e053660013ac6945`, brick, 111 m² and 1.8 m
//! wide on average, between the `kademuur`s `…a43be…` on the water and
//! `…a61cc…` under the street) AHN reads the walk at −0.8 m NAP and the street
//! at +0.7 to +1.0 (measured 2026-11-05), and benches people sit on beside the
//! water stood on pavement at the top of the wall.
//!
//! So a surface that runs along a quay wall ([`QuayIndex::along`]) is measured:
//! if most of what is inside it is one flat level, and the ground just outside
//! part of its outline stands [`MIN_FACE_M`] above that level, it is a lower
//! quay, and its level is shipped beside it (`formats::bgt::Terrace`). Only
//! surfaces along a quay are asked, so a city's tens of thousands of pavements
//! cost AHN nothing.

use std::collections::HashMap;

use super::geom::{point_in_rings, Pt};

/// Narrowest surface measured, metres of mean width (area over half the
/// perimeter): a sliver between two outlines is no walk.
pub const MIN_WIDTH_M: f64 = 1.0;

/// How close to a quay wall's outline a surface's must come to run along it,
/// metres: the two are surveyed edge to edge.
const ALONG_M: f64 = 0.5;

/// Side of the grid the quay walls are binned on, metres.
const BIN_M: f64 = 50.0;

/// The quay walls' outlines, RD metres, binned for "does this surface run along
/// one" asked of every surface in a city.
pub struct QuayIndex {
    rings: Vec<Vec<Pt>>,
    bins: HashMap<(i64, i64), Vec<usize>>,
}

impl QuayIndex {
    #[must_use]
    pub fn new(rings: Vec<Vec<Pt>>) -> Self {
        let mut bins: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (k, ring) in rings.iter().enumerate() {
            let Some((min, max)) = bounds(ring) else {
                continue;
            };
            for bx in bin(min[0] - ALONG_M)..=bin(max[0] + ALONG_M) {
                for by in bin(min[1] - ALONG_M)..=bin(max[1] + ALONG_M) {
                    bins.entry((bx, by)).or_default().push(k);
                }
            }
        }
        Self { rings, bins }
    }

    /// Whether a surface's outer ring comes within [`ALONG_M`] of a quay wall's.
    #[must_use]
    pub fn along(&self, ring: &[Pt]) -> bool {
        let Some((min, max)) = bounds(ring) else {
            return false;
        };
        let mut seen = Vec::new();
        for bx in bin(min[0])..=bin(max[0]) {
            for by in bin(min[1])..=bin(max[1]) {
                for &k in self.bins.get(&(bx, by)).into_iter().flatten() {
                    if seen.contains(&k) {
                        continue;
                    }
                    seen.push(k);
                    let quay = &self.rings[k];
                    let near = |p: &Pt, other: &[Pt]| {
                        other
                            .windows(2)
                            .any(|w| distance_to_segment(*p, w[0], w[1]) <= ALONG_M)
                    };
                    if ring.iter().any(|p| near(p, quay)) || quay.iter().any(|p| near(p, ring)) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

fn bin(v: f64) -> i64 {
    (v / BIN_M).floor() as i64
}

fn bounds(ring: &[Pt]) -> Option<(Pt, Pt)> {
    if ring.is_empty() {
        return None;
    }
    let (mut min, mut max) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in ring {
        min = [min[0].min(p[0]), min[1].min(p[1])];
        max = [max[0].max(p[0]), max[1].max(p[1])];
    }
    Some((min, max))
}

/// Fewest AHN samples inside the polygon to call it anything: 6 m² at 0.5 m.
const MIN_SAMPLES: usize = 24;

/// How far from the level a sample may lie and still be the walk, metres.
const FLAT_M: f64 = 0.3;

/// Least share of the inside samples that must be the walk. The rest is the
/// wall's own top, a bench, a planter.
const FLAT_SHARE: f64 = 0.5;

/// Least step from the walk up to the ground behind it, metres. Below it the
/// quay is a kerb, which the ground draws on its own.
pub const MIN_FACE_M: f64 = 0.8;

/// How far outside the outline the ground behind it is read, metres.
const NEAR_M: f64 = 3.0;

/// How far out a landward edge is probed, metres: past the wall's own top,
/// short of the next thing.
const PROBE_M: f64 = 1.0;

/// Shortest wall run kept, metres.
const MIN_RUN_M: f64 = 2.0;

/// AHN over a square, north row first, as [`crate::terrain::HeightSource`]
/// answers it: `n × n` samples of `cell` metres, `NaN` where nothing was
/// measured.
pub struct Raster {
    pub min: Pt,
    pub cell: f64,
    pub n: usize,
    pub h: Vec<f32>,
}

impl Raster {
    /// The sample a point falls in, if it was measured.
    fn at(&self, p: Pt) -> Option<f64> {
        let i = ((p[0] - self.min[0]) / self.cell).floor();
        let j = ((self.min[1] + self.cell * self.n as f64 - p[1]) / self.cell).floor();
        if i < 0.0 || j < 0.0 || i >= self.n as f64 || j >= self.n as f64 {
            return None;
        }
        let v = self.h[j as usize * self.n + i as usize];
        v.is_finite().then_some(f64::from(v))
    }

    /// Every sample's centre and height.
    fn samples(&self) -> impl Iterator<Item = (Pt, f32)> + '_ {
        let top = self.min[1] + self.cell * self.n as f64;
        (0..self.n * self.n).map(move |k| {
            let (i, j) = (k % self.n, k / self.n);
            let c = [
                self.min[0] + (i as f64 + 0.5) * self.cell,
                top - (j as f64 + 0.5) * self.cell,
            ];
            (c, self.h[k])
        })
    }
}

/// What a lower quay was measured as.
#[derive(Debug, Clone, PartialEq)]
pub struct LowerQuay {
    /// The walk, metres above NAP.
    pub level: f64,
    /// The ground behind its wall, metres above NAP.
    pub street: f64,
    /// The landward edge: runs of indices into the outer ring, in its order.
    pub walls: Vec<Vec<usize>>,
}

/// The outer ring without its closing repeat.
fn open(ring: &[Pt]) -> &[Pt] {
    match ring {
        [first, .., last] if first == last => &ring[..ring.len() - 1],
        _ => ring,
    }
}

fn ring_area(ring: &[Pt]) -> f64 {
    let n = ring.len();
    (0..n)
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        .abs()
        / 2.0
}

/// Whether a polygon (metres, outer ring first) is wide enough to be measured.
#[must_use]
pub fn wide_enough(rings: &[Vec<Pt>]) -> bool {
    let Some(outer) = rings.first().map(|r| open(r)) else {
        return false;
    };
    if outer.len() < 3 {
        return false;
    }
    let area = ring_area(outer) - rings[1..].iter().map(|r| ring_area(open(r))).sum::<f64>();
    let perimeter: f64 = (0..outer.len())
        .map(|i| {
            let (a, b) = (outer[i], outer[(i + 1) % outer.len()]);
            (b[0] - a[0]).hypot(b[1] - a[1])
        })
        .sum();
    perimeter > 0.0 && area / (perimeter / 2.0) >= MIN_WIDTH_M
}

/// The square to ask AHN for around a polygon: its bounding box grown by the
/// distance its surroundings are read at, `(min, max)`.
#[must_use]
pub fn window(rings: &[Vec<Pt>]) -> (Pt, Pt) {
    let (mut min, mut max) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in rings.iter().flatten() {
        min = [min[0].min(p[0]), min[1].min(p[1])];
        max = [max[0].max(p[0]), max[1].max(p[1])];
    }
    let side = (max[0] - min[0]).max(max[1] - min[1]) + 2.0 * (NEAR_M + 1.0);
    let c = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0];
    (
        [c[0] - side / 2.0, c[1] - side / 2.0],
        [c[0] + side / 2.0, c[1] + side / 2.0],
    )
}

pub fn distance_to_segment(p: Pt, a: Pt, b: Pt) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Measure a polygon (metres, outer ring first) against AHN. `None` when it is
/// not a lower quay — too few measurements, no one level inside it, or no
/// ground outside it that stands [`MIN_FACE_M`] above that level.
#[must_use]
pub fn measure(rings: &[Vec<Pt>], raster: &Raster) -> Option<LowerQuay> {
    let outer = open(rings.first()?);
    if outer.len() < 3 {
        return None;
    }
    let edges: Vec<(Pt, Pt)> = (0..outer.len())
        .map(|i| (outer[i], outer[(i + 1) % outer.len()]))
        .collect();
    let (mut inside, mut near) = (Vec::new(), Vec::new());
    for (c, h) in raster.samples() {
        if !h.is_finite() {
            continue;
        }
        if point_in_rings(c[0], c[1], rings) {
            inside.push(f64::from(h));
        } else if edges.iter().any(|&(a, b)| distance_to_segment(c, a, b) <= NEAR_M) {
            near.push(f64::from(h));
        }
    }
    if inside.len() < MIN_SAMPLES {
        return None;
    }
    let level = median(inside.clone());
    let flat = inside.iter().filter(|h| (*h - level).abs() <= FLAT_M).count();
    if (flat as f64) < FLAT_SHARE * inside.len() as f64 {
        return None;
    }
    let behind: Vec<f64> = near.into_iter().filter(|h| *h >= level + MIN_FACE_M).collect();
    if behind.len() < MIN_SAMPLES / 2 {
        return None;
    }
    let street = median(behind);
    // An edge is landward where the ground just outside it is nearer the street
    // than the walk. Its outward side by probing 5 cm off it, never by winding.
    let landward: Vec<bool> = edges
        .iter()
        .map(|&(a, b)| {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len = dx.hypot(dy);
            if len < 1e-6 {
                return false;
            }
            let mut n = [dy / len, -dx / len];
            let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
            if point_in_rings(mid[0] + n[0] * 0.05, mid[1] + n[1] * 0.05, rings) {
                n = [-n[0], -n[1]];
            }
            let steps = (len / 0.5).ceil().max(1.0) as usize;
            let hs: Vec<f64> = (0..steps)
                .filter_map(|k| {
                    let t = (k as f64 + 0.5) / steps as f64;
                    raster.at([
                        a[0] + dx * t + n[0] * PROBE_M,
                        a[1] + dy * t + n[1] * PROBE_M,
                    ])
                })
                .collect();
            !hs.is_empty() && median(hs) >= (level + street) / 2.0
        })
        .collect();
    // Runs of landward edges, as vertex indices, starting after a seaward edge
    // so that no run is cut where the ring wraps.
    let Some(start) = landward.iter().position(|l| !l) else {
        return None;
    };
    let n = edges.len();
    let mut walls = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    for k in 1..=n {
        let e = (start + k) % n;
        if landward[e] {
            if run.is_empty() {
                run.push(e);
            }
            run.push((e + 1) % n);
        } else if !run.is_empty() {
            walls.push(std::mem::take(&mut run));
        }
    }
    if !run.is_empty() {
        walls.push(run);
    }
    walls.retain(|r| {
        r.windows(2)
            .map(|w| {
                let (a, b) = (outer[w[0]], outer[w[1]]);
                (b[0] - a[0]).hypot(b[1] - a[1])
            })
            .sum::<f64>()
            >= MIN_RUN_M
    });
    (!walls.is_empty()).then_some(LowerQuay {
        level,
        street,
        walls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 40 × 60 m square of AHN: water (no data) west of x = 10, the walk at
    /// −0.8 to x = 12.5, the street at +0.8 beyond.
    fn quay_raster() -> Raster {
        let (n, cell) = (120usize, 0.5);
        let mut h = Vec::with_capacity(n * n);
        for _j in 0..n {
            for i in 0..n {
                let x = (i as f64 + 0.5) * cell;
                h.push(if x < 10.0 {
                    f32::NAN
                } else if x < 12.5 {
                    -0.8
                } else {
                    0.8
                });
            }
        }
        Raster {
            min: [0.0, 0.0],
            cell,
            n,
            h,
        }
    }

    /// A surface x 10..`x1`, y 5..55: the walk itself is x 10..12.5.
    fn strip(x1: f64) -> Vec<Vec<Pt>> {
        vec![vec![
            [10.0, 5.0],
            [x1, 5.0],
            [x1, 55.0],
            [10.0, 55.0],
            [10.0, 5.0],
        ]]
    }

    #[test]
    fn a_walk_below_the_street_is_a_lower_quay_walled_on_its_landward_side() {
        let rings = strip(12.5);
        assert!(wide_enough(&rings));
        let q = measure(&rings, &quay_raster()).expect("a lower quay");
        assert!((q.level + 0.8).abs() < 1e-6, "{q:?}");
        assert!((q.street - 0.8).abs() < 1e-6, "{q:?}");
        // One run, the east edge, and nothing along the water or the ends.
        assert_eq!(q.walls, vec![vec![1, 2]]);
    }

    #[test]
    fn a_sliver_is_not_measured() {
        assert!(!wide_enough(&strip(10.5)));
    }

    /// A surface edge to edge with a quay wall runs along it; one a street
    /// away does not, and is never asked of AHN.
    #[test]
    fn only_a_surface_along_a_quay_wall_is_measured() {
        let wall = vec![[12.5, 5.0], [12.8, 5.0], [12.8, 55.0], [12.5, 55.0], [12.5, 5.0]];
        let quays = QuayIndex::new(vec![wall]);
        assert!(quays.along(&strip(12.5)[0]));
        let across = vec![[20.0, 5.0], [24.0, 5.0], [24.0, 55.0], [20.0, 55.0], [20.0, 5.0]];
        assert!(!quays.along(&across));
    }

    #[test]
    fn a_quay_at_street_level_is_no_lower_quay() {
        let mut flat = quay_raster();
        for v in &mut flat.h {
            if v.is_finite() {
                *v = 0.8;
            }
        }
        assert_eq!(measure(&strip(12.5), &flat), None);
    }

    #[test]
    fn a_bank_sloping_to_the_water_is_no_lower_quay() {
        let mut slope = quay_raster();
        for (k, v) in slope.h.iter_mut().enumerate() {
            if v.is_finite() {
                let x = ((k % 120) as f64 + 0.5) * 0.5;
                *v = (-0.8 + (x - 10.0) * 0.3) as f32;
            }
        }
        assert_eq!(measure(&strip(16.0), &slope), None);
    }
}
