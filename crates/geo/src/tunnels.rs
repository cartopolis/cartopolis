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
//! So the deck rises to meet the ground over [`ramp_m`] at each genuine
//! entrance, at [`MAX_GRADE`] — **on the road beyond the portal** where there is
//! one to build it on ([`find_approaches`]), because the tag marks the covered
//! part and the ramp is not covered. Only what does not fit out there is spent
//! inside the tagged run, and a run too short for that never reaches full depth —
//! honest for an underpass, wrong for a tunnel under a hill, and the tiles cannot
//! tell those apart.
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
use crate::terrain_refine::GroundField;
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

/// How deep an open stretch must be before the survey's trench beside it counts as
/// its own cut, and is filled back to the land around ([`Tunnel::fill_weight`]).
/// A metre: shallower is the top of a ramp, where the ground and the road meet.
pub const FILL_DEPTH_M: f32 = 1.0;

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
    /// Whether `road_y` is the road's own solved height relative to the ground
    /// under it (`crate::road_levels`), rather than the convention a consumer
    /// lifts back by what the survey dug. A measured depth takes no lift:
    /// [`lift_for_road`].
    pub measured: bool,
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
        road_under_line(&self.centre, &self.road_y, self.half_w, p)
    }

    /// The deck level under `p` on this corridor widened by `reach` metres either
    /// side, or `None` past that.
    #[must_use]
    pub fn road_near(&self, p: [f32; 2], reach: f32) -> Option<f32> {
        road_under_line(&self.centre, &self.road_y, self.half_w + reach, p)
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
    /// Bounding box of the covered span, widened by the carriageway's half width:
    /// `[min_x, min_y, max_x, max_y]`, or `None` where nothing is covered.
    ///
    /// [`covered_weight`](Self::covered_weight) walks every segment and its only
    /// early exit fires *inside* a tunnel, so a point nowhere near one — which is
    /// almost every point the ground asks about — is its worst case. A caller
    /// holding many tunnels should reject against this first, expanded by its own
    /// taper. Kept as a method rather than a field because `centre` and `covered`
    /// are public and a cached box would go stale behind them.
    #[must_use]
    pub fn covered_bounds(&self) -> Option<[f32; 4]> {
        self.bounds_where(|i| self.covered[i] && self.covered[i + 1])
    }

    /// [`covered_bounds`](Self::covered_bounds) for [`fill_weight`](Self::fill_weight)'s
    /// segments.
    #[must_use]
    pub fn fill_bounds(&self) -> Option<[f32; 4]> {
        self.bounds_where(|i| self.fills(i))
    }

    /// Whether segment `i` is covered, or sunk deep enough that the survey's
    /// trench beside it is this road's own cut.
    fn fills(&self, i: usize) -> bool {
        (self.covered[i] && self.covered[i + 1])
            || self.road_y[i].min(self.road_y[i + 1]) < -FILL_DEPTH_M
    }

    fn bounds_where(&self, keep: impl Fn(usize) -> bool) -> Option<[f32; 4]> {
        let mut b: Option<[f32; 4]> = None;
        for i in 0..self.centre.len().saturating_sub(1) {
            if !keep(i) {
                continue;
            }
            for p in [self.centre[i], self.centre[i + 1]] {
                b = Some(match b {
                    None => [p[0], p[1], p[0], p[1]],
                    Some(v) => [
                        v[0].min(p[0]),
                        v[1].min(p[1]),
                        v[2].max(p[0]),
                        v[3].max(p[1]),
                    ],
                });
            }
        }
        b.map(|v| {
            [
                v[0] - self.half_w,
                v[1] - self.half_w,
                v[2] + self.half_w,
                v[3] + self.half_w,
            ]
        })
    }

    #[must_use]
    pub fn covered_weight(&self, p: [f32; 2], taper_m: f32) -> f32 {
        self.weight_where(p, taper_m, |i| self.covered[i] && self.covered[i + 1])
    }

    /// How much the ground at `p` should be put back to the level of the land
    /// around it: [`covered_weight`](Self::covered_weight), and **also** beside an
    /// open stretch sunk more than [`FILL_DEPTH_M`].
    ///
    /// The survey was flown when the whole ring was an open trench, so beside an
    /// open cutting it still shows that trench's sloped sides wider than the
    /// cutting drawn here — and the ground there tore down the slope from the
    /// top of the wall, which stood five metres short of the land around it
    /// (measured over the ring's cuttings, 2026-09-23). Inside the cutting the
    /// ground is cut away, so filling it cannot close the hole; what this lifts is
    /// the verge beside the wall.
    #[must_use]
    pub fn fill_weight(&self, p: [f32; 2], taper_m: f32) -> f32 {
        self.weight_where(p, taper_m, |i| self.fills(i))
    }

    fn weight_where(&self, p: [f32; 2], taper_m: f32, keep: impl Fn(usize) -> bool) -> f32 {
        let mut best = 0.0f32;
        for i in 0..self.centre.len().saturating_sub(1) {
            if !keep(i) {
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
    /// The same corridors keyed to the `streets` feature each came off, for the
    /// consumers that **travel** the road rather than draw it. See [`SunkenWay`].
    pub sunken: Vec<SunkenWay>,
}

/// One street's sunken profile, keyed to the feature it belongs to.
///
/// **Keyed, because position is not enough, and that mistake has been made here
/// twice already.** `DeckPlates::lift_at` sampled a bridge's height as a function
/// of world XZ and hoisted the underpass onto the viaduct above it
/// ([`grade-separation.md`](../../../docs/notes/grade-separation.md)); asking
/// "is there a tunnel under this car" has the identical shape, and Groningen's
/// ring has ordinary streets crossing over it. The only thing that knows whether
/// a car is *on* the sunken road or on the one above it is which feature its
/// line came from, so that is what travels with the profile.
///
/// It covers the cuttings as well as the lids, and it has to: a cutting is an
/// untagged surface way that this module has decided is seven metres down, so a
/// consumer reading the tunnel flag would put its cars on the ground over an open
/// trench.
#[derive(Clone, Debug)]
pub struct SunkenWay {
    /// Index into the tile's `streets` features, as `tile.features` orders them.
    pub feature: usize,
    /// Centre line, tile-local metres.
    pub centre: Vec<[f32; 2]>,
    /// Deck level at each vertex, metres, negative.
    pub road_y: Vec<f32>,
    /// Half the carriageway, so a point beside the corridor can be told from one
    /// on it.
    pub half_w: f32,
    /// See [`Tunnel::measured`].
    pub measured: bool,
}

impl SunkenWay {
    /// The deck level under `p`, or `None` where `p` is not over this corridor.
    ///
    /// The same clamped-projection walk [`Tunnel::road_under`] does, and for the
    /// same reason — a chain of segments has to behave as one corridor with
    /// rounded ends rather than as a set of infinite strips.
    #[must_use]
    pub fn road_under(&self, p: [f32; 2]) -> Option<f32> {
        road_under_line(&self.centre, &self.road_y, self.half_w, p)
    }
}

/// The deck level under `p` on a corridor, or `None` where `p` is not over it.
///
/// One function because two types hold the same three fields and a car asks this
/// per frame — [`Tunnel`], which is what the drawn world and the avatar's footing
/// read, and [`SunkenWay`], which is what the drive network reads. They agreeing
/// is the difference between a car on the carriageway and a car in the roof.
fn road_under_line(centre: &[[f32; 2]], road_y: &[f32], half_w: f32, p: [f32; 2]) -> Option<f32> {
    let mut best: Option<(f32, f32)> = None;
    for i in 0..centre.len().saturating_sub(1) {
        let (a, b) = (centre[i], centre[i + 1]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len_sq = dx * dx + dy * dy;
        if len_sq <= 1e-9 {
            continue;
        }
        // Clamped projection: past either end of a segment the nearest point is
        // that end, which is what makes a chain of segments behave as one
        // corridor with rounded ends rather than as a set of infinite strips.
        let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len_sq).clamp(0.0, 1.0);
        let (cx, cy) = (a[0] + dx * t, a[1] + dy * t);
        let d = (p[0] - cx).hypot(p[1] - cy);
        if d > half_w {
            continue;
        }
        let y = road_y[i] + (road_y[i + 1] - road_y[i]) * t;
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, y));
        }
    }
    best.map(|(_, y)| y)
}

/// How far to reach for the land *around* a tunnel when asking how much of its
/// depth the height survey has already dug.
///
/// Wider than the cut and narrower than the block: a dual carriageway in a trench
/// is about 25 m of road plus its walls, so a shorter reach samples the trench
/// itself and finds no drop at all, while a much longer one starts finding the
/// next street's embankment and lifting the tunnel out of its own hole.
pub const DATUM_M: f32 = 30.0;

/// How far back up a tunnel is lifted at one place: whatever the height survey has
/// already dug there, but never more than the depth the tessellator put the
/// carriageway at.
///
/// **A tunnel must not be dug twice, and it must not be filled in either.** The
/// survey's bare-ground model has no roof to hide the bottom of a cutting, so over
/// an open cut it measures the trench — and sinking an invented seven metres into
/// a measured nine puts the carriageway sixteen metres under the street. Lifting
/// by `dug` cancels exactly the part the measurement already accounts for.
///
/// The cap is what stops the other error. Clamped to `depth`, the lift can raise
/// the deck to the measured ground at that place and no further, so a fully
/// measured cut lands the deck on the trench floor with its wall tops at street
/// level; a covered tunnel, where the laser saw the lid and `dug` is zero, keeps
/// the whole convention; a partly measured cut absorbs what was measured and
/// invents the rest; and outside the Netherlands the field is flat zero and this
/// is the old behaviour to the bit.
#[must_use]
pub fn lift_for(depth: f32, dug: f32) -> f32 {
    dug.max(0.0).min(depth.max(0.0))
}

/// [`lift_for`] for a corridor that may already be measured: a depth solved on
/// the road network (`crate::road_levels`) is the road's own height relative to
/// the ground under it, and lifting it by what the survey dug would count the
/// survey twice.
#[must_use]
pub fn lift_for_road(measured: bool, depth: f32, dug: f32) -> f32 {
    if measured {
        0.0
    } else {
        lift_for(depth, dug)
    }
}

/// What the height survey says about one place, in whatever frame the caller's
/// meshes are in.
///
/// Two questions and no more, because those are the two the tunnel rules ask: how
/// high the ground is *here*, and how high the land is *around* here. The client
/// answers them from its streamed `TerrainField` and the harness from one blocking
/// request, and neither detail belongs in the rules.
pub trait Survey {
    /// Ground height at a point.
    fn here(&self, x: f32, z: f32) -> f32;
    /// Height of the land around that point, [`DATUM_M`] out — the level the
    /// tunnel was cut down from.
    fn around(&self, x: f32, z: f32) -> f32;
}

/// Stand a tunnel's own mesh — its deck, its walls, its roof — on the measured
/// ground.
///
/// `depth_at` answers how deep the tessellator put the carriageway at a point, as
/// a positive number of metres, and `None` where no corridor covers it; those
/// vertices (the notch triangles on the outside of a bend reach a hair past the
/// corridor) keep `fallback`, which callers set to the deepest vertex in the mesh.
///
/// **The cap is the depth under this vertex, not the deepest point of the mesh it
/// is in.** A merged mesh is the whole tile's tunnels, so a mesh-wide cap lifts
/// everything shallower too far — and almost all of a tunnel is shallower, because
/// the ramps spend about 117 m each reaching full depth on runs that arrive
/// 170–240 m long. Asking by position also keeps a wall from shearing: the top of
/// a wall quad and the deck under it get one lift, so the clearance between them
/// survives.
pub fn stand_tunnel_on(
    positions: &mut [[f32; 3]],
    origin: [f32; 2],
    survey: &dyn Survey,
    depth_at: &dyn Fn([f32; 2]) -> Option<f32>,
) {
    let fallback = -positions.iter().map(|p| p[1]).fold(0.0_f32, f32::min);
    for p in positions.iter_mut() {
        let (x, z) = (origin[0] + p[0], origin[1] + p[2]);
        let here = survey.here(x, z);
        let dug = survey.around(x, z) - here;
        let depth = depth_at([x, z]).unwrap_or(fallback);
        p[1] += here + lift_for(depth, dug);
    }
}

/// The measured ground with every lid put back on it.
///
/// **Two sources disagree and one of them is stale.** The height survey measures
/// what it flew over, and Groningen's ring was an open trench when it was flown
/// and has a lid on it now — so the survey digs nine metres out of ground that is
/// continuous, the lid stays at street level, and a covered road reads as a raised
/// platform with a ditch either side. The map's tag is the newer fact, so over a
/// covered stretch the ground is lifted back to the level of the land around it,
/// tapering out so the fill has no crease at its edge.
///
/// **It is a `GroundField` and not a pass over finished vertices**, and that is
/// the whole point of the type. Added afterwards, the lift is a second surface the
/// refinement never saw: a land parcel earcut into two triangles interpolates it
/// across a 100 m chord while the road beside it, which has a vertex every few
/// metres, follows it — so the two disagree by metres and the ground tears open
/// along the buried road. Those were the dark jagged seams running down the
/// Zuiderplantsoen over Groningen's ring (measured 2026-09-21). Handed to
/// [`crate::terrain_refine::refine_for_terrain`] as the crease field instead, the
/// lift is part of the surface being resolved, every mesh splits its edges against
/// the same function, and the shared-edge agreement the refinement already
/// guarantees covers the lid too.
///
/// Nothing happens where there is no tunnel (the weight is 0 and this is one
/// comparison), and nothing happens over an *open* stretch — which is right,
/// because there the survey and the map agree that the ground is missing.
pub struct Lidded<'a> {
    /// The measured ground, before any lid.
    pub ground: &'a dyn GroundField,
    /// The level of the land around a point, [`DATUM_M`] out — what a lid is
    /// lifted back to.
    pub around: &'a dyn Fn(f32, f32) -> f32,
    /// [`Tunnel::covered_weight`] over whichever tunnels the caller holds.
    pub weight: &'a dyn Fn([f32; 2]) -> f32,
}

impl GroundField for Lidded<'_> {
    fn min_ground_edge_m(&self) -> f32 {
        self.ground.min_ground_edge_m()
    }

    fn at(&self, x: f32, z: f32) -> f32 {
        let here = self.ground.at(x, z);
        let w = (self.weight)([x, z]);
        if w > 0.0 {
            here + w * ((self.around)(x, z) - here)
        } else {
            here
        }
    }
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
    // **Before the buildings are touched at all.** A passage is a way that is
    // flagged underground; where the tile has none — which is most tiles, and
    // every tile in a city with no tunnels in it — there is nothing to test and
    // no reason to have an opinion about the buildings. `building_rings` walks
    // all 8,552 footprints on the Groningen fixture and allocates a `Vec` per
    // one, and this is called from two places per cell, so the answer that is
    // always the empty set cost ~17,000 allocations and two copies of every
    // building in the tile. It also *forces the decode* of the buildings layer
    // inside the road tessellator, whether or not anything else wanted it.
    if !features.iter().any(underground) {
        return out;
    }
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

/// Whether a building whose footprint centres on `p` stands **in the trench of
/// a tunnel that has a lid on it now** — the survey's old cut, under a covered
/// stretch or in a mouth.
///
/// Such a thing is the tunnel's own works, registered while the road was being
/// built: over Groningen's ring there is a 2019 building right at a mouth, and
/// drawn on the ground under it — the trench floor the height survey measured
/// before the lid went on — it was a pale block filling the tunnel mouth from the
/// deck to above the lid (2026-09-23). Both tests must hold: `p` is over a
/// covered corridor or an opening, **and** `dug`, how far the ground at `p` is
/// below the land around it, exceeds [`TRENCH_DUG_M`]. The second keeps an
/// ordinary building that stands *on* a lid, at street level, drawn.
///
/// Frame-agnostic: `p`, the tunnels and the openings only have to share one.
#[must_use]
pub fn stands_in_a_covered_trench(
    tunnels: &[Tunnel],
    openings: &[Vec<[f32; 2]>],
    p: [f32; 2],
    dug: f32,
) -> bool {
    if dug <= TRENCH_DUG_M {
        return false;
    }
    tunnels
        .iter()
        .any(|t| t.covered_weight(p, 0.0) >= 1.0 && t.road_under(p).is_some_and(|y| y < -2.0))
        || in_an_opening(openings, p)
}

/// How far outside a sunken road's carriageway a surveyed wall may run and still
/// be that road's own retaining or portal wall, metres.
pub const WALL_REACH_M: f32 = 3.0;

/// Whether a surveyed **wall** is part of a sunken road — a retaining wall along a
/// cutting, a portal's headwall across a mouth, a tunnel's side wall under a lid
/// — and so already drawn by this module.
///
/// The survey registers those walls as ordinary boundaries, and the 3D product
/// measures them from the trench floor to the top. Drawn the way every other
/// boundary is — a box standing *up* from the ground by its measured height — the
/// ring's east portal wall became a 7 m slab across both mouths and the north
/// cutting's retaining wall a 9 m one along its edge, pale blocks that filled the
/// mouths from the deck to above the lid (2026-09-23).
///
/// The test is by length, not by a point: the line is sampled every metre or two,
/// and it is the tunnel's when most of the samples lie over a sunken corridor
/// (deck more than a metre down) widened by [`WALL_REACH_M`], or in an opening.
/// A garden wall that merely crosses the ground over a lid is mostly elsewhere.
#[must_use]
pub fn wall_of_a_sunken_road(
    tunnels: &[Tunnel],
    openings: &[Vec<[f32; 2]>],
    line: &[[f32; 2]],
) -> bool {
    if line.len() < 2 || (tunnels.is_empty() && openings.is_empty()) {
        return false;
    }
    let mut samples = 0usize;
    let mut near = 0usize;
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = (b[0] - a[0]).hypot(b[1] - a[1]);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps = (len / 1.5).ceil().max(1.0) as usize;
        for k in 0..=steps {
            #[allow(clippy::cast_precision_loss)]
            let t = k as f32 / steps as f32;
            let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            samples += 1;
            let sunken = tunnels.iter().any(|tu| {
                road_under_line(&tu.centre, &tu.road_y, tu.half_w + WALL_REACH_M, p)
                    .is_some_and(|y| y < -1.0)
            });
            if sunken || in_an_opening(openings, p) {
                near += 1;
            }
        }
    }
    near * 5 >= samples * 3
}

/// [`stands_in_a_covered_trench`] for a whole footprint: true when any of its
/// corners, the middle of any of its edges, or its centre qualifies, each with
/// `dug` asked at that point.
///
/// The centre alone is a weak question for the shapes this is for: the building
/// in the ring's eastern mouth (pand 0014100040019318) is a strip about five
/// metres wide arched across both carriageways — the portal's own frame — and a
/// shape like that can have its centre anywhere relative to what it spans.
#[must_use]
pub fn footprint_in_a_covered_trench(
    tunnels: &[Tunnel],
    openings: &[Vec<[f32; 2]>],
    ring: &[[f32; 2]],
    dug: &dyn Fn([f32; 2]) -> f32,
) -> bool {
    if ring.is_empty() || (tunnels.is_empty() && openings.is_empty()) {
        return false;
    }
    let n = ring.len() as f32;
    let centre = ring
        .iter()
        .fold([0.0f32; 2], |a, p| [a[0] + p[0] / n, a[1] + p[1] / n]);
    let mids = ring
        .windows(2)
        .map(|w| [(w[0][0] + w[1][0]) * 0.5, (w[0][1] + w[1][1]) * 0.5]);
    std::iter::once(centre)
        .chain(ring.iter().copied())
        .chain(mids)
        .any(|p| stands_in_a_covered_trench(tunnels, openings, p, dug(p)))
}

/// How far below the land around it a building's ground must be to count as
/// standing in a tunnel's old trench — well past the half metre of an ordinary
/// Dutch street's camber and kerb, well short of a seven-metre cut.
pub const TRENCH_DUG_M: f32 = 2.0;

/// [`in_an_opening`], for something with a footprint: true when `p` is inside
/// an opening or within `r` of its edge.
///
/// A tree is tested by its trunk and drawn by its crown, and a 4 m crown whose
/// trunk stands on the lip of a trench hangs over the carriageway — which is
/// what the captures over Groningen's cuttings showed (2026-09-23).
#[must_use]
pub fn near_an_opening(openings: &[Vec<[f32; 2]>], p: [f32; 2], r: f32) -> bool {
    openings.iter().any(|ring| {
        point_in_ring(p, ring)
            || ring.windows(2).any(|w| {
                let (a, b) = (w[0], w[1]);
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let len_sq = (dx * dx + dy * dy).max(1e-9);
                let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len_sq).clamp(0.0, 1.0);
                (p[0] - (a[0] + dx * t)).hypot(p[1] - (a[1] + dy * t)) < r
            })
    })
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

/// How long an untagged gap between two lids must be before it is open sky.
///
/// The tunnel flag marks the covered parts, so the stretches between them arrive
/// untagged — but *not tagged* is not the same as *not covered*, and the tiles
/// cannot tell a hundred metres of open cutting from a four-metre seam where
/// somebody split one way in two. A stretch of road is the unit here, so this is
/// `traffic::MIN_DRIVE_LEN_M`'s twenty-five metres rather than a tolerance:
/// above it the gap is a piece of road with sky over it, below it the lid simply
/// carries on. Groningen's ring is not near the line in either direction — its
/// open stretches are 107 m and 109 m, and the fixture's seam is 4 m.
const MIN_CUTTING_M: f32 = 25.0;

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
///
/// `outside` is how much of each entrance ramp can be built *outside* the run,
/// on the road beyond its portal ([`find_approaches`]). Only the part of a ramp
/// that does not fit outside is spent inside, so a run whose approaches take both
/// ramps whole is covered end to end at full depth — which is what the tag says
/// it is.
fn depth_for_run(
    class: RoadClass,
    total: f32,
    entrances: OpenEnds,
    outside: [f32; 2],
) -> (f32, f32) {
    let (depth, grade) = depth_and_grade(class);
    let inside = |d: f32| -> f32 {
        (0..2)
            .filter(|&k| entrances[k])
            .map(|k| (d / grade.max(1e-3) - outside[k]).max(0.0))
            .sum()
    };
    if total - inside(depth) >= MIN_COVERED_M {
        return (depth, grade);
    }
    // The covered length only shrinks as the depth grows, so the deepest depth
    // that still leaves `MIN_COVERED_M` under a lid is a bisection away.
    let floor = min_roofable_depth(class);
    let (mut lo, mut hi) = (0.0f32, depth);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if total - inside(mid) >= MIN_COVERED_M {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo.max(floor).min(depth), grade)
}

/// The depth at each end of a run where its inner ramp starts: zero at an
/// entrance that climbs inside the run, and however far the approach outside
/// has already taken it down where there is one. Not an entrance: unused.
fn entry_depths(depth: f32, grade: f32, entrances: OpenEnds, outside: [f32; 2]) -> [f32; 2] {
    [0, 1].map(|k| {
        if entrances[k] {
            (outside[k] * grade).clamp(0.0, depth)
        } else {
            depth
        }
    })
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
    entry: [f32; 2],
) -> Vec<[f32; 2]> {
    let ramp = |k: usize| ramp_m(depth - entry[k], grade);
    let total: f32 = centre
        .windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .sum();
    let mut cuts: Vec<f32> = Vec::with_capacity(2);
    if entrances[0] {
        cuts.push(ramp(0));
    }
    if entrances[1] {
        cuts.push(total - ramp(1));
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
#[cfg(test)]
fn depth_at(s: f32, total: f32, depth: f32, grade: f32, entrances: OpenEnds) -> f32 {
    depth_at_from(s, total, depth, grade, entrances, [0.0, 0.0])
}

/// [`depth_at`], for a run whose inner ramps start `entry` metres down rather
/// than at the surface — the rest of the descent having been built outside the
/// portal ([`entry_depths`]).
fn depth_at_from(
    s: f32,
    total: f32,
    depth: f32,
    grade: f32,
    entrances: OpenEnds,
    entry: [f32; 2],
) -> f32 {
    let mut d = depth;
    for (k, along) in [(0usize, s), (1, total - s)] {
        if !entrances[k] {
            continue;
        }
        let e = entry[k].min(depth);
        let ramp = ramp_m(depth - e, grade);
        if ramp <= 1e-3 {
            d = d.min(depth.max(e));
            continue;
        }
        d = d.min(e + (depth - e) * (along / ramp).clamp(0.0, 1.0));
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

/// **A gap between two lids is not a portal, and the road does not climb out of
/// it.**
///
/// Groningen's southern ring is built *dicht-open-dicht-open*: three covered
/// stretches with short open ones between, and the carriageway stays at depth the
/// whole way. Read off the tile on 2026-09-20 it is lid 174 m, open 109 m, lid
/// 241 m, open 107 m, lid 233 m — 863 m of sunken road with exactly two real
/// portals, at the far ends.
///
/// The tunnel flag marks only the covered parts, so each lid arrives here as its
/// own run with two ends, and both look like entrances: the model ramped to the
/// surface and back down inside a 108 m gap, which is why every lid was covered
/// for a few metres instead of its whole length.
///
/// **The bound is the road's own gradient, not a tolerance.** Coming up and going
/// back down costs `2 x ramp_m`, so a gap shorter than that *cannot* reach the
/// surface whatever the map says; the open stretch is a cutting, not a pair of
/// portals. At [`LEVEL_DEPTH_M`] and [`MAX_GRADE`] that is 233 m against
/// Groningen's 107 m, so the answer is not close to the line.
///
/// Only a **surface** run bridges: two tunnels meeting directly are already
/// handled by [`open_ends`], and a gap with nothing in it is a gap in the data
/// rather than a road.
fn bridge_short_gaps(
    tile: &TileData,
    tile_m: f32,
    lines: &[Vec<[f32; 2]>],
    unshared: &mut [OpenEnds],
) -> Vec<Cutting> {
    let mut cuttings = Vec::new();
    let Some(meta) = tile.layer("streets") else {
        return cuttings;
    };
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    let span = 2.0 * ramp_m(LEVEL_DEPTH_M, MAX_GRADE);

    // Every tunnel end, as (run, which end, point).
    let ends: Vec<(usize, usize, [f32; 2])> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.len() >= 2)
        .flat_map(|(i, l)| [(i, 0, l[0]), (i, 1, l[l.len() - 1])])
        .collect();

    for (fi, feature) in tile.features(meta).iter().enumerate() {
        if underground(feature) {
            continue;
        }
        let mut geoms: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => geoms.push(ls),
            Geometry::MultiLineString(mls) => geoms.extend(mls.iter()),
            _ => continue,
        }
        for ls in geoms {
            let pts: Vec<[f32; 2]> =
                ls.0.iter()
                    .filter(|c| c.x.is_finite() && c.y.is_finite())
                    .map(|c| [c.x * scale, c.y * scale])
                    .collect();
            if pts.len() < 2 {
                continue;
            }
            let len: f32 = pts
                .windows(2)
                .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
                .sum();
            if len > span {
                continue;
            }
            let (head, tail) = (pts[0], pts[pts.len() - 1]);
            let at = |p: [f32; 2]| -> Option<(usize, usize)> {
                ends.iter()
                    .find(|(_, _, q)| (q[0] - p[0]).hypot(q[1] - p[1]) <= SAME_POINT_M)
                    .map(|(i, k, _)| (*i, *k))
            };
            // Both ends on a tunnel, and not the same tunnel run twice — that
            // would be a loop off one portal rather than a gap between two.
            if let (Some((i, ki)), Some((j, kj))) = (at(head), at(tail)) {
                if i != j {
                    unshared[i][ki] = false;
                    unshared[j][kj] = false;
                    // **And the way itself is not a surface street.** Clearing the
                    // two ends says the road does not climb out here; it does not
                    // say what the road between them is, and until 2026-09-22
                    // nothing did — so the carriageway was drawn on the ground at
                    // `y = 0` between two decks seven metres under it. Keeping the
                    // geometry is what lets the cutting be built at depth below.
                    let flags = StreetFlags {
                        oneway: flag_of(feature, "oneway") || flag_of(feature, "oneway_reverse"),
                        oneway_reverse: flag_of(feature, "oneway_reverse"),
                        link: flag_of(feature, "link"),
                    };
                    let kind = kind_of(feature).unwrap_or("");
                    if let Some(class) = road_class(kind, flags) {
                        let half = road_width_m(kind, flags) * 0.5;
                        if half > 0.0 {
                            cuttings.push(Cutting {
                                centre: pts.clone(),
                                half,
                                class,
                                joins: [(i, ki), (j, kj)],
                                feature: fi,
                            });
                        }
                    }
                }
            }
        }
    }
    cuttings
}

/// A surface street that runs between two lids, and therefore is not a surface
/// street at all.
///
/// Groningen's southern ring is built *dicht-open-dicht-open* and the tunnel flag
/// marks only the covered parts, so the open stretches between them arrive as
/// ordinary untagged ways. [`bridge_short_gaps`] already had to find them — that
/// is how it knows the lids either side are not portals — and threw the geometry
/// away, which left the road drawn flat on the ground between two decks at
/// −7 m: six vertical seven-metre steps in 863 m of carriageway, and a car that
/// teleports at each one.
///
/// So the way is absorbed into the corridor and built as an **open cutting**: the
/// same deck, walls and ground hole as a tunnel, and never a lid, because the one
/// thing the tile does say about this stretch is that it is not covered.
struct Cutting {
    /// The way's own geometry, tile-local metres, running from `joins[0]` to
    /// `joins[1]`.
    centre: Vec<[f32; 2]>,
    /// Half the carriageway width the *surface* feature asks for. Its own, not the
    /// lid's: this is the ribbon the ground hole has to swallow, so a cut sized
    /// from the lid would leave a sliver of surface road beside the trench.
    half: f32,
    class: RoadClass,
    /// `(run, end)` at each end, so the depth the cutting is held at is the depth
    /// of the lids it joins rather than a second opinion about it. A cutting that
    /// decided its own depth would step at both ends instead of one.
    joins: [(usize, usize); 2],
    /// The untagged `streets` feature this was read off.
    feature: usize,
}

/// The road beyond a tunnel's portal, down which its entrance ramp is built.
///
/// **OSM tags the covered part, and the ramp is not covered.** Groningen's ring
/// under the Oude Winschoterdiep is the case that made this plain: the tagged way
/// is 130 m from the last lid to 35 m past the canal, and the road climbs out on
/// about 300 m of open ramp tagged as ordinary road. Building the ramp *inside*
/// the tag — the only thing this module could do before — put the carriageway
/// 2 m down and roofless where the canal crosses it, so the ring was drawn
/// crossing the water in an open trench (reported from a phone, 2026-09-23).
///
/// So each entrance looks for the way that carries on from it, and the ramp goes
/// there. The tagged run then stays at depth to its own end, which is where the
/// lid ends in the world too.
struct Approach {
    /// Pieces of the approach from the portal outward, each on its own `streets`
    /// feature so the drive network can key a car's depth to its own road
    /// ([`SunkenWay`]). Consecutive pieces share their joining point.
    pieces: Vec<(usize, Vec<[f32; 2]>)>,
    /// How much of it a ramp may use, metres: up to the first thing that meets it
    /// at grade, or the tile border, whichever is nearer.
    usable: f32,
    class: RoadClass,
    half: f32,
}

/// Longest chain of ways an approach is followed along. Shortbread merges ways by
/// attribute, so a ramp is one or two features; this is a bound, not a count.
const APPROACH_MAX_WAYS: usize = 4;

/// How far short of anything that meets the approach at grade the ramp must have
/// reached the surface, metres. A junction needs a little level road either side.
const APPROACH_CLEAR_M: f32 = 5.0;

/// A surface street line: its feature, its geometry in tile-local metres, and
/// whether each end is the line's own (as against a cut by the tile border).
pub(crate) struct SurfaceLine {
    pub(crate) feature: usize,
    pub(crate) pts: Vec<[f32; 2]>,
    pub(crate) own: [bool; 2],
    /// Carried over everything at grade, so it cannot stop a ramp under it.
    pub(crate) bridge: bool,
}

pub(crate) fn surface_lines(tile: &TileData, tile_m: f32) -> Vec<SurfaceLine> {
    let mut out = Vec::new();
    let Some(meta) = tile.layer("streets") else {
        return out;
    };
    let extent = TileData::extent(meta);
    let scale = tile_m / extent;
    for (fi, feature) in tile.features(meta).iter().enumerate() {
        if underground(feature) {
            continue;
        }
        let bridge = flag_of(feature, "bridge");
        let mut geoms: Vec<&geo_types::LineString<f32>> = Vec::new();
        match &feature.geometry {
            Geometry::LineString(ls) => geoms.push(ls),
            Geometry::MultiLineString(mls) => geoms.extend(mls.iter()),
            _ => continue,
        }
        for ls in geoms {
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
                let own = [run.first() == Some(&head), run.last() == Some(&tail)];
                out.push(SurfaceLine {
                    feature: fi,
                    pts: run.iter().map(|p| [p[0] * scale, p[1] * scale]).collect(),
                    own,
                    bridge,
                });
            }
        }
    }
    out
}

/// Unit direction leaving `pts[0]`, measured to the first point far enough away
/// to have one — a generalised line often starts with a vertex a hair from the
/// node.
pub(crate) fn leaving(pts: &[[f32; 2]]) -> Option<[f32; 2]> {
    let a = pts[0];
    pts.iter().skip(1).find_map(|b| {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy);
        (len >= 2.0).then(|| [dx / len, dy / len])
    })
}

pub(crate) fn polyline_len(pts: &[[f32; 2]]) -> f32 {
    pts.windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .sum()
}

/// Distance along `line` to the first place `other` crosses it or ends on it,
/// ignoring anything within `skip` metres of the start.
fn first_meeting(line: &[[f32; 2]], other: &[[f32; 2]], skip: f32) -> Option<f32> {
    let mut best: Option<f32> = None;
    let mut walked = 0.0f32;
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let seg = dx.hypot(dy);
        if seg <= 1e-6 {
            continue;
        }
        let mut consider = |t: f32| {
            let at = walked + t * seg;
            if at > skip && best.is_none_or(|b| at < b) {
                best = Some(at);
            }
        };
        for v in other.windows(2) {
            let (c, d) = (v[0], v[1]);
            let (ex, ey) = (d[0] - c[0], d[1] - c[1]);
            let den = dx * ey - dy * ex;
            if den.abs() <= 1e-9 {
                continue;
            }
            let t = ((c[0] - a[0]) * ey - (c[1] - a[1]) * ex) / den;
            let u = ((c[0] - a[0]) * dy - (c[1] - a[1]) * dx) / den;
            if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
                consider(t);
            }
        }
        // A way that *ends* on the approach — a side street's T — meets it too,
        // and a generalised node need not land exactly on the line.
        for p in [other[0], other[other.len() - 1]] {
            let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (seg * seg)).clamp(0.0, 1.0);
            let (cx, cy) = (a[0] + dx * t, a[1] + dy * t);
            if (p[0] - cx).hypot(p[1] - cy) <= SAME_POINT_M {
                consider(t);
            }
        }
        walked += seg;
    }
    best
}

/// Truncate a polyline at distance `at` along it.
pub(crate) fn cut_at(pts: &[[f32; 2]], at: f32) -> Vec<[f32; 2]> {
    let mut out = vec![pts[0]];
    let mut walked = 0.0f32;
    for w in pts.windows(2) {
        let seg = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
        if walked + seg >= at {
            let t = ((at - walked) / seg.max(1e-6)).clamp(0.0, 1.0);
            let p = [
                w[0][0] + (w[1][0] - w[0][0]) * t,
                w[0][1] + (w[1][1] - w[0][1]) * t,
            ];
            if out
                .last()
                .is_none_or(|q| (p[0] - q[0]).hypot(p[1] - q[1]) > 0.05)
            {
                out.push(p);
            }
            return out;
        }
        walked += seg;
        out.push(w[1]);
    }
    out
}

/// The approach beyond each entrance, per run and end.
///
/// A way carries on from a portal when one of its ends is on the portal and it
/// leaves pointing the way the tunnel was going ([`JOIN_COS`]). It is followed
/// through further ways while exactly one carries on and nothing else joins
/// there — a node where a third road meets is a junction, and a ramp cannot run
/// through one — and cut short of the first street that crosses it or ends on it,
/// for the same reason. Bridges do not stop it: they go over.
///
/// What is left is how much ramp there is room for outside. If it is not the
/// whole ramp, the remainder is built inside the tunnel, as it always was.
fn find_approaches(
    tile: &TileData,
    tile_m: f32,
    lines: &[Vec<[f32; 2]>],
    entrances: &[OpenEnds],
    classes: &[Option<RoadClass>],
) -> Vec<[Vec<Approach>; 2]> {
    let mut out: Vec<[Vec<Approach>; 2]> = lines.iter().map(|_| [Vec::new(), Vec::new()]).collect();
    if !entrances.iter().any(|e| e[0] || e[1]) {
        return out;
    }
    let Some(meta) = tile.layer("streets") else {
        return out;
    };
    let features = tile.features(meta);
    let surface = surface_lines(tile, tile_m);
    // An end of a surface line, oriented to leave from it.
    let oriented = |i: usize, end: usize| -> Vec<[f32; 2]> {
        let mut p = surface[i].pts.clone();
        if end == 1 {
            p.reverse();
        }
        p
    };
    let ends_at = |q: [f32; 2]| -> Vec<(usize, usize)> {
        surface
            .iter()
            .enumerate()
            .flat_map(|(i, l)| {
                [(i, 0usize, l.pts[0]), (i, 1, l.pts[l.pts.len() - 1])]
                    .into_iter()
                    .filter(move |&(_, k, p)| {
                        l.own[k] && (p[0] - q[0]).hypot(p[1] - q[1]) <= SAME_POINT_M
                    })
                    .map(|(i, k, _)| (i, k))
            })
            .collect()
    };

    for (ri, line) in lines.iter().enumerate() {
        let Some(tunnel_class) = classes.get(ri).copied().flatten() else {
            continue;
        };
        if line.len() < 2 {
            continue;
        }
        let (depth, grade) = depth_and_grade(tunnel_class);
        let wanted = ramp_m(depth, grade);
        for end in 0..2 {
            if !entrances[ri][end] {
                continue;
            }
            let (portal, inward) = if end == 0 {
                (line[0], line[1])
            } else {
                (line[line.len() - 1], line[line.len() - 2])
            };
            let (dx, dy) = (portal[0] - inward[0], portal[1] - inward[1]);
            let len = dx.hypot(dy).max(1e-6);
            let outward = [dx / len, dy / len];
            let leaves = |here: [f32; 2], heading: [f32; 2], used: &[usize]| {
                ends_at(here)
                    .into_iter()
                    .filter(|(i, _)| !used.contains(i))
                    .filter_map(|(i, k)| {
                        let d = leaving(&oriented(i, k))?;
                        Some((i, k, d[0] * heading[0] + d[1] * heading[1]))
                    })
                    .collect::<Vec<(usize, usize, f32)>>()
            };

            // **Every road out of a portal is a ramp, not only the straightest.**
            // A slip road that leaves at the portal leaves from seven metres
            // down just as the carriageway does: at the ring's eastern portal the
            // exit to the Europaweg was drawn at street level beside the trench,
            // on top of the sunken road the height survey still measures there
            // (2026-09-23). So each way that leaves within `JOIN_COS` is walked.
            for first in leaves(portal, outward, &[]) {
                let mut heading = outward;
                let mut used: Vec<usize> = Vec::new();
                let mut pieces: Vec<(usize, Vec<[f32; 2]>)> = Vec::new();
                let mut here = portal;
                // After the first way, a fork is a junction and ends the walk.
                for step in 0..APPROACH_MAX_WAYS {
                    let candidates = if step == 0 {
                        vec![first]
                    } else {
                        leaves(here, heading, &used)
                    };
                    if candidates.len() != 1 {
                        break;
                    }
                    let (i, k, cos) = candidates[0];
                    if cos < JOIN_COS || surface[i].bridge {
                        break;
                    }
                    let flags = StreetFlags {
                        oneway: flag_of(&features[surface[i].feature], "oneway")
                            || flag_of(&features[surface[i].feature], "oneway_reverse"),
                        oneway_reverse: flag_of(&features[surface[i].feature], "oneway_reverse"),
                        link: flag_of(&features[surface[i].feature], "link"),
                    };
                    if road_class(kind_of(&features[surface[i].feature]).unwrap_or(""), flags)
                        .is_none()
                    {
                        break;
                    }
                    let mut pts = oriented(i, k);
                    // Snapped to the portal, or the deck parts at the join.
                    pts[0] = here;
                    used.push(i);
                    let far = pts[pts.len() - 1];
                    if pts.len() >= 2 {
                        let (a, b) = (pts[pts.len() - 2], far);
                        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
                        let l = ex.hypot(ey).max(1e-6);
                        heading = [ex / l, ey / l];
                    }
                    pieces.push((surface[i].feature, pts));
                    if !surface[i].own[1 - k] {
                        break;
                    }
                    let so_far: f32 = pieces.iter().map(|(_, p)| polyline_len(p)).sum();
                    if so_far >= wanted {
                        break;
                    }
                    here = far;
                }
                if pieces.is_empty() {
                    continue;
                }

                // How far the ramp may run: to the first street that meets it, less a
                // little level road, or to the end of what was walked.
                let whole: Vec<[f32; 2]> = pieces
                    .iter()
                    .enumerate()
                    .flat_map(|(n, (_, p))| p.iter().skip(usize::from(n > 0)).copied())
                    .collect();
                let total = polyline_len(&whole);
                let mut usable = total;
                for (j, other) in surface.iter().enumerate() {
                    if used.contains(&j) || other.bridge {
                        continue;
                    }
                    if let Some(at) = first_meeting(&whole, &other.pts, SAME_POINT_M) {
                        usable = usable.min(at - APPROACH_CLEAR_M);
                    }
                }
                // The other tunnels too, bar this one: two portals side by side are a
                // dual carriageway, but a tunnel *crossing* the approach is a road
                // passing under it, and the ramp must not dig into its lid.
                for (j, other) in lines.iter().enumerate() {
                    if j == ri {
                        continue;
                    }
                    if let Some(at) = first_meeting(&whole, other, SAME_POINT_M) {
                        usable = usable.min(at - APPROACH_CLEAR_M);
                    }
                }
                // A walk the tile border stopped is fine as it is: the ramp then
                // reaches the surface by the border, where the neighbour draws the
                // same road at grade.
                if usable < APPROACH_CLEAR_M {
                    continue;
                }
                let usable = usable.min(wanted);

                let first = &features[pieces[0].0];
                let flags = StreetFlags {
                    oneway: flag_of(first, "oneway") || flag_of(first, "oneway_reverse"),
                    oneway_reverse: flag_of(first, "oneway_reverse"),
                    link: flag_of(first, "link"),
                };
                let kind = kind_of(first).unwrap_or("");
                let Some(class) = road_class(kind, flags) else {
                    continue;
                };
                out[ri][end].push(Approach {
                    pieces,
                    usable,
                    class,
                    half: road_width_m(kind, flags) * 0.5,
                });
            }
        }
    }
    out
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

/// One stretch of sunken road, resolved: where it runs, how deep it is at each
/// vertex, and which of those vertices have a lid over them.
///
/// **The profile is settled before anything is built**, which is the whole reason
/// this type exists. A cutting's depth is the depth of the lids at its two ends,
/// so there has to be a moment when every lid's answer is known and no geometry
/// has been emitted yet. It also makes the builder one loop over one kind of
/// thing, rather than one loop that knows about tags and a second that does not.
#[derive(Clone)]
struct Plan {
    /// Centre line, tile-local metres, with a vertex wherever the shape changes.
    centre: Vec<[f32; 2]>,
    /// Distance along `centre` at each vertex — the texture's `v`, kept rather
    /// than re-derived so the deck and the walls cannot disagree about it.
    travelled: Vec<f32>,
    /// Deck level at each vertex, negative.
    road_y: Vec<f32>,
    /// Whether each vertex has a lid over it. A cutting is `false` throughout:
    /// the tunnel flag is what says where a lid is, and it does not reach here.
    roofed: Vec<bool>,
    class: RoadClass,
    half: f32,
    clearance: f32,
    /// The `streets` feature this stretch came off — a tunnel-flagged way for a
    /// lid, the untagged way it absorbed for a cutting. Carried so the drive
    /// network can key a car's depth to its own road rather than to whatever
    /// happens to be under it ([`SunkenWay`]).
    feature: usize,
    /// Whether each end of the run opens onto sky — an entrance, or a cutting —
    /// so a lid that reaches it needs a headwall. False where the run carries on
    /// under the same lid: across a tile border, or into a split of its own way.
    mouth: [bool; 2],
}

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

/// Whether the plan triangle `a b c` comes out facing up once lifted into the
/// mesh frame (x east, z = tile y south) — the orientation the deck is wound in.
fn faces_up(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]) < -1e-6
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
    let out = TunnelTessellation::default();
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
    let mut unshared = open_ends(&lines);
    let cuttings = bridge_short_gaps(tile, tile_m, &lines, &mut unshared);
    let entrances: Vec<OpenEnds> = runs
        .iter()
        .zip(&unshared)
        .map(|((_, _, own), u)| [own[0] && u[0], own[1] && u[1]])
        .collect();
    let classes: Vec<Option<RoadClass>> = runs
        .iter()
        .map(|(fi, _, _)| {
            let feature = &features[*fi];
            let flags = StreetFlags {
                oneway: flag_of(feature, "oneway") || flag_of(feature, "oneway_reverse"),
                oneway_reverse: flag_of(feature, "oneway_reverse"),
                link: flag_of(feature, "link"),
            };
            road_class(kind_of(feature).unwrap_or(""), flags)
        })
        .collect();
    let approaches = find_approaches(tile, tile_m, &lines, &entrances, &classes);

    // **Every run's profile is resolved before any of it is built**, because a
    // cutting takes its depth from the lids at its two ends and cannot be planned
    // until they are. Parallel to `runs`, so `Cutting::joins` still indexes it
    // after the runs that carry no road class have dropped out.
    let mut planned: Vec<Option<Plan>> = Vec::with_capacity(runs.len());
    for (idx, (fi, centre, _)) in runs.iter().enumerate() {
        let feature = &features[*fi];
        let flags = StreetFlags {
            oneway: flag_of(feature, "oneway") || flag_of(feature, "oneway_reverse"),
            oneway_reverse: flag_of(feature, "oneway_reverse"),
            link: flag_of(feature, "link"),
        };
        let kind = kind_of(feature).unwrap_or("");
        let Some(class) = road_class(kind, flags) else {
            planned.push(None);
            continue;
        };
        let half = road_width_m(kind, flags) * 0.5;
        if half <= 0.0 {
            planned.push(None);
            continue;
        }
        let entrances = entrances[idx];
        let run_len: f32 = centre
            .windows(2)
            .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
            .sum();
        // The longest road out decides how much of the ramp the run can shed —
        // the carriageway, normally; a slip road that meets a junction sooner
        // is steepened to fit rather than allowed to hold the tunnel shallow.
        let outside = [0, 1].map(|k| {
            approaches[idx][k]
                .iter()
                .map(|a| a.usable)
                .fold(0.0f32, f32::max)
        });
        let (depth, grade) = depth_for_run(class, run_len, entrances, outside);
        let entry = entry_depths(depth, grade, entrances, outside);

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
        let centre = &with_ramp_breaks(centre, depth, grade, entrances, entry);

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
            .map(|&s| -depth_at_from(s, total, depth, grade, entrances, entry))
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
        planned.push(Some(Plan {
            centre: centre.clone(),
            travelled,
            road_y,
            roofed,
            class,
            half,
            clearance,
            feature: *fi,
            mouth: entrances,
        }));
    }

    // The open stretches between two lids, at the depth of the lids either side.
    //
    // Built here rather than in the loop above because it is the one run whose
    // profile is a function of its *neighbours*: holding it at its own idea of
    // full depth would step wherever a lid had to settle for less
    // ([`depth_for_run`]), which is the fault this is fixing, moved along by one
    // road. Never roofed — the tile says the lid stops here.
    let mut plans: Vec<Plan> = Vec::new();
    // The lid ends a cutting opens, for their headwalls.
    let mut open_joins: Vec<(usize, usize)> = Vec::new();
    for cut in &cuttings {
        let end_depth = |(run, end): (usize, usize)| -> Option<f32> {
            let p = planned.get(run)?.as_ref()?;
            let y = if end == 0 {
                p.road_y.first()
            } else {
                p.road_y.last()
            }?;
            Some(-*y)
        };
        let (Some(d0), Some(d1)) = (end_depth(cut.joins[0]), end_depth(cut.joins[1])) else {
            continue;
        };
        // Both lids at the surface is a pair of dips somebody tagged, not a
        // cutting, and sinking the road between them would dig a trench across
        // ground the map never said was open.
        if d0 <= ROOF_AT_DEPTH_M && d1 <= ROOF_AT_DEPTH_M {
            continue;
        }
        // **Snapped to the decks it joins.** The two ends were matched within
        // `SAME_POINT_M`, so leaving them where the surface way put them parts the
        // carriageway by up to a metre and a half at each lid — a slot in the road
        // at exactly the place this exists to close.
        let mut centre = cut.centre.clone();
        let anchor = |(run, end): (usize, usize)| -> Option<[f32; 2]> {
            let p = planned.get(run)?.as_ref()?;
            if end == 0 {
                p.centre.first().copied()
            } else {
                p.centre.last().copied()
            }
        };
        if let (Some(a), Some(b), true) = (
            anchor(cut.joins[0]),
            anchor(cut.joins[1]),
            centre.len() >= 2,
        ) {
            let last = centre.len() - 1;
            centre[0] = a;
            centre[last] = b;
        }
        let mut travelled = Vec::with_capacity(centre.len());
        let mut total = 0.0f32;
        for i in 0..centre.len() {
            if i > 0 {
                total += (centre[i][0] - centre[i - 1][0]).hypot(centre[i][1] - centre[i - 1][1]);
            }
            travelled.push(total);
        }
        if total <= 1e-3 {
            continue;
        }
        let road_y: Vec<f32> = travelled
            .iter()
            .map(|&s| -(d0 + (d1 - d0) * (s / total)))
            .collect();
        let clearance = if matches!(cut.class, RoadClass::Path) {
            PATH_CLEARANCE_M
        } else {
            CLEARANCE_M
        };
        // **A short gap is a seam in the tagging, not a stretch of sky.** The
        // deck runs through either way — that is what a cutting is for — but
        // taking the ground away over it is a claim the tile never made. On the
        // fixture two path tunnels end four metres apart, and cutting that
        // punched a 5 × 4 m hole in the ground over what is plainly one
        // continuous tunnel.
        let roofed: Vec<bool> = if total < MIN_CUTTING_M {
            road_y
                .iter()
                .map(|&y| y + clearance <= CEILING_MAX_Y)
                .collect()
        } else {
            open_joins.extend(cut.joins);
            vec![false; centre.len()]
        };
        plans.push(Plan {
            roofed,
            centre,
            travelled,
            road_y,
            class: cut.class,
            // The wider of the two, so the hole this opens in the ground always
            // swallows the surface ribbon it is replacing *and* meets the lid's
            // wall without a step where the two widths disagree.
            half: cut
                .half
                .max(planned[cut.joins[0].0].as_ref().map_or(0.0, |p| p.half)),
            clearance,
            feature: cut.feature,
            mouth: [false; 2],
        });
    }
    for (idx, plan) in planned.iter().enumerate() {
        let Some(plan) = plan else {
            continue;
        };
        let mut plan = plan.clone();
        for (k, mouth) in plan.mouth.iter_mut().enumerate() {
            *mouth |= open_joins.contains(&(idx, k));
        }
        plans.push(plan);
    }

    // The ramps outside the portals, now that each run's depth at its ends is
    // known. Never roofed: the tag stops at the portal, and so does the lid.
    for (idx, ends) in approaches.iter().enumerate() {
        let Some(run) = planned.get(idx).and_then(Option::as_ref) else {
            continue;
        };
        for (end, approach) in ends
            .iter()
            .enumerate()
            .flat_map(|(end, list)| list.iter().map(move |a| (end, a)))
        {
            let e = -if end == 0 {
                run.road_y[0]
            } else {
                run.road_y[run.road_y.len() - 1]
            };
            if e <= 0.05 {
                continue;
            }
            let (_, grade) = depth_and_grade(run.class);
            let length = ramp_m(e, grade).min(approach.usable);
            let clearance = if matches!(approach.class, RoadClass::Path) {
                PATH_CLEARANCE_M
            } else {
                CLEARANCE_M
            };
            let mut walked = 0.0f32;
            for (feature, pts) in &approach.pieces {
                if walked >= length - 0.05 {
                    break;
                }
                let piece = cut_at(pts, length - walked);
                let mut travelled = Vec::with_capacity(piece.len());
                let mut along = 0.0f32;
                for i in 0..piece.len() {
                    if i > 0 {
                        along +=
                            (piece[i][0] - piece[i - 1][0]).hypot(piece[i][1] - piece[i - 1][1]);
                    }
                    // Continuous across pieces, so the markings do not jump.
                    travelled.push(walked + along);
                }
                if along <= 1e-3 {
                    break;
                }
                let road_y: Vec<f32> = travelled
                    .iter()
                    .map(|&s| -(e * (1.0 - s / length)).max(0.0))
                    .collect();
                walked += along;
                plans.push(Plan {
                    roofed: vec![false; piece.len()],
                    centre: piece,
                    travelled,
                    road_y,
                    class: approach.class,
                    // The wider of the two, as a cutting takes it: the hole has to
                    // swallow the surface ribbon *and* meet the tunnel's own walls.
                    half: approach.half.max(run.half),
                    clearance,
                    feature: *feature,
                    mouth: [false; 2],
                });
            }
        }
    }

    emit_plans(&plans, max_bytes, false)
}

/// One street run and its solved heights, as [`build_tunnels_on_levels`] takes it.
pub struct LevelledRun<'a> {
    pub run: &'a crate::road_levels::StreetRun,
    /// The run's solved height at each of its points, in the caller's frame —
    /// the frame `here` and `lid` answer in.
    pub ys: &'a [f32],
}

/// How far below the ground under it a road must run to be built as a sunken
/// stretch — walls either side and the ground opened over it — metres. The
/// ground is laid 5 cm under an ordinary road (`terrain_grade::UNDER_ROAD_M`), so
/// this is well clear of that and well short of any real cutting.
pub const SUNK_M: f32 = 0.3;

/// Tunnels, ramps and cuttings from the heights the road network was solved to
/// (`crate::road_levels`), instead of from a plan invented here.
///
/// **What the heights retire.** [`build_tunnels`] decides every depth itself:
/// which ends are entrances ([`open_ends`]), how much ramp fits on the road
/// beyond each portal ([`find_approaches`]), how deep a run too short for its
/// ramps may go ([`depth_for_run`]), what a cutting between two lids is held at
/// ([`bridge_short_gaps`]) — all within one tile, so a ramp crossing a tile edge
/// was held at full depth where the border cut it. A solved height already
/// answers every one of those, on the network over the tile and its neighbours:
/// a tunnel's lid is a ceiling its road stays under, and the ramp down to it is
/// the grade limit working from that ceiling, the mirror of an embankment up to
/// a deck.
///
/// So this only reads the heights. Every tunnel run is a corridor at its solved
/// depth below the ground under it (`here`), lidded wherever a roof fits under
/// the land around it (`lid`, the level a lid is filled back to); every
/// ground-level run contributes the stretches that run more than [`SUNK_M`]
/// under the ground — the ramps and the cuttings — open to the sky. The meshes,
/// corridors and openings are then [`build_tunnels`]' own, and the depths are
/// marked measured so nothing lifts them again.
#[must_use]
pub fn build_tunnels_on_levels(
    runs: &[LevelledRun<'_>],
    here: &dyn Fn([f32; 2]) -> f32,
    lid: &dyn Fn([f32; 2]) -> f32,
    max_bytes: Option<usize>,
) -> TunnelTessellation {
    let clearance_of = |class: RoadClass| {
        if matches!(class, RoadClass::Path) {
            PATH_CLEARANCE_M
        } else {
            CLEARANCE_M
        }
    };
    let travelled_of = |centre: &[[f32; 2]]| -> Vec<f32> {
        let mut out = Vec::with_capacity(centre.len());
        let mut total = 0.0f32;
        for (i, p) in centre.iter().enumerate() {
            if i > 0 {
                total += (p[0] - centre[i - 1][0]).hypot(p[1] - centre[i - 1][1]);
            }
            out.push(total);
        }
        out
    };
    let usable: Vec<&LevelledRun<'_>> = runs
        .iter()
        .filter(|r| r.ys.len() == r.run.points.len() && r.run.points.len() >= 2)
        .collect();

    // A tunnel end opens onto sky when the line ends there, the border did not cut
    // it, and no second tunnel run ends on the same point — the rule
    // `build_tunnels` applies, on the same lines.
    let bores: Vec<&LevelledRun<'_>> = usable.iter().copied().filter(|r| r.run.level < 0).collect();
    let bore_lines: Vec<Vec<[f32; 2]>> = bores.iter().map(|r| r.run.points.clone()).collect();
    let unshared = open_ends(&bore_lines);

    let mut plans: Vec<Plan> = Vec::new();
    for (b, u) in bores.iter().zip(&unshared) {
        let centre = b.run.points.clone();
        let clearance = clearance_of(b.run.class);
        let road_y: Vec<f32> = centre
            .iter()
            .zip(b.ys)
            .map(|(&p, &y)| y - here(p))
            .collect();
        // A roof where one fits under the land around: the ceiling at least as far
        // under the lid as `build_tunnels` keeps it under the ground.
        let roofed: Vec<bool> = centre
            .iter()
            .zip(b.ys)
            .map(|(&p, &y)| y + clearance <= lid(p) + CEILING_MAX_Y)
            .collect();
        plans.push(Plan {
            travelled: travelled_of(&centre),
            centre,
            road_y,
            roofed,
            class: b.run.class,
            half: b.run.half_w,
            clearance,
            feature: b.run.feature,
            mouth: [!b.run.cut[0] && u[0], !b.run.cut[1] && u[1]],
        });
    }

    // The ramps and cuttings: every stretch of a ground-level road that runs under
    // the ground, with one vertex either side so the corridor starts at the
    // surface rather than already below it.
    for r in usable.iter().filter(|r| r.run.level == 0) {
        let pts = &r.run.points;
        let depth: Vec<f32> = pts.iter().zip(r.ys).map(|(&p, &y)| here(p) - y).collect();
        let mut i = 0;
        while i < pts.len() {
            if depth[i] <= SUNK_M {
                i += 1;
                continue;
            }
            let start = i.saturating_sub(1);
            let mut end = i;
            while end + 1 < pts.len() && depth[end + 1] > SUNK_M {
                end += 1;
            }
            let end = (end + 1).min(pts.len() - 1);
            i = end + 1;
            if end <= start {
                continue;
            }
            let centre: Vec<[f32; 2]> = pts[start..=end].to_vec();
            let road_y: Vec<f32> = (start..=end).map(|k| r.ys[k] - here(pts[k])).collect();
            plans.push(Plan {
                travelled: travelled_of(&centre),
                roofed: vec![false; centre.len()],
                centre,
                road_y,
                class: r.run.class,
                half: r.run.half_w,
                clearance: clearance_of(r.run.class),
                feature: r.run.feature,
                mouth: [false; 2],
            });
        }
    }

    emit_plans(&plans, max_bytes, true)
}

/// Build the meshes, corridors and openings for a set of settled plans — the half
/// of the tunnel builder that does not care where the depths came from.
///
/// `measured` says the plans' deck levels are already the road's own height
/// relative to the ground under it (`build_tunnels_on_levels`), rather than the
/// convention that placement lifts back by whatever the survey dug.
fn emit_plans(plans: &[Plan], max_bytes: Option<usize>, measured: bool) -> TunnelTessellation {
    let mut out = TunnelTessellation::default();
    // **One deck builder per class, not one for the tile.** A tunnel's carriageway
    // is the surface road continued, so it wants that road's own texture: gravel
    // under a footpath, tarmac and lane markings under a trunk road. This was a
    // single builder and a single `Option<RoadClass>` set by whichever tunnel came
    // first, so a cell with a cycle underpass and a motorway tunnel drew both as
    // whichever it met first — visibly, over Groningen's ring, as a motorway
    // surfaced like a towpath (2026-09-20).
    let mut decks: Vec<(RoadClass, Build)> = Vec::new();
    // The class the open deck buffer is drawing markings for. A batch takes the
    // class of the run that opened it, exactly as the ground road sheet's
    // per-group meshes do — the alternative is a mesh per class per cell, for a
    // difference that is which dashes are painted on a carriageway nobody sees
    // two of at once.

    let mut shell = Build::default();
    let mut meshes: Vec<SurfaceMesh> = Vec::new();
    // Openings carry the corridor they came out of until the merge below has had
    // its say — see [`merge_parallel_openings`].
    let mut openings: Vec<(RoadClass, f32, Vec<[f32; 2]>)> = Vec::new();

    for plan in plans {
        let Plan {
            centre,
            travelled,
            road_y,
            roofed,
            class,
            half,
            clearance,
            mouth,
            ..
        } = plan;
        let (class, half, clearance) = (*class, *half, *clearance);
        let sides = offset_sides(centre, half);
        let deck_slot = match decks.iter().position(|(c, _)| *c == class) {
            Some(i) => i,
            None => {
                decks.push((class, Build::default()));
                decks.len() - 1
            }
        };
        let deck = &mut decks[deck_slot].1;
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
            //
            // **Wound to face up, and it was not.** The order below is the left
            // edge first, which in this frame — x east, y south — is the way round
            // that presents the *top* face to a camera above it. It used to run
            // left, right, right, left and so faced down: backface-culled from
            // every angle anybody looks at a road from, so the carriageway simply
            // was not there. The walls and the roof were fine, which is what made
            // it read as "the tunnel has no road in it" rather than as a winding
            // fault — and what a capture showed was the surface road running into
            // the ground and stopping. `a_tunnel_deck_faces_up` pins it, the same
            // way `ribbon_faces_up` pins the surface roads and for the same
            // reason: winding is invisible until it is wrong, and then it is
            // invisible in the other direction.
            deck.quad(
                [
                    [l0[0], y0, l0[1]],
                    [l1[0], y1, l1[1]],
                    [r1[0], y1, r1[1]],
                    [r0[0], y0, r0[1]],
                ],
                [[0.0, v0], [0.0, v1], [1.0, v1], [1.0, v0]],
                UP,
                rung,
            );

            // Walls, inward-facing: the outer face is buried in the ground the
            // tunnel is cut into and no camera can reach it.
            //
            // **Inward in the winding, not only in the normal.** The material
            // culls back faces, and both walls were wound outward while their
            // normals said inward — so from the deck, the one place a wall is
            // ever seen from, there were none, and the ground's own culled
            // underside left the city showing through
            // (`every_tunnel_face_is_wound_toward_its_normal`).
            //
            // **Up to the ceiling under a lid, and to the ground only where it is
            // open.** Between a roof and the ground over it is earth, which has
            // no mesh; a wall carried on up to street level stood out of anything
            // lower than the street — along the ring's lid under the Oude
            // Winschoterdiep the two wall tops ran out of the canal as long thin
            // slivers (2026-09-23).
            let covered = roofed[i] && roofed[i + 1];
            let (t0, t1) = if covered {
                (y0 + clearance, y1 + clearance)
            } else {
                (WALL_TOP_Y, WALL_TOP_Y)
            };
            let n_left = wall_normal(l0, l1, true);
            shell.quad(
                [
                    [l1[0], y1, l1[1]],
                    [l0[0], y0, l0[1]],
                    [l0[0], t0, l0[1]],
                    [l1[0], t1, l1[1]],
                ],
                [[v1, 0.0], [v0, 0.0], [v0, 1.0], [v1, 1.0]],
                n_left,
                0.0,
            );
            let n_right = wall_normal(r0, r1, false);
            shell.quad(
                [
                    [r0[0], y0, r0[1]],
                    [r1[0], y1, r1[1]],
                    [r1[0], t1, r1[1]],
                    [r0[0], t0, r0[1]],
                ],
                [[v0, 0.0], [v1, 0.0], [v1, 1.0], [v0, 1.0]],
                n_right,
                0.0,
            );

            // Roof, where one fits under the ground. Absent over the approach,
            // which is what opens the opening — see the module note. Wound the
            // other way round from the deck, so it faces *down*: it was wound
            // like the deck, faced the earth above it, and was culled from
            // underneath — a lid you could see the sky through.
            let (c0, c1) = (y0 + clearance, y1 + clearance);
            if roofed[i] && roofed[i + 1] {
                shell.quad(
                    [
                        [l0[0], c0, l0[1]],
                        [r0[0], c0, r0[1]],
                        [r1[0], c1, r1[1]],
                        [l1[0], c1, l1[1]],
                    ],
                    [[0.0, v0], [1.0, v0], [1.0, v1], [0.0, v1]],
                    DOWN,
                    0.0,
                );
            }

            // The notch on the outside of a bend, in the deck, the wall on that
            // side and the roof.
            //
            // **Only on the outside.** Both sides used to be emitted on the
            // reasoning that the inner one is "degenerate to a hair", and it is
            // not: on the inside of a bend the two offset edges *cross*, so its
            // triangle is a real one wound backwards and its wall quad stands
            // inside the carriageway facing the wrong way. The side is the one
            // whose triangle comes out facing up — the same orientation test the
            // deck is held to — so the rule cannot disagree with the winding.
            if i + 1 < sides.left.len() {
                let p = centre[i + 1];
                let y = road_y[i + 1];
                let c = y + clearance;
                let v = v1;
                let next_l = sides.left[i + 1][0];
                let next_r = sides.right[i + 1][0];
                let roof = roofed[i + 1];
                for (a, b, u) in [(l1, next_l, 0.0), (next_r, r1, 1.0)] {
                    if !faces_up(p, a, b) {
                        continue;
                    }
                    deck.tri(
                        [[p[0], y, p[1]], [a[0], y, a[1]], [b[0], y, b[1]]],
                        [[0.5, v], [u, v], [u, v]],
                        UP,
                        rung,
                    );
                    if roof {
                        shell.tri(
                            [[p[0], c, p[1]], [b[0], c, b[1]], [a[0], c, a[1]]],
                            [[0.5, v], [u, v], [u, v]],
                            DOWN,
                            0.0,
                        );
                    }
                    // A vertical quad closing the gap between the two offset
                    // points. Without it a bend is a slot you can see the void
                    // through from inside the tunnel. Faces back toward the
                    // centre of the bend, which is inward.
                    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
                    let len = dx.hypot(dz).max(1e-6);
                    let top = if roof { c } else { WALL_TOP_Y };
                    shell.quad(
                        [
                            [b[0], y, b[1]],
                            [a[0], y, a[1]],
                            [a[0], top, a[1]],
                            [b[0], top, b[1]],
                        ],
                        [[v, 0.0], [v, 0.0], [v, 1.0], [v, 1.0]],
                        [dz / len, 0.0, -dx / len],
                        0.0,
                    );
                }
            }
        }

        // **A headwall over every mouth.** Where a lid stops, the ground over it
        // stops too — it is cut away over the open stretch beyond — and between
        // the roof's edge and the ground there was nothing: the earth has no
        // mesh, so from the cutting the mouth showed the lid's culled underside
        // and the grass on top of it (2026-09-23). A vertical face across the
        // corridor from the ceiling up to the wall tops closes it, facing out of
        // the tunnel toward the open side.
        let n = centre.len();
        for k in 0..n {
            if !roofed[k] {
                continue;
            }
            // (roofed side of the mouth, left and right corner, facing).
            let mut faces: Vec<([f32; 2], [f32; 2], [f32; 2])> = Vec::new();
            let back_open = if k == 0 { mouth[0] } else { !roofed[k - 1] };
            if back_open && k + 1 < n && roofed[k + 1] {
                let (a, b) = (centre[k], centre[k + 1]);
                faces.push((
                    sides.left[k][0],
                    sides.right[k][0],
                    [a[0] - b[0], a[1] - b[1]],
                ));
            }
            let ahead_open = if k + 1 == n { mouth[1] } else { !roofed[k + 1] };
            if ahead_open && k > 0 && roofed[k - 1] {
                let (a, b) = (centre[k - 1], centre[k]);
                faces.push((
                    sides.left[k - 1][1],
                    sides.right[k - 1][1],
                    [b[0] - a[0], b[1] - a[1]],
                ));
            }
            let c = road_y[k] + clearance;
            for (l, r, d) in faces {
                let len = d[0].hypot(d[1]).max(1e-6);
                let out = [d[0] / len, 0.0, d[1] / len];
                // Wound to face `out`, whichever way round the corners lie
                // against it — a mouth faces forward at one end and back at the other.
                let forward = (r[0] - l[0]) * d[1] - (r[1] - l[1]) * d[0] > 0.0;
                let (p, q) = if forward { (l, r) } else { (r, l) };
                shell.quad(
                    [
                        [p[0], c, p[1]],
                        [q[0], c, q[1]],
                        [q[0], WALL_TOP_Y, q[1]],
                        [p[0], WALL_TOP_Y, p[1]],
                    ],
                    [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                    out,
                    0.0,
                );
            }
        }

        // The roofless stretches, as plan corridors for the caller to cut out of
        // the ground. Keyed on the roof being missing rather than on the entrance
        // flag, so the hole and the absent ceiling are decided by one test and
        // cannot disagree about where the opening is.
        openings.extend(
            opening_rings(centre, roofed, half)
                .into_iter()
                .map(|ring| (class, half, ring)),
        );

        out.tunnels.push(Tunnel {
            centre: centre.clone(),
            road_y: road_y.clone(),
            half_w: half,
            clearance,
            covered: roofed.clone(),
            measured,
        });
        // The same corridor, keyed to its street, for the drive network. Only
        // where it is genuinely below the surface: a flagged way that never
        // descends is an arcade, and a car on one is on the ground.
        if road_y.iter().any(|&y| y < -0.05) {
            out.sunken.push(SunkenWay {
                feature: plan.feature,
                centre: centre.clone(),
                road_y: road_y.clone(),
                half_w: half,
                measured,
            });
        }

        // Cut the meshes loose as they fill, the same contract every other
        // builder in this crate keeps.
        if let Some(limit) = max_bytes {
            if deck.positions.len() * VERTEX_BYTES >= limit && !deck.is_empty() {
                let group = SurfaceGroup::TunnelRoad(class);
                meshes.push(std::mem::take(deck).into_mesh(group));
            }
            if shell.positions.len() * VERTEX_BYTES >= limit && !shell.is_empty() {
                meshes.push(std::mem::take(&mut shell).into_mesh(SurfaceGroup::TunnelStructure));
            }
        }
    }

    for (class, deck) in decks {
        if !deck.is_empty() {
            meshes.push(deck.into_mesh(SurfaceGroup::TunnelRoad(class)));
        }
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
    out.openings = merge_parallel_openings(openings);
    out.meshes = meshes;
    out
}

/// Farthest apart two corridors may be and still be one trench, in metres.
///
/// A Dutch dual carriageway's halves are mapped 10–20 m apart. Thirty takes those
/// and stops well short of the next parallel street.
const MEDIAN_M: f32 = 30.0;

/// Replace pairs of near, parallel corridors with the corridor containing both.
/// **Only the two halves of one road**, which is what the carriageway key is for.
///
/// The proximity half of the test is an overlap of *bounding boxes* grown by
/// [`MEDIAN_M`], and for two long diagonal corridors that is satisfied a long way
/// before the corridors themselves are near: measured over the DUO cut on
/// 2026-09-22, a 46 m footpath and the ring's carriageway **88 m away** passed it,
/// were parallel to within a degree, and hulled a 130 × 102 m crater out of the
/// park between them. Tightening the distance does not settle it either — the two
/// rings come within 19 m at one corner, and a cutting legitimately *touches* the
/// lid it joins.
///
/// What actually distinguishes the case this exists for is that a dual
/// carriageway's halves are the same road: same class, same width, mapped as two
/// ways. So that is the key, and a footpath can no longer be hulled into a trunk
/// road however parallel it happens to run.
fn merge_parallel_openings(openings: Vec<(RoadClass, f32, Vec<[f32; 2]>)>) -> Vec<Vec<[f32; 2]>> {
    let mut out: Vec<Vec<[f32; 2]>> = Vec::with_capacity(openings.len());
    let mut merged = vec![false; openings.len()];
    for i in 0..openings.len() {
        if merged[i] {
            continue;
        }
        let (class, half, ref first) = openings[i];
        let mut ring = first.clone();
        for j in (i + 1)..openings.len() {
            let (other_class, other_half, ref other) = openings[j];
            if merged[j]
                || other_class != class
                || (other_half - half).abs() > 0.01
                || !parallel_and_near(&ring, other)
            {
                continue;
            }
            let mut both = ring.clone();
            both.extend_from_slice(other);
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
    if (da[0] * db[0] + da[1] * db[1]).abs() < JOIN_COS {
        return false;
    }
    // **Beside, not beyond** — the rule `open_ends` already learned. The two
    // halves of a dual carriageway overlap along the road; the two ends of *one*
    // short tunnel are parallel, near, the same road and the same width too, and
    // lie end to end with the lid between them. Since the ramps moved out past
    // the portals a tunnel can open at one end outside and the other inside, and
    // hulling that pair filled the whole lid in with a hole
    // (`no_opening_swallows_a_roofed_stretch`).
    let span = |r: &[[f32; 2]]| {
        r.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            let t = p[0] * da[0] + p[1] * da[1];
            (lo.min(t), hi.max(t))
        })
    };
    let ((a0, a1), (b0, b1)) = (span(a), span(b));
    let overlap = a1.min(b1) - a0.max(b0);
    overlap >= 0.3 * (a1 - a0).min(b1 - b0)
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
            | SurfaceGroup::QuayWall
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
mod terrain_tests {
    use super::{lift_for, stand_tunnel_on, GroundField, Lidded, Survey, Tunnel, DATUM_M};

    /// A survey that has already dug a trench `dug` metres deep everywhere.
    struct Dug(f32);

    impl Survey for Dug {
        fn here(&self, _x: f32, _z: f32) -> f32 {
            -self.0
        }
        fn around(&self, _x: f32, _z: f32) -> f32 {
            0.0
        }
    }

    impl GroundField for Dug {
        fn min_ground_edge_m(&self) -> f32 {
            crate::terrain_refine::MIN_GROUND_EDGE_M
        }
        fn at(&self, _x: f32, _z: f32) -> f32 {
            -self.0
        }
    }

    /// The ground a mesh is actually bent onto over `t`, with `dug` metres already
    /// measured out of it. Handed to a closure rather than returned: `Lidded`
    /// borrows the two closures it is built from, which cannot outlive the call
    /// that makes them.
    fn lidded<R>(ground: &Dug, t: &Tunnel, f: impl FnOnce(&Lidded<'_>) -> R) -> R {
        let around = |_x: f32, _z: f32| 0.0;
        let weight = |p: [f32; 2]| t.covered_weight(p, DATUM_M);
        f(&Lidded {
            ground,
            around: &around,
            weight: &weight,
        })
    }

    /// One straight tunnel 200 m long, 10 m wide, its deck 7 m down, with
    /// `covered` as given per vertex.
    /// **A tunnel built from solved heights**: the bore is lidded where the roof
    /// fits, the road running up out of it is an open ramp whose ground is opened,
    /// and everything is marked measured so nothing lifts it again.
    #[test]
    fn a_tunnel_is_built_from_solved_heights() {
        use super::{build_tunnels_on_levels, LevelledRun, RoadClass, SurfaceGroup};
        use crate::road_levels::{densify, StreetRun};
        let run = |level: i8, from: f32, to: f32, feature: usize| StreetRun {
            key: 0,
            feature,
            level,
            class: RoadClass::Carriageway,
            half_w: 5.0,
            points: densify(&[[from, 0.0], [to, 0.0]], 3.0),
            cut: [false; 2],
        };
        let bore = run(-1, 0.0, 200.0, 0);
        let ramp = run(0, 200.0, 400.0, 1);
        let bore_ys: Vec<f32> = bore.points.iter().map(|_| -7.0).collect();
        // Up at 6 % from -7 m at the portal to the ground 117 m on.
        let ramp_ys: Vec<f32> = ramp
            .points
            .iter()
            .map(|p| (-7.0 + (p[0] - 200.0) * 0.06).min(0.0))
            .collect();
        let out = build_tunnels_on_levels(
            &[
                LevelledRun {
                    run: &bore,
                    ys: &bore_ys,
                },
                LevelledRun {
                    run: &ramp,
                    ys: &ramp_ys,
                },
            ],
            &|_| 0.0,
            &|_| 0.0,
            None,
        );
        assert!(out.tunnels.iter().all(|t| t.measured));
        assert!(out.sunken.iter().all(|w| w.measured));
        let lidded = out
            .tunnels
            .iter()
            .find(|t| t.covered.iter().any(|&c| c))
            .expect("no lid");
        assert!((lidded.road_under([100.0, 0.0]).unwrap() + 7.0).abs() < 1e-4);
        assert!(
            out.openings
                .iter()
                .any(|ring| ring.iter().any(|p| p[0] > 250.0)),
            "the ramp's ground was not opened"
        );
        assert!(
            !out.openings
                .iter()
                .any(|ring| ring.iter().any(|p| p[0] > 330.0)),
            "the ground was opened past where the ramp surfaces"
        );
        assert!(out
            .meshes
            .iter()
            .any(|m| matches!(m.group, SurfaceGroup::TunnelRoad(_))));
        assert!(out
            .meshes
            .iter()
            .any(|m| m.group == SurfaceGroup::TunnelStructure));
    }

    fn bore(covered: [bool; 3]) -> Tunnel {
        Tunnel {
            centre: vec![[0.0, 0.0], [100.0, 0.0], [200.0, 0.0]],
            road_y: vec![-7.0, -7.0, -7.0],
            half_w: 5.0,
            clearance: super::CLEARANCE_M,
            covered: covered.to_vec(),
            measured: false,
        }
    }

    /// **A tunnel must not be dug twice.** The survey's bare-ground model has no
    /// roof to hide the bottom of a cutting, so over an open cut it measures the
    /// trench — and displacing an invented seven metres by a measured nine puts the
    /// carriageway sixteen metres under the street, with its wall tops buried.
    ///
    /// Three cases and one piece of arithmetic settles all of them, which is why
    /// they are one test: a fully measured cut lands the deck exactly on the
    /// measured floor, a lid the laser flew over (nothing dug) keeps the whole
    /// convention, and a partly measured cut absorbs what was measured and invents
    /// the rest.
    #[test]
    fn a_tunnel_is_not_dug_twice() {
        let depth = |_: [f32; 2]| Some(7.0);
        for (dug, want) in [(7.0, -7.0), (0.0, -7.0), (3.0, -7.0), (9.0, -9.0)] {
            let mut deck = vec![[50.0, -7.0, 0.0]];
            stand_tunnel_on(&mut deck, [0.0, 0.0], &Dug(dug), &depth);
            assert!(
                (deck[0][1] - want).abs() < 1e-3,
                "a {dug} m cut put the deck at {} m, not {want} m",
                deck[0][1]
            );
        }
    }

    /// The lift is capped by the depth at *that place*, so it can raise a deck to
    /// the measured ground and no further. Without the cap a survey that dug deeper
    /// than the convention lifts the deck above the land beside it — the
    /// carriageway standing proud of the ground.
    #[test]
    fn the_lift_never_exceeds_the_depth() {
        assert!((lift_for(7.0, 20.0) - 7.0).abs() < 1e-6);
        assert!((lift_for(2.4, 6.4) - 2.4).abs() < 1e-6);
        // Rising ground rather than a cutting is not a lift at all.
        assert!(lift_for(7.0, -3.0).abs() < 1e-6);
    }

    /// **Where a road has a lid, the ground over it is whole**, and the survey —
    /// flown when the road was an open trench — says it is not. Over a covered
    /// stretch the ground is lifted back to the land around it; over an open one
    /// nothing happens, because there the two sources agree that the ground is
    /// missing.
    #[test]
    fn ground_is_lifted_over_a_lid_and_left_alone_over_a_gap() {
        let ground = Dug(9.0);
        let covered = bore([true, true, true]);
        let open = bore([false, false, false]);

        let lid = lidded(&ground, &covered, |f| f.at(100.0, 0.0));
        assert!(lid.abs() < 1e-3, "ground over a lid stayed {lid} m down");

        let gap = lidded(&ground, &open, |f| f.at(100.0, 0.0));
        assert!(
            (gap + 9.0).abs() < 1e-3,
            "ground over an open cut was filled in to {gap} m"
        );
    }

    /// The fill tapers rather than stopping, or it leaves a crease along the line
    /// where it ends — a straight-sided plateau in the middle of a park.
    #[test]
    fn the_fill_tapers_out_rather_than_creasing() {
        let ground = Dug(9.0);
        let t = bore([true, true, true]);
        lidded(&ground, &t, |field| {
            let at = |off: f32| field.at(100.0, off);
            // Over the carriageway, fully lifted; a datum out, not lifted at all;
            // and monotone in between.
            assert!(at(0.0).abs() < 1e-3);
            assert!((at(5.0 + DATUM_M + 1.0) + 9.0).abs() < 1e-3);
            let mid = at(5.0 + DATUM_M * 0.5);
            assert!(
                mid < -1.0 && mid > -8.0,
                "the taper's midpoint is {mid} m, which is a step and not a slope"
            );
        });
    }

    /// **The lid has to be part of the surface the refinement resolves.**
    ///
    /// A mesh is split until the field departs from its own chord by less than
    /// `CREASE_SAG_M`. Over a lid the lift is nine metres across a thirty-metre
    /// taper, so an unrefined edge crossing it misses by metres — and the mesh
    /// next to it, subdivided differently, misses by a different amount. That
    /// difference is a hole in the ground along the buried road.
    ///
    /// This pins the property that makes the refinement close it: the field the
    /// crease rule reads must *be* the ground with the lid on, so the sag it
    /// measures is the real one.
    #[test]
    fn the_lid_is_visible_to_the_crease_rule() {
        let ground = Dug(9.0);
        let t = bore([true, true, true]);
        lidded(&ground, &t, |field| {
            // A 60 m edge straight across the corridor: its ends are down in the
            // trench's datum and its middle is up on the lid.
            let (a, b) = ([100.0, -50.0], [100.0, 50.0]);
            let chord = f32::midpoint(field.at(a[0], a[1]), field.at(b[0], b[1]));
            let sag = (field.at(100.0, 0.0) - chord).abs();
            assert!(
                sag > crate::terrain_refine::CREASE_SAG_M,
                "the lid sags {sag} m under its own chord, which the crease rule \
                 would not split — so the lift is invisible to it and the ground tears"
            );
        });
    }
}

#[cfg(test)]
mod tests {
    /// **A tunnel's carriageway must face up**, or the road vanishes at the
    /// portal.
    ///
    /// The deck was wound downward and so was backface-culled from above — the
    /// one direction anybody looks at a road from. The walls and the roof were
    /// wound correctly, so a tunnel drew its structure and no carriageway, which
    /// reads as "the tunnel has no road in it" and not as a winding fault. What it
    /// looked like over Groningen's ring is a motorway running into the ground and
    /// stopping (2026-09-20).
    ///
    /// Same rule and same test as `vector_tiles`' `ribbon_faces_up`: in this frame
    /// — x east, y south — an up-facing triangle has a non-positive cross product.
    #[test]
    fn a_tunnel_deck_faces_up() {
        let out = build_tunnels(&crate::testdata::groningen(), 1471.0, None);
        let mut checked = 0usize;
        for m in &out.meshes {
            if !matches!(m.group, SurfaceGroup::TunnelRoad(_)) {
                continue;
            }
            for t in m.indices.chunks_exact(3) {
                let (a, b, c) = (
                    m.positions[t[0] as usize],
                    m.positions[t[1] as usize],
                    m.positions[t[2] as usize],
                );
                let cross = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
                assert!(
                    cross <= 1e-4,
                    "a tunnel deck triangle is wound downward and will be culled \
                     from above: {a:?} {b:?} {c:?}"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "the fixture built no tunnel deck to check");
    }

    /// **A tunnel meets the surface road at ground level, or the road jumps.**
    ///
    /// The deck is the same carriageway the surface ribbon is, continued — same
    /// width, same texture, same markings — so where the tunnel has an entrance
    /// its first vertex has to sit at `y = 0`. A step there is a cliff the road
    /// falls off, and at a shallow angle it reads as the tunnel being somewhere
    /// else entirely.
    ///
    /// The other end of the same rule: a vertex with no entrance is a tunnel that
    /// continues past the tile border or into another run, and it must *not* come
    /// up to the surface there — that was the old fault where every tunnel rose to
    /// meet a border it does not really reach.
    #[test]
    fn a_tunnel_meets_the_road_at_its_portal() {
        let out = build_tunnels(&crate::testdata::groningen(), 1471.0, None);
        assert!(!out.tunnels.is_empty(), "the fixture has tunnels");
        for t in &out.tunnels {
            let (first, last) = (t.road_y[0], t.road_y[t.road_y.len() - 1]);
            // An entrance is the only way a deck reaches the surface, and every
            // deck level is at or below it.
            assert!(first <= 0.001, "a deck starts above ground at {first} m");
            assert!(last <= 0.001, "a deck ends above ground at {last} m");
            // Whatever the depth, the deck never exceeds the class's own.
            let deepest = t.road_y.iter().fold(0.0f32, |a, y| a.min(*y));
            assert!(
                deepest >= -LEVEL_DEPTH_M - 0.01,
                "a deck reaches {deepest} m, past the {LEVEL_DEPTH_M} m this class digs"
            );
        }
    }

    /// **A roofed tunnel has headroom under its lid.**
    ///
    /// `depth_for_run` reduces the depth of a short run so that something is
    /// covered at all, and the floor under that reduction is the depth at which
    /// the ceiling still clears the ground. If it ever goes below, the lid is
    /// drawn *inside* the carriageway and the road runs through its own roof.
    #[test]
    fn a_roofed_deck_keeps_its_headroom() {
        let out = build_tunnels(&crate::testdata::groningen(), 1471.0, None);
        for t in &out.tunnels {
            for (i, covered) in t.covered.iter().enumerate() {
                if !covered {
                    continue;
                }
                let ceiling = t.road_y[i] + t.clearance;
                assert!(
                    ceiling <= CEILING_MAX_Y + 0.01,
                    "a lid at {ceiling} m is above the {CEILING_MAX_Y} m it must clear"
                );
            }
        }
    }

    /// **Every face of a tunnel is wound toward the side its normal says**, which
    /// for the roof and the walls is the *inside*.
    ///
    /// The tunnel material culls back faces, so a winding that disagrees with the
    /// normal is not a lighting fault, it is a surface that is not there from the
    /// only side anybody sees it from. The roof was wound like the deck — facing
    /// up, into the earth — and both walls faced outward, so from inside a lid the
    /// tunnel had no ceiling and no sides: the underside of the ground is culled
    /// too, and a phone on the deck saw sky, the far city and the lawn's grass
    /// hanging overhead (2026-09-23). `a_tunnel_deck_faces_up` only ever checked
    /// the deck.
    #[test]
    fn every_tunnel_face_is_wound_toward_its_normal() {
        let tile_m = ring_tile_m();
        let mut checked = [0usize; 2];
        for out in [
            build_tunnels(&crate::testdata::groningen(), 1471.0, None),
            build_tunnels(&crate::testdata::groningen_ring(), tile_m, None),
        ] {
            for m in &out.meshes {
                let slot = match m.group {
                    SurfaceGroup::TunnelRoad(_) => 0,
                    SurfaceGroup::TunnelStructure => 1,
                    _ => continue,
                };
                for t in m.indices.chunks_exact(3) {
                    let [a, b, c] = [0, 1, 2].map(|k| m.positions[t[k] as usize]);
                    let (u, v) = (
                        [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                        [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
                    );
                    let g = [
                        u[1] * v[2] - u[2] * v[1],
                        u[2] * v[0] - u[0] * v[2],
                        u[0] * v[1] - u[1] * v[0],
                    ];
                    let len = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
                    // The notch triangles on the inside of a bend collapse to a
                    // hair and have no winding worth asking about.
                    if len < 1e-3 {
                        continue;
                    }
                    let n = m.normals[t[0] as usize];
                    let dot = (g[0] * n[0] + g[1] * n[1] + g[2] * n[2]) / len;
                    assert!(
                        dot > 0.5,
                        "a {:?} triangle is wound away from its normal {n:?} and is \
                         culled from the side it is meant to be seen from: {a:?} {b:?} {c:?}",
                        m.group
                    );
                    checked[slot] += 1;
                }
            }
        }
        assert!(
            checked[0] > 0 && checked[1] > 0,
            "no tunnel faces to check: {checked:?}"
        );
    }

    /// **Under a lid the walls stop at the ceiling.** Above it is earth, and a
    /// wall carried on to street level stood out of the Oude Winschoterdiep as
    /// two long slivers along the lid under it (2026-09-23).
    #[test]
    fn no_wall_rises_through_a_lid() {
        let tile_m = ring_tile_m();
        let out = build_tunnels(&crate::testdata::groningen_ring(), tile_m, None);
        let mut checked = 0usize;
        for m in out
            .meshes
            .iter()
            .filter(|m| m.group == SurfaceGroup::TunnelStructure)
        {
            for p in &m.positions {
                // Carriageways: the fixture's garage loop overlaps two path
                // tunnels, which is its own tagging, not a lid.
                for t in out.tunnels.iter().filter(|t| t.half_w >= 3.0) {
                    // A wall's corners stand at the centre line's vertices, so
                    // ask at the vertices with a lid on both sides of them.
                    for k in 1..t.centre.len().saturating_sub(1) {
                        if !(t.covered[k - 1] && t.covered[k] && t.covered[k + 1]) {
                            continue;
                        }
                        let c = t.centre[k];
                        if (p[0] - c[0]).hypot(p[2] - c[1]) > t.half_w + 0.05 {
                            continue;
                        }
                        let ceiling = t.road_y[k] + t.clearance;
                        checked += 1;
                        assert!(
                            p[1] <= ceiling + 0.01,
                            "a tunnel wall reaches {:.2} m under a lid whose ceiling is {ceiling:.2} m",
                            p[1]
                        );
                    }
                }
            }
        }
        assert!(checked > 0, "no structure under a lid was checked");
    }

    /// **A building in a covered tunnel's old trench is the tunnel's, and one on
    /// the lid is not.** The rule needs both halves: the place, and the ground
    /// under it being well below the land around.
    #[test]
    fn a_building_in_a_covered_trench_is_told_from_one_on_the_lid() {
        // The portal frame in the eastern mouth, pand 0014100040019318, in this
        // tile's metres (from OSM way 1157021132): an arch across both
        // carriageways.
        let frame: Vec<[f32; 2]> = vec![
            [759.1, 465.8],
            [774.4, 460.5],
            [788.3, 476.0],
            [790.1, 481.0],
            [786.3, 486.3],
            [782.7, 492.4],
            [781.3, 484.7],
            [764.8, 466.6],
            [759.1, 465.8],
        ];
        let lidded = build_tunnels(&crate::testdata::groningen_ring(), ring_tile_m(), None);
        assert!(
            footprint_in_a_covered_trench(&lidded.tunnels, &lidded.openings, &frame, &|_| 7.0),
            "the portal frame in the eastern mouth is not recognised"
        );
        assert!(
            !footprint_in_a_covered_trench(&lidded.tunnels, &lidded.openings, &frame, &|_| 0.5),
            "a building on the lid at street level is taken out"
        );
        let out = build_tunnels(&crate::testdata::groningen_ring(), ring_tile_m(), None);
        // The middle of the ring's covered stretch under the Oude Winschoterdiep.
        let t = out
            .tunnels
            .iter()
            .find(|t| t.half_w >= 3.0 && t.covered.iter().all(|c| *c) && t.centre.len() >= 3)
            .expect("the ring has a lid");
        let (a, b) = (t.centre[1], t.centre[2]);
        let over = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        assert!(stands_in_a_covered_trench(
            &out.tunnels,
            &out.openings,
            over,
            7.0
        ));
        assert!(!stands_in_a_covered_trench(
            &out.tunnels,
            &out.openings,
            over,
            0.5
        ));
        let beside = [over[0] + 40.0, over[1] + 40.0];
        assert!(!stands_in_a_covered_trench(
            &out.tunnels,
            &out.openings,
            beside,
            7.0
        ));
    }

    /// **The survey's own tunnel walls are recognised, and a wall elsewhere is
    /// not.** Measured on the ring tile's BGT (cartopy `bgt.ts`, 2026-09-23), in
    /// this tile's metres.
    #[test]
    fn the_surveyed_walls_of_the_ring_are_the_tunnels() {
        let out = build_tunnels(&crate::testdata::groningen_ring(), ring_tile_m(), None);
        // The east portal's headwall, measured 7.02 m, across both mouths.
        let headwall = [[956.0f32, 319.0], [969.0, 352.0]];
        // The north cutting's retaining wall, measured 9.11 m.
        let retaining = [[691.0f32, 556.0], [779.0, 478.0]];
        // A wall running along the lid.
        let along_lid = [[772.0f32, 474.0], [961.0, 333.0]];
        for (name, w) in [
            ("headwall", &headwall[..]),
            ("retaining wall", &retaining[..]),
            ("wall along the lid", &along_lid[..]),
        ] {
            assert!(
                wall_of_a_sunken_road(&out.tunnels, &out.openings, w),
                "the ring's {name} is not recognised as the tunnel's"
            );
        }
        // A wall of the same length a hundred metres from the ring.
        let elsewhere = [[691.0f32, 656.0], [779.0, 578.0]];
        assert!(!wall_of_a_sunken_road(
            &out.tunnels,
            &out.openings,
            &elsewhere
        ));
    }

    /// **No road leaves a sunken deck at street level.** At the ring's eastern
    /// portal both the carriageway and the slip road to the Europaweg come out of
    /// the tunnel; only the carriageway was given a ramp, so the slip road began
    /// at grade beside a trench seven metres deep (2026-09-23). Every surface way
    /// that carries on from a deck end more than a metre down must be sunken too.
    #[test]
    fn every_road_out_of_a_deck_is_a_ramp() {
        let tile_m = ring_tile_m();
        let tile = crate::testdata::groningen_ring();
        let out = build_tunnels(&tile, tile_m, None);
        let surface = surface_lines(&tile, tile_m);
        let mut checked = 0usize;
        // Carriageways, as `the_ring_road_never_steps_…` takes them: the
        // fixture's paths include a garage's service loop whose own tagging
        // holds one end a metre down, which is not a portal.
        for t in out.tunnels.iter().filter(|t| t.half_w >= 3.0) {
            let n = t.centre.len();
            for (p, inward, y) in [
                (t.centre[0], t.centre[1], t.road_y[0]),
                (t.centre[n - 1], t.centre[n - 2], t.road_y[n - 1]),
            ] {
                if y > -1.0 {
                    continue;
                }
                let (dx, dy) = (p[0] - inward[0], p[1] - inward[1]);
                let len = dx.hypot(dy).max(1e-6);
                for l in &surface {
                    for k in 0..2 {
                        let q = if k == 0 {
                            l.pts[0]
                        } else {
                            l.pts[l.pts.len() - 1]
                        };
                        if !l.own[k] || (q[0] - p[0]).hypot(q[1] - p[1]) > SAME_POINT_M {
                            continue;
                        }
                        let mut pts = l.pts.clone();
                        if k == 1 {
                            pts.reverse();
                        }
                        let Some(d) = leaving(&pts) else { continue };
                        if (d[0] * dx + d[1] * dy) / len < JOIN_COS || l.bridge {
                            continue;
                        }
                        checked += 1;
                        assert!(
                            out.sunken.iter().any(|s| s.feature == l.feature),
                            "street feature {} leaves a deck {:.1} m down at {p:?} at grade",
                            l.feature,
                            -y
                        );
                    }
                }
            }
        }
        assert!(
            checked >= 2,
            "only {checked} roads out of a deck — the portals are not being looked at"
        );
    }

    /// **No crown hangs over an open trench** once the caller's filter has run.
    /// The ring fixture's park is laid over the whole corridor, so the scatter
    /// plants into the cuttings unless the openings keep it out — and the trunk
    /// alone is not enough, since a crown on the lip overhangs the carriageway.
    #[test]
    fn no_tree_crown_reaches_into_an_opening() {
        let tile_m = ring_tile_m();
        let tile = crate::testdata::groningen_ring();
        let out = build_tunnels(&tile, tile_m, None);
        let mut trees = crate::scatter::scatter_vegetation(&tile, tile_m, 7).expect("scatters");
        let before = trees.len();
        trees.retain(|t| !near_an_opening(&out.openings, [t.x, t.z], t.radius * 0.8));
        assert!(
            trees.len() < before,
            "the filter removed nothing — no tree was near an opening"
        );
        for t in &trees {
            for ring in &out.openings {
                assert!(
                    !point_in_ring([t.x, t.z], ring),
                    "a tree stands in an opening at {},{}",
                    t.x,
                    t.z
                );
            }
        }
    }

    /// **A sunken road under water is covered.** The ring passes under the Oude
    /// Winschoterdiep about 35 m short of its eastern portal, and until the ramps
    /// moved outside the tag it did so 2 m down in an open trench — the phone
    /// showed the carriageway crossing the canal at water level (2026-09-23). A
    /// road can be open to the sky or under a canal, never both, so no open
    /// stretch of deck may lie inside a water polygon, and the ring's must lie
    /// under one somewhere (or the test is not looking at the canal).
    #[test]
    fn a_road_under_water_has_a_lid_over_it() {
        let water = |tile: &TileData, tile_m: f32| -> Vec<Vec<[f32; 2]>> {
            let Some(meta) = tile.layer("water_polygons") else {
                return Vec::new();
            };
            let scale = tile_m / TileData::extent(meta);
            let ring = |p: &geo_types::Polygon<f32>| -> Vec<[f32; 2]> {
                p.exterior()
                    .0
                    .iter()
                    .map(|c| [c.x * scale, c.y * scale])
                    .collect()
            };
            tile.features(meta)
                .iter()
                .flat_map(|f| match &f.geometry {
                    Geometry::Polygon(p) => vec![ring(p)],
                    Geometry::MultiPolygon(mp) => mp.0.iter().map(ring).collect(),
                    _ => Vec::new(),
                })
                .collect()
        };
        let mut covered_under_water = 0usize;
        for (tile, tile_m, name) in [
            (crate::testdata::groningen(), 1471.0, "groningen"),
            (
                crate::testdata::groningen_ring(),
                ring_tile_m(),
                "groningen_ring",
            ),
        ] {
            let pools = water(&tile, tile_m);
            let out = build_tunnels(&tile, tile_m, None);
            for t in &out.tunnels {
                for i in 0..t.centre.len().saturating_sub(1) {
                    let (a, b) = (t.centre[i], t.centre[i + 1]);
                    let deep = t.road_y[i].min(t.road_y[i + 1]) < -1.0;
                    for f in [0.25f32, 0.5, 0.75] {
                        let p = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f];
                        if !pools.iter().any(|r| point_in_ring(p, r)) {
                            continue;
                        }
                        if t.covered[i] && t.covered[i + 1] {
                            covered_under_water += 1;
                        } else {
                            assert!(
                                !deep,
                                "{name}: a sunken road at {p:?} ({:.1} m down) runs \
                                 open under water",
                                -t.road_y[i]
                            );
                        }
                    }
                }
            }
        }
        assert!(
            covered_under_water > 0,
            "no tunnel passes under water in either fixture — the ring's aqueduct \
             under the Oude Winschoterdiep is not being looked at"
        );
    }

    /// **The ground may not be cut away over a roofed stretch.**
    ///
    /// A picture cannot answer this over Groningen's ring: the whole Julianaplein
    /// is a building site the map carries no landuse for, so every capture shows
    /// bare ground whether the tunnel opened it or not. Two hours went into
    /// telling those apart by eye on 2026-09-20 before measuring it instead.
    ///
    /// The rule is the one the module already states: the hole and the absent
    /// ceiling are decided by one test, so a vertex with a lid over it must not
    /// lie inside any opening. `merge_parallel_openings` is the thing that could
    /// break it — it replaces two rings with their convex **hull**, which is
    /// bigger than either, and a hull is free to swallow the roofed stretch
    /// between two ramps.
    #[test]
    fn no_opening_swallows_a_roofed_stretch() {
        // Along each roofed *segment* rather than at its vertices: since the
        // ramps moved outside the portals, a lid reaches its run's last vertex,
        // and the approach's opening starts on exactly that point — on the
        // ring's boundary, where `point_in_ring` may answer either way.
        let out = build_tunnels(&crate::testdata::groningen(), 1471.0, None);
        for t in &out.tunnels {
            for i in 0..t.covered.len().saturating_sub(1) {
                if !(t.covered[i] && t.covered[i + 1]) {
                    continue;
                }
                let (a, b) = (t.centre[i], t.centre[i + 1]);
                for f in [0.1f32, 0.5, 0.9] {
                    let p = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f];
                    for ring in &out.openings {
                        assert!(
                            !point_in_ring(p, ring),
                            "a roofed stretch at {p:?} lies inside an opening — \
                             the ground is cut away over a lid"
                        );
                    }
                }
            }
        }
    }

    /// Even-odd crossing count, for the test above.
    fn point_in_ring(p: [f32; 2], ring: &[[f32; 2]]) -> bool {
        let mut inside = false;
        for e in ring.windows(2) {
            let (a, b) = (e[0], e[1]);
            if (a[1] > p[1]) != (b[1] > p[1]) {
                let t = (p[1] - a[1]) / (b[1] - a[1]);
                if p[0] < a[0] + t * (b[0] - a[0]) {
                    inside = !inside;
                }
            }
        }
        inside
    }

    /// **Each tunnel keeps its own class**, so a cell with a cycle underpass and a
    /// road tunnel draws gravel under one and tarmac with lane markings under the
    /// other.
    ///
    /// `road_class_of_tunnel` used to be a single `Option<RoadClass>` for the whole
    /// call — set by the first tunnel, read once for the group every deck was
    /// merged into. Over Groningen's ring that drew the motorway in the
    /// carriageway's tarmac in one cell and in a footpath's gravel in the next,
    /// changing at the cell boundary, which is the one place a viewer is
    /// guaranteed to look (2026-09-20).
    ///
    /// **A class is only expected when its tunnels actually build something.** A
    /// way through a building is a passage rather than a tunnel and is excluded
    /// by design, so the classes asked for here are the underground features
    /// `build_tunnels` itself would keep.
    #[test]
    fn each_tunnel_keeps_its_own_class() {
        let tile = crate::testdata::groningen();
        let out = build_tunnels(&tile, 1471.0, None);

        let Some(meta) = tile.layer("streets") else {
            return;
        };
        let features = tile.features(meta).to_vec();
        let passages = building_passages(&tile, &features);
        let mut wanted: Vec<RoadClass> = Vec::new();
        for (fi, feature) in features.iter().enumerate() {
            if !underground(feature) || passages.contains(&fi) {
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

        let mut drawn: Vec<RoadClass> = Vec::new();
        for m in &out.meshes {
            if let SurfaceGroup::TunnelRoad(c) = m.group {
                if !drawn.contains(&c) {
                    drawn.push(c);
                }
            }
        }
        // Nothing invented: every class drawn is one the tile asked for.
        for c in &drawn {
            assert!(
                wanted.contains(c),
                "drew a {c:?} tunnel the tile never asked for; wanted {wanted:?}"
            );
        }
        // And the classes are not collapsed onto one another.
        assert!(
            drawn.len() >= wanted.len().min(2),
            "the tile has tunnels of {} classes ({wanted:?}) and they are drawn as \
             {drawn:?} — the decks were merged onto one class",
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
            measured: false,
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
            // **Every tunnel's position on Earth, not only the deep ones.** The
            // entrance printout above skips anything shallower than a metre and
            // anything already at full depth on its first vertex, which is most of
            // a city's underpasses — so aiming a camera at one of *those* meant
            // hand-converting tile metres again, and two of `grade-shots.toml`'s
            // views were still missing by 26 and 74 metres for exactly that
            // reason. The conversion is right here and can be wrong only once.
            let mid = b
                .centre
                .get(b.centre.len() / 2)
                .copied()
                .unwrap_or([0.0, 0.0]);
            let here = key.map_or(String::new(), |k| {
                let (lat, lon) = {
                    let (z, x, y) = k;
                    let n = 2f64.powf(z);
                    let lat0 = (std::f64::consts::PI * (1.0 - 2.0 * y / n)).sinh().atan();
                    let span = 40_075_016.686 * lat0.cos() / n;
                    let (fx, fy) = (x + f64::from(mid[0]) / span, y + f64::from(mid[1]) / span);
                    (
                        (std::f64::consts::PI * (1.0 - 2.0 * fy / n))
                            .sinh()
                            .atan()
                            .to_degrees(),
                        fx / n * 360.0 - 180.0,
                    )
                };
                format!("  mid {lat:.6},{lon:.6}")
            });
            println!(
                "tunnel {i}: half_w={:.1} len={len:.0}m deepest={deepest:.2} ends=({:.0},{:.0})->({:.0},{:.0}){here}",
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

    /// **The carriageway may not step**, and until 2026-09-22 Groningen's ring
    /// stepped six times.
    ///
    /// The tunnel flag marks only the covered parts, so the ring arrives as six
    /// lid runs with untagged road between them. [`bridge_short_gaps`] knew that
    /// — it is why those lids get no portal — and then threw the connecting way
    /// away, so the road between two decks at −7 m was drawn on the ground at
    /// zero: 863 m of carriageway with six vertical seven-metre steps in it, a
    /// car teleporting at each, and from above a road that simply stopped dead in
    /// a park.
    ///
    /// The invariant is local and needs no camera: at every end of every sunken
    /// carriageway, either another corridor continues at the same depth, or the
    /// road has come back up to the surface, or the tile border cut it. A step is
    /// none of those.
    /// The ring tile's own ground size in metres, which is what `build_tunnels`
    /// scales tile units by. Derived from the tile's coordinates rather than
    /// written down: a hand-typed span puts every distance measured here out by a
    /// fraction of a per cent, and these tests compare distances.
    fn ring_tile_m() -> f32 {
        let (_, y) = crate::testdata::GRONINGEN_RING_TILE;
        let n = 2f64.powi(14);
        #[allow(clippy::cast_precision_loss)]
        let lat = (std::f64::consts::PI * (1.0 - 2.0 * y as f64 / n))
            .sinh()
            .atan();
        #[allow(clippy::cast_possible_truncation)]
        let m = (40_075_016.686 * lat.cos() / n) as f32;
        m
    }

    #[test]
    fn the_ring_road_never_steps_between_a_lid_and_the_road_beside_it() {
        let tile_m = ring_tile_m();
        let out = build_tunnels(&crate::testdata::groningen_ring(), tile_m, None);
        // The carriageways, not the footpaths: `half_w` is the road's own width
        // and the ring's is 5 m against a cycle underpass's 1.1.
        let roads: Vec<&Tunnel> = out.tunnels.iter().filter(|t| t.half_w >= 4.0).collect();
        assert!(
            roads.len() >= 10,
            "expected the ring's six lids and the cuttings between them, got {}",
            roads.len()
        );

        // Every end of every corridor, as (point, depth there).
        let ends: Vec<([f32; 2], f32)> = roads
            .iter()
            .filter(|t| t.centre.len() >= 2)
            .flat_map(|t| {
                [
                    (t.centre[0], t.road_y[0]),
                    (t.centre[t.centre.len() - 1], t.road_y[t.road_y.len() - 1]),
                ]
            })
            .collect();

        let mut deep_ends = 0;
        for (i, (p, y)) in ends.iter().enumerate() {
            // At the surface: the road has climbed out, which is a portal.
            if *y > -0.5 {
                continue;
            }
            // Cut by the tile border: the run carries on into the neighbour and
            // is deliberately held at depth so the two halves meet flush.
            if p[0] <= 1.0 || p[1] <= 1.0 || p[0] >= tile_m - 1.0 || p[1] >= tile_m - 1.0 {
                continue;
            }
            deep_ends += 1;
            let partner = ends
                .iter()
                .enumerate()
                .find(|(j, (q, _))| *j != i && (q[0] - p[0]).hypot(q[1] - p[1]) <= SAME_POINT_M);
            let Some((_, (_, other))) = partner else {
                panic!(
                    "a carriageway {:.1} m down ends at {p:?} with nothing continuing it — \
                     the road steps to the surface here",
                    -y
                );
            };
            assert!(
                (other - y).abs() <= 0.25,
                "two corridors meet at {p:?} at {y:.2} m and {other:.2} m — \
                 a {:.1} m step in the carriageway",
                (other - y).abs()
            );
        }
        assert!(
            deep_ends >= 8,
            "only {deep_ends} sunken ends were checked, so the fixture is not the ring"
        );
    }

    /// The open stretches between the lids are open, and the lids are not.
    ///
    /// The other half of the same change: a cutting that took a lid with it would
    /// roof the whole ring and there would be nothing to see, and one that opened
    /// a four-metre tagging seam would punch a hole in ground nothing said was
    /// open ([`MIN_CUTTING_M`]).
    #[test]
    fn the_cuttings_are_open_and_the_lids_are_not() {
        let out = build_tunnels(&crate::testdata::groningen_ring(), ring_tile_m(), None);
        let roads: Vec<&Tunnel> = out.tunnels.iter().filter(|t| t.half_w >= 4.0).collect();
        let (mut open, mut lidded) = (0, 0);
        for t in &roads {
            let len: f32 = t
                .centre
                .windows(2)
                .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
                .sum();
            if t.covered.iter().all(|c| !c) {
                // An open cutting, which is what the 107 m gaps are.
                assert!(
                    len >= MIN_CUTTING_M,
                    "a {len:.0} m stretch was opened; under {MIN_CUTTING_M} m the \
                     lid carries on"
                );
                open += 1;
            } else if t.covered.iter().any(|c| *c) {
                lidded += 1;
            }
        }
        assert!(open >= 4, "expected the ring's open cuttings, got {open}");
        assert!(lidded >= 6, "expected the ring's lids, got {lidded}");
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
            measured: false,
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
