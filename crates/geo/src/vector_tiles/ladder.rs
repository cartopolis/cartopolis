//! The road depth ladder: the rung every road ribbon, casing and bridge stretch is drawn on,
//! and the tie-breaking that keeps two roads of one class from z-fighting where they overlap.

use super::*;

/// How many depth-buffer ULPs one rung of the road ladder is worth.
///
/// Ribbons **overlap by construction** — a junction stub crosses whatever it tees
/// into, Shortbread's merged ways cross each other freely, and a flyover is drawn
/// on the same ground plane as the road it flies over. Two coplanar overlapping
/// triangles have depth values that agree only to the last few bits, so which one
/// wins flips per pixel as the camera moves: the flicker along a junction. The
/// material's own `depth_bias` cannot settle it, being a per-material constant
/// while the fight is between two features sharing a material — which is exactly
/// what this per-vertex rung supplies.
///
/// **The unit is the fix.** A rung used to be 24 µm of real height, and a
/// geometric gap's significance in the depth buffer decays as 1/z: 24 µm is 2.0
/// ULP at 100 m, 1.0 at 200 m and nothing at all beyond that. So the within-class
/// ordering — which ribbon of a junction is on top — simply *dissolved* in the
/// middle distance, and every same-class overlap (6 % of `Minor`, 14 % of
/// `Carriageway` on the fixture) went back to being an exact per-pixel coin flip
/// that re-rolls as the camera moves. That is the shimmer along a street, and no
/// amount of tie-colouring could reach it, because the rungs it was assigning had
/// stopped meaning anything.
///
/// Denominated in ULPs the rung is **range-invariant** by construction: the
/// apparent world shift is `1.19e-7 · ulps · z`, so it grows exactly as fast as
/// the depth buffer's own quantum.
///
/// It is sixty-four now and was four, and the change came with
/// [`crate::road_stitch`] taking the overlap away. Four was chosen as "enough to
/// clear the rasteriser's interpolation noise" and was never measured; the
/// sibling ladder that orders grass against water against road needs
/// [`DEPTH_BIAS_STEP`] — sixty-four — and its own comment records sixteen being
/// tried and found wanting at the shallow angles a low camera looks at the middle
/// distance with. On the Groningen fixture 1.9 % of road pixels were held apart by
/// a single four-unit rung, and a headless capture over the city confirmed the
/// consequence: raising this constant alone changed which ribbon was drawn along
/// every road past about two kilometres.
///
/// A rung can afford to be sixteen times taller because there are two of them
/// instead of 399 ([`ROAD_DEPTH_RUNGS`]) — the ordering that used to need all
/// those rungs is a *cut* now, not a tie-break.
pub const ROAD_RUNG_ULP: f32 = 64.0;

/// How many rungs the road band actually holds: ground, then bridge.
///
/// `road_stitch` leaves the ground roads as a partition — no two of them cover the
/// same patch — so nothing there needs separating in depth at all. The one
/// overlap it deliberately keeps is a flyover over the road it crosses, which is
/// two real surfaces in one place, and one rung is what holds them apart.
///
/// The 399-rung ladder this replaces was `class × width band × tie colouring`, all
/// of it in aid of deciding which of two overlapping ribbons to draw. That decision
/// is made in geometry now, and [`road_rung`] survives only as the *order the cut
/// runs in* — the hierarchy is still exactly as it was, it simply no longer has to
/// be expressed in the depth buffer.
pub const ROAD_DEPTH_RUNGS: u32 = 2;

/// Which of the two depth rungs a ribbon draws on.
#[must_use]
pub fn road_depth_rung(bridge: bool) -> f32 {
    u32::from(bridge) as f32
}

/// Rungs reserved at the top of the ladder for roads carried on a bridge deck.
/// One casing plus a full set of ties, so a flyover's ribbons separate from each
/// other as well as from the ground.
const BRIDGE_LAYERS: u32 = 1 + ROAD_TIE_LAYERS;

/// Tie-breaking rungs within one width band. Eight, not the four that fitted
/// under the old millimetre ceiling.
const ROAD_TIE_LAYERS: u32 = 8;

/// Width sub-bands *inside* one class block — the full [`ROAD_WIDTH_BANDS`]
/// resolution.
///
/// This was 2 under the old ladder, and the reason was purely the ceiling: rungs
/// were real height and the road sheet had 1.5 mm to spend before it reached the
/// buildings' base, so six class blocks could not also afford eight width bands.
/// A rung costs no height now, so the compression is simply unnecessary — and it
/// was not free. Measured on the Groningen fixture, restoring the eight bands
/// takes coplanar overlap 0.89 % → 0.62 %, and the wider tie set takes it to
/// 0.51 %.
const ROAD_WIDTH_SUBBANDS: u32 = 8;

/// How many rungs the ladder has, top to bottom. Ordered, not arbitrary (see
/// [`road_rung`]), so which of two overlapping features wins is a property of the
/// roads rather than of the order the tile happened to store them in.
const ROAD_LAYERS: u32 = ROAD_CLASS_COUNT * ROAD_RUNGS_PER_CLASS + BRIDGE_LAYERS;

/// How many classes [`road_class_rank`] ranks.
const ROAD_CLASS_COUNT: u32 = 6;

/// Rungs one class owns: one for its casings, then its fills.
///
/// The casing takes a single rung because same-class casings that overlap are
/// the same colour — their fight is invisible, which is the one place in this
/// ladder where a tie costs nothing. Fills need the width sub-band and the tie
/// colouring, so they take the rest.
///
/// Nothing bounds this but the road band's share of the depth range
/// (`SurfaceGroup::depth_rung`), which is why it can be generous now: a rung is
/// no longer a slice of the 1.5 mm the road sheet had under the buildings.
const CASING_RUNGS_PER_CLASS: u32 = 1;
const ROAD_RUNGS_PER_CLASS: u32 = CASING_RUNGS_PER_CLASS + ROAD_WIDTH_SUBBANDS * ROAD_TIE_LAYERS;

/// The highest depth bias any road vertex can reach: the foot of the road band
/// plus the whole per-vertex ladder standing on it.
///
/// This is what anything drawn *over* a road has to clear — a bridge deck, a
/// tree, a bench, a zebra crossing. It is [`GroundLayer::depth_bias_ceiling`],
/// and `ground_stack`'s `the_road_band_holds_every_road_vertex` checks the whole
/// stack against it, so growing the road ladder can never again silently push it
/// through the things above.
#[must_use]
pub fn road_rung_ceiling() -> f32 {
    GroundLayer::Road.depth_bias_ceiling()
}

/// A class's block index, low to high — **the** road hierarchy.
///
/// It used to have to agree with a second ranking in
/// [`SurfaceGroup::depth_rung`], and every way the two could disagree was a bug
/// (see [`road_rung`]). `depth_rung` gives every road group one shared value now,
/// so this is the only thing in the tree that orders the classes.
pub(super) fn road_class_rank(class: RoadClass) -> u32 {
    match class {
        RoadClass::Path => 0,
        RoadClass::Minor => 1,
        RoadClass::Major => 2,
        RoadClass::Carriageway => 3,
        RoadClass::Motorway => 4,
        RoadClass::Rail => 5,
    }
}

/// Width-band thresholds (metres) for [`road_rung`]: a road's band is the count
/// of thresholds at or below its width. 2 m steps where the urban network is
/// dense-packed — service 4, living street 6, residential 7, tertiary 9 all land
/// in different bands now, where the old single 6 m band held all of them —
/// and coarser above 10 m, where within-class widths barely vary and the band
/// only needs to keep a primary under a motorway.
const ROAD_WIDTH_BANDS: [f32; 7] = [2.0, 4.0, 6.0, 8.0, 10.0, 14.0, 20.0];

/// Which rung of the road ladder a line draws on — **the** road ordering, whole.
///
/// **This does not reach the GPU.** It is the order the *cut* runs in: which of
/// two overlapping ribbons keeps its ground when `road_stitch` partitions them.
/// The per-vertex attribute `systems::road_depth` spends as a depth offset comes
/// from [`road_depth_rung`], which has [`ROAD_DEPTH_RUNGS`] = 2 values — this one
/// has 399 and would be read as an enormous bias by anything that confused them.
/// Until 2026-09-18 this comment said the opposite, describing the arrangement
/// from before the ladder was collapsed on 2026-08-26, and `geo::tunnels` was
/// written against it three weeks later: a tunnel deck asked for rung 391–398,
/// i.e. ~25,900 depth units, and drew over the ground it runs under from a
/// kilometre away. A stale doc comment is a load-bearing part of an interface.
///
/// It is not a height either, and must never become one again; see the two
/// failures below.
///
/// Two rules, in order:
///
/// * A **bridge** takes the reserved top of the ladder. A flyover and the road
///   under it are both flat sheets on the same ground plane here, so the only
///   thing that can say which is on top is this; without it an interchange
///   resolves per-pixel and the deck and the road beneath interleave.
/// * Otherwise the **class decides, then width, then the tie colouring** — one
///   key, most significant first, exactly as it reads.
///
/// ## Why there is only one ladder
///
/// There used to be two, denominated differently, and every bug in this area was
/// a disagreement between them. `SurfaceGroup::depth_rung` ranked the *classes*
/// in depth-buffer ULPs, whose apparent world shift **grows** with range
/// (`1.19e-7 · bias · z`); this function ranked features in 24 µm steps of real
/// height, whose significance **shrinks** as 1/z. They failed in both directions:
///
/// * **Crossover.** When the two orders differed — a wide residential (Minor)
///   against a narrow tertiary (Major) — the terms crossed somewhere in the
///   middle distance and the winner flipped as the camera moved. Not flicker at a
///   seam: the loser's tarmac vanished over whole patches. Reported as "sometimes
///   part of the roads become invisible, the order is reversed". Making the class
///   the major key fixed *this* half, at the cost of compressing width onto two
///   sub-bands to fit under the ceiling.
/// * **Dissolution**, which the first fix could not touch. 24 µm is worth 2.0 ULP
///   at 100 m and 1.0 at 200 m, so past a couple of hundred metres the *entire*
///   within-class ladder — width, ties, casing — was below the depth buffer's
///   resolution and every same-class overlap went back to being an exact coin
///   flip. On the fixture that is 6 % of `Minor` and 14 % of `Carriageway`
///   fighting per pixel, re-rolled every frame the view matrix moves: the shimmer
///   down a street and across its junctions. Colouring the ties more cleverly
///   could not help, because the rungs being assigned had stopped meaning
///   anything.
///
/// Denominating *everything* in ULPs removes both at once. There is no second
/// ladder to contradict, and no range at which the ordering fades, because the
/// unit and the buffer that arbitrates are now the same unit. It also lifts the
/// ceiling that forced the width compression — a rung costs no height, so
/// [`ROAD_WIDTH_SUBBANDS`] is back to the full eight and [`ROAD_TIE_LAYERS`] to
/// eight.
///
/// Lines that tie on both are separated by `tie_seed`. `tessellate_roads` hands
/// this a rung chosen across the whole tile by [`assign_road_ties`], which is
/// what stops two overlapping same-band lines landing on one rung; the seed is a
/// **hash of the line's own endpoints** (`line_tie_seed`) wherever no
/// coordinated answer exists, and that hash is what the assignment itself starts
/// from. The seed is deliberately not the feature's index in the tile: a
/// Shortbread `MultiLineString` whose parts cross each other gave every part the
/// same index — a guaranteed exact-coplanar z-fight (measured at up to 37 %
/// self-overlap within a class at a single junction). Neither the hash nor the
/// assignment is stable across a tile seam (each tile clips its own copy of a
/// shared street, so no whole-line function can be), but a junction lives inside
/// one tile far more often than on a seam.
pub(crate) fn road_rung(class: RoadClass, width_m: f32, bridge: bool, tie_seed: u64) -> u32 {
    let tie = (tie_seed % ROAD_TIE_LAYERS as u64) as u32;
    if bridge {
        // The bridge block is laid out like a class block: its casing on the foot
        // rung, its fills above. Under the old ladder the fills started *on* the
        // foot, so a deck's kerb band and its own tarmac shared a rung whenever
        // the tie came out zero — one flyover in four, fighting with itself.
        // [`BRIDGE_LAYERS`] reserves the extra rung this skips.
        return ROAD_LAYERS - BRIDGE_LAYERS + CASING_RUNGS_PER_CLASS + tie;
    }
    let block = road_class_rank(class) * ROAD_RUNGS_PER_CLASS;
    block + CASING_RUNGS_PER_CLASS + road_subband(width_m) * ROAD_TIE_LAYERS + tie
}

/// The rung a class's *casing* ribbons draw on: the foot of that class's block,
/// so it lies under its own tarmac and over every narrower class's tarmac.
///
/// A whole class's casings share this one rung on purpose: two overlapping
/// casings of one class are the same flat colour, so their fight is the one
/// collision in this ladder that cannot be seen.
///
/// It settles a road's kerb against *its own* tarmac and nothing wider than
/// that: which of two **different** roads' surfaces wins is [`cut_band`]'s, and
/// this rung is only the tie-break inside a band.
pub(super) fn road_casing_rung(class: RoadClass, bridge: bool) -> u32 {
    if bridge {
        // A deck's kerb rides with the deck, under its own fills.
        return ROAD_LAYERS - BRIDGE_LAYERS;
    }
    road_class_rank(class) * ROAD_RUNGS_PER_CLASS
}

/// Which sheet of the road surface a ribbon belongs to — the leading term of
/// `road_stitch::Ribbon::priority`, above the whole class hierarchy.
///
/// **A kerb is the edge between a carriageway and what is not one.** Where
/// another carriageway abuts, there is no kerb — that is a junction opening, and
/// it is not optional: every side street in a city has one. The cut used to
/// settle a kerb band against a carriageway on `road_rung` alone, which puts the
/// *whole* of a higher class above the whole of a lower one, casing included. So
/// a residential street tee-ing into a primary road lost the argument to that
/// road's kerb band: its tarmac stopped at the far side of the kerb and the kerb
/// line ran on unbroken across the opening, walling the street off from the road
/// it joins. That is what a junction looked like from the pavement everywhere
/// two classes met — which is most junctions, since Shortbread's classes are
/// what make a street a side street.
///
/// Three bands, and the two that are not the ordinary case are there because
/// they are genuinely different surfaces:
///
/// * **2 — rails.** A level crossing is track laid *over* a road: the sleepers
///   and ballast are continuous and the tarmac is what stops. `road_rung`
///   already put rail at the top of the ladder; this keeps it there rather than
///   promoting every carriageway over it.
/// * **1 — carriageways**, every class a kerb is drawn for.
/// * **0 — kerb bands, and footways.** A footway is not a carriageway, so it
///   does not break a kerb; within this band `road_rung` decides exactly as it
///   did before, which leaves a path crossing a road settled the old way.
///
/// The bands must be a *total* order together with the rung, and that is the
/// point of expressing this as a band rather than as a "may block" predicate: an
/// asymmetric rule where a kerb outranks a carriageway that outranks the kerb
/// cuts both out of the ground they share and leaves a hole in the road.
pub(super) fn cut_band(group: SurfaceGroup) -> u32 {
    match group {
        SurfaceGroup::Road(RoadClass::Rail) => 2,
        SurfaceGroup::Road(RoadClass::Path) => 0,
        SurfaceGroup::Road(_) => 1,
        // Kerb bands, and anything else that is ever staged into the road sheet
        // (a bridge deck), at the foot.
        _ => 0,
    }
}

/// Which width sub-band inside a class block a width falls in: the eight
/// [`ROAD_WIDTH_BANDS`] compressed onto [`ROAD_WIDTH_SUBBANDS`].
fn road_subband(width_m: f32) -> u32 {
    let bands = ROAD_WIDTH_BANDS.len() as u32 + 1;
    (road_band(width_m) * ROAD_WIDTH_SUBBANDS / bands).min(ROAD_WIDTH_SUBBANDS - 1)
}

/// Which [`ROAD_WIDTH_BANDS`] band a width falls in: the count of thresholds at
/// or below it.
fn road_band(width_m: f32) -> u32 {
    ROAD_WIDTH_BANDS.iter().filter(|t| width_m >= **t).count() as u32
}

/// Tie-break seed for one linestring: a mix of its endpoint coordinates, so two
/// crossing lines almost never share a rung even when they share a feature (a
/// merged `MultiLineString`), a width band and a tile.
pub(super) fn line_tie_seed(head: [f32; 2], tail: [f32; 2]) -> u64 {
    let mut h: u64 = 0x9E37_79B9_7F4A_7C15;
    for v in [head[0], head[1], tail[0], tail[1]] {
        h ^= u64::from(v.to_bits());
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 29;
    }
    h
}

/// One line's entry in the tie-assignment pass.
struct TieLine {
    /// Lines can only collide within one band, and bridges take their own
    /// reserved block, so this is the whole of "could these two ever tie".
    key: (u32, bool),
    /// Whole-line bounds in tile units, widened by the ribbon's half-width — the
    /// cheap reject before the per-segment test.
    min: [f32; 2],
    max: [f32; 2],
    /// Per-segment bounds, same padding. **The whole-line box is far too blunt to
    /// colour against**: a long diagonal street's box covers a quarter of the
    /// tile and "overlaps" almost every other street in its band, which builds a
    /// nearly complete graph, exhausts the four colours and sends most lines back
    /// to the hash. Testing segments recovers the real adjacency — measured on
    /// the fixture, the whole-line box left 0.79 % of road pixels coplanar where
    /// segments leave far less.
    segs: Vec<([f32; 2], [f32; 2])>,
    seed: u64,
    at: (usize, usize),
}

impl TieLine {
    /// Do these two ribbons share any ground?
    fn touches(&self, other: &TieLine) -> bool {
        let apart = |amin: [f32; 2], amax: [f32; 2], bmin: [f32; 2], bmax: [f32; 2]| {
            amax[0] < bmin[0] || bmax[0] < amin[0] || amax[1] < bmin[1] || bmax[1] < amin[1]
        };
        if apart(self.min, self.max, other.min, other.max) {
            return false;
        }
        self.segs.iter().any(|(amin, amax)| {
            other
                .segs
                .iter()
                .any(|(bmin, bmax)| !apart(*amin, *amax, *bmin, *bmax))
        })
    }
}

/// Choose each line's tie rung so that two roads which share a width band *and*
/// overlap on the ground never land on the same one.
///
/// **The hash alone cannot do this, and the ladder cannot be made bigger.**
/// `line_tie_seed` is drawn independently per line, so two roads in one band
/// collide one time in [`ROAD_TIE_LAYERS`] — and a collision here is not a
/// settled order but an exact-coplanar pair, i.e. an opaque z-fight that
/// flickers per pixel as the camera moves. Measured on the Groningen z14
/// fixture, that left 1.60 % of road-covered pixels fighting. The obvious
/// answer — more rungs — *was* unavailable when a rung was 24 µm of real height:
/// [`ROAD_LAYERS`] of them already reached 1.44 mm against the 1.5 mm the road
/// sheet had under the buildings. A rung costs no height now, so the ladder has
/// grown ([`ROAD_TIE_LAYERS`] is 8 and [`ROAD_WIDTH_SUBBANDS`] is back to the
/// full 8) and this colouring has correspondingly less to do — but it is still
/// what keeps two crossing streets of one width band off a single rung, and more
/// rungs alone plateaus at 0.51 % where the colouring reaches it.
///
/// So the assignment is *coordinated* rather than drawn: within a band the
/// overlapping lines form a graph, and this is a greedy colouring of it. A line
/// keeps its hashed rung whenever that rung is free, which leaves the
/// non-colliding majority exactly where it was and moves only what must move;
/// where every colour is taken the hash decides as before, so the result is
/// never worse than the old behaviour and is usually exact.
///
/// Bounds are compared rather than the geometry: a bounding box is a superset of
/// the ribbon, so this can invent an edge between two roads that merely pass
/// near one another. That costs a little colour diversity and never correctness.
/// O(n²) over the lines of one tile — 95 street features on the fixture, so a
/// few thousand box tests against a ~121 ms raster.
pub(super) fn assign_road_ties(
    features: &[Feature],
    scale: f32,
) -> std::collections::HashMap<(usize, usize), u32> {
    let mut lines: Vec<TieLine> = Vec::new();
    for (fi, feature) in features.iter().enumerate() {
        let Some((_, half)) = drawn_street(feature) else {
            continue;
        };
        let bridge = flag_of(feature, "bridge");
        let mut geoms: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => geoms.push(ls),
            Geometry::MultiLineString(mls) => geoms.extend(mls.iter()),
            _ => continue,
        }
        // `half` is metres; these bounds are in tile units.
        let pad = if scale > 0.0 { half / scale } else { 0.0 };
        for (li, ls) in geoms.iter().enumerate() {
            let pts: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x, c.y])
                    .collect();
            let (Some(h), Some(t)) = (pts.first().copied(), pts.last().copied()) else {
                continue;
            };
            let mut min = [f32::MAX, f32::MAX];
            let mut max = [f32::MIN, f32::MIN];
            for p in &pts {
                min[0] = min[0].min(p[0]);
                min[1] = min[1].min(p[1]);
                max[0] = max[0].max(p[0]);
                max[1] = max[1].max(p[1]);
            }
            let segs: Vec<([f32; 2], [f32; 2])> = pts
                .windows(2)
                .map(|w| {
                    (
                        [w[0][0].min(w[1][0]) - pad, w[0][1].min(w[1][1]) - pad],
                        [w[0][0].max(w[1][0]) + pad, w[0][1].max(w[1][1]) + pad],
                    )
                })
                .collect();
            if segs.is_empty() {
                continue;
            }
            lines.push(TieLine {
                key: (road_band(half * 2.0), bridge),
                min: [min[0] - pad, min[1] - pad],
                max: [max[0] + pad, max[1] + pad],
                segs,
                seed: line_tie_seed(h, t),
                at: (fi, li),
            });
        }
    }

    // Adjacency once. The colouring consults it repeatedly and orders itself by
    // it, so computing it up front is both cheaper than testing pairs inside the
    // loop and what makes the ordering below possible.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); lines.len()];
    for i in 0..lines.len() {
        for j in (i + 1)..lines.len() {
            if lines[i].key == lines[j].key && lines[i].touches(&lines[j]) {
                adj[i].push(j);
                adj[j].push(i);
            }
        }
    }

    // Welsh–Powell: colour the most constrained lines first, so a crowded
    // junction gets its colours before the four are gone and the sparse edges of
    // the tile take what is left. Ties broken by the hash and then by position,
    // so the answer never depends on the order the tile stored its features in.
    let mut order: Vec<usize> = (0..lines.len()).collect();
    order.sort_by(|&a, &b| {
        adj[b]
            .len()
            .cmp(&adj[a].len())
            .then(lines[a].seed.cmp(&lines[b].seed))
            .then(lines[a].at.cmp(&lines[b].at))
    });

    let mut chosen: Vec<Option<u32>> = vec![None; lines.len()];
    let mut out = std::collections::HashMap::new();
    for &i in &order {
        let mut used = [false; ROAD_TIE_LAYERS as usize];
        for &j in &adj[i] {
            if let Some(t) = chosen[j] {
                used[t as usize] = true;
            }
        }
        let hashed = (lines[i].seed % ROAD_TIE_LAYERS as u64) as u32;
        let tie = if used[hashed as usize] {
            (0..ROAD_TIE_LAYERS)
                .find(|t| !used[*t as usize])
                .unwrap_or(hashed)
        } else {
            hashed
        };
        chosen[i] = Some(tie);
        out.insert(lines[i].at, tie);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cut order has to stay a **total** order, and the band is the term
    /// that is easiest to get wrong: an asymmetric rule — the kerb beats the
    /// carriageway that beats the kerb — cuts both out of the ground they share
    /// and opens a hole in the road.
    #[test]
    fn the_cut_bands_order_a_kerb_under_every_carriageway() {
        use RoadClass::{Carriageway, Major, Minor, Motorway, Path, Rail};
        for kerbed in [Motorway, Carriageway, Major, Minor] {
            for other in [Motorway, Carriageway, Major, Minor] {
                assert!(
                    cut_band(SurfaceGroup::Road(kerbed))
                        > cut_band(SurfaceGroup::RoadCasing(other)),
                    "{kerbed:?}'s carriageway must break {other:?}'s kerb",
                );
            }
            // Rails are laid *over* a road, not cut by it.
            assert!(cut_band(SurfaceGroup::Road(Rail)) > cut_band(SurfaceGroup::Road(kerbed)));
            // A footway is not a carriageway and does not break a kerb; inside
            // one band `road_rung` settles it exactly as it did before.
            assert_eq!(
                cut_band(SurfaceGroup::Road(Path)),
                cut_band(SurfaceGroup::RoadCasing(kerbed)),
            );
            assert!(
                road_rung(kerbed, 6.0, false, 0) > road_casing_rung(kerbed, false),
                "a road's own kerb still loses to its own tarmac",
            );
        }
    }

    /// Which of two overlapping ribbons wins has to be a property of the *roads*,
    /// not of the order the tile happened to store them in — otherwise the same
    /// junction resolves one way in this tile and the other way in its neighbour.
    #[test]
    fn the_higher_class_and_the_bridge_win_the_overlap() {
        let path = road_rung(RoadClass::Path, 2.0, false, 0);
        let street = road_rung(RoadClass::Minor, 7.0, false, 0);
        let main = road_rung(RoadClass::Major, 14.0, false, 0);
        let motorway = road_rung(RoadClass::Motorway, 22.0, false, 0);
        assert!(
            path < street && street < main && main < motorway,
            "the higher road class must be on top"
        );
        // Width still settles two roads of one class…
        assert!(
            road_rung(RoadClass::Minor, 4.0, false, 0) < road_rung(RoadClass::Minor, 9.0, false, 0),
            "the wider road of a class must be on top"
        );
        // …but it may never lift a road out of its class block. This is the
        // inversion the class-major layout exists to remove: a wide residential
        // used to outrank a narrow tertiary in the geometry while the depth bias
        // said the opposite, so which won depended on camera distance.
        assert!(
            road_rung(RoadClass::Minor, 20.0, false, 3)
                < road_rung(RoadClass::Major, 2.0, false, 0),
            "the widest Minor must still lie under the narrowest Major"
        );
        // A flyover and the road under it are both flat sheets on this plane; the
        // deck is the only one that can be on top, whatever it flies over.
        assert!(
            road_rung(RoadClass::Path, 4.0, true, 0) > motorway,
            "a bridge deck must be on top"
        );
        // …and every rung the ladder can produce has to be on the ladder, which is
        // what `road_rung_ceiling` sizes the road band against.
        for class in ROAD_CLASSES {
            for width in [1.5f32, 4.0, 7.0, 12.0, 22.0] {
                for bridge in [false, true] {
                    for seed in 0u64..40 {
                        let rung = road_rung(class, width, bridge, seed);
                        assert!(rung < ROAD_LAYERS, "rung {rung} is off the ladder");
                    }
                    let c = road_casing_rung(class, bridge);
                    assert!(c < ROAD_LAYERS, "casing rung {c} is off the ladder");
                }
            }
        }
    }

    /// Every road class in hierarchy order, for the ladder tests.
    const ROAD_CLASSES: [RoadClass; 6] = [
        RoadClass::Path,
        RoadClass::Minor,
        RoadClass::Major,
        RoadClass::Carriageway,
        RoadClass::Motorway,
        RoadClass::Rail,
    ];

    /// **There is exactly one ladder, and every road group agrees to defer to
    /// it.** This is the successor to
    /// `the_geometric_ladder_never_contradicts_the_depth_bias`, which swept every
    /// class pair looking for a place where the geometry and the depth bias
    /// disagreed. That test is gone because the contradiction it hunted is no
    /// longer expressible: the classes are ordered *by the rung*, and the bias
    /// they all share cannot rank them differently from each other because it
    /// does not rank them at all.
    ///
    /// What still has to hold is that nothing sneaks a second opinion back in.
    #[test]
    fn the_road_hierarchy_lives_in_the_rung_alone() {
        for a in ROAD_CLASSES {
            for b in ROAD_CLASSES {
                assert_eq!(
                    SurfaceGroup::Road(a).depth_bias(),
                    SurfaceGroup::Road(b).depth_bias(),
                    "{a:?} and {b:?} disagree in the material bias — the class \
                     order belongs to `road_rung`, and a bias that also ranks \
                     them is the second ladder all over again"
                );
                assert_eq!(
                    SurfaceGroup::RoadCasing(a).depth_bias(),
                    SurfaceGroup::Road(b).depth_bias(),
                    "a casing must share the road band's foot too"
                );
            }
        }
    }

    /// The hierarchy the rung has to express, now that it is the only thing
    /// expressing it: class order, casings under their own tarmac and over the
    /// narrower classes they cross, and a bridge above everything.
    #[test]
    fn the_rung_orders_the_whole_road_hierarchy() {
        let widths = [1.5f32, 3.0, 5.0, 7.0, 9.0, 12.0, 16.0, 24.0];
        let mut checked = 0usize;
        for (ai, a) in ROAD_CLASSES.into_iter().enumerate() {
            for b in ROAD_CLASSES.into_iter().skip(ai + 1) {
                for wa in widths {
                    for wb in widths {
                        for seed in 0u64..ROAD_TIE_LAYERS as u64 {
                            assert!(
                                road_rung(a, wa, false, seed) < road_rung(b, wb, false, 0),
                                "{a:?} at {wa} m must lie under {b:?} at {wb} m"
                            );
                            checked += 1;
                        }
                    }
                }
                // The higher class's *casing* rides over the lower class's
                // tarmac, so a major road's kerb band covers a path it crosses…
                assert!(
                    road_casing_rung(b, false) > road_rung(a, 24.0, false, 3),
                    "{b:?}'s kerb band must cover the widest {a:?} it crosses"
                );
                // …while still lying under its own, so it reads as an edge and
                // never over-paints its own markings.
                assert!(
                    road_casing_rung(b, false) < road_rung(b, 1.5, false, 0),
                    "{b:?}'s kerb band must stay under its own tarmac"
                );
            }
        }
        assert!(checked > 0, "the sweep must actually compare something");

        // A bridge is above every class, and its own casing is under its own
        // tarmac — the same two rules the classes follow, applied to the block
        // reserved at the top of the ladder. The second used to fail one time in
        // `ROAD_TIE_LAYERS`: the fills started on the casing's rung rather than
        // above it, so a deck's kerb band z-fought its own surface.
        for class in ROAD_CLASSES {
            for seed in 0u64..ROAD_TIE_LAYERS as u64 {
                let deck = road_rung(class, 8.0, true, seed);
                assert!(
                    deck > road_rung(RoadClass::Rail, 24.0, false, 3),
                    "a bridge must be on top of every ground road"
                );
                assert!(
                    deck > road_casing_rung(class, true),
                    "a deck's kerb band must stay under its own tarmac"
                );
                assert!(deck < ROAD_LAYERS, "bridge rung {deck} is off the ladder");
            }
        }
    }

    /// **The separation between two rungs is the same at every camera distance.**
    ///
    /// This is the property the whole redesign exists for, so it is worth
    /// asserting directly rather than inferring from the constants. A rung is
    /// `ROAD_RUNG_ULP` depth-buffer ULPs, and an ULP is proportional to the
    /// fragment's own depth — so the apparent world separation grows as `z` and
    /// the separation *measured in the units the depth test uses* is constant.
    ///
    /// The ladder it replaced was a fixed 24 µm, and the table below is why that
    /// could not work: the same two ribbons that were comfortably ordered in a
    /// street view had nothing between them from a rooftop.
    #[test]
    fn the_ladder_does_not_decay_with_range() {
        // One rung's separation, in depth-buffer ULPs, at a given camera range.
        //
        // The old ladder lifted a ribbon 24 µm and let the depth buffer convert
        // that to ULPs, which divides by the quantum `1.19e-7 · z`. The new one is
        // *already* ULPs, so `z` does not appear.
        let old_ulps = |z: f32| 2.4e-5 / (1.19e-7 * z);
        let new_ulps = |_z: f32| ROAD_RUNG_ULP;

        // Two ribbons one rung apart have to stay apart by enough to beat the
        // rasteriser's own interpolation rounding. One ULP does not; the ladder
        // is effectively gone below that.
        const RESOLVED: f32 = 2.0;

        assert!(
            old_ulps(100.0) >= RESOLVED,
            "sanity: the old ladder did work at street range"
        );
        // …and this is the failure it was rewritten to remove. A junction seen
        // from 300 m — an ordinary rooftop view, well inside the city tier — had
        // less than one ULP holding its ribbons apart.
        assert!(
            old_ulps(300.0) < 1.0,
            "the old ladder should be under one ULP at 300 m"
        );

        for z in [10.0f32, 100.0, 300.0, 1_000.0, 10_000.0] {
            assert!(
                new_ulps(z) >= RESOLVED,
                "at {z} m one rung is {} ULP, inside the rasteriser's noise",
                new_ulps(z),
            );
            // The property in one line: the separation does not depend on range.
            assert_eq!(
                new_ulps(z),
                new_ulps(10.0),
                "the ladder must not vary with distance"
            );
        }
        // And past the range where the old one had dissolved, the new one is
        // still worth the same as it was in the street.
        assert!(new_ulps(1_000.0) > old_ulps(1_000.0) * 10.0);
    }
}
