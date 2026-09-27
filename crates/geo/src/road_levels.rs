//! How high every road stands, solved on the road network rather than read off
//! the ground under it.
//!
//! # Why the network and not the height field
//!
//! Until this module a carriageway had no height of its own. It was a flat ribbon
//! draped over the terrain field, and everything grade-separated was a correction
//! to that field: an approach was the *ground* raised towards a deck
//! (`road_grade::ramp_from_pins`), the joint was the ground held at the deck's top
//! under its end (`terrain_grade::ABUTMENT_SAMPLES`), and a tunnel's ramp was a
//! plan invented in `tunnels` and lifted back by whatever the survey had dug. Each
//! of those meets its neighbour through a raster sampled every few metres and
//! resampled again by the client, so where two pieces of road meet, their heights
//! are two interpolations of one field that need not agree — and at the
//! Europaplein (2026-09-27) they did not, by a few tenths of a metre at every
//! joint, however the field was pinned.
//!
//! Here the road is the thing that has a height, and continuity is structural:
//! two ways that meet share a **node**, a node has one height, and so the deck and
//! the road running off it cannot disagree at the joint because they are asking
//! for the same number. The ground is conformed to the roads afterwards, not the
//! other way round.
//!
//! # What goes in
//!
//! Lines with a **level** — 0 on the ground, above it for a bridge, below for a
//! tunnel — and, at each vertex, what is known about the height there
//! ([`Height`]): a measurement to be trusted a little ([`Height::Measured`]), a
//! height the road must have ([`Height::Fixed`]), or nothing. Where those come
//! from is the caller's business and differs by who is asking: atlas reads the
//! national height survey (the bare ground along a road, the surface over a
//! span), the client reads what atlas published. This module only needs them in
//! one flat metric frame.
//!
//! # How it is solved
//!
//! 1. **Nodes.** Vertices at the same point merge when either is the end of its
//!    line, or both are at the same level — the rule the routing graph already
//!    uses (`grade-separation.md`): a street carrying on past a point never met
//!    what crosses it there, and a bridge's vertex over the road beneath is not a
//!    junction with it.
//! 2. **Outliers are taken out** of the measurements with a median along the
//!    network, so a lorry in the surface model or a tree over the lane does not
//!    become a bump.
//! 3. **Unknown heights** are interpolated along the network from the known ones.
//! 4. **Measurements are smoothed** against their neighbours.
//! 5. **The grade is limited**: the result is the midpoint of the tightest upper
//!    and lower envelopes that climb no faster than each line's `max_grade`. That
//!    is what turns a deck's height into an embankment on the road leading to it
//!    and a tunnel's floor into a cutting — the same operation for both, which is
//!    the point: a bridge and a tunnel are one road at two levels.
//! 6. **Fixed heights are honoured exactly**: the result is clamped between the
//!    envelopes the fixed heights alone allow, which pins them and grades
//!    everything around them.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// What is known about a road's height at one vertex.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Height {
    /// Nothing; interpolated from the network around it.
    Unknown,
    /// A measurement, smoothed against its neighbours in proportion to `weight`
    /// (larger holds it more firmly).
    Measured { y: f32, weight: f32 },
    /// The height the road has here, exactly — a deck's surveyed top, a tunnel's
    /// floor.
    Fixed(f32),
}

/// One road line going in.
#[derive(Clone, Debug)]
pub struct Line {
    /// Centre line, in the caller's flat metric frame.
    pub points: Vec<[f32; 2]>,
    /// What is known at each point; parallel to `points`.
    pub heights: Vec<Height>,
    /// 0 on the ground, positive on a structure over it, negative under it.
    pub level: i8,
    /// The steepest this road may climb, rise over run.
    pub max_grade: f32,
}

/// How firmly measurements are smoothed: the weight each neighbour carries against
/// a measurement of weight 1.
pub const SMOOTHING: f32 = 1.0;

/// Smoothing passes. Each reaches one vertex further; at the few metres a line is
/// densified to, this spans a car or a tree and not the shape of an embankment.
pub const SMOOTHING_PASSES: usize = 8;

/// Interpolation passes for the vertices nothing is known about, after they are
/// seeded from the nearest known one. Enough to straighten a gap the length of a
/// long underpass at a few metres a vertex.
pub const FILL_PASSES: usize = 200;

/// How far along the road the median that removes outliers reaches either way,
/// metres: wider than a lorry, narrower than the shortest ramp.
pub const MEDIAN_RADIUS_M: f32 = 8.0;

/// Two vertices closer than this are one point, metres.
pub const SAME_POINT_M: f32 = 0.001;

/// Break a line into pieces no longer than `step`, keeping every original vertex.
///
/// The solver works per vertex, and a generalised tile routinely has a hundred
/// metres of straight road between two: densify before sampling the heights along
/// it, or the profile has nothing to follow the ground with.
#[must_use]
pub fn densify(points: &[[f32; 2]], step: f32) -> Vec<[f32; 2]> {
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(points.len());
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = (b[0] - a[0]).hypot(b[1] - a[1]);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = (len / step.max(1e-3)).ceil().max(1.0) as usize;
        for k in 0..n {
            #[allow(clippy::cast_precision_loss)]
            let t = k as f32 / n as f32;
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
        }
    }
    if let Some(&last) = points.last() {
        out.push(last);
    }
    out
}

/// The solved heights, one per point of each input line.
#[derive(Clone, Debug, Default)]
pub struct Solved {
    pub heights: Vec<Vec<f32>>,
}

/// Solve every line's height. See the module docs for the steps.
#[must_use]
pub fn solve(lines: &[Line]) -> Solved {
    let net = Net::build(lines);
    let n = net.nodes;
    if n == 0 {
        return Solved {
            heights: lines.iter().map(|l| vec![0.0; l.points.len()]).collect(),
        };
    }

    // What is known at each node.
    let mut fixed: Vec<Option<f32>> = vec![None; n];
    let mut measured: Vec<Option<(f32, f32)>> = vec![None; n];
    {
        let mut fixed_sum = vec![(0.0f32, 0u32); n];
        let mut meas_sum = vec![(0.0f32, 0.0f32); n];
        for (li, line) in lines.iter().enumerate() {
            for (vi, h) in line.heights.iter().enumerate() {
                let node = net.node_of[li][vi];
                match *h {
                    Height::Fixed(y) if y.is_finite() => {
                        fixed_sum[node].0 += y;
                        fixed_sum[node].1 += 1;
                    }
                    Height::Measured { y, weight } if y.is_finite() && weight > 0.0 => {
                        meas_sum[node].0 += y * weight;
                        meas_sum[node].1 += weight;
                    }
                    _ => {}
                }
            }
        }
        for i in 0..n {
            if fixed_sum[i].1 > 0 {
                #[allow(clippy::cast_precision_loss)]
                let mean = fixed_sum[i].0 / fixed_sum[i].1 as f32;
                fixed[i] = Some(mean);
            } else if meas_sum[i].1 > 0.0 {
                measured[i] = Some((meas_sum[i].0 / meas_sum[i].1, meas_sum[i].1));
            }
        }
    }

    // 2. Take the outliers out of the measurements with a median along the
    // network — what disturbs a surface model over a road is an outlier (a lorry,
    // a lamppost, a tree over the lane), and an average only spreads one out. First,
    // so that nothing seeded from a measurement below inherits one either.
    let soft: Vec<usize> = (0..n).filter(|&i| measured[i].is_some()).collect();
    let medians: Vec<f32> = soft
        .iter()
        .map(|&i| {
            let mut vals: Vec<f32> = net
                .within(i, MEDIAN_RADIUS_M)
                .into_iter()
                .filter_map(|j| measured[j].map(|(v, _)| v))
                .collect();
            vals.sort_by(f32::total_cmp);
            vals[vals.len() / 2]
        })
        .collect();
    for (&i, &m) in soft.iter().zip(&medians) {
        if let Some((v, _)) = measured[i].as_mut() {
            *v = m;
        }
    }

    // 3. Seed every node from the nearest known one along the network, then relax
    // the unknown ones towards the mean of their neighbours.
    let mut y = vec![0.0f32; n];
    let known: Vec<(usize, f32)> = (0..n)
        .filter_map(|i| fixed[i].or(measured[i].map(|(v, _)| v)).map(|v| (i, v)))
        .collect();
    if known.is_empty() {
        return Solved {
            heights: lines.iter().map(|l| vec![0.0; l.points.len()]).collect(),
        };
    }
    let nearest = net.nearest(&known);
    for i in 0..n {
        y[i] = nearest[i];
    }
    let unknown: Vec<usize> = (0..n)
        .filter(|&i| fixed[i].is_none() && measured[i].is_none())
        .collect();
    let mut next = vec![0.0f32; unknown.len()];
    for _ in 0..FILL_PASSES {
        for (slot, &i) in next.iter_mut().zip(&unknown) {
            let (mut sum, mut w) = (0.0f32, 0.0f32);
            for &(j, len, _) in &net.adj[i] {
                let k = 1.0 / len.max(0.01);
                sum += y[j] * k;
                w += k;
            }
            *slot = if w > 0.0 { sum / w } else { y[i] };
        }
        for (&i, &v) in unknown.iter().zip(&next) {
            y[i] = v;
        }
    }

    // 4. Smooth what is left against the neighbours.
    let mut next = vec![0.0f32; soft.len()];
    for _ in 0..SMOOTHING_PASSES {
        for (slot, &i) in next.iter_mut().zip(&soft) {
            let (m, w) = measured[i].expect("soft nodes are measured");
            let deg = net.adj[i].len() as f32;
            let around: f32 = net.adj[i].iter().map(|&(j, _, _)| y[j]).sum();
            *slot = (m * w + SMOOTHING * around) / (w + SMOOTHING * deg);
        }
        for (&i, &v) in soft.iter().zip(&next) {
            y[i] = v;
        }
    }
    for i in 0..n {
        if let Some(f) = fixed[i] {
            y[i] = f;
        }
    }

    // 5. Limit the grade: the midpoint of the tightest envelopes over every node.
    let all: Vec<(usize, f32)> = (0..n).map(|i| (i, y[i])).collect();
    let upper = net.envelope(&all, Envelope::Upper);
    let lower = net.envelope(&all, Envelope::Lower);
    for i in 0..n {
        y[i] = 0.5 * (upper[i] + lower[i]);
    }

    // 6. Honour the fixed heights exactly, grading everything around them.
    let pins: Vec<(usize, f32)> = (0..n).filter_map(|i| fixed[i].map(|f| (i, f))).collect();
    if !pins.is_empty() {
        let hi = net.envelope(&pins, Envelope::Upper);
        let lo = net.envelope(&pins, Envelope::Lower);
        for i in 0..n {
            y[i] = if lo[i] > hi[i] {
                // Two fixed heights closer than the grade allows: meet between.
                0.5 * (lo[i] + hi[i])
            } else {
                y[i].clamp(lo[i], hi[i])
            };
        }
    }

    Solved {
        heights: lines
            .iter()
            .enumerate()
            .map(|(li, l)| {
                (0..l.points.len())
                    .map(|vi| y[net.node_of[li][vi]])
                    .collect()
            })
            .collect(),
    }
}

#[derive(Clone, Copy)]
enum Envelope {
    /// The lowest function at or above every source that climbs no faster than
    /// the grade: `min(y_m + grade·d)`.
    Upper,
    /// Its mirror: `max(y_m − grade·d)`.
    Lower,
}

/// The road network: nodes, and the segments between them.
struct Net {
    nodes: usize,
    /// Node of each vertex, per line.
    node_of: Vec<Vec<usize>>,
    /// Per node: (neighbour, length, grade budget = length × max_grade).
    adj: Vec<Vec<(usize, f32, f32)>>,
}

impl Net {
    fn build(lines: &[Line]) -> Self {
        // Vertices grouped by point.
        let key = |p: [f32; 2]| {
            #[allow(clippy::cast_possible_truncation)]
            (
                (p[0] / SAME_POINT_M).round() as i64,
                (p[1] / SAME_POINT_M).round() as i64,
            )
        };
        let mut ids: Vec<Vec<usize>> = Vec::with_capacity(lines.len());
        let mut total = 0usize;
        for l in lines {
            ids.push((total..total + l.points.len()).collect());
            total += l.points.len();
        }
        let mut parent: Vec<usize> = (0..total).collect();
        fn find(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        let mut groups: HashMap<(i64, i64), Vec<(usize, bool, i8)>> = HashMap::new();
        for (li, l) in lines.iter().enumerate() {
            let last = l.points.len().saturating_sub(1);
            for (vi, &p) in l.points.iter().enumerate() {
                groups.entry(key(p)).or_default().push((
                    ids[li][vi],
                    vi == 0 || vi == last,
                    l.level,
                ));
            }
        }
        for members in groups.values() {
            for a in 0..members.len() {
                for b in a + 1..members.len() {
                    let (ia, end_a, lev_a) = members[a];
                    let (ib, end_b, lev_b) = members[b];
                    // A dangle meets whatever it lands on; two lines carrying on
                    // through one point only meet at the same level.
                    if end_a || end_b || lev_a == lev_b {
                        let (ra, rb) = (find(&mut parent, ia), find(&mut parent, ib));
                        if ra != rb {
                            parent[ra] = rb;
                        }
                    }
                }
            }
        }
        let mut compact: HashMap<usize, usize> = HashMap::new();
        let mut node_of: Vec<Vec<usize>> = Vec::with_capacity(lines.len());
        for li in 0..lines.len() {
            let mut row = Vec::with_capacity(ids[li].len());
            for &v in &ids[li] {
                let r = find(&mut parent, v);
                let next = compact.len();
                row.push(*compact.entry(r).or_insert(next));
            }
            node_of.push(row);
        }
        let nodes = compact.len();
        let mut adj: Vec<Vec<(usize, f32, f32)>> = vec![Vec::new(); nodes];
        for (li, l) in lines.iter().enumerate() {
            for vi in 1..l.points.len() {
                let (a, b) = (node_of[li][vi - 1], node_of[li][vi]);
                if a == b {
                    continue;
                }
                let (p, q) = (l.points[vi - 1], l.points[vi]);
                let len = (q[0] - p[0]).hypot(q[1] - p[1]);
                let budget = len * l.max_grade.max(0.0);
                adj[a].push((b, len, budget));
                adj[b].push((a, len, budget));
            }
        }
        Self {
            nodes,
            node_of,
            adj,
        }
    }

    /// Every node within `radius` of `from` along the network, `from` included.
    fn within(&self, from: usize, radius: f32) -> Vec<usize> {
        let mut seen: HashMap<usize, f32> = HashMap::new();
        let mut heap = BinaryHeap::new();
        seen.insert(from, 0.0);
        heap.push(Item(0.0, from));
        while let Some(Item(d, i)) = heap.pop() {
            if seen.get(&i).is_some_and(|&b| d > b) {
                continue;
            }
            for &(j, len, _) in &self.adj[i] {
                let nd = d + len;
                if nd <= radius && seen.get(&j).is_none_or(|&b| nd < b) {
                    seen.insert(j, nd);
                    heap.push(Item(nd, j));
                }
            }
        }
        seen.into_keys().collect()
    }

    /// Each node's value from the known node nearest it along the network.
    fn nearest(&self, sources: &[(usize, f32)]) -> Vec<f32> {
        let mut dist = vec![f32::INFINITY; self.nodes];
        let mut value = vec![0.0f32; self.nodes];
        let mut heap = BinaryHeap::new();
        for &(i, v) in sources {
            dist[i] = 0.0;
            value[i] = v;
            heap.push(Item(0.0, i));
        }
        while let Some(Item(d, i)) = heap.pop() {
            if d > dist[i] {
                continue;
            }
            for &(j, len, _) in &self.adj[i] {
                let nd = d + len;
                if nd < dist[j] {
                    dist[j] = nd;
                    value[j] = value[i];
                    heap.push(Item(nd, j));
                }
            }
        }
        value
    }

    /// The grade envelope of `sources` — see [`Envelope`]. A node no source reaches
    /// answers infinity (upper) or minus infinity (lower), which leaves it
    /// unconstrained.
    fn envelope(&self, sources: &[(usize, f32)], which: Envelope) -> Vec<f32> {
        // Both are one shortest-path search: the upper envelope directly, the
        // lower one on negated heights.
        let sign = match which {
            Envelope::Upper => 1.0,
            Envelope::Lower => -1.0,
        };
        let mut best = vec![f32::INFINITY; self.nodes];
        let mut heap = BinaryHeap::new();
        for &(i, v) in sources {
            let key = sign * v;
            if key < best[i] {
                best[i] = key;
                heap.push(Item(key, i));
            }
        }
        while let Some(Item(k, i)) = heap.pop() {
            if k > best[i] {
                continue;
            }
            for &(j, _, budget) in &self.adj[i] {
                let nk = k + budget;
                if nk < best[j] {
                    best[j] = nk;
                    heap.push(Item(nk, j));
                }
            }
        }
        best.into_iter().map(|k| sign * k).collect()
    }
}

/// A min-heap entry on an `f32` key.
#[derive(Clone, Copy)]
struct Item(f32, usize);

impl PartialEq for Item {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_cmp(&other.0) == Ordering::Equal
    }
}
impl Eq for Item {}
impl PartialOrd for Item {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Item {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: `BinaryHeap` is a max-heap.
        other.0.total_cmp(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: f32 = 0.06;

    fn line(points: Vec<[f32; 2]>, level: i8, h: impl Fn([f32; 2]) -> Height) -> Line {
        let heights = points.iter().map(|&p| h(p)).collect();
        Line {
            points,
            heights,
            level,
            max_grade: G,
        }
    }

    fn straight(from: f32, to: f32, z: f32) -> Vec<[f32; 2]> {
        densify(&[[from, z], [to, z]], 2.0)
    }

    fn slope(ys: &[f32], pts: &[[f32; 2]]) -> f32 {
        ys.windows(2)
            .zip(pts.windows(2))
            .map(|(y, p)| (y[1] - y[0]).abs() / (p[1][0] - p[0][0]).hypot(p[1][1] - p[0][1]))
            .fold(0.0, f32::max)
    }

    /// **The joint is one number.** A span fixed at its deck's top and the ground
    /// road running off its end share the node where they meet, so they are at
    /// the same height there by construction — the property the raster grading
    /// could only approach.
    #[test]
    fn a_span_and_its_approach_meet_at_one_height() {
        let span = line(straight(0.0, 60.0, 0.0), 1, |_| Height::Fixed(8.0));
        let road = line(straight(60.0, 300.0, 0.0), 0, |_| Height::Measured {
            y: 0.0,
            weight: 1.0,
        });
        let s = solve(&[span, road]);
        let (deck_end, road_start) = (*s.heights[0].last().unwrap(), s.heights[1][0]);
        assert_eq!(deck_end, road_start);
        assert!(
            (deck_end - 8.0).abs() < 1e-4,
            "the deck moved to {deck_end}"
        );
    }

    /// **A deck raises its approach into an embankment at the grade, and the
    /// embankment meets the ground.** Eight metres at 6 % is 133 m of ramp.
    #[test]
    fn a_deck_grades_its_approach() {
        let span = line(straight(0.0, 60.0, 0.0), 1, |_| Height::Fixed(8.0));
        let pts = straight(60.0, 400.0, 0.0);
        let road = line(pts.clone(), 0, |_| Height::Measured {
            y: 0.0,
            weight: 1.0,
        });
        let s = solve(&[span, road]);
        let ys = &s.heights[1];
        assert!(slope(ys, &pts) <= G + 1e-4, "steeper than the grade");
        let at = |x: f32| {
            let i = pts.iter().position(|p| p[0] >= x).unwrap();
            ys[i]
        };
        assert!(
            (at(60.0 + 66.7) - 4.0).abs() < 0.3,
            "halfway up the ramp: {}",
            at(126.7)
        );
        assert!(
            at(300.0).abs() < 0.05,
            "the road past the ramp left the ground"
        );
    }

    /// **A tunnel is the same operation, downwards**: its floor lowers the road
    /// into a cutting at the grade.
    #[test]
    fn a_tunnel_grades_its_approach_downwards() {
        let bore = line(straight(0.0, 200.0, 0.0), -1, |_| Height::Fixed(-7.0));
        let pts = straight(200.0, 500.0, 0.0);
        let road = line(pts.clone(), 0, |_| Height::Measured {
            y: 0.0,
            weight: 1.0,
        });
        let s = solve(&[bore, road]);
        let ys = &s.heights[1];
        assert_eq!(ys[0], -7.0);
        assert!(slope(ys, &pts) <= G + 1e-4);
        assert!(
            ys.iter().all(|&y| y <= 1e-4),
            "a cutting rose above the ground"
        );
        assert!(ys.last().unwrap().abs() < 0.05);
    }

    /// **A road passing under a bridge does not join it.** They cross at a point
    /// both lines have a vertex at, and neither ends there: a flyover over a
    /// street is not a junction with it, or the street would climb onto the deck.
    #[test]
    fn a_road_under_a_bridge_stays_on_the_ground() {
        let span = line(straight(-40.0, 40.0, 0.0), 1, |_| Height::Fixed(7.0));
        let under = line(densify(&[[0.0, -200.0], [0.0, 200.0]], 2.0), 0, |_| {
            Height::Measured {
                y: 0.0,
                weight: 1.0,
            }
        });
        let s = solve(&[span, under]);
        assert!(
            s.heights[1].iter().all(|y| y.abs() < 1e-3),
            "the road under the flyover was lifted to {:?}",
            s.heights[1].iter().copied().fold(0.0f32, f32::max)
        );
    }

    /// **Noise is smoothed and nothing climbs past the grade**, and a fixed
    /// height is honoured to the bit.
    #[test]
    fn measurements_are_smoothed_and_graded() {
        let pts = straight(0.0, 200.0, 0.0);
        // A lorry at x = 100: a 3 m spike in the surface model.
        let road = line(pts.clone(), 0, |p| Height::Measured {
            y: if (p[0] - 100.0).abs() < 3.0 { 3.0 } else { 1.0 },
            weight: 1.0,
        });
        let s = solve(&[road]);
        assert!(slope(&s.heights[0], &pts) <= G + 1e-4);
        let peak = s.heights[0].iter().copied().fold(f32::MIN, f32::max);
        assert!(peak < 1.5, "the lorry stayed a {peak} m bump");
    }

    /// **Unknown stretches are bridged along the network**: a road whose middle
    /// the survey could not see (under a viaduct) runs straight across the gap.
    #[test]
    fn an_unmeasured_stretch_is_interpolated() {
        let pts = straight(0.0, 100.0, 0.0);
        let road = line(pts.clone(), 0, |p| {
            if (30.0..70.0).contains(&p[0]) {
                Height::Unknown
            } else if p[0] < 30.0 {
                Height::Measured {
                    y: 1.0,
                    weight: 1.0,
                }
            } else {
                Height::Measured {
                    y: 2.0,
                    weight: 1.0,
                }
            }
        });
        let s = solve(&[road]);
        let mid = s.heights[0][pts.iter().position(|p| p[0] >= 50.0).unwrap()];
        assert!((mid - 1.5).abs() < 0.2, "the gap reads {mid}");
    }
}
