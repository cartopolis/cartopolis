//! Refining a flat ground mesh so it has vertices to be bent onto terrain *by*.
//!
//! This is the client's own refinement, moved here so that **the harness can run
//! the same one**. `cartopolis_viewer` deliberately does not depend on
//! `cartopolis`, and without this it bent whatever triangles earcut happened to
//! emit: a land fill's 100 m edge chorded straight across a 95 m terrain wave
//! while the road beside it followed, so the two gapped by metres and the picture
//! showed cracks the client would never draw. Measured 2026-09-19 — the gaps
//! scaled exactly with `--relief`, which is what a chord does and what a real
//! fault does not.
//!
//! Nothing here touches Bevy or the client's terrain type. The four places the
//! algorithm consults the ground go through [`GroundField`], which the client
//! implements for its measured `TerrainSnapshot` and the harness for its synthetic
//! one — so both refine identically and neither can drift from the other.

/// The ground a crease-aware refinement tests a mesh against.
///
/// Two methods, because that is all the algorithm asks of a field: how fine it is
/// worth refining to, and how high the ground is at a point. Anything that can
/// answer those can be refined against, which is what lets one implementation
/// serve the measured field and the synthetic one.
pub trait GroundField {
    /// The floor the crease rule refines to — never finer than the measurement the
    /// machine actually got. See [`MIN_GROUND_EDGE_M`].
    fn min_ground_edge_m(&self) -> f32;

    /// Ground height at a world-XZ point.
    fn at(&self, x: f32, z: f32) -> f32;
}

/// The measured ground a crease-aware refinement tests against, and the frame its
/// mesh coordinates are in.
///
/// `None` at a call site means "long edges only" — [`drape_mesh`] passes one, the
/// volumetric groups do not, and a caller with no field to consult cannot ask for a
/// rule that is defined in terms of one.
#[derive(Clone, Copy)]
pub struct Crease<'a> {
    /// The **coarse** field, always. The half-metre tier re-centres as the camera
    /// walks, and a mesh that sampled it would change shape every time it did.
    pub field: &'a dyn GroundField,
    /// World-XZ point the mesh's local coordinates are measured from.
    pub origin: [f32; 2],
}

/// How far a mesh edge's midpoint may sit off the measured ground, in metres,
/// before the edge is split even though it is already short enough for the
/// long-edge rule. The crease rule; see the client's `terrain` module for the quay
/// wall this was measured against.
pub const CREASE_SAG_M: f32 = 0.15;

/// The floor the crease rule refines to on a machine that got the full field.
///
/// Half the coarse field's own spacing. A floor for the desktop's field, not the
/// number itself — read it through [`GroundField::min_ground_edge_m`], which scales
/// it to the field the machine actually has.
pub const MIN_GROUND_EDGE_M: f32 = crate::ahn::SAMPLE_M * 0.5;

/// Vertices one [`refine_for_terrain`] call may add on the crease rule's account.
/// A bound on the burst, not a target.
pub const CREASE_VERTEX_BUDGET: usize = 48_000;

/// Refinement passes. Five takes a cell-spanning 1.5 km triangle down to ~46 m.
const MAX_REFINE_PASSES: usize = 5;

/// The hasher for the refinement's two edge maps: a multiply-rotate over the
/// key's words (rustc's `FxHasher`), written out because it is ten lines.
///
/// **The default hasher was a third of the refinement.** SipHash is built to
/// resist a hostile key set, and these keys are vertex indices and millimetre
/// coordinates from our own mesh. On the Deck (2026-09-24) a city cell's
/// refinement took 0.8–1.8 s, of which the ground lookups were ~10 %; swapping
/// the hasher took `refine_a_cell` from 790 ms to 522 ms.
#[derive(Default, Clone, Copy)]
struct EdgeHasher(u64);

impl std::hash::Hasher for EdgeHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(4) {
            let mut w = [0u8; 4];
            w[..chunk.len()].copy_from_slice(chunk);
            self.write_u32(u32::from_le_bytes(w));
        }
    }

    fn write_u32(&mut self, v: u32) {
        self.write_u64(u64::from(v));
    }

    fn write_i32(&mut self, v: i32) {
        self.write_u64(u64::from(v as u32));
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }
}

/// A fixed-size key of 32-bit words for the maps here, hashed two words at a time.
///
/// **Not an array**, whose `Hash` goes through `hash_slice` and hands the hasher
/// the words as *bytes*, which [`EdgeHasher::write`] then re-chunks four at a time:
/// that byte loop was 11 % of a street-level fill's worker time on `deck-1`
/// (2026-10-20, a sampling profile of the dev build), more than the ground lookups
/// the edges were keyed to avoid.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Words<const N: usize>([u32; N]);

impl<const N: usize> std::hash::Hash for Words<N> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        for pair in self.0.chunks(2) {
            let hi = pair.get(1).copied().unwrap_or(0);
            state.write_u64(u64::from(pair[0]) | (u64::from(hi) << 32));
        }
    }
}

/// Merge the vertices that are the same vertex: equal in position and in every
/// attribute the mesh carries, bit for bit.
///
/// **Every flat ground layer reaches the drape with each triangle owning its three
/// corners** — `flat_fill` emits them so (the surveyed paving, the crop parcels) and
/// the land fills come out of the road cut the same way. At street level over
/// Groningen (`deck-1`, 2026-10-20) that was 3.0 vertices a triangle for the paving
/// and the built land, 11.5 M of them across the nine cells. Everything after this
/// pays per vertex: [`refine_for_terrain`] mints a midpoint per *index pair*, so an
/// edge two triangles own is split twice into two coincident vertices; the drape
/// samples the field once a vertex and the normals do too; and the upload, the
/// residency and the vertex shader in the main pass are all per vertex.
///
/// Exact by construction. Only bit-identical vertices merge, so nothing moves and
/// nothing is recoloured, and every triangle keeps its corners in the same order,
/// so the winding `flat_fill` fixed survives. Two vertices that differ in any
/// attribute — the corners of two parcels of different crops — stay two.
pub fn weld(
    positions: &mut Vec<[f32; 3]>,
    indices: &mut [u32],
    uvs: &mut Vec<[f32; 2]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    rungs: &mut Vec<f32>,
) {
    type HashMap<K, V> = std::collections::HashMap<K, V, std::hash::BuildHasherDefault<EdgeHasher>>;

    let n = positions.len();
    let has = |len: usize| len == n && n > 0;
    let (has_uv, has_normal, has_color, has_rung) = (
        has(uvs.len()),
        has(normals.len()),
        has(colors.len()),
        has(rungs.len()),
    );
    let key = |i: usize| -> [u32; 13] {
        let mut k = [0u32; 13];
        for (slot, v) in k.iter_mut().zip(positions[i]) {
            *slot = v.to_bits();
        }
        if has_uv {
            k[3] = uvs[i][0].to_bits();
            k[4] = uvs[i][1].to_bits();
        }
        if has_normal {
            for (slot, v) in k[5..8].iter_mut().zip(normals[i]) {
                *slot = v.to_bits();
            }
        }
        if has_color {
            for (slot, v) in k[8..12].iter_mut().zip(colors[i]) {
                *slot = v.to_bits();
            }
        }
        if has_rung {
            k[12] = rungs[i].to_bits();
        }
        k
    };
    let mut first: HashMap<Words<13>, u32> =
        HashMap::with_capacity_and_hasher(n / 2, Default::default());
    let mut remap = Vec::with_capacity(n);
    let mut kept: Vec<usize> = Vec::with_capacity(n / 2);
    for i in 0..n {
        let next = kept.len() as u32;
        let to = *first.entry(Words(key(i))).or_insert(next);
        if to == next {
            kept.push(i);
        }
        remap.push(to);
    }
    if kept.len() == n {
        return;
    }
    for i in indices.iter_mut() {
        *i = remap[*i as usize];
    }
    fn compact<T: Copy>(v: &mut Vec<T>, kept: &[usize]) {
        *v = kept.iter().map(|&i| v[i]).collect();
    }
    compact(positions, &kept);
    if has_uv {
        compact(uvs, &kept);
    }
    if has_normal {
        compact(normals, &kept);
    }
    if has_color {
        compact(colors, &kept);
    }
    if has_rung {
        compact(rungs, &kept);
    }
}

/// Split every triangle edge longer than `max_edge`, repeatedly, so a flat polygon
/// has vertices to be displaced *by*.
///
/// Earcut answers "cover this outline" with as few triangles as it can, which for
/// a field or a lake is two — and two triangles cannot follow a hill, they can only
/// be tilted by it. The refinement is **per edge, not per triangle**: an edge is
/// split once, its midpoint is shared by both triangles that own it, and each
/// triangle is then re-cut according to how many of its edges were split (the
/// classic red-green refinement). Deciding per triangle instead — "this one is big,
/// subdivide it" — leaves a T-junction wherever a split triangle meets an unsplit
/// neighbour, and a T-junction on a slope is a crack you can see the sky through.
///
/// Attributes ride along, interpolated at each midpoint, so UV-mapped roads and
/// colour-per-vertex canopies survive refinement.
///
/// **Two rules, not one.** `max_edge` is the old one and answers "is this polygon
/// bigger than the ground can be described in"; `crease` is the second and answers
/// "does the mesh disagree with the ground *here*". The first is uniform and is
/// therefore priced against the whole cell, which is why it sits at 12.5 m and not
/// at the 6.25 m the field could carry ([`MAX_GROUND_EDGE_M`] has the burst that
/// decided it). The second spends only where there is something to resolve — a quay
/// wall, an embankment lip, a dyke — and is bounded by [`CREASE_VERTEX_BUDGET`] so a
/// cell that is crease all the way across cannot reintroduce that burst. Pass `None`
/// for the second where there is no field to consult, and note the two compose: an
/// edge splits if *either* rule asks.
pub fn refine_for_terrain(
    positions: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
    uvs: &mut Vec<[f32; 2]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    rungs: &mut Vec<f32>,
    max_edge: f32,
    crease: Option<Crease<'_>>,
) {
    refine_mesh(
        positions, indices, uvs, normals, colors, rungs, max_edge, crease, true,
    );
}

/// [`refine_for_terrain`], with the finished-triangle skip switchable so a test
/// can hold the two against each other.
#[allow(clippy::too_many_arguments)]
fn refine_mesh(
    positions: &mut Vec<[f32; 3]>,
    indices: &mut Vec<u32>,
    uvs: &mut Vec<[f32; 2]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    rungs: &mut Vec<f32>,
    max_edge: f32,
    crease: Option<Crease<'_>>,
    skip_settled: bool,
) {
    type HashMap<K, V> = std::collections::HashMap<K, V, std::hash::BuildHasherDefault<EdgeHasher>>;

    let limit = max_edge.max(1.0);
    let limit_sq = limit * limit;
    let floor = crease.map_or(MIN_GROUND_EDGE_M, |c| c.field.min_ground_edge_m());
    let floor_sq = floor * floor;
    // Spent across every pass, not per pass: the budget bounds the mesh handed to
    // the driver, and three passes each spending it in full is the burst it exists
    // to stop.
    let mut spent = 0usize;
    // **Across every pass, for the same reason it is keyed on positions.** The
    // answer is a pure function of the two quantised endpoints and the field, and
    // neither moves between passes — so an edge the crease rule refused in pass 1
    // is refused in pass 2 for exactly the same reason, and re-deriving it costs
    // three `field.at` samples of a 2,049² grid to reach the answer already held.
    // An edge it *accepted* was split and is not asked about again.
    //
    // A refusal charged to an exhausted budget survives too, and must: `spent`
    // only ever grows, so the later pass would refuse it anyway, and answering it
    // consistently is what the per-edge key exists for.
    let mut decided: HashMap<Words<6>, bool> = HashMap::default();
    // **A triangle none of whose edges split is finished for good**, and is copied
    // through every later pass without asking again. Each "no" is final: the long-
    // edge rule is a pure function of the edge's length, a crease refusal is held in
    // `decided` (which even a neighbour owning the same edge reads), and the budget
    // only shrinks. So the question has one answer, and re-asking it cost a lookup
    // in a table of millions of edges per edge per pass — at street level over
    // Groningen (`deck-1`, 2026-10-20) the paving's refinement was ~12 µs a
    // triangle and every triangle of a cell was visited five times. Exact: the same
    // triangles come out in the same order.
    let mut settled: Vec<bool> = vec![false; indices.len() / 3];
    for _ in 0..MAX_REFINE_PASSES {
        let open = settled.iter().filter(|&&s| !s).count();
        let mut midpoints: HashMap<(u32, u32), u32> =
            HashMap::with_capacity_and_hasher(open * 3 / 2, Default::default());
        // **Whether an edge splits is a property of where it is, not of which
        // vertices happen to name it.**
        //
        // `midpoints` is keyed on the index pair, which is the right key for the
        // *vertex* — reusing it welds two triangles that genuinely share a corner.
        // It is the wrong key for the *decision*, and two of the three layers that
        // tear emit each triangle's corners unshared: `flat_fill` does it for the
        // surveyed paving and the crop parcels, and `road_stitch` re-emits every
        // ribbon its junction cut touched the same way. An interior edge of those
        // meshes is therefore two index pairs over one piece of ground, so it was
        // tested twice, charged to the budget twice, and — this is the part that
        // shows — could be answered *differently* for its two owners when the
        // budget ran out between them. That is a T-junction, which is the crack the
        // per-edge design exists to prevent, reintroduced by the vertex layout.
        //
        // Deciding on the quantised endpoints instead makes one geometric edge one
        // decision. Two consequences beyond the crack: the budget now buys twice as
        // much ground, and the answer is a pure function of the two endpoints and
        // the field — so two *different meshes* sharing a boundary edge reach it
        // independently and agree, which is what stops a seam opening between the
        // paving and the fill it is cut out of.
        //
        // Millimetres, and all three axes. The coordinates come from one source
        // vertex so they are bit-identical and any quantisation would do; the
        // rounding is against a future source that computes them twice. `y` is in
        // the key because a surveyed wall has two vertices over one plan position
        // and they are not one edge.
        let split = |a: u32,
                     b: u32,
                     positions: &mut Vec<[f32; 3]>,
                     uvs: &mut Vec<[f32; 2]>,
                     normals: &mut Vec<[f32; 3]>,
                     colors: &mut Vec<[f32; 4]>,
                     rungs: &mut Vec<f32>,
                     midpoints: &mut HashMap<(u32, u32), u32>,
                     decided: &mut HashMap<Words<6>, bool>,
                     spent: &mut usize|
         -> Option<u32> {
            let key = if a < b { (a, b) } else { (b, a) };
            if let Some(m) = midpoints.get(&key) {
                return Some(*m);
            }
            let (pa, pb) = (positions[a as usize], positions[b as usize]);
            let q = |v: f32| (v * 1000.0).round() as i32;
            let (ka, kb) = (
                [q(pa[0]), q(pa[1]), q(pa[2])],
                [q(pb[0]), q(pb[1]), q(pb[2])],
            );
            let (lo, hi) = if ka <= kb { (ka, kb) } else { (kb, ka) };
            let edge = Words([lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]].map(|v| v as u32));
            let d = [pa[0] - pb[0], pa[1] - pb[1], pa[2] - pb[2]];
            let len_sq = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            if len_sq <= limit_sq {
                // Short enough for the long-edge rule. The crease rule gets its say,
                // and it is the only one that can ask for a vertex down here.
                let Some(c) = crease else {
                    return None;
                };
                match decided.get(&edge) {
                    // Already answered for this piece of ground. Note the budget is
                    // *not* consulted again: a second owner of an edge the first
                    // already paid for must get the same answer, or the two of them
                    // disagree about the shape of their shared boundary.
                    Some(false) => return None,
                    Some(true) => {}
                    None => {
                        if len_sq <= floor_sq || *spent >= CREASE_VERTEX_BUDGET {
                            decided.insert(edge, false);
                            return None;
                        }
                        // Sag: how far the ground under the midpoint is from the
                        // chord this edge would draw. Sampled on the field, never on
                        // the mesh — the displacement has not happened yet, and on a
                        // road the mesh's own y is a depth rung rather than a height.
                        let (ox, oz) = (c.origin[0], c.origin[1]);
                        let ha = c.field.at(ox + pa[0], oz + pa[2]);
                        let hb = c.field.at(ox + pb[0], oz + pb[2]);
                        let hm = c
                            .field
                            .at(ox + (pa[0] + pb[0]) * 0.5, oz + (pa[2] + pb[2]) * 0.5);
                        if (hm - (ha + hb) * 0.5).abs() < CREASE_SAG_M {
                            decided.insert(edge, false);
                            return None;
                        }
                        decided.insert(edge, true);
                        *spent += 1;
                    }
                }
            }
            let m = positions.len() as u32;
            positions.push([
                (pa[0] + pb[0]) * 0.5,
                (pa[1] + pb[1]) * 0.5,
                (pa[2] + pb[2]) * 0.5,
            ]);
            if !uvs.is_empty() {
                let (ua, ub) = (uvs[a as usize], uvs[b as usize]);
                uvs.push([(ua[0] + ub[0]) * 0.5, (ua[1] + ub[1]) * 0.5]);
            }
            if !normals.is_empty() {
                let (na, nb) = (normals[a as usize], normals[b as usize]);
                // Written out rather than through glam: this crate has no
                // linear-algebra dependency and one normalise is not a reason to
                // grow one.
                let sum = [na[0] + nb[0], na[1] + nb[1], na[2] + nb[2]];
                let len = (sum[0] * sum[0] + sum[1] * sum[1] + sum[2] * sum[2]).sqrt();
                normals.push(if len > f32::EPSILON {
                    [sum[0] / len, sum[1] / len, sum[2] / len]
                } else {
                    [0.0, 1.0, 0.0]
                });
            }
            if !colors.is_empty() {
                let (ca, cb) = (colors[a as usize], colors[b as usize]);
                colors.push(std::array::from_fn(|i| (ca[i] + cb[i]) * 0.5));
            }
            // A road's depth rung is constant across its whole ribbon and an edge
            // never spans two of them (features are tessellated into separate
            // vertices), so the midpoint of a split edge has the rung both ends
            // already had. Averaging is only the general form of that.
            if !rungs.is_empty() {
                rungs.push((rungs[a as usize] + rungs[b as usize]) * 0.5);
            }
            midpoints.insert(key, m);
            Some(m)
        };

        let mut out: Vec<u32> = Vec::with_capacity(indices.len());
        let mut out_settled: Vec<bool> = Vec::with_capacity(settled.len());
        let mut any = false;
        for (tri, &done) in indices.chunks_exact(3).zip(&settled) {
            let (a, b, c) = (tri[0], tri[1], tri[2]);
            if done && skip_settled {
                out.extend_from_slice(&[a, b, c]);
                out_settled.push(true);
                continue;
            }
            #[rustfmt::skip]
            let ab = split(a, b, positions, uvs, normals, colors, rungs, &mut midpoints, &mut decided, &mut spent);
            #[rustfmt::skip]
            let bc = split(b, c, positions, uvs, normals, colors, rungs, &mut midpoints, &mut decided, &mut spent);
            #[rustfmt::skip]
            let ca = split(c, a, positions, uvs, normals, colors, rungs, &mut midpoints, &mut decided, &mut spent);
            let cut = ab.is_some() || bc.is_some() || ca.is_some();
            any |= cut;
            let before = out.len();
            match (ab, bc, ca) {
                (None, None, None) => out.extend_from_slice(&[a, b, c]),
                // One split edge: a straight cut from the midpoint to the opposite
                // corner. Winding is preserved in every arm — a flipped sub-triangle
                // is a backface-culled hole in the ground.
                (Some(m), None, None) => out.extend_from_slice(&[a, m, c, m, b, c]),
                (None, Some(m), None) => out.extend_from_slice(&[b, m, a, m, c, a]),
                (None, None, Some(m)) => out.extend_from_slice(&[c, m, b, m, a, b]),
                (Some(m1), Some(m2), None) => {
                    out.extend_from_slice(&[m1, b, m2, a, m1, m2, a, m2, c]);
                }
                (None, Some(m1), Some(m2)) => {
                    out.extend_from_slice(&[m1, c, m2, b, m1, m2, b, m2, a]);
                }
                (Some(m2), None, Some(m1)) => {
                    out.extend_from_slice(&[m1, a, m2, c, m1, m2, c, m2, b]);
                }
                (Some(mab), Some(mbc), Some(mca)) => {
                    out.extend_from_slice(&[a, mab, mca, mab, b, mbc, mca, mbc, c, mab, mbc, mca]);
                }
            }
            // An uncut triangle is finished; the pieces of a cut one get asked again.
            out_settled.resize(out_settled.len() + (out.len() - before) / 3, !cut);
        }
        *indices = out;
        settled = out_settled;
        if !any {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ground with enough curvature to make the crease rule fire everywhere.
    struct Bumpy;

    impl GroundField for Bumpy {
        fn min_ground_edge_m(&self) -> f32 {
            1.5
        }

        fn at(&self, x: f32, z: f32) -> f32 {
            (x / 17.0).sin() * 2.0 + (z / 23.0).cos() * 1.5
        }
    }

    /// One square of ground, in a local frame `origin` metres from the world origin.
    fn square(side: f32) -> (Vec<[f32; 3]>, Vec<u32>) {
        (
            vec![
                [0.0, 0.0, 0.0],
                [side, 0.0, 0.0],
                [side, 0.0, side],
                [0.0, 0.0, side],
            ],
            vec![0, 1, 2, 0, 2, 3],
        )
    }

    fn refine(origin: [f32; 2], side: f32, max_edge: f32) -> Vec<[f32; 3]> {
        let (mut positions, mut indices) = square(side);
        refine_for_terrain(
            &mut positions,
            &mut indices,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            max_edge,
            Some(Crease {
                field: &Bumpy,
                origin,
            }),
        );
        // Into world metres, so two different local frames are comparable.
        positions
            .iter()
            .map(|p| [origin[0] + p[0], p[1], origin[1] + p[2]])
            .collect()
    }

    /// Vertices on the plane `x == wx`, sorted, rounded to millimetres.
    fn on_seam(v: &[[f32; 3]], wx: f32) -> Vec<i64> {
        let mut out: Vec<i64> = v
            .iter()
            .filter(|p| (p[0] - wx).abs() < 1e-3)
            .map(|p| (f64::from(p[2]) * 1000.0).round() as i64)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// **Two cells must agree about their shared boundary, having never met.**
    ///
    /// This is the property the whole per-edge, decide-on-quantised-endpoints
    /// design exists for, and it is the one a map tiled into cells depends on: a
    /// feature crossing a tile border is clipped into two meshes that are
    /// tessellated, refined and draped independently, and if one splits a boundary
    /// edge the other does not, the result is a T-junction — a crack you can see
    /// the sky through, along a seam, in the one place nothing is looking.
    ///
    /// Worth pinning here rather than in the client: the client's cells arrive from
    /// a streamer and a failure would be a rare visual artifact somebody has to
    /// notice and then reproduce.
    #[test]
    fn two_cells_agree_about_the_edge_they_share() {
        const SIDE: f32 = 120.0;
        let left = refine([0.0, 0.0], SIDE, 12.5);
        let right = refine([SIDE, 0.0], SIDE, 12.5);

        let a = on_seam(&left, SIDE);
        let b = on_seam(&right, SIDE);
        assert!(
            a.len() > 4,
            "the ground is not bumpy enough to be testing anything: {} vertices",
            a.len()
        );
        assert_eq!(
            a, b,
            "the two cells refined their shared boundary differently, which is a \
             T-junction along the seam"
        );
    }

    /// Agreement must survive the two meshes being **cut differently**.
    ///
    /// This is not hypothetical: a feature crossing a tile border is clipped into
    /// two different polygons, and earcut answers each with its own triangulation.
    /// So the two cells meet along one geometric edge while disagreeing about every
    /// triangle behind it, and the shared boundary still has to come out the same.
    /// It does because the decision is a pure function of the edge's two endpoints
    /// and the ground — never of the triangle asking.
    #[test]
    fn agreement_survives_a_different_triangulation() {
        const SIDE: f32 = 120.0;
        let straight = refine([SIDE, 0.0], SIDE, 12.5);

        // The same square, cut along the other diagonal.
        let (mut positions, _) = square(SIDE);
        let mut indices = vec![0, 1, 3, 1, 2, 3];
        refine_for_terrain(
            &mut positions,
            &mut indices,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            12.5,
            Some(Crease {
                field: &Bumpy,
                origin: [SIDE, 0.0],
            }),
        );
        let flipped: Vec<[f32; 3]> = positions
            .iter()
            .map(|p| [SIDE + p[0], p[1], p[2]])
            .collect();

        assert_eq!(
            on_seam(&straight, SIDE),
            on_seam(&flipped, SIDE),
            "the shared boundary came out differently because the interior was cut \
             differently"
        );
    }

    /// **Skipping the finished triangles changes nothing**: the same vertices, the
    /// same triangles, in the same order, bit for bit. Over a field bumpy enough to
    /// spend the whole crease budget, so the refusals that come from an exhausted
    /// budget are covered as well as the ones that come from the ground.
    #[test]
    fn skipping_finished_triangles_refines_exactly_the_same_mesh() {
        let (n, step) = (150u32, 24.5f32);
        let mut positions = Vec::new();
        let mut colors = Vec::new();
        let mut indices = Vec::new();
        for j in 0..n {
            for i in 0..n {
                let (x, z) = (i as f32 * step, j as f32 * step);
                let b = positions.len() as u32;
                positions.extend([
                    [x, 0.0, z],
                    [x + step, 0.0, z],
                    [x + step, 0.0, z + step],
                    [x, 0.0, z + step],
                ]);
                colors.extend([[i as f32 / n as f32, 0.5, 0.5, 1.0]; 4]);
                indices.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
            }
        }
        let run = |skip: bool| {
            let (mut p, mut i, mut c) = (positions.clone(), indices.clone(), colors.clone());
            refine_mesh(
                &mut p,
                &mut i,
                &mut Vec::new(),
                &mut Vec::new(),
                &mut c,
                &mut Vec::new(),
                12.5,
                Some(Crease {
                    field: &Bumpy,
                    origin: [0.0, 0.0],
                }),
                skip,
            );
            let bits = |v: &[[f32; 3]]| v.iter().flatten().map(|f| f.to_bits()).collect::<Vec<_>>();
            let cbits =
                |v: &[[f32; 4]]| v.iter().flatten().map(|f| f.to_bits()).collect::<Vec<_>>();
            (bits(&p), i, cbits(&c))
        };
        let (with, without) = (run(true), run(false));
        assert!(
            with.0.len() / 3 > positions.len() + CREASE_VERTEX_BUDGET,
            "the field did not spend the crease budget, so its refusals are untested"
        );
        assert!(
            with == without,
            "skipping finished triangles changed the mesh"
        );
    }

    /// **Welding first changes no triangle the refinement draws, and only removes
    /// vertices.** The mesh is laid out the way `flat_fill` emits one — every
    /// triangle with its own corners — with a second colour region so that a seam
    /// between two parcels has to stay unwelded.
    #[test]
    fn welding_before_refining_draws_the_same_triangles_with_fewer_vertices() {
        let (n, step) = (12u32, 20.0f32);
        let mut positions = Vec::new();
        let mut colors = Vec::new();
        let mut indices = Vec::new();
        for j in 0..n {
            for i in 0..n {
                let (x, z) = (i as f32 * step, j as f32 * step);
                let tone = if i < n / 2 {
                    [0.2, 0.5, 0.1, 1.0]
                } else {
                    [0.6, 0.6, 0.3, 1.0]
                };
                for c in [
                    [x, 0.0, z],
                    [x + step, 0.0, z],
                    [x + step, 0.0, z + step],
                    [x, 0.0, z],
                    [x + step, 0.0, z + step],
                    [x, 0.0, z + step],
                ] {
                    indices.push(positions.len() as u32);
                    positions.push(c);
                    colors.push(tone);
                }
            }
        }
        let run = |weld_first: bool| {
            let (mut p, mut i, mut c) = (positions.clone(), indices.clone(), colors.clone());
            if weld_first {
                weld(
                    &mut p,
                    &mut i,
                    &mut Vec::new(),
                    &mut Vec::new(),
                    &mut c,
                    &mut Vec::new(),
                );
            }
            refine_for_terrain(
                &mut p,
                &mut i,
                &mut Vec::new(),
                &mut Vec::new(),
                &mut c,
                &mut Vec::new(),
                12.5,
                Some(Crease {
                    field: &Bumpy,
                    origin: [0.0, 0.0],
                }),
            );
            let mut tris: Vec<[u32; 21]> = i
                .chunks_exact(3)
                .map(|t| {
                    let mut k = [0u32; 21];
                    for (corner, &v) in t.iter().enumerate() {
                        let v = v as usize;
                        for (a, f) in p[v].iter().chain(&c[v]).enumerate() {
                            k[corner * 7 + a] = f.to_bits();
                        }
                    }
                    k
                })
                .collect();
            tris.sort_unstable();
            (tris, p.len())
        };
        let (plain, plain_verts) = run(false);
        let (welded, welded_verts) = run(true);
        assert_eq!(plain, welded, "welding changed what the refinement draws");
        assert!(
            welded_verts * 2 < plain_verts,
            "{welded_verts} vertices welded against {plain_verts}"
        );

        // And the seam between the two colours is still two vertices a point.
        let (mut p, mut i, mut c) = (positions.clone(), indices.clone(), colors.clone());
        weld(
            &mut p,
            &mut i,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut c,
            &mut Vec::new(),
        );
        let seam_x = (n / 2) as f32 * step;
        let on_seam = p.iter().filter(|v| v[0] == seam_x).count();
        assert_eq!(
            on_seam,
            2 * (n as usize + 1),
            "the colour seam was welded shut"
        );
    }

    /// A height lands on every vertex the refinement added, not only the corners.
    ///
    /// The refinement is only worth anything if the vertices it adds are then
    /// *used*; a mesh that is subdivided and then draped by its corners alone is
    /// the flat chord again, wearing more triangles.
    #[test]
    fn the_added_vertices_sit_on_the_ground() {
        let v = refine([0.0, 0.0], 120.0, 12.5);
        let worst = v
            .iter()
            .map(|p| (Bumpy.at(p[0], p[2]) - p[1]).abs())
            .fold(0.0f32, f32::max);
        // The refinement does not displace — it only provides vertices to displace
        // *by* — so every y is still zero here. This pins that contract: if the
        // function ever starts draping, the callers that drape afterwards would be
        // applying the ground twice.
        assert!(
            v.iter().all(|p| p[1] == 0.0),
            "refine_for_terrain displaced a vertex; its callers drape afterwards \
             and would now do it twice (worst {worst} m)"
        );
    }

    /// Wall-clock for a cell-sized mesh, printed rather than asserted (see
    /// `perf`'s module note on timers). Sized to what a city cell's refinement
    /// adds, ~500k vertices: 24.5 m quads, so the long-edge rule splits every one,
    /// each quad's corners unshared as `flat_fill` emits them — and beside it a
    /// paving-shaped one, 3 m quads that the long-edge rule never splits, which is
    /// what most of a street-level cell's ground is. Each with and without the
    /// finished-triangle skip.
    /// `cargo test -p cartopolis_geo --release refine_a_cell -- --ignored --nocapture`
    #[test]
    #[ignore = "a timing, not a check"]
    fn refine_a_cell() {
        for (name, n, step) in [("24.5 m quads", 240u32, 24.5f32), ("3 m quads", 480, 3.0)] {
            let mut positions = Vec::new();
            let mut indices = Vec::new();
            for j in 0..n {
                for i in 0..n {
                    let (x, z) = (i as f32 * step, j as f32 * step);
                    let b = positions.len() as u32;
                    positions.extend([
                        [x, 0.0, z],
                        [x + step, 0.0, z],
                        [x + step, 0.0, z + step],
                        [x, 0.0, z + step],
                    ]);
                    indices.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
                }
            }
            for skip in [false, true] {
                let (mut p, mut i) = (positions.clone(), indices.clone());
                let t = std::time::Instant::now();
                refine_mesh(
                    &mut p,
                    &mut i,
                    &mut Vec::new(),
                    &mut Vec::new(),
                    &mut Vec::new(),
                    &mut Vec::new(),
                    12.5,
                    Some(Crease {
                        field: &Bumpy,
                        origin: [0.0, 0.0],
                    }),
                    skip,
                );
                println!(
                    "{name}, skip {skip}: refined to {} vertices, {} triangles in {:?}",
                    p.len(),
                    i.len() / 3,
                    t.elapsed()
                );
            }
        }
    }
}
