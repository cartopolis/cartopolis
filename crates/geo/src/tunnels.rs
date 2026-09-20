//! Roads that run under the ground, and the hole you get into them by.
//!
//! No Bevy: a tile in, [`SurfaceMesh`]es out. The streaming half is
//! `systems::tunnels`, and `docs/notes/tunnels.md` has the reasoning this module
//! only summarises.
//!
//! # What was here before
//!
//! Nothing. A way Shortbread flags as a tunnel was refused by
//! [`drawn_street`](crate::vector_tiles::drawn_street), dropped from the raster
//! paint and dropped from the street labels, so a tunnel was a gap in the map —
//! no carriageway, no markings, no name, no entrance. That was a deliberate choice
//! and it was right as far as it went: painting a tunnel *on the ground* rules a
//! pale band across every junction it passes under, which is a lie about what is
//! up there. What it never did was draw the tunnel anywhere else.
//!
//! So the rule changes shape rather than going away. A tunnel still leaves the
//! ground sheet — it is not on the ground — and it is drawn **below** it instead,
//! as a carriageway with walls and a roof, which is what it is.
//!
//! # The tiles carry a boolean and nothing else
//!
//! Not a depth, and — measured on the Groningen fixture, which is what
//! [`tests::the_tiles_say_which_streets_are_tunnels`] pins — **not a `layer`
//! either**: ten streets in that tile are flagged `tunnel` and the attribute set
//! is `kind, bridge, tunnel, rail, oneway, oneway_reverse, link, surface`, with no
//! `layer` anywhere in the layer. (`vector_tiles::layer_of` reads one and its doc
//! said Shortbread carries it on `streets`; on this tileset it answers 0 for every
//! street there is, which also makes the raster's "flyover paints over what it
//! crosses" sort a no-op. Left alone here — it is a separate thing to fix.)
//!
//! So every tunnel is one level deep, [`LEVEL_DEPTH_M`], and that is a convention
//! rather than a measurement. It is the one number here a real source could
//! replace.
//!
//! # An arcade is flagged a tunnel too, so ask the buildings
//!
//! OSM's `tunnel` also covers `building_passage` — a way through a building, at
//! ground level, that people walk down — and roughly a quarter of the flagged ways
//! around Groningen's centre are those. With no `layer` to separate them, sinking
//! every flagged way seven metres would put a hole under a shopping street.
//!
//! The tile answers it: a passage runs *through a building* and a tunnel does not,
//! and the `buildings` layer is in the same tile this is read from. So
//! [`building_passages`] tests each flagged way against the building polygons, and
//! what comes back stays on the ground sheet as ordinary road. It is the same move
//! `interiors` makes with a door's normal — probe the data rather than assume a
//! rule — and it fails in the safe direction: a tunnel we mistake for a passage is
//! drawn where it always was, not drawn as a hole.
//!
//! # The entrance is this module's invention too, and it has to be
//!
//! OSM tags the tunnel at full depth for its whole length, and the road that
//! *descends* to the entrance is an ordinary surface way with nothing on it to say
//! it is a ramp. Drawn literally, that is a carriageway seven metres down meeting
//! a carriageway at ground level with a vertical step between them — a hole in
//! the road you fall into. [`bridge-heights.md`](../../../docs/notes/bridge-heights.md)
//! describes the same constraint from the other side and accepts the step, because
//! a deck with abrupt ends still reads as a deck. A tunnel cannot accept it: the
//! step *is* the way in.
//!
//! So the ramp is built inside the tagged tunnel: the deck rises to meet the
//! ground over [`ramp_m`] at each genuine entrance, at [`MAX_GRADE`]. Two
//! consequences follow and both are deliberate. A tunnel shorter than two ramps
//! never reaches full depth — which is honest for an underpass and wrong for a
//! tunnel under a hill, and the tiles cannot tell those apart. And the opening sits
//! *inside* the tagged length rather than at its end.
//!
//! An end is a entrance only when the line genuinely ends there. A run cut by the
//! tile border continues in the next tile and is held at full depth, so the two
//! halves meet at the border at the same depth; and an end that another tunnel
//! run also ends on is a tagging split, not a opening ([`open_ends`]).
//!
//! # The roof stops where it would surface
//!
//! There is no separate "how long is the opening" constant: the ceiling is drawn
//! wherever it fits under the ground ([`CEILING_MAX_Y`]) and is absent where the
//! rising deck would push it through, which is exactly the open approach. The
//! plan corridor of that roofless stretch comes back as a [`opening`](TunnelTessellation::openings)
//! — it is the ground the caller has to take away, or the tunnel is a room with
//! a lid over its own entrance.

use crate::road_texture::{road_class, road_width_m, RoadClass, StreetFlags};
use crate::vector_tiles::{
    clip_polyline, flag_of, kind_of, road_depth_rung, SurfaceGroup, SurfaceMesh, TileData,
    VERTEX_BYTES,
};
use geo_types::Geometry;
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay::{Overlay, ShapeType};
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;
use mvt_reader::feature::Feature;

/// Depth of one OSM `layer` level, in metres, measured to the carriageway.
///
/// A road tunnel is its clearance plus its roof plus its pavement: 4.8 m of
/// headroom, a slab, and the make-up under the deck. Seven metres is the round
/// number that lands on, and it is *a convention, not a measurement* — see the
/// module note. `layer=-2` is twice it, which is how a tunnel under a tunnel
/// (the Julianaweg under the Groningen ring) keeps its order.
pub const LEVEL_DEPTH_M: f32 = 7.0;

/// Headroom over a path tunnel's deck, in metres. A cycle underpass is signed at
/// 2.5 m and feels like it.
pub const PATH_CLEARANCE_M: f32 = 2.6;

/// Headroom over the carriageway, in metres.
///
/// The Dutch minimum over a trunk road is 4.6 m and the sign at the entrance reads
/// 4.5; 4.8 puts the ceiling above the tallest thing that may legally drive under
/// it without making the tunnel read as a cathedral.
pub const CLEARANCE_M: f32 = 4.8;

/// Depth of a tunnel carrying a path rather than traffic, in metres.
///
/// A cycle underpass is not a road tunnel with smaller cars in it: 2.5 m of
/// headroom against 4.8, a thinner deck, and no ventilation over it. Measured
/// from the other end — a Dutch fietstunnel's ramps typically drop about 3.5 m.
///
/// Giving these the road's seven metres is not merely too deep, it is the
/// difference between a shallow dip and nothing at all: they are **short**, the
/// ramp is derived from the depth, and a 30 m underpass ramping toward 7 m
/// reaches a metre and looks like a mistake in the ground.
pub const PATH_DEPTH_M: f32 = 3.6;

/// Steepest grade a road tunnel's entrance ramp may take, as a fraction.
///
/// Six per cent. Dutch practice holds a motorway ramp near four and a city
/// underpass near seven; the middle keeps a 7 m descent inside 120 m, which is
/// short enough that both ends of an ordinary urban tunnel still reach full
/// depth.
pub const MAX_GRADE: f32 = 0.06;

/// …and for a path, where the ramp is a person on a bicycle rather than a lorry.
///
/// Ten per cent. The Dutch design norm for a cycle ramp is nearer five, but that
/// is a norm about *comfort over the whole approach*, and the approach here is
/// only the part inside the tagged tunnel — the rest of the real ramp is an
/// ordinary path way this cannot see.
pub const PATH_GRADE: f32 = 0.10;

/// How close to full depth the deck must be before a roof *may* go over it.
///
/// **This is what makes a entrance read as a entrance.** Without it the roof begins
/// the moment a ceiling fits under the ground — 5.4 m down for a road tunnel — and
/// since that is most of the way down the ramp, the only stretch left open is the
/// shallow end. The hole then shows a carriageway running from flush with the
/// grass to a few metres down and no further, which is exactly what the first
/// capture beside DUO looked like: a road at lawn level with a lid over the
/// interesting part.
///
/// So the roof covers only the stretch that has reached depth and the ramp is an
/// open cut — which is also what the thing being modelled looks like. Groningen's
/// southern ring is a long open trench with three short lids over it, not a tube
/// with a hole at each end.
pub const ROOF_AT_DEPTH_M: f32 = 0.3;

/// Highest the ceiling may be and still be a roof, in metres below ground.
///
/// Slightly under the ground rather than flush with it: a ceiling at exactly
/// `y = 0` is coplanar with the ground sheet over it, which is the depth fight
/// this codebase has spent the most effort removing elsewhere. Six hundred
/// millimetres is also roughly a real slab, so the number is not arbitrary in
/// two ways at once.
pub const CEILING_MAX_Y: f32 = -0.6;

/// Thickness the walls are drawn with — zero. They are single quads.
///
/// A retaining wall has a thickness in the world and drawing it would double the
/// wall count for a surface nobody can get behind: the inner face is the only one
/// any camera reaches, because the outer face is buried in the ground the tunnel is
/// cut into.
const WALL_TOP_Y: f32 = 0.0;

/// Two ends of a run and whether each is a entrance.
type OpenEnds = [bool; 2];

/// One tunnel's plan corridor and the depth along it, for the consumers that need
/// to know where the road *is* rather than what it looks like: the avatar's
/// footing, and the ground cut at the opening.
#[derive(Clone, Debug, Default)]
pub struct Tunnel {
    /// Centre line, tile-local metres.
    pub centre: Vec<[f32; 2]>,
    /// Deck level at each centre vertex, metres (negative — below the ground
    /// sheet, before the terrain field displaces both together).
    pub road_y: Vec<f32>,
    /// Half the carriageway width, metres.
    pub half_w: f32,
    /// Headroom over the deck, metres. Carried rather than read off
    /// [`CLEARANCE_M`] so a consumer asking "am I inside this tunnel" gets the
    /// answer for *this* tunnel.
    pub clearance: f32,
    /// Whether each centre vertex has a lid over it, as against being the open
    /// ramp at either end.
    ///
    /// Carried because the *ground* needs it. Where a tunnel has a lid, the land
    /// above it is continuous — you walk over the top — and the measured height
    /// data does not always agree: Groningen's ring was flown as an open trench
    /// and has since been rebuilt with a lid, so the survey digs nine metres out
    /// of ground that is now whole. [`covered_weight`](Self::covered_weight) is
    /// how a ground mesh asks whether to believe the survey here.
    pub covered: Vec<bool>,
}

impl Tunnel {
    /// The deck level under `p`, if `p` is over this tunnel's carriageway.
    ///
    /// `None` rather than zero for "not over it", because zero is a real answer
    /// here — the deck reaches ground level at the entrance — and a caller that
    /// cannot tell those apart puts the avatar on a tunnel floor in the middle of
    /// a field.
    #[must_use]
    pub fn road_under(&self, p: [f32; 2]) -> Option<f32> {
        let mut best: Option<(f32, f32)> = None;
        for i in 0..self.centre.len().saturating_sub(1) {
            let (a, b) = (self.centre[i], self.centre[i + 1]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len_sq = dx * dx + dy * dy;
            if len_sq <= 1e-9 {
                continue;
            }
            // Clamped projection: past either end of a segment the nearest point
            // is that end, which is what makes a chain of segments behave as one
            // corridor with rounded ends rather than as a set of infinite strips.
            let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len_sq).clamp(0.0, 1.0);
            let (cx, cy) = (a[0] + dx * t, a[1] + dy * t);
            let d = (p[0] - cx).hypot(p[1] - cy);
            if d > self.half_w {
                continue;
            }
            let y = self.road_y[i] + (self.road_y[i + 1] - self.road_y[i]) * t;
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, y));
            }
        }
        best.map(|(_, y)| y)
    }

    /// How far `p` is from this tunnel's carriageway in plan, and the deck level
    /// there. Zero distance means `p` is over it.
    ///
    /// **This is the question a picture of a tunnel cannot answer.** A road seen
    /// from a low oblique is foreshortened into a band a few pixels deep, and a
    /// band seven metres down looks very much like a band on the ground —
    /// especially beside a second, ordinary road running parallel, which is
    /// exactly what a tunnel approach is. So a capture that seems to show a
    /// sunken carriageway and one that is pointed fifty metres off it at an
    /// ordinary street are the same picture. The distance separates them, and
    /// unlike [`Self::road_under`] it answers for a point that is *not* over the
    /// tunnel, which is the case that needs reporting.
    #[must_use]
    pub fn distance_to(&self, p: [f32; 2]) -> (f32, f32) {
        let mut best = (f32::MAX, 0.0);
        for i in 0..self.centre.len().saturating_sub(1) {
            let (a, b) = (self.centre[i], self.centre[i + 1]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len_sq = dx * dx + dy * dy;
            if len_sq <= 1e-9 {
                continue;
            }
            let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len_sq).clamp(0.0, 1.0);
            let d = (p[0] - (a[0] + dx * t)).hypot(p[1] - (a[1] + dy * t));
            if d < best.0 {
                let y = self.road_y[i] + (self.road_y[i + 1] - self.road_y[i]) * t;
                best = ((d - self.half_w).max(0.0), y);
            }
        }
        best
    }

    /// How much the land at `p` should be treated as continuous ground over a
    /// lid: 1 directly over a covered stretch, falling to 0 `taper_m` beyond it.
    ///
    /// **This exists because two sources disagree and one of them is stale.** The
    /// height survey measures what it flew over, and where a road has since been
    /// rebuilt with a lid, the survey still shows the open trench — so the ground
    /// beside a covered tunnel sinks nine metres while the lid stays at street
    /// level, and the tunnel reads as a raised platform. The map's own tag is the
    /// newer fact, so over a covered stretch the ground is lifted back to the
    /// level of the land around it.
    ///
    /// The taper is generous on purpose: a trench is wider than the road in it —
    /// two carriageways, their walls and their verges — so filling only the
    /// carriageway's own width would leave a ditch either side of a flush lid,
    /// which is the same fault in a narrower shape.
    #[must_use]
    pub fn covered_weight(&self, p: [f32; 2], taper_m: f32) -> f32 {
        let mut best = 0.0f32;
        for i in 0..self.centre.len().saturating_sub(1) {
            if !(self.covered[i] && self.covered[i + 1]) {
                continue;
            }
            let (a, b) = (self.centre[i], self.centre[i + 1]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len_sq = dx * dx + dy * dy;
            if len_sq <= 1e-9 {
                continue;
            }
            let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len_sq).clamp(0.0, 1.0);
            let d = (p[0] - (a[0] + dx * t)).hypot(p[1] - (a[1] + dy * t)) - self.half_w;
            let w = if d <= 0.0 {
                1.0
            } else if d >= taper_m {
                0.0
            } else {
                // Smooth rather than linear: a straight ramp leaves a visible
                // crease along the line where the fill stops.
                let x = 1.0 - d / taper_m;
                x * x * (3.0 - 2.0 * x)
            };
            best = best.max(w);
            if best >= 1.0 {
                break;
            }
        }
        best
    }

    /// The carriageway's plan outline, closed.
    ///
    /// The same offsets the walls were built on, so the surface the avatar is
    /// stopped by and the wall it can see are one shape rather than two
    /// descriptions of one.
    #[must_use]
    pub fn corridor(&self) -> Vec<[f32; 2]> {
        let sides = offset_sides(&self.centre, self.half_w);
        let mut ring: Vec<[f32; 2]> = Vec::with_capacity(self.centre.len() * 2 + 1);
        for s in &sides.left {
            ring.push(s[0]);
        }
        if let Some(last) = sides.left.last() {
            ring.push(last[1]);
        }
        for s in sides.right.iter().rev() {
            ring.push(s[1]);
        }
        if let Some(first) = sides.right.first() {
            ring.push(first[0]);
        }
        if let Some(&head) = ring.first() {
            ring.push(head);
        }
        ring
    }
}

/// Everything one tile's tunnels produce.
#[derive(Default)]
pub struct TunnelTessellation {
    /// The drawn tunnel: deck, walls and roof.
    pub meshes: Vec<SurfaceMesh>,
    /// Plan corridors of the roofless approaches, tile-local metres, each a
    /// closed ring. **The caller must cut these out of the ground**, or the
    /// entrance is roofed by the ground sheet and the tunnel cannot be entered.
    pub openings: Vec<Vec<[f32; 2]>>,
    /// Every tunnel in the tile, for footing and collision.
    pub tunnels: Vec<Tunnel>,
}

/// True for a way the tiles flag as a tunnel.
///
/// **Not the same question as "is it a tunnel"**, and the difference is the whole of
/// [`building_passages`]: this is every flagged way, including the arcades, and it
/// is what keeps all of them off the ground road sheet at the point where the
/// buildings are not available to ask (`vector_tiles::drawn_street`, and the
/// scatter and furniture layers that read it). `tessellate_roads` re-admits the
/// passages once it has the tile.
#[must_use]
pub fn underground(feature: &Feature) -> bool {
    flag_of(feature, "tunnel")
}

/// Which of the tile's streets are ways *through a building* rather than tunnels.
///
/// Feature indices into `tile.features(streets)`. A way is a passage when a point
/// on it lies inside one of the tile's building polygons — tested at several
/// points along the line and decided by the majority, because a tunnel that passes
/// under one corner of a building is still a tunnel, and a passage clipped by the
/// tile edge may start outside the wall it runs through.
#[must_use]
pub fn building_passages(
    tile: &TileData,
    features: &[Feature],
) -> std::collections::HashSet<usize> {
    let mut out = std::collections::HashSet::new();
    let rings = building_rings(tile);
    if rings.is_empty() {
        // No buildings in this tile: nothing here is a passage, which is the
        // right answer and not merely the available one — a building passage
        // with no building around it is a way through nothing.
        return out;
    }
    for (fi, feature) in features.iter().enumerate() {
        if !underground(feature) {
            continue;
        }
        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            _ => continue,
        }
        let mut inside = 0usize;
        let mut tested = 0usize;
        for ls in lines {
            let pts: Vec<[f32; 2]> = ls.0.iter().map(|c| [c.x, c.y]).collect();
            for p in sample_points(&pts) {
                tested += 1;
                if under_a_building(&rings, p) {
                    inside += 1;
                }
            }
        }
        if tested > 0 && inside * 2 > tested {
            out.insert(fi);
        }
    }
    out
}

/// Every building footprint in the tile, with its bounding box, in tile units.
///
/// Read twice — by [`building_passages`] and by the roof test — so it is one walk
/// of the layer rather than two.
fn building_rings(tile: &TileData) -> Vec<(Bbox, Vec<[f32; 2]>)> {
    let Some(meta) = tile.layer("buildings") else {
        return Vec::new();
    };
    tile.features(meta)
        .iter()
        .filter_map(|f| match &f.geometry {
            Geometry::Polygon(p) => Some(p.exterior().0.iter().map(|c| [c.x, c.y]).collect()),
            Geometry::MultiPolygon(mp) => {
                mp.0.first()
                    .map(|p| p.exterior().0.iter().map(|c| [c.x, c.y]).collect())
            }
            _ => None,
        })
        .filter(|ring: &Vec<[f32; 2]>| ring.len() >= 4)
        .map(|ring| (bbox_of_ring(&ring), ring))
        .collect()
}

/// True where a building stands over this point.
fn under_a_building(rings: &[(Bbox, Vec<[f32; 2]>)], p: [f32; 2]) -> bool {
    rings
        .iter()
        .any(|(b, ring)| point_in_bbox(p, b) && point_in_ring(p, ring))
}

/// A handful of points along a line: both ends and three interior fractions.
fn sample_points(pts: &[[f32; 2]]) -> Vec<[f32; 2]> {
    if pts.len() < 2 {
        return pts.to_vec();
    }
    let at = |t: f32| -> [f32; 2] {
        let f = t * (pts.len() - 1) as f32;
        let i = (f.floor() as usize).min(pts.len() - 2);
        let u = f - i as f32;
        [
            pts[i][0] + (pts[i + 1][0] - pts[i][0]) * u,
            pts[i][1] + (pts[i + 1][1] - pts[i][1]) * u,
        ]
    };
    vec![at(0.05), at(0.25), at(0.5), at(0.75), at(0.95)]
}

fn point_in_bbox(p: [f32; 2], b: &Bbox) -> bool {
    p[0] >= b[0] && p[0] <= b[2] && p[1] >= b[1] && p[1] <= b[3]
}

/// True where `p` falls in one of the openings.
///
/// For the layers that are cut by *instance* rather than by triangle: a third of
/// a tree is not a smaller tree, so a canopy standing in the hole has to be
/// dropped whole or kept whole. [`cut_ground`] says why it leaves them alone, and
/// this is what the caller uses instead.
#[must_use]
pub fn in_an_opening(openings: &[Vec<[f32; 2]>], p: [f32; 2]) -> bool {
    openings.iter().any(|ring| point_in_ring(p, ring))
}

/// Even-odd point in ring.
pub(crate) fn point_in_ring(p: [f32; 2], ring: &[[f32; 2]]) -> bool {
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[j]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// How deep this tunnel's deck runs, in metres (positive), and the grade its ramp
/// may take.
///
/// The tiles carry no depth and no level, so the only thing left to vary it by is
/// what the tunnel carries — and that one distinction matters more than it looks,
/// because the ramp length is derived from the depth. See [`PATH_DEPTH_M`].
fn depth_and_grade(class: RoadClass) -> (f32, f32) {
    match class {
        RoadClass::Path => (PATH_DEPTH_M, PATH_GRADE),
        _ => (LEVEL_DEPTH_M, MAX_GRADE),
    }
}

/// How much of a tunnel must end up under a lid for it to read as a tunnel.
///
/// Not a tolerance — a length. Under this the thing is a dip in the road with a
/// bridge over it, and drawing it as an open cutting is the truthful answer.
const MIN_COVERED_M: f32 = 40.0;

/// The deepest this class may go and still have headroom under a lid.
///
/// The roof test wants the ceiling at or below [`CEILING_MAX_Y`], and the ceiling
/// sits `clearance` over the deck — so a tunnel shallower than this can never be
/// roofed however long it is.
fn min_roofable_depth(class: RoadClass) -> f32 {
    let clearance = if matches!(class, RoadClass::Path) {
        PATH_CLEARANCE_M
    } else {
        CLEARANCE_M
    };
    clearance - CEILING_MAX_Y + ROOF_AT_DEPTH_M
}

/// **A tunnel is as deep as its length allows, not as deep as its class.**
///
/// Both ramps are `depth / grade` long, so a run that ramps from both ends spends
/// `total - 2 x depth / grade` under a lid — and at [`LEVEL_DEPTH_M`] and
/// [`MAX_GRADE`] that is 117 m of ramp each end, so a 238 m tunnel is covered for
/// **four metres**. Measured over Groningen's southern ring on 2026-09-20: six
/// carriageway tunnels of 171 m to 241 m, every one of them an open canyon with a
/// few metres of lid somewhere in the middle, which is not what any of them looks
/// like.
///
/// So the depth is reduced until [`MIN_COVERED_M`] fits, and refused outright
/// below [`min_roofable_depth`] — a run too short to be a tunnel at any depth
/// stays an open cutting, which is the honest drawing of a dip with a bridge over
/// it. A tunnel with only one entrance ramps once and gets the full depth.
fn depth_for_run(class: RoadClass, total: f32, entrances: OpenEnds) -> (f32, f32) {
    let (depth, grade) = depth_and_grade(class);
    let ramps = f32::from(u8::from(entrances[0])) + f32::from(u8::from(entrances[1]));
    if ramps == 0.0 {
        return (depth, grade);
    }
    // The depth at which the ramps and the covered stretch exactly fill the run.
    let fits = (total - MIN_COVERED_M) * grade / ramps;
    if fits >= depth {
        return (depth, grade);
    }
    (fits.max(min_roofable_depth(class)), grade)
}

/// The centre line with a vertex inserted where each entrance ramp reaches full
/// depth.
///
/// Returns the line unchanged when it has no entrance, or is too short to reach
/// depth at all — in the second case it never gets a roof whatever is inserted,
/// which is correct: a run shorter than its own two ramps is a dip, not a tunnel.
fn with_ramp_breaks(
    centre: &[[f32; 2]],
    depth: f32,
    grade: f32,
    entrances: OpenEnds,
) -> Vec<[f32; 2]> {
    let ramp = ramp_m(depth, grade);
    let total: f32 = centre
        .windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .sum();
    let mut cuts: Vec<f32> = Vec::with_capacity(2);
    if entrances[0] {
        cuts.push(ramp);
    }
    if entrances[1] {
        cuts.push(total - ramp);
    }
    // A cut outside the run, or on a vertex already, buys nothing.
    cuts.retain(|s| *s > 0.5 && *s < total - 0.5);
    if cuts.is_empty() {
        return centre.to_vec();
    }
    cuts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut out: Vec<[f32; 2]> = Vec::with_capacity(centre.len() + cuts.len());
    let mut walked = 0.0f32;
    let mut next = 0usize;
    out.push(centre[0]);
    for w in centre.windows(2) {
        let (a, b) = (w[0], w[1]);
        let seg = (b[0] - a[0]).hypot(b[1] - a[1]);
        while next < cuts.len() && cuts[next] <= walked + seg {
            let t = ((cuts[next] - walked) / seg.max(1e-6)).clamp(0.0, 1.0);
            let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            // Not on top of a vertex that is already there.
            if out
                .last()
                .is_none_or(|q| (p[0] - q[0]).hypot(p[1] - q[1]) > 0.05)
            {
                out.push(p);
            }
            next += 1;
        }
        walked += seg;
        out.push(b);
    }
    out
}

/// Length of the entrance ramp for a tunnel of this depth and grade, in metres.
#[must_use]
pub fn ramp_m(depth: f32, grade: f32) -> f32 {
    depth / grade.max(1e-3)
}

/// Depth at distance `s` along a run whose full depth is `depth` and whose total
/// length is `total`, given which ends are entrances.
///
/// The two ramps are applied independently and the *shallower* of the two wins,
/// so a run shorter than two ramps sags to a dish rather than crossing itself.
fn depth_at(s: f32, total: f32, depth: f32, grade: f32, entrances: OpenEnds) -> f32 {
    let ramp = ramp_m(depth, grade).max(1e-3);
    let mut d = depth;
    if entrances[0] {
        d = d.min(depth * (s / ramp).clamp(0.0, 1.0));
    }
    if entrances[1] {
        d = d.min(depth * ((total - s) / ramp).clamp(0.0, 1.0));
    }
    d
}

/// How far apart two tunnel runs' ends may be and still be one tunnel, in metres.
///
/// **This number is the difference between a tunnel and a row of scoops.**
/// Measured on the live Groningen tile 2026-09-15: the southern ring arrives as
/// runs of 172–242 m whose ends stand **16 m apart** — they are separate features
/// with separately generalised geometry, not a shared node. Ramping at every one
/// of those ends made each run dive to 6 m and climb back to grade, so the ring
/// read as a series of dishes in a park rather than as one sunken road, which is
/// what two captures showed before this was measured.
///
/// Twenty-five metres takes those in and is still well under the shortest gap
/// between unrelated entrances.
const JOIN_RADIUS_M: f32 = 25.0;

/// How nearly two runs must point the same way to be joined, as a dot product.
///
/// `cos 35°`. Proximity alone is not enough: a cycle underpass ending beside a
/// road tunnel's entrance would suppress a real opening and leave a carriageway
/// disappearing into the ground with no ramp down to it. Sign-free, because two
/// runs meeting head-to-head are as much a continuation as head-to-tail.
const JOIN_COS: f32 = 0.819;

/// Which ends of each line are genuine entrances.
///
/// An end is a entrance when the line ends there and **no other tunnel run carries
/// on from it** — near enough ([`JOIN_RADIUS_M`]) and pointing the same way
/// ([`JOIN_COS`]). Shortbread merges ways by attribute, so a tunnel whose tagging
/// changes half way arrives as several lines end to end; ramping at each of those
/// would build entrances facing each other in the middle of a tunnel.
fn open_ends(lines: &[Vec<[f32; 2]>]) -> Vec<OpenEnds> {
    // (point, unit direction along the line at that end), per run per end.
    let dir = |a: [f32; 2], b: [f32; 2]| -> [f32; 2] {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy).max(1e-6);
        [dx / len, dy / len]
    };
    let ends: Vec<Option<[([f32; 2], [f32; 2]); 2]>> = lines
        .iter()
        .map(|l| {
            (l.len() >= 2).then(|| {
                [
                    (l[0], dir(l[0], l[1])),
                    (l[l.len() - 1], dir(l[l.len() - 2], l[l.len() - 1])),
                ]
            })
        })
        .collect();
    ends.iter()
        .enumerate()
        .map(|(i, mine)| {
            let Some(mine) = mine else {
                return [false, false];
            };
            let joined = |k: usize| -> bool {
                let (p, d) = mine[k];
                ends.iter().enumerate().any(|(j, other)| {
                    if j == i {
                        return false;
                    }
                    let Some(other) = other else {
                        return false;
                    };
                    other.iter().any(|(q, e)| {
                        let gap = (q[0] - p[0]).hypot(q[1] - p[1]);
                        if gap > JOIN_RADIUS_M || (d[0] * e[0] + d[1] * e[1]).abs() < JOIN_COS {
                            return false;
                        }
                        // **Beside is not the same as beyond**, and conflating them
                        // is what stopped a dual carriageway ever reaching the
                        // surface. The two halves of one end side by side at the
                        // entrance, about eighteen metres apart and pointing the
                        // same way — so each saw the other inside the radius,
                        // each concluded it was a way somebody had split in two,
                        // and both stayed at full depth. Measured over Groningen:
                        // every 7 m road tunnel in the streamed map was covered
                        // from edge to edge with no entrance anywhere, which reads
                        // exactly like a tunnel that was never built.
                        //
                        // A real split continues the line: the other end lies
                        // *along* the direction of travel. Two carriageways lie
                        // across it. Ends that coincide are the ordinary case of a
                        // shared node, where the offset has no direction to test.
                        gap <= SAME_POINT_M || {
                            let (ox, oy) = ((q[0] - p[0]) / gap, (q[1] - p[1]) / gap);
                            (ox * d[0] + oy * d[1]).abs() >= JOIN_ALONG_COS
                        }
                    })
                })
            };
            [!joined(0), !joined(1)]
        })
        .collect()
}

/// Two ends closer than this are the same point, and the direction between them
/// means nothing — the ordinary case of two ways sharing a node.
const SAME_POINT_M: f32 = 1.5;

/// How nearly the offset between two ends must lie *along* the road before they
/// count as one way split in two rather than two roads side by side.
///
/// About 45°, which separates the two cases with room to spare: a split continues
/// the line (offset parallel to travel, cosine near 1) and a pair of carriageways
/// sits across it (offset perpendicular, cosine near 0).
const JOIN_ALONG_COS: f32 = 0.7;

/// A corridor's two offset sides, per segment.
///
/// Per segment rather than per vertex: a shared offset vertex is a mitre, and a
/// mitre runs to infinity at a sharp turn — the spike `navigation::ribbon`
/// documents shooting across a junction. Each segment gets its own butt-ended
/// pair and the notch left on the outside of a bend is filled with a triangle,
/// which can neither spike nor pinch.
struct Sides {
    left: Vec<[[f32; 2]; 2]>,
    right: Vec<[[f32; 2]; 2]>,
}

fn offset_sides(centre: &[[f32; 2]], half: f32) -> Sides {
    let mut left = Vec::with_capacity(centre.len().saturating_sub(1));
    let mut right = Vec::with_capacity(centre.len().saturating_sub(1));
    for i in 0..centre.len().saturating_sub(1) {
        let (a, b) = (centre[i], centre[i + 1]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy).max(1e-6);
        // Left normal in tile space (x east, y south).
        let n = [-dy / len * half, dx / len * half];
        left.push([[a[0] + n[0], a[1] + n[1]], [b[0] + n[0], b[1] + n[1]]]);
        right.push([[a[0] - n[0], a[1] - n[1]], [b[0] - n[0], b[1] - n[1]]]);
    }
    Sides { left, right }
}

/// Mesh under construction: positions, indices, UVs, normals, rungs.
#[derive(Default)]
struct Build {
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
    uvs: Vec<[f32; 2]>,
    normals: Vec<[f32; 3]>,
    rungs: Vec<f32>,
}

impl Build {
    fn tri(&mut self, p: [[f32; 3]; 3], uv: [[f32; 2]; 3], n: [f32; 3], rung: f32) {
        let base = self.positions.len() as u32;
        for k in 0..3 {
            self.positions.push(p[k]);
            self.uvs.push(uv[k]);
            self.normals.push(n);
            self.rungs.push(rung);
        }
        self.indices.extend([base, base + 1, base + 2]);
    }

    /// A quad as two triangles, wound `a b c` / `a c d`.
    fn quad(&mut self, p: [[f32; 3]; 4], uv: [[f32; 2]; 4], n: [f32; 3], rung: f32) {
        self.tri([p[0], p[1], p[2]], [uv[0], uv[1], uv[2]], n, rung);
        self.tri([p[0], p[2], p[3]], [uv[0], uv[2], uv[3]], n, rung);
    }

    fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn into_mesh(self, group: SurfaceGroup) -> SurfaceMesh {
        SurfaceMesh {
            group,
            positions: self.positions,
            indices: self.indices,
            uvs: self.uvs,
            normals: self.normals,
            colors: Vec::new(),
            rungs: self.rungs,
        }
    }
}

/// Upward normal, for the deck.
const UP: [f32; 3] = [0.0, 1.0, 0.0];
/// Downward, for the ceiling.
const DOWN: [f32; 3] = [0.0, -1.0, 0.0];

/// Build every tunnel in one tile.
///
/// `tile_m` is the tile's side in metres, as everywhere else in this crate;
/// `max_bytes` cuts the result into several meshes the way the other builders do,
/// because bevy's upload budget writes one whole asset per frame and cannot
/// spread a single huge one (`docs/notes/upload-bursts.md`).
#[must_use]
pub fn build_tunnels(tile: &TileData, tile_m: f32, max_bytes: Option<usize>) -> TunnelTessellation {
    let mut out = TunnelTessellation::default();
    let Some(meta) = tile.layer("streets") else {
        return out;
    };
    let features = tile.features(meta);
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    // The ways through a building stay on the ground sheet, where
    // `tessellate_roads` draws them. This is the only thing the buildings layer
    // decides here: what is covered and what is an open ramp is a question about
    // depth (see the roof rule below), not about what happens to stand overhead.
    let passages = building_passages(tile, &features);

    // Every tunnel line in the tile, in metres, before anything is built: the
    // entrance test needs to see all of them at once. Each carries which of its
    // ends are the *line's* own, as against a cut by the tile border.
    let mut runs: Vec<(usize, Vec<[f32; 2]>, OpenEnds)> = Vec::new();
    for (fi, feature) in features.iter().enumerate() {
        if !underground(feature) || passages.contains(&fi) {
            continue;
        }
        let mut lines: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => lines.push(ls),
            Geometry::MultiLineString(mls) => lines.extend(mls.iter()),
            _ => continue,
        }
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
                // A run the tile border cut carries on into the neighbour, so
                // only an end that is still the line's own end can be a opening.
                let own = [run.first() == Some(&head), run.last() == Some(&tail)];
                let metres: Vec<[f32; 2]> =
                    run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect();
                if metres.len() < 2 {
                    continue;
                }
                runs.push((fi, metres, own));
            }
        }
    }
    if runs.is_empty() {
        return out;
    }

    // An end is a entrance when the line ends there, the border did not cut it,
    // and no second tunnel run ends on the same point.
    let lines: Vec<Vec<[f32; 2]>> = runs.iter().map(|(_, l, _)| l.clone()).collect();
    let unshared = open_ends(&lines);

    let mut deck = Build::default();
    // The class the open deck buffer is drawing markings for. A batch takes the
    // class of the run that opened it, exactly as the ground road sheet's
    // per-group meshes do — the alternative is a mesh per class per cell, for a
    // difference that is which dashes are painted on a carriageway nobody sees
    // two of at once.
    let mut road_class_of_tunnel: Option<RoadClass> = None;
    let mut shell = Build::default();
    let mut meshes: Vec<SurfaceMesh> = Vec::new();

    for (idx, (fi, centre, own)) in runs.iter().enumerate() {
        let feature = &features[*fi];
        let flags = StreetFlags {
            oneway: flag_of(feature, "oneway") || flag_of(feature, "oneway_reverse"),
            oneway_reverse: flag_of(feature, "oneway_reverse"),
            link: flag_of(feature, "link"),
        };
        let kind = kind_of(feature).unwrap_or("");
        let Some(class) = road_class(kind, flags) else {
            continue;
        };
        let half = road_width_m(kind, flags) * 0.5;
        if half <= 0.0 {
            continue;
        }
        let entrances = [own[0] && unshared[idx][0], own[1] && unshared[idx][1]];
        let run_len: f32 = centre
            .windows(2)
            .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
            .sum();
        let (depth, grade) = depth_for_run(class, run_len, entrances);

        // **Put a vertex where the ramp ends, or the tunnel is never roofed.**
        //
        // Everything below — the deck level, and with it whether a lid goes on —
        // is evaluated *at the centre line's vertices*, and a generalised tunnel
        // has very few. At [`MAX_GRADE`] a [`LEVEL_DEPTH_M`] tunnel ramps for
        // 117 m, so a 238 m one is at full depth for about **four metres**; with
        // five vertices spread over 238 m, none of them lands in that window.
        // Measured over Groningen's southern ring on 2026-09-20: six carriageway
        // tunnels, 171 m to 241 m long, and **every one reported 0 of its
        // vertices roofed** — so the ground was cut open along the whole of each
        // and the ring was drawn as an open canyon through the city.
        //
        // The break points are where the shape actually changes, so this adds two
        // vertices and no approximation: between them the ramp is linear and
        // needed none, and past them the tunnel is flat.
        let centre = &with_ramp_breaks(centre, depth, grade, entrances);

        // Distance along the centre line at each vertex, and the deck level there.
        let mut travelled = Vec::with_capacity(centre.len());
        let mut total = 0.0f32;
        for i in 0..centre.len() {
            if i > 0 {
                total += (centre[i][0] - centre[i - 1][0]).hypot(centre[i][1] - centre[i - 1][1]);
            }
            travelled.push(total);
        }
        let road_y: Vec<f32> = travelled
            .iter()
            .map(|&s| -depth_at(s, total, depth, grade, entrances))
            .collect();

        let clearance = if matches!(class, RoadClass::Path) {
            PATH_CLEARANCE_M
        } else {
            CLEARANCE_M
        };
        // **A tunnel is covered. The open part is the ramp, and only the ramp.**
        //
        // This was briefly gated on a building standing over the point, on the
        // reasoning that the tiles cannot say where a lid is but can say where a
        // building is. That was wrong, and wrong in a way that took days to see:
        // Groningen's ring runs under a *park*, so nothing was ever covered, every
        // tunnel was drawn as an open canyon, and every capture of a "tunnel" was a
        // picture of a trench. The observation that pushed me there — roofing by
        // depth "buried the whole ring, with nothing to see" — is not a bug
        // report. It is what a tunnel looks like from above.
        //
        // What keeps a entrance visible is [`ROOF_AT_DEPTH_M`], not the buildings
        // layer: the ramp has not reached depth, so it takes no roof, so the
        // ground is cut away over it and you can see the road descend and go
        // under. That is the whole of the shape, and it needs no second source.
        //
        // One answer per vertex, computed once: the ceiling below and the hole cut
        // in the ground both read it, so they cannot disagree about where the
        // opening is — which is what two expressions of one rule eventually do.
        let roofed: Vec<bool> = road_y
            .iter()
            .map(|&y| y <= -(depth - ROOF_AT_DEPTH_M) && y + clearance <= CEILING_MAX_Y)
            .collect();
        let sides = offset_sides(centre, half);
        road_class_of_tunnel.get_or_insert(class);
        let tex = class.texture_length_m().max(1.0);
        // **A carriageway metres underground asks the depth buffer for nothing.**
        //
        // This read `road_rung(class, half * 2.0, true, idx)` until 2026-09-18, and
        // that is not the depth ladder — it stopped being one on 2026-08-26, when
        // the ladder was collapsed to `ROAD_DEPTH_RUNGS` = 2 and `road_rung`
        // survived only as the order the *cut* runs in. It still returns 0..399, and
        // with `bridge: true` it returns the top of that range. The deck therefore
        // asked to be drawn ~25,900 depth units in front of itself against a surface
        // road's 512, which the shader spends as a fraction of the fragment's own
        // depth: 0.3 m nearer at 100 m away, 3.1 m at a kilometre, and past about
        // 2.3 km more than the whole `LEVEL_DEPTH_M` the tunnel is buried by — so
        // the tunnelled road drew *over* the ground it runs under, worse the further
        // away you looked. The walls and the roof carried 0 throughout, which is why
        // the artefact was the carriageway alone.
        //
        // Zero is not a smaller correction of the old number, it is the right
        // answer: a tunnel is held under the ground by metres of real geometry, so
        // there is no coplanar fight here for a bias to settle. `road_depth_rung`
        // rather than a bare literal, so this reads as a rung and moves with the
        // band if the band ever grows.
        let rung = road_depth_rung(false);

        for i in 0..sides.left.len() {
            let (l0, l1) = (sides.left[i][0], sides.left[i][1]);
            let (r0, r1) = (sides.right[i][0], sides.right[i][1]);
            let (y0, y1) = (road_y[i], road_y[i + 1]);
            let (v0, v1) = (travelled[i] / tex, travelled[i + 1] / tex);

            // Deck. `u` runs across the carriageway so the lane markings register
            // with the geometry, exactly as a surface road's do.
            deck.quad(
                [
                    [l0[0], y0, l0[1]],
                    [r0[0], y0, r0[1]],
                    [r1[0], y1, r1[1]],
                    [l1[0], y1, l1[1]],
                ],
                [[0.0, v0], [1.0, v0], [1.0, v1], [0.0, v1]],
                UP,
                rung,
            );

            // Walls, inward-facing: the outer face is buried in the ground the
            // tunnel is cut into and no camera can reach it.
            let n_left = wall_normal(l0, l1, true);
            shell.quad(
                [
                    [l0[0], y0, l0[1]],
                    [l1[0], y1, l1[1]],
                    [l1[0], WALL_TOP_Y, l1[1]],
                    [l0[0], WALL_TOP_Y, l0[1]],
                ],
                [[v0, 0.0], [v1, 0.0], [v1, 1.0], [v0, 1.0]],
                n_left,
                0.0,
            );
            let n_right = wall_normal(r0, r1, false);
            shell.quad(
                [
                    [r1[0], y1, r1[1]],
                    [r0[0], y0, r0[1]],
                    [r0[0], WALL_TOP_Y, r0[1]],
                    [r1[0], WALL_TOP_Y, r1[1]],
                ],
                [[v1, 0.0], [v0, 0.0], [v0, 1.0], [v1, 1.0]],
                n_right,
                0.0,
            );

            // Roof, where one fits under the ground. Absent over the approach,
            // which is what opens the opening — see the module note.
            let (c0, c1) = (y0 + clearance, y1 + clearance);
            if roofed[i] && roofed[i + 1] {
                shell.quad(
                    [
                        [l0[0], c0, l0[1]],
                        [l1[0], c1, l1[1]],
                        [r1[0], c1, r1[1]],
                        [r0[0], c0, r0[1]],
                    ],
                    [[0.0, v0], [0.0, v1], [1.0, v1], [1.0, v0]],
                    DOWN,
                    0.0,
                );
            }

            // The notch on the outside of a bend, in the deck and in both walls.
            if i + 1 < sides.left.len() {
                let p = centre[i + 1];
                let y = road_y[i + 1];
                let v = v1;
                let next_l = sides.left[i + 1][0];
                let next_r = sides.right[i + 1][0];
                // Which way the corner turns decides which side has the gap; the
                // other side's triangle is degenerate to a hair and costs three
                // vertices to keep the code one case.
                deck.tri(
                    [
                        [p[0], y, p[1]],
                        [l1[0], y, l1[1]],
                        [next_l[0], y, next_l[1]],
                    ],
                    [[0.5, v], [0.0, v], [0.0, v]],
                    UP,
                    rung,
                );
                deck.tri(
                    [
                        [p[0], y, p[1]],
                        [next_r[0], y, next_r[1]],
                        [r1[0], y, r1[1]],
                    ],
                    [[0.5, v], [1.0, v], [1.0, v]],
                    UP,
                    rung,
                );
                // Walls: a vertical quad closing the gap between the two offset
                // points. Without it a bend is a slot you can see the void
                // through from inside the tunnel.
                shell.quad(
                    [
                        [l1[0], y, l1[1]],
                        [next_l[0], y, next_l[1]],
                        [next_l[0], WALL_TOP_Y, next_l[1]],
                        [l1[0], WALL_TOP_Y, l1[1]],
                    ],
                    [[v, 0.0], [v, 0.0], [v, 1.0], [v, 1.0]],
                    n_left,
                    0.0,
                );
                shell.quad(
                    [
                        [next_r[0], y, next_r[1]],
                        [r1[0], y, r1[1]],
                        [r1[0], WALL_TOP_Y, r1[1]],
                        [next_r[0], WALL_TOP_Y, next_r[1]],
                    ],
                    [[v, 0.0], [v, 0.0], [v, 1.0], [v, 1.0]],
                    n_right,
                    0.0,
                );
            }
        }

        // The roofless stretches, as plan corridors for the caller to cut out of
        // the ground. Keyed on the roof being missing rather than on the entrance
        // flag, so the hole and the absent ceiling are decided by one test and
        // cannot disagree about where the opening is.
        out.openings.extend(opening_rings(centre, &roofed, half));

        out.tunnels.push(Tunnel {
            centre: centre.clone(),
            road_y,
            half_w: half,
            clearance,
            covered: roofed.clone(),
        });

        // Cut the meshes loose as they fill, the same contract every other
        // builder in this crate keeps.
        if let Some(limit) = max_bytes {
            if deck.positions.len() * VERTEX_BYTES >= limit && !deck.is_empty() {
                let group = SurfaceGroup::TunnelRoad(road_class_of_tunnel.take().unwrap_or(class));
                meshes.push(std::mem::take(&mut deck).into_mesh(group));
            }
            if shell.positions.len() * VERTEX_BYTES >= limit && !shell.is_empty() {
                meshes.push(std::mem::take(&mut shell).into_mesh(SurfaceGroup::TunnelStructure));
            }
        }
    }

    if !deck.is_empty() {
        let group = SurfaceGroup::TunnelRoad(road_class_of_tunnel.unwrap_or(RoadClass::Minor));
        meshes.push(deck.into_mesh(group));
    }
    if !shell.is_empty() {
        meshes.push(shell.into_mesh(SurfaceGroup::TunnelStructure));
    }
    // One trench, not two slots with a wall of earth between them.
    //
    // A dual carriageway is two ways in OSM and so two tunnels here, each opening
    // only its own width — which leaves the median standing as ground, with
    // surface traffic driving along it, down the middle of what is one box in the
    // world. Merged by convex hull, which is exactly right for two parallel
    // corridors; the parallelism test is what guards it, because the hull of two
    // corridors that diverge is a wedge of ground taken away for nothing.
    out.openings = merge_parallel_openings(std::mem::take(&mut out.openings));
    out.meshes = meshes;
    out
}

/// Farthest apart two corridors may be and still be one trench, in metres.
///
/// A Dutch dual carriageway's halves are mapped 10–20 m apart. Thirty takes those
/// and stops well short of the next parallel street.
const MEDIAN_M: f32 = 30.0;

/// Replace pairs of near, parallel corridors with the corridor containing both.
fn merge_parallel_openings(openings: Vec<Vec<[f32; 2]>>) -> Vec<Vec<[f32; 2]>> {
    let mut out: Vec<Vec<[f32; 2]>> = Vec::with_capacity(openings.len());
    let mut merged = vec![false; openings.len()];
    for i in 0..openings.len() {
        if merged[i] {
            continue;
        }
        let mut ring = openings[i].clone();
        for j in (i + 1)..openings.len() {
            if merged[j] || !parallel_and_near(&ring, &openings[j]) {
                continue;
            }
            let mut both = ring.clone();
            both.extend_from_slice(&openings[j]);
            ring = convex_hull(&both);
            merged[j] = true;
        }
        out.push(ring);
    }
    out
}

/// Whether two corridors run alongside each other closely enough, and in the same
/// direction, to be one trench.
fn parallel_and_near(a: &[[f32; 2]], b: &[[f32; 2]]) -> bool {
    let (ba, bb) = (bbox_of_ring(a), bbox_of_ring(b));
    if ba[0] - MEDIAN_M > bb[2]
        || bb[0] - MEDIAN_M > ba[2]
        || ba[1] - MEDIAN_M > bb[3]
        || bb[1] - MEDIAN_M > ba[3]
    {
        return false;
    }
    let (da, db) = (principal_axis(a), principal_axis(b));
    // Sign-free: the two halves of a dual carriageway run opposite ways.
    (da[0] * db[0] + da[1] * db[1]).abs() >= JOIN_COS
}

/// The long axis of a ring, as a unit vector: its two most distant points.
fn principal_axis(ring: &[[f32; 2]]) -> [f32; 2] {
    let (mut best, mut pair) = (0.0f32, ([0.0f32; 2], [0.0f32; 2]));
    for (i, p) in ring.iter().enumerate() {
        for q in &ring[i + 1..] {
            let d = (p[0] - q[0]).hypot(p[1] - q[1]);
            if d > best {
                best = d;
                pair = (*p, *q);
            }
        }
    }
    let (dx, dy) = (pair.1[0] - pair.0[0], pair.1[1] - pair.0[1]);
    let len = dx.hypot(dy).max(1e-6);
    [dx / len, dy / len]
}

/// Convex hull (Andrew's monotone chain), closed.
fn convex_hull(pts: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut p: Vec<[f32; 2]> = pts.to_vec();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup_by(|a, b| a == b);
    if p.len() < 3 {
        return pts.to_vec();
    }
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut hull: Vec<[f32; 2]> = Vec::with_capacity(p.len() * 2);
    for &q in p.iter().chain(p.iter().rev()) {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], q) <= 0.0 {
            hull.pop();
        }
        hull.push(q);
    }
    hull.pop();
    if let Some(&head) = hull.first() {
        hull.push(head);
    }
    hull
}

/// Inward normal of a wall segment. `left` picks which side of the direction the
/// interior is on.
fn wall_normal(a: [f32; 2], b: [f32; 2], left: bool) -> [f32; 3] {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dy).max(1e-6);
    let (nx, ny) = (-dy / len, dx / len);
    if left {
        [-nx, 0.0, -ny]
    } else {
        [nx, 0.0, ny]
    }
}

/// The plan corridors of every roofless stretch of one run.
///
/// **Every stretch, not the two ends.** This used to take the run before the
/// first roofed vertex and the run after the last, which is the right shape only
/// while the roof is a single band in the middle — as it was when a roof went
/// over whatever had reached depth. Now that the roof follows the *buildings*
/// over the tunnel, an open stretch can be anywhere and there can be several, and a
/// tunnel whose first and last vertices happen to be built over came back with no
/// opening at all.
///
/// What that failure looked like is worth remembering: the ground was cut, by
/// tens of thousands of square metres, in 10 m fragments at the ends of tunnels,
/// while the 200 m of open trench between them stayed roofed by grass. Every
/// capture showed an unbroken park with a one-pixel line of tunnel wall standing
/// 8 mm proud of it (2026-09-15).
fn opening_rings(centre: &[[f32; 2]], roofed: &[bool], half: f32) -> Vec<Vec<[f32; 2]>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < centre.len() {
        if roofed[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < centre.len() && !roofed[i] {
            i += 1;
        }
        // One vertex of overlap into the roofed part at each side, so the hole
        // meets the ceiling rather than leaving a sliver of ground on the lip of
        // the opening — the same reason the corridor is cut wider than the
        // carriageway.
        let lo = start.saturating_sub(1);
        let hi = (i).min(centre.len() - 1);
        if hi > lo {
            out.extend(mouth_ring(&centre[lo..=hi], half));
        }
    }
    out
}

/// One corridor as a closed ring, a little wider than the carriageway.
fn mouth_ring(run: &[[f32; 2]], half: f32) -> Option<Vec<[f32; 2]>> {
    if run.len() < 2 {
        return None;
    }
    let run: Vec<[f32; 2]> = run.to_vec();
    // A little wider than the carriageway: the walls stand at the carriageway
    // edge and the ground has to come away from *outside* them, or a hairline of
    // sheet is left standing on top of the wall.
    let sides = offset_sides(&run, half + 0.5);
    let mut ring: Vec<[f32; 2]> = Vec::with_capacity(run.len() * 2 + 1);
    for s in &sides.left {
        ring.push(s[0]);
    }
    if let Some(last) = sides.left.last() {
        ring.push(last[1]);
    }
    for s in sides.right.iter().rev() {
        ring.push(s[1]);
    }
    if let Some(first) = sides.right.first() {
        ring.push(first[0]);
    }
    let head = *ring.first()?;
    ring.push(head);
    Some(ring)
}

/// Take the ground away over every opening.
///
/// **Without this there is no tunnel**, only a room with a lid over its own
/// entrance: the approach is roofless by construction ([`opening_rings`]) and the
/// ground sheet is what would otherwise roof it. It is the same operation
/// `road_stitch` performs on the road ribbons, for the same reason and on the
/// same integer millimetre lattice — a boundary produced by subtracting one
/// shape from another is built from the subtracted shape's own vertices, so the
/// hole's edge and the wall standing in it are the same line rather than two
/// opinions about one line.
///
/// What is cut: every sheet that is *ground*. Not the tunnel's own deck, walls and
/// roof, obviously; not a bridge deck, which is a structure that may legitimately
/// cross an open cut; and not the volumetric groups, because a third of a tree is
/// not a smaller tree — a canopy standing in the opening is left standing, which
/// is wrong in the small and much less wrong than the alternative.
pub fn cut_ground(meshes: &mut [SurfaceMesh], openings: &[Vec<[f32; 2]>]) {
    let clips = clips_of(openings);
    if clips.is_empty() {
        return;
    }
    let mut overlay: Overlay<i32> = Overlay::new(64);
    for mesh in meshes.iter_mut() {
        if !is_ground(mesh.group) {
            continue;
        }
        cut_mesh(mesh, &clips, &mut overlay);
    }
}

/// Take the same openings out of one sheet that is not a vector surface at all —
/// a basemap clipmap tile.
///
/// **The hole in the vector ground is not the whole hole.** Under those surfaces
/// lies the raster basemap, three centimetres down and painted with a picture of
/// the same streets, so cutting only the vector sheet opens the tunnel onto a copy
/// of the map rather than onto the trench: measured over the DUO cut, the
/// carriageway was seven metres below a plausible-looking road surface with the
/// city's own cars drawn on it. Every clipmap level is above the deck, so every
/// level that a opening falls in needs the hole too — which is why this takes one
/// mesh rather than a slice, and why [`is_ground`] has no say here: the caller
/// already knows what it is handing over.
///
/// Coordinates are the mesh's own local XZ, so the caller translates the openings
/// into the tile's frame before calling. Answers whether anything was removed,
/// which is what lets a tile that no longer overlaps go back to the shared quad.
pub fn cut_sheet(mesh: &mut SurfaceMesh, openings: &[Vec<[f32; 2]>]) -> bool {
    let clips = clips_of(openings);
    if clips.is_empty() {
        return false;
    }
    let mut overlay: Overlay<i32> = Overlay::new(64);
    cut_mesh(mesh, &clips, &mut overlay)
}

/// The openings as i_overlay clip contours, each with the bounding box that lets a
/// triangle skip it.
fn clips_of(openings: &[Vec<[f32; 2]>]) -> Vec<(Bbox, Vec<IntPoint<i32>>)> {
    openings
        .iter()
        .filter(|ring| ring.len() >= 4)
        .map(|ring| {
            let pts: Vec<IntPoint<i32>> = ring
                .iter()
                .take(ring.len() - 1)
                .map(|p| IntPoint::new(to_mm(p[0]), to_mm(p[1])))
                .collect();
            (bbox_of_ring(ring), pts)
        })
        .filter(|(_, pts)| pts.len() >= 3)
        .collect()
}

/// Whether a group is a sheet lying on the ground, and so something the opening
/// has to be taken out of.
fn is_ground(group: SurfaceGroup) -> bool {
    !matches!(
        group,
        SurfaceGroup::TunnelRoad(_)
            | SurfaceGroup::TunnelStructure
            | SurfaceGroup::BridgeDeck
            | SurfaceGroup::Canopy
            | SurfaceGroup::Trunk
            | SurfaceGroup::Furniture
    )
}

type Bbox = [f32; 4];

fn bbox_of_ring(ring: &[[f32; 2]]) -> Bbox {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for p in ring {
        b[0] = b[0].min(p[0]);
        b[1] = b[1].min(p[1]);
        b[2] = b[2].max(p[0]);
        b[3] = b[3].max(p[1]);
    }
    b
}

fn bbox_overlaps(a: &Bbox, b: &Bbox) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

/// Integer millimetres of tile-local metres — `road_stitch`'s lattice, for the
/// reason its module note gives.
fn to_mm(v: f32) -> i32 {
    (v * 1000.0).round() as i32
}

fn from_mm(v: i32) -> f32 {
    v as f32 / 1000.0
}

/// Barycentric weights of `p` in `tri`, as `road_stitch` computes them: the way
/// every attribute of a cut triangle is carried over rather than re-derived.
fn barycentric(tri: [[f32; 2]; 3], p: [f32; 2]) -> [f32; 3] {
    let (x1, y1) = (tri[0][0], tri[0][1]);
    let (x2, y2) = (tri[1][0], tri[1][1]);
    let (x3, y3) = (tri[2][0], tri[2][1]);
    let det = (y2 - y3) * (x1 - x3) + (x3 - x2) * (y1 - y3);
    if det.abs() < 1e-12 {
        return [1.0, 0.0, 0.0];
    }
    let l1 = ((y2 - y3) * (p[0] - x3) + (x3 - x2) * (p[1] - y3)) / det;
    let l2 = ((y3 - y1) * (p[0] - x3) + (x1 - x3) * (p[1] - y3)) / det;
    [l1, l2, 1.0 - l1 - l2]
}

/// Subtract the openings from one mesh, in place.
fn cut_mesh(
    mesh: &mut SurfaceMesh,
    clips: &[(Bbox, Vec<IntPoint<i32>>)],
    overlay: &mut Overlay<i32>,
) -> bool {
    let has_uv = !mesh.uvs.is_empty();
    let has_n = !mesh.normals.is_empty();
    let has_c = !mesh.colors.is_empty();
    let has_r = !mesh.rungs.is_empty();
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(mesh.positions.len());
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colors: Vec<[f32; 4]> = Vec::new();
    let mut rungs: Vec<f32> = Vec::new();
    let mut indices: Vec<u32> = Vec::with_capacity(mesh.indices.len());
    let mut touched = false;

    for t in mesh.indices.chunks_exact(3) {
        let vi = [t[0] as usize, t[1] as usize, t[2] as usize];
        let vs = [
            mesh.positions[vi[0]],
            mesh.positions[vi[1]],
            mesh.positions[vi[2]],
        ];
        let flat = [
            [vs[0][0], vs[0][2]],
            [vs[1][0], vs[1][2]],
            [vs[2][0], vs[2][2]],
        ];
        let tb = bbox_of_ring(&flat);
        let subj = [
            IntPoint::new(to_mm(flat[0][0]), to_mm(flat[0][1])),
            IntPoint::new(to_mm(flat[1][0]), to_mm(flat[1][1])),
            IntPoint::new(to_mm(flat[2][0]), to_mm(flat[2][1])),
        ];

        // Copy a vertex across unchanged, attributes and all.
        let push_src = |positions: &mut Vec<[f32; 3]>,
                        uvs: &mut Vec<[f32; 2]>,
                        normals: &mut Vec<[f32; 3]>,
                        colors: &mut Vec<[f32; 4]>,
                        rungs: &mut Vec<f32>,
                        k: usize| {
            positions.push(mesh.positions[vi[k]]);
            if has_uv {
                uvs.push(mesh.uvs[vi[k]]);
            }
            if has_n {
                normals.push(mesh.normals[vi[k]]);
            }
            if has_c {
                colors.push(mesh.colors[vi[k]]);
            }
            if has_r {
                rungs.push(mesh.rungs[vi[k]]);
            }
        };

        overlay.clear();
        let mut any = false;
        for (cb, contour) in clips {
            if !bbox_overlaps(&tb, cb) {
                continue;
            }
            overlay.add_contour(contour, ShapeType::Clip);
            any = true;
        }
        if !any {
            let base = positions.len() as u32;
            for k in 0..3 {
                push_src(
                    &mut positions,
                    &mut uvs,
                    &mut normals,
                    &mut colors,
                    &mut rungs,
                    k,
                );
            }
            indices.extend([base, base + 1, base + 2]);
            continue;
        }
        touched = true;
        overlay.add_contour(&subj, ShapeType::Subject);
        let kept = overlay.overlay(OverlayRule::Difference, FillRule::NonZero);
        for shape in &kept {
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
                let mut out = [[0.0f32; 3]; 3];
                let mut w3 = [[0.0f32; 3]; 3];
                for (k, idx) in c.iter().enumerate() {
                    let p = [flat_xy[idx * 2] as f32, flat_xy[idx * 2 + 1] as f32];
                    let w = barycentric(flat, p);
                    w3[k] = w;
                    out[k] = [
                        p[0],
                        w[0] * vs[0][1] + w[1] * vs[1][1] + w[2] * vs[2][1],
                        p[1],
                    ];
                }
                // Earcut keeps its input's orientation, which the difference does
                // not guarantee; ground geometry is single-sided and faces up.
                let cross = (out[1][0] - out[0][0]) * (out[2][2] - out[0][2])
                    - (out[1][2] - out[0][2]) * (out[2][0] - out[0][0]);
                if cross.abs() < f32::EPSILON {
                    continue;
                }
                let order = if cross < 0.0 { [0, 1, 2] } else { [0, 2, 1] };
                let base = positions.len() as u32;
                for k in order {
                    let w = w3[k];
                    positions.push(out[k]);
                    if has_uv {
                        uvs.push([
                            w[0] * mesh.uvs[vi[0]][0]
                                + w[1] * mesh.uvs[vi[1]][0]
                                + w[2] * mesh.uvs[vi[2]][0],
                            w[0] * mesh.uvs[vi[0]][1]
                                + w[1] * mesh.uvs[vi[1]][1]
                                + w[2] * mesh.uvs[vi[2]][1],
                        ]);
                    }
                    if has_n {
                        normals.push(mesh.normals[vi[0]]);
                    }
                    if has_c {
                        let mut col = [0.0f32; 4];
                        for ch in 0..4 {
                            col[ch] = w[0] * mesh.colors[vi[0]][ch]
                                + w[1] * mesh.colors[vi[1]][ch]
                                + w[2] * mesh.colors[vi[2]][ch];
                        }
                        colors.push(col);
                    }
                    if has_r {
                        rungs.push(mesh.rungs[vi[0]]);
                    }
                }
                indices.extend([base, base + 1, base + 2]);
            }
        }
    }

    if !touched {
        return false;
    }
    mesh.positions = positions;
    mesh.indices = indices;
    mesh.uvs = uvs;
    mesh.normals = normals;
    mesh.colors = colors;
    mesh.rungs = rungs;
    true
}

#[cfg(test)]
mod tests {
    /// **Every tunnel in a tile is drawn as whichever one was built first.**
    ///
    /// `road_class_of_tunnel` is a single `Option<RoadClass>` for the whole call:
    /// set by the first tunnel with `get_or_insert`, and read once at the end for
    /// the group every deck is merged into. A tile with a footpath underpass and a
    /// road tunnel in it therefore draws both with one class's surface — gravel on
    /// a trunk road, or tarmac with lane markings down a cycle underpass.
    ///
    /// Measured over Groningen's ring on 2026-09-20: cell `14/8490/5321` holds
    /// tunnels of seven kinds and emits one `TunnelRoad(Carriageway)`; its
    /// neighbour `14/8491/5321` holds nine and emits one `TunnelRoad(Path)`. The
    /// *same* tunnel changes class at the cell boundary, which is the one place a
    /// viewer is guaranteed to see it.
    ///
    /// `#[ignore]` — reproduced, not yet fixed. The fix is to carry the class on
    /// the deck builder rather than beside it, which means a builder per class or
    /// a group per tunnel, and that is a change to how these meshes are batched.
    #[test]
    #[ignore = "reproduces a known fault: one class for every tunnel in a tile"]
    fn each_tunnel_keeps_its_own_class() {
        let tile = crate::testdata::groningen();
        let out = build_tunnels(&tile, 1471.0, None);

        // What the tile actually asks for.
        let mut wanted: Vec<RoadClass> = Vec::new();
        if let Some(meta) = tile.layer("streets") {
            for feature in tile.features(meta) {
                if !underground(feature) {
                    continue;
                }
                let flags = StreetFlags {
                    oneway: flag_of(feature, "oneway") || flag_of(feature, "oneway_reverse"),
                    oneway_reverse: flag_of(feature, "oneway_reverse"),
                    link: flag_of(feature, "link"),
                };
                if let Some(c) = road_class(kind_of(feature).unwrap_or(""), flags) {
                    if !wanted.contains(&c) {
                        wanted.push(c);
                    }
                }
            }
        }
        if wanted.len() < 2 {
            return; // nothing to tell apart in this fixture
        }

        let mut drawn: Vec<RoadClass> = Vec::new();
        for m in &out.meshes {
            if let SurfaceGroup::TunnelRoad(c) = m.group {
                if !drawn.contains(&c) {
                    drawn.push(c);
                }
            }
        }
        assert!(
            drawn.len() >= 2,
            "the tile has tunnels of {} classes ({wanted:?}) and they are drawn as \
             {drawn:?} — every deck took the first tunnel's class",
            wanted.len()
        );
    }

    use super::*;

    #[test]
    fn a_ramp_reaches_the_ground_at_a_portal_and_full_depth_inside() {
        let depth = LEVEL_DEPTH_M;
        let total = 400.0;
        let entrances = [true, true];
        assert!(depth_at(0.0, total, depth, MAX_GRADE, entrances).abs() < 1e-6);
        assert!((depth_at(total, total, depth, MAX_GRADE, entrances)).abs() < 1e-6);
        assert!((depth_at(total * 0.5, total, depth, MAX_GRADE, entrances) - depth).abs() < 1e-3);
    }

    #[test]
    fn an_end_at_the_tile_border_is_held_at_full_depth() {
        let depth = LEVEL_DEPTH_M;
        let total = 400.0;
        // Only the far end is a entrance, so the border end keeps its depth and the
        // two halves of a tunnel crossing a tile edge meet flush.
        let d = depth_at(0.0, total, depth, MAX_GRADE, [false, true]);
        assert!((d - depth).abs() < 1e-3, "border end surfaced: {d}");
    }

    #[test]
    fn a_short_tunnel_sags_instead_of_crossing_itself() {
        let depth = LEVEL_DEPTH_M;
        // Far shorter than two ramps.
        let total = 40.0;
        let mid = depth_at(total * 0.5, total, depth, MAX_GRADE, [true, true]);
        assert!(mid > 0.0, "an underpass with no depth at all");
        assert!(mid < depth, "a 40 m underpass reached full depth: {mid}");
    }

    #[test]
    fn two_runs_meeting_end_to_end_build_no_portal_between_them() {
        let a = vec![[0.0, 0.0], [50.0, 0.0]];
        let b = vec![[50.0, 0.0], [100.0, 0.0]];
        let ends = open_ends(&[a, b]);
        assert_eq!(ends[0], [true, false], "a tagging split became a entrance");
        assert_eq!(ends[1], [false, true]);
    }

    /// **A dual carriageway must be able to surface.**
    ///
    /// Its two halves end side by side at the entrance, pointing the same way, a
    /// carriageway's width apart. The rule that spots a way somebody has split in
    /// two saw exactly that and concluded both were splits, so neither got an
    /// entrance and every road tunnel in Groningen was covered from edge to edge —
    /// indistinguishable, from outside, from a tunnel that was never built.
    #[test]
    fn two_carriageways_side_by_side_both_get_an_entrance() {
        // Both run east and stop at x = 0; 18 m apart across the direction of
        // travel, which is an ordinary dual carriageway.
        let a = vec![[-60.0, 0.0], [0.0, 0.0]];
        let b = vec![[-60.0, 18.0], [0.0, 18.0]];
        let ends = open_ends(&[a, b]);
        assert_eq!(
            ends[0],
            [true, true],
            "the near carriageway lost its entrance"
        );
        assert_eq!(
            ends[1],
            [true, true],
            "the far carriageway lost its entrance"
        );

        // …while a way split in two along its own line still joins, which is the
        // case the radius was added for.
        let c = vec![[0.0, 0.0], [50.0, 0.0]];
        let d = vec![[50.0, 0.0], [100.0, 0.0]];
        let split = open_ends(&[c, d]);
        assert_eq!(split[0], [true, false]);
        assert_eq!(split[1], [false, true]);

        // And a run that stops 10 m short of another, in line with it, is still a
        // split rather than two ends that happen to be near each other.
        let e = vec![[0.0, 0.0], [50.0, 0.0]];
        let f = vec![[60.0, 0.0], [110.0, 0.0]];
        let gap = open_ends(&[e, f]);
        assert_eq!(
            gap[0],
            [true, false],
            "a 10 m gap in line is still one road"
        );
    }

    #[test]
    fn the_deck_is_found_under_the_carriageway_and_not_beside_it() {
        let tunnel = Tunnel {
            centre: vec![[0.0, 0.0], [100.0, 0.0]],
            road_y: vec![-7.0, -7.0],
            half_w: 6.0,
            clearance: CLEARANCE_M,
            covered: vec![true; 2],
        };
        assert_eq!(tunnel.road_under([50.0, 0.0]), Some(-7.0));
        assert_eq!(tunnel.road_under([50.0, 5.9]), Some(-7.0));
        assert!(tunnel.road_under([50.0, 8.0]).is_none());
        assert!(tunnel.road_under([-20.0, 0.0]).is_none());
    }

    #[test]
    fn the_hole_in_the_ground_covers_exactly_the_stretch_with_no_roof() {
        // A straight 600 m tunnel with a entrance at each end: roofed in the middle,
        // open at both approaches, so two corridors come back.
        let centre: Vec<[f32; 2]> = (0..=60).map(|i| [i as f32 * 10.0, 0.0]).collect();
        let total = 600.0;
        let road_y: Vec<f32> = centre
            .iter()
            .enumerate()
            .map(|(i, _)| {
                -depth_at(
                    i as f32 * 10.0,
                    total,
                    LEVEL_DEPTH_M,
                    MAX_GRADE,
                    [true, true],
                )
            })
            .collect();
        let roofed: Vec<bool> = road_y
            .iter()
            .map(|&y| y <= -(LEVEL_DEPTH_M - ROOF_AT_DEPTH_M) && y + CLEARANCE_M <= CEILING_MAX_Y)
            .collect();
        let rings = opening_rings(&centre, &roofed, 6.0);
        assert_eq!(
            rings.len(),
            2,
            "a entrance at each end should open two holes"
        );
        for ring in &rings {
            assert!(ring.len() >= 4);
            assert_eq!(ring.first(), ring.last(), "the hole is not a closed ring");
        }
    }

    /// What the live tiles actually carry, read off the Groningen fixture: this
    /// is the one thing in this module that is a claim about the data rather
    /// than about the geometry, and it is the claim the whole module rests on.
    #[test]
    fn the_tiles_say_which_streets_are_tunnels() {
        let tile = crate::testdata::groningen();
        let meta = tile.layer("streets").expect("a streets layer");
        let features = tile.features(meta);
        let flagged = features
            .iter()
            .filter(|f| crate::vector_tiles::flag_of(f, "tunnel"))
            .count();
        let below = features.iter().filter(|f| underground(f)).count();
        assert!(flagged > 0, "no street in the fixture is flagged a tunnel");
        assert_eq!(
            below, flagged,
            "`underground` is the bare flag now — the tiles carry no `layer`"
        );
        // **No `layer` anywhere in the layer.** This is the measurement the
        // depth convention rests on: if a tileset ever starts carrying one,
        // this fails and `full_depth` has something better to read.
        assert!(
            features
                .iter()
                .all(|f| crate::vector_tiles::layer_of(f) == 0),
            "a street carries a `layer` now — `full_depth` can stop guessing"
        );
        // And the arcades are told apart from the tunnels by the buildings around
        // them, which is the other half of having no `layer` to read.
        let passages = building_passages(&tile, &features);
        assert!(
            passages.len() < flagged,
            "every flagged way was called a building passage"
        );
    }

    /// Build the fixture's tunnels and report them, so a capture that says the
    /// avatar is not standing on one can be told from a tunnel that is not there.
    #[test]
    #[ignore = "diagnostic"]
    fn dump_the_fixture_bores() {
        // `CARTO_TUNNEL_TILE=/path/to.mvt` to read a live tile instead.
        let owned = std::env::var("CARTO_TUNNEL_TILE")
            .ok()
            .map(|p| std::fs::read(p).expect("tile reads"));
        let tile = match &owned {
            Some(b) => TileData::parse(b).expect("tile parses"),
            None => crate::testdata::groningen(),
        };
        let out = build_tunnels(&tile, 1471.0, None);
        println!(
            "tunnels={} openings={}",
            out.tunnels.len(),
            out.openings.len()
        );
        let key: Option<(f64, f64, f64)> = std::env::var("CARTO_TUNNEL_XYZ").ok().and_then(|s| {
            let v: Vec<f64> = s.split('/').filter_map(|p| p.parse().ok()).collect();
            (v.len() == 3).then(|| (v[0], v[1], v[2]))
        });
        // **Where the road goes under, and which way it is heading.**
        //
        // The centre of an opening is not it: an opening is the stretch that has
        // *not* reached depth, so its middle is a road at lawn level, which is
        // what four overhead captures dutifully photographed. The entrance is the
        // first vertex that is deep enough to take a roof, and a camera wants to
        // stand outside it looking along the road — so the bearing matters as much
        // as the point.
        if let Some((z, x, y)) = key {
            let n = 2f64.powf(z);
            let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
            let span = 40_075_016.686 * lat0.cos() / n;
            let to_geo = |p: [f32; 2]| -> (f64, f64) {
                let (fx, fy) = (x + f64::from(p[0]) / span, y + f64::from(p[1]) / span);
                (
                    (std::f64::consts::PI * (1.0 - 2.0 * fy / n))
                        .sinh()
                        .atan()
                        .to_degrees(),
                    fx / n * 360.0 - 180.0,
                )
            };
            for (i, b) in out.tunnels.iter().enumerate() {
                let deepest = b.road_y.iter().copied().fold(0.0f32, f32::min);
                if deepest > -1.0 {
                    continue;
                }
                let full = deepest + ROOF_AT_DEPTH_M;
                let Some(k) = b.road_y.iter().position(|&y| y <= full) else {
                    continue;
                };
                if k == 0 || k + 1 >= b.centre.len() {
                    continue;
                }
                let (a, c) = (b.centre[k - 1], b.centre[k]);
                // +z runs south, so north is −dz.
                let bearing = (f64::from(c[0] - a[0]).atan2(f64::from(a[1] - c[1]))).to_degrees();
                let (lat, lon) = to_geo(c);
                println!(
                    "entrance {i}: at {lat:.6},{lon:.6}  heading {:.0}°  roof starts at {:.1} m down",
                    (bearing + 360.0) % 360.0,
                    -b.road_y[k]
                );
            }
        }
        // A side view of the deepest road, in text.
        //
        // **Because the pictures were not readable.** Six months of this feature's
        // evidence was software-rendered stills at handset quality, and a person
        // looking at one could not tell a covered road from a pale strip of
        // ground — reasonably, because that is what it looks like. A profile is
        // unambiguous: it says how far down the road goes, over what distance, and
        // which part of it has a lid.
        if let Some(b) = out
            .tunnels
            .iter()
            .min_by(|a, c| {
                let d = |x: &Tunnel| x.road_y.iter().copied().fold(0.0f32, f32::min);
                d(a).total_cmp(&d(c))
            })
            .filter(|b| b.road_y.iter().copied().fold(0.0f32, f32::min) < -1.0)
        {
            let deepest = b.road_y.iter().copied().fold(0.0f32, f32::min);
            let full = deepest + ROOF_AT_DEPTH_M;
            println!("\nprofile of the deepest road (metres along / metres down / lid):");
            let mut along = 0.0f32;
            for i in 0..b.road_y.len() {
                if i > 0 {
                    along += (b.centre[i][0] - b.centre[i - 1][0])
                        .hypot(b.centre[i][1] - b.centre[i - 1][1]);
                }
                let y = b.road_y[i];
                let lid = if y <= full { "lid" } else { "open sky" };
                let bar = "#".repeat(((-y) * 2.0).round().max(0.0) as usize);
                println!("  {along:6.0} m  {y:6.2} m  {lid:8}  {bar}");
            }
        }
        for (i, b) in out.tunnels.iter().enumerate() {
            let deepest = b.road_y.iter().copied().fold(0.0f32, f32::min);
            let len: f32 = b
                .centre
                .windows(2)
                .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
                .sum();
            println!(
                "tunnel {i}: half_w={:.1} len={len:.0}m deepest={deepest:.2} ends=({:.0},{:.0})->({:.0},{:.0})",
                b.half_w,
                b.centre.first().map_or(0.0, |p| p[0]),
                b.centre.first().map_or(0.0, |p| p[1]),
                b.centre.last().map_or(0.0, |p| p[0]),
                b.centre.last().map_or(0.0, |p| p[1]),
            );
        }
        // Where each opening is **on Earth**, not in tile-local metres.
        //
        // The metres were what this printed first, and converting them to a
        // latitude and longitude by hand to aim a camera went wrong in three
        // separate sessions — the captures landed on a park, a rail yard and the
        // side of an office block, and each one cost five minutes of software
        // rendering to find out. `CARTO_TUNNEL_XYZ=z/x/y` names the tile the file
        // came from and the conversion happens here, once, in code that can be
        // wrong only once.
        for (i, m) in out.openings.iter().enumerate() {
            let b = bbox_of_ring(m);
            let (cx, cz) = ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5);
            let here = key.map_or(String::new(), |(z, x, y)| {
                let n = 2f64.powf(z);
                // Metres across this tile, at this tile's own latitude.
                let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
                let span = 40_075_016.686 * lat0.cos() / n;
                let (fx, fy) = (x + f64::from(cx) / span, y + f64::from(cz) / span);
                let lon = fx / n * 360.0 - 180.0;
                let lat = (std::f64::consts::PI * (1.0 - 2.0 * fy / n))
                    .sinh()
                    .atan()
                    .to_degrees();
                format!("  at {lat:.6},{lon:.6}")
            });
            println!(
                "opening {i}: {:.0}x{:.0} m  x {:.0}..{:.0} z {:.0}..{:.0}{here}",
                b[2] - b[0],
                b[3] - b[1],
                b[0],
                b[2],
                b[1],
                b[3]
            );
        }
    }

    /// The cut must actually take ground away. It is the one step between a tunnel
    /// and a tunnel you can see, and a no-op here looks exactly like a feature that
    /// was never built: the geometry is all correct and the ground roofs it.
    #[test]
    fn the_cut_removes_the_ground_over_an_opening() {
        // A 100 m square of ground as two triangles, the way earcut emits a
        // simple polygon.
        let mesh = SurfaceMesh {
            group: SurfaceGroup::Grass,
            positions: vec![
                [0.0, 0.0, 0.0],
                [100.0, 0.0, 0.0],
                [100.0, 0.0, 100.0],
                [0.0, 0.0, 100.0],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uvs: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            rungs: Vec::new(),
        };
        let before = mesh_area(&mesh);
        // A 20 m wide corridor straight through the middle.
        let opening = vec![
            [40.0, -10.0],
            [60.0, -10.0],
            [60.0, 110.0],
            [40.0, 110.0],
            [40.0, -10.0],
        ];
        // **Both ways round.** A opening's winding follows the direction of the tunnel
        // it belongs to, so a rule that only works on one of them works on half a
        // city — and looks like a tunnel that was never built.
        for reversed in [false, true] {
            let mut ring = opening.clone();
            if reversed {
                ring.reverse();
            }
            let mut m = mesh.clone();
            cut_ground(std::slice::from_mut(&mut m), &[ring]);
            let after = mesh_area(&m);
            assert!(
                (before - after - 2000.0).abs() < 200.0,
                "reversed={reversed}: expected ~2000 m² gone, {:.0} went",
                before - after
            );
        }
    }

    /// **The basemap takes the same hole, and keeps its texture registered.**
    ///
    /// A clipmap tile is one textured quad, and what reveals a cut that went wrong
    /// there is not a missing triangle but a *slipped* one: the imagery is mapped
    /// 0..1 across the tile, so a re-triangulated corner whose texture coordinates
    /// were not carried across draws a smear of the wrong part of the city. The
    /// area assertion says the hole is there; the corner assertion says the rest of
    /// the tile still shows what it showed before.
    #[test]
    fn the_basemap_sheet_takes_the_hole_with_its_texture_intact() {
        let mut sheet = SurfaceMesh {
            group: SurfaceGroup::Built,
            positions: vec![
                [-92.0, 0.0, -92.0],
                [92.0, 0.0, -92.0],
                [92.0, 0.0, 92.0],
                [-92.0, 0.0, 92.0],
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            normals: vec![[0.0, 1.0, 0.0]; 4],
            colors: Vec::new(),
            rungs: Vec::new(),
        };
        let before = mesh_area(&sheet);
        // A 20 m corridor across the middle, the shape a entrance approach makes.
        let opening = vec![
            [-10.0, -100.0],
            [10.0, -100.0],
            [10.0, 100.0],
            [-10.0, 100.0],
            [-10.0, -100.0],
        ];
        assert!(cut_sheet(&mut sheet, &[opening]));
        let gone = before - mesh_area(&sheet);
        assert!(
            (gone - 20.0 * 184.0).abs() < 100.0,
            "expected ~{:.0} m² gone, {gone:.0} went",
            20.0 * 184.0
        );
        // Every surviving vertex still carries the texture coordinate its position
        // implies — u east, v south, 0..1 across the 184 m tile.
        for (p, uv) in sheet.positions.iter().zip(sheet.uvs.iter()) {
            let want = [(p[0] + 92.0) / 184.0, (p[2] + 92.0) / 184.0];
            assert!(
                (uv[0] - want[0]).abs() < 1e-3 && (uv[1] - want[1]).abs() < 1e-3,
                "vertex at {p:?} carries uv {uv:?}, not {want:?}"
            );
        }
        // A tile the opening misses is left exactly as it was: the caller uses this
        // answer to put the shared quad back, and a false yes there would give every
        // tile in the pyramid a mesh of its own.
        let mut elsewhere = SurfaceMesh {
            group: SurfaceGroup::Built,
            ..sheet.clone()
        };
        elsewhere.positions = vec![
            [1000.0, 0.0, 1000.0],
            [1100.0, 0.0, 1000.0],
            [1100.0, 0.0, 1100.0],
        ];
        elsewhere.uvs = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]];
        elsewhere.normals = vec![[0.0, 1.0, 0.0]; 3];
        elsewhere.indices = vec![0, 1, 2];
        let far = vec![
            [-10.0, -10.0],
            [10.0, -10.0],
            [10.0, 10.0],
            [-10.0, 10.0],
            [-10.0, -10.0],
        ];
        assert!(!cut_sheet(&mut elsewhere, &[far]));
        assert_eq!(elsewhere.indices, vec![0, 1, 2]);
    }

    /// **Ground over a lid is whole; ground over the open ramp is not.**
    ///
    /// The weight is what lets a ground mesh overrule a stale height survey, so
    /// it has to be confined to the covered part. If it leaked onto the ramp it
    /// would fill in the very hole you drive through.
    #[test]
    fn the_fill_covers_the_lid_and_stops_at_the_ramp() {
        // 100 m of tunnel: the first half open, the second half covered.
        let tunnel = Tunnel {
            centre: vec![[0.0, 0.0], [50.0, 0.0], [100.0, 0.0]],
            road_y: vec![0.0, -7.0, -7.0],
            half_w: 6.0,
            clearance: CLEARANCE_M,
            covered: vec![false, true, true],
        };
        let taper = 30.0;
        assert_eq!(
            tunnel.covered_weight([75.0, 0.0], taper),
            1.0,
            "ground directly over the lid must be filled"
        );
        assert_eq!(
            tunnel.covered_weight([75.0, 5.0], taper),
            1.0,
            "…and so must ground within the carriageway's own width"
        );
        assert_eq!(
            tunnel.covered_weight([10.0, 0.0], taper),
            0.0,
            "the open ramp must not be filled in — it is the way in"
        );
        assert_eq!(
            tunnel.covered_weight([75.0, 400.0], taper),
            0.0,
            "ground nowhere near a tunnel is left to the survey"
        );
        // Between the two, a smooth ramp rather than a step: a hard edge to the
        // fill is a crease running the length of the road.
        let near = tunnel.covered_weight([75.0, 12.0], taper);
        let far = tunnel.covered_weight([75.0, 30.0], taper);
        assert!(
            (0.0..1.0).contains(&near) && near > far,
            "the fill must fade out with distance: {near} then {far}"
        );
    }

    /// Plan area of a mesh's triangles.
    fn mesh_area(m: &SurfaceMesh) -> f32 {
        m.indices
            .chunks_exact(3)
            .map(|t| {
                let p = [
                    m.positions[t[0] as usize],
                    m.positions[t[1] as usize],
                    m.positions[t[2] as usize],
                ];
                ((p[1][0] - p[0][0]) * (p[2][2] - p[0][2])
                    - (p[2][0] - p[0][0]) * (p[1][2] - p[0][2]))
                    .abs()
                    * 0.5
            })
            .sum()
    }

    #[test]
    fn a_bore_roofed_end_to_end_opens_no_hole() {
        let centre = vec![[0.0, 0.0], [100.0, 0.0]];
        assert!(opening_rings(&centre, &[true, true], 6.0).is_empty());
    }

    /// **Nothing a tunnel emits may ask the depth buffer for more than the road
    /// band holds.**
    ///
    /// The rung is a per-vertex depth offset of `ROAD_RUNG_ULP` units each, and
    /// `ground_stack` reserves exactly `ROAD_DEPTH_RUNGS` of them above the road
    /// sheet's own bias. A value from outside that band is not a slightly wrong
    /// ordering, it is a surface pulled toward the camera by a fraction of its own
    /// distance — which is what `road_rung`'s 391 did here until 2026-09-18, and
    /// what no capture caught, because the error grows with range and a tunnel
    /// photographed from close up looks right.
    ///
    /// `ground_stack`'s `the_road_band_holds_every_road_vertex` pins the same
    /// property for the surface roads. This is the tunnels' half of it: the band
    /// is only a band if everything that writes the attribute stays inside it.
    #[test]
    fn a_tunnel_asks_for_no_more_depth_bias_than_the_road_band_holds() {
        let out = build_tunnels(&crate::testdata::groningen(), 1471.0, None);
        assert!(
            !out.meshes.is_empty(),
            "fixture grew no tunnels, so this asserts nothing"
        );
        let ceiling = crate::vector_tiles::ROAD_DEPTH_RUNGS as f32;
        for m in &out.meshes {
            for &r in &m.rungs {
                assert!(
                    (0.0..ceiling).contains(&r),
                    "{:?} emitted rung {r}, outside the {ceiling}-rung road band \
                     — see `road_rung`, which is the cut order and not this",
                    m.group
                );
            }
        }
    }

    /// The deck is the only thing in a tunnel with a road texture on it, and it is
    /// held off the ground by metres of geometry — so it wants no bias at all, not
    /// merely a small one. Pinned separately from the band test above because
    /// "inside the band" would still be satisfied by the bridge rung, which would
    /// put a tunnel over a flyover.
    #[test]
    fn a_tunnel_deck_draws_on_the_ground_rung() {
        let out = build_tunnels(&crate::testdata::groningen(), 1471.0, None);
        let decks: Vec<_> = out
            .meshes
            .iter()
            .filter(|m| matches!(m.group, SurfaceGroup::TunnelRoad(_)))
            .collect();
        assert!(!decks.is_empty(), "fixture grew no tunnel decks");
        for m in decks {
            assert!(
                m.rungs.iter().all(|&r| r == road_depth_rung(false)),
                "a tunnel deck asked for a rung above the ground one"
            );
        }
    }
}
