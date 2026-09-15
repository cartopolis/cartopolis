//! Cutting coplanar ground surfaces against each other, so no two of them cover
//! the same patch — the road ribbons first, and the ground fills since.
//!
//! Road ribbons used to **overlap by construction** — a junction stub crosses
//! whatever it tees into, Shortbread's merged ways cross each other freely, and
//! every kerb band was drawn as a slab the full width of the road with the tarmac
//! laid on top of it. Every one of those overlaps was two opaque coplanar
//! triangles whose depth values agree to the last few bits, so which one the
//! rasteriser drew was decided by rounding and re-decided every frame the view
//! matrix moved: the shimmer down a street and the bleeding at its junctions.
//! Measured on the Groningen z14 fixture, **76 % of road pixels were covered
//! twice** and 12 % three times or more.
//!
//! Three earlier attempts settled it by *ordering* the overlap rather than
//! removing it — a lift in millimetres, then a depth bias per material, then a
//! depth offset per vertex. The last of those is sound in principle and its unit
//! does not decay with range, but it is still a tie-break, and a tie-break only
//! works while the separation it asks for is larger than the rasteriser's own
//! rounding. One rung was four depth-buffer units; the sibling ladder that orders
//! grass against water against road needs sixty-four, and its own comment records
//! that sixteen was measured as not enough at the shallow angles a low camera
//! looks at the middle distance with. A road is the surface you almost always see
//! at a shallow angle. On the fixture, 1.9 % of road pixels were held apart by one
//! rung and 3.3 % by three rungs or fewer.
//!
//! So the overlap goes away instead. Each ribbon keeps only the part of itself
//! that no higher-priority ribbon claims, and what is drawn is then a partition:
//! each patch of ground belongs to exactly one ribbon, and there is nothing left
//! for the depth buffer to arbitrate. The cut works directly on the ribbons'
//! **triangles** — the ones about to be drawn, not a separate description of where
//! they are — so it cannot disagree with what it is cutting.
//!
//! ## Why this cannot open a crack
//!
//! The failure everyone expects from cutting shapes into each other is a hairline
//! gap, where two pieces worked out their shared edge separately in floating point
//! and landed a fraction apart. It cannot happen here, for the same reason the
//! mitred junction joins in `vector_tiles` are watertight: there is nothing left
//! to disagree about.
//!
//! * Every road vertex is snapped to a **millimetre lattice** before the cut, and
//!   the snapped coordinates are what gets drawn. A ribbon that is cut and a
//!   neighbour that is left whole therefore quantise identically.
//! * The cut runs on `i_overlay`'s **integer** interface, on that same lattice. A
//!   boundary produced by subtracting B from A is built from B's own vertices and
//!   the exact lattice points where the two boundaries cross, so the edge A ends
//!   on and the edge B starts from are the same two points, not two computations
//!   of one point.
//! * A triangle no higher-priority ribbon reaches is passed through untouched, so
//!   the strip's own tessellation — and with it the texture registration down a
//!   curve — survives everywhere except the junctions themselves.
//!
//! The fixture measures the result directly: road pixels covered twice fall from
//! 76.1 % to 0.8 %, while the *covered* area changes by 4 pixels in 423,028
//! (0.001 %). Nothing was given up but the double coverage.
//!
//! ## What still overlaps, deliberately
//!
//! A flyover and the road beneath it. Both are drawn on the ground plane here, so
//! they are two real surfaces in one place and cutting one out of the other would
//! be a lie about which is which. Bridges are cut against each other and against
//! nothing else, and the depth rung keeps a deck above the road it crosses.
//!
//! A ribbon also still overlaps *itself* a little, where a bevel fill at a tight
//! corner laps back over the strip — 0.8 % of road pixels. That is the one
//! collision this ladder never had to settle: one ribbon is one material, one
//! colour and one continuous run of texture coordinates, so which of its own two
//! triangles wins is not visible.
//!
//! ## It is not only the roads any more
//!
//! `vector_tiles::tessellate_surfaces` cuts the ground *fills* through the same
//! [`stitch`] — a cemetery inside a park is the same coplanar overlap a
//! junction stub is, and 9.7 % of the grass over Groningen was covered twice by
//! its own polygons. The module keeps its name because the roads are where the
//! argument above was worked out, and because a `Ribbon` is still what it takes:
//! a little mesh with a place in a strict order. Its two attribute sets are
//! optional, so a fill passes neither and pays for neither.
//!
//! What that changed here is the index. A fill's triangles are nothing like a
//! road's — see [`MIN_BUCKET_M`] — and sizing the grid to the geometry rather than
//! to a carriageway took the fills from 317 ms to 49 ms *and* the roads from
//! 534 ms to about 300 ms, for the same output. `docs/notes/the-ground-stack.md`
//! carries both halves.

use crate::vector_tiles::SurfaceGroup;
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay::{Overlay, ShapeType};
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;

/// Lattice the cut runs on: integer millimetres of tile-local metres.
///
/// A road is metres wide and a kerb band 0.75 m, so a millimetre is three orders
/// of magnitude below anything this geometry means. What it buys is that every
/// coordinate in the road sheet is an exact integer, which is what makes a shared
/// boundary the *same* boundary for both pieces rather than two floating-point
/// opinions about one line.
///
/// A tile is 1,000 m across and the ribbons reach a little past it, so the range
/// is about ±1.3 × 10⁶ — comfortably inside `i32`.
const MM_PER_M: f32 = 1000.0;

/// Smallest side of one bucket of the blocker index, in metres.
///
/// The index is what makes the cut affordable. A bounding box is a hopeless proxy
/// for a road — a street running the diagonal of the tile has a box covering a
/// quarter of it — so "does this triangle's box meet that ribbon's box" answered
/// *yes* for almost every triangle of every blocked ribbon, and each of those
/// answers cost a boolean against a contour hundreds of points long. Eight metres
/// is a little under the narrowest carriageway plus its kerbs, so a bucket holds
/// the handful of road triangles that genuinely pass through it.
///
/// It is a **floor** rather than the size, because the ground fills are cut by the
/// same machinery and their triangles are nothing like a road's. Earcut answers a
/// landuse polygon with as few triangles as it can, so a residential block arrives
/// as two triangles spanning half the tile — at a fixed eight metres each of those
/// is inserted into some 8,000 buckets and read back out of every one of them.
/// Measured over the Groningen fixture: `tessellate_surfaces` went from 1.46 ms to
/// **317 ms**, all of it index bookkeeping rather than geometry.
const MIN_BUCKET_M: f32 = 8.0;

/// How many buckets a triangle should span, on average.
///
/// [`Buckets::build`] sizes the grid from the mean triangle extent so that this
/// holds whatever it is handed. One, because the point of the index is that the
/// common case reads a single bucket's list and that list is already short:
/// smaller and a big triangle pays to be written into hundreds of cells, larger
/// and each lookup drags in neighbours the separating-axis test then has to
/// reject.
const SPANS_PER_TRI: f32 = 1.0;

/// One ribbon as it comes out of `vector_tiles::ribbon`: its own little mesh, and
/// where it stands in the cut order.
pub(crate) struct Ribbon {
    /// Which [`SurfaceGroup`] mesh the survivors are appended to.
    pub group: SurfaceGroup,
    /// The depth rung every one of its vertices carries.
    pub rung: f32,
    /// Cut order — **strictly** ordered, so "who wins this ground" is never a
    /// draw, and strictly ordered is not optional: an asymmetric rule (this beats
    /// that, that beats this) cuts *both* out of the ground they share and opens
    /// a hole.
    ///
    /// `(band, rung, feature, linestring, part)`. The band is
    /// `vector_tiles::cut_band` — which sheet of the road surface this ribbon
    /// belongs to, and the reason it exists is the kerb: a kerb band is the edge
    /// between a carriageway and what is *not* one, so wherever another
    /// carriageway abuts there is no kerb, whatever the two roads' classes are.
    /// Then `road_rung`, which is the road hierarchy proper. The rest is the
    /// ribbon's position in the tile, which only has to be *some* stable answer.
    pub priority: (u32, u32, usize, usize, usize),
    /// A bridge is cut against other bridges and against nothing else — see the
    /// module note.
    pub bridge: bool,
    pub positions: Vec<[f32; 3]>,
    /// Texture coordinates, or empty. A road ribbon carries them (the lane
    /// markings are registered down the strip); a ground fill does not.
    pub uvs: Vec<[f32; 2]>,
    /// Per-vertex colour, or empty. A surveyed pavement and a crop parcel carry
    /// the form's own tone here; a road does not.
    ///
    /// Both attribute sets are optional and interpolated the same way — the cut
    /// only ever produces points inside the triangle it is cutting, so the
    /// barycentric weights carry whatever that triangle had across.
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

/// `[min_x, min_z, max_x, max_z]` in tile-local metres.
type Bbox = [f32; 4];

fn overlaps(a: &Bbox, b: &Bbox) -> bool {
    a[2] >= b[0] && b[2] >= a[0] && a[3] >= b[1] && b[3] >= a[1]
}

fn to_mm(v: f32) -> i32 {
    (v * MM_PER_M).round() as i32
}

fn from_mm(v: i32) -> f32 {
    v as f32 / MM_PER_M
}

/// One triangle of one ribbon, as the cut sees it.
#[derive(Clone, Copy)]
struct Tri {
    ribbon: u32,
    /// Which triangle of that ribbon this is, counting from zero.
    ///
    /// A ribbon is cut against *itself* as well as against its superiors (see
    /// [`cut_ribbon`]), and this is the order that cut runs in: the earlier
    /// triangle keeps the ground. It has to be a strict total order for the same
    /// reason `priority` does — an order with a draw in it cuts both sides out of
    /// the ground they share and opens a hole.
    seq: u32,
    /// Corners on the millimetre lattice, wound as `push_triangle_up` wound them
    /// — which is one consistent direction for every road triangle in the tile,
    /// and therefore safe to feed to a non-zero fill rule as a set.
    p: [IntPoint<i32>; 3],
    bbox: Bbox,
}

/// Which road triangles pass through each bucket of a coarse grid over the tile.
struct Buckets {
    origin: [f32; 2],
    /// Side of one bucket, chosen from the data — see [`MIN_BUCKET_M`].
    edge: f32,
    cols: usize,
    rows: usize,
    cells: Vec<Vec<u32>>,
    tris: Vec<Tri>,
}

impl Buckets {
    fn build(ribbons: &[Ribbon]) -> Self {
        let mut world = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for r in ribbons {
            for p in &r.positions {
                world[0] = world[0].min(p[0]);
                world[1] = world[1].min(p[2]);
                world[2] = world[2].max(p[0]);
                world[3] = world[3].max(p[2]);
            }
        }
        if world[0] > world[2] {
            world = [0.0, 0.0, 0.0, 0.0];
        }
        // Size the grid to the geometry. One pass to measure the mean triangle,
        // one to fill — cheap beside what a mis-sized grid costs.
        let mut extent_sum = 0.0f32;
        let mut tri_count = 0usize;
        for r in ribbons {
            for t in r.indices.chunks_exact(3) {
                let b = bbox_of(&[
                    flat_of(&r.positions, t[0]),
                    flat_of(&r.positions, t[1]),
                    flat_of(&r.positions, t[2]),
                ]);
                extent_sum += (b[2] - b[0]).max(b[3] - b[1]);
                tri_count += 1;
            }
        }
        let mean_extent = if tri_count > 0 {
            extent_sum / tri_count as f32
        } else {
            MIN_BUCKET_M
        };
        let edge = (mean_extent / SPANS_PER_TRI).max(MIN_BUCKET_M);
        let cols = (((world[2] - world[0]) / edge).ceil() as usize + 1).max(1);
        let rows = (((world[3] - world[1]) / edge).ceil() as usize + 1).max(1);
        let mut me = Self {
            origin: [world[0], world[1]],
            edge,
            cols,
            rows,
            cells: vec![Vec::new(); cols * rows],
            tris: Vec::with_capacity(tri_count),
        };
        for (i, r) in ribbons.iter().enumerate() {
            for (seq, t) in r.indices.chunks_exact(3).enumerate() {
                let flat: [[f32; 2]; 3] = [
                    flat_of(&r.positions, t[0]),
                    flat_of(&r.positions, t[1]),
                    flat_of(&r.positions, t[2]),
                ];
                let bbox = bbox_of(&flat);
                let id = me.tris.len() as u32;
                me.tris.push(Tri {
                    ribbon: i as u32,
                    seq: seq as u32,
                    p: [
                        IntPoint::new(to_mm(flat[0][0]), to_mm(flat[0][1])),
                        IntPoint::new(to_mm(flat[1][0]), to_mm(flat[1][1])),
                        IntPoint::new(to_mm(flat[2][0]), to_mm(flat[2][1])),
                    ],
                    bbox,
                });
                let (x0, y0, x1, y1) = me.range(&bbox);
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        me.cells[y * me.cols + x].push(id);
                    }
                }
            }
        }
        me
    }

    fn range(&self, b: &Bbox) -> (usize, usize, usize, usize) {
        let edge = self.edge;
        let cell = |v: f32, o: f32, n: usize| -> usize {
            (((v - o) / edge).floor().max(0.0) as usize).min(n.saturating_sub(1))
        };
        let x0 = cell(b[0], self.origin[0], self.cols);
        let y0 = cell(b[1], self.origin[1], self.rows);
        let x1 = cell(b[2], self.origin[0], self.cols);
        let y1 = cell(b[3], self.origin[1], self.rows);
        (x0.min(x1), y0.min(y1), x1.max(x0), y1.max(y0))
    }

    /// Every road triangle sharing a bucket with this box, into `out`.
    ///
    /// A road triangle at z14 is usually a fraction of a bucket across, so the
    /// common case is one bucket and its list is already unique — the sort and
    /// dedup only earn their keep when a triangle straddles a bucket line.
    fn candidates(&self, b: &Bbox, out: &mut Vec<u32>) {
        out.clear();
        let (x0, y0, x1, y1) = self.range(b);
        if x0 == x1 && y0 == y1 {
            out.extend_from_slice(&self.cells[y0 * self.cols + x0]);
            return;
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                out.extend_from_slice(&self.cells[y * self.cols + x]);
            }
        }
        out.sort_unstable();
        out.dedup();
    }
}

/// Do these two lattice triangles share any *area*?
///
/// The separating-axis test, in exact integer arithmetic on the millimetre
/// lattice. Touching along an edge or at a corner counts as **not** overlapping,
/// which is the answer this wants: two ribbons that abut have nothing to cut, and
/// telling them apart from two that cross is most of what makes the cut cheap.
///
/// It exists because a bounding box is a poor test for a long thin triangle laid
/// down a road at an angle: on the Groningen fixture the box test let 10.4 clip
/// triangles into the average boolean, where about two of them actually met the
/// subject. Every one of the other eight was sorted, split and graph-walked for
/// nothing.
fn tris_overlap(a: &[IntPoint<i32>; 3], b: &[IntPoint<i32>; 3]) -> bool {
    !separated(a, b) && !separated(b, a)
}

/// Does `outer` cover every corner of `inner`?
///
/// Both are convex, so this settles the whole triangle. It is worth its own test
/// because the case is common and the answer is the expensive one: at the middle
/// of a crossing the road underneath is wholly covered, and asking the boolean
/// solver produces an empty result after all the same sorting and graph-building
/// it would do for a real cut.
fn contains(outer: &[IntPoint<i32>; 3], inner: &[IntPoint<i32>; 3]) -> bool {
    let side = |a: &IntPoint<i32>, b: &IntPoint<i32>, p: &IntPoint<i32>| -> i64 {
        (i64::from(b.x) - i64::from(a.x)) * (i64::from(p.y) - i64::from(a.y))
            - (i64::from(b.y) - i64::from(a.y)) * (i64::from(p.x) - i64::from(a.x))
    };
    // Winding is consistent across every road triangle, but read it rather than
    // assume it: a degenerate one would answer zero and must not be trusted.
    let turn = side(&outer[0], &outer[1], &outer[2]);
    if turn == 0 {
        return false;
    }
    let want_positive = turn > 0;
    for i in 0..3 {
        let (a, b) = (&outer[i], &outer[(i + 1) % 3]);
        for p in inner {
            let s = side(a, b, p);
            if s != 0 && (s > 0) != want_positive {
                return false;
            }
        }
    }
    true
}

/// Is there an axis, among the edge normals of `a`, that puts the two apart?
fn separated(a: &[IntPoint<i32>; 3], b: &[IntPoint<i32>; 3]) -> bool {
    for i in 0..3 {
        let p = a[i];
        let q = a[(i + 1) % 3];
        let nx = i64::from(q.y) - i64::from(p.y);
        let ny = i64::from(p.x) - i64::from(q.x);
        let proj = |r: &IntPoint<i32>| {
            nx * (i64::from(r.x) - i64::from(p.x)) + ny * (i64::from(r.y) - i64::from(p.y))
        };
        let (a0, a1, a2) = (proj(&a[0]), proj(&a[1]), proj(&a[2]));
        let (b0, b1, b2) = (proj(&b[0]), proj(&b[1]), proj(&b[2]));
        let a_lo = a0.min(a1).min(a2);
        let a_hi = a0.max(a1).max(a2);
        let b_lo = b0.min(b1).min(b2);
        let b_hi = b0.max(b1).max(b2);
        if a_hi <= b_lo || b_hi <= a_lo {
            return true;
        }
    }
    false
}

fn flat_of(positions: &[[f32; 3]], i: u32) -> [f32; 2] {
    let p = positions[i as usize];
    [p[0], p[2]]
}

fn bbox_of(p: &[[f32; 2]; 3]) -> Bbox {
    [
        p[0][0].min(p[1][0]).min(p[2][0]),
        p[0][1].min(p[1][1]).min(p[2][1]),
        p[0][0].max(p[1][0]).max(p[2][0]),
        p[0][1].max(p[1][1]).max(p[2][1]),
    ]
}

/// Snap one ribbon's vertices to the millimetre lattice.
///
/// Called on **every** ribbon, cut or not: a ribbon that is left whole has to
/// quantise the same way as one that is cut, or the two would meet half a
/// millimetre apart along the edge they share.
fn snap(r: &mut Ribbon) {
    for p in &mut r.positions {
        p[0] = from_mm(to_mm(p[0]));
        p[2] = from_mm(to_mm(p[2]));
    }
}

/// Where a point sits inside a triangle, as the three barycentric weights.
///
/// The cut only ever produces points inside (or exactly on the boundary of) the
/// triangle being cut, so the weights are what carry the original texture
/// coordinates across — no reprojection onto the centre line, and therefore no
/// second opinion about where the lane markings go.
fn barycentric(tri: [[f32; 2]; 3], p: [f32; 2]) -> [f32; 3] {
    let (a, b, c) = (tri[0], tri[1], tri[2]);
    let v0 = [b[0] - a[0], b[1] - a[1]];
    let v1 = [c[0] - a[0], c[1] - a[1]];
    let v2 = [p[0] - a[0], p[1] - a[1]];
    let den = v0[0] * v1[1] - v1[0] * v0[1];
    if den.abs() < 1e-12 {
        return [1.0, 0.0, 0.0];
    }
    let w1 = (v2[0] * v1[1] - v1[0] * v2[1]) / den;
    let w2 = (v0[0] * v2[1] - v2[0] * v0[1]) / den;
    [1.0 - w1 - w2, w1, w2]
}

/// Cut every ribbon down to the ground no higher-priority ribbon has claimed.
///
/// Order-independent by construction, and deliberately so: a ribbon is cut
/// against the **uncut** geometry of all its superiors at once, never against a
/// running union, so what survives does not depend on the order this walks the
/// list in. Every patch of ground ends up with the single highest-priority ribbon
/// that covers it.
pub(crate) fn stitch(ribbons: &mut [Ribbon]) {
    for r in ribbons.iter_mut() {
        snap(r);
    }
    let buckets = Buckets::build(ribbons);
    // One overlay reused across the whole tile: building a fresh one per triangle
    // is a large share of what a cut costs, and there are thousands of them.
    let mut overlay: Overlay<i32> = Overlay::new(64);
    let mut scratch: Vec<u32> = Vec::new();
    let mut cuts: Vec<(usize, Cut)> = Vec::new();

    for i in 0..ribbons.len() {
        if let Some(cut) = cut_ribbon(i, ribbons, &buckets, &mut overlay, &mut scratch) {
            cuts.push((i, cut));
        }
    }
    for (i, cut) in cuts {
        ribbons[i].positions = cut.positions;
        ribbons[i].uvs = cut.uvs;
        ribbons[i].colors = cut.colors;
        ribbons[i].indices = cut.indices;
    }
}

/// What survives one ribbon's cut.
pub(crate) struct Cut {
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

/// The parts of one ribbon's triangles that no higher-priority ribbon claims, or
/// `None` if nothing claims any of it and the ribbon can be left exactly as laid.
///
/// The clip is built out of the blockers' own **triangles**, three points each,
/// rather than their ribbon contours. That is what makes this affordable: a road
/// contour at z14 is hundreds of points and every one of them was being fed to
/// every boolean, where what actually decides the cut is the two or three
/// triangles of the crossing road that reach this triangle. Same answer — a set of
/// consistently wound triangles is its own union under a non-zero fill rule — for
/// a fraction of the work.
fn cut_ribbon(
    i: usize,
    ribbons: &[Ribbon],
    buckets: &Buckets,
    overlay: &mut Overlay<i32>,
    scratch: &mut Vec<u32>,
) -> Option<Cut> {
    let r = &ribbons[i];
    let has_uvs = !r.uvs.is_empty();
    let has_colors = !r.colors.is_empty();
    let mut touched = false;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(r.positions.len());
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(r.uvs.len());
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(r.colors.len());
    let mut indices: Vec<u32> = Vec::with_capacity(r.indices.len());
    // Vertices are re-emitted per triangle rather than shared. A cut triangle
    // needs its own anyway, and a road ribbon is a few hundred vertices — the
    // duplication costs less than de-duplicating them would.
    let mut emit = |tri: [[f32; 3]; 3], uv: [[f32; 2]; 3], col: [[f32; 4]; 3]| {
        let base = positions.len() as u32;
        for k in 0..3 {
            positions.push(tri[k]);
            if has_uvs {
                uvs.push(uv[k]);
            }
            if has_colors {
                colors.push(col[k]);
            }
        }
        indices.extend([base, base + 1, base + 2]);
    };

    let y = r.positions.first().map_or(0.0, |p| p[1]);
    for (seq, t) in r.indices.chunks_exact(3).enumerate() {
        let vs = [
            r.positions[t[0] as usize],
            r.positions[t[1] as usize],
            r.positions[t[2] as usize],
        ];
        let at_uv = |k: u32| if has_uvs { r.uvs[k as usize] } else { [0.0; 2] };
        let at_col = |k: u32| {
            if has_colors {
                r.colors[k as usize]
            } else {
                [0.0; 4]
            }
        };
        let uv = [at_uv(t[0]), at_uv(t[1]), at_uv(t[2])];
        let col = [at_col(t[0]), at_col(t[1]), at_col(t[2])];
        let flat = [
            [vs[0][0], vs[0][2]],
            [vs[1][0], vs[1][2]],
            [vs[2][0], vs[2][2]],
        ];
        let tb = bbox_of(&flat);
        // The overwhelming majority of a road is nowhere near a junction. Those
        // triangles keep the strip's own tessellation, which is what keeps the
        // markings registered down a curve.
        buckets.candidates(&tb, scratch);
        let subj = [
            IntPoint::new(to_mm(flat[0][0]), to_mm(flat[0][1])),
            IntPoint::new(to_mm(flat[1][0]), to_mm(flat[1][1])),
            IntPoint::new(to_mm(flat[2][0]), to_mm(flat[2][1])),
        ];
        let mut any = false;
        let mut buried = false;
        overlay.clear();
        for c in scratch.iter() {
            let other = &buckets.tris[*c as usize];
            let j = other.ribbon as usize;
            if !overlaps(&tb, &other.bbox) {
                continue;
            }
            if j == i {
                // A ribbon laps over *itself* wherever its centre line doubles
                // back — a hairpin, a spur drawn out and back along one
                // alignment. The two layers are the same material, which is why
                // this used to be left alone, but they are **not** the same
                // texture: `v` is metres travelled, so the returning leg carries
                // different lane dashes and edge lines from the outgoing one at
                // the same depth. That is a per-pixel coin flip that re-rolls
                // with the view matrix — the very thing the cut exists to
                // remove, surviving inside the one case it exempted.
                //
                // The earlier triangle keeps the ground. Adjacent strip
                // triangles share an edge and `tris_overlap` reads a shared edge
                // as *not* overlapping, so an honest strip is untouched and only
                // a fold loses its second layer.
                if other.seq >= seq as u32 {
                    continue;
                }
            } else {
                let o = &ribbons[j];
                // A flyover and the road under it are two surfaces in one place,
                // so they are never each other's blockers.
                if o.bridge != r.bridge || o.priority <= r.priority {
                    continue;
                }
            }
            if !tris_overlap(&subj, &other.p) {
                continue;
            }
            if contains(&other.p, &subj) {
                // Wholly claimed by one blocker: nothing survives, and there is
                // nothing for the solver to work out.
                buried = true;
                break;
            }
            overlay.add_contour(&other.p, ShapeType::Clip);
            any = true;
        }
        if buried {
            touched = true;
            continue;
        }
        if !any {
            emit(vs, uv, col);
            continue;
        }
        touched = true;
        overlay.add_contour(&subj, ShapeType::Subject);
        let kept = overlay.overlay(OverlayRule::Difference, FillRule::NonZero);
        for shape in &kept {
            // Contour 0 is the outer boundary and the rest are holes, which is
            // earcut's own convention once the holes are appended and their start
            // offsets handed over.
            let mut flat_xy: Vec<f64> = Vec::new();
            let mut holes: Vec<usize> = Vec::new();
            for (ci, contour) in shape.iter().enumerate() {
                if contour.len() < 3 {
                    continue;
                }
                if ci > 0 {
                    holes.push(flat_xy.len() / 2);
                }
                for p in contour {
                    flat_xy.push(f64::from(from_mm(p.x)));
                    flat_xy.push(f64::from(from_mm(p.y)));
                }
            }
            if flat_xy.len() < 6 {
                continue;
            }
            let Ok(tris) = earcutr::earcut(&flat_xy, &holes, 2) else {
                continue;
            };
            for c in tris.chunks_exact(3) {
                let mut out_p = [[0.0f32; 3]; 3];
                let mut out_uv = [[0.0f32; 2]; 3];
                let mut out_col = [[0.0f32; 4]; 3];
                for (k, idx) in c.iter().enumerate() {
                    let p = [flat_xy[idx * 2] as f32, flat_xy[idx * 2 + 1] as f32];
                    let w = barycentric(flat, p);
                    out_p[k] = [p[0], y, p[1]];
                    out_uv[k] = [
                        w[0] * uv[0][0] + w[1] * uv[1][0] + w[2] * uv[2][0],
                        w[0] * uv[0][1] + w[1] * uv[1][1] + w[2] * uv[2][1],
                    ];
                    out_col[k] = std::array::from_fn(|ch| {
                        w[0] * col[0][ch] + w[1] * col[1][ch] + w[2] * col[2][ch]
                    });
                }
                // Ground geometry is single-sided and faces up; earcut keeps its
                // input's orientation, which this one does not guarantee.
                let cross = (out_p[1][0] - out_p[0][0]) * (out_p[2][2] - out_p[0][2])
                    - (out_p[1][2] - out_p[0][2]) * (out_p[2][0] - out_p[0][0]);
                if cross.abs() < f32::EPSILON {
                    continue;
                }
                if cross < 0.0 {
                    emit(out_p, out_uv, out_col);
                } else {
                    emit(
                        [out_p[0], out_p[2], out_p[1]],
                        [out_uv[0], out_uv[2], out_uv[1]],
                        [out_col[0], out_col[2], out_col[1]],
                    );
                }
            }
        }
    }
    drop(emit);
    touched.then_some(Cut {
        positions,
        uvs,
        colors,
        indices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ribbon whose triangles are the two halves of an axis-aligned quad.
    fn bar(x0: f32, z0: f32, x1: f32, z1: f32, priority: u32) -> Ribbon {
        Ribbon {
            group: SurfaceGroup::Water,
            rung: 0.0,
            // One band, so the bare `priority` is the whole order — these tests
            // are about the cut itself, not about which sheet a ribbon is on.
            priority: (0, priority, 0, 0, 0),
            bridge: false,
            positions: vec![[x0, 0.0, z0], [x1, 0.0, z0], [x1, 0.0, z1], [x0, 0.0, z1]],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            colors: Vec::new(),
            indices: vec![0, 1, 2, 0, 2, 3],
        }
    }

    fn area(r: &Ribbon) -> f32 {
        r.indices
            .chunks_exact(3)
            .map(|t| {
                let a = r.positions[t[0] as usize];
                let b = r.positions[t[1] as usize];
                let c = r.positions[t[2] as usize];
                ((b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0])).abs() * 0.5
            })
            .sum()
    }

    /// The whole point: after the cut the two ribbons share no ground at all, and
    /// the loser gives up the crossing and not one square metre more.
    #[test]
    fn the_loser_gives_up_exactly_the_shared_ground() {
        // A 10 x 2 bar crossed by a 2 x 10 one that outranks it.
        let mut ribbons = vec![bar(0.0, 0.0, 10.0, 2.0, 1), bar(4.0, -4.0, 6.0, 6.0, 2)];
        let before = area(&ribbons[0]);
        stitch(&mut ribbons);
        let after = area(&ribbons[0]);
        assert!(
            (before - after - 4.0).abs() < 1e-3,
            "the 2 x 2 crossing should be the only thing given up: {before} -> {after}",
        );
        assert!(
            (area(&ribbons[1]) - 20.0).abs() < 1e-3,
            "the winner keeps all of itself",
        );
    }

    /// The survivors have to abut the winner along the exact line it claimed, or
    /// the seam is a crack.
    #[test]
    fn the_survivors_end_on_the_winners_edge() {
        let mut ribbons = vec![bar(0.0, 0.0, 10.0, 2.0, 1), bar(4.0, -4.0, 6.0, 6.0, 2)];
        stitch(&mut ribbons);
        let xs: Vec<f32> = ribbons[0].positions.iter().map(|p| p[0]).collect();
        assert!(
            xs.iter().any(|x| (*x - 4.0).abs() < 1e-6),
            "a survivor must land exactly on x = 4",
        );
        assert!(
            xs.iter().any(|x| (*x - 6.0).abs() < 1e-6),
            "a survivor must land exactly on x = 6",
        );
        assert!(
            !xs.iter().any(|x| *x > 4.0 + 1e-6 && *x < 6.0 - 1e-6),
            "nothing may survive inside the winner",
        );
    }

    /// Texture coordinates ride along barycentrically, so a cut edge carries the
    /// coordinate the original triangle had there — not one recomputed from the
    /// centre line, which is how the markings would drift at a junction.
    #[test]
    fn texture_coordinates_survive_the_cut() {
        let mut ribbons = vec![bar(0.0, 0.0, 10.0, 2.0, 1), bar(4.0, -4.0, 6.0, 6.0, 2)];
        stitch(&mut ribbons);
        for (p, uv) in ribbons[0].positions.iter().zip(&ribbons[0].uvs) {
            // `bar` lays u across x/10 and v across z/2, so this is exact.
            assert!(
                (uv[0] - p[0] / 10.0).abs() < 1e-3 && (uv[1] - p[2] / 2.0).abs() < 1e-3,
                "uv {uv:?} does not match position {p:?}",
            );
        }
    }

    /// A bridge is not cut by the road it flies over, nor the road by the bridge.
    #[test]
    fn a_flyover_and_the_road_beneath_it_both_keep_their_tarmac() {
        let mut ribbons = vec![bar(0.0, 0.0, 10.0, 2.0, 1), bar(4.0, -4.0, 6.0, 6.0, 2)];
        ribbons[1].bridge = true;
        stitch(&mut ribbons);
        assert!((area(&ribbons[0]) - 20.0).abs() < 1e-3, "the road is whole");
        assert!(
            (area(&ribbons[1]) - 20.0).abs() < 1e-3,
            "the bridge is whole"
        );
    }

    /// A ribbon laps over **itself** wherever its centre line doubles back, and
    /// that used to be left alone on the grounds that one ribbon is one material
    /// and one run of texture coordinates. It is not: `v` is metres travelled, so
    /// the returning leg carries different lane dashes from the outgoing one at
    /// the same depth, and which draws is a coin flip that re-rolls with the view
    /// matrix. The fold loses its second layer.
    ///
    /// The Groningen fixture's worked example is feature 69 — a `tertiary`
    /// turning head whose two legs are 5 m apart while the road is drawn 9 m
    /// wide, so the legs cover each other outright.
    #[test]
    fn a_ribbon_that_folds_back_keeps_one_layer() {
        let mut r = bar(0.0, 0.0, 10.0, 2.0, 1);
        // The same ribbon laid back over its own first half.
        let base = r.positions.len() as u32;
        r.positions.extend([
            [0.0, 0.0, 0.0],
            [5.0, 0.0, 0.0],
            [5.0, 0.0, 2.0],
            [0.0, 0.0, 2.0],
        ]);
        r.uvs
            .extend([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]);
        r.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        let mut ribbons = vec![r];
        assert!(
            (area(&ribbons[0]) - 30.0).abs() < 1e-3,
            "the fold is 10 m² of doubled ground before the cut",
        );
        stitch(&mut ribbons);
        assert!(
            (area(&ribbons[0]) - 20.0).abs() < 1e-3,
            "the ribbon should cover its 10 x 2 m of ground exactly once, not {}",
            area(&ribbons[0]),
        );
    }

    /// …and the ordinary strip is untouched by that. Every quad of a ribbon
    /// shares an edge with its neighbour, and a shared edge is not an overlap —
    /// if it were read as one, the cut would nibble a hairline out of every
    /// triangle in the tile.
    #[test]
    fn the_shared_edges_of_one_strip_are_not_a_fold() {
        let mut ribbons = vec![bar(0.0, 0.0, 10.0, 2.0, 1)];
        let before = ribbons[0].indices.clone();
        stitch(&mut ribbons);
        assert_eq!(
            ribbons[0].indices, before,
            "a strip's own shared edges must not cut it",
        );
    }

    /// Two ribbons that merely pass near each other cost nothing: no boolean runs,
    /// and the strip's own tessellation is handed straight through.
    #[test]
    fn a_ribbon_with_no_blocker_is_left_alone() {
        let mut ribbons = vec![bar(0.0, 0.0, 10.0, 2.0, 1), bar(0.0, 50.0, 10.0, 52.0, 2)];
        let before = ribbons[0].indices.clone();
        stitch(&mut ribbons);
        assert_eq!(ribbons[0].indices, before);
    }

    /// Cutting is decided against the *uncut* geometry of the superiors, so the
    /// answer cannot depend on which ribbon this happened to reach first. Three
    /// bars stacked in priority is the case that would expose a running union: B
    /// loses ground to C, and if A were then cut against B's *remains* it would
    /// keep the strip C had already claimed.
    #[test]
    fn the_result_does_not_depend_on_the_order_of_the_list() {
        let build = || {
            vec![
                bar(0.0, 0.0, 10.0, 2.0, 1),
                bar(3.0, -4.0, 5.0, 6.0, 2),
                bar(4.0, -4.0, 6.0, 6.0, 3),
            ]
        };
        let mut forward = build();
        stitch(&mut forward);
        let mut backward: Vec<Ribbon> = build().into_iter().rev().collect();
        stitch(&mut backward);
        for (f, b) in forward.iter().zip(backward.iter().rev()) {
            assert!(
                (area(f) - area(b)).abs() < 1e-3,
                "one ribbon kept {} laid forwards and {} laid backwards",
                area(f),
                area(b),
            );
        }
    }
}
