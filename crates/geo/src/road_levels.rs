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
//! 6. **Fixed heights are honoured exactly, and ceilings kept under**: the result
//!    is clamped between the envelopes the fixed heights and the ceilings allow,
//!    which pins the one, keeps the other and grades everything around them —
//!    embankments up from the decks, cuttings down to the tunnels.

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
    /// The height the road has here, exactly — a deck's surveyed top.
    Fixed(f32),
    /// The highest the road may be here, with whatever was measured of it: under
    /// a tunnel's lid, the lid less the headroom and the roof. A ceiling and not a
    /// fixed height, because that is all a lid says — a short underpass is not
    /// obliged to sit at a full level's depth, only to clear what is over it — and
    /// the ramp down to it is then the grade limit working from the ceiling, the
    /// mirror of an embankment working from a deck.
    AtMost { ceiling: f32, measured: Option<f32> },
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
    /// Whether each end was cut by a tile border rather than being where the
    /// road ends. A cut end is joined to the matching cut end of the same road in
    /// the next tile — see [`BORDER_JOIN_M`].
    pub cut: [bool; 2],
}

/// How far apart two cut ends at a tile border may be and still be one road,
/// metres. Each tile quantises its own copy of a line to its own lattice, so the
/// two halves of a road crossing a border are cut at points a few decimetres
/// apart rather than at one point.
pub const BORDER_JOIN_M: f32 = 1.0;

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
    let mut ceiling: Vec<f32> = vec![f32::INFINITY; n];
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
                    Height::AtMost {
                        ceiling: c,
                        measured: m,
                    } => {
                        if c.is_finite() {
                            ceiling[node] = ceiling[node].min(c);
                        }
                        if let Some(y) = m.filter(|y| y.is_finite()) {
                            meas_sum[node].0 += y;
                            meas_sum[node].1 += 1.0;
                        }
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

    // 6. Honour the fixed heights exactly and keep under every ceiling, grading
    // everything around them: the upper bound is the tighter of what the pins and
    // the ceilings allow, the lower bound what the pins alone do.
    let pins: Vec<(usize, f32)> = (0..n).filter_map(|i| fixed[i].map(|f| (i, f))).collect();
    let tops: Vec<(usize, f32)> = pins
        .iter()
        .copied()
        .chain(
            (0..n)
                .filter(|&i| ceiling[i].is_finite())
                .map(|i| (i, ceiling[i])),
        )
        .collect();
    if !tops.is_empty() {
        let hi = net.envelope(&tops, Envelope::Upper);
        let lo = if pins.is_empty() {
            vec![f32::NEG_INFINITY; n]
        } else {
            net.envelope(&pins, Envelope::Lower)
        };
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
        // Cut ends at a tile border: join each to the nearest cut end of another
        // line at the same level within reach.
        let cut_ends: Vec<(usize, [f32; 2], i8)> = lines
            .iter()
            .enumerate()
            .flat_map(|(li, l)| {
                let last = l.points.len().saturating_sub(1);
                [(0usize, 0usize), (1, last)]
                    .into_iter()
                    .filter(move |&(k, _)| l.cut[k] && !l.points.is_empty())
                    .map(move |(_, vi)| (li, vi))
            })
            .map(|(li, vi)| (ids[li][vi], lines[li].points[vi], lines[li].level))
            .collect();
        let cell = |p: [f32; 2]| {
            #[allow(clippy::cast_possible_truncation)]
            (
                (p[0] / BORDER_JOIN_M).floor() as i64,
                (p[1] / BORDER_JOIN_M).floor() as i64,
            )
        };
        let mut buckets: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (k, &(_, p, _)) in cut_ends.iter().enumerate() {
            buckets.entry(cell(p)).or_default().push(k);
        }
        for (k, &(id, p, level)) in cut_ends.iter().enumerate() {
            let (cx, cz) = cell(p);
            let mut best: Option<(f32, usize)> = None;
            for dx in -1..=1 {
                for dz in -1..=1 {
                    for &o in buckets.get(&(cx + dx, cz + dz)).into_iter().flatten() {
                        let (oid, q, olevel) = cut_ends[o];
                        if o == k
                            || olevel != level
                            || find(&mut parent, oid) == find(&mut parent, id)
                        {
                            continue;
                        }
                        let d = (p[0] - q[0]).hypot(p[1] - q[1]);
                        if d <= BORDER_JOIN_M && best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, oid));
                        }
                    }
                }
            }
            if let Some((_, oid)) = best {
                let (ra, rb) = (find(&mut parent, id), find(&mut parent, oid));
                if ra != rb {
                    parent[ra] = rb;
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

// ─── Streets as a tile draws them ────────────────────────────────────────────

/// How far apart the points of a [`StreetRun`] are, in tile units: about three
/// metres at z14. In units rather than metres so atlas and the client — which
/// each work out a tile's size for themselves — densify a run into exactly the
/// same points, which is what lets one's heights be read against the other's.
pub const DENSIFY_UNITS: f32 = 8.0;

/// One run of a street as a tile draws it: a `streets` line clipped to the tile
/// and densified, with its level and a key that names it.
#[derive(Clone, Debug)]
pub struct StreetRun {
    /// Names this run: the same tile read by atlas and by the client gives the
    /// same key. See [`run_key`].
    pub key: u64,
    /// Index into the tile's `streets` features.
    pub feature: usize,
    /// 0 on the ground, 1 on a bridge, -1 in a tunnel.
    pub level: i8,
    pub class: crate::road_texture::RoadClass,
    /// Half the carriageway, metres.
    pub half_w: f32,
    /// Densified centre line, tile-local metres.
    pub points: Vec<[f32; 2]>,
    /// Whether each end was cut by the tile border.
    pub cut: [bool; 2],
}

/// The steepest a road of this class is built to climb, rise over run: the
/// tunnels' long-standing 6 % for roads and 10 % for paths, and a railway's much
/// gentler 2.5 %.
#[must_use]
pub fn max_grade(class: crate::road_texture::RoadClass) -> f32 {
    use crate::road_texture::RoadClass;
    match class {
        RoadClass::Path => 0.10,
        RoadClass::Rail => 0.025,
        _ => 0.06,
    }
}

/// Every drawn street in a tile as [`StreetRun`]s. Tunnels are included at
/// level -1 and building passages at 0, as the ground-level ways they are.
#[must_use]
pub fn street_runs(tile: &crate::vector_tiles::TileData, tile_m: f32) -> Vec<StreetRun> {
    use crate::vector_tiles::{clip_polyline, drawn_street, flag_of, street_shape, TileData};
    let mut out = Vec::new();
    let Some(meta) = tile.layer("streets") else {
        return out;
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    let passages = crate::tunnels::building_passages(tile, features);
    for (fi, feature) in features.iter().enumerate() {
        let (shape, level) = match drawn_street(feature) {
            Some(s) => (s, i8::from(flag_of(feature, "bridge"))),
            None => match street_shape(feature) {
                Some(s) if passages.contains(&fi) => (s, 0),
                Some(s) => (s, -1),
                None => continue,
            },
        };
        let (class, half_w) = shape;
        let lines: Vec<&geo_types::LineString<f32>> = match &feature.geometry {
            geo_types::Geometry::LineString(ls) => vec![ls],
            geo_types::Geometry::MultiLineString(mls) => mls.iter().collect(),
            _ => continue,
        };
        for ls in lines {
            let pts: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x, c.y])
                    .collect();
            let (Some(&head), Some(&tail)) = (pts.first(), pts.last()) else {
                continue;
            };
            for run in clip_polyline(&pts, 0.0, extent) {
                if run.len() < 2 {
                    continue;
                }
                let cut = [run.first() != Some(&head), run.last() != Some(&tail)];
                let key = run_key(level, &run);
                let points = densify(&run, DENSIFY_UNITS)
                    .into_iter()
                    .map(|p| [p[0] * scale, p[1] * scale])
                    .collect();
                out.push(StreetRun {
                    key,
                    feature: fi,
                    level,
                    class,
                    half_w,
                    points,
                    cut,
                });
            }
        }
    }
    out
}

/// A run's name: FNV-1a over its level and its points in tile units, to a
/// sixteenth of a unit.
///
/// Spelled out rather than `std::hash`, whose output is not promised to be stable
/// between Rust releases — and atlas and the client are built separately.
#[must_use]
pub fn run_key(level: i8, tile_units: &[[f32; 2]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(&level.to_le_bytes());
    for p in tile_units {
        for v in p {
            #[allow(clippy::cast_possible_truncation)]
            eat(&((v * 16.0).round() as i32).to_le_bytes());
        }
    }
    h
}

// ─── The published heights ───────────────────────────────────────────────────

/// Solved heights per run, as atlas publishes them for one tile: the height at
/// each of the run's densified points, keyed by [`StreetRun::key`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoadHeights {
    pub runs: HashMap<u64, Vec<f32>>,
}

/// The format's first four bytes.
const MAGIC: &[u8; 4] = b"CRH1";

impl RoadHeights {
    /// `CRH1`, a `u32` run count, then per run a `u64` key, a `u32` point count
    /// and that many heights as `i32` centimetres. Little-endian throughout.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut keys: Vec<&u64> = self.runs.keys().collect();
        keys.sort_unstable();
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        #[allow(clippy::cast_possible_truncation)]
        out.extend_from_slice(&(keys.len() as u32).to_le_bytes());
        for k in keys {
            let ys = &self.runs[k];
            out.extend_from_slice(&k.to_le_bytes());
            #[allow(clippy::cast_possible_truncation)]
            out.extend_from_slice(&(ys.len() as u32).to_le_bytes());
            for y in ys {
                #[allow(clippy::cast_possible_truncation)]
                out.extend_from_slice(&((y * 100.0).round() as i32).to_le_bytes());
            }
        }
        out
    }

    /// The inverse of [`Self::encode`].
    ///
    /// # Errors
    /// A wrong magic or a truncated body.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut at = 0usize;
        let mut take = |n: usize| -> Result<&[u8], String> {
            let s = bytes
                .get(at..at + n)
                .ok_or_else(|| "road heights truncated".to_string())?;
            at += n;
            Ok(s)
        };
        if take(4)? != MAGIC {
            return Err("not a road heights file".into());
        }
        let u32_of = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let count = u32_of(take(4)?);
        let mut runs = HashMap::new();
        for _ in 0..count {
            let k = take(8)?;
            let key = u64::from_le_bytes([k[0], k[1], k[2], k[3], k[4], k[5], k[6], k[7]]);
            let n = u32_of(take(4)?) as usize;
            let mut ys = Vec::with_capacity(n);
            for _ in 0..n {
                let b = take(4)?;
                #[allow(clippy::cast_precision_loss)]
                ys.push(i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 100.0);
            }
            runs.insert(key, ys);
        }
        Ok(Self { runs })
    }
}

// ─── Asking for a height ─────────────────────────────────────────────────────

/// Solved runs, asked "how high is the road at this point".
///
/// The answer is the nearest run of an accepted level whose carriageway covers
/// the point, interpolated along it — the clamped projection `tunnels` asks its
/// corridors, so a chain of segments behaves as one corridor with rounded ends.
#[derive(Clone, Debug, Default)]
pub struct LevelMap {
    lines: Vec<MapLine>,
    buckets: HashMap<(i32, i32), Vec<(u32, u32)>>,
}

#[derive(Clone, Debug)]
struct MapLine {
    level: i8,
    half_w: f32,
    points: Vec<[f32; 2]>,
    ys: Vec<f32>,
}

/// Bucket edge for [`LevelMap`], metres.
const BUCKET_M: f32 = 16.0;

impl LevelMap {
    /// Add a run's points and heights. Mismatched lengths are ignored.
    pub fn add(&mut self, level: i8, half_w: f32, points: Vec<[f32; 2]>, ys: Vec<f32>) {
        if points.len() != ys.len() || points.len() < 2 {
            return;
        }
        #[allow(clippy::cast_possible_truncation)]
        let li = self.lines.len() as u32;
        let reach = half_w + BUCKET_M;
        for (si, w) in points.windows(2).enumerate() {
            let (a, b) = (w[0], w[1]);
            let lo = [a[0].min(b[0]) - reach, a[1].min(b[1]) - reach];
            let hi = [a[0].max(b[0]) + reach, a[1].max(b[1]) + reach];
            #[allow(clippy::cast_possible_truncation)]
            let cell = |v: f32| (v / BUCKET_M).floor() as i32;
            for x in cell(lo[0])..=cell(hi[0]) {
                for z in cell(lo[1])..=cell(hi[1]) {
                    #[allow(clippy::cast_possible_truncation)]
                    self.buckets
                        .entry((x, z))
                        .or_default()
                        .push((li, si as u32));
                }
            }
        }
        self.lines.push(MapLine {
            level,
            half_w,
            points,
            ys,
        });
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The road's height at `p` on the nearest run of an accepted level within
    /// its half-width plus `reach`, or `None`.
    #[must_use]
    pub fn y_at(&self, p: [f32; 2], accept: impl Fn(i8) -> bool, reach: f32) -> Option<f32> {
        #[allow(clippy::cast_possible_truncation)]
        let key = (
            (p[0] / BUCKET_M).floor() as i32,
            (p[1] / BUCKET_M).floor() as i32,
        );
        let mut best: Option<(f32, f32)> = None;
        for &(li, si) in self.buckets.get(&key).into_iter().flatten() {
            let line = &self.lines[li as usize];
            if !accept(line.level) {
                continue;
            }
            let si = si as usize;
            let (a, b) = (line.points[si], line.points[si + 1]);
            let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
            let len2 = dx * dx + dz * dz;
            let t = if len2 > 1e-9 {
                (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = (p[0] - a[0] - dx * t).hypot(p[1] - a[1] - dz * t);
            if d > line.half_w + reach {
                continue;
            }
            let y = line.ys[si] + (line.ys[si + 1] - line.ys[si]) * t;
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, y));
            }
        }
        best.map(|(_, y)| y)
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
            cut: [false; 2],
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
        let bore = line(straight(0.0, 200.0, 0.0), -1, |_| Height::AtMost {
            ceiling: -7.0,
            measured: None,
        });
        let pts = straight(200.0, 500.0, 0.0);
        let road = line(pts.clone(), 0, |_| Height::Measured {
            y: 0.0,
            weight: 1.0,
        });
        let s = solve(&[bore, road]);
        let ys = &s.heights[1];
        assert!((ys[0] + 7.0).abs() < 1e-4, "the portal is at {}", ys[0]);
        assert!(
            s.heights[0].iter().all(|&y| y <= -7.0 + 1e-4),
            "the bore rose over its ceiling"
        );
        assert!(slope(ys, &pts) <= G + 1e-4);
        assert!(
            ys.iter().all(|&y| y <= 1e-4),
            "a cutting rose above the ground"
        );
        assert!(ys.last().unwrap().abs() < 0.05);
    }

    /// **An underpass shorter than its ramps still clears what is over it**, and
    /// the ramps run out onto the roads either side. Under a lid the road is only
    /// bounded, so a 30 m underpass is not dragged to a tunnel's full level.
    #[test]
    fn a_short_underpass_ramps_out_both_ways() {
        let before = densify(&[[-300.0, 0.0], [0.0, 0.0]], 2.0);
        let under = line(densify(&[[0.0, 0.0], [30.0, 0.0]], 2.0), -1, |_| {
            Height::AtMost {
                ceiling: -5.5,
                measured: None,
            }
        });
        let after = densify(&[[30.0, 0.0], [330.0, 0.0]], 2.0);
        let ground = |_| Height::Measured {
            y: 0.0,
            weight: 1.0,
        };
        let s = solve(&[
            line(before.clone(), 0, ground),
            under,
            line(after.clone(), 0, ground),
        ]);
        assert!(s.heights[1].iter().all(|&y| y <= -5.5 + 1e-4));
        assert!(
            s.heights[1].iter().all(|&y| y >= -5.6),
            "dug deeper than it needed to"
        );
        assert!(slope(&s.heights[0], &before) <= G + 1e-4);
        assert!(s.heights[0][0].abs() < 0.05 && s.heights[2].last().unwrap().abs() < 0.05);
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

    /// **A road cut by a tile border is still one road.** The two halves end a
    /// few decimetres apart, each tile having quantised its own copy; joined, a
    /// deck in one tile grades the approach that runs on into the next.
    #[test]
    fn a_road_across_a_tile_border_is_joined() {
        let span = line(straight(0.0, 60.0, 0.0), 1, |_| Height::Fixed(8.0));
        let mut near = line(straight(60.0, 100.0, 0.0), 0, |_| Height::Measured {
            y: 0.0,
            weight: 1.0,
        });
        near.cut[1] = true;
        let mut far = line(straight(100.3, 400.0, 0.2), 0, |_| Height::Measured {
            y: 0.0,
            weight: 1.0,
        });
        far.cut[0] = true;
        let s = solve(&[span, near, far]);
        let (end, start) = (*s.heights[1].last().unwrap(), s.heights[2][0]);
        assert_eq!(end, start, "the halves meet at two heights");
        assert!(end > 5.0, "the ramp stopped at the border: {end}");
    }

    /// **The runs a tile yields are named the same way every time**, and the
    /// ring fixture's Europaplein flyover comes out on a bridge.
    #[test]
    fn a_tiles_runs_are_named_and_levelled() {
        let tile = crate::testdata::groningen_ring();
        let a = street_runs(&tile, 1465.3);
        let b = street_runs(&tile, 1465.0);
        assert!(!a.is_empty());
        assert_eq!(
            a.iter().map(|r| r.key).collect::<Vec<_>>(),
            b.iter().map(|r| r.key).collect::<Vec<_>>(),
            "a run's key depends on the tile's size in metres"
        );
        assert_eq!(
            a.iter().map(|r| r.points.len()).collect::<Vec<_>>(),
            b.iter().map(|r| r.points.len()).collect::<Vec<_>>(),
            "a run densifies differently with the tile's size in metres"
        );
        let flyover = [1247.9f32, 125.7];
        assert!(
            a.iter().any(|r| r.level == 1
                && r.points
                    .iter()
                    .any(|p| (p[0] - flyover[0]).hypot(p[1] - flyover[1]) < 20.0)),
            "no bridge run at the Europaplein"
        );
        assert!(
            a.iter().any(|r| r.level == -1),
            "the ring's tunnels are missing"
        );
    }

    #[test]
    fn road_heights_survive_the_wire() {
        let mut h = RoadHeights::default();
        h.runs.insert(7, vec![1.25, -3.5, 8.0]);
        h.runs.insert(u64::MAX, vec![0.0, 0.01]);
        let back = RoadHeights::decode(&h.encode()).expect("decodes");
        assert_eq!(back, h);
        assert!(RoadHeights::decode(b"nope").is_err());
    }

    /// The lookup answers on the carriageway and not beside it, and only for the
    /// levels asked about — the road under a flyover must not read the deck.
    #[test]
    fn a_level_map_answers_on_the_road_at_its_level() {
        let mut m = LevelMap::default();
        m.add(0, 4.0, vec![[0.0, 0.0], [100.0, 0.0]], vec![0.0, 10.0]);
        m.add(1, 4.0, vec![[50.0, -50.0], [50.0, 50.0]], vec![7.0, 7.0]);
        let ground = |l: i8| l == 0;
        assert!((m.y_at([25.0, 1.0], ground, 0.0).unwrap() - 2.5).abs() < 1e-4);
        assert!(m.y_at([25.0, 9.0], ground, 0.0).is_none());
        assert!((m.y_at([50.0, 0.0], ground, 0.0).unwrap() - 5.0).abs() < 1e-4);
        assert_eq!(m.y_at([50.0, 0.0], |l| l > 0, 0.0), Some(7.0));
    }
}
