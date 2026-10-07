//! Street ribbons: which streets are drawn and how wide, the junction graph their ends are
//! joined across, corner filleting, and the ribbon and kerb-band meshes themselves.

use super::*;

/// How far a ribbon may be pushed past the end of its centre line, in metres, to
/// fill the notch at a junction.
///
/// Streets arrive as separate features that merely *share an endpoint*, so a road
/// tee-ing into another stops at that road's centre line and leaves a wedge of
/// bare ground either side of it. The stub that fills the wedge has to reach the
/// **far kerb of the road being joined**, which is that road's half-width — not
/// this one's. Extending by its own half-width (what this used to do) is right
/// only when the two are the same width; a motorway ending at a service road threw
/// an 11 m flap of tarmac across a 4 m street, and every road that simply *ends*
/// — a cul-de-sac, a track petering out — grew the same flap into open ground.
///
/// So the length now comes from [`junction_extent`], and this is only the cap that
/// keeps a pathological node (a footpath meeting a motorway) from painting a stub
/// longer than anything it could plausibly be joining.
const JUNCTION_EXTEND_MAX: f32 = 12.0;

/// The across-road `u` span a junction stub samples: a column of plain road
/// surface clear of every class's markings (the nearest marking edge is at
/// u = 0.037 on a motorway and the wheel tracks start at 0.215 on a minor
/// road). A stub carrying the full cross-section ruled its kerb lines and
/// centre dash across the road being joined; a stub carrying only surface
/// tone reads as the open tarmac a real junction is.
const STUB_PLAIN_U: (f32, f32) = (0.06, 0.16);

/// How far a [`SurfaceGroup::RoadCasing`] ribbon reaches past its fill on each
/// side, in metres — a kerb-and-verge band, not a second road.
const ROAD_CASING_M: f32 = 0.75;

/// Miter limit: how far a join may push the **outer** corner out relative to the
/// half-width before it is bevelled off instead. Unbounded miters spike to
/// infinity at a hairpin, which OSM has plenty of (motorway sliproads,
/// switchbacks).
///
/// Past this the join emits two outer vertices and a fill triangle rather than a
/// single stretched one — see [`ribbon`] on why clamping the miter *in place* is
/// not the same thing.
///
/// **2.5 was a stroke-join default, not a number about roads**, and it is too
/// generous by a wide margin: a join may reach `2.5 · half` from the centre line,
/// which is an 11 m spike on a 9 m street and a 27 m one on a motorway. The
/// Groningen fixture has a worked example — feature 69's first linestring is a
/// `tertiary` turning head that reverses through **128.2°**, whose miter of
/// `1 / cos(64.1°) = 2.29` slipped under the old limit and drew a ten-metre
/// tongue of tarmac out into the field past the road it serves.
///
/// 1.5 instead, for one reason that can be stated: a right angle mitres at
/// `1 / cos 45° = 1.414`, and a street grid's right angles are corners the map is
/// asserting (the same licence [`ROUND_MAX_DEG`] draws). So every corner up to
/// ~96° keeps its point and nothing in an ordinary city moves; past that the
/// point stops being a shape a road has, and a bevel — the chord between the two
/// square corners — is what a turning head actually looks like.
///
/// **It has to stay a ratio, not a distance.** [`kerb_band`] zips the carriageway
/// against a second, wider run of this same code and relies on every join
/// decision being independent of the width; a limit in metres would let the fill
/// and its kerb bevel at different corners, desynchronise the two chains and drop
/// the band to its slab fallback.
const MITER_LIMIT: f32 = 1.5;

/// Fraction of the shorter adjacent segment that the **inner** corner may
/// backtrack over. See [`ribbon`]: the inner miter runs back `half / tan(θ/2)`
/// along both segments, and past their far end the ribbon folds through itself.
///
/// Half, not all, because the segment's *other* end has a join of its own eating
/// into the same length — two joins each taking half meet exactly, and never
/// cross.
const INNER_BACKTRACK_BUDGET: f32 = 0.5;

/// A street's drawn class and half-width, or `None` if it is not drawn as ground
/// geometry at all.
///
/// **Tunnels are not drawn.** A tunnel is tarmac the camera cannot see: painting
/// it on the ground plane lays a road across whatever is genuinely up there —
/// which at an interchange is the junction the tunnel passes under, and in a city
/// is a park or a canal with a carriageway ruled through it. `road_class` already
/// refuses `subway` for the same reason; the `tunnel` flag is that rule applied to
/// the kinds that can be either.
pub(crate) fn drawn_street(feature: &Feature) -> Option<(RoadClass, f32)> {
    // A tunnel leaves the ground sheet because it is not on the ground — it is
    // drawn under it by `crate::tunnels`. An arcade through a building is
    // flagged a tunnel too and *is* on the ground, so it stays here; see
    // `tunnels::underground`.
    if crate::tunnels::underground(feature) {
        return None;
    }
    street_shape(feature)
}

/// A street's class and half-width, with no judgement about whether it is drawn.
///
/// The half of [`drawn_street`] that is about the road itself. Split out for the
/// one caller that has to overrule the tunnel refusal above — `tessellate_roads`,
/// which has the tile and so can tell an arcade from a tunnel.
pub(crate) fn street_shape(feature: &Feature) -> Option<(RoadClass, f32)> {
    let kind = kind_of(feature).unwrap_or("");
    let flags = StreetFlags {
        oneway: flag_of(feature, "oneway") || flag_of(feature, "oneway_reverse"),
        oneway_reverse: flag_of(feature, "oneway_reverse"),
        link: flag_of(feature, "link"),
    };
    Some((road_class(kind, flags)?, road_width_m(kind, flags) * 0.5))
}

/// The half-width [`drawn_street`] would answer for a way of this `kind` with no
/// flags set, `None` on a kind it refuses.
///
/// The kind-only half of the same rule, for a caller holding a class name rather
/// than a decoded feature — `WayField::push_line`, which synthesises ways in
/// tests. It shares the two functions above rather than restating a number, so a
/// width edit cannot land on one path and miss the other.
#[cfg(test)]
pub(crate) fn drawn_half_width(kind: &str) -> Option<f32> {
    let flags = StreetFlags::default();
    road_class(kind, flags).map(|_| road_width_m(kind, flags) * 0.5)
}

/// One road arriving at one node, as [`street_nodes`] records it.
#[derive(Clone, Copy)]
struct NodeArm {
    /// Which feature this is, so a road never treats itself as a junction.
    feature: usize,
    /// Half the carriageway width, in metres.
    half: f32,
    class: RoadClass,
    /// The road's direction *through* the node, sign-free — only the crossing
    /// angle is ever read from it.
    axis: [f32; 2],
    /// Set when the node is one of this line's own **ends**: the direction the
    /// road leaves the node in, and how long its first segment is, in metres.
    /// `None` for an interior vertex — a road that merely passes through cannot
    /// be the thing that *continues* another one.
    leaves: Option<([f32; 2], f32)>,
}

/// Every vertex of every drawn street in the tile, keyed on the integer lattice,
/// carrying the features that touch it and how wide each of them is.
///
/// Keyed on the raw tile units *rounded to an integer*, with no tolerance, because
/// none is needed: MVT quantises all of a tile's geometry onto one integer lattice,
/// so two ways sharing an OSM node share the exact coordinate (the same property
/// `routing::RoadGraph` leans on to stitch tiles without a fuzzy match). A tolerance
/// would only start fusing genuinely distinct nodes in a dense junction.
///
/// *Every* vertex goes in, not just endpoints: a side street tees into the middle
/// of a through road, so the node it shares is an interior vertex of that road.
/// Whether the node *is* an end is recorded ([`NodeArm::leaves`]) rather than
/// used to filter, because the two questions asked of this table want opposite
/// answers — [`junction_extent`] wants every road at the node, and
/// [`continuation`] wants only the ones that end there.
///
/// Keys are tile units; `scale` converts those to the metres the ribbon builder
/// works in.
/// How far the fillet at vertex `i` of a raw MVT linestring reaches back along
/// each of its segments, in **tile units**.
///
/// The scale is uniform, so [`corner_cut`]'s answer in tile units times the scale
/// is its answer in metres — which is what lets [`street_nodes`] report the same
/// number `ribbon` will measure without converting the whole line first.
fn fillet_units(line: &[geo_types::Coord<f32>], i: usize, half_units: f32) -> f32 {
    if i == 0 || i + 1 >= line.len() {
        return 0.0;
    }
    let (a, p, b) = (line[i - 1], line[i], line[i + 1]);
    let (l_in, l_out) = ((p.x - a.x).hypot(p.y - a.y), (b.x - p.x).hypot(b.y - p.y));
    if l_in < 1e-6 || l_out < 1e-6 {
        return 0.0;
    }
    let cos =
        (((p.x - a.x) * (b.x - p.x) + (p.y - a.y) * (b.y - p.y)) / (l_in * l_out)).clamp(-1.0, 1.0);
    corner_cut(l_in, l_out, cos.acos(), half_units)
}

fn street_nodes(features: &[Feature], scale: f32) -> HashMap<(i32, i32), Vec<NodeArm>> {
    let mut nodes: HashMap<(i32, i32), Vec<NodeArm>> = HashMap::new();
    for (fi, feature) in features.iter().enumerate() {
        let Some((class, half)) = drawn_street(feature) else {
            continue;
        };
        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            _ => continue,
        }
        for ls in &lines {
            let n = ls.0.len();
            for (j, c) in ls.0.iter().enumerate() {
                if !c.x.is_finite() || !c.y.is_finite() {
                    continue;
                }
                // The road's direction through this node (sign-free — only the
                // crossing *angle* is ever read), from whichever adjacent
                // segment exists.
                let (ax, ay) = if j + 1 < n {
                    (ls.0[j + 1].x - c.x, ls.0[j + 1].y - c.y)
                } else if j > 0 {
                    (c.x - ls.0[j - 1].x, c.y - ls.0[j - 1].y)
                } else {
                    (0.0, 0.0)
                };
                let len = ax.hypot(ay);
                let d = if len > 1e-6 && len.is_finite() {
                    [ax / len, ay / len]
                } else {
                    [0.0, 0.0]
                };
                // At an end, `axis` already points from the node into the line
                // (`j + 1 < n` at the head, `j > 0` reversed at the tail), so
                // the leaving direction is the axis or its negation.
                //
                // The length is the one **`ribbon` will actually measure**, i.e.
                // after the fillet at the next vertex along has eaten its way
                // back into this segment — see [`corner_cut`]. Reporting the raw
                // length here and the cut one there is how the two halves of a
                // continuation would come to clamp their shared inner corner
                // against different numbers, which opens the crack the mitre
                // exists to close.
                let leaves = match (j, len > 1e-6 && len.is_finite()) {
                    (0, true) => Some((d, (len - fillet_units(&ls.0, 1, half / scale)) * scale)),
                    (j, true) if j + 1 == n => Some((
                        [-d[0], -d[1]],
                        (len - fillet_units(&ls.0, n - 2, half / scale)) * scale,
                    )),
                    _ => None,
                };
                let at = nodes.entry(node_key([c.x, c.y])).or_default();
                // One entry per feature: a way that doubles back through its own
                // node is not a junction with itself.
                if !at.iter().any(|a| a.feature == fi) {
                    at.push(NodeArm {
                        feature: fi,
                        half,
                        class,
                        axis: d,
                        leaves,
                    });
                }
            }
        }
    }
    nodes
}

fn node_key(p: [f32; 2]) -> (i32, i32) {
    (p[0].round() as i32, p[1].round() as i32)
}

/// Below this crossing sine (≈15°) the road at the far side of the node is not
/// **crossing** this one, it is *continuing* it, and a continuation needs no
/// stub at all: the two ribbons already abut exactly at the shared node.
///
/// This is the common case, not an edge one. Shortbread merges ways by
/// attribute, so an ordinary street arrives as a chain of features that meet
/// end to end — measured on the Groningen fixture, **58 % of all junction ends
/// are collinear joins like that** (418 of 722). They used to be the *worst*
/// case rather than a free one: the old form of this floored the sine at 0.35
/// and divided by it, so two collinear features each fired the longest stub the
/// cap allowed — 10–12 m of [`STUB_PLAIN_U`] tarmac laid over its neighbour,
/// from both sides. What that looks like on the ground is a street whose kerb
/// lines and centre dashes stop dead for twenty metres every time the name
/// changes, over a doubled ribbon that fights in depth. Flooring the sine
/// bounded the spear; it did not stop the code aiming one down every street.
const MERGE_SIN: f32 = 0.26;

/// The sine at and above which the stub is the honest `half / sin` (≈30°, so at
/// most `2 × half`). Between here and [`MERGE_SIN`] the stub ramps to zero, so
/// a shallow join loses its stub gradually rather than at a threshold that
/// would show as one street in a row of similar ones being drawn differently.
const JUNCTION_SIN: f32 = 0.5;

/// How far past `end` the feature's ribbon has to run to fill the junction
/// there, in metres: for each **other** road meeting at that node, its
/// half-width over the sine of the crossing angle — the distance to its far
/// kerb along *this* road's own direction — and the largest of those wins.
/// Plain `half` (what this used to return) is only right at 90°: a road
/// crossed at 30° left a notch on the acute side while overshooting on the
/// obtuse one.
///
/// Two ends get nothing. A **dead end** keeps a square end on the ground
/// instead of a flap of tarmac reaching into the field beyond it (nothing else
/// meets there, so the fold starts and finishes at `0.0`), and a **collinear
/// continuation** keeps its markings running through — see [`MERGE_SIN`].
fn junction_extent(
    nodes: &HashMap<(i32, i32), Vec<NodeArm>>,
    end: [f32; 2],
    feature_index: usize,
    out_dir: [f32; 2],
) -> f32 {
    // Bisecting switch: `CARTO_NO_STUB=1` lays every ribbon with a square end, so
    // a shape at a junction can be told apart from the stub that fills it. Read
    // once — this runs at every junction end, and the perf tests time it.
    #[cfg(test)]
    {
        static NO_STUB: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *NO_STUB.get_or_init(|| std::env::var("CARTO_NO_STUB").is_ok()) {
            return 0.0;
        }
    }
    nodes
        .get(&node_key(end))
        .map(|at| {
            at.iter()
                .filter(|a| a.feature != feature_index)
                .map(|a| {
                    let sin = (out_dir[0] * a.axis[1] - out_dir[1] * a.axis[0]).abs();
                    // The far-kerb distance, and how much of a *junction* this
                    // crossing angle actually is.
                    let reach = a.half / sin.max(JUNCTION_SIN);
                    let crossing = ((sin - MERGE_SIN) / (JUNCTION_SIN - MERGE_SIN)).clamp(0.0, 1.0);
                    reach * crossing
                })
                .fold(0.0, f32::max)
        })
        .unwrap_or(0.0)
        .min(JUNCTION_EXTEND_MAX)
}

/// The smallest cosine — i.e. the widest bend — at which two features that end
/// at the same node still count as one street continuing through it. 40° is
/// well past what a road bends at a single OSM vertex and comfortably short of
/// the 90° that would let a genuine side road be mistaken for a continuation.
const CONTINUATION_COS: f32 = 0.77;

/// The road that **continues** this one past `end`, if there is one: the
/// direction it leaves the node in, and how long its first segment is.
///
/// Shortbread merges ways by attribute, so one street reaches the tessellator as
/// a chain of features that meet end to end — 272 such joins in the Groningen
/// fixture, 55 of them turning by 10° or more. Each half used to stop dead at
/// the shared node with a square butt cap, and two square caps meeting at an
/// angle leave a wedge of bare ground on the outside of the bend and a doubled
/// overlap on the inside: measured up to 1.28 m across on a 7 m street, which is
/// most of what "the roads don't connect" looks like from the pavement. (It went
/// unnoticed for as long as it did because the old stub buried the seam under
/// ten metres of blank tarmac, which was worse.)
///
/// Mitring closes it exactly rather than approximately, and that is the point of
/// returning the neighbour's *segment length* alongside its direction: both
/// halves then feed [`ribbon`] the identical `(d_in, d_out)` pair and the
/// identical inner-backtrack budget, so both place their two end vertices on the
/// same two points. The join is watertight by construction, not by tolerance.
///
/// Only a road that **ends** here can continue this one (a road passing through
/// is being crossed, not joined), only one of the same class and width can
/// (grafting a 4 m service road onto a 22 m motorway is not a continuation), and
/// only one leaving in roughly the direction this one is pointing.
fn continuation(
    nodes: &HashMap<(i32, i32), Vec<NodeArm>>,
    end: [f32; 2],
    feature_index: usize,
    out_dir: [f32; 2],
    class: RoadClass,
    half: f32,
) -> Option<([f32; 2], f32)> {
    let mut best: Option<(f32, [f32; 2], f32)> = None;
    for arm in nodes.get(&node_key(end))? {
        if arm.feature == feature_index || arm.class != class {
            continue;
        }
        if (arm.half - half).abs() > 0.15 * half {
            continue;
        }
        let Some((leaves, seg_m)) = arm.leaves else {
            continue;
        };
        let cos = leaves[0] * out_dir[0] + leaves[1] * out_dir[1];
        if cos < CONTINUATION_COS {
            continue;
        }
        if best.is_none_or(|(c, _, _)| cos > c) {
            best = Some((cos, leaves, seg_m));
        }
    }
    best.map(|(_, leaves, seg_m)| (leaves, seg_m))
}

/// What a ribbon meets at one of its two ends. The two cases are exclusive by
/// construction — a street that continues through a node has no notch there for
/// a stub to fill, so [`tessellate_roads`] never asks for both.
#[derive(Clone, Copy, Default)]
struct EndJoin {
    /// Metres of junction stub past the end, from [`junction_extent`]: how far
    /// this road has to run *into* whatever it meets there. Zero at a dead end,
    /// at a tile border and wherever `cont` is set.
    extend: f32,
    /// The feature that continues this road past the end, from
    /// [`continuation`]: the direction it leaves in and its first segment's
    /// length. The end is mitred into it instead of butt-capped.
    cont: Option<([f32; 2], f32)>,
}

/// Below this turn there is no corner worth rounding: the kink is finer than the
/// ribbon's own kerb line is wide and rounding it would only add vertices.
const ROUND_MIN_DEG: f32 = 2.0;

/// Above this turn the corner is **real** and is kept. A street grid's right
/// angles, a hairpin and a switchback are all things the map is asserting; a
/// fillet there would cut the corner off a road that genuinely turns there, and
/// at a junction it would pull the carriageway away from the node it meets.
const ROUND_MAX_DEG: f32 = 60.0;

/// How far a fillet may pull the carriageway away from the vertex the map
/// recorded, in metres.
///
/// The whole licence this rounding takes is that a generalised vertex is an
/// artefact of drawing the map small, not a statement that the road has a kink
/// there. That licence runs out somewhere, and this is where: a metre is inside
/// the width of the lane the vertex sits in, so no fillet can move a road onto
/// ground the map says is something else.
const ROUND_MAX_OFFSET_M: f32 = 1.0;

/// How far back along each adjacent segment a fillet may reach, as a fraction of
/// the **shorter** of the two.
///
/// Well under the `INNER_BACKTRACK_BUDGET` the miter join is allowed, so a
/// rounded corner can never eat the segment its neighbour's join is measured
/// against.
const ROUND_TANGENT_FRAC: f32 = 0.35;

/// The largest turn one segment of a fillet may carry. The sagitta it leaves is
/// `r · (1 − cos(step/2))` — four centimetres on a five-metre radius at this
/// step, i.e. under the width of the painted line that runs along the edge.
const ROUND_STEP_DEG: f32 = 8.0;

/// How far back along its two segments the fillet at a corner reaches, in metres,
/// or `0` where the corner is kept.
///
/// Shared with [`street_nodes`], and that is not tidiness — it is the whole
/// reason the mitred continuation joins stay watertight. `ribbon` clamps the
/// inner corner against the *shorter* of the two segments meeting there, and at
/// a continuation one of those two comes over from the neighbouring feature as
/// `NodeArm::leaves`. If this rounding shortened the segment `ribbon` measures
/// while `NodeArm` still reported the raw one, the two halves of one street
/// would clamp against different numbers and land on different points — the
/// hairline crack `two_halves_of_one_street_meet_on_the_same_two_points` exists
/// to forbid. So `street_nodes` reports the length *after* this cut, computed by
/// this same function.
/// `half` is the carriageway's half-width, and it is what decides whether the
/// corner may be rounded at all — see the fold rule below.
fn corner_cut(prev_len: f32, next_len: f32, turn_rad: f32, half: f32) -> f32 {
    let deg = turn_rad.to_degrees();
    if !(ROUND_MIN_DEG..=ROUND_MAX_DEG).contains(&deg) {
        return 0.0;
    }
    let tan_half = (turn_rad * 0.5).tan();
    if tan_half <= 1e-6 {
        return 0.0;
    }
    // As far back as the two segments can spare…
    let budget = ROUND_TANGENT_FRAC * prev_len.min(next_len);
    // …but never so far that the carriageway leaves the vertex the map recorded
    // by more than [`ROUND_MAX_OFFSET_M`]. A fillet's departure from its corner
    // is `r · (sec(θ/2) − 1)`, and `t = r · tan(θ/2)`, so the tangent that lands
    // exactly on the allowance is this.
    let sec_minus_one = 1.0 / (turn_rad * 0.5).cos() - 1.0;
    let by_offset = if sec_minus_one > 1e-9 {
        ROUND_MAX_OFFSET_M / sec_minus_one * tan_half
    } else {
        f32::MAX
    };
    let t = budget.min(by_offset);
    // **A fillet tighter than the road is wide folds the inner edge through
    // itself.** The ribbon is offset `half` either side of the centre line, so
    // on the inside of an arc of radius `r` the edge has radius `r − half`: at
    // `r < half` that is negative and the strip turns inside out — the doubled,
    // self-intersecting triangles `a_tight_corner_does_not_fold_the_ribbon`
    // exists to forbid. `r = t / tan(θ/2)`, so the corner is roundable only
    // while `t ≥ half · tan(θ/2)`, and where it is not the kink is kept. That is
    // what makes this safe on a 22 m motorway and generous on an alley, with one
    // rule rather than a width table.
    if t < half * tan_half {
        return 0.0;
    }
    t
}

/// Replace each moderate kink in a centre line with a circular fillet.
///
/// **A road is not a polygon, and at street level this is what says so.**
/// Shortbread generalises its geometry for a map read from above, where a 20°
/// kink in a street is a pixel; extruded into a ribbon and seen from the
/// pavement it is a visible corner in the kerb and in the painted edge line, and
/// the whole street reads as a chain of straight boards. Measured on the
/// Groningen z14 fixture, **79 % of interior vertices turn by 2–70° and 49 % by
/// ten degrees or more** — this is the ordinary case, not an edge one.
///
/// Three properties are load-bearing:
///
/// * **The endpoints never move.** A road's ends are its junction nodes, and
///   every join in this module — the mitred continuation, the junction stub, the
///   half-open tile ownership — is anchored on them.
/// * **The end *directions* never change either.** A fillet at the second vertex
///   pulls that vertex back *along* the first segment, so the first segment gets
///   shorter and keeps its bearing. `junction_extent` and `continuation` read
///   the bearing and are untouched.
/// * **A sharp corner is kept** ([`ROUND_MAX_DEG`]). Rounding a right angle
///   would move the carriageway off a corner the map is asserting is there.
fn round_corners(pts: &[[f32; 2]], half: f32) -> Vec<[f32; 2]> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let len_of = |a: [f32; 2], b: [f32; 2]| (b[0] - a[0]).hypot(b[1] - a[1]);
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(pts.len() * 2);
    out.push(pts[0]);
    for i in 1..pts.len() - 1 {
        let (p, a, b) = (pts[i], pts[i - 1], pts[i + 1]);
        let (l_in, l_out) = (len_of(a, p), len_of(p, b));
        if l_in < 1e-4 || l_out < 1e-4 {
            continue;
        }
        let d_in = [(p[0] - a[0]) / l_in, (p[1] - a[1]) / l_in];
        let d_out = [(b[0] - p[0]) / l_out, (b[1] - p[1]) / l_out];
        let cos = (d_in[0] * d_out[0] + d_in[1] * d_out[1]).clamp(-1.0, 1.0);
        let turn = cos.acos();
        let t = corner_cut(l_in, l_out, turn, half);
        if t <= 0.0 {
            out.push(p);
            continue;
        }
        // The fillet runs from `t` back along the incoming segment to `t` along
        // the outgoing one. Sampled as a quadratic Bézier through the corner,
        // which is tangent to both segments at exactly those two points and is
        // within a fraction of a millimetre of the true arc over this angle —
        // and needs no centre, no radius and no sign of the turn.
        let start = [p[0] - d_in[0] * t, p[1] - d_in[1] * t];
        let end = [p[0] + d_out[0] * t, p[1] + d_out[1] * t];
        let steps = (turn.to_degrees() / ROUND_STEP_DEG).ceil().max(1.0) as usize;
        for k in 0..=steps {
            let s = k as f32 / steps as f32;
            let w = 1.0 - s;
            out.push([
                w * w * start[0] + 2.0 * w * s * p[0] + s * s * end[0],
                w * w * start[1] + 2.0 * w * s * p[1] + s * s * end[1],
            ]);
        }
    }
    out.push(pts[pts.len() - 1]);
    // Consecutive duplicates would produce a NaN normal downstream; a fillet
    // whose tangent point lands on its neighbour's is the case that can make one.
    out.dedup_by(|a, b| len_of(*a, *b) <= 1e-4);
    out
}

/// Extrude a centre line into a flat ribbon of `2 × half` metres at height `y`,
/// writing positions, UVs and indices into shared buffers.
///
/// Joins are mitered from the average of the two adjacent segment normals, which
/// keeps the carriageway a constant width around a bend — the naive
/// quad-per-segment alternative leaves a wedge of gap on the outside of every
/// corner, and OSM road geometry is nothing but corners.
///
/// **The two sides of a corner are not symmetric**, and treating them as one
/// number is what made junctions look bad. A single `ext` along the bisector,
/// clamped by [`MITER_LIMIT`], fails at both ends of the range:
///
/// * **Outside.** Clamping pulls the *outer* edge in along with the inner one, so
///   a sharp bend necks the carriageway down instead of just blunting the corner:
///   a 22 m motorway measured 14.2 m through a 150° turn and 4.8 m through a 170°
///   hairpin. The outer corner therefore keeps the full half-width and is
///   **bevelled** past the limit — two vertices, one square on each segment, with
///   a fill triangle across the gap. That cannot spike (the same reasoning
///   `systems::navigation::ribbon` documents for the route line) and cannot pinch.
/// * **Inside.** The inner miter backtracks `half / tan(θ/2)` along *both*
///   adjacent segments, and nothing bounded it against how long they actually
///   are. On a 22 m motorway a 60° turn overruns a 6 m segment — and z14
///   Shortbread geometry is full of sub-10 m segments — so the ribbon folded
///   through itself: doubled triangles whose two layers carry different `v`, which
///   is the centre line fighting its own reflection along the bend. The inner
///   extension is capped at [`INNER_BACKTRACK_BUDGET`] of the shorter neighbour.
///   A capped inner corner is slightly narrow on the inside of a turn tighter than
///   the road is wide; a fold is a rendering artefact.
///
/// `y` is the height of the ribbon's plane, and for a road it is **always zero**:
/// the ladder that keeps two overlapping ribbons from flickering against each
/// other at a junction is a depth offset carried per vertex ([`road_rung`]), not
/// a lift. The parameter survives because the tests lay ribbons at other heights.
///
/// `ends` is what the ribbon meets at its head and its tail — see [`EndJoin`].
///
/// `ring` says the centre line closes on itself — a roundabout, a turning
/// circle, a loop road. Its two "ends" are the same point and are not ends at
/// all, so each takes its miter from the segment on the other side of the join
/// instead of a butt cap. Without that the ring meets itself at an angle with
/// two square ends: a wedge of bare ground bitten out of the outer kerb and a
/// doubled overlap on the inner one, which is what put a notch in every
/// roundabout. Position and width match on both sides of the seam, so the
/// closure is watertight; only `v` wraps, and one texture discontinuity
/// somewhere on a loop is unavoidable unless its length happens to be a whole
/// number of texture repeats.
fn ribbon(
    points: &[[f32; 2]],
    half: f32,
    y: f32,
    tex_len_m: f32,
    ends: [EndJoin; 2],
    ring: bool,
    positions: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
) -> RibbonEdges {
    let extend = [ends[0].extend, ends[1].extend];
    // Consecutive duplicates have no direction and would produce NaN normals.
    let mut pts: Vec<[f32; 2]> = Vec::with_capacity(points.len());
    for p in points {
        if pts
            .last()
            .is_none_or(|q: &[f32; 2]| (p[0] - q[0]).hypot(p[1] - q[1]) > 1e-4)
        {
            pts.push(*p);
        }
    }
    if pts.len() < 2 || half <= 0.0 {
        return RibbonEdges::default();
    }
    // A ring needs three distinct points plus the repeat to have an interior;
    // and the caller's flag is only a hint until the geometry agrees with it
    // (a clipped run of a closed line is an ordinary open polyline).
    let ring = ring
        && pts.len() >= 4
        && (pts[0][0] - pts[pts.len() - 1][0]).hypot(pts[0][1] - pts[pts.len() - 1][1]) <= 1e-4;
    // A generalised centre line is a chain of straight segments, and the ribbon
    // built on it inherits every kink as a corner in the kerb and in the painted
    // edge line. Round them first — see `round_corners`. Both of `kerb_band`'s
    // runs come through here, so a carriageway and its kerb are rounded from the
    // same points and cannot drift apart.
    //
    // **Not a ring.** A roundabout's polygon is not a generalisation artefact
    // the way a kink in a street is: its vertices lie *on* the circle the
    // junction actually is, so cutting its corners would shrink the roundabout
    // rather than smooth it. Its seam is already mitred closed.
    if !ring {
        pts = round_corners(&pts, half);
        if pts.len() < 2 {
            return RibbonEdges::default();
        }
    }
    let last = pts.len() - 1;

    let dir = |a: [f32; 2], b: [f32; 2]| -> [f32; 2] {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy).max(1e-6);
        [dx / len, dy / len]
    };

    let seg_len =
        |i: usize| -> f32 { (pts[i + 1][0] - pts[i][0]).hypot(pts[i + 1][1] - pts[i][1]) };
    // The `(u = 0, u = 1)` vertex pair the *incoming* quad closes on and the pair
    // the *outgoing* quad opens from, per point. A miter join shares one pair; a
    // bevel splits the outer vertex in two and keeps the inner one shared, so the
    // strip can no longer be indexed as `base + i * 2`.
    let mut joins: Vec<((u32, u32), (u32, u32))> = Vec::with_capacity(pts.len());
    let mut travelled = 0.0f32;

    for i in 0..pts.len() {
        if i > 0 {
            travelled += (pts[i][0] - pts[i - 1][0]).hypot(pts[i][1] - pts[i - 1][1]);
        }
        // On a ring the first point's incoming segment is the *last* one and the
        // last point's outgoing segment is the first, so both ends miter into
        // the join rather than butt-ending against it. Failing that, an end with
        // a continuation borrows the neighbouring feature's direction: `leaves`
        // points *away* from the node into that road, so the traffic arriving
        // along it runs the other way.
        let d_in = if i > 0 {
            dir(pts[i - 1], pts[i])
        } else if ring {
            dir(pts[last - 1], pts[last])
        } else if let Some((leaves, _)) = ends[0].cont {
            [-leaves[0], -leaves[1]]
        } else {
            dir(pts[0], pts[1])
        };
        let d_out = if i + 1 < pts.len() {
            dir(pts[i], pts[i + 1])
        } else if ring {
            dir(pts[0], pts[1])
        } else if let Some((leaves, _)) = ends[1].cont {
            leaves
        } else {
            d_in
        };
        // Left normal of a direction in tile space (x east, y south).
        let n_in = [-d_in[1], d_in[0]];
        let n_out = [-d_out[1], d_out[0]];
        let mut m = [n_in[0] + n_out[0], n_in[1] + n_out[1]];
        let m_len = m[0].hypot(m[1]);
        // A perfect reversal (a spike in the source geometry) cancels the two
        // normals; fall back to the incoming one rather than dividing by zero.
        if m_len < 1e-4 {
            m = n_in;
        } else {
            m = [m[0] / m_len, m[1] / m_len];
        }
        // Lengthen the miter so the *edge* stays `half` from the centre line
        // through the join.
        let cos_half = (m[0] * n_out[0] + m[1] * n_out[1]).abs().max(1e-3);
        let miter = 1.0 / cos_half;

        // Which side of the bisector the corner opens away from. `m` points into
        // the inside of the turn when the cross product is positive, so the outer
        // side is the other one; a straight point has no inside and takes neither
        // branch below.
        let cross = d_in[0] * d_out[1] - d_in[1] * d_out[0];
        let inner_s = if cross > 0.0 { 1.0f32 } else { -1.0f32 };
        let outer_s = -inner_s;

        // Outside: full half-width always, bevelled rather than stretched.
        let ext_outer = half * miter.min(MITER_LIMIT);
        // Inside: the same miter, but never further back than the segments it
        // eats into are long.
        let back_per_m = (m[0] * d_in[0] + m[1] * d_in[1]).abs();
        // The neighbour's segment length comes over with its direction, so both
        // halves of a continuation clamp the inner corner against the same pair
        // of lengths and land on the same vertex.
        let prev_len = if i > 0 {
            Some(seg_len(i - 1))
        } else if ring {
            Some(seg_len(last - 1))
        } else {
            ends[0].cont.map(|(_, seg_m)| seg_m)
        };
        let next_len = if i + 1 < pts.len() {
            Some(seg_len(i))
        } else if ring {
            Some(seg_len(0))
        } else {
            ends[1].cont.map(|(_, seg_m)| seg_m)
        };
        let neighbour = match (prev_len, next_len) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => 0.0,
        };
        let mut ext_inner = half * miter;
        let budget = INNER_BACKTRACK_BUDGET * neighbour;
        if back_per_m * ext_inner > budget {
            ext_inner = budget / back_per_m.max(1e-3);
        }

        let v = travelled / tex_len_m;
        // `u = 0` is the `-m` side and `u = 1` the `+m` side — the across-road
        // axis the centre line registers against (see `road_texture`).
        let (u_outer, u_inner) = if outer_s > 0.0 {
            (1.0, 0.0)
        } else {
            (0.0, 1.0)
        };
        let bevelled = miter > MITER_LIMIT;

        let inner_p = [
            pts[i][0] + inner_s * m[0] * ext_inner,
            pts[i][1] + inner_s * m[1] * ext_inner,
        ];
        // One outer vertex for a miter, two square ones for a bevel.
        let (outer_in_p, outer_out_p) = if bevelled {
            (
                [
                    pts[i][0] + outer_s * n_in[0] * half,
                    pts[i][1] + outer_s * n_in[1] * half,
                ],
                [
                    pts[i][0] + outer_s * n_out[0] * half,
                    pts[i][1] + outer_s * n_out[1] * half,
                ],
            )
        } else {
            let p = [
                pts[i][0] + outer_s * m[0] * ext_outer,
                pts[i][1] + outer_s * m[1] * ext_outer,
            ];
            (p, p)
        };

        let inner = positions.len() as u32;
        positions.push([inner_p[0], y, inner_p[1]]);
        uvs.push([u_inner, v]);
        let outer_in = positions.len() as u32;
        positions.push([outer_in_p[0], y, outer_in_p[1]]);
        uvs.push([u_outer, v]);
        let outer_out = if bevelled {
            let idx = positions.len() as u32;
            positions.push([outer_out_p[0], y, outer_out_p[1]]);
            uvs.push([u_outer, v]);
            idx
        } else {
            outer_in
        };

        // Order a pair as (u = 0 vertex, u = 1 vertex) so the strip stitches the
        // same way whichever direction the road turns.
        let pair = |outer: u32| {
            if outer_s > 0.0 {
                (inner, outer)
            } else {
                (outer, inner)
            }
        };
        joins.push((pair(outer_in), pair(outer_out)));

        if bevelled {
            // The wedge between the two square ends, filled back to the shared
            // inner vertex. This is the bevel.
            push_triangle_up(positions, indices, outer_in, outer_out, inner);
        }
        if i > 0 {
            let ((a0, a1), (b0, b1)) = (joins[i - 1].1, joins[i].0);
            push_triangle_up(positions, indices, a0, b0, b1);
            push_triangle_up(positions, indices, a0, b1, a1);
        }
    }

    // The area this ribbon covers, read straight off the strip it just laid.
    //
    // `joins` holds, per centre-line point, the `(u = 0, u = 1)` vertex pair the
    // incoming quad closes on and the pair the outgoing one opens from — so the
    // two sides of the ribbon *are* the two chains through those pairs, and the
    // boundary is one walked forward and the other walked back. Reading it out of
    // the strip rather than recomputing the offsets is the point: `road_stitch`
    // cuts against exactly the triangles that were laid, never against a second
    // opinion about where they are.
    let n = joins.len();
    // On a ring the last join is the first one again; keeping it would close the
    // contour on a doubled vertex.
    let span = if ring { n.saturating_sub(1) } else { n };
    let mut side_a: Vec<[f32; 2]> = Vec::with_capacity(span * 2);
    let mut side_b: Vec<[f32; 2]> = Vec::with_capacity(span * 2);
    fn push_flat(chain: &mut Vec<[f32; 2]>, positions: &[[f32; 3]], idx: u32) {
        let p = positions[idx as usize];
        let q = [p[0], p[2]];
        if chain
            .last()
            .is_none_or(|l: &[f32; 2]| (l[0] - q[0]).hypot(l[1] - q[1]) > 1e-6)
        {
            chain.push(q);
        }
    }
    for j in joins.iter().take(span) {
        push_flat(&mut side_a, positions, j.0 .0);
        push_flat(&mut side_a, positions, j.1 .0);
        push_flat(&mut side_b, positions, j.0 .1);
        push_flat(&mut side_b, positions, j.1 .1);
    }
    let sides = [side_a.clone(), side_b.clone()];
    let mut contours: Vec<Vec<[f32; 2]>> = Vec::new();
    if ring {
        // A closed ribbon is an annulus, so its two sides are two separate
        // contours — the outer boundary and the hole in the middle of the
        // roundabout — not one loop walked out and back.
        contours.push(side_a);
        contours.push(side_b);
    } else {
        side_b.reverse();
        side_a.extend(side_b);
        contours.push(side_a);
    }

    // Junction stubs, as standalone quads rather than extra strip points, for
    // two reasons. They sample [`STUB_PLAIN_U`] — a marking-free column of the
    // class texture — so the kerb lines and centre dashes *break* at the
    // junction instead of being ruled straight across the road being joined,
    // ending in a hard white T at its far kerb (the single most wrong-looking
    // thing about the old junctions). And a standalone quad reuses the strip's
    // own butt-end coordinates (the ends are unmitred: `d_out == d_in` there),
    // so the seam is watertight by construction.
    let total = travelled;
    let mut stub_contours: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut stub = |end: [f32; 2], d: [f32; 2], len: f32, v0: f32| {
        let n = [-d[1], d[0]];
        let far = [end[0] + d[0] * len, end[1] + d[1] * len];
        let v1 = v0 + len / tex_len_m;
        let base = positions.len() as u32;
        let corners = [
            (
                [end[0] + n[0] * half, end[1] + n[1] * half],
                STUB_PLAIN_U.0,
                v0,
            ),
            (
                [end[0] - n[0] * half, end[1] - n[1] * half],
                STUB_PLAIN_U.1,
                v0,
            ),
            (
                [far[0] - n[0] * half, far[1] - n[1] * half],
                STUB_PLAIN_U.1,
                v1,
            ),
            (
                [far[0] + n[0] * half, far[1] + n[1] * half],
                STUB_PLAIN_U.0,
                v1,
            ),
        ];
        for (p, u, v) in corners {
            positions.push([p[0], y, p[1]]);
            uvs.push([u, v]);
        }
        stub_contours.push(corners.iter().map(|(p, _, _)| *p).collect());
        push_triangle_up(positions, indices, base, base + 1, base + 2);
        push_triangle_up(positions, indices, base, base + 2, base + 3);
    };
    if extend[0] > 0.0 {
        stub(pts[0], dir(pts[1], pts[0]), extend[0], 0.0);
    }
    if extend[1] > 0.0 {
        stub(
            pts[last],
            dir(pts[last - 1], pts[last]),
            extend[1],
            total / tex_len_m,
        );
    }
    drop(stub);
    contours.extend(stub_contours);
    // One winding for every solid contour and the other for a hole, because the
    // boolean below reads them under a non-zero fill rule and a contour wound the
    // wrong way there subtracts itself from its own neighbours.
    let outer = contours
        .iter()
        .map(|c| ring_area(c).abs())
        .fold(0.0f32, f32::max);
    for c in &mut contours {
        let a = ring_area(c);
        // The roundabout's hole is the one contour that must run the other way;
        // it is the small one, and `ring` is the only thing that produces a pair.
        let want_positive = !ring || ring_area(c).abs() >= outer - f32::EPSILON;
        if (a > 0.0) != want_positive {
            c.reverse();
        }
    }
    contours.retain(|c| c.len() >= 3);
    RibbonEdges { contours, sides }
}

/// The boundary of what a ribbon covers, as [`ribbon`] laid it.
///
/// Both fields exist for [`kerb_band`], which is the only caller that reads them:
/// `sides` are the two offset chains, one per side of the centre line and in
/// centre-line order, which it zips against a second, wider pair to make the kerb
/// a band rather than a slab; `contours` is the closed form, whose entries past
/// the strip itself are the junction stub quads.
///
/// **`road_stitch` does not use either.** It cuts the ribbons' *triangles* — the
/// geometry about to be drawn — rather than a separate description of where that
/// geometry is, which is one fewer thing that can disagree.
#[derive(Default)]
struct RibbonEdges {
    contours: Vec<Vec<[f32; 2]>>,
    sides: [Vec<[f32; 2]>; 2],
}

/// Lay a road's kerb band: the strip of [`ROAD_CASING_M`] outside each edge of
/// the carriageway, and **not** the carriageway itself.
///
/// The kerb used to be a second ribbon the full width of road-plus-kerb, drawn
/// underneath the tarmac. That is a slab, and it made every carriageway pixel in
/// the tile a place where two opaque coplanar surfaces met — 70 % of the road
/// sheet, and by a long way the largest overlap in the scene. `road_stitch` will
/// cut it back to the two side strips whatever this does, but cutting a shape you
/// did not have to draw is work: laying the slab and subtracting it again was 60 %
/// of the whole cut's cost on the Groningen fixture.
///
/// The band is zipped between two runs of [`ribbon`] rather than offset by hand,
/// and that is what makes its inner edge *exactly* the carriageway's outer edge:
/// the same join code, the same miter, the same bevel decision and the same inner
/// backtrack clamp produce both, so the two meet on shared coordinates and the
/// seam cannot open. If the two runs ever disagree about how many points a side
/// has — they cannot today, since every one of those decisions is independent of
/// the width — this falls back to the slab, which `road_stitch` then cuts as
/// before. Slower, never wrong.
///
/// Kerb geometry carries no texture: `RoadCasing`'s material is flat-coloured
/// (see `road_base_material`), so the texture coordinates here only have to
/// exist.
#[allow(clippy::too_many_arguments)]
fn kerb_band(
    points: &[[f32; 2]],
    half: f32,
    tex_len_m: f32,
    ends: [EndJoin; 2],
    kerb_ends: [EndJoin; 2],
    ring: bool,
    positions: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
) {
    let mut sink = (Vec::new(), Vec::new(), Vec::new());
    let inner = ribbon(
        points,
        half,
        0.0,
        tex_len_m,
        ends,
        ring,
        &mut sink.0,
        &mut sink.1,
        &mut sink.2,
    );
    sink = (Vec::new(), Vec::new(), Vec::new());
    let outer = ribbon(
        points,
        half + ROAD_CASING_M,
        0.0,
        tex_len_m,
        kerb_ends,
        ring,
        &mut sink.0,
        &mut sink.1,
        &mut sink.2,
    );
    let zips = inner
        .sides
        .iter()
        .zip(outer.sides.iter())
        .all(|(a, b)| a.len() == b.len() && a.len() >= 2);
    if !zips {
        // Fall back to the slab; the cut removes the doubled area either way.
        let base = positions.len() as u32;
        positions.append(&mut sink.0);
        uvs.append(&mut sink.1);
        indices.extend(sink.2.iter().map(|i| i + base));
        return;
    }

    for (a, b) in inner.sides.iter().zip(outer.sides.iter()) {
        let base = positions.len() as u32;
        for (k, (p, q)) in a.iter().zip(b.iter()).enumerate() {
            let v = k as f32 / (a.len() - 1) as f32;
            positions.push([p[0], 0.0, p[1]]);
            uvs.push([0.0, v]);
            positions.push([q[0], 0.0, q[1]]);
            uvs.push([1.0, v]);
        }
        for k in 0..a.len() - 1 {
            let (i0, o0) = (base + (k as u32) * 2, base + (k as u32) * 2 + 1);
            let (i1, o1) = (i0 + 2, o0 + 2);
            push_triangle_up(positions, indices, i0, o0, o1);
            push_triangle_up(positions, indices, i0, o1, i1);
        }
    }
    // A kerb also wraps the opening of a junction stub, where the carriageway ends
    // square against the road it joins rather than being mitred into it. Those
    // patches are not side strips, so they are laid whole — as the quads `ribbon`
    // made them — and the general cut takes the carriageway back out of them.
    // There are at most two per ribbon, so the cut has almost nothing to do.
    let strips = if ring { 2 } else { 1 };
    for stub in outer.contours.into_iter().skip(strips) {
        if stub.len() != 4 {
            continue;
        }
        let base = positions.len() as u32;
        for p in &stub {
            positions.push([p[0], 0.0, p[1]]);
            uvs.push([0.0, 0.0]);
        }
        push_triangle_up(positions, indices, base, base + 1, base + 2);
        push_triangle_up(positions, indices, base, base + 2, base + 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector_tiles::bytes::tessellate_roads;

    // A real z14 tile over Groningen, from the VersaTiles Shortbread source
    // (now self-hosted at tiles.cartopolis.org; the archive is identical).
    use crate::testdata::GRONINGEN_Z14;

    /// **A side street's opening has to be open.** A kerb is the edge between a
    /// carriageway and what is not one, so where another carriageway arrives
    /// there is no kerb.
    ///
    /// Until `cut_band` the cut settled that on `road_rung` alone, which puts a
    /// whole class above a whole class, kerb band included — so a residential
    /// street tee-ing into a primary road lost to that road's *kerb*: its tarmac
    /// stopped at the far side of the band and the kerb line ran on unbroken
    /// across the opening. Measured on the fixture over the junctions where two
    /// kerbed classes meet at a clear angle, the point on the side street's
    /// centre line halfway through the main road's kerb band was drawn as:
    ///
    /// | | before | after |
    /// |---|---|---|
    /// | the side street's carriageway | 0 | 129 |
    /// | the main road's kerb | 129 | 0 |
    ///
    /// A picture cannot settle this — a kerb crossing a junction opening is a
    /// hairline at any altitude a whole junction fits in — which is why it is a
    /// geometric assertion and not a screenshot.
    #[test]
    fn a_side_street_reaches_the_road_it_joins() {
        let tile_m = 1000.0f32;
        let parsed = TileData::parse(GRONINGEN_Z14).expect("parse");
        let meta = parsed.layer("streets").expect("a streets layer");
        let features = parsed.features(meta);
        let scale = tile_m / TileData::extent(meta);
        let nodes = street_nodes(&features, scale);
        let meshes = tessellate_roads(GRONINGEN_Z14, tile_m, &[]).expect("tessellate");

        let covers = |group: &dyn Fn(SurfaceGroup) -> bool, p: [f32; 2]| -> bool {
            meshes.iter().filter(|m| group(m.group)).any(|m| {
                m.indices.chunks_exact(3).any(|t| {
                    let q: Vec<[f32; 2]> = t
                        .iter()
                        .map(|i| [m.positions[*i as usize][0], m.positions[*i as usize][2]])
                        .collect();
                    let s = |a: &[f32; 2], b: &[f32; 2]| {
                        (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
                    };
                    let (d0, d1, d2) = (s(&q[0], &q[1]), s(&q[1], &q[2]), s(&q[2], &q[0]));
                    (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0) || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
                })
            })
        };
        let kerbed = |c: RoadClass| {
            matches!(
                c,
                RoadClass::Motorway | RoadClass::Carriageway | RoadClass::Major | RoadClass::Minor
            )
        };

        let (mut open, mut walled, mut bare) = (0usize, 0usize, 0usize);
        let (mut ranked, mut angled) = (0usize, 0usize);
        for (key, arms) in &nodes {
            for side in arms {
                // A street that *ends* here…
                let Some((leaves, _)) = side.leaves else {
                    continue;
                };
                if !kerbed(side.class) {
                    continue;
                }
                for main in arms {
                    // …onto a road of a higher class, which is exactly the pair
                    // the old ordering got wrong: the kerb band goes in at the
                    // foot of its own class's block, so a whole class's kerb sat
                    // above a whole lower class's tarmac.
                    if main.feature == side.feature
                        || main.leaves.is_some()
                        || !kerbed(main.class)
                        || road_class_rank(main.class) <= road_class_rank(side.class)
                    {
                        continue;
                    }
                    ranked += 1;
                    // Shallow crossings are excluded: they stretch the band the
                    // sample has to land in along the side street until the
                    // street's own first segment is too short to hold it, and
                    // below `MERGE_SIN` the two are a continuation rather than a
                    // junction at all.
                    let sin = (leaves[0] * main.axis[1] - leaves[1] * main.axis[0]).abs();
                    if sin < 0.45 {
                        continue;
                    }
                    // The sample sits on the side street's own centre line, in
                    // the main road's kerb band **on the approach side** — the
                    // node is on the main road's centre line, so the band the
                    // side street has to break through is the one between it and
                    // the carriageway. It is `1 / sin` further out along the side
                    // street than it is across the main road, which is what makes
                    // the crossing angle matter here at all. Placed 40 % into the
                    // 0.75 m band, which leaves room for the node key's rounding
                    // to whole tile units (±0.12 m at z14).
                    let d = (main.half + ROAD_CASING_M * 0.4) / sin;
                    // …and only where the side street actually runs that far in a
                    // straight line, or the sample is off its centre line.
                    if side.leaves.is_some_and(|(_, seg)| seg < d) {
                        continue;
                    }
                    angled += 1;
                    let p = [
                        key.0 as f32 * scale + leaves[0] * d,
                        key.1 as f32 * scale + leaves[1] * d,
                    ];
                    if covers(&|g| matches!(g, SurfaceGroup::Road(_)), p) {
                        open += 1;
                    } else if covers(&|g| matches!(g, SurfaceGroup::RoadCasing(_)), p) {
                        walled += 1;
                    } else {
                        bare += 1;
                    }
                }
            }
        }
        println!(
            "cross-class junction ends {ranked}, of those at a usable angle {angled}; \
             openings: {open} open, {walled} walled off, {bare} bare"
        );
        // The population is small because most of a city centre's streets are
        // one Shortbread class (`Minor` covers tertiary down to service), and it
        // is only *across* classes that a kerb ever outranked a carriageway.
        assert!(open + walled + bare >= 12, "too few junctions to conclude");
        assert_eq!(
            walled,
            0,
            "{walled} of {} openings are sealed by the kerb of the road they join",
            open + walled + bare,
        );
    }

    /// **A drawn street is a curve, not a chain of boards.**
    ///
    /// Shortbread generalises its geometry for a map read from above, where a
    /// 20° kink in a street is a pixel. Extruded into a ribbon and seen from the
    /// pavement, that kink is a corner in the kerb and in the painted edge line,
    /// and the street reads as a row of straight planks. Measured on the fixture
    /// over every drawn centre line's interior vertices:
    ///
    /// | turn | raw | rounded |
    /// |---|---|---|
    /// | under 10° | 648 (38 %) | 4616 (94 %) |
    /// | 10–20° | 378 | 2 |
    /// | 20–70° | 447 | 70 |
    /// | over 70° — a corner the map asserts | 216 | 216 |
    ///
    /// The last row is the one that must **not** move: a street grid's right
    /// angles are real, and `round_corners` keeps them. What is left in 20–70° is
    /// the corners the fold rule and the one-metre allowance refused, which is
    /// the guard working rather than a miss.
    #[test]
    fn a_drawn_street_is_a_curve_not_a_chain_of_boards() {
        let tile_m = 1000.0f32;
        let parsed = TileData::parse(GRONINGEN_Z14).expect("parse");
        let meta = parsed.layer("streets").expect("streets");
        let features = parsed.features(meta);
        let scale = tile_m / TileData::extent(meta);

        let tally = |rounded: bool| -> [usize; 4] {
            let mut bins = [0usize; 4];
            for f in features.iter() {
                let Some((_, half)) = drawn_street(f) else {
                    continue;
                };
                let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
                match &f.geometry {
                    geo_types::Geometry::LineString(ls) => lines.push(ls),
                    geo_types::Geometry::MultiLineString(ml) => lines.extend(ml.iter()),
                    _ => continue,
                }
                for ls in lines {
                    let raw: Vec<[f32; 2]> =
                        ls.0.iter().map(|c| [c.x * scale, c.y * scale]).collect();
                    let p = if rounded {
                        round_corners(&raw, half)
                    } else {
                        raw
                    };
                    for i in 1..p.len().saturating_sub(1) {
                        let a = [p[i][0] - p[i - 1][0], p[i][1] - p[i - 1][1]];
                        let b = [p[i + 1][0] - p[i][0], p[i + 1][1] - p[i][1]];
                        let (la, lb) = (a[0].hypot(a[1]), b[0].hypot(b[1]));
                        if la < 1e-4 || lb < 1e-4 {
                            continue;
                        }
                        let cos = ((a[0] * b[0] + a[1] * b[1]) / (la * lb)).clamp(-1.0, 1.0);
                        let deg = cos.acos().to_degrees();
                        bins[match deg {
                            d if d < 10.0 => 0,
                            d if d < 20.0 => 1,
                            d if d < 70.0 => 2,
                            _ => 3,
                        }] += 1;
                    }
                }
            }
            bins
        };

        let raw = tally(false);
        let round = tally(true);
        println!(
            "raw     <10 {} 10-20 {} 20-70 {} >70 {}",
            raw[0], raw[1], raw[2], raw[3]
        );
        println!(
            "rounded <10 {} 10-20 {} 20-70 {} >70 {}",
            round[0], round[1], round[2], round[3]
        );

        let visible = |b: [usize; 4]| b[1] + b[2];
        assert!(
            visible(raw) > 700,
            "the fixture should be faceted to begin with: {raw:?}"
        );
        assert!(
            visible(round) * 8 < visible(raw),
            "rounding left {} visible kinks of {}",
            visible(round),
            visible(raw),
        );
        assert_eq!(
            raw[3], round[3],
            "a corner the map asserts must survive rounding untouched",
        );
    }

    /// The three properties `round_corners` is allowed to have, and the one it is
    /// not. Each is load-bearing somewhere else in this module, so each is
    /// asserted here rather than left to the fixture.
    #[test]
    fn a_fillet_keeps_the_ends_the_bearings_and_the_corner() {
        // A gentle bend on long segments: rounded.
        let bend = [[-60.0f32, 0.0], [0.0, 0.0], [50.0, 30.0]];
        let out = round_corners(&bend, 3.5);
        assert!(out.len() > bend.len(), "a 31° bend should be filleted");
        assert_eq!(
            out[0], bend[0],
            "the head is a junction node and may not move"
        );
        assert_eq!(out[out.len() - 1], bend[2], "and so is the tail",);
        // The first segment keeps its bearing — `junction_extent` and
        // `continuation` both read it off the raw line.
        let bearing = |a: [f32; 2], b: [f32; 2]| (b[1] - a[1]).atan2(b[0] - a[0]);
        assert!(
            (bearing(out[0], out[1]) - bearing(bend[0], bend[1])).abs() < 1e-4,
            "the fillet turned the first segment",
        );
        // …and it never wanders further from the recorded corner than it is
        // allowed to.
        let worst = out
            .iter()
            .map(|p| (p[0] - bend[1][0]).hypot(p[1] - bend[1][1]))
            .fold(f32::MAX, f32::min);
        assert!(
            worst <= ROUND_MAX_OFFSET_M + 1e-3,
            "the fillet cut {worst:.2} m off a corner it may cut {ROUND_MAX_OFFSET_M} m off",
        );

        // A right angle is a corner the map asserts, and survives.
        let square = [[-60.0f32, 0.0], [0.0, 0.0], [0.0, 60.0]];
        assert_eq!(round_corners(&square, 3.5), square.to_vec());

        // And a bend too tight for the road's own width is refused rather than
        // folded: a 22 m carriageway cannot be bent round a 3 m fillet.
        let tight = [[-8.0f32, 0.0], [0.0, 0.0], [7.0, 4.0]];
        assert_eq!(
            round_corners(&tight, 11.0),
            tight.to_vec(),
            "a fillet tighter than the half-width would fold the inner edge",
        );
        assert!(
            round_corners(&tight, 1.5).len() > tight.len(),
            "…while the same bend on an alley is fine",
        );
    }

    /// One arm of a junction, as `street_nodes` would have recorded it.
    fn arm(feature: usize, half: f32, class: RoadClass, leaves: [f32; 2]) -> NodeArm {
        NodeArm {
            feature,
            half,
            class,
            axis: leaves,
            leaves: Some((leaves, 50.0)),
        }
    }

    /// A street that Shortbread split in two is not a junction with itself, and
    /// this is the common case: 58 % of the fixture's junction ends are joins
    /// like that. The old rule *floored* the crossing sine and divided by it, so
    /// two collinear features each fired the longest stub the cap allowed —
    /// twelve metres of blank tarmac over its neighbour, from both sides. A real
    /// crossing still gets its fill.
    #[test]
    fn a_street_that_continues_asks_for_no_stub() {
        let east = [1.0, 0.0];
        let mut nodes: HashMap<(i32, i32), Vec<NodeArm>> = HashMap::new();
        // Feature 0 runs west→east and ends here; feature 1 carries it on east.
        nodes.insert(
            (0, 0),
            vec![
                arm(0, 3.5, RoadClass::Minor, [-1.0, 0.0]),
                arm(1, 3.5, RoadClass::Minor, east),
            ],
        );
        assert_eq!(
            junction_extent(&nodes, [0.0, 0.0], 0, east),
            0.0,
            "a collinear continuation is not a notch to fill"
        );
        assert!(
            continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_some(),
            "the road carrying on east must be found as the continuation"
        );

        // The same node with a genuine side road crossing at 90°: the stub is
        // back, and reaches that road's far kerb.
        nodes.get_mut(&(0, 0)).unwrap()[1] = arm(1, 5.0, RoadClass::Major, [0.0, 1.0]);
        assert!(
            (junction_extent(&nodes, [0.0, 0.0], 0, east) - 5.0).abs() < 1e-4,
            "a perpendicular crossing still needs its junction filled"
        );
        assert!(
            continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none(),
            "a road crossing at 90 degrees is not a continuation"
        );

        // Nor is a road of another class, or one of a very different width,
        // however well aligned: grafting a service road's cap onto a motorway's
        // is not a join.
        nodes.get_mut(&(0, 0)).unwrap()[1] = arm(1, 11.0, RoadClass::Motorway, east);
        assert!(continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none());
        nodes.get_mut(&(0, 0)).unwrap()[1] = arm(1, 8.0, RoadClass::Minor, east);
        assert!(continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none());

        // A road that merely *passes through* the node is being crossed, not
        // joined, even when it is aligned — it has no end here to meet.
        let mut through = arm(1, 3.5, RoadClass::Minor, east);
        through.leaves = None;
        nodes.get_mut(&(0, 0)).unwrap()[1] = through;
        assert!(continuation(&nodes, [0.0, 0.0], 0, east, RoadClass::Minor, 3.5).is_none());
    }

    /// The point of mitring a continuation rather than approximating it: the two
    /// halves must put their end vertices on the *same two points*, so the join
    /// is watertight by construction. Two square butt caps meeting at a 30° bend
    /// leave a wedge of bare ground on the outside and a doubled overlap on the
    /// inside — measured up to 1.28 m across on the fixture's 7 m streets.
    #[test]
    fn two_halves_of_one_street_meet_on_the_same_two_points() {
        let turn = 30.0f32.to_radians();
        // A comes in from the west and stops at the origin; B carries on, bent
        // by `turn`. Each is told the direction the other leaves the node in.
        let b_leaves = [turn.cos(), turn.sin()];
        let a_leaves = [-1.0, 0.0];
        let lay = |pts: &[[f32; 2]], ends: [EndJoin; 2]| {
            let (mut p, mut u, mut i) = (Vec::new(), Vec::new(), Vec::new());
            ribbon(pts, 3.5, 0.0, 12.0, ends, false, &mut p, &mut u, &mut i);
            p
        };
        let seg = 40.0;
        let a = lay(
            &[[-seg, 0.0], [0.0, 0.0]],
            [
                EndJoin::default(),
                EndJoin {
                    extend: 0.0,
                    cont: Some((b_leaves, seg)),
                },
            ],
        );
        let b = lay(
            &[[0.0, 0.0], [b_leaves[0] * seg, b_leaves[1] * seg]],
            [
                EndJoin {
                    extend: 0.0,
                    cont: Some((a_leaves, seg)),
                },
                EndJoin::default(),
            ],
        );
        // The two vertices each ribbon puts *at* the node.
        let at_node = |v: &[[f32; 3]]| -> Vec<[f32; 2]> {
            let mut out: Vec<[f32; 2]> = v
                .iter()
                .filter(|p| p[0].hypot(p[2]) < 3.5 * MITER_LIMIT + 1e-3)
                .map(|p| [p[0], p[2]])
                .collect();
            out.sort_by(|x, y| x[1].partial_cmp(&y[1]).unwrap());
            out.dedup_by(|x, y| (x[0] - y[0]).hypot(x[1] - y[1]) < 1e-4);
            out
        };
        let (ea, eb) = (at_node(&a), at_node(&b));
        assert_eq!(ea.len(), 2, "an end is two vertices, got {ea:?}");
        assert_eq!(eb.len(), 2, "an end is two vertices, got {eb:?}");
        for (x, y) in ea.iter().zip(&eb) {
            assert!(
                (x[0] - y[0]).hypot(x[1] - y[1]) < 1e-4,
                "the two halves end on different points: {x:?} against {y:?}"
            );
        }
        // And the cap really is mitred, not square: a butt end would sit on the
        // segment normal, i.e. exactly on x = 0.
        assert!(
            ea.iter().any(|p| p[0].abs() > 0.1),
            "the cap is still square: {ea:?}"
        );
    }

    /// A roundabout's centre line closes on itself, so its "two ends" are one
    /// node the road runs *through*. Butt-capping there bit a notch out of every
    /// roundabout in the city; closing the ribbon puts both ends on one miter.
    #[test]
    fn a_ring_closes_on_itself_instead_of_butting() {
        let n = 12;
        let ring: Vec<[f32; 2]> = (0..=n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                [20.0 * a.cos(), 20.0 * a.sin()]
            })
            .collect();
        let lay = |ring_flag: bool| {
            let (mut p, mut u, mut i) = (Vec::new(), Vec::new(), Vec::new());
            ribbon(
                &ring,
                3.5,
                0.0,
                12.0,
                [EndJoin::default(); 2],
                ring_flag,
                &mut p,
                &mut u,
                &mut i,
            );
            p
        };
        let closed = lay(true);
        let butted = lay(false);
        // The seam's first and last vertex pairs are the same two points when the
        // ribbon closes, and different ones when it butt-caps.
        let seam = |p: &[[f32; 3]]| {
            let last = p.len() - 2;
            (
                (p[0][0] - p[last][0]).hypot(p[0][2] - p[last][2]),
                (p[1][0] - p[last + 1][0]).hypot(p[1][2] - p[last + 1][2]),
            )
        };
        let (d0, d1) = seam(&closed);
        assert!(
            d0 < 1e-4 && d1 < 1e-4,
            "the ring does not close: seam gaps {d0} and {d1} m"
        );
        let (b0, b1) = seam(&butted);
        assert!(
            b0.max(b1) > 0.2,
            "the butt-capped control should show the notch this test is about"
        );
        // Every vertex still sits on the carriageway: the closing miter must not
        // spike, and the ring must not neck in.
        for p in &closed {
            let r = p[0].hypot(p[2]);
            assert!(
                (16.0..=24.5).contains(&r),
                "ring vertex at radius {r} is off the carriageway"
            );
        }
    }

    /// A straight run of road must come out exactly its class's width — this is
    /// the number that decides whether a street looks like a street from the
    /// pavement, and the miter maths is the easiest place to lose it.
    #[test]
    fn a_straight_ribbon_is_the_road_width_across() {
        let mut positions = Vec::new();
        let mut uvs = Vec::new();
        let mut indices = Vec::new();
        // 100 m due east, 7 m wide (a residential street), joining a 10 m road at
        // its far end and nothing at all at its near one.
        ribbon(
            &[[0.0, 0.0], [100.0, 0.0]],
            3.5,
            0.0,
            12.0,
            [
                EndJoin::default(),
                EndJoin {
                    extend: 5.0,
                    cont: None,
                },
            ],
            false,
            &mut positions,
            &mut uvs,
            &mut indices,
        );
        assert!(!indices.is_empty());
        for p in &positions {
            assert!(
                (p[2].abs() - 3.5).abs() < 1e-3,
                "edge at {:?} is not 3.5 m from the centre line",
                p
            );
        }
        // The junction stub reaches the far kerb of the road it joins, and the
        // dead end stops where the centre line does.
        let x_max = positions.iter().fold(f32::MIN, |a, p| a.max(p[0]));
        let x_min = positions.iter().fold(f32::MAX, |a, p| a.min(p[0]));
        assert!((x_max - 105.0).abs() < 1e-3, "tail at {x_max}");
        assert!(x_min.abs() < 1e-3, "head at {x_min}");
        // `v` counts metres travelled over the class's texture length.
        let v_max = uvs.iter().fold(0.0f32, |a, uv| a.max(uv[1]));
        assert!((v_max - 105.0 / 12.0).abs() < 1e-2, "v_max {v_max}");
    }

    /// **A roundabout's ribbon may not spike into its own island.**
    ///
    /// A ring is the shape this geometry is worst at: every vertex turns the same
    /// way, so every *inner* join runs toward one point, and the miter length
    /// there is `half / sin(theta/2)` — which grows without bound as a segment
    /// gets short. Groningen has roundabouts whose island is a few metres across
    /// and whose ring the encoder generalises to a dozen uneven segments, and one
    /// of those drew a tooth of tarmac across the middle of its own island
    /// (seen 2026-09-20).
    ///
    /// The invariant is radial and needs no tuning: on a closed ring of radius
    /// `r` drawn at half-width `h`, every vertex belongs in the annulus between
    /// `r - h` and `r + h`, give or take what a miter legitimately adds at a
    /// corner. A vertex nearer the centre than that has folded through the inner
    /// edge, and one at the centre or beyond it is the tooth.
    fn ring_annulus(radius: f32, segs: usize, jitter: f32, half: f32) -> (f32, f32) {
        ring_annulus_as(radius, segs, jitter, half, true)
    }

    fn ring_annulus_as(radius: f32, segs: usize, jitter: f32, half: f32, ring: bool) -> (f32, f32) {
        ring_annulus_ends(radius, segs, jitter, half, ring, 0.0)
    }

    fn ring_annulus_ends(
        radius: f32,
        segs: usize,
        jitter: f32,
        half: f32,
        ring: bool,
        extend: f32,
    ) -> (f32, f32) {
        let mut centre: Vec<[f32; 2]> = (0..segs)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32 / segs as f32 * std::f32::consts::TAU;
                // Uneven segments, which is what a generalised ring really is.
                #[allow(clippy::cast_precision_loss)]
                let r = radius * (1.0 + jitter * ((i * 7) % 5) as f32 / 5.0);
                [r * t.cos(), r * t.sin()]
            })
            .collect();
        centre.push(centre[0]);
        let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
        ribbon(
            &centre,
            half,
            0.0,
            12.0,
            [EndJoin { extend, cont: None }; 2],
            ring,
            &mut positions,
            &mut uvs,
            &mut indices,
        );
        assert!(!indices.is_empty(), "the ring built nothing");
        positions.iter().fold((f32::MAX, 0.0f32), |(lo, hi), p| {
            let d = p[0].hypot(p[2]);
            (lo.min(d), hi.max(d))
        })
    }

    /// The same ring handed over as an **open** polyline whose ends coincide —
    /// which is what a roundabout is if anything upstream fails to notice it
    /// closes. The two ends then get their own joins at the same place.
    #[test]
    fn a_ring_drawn_open_is_what_spikes() {
        let (half, radius) = (3.5, 7.5);
        let (closed_lo, _) = ring_annulus_as(radius, 12, 0.25, half, true);
        let (open_lo, _) = ring_annulus_as(radius, 12, 0.25, half, false);
        assert!(
            closed_lo > 1.0,
            "a ring drawn as a ring should keep its island: {closed_lo} m"
        );
        assert!(
            open_lo >= closed_lo - 0.01,
            "drawn open, the ring reaches {open_lo} m from its centre against \
             {closed_lo} m closed — the ends' joins are the spike"
        );
    }

    /// **An open ring whose ends are extended is the spike.**
    ///
    /// `junction_extent` runs a road's end on into whatever it meets, which is
    /// right at a tee and wrong on a circle: the stub leaves along the *tangent*,
    /// and a tangent from a small ring crosses the island. If a roundabout ever
    /// reaches `ribbon` as an open polyline with stubs — because nothing upstream
    /// noticed it closes — this is the tooth of tarmac it draws across its own
    /// middle.
    #[test]
    fn an_extended_open_ring_is_the_spike() {
        let (half, radius) = (3.5, 7.5);
        let (no_stub, _) = ring_annulus_ends(radius, 12, 0.25, half, false, 0.0);
        let (stubbed, _) = ring_annulus_ends(radius, 12, 0.25, half, false, half * 2.0);
        assert!(
            stubbed >= no_stub - 0.01,
            "stubs pull the ring to {stubbed} m from its centre against {no_stub} m \
             without them — that is a tangent crossing the island"
        );
    }

    /// **An arc of a roundabout, stubbed at both ends.**
    ///
    /// This is what a real roundabout is: OSM closes the way, the encoder splits
    /// it at each exit, and what reaches `ribbon` is three or four arcs, each an
    /// open polyline whose ends are junctions. `junction_extent` then runs each
    /// end on along its **tangent** — and the tangent of a small circle cuts the
    /// chord, which is to say it crosses the island.
    ///
    /// The full-circle cases above pass because a whole ring closes on itself and
    /// never grows a stub. Splitting it is what exposes this.
    #[test]
    fn an_arc_of_a_roundabout_does_not_cross_its_island() {
        let (half, radius) = (2.9, 6.9);
        for arc_deg in [120.0f32, 90.0, 72.0] {
            let segs = 5;
            let centre: Vec<[f32; 2]> = (0..=segs)
                .map(|i| {
                    #[allow(clippy::cast_precision_loss)]
                    let t = (i as f32 / segs as f32) * arc_deg.to_radians();
                    [radius * t.cos(), radius * t.sin()]
                })
                .collect();
            let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
            ribbon(
                &centre,
                half,
                0.0,
                12.0,
                [EndJoin {
                    extend: half * 2.0,
                    cont: None,
                }; 2],
                false,
                &mut positions,
                &mut uvs,
                &mut indices,
            );
            let nearest = positions
                .iter()
                .fold(f32::MAX, |a, p| a.min(p[0].hypot(p[2])));
            assert!(
                nearest > radius - half - 0.6,
                "a {arc_deg} deg arc reaches {nearest} m from the centre, inside the \
                 {} m its inner edge may reach — the end stubs are crossing the island",
                radius - half
            );
        }
    }

    /// **A kink in a ring must not throw its inner edge across the island.**
    ///
    /// This is the real Groningen roundabout, reduced: a generalised ring with one
    /// vertex sharper than its neighbours. The *outer* side of a sharp corner is
    /// bevelled once the miter passes [`MITER_LIMIT`]; the inner side is clamped
    /// only in how far it runs back **along** the two segments, and nothing bounds
    /// how far it moves **across** them. At a sharp kink the bisector points over
    /// the carriageway, so the inner vertex lands well inside the ring — a tooth
    /// of tarmac across the middle of the island, which is what the map drew
    /// (seen 2026-09-20, in the carriageway and in its kerb band at the same
    /// vertex, because both come off one centre line).
    #[test]
    fn a_kink_in_a_ring_does_not_cross_the_island() {
        let (half, radius) = (2.9, 6.9);
        let segs = 12;
        let mut centre: Vec<[f32; 2]> = (0..segs)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32 / segs as f32 * std::f32::consts::TAU;
                [radius * t.cos(), radius * t.sin()]
            })
            .collect();
        // One vertex pulled in hard — a generalised ring's short segment.
        centre[3] = [centre[3][0] * 0.35, centre[3][1] * 0.35];
        centre.push(centre[0]);

        let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
        ribbon(
            &centre,
            half,
            0.0,
            12.0,
            [EndJoin::default(); 2],
            true,
            &mut positions,
            &mut uvs,
            &mut indices,
        );
        let nearest = positions
            .iter()
            .fold(f32::MAX, |a, p| a.min(p[0].hypot(p[2])));
        // The kinked vertex itself sits at 0.35 r, so its own edges legitimately
        // reach `0.35 r - half`. Past the centre is never legitimate.
        assert!(
            nearest > 0.0,
            "the ribbon reached {nearest} m from the centre — through the middle \
             of the island and out the other side"
        );
    }

    /// **A road joining a roundabout must not stub across the island**, which is
    /// the tooth, and these are the tile's own coordinates for it.
    ///
    /// `--dump-lines` over `14/8490/5321`: the roundabout is the closing tail of a
    /// `residential` way, and a `service` road begins **on one of its vertices**
    /// and leaves to the south-east. `junction_extent` runs that end on into
    /// whatever it meets, because at a tee the two have to overlap for the cut to
    /// resolve them. A roundabout is not a tee: the road it meets curves away, so
    /// a straight stub cuts the chord — and on a ring this small the chord is the
    /// island.
    ///
    /// The five synthetic rings above all pass because they test the *ring's* own
    /// ends. The tooth belongs to the road joining it.
    /// **Known fault, reproduced, not yet fixed** — `#[ignore]` so it states the
    /// bug without failing the suite. Run it with
    /// `cargo test -p cartopolis_geo -- --ignored stub_across`.
    ///
    /// Measured: the stub reaches **2.35 m** of the roundabout's centre where the
    /// island begins at 3.05 m. The fix is not a smaller cap — the reach is
    /// already bounded at `2 x half` and the overshoot is 0.7 m, so any constant
    /// that fixes this one is a threshold waiting for a tighter roundabout. It
    /// has to come from the joined road's **curvature at the node**: `half / sin`
    /// is the far kerb of a road that runs straight through, and a road that
    /// turns there has its far kerb nearer. `NodeArm` carries one axis per arm and
    /// a ring contributes two, so the turn is available; spending it correctly is
    /// the work.
    #[test]
    #[ignore = "reproduces a known fault: a stub crosses a small roundabout's island"]
    fn a_road_joining_a_roundabout_does_not_stub_across_the_island() {
        // The ring, verbatim, and the centre it turns about.
        let ring: [[f32; 2]; 10] = [
            [-1.97, -6.10],
            [-6.26, -3.24],
            [-7.33, 1.77],
            [-4.83, 6.06],
            [-0.54, 7.85],
            [1.97, 7.49],
            [5.54, 4.99],
            [6.97, 0.34],
            [6.26, -2.17],
            [2.68, -5.74],
        ];
        #[allow(clippy::cast_precision_loss)]
        let hub = ring.iter().fold([0.0f32; 2], |a, p| {
            [
                a[0] + p[0] / ring.len() as f32,
                a[1] + p[1] / ring.len() as f32,
            ]
        });
        let inner = ring
            .iter()
            .fold(f32::MAX, |a, p| a.min((p[0] - hub[0]).hypot(p[1] - hub[1])))
            - 3.5;

        // The service road, verbatim, stubbed back into the ring at its head.
        let spur = [[5.54, 4.99], [17.70, 39.68]];
        let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
        ribbon(
            &spur,
            2.0,
            0.0,
            12.0,
            [
                EndJoin {
                    extend: 7.0,
                    cont: None,
                },
                EndJoin::default(),
            ],
            false,
            &mut positions,
            &mut uvs,
            &mut indices,
        );
        let nearest = positions
            .iter()
            .fold(f32::MAX, |a, p| a.min((p[0] - hub[0]).hypot(p[2] - hub[1])));
        assert!(
            nearest >= inner,
            "the joining road reaches {nearest} m of the roundabout's centre, \
             inside the {inner} m where the island is — that is the tooth"
        );
    }

    #[test]
    fn a_roundabout_does_not_spike_into_its_island() {
        // A real small roundabout: ~7.5 m centre-line radius, a 7 m carriageway.
        for (segs, jitter) in [(12, 0.0), (12, 0.25), (9, 0.3), (8, 0.35)] {
            let (half, radius) = (3.5, 7.5);
            let (lo, hi) = ring_annulus(radius, segs, jitter, half);
            let floor = (radius * (1.0 - jitter)) - half * 1.6;
            assert!(
                lo > floor.max(0.0),
                "{segs} segments, jitter {jitter}: a vertex {lo} m from the centre, \
                 inside the {floor} m the inner edge may reach — the ribbon has \
                 folded through its own island"
            );
            assert!(hi < radius * (1.0 + jitter) + half * 1.6, "outer {hi}");
        }
    }

    /// Build one corner and hand back the ribbon's vertices in the XZ plane, each
    /// tagged with its `u` (which side of the road it is), plus the centre line
    /// the ribbon was actually built around.
    ///
    /// Built with no junction stubs (`extend = [0, 0]`): this is about what the
    /// join does to the two segments meeting at the corner, and a stub only ever
    /// lengthens the outer *ends*, which both tests filter away anyway.
    fn corner(turn_deg: f32, seg_m: f32, half: f32) -> (Vec<([f32; 2], f32)>, Vec<[f32; 2]>) {
        let t = turn_deg.to_radians();
        let centre = vec![
            [-seg_m, 0.0],
            [0.0, 0.0],
            [seg_m * t.cos(), seg_m * t.sin()],
        ];
        let (mut positions, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new());
        ribbon(
            &centre,
            half,
            0.0,
            12.0,
            [EndJoin::default(); 2],
            false,
            &mut positions,
            &mut uvs,
            &mut indices,
        );
        assert_eq!(uvs.len(), positions.len());
        assert!(!indices.is_empty());
        (
            positions
                .iter()
                .zip(&uvs)
                .map(|(p, uv)| ([p[0], p[2]], uv[0]))
                .collect(),
            // The line the ribbon is actually built on, which is the rounded one
            // wherever `corner_cut` allowed a fillet. Handing back the *input*
            // here would measure every one of these tests against a centre line
            // the road does not follow.
            round_corners(&centre, half),
        )
    }

    /// The **outer** edge of a bend keeps the full carriageway, however sharp.
    ///
    /// A single miter clamped by `MITER_LIMIT` pulled both edges in together, so a
    /// hairpin necked the road down instead of blunting its corner: a 22 m
    /// motorway measured 14.2 m across through a 150° turn and 4.8 m through a
    /// 170° one. Past the limit the corner is bevelled — two square ends at the
    /// full half-width — so the outer edge stays a genuine offset of the
    /// carriageway it belongs to.
    ///
    /// Measured against the segment *lines*, not the nearest point on the centre
    /// line: on the outside of a corner the nearest centre-line point is the
    /// corner vertex itself, which reports `half × miter` and so cannot tell a
    /// stretched join from a necked one.
    #[test]
    fn a_sharp_bend_keeps_the_full_carriageway_width() {
        let half = 11.0; // a 22 m motorway, the widest class and the worst case
        for turn in [5.0f32, 60.0, 120.0, 150.0, 170.0] {
            let t = turn.to_radians();
            let (verts, _) = corner(turn, 60.0, half);
            // Both segments run through the corner at the origin, so a vertex's
            // distance from each carriageway is just the cross product.
            let perp = |p: [f32; 2], d: [f32; 2]| (p[0] * d[1] - p[1] * d[0]).abs();
            // A positive turn puts the inside of the corner on `+m`, which is
            // `u = 1`; the outer edge is therefore the `u = 0` one. The segments
            // are 60 m long so that the corner's own vertices are the only ones
            // anywhere near the origin — every other vertex is a plain
            // perpendicular offset with nothing to say about the join.
            let at_corner = verts
                .iter()
                .filter(|(_, u)| *u == 0.0)
                .filter(|(p, _)| p[0].hypot(p[1]) < 45.0);
            let mut seen = 0;
            for (p, _) in at_corner {
                // Every outer vertex serves one of the two segments; it must sit
                // the full half-width off of that one.
                let reach = perp(*p, [1.0, 0.0]).max(perp(*p, [t.cos(), t.sin()]));
                assert!(
                    reach >= half - 1e-2,
                    "{turn}° turn necks the carriageway to {reach:.2} m of {half} m"
                );
                assert!(
                    reach <= half * MITER_LIMIT + 1e-2,
                    "{turn}° turn spikes the outer corner to {reach:.2} m"
                );
                seen += 1;
            }
            assert!(seen > 0, "{turn}° turn: no outer vertex at the corner");
        }
    }

    /// The **inner** edge must not fold back through the ribbon.
    ///
    /// The inner miter backtracks `half / tan(θ/2)` along both neighbours, and
    /// nothing bounded that against how long they actually are: on a 22 m
    /// motorway a 60° turn overran a 6 m segment — and z14 Shortbread geometry is
    /// full of sub-10 m segments. The fold is a self-intersecting quad, whose two
    /// layers carry different `v`, i.e. the centre line fighting its own
    /// reflection along the bend.
    ///
    /// A fold is the inner rail *reversing* along the road, so that is what this
    /// measures. It is not about quads overlapping each other: a 22 m carriageway
    /// around a 3 m corner has to double back over itself, and the union of that
    /// overlap is still the right piece of road.
    #[test]
    fn a_tight_corner_does_not_fold_the_ribbon() {
        for turn in [30.0f32, 60.0, 90.0, 120.0, 150.0, 170.0] {
            for seg in [3.0f32, 6.0, 15.0, 60.0] {
                let (verts, centre) = corner(turn, seg, 11.0);
                // A bevel splits the *outer* vertex in two and shares the inner
                // one, so the inner rail is the one with a vertex per centre-line
                // point — exactly the rail a fold shows up on, and the pairing
                // that lets each vertex be measured against its own segment
                // rather than whichever happens to be nearest (with a 22 m road
                // on 3 m segments, that is routinely the wrong one).
                let inner: Vec<[f32; 2]> = verts
                    .iter()
                    .filter(|(_, u)| *u == 1.0)
                    .map(|(p, _)| *p)
                    .collect();
                assert_eq!(inner.len(), centre.len(), "one inner vertex per point");
                for k in 0..centre.len() - 1 {
                    let (a, b) = (centre[k], centre[k + 1]);
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    let len = dx.hypot(dy);
                    let (ux, uy) = (dx / len, dy / len);
                    // How far each end of this quad's inner rail has advanced
                    // along the segment it belongs to.
                    let along = |p: [f32; 2]| (p[0] - a[0]) * ux + (p[1] - a[1]) * uy;
                    let (from, to) = (along(inner[k]), along(inner[k + 1]));
                    assert!(
                        to >= from - 1e-3,
                        "{turn}° turn on {seg} m segments folds the ribbon: over segment \
                         {k} the inner edge runs backwards, {from:.2} m to {to:.2} m"
                    );
                }
            }
        }
    }

    /// A join may not throw a spike into the field.
    ///
    /// [`MITER_LIMIT`] bounds how far the outer corner may stand from the centre
    /// line, and the bevel is what enforces it. This measures the geometry rather
    /// than the constant: every vertex of the built ribbon, against the line it
    /// was built on.
    #[test]
    fn a_reversal_is_bevelled_rather_than_drawn_as_a_spike() {
        let seg_dist = |p: [f32; 2], a: [f32; 2], b: [f32; 2]| -> f32 {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 1e-9 {
                (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (p[0] - (a[0] + dx * t)).hypot(p[1] - (a[1] + dy * t))
        };
        for turn in [30.0f32, 60.0, 90.0, 100.0, 128.2, 150.0, 170.0] {
            for half in [2.0f32, 4.5, 11.0] {
                let (verts, centre) = corner(turn, 6.0, half);
                let worst = verts
                    .iter()
                    .map(|(p, _)| {
                        centre
                            .windows(2)
                            .map(|w| seg_dist(*p, w[0], w[1]))
                            .fold(f32::MAX, f32::min)
                    })
                    .fold(0.0f32, f32::max);
                assert!(
                    worst <= half * MITER_LIMIT + 1e-3,
                    "a {turn}° turn on a {:.0} m road reaches {worst:.2} m from its centre \
                     line, past the {:.2} m the miter limit allows",
                    half * 2.0,
                    half * MITER_LIMIT,
                );
            }
        }

        // …and the limit is where it is for a reason, which is the half of this
        // that would otherwise drift. A right angle mitres at `1 / cos 45°` and
        // **keeps its point** — a street grid's right angles are corners the map
        // asserts, the same licence `ROUND_MAX_DEG` draws. A reversal does not:
        // the Groningen fixture's feature 69 is a `tertiary` turning head that
        // reverses through 128.2°, and its miter of `1 / cos 64.1° = 2.29` slipped
        // under the old limit of 2.5 and drew a ten-metre tongue of tarmac out
        // into the field past the end of the road.
        assert!(
            MITER_LIMIT >= 1.0 / std::f32::consts::FRAC_PI_4.cos(),
            "a right angle must keep its point",
        );
        assert!(
            MITER_LIMIT < 1.0 / 64.1f32.to_radians().cos(),
            "a 128° reversal must bevel rather than mitre",
        );
    }
}
